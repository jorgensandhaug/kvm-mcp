use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{
        Query, Request, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use kvm_mcp::{
    comet::Comet,
    config::{CometConfig, Config, read_secret},
    server::{App, router},
};
use serde_json::{Value, json};
use std::{collections::HashMap, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tower::ServiceExt;

const AUTH: &str = "test-only-mcp-credential-at-least-32-characters";
#[derive(Default)]
struct Device {
    active: u8,
    connected: bool,
    images: HashMap<String, Vec<u8>>,
    calls: Vec<(String, HashMap<String, String>, Vec<u8>)>,
    reject_power: bool,
    disable_media: bool,
}
type Shared = Arc<Mutex<Device>>;
fn ok(value: Value) -> Response {
    axum::Json(json!({"ok":true,"result":value})).into_response()
}
async fn ws(
    State(_state): State<Shared>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if headers.get("Token").and_then(|v| v.to_str().ok()) != Some("test-session") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    upgrade.on_upgrade(|mut socket| async move {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
            if socket
                .send(Message::Text(r#"{"event_type":"pong","event":{}}"#.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}
async fn api(
    State(state): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_string();
    if path == "/api/auth/login" {
        return (
            [("set-cookie", "auth_token=test-session; HttpOnly")],
            axum::Json(json!({"ok":true,"result":{}})),
        )
            .into_response();
    }
    if request.headers().get("Token").and_then(|v| v.to_str().ok()) != Some("test-session") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let data = match to_bytes(request.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b.to_vec(),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let mut state = state.lock().await;
    state
        .calls
        .push((path.clone(), query.clone(), data.clone()));
    match path.as_str() {
        "/api/switch" => ok(json!({"summary":{"active_port":state.active}})),
        "/api/switch/set_active" => {
            state.active = query["port"].parse().unwrap();
            ok(json!({}))
        }
        "/api/msd" => ok(
            json!({"enabled":!state.disable_media,"drive":if state.disable_media {json!(null)} else {json!({"connected":state.connected})},"storage":{"images":state.images.iter().map(|(n,b)|(n.clone(),json!({"size":b.len()}))).collect::<HashMap<_,_>>()}}),
        ),
        "/api/atx" => ok(json!({"enabled":true,"leds":{"power":true}})),
        "/api/hid/keymaps" => ok(json!({"keymaps":{"default":"en-us","available":["en-us"]}})),
        "/api/atx/power" if state.reject_power => {
            (StatusCode::BAD_GATEWAY, "secret-upstream-details").into_response()
        }
        "/api/atx/power"
        | "/api/atx/click"
        | "/api/hid/events/send_shortcut"
        | "/api/hid/events/send_mouse_move"
        | "/api/hid/events/send_mouse_button"
        | "/api/hid/events/send_mouse_wheel"
        | "/api/hid/print"
        | "/api/hid/reset" => ok(json!({})),
        "/api/msd/set_params" => ok(json!({})),
        "/api/msd/set_connected" => {
            state.connected = query["connected"] == "true";
            ok(json!({}))
        }
        "/api/msd/remove" => {
            state.images.remove(&query["image"]);
            ok(json!({}))
        }
        "/api/msd/write" => {
            let size = data.len();
            state.images.insert(query["image"].clone(), data);
            ok(json!({"image":{"name":query["image"],"written":size,"size":size}}))
        }
        "/api/msd/read" => match state.images.get(&query["image"]) {
            Some(b) => ([("content-length", b.len().to_string())], b.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        "/api/streamer/snapshot" => {
            let image = image::RgbImage::from_pixel(1600, 900, image::Rgb([12, 34, 56]));
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut bytes)
                .encode_image(&image)
                .unwrap();
            ([("content-type", "image/jpeg")], bytes).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

struct Rig {
    router: Router,
    state: Shared,
    task: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn setup() -> Rig {
    let state = Arc::new(Mutex::new(Device::default()));
    let api = Router::new()
        .route("/api/ws", get(ws))
        .route("/api/{*path}", any(api))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, api).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("backend.secret");
    std::fs::write(&path, "test-only-backend-password").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = Comet::new(
        CometConfig {
            url: format!("http://{addr}").parse().unwrap(),
            username: "admin".into(),
            password_file: path,
            ca_file: None,
            ports: 4,
        },
        Duration::from_secs(60),
    )
    .unwrap();
    let app = App::new(
        Arc::new(backend),
        "https://kvm.example.test:8443".parse().unwrap(),
        AUTH,
        4 * 1024 * 1024,
        Duration::from_secs(60),
    )
    .unwrap();
    Rig {
        router: router(app),
        state,
        task,
        _dir: dir,
    }
}
async fn rpc(router: &Router, method: &str, params: Value) -> Value {
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("authorization", format!("Bearer {AUTH}"))
        .header("host", "kvm.example.test:8443")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
        ))
        .unwrap();
    let response = router.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}
async fn tool(router: &Router, name: &str, args: Value) -> Value {
    rpc(router, "tools/call", json!({"name":name,"arguments":args})).await["result"].clone()
}
fn text(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}
fn path(grant: &Value) -> String {
    url::Url::parse(grant["url"].as_str().unwrap())
        .unwrap()
        .path()
        .to_owned()
}

#[tokio::test]
async fn mcp_handshake_controls_images_and_mount_safety() {
    let rig = setup().await;
    let init=rpc(&rig.router,"initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}})).await;
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let tools = rpc(&rig.router, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 11);
    let status = text(&tool(&rig.router, "kvm_status", json!({})).await);
    assert_eq!(status["active_target"], 1);
    let capture = tool(
        &rig.router,
        "kvm_capture",
        json!({"target":2,"max_width":800}),
    )
    .await;
    assert_eq!(capture["content"][1]["type"], "image");
    let jpeg = STANDARD
        .decode(capture["content"][1]["data"].as_str().unwrap())
        .unwrap();
    let image = image::load_from_memory(&jpeg).unwrap();
    assert_eq!((image.width(), image.height()), (800, 450));
    assert_eq!(text(&capture)["target"], 2);
    for input in [
        json!({"action":"chord","keys":["ControlLeft","KeyA"]}),
        json!({"action":"text","text":"hello æøå","keymap":"en-us"}),
        json!({"action":"move","x":1.0,"y":0.0}),
        json!({"action":"click","button":"left"}),
        json!({"action":"scroll","x":0,"y":1}),
        json!({"action":"release_all"}),
    ] {
        let result = tool(&rig.router, "kvm_input", json!({"target":2,"input":input})).await;
        assert_ne!(result["isError"], true, "{result}");
    }
    let power = tool(&rig.router, "kvm_power", json!({"target":3,"action":"on"})).await;
    assert_ne!(power["isError"], true);
    let mount = tool(
        &rig.router,
        "media_mount",
        json!({"target":3,"image":"installer.iso"}),
    )
    .await;
    assert_ne!(mount["isError"], true);
    assert_eq!(
        tool(&rig.router, "kvm_select", json!({"target":4})).await["isError"],
        true
    );
    assert_eq!(
        tool(&rig.router, "media_eject", json!({"target":2})).await["isError"],
        true
    );
    assert_ne!(
        tool(&rig.router, "media_eject", json!({"target":3})).await["isError"],
        true
    );
    assert_ne!(
        tool(&rig.router, "kvm_select", json!({"target":4})).await["isError"],
        true
    );
    let state = rig.state.lock().await;
    assert_eq!(state.active, 3);
    assert!(
        state.calls.iter().any(
            |(p, q, _)| p == "/api/hid/events/send_shortcut" && q["keys"] == "ControlLeft,KeyA"
        )
    );
    assert!(
        state
            .calls
            .iter()
            .any(|(p, q, _)| p == "/api/hid/events/send_mouse_move"
                && q["to_x"] == "32767"
                && q["to_y"] == "-32768")
    );
    assert!(
        state
            .calls
            .iter()
            .any(|(p, _, b)| p == "/api/hid/print" && b == "hello æøå".as_bytes())
    );
    assert!(
        !state
            .calls
            .iter()
            .any(|(p, _, _)| p.contains("/switch/atx"))
    );
}

#[tokio::test]
async fn transfers_stream_beyond_mcp_limits_and_are_single_use() {
    let rig = setup().await;
    let data = vec![42u8; 3 * 1024 * 1024];
    let grant = text(
        &tool(
            &rig.router,
            "media_transfer",
            json!({"direction":"upload","image":"linux.iso","size_bytes":data.len()}),
        )
        .await,
    );
    let request = Request::builder()
        .method("PUT")
        .uri(path(&grant))
        .header("content-length", data.len())
        .body(Body::from(data.clone()))
        .unwrap();
    let response = rig.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(rig.state.lock().await.images["linux.iso"], data);
    let inventory = text(&tool(&rig.router, "media_list", json!({})).await);
    assert_eq!(inventory["images"][0]["name"], "linux.iso");
    assert_eq!(inventory["images"][0]["size_bytes"], data.len());
    let request = Request::builder()
        .method("PUT")
        .uri(path(&grant))
        .header("content-length", data.len())
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        rig.router.clone().oneshot(request).await.unwrap().status(),
        StatusCode::GONE
    );
    let download = text(
        &tool(
            &rig.router,
            "media_transfer",
            json!({"direction":"download","image":"linux.iso"}),
        )
        .await,
    );
    for (method, range, status) in [
        ("HEAD", false, StatusCode::METHOD_NOT_ALLOWED),
        ("GET", true, StatusCode::RANGE_NOT_SATISFIABLE),
    ] {
        let mut req = Request::builder().method(method).uri(path(&download));
        if range {
            req = req.header("range", "bytes=0-10");
        }
        assert_eq!(
            rig.router
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            status
        );
    }
    let response = rig
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path(&download))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        data
    );
    assert_eq!(
        rig.router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path(&download))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
}

#[tokio::test]
async fn authentication_origin_and_errors_are_safe() {
    let rig = setup().await;
    let response = rig
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = rig
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    rig.state.lock().await.reject_power = true;
    let result = tool(
        &rig.router,
        "kvm_power",
        json!({"target":1,"action":"off_hard"}),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert!(!result.to_string().contains("secret-upstream"));
    assert_eq!(
        rig.state
            .lock()
            .await
            .calls
            .iter()
            .filter(|(p, _, _)| p == "/api/atx/power")
            .count(),
        1
    );
    assert_eq!(
        tool(
            &rig.router,
            "media_transfer",
            json!({"direction":"upload","image":"../bad.iso","size_bytes":10})
        )
        .await["isError"],
        true
    );
}

#[test]
fn config_rejects_cleartext_remote_and_world_readable_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, "dummy").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_secret(&secret).is_err());
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_secret(&secret).unwrap(), "dummy");
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(read_secret(&secret).is_err());
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&secret, "x".repeat(4097)).unwrap();
    assert!(read_secret(&secret).is_err());
    let cfg: Config = toml::from_str(
        r#"listen="127.0.0.1:9100"
public_url="https://kvm.example.test:8443"
auth_token_file="/example/auth.secret"
[comet]
url="http://192.0.2.1"
password_file="/example/backend.secret"
"#,
    )
    .unwrap();
    assert!(cfg.validate().is_err());
}

#[tokio::test]
async fn upload_length_and_encoding_checks_do_not_report_false_success() {
    let rig = setup().await;
    for (name, data) in [("short.iso", vec![1u8; 2]), ("long.iso", vec![1u8; 12])] {
        let grant = text(
            &tool(
                &rig.router,
                "media_transfer",
                json!({"direction":"upload","image":name,"size_bytes":10}),
            )
            .await,
        );
        let req = Request::builder()
            .method("PUT")
            .uri(path(&grant))
            .header("content-length", 10)
            .body(Body::from(data))
            .unwrap();
        let response = rig.router.clone().oneshot(req).await.unwrap();
        assert!(!response.status().is_success());
        assert!(!rig.state.lock().await.images.contains_key(name));
    }
    let grant = text(
        &tool(
            &rig.router,
            "media_transfer",
            json!({"direction":"upload","image":"safe.iso","size_bytes":10}),
        )
        .await,
    );
    let bad = Request::builder()
        .method("PUT")
        .uri(path(&grant))
        .header("content-length", 10)
        .header("content-encoding", "gzip")
        .body(Body::from(vec![1u8; 10]))
        .unwrap();
    assert_eq!(
        rig.router.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let good = Request::builder()
        .method("PUT")
        .uri(path(&grant))
        .header("content-length", 10)
        .body(Body::from(vec![1u8; 10]))
        .unwrap();
    assert_eq!(
        rig.router.clone().oneshot(good).await.unwrap().status(),
        StatusCode::OK
    );
    // A new ticket cannot overwrite an existing image implicitly.
    let other = text(
        &tool(
            &rig.router,
            "media_transfer",
            json!({"direction":"upload","image":"safe.iso","size_bytes":10}),
        )
        .await,
    );
    let req = Request::builder()
        .method("PUT")
        .uri(path(&other))
        .header("content-length", 10)
        .body(Body::from(vec![2u8; 10]))
        .unwrap();
    assert_eq!(
        rig.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(rig.state.lock().await.images["safe.iso"], vec![1u8; 10]);
}

#[tokio::test]
async fn disabled_virtual_media_does_not_block_port_selection() {
    let rig = setup().await;
    rig.state.lock().await.disable_media = true;
    let selected = tool(&rig.router, "kvm_select", json!({"target":2})).await;
    assert_ne!(selected["isError"], true, "{selected}");
    assert_eq!(text(&selected)["active_target"], 2);
    let mount = tool(
        &rig.router,
        "media_mount",
        json!({"target":2,"image":"linux.iso"}),
    )
    .await;
    assert_eq!(mount["isError"], true);
    assert!(
        mount["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
}
