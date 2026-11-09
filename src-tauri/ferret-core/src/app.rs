//! What the core needs from the app that hosts it (the Tauri shell today), and the state they
//! share. The UI is reached only through [`AppLink`], a trait object set once at startup: code
//! the tests run (listing, connecting, copying) then never pulls in a window runtime, whose
//! dialogs a test program can't load. Without a link (tests, the indexer service) events go
//! nowhere and nothing runs on a UI thread.

use serde::Serialize;
use std::sync::{Mutex, OnceLock};

use crate::watch::DirWatcher;

/// The app's side: telling the window things, and running work on its UI thread.
pub trait AppLink: Send + Sync {
    /// Sends `event` with `payload` (as the page receives it) to the app's windows.
    fn emit(&self, event: &str, payload: serde_json::Value);
    /// Runs `f` on the UI (main) thread, without waiting for it.
    fn run_on_main_thread(&self, f: Box<dyn FnOnce() + Send>) -> Result<(), String>;
}

/// The settings database and the folder watcher, opened once by the app at startup ([`init`]).
pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub watcher: Mutex<DirWatcher>,
}

static LINK: OnceLock<Box<dyn AppLink>> = OnceLock::new();
static STATE: OnceLock<AppState> = OnceLock::new();

/// Connects the core to its app: `link` for events and the UI thread, and the database it opened
/// (`db::open`). Call once, at startup.
pub fn init(link: Box<dyn AppLink>, db: rusqlite::Connection) {
    let _ = LINK.set(link);
    let _ = STATE.set(AppState { db: Mutex::new(db), watcher: Mutex::new(DirWatcher::new()) });
    crate::remote::start();
}

/// The shared state, once the app has started ([`init`]).
pub fn state() -> Option<&'static AppState> {
    STATE.get()
}

/// The shared state, or why there's none (for commands that need it).
pub fn try_state() -> Result<&'static AppState, String> {
    state().ok_or_else(|| "Ferret hasn't started yet".to_string())
}

/// A window can be told things (and asked: remote/prompt.rs).
pub fn has_window() -> bool {
    LINK.get().is_some()
}

/// Sends an event to the window.
pub fn emit(event: &str, payload: impl Serialize) {
    if let Some(link) = LINK.get() {
        link.emit(event, serde_json::to_value(payload).unwrap_or_default());
    }
}

/// Runs `f` on the UI thread and waits for its result without blocking the async runtime.
pub async fn on_main_thread<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let link = LINK.get().ok_or("no window")?;
    let (tx, rx) = std::sync::mpsc::channel();
    link.run_on_main_thread(Box::new(move || {
        let _ = tx.send(f());
    }))?;
    crate::rt::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "main-thread task was dropped".to_string())?
}

/// Runs `f` with the database, if the app has opened one.
pub fn with_db<T>(f: impl FnOnce(&mut rusqlite::Connection) -> T) -> Option<T> {
    let state = state()?;
    let mut conn = state.db.lock().ok()?;
    Some(f(&mut conn))
}
