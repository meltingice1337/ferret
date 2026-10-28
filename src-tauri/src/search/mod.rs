//! Instant whole-disk file search.
//!
//! * `index`  – struct-of-arrays name index (one per volume / walk root)
//! * `query`  – query syntax + parallel matching / ranking engine
//! * `engine` – volumes, incremental-refinement cache, result shaping
//! * `walk`   – no-admin provider: parallel walk + ReadDirectoryChangesW
//! * `mft`    – admin provider: MFT enumeration + USN journal tailing
//! * `ipc`    – named-pipe protocol (`\\.\pipe\ferret-index`)
//! * `service`– `ferret-indexer` Windows service + CLI
//! * `content`– `content:"text"`: reads the files under a folder (in the app, never the service)
//!
//! Tauri commands (JS: `invoke('search_query', { query, limit, scope, frecency })`,
//! `invoke('search_status')`, `invoke('search_install_service')`) prefer the
//! FerretIndexer service when its pipe is reachable and otherwise fall back to
//! the in-process walk provider, which starts lazily in the background.

pub mod content;
pub mod engine;
pub mod filetypes;
pub mod index;
pub mod ipc;
pub mod mft;
pub mod query;
pub mod remote;
pub mod service;
pub mod signed;
pub mod sizes;
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

/// Folder sizes from the drive index service (its file table: sizes.rs), in the order asked,
/// when it's running and knows how (indexer 3 on). Whether it does is asked once a minute, so
/// without it this costs nothing.
pub fn service_folder_sizes(paths: &[String]) -> Option<Vec<Option<ipc::FolderTotal>>> {
    #[cfg(windows)]
    {
        static CAN: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
        let a = app();
        let known = (*CAN.lock()).filter(|(t, _)| t.elapsed() < Duration::from_secs(60)).map(|(_, ok)| ok);
        let can = known.unwrap_or_else(|| {
            let ok = matches!(service_request(a, &ipc::Request::Status, Duration::from_secs(1)), Some(ipc::Response::Status(st)) if st.indexer >= 3);
            *CAN.lock() = Some((Instant::now(), ok));
            ok
        });
        if !can {
            return None;
        }
        match service_request(a, &ipc::Request::Sizes { paths: paths.to_vec() }, Duration::from_secs(5)) {
            Some(ipc::Response::Sizes { sizes }) if sizes.len() == paths.len() => Some(sizes),
            _ => None,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        None
    }
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
        // WSL and server indexes carry size and date already (reading them via \\wsl.localhost
        // is slow; a server's paths aren't Windows paths at all).
        if crate::paths::is_wsl(&h.path) || crate::paths::is_remote(&h.path) {
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

/// WSL distros and servers are indexed by the app (the service only reads NTFS drives): their
/// matches join the drives' by score.
fn merge_wsl(res: &mut SearchResults, req: &SearchRequest) {
    wsl::refresh();
    remote::refresh();
    merge(res, req, wsl::engine().search(req));
    merge(res, req, remote::engine().search(req));
}

fn merge(res: &mut SearchResults, req: &SearchRequest, w: SearchResults) {
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
    // Opening search is a good moment to (re)list WSL distros and connected servers.
    wsl::refresh();
    remote::refresh();
    let wsl_volumes: Vec<_> = wsl::engine().status().into_iter().chain(remote::engine().status()).collect();
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
    // `content:` reads the files under the palette's folder instead (content.rs).
    let (rest, needles) = content::split(&query);
    if !needles.is_empty() {
        let rest = filetypes::expand(&rest);
        let limit = limit.unwrap_or(engine::DEFAULT_LIMIT).min(engine::MAX_LIMIT) as usize;
        return tauri::async_runtime::spawn_blocking(move || content::search(&rest, &needles, scope.as_deref(), limit, true))
            .await
            .map_err(|e| e.to_string());
    }
    let query = filetypes::expand(&query);
    let req = SearchRequest { query, limit: limit.unwrap_or(engine::DEFAULT_LIMIT), scope, frecency };
    tauri::async_runtime::spawn_blocking(move || run_query(req)).await.map_err(|e| e.to_string())
}

/// A search opened as a tab: the virtual folder `search:<query>` (saved searches in the
/// sidebar, "Open results in a tab" in Ctrl+K). Lists like a folder, always current.
pub const SEARCH_PREFIX: &str = "search:";
/// Most results a search tab lists.
const TAB_LIMIT: u32 = 2_000;

pub fn search_query_of(path: &str) -> Option<&str> {
    path.get(..SEARCH_PREFIX.len()).filter(|p| p.eq_ignore_ascii_case(SEARCH_PREFIX)).map(|_| path[SEARCH_PREFIX.len()..].trim())
}

/// The matches of `query` as folder entries, each with the folder it's in as `origin`.
/// Items that vanished since they were indexed are left out.
pub fn list_matches(query: &str) -> Vec<crate::listing::Entry> {
    let (rest, needles) = content::split(query);
    let hits = if needles.is_empty() {
        run_query(SearchRequest { query: filetypes::expand(query), limit: TAB_LIMIT, scope: None, frecency: None }).results
    } else {
        content::search(&filetypes::expand(&rest), &needles, None, TAB_LIMIT as usize, false).results
    };
    hits.par_iter()
        .filter_map(|h| {
            // A server's matches as its index has them (its files aren't on this PC).
            let mut e = if crate::paths::is_remote(&h.path) { server_entry(h) } else { crate::listing::entry_for(&h.path)? };
            e.origin = crate::paths::parent(&h.path);
            Some(e)
        })
        .collect()
}

fn server_entry(h: &engine::Hit) -> crate::listing::Entry {
    let ext = if h.is_dir {
        String::new()
    } else {
        h.name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map(|(_, e)| e.to_lowercase()).unwrap_or_default()
    };
    crate::listing::Entry {
        name: h.name.clone(),
        path: h.path.clone(),
        is_dir: h.is_dir,
        is_symlink: false,
        hidden: h.name.starts_with('.'),
        size: h.size.unwrap_or(0),
        modified: h.modified,
        created: None,
        ext,
        in_archive: false,
        origin: None,
    }
}

/// Stops a search inside files that the palette started (something newer was typed).
#[tauri::command]
pub fn search_cancel() {
    content::cancel();
}

#[tauri::command]
pub async fn search_status() -> Result<SearchStatus, String> {
    tauri::async_runtime::spawn_blocking(status).await.map_err(|e| e.to_string())
}

/// What the installed service needs, from what it says about itself.
#[derive(Debug, PartialEq, Eq)]
enum ServiceNeeds {
    Nothing,
    /// To update itself to the release this Ferret ships (`Request::Update`: no prompt).
    Update,
    /// The installer (one UAC prompt, which the user is asked for): it can't update itself.
    Installer,
}

/// The service should be the release this Ferret ships, or a newer one. One that can update
/// itself (indexer 2 on) is updated when it's older; one that can't needs the installer when its
/// indexer is older than this app's.
fn service_needs(st: &ipc::ServiceStatus, app_version: &str) -> ServiceNeeds {
    if st.indexer < 2 {
        return if st.indexer < ipc::INDEXER_VERSION { ServiceNeeds::Installer } else { ServiceNeeds::Nothing };
    }
    // 1.2.3 as numbers (anything after them, like "-dev", aside).
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    if parts(app_version) > parts(&st.version) || st.indexer < ipc::INDEXER_VERSION {
        ServiceNeeds::Update
    } else {
        ServiceNeeds::Nothing
    }
}

/// Is the installed service out of date in a way that needs the user (the installer's UAC
/// prompt)? Called once at startup: a service that can update itself to this release is asked to,
/// quietly, here. Unlike `search_status`, this never starts the in-process walk.
#[tauri::command]
pub async fn search_service_outdated() -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        if service::service_state().1 {
            if let Some(ipc::Response::Status(st)) = service_request(app(), &ipc::Request::Status, Duration::from_secs(2)) {
                return match service_needs(&st, env!("CARGO_PKG_VERSION")) {
                    ServiceNeeds::Nothing => false,
                    ServiceNeeds::Installer => true,
                    // (A development build's indexer isn't signed: the service would refuse it.)
                    ServiceNeeds::Update if cfg!(debug_assertions) => false,
                    ServiceNeeds::Update => {
                        log::info!(
                            "search service {} (indexer {}) is older than this app ({}): updating it",
                            st.version,
                            st.indexer,
                            env!("CARGO_PKG_VERSION")
                        );
                        match update_service_itself() {
                            SelfUpdate::Started => false,
                            // It doesn't know how after all: the installer, then.
                            SelfUpdate::CantUpdate => true,
                            SelfUpdate::Refused(e) | SelfUpdate::NoAnswer(e) => {
                                log::warn!("the search service didn't update: {e}");
                                false
                            }
                        }
                    }
                };
            }
        }
        false
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(windows)]
enum SelfUpdate {
    Started,
    /// From before services could (indexer 1: "bad request"): it needs the installer.
    CantUpdate,
    /// It said no (not signed, older than itself…).
    Refused(String),
    NoAnswer(String),
}

/// Asks the running service to update itself to Ferret's own `ferret-indexer.exe`, which it
/// installs only if that's a newer release signed with Ferret's release key
/// (`service::update_from`), so a copy swapped into Ferret's folder (which this account can
/// change) can't become the service. No UAC prompt.
#[cfg(windows)]
fn update_service_itself() -> SelfUpdate {
    let Some(exe) = indexer_exe() else { return SelfUpdate::Refused("ferret-indexer.exe was not found next to Ferret".into()) };
    let ask = ipc::Request::Update { exe: exe.to_string_lossy().into_owned() };
    let answer = ipc::PipeClient::connect_to_service(Duration::from_secs(2)).and_then(|mut c| c.request(&ask, Duration::from_secs(30)));
    match answer {
        Ok(ipc::Response::Updating) => {
            log::info!("the search service is updating itself to {}", exe.display());
            app().pipe.lock().next_try = None;
            SelfUpdate::Started
        }
        Ok(ipc::Response::Error { message }) if message.starts_with("bad request") => SelfUpdate::CantUpdate,
        Ok(ipc::Response::Error { message }) => SelfUpdate::Refused(message),
        other => SelfUpdate::NoAnswer(format!("{other:?}")),
    }
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
    // A running service updates itself (`update_service_itself`: no UAC prompt). Services from
    // before that (indexer 1) are reinstalled below, as is a first install, and a development
    // build's (it isn't signed).
    if !cfg!(debug_assertions) && service::service_state().1 {
        match update_service_itself() {
            SelfUpdate::Started => return Ok(()),
            SelfUpdate::CantUpdate => log::info!("the search service can't update itself: installing it again"),
            SelfUpdate::NoAnswer(e) => log::info!("the search service didn't answer ({e}): installing it again"),
            // Never the installer instead: that would install the copy it refused.
            SelfUpdate::Refused(e) => {
                log::warn!("the search service didn't update: {e}");
                return Err(format!("The drive index service wasn't updated: {e}"));
            }
        }
    }
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

#[cfg(test)]
mod service_tests {
    use super::*;

    fn st(version: &str, indexer: u32) -> ipc::ServiceStatus {
        ipc::ServiceStatus { version: version.into(), indexer, ..Default::default() }
    }

    #[test]
    fn the_service_follows_this_release() {
        let me = "0.8.2";
        assert_eq!(service_needs(&st("0.8.2", 3), me), ServiceNeeds::Nothing);
        assert_eq!(service_needs(&st("0.8.0", 3), me), ServiceNeeds::Update, "a development build of an earlier version");
        assert_eq!(service_needs(&st("0.8.0", 2), me), ServiceNeeds::Update, "0.8.0's service");
        assert_eq!(service_needs(&st("0.8.2", 2), me), ServiceNeeds::Update, "the same version with an older indexer");
        assert_eq!(service_needs(&st("0.7.2", 1), me), ServiceNeeds::Installer, "from before services updated themselves");
        assert_eq!(service_needs(&st("0.5.0", 0), me), ServiceNeeds::Installer);
        assert_eq!(service_needs(&st("0.9.0", 3), me), ServiceNeeds::Nothing, "newer than this app");
        assert_eq!(service_needs(&st("0.8.10", 3), "0.8.9"), ServiceNeeds::Nothing, "numbers, not text");
        assert_eq!(service_needs(&st("0.8.9", 3), "0.8.10"), ServiceNeeds::Update);
    }
}
