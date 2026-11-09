//! Folder sizes from the NTFS file table (MFT), for the indexer service.
//!
//! Every file's record says how big it is and which folders it's in, so one pass over the file
//! table gives the size of every folder on the drive, as WizTree does, and the change journal
//! the service already follows keeps them current: a changed file's record is looked at again
//! and the difference added to each folder above it. NTFS reads the table for us
//! (`FSCTL_QUERY_FILE_LAYOUT`: names, streams, security ids), so its record formats, attribute
//! lists and fixups stay NTFS's business.
//!
//! What's counted is what Ferret's own walk counts (foldersize.rs), so the two agree: each
//! name of a file (a hard link counts in each folder it's in) with the size of its main data
//! stream; no symlinks (files or folders: name-surrogate reparse points), and nothing of
//! NTFS's own ($MFT, $Extend…). Which folders a user may see inside is decided per request
//! (ipc.rs): the tree has everything.

use super::index::FastMap;

/// The root folder's record number.
pub const ROOT: u32 = 5;
/// Records below this are NTFS's own (the root aside), which a listing never shows.
const FIRST_USER_RECORD: u32 = 24;
const NONE: u32 = u32::MAX;
/// Deepest a folder can be (a guard against a damaged table's loops).
const MAX_DEPTH: usize = 4096;

pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// The main data stream's attribute type, and the reparse point's.
const DATA: u32 = 0x80;
const REPARSE_POINT: u32 = 0xC0;

/// What the file table says about one file or folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// File reference: sequence number (top 16 bits) and record number.
    pub frn: u64,
    pub attributes: u32,
    /// The folder of each of its names (file references), short DOS names aside: one per hard
    /// link, a folder more than once if it has two names there.
    pub parents: Vec<u64>,
    /// Its main (unnamed) data stream's size.
    pub size: u64,
    pub security: u32,
    /// Its reparse tag, if it's a reparse point.
    pub reparse_tag: u32,
}

impl Record {
    pub fn recno(&self) -> u64 {
        self.frn & 0x0000_FFFF_FFFF_FFFF
    }

    /// A folder that's in the tree: not a junction or folder symlink, which a listing doesn't go
    /// into (and so never finds closed either).
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !self.is_link()
    }

    /// A symlink or junction (a name surrogate): a listing doesn't count what it points to.
    pub fn is_link(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && self.reparse_tag & 0x2000_0000 != 0
    }
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// Reads one `FSCTL_QUERY_FILE_LAYOUT` answer (QUERY_FILE_LAYOUT_OUTPUT, then a chain of
/// FILE_LAYOUT_ENTRY with their names, streams and extra info), each entry into a `Record` for
/// `each`. Returns how many entries it had; `None` if it's malformed.
pub fn parse_layout(buf: &[u8], mut each: impl FnMut(&Record)) -> Option<u32> {
    let count = u32_at(buf, 0)?;
    let mut at = u32_at(buf, 4)? as usize;
    let mut rec = Record::default();
    let mut names: Vec<(u64, bool)> = Vec::new();
    for _ in 0..count {
        let e = buf.get(at..)?;
        rec.attributes = u32_at(e, 12)?;
        rec.frn = u64_at(e, 16)?;
        rec.size = 0;
        rec.security = 0;
        rec.reparse_tag = 0;
        rec.parents.clear();
        names.clear();
        // FILE_LAYOUT_NAME_ENTRY: next (from this one), flags, parent, length, name.
        let mut n = u32_at(e, 24)? as usize;
        while n != 0 {
            let ne = e.get(n..)?;
            // Flags: 1 the primary name, 2 a DOS name. A short alias (ALONGF~1.TXT) is DOS only
            // and comes with its long name; a name that's both (an 8.3 name, stored once) is 3.
            const DOS_ALIAS: u32 = 2;
            names.push((u64_at(ne, 8)?, u32_at(ne, 4)? == DOS_ALIAS));
            match u32_at(ne, 0)? as usize {
                0 => break,
                next => n += next,
            }
        }
        for (k, &(parent, alias)) in names.iter().enumerate() {
            if !alias || !names.iter().enumerate().any(|(j, &(p, _))| j != k && p == parent) {
                rec.parents.push(parent);
            }
        }
        // STREAM_LAYOUT_ENTRY: next (from this one) at 4, end of file at 24, its information
        // at 32 (from this one), attribute type at 36, identifier length and identifier at 44.
        let mut s = u32_at(e, 28)? as usize;
        while s != 0 {
            let se = e.get(s..)?;
            let kind = u32_at(se, 36)?;
            let id_len = u32_at(se, 44)? as usize;
            if kind == DATA {
                // The main stream is the unnamed one ("::$DATA"; a named one is ":name:$DATA").
                let id = se.get(48..48 + id_len)?;
                let unnamed = id_len == 0 || id.starts_with(&[b':', 0, b':', 0]);
                if unnamed {
                    rec.size = u64_at(se, 24)?;
                }
            } else if kind == REPARSE_POINT {
                // STREAM_INFORMATION_ENTRY: the reparse data's offset (from it) at 16; the
                // data starts with the tag.
                let info = u32_at(se, 32)? as usize;
                if info != 0 {
                    let ie = se.get(info..)?;
                    let data = u32_at(ie, 16)? as usize;
                    if let Some(tag) = u32_at(ie, data) {
                        rec.reparse_tag = tag;
                    }
                }
            }
            match u32_at(se, 4)? as usize {
                0 => break,
                next => s += next,
            }
        }
        // FILE_LAYOUT_INFO_ENTRY: basic information (40 bytes), owner id, security id.
        let extra = u32_at(e, 32)? as usize;
        if extra != 0 {
            rec.security = u32_at(e, extra + 44)?;
        }
        each(&rec);
        match u32_at(e, 4)? as usize {
            0 => break,
            next => at += next,
        }
    }
    Some(count)
}

/// What a `FSCTL_QUERY_FILE_LAYOUT` answer says, entry by entry, for a person to read (the
/// benchmark's diagnostics): each name's folder and flags, each stream's type, identifier and
/// size, the reparse tag and the security id. File names only with `names`.
pub fn describe_layout(buf: &[u8], names: bool) -> Vec<String> {
    let mut out = Vec::new();
    let utf16 = |b: &[u8]| String::from_utf16_lossy(&b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>());
    let (Some(count), Some(first)) = (u32_at(buf, 0), u32_at(buf, 4)) else { return out };
    let mut at = first as usize;
    for _ in 0..count {
        let Some(e) = buf.get(at..) else { break };
        let (Some(attrs), Some(frn)) = (u32_at(e, 12), u64_at(e, 16)) else { break };
        let mut line = format!("record {} (seq {}) attributes {attrs:#x}:", frn & 0x0000_FFFF_FFFF_FFFF, frn >> 48);
        let mut n = u32_at(e, 24).unwrap_or(0) as usize;
        while n != 0 {
            let Some(ne) = e.get(n..) else { break };
            let (flags, parent, len) = (u32_at(ne, 4).unwrap_or(0), u64_at(ne, 8).unwrap_or(0), u32_at(ne, 16).unwrap_or(0) as usize);
            let name = ne.get(24..24 + len).map(utf16).unwrap_or_default();
            let shown = if names { name } else { name.rsplit_once('.').map(|(_, x)| format!("*.{x}")).unwrap_or("*".into()) };
            line += &format!(" name[{shown:?} in {}/{} flags {flags}]", parent & 0x0000_FFFF_FFFF_FFFF, parent >> 48);
            match u32_at(ne, 0).unwrap_or(0) as usize {
                0 => break,
                next => n += next,
            }
        }
        let mut s = u32_at(e, 28).unwrap_or(0) as usize;
        while s != 0 {
            let Some(se) = e.get(s..) else { break };
            let (flags, eof, info, kind, id_len) =
                (u32_at(se, 8).unwrap_or(0), u64_at(se, 24).unwrap_or(0), u32_at(se, 32).unwrap_or(0), u32_at(se, 36).unwrap_or(0), u32_at(se, 44).unwrap_or(0) as usize);
            let id = se.get(48..48 + id_len).map(utf16).unwrap_or_default();
            line += &format!(" stream[{kind:#x} {id:?} {eof} bytes flags {flags:#x}");
            if let Some(ie) = se.get(info as usize..).filter(|_| info != 0) {
                line += &format!(" info {:02x?}", &ie[..ie.len().min(32)]);
            }
            line += "]";
            match u32_at(se, 4).unwrap_or(0) as usize {
                0 => break,
                next => s += next,
            }
        }
        let extra = u32_at(e, 32).unwrap_or(0) as usize;
        if extra != 0 {
            line += &format!(" security {}", u32_at(e, extra + 44).unwrap_or(0));
        }
        out.push(line);
        match u32_at(e, 4).unwrap_or(0) as usize {
            0 => break,
            next => at += next,
        }
    }
    out
}

/// A folder's total: everything inside it, at any depth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dir {
    pub bytes: u64,
    pub files: u64,
    pub security: u32,
}

const USED: u8 = 1;
const DIR: u8 = 2;
/// A file that counts: not a folder, not a symlink.
const COUNTED: u8 = 4;

/// Every folder of a drive with its total, by record number.
#[derive(Default)]
pub struct Tree {
    /// Per record number: its sequence number, what it is, its size, and the folder of its
    /// first name (the others' are in `more`).
    seq: Vec<u16>,
    flags: Vec<u8>,
    size: Vec<u64>,
    parent: Vec<u32>,
    more: FastMap<u32, Vec<u32>>,
    /// The folders that reach the root (the root too).
    dirs: FastMap<u32, Dir>,
    /// Bumped on every change.
    pub generation: u64,
}

impl Tree {
    /// Builds the tree from every record of a drive (any order).
    pub fn build(records: impl IntoIterator<Item = Record>) -> Tree {
        let mut b = Builder::default();
        for r in records {
            b.add(&r);
        }
        b.finish()
    }

    pub fn dir(&self, recno: u32) -> Option<&Dir> {
        self.dirs.get(&recno)
    }

    pub fn dirs(&self) -> impl Iterator<Item = (u32, &Dir)> {
        self.dirs.iter().map(|(&r, d)| (r, d))
    }

    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    /// The folder a folder is in (none for the root).
    pub fn up(&self, recno: u32) -> Option<u32> {
        (recno != ROOT).then(|| self.parent.get(recno as usize).copied()).flatten().filter(|&p| p != NONE)
    }

    /// The full file reference of a record (with its sequence number).
    pub fn frn(&self, recno: u32) -> u64 {
        (u64::from(self.seq.get(recno as usize).copied().unwrap_or(0)) << 48) | u64::from(recno)
    }

    /// What the tree holds about a record (diagnostics).
    pub fn debug(&self, recno: u32) -> String {
        let i = recno as usize;
        if i >= self.seq.len() {
            return "not in the table".into();
        }
        format!(
            "seq {} flags {} size {} parent {} more {:?} folder {:?}",
            self.seq[i],
            self.flags[i],
            self.size[i],
            self.parent[i],
            self.more.get(&recno),
            self.dirs.get(&recno)
        )
    }

    /// The records with a name in this folder (diagnostics: it looks at every record).
    pub fn in_folder(&self, dir: u32) -> Vec<u32> {
        (0..self.parent.len() as u32)
            .filter(|&r| self.parent[r as usize] == dir || self.more.get(&r).is_some_and(|m| m.contains(&dir)))
            .collect()
    }

    /// Is `inner` inside `outer` (at any depth)?
    pub fn inside(&self, inner: u32, outer: u32) -> bool {
        let mut cur = inner;
        for _ in 0..MAX_DEPTH {
            match self.up(cur) {
                Some(p) if p == outer => return true,
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    fn grow(&mut self, recno: u32) {
        let n = recno as usize + 1;
        if self.seq.len() < n {
            self.seq.resize(n, 0);
            self.flags.resize(n, 0);
            self.size.resize(n, 0);
            self.parent.resize(n, NONE);
        }
    }

    /// The folders this record counts in: its names' folders.
    fn parents_of(&self, recno: u32) -> Vec<u32> {
        let mut out = Vec::new();
        if let Some(&p) = self.parent.get(recno as usize).filter(|&&p| p != NONE) {
            out.push(p);
        }
        if let Some(m) = self.more.get(&recno) {
            out.extend(m);
        }
        out
    }

    /// Adds to a folder and every folder above it (subtracts, with negative numbers).
    fn add_up(&mut self, from: u32, bytes: i64, files: i64) {
        let mut cur = from;
        for _ in 0..MAX_DEPTH {
            let Some(d) = self.dirs.get_mut(&cur) else { return };
            d.bytes = d.bytes.saturating_add_signed(bytes);
            d.files = d.files.saturating_add_signed(files);
            match self.up(cur) {
                Some(p) => cur = p,
                None => return,
            }
        }
    }

    /// Is `frn` (a name's folder) a folder in the tree, the same one the name means?
    fn folder_ok(&self, frn: u64) -> Option<u32> {
        let r = u32::try_from(frn & 0x0000_FFFF_FFFF_FFFF).ok()?;
        let seq = (frn >> 48) as u16;
        (self.dirs.contains_key(&r) && (seq == 0 || self.seq.get(r as usize) == Some(&seq))).then_some(r)
    }

    /// One record, as the file table has it now (`None`: it's gone): its old share of every
    /// folder above it comes out, its new one goes in. For a folder, its share is its total, so
    /// moving a folder moves everything in it.
    pub fn update(&mut self, recno: u32, now: Option<&Record>) {
        if recno == ROOT || recno < FIRST_USER_RECORD {
            return;
        }
        self.grow(recno);
        let i = recno as usize;
        // Out with the old.
        if let Some(d) = self.dirs.get(&recno).copied() {
            if let Some(p) = self.up(recno) {
                self.add_up(p, -(d.bytes as i64), -(d.files as i64));
            }
        } else if self.flags[i] & COUNTED != 0 {
            let size = self.size[i] as i64;
            for p in self.parents_of(recno) {
                self.add_up(p, -size, -1);
            }
        }
        // The new state.
        self.more.remove(&recno);
        match now {
            None => {
                self.flags[i] = 0;
                self.size[i] = 0;
                self.parent[i] = NONE;
                // Anything still in a folder that's gone was taken out with it.
                self.dirs.remove(&recno);
            }
            Some(r) => {
                self.seq[i] = (r.frn >> 48) as u16;
                let mut parents: Vec<u32> = r.parents.iter().filter_map(|&p| self.folder_ok(p)).filter(|&p| p != recno).collect();
                self.parent[i] = if parents.is_empty() { NONE } else { parents.remove(0) };
                if !parents.is_empty() && !r.is_dir() {
                    self.more.insert(recno, parents);
                }
                self.size[i] = r.size;
                if r.is_dir() {
                    self.flags[i] = USED | DIR;
                    if self.parent[i] == NONE {
                        self.dirs.remove(&recno);
                    } else {
                        self.dirs.entry(recno).or_default().security = r.security;
                    }
                } else {
                    self.dirs.remove(&recno);
                    let file = !r.is_link() && r.attributes & FILE_ATTRIBUTE_DIRECTORY == 0;
                    self.flags[i] = USED | if file { COUNTED } else { 0 };
                }
            }
        }
        // In with the new.
        if let Some(d) = self.dirs.get(&recno).copied() {
            if let Some(p) = self.up(recno) {
                self.add_up(p, d.bytes as i64, d.files as i64);
            }
        } else if self.flags[i] & COUNTED != 0 {
            let size = self.size[i] as i64;
            for p in self.parents_of(recno) {
                self.add_up(p, size, 1);
            }
        }
        self.generation += 1;
    }

    /// Several records, as they are now, in an order that keeps the tree whole: folders that
    /// are there (each after the one it's in), then files, then folders that are gone (after
    /// what was in them).
    pub fn update_all(&mut self, changed: Vec<(u32, Option<Record>)>) {
        let (mut folders, mut rest): (Vec<_>, Vec<_>) = changed.into_iter().partition(|(_, r)| r.as_ref().is_some_and(|r| r.is_dir()));
        let (mut gone_folders, files): (Vec<_>, Vec<_>) = rest.drain(..).partition(|(n, r)| r.is_none() && self.dirs.contains_key(n));
        // A new folder inside another new one comes after it.
        while !folders.is_empty() {
            let before = folders.len();
            let mut later = Vec::new();
            for (n, r) in folders {
                let r = r.expect("a folder that's there");
                if r.parents.iter().any(|&p| self.folder_ok(p).is_some()) {
                    self.update(n, Some(&r));
                } else {
                    later.push((n, Some(r)));
                }
            }
            if later.len() == before {
                // Nowhere to go (its folder isn't in the tree): it's out of it.
                for (n, r) in later {
                    self.update(n, r.as_ref());
                }
                break;
            }
            folders = later;
        }
        for (n, r) in files {
            self.update(n, r.as_ref());
        }
        // Deepest first.
        gone_folders.sort_by_key(|(n, _)| std::cmp::Reverse(self.depth(*n)));
        for (n, _) in gone_folders {
            self.update(n, None);
        }
    }

    fn depth(&self, recno: u32) -> usize {
        let mut d = 0;
        let mut cur = recno;
        while let Some(p) = self.up(cur) {
            d += 1;
            cur = p;
            if d > MAX_DEPTH {
                break;
            }
        }
        d
    }

    /// Memory it takes (about).
    pub fn heap_bytes(&self) -> usize {
        self.seq.capacity() * 2 + self.flags.capacity() + self.size.capacity() * 8 + self.parent.capacity() * 4 + self.dirs.capacity() * 40
            + self.more.capacity() * 40
    }
}

/// Collects records for a `Tree` (they come in record order, and a name's folder can come later).
#[derive(Default)]
pub struct Builder {
    tree: Tree,
    /// Per record: its names' folders (file references), and its security id.
    first: Vec<u64>,
    more: FastMap<u32, Vec<u64>>,
    security: Vec<u32>,
    records: u64,
}

impl Builder {
    pub fn add(&mut self, r: &Record) {
        let Ok(recno) = u32::try_from(r.recno()) else { return };
        if recno == NONE {
            return;
        }
        self.records += 1;
        self.tree.grow(recno);
        if self.first.len() < self.tree.seq.len() {
            self.first.resize(self.tree.seq.len(), 0);
            self.security.resize(self.tree.seq.len(), 0);
        }
        let i = recno as usize;
        let t = &mut self.tree;
        t.seq[i] = (r.frn >> 48) as u16;
        t.size[i] = r.size;
        // A junction or symlink is neither a folder nor a file here.
        t.flags[i] = USED
            | if r.is_dir() {
                DIR
            } else if r.is_link() || r.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                0
            } else {
                COUNTED
            };
        self.security[i] = r.security;
        let mut ps = r.parents.iter();
        self.first[i] = ps.next().copied().unwrap_or(0);
        let rest: Vec<u64> = ps.copied().collect();
        if !rest.is_empty() {
            self.more.insert(recno, rest);
        }
    }

    /// Records seen so far.
    pub fn records(&self) -> u64 {
        self.records
    }

    pub fn finish(mut self) -> Tree {
        let n = self.tree.seq.len();
        let t = &mut self.tree;
        // A name's folder: a folder that's in use, with the sequence number the name expects
        // (else the folder was deleted and its record reused).
        let folder = |t: &Tree, frn: u64| -> u32 {
            let Ok(p) = u32::try_from(frn & 0x0000_FFFF_FFFF_FFFF) else { return NONE };
            let seq = (frn >> 48) as u16;
            let ok = (p as usize) < n && t.flags[p as usize] & (USED | DIR) == USED | DIR && (seq == 0 || t.seq[p as usize] == seq);
            if ok { p } else { NONE }
        };
        for i in 0..n {
            if t.flags[i] & USED == 0 {
                continue;
            }
            let mut ps: Vec<u32> = std::iter::once(self.first[i])
                .chain(self.more.get(&(i as u32)).into_iter().flatten().copied())
                .filter(|&f| f != 0)
                .map(|f| folder(t, f))
                .filter(|&p| p != NONE && p as usize != i)
                .collect();
            t.parent[i] = if ps.is_empty() { NONE } else { ps.remove(0) };
            if !ps.is_empty() && t.flags[i] & DIR == 0 {
                t.more.insert(i as u32, ps);
            }
        }
        // The folders that reach the root (NTFS's own aside), and how deep: 0 unknown, 1 the
        // root, d + 1 for d levels down; u32::MAX for one that doesn't.
        let mut depth = vec![0u32; n];
        if (ROOT as usize) < n && t.flags[ROOT as usize] & DIR != 0 {
            depth[ROOT as usize] = 1;
        }
        let mut path = Vec::new();
        for i in 0..n {
            if t.flags[i] & DIR == 0 || depth[i] != 0 {
                continue;
            }
            path.clear();
            let mut cur = i as u32;
            let known = loop {
                if depth[cur as usize] != 0 {
                    break depth[cur as usize];
                }
                if cur < FIRST_USER_RECORD || path.len() > MAX_DEPTH || path.contains(&cur) {
                    break u32::MAX;
                }
                path.push(cur);
                match t.parent[cur as usize] {
                    NONE => break u32::MAX,
                    p => cur = p,
                }
            };
            for (k, &d) in path.iter().rev().enumerate() {
                depth[d as usize] = if known == u32::MAX { u32::MAX } else { known + 1 + k as u32 };
            }
        }
        let mut order: Vec<u32> = (0..n as u32).filter(|&i| depth[i as usize] != 0 && depth[i as usize] != u32::MAX).collect();
        for &d in &order {
            t.dirs.insert(d, Dir { bytes: 0, files: 0, security: self.security[d as usize] });
        }
        // Each file in its folders (NTFS's own aside), then each folder in the one above it,
        // deepest first.
        for i in FIRST_USER_RECORD as usize..n {
            if t.flags[i] & COUNTED == 0 {
                continue;
            }
            let size = t.size[i];
            for p in t.parents_of(i as u32) {
                if let Some(d) = t.dirs.get_mut(&p) {
                    d.bytes += size;
                    d.files += 1;
                }
            }
        }
        order.sort_by_key(|&d| std::cmp::Reverse(depth[d as usize]));
        for d in order {
            if d == ROOT {
                continue;
            }
            let (Some(own), Some(&p)) = (t.dirs.get(&d).copied(), t.parent.get(d as usize)) else { continue };
            if let Some(up) = t.dirs.get_mut(&p) {
                up.bytes += own.bytes;
                up.files += own.files;
            }
        }
        // Folders out of the tree keep no parent (`up` would walk out of it).
        for i in 0..n {
            if t.flags[i] & DIR != 0 && !t.dirs.contains_key(&(i as u32)) {
                t.parent[i] = NONE;
            }
        }
        t.generation = 1;
        self.tree
    }
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::search::mft::{map_err, MftError, VolHandle};
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::ERROR_HANDLE_EOF;
    use windows::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_QUERY_FILE_LAYOUT};

    // QUERY_FILE_LAYOUT_INPUT flags: names, streams (resident ones too), the security id, and the
    // reparse tag.
    const RESTART: u32 = 0x1;
    const WHAT: u32 = 0x2 | 0x4 | 0x10 | 0x20 | 0x80 | 0x4000;
    const FILTER_FILEID: u32 = 2;

    /// How a scan went.
    #[derive(Debug, Default, Clone)]
    pub struct ScanStats {
        pub records: u64,
        pub calls: u64,
        pub output_bytes: u64,
        pub took: Duration,
    }

    fn bytes_mut(v: &mut [u64]) -> &mut [u8] {
        // SAFETY: u64s viewed as their bytes (8-byte aligned, as the ioctls want).
        unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, v.len() * 8) }
    }

    /// QUERY_FILE_LAYOUT_INPUT: no filter (the whole table), or these record ranges.
    fn query_input(flags: u32, ranges: &[(u64, u64)]) -> Vec<u8> {
        let mut b = vec![0u8; 16 + 16 * ranges.len().max(1)];
        b[0..4].copy_from_slice(&(ranges.len() as u32).to_le_bytes());
        b[4..8].copy_from_slice(&flags.to_le_bytes());
        b[8..12].copy_from_slice(&(if ranges.is_empty() { 0 } else { FILTER_FILEID }).to_le_bytes());
        for (i, &(start, end)) in ranges.iter().enumerate() {
            b[16 + i * 16..24 + i * 16].copy_from_slice(&start.to_le_bytes());
            b[24 + i * 16..32 + i * 16].copy_from_slice(&end.to_le_bytes());
        }
        b
    }

    /// Asks the file table (`ranges`: these records, or all), answer after answer.
    fn query(vol: &VolHandle, ranges: &[(u64, u64)], stop: &AtomicBool, mut each: impl FnMut(&Record), stats: &mut ScanStats) -> Result<(), MftError> {
        let mut out = vec![0u64; 512 * 1024]; // 4 MiB
        let mut flags = WHAT | RESTART;
        loop {
            if stop.load(Relaxed) {
                return Err(MftError::Io("stopped".into()));
            }
            let input = query_input(flags, ranges);
            let buf = bytes_mut(&mut out);
            let n = match vol.ioctl(FSCTL_QUERY_FILE_LAYOUT, Some(&input), buf) {
                Ok(n) => n,
                Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => return Ok(()),
                Err(e) => return Err(map_err(e, "FSCTL_QUERY_FILE_LAYOUT")),
            };
            flags = WHAT;
            stats.calls += 1;
            stats.output_bytes += n as u64;
            let got = parse_layout(&buf[..n], &mut each).ok_or_else(|| MftError::Io("FSCTL_QUERY_FILE_LAYOUT gave a malformed answer".into()))?;
            if got == 0 {
                return Ok(());
            }
        }
    }

    /// Reads the whole file table into a `Tree`.
    pub fn scan(vol: &VolHandle, stop: &AtomicBool) -> Result<(Tree, ScanStats), MftError> {
        let t0 = Instant::now();
        let mut stats = ScanStats::default();
        let mut b = Builder::default();
        query(vol, &[], stop, |r| b.add(r), &mut stats)?;
        stats.records = b.records();
        let tree = b.finish();
        stats.took = t0.elapsed();
        Ok((tree, stats))
    }

    /// These records as the file table has them now (`None`: not in use).
    pub fn look_up(vol: &VolHandle, recnos: &[u32]) -> Result<Vec<(u32, Option<Record>)>, MftError> {
        let mut found: FastMap<u32, Record> = FastMap::default();
        let never = AtomicBool::new(false);
        let mut stats = ScanStats::default();
        for chunk in recnos.chunks(256) {
            let ranges: Vec<(u64, u64)> = chunk.iter().map(|&r| (u64::from(r), u64::from(r))).collect();
            query(
                vol,
                &ranges,
                &never,
                |r| {
                    if let Ok(k) = u32::try_from(r.recno()) {
                        found.insert(k, r.clone());
                    }
                },
                &mut stats,
            )?;
        }
        Ok(recnos.iter().map(|&r| (r, found.remove(&r))).collect())
    }

    /// What the file table says about these records, for a person to read (`describe_layout`).
    pub fn describe(vol: &VolHandle, recnos: &[u32], names: bool) -> Result<Vec<String>, MftError> {
        let mut out = vec![0u64; 128 * 1024];
        let mut lines = Vec::new();
        for chunk in recnos.chunks(64) {
            let ranges: Vec<(u64, u64)> = chunk.iter().map(|&r| (u64::from(r), u64::from(r))).collect();
            let mut flags = WHAT | RESTART;
            loop {
                let input = query_input(flags, &ranges);
                let buf = bytes_mut(&mut out);
                let n = match vol.ioctl(FSCTL_QUERY_FILE_LAYOUT, Some(&input), buf) {
                    Ok(n) => n,
                    Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => break,
                    Err(e) => return Err(map_err(e, "FSCTL_QUERY_FILE_LAYOUT")),
                };
                flags = WHAT;
                let got = describe_layout(&buf[..n], names);
                if got.is_empty() {
                    break;
                }
                lines.extend(got);
            }
        }
        Ok(lines)
    }

    /// The file table read straight from the disk, as WizTree reads it, without making anything
    /// of it: what that alone costs, to compare with `scan`. (bytes read, records in use, time)
    pub fn raw_read(vol: &VolHandle) -> Result<(u64, u64, Duration), MftError> {
        let t0 = Instant::now();
        let mut vd = [0u8; 128];
        let n = vol.ioctl(FSCTL_GET_NTFS_VOLUME_DATA, None, &mut vd).map_err(|e| map_err(e, "FSCTL_GET_NTFS_VOLUME_DATA"))?;
        if n < 72 {
            return Err(MftError::Io("short NTFS volume data".into()));
        }
        let g32 = |o: usize| u64::from(u32::from_le_bytes(vd[o..o + 4].try_into().unwrap()));
        let g64 = |o: usize| u64::from_le_bytes(vd[o..o + 8].try_into().unwrap());
        let (sector, cluster, record, valid, mft_lcn) = (g32(40), g32(44), g32(48) as usize, g64(56), g64(64));
        let io = |e: windows::core::Error| map_err(e, "reading the file table");
        // Record 0 is the table's own: its data runs say where the table is.
        let mut first = vec![0u8; (record as u64).max(sector) as usize];
        vol.read_at(mft_lcn * cluster, &mut first).map_err(io)?;
        let runs = data_runs(&mut first[..record]).ok_or_else(|| MftError::Io("can't read the file table's own record".into()))?;
        let mut buf = vec![0u64; 2 * 1024 * 1024]; // 16 MiB
        let buf = bytes_mut(&mut buf);
        let (mut read, mut used, mut left) = (0u64, 0u64, valid);
        for (lcn, clusters) in runs {
            let (mut at, mut len) = (lcn * cluster, (clusters * cluster).min(left));
            left -= len;
            while len > 0 {
                let want = len.min(buf.len() as u64) as usize;
                let got = vol.read_at(at, &mut buf[..want]).map_err(io)?;
                if got == 0 {
                    break;
                }
                for r in buf[..got].chunks_exact(record) {
                    if &r[0..4] == b"FILE" && r[22] & 1 != 0 {
                        used += 1;
                    }
                }
                read += got as u64;
                at += got as u64;
                len -= got as u64;
            }
            if left == 0 {
                break;
            }
        }
        Ok((read, used, t0.elapsed()))
    }

    /// The data runs (first cluster, clusters) of a file record's unnamed non-resident $DATA,
    /// after its fixups.
    fn data_runs(rec: &mut [u8]) -> Option<Vec<(u64, u64)>> {
        let u16_at = |b: &[u8], o: usize| Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize);
        if rec.get(0..4)? != b"FILE" {
            return None;
        }
        // The last two bytes of each 512-byte sector are in the update sequence array.
        let (usa, count) = (u16_at(rec, 4)?, u16_at(rec, 6)?);
        for i in 1..count {
            let end = i * 512;
            let fix = rec.get(usa + i * 2..usa + i * 2 + 2)?.to_vec();
            rec.get_mut(end - 2..end)?.copy_from_slice(&fix);
        }
        let mut at = u16_at(rec, 20)?;
        loop {
            let kind = u32_at(rec, at)?;
            let len = u32_at(rec, at + 4)? as usize;
            if kind == 0xFFFF_FFFF || len == 0 {
                return None;
            }
            if kind == DATA && *rec.get(at + 8)? == 1 && *rec.get(at + 9)? == 0 {
                let mut p = at + u16_at(rec, at + 32)?;
                let (mut runs, mut lcn) = (Vec::new(), 0i64);
                loop {
                    let h = *rec.get(p)?;
                    if h == 0 {
                        return Some(runs);
                    }
                    let (ls, os) = ((h & 0xF) as usize, (h >> 4) as usize);
                    let num = |b: &[u8], signed: bool| {
                        let mut v = 0i64;
                        for (k, &x) in b.iter().enumerate() {
                            v |= i64::from(x) << (8 * k);
                        }
                        if signed && !b.is_empty() && b[b.len() - 1] & 0x80 != 0 && b.len() < 8 {
                            v -= 1 << (8 * b.len());
                        }
                        v
                    };
                    let length = num(rec.get(p + 1..p + 1 + ls)?, false) as u64;
                    lcn += num(rec.get(p + 1 + ls..p + 1 + ls + os)?, true);
                    if os > 0 {
                        runs.push((lcn as u64, length));
                    }
                    p += 1 + ls + os;
                }
            }
            at += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: u32 = FILE_ATTRIBUTE_DIRECTORY;

    fn frn(recno: u32, seq: u16) -> u64 {
        (u64::from(seq) << 48) | u64::from(recno)
    }

    fn rec(recno: u32, attributes: u32, parents: &[u32], size: u64) -> Record {
        Record { frn: frn(recno, 1), attributes, parents: parents.iter().map(|&p| frn(p, 1)).collect(), size, security: 0, reparse_tag: 0 }
    }

    /// A drive: the root (5), NTFS's own files under it, $Extend (11) with a journal inside,
    /// and some folders and files.
    fn drive() -> Vec<Record> {
        vec![
            rec(0, 0, &[5], 1 << 30), // $MFT
            rec(5, D, &[5], 0),
            rec(11, D, &[5], 0),            // $Extend
            rec(40, 0, &[11], 50 << 30),    // $UsnJrnl, inside $Extend
            rec(30, D, &[5], 0),            // a
            rec(31, D, &[30], 0),           // a\b
            rec(32, 0, &[30], 100),         // a\f
            rec(33, 0, &[31], 200),         // a\b\g
            rec(34, 0, &[31, 30], 1000),    // a\b\h, and a hard link in a
            rec(35, 0, &[5], 7),            // top
            Record { reparse_tag: 0xA000_000C, ..rec(36, FILE_ATTRIBUTE_REPARSE_POINT, &[30], 0) }, // a symlink in a
            Record { reparse_tag: 0x9000_001A, ..rec(37, FILE_ATTRIBUTE_REPARSE_POINT, &[31], 4096) }, // a OneDrive placeholder
            rec(38, D, &[99], 0),           // a folder whose folder isn't there
            rec(39, 0, &[38], 5),           // inside it
            // A junction (like Public\Documents\My Music): neither a folder nor a file.
            Record { reparse_tag: 0xA000_0003, ..rec(41, D | FILE_ATTRIBUTE_REPARSE_POINT, &[30], 0) },
        ]
    }

    fn total(t: &Tree, d: u32) -> (u64, u64) {
        t.dir(d).map_or((0, 0), |d| (d.bytes, d.files))
    }

    #[test]
    fn folders_add_up_what_a_listing_shows() {
        let t = Tree::build(drive());
        assert_eq!(total(&t, 31), (200 + 1000 + 4096, 3), "a\\b: g, h, the placeholder");
        assert_eq!(total(&t, 30), (100 + 200 + 1000 + 4096 + 1000, 5), "a: f, b's, and h again (its second name)");
        assert_eq!(total(&t, ROOT), (100 + 200 + 1000 + 4096 + 1000 + 7, 6), "nothing of NTFS's, no symlink, no orphan");
        assert!(t.dir(11).is_none() && t.dir(38).is_none() && t.dir(41).is_none(), "NTFS's own, an orphan, a junction");
        assert!(t.inside(33, ROOT) && t.inside(31, 30) && !t.inside(30, 31));
    }

    #[test]
    fn a_name_whose_folder_was_reused_isnt_in_it() {
        let mut d = drive();
        // a\f names folder 30 with sequence 2, but 30 is at 1 (deleted and reused since).
        d[6].parents = vec![frn(30, 2)];
        let t = Tree::build(d);
        assert_eq!(total(&t, 30).1, 4);
    }

    /// After changes, the tree is what building it from the records as they are now gives.
    #[test]
    fn changes_keep_it_as_a_fresh_build_would_be() {
        let mut now: std::collections::BTreeMap<u32, Record> = drive().into_iter().map(|r| (r.recno() as u32, r)).collect();
        let mut t = Tree::build(now.values().cloned());
        let check = |t: &Tree, now: &std::collections::BTreeMap<u32, Record>, what: &str| {
            let fresh = Tree::build(now.values().cloned());
            for (d, dir) in fresh.dirs() {
                assert_eq!(t.dir(d), Some(dir), "{what}: folder {d}");
            }
            assert_eq!(t.dir_count(), fresh.dir_count(), "{what}: folders");
        };
        let change = |t: &mut Tree, now: &mut std::collections::BTreeMap<u32, Record>, r: Record| {
            let n = r.recno() as u32;
            now.insert(n, r.clone());
            t.update_all(vec![(n, Some(r))]);
        };
        change(&mut t, &mut now, rec(33, 0, &[31], 5000));
        check(&t, &now, "a file grew");
        change(&mut t, &mut now, rec(32, 0, &[5], 100));
        check(&t, &now, "a file moved up");
        change(&mut t, &mut now, rec(31, D, &[5], 0));
        check(&t, &now, "a folder moved, with what's in it");
        change(&mut t, &mut now, rec(34, 0, &[31], 1000));
        check(&t, &now, "a hard link removed");
        // A new folder with a new folder and a file in it, in one batch (children first).
        let batch = vec![rec(52, 0, &[51], 9), rec(51, D, &[50], 0), rec(50, D, &[30], 0)];
        for r in &batch {
            now.insert(r.recno() as u32, r.clone());
        }
        t.update_all(batch.into_iter().map(|r| (r.recno() as u32, Some(r))).collect());
        check(&t, &now, "new folders");
        // Deleted together: the file and the folders it was in.
        for n in [52, 51, 50] {
            now.remove(&n);
        }
        t.update_all(vec![(50, None), (52, None), (51, None)]);
        check(&t, &now, "deleted");
        change(&mut t, &mut now, Record { reparse_tag: 0xA000_000C, ..rec(35, FILE_ATTRIBUTE_REPARSE_POINT, &[5], 7) });
        check(&t, &now, "a file became a symlink");
        // f, b's g, h and the placeholder; top is a symlink now.
        assert_eq!(total(&t, ROOT).1, 4);
    }

    /// Builds a QUERY_FILE_LAYOUT answer as Windows lays it out.
    fn layout(entries: &[(u64, u32, &[(u64, u32)], &[(u32, &str, u64)], Option<u32>, u32)]) -> Vec<u8> {
        let put32 = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put64 = |b: &mut Vec<u8>, o: usize, v: u64| b[o..o + 8].copy_from_slice(&v.to_le_bytes());
        let mut b = vec![0u8; 16];
        put32(&mut b, 0, entries.len() as u32);
        put32(&mut b, 4, 16);
        let mut last_entry = None;
        for &(frn, attrs, names, streams, tag, security) in entries {
            let e = b.len();
            if let Some(prev) = last_entry {
                put32(&mut b, prev + 4, (e - prev) as u32);
            }
            last_entry = Some(e);
            b.resize(e + 40, 0);
            put32(&mut b, e + 12, attrs);
            put64(&mut b, e + 16, frn);
            let mut last = None;
            for &(parent, flags) in names {
                let n = b.len();
                put32(&mut b, if last.is_none() { e + 24 } else { last.unwrap() }, (n - last.unwrap_or(e)) as u32);
                b.resize(n + 24 + 8, 0);
                put32(&mut b, n + 4, flags);
                put64(&mut b, n + 8, parent);
                put32(&mut b, n + 16, 8);
                last = Some(n);
            }
            let mut last = None;
            for &(kind, id, size) in streams {
                let s = b.len();
                let link = |l: Option<usize>| l.map_or(e + 28, |l| l + 4);
                put32(&mut b, link(last), (s - last.unwrap_or(e)) as u32);
                let id: Vec<u8> = id.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
                b.resize(s + 48 + id.len(), 0);
                put64(&mut b, s + 24, size);
                put32(&mut b, s + 36, kind);
                put32(&mut b, s + 44, id.len() as u32);
                b[s + 48..s + 48 + id.len()].copy_from_slice(&id);
                if let (REPARSE_POINT, Some(tag)) = (kind, tag) {
                    let info = b.len();
                    put32(&mut b, s + 32, (info - s) as u32);
                    b.resize(info + 24 + 8, 0);
                    put32(&mut b, info + 16, 24);
                    put32(&mut b, info + 24, tag);
                }
                last = Some(s);
            }
            let x = b.len();
            put32(&mut b, e + 32, (x - e) as u32);
            b.resize(x + 64, 0);
            put32(&mut b, x + 44, security);
        }
        b
    }

    #[test]
    fn reads_the_file_layout_windows_gives() {
        const DOS: u32 = 2;
        let buf = layout(&[
            // A file with a long name and its short name in folder 30, and a hard link in 31;
            // its main stream, and a named one (not counted).
            (frn(40, 3), 0x20, &[(frn(30, 1), 1), (frn(30, 1), DOS), (frn(31, 1), 0)], &[(DATA, "::$DATA", 1234), (DATA, ":Zone.Identifier:$DATA", 26)], None, 7),
            // A folder whose only name is an 8.3 one (stored once, as a DOS name).
            (frn(41, 1), D, &[(frn(5, 5), DOS)], &[(0xA0, "$I30", 4096)], None, 9),
            // A symlink.
            (frn(42, 1), FILE_ATTRIBUTE_REPARSE_POINT, &[(frn(30, 1), 1)], &[(DATA, "", 0), (REPARSE_POINT, "", 40)], Some(0xA000_000C), 7),
            // An 8.3 name (both primary and DOS: 3) with two hard links in the same folder (0),
            // as Windows reported x\many\file: three names there.
            (frn(43, 1), 0x20, &[(frn(30, 1), 0), (frn(30, 1), 3), (frn(30, 1), 0)], &[(DATA, "", 777)], None, 7),
        ]);
        let mut got = Vec::new();
        assert_eq!(parse_layout(&buf, |r| got.push(r.clone())), Some(4));
        assert_eq!(got[3].parents, vec![frn(30, 1); 3]);
        assert_eq!(got[0].parents, vec![frn(30, 1), frn(31, 1)]);
        assert_eq!((got[0].size, got[0].security, got[0].is_dir()), (1234, 7, false));
        assert_eq!(got[1].parents, vec![frn(5, 5)]);
        assert!(got[1].is_dir() && got[1].size == 0 && got[1].security == 9);
        assert!(got[2].is_link() && !got[0].is_link());
        assert_eq!(parse_layout(&buf[..40], |_| {}), None, "cut short");
    }
}
