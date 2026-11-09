//! Starting up, listing folders, the path bar, settings, favorites and recents.

use serde_json::Value;
use tauri::WebviewWindow;

use ferret_core::browse::{self, Bootstrap, DirListing, ResolvedInput, SharedState};
use ferret_core::{db, places};

type Res<T> = Result<T, String>;

#[tauri::command]
pub async fn bootstrap() -> Res<Bootstrap> {
    browse::bootstrap().await
}

#[tauri::command]
pub fn shared_state() -> Res<SharedState> {
    browse::shared_state()
}

#[tauri::command]
pub async fn get_places() -> Res<places::Places> {
    browse::get_places().await
}

#[tauri::command]
pub async fn list_dir(path: String) -> Res<DirListing> {
    browse::list_dir(path).await
}

#[tauri::command]
pub async fn resolve_input(input: String, current: Option<String>) -> Res<ResolvedInput> {
    browse::resolve_input(input, current).await
}

#[tauri::command]
pub fn open_path(path: String) -> Res<()> {
    browse::open_path(path)
}

/// Off the main thread: watching a folder opens it, which waits on a WSL distro that's starting
/// or a network share that's gone (20–30 s), and the window would stop answering meanwhile.
#[tauri::command(async)]
pub fn watch_dirs(paths: Vec<String>) -> Res<()> {
    browse::watch_dirs(paths)
}

#[tauri::command]
pub fn save_settings(settings: Value) -> Res<()> {
    browse::save_settings(settings)
}

#[tauri::command]
pub fn save_session(session: Value) -> Res<()> {
    browse::save_session(session)
}

#[tauri::command]
pub fn set_folder_view(path: String, view: Value, recursive: bool) -> Res<()> {
    browse::set_folder_view(path, view, recursive)
}

#[tauri::command]
pub fn clear_folder_view(path: String) -> Res<()> {
    browse::clear_folder_view(path)
}

#[tauri::command]
pub fn list_folder_views() -> Res<Vec<db::FolderView>> {
    browse::list_folder_views()
}

#[tauri::command]
pub fn clear_all_folder_views() -> Res<()> {
    browse::clear_all_folder_views()
}

#[tauri::command]
pub fn add_favorite(path: String, name: String, is_dir: bool) -> Res<Vec<db::Favorite>> {
    browse::add_favorite(path, name, is_dir)
}

#[tauri::command]
pub fn remove_favorite(path: String) -> Res<Vec<db::Favorite>> {
    browse::remove_favorite(path)
}

#[tauri::command]
pub fn update_favorite(path: String, name: String, color: Option<String>) -> Res<Vec<db::Favorite>> {
    browse::update_favorite(path, name, color)
}

#[tauri::command]
pub fn reorder_favorites(paths: Vec<String>) -> Res<Vec<db::Favorite>> {
    browse::reorder_favorites(paths)
}

#[tauri::command]
pub fn push_recent(path: String) -> Res<Vec<db::Recent>> {
    browse::push_recent(path)
}

#[tauri::command]
pub fn remove_recent(path: String) -> Res<Vec<db::Recent>> {
    browse::remove_recent(path)
}

#[tauri::command]
pub fn clear_recents() -> Res<Vec<db::Recent>> {
    browse::clear_recents()
}

#[tauri::command]
pub async fn drag_icon(path: Option<String>) -> Res<String> {
    browse::drag_icon(path).await
}

/// Applies the window backdrop. Returns false if the OS refused (e.g. Mica on
/// Windows 10), in which case the UI paints a solid background.
#[tauri::command]
pub fn set_backdrop(window: WebviewWindow, mode: String, dark: bool) -> bool {
    crate::window::set_backdrop(&window, &mode, dark)
}
