//! A server's files over SFTP: listing, small reads for previews, the server's trash, and the
//! file system copies use (`RemoteFs`).
//!
//! Delete moves items to the server's own trash, the freedesktop one desktop Linux uses
//! (`~/.local/share/Trash/files`, with `info/<name>.trashinfo` saying where each came from), so
//! undo puts them back and a desktop on the server shows them in its trash. An item on another
//! file system than the home folder can't be moved there: the delete job reports it as
//! `NOT_TRASHABLE`, and the window asks before deleting it for good.

use russh_sftp::client::fs::Metadata;
use russh_sftp::protocol::{OpenFlags, StatusCode};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::block;
use super::ssh::{self, Session};
use crate::listing::Entry;
use crate::paths;
use crate::vfs::{Meta, Vfs};

type Res<T> = Result<T, String>;

/// What the delete job says about an item that can't go to the server's trash.
pub const NOT_TRASHABLE: &str = "not_trashable:";

/// The connection and the Linux path of a Ferret path.
pub fn split(p: &str) -> Res<(String, String)> {
    let n = paths::normalize(p);
    paths::remote_parts(&n).map(|(id, l)| (id.to_string(), l)).ok_or_else(|| format!("{p} isn't on a server"))
}

/// The session for a path, connecting if need be.
pub async fn session_of(p: &str) -> Res<(Arc<Session>, String)> {
    let (id, linux) = split(p)?;
    Ok((ssh::session(&id).await?, linux))
}

/// An SFTP error in words. A folder that can't be opened says `denied:` (the window explains).
pub fn sftp_err(p: &str, e: russh_sftp::client::error::Error) -> String {
    use russh_sftp::client::error::Error;
    match &e {
        Error::Status(s) if s.status_code == StatusCode::NoSuchFile => format!("“{}” no longer exists", name_of(p)),
        Error::Status(s) if s.status_code == StatusCode::PermissionDenied => format!("denied:{p}"),
        Error::Status(s) if !s.error_message.is_empty() => s.error_message.clone(),
        Error::Timeout => "The server stopped answering".into(),
        _ => e.to_string(),
    }
}

fn io_err(e: russh_sftp::client::error::Error) -> std::io::Error {
    use russh_sftp::client::error::Error;
    let kind = match &e {
        Error::Status(s) if s.status_code == StatusCode::NoSuchFile => std::io::ErrorKind::NotFound,
        Error::Status(s) if s.status_code == StatusCode::PermissionDenied => std::io::ErrorKind::PermissionDenied,
        Error::Timeout => std::io::ErrorKind::TimedOut,
        _ => std::io::ErrorKind::Other,
    };
    std::io::Error::new(kind, e.to_string())
}

fn name_of(p: &str) -> &str {
    p.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(p)
}

fn ms(m: &Metadata) -> Option<i64> {
    m.mtime.map(|t| t as i64 * 1000)
}

fn linux_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

fn entry(id: &str, dir: &str, name: String, lm: &Metadata, target: Option<&Metadata>) -> Entry {
    let is_symlink = lm.is_symlink();
    let is_dir = if is_symlink { target.is_some_and(|t| t.is_dir()) } else { lm.is_dir() };
    let size = if is_dir { 0 } else { target.unwrap_or(lm).size.unwrap_or(0) };
    let ext = if is_dir {
        String::new()
    } else {
        name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map(|(_, e)| e.to_lowercase()).unwrap_or_default()
    };
    Entry {
        hidden: name.starts_with('.'),
        path: paths::remote_path(id, &linux_join(dir, &name)),
        size,
        modified: ms(target.unwrap_or(lm)),
        created: None,
        name,
        is_dir,
        is_symlink,
        ext,
        in_archive: false,
        origin: None,
    }
}

/// A server folder's contents (connecting first if need be).
pub async fn list(p: &str) -> Res<(String, Vec<Entry>)> {
    let (s, linux) = session_of(p).await?;
    let (id, _) = split(p)?;
    s.touch();
    let rd = s.sftp.read_dir(linux.clone()).await.map_err(|e| sftp_err(&paths::normalize(p), e))?;
    let mut out = Vec::new();
    let mut links = Vec::new();
    for e in rd {
        let name = e.file_name();
        // Windows paths can't show a name with a backslash (Linux allows it).
        if name.contains('\\') {
            continue;
        }
        let m = e.metadata();
        if m.is_symlink() {
            links.push((name, m));
        } else {
            out.push(entry(&id, &linux, name, &m, None));
        }
    }
    // Links: what they point at decides folder or file (a few at a time).
    let mut set = tokio::task::JoinSet::new();
    for (name, m) in links {
        let s = s.clone();
        let full = linux_join(&linux, &name);
        set.spawn(async move { let t = s.sftp.metadata(full).await.ok(); (name, m, t) });
    }
    while let Some(Ok((name, m, t))) = set.join_next().await {
        out.push(entry(&id, &linux, name, &m, t.as_ref()));
    }
    Ok((paths::remote_path(&id, &linux), out))
}

/// What a typed server path is ("dir", "file", "missing"), with `~` (`/~/…`, from parse_input)
/// as the user's home there.
pub async fn resolve(p: &str) -> Res<(String, &'static str)> {
    let (s, mut linux) = session_of(p).await?;
    let (id, _) = split(p)?;
    if linux == "/~" || linux.starts_with("/~/") {
        linux = format!("{}{}", s.home.trim_end_matches('/'), &linux[2..]);
    }
    let kind = match s.sftp.metadata(linux.clone()).await {
        Ok(m) if m.is_dir() => "dir",
        Ok(_) => "file",
        Err(_) => "missing",
    };
    Ok((paths::remote_path(&id, &linux), kind))
}

pub async fn exists(s: &Session, linux: &str) -> bool {
    s.sftp.symlink_metadata(linux.to_string()).await.is_ok()
}

/// `name (2).ext`, … : the first free name in a server folder.
pub async fn unique_name(s: &Session, dir: &str, name: &str) -> String {
    if !exists(s, &linux_join(dir, name)).await {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 2..10_000 {
        let c = format!("{stem} ({i}){ext}");
        if !exists(s, &linux_join(dir, &c)).await {
            return c;
        }
    }
    format!("{stem} ({}){ext}", std::process::id())
}

/// Linux allows any name but `/` and NUL; Ferret also can't show a backslash.
fn check_name(name: &str) -> Res<()> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." {
        return Err("Name can't be empty".into());
    }
    if let Some(c) = n.chars().find(|c| matches!(c, '/' | '\\' | '\0')) {
        return Err(format!("A name can't contain {c:?}"));
    }
    Ok(())
}

pub async fn create_folder(dir: &str, name: &str) -> Res<String> {
    check_name(name)?;
    let (s, linux) = session_of(dir).await?;
    let name = unique_name(&s, &linux, name.trim()).await;
    let full = linux_join(&linux, &name);
    s.sftp.create_dir(full.clone()).await.map_err(|e| sftp_err(dir, e))?;
    Ok(paths::remote_path(&split(dir)?.0, &full))
}

pub async fn remove_empty_folder(p: &str) -> bool {
    match session_of(p).await {
        Ok((s, linux)) => s.sftp.remove_dir(linux).await.is_ok(),
        Err(_) => false,
    }
}

pub async fn create_file(dir: &str, name: &str, content: Option<String>) -> Res<String> {
    check_name(name)?;
    let (s, linux) = session_of(dir).await?;
    let name = unique_name(&s, &linux, name.trim()).await;
    let full = linux_join(&linux, &name);
    let mut f = s
        .sftp
        .open_with_flags(full.clone(), OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE)
        .await
        .map_err(|e| sftp_err(dir, e))?;
    if let Some(c) = content {
        f.write_all(c.as_bytes()).await.map_err(|e| e.to_string())?;
    }
    f.shutdown().await.map_err(|e| e.to_string())?;
    Ok(paths::remote_path(&split(dir)?.0, &full))
}

pub async fn rename(p: &str, new_name: &str) -> Res<String> {
    let new_name = new_name.trim();
    check_name(new_name)?;
    let (s, linux) = session_of(p).await?;
    let (dir, _) = linux.rsplit_once('/').ok_or("Can't rename the server's root")?;
    let dir = if dir.is_empty() { "/" } else { dir };
    let to = linux_join(dir, new_name);
    if to == linux {
        return Ok(paths::normalize(p));
    }
    if exists(&s, &to).await {
        return Err(format!("“{new_name}” already exists here"));
    }
    s.sftp.rename(linux.clone(), to.clone()).await.map_err(|e| sftp_err(p, e))?;
    log::info!("renamed on {}: \"{linux}\" -> \"{to}\"", s.name);
    Ok(paths::remote_path(&split(p)?.0, &to))
}

// ---------- reading, for previews ----------

pub async fn read_head(p: &str, max: u64) -> Res<(Vec<u8>, u64)> {
    let (s, linux) = session_of(p).await?;
    let mut f = s.sftp.open(linux).await.map_err(|e| sftp_err(p, e))?;
    let size = f.metadata().await.ok().and_then(|m| m.size).unwrap_or(0);
    let mut buf = Vec::with_capacity(max.min(size) as usize);
    (&mut f).take(max).read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
    Ok((buf, size))
}

/// `len` bytes from `start` (the fs:// scheme's ranges, for video); with the file's size.
pub fn read_range(p: &str, start: u64, len: u64) -> Res<(Vec<u8>, u64)> {
    block(async {
        let (s, linux) = session_of(p).await?;
        s.touch();
        let mut f = s.sftp.open(linux).await.map_err(|e| sftp_err(p, e))?;
        let size = f.metadata().await.ok().and_then(|m| m.size).unwrap_or(0);
        f.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(len.min(size.saturating_sub(start)) as usize);
        (&mut f).take(len).read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        Ok((buf, size))
    })
}

pub fn size_of(p: &str) -> Option<u64> {
    block(async {
        let (id, linux) = split(p).ok()?;
        ssh::connected(&id)?.sftp.metadata(linux).await.ok()?.size
    })
}

/// Where server files are kept for previews (and opened copies: edit.rs).
pub fn cache_dir(kind: &str, id: &str) -> Res<PathBuf> {
    let d = dirs::data_local_dir().ok_or("no local app data folder")?.join("Ferret").join(kind).join(id);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

/// A local copy of a server file for previews: kept by path, size and time, so showing it
/// again doesn't download it again. Files bigger than `max` aren't copied.
pub async fn materialize(p: &str, max: u64) -> Res<String> {
    let (s, linux) = session_of(p).await?;
    let (id, _) = split(p)?;
    let m = s.sftp.metadata(linux.clone()).await.map_err(|e| sftp_err(p, e))?;
    let size = m.size.unwrap_or(0);
    if size > max {
        return Err(format!("“{}” is too big to preview from the server ({} MB)", name_of(p), size >> 20));
    }
    let key = format!("{linux}|{size}|{}", m.mtime.unwrap_or(0));
    let dir = cache_dir("remote-cache", &id)?.join(format!("{:016x}", crate::archive::fnv1a(key.as_bytes())));
    let file = dir.join(name_of(&linux));
    if std::fs::metadata(&file).is_ok_and(|f| f.len() == size) {
        return Ok(file.to_string_lossy().into_owned());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _hold = s.hold();
    let mut src = s.sftp.open(linux).await.map_err(|e| sftp_err(p, e))?;
    let part = dir.join(".part");
    let mut out = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
    tokio::io::copy(&mut src, &mut out).await.map_err(|e| e.to_string())?;
    out.flush().await.map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(&part, &file).map_err(|e| e.to_string())?;
    Ok(file.to_string_lossy().into_owned())
}

// ---------- the server's trash ----------

/// `/home/me/Trash dir` stuff: (files, info) folders of the user's trash.
fn trash_dirs(home: &str) -> (String, String) {
    let base = linux_join(home, ".local/share/Trash");
    (linux_join(&base, "files"), linux_join(&base, "info"))
}

/// A path as .trashinfo writes it: URL-escaped, slashes kept.
fn trash_escape(p: &str) -> String {
    let mut out = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn trash_info(original: &str, when: chrono::NaiveDateTime) -> String {
    format!("[Trash Info]\nPath={}\nDeletionDate={}\n", trash_escape(original), when.format("%Y-%m-%dT%H:%M:%S"))
}

async fn mkdirs(s: &Session, dir: &str) {
    let mut cur = String::new();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        cur.push('/');
        cur.push_str(part);
        if s.sftp.metadata(cur.clone()).await.is_err() {
            let _ = s.sftp.create_dir(cur.clone()).await;
        }
    }
}

/// Moves one item to the server's trash; where it went (a Ferret path), for undo.
pub async fn trash_one(s: &Session, id: &str, linux: &str) -> Res<String> {
    let (files, info) = trash_dirs(&s.home);
    if linux == "/" || linux.starts_with(&format!("{}/", linux_join(&s.home, ".local/share/Trash"))) {
        return Err(format!("{NOT_TRASHABLE}It's in the trash already"));
    }
    mkdirs(s, &files).await;
    mkdirs(s, &info).await;
    let name = name_of(linux).to_string();
    // A free name in both folders.
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut pick = name.clone();
    for i in 2..10_000 {
        if !exists(s, &linux_join(&files, &pick)).await && !exists(s, &linux_join(&info, &format!("{pick}.trashinfo"))).await {
            break;
        }
        pick = format!("{stem}.{i}{ext}");
    }
    let info_file = linux_join(&info, &format!("{pick}.trashinfo"));
    let text = trash_info(linux, chrono::Local::now().naive_local());
    let mut f = s
        .sftp
        .open_with_flags(info_file.clone(), OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE)
        .await
        .map_err(|e| format!("{NOT_TRASHABLE}{}", sftp_err(linux, e)))?;
    f.write_all(text.as_bytes()).await.map_err(|e| e.to_string())?;
    f.shutdown().await.map_err(|e| e.to_string())?;
    let to = linux_join(&files, &pick);
    if let Err(e) = s.sftp.rename(linux.to_string(), to.clone()).await {
        let _ = s.sftp.remove_file(info_file).await;
        // Another file system than the home folder (or no right to move it).
        return Err(format!("{NOT_TRASHABLE}{}", sftp_err(linux, e)));
    }
    Ok(paths::remote_path(id, &to))
}

/// Puts a trashed item back (undo); its .trashinfo goes too.
pub async fn untrash(s: &Session, trashed: &str, original: &str) -> Res<()> {
    if exists(s, original).await {
        return Err(format!("Something named “{}” is back where it was", name_of(original)));
    }
    if let Some((dir, _)) = original.rsplit_once('/') {
        mkdirs(s, dir).await;
    }
    s.sftp.rename(trashed.to_string(), original.to_string()).await.map_err(|e| sftp_err(original, e))?;
    if let Some((files_dir, name)) = trashed.rsplit_once('/')
        && files_dir.ends_with("/Trash/files")
    {
        let info = format!("{}/info/{name}.trashinfo", files_dir.trim_end_matches("/files"));
        let _ = s.sftp.remove_file(info).await;
    }
    Ok(())
}

/// `%20` and the like back to bytes (a .trashinfo's Path=).
fn trash_unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) if b[i] == b'%' => {
                out.push(v);
                i += 3;
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Undo of something the server's trash took by its original path (a copy that replaced it,
/// like the Recycle Bin's restore): the latest deletion of each goes back. Where they are now.
pub async fn restore_by_original(items: &[String]) -> Res<Vec<String>> {
    let mut restored = Vec::new();
    for p in items {
        let (s, original) = session_of(p).await?;
        let (files, info) = trash_dirs(&s.home);
        let rd = s.sftp.read_dir(info.clone()).await.map_err(|e| sftp_err(p, e))?;
        // (deleted when, name in the trash) of each deletion of this path.
        let mut found: Vec<(String, String)> = Vec::new();
        for e in rd {
            let name = e.file_name();
            let Some(stem) = name.strip_suffix(".trashinfo") else { continue };
            let Ok(text) = s.sftp.read(linux_join(&info, &name)).await else { continue };
            let text = String::from_utf8_lossy(&text);
            let path = text.lines().find_map(|l| l.strip_prefix("Path=")).map(trash_unescape);
            let when = text.lines().find_map(|l| l.strip_prefix("DeletionDate=")).unwrap_or("").to_string();
            if path.as_deref() == Some(original.as_str()) {
                found.push((when, stem.to_string()));
            }
        }
        let (_, stem) = found.into_iter().max().ok_or_else(|| format!("“{}” is no longer in the trash on {}", name_of(&original), s.name))?;
        untrash(&s, &linux_join(&files, &stem), &original).await?;
        restored.push(paths::normalize(p));
    }
    Ok(restored)
}

/// Deletes for good: `rm -rf` where the server runs commands (fast for big folders), else
/// item by item over SFTP.
pub async fn remove_all(s: &Session, linux: &str) -> Res<()> {
    if linux == "/" || linux == s.home {
        return Err("Ferret doesn't delete a server's root or home folder".into());
    }
    if s.arch.is_some() {
        let cmd = format!("rm -rf -- {}", ssh::quote(linux));
        if s.run(&cmd, Duration::from_secs(3600)).await.is_ok() && !exists(s, linux).await {
            return Ok(());
        }
    }
    remove_tree(s, linux).await
}

async fn remove_tree(s: &Session, linux: &str) -> Res<()> {
    let m = s.sftp.symlink_metadata(linux.to_string()).await.map_err(|e| sftp_err(linux, e))?;
    if m.is_dir() {
        for e in s.sftp.read_dir(linux.to_string()).await.map_err(|e| sftp_err(linux, e))? {
            Box::pin(remove_tree(s, &linux_join(linux, &e.file_name()))).await?;
        }
        s.sftp.remove_dir(linux.to_string()).await.map_err(|e| sftp_err(linux, e))
    } else {
        s.sftp.remove_file(linux.to_string()).await.map_err(|e| sftp_err(linux, e))
    }
}

// ---------- the file system copies use ----------

/// A server as a file system for the copy engine (blocking calls, run on job threads).
pub struct RemoteFs {
    id: String,
}

impl RemoteFs {
    pub fn shared(id: &str) -> Arc<dyn Vfs> {
        Arc::new(RemoteFs { id: id.to_string() })
    }

    fn session(&self) -> std::io::Result<Arc<Session>> {
        block(ssh::session(&self.id)).map_err(std::io::Error::other)
    }

    fn linux(&self, p: &str) -> std::io::Result<String> {
        match paths::remote_parts(&paths::normalize(p)) {
            Some((id, l)) if id == self.id => Ok(l),
            _ => Err(std::io::Error::other(format!("{p} isn't on {}", self.id))),
        }
    }
}

fn meta(m: &Metadata) -> Meta {
    Meta {
        is_dir: m.is_dir(),
        len: if m.is_dir() { 0 } else { m.size.unwrap_or(0) },
        modified: m.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t as u64)),
        mode: m.permissions,
        readonly: false,
    }
}

/// A server file read with blocking calls.
struct Reader(russh_sftp::client::fs::File);
impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        block(self.0.read(buf))
    }
}

struct Writer(Option<russh_sftp::client::fs::File>);
impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let f = self.0.as_mut().ok_or_else(|| std::io::Error::other("closed"))?;
        block(f.write(buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.as_mut() {
            Some(f) => block(f.flush()),
            None => Ok(()),
        }
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(mut f) = self.0.take() {
            let _ = block(f.shutdown());
        }
    }
}

impl Vfs for RemoteFs {
    fn lstat(&self, p: &str) -> std::io::Result<Meta> {
        let (s, l) = (self.session()?, self.linux(p)?);
        block(s.sftp.symlink_metadata(l)).map(|m| meta(&m)).map_err(io_err)
    }
    fn stat(&self, p: &str) -> std::io::Result<Meta> {
        let (s, l) = (self.session()?, self.linux(p)?);
        block(s.sftp.metadata(l)).map(|m| meta(&m)).map_err(io_err)
    }
    fn read_dir(&self, p: &str) -> std::io::Result<Vec<String>> {
        let (s, l) = (self.session()?, self.linux(p)?);
        s.touch();
        let rd = block(s.sftp.read_dir(l)).map_err(io_err)?;
        Ok(rd.map(|e| e.file_name()).filter(|n| !n.contains('\\')).collect())
    }
    fn open(&self, p: &str) -> std::io::Result<Box<dyn Read + Send>> {
        let (s, l) = (self.session()?, self.linux(p)?);
        Ok(Box::new(Reader(block(s.sftp.open(l)).map_err(io_err)?)))
    }
    fn create(&self, p: &str) -> std::io::Result<Box<dyn Write + Send>> {
        let (s, l) = (self.session()?, self.linux(p)?);
        let f = block(s.sftp.open_with_flags(l, OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE)).map_err(io_err)?;
        Ok(Box::new(Writer(Some(f))))
    }
    fn mkdir(&self, p: &str) -> std::io::Result<()> {
        let (s, l) = (self.session()?, self.linux(p)?);
        block(s.sftp.create_dir(l)).map_err(io_err)
    }
    fn rename(&self, from: &str, to: &str) -> std::io::Result<()> {
        let (s, a, b) = (self.session()?, self.linux(from)?, self.linux(to)?);
        block(s.sftp.rename(a, b)).map_err(io_err)
    }
    fn remove(&self, p: &str) -> std::io::Result<()> {
        let (s, l) = (self.session()?, self.linux(p)?);
        block(remove_all(&s, &l)).map_err(std::io::Error::other)
    }
    fn finish(&self, p: &str, like: &Meta) {
        let (Ok(s), Ok(l)) = (self.session(), self.linux(p)) else { return };
        let mtime = like.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32);
        let mut m = Metadata { mtime, atime: mtime, ..Default::default() };
        // From another server: its permissions; from Windows, the server's defaults stay.
        if let Some(mode) = like.mode {
            m.permissions = Some(mode & 0o7777);
        }
        let _ = block(s.sftp.set_metadata(l, m));
    }
    fn displace(&self, p: &str) -> Result<Option<String>, String> {
        let s = self.session().map_err(|e| e.to_string())?;
        let l = self.linux(p).map_err(|e| e.to_string())?;
        // Reported by its original path, as the Recycle Bin's are: undo restores it by that.
        match block(trash_one(&s, &self.id, &l)) {
            Ok(_) => Ok(Some(paths::normalize(p))),
            Err(_) => block(remove_all(&s, &l)).map(|_| None),
        }
    }
    fn same(&self, a: &str, b: &str) -> bool {
        let id = |p: &str| paths::remote_parts(&paths::normalize(p)).map(|(i, _)| i.to_string());
        id(a).is_some_and(|x| Some(x) == id(b))
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_info_format() {
        let when = chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap().and_hms_opt(17, 5, 9).unwrap();
        assert_eq!(
            trash_info("/home/me/My Notes/ä#1.txt", when),
            "[Trash Info]\nPath=/home/me/My%20Notes/%C3%A4%231.txt\nDeletionDate=2026-10-28T17:05:09\n"
        );
        assert_eq!(trash_dirs("/home/me"), ("/home/me/.local/share/Trash/files".into(), "/home/me/.local/share/Trash/info".into()));
        assert_eq!(trash_unescape("/home/me/My%20Notes/%C3%A4%231.txt"), "/home/me/My Notes/ä#1.txt");
        assert_eq!(trash_unescape("/100%/x%2"), "/100%/x%2");
        assert_eq!(split(r"\\ssh:\home.local\srv\x").unwrap(), ("home.local".into(), "/srv/x".into()));
        assert!(check_name("a/b").is_err());
        assert!(check_name("CON.txt").is_ok());
    }
}
