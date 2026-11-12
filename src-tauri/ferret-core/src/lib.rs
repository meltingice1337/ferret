//! Ferret without its UI: folders and listings, file operations and jobs, previews (archives,
//! media, office files), folder sizes, whole-disk search and its service, the Recycle Bin, the
//! Windows shell (menus, clipboard, thumbnails), Windows integration, network folders and SSH
//! connections.
//!
//! An app (the Tauri shell in src-tauri today) calls these modules' functions and links itself
//! in once at startup with [`app::init`]: that's how the core sends events and runs work on the
//! UI thread, and where the settings database lives. Async work runs on [`rt`]'s runtime.

pub mod app;
pub mod archive;
pub mod browse;
pub mod db;
pub mod details;
pub mod fileops;
pub mod foldersize;
pub mod integration;
pub mod jobs;
pub mod linux;
pub mod listing;
pub mod logging;
pub mod media;
pub mod network;
pub mod office;
pub mod paths;
pub mod places;
pub mod recycle;
pub mod redirect;
pub mod remote;
pub mod remove;
pub mod rt;
pub mod search;
pub mod shell;
pub mod shellmenu;
pub mod shellops;
pub mod standby;
pub mod vfs;
pub mod watch;
pub mod wsl;
