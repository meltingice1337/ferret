//! Diagnostics: a rotating log file in the app's log folder
//! (%LOCALAPPDATA%\<identifier>\logs\ferret.log). Release builds have no console, so this
//! is the only record of what went wrong. Every window is its own process, so each line
//! carries the process id. The webview's logs (`@tauri-apps/plugin-log`) land in the same file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use log::LevelFilter;
use tauri::{AppHandle, Manager, Runtime, ipc::Invoke, plugin::TauriPlugin};
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
        // A line per formula it skips in an .xls.
        .level_for("calamine", LevelFilter::Warn)
        // A line per SSH / SFTP packet.
        .level_for("russh", LevelFilter::Warn)
        .level_for("russh_sftp", LevelFilter::Warn)
        .level_for("pageant", LevelFilter::Warn)
        .build()
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

// ---------- the main thread ----------
//
// Windows shows "Not Responding" once the window's thread hasn't answered for 5 s. Commands
// without `async` run on that thread (and so does everything else the window does): a slow one
// is logged by name, and a watchdog logs any stall, in a command or not, and how long it lasted.

/// A main-thread command that takes this long is logged.
const SLOW_COMMAND: Duration = Duration::from_millis(300);
/// The main thread not answering for this long is logged.
const STALL: Duration = Duration::from_secs(2);

static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();
/// The command the main thread is running, and since when.
static IN_COMMAND: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Wraps the command handler: commands that keep the main thread for long are logged by name.
pub fn timed<R: Runtime>(handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        if MAIN_THREAD.get() != Some(&std::thread::current().id()) {
            return handler(invoke);
        }
        let name = invoke.message.command().to_string();
        let started = Instant::now();
        *IN_COMMAND.lock().unwrap_or_else(|e| e.into_inner()) = Some((name.clone(), started));
        let handled = handler(invoke);
        *IN_COMMAND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let took = started.elapsed();
        if took >= SLOW_COMMAND {
            log::warn!("main thread: {name} took {took:.1?}");
        }
        handled
    }
}

/// From the main thread (setup): logs whenever it stops answering for a while, and what it was
/// running then.
pub fn watch_main_thread(app: AppHandle) {
    let _ = MAIN_THREAD.set(std::thread::current().id());
    let _ = std::thread::Builder::new().name("main thread watchdog".into()).spawn(move || loop {
        let answered = Arc::new(AtomicBool::new(false));
        let a = answered.clone();
        let mut asked = Instant::now();
        if app.run_on_main_thread(move || a.store(true, Ordering::Relaxed)).is_err() {
            return;
        }
        let mut reported = false;
        let mut tick = Instant::now();
        while !answered.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            // This thread didn't run either (the PC slept, a debugger paused the process): that's
            // not the main thread's doing.
            if !reported && tick.elapsed() > Duration::from_secs(1) {
                asked = Instant::now();
            }
            tick = Instant::now();
            if !reported && asked.elapsed() >= STALL {
                reported = true;
                let doing = match &*IN_COMMAND.lock().unwrap_or_else(|e| e.into_inner()) {
                    Some((name, since)) => format!("running {name} for {:.1?}", since.elapsed()),
                    None => "not in a command".into(),
                };
                log::warn!("main thread: not answering for {STALL:?} ({doing})");
            }
        }
        if reported {
            log::warn!("main thread: answering again after {:.1?}", asked.elapsed());
        }
        std::thread::sleep(Duration::from_secs(1));
    });
}

/// The folder with the log files (made if need be), which Settings → Open logs opens in a new
/// tab: handing it to Windows would open it in whatever the default file manager is.
#[tauri::command]
pub fn log_folder(app: AppHandle) -> Result<String, String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().into_owned())
}
