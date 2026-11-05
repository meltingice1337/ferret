//! Deleting for good, fast and stoppable, wherever Windows' own file calls work: this PC's drives,
//! network folders, USB sticks (Linux deletes its own: `fileops::delete_in_linux`).
//!
//! [`scan`] lists everything first: the job knows how much there is, and stopped there, nothing is
//! gone. [`run`] then deletes the files, each of a few threads working through one folder at a
//! time, and the folders after them, deepest first. Stopped midway, what was reached is gone and
//! the rest stays. What can't go doesn't stop the rest, as with Skip in Explorer: it's reported at
//! the end, and the folders holding it stay.
//!
//! Measured on 30,000 files in 320 folders (debug build, SSD): 12,300 files/s on one thread, 26,700
//! on 8, no more on 16. Threads taking files from the same folder get in each other's way (8: 11,500),
//! hence one folder per thread. Windows' own delete got through a build folder at about 1,760/s.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Deleted files are reported in groups this big.
const REPORT_EVERY: u64 = 32;
const SEEN_EVERY: u64 = 4096;
const ERROR_DIR_NOT_EMPTY: i32 = 145;

struct Folder {
    path: PathBuf,
    /// Which of the items asked for it's part of.
    top: usize,
    /// Its files and links (a link goes, never what it points to).
    names: Vec<OsString>,
    /// False when it only holds an item that's a file: the folder itself stays.
    remove: bool,
}

/// What couldn't be deleted of one item asked for: the first thing that wouldn't go.
#[derive(Debug)]
pub struct Failure {
    pub top: usize,
    pub path: PathBuf,
    pub error: io::Error,
    /// How many others of this item wouldn't go either.
    pub more: u64,
}

#[derive(Default)]
struct Failures(Vec<Failure>);

impl Failures {
    fn add(&mut self, top: usize, path: PathBuf, error: io::Error) {
        match self.0.iter_mut().find(|f| f.top == top) {
            Some(f) => f.more += 1,
            None => self.0.push(Failure { top, path, error, more: 0 }),
        }
    }

    fn has(&self, top: usize) -> bool {
        self.0.iter().any(|f| f.top == top)
    }
}

/// Everything under the items asked for.
#[derive(Default)]
pub struct Scan {
    folders: Vec<Folder>,
    failures: Failures,
}

impl Scan {
    /// How many files, links and folders there are to delete.
    pub fn entries(&self) -> u64 {
        self.folders.iter().map(|f| f.names.len() as u64 + u64::from(f.remove)).sum()
    }
}

/// Lists what deleting `items` means. None once `stop` says so. `seen` gets the running count now
/// and then. An item that's already gone is nothing to do.
pub fn scan(items: &[PathBuf], stop: &dyn Fn() -> bool, seen: &mut dyn FnMut(u64)) -> Option<Scan> {
    let mut out = Scan::default();
    let mut count = 0u64;
    for (top, item) in items.iter().enumerate() {
        let meta = match fs::symlink_metadata(item) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                out.failures.add(top, item.clone(), e);
                continue;
            }
        };
        if !meta.is_dir() {
            let (Some(parent), Some(name)) = (item.parent(), item.file_name()) else { continue };
            out.folders.push(Folder { path: parent.to_path_buf(), top, names: vec![name.to_os_string()], remove: false });
            continue;
        }
        // Parents come before what's inside them, so going backwards takes the deepest first.
        let mut i = out.folders.len();
        out.folders.push(Folder { path: item.clone(), top, names: Vec::new(), remove: true });
        while i < out.folders.len() {
            if stop() {
                return None;
            }
            let path = out.folders[i].path.clone();
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    i += 1;
                    continue;
                }
                Err(e) => {
                    out.failures.add(top, path, e);
                    i += 1;
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        out.failures.add(top, path.clone(), e);
                        break;
                    }
                };
                // Links and junctions don't count as folders here: they aren't walked.
                let is_folder = entry.file_type().is_ok_and(|t| t.is_dir());
                if is_folder {
                    out.folders.push(Folder { path: entry.path(), top, names: Vec::new(), remove: true });
                } else {
                    out.folders[i].names.push(entry.file_name());
                }
                count += 1;
                if count % SEEN_EVERY == 0 {
                    seen(count);
                }
            }
            i += 1;
        }
    }
    Some(out)
}

/// Deletes what `scan` found on `threads` threads, telling `done` how many went and the latest of
/// them. Returns what wouldn't go; stopped, it just returns early (the caller knows it stopped).
pub fn run(scan: Scan, threads: usize, stop: &(dyn Fn() -> bool + Sync), done: &(dyn Fn(u64, &Path) + Sync)) -> Vec<Failure> {
    let Scan { folders, failures } = scan;
    let failures = Mutex::new(failures);
    let next = AtomicUsize::new(0);
    let work = || {
        while let Some(folder) = folders.get(next.fetch_add(1, Ordering::Relaxed)) {
            let mut unreported = 0;
            let mut last = None;
            for name in &folder.names {
                if stop() {
                    return;
                }
                let path = folder.path.join(name);
                match remove_entry(&path) {
                    Ok(()) => {
                        unreported += 1;
                        if unreported == REPORT_EVERY {
                            done(unreported, &path);
                            unreported = 0;
                            last = None;
                        } else {
                            last = Some(path);
                        }
                    }
                    Err(e) => failures.lock().unwrap().add(folder.top, path, e),
                }
            }
            if let Some(path) = last {
                done(unreported, &path);
            }
        }
    };
    std::thread::scope(|s| {
        for _ in 1..threads.clamp(1, folders.len().max(1)) {
            s.spawn(&work);
        }
        work();
    });
    for folder in folders.iter().rev().filter(|f| f.remove) {
        if stop() {
            break;
        }
        // One that holds something that wouldn't go can't be removed, and that says nothing new.
        let holds_failure = failures.lock().unwrap().has(folder.top);
        match remove_folder(&folder.path, !holds_failure) {
            Ok(()) => done(1, &folder.path),
            Err(e) if holds_failure && e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY) => {}
            Err(e) => failures.lock().unwrap().add(folder.top, folder.path.clone(), e),
        }
    }
    failures.into_inner().unwrap().0
}

/// How many threads delete on this path's drive: a USB stick only gets slower with more.
pub fn threads_for(path: &str) -> usize {
    match crate::recycle::drive_type(path) {
        Some(crate::recycle::DRIVE_FIXED) => 8,
        Some(crate::recycle::DRIVE_REMOTE) => 4,
        None if path.starts_with(r"\\") => 4,
        _ => 1,
    }
}

/// Why something wouldn't go, for a person.
pub fn reason(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(32 | 33) => "it's open in another program".into(),
        Some(5) => "access denied".into(),
        Some(ERROR_DIR_NOT_EMPTY) => "something new appeared inside it".into(),
        _ => {
            let text = e.to_string();
            let text = text.split(" (os error").next().unwrap_or(&text).trim_end_matches('.').to_string();
            let mut c = text.chars();
            c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or(text)
        }
    }
}

/// A file or link. Gone already is fine.
fn remove_entry(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        // A link to a folder, or read-only, or really not allowed: only then is it looked at.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let meta = match fs::symlink_metadata(p) {
                Ok(m) => m,
                Err(gone) if gone.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(_) => return Err(e),
            };
            #[cfg(windows)]
            let as_folder = {
                use std::os::windows::fs::FileTypeExt;
                meta.file_type().is_symlink_dir()
            };
            #[cfg(not(windows))]
            let as_folder = false;
            let remove = || if as_folder { fs::remove_dir(p) } else { fs::remove_file(p) };
            without_read_only(p, &meta, &remove)
        }
        Err(e) => Err(e),
    }
}

/// An empty folder. `patient`: a program that's still looking at what was inside (a virus scanner,
/// an indexer) can keep it from being empty for a moment.
fn remove_folder(p: &Path, patient: bool) -> io::Result<()> {
    let remove = || fs::remove_dir(p);
    let mut waits = [10u64, 50, 200].iter();
    loop {
        match remove() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let Ok(meta) = fs::symlink_metadata(p) else { return Err(e) };
                return without_read_only(p, &meta, &remove);
            }
            Err(e) if patient && e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY) => match waits.next() {
                Some(ms) => std::thread::sleep(Duration::from_millis(*ms)),
                None => return Err(e),
            },
            Err(e) => return Err(e),
        }
    }
}

/// Read-only doesn't stop a delete, as for Explorer (through a link it would change what the link
/// points to, so not those).
fn without_read_only(p: &Path, meta: &fs::Metadata, remove: &dyn Fn() -> io::Result<()>) -> io::Result<()> {
    match remove() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && meta.permissions().readonly() && !meta.file_type().is_symlink() => {
            let mut writable = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)] // On Windows it only clears the attribute.
            writable.set_readonly(false);
            fs::set_permissions(p, writable)?;
            remove()
        }
        done => done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    fn temp(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ferret-remove-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn scan_all(items: &[PathBuf]) -> Scan {
        scan(items, &|| false, &mut |_| {}).unwrap()
    }

    /// A folder with a read-only file and folder, and a link to a folder that must stay.
    #[test]
    fn deletes_everything_but_what_links_point_to() {
        let root = temp("all");
        let (dir, keep) = (root.join("dir"), root.join("keep"));
        fs::create_dir_all(dir.join("sub/readonly")).unwrap();
        fs::create_dir_all(&keep).unwrap();
        fs::write(keep.join("kept.txt"), "kept").unwrap();
        fs::write(dir.join("sub/a.txt"), "a").unwrap();
        fs::write(dir.join("sub/readonly/f.txt"), "f").unwrap();
        for p in [dir.join("sub/a.txt"), dir.join("sub/readonly")] {
            let mut perm = fs::metadata(&p).unwrap().permissions();
            perm.set_readonly(true);
            fs::set_permissions(&p, perm).unwrap();
        }
        // A junction needs no special rights.
        let out = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(dir.join("junction")).arg(&keep).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let found = scan_all(std::slice::from_ref(&dir));
        // dir, sub, readonly; a.txt, f.txt, junction.
        assert_eq!(found.entries(), 6);
        let reported = AtomicU64::new(0);
        let failures = run(found, 4, &|| false, &|n, _| {
            reported.fetch_add(n, Ordering::Relaxed);
        });
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(reported.load(Ordering::Relaxed), 6);
        assert!(!dir.exists());
        assert_eq!(fs::read_to_string(keep.join("kept.txt")).unwrap(), "kept", "the link's target stays");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stopping_leaves_the_rest() {
        let root = temp("stop");
        let dir = root.join("dir");
        for d in 0..20 {
            let sub = dir.join(format!("d{d}"));
            fs::create_dir_all(&sub).unwrap();
            for f in 0..50 {
                fs::write(sub.join(format!("f{f}")), "x").unwrap();
            }
        }
        // Stopped while listing: nothing goes, and it says so.
        assert!(scan(std::slice::from_ref(&dir), &|| true, &mut |_| {}).is_none());
        // Stopped right away: nothing goes.
        run(scan_all(std::slice::from_ref(&dir)), 4, &|| true, &|_, _| panic!("nothing should go"));
        assert_eq!(scan_all(std::slice::from_ref(&dir)).entries(), 1021);
        // Stopped midway: some goes, the rest stays.
        let asked = AtomicUsize::new(0);
        let failures = run(scan_all(std::slice::from_ref(&dir)), 4, &|| asked.fetch_add(1, Ordering::Relaxed) > 300, &|_, _| {});
        assert!(failures.is_empty(), "{failures:?}");
        let left = scan_all(std::slice::from_ref(&dir)).entries();
        assert!(left > 0 && left < 1021, "{left} left");
        // And carrying on finishes it.
        assert!(run(scan_all(std::slice::from_ref(&dir)), 4, &|| false, &|_, _| {}).is_empty());
        assert!(!dir.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_wont_go_stays_and_the_rest_goes() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = temp("locked");
        let dir = root.join("dir");
        let other = root.join("other.txt");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("sub/locked.txt"), "l").unwrap();
        fs::write(dir.join("sub/b.txt"), "b").unwrap();
        fs::write(&other, "o").unwrap();
        let hold = fs::OpenOptions::new().read(true).share_mode(0).open(dir.join("sub/locked.txt")).unwrap();

        let items = [dir.clone(), other.clone()];
        let failures = run(scan_all(&items), 2, &|| false, &|_, _| {});
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].top, 0);
        assert!(failures[0].path.ends_with("locked.txt"));
        assert_eq!(reason(&failures[0].error), "it's open in another program");
        assert_eq!(failures[0].more, 0, "the folders holding it aren't failures of their own");
        assert!(!dir.join("a.txt").exists() && !dir.join("sub/b.txt").exists() && !other.exists());
        assert!(dir.join("sub/locked.txt").exists());

        // Try again, once it's closed.
        drop(hold);
        assert!(run(scan_all(std::slice::from_ref(&dir)), 2, &|| false, &|_, _| {}).is_empty());
        assert!(!dir.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn what_is_gone_already_is_fine() {
        let root = temp("gone");
        let gone = root.join("gone");
        assert_eq!(scan_all(std::slice::from_ref(&gone)).entries(), 0);
        // A file that goes between listing and deleting.
        let file = root.join("f.txt");
        fs::write(&file, "x").unwrap();
        let found = scan_all(std::slice::from_ref(&file));
        assert_eq!(found.entries(), 1, "the file, not the folder holding it");
        fs::remove_file(&file).unwrap();
        assert!(run(found, 1, &|| false, &|_, _| {}).is_empty());
        assert!(root.exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
