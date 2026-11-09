//! File operations: creating, renaming, copying and moving, deleting and restoring, item info,
//! jobs, folder sizes and the shell copy engine.

use tauri::WebviewWindow;

use ferret_core::fileops::{self, Conflict, ItemInfo, Policy, Stats, TextPreview};
use ferret_core::foldersize::{self, FolderSize};
use ferret_core::shellops::{self, ShellOp};
use ferret_core::{details, jobs};

type Res<T> = Result<T, String>;

#[tauri::command]
pub fn cancel_job(id: u64) {
    jobs::cancel_job(id)
}

#[tauri::command]
pub fn answer_job(id: u64, answer: String) {
    jobs::answer_job(id, answer)
}

#[tauri::command]
pub async fn create_folder(dir: String, name: Option<String>) -> Res<String> {
    fileops::create_folder(dir, name).await
}

#[tauri::command]
pub async fn remove_empty_folder(path: String) -> bool {
    fileops::remove_empty_folder(path).await
}

#[tauri::command]
pub async fn create_file(dir: String, name: String, content: Option<String>) -> Res<String> {
    fileops::create_file(dir, name, content).await
}

#[tauri::command]
pub async fn rename_item(path: String, new_name: String) -> Res<String> {
    fileops::rename_item(path, new_name).await
}

#[tauri::command(async)]
pub fn check_conflicts(sources: Vec<String>, dest: String) -> Vec<Conflict> {
    fileops::check_conflicts(sources, dest)
}

#[tauri::command(async)]
pub fn transfer(op: String, sources: Vec<String>, dest: String, policy: Policy) -> Res<u64> {
    fileops::transfer(op, sources, dest, policy)
}

#[tauri::command(async)]
pub fn undo_moves(pairs: Vec<(String, String)>) -> Res<u64> {
    fileops::undo_moves(pairs)
}

#[tauri::command(async)]
pub fn delete_items(items: Vec<String>, permanent: bool) -> Res<u64> {
    fileops::delete_items(items, permanent)
}

#[tauri::command]
pub async fn restore_from_trash(items: Vec<String>) -> Res<Vec<String>> {
    fileops::restore_from_trash(items).await
}

#[tauri::command]
pub async fn find_in_trash(items: Vec<String>) -> Res<Vec<String>> {
    fileops::find_in_trash(items).await
}

#[tauri::command]
pub async fn item_info(path: String) -> Res<ItemInfo> {
    fileops::item_info(path).await
}

#[tauri::command]
pub async fn folder_stats(items: Vec<String>) -> Res<Stats> {
    fileops::folder_stats(items).await
}

/// Off the main thread: with thousands of items this can take a moment.
#[tauri::command(async)]
pub fn folder_sizes(requester: u32, paths: Vec<String>, refresh: bool, dir: Option<String>) -> Vec<FolderSize> {
    foldersize::folder_sizes(requester, paths, refresh, dir)
}

#[tauri::command(async)]
pub fn folder_sizes_invalidate(paths: Vec<String>, deep: bool) {
    foldersize::folder_sizes_invalidate(paths, deep)
}

#[tauri::command]
pub fn set_attributes(path: String, readonly: bool, hidden: bool) -> Res<()> {
    fileops::set_attributes(path, readonly, hidden)
}

#[tauri::command]
pub async fn read_text(path: String, max_bytes: Option<u64>) -> Res<TextPreview> {
    fileops::read_text(path, max_bytes).await
}

#[tauri::command]
pub async fn read_bytes(path: String, count: Option<u64>) -> Res<Vec<u8>> {
    fileops::read_bytes(path, count).await
}

#[tauri::command]
pub async fn file_details(path: String) -> Res<details::FileDetails> {
    details::file_details(path).await
}

/// Runs `op` through the shell copy engine as a background job and returns its id.
/// With `elevate`, the shell may show the UAC consent prompt (owned by `window`).
#[tauri::command]
pub fn shell_op(window: WebviewWindow, op: ShellOp, elevate: bool) -> Res<u64> {
    let owner = crate::window::raw_hwnd(&window).ok();
    shellops::shell_op(owner, op, elevate)
}
