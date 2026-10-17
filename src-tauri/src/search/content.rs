//! Search inside files: `content:"some text"` in Ctrl+K or a search tab.
//!
//! The index only knows names, so this walks the folder being searched (the palette's "This
//! folder", or an `in:` filter) and reads the files in it. That happens here, in Ferret's own
//! process with the user's rights: the indexer service runs as SYSTEM and only answers name
//! queries, so it never reads anyone's files. The rest of the query (`ext:`, `size:`,
//! `modified:`, words that must be in the path, `!` excludes) picks the files before any is read.
//!
//! Text matches ignoring case, in UTF-8, UTF-16 with a byte order mark, and ANSI files (for
//! ASCII text). Binary files (by extension, or a NUL byte near the start), files over 64 MB,
//! and `.git` and `node_modules` folders below the one searched are skipped.

use super::engine::{Hit, LineMatch, SearchResults};
use super::index::lower;
use super::query::{self, Kind, Query, Term};
use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, UNIX_EPOCH};

/// Bigger files aren't read.
const MAX_BYTES: u64 = 64 << 20;
/// How far into a file a NUL byte makes it binary.
const SNIFF: usize = 8 << 10;
/// Files read in parallel at a time (results keep the folder's order).
const BATCH: usize = 256;
/// Longest piece of a line shown with a result.
const EXCERPT: usize = 160;
/// Matches counted per file, at most.
const MAX_COUNT: usize = 9_999;
/// Folders never searched below the one asked for: version control data, installed
/// packages, and Windows' own.
const SKIP_DIRS: [&str; 4] = [".git", "node_modules", "$recycle.bin", "system volume information"];
/// Always binary (or compressed, like PDF and Word files), so not even opened unless an
/// `ext:` asks for them. Build folders are full of the first few.
const BINARY_EXTS: &[&str] = &[
    "exe", "dll", "pdb", "rlib", "rmeta", "lib", "obj", "o", "a", "so", "dylib", "class", "pyc", "wasm", "node", "msi", "cab",
    "zip", "7z", "rar", "gz", "xz", "zst", "bz2", "tar", "iso", "jar", "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "heic",
    "tif", "tiff", "psd", "mp4", "mkv", "mov", "avi", "webm", "mp3", "flac", "wav", "ogg", "m4a", "pdf", "docx", "xlsx",
    "pptx", "woff", "woff2", "ttf", "otf", "sqlite", "db",
];

/// Bumped to stop the palette's running search (a newer one is coming).
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Stops the palette's content searches running now.
pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Takes the `content:` filters out of a query: the rest (for names and filters) and the texts
/// every result must contain. `content:` with nothing after it yet is dropped.
pub fn split(query: &str) -> (String, Vec<String>) {
    let mut rest = Vec::new();
    let mut needles = Vec::new();
    for (raw, text, bare) in tokens(query) {
        if bare && text.get(..8).is_some_and(|k| k.eq_ignore_ascii_case("content:")) {
            let n = text[8..].trim();
            if !n.is_empty() {
                needles.push(n.to_string());
            }
        } else {
            rest.push(raw);
        }
    }
    (rest.join(" "), needles)
}

/// Splits on whitespace outside double quotes, like the query parser: each token as typed,
/// without its quotes, and whether any of it was outside quotes (a fully quoted token is a
/// phrase, never a filter).
fn tokens(s: &str) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let (mut raw, mut text) = (String::new(), String::new());
    let (mut inq, mut bare) = (false, false);
    for c in s.chars() {
        if c.is_whitespace() && !inq {
            if !raw.is_empty() {
                out.push((std::mem::take(&mut raw), std::mem::take(&mut text), bare));
            }
            bare = false;
            continue;
        }
        raw.push(c);
        if c == '"' {
            inq = !inq;
        } else {
            bare |= !inq;
            text.push(c);
        }
    }
    if !raw.is_empty() {
        out.push((raw, text, bare));
    }
    out
}

/// A file that passed the name and date filters, waiting to be read.
struct Candidate {
    path: String,
    size: u64,
    modified: Option<i64>,
}

/// Searches the files under the query's folder for every one of `needles`. `query` is the rest
/// of the search (already expanded by `filetypes::expand`); `scope` is the palette's folder.
/// Stops after `limit` files (then `more` is set). `cancellable`: [`cancel`] stops it.
pub fn search(query: &str, needles: &[String], scope: Option<&str>, limit: usize, cancellable: bool) -> SearchResults {
    let t0 = Instant::now();
    let generation = GENERATION.load(Ordering::Relaxed);
    let cancelled = || cancellable && GENERATION.load(Ordering::Relaxed) != generation;
    let done = |results: Vec<Hit>, more: bool, scanned: u64| SearchResults {
        total: results.len() as u64,
        results,
        took_us: t0.elapsed().as_micros() as u64,
        source: "content".into(),
        more,
        scanned,
        ..Default::default()
    };
    let q = query::parse(query);
    let res: Vec<Regex> = needles
        .iter()
        .filter_map(|n| RegexBuilder::new(&regex::escape(n)).case_insensitive(true).build().ok())
        .collect();
    let Some(root) = root_of(&q, scope) else { return done(Vec::new(), false, 0) };
    if res.is_empty() || q.kind == Some(Kind::Folder) {
        return done(Vec::new(), false, 0);
    }

    let walker = walkdir::WalkDir::new(crate::paths::for_io(&root))
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_type().is_dir() || dir_wanted(&q, e));
    let mut hits: Vec<Hit> = Vec::new();
    let mut batch: Vec<Candidate> = Vec::with_capacity(BATCH);
    let mut scanned = 0u64;
    let mut more = false;
    let mut read = |batch: &mut Vec<Candidate>, hits: &mut Vec<Hit>| {
        scanned += batch.len() as u64;
        let found: Vec<Option<LineMatch>> = batch.par_iter().map(|c| if cancelled() { None } else { scan(&c.path, &res) }).collect();
        for (c, m) in batch.drain(..).zip(found) {
            if let Some(m) = m {
                hits.push(hit(c, m));
            }
        }
    };
    let mut finished = true;
    for entry in walker {
        if cancelled() {
            return done(hits, true, scanned);
        }
        let Ok(e) = entry else { continue };
        if !e.file_type().is_file() {
            continue;
        }
        let Some(c) = candidate(&q, &e) else { continue };
        batch.push(c);
        if batch.len() == BATCH {
            read(&mut batch, &mut hits);
            if hits.len() >= limit {
                finished = false;
                break;
            }
        }
    }
    if finished {
        read(&mut batch, &mut hits);
    }
    if hits.len() > limit || !finished {
        more = true;
        hits.truncate(limit);
    }
    log::debug!("content search in {root}: {} found in {scanned} files read, {:?}", hits.len(), t0.elapsed());
    done(hits, more, scanned)
}

/// The folder to search: the deepest of the `in:` filters and the palette's folder, provided
/// it's inside all the others and exists. Never the whole computer.
fn root_of(q: &Query, scope: Option<&str>) -> Option<String> {
    let mut scopes: Vec<String> = q.scopes.iter().map(String::as_str).chain(scope).map(|s| crate::paths::normalize(s)).collect();
    scopes.retain(|s| !s.is_empty());
    for s in scopes.iter_mut() {
        *s = super::walk::long_path(s);
    }
    scopes.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let root = scopes.first()?.clone();
    let key = crate::paths::key(&root);
    let inside = |outer: &str| {
        let o = crate::paths::key(outer);
        key == o || (key.starts_with(&o) && (o.ends_with('\\') || key.as_bytes().get(o.len()) == Some(&b'\\')))
    };
    if !scopes.iter().all(|s| inside(s)) {
        return None;
    }
    std::path::Path::new(&crate::paths::for_io(&root)).is_dir().then_some(root)
}

/// A folder below the searched one: skipped when it's one of [`SKIP_DIRS`] or a `!` filter
/// leaves it out.
fn dir_wanted(q: &Query, e: &walkdir::DirEntry) -> bool {
    let name = e.file_name().to_string_lossy().to_lowercase();
    if SKIP_DIRS.contains(&name.as_str()) {
        return false;
    }
    let mut path = lower(&e.path().to_string_lossy());
    path.push(b'\\');
    !path_excluded(q, &path)
}

/// `!C:\Windows` (a folder and what's inside it) or `!node_modules\` (anywhere in the path).
fn path_excluded(q: &Query, path: &[u8]) -> bool {
    q.path_excludes.iter().any(|(pat, prefix)| {
        if *prefix {
            path.starts_with(pat) && (pat.ends_with(b"\\") || path.len() == pat.len() || path[pat.len()] == b'\\')
        } else {
            memchr::memmem::find(path, pat).is_some()
        }
    })
}

/// A file, if the rest of the query keeps it: words in its path, extensions, wildcards, `!`
/// excludes, size and date.
fn candidate(q: &Query, e: &walkdir::DirEntry) -> Option<Candidate> {
    let path = e.path().to_string_lossy().into_owned();
    let path_l = lower(&path);
    let name_l = lower(&e.file_name().to_string_lossy());
    let parent_l = &path_l[..path_l.len() - name_l.len()];
    if path_excluded(q, &path_l) || q.excludes.iter().any(|x| memchr::memmem::find(&name_l, x).is_some()) {
        return None;
    }
    let ext = name_l.iter().rposition(|&b| b == b'.').map(|i| &name_l[i + 1..]);
    if q.exts.is_empty() {
        if ext.is_some_and(|e| BINARY_EXTS.iter().any(|b| b.as_bytes() == e)) {
            return None;
        }
    } else if !ext.is_some_and(|e| q.exts.iter().any(|x| x.as_slice() == e)) {
        return None;
    }
    let terms_ok = q.terms.iter().all(|t| match t {
        Term::Sub(s) => memchr::memmem::find(&path_l, s).is_some(),
        Term::Glob(g) => query::glob_match(g, &name_l),
        Term::Path { dir, name } => memchr::memmem::find(&name_l, name).is_some() && memchr::memmem::find(parent_l, dir).is_some(),
    });
    if !terms_ok {
        return None;
    }
    let meta = e.metadata().ok()?;
    let size = meta.len();
    if size == 0 || size > MAX_BYTES || !q.size.iter().all(|&(c, v)| c.test(size, v)) {
        return None;
    }
    let secs = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    if !q.modified.is_empty() && !secs.is_some_and(|s| q.modified.iter().all(|&(c, v)| c.test(s, v))) {
        return None;
    }
    Some(Candidate { path, size, modified: secs.map(|s| s * 1000) })
}

fn hit(c: Candidate, m: LineMatch) -> Hit {
    let name = std::path::Path::new(&c.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Hit { path: c.path, name, is_dir: false, size: Some(c.size), modified: c.modified, score: 0.0, matches: Vec::new(), line: Some(m) }
}

/// The first line with the first needle, if the file is text and has every needle. Only the
/// start of a binary file is read.
fn scan(path: &str, res: &[Regex]) -> Option<LineMatch> {
    use std::io::Read;
    let mut f = std::fs::File::open(crate::paths::for_io(path)).ok()?;
    let mut bytes = Vec::new();
    (&mut f).take(SNIFF as u64).read_to_end(&mut bytes).ok()?;
    decode(&bytes)?;
    f.read_to_end(&mut bytes).ok()?;
    let text = decode(&bytes)?;
    let first = res[0].find(&text)?;
    if !res[1..].iter().all(|r| r.is_match(&text)) {
        return None;
    }
    let count = res[0].find_iter(&text).take(MAX_COUNT).count() as u32;
    Some(excerpt(&text, first.start(), first.end(), count))
}

/// The file as UTF-8 bytes to search, or None for a binary file.
fn decode(b: &[u8]) -> Option<Cow<'_, [u8]>> {
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Some(Cow::Borrowed(rest));
    }
    for (bom, enc) in [([0xFF, 0xFE], encoding_rs::UTF_16LE), ([0xFE, 0xFF], encoding_rs::UTF_16BE)] {
        if b.starts_with(&bom) {
            let (s, _, _) = enc.decode(b);
            return Some(Cow::Owned(s.into_owned().into_bytes()));
        }
    }
    if memchr::memchr(0, &b[..b.len().min(SNIFF)]).is_some() {
        return None;
    }
    Some(Cow::Borrowed(b))
}

/// The line around `start..end`: the whole line, or a piece of a long one with "…" where it
/// was cut; tabs and other control characters become spaces.
fn excerpt(text: &[u8], start: usize, end: usize, count: u32) -> LineMatch {
    let ls = memchr::memrchr(b'\n', &text[..start]).map_or(0, |i| i + 1);
    let le = memchr::memchr(b'\n', &text[end..]).map_or(text.len(), |i| end + i);
    let line = memchr::memchr_iter(b'\n', &text[..ls]).count() as u32 + 1;
    let (mut a, mut b) = (ls, le);
    if le - ls > EXCERPT {
        a = start.saturating_sub(EXCERPT / 3).max(ls);
        b = (a + EXCERPT).max(end).min(le);
    }
    // Whole characters only.
    while a > ls && (text[a] & 0xC0) == 0x80 {
        a -= 1;
    }
    while b < le && (text[b] & 0xC0) == 0x80 {
        b += 1;
    }
    let clean = |s: &[u8]| String::from_utf8_lossy(s).chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>();
    let mut before = clean(&text[a..start]);
    if a == ls {
        before = before.trim_start().to_string();
    } else {
        before.insert(0, '…');
    }
    let found = clean(&text[start..end]);
    let mut after = clean(&text[end..b]);
    if b == le {
        after = after.trim_end().to_string();
    } else {
        after.push('…');
    }
    let s = before.encode_utf16().count() as u32;
    let e = s + found.encode_utf16().count() as u32;
    LineMatch { line, text: before + &found + &after, start: s, end: e, count }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn takes_content_filters_out() {
        assert_eq!(split(r#"report content:"total due" ext:pdf"#), ("report ext:pdf".to_string(), vec!["total due".to_string()]));
        assert_eq!(split("Content:TODO in:C:\\dev"), ("in:C:\\dev".to_string(), vec!["TODO".to_string()]));
        // Being typed, quoted as a phrase, or several.
        assert_eq!(split("notes content:"), ("notes".to_string(), vec![]));
        assert_eq!(split(r#""content:x" a"#), (r#""content:x" a"#.to_string(), vec![]));
        assert_eq!(split("content:a content:b"), (String::new(), vec!["a".to_string(), "b".to_string()]));
        assert_eq!(split("plain words"), ("plain words".to_string(), vec![]));
    }

    #[test]
    fn excerpts_with_the_match_marked() {
        let t = "first\n\tlet total = 42; // Total\nlast".as_bytes();
        let at = memchr::memmem::find(t, b"total").unwrap();
        let m = excerpt(t, at, at + 5, 2);
        assert_eq!((m.line, m.text.as_str()), (2, "let total = 42; // Total"));
        assert_eq!(&m.text.encode_utf16().collect::<Vec<_>>()[m.start as usize..m.end as usize], "total".encode_utf16().collect::<Vec<_>>());
        // A long line is cut around the match, on whole characters, offsets in UTF-16.
        let long = format!("{}needle{}", "é😀".repeat(80), "x".repeat(300));
        let at = long.find("needle").unwrap();
        let m = excerpt(long.as_bytes(), at, at + 6, 1);
        assert!(m.text.starts_with('…') && m.text.ends_with('…'));
        assert_eq!(String::from_utf16(&m.text.encode_utf16().collect::<Vec<_>>()[m.start as usize..m.end as usize]).unwrap(), "needle");
    }

    #[test]
    fn reads_text_and_skips_binary() {
        assert!(decode(b"plain text").is_some());
        assert!(decode(b"MZ\0\0binary").is_none());
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("Grüße needle".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(&*decode(&utf16).unwrap(), "Grüße needle".as_bytes());
    }

    #[test]
    fn searches_a_folder_with_the_other_filters() {
        let dir = std::env::temp_dir().join(format!("ferret-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("node_modules").join("pkg")).unwrap();
        fs::create_dir_all(dir.join("skip")).unwrap();
        fs::write(dir.join("src").join("a.rs"), "fn main() {\n    // The Needle is here\n}\n").unwrap();
        fs::write(dir.join("src").join("b.txt"), "a NEEDLE and another needle").unwrap();
        fs::write(dir.join("src").join("c.txt"), "nothing to see").unwrap();
        fs::write(dir.join("bin.dat"), b"needle\0\0\0").unwrap();
        fs::write(dir.join("node_modules").join("pkg").join("x.js"), "needle").unwrap();
        fs::write(dir.join("skip").join("d.txt"), "needle").unwrap();
        let root = dir.to_string_lossy().into_owned();
        let names = |r: &SearchResults| r.results.iter().map(|h| h.name.clone()).collect::<Vec<_>>();
        let needle = vec!["needle".to_string()];

        let r = search("", &needle, Some(&root), 50, false);
        assert_eq!(names(&r), ["d.txt", "a.rs", "b.txt"], "binary files and node_modules are skipped");
        let b = r.results.iter().find(|h| h.name == "b.txt").unwrap().line.as_ref().unwrap();
        assert_eq!((b.line, b.count), (1, 2));
        assert_eq!(names(&search("ext:rs", &needle, Some(&root), 50, false)), ["a.rs"]);
        assert_eq!(names(&search(r"!skip\", &needle, Some(&root), 50, false)), ["a.rs", "b.txt"]);
        assert_eq!(names(&search("", &["needle".into(), "another".into()], Some(&root), 50, false)), ["b.txt"], "every text must be in the file");
        let one = search("", &needle, Some(&root), 1, false);
        assert!(one.more && one.results.len() == 1);
        // In node_modules itself, it's searched.
        let inside = dir.join("node_modules").to_string_lossy().into_owned();
        assert_eq!(names(&search("", &needle, Some(&inside), 50, false)), ["x.js"]);
        // No folder: nothing (never the whole computer).
        assert!(search("", &needle, None, 50, false).results.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
