//! Struct-of-arrays name index for one volume (or one walk root).
//!
//! Layout
//! ------
//! All names live in one contiguous *scan buffer* (`low`, lower-cased) made of
//! records `[0x00][6 base-31 digits in 0x01..=0x1F][name bytes]`, followed by a
//! final `0x00` sentinel. Because file names never contain bytes < 0x20 (we
//! sanitize them) and query tokens never do either, a `memmem` hit can never
//! straddle two records or touch a header; the owning entry is found by
//! `memrchr(0)` back to the header and decoding the 6 digits. `orig` has the
//! exact same layout with the original-case bytes (our lower-casing is length
//! preserving per char), so one offset addresses both.
//!
//! Per entry: record offset, parent, flags, first-child / next-sibling links
//! (lazy: deleted children stay linked until compaction), and optionally size
//! + mtime (walk provider). MFT volumes also keep a dense
//! `MFT record number → entry` map for USN updates.
//!
//! Renames append a new record and blank the old one (bytes → 0x01, which no
//! query can match); deletions blank the record and set `DEAD`. `compact()`
//! rebuilds everything once garbage grows.

use memchr::memchr;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, Read, Write};

pub const NONE: u32 = u32::MAX;
/// Record header size: separator + 6 index digits.
pub const HDR: usize = 7;
/// Largest index encodable in 6 base-31 digits.
pub const MAX_ENTRIES: usize = 887_503_680;

pub mod flags {
    pub const DIR: u8 = 1;
    pub const HIDDEN: u8 = 2;
    pub const SYSTEM: u8 = 4;
    pub const DEAD: u8 = 8;
    pub const ROOT: u8 = 16;
    /// Reparse point (symlink / junction) — not descended by the walker.
    pub const LINK: u8 = 32;
}
use flags::*;

/// Byte used to blank out removed names (never part of a query token).
const BLANK: u8 = 0x01;

// ---------------------------------------------------------------- hashing

/// Tiny Fx-style hasher for integer keys (hot paths only).
#[derive(Default, Clone, Copy)]
pub struct FxHasher(u64);
impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64)
    }
}
pub type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub type FastSet<K> = std::collections::HashSet<K, BuildHasherDefault<FxHasher>>;

// ---------------------------------------------------------------- names

/// Length-preserving lower-casing: ASCII always, other chars only when their
/// lowercase form is a single char of the same UTF-8 length. Control chars
/// become `?` so they can never collide with record headers.
pub fn lower_into(s: &str, out: &mut Vec<u8>) {
    if s.is_ascii() {
        out.extend(s.bytes().map(|b| if b < 0x20 { b'?' } else { b.to_ascii_lowercase() }));
        return;
    }
    let mut tmp = [0u8; 4];
    for c in s.chars() {
        if c.is_ascii() {
            let b = c as u8;
            out.push(if b < 0x20 { b'?' } else { b.to_ascii_lowercase() });
            continue;
        }
        let mut it = c.to_lowercase();
        let l = match (it.next(), it.next()) {
            (Some(l), None) if l.len_utf8() == c.len_utf8() => l,
            _ => c,
        };
        out.extend_from_slice(l.encode_utf8(&mut tmp).as_bytes());
    }
}

pub fn lower(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len());
    lower_into(s, &mut v);
    v
}

/// The name without accents, for searches typed without them: Latin letters with a diacritic
/// become their base letter (é → e, ł → l, ș → s), ligatures their letters (æ → ae, ß → ss).
/// Everything else is kept, lower-cased. Not length-preserving: only for comparing.
pub fn fold_into(s: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let Ok(s) = std::str::from_utf8(s) else {
        out.extend_from_slice(s);
        return;
    };
    let mut tmp = [0u8; 4];
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase() as u8);
            continue;
        }
        let c = c.to_lowercase().next().unwrap_or(c);
        match fold_char(c) {
            Some(base) => out.extend_from_slice(base.as_bytes()),
            None => out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes()),
        }
    }
}

fn fold_char(c: char) -> Option<&'static str> {
    Some(match c {
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ƒ' => "f",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' | 'ĸ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => "n",
        'ò'..='ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' | 'ſ' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Original bytes with control chars replaced (same length as `lower`).
fn sanitize_into(s: &str, out: &mut Vec<u8>) {
    out.extend(s.bytes().map(|b| if b < 0x20 { b'?' } else { b }));
}

#[inline]
pub(crate) fn decode_idx(d: &[u8]) -> u32 {
    let mut v = 0u32;
    let mut i = 6;
    while i > 0 {
        i -= 1;
        v = v * 31 + (d[i] - 1) as u32;
    }
    v
}

#[inline]
fn encode_idx(mut v: u32) -> [u8; 6] {
    let mut d = [0u8; 6];
    for x in d.iter_mut() {
        *x = (v % 31) as u8 + 1;
        v /= 31;
    }
    d
}

// ---------------------------------------------------------------- index

pub struct Index {
    /// Display path of entry 0, e.g. `C:\` or `C:\Users\me`.
    pub root_path: String,
    pub(crate) low: Vec<u8>,
    pub(crate) orig: Vec<u8>,
    pub(crate) rec: Vec<u32>,
    pub(crate) parent: Vec<u32>,
    pub(crate) flags: Vec<u8>,
    pub(crate) first_child: Vec<u32>,
    pub(crate) next_sib: Vec<u32>,
    /// Present when `has_meta` (walk provider); empty otherwise.
    pub(crate) size: Vec<u64>,
    /// Unix seconds, 0 = unknown. Present when `has_meta`.
    pub(crate) mtime: Vec<u32>,
    pub has_meta: bool,
    /// MFT volumes: record number (FRN & 0xFFFF_FFFF_FFFF) → entry.
    pub(crate) frn_map: Vec<u32>,
    pub(crate) byte_freq: Box<[u64; 256]>,
    live: usize,
    dead: usize,
    garbage: usize,
    /// Bumped on every mutation batch; used to invalidate query caches.
    pub generation: u64,
}

impl Index {
    pub fn new(root_path: &str, has_meta: bool) -> Self {
        let mut ix = Index {
            root_path: root_path.to_string(),
            low: vec![0],
            orig: vec![0],
            rec: Vec::new(),
            parent: Vec::new(),
            flags: Vec::new(),
            first_child: Vec::new(),
            next_sib: Vec::new(),
            size: Vec::new(),
            mtime: Vec::new(),
            has_meta,
            frn_map: Vec::new(),
            byte_freq: Box::new([0; 256]),
            live: 0,
            dead: 0,
            garbage: 0,
            generation: 0,
        };
        ix.push_unlinked(NONE, "", DIR | ROOT, 0, 0);
        ix
    }

    pub fn with_capacity(root_path: &str, has_meta: bool, entries: usize, bytes: usize) -> Self {
        let mut ix = Self::new(root_path, has_meta);
        ix.reserve(entries, bytes);
        ix
    }

    pub fn reserve(&mut self, entries: usize, bytes: usize) {
        self.low.reserve(bytes);
        self.orig.reserve(bytes);
        self.rec.reserve(entries);
        self.parent.reserve(entries);
        self.flags.reserve(entries);
        self.first_child.reserve(entries);
        self.next_sib.reserve(entries);
        if self.has_meta {
            self.size.reserve(entries);
            self.mtime.reserve(entries);
        }
    }

    pub fn shrink_to_fit(&mut self) {
        self.low.shrink_to_fit();
        self.orig.shrink_to_fit();
        self.rec.shrink_to_fit();
        self.parent.shrink_to_fit();
        self.flags.shrink_to_fit();
        self.first_child.shrink_to_fit();
        self.next_sib.shrink_to_fit();
        self.size.shrink_to_fit();
        self.mtime.shrink_to_fit();
        self.frn_map.shrink_to_fit();
    }

    /// Number of entry slots (including dead ones and the root).
    #[inline]
    pub fn slots(&self) -> usize {
        self.rec.len()
    }

    /// Live entries, excluding the root.
    #[inline]
    pub fn live(&self) -> usize {
        self.live.saturating_sub(1)
    }

    pub fn scan_bytes(&self) -> usize {
        self.low.len()
    }

    /// Approximate heap usage.
    pub fn heap_bytes(&self) -> usize {
        self.low.capacity()
            + self.orig.capacity()
            + 4 * (self.rec.capacity() + self.parent.capacity() + self.first_child.capacity() + self.next_sib.capacity())
            + self.flags.capacity()
            + 8 * self.size.capacity()
            + 4 * self.mtime.capacity()
            + 4 * self.frn_map.capacity()
    }

    fn append_record(&mut self, idx: u32, name: &str) -> u32 {
        self.generation += 1;
        // The trailing sentinel 0 becomes this record's separator.
        let h = self.low.len() - 1;
        let d = encode_idx(idx);
        self.low.extend_from_slice(&d);
        self.orig.extend_from_slice(&d);
        let start = self.low.len();
        sanitize_into(name, &mut self.orig);
        lower_into(name, &mut self.low);
        debug_assert_eq!(self.low.len(), self.orig.len());
        for &b in &self.low[start..] {
            self.byte_freq[b as usize] += 1;
        }
        self.low.push(0);
        self.orig.push(0);
        h as u32
    }

    /// Appends an entry without linking it into its parent's child list
    /// (bulk builds call `relink()` afterwards).
    pub fn push_unlinked(&mut self, parent: u32, name: &str, fl: u8, size: u64, mtime: u32) -> u32 {
        let idx = self.rec.len() as u32;
        assert!((idx as usize) < MAX_ENTRIES, "index full");
        let h = self.append_record(idx, name);
        self.rec.push(h);
        self.parent.push(parent);
        self.flags.push(fl & !DEAD);
        self.first_child.push(NONE);
        self.next_sib.push(NONE);
        if self.has_meta {
            self.size.push(size);
            self.mtime.push(mtime);
        }
        self.live += 1;
        idx
    }

    /// Appends an entry and links it under `parent`.
    pub fn insert(&mut self, parent: u32, name: &str, fl: u8, size: u64, mtime: u32) -> u32 {
        let idx = self.push_unlinked(parent, name, fl, size, mtime);
        self.link(parent, idx);
        idx
    }

    fn link(&mut self, parent: u32, idx: u32) {
        if parent == NONE {
            return;
        }
        self.next_sib[idx as usize] = self.first_child[parent as usize];
        self.first_child[parent as usize] = idx;
    }

    fn unlink(&mut self, parent: u32, idx: u32) {
        if parent == NONE {
            return;
        }
        let mut cur = self.first_child[parent as usize];
        if cur == idx {
            self.first_child[parent as usize] = self.next_sib[idx as usize];
            return;
        }
        while cur != NONE {
            let nx = self.next_sib[cur as usize];
            if nx == idx {
                self.next_sib[cur as usize] = self.next_sib[idx as usize];
                return;
            }
            cur = nx;
        }
    }

    /// Rebuilds child lists from `parent` and marks entries that cannot reach
    /// the root (orphans, cycles) as dead.
    pub fn relink(&mut self) {
        let n = self.slots();
        self.first_child.iter_mut().for_each(|x| *x = NONE);
        self.next_sib.iter_mut().for_each(|x| *x = NONE);
        // Link in reverse so child lists come out in insertion order.
        for i in (1..n).rev() {
            if self.flags[i] & DEAD != 0 {
                continue;
            }
            let p = self.parent[i];
            if p == NONE || p as usize >= n || p as usize == i {
                continue;
            }
            self.next_sib[i] = self.first_child[p as usize];
            self.first_child[p as usize] = i as u32;
        }
        // Reachability from the root.
        let mut seen = vec![false; n];
        let mut stack = vec![0u32];
        seen[0] = true;
        while let Some(d) = stack.pop() {
            let mut c = self.first_child[d as usize];
            while c != NONE {
                if !seen[c as usize] {
                    seen[c as usize] = true;
                    stack.push(c);
                }
                c = self.next_sib[c as usize];
            }
        }
        for (i, &reached) in seen.iter().enumerate() {
            if !reached && self.flags[i] & DEAD == 0 {
                self.kill(i as u32);
            }
        }
        self.generation += 1;
    }

    #[inline]
    pub(crate) fn bounds(&self, idx: u32) -> (usize, usize) {
        let ns = self.rec[idx as usize] as usize + HDR;
        let ne = ns + memchr(0, &self.low[ns..]).unwrap_or(0);
        (ns, ne)
    }

    #[inline]
    pub fn name_low(&self, idx: u32) -> &[u8] {
        let (a, b) = self.bounds(idx);
        &self.low[a..b]
    }

    pub fn name(&self, idx: u32) -> &str {
        let (a, b) = self.bounds(idx);
        std::str::from_utf8(&self.orig[a..b]).unwrap_or("?")
    }

    #[inline]
    pub fn flags_of(&self, idx: u32) -> u8 {
        self.flags[idx as usize]
    }

    #[inline]
    pub fn is_dir(&self, idx: u32) -> bool {
        self.flags[idx as usize] & DIR != 0
    }

    #[inline]
    pub fn is_live(&self, idx: u32) -> bool {
        (idx as usize) < self.slots() && self.flags[idx as usize] & DEAD == 0
    }

    #[inline]
    pub fn parent_of(&self, idx: u32) -> u32 {
        self.parent[idx as usize]
    }

    pub fn meta(&self, idx: u32) -> (Option<u64>, Option<u32>) {
        if !self.has_meta {
            return (None, None);
        }
        let i = idx as usize;
        let m = self.mtime[i];
        (Some(self.size[i]), (m != 0).then_some(m))
    }

    pub fn set_meta(&mut self, idx: u32, size: u64, mtime: u32) {
        self.generation += 1;
        if self.has_meta {
            self.size[idx as usize] = size;
            self.mtime[idx as usize] = mtime;
        }
    }

    pub fn set_flags(&mut self, idx: u32, fl: u8) {
        self.generation += 1;
        let f = &mut self.flags[idx as usize];
        *f = (*f & (DEAD | ROOT)) | (fl & !(DEAD | ROOT));
    }

    pub fn children(&self, parent: u32) -> Children<'_> {
        Children { ix: self, cur: self.first_child[parent as usize] }
    }

    /// Live child of `parent` whose lower-cased name equals `lname`.
    pub fn child(&self, parent: u32, lname: &[u8]) -> Option<u32> {
        let mut c = self.first_child[parent as usize];
        while c != NONE {
            if self.flags[c as usize] & DEAD == 0 && self.name_low(c) == lname {
                return Some(c);
            }
            c = self.next_sib[c as usize];
        }
        None
    }

    /// Pairs of live entries sharing (parent, lower-cased name) — must always
    /// be empty. O(n); for tests and diagnostics.
    pub fn duplicate_children(&self) -> Vec<(u32, u32)> {
        let mut seen: std::collections::HashMap<(u32, &[u8]), u32> = std::collections::HashMap::new();
        let mut dups = Vec::new();
        for i in 1..self.slots() as u32 {
            if self.flags[i as usize] & DEAD != 0 {
                continue;
            }
            if let Some(&first) = seen.get(&(self.parent[i as usize], self.name_low(i))) {
                dups.push((first, i));
            } else {
                seen.insert((self.parent[i as usize], self.name_low(i)), i);
            }
        }
        dups
    }

    /// Does `dir` have two live children with the same lower-cased name?
    pub fn dir_has_duplicates(&self, dir: u32) -> bool {
        let mut names: Vec<&[u8]> = self.children(dir).map(|c| self.name_low(c)).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        names.len() != n
    }

    fn kill(&mut self, idx: u32) {
        let i = idx as usize;
        if self.flags[i] & DEAD != 0 {
            return;
        }
        self.generation += 1;
        let (a, b) = self.bounds(idx);
        self.low[a..b].fill(BLANK);
        self.garbage += HDR + (b - a);
        self.flags[i] |= DEAD;
        self.live -= 1;
        self.dead += 1;
    }

    /// Removes an entry and its whole subtree. The root cannot be removed.
    pub fn remove(&mut self, idx: u32) {
        if idx == 0 || !self.is_live(idx) {
            return;
        }
        let p = self.parent[idx as usize];
        self.unlink(p, idx);
        let mut stack = vec![idx];
        while let Some(e) = stack.pop() {
            let mut c = self.first_child[e as usize];
            while c != NONE {
                if self.flags[c as usize] & DEAD == 0 {
                    stack.push(c);
                }
                c = self.next_sib[c as usize];
            }
            self.first_child[e as usize] = NONE;
            self.kill(e);
        }
    }

    /// Renames and/or moves an entry (its subtree follows automatically).
    pub fn rename(&mut self, idx: u32, new_parent: u32, new_name: &str) {
        if idx == 0 || !self.is_live(idx) || new_parent != NONE && self.is_ancestor(idx, new_parent) {
            return;
        }
        let i = idx as usize;
        if self.name(idx) != new_name {
            let (a, b) = self.bounds(idx);
            self.low[a..b].fill(BLANK);
            self.garbage += HDR + (b - a);
            self.rec[i] = self.append_record(idx, new_name);
        }
        self.generation += 1;
        let old = self.parent[i];
        if old != new_parent && new_parent != NONE {
            self.unlink(old, idx);
            self.parent[i] = new_parent;
            self.link(new_parent, idx);
        }
    }

    /// True if `a` is `b` or one of its ancestors.
    pub fn is_ancestor(&self, a: u32, mut b: u32) -> bool {
        let mut guard = 0;
        while b != NONE && guard < 4096 {
            if a == b {
                return true;
            }
            b = self.parent[b as usize];
            guard += 1;
        }
        false
    }

    pub fn depth(&self, mut idx: u32) -> u32 {
        let mut d = 0;
        while idx != 0 && idx != NONE && d < 4096 {
            idx = self.parent[idx as usize];
            d += 1;
        }
        d
    }

    /// Full display path of an entry.
    pub fn path(&self, idx: u32) -> String {
        let mut chain = Vec::with_capacity(16);
        let mut cur = idx;
        while cur != 0 && cur != NONE && chain.len() < 4096 {
            chain.push(cur);
            cur = self.parent[cur as usize];
        }
        let mut s = String::with_capacity(self.root_path.len() + chain.len() * 16);
        s.push_str(&self.root_path);
        for &c in chain.iter().rev() {
            if !s.ends_with('\\') {
                s.push('\\');
            }
            s.push_str(self.name(c));
        }
        s
    }

    /// Lower-cased root path without a trailing backslash.
    pub fn root_key(&self) -> Vec<u8> {
        let mut r = lower(&self.root_path);
        while r.last() == Some(&b'\\') {
            r.pop();
        }
        r
    }

    /// Resolves a display path (any case) to an entry.
    pub fn resolve(&self, path: &str) -> Option<u32> {
        let p = lower(path.trim_end_matches('\\'));
        let root = self.root_key();
        if !p.starts_with(&root) {
            return None;
        }
        let rest = &p[root.len()..];
        if !rest.is_empty() && rest[0] != b'\\' {
            return None;
        }
        let mut cur = 0u32;
        for comp in rest.split(|&b| b == b'\\').filter(|c| !c.is_empty()) {
            cur = self.child(cur, comp)?;
        }
        Some(cur)
    }

    /// Marks the index as changed (invalidates incremental query caches).
    pub fn bump(&mut self) {
        self.generation += 1;
    }

    // ------------------------------------------------------------ MFT map

    pub fn frn_get(&self, recno: u64) -> Option<u32> {
        let v = *self.frn_map.get(recno as usize)?;
        (v != NONE && self.is_live(v)).then_some(v)
    }

    pub fn frn_set(&mut self, recno: u64, idx: u32) {
        let r = recno as usize;
        if r >= self.frn_map.len() {
            self.frn_map.resize(r + 1 + r / 8, NONE);
        }
        self.frn_map[r] = idx;
    }

    pub fn frn_clear(&mut self, recno: u64) {
        if let Some(v) = self.frn_map.get_mut(recno as usize) {
            *v = NONE;
        }
    }

    // ------------------------------------------------------------ compaction

    pub fn garbage_ratio(&self) -> f64 {
        self.garbage as f64 / self.low.len().max(1) as f64
    }

    pub fn needs_compaction(&self) -> bool {
        self.dead > 50_000 && self.dead * 5 > self.live || self.garbage > (4 << 20) && self.garbage_ratio() > 0.25
    }

    /// Copy with dead entries and garbage removed (entries renumbered).
    pub fn compacted(&self) -> Index {
        let n = self.slots();
        let mut remap = vec![NONE; n];
        let mut order: Vec<u32> = Vec::with_capacity(self.live);
        for i in 0..n {
            if self.flags[i] & DEAD == 0 {
                remap[i] = order.len() as u32;
                order.push(i as u32);
            }
        }
        let bytes = self.low.len() - self.garbage;
        let mut out = Index {
            root_path: self.root_path.clone(),
            low: Vec::with_capacity(bytes + 1),
            orig: Vec::with_capacity(bytes + 1),
            rec: Vec::with_capacity(order.len()),
            parent: Vec::with_capacity(order.len()),
            flags: Vec::with_capacity(order.len()),
            first_child: vec![NONE; order.len()],
            next_sib: vec![NONE; order.len()],
            size: Vec::with_capacity(if self.has_meta { order.len() } else { 0 }),
            mtime: Vec::with_capacity(if self.has_meta { order.len() } else { 0 }),
            has_meta: self.has_meta,
            frn_map: self.frn_map.iter().map(|&v| if v == NONE { NONE } else { remap[v as usize] }).collect(),
            byte_freq: Box::new([0; 256]),
            live: order.len(),
            dead: 0,
            garbage: 0,
            generation: self.generation + 1,
        };
        out.low.push(0);
        out.orig.push(0);
        for (ni, &old) in order.iter().enumerate() {
            let (a, b) = self.bounds(old);
            let h = out.low.len() - 1;
            let d = encode_idx(ni as u32);
            out.low.extend_from_slice(&d);
            out.orig.extend_from_slice(&d);
            out.low.extend_from_slice(&self.low[a..b]);
            out.orig.extend_from_slice(&self.orig[a..b]);
            for &c in &self.low[a..b] {
                out.byte_freq[c as usize] += 1;
            }
            out.low.push(0);
            out.orig.push(0);
            out.rec.push(h as u32);
            let p = self.parent[old as usize];
            out.parent.push(if p == NONE { NONE } else { remap[p as usize] });
            out.flags.push(self.flags[old as usize]);
            if self.has_meta {
                out.size.push(self.size[old as usize]);
                out.mtime.push(self.mtime[old as usize]);
            }
        }
        out.relink();
        out
    }

    pub fn compact(&mut self) {
        let generation = self.generation;
        *self = self.compacted();
        self.generation = generation + 2;
    }

    // ------------------------------------------------------------ snapshot

    const MAGIC: &'static [u8; 8] = b"FRTIDX02";

    /// Writes a compacted copy of the index.
    pub fn write_snapshot(&self, w: &mut impl Write) -> io::Result<()> {
        let c;
        let ix = if self.dead == 0 && self.garbage == 0 {
            self
        } else {
            c = self.compacted();
            &c
        };
        w.write_all(Self::MAGIC)?;
        write_bytes(w, ix.root_path.as_bytes())?;
        w.write_all(&[ix.has_meta as u8])?;
        w.write_all(&(ix.slots() as u32).to_le_bytes())?;
        write_bytes(w, &ix.orig)?;
        write_u32s(w, &ix.parent)?;
        w.write_all(&ix.flags)?;
        if ix.has_meta {
            let mut buf = Vec::with_capacity(ix.size.len() * 8);
            for s in &ix.size {
                buf.extend_from_slice(&s.to_le_bytes());
            }
            w.write_all(&buf)?;
            write_u32s(w, &ix.mtime)?;
        }
        w.write_all(&(ix.frn_map.len() as u32).to_le_bytes())?;
        write_u32s(w, &ix.frn_map)?;
        Ok(())
    }

    pub fn read_snapshot(r: &mut impl Read) -> io::Result<Index> {
        let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != Self::MAGIC {
            return Err(bad("bad snapshot magic"));
        }
        let root = String::from_utf8(read_bytes(r)?).map_err(|_| bad("bad root"))?;
        let mut b1 = [0u8; 1];
        r.read_exact(&mut b1)?;
        let has_meta = b1[0] != 0;
        let n = read_u32(r)? as usize;
        let orig = read_bytes(r)?;
        let parent = read_u32s(r, n)?;
        let mut fl = vec![0u8; n];
        r.read_exact(&mut fl)?;
        let (size, mtime) = if has_meta {
            let mut raw = vec![0u8; n * 8];
            r.read_exact(&mut raw)?;
            let size = raw.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
            (size, read_u32s(r, n)?)
        } else {
            (Vec::new(), Vec::new())
        };
        let fn_len = read_u32(r)? as usize;
        let frn_map = read_u32s(r, fn_len)?;
        if orig.first() != Some(&0) || orig.last() != Some(&0) || n == 0 {
            return Err(bad("bad name buffer"));
        }
        // Rebuild rec offsets + the lower-cased scan buffer.
        let mut low = Vec::with_capacity(orig.len());
        let mut rec = vec![NONE; n];
        let mut freq = Box::new([0u64; 256]);
        let mut pos = 0usize;
        let mut count = 0usize;
        while pos + 1 < orig.len() {
            if orig[pos] != 0 || pos + HDR > orig.len() {
                return Err(bad("bad record"));
            }
            let idx = decode_idx(&orig[pos + 1..pos + HDR]) as usize;
            if idx >= n {
                return Err(bad("bad record index"));
            }
            rec[idx] = pos as u32;
            low.extend_from_slice(&orig[pos..pos + HDR]);
            let ns = pos + HDR;
            let ne = ns + memchr(0, &orig[ns..]).ok_or_else(|| bad("unterminated"))?;
            let start = low.len();
            match std::str::from_utf8(&orig[ns..ne]) {
                Ok(s) => lower_into(s, &mut low),
                Err(_) => low.extend(orig[ns..ne].iter().map(|b| b.to_ascii_lowercase())),
            }
            if low.len() - start != ne - ns {
                return Err(bad("case map length"));
            }
            for &c in &low[start..] {
                freq[c as usize] += 1;
            }
            pos = ne;
            count += 1;
        }
        low.push(0);
        if count != n || rec.iter().any(|&r| r == NONE) || parent.len() != n {
            return Err(bad("record count mismatch"));
        }
        let mut ix = Index {
            root_path: root,
            low,
            orig,
            rec,
            parent,
            flags: fl,
            first_child: vec![NONE; n],
            next_sib: vec![NONE; n],
            size,
            mtime,
            has_meta,
            frn_map,
            byte_freq: freq,
            live: n,
            dead: 0,
            garbage: 0,
            generation: 1,
        };
        ix.relink();
        Ok(ix)
    }
}

pub struct Children<'a> {
    ix: &'a Index,
    cur: u32,
}

impl Iterator for Children<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        while self.cur != NONE {
            let c = self.cur;
            self.cur = self.ix.next_sib[c as usize];
            if self.ix.flags[c as usize] & DEAD == 0 {
                return Some(c);
            }
        }
        None
    }
}

fn write_bytes(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    w.write_all(&(b.len() as u64).to_le_bytes())?;
    w.write_all(b)
}

fn write_u32s(w: &mut impl Write, v: &[u32]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(v.len().min(1 << 20) * 4);
    for chunk in v.chunks(1 << 20) {
        buf.clear();
        for x in chunk {
            buf.extend_from_slice(&x.to_le_bytes());
        }
        w.write_all(&buf)?;
    }
    Ok(())
}

pub(crate) fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_bytes(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    let n = u64::from_le_bytes(b) as usize;
    if n > (8usize << 30) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot blob too large"));
    }
    let mut v = vec![0u8; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn read_u32s(r: &mut impl Read, n: usize) -> io::Result<Vec<u32>> {
    let mut raw = vec![0u8; n * 4];
    r.read_exact(&mut raw)?;
    Ok(raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Index, u32, u32, u32) {
        let mut ix = Index::new(r"C:\", true);
        let users = ix.insert(0, "Users", DIR, 0, 0);
        let me = ix.insert(users, "Me", DIR, 0, 0);
        let f = ix.insert(me, "Report.PDF", 0, 1234, 1_700_000_000);
        (ix, users, me, f)
    }

    #[test]
    fn idx_codec_roundtrip() {
        for v in [0u32, 1, 30, 31, 961, 1_000_000, 3_000_000, MAX_ENTRIES as u32 - 1] {
            let d = encode_idx(v);
            assert!(d.iter().all(|&b| (1..=0x1f).contains(&b)));
            assert_eq!(decode_idx(&d), v);
        }
    }

    #[test]
    fn lowercase_is_length_preserving() {
        for s in ["Straße", "ÀÉÎÕÜ", "İstanbul", "ΣΑΣ", "Ǆ", "file\u{1}x", "日本語.TXT"] {
            assert_eq!(lower(s).len(), s.len(), "{s}");
        }
        assert_eq!(lower("ÀB.TXT"), "àb.txt".as_bytes());
    }

    #[test]
    fn path_reconstruction_and_resolve() {
        let (ix, users, me, f) = sample();
        assert_eq!(ix.path(0), r"C:\");
        assert_eq!(ix.path(users), r"C:\Users");
        assert_eq!(ix.path(f), r"C:\Users\Me\Report.PDF");
        assert_eq!(ix.resolve(r"c:\users\ME\report.pdf"), Some(f));
        assert_eq!(ix.resolve(r"C:\Users\Me\"), Some(me));
        assert_eq!(ix.resolve(r"C:\"), Some(0));
        assert_eq!(ix.resolve(r"C:\Nope"), None);
        assert_eq!(ix.resolve(r"D:\Users"), None);
        assert_eq!(ix.depth(f), 3);
        assert_eq!(ix.name(f), "Report.PDF");
        assert_eq!(ix.name_low(f), b"report.pdf");
        assert_eq!(ix.meta(f), (Some(1234), Some(1_700_000_000)));

        let mut sub = Index::new(r"C:\Users\meltingice", false);
        let a = sub.insert(0, "dev", DIR, 0, 0);
        let b = sub.insert(a, "x.rs", 0, 0, 0);
        assert_eq!(sub.path(b), r"C:\Users\meltingice\dev\x.rs");
        assert_eq!(sub.resolve(r"c:\users\MELTINGICE\dev\X.RS"), Some(b));
        assert_eq!(sub.resolve(r"C:\Users\meltingicex"), None);
    }

    #[test]
    fn insert_remove_rename() {
        let (mut ix, users, me, f) = sample();
        let docs = ix.insert(me, "Docs", DIR, 0, 0);
        let g = ix.insert(docs, "notes.txt", 0, 1, 1);
        assert_eq!(ix.live(), 5);

        // rename in place
        ix.rename(f, me, "Summary.pdf");
        assert_eq!(ix.path(f), r"C:\Users\Me\Summary.pdf");
        assert_eq!(ix.resolve(r"C:\Users\Me\Report.PDF"), None);
        assert_eq!(ix.resolve(r"C:\Users\Me\summary.PDF"), Some(f));
        // old record blanked: the scan buffer no longer contains the old name
        assert!(memchr::memmem::find(&ix.low, b"report").is_none());

        // move a folder: subtree follows
        ix.rename(docs, users, "Documents");
        assert_eq!(ix.path(g), r"C:\Users\Documents\notes.txt");
        assert_eq!(ix.children(me).collect::<Vec<_>>(), vec![f]);
        // a folder cannot be moved into its own subtree
        ix.rename(users, g, "x");
        assert_eq!(ix.parent_of(users), 0);

        // remove subtree
        ix.remove(docs);
        assert!(!ix.is_live(docs) && !ix.is_live(g));
        assert_eq!(ix.resolve(r"C:\Users\Documents"), None);
        assert_eq!(ix.live(), 3);
        assert!(memchr::memmem::find(&ix.low, b"notes").is_none());

        // compaction keeps paths
        let c = ix.compacted();
        assert_eq!(c.live(), 3);
        let nf = c.resolve(r"C:\Users\Me\Summary.pdf").unwrap();
        assert_eq!(c.path(nf), r"C:\Users\Me\Summary.pdf");
        assert_eq!(c.meta(nf).0, Some(1234));
    }

    #[test]
    fn frn_map_and_relink_orphans() {
        let mut ix = Index::new(r"D:\", false);
        let a = ix.push_unlinked(0, "a", DIR, 0, 0);
        let b = ix.push_unlinked(a, "b.txt", 0, 0, 0);
        let orphan = ix.push_unlinked(999_999, "lost", 0, 0, 0);
        ix.parent[orphan as usize] = NONE;
        ix.frn_set(40, a);
        ix.frn_set(41, b);
        ix.relink();
        assert!(ix.is_live(b));
        assert!(!ix.is_live(orphan));
        assert_eq!(ix.frn_get(41), Some(b));
        assert_eq!(ix.path(b), r"D:\a\b.txt");
        ix.remove(a);
        assert_eq!(ix.frn_get(41), None);
    }

    #[test]
    fn snapshot_roundtrip() {
        let (mut ix, _, me, _) = sample();
        let x = ix.insert(me, "Ünïcode Fïlé.TXT", HIDDEN, 7, 9);
        ix.frn_set(77, x);
        ix.remove(ix.resolve(r"C:\Users\Me\Report.PDF").unwrap());
        let mut buf = Vec::new();
        ix.write_snapshot(&mut buf).unwrap();
        let back = Index::read_snapshot(&mut buf.as_slice()).unwrap();
        assert_eq!(back.live(), ix.live());
        let nx = back.resolve(r"C:\Users\Me\ünïcode fïlé.txt").unwrap();
        assert_eq!(back.path(nx), r"C:\Users\Me\Ünïcode Fïlé.TXT");
        assert_eq!(back.flags_of(nx) & HIDDEN, HIDDEN);
        assert_eq!(back.meta(nx), (Some(7), Some(9)));
        assert_eq!(back.frn_get(77), Some(nx));
        assert!(Index::read_snapshot(&mut &buf[..buf.len() / 2]).is_err());
    }
}
