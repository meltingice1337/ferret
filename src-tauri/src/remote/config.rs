//! Saved connections, and what ~/.ssh/config says about a host.
//!
//! A connection is what the user filled in (or picked from ~/.ssh/config and adjusted) in the
//! connection form. One made from a config host keeps its alias: at each connect the config is
//! read again for what the form doesn't show (more key files, IdentitiesOnly, ProxyJump), and the
//! form's own values win.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    /// Stable and path-safe: the server's folders are `\\ssh:\<id>\…` (favorites, recents and
    /// remembered views keep working after a rename).
    pub id: String,
    pub name: String,
    /// "ssh" (others may follow: ftp, smb).
    #[serde(default = "ssh")]
    pub kind: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    /// The ~/.ssh/config Host it was made from.
    #[serde(default)]
    pub ssh_alias: Option<String>,
    /// "auto": the SSH agent, then the key files, then a password if the server takes one;
    /// "key": only `identity_file`; "password": only a password.
    #[serde(default = "auto")]
    pub auth: String,
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Where it opens (a Linux path); the user's home when empty.
    #[serde(default)]
    pub start_dir: Option<String>,
    /// Ctrl+K searches it while it's connected.
    #[serde(default)]
    pub search: bool,
}

fn ssh() -> String {
    "ssh".into()
}
fn auto() -> String {
    "auto".into()
}
fn default_port() -> u16 {
    22
}

/// A path-safe id for a new connection, from its name or host ("Home server" → "home-server"),
/// different from the ones taken.
pub fn new_id(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches(['-', '.']).to_string();
    let base = if base.is_empty() { "server".to_string() } else { base };
    let free = |id: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(id));
    if free(&base) {
        return base;
    }
    (2..).map(|i| format!("{base}-{i}")).find(|id| free(id)).expect("some number is free")
}

// ---------- ~/.ssh/config ----------

/// What the config says for one host (the first value of each option wins, as in OpenSSH).
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostConfig {
    /// The alias, as written after `Host`.
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<String>,
    pub identities_only: bool,
    pub proxy_jump: Option<String>,
    pub host_key_alias: Option<String>,
    pub known_hosts_files: Vec<String>,
}

/// One `Keyword value` line, with the block it's in.
#[derive(Debug, Clone)]
struct Line {
    /// The `Host` patterns this line applies to; None: before any Host (applies to all), and
    /// `Match` blocks get an empty list (never applies: Ferret doesn't evaluate them).
    hosts: Option<Vec<String>>,
    key: String,
    value: String,
}

/// ~/.ssh, or in the end-to-end tests a folder of their own (FERRET_SSH_DIR): their server's key
/// is trusted there, never in the user's known_hosts.
fn ssh_dir() -> Option<PathBuf> {
    match test_ssh_dir() {
        Some(d) => Some(d),
        None => dirs::home_dir().map(|h| h.join(".ssh")),
    }
}

/// The tests' own ~/.ssh (only in the end-to-end test copy). With it, nothing of the user's SSH
/// setup is used: no agent, no system-wide known_hosts.
pub fn test_ssh_dir() -> Option<PathBuf> {
    crate::integration::e2e().then(|| std::env::var_os("FERRET_SSH_DIR").map(PathBuf::from)).flatten()
}

pub fn config_path() -> Option<PathBuf> {
    ssh_dir().map(|d| d.join("config"))
}

/// `Keyword value`, `Keyword=value` or `Keyword "quoted value"`.
fn split_line(raw: &str) -> Option<(String, String)> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let at = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..at].to_ascii_lowercase();
    let mut value = line[at..].trim_start_matches(|c: char| c.is_whitespace()).trim_start_matches('=').trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    Some((key, value.to_string()))
}

/// Words of a value, with "quoted words" kept together.
fn words(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn read_lines(path: &Path, depth: u32, inherited: Option<Vec<String>>, out: &mut Vec<Line>) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let mut hosts = inherited.clone();
    for raw in text.lines() {
        let Some((key, value)) = split_line(raw) else { continue };
        match key.as_str() {
            "host" => hosts = Some(words(&value)),
            "match" => hosts = Some(Vec::new()),
            // Included files are read where the Include is, as part of the block it's in.
            "include" if depth < 8 => {
                for pattern in words(&value) {
                    for file in expand_include(&pattern) {
                        read_lines(&file, depth + 1, hosts.clone(), out);
                    }
                }
            }
            _ => out.push(Line { hosts: hosts.clone(), key, value }),
        }
    }
}

/// An Include pattern's files: relative to ~/.ssh, `*` and `?` in the last part only.
fn expand_include(pattern: &str) -> Vec<PathBuf> {
    let p = expand_home(pattern);
    let p = if Path::new(&p).is_absolute() { PathBuf::from(p) } else { ssh_dir().unwrap_or_default().join(p) };
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![p];
    }
    let Some(dir) = p.parent() else { return vec![] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| wildcard(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

/// `~/.ssh/id_ed25519` → `C:\Users\me\.ssh\id_ed25519` (Windows' slashes throughout).
fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.replace('/', "\\")).to_string_lossy().into_owned(),
        _ => p.to_string(),
    }
}

/// OpenSSH's patterns: `*` any run of characters, `?` one, case-insensitive.
pub(crate) fn wildcard(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    go(&p, &t)
}

/// A Host line's patterns match `host`: one of them does, and no `!pattern` does.
fn host_matches(patterns: &[String], host: &str) -> bool {
    let mut hit = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if wildcard(neg, host) {
                return false;
            }
        } else if wildcard(p, host) {
            hit = true;
        }
    }
    hit
}

fn lines() -> Vec<Line> {
    let mut out = Vec::new();
    if let Some(p) = config_path() {
        read_lines(&p, 0, None, &mut out);
    }
    out
}

/// The hosts the config names outright (no wildcards), in its order, for the connection form.
pub fn config_hosts() -> Vec<HostConfig> {
    let all = lines();
    let mut seen: Vec<String> = Vec::new();
    for l in &all {
        for h in l.hosts.iter().flatten() {
            if !h.contains(['*', '?', '!']) && !seen.iter().any(|s| s.eq_ignore_ascii_case(h)) {
                seen.push(h.clone());
            }
        }
    }
    seen.into_iter().map(|h| resolve_in(&all, &h)).collect()
}

/// Everything the config says for `alias`.
pub fn resolve(alias: &str) -> HostConfig {
    resolve_in(&lines(), alias)
}

fn resolve_in(all: &[Line], alias: &str) -> HostConfig {
    let mut c = HostConfig { alias: alias.to_string(), ..Default::default() };
    let mut identities_only = None;
    for l in all {
        let applies = match &l.hosts {
            None => true,
            Some(p) => host_matches(p, alias),
        };
        if !applies {
            continue;
        }
        let first = words(&l.value).into_iter().next().unwrap_or_default();
        match l.key.as_str() {
            "hostname" if c.host_name.is_none() => c.host_name = Some(first),
            "user" if c.user.is_none() => c.user = Some(first),
            "port" if c.port.is_none() => c.port = first.parse().ok(),
            "identityfile" => {
                if !l.value.eq_ignore_ascii_case("none") {
                    c.identity_files.push(l.value.clone());
                }
            }
            "identitiesonly" if identities_only.is_none() => identities_only = Some(first.eq_ignore_ascii_case("yes")),
            "proxyjump" if c.proxy_jump.is_none() && !first.eq_ignore_ascii_case("none") => c.proxy_jump = Some(first),
            "hostkeyalias" if c.host_key_alias.is_none() => c.host_key_alias = Some(first),
            "userknownhostsfile" if c.known_hosts_files.is_empty() => c.known_hosts_files = words(&l.value),
            _ => {}
        }
    }
    c.identities_only = identities_only.unwrap_or(false);
    // %h, %u… in key file names, as OpenSSH expands them.
    let host = c.host_name.clone().unwrap_or_else(|| alias.to_string());
    let user = c.user.clone().unwrap_or_default();
    c.identity_files = c.identity_files.iter().map(|f| expand_tokens(f, alias, &host, &user)).collect();
    c.known_hosts_files = c.known_hosts_files.iter().map(|f| expand_tokens(f, alias, &host, &user)).collect();
    c
}

fn expand_tokens(s: &str, alias: &str, host: &str, user: &str) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    let local_user = std::env::var("USERNAME").unwrap_or_default();
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('h') => out.push_str(host),
            Some('n') => out.push_str(alias),
            Some('r') => out.push_str(user),
            Some('u') => out.push_str(&local_user),
            Some('d') => out.push_str(&home),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    expand_home(&out)
}

/// The key files to try for a connection: its own, then the config's, then OpenSSH's defaults
/// (unless the config says IdentitiesOnly and names some).
pub fn identity_files(conn: &Connection, cfg: &HostConfig) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !files.contains(&p) {
            files.push(p);
        }
    };
    if let Some(f) = conn.identity_file.as_deref().filter(|f| !f.trim().is_empty()) {
        push(PathBuf::from(expand_home(f.trim())));
    }
    if conn.auth == "key" {
        return files;
    }
    for f in &cfg.identity_files {
        push(PathBuf::from(f));
    }
    if (!cfg.identities_only || cfg.identity_files.is_empty())
        && let Some(dir) = ssh_dir()
    {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            push(dir.join(name));
        }
    }
    files.into_iter().filter(|f| f.is_file()).collect()
}

/// The known_hosts files to check a server's key against (the first one learns new keys).
pub fn known_hosts_files(cfg: &HostConfig) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = if cfg.known_hosts_files.is_empty() {
        ssh_dir().map(|d| vec![d.join("known_hosts"), d.join("known_hosts2")]).unwrap_or_default()
    } else {
        cfg.known_hosts_files.iter().map(PathBuf::from).collect()
    };
    // Windows OpenSSH's system-wide file.
    if let Ok(pd) = std::env::var("PROGRAMDATA")
        && test_ssh_dir().is_none()
    {
        files.push(Path::new(&pd).join("ssh").join("ssh_known_hosts"));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Line> {
        let dir = std::env::temp_dir().join(format!("ferret-sshcfg-{}-{}", std::process::id(), text.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config");
        std::fs::write(&f, text).unwrap();
        let mut out = Vec::new();
        read_lines(&f, 0, None, &mut out);
        std::fs::remove_dir_all(&dir).unwrap();
        out
    }

    #[test]
    fn resolves_like_openssh() {
        let all = parse(
            "# comment\nIdentitiesOnly yes\n\nHost home.local nas\n   HostName 192.168.0.200\n   User darius\n   IdentityFile ~/.ssh/id_ed25519\n\
             \nHost *.ops.example.com !bastion.ops.example.com\n  User deploy\n  ProxyJump bastion.ops.example.com\n\
             Host *\n  User fallback\n  Port=2222\n  IdentityFile \"~/.ssh/other key\"\nMatch host x\n  User never\n",
        );
        let home = resolve_in(&all, "home.local");
        assert_eq!(home.host_name.as_deref(), Some("192.168.0.200"));
        assert_eq!(home.user.as_deref(), Some("darius"));
        assert_eq!(home.port, Some(2222));
        assert!(home.identities_only);
        assert_eq!(home.identity_files.len(), 2);
        assert!(home.identity_files[1].ends_with("other key"));
        let app = resolve_in(&all, "app1.ops.example.com");
        assert_eq!(app.user.as_deref(), Some("deploy"));
        assert_eq!(app.proxy_jump.as_deref(), Some("bastion.ops.example.com"));
        let bastion = resolve_in(&all, "bastion.ops.example.com");
        assert_eq!(bastion.user.as_deref(), Some("fallback"));
        assert_eq!(bastion.proxy_jump, None);
        // Match blocks are never applied.
        assert_ne!(resolve_in(&all, "x").user.as_deref(), Some("never"));
    }

    #[test]
    fn patterns() {
        assert!(wildcard("*.local", "home.local"));
        assert!(wildcard("h?me.LOCAL", "home.local"));
        assert!(!wildcard("*.local", "home.lan"));
        assert!(host_matches(&["*".into(), "!secret".into()], "home"));
        assert!(!host_matches(&["*".into(), "!secret".into()], "secret"));
        assert_eq!(split_line("Port=22"), Some(("port".into(), "22".into())));
        assert_eq!(words(r#"a "b c" d"#), vec!["a", "b c", "d"]);
    }

    #[test]
    fn ids() {
        assert_eq!(new_id("Home server", &[]), "home-server");
        assert_eq!(new_id("home.local", &["home.local".into()]), "home.local-2");
        assert_eq!(new_id("  ", &[]), "server");
        assert_eq!(new_id("a/b\\c:d", &[]), "a-b-c-d");
    }
}
