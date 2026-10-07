use crate::{
    Error, Result,
    backend::*,
    config::{CometConfig, read_secret},
};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Method, Response};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::Instant,
};
use tokio_tungstenite::{
    Connector,
    tungstenite::{Message, client::IntoClientRequest},
};

/// A single appliance. Public target IDs are 1-based; firmware channel IDs are 0-based.
pub struct Comet {
    config: CometConfig,
    http: Client,
    token: Mutex<Option<String>>,
    gate: Arc<Mutex<()>>,
    ws: Mutex<Option<(JoinHandle<()>, watch::Sender<Instant>)>>,
    connector: Connector,
    transfer_timeout: Duration,
}

impl Comet {
    pub fn new(config: CometConfig, transfer_timeout: Duration) -> Result<Self> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .referer(false)
            .connect_timeout(Duration::from_secs(5));
        // Every request sets its own total deadline. A client-level read timeout
        // also starts while waiting for upload response headers, so it would
        // abort otherwise healthy large uploads before their transfer deadline.
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(path) = &config.ca_file {
            let pem = std::fs::read(path).map_err(|_| Error::Invalid("cannot read CA file"))?;
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&pem)
                    .map_err(|_| Error::Invalid("invalid CA certificate"))?,
            );
            for cert in rustls_pemfile::certs(&mut Cursor::new(&pem)) {
                roots
                    .add(cert.map_err(|_| Error::Invalid("invalid CA certificate"))?)
                    .map_err(|_| Error::Invalid("invalid CA certificate"))?;
            }
        }
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config,
            http: builder.build().map_err(|_| Error::Transport)?,
            token: Mutex::new(None),
            gate: Arc::new(Mutex::new(())),
            ws: Mutex::new(None),
            connector: Connector::Rustls(Arc::new(tls)),
            transfer_timeout,
        })
    }

    async fn auth(&self) -> Result<String> {
        let mut token = self.token.lock().await;
        if let Some(value) = token.as_ref() {
            return Ok(value.clone());
        }
        let password = read_secret(&self.config.password_file).map_err(|_| Error::Auth)?;
        let response = self
            .http
            .post(
                self.config
                    .url
                    .join("api/auth/login")
                    .map_err(|_| Error::Protocol)?,
            )
            .timeout(Duration::from_secs(10))
            .form(&[
                ("user", self.config.username.as_str()),
                ("passwd", password.as_str()),
                ("expire", "0"),
            ])
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        if !response.status().is_success() {
            return Err(Error::Auth);
        }
        let found = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .filter_map(|h| h.split(';').next())
            .find_map(|c| c.strip_prefix("auth_token="))
            .filter(|s| !s.is_empty())
            .ok_or(Error::Auth)?
            .to_owned();
        *token = Some(found.clone());
        Ok(found)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<reqwest::RequestBuilder> {
        Ok(self
            .http
            .request(
                method,
                self.config.url.join(path).map_err(|_| Error::Protocol)?,
            )
            .header("Token", self.auth().await?)
            .query(query)
            .timeout(Duration::from_secs(30)))
    }
    async fn check(&self, response: Response) -> Result<Response> {
        if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
            *self.token.lock().await = None;
            if let Some((task, _)) = self.ws.lock().await.take() {
                task.abort();
            }
            return Err(Error::Auth);
        }
        if !response.status().is_success() {
            return Err(Error::Upstream(response.status().as_u16()));
        }
        Ok(response)
    }
    async fn parse(&self, response: Response) -> Result<Value> {
        let response = self.check(response).await?;
        let data = bounded(response, 1024 * 1024).await?;
        let mut value: Value = serde_json::from_slice(&data).map_err(|_| Error::Protocol)?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Protocol);
        }
        Ok(value.get_mut("result").ok_or(Error::Protocol)?.take())
    }
    async fn get(&self, path: &str) -> Result<Value> {
        self.parse(
            self.request(Method::GET, path, &[])
                .await?
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )
        .await
    }
    async fn post(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.parse(
            self.request(Method::POST, path, query)
                .await?
                .send()
                .await
                .map_err(|_| Error::Transport)?,
        )
        .await
    }
    fn validate_target(&self, target: u8) -> Result<()> {
        if target == 0 || target > self.config.ports {
            return Err(Error::Invalid(
                "target is outside configured 1-based port range",
            ));
        }
        Ok(())
    }
    async fn active(&self) -> Result<u8> {
        let state = self.get("api/switch").await?;
        let raw = state
            .pointer("/summary/active_port")
            .and_then(Value::as_u64)
            .ok_or(Error::Protocol)?;
        if raw >= u64::from(self.config.ports) {
            return Err(Error::Protocol);
        }
        Ok(raw as u8 + 1)
    }
    async fn selected(&self, target: u8) -> Result<()> {
        self.validate_target(target)?;
        if self.active().await? == target {
            return Ok(());
        }
        let media = self.get("api/msd").await?;
        if media_connected(&media)? {
            return Err(Error::MediaConnected);
        }
        self.post(
            "api/switch/set_active",
            &[("port", (target - 1).to_string())],
        )
        .await?;
        for _ in 0..20 {
            if self.active().await? == target {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(Error::Protocol)
    }

    async fn ensure_stream(&self) -> Result<()> {
        let mut task = self.ws.lock().await;
        if let Some((handle, activity)) = task.as_ref()
            && !handle.is_finished()
        {
            activity.send_replace(Instant::now());
            return Ok(());
        }
        let mut url = self
            .config
            .url
            .join("api/ws?stream=true")
            .map_err(|_| Error::Protocol)?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|_| Error::Protocol)?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| Error::Protocol)?;
        request.headers_mut().insert(
            "Token",
            self.auth().await?.parse().map_err(|_| Error::Auth)?,
        );
        let opened = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_tls_with_config(
                request,
                None,
                false,
                Some(self.connector.clone()),
            ),
        )
        .await
        .map_err(|_| Error::Transport)?
        .map_err(|_| Error::Transport)?;
        let mut socket = opened.0;
        let (activity, mut last_used) = watch::channel(Instant::now());
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                let deadline = *last_used.borrow() + Duration::from_secs(30);
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    changed = last_used.changed() => if changed.is_err() { break; },
                    _ = interval.tick() => if socket.send(Message::Text(r#"{"event_type":"ping","event":{}}"#.into())).await.is_err() { break; },
                    msg = socket.next() => match msg { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break, _ => {} },
                }
            }
        });
        *task = Some((handle, activity));
        Ok(())
    }
    async fn not_connected(&self) -> Result<Value> {
        let state = self.get("api/msd").await?;
        if state.get("enabled").and_then(Value::as_bool) == Some(false) {
            return Err(Error::Unsupported("virtual media is disabled"));
        }
        if media_connected(&state)? {
            return Err(Error::MediaConnected);
        }
        Ok(state)
    }
}

#[async_trait]
impl KvmBackend for Comet {
    async fn status(&self) -> Result<KvmStatus> {
        let _guard = self.gate.lock().await;
        let active = self.active().await?;
        let atx = self.get("api/atx").await?;
        let msd = self.get("api/msd").await?;
        Ok(KvmStatus {
            backend: "comet".into(),
            active_target: active,
            targets: (1..=self.config.ports)
                .map(|id| TargetInfo {
                    id,
                    label: format!("Port {id}"),
                })
                .collect(),
            capabilities: Capabilities {
                screenshot: true,
                keyboard: true,
                pointer: true,
                clipboard: false,
                power: atx.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                virtual_media: msd.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                resumable_transfer: false,
            },
            diagnostics: json!({"atx":atx,"keymaps":self.get("api/hid/keymaps").await?}),
        })
    }
    async fn select(&self, target: u8) -> Result<Value> {
        let _guard = self.gate.lock().await;
        self.selected(target).await?;
        Ok(json!({"active_target":target}))
    }
    async fn capture(&self, target: u8, max_width: u32) -> Result<Snapshot> {
        if !(320..=4096).contains(&max_width) {
            return Err(Error::Invalid("max_width must be 320..4096"));
        }
        let _guard = self.gate.lock().await;
        self.selected(target).await?;
        self.ensure_stream().await?;
        for attempt in 0..6 {
            let response = self
                .request(Method::GET, "api/streamer/snapshot", &[])
                .await?
                .send()
                .await
                .map_err(|_| Error::Transport)?;
            if response.status().as_u16() == 503 && attempt < 5 {
                tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                continue;
            }
            let bytes = bounded(self.check(response).await?, 16 * 1024 * 1024).await?;
            let (jpeg, width, height) =
                tokio::task::spawn_blocking(move || resize_jpeg(bytes, max_width))
                    .await
                    .map_err(|_| Error::Protocol)??;
            return Ok(Snapshot {
                jpeg,
                width,
                height,
                captured_at: SystemTime::now(),
            });
        }
        Err(Error::Protocol)
    }
    async fn input(&self, target: u8, input: Input) -> Result<Value> {
        validate_input(&input)?;
        let _guard = self.gate.lock().await;
        self.selected(target).await?;
        match input {
            Input::Chord { keys } => {
                self.post("api/hid/events/send_shortcut", &[("keys", keys.join(","))])
                    .await
            }
            Input::Text { text, keymap } => {
                let mut query = vec![("limit", "0".into())];
                if let Some(keymap) = keymap {
                    query.push(("keymap", keymap));
                }
                self.parse(
                    self.request(Method::POST, "api/hid/print", &query)
                        .await?
                        .header("Content-Type", "text/plain; charset=utf-8")
                        .body(text)
                        .send()
                        .await
                        .map_err(|_| Error::Transport)?,
                )
                .await
            }
            Input::Move { x, y } => {
                self.post(
                    "api/hid/events/send_mouse_move",
                    &[
                        ("to_x", coordinate(x).to_string()),
                        ("to_y", coordinate(y).to_string()),
                    ],
                )
                .await
            }
            Input::Click { button } => {
                self.post(
                    "api/hid/events/send_mouse_button",
                    &[(
                        "button",
                        match button {
                            Button::Left => "left",
                            Button::Right => "right",
                            Button::Middle => "middle",
                        }
                        .into(),
                    )],
                )
                .await
            }
            Input::Scroll { x, y } => {
                self.post(
                    "api/hid/events/send_mouse_wheel",
                    &[("delta_x", x.to_string()), ("delta_y", y.to_string())],
                )
                .await
            }
            Input::ReleaseAll => self.post("api/hid/reset", &[]).await,
        }
    }
    async fn power(&self, target: u8, action: Power) -> Result<Value> {
        let _guard = self.gate.lock().await;
        self.selected(target).await?;
        if self
            .get("api/atx")
            .await?
            .get("enabled")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(Error::Unsupported("ATX accessory unavailable"));
        }
        let (path, key, value) = match action {
            Power::On => ("api/atx/power", "action", "on"),
            Power::Off => ("api/atx/power", "action", "off"),
            Power::OffHard => ("api/atx/power", "action", "off_hard"),
            Power::ResetHard => ("api/atx/power", "action", "reset_hard"),
            Power::Button => ("api/atx/click", "button", "power"),
            Power::ButtonLong => ("api/atx/click", "button", "power_long"),
            Power::ResetButton => ("api/atx/click", "button", "reset"),
        };
        self.post(path, &[(key, value.into()), ("wait", "true".into())])
            .await
    }
    async fn media_list(&self) -> Result<MediaCatalog> {
        let _guard = self.gate.lock().await;
        catalog(self.get("api/msd").await?)
    }
    async fn media_mount(&self, target: u8, name: &str, cdrom: bool) -> Result<Value> {
        validate_name(name)?;
        let _guard = self.gate.lock().await;
        self.selected(target).await?;
        self.not_connected().await?;
        self.post(
            "api/msd/set_params",
            &[
                ("image", name.into()),
                ("cdrom", cdrom.to_string()),
                ("rw", "false".into()),
            ],
        )
        .await?;
        self.post("api/msd/set_connected", &[("connected", "true".into())])
            .await
    }
    async fn media_eject(&self, target: u8) -> Result<Value> {
        let _guard = self.gate.lock().await;
        self.validate_target(target)?;
        if self.active().await? != target {
            return Err(Error::Invalid(
                "eject must name the currently selected target",
            ));
        }
        self.post("api/msd/set_connected", &[("connected", "false".into())])
            .await
    }
    async fn media_delete(&self, name: &str) -> Result<Value> {
        validate_name(name)?;
        let _guard = self.gate.lock().await;
        self.not_connected().await?;
        self.post("api/msd/remove", &[("image", name.into())]).await
    }
    async fn upload(&self, name: &str, size: u64, stream: ByteStream) -> Result<Value> {
        validate_name(name)?;
        let _guard = self.gate.lock().await;
        let state = self.not_connected().await?;
        if state
            .pointer("/storage/images")
            .and_then(|v| v.get(name))
            .is_some()
        {
            return Err(Error::Invalid(
                "image already exists; choose another name or explicitly delete it",
            ));
        }
        let response = self
            .request(
                Method::POST,
                "api/msd/write",
                &[("image", name.into()), ("remove_incomplete", "true".into())],
            )
            .await?
            .timeout(self.transfer_timeout)
            .header("Content-Length", size)
            .header("Content-Type", "application/octet-stream")
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        let result = self.parse(response).await?;
        if result.pointer("/image/written").and_then(Value::as_u64) != Some(size) {
            return Err(Error::Protocol);
        }
        Ok(result)
    }
    async fn download(&self, name: &str) -> Result<Download> {
        validate_name(name)?;
        let guard = self.gate.clone().lock_owned().await;
        let response = self
            .request(
                Method::GET,
                "api/msd/read",
                &[("image", name.into()), ("compress", "none".into())],
            )
            .await?
            .timeout(self.transfer_timeout)
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        let response = self.check(response).await?;
        let size = response.content_length().ok_or(Error::Protocol)?;
        let mut chunks = response.bytes_stream();
        let stream = async_stream::try_stream! {
            let _guard=guard;
            while let Some(chunk)=chunks.next().await {yield chunk.map_err(|_|std::io::Error::other("backend download interrupted"))?;}
        };
        Ok(Download {
            size,
            stream: Box::pin(stream),
        })
    }
}

fn coordinate(value: f64) -> i32 {
    (value * 65535.0 - 32768.0).round() as i32
}
fn catalog(state: Value) -> Result<MediaCatalog> {
    let enabled = state
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or(Error::Protocol)?;
    let connected = state.pointer("/drive/connected").and_then(Value::as_bool);
    if enabled && connected.is_none() {
        return Err(Error::Protocol);
    }
    let mut images = Vec::new();
    if let Some(stored) = state.pointer("/storage/images").and_then(Value::as_object) {
        for (name, image) in stored {
            images.push(MediaImage {
                name: name.clone(),
                size_bytes: image
                    .get("size")
                    .and_then(Value::as_u64)
                    .ok_or(Error::Protocol)?,
                complete: image.get("complete").and_then(Value::as_bool),
            });
        }
    }
    Ok(MediaCatalog {
        enabled,
        connected: connected.unwrap_or(false),
        mounted_image: state
            .pointer("/drive/image/name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        free_bytes: state
            .pointer("/storage/parts//free")
            .and_then(Value::as_u64),
        images,
    })
}
fn media_connected(state: &Value) -> Result<bool> {
    match state.get("enabled").and_then(Value::as_bool) {
        Some(false) => Ok(false),
        Some(true) => state
            .pointer("/drive/connected")
            .and_then(Value::as_bool)
            .ok_or(Error::Protocol),
        None => Err(Error::Protocol),
    }
}
async fn bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    let mut chunks = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| Error::Transport)?;
        if chunk.len() > limit.saturating_sub(data.len()) {
            return Err(Error::Protocol);
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
fn resize_jpeg(bytes: Vec<u8>, max_width: u32) -> Result<(Vec<u8>, u32, u32)> {
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), image::ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| Error::Protocol)?;
    let image = if image.width() > max_width {
        image.resize(max_width, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let (width, height) = (image.width(), image.height());
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&image)
        .map_err(|_| Error::Protocol)?;
    Ok((jpeg, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_endpoints() {
        assert_eq!(coordinate(0.0), -32768);
        assert_eq!(coordinate(1.0), 32767);
    }
    #[test]
    fn malformed_snapshot_fails() {
        assert!(resize_jpeg(vec![0, 1, 2], 1280).is_err());
    }
}
