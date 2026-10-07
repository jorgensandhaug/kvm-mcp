use crate::{
    Error, Result,
    backend::{Input, KvmBackend, Power},
    transfer::{Direction, Tickets},
};
use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct App {
    pub backend: Arc<dyn KvmBackend>,
    pub tickets: Arc<Tickets>,
    pub public_url: url::Url,
    auth_hash: [u8; 32],
    transfer_timeout: Duration,
}
impl App {
    pub fn new(
        backend: Arc<dyn KvmBackend>,
        public_url: url::Url,
        auth: &str,
        max_bytes: u64,
        transfer_timeout: Duration,
    ) -> Result<Self> {
        if auth.len() < 32 {
            return Err(Error::Invalid(
                "MCP bearer token must have at least 32 characters",
            ));
        }
        Ok(Self {
            backend,
            tickets: Arc::new(Tickets::new(max_bytes)),
            public_url,
            auth_hash: Sha256::digest(auth.as_bytes()).into(),
            transfer_timeout,
        })
    }
}

#[derive(Clone)]
pub struct Mcp {
    app: App,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Target ID from kvm_status, 1-based on Comet.
    pub target: u8,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub target: u8,
    /// Defaults to 1280 pixels. 320..4096; coordinates refer to the full returned frame.
    pub max_width: Option<u32>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendInput {
    pub target: u8,
    pub input: Input,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendPower {
    pub target: u8,
    pub action: Power,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub target: u8,
    pub image: String,
    /// CD-ROM emulation, default true. Always read-only.
    pub cdrom: Option<bool>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageName {
    pub image: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transfer {
    pub direction: Direction,
    pub image: String,
    /// Exact byte count; required for uploads, omitted for downloads.
    pub size_bytes: Option<u64>,
    /// Validity to BEGIN transfer, 10..3600 seconds, default 900. Single-use.
    pub expires_in_seconds: Option<u64>,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revoke {
    pub ticket_id: String,
}

fn result(value: Result<Value>) -> CallToolResult {
    match value {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
    }
}

#[tool_router(server_handler)]
impl Mcp {
    #[tool(
        description = "Discover targets, selected target, power/media availability and keyboard layouts. Status is observed, not a guarantee of physical wiring."
    )]
    async fn kvm_status(&self) -> CallToolResult {
        result(
            self.app
                .backend
                .status()
                .await
                .and_then(|v| serde_json::to_value(v).map_err(|_| Error::Protocol)),
        )
    }

    #[tool(
        description = "Select target. Switching can reconnect USB; refused while virtual media is connected. Leave the installer target selected while its media is in use."
    )]
    async fn kvm_select(&self, Parameters(p): Parameters<Target>) -> CallToolResult {
        result(self.app.backend.select(p.target).await)
    }

    #[tool(
        description = "Select target and capture a JPEG as native MCP image content. Includes image dimensions and timestamp. Use normalized full-image coordinates for mouse input."
    )]
    async fn kvm_capture(&self, Parameters(p): Parameters<Capture>) -> CallToolResult {
        match self.app.backend.capture(p.target,p.max_width.unwrap_or(1280)).await {
            Ok(snapshot)=>CallToolResult::success(vec![
                ContentBlock::text(json!({"target":p.target,"width":snapshot.width,"height":snapshot.height,
                    "captured_at_unix_ms":snapshot.captured_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()}).to_string()),
                ContentBlock::image(STANDARD.encode(snapshot.jpeg),"image/jpeg")]),
            Err(error)=>result(Err(error)),
        }
    }
    #[tool(
        description = "Select target and send a key chord, emulated text, pointer movement, click, scroll or release-all. Text is NOT clipboard access. Chords use DOM key codes. Do not blindly retry input after a transport error."
    )]
    async fn kvm_input(&self, Parameters(p): Parameters<SendInput>) -> CallToolResult {
        result(self.app.backend.input(p.target, p.input).await)
    }

    #[tool(
        description = "Operate the selected target's ATX accessory. Hard off/reset can lose data. Requires a connected compatible board; never retries mutations automatically."
    )]
    async fn kvm_power(&self, Parameters(p): Parameters<SendPower>) -> CallToolResult {
        result(self.app.backend.power(p.target, p.action).await)
    }

    #[tool(description = "List stored virtual-media images, available space and connection state.")]
    async fn media_list(&self) -> CallToolResult {
        result(
            self.app
                .backend
                .media_list()
                .await
                .and_then(|v| serde_json::to_value(v).map_err(|_| Error::Protocol)),
        )
    }

    #[tool(
        description = "Mount an existing image read-only on a target. Eject existing media first. Do not change targets while installation depends on the virtual drive."
    )]
    async fn media_mount(&self, Parameters(p): Parameters<Mount>) -> CallToolResult {
        result(
            self.app
                .backend
                .media_mount(p.target, &p.image, p.cdrom.unwrap_or(true))
                .await,
        )
    }

    #[tool(
        description = "Disconnect virtual media from the explicitly named CURRENT target. Does not delete the image. Ensure the guest is no longer using it."
    )]
    async fn media_eject(&self, Parameters(p): Parameters<Target>) -> CallToolResult {
        result(self.app.backend.media_eject(p.target).await)
    }

    #[tool(
        description = "Permanently delete the named stored image. Refused while virtual media is connected. This is not eject."
    )]
    async fn media_delete(&self, Parameters(p): Parameters<ImageName>) -> CallToolResult {
        result(self.app.backend.media_delete(&p.image).await)
    }

    #[tool(
        description = "Mint a single-use, expiring private HTTP transfer URL scoped to one image and direction. Upload: raw PUT body and exact Content-Length. Download: GET. URLs are bearer secrets; use from a network-reachable transfer client, never log or share them. Large bytes bypass MCP. No Range/resume; mint a fresh URL after an interrupted transfer and inspect media state."
    )]
    async fn media_transfer(&self, Parameters(p): Parameters<Transfer>) -> CallToolResult {
        result(
            self.app
                .tickets
                .mint(
                    &self.app.public_url,
                    p.direction,
                    p.image,
                    p.size_bytes,
                    p.expires_in_seconds.unwrap_or(900),
                )
                .and_then(|grant| serde_json::to_value(grant).map_err(|_| Error::Protocol)),
        )
    }
    #[tool(
        description = "Revoke an unused media transfer grant using its ticket_id. Does not cancel an already-started transfer."
    )]
    async fn media_transfer_revoke(&self, Parameters(p): Parameters<Revoke>) -> CallToolResult {
        result(
            self.app
                .tickets
                .revoke(&p.ticket_id)
                .map(|_| json!({"revoked":true})),
        )
    }
}

pub fn router(app: App) -> Router {
    let service_app = app.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(Mcp {
                app: service_app.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts([
                app.public_url[url::Position::BeforeHost..url::Position::AfterPort].to_owned(),
                "127.0.0.1".into(),
                "localhost".into(),
                "::1".into(),
            ])
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );
    let mcp = Router::new()
        .nest_service("/mcp", service)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(64 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .merge(mcp)
        .route("/transfer/{token}", get(download).put(upload))
        .route("/health", get(|| async { "ok" }))
        .layer(middleware::from_fn_with_state(app.clone(), response_policy))
        .with_state(app)
}
async fn authenticate(State(app): State<App>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    let hash: [u8; 32] = Sha256::digest(supplied.as_bytes()).into();
    if !bool::from(hash.ct_eq(&app.auth_hash)) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "authentication required",
        )
            .into_response();
    }
    next.run(request).await
}
async fn response_policy(State(app): State<App>, request: Request, next: Next) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && origin.to_str().ok() != Some(app.public_url.origin().ascii_serialization().as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

async fn upload(
    State(app): State<App>,
    Path(token): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response> {
    if headers.contains_key(header::CONTENT_ENCODING)
        || headers.contains_key(header::TRANSFER_ENCODING)
        || headers.contains_key(header::CONTENT_RANGE)
    {
        return Err(Error::Invalid(
            "raw fixed-length uploads only; no encoding, chunked or range transfer",
        ));
    }
    let length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<u64>().ok())
        .ok_or(Error::Invalid("Content-Length required"))?;
    let ticket = app.tickets.claim(&token, Direction::Upload, Some(length))?;
    let mut chunks = body.into_data_stream();
    let stream = async_stream::try_stream! {
        let mut seen=0u64;
        while let Some(chunk)=chunks.next().await {
            let chunk=chunk.map_err(|_|std::io::Error::other("upload interrupted"))?;
            seen=seen.checked_add(chunk.len() as u64).ok_or_else(||std::io::Error::other("upload too large"))?;
            if seen>length {Err(std::io::Error::other("upload exceeds declared size"))?;}
            yield chunk;
        }
        if seen!=length {Err(std::io::Error::other("upload shorter than declared size"))?;}
    };
    let value = tokio::time::timeout(
        app.transfer_timeout,
        app.backend.upload(&ticket.image, length, Box::pin(stream)),
    )
    .await
    .map_err(|_| Error::Transport)??;
    Ok(axum::Json(value).into_response())
}
async fn download(
    State(app): State<App>,
    Path(token): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    // Axum GET also matches HEAD: reject it before consuming a single-use URL.
    if method != Method::GET {
        return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
    }
    if headers.contains_key(header::RANGE) {
        return Ok(StatusCode::RANGE_NOT_SATISFIABLE.into_response());
    }
    let ticket = app.tickets.claim(&token, Direction::Download, None)?;
    let deadline = tokio::time::Instant::now() + app.transfer_timeout;
    let file = tokio::time::timeout_at(deadline, app.backend.download(&ticket.image))
        .await
        .map_err(|_| Error::Transport)??;
    let size = file.size;
    let mut chunks = file.stream;
    let stream = async_stream::try_stream! {
        let mut seen=0u64;
        while let Some(chunk)=tokio::time::timeout_at(deadline,chunks.next()).await.map_err(|_|std::io::Error::other("download deadline exceeded"))? {
            let chunk=chunk?;
            seen=seen.checked_add(chunk.len() as u64).ok_or_else(||std::io::Error::other("download too large"))?;
            if seen>size {Err(std::io::Error::other("backend exceeded declared size"))?;}
            yield chunk;
        }
        if seen!=size {Err(std::io::Error::other("download incomplete"))?;}
    };
    let stream: crate::backend::ByteStream = Box::pin(stream);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", ticket.image),
        )
        .header(header::ACCEPT_RANGES, "none")
        .body(Body::from_stream(stream))
        .map_err(|_| Error::Protocol)
}
