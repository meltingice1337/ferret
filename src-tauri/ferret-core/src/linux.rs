//! Linux systems Ferret runs its programs on: WSL distros (through `wsl.exe`) and SSH servers
//! (through a command channel of an open session). Folder sizes and search drive both the same
//! way: the same helper, the same scripts, the same output.
//!
//! The helper (src-tauri/wsl-size, built for x86_64 and ARM64 and embedded with the app) is put
//! in the user's cache folder there the first time it's needed, as VS Code puts its server in
//! ~/.vscode-server, and checked against its SHA-256 before each run.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::paths;
use crate::remote::ssh;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    Wsl(String),
    Ssh(String),
}

impl Host {
    /// The system a Ferret path is on, and the path there.
    pub fn of(path: &str) -> Option<(Host, String)> {
        if let Some((id, linux)) = paths::remote_parts(&paths::normalize(path)) {
            return Some((Host::Ssh(id.to_string()), linux));
        }
        crate::wsl::linux_path(path).map(|(d, l)| (Host::Wsl(d), l))
    }

    /// The Ferret path of `linux` (an absolute path) on this system.
    pub fn path(&self, linux: &str) -> String {
        match self {
            Host::Wsl(d) => {
                let inside = linux.trim_matches('/');
                if inside.is_empty() { paths::wsl_root(d) } else { format!("{}\\{}", paths::wsl_root(d), inside.replace('/', "\\")) }
            }
            Host::Ssh(id) => paths::remote_path(id, linux),
        }
    }

    /// For logs.
    pub fn label(&self) -> String {
        match self {
            Host::Wsl(d) => format!("WSL ({d})"),
            Host::Ssh(id) => crate::remote::name_of(id),
        }
    }

    /// Up right now: a running distro, an open session. Background work never starts either.
    pub fn running(&self) -> bool {
        match self {
            Host::Wsl(d) => distro_running(d),
            Host::Ssh(id) => ssh::connected(id).is_some(),
        }
    }

    /// The machine type, to pick the helper's build.
    fn arch(&self) -> Option<String> {
        match self {
            Host::Wsl(_) => Some(std::env::consts::ARCH.to_string()),
            Host::Ssh(id) => ssh::connected(id)?.arch.clone(),
        }
    }

    /// Starts `program` with `args` (no shell in WSL; the login shell runs the quoted words over
    /// SSH, as `ssh host 'cmd' 'arg'` does).
    pub fn spawn(&self, program: &str, args: &[&str]) -> std::io::Result<Child> {
        match self {
            Host::Wsl(d) => {
                let mut c = crate::wsl::command(d, program);
                c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
                let mut p = c.spawn()?;
                Ok(Child {
                    stdin: p.stdin.take().map(|s| Box::new(s) as Box<dyn Write + Send>),
                    stdout: p.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    stderr: p.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    inner: Inner::Process(p),
                })
            }
            Host::Ssh(id) => {
                let s = ssh::connected(id).ok_or_else(|| std::io::Error::other("not connected"))?;
                let line: Vec<String> = std::iter::once(program).chain(args.iter().copied()).map(ssh::quote).collect();
                let mut p = crate::remote::block(s.exec(&line.join(" "))).map_err(std::io::Error::other)?;
                Ok(Child {
                    stdin: p.stdin.take().map(|s| Box::new(s) as Box<dyn Write + Send>),
                    stdout: p.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    stderr: p.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    inner: Inner::Ssh(p),
                })
            }
        }
    }
}

/// A running Linux program, wherever it runs.
pub struct Child {
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    pub stderr: Option<Box<dyn Read + Send>>,
    inner: Inner,
}

enum Inner {
    Process(std::process::Child),
    Ssh(ssh::Proc),
}

impl Child {
    pub fn kill(&mut self) {
        match &mut self.inner {
            Inner::Process(p) => {
                let _ = p.kill();
            }
            Inner::Ssh(p) => p.kill(),
        }
    }

    /// Its exit code (None: ended without one).
    pub fn wait(&mut self) -> Option<i32> {
        self.stdin = None;
        match &mut self.inner {
            Inner::Process(p) => p.wait().ok().and_then(|s| s.code()),
            Inner::Ssh(p) => p.wait().map(|c| c as i32),
        }
    }
}

// ---------- deleting ----------

/// Deletes the items given. `S n`: item n starts, `D n`: it's deleted, `F n why`: it isn't (and
/// rm's messages come in between, on the same pipe). Its input closing stops it, rm included:
/// that's how Cancel works, as ending `wsl.exe` or an SSH channel leaves Linux programs running.
/// `--one-file-system`: never into something mounted inside a folder (a Windows drive).
const REMOVE: &str = concat!(
    r#"exec 3<&0 0</dev/null; o=; rm --help 2>&1 | grep -q one-file-system && o=--one-file-system; r=; "#,
    r#"trap 'kill "$r" 2>/dev/null; exit 130' TERM; ( cat <&3 >/dev/null; kill -TERM "$$" ) >/dev/null 2>&1 & w=$!; i=0; "#,
    r#"for p do printf 'S\t%d\n' "$i"; case "$HOME/" in "$p"/*) printf 'F\t%d\tIt holds your home folder\n' "$i";; "#,
    r#"*) rm -rf $o -- "$p" 3<&- 2>&1 & r=$!; if wait "$r"; then printf 'D\t%d\n' "$i"; else printf 'F\t%d\t\n' "$i"; fi;; esac; i=$((i+1)); done; "#,
    r#"kill "$w" 2>/dev/null"#,
);

/// How long a stopped delete gets to end by itself.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// What happened to an item (by its place in the list) `Host::remove_all` was given.
#[derive(Debug, PartialEq)]
pub enum Removal {
    Started(usize),
    Deleted(usize),
    Failed(usize, String),
}

/// Folders a delete must never take: the root, everything right under it (`/home`, `/mnt`) and
/// a Windows drive in WSL. (The home folder is refused in Linux, where it's known.)
pub fn protected(linux: &str) -> bool {
    let parts: Vec<&str> = linux.split('/').filter(|p| !p.is_empty()).collect();
    parts.len() < 2 || (parts.len() == 2 && parts[0] == "mnt")
}

impl Host {
    /// `rm -rf` of `linux` (absolute paths) on this system. `stop` is asked all along; Ok(false)
    /// when it stopped the delete, which leaves what wasn't deleted yet. An item not reported
    /// as deleted or failed wasn't reached. rm is happy with what doesn't exist: `Deleted` means
    /// it's not there, not that this path was the item meant.
    pub fn remove_all(&self, linux: &[String], stop: &dyn Fn() -> bool, report: &mut dyn FnMut(Removal)) -> Result<bool, String> {
        let mut args = vec!["-c", REMOVE, "sh"];
        args.extend(linux.iter().map(String::as_str));
        let mut child = self.spawn("sh", &args).map_err(|e| format!("Couldn't start it on {}: {e}", self.label()))?;
        let out = child.stdout.take().ok_or("It has no output")?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                if tx.send(String::from_utf8_lossy(&line).trim_end_matches('\r').to_string()).is_err() {
                    return;
                }
            }
        });
        if let Some(mut err) = child.stderr.take() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut err, &mut std::io::sink());
            });
        }
        // rm's first message about the item it's on, for when it fails.
        let mut said: Option<String> = None;
        loop {
            if stop() {
                child.stdin = None;
                let asked = Instant::now();
                while asked.elapsed() < STOP_WAIT && !matches!(rx.recv_timeout(Duration::from_millis(100)), Err(RecvTimeoutError::Disconnected)) {}
                child.kill();
                child.wait();
                return Ok(false);
            }
            let line = match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut f = line.splitn(3, '\t');
            match (f.next(), f.next().and_then(|n| n.parse::<usize>().ok()).filter(|n| *n < linux.len()), f.next()) {
                (Some("S"), Some(n), None) => {
                    said = None;
                    report(Removal::Started(n));
                }
                (Some("D"), Some(n), None) => report(Removal::Deleted(n)),
                (Some("F"), Some(n), Some(why)) => {
                    let why = if why.is_empty() { said.take().unwrap_or_else(|| "rm didn't say why".into()) } else { why.to_string() };
                    report(Removal::Failed(n, why));
                }
                _ => {
                    if said.is_none() {
                        said = Some(line.strip_prefix("rm: ").unwrap_or(&line).to_string());
                    }
                }
            }
        }
        child.wait();
        Ok(true)
    }
}

/// Is this distro running? Asked at most once a minute (starting `wsl.exe` takes ~100 ms).
fn distro_running(distro: &str) -> bool {
    static SEEN: OnceLock<Mutex<(Option<Instant>, Vec<String>)>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Mutex::default).lock();
    if seen.0.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
        *seen = (Some(Instant::now()), crate::wsl::running());
    }
    seen.1.iter().any(|d| d.eq_ignore_ascii_case(distro))
}

// ---------- the helper ----------

const HELPER_X64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ferret-wsl-size-x86_64-unknown-linux-musl"));
const HELPER_ARM64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ferret-wsl-size-aarch64-unknown-linux-musl"));
const HELPER_X64_SHA256: &str = env!("FERRET_HELPER_X64_SHA256");
const HELPER_ARM64_SHA256: &str = env!("FERRET_HELPER_ARM64_SHA256");

/// How Linux starts the helper (its name: $1), at the lowest priority, once its SHA-256 is the
/// one Ferret built ($2; "-": not checked). 98: it isn't there yet; 99: it isn't the one Ferret
/// put there (damaged, or replaced). The check can't stop someone who already runs as this Linux
/// user (they could change the copy right after it), but the helper runs with no more rights
/// than theirs, and what it reports is only taken as sizes.
pub const HELPER_START: &str = r#"f="${XDG_CACHE_HOME:-$HOME/.cache}/ferret/$1"; want=$2; shift 2; [ -x "$f" ] || exit 98; if [ "$want" != - ] && command -v sha256sum >/dev/null 2>&1; then s=$(sha256sum "$f") && [ "${s%% *}" = "$want" ] || exit 99; fi; command -v ionice >/dev/null 2>&1 && ionice -c 3 -p $$ >/dev/null 2>&1; command -v nice >/dev/null 2>&1 && exec nice -n 19 "$f" "$@"; exec "$f" "$@""#;

/// How it gets there: from stdin, under its name ($1), once all its bytes ($2) are in. Copies of
/// other versions go a month after they came (another Ferret, the installed one next to a
/// development build, may still use one).
pub(crate) const HELPER_PLACE: &str = r#"set -e; d="${XDG_CACHE_HOME:-$HOME/.cache}/ferret"; mkdir -p "$d"; t=$(mktemp "$d/.part.XXXXXX"); cat > "$t"; [ $(($(wc -c < "$t"))) -eq "$2" ] || { rm -f "$t"; exit 1; }; chmod 755 "$t"; mv -f "$t" "$d/$1"; find "$d" -maxdepth 1 -name 'ferret-wsl-size-*' ! -name "$1" -mtime +30 -exec rm -f {} + 2>/dev/null || true"#;

pub struct Helper {
    /// Its name on each system: from its hash, so each version has its own.
    pub name: String,
    pub sha256: String,
    bytes: &'static [u8],
}

/// The helper's build for a system (by `uname -m`), if Ferret has one and it's in this build.
pub fn helper(host: &Host) -> Option<&'static Helper> {
    static HELPERS: OnceLock<[Option<Helper>; 2]> = OnceLock::new();
    let all = HELPERS.get_or_init(|| {
        let make = |bytes: &'static [u8], sha: &str| {
            // Tests: "off" counts with find; "broken" with a helper that can't (for the fallback).
            #[cfg(debug_assertions)]
            match std::env::var("FERRET_WSL_HELPER").as_deref() {
                Ok("off") => return None,
                Ok("broken") => return Some(Helper { name: "ferret-wsl-size-broken".into(), sha256: "-".into(), bytes: &b"exit 42\n"[..] }),
                _ => {}
            }
            (!bytes.is_empty()).then(|| Helper { name: format!("ferret-wsl-size-{}", &sha[..16]), sha256: sha.to_string(), bytes })
        };
        [make(HELPER_X64, HELPER_X64_SHA256), make(HELPER_ARM64, HELPER_ARM64_SHA256)]
    });
    let arch = host.arch()?;
    let at = match arch.as_str() {
        "x86_64" | "amd64" => 0,
        "aarch64" | "arm64" | "armv8l" => 1,
        _ => return None,
    };
    let h = all[at].as_ref()?;
    (state().lock().get(host) != Some(&State::Failed)).then_some(h)
}

/// The helper on each system, this run: put there (at first, or after it was found changed:
/// 98, 99), or not starting there (a home mounted noexec…: counted with find).
#[derive(Clone, Copy, PartialEq)]
enum State {
    Placed,
    Failed,
}

fn state() -> &'static Mutex<HashMap<Host, State>> {
    static S: OnceLock<Mutex<HashMap<Host, State>>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

/// A server's session closed: its next one checks the helper again.
pub fn forget_ssh(id: &str) {
    state().lock().remove(&Host::Ssh(id.to_string()));
}

/// Puts the helper on a system (`HELPER_PLACE`).
fn place(host: &Host, h: &Helper) -> bool {
    place_as(host, h, &h.name)
}

/// … under a name of its own (tests).
pub(crate) fn place_as(host: &Host, h: &Helper, name: &str) -> bool {
    let size = h.bytes.len().to_string();
    let Ok(mut child) = host.spawn("sh", &["-c", HELPER_PLACE, "sh", name, &size]) else { return false };
    let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(h.bytes).is_ok());
    let mut said = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut said);
    }
    let code = child.wait();
    if wrote && code == Some(0) {
        return true;
    }
    log::warn!("{}: couldn't put the counting helper there (exit {code:?}: {})", host.label(), said.trim());
    false
}

/// After a run of the helper that didn't start (it printed nothing): puts it there and says to
/// try again (98, 99: once a run), or marks the system as counted without it.
pub fn after_failed_start(host: &Host, h: &Helper, code: Option<i32>, said: &str) -> bool {
    let placed = state().lock().get(host) == Some(&State::Placed);
    if matches!(code, Some(98 | 99)) && !placed && place(host, h) {
        let why = if code == Some(99) { ", replacing a copy that wasn't Ferret's" } else { "" };
        log::info!("{}: put the counting helper there ({}{why})", host.label(), h.name);
        state().lock().insert(host.clone(), State::Placed);
        return true;
    }
    log::warn!("{}: the counting helper didn't start (exit {code:?}; {said:?}), counting with find", host.label());
    state().lock().insert(host.clone(), State::Failed);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_of_paths() {
        assert_eq!(Host::of(r"\\wsl.localhost\Ubuntu\home\me"), Some((Host::Wsl("Ubuntu".into()), "/home/me".into())));
        assert_eq!(Host::of(r"\\ssh:\home.local\srv"), Some((Host::Ssh("home.local".into()), "/srv".into())));
        assert_eq!(Host::of(r"C:\Users"), None);
    }

    #[test]
    fn protected_folders() {
        for p in ["/", "/home", "/mnt", "/usr", "/mnt/c", "/mnt/d/"] {
            assert!(protected(p), "{p}");
        }
        for p in ["/home/me", "/home/me/x", "/mnt/c/x", "/tmp/x", "/usr/lib"] {
            assert!(!protected(p), "{p}");
        }
    }

    fn sh(host: &Host, script: &str, arg: &str) -> String {
        let mut c = host.spawn("sh", &["-c", script, "sh", arg]).unwrap();
        let mut said = String::new();
        c.stdout.take().unwrap().read_to_string(&mut said).unwrap();
        c.wait();
        said.trim().to_string()
    }

    /// Only in folders of the test's: names Windows can't have, a link to a folder (its target
    /// stays), what can't be deleted, the home folder (a stand-in: a test must never hand the
    /// real one to a delete), and stopping it midway (in `big`, on a real disk: /tmp can take
    /// memory, and deletes too fast to catch).
    fn removes_and_stops(host: &Host, dir: &str, big: &str) {
        sh(host, r#"mkdir -p "$1/keep/inside" "$1/full/sub" "$1/locked/in"; cd "$1"; touch -- 'a:b' 'q?x' 'dq"x' 'back\slash' 'tab	x' '-rf' "it's" '$HOME' 'café 日本語' keep/inside/f full/sub/f locked/in/f; ln -s keep link; chmod 555 locked/in"#, dir);
        let names = ["a:b", "q?x", "dq\"x", "back\\slash", "tab\tx", "-rf", "it's", "$HOME", "café 日本語", "full", "link"];
        // rm -f is happy with what doesn't exist: it all has to be there first.
        assert_eq!(sh(host, r#"ls -A "$1" | wc -l"#, dir), "13");
        let mut items: Vec<String> = names.iter().map(|n| format!("{dir}/{n}")).collect();
        items.push(format!("{dir}/locked/in/f"));
        let mut seen = Vec::new();
        assert_eq!(host.remove_all(&items, &|| false, &mut |r| seen.push(r)), Ok(true));
        let n = names.len();
        for (i, name) in names.iter().enumerate() {
            assert!(seen.contains(&Removal::Deleted(i)), "{name}: {seen:?}");
        }
        assert!(seen.iter().any(|r| matches!(r, Removal::Failed(i, why) if *i == n && why.contains("ermission denied"))), "{seen:?}");
        assert_eq!(sh(host, r#"cd "$1"; ls -A | sort | tr '\n' ' '; find keep locked/in -type f | sort | tr '\n' ' '"#, dir), "keep locked keep/inside/f locked/in/f");

        let (home, holds) = (format!("{dir}/keep/inside"), format!("{dir}/keep"));
        let mut c = host.spawn("env", &[format!("HOME={home}").as_str(), "sh", "-c", REMOVE, "sh", &home, &holds]).unwrap();
        let mut said = String::new();
        c.stdout.take().unwrap().read_to_string(&mut said).unwrap();
        c.wait();
        assert_eq!(said.lines().filter(|l| l.ends_with("It holds your home folder")).count(), 2, "{said}");
        assert_eq!(sh(host, r#"find "$1/keep" -type f"#, dir), format!("{dir}/keep/inside/f"));
        sh(host, r#"chmod -R u+w "$1"; rm -rf -- "$1""#, dir);

        sh(host, r#"mkdir -p "$1"; cd "$1"; seq 1 300000 | xargs touch"#, big);
        let began = std::cell::Cell::new(None::<Instant>);
        let stop = || began.get().is_some_and(|t: Instant| t.elapsed() > Duration::from_millis(150));
        let result = host.remove_all(&[big.to_string()], &stop, &mut |r| {
            if matches!(r, Removal::Started(_)) {
                began.set(Some(Instant::now()));
            }
        });
        let took = began.get().unwrap().elapsed();
        let left = || sh(host, r#"find "$1" | wc -l"#, big);
        let a = left();
        std::thread::sleep(Duration::from_millis(800));
        let b = left();
        let running = sh(host, r#"ps -eo args | grep '^rm ' | grep -c -- "$1""#, big);
        sh(host, r#"rm -rf -- "$1""#, big);
        assert_eq!(result, Ok(false));
        assert!(took < STOP_WAIT, "took {took:?} to stop");
        assert_eq!(a, b, "still deleting after it stopped");
        assert_eq!(running, "0");
        let a: u64 = a.parse().unwrap();
        assert!(a > 1000 && a < 300_001, "{a} left of 300,001");
        println!("{}: stopped in {took:?}, {a} of 300,001 left", host.label());
    }

    /// In a running distro. `cargo test --lib wsl_remove -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn wsl_remove() {
        let Some(distro) = crate::wsl::running().into_iter().next() else { return };
        let pid = std::process::id();
        removes_and_stops(&Host::Wsl(distro), &format!("/tmp/ferret-rm-{pid}"), &format!("/var/tmp/ferret-rm-test-{pid}"));
    }

    /// `FERRET_TEST_SSH=<a ~/.ssh/config host> cargo test --lib ssh_remove -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn ssh_remove() {
        let Ok(alias) = std::env::var("FERRET_TEST_SSH") else { return };
        let id = crate::remote::test_connection(&alias).expect("connects");
        let s = crate::remote::block(ssh::session(&id)).unwrap();
        let pid = std::process::id();
        removes_and_stops(&Host::Ssh(id), &format!("/tmp/ferret-rm-{pid}"), &format!("{}/ferret-rm-test-{pid}", s.home));
    }
}
