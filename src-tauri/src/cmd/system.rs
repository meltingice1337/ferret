//! Search, the Recycle Bin, Windows integration (default file manager, launch arguments) and
//! the window itself (showing it, restarting it as administrator, opening another).

use std::collections::HashMap;

use tauri::WebviewWindow;

use ferret_core::integration::{self, DefaultStatus, LaunchTarget, WindowInfo};
use ferret_core::recycle::{self, RecycleInfo};
use ferret_core::search::{self, engine::SearchResults, SearchStatus};

type Res<T> = Result<T, String>;

#[tauri::command]
pub async fn search_query(query: String, limit: Option<u32>, scope: Option<String>, frecency: Option<HashMap<String, f32>>) -> Res<SearchResults> {
    search::search_query(query, limit, scope, frecency).await
}

#[tauri::command]
pub async fn search_status() -> Res<SearchStatus> {
    search::search_status().await
}

#[tauri::command]
pub fn search_cancel() {
    search::search_cancel()
}

#[tauri::command]
pub async fn search_install_service() -> Res<()> {
    search::search_install_service().await
}

#[tauri::command]
pub async fn search_service_outdated() -> Res<bool> {
    search::search_service_outdated().await
}

#[tauri::command]
pub async fn recycle_info() -> Res<RecycleInfo> {
    recycle::recycle_info().await
}

#[tauri::command(async)]
pub fn no_recycle_bin(paths: Vec<String>) -> Vec<String> {
    recycle::no_recycle_bin(paths)
}

#[tauri::command(async)]
pub fn empty_recycle_bin() -> Res<u64> {
    recycle::empty_recycle_bin()
}

#[tauri::command]
pub fn launch_targets() -> Vec<LaunchTarget> {
    integration::launch_targets()
}

#[tauri::command]
pub fn default_manager_status() -> DefaultStatus {
    integration::default_manager_status()
}

#[tauri::command]
pub fn set_default_manager(enable: bool) -> Res<DefaultStatus> {
    integration::set_default_manager(enable)
}

#[tauri::command]
pub fn open_in_explorer(path: String) -> Res<()> {
    integration::open_in_explorer(path)
}

#[tauri::command]
pub fn window_info() -> WindowInfo {
    integration::window_info()
}

#[tauri::command]
pub fn show_window(window: WebviewWindow) -> bool {
    crate::window::reveal(&window)
}

#[tauri::command]
pub fn new_window(path: Option<String>) -> Res<()> {
    integration::new_window(path)
}

/// Restarts this window as administrator: Windows shows the UAC prompt, then an elevated
/// copy opens with the same tabs in the same place, and this one goes away (so it looks like
/// the same window becoming administrator). "Cancelled" if the prompt was declined.
#[tauri::command]
pub async fn restart_elevated(window: WebviewWindow, tabs: Vec<String>, active: usize) -> Res<()> {
    let at = crate::window::placement(&window)?;
    let owner = crate::window::raw_hwnd(&window).ok();
    integration::restart_elevated(owner, at, tabs, active).await?;
    let _ = window.destroy();
    Ok(())
}
