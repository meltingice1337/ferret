//! Folder sizes for the list view. Counted in the background by two low-priority workers
//! (CPU and disk) and saved, so any window shows the last known size at once. A size is
//! counted again (its old value showing meanwhile, marked as refreshing) when it's ten
//! minutes old, when Ferret changed something inside the folder, or on refresh.

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::AppState;
use crate::{db, paths};

/// Milliseconds after which a size is counted again.
const STALE_AFTER: i64 = 10 * 60 * 1000;
const WORKERS: usize = 2;
/// How often a folder that's still being counted reports its running total.
const PROGRESS_EVERY: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub path: String,
    pub bytes: u64,
    /// Not known for WSL folders (counted by `du`).
    pub files: Option<u64>,
    /// Some folders inside couldn't be read.
    pub partial: bool,
    /// False while still counting (`bytes` is the total so far).
    pub done: bool,
    /// An earlier count, being counted again right now.
    pub refreshing: bool,
    /// When this size was counted (Unix milliseconds).
    pub counted_at: i64,
}

struct Cached {
    size: FolderSize,
    /// Something inside changed since it was counted.
    stale: bool,
}

struct Job {
    path: String,
    key: String,
    /// Tabs that want this folder; when none are left, the job is dropped.
    wanted_by: HashSet<u32>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct Shared {
    cache: HashMap<String, Cached>,
    queue: VecDeque<Job>,
    /// Folders being counted right now: who wants them, and how to stop them.
    running: HashMap<String, (HashSet<u32>, Arc<AtomicBool>)>,
    started: bool,
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

/// Sizes of a tab's folders, in display order. Known ones (from this window or saved by any)
/// come back at once; missing ones, then out-of-date ones, are counted in the background and
/// reported as `folder-size` events (also while counting). A newer request from the same tab
/// replaces the work it no longer needs. `refresh` counts everything again (F5).
#[tauri::command]
pub fn folder_sizes(app: AppHandle, requester: u32, paths: Vec<String>, refresh: bool) -> Vec<FolderSize> {
    let s = sizes();
    let wanted: Vec<(String, String)> =
        paths.iter().filter(|p| countable(p)).map(|p| (paths::key(p), paths::normalize(p))).collect();

    // Sizes this window hasn't seen yet may have been saved earlier (by any window).
    let unseen: Vec<String> = {
        let st = s.state.lock();
        wanted.iter().filter(|(k, _)| !st.cache.contains_key(k)).map(|(k, _)| k.clone()).collect()
    };
    if !unseen.is_empty() {
        if let Some(saved) = with_db(&app, |c| db::folder_sizes(c, &unseen)) {
            let mut st = s.state.lock();
            for (key, v) in saved {
                let size = FolderSize {
                    path: v.path, bytes: v.bytes, files: v.files, partial: v.partial, done: true, refreshing: false,
                    counted_at: v.counted_at,
                };
                st.cache.entry(key).or_insert(Cached { size, stale: v.stale });
            }
        }
    }

    let mut st = s.state.lock();
    if !st.started {
        st.started = true;
        for _ in 0..WORKERS {
            let app = app.clone();
            std::thread::spawn(move || worker(app));
        }
    }
    let keys: HashSet<&str> = wanted.iter().map(|(k, _)| k.as_str()).collect();

    // Work this tab asked for earlier but no longer shows: forget it, or stop counting.
    for job in st.queue.iter_mut() {
        if !keys.contains(job.key.as_str()) {
            job.wanted_by.remove(&requester);
        }
    }
    st.queue.retain(|j| !j.wanted_by.is_empty());
    for (key, (who, cancel)) in st.running.iter_mut() {
        if !keys.contains(key.as_str()) && who.remove(&requester) && who.is_empty() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    let now = db::now();
    let mut out = Vec::new();
    let (mut missing, mut outdated) = (Vec::new(), Vec::new());
    for (key, path) in wanted {
        match st.cache.get(&key) {
            Some(c) if !refresh && !c.stale && now - c.size.counted_at < STALE_AFTER => out.push(c.size.clone()),
            Some(c) => {
                out.push(FolderSize { refreshing: true, ..c.size.clone() });
                outdated.push((key, path));
            }
            None => missing.push((key, path)),
        }
    }
    // Folders with no size at all come before refreshing known ones.
    for (key, path) in missing.into_iter().chain(outdated) {
        if let Some((who, _)) = st.running.get_mut(&key) {
            who.insert(requester);
        } else if let Some(job) = st.queue.iter_mut().find(|j| j.key == key) {
            job.wanted_by.insert(requester);
        } else {
            st.queue.push_back(Job { path, key, wanted_by: HashSet::from([requester]), cancel: Arc::default() });
        }
    }
    drop(st);
    s.wake.notify_all();
    out
}

/// Something changed inside these folders, so their sizes and those of every folder
/// containing them are out of date; with `deep`, so are those of every folder inside them
/// (for changes whose exact place isn't known, like an undo or a refresh). They're counted
/// again the next time a tab asks, and the old value stays on screen until then.
#[tauri::command]
pub fn folder_sizes_invalidate(app: AppHandle, paths: Vec<String>, deep: bool) {
    let changed: Vec<String> = paths.iter().map(|p| paths::key(p)).collect();
    {
        let mut st = sizes().state.lock();
        for (key, c) in st.cache.iter_mut() {
            if changed.iter().any(|p| contains(key, p) || (deep && contains(p, key))) {
                c.stale = true;
            }
        }
    }
    // The saved copies too, so other windows (and later launches) count them again.
    let ancestors: Vec<String> = paths.iter().flat_map(|p| ancestor_keys(p)).collect();
    let below: Vec<String> = if deep { changed } else { vec![] };
    with_db(&app, |c| db::stale_folder_sizes(c, &ancestors, &below));
}

/// Keys of `path` and every folder containing it.
fn ancestor_keys(path: &str) -> Vec<String> {
    let mut out = vec![paths::key(path)];
    let mut cur = paths::normalize(path);
    while let Some(p) = paths::parent(&cur) {
        out.push(paths::key(&p));
        cur = p;
    }
    out
}

/// Is `path` the folder `ancestor` or inside it? (Both are keys.)
fn contains(ancestor: &str, path: &str) -> bool {
    path.strip_prefix(ancestor)
        .is_some_and(|rest| rest.is_empty() || ancestor.ends_with('\\') || rest.starts_with('\\'))
}

fn worker(app: AppHandle) {
    background_priority();
    let s = sizes();
    loop {
        let job = {
            let mut st = s.state.lock();
            loop {
                if let Some(job) = st.queue.pop_front() {
                    st.running.insert(job.key.clone(), (job.wanted_by.clone(), job.cancel.clone()));
                    break job;
                }
                s.wake.wait(&mut st);
            }
        };
        let result = if paths::is_wsl(&job.path) {
            count_wsl(&job.path, &job.cancel)
        } else {
            count(&job.path, &job.cancel, |progress| {
                let _ = app.emit("folder-size", progress);
            })
        };
        let mut st = s.state.lock();
        st.running.remove(&job.key);
        let Some(mut size) = result else {
            // Couldn't count it again (not stopped on purpose): the earlier size stops showing as refreshing.
            if !job.cancel.load(Ordering::Relaxed) {
                if let Some(c) = st.cache.get(&job.key) {
                    let _ = app.emit("folder-size", FolderSize { refreshing: false, ..c.size.clone() });
                }
            }
            continue;
        };
        size.counted_at = db::now();
        st.cache.insert(job.key.clone(), Cached { size: size.clone(), stale: false });
        drop(st);
        let saved = db::SavedSize {
            path: size.path.clone(), bytes: size.bytes, files: size.files, partial: size.partial,
            counted_at: size.counted_at, stale: false,
        };
        with_db(&app, |c| db::put_folder_size(c, &job.key, &saved));
        let _ = app.emit("folder-size", size);
    }
}

/// Total size of the files under `path`. Directory listings carry each file's size, so this
/// reads no file. Links and junctions aren't followed: what they point to is counted where
/// it lives (and can't loop). None when cancelled.
fn count(path: &str, cancel: &AtomicBool, mut progress: impl FnMut(FolderSize)) -> Option<FolderSize> {
    let mut size = FolderSize { path: path.to_string(), bytes: 0, files: Some(0), partial: false, done: false, refreshing: false, counted_at: 0 };
    let mut files = 0u64;
    let mut stack = vec![PathBuf::from(paths::for_io(path))];
    let mut last = Instant::now();
    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            size.partial = true;
            continue;
        };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                stack.push(e.path());
            } else if !kind.is_symlink() {
                if let Ok(m) = e.metadata() {
                    size.bytes += m.len();
                    files += 1;
                }
            }
        }
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            size.files = Some(files);
            progress(size.clone());
        }
    }
    size.files = Some(files);
    size.done = true;
    Some(size)
}

/// A WSL folder: Linux's own `du` counts it (at the lowest priority, staying on the folder's
/// file system so mounted Windows drives under /mnt aren't counted). Ending `wsl.exe` also
/// ends `du`, so leaving the folder stops the count. None when cancelled or `du` failed.
fn count_wsl(path: &str, cancel: &AtomicBool) -> Option<FolderSize> {
    use std::io::Read;
    use std::process::Stdio;
    let (distro, linux) = crate::wsl::linux_path(path)?;
    let mut child = crate::wsl::command(&distro, "nice")
        .args(["-n", "19", "du", "-s", "-b", "-x", "--", &linux])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Unreadable folders are reported on stderr (read on its own so the pipe can't fill up).
    let mut stderr = child.stderr.take()?;
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let bytes = out.split('\t').next()?.trim().parse().ok()?;
    let partial = !errors.join().unwrap_or_default().trim().is_empty();
    Some(FolderSize { path: path.to_string(), bytes, files: None, partial, done: true, refreshing: false, counted_at: 0 })
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
    fn counts_files_below_a_folder() {
        let dir = std::env::temp_dir().join(format!("ferret-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("one.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("a").join("two.bin"), vec![0u8; 2000]).unwrap();
        std::fs::write(dir.join("a").join("b").join("three.bin"), vec![0u8; 3000]).unwrap();
        let size = count(&dir.to_string_lossy(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((size.bytes, size.files, size.partial, size.done), (6000, Some(3), false, true));
        assert!(count(&dir.to_string_lossy(), &AtomicBool::new(true), |_| {}).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
