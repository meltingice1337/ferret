//! The commands the page calls: thin `#[tauri::command]` wrappers over the core. Their names,
//! argument names and results are the page's API (src/lib/api.ts) and must not change.

pub mod archive;
pub mod browse;
pub mod fileops;
pub mod remote;
pub mod shell;
pub mod system;
