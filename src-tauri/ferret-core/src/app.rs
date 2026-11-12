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
    /// Settings, the session, favorites, recents, remembered views: small things, asked for by
    /// the window. Use [`with_db`], which logs a slow turn.
    pub db: Mutex<rusqlite::Connection>,
    /// A second connection to the same file, for folder sizes: counting saves thousands of rows
    /// from background threads, and behind one shared lock the window waited for each of them
    /// (its sidebar froze for 1.5 s once, drawing). None for a database in memory (tests), where
    /// folder sizes use `db`.
    sizes_db: Option<Mutex<rusqlite::Connection>>,
    pub watcher: Mutex<DirWatcher>,
}

static LINK: OnceLock<Box<dyn AppLink>> = OnceLock::new();
static STATE: OnceLock<AppState> = OnceLock::new();

/// Connects the core to its app: `link` for events and the UI thread, and the database it opened
/// (`db::open`). Call once, at startup.
pub fn init(link: Box<dyn AppLink>, db: rusqlite::Connection) {
    let _ = LINK.set(link);
    let sizes_db = db.path().filter(|p| !p.is_empty()).and_then(|p| crate::db::open_again(std::path::Path::new(p)).ok());
    if sizes_db.is_some() {
        // The window's connection never tidies the write-ahead log (copying it into the database
        // file and syncing the disk, tens of ms): the folder-size connection does, in the
        // background (its own writes, and `tidy_db`).
        let _ = db.execute_batch("PRAGMA wal_autocheckpoint=0;");
    }
    let _ = STATE.set(AppState { db: Mutex::new(db), sizes_db: sizes_db.map(Mutex::new), watcher: Mutex::new(DirWatcher::new()) });
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

/// Runs `f` with the database, if the app has opened one. Keep `f` to the database: the window
/// asks for it too, and waits while another thread has it (no looking at files in there).
#[track_caller]
pub fn with_db<T>(f: impl FnOnce(&mut rusqlite::Connection) -> T) -> Option<T> {
    turn(&state()?.db, "the database", std::panic::Location::caller(), f)
}

/// Runs `f` with the folder sizes' own connection to the database (see [`AppState`]).
#[track_caller]
pub fn with_sizes_db<T>(f: impl FnOnce(&mut rusqlite::Connection) -> T) -> Option<T> {
    let state = state()?;
    turn(state.sizes_db.as_ref().unwrap_or(&state.db), "the folder-size database", std::panic::Location::caller(), f)
}

/// A turn with a connection. One that waited, or took, a tenth of a second is logged with who
/// asked: that long on the UI thread is a visible hitch, and the log should say whose it was.
fn turn<T>(db: &Mutex<rusqlite::Connection>, what: &str, by: &std::panic::Location, f: impl FnOnce(&mut rusqlite::Connection) -> T) -> Option<T> {
    const SLOW: std::time::Duration = std::time::Duration::from_millis(100);
    let asked = std::time::Instant::now();
    let mut conn = db.lock().ok()?;
    let got = std::time::Instant::now();
    let out = f(&mut conn);
    drop(conn);
    let (waited, held) = (got - asked, got.elapsed());
    if waited >= SLOW || held >= SLOW {
        log::info!(target: "ferret::perf", "{what}: waited {} ms for it, then used it for {} ms ({}:{})", waited.as_millis(), held.as_millis(), by.file(), by.line());
    }
    Some(out)
}

/// Copies a write-ahead log that grew past a few MB into the database file, from a background
/// thread (the window's connection doesn't: see [`init`]). Folder-size writes do it as they go;
/// this is for a window that only ever saves settings and its session.
pub fn tidy_db() {
    let Some(state) = state() else { return };
    let Some(sizes) = state.sizes_db.as_ref() else { return };
    let Ok(conn) = sizes.lock() else { return };
    let big = conn.path().and_then(|p| std::fs::metadata(format!("{p}-wal")).ok()).is_some_and(|m| m.len() > 8 << 20);
    if big {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }
}
