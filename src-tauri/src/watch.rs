//! Non-recursive watchers on the folders currently open in tabs. Emits
//! `fs-changed` with the affected folder paths; the UI debounces and reloads.
//! The Recycle Bin is watched through the user's `$Recycle.Bin\<SID>` folders
//! and reported under its virtual path.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

use crate::{paths, recycle};

pub struct DirWatcher {
    watcher: Option<RecommendedWatcher>,
    /// path key → display path
    watched: HashMap<String, String>,
    /// Keys of watched Recycle Bin folders (events there mean "the Recycle Bin changed").
    bins: Arc<Mutex<HashSet<String>>>,
}

impl DirWatcher {
    pub fn new(app: AppHandle) -> Self {
        let bins: Arc<Mutex<HashSet<String>>> = Arc::default();
        let bin_keys = bins.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Report each path's folder and the path itself (a watched folder that
            // gets deleted or renamed reports its own path).
            let mut dirs: HashSet<String> = ev
                .paths
                .iter()
                .flat_map(|p| {
                    let s = p.to_string_lossy();
                    [paths::parent(&s), Some(paths::normalize(&s))]
                })
                .flatten()
                .collect();
            let bins = bin_keys.lock();
            if !bins.is_empty() && dirs.iter().any(|d| bins.contains(&paths::key(d))) {
                dirs.insert(recycle::PATH.to_string());
            }
            drop(bins);
            if !dirs.is_empty() {
                let _ = app.emit("fs-changed", dirs.into_iter().collect::<Vec<_>>());
            }
        })
        .inspect_err(|e| log::warn!("file watcher unavailable: {e}"))
        .ok();
        Self { watcher, watched: HashMap::new(), bins }
    }

    pub fn set(&mut self, wanted: Vec<String>) {
        let Some(w) = self.watcher.as_mut() else { return };
        // The Recycle Bin stands for the real folders behind it.
        let bin_dirs = if wanted.iter().any(|p| recycle::is_recycle(p)) { recycle::bin_dirs() } else { vec![] };
        *self.bins.lock() = bin_dirs.iter().map(|d| paths::key(d)).collect();
        let wanted: HashMap<String, String> = wanted
            .into_iter()
            .filter(|p| !recycle::is_recycle(p))
            .chain(bin_dirs)
            .map(|p| (paths::key(&p), paths::normalize(&p)))
            .collect();
        self.watched.retain(|k, p| {
            if wanted.contains_key(k) {
                true
            } else {
                let _ = w.unwatch(&PathBuf::from(paths::for_io(p)));
                false
            }
        });
        for (k, p) in wanted {
            if self.watched.contains_key(&k) {
                continue;
            }
            // Watching can fail on some network/WSL mounts; those tabs just
            // won't auto-refresh.
            match w.watch(&PathBuf::from(paths::for_io(&p)), RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(k, p);
                }
                Err(e) => log::debug!("not watching \"{p}\": {e}"),
            }
        }
    }
}
