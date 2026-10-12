//! Diagnostics: a rotating log file in the app's log folder
//! (%LOCALAPPDATA%\<identifier>\logs\ferret.log). Release builds have no console, so this
//! is the only record of what went wrong. Every window is its own process, so each line
//! carries the process id. The webview's logs (`@tauri-apps/plugin-log`) land in the same file.

use log::LevelFilter;
use tauri::{AppHandle, Manager, Runtime, plugin::TauriPlugin};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

const MAX_FILE_BYTES: u128 = 5 << 20;

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    let level = if cfg!(debug_assertions) { LevelFilter::Debug } else { LevelFilter::Info };
    let mut targets = vec![Target::new(TargetKind::LogDir { file_name: Some("ferret".into()) })];
    if cfg!(debug_assertions) {
        targets.push(Target::new(TargetKind::Stdout));
    }
    tauri_plugin_log::Builder::new()
        .clear_targets()
        .targets(targets)
        .max_file_size(MAX_FILE_BYTES)
        .rotation_strategy(RotationStrategy::KeepSome(3))
        // Rotated file names use local time; the line format below replaces the plugin's.
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .format(|out, message, record| {
            // Webview targets carry the JS caller ("webview::send@http://…/log.ts:6:2"),
            // which is always the log.ts wrapper; the message says where it came from.
            let target = record.target();
            let target = if target.starts_with("webview") { "webview" } else { target };
            out.finish(format_args!(
                "{} [{}] {:<5} {}: {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                std::process::id(),
                record.level(),
                target,
                message
            ))
        })
        .level(level)
        // Framework internals are noise unless something is actually wrong.
        .level_for("tao", LevelFilter::Warn)
        .level_for("wry", LevelFilter::Warn)
        .level_for("tauri", LevelFilter::Warn)
        .level_for("hyper", LevelFilter::Warn)
        .level_for("hyper_util", LevelFilter::Warn)
        .level_for("reqwest", LevelFilter::Warn)
        .level_for("rustls", LevelFilter::Warn)
        .level_for("notify", LevelFilter::Warn)
        .build()
}

/// Records panics (including the ones jobs and the shell menu catch) before the
/// default hook runs.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string payload>".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        log::error!(target: "panic", "thread '{}' panicked at {at}: {msg}", thread.name().unwrap_or("<unnamed>"));
        previous(info);
    }));
}

/// Startup line: which build, which process, and how it was launched.
pub fn log_startup(app: &AppHandle, admin: bool) {
    let info = app.package_info();
    log::info!(
        "Ferret {} starting (pid {}, {}{}), args: {:?}",
        info.version,
        std::process::id(),
        if cfg!(debug_assertions) { "dev" } else { "release" },
        if admin { ", administrator" } else { "" },
        std::env::args().skip(1).collect::<Vec<_>>()
    );
}

/// A list of paths for a log line, capped so bulk operations don't flood the file.
pub fn paths<S: AsRef<str>>(items: &[S]) -> String {
    const SHOWN: usize = 10;
    let mut out = items.iter().take(SHOWN).map(|p| format!("\"{}\"", p.as_ref())).collect::<Vec<_>>().join(", ");
    if items.len() > SHOWN {
        out.push_str(&format!(" … and {} more", items.len() - SHOWN));
    }
    out
}

#[tauri::command]
pub fn open_log_folder(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
}
