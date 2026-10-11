//! Folder sizes for the list view. Counted in the background by two low-priority workers
//! (CPU and disk), kept for the session, and counted again when Ferret changes something
//! inside a folder, on refresh, or once a value is ten minutes old (the old value stays on
//! screen meanwhile).

use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use crate::paths;

const STALE_AFTER: Duration = Duration::from_secs(600);
const WORKERS: usize = 2;
/// How often a folder that's still being counted reports its running total.
const PROGRESS_EVERY: Duration = Duration::from_millis(300);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSize {
    pub path: String,
    pub bytes: u64,
    pub files: u64,
    /// Some folders inside couldn't be read.
    pub partial: bool,
    /// False while still counting (`bytes` is the total so far).
    pub done: bool,
}

struct Cached {
    size: FolderSize,
    at: Instant,
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

/// Sizes of a tab's folders, in display order. Known ones come back at once; missing or
/// out-of-date ones are counted in the background and reported as `folder-size` events
/// (also while counting). A newer request from the same tab replaces the work it no longer
/// needs. `refresh` counts everything again (F5).
#[tauri::command]
pub fn folder_sizes(app: AppHandle, requester: u32, paths: Vec<String>, refresh: bool) -> Vec<FolderSize> {
    let s = sizes();
    let mut st = s.state.lock();
    if !st.started {
        st.started = true;
        for _ in 0..WORKERS {
            let app = app.clone();
            std::thread::spawn(move || worker(app));
        }
    }
    let wanted: Vec<(String, String)> = paths.iter().map(|p| (paths::key(p), paths::normalize(p))).collect();
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

    let mut out = Vec::new();
    for (key, path) in wanted {
        let cached = st.cache.get(&key);
        if let Some(c) = cached {
            out.push(c.size.clone());
        }
        let fresh = cached.is_some_and(|c| !refresh && !c.stale && c.at.elapsed() < STALE_AFTER);
        if fresh {
            continue;
        }
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
pub fn folder_sizes_invalidate(paths: Vec<String>, deep: bool) {
    let changed: Vec<String> = paths.iter().map(|p| paths::key(p)).collect();
    let mut st = sizes().state.lock();
    for (key, c) in st.cache.iter_mut() {
        if changed.iter().any(|p| contains(key, p) || (deep && contains(p, key))) {
            c.stale = true;
        }
    }
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
        let result = count(&job.path, &job.cancel, |progress| {
            let _ = app.emit("folder-size", progress);
        });
        let mut st = s.state.lock();
        st.running.remove(&job.key);
        if let Some(size) = result {
            st.cache.insert(job.key, Cached { size: size.clone(), at: Instant::now(), stale: false });
            drop(st);
            let _ = app.emit("folder-size", size);
        }
    }
}

/// Total size of the files under `path`. Directory listings carry each file's size, so this
/// reads no file. Links and junctions aren't followed: what they point to is counted where
/// it lives (and can't loop). None when cancelled.
fn count(path: &str, cancel: &AtomicBool, mut progress: impl FnMut(FolderSize)) -> Option<FolderSize> {
    let mut size = FolderSize { path: path.to_string(), bytes: 0, files: 0, partial: false, done: false };
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
                    size.files += 1;
                }
            }
        }
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(size.clone());
        }
    }
    size.done = true;
    Some(size)
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
    fn counts_files_below_a_folder() {
        let dir = std::env::temp_dir().join(format!("ferret-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a").join("b")).unwrap();
        std::fs::write(dir.join("one.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.join("a").join("two.bin"), vec![0u8; 2000]).unwrap();
        std::fs::write(dir.join("a").join("b").join("three.bin"), vec![0u8; 3000]).unwrap();
        let size = count(&dir.to_string_lossy(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((size.bytes, size.files, size.partial, size.done), (6000, 3, false, true));
        assert!(count(&dir.to_string_lossy(), &AtomicBool::new(true), |_| {}).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
