//! Archive engine — browse zip / 7z / rar / tar(.gz|.xz|.zst|.bz2) and single-file
//! .gz / .xz / .zst / .bz2 archives as if they were folders, preview files inside
//! them, and extract / compress with progress.
//!
//! Paths inside archives are *virtual*: `C:\dl\foo.zip\inner\dir`. [`split_virtual`]
//! finds the real archive file in such a path and [`list_children`] lists a folder
//! inside it from an in-memory index (zip / 7z / rar only read the central directory
//! or headers; tar streams are scanned once). Indexes are cached per
//! (archive path, mtime, size) in a small LRU. Single entries are materialized into
//! `%LOCALAPPDATA%\Ferret\archive-cache\<hash>\…` for previews and "open with".
//!
//! Entry names are sanitized when the index is built: entries with `..`, absolute
//! paths or drive letters are dropped (zip-slip), and characters Windows can't store
//! are replaced, so every name shown in a listing is also a safe file name.

use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use crate::jobs::{self, JobCtx};
use crate::listing::{self, Entry};
use crate::paths;

/// Error string returned when an encrypted archive needs a password.
pub const PASSWORD_REQUIRED: &str = "password-required";
/// Error string returned when the supplied password is wrong.
pub const WRONG_PASSWORD: &str = "wrong-password";
const NOT_FOUND: &str = "Not found in archive";
const NOT_ARCHIVE: &str = "Not an archive";
const CANCELLED: &str = "Cancelled";
const COPY_BUF: usize = 256 * 1024;
const INDEX_CACHE_CAP: usize = if cfg!(test) { 64 } else { 8 };
const CACHE_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

// ───────────────────────────── archive kinds ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    Zip,
    SevenZ,
    Rar,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    /// Single compressed file (one entry = file name minus the extension).
    Gz,
    Xz,
    Zst,
    Bz2,
}

impl ArchiveKind {
    pub fn label(self) -> &'static str {
        use ArchiveKind::*;
        match self {
            Zip => "ZIP",
            SevenZ => "7Z",
            Rar => "RAR",
            Tar => "TAR",
            TarGz => "TAR.GZ",
            TarXz => "TAR.XZ",
            TarZst => "TAR.ZST",
            TarBz2 => "TAR.BZ2",
            Gz => "GZ",
            Xz => "XZ",
            Zst => "ZST",
            Bz2 => "BZ2",
        }
    }

    fn is_tar(self) -> bool {
        use ArchiveKind::*;
        matches!(self, Tar | TarGz | TarXz | TarZst | TarBz2)
    }

    fn is_single(self) -> bool {
        use ArchiveKind::*;
        matches!(self, Gz | Xz | Zst | Bz2)
    }
}

const ZIP_EXTS: &[&str] = &["zip", "jar", "apk", "cbz", "xpi", "nupkg", "vsix", "whl", "epub"];

fn file_name_of(path: &str) -> &str {
    let t = path.trim_end_matches(['\\', '/']);
    t.rsplit(['\\', '/']).next().unwrap_or(t)
}

/// Archive kind by file extension (case-insensitive), `None` for other files.
pub fn archive_kind(path: &str) -> Option<ArchiveKind> {
    use ArchiveKind::*;
    let name = file_name_of(path).to_ascii_lowercase();
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let tarred = stem.len() > 4 && stem.ends_with(".tar");
    Some(match ext {
        "7z" => SevenZ,
        "rar" | "cbr" => Rar,
        "tar" => Tar,
        "tgz" => TarGz,
        "txz" => TarXz,
        "tzst" => TarZst,
        "tbz2" | "tbz" => TarBz2,
        "gz" if tarred => TarGz,
        "xz" if tarred => TarXz,
        "zst" if tarred => TarZst,
        "bz2" if tarred => TarBz2,
        "gz" => Gz,
        "xz" => Xz,
        "zst" => Zst,
        "bz2" => Bz2,
        e if ZIP_EXTS.contains(&e) => Zip,
        _ => return None,
    })
}

/// `foo.tar.gz` → `foo`, `foo.zip` → `foo` (name for "extract here" folders).
fn archive_stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for suf in [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2"] {
        if lower.len() > suf.len() && lower.ends_with(suf) {
            return name[..name.len() - suf.len()].to_string();
        }
    }
    match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s.to_string(),
        _ => name.to_string(),
    }
}

fn ext_of(name: &str) -> String {
    name.rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

// ───────────────────────────── progress ─────────────────────────────

/// Progress sink for extraction / compression; implemented by [`JobCtx`] and by
/// test doubles so the work functions don't need Tauri.
pub trait Progress {
    /// Err("Cancelled") once the user cancelled.
    fn check(&self) -> Result<(), String>;
    fn set_totals(&self, bytes: u64, files: u64);
    fn set_current(&self, current: &str);
    fn add_bytes(&self, n: u64);
    fn file_done(&self);
    fn created(&self, path: &str);
}

impl Progress for JobCtx {
    fn check(&self) -> Result<(), String> {
        JobCtx::check(self)
    }
    fn set_totals(&self, bytes: u64, files: u64) {
        JobCtx::set_totals(self, bytes, files)
    }
    fn set_current(&self, current: &str) {
        JobCtx::set_current(self, current)
    }
    fn add_bytes(&self, n: u64) {
        JobCtx::add_bytes(self, n)
    }
    fn file_done(&self) {
        JobCtx::file_done(self)
    }
    fn created(&self, path: &str) {
        JobCtx::created(self, path)
    }
}

struct NoProgress;

impl Progress for NoProgress {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn set_totals(&self, _: u64, _: u64) {}
    fn set_current(&self, _: &str) {}
    fn add_bytes(&self, _: u64) {}
    fn file_done(&self) {}
    fn created(&self, _: &str) {}
}

// ───────────────────────────── small helpers ─────────────────────────────

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn io_err(e: io::Error) -> String {
    e.to_string()
}

fn path_err(e: io::Error, p: &Path) -> String {
    format!("{}: {e}", p.display())
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn system_to_ms(t: SystemTime) -> Option<i64> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Some(d.as_millis() as i64),
        Err(e) => Some(-(e.duration().as_millis() as i64)),
    }
}

fn ms_to_system(ms: i64) -> SystemTime {
    if ms >= 0 {
        UNIX_EPOCH + Duration::from_millis(ms as u64)
    } else {
        UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
    }
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn thread_count() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

/// Converts DOS local wall-clock stamps (zip, rar) to epoch ms. The local UTC
/// offset is cached per (date, hour) so big archives don't hit the tz database
/// for every entry.
#[derive(Default)]
struct LocalClock {
    offsets: HashMap<(u16, u16), i64>,
}

impl LocalClock {
    fn dos(&mut self, date: u16, time: u16) -> Option<i64> {
        let year = 1980 + (date >> 9) as i32;
        let month = ((date >> 5) & 0xF) as u32;
        let day = (date & 0x1F) as u32;
        let hour = (time >> 11) as u32;
        let min = ((time >> 5) & 0x3F) as u32;
        let sec = ((time & 0x1F) as u32 * 2).min(59);
        let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, min, sec)?;
        let off = *self
            .offsets
            .entry((date, hour as u16))
            .or_insert_with(|| local_offset_ms(&naive));
        Some(naive.and_utc().timestamp_millis() - off)
    }
}

fn local_offset_ms(naive: &NaiveDateTime) -> i64 {
    let off = |n: &NaiveDateTime| {
        Local
            .from_local_datetime(n)
            .earliest()
            .map(|dt| dt.offset().local_minus_utc() as i64 * 1000)
    };
    // Times inside a DST gap don't exist locally; use the offset an hour later.
    off(naive)
        .or_else(|| off(&(*naive + chrono::Duration::hours(1))))
        .unwrap_or(0)
}

/// Local time → zip DOS datetime (zip stores local wall-clock time).
fn zip_datetime(t: SystemTime) -> zip::DateTime {
    let dt: chrono::DateTime<Local> = t.into();
    let year = dt.year();
    if !(1980..=2107).contains(&year) {
        return zip::DateTime::default();
    }
    zip::DateTime::from_date_and_time(
        year as u16,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
        dt.second() as u8,
    )
    .unwrap_or_default()
}

// ───────────────────────────── name sanitizing ─────────────────────────────

const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Splits a raw entry name into safe path components. `None` = unsafe entry
/// (`..`, absolute path, drive letter) that must never be listed or extracted.
fn sanitize_inner(raw: &str) -> Option<Vec<String>> {
    let s = raw.replace('\\', "/");
    if s.starts_with('/') {
        return None;
    }
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return None;
    }
    let mut out = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => out.push(sanitize_component(c)),
        }
    }
    Some(out)
}

/// Makes one component storable on NTFS: no `<>:"|?*` / control chars (a `:` would
/// otherwise create an alternate data stream), no trailing dots/spaces, no device names.
fn sanitize_component(c: &str) -> String {
    let mut s: String = c
        .chars()
        .map(|ch| {
            if (ch as u32) < 0x20 || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let keep = s.trim_end_matches(['.', ' ']).len();
    if keep < s.len() {
        let n = s.len() - keep;
        s.truncate(keep);
        s.push_str(&"_".repeat(n));
    }
    let stem = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    s
}

// ───────────────────────────── index ─────────────────────────────

struct Node {
    name: String,
    /// Inner path with `/`, "" for the root.
    path: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
    encrypted: bool,
    /// Position of the entry in the archive (zip index, 7z file index, rar/tar
    /// ordinal). `None` for synthesized directories.
    src: Option<usize>,
    parent: usize,
    children: Vec<usize>,
}

struct Index {
    kind: ArchiveKind,
    nodes: Vec<Node>,
    by_path: HashMap<String, usize>,
    by_lower: OnceLock<HashMap<String, usize>>,
    comment: Option<String>,
    /// Any entry (or the header) is encrypted.
    encrypted: bool,
    /// Unsafe entries dropped while indexing.
    skipped: usize,
}

impl Index {
    fn new(kind: ArchiveKind, root_name: &str) -> Index {
        Index {
            kind,
            nodes: vec![Node {
                name: root_name.to_string(),
                path: String::new(),
                is_dir: true,
                size: 0,
                modified: None,
                encrypted: false,
                src: None,
                parent: 0,
                children: Vec::new(),
            }],
            by_path: HashMap::new(),
            by_lower: OnceLock::new(),
            comment: None,
            encrypted: false,
            skipped: 0,
        }
    }

    /// Adds one archive entry, synthesizing missing parent directories.
    fn add(&mut self, raw: &str, is_dir: bool, size: u64, modified: Option<i64>, encrypted: bool, src: usize) {
        let Some(comps) = sanitize_inner(raw) else {
            self.skipped += 1;
            return;
        };
        if comps.is_empty() {
            return;
        }
        let file_encrypted = encrypted && !is_dir;
        self.encrypted |= file_encrypted;
        let last = comps.len() - 1;
        let mut parent = 0usize;
        let mut path = String::with_capacity(raw.len());
        for (i, comp) in comps.into_iter().enumerate() {
            if i > 0 {
                path.push('/');
            }
            path.push_str(&comp);
            let leaf = i == last;
            let id = match self.by_path.get(path.as_str()) {
                Some(&id) => {
                    let n = &mut self.nodes[id];
                    if !leaf {
                        if !n.is_dir {
                            n.is_dir = true;
                            n.size = 0;
                            n.encrypted = false;
                            n.src = None;
                        }
                    } else if is_dir {
                        n.is_dir = true;
                        n.size = 0;
                        n.encrypted = false;
                        if modified.is_some() {
                            n.modified = modified;
                        }
                    } else if n.children.is_empty() {
                        // Duplicate file entry: the later one wins (like tar does).
                        n.is_dir = false;
                        n.size = size;
                        n.modified = modified;
                        n.encrypted = file_encrypted;
                        n.src = Some(src);
                    }
                    id
                }
                None => {
                    let id = self.nodes.len();
                    let file = leaf && !is_dir;
                    self.nodes.push(Node {
                        name: comp,
                        path: path.clone(),
                        is_dir: !file,
                        size: if file { size } else { 0 },
                        modified: if leaf { modified } else { None },
                        encrypted: file && file_encrypted,
                        src: if leaf { Some(src) } else { None },
                        parent,
                        children: Vec::new(),
                    });
                    self.nodes[parent].children.push(id);
                    self.by_path.insert(path.clone(), id);
                    id
                }
            };
            parent = id;
        }
    }

    /// Node for an inner path (`a/b`, "" = root). Exact match first, then
    /// case-insensitive (Windows users type paths in any case).
    fn find(&self, inner: &str) -> Option<usize> {
        let norm = inner.replace('\\', "/");
        let t = norm.trim_matches('/');
        if t.is_empty() {
            return Some(0);
        }
        if let Some(&id) = self.by_path.get(t) {
            return Some(id);
        }
        let lower = self.by_lower.get_or_init(|| {
            let mut m = HashMap::with_capacity(self.nodes.len());
            for (id, n) in self.nodes.iter().enumerate().skip(1) {
                m.entry(n.path.to_lowercase()).or_insert(id);
            }
            m
        });
        lower.get(&t.to_lowercase()).copied()
    }

    /// First encrypted file in the given subtrees.
    fn first_encrypted(&self, items: &[usize]) -> Option<usize> {
        if !self.encrypted {
            return None;
        }
        let mut stack: Vec<usize> = items.to_vec();
        while let Some(id) = stack.pop() {
            let n = &self.nodes[id];
            if n.is_dir {
                stack.extend(n.children.iter().copied());
            } else if n.encrypted {
                return Some(id);
            }
        }
        None
    }

    fn entry(&self, id: usize, vdir: &str) -> Entry {
        let n = &self.nodes[id];
        Entry {
            name: n.name.clone(),
            path: listing::join(vdir, &n.name),
            is_dir: n.is_dir,
            is_symlink: false,
            hidden: false,
            size: if n.is_dir { 0 } else { n.size },
            modified: n.modified,
            created: None,
            ext: if n.is_dir { String::new() } else { ext_of(&n.name) },
            in_archive: true,
            origin: None,
        }
    }
}

fn build_index(path: &str, kind: ArchiveKind, pw: Option<&str>) -> Result<Index, String> {
    let root = file_name_of(path);
    match kind {
        ArchiveKind::Zip => index_zip(path, root),
        ArchiveKind::SevenZ => index_7z(path, root, pw),
        ArchiveKind::Rar => index_rar(path, root, pw),
        k if k.is_tar() => index_tar(path, root, k),
        k => index_single(path, root, k),
    }
}

// ── zip ──

fn zip_err(e: zip::result::ZipError, has_pw: bool) -> String {
    use zip::result::ZipError;
    match e {
        ZipError::UnsupportedArchive(m) if m == ZipError::PASSWORD_REQUIRED => PASSWORD_REQUIRED.into(),
        ZipError::InvalidPassword if has_pw => WRONG_PASSWORD.into(),
        ZipError::InvalidPassword => PASSWORD_REQUIRED.into(),
        ZipError::Io(e) => e.to_string(),
        ZipError::InvalidArchive(m) => format!("Damaged or unsupported zip archive ({m})"),
        e => e.to_string(),
    }
}

fn open_zip(path: &str) -> Result<zip::ZipArchive<BufReader<File>>, String> {
    let f = File::open(path).map_err(io_err)?;
    zip::ZipArchive::new(BufReader::with_capacity(64 * 1024, f)).map_err(|e| zip_err(e, false))
}

fn index_zip(path: &str, root: &str) -> Result<Index, String> {
    let zip = open_zip(path)?;
    let mut idx = Index::new(ArchiveKind::Zip, root);
    let mut clock = LocalClock::default();
    for i in 0..zip.len() {
        let e = zip.by_index_data(i).map_err(|e| zip_err(e, false))?;
        let name = match e.name() {
            Ok(n) => n.into_owned(),
            Err(_) => String::from_utf8_lossy(e.name_raw()).into_owned(),
        };
        let is_dir = e.is_dir() || name.ends_with('/') || name.ends_with('\\');
        let modified = e.last_modified().and_then(|d| clock.dos(d.datepart(), d.timepart()));
        idx.add(&name, is_dir, e.size(), modified, e.encrypted(), i);
    }
    let comment = zip.comment();
    if !comment.is_empty() {
        idx.comment = Some(String::from_utf8_lossy(comment).into_owned());
    }
    Ok(idx)
}

// ── 7z ──

fn sz_err(e: sevenz_rust2::Error, has_pw: bool) -> String {
    use sevenz_rust2::Error as E;
    match e {
        E::PasswordRequired | E::MaybeBadPassword(_) => {
            if has_pw { WRONG_PASSWORD } else { PASSWORD_REQUIRED }.into()
        }
        E::Io(e, _) | E::FileOpen(e, _) => e.to_string(),
        E::BadSignature(_) => "Not a 7z archive".into(),
        E::UnsupportedCompressionMethod(m) => format!("Unsupported 7z compression method: {m}"),
        e => format!("Damaged or unsupported 7z archive ({e})"),
    }
}

fn sz_password(pw: Option<&str>) -> sevenz_rust2::Password {
    pw.map(sevenz_rust2::Password::new).unwrap_or_else(sevenz_rust2::Password::empty)
}

fn read_7z(path: &str, pw: Option<&str>) -> Result<(BufReader<File>, sevenz_rust2::Archive), String> {
    let mut f = BufReader::with_capacity(64 * 1024, File::open(path).map_err(io_err)?);
    match sevenz_rust2::Archive::read(&mut f, &sz_password(pw)) {
        Ok(a) => Ok((f, a)),
        Err(e) => {
            if pw.is_some() && !matches!(e, sevenz_rust2::Error::Io(..)) {
                // A wrong key garbles an encrypted header in many ways; it's a password
                // problem if the archive demands one.
                let plain = sevenz_rust2::Archive::read(&mut f, &sevenz_rust2::Password::empty());
                if matches!(plain, Err(sevenz_rust2::Error::PasswordRequired)) {
                    return Err(WRONG_PASSWORD.into());
                }
            }
            Err(sz_err(e, pw.is_some()))
        }
    }
}

fn block_encrypted(b: &sevenz_rust2::Block) -> bool {
    b.coders
        .iter()
        .any(|c| c.encoder_method_id() == sevenz_rust2::EncoderMethod::ID_AES256_SHA256)
}

fn index_7z(path: &str, root: &str, pw: Option<&str>) -> Result<Index, String> {
    let (_, a) = read_7z(path, pw)?;
    let mut idx = Index::new(ArchiveKind::SevenZ, root);
    for (i, e) in a.files.iter().enumerate() {
        if e.is_anti_item {
            continue;
        }
        let encrypted = a
            .stream_map
            .file_block_index
            .get(i)
            .copied()
            .flatten()
            .and_then(|b| a.blocks.get(b))
            .is_some_and(block_encrypted);
        let modified = if e.has_last_modified_date {
            system_to_ms(e.last_modified_date.into())
        } else {
            None
        };
        idx.add(&e.name, e.is_directory, e.size, modified, encrypted, i);
    }
    Ok(idx)
}

// ── rar ──

fn rar_err(e: unrar::error::UnrarError, has_pw: bool, encrypted: bool) -> String {
    use unrar::error::Code;
    match e.code {
        Code::MissingPassword => PASSWORD_REQUIRED.into(),
        Code::BadPassword => WRONG_PASSWORD.into(),
        // RAR 4 has no password check value: a wrong key shows up as bad data.
        Code::BadData if encrypted && has_pw => WRONG_PASSWORD.into(),
        Code::BadData if encrypted => PASSWORD_REQUIRED.into(),
        _ => e.to_string(),
    }
}

fn rar_archive<'a>(path: &'a str, pw: Option<&'a str>) -> unrar::Archive<'a> {
    match pw {
        Some(p) => unrar::Archive::with_password(path, p.as_bytes()),
        None => unrar::Archive::new(path),
    }
}

fn index_rar(path: &str, root: &str, pw: Option<&str>) -> Result<Index, String> {
    let has_pw = pw.is_some();
    let list = rar_archive(path, pw)
        .open_for_listing()
        .map_err(|e| rar_err(e, has_pw, true))?;
    let header_encrypted = list.has_encrypted_headers();
    let mut idx = Index::new(ArchiveKind::Rar, root);
    idx.encrypted = header_encrypted;
    let mut clock = LocalClock::default();
    for (i, h) in list.enumerate() {
        let h = h.map_err(|e| rar_err(e, has_pw, header_encrypted))?;
        let name = h.filename.to_string_lossy().into_owned();
        let modified = clock.dos((h.file_time >> 16) as u16, (h.file_time & 0xFFFF) as u16);
        idx.add(&name, h.is_directory(), h.unpacked_size, modified, h.is_encrypted(), i);
    }
    Ok(idx)
}

// ── tar & single-file streams ──

fn open_stream(path: &str, kind: ArchiveKind) -> Result<Box<dyn Read>, String> {
    use ArchiveKind::*;
    let r = BufReader::with_capacity(256 * 1024, File::open(path).map_err(io_err)?);
    Ok(match kind {
        TarGz | Gz => Box::new(flate2::bufread::MultiGzDecoder::new(r)),
        TarXz | Xz => Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(r)),
        TarZst | Zst => Box::new(zstd::stream::read::Decoder::with_buffer(r).map_err(io_err)?),
        TarBz2 | Bz2 => Box::new(bzip2::bufread::MultiBzDecoder::new(r)),
        _ => Box::new(r),
    })
}

fn damaged(e: io::Error) -> String {
    format!("Damaged archive ({e})")
}

fn index_tar(path: &str, root: &str, kind: ArchiveKind) -> Result<Index, String> {
    let mut idx = Index::new(kind, root);
    if kind == ArchiveKind::Tar {
        let f = BufReader::with_capacity(64 * 1024, File::open(path).map_err(io_err)?);
        let mut ar = tar::Archive::new(f);
        scan_tar(ar.entries_with_seek().map_err(damaged)?, &mut idx)?;
    } else {
        let mut ar = tar::Archive::new(open_stream(path, kind)?);
        scan_tar(ar.entries().map_err(damaged)?, &mut idx)?;
    }
    Ok(idx)
}

fn scan_tar<R: Read>(entries: tar::Entries<'_, R>, idx: &mut Index) -> Result<(), String> {
    for (i, e) in entries.enumerate() {
        let e = e.map_err(damaged)?;
        let et = e.header().entry_type();
        let name = String::from_utf8_lossy(&e.path_bytes()).into_owned();
        let is_dir = et.is_dir() || (et.is_file() && name.ends_with('/'));
        if !is_dir && !matches!(et, tar::EntryType::Regular | tar::EntryType::Continuous) {
            continue; // links, devices, fifos
        }
        let modified = e.header().mtime().ok().map(|s| (s as i64).saturating_mul(1000));
        idx.add(&name, is_dir, e.size(), modified, false, i);
    }
    Ok(())
}

fn index_single(path: &str, root: &str, kind: ArchiveKind) -> Result<Index, String> {
    let meta = fs::metadata(path).map_err(io_err)?;
    let inner = match root.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => "data",
    };
    let size = single_size(path, kind, meta.len()).unwrap_or(0);
    let mut idx = Index::new(kind, root);
    idx.add(inner, false, size, meta.modified().ok().and_then(system_to_ms), false, 0);
    Ok(idx)
}

/// Uncompressed size of a single-file stream, from its trailer / frame header when
/// the format records it, otherwise by decoding small files.
fn single_size(path: &str, kind: ArchiveKind, file_len: u64) -> Option<u64> {
    let parsed = (|| {
        let mut f = File::open(path).ok()?;
        match kind {
            ArchiveKind::Gz if file_len >= 18 => {
                f.seek(SeekFrom::End(-4)).ok()?;
                let mut b = [0u8; 4];
                f.read_exact(&mut b).ok()?;
                Some(u32::from_le_bytes(b) as u64)
            }
            ArchiveKind::Zst => {
                let mut b = [0u8; 18];
                let mut n = 0;
                while n < b.len() {
                    match f.read(&mut b[n..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => n += k,
                    }
                }
                zstd::zstd_safe::get_frame_content_size(&b[..n]).ok().flatten()
            }
            ArchiveKind::Xz => xz_size(&mut f, file_len),
            _ => None,
        }
    })();
    parsed.or_else(|| {
        if file_len > 8 << 20 {
            return None;
        }
        let mut r = open_stream(path, kind).ok()?;
        io::copy(&mut r, &mut io::sink()).ok()
    })
}

fn read_vli(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for i in 0..9 {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7F) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Sums the uncompressed sizes recorded in the xz index of every stream.
fn xz_size(f: &mut File, len: u64) -> Option<u64> {
    let mut end = len;
    let mut total = 0u64;
    while end > 0 {
        if end < 24 {
            return None;
        }
        let mut footer = [0u8; 12];
        f.seek(SeekFrom::Start(end - 12)).ok()?;
        f.read_exact(&mut footer).ok()?;
        if &footer[10..12] != b"YZ" {
            if footer[8..12] == [0, 0, 0, 0] {
                end -= 4; // stream padding
                continue;
            }
            return None;
        }
        let backward = (u32::from_le_bytes(footer[4..8].try_into().ok()?) as u64 + 1) * 4;
        if backward > 64 << 20 {
            return None;
        }
        let index_start = (end - 12).checked_sub(backward)?;
        let mut index = vec![0u8; backward as usize];
        f.seek(SeekFrom::Start(index_start)).ok()?;
        f.read_exact(&mut index).ok()?;
        if index.first() != Some(&0) {
            return None;
        }
        let mut pos = 1;
        let records = read_vli(&index, &mut pos)?;
        let mut blocks = 0u64;
        for _ in 0..records {
            let unpadded = read_vli(&index, &mut pos)?;
            let uncompressed = read_vli(&index, &mut pos)?;
            total = total.checked_add(uncompressed)?;
            blocks = blocks.checked_add(unpadded.checked_add(3)? & !3)?;
        }
        end = index_start.checked_sub(blocks)?.checked_sub(12)?;
    }
    Some(total)
}

// ───────────────────────────── index cache & passwords ─────────────────────────────

#[derive(Clone, PartialEq, Eq, Debug)]
struct ArcKey {
    path: String,
    mtime: Option<SystemTime>,
    size: u64,
}

fn arc_key(path: &str) -> Result<ArcKey, String> {
    let m = fs::metadata(path).map_err(io_err)?;
    if !m.is_file() {
        return Err(NOT_ARCHIVE.into());
    }
    Ok(ArcKey { path: paths::key(path), mtime: m.modified().ok(), size: m.len() })
}

struct Cached {
    key: ArcKey,
    index: Arc<Index>,
}

fn index_cache() -> &'static Mutex<Vec<Cached>> {
    static C: OnceLock<Mutex<Vec<Cached>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Parsed index of an archive file, from the LRU cache when the file is unchanged.
fn load_index(path: &str, pw: Option<&str>) -> Result<(ArcKey, Arc<Index>), String> {
    let kind = archive_kind(path).ok_or(NOT_ARCHIVE)?;
    let key = arc_key(path)?;
    {
        let mut c = lock(index_cache());
        if let Some(pos) = c.iter().position(|e| e.key == key) {
            let hit = c.remove(pos);
            let idx = hit.index.clone();
            c.insert(0, hit);
            return Ok((key, idx));
        }
    }
    let pw = pw.map(str::to_string).or_else(|| recall_password(&key.path));
    let idx = Arc::new(build_index(path, kind, pw.as_deref())?);
    let mut c = lock(index_cache());
    c.retain(|e| e.key.path != key.path);
    c.insert(0, Cached { key: key.clone(), index: idx.clone() });
    c.truncate(INDEX_CACHE_CAP);
    Ok((key, idx))
}

fn password_store() -> &'static Mutex<HashMap<String, String>> {
    static P: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Passwords that worked are remembered for the session (per archive file), so
/// browsing an archive with encrypted headers keeps working after unlocking it.
fn remember_password(archive: &str, pw: &str) {
    lock(password_store()).insert(paths::key(archive), pw.to_string());
}

fn recall_password(archive: &str) -> Option<String> {
    lock(password_store()).get(&paths::key(archive)).cloned()
}

fn effective_password(archive: &str, pw: Option<&str>) -> Option<String> {
    pw.filter(|p| !p.is_empty()).map(str::to_string).or_else(|| recall_password(archive))
}

// ───────────────────────────── virtual paths ─────────────────────────────

/// Splits a virtual path into (archive file, inner path with `/`). With
/// `descend_last`, a final component that is itself an archive inside the archive
/// is materialized and returned as the archive (browse into nested archives);
/// without it the nested archive is returned as an inner *file*.
fn split_impl(path: &str, descend_last: bool) -> Option<(String, String)> {
    let p = paths::normalize(path);
    let unc = p.starts_with(r"\\");
    let comps: Vec<&str> = p.split('\\').filter(|c| !c.is_empty()).collect();
    let root_len = if unc { 2 } else { 1 };
    if comps.len() <= root_len {
        return None;
    }
    let mut cur = if unc {
        format!(r"\\{}\{}", comps[0], comps[1])
    } else if comps[0].ends_with(':') {
        format!("{}\\", comps[0])
    } else {
        comps[0].to_string()
    };
    for i in root_len..comps.len() {
        cur = listing::join(&cur, comps[i]);
        if archive_kind(comps[i]).is_none() {
            continue; // only archive-looking components are worth a stat
        }
        match fs::metadata(&cur) {
            Ok(m) if m.is_file() => return Some(descend(cur, &comps[i + 1..], descend_last)),
            Ok(_) => continue, // a folder named like an archive
            Err(_) => return None,
        }
    }
    None
}

fn descend(archive: String, rest: &[&str], descend_last: bool) -> (String, String) {
    for j in 0..rest.len() {
        if archive_kind(rest[j]).is_none() {
            continue;
        }
        if j + 1 == rest.len() && !descend_last {
            break;
        }
        let Ok((key, idx)) = load_index(&archive, None) else { break };
        let Some(id) = idx.find(&rest[..=j].join("/")) else { break };
        if idx.nodes[id].is_dir {
            continue;
        }
        let pw = recall_password(&archive);
        match materialize_node(&archive, &key, &idx, id, pw.as_deref()) {
            Ok(real) => return descend(path_str(&real), &rest[j + 1..], descend_last),
            Err(_) => break,
        }
    }
    (archive, rest.join("/"))
}

/// For a virtual path like `C:\dl\foo.zip\inner\dir`, returns
/// `("C:\dl\foo.zip", "inner/dir")`; `(path, "")` when `path` is an archive file;
/// `None` for ordinary paths. Only path components with an archive extension are
/// stat'ed. An archive nested inside an archive is materialized to the cache and
/// the returned archive path points at that copy.
pub fn split_virtual(path: &str) -> Option<(String, String)> {
    split_impl(path, true)
}

/// Immediate children of a folder inside an archive (`C:\a.zip\dir`). Directories
/// missing from the archive (common in zips) are synthesized.
#[allow(dead_code)] // called from the listing commands
pub fn list_children(virtual_dir: &str) -> Result<Vec<Entry>, String> {
    let vdir = paths::normalize(virtual_dir);
    let (arc, inner) = split_virtual(&vdir).ok_or(NOT_ARCHIVE)?;
    let (_, idx) = load_index(&arc, None)?;
    let id = idx.find(&inner).ok_or(NOT_FOUND)?;
    let node = &idx.nodes[id];
    if !node.is_dir {
        return Err(NOT_FOUND.into());
    }
    Ok(node.children.iter().map(|&c| idx.entry(c, &vdir)).collect())
}

// ───────────────────────────── extraction engine ─────────────────────────────

struct Task {
    node: usize,
    target: PathBuf,
}

#[derive(Default)]
struct Plan {
    dirs: Vec<PathBuf>,
    files: Vec<Task>,
    bytes: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// User extraction: never overwrite, pick `name (2).ext` instead.
    Extract,
    /// Archive cache: write to a temp file, then replace the target.
    Cache,
}

fn plan_node(idx: &Index, id: usize, target: PathBuf, plan: &mut Plan) {
    let n = &idx.nodes[id];
    if n.is_dir {
        for &c in &n.children {
            plan_node(idx, c, target.join(&idx.nodes[c].name), plan);
        }
        plan.dirs.push(target);
    } else {
        plan.bytes += n.size;
        plan.files.push(Task { node: id, target });
    }
}

/// `foo.txt` → `foo (n).txt`; folders get the suffix at the end.
fn numbered(p: &Path, n: u32, is_dir: bool) -> PathBuf {
    let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !is_dir && !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    p.with_file_name(format!("{stem} ({n}){ext}"))
}

fn unique_path(p: &Path, is_dir: bool, taken: &mut HashSet<String>) -> PathBuf {
    let free = |q: &Path, taken: &HashSet<String>| {
        fs::symlink_metadata(q).is_err() && !taken.contains(&paths::key(&path_str(q)))
    };
    let mut cand = p.to_path_buf();
    let mut n = 2;
    while !free(&cand, taken) && n < 100_000 {
        cand = numbered(p, n, is_dir);
        n += 1;
    }
    taken.insert(paths::key(&path_str(&cand)));
    cand
}

/// Plans extracting `items` into `dest`, giving each top-level item a free name.
/// Returns the plan and the top-level paths it will create.
fn plan_items(idx: &Index, items: &[usize], dest: &Path) -> (Plan, Vec<PathBuf>) {
    let mut plan = Plan::default();
    let mut taken = HashSet::new();
    let mut tops = Vec::new();
    for &id in items {
        let n = &idx.nodes[id];
        let top = unique_path(&dest.join(&n.name), n.is_dir, &mut taken);
        plan_node(idx, id, top.clone(), &mut plan);
        tops.push(top);
    }
    (plan, tops)
}

fn part_path(target: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!("{name}.{}-{}.part", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

fn create_unique_file(target: &Path) -> Result<(File, PathBuf), String> {
    let mut cand = target.to_path_buf();
    for n in 2..100_000u32 {
        match OpenOptions::new().write(true).create_new(true).open(&cand) {
            Ok(f) => return Ok((f, cand)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => cand = numbered(target, n, false),
            Err(e) => return Err(path_err(e, &cand)),
        }
    }
    Err(format!("{}: too many name conflicts", target.display()))
}

/// Opens the file a task writes to: the target itself (Extract) or a temp file next
/// to it (Cache). Returns the handle and the path actually written.
fn open_target(target: &Path, mode: Mode) -> Result<(File, PathBuf), String> {
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(|e| path_err(e, dir))?;
    }
    match mode {
        Mode::Extract => create_unique_file(target),
        Mode::Cache => {
            let tmp = part_path(target);
            let f = File::create(&tmp).map_err(|e| path_err(e, &tmp))?;
            Ok((f, tmp))
        }
    }
}

fn finish_target(written: &Path, target: &Path, mode: Mode) -> Result<(), String> {
    if mode == Mode::Cache && written != target {
        if let Err(e) = fs::rename(written, target) {
            let _ = fs::remove_file(written);
            // Another materialize of the same entry may have won the race (or the
            // existing copy is open in a viewer): keep the copy that's there.
            if !target.is_file() {
                return Err(path_err(e, target));
            }
        }
    }
    Ok(())
}

fn set_file_mtime(path: &Path, ms: i64) {
    if let Ok(f) = OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(ms_to_system(ms));
    }
}

/// Reads and discards an entry; Ok(false) if cancelled midway.
fn drain(r: &mut dyn Read, p: &dyn Progress, buf: &mut [u8]) -> io::Result<bool> {
    loop {
        if p.check().is_err() {
            return Ok(false);
        }
        match r.read(buf) {
            Ok(0) => return Ok(true),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

struct Extractor<'a> {
    idx: &'a Index,
    p: &'a dyn Progress,
    mode: Mode,
    buf: Vec<u8>,
}

impl Extractor<'_> {
    /// Streams one entry to its target. `pw_suspect`: a read error means the
    /// password was wrong (encrypted entry, password supplied).
    fn write(&mut self, r: &mut dyn Read, t: &Task, pw_suspect: bool) -> Result<(), String> {
        let idx = self.idx;
        let node = &idx.nodes[t.node];
        self.p.check()?;
        self.p.set_current(&node.name);
        let (mut file, written) = open_target(&t.target, self.mode)?;
        let mut res = Ok(());
        loop {
            if let Err(e) = self.p.check() {
                res = Err(e);
                break;
            }
            let n = match r.read(&mut self.buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    res = Err(if pw_suspect {
                        WRONG_PASSWORD.to_string()
                    } else {
                        format!("Couldn't read {} from the archive ({e})", node.name)
                    });
                    break;
                }
            };
            if let Err(e) = file.write_all(&self.buf[..n]) {
                res = Err(path_err(e, &written));
                break;
            }
            self.p.add_bytes(n as u64);
        }
        if res.is_ok() {
            if let Some(ms) = node.modified {
                let _ = file.set_modified(ms_to_system(ms));
            }
        }
        drop(file);
        match res {
            Err(e) => {
                let _ = fs::remove_file(&written);
                Err(e)
            }
            Ok(()) => {
                finish_target(&written, &t.target, self.mode)?;
                self.p.file_done();
                Ok(())
            }
        }
    }

    fn zip(&mut self, archive: &str, plan: &Plan, pw: Option<&str>) -> Result<(), String> {
        let idx = self.idx;
        let mut zip = open_zip(archive)?;
        let mut tasks: Vec<&Task> = plan.files.iter().collect();
        tasks.sort_by_key(|t| idx.nodes[t.node].src);
        for t in tasks {
            self.p.check()?;
            let node = &idx.nodes[t.node];
            let Some(src) = node.src else { continue };
            let opts = zip::ZipReadOptions::new().password(pw.map(str::as_bytes));
            let mut f = zip.by_index_with_options(src, opts).map_err(|e| zip_err(e, pw.is_some()))?;
            self.write(&mut f, t, node.encrypted && pw.is_some())?;
        }
        Ok(())
    }

    fn sevenz(&mut self, archive: &str, plan: &Plan, pw: Option<&str>) -> Result<(), String> {
        let idx = self.idx;
        let has_pw = pw.is_some();
        let (mut src, a) = read_7z(archive, pw)?;
        let password = sz_password(pw);
        let mut by_file: HashMap<usize, &Task> = HashMap::new();
        let mut blocks: BTreeMap<usize, usize> = BTreeMap::new();
        for t in &plan.files {
            let Some(fi) = idx.nodes[t.node].src else { continue };
            let e = a.files.get(fi).ok_or(NOT_FOUND)?;
            match a.stream_map.file_block_index.get(fi).copied().flatten() {
                Some(b) if e.has_stream && e.size > 0 => {
                    by_file.insert(fi, t);
                    *blocks.entry(b).or_insert(0) += 1;
                }
                _ => self.write(&mut io::empty(), t, false)?,
            }
        }
        let threads = thread_count();
        let base = a.files.as_ptr() as usize;
        let stride = std::mem::size_of::<sevenz_rust2::ArchiveEntry>().max(1);
        let mut skip_buf = vec![0u8; 64 * 1024];
        for (b, mut remaining) in blocks {
            self.p.check()?;
            let suspect = has_pw && block_encrypted(&a.blocks[b]);
            let mut failure: Option<String> = None;
            let decoder = sevenz_rust2::BlockDecoder::new(threads, b, &a, &password, &mut src);
            let result = decoder.for_each_entries(
                &mut |entry: &sevenz_rust2::ArchiveEntry,
                      reader: &mut dyn Read|
                      -> Result<bool, sevenz_rust2::Error> {
                    // Entries are handed out by reference into `a.files`: recover the index.
                    let fi = (entry as *const sevenz_rust2::ArchiveEntry as usize).wrapping_sub(base) / stride;
                    match by_file.get(&fi) {
                        Some(t) => {
                            if let Err(e) = self.write(reader, t, suspect) {
                                failure = Some(e);
                                return Ok(false);
                            }
                            remaining -= 1;
                            Ok(remaining > 0)
                        }
                        None => {
                            // Solid blocks: skipped entries must still be decoded.
                            if !drain(reader, self.p, &mut skip_buf)? {
                                failure = Some(CANCELLED.into());
                                return Ok(false);
                            }
                            Ok(true)
                        }
                    }
                },
            );
            if let Some(e) = failure {
                return Err(e);
            }
            result.map_err(|e| sz_err(e, has_pw))?;
        }
        Ok(())
    }

    fn rar(&mut self, archive: &str, plan: &Plan, pw: Option<&str>) -> Result<(), String> {
        let idx = self.idx;
        let has_pw = pw.is_some();
        let mut wanted: HashMap<&str, &Task> =
            plan.files.iter().map(|t| (idx.nodes[t.node].path.as_str(), t)).collect();
        let mut open = rar_archive(archive, pw)
            .open_for_processing()
            .map_err(|e| rar_err(e, has_pw, idx.encrypted))?;
        while !wanted.is_empty() {
            self.p.check()?;
            let Some(h) = open.read_header().map_err(|e| rar_err(e, has_pw, idx.encrypted))? else {
                break;
            };
            let (task, encrypted) = {
                let hdr = h.entry();
                let task = if hdr.is_file() {
                    sanitize_inner(&hdr.filename.to_string_lossy())
                        .and_then(|c| wanted.remove(c.join("/").as_str()))
                } else {
                    None
                };
                (task, hdr.is_encrypted())
            };
            let Some(t) = task else {
                open = h.skip().map_err(|e| rar_err(e, has_pw, encrypted))?;
                continue;
            };
            let node = &idx.nodes[t.node];
            self.p.set_current(&node.name);
            // Reserve the name, then let unrar (which overwrites) write the file.
            let (file, written) = open_target(&t.target, self.mode)?;
            drop(file);
            open = match h.extract_to(&written) {
                Ok(o) => o,
                Err(e) => {
                    let _ = fs::remove_file(&written);
                    return Err(rar_err(e, has_pw, encrypted));
                }
            };
            if let Some(ms) = node.modified {
                set_file_mtime(&written, ms);
            }
            finish_target(&written, &t.target, self.mode)?;
            self.p.add_bytes(node.size);
            self.p.file_done();
        }
        if !wanted.is_empty() {
            return Err(format!("{} item(s) couldn't be extracted (missing volume?)", wanted.len()));
        }
        Ok(())
    }

    fn tar(&mut self, archive: &str, plan: &Plan) -> Result<(), String> {
        let idx = self.idx;
        let mut wanted: HashMap<usize, &Task> = plan
            .files
            .iter()
            .filter_map(|t| idx.nodes[t.node].src.map(|s| (s, t)))
            .collect();
        if idx.kind == ArchiveKind::Tar {
            let f = BufReader::with_capacity(64 * 1024, File::open(archive).map_err(io_err)?);
            let mut ar = tar::Archive::new(f);
            self.tar_entries(ar.entries_with_seek().map_err(damaged)?, &mut wanted)
        } else {
            let mut ar = tar::Archive::new(open_stream(archive, idx.kind)?);
            self.tar_entries(ar.entries().map_err(damaged)?, &mut wanted)
        }
    }

    fn tar_entries<R: Read>(&mut self, entries: tar::Entries<'_, R>, wanted: &mut HashMap<usize, &Task>) -> Result<(), String> {
        for (i, e) in entries.enumerate() {
            if wanted.is_empty() {
                break;
            }
            self.p.check()?;
            let mut e = e.map_err(damaged)?;
            if let Some(t) = wanted.remove(&i) {
                self.write(&mut e, t, false)?;
            }
        }
        Ok(())
    }

    fn single(&mut self, archive: &str, plan: &Plan) -> Result<(), String> {
        for t in &plan.files {
            let mut r = open_stream(archive, self.idx.kind)?;
            self.write(r.as_mut(), t, false)?;
        }
        Ok(())
    }
}

fn run_plan(archive: &str, idx: &Index, plan: &Plan, pw: Option<&str>, p: &dyn Progress, mode: Mode) -> Result<(), String> {
    p.set_totals(plan.bytes, plan.files.len() as u64);
    // Directory times are not preserved (Windows needs backup-semantics handles).
    for d in &plan.dirs {
        fs::create_dir_all(d).map_err(|e| path_err(e, d))?;
    }
    if plan.files.is_empty() {
        return Ok(());
    }
    let mut x = Extractor { idx, p, mode, buf: vec![0u8; COPY_BUF] };
    match idx.kind {
        ArchiveKind::Zip => x.zip(archive, plan, pw),
        ArchiveKind::SevenZ => x.sevenz(archive, plan, pw),
        ArchiveKind::Rar => x.rar(archive, plan, pw),
        k if k.is_tar() => x.tar(archive, plan),
        _ => x.single(archive, plan),
    }
}

// ───────────────────────────── materialize (archive cache) ─────────────────────────────

fn cache_base() -> PathBuf {
    if cfg!(test) {
        std::env::temp_dir().join("ferret-archive-tests").join("cache")
    } else {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("Ferret")
            .join("archive-cache")
    }
}

/// `<cache>\<short hash of archive path + mtime + size>`.
fn cache_dir_for(key: &ArcKey) -> PathBuf {
    let mtime = key
        .mtime
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let h = fnv1a(format!("{}|{}|{}", key.path, mtime, key.size).as_bytes());
    cache_base().join(format!("{:012x}", h & 0xFFFF_FFFF_FFFF))
}

/// Once per run, drops cache folders not used for two weeks.
fn prune_cache_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| {
            let Ok(rd) = fs::read_dir(cache_base()) else { return };
            let Some(cutoff) = SystemTime::now().checked_sub(CACHE_MAX_AGE) else { return };
            for e in rd.flatten() {
                let used = fs::metadata(e.path().join(".ferret-used"))
                    .or_else(|_| e.metadata())
                    .and_then(|m| m.modified());
                if matches!(used, Ok(t) if t < cutoff) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        });
    });
}

/// Extracts one node (file or subtree) into the archive cache, reusing files
/// already there with the right size. Returns the real path.
fn materialize_node(archive: &str, key: &ArcKey, idx: &Index, id: usize, pw: Option<&str>) -> Result<PathBuf, String> {
    prune_cache_once();
    let root = cache_dir_for(key);
    fs::create_dir_all(&root).map_err(|e| path_err(e, &root))?;
    let _ = fs::write(root.join(".ferret-used"), b"");
    let node = &idx.nodes[id];
    let target = node.path.split('/').fold(root, |p, c| p.join(c));
    let mut plan = Plan::default();
    plan_node(idx, id, target.clone(), &mut plan);
    plan.files.retain(|t| {
        let want = idx.nodes[t.node].size;
        !fs::metadata(&t.target).is_ok_and(|m| m.is_file() && m.len() == want)
    });
    plan.bytes = plan.files.iter().map(|t| idx.nodes[t.node].size).sum();
    run_plan(archive, idx, &plan, pw, &NoProgress, Mode::Cache)?;
    Ok(target)
}

pub(crate) fn materialize(path: &str, password: Option<&str>) -> Result<String, String> {
    let (arc, inner) = split_impl(path, false).ok_or(NOT_ARCHIVE)?;
    if inner.is_empty() {
        return Ok(arc); // already a real file
    }
    let pw = effective_password(&arc, password);
    let (key, idx) = load_index(&arc, pw.as_deref())?;
    let id = idx.find(&inner).ok_or(NOT_FOUND)?;
    if let Some(enc) = idx.first_encrypted(&[id]) {
        verify_password(&arc, &idx, enc, pw.as_deref())?;
    }
    let real = materialize_node(&arc, &key, &idx, id, pw.as_deref())?;
    if let Some(p) = pw.as_deref().filter(|_| idx.encrypted) {
        remember_password(&arc, p);
    }
    Ok(path_str(&real))
}

// ───────────────────────────── passwords ─────────────────────────────

/// Checks a password against one encrypted entry without extracting everything.
/// zip and 7z are verified by decoding the start of the entry; rar only checks that
/// a password is present (unrar can't test a prefix; a wrong one fails the job).
fn verify_password(arc: &str, idx: &Index, id: usize, pw: Option<&str>) -> Result<(), String> {
    let Some(pw) = pw else { return Err(PASSWORD_REQUIRED.into()) };
    let Some(src) = idx.nodes[id].src else { return Ok(()) };
    match idx.kind {
        ArchiveKind::Zip => {
            let mut zip = open_zip(arc)?;
            let opts = zip::ZipReadOptions::new().password(Some(pw.as_bytes()));
            let mut f = zip.by_index_with_options(src, opts).map_err(|e| zip_err(e, true))?;
            // ZipCrypto's header check passes 1 in 256 wrong keys; decoding the first
            // MiB catches nearly all of those (and the CRC catches small files).
            let mut buf = vec![0u8; 64 * 1024];
            let mut left = 1usize << 20;
            while left > 0 {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => left = left.saturating_sub(n),
                    Err(_) => return Err(WRONG_PASSWORD.into()),
                }
            }
            Ok(())
        }
        ArchiveKind::SevenZ => {
            let (mut r, a) = read_7z(arc, Some(pw))?;
            let Some(b) = a.stream_map.file_block_index.get(src).copied().flatten() else {
                return Ok(());
            };
            let password = sz_password(Some(pw));
            let mut buf = vec![0u8; 64 * 1024];
            let decoder = sevenz_rust2::BlockDecoder::new(1, b, &a, &password, &mut r);
            decoder
                .for_each_entries(
                    &mut |_e: &sevenz_rust2::ArchiveEntry,
                          reader: &mut dyn Read|
                          -> Result<bool, sevenz_rust2::Error> {
                        let _ = reader.read(&mut buf)?;
                        Ok(false)
                    },
                )
                .map(|_| ())
                .map_err(|_| WRONG_PASSWORD.to_string())
        }
        _ => Ok(()),
    }
}

/// Resolves the items to extract: the children of the `inner` folder (or the
/// `inner` file itself), or the given virtual `entries` of the same archive.
fn resolve_items(arc: &str, idx: &Index, inner: &str, entries: Option<&[String]>) -> Result<Vec<usize>, String> {
    let Some(list) = entries.filter(|l| !l.is_empty()) else {
        let id = idx.find(inner).ok_or(NOT_FOUND)?;
        let n = &idx.nodes[id];
        return Ok(if n.is_dir { n.children.clone() } else { vec![id] });
    };
    let akey = paths::key(arc);
    let mut picked: Vec<usize> = Vec::new();
    for e in list {
        let (a2, i2) = split_impl(e, false).ok_or_else(|| format!("{e} is not inside an archive"))?;
        if paths::key(&a2) != akey {
            return Err(format!("{e} is not inside this archive"));
        }
        let id = idx.find(&i2).ok_or(NOT_FOUND)?;
        if id == 0 {
            picked.extend(idx.nodes[0].children.iter().copied());
        } else {
            picked.push(id);
        }
    }
    // Drop duplicates and items whose ancestor is also selected.
    let set: HashSet<usize> = picked.iter().copied().collect();
    let mut seen = HashSet::new();
    picked.retain(|&id| {
        let mut p = idx.nodes[id].parent;
        while p != 0 {
            if set.contains(&p) {
                return false;
            }
            p = idx.nodes[p].parent;
        }
        seen.insert(id)
    });
    Ok(picked)
}

/// Synchronous pre-flight for extraction so the UI gets `password-required` /
/// `wrong-password` straight from the command. Returns the password to use.
fn check_password(archive: &str, entries: Option<&[String]>, password: Option<&str>) -> Result<Option<String>, String> {
    let (arc, inner) = split_impl(archive, true).ok_or(NOT_ARCHIVE)?;
    let kind = archive_kind(&arc).ok_or(NOT_ARCHIVE)?;
    let pw = effective_password(&arc, password);
    if !matches!(kind, ArchiveKind::Zip | ArchiveKind::SevenZ | ArchiveKind::Rar) {
        return Ok(pw); // tar & co. have no encryption (and indexing them means a full scan)
    }
    let (_, idx) = load_index(&arc, pw.as_deref())?;
    let items = resolve_items(&arc, &idx, &inner, entries)?;
    if let Some(enc) = idx.first_encrypted(&items) {
        verify_password(&arc, &idx, enc, pw.as_deref())?;
        if let Some(p) = &pw {
            remember_password(&arc, p);
        }
    }
    Ok(pw)
}

pub(crate) fn unlock(path: &str, password: &str) -> Result<(), String> {
    let (arc, _) = split_virtual(path).ok_or(NOT_ARCHIVE)?;
    let (_, idx) = load_index(&arc, Some(password))?;
    if let Some(enc) = idx.first_encrypted(&[0]) {
        verify_password(&arc, &idx, enc, Some(password))?;
    }
    remember_password(&arc, password);
    Ok(())
}

// ───────────────────────────── extract jobs ─────────────────────────────

fn report_tops(p: &dyn Progress, tops: &[PathBuf]) {
    for t in tops {
        if fs::symlink_metadata(t).is_ok() {
            p.created(&path_str(t));
        }
    }
}

/// Extracts `archive` (real archive: everything; virtual folder: its contents;
/// virtual file: that file), or just `entries`, into `dest`.
pub(crate) fn extract_impl(archive: &str, dest: &str, entries: Option<&[String]>, pw: Option<&str>, p: &dyn Progress) -> Result<(), String> {
    let (arc, inner) = split_impl(archive, true).ok_or(NOT_ARCHIVE)?;
    let pw = effective_password(&arc, pw);
    let (_, idx) = load_index(&arc, pw.as_deref())?;
    let items = resolve_items(&arc, &idx, &inner, entries)?;
    let dest_norm = paths::normalize(dest);
    if split_virtual(&dest_norm).is_some_and(|(_, i)| !i.is_empty()) {
        return Err("Can't extract into an archive".into());
    }
    let dest_dir = PathBuf::from(paths::for_io(&dest_norm));
    fs::create_dir_all(&dest_dir).map_err(|e| path_err(e, &dest_dir))?;
    let (plan, tops) = plan_items(&idx, &items, &dest_dir);
    let res = run_plan(&arc, &idx, &plan, pw.as_deref(), p, Mode::Extract);
    report_tops(p, &tops);
    res
}

/// "Extract here": a lone top-level folder (or the single file of a .gz/.xz/...)
/// goes straight next to the archive, anything else into a new `<stem>` folder.
pub(crate) fn extract_smart_impl(archive: &str, pw: Option<&str>, p: &dyn Progress) -> Result<(), String> {
    let vpath = paths::normalize(archive);
    let (arc, inner) = split_impl(&vpath, true).ok_or(NOT_ARCHIVE)?;
    let parent = paths::parent(&vpath).ok_or("The archive has no parent folder")?;
    if split_virtual(&parent).is_some() {
        return Err("Can't extract into a folder inside an archive".into());
    }
    let pw = effective_password(&arc, pw);
    let (_, idx) = load_index(&arc, pw.as_deref())?;
    let root = idx.find(&inner).ok_or(NOT_FOUND)?;
    let rn = &idx.nodes[root];
    let parent_dir = PathBuf::from(paths::for_io(&parent));
    let lone = rn.children.len() == 1;
    if lone && (idx.nodes[rn.children[0]].is_dir || idx.kind.is_single()) {
        let (plan, tops) = plan_items(&idx, &rn.children, &parent_dir);
        let res = run_plan(&arc, &idx, &plan, pw.as_deref(), p, Mode::Extract);
        report_tops(p, &tops);
        return res;
    }
    let stem = archive_stem(file_name_of(&vpath));
    let first = parent_dir.join(&stem);
    let mut folder = first.clone();
    let mut n = 2u32;
    loop {
        match fs::create_dir(&folder) {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100_000 => {
                folder = numbered(&first, n, true);
                n += 1;
            }
            Err(e) => return Err(path_err(e, &folder)),
        }
    }
    p.created(&path_str(&folder));
    let (plan, _) = plan_items(&idx, &rn.children, &folder);
    run_plan(&arc, &idx, &plan, pw.as_deref(), p, Mode::Extract)
}

// ───────────────────────────── compress ─────────────────────────────

/// Formats that don't shrink further: stored as-is for speed.
const STORED_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "heif", "jxl", "mp4", "m4v", "mkv", "mov",
    "avi", "webm", "wmv", "flv", "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wma", "zip",
    "7z", "rar", "gz", "tgz", "xz", "txz", "zst", "bz2", "lz", "lz4", "lzma", "cab", "jar", "apk",
    "cbz", "cbr", "epub", "docx", "xlsx", "pptx", "odt", "ods", "odp", "woff", "woff2", "msi",
    "nupkg", "vsix", "whl", "xpi", "dmg", "iso",
];

struct SrcItem {
    path: PathBuf,
    /// Name inside the zip (`/`-separated, dirs end with `/`).
    rel: String,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

fn collect_sources(path: &Path, rel: String, dest_key: &str, out: &mut Vec<SrcItem>, p: &dyn Progress) -> Result<(), String> {
    p.check()?;
    if paths::key(&path_str(path)) == dest_key {
        return Ok(()); // never zip the zip into itself
    }
    let meta = fs::symlink_metadata(path).map_err(|e| path_err(e, path))?;
    let meta = if meta.file_type().is_symlink() {
        // Follow links to files; don't descend into linked folders / junctions
        // (they can loop): they become empty folders.
        match fs::metadata(path) {
            Ok(m) if m.is_file() => m,
            Ok(m) if m.is_dir() => {
                out.push(SrcItem { path: path.to_path_buf(), rel: format!("{rel}/"), is_dir: true, size: 0, modified: m.modified().ok() });
                return Ok(());
            }
            _ => return Ok(()),
        }
    } else {
        meta
    };
    if meta.is_dir() {
        out.push(SrcItem { path: path.to_path_buf(), rel: format!("{rel}/"), is_dir: true, size: 0, modified: meta.modified().ok() });
        let mut kids: Vec<_> = fs::read_dir(path)
            .map_err(|e| path_err(e, path))?
            .flatten()
            .map(|e| e.file_name())
            .collect();
        kids.sort();
        for k in kids {
            let name = k.to_string_lossy().into_owned();
            collect_sources(&path.join(&k), format!("{rel}/{name}"), dest_key, out, p)?;
        }
    } else {
        out.push(SrcItem { path: path.to_path_buf(), rel, is_dir: false, size: meta.len(), modified: meta.modified().ok() });
    }
    Ok(())
}

fn zip_werr(e: zip::result::ZipError) -> String {
    match e {
        zip::result::ZipError::Io(e) => e.to_string(),
        e => e.to_string(),
    }
}

fn write_zip(file: File, items: &[SrcItem], p: &dyn Progress) -> Result<(), String> {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    let mut zw = zip::ZipWriter::new(BufWriter::with_capacity(1 << 20, file));
    let mut buf = vec![0u8; COPY_BUF];
    for it in items {
        p.check()?;
        let base = SimpleFileOptions::default()
            .last_modified_time(it.modified.map(zip_datetime).unwrap_or_default());
        if it.is_dir {
            zw.add_directory(it.rel.as_str(), base).map_err(zip_werr)?;
            continue;
        }
        p.set_current(file_name_of(&it.rel));
        let stored = it.size == 0 || STORED_EXTS.contains(&ext_of(file_name_of(&it.rel)).as_str());
        let opts = base
            .compression_method(if stored { CompressionMethod::Stored } else { CompressionMethod::Deflated })
            .large_file(it.size >= zip::ZIP64_BYTES_THR);
        zw.start_file(it.rel.as_str(), opts).map_err(zip_werr)?;
        let mut f = File::open(&it.path).map_err(|e| path_err(e, &it.path))?;
        loop {
            p.check()?;
            let n = match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(path_err(e, &it.path)),
            };
            zw.write_all(&buf[..n]).map_err(io_err)?;
            p.add_bytes(n as u64);
        }
        p.file_done();
    }
    zw.finish().map_err(zip_werr)?.flush().map_err(io_err)?;
    Ok(())
}

fn validate_compress(sources: &[String], dest: &str) -> Result<(), String> {
    if sources.is_empty() {
        return Err("Nothing to compress".into());
    }
    if fs::symlink_metadata(dest).is_ok() {
        return Err(format!("{} already exists", file_name_of(dest)));
    }
    if let Some(dir) = paths::parent(dest) {
        if !Path::new(&paths::for_io(&dir)).is_dir() {
            return Err(format!("Folder not found: {dir}"));
        }
    }
    for s in sources {
        if split_virtual(s).is_some_and(|(_, inner)| !inner.is_empty()) {
            return Err("Can't compress items that are inside an archive".into());
        }
        if fs::symlink_metadata(paths::for_io(&paths::normalize(s))).is_err() {
            return Err(format!("Not found: {s}"));
        }
    }
    Ok(())
}

/// Zips files/folders (paths relative to each source's parent) into `dest`.
/// The partial zip is deleted on error or cancel.
pub(crate) fn compress_zip_impl(sources: &[String], dest: &str, p: &dyn Progress) -> Result<(), String> {
    let dest = paths::normalize(dest);
    validate_compress(sources, &dest)?;
    let dest_key = paths::key(&dest);
    let mut items = Vec::new();
    for s in sources {
        let sn = paths::normalize(s);
        let name = file_name_of(&sn).trim_end_matches(':').to_string();
        collect_sources(Path::new(&paths::for_io(&sn)), name, &dest_key, &mut items, p)?;
    }
    let files = items.iter().filter(|i| !i.is_dir).count() as u64;
    p.set_totals(items.iter().map(|i| i.size).sum(), files);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map_err(|e| path_err(e, Path::new(&dest)))?;
    match write_zip(file, &items, p) {
        Ok(()) => {
            p.created(&dest);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&dest);
            Err(e)
        }
    }
}

// ───────────────────────────── info ─────────────────────────────

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo {
    pub format: String,
    pub entries: u64,
    pub files: u64,
    pub dirs: u64,
    /// Uncompressed bytes.
    pub total_size: u64,
    /// Archive file size on disk.
    pub compressed_size: u64,
    pub encrypted: bool,
    /// Up to 50 names, folders first with a trailing `/`.
    pub top_level: Vec<String>,
    pub comment: Option<String>,
}

pub(crate) fn info_impl(path: &str) -> Result<ArchiveInfo, String> {
    let (arc, inner) = split_virtual(path).ok_or(NOT_ARCHIVE)?;
    let (key, idx) = load_index(&arc, None)?;
    let id = idx.find(&inner).ok_or(NOT_FOUND)?;
    let (mut files, mut dirs, mut total) = (0u64, 0u64, 0u64);
    let mut stack = vec![id];
    while let Some(i) = stack.pop() {
        let n = &idx.nodes[i];
        if n.is_dir {
            if i != id {
                dirs += 1;
            }
            stack.extend(n.children.iter().copied());
        } else {
            files += 1;
            total += n.size;
        }
    }
    let node = &idx.nodes[id];
    let mut kids: Vec<&Node> = if node.is_dir {
        node.children.iter().map(|&c| &idx.nodes[c]).collect()
    } else {
        vec![node]
    };
    kids.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let top_level = kids
        .iter()
        .take(50)
        .map(|n| if n.is_dir { format!("{}/", n.name) } else { n.name.clone() })
        .collect();
    Ok(ArchiveInfo {
        format: idx.kind.label().to_string(),
        entries: files + dirs,
        files,
        dirs,
        total_size: total,
        compressed_size: key.size,
        encrypted: idx.encrypted,
        top_level,
        comment: if id == 0 { idx.comment.clone() } else { None },
    })
}

// ───────────────────────────── commands ─────────────────────────────

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Extracts a file or folder inside an archive to the archive cache and returns its
/// real path (for previews / "open with").
#[tauri::command]
pub async fn archive_materialize(path: String, password: Option<String>) -> Result<String, String> {
    blocking(move || materialize(&path, password.as_deref())).await
}

/// Starts an extract job and returns its id. Fails fast with `password-required` /
/// `wrong-password` for encrypted zip / 7z / rar.
#[tauri::command]
pub async fn archive_extract(
    app: AppHandle,
    archive: String,
    dest: String,
    entries: Option<Vec<String>>,
    password: Option<String>,
) -> Result<u64, String> {
    let (archive, entries, pw) = blocking(move || {
        let pw = check_password(&archive, entries.as_deref(), password.as_deref())?;
        Ok((archive, entries, pw))
    })
    .await?;
    log::info!("extract \"{archive}\" to \"{dest}\" ({})", entries.as_ref().map_or("all entries".into(), |e| format!("{} entries", e.len())));
    let label = format!("Extracting {}", file_name_of(&archive));
    Ok(jobs::spawn(&app, "extract", &label, move |ctx| {
        extract_impl(&archive, &dest, entries.as_deref(), pw.as_deref(), ctx)
    }))
}

/// "Extract here" job; returns its id.
#[tauri::command]
pub async fn archive_extract_smart(app: AppHandle, archive: String, password: Option<String>) -> Result<u64, String> {
    let (archive, pw) = blocking(move || {
        let pw = check_password(&archive, None, password.as_deref())?;
        Ok((archive, pw))
    })
    .await?;
    log::info!("extract here \"{archive}\"");
    let label = format!("Extracting {}", file_name_of(&archive));
    Ok(jobs::spawn(&app, "extract", &label, move |ctx| {
        extract_smart_impl(&archive, pw.as_deref(), ctx)
    }))
}

/// Starts a job zipping `sources` into `dest` (full .zip path, must not exist).
#[tauri::command]
pub fn compress_zip(app: AppHandle, sources: Vec<String>, dest: String) -> Result<u64, String> {
    let dest = paths::normalize(&dest);
    validate_compress(&sources, &dest)?;
    log::info!("compress to \"{dest}\": {}", crate::logging::paths(&sources));
    let label = format!("Compressing {}", file_name_of(&dest));
    Ok(jobs::spawn(&app, "compress", &label, move |ctx| compress_zip_impl(&sources, &dest, ctx)))
}

/// Summary of an archive (or of a folder inside one) for the preview pane.
#[tauri::command]
pub async fn archive_info(path: String) -> Result<ArchiveInfo, String> {
    blocking(move || info_impl(&path)).await
}

/// Verifies and remembers (for this session) an archive's password, so archives
/// with encrypted headers can be browsed afterwards.
#[tauri::command]
pub async fn archive_unlock(path: String, password: String) -> Result<(), String> {
    blocking(move || unlock(&path, &password)).await
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use zip::write::SimpleFileOptions;

    static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("ferret-archive-tests").join(format!(
            "{name}-{}-{}",
            std::process::id(),
            DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // Display-form path (what the UI passes around).
        PathBuf::from(paths::normalize(&path_str(&d)))
    }

    fn s(p: &Path) -> String {
        path_str(p)
    }

    #[derive(Default)]
    struct TestProgress {
        cancel_after: Option<u64>,
        bytes: AtomicU64,
        files: AtomicU64,
        totals: Mutex<(u64, u64)>,
        created: Mutex<Vec<String>>,
    }

    impl Progress for TestProgress {
        fn check(&self) -> Result<(), String> {
            match self.cancel_after {
                Some(n) if self.bytes.load(Ordering::Relaxed) >= n => Err(CANCELLED.into()),
                _ => Ok(()),
            }
        }
        fn set_totals(&self, bytes: u64, files: u64) {
            *lock(&self.totals) = (bytes, files);
        }
        fn set_current(&self, _: &str) {}
        fn add_bytes(&self, n: u64) {
            self.bytes.fetch_add(n, Ordering::Relaxed);
        }
        fn file_done(&self) {
            self.files.fetch_add(1, Ordering::Relaxed);
        }
        fn created(&self, path: &str) {
            lock(&self.created).push(path.to_string());
        }
    }

    fn dt() -> zip::DateTime {
        zip::DateTime::from_date_and_time(2020, 5, 17, 13, 45, 30).unwrap()
    }

    /// `None` data = directory entry.
    fn make_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(dt());
        for (name, data) in entries {
            match data {
                None => z.add_directory(*name, opts).unwrap(),
                Some(d) => {
                    z.start_file(*name, opts).unwrap();
                    z.write_all(d).unwrap();
                }
            }
        }
        z.set_comment("hello comment").unwrap();
        z.finish().unwrap();
    }

    fn list_err(p: &str) -> String {
        match list_children(p) {
            Ok(_) => panic!("listing {p} should fail"),
            Err(e) => e,
        }
    }

    fn names(v: &[Entry]) -> Vec<String> {
        let mut n: Vec<String> = v.iter().map(|e| e.name.clone()).collect();
        n.sort();
        n
    }

    fn tar_bytes(files: &[(&str, &[u8])], dirs: &[&str]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for d in dirs {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
            h.set_mode(0o755);
            h.set_mtime(1_600_000_000);
            b.append_data(&mut h, d, &[][..]).unwrap();
        }
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Regular);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_600_000_000);
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn make_7z(path: &Path, pw: Option<&str>) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let mut w = ArchiveWriter::create(path).unwrap();
        if let Some(p) = pw {
            w.set_content_methods(vec![
                sevenz_rust2::encoder_options::AesEncoderOptions::new(sevenz_rust2::Password::from(p)).into(),
                sevenz_rust2::EncoderConfiguration::new(sevenz_rust2::EncoderMethod::LZMA2),
            ]);
        }
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("music"), None).unwrap();
        w.push_archive_entry(ArchiveEntry::new_file("music/song.txt"), Some(&b"la la la"[..])).unwrap();
        // One solid block with three files: extracting the last one decodes the others.
        w.push_archive_entries(
            vec![
                ArchiveEntry::new_file("docs/a.txt"),
                ArchiveEntry::new_file("docs/b.txt"),
                ArchiveEntry::new_file("docs/deep/c.txt"),
            ],
            vec![
                SourceReader::new(&b"alpha"[..]),
                SourceReader::new(&b"bravo bravo"[..]),
                SourceReader::new(&b"charlie"[..]),
            ],
        )
        .unwrap();
        w.push_archive_entry(ArchiveEntry::new_file("empty.txt"), Some(&b""[..])).unwrap();
        w.push_archive_entry(ArchiveEntry::new_file("top.txt"), Some(&b"top level"[..])).unwrap();
        w.finish().unwrap();
    }

    fn sample_zip(dir: &Path) -> PathBuf {
        let z = dir.join("sample.zip");
        make_zip(
            &z,
            &[
                ("docs/", None),
                ("docs/readme.txt", Some(b"read me")),
                ("docs/sub/", None),
                ("docs/sub/deep.txt", Some(b"deep down")),
                ("ünïcødé/文件.txt", Some("unicode ✓".as_bytes())),
                ("root.txt", Some(b"at the root")),
            ],
        );
        z
    }

    #[test]
    fn archive_kinds() {
        use ArchiveKind::*;
        assert_eq!(archive_kind(r"C:\a\B.ZIP"), Some(Zip));
        assert_eq!(archive_kind("x.epub"), Some(Zip));
        assert_eq!(archive_kind("x.whl"), Some(Zip));
        assert_eq!(archive_kind("x.7z"), Some(SevenZ));
        assert_eq!(archive_kind("x.cbr"), Some(Rar));
        assert_eq!(archive_kind("x.tar"), Some(Tar));
        assert_eq!(archive_kind("x.tar.gz"), Some(TarGz));
        assert_eq!(archive_kind("x.TGZ"), Some(TarGz));
        assert_eq!(archive_kind("x.tar.xz"), Some(TarXz));
        assert_eq!(archive_kind("x.tzst"), Some(TarZst));
        assert_eq!(archive_kind("x.tar.bz2"), Some(TarBz2));
        assert_eq!(archive_kind("x.tbz"), Some(TarBz2));
        assert_eq!(archive_kind("notes.txt.gz"), Some(Gz));
        assert_eq!(archive_kind("x.xz"), Some(Xz));
        assert_eq!(archive_kind("x.zst"), Some(Zst));
        assert_eq!(archive_kind("x.bz2"), Some(Bz2));
        assert_eq!(archive_kind("x.txt"), None);
        assert_eq!(archive_kind("folder"), None);
        assert_eq!(archive_kind(".zip"), None);
        assert_eq!(archive_stem("foo.tar.gz"), "foo");
        assert_eq!(archive_stem("foo.zip"), "foo");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_inner("../evil.txt"), None);
        assert_eq!(sanitize_inner("a/../../evil.txt"), None);
        assert_eq!(sanitize_inner("/abs.txt"), None);
        assert_eq!(sanitize_inner(r"\abs.txt"), None);
        assert_eq!(sanitize_inner("C:/drive.txt"), None);
        assert_eq!(sanitize_inner("./a//b/"), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(sanitize_inner(r"a\b"), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(sanitize_component("a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize_component("trail. "), "trail__");
        assert_eq!(sanitize_component("con.txt"), "_con.txt");
    }

    #[test]
    fn split_virtual_paths() {
        let d = tmp("split");
        let z = sample_zip(&d);
        let zs = s(&z);
        assert_eq!(split_virtual(&zs), Some((zs.clone(), String::new())));
        assert_eq!(split_virtual(&format!(r"{zs}\docs\sub")), Some((zs.clone(), "docs/sub".to_string())));
        assert_eq!(split_virtual(&format!(r"{zs}\docs\sub\")), Some((zs.clone(), "docs/sub".to_string())));
        assert_eq!(split_virtual(&s(&d)), None);
        assert_eq!(split_virtual(&s(&d.join("missing.zip").join("x"))), None);
        // A folder that merely looks like an archive is an ordinary path.
        let fake = d.join("fake.zip");
        fs::create_dir_all(fake.join("inner")).unwrap();
        fs::write(fake.join("inner").join("f.txt"), b"x").unwrap();
        assert_eq!(split_virtual(&s(&fake.join("inner").join("f.txt"))), None);
        assert_eq!(split_virtual(&s(&fake)), None);
    }

    #[test]
    fn zip_with_dir_entries_and_unicode() {
        let d = tmp("zipdirs");
        let z = s(&sample_zip(&d));
        let root = list_children(&z).unwrap();
        assert_eq!(names(&root), vec!["docs", "root.txt", "ünïcødé"]);
        let docs = root.iter().find(|e| e.name == "docs").unwrap();
        assert!(docs.is_dir && docs.in_archive && docs.ext.is_empty());
        assert_eq!(docs.path, format!(r"{z}\docs"));
        let rt = root.iter().find(|e| e.name == "root.txt").unwrap();
        assert_eq!(rt.size, 11);
        assert_eq!(rt.ext, "txt");
        // DOS time is local wall-clock time.
        let expect = Local.with_ymd_and_hms(2020, 5, 17, 13, 45, 30).unwrap().timestamp_millis();
        assert_eq!(rt.modified, Some(expect));

        let sub = list_children(&format!(r"{z}\docs\sub")).unwrap();
        assert_eq!(names(&sub), vec!["deep.txt"]);
        assert_eq!(sub[0].path, format!(r"{z}\docs\sub\deep.txt"));
        let uni = list_children(&format!(r"{z}\ünïcødé")).unwrap();
        assert_eq!(names(&uni), vec!["文件.txt"]);
        // Case-insensitive lookup.
        assert_eq!(names(&list_children(&format!(r"{z}\DOCS")).unwrap()), vec!["readme.txt", "sub"]);
        assert_eq!(list_err(&format!(r"{z}\nope")), NOT_FOUND);
        assert_eq!(list_err(&format!(r"{z}\root.txt")), NOT_FOUND);
    }

    #[test]
    fn zip_without_dir_entries() {
        let d = tmp("zipnodirs");
        let z = d.join("flat.zip");
        make_zip(&z, &[("a/b/c.txt", Some(b"c")), ("a/d.txt", Some(b"dd")), ("e.txt", Some(b"eee"))]);
        let z = s(&z);
        let root = list_children(&z).unwrap();
        assert_eq!(names(&root), vec!["a", "e.txt"]);
        assert!(root.iter().find(|e| e.name == "a").unwrap().is_dir);
        assert_eq!(names(&list_children(&format!(r"{z}\a")).unwrap()), vec!["b", "d.txt"]);
        let b = list_children(&format!(r"{z}\a\b")).unwrap();
        assert_eq!(names(&b), vec!["c.txt"]);
        assert_eq!(b[0].size, 1);
        let info = info_impl(&z).unwrap();
        assert_eq!((info.files, info.dirs, info.entries, info.total_size), (3, 2, 5, 6));
        assert_eq!(info.top_level, vec!["a/", "e.txt"]);
        assert_eq!(info.format, "ZIP");
        assert_eq!(info.comment.as_deref(), Some("hello comment"));
        assert!(!info.encrypted);
    }

    #[test]
    fn tar_gz_and_tar_zst() {
        let d = tmp("tars");
        let raw = tar_bytes(
            &[("proj/src/main.rs", b"fn main() {}"), ("proj/README.md", b"# hi"), ("./proj/link-free.txt", b"x")],
            &["proj/empty/"],
        );
        let tgz = d.join("p.tar.gz");
        let mut enc = flate2::write::GzEncoder::new(File::create(&tgz).unwrap(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
        let tzst = d.join("p.tar.zst");
        fs::write(&tzst, zstd::encode_all(&raw[..], 3).unwrap()).unwrap();
        let tar_plain = d.join("p.tar");
        fs::write(&tar_plain, &raw).unwrap();

        for a in [&tgz, &tzst, &tar_plain] {
            let a = s(a);
            assert_eq!(names(&list_children(&a).unwrap()), vec!["proj"], "{a}");
            let proj = list_children(&format!(r"{a}\proj")).unwrap();
            assert_eq!(names(&proj), vec!["README.md", "empty", "link-free.txt", "src"]);
            let readme = proj.iter().find(|e| e.name == "README.md").unwrap();
            assert_eq!(readme.modified, Some(1_600_000_000_000));
            let out = d.join(format!("out-{}", file_name_of(&a)));
            let p = TestProgress::default();
            extract_impl(&a, &s(&out), None, None, &p).unwrap();
            assert_eq!(fs::read(out.join("proj").join("src").join("main.rs")).unwrap(), b"fn main() {}");
            assert!(out.join("proj").join("empty").is_dir());
            assert_eq!(*lock(&p.totals), (17, 3));
            assert_eq!(p.files.load(Ordering::Relaxed), 3);
            assert_eq!(*lock(&p.created), vec![s(&out.join("proj"))]);
            let m = materialize(&format!(r"{a}\proj\README.md"), None).unwrap();
            assert_eq!(fs::read(&m).unwrap(), b"# hi");
        }
    }

    #[test]
    fn sevenz_list_materialize_extract() {
        let d = tmp("7z");
        let a = d.join("s.7z");
        make_7z(&a, None);
        let a = s(&a);
        assert_eq!(names(&list_children(&a).unwrap()), vec!["docs", "empty.txt", "music", "top.txt"]);
        let docs = list_children(&format!(r"{a}\docs")).unwrap();
        assert_eq!(names(&docs), vec!["a.txt", "b.txt", "deep"]);
        assert_eq!(docs.iter().find(|e| e.name == "b.txt").unwrap().size, 11);
        let c = materialize(&format!(r"{a}\docs\deep\c.txt"), None).unwrap();
        assert_eq!(fs::read_to_string(&c).unwrap(), "charlie");
        assert!(c.ends_with(r"docs\deep\c.txt"));
        let e = materialize(&format!(r"{a}\empty.txt"), None).unwrap();
        assert_eq!(fs::read(&e).unwrap().len(), 0);
        // Subset extraction keeps names relative to their parent.
        let out = d.join("out");
        let p = TestProgress::default();
        let entries = vec![format!(r"{a}\docs\b.txt"), format!(r"{a}\music")];
        extract_impl(&a, &s(&out), Some(&entries), None, &p).unwrap();
        assert_eq!(fs::read_to_string(out.join("b.txt")).unwrap(), "bravo bravo");
        assert_eq!(fs::read_to_string(out.join("music").join("song.txt")).unwrap(), "la la la");
        assert!(!out.join("docs").exists());
        let info = info_impl(&a).unwrap();
        assert_eq!(info.format, "7Z");
        assert_eq!(info.files, 6);
    }

    #[test]
    fn sevenz_encrypted_headers() {
        let d = tmp("7zpw");
        let a = d.join("secret.7z");
        make_7z(&a, Some("hunter2"));
        let a = s(&a);
        assert_eq!(list_err(&a), PASSWORD_REQUIRED);
        assert_eq!(info_impl(&a).unwrap_err(), PASSWORD_REQUIRED);
        assert_eq!(unlock(&a, "nope").unwrap_err(), WRONG_PASSWORD);
        assert_eq!(check_password(&a, None, None).unwrap_err(), PASSWORD_REQUIRED);
        unlock(&a, "hunter2").unwrap();
        assert_eq!(names(&list_children(&a).unwrap()), vec!["docs", "empty.txt", "music", "top.txt"]);
        assert!(info_impl(&a).unwrap().encrypted);
        let out = d.join("out");
        extract_impl(&a, &s(&out), None, None, &TestProgress::default()).unwrap();
        assert_eq!(fs::read_to_string(out.join("docs").join("deep").join("c.txt")).unwrap(), "charlie");
    }

    #[test]
    fn encrypted_zip_passwords() {
        let d = tmp("zippw");
        let z = d.join("locked.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&z).unwrap());
            let opts = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "s3cret");
            w.start_file("secret.txt", opts).unwrap();
            w.write_all(b"top secret").unwrap();
            w.start_file("plain.txt", SimpleFileOptions::default()).unwrap();
            w.write_all(b"public").unwrap();
            w.finish().unwrap();
        }
        let z = s(&z);
        // Listing works without a password.
        assert_eq!(names(&list_children(&z).unwrap()), vec!["plain.txt", "secret.txt"]);
        assert!(info_impl(&z).unwrap().encrypted);
        let secret = format!(r"{z}\secret.txt");
        assert_eq!(materialize(&secret, None).unwrap_err(), PASSWORD_REQUIRED);
        assert_eq!(materialize(&secret, Some("wrong")).unwrap_err(), WRONG_PASSWORD);
        assert_eq!(check_password(&z, None, None).unwrap_err(), PASSWORD_REQUIRED);
        assert_eq!(check_password(&z, None, Some("wrong")).unwrap_err(), WRONG_PASSWORD);
        // Only unencrypted entries selected: no password needed.
        assert_eq!(check_password(&z, Some(&[format!(r"{z}\plain.txt")]), None).unwrap(), None);
        let out = d.join("bad");
        assert_eq!(extract_impl(&z, &s(&out), None, Some("wrong"), &TestProgress::default()).unwrap_err(), WRONG_PASSWORD);
        assert_eq!(fs::read_to_string(materialize(&secret, Some("s3cret")).unwrap()).unwrap(), "top secret");
        // Remembered for the session.
        assert_eq!(check_password(&z, None, None).unwrap().as_deref(), Some("s3cret"));
        let out = d.join("good");
        extract_impl(&z, &s(&out), None, None, &TestProgress::default()).unwrap();
        assert_eq!(fs::read_to_string(out.join("secret.txt")).unwrap(), "top secret");
    }

    #[test]
    fn single_file_streams() {
        let d = tmp("single");
        let gz = d.join("note.txt.gz");
        let mut enc = flate2::write::GzEncoder::new(File::create(&gz).unwrap(), flate2::Compression::default());
        enc.write_all(b"hello world").unwrap();
        enc.finish().unwrap();
        let xz = d.join("data.bin.xz");
        let mut enc = liblzma::write::XzEncoder::new(File::create(&xz).unwrap(), 6);
        enc.write_all(&vec![7u8; 100_000]).unwrap();
        enc.finish().unwrap();
        let zst = d.join("z.log.zst");
        fs::write(&zst, zstd::encode_all(&b"zstd data"[..], 3).unwrap()).unwrap();
        let bz = d.join("b.txt.bz2");
        let mut enc = bzip2::write::BzEncoder::new(File::create(&bz).unwrap(), bzip2::Compression::default());
        enc.write_all(b"bzip two").unwrap();
        enc.finish().unwrap();

        let gzs = s(&gz);
        let l = list_children(&gzs).unwrap();
        assert_eq!((l[0].name.as_str(), l[0].size, l[0].ext.as_str()), ("note.txt", 11, "txt"));
        assert_eq!(list_children(&s(&xz)).unwrap()[0].size, 100_000);
        assert_eq!(list_children(&s(&zst)).unwrap()[0].size, 9);
        assert_eq!(list_children(&s(&bz)).unwrap()[0].size, 8);
        assert_eq!(fs::read_to_string(materialize(&format!(r"{gzs}\note.txt"), None).unwrap()).unwrap(), "hello world");
        assert_eq!(fs::read(materialize(&format!(r"{}\data.bin", s(&xz)), None).unwrap()).unwrap().len(), 100_000);
        // Extract here: the lone file lands next to the archive, no wrapper folder.
        let p = TestProgress::default();
        extract_smart_impl(&gzs, None, &p).unwrap();
        assert_eq!(fs::read_to_string(d.join("note.txt")).unwrap(), "hello world");
        extract_smart_impl(&gzs, None, &TestProgress::default()).unwrap();
        assert_eq!(fs::read_to_string(d.join("note (2).txt")).unwrap(), "hello world");
        assert_eq!(*lock(&p.created), vec![s(&d.join("note.txt"))]);
    }

    #[test]
    fn extract_all_subset_conflicts() {
        let d = tmp("extract");
        let z = s(&sample_zip(&d));
        let out = d.join("out");
        let p = TestProgress::default();
        extract_impl(&z, &s(&out), None, None, &p).unwrap();
        assert_eq!(fs::read_to_string(out.join("docs").join("sub").join("deep.txt")).unwrap(), "deep down");
        assert_eq!(fs::read_to_string(out.join("ünïcødé").join("文件.txt")).unwrap(), "unicode ✓");
        assert_eq!(*lock(&p.totals), (7 + 9 + "unicode ✓".len() as u64 + 11, 4));
        assert_eq!(p.bytes.load(Ordering::Relaxed), lock(&p.totals).0);
        let mut created = lock(&p.created).clone();
        created.sort();
        assert_eq!(created, vec![s(&out.join("docs")), s(&out.join("root.txt")), s(&out.join("ünïcødé"))]);
        // Modified time preserved.
        let m = fs::metadata(out.join("root.txt")).unwrap().modified().unwrap();
        let expect = Local.with_ymd_and_hms(2020, 5, 17, 13, 45, 30).unwrap().timestamp_millis();
        assert_eq!(system_to_ms(m), Some(expect));

        // Again: nothing is overwritten.
        let p = TestProgress::default();
        extract_impl(&z, &s(&out), None, None, &p).unwrap();
        assert!(out.join("docs (2)").join("readme.txt").is_file());
        assert_eq!(fs::read_to_string(out.join("root (2).txt")).unwrap(), "at the root");

        // Subset: a folder and a file, relative to their parents.
        let sub = d.join("subset");
        let entries = vec![format!(r"{z}\docs\sub"), format!(r"{z}\root.txt"), format!(r"{z}\docs\sub\deep.txt")];
        extract_impl(&z, &s(&sub), Some(&entries), None, &TestProgress::default()).unwrap();
        let mut got: Vec<String> = fs::read_dir(&sub).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        got.sort();
        assert_eq!(got, vec!["root.txt", "sub"]);
        assert!(sub.join("sub").join("deep.txt").is_file());

        // Virtual folder as the archive: its contents.
        let vf = d.join("virtual");
        extract_impl(&format!(r"{z}\docs"), &s(&vf), None, None, &TestProgress::default()).unwrap();
        assert!(vf.join("readme.txt").is_file() && vf.join("sub").join("deep.txt").is_file());

        // Entries from another archive are rejected.
        let other = d.join("other.zip");
        make_zip(&other, &[("x.txt", Some(b"x"))]);
        let bad = vec![format!(r"{}\x.txt", s(&other))];
        assert!(extract_impl(&z, &s(&d.join("bad")), Some(&bad), None, &TestProgress::default()).is_err());
    }

    #[test]
    fn smart_extract_modes() {
        let d = tmp("smart");
        let single = d.join("proj.zip");
        make_zip(&single, &[("proj/a.txt", Some(b"a")), ("proj/b/c.txt", Some(b"c"))]);
        let p = TestProgress::default();
        extract_smart_impl(&s(&single), None, &p).unwrap();
        assert!(d.join("proj").join("b").join("c.txt").is_file());
        assert_eq!(*lock(&p.created), vec![s(&d.join("proj"))]);
        let p = TestProgress::default();
        extract_smart_impl(&s(&single), None, &p).unwrap();
        assert!(d.join("proj (2)").join("a.txt").is_file());
        assert_eq!(*lock(&p.created), vec![s(&d.join("proj (2)"))]);

        let multi = d.join("bundle.tar.gz");
        let raw = tar_bytes(&[("one.txt", b"1"), ("two/three.txt", b"3")], &[]);
        let mut enc = flate2::write::GzEncoder::new(File::create(&multi).unwrap(), flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
        let p = TestProgress::default();
        extract_smart_impl(&s(&multi), None, &p).unwrap();
        assert_eq!(fs::read_to_string(d.join("bundle").join("one.txt")).unwrap(), "1");
        assert!(d.join("bundle").join("two").join("three.txt").is_file());
        assert_eq!(*lock(&p.created), vec![s(&d.join("bundle"))]);
        extract_smart_impl(&s(&multi), None, &TestProgress::default()).unwrap();
        assert!(d.join("bundle (2)").join("one.txt").is_file());
    }

    #[test]
    fn zip_slip_is_rejected() {
        let d = tmp("slip");
        let z = d.join("evil.zip");
        make_zip(
            &z,
            &[
                ("../evil.txt", Some(b"pwned")),
                ("ok/../../evil2.txt", Some(b"pwned")),
                ("/abs.txt", Some(b"pwned")),
                ("C:/drive.txt", Some(b"pwned")),
                ("good.txt", Some(b"fine")),
                ("we:ird.txt", Some(b"ads")),
            ],
        );
        let z = s(&z);
        assert_eq!(names(&list_children(&z).unwrap()), vec!["good.txt", "we_ird.txt"]);
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        extract_impl(&z, &s(&out), None, None, &TestProgress::default()).unwrap();
        assert!(!d.join("evil.txt").exists());
        assert!(!d.join("evil2.txt").exists());
        assert!(!Path::new(r"C:\drive.txt").exists() || fs::read(r"C:\drive.txt").unwrap() != b"pwned");
        let mut got: Vec<String> = fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        got.sort();
        assert_eq!(got, vec!["good.txt", "we_ird.txt"]);
        assert_eq!(materialize(&format!(r"{z}\..\evil.txt"), None).ok().map(|p| fs::read(p).unwrap()), None);
    }

    #[test]
    fn compress_round_trip() {
        let d = tmp("compress");
        let src = d.join("src");
        fs::create_dir_all(src.join("nested").join("empty")).unwrap();
        fs::write(src.join("a.txt"), "aaaa".repeat(1000)).unwrap();
        fs::write(src.join("nested").join("ünï 文件.txt"), "unicode").unwrap();
        fs::write(src.join("photo.jpg"), [0xFFu8, 0xD8, 0xFF, 0xE0, 1, 2, 3]).unwrap();
        let loose = d.join("loose.md");
        fs::write(&loose, "# loose").unwrap();
        let mtime = SystemTime::now() - Duration::from_secs(86_400 * 3);
        File::options().write(true).open(&loose).unwrap().set_modified(mtime).unwrap();

        let dest = d.join("out.zip");
        let p = TestProgress::default();
        compress_zip_impl(&[s(&src), s(&loose)], &s(&dest), &p).unwrap();
        assert_eq!(*lock(&p.created), vec![s(&dest)]);
        assert_eq!(lock(&p.totals).1, 4);
        // Refuses to overwrite.
        assert!(compress_zip_impl(&[s(&src)], &s(&dest), &TestProgress::default()).is_err());

        let zs = s(&dest);
        assert_eq!(names(&list_children(&zs).unwrap()), vec!["loose.md", "src"]);
        assert_eq!(names(&list_children(&format!(r"{zs}\src")).unwrap()), vec!["a.txt", "nested", "photo.jpg"]);
        assert_eq!(names(&list_children(&format!(r"{zs}\src\nested")).unwrap()), vec!["empty", "ünï 文件.txt"]);
        let l = list_children(&zs).unwrap();
        let md = l.iter().find(|e| e.name == "loose.md").unwrap();
        assert!((md.modified.unwrap() - system_to_ms(mtime).unwrap()).abs() <= 2000);
        {
            let mut zip = open_zip(&zs).unwrap();
            assert_eq!(zip.by_name("src/photo.jpg").unwrap().compression(), zip::CompressionMethod::Stored);
            assert_eq!(zip.by_name("src/a.txt").unwrap().compression(), zip::CompressionMethod::Deflated);
        }
        let out = d.join("x");
        extract_impl(&zs, &s(&out), None, None, &TestProgress::default()).unwrap();
        assert_eq!(fs::read(out.join("src").join("a.txt")).unwrap(), fs::read(src.join("a.txt")).unwrap());
        assert_eq!(fs::read_to_string(out.join("src").join("nested").join("ünï 文件.txt")).unwrap(), "unicode");
        assert!(out.join("src").join("nested").join("empty").is_dir());
    }

    #[test]
    fn compress_cancel_removes_partial_zip() {
        let d = tmp("cancel");
        let big = d.join("big.bin");
        let data: Vec<u8> = (0..6_000_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        fs::write(&big, &data).unwrap();
        let dest = d.join("big.zip");
        let p = TestProgress { cancel_after: Some(1_000_000), ..Default::default() };
        assert_eq!(compress_zip_impl(&[s(&big)], &s(&dest), &p).unwrap_err(), CANCELLED);
        assert!(!dest.exists());
        assert!(lock(&p.created).is_empty());

        // Extraction cancel removes the half-written file too.
        let z = d.join("ok.zip");
        compress_zip_impl(&[s(&big)], &s(&z), &TestProgress::default()).unwrap();
        let out = d.join("out");
        let p = TestProgress { cancel_after: Some(1_000_000), ..Default::default() };
        assert_eq!(extract_impl(&s(&z), &s(&out), None, None, &p).unwrap_err(), CANCELLED);
        assert!(!out.join("big.bin").exists());
    }

    #[test]
    fn nested_archives() {
        let d = tmp("nested");
        let inner = d.join("inner.zip");
        make_zip(&inner, &[("deep/x.txt", Some(b"nested x"))]);
        let outer = d.join("outer.zip");
        let inner_bytes = fs::read(&inner).unwrap();
        make_zip(&outer, &[("pack/inner.zip", Some(&inner_bytes)), ("readme", Some(b"r"))]);
        let o = s(&outer);
        let nested = format!(r"{o}\pack\inner.zip");
        assert_eq!(names(&list_children(&nested).unwrap()), vec!["deep"]);
        let x = list_children(&format!(r"{nested}\deep")).unwrap();
        assert_eq!(x[0].path, format!(r"{nested}\deep\x.txt"));
        assert_eq!(fs::read_to_string(materialize(&format!(r"{nested}\deep\x.txt"), None).unwrap()).unwrap(), "nested x");
        // Materializing the nested archive itself yields the .zip file.
        let m = materialize(&nested, None).unwrap();
        assert_eq!(fs::read(&m).unwrap(), inner_bytes);
        assert_eq!(info_impl(&nested).unwrap().files, 1);
        let out = d.join("out");
        extract_impl(&nested, &s(&out), None, None, &TestProgress::default()).unwrap();
        assert!(out.join("deep").join("x.txt").is_file());
    }

    #[test]
    fn materialize_reuses_cache() {
        let d = tmp("mat");
        let z = s(&sample_zip(&d));
        let f = materialize(&format!(r"{z}\docs\readme.txt"), None).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "read me");
        assert!(f.starts_with(&s(&cache_base())));
        assert!(f.ends_with(r"docs\readme.txt"));
        let t1 = fs::metadata(&f).unwrap().modified().unwrap();
        // Mark the cached copy: a reuse must not rewrite it.
        File::options().write(true).open(&f).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000_000)).unwrap();
        let f2 = materialize(&format!(r"{z}\DOCS\readme.txt"), None).unwrap();
        assert_eq!(f, f2);
        let t2 = fs::metadata(&f2).unwrap().modified().unwrap();
        assert_ne!(t1, t2);
        assert_eq!(t2, UNIX_EPOCH + Duration::from_secs(1_000_000_000));
        // Folders materialize as a tree.
        let dir = materialize(&format!(r"{z}\docs"), None).unwrap();
        assert!(Path::new(&dir).join("sub").join("deep.txt").is_file());
        // The archive itself is already real.
        assert_eq!(materialize(&z, None).unwrap(), z);
        assert_eq!(materialize(&format!(r"{z}\missing.txt"), None).unwrap_err(), NOT_FOUND);
    }

    #[test]
    fn large_zip_listing_is_cached() {
        let d = tmp("large");
        let z = d.join("many.zip");
        {
            let mut w = zip::ZipWriter::new(BufWriter::new(File::create(&z).unwrap()));
            let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for i in 0..20_000 {
                w.start_file(format!("dir{:03}/file{i}.txt", i % 200), opts).unwrap();
                w.write_all(b"x").unwrap();
            }
            w.finish().unwrap();
        }
        let zs = s(&z);
        let t = Instant::now();
        let first = list_children(&zs).unwrap();
        let cold = t.elapsed();
        assert_eq!(first.len(), 200);
        let t = Instant::now();
        let second = list_children(&zs).unwrap();
        let warm = t.elapsed();
        assert_eq!(second.len(), 200);
        let t = Instant::now();
        assert_eq!(list_children(&format!(r"{zs}\dir007")).unwrap().len(), 100);
        let nav = t.elapsed();
        eprintln!("20k-entry zip: cold {cold:?}, warm {warm:?}, subdir {nav:?}");
        let (_, a) = load_index(&zs, None).unwrap();
        let (_, b) = load_index(&zs, None).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second load must be a cache hit");
        assert!(warm < cold, "cached listing ({warm:?}) should beat the first ({cold:?})");
        assert!(warm < Duration::from_millis(50));
        assert!(nav < Duration::from_millis(50));
    }

    #[test]
    fn modified_archive_is_reindexed() {
        let d = tmp("reindex");
        let z = d.join("r.zip");
        make_zip(&z, &[("one.txt", Some(b"1"))]);
        assert_eq!(names(&list_children(&s(&z)).unwrap()), vec!["one.txt"]);
        std::thread::sleep(Duration::from_millis(20));
        fs::remove_file(&z).unwrap();
        make_zip(&z, &[("one.txt", Some(b"1")), ("two.txt", Some(b"22"))]);
        assert_eq!(names(&list_children(&s(&z)).unwrap()), vec!["one.txt", "two.txt"]);
    }

    fn unrar_data() -> Option<PathBuf> {
        let home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))?;
        for reg in fs::read_dir(home.join("registry").join("src")).ok()?.flatten() {
            for c in fs::read_dir(reg.path()).ok()?.flatten() {
                let n = c.file_name().to_string_lossy().into_owned();
                if n.starts_with("unrar-") && c.path().join("data").join("version.rar").is_file() {
                    return Some(c.path().join("data"));
                }
            }
        }
        None
    }

    #[test]
    fn rar_archives() {
        let Some(data) = unrar_data() else {
            eprintln!("unrar test data not found; skipping RAR test");
            return;
        };
        let d = tmp("rar");
        for f in ["version.rar", "crypted.rar", "comment-hpw-password.rar"] {
            fs::copy(data.join(f), d.join(f)).unwrap();
        }
        let v = s(&d.join("version.rar"));
        let l = list_children(&v).unwrap();
        assert_eq!(names(&l), vec!["VERSION"]);
        assert!(l[0].modified.is_some());
        assert_eq!(fs::read_to_string(materialize(&format!(r"{v}\VERSION"), None).unwrap()).unwrap(), "unrar-0.4.0");
        let out = d.join("out");
        let p = TestProgress::default();
        extract_impl(&v, &s(&out), None, None, &p).unwrap();
        assert_eq!(fs::read_to_string(out.join("VERSION")).unwrap(), "unrar-0.4.0");
        assert_eq!(*lock(&p.created), vec![s(&out.join("VERSION"))]);
        assert_eq!(info_impl(&v).unwrap().format, "RAR");

        // Encrypted file data: listing works, extraction needs the password.
        let c = s(&d.join("crypted.rar"));
        assert_eq!(names(&list_children(&c).unwrap()), vec![".gitignore"]);
        assert!(info_impl(&c).unwrap().encrypted);
        assert_eq!(check_password(&c, None, None).unwrap_err(), PASSWORD_REQUIRED);
        assert_eq!(materialize(&format!(r"{c}\.gitignore"), None).unwrap_err(), PASSWORD_REQUIRED);
        let bad = d.join("bad");
        assert_eq!(extract_impl(&c, &s(&bad), None, Some("nope"), &TestProgress::default()).unwrap_err(), WRONG_PASSWORD);
        assert!(!bad.join(".gitignore").exists());
        let good = d.join("good");
        extract_impl(&c, &s(&good), None, Some("unrar"), &TestProgress::default()).unwrap();
        assert_eq!(fs::read_to_string(good.join(".gitignore")).unwrap(), "target\nCargo.lock\n");

        // Encrypted headers: listing needs the password.
        let h = s(&d.join("comment-hpw-password.rar"));
        assert_eq!(list_err(&h), PASSWORD_REQUIRED);
        assert_eq!(unlock(&h, "wrong").unwrap_err(), WRONG_PASSWORD);
        unlock(&h, "password").unwrap();
        assert_eq!(names(&list_children(&h).unwrap()), vec![".gitignore"]);
        assert_eq!(fs::read_to_string(materialize(&format!(r"{h}\.gitignore"), None).unwrap()).unwrap(), "target\nCargo.lock\n");
    }
}
