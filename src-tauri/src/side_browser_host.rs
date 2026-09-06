//! Embedded side-browser automation surface.
//!
//! All side browser tabs are **in-app** Tauri child Webviews (WKWebView /
//! WebView2 / webkit2gtk). Automation (navigate / eval / url) targets those
//! labeled webviews so Agent tooling can drive the same surface the user sees.
//!
//! Child webviews **must** be created via [`create`] so downloads get an
//! `on_download` handler. Frontend `new Webview()` cannot attach that hook.
//!
//! ## Download UX
//!
//! WKWebView / WebView2 call `on_download` from a platform callback that must
//! return a destination path quickly. Showing a modal `rfd` save dialog inside
//! `DownloadEvent::Requested` often fails silently on macOS (nested modal in a
//! WebKit download decision) — the handler then returns `false` and the
//! download is cancelled with no UI.
//!
//! Fix: accept immediately into an app cache path, then after
//! `DownloadEvent::Finished` show a parented save dialog and move the file.
//! Pending destinations are tracked because macOS wry always reports
//! `path: None` on finish.
//!
//! True Chromium-in-process (CEF) is **not** available in Tauri/Wry today.
//! When CEF lands, it should register under the same label scheme and reuse
//! these commands so automation clients stay compatible.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::LazyLock;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::webview::{DownloadEvent, PageLoadEvent, WebviewBuilder};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl};
use tauri::{LogicalPosition, LogicalSize, Rect, Url};

pub const LABEL_PREFIX: &str = "resource-browser";
const DOWNLOAD_EVENT: &str = "side-browser://download";
const PAGE_LOAD_EVENT: &str = "side-browser://page-load";

/// Last Browser panel the user (or Agent open) focused. MCP tools default here.
static FOCUSED_LABEL: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// url → staging path chosen in `Requested` (macOS finish omits path).
static PENDING_DOWNLOADS: LazyLock<Mutex<HashMap<String, PendingDownload>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DOWNLOAD_SEQ: AtomicU64 = AtomicU64::new(1);

struct PendingDownload {
    label: String,
    staging: PathBuf,
    suggested: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBrowserInfo {
    pub label: String,
    pub url: Option<String>,
}

/// Payload for `side-browser://download` (UI toast / status line).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBrowserDownloadPayload {
    /// `requested` | `finished` | `cancelled`
    pub phase: String,
    pub label: String,
    pub url: String,
    pub path: Option<String>,
    pub success: Option<bool>,
    pub file_name: Option<String>,
}

/// Payload for `side-browser://page-load` (loading bar / anti-white-screen UX).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideBrowserPageLoadPayload {
    /// `started` | `finished`
    pub phase: String,
    pub label: String,
    pub url: String,
}

/// Emit download status for the EmbeddedBrowser status line (HTTP + blob paths).
pub fn emit_download_payload(app: &AppHandle, payload: SideBrowserDownloadPayload) {
    emit_download(app, payload);
}

fn emit_page_load(app: &AppHandle, phase: &str, label: &str, url: &str) {
    if let Err(e) = app.emit(
        PAGE_LOAD_EVENT,
        SideBrowserPageLoadPayload {
            phase: phase.into(),
            label: label.into(),
            url: url.into(),
        },
    ) {
        tracing::warn!(error = %e, "side-browser page-load emit failed");
    }
}

pub(crate) fn validate_label(label: &str) -> Result<(), String> {
    let t = label.trim();
    if t.is_empty() || t.len() > 96 {
        return Err("invalid webview label".into());
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '/'))
    {
        return Err("invalid webview label chars".into());
    }
    Ok(())
}

pub(crate) fn validate_side_label(label: &str) -> Result<(), String> {
    validate_label(label)?;
    if !label.starts_with(LABEL_PREFIX) {
        return Err(format!("side browser label must start with {LABEL_PREFIX}"));
    }
    Ok(())
}

pub(crate) fn validate_url(url: &str) -> Result<Url, String> {
    let u = url.trim();
    if u.is_empty() {
        return Err("url empty".into());
    }
    let lower = u.to_ascii_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("about:")
        || lower.starts_with("file://")
        || lower.starts_with("data:"))
    {
        return Err("url scheme not allowed".into());
    }
    Url::parse(u).map_err(|e| format!("bad url: {e}"))
}

fn get_side_webview<R: tauri::Runtime>(
    app: &AppHandle<R>,
    label: &str,
) -> Result<tauri::Webview<R>, String> {
    validate_label(label)?;
    app.get_webview(label)
        .ok_or_else(|| format!("side browser webview not found: {label}"))
}

/// UA used by Rust-side HTTP downloads (reqwest has no Client Hints).
/// The embedded WebView itself must NOT spoof a Chrome-only string — WebView2
/// still sends `Sec-CH-UA: Microsoft Edge`, and that mismatch is exactly what
/// trips Google's "unusual traffic" /sorry interstitial.
pub(crate) fn side_browser_user_agent() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"
    }
    #[cfg(target_os = "macos")]
    {
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}

fn side_browser_stealth_script() -> &'static str {
    r#"(function(){
  try {
    Object.defineProperty(navigator, "webdriver", { get: function(){ return false; }, configurable: true });
  } catch (e) {}
})();"#
}

/// Sanitize a suggested download file name for the save dialog.
fn sanitize_file_name(raw: &str) -> String {
    let trimmed = raw.trim();
    let base = if trimmed.is_empty() {
        "download"
    } else {
        trimmed
    };
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.');
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// Prefer WK/WebView2 suggested path name; fall back to URL path / query.
/// WebView2 often fills destination with `download.bin` when the server omitted
/// Content-Disposition — treat that as missing so the URL can win.
fn suggested_download_name(destination: &Path, url: &str) -> String {
    let dest_name = destination
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(sanitize_file_name);

    if let Some(ref name) = dest_name {
        if !crate::side_browser_blob::is_generic_download_name(name) {
            return name.clone();
        }
    }
    if let Some(from_url) = crate::side_browser_blob::filename_from_url_query(url) {
        if !crate::side_browser_blob::is_generic_download_name(&from_url) {
            return from_url;
        }
    }
    dest_name.unwrap_or_else(|| "download".into())
}

/// Staging dir for in-progress side-browser downloads (`{app_data}/cache/side-browser-downloads`).
fn side_browser_download_staging_dir() -> PathBuf {
    let dir = crate::paths::app_data_root()
        .join("cache")
        .join("side-browser-downloads");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Unique absolute staging path; wry requires an absolute destination.
fn staging_download_path(suggested: &str) -> PathBuf {
    let seq = DOWNLOAD_SEQ.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    // Keep original basename for the eventual save dialog; prefix for uniqueness.
    let name = format!("{stamp}-{seq}-{suggested}");
    side_browser_download_staging_dir().join(name)
}

fn emit_download(app: &AppHandle, payload: SideBrowserDownloadPayload) {
    if let Err(e) = app.emit(DOWNLOAD_EVENT, &payload) {
        tracing::warn!(error = %e, "side-browser download emit failed");
    }
}

/// Parent save dialog to the hosting window so it paints above child webviews.
fn save_dialog_for_webview(webview: &tauri::Webview, suggested: &str) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new()
        .set_title("Save file / 保存文件")
        .set_file_name(suggested);
    // Parent when possible — sheet/modal above the side-browser child view.
    let win = webview.window();
    dlg = dlg.set_parent(&win);
    dlg.save_file()
}

/// Move/copy staging file to user path; remove staging on success.
fn finalize_download_to(staging: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::rename(staging, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(staging, dest).map_err(|e| format!("copy download: {e}"))?;
            let _ = std::fs::remove_file(staging);
            Ok(())
        }
    }
}

fn take_pending(url: &str) -> Option<PendingDownload> {
    PENDING_DOWNLOADS.lock().remove(url)
}

/// Strip staging prefix `{millis}-{seq}-` to recover the user-facing name.
fn suggested_from_staging_name(file_name: &str) -> String {
    let s = file_name.trim();
    if s.is_empty() {
        return "download".into();
    }
    // "{stamp}-{seq}-{rest}" — rest may contain dashes.
    let mut parts = s.splitn(3, '-');
    let _stamp = parts.next();
    let _seq = parts.next();
    if let Some(rest) = parts.next() {
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    s.to_string()
}

/// Create (or replace) an in-app side-browser child webview with download UX.
///
/// `window_label` is usually `main` or `session-*` — the window that hosts the
/// overlay bounds. Position/size are logical pixels matching the host DOM rect.
#[allow(clippy::too_many_arguments)]
pub fn create(
    app: &AppHandle,
    label: String,
    url: String,
    window_label: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    validate_side_label(&label)?;
    let parsed = validate_url(&url)?;
    let win_label = window_label.trim();
    if win_label.is_empty() {
        return Err("window_label empty".into());
    }

    let width = width.max(40.0);
    let height = height.max(40.0);

    let window = app
        .get_window(win_label)
        .ok_or_else(|| format!("window not found: {win_label}"))?;

    // A quick React remount (including development StrictMode) can ask for the
    // same label while its native WebView is still alive. Reuse it instead of
    // synchronously destroying and recreating a WebView2 controller. On
    // Windows, close -> add_child overlap can re-enter controller teardown and
    // leave the create command waiting forever.
    if let Some(existing) = app.get_webview(&label) {
        tracing::info!(
            target: "side_browser",
            webview_label = %label,
            window = %win_label,
            url = %url,
            "reusing existing side browser webview"
        );
        existing
            .set_bounds(Rect {
                position: LogicalPosition::new(x, y).into(),
                size: LogicalSize::new(width, height).into(),
            })
            .map_err(|e| format!("reuse side browser bounds: {e}"))?;
        let should_navigate = existing
            .url()
            .map(|current| current != parsed)
            .unwrap_or(true);
        if should_navigate {
            emit_page_load(app, "started", &label, &url);
            existing
                .navigate(parsed)
                .map_err(|e| format!("reuse side browser navigate: {e}"))?;
        }
        tracing::info!(
            target: "side_browser",
            webview_label = %label,
            "existing side browser webview ready"
        );
        return Ok(());
    }

    let webview_label = label.clone();
    // Polyfill blob:/data: `<a download>` (ChatCut etc.) — WKWebView skips on_download.
    // Bridge: FileReader in-page → document.title signal → host eval pull → Downloads.
    let polyfill = crate::side_browser_blob::blob_download_polyfill(&label);
    let polyfill_reload = polyfill.clone();
    let title_label = label.clone();
    let page_load_label = label.clone();
    // Do not steal keyboard focus from the main chat/composer on create —
    // users click the page when they want to type there.
    // First document load starts immediately after create.
    emit_page_load(app, "started", &label, &url);
    let builder = WebviewBuilder::new(label.clone(), WebviewUrl::External(parsed.clone()))
        .accept_first_mouse(true)
        .focused(false)
        // Keep the child on Wry's default WebView2 environment arguments.
        // Supplying a different argument string creates a second environment
        // against the same user-data folder and WebView2 rejects it with
        // HRESULT 0x8007139F (the blank, non-interactive browser symptom).
        .initialization_script(side_browser_stealth_script())
        .initialization_script(polyfill)
        // Drive UI loading bar + re-assert download polyfill after navigations.
        // Polyfill early-returns if already installed — cheap.
        .on_page_load(move |webview, payload| {
            let url = payload.url().to_string();
            let label = {
                let live = webview.label().to_string();
                if live.is_empty() {
                    page_load_label.clone()
                } else {
                    live
                }
            };
            match payload.event() {
                PageLoadEvent::Started => {
                    emit_page_load(webview.app_handle(), "started", &label, &url);
                }
                PageLoadEvent::Finished => {
                    emit_page_load(webview.app_handle(), "finished", &label, &url);
                    let _ = webview.eval(polyfill_reload.clone());
                }
            }
        })
        .on_document_title_changed(move |webview, title| {
            if crate::side_browser_blob::is_download_signal_title(&title) {
                let label = webview.label().to_string();
                // Prefer live label; fall back to create-time label.
                let label = if label.is_empty() {
                    title_label.clone()
                } else {
                    label
                };
                crate::side_browser_blob::handle_title_signal(webview.app_handle(), &label, &title);
            }
        })
        .on_download(move |webview, event| {
            let label = webview.label().to_string();
            match event {
                DownloadEvent::Requested { url, destination } => {
                    let url_s = url.to_string();
                    let suggested = suggested_download_name(destination, &url_s);
                    // Google sorry / recaptcha / html navigations. Accepting them
                    // as files pops "Save file / download.bin" and blanks the frame.
                    if crate::side_browser_blob::should_ignore_requested_download(
                        &url_s, &suggested,
                    ) {
                        tracing::info!(
                            target: "side_browser",
                            %label,
                            url = %url_s,
                            %suggested,
                            "download ignored at request (inline document)"
                        );
                        return false;
                    }
                    // Accept immediately into a unique staging path. Modal save
                    // dialogs inside this callback frequently fail on macOS
                    // (WebKit download decision + nested NSSavePanel), which
                    // cancels the download with no UI.
                    let staging = staging_download_path(&suggested);
                    tracing::info!(
                        target: "side_browser",
                        %label,
                        url = %url_s,
                        %suggested,
                        staging = %staging.display(),
                        "download requested → staging"
                    );

                    PENDING_DOWNLOADS.lock().insert(
                        url_s.clone(),
                        PendingDownload {
                            label: label.clone(),
                            staging: staging.clone(),
                            suggested: suggested.clone(),
                        },
                    );

                    crate::path_scope::grant_path(&staging);
                    *destination = staging;

                    emit_download(
                        webview.app_handle(),
                        SideBrowserDownloadPayload {
                            phase: "requested".into(),
                            label,
                            url: url_s,
                            path: None,
                            success: None,
                            file_name: Some(suggested),
                        },
                    );
                    true
                }
                DownloadEvent::Finished { url, path, success } => {
                    let url_s = url.to_string();
                    let pending = take_pending(&url_s);
                    // Prefer platform path; macOS wry often yields None — use staging.
                    let staging = path
                        .map(|p| p.to_path_buf())
                        .or_else(|| pending.as_ref().map(|p| p.staging.clone()));
                    let suggested = pending
                        .as_ref()
                        .map(|p| p.suggested.clone())
                        .or_else(|| {
                            staging
                                .as_ref()
                                .and_then(|p| p.file_name())
                                .and_then(|n| n.to_str())
                                .map(suggested_from_staging_name)
                        })
                        .unwrap_or_else(|| "download".into());
                    let label = pending.as_ref().map(|p| p.label.clone()).unwrap_or(label);

                    if !success {
                        if let Some(ref s) = staging {
                            let _ = std::fs::remove_file(s);
                        }
                        tracing::info!(
                            target: "side_browser",
                            %label,
                            url = %url_s,
                            "download failed"
                        );
                        emit_download(
                            webview.app_handle(),
                            SideBrowserDownloadPayload {
                                phase: "finished".into(),
                                label,
                                url: url_s,
                                path: None,
                                success: Some(false),
                                file_name: Some(suggested),
                            },
                        );
                        return true;
                    }

                    let Some(staging_path) = staging else {
                        tracing::warn!(
                            target: "side_browser",
                            %label,
                            url = %url_s,
                            "download finished without path"
                        );
                        emit_download(
                            webview.app_handle(),
                            SideBrowserDownloadPayload {
                                phase: "finished".into(),
                                label,
                                url: url_s,
                                path: None,
                                success: Some(false),
                                file_name: Some(suggested),
                            },
                        );
                        return true;
                    };

                    if !staging_path.exists() {
                        tracing::warn!(
                            target: "side_browser",
                            %label,
                            url = %url_s,
                            path = %staging_path.display(),
                            "download staging missing"
                        );
                        emit_download(
                            webview.app_handle(),
                            SideBrowserDownloadPayload {
                                phase: "finished".into(),
                                label,
                                url: url_s,
                                path: None,
                                success: Some(false),
                                file_name: Some(suggested),
                            },
                        );
                        return true;
                    }

                    // Google sorry / recaptcha iframes are HTML documents. WebView2
                    // still fires on_download as download.bin — do not steal focus
                    // with a save dialog.
                    if crate::side_browser_blob::should_ignore_finished_download(
                        &url_s,
                        &suggested,
                        &staging_path,
                    ) {
                        let _ = std::fs::remove_file(&staging_path);
                        tracing::info!(
                            target: "side_browser",
                            %label,
                            url = %url_s,
                            "download ignored (inline HTML, not a file)"
                        );
                        emit_download(
                            webview.app_handle(),
                            SideBrowserDownloadPayload {
                                phase: "cancelled".into(),
                                label,
                                url: url_s,
                                path: None,
                                success: Some(false),
                                file_name: Some(suggested),
                            },
                        );
                        return true;
                    }

                    // Save dialog after bytes land — not inside Requested.
                    let chosen = save_dialog_for_webview(&webview, &suggested);
                    let app = webview.app_handle().clone();
                    match chosen {
                        Some(dest) => match finalize_download_to(&staging_path, &dest) {
                            Ok(()) => {
                                crate::path_scope::grant_path(&dest);
                                let file_name = dest
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .map(|s| s.to_string())
                                    .or(Some(suggested));
                                let path_s = dest.display().to_string();
                                tracing::info!(
                                    target: "side_browser",
                                    %label,
                                    url = %url_s,
                                    path = %path_s,
                                    "download saved"
                                );
                                emit_download(
                                    &app,
                                    SideBrowserDownloadPayload {
                                        phase: "finished".into(),
                                        label,
                                        url: url_s,
                                        path: Some(path_s),
                                        success: Some(true),
                                        file_name,
                                    },
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    target: "side_browser",
                                    error = %e,
                                    "download finalize failed"
                                );
                                let _ = std::fs::remove_file(&staging_path);
                                emit_download(
                                    &app,
                                    SideBrowserDownloadPayload {
                                        phase: "finished".into(),
                                        label,
                                        url: url_s,
                                        path: None,
                                        success: Some(false),
                                        file_name: Some(suggested),
                                    },
                                );
                            }
                        },
                        None => {
                            // User cancelled post-download save — drop staging.
                            let _ = std::fs::remove_file(&staging_path);
                            tracing::info!(
                                target: "side_browser",
                                %label,
                                url = %url_s,
                                "download save cancelled"
                            );
                            emit_download(
                                &app,
                                SideBrowserDownloadPayload {
                                    phase: "cancelled".into(),
                                    label,
                                    url: url_s,
                                    path: None,
                                    success: Some(false),
                                    file_name: Some(suggested),
                                },
                            );
                        }
                    }
                    true
                }
                _ => true,
            }
        });

    tracing::info!(
        target: "side_browser",
        %webview_label,
        window = %win_label,
        url = %url,
        x,
        y,
        width,
        height,
        "creating side browser child webview"
    );

    window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width, height),
        )
        .map_err(|e| format!("side browser create: {e}"))?;

    if let Some(wv) = app.get_webview(&label) {
        let _ = wv.show();
    }

    #[cfg(windows)]
    if let Some(host) = app.get_webview_window(win_label) {
        crate::win_shell::raise_side_browser_webviews(&host);
    }

    // Empty tabs stay on about:blank. Finished often never re-fires for that
    // document, which left the chrome spinner running over a white pane.
    if parsed.scheme() == "about" {
        emit_page_load(app, "finished", &label, &url);
    }

    tracing::info!(
        target: "side_browser",
        %webview_label,
        window = %win_label,
        url = %url,
        "side browser webview created"
    );
    Ok(())
}

/// Close a side-browser webview if present (no error when already gone).
pub fn close(app: &AppHandle, label: String) -> Result<(), String> {
    validate_side_label(&label)?;
    clear_focus_if(&label);
    if let Some(wv) = app.get_webview(&label) {
        wv.close().map_err(|e| format!("side browser close: {e}"))?;
    }
    Ok(())
}

/// Apply a child-webview rectangle in one runtime dispatch. Sending separate
/// position/size commands during a sidebar drag lets WebView2 paint one frame
/// with mixed old/new bounds and, under focus changes, can leave input routed
/// to a stale surface. `set_bounds` keeps the pair together.
pub fn set_bounds(
    app: &AppHandle,
    label: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    validate_side_label(&label)?;
    if !x.is_finite() || !y.is_finite() || !width.is_finite() || !height.is_finite() {
        return Err("side browser bounds must be finite".into());
    }
    let width = width.max(2.0);
    let height = height.max(2.0);
    let wv = get_side_webview(app, &label)?;
    let started = std::time::Instant::now();
    wv.set_bounds(Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(width, height).into(),
    })
    .map_err(|e| format!("side browser bounds: {e}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    if elapsed_ms >= 250 {
        tracing::warn!(
            target: "side_browser",
            webview_label = %label,
            elapsed_ms,
            x,
            y,
            width,
            height,
            "slow side browser bounds update"
        );
    }
    Ok(())
}

/// Record the Browser panel the user is looking at (MCP default target).
pub fn set_focus(label: String) -> Result<(), String> {
    validate_side_label(&label)?;
    *FOCUSED_LABEL.lock() = Some(label);
    Ok(())
}

fn json_number(v: &serde_json::Value, key: &str) -> Result<f64, String> {
    v.get(key)
        .and_then(|n| n.as_f64())
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("frame response missing {key}"))
}

/// Focus and click a top-level iframe using the real WebView input path.
/// The iframe document is never inspected; only its parent-page rectangle is
/// used to reproduce a normal user click for hosted fields.
pub fn focus_frame(
    app: &AppHandle,
    label: String,
    selector: Option<String>,
) -> Result<String, String> {
    validate_side_label(&label)?;
    let script = crate::side_browser_a11y::focus_frame_js(selector.as_deref());
    let raw = decode_eval_result(&eval(app, label.clone(), script)?);
    let payload: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("focus frame response: {e}"))?;
    if payload.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("iframe focus failed")
            .to_string());
    }

    let wv = get_side_webview(app, &label)?;
    wv.window()
        .set_focus()
        .map_err(|e| format!("focus browser window: {e}"))?;
    wv.set_focus()
        .map_err(|e| format!("focus browser webview: {e}"))?;

    #[cfg(windows)]
    {
        let left = json_number(&payload, "left")?;
        let top = json_number(&payload, "top")?;
        let width = json_number(&payload, "width")?;
        let height = json_number(&payload, "height")?;
        let child = wv
            .position()
            .map_err(|e| format!("iframe child position: {e}"))?;
        let origin = wv
            .window()
            .inner_position()
            .map_err(|e| format!("browser window position: {e}"))?;
        let scale = wv
            .window()
            .scale_factor()
            .map_err(|e| format!("browser scale factor: {e}"))?;
        let x = origin.x + child.x + ((left + width / 2.0) * scale).round() as i32;
        let y = origin.y + child.y + ((top + height / 2.0) * scale).round() as i32;
        crate::win_shell::click_screen_point(x, y)?;
    }

    Ok(raw)
}

/// Dispatch real OS keyboard input to the focused embedded WebView. Unlike a
/// synthetic KeyboardEvent, this reaches cross-origin hosted inputs such as
/// Stripe Elements.
pub fn send_key(app: &AppHandle, label: String, key: String) -> Result<String, String> {
    validate_side_label(&label)?;
    let wv = get_side_webview(app, &label)?;
    wv.window()
        .set_focus()
        .map_err(|e| format!("focus browser window: {e}"))?;
    wv.set_focus()
        .map_err(|e| format!("focus browser webview: {e}"))?;
    #[cfg(windows)]
    {
        crate::win_shell::send_key(&key)?;
        return Ok(format!("native key dispatched: {}", key.trim()));
    }
    #[cfg(not(windows))]
    {
        let raw = decode_eval_result(&eval(
            app,
            label,
            crate::side_browser_a11y::press_js(&key),
        )?);
        Ok(raw)
    }
}

pub fn send_text(app: &AppHandle, label: String, text: String) -> Result<String, String> {
    validate_side_label(&label)?;
    let wv = get_side_webview(app, &label)?;
    wv.window()
        .set_focus()
        .map_err(|e| format!("focus browser window: {e}"))?;
    wv.set_focus()
        .map_err(|e| format!("focus browser webview: {e}"))?;
    #[cfg(windows)]
    {
        crate::win_shell::send_unicode_text(&text)?;
        return Ok(format!(
            "native text dispatched: {} UTF-16 units",
            text.encode_utf16().count()
        ));
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err("native keyboard dispatch is only implemented on Windows".into())
    }
}

pub fn focused_label() -> Option<String> {
    FOCUSED_LABEL.lock().clone()
}

fn clear_focus_if(label: &str) {
    let mut g = FOCUSED_LABEL.lock();
    if g.as_deref() == Some(label) {
        *g = None;
    }
}

/// List known side-browser webviews (label prefix `resource-browser`).
pub fn list(app: &AppHandle) -> Result<Vec<SideBrowserInfo>, String> {
    let mut out = Vec::new();
    for w in app.webviews().values() {
        let label = w.label().to_string();
        if !label.starts_with(LABEL_PREFIX) {
            continue;
        }
        let url = w.url().ok().map(|u| u.to_string());
        out.push(SideBrowserInfo { label, url });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(out)
}

pub fn navigate(app: &AppHandle, label: String, url: String) -> Result<(), String> {
    let parsed = validate_url(&url)?;
    let wv = get_side_webview(app, &label)?;
    tracing::info!(
        target: "side_browser",
        webview_label = %label,
        url = %url,
        "navigating side browser"
    );
    // Optimistic start so the UI can paint a progress bar before WK/WebView2
    // fires PageLoadEvent::Started (can lag on slow DNS / first byte).
    emit_page_load(app, "started", &label, &url);
    wv.navigate(parsed.clone())
        .map_err(|e| format!("navigate: {e}"))?;
    if parsed.scheme() == "about" {
        emit_page_load(app, "finished", &label, &url);
    }
    #[cfg(windows)]
    if let Some(host) = app.get_webview_window(wv.window().label()) {
        crate::win_shell::raise_side_browser_webviews(&host);
    }
    Ok(())
}

pub fn reload(app: &AppHandle, label: String) -> Result<(), String> {
    let wv = get_side_webview(app, &label)?;
    let url = wv.url().ok().map(|u| u.to_string()).unwrap_or_default();
    emit_page_load(app, "started", &label, &url);
    wv.reload().map_err(|e| format!("reload: {e}"))
}

pub fn current_url(app: &AppHandle, label: String) -> Result<String, String> {
    let wv = get_side_webview(app, &label)?;
    wv.url()
        .map(|u| u.to_string())
        .map_err(|e| format!("url: {e}"))
}

/// Evaluate JS in the embedded webview; return JSON-serialized result string.
///
/// Script should be an expression or IIFE that returns a value. Exceptions
/// should be caught in-script (Windows WebView2 limitation).
///
/// Callers must not run this on the UI/invoke thread: `eval_with_callback`
/// needs the platform runloop, and `recv_timeout` would otherwise freeze
/// the app (up to 15s) whenever the child document is navigating.
pub fn eval(app: &AppHandle, label: String, script: String) -> Result<String, String> {
    validate_label(&label)?;
    if script.trim().is_empty() {
        return Err("script empty".into());
    }
    if script.len() > 512_000 {
        return Err("script too large".into());
    }
    let wv = get_side_webview(app, &label)?;
    let (tx, rx) = mpsc::channel::<String>();
    wv.eval_with_callback(script, move |result| {
        let _ = tx.send(result);
    })
    .map_err(|e| format!("eval: {e}"))?;
    rx.recv_timeout(Duration::from_secs(15))
        .map_err(|_| "eval timeout".to_string())
}

/// WK/WebView2 `eval` may JSON-encode a returned string. Unwrap that layer.
pub fn decode_eval_result(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t == "null" || t == "undefined" {
        return String::new();
    }
    if let Ok(s) = serde_json::from_str::<String>(t) {
        return s;
    }
    t.trim_matches('"').to_string()
}

/// Convenience: page snapshot for automation (title + href + body text sample).
pub fn snapshot(app: &AppHandle, label: String) -> Result<String, String> {
    let script = r#"(function(){
  try {
    return JSON.stringify({
      title: document.title || '',
      href: location.href || '',
      readyState: document.readyState || '',
      text: (document.body && document.body.innerText || '').slice(0, 8000)
    });
  } catch (e) {
    return JSON.stringify({ error: String(e) });
  }
})()"#;
    eval(app, label, script.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn label_rules() {
        assert!(validate_label("resource-browser-tab1").is_ok());
        assert!(validate_label("../x").is_err());
        assert!(validate_side_label("resource-browser-x").is_ok());
        assert!(validate_side_label("other").is_err());
    }

    #[test]
    fn url_scheme_rules() {
        assert!(validate_url("https://example.com").is_ok());
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn decode_eval_unwraps_json_string() {
        assert_eq!(decode_eval_result("\"{\\\"ok\\\":true}\""), "{\"ok\":true}");
        assert_eq!(decode_eval_result("{\"ok\":true}"), "{\"ok\":true}");
        assert_eq!(decode_eval_result("null"), "");
    }

    #[test]
    fn focus_roundtrip() {
        set_focus("resource-browser-tab1".into()).unwrap();
        assert_eq!(focused_label().as_deref(), Some("resource-browser-tab1"));
        clear_focus_if("resource-browser-tab1");
        assert_eq!(focused_label(), None);
        assert!(set_focus("other".into()).is_err());
    }

    #[test]
    fn sanitize_file_name_strips_path_chars() {
        assert_eq!(sanitize_file_name("a/b\\c:d?.pdf"), "a_b_c_d_.pdf");
        assert_eq!(sanitize_file_name("  "), "download");
        assert_eq!(sanitize_file_name("..."), "download");
    }

    #[test]
    fn suggested_name_from_destination_or_url() {
        let dest = PathBuf::from("/tmp/report.zip");
        assert_eq!(
            suggested_download_name(&dest, "https://example.com/x.bin"),
            "report.zip"
        );
        // No file name component → fall back to URL path segment.
        let empty = PathBuf::new();
        assert_eq!(
            suggested_download_name(&empty, "https://cdn.example.com/files/data.csv"),
            "data.csv"
        );
        // WebView2 generic destination must not hide a real URL filename.
        let generic = PathBuf::from(r"C:\Users\x\Downloads\download.bin");
        assert_eq!(
            suggested_download_name(&generic, "https://cdn.example.com/files/invoice.pdf"),
            "invoice.pdf"
        );
        assert_eq!(
            suggested_download_name(&generic, "https://example.com/dl?filename=notes.txt"),
            "notes.txt"
        );
    }

    #[test]
    fn staging_name_strips_prefix() {
        assert_eq!(
            suggested_from_staging_name("171000-3-report.zip"),
            "report.zip"
        );
        assert_eq!(
            suggested_from_staging_name("1-2-my-file.tar.gz"),
            "my-file.tar.gz"
        );
        assert_eq!(suggested_from_staging_name("plain"), "plain");
    }

    #[test]
    fn sanitize_and_staging_path_are_absolute() {
        let p = staging_download_path("hello world.pdf");
        assert!(p.is_absolute());
        assert!(p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("hello world.pdf")));
    }
}
