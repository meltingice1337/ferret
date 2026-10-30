//! `ferret-wsl-size grep ROOT [--max FILES] [--threads N] [--ext a,b | --skip-ext a,b] -- TEXT...`
//!
//! Searches inside the files under ROOT for every TEXT (ignoring ASCII case), for Ferret's
//! `content:` searches in WSL and on servers: Linux reads the files, and only the results travel.
//! As Ferret's own search inside files: folders named .git and node_modules aren't entered,
//! symlinks aren't followed, other file systems mounted inside aren't entered, files over 64 MB
//! and binary ones (a NUL byte near the start) are skipped, and `--ext` keeps only those
//! extensions (`--skip-ext`: all but those). Files are taken in name order, folder by folder, and
//! it stops once FILES have matched.
//!
//! Output, tab-separated (names with a tab or line break in them are skipped):
//!   H  1                                                      started
//!   M  size  mtime  line  count  start  end  path  text      a file with every TEXT: its size, modified
//!                                                           (Unix seconds), the first line with the
//!                                                           first TEXT (1-based), how many times it's
//!                                                           in the file (up to 9,999), where in `text`
//!                                                           (bytes), the path under ROOT, and that line
//!                                                           (at most 4 KB around the match; tabs and
//!                                                           other control characters as spaces)
//!   E  path                                                  couldn't be read

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;

const MAX_BYTES: u64 = 64 << 20;
const SNIFF: usize = 8 << 10;
const MAX_COUNT: usize = 9_999;
const LINE_MAX: usize = 4096;
const BATCH: usize = 256;
const SKIP_DIRS: [&[u8]; 2] = [b".git", b"node_modules"];

struct Options {
    root: Vec<u8>,
    max: usize,
    threads: usize,
    exts: Vec<Vec<u8>>,
    skip_exts: Vec<Vec<u8>>,
    needles: Vec<Vec<u8>>,
}

fn parse(args: impl Iterator<Item = String>) -> Option<Options> {
    let mut o = Options { root: Vec::new(), max: 60, threads: 16, exts: vec![], skip_exts: vec![], needles: vec![] };
    let list = |s: Option<String>| -> Vec<Vec<u8>> {
        s.unwrap_or_default().split(',').filter(|e| !e.is_empty()).map(|e| e.to_ascii_lowercase().into_bytes()).collect()
    };
    let mut args = args.peekable();
    let mut texts = false;
    while let Some(a) = args.next() {
        if texts {
            o.needles.push(a.to_ascii_lowercase().into_bytes());
            continue;
        }
        match a.as_str() {
            "--max" => o.max = args.next()?.parse().ok()?,
            "--threads" => o.threads = args.next()?.parse().ok()?,
            "--ext" => o.exts = list(args.next()),
            "--skip-ext" => o.skip_exts = list(args.next()),
            "--" => texts = true,
            _ if o.root.is_empty() => o.root = a.into_bytes(),
            _ => return None,
        }
    }
    (!o.root.is_empty() && !o.needles.is_empty()).then_some(o)
}

pub fn main(args: impl Iterator<Item = String>) {
    let Some(o) = parse(args) else {
        eprintln!("usage: ferret-wsl-size grep ROOT [--max FILES] [--threads N] [--ext a,b | --skip-ext a,b] -- TEXT...");
        std::process::exit(2);
    };
    let mut out = BufWriter::with_capacity(64 * 1024, io::stdout());
    if out.write_all(b"H\t1\n").and_then(|_| out.flush()).is_err() {
        std::process::exit(1);
    }
    let root = o.root.strip_suffix(b"/").filter(|r| !r.is_empty()).unwrap_or(&o.root).to_vec();
    let Ok(meta) = fs::metadata(OsStr::from_bytes(&root)) else {
        let _ = writeln!(out, "E\t{}", String::from_utf8_lossy(&root));
        return;
    };
    let mut w = Walk { o: &o, root: &root, dev: meta.dev(), found: 0, batch: Vec::with_capacity(BATCH), out: &mut out };
    if w.dir(&root) && w.found < o.max {
        let room = o.max - w.found;
        read(&o, &root, &mut w.batch, w.out, room);
    }
    let _ = out.flush();
}

/// Depth first, in name order with folders among the files (as Ferret walks its own folders),
/// files read a batch at a time.
struct Walk<'a, W: Write> {
    o: &'a Options,
    root: &'a [u8],
    dev: u64,
    found: usize,
    batch: Vec<(Vec<u8>, u64, i64)>,
    out: &'a mut W,
}

impl<W: Write> Walk<'_, W> {
    /// False once enough files matched.
    fn dir(&mut self, dir: &[u8]) -> bool {
        let Ok(rd) = fs::read_dir(OsStr::from_bytes(dir)) else {
            let _ = writeln!(self.out, "E\t{}", String::from_utf8_lossy(dir));
            return true;
        };
        let mut items: Vec<(Vec<u8>, fs::Metadata)> = rd
            .flatten()
            .filter_map(|e| Some((e.file_name().as_bytes().to_vec(), e.metadata().ok()?)))
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, m) in items {
            if name.iter().any(|&b| b == b'\t' || b == b'\n') {
                continue;
            }
            let mut path = dir.to_vec();
            if path != b"/" {
                path.push(b'/');
            }
            path.extend_from_slice(&name);
            let ft = m.file_type();
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_slice()) && m.dev() == self.dev && !self.dir(&path) {
                    return false;
                }
            } else if ft.is_file() && m.len() > 0 && m.len() <= MAX_BYTES && wanted(self.o, &name) {
                self.batch.push((path, m.len(), m.mtime()));
                if self.batch.len() == BATCH {
                    let room = self.o.max - self.found;
                    self.found += read(self.o, self.root, &mut self.batch, self.out, room);
                    if self.found >= self.o.max {
                        return false;
                    }
                }
            }
        }
        true
    }
}

/// Its extension is one asked for (or not one left out).
fn wanted(o: &Options, name: &[u8]) -> bool {
    let ext = name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0).map(|i| name[i + 1..].to_ascii_lowercase());
    if !o.exts.is_empty() {
        return ext.is_some_and(|e| o.exts.contains(&e));
    }
    !ext.is_some_and(|e| o.skip_exts.contains(&e))
}

/// Reads a batch of files in parallel and prints the matches, in order (at most `room`); how
/// many matched.
fn read(o: &Options, root: &[u8], batch: &mut Vec<(Vec<u8>, u64, i64)>, out: &mut impl Write, room: usize) -> usize {
    let chunk = batch.len().div_ceil(o.threads.max(1)).max(1);
    let results: Vec<Option<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = batch.chunks(chunk).map(|part| s.spawn(move || part.iter().map(|f| scan(o, root, f)).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    batch.clear();
    let mut n = 0;
    for line in results.into_iter().flatten() {
        if n == room {
            break;
        }
        let _ = out.write_all(line.as_bytes());
        n += 1;
    }
    let _ = out.flush();
    n
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn scan(o: &Options, root: &[u8], (path, size, mtime): &(Vec<u8>, u64, i64)) -> Option<String> {
    let mut f = fs::File::open(OsStr::from_bytes(path)).ok()?;
    let mut bytes = Vec::with_capacity(*size as usize);
    (&mut f).take(SNIFF as u64).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) || bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return None;
    }
    f.read_to_end(&mut bytes).ok()?;
    let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let lower = text.to_ascii_lowercase();
    let first = find(&lower, &o.needles[0])?;
    if !o.needles[1..].iter().all(|n| find(&lower, n).is_some()) {
        return None;
    }
    let n0 = &o.needles[0];
    let mut count = 0;
    let mut at = 0;
    while count < MAX_COUNT {
        match find(&lower[at..], n0) {
            Some(i) => {
                count += 1;
                at += i + n0.len().max(1);
            }
            None => break,
        }
    }
    // The line with the first match (a window of it when it's long), control characters as spaces.
    let ls = text[..first].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let le = text[first..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| first + i);
    let line_no = text[..ls].iter().filter(|&&b| b == b'\n').count() + 1;
    let (mut a, mut b) = (ls, le);
    if le - ls > LINE_MAX {
        a = first.saturating_sub(LINE_MAX / 4).max(ls);
        b = (a + LINE_MAX).min(le);
    }
    while a > ls && (text[a] & 0xC0) == 0x80 {
        a -= 1;
    }
    while b < le && (text[b] & 0xC0) == 0x80 {
        b += 1;
    }
    let shown: Vec<u8> = text[a..b].iter().map(|&c| if c < 32 || c == 127 { b' ' } else { c }).collect();
    let rel = &path[root.len()..].strip_prefix(b"/").unwrap_or(&path[root.len()..]);
    Some(format!(
        "M\t{size}\t{mtime}\t{line_no}\t{count}\t{}\t{}\t{}\t{}\n",
        first - a,
        first - a + n0.len(),
        String::from_utf8_lossy(rel),
        String::from_utf8_lossy(&shown)
    ))
}
