//! Side-browser download takeover (generic; not ChatCut-only).
//!
//! ## What is generic vs site-specific
//!
//! | Path | Trigger (any site) | Host action |
//! |------|--------------------|-------------|
//! | `blob:` / `data:` | injected JS hooks | FileReader → save dialog → write |
//! | `http(s)` + download-looking URL / hidden iframe | same hooks | reqwest → save dialog → stream |
//! | Authenticated API (e.g. ChatCut) | same hooks | WebView **cookies** on first hop, then public redirect (S3) **without** cookies |
//!
//! Iframe takeover is **download-shaped URLs only** (`blob:` / `data:` / file
//! extension / `/download` / `/export` / `/jobs/`). Generic page iframes
//! (Google recaptcha, ads) must load in-place — never `download.bin`.
//!
//! ChatCut is only special in that `/api/.../download` needs session cookies and
//! 302s to a short-lived S3 URL. Public CDN / static files need no cookies.
//!
//! ```text
//! page click / a[download] / hidden iframe
//!        │
//!        ▼
//!   JS interceptor (injected, any origin)
//!        │
//!   ┌────┴────────────────────┐
//!   │                         │
//! http(s) URL              blob: / data:
//!   │                         │
//!   ▼                         ▼
//! title + sbdl GET       FileReader → base64 in page
//!        │                    │
//!        └──────────┬─────────┘
//!                   ▼
//!            native save dialog
//!                   ▼
//!         Rust write / stream to path
//! ```

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use parking_lot::Mutex;
use tauri::http::{header, Method, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, UriSchemeResponder};

use crate::side_browser_host::{emit_download_payload, SideBrowserDownloadPayload};

const TITLE_BLOB_PREFIX: &str = "__GROK_SBDL__";
const TITLE_URL_PREFIX: &str = "__GROK_DL_URL__";
const MAX_BLOB_BYTES: usize = 1_500 * 1024 * 1024;
const PULL_CHUNK_B64: usize = 512 * 1024;
const MAX_HTTP_DOWNLOAD_BYTES: u64 = 1_500 * 1024 * 1024;

#[cfg_attr(not(test), allow(dead_code))]
static SAVE_SEQ: AtomicU64 = AtomicU64::new(1);
/// Dedupe concurrent title + Image signals for the same blob id / url.
static INFLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

pub fn is_download_signal_title(title: &str) -> bool {
    title.starts_with(TITLE_BLOB_PREFIX) || title.starts_with(TITLE_URL_PREFIX)
}

/// A page-level WebView signal can arrive from a stale interceptor that was
/// installed while the document was transitioning through `about:blank`.
/// Google / reCAPTCHA then gets misclassified as a generic `download.bin`.
/// Re-check the current top-level URL at the Rust boundary before opening any
/// save dialog.
fn current_page_is_inline_document(app: &AppHandle, webview_label: &str) -> bool {
    let Some(webview) = app.get_webview(webview_label) else {
        return false;
    };
    let Ok(url) = webview.url() else {
        return false;
    };
    url_looks_like_inline_document(url.as_str())
}

/// Handle `document.title` signals from the polyfill.
pub fn handle_title_signal(app: &AppHandle, webview_label: &str, title: &str) {
    if current_page_is_inline_document(app, webview_label) {
        tracing::info!(
            target: "side_browser",
            %webview_label,
            page = %app
                .get_webview(webview_label)
                .and_then(|webview| webview.url().ok())
                .map(|url| url.to_string())
                .unwrap_or_default(),
            "download signal ignored for inline browser document"
        );
        return;
    }
    if let Some(rest) = title.strip_prefix(TITLE_URL_PREFIX) {
        // `__GROK_DL_URL__|{fileName}|{url}`  (url may contain `|` rarely — split once)
        let rest = rest.strip_prefix('|').unwrap_or(rest);
        let (name, url) = match rest.split_once('|') {
            Some((n, u)) => (n, u),
            None => return,
        };
        spawn_http_download(app, webview_label, url.to_string(), sanitize_filename(name));
        return;
    }

    let rest = match title.strip_prefix(TITLE_BLOB_PREFIX) {
        Some(r) if r.starts_with('|') => &r[1..],
        _ => return,
    };
    let mut parts = rest.splitn(3, '|');
    let id = match parts.next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return,
    };
    let b64_len: usize = match parts.next().and_then(|s| s.parse().ok()) {
        Some(n) => n,
        None => return,
    };
    let file_name = sanitize_filename(parts.next().unwrap_or("download.bin"));
    spawn_blob_pull(app, webview_label, id, b64_len, file_name);
}

fn try_begin(key: String) -> bool {
    INFLIGHT.lock().insert(key)
}

fn end_inflight(key: &str) {
    INFLIGHT.lock().remove(key);
}

fn spawn_blob_pull(
    app: &AppHandle,
    webview_label: &str,
    id: String,
    b64_len: usize,
    file_name: String,
) {
    if current_page_is_inline_document(app, webview_label) {
        tracing::info!(
            target: "side_browser",
            %webview_label,
            "blob download ignored for inline browser document"
        );
        return;
    }
    if b64_len == 0 || b64_len > MAX_BLOB_BYTES * 4 / 3 + 64 {
        tracing::warn!(
            target: "side_browser",
            %webview_label,
            b64_len,
            "blob pull rejected (size)"
        );
        return;
    }
    let key = format!("blob:{webview_label}:{id}");
    if !try_begin(key.clone()) {
        tracing::debug!(target: "side_browser", %id, "blob pull already in flight");
        return;
    }

    tracing::info!(
        target: "side_browser",
        %webview_label,
        %id,
        %file_name,
        b64_len,
        "blob pull start"
    );

    emit_download_payload(
        app,
        SideBrowserDownloadPayload {
            phase: "requested".into(),
            label: webview_label.to_string(),
            url: format!("blob-download:{file_name}"),
            path: None,
            success: None,
            file_name: Some(file_name.clone()),
        },
    );

    let app = app.clone();
    let label = webview_label.to_string();
    let id_for_thread = id.clone();
    let _ = std::thread::Builder::new()
        .name("sbdl-blob-pull".into())
        .spawn(move || {
            let result = pull_blob_and_save(&app, &label, &id_for_thread, b64_len, &file_name);
            end_inflight(&key);
            match result {
                Ok(path) => finish_ok(&app, &label, &file_name, path),
                Err(e) if e == "cancelled" => {
                    emit_download_payload(
                        &app,
                        SideBrowserDownloadPayload {
                            phase: "cancelled".into(),
                            label: label.clone(),
                            url: format!("blob-download:{file_name}"),
                            path: None,
                            success: Some(false),
                            file_name: Some(file_name),
                        },
                    );
                }
                Err(e) => finish_err(&app, &label, &file_name, &e, Some(&id_for_thread)),
            }
        });
}

fn spawn_http_download(app: &AppHandle, webview_label: &str, url: String, file_name: String) {
    if current_page_is_inline_document(app, webview_label) {
        tracing::info!(
            target: "side_browser",
            %webview_label,
            "http download ignored for inline browser document"
        );
        return;
    }
    let url = url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        tracing::warn!(target: "side_browser", %url, "http download rejected (scheme)");
        return;
    }
    // Signed S3 URLs can be long (query + security token).
    if url.len() > 32_000 {
        tracing::warn!(target: "side_browser", len = url.len(), "http download rejected (url too long)");
        return;
    }
    if url_looks_like_inline_document(&url) {
        tracing::info!(
            target: "side_browser",
            %webview_label,
            url = %url.chars().take(160).collect::<String>(),
            "http download skipped (inline document url)"
        );
        return;
    }

    // Dedupe by render/download path (ignore query noise).
    let dedupe_key = url.split('?').next().unwrap_or(&url).to_string();
    let key = format!("url:{webview_label}:{dedupe_key}");
    if !try_begin(key.clone()) {
        tracing::info!(
            target: "side_browser",
            %webview_label,
            "http download already in progress — wait for current transfer"
        );
        return;
    }

    // Prefer filename embedded in signed S3 query (ChatCut export).
    let mut file_name = file_name;
    if is_generic_download_name(&file_name) {
        if let Some(n) = filename_from_url_query(&url) {
            file_name = n;
        }
    }

    // Collect WebView cookies *before* worker (auth for api.chatcut.io).
    let cookie = cookie_header_for_download(app, webview_label, &url);

    tracing::info!(
        target: "side_browser",
        %webview_label,
        %file_name,
        url = %url.chars().take(120).collect::<String>(),
        has_cookie = cookie.is_some(),
        "http download start (Rust reqwest)"
    );

    emit_download_payload(
        app,
        SideBrowserDownloadPayload {
            phase: "requested".into(),
            label: webview_label.to_string(),
            url: url.clone(),
            path: None,
            success: None,
            file_name: Some(file_name.clone()),
        },
    );

    let app = app.clone();
    let label = webview_label.to_string();
    let _ = std::thread::Builder::new()
        .name("sbdl-http-dl".into())
        .spawn(move || {
            let result =
                http_download_to_downloads(&app, &label, &url, &file_name, cookie.as_deref());
            end_inflight(&key);
            match result {
                Ok(path) => finish_ok(&app, &label, &file_name, path),
                Err(e) if e == "cancelled" || e == "skipped-inline-html" => {
                    if e == "skipped-inline-html" {
                        tracing::info!(
                            target: "side_browser",
                            %label,
                            url = %url.chars().take(160).collect::<String>(),
                            "http download skipped (inline HTML, not a file)"
                        );
                    }
                    emit_download_payload(
                        &app,
                        SideBrowserDownloadPayload {
                            phase: "cancelled".into(),
                            label: label.clone(),
                            url: url.clone(),
                            path: None,
                            success: Some(false),
                            file_name: Some(file_name),
                        },
                    );
                }
                Err(e) => finish_err(&app, &label, &file_name, &e, None),
            }
        });
}

/// Native save dialog (suggested name). `None` = user cancelled.
fn pick_save_path(app: &AppHandle, webview_label: &str, suggested: &str) -> Option<PathBuf> {
    let suggested = sanitize_filename(suggested);
    let mut dlg = rfd::FileDialog::new()
        .set_title("Save file / 保存文件")
        .set_file_name(&suggested);

    // Prefer the hosting window so the sheet appears above the child webview.
    if let Some(wv) = app.get_webview(webview_label) {
        let win = wv.window();
        dlg = dlg.set_parent(&win);
    } else if let Some(win) = app.get_window("main") {
        dlg = dlg.set_parent(&win);
    }

    // Default directory: system Downloads (user can still navigate away).
    let dl = system_downloads_dir();
    if dl.is_dir() {
        dlg = dlg.set_directory(&dl);
    }

    tracing::info!(
        target: "side_browser",
        %webview_label,
        %suggested,
        "save dialog open"
    );
    let chosen = dlg.save_file();
    if chosen.is_none() {
        tracing::info!(target: "side_browser", %webview_label, "save dialog cancelled");
    }
    chosen
}

/// Build Cookie header from the side-browser WebView store (HTTP-only included).
fn cookie_header_for_download(app: &AppHandle, label: &str, url: &str) -> Option<String> {
    let wv = app.get_webview(label)?;
    let mut jar: Vec<(String, String)> = Vec::new();

    let mut try_url = |u: &str| {
        if let Ok(parsed) = url::Url::parse(u) {
            if let Ok(cookies) = wv.cookies_for_url(parsed) {
                for c in cookies {
                    let name = c.name().to_string();
                    let value = c.value().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    if !jar.iter().any(|(n, _)| n == &name) {
                        jar.push((name, value));
                    }
                }
            }
        }
    };

    try_url(url);
    // ChatCut: page on app.* API on api.*
    if let Ok(parsed) = url::Url::parse(url) {
        if let Some(host) = parsed.host_str() {
            if host.contains("chatcut") {
                try_url("https://api.chatcut.io/");
                try_url("https://app.chatcut.io/");
                try_url("https://www.chatcut.io/");
            }
            // Always try origin root
            let origin = format!("{}://{}/", parsed.scheme(), host);
            try_url(&origin);
        }
    }

    if jar.is_empty() {
        // Last resort: all cookies in the store (can be large; filter to chatcut hosts).
        if let Ok(all) = wv.cookies() {
            for c in all {
                let domain = c.domain().unwrap_or("").to_string();
                if domain.contains("chatcut") || url.contains(domain.trim_start_matches('.')) {
                    let name = c.name().to_string();
                    let value = c.value().to_string();
                    if !name.is_empty() && !jar.iter().any(|(n, _)| n == &name) {
                        jar.push((name, value));
                    }
                }
            }
        }
    }

    if jar.is_empty() {
        return None;
    }
    Some(
        jar.into_iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Names WebView2 / the polyfill invent when the server sent no filename.
pub(crate) fn is_generic_download_name(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.is_empty()
        || n == "download"
        || n == "download.bin"
        || n == "untitled"
        || n == "untitled.bin"
        || n == "file"
        || n == "file.bin"
        || n == "out.mp4"
}

/// HTML documents served for navigation (Google sorry / recaptcha iframes),
/// not `Content-Disposition: attachment` files the user asked to save.
pub(crate) fn is_inline_html_content_type(ct: &str) -> bool {
    let t = ct
        .split(';')
        .next()
        .unwrap_or(ct)
        .trim()
        .to_ascii_lowercase();
    t == "text/html" || t == "application/xhtml+xml"
}

pub(crate) fn html_prefix_looks_like_document(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .unwrap_or(0);
    let mut s = bytes.get(start..).unwrap_or(&[]);
    // UTF-8 BOM — Google / some CDNs still emit it; must not fail the sniff.
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s = s.get(3..).unwrap_or(&[]);
        let start2 = s
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .unwrap_or(0);
        s = s.get(start2..).unwrap_or(&[]);
    }
    let t = String::from_utf8_lossy(s).to_ascii_lowercase();
    let t = t.trim_start();
    t.starts_with("<!doctype")
        || t.starts_with("<html")
        || t.starts_with("<head")
        || t.starts_with("<body")
        || t.starts_with("<meta")
        || t.starts_with("<title")
        || t.starts_with("<script")
        || t.starts_with("<div")
        || t.starts_with("<iframe")
        || (t.starts_with("<!--") && (t.contains("<html") || t.contains("<!doctype")))
}

fn host_is_object_store(host: &str) -> bool {
    host.contains("amazonaws.com")
        || host.contains(".s3.")
        || host.contains("cloudfront.net")
        || host.contains("r2.cloudflarestorage.com")
        || host.contains("blob.core.windows.net")
        || host.contains("storage.googleapis.com")
}

fn path_is_api_download(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p.ends_with("/download")
        || p.ends_with("/export")
        || p.ends_with("/render")
        || p.contains("/download/")
        || p.contains("/export/")
        || p.contains("/render/")
        || p.contains("/jobs/")
}

fn path_file_extension(path: &str) -> Option<&str> {
    let last = path.rsplit('/').next().unwrap_or("");
    if last.is_empty() || last.starts_with('.') {
        return None;
    }
    let (_, ext) = last.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

fn is_document_extension(ext: &str) -> bool {
    matches!(ext, "html" | "htm" | "php" | "asp" | "aspx" | "jsp" | "cgi")
}

fn is_challenge_document(host: &str, path: &str) -> bool {
    host.contains("recaptcha")
        || path.contains("/recaptcha")
        || path.contains("/sorry")
        || path.contains("/challenge")
}

fn is_google_web_property(host: &str) -> bool {
    let h = host.trim_end_matches('.');
    if h.contains("drive.google.")
        || h.contains("docs.google.")
        || h.contains("googleapis.com")
        || h.contains("googleusercontent.com")
    {
        return false;
    }
    h == "google.com"
        || h.ends_with(".google.com")
        || h == "gstatic.com"
        || h.ends_with(".gstatic.com")
        || h == "youtube.com"
        || h.ends_with(".youtube.com")
        || h.starts_with("www.google.")
        || h == "google.cn"
        || h.ends_with(".google.cn")
}

/// Page / captcha / iframe document — not a file the user asked to save.
///
/// Query strings must be ignored: Google sorry tokens often contain `.bin` /
/// `.png` as substrings, which used to trip the JS interceptor.
pub(crate) fn url_looks_like_inline_document(url: &str) -> bool {
    let raw = url.trim();
    if raw.is_empty() {
        return false;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("data:") {
        return lower.contains("text/html") || lower.contains("application/xhtml");
    }
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let path = u.path().to_ascii_lowercase();
    if host_is_object_store(&host) {
        return false;
    }
    if path_is_api_download(&path) {
        return false;
    }
    if is_challenge_document(&host, &path) {
        return true;
    }
    if let Some(ext) = path_file_extension(&path) {
        return is_document_extension(ext);
    }
    is_google_web_property(&host) || path == "/" || path.is_empty()
}

/// WebView2 `DownloadStarting` for a document. Returning false cancels the
/// download so we never pop `Save file / download.bin` over Google / captcha.
pub(crate) fn should_ignore_requested_download(url: &str, _suggested: &str) -> bool {
    url_looks_like_inline_document(url)
}

/// After bytes land: drop HTML that WebView2 / the polyfill misclassified.
pub(crate) fn should_ignore_finished_download(url: &str, suggested: &str, path: &Path) -> bool {
    url_looks_like_inline_document(url) || should_ignore_html_download(suggested, path)
}

/// True when a staged "download" is actually an HTML page WebView2 / the
/// polyfill misclassified (generic `download.bin` + HTML sniff).
pub(crate) fn should_ignore_html_download(suggested: &str, path: &Path) -> bool {
    let lower = suggested.trim().to_ascii_lowercase();
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        return false;
    }
    if !is_generic_download_name(suggested)
        && Path::new(suggested)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.len() >= 2)
    {
        return false;
    }
    let mut buf = [0u8; 512];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(n) = std::io::Read::read(&mut f, &mut buf) else {
        return false;
    };
    html_prefix_looks_like_document(&buf[..n])
}

/// Map a MIME type to a file extension when the suggested name is generic.
fn extension_from_content_type(ct: &str) -> Option<&'static str> {
    let ct = ct.split(';').next()?.trim().to_ascii_lowercase();
    Some(match ct.as_str() {
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/json" => "json",
        "application/xml" | "text/xml" => "xml",
        "text/csv" => "csv",
        "text/plain" => "txt",
        "text/html" => "html",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            "docx"
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            "pptx"
        }
        _ => return None,
    })
}

/// If `name` is generic (or has no extension), attach an extension from Content-Type.
fn apply_content_type_extension(name: &str, content_type: &str) -> String {
    let Some(ext) = extension_from_content_type(content_type) else {
        return name.to_string();
    };
    let name = name.trim();
    if is_generic_download_name(name) {
        return format!("download.{ext}");
    }
    let has_ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.len() >= 2 && e.len() <= 5);
    if has_ext {
        name.to_string()
    } else {
        format!("{name}.{ext}")
    }
}

/// Parse `response-content-disposition` / `filename` from a signed URL query.
pub(crate) fn filename_from_url_query(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    for (k, v) in u.query_pairs() {
        let key = k.to_ascii_lowercase();
        if key == "response-content-disposition" || key == "filename" {
            // attachment; filename*=UTF-8''%E6%96%B0...
            if let Some(n) = parse_content_disposition_filename(&v) {
                // ChatCut double-encodes filename* value
                let n = urlencoding_decode(&n).unwrap_or(n);
                let n = urlencoding_decode(&n).unwrap_or(n);
                return Some(sanitize_filename(&n));
            }
            if key == "filename" {
                return Some(sanitize_filename(&v));
            }
        }
    }
    // path segment fallback
    u.path_segments()
        .and_then(|mut s| s.next_back())
        .map(|s| s.to_string())
        .filter(|s| s.contains('.') && s != "download" && s != "out.mp4")
        .map(|s| sanitize_filename(&s))
}

fn finish_ok(app: &AppHandle, label: &str, file_name: &str, path: PathBuf) {
    tracing::info!(
        target: "side_browser",
        %label,
        path = %path.display(),
        "download saved"
    );
    crate::path_scope::grant_path(&path);
    let path_s = path.display().to_string();
    // Shared reveal: macOS open -R / Windows explorer /select / Linux ShowItems.
    // Do not use process_util::command (CREATE_NO_WINDOW breaks explorer select).
    let _ = crate::process_util::reveal_in_file_manager(&path);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_name)
        .to_string();
    emit_download_payload(
        app,
        SideBrowserDownloadPayload {
            phase: "finished".into(),
            label: label.to_string(),
            url: format!("download:{name}"),
            path: Some(path_s),
            success: Some(true),
            file_name: Some(name),
        },
    );
}

fn finish_err(app: &AppHandle, label: &str, file_name: &str, err: &str, blob_id: Option<&str>) {
    tracing::warn!(target: "side_browser", %label, error = %err, "download failed");
    if let Some(id) = blob_id {
        let id_js = serde_json::to_string(id).unwrap_or_else(|_| "\"\"".into());
        let cleanup = format!(
            r#"(function(){{try{{delete window.__grokSbdlData[{id_js}];}}catch(e){{}}}})()"#
        );
        let _ = crate::side_browser_host::eval(app, label.to_string(), cleanup);
    }
    emit_download_payload(
        app,
        SideBrowserDownloadPayload {
            phase: "finished".into(),
            label: label.to_string(),
            url: format!("download:{file_name}"),
            path: None,
            success: Some(false),
            file_name: Some(file_name.to_string()),
        },
    );
}

/// Rust-owned HTTP(S) download — no WebView CORS.
///
/// **Do not** auto-follow redirects while holding session cookies: reqwest would
/// forward `Cookie` to CDN/S3. Instead: cookies only on first-party hosts, then
/// public redirect target without cookies. Save path via native dialog.
fn http_download_to_downloads(
    app: &AppHandle,
    webview_label: &str,
    url: &str,
    file_name: &str,
    cookie_header: Option<&str>,
) -> Result<PathBuf, String> {
    let start_url = url.to_string();
    let mut file_name = file_name.to_string();
    if let Some(n) = filename_from_url_query(&start_url) {
        if is_generic_download_name(&file_name) {
            file_name = n;
        }
    }
    let cookie_header = cookie_header.map(|s| s.to_string());
    let app = app.clone();
    let webview_label = webview_label.to_string();

    tauri::async_runtime::block_on(async move {
        // HTTP/1.1 only: some long presigned S3 URLs misbehave under h2.
        let client = crate::proxy::apply_to_reqwest(reqwest::Client::builder())
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .build()
            .map_err(|e| format!("client: {e}"))?;

        let ua = crate::side_browser_host::side_browser_user_agent();

        let mut current = start_url;
        let mut name = file_name;

        for hop in 0..8 {
            let host_l = url::Url::parse(&current)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
                .unwrap_or_default();
            // Public object stores / CDNs: never forward first-party session cookies.
            let host_is_object_store = host_l.contains("amazonaws.com")
                || host_l.contains(".s3.")
                || host_l.contains("cloudfront.net")
                || host_l.contains("r2.cloudflarestorage.com")
                || host_l.contains("blob.core.windows.net")
                || host_l.contains("storage.googleapis.com");
            // Attach WebView cookies only on first-party / API hosts (any site, not just ChatCut).
            let send_cookie = cookie_header.is_some() && !host_is_object_store;

            let mut req = client.get(&current).header(reqwest::header::USER_AGENT, ua);
            // Minimal headers on signed object URLs (often only `host` is signed).
            if !host_is_object_store {
                req = req.header(reqwest::header::ACCEPT, "*/*");
            }
            if send_cookie {
                if let Some(ref c) = cookie_header {
                    req = req.header(reqwest::header::COOKIE, c);
                }
            }

            tracing::info!(
                target: "side_browser",
                hop,
                url_len = current.len(),
                url = %current.chars().take(160).collect::<String>(),
                with_cookie = send_cookie,
                "http download hop"
            );

            let response = req
                .send()
                .await
                .map_err(|e| format!("request hop{hop}: {e}"))?;
            let status = response.status();
            tracing::info!(
                target: "side_browser",
                hop,
                %status,
                "http download response headers"
            );

            if status.is_redirection() {
                let loc = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| format!("HTTP {status} without Location"))?;
                let next = url::Url::parse(&current)
                    .ok()
                    .and_then(|base| base.join(loc).ok())
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| loc.to_string());
                if let Some(n) = filename_from_url_query(&next) {
                    name = n;
                }
                tracing::info!(
                    target: "side_browser",
                    hop,
                    next_len = next.len(),
                    next = %next.chars().take(160).collect::<String>(),
                    "http download redirect"
                );
                drop(response);
                current = next;
                continue;
            }

            if !status.is_success() {
                return Err(format!("HTTP {status} at hop {hop}"));
            }

            if let Some(cd) = response.headers().get(reqwest::header::CONTENT_DISPOSITION) {
                if let Ok(s) = cd.to_str() {
                    if let Some(n) = parse_content_disposition_filename(s) {
                        name = sanitize_filename(&n);
                    }
                }
            }
            if let Some(n) = filename_from_url_query(&current) {
                if is_generic_download_name(&name) {
                    name = n;
                }
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let cd_attachment = response
                .headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|s| s.to_ascii_lowercase().contains("attachment"));
            if !cd_attachment
                && (is_inline_html_content_type(&content_type)
                    || url_looks_like_inline_document(&current))
            {
                return Err("skipped-inline-html".into());
            }
            if !content_type.is_empty() {
                name = apply_content_type_extension(&name, &content_type);
            }

            let content_len = response.content_length();
            if let Some(len) = content_len {
                if len > MAX_HTTP_DOWNLOAD_BYTES {
                    return Err(format!("too large: {len} bytes"));
                }
            }

            use futures_util::StreamExt;
            use std::io::Write as _;
            let mut stream = response.bytes_stream();
            let first_chunk = match stream.next().await {
                Some(Ok(c)) => c,
                Some(Err(e)) => return Err(format!("stream: {e}")),
                None => return Err("empty body".into()),
            };
            let ct_l = content_type.to_ascii_lowercase();
            let generic_ct = ct_l.is_empty()
                || ct_l.contains("octet-stream")
                || ct_l.contains("application/binary")
                || ct_l == "text/plain";
            if !cd_attachment
                && generic_ct
                && html_prefix_looks_like_document(&first_chunk)
            {
                return Err("skipped-inline-html".into());
            }

            // Ask user where to save (after we know it is actually a file).
            let dest = match pick_save_path(&app, &webview_label, &name) {
                Some(p) => p,
                None => return Err("cancelled".into()),
            };
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            crate::path_scope::grant_path(&dest);

            tracing::info!(
                target: "side_browser",
                hop,
                content_len = ?content_len,
                first_chunk = first_chunk.len(),
                %name,
                dest = %dest.display(),
                "http download reading body (stream → chosen path)"
            );

            let mut tmp_os = dest.as_os_str().to_os_string();
            tmp_os.push(".part");
            let tmp = PathBuf::from(tmp_os);

            {
                let mut file =
                    std::fs::File::create(&tmp).map_err(|e| format!("create part: {e}"))?;
                let mut written: u64 = first_chunk.len() as u64;
                file.write_all(&first_chunk)
                    .map_err(|e| format!("write part: {e}"))?;
                let mut last_log: u64 = 0;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| format!("stream: {e}"))?;
                    file.write_all(&chunk)
                        .map_err(|e| format!("write part: {e}"))?;
                    written += chunk.len() as u64;
                    if written > MAX_HTTP_DOWNLOAD_BYTES {
                        let _ = std::fs::remove_file(&tmp);
                        return Err("too large".into());
                    }
                    if written - last_log >= 2 * 1024 * 1024 {
                        last_log = written;
                        tracing::info!(
                            target: "side_browser",
                            written,
                            content_len = ?content_len,
                            "http download progress"
                        );
                    }
                }
                file.flush().map_err(|e| format!("flush part: {e}"))?;
                if written == 0 {
                    let _ = std::fs::remove_file(&tmp);
                    return Err("empty body".into());
                }
                tracing::info!(
                    target: "side_browser",
                    written,
                    %name,
                    "http download body ok"
                );
            }

            std::fs::rename(&tmp, &dest).or_else(|_| {
                std::fs::copy(&tmp, &dest)
                    .map(|_| {
                        let _ = std::fs::remove_file(&tmp);
                    })
                    .map_err(|e| format!("finalize: {e}"))
            })?;
            return Ok(dest);
        }

        Err("too many redirects".into())
    })
}

fn parse_content_disposition_filename(cd: &str) -> Option<String> {
    // attachment; filename*=UTF-8''%E6%96%B0...  or filename="clip.mp4"
    for part in cd.split(';') {
        let p = part.trim();
        let lower = p.to_ascii_lowercase();
        if lower.starts_with("filename*=") {
            let rest = p.split_once('=')?.1.trim().trim_matches('"');
            // UTF-8''percent-encoded  OR charset'lang'value
            let encoded = rest.split_once("''").map(|(_, e)| e).unwrap_or(rest);
            let mut name = urlencoding_decode(encoded).unwrap_or_else(|| encoded.to_string());
            // ChatCut double-encodes in signed URL query
            if name.contains('%') {
                name = urlencoding_decode(&name).unwrap_or(name);
            }
            if !name.is_empty() {
                return Some(name);
            }
        }
        if lower.starts_with("filename=") && !lower.starts_with("filename*=") {
            let rest = p.split_once('=')?.1.trim().trim_matches('"');
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn pull_blob_and_save(
    app: &AppHandle,
    label: &str,
    id: &str,
    b64_len: usize,
    file_name: &str,
) -> Result<PathBuf, String> {
    let id_js = serde_json::to_string(id).map_err(|e| e.to_string())?;
    let mut b64 = String::with_capacity(b64_len + 8);
    let mut offset = 0usize;
    while offset < b64_len {
        let end = (offset + PULL_CHUNK_B64).min(b64_len);
        let script = format!(
            r#"(function(){{
  try {{
    var d = window.__grokSbdlData && window.__grokSbdlData[{id}];
    if (!d || typeof d !== 'string') return '';
    return d.substring({offset},{end});
  }} catch (e) {{ return ''; }}
}})()"#,
            id = id_js,
            offset = offset,
            end = end,
        );
        let raw = crate::side_browser_host::eval(app, label.to_string(), script)?;
        let chunk = decode_eval_string_result(&raw);
        if chunk.is_empty() && end > offset {
            return Err(format!(
                "empty chunk at {offset}..{end} (raw starts {:?})",
                raw.chars().take(48).collect::<String>()
            ));
        }
        b64.push_str(&chunk);
        offset = end;
    }

    let cleanup = format!(
        r#"(function(){{try{{delete window.__grokSbdlData[{id}];}}catch(e){{}}}})()"#,
        id = id_js
    );
    let _ = crate::side_browser_host::eval(app, label.to_string(), cleanup);

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("base64: {e}"))?;
    if bytes.is_empty() {
        return Err("empty payload".into());
    }
    if bytes.len() > MAX_BLOB_BYTES {
        return Err("too large".into());
    }

    let dest = match pick_save_path(app, label, file_name) {
        Some(p) => p,
        None => return Err("cancelled".into()),
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_all_atomic(&dest, &bytes)?;
    crate::path_scope::grant_path(&dest);
    Ok(dest)
}

fn decode_eval_string_result(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t == "null" || t == "undefined" {
        return String::new();
    }
    if let Ok(s) = serde_json::from_str::<String>(t) {
        return s;
    }
    t.trim_matches('"').to_string()
}

/// Injected interceptor.
///
/// ChatCut `Xx(url, filename)`:
/// - same-origin / blob / data → `<a download>` + click (then revokeObjectURL)
/// - cross-origin → **hidden iframe** (our old a-only hooks missed this)
///
/// Auth: same-origin API download needs cookies → fetch in-page with
/// `credentials:"include"`, then FileReader bridge. Cross-origin falls back to
/// Rust reqwest (signed CDN URLs usually work without cookies).
pub fn blob_download_polyfill(label: &str) -> String {
    let label_js = serde_json::to_string(label).unwrap_or_else(|_| "\"resource-browser\"".into());
    // Use r## so JS like split("#") does not terminate the raw string.
    format!(
        r##"(function () {{
  var LABEL = {label_js};
  if (window.__grokSbdlInstalled) {{
    window.__grokSbdlLabel = LABEL;
    return;
  }}
  // Captcha / sorry frames must load in-place. Hooking appendChild/iframe.src
  // here blanks recaptcha and pops Save file / download.bin.
  try {{
    var _loc = String(location.href || "").toLowerCase();
    var _host = String(location.hostname || "").toLowerCase();
    if (
      _loc.indexOf("/recaptcha") >= 0 ||
      _loc.indexOf("/sorry/") >= 0 ||
      _host.indexOf("recaptcha") >= 0 ||
      (
        _host.indexOf("googleapis.com") < 0 &&
        _host.indexOf("googleusercontent.com") < 0 &&
        (_host === "google.com" || _host === "www.google.com" || _host.indexOf("www.google.") === 0) &&
        _host.indexOf("drive.") < 0 &&
        _host.indexOf("docs.") < 0
      )
    ) {{
      return;
    }}
  }} catch (eSkip) {{}}
  window.__grokSbdlData = {{}};
  window.__grokSbdlBlobMap = new Map();
  window.__grokSbdlSeq = 1;
  window.__grokSbdlRecent = Object.create(null);

  function dedupe(key) {{
    var now = Date.now();
    if (window.__grokSbdlRecent[key] && now - window.__grokSbdlRecent[key] < 2500) return true;
    window.__grokSbdlRecent[key] = now;
    return false;
  }}

  function isProtectedPage() {{
    try {{
      var loc = String(location.href || "").toLowerCase();
      var host = String(location.hostname || "").toLowerCase();
      var path = String(location.pathname || "").toLowerCase();
      return (
        host.indexOf("recaptcha") >= 0 ||
        path.indexOf("/recaptcha") >= 0 ||
        path.indexOf("/sorry") >= 0 ||
        path.indexOf("/challenge") >= 0 ||
        host === "google.com" ||
        host === "www.google.com" ||
        host.indexOf("www.google.") === 0 ||
        loc.indexOf("accounts.google.") >= 0
      );
    }} catch (e) {{
      return false;
    }}
  }}

  try {{
    var origCreate = URL.createObjectURL.bind(URL);
    var origRevoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = function (obj) {{
      var url = origCreate(obj);
      try {{
        if (obj && typeof Blob !== "undefined" && obj instanceof Blob) {{
          window.__grokSbdlBlobMap.set(url, obj);
        }}
      }} catch (e) {{}}
      return url;
    }};
    URL.revokeObjectURL = function (url) {{
      // Keep map entry until after click handlers; delete async.
      var u = url;
      setTimeout(function () {{
        try {{ window.__grokSbdlBlobMap.delete(u); }} catch (e) {{}}
      }}, 3000);
      return origRevoke(url);
    }};
  }} catch (e) {{}}

  function isGenericName(n) {{
    var s = String(n || "").trim().toLowerCase();
    return !s || s === "download" || s === "download.bin" || s === "untitled" || s === "file" || s === "out.mp4";
  }}
  function hrefPathname(href) {{
    try {{
      return new URL(href, location.href).pathname.toLowerCase();
    }} catch (e) {{
      return String(href || "").split("#")[0].split("?")[0].toLowerCase();
    }}
  }}
  function pathLooksFile(href) {{
    var p = hrefPathname(href).replace(/\/+$/, "");
    var last = p.split("/").pop() || "";
    var dot = last.lastIndexOf(".");
    if (dot < 1) return false;
    var ext = last.slice(dot + 1);
    var exts = "mp4|webm|mov|mkv|zip|7z|rar|gz|tar|pdf|png|jpg|jpeg|gif|webp|svg|bmp|ico|bin|wav|mp3|m4a|aac|flac|ogg|xls|xlsx|doc|docx|ppt|pptx|csv|txt|json|wasm";
    return ("|" + exts + "|").indexOf("|" + ext + "|") >= 0;
  }}
  function looksApiDownload(href) {{
    var p = hrefPathname(href);
    if (p.length > 1 && p.charAt(p.length - 1) === "/") p = p.slice(0, -1);
    return (
      p === "/download" || p.slice(-9) === "/download" ||
      p === "/export" || p.slice(-7) === "/export" ||
      p === "/render" || p.slice(-7) === "/render" ||
      p.indexOf("/download/") >= 0 || p.indexOf("/export/") >= 0 ||
      p.indexOf("/render/") >= 0 || p.indexOf("/jobs/") >= 0
    );
  }}
  function isDocumentBlob(blob) {{
    if (!blob || !blob.type) return false;
    var t = String(blob.type).toLowerCase();
    return t.indexOf("text/html") >= 0 || t.indexOf("application/xhtml") >= 0 ||
      t.indexOf("text/javascript") >= 0 || t === "application/javascript" ||
      t.indexOf("text/css") >= 0;
  }}
  function guessName(name, blob, href) {{
    var n = (name && String(name).trim()) || "";
    if (n && !isGenericName(n)) return n;
    if (blob && blob.type) {{
      var t = String(blob.type);
      if (t.indexOf("mp4") >= 0) return "download.mp4";
      if (t.indexOf("webm") >= 0) return "download.webm";
      if (t.indexOf("png") >= 0) return "download.png";
      if (t.indexOf("jpeg") >= 0 || t.indexOf("jpg") >= 0) return "download.jpg";
      if (t.indexOf("gif") >= 0) return "download.gif";
      if (t.indexOf("webp") >= 0) return "download.webp";
      if (t.indexOf("pdf") >= 0) return "download.pdf";
      if (t.indexOf("zip") >= 0) return "download.zip";
      if (t.indexOf("json") >= 0) return "download.json";
      if (t.indexOf("csv") >= 0) return "download.csv";
      if (t.indexOf("html") >= 0) return "download.html";
    }}
    if (href) {{
      try {{
        var raw = String(href);
        var path = raw.split("?")[0].split("#")[0];
        var base = path.split("/").pop() || "";
        if (base && base.indexOf(".") > 0) return decodeURIComponent(base);
        var q = raw.indexOf("?") >= 0 ? raw.split("?")[1] : "";
        var keys = ["filename", "file", "name", "download"];
        var parts = q.split("&");
        for (var i = 0; i < parts.length; i++) {{
          var kv = parts[i].split("=");
          var k = decodeURIComponent((kv[0] || "").replace(/\+/g, " ")).toLowerCase();
          for (var j = 0; j < keys.length; j++) {{
            if (k === keys[j] && kv[1]) {{
              var v = decodeURIComponent((kv[1] || "").replace(/\+/g, " "));
              if (v && !isGenericName(v)) return v;
            }}
          }}
        }}
      }} catch (e) {{}}
    }}
    return n || "download.bin";
  }}

  function pingScheme(pathAndQuery) {{
    var roots = ["sbdl://localhost", "http://sbdl.localhost"];
    for (var i = 0; i < roots.length; i++) {{
      try {{
        var img = new Image();
        img.src = roots[i] + pathAndQuery + "&_t=" + Date.now() + "&i=" + i;
      }} catch (e) {{}}
    }}
  }}

  function signalUrlDownload(name, url) {{
    if (isProtectedPage()) return;
    var safeName = String(name || "download.bin").replace(/\|/g, "_");
    if (dedupe("url:" + url)) return;
    try {{
      var prev = document.title;
      document.title = "__GROK_DL_URL__|" + safeName + "|" + url;
      setTimeout(function () {{ try {{ document.title = prev; }} catch (e) {{}} }}, 150);
    }} catch (e) {{}}
    pingScheme(
      "/url?label=" + encodeURIComponent(LABEL) +
      "&name=" + encodeURIComponent(safeName) +
      "&u=" + encodeURIComponent(url)
    );
  }}

  function signalBlobReady(id, b64Len, name) {{
    if (isProtectedPage()) return;
    var safeName = String(name || "download.bin").replace(/\|/g, "_");
    try {{
      var prev = document.title;
      document.title = "__GROK_SBDL__|" + id + "|" + b64Len + "|" + safeName;
      setTimeout(function () {{ try {{ document.title = prev; }} catch (e) {{}} }}, 150);
    }} catch (e) {{}}
    pingScheme(
      "/ready?label=" + encodeURIComponent(LABEL) +
      "&id=" + encodeURIComponent(id) +
      "&len=" + encodeURIComponent(String(b64Len)) +
      "&name=" + encodeURIComponent(safeName)
    );
  }}

  function deliverBlob(blob, filename) {{
    if (!blob || isProtectedPage()) return;
    var name = filename || "download.bin";
    var dkey = "blob:" + name + ":" + (blob.size || 0);
    if (dedupe(dkey)) return;
    var id = String(window.__grokSbdlSeq++);
    window.__grokSbdlLast = {{ id: id, name: name, size: blob.size, at: Date.now(), phase: "reading" }};
    try {{
      var reader = new FileReader();
      reader.onloadend = function () {{
        try {{
          var result = reader.result;
          if (typeof result !== "string") {{
            window.__grokSbdlLast.phase = "fail";
            return;
          }}
          var comma = result.indexOf(",");
          var b64 = comma >= 0 ? result.slice(comma + 1) : result;
          window.__grokSbdlData[id] = b64;
          window.__grokSbdlLast.phase = "signaled";
          window.__grokSbdlLast.b64Len = b64.length;
          signalBlobReady(id, b64.length, name);
        }} catch (e) {{
          try {{ console.warn("[grok-sbdl] reader", e); }} catch (e2) {{}}
        }}
      }};
      reader.onerror = function () {{
        try {{ window.__grokSbdlLast.phase = "fail"; }} catch (e) {{}}
      }};
      reader.readAsDataURL(blob);
    }} catch (e) {{
      try {{ console.warn("[grok-sbdl] deliver", e); }} catch (e2) {{}}
    }}
  }}

  function dataUrlToBlob(href) {{
    var comma = href.indexOf(",");
    if (comma < 0) return null;
    var meta = href.slice(0, comma);
    var data = href.slice(comma + 1);
    var isB64 = /;base64/i.test(meta);
    var mimeMatch = /^data:([^;,]+)/i.exec(meta);
    var mime = (mimeMatch && mimeMatch[1]) || "application/octet-stream";
    if (isB64) {{
      var raw = atob(data);
      var arr = new Uint8Array(raw.length);
      for (var i = 0; i < raw.length; i++) arr[i] = raw.charCodeAt(i);
      return new Blob([arr], {{ type: mime }});
    }}
    try {{
      return new Blob([decodeURIComponent(data)], {{ type: mime }});
    }} catch (e) {{
      return new Blob([data], {{ type: mime }});
    }}
  }}

  // HTTP(S): do NOT follow redirects in-page (S3 signed URLs fail CORS with Origin null
  // in WKWebView). Prefer redirect:manual → pass Location to Rust, else Rust + cookies.
  function fetchThenDeliver(href, name) {{
    if (dedupe("fetch:" + href + ":" + name)) return;
    window.__grokSbdlLast = {{ name: name, href: href, phase: "fetch", at: Date.now() }};
    var done = false;
    function toRust(u) {{
      if (done) return;
      done = true;
      signalUrlDownload(name, u || href);
    }}
    try {{
      fetch(href, {{
        credentials: "include",
        mode: "cors",
        redirect: "manual",
        cache: "no-store"
      }})
        .then(function (r) {{
          // Same-origin API often 302 → S3. Location is the signed URL (no CORS on Rust).
          if (r.status >= 300 && r.status < 400) {{
            var loc = r.headers.get("Location") || r.headers.get("location");
            if (loc) {{
              try {{ loc = new URL(loc, href).href; }} catch (e) {{}}
              window.__grokSbdlLast.phase = "redirect";
              window.__grokSbdlLast.location = loc;
              toRust(loc);
              return;
            }}
          }}
          // opaqueredirect / status 0
          if (r.type === "opaqueredirect" || r.status === 0) {{
            toRust(href);
            return;
          }}
          if (r.ok) {{
            var ct = String(r.headers.get("Content-Type") || "").toLowerCase();
            var cd = String(r.headers.get("Content-Disposition") || "").toLowerCase();
            var attach = cd.indexOf("attachment") >= 0;
            if (!attach && (ct.indexOf("text/html") >= 0 || ct.indexOf("application/xhtml") >= 0)) {{
              done = true;
              return;
            }}
            return r.blob().then(function (b) {{
              done = true;
              deliverBlob(b, name);
            }});
          }}
          toRust(href);
        }})
        .catch(function (err) {{
          try {{ console.warn("[grok-sbdl] page fetch", err); }} catch (e) {{}}
          toRust(href);
        }});
    }} catch (e) {{
      toRust(href);
    }}
  }}

  function handleHref(href, filename, force) {{
    if (!href || typeof href !== "string" || isProtectedPage()) return false;
    var name = guessName(filename, null, href);

    if (href.indexOf("blob:") === 0) {{
      var blob = window.__grokSbdlBlobMap.get(href);
      name = guessName(filename, blob, href);
      // Iframe / navigation blobs (recaptcha, sorry) are documents — never steal.
      if (!force) {{
        if (!blob || isDocumentBlob(blob)) return false;
      }}
      if (blob) {{
        deliverBlob(blob, name);
        return true;
      }}
      try {{
        fetch(href)
          .then(function (r) {{ return r.blob(); }})
          .then(function (b) {{
            if (!force && isDocumentBlob(b)) return;
            deliverBlob(b, name);
          }})
          .catch(function (err) {{
            try {{ console.warn("[grok-sbdl] blob fetch", err); }} catch (e) {{}}
          }});
        return true;
      }} catch (e) {{
        return false;
      }}
    }}

    if (href.indexOf("data:") === 0) {{
      if (!force) {{
        var dl = href.slice(0, 48).toLowerCase();
        if (dl.indexOf("data:text/html") === 0 || dl.indexOf("data:application/xhtml") === 0) return false;
      }}
      try {{
        var b = dataUrlToBlob(href);
        if (!b) return false;
        if (!force && isDocumentBlob(b)) return false;
        deliverBlob(b, name);
        return true;
      }} catch (e) {{
        return false;
      }}
    }}

    if (href.indexOf("https://") === 0 || href.indexOf("http://") === 0) {{
      // Match the PATH only. Query tokens on Google sorry/recaptcha often
      // contain ".bin" / ".png" / "/render" as substrings.
      if (force || pathLooksFile(href) || looksApiDownload(href)) {{
        fetchThenDeliver(href, name);
        return true;
      }}
    }}
    return false;
  }}

  function tryAnchor(a) {{
    if (!a || !a.tagName || String(a.tagName).toUpperCase() !== "A") return false;
    if (a.getAttribute("data-grok-sbdl") === "1") return true;
    var href = "";
    try {{ href = a.href || a.getAttribute("href") || ""; }} catch (e) {{
      href = a.getAttribute("href") || "";
    }}
    var name = "";
    try {{ name = a.getAttribute("download") || a.download || ""; }} catch (e) {{}}
    var explicitDl =
      a.hasAttribute("download") ||
      (typeof a.download === "string" && a.download !== "");
    var blobish = href.indexOf("blob:") === 0 || href.indexOf("data:") === 0;
    if (!explicitDl && !blobish) return false;
    var ok = handleHref(href, name, explicitDl);
    if (ok) {{
      try {{ a.setAttribute("data-grok-sbdl", "1"); }} catch (e) {{}}
    }}
    return ok;
  }}

  function tryIframeSrc(src) {{
    if (!src || typeof src !== "string") return false;
    if (src === "about:blank" || src.indexOf("javascript:") === 0) return false;
    // ChatCut Xx() uses a hidden iframe to a *download* URL. Do NOT force:
    // Google sorry / recaptcha / ads also inject http(s) iframes; treating
    // those as files pops "Save file / download.bin" and blanks the frame.
    return handleHref(src, "", false);
  }}

  // --- hooks ---
  document.addEventListener(
    "click",
    function (ev) {{
      try {{
        var t = ev.target;
        if (!t || !t.closest) return;
        var a = t.closest("a");
        if (!a) return;
        if (tryAnchor(a)) {{
          ev.preventDefault();
          ev.stopPropagation();
          if (ev.stopImmediatePropagation) ev.stopImmediatePropagation();
        }}
      }} catch (e) {{}}
    }},
    true
  );

  try {{
    var aProto = HTMLAnchorElement.prototype;
    var origAClick = aProto.click;
    aProto.click = function () {{
      try {{
        if (tryAnchor(this)) return;
      }} catch (e) {{}}
      return origAClick.apply(this, arguments);
    }};
  }} catch (e) {{}}

  // Critical: ChatCut does appendChild(a); a.click(); revoke — handle on append
  // BEFORE click/revoke. Also catch iframe download fallback.
  try {{
    var origAppend = Node.prototype.appendChild;
    Node.prototype.appendChild = function (child) {{
      try {{
        if (child && child.tagName) {{
          var tag = String(child.tagName).toUpperCase();
          if (tag === "A") {{
            tryAnchor(child);
          }} else if (tag === "IFRAME") {{
            var s = "";
            try {{ s = child.src || child.getAttribute("src") || ""; }} catch (e) {{}}
            if (s && tryIframeSrc(s)) {{
              try {{ child.removeAttribute("src"); child.src = "about:blank"; }} catch (e2) {{}}
            }}
          }}
        }}
      }} catch (e) {{}}
      return origAppend.apply(this, arguments);
    }};
  }} catch (e) {{}}

  try {{
    var origInsert = Node.prototype.insertBefore;
    Node.prototype.insertBefore = function (child, ref) {{
      try {{
        if (child && child.tagName) {{
          var tag = String(child.tagName).toUpperCase();
          if (tag === "A") tryAnchor(child);
          else if (tag === "IFRAME") {{
            var s = "";
            try {{ s = child.src || child.getAttribute("src") || ""; }} catch (e) {{}}
            if (s && tryIframeSrc(s)) {{
              try {{ child.removeAttribute("src"); child.src = "about:blank"; }} catch (e2) {{}}
            }}
          }}
        }}
      }} catch (e) {{}}
      return origInsert.apply(this, arguments);
    }};
  }} catch (e) {{}}

  // iframe.src = url (set after create)
  try {{
    var iframeProto = HTMLIFrameElement.prototype;
    var srcDesc = Object.getOwnPropertyDescriptor(iframeProto, "src") ||
      Object.getOwnPropertyDescriptor(HTMLElement.prototype, "src");
    if (srcDesc && srcDesc.set) {{
      Object.defineProperty(iframeProto, "src", {{
        configurable: true,
        enumerable: true,
        get: srcDesc.get,
        set: function (v) {{
          try {{
            if (v && tryIframeSrc(String(v))) {{
              return srcDesc.set.call(this, "about:blank");
            }}
          }} catch (e) {{}}
          return srcDesc.set.call(this, v);
        }}
      }});
    }}
  }} catch (e) {{}}

  window.__grokSbdlInstalled = true;
  window.__grokSbdlLabel = LABEL;
  try {{ console.info("[grok-sbdl] ready v5", LABEL); }} catch (e) {{}}
}})();"##,
        label_js = label_js
    )
}

/// Non-blocking polyfill inject.
pub fn install_hook(app: &AppHandle, label: String) -> Result<(), String> {
    let t = label.trim();
    if t.is_empty() {
        return Err("label empty".into());
    }
    let wv = app
        .get_webview(t)
        .ok_or_else(|| format!("side browser webview not found: {t}"))?;
    let script = blob_download_polyfill(t);
    wv.eval(script)
        .map_err(|e| format!("install download polyfill: {e}"))?;
    tracing::debug!(target: "side_browser", label = %t, "download polyfill eval scheduled");
    Ok(())
}

// ── custom protocol: GET notify (Image/beacon) + optional POST body ─────

fn cors_headers(builder: tauri::http::response::Builder) -> tauri::http::response::Builder {
    builder
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "POST, OPTIONS, GET")
        .header(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Content-Type, X-Grok-Filename, X-Grok-Label, Content-Length",
        )
        .header(header::ACCESS_CONTROL_MAX_AGE, "86400")
}

fn json_response(status: StatusCode, body: &str) -> Response<Vec<u8>> {
    cors_headers(
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8"),
    )
    .body(body.as_bytes().to_vec())
    .unwrap_or_else(|_| {
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(b"{}".to_vec())
            .expect("response")
    })
}

fn sanitize_filename(raw: &str) -> String {
    let trimmed = raw.trim();
    let base = if trimmed.is_empty() {
        "download.bin"
    } else {
        trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed)
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
        "download.bin".into()
    } else {
        cleaned.chars().take(200).collect()
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == key {
            return Some(urlencoding_decode(v).unwrap_or_else(|| v.replace('+', " ")));
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let b = u8::from_str_radix(h, 16).ok()?;
                out.push(b);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn system_downloads_dir() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new() {
        if let Some(dl) = dirs.download_dir() {
            return dl.to_path_buf();
        }
    }
    crate::process_util::user_home().join("Downloads")
}

#[cfg_attr(not(test), allow(dead_code))]
fn unique_download_path(dir: &Path, suggested: &str) -> PathBuf {
    let name = sanitize_filename(suggested);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e))
            if !s.is_empty() && e.len() <= 12 && e.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (s.to_string(), format!(".{e}"))
        }
        _ => (name.clone(), String::new()),
    };
    let mut candidate = dir.join(format!("{stem}{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 1..10_000 {
        candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{stem}-{seq}{ext}"))
}

fn write_all_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut f = std::fs::File::create(path).map_err(|e| format!("create: {e}"))?;
    f.write_all(data).map_err(|e| format!("write: {e}"))?;
    f.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

pub fn dispatch_async(
    app: AppHandle,
    webview_label: String,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    if let Err(e) = std::thread::Builder::new()
        .name("sbdl-proto".into())
        .spawn(move || {
            let response = handle_protocol(&app, &webview_label, request);
            responder.respond(response);
        })
    {
        tracing::warn!(error = %e, "sbdl spawn failed");
    }
}

fn handle_protocol(
    app: &AppHandle,
    webview_label: &str,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let method = request.method().clone();
    let uri = request.uri().to_string();
    tracing::info!(
        target: "side_browser",
        %method,
        uri = %uri,
        label = %webview_label,
        "sbdl protocol hit"
    );

    if method == Method::OPTIONS {
        return cors_headers(Response::builder().status(StatusCode::NO_CONTENT))
            .body(Vec::new())
            .unwrap_or_else(|_| json_response(StatusCode::NO_CONTENT, ""));
    }

    if method == Method::GET {
        let path = request.uri().path();
        let label = query_param(&uri, "label").unwrap_or_else(|| webview_label.to_string());

        // Image/beacon notify: blob ready for eval pull
        if path.ends_with("/ready") || path.contains("ready") {
            let id = query_param(&uri, "id").unwrap_or_default();
            let len: usize = query_param(&uri, "len")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let name = query_param(&uri, "name").unwrap_or_else(|| "download.bin".into());
            if !id.is_empty() && len > 0 {
                spawn_blob_pull(app, &label, id, len, sanitize_filename(&name));
            }
            return json_response(StatusCode::OK, r#"{"ok":true,"queued":"blob"}"#);
        }

        // Image/beacon notify: http(s) URL for Rust download
        if path.ends_with("/url") || path.contains("/url") {
            let name = query_param(&uri, "name").unwrap_or_else(|| "download.bin".into());
            let url = query_param(&uri, "u").unwrap_or_default();
            if !url.is_empty() {
                spawn_http_download(app, &label, url, sanitize_filename(&name));
            }
            return json_response(StatusCode::OK, r#"{"ok":true,"queued":"url"}"#);
        }

        return json_response(StatusCode::OK, r#"{"ok":true,"pong":true}"#);
    }

    if method != Method::POST {
        return json_response(
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"ok":false,"error":"method"}"#,
        );
    }

    // POST body path (if a page can reach it)
    let body = request.body();
    if body.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, r#"{"ok":false,"error":"empty"}"#);
    }
    if body.len() > MAX_BLOB_BYTES {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            r#"{"ok":false,"error":"too large"}"#,
        );
    }
    let label = query_param(&uri, "label").unwrap_or_else(|| webview_label.to_string());
    let name = query_param(&uri, "name")
        .map(|s| sanitize_filename(&s))
        .unwrap_or_else(|| "download.bin".into());
    let Some(dest) = pick_save_path(app, &label, &name) else {
        return json_response(StatusCode::OK, r#"{"ok":false,"cancelled":true}"#);
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = write_all_atomic(&dest, body) {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!(r#"{{"ok":false,"error":"{e}"}}"#),
        );
    }
    finish_ok(app, &label, &name, dest.clone());
    let body_json = serde_json::json!({
        "ok": true,
        "path": dest.display().to_string(),
    });
    json_response(StatusCode::OK, &body_json.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_titles() {
        assert!(is_download_signal_title("__GROK_SBDL__|1|10|a.mp4"));
        assert!(is_download_signal_title(
            "__GROK_DL_URL__|a.mp4|https://x/y"
        ));
        assert!(!is_download_signal_title("ChatCut"));
    }

    #[test]
    fn content_disposition() {
        assert_eq!(
            parse_content_disposition_filename("attachment; filename=\"clip.mp4\"").as_deref(),
            Some("clip.mp4")
        );
        let s3 = "https://bucket.s3.amazonaws.com/out.mp4?response-content-disposition=attachment%3B%20filename%2A%3DUTF-8%27%27hello%2520world.mp4&x-id=GetObject";
        let n = filename_from_url_query(s3).expect("name");
        assert!(n.contains("hello"), "{n}");
        assert!(n.ends_with(".mp4"), "{n}");
    }

    #[test]
    fn generic_download_names() {
        assert!(is_generic_download_name("download.bin"));
        assert!(is_generic_download_name("DOWNLOAD"));
        assert!(is_generic_download_name(" untitled.bin "));
        assert!(!is_generic_download_name("report.pdf"));
    }

    #[test]
    fn content_type_fills_generic_name() {
        assert_eq!(
            apply_content_type_extension("download.bin", "application/pdf"),
            "download.pdf"
        );
        assert_eq!(
            apply_content_type_extension("clip", "video/mp4"),
            "clip.mp4"
        );
        assert_eq!(
            apply_content_type_extension("report.pdf", "image/png"),
            "report.pdf"
        );
    }

    #[test]
    fn polyfill_has_both_bridges() {
        let s = blob_download_polyfill("resource-browser-x");
        assert!(s.contains("__GROK_SBDL__"));
        assert!(s.contains("__GROK_DL_URL__"));
        assert!(s.contains("createObjectURL"));
        assert!(s.contains("/ready?"));
        assert!(s.contains("/url?"));
        assert!(s.contains("__grokSbdlInstalled"));
        assert!(s.contains("IFRAME"));
        assert!(s.contains("credentials"));
        assert!(s.contains("appendChild"));
        assert!(s.contains("isGenericName"));
        assert!(s.contains("filename"));
        // Iframes must not force-download every http(s) src (Google recaptcha).
        assert!(s.contains(r#"handleHref(src, "", false)"#));
        assert!(!s.contains(r#"handleHref(src, "", true)"#));
        assert!(s.contains("text/html"));
        assert!(s.contains("skipped") || s.contains("application/xhtml"));
        assert!(s.contains("hrefPathname"));
        assert!(s.contains("pathLooksFile"));
        // Full-URL substring `.bin` / `.png` must not classify Google tokens.
        assert!(!s.contains("low.indexOf(\".bin\")"));
        assert!(!s.contains("low.indexOf(\".png\")"));
        assert!(s.contains("/sorry/"));
        assert!(s.contains("ready v5"));
    }

    #[test]
    fn inline_html_is_not_a_file_download() {
        assert!(is_inline_html_content_type("text/html; charset=utf-8"));
        assert!(is_inline_html_content_type("application/xhtml+xml"));
        assert!(!is_inline_html_content_type("application/pdf"));
        assert!(!is_inline_html_content_type("video/mp4"));
        assert!(html_prefix_looks_like_document(
            b"\n\t<!DOCTYPE html><html><head>"
        ));
        assert!(html_prefix_looks_like_document(b"<html lang=en>"));
        assert!(html_prefix_looks_like_document(
            b"\xEF\xBB\xBF<!DOCTYPE html><html>"
        ));
        assert!(html_prefix_looks_like_document(
            b"<!-- foo -->\n<html lang=zh>"
        ));
        assert!(!html_prefix_looks_like_document(b"\x00\x00ftypisom"));
        let dir = std::env::temp_dir().join(format!("sbdl-html-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let html = dir.join("download.bin");
        std::fs::write(&html, b"<!DOCTYPE html><html><body>sorry</body></html>").unwrap();
        assert!(should_ignore_html_download("download.bin", &html));
        let named = dir.join("page.html");
        std::fs::write(&named, b"<!DOCTYPE html><html></html>").unwrap();
        assert!(!should_ignore_html_download("page.html", &named));
        let mp4 = dir.join("clip.mp4");
        std::fs::write(&mp4, b"\x00\x00\x00\x18ftypmp42").unwrap();
        assert!(!should_ignore_html_download("clip.mp4", &mp4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn google_sorry_and_recaptcha_are_documents_not_files() {
        assert!(url_looks_like_inline_document("https://www.google.com/"));
        assert!(url_looks_like_inline_document(
            "https://www.google.com/sorry/index?continue=https://www.google.com/&q=EgQj.bin.png"
        ));
        assert!(url_looks_like_inline_document(
            "https://www.google.com/recaptcha/api2/anchor?k=abc&co=aHR0cHM6Ly93d3cuZ29vZ2xlLmNvbTo0NDM."
        ));
        assert!(url_looks_like_inline_document(
            "https://www.gstatic.com/recaptcha/releases/x/recaptcha__zh_cn.js"
        ));
        assert!(should_ignore_requested_download(
            "https://www.google.com/",
            "download.bin"
        ));
        assert!(should_ignore_requested_download(
            "https://www.google.com/sorry/index?q=xx.bin",
            "download.bin"
        ));
        assert!(!url_looks_like_inline_document(
            "https://api.chatcut.io/api/jobs/abc/download"
        ));
        assert!(!url_looks_like_inline_document(
            "https://cdn.example.com/files/clip.mp4"
        ));
        assert!(!url_looks_like_inline_document(
            "https://www.googleapis.com/drive/v3/files/abc?alt=media"
        ));
        assert!(!should_ignore_requested_download(
            "https://cdn.example.com/files/invoice.pdf",
            "invoice.pdf"
        ));
        assert!(!should_ignore_requested_download(
            "https://bucket.s3.amazonaws.com/out?filename=clip.mp4",
            "download.bin"
        ));
    }

    #[test]
    fn sanitize_and_unique() {
        assert_eq!(sanitize_filename("a/b.mp4"), "b.mp4");
        let dir = std::env::temp_dir().join(format!("sbdl-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_download_path(&dir, "c.mp4");
        std::fs::write(&a, b"1").unwrap();
        let b = unique_download_path(&dir, "c.mp4");
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
