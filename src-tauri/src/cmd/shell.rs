//! The Windows shell: Explorer's context menus, Properties and Open with, the clipboard,
//! terminals and editors. Menus and dialogs are owned by the window, at the page's point.

use tauri::WebviewWindow;

use ferret_core::shell::{self, ClipboardFiles, OpenWithApp};
use ferret_core::shellmenu::{self, ShellMenu};
use crate::window::{raw_hwnd, screen_point};

type Res<T> = Result<T, String>;

/// Shows the real Explorer context menu for `paths` (same folder) at the
/// webview client position (`x`, `y` in CSS px). Returns the invoked verb
/// (`""` when the command has none), `"rename"` without invoking it, or `null`
/// when dismissed.
#[tauri::command]
pub async fn show_native_menu(window: WebviewWindow, paths: Vec<String>, x: f64, y: f64) -> Res<Option<String>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let raw = raw_hwnd(&window)?;
    let (sx, sy) = screen_point(&window, x, y);
    shell::show_native_menu(raw, paths, sx, sy).await
}

/// Explorer's folder-background menu for `folder`.
#[tauri::command]
pub async fn show_native_background_menu(window: WebviewWindow, folder: String, x: f64, y: f64) -> Res<Option<String>> {
    let raw = raw_hwnd(&window)?;
    let (sx, sy) = screen_point(&window, x, y);
    shell::show_native_background_menu(raw, folder, sx, sy).await
}

#[tauri::command]
pub async fn show_properties(window: WebviewWindow, paths: Vec<String>) -> Res<()> {
    if paths.is_empty() {
        return Ok(());
    }
    shell::show_properties(raw_hwnd(&window)?, paths).await
}

#[tauri::command]
pub async fn open_with_dialog(window: WebviewWindow, path: String) -> Res<()> {
    shell::open_with_dialog(raw_hwnd(&window)?, path).await
}

#[tauri::command]
pub async fn list_open_with(path: String) -> Res<Vec<OpenWithApp>> {
    shell::list_open_with(path).await
}

#[tauri::command]
pub async fn open_with(path: String, app_id: String) -> Res<()> {
    shell::open_with(path, app_id).await
}

#[tauri::command]
pub async fn clipboard_write_files(paths: Vec<String>, cut: bool) -> Res<()> {
    shell::clipboard_write_files(paths, cut).await
}

#[tauri::command]
pub async fn clipboard_after_cut(cut: Vec<String>, left: Vec<String>) -> Res<bool> {
    shell::clipboard_after_cut(cut, left).await
}

#[tauri::command]
pub fn clipboard_sequence() -> u32 {
    shell::clipboard_sequence()
}

#[tauri::command]
pub async fn clipboard_read_files() -> Res<Option<ClipboardFiles>> {
    shell::clipboard_read_files().await
}

#[tauri::command]
pub async fn open_terminal(path: String) -> Res<()> {
    shell::open_terminal(path).await
}

#[tauri::command]
pub async fn open_in_vscode(path: String) -> Res<()> {
    shell::open_in_vscode(path).await
}

#[tauri::command]
pub async fn reveal_in_explorer(path: String) -> Res<()> {
    shell::reveal_in_explorer(path).await
}

/// Explorer's extra context-menu entries for `paths`, or for the background of folder
/// `paths[0]` when `background`.
#[tauri::command]
pub async fn shell_menu_items(window: WebviewWindow, paths: Vec<String>, background: bool) -> Res<ShellMenu> {
    shellmenu::shell_menu_items(raw_hwnd(&window)?, paths, background).await
}

/// Runs entry `id` of the menu `token` from `shell_menu_items`.
#[tauri::command]
pub async fn shell_menu_invoke(window: WebviewWindow, token: u64, id: String) -> Res<Option<String>> {
    shellmenu::shell_menu_invoke(raw_hwnd(&window)?, token, id).await
}
