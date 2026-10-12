//! Instant whole-disk file search.
//!
//! * `index`  – struct-of-arrays name index (one per volume / walk root)
//! * `query`  – query syntax + parallel matching / ranking engine
//! * `engine` – volumes, incremental-refinement cache, result shaping
//! * `walk`   – no-admin provider: parallel walk + ReadDirectoryChangesW
//! * `mft`    – admin provider: MFT enumeration + USN journal tailing
//! * `ipc`    – named-pipe protocol (`\\.\pipe\ferret-index`)
//! * `service`– `ferret-indexer` Windows service + CLI
//!
//! Tauri commands (JS: `invoke('search_query', { query, limit, scope, frecency })`,
//! `invoke('search_status')`, `invoke('search_install_service')`) prefer the
//! FerretIndexer service when its pipe is reachable and otherwise fall back to
//! the in-process walk provider, which starts lazily in the background.

pub mod engine;
pub mod filetypes;
pub mod index;
pub mod ipc;
pub mod mft;
pub mod query;
pub mod service;
pub mod walk;
pub mod wsl;

#[cfg(test)]
mod bench;
#[cfg(test)]
mod tests;

pub use engine::{Engine, Hit, SearchRequest, SearchResults, VolumeStatus};

use parking_lot::Mutex;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};
use walk::{WalkOptions, WalkProvider};

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    /// "service" | "walk" | "none"
    pub source: String,
    pub service_installed: bool,
    pub service_running: bool,
    /// The running service is older than this app's [`ipc::INDEXER_VERSION`].
    pub service_outdated: bool,
    pub volumes: Vec<VolumeStatus>,
    pub building: bool,
}

struct PipeState {
    #[cfg(windows)]
    client: Option<ipc::PipeClient>,
    /// Don't retry connecting before this instant.
    next_try: Option<Instant>,
    ok_streak: u32,
}

struct AppSearch {
    engine: Arc<Engine>,
    walk: Mutex<Option<Arc<WalkProvider>>>,
    pipe: Mutex<PipeState>,
}

fn app() -> &'static AppSearch {
    static APP: OnceLock<AppSearch> = OnceLock::new();
    APP.get_or_init(|| AppSearch {
        engine: Engine::new(),
        walk: Mutex::new(None),
        pipe: Mutex::new(PipeState {
            #[cfg(windows)]
            client: None,
            next_try: None,
            ok_streak: 0,
        }),
    })
}

/// Starts the in-process walk provider once (non-blocking).
fn ensure_walk(a: &AppSearch) -> Arc<WalkProvider> {
    let mut w = a.walk.lock();
    if let Some(p) = w.as_ref() {
        return p.clone();
    }
    let p = WalkProvider::start(a.engine.clone(), WalkOptions::default_drives());
    *w = Some(p.clone());
    p
}

/// Frees the walk index once the service has proven reliable.
fn retire_walk(a: &'static AppSearch) {
    let Some(p) = a.walk.lock().take() else { return };
    std::thread::spawn(move || {
        p.save();
        p.stop();
    });
}

#[cfg(windows)]
fn service_request(a: &'static AppSearch, req: &ipc::Request, timeout: Duration) -> Option<ipc::Response> {
    let mut p = a.pipe.lock();
    if p.client.is_none() {
        if p.next_try.is_some_and(|t| Instant::now() < t) {
            return None;
        }
        match ipc::PipeClient::connect_to_service(Duration::from_millis(50)) {
            Ok(c) => p.client = Some(c),
            Err(e) => {
                // Expected when the service isn't installed; retried every few seconds.
                log::debug!("search service unavailable: {e}");
                p.next_try = Some(Instant::now() + Duration::from_secs(5));
                return None;
            }
        }
    }
    let c = p.client.as_mut()?;
    match c.request(req, timeout) {
        bad @ (Ok(ipc::Response::Error { .. }) | Err(_)) => {
            log::warn!("search service request failed: {bad:?}");
            p.client = None;
            p.ok_streak = 0;
            p.next_try = Some(Instant::now() + Duration::from_secs(2));
            None
        }
        Ok(r) => {
            p.ok_streak += 1;
            if p.ok_streak == 3 {
                drop(p);
                retire_walk(a);
            }
            Some(r)
        }
    }
}

/// Size + modified (ms) straight from the file system for the final hits.
fn fill_meta(hits: &mut [Hit]) {
    hits.par_iter_mut().for_each(|h| {
        // WSL indexes carry size and date already (and reading them via \\wsl.localhost is slow).
        if crate::paths::is_wsl(&h.path) {
            return;
        }
        if let Ok(m) = std::fs::symlink_metadata(crate::paths::for_io(&h.path)) {
            h.size = if h.is_dir { None } else { Some(m.len()) };
            h.modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64);
        }
    });
}

fn run_query(req: SearchRequest) -> SearchResults {
    let t0 = Instant::now();
    let a = app();
    #[cfg(windows)]
    let via_service = match service_request(a, &ipc::Request::Query(req.clone()), Duration::from_secs(3)) {
        Some(ipc::Response::Results(mut r)) => {
            r.source = "service".into();
            Some(r)
        }
        _ => None,
    };
    #[cfg(not(windows))]
    let via_service: Option<SearchResults> = None;
    let mut res = match via_service {
        Some(r) => r,
        None => {
            let w = ensure_walk(a);
            let mut r = a.engine.search(&req);
            r.source = if a.engine.volumes().is_empty() { "none".into() } else { "walk".into() };
            r.partial |= w.building();
            r
        }
    };
    merge_wsl(&mut res, &req);
    fill_meta(&mut res.results);
    res.took_us = t0.elapsed().as_micros() as u64;
    res
}

/// WSL distros are indexed by the app (the service only reads NTFS drives): their matches
/// join the drives' by score.
fn merge_wsl(res: &mut SearchResults, req: &SearchRequest) {
    wsl::refresh();
    let w = wsl::engine().search(req);
    res.partial |= w.partial;
    if w.results.is_empty() {
        return;
    }
    let limit = if req.limit == 0 { engine::DEFAULT_LIMIT } else { req.limit.min(engine::MAX_LIMIT) } as usize;
    res.total += w.total;
    res.results.extend(w.results);
    res.results.sort_by(|a, b| b.score.total_cmp(&a.score));
    res.results.truncate(limit);
}

fn status() -> SearchStatus {
    // Opening search is a good moment to (re)list WSL distros.
    wsl::refresh();
    let wsl_volumes = wsl::engine().status();
    let wsl_building = wsl_volumes.iter().any(|v| !v.ready);
    let a = app();
    let (installed, running) = service::service_state();
    #[cfg(windows)]
    if running {
        if let Some(ipc::Response::Status(st)) = service_request(a, &ipc::Request::Status, Duration::from_secs(2)) {
            return SearchStatus {
                source: "service".into(),
                service_installed: installed,
                service_running: running,
                service_outdated: st.indexer < ipc::INDEXER_VERSION,
                volumes: st.volumes.into_iter().chain(wsl_volumes).collect(),
                building: st.building || wsl_building,
            };
        }
    }
    let w = ensure_walk(a);
    let volumes = a.engine.status();
    SearchStatus {
        source: if volumes.is_empty() { "none".into() } else { "walk".into() },
        service_installed: installed,
        service_running: running,
        service_outdated: false,
        building: w.building() || volumes.iter().any(|v| !v.ready) || wsl_building,
        volumes: volumes.into_iter().chain(wsl_volumes).collect(),
    }
}

/// Searches file and folder names on all fixed drives (and WSL). `type:image` and the like
/// are turned into extension lists here, so every engine understands them.
#[tauri::command]
pub async fn search_query(
    query: String,
    limit: Option<u32>,
    scope: Option<String>,
    frecency: Option<HashMap<String, f32>>,
) -> Result<SearchResults, String> {
    let query = filetypes::expand(&query);
    let req = SearchRequest { query, limit: limit.unwrap_or(engine::DEFAULT_LIMIT), scope, frecency };
    tauri::async_runtime::spawn_blocking(move || run_query(req)).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_status() -> Result<SearchStatus, String> {
    tauri::async_runtime::spawn_blocking(status).await.map_err(|e| e.to_string())
}

/// Is the installed service older than this app expects? Unlike `search_status`, this
/// never starts the in-process walk, so it is cheap to call at startup.
#[tauri::command]
pub async fn search_service_outdated() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        if service::service_state().1 {
            if let Some(ipc::Response::Status(st)) = service_request(app(), &ipc::Request::Status, Duration::from_secs(2)) {
                return st.indexer < ipc::INDEXER_VERSION;
            }
        }
        false
    })
    .await
    .map_err(|e| e.to_string())
}

/// Launches `ferret-indexer.exe --install` elevated (UAC prompt); returns once
/// it has been launched.
#[tauri::command]
pub async fn search_install_service() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(launch_installer).await.map_err(|e| e.to_string())?
}

fn indexer_exe() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join("ferret-indexer.exe")];
    // Dev layouts: target/{debug,release}/ferret(.exe)
    if let Some(target) = dir.parent() {
        candidates.push(target.join("debug").join("ferret-indexer.exe"));
        candidates.push(target.join("release").join("ferret-indexer.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn launch_installer() -> Result<(), String> {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let exe = indexer_exe().ok_or_else(|| "ferret-indexer.exe was not found next to Ferret".to_string())?;
    log::info!("launching the search service installer: {}", exe.display());
    let file: Vec<u16> = exe.to_string_lossy().encode_utf16().chain(Some(0)).collect();
    let r = unsafe { ShellExecuteW(None, w!("runas"), PCWSTR(file.as_ptr()), w!("--install"), PCWSTR::null(), SW_HIDE) };
    let code = r.0 as isize;
    if code <= 32 {
        return Err(if code == 5 {
            "installation was cancelled or not permitted".into()
        } else {
            format!("could not launch the installer (ShellExecute error {code})")
        });
    }
    // Let the next query try the pipe right away.
    app().pipe.lock().next_try = None;
    Ok(())
}

#[cfg(not(windows))]
fn launch_installer() -> Result<(), String> {
    Err("the indexer service is only available on Windows".into())
}
