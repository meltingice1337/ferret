//! WSL distros in search. The indexer service only reads NTFS drives, so the app lists each
//! running distro itself with one native `find` (fast inside Linux, unlike walking
//! `\\wsl.localhost` from Windows) and lists it again every 15 minutes while it runs. A
//! distro that isn't running is never started just to be indexed; its last listing stays
//! searchable.

use super::engine::{Engine, Volume};
use super::index::{flags, Index};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const RESCAN_EVERY: Duration = Duration::from_secs(15 * 60);
/// How often a search may check which distros are running (starting `wsl.exe` costs ~100 ms).
const CHECK_EVERY: Duration = Duration::from_secs(60);

#[derive(Default)]
struct State {
    last_check: Option<Instant>,
    scanned: HashMap<String, Instant>,
    busy: HashSet<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}

/// Indexes of the WSL distros (searched next to the drives).
pub fn engine() -> &'static Arc<Engine> {
    static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
    ENGINE.get_or_init(Engine::new)
}

/// At most once a minute: (re)lists the running distros that are due, in the background.
pub fn refresh() {
    {
        let mut st = state().lock();
        if st.last_check.is_some_and(|t| t.elapsed() < CHECK_EVERY) {
            return;
        }
        st.last_check = Some(Instant::now());
    }
    std::thread::spawn(|| {
        for distro in crate::wsl::running() {
            {
                let mut st = state().lock();
                let due = st.scanned.get(&distro).map_or(true, |t| t.elapsed() >= RESCAN_EVERY);
                if !due || !st.busy.insert(distro.clone()) {
                    continue;
                }
            }
            let root = crate::paths::wsl_root(&distro);
            // First listing: an empty, not-ready volume makes searches say "still indexing".
            if !engine().volumes().iter().any(|v| v.root.eq_ignore_ascii_case(&root)) {
                engine().add_volume(Volume::new(Index::new(&root, true), false));
            }
            let listed = scan(&distro);
            let mut st = state().lock();
            st.busy.remove(&distro);
            // A failed listing (no GNU find, say) isn't retried every minute either.
            st.scanned.insert(distro.clone(), Instant::now());
            drop(st);
            match listed {
                Some(ix) => engine().add_volume(Volume::new(ix, true)),
                None => {
                    // Drop the "still indexing" placeholder; an earlier listing (if any) stays.
                    let placeholder = |v: &Arc<Volume>| v.root.eq_ignore_ascii_case(&root) && !v.ready.load(Relaxed);
                    engine().set_volumes(engine().volumes().into_iter().filter(|v| !placeholder(v)).collect());
                }
            }
        }
    });
}

/// Where people keep their own files. System folders (/usr, /var…) would multiply the index
/// for little use; they can still be browsed.
const ROOTS: &[&str] = &["/home", "/root", "/etc", "/opt", "/srv", "/usr/local"];

/// Dependency, cache and tool folders: listed themselves, but their contents (often hundreds
/// of thousands of files) aren't indexed.
const SKIP: &[&str] = &[
    "node_modules", ".git", ".cache", "__pycache__", ".venv", "venv", ".npm", ".pnpm-store", ".yarn", ".cargo",
    ".rustup", ".nvm", ".gradle", ".m2", ".pyenv", ".rbenv", ".docker", "snap", ".vscode-server", ".cursor-server",
];

/// The distro's own files (see ROOTS and SKIP), staying on its file system so mounted
/// Windows drives aren't walked, as the distro's default user (unreadable folders are left out).
fn scan(distro: &str) -> Option<Index> {
    const FMT: &str = r"%y\t%s\t%T@\t%p\0";
    let mut args: Vec<&str> = ROOTS.to_vec();
    args.extend(["-xdev", "("]);
    for (i, name) in SKIP.iter().enumerate() {
        if i > 0 {
            args.push("-o");
        }
        args.extend(["-name", name]);
    }
    args.extend(["-o", "-path", "*/.local/share", ")", "-prune", "-printf", FMT, "-o", "-printf", FMT]);
    let mut child = crate::wsl::command(distro, "nice")
        .args(["-n", "19", "find"])
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut ix = Index::with_capacity(&crate::paths::wsl_root(distro), true, 200_000, 8 << 20);
    // Folder (path without the leading "/") → entry; `find` lists a folder before its contents,
    // and the roots' parents (/, /usr) are added as needed.
    let mut dirs: HashMap<String, u32> = HashMap::from([(String::new(), 0)]);
    let mut reader = BufReader::with_capacity(1 << 20, child.stdout.take()?);
    let mut rec = Vec::new();
    while reader.read_until(0, &mut rec).ok()? > 0 {
        if rec.last() == Some(&0) {
            rec.pop();
        }
        if let Some((kind, size, mtime, rel)) = parse(&rec) {
            // Linux allows backslashes in names; Windows paths can't show them.
            let (parent_rel, name) = rel.rsplit_once('/').unwrap_or(("", rel));
            if parent_rel == "usr" && !dirs.contains_key("usr") {
                let usr = ix.push_unlinked(0, "usr", flags::DIR, 0, 0);
                dirs.insert("usr".into(), usr);
            }
            if let (Some(&parent), false) = (dirs.get(parent_rel), name.contains('\\')) {
                let mut fl = match kind {
                    'd' => flags::DIR,
                    'l' => flags::LINK,
                    _ => 0,
                };
                if name.starts_with('.') {
                    fl |= flags::HIDDEN;
                }
                let idx = ix.push_unlinked(parent, name, fl, if kind == 'd' { 0 } else { size }, mtime);
                if kind == 'd' {
                    dirs.insert(rel.to_string(), idx);
                }
            }
        }
        rec.clear();
    }
    let ok = child.wait().map(|s| s.success() || ix.live() > 1).unwrap_or(false);
    if !ok || ix.live() <= 1 {
        return None;
    }
    ix.relink();
    ix.shrink_to_fit();
    Some(ix)
}

/// One `find -printf '%y\t%s\t%T@\t%p'` record: kind, size, modified (Unix seconds), path
/// without the leading `/`.
fn parse(rec: &[u8]) -> Option<(char, u64, u32, &str)> {
    let s = std::str::from_utf8(rec).ok()?;
    let mut it = s.splitn(4, '\t');
    let kind = it.next()?.chars().next()?;
    let size = it.next()?.parse().unwrap_or(0);
    let mtime = it.next()?.split('.').next()?.parse::<u64>().unwrap_or(0).min(u32::MAX as u64) as u32;
    let rel = it.next()?.trim_start_matches('/');
    (!rel.is_empty()).then_some((kind, size, mtime, rel))
}

#[cfg(test)]
mod tests {
    /// Lists the first running distro for real: `cargo test --lib real_distro -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_distro() {
        let Some(distro) = crate::wsl::running().into_iter().next() else { return };
        let t = std::time::Instant::now();
        let ix = super::scan(&distro).expect("listing");
        let found = ix.resolve(&format!("{}\\home", crate::paths::wsl_root(&distro)));
        println!("{distro}: {} entries in {:?}, /home resolves: {}", ix.live(), t.elapsed(), found.is_some());
        assert!(ix.live() > 100 && found.is_some());
    }

    #[test]
    fn parses_find_records() {
        assert_eq!(super::parse(b"f\t1234\t1728912345.5\t/home/me/notes.txt"), Some(('f', 1234, 1728912345, "home/me/notes.txt")));
        assert_eq!(super::parse(b"d\t4096\t1728912345.0000000000\t/home"), Some(('d', 4096, 1728912345, "home")));
        assert_eq!(super::parse(b"d\t4096\t1728912345.0\t/"), None);
        assert_eq!(super::parse(b"garbage"), None);
    }
}
