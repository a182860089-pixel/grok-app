//! Windows shell integration for the main workbench window.
//!
//! Frameless (`decorations: false`) + tray `set_skip_taskbar` can leave the HWND
//! in a state where Explorer's **Show Desktop** (taskbar far-right / Win+D)
//! does not treat us as a significant top-level app window when we are alone.
//! With other normal windows open, minimize-all still sweeps us up — matching
//! the reported "alone = no effect; multi-window = works" symptom.
//!
//! This module forces shell-friendly styles, AppUserModelID, and taskbar tab
//! registration so the window participates in Show Desktop consistently.
//!
//! It also forwards Alt-Tab / taskbar activation into the child WebView2 HWND.
//! With Tauri `unstable` (multi-webview), the page is a `WRY_WEBVIEW` child and
//! wry does not subclass the parent to `MoveFocus`, so the window can be
//! foreground while keyboard events never reach JS until a click.

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Manager, WebviewWindow};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    mouse_event, GetAsyncKeyState, GetFocus, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    VIRTUAL_KEY, VK_LBUTTON,
};
use windows::Win32::UI::Shell::{
    ExtractIconExW, ITaskbarList, SetCurrentProcessExplicitAppUserModelID, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DrawMenuBar, GetClassNameW, GetPropW, GetWindow, GetWindowLongPtrW,
    GetWindowLongW, IsChild, IsWindow, IsWindowVisible, PostMessageW, RemovePropW, SendMessageW,
    SetClassLongPtrW, SetMenu, SetPropW, SetWindowLongPtrW, SetWindowLongW, SetWindowPos,
    GCLP_HICON, GCLP_HICONSM, GWLP_HWNDPARENT, GWLP_WNDPROC, GWL_EXSTYLE, GWL_STYLE, GW_CHILD,
    GW_HWNDNEXT, GW_OWNER, HICON, HWND_NOTOPMOST, SetCursorPos, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WA_ACTIVE, WA_CLICKACTIVE, WM_ACTIVATE, WM_APP,
    WM_NCDESTROY, WM_SETFOCUS, WM_SETICON, WNDPROC, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
};

/// Call once early in process startup (before or right after creating the main window).
///
/// `id` must be the bundled Tauri `identifier` (release `com.grokapp.desktop`;
/// `pnpm dev` overlay `com.grokapp.desktop.dev`) so Explorer groups this
/// process with the matching shortcuts / toasts, not the other install.
pub fn set_process_app_user_model_id(id: &str) {
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if let Err(e) = SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) {
            tracing::warn!("SetCurrentProcessExplicitAppUserModelID: {e}");
        }
    }
}

/// True while the physical left mouse button is down.
///
/// Used by the pet overlay: `startDragging()` often swallows WebView `pointerup`,
/// so the host must notice the button release itself.
pub fn primary_mouse_button_down() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}

/// Move the real pointer and click a point inside the native child WebView.
/// This is used only after the page has returned a concrete iframe rectangle;
/// it gives cross-origin hosted fields the same user-gesture path as a manual
/// click without reading the iframe contents.
pub fn click_screen_point(x: i32, y: i32) -> Result<(), String> {
    unsafe {
        SetCursorPos(x, y).map_err(|e| format!("SetCursorPos: {e}"))?;
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
    }
    Ok(())
}

fn send_inputs(inputs: &[INPUT]) -> Result<(), String> {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        Ok(())
    } else {
        Err(format!("SendInput sent {sent}/{}", inputs.len()))
    }
}

/// Send text through the OS input queue so Chromium/WebView2 forwards it into
/// the currently focused document, including a cross-origin iframe.
pub fn send_unicode_text(text: &str) -> Result<(), String> {
    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        let down = KEYBDINPUT {
            wVk: VIRTUAL_KEY(0),
            wScan: unit,
            dwFlags: KEYEVENTF_UNICODE,
            time: 0,
            dwExtraInfo: 0,
        };
        let up = KEYBDINPUT {
            dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ..down
        };
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: down },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: up },
        });
    }
    if inputs.is_empty() {
        return Ok(());
    }
    send_inputs(&inputs)
}

fn virtual_key_for_name(key: &str) -> Option<u16> {
    match key.trim().to_ascii_lowercase().as_str() {
        "tab" => Some(0x09),
        "enter" | "return" => Some(0x0D),
        "escape" | "esc" => Some(0x1B),
        "backspace" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "arrowup" | "up" => Some(0x26),
        "arrowdown" | "down" => Some(0x28),
        "arrowleft" | "left" => Some(0x25),
        "arrowright" | "right" => Some(0x27),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pageup" => Some(0x21),
        "pagedown" => Some(0x22),
        "space" => Some(0x20),
        "f1" => Some(0x70),
        "f2" => Some(0x71),
        "f3" => Some(0x72),
        "f4" => Some(0x73),
        "f5" => Some(0x74),
        "f6" => Some(0x75),
        "f7" => Some(0x76),
        "f8" => Some(0x77),
        "f9" => Some(0x78),
        "f10" => Some(0x79),
        "f11" => Some(0x7A),
        "f12" => Some(0x7B),
        _ => None,
    }
}

/// Send one named key (or a single printable character) as a real keyboard
/// input. Supports common Ctrl/Alt/Shift/Meta combinations.
pub fn send_key(key: &str) -> Result<(), String> {
    let raw = key.trim();
    if raw.is_empty() {
        return Ok(());
    }
    if !raw.contains('+') && raw.chars().count() == 1 {
        return send_unicode_text(raw);
    }

    let parts: Vec<&str> = raw.split('+').map(str::trim).collect();
    let mut inputs = Vec::new();
    let mut modifiers = Vec::new();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        let vk = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => 0x11,
            "alt" => 0x12,
            "shift" => 0x10,
            "meta" | "win" | "cmd" => 0x5B,
            _ => continue,
        };
        modifiers.push(vk);
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    ..Default::default()
                },
            },
        });
    }
    let last = parts.last().copied().unwrap_or(raw);
    if let Some(vk) = virtual_key_for_name(last) {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    ..Default::default()
                },
            },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    dwFlags: KEYEVENTF_KEYUP,
                    ..Default::default()
                },
            },
        });
    } else {
        send_unicode_text(last)?;
    }
    for vk in modifiers.into_iter().rev() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    dwFlags: KEYEVENTF_KEYUP,
                    ..Default::default()
                },
            },
        });
    }
    if inputs.is_empty() {
        return Err(format!("unsupported key: {raw}"));
    }
    send_inputs(&inputs)
}

/// Desktop-pet overlay: drop the Win32 menu bar (File / Edit / Window / Help).
///
/// Tauri `app.set_menu` attaches the app-wide menu to every window that did not
/// install its own. `SetMenu(NULL)` + `DrawMenuBar` collapses the extra strip
/// even when `decorations(false)` left the muda bar painted.
pub fn strip_overlay_native_menu(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        let _ = SetMenu(hwnd, None);
        let _ = DrawMenuBar(hwnd);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

/// Ensure the main window is a normal taskbar / Alt-Tab / Show-Desktop participant.
///
/// Safe to call repeatedly (setup, show-from-tray, after skip_taskbar restore).
pub fn ensure_main_window_shell_integration(window: &WebviewWindow) {
    if let Some(icon) = window.app_handle().default_window_icon() {
        if let Err(e) = window.set_icon(icon.clone()) {
            tracing::warn!("win_shell: default_window_icon: {e}");
        }
    }
    let Ok(hwnd) = window.hwnd() else {
        tracing::warn!("win_shell: no hwnd for main window");
        return;
    };
    ensure_hwnd_shell_integration(hwnd, /*register_taskbar*/ true);
    attach_hwnd_webview_keyboard_focus(hwnd);
}

/// Push the exe's first icon onto ICON_BIG / ICON_SMALL before Explorer AddTab.
///
/// Frameless release windows often have ICON_SMALL only. After an NSIS update
/// the icon cache misses and `DeleteTab`+`AddTab` then paints a generic
/// document glyph (#943).
fn apply_exe_window_icons(hwnd: HWND) {
    // Extract once: this path runs on setup, tray restore, and skip_taskbar
    // refresh. ExtractIconExW allocates new HICONs each call.
    static ICONS: std::sync::OnceLock<(isize, isize)> = std::sync::OnceLock::new();
    let (big, small) = *ICONS.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else {
            return (0, 0);
        };
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut big = HICON::default();
        let mut small = HICON::default();
        unsafe {
            let n = ExtractIconExW(
                PCWSTR(wide.as_ptr()),
                0,
                Some(std::ptr::from_mut(&mut big)),
                Some(std::ptr::from_mut(&mut small)),
                1,
            );
            if n == 0 {
                tracing::warn!("win_shell: ExtractIconExW returned 0");
                return (0, 0);
            }
            (
                if big.0.is_null() { 0 } else { big.0 as isize },
                if small.0.is_null() {
                    0
                } else {
                    small.0 as isize
                },
            )
        }
    });
    if big == 0 && small == 0 {
        return;
    }
    unsafe {
        // WM_SETICON wParam: ICON_SMALL=0, ICON_BIG=1.
        if big != 0 {
            let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(1)), Some(LPARAM(big)));
            let _ = SetClassLongPtrW(hwnd, GCLP_HICON, big);
        }
        if small != 0 {
            let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(0)), Some(LPARAM(small)));
            let _ = SetClassLongPtrW(hwnd, GCLP_HICONSM, small);
        }
    }
}

/// Apply or clear "live in tray only" extended styles + taskbar tab.
/// Prefer this over bare `set_skip_taskbar` so TOOLWINDOW/APPWINDOW stay consistent.
pub fn set_main_window_skip_taskbar(window: &WebviewWindow, skip: bool) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    unsafe {
        let mut ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if skip {
            ex |= WS_EX_TOOLWINDOW.0;
            ex &= !WS_EX_APPWINDOW.0;
        } else {
            ex &= !WS_EX_TOOLWINDOW.0;
            ex |= WS_EX_APPWINDOW.0;
        }
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex as i32);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
        taskbar_set_tab(hwnd, !skip);
    }
    if !skip {
        // Full re-assert (minimize box, owner clear, not topmost, refresh tab).
        ensure_hwnd_shell_integration(hwnd, /*register_taskbar*/ true);
        attach_hwnd_webview_keyboard_focus(hwnd);
    }
}

fn ensure_hwnd_shell_integration(hwnd: HWND, register_taskbar: bool) {
    apply_exe_window_icons(hwnd);
    unsafe {
        // Clear accidental owner (GWLP_HWNDPARENT on a top-level window is the owner).
        // Owned windows are often skipped by Show Desktop when alone.
        let owner_ptr = GetWindowLongPtrW(hwnd, GWLP_HWNDPARENT);
        if owner_ptr != 0 {
            let _ = SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0);
            tracing::debug!("win_shell: cleared window owner");
        }
        if let Ok(gw_owner) = GetWindow(hwnd, GW_OWNER) {
            if !gw_owner.0.is_null() {
                let _ = SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0);
            }
        }

        let mut style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let mut ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let mut changed = false;

        if style & WS_MINIMIZEBOX.0 == 0 {
            style |= WS_MINIMIZEBOX.0;
            changed = true;
        }
        // Frameless HWNDs still need MAXIMIZEBOX so IsZoomed / SW_MAXIMIZE work.
        if style & WS_MAXIMIZEBOX.0 == 0 {
            style |= WS_MAXIMIZEBOX.0;
            changed = true;
        }
        // Visible app windows must not be tool windows — TOOLWINDOW alone is excluded
        // from Show Desktop's "significant window" set when it is the only one open.
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            ex &= !WS_EX_TOOLWINDOW.0;
            changed = true;
        }
        if ex & WS_EX_APPWINDOW.0 == 0 {
            ex |= WS_EX_APPWINDOW.0;
            changed = true;
        }
        let was_topmost = ex & WS_EX_TOPMOST.0 != 0;
        if was_topmost {
            ex &= !WS_EX_TOPMOST.0;
            changed = true;
        }

        if changed {
            SetWindowLongW(hwnd, GWL_STYLE, style as i32);
            SetWindowLongW(hwnd, GWL_EXSTYLE, ex as i32);
        }

        // Always poke FRAMECHANGED so Explorer re-reads styles; drop TOPMOST z-order if needed.
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED;
        if was_topmost {
            let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
        } else {
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags | SWP_NOZORDER);
        }

        if register_taskbar {
            // Delete+Add forces Explorer to refresh the button / ToggleDesktop set.
            taskbar_set_tab(hwnd, true);
        }
    }
}

fn taskbar_set_tab(hwnd: HWND, present: bool) {
    // COM calls are unsafe; the closure body is not covered by `com_scope`'s
    // outer `unsafe` block (only the call site of `f()` is).
    let _ = com_scope(|| unsafe {
        let taskbar: ITaskbarList = CoCreateInstance(&TaskbarList, None, CLSCTX_SERVER)?;
        taskbar.HrInit()?;
        if present {
            let _ = taskbar.DeleteTab(hwnd);
            taskbar.AddTab(hwnd)?;
        } else {
            taskbar.DeleteTab(hwnd)?;
        }
        Ok(())
    });
}

fn com_scope<F, T>(f: F) -> windows::core::Result<T>
where
    F: FnOnce() -> windows::core::Result<T>,
{
    unsafe {
        // CoInitializeEx returns HRESULT (not Result). S_OK / S_FALSE both succeed and
        // must be balanced with CoUninitialize (MSDN). RPC_E_CHANGED_MODE → skip uninit.
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let need_uninit = hr.is_ok();
        let result = f();
        if need_uninit {
            CoUninitialize();
        }
        result
    }
}

/// wry child-webview class (Tauri `unstable` / `build_as_child`).
const WRY_WEBVIEW_CLASS: &str = "WRY_WEBVIEW";
/// Stored original WndProc pointer (`SetWindowLongPtr` subclass).
const ORIG_PROC_PROP: PCWSTR = windows::core::w!("GrokWvKbdFocusOrig");
/// Primary WebView chosen before side-browser children are added.
///
/// The main window can contain the workbench WebView plus several native side
/// browser WebViews. Picking the first visible child at every activation is
/// nondeterministic and can move focus into the wrong surface.
const PRIMARY_WEBVIEW_PROP: PCWSTR = windows::core::w!("GrokPrimaryWebview");
/// Deferred focus message. Posting lets Windows finish the activation/focus
/// transition before we inspect the current child focus.
const FOCUS_REASSERT_MESSAGE: u32 = WM_APP + 0x4A1;
static FORWARDING_KEYBOARD_FOCUS: AtomicBool = AtomicBool::new(false);

/// Forward Alt-Tab / taskbar activation into the child WebView2 HWND.
///
/// Safe to call repeatedly (skips if the original WndProc prop is already set).
pub fn attach_webview_keyboard_focus(window: &WebviewWindow) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    attach_hwnd_webview_keyboard_focus(hwnd);
}

fn attach_hwnd_webview_keyboard_focus(hwnd: HWND) {
    unsafe {
        if !GetPropW(hwnd, ORIG_PROC_PROP).0.is_null() {
            return;
        }
        // Capture the workbench WebView before any side-browser child is
        // created. This gives activation recovery a stable target instead of
        // whichever child happens to be first in z-order later.
        remember_primary_webview(hwnd);
        let prev = SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            keyboard_focus_wndproc as *const () as isize,
        );
        if prev == 0 {
            return;
        }
        if SetPropW(
            hwnd,
            ORIG_PROC_PROP,
            Some(HANDLE(prev as *mut std::ffi::c_void)),
        )
        .is_err()
        {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, prev);
        }
    }
}

unsafe extern "system" fn keyboard_focus_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == FOCUS_REASSERT_MESSAGE {
        forward_keyboard_focus_to_webview(hwnd);
    }
    let orig = GetPropW(hwnd, ORIG_PROC_PROP);
    if msg == WM_NCDESTROY {
        let _ = RemovePropW(hwnd, ORIG_PROC_PROP);
        let _ = RemovePropW(hwnd, PRIMARY_WEBVIEW_PROP);
    }
    if should_handle_focus_message(msg, wparam.0 as u32) {
        // Do not call SetFocus from inside WM_ACTIVATE/WM_SETFOCUS. That
        // re-enters the native focus chain while Windows is still dispatching
        // activation and was the source of the observed focus oscillation.
        unsafe {
            let _ = PostMessageW(Some(hwnd), FOCUS_REASSERT_MESSAGE, WPARAM(0), LPARAM(0));
        }
    }
    type WndProcFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
    let prev: WNDPROC = if orig.0.is_null() {
        None
    } else {
        Some(std::mem::transmute::<*mut std::ffi::c_void, WndProcFn>(
            orig.0,
        ))
    };
    CallWindowProcW(prev, hwnd, msg, wparam, lparam)
}

fn forward_keyboard_focus_to_webview(hwnd: HWND) {
    if FORWARDING_KEYBOARD_FOCUS.swap(true, Ordering::SeqCst) {
        return;
    }
    let _guard = ForwardingGuard;
    unsafe {
        let focus = GetFocus();
        if !focus.0.is_null() && IsChild(hwnd, focus).as_bool() {
            return;
        }
        let child = primary_webview_child(hwnd).or_else(|| {
            // A side-browser child may be created after startup. Only use a
            // dynamic fallback while there is exactly one visible WRY child;
            // with multiple children, guessing is worse than leaving focus
            // where Windows put it.
            let visible = visible_wry_webview_children(hwnd);
            (visible.len() == 1).then(|| visible[0])
        });
        if let Some(child) = child {
            let _ = SetFocus(Some(child));
        }
    }
}

struct ForwardingGuard;
impl Drop for ForwardingGuard {
    fn drop(&mut self) {
        FORWARDING_KEYBOARD_FOCUS.store(false, Ordering::SeqCst);
    }
}

fn remember_primary_webview(parent: HWND) -> Option<HWND> {
    // Setup attaches the subclass while the top-level window is still
    // hidden, so `IsWindowVisible` is false for the main WebView at this
    // point. Capture any WRY child here; validity is rechecked before focus.
    let child = all_wry_webview_children(parent).into_iter().next()?;
    unsafe {
        let _ = SetPropW(
            parent,
            PRIMARY_WEBVIEW_PROP,
            Some(HANDLE(child.0 as *mut std::ffi::c_void)),
        );
    }
    Some(child)
}

fn primary_webview_child(parent: HWND) -> Option<HWND> {
    unsafe {
        let raw = GetPropW(parent, PRIMARY_WEBVIEW_PROP);
        if raw.0.is_null() {
            return None;
        }
        let child = HWND(raw.0);
        if IsWindow(Some(child)).as_bool()
            && IsWindowVisible(child).as_bool()
            && IsChild(parent, child).as_bool()
            && hwnd_is_wry_webview(child)
        {
            Some(child)
        } else {
            None
        }
    }
}

fn visible_wry_webview_children(parent: HWND) -> Vec<HWND> {
    all_wry_webview_children(parent)
        .into_iter()
        .filter(|child| unsafe { IsWindowVisible(*child).as_bool() })
        .collect()
}

fn all_wry_webview_children(parent: HWND) -> Vec<HWND> {
    let mut out = Vec::new();
    unsafe {
        let Some(mut child) = hwnd_or_none(GetWindow(parent, GW_CHILD).ok()) else {
            return out;
        };
        loop {
            if hwnd_is_wry_webview(child) {
                out.push(child);
            }
            let Some(next) = hwnd_or_none(GetWindow(child, GW_HWNDNEXT).ok()) else {
                break;
            };
            child = next;
        }
    }
    out
}

fn hwnd_or_none(hwnd: Option<HWND>) -> Option<HWND> {
    hwnd.filter(|h| !h.0.is_null())
}

fn hwnd_is_wry_webview(hwnd: HWND) -> bool {
    is_wry_webview_class(&hwnd_class_name(hwnd))
}

fn hwnd_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// `WM_SETFOCUS`, or `WM_ACTIVATE` that is not minimize / deactivate.
fn should_handle_focus_message(msg: u32, wparam: u32) -> bool {
    if msg == WM_SETFOCUS {
        return true;
    }
    if msg != WM_ACTIVATE {
        return false;
    }
    let state = wparam & 0xffff;
    let minimized = ((wparam >> 16) & 0xffff) != 0;
    !minimized && (state == WA_ACTIVE || state == WA_CLICKACTIVE)
}

fn is_wry_webview_class(name: &str) -> bool {
    name.eq_ignore_ascii_case(WRY_WEBVIEW_CLASS)
}

/// Pure helper for unit tests: Alt-Tab / Show-Desktop significance rules (simplified).
#[cfg(test)]
pub fn is_shell_significant_for_tests(style: u32, ex: u32, has_owner: bool) -> bool {
    let tool = ex & WS_EX_TOOLWINDOW.0 != 0;
    let app = ex & WS_EX_APPWINDOW.0 != 0;
    let minbox = style & WS_MINIMIZEBOX.0 != 0;
    if has_owner && !app {
        return false;
    }
    if tool && !app {
        return false;
    }
    minbox && (app || !tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATEAPP;

    #[test]
    fn toolwindow_without_appwindow_is_not_significant() {
        let style = WS_MINIMIZEBOX.0;
        let ex = WS_EX_TOOLWINDOW.0;
        assert!(!is_shell_significant_for_tests(style, ex, false));
    }

    #[test]
    fn appwindow_with_minimize_is_significant() {
        let style = WS_MINIMIZEBOX.0;
        let ex = WS_EX_APPWINDOW.0;
        assert!(is_shell_significant_for_tests(style, ex, false));
    }

    #[test]
    fn owned_without_appwindow_is_not_significant() {
        let style = WS_MINIMIZEBOX.0;
        let ex = 0;
        assert!(!is_shell_significant_for_tests(style, ex, true));
    }

    #[test]
    fn owned_with_appwindow_is_significant() {
        let style = WS_MINIMIZEBOX.0;
        let ex = WS_EX_APPWINDOW.0;
        assert!(is_shell_significant_for_tests(style, ex, true));
    }

    #[test]
    fn wry_webview_class_matches_child_container() {
        assert!(is_wry_webview_class("WRY_WEBVIEW"));
        assert!(is_wry_webview_class("wry_webview"));
        assert!(!is_wry_webview_class("Chrome_WidgetWin_1"));
        assert!(!is_wry_webview_class(""));
    }

    #[test]
    fn alt_tab_activate_and_setfocus_forward_to_webview() {
        assert!(should_handle_focus_message(WM_SETFOCUS, 0));
        assert!(should_handle_focus_message(WM_ACTIVATE, WA_ACTIVE));
        assert!(should_handle_focus_message(WM_ACTIVATE, WA_CLICKACTIVE));
        assert!(!should_handle_focus_message(WM_ACTIVATE, 0));
        // HIWORD set → window is minimized while activating.
        assert!(!should_handle_focus_message(
            WM_ACTIVATE,
            WA_ACTIVE | (1 << 16)
        ));
        assert!(!should_handle_focus_message(WM_ACTIVATEAPP, WA_ACTIVE));
    }

    #[test]
    fn focus_reassert_message_is_private_to_this_module() {
        assert!(FOCUS_REASSERT_MESSAGE >= WM_APP);
        assert_ne!(FOCUS_REASSERT_MESSAGE, WM_SETFOCUS);
        assert_ne!(FOCUS_REASSERT_MESSAGE, WM_ACTIVATE);
    }

    #[test]
    fn applies_exe_icons_before_taskbar_addtab() {
        let src = include_str!("win_shell.rs");
        let extract = src
            .find("ExtractIconExW")
            .expect("ExtractIconExW loads the exe icon");
        let seticon = src
            .find("WM_SETICON")
            .expect("WM_SETICON pushes ICON_BIG / ICON_SMALL");
        let addtab = src
            .rfind("taskbar.AddTab")
            .expect("AddTab registers the refreshed button");
        assert!(
            extract < seticon && seticon < addtab,
            "exe icons must land on the HWND before Explorer AddTab"
        );
    }
}
