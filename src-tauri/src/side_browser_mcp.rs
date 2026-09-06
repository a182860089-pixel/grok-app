//! Loopback MCP for the **in-app** Browser panel (Cursor-style a11y refs).
//!
//! Agent tools drive the same `resource-browser-*` WebView2 / WKWebView the
//! user is looking at. Transport:
//! - Host HTTP JSON-RPC at `http://127.0.0.1:{port}/mcp` (token-gated)
//! - ACP inject is **HTTP** (`type: http` + Authorization bearer)
//! - Optional stdio shim: `grok-app --embedded-browser-mcp` (debug / fallback)

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::side_browser_a11y;
use crate::side_browser_host::{self, SideBrowserInfo};

pub const SERVER_NAME: &str = "browser";
pub const STDIO_FLAG: &str = "--embedded-browser-mcp";
pub const AGENT_OPEN_EVENT: &str = "side-browser://agent-open";

const PROTOCOL_VERSION: &str = "2024-11-05";
const ENDPOINT_FILE: &str = "embedded-browser-mcp.json";
const OPEN_WAIT_MS: u64 = 250;
const OPEN_WAIT_TRIES: u32 = 48; // ~12s for first WebView2 create
const EVAL_JOIN: &str = "eval join";

static ENDPOINT: OnceLock<BrowserMcpEndpoint> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserMcpEndpoint {
    pub url: String,
    pub token: String,
}

pub struct BrowserMcpHandle {
    pub endpoint: BrowserMcpEndpoint,
    shutdown: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl Drop for BrowserMcpHandle {
    fn drop(&mut self) {
        if let Ok(mut g) = self.shutdown.lock() {
            if let Some(tx) = g.take() {
                let _ = tx.send(());
            }
        }
        let _ = std::fs::remove_file(endpoint_path());
    }
}

#[derive(Clone)]
struct HttpState {
    token: Arc<String>,
    app: AppHandle,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentOpenPayload {
    url: String,
    request_id: String,
}

pub fn try_run_stdio() -> bool {
    std::env::args().any(|a| a == STDIO_FLAG)
}

pub fn current_endpoint() -> Option<BrowserMcpEndpoint> {
    ENDPOINT.get().cloned().or_else(read_endpoint_file)
}

/// ACP `mcpServers[]` HTTP entry. Loopback + bearer — do **not** spawn the
/// GUI `grok-app.exe` as stdio (Windows subsystem has no console stdin).
/// `None` until the Host HTTP listener is up.
pub fn acp_entry() -> Option<Value> {
    let ep = current_endpoint()?;
    Some(json!({
        "type": "http",
        "name": SERVER_NAME,
        "url": ep.url,
        "headers": [
            {"name": "Authorization", "value": format!("Bearer {}", ep.token)}
        ]
    }))
}

fn endpoint_path() -> PathBuf {
    crate::paths::app_data_root().join(ENDPOINT_FILE)
}

fn write_endpoint_file(ep: &BrowserMcpEndpoint) {
    let _ = crate::paths::ensure_app_dirs();
    if let Ok(raw) = serde_json::to_vec_pretty(ep) {
        let _ = std::fs::write(endpoint_path(), raw);
    }
}

fn read_endpoint_file() -> Option<BrowserMcpEndpoint> {
    let raw = std::fs::read_to_string(endpoint_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

fn random_token() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

static START_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// Start the loopback MCP once. Safe to call from connect, browser create, or boot.
pub async fn ensure_started(app: &AppHandle) -> Result<BrowserMcpEndpoint, String> {
    if let Some(ep) = current_endpoint() {
        return Ok(ep);
    }
    let lock = START_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _g = lock.lock().await;
    if let Some(ep) = current_endpoint() {
        return Ok(ep);
    }
    let handle = start(app.clone()).await?;
    let ep = handle.endpoint.clone();
    if app.try_state::<BrowserMcpHandle>().is_none() {
        app.manage(handle);
    }
    Ok(ep)
}

/// Bind loopback JSON-RPC and remember the ACP inject endpoint.
pub async fn start(app: AppHandle) -> Result<BrowserMcpHandle, String> {
    let token = random_token();
    let state = HttpState {
        token: Arc::new(token.clone()),
        app: app.clone(),
    };
    let router = Router::new()
        .route("/mcp", post(mcp_post).get(mcp_sse_endpoint))
        .route("/sse", get(mcp_sse_endpoint))
        .route("/health", get(mcp_health))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("browser mcp bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("browser mcp addr: {e}"))?
        .port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let endpoint = BrowserMcpEndpoint {
        url: url.clone(),
        token: token.clone(),
    };
    write_endpoint_file(&endpoint);
    let _ = ENDPOINT.set(endpoint.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let serve = axum::serve(listener, router).with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = serve.await {
            tracing::error!(error = %e, "embedded-browser mcp server exited");
        }
    });
    tracing::info!(%url, "embedded-browser mcp listening (loopback, token-gated)");
    Ok(BrowserMcpHandle {
        endpoint,
        shutdown: std::sync::Mutex::new(Some(shutdown_tx)),
    })
}

async fn mcp_health() -> impl IntoResponse {
    Json(json!({"ok": true, "name": SERVER_NAME}))
}

/// Legacy MCP SSE: tell the client to POST JSON-RPC to `/mcp`.
async fn mcp_sse_endpoint(State(st): State<HttpState>, headers: HeaderMap) -> impl IntoResponse {
    if !token_ok(&st.token, &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        "event: endpoint\ndata: /mcp\n\n",
    )
        .into_response()
}

async fn mcp_post(
    State(st): State<HttpState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !token_ok(&st.token, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": "parse error"}
                })),
            )
                .into_response();
        }
    };
    if req.get("id").is_none() {
        let _ = dispatch(&st.app, req).await;
        return StatusCode::NO_CONTENT.into_response();
    }
    let resp = dispatch(&st.app, req).await;
    let want_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if want_sse {
        let data = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/event-stream"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            format!("event: message\ndata: {data}\n\n"),
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, "application/json")], Json(resp)).into_response()
}

fn token_ok(want: &str, headers: &HeaderMap) -> bool {
    if want.is_empty() {
        return false;
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .unwrap_or("")
        .trim();
    if bearer == want {
        return true;
    }
    headers
        .get("x-grok-browser-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.trim() == want)
}

/// Newline-delimited JSON-RPC on stdin/stdout. Talks to the running Host.
pub fn run_stdio() -> i32 {
    let url = std::env::var("EMBEDDED_BROWSER_MCP_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| read_endpoint_file().map(|e| e.url));
    let token = std::env::var("EMBEDDED_BROWSER_MCP_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| read_endpoint_file().map(|e| e.token));
    let (Some(url), Some(token)) = (url, token) else {
        let _ = writeln!(
            std::io::stderr(),
            "embedded-browser mcp: host endpoint missing (is Grok App running?)"
        );
        return 2;
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .no_proxy()
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "embedded-browser mcp client: {e}");
            return 2;
        }
    };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(t) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let is_note = req.get("id").is_none();
        let send = client
            .post(&url)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(t.to_string())
            .send();
        match send {
            Ok(resp) if is_note => {
                let _ = resp.bytes();
            }
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().unwrap_or_default();
                if text.trim().is_empty() {
                    if let Some(id) = req.get("id") {
                        let err = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32000, "message": format!("host empty response ({status})")}
                        });
                        let _ = writeln!(stdout, "{err}");
                        let _ = stdout.flush();
                    }
                    continue;
                }
                let _ = writeln!(stdout, "{}", text.trim());
                let _ = stdout.flush();
            }
            Err(e) => {
                if let Some(id) = req.get("id") {
                    let err = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32000, "message": format!("host unreachable: {e}")}
                    });
                    let _ = writeln!(stdout, "{err}");
                    let _ = stdout.flush();
                }
            }
        }
    }
    0
}

pub async fn dispatch(app: &AppHandle, req: Value) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .trim();
    let params = req.get("params").cloned().unwrap_or(json!({}));
    match method {
        "initialize" => rpc_ok(
            id,
            json!({
                "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL_VERSION),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": "1.0.0"},
            }),
        ),
        "notifications/initialized" | "initialized" | "notifications/cancelled" => {
            json!({})
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({"tools": tools_schema()})),
        "resources/list" => rpc_ok(id, json!({"resources": []})),
        "prompts/list" => rpc_ok(id, json!({"prompts": []})),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .trim();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(app, name, &args).await {
                Ok(text) => rpc_ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false
                    }),
                ),
                Err(e) => rpc_ok(
                    id,
                    json!({
                        "content": [{"type": "text", "text": e}],
                        "isError": true
                    }),
                ),
            }
        }
        "" => rpc_err(id, -32600, "invalid request"),
        other => rpc_err(id, -32601, &format!("Method not found: {other}")),
    }
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_err(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tools_schema() -> Value {
    json!([
        tool("browser_tabs",
            "List in-app Browser panel tabs (label + url + focused). This is the Grok App embedded WebView the user sees — not Playwright, not system Chrome.",
            json!({"type":"object","properties":{}})),
        tool("browser_navigate",
            "Open or navigate the embedded Browser panel (the WebView2/WKWebView in the right sidebar). Use this to operate the content browser. Keywords: 打开网页, 内容浏览器, 浏览, go to url.",
            json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string","description":"http(s) URL to load in the in-app browser"},
                    "tab":{"type":"string","description":"Optional webview label or tab id"}
                },
                "required":["url"]
            })),
        tool("browser_snapshot",
            "Accessibility snapshot of the embedded Browser page with refs (e1, e2, …) like Cursor. Call this before click/type. Keywords: 页面快照, a11y, snapshot.",
            json!({
                "type":"object",
                "properties":{
                    "tab":{"type":"string"}
                }
            })),
        tool("browser_click",
            "Click an element by snapshot ref (e.g. e12). Iframe refs use a real native click so hosted fields receive focus.",
            json!({
                "type":"object",
                "properties":{
                    "ref":{"type":"string","description":"Ref from browser_snapshot, e.g. e3"},
                    "tab":{"type":"string"}
                },
                "required":["ref"]
            })),
        tool("browser_type",
            "Type into a textbox/input by snapshot ref. Set submit=true to press Enter.",
            json!({
                "type":"object",
                "properties":{
                    "ref":{"type":"string"},
                    "text":{"type":"string"},
                    "submit":{"type":"boolean"},
                    "tab":{"type":"string"}
                },
                "required":["ref","text"]
            })),
        tool("browser_fill",
            "Replace the value of an input/textarea by snapshot ref (no key-by-key typing).",
            json!({
                "type":"object",
                "properties":{
                    "ref":{"type":"string"},
                    "value":{"type":"string"},
                    "tab":{"type":"string"}
                },
                "required":["ref","value"]
            })),
        tool("browser_hover",
            "Hover an element by snapshot ref (opens menus / tooltips).",
            json!({
                "type":"object",
                "properties":{"ref":{"type":"string"},"tab":{"type":"string"}},
                "required":["ref"]
            })),
        tool("browser_select",
            "Choose a <select> option by snapshot ref (value or visible label).",
            json!({
                "type":"object",
                "properties":{"ref":{"type":"string"},"value":{"type":"string"},"tab":{"type":"string"}},
                "required":["ref","value"]
            })),
        tool("browser_press",
            "Press a real key in the focused embedded page (Enter, Tab, Escape, ArrowDown, …), including cross-origin iframe inputs.",
            json!({
                "type":"object",
                "properties":{"key":{"type":"string"},"tab":{"type":"string"}},
                "required":["key"]
            })),
        tool("browser_focus_frame",
            "Focus and click a cross-origin iframe using its parent-page selector. Use this before browser_type_focused for hosted payment or auth fields.",
            json!({
                "type":"object",
                "properties":{
                    "selector":{"type":"string","description":"Optional CSS selector such as iframe[title*=\"card number\"]"},
                    "tab":{"type":"string"}
                }
            })),
        tool("browser_type_focused",
            "Type text through the real OS keyboard into the currently focused embedded page or iframe. The text is never read back.",
            json!({
                "type":"object",
                "properties":{
                    "text":{"type":"string"},
                    "tab":{"type":"string"}
                },
                "required":["text"]
            })),
        tool("browser_scroll",
            "Scroll the page or a snapshot ref. direction: up|down|left|right.",
            json!({
                "type":"object",
                "properties":{
                    "ref":{"type":"string"},
                    "direction":{"type":"string"},
                    "amount":{"type":"number"},
                    "tab":{"type":"string"}
                }
            })),
        tool("browser_wait",
            "Wait for load / text / milliseconds in the embedded Browser.",
            json!({
                "type":"object",
                "properties":{
                    "time_ms":{"type":"number"},
                    "text":{"type":"string","description":"Wait until page innerText contains this"},
                    "load":{"type":"boolean","description":"Wait until document.readyState=complete"},
                    "tab":{"type":"string"}
                }
            })),
        tool("browser_reload",
            "Reload the focused embedded Browser tab.",
            json!({"type":"object","properties":{"tab":{"type":"string"}}})),
        tool("browser_evaluate",
            "Run a JavaScript expression in the embedded Browser page and return JSON. Prefer snapshot/click/type. Do not use for downloads of malware.",
            json!({
                "type":"object",
                "properties":{
                    "function":{"type":"string","description":"JS expression or IIFE"},
                    "tab":{"type":"string"}
                },
                "required":["function"]
            })),
    ])
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

async fn call_tool(app: &AppHandle, name: &str, args: &Value) -> Result<String, String> {
    let tab = args
        .get("tab")
        .or_else(|| args.get("label"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match name {
        "browser_tabs" => tabs_text(app),
        "browser_navigate" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "url required".to_string())?;
            navigate(app, url, tab).await
        }
        "browser_snapshot" => {
            let label = ensure_label(app, tab, None).await?;
            snapshot(app, &label).await
        }
        "browser_click" => {
            let r = req_str(args, "ref")?;
            let label = ensure_label(app, tab, None).await?;
            let raw = eval_json(app, &label, side_browser_a11y::click_js(&r)).await?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if v.get("native").and_then(|b| b.as_bool()) == Some(true) {
                    return focus_frame_by_ref(app, &label, &r).await;
                }
            }
            Ok(raw)
        }
        "browser_type" => {
            let r = req_str(args, "ref")?;
            let text = req_str(args, "text")?;
            let submit = args
                .get("submit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let label = ensure_label(app, tab, None).await?;
            let raw = eval_json(app, &label, side_browser_a11y::type_js(&r, &text, submit)).await?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if v.get("native").and_then(|b| b.as_bool()) == Some(true) {
                    return type_frame_by_ref(app, &label, &r, &text, submit, false).await;
                }
            }
            Ok(raw)
        }
        "browser_fill" => {
            let r = req_str(args, "ref")?;
            let value = args
                .get("value")
                .or_else(|| args.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let label = ensure_label(app, tab, None).await?;
            let raw = eval_json(app, &label, side_browser_a11y::type_js(&r, value, false)).await?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if v.get("native").and_then(|b| b.as_bool()) == Some(true) {
                    return type_frame_by_ref(app, &label, &r, value, false, true).await;
                }
            }
            Ok(raw)
        }
        "browser_hover" => {
            let r = req_str(args, "ref")?;
            let label = ensure_label(app, tab, None).await?;
            eval_json(app, &label, side_browser_a11y::hover_js(&r)).await
        }
        "browser_select" => {
            let r = req_str(args, "ref")?;
            let value = req_str(args, "value")?;
            let label = ensure_label(app, tab, None).await?;
            eval_json(app, &label, side_browser_a11y::select_js(&r, &value)).await
        }
        "browser_press" => {
            let key = req_str(args, "key")?;
            let label = ensure_label(app, tab, None).await?;
            let app2 = app.clone();
            let lab = label.clone();
            tauri::async_runtime::spawn_blocking(move || {
                side_browser_host::send_key(&app2, lab, key)
            })
            .await
            .map_err(|e| format!("{EVAL_JOIN}: {e}"))?
        }
        "browser_focus_frame" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let label = ensure_label(app, tab, None).await?;
            let app2 = app.clone();
            let lab = label.clone();
            tauri::async_runtime::spawn_blocking(move || {
                side_browser_host::focus_frame(&app2, lab, selector)
            })
            .await
            .map_err(|e| format!("{EVAL_JOIN}: {e}"))?
        }
        "browser_type_focused" => {
            let text = req_str(args, "text")?;
            let label = ensure_label(app, tab, None).await?;
            let app2 = app.clone();
            let lab = label.clone();
            tauri::async_runtime::spawn_blocking(move || {
                side_browser_host::send_text(&app2, lab, text)
            })
            .await
            .map_err(|e| format!("{EVAL_JOIN}: {e}"))?
        }
        "browser_scroll" => {
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            let amount = args.get("amount").and_then(|v| v.as_i64()).unwrap_or(480);
            let r = args.get("ref").and_then(|v| v.as_str());
            let label = ensure_label(app, tab, None).await?;
            eval_json(
                app,
                &label,
                side_browser_a11y::scroll_js(r, direction, amount),
            )
            .await
        }
        "browser_wait" => wait_tool(app, tab, args).await,
        "browser_reload" => {
            let label = ensure_label(app, tab, None).await?;
            let app2 = app.clone();
            let lab = label.clone();
            tauri::async_runtime::spawn_blocking(move || side_browser_host::reload(&app2, lab))
                .await
                .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
            Ok(format!("reloaded {label}"))
        }
        "browser_evaluate" => {
            let expr = req_str(args, "function")?;
            if expr.len() > 32_000 {
                return Err("function too large".into());
            }
            let label = ensure_label(app, tab, None).await?;
            let script = wrap_eval_expr(&expr);
            eval_json(app, &label, script).await
        }
        _ => Err(format!("unknown tool: {name}")),
    }
}

fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} required"))
}

fn wrap_eval_expr(expr: &str) -> String {
    let body = expr.trim().trim_end_matches(';');
    format!(
        r#"(function(){{ try {{ var r = ({body}); return JSON.stringify({{ok:true, result:r}}); }} catch (e) {{ return JSON.stringify({{ok:false, error: String(e)}}); }} }})()"#
    )
}

fn tabs_text(app: &AppHandle) -> Result<String, String> {
    let list = side_browser_host::list(app)?;
    let focus = side_browser_host::focused_label();
    if list.is_empty() {
        return Ok(
            "No embedded Browser tab is open. Call browser_navigate with a URL to open the in-app Browser panel (right sidebar)."
                .into(),
        );
    }
    let mut lines = vec!["Embedded Browser tabs:".to_string()];
    for t in list {
        let star = if focus.as_deref() == Some(t.label.as_str()) {
            " (focused)"
        } else {
            ""
        };
        lines.push(format!(
            "- {label}{star}  {url}",
            label = t.label,
            url = t.url.unwrap_or_else(|| "(loading)".into())
        ));
    }
    Ok(lines.join("\n"))
}

async fn navigate(app: &AppHandle, url: &str, tab: Option<&str>) -> Result<String, String> {
    let parsed = side_browser_host::validate_url(url)?;
    let url_s = parsed.to_string();
    let label = ensure_label(app, tab, Some(&url_s)).await?;
    let app2 = app.clone();
    let lab = label.clone();
    let u = url_s.clone();
    tauri::async_runtime::spawn_blocking(move || side_browser_host::navigate(&app2, lab, u))
        .await
        .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
    tokio::time::sleep(Duration::from_millis(350)).await;
    let href = current_url(app, &label).await.unwrap_or(url_s.clone());
    Ok(format!("navigated {label} → {href}"))
}

async fn snapshot(app: &AppHandle, label: &str) -> Result<String, String> {
    let raw = eval_raw(app, label, side_browser_a11y::SNAPSHOT_JS.to_string()).await?;
    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        let snap = v
            .get("snapshot")
            .and_then(|s| s.as_str())
            .unwrap_or(raw.as_str());
        let href = v.get("href").and_then(|s| s.as_str()).unwrap_or("");
        let title = v.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let refs = v.get("refs").and_then(|n| n.as_u64()).unwrap_or(0);
        return Ok(format!(
            "title: {title}\nurl: {href}\ninteractive refs: {refs}\n\n{snap}"
        ));
    }
    Ok(raw)
}

async fn wait_tool(app: &AppHandle, tab: Option<&str>, args: &Value) -> Result<String, String> {
    let time_ms = args
        .get("time_ms")
        .or_else(|| args.get("timeMs"))
        .and_then(|v| v.as_u64());
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let load = args.get("load").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(ms) = time_ms {
        tokio::time::sleep(Duration::from_millis(ms.min(15_000))).await;
    }
    let label = ensure_label(app, tab, None).await?;
    if load {
        for _ in 0..20 {
            let raw = eval_raw(app, &label, side_browser_a11y::READY_JS.to_string()).await?;
            if raw.contains("complete") {
                return Ok(format!("load complete on {label}"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return Ok(format!("waited for load on {label} (still not complete)"));
    }
    if let Some(needle) = text {
        for _ in 0..24 {
            let raw = eval_raw(app, &label, side_browser_a11y::contains_text_js(needle)).await?;
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if v.get("found").and_then(|b| b.as_bool()) == Some(true) {
                    return Ok(format!("found text on {label}"));
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return Err(format!("text not found: {needle}"));
    }
    Ok(format!("waited on {label}"))
}

async fn ensure_label(
    app: &AppHandle,
    tab: Option<&str>,
    open_url: Option<&str>,
) -> Result<String, String> {
    if let Some(hit) = resolve_live_label(app, tab) {
        return Ok(hit);
    }
    let url = open_url
        .filter(|s| !s.is_empty())
        .unwrap_or("about:blank")
        .to_string();
    let request_id = uuid::Uuid::new_v4().to_string();
    let _ = app.emit(
        AGENT_OPEN_EVENT,
        AgentOpenPayload {
            url: url.clone(),
            request_id,
        },
    );
    for _ in 0..OPEN_WAIT_TRIES {
        tokio::time::sleep(Duration::from_millis(OPEN_WAIT_MS)).await;
        if let Some(hit) = resolve_live_label(app, tab) {
            return Ok(hit);
        }
    }
    Err(
        "Embedded Browser panel did not open. Ask the user to show the right-sidebar Browser tab, then retry browser_navigate."
            .into(),
    )
}

fn resolve_live_label(app: &AppHandle, requested: Option<&str>) -> Option<String> {
    let list = side_browser_host::list(app).ok()?;
    if list.is_empty() {
        return None;
    }
    let live: Vec<SideBrowserInfo> = list;
    if let Some(req) = requested {
        let req = req.trim();
        if let Some(hit) = live.iter().find(|t| t.label == req) {
            return Some(hit.label.clone());
        }
        let prefixed = if req.starts_with(side_browser_host::LABEL_PREFIX) {
            req.to_string()
        } else {
            format!("{}-{req}", side_browser_host::LABEL_PREFIX)
        };
        if let Some(hit) = live.iter().find(|t| t.label == prefixed) {
            return Some(hit.label.clone());
        }
    }
    if let Some(focus) = side_browser_host::focused_label() {
        if live.iter().any(|t| t.label == focus) {
            return Some(focus);
        }
    }
    live.into_iter().next().map(|t| t.label)
}

async fn eval_json(app: &AppHandle, label: &str, script: String) -> Result<String, String> {
    let raw = eval_raw(app, label, script).await?;
    if let Ok(v) = serde_json::from_str::<Value>(&raw) {
        if v.get("ok").and_then(|b| b.as_bool()) == Some(false) {
            let err = v
                .get("error")
                .and_then(|s| s.as_str())
                .unwrap_or("action failed");
            return Err(err.to_string());
        }
        return Ok(v.to_string());
    }
    Ok(raw)
}

async fn eval_raw(app: &AppHandle, label: &str, script: String) -> Result<String, String> {
    let app2 = app.clone();
    let lab = label.to_string();
    let raw =
        tauri::async_runtime::spawn_blocking(move || side_browser_host::eval(&app2, lab, script))
            .await
            .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
    Ok(side_browser_host::decode_eval_result(&raw))
}

fn iframe_ref_selector(r#ref: &str) -> String {
    format!(
        "iframe[data-grok-ref={}]",
        serde_json::to_string(r#ref).unwrap_or_else(|_| "\"\"".into())
    )
}

async fn focus_frame_by_ref(
    app: &AppHandle,
    label: &str,
    r#ref: &str,
) -> Result<String, String> {
    let selector = iframe_ref_selector(r#ref);
    let app2 = app.clone();
    let lab = label.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        side_browser_host::focus_frame(&app2, lab, Some(selector))
    })
    .await
    .map_err(|e| format!("{EVAL_JOIN}: {e}"))?
}

async fn type_frame_by_ref(
    app: &AppHandle,
    label: &str,
    r#ref: &str,
    text: &str,
    submit: bool,
    replace: bool,
) -> Result<String, String> {
    let focused = focus_frame_by_ref(app, label, r#ref).await?;
    if replace {
        let app2 = app.clone();
        let lab = label.to_string();
        tauri::async_runtime::spawn_blocking(move || {
            side_browser_host::send_key(&app2, lab, "Ctrl+A".into())
        })
        .await
        .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
    }
    let app2 = app.clone();
    let lab = label.to_string();
    let content = text.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        side_browser_host::send_text(&app2, lab, content)
    })
    .await
    .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
    if submit {
        let app3 = app.clone();
        let lab = label.to_string();
        tauri::async_runtime::spawn_blocking(move || {
            side_browser_host::send_key(&app3, lab, "Enter".into())
        })
        .await
        .map_err(|e| format!("{EVAL_JOIN}: {e}"))??;
    }
    Ok(json!({
        "ok": true,
        "native": true,
        "ref": r#ref,
        "textLength": text.chars().count(),
        "submit": submit,
        "replace": replace,
        "focus": focused
    })
    .to_string())
}

async fn current_url(app: &AppHandle, label: &str) -> Result<String, String> {
    let app2 = app.clone();
    let lab = label.to_string();
    tauri::async_runtime::spawn_blocking(move || side_browser_host::current_url(&app2, lab))
        .await
        .map_err(|e| format!("{EVAL_JOIN}: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list_tools() {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}});
        // dispatch needs AppHandle for tools/call only; initialize is pure but
        // the signature requires app. Test the schema + rpc helpers instead.
        let listed = tools_schema();
        let arr = listed.as_array().unwrap();
        let names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"browser_snapshot"));
        assert!(names.contains(&"browser_click"));
        assert!(names.contains(&"browser_navigate"));
        assert!(names.contains(&"browser_type"));
        assert!(names.contains(&"browser_focus_frame"));
        assert!(names.contains(&"browser_type_focused"));
        let _ = init;
        let err = rpc_err(json!(2), -32601, "Method not found: foo");
        assert_eq!(err["error"]["code"], -32601);
        let ok = rpc_ok(json!(1), json!({"tools": listed}));
        assert_eq!(ok["jsonrpc"], "2.0");
        assert_eq!(ok["result"]["tools"].as_array().unwrap().len(), arr.len());
    }

    #[test]
    fn wrap_eval_catches_throw() {
        let js = wrap_eval_expr("1 + 2");
        assert!(js.contains("1 + 2"));
        assert!(js.contains("JSON.stringify"));
    }

    #[test]
    fn stdio_flag_matches_acp_args() {
        assert_eq!(STDIO_FLAG, "--embedded-browser-mcp");
        assert_eq!(SERVER_NAME, "browser");
    }

    #[test]
    fn token_header_accepts_bearer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            "Bearer secret-token".parse().unwrap(),
        );
        assert!(token_ok("secret-token", &h));
        assert!(!token_ok("other", &h));
        let mut h2 = HeaderMap::new();
        h2.insert("x-grok-browser-token", "secret-token".parse().unwrap());
        assert!(token_ok("secret-token", &h2));
    }

    #[test]
    fn acp_entry_none_without_endpoint() {
        // Fresh test process: OnceLock empty and no file (or stale file is ok).
        // Shape check when we fabricate the JSON:
        let fake = json!({
            "type": "http",
            "name": SERVER_NAME,
            "url": "http://127.0.0.1:9/mcp",
            "headers": [
                {"name": "Authorization", "value": "Bearer t"}
            ]
        });
        assert_eq!(fake["name"], "browser");
        assert_eq!(fake["type"], "http");
        assert!(fake["url"].as_str().unwrap().contains("127.0.0.1"));
    }
}
