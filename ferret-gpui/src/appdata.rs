//! Where Ferret keeps its settings and its log, and the log itself.
//!
//! Installed (the installer's uninstall.exe sits beside the exe), it's the app's own folders, the
//! ones the Tauri app used (its identifier, `com.ferret.app`), so an update from it keeps every
//! setting, favorite and server: %APPDATA%\com.ferret.app\ferret.db and
//! %LOCALAPPDATA%\com.ferret.app\logs\ferret.log (add-ons: ...\addons). A build run from `target\` keeps its own, under
//! %TEMP%\ferret-gpui, so trying a build never touches the real settings (tests point TEMP at a
//! profile of their own). FERRET_DATA_DIR overrides both.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// The app's identifier: its folders' names, as the Tauri app's (tauri.conf.json).
pub const IDENTIFIER: &str = "com.ferret.app";

/// Started from an installed copy (the installer's uninstaller beside it).
pub fn installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("uninstall.exe").is_file())).unwrap_or(false)
}

fn known(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The folder of the settings database.
pub fn data_dir() -> PathBuf {
    if let Some(d) = known("FERRET_DATA_DIR") {
        return d;
    }
    match known("APPDATA") {
        Some(roaming) if installed() => roaming.join(IDENTIFIER),
        _ => std::env::temp_dir().join("ferret-gpui"),
    }
}

/// The folder for what stays on this computer (it doesn't roam with the Windows profile): the log
/// and add-ons, which are programs (addons.rs). Installed: %LOCALAPPDATA%\com.ferret.app; a build
/// run from `target\` (or one given FERRET_DATA_DIR) keeps them in its data folder.
pub fn local_dir() -> PathBuf {
    if known("FERRET_DATA_DIR").is_none() && installed() {
        if let Some(local) = known("LOCALAPPDATA") {
            return local.join(IDENTIFIER);
        }
    }
    data_dir()
}

/// The folder of the log.
pub fn log_dir() -> PathBuf {
    local_dir().join("logs")
}

/// A log file grows to this, then starts over (the previous one kept as ferret.log.1).
const LOG_MAX: u64 = 4 * 1024 * 1024;

struct FileLog {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl log::Log for FileLog {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        // Ferret's own lines from Info; the libraries' only when something's wrong.
        let ours = meta.target().starts_with("ferret") || meta.target() == "panic";
        meta.level() <= if ours { log::Level::Info } else { log::Level::Warn }
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "[{}][{}][{}] {}\n",
            chrono::Local::now().format("%Y-%m-%d][%H:%M:%S"),
            record.target(),
            record.level(),
            record.args()
        );
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if file.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > LOG_MAX) {
            *file = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }
        if file.is_none() {
            *file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = file.as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = f.flush();
        }
    }
}

/// Starts the log (and records panics in it). Without a folder for it, nothing is logged.
pub fn start_log() {
    let dir = log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let logger = FileLog { path: dir.join("ferret.log"), file: Mutex::new(None) };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
        ferret_core::logging::install_panic_hook();
        log::info!(target: "ferret", "Ferret {} started{}", env!("CARGO_PKG_VERSION"), if installed() { "" } else { " (not installed)" });
    }
}

/// The installed Ferret people use: installed, and not pointed at test data (FERRET_DATA_DIR,
/// as a test install is). Only it takes over as the default file manager and checks for updates
/// on its own.
pub fn real_install() -> bool {
    installed() && known("FERRET_DATA_DIR").is_none()
}
