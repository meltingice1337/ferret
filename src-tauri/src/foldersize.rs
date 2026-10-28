//! Folder sizes for the list view: counted in the background and saved, so any window shows
//! the last known size at once.
//!
//! What's counted, most urgent first:
//! 1. the folders on screen (in any tab);
//! 2. nearby ones: the folder on screen and each one above it, so going up shows sizes at once,
//!    and folders that were on screen when a tab moved on;
//! 3. everything else: the home folder, the drives and the running WSL distros.
//!
//! Background counting (2 and 3) steps aside whenever a folder on screen waits, and goes on later
//! from where it got: a count saves the sizes of the folders inside as it finishes each (those
//! worth keeping), and it uses recent sizes of folders inside instead of walking them again.
//!
//! Two low-priority workers count Windows folders; WSL folders are counted by Linux itself, many
//! at a time (`wsl_worker`), with Ferret's own Linux program (`WSL_HELPER`) or else with find. A size on screen is counted again (its old value showing meanwhile,
//! marked as refreshing) when it's ten minutes old, when Ferret changed something inside the
//! folder, or on refresh; other sizes once a day.

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::{db, paths};

/// Milliseconds after which a size on screen is counted again.
const STALE_AFTER: i64 = 10 * 60 * 1000;
/// Milliseconds after which a size nobody is looking at is counted again.
const BACKGROUND_STALE_AFTER: i64 = 24 * 60 * 60 * 1000;
/// How often the background work looks for folders that are due (home, drives, distros).
const PLAN_EVERY: Duration = Duration::from_secs(10 * 60);
const WORKERS: usize = 2;
/// How often a folder that's still being counted reports its running total.
const PROGRESS_EVERY: Duration = Duration::from_millis(300);
/// The folders a count finds inside are saved in bundles of this many (and when it ends).
const LEARN_EVERY: usize = 2000;
/// Folders deep inside one counted are kept from that count from this size (or number of files)
/// up; see `worth_keeping`.
const KEEP_BYTES: u64 = 1 << 20;
const KEEP_FILES: u64 = 1000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub path: String,
    pub bytes: u64,
    /// Not known when a WSL distro can't list them (no GNU find there).
    pub files: Option<u64>,
    /// Some folders inside couldn't be read.
    pub partial: bool,
    /// False while still counting (`bytes` is the total so far).
    pub done: bool,
    /// An earlier count, being counted again right now.
    pub refreshing: bool,
    /// When this size was counted (Unix milliseconds); with sizes of folders inside used from
    /// earlier counts, when the oldest of them was.
    pub counted_at: i64,
}

struct Cached {
    size: FolderSize,
    /// Something inside changed since it was counted.
    stale: bool,
}

/// Why a folder is counted, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// On screen.
    Shown,
    /// Around what's on screen: the folder itself and those above it, and folders that were on
    /// screen.
    Near,
    /// Everything else: home, drives, distros.
    Fill,
}

struct Job {
    path: String,
    key: String,
    /// Tabs showing this folder: while any does, it goes first.
    wanted_by: HashSet<u32>,
    /// Its rank when no tab shows it.
    background: Rank,
    /// Counted again for real (refresh): no earlier size of a folder inside is used.
    refresh: bool,
    /// Asks the count to stop, to make way for a folder on screen (it's queued again).
    stop: Arc<AtomicBool>,
    /// In a WSL distro: counted by `wsl_worker`.
    wsl: bool,
}

impl Job {
    fn new(path: String, key: String, background: Rank) -> Self {
        let wsl = paths::is_wsl(&path);
        Job { path, key, wanted_by: HashSet::new(), background, refresh: false, stop: Arc::default(), wsl }
    }

    fn rank(&self) -> Rank {
        if self.wanted_by.is_empty() { self.background } else { Rank::Shown }
    }
}

#[derive(Default)]
struct Shared {
    cache: HashMap<String, Cached>,
    queue: VecDeque<Job>,
    /// Folders being counted right now.
    running: HashMap<String, Running>,
    /// Windows workers waiting for work.
    idle: usize,
    started: bool,
    /// The home folder, drives and distros are counted in the background (since a tab showed
    /// sizes).
    planning: bool,
    /// When sizes were last marked out of date (something was copied, moved, deleted…).
    changed: Option<Instant>,
}

/// How far behind a change the drive index service can be: it reads the change journal a few
/// times a second. A size asked for right after a change (Ferret asks as soon as its job ends)
/// waits until then, or the service would hand back the size from before.
const JOURNAL_LAG: Duration = Duration::from_millis(800);

struct Running {
    /// Tabs that want it.
    who: HashSet<u32>,
    /// Stops the count.
    stop: Arc<AtomicBool>,
    path: String,
    wsl: bool,
}

/// What the counters are doing, for the status bar: sent as `folder-sizes-activity` whenever it
/// changes.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Activity {
    /// Folders on screen waiting or being counted.
    left: usize,
    /// Other folders waiting or being counted (in the background).
    background: usize,
    /// Being counted right now (a few of them, those on screen first).
    counting: Vec<String>,
}

fn report(app: &AppHandle, st: &Shared) {
    let shown = st.queue.iter().filter(|j| !j.wanted_by.is_empty()).count()
        + st.running.values().filter(|r| !r.who.is_empty()).count();
    let mut now: Vec<&Running> = st.running.values().collect();
    now.sort_by_key(|r| r.who.is_empty());
    let counting = now.iter().take(3).map(|r| r.path.clone()).collect();
    let background = st.queue.len() + st.running.len() - shown;
    let _ = app.emit("folder-sizes-activity", Activity { left: shown, background, counting });
}

struct Sizes {
    state: Mutex<Shared>,
    wake: Condvar,
}

fn sizes() -> &'static Sizes {
    static SIZES: OnceLock<Sizes> = OnceLock::new();
    SIZES.get_or_init(|| Sizes { state: Mutex::new(Shared::default()), wake: Condvar::new() })
}

fn with_db<T>(app: &AppHandle, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> Option<T> {
    let state = app.try_state::<AppState>()?;
    let conn = state.db.lock().ok()?;
    f(&conn).ok()
}

fn from_saved(v: db::SavedSize) -> FolderSize {
    FolderSize { path: v.path, bytes: v.bytes, files: v.files, partial: v.partial, done: true, refreshing: false, counted_at: v.counted_at }
}

fn to_saved(s: &FolderSize) -> db::SavedSize {
    db::SavedSize { path: s.path.clone(), bytes: s.bytes, files: s.files, partial: s.partial, counted_at: s.counted_at, stale: false }
}

/// Sizes saved earlier (by any window) that this window hasn't looked at yet.
fn load_saved(app: &AppHandle, keys: impl IntoIterator<Item = String>) {
    let unseen: Vec<String> = {
        let st = sizes().state.lock();
        keys.into_iter().filter(|k| !st.cache.contains_key(k)).collect()
    };
    if unseen.is_empty() {
        return;
    }
    if let Some(saved) = with_db(app, |c| db::folder_sizes(c, &unseen)) {
        let mut st = sizes().state.lock();
        for (key, v) in saved {
            let stale = v.stale;
            st.cache.entry(key).or_insert(Cached { size: from_saved(v), stale });
        }
    }
}

/// Sizes of a tab's folders, in display order. Known ones (from this window or saved by any)
/// come back at once; missing ones, then out-of-date ones, are counted first thing and reported
/// as `folder-size` events (also while counting). A newer request from the same tab replaces the
/// one before: what it no longer shows goes on in the background. `refresh` counts everything
/// again (F5). `dir` is the folder the tab shows: it and the folders above it are counted in the
/// background, so going up shows their sizes at once.
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn folder_sizes(app: AppHandle, requester: u32, paths: Vec<String>, refresh: bool, dir: Option<String>) -> Vec<FolderSize> {
    let s = sizes();
    let wanted: Vec<(String, String)> =
        paths.iter().filter(|p| countable(p)).map(|p| (paths::key(p), paths::normalize(p))).collect();
    // From the drive index service where it has them (current to the second): the rest from what
    // was counted here before, or counted now.
    let indexed = from_drive_index(&wanted);
    load_saved(&app, wanted.iter().filter(|(k, _)| !indexed.contains_key(k)).map(|(k, _)| k.clone()));

    let mut st = s.state.lock();
    if !st.started {
        st.started = true;
        for _ in 0..WORKERS {
            let app = app.clone();
            std::thread::spawn(move || worker(app));
        }
        let app = app.clone();
        std::thread::spawn(move || wsl_worker(app));
    }
    if dir.is_some() && !st.planning {
        st.planning = true;
        let app = app.clone();
        std::thread::spawn(move || planner(app));
    }
    let keys: HashSet<&str> = wanted.iter().map(|(k, _)| k.as_str()).collect();

    // What this tab no longer shows goes on in the background.
    for job in st.queue.iter_mut() {
        if !keys.contains(job.key.as_str()) {
            job.wanted_by.remove(&requester);
        }
    }
    for (key, r) in st.running.iter_mut() {
        if !keys.contains(key.as_str()) {
            r.who.remove(&requester);
        }
    }

    let now = db::now();
    let mut out = Vec::new();
    let (mut missing, mut outdated) = (Vec::new(), Vec::new());
    for (key, path) in wanted {
        if let Some(size) = indexed.get(&key) {
            st.cache.insert(key, Cached { size: size.clone(), stale: false });
            out.push(size.clone());
            continue;
        }
        match st.cache.get(&key) {
            Some(c) if !refresh && !c.stale && now - c.size.counted_at < STALE_AFTER => out.push(c.size.clone()),
            Some(c) => {
                out.push(FolderSize { refreshing: true, ..c.size.clone() });
                outdated.push((key, path));
            }
            None => missing.push((key, path)),
        }
    }
    let (known, to_count) = (out.len() - outdated.len() - indexed.len(), missing.len() + outdated.len());
    // Folders with no size at all come before refreshing known ones.
    for (key, path) in missing.into_iter().chain(outdated) {
        if let Some(r) = st.running.get_mut(&key) {
            r.who.insert(requester);
            r.stop.store(false, Ordering::Relaxed);
        } else if let Some(job) = st.queue.iter_mut().find(|j| j.key == key) {
            job.wanted_by.insert(requester);
            job.refresh |= refresh;
        } else {
            let mut job = Job::new(path, key, Rank::Near);
            job.wanted_by.insert(requester);
            job.refresh = refresh;
            st.queue.push_back(job);
        }
    }
    make_way(&st);
    let shown = st.queue.iter().filter(|j| !j.wanted_by.is_empty()).count();
    let (background, running) = (st.queue.len() - shown, st.running.len());
    report(&app, &st);
    drop(st);
    s.wake.notify_all();
    log::info!(
        "folder sizes: tab {requester} asked for {} ({}{known} known, {to_count} to count{}); queue {shown} (+{background} in the background), counting {running}",
        paths.len(),
        if indexed.is_empty() { String::new() } else { format!("{} from the drive index, ", indexed.len()) },
        if refresh { ", refresh" } else { "" }
    );
    if let Some(dir) = dir {
        plan(&app, Rank::Near, upward(&dir));
    }
    out
}

/// Something changed inside these folders, so their sizes and those of every folder
/// containing them are out of date; with `deep`, so are those of every folder inside them
/// (for changes whose exact place isn't known, like an undo or a refresh). They're counted
/// again the next time a tab asks, and the old value stays on screen until then.
// Off the main thread: with thousands of items this can take a moment, and the window
// must keep answering meanwhile.
#[tauri::command(async)]
pub fn folder_sizes_invalidate(app: AppHandle, paths: Vec<String>, deep: bool) {
    let changed: Vec<String> = paths.iter().map(|p| paths::key(p)).collect();
    {
        let mut st = sizes().state.lock();
        st.changed = Some(Instant::now());
        for (key, c) in st.cache.iter_mut() {
            if changed.iter().any(|p| contains(key, p) || (deep && contains(p, key))) {
                c.stale = true;
            }
        }
    }
    // The saved copies too, so other windows (and later launches) count them again.
    let ancestors: Vec<String> = paths.iter().flat_map(|p| upward(p)).map(|p| paths::key(&p)).collect();
    let below: Vec<String> = if deep { changed } else { vec![] };
    with_db(&app, |c| db::stale_folder_sizes(c, &ancestors, &below));
}

/// `path` and every folder containing it, innermost first.
fn upward(path: &str) -> Vec<String> {
    let mut out = vec![paths::normalize(path)];
    while let Some(p) = out.last().and_then(|p| paths::parent(p)) {
        out.push(p);
    }
    out
}

/// Is `path` the folder `ancestor` or inside it? (Both are keys.)
fn contains(ancestor: &str, path: &str) -> bool {
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.is_empty() || ancestor.ends_with('\\') || rest.starts_with('\\'))
}

/// Queues background counts of these folders (most urgent first) where they're due: never
/// counted, out of date, or counted over a day ago. Nearby ones go ahead of the rest of the
/// background work, the nearest first.
fn plan(app: &AppHandle, rank: Rank, folders: Vec<String>) {
    let folders: Vec<(String, String)> = folders
        .iter()
        .filter(|p| countable(p) && local(p) && background_allowed(p))
        .map(|p| (paths::key(p), paths::normalize(p)))
        .collect();
    if folders.is_empty() {
        return;
    }
    load_saved(app, folders.iter().map(|(k, _)| k.clone()));
    let s = sizes();
    let now = db::now();
    let mut st = s.state.lock();
    let mut ahead = Vec::new();
    for (key, path) in folders {
        let due = st.cache.get(&key).is_none_or(|c| c.stale || now - c.size.counted_at >= BACKGROUND_STALE_AFTER);
        if !due || st.running.contains_key(&key) {
            continue;
        }
        match st.queue.iter().position(|j| j.key == key) {
            Some(i) if rank == Rank::Near => {
                let mut job = st.queue.remove(i).expect("position is in the queue");
                job.background = job.background.min(rank);
                ahead.push(job);
            }
            Some(_) => {}
            None => ahead.push(Job::new(path, key, rank)),
        }
    }
    if ahead.is_empty() {
        return;
    }
    if rank == Rank::Near {
        for job in ahead.into_iter().rev() {
            st.queue.push_front(job);
        }
    } else {
        st.queue.extend(ahead);
    }
    report(app, &st);
    drop(st);
    s.wake.notify_all();
}

/// Every ten minutes: the home folder, the drives and the running WSL distros, counted in the
/// background where due (after the folders around what's on screen).
fn planner(app: AppHandle) {
    // Tests count their own folders only (see `background_allowed`).
    if crate::integration::e2e() {
        return;
    }
    // Not while the window starts and counts what it shows.
    std::thread::sleep(Duration::from_secs(30));
    loop {
        let mut roots: Vec<String> = std::env::var("USERPROFILE").into_iter().collect();
        roots.extend(crate::search::walk::fixed_drives());
        for distro in crate::wsl::running() {
            let root = paths::wsl_root(&distro);
            roots.push(format!("{root}\\home"));
            roots.push(root);
        }
        plan(&app, Rank::Fill, roots);
        std::thread::sleep(PLAN_EVERY);
    }
}

/// On this computer: a fixed drive or a WSL distro (not a network share or a removable drive).
fn local(path: &str) -> bool {
    static FIXED: OnceLock<Vec<String>> = OnceLock::new();
    let key = paths::key(path);
    paths::is_wsl(&key)
        || FIXED
            .get_or_init(|| crate::search::walk::fixed_drives().iter().map(|d| paths::key(d)).collect())
            .iter()
            .any(|d| key.starts_with(d.as_str()))
}

/// Sizes from the drive index service for these folders (key, path): those on a drive it reads
/// (fixed NTFS drives, from their file table: search/sizes.rs) and that this user may open, all
/// at once, as current as the drive's change journal. The rest are counted here.
fn from_drive_index(folders: &[(String, String)]) -> HashMap<String, FolderSize> {
    let on_drives: Vec<&(String, String)> = folders.iter().filter(|(k, p)| !paths::is_wsl(k) && local(p)).collect();
    if on_drives.is_empty() {
        return HashMap::new();
    }
    let asked: Vec<String> = on_drives.iter().map(|(_, p)| p.clone()).collect();
    let changed = sizes().state.lock().changed;
    if let Some(wait) = changed.and_then(|t| JOURNAL_LAG.checked_sub(t.elapsed())) {
        std::thread::sleep(wait);
    }
    let Some(sizes) = crate::search::service_folder_sizes(&asked) else { return HashMap::new() };
    let now = db::now();
    on_drives
        .into_iter()
        .zip(sizes)
        .filter_map(|((key, path), s)| {
            let s = s?;
            Some((key.clone(), FolderSize { path: path.clone(), bytes: s.bytes, files: Some(s.files), partial: s.partial, done: true, refreshing: false, counted_at: now }))
        })
        .collect()
}

/// Background counting goes anywhere, except in the end-to-end tests: only inside the folders in
/// FERRET_SIZES_WITHIN (separated by `;`), so a test never walks anyone's own folders.
fn background_allowed(path: &str) -> bool {
    static WITHIN: OnceLock<Option<Vec<String>>> = OnceLock::new();
    let within = WITHIN.get_or_init(|| {
        crate::integration::e2e().then(|| {
            let list = std::env::var("FERRET_SIZES_WITHIN").unwrap_or_default();
            list.split(';').map(str::trim).filter(|s| !s.is_empty()).map(paths::key).collect()
        })
    });
    let key = paths::key(path);
    within.as_ref().is_none_or(|w| w.iter().any(|root| contains(root, &key)))
}

/// Where in the queue the next job for a worker (WSL or not) is: the most urgent, first come
/// first. Background work waits while a folder containing it, or one inside it, is counted: it
/// would walk the same folders (and can use that size once it's done).
fn pick(st: &Shared, wsl: bool) -> Option<usize> {
    let mut best: Option<(Rank, usize)> = None;
    for (i, job) in st.queue.iter().enumerate() {
        let rank = job.rank();
        if job.wsl != wsl || best.is_some_and(|(b, _)| b <= rank) {
            continue;
        }
        if rank != Rank::Shown && st.running.keys().any(|k| contains(k, &job.key) || contains(&job.key, k)) {
            continue;
        }
        best = Some((rank, i));
        if rank == Rank::Shown {
            break;
        }
    }
    best.map(|(_, i)| i)
}

/// Background counts step aside for folders on screen that wait for a worker: they're asked to
/// stop, and are queued again to go on later.
fn make_way(st: &Shared) {
    let waiting = |wsl: bool| st.queue.iter().filter(|j| j.wsl == wsl && !j.wanted_by.is_empty()).count();
    let background: Vec<&Running> = st.running.values().filter(|r| r.who.is_empty()).collect();
    let stopping = background.iter().filter(|r| !r.wsl && r.stop.load(Ordering::Relaxed)).count();
    let mut windows = waiting(false).saturating_sub(st.idle + stopping);
    let linux = waiting(true) > 0;
    for r in background {
        if r.wsl {
            if linux {
                r.stop.store(true, Ordering::Relaxed);
            }
        } else if windows > 0 && !r.stop.load(Ordering::Relaxed) {
            r.stop.store(true, Ordering::Relaxed);
            windows -= 1;
        }
    }
}

fn start(st: &mut Shared, job: &Job) {
    let r = Running { who: job.wanted_by.clone(), stop: job.stop.clone(), path: job.path.clone(), wsl: job.wsl };
    st.running.insert(job.key.clone(), r);
}

fn worker(app: AppHandle) {
    background_priority();
    let s = sizes();
    loop {
        let job = {
            let mut st = s.state.lock();
            loop {
                // File operations go first: counting walks the same folders and competes for the disk.
                if crate::jobs::busy() {
                    s.wake.wait_for(&mut st, Duration::from_millis(500));
                    continue;
                }
                // WSL folders have a worker of their own (wsl_worker).
                if let Some(i) = pick(&st, false) {
                    let job = st.queue.remove(i).expect("pick is in the queue");
                    start(&mut st, &job);
                    report(&app, &st);
                    break job;
                }
                st.idle += 1;
                s.wake.wait(&mut st);
                st.idle -= 1;
            }
        };
        // On a drive the indexer service reads: its size, at once.
        if let Some(size) = from_drive_index(&[(job.key.clone(), job.path.clone())]).remove(&job.key) {
            finish(&app, &job, Some(size), None);
            continue;
        }
        let started = Instant::now();
        let reuse = reusable(&app, &job);
        let mut found = Vec::new();
        let result = count(
            &job.path,
            db::now(),
            || job.stop.load(Ordering::Relaxed) || crate::jobs::busy(),
            |inside| if reuse.is_empty() { None } else { reuse.get(&paths::key(inside)).cloned() },
            |progress| {
                let _ = app.emit("folder-size", progress);
            },
            |done| {
                found.push(done);
                if found.len() >= LEARN_EVERY {
                    learn(&app, Some(&job.path), std::mem::take(&mut found));
                }
            },
        );
        learn(&app, Some(&job.path), found);
        match result {
            Some(size) => finish(&app, &job, Some(size), Some(started.elapsed())),
            // Stopped for a folder on screen or a file operation: goes on later.
            None => requeue(&app, job),
        }
    }
}

/// Recent sizes of folders inside the job's folder that its count uses instead of walking them
/// again (the outermost ones), by key: for a folder on screen, sizes from the last ten minutes;
/// in the background, from the last day. None for a refresh.
fn reusable(app: &AppHandle, job: &Job) -> HashMap<String, FolderSize> {
    if job.refresh {
        return HashMap::new();
    }
    let since = db::now() - if job.wanted_by.is_empty() { BACKGROUND_STALE_AFTER } else { STALE_AFTER };
    let rows = with_db(app, |c| db::recent_folder_sizes_inside(c, &job.key, since)).unwrap_or_default();
    rows.into_iter().map(|(k, v)| (k, from_saved(v))).collect()
}

/// Kept from a count of `root`: the folders two levels down, and bigger ones further down
/// (1 MB or 1,000 files and up). Small ones deep inside are quick to count when opened, and
/// there are many (`node_modules`: four out of five folders of a drive).
fn worth_keeping(root: &str, size: &FolderSize) -> bool {
    let inside = size.path.get(root.len()..).unwrap_or_default().trim_start_matches('\\');
    inside.split('\\').count() <= 2 || size.bytes >= KEEP_BYTES || size.files.is_some_and(|f| f >= KEEP_FILES)
}

/// Sizes of the folders inside one being counted (`root`), found on the way: kept and saved like
/// counted ones (those worth keeping), so opening one of those folders shows its folders' sizes
/// at once. Any of them waiting to be counted is answered now. Saved a bundle at a time. Without
/// `root`, all are kept (a WSL count reports only those worth keeping).
fn learn(app: &AppHandle, root: Option<&str>, mut found: Vec<FolderSize>) {
    if found.is_empty() {
        return;
    }
    {
        let mut st = sizes().state.lock();
        let queued: HashSet<&str> = st.queue.iter().map(|j| j.key.as_str()).collect();
        let mut answered = HashSet::new();
        let mut seen = Vec::new();
        let mut keep = Vec::with_capacity(found.len());
        for size in &found {
            let key = paths::key(&size.path);
            let waiting = queued.contains(key.as_str());
            let shown = waiting || st.cache.contains_key(&key);
            keep.push(shown || root.is_none_or(|r| worth_keeping(r, size)));
            if waiting {
                answered.insert(key.clone());
            }
            if shown {
                seen.push((key, size.clone()));
            }
        }
        let mut kept = keep.into_iter();
        found.retain(|_| kept.next().unwrap_or(false));
        // Only what a tab has seen (or waits for) is kept in memory; the rest is in the database.
        for (key, size) in seen {
            let _ = app.emit("folder-size", size.clone());
            st.cache.insert(key, Cached { size, stale: false });
        }
        if !answered.is_empty() {
            st.queue.retain(|j| !answered.contains(&j.key));
        }
        report(app, &st);
    }
    // In bundles, so other uses of the database don't wait for all of them.
    for chunk in found.chunks(1000) {
        with_db(app, |c| {
            let tx = c.unchecked_transaction()?;
            for s in chunk {
                db::put_folder_size(&tx, &paths::key(&s.path), &to_saved(s))?;
            }
            tx.commit()
        });
    }
}

/// A count is over: the size is kept (here and saved for other windows) and reported. One that
/// couldn't be counted stops showing as refreshing. Slow counts and failures are logged (fast
/// ones would flood the log), unless `took` is None (a WSL batch logs itself as a whole).
fn finish(app: &AppHandle, job: &Job, result: Option<FolderSize>, took: Option<Duration>) {
    let mut st = sizes().state.lock();
    st.running.remove(&job.key);
    report(app, &st);
    if let Some(took) = took.filter(|t| *t >= Duration::from_millis(500) || result.is_none()) {
        log::info!(
            "folder size {}{}: {} in {took:?}; {} still queued",
            job.path,
            if job.wanted_by.is_empty() { " (in the background)" } else { "" },
            match &result { Some(r) => format!("{} files", r.files.unwrap_or(0)), None => "failed".into() },
            st.queue.len()
        );
    }
    let Some(mut size) = result else {
        if let Some(c) = st.cache.get(&job.key) {
            let _ = app.emit("folder-size", FolderSize { refreshing: false, ..c.size.clone() });
        }
        return;
    };
    if size.counted_at == 0 {
        size.counted_at = db::now();
    }
    st.cache.insert(job.key.clone(), Cached { size: size.clone(), stale: false });
    drop(st);
    with_db(app, |c| db::put_folder_size(c, &job.key, &to_saved(&size)));
    let _ = app.emit("folder-size", size);
}

/// `finish` for many counts at once (a WSL batch tells hundreds within milliseconds): one lock,
/// one report and one transaction for all, instead of one each (a commit each, a report sorting
/// all that's running each): 1,500 folders took 1.05 s that way, 0.16 s like this.
fn finish_all(app: &AppHandle, counted: Vec<(&Job, FolderSize)>) {
    {
        let mut st = sizes().state.lock();
        for (job, size) in &counted {
            st.running.remove(&job.key);
            st.cache.insert(job.key.clone(), Cached { size: size.clone(), stale: false });
        }
        report(app, &st);
    }
    with_db(app, |c| {
        let tx = c.unchecked_transaction()?;
        for (job, size) in &counted {
            db::put_folder_size(&tx, &job.key, &to_saved(size))?;
        }
        tx.commit()
    });
    for (_, size) in counted {
        let _ = app.emit("folder-size", size);
    }
}

/// A count that stopped (to make way, or for a file operation) is queued again, first in line for
/// its rank: it goes on from where it got, using the sizes it found meanwhile.
fn requeue(app: &AppHandle, job: Job) {
    let s = sizes();
    let mut st = s.state.lock();
    let who = st.running.remove(&job.key).map(|r| r.who).unwrap_or_default();
    st.queue.push_front(Job { wanted_by: who, stop: Arc::default(), ..job });
    report(app, &st);
    drop(st);
    s.wake.notify_all();
}

/// Adds the total of a folder inside to the one containing it.
fn add(to: &mut FolderSize, part: &FolderSize) {
    to.bytes += part.bytes;
    to.files = to.files.zip(part.files).map(|(a, b)| a + b);
    to.partial |= part.partial;
    to.counted_at = to.counted_at.min(part.counted_at);
}

/// Total size of the files under `path` (none is read: directory listings carry each file's
/// size). Each folder inside is reported to `found` as soon as it's done, with its own total; one
/// with a size in `recent` isn't walked, that size is used. Links and junctions aren't followed:
/// what they point to is counted where it lives (and can't loop). None once `stop` says so.
fn count(
    path: &str,
    counted_at: i64,
    stop: impl Fn() -> bool,
    mut recent: impl FnMut(&str) -> Option<FolderSize>,
    mut progress: impl FnMut(FolderSize),
    mut found: impl FnMut(FolderSize),
) -> Option<FolderSize> {
    // A folder being walked: what's left to list, and its total so far.
    struct Open {
        entries: std::fs::ReadDir,
        size: FolderSize,
    }
    let blank = |p: String| FolderSize { path: p, bytes: 0, files: Some(0), partial: false, done: true, refreshing: false, counted_at };
    let mut root = blank(path.to_string());
    let entries = match std::fs::read_dir(paths::for_io(path)) {
        Ok(entries) => entries,
        Err(_) => {
            root.partial = true;
            return Some(root);
        }
    };
    let mut open = vec![Open { entries, size: root }];
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut last = Instant::now();
    loop {
        let top = open.last_mut().expect("the folder being counted is open");
        let Some(entry) = top.entries.next() else {
            let done = open.pop().expect("an open folder").size;
            let Some(parent) = open.last_mut() else { return Some(done) };
            add(&mut parent.size, &done);
            found(done);
            continue;
        };
        let Ok(entry) = entry else {
            top.size.partial = true;
            continue;
        };
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let shown = crate::listing::join(&top.size.path, &entry.file_name().to_string_lossy());
            if let Some(known) = recent(&shown) {
                bytes += known.bytes;
                files += known.files.unwrap_or(0);
                add(&mut top.size, &known);
                continue;
            }
            if stop() {
                return None;
            }
            match std::fs::read_dir(entry.path()) {
                Ok(entries) => open.push(Open { entries, size: blank(shown) }),
                Err(_) => {
                    top.size.partial = true;
                    found(FolderSize { partial: true, ..blank(shown) });
                }
            }
        } else if !kind.is_symlink() {
            if let Ok(m) = entry.metadata() {
                top.size.bytes += m.len();
                top.size.files = top.size.files.map(|n| n + 1);
                bytes += m.len();
                files += 1;
            }
        }
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(FolderSize { bytes, files: Some(files), done: false, ..blank(path.to_string()) });
        }
    }
}

/// A folder counted the way Ferret counts one, start to end (to compare with: the indexer's
/// `--sizes-bench`).
pub(crate) fn count_folder(path: &str) -> Option<FolderSize> {
    count(path, db::now(), || false, |_| None, |_| {}, |_| {})
}

/// The same, with every folder inside it too (each as its count finished).
pub(crate) fn count_folder_all(path: &str) -> (Option<FolderSize>, Vec<FolderSize>) {
    let mut inside = Vec::new();
    let top = count(path, db::now(), || false, |_| None, |_| {}, |s| inside.push(s));
    (top, inside)
}

/// Most WSL folders one `wsl.exe` counts with find, and the room their paths may take on its
/// command line. The helper reads them from stdin: any number, but no more than this at once.
const WSL_BATCH: usize = 500;
const WSL_ARGS_CHARS: usize = 24_000;
const HELPER_BATCH: usize = 5_000;
/// How many folders the helper reads at once for a count in the background (on screen: its own
/// 64). Linux's disk priority does nothing in WSL (no I/O scheduler there), so a background count
/// with 64 would crowd the disk for whatever else runs in the distro; 8 still counted 1M files
/// in 3.7 s with the cache cleared (find: 27 s).
const HELPER_BACKGROUND_THREADS: &str = "8";
/// Most folders inside that a WSL count skips for their recent sizes (the biggest): find checks
/// every folder it meets against each of them; the helper looks them up.
const SKIP_MAX: usize = 300;
const HELPER_SKIP_MAX: usize = 20_000;

/// Ferret's Linux program for counting WSL folders (src-tauri/wsl-size), built and embedded with
/// the app (build.rs; empty if it was built without it). It reads many folders at once, where
/// find reads one after another: with Linux's cache cleared, 1M files took 1.3 s instead of 27 s
/// (cached: 0.27 s instead of 3 s). Linked statically, so it runs in any distro.
///
/// Each distro gets its own copy, in the user's cache folder there, the first time it's needed
/// (as VS Code puts its server in ~/.vscode-server). Run from the Windows drive instead, it would
/// depend on how the distro mounts it, and on Linux seeing the AppData Ferret writes to: a
/// program started from a packaged (Store) app writes to that app's own copy of AppData.
const WSL_HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ferret-wsl-size"));

/// How Linux starts the helper (its name: $1), at the lowest priority (as the find script runs),
/// once its SHA-256 is the one Ferret built ($2; "-": not checked). 98: it isn't in this distro
/// yet; 99: it isn't the one Ferret put there (damaged, or replaced). The check can't stop someone
/// who already runs as this Linux user (they could change the copy right after it), but the
/// helper runs with no more rights than theirs, and what it reports is only taken as sizes.
const WSL_HELPER_START: &str = r#"f="${XDG_CACHE_HOME:-$HOME/.cache}/ferret/$1"; want=$2; shift 2; [ -x "$f" ] || exit 98; if [ "$want" != - ] && command -v sha256sum >/dev/null 2>&1; then s=$(sha256sum "$f") && [ "${s%% *}" = "$want" ] || exit 99; fi; command -v ionice >/dev/null 2>&1 && ionice -c 3 -p $$ >/dev/null 2>&1; command -v nice >/dev/null 2>&1 && exec nice -n 19 "$f" "$@"; exec "$f" "$@""#;

/// How it gets there: from stdin, under its name ($1), once all its bytes ($2) are in. Copies of
/// other versions go a month after they came (another Ferret, the installed one next to a
/// development build, may still use one).
const WSL_HELPER_PLACE: &str = r#"set -e; d="${XDG_CACHE_HOME:-$HOME/.cache}/ferret"; mkdir -p "$d"; t=$(mktemp "$d/.part.XXXXXX"); cat > "$t"; [ $(($(wc -c < "$t"))) -eq "$2" ] || { rm -f "$t"; exit 1; }; chmod 755 "$t"; mv -f "$t" "$d/$1"; find "$d" -maxdepth 1 -name 'ferret-wsl-size-*' ! -name "$1" -mtime +30 -exec rm -f {} + 2>/dev/null || true"#;

/// The helper's SHA-256 (build.rs).
const WSL_HELPER_SHA256: &str = env!("FERRET_WSL_HELPER_SHA256");

struct Helper {
    /// Its name in each distro: from its hash, so each version has its own.
    name: String,
    sha256: String,
    bytes: &'static [u8],
}

fn wsl_helper() -> Option<&'static Helper> {
    static HELPER: OnceLock<Option<Helper>> = OnceLock::new();
    HELPER
        .get_or_init(|| {
            // Tests: "off" counts with find; "broken" with a helper that can't (for the fallback).
            #[cfg(debug_assertions)]
            match std::env::var("FERRET_WSL_HELPER").as_deref() {
                Ok("off") => return None,
                Ok("broken") => {
                    return Some(Helper { name: "ferret-wsl-size-broken".into(), sha256: "-".into(), bytes: &b"exit 42\n"[..] });
                }
                _ => {}
            }
            if WSL_HELPER.is_empty() {
                return None;
            }
            let name = format!("ferret-wsl-size-{}", &WSL_HELPER_SHA256[..16]);
            Some(Helper { name, sha256: WSL_HELPER_SHA256.to_string(), bytes: WSL_HELPER })
        })
        .as_ref()
}

/// Puts the helper in a distro (`WSL_HELPER_PLACE`).
fn place_wsl_helper(distro: &str, name: &str, bytes: &[u8]) -> bool {
    use std::io::Write;
    use std::process::Stdio;
    let spawned = crate::wsl::command(distro, "sh")
        .args(["-c", WSL_HELPER_PLACE, "sh", name, &bytes.len().to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = spawned else { return false };
    let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(bytes).is_ok());
    match child.wait_with_output() {
        Ok(out) if wrote && out.status.success() => true,
        out => {
            let said = out.map(|o| String::from_utf8_lossy(&o.stderr).trim().to_string()).unwrap_or_default();
            log::warn!("folder sizes in WSL ({distro}): couldn't put the counting helper there ({said})");
            false
        }
    }
}

/// The helper in each distro, this run: put there (at first, or after it was found changed: 98,
/// 99), or not starting there (an ARM distro, a home mounted noexec…: counted with find).
#[derive(Clone, Copy, PartialEq)]
enum HelperIn {
    Placed,
    Failed,
}

fn helper_in() -> &'static Mutex<HashMap<String, HelperIn>> {
    static DISTROS: OnceLock<Mutex<HashMap<String, HelperIn>>> = OnceLock::new();
    DISTROS.get_or_init(Mutex::default)
}

/// Is this distro counted with the helper (so far)?
fn uses_helper(distro: &str) -> bool {
    wsl_helper().is_some() && helper_in().lock().get(distro) != Some(&HelperIn::Failed)
}

/// What Linux runs to count a batch of folders (its arguments). `find` walks them in order,
/// staying on each one's file system (Windows drives under /mnt aren't counted), skipping the
/// folders in SKIP (sizes known), and listing regular files (sizes counted, as on Windows) and
/// folders. `awk` adds them up: a folder is done once find moves past it, and is reported then
/// ("S", bytes, files, the folder counted, the one inside) if it's worth keeping (two levels
/// down, or KEEP_BYTES / KEEP_FILES and up: see `worth_keeping`), then each folder counted as a
/// whole ("D", bytes, files, folder), and every 5,000 files the running total ("P"). All at the
/// lowest CPU and disk priority. Without GNU find (BusyBox), `du` gives the size alone.
const WSL_COUNT: &str = r#"command -v ionice >/dev/null 2>&1 && ionice -c 3 -p $$ >/dev/null 2>&1
if find / -maxdepth 0 -printf '' >/dev/null 2>&1; then
  find "$@" -xdev SKIP \( -type f -printf '%H\t%s\t%P\n' \) -o \( -type d -printf '%H\t-1\t%P\n' \) | awk -F '\t' '
    function close_dir() {
      if (sp > 1) {
        if (depth[sp] <= 2 || b[sp] >= 1048576 || n[sp] >= 1000) printf "S\t%.0f\t%.0f\t%s\t%s\n", b[sp], n[sp], cur, rel[sp]
        if (sp == 2) fflush()
        b[sp - 1] += b[sp]; n[sp - 1] += n[sp]
      }
      sp--
    }
    function done() {
      if (cur == "") return
      while (sp > 1) close_dir()
      printf "D\t%.0f\t%.0f\t%s\n", b[1], n[1], cur; fflush()
    }
    $1 != cur { done(); cur = $1; sp = 1; b[1] = 0; n[1] = 0; depth[1] = 0; files = 0 }
    {
      r = substr($0, length($1) + length($2) + 3)
      if (r == "") next
      d = split(r, part, "/")
      while (depth[sp] >= d) close_dir()
      if ($2 == -1) { sp++; rel[sp] = r; depth[sp] = d; b[sp] = 0; n[sp] = 0; next }
      b[sp] += $2; n[sp]++; files++
      if (files % 5000 == 0) { t = 0; for (i = 1; i <= sp; i++) t += b[i]; printf "P\t%.0f\t%.0f\t%s\n", t, files, cur; fflush() }
    }
    END { done() }'
else
  for p in "$@"; do du -s -k -x -- "$p" | { read -r k rest; printf 'D\t%s\t-1\t%s\n' "$((k * 1024))" "$p"; }; done
fi
"#;

/// The script with these Linux folders skipped.
fn wsl_script(skip: &[&str]) -> String {
    let prune = if skip.is_empty() {
        String::new()
    } else {
        // A pattern for find (its own wildcards escaped), quoted for the shell.
        let pattern = |p: &str| {
            let mut escaped = String::with_capacity(p.len() + 2);
            for c in p.chars() {
                if matches!(c, '\\' | '*' | '?' | '[') {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            format!("'{}'", escaped.replace('\'', r"'\''"))
        };
        let tests: Vec<String> = skip.iter().map(|p| format!("-path {}", pattern(p))).collect();
        format!(r"\( -type d \( {} \) -prune \) -o", tests.join(" -o "))
    };
    WSL_COUNT.replacen("SKIP", &prune, 1)
}

/// `/home/me` → `/home`, `/home` → `/`.
fn linux_parent(p: &str) -> Option<&str> {
    if p == "/" {
        return None;
    }
    match p.rfind('/') {
        Some(0) => Some("/"),
        Some(i) => Some(&p[..i]),
        None => None,
    }
}

/// The folder find complains about ("find: '/root': Permission denied").
fn complaint_path(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("find: ")?;
    let (rest, close) = if let Some(r) = rest.strip_prefix('\'') {
        (r, "': ")
    } else if let Some(r) = rest.strip_prefix('\u{2018}') {
        (r, "\u{2019}: ")
    } else {
        return None;
    };
    rest.rfind(close).map(|i| &rest[..i])
}

/// Is this distro running? Asked at most once a minute (starting `wsl.exe` takes ~100 ms).
fn distro_running(distro: &str) -> bool {
    static SEEN: OnceLock<Mutex<(Option<Instant>, Vec<String>)>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Mutex::default).lock();
    if seen.0.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        *seen = (Some(Instant::now()), crate::wsl::running());
    }
    seen.1.iter().any(|d| d.eq_ignore_ascii_case(distro))
}

/// Counts WSL folders: those waiting in one distro, at once (see `count_wsl`), on its own so
/// Linux counting and Windows counting don't wait for each other.
fn wsl_worker(app: AppHandle) {
    let s = sizes();
    let distro_of = |p: &str| crate::wsl::linux_path(p).map(|(d, _)| d);
    loop {
        let (distro, batch) = {
            let mut st = s.state.lock();
            loop {
                if let Some(first) = pick(&st, true) {
                    let distro = distro_of(&st.queue[first].path).unwrap_or_default();
                    let rank = st.queue[first].rank();
                    let (most, room) = if uses_helper(&distro) { (HELPER_BATCH, usize::MAX) } else { (WSL_BATCH, WSL_ARGS_CHARS) };
                    let (mut batch, mut chars, mut i): (Vec<Job>, usize, usize) = (Vec::new(), 0, first);
                    while i < st.queue.len() && batch.len() < most && chars < room {
                        // One distro, one rank (a folder in the background would keep those on
                        // screen waiting), and never a folder together with one containing it.
                        let j = &st.queue[i];
                        let fits = j.wsl
                            && j.rank() == rank
                            && distro_of(&j.path).as_deref() == Some(distro.as_str())
                            && !batch.iter().any(|b| contains(&b.key, &j.key) || contains(&j.key, &b.key));
                        if !fits {
                            i += 1;
                            continue;
                        }
                        let job = st.queue.remove(i).expect("index is in the queue");
                        chars += job.path.len() + 3;
                        start(&mut st, &job);
                        batch.push(job);
                    }
                    // `find` walks them one after the other, so the smallest go first (by their
                    // last size; never counted ones first too): one big folder, like /home, would
                    // otherwise keep every other size waiting until it's done.
                    batch.sort_by_key(|j: &Job| st.cache.get(&j.key).map_or(0, |c| c.size.bytes));
                    report(&app, &st);
                    break (distro, batch);
                }
                s.wake.wait(&mut st);
            }
        };
        // Background counting never starts a distro (`wsl.exe -d` would): the folders of one
        // that stopped since are dropped.
        if batch.iter().all(|j| j.wanted_by.is_empty()) && !distro_running(&distro) {
            let mut st = s.state.lock();
            for job in &batch {
                st.running.remove(&job.key);
            }
            report(&app, &st);
            log::info!("folder sizes in WSL ({distro}): not running, {} folders left uncounted", batch.len());
            continue;
        }
        count_wsl(&app, &distro, batch);
    }
}

/// Is this a folder inside the one counted, as a count tells it (`a/b`)? What Linux reports is
/// only data: nothing in it may name a folder elsewhere (`..`, `/etc`). And Windows can't show a
/// name with a backslash in it.
fn inside_path(rel: &str) -> bool {
    !rel.contains('\\') && rel.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Marks a folder (and each one containing it) as not read through.
fn unreadable_with_parents(set: &Mutex<HashSet<String>>, path: &str) {
    let mut set = set.lock();
    let mut cur = Some(path);
    while let Some(c) = cur {
        if !set.insert(c.to_string()) {
            break;
        }
        cur = linux_parent(c);
    }
}

/// Counts a batch of folders in one WSL distro the way Linux would (`WSL_HELPER`, or find:
/// `WSL_COUNT`), reporting each as soon as it's done, and the folders inside them. One
/// `wsl.exe` for all of them: starting one takes about 150–200 ms, longer than Linux needs for
/// most folders (40 folders: 4.4 s one at a time, 0.12 s together). Ending `wsl.exe` ends the
/// count: a batch in the background stops when a folder on screen waits, and is queued again.
fn count_wsl(app: &AppHandle, distro: &str, batch: Vec<Job>) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    let started = Instant::now();
    let counted_at = db::now();
    let background = batch.iter().all(|j| j.wanted_by.is_empty());
    // Each folder as Linux sees it. One with a tab or line break in its name couldn't be told
    // apart in the report, so it gets no size.
    let linux: Vec<Option<String>> = batch
        .iter()
        .map(|j| crate::wsl::linux_path(&j.path).map(|(_, l)| l).filter(|l| !l.contains(['\t', '\n'])))
        .collect();
    // The order find walks them in, and where each folder is in the batch.
    let order: Vec<usize> = (0..batch.len()).filter(|&i| linux[i].is_some()).collect();
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &order {
        index.entry(linux[i].as_deref().unwrap_or_default()).or_default().push(i);
    }
    let position: HashMap<usize, usize> = order.iter().enumerate().map(|(pos, &i)| (i, pos)).collect();
    let mut done = vec![false; batch.len()];
    let helper = wsl_helper().filter(|_| uses_helper(distro));

    // Recent sizes of folders inside, used instead of walking those again (find skips them):
    // the biggest ones. Each adds to the folders containing it (`extra`, by Linux folder).
    let mut skip: Vec<(String, FolderSize)> = Vec::new();
    for (i, job) in batch.iter().enumerate() {
        let Some(root) = linux[i].as_deref() else { continue };
        let below = if root == "/" { "/".to_string() } else { format!("{root}/") };
        for (_, size) in reusable(app, job) {
            match crate::wsl::linux_path(&size.path) {
                Some((_, l)) if l.starts_with(&below) && !l.contains(['\t', '\n']) => skip.push((l, size)),
                _ => {}
            }
        }
    }
    skip.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes));
    skip.truncate(if helper.is_some() { HELPER_SKIP_MAX } else { SKIP_MAX });
    let zero = FolderSize { path: String::new(), bytes: 0, files: Some(0), partial: false, done: true, refreshing: false, counted_at };
    let mut extra: HashMap<String, FolderSize> = HashMap::new();
    for (l, size) in &skip {
        let mut cur = l.as_str();
        while let Some(up) = linux_parent(cur) {
            add(extra.entry(up.to_string()).or_insert_with(|| zero.clone()), size);
            if index.contains_key(up) {
                break;
            }
            cur = up;
        }
    }

    let skipped: Vec<&str> = skip.iter().map(|(l, _)| l.as_str()).collect();
    let roots: Vec<&str> = order.iter().map(|&i| linux[i].as_deref().unwrap_or_default()).collect();
    let mut command = if let Some(h) = helper {
        let mut c = crate::wsl::command(distro, "sh");
        c.args(["-c", WSL_HELPER_START, "sh", &h.name, &h.sha256, "--watch-stdin", "--roots-stdin"]);
        c.args(["--keep", &KEEP_BYTES.to_string(), &KEEP_FILES.to_string()]);
        if background {
            c.args(["--threads", HELPER_BACKGROUND_THREADS]);
        }
        c
    } else {
        let mut c = crate::wsl::command(distro, "nice");
        c.args(["-n", "19", "sh", "-s", "--"]).args(&roots);
        c
    };
    let spawned = if order.is_empty() {
        None
    } else {
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()
    };
    let Some(mut child) = spawned else {
        for job in &batch {
            finish(app, job, None, None);
        }
        log::warn!("folder sizes in WSL ({distro}): couldn't start counting {} folders", batch.len());
        return;
    };
    // The script for find; for the helper, the folders to count and then those it skips, each
    // list up to an empty line. The helper ends when its input closes, so that's kept open until
    // the count is over.
    let mut stdin = child.stdin.take();
    if let Some(s) = stdin.as_mut() {
        let list = |paths: &[&str]| paths.iter().map(|p| format!("{p}\n")).collect::<String>() + "\n";
        let input = match helper {
            Some(_) => list(&roots) + &list(&skipped),
            None => wsl_script(&skipped),
        };
        let _ = s.write_all(input.as_bytes());
    }
    if helper.is_none() {
        stdin = None;
    }
    // Lines as text: a name that isn't UTF-8 can't stop the reading.
    fn lines(from: impl std::io::Read) -> impl Iterator<Item = String> {
        BufReader::new(from).split(b'\n').map_while(Result::ok).map(|l| String::from_utf8_lossy(&l).into_owned())
    }
    // Folders that couldn't be read (and every folder containing one), from find's complaints:
    // read on their own (so the pipe can't fill up) and as they come, so a folder's are usually
    // in before its size. (The helper tells them in order with the sizes.) Anything else it says
    // is kept for the log.
    let unreadable = Arc::new(Mutex::new(HashSet::<String>::new()));
    let err_thread = child.stderr.take().map(|e| {
        let unreadable = unreadable.clone();
        std::thread::spawn(move || {
            let mut other = Vec::new();
            for line in lines(e) {
                match complaint_path(&line) {
                    Some(p) => unreadable_with_parents(&unreadable, p),
                    None if other.len() < 3 => other.push(line),
                    None => {}
                }
            }
            other
        })
    });
    let (tx, rx) = channel::<String>();
    if let Some(out) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in lines(out) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    // A folder's size as reported, plus what the skipped folders inside add.
    let size = |linux_dir: &str, path: String, bytes: u64, files: Option<u64>, done: bool| {
        let partial = done && unreadable.lock().contains(linux_dir);
        let mut s = FolderSize { path, bytes, files, partial, done, refreshing: false, counted_at };
        if let Some(e) = extra.get(linux_dir) {
            add(&mut s, e);
        }
        s
    };
    let inside = |root: &str, rel: &str| if root == "/" { format!("/{rel}") } else { format!("{root}/{rel}") };

    let mut next = 0; // in `order`: the first folder not reported yet
    let mut found: Vec<FolderSize> = Vec::new(); // the folders inside, done
    let mut stopped = false;
    let mut helper_ran = false;
    let mut checked = Instant::now();
    loop {
        // Making way for a folder on screen: stopped, to go on later.
        if checked.elapsed() >= Duration::from_millis(100) {
            checked = Instant::now();
            let left: Vec<&Job> = batch.iter().zip(&done).filter(|(_, d)| !**d).map(|(j, _)| j).collect();
            if !left.is_empty() && left.iter().all(|j| j.stop.load(Ordering::Relaxed)) {
                let _ = child.kill();
                stopped = true;
                break;
            }
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            // With what came meanwhile: the folders done are finished together (`finish_all`).
            Ok(first) => {
                let mut counted: Vec<(&Job, FolderSize)> = Vec::new();
                for line in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())).take(5000) {
                    if line.starts_with("H\t") {
                        helper_ran = true;
                        continue;
                    }
                    if let Some(p) = line.strip_prefix("E\t") {
                        unreadable_with_parents(&unreadable, p);
                        continue;
                    }
                    let mut f = line.splitn(5, '\t');
                    let (Some(kind), Some(Ok(bytes)), Some(Ok(files)), Some(dir)) =
                        (f.next(), f.next().map(str::parse::<u64>), f.next().map(str::parse::<i64>), f.next())
                    else {
                        continue;
                    };
                    let files = u64::try_from(files).ok();
                    let Some(ix) = index.get(dir) else { continue };
                    if kind == "S" {
                        let Some(rel) = f.next().filter(|r| inside_path(r)) else { continue };
                        for &i in ix {
                            let path = crate::listing::join(&batch[i].path, &rel.replace('/', "\\"));
                            found.push(size(&inside(dir, rel), path, bytes, files, true));
                        }
                        if found.len() >= LEARN_EVERY {
                            learn(app, None, std::mem::take(&mut found));
                        }
                        continue;
                    }
                    if kind == "P" {
                        for &i in ix {
                            let _ = app.emit("folder-size", size(dir, batch[i].path.clone(), bytes, files, false));
                        }
                        continue;
                    }
                    // find walks them in order: the folders before this one had no files at all.
                    // (The helper tells each, as it's done.)
                    if helper.is_none() {
                        let pos = position[&ix[0]];
                        while next < pos {
                            let i = order[next];
                            if !done[i] {
                                done[i] = true;
                                let l = linux[i].as_deref().unwrap_or_default();
                                counted.push((&batch[i], size(l, batch[i].path.clone(), 0, Some(0), true)));
                            }
                            next += 1;
                        }
                        next = next.max(pos + 1);
                    }
                    for &i in ix {
                        if !done[i] {
                            done[i] = true;
                            counted.push((&batch[i], size(dir, batch[i].path.clone(), bytes, files, true)));
                        }
                    }
                }
                if !counted.is_empty() {
                    learn(app, None, std::mem::take(&mut found));
                    finish_all(app, counted);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(stdin);
    let status = child.wait();
    let ok = status.as_ref().is_ok_and(|s| s.success()) && !stopped;
    let said = err_thread.and_then(|t| t.join().ok()).unwrap_or_default();
    // The helper isn't in this distro yet, or isn't the one Ferret put there: put there (once a
    // run), and the batch counted again. Or it didn't start: the distro is counted with find from
    // now on, the batch queued again (to be split into the smaller ones find takes).
    if let Some(h) = helper.filter(|_| !helper_ran && !stopped) {
        let code = status.ok().and_then(|s| s.code());
        let placed = helper_in().lock().get(distro) == Some(&HelperIn::Placed);
        if matches!(code, Some(98 | 99)) && !placed && place_wsl_helper(distro, &h.name, h.bytes) {
            let why = if code == Some(99) { ", replacing a copy that wasn't Ferret's" } else { "" };
            log::info!("folder sizes in WSL ({distro}): put the counting helper there ({}{why})", h.name);
            helper_in().lock().insert(distro.to_string(), HelperIn::Placed);
            return count_wsl(app, distro, batch);
        }
        let code = code.map_or("none".to_string(), |c| c.to_string());
        log::warn!("folder sizes in WSL ({distro}): the counting helper didn't start (exit {code}; {said:?}), counting with find");
        helper_in().lock().insert(distro.to_string(), HelperIn::Failed);
        for job in batch {
            requeue(app, job);
        }
        return;
    }
    // What was done before a stop is kept: the count goes on from there.
    learn(app, None, found);
    let total = batch.len();
    // Not reported by the end: nothing in them, unless the count was stopped or failed.
    for (i, job) in batch.into_iter().enumerate() {
        if done[i] {
            continue;
        }
        if stopped {
            requeue(app, job);
        } else {
            let l = linux[i].as_deref().unwrap_or_default();
            finish(app, &job, (ok && linux[i].is_some()).then(|| size(l, job.path.clone(), 0, Some(0), true)), None);
        }
    }
    log::info!(
        "folder sizes in WSL ({distro}): {total} folders in {:?}{}{}{}{}",
        started.elapsed(),
        if helper.is_none() { " with find" } else { "" },
        if background { ", in the background" } else { "" },
        if skipped.is_empty() { String::new() } else { format!(", {} folders inside known already", skipped.len()) },
        if stopped { ", stopped to make way for folders on screen" } else if ok { "" } else { ", failed" }
    );
}

/// Not every folder gets a size: in WSL, Linux's /proc and /sys describe the running system
/// rather than hold files (/proc alone "contains" 128 TB), and /mnt/c is the Windows drive
/// itself, very slow to walk from Linux and counted directly when opened as C:.
fn countable(path: &str) -> bool {
    !paths::is_wsl(path) || crate::wsl::linux_path(path).is_none_or(|(_, linux)| countable_in_linux(&linux))
}

fn countable_in_linux(linux: &str) -> bool {
    let first = |p: &str| p.trim_start_matches('/').split('/').next().unwrap_or("").to_string();
    let top = first(linux);
    let drive = linux.strip_prefix("/mnt/").map(first).is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_alphabetic()));
    top != "proc" && top != "sys" && !drive
}

/// Lower CPU and disk priority for this thread, so counting never slows anything else down.
fn background_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_by_key() {
        assert!(contains(r"c:\users", r"c:\users"));
        assert!(contains(r"c:\users", r"c:\users\me\docs"));
        assert!(!contains(r"c:\users", r"c:\usersx"));
        assert!(contains(r"c:\", r"c:\users"));
        assert!(!contains(r"d:\", r"c:\users"));
    }

    #[test]
    fn skips_linux_folders_that_hold_no_files() {
        for p in ["/proc", "/proc/1/fd", "/sys", "/sys/fs/cgroup", "/mnt/c", "/mnt/d/Users"] {
            assert!(!countable_in_linux(p), "{p}");
        }
        for p in ["/", "/home/me", "/usr", "/process", "/system", "/mnt", "/mnt/wsl", "/mnt/cdrom", "/home/proc"] {
            assert!(countable_in_linux(p), "{p}");
        }
        assert!(!countable(r"\\wsl.localhost\Ubuntu\proc"));
        assert!(!countable(r"\\wsl$\Ubuntu\mnt\c"));
        assert!(countable(r"\\wsl.localhost\Ubuntu\home"));
        assert!(countable(r"C:\proc") && countable(r"C:\mnt\c"));
    }

    #[test]
    fn counts_every_folder_below_and_uses_recent_sizes() {
        let dir = std::env::temp_dir().join(format!("ferret-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::create_dir_all(dir.join("c")).unwrap();
        std::fs::write(dir.join("one.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("a").join("two.bin"), vec![0u8; 2000]).unwrap();
        std::fs::write(dir.join("a").join("b").join("three.bin"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.join("c").join("four.bin"), vec![0u8; 4000]).unwrap();
        let base = dir.to_string_lossy().to_string();
        let rel = |s: &FolderSize| s.path[base.len()..].trim_start_matches('\\').to_string();

        let mut found = Vec::new();
        let size = count(&base, 7, || false, |_| None, |_| {}, |s| found.push(s)).unwrap();
        assert_eq!((size.bytes, size.files, size.partial, size.done, size.counted_at), (10000, Some(4), false, true, 7));
        // Every folder inside, at any depth, comes with it.
        let mut got: Vec<_> = found.iter().map(|s| (rel(s), s.bytes, s.files)).collect();
        got.sort();
        assert_eq!(got, [("a".into(), 5000, Some(2)), (r"a\b".into(), 3000, Some(1)), ("c".to_string(), 4000, Some(1))]);

        // A recent size of a\b is used instead of walking it again: not reported again, and its
        // age and partial mark carry up.
        let known = FolderSize { path: format!(r"{base}\a\b"), bytes: 99, files: Some(9), partial: true, done: true, refreshing: false, counted_at: 5 };
        let mut found = Vec::new();
        let size = count(&base, 7, || false, |p| p.ends_with(r"\b").then(|| known.clone()), |_| {}, |s| found.push(s)).unwrap();
        assert_eq!((size.bytes, size.files, size.partial, size.counted_at), (7099, Some(12), true, 5));
        let a = found.iter().find(|s| rel(s) == "a").unwrap();
        assert_eq!((a.bytes, a.files, a.counted_at), (2099, Some(10), 5));
        assert!(found.iter().all(|s| rel(s) != r"a\b"));

        assert!(count(&base, 7, || true, |_| None, |_| {}, |_| {}).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_the_folders_near_the_top_and_the_big_ones() {
        let size = |p: &str, bytes: u64, files: u64| FolderSize { path: p.into(), bytes, files: Some(files), partial: false, done: true, refreshing: false, counted_at: 1 };
        assert!(worth_keeping(r"C:\a", &size(r"C:\a\b", 1, 1)));
        assert!(worth_keeping(r"C:\a", &size(r"C:\a\b\c", 1, 1)));
        assert!(!worth_keeping(r"C:\a", &size(r"C:\a\b\c\d", 1, 1)));
        assert!(worth_keeping(r"C:\a", &size(r"C:\a\b\c\d", KEEP_BYTES, 1)));
        assert!(worth_keeping(r"C:\a", &size(r"C:\a\b\c\d", 1, KEEP_FILES)));
        assert!(worth_keeping(r"C:\", &size(r"C:\a\b", 1, 1)) && !worth_keeping(r"C:\", &size(r"C:\a\b\c", 1, 1)));
    }

    fn job(path: &str, rank: Rank, tab: Option<u32>) -> Job {
        let mut j = Job::new(path.into(), paths::key(path), rank);
        j.wanted_by.extend(tab);
        j
    }

    fn running(path: &str, tab: Option<u32>) -> (String, Running) {
        let r = Running { who: tab.into_iter().collect(), stop: Arc::default(), path: path.into(), wsl: paths::is_wsl(path) };
        (paths::key(path), r)
    }

    #[test]
    fn folders_on_screen_go_first_then_nearby_ones_then_the_rest() {
        let mut st = Shared::default();
        st.queue.extend([
            job(r"C:\fill", Rank::Fill, None),
            job(r"C:\near", Rank::Near, None),
            job(r"C:\shown", Rank::Near, Some(1)),
            job(r"\\wsl.localhost\U\shown", Rank::Near, Some(1)),
        ]);
        assert_eq!(pick(&st, false), Some(2));
        assert_eq!(pick(&st, true), Some(3));
        st.queue.remove(2);
        assert_eq!(pick(&st, false), Some(1));
        // Background work waits while a folder containing it is counted; folders on screen don't.
        st.running.extend([running(r"C:\", None)]);
        assert_eq!(pick(&st, false), None);
        st.queue.push_back(job(r"C:\shown2", Rank::Near, Some(2)));
        assert_eq!(pick(&st, false), Some(3));
    }

    #[test]
    fn background_counts_make_way_for_folders_on_screen() {
        let mut st = Shared::default();
        st.running.extend([running(r"C:\big", None), running(r"C:\other", None), running(r"\\wsl.localhost\U\big", None), running(r"C:\shown", Some(1))]);
        let stopped = |st: &Shared| {
            let mut v: Vec<String> = st.running.iter().filter(|(_, r)| r.stop.load(Ordering::Relaxed)).map(|(k, _)| k.clone()).collect();
            v.sort();
            v
        };
        make_way(&st);
        assert!(stopped(&st).is_empty(), "nothing waits");
        st.queue.push_back(job(r"C:\next", Rank::Near, Some(1)));
        st.idle = 1;
        make_way(&st);
        assert!(stopped(&st).is_empty(), "a worker is free for it");
        // One folder waits: one background count stops (not both, not the one on screen, not WSL's).
        st.idle = 0;
        make_way(&st);
        make_way(&st);
        let first = stopped(&st);
        assert!(first.len() == 1 && (first[0] == r"c:\big" || first[0] == r"c:\other"), "{first:?}");
        st.queue.push_back(job(r"\\wsl.localhost\U\next", Rank::Near, Some(1)));
        make_way(&st);
        assert_eq!(stopped(&st).len(), 2);
        assert!(stopped(&st).contains(&r"\\wsl.localhost\U\big".to_string()));
    }

    #[test]
    fn reads_linux_paths_and_complaints() {
        assert_eq!(linux_parent("/home/me"), Some("/home"));
        assert_eq!(linux_parent("/home"), Some("/"));
        assert_eq!(linux_parent("/"), None);
        assert_eq!(complaint_path("find: '/root': Permission denied"), Some("/root"));
        assert_eq!(complaint_path("find: \u{2018}/a/it's\u{2019}: Permission denied"), Some("/a/it's"));
        assert_eq!(complaint_path("find: warning: something"), None);
        // What a count may report as a folder inside.
        assert!(inside_path("a") && inside_path("a/b c/d") && inside_path("t\tab") && inside_path("..a/b."));
        for bad in ["", "/etc", "a/../..", "..", "./a", "a//b", "a/", r"a\b"] {
            assert!(!inside_path(bad), "{bad:?}");
        }
        let script = wsl_script(&["/home/me/a*b", "/home/it's"]);
        assert!(script.contains(r"\( -type d \( -path '/home/me/a\*b' -o -path '/home/it'\''s' \) -prune \) -o"), "{script}");
        assert!(!wsl_script(&[]).contains("SKIP"));
    }

    /// Runs the WSL count in the first running distro (in /tmp): find's script with each awk
    /// installed there, and the helper, which must agree, then what only the helper gets right:
    /// `cargo test --lib wsl_count_in_linux -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn wsl_count_in_linux() {
        use std::io::Write;
        let Some(distro) = crate::wsl::running().into_iter().next() else { return };
        let dir = format!("/tmp/ferret-count-{}", std::process::id());
        let sh = |args: &[&str], input: &str| {
            let mut c = crate::wsl::command(&distro, "sh")
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            c.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let out = c.wait_with_output().unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stdout).to_string())
        };
        // A symlink to a folder (not followed), and a folder that can't be read (no size).
        let setup = r#"set -e; rm -rf "$1" "$1-other" "$1-odd"; mkdir -p "$1/a/b/c" "$1/a/b/big" "$1/a/skip/deep" "$1/e" "$1/t	ab" "$1/locked" "$1-other"; cd "$1"
            head -c 100 /dev/zero > top; head -c 200 /dev/zero > a/f; head -c 300 /dev/zero > a/b/f; head -c 400 /dev/zero > a/b/c/f
            head -c 1048576 /dev/zero > a/b/big/f; head -c 500 /dev/zero > a/skip/deep/f; head -c 600 /dev/zero > "t	ab/f"
            head -c 700 /dev/zero > locked/f; chmod 000 locked; ln -s .. a/up
            head -c 50 /dev/zero > "$1-other/f"
            bad=$(printf 'bad\377'); nl=$(printf 'line\nbreak'); mkdir -p "$1-odd/ok" "$1-odd/$bad" "$1-odd/$nl"
            head -c 10 /dev/zero > "$1-odd/ok/f"; head -c 20 /dev/zero > "$1-odd/$bad/f"; head -c 30 /dev/zero > "$1-odd/$nl/f""#;
        assert_eq!(sh(&["-s", "--", &dir], setup).0, Some(0));
        let other = format!("{dir}-other");
        let sorted = |out: &str| {
            let mut lines: Vec<String> = out.lines().filter(|l| !l.starts_with(['P', 'H', 'E'])).map(str::to_string).collect();
            lines.sort();
            lines
        };
        // a/b/c is small and three levels down: not kept (but counted in a/b and a); a/b/big is as
        // deep, and big.
        let mut want = vec![
            format!("D\t1050176\t6\t{dir}"),
            format!("D\t50\t1\t{other}"),
            format!("S\t0\t0\t{dir}\te"),
            format!("S\t0\t0\t{dir}\tlocked"),
            format!("S\t600\t1\t{dir}\tt\tab"),
            format!("S\t1048576\t1\t{dir}\ta/b/big"),
            format!("S\t1049276\t3\t{dir}\ta/b"),
            format!("S\t1049476\t4\t{dir}\ta"),
        ];
        want.sort();
        for awk in ["awk", "gawk", "mawk"] {
            let script = wsl_script(&[&format!("{dir}/a/skip")]).replace("| awk -F", &format!("| {awk} -F"));
            let (code, out) = sh(&["-s", "--", &dir, &other], &format!("command -v {awk} >/dev/null || exit 3\n{script}"));
            if code == Some(3) {
                println!("{awk}: not installed");
                continue;
            }
            assert_eq!(sorted(&out), want, "{awk}");
            println!("{awk}: ok");
        }

        // The helper, put in the distro under a name of its own (removed at the end).
        let helper = format!("ferret-wsl-size-test-{}", std::process::id());
        let start = |name: &str, input: &str, roots: &[&str]| {
            let mut args = vec!["-c", WSL_HELPER_START, "sh", name, WSL_HELPER_SHA256, "--keep", "1048576", "1000", "--"];
            args.extend(roots);
            sh(&args, input)
        };
        let run = |input: &str, roots: &[&str]| {
            let (code, out) = start(&helper, input, roots);
            assert_eq!(code, Some(0), "{out}");
            assert!(out.starts_with("H\t1\n"), "{out}");
            out
        };
        if WSL_HELPER.is_empty() {
            println!("helper: not built");
        } else {
            assert_eq!(start(&helper, "\n", &[&dir]).0, Some(98), "not there yet");
            // Only once all of it came.
            let (short, _) = sh(&["-c", WSL_HELPER_PLACE, "sh", &helper, "999999999"], "#!/bin/sh\n");
            assert_ne!(short, Some(0));
            assert_eq!(start(&helper, "\n", &[&dir]).0, Some(98), "a short copy isn't kept");
            assert!(place_wsl_helper(&distro, &helper, WSL_HELPER));
            let out = run(&format!("{dir}/a/skip\n\n"), &[&dir, &other]);
            assert_eq!(sorted(&out), want, "helper");
            assert!(out.lines().any(|l| l == format!("E\t{dir}/locked")), "{out}");
            // The folders to count on stdin, as Ferret sends them.
            let args = ["-c", WSL_HELPER_START, "sh", &helper, WSL_HELPER_SHA256, "--roots-stdin", "--keep", "1048576", "1000"];
            let (code, out) = sh(&args, &format!("{dir}\n{other}\n\n{dir}/a/skip\n\n"));
            assert_eq!(code, Some(0), "{out}");
            assert_eq!(sorted(&out), want, "helper, folders on stdin");
            // Names Ferret can't show (not UTF-8, a line break): counted, not reported.
            let odd = format!("{dir}-odd");
            assert_eq!(sorted(&run("\n", &[&odd])), [format!("D\t60\t3\t{odd}"), format!("S\t10\t1\t{odd}\tok")]);
            // Other file systems inside (the Windows drive): listed, not entered.
            let (has_c, _) = sh(&["-c", "[ -d /mnt/c ]"], "");
            if has_c == Some(0) {
                let out = run("\n", &["/mnt"]);
                assert!(out.lines().any(|l| l == "S\t0\t0\t/mnt\tc"), "{out}");
                let files: u64 = out.lines().find_map(|l| l.strip_prefix("D\t")).and_then(|l| l.split('\t').nth(1)?.parse().ok()).unwrap();
                assert!(files < 100, "{out}");
            }
            // A copy that isn't the one Ferret built doesn't run (99).
            sh(&["-c", r#"printf x >> "${XDG_CACHE_HOME:-$HOME/.cache}/ferret/$1""#, "sh", &helper], "");
            assert_eq!(start(&helper, "\n", &[&dir]).0, Some(99), "a changed copy");
            println!("helper: ok");
        }
        let clean = r#"chmod 755 "$1/locked"; rm -rf "$1" "$1-other" "$1-odd" "${XDG_CACHE_HOME:-$HOME/.cache}/ferret/$2""#;
        sh(&["-c", clean, "sh", &dir, &helper], "");
    }
}
