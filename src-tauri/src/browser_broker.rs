//! Persistent Chromium CDP target for `chrome-devtools-mcp`.
//!
//! Dedicated Chrome (`--remote-debugging-port` + isolated user-data-dir) does
//! not show Chrome 144+ "Allow remote debugging". Daily Chrome/Edge/Brave still
//! need that toggle; the panel surfaces it honestly.
//!
//! The MCP process is still stdio-per-session (ACP), but it reconnects to the
//! same debug port the Host keeps alive.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::extensions;
use crate::paths;
use crate::store;

pub const SERVER_NAME: &str = "chrome-devtools";
pub const DEDICATED_ID: &str = "dedicated";
pub const EMBEDDED_ID: &str = "embedded";
pub const DEDICATED_PORT: u16 = 9333;
pub const CONNECT_SCRIPT_FILE: &str = "chrome-devtools-mcp-connect.js";
pub const ENDPOINT_FILE: &str = "chrome-devtools-endpoint.json";
pub const STATE_FILE: &str = "browser-broker.json";

pub const CONNECT_SCRIPT: &str = include_str!("../../scripts/chrome-devtools-mcp-connect.js");

const VERSION_TIMEOUT: Duration = Duration::from_millis(800);
const LAUNCH_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserKind {
    Embedded,
    Dedicated,
    Chrome,
    Edge,
    Brave,
    Chromium,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserDebugStatus {
    Ready,
    Running,
    NeedsAllow,
    Stopped,
    Launching,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTarget {
    pub id: String,
    pub kind: BrowserKind,
    pub name: String,
    pub debug_status: BrowserDebugStatus,
    pub port: Option<u16>,
    pub detail: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBrokerSnapshot {
    pub connected: bool,
    pub selected_id: Option<String>,
    pub browser_url: Option<String>,
    pub keep_alive: bool,
    pub error: Option<String>,
    pub targets: Vec<BrowserTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    selected_id: Option<String>,
    dedicated_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndpointFile {
    browser_url: Option<String>,
    ws_endpoint: Option<String>,
    selected_id: Option<String>,
}

#[derive(Debug, Clone)]
struct VersionInfo {
    browser: String,
    ws_url: Option<String>,
}

struct Broker {
    selected_id: Option<String>,
    dedicated_port: u16,
    launching: bool,
    last_error: Option<String>,
    last_url: Option<String>,
}

impl Default for Broker {
    fn default() -> Self {
        let persisted = load_persisted();
        Self {
            selected_id: persisted.selected_id,
            dedicated_port: persisted.dedicated_port.unwrap_or(DEDICATED_PORT),
            launching: false,
            last_error: None,
            last_url: None,
        }
    }
}

fn inner() -> &'static Mutex<Broker> {
    static INNER: OnceLock<Mutex<Broker>> = OnceLock::new();
    INNER.get_or_init(|| Mutex::new(Broker::default()))
}

fn state_path() -> PathBuf {
    paths::app_data_root().join(STATE_FILE)
}

fn dedicated_profile_dir() -> PathBuf {
    let dir = paths::app_data_root().join("chrome-mcp-profile");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn load_persisted() -> PersistedState {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_persisted(selected_id: Option<&str>, dedicated_port: u16) {
    let _ = paths::ensure_app_dirs();
    let body = json!({
        "selectedId": selected_id,
        "dedicatedPort": dedicated_port,
    });
    let _ = fs::write(state_path(), serde_json::to_vec_pretty(&body).unwrap_or_default());
}

fn parse_devtools_active_port(raw: &str) -> Option<(u16, Option<String>)> {
    let mut lines = raw.lines().map(|l| l.trim()).filter(|l| !l.is_empty());
    let port: u16 = lines.next()?.parse().ok()?;
    let ws = lines.next().map(|p| {
        if p.starts_with('/') {
            format!("ws://127.0.0.1:{port}{p}")
        } else {
            format!("ws://127.0.0.1:{port}/{p}")
        }
    });
    Some((port, ws))
}

fn parse_json_version(body: &str) -> Option<VersionInfo> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let browser = v
        .get("Browser")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let ws = v
        .get("webSocketDebuggerUrl")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    if browser.is_empty() && ws.is_none() {
        return None;
    }
    Some(VersionInfo { browser, ws_url: ws })
}

fn kind_from_browser_label(label: &str) -> BrowserKind {
    let l = label.to_ascii_lowercase();
    if l.contains("edg/") || l.contains("microsoft edge") {
        BrowserKind::Edge
    } else if l.contains("brave") {
        BrowserKind::Brave
    } else if l.contains("chrom") {
        BrowserKind::Chrome
    } else {
        BrowserKind::Other
    }
}

fn browser_url_for_port(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn probe_version(port: u16) -> Option<VersionInfo> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let client = reqwest::blocking::Client::builder()
        .timeout(VERSION_TIMEOUT)
        .no_proxy()
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().ok()?;
    parse_json_version(&text)
}

fn read_port_file(path: &Path) -> Option<(u16, Option<String>)> {
    let raw = fs::read_to_string(path).ok()?;
    parse_devtools_active_port(&raw)
}

fn profile_lock_present(user_data: &Path) -> bool {
    user_data.join("lockfile").exists()
        || user_data.join("SingletonLock").exists()
        || user_data.join("DevToolsActivePort").exists()
}

struct BrowserHome {
    kind: BrowserKind,
    name: &'static str,
    user_data: PathBuf,
    exe: Option<PathBuf>,
}

fn known_homes() -> Vec<BrowserHome> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let pf = std::env::var("PROGRAMFILES").unwrap_or_default();
        let pf86 = std::env::var("PROGRAMFILES(X86)").unwrap_or_default();
        out.push(BrowserHome {
            kind: BrowserKind::Chrome,
            name: "Google Chrome",
            user_data: PathBuf::from(&local).join("Google/Chrome/User Data"),
            exe: first_existing(&[
                PathBuf::from(&pf).join("Google/Chrome/Application/chrome.exe"),
                PathBuf::from(&pf86).join("Google/Chrome/Application/chrome.exe"),
                PathBuf::from(&local).join("Google/Chrome/Application/chrome.exe"),
            ]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Edge,
            name: "Microsoft Edge",
            user_data: PathBuf::from(&local).join("Microsoft/Edge/User Data"),
            exe: first_existing(&[
                PathBuf::from(&pf).join("Microsoft/Edge/Application/msedge.exe"),
                PathBuf::from(&pf86).join("Microsoft/Edge/Application/msedge.exe"),
            ]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Brave,
            name: "Brave",
            user_data: PathBuf::from(&local).join("BraveSoftware/Brave-Browser/User Data"),
            exe: first_existing(&[
                PathBuf::from(&pf).join("BraveSoftware/Brave-Browser/Application/brave.exe"),
                PathBuf::from(&local)
                    .join("BraveSoftware/Brave-Browser/Application/brave.exe"),
            ]),
        });
    }
    #[cfg(target_os = "macos")]
    {
        let home = crate::process_util::user_home();
        let support = home.join("Library/Application Support");
        out.push(BrowserHome {
            kind: BrowserKind::Chrome,
            name: "Google Chrome",
            user_data: support.join("Google/Chrome"),
            exe: first_existing(&[PathBuf::from(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            )]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Edge,
            name: "Microsoft Edge",
            user_data: support.join("Microsoft Edge"),
            exe: first_existing(&[PathBuf::from(
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            )]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Brave,
            name: "Brave",
            user_data: support.join("BraveSoftware/Brave-Browser"),
            exe: first_existing(&[PathBuf::from(
                "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            )]),
        });
    }
    #[cfg(target_os = "linux")]
    {
        let home = crate::process_util::user_home();
        let config = home.join(".config");
        out.push(BrowserHome {
            kind: BrowserKind::Chrome,
            name: "Google Chrome",
            user_data: config.join("google-chrome"),
            exe: first_existing(&[
                PathBuf::from("/usr/bin/google-chrome"),
                PathBuf::from("/usr/bin/google-chrome-stable"),
            ]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Chromium,
            name: "Chromium",
            user_data: config.join("chromium"),
            exe: first_existing(&[PathBuf::from("/usr/bin/chromium"), PathBuf::from("/usr/bin/chromium-browser")]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Edge,
            name: "Microsoft Edge",
            user_data: config.join("microsoft-edge"),
            exe: first_existing(&[PathBuf::from("/usr/bin/microsoft-edge")]),
        });
        out.push(BrowserHome {
            kind: BrowserKind::Brave,
            name: "Brave",
            user_data: config.join("BraveSoftware/Brave-Browser"),
            exe: first_existing(&[PathBuf::from("/usr/bin/brave-browser")]),
        });
    }
    out
}

fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_file()).cloned()
}

fn chrome_exe() -> Option<PathBuf> {
    known_homes()
        .into_iter()
        .find(|h| h.kind == BrowserKind::Chrome)
        .and_then(|h| h.exe)
}

fn scan_targets(selected_id: Option<&str>, dedicated_port: u16, launching: bool) -> Vec<BrowserTarget> {
    let mut targets = Vec::new();
    let mut used_ports = std::collections::HashSet::new();

    targets.push(BrowserTarget {
        id: EMBEDDED_ID.into(),
        kind: BrowserKind::Embedded,
        name: "In-app browser".into(),
        debug_status: BrowserDebugStatus::Ready,
        port: None,
        detail: Some("Grok App sidebar WebView".into()),
        selected: selected_id == Some(EMBEDDED_ID),
    });

    let dedicated_info = probe_version(dedicated_port);
    let dedicated_port_file = read_port_file(&dedicated_profile_dir().join("DevToolsActivePort"));
    let dedicated_ready = dedicated_info.is_some() || dedicated_port_file.is_some();
    let dedicated_status = if launching && !dedicated_ready {
        BrowserDebugStatus::Launching
    } else if dedicated_ready {
        BrowserDebugStatus::Ready
    } else {
        BrowserDebugStatus::Stopped
    };
    if dedicated_ready {
        used_ports.insert(dedicated_port);
    }
    targets.push(BrowserTarget {
        id: DEDICATED_ID.into(),
        kind: BrowserKind::Dedicated,
        name: "Grok debug Chrome".into(),
        debug_status: dedicated_status,
        port: dedicated_ready.then_some(dedicated_port),
        detail: Some(format!("port {dedicated_port} · isolated profile")),
        selected: selected_id == Some(DEDICATED_ID),
    });

    for home in known_homes() {
        let port_file = home.user_data.join("DevToolsActivePort");
        let parsed = read_port_file(&port_file);
        let running = profile_lock_present(&home.user_data);
        let (status, port, detail) = if let Some((port, _)) = parsed {
            if used_ports.contains(&port) {
                continue;
            }
            let ver = probe_version(port);
            used_ports.insert(port);
            if ver.is_some() {
                (
                    BrowserDebugStatus::Ready,
                    Some(port),
                    Some(format!("debug port {port}")),
                )
            } else {
                (
                    BrowserDebugStatus::NeedsAllow,
                    Some(port),
                    Some("DevToolsActivePort present — confirm Allow remote debugging".into()),
                )
            }
        } else if running {
            (
                BrowserDebugStatus::NeedsAllow,
                None,
                Some("running · chrome://inspect/#remote-debugging".into()),
            )
        } else {
            (BrowserDebugStatus::Stopped, None, None)
        };
        let id = match home.kind {
            BrowserKind::Chrome => "chrome",
            BrowserKind::Edge => "edge",
            BrowserKind::Brave => "brave",
            BrowserKind::Chromium => "chromium",
            _ => "other",
        };
        targets.push(BrowserTarget {
            id: id.into(),
            kind: home.kind,
            name: home.name.into(),
            debug_status: status,
            port,
            detail,
            selected: selected_id == Some(id),
        });
    }

    for extra in [9222_u16, 9229] {
        if used_ports.contains(&extra) {
            continue;
        }
        if let Some(ver) = probe_version(extra) {
            used_ports.insert(extra);
            let kind = kind_from_browser_label(&ver.browser);
            targets.push(BrowserTarget {
                id: format!("port-{extra}"),
                kind,
                name: if ver.browser.is_empty() {
                    format!("Chromium :{extra}")
                } else {
                    ver.browser.clone()
                },
                debug_status: BrowserDebugStatus::Ready,
                port: Some(extra),
                detail: Some(format!("debug port {extra}")),
                selected: selected_id == Some(format!("port-{extra}").as_str()),
            });
        }
    }

    targets
}

fn write_connect_script(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join("scripts");
    fs::create_dir_all(&dir).map_err(|e| format!("scripts dir: {e}"))?;
    let dest = dir.join(CONNECT_SCRIPT_FILE);
    let current = fs::read_to_string(&dest).unwrap_or_default();
    if current != CONNECT_SCRIPT {
        fs::write(&dest, CONNECT_SCRIPT).map_err(|e| format!("write connect script: {e}"))?;
    }
    Ok(dest)
}

fn write_endpoint_file(
    home: &Path,
    port: u16,
    ws: Option<&str>,
    selected_id: Option<&str>,
) -> Result<PathBuf, String> {
    let dest = home.join(ENDPOINT_FILE);
    let body = EndpointFile {
        browser_url: Some(browser_url_for_port(port)),
        ws_endpoint: ws.map(|s| s.to_string()),
        selected_id: selected_id.map(|s| s.to_string()),
    };
    let raw = serde_json::to_vec_pretty(&body).map_err(|e| e.to_string())?;
    fs::write(&dest, raw).map_err(|e| format!("write endpoint: {e}"))?;
    Ok(dest)
}

fn grok_homes() -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let agent = paths::agent_home_dir();
    homes.push(agent.clone());
    let settings = store::load_settings();
    let resolved = paths::resolve_agent_grok_home(&settings.session_data_mode);
    if resolved != agent {
        homes.push(resolved);
    }
    homes
}

fn ensure_scripts() -> Result<(PathBuf, PathBuf), String> {
    let _ = paths::ensure_app_dirs();
    let mut script = None;
    for home in grok_homes() {
        script = Some(write_connect_script(&home)?);
    }
    let script = script.ok_or_else(|| "no agent home".to_string())?;
    Ok((script, paths::agent_home_dir().join(ENDPOINT_FILE)))
}

fn ensure_mcp_config(script: &Path, endpoint: &Path) -> Result<(), String> {
    let settings = store::load_settings();
    let toml_path = extensions::mcp_agent_config_path(&settings.session_data_mode);
    if let Some(parent) = toml_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = fs::read_to_string(&toml_path).unwrap_or_default();
    let mut env = HashMap::new();
    env.insert(
        "GROK_BROWSER_ENDPOINT_FILE".into(),
        endpoint.to_string_lossy().into_owned(),
    );
    let args = vec![script.to_string_lossy().into_owned()];
    let mut block = extensions::format_mcp_stdio_toml_block(SERVER_NAME, "node", &args, Some(&env));
    if !block.contains("startup_timeout_sec") {
        block = block.replace(
            "enabled = true\n",
            "enabled = true\nstartup_timeout_sec = 90\n",
        );
    }
    let stripped = extensions::remove_mcp_server_from_toml(&existing, SERVER_NAME);
    let base = stripped.trim_end();
    let next = if base.is_empty() {
        block
    } else {
        format!("{base}\n\n{block}")
    };
    if next != existing {
        fs::write(&toml_path, next).map_err(|e| e.to_string())?;
        extensions::invalidate_mcp_cache();
        let _ = extensions::set_mcp_enabled(SERVER_NAME, true);
    }
    Ok(())
}

fn publish_endpoint(port: u16, ws: Option<&str>, selected_id: Option<&str>) -> Result<String, String> {
    let (script, _) = ensure_scripts()?;
    let mut endpoint = PathBuf::new();
    for home in grok_homes() {
        endpoint = write_endpoint_file(&home, port, ws, selected_id)?;
    }
    ensure_mcp_config(&script, &endpoint)?;
    Ok(browser_url_for_port(port))
}

fn wait_for_port(port: u16, budget: Duration) -> Option<VersionInfo> {
    let start = Instant::now();
    loop {
        if let Some(v) = probe_version(port) {
            return Some(v);
        }
        if let Some((p, ws)) = read_port_file(&dedicated_profile_dir().join("DevToolsActivePort")) {
            if p == port {
                return Some(VersionInfo {
                    browser: "Chrome".into(),
                    ws_url: ws,
                });
            }
        }
        if start.elapsed() >= budget {
            return None;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn spawn_dedicated(port: u16) -> Result<(), String> {
    let exe = chrome_exe().ok_or_else(|| "Google Chrome is not installed".to_string())?;
    let dir = dedicated_profile_dir();
    let mut cmd = Command::new(&exe);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-sync")
        .arg("about:blank");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .map_err(|e| format!("launch Chrome: {e}"))?;
    Ok(())
}

fn target_port(id: &str, dedicated_port: u16, targets: &[BrowserTarget]) -> Result<(u16, Option<String>), String> {
    if id == DEDICATED_ID {
        if let Some(v) = probe_version(dedicated_port) {
            return Ok((dedicated_port, v.ws_url));
        }
        if let Some((p, ws)) = read_port_file(&dedicated_profile_dir().join("DevToolsActivePort")) {
            return Ok((p, ws));
        }
        return Err("debug Chrome is not listening yet".into());
    }
    let t = targets
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("unknown browser {id}"))?;
    match t.port {
        Some(p) => {
            let ws = probe_version(p).and_then(|v| v.ws_url);
            Ok((p, ws))
        }
        None => Err("this browser has no debug port — allow remote debugging first".into()),
    }
}

fn snapshot_from(
    selected_id: Option<String>,
    dedicated_port: u16,
    launching: bool,
    last_url: Option<String>,
    last_error: Option<String>,
) -> BrowserBrokerSnapshot {
    let targets = scan_targets(selected_id.as_deref(), dedicated_port, launching);
    let connected = match selected_id.as_deref() {
        Some(EMBEDDED_ID) => true,
        Some(id) => {
            targets
                .iter()
                .find(|t| t.id == id)
                .is_some_and(|t| t.debug_status == BrowserDebugStatus::Ready)
                && last_url.is_some()
        }
        None => false,
    };
    BrowserBrokerSnapshot {
        connected,
        keep_alive: selected_id.as_deref() == Some(DEDICATED_ID) && connected,
        selected_id,
        browser_url: last_url,
        error: last_error,
        targets,
    }
}

pub fn selected_id() -> Option<String> {
    inner()
        .lock()
        .ok()
        .and_then(|b| b.selected_id.clone())
}

/// Inject the in-app `browser` MCP unless the user picked an external Chrome.
pub fn wants_embedded_mcp() -> bool {
    match selected_id().as_deref() {
        None => true,
        Some(id) => id == EMBEDDED_ID,
    }
}

pub fn snapshot() -> BrowserBrokerSnapshot {
    let (selected_id, dedicated_port, launching, last_url, last_error) = {
        let b = inner().lock().unwrap_or_else(|e| e.into_inner());
        (
            b.selected_id.clone(),
            b.dedicated_port,
            b.launching,
            b.last_url.clone(),
            b.last_error.clone(),
        )
    };
    snapshot_from(selected_id, dedicated_port, launching, last_url, last_error)
}

fn connect_embedded() -> Result<BrowserBrokerSnapshot, String> {
    let dedicated_port = {
        let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
        b.launching = false;
        b.last_error = None;
        b.dedicated_port
    };
    let _ = extensions::set_mcp_enabled(SERVER_NAME, false);
    save_persisted(Some(EMBEDDED_ID), dedicated_port);
    {
        let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
        b.selected_id = Some(EMBEDDED_ID.into());
        b.launching = false;
        b.last_url = Some("in-app".into());
        b.last_error = None;
    }
    Ok(snapshot())
}

fn disconnect() -> Result<BrowserBrokerSnapshot, String> {
    let dedicated_port = {
        let b = inner().lock().unwrap_or_else(|e| e.into_inner());
        b.dedicated_port
    };
    let _ = extensions::set_mcp_enabled(SERVER_NAME, false);
    save_persisted(None, dedicated_port);
    {
        let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
        b.selected_id = None;
        b.launching = false;
        b.last_url = None;
        b.last_error = None;
    }
    Ok(snapshot())
}

fn connect_id(id: &str) -> Result<BrowserBrokerSnapshot, String> {
    if id == EMBEDDED_ID {
        return connect_embedded();
    }
    let dedicated_port = {
        let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
        if id == DEDICATED_ID {
            b.launching = true;
        }
        b.last_error = None;
        b.dedicated_port
    };

    if id == DEDICATED_ID {
        let already = probe_version(dedicated_port).is_some()
            || read_port_file(&dedicated_profile_dir().join("DevToolsActivePort")).is_some();
        if !already {
            spawn_dedicated(dedicated_port)?;
            if wait_for_port(dedicated_port, LAUNCH_WAIT).is_none() {
                let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
                b.launching = false;
                b.last_error = Some("debug Chrome did not open a debug port".into());
                return Err("debug Chrome did not open a debug port".into());
            }
        }
    }

    let targets = scan_targets(Some(id), dedicated_port, false);
    let (port, ws) = target_port(id, dedicated_port, &targets)?;
    let url = publish_endpoint(port, ws.as_deref(), Some(id))?;
    save_persisted(Some(id), dedicated_port);

    {
        let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
        b.selected_id = Some(id.to_string());
        b.launching = false;
        b.last_url = Some(url);
        b.last_error = None;
    }
    Ok(snapshot())
}

fn open_inspect_for(id: &str) -> Result<(), String> {
    let homes = known_homes();
    let exe = match id {
        DEDICATED_ID => chrome_exe(),
        "chrome" => homes.iter().find(|h| h.kind == BrowserKind::Chrome).and_then(|h| h.exe.clone()),
        "edge" => homes.iter().find(|h| h.kind == BrowserKind::Edge).and_then(|h| h.exe.clone()),
        "brave" => homes.iter().find(|h| h.kind == BrowserKind::Brave).and_then(|h| h.exe.clone()),
        _ => chrome_exe(),
    }
    .ok_or_else(|| "browser executable not found".to_string())?;
    let mut cmd = Command::new(exe);
    cmd.arg("chrome://inspect/#remote-debugging");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    cmd.spawn().map_err(|e| format!("open inspect: {e}"))?;
    Ok(())
}

/// Restore last dedicated Chrome (if selected) without blocking UI launch.
pub fn boot_restore() {
    let _ = ensure_scripts();
    let selected = load_persisted().selected_id;
    if selected.as_deref() == Some(EMBEDDED_ID) {
        let _ = connect_embedded();
        return;
    }
    if selected.as_deref() == Some(DEDICATED_ID) {
        if let Err(e) = connect_id(DEDICATED_ID) {
            tracing::info!(error = %e, "browser broker: dedicated Chrome not restored yet");
        }
    } else if let Some(id) = selected {
        let dedicated_port = DEDICATED_PORT;
        let targets = scan_targets(Some(&id), dedicated_port, false);
        if let Ok((port, ws)) = target_port(&id, dedicated_port, &targets) {
            if let Ok(url) = publish_endpoint(port, ws.as_deref(), Some(&id)) {
                let mut b = inner().lock().unwrap_or_else(|e| e.into_inner());
                b.selected_id = Some(id);
                b.last_url = Some(url);
            }
        }
    }
}

pub fn start_keepalive() {
    std::thread::Builder::new()
        .name("browser-broker-keepalive".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(8));
            let (want_dedicated, port, selected) = {
                let b = inner().lock().unwrap_or_else(|e| e.into_inner());
                (
                    b.selected_id.as_deref() == Some(DEDICATED_ID),
                    b.dedicated_port,
                    b.selected_id.clone(),
                )
            };
            if !want_dedicated {
                continue;
            }
            if probe_version(port).is_some() {
                let ws = probe_version(port).and_then(|v| v.ws_url);
                let _ = publish_endpoint(port, ws.as_deref(), selected.as_deref());
                continue;
            }
            if let Err(e) = spawn_dedicated(port) {
                tracing::debug!(error = %e, "browser broker keepalive launch skipped");
                continue;
            }
            if wait_for_port(port, Duration::from_secs(12)).is_some() {
                let ws = probe_version(port).and_then(|v| v.ws_url);
                let _ = publish_endpoint(port, ws.as_deref(), Some(DEDICATED_ID));
            }
        })
        .ok();
}

#[tauri::command]
pub async fn browser_broker_snapshot() -> BrowserBrokerSnapshot {
    tauri::async_runtime::spawn_blocking(snapshot)
        .await
        .unwrap_or_else(|_| BrowserBrokerSnapshot {
            connected: false,
            selected_id: None,
            browser_url: None,
            keep_alive: false,
            error: Some("snapshot join failed".into()),
            targets: vec![],
        })
}

#[tauri::command]
pub async fn browser_broker_connect(
    app: tauri::AppHandle,
    id: String,
) -> Result<BrowserBrokerSnapshot, String> {
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err("browser id required".into());
    }
    if id == EMBEDDED_ID {
        let _ = crate::side_browser_mcp::ensure_started(&app).await;
        let snap = tauri::async_runtime::spawn_blocking(connect_embedded)
            .await
            .map_err(|e| e.to_string())??;
        crate::side_browser_mcp::request_open(&app, "about:blank");
        return Ok(snap);
    }
    tauri::async_runtime::spawn_blocking(move || connect_id(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn browser_broker_disconnect() -> Result<BrowserBrokerSnapshot, String> {
    tauri::async_runtime::spawn_blocking(disconnect)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn browser_broker_launch_dedicated() -> Result<BrowserBrokerSnapshot, String> {
    tauri::async_runtime::spawn_blocking(|| connect_id(DEDICATED_ID))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn browser_broker_open_inspect(id: String) -> Result<BrowserBrokerSnapshot, String> {
    let id = id.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        open_inspect_for(&id)?;
        Ok(snapshot())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_port_file_with_ws_path() {
        let raw = "9333\n/devtools/browser/abc\n";
        let (port, ws) = parse_devtools_active_port(raw).unwrap();
        assert_eq!(port, 9333);
        assert_eq!(
            ws.as_deref(),
            Some("ws://127.0.0.1:9333/devtools/browser/abc")
        );
    }

    #[test]
    fn parse_port_file_port_only() {
        let (port, ws) = parse_devtools_active_port("9222\n").unwrap();
        assert_eq!(port, 9222);
        assert!(ws.is_none());
    }

    #[test]
    fn parse_port_file_rejects_garbage() {
        assert!(parse_devtools_active_port("").is_none());
        assert!(parse_devtools_active_port("nope\n/ws\n").is_none());
    }

    #[test]
    fn parse_version_extracts_ws() {
        let body = r#"{"Browser":"Chrome/144.0.0.0","webSocketDebuggerUrl":"ws://127.0.0.1:9333/devtools/browser/x"}"#;
        let v = parse_json_version(body).unwrap();
        assert!(v.browser.contains("Chrome"));
        assert_eq!(
            v.ws_url.as_deref(),
            Some("ws://127.0.0.1:9333/devtools/browser/x")
        );
    }

    #[test]
    fn kind_from_edge_label() {
        assert_eq!(
            kind_from_browser_label("Mozilla/5.0 Edg/144.0"),
            BrowserKind::Edge
        );
        assert_eq!(kind_from_browser_label("Chrome/144.0.0.0"), BrowserKind::Chrome);
    }

    #[test]
    fn browser_url_format() {
        assert_eq!(browser_url_for_port(9333), "http://127.0.0.1:9333");
    }

    #[test]
    fn embedded_id_is_stable() {
        assert_eq!(EMBEDDED_ID, "embedded");
        assert_ne!(EMBEDDED_ID, DEDICATED_ID);
    }
}
