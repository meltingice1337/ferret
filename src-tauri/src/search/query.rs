//! Query parsing and the matching / ranking engine.
//!
//! Strategy (per query):
//! 1. pick the rarest positive token (or a glob literal / `.ext\0` pattern) and
//!    scan every volume's name buffer for it with `memchr::memmem`, split into
//!    record-aligned chunks processed in parallel by rayon;
//! 2. every hit is verified against the remaining tokens + filters, counted,
//!    and scored only if its upper-bound score can still enter the top-K
//!    (per-chunk heaps + a shared atomic threshold);
//! 3. if there are fewer than `limit` substring hits, a fuzzy subsequence pass
//!    (prefiltered by the rarest needle byte, scored by nucleo) tops them up;
//! 4. when a query refines the previous one (same filters, every old token is
//!    contained in a new one) only the cached candidate records are re-checked.

use super::index::{decode_idx, flags::*, lower, FastMap, FastSet, Index, HDR, NONE};
use memchr::memmem::Finder;
use memchr::{memchr, memrchr};
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

// ====================================================================== parse

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Term {
    /// Case-insensitive substring of the name (lower-cased).
    Sub(Vec<u8>),
    /// Whole-name wildcard pattern (`*`, `?`), lower-cased.
    Glob(Vec<u8>),
    /// Token containing `\`: `name` must be in the file name and `dir` (ending
    /// in `\`) must occur in the parent folder's path.
    Path { dir: Vec<u8>, name: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

impl Cmp {
    #[inline]
    pub(crate) fn test<T: PartialOrd>(self, a: T, b: T) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Eq => a == b,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
    /// `!word` — names containing it are dropped.
    pub excludes: Vec<Vec<u8>>,
    /// `!node_modules\`, `!\Backup\`, `!C:\Windows`, `!D:\` — tokens with a `\`
    /// (or `/`) or `:` exclude by full path: (lower-cased pattern, is_prefix).
    /// A leading drive makes it a prefix that respects folder boundaries;
    /// otherwise it is a substring of the full path.
    pub path_excludes: Vec<(Vec<u8>, bool)>,
    /// `ext:pdf,docx` (lower-cased, no dot).
    pub exts: Vec<Vec<u8>>,
    pub kind: Option<Kind>,
    /// `in:` folders (all must contain the result).
    pub scopes: Vec<String>,
    pub size: Vec<(Cmp, u64)>,
    /// Unix seconds.
    pub modified: Vec<(Cmp, i64)>,
}

impl Query {
    /// Nothing that would narrow the result set (an empty box).
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
            && self.exts.is_empty()
            && self.kind.is_none()
            && self.scopes.is_empty()
            && self.size.is_empty()
            && self.modified.is_empty()
    }

    pub fn has_meta_filters(&self) -> bool {
        !self.size.is_empty() || !self.modified.is_empty()
    }

    /// Literals that must occur in the file name: plain tokens and the name
    /// part of `dir\name` tokens (used for highlighting).
    fn sub_terms(&self) -> impl Iterator<Item = &[u8]> {
        self.terms.iter().filter_map(|t| match t {
            Term::Sub(s) => Some(s.as_slice()),
            Term::Path { name, .. } if !name.is_empty() => Some(name.as_slice()),
            _ => None,
        })
    }

    /// Plain tokens (may match the name or a parent folder name).
    fn plain_terms(&self) -> impl Iterator<Item = &[u8]> {
        self.terms.iter().filter_map(|t| if let Term::Sub(s) = t { Some(s.as_slice()) } else { None })
    }

    fn same_filters(&self, o: &Query) -> bool {
        let rest = |q: &Query| q.terms.iter().filter(|t| !matches!(t, Term::Sub(_))).cloned().collect::<Vec<_>>();
        self.excludes == o.excludes
            && self.path_excludes == o.path_excludes
            && self.exts == o.exts
            && self.kind == o.kind
            && self.scopes == o.scopes
            && self.size == o.size
            && self.modified == o.modified
            && rest(self) == rest(o)
    }

    /// True when every match of `self` is guaranteed to match `prev` too.
    /// Tokens may match parent folders as long as one is in the name, so
    /// besides every old token living inside a new one, every new token must
    /// contain an old one (adding an unrelated token is not a refinement).
    pub fn refines(&self, prev: &Query) -> bool {
        let old: Vec<&[u8]> = prev.plain_terms().collect();
        if old.is_empty() || !self.same_filters(prev) {
            return false;
        }
        let new: Vec<&[u8]> = self.plain_terms().collect();
        let within = |a: &[u8], b: &[u8]| memchr::memmem::find(b, a).is_some();
        old.iter().all(|o| new.iter().any(|n| within(o, n))) && new.iter().all(|n| old.iter().any(|o| within(o, n)))
    }
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn parse(input: &str) -> Query {
    parse_at(input, now_secs())
}

/// Splits on whitespace outside double quotes. Returns (text, fully_quoted).
fn tokenize(s: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut inq, mut quoted, mut bare) = (false, false, false);
    let mut flush = |cur: &mut String, quoted: &mut bool, bare: &mut bool| {
        if !cur.is_empty() {
            out.push((std::mem::take(cur), *quoted && !*bare));
        }
        *quoted = false;
        *bare = false;
    };
    for c in s.chars() {
        if c == '"' {
            inq = !inq;
            quoted = true;
        } else if c.is_whitespace() && !inq {
            flush(&mut cur, &mut quoted, &mut bare);
        } else {
            if !inq {
                bare = true;
            }
            cur.push(c);
        }
    }
    flush(&mut cur, &mut quoted, &mut bare);
    out
}

fn clean(b: Vec<u8>) -> Vec<u8> {
    b.into_iter().filter(|&c| c >= 0x20).collect()
}

pub fn parse_at(input: &str, now: i64) -> Query {
    let mut q = Query::default();
    for (tok, phrase) in tokenize(input) {
        if phrase {
            let t = clean(lower(&tok));
            if !t.is_empty() {
                q.terms.push(Term::Sub(t));
            }
            continue;
        }
        if let Some(rest) = tok.strip_prefix('!') {
            let mut t = clean(lower(&rest.replace('/', "\\")));
            if t.is_empty() {
                continue;
            }
            if t.contains(&b'\\') || t.contains(&b':') {
                let prefix = t.len() >= 2 && t[1] == b':' && t[0].is_ascii_alphabetic();
                if prefix && t.len() == 2 {
                    t.push(b'\\'); // `!D:` = the whole drive
                }
                q.path_excludes.push((t, prefix));
            } else {
                q.excludes.push(t);
            }
            continue;
        }
        if let Some((key, val)) = tok.split_once(':') {
            let k = key.to_ascii_lowercase();
            let handled = match k.as_str() {
                "ext" => {
                    for e in val.split([',', ';', '|']) {
                        let e = clean(lower(e.trim().trim_start_matches('.')));
                        if !e.is_empty() && !q.exts.contains(&e) {
                            q.exts.push(e);
                        }
                    }
                    true
                }
                "in" | "scope" => {
                    let v = val.trim();
                    if !v.is_empty() {
                        q.scopes.push(crate::paths::normalize(v));
                    }
                    true
                }
                "type" | "kind" => {
                    match val.to_ascii_lowercase().as_str() {
                        "folder" | "folders" | "dir" | "dirs" | "directory" => q.kind = Some(Kind::Folder),
                        "file" | "files" => q.kind = Some(Kind::File),
                        _ => {}
                    }
                    true
                }
                "size" => {
                    if let Some(f) = parse_size_filter(val) {
                        q.size.extend(f);
                    }
                    true
                }
                "modified" | "mod" | "dm" | "date" | "datemodified" => {
                    if let Some(f) = parse_date_filter(val, now) {
                        q.modified.extend(f);
                    }
                    true
                }
                _ => false,
            };
            if handled {
                continue;
            }
        }
        let t = clean(lower(&tok.replace('/', "\\")));
        if t.is_empty() {
            continue;
        }
        let is_drive = t.len() == 2 && t[1] == b':' && t[0].is_ascii_alphabetic();
        if t.contains(&b'\\') || is_drive {
            let mut t = t;
            if is_drive {
                t.push(b'\\');
            }
            let cut = t.iter().rposition(|&b| b == b'\\').unwrap();
            let name = t[cut + 1..].to_vec();
            let dir = t[..=cut].to_vec();
            q.terms.push(Term::Path { dir, name });
        } else if t.iter().any(|&b| b == b'*' || b == b'?') {
            q.terms.push(Term::Glob(t));
        } else {
            q.terms.push(Term::Sub(t));
        }
    }
    q
}

fn split_cmp(v: &str) -> (Cmp, &str) {
    for (p, c) in [(">=", Cmp::Ge), ("<=", Cmp::Le), (">", Cmp::Gt), ("<", Cmp::Lt), ("=", Cmp::Eq)] {
        if let Some(r) = v.strip_prefix(p) {
            return (c, r.trim());
        }
    }
    (Cmp::Eq, v.trim())
}

pub fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim().to_ascii_lowercase();
    let split = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mul: f64 = match unit.trim() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mul) as u64)
}

fn parse_size_filter(v: &str) -> Option<Vec<(Cmp, u64)>> {
    let v = v.trim();
    if v.eq_ignore_ascii_case("empty") {
        return Some(vec![(Cmp::Eq, 0)]);
    }
    if let Some((a, b)) = v.split_once("..") {
        return Some(vec![(Cmp::Ge, parse_size(a)?), (Cmp::Le, parse_size(b)?)]);
    }
    let (c, rest) = split_cmp(v);
    let n = parse_size(rest)?;
    // A bare `size:10mb` means "at least".
    let c = if v.starts_with('=') || !rest.is_empty() && v.len() != rest.len() { c } else { Cmp::Ge };
    Some(vec![(c, n)])
}

fn parse_duration(v: &str) -> Option<i64> {
    let v = v.trim().to_ascii_lowercase();
    let split = v.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = v.split_at(split);
    let n: i64 = num.parse().ok()?;
    let secs = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        "w" | "wk" | "week" | "weeks" => 7 * 86_400,
        "mo" | "month" | "months" => 30 * 86_400,
        "y" | "yr" | "year" | "years" => 365 * 86_400,
        _ => return None,
    };
    Some(n * secs)
}

fn local_midnight(d: chrono::NaiveDate) -> i64 {
    use chrono::TimeZone;
    let ndt = d.and_hms_opt(0, 0, 0).unwrap_or_default();
    chrono::Local
        .from_local_datetime(&ndt)
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or_else(|| ndt.and_utc().timestamp())
}

fn parse_date(v: &str) -> Option<chrono::NaiveDate> {
    let mut it = v.trim().split(['-', '/', '.']);
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next().map(|s| s.parse().ok()).unwrap_or(Some(1))?;
    let d: u32 = it.next().map(|s| s.parse().ok()).unwrap_or(Some(1))?;
    if it.next().is_some() || y < 1601 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(y, m, d)
}

fn parse_date_filter(v: &str, now: i64) -> Option<Vec<(Cmp, i64)>> {
    use chrono::TimeZone;
    let today = chrono::Local.timestamp_opt(now, 0).single()?.date_naive();
    let start = |d: chrono::NaiveDate| local_midnight(d);
    let next = |d: chrono::NaiveDate| d.succ_opt().map(local_midnight).unwrap_or(i64::MAX);
    let v = v.trim().to_ascii_lowercase();
    match v.as_str() {
        "today" => return Some(vec![(Cmp::Ge, start(today))]),
        "yesterday" => {
            let y = today.pred_opt()?;
            return Some(vec![(Cmp::Ge, start(y)), (Cmp::Lt, start(today))]);
        }
        "week" | "thisweek" => return Some(vec![(Cmp::Ge, now - 7 * 86_400)]),
        "month" | "thismonth" => return Some(vec![(Cmp::Ge, now - 30 * 86_400)]),
        "year" | "thisyear" => return Some(vec![(Cmp::Ge, now - 365 * 86_400)]),
        _ => {}
    }
    if let Some((a, b)) = v.split_once("..") {
        let (a, b) = (parse_date(a)?, parse_date(b)?);
        return Some(vec![(Cmp::Ge, start(a)), (Cmp::Lt, next(b))]);
    }
    let (c, rest) = split_cmp(&v);
    if let Some(age) = parse_duration(rest) {
        // `<7d` = younger than 7 days, `>7d` = older than 7 days.
        let t = now - age;
        return Some(vec![match c {
            Cmp::Lt | Cmp::Eq => (Cmp::Gt, t),
            Cmp::Le => (Cmp::Ge, t),
            Cmp::Gt => (Cmp::Lt, t),
            Cmp::Ge => (Cmp::Le, t),
        }]);
    }
    let d = parse_date(rest)?;
    Some(match c {
        Cmp::Gt => vec![(Cmp::Ge, next(d))],
        Cmp::Ge => vec![(Cmp::Ge, start(d))],
        Cmp::Lt => vec![(Cmp::Lt, start(d))],
        Cmp::Le => vec![(Cmp::Lt, next(d))],
        Cmp::Eq => vec![(Cmp::Ge, start(d)), (Cmp::Lt, next(d))],
    })
}

// ====================================================================== scoring

pub const EXACT: i32 = 20_000;
pub const STEM: i32 = 17_000;
pub const PREFIX: i32 = 14_000;
pub const WORD: i32 = 10_000;
pub const SUBSTR: i32 = 6_000;
const GLOB: i32 = 12_000;
const FILTER_ONLY: i32 = 8_000;
const FUZZY_MAX: i32 = 3_000;
/// Result tiers: every token in the name > some token only in a parent
/// folder > fuzzy (tier 0). Gaps exceed every other adjustment incl. frecency.
const NAME_TIER: i32 = 50_000;
const PATH_TIER: i32 = 20_000;
const DIR_BONUS: i32 = 300;
const HIDDEN_PENALTY: i32 = 400;
const DEPTH_STEP: i32 = 40;
const DEPTH_CAP: u32 = 25;
const COVER_MAX: i32 = 1_500;
const SCORE_OFF: i64 = 1 << 22;
const MASK40: u64 = (1 << 40) - 1;

/// Packs (score, vol, idx) into one totally ordered key: higher score first,
/// then lower (vol, idx) for deterministic ties.
#[inline]
fn pack(score: i32, vol: usize, idx: u32) -> u64 {
    let s = (score as i64 + SCORE_OFF).clamp(0, (1 << 24) - 1) as u64;
    (s << 40) | (!(((vol as u64) << 32) | idx as u64) & MASK40)
}

#[inline]
fn unpack(key: u64) -> (i32, usize, u32) {
    let s = ((key >> 40) as i64 - SCORE_OFF) as i32;
    let lo = !key & MASK40;
    (s, (lo >> 32) as usize, lo as u32)
}

#[inline]
fn is_sep(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'_' | b'-' | b'.' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'+' | b',' | b';' | b'\'' | b'&' | b'@' | b'#' | b'!' | b'~' | b'='
    )
}

#[inline]
fn boundary(name: &[u8], orig: &[u8], p: usize) -> bool {
    if p == 0 {
        return true;
    }
    let (pv, cu) = (name[p - 1], name[p]);
    if is_sep(pv) {
        return true;
    }
    let (po, co) = (orig[p - 1], orig[p]);
    if po.is_ascii_lowercase() && co.is_ascii_uppercase() {
        return true;
    }
    pv.is_ascii_alphanumeric() && cu.is_ascii_alphanumeric() && pv.is_ascii_digit() != cu.is_ascii_digit()
}

/// Best match class of `t` in `name` and the position it was found at.
#[inline]
fn best_class(name: &[u8], orig: &[u8], t: &[u8], f: &Finder) -> Option<(i32, usize)> {
    let tl = t.len();
    if tl > name.len() {
        return None;
    }
    let p = f.find(name)?;
    if p == 0 {
        if tl == name.len() {
            return Some((EXACT, 0));
        }
        if name[tl] == b'.' && memchr(b'.', &name[tl + 1..]).is_none() {
            return Some((STEM, 0));
        }
        return Some((PREFIX, 0));
    }
    if boundary(name, orig, p) {
        return Some((WORD, p));
    }
    let mut from = p + 1;
    while from + tl <= name.len() {
        match f.find(&name[from..]) {
            Some(q) => {
                let q = from + q;
                if boundary(name, orig, q) {
                    return Some((WORD, q));
                }
                from = q + 1;
            }
            None => break,
        }
    }
    Some((SUBSTR, p))
}

pub(crate) fn glob_match(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star_p, mut star_s) = (usize::MAX, 0usize);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'?' {
            pi += 1;
            si += 1;
            while si < s.len() && (s[si] & 0xC0) == 0x80 {
                si += 1;
            }
        } else if pi < p.len() && p[pi] == b'*' {
            star_p = pi;
            pi += 1;
            star_s = si;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if star_p != usize::MAX {
            pi = star_p + 1;
            star_s += 1;
            si = star_s;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// `X\`, `\X\`, `\X` (X without `\` or `:`) are single-folder patterns.
fn folder_pred(p: &[u8], prefix: bool) -> Option<(FolderPred, Vec<u8>)> {
    if prefix {
        // Drive-anchored: resolved to one folder per volume.
        let trail = p.last() == Some(&b'\\');
        let mut t = p.to_vec();
        while t.last() == Some(&b'\\') {
            t.pop();
        }
        return Some((FolderPred::Under(!trail), t));
    }
    let lead = p.first() == Some(&b'\\');
    let trail = p.last() == Some(&b'\\');
    if p.len() < 2 + (lead && trail) as usize {
        return None;
    }
    let inner = &p[lead as usize..p.len() - trail as usize];
    if inner.is_empty() || inner.contains(&b'\\') || inner.contains(&b':') {
        return None;
    }
    let pred = match (lead, trail) {
        (true, true) => FolderPred::Exact,
        (false, true) => FolderPred::Ends,
        (true, false) => FolderPred::Starts,
        (false, false) => return None,
    };
    Some((pred, inner.to_vec()))
}

fn glob_literal(g: &[u8]) -> &[u8] {
    g.split(|&b| b == b'*' || b == b'?').max_by_key(|s| s.len()).unwrap_or(&[])
}

fn is_subsequence(needle: &[u8], hay: &[u8]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|c| it.any(|h| h == c))
}

// ====================================================================== engine

struct TopK {
    k: usize,
    heap: BinaryHeap<Reverse<u64>>,
}

impl TopK {
    fn new(k: usize) -> Self {
        TopK { k, heap: BinaryHeap::with_capacity(k + 1) }
    }
    #[inline]
    fn floor(&self) -> u64 {
        if self.heap.len() < self.k {
            0
        } else {
            self.heap.peek().map(|r| r.0).unwrap_or(0)
        }
    }
    #[inline]
    fn push(&mut self, key: u64) -> bool {
        if self.heap.len() < self.k {
            self.heap.push(Reverse(key));
            self.heap.len() == self.k
        } else if key > self.heap.peek().unwrap().0 {
            self.heap.pop();
            self.heap.push(Reverse(key));
            true
        } else {
            false
        }
    }
}

/// Where a volume's results may come from.
#[derive(Clone)]
enum Scope {
    All,
    Skip,
    /// Must be below every listed folder.
    Under(Vec<u32>),
}

struct VolCtx<'a> {
    vol: usize,
    ix: &'a Index,
    scope: Scope,
    frec: Option<&'a FastMap<u32, i32>>,
    /// Per plain token: folders of this volume whose name contains it, filled
    /// by the token's scan pass. `None` → fall back to path strings.
    folders: Option<&'a [FolderSet]>,
    /// Tokens whose folder sets are complete (bit i = token i).
    known: u64,
    /// Per single-folder path exclude: matching folders (`None` → not
    /// computed, use path strings).
    ex: Option<&'a [FolderSet]>,
    /// The root's own path matches a path exclude: everything is excluded.
    ex_all: bool,
}

/// Bit set over a volume's entry slots (folders matching a name predicate).
#[derive(Default, Clone)]
struct FolderSet {
    bits: Vec<u64>,
    n: usize,
}

impl FolderSet {
    fn add(&mut self, slots: usize, i: u32) {
        if self.bits.is_empty() {
            self.bits = vec![0; slots / 64 + 1];
        }
        let w = &mut self.bits[(i >> 6) as usize];
        if *w >> (i & 63) & 1 == 0 {
            *w |= 1 << (i & 63);
            self.n += 1;
        }
    }

    #[inline]
    fn has(&self, i: u32) -> bool {
        self.n > 0 && self.bits.get((i >> 6) as usize).is_some_and(|w| w >> (i & 63) & 1 != 0)
    }
}

/// Single-folder path excludes become folder-name predicates.
#[derive(Clone, Copy, PartialEq)]
enum FolderPred {
    /// `X\`: some parent folder name ends with X.
    Ends,
    /// `\X\`: some parent folder is named X.
    Exact,
    /// `\X`: some parent folder name (or the name itself) starts with X.
    Starts,
    /// `C:\Windows` / `C:\Windows\`: below one resolved folder (and the
    /// folder itself unless the pattern ends in `\`).
    Under(bool),
}

impl FolderPred {
    fn test(self, name: &[u8], x: &[u8]) -> bool {
        match self {
            FolderPred::Ends => name.ends_with(x),
            FolderPred::Exact => name == x,
            FolderPred::Starts => name.starts_with(x),
            FolderPred::Under(_) => false,
        }
    }
}

/// A record whose tokens need a parent-folder check, deferred until the
/// folder sets are complete.
#[derive(Clone, Copy)]
struct Pending {
    h: u32,
    ne: u32,
    ti: u32,
    rank: u32,
    pos: u32,
}

struct SubT {
    bytes: Vec<u8>,
    finder: Finder<'static>,
}

enum Pre {
    /// Scan for each plain token in turn (same cache-hot chunk); a record is
    /// evaluated only by the scan of the first-ranked token in its name.
    Subs(Vec<usize>),
    /// A literal that must be in the name (glob / `dir\name` part).
    Lit(Finder<'static>),
    Exts(Vec<Finder<'static>>),
    All,
}

struct Shared {
    thr: AtomicU64,
    cands: AtomicUsize,
    overflow: AtomicBool,
    fuzzy_seen: AtomicUsize,
}

/// Lower-cased folder paths (trailing `\`) built once per folder per scan task
/// in one arena; descendants extend their parent's cached path.
#[derive(Default)]
struct PathCache {
    arena: Vec<u8>,
    map: FastMap<u32, (u32, u32)>,
    chain: Vec<u32>,
    /// Length of the root's own path (`c:\`), not part of any folder name.
    root_len: usize,
}

impl PathCache {
    fn get(&mut self, ix: &Index, dir: u32) -> &[u8] {
        let (o, l) = self.range(ix, dir);
        &self.arena[o as usize..(o + l) as usize]
    }

    fn range(&mut self, ix: &Index, dir: u32) -> (u32, u32) {
        if let Some(&r) = self.map.get(&dir) {
            return r;
        }
        self.chain.clear();
        let mut cur = dir;
        let mut base = None;
        while cur != NONE && cur != 0 && self.chain.len() < 4096 {
            if let Some(&r) = self.map.get(&cur) {
                base = Some(r);
                break;
            }
            self.chain.push(cur);
            cur = ix.parent[cur as usize];
        }
        let mut prev = match base.or_else(|| self.map.get(&0).copied()) {
            Some(r) => r,
            None => {
                let start = self.arena.len();
                self.arena.extend_from_slice(&ix.root_key());
                self.arena.push(b'\\');
                self.root_len = self.arena.len() - start;
                let r = (start as u32, self.root_len as u32);
                self.map.insert(0, r);
                r
            }
        };
        for i in (0..self.chain.len()).rev() {
            let d = self.chain[i];
            let start = self.arena.len();
            self.arena.extend_from_within(prev.0 as usize..(prev.0 + prev.1) as usize);
            self.arena.extend_from_slice(ix.name_low(d));
            self.arena.push(b'\\');
            prev = (start as u32, (self.arena.len() - start) as u32);
            self.map.insert(d, prev);
        }
        prev
    }
}

struct St {
    top: TopK,
    total: u64,
    cands: Vec<u32>,
    collecting: bool,
    /// (entry, score before per-entry adjustments) awaiting `fs::metadata`.
    lazy: Vec<(u32, i32)>,
    scope_memo: FastMap<u32, bool>,
    paths: PathCache,
    buf: Vec<u8>,
    /// Multi-token pass: defer checks that need a later token's folders.
    defer: bool,
    /// Only collect folder hits (no new records can match in this volume,
    /// but deferred ones still need this token's folders).
    collect_only: bool,
    pending: Vec<Pending>,
    /// Per plain token: folder hits of its scan pass.
    dir_hits: Vec<Vec<u32>>,
}

impl St {
    fn new(k: usize, collecting: bool) -> Self {
        St {
            top: TopK::new(k),
            total: 0,
            cands: Vec::new(),
            collecting,
            lazy: Vec::new(),
            scope_memo: FastMap::default(),
            paths: PathCache::default(),
            buf: Vec::new(),
            defer: false,
            collect_only: false,
            pending: Vec::new(),
            dir_hits: Vec::new(),
        }
    }
}

/// Max candidates kept for incremental refinement.
pub const CACHE_CAP: usize = 400_000;
/// Max candidates resolved with `fs::metadata` for size/date filters on
/// volumes without stored metadata (MFT).
pub const LAZY_CAP: usize = 50_000;
const FUZZY_CAP: usize = 200_000;
const CHUNK_MIN: usize = 64 << 10;
const CAND_STEP: usize = 16_384;
/// Plain tokens tracked per query (bit masks).
const MAX_TOKENS: usize = 64;

pub struct RunOpts<'a> {
    pub limit: usize,
    /// Per volume: entry → boost.
    pub frecency: &'a [FastMap<u32, i32>],
    pub fuzzy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawHit {
    pub vol: usize,
    pub idx: u32,
    pub score: i32,
    pub fuzzy: bool,
}

#[derive(Default)]
pub struct RunOut {
    pub hits: Vec<RawHit>,
    pub total: u64,
    /// Candidate list for refinement (record offsets per volume), when small.
    pub cands: Option<Vec<Vec<u32>>>,
    pub used_cache: bool,
    /// Lazy metadata filtering hit its cap.
    pub truncated: bool,
}

/// Outcome of the name / filter checks for one record.
struct Checked {
    /// Summed class of the tokens found in the name (minus the scanned one).
    cls: i32,
    /// Bytes of the tokens found in the name (for the coverage bonus).
    in_name: usize,
    /// Plain tokens not in the name (bit i = token i): they must be found in a
    /// parent folder name (`path_tokens_ok`).
    missing: u64,
}

struct Compiled<'q> {
    q: &'q Query,
    /// Plain tokens: in the name or in a parent folder name.
    subs: Vec<SubT>,
    /// `dir\name` name parts: must be in the name.
    name_subs: Vec<SubT>,
    sub_len: usize,
    globs: Vec<Vec<u8>>,
    path_dirs: Vec<Vec<u8>>,
    excl: Vec<Finder<'static>>,
    /// Full-path excludes: (finder, pattern, is_prefix).
    path_excl: Vec<(Finder<'static>, Vec<u8>, bool)>,
    /// Same order as `path_excl`: single-folder patterns as folder predicates.
    ex_folder: Vec<Option<(FolderPred, Vec<u8>)>>,
    /// Single plain token and no other per-record checks (fast path).
    simple: bool,
    exts: Vec<Vec<u8>>,
    meta_filters: bool,
    pre: Pre,
    /// Scan rank of each plain token under `Pre::Subs`.
    rank: Vec<usize>,
    /// Single-token query: that token is always in the name.
    sole: Option<usize>,
}

impl<'q> Compiled<'q> {
    fn new(q: &'q Query, ixs: &[&Index]) -> Self {
        let mut subs: Vec<SubT> = Vec::new();
        for s in q.plain_terms() {
            if subs.len() < MAX_TOKENS && !subs.iter().any(|x| x.bytes == s) {
                subs.push(SubT { bytes: s.to_vec(), finder: Finder::new(s).into_owned() });
            }
        }
        let mut name_subs: Vec<SubT> = Vec::new();
        for t in &q.terms {
            if let Term::Path { name, .. } = t {
                if !name.is_empty() && !name_subs.iter().any(|x| &x.bytes == name) {
                    name_subs.push(SubT { bytes: name.clone(), finder: Finder::new(name).into_owned() });
                }
            }
        }
        let globs: Vec<Vec<u8>> =
            q.terms.iter().filter_map(|t| if let Term::Glob(g) = t { Some(g.clone()) } else { None }).collect();
        let path_dirs: Vec<Vec<u8>> =
            q.terms.iter().filter_map(|t| if let Term::Path { dir, .. } = t { Some(dir.clone()) } else { None }).collect();
        // `.gz` subsumes `.tar.gz`; keep the shortest so ext scans never double count.
        let mut exts: Vec<Vec<u8>> = q.exts.iter().map(|e| [b".".as_slice(), e].concat()).collect();
        exts.sort_by_key(|e| e.len());
        let mut kept: Vec<Vec<u8>> = Vec::new();
        for e in exts {
            if !kept.iter().any(|k| e.ends_with(k)) {
                kept.push(e);
            }
        }

        // Estimated hit count of a literal: rarest byte, shrunk per extra char.
        let mut freq = [0u64; 256];
        for ix in ixs {
            for (f, g) in freq.iter_mut().zip(ix.byte_freq.iter()) {
                *f += g;
            }
        }
        let est = |b: &[u8]| -> f64 {
            let m = b.iter().map(|&c| freq[c as usize]).min().unwrap_or(u64::MAX) as f64;
            m / 4f64.powi(b.len() as i32 - 1)
        };
        // A literal that must be in the name makes the cheapest prefilter;
        // otherwise every plain token is scanned (any of them may be the one
        // in the name while the rest sit in parent folders).
        let mut name_only: Option<(f64, Finder<'static>)> = None;
        let lits = name_subs.iter().map(|s| s.bytes.as_slice()).chain(globs.iter().map(|g| glob_literal(g)));
        for lit in lits.filter(|l| !l.is_empty()) {
            let e = est(lit);
            if name_only.as_ref().map_or(true, |(b, _)| e < *b) {
                name_only = Some((e, Finder::new(lit).into_owned()));
            }
        }
        let mut rank = vec![usize::MAX; subs.len()];
        let pre = if let Some((_, f)) = name_only {
            Pre::Lit(f)
        } else if !subs.is_empty() {
            let mut order: Vec<usize> = (0..subs.len()).collect();
            order.sort_by(|&a, &b| est(&subs[a].bytes).total_cmp(&est(&subs[b].bytes)));
            for (r, &i) in order.iter().enumerate() {
                rank[i] = r;
            }
            Pre::Subs(order)
        } else if !kept.is_empty() {
            Pre::Exts(kept.iter().map(|e| Finder::new(&[e.as_slice(), &[0u8]].concat()).into_owned()).collect())
        } else {
            Pre::All
        };
        let sole = (subs.len() == 1 && name_subs.is_empty() && globs.is_empty()).then_some(0);
        let simple = sole.is_some()
            && path_dirs.is_empty()
            && q.excludes.is_empty()
            && q.path_excludes.is_empty()
            && kept.is_empty()
            && q.kind.is_none();
        Compiled {
            q,
            sub_len: subs.iter().map(|s| s.bytes.len()).sum(),
            subs,
            name_subs,
            globs,
            path_dirs,
            excl: q.excludes.iter().map(|e| Finder::new(e).into_owned()).collect(),
            path_excl: q.path_excludes.iter().map(|(p, pre)| (Finder::new(p).into_owned(), p.clone(), *pre)).collect(),
            ex_folder: q.path_excludes.iter().map(|(p, pre)| folder_pred(p, *pre)).collect(),
            simple,
            exts: kept,
            meta_filters: q.has_meta_filters(),
            pre,
            rank,
            sole,
        }
    }

    #[inline]
    fn meta_ok(&self, size: u64, mtime: i64, dir: bool) -> bool {
        if !self.q.size.is_empty() && (dir || !self.q.size.iter().all(|&(c, v)| c.test(size, v))) {
            return false;
        }
        if !self.q.modified.is_empty() && (mtime <= 0 || !self.q.modified.iter().all(|&(c, v)| c.test(mtime, v))) {
            return false;
        }
        true
    }

    /// Full-path excludes (`!node_modules\`, `!C:\Windows`).
    /// Single-folder patterns are answered from the per-volume folder sets
    /// (`v.ex`) by walking the parents; others match the full path string.
    fn path_excluded(&self, v: &VolCtx, idx: u32, name: &[u8], st: &mut St) -> bool {
        if self.path_excl.is_empty() {
            return false;
        }
        let ix = v.ix;
        let parent = ix.parent[idx as usize];
        let mut by_string = false;
        if v.ex.is_some() && v.ex_all {
            return true;
        }
        for (j, fp) in self.ex_folder.iter().enumerate() {
            match (fp, v.ex) {
                (Some((pred, x)), Some(sets)) => {
                    let set = &sets[j];
                    match *pred {
                        FolderPred::Starts if name.starts_with(x) => return true,
                        FolderPred::Under(true) if set.has(idx) => return true,
                        _ => {}
                    }
                    if set.n > 0 {
                        let (mut d, mut guard) = (parent, 0);
                        while d != NONE && d != 0 && guard < 4096 {
                            if set.has(d) {
                                return true;
                            }
                            d = ix.parent[d as usize];
                            guard += 1;
                        }
                    }
                }
                _ => by_string = true,
            }
        }
        if !by_string {
            return false;
        }
        let (paths, buf) = (&mut st.paths, &mut st.buf);
        buf.clear();
        buf.extend_from_slice(paths.get(ix, parent));
        buf.extend_from_slice(name);
        self.path_excl.iter().zip(&self.ex_folder).any(|((f, pat, prefix), fp)| {
            if fp.is_some() && v.ex.is_some() {
                false // answered above
            } else if *prefix {
                buf.starts_with(pat) && (pat.last() == Some(&b'\\') || buf.len() == pat.len() || buf[pat.len()] == b'\\')
            } else {
                f.find(buf).is_some()
            }
        })
    }

    /// Folders (live dirs) whose lower-cased name contains `x` and satisfies
    /// `pred`, per scan task.
    fn collect_folders(&self, vctx: &[VolCtx], tasks: &[(usize, usize, usize)], x: &[u8], pred: FolderPred) -> Vec<(usize, Vec<u32>)> {
        let f = Finder::new(x);
        tasks
            .par_iter()
            .map(|&(vol, s, e)| {
                let ix = vctx[vol].ix;
                let hay = &ix.low[s..=e];
                let last = hay.len() - 1;
                let mut pos = 0usize;
                let mut out = Vec::new();
                while pos < last {
                    let Some(m) = f.find(&hay[pos..]) else { break };
                    let p = pos + m;
                    let h = memrchr(0, &hay[..p]).unwrap_or(0);
                    let ne = p + memchr(0, &hay[p..]).unwrap_or(last - p);
                    let hh = s + h;
                    let idx = decode_idx(&ix.low[hh + 1..hh + HDR]);
                    if (idx as usize) < ix.rec.len()
                        && ix.rec[idx as usize] as usize == hh
                        && ix.flags[idx as usize] & (DIR | DEAD | ROOT) == DIR
                        && pred.test(&ix.low[hh + HDR..s + ne], x)
                    {
                        out.push(idx);
                    }
                    pos = ne;
                }
                (vol, out)
            })
            .collect()
    }

    /// Name / flag / scope / path checks. `pre` = (scanned token, scan rank):
    /// that token is known to be in the name and its class is left to the
    /// caller; records holding an earlier-ranked token belong to that scan.
    #[inline]
    fn check(&self, v: &VolCtx, idx: u32, ns: usize, ne: usize, pre: Option<(usize, usize)>, st: &mut St) -> Option<Checked> {
        let ix = v.ix;
        let fl = ix.flags[idx as usize];
        if fl & (DEAD | ROOT) != 0 {
            return None;
        }
        match self.q.kind {
            Some(Kind::File) if fl & DIR != 0 => return None,
            Some(Kind::Folder) if fl & DIR == 0 => return None,
            _ => {}
        }
        let name = &ix.low[ns..ne];
        if !self.exts.is_empty() && (fl & DIR != 0 || !self.exts.iter().any(|e| name.len() > e.len() && name.ends_with(e))) {
            return None;
        }
        let orig = &ix.orig[ns..ne];
        let mut cls = 0;
        let mut in_name = 0usize;
        let mut missing = 0u64;
        for (i, t) in self.subs.iter().enumerate() {
            if let Some((pi, r)) = pre {
                if i == pi {
                    in_name += t.bytes.len();
                    continue;
                }
                match best_class(name, orig, &t.bytes, &t.finder) {
                    Some(_) if self.rank[i] < r => return None,
                    Some((c, _)) => {
                        cls += c;
                        in_name += t.bytes.len();
                    }
                    None => missing |= 1 << i,
                }
            } else {
                match best_class(name, orig, &t.bytes, &t.finder) {
                    Some((c, _)) => {
                        cls += c;
                        in_name += t.bytes.len();
                    }
                    None => missing |= 1 << i,
                }
            }
        }
        for t in &self.name_subs {
            cls += best_class(name, orig, &t.bytes, &t.finder)?.0;
            in_name += t.bytes.len();
        }
        // At least one text token has to be in the name itself.
        if in_name == 0 && !self.subs.is_empty() && self.globs.is_empty() {
            return None;
        }
        for g in &self.globs {
            if !glob_match(g, name) {
                return None;
            }
            cls += GLOB;
        }
        for e in &self.excl {
            if e.find(name).is_some() {
                return None;
            }
        }
        match &v.scope {
            Scope::All => {}
            Scope::Skip => return None,
            Scope::Under(dirs) => {
                if !in_scope(ix, idx, dirs, &mut st.scope_memo) {
                    return None;
                }
            }
        }
        if !self.path_dirs.is_empty() {
            let pp = st.paths.get(ix, ix.parent[idx as usize]);
            if !self.path_dirs.iter().all(|d| memchr::memmem::find(pp, d).is_some()) {
                return None;
            }
        }
        Some(Checked { cls, in_name, missing })
    }

    /// Plain tokens missing from the name must occur in some parent folder
    /// name (the root's own `c:\` doesn't count).
    fn path_tokens_ok(&self, v: &VolCtx, idx: u32, missing: u64, st: &mut St) -> bool {
        if missing == 0 {
            return true;
        }
        let ix = v.ix;
        let parent = ix.parent[idx as usize];
        match v.folders {
            // Folder sets known: walk the parents testing bits, no path strings.
            Some(sets) => {
                let mut m = missing;
                while m != 0 {
                    let i = m.trailing_zeros() as usize;
                    m &= m - 1;
                    if sets[i].n == 0 {
                        return false;
                    }
                }
                let mut need = missing;
                let (mut d, mut guard) = (parent, 0);
                while d != NONE && d != 0 && guard < 4096 {
                    let mut m = need;
                    while m != 0 {
                        let i = m.trailing_zeros() as usize;
                        m &= m - 1;
                        if sets[i].has(d) {
                            need &= !(1 << i);
                        }
                    }
                    if need == 0 {
                        return true;
                    }
                    d = ix.parent[d as usize];
                    guard += 1;
                }
                false
            }
            None => {
                let (o, l) = st.paths.range(ix, parent);
                let root_len = st.paths.root_len;
                let pp = &st.paths.arena[o as usize..(o + l) as usize];
                let folders = &pp[root_len.min(pp.len())..];
                let mut m = missing;
                while m != 0 {
                    let i = m.trailing_zeros() as usize;
                    m &= m - 1;
                    if self.subs[i].finder.find(folders).is_none() {
                        return false;
                    }
                }
                true
            }
        }
    }

    /// Everything `eval` requires, minus scoring (fuzzy uses it to skip
    /// records the substring pass already counted).
    fn full_match(&self, v: &VolCtx, idx: u32, ns: usize, ne: usize, st: &mut St) -> bool {
        match self.check(v, idx, ns, ne, None, st) {
            Some(c) => self.path_tokens_ok(v, idx, c.missing, st) && !self.path_excluded(v, idx, &v.ix.low[ns..ne], st),
            None => false,
        }
    }

    #[inline]
    fn base_score(&self, cls: i32, in_name: usize, name_len: usize) -> i32 {
        let n = (self.subs.len() + self.name_subs.len() + self.globs.len()) as i32;
        if n == 0 {
            return FILTER_ONLY;
        }
        let cover = if in_name > 0 && name_len > 0 {
            (COVER_MAX as usize * in_name.min(name_len) / name_len) as i32
        } else {
            0
        };
        cls / n + cover
    }

    #[inline]
    fn adjust(&self, v: &VolCtx, idx: u32) -> i32 {
        let fl = v.ix.flags[idx as usize];
        let mut s = 0;
        if fl & DIR != 0 {
            s += DIR_BONUS;
        }
        if fl & (HIDDEN | SYSTEM) != 0 {
            s -= HIDDEN_PENALTY;
        }
        if let Some(f) = v.frec {
            if let Some(b) = f.get(&idx) {
                s += *b;
            }
        }
        s
    }

    #[inline]
    fn floor(st: &St, sh: &Shared) -> u64 {
        st.top.floor().max(sh.thr.load(Relaxed))
    }

    #[inline]
    fn offer(&self, v: &VolCtx, idx: u32, pre: i32, st: &mut St, sh: &Shared) {
        let floor = Self::floor(st, sh);
        if pack(pre - DEPTH_STEP, v.vol, idx) <= floor {
            return;
        }
        let d = v.ix.depth(idx).min(DEPTH_CAP) as i32;
        let key = pack(pre - DEPTH_STEP * d, v.vol, idx);
        if key > floor && st.top.push(key) {
            sh.thr.fetch_max(st.top.floor(), Relaxed);
        }
    }

    /// `pre`: (scanned token, its scan rank, position of its first hit in the name).
    #[inline]
    fn eval(&self, v: &VolCtx, h: usize, ns: usize, ne: usize, pre: Option<(usize, usize, usize)>, st: &mut St, sh: &Shared) {
        let ix = v.ix;
        let idx = decode_idx(&ix.low[h + 1..h + HDR]);
        if idx as usize >= ix.rec.len() || ix.rec[idx as usize] as usize != h {
            return;
        }
        let c = if self.simple && pre.is_some() {
            // One token, already found in the name, no other per-record checks.
            if ix.flags[idx as usize] & (DEAD | ROOT) != 0 {
                return;
            }
            if let Scope::Under(dirs) = &v.scope {
                if !in_scope(ix, idx, dirs, &mut st.scope_memo) {
                    return;
                }
            } else if matches!(v.scope, Scope::Skip) {
                return;
            }
            Checked { cls: 0, in_name: self.sub_len, missing: 0 }
        } else {
            let Some(c) = self.check(v, idx, ns, ne, pre.map(|p| (p.0, p.1)), st) else { return };
            c
        };
        if c.missing != 0 {
            // Tokens whose folder sets are complete are settled now; the rest
            // wait for their own scan pass.
            let known = if v.folders.is_some() { v.known } else { u64::MAX };
            let now = c.missing & known;
            if now != 0 && !self.path_tokens_ok(v, idx, now, st) {
                return;
            }
            if c.missing & !known != 0 {
                if let (true, Some((ti, rank, pos))) = (st.defer, pre) {
                    st.pending.push(Pending { h: h as u32, ne: ne as u32, ti: ti as u32, rank: rank as u32, pos: pos as u32 });
                }
                return;
            }
        }
        if self.path_excluded(v, idx, &ix.low[ns..ne], st) {
            return;
        }
        let tier = if c.missing != 0 { PATH_TIER } else { NAME_TIER };
        let mut cls = c.cls;
        let pre_class = |ti: usize| {
            let t = &self.subs[ti];
            best_class(&ix.low[ns..ne], &ix.orig[ns..ne], &t.bytes, &t.finder).map_or(SUBSTR, |c| c.0)
        };
        if self.meta_filters {
            let i = idx as usize;
            if ix.has_meta {
                if !self.meta_ok(ix.size[i], ix.mtime[i] as i64, ix.flags[i] & DIR != 0) {
                    return;
                }
            } else {
                let full = cls + pre.map_or(0, |p| pre_class(p.0));
                st.lazy.push((idx, self.base_score(full, c.in_name, ne - ns) + tier));
                return;
            }
        }
        st.total += 1;
        if st.collecting {
            st.cands.push(h as u32);
            if st.cands.len() & 1023 == 0 && sh.cands.fetch_add(1024, Relaxed) + 1024 > CACHE_CAP {
                sh.overflow.store(true, Relaxed);
                st.collecting = false;
                st.cands = Vec::new();
            }
        }
        let adj = self.adjust(v, idx) + tier;
        if let Some((ti, _, pos)) = pre {
            // A first hit past the name start can at best be a word-boundary match:
            // skip the exact class computation when even that cannot make the cut.
            let ub = if pos == 0 { EXACT } else { WORD };
            let bound = self.base_score(cls + ub, c.in_name, ne - ns) + adj;
            if pack(bound - DEPTH_STEP, v.vol, idx) <= Self::floor(st, sh) {
                return;
            }
            cls += pre_class(ti);
        }
        let score = self.base_score(cls, c.in_name, ne - ns) + adj;
        self.offer(v, idx, score, st, sh);
    }

    /// `term`: (plain token, scan rank) when scanning for a plain token.
    #[allow(clippy::too_many_arguments)]
    fn scan_finder(&self, f: &Finder, term: Option<(usize, usize)>, v: &VolCtx, base: usize, hay: &[u8], st: &mut St, sh: &Shared) {
        let last = hay.len() - 1;
        let mut pos = 0usize;
        while pos < last {
            let Some(m) = f.find(&hay[pos..]) else { break };
            let p = pos + m;
            let h = memrchr(0, &hay[..p]).unwrap_or(0);
            let ne = p + memchr(0, &hay[p..]).unwrap_or(last - p);
            let pre = term.map(|(t, r)| (t, r, p - h - HDR));
            // Multi-token pass: every folder whose name holds the token is a
            // hit of this scan — remember it for the parent-folder checks.
            if let (Some((t, _)), false) = (term, st.dir_hits.is_empty()) {
                let ix = v.ix;
                let hh = base + h;
                let idx = decode_idx(&ix.low[hh + 1..hh + HDR]);
                if (idx as usize) < ix.rec.len()
                    && ix.rec[idx as usize] as usize == hh
                    && ix.flags[idx as usize] & (DIR | DEAD | ROOT) == DIR
                {
                    st.dir_hits[t].push(idx);
                }
                if st.collect_only {
                    pos = ne;
                    continue;
                }
            }
            self.eval(v, base + h, base + h + HDR, base + ne, pre, st, sh);
            pos = ne;
        }
    }

    /// Scans `[start, end]` (both record separators) of one volume.
    /// `pass`: which plain token (by scan rank) to scan for under `Pre::Subs`.
    #[allow(clippy::too_many_arguments)]
    fn scan_chunk(&self, v: &VolCtx, start: usize, end: usize, pass: usize, st: &mut St, sh: &Shared) {
        let low = &v.ix.low;
        let hay = &low[start..=end];
        match &self.pre {
            Pre::Subs(order) => {
                let ti = order[pass];
                self.scan_finder(&self.subs[ti].finder, Some((ti, pass)), v, start, hay, st, sh);
            }
            Pre::Lit(f) => self.scan_finder(f, None, v, start, hay, st, sh),
            Pre::Exts(fs) => {
                for f in fs {
                    self.scan_finder(f, None, v, start, hay, st, sh);
                }
            }
            Pre::All => {
                let mut h = start;
                while h < end {
                    let ns = h + HDR;
                    let ne = ns + memchr(0, &low[ns..=end]).unwrap_or(0);
                    self.eval(v, h, ns, ne, None, st, sh);
                    h = ne;
                }
            }
        }
    }

    fn scan_cands(&self, v: &VolCtx, cands: &[u32], st: &mut St, sh: &Shared) {
        let low = &v.ix.low;
        for &h in cands {
            let h = h as usize;
            let ns = h + HDR;
            let Some(n) = memchr(0, &low[ns..]) else { continue };
            // A single-token query's token is always in the name.
            let pre = match self.sole {
                Some(i) => match self.subs[i].finder.find(&low[ns..ns + n]) {
                    Some(p) => Some((i, 0, p)),
                    None => continue,
                },
                None => None,
            };
            self.eval(v, h, ns, ns + n, pre, st, sh);
        }
    }

    /// Subsequence + nucleo scoring for records the substring pass missed.
    #[allow(clippy::too_many_arguments)]
    fn fuzzy_chunk(&self, v: &VolCtx, start: usize, end: usize, byte: u8, pat: &Pattern, st: &mut St, sh: &Shared) {
        let low = &v.ix.low;
        let hay = &low[start..=end];
        let last = hay.len() - 1;
        let mut pos = 0usize;
        let needle_chars: usize = self.subs.iter().map(|s| s.bytes.len()).sum();
        let mut buf = Vec::new();
        while pos < last {
            let Some(m) = memchr(byte, &hay[pos..last]) else { break };
            let p = pos + m;
            let h = memrchr(0, &hay[..p]).unwrap_or(0);
            let ne = p + memchr(0, &hay[p..]).unwrap_or(last - p);
            pos = ne;
            let (hh, ns, nend) = (start + h, start + h + HDR, start + ne);
            let name = &low[ns..nend];
            if !self.subs.iter().all(|s| is_subsequence(&s.bytes, name)) {
                continue;
            }
            if sh.fuzzy_seen.fetch_add(1, Relaxed) > FUZZY_CAP {
                break;
            }
            let ix = v.ix;
            let idx = decode_idx(&low[hh + 1..hh + HDR]);
            if idx as usize >= ix.rec.len() || ix.rec[idx as usize] as usize != hh {
                continue;
            }
            // Already counted by the substring pass (tokens in the name or in
            // parent folders).
            if self.full_match(v, idx, ns, nend, st) {
                continue;
            }
            // Same filters as `check`, minus the name tokens.
            let fl = ix.flags[idx as usize];
            if fl & (DEAD | ROOT) != 0
                || matches!(self.q.kind, Some(Kind::File) if fl & DIR != 0)
                || matches!(self.q.kind, Some(Kind::Folder) if fl & DIR == 0)
                || (!self.exts.is_empty() && (fl & DIR != 0 || !self.exts.iter().any(|e| name.ends_with(e))))
                || self.excl.iter().any(|e| e.find(name).is_some())
            {
                continue;
            }
            match &v.scope {
                Scope::All => {}
                Scope::Skip => continue,
                Scope::Under(d) => {
                    if !in_scope(ix, idx, d, &mut st.scope_memo) {
                        continue;
                    }
                }
            }
            if self.path_excluded(v, idx, name, st) {
                continue;
            }
            if self.meta_filters {
                if ix.has_meta {
                    let i = idx as usize;
                    if !self.meta_ok(ix.size[i], ix.mtime[i] as i64, fl & DIR != 0) {
                        continue;
                    }
                } else {
                    continue; // no lazy metadata for fuzzy candidates
                }
            }
            let Ok(oname) = std::str::from_utf8(&ix.orig[ns..nend]) else { continue };
            let score = MATCHER.with(|m| pat.score(Utf32Str::new(oname, &mut buf), &mut m.borrow_mut()));
            let Some(score) = score else { continue };
            if (score as usize) < needle_chars * 8 {
                continue;
            }
            st.total += 1;
            let f = (score as f32 / (needle_chars.max(1) as f32 * 24.0)).min(1.0);
            let cover = (500 * needle_chars.min(name.len()) / name.len().max(1)) as i32;
            let pre = (FUZZY_MAX as f32 * f) as i32 + cover + self.adjust(v, idx);
            self.offer(v, idx, pre, st, sh);
        }
    }
}

thread_local! {
    static MATCHER: RefCell<Matcher> = RefCell::new(Matcher::new(Config::DEFAULT));
}

fn fuzzy_pattern(subs: &[SubT]) -> Pattern {
    let mut p = Pattern::default();
    p.atoms = subs
        .iter()
        .map(|s| {
            let text = String::from_utf8_lossy(&s.bytes);
            Atom::new(&text, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy, false)
        })
        .collect();
    p
}

/// Is `idx` strictly below every folder in `dirs`? Memoized per parent.
fn in_scope(ix: &Index, idx: u32, dirs: &[u32], memo: &mut FastMap<u32, bool>) -> bool {
    let p = ix.parent[idx as usize];
    if let Some(&r) = memo.get(&p) {
        return r;
    }
    let r = dirs.iter().all(|&d| d != idx && ix.is_ancestor(d, p));
    memo.insert(p, r);
    r
}

/// Buffer chunks for all non-skipped volumes, sized for ~6 tasks per thread.
fn chunk_tasks(vctx: &[VolCtx]) -> Vec<(usize, usize, usize)> {
    let total: usize = vctx.iter().map(|v| v.ix.low.len()).sum();
    let target = (total / (rayon::current_num_threads() * 6).max(1)).max(CHUNK_MIN);
    let mut tasks = Vec::new();
    for v in vctx {
        if !matches!(v.scope, Scope::Skip) {
            chunks(&v.ix.low, target, v.vol, &mut tasks);
        }
    }
    tasks
}

/// Record-aligned chunk boundaries `[start, end]` over a scan buffer.
fn chunks(low: &[u8], target: usize, vol: usize, out: &mut Vec<(usize, usize, usize)>) {
    let last = low.len() - 1;
    let mut s = 0usize;
    while s < last {
        let mut e = (s + target).min(last);
        if e < last {
            e += memchr(0, &low[e..]).unwrap_or(last - e);
        }
        out.push((vol, s, e));
        s = e;
    }
}

fn resolve_scopes(q: &Query, ix: &Index) -> Scope {
    if q.scopes.is_empty() {
        return Scope::All;
    }
    let root = ix.root_key();
    let mut dirs = Vec::new();
    for s in &q.scopes {
        let key = lower(s.trim_end_matches('\\'));
        if key.len() < root.len() {
            // Scope is above this root (e.g. `in:C:\` for a `C:\Users\me` walk root).
            if root.starts_with(&key) && root.get(key.len()) == Some(&b'\\') {
                continue;
            }
            return Scope::Skip;
        }
        match ix.resolve(s) {
            Some(0) => {}
            Some(d) => dirs.push(d),
            None => return Scope::Skip,
        }
    }
    if dirs.is_empty() {
        Scope::All
    } else {
        Scope::Under(dirs)
    }
}

/// Runs a query over a set of (read-locked) indexes.
pub fn run(ixs: &[&Index], q: &Query, opts: &RunOpts, cached: Option<&[Vec<u32>]>) -> RunOut {
    let k = opts.limit.max(1);
    if q.is_empty() || ixs.is_empty() {
        return RunOut::default();
    }
    let c = Compiled::new(q, ixs);
    let vctx: Vec<VolCtx> = ixs
        .iter()
        .enumerate()
        .map(|(i, ix)| VolCtx {
            vol: i,
            ix,
            scope: resolve_scopes(q, ix),
            frec: opts.frecency.get(i).filter(|m| !m.is_empty()),
            folders: None,
            known: 0,
            ex: None,
            ex_all: false,
        })
        .collect();
    let sh = Shared {
        thr: AtomicU64::new(0),
        cands: AtomicUsize::new(0),
        overflow: AtomicBool::new(false),
        fuzzy_seen: AtomicUsize::new(0),
    };
    let collect = !c.subs.is_empty() && !c.meta_filters;
    let used_cache = cached.is_some_and(|cv| cv.len() == ixs.len());

    // (volume, start, end): buffer chunks, or slices of the cached candidate lists.
    let tasks: Vec<(usize, usize, usize)> = match cached {
        Some(cv) if used_cache => {
            let mut t = Vec::new();
            let n: usize = cv.iter().map(|l| l.len()).sum();
            let step = (n / (rayon::current_num_threads() * 4).max(1)).clamp(512, CAND_STEP);
            for (vol, list) in cv.iter().enumerate() {
                if matches!(vctx[vol].scope, Scope::Skip) {
                    continue;
                }
                let mut s = 0;
                while s < list.len() {
                    t.push((vol, s, (s + step).min(list.len())));
                    s += step;
                }
            }
            t
        }
        _ => chunk_tasks(&vctx),
    };
    // Single-folder path excludes (`!node_modules\`): one name scan per
    // pattern collects the matching folders, so records are tested with a
    // parent walk over bit sets instead of building their path strings.
    let ex_sets: Vec<Vec<FolderSet>> = if !used_cache && c.ex_folder.iter().any(|f| f.is_some()) {
        let mut sets = vec![vec![FolderSet::default(); c.ex_folder.len()]; ixs.len()];
        for (j, fp) in c.ex_folder.iter().enumerate() {
            match fp {
                Some((FolderPred::Under(_), t)) => {
                    let t = String::from_utf8_lossy(t);
                    for (vol, ix) in ixs.iter().enumerate() {
                        if let Some(f) = ix.resolve(&t).filter(|&f| f != 0) {
                            sets[vol][j].add(ix.slots(), f);
                        }
                    }
                }
                Some((pred, x)) => {
                    for (vol, hits) in c.collect_folders(&vctx, &tasks, x, *pred) {
                        for i in hits {
                            sets[vol][j].add(ixs[vol].slots(), i);
                        }
                    }
                }
                None => {}
            }
        }
        sets
    } else {
        Vec::new()
    };
    let vctx: Vec<VolCtx> = vctx
        .into_iter()
        .map(|mut v| {
            if !ex_sets.is_empty() {
                v.ex = Some(&ex_sets[v.vol]);
                // The root's own path (e.g. `c:\users\me\`) matching a pattern
                // excludes the whole volume.
                let root = v.ix.root_key();
                let mut root_slash = root.clone();
                root_slash.push(b'\\');
                v.ex_all = c.path_excl.iter().zip(&c.ex_folder).any(|((f, _, _), fp)| match fp {
                    // `!C:\` or `!C:\Users` while indexing `C:\Users\me`.
                    Some((FolderPred::Under(_), t)) => {
                        root.starts_with(t) && (root.len() == t.len() || root[t.len()] == b'\\')
                    }
                    Some(_) => f.find(&root_slash).is_some(),
                    None => false,
                });
            }
            v
        })
        .collect();
    // Multi-token queries: one scan pass per plain token, rarest first. A
    // record belongs to the pass of the first-ranked token in its name, so
    // every earlier-ranked token must sit in one of its parent folders. Each
    // pass also collects the folders whose name holds its token; once a pass
    // is done that set is complete, so later passes settle earlier tokens with
    // ancestor bit masks (no path strings) and skip a volume outright when an
    // earlier token occurs in none of its folders. Records still waiting on a
    // later token's folders are deferred and settled after the last pass.
    let order: Vec<usize> = match (&c.pre, used_cache) {
        (Pre::Subs(o), false) => o.clone(),
        _ => Vec::new(),
    };
    let multi = order.len() >= 2;
    let passes = order.len().max(1);
    let mut out = RunOut { used_cache, ..Default::default() };
    let mut top = TopK::new(k);
    let mut lazy: Vec<(usize, u32, i32)> = Vec::new();
    let mut cands: Vec<Vec<u32>> = vec![Vec::new(); ixs.len()];
    let mut sets: Vec<Vec<FolderSet>> = vec![vec![FolderSet::default(); c.subs.len()]; if multi { ixs.len() } else { 0 }];
    let mut pending: Vec<(usize, Vec<Pending>)> = Vec::new();
    let mut known = 0u64;
    let mut absorb = |st: St, vol: usize, out: &mut RunOut, top: &mut TopK| {
        out.total += st.total;
        for r in st.top.heap {
            top.push(r.0);
        }
        if collect {
            cands[vol].extend_from_slice(&st.cands);
        }
        lazy.extend(st.lazy.into_iter().map(|(i, base)| (vol, i, base)));
    };
    for pass in 0..passes {
        // Volumes where an earlier token is in no folder can't match any more.
        let alive: Vec<bool> = (0..ixs.len())
            .map(|vol| !multi || order[..pass].iter().all(|&t| sets[vol][t].n > 0))
            .collect();
        // Dead volumes with deferred records still need this token's folders.
        let waiting: Vec<bool> = (0..ixs.len()).map(|vol| pending.iter().any(|(v, _)| *v == vol)).collect();
        if !(0..ixs.len()).any(|v| alive[v] || waiting[v]) {
            break;
        }
        let states: Vec<(usize, St)> = {
            let vp: Vec<VolCtx> = vctx
                .iter()
                .map(|v| VolCtx {
                    vol: v.vol,
                    ix: v.ix,
                    scope: v.scope.clone(),
                    frec: v.frec,
                    folders: if multi { Some(&sets[v.vol]) } else { None },
                    known,
                    ex: v.ex,
                    ex_all: v.ex_all,
                })
                .collect();
            tasks
                .par_iter()
                .filter(|t| alive[t.0] || waiting[t.0])
                .map(|&(vol, a, b)| {
                    let mut st = St::new(k, collect);
                    if multi {
                        st.defer = true;
                        st.collect_only = !alive[vol];
                        st.dir_hits = vec![Vec::new(); c.subs.len()];
                    }
                    match cached {
                        Some(cv) if used_cache => c.scan_cands(&vp[vol], &cv[vol][a..b], &mut st, &sh),
                        _ => c.scan_chunk(&vp[vol], a, b, pass, &mut st, &sh),
                    }
                    (vol, st)
                })
                .collect()
        };
        // Tasks are ordered by (volume, offset), so concatenating keeps buffer order.
        for (vol, mut st) in states {
            if multi {
                for (t, hits) in st.dir_hits.drain(..).enumerate() {
                    for i in hits {
                        sets[vol][t].add(ixs[vol].slots(), i);
                    }
                }
                if !st.pending.is_empty() {
                    pending.push((vol, std::mem::take(&mut st.pending)));
                }
            }
            absorb(st, vol, &mut out, &mut top);
        }
        if multi {
            known |= 1 << order[pass];
        }
    }
    let vctx: Vec<VolCtx> = vctx
        .into_iter()
        .map(|mut v| {
            if multi {
                v.folders = Some(&sets[v.vol]);
                v.known = known;
            }
            v
        })
        .collect();
    if !pending.is_empty() {
        let res: Vec<(usize, St)> = pending
            .par_iter()
            .map(|(vol, list)| {
                let mut st = St::new(k, collect);
                let v = &vctx[*vol];
                for p in list {
                    let h = p.h as usize;
                    let pre = Some((p.ti as usize, p.rank as usize, p.pos as usize));
                    c.eval(v, h, h + HDR, p.ne as usize, pre, &mut st, &sh);
                }
                (*vol, st)
            })
            .collect();
        for (vol, st) in res {
            absorb(st, vol, &mut out, &mut top);
        }
    }
    drop(absorb);
    if collect && !sh.overflow.load(Relaxed) {
        out.cands = Some(cands);
    }

    // Size/date filters on volumes without stored metadata: stat candidates.
    if !lazy.is_empty() {
        if lazy.len() > LAZY_CAP {
            lazy.truncate(LAZY_CAP);
            out.truncated = true;
        }
        let res: Vec<(u64, TopK)> = lazy
            .par_chunks(256)
            .map(|chunk| {
                let mut st = St::new(k, false);
                let mut n = 0u64;
                for &(vol, idx, base) in chunk {
                    let v = &vctx[vol];
                    let path = v.ix.path(idx);
                    let Ok(m) = std::fs::symlink_metadata(crate::paths::for_io(&path)) else { continue };
                    let mt = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    if !c.meta_ok(m.len(), mt, m.is_dir()) {
                        continue;
                    }
                    n += 1;
                    c.offer(v, idx, base + c.adjust(v, idx), &mut st, &sh);
                }
                (n, st.top)
            })
            .collect();
        for (n, t) in res {
            out.total += n;
            for r in t.heap {
                top.push(r.0);
            }
        }
    }

    // Fuzzy top-up when substring matches are scarce.
    let mut fuzzy_keys: FastSet<u64> = FastSet::default();
    if opts.fuzzy && (out.total as usize) < k && c.globs.is_empty() && c.path_dirs.is_empty() && c.sub_len >= 3 {
        let found: FastSet<u64> = top.heap.iter().map(|r| r.0).collect();
        let mut freq = [0u64; 256];
        for ix in ixs {
            for (f, g) in freq.iter_mut().zip(ix.byte_freq.iter()) {
                *f += g;
            }
        }
        let byte = c.subs.iter().flat_map(|s| s.bytes.iter().copied()).min_by_key(|&b| freq[b as usize]).unwrap();
        let pat = fuzzy_pattern(&c.subs);
        let tasks = chunk_tasks(&vctx);
        // Fresh threshold: fuzzy hits never outrank substring hits anyway.
        sh.thr.store(0, Relaxed);
        let res: Vec<St> = tasks
            .par_iter()
            .map(|&(vol, s, e)| {
                let mut st = St::new(k, false);
                c.fuzzy_chunk(&vctx[vol], s, e, byte, &pat, &mut st, &sh);
                st
            })
            .collect();
        for st in res {
            out.total += st.total;
            for r in st.top.heap {
                if !found.contains(&r.0) {
                    fuzzy_keys.insert(r.0);
                    top.push(r.0);
                }
            }
        }
    }

    let mut keys: Vec<u64> = top.heap.into_iter().map(|r| r.0).collect();
    keys.sort_unstable_by(|a, b| b.cmp(a));
    out.hits = keys
        .into_iter()
        .map(|key| {
            let (score, vol, idx) = unpack(key);
            RawHit { vol, idx, score, fuzzy: fuzzy_keys.contains(&key) }
        })
        .collect();
    out
}

/// UTF-16 offsets of the characters to highlight in the hit's name.
pub fn highlight(ix: &Index, idx: u32, q: &Query, fuzzy: bool) -> Vec<u32> {
    let name = ix.name(idx);
    let lname = ix.name_low(idx);
    let orig = name.as_bytes();
    let terms: Vec<&[u8]> = q.sub_terms().collect();
    let mut char_hits: Vec<u32> = Vec::new(); // char indices
    if fuzzy {
        let subs: Vec<SubT> =
            terms.iter().map(|t| SubT { bytes: t.to_vec(), finder: Finder::new(t).into_owned() }).collect();
        let pat = fuzzy_pattern(&subs);
        let mut buf = Vec::new();
        let mut ind = Vec::new();
        MATCHER.with(|m| pat.indices(Utf32Str::new(name, &mut buf), &mut m.borrow_mut(), &mut ind));
        char_hits = ind;
    } else {
        let mut byte_ranges: Vec<(usize, usize)> = Vec::new();
        for t in &terms {
            let f = Finder::new(t);
            if let Some((_, p)) = best_class(lname, orig, t, &f) {
                byte_ranges.push((p, p + t.len()));
            }
        }
        for (ci, (bi, _)) in name.char_indices().enumerate() {
            if byte_ranges.iter().any(|&(a, b)| bi >= a && bi < b) {
                char_hits.push(ci as u32);
            }
        }
    }
    char_hits.sort_unstable();
    char_hits.dedup();
    // char index → UTF-16 offset (astral chars take two units).
    let mut out = Vec::with_capacity(char_hits.len());
    let mut u16pos = 0u32;
    let mut it = char_hits.iter().peekable();
    for (ci, ch) in name.chars().enumerate() {
        while let Some(&&c) = it.peek() {
            if c as usize == ci {
                out.push(u16pos);
                if ch.len_utf16() == 2 {
                    out.push(u16pos + 1);
                }
                it.next();
            } else if (c as usize) < ci {
                it.next();
            } else {
                break;
            }
        }
        u16pos += ch.len_utf16() as u32;
    }
    out
}

/// Cached candidates of the previous query for incremental refinement.
pub struct QueryCache {
    pub query: Query,
    /// (volume identity, generation) at the time of caching.
    pub vols: Vec<(usize, u64)>,
    pub cands: Arc<Vec<Vec<u32>>>,
}

impl QueryCache {
    pub fn usable_for(&self, q: &Query, vols: &[(usize, u64)]) -> bool {
        self.vols == vols && q.refines(&self.query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Query {
        parse_at(s, 1_800_000_000)
    }

    #[test]
    fn parses_terms_phrases_filters() {
        let q = p(r#"readme "foo bar" ext:PDF,.docx type:folder !tmp in:c:/dev size:>100mb modified:<7d"#);
        assert_eq!(q.terms, vec![Term::Sub(b"readme".to_vec()), Term::Sub(b"foo bar".to_vec())]);
        assert_eq!(q.exts, vec![b"pdf".to_vec(), b"docx".to_vec()]);
        assert_eq!(q.kind, Some(Kind::Folder));
        assert_eq!(q.excludes, vec![b"tmp".to_vec()]);
        assert_eq!(q.scopes, vec![r"C:\dev".to_string()]);
        assert_eq!(q.size, vec![(Cmp::Gt, 100 * 1024 * 1024)]);
        assert_eq!(q.modified, vec![(Cmp::Gt, 1_800_000_000 - 7 * 86_400)]);

        let q = p("size:<1kb size:1mb..2mb modified:>30d");
        assert_eq!(q.size, vec![(Cmp::Lt, 1024), (Cmp::Ge, 1 << 20), (Cmp::Le, 2 << 20)]);
        assert_eq!(q.modified, vec![(Cmp::Lt, 1_800_000_000 - 30 * 86_400)]);

        let q = p(r#"in:"C:\Program Files" *.rs src\main c: a?c"#);
        assert_eq!(q.scopes, vec![r"C:\Program Files".to_string()]);
        assert_eq!(
            q.terms,
            vec![
                Term::Glob(b"*.rs".to_vec()),
                Term::Path { dir: b"src\\".to_vec(), name: b"main".to_vec() },
                Term::Path { dir: b"c:\\".to_vec(), name: vec![] },
                Term::Glob(b"a?c".to_vec()),
            ]
        );
        // incomplete filters while typing are ignored, not turned into text
        let q = p("size:> modified: type:");
        assert!(q.is_empty());
        assert!(p("   ").is_empty());
        // date filters
        let q = p("modified:>2026-01-01");
        assert_eq!(q.modified.len(), 1);
        assert_eq!(q.modified[0].0, Cmp::Ge);
        assert_eq!(q.modified[0].1, local_midnight(chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()));
        let q = p("dm:2026-03-04");
        assert_eq!(q.modified.len(), 2);
    }

    #[test]
    fn parses_path_excludes() {
        let q = p(r"package.json !node_modules\ !\Backup\ !D: !C:\Windows !/tmp/ !old");
        assert_eq!(q.terms, vec![Term::Sub(b"package.json".to_vec())]);
        assert_eq!(q.excludes, vec![b"old".to_vec()]);
        assert_eq!(
            q.path_excludes,
            vec![
                (br"node_modules\".to_vec(), false),
                (br"\backup\".to_vec(), false),
                (br"d:\".to_vec(), true),
                (br"c:\windows".to_vec(), true),
                (br"\tmp\".to_vec(), false),
            ]
        );
        // path excludes alone don't make a query
        assert!(p(r"!node_modules\").is_empty());
    }

    #[test]
    fn refinement_rules() {
        let a = p("nod ext:js");
        assert!(p("node ext:js").refines(&a));
        // A new token may match a parent folder instead of the name, so adding
        // one is not a refinement (the old token might then be path-only).
        assert!(!p("nod x ext:js").refines(&a));
        assert!(p("nodule ext:js").refines(&a));
        assert!(!p("nod ext:js !x\\").refines(&a));
        assert!(p("nod ext:js").refines(&a));
        assert!(!p("node").refines(&a));
        assert!(!p("no ext:js").refines(&a));
        assert!(!p("node ext:js").refines(&p("ext:js")));
    }

    #[test]
    fn globbing() {
        assert!(glob_match(b"*.rs", b"main.rs"));
        assert!(!glob_match(b"*.rs", b"main.rsx"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(glob_match("a?c".as_bytes(), "aéc".as_bytes()));
        assert!(glob_match(b"*read*me*", b"xreadxxmex"));
        assert!(!glob_match(b"a*b", b"acbx"));
        assert_eq!(glob_literal(b"*read*me*"), b"read");
    }

    #[test]
    fn classes() {
        let f = |n: &str, t: &str| {
            let fi = Finder::new(t.as_bytes());
            best_class(&lower(n), n.as_bytes(), &lower(t), &fi).map(|x| x.0)
        };
        assert_eq!(f("node", "node"), Some(EXACT));
        assert_eq!(f("node.exe", "node"), Some(STEM));
        assert_eq!(f("node_modules", "node"), Some(PREFIX));
        assert_eq!(f("my-node", "node"), Some(WORD));
        assert_eq!(f("myNode", "node"), Some(WORD));
        assert_eq!(f("anode", "node"), Some(SUBSTR));
        assert_eq!(f("anode node2", "node"), Some(WORD));
        assert_eq!(f("nod", "node"), None);
        assert_eq!(f("file2024", "2024"), Some(WORD));
    }

    #[test]
    fn pack_roundtrip() {
        for (s, v, i) in [(0, 0, 0), (-1500, 3, 77), (25_000, 1, u32::MAX - 1)] {
            assert_eq!(unpack(pack(s, v, i)), (s, v, i));
        }
        assert!(pack(10, 0, 5) > pack(9, 0, 1));
        assert!(pack(10, 0, 1) > pack(10, 0, 5));
    }
}
