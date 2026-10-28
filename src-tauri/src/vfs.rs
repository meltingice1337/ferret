//! What copying and moving needs from a file system, for this PC's disks and for servers alike
//! (remote/), so one copy engine serves every pair: disk to disk, disk to server, server to disk,
//! server to server. Paths are Ferret's (`C:\a\b`, `\\ssh:\home.local\home\me`); each file system
//! turns them into its own.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::jobs::JobCtx;
use crate::{listing, paths};

type Res<T> = Result<T, String>;

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub is_dir: bool,
    pub len: u64,
    pub modified: Option<SystemTime>,
    /// Unix permission bits (servers).
    pub mode: Option<u32>,
    /// Windows' read-only attribute (this PC).
    pub readonly: bool,
}

pub trait Vfs: Send + Sync {
    /// The item itself (a link as a link).
    fn lstat(&self, p: &str) -> std::io::Result<Meta>;
    /// What a link points at.
    fn stat(&self, p: &str) -> std::io::Result<Meta>;
    /// The names in a folder.
    fn read_dir(&self, p: &str) -> std::io::Result<Vec<String>>;
    fn open(&self, p: &str) -> std::io::Result<Box<dyn Read + Send>>;
    /// A new file (or an existing one emptied).
    fn create(&self, p: &str) -> std::io::Result<Box<dyn Write + Send>>;
    fn mkdir(&self, p: &str) -> std::io::Result<()>;
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()>;
    /// Removes a file, or a folder with everything in it, for good.
    fn remove(&self, p: &str) -> std::io::Result<()>;
    /// Sets what a copy keeps from its original (modified time, permissions).
    fn finish(&self, p: &str, like: &Meta);
    /// Moves an item about to be replaced where undo can get it back (the Recycle Bin, the
    /// server's trash); Some(where it went, for undo), or None when it was deleted instead.
    fn displace(&self, p: &str) -> Res<Option<String>>;
    /// Same file system (a move is a rename).
    fn same(&self, a: &str, b: &str) -> bool;
}

pub fn exists(fs: &dyn Vfs, p: &str) -> bool {
    fs.lstat(p).is_ok()
}

/// `name (2).ext`, `name (3).ext`… (or `name - Copy.ext` for a copy next to its original): the
/// first that isn't in `dir`.
pub fn unique_name(fs: &dyn Vfs, dir: &str, name: &str, copy_style: bool) -> String {
    if !exists(fs, &listing::join(dir, name)) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..10_000)
        .map(|i| {
            if copy_style {
                if i == 1 { format!("{stem} - Copy{ext}") } else { format!("{stem} - Copy ({i}){ext}") }
            } else {
                format!("{stem} ({}){ext}", i + 1)
            }
        })
        .find(|c| !exists(fs, &listing::join(dir, c)))
        .unwrap_or_else(|| format!("{stem} ({}){ext}", std::process::id()))
}

/// The file system a path is on.
pub fn of(p: &str) -> Arc<dyn Vfs> {
    match paths::remote_parts(p) {
        Some((id, _)) => crate::remote::fs::RemoteFs::shared(id),
        None => Arc::new(Local),
    }
}

// ---------- this PC's disks ----------

pub struct Local;

fn io(p: &str) -> PathBuf {
    PathBuf::from(paths::for_io(p))
}

fn meta(m: &fs::Metadata) -> Meta {
    Meta {
        is_dir: m.is_dir(),
        len: if m.is_dir() { 0 } else { m.len() },
        modified: m.modified().ok(),
        mode: None,
        readonly: m.permissions().readonly(),
    }
}

impl Vfs for Local {
    fn lstat(&self, p: &str) -> std::io::Result<Meta> {
        fs::symlink_metadata(io(p)).map(|m| meta(&m))
    }
    fn stat(&self, p: &str) -> std::io::Result<Meta> {
        fs::metadata(io(p)).map(|m| meta(&m))
    }
    fn read_dir(&self, p: &str) -> std::io::Result<Vec<String>> {
        Ok(fs::read_dir(io(p))?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
    }
    fn open(&self, p: &str) -> std::io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(fs::File::open(io(p))?))
    }
    fn create(&self, p: &str) -> std::io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(fs::File::create(io(p))?))
    }
    fn mkdir(&self, p: &str) -> std::io::Result<()> {
        fs::create_dir(io(p))
    }
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        fs::rename(io(from), io(to))
    }
    fn remove(&self, p: &str) -> std::io::Result<()> {
        let path = io(p);
        if fs::symlink_metadata(&path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
    }
    fn finish(&self, p: &str, like: &Meta) {
        let path = io(p);
        if let Some(t) = like.modified {
            // A folder's time is set through a handle opened for it (std opens folders too).
            if let Ok(f) = fs::File::options().write(!like.is_dir).read(like.is_dir).open(&path) {
                let _ = f.set_modified(t);
            }
        }
        if like.readonly && !like.is_dir
            && let Ok(m) = fs::metadata(&path)
        {
            let mut p = m.permissions();
            p.set_readonly(true);
            let _ = fs::set_permissions(&path, p);
        }
    }
    fn displace(&self, p: &str) -> Res<Option<String>> {
        match trash::delete(io(p)) {
            Ok(()) => Ok(Some(p.to_string())),
            // Where there's no Recycle Bin (some network and WSL shares).
            Err(_) => self.remove(p).map(|_| None).map_err(|e| format!("Couldn't replace {p}: {e}")),
        }
    }
    fn same(&self, a: &str, b: &str) -> bool {
        let root = |p: &str| -> String {
            let k = paths::key(p);
            match k.strip_prefix(r"\\") {
                Some(rest) => rest.split('\\').take(2).collect::<Vec<_>>().join("\\").to_lowercase(),
                None => k.chars().take(2).collect(),
            }
        };
        !paths::is_remote(a) && !paths::is_remote(b) && root(a) == root(b)
    }
}

// ---------- the copy engine ----------

/// Bytes and files under `p` (for the progress bar).
pub fn totals(fs: &dyn Vfs, p: &str, bytes: &mut u64, files: &mut u64, ctx: &JobCtx) -> Res<()> {
    ctx.check()?;
    let m = fs.lstat(p).map_err(|e| e.to_string())?;
    if m.is_dir {
        for name in fs.read_dir(p).map_err(|e| e.to_string())? {
            totals(fs, &listing::join(p, &name), bytes, files, ctx)?;
        }
    } else {
        *bytes += m.len;
        *files += 1;
    }
    Ok(())
}

/// Copies one file's bytes, with progress; a copy that fails or is cancelled is removed.
pub fn copy_file(from: &dyn Vfs, src: &str, to: &dyn Vfs, dst: &str, ctx: &JobCtx) -> Res<()> {
    let m = from.stat(src).map_err(|e| format!("{src}: {e}"))?;
    let mut input = from.open(src).map_err(|e| format!("{src}: {e}"))?;
    let mut output = to.create(dst).map_err(|e| format!("{dst}: {e}"))?;
    let mut buf = vec![0u8; 1 << 20];
    let result = (|| -> Res<()> {
        loop {
            ctx.check()?;
            let n = input.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            ctx.add_bytes(n as u64);
        }
        output.flush().map_err(|e| e.to_string())
    })();
    drop(output);
    match result {
        Ok(()) => {
            to.finish(dst, &m);
            Ok(())
        }
        Err(e) => {
            let _ = to.remove(dst);
            Err(e)
        }
    }
}

/// What to do about an item that's already there.
#[derive(serde::Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Policy {
    Replace,
    Skip,
    KeepBoth,
}

/// Moves a replaced item out of the way, reporting it for undo.
pub fn displace(fs: &dyn Vfs, p: &str, ctx: &JobCtx) -> Res<()> {
    if let Some(at) = fs.displace(p)? {
        ctx.replaced(at);
    }
    Ok(())
}

/// Copies `src` to `dst`, merging into existing folders. Reports exactly what it created
/// (`report`), so undo never touches content that was already there, and sets aside anything
/// it replaces.
pub fn copy_tree(from: &dyn Vfs, src: &str, to: &dyn Vfs, dst: &str, policy: Policy, ctx: &JobCtx, report: bool) -> Res<()> {
    ctx.check()?;
    let m = from.lstat(src).map_err(|e| format!("{src}: {e}"))?;
    if m.is_dir {
        let merging = to.stat(dst).is_ok_and(|d| d.is_dir);
        if !merging {
            to.mkdir(dst).map_err(|e| format!("{dst}: {e}"))?;
            if report {
                ctx.created(dst.to_string());
                ctx.pair(src.to_string(), dst.to_string());
            }
        }
        for name in from.read_dir(src).map_err(|e| e.to_string())? {
            // Inside a brand-new folder only the folder itself is reported.
            copy_tree(from, &listing::join(src, &name), to, &listing::join(dst, &name), policy, ctx, report && merging)?;
        }
        to.finish(dst, &m);
        return Ok(());
    }
    ctx.set_current(src.rsplit('\\').next().unwrap_or(src));
    let mut target = dst.to_string();
    if exists(to, dst) {
        match policy {
            Policy::Skip => {
                ctx.add_bytes(m.len);
                ctx.file_done();
                ctx.skipped();
                return Ok(());
            }
            Policy::Replace => displace(to, dst, ctx)?,
            Policy::KeepBoth => {
                let dir = paths::parent(dst).unwrap_or_default();
                let name = dst.rsplit('\\').next().unwrap_or(dst);
                target = listing::join(&dir, &unique_name(to, &dir, name, false));
            }
        }
    }
    copy_file(from, src, to, &target, ctx)?;
    if report {
        ctx.created(target.clone());
        ctx.pair(src.to_string(), target);
    }
    ctx.file_done();
    Ok(())
}
