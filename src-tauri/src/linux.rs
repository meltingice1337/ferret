//! Linux systems Ferret runs its programs on: WSL distros (through `wsl.exe`) and SSH servers
//! (through a command channel of an open session). Folder sizes and search drive both the same
//! way: the same helper, the same scripts, the same output.
//!
//! The helper (src-tauri/wsl-size, built for x86_64 and ARM64 and embedded with the app) is put
//! in the user's cache folder there the first time it's needed, as VS Code puts its server in
//! ~/.vscode-server, and checked against its SHA-256 before each run.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::OnceLock;
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
}
