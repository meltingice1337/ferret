//! Checking a server's key against OpenSSH's known_hosts files, and adding one the user trusts.
//!
//! Ferret reads the same files as `ssh` (so a server already trusted there is trusted here, and
//! the other way round), with OpenSSH's rules: plain and hashed (`|1|salt|hash`) names, `*`/`?`
//! and `!negated` patterns, `[host]:port` for other ports, `@revoked` keys (never accepted) and
//! `@cert-authority` lines (not used: Ferret doesn't take host certificates). A line it can't read
//! is skipped, not taken as a match.

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::ssh_key::PublicKey;
use sha1::Sha1;
use std::io::Write;
use std::path::Path;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// A line names this host with this key.
    Known,
    /// Lines name this host, with other keys of the same type: the key changed (or someone is
    /// in between).
    Changed { file: String, line: usize },
    /// A line marks this key revoked.
    Revoked,
    /// Nothing about this host (or only keys of other types).
    Unknown,
}

/// The name a host is recorded under: `host`, or `[host]:port` for a port other than 22.
pub fn host_entry(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("[{host}]:{port}") }
}

fn name_matches(entry: &str, pattern: &str) -> bool {
    if let Some(hashed) = pattern.strip_prefix("|1|") {
        let mut parts = hashed.split('|');
        let b64 = base64::engine::general_purpose::STANDARD;
        let (Some(Ok(salt)), Some(Ok(hash))) = (parts.next().map(|s| b64.decode(s)), parts.next().map(|s| b64.decode(s))) else {
            return false;
        };
        let Ok(mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt) else { return false };
        return mac.chain_update(entry.as_bytes()).verify_slice(&hash).is_ok();
    }
    super::config::wildcard(pattern, entry)
}

/// Does a line's host list name `entry`? (One pattern does and no `!pattern` does.)
fn hosts_match(list: &str, entry: &str) -> bool {
    let mut hit = false;
    for p in list.split(',') {
        if let Some(neg) = p.strip_prefix('!') {
            if name_matches(entry, neg) {
                return false;
            }
        } else if name_matches(entry, p) {
            hit = true;
        }
    }
    hit
}

/// Checks `key` for `host`/`port` against these files (missing ones are skipped).
pub fn check(files: &[impl AsRef<Path>], host: &str, port: u16, key: &PublicKey) -> Verdict {
    let entry = host_entry(host, port);
    let mut changed = None;
    let mut known = false;
    for file in files {
        let Ok(text) = std::fs::read_to_string(file.as_ref()) else { continue };
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let first = fields.next().unwrap_or_default();
            let (marker, hosts) = if first.starts_with('@') { (Some(first), fields.next().unwrap_or_default()) } else { (None, first) };
            let (Some(kind), Some(b64)) = (fields.next(), fields.next()) else { continue };
            if !hosts_match(hosts, &entry) {
                continue;
            }
            let Ok(recorded) = PublicKey::from_openssh(&format!("{kind} {b64}")) else { continue };
            match marker {
                Some(m) if m.eq_ignore_ascii_case("@revoked") => {
                    if recorded.key_data() == key.key_data() {
                        return Verdict::Revoked;
                    }
                }
                Some(_) => {}
                None if recorded.key_data() == key.key_data() => known = true,
                None if recorded.algorithm() == key.algorithm() && changed.is_none() => {
                    changed = Some((file.as_ref().to_string_lossy().into_owned(), n + 1));
                }
                None => {}
            }
        }
    }
    // A matching line wins over an older key of the same type elsewhere (OpenSSH does the same:
    // a host can have several keys of one type listed).
    if known {
        Verdict::Known
    } else if let Some((file, line)) = changed {
        Verdict::Changed { file, line }
    } else {
        Verdict::Unknown
    }
}

/// Adds `host`/`port` with `key` to `file` (made if need be), as `ssh` writes it.
pub fn learn(file: &Path, host: &str, port: u16, key: &PublicKey) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::read(file).unwrap_or_default();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        f.write_all(b"\n")?;
    }
    let mut public = key.clone();
    public.set_comment("");
    let text = public.to_openssh().map_err(std::io::Error::other)?;
    writeln!(f, "{} {}", host_entry(host, port), text.trim())
}

/// SHA256:… as `ssh` shows it.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(russh::keys::HashAlg::Sha256).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB6FoOD0zLkZJ9zF5ut2EAQMiS3X5I4B3Hw+Y8mX6GaB";

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    fn file(text: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ferret-kh-{}-{:x}", std::process::id(), text.len() * 31 + text.bytes().map(|b| b as usize).sum::<usize>()));
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn verdicts() {
        let f = file(&format!(
            "# comment\r\nnot a valid line\r\nhome.local,192.168.0.200 {KEY_A}\r\n[nas]:2222 {KEY_A}\r\n*.lan {KEY_B}\r\nother {KEY_B} comment here\r\n@revoked bad.example {KEY_B}\r\n"
        ));
        let files = [&f];
        assert_eq!(check(&files, "192.168.0.200", 22, &key(KEY_A)), Verdict::Known);
        assert_eq!(check(&files, "home.local", 22, &key(KEY_B)), Verdict::Changed { file: f.to_string_lossy().into(), line: 3 });
        assert_eq!(check(&files, "nas", 2222, &key(KEY_A)), Verdict::Known);
        assert_eq!(check(&files, "nas", 22, &key(KEY_A)), Verdict::Unknown);
        assert_eq!(check(&files, "box.lan", 22, &key(KEY_B)), Verdict::Known);
        assert_eq!(check(&files, "nowhere", 22, &key(KEY_A)), Verdict::Unknown);
        assert_eq!(check(&files, "bad.example", 22, &key(KEY_B)), Verdict::Revoked);
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn hashed_names() {
        // `ssh-keygen -H` form of "home.local".
        let salt = [7u8; 20];
        let mac = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt).unwrap().chain_update(b"home.local").finalize().into_bytes();
        let b64 = base64::engine::general_purpose::STANDARD;
        let f = file(&format!("|1|{}|{} {KEY_A}\n", b64.encode(salt), b64.encode(mac)));
        assert_eq!(check(&[&f], "home.local", 22, &key(KEY_A)), Verdict::Known);
        assert_eq!(check(&[&f], "home.lan", 22, &key(KEY_A)), Verdict::Unknown);
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn learns() {
        let f = file("home.local ssh-ed25519 AAAA"); // no line break at the end
        learn(&f, "nas", 2222, &key(KEY_B)).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.ends_with(&format!("\n[nas]:2222 {KEY_B}\n")), "{text}");
        assert_eq!(check(&[&f], "nas", 2222, &key(KEY_B)), Verdict::Known);
        std::fs::remove_file(&f).unwrap();
        assert!(fingerprint(&key(KEY_A)).starts_with("SHA256:"));
    }
}
