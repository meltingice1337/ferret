//! Counts folder sizes inside WSL, for Ferret.
//!
//! Ferret puts it in each distro it counts in (~/.cache/ferret) and runs it there to size the
//! folders it shows: `ferret-wsl-size [--watch-stdin] [--roots-stdin] [--threads N]
//! [--keep BYTES FILES] [FOLDER...]`. It's linked statically (musl), so it runs in any distro,
//! whatever its libc. It counts what `find FOLDER -xdev -type f` finds: regular files, by their
//! size, without following symlinks or entering other file systems mounted inside (as Ferret's find
//! script, which it replaces). But it reads many folders at once: when they aren't in Linux's
//! cache, waiting on the disk for one folder after another is most of a count.
//!
//! On stdin, one full path a line: with `--roots-stdin`, the folders to count first, up to an empty
//! line (any number: a command line has room for a few hundred); then the folders whose sizes
//! Ferret knows already, up to an empty line, which are skipped (Ferret adds their sizes itself).
//! With `--watch-stdin` it ends when stdin closes after that, so a count Ferret stops (by ending
//! `wsl.exe`) ends here too.
//!
//! It writes one line for each result, tab-separated:
//!   H  1                              started (the version of this format)
//!   S  bytes  files  FOLDER  inside   a folder inside FOLDER, once it's done, if worth keeping:
//!                                     two levels down at most, or BYTES / FILES and up
//!   D  bytes  files  FOLDER           FOLDER, once it's done (in the order they finish)
//!   P  bytes  files  FOLDER           FOLDER so far, every 250 ms while it changes
//!   E  path                           couldn't be read: the sizes of the folders containing it
//!                                     are short of it

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// A folder being counted.
struct Folder {
    parent: Option<Arc<Folder>>,
    root: usize,
    path: Vec<u8>,
    /// Levels below its root (the root is 0).
    depth: u32,
    bytes: AtomicU64,
    files: AtomicU64,
    /// Its own listing, and each folder inside that isn't done yet.
    pending: AtomicUsize,
}

/// A folder Ferret asked for.
struct Root {
    /// As Ferret gave it: the name it's reported by.
    name: String,
    /// How much of the path of a folder inside it is the root's own (and the slash after it).
    prefix: usize,
    /// The file system it's on: folders on any other aren't entered.
    dev: u64,
    bytes: AtomicU64,
    files: AtomicU64,
    done: AtomicBool,
}

struct Count {
    roots: Vec<Root>,
    skip: HashSet<Vec<u8>>,
    keep_bytes: u64,
    keep_files: u64,
    /// Folders for any worker. Each keeps the ones it finds to itself (no locking, and each
    /// folder is done soon after it's started), and shares half when others wait for work.
    queue: Mutex<Vec<Arc<Folder>>>,
    more: Condvar,
    /// Every root is done (with the queue's lock).
    ended: Condvar,
    /// Workers waiting for work.
    idle: AtomicUsize,
    /// Roots not done.
    left: AtomicUsize,
    out: Mutex<BufWriter<io::Stdout>>,
}

/// Room for a folder's names, read many at a time.
const LISTING: usize = 64 * 1024;

impl Count {
    fn write(&self, line: &[u8], flush: bool) {
        let mut out = self.out.lock().unwrap();
        // Nobody reading any more (Ferret stopped the count): nothing left to do.
        if out.write_all(line).is_err() || (flush && out.flush().is_err()) {
            std::process::exit(1);
        }
    }

    fn complain(&self, path: &[u8]) {
        let mut line = b"E\t".to_vec();
        line.extend_from_slice(path);
        line.push(b'\n');
        self.write(&line, false);
    }

    /// Reads one folder: adds up the files in it, and keeps the folders in it for later.
    fn read(&self, folder: &Arc<Folder>, listing: &mut [u8], later: &mut Vec<Arc<Folder>>) {
        let (mut bytes, mut files) = (0u64, 0u64);
        let mut path = folder.path.clone();
        path.push(0);
        // SAFETY: a NUL-terminated path; the descriptor is closed below.
        let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 {
            self.complain(&folder.path);
        } else {
            'listing: loop {
                // SAFETY: the buffer is LISTING bytes, and the kernel writes at most that.
                let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, listing.as_mut_ptr(), listing.len()) };
                if n <= 0 {
                    if n < 0 {
                        self.complain(&folder.path);
                    }
                    break;
                }
                let mut at = 0usize;
                while at < n as usize {
                    // struct linux_dirent64 { u64 d_ino; i64 d_off; u16 d_reclen; u8 d_type; char d_name[]; }
                    let len = u16::from_ne_bytes([listing[at + 16], listing[at + 17]]) as usize;
                    if len == 0 {
                        break 'listing;
                    }
                    let kind = listing[at + 18];
                    let entry = &listing[at + 19..at + len];
                    let name = &entry[..entry.iter().position(|&b| b == 0).unwrap_or(entry.len())];
                    at += len;
                    if name == b"." || name == b".." || !matches!(kind, libc::DT_REG | libc::DT_DIR | libc::DT_UNKNOWN) {
                        continue;
                    }
                    // SAFETY: `name` ends in the NUL the kernel put after it, inside the buffer.
                    let mut st: libc::stat = unsafe { std::mem::zeroed() };
                    let ok = unsafe { libc::fstatat(fd, name.as_ptr().cast(), &mut st, libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT) } == 0;
                    if !ok {
                        // Gone since it was listed: nothing missing.
                        if io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
                            self.complain(&join(&folder.path, name));
                        }
                        continue;
                    }
                    match st.st_mode & libc::S_IFMT {
                        libc::S_IFREG => {
                            bytes += st.st_size as u64;
                            files += 1;
                        }
                        libc::S_IFDIR => {
                            let path = join(&folder.path, name);
                            if self.skip.contains(&path) {
                                continue;
                            }
                            let inside = Folder {
                                parent: Some(folder.clone()),
                                root: folder.root,
                                path,
                                depth: folder.depth + 1,
                                bytes: AtomicU64::new(0),
                                files: AtomicU64::new(0),
                                pending: AtomicUsize::new(1),
                            };
                            if st.st_dev != self.roots[folder.root].dev {
                                // Another file system (a Windows drive, /proc): listed, not entered.
                                self.inside_done(&inside, 0, 0);
                                continue;
                            }
                            folder.pending.fetch_add(1, Ordering::Relaxed);
                            later.push(Arc::new(inside));
                        }
                        _ => {}
                    }
                }
            }
            // SAFETY: opened above.
            unsafe { libc::close(fd) };
        }
        let root = &self.roots[folder.root];
        root.bytes.fetch_add(bytes, Ordering::Relaxed);
        root.files.fetch_add(files, Ordering::Relaxed);
        folder.bytes.fetch_add(bytes, Ordering::Relaxed);
        folder.files.fetch_add(files, Ordering::Relaxed);
        self.release(folder.clone());
    }

    /// One part of a folder is done (its listing, or a folder inside): once all are, the folder
    /// is done, and adds to the one containing it.
    fn release(&self, mut folder: Arc<Folder>) {
        loop {
            if folder.pending.fetch_sub(1, Ordering::AcqRel) != 1 {
                return;
            }
            let bytes = folder.bytes.load(Ordering::Relaxed);
            let files = folder.files.load(Ordering::Relaxed);
            let Some(parent) = folder.parent.clone() else {
                self.root_done(folder.root, bytes, files);
                return;
            };
            self.inside_done(&folder, bytes, files);
            parent.bytes.fetch_add(bytes, Ordering::Relaxed);
            parent.files.fetch_add(files, Ordering::Relaxed);
            folder = parent;
        }
    }

    fn inside_done(&self, folder: &Folder, bytes: u64, files: u64) {
        if folder.depth > 2 && bytes < self.keep_bytes && files < self.keep_files {
            return;
        }
        let root = &self.roots[folder.root];
        // Ferret couldn't read a name that isn't UTF-8 or holds a line break back: it's counted
        // in the folders containing it all the same.
        let Ok(rel) = std::str::from_utf8(&folder.path[root.prefix..]) else { return };
        if rel.contains('\n') {
            return;
        }
        let line = format!("S\t{bytes}\t{files}\t{}\t{rel}\n", root.name);
        // The folders right inside a root are flushed at once (as find's script did): a count
        // stopped later keeps them.
        self.write(line.as_bytes(), folder.depth == 1);
    }

    fn root_done(&self, root: usize, bytes: u64, files: u64) {
        let r = &self.roots[root];
        r.done.store(true, Ordering::Relaxed);
        self.write(format!("D\t{bytes}\t{files}\t{}\n", r.name).as_bytes(), true);
        if self.left.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _queue = self.queue.lock().unwrap();
            self.more.notify_all();
            self.ended.notify_all();
        }
    }

    /// A folder from the shared queue, waiting for one; none once every root is done.
    fn take(&self) -> Option<Arc<Folder>> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if let Some(f) = queue.pop() {
                return Some(f);
            }
            if self.left.load(Ordering::Acquire) == 0 {
                return None;
            }
            self.idle.fetch_add(1, Ordering::Relaxed);
            queue = self.more.wait(queue).unwrap();
            self.idle.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn work(&self) {
        let mut listing = vec![0u8; LISTING];
        let mut mine: Vec<Arc<Folder>> = Vec::new();
        loop {
            let Some(folder) = mine.pop().or_else(|| self.take()) else { return };
            self.read(&folder, &mut listing, &mut mine);
            let idle = self.idle.load(Ordering::Relaxed);
            if idle > 0 && mine.len() > 1 {
                // The ones found first: higher up, so likely more in them.
                let give: Vec<_> = mine.drain(..mine.len() / 2).collect();
                let wake = give.len().min(idle);
                self.queue.lock().unwrap().extend(give);
                for _ in 0..wake {
                    self.more.notify_one();
                }
            }
        }
    }
}

/// A root's path for joining (no trailing slash, unless it's `/`).
fn folder_path(name: &str) -> &[u8] {
    let t = name.trim_end_matches('/');
    if t.is_empty() { b"/" } else { t.as_bytes() }
}

/// How much of a path inside this folder is the folder (and the slash after it).
fn prefix_len(folder: &[u8]) -> usize {
    if folder == b"/" { 1 } else { folder.len() + 1 }
}

fn join(folder: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(folder.len() + 1 + name.len());
    p.extend_from_slice(folder);
    if folder != b"/" {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

mod grep;

fn main() {
    // `grep`: searching inside files instead (grep.rs).
    if std::env::args().nth(1).as_deref() == Some("grep") {
        return grep::main(std::env::args().skip(2));
    }
    let mut watch_stdin = false;
    let mut roots_stdin = false;
    let mut threads = 0usize;
    let (mut keep_bytes, mut keep_files) = (1u64 << 20, 1000u64);
    let mut names: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    let mut options = true;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--watch-stdin" if options => watch_stdin = true,
            "--roots-stdin" if options => roots_stdin = true,
            "--threads" if options => threads = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            "--keep" if options => {
                keep_bytes = args.next().and_then(|n| n.parse().ok()).unwrap_or(keep_bytes);
                keep_files = args.next().and_then(|n| n.parse().ok()).unwrap_or(keep_files);
            }
            "--" if options => options = false,
            _ => names.push(a),
        }
    }
    // However many processors: most of the time each one waits on the disk. With the cache
    // cleared, 1M files took 3.7 s with 8, 2.1 s with 16, 1.6 s with 32, 1.3 s with 64, and 128
    // were no faster.
    if threads == 0 {
        threads = 64;
    }

    let mut out = BufWriter::with_capacity(64 * 1024, io::stdout());
    if out.write_all(b"H\t1\n").and_then(|_| out.flush()).is_err() {
        std::process::exit(1);
    }

    // Paths up to an empty line (or the end).
    let mut stdin = io::stdin().lock();
    let mut list = || {
        let mut paths = Vec::new();
        let mut line = Vec::new();
        while stdin.read_until(b'\n', &mut line).unwrap_or(0) > 0 {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.is_empty() {
                break;
            }
            paths.push(std::mem::take(&mut line));
        }
        paths
    };
    if roots_stdin {
        // (Ferret sends UTF-8 only: it shows these names.)
        names.extend(list().into_iter().filter_map(|p| String::from_utf8(p).ok()));
    }
    let skip: HashSet<Vec<u8>> = list().into_iter().collect();
    drop(stdin);
    if watch_stdin {
        std::thread::spawn(|| {
            let mut rest = [0u8; 256];
            while io::stdin().read(&mut rest).is_ok_and(|n| n > 0) {}
            std::process::exit(0);
        });
    }

    // The roots, each name once (Ferret matches the results by name). One that isn't there can't
    // be read; one that isn't a folder (a file, a symlink) has nothing in it.
    let mut roots = Vec::new();
    let mut first = Vec::new();
    let mut empty = Vec::new();
    let mut missing = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            continue;
        }
        let path = folder_path(&name).to_vec();
        let Ok(meta) = fs::symlink_metadata(OsStr::from_bytes(&path)) else {
            missing.push(path);
            continue;
        };
        let i = roots.len();
        let prefix = prefix_len(&path);
        if meta.is_dir() {
            first.push(Arc::new(Folder {
                parent: None,
                root: i,
                path,
                depth: 0,
                bytes: AtomicU64::new(0),
                files: AtomicU64::new(0),
                pending: AtomicUsize::new(1),
            }));
        } else {
            empty.push(i);
        }
        roots.push(Root {
            name,
            prefix,
            dev: meta.dev(),
            bytes: AtomicU64::new(0),
            files: AtomicU64::new(0),
            done: AtomicBool::new(false),
        });
    }
    let count = Arc::new(Count {
        left: AtomicUsize::new(roots.len()),
        roots,
        skip,
        keep_bytes,
        keep_files,
        queue: Mutex::new(first),
        more: Condvar::new(),
        ended: Condvar::new(),
        idle: AtomicUsize::new(0),
        out: Mutex::new(out),
    });
    for path in missing {
        count.complain(&path);
    }
    for i in empty {
        count.root_done(i, 0, 0);
    }

    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let c = count.clone();
            std::thread::spawn(move || c.work())
        })
        .collect();
    // Progress, and whatever waits in the buffer, every 250 ms until the end.
    let mut last: Vec<u64> = vec![u64::MAX; count.roots.len()];
    loop {
        {
            let queue = count.queue.lock().unwrap();
            if count.left.load(Ordering::Acquire) == 0 {
                break;
            }
            let _ = count.ended.wait_timeout(queue, Duration::from_millis(250)).unwrap();
        }
        let mut lines = String::new();
        for (i, r) in count.roots.iter().enumerate() {
            let files = r.files.load(Ordering::Relaxed);
            if r.done.load(Ordering::Relaxed) || files == last[i] {
                continue;
            }
            last[i] = files;
            lines.push_str(&format!("P\t{}\t{files}\t{}\n", r.bytes.load(Ordering::Relaxed), r.name));
        }
        count.write(lines.as_bytes(), true);
    }
    for w in workers {
        let _ = w.join();
    }
    count.write(b"", true);
}
