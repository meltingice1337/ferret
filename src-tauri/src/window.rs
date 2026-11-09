//! The Tauri window: its handle for the Windows shell's menus and dialogs, its icon, backdrop,
//! placement and first appearance.

use tauri::WebviewWindow;

use ferret_core::integration;

/// Raw HWND (pointer-sized) so it can cross threads.
pub fn raw_hwnd(window: &WebviewWindow) -> Result<isize, String> {
    let h = window.hwnd().map_err(|e| e.to_string())?;
    Ok(h.0 as isize)
}

/// CSS-pixel client coordinates of the webview → physical screen coordinates.
pub fn screen_point(window: &WebviewWindow, x: f64, y: f64) -> (i32, i32) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let origin = window.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
    (origin.0 + (x * scale).round() as i32, origin.1 + (y * scale).round() as i32)
}

/// Where the window is on screen, for a restarted copy to open in the same place.
pub fn placement(window: &WebviewWindow) -> Result<integration::Placement, String> {
    let maximized = window.is_maximized().unwrap_or(false);
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    Ok(integration::Placement { x: pos.x, y: pos.y, w: size.width, h: size.height, maximized })
}

/// Puts a restarted window exactly where the one it replaces was.
pub fn apply_placement(window: &WebviewWindow) {
    let Some(r) = integration::placement() else { return };
    let _ = window.set_size(tauri::PhysicalSize::new(r.w, r.h));
    let _ = window.set_position(tauri::PhysicalPosition::new(r.x, r.y));
    if r.maximized {
        let _ = window.maximize();
    }
}

/// Shows the window, once. True when it was already on screen.
pub fn reveal(window: &WebviewWindow) -> bool {
    if integration::mark_shown() {
        return true;
    }
    if integration::e2e() {
        #[cfg(windows)]
        if let Ok(raw) = raw_hwnd(window) {
            integration::show_without_activating(raw);
        }
    } else {
        let _ = window.show();
        let _ = window.set_focus();
    }
    integration::log_shown();
    false
}

/// Tauri only sets the window's small icon. Without a big one the taskbar and
/// Alt+Tab fall back to the exe icon via the shell icon cache, which can stay
/// stale for a long time. Set ICON_BIG from the exe's own icon resource
/// (id 32512, embedded by tauri-build) at the window's DPI.
#[cfg(windows)]
pub fn set_big_icon(window: &WebviewWindow) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, WM_SETICON,
    };
    let Ok(raw) = raw_hwnd(window) else { return };
    let hwnd = HWND(raw as *mut std::ffi::c_void);
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
        let size = GetSystemMetricsForDpi(SM_CXICON, GetDpiForWindow(hwnd)).max(32);
        let resource = PCWSTR(32512 as *const u16);
        let image = LoadImageW(Some(HINSTANCE(module.0)), resource, IMAGE_ICON, size, size, LR_DEFAULTCOLOR);
        if let Ok(icon) = image {
            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(icon.0 as isize)));
        }
    }
}

#[cfg(not(windows))]
pub fn set_big_icon(_window: &WebviewWindow) {}

/// Applies the window backdrop. Returns false if the OS refused (e.g. Mica on
/// Windows 10), in which case the UI paints a solid background.
pub fn set_backdrop(window: &WebviewWindow, mode: &str, dark: bool) -> bool {
    #[cfg(windows)]
    {
        use window_vibrancy::{apply_acrylic, apply_mica, clear_acrylic, clear_mica};
        let _ = clear_mica(window);
        let _ = clear_acrylic(window);
        match mode {
            "mica" => apply_mica(window, Some(dark)).is_ok(),
            "acrylic" => {
                let tint = if dark { (18, 12, 20, 140) } else { (245, 240, 245, 120) };
                apply_acrylic(window, Some(tint)).is_ok()
            }
            _ => false,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, mode, dark);
        false
    }
}
