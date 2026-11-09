//! MFT provider (needs administrator / LocalSystem): enumerates every file
//! record of an NTFS volume through the USN journal (`FSCTL_ENUM_USN_DATA`),
//! then tails the journal (`FSCTL_READ_USN_JOURNAL`) to stay live. Snapshots
//! go to `%PROGRAMDATA%\Ferret\index-<serial>.bin` together with the journal
//! id + next USN so a restart only replays what changed.
//!
//! The record parser is a pure function (`parse_records`) and is unit-tested
//! against hand-built USN_RECORD_V2 / V3 buffers.

use super::engine::{Engine, Volume};
use super::index::{flags::*, Index, NONE};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;

pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub const USN_REASON_CLOSE: u32 = 0x8000_0000;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    /// File reference number (low 64 bits for V3 / FILE_ID_128).
    pub frn: u64,
    pub parent: u64,
    pub usn: i64,
    /// FILETIME of the change.
    pub timestamp: i64,
    pub reason: u32,
    pub attrs: u32,
    pub name: String,
}

/// MFT record number (strips the 16-bit sequence number).
#[inline]
pub fn recno(frn: u64) -> u64 {
    frn & 0x0000_FFFF_FFFF_FFFF
}

pub fn attr_flags(attrs: u32) -> u8 {
    let mut fl = 0;
    if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        fl |= DIR;
    }
    if attrs & FILE_ATTRIBUTE_HIDDEN != 0 {
        fl |= HIDDEN;
    }
    if attrs & FILE_ATTRIBUTE_SYSTEM != 0 {
        fl |= SYSTEM;
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        fl |= LINK;
    }
    fl
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Parses one USN_RECORD_V2 / V3 at the start of `b`. Returns the record and
/// its `RecordLength`; `None` for truncated / malformed input. Records of an
/// unknown major version yield `(None, len)` so callers can skip them.
pub fn parse_record(b: &[u8]) -> Option<(Option<UsnRecord>, usize)> {
    let len = u32_at(b, 0)? as usize;
    if len < 8 || len > b.len() {
        return None;
    }
    let major = u16_at(b, 4)?;
    let r = &b[..len];
    // Field offsets: V2 has 64-bit FRNs, V3 FILE_ID_128 (we keep the low half,
    // which is the NTFS file reference number).
    let (frn, parent, base) = match major {
        2 => (u64_at(r, 8)?, u64_at(r, 16)?, 24),
        3 => (u64_at(r, 8)?, u64_at(r, 24)?, 40),
        _ => return Some((None, len)),
    };
    let usn = u64_at(r, base)? as i64;
    let timestamp = u64_at(r, base + 8)? as i64;
    let reason = u32_at(r, base + 16)?;
    let attrs = u32_at(r, base + 28)?;
    let name_len = u16_at(r, base + 32)? as usize;
    let name_off = u16_at(r, base + 34)? as usize;
    let raw = r.get(name_off..name_off + name_len)?;
    let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let name = String::from_utf16_lossy(&units);
    Some((Some(UsnRecord { frn, parent, usn, timestamp, reason, attrs, name }), len))
}

/// Parses a packed sequence of records (the output buffer after its leading
/// 8-byte USN / FRN).
pub fn parse_records(mut b: &[u8]) -> Vec<UsnRecord> {
    let mut out = Vec::new();
    while b.len() >= 8 {
        let Some((rec, len)) = parse_record(b) else { break };
        if let Some(r) = rec {
            out.push(r);
        }
        // Records are 8-byte aligned; RecordLength already includes padding.
        b = &b[len..];
    }
    out
}

/// Applies tailed journal records. Returns how many changed the index.
pub fn apply_records(ix: &mut Index, recs: &[UsnRecord], root_frn: u64) -> usize {
    let mut changed = 0;
    for r in recs {
        if recno(r.frn) == recno(root_frn) {
            continue;
        }
        let rn = recno(r.frn);
        if r.reason & USN_REASON_FILE_DELETE != 0 {
            if let Some(i) = ix.frn_get(rn) {
                ix.remove(i);
                changed += 1;
            }
            ix.frn_clear(rn);
            continue;
        }
        if r.reason & USN_REASON_RENAME_OLD_NAME != 0 && r.reason & USN_REASON_RENAME_NEW_NAME == 0 {
            continue;
        }
        let Some(p) = ix.frn_get(recno(r.parent)) else { continue };
        if !ix.is_dir(p) {
            continue;
        }
        let fl = attr_flags(r.attrs);
        match ix.frn_get(rn) {
            Some(i) => {
                if ix.parent_of(i) != p || ix.name(i) != r.name {
                    ix.rename(i, p, &r.name);
                    changed += 1;
                }
                if ix.flags_of(i) & !(DEAD | ROOT) != fl {
                    ix.set_flags(i, fl);
                    changed += 1;
                }
            }
            None => {
                let i = ix.insert(p, &r.name, fl, 0, 0);
                ix.frn_set(rn, i);
                changed += 1;
            }
        }
    }
    changed
}

/// Builds an index from enumerated records (any order).
pub fn build_from_records(root_path: &str, root_frn: u64, recs: impl IntoIterator<Item = UsnRecord>) -> Index {
    let mut b = Builder::new(root_path, root_frn, 0);
    for r in recs {
        b.push(&r);
    }
    b.finish()
}

/// Incremental builder used during enumeration (parents may come later).
pub struct Builder {
    ix: Index,
    parents: Vec<u64>,
    root_frn: u64,
}

impl Builder {
    pub fn new(root_path: &str, root_frn: u64, expect: usize) -> Self {
        let mut ix = Index::with_capacity(root_path, false, expect + 1, expect * 22);
        ix.frn_set(recno(root_frn), 0);
        let mut parents = Vec::with_capacity(expect + 1);
        parents.push(u64::MAX);
        Builder { ix, parents, root_frn }
    }

    pub fn push(&mut self, r: &UsnRecord) {
        if recno(r.frn) == recno(self.root_frn) || r.reason & USN_REASON_FILE_DELETE != 0 {
            return;
        }
        let i = self.ix.push_unlinked(NONE, &r.name, attr_flags(r.attrs), 0, 0);
        self.parents.push(r.parent);
        self.ix.frn_set(recno(r.frn), i);
    }

    pub fn len(&self) -> usize {
        self.parents.len()
    }

    pub fn finish(mut self) -> Index {
        for i in 1..self.parents.len() {
            let p = self.ix.frn_map.get(recno(self.parents[i]) as usize).copied().unwrap_or(NONE);
            self.ix.parent[i] = p;
        }
        // Links children and drops records that don't reach the root
        // (metadata files under $Extend, orphans).
        self.ix.relink();
        let mut ix = if self.ix.needs_compaction() || self.ix.garbage_ratio() > 0.01 {
            self.ix.compacted()
        } else {
            self.ix
        };
        ix.shrink_to_fit();
        ix
    }
}

// ====================================================================== Win32

#[derive(Debug)]
pub enum MftError {
    /// Opening the raw volume needs administrator rights.
    AccessDenied(String),
    NotNtfs(String),
    JournalWrapped,
    JournalGone,
    Io(String),
}

impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MftError::AccessDenied(v) => write!(
                f,
                "access denied opening {v}: reading the MFT/USN journal requires administrator rights \
                 (run elevated, or install the FerretIndexer service with `ferret-indexer --install`)"
            ),
            MftError::NotNtfs(v) => write!(f, "{v} is not an NTFS volume"),
            MftError::JournalWrapped => write!(f, "USN journal wrapped past the saved position"),
            MftError::JournalGone => write!(f, "USN journal was deleted or disabled"),
            MftError::Io(e) => write!(f, "{e}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct JournalInfo {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED,
        ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, GetVolumeInformationW, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_USN_JOURNAL,
        FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    #[repr(C)]
    struct MftEnumDataV0 {
        start_frn: u64,
        low_usn: i64,
        high_usn: i64,
    }

    #[repr(C)]
    struct ReadUsnJournalDataV0 {
        start_usn: i64,
        reason_mask: u32,
        return_only_on_close: u32,
        timeout: u64,
        bytes_to_wait_for: u64,
        usn_journal_id: u64,
    }

    #[repr(C)]
    struct CreateUsnJournalData {
        maximum_size: u64,
        allocation_delta: u64,
    }

    /// Owned volume handle.
    pub struct VolHandle {
        h: HANDLE,
        pub letter: char,
    }

    unsafe impl Send for VolHandle {}

    impl Drop for VolHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.h);
            }
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub(crate) fn map_err(e: windows::core::Error, what: &str) -> MftError {
        let c = e.code();
        if c == ERROR_ACCESS_DENIED.to_hresult() {
            MftError::AccessDenied(what.to_string())
        } else if c == ERROR_JOURNAL_ENTRY_DELETED.to_hresult() {
            MftError::JournalWrapped
        } else if c == ERROR_JOURNAL_NOT_ACTIVE.to_hresult() || c == ERROR_JOURNAL_DELETE_IN_PROGRESS.to_hresult() {
            MftError::JournalGone
        } else {
            MftError::Io(format!("{what}: {e}"))
        }
    }

    /// (serial, filesystem name) of `C:\`.
    pub fn volume_info(root: &str) -> Option<(u32, String)> {
        let mut serial = 0u32;
        let mut fs = [0u16; 64];
        unsafe {
            GetVolumeInformationW(PCWSTR(wide(root).as_ptr()), None, Some(&mut serial), None, None, Some(&mut fs)).ok()?;
        }
        let n = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        Some((serial, String::from_utf16_lossy(&fs[..n])))
    }

    /// Fixed NTFS volumes, e.g. `["C:\"]`.
    pub fn ntfs_fixed_volumes() -> Vec<String> {
        crate::search::walk::fixed_drives()
            .into_iter()
            .filter(|r| volume_info(r).is_some_and(|(_, fs)| fs.eq_ignore_ascii_case("NTFS")))
            .collect()
    }

    pub fn open_volume(letter: char) -> Result<VolHandle, MftError> {
        open_volume_with(letter, GENERIC_READ.0)
    }

    fn open_volume_with(letter: char, access: u32) -> Result<VolHandle, MftError> {
        let path = format!(r"\\.\{letter}:");
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide(&path).as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        }
        .map_err(|e| map_err(e, &path))?;
        Ok(VolHandle { h, letter })
    }

    /// File reference number of the volume root directory.
    pub fn root_frn(root: &str) -> Result<u64, MftError> {
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide(root).as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|e| map_err(e, root))?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let r = unsafe { GetFileInformationByHandle(h, &mut info) };
        unsafe {
            let _ = CloseHandle(h);
        }
        r.map_err(|e| map_err(e, root))?;
        Ok(((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64)
    }

    impl VolHandle {
        /// Reads the volume at a byte offset (a multiple of its sector size).
        pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> windows::core::Result<usize> {
            use windows::Win32::Storage::FileSystem::ReadFile;
            use windows::Win32::System::IO::OVERLAPPED;
            let mut ov = OVERLAPPED::default();
            ov.Anonymous.Anonymous.Offset = offset as u32;
            ov.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
            let mut got = 0u32;
            unsafe { ReadFile(self.h, Some(buf), Some(&mut got), Some(&mut ov))? };
            Ok(got as usize)
        }

        pub(crate) fn ioctl(&self, code: u32, input: Option<&[u8]>, out: &mut [u8]) -> windows::core::Result<usize> {
            let mut got = 0u32;
            unsafe {
                DeviceIoControl(
                    self.h,
                    code,
                    input.map(|i| i.as_ptr() as *const _),
                    input.map_or(0, |i| i.len() as u32),
                    Some(out.as_mut_ptr() as *mut _),
                    out.len() as u32,
                    Some(&mut got),
                    None,
                )?;
            }
            Ok(got as usize)
        }

        pub fn query_journal(&self) -> Result<JournalInfo, MftError> {
            let mut out = [0u8; 80];
            let n = self.ioctl(FSCTL_QUERY_USN_JOURNAL, None, &mut out).map_err(|e| map_err(e, "FSCTL_QUERY_USN_JOURNAL"))?;
            if n < 32 {
                return Err(MftError::Io("short FSCTL_QUERY_USN_JOURNAL reply".into()));
            }
            let g = |o: usize| u64::from_le_bytes(out[o..o + 8].try_into().unwrap());
            Ok(JournalInfo { id: g(0), first_usn: g(8) as i64, next_usn: g(16) as i64, lowest_valid_usn: g(24) as i64 })
        }

        /// Queries the journal, creating it (32 MiB) if the volume has none.
        pub fn query_or_create_journal(&self) -> Result<JournalInfo, MftError> {
            match self.query_journal() {
                Err(MftError::JournalGone) => {
                    let data = CreateUsnJournalData { maximum_size: 32 << 20, allocation_delta: 4 << 20 };
                    let bytes = unsafe {
                        std::slice::from_raw_parts(&data as *const _ as *const u8, std::mem::size_of::<CreateUsnJournalData>())
                    };
                    self.ioctl(FSCTL_CREATE_USN_JOURNAL, Some(bytes), &mut [])
                        .map_err(|e| map_err(e, "FSCTL_CREATE_USN_JOURNAL"))?;
                    self.query_journal()
                }
                r => r,
            }
        }

        /// Total MFT records (for progress), if available.
        pub fn mft_records(&self) -> Option<u64> {
            let mut out = [0u8; 128];
            let n = self.ioctl(FSCTL_GET_NTFS_VOLUME_DATA, None, &mut out).ok()?;
            if n < 64 {
                return None;
            }
            let per = u32::from_le_bytes(out[48..52].try_into().unwrap()) as u64;
            let valid = u64::from_le_bytes(out[56..64].try_into().unwrap());
            (per > 0).then(|| valid / per)
        }

        /// Enumerates every record via FSCTL_ENUM_USN_DATA into a fresh index.
        pub fn enumerate(
            &self,
            root_path: &str,
            root_frn: u64,
            high_usn: i64,
            stop: &AtomicBool,
            mut progress: impl FnMut(f32, usize),
        ) -> Result<Index, MftError> {
            let total = self.mft_records().unwrap_or(0);
            let mut b = Builder::new(root_path, root_frn, (total as usize).min(50_000_000));
            let mut buf = vec![0u64; 128 * 1024]; // 1 MiB, 8-byte aligned
            let mut med = MftEnumDataV0 { start_frn: 0, low_usn: 0, high_usn };
            let mut last = Instant::now();
            loop {
                if stop.load(Relaxed) {
                    return Err(MftError::Io("stopped".into()));
                }
                let input = unsafe {
                    std::slice::from_raw_parts(&med as *const _ as *const u8, std::mem::size_of::<MftEnumDataV0>())
                };
                let out = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
                let n = match self.ioctl(FSCTL_ENUM_USN_DATA, Some(input), out) {
                    Ok(n) => n,
                    Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => break,
                    Err(e) => return Err(map_err(e, "FSCTL_ENUM_USN_DATA")),
                };
                if n <= 8 {
                    break;
                }
                let next = u64::from_le_bytes(out[0..8].try_into().unwrap());
                for r in parse_records(&out[8..n]) {
                    b.push(&r);
                }
                med.start_frn = next;
                if last.elapsed() > Duration::from_millis(200) {
                    last = Instant::now();
                    let p = if total > 0 { recno(next) as f32 / total as f32 } else { 0.0 };
                    progress(p.min(0.99), b.len());
                }
            }
            Ok(b.finish())
        }

        /// Reads journal records after `start`; returns (next usn, records).
        pub fn read_journal(&self, journal_id: u64, start: i64, buf: &mut [u64]) -> Result<(i64, Vec<UsnRecord>), MftError> {
            let rd = ReadUsnJournalDataV0 {
                start_usn: start,
                reason_mask: 0xFFFF_FFFF,
                return_only_on_close: 0,
                timeout: 0,
                bytes_to_wait_for: 0,
                usn_journal_id: journal_id,
            };
            let input = unsafe {
                std::slice::from_raw_parts(&rd as *const _ as *const u8, std::mem::size_of::<ReadUsnJournalDataV0>())
            };
            let out = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 8) };
            let n = self.ioctl(FSCTL_READ_USN_JOURNAL, Some(input), out).map_err(|e| map_err(e, "FSCTL_READ_USN_JOURNAL"))?;
            if n < 8 {
                return Ok((start, Vec::new()));
            }
            let next = i64::from_le_bytes(out[0..8].try_into().unwrap());
            Ok((next, parse_records(&out[8..n])))
        }
    }

    /// What works without elevation on a volume (diagnostics for `--probe`).
    pub fn probe(root: &str) -> Vec<String> {
        let letter = root.chars().next().unwrap_or('C');
        let mut out = Vec::new();
        let vol = format!(r"\\.\{letter}:");
        match open_volume(letter) {
            Ok(v) => {
                out.push(format!("open {vol} GENERIC_READ: ok"));
                probe_handle(&v, &mut out);
            }
            Err(e) => out.push(format!("open {vol} GENERIC_READ: {e}")),
        }
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        for (label, access) in [("no access rights", 0u32), ("FILE_READ_ATTRIBUTES", FILE_READ_ATTRIBUTES)] {
            match open_volume_with(letter, access) {
                Err(e) => out.push(format!("open {vol} ({label}): {e}")),
                Ok(v) => {
                    out.push(format!("open {vol} ({label}): ok"));
                    probe_handle(&v, &mut out);
                }
            }
        }
        // Root directory handle: the documented route for unprivileged reads.
        match unsafe {
            CreateFileW(
                PCWSTR(wide(root).as_ptr()),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        } {
            Err(e) => out.push(format!("open {root} (directory): {e}")),
            Ok(h) => {
                out.push(format!("open {root} (directory, FILE_READ_ATTRIBUTES): ok"));
                probe_handle(&VolHandle { h, letter }, &mut out);
            }
        }
        out
    }

    fn probe_handle(v: &VolHandle, out: &mut Vec<String>) {
        let journal = match v.query_journal() {
            Ok(j) => {
                out.push(format!("    FSCTL_QUERY_USN_JOURNAL: ok (journal {:#x}, next usn {})", j.id, j.next_usn));
                Some(j)
            }
            Err(e) => {
                out.push(format!("    FSCTL_QUERY_USN_JOURNAL: {e}"));
                None
            }
        };
        let med = MftEnumDataV0 { start_frn: 0, low_usn: 0, high_usn: i64::MAX };
        let input =
            unsafe { std::slice::from_raw_parts(&med as *const _ as *const u8, std::mem::size_of::<MftEnumDataV0>()) };
        let mut buf = vec![0u8; 64 * 1024];
        match v.ioctl(FSCTL_ENUM_USN_DATA, Some(input), &mut buf) {
            Ok(n) => out.push(format!("    FSCTL_ENUM_USN_DATA: ok ({} records in first batch)", parse_records(&buf[8..n.max(8)]).len())),
            Err(e) => out.push(format!("    FSCTL_ENUM_USN_DATA: {}", map_err(e, "FSCTL_ENUM_USN_DATA"))),
        }
        let Some(j) = journal else { return };
        let rd = ReadUsnJournalDataV0 {
            start_usn: j.next_usn.saturating_sub(256 * 1024).max(j.first_usn),
            reason_mask: 0xFFFF_FFFF,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: j.id,
        };
        let input =
            unsafe { std::slice::from_raw_parts(&rd as *const _ as *const u8, std::mem::size_of::<ReadUsnJournalDataV0>()) };
        for (name, code) in [
            ("FSCTL_READ_USN_JOURNAL", FSCTL_READ_USN_JOURNAL),
            ("FSCTL_READ_UNPRIVILEGED_USN_JOURNAL", FSCTL_READ_UNPRIVILEGED_USN_JOURNAL),
        ] {
            match v.ioctl(code, Some(input), &mut buf) {
                Ok(n) => {
                    let recs = parse_records(&buf[8..n.max(8)]);
                    let named = recs.iter().filter(|r| !r.name.is_empty()).count();
                    let sample = recs.iter().rev().find(|r| !r.name.is_empty()).map(|r| r.name.clone()).unwrap_or_default();
                    out.push(format!("    {name}: ok ({} records, {named} with names, e.g. {sample:?})", recs.len()))
                }
                Err(e) => out.push(format!("    {name}: {}", map_err(e, name))),
            }
        }
        // READ_USN_JOURNAL_DATA_V1 (adds Min/MaxMajorVersion).
        #[repr(C)]
        struct ReadV1 {
            v0: ReadUsnJournalDataV0,
            min_major: u16,
            max_major: u16,
        }
        let v1 = ReadV1 { v0: rd, min_major: 2, max_major: 3 };
        let input = unsafe { std::slice::from_raw_parts(&v1 as *const _ as *const u8, std::mem::size_of::<ReadV1>()) };
        match v.ioctl(FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, Some(input), &mut buf) {
            Ok(n) => {
                let recs = parse_records(&buf[8..n.max(8)]);
                let named = recs.iter().filter(|r| !r.name.is_empty()).count();
                let sample = recs.iter().rev().find(|r| !r.name.is_empty()).map(|r| r.name.clone()).unwrap_or_default();
                out.push(format!(
                    "    FSCTL_READ_UNPRIVILEGED_USN_JOURNAL (V1 input): ok ({} records, {named} with names, e.g. {sample:?})",
                    recs.len()
                ))
            }
            Err(e) => out.push(format!("    FSCTL_READ_UNPRIVILEGED_USN_JOURNAL (V1 input): {}", map_err(e, "read"))),
        }
    }

    // ------------------------------------------------------------ snapshot

    const SNAP_MAGIC: &[u8; 8] = b"FRTMFT01";

    pub fn snapshot_path(dir: &Path, serial: u32) -> PathBuf {
        dir.join(format!("index-{serial:08x}.bin"))
    }

    pub fn save_snapshot(path: &Path, ix: &Index, j: &JournalInfo, next_usn: i64, root_frn: u64) -> std::io::Result<()> {
        use std::io::Write;
        let tmp = path.with_extension("tmp");
        {
            let mut w = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
            w.write_all(SNAP_MAGIC)?;
            w.write_all(&j.id.to_le_bytes())?;
            w.write_all(&next_usn.to_le_bytes())?;
            w.write_all(&root_frn.to_le_bytes())?;
            ix.write_snapshot(&mut w)?;
            w.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// (index, journal id, next usn, root frn)
    pub fn load_snapshot(path: &Path) -> std::io::Result<(Index, u64, i64, u64)> {
        use std::io::Read;
        let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
        let mut h = [0u8; 32];
        r.read_exact(&mut h)?;
        if &h[..8] != SNAP_MAGIC {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad mft snapshot"));
        }
        let g = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
        let ix = Index::read_snapshot(&mut r)?;
        Ok((ix, g(8), g(16) as i64, g(24)))
    }

    /// Indexes one NTFS volume and keeps it live until `stop`. Registers the
    /// volume with `engine` as soon as a usable index exists.
    pub fn run_volume(
        engine: &Arc<Engine>,
        root: &str,
        state_dir: &Path,
        stop: &AtomicBool,
        log: &(dyn Fn(String) + Sync),
    ) -> Result<(), MftError> {
        let letter = root.chars().next().unwrap_or('C').to_ascii_uppercase();
        let (serial, fs) = volume_info(root).ok_or_else(|| MftError::Io(format!("GetVolumeInformation({root}) failed")))?;
        if !fs.eq_ignore_ascii_case("NTFS") {
            return Err(MftError::NotNtfs(root.to_string()));
        }
        let h = open_volume(letter)?;
        let root_frn = root_frn(root)?;
        let snap = snapshot_path(state_dir, serial);
        let mut journal = h.query_or_create_journal()?;

        // Fast path: snapshot + journal replay.
        let mut resume: Option<(Index, i64)> = None;
        if let Ok((ix, jid, next, rf)) = load_snapshot(&snap) {
            if jid == journal.id && rf == root_frn && next >= journal.first_usn.max(journal.lowest_valid_usn) && next <= journal.next_usn {
                log(format!("{root}: loaded snapshot ({} entries), replaying journal from usn {next}", ix.live()));
                resume = Some((ix, next));
            } else {
                log(format!("{root}: snapshot stale (journal changed or wrapped); re-enumerating"));
            }
        }
        let vol;
        let mut next_usn;
        match resume {
            Some((ix, next)) => {
                vol = Volume::new(ix, true);
                next_usn = next;
                engine.add_volume(vol.clone());
            }
            None => {
                vol = Volume::new(Index::new(root, false), false);
                engine.add_volume(vol.clone());
                let t = std::time::Instant::now();
                next_usn = journal.next_usn;
                let ix = h.enumerate(root, root_frn, i64::MAX, stop, |p, n| {
                    vol.set_progress(p);
                    vol.set_entries(n as u64);
                })?;
                log(format!("{root}: enumerated {} entries in {:.1}s", ix.live(), t.elapsed().as_secs_f64()));
                *vol.index.write() = ix;
                vol.set_entries(vol.index.read().live() as u64);
                vol.ready.store(true, Relaxed);
                vol.set_progress(1.0);
                let _ = save_snapshot(&snap, &vol.index.read(), &journal, next_usn, root_frn);
            }
        }

        // Every folder's size, from the file table, kept current with the journal below; read
        // again now and then, in case the two ever drift apart.
        let mut sized = read_sizes(&h, &vol, root, stop, log);
        let mut sizes_tried = Instant::now();

        // Tail the journal.
        let mut buf = vec![0u64; 64 * 1024];
        let mut last_save = Instant::now();
        let mut dirty = false;
        while !stop.load(Relaxed) {
            match h.read_journal(journal.id, next_usn, &mut buf) {
                Ok((next, recs)) => {
                    if !recs.is_empty() {
                        let mut ix = vol.index.write();
                        if apply_records(&mut ix, &recs, root_frn) > 0 {
                            dirty = true;
                        }
                        if ix.needs_compaction() {
                            ix.compact();
                        }
                        drop(ix);
                        // What changed, looked at again: its old share of each folder above it
                        // out, its new one in.
                        if sized.is_some() {
                            let mut changed: Vec<u32> = recs.iter().filter_map(|r| u32::try_from(recno(r.frn)).ok()).collect();
                            changed.sort_unstable();
                            changed.dedup();
                            match crate::search::sizes::look_up(&h, &changed) {
                                Ok(now) => {
                                    if let Some(tree) = vol.sizes.write().as_mut() {
                                        tree.update_all(now);
                                    }
                                }
                                Err(e) => log(format!("{root}: folder sizes: {e}")),
                            }
                        }
                    }
                    let advanced = next != next_usn;
                    next_usn = next;
                    if advanced {
                        continue; // drain without sleeping
                    }
                }
                Err(MftError::JournalWrapped) | Err(MftError::JournalGone) => {
                    log(format!("{root}: journal reset; re-enumerating"));
                    journal = h.query_or_create_journal()?;
                    next_usn = journal.next_usn;
                    let ix = h.enumerate(root, root_frn, i64::MAX, stop, |_, _| {})?;
                    *vol.index.write() = ix;
                    dirty = true;
                    sized = read_sizes(&h, &vol, root, stop, log);
                    sizes_tried = Instant::now();
                }
                Err(e) => {
                    log(format!("{root}: {e}"));
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
            if dirty && last_save.elapsed() > Duration::from_secs(300) {
                let _ = save_snapshot(&snap, &vol.index.read(), &journal, next_usn, root_frn);
                last_save = Instant::now();
                dirty = false;
            }
            // Sizes read again every 6 hours (a failed read: every hour).
            let again = if sized.is_some() { 6 * 3600 } else { 3600 };
            if sizes_tried.elapsed() > Duration::from_secs(again) {
                sized = read_sizes(&h, &vol, root, stop, log).or(sized);
                sizes_tried = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = save_snapshot(&snap, &vol.index.read(), &journal, next_usn, root_frn);
        Ok(())
    }

    /// Reads every folder's size from the file table into `vol` (sizes.rs); when, if it did.
    fn read_sizes(h: &VolHandle, vol: &Volume, root: &str, stop: &AtomicBool, log: &(dyn Fn(String) + Sync)) -> Option<Instant> {
        match crate::search::sizes::scan(h, stop) {
            Ok((tree, st)) => {
                let all = tree.dir(crate::search::sizes::ROOT).copied().unwrap_or_default();
                log(format!(
                    "{root}: folder sizes from {} records in {:.1}s: {} folders, {} files, {:.0} MB in memory",
                    st.records,
                    st.took.as_secs_f64(),
                    tree.dir_count(),
                    all.files,
                    tree.heap_bytes() as f64 / 1e6
                ));
                *vol.sizes.write() = Some(tree);
                Some(Instant::now())
            }
            Err(e) => {
                log(format!("{root}: folder sizes: {e}"));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2(frn: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
        let n: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let len = (60 + n.len() + 7) & !7;
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes());
        b[16..24].copy_from_slice(&parent.to_le_bytes());
        b[24..32].copy_from_slice(&usn.to_le_bytes());
        b[32..40].copy_from_slice(&132_000_000_000_000_000i64.to_le_bytes());
        b[40..44].copy_from_slice(&reason.to_le_bytes());
        b[52..56].copy_from_slice(&attrs.to_le_bytes());
        b[56..58].copy_from_slice(&(n.len() as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        b[60..60 + n.len()].copy_from_slice(&n);
        b
    }

    fn v3(frn: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
        let n: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let len = (76 + n.len() + 7) & !7;
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&3u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes()); // FILE_ID_128 low half
        b[16..24].copy_from_slice(&0xDEADu64.to_le_bytes()); // high half (ignored)
        b[24..32].copy_from_slice(&parent.to_le_bytes());
        b[40..48].copy_from_slice(&usn.to_le_bytes());
        b[48..56].copy_from_slice(&7i64.to_le_bytes());
        b[56..60].copy_from_slice(&reason.to_le_bytes());
        b[68..72].copy_from_slice(&attrs.to_le_bytes());
        b[72..74].copy_from_slice(&(n.len() as u16).to_le_bytes());
        b[74..76].copy_from_slice(&76u16.to_le_bytes());
        b[76..76 + n.len()].copy_from_slice(&n);
        b
    }

    const ROOT: u64 = 0x0005_0000_0000_0005;
    fn frn(rec: u64, seq: u64) -> u64 {
        (seq << 48) | rec
    }

    #[test]
    fn search_mft_parse_v2_v3() {
        let mut buf = v2(frn(40, 1), ROOT, 100, USN_REASON_FILE_CREATE, 0x10, "Users");
        buf.extend(v3(frn(41, 2), frn(40, 1), 200, USN_REASON_RENAME_NEW_NAME | USN_REASON_CLOSE, 0x22, "Ünï.txt"));
        // unknown major version is skipped, not fatal
        let mut odd = v2(1, 1, 1, 0, 0, "x");
        odd[4] = 9;
        buf.extend(odd);
        buf.extend(v2(frn(42, 1), frn(40, 1), 300, 0, 0x6, "pagefile.sys"));
        let recs = parse_records(&buf);
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].name, "Users");
        assert_eq!(recs[0].parent, ROOT);
        assert_eq!(attr_flags(recs[0].attrs), DIR);
        assert_eq!(recs[1].frn, frn(41, 2));
        assert_eq!(recs[1].parent, frn(40, 1));
        assert_eq!(recs[1].usn, 200);
        assert_eq!(recs[1].timestamp, 7);
        assert_eq!(recs[1].name, "Ünï.txt");
        assert_eq!(recs[1].reason, USN_REASON_RENAME_NEW_NAME | USN_REASON_CLOSE);
        assert_eq!(attr_flags(recs[2].attrs), HIDDEN | SYSTEM);
        // truncated input stops cleanly
        assert_eq!(parse_records(&buf[..buf.len() - 3]).len(), 2);
        assert!(parse_records(&[1, 0, 0]).is_empty());
        assert!(parse_record(&[0xff, 0xff, 0, 0, 2, 0, 0, 0]).is_none());
    }

    #[test]
    fn search_mft_build_and_tail() {
        // Children before parents, an orphan, and a $Extend child.
        let recs = vec![
            parse_records(&v2(frn(50, 1), frn(40, 1), 1, 0, 0x20, "notes.txt")),
            parse_records(&v2(frn(40, 1), ROOT, 1, 0, 0x10, "Docs")),
            parse_records(&v2(frn(60, 1), frn(999, 1), 1, 0, 0x20, "orphan.txt")),
            parse_records(&v2(frn(41, 1), ROOT, 1, 0, 0x10, "Src")),
        ]
        .concat();
        let mut ix = build_from_records(r"C:\", ROOT, recs);
        assert_eq!(ix.live(), 3);
        let notes = ix.frn_get(50).unwrap();
        assert_eq!(ix.path(notes), r"C:\Docs\notes.txt");
        assert!(ix.frn_get(60).is_none());

        // create, rename+move, delete folder (children first, like NTFS does)
        let tail = [
            v2(frn(70, 1), frn(41, 1), 2, USN_REASON_FILE_CREATE, 0x20, "main.rs"),
            v2(frn(50, 1), frn(40, 1), 3, USN_REASON_RENAME_OLD_NAME, 0x20, "notes.txt"),
            v2(frn(50, 1), frn(41, 1), 4, USN_REASON_RENAME_NEW_NAME, 0x20, "README.md"),
            v2(frn(80, 1), frn(40, 1), 5, USN_REASON_FILE_CREATE, 0x10, "sub"),
            v2(frn(81, 1), frn(80, 1), 6, USN_REASON_FILE_CREATE, 0x20, "deep.txt"),
            v2(frn(81, 1), frn(80, 1), 7, USN_REASON_FILE_DELETE | USN_REASON_CLOSE, 0x20, "deep.txt"),
            v2(frn(80, 1), frn(40, 1), 8, USN_REASON_FILE_DELETE | USN_REASON_CLOSE, 0x10, "sub"),
            v2(frn(90, 1), frn(12345, 1), 9, USN_REASON_FILE_CREATE, 0x20, "unknown-parent"),
        ]
        .concat();
        let n = apply_records(&mut ix, &parse_records(&tail), ROOT);
        assert!(n >= 5);
        let main = ix.frn_get(70).unwrap();
        assert_eq!(ix.path(main), r"C:\Src\main.rs");
        let readme = ix.frn_get(50).unwrap();
        assert_eq!(ix.path(readme), r"C:\Src\README.md");
        assert!(ix.frn_get(80).is_none() && ix.frn_get(81).is_none() && ix.frn_get(90).is_none());
        assert_eq!(ix.resolve(r"C:\Docs\notes.txt"), None);
        // MFT record reuse with a new sequence number
        let reuse = v2(frn(80, 2), ROOT, 10, USN_REASON_FILE_CREATE, 0x20, "reused.bin");
        apply_records(&mut ix, &parse_records(&reuse), ROOT);
        assert_eq!(ix.path(ix.frn_get(80).unwrap()), r"C:\reused.bin");
        // compaction keeps the FRN map consistent
        ix.compact();
        assert_eq!(ix.path(ix.frn_get(50).unwrap()), r"C:\Src\README.md");
        assert_eq!(ix.path(ix.frn_get(80).unwrap()), r"C:\reused.bin");
    }

    #[cfg(windows)]
    #[test]
    fn search_mft_snapshot_roundtrip() {
        let recs = parse_records(&v2(frn(40, 1), ROOT, 1, 0, 0x10, "Docs"));
        let ix = build_from_records(r"C:\", ROOT, recs);
        let p = std::env::temp_dir().join(format!("ferret-mft-snap-{}.bin", std::process::id()));
        let j = JournalInfo { id: 0xABCD, first_usn: 0, next_usn: 99, lowest_valid_usn: 0 };
        save_snapshot(&p, &ix, &j, 77, ROOT).unwrap();
        let (back, jid, next, rf) = load_snapshot(&p).unwrap();
        assert_eq!((jid, next, rf), (0xABCD, 77, ROOT));
        assert_eq!(back.path(back.frn_get(40).unwrap()), r"C:\Docs");
        let _ = std::fs::remove_file(p);
    }
}
