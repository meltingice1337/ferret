//! Walk provider (no admin rights needed): parallel directory walk of all fixed
//! drives into one index per drive, kept live with recursive
//! `ReadDirectoryChangesW` watchers, persisted to
//! `%LOCALAPPDATA%\Ferret\index-walk.bin` for instant restarts.
//!
//! notify's Windows backend uses a 16 KiB buffer and drops the watch on
//! overflow, which is fatal for whole-drive watches, so this module runs its
//! own watcher threads (1 MiB buffer; overflow → rate-limited rescan).

use super::engine::{Engine, Volume};
use super::index::{flags::*, lower, Index};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------- options

#[derive(Clone, Debug)]
pub struct WalkOptions {
    /// Roots to index (`C:\`, or any folder).
    pub roots: Vec<String>,
    /// Extra excluded folders (full paths, any case).
    pub excludes: Vec<String>,
    /// Snapshot file; `None` disables persistence.
    pub snapshot: Option<PathBuf>,
    pub watch: bool,
    pub threads: usize,
}

impl WalkOptions {
    /// All fixed drives, default excludes, snapshot in `%LOCALAPPDATA%\Ferret`.
    pub fn default_drives() -> Self {
        WalkOptions {
            roots: fixed_drives(),
            excludes: Vec::new(),
            snapshot: dirs::data_local_dir().map(|d| d.join("Ferret").join("index-walk.bin")),
            watch: true,
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 12),
        }
    }
}

/// Fixed (non-removable, non-network) drive roots, e.g. `["C:\", "D:\"]`.
#[cfg(windows)]
pub fn fixed_drives() -> Vec<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| format!("{}:\\", (b'A' + i) as char))
        .filter(|root| {
            let w: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == DRIVE_FIXED }
        })
        .collect()
}

#[cfg(not(windows))]
pub fn fixed_drives() -> Vec<String> {
    vec!["/".into()]
}

/// Folders never descended into (the folder entry itself is still indexed).
pub struct Excludes {
    names: Vec<Vec<u8>>,
    paths: Vec<Vec<u8>>,
}

impl Excludes {
    pub fn new(extra: &[String]) -> Self {
        let mut paths: Vec<Vec<u8>> = vec![b"?:\\windows\\winsxs".to_vec()];
        paths.extend(extra.iter().map(|p| lower(crate::paths::normalize(p).trim_end_matches('\\'))));
        Excludes { names: vec![b"$recycle.bin".to_vec(), b"system volume information".to_vec()], paths }
    }

    fn path_matches(pat: &[u8], p: &[u8]) -> bool {
        pat.len() == p.len() && pat.iter().zip(p).enumerate().all(|(i, (a, b))| a == b || i == 0 && *a == b'?')
    }

    /// Should the folder `path` (lower-case) not be descended into?
    pub fn skip_dir(&self, lpath: &[u8]) -> bool {
        let name = lpath.rsplit(|&b| b == b'\\').next().unwrap_or(lpath);
        self.names.iter().any(|n| n.as_slice() == name) || self.paths.iter().any(|p| Self::path_matches(p, lpath))
    }

    /// Is `lpath` inside (strictly below) an excluded folder?
    pub fn inside_excluded(&self, lpath: &[u8]) -> bool {
        let mut i = 0;
        while let Some(off) = lpath[i..].iter().position(|&b| b == b'\\') {
            let end = i + off;
            if end > 2 && self.skip_dir(&lpath[..end]) {
                return true;
            }
            i = end + 1;
        }
        false
    }
}

// ---------------------------------------------------------------- reading

pub(crate) struct Item {
    pub name: String,
    pub fl: u8,
    pub size: u64,
    pub mtime: u32,
    pub descend: bool,
}

fn unix_secs(t: std::io::Result<SystemTime>) -> u32 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

#[cfg(windows)]
fn attr_flags(attrs: u32) -> u8 {
    let mut fl = 0;
    if attrs & 0x10 != 0 {
        fl |= DIR;
    }
    if attrs & 0x2 != 0 {
        fl |= HIDDEN;
    }
    if attrs & 0x4 != 0 {
        fl |= SYSTEM;
    }
    if attrs & 0x400 != 0 {
        fl |= LINK;
    }
    fl
}

/// Folder contents; `None` when the folder can't be read (so a resync never
/// mistakes "access denied" for "empty").
fn read_items(path: &str) -> Option<Vec<Item>> {
    let rd = std::fs::read_dir(crate::paths::for_io(path)).ok()?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let symlink = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        #[cfg(windows)]
        let fl = {
            use std::os::windows::fs::MetadataExt;
            attr_flags(meta.file_attributes())
        };
        #[cfg(not(windows))]
        let fl = if meta.is_dir() { DIR } else { 0 };
        let dir = fl & DIR != 0;
        out.push(Item {
            name,
            fl,
            size: if dir { 0 } else { meta.len() },
            mtime: unix_secs(meta.modified()),
            // Junctions / symlinks are indexed but never followed (cycles, duplicates).
            descend: dir && !symlink,
        });
    }
    Some(out)
}

/// Debug check: no folder may have two live children with the same name.
#[inline]
fn verify_dir(ix: &Index, dir: u32) {
    #[cfg(debug_assertions)]
    if ix.dir_has_duplicates(dir) {
        let msg = format!("search index: duplicate children under {}", ix.path(dir));
        if cfg!(test) {
            panic!("{msg}");
        }
        log::warn!("{msg}");
    }
    let _ = (ix, dir);
}

/// Single-file stat that also returns the on-disk name casing.
#[cfg(windows)]
pub(crate) fn stat(path: &str) -> Option<Item> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FIND_FIRST_EX_FLAGS, WIN32_FIND_DATAW,
    };
    let p = if path.len() > 240 && !path.starts_with(r"\\") { format!(r"\\?\{path}") } else { path.to_string() };
    let w: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
    let mut fd = WIN32_FIND_DATAW::default();
    let h = unsafe {
        FindFirstFileExW(
            PCWSTR(w.as_ptr()),
            FindExInfoBasic,
            &mut fd as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_FLAGS(0),
        )
    }
    .ok()?;
    unsafe {
        let _ = FindClose(h);
    }
    let n = fd.cFileName.iter().position(|&c| c == 0).unwrap_or(fd.cFileName.len());
    let name = String::from_utf16_lossy(&fd.cFileName[..n]);
    let fl = attr_flags(fd.dwFileAttributes);
    let ft = ((fd.ftLastWriteTime.dwHighDateTime as u64) << 32) | fd.ftLastWriteTime.dwLowDateTime as u64;
    let mtime = (ft.saturating_sub(116_444_736_000_000_000) / 10_000_000).min(u32::MAX as u64) as u32;
    let symlink = fl & LINK != 0 && matches!(fd.dwReserved0, 0xA000_000C | 0xA000_0003);
    let dir = fl & DIR != 0;
    Some(Item {
        name,
        fl,
        size: if dir { 0 } else { ((fd.nFileSizeHigh as u64) << 32) | fd.nFileSizeLow as u64 },
        mtime,
        descend: dir && !symlink,
    })
}

#[cfg(not(windows))]
pub(crate) fn stat(path: &str) -> Option<Item> {
    let m = std::fs::symlink_metadata(path).ok()?;
    let name = std::path::Path::new(path).file_name()?.to_string_lossy().into_owned();
    Some(Item {
        name,
        fl: if m.is_dir() { DIR } else { 0 },
        size: m.len(),
        mtime: unix_secs(m.modified()),
        descend: m.is_dir(),
    })
}

/// Expands 8.3 aliases (`MELTIN~1`) without resolving links. Paths that no
/// longer exist get their deepest existing ancestor expanded.
#[cfg(windows)]
pub(crate) fn long_path(p: &str) -> String {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetLongPathNameW;
    if !p.contains('~') {
        return p.to_string();
    }
    let w: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetLongPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    if n > 0 && n < buf.len() {
        return String::from_utf16_lossy(&buf[..n]);
    }
    match p.trim_end_matches('\\').rsplit_once('\\') {
        Some((parent, name)) if parent.len() > 2 => format!("{}\\{name}", long_path(parent)),
        _ => p.to_string(),
    }
}

#[cfg(not(windows))]
pub(crate) fn long_path(p: &str) -> String {
    p.to_string()
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

// ---------------------------------------------------------------- walking

#[derive(Default)]
pub struct Counters {
    pub entries: AtomicU64,
    pub dirs_done: AtomicU64,
    pub dirs_queued: AtomicU64,
}

struct WalkCtx<'a> {
    target: &'a RwLock<Index>,
    ex: &'a Excludes,
    stop: &'a AtomicBool,
    c: &'a Counters,
    vol: Option<&'a Volume>,
    /// Expected entry count (previous snapshot) for progress.
    expect: u64,
}

impl WalkCtx<'_> {
    fn report(&self) {
        let Some(v) = self.vol else { return };
        let e = self.c.entries.load(Relaxed);
        v.set_entries(e);
        let p = if self.expect > 0 {
            e as f32 / self.expect as f32
        } else {
            let done = self.c.dirs_done.load(Relaxed) as f32;
            let q = self.c.dirs_queued.load(Relaxed) as f32;
            done / q.max(1.0)
        };
        v.set_progress(p.min(0.99));
    }
}

fn walk_dir<'s>(s: &rayon::Scope<'s>, ctx: &'s WalkCtx<'s>, path: String, idx: u32) {
    if ctx.stop.load(Relaxed) {
        return;
    }
    let Some(items) = read_items(&path) else {
        ctx.c.dirs_done.fetch_add(1, Relaxed);
        return;
    };
    let n = items.len() as u64;
    let mut sub = Vec::new();
    {
        let mut ix = ctx.target.write();
        if !ix.is_live(idx) {
            return;
        }
        if ix.children(idx).next().is_none() {
            // Fresh folder (every folder of a bulk walk): plain inserts.
            for it in items {
                let c = ix.insert(idx, &it.name, it.fl, it.size, it.mtime);
                if it.descend {
                    sub.push((c, it.name));
                }
            }
        } else {
            sync_children(&mut ix, idx, items, &mut sub);
        }
        verify_dir(&ix, idx);
    }
    ctx.c.entries.fetch_add(n, Relaxed);
    ctx.c.dirs_done.fetch_add(1, Relaxed);
    let mut queued = 0;
    for (c, name) in sub {
        let p = join(&path, &name);
        if ctx.ex.skip_dir(&lower(&p)) {
            continue;
        }
        queued += 1;
        s.spawn(move |s| walk_dir(s, ctx, p, c));
    }
    ctx.c.dirs_queued.fetch_add(queued, Relaxed);
    if ctx.c.dirs_done.load(Relaxed) % 256 == 0 {
        ctx.report();
    }
}

/// Reconciles an already indexed folder with its on-disk listing: updates
/// existing children, inserts new ones (queued for walking if folders), drops
/// vanished ones and any duplicates. Existing subfolders are not descended
/// (shallow), so a resync never re-walks a whole tree.
fn sync_children(ix: &mut Index, dir: u32, items: Vec<Item>, sub: &mut Vec<(u32, String)>) {
    let mut existing: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
    let kids: Vec<u32> = ix.children(dir).collect();
    for c in kids {
        let key = ix.name_low(c).to_vec();
        if existing.contains_key(&key) {
            ix.remove(c); // heal a duplicate
        } else {
            existing.insert(key, c);
        }
    }
    for it in items {
        match existing.remove(&lower(&it.name)) {
            Some(c) if ix.is_dir(c) == (it.fl & DIR != 0) => {
                if ix.name(c) != it.name {
                    ix.rename(c, dir, &it.name);
                }
                ix.set_flags(c, it.fl);
                ix.set_meta(c, it.size, it.mtime);
            }
            other => {
                if let Some(c) = other {
                    ix.remove(c); // file ↔ folder swap
                }
                let c = ix.insert(dir, &it.name, it.fl, it.size, it.mtime);
                if it.descend {
                    sub.push((c, it.name));
                }
            }
        }
    }
    for (_, c) in existing {
        ix.remove(c);
    }
}

/// Walks `path` (already indexed as `idx`) into `target` using `pool`.
pub(crate) fn walk_into(
    pool: &rayon::ThreadPool,
    target: &RwLock<Index>,
    path: &str,
    idx: u32,
    ex: &Excludes,
    stop: &AtomicBool,
    vol: Option<&Volume>,
    expect: u64,
) -> u64 {
    let c = Counters::default();
    c.dirs_queued.store(1, Relaxed);
    let ctx = WalkCtx { target, ex, stop, c: &c, vol, expect };
    let p = path.to_string();
    pool.scope(|s| walk_dir(s, &ctx, p, idx));
    ctx.report();
    c.entries.load(Relaxed)
}

/// Builds a fresh index for `root`.
pub fn build_index(pool: &rayon::ThreadPool, root: &str, ex: &Excludes, stop: &AtomicBool, vol: Option<&Volume>, expect: u64) -> Index {
    let target = RwLock::new(Index::with_capacity(root, true, 1 << 16, 1 << 20));
    walk_into(pool, &target, root, 0, ex, stop, vol, expect);
    let mut ix = target.into_inner();
    ix.shrink_to_fit();
    ix
}

// ---------------------------------------------------------------- live updates

/// A folder `apply_path` wants walked: `fresh` = just inserted (the walk
/// covers everything below it); otherwise a shallow resync of its listing.
pub(crate) struct WalkReq {
    pub idx: u32,
    pub path: String,
    pub fresh: bool,
}

/// Applies one changed path (created / deleted / renamed / modified) by
/// re-reading its current state from disk. Returns folders that need a walk.
///
/// Watchers may name any component by its 8.3 alias (`Users\MELTIN~1\...`),
/// so aliases are expanded first and every lookup that misses is retried with
/// the on-disk long name before anything is inserted: a miss must never turn
/// an existing folder into a duplicate (and a full re-walk of its subtree).
pub(crate) fn apply_path(ix: &mut Index, ex: &Excludes, path: &str) -> Vec<WalkReq> {
    let mut walks = Vec::new();
    let path = &long_path(path);
    let lp = lower(path);
    let root = ix.root_key();
    if lp.len() <= root.len() || !lp.starts_with(&root) || lp[root.len()] != b'\\' {
        return walks;
    }
    if ex.inside_excluded(&lp) {
        return walks;
    }
    let rel = &path[ix.root_path.trim_end_matches('\\').len()..];
    let comps: Vec<&str> = rel.split('\\').filter(|c| !c.is_empty()).collect();
    let Some((last, dirs)) = comps.split_last() else { return walks };

    // Resolve the parent folder; index + walk the first missing ancestor.
    let mut cur = 0u32;
    let mut cur_path = ix.root_path.clone();
    for comp in dirs {
        let parent_path = cur_path.clone();
        cur_path = join(&cur_path, comp);
        if let Some(c) = ix.child(cur, &lower(comp)) {
            cur = c;
            continue;
        }
        match stat(&cur_path) {
            Some(info) => {
                if let Some(c) = ix.child(cur, &lower(&info.name)) {
                    cur = c; // alias of an indexed folder
                    cur_path = join(&parent_path, &info.name);
                    continue;
                }
                let n = ix.insert(cur, &info.name, info.fl, info.size, info.mtime);
                verify_dir(ix, cur);
                if info.descend && !ex.skip_dir(&lower(&cur_path)) {
                    walks.push(WalkReq { idx: n, path: cur_path, fresh: true });
                }
                return walks; // the walk of the new folder covers `path`
            }
            None => {
                // Gone from disk. If it was named by an alias we can't map it
                // back to an entry, so resync the parent's listing instead.
                if comp.contains('~') {
                    walks.push(WalkReq { idx: cur, path: parent_path, fresh: false });
                }
                return walks;
            }
        }
    }

    match stat(path) {
        None => match ix.child(cur, &lower(last)) {
            Some(e) => ix.remove(e),
            None if last.contains('~') => walks.push(WalkReq { idx: cur, path: cur_path, fresh: false }),
            None => {}
        },
        Some(it) => {
            let found = ix.child(cur, &lower(last)).or_else(|| ix.child(cur, &lower(&it.name)));
            let full = join(&cur_path, &it.name);
            match found {
                Some(c) if ix.is_dir(c) == (it.fl & DIR != 0) => {
                    if ix.name(c) != it.name {
                        ix.rename(c, cur, &it.name);
                    }
                    ix.set_flags(c, it.fl);
                    ix.set_meta(c, it.size, it.mtime);
                }
                other => {
                    if let Some(c) = other {
                        ix.remove(c); // file ↔ folder swap
                    }
                    let n = ix.insert(cur, &it.name, it.fl, it.size, it.mtime);
                    if it.descend && !ex.skip_dir(&lower(&full)) {
                        walks.push(WalkReq { idx: n, path: full, fresh: true });
                    }
                }
            }
            verify_dir(ix, cur);
        }
    }
    walks
}

enum Msg {
    Paths(usize, Vec<String>),
    Overflow(usize),
}

#[cfg(windows)]
struct DirWatch {
    handle: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl DirWatch {
    fn start(root: &str, vol: usize, tx: Sender<Msg>, stop: Arc<AtomicBool>) -> std::io::Result<DirWatch> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        const FILE_LIST_DIRECTORY: u32 = 1;
        let w: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        let raw = h.0 as isize;
        let root = root.to_string();
        let thread = std::thread::Builder::new()
            .name("ferret-watch".into())
            .spawn(move || watch_loop(raw, &root, vol, tx, stop))?;
        Ok(DirWatch { handle: raw, thread: Some(thread) })
    }
}

#[cfg(windows)]
impl Drop for DirWatch {
    fn drop(&mut self) {
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::IO::CancelIoEx;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::System::IO::CancelSynchronousIo;
        let h = HANDLE(self.handle as *mut _);
        let Some(t) = self.thread.take() else { return };
        // The thread may be just about to re-enter ReadDirectoryChangesW, so
        // keep cancelling until it notices the stop flag.
        for _ in 0..100 {
            unsafe {
                let _ = CancelIoEx(h, None);
                let _ = CancelSynchronousIo(HANDLE(t.as_raw_handle()));
            }
            if t.is_finished() {
                let _ = t.join();
                unsafe {
                    let _ = CloseHandle(h);
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Give up (leak the handle) rather than hang the caller.
        log::warn!("watcher thread did not stop after cancel");
    }
}

#[cfg(windows)]
fn watch_loop(raw: isize, root: &str, vol: usize, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    use windows::Win32::Foundation::{ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        ReadDirectoryChangesW, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
        FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    };
    let h = HANDLE(raw as *mut _);
    let mut words = 256 * 1024; // 1 MiB (DWORD aligned)
    let mut buf = vec![0u32; words];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES;
    let mut failures = 0;
    while !stop.load(Relaxed) {
        let mut got = 0u32;
        let r = unsafe {
            ReadDirectoryChangesW(h, buf.as_mut_ptr() as *mut _, (words * 4) as u32, true, filter, Some(&mut got), None, None)
        };
        if stop.load(Relaxed) {
            break;
        }
        match r {
            Err(e) if e.code() == ERROR_OPERATION_ABORTED.to_hresult() => break,
            Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => {
                let _ = tx.send(Msg::Overflow(vol));
            }
            Err(_) => {
                failures += 1;
                if words > 16 * 1024 {
                    // Some file systems refuse > 64 KiB buffers.
                    words = 16 * 1024;
                    buf.truncate(words);
                } else if failures > 5 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Ok(()) if got == 0 => {
                let _ = tx.send(Msg::Overflow(vol));
            }
            Ok(()) => {
                failures = 0;
                let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) };
                let paths = parse_notify(bytes, root);
                if !paths.is_empty() && tx.send(Msg::Paths(vol, paths)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Parses a FILE_NOTIFY_INFORMATION chain into absolute paths.
pub(crate) fn parse_notify(bytes: &[u8], root: &str) -> Vec<String> {
    let rd = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 12 <= bytes.len() {
        let next = rd(off) as usize;
        let len = rd(off + 8) as usize;
        let s = off + 12;
        if s + len > bytes.len() {
            break;
        }
        let units: Vec<u16> = bytes[s..s + len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push(join(root, &String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

// ---------------------------------------------------------------- snapshot

// v2: v1 snapshots may contain duplicate entries (8.3-alias events), never load them.
const SNAP_MAGIC: &[u8; 8] = b"FRTWALK2";

pub fn save_snapshot(path: &std::path::Path, vols: &[Arc<Volume>]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
        w.write_all(SNAP_MAGIC)?;
        let ready: Vec<&Arc<Volume>> = vols.iter().filter(|v| v.ready.load(Relaxed)).collect();
        w.write_all(&(ready.len() as u32).to_le_bytes())?;
        for v in ready {
            let snap = v.index.read().compacted();
            if !snap.duplicate_children().is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "refusing to save duplicate entries"));
            }
            snap.write_snapshot(&mut w)?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load_snapshot(path: &std::path::Path) -> std::io::Result<Vec<Index>> {
    use std::io::Read;
    let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != SNAP_MAGIC {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad walk snapshot"));
    }
    let n = super::index::read_u32(&mut r)? as usize;
    let list = (0..n.min(64)).map(|_| Index::read_snapshot(&mut r)).collect::<std::io::Result<Vec<Index>>>()?;
    // Defensive: never resurrect duplicates — a polluted snapshot is rebuilt.
    if list.iter().any(|ix| !ix.duplicate_children().is_empty()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "walk snapshot has duplicate entries"));
    }
    Ok(list)
}

// ---------------------------------------------------------------- provider

/// 0 = live, 1 = initial build (buffer events), 2 = refresh (apply + record).
const LIVE: u8 = 0;
const INITIAL: u8 = 1;
const REFRESH: u8 = 2;

struct VolState {
    vol: Arc<Volume>,
    mode: AtomicU8,
    replay: Mutex<Vec<String>>,
    last_rescan: Mutex<Option<Instant>>,
    /// Serializes `apply` batches and index swaps (walk requests carry entry
    /// numbers that are only valid for the index they were made against).
    apply_lock: Mutex<()>,
}

pub struct WalkProvider {
    engine: Arc<Engine>,
    opts: WalkOptions,
    ex: Excludes,
    stop: Arc<AtomicBool>,
    pool: rayon::ThreadPool,
    vols: RwLock<Vec<Arc<VolState>>>,
    building: AtomicBool,
    #[cfg(windows)]
    watches: Mutex<Vec<DirWatch>>,
    started: Instant,
    pub build_secs: Mutex<Option<f64>>,
    /// Tests: feed paths through the real event loop.
    #[cfg(test)]
    inject_tx: Mutex<Option<Sender<Msg>>>,
}

impl WalkProvider {
    /// Starts indexing in the background; returns immediately.
    pub fn start(engine: Arc<Engine>, opts: WalkOptions) -> Arc<WalkProvider> {
        let threads = opts.threads.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("ferret-walk-{i}"))
            .start_handler(|_| lower_thread_priority())
            .build()
            .expect("walk pool");
        let p = Arc::new(WalkProvider {
            engine,
            ex: Excludes::new(&opts.excludes),
            opts,
            stop: Arc::new(AtomicBool::new(false)),
            pool,
            vols: RwLock::new(Vec::new()),
            building: AtomicBool::new(true),
            #[cfg(windows)]
            watches: Mutex::new(Vec::new()),
            started: Instant::now(),
            build_secs: Mutex::new(None),
            #[cfg(test)]
            inject_tx: Mutex::new(None),
        });
        let me = p.clone();
        std::thread::Builder::new().name("ferret-walk".into()).spawn(move || me.run()).expect("walk thread");
        p
    }

    /// Tests: deliver change notifications as if a watcher reported them.
    #[cfg(test)]
    fn inject(&self, vol: usize, paths: Vec<String>) -> bool {
        match self.inject_tx.lock().as_ref() {
            Some(tx) => tx.send(Msg::Paths(vol, paths)).is_ok(),
            None => false,
        }
    }

    pub fn building(&self) -> bool {
        self.building.load(Relaxed)
    }

    pub fn volumes(&self) -> Vec<Arc<Volume>> {
        self.vols.read().iter().map(|s| s.vol.clone()).collect()
    }

    /// Stops walking + watching and removes the volumes from the engine.
    pub fn stop(&self) {
        self.stop.store(true, Relaxed);
        #[cfg(test)]
        {
            self.inject_tx.lock().take();
        }
        #[cfg(windows)]
        self.watches.lock().clear();
        let mine = self.volumes();
        let keep: Vec<Arc<Volume>> =
            self.engine.volumes().into_iter().filter(|v| !mine.iter().any(|m| Arc::ptr_eq(m, v))).collect();
        self.engine.set_volumes(keep);
    }

    fn run(self: Arc<Self>) {
        let roots: Vec<String> = self.opts.roots.iter().map(|r| long_path(&crate::paths::normalize(r))).collect();
        let mut snap: HashMap<String, Index> = HashMap::new();
        if let Some(p) = &self.opts.snapshot {
            if let Ok(list) = load_snapshot(p) {
                for ix in list {
                    snap.insert(ix.root_path.to_lowercase(), ix);
                }
            }
        }
        let mut states = Vec::new();
        for root in &roots {
            let (ix, loaded) = match snap.remove(&root.to_lowercase()) {
                Some(ix) => (ix, true),
                None => (Index::new(root, true), false),
            };
            let vol = Volume::new(ix, loaded);
            self.engine.add_volume(vol.clone());
            states.push(Arc::new(VolState {
                vol,
                mode: AtomicU8::new(if loaded { REFRESH } else { INITIAL }),
                replay: Mutex::new(Vec::new()),
                last_rescan: Mutex::new(None),
                apply_lock: Mutex::new(()),
            }));
        }
        *self.vols.write() = states.clone();

        let (tx, rx) = channel();
        #[cfg(windows)]
        if self.opts.watch {
            let mut w = self.watches.lock();
            for (i, s) in states.iter().enumerate() {
                if let Ok(dw) = DirWatch::start(&s.vol.root, i, tx.clone(), self.stop.clone()) {
                    w.push(dw);
                }
            }
        }
        #[cfg(test)]
        {
            *self.inject_tx.lock() = Some(tx.clone());
        }
        drop(tx);
        let me = self.clone();
        std::thread::Builder::new()
            .name("ferret-walk-events".into())
            .spawn(move || me.event_loop(rx))
            .expect("event thread");

        // Walk all roots concurrently on the shared pool.
        std::thread::scope(|sc| {
            for s in &states {
                let me = &self;
                sc.spawn(move || me.rebuild(s));
            }
        });
        *self.build_secs.lock() = Some(self.started.elapsed().as_secs_f64());
        self.building.store(false, Relaxed);
        if !self.stop.load(Relaxed) {
            self.save();
        }
    }

    /// Full (re)walk of one root. Initial builds fill the live index directly;
    /// refreshes build a detached index and swap it in.
    fn rebuild(&self, s: &VolState) {
        if self.stop.load(Relaxed) {
            return;
        }
        let root = s.vol.root.clone();
        if s.mode.load(Relaxed) == INITIAL {
            walk_into(&self.pool, &s.vol.index, &root, 0, &self.ex, &self.stop, Some(&s.vol), 0);
            s.vol.index.write().shrink_to_fit();
        } else {
            s.mode.store(REFRESH, Relaxed);
            let expect = s.vol.index.read().live() as u64;
            let ix = build_index(&self.pool, &root, &self.ex, &self.stop, None, expect);
            if self.stop.load(Relaxed) {
                return;
            }
            // Not while an `apply` holds walk requests for the old index.
            let _serial = s.apply_lock.lock();
            *s.vol.index.write() = ix;
        }
        // Replay changes seen while walking, then go live. The mode flips under
        // the replay lock so the event thread can't append after the last drain.
        let pending = std::mem::take(&mut *s.replay.lock());
        self.apply(s, pending);
        let more = {
            let mut rp = s.replay.lock();
            s.mode.store(LIVE, Relaxed);
            std::mem::take(&mut *rp)
        };
        self.apply(s, more);
        s.vol.set_entries(s.vol.index.read().live() as u64);
        s.vol.ready.store(true, Relaxed);
        s.vol.set_progress(1.0);
    }

    fn apply(&self, s: &VolState, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        // One batch at a time per volume (the event thread and a rebuild's
        // replay may both apply), so their folder walks never interleave.
        let _serial = s.apply_lock.lock();
        let mut walks: Vec<WalkReq> = Vec::new();
        let mut fresh_keys: Vec<Vec<u8>> = Vec::new();
        {
            let mut ix = s.vol.index.write();
            for p in &paths {
                // Inside a new folder that is about to be walked anyway.
                let lp = lower(&long_path(p));
                if fresh_keys.iter().any(|k| lp.len() > k.len() && lp.starts_with(k) && lp[k.len()] == b'\\') {
                    continue;
                }
                for w in apply_path(&mut ix, &self.ex, p) {
                    if w.fresh {
                        fresh_keys.push(lower(&w.path));
                    }
                    walks.push(w);
                }
            }
        }
        for w in walks {
            walk_into(&self.pool, &s.vol.index, &w.path, w.idx, &self.ex, &self.stop, None, 0);
        }
        // Compaction renumbers entries: only after the walks above used theirs.
        let mut ix = s.vol.index.write();
        if ix.needs_compaction() {
            ix.compact();
        }
    }

    fn event_loop(self: Arc<Self>, rx: Receiver<Msg>) {
        let mut last_save = Instant::now();
        let mut saved_gen: Vec<u64> = Vec::new();
        loop {
            if self.stop.load(Relaxed) {
                return;
            }
            let first = match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    if !self.opts.watch || self.stop.load(Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(30));
                    None
                }
            };
            // Coalesce a short burst.
            let mut batch: Vec<Msg> = first.into_iter().collect();
            if !batch.is_empty() {
                let until = Instant::now() + Duration::from_millis(150);
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match rx.recv_timeout(left) {
                        Ok(m) => batch.push(m),
                        Err(_) => break,
                    }
                }
            }
            let states = self.vols.read().clone();
            let mut per: Vec<Vec<String>> = vec![Vec::new(); states.len()];
            let mut seen: Vec<std::collections::HashSet<Vec<u8>>> = vec![Default::default(); states.len()];
            for m in batch {
                match m {
                    Msg::Paths(v, ps) if v < states.len() => {
                        for p in ps {
                            if seen[v].insert(lower(&p)) {
                                per[v].push(p);
                            }
                        }
                    }
                    Msg::Overflow(v) if v < states.len() => self.schedule_rescan(states[v].clone()),
                    _ => {}
                }
            }
            for (s, paths) in states.iter().zip(per) {
                if paths.is_empty() {
                    continue;
                }
                let mut rp = s.replay.lock();
                match s.mode.load(Relaxed) {
                    INITIAL => rp.extend(paths),
                    REFRESH => {
                        rp.extend(paths.iter().cloned());
                        drop(rp);
                        self.apply(s, paths);
                    }
                    _ => {
                        drop(rp);
                        self.apply(s, paths);
                    }
                }
            }
            // Periodic snapshot when something changed.
            if !self.building() && last_save.elapsed() > Duration::from_secs(600) {
                let gens: Vec<u64> = states.iter().map(|s| s.vol.index.read().generation).collect();
                if gens != saved_gen {
                    self.save();
                    saved_gen = gens;
                }
                last_save = Instant::now();
            }
        }
    }

    fn schedule_rescan(self: &Arc<Self>, s: Arc<VolState>) {
        {
            let mut last = s.last_rescan.lock();
            if s.mode.load(Relaxed) != LIVE || last.is_some_and(|t| t.elapsed() < Duration::from_secs(600)) {
                return;
            }
            *last = Some(Instant::now());
        }
        s.mode.store(REFRESH, Relaxed);
        let me = self.clone();
        let _ = std::thread::Builder::new().name("ferret-rescan".into()).spawn(move || {
            me.rebuild(&s);
            me.save();
        });
    }

    pub fn save(&self) {
        if let Some(p) = &self.opts.snapshot {
            let _ = save_snapshot(p, &self.volumes());
        }
    }
}

impl Drop for WalkProvider {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

fn lower_thread_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::engine::SearchRequest;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferret-walk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(eng: &Engine, q: &str) -> Vec<String> {
        eng.search(&SearchRequest { query: q.into(), limit: 50, ..Default::default() })
            .results
            .into_iter()
            .map(|h| h.name)
            .collect()
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[cfg(windows)]
    fn short_path(p: &str) -> String {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::GetShortPathNameW;
        let w: Vec<u16> = p.encode_utf16().chain(Some(0)).collect();
        let mut buf = vec![0u16; 1024];
        let n = unsafe { GetShortPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    }

    /// Events can name a path through 8.3 aliases (`MELTIN~1`); they must hit
    /// the existing entries instead of inserting (and re-walking) duplicates.
    #[cfg(windows)]
    #[test]
    fn search_walk_no_duplicates_from_short_name_events() {
        let d = tmpdir("short");
        let long_dir = d.join("LongFolderNameAlpha");
        std::fs::create_dir_all(long_dir.join("Sub Folder Beta")).unwrap();
        std::fs::write(long_dir.join(r"Sub Folder Beta\package.json"), "{}").unwrap();
        std::fs::write(long_dir.join("SomeLongFileName.txt"), "x").unwrap();
        // Root = long form, like the real drive roots.
        let root = long_path(&d.to_string_lossy());
        let file = format!(r"{root}\LongFolderNameAlpha\Sub Folder Beta\package.json");
        // 8.3 alias of the last component only (aliases *below* the root, as
        // with a `C:\` watch reporting `Users\MELTIN~1\...`).
        let alias = |p: &str| short_path(p).rsplit('\\').next().unwrap_or_default().to_string();
        let a_dir = alias(&format!(r"{root}\LongFolderNameAlpha"));
        let a_sub = alias(&format!(r"{root}\LongFolderNameAlpha\Sub Folder Beta"));
        let a_txt = alias(&format!(r"{root}\LongFolderNameAlpha\SomeLongFileName.txt"));
        let short_dir = format!(r"{root}\{a_dir}");
        let short_file = format!(r"{root}\{a_dir}\{a_sub}\package.json");
        if !a_dir.contains('~') {
            eprintln!("8.3 names are disabled on this volume; short-name aliasing not exercised");
        }
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
        let ex = Excludes::new(&[]);
        let stop = AtomicBool::new(false);
        let target = RwLock::new(build_index(&pool, &root, &ex, &stop, None, 0));
        let live = target.read().live();
        assert!(target.read().duplicate_children().is_empty());

        // Replay the kinds of events seen on a live system, several times.
        let events = vec![
            short_file.clone(),
            short_dir.clone(),
            format!(r"{short_dir}\{a_txt}"),
            file.clone(),
            format!(r"{root}\LongFolderNameAlpha"),
            format!(r"{root}\LONGFOLDERNAMEALPHA\sub folder beta\PACKAGE.JSON"),
        ];
        for _ in 0..3 {
            let walks = {
                let mut ix = target.write();
                events.iter().flat_map(|p| apply_path(&mut ix, &ex, p)).collect::<Vec<_>>()
            };
            for w in walks {
                walk_into(&pool, &target, &w.path, w.idx, &ex, &stop, None, 0);
            }
        }
        // A folder walked twice (e.g. replay + live event racing) must not duplicate either.
        let ix_dir = target.read().resolve(&format!(r"{root}\LongFolderNameAlpha")).unwrap();
        walk_into(&pool, &target, &format!(r"{root}\LongFolderNameAlpha"), ix_dir, &ex, &stop, None, 0);

        {
            let ix = target.read();
            let dups = ix.duplicate_children();
            assert!(dups.is_empty(), "duplicates: {:?}", dups.iter().map(|&(a, _)| ix.path(a)).collect::<Vec<_>>());
            assert_eq!(ix.live(), live);
        }

        // Deletion reported only through the alias of the (now gone) file:
        // resolved by a shallow resync of its folder.
        let txt = format!(r"{root}\LongFolderNameAlpha\SomeLongFileName.txt");
        std::fs::remove_file(&txt).unwrap();
        let walks = apply_path(&mut target.write(), &ex, &format!(r"{short_dir}\{a_txt}"));
        for w in walks {
            assert!(!w.fresh);
            walk_into(&pool, &target, &w.path, w.idx, &ex, &stop, None, 0);
        }
        let ix = target.read();
        if a_txt.contains('~') {
            assert_eq!(ix.resolve(&txt), None);
            assert_eq!(ix.live(), live - 1);
        }
        assert!(ix.resolve(&file).is_some());
        assert!(ix.duplicate_children().is_empty());
        drop(ix);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Events for existing files/folders (long names, 8.3 aliases, odd casing)
    /// delivered through the real event loop while the provider is in INITIAL
    /// (buffer + replay), LIVE, and — after a snapshot load — REFRESH mode
    /// (apply to the old index + replay onto the new one) must never create
    /// duplicate entries.
    #[cfg(windows)]
    #[test]
    fn search_walk_modes_no_duplicates() {
        let d = tmpdir("modes");
        for i in 0..30 {
            let dir = d.join(format!(r"LongProjectFolder{i:02}\Source Files"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.parent().unwrap().join("package.json"), "{}").unwrap();
            for j in 0..40 {
                std::fs::write(dir.join(format!("ComponentFile{j:02}.tsx")), "x").unwrap();
            }
        }
        let root = long_path(&d.to_string_lossy());
        let on_disk = {
            fn count(p: &std::path::Path) -> usize {
                std::fs::read_dir(p)
                    .unwrap()
                    .flatten()
                    .map(|e| 1 + if e.file_type().unwrap().is_dir() { count(&e.path()) } else { 0 })
                    .sum()
            }
            count(std::path::Path::new(&root))
        };
        // A mix of event spellings for existing entries.
        let events = || -> Vec<String> {
            let mut v = Vec::new();
            for i in (0..30).step_by(3) {
                let dir = format!(r"{root}\LongProjectFolder{i:02}");
                let a_dir = short_path(&dir).rsplit('\\').next().unwrap_or_default().to_string();
                v.push(dir.clone());
                v.push(format!(r"{root}\{a_dir}"));
                v.push(format!(r"{root}\{a_dir}\package.json"));
                v.push(format!(r"{root}\{a_dir}\Source Files\ComponentFile07.tsx"));
                v.push(format!(r"{root}\LONGPROJECTFOLDER{i:02}\SOURCE FILES"));
                v.push(format!(r"{dir}\Source Files\ComponentFile01.tsx"));
            }
            v
        };
        let check = |eng: &Engine, phase: &str| {
            let vols = eng.volumes();
            let ix = vols[0].index.read();
            let dups = ix.duplicate_children();
            assert!(dups.is_empty(), "{phase}: duplicates {:?}", dups.iter().take(5).map(|&(a, _)| ix.path(a)).collect::<Vec<_>>());
            assert_eq!(ix.live(), on_disk, "{phase}: entry count");
            let r = eng.search(&SearchRequest { query: "package.json".into(), limit: 100, ..Default::default() });
            assert_eq!(r.total, 30, "{phase}: package.json hits");
        };
        let snap = d.with_extension("modes.bin");
        let opts = WalkOptions { roots: vec![root.clone()], excludes: vec![], snapshot: Some(snap.clone()), watch: false, threads: 2 };

        // INITIAL: inject while the first walk runs (buffered, replayed after).
        let eng = Engine::new();
        let p = WalkProvider::start(eng.clone(), opts.clone());
        assert!(wait_until(|| p.inject(0, events())));
        while p.building() {
            p.inject(0, events());
            std::thread::sleep(Duration::from_millis(2));
        }
        // LIVE
        for _ in 0..3 {
            assert!(p.inject(0, events()));
        }
        std::thread::sleep(Duration::from_millis(600));
        check(&eng, "initial+live");
        p.save();
        p.stop();

        // Snapshot load → REFRESH: events hit the old index and are replayed onto the new one.
        let eng2 = Engine::new();
        let p2 = WalkProvider::start(eng2.clone(), opts);
        assert!(wait_until(|| p2.inject(0, events())));
        while p2.building() {
            p2.inject(0, events());
            std::thread::sleep(Duration::from_millis(2));
        }
        for _ in 0..3 {
            assert!(p2.inject(0, events()));
        }
        std::thread::sleep(Duration::from_millis(600));
        check(&eng2, "snapshot+refresh");
        p2.save();
        p2.stop();
        let reloaded = load_snapshot(&snap).unwrap();
        assert!(reloaded[0].duplicate_children().is_empty());
        assert_eq!(reloaded[0].live(), on_disk);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&snap);
    }

    #[test]
    fn search_walk_excludes() {
        let ex = Excludes::new(&[r"D:\Big\Skip".to_string()]);
        assert!(ex.skip_dir(br"c:\windows\winsxs"));
        assert!(ex.skip_dir(br"e:\$recycle.bin"));
        assert!(ex.skip_dir(br"d:\big\skip"));
        assert!(!ex.skip_dir(br"c:\windows"));
        assert!(ex.inside_excluded(br"c:\windows\winsxs\x\y.dll"));
        assert!(!ex.inside_excluded(br"c:\windows\winsxs"));
        assert!(!ex.inside_excluded(br"c:\windows\system32\x.dll"));
    }

    #[test]
    fn search_walk_parse_notify() {
        let mut b = Vec::new();
        let rec = |name: &str, last: bool, b: &mut Vec<u8>| {
            let u: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
            let len = 12 + u.len();
            let padded = (len + 3) & !3;
            b.extend_from_slice(&(if last { 0 } else { padded as u32 }).to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&(u.len() as u32).to_le_bytes());
            b.extend_from_slice(&u);
            b.resize(b.len() + (padded - len), 0);
        };
        rec(r"dir\a.txt", false, &mut b);
        rec("ü.md", true, &mut b);
        assert_eq!(parse_notify(&b, r"C:\"), vec![r"C:\dir\a.txt".to_string(), r"C:\ü.md".to_string()]);
    }

    #[test]
    fn search_walk_build_watch_and_snapshot() {
        let d = tmpdir("live");
        let root = long_path(&d.to_string_lossy());
        std::fs::create_dir_all(d.join(r"proj\src")).unwrap();
        std::fs::write(d.join(r"proj\package.json"), "{}").unwrap();
        std::fs::write(d.join(r"proj\src\main.rs"), "fn main(){}").unwrap();
        std::fs::create_dir_all(d.join(r"$Recycle.Bin\junk")).unwrap();
        std::fs::write(d.join(r"$Recycle.Bin\junk\deleted.txt"), "").unwrap();
        let snap = d.with_extension("bin");
        let eng = Engine::new();
        let opts = WalkOptions {
            roots: vec![root.clone()],
            excludes: vec![],
            snapshot: Some(snap.clone()),
            watch: true,
            threads: 4,
        };
        let p = WalkProvider::start(eng.clone(), opts.clone());
        assert!(wait_until(|| !p.building()));
        assert_eq!(names(&eng, "package.json"), vec!["package.json"]);
        assert_eq!(names(&eng, "main"), vec!["main.rs"]);
        assert!(names(&eng, "deleted").is_empty(), "excluded folder content indexed");
        let st = eng.status();
        assert_eq!(st.len(), 1);
        assert!(st[0].ready && st[0].entries >= 5);

        // live: create, rename, delete, new folder with content
        std::fs::write(d.join(r"proj\src\lib.rs"), "").unwrap();
        assert!(wait_until(|| names(&eng, "lib.rs") == vec!["lib.rs"]));
        std::fs::rename(d.join(r"proj\src\lib.rs"), d.join(r"proj\src\Library.rs")).unwrap();
        // (fuzzy still offers Library.rs for "lib.rs", so check the old name is gone)
        assert!(wait_until(|| names(&eng, "library") == vec!["Library.rs"] && !names(&eng, "lib.rs").contains(&"lib.rs".to_string())));
        std::fs::remove_file(d.join(r"proj\src\Library.rs")).unwrap();
        assert!(wait_until(|| names(&eng, "library").is_empty()));
        std::fs::create_dir_all(d.join(r"proj\newdir\deep")).unwrap();
        std::fs::write(d.join(r"proj\newdir\deep\inner.txt"), "x").unwrap();
        assert!(wait_until(|| names(&eng, "inner.txt") == vec!["inner.txt"]));
        std::fs::rename(d.join(r"proj\newdir"), d.join("moved")).unwrap();
        assert!(wait_until(|| {
            let r = eng.search(&SearchRequest { query: "inner.txt".into(), limit: 5, ..Default::default() });
            r.results.len() == 1 && r.results[0].path.ends_with(r"moved\deep\inner.txt")
        }));
        std::fs::remove_dir_all(d.join("moved")).unwrap();
        assert!(wait_until(|| names(&eng, "inner").is_empty() && names(&eng, "deep").is_empty()));

        // snapshot reload
        p.save();
        p.stop();
        assert!(eng.volumes().is_empty());
        let loaded = load_snapshot(&snap).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].resolve(&format!(r"{root}\proj\src\main.rs")).is_some());
        let eng2 = Engine::new();
        let p2 = WalkProvider::start(eng2.clone(), WalkOptions { watch: false, ..opts });
        // Served from the snapshot immediately, then refreshed in the background.
        assert!(wait_until(|| names(&eng2, "main.rs") == vec!["main.rs"]));
        assert!(wait_until(|| !p2.building()));
        p2.stop();
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(&snap);
    }

    /// Real-profile run (read-only): `cargo test --release --lib search::walk::tests::live_profile -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_profile() {
        let root = std::env::var("FERRET_WALK_ROOT")
            .unwrap_or_else(|_| dirs::home_dir().unwrap().to_string_lossy().to_string());
        let eng = Engine::new();
        let t = Instant::now();
        let p = WalkProvider::start(
            eng.clone(),
            WalkOptions { roots: vec![root.clone()], excludes: vec![], snapshot: None, watch: false, threads: 12 },
        );
        while p.building() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let secs = t.elapsed().as_secs_f64();
        let vols = eng.volumes();
        let (entries, heap) = {
            let ix = vols[0].index.read();
            assert!(ix.duplicate_children().is_empty(), "duplicate entries in the real-profile index");
            (ix.live(), ix.heap_bytes())
        };
        println!(
            "walked {root}: {entries} entries in {secs:.2}s ({:.0} entries/s), heap {:.1} MB ({:.1} B/entry)",
            entries as f64 / secs,
            heap as f64 / 1e6,
            heap as f64 / entries.max(1) as f64
        );
        for q in [
            "node", "package.json", ".gitignore", "readme md", "ext:rs main", "zqxj", "node-hid package.json",
            r"package.json !node_modules\", "src index.ts",
        ] {
            let mut v = Vec::new();
            let mut r = None;
            for i in 0..23 {
                eng.clear_cache();
                let t = Instant::now();
                let x = eng.search(&SearchRequest { query: q.into(), limit: 50, ..Default::default() });
                if i >= 3 {
                    v.push(t.elapsed().as_secs_f64() * 1e3);
                }
                r = Some(x);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let r = r.unwrap();
            println!(
                "| `{q}` | {} | {} | {:.2} | {:.2} |",
                r.total,
                r.results.first().map(|h| h.path.as_str()).unwrap_or("-"),
                v[v.len() / 2],
                v[(v.len() as f64 * 0.95) as usize - 1]
            );
        }
        p.stop();
    }
}
