//! Windows / UNC / WSL / server path helpers. Paths are passed around as display strings
//! (`C:\Users\me`, `\\wsl.localhost\Arch\home`, `\\ssh:\home.local\home\me`) and keyed
//! case-insensitively, except WSL and server paths which keep their case.

const WSL_PREFIX: &str = r"\\wsl.localhost\";

pub fn is_wsl(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.starts_with(r"\\wsl.localhost\") || l.starts_with(r"\\wsl$\")
}

/// Folders on servers Ferret connects to (remote/): `\\ssh:\<connection>\home\me` is `/home/me`
/// on that connection's server. A colon is never part of a Windows server name, so a path like
/// this that reaches Windows by mistake is turned down at once instead of being looked for on
/// the network.
const REMOTE_PREFIX: &str = r"\\ssh:\";

pub fn is_remote(p: &str) -> bool {
    p.get(..REMOTE_PREFIX.len()).is_some_and(|s| s.eq_ignore_ascii_case(REMOTE_PREFIX))
}

/// `\\ssh:\home.local\home\me` → `("home.local", "/home/me")`; the connection's root is `/`.
pub fn remote_parts(p: &str) -> Option<(&str, String)> {
    if !is_remote(p) {
        return None;
    }
    let rest = &p[REMOTE_PREFIX.len()..];
    let (id, inside) = rest.split_once('\\').unwrap_or((rest, ""));
    if id.is_empty() {
        return None;
    }
    let inside = inside.trim_matches('\\');
    Some((id, if inside.is_empty() { "/".to_string() } else { format!("/{}", inside.replace('\\', "/")) }))
}

/// The Ferret path of `linux` (an absolute path) on connection `id`.
pub fn remote_path(id: &str, linux: &str) -> String {
    let inside = linux.trim_matches('/');
    if inside.is_empty() { format!("{REMOTE_PREFIX}{id}") } else { format!("{REMOTE_PREFIX}{id}\\{}", inside.replace('/', "\\")) }
}

/// Canonical display form: backslashes, upper-case drive letter, `\\wsl$` →
/// `\\wsl.localhost`, no trailing slash except on drive roots.
pub fn normalize(input: &str) -> String {
    let mut s = input.trim().trim_matches('"').replace('/', "\\");
    // The long-path form (`\\?\C:\…`, `\\?\UNC\server\share\…`), which the Recycle Bin records
    // for long paths.
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.len() > 4 && rest[..4].eq_ignore_ascii_case("UNC\\") {
            s = format!(r"\\{}", &rest[4..]);
        } else if rest.as_bytes().get(1) == Some(&b':') {
            s = rest.to_string();
        }
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with(r"\\wsl$\") {
        s = format!("{WSL_PREFIX}{}", &s[7..]);
    } else if lower.starts_with(r"\\wsl.localhost\") {
        s = format!("{WSL_PREFIX}{}", &s[16..]);
    }
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        s = format!("{}{}", (bytes[0] as char).to_ascii_uppercase(), &s[1..]);
        if s.len() == 2 {
            s.push('\\');
        }
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// Form handed to std::fs: share roots need a trailing slash.
pub fn for_io(p: &str) -> String {
    if p.starts_with(r"\\") && p[2..].split('\\').filter(|s| !s.is_empty()).count() == 2 {
        format!("{p}\\")
    } else {
        p.to_string()
    }
}

pub fn key(p: &str) -> String {
    let n = normalize(p);
    if is_wsl(&n) || is_remote(&n) {
        n
    } else {
        n.to_lowercase()
    }
}

pub fn parent(p: &str) -> Option<String> {
    let p = normalize(p);
    if p.len() == 3 && p.ends_with(":\\") {
        return None;
    }
    if let Some(rest) = p.strip_prefix(r"\\") {
        let parts: Vec<&str> = rest.split('\\').filter(|s| !s.is_empty()).collect();
        if parts.len() <= 2 {
            return None;
        }
        return Some(format!(r"\\{}", parts[..parts.len() - 1].join("\\")));
    }
    let idx = p.rfind('\\')?;
    let par = &p[..idx];
    if par.len() == 2 && par.ends_with(':') {
        Some(format!("{par}\\"))
    } else {
        Some(par.to_string())
    }
}

/// `\\wsl.localhost\Arch\home` → `Some("Arch")`
pub fn wsl_distro(p: &str) -> Option<String> {
    let n = normalize(p);
    n.strip_prefix(WSL_PREFIX)
        .and_then(|r| r.split('\\').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn wsl_root(distro: &str) -> String {
    format!("{WSL_PREFIX}{distro}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("c:"), r"C:\");
        assert_eq!(normalize(r"c:\Users\"), r"C:\Users");
        assert_eq!(normalize(r"\\wsl$\Arch\home\"), r"\\wsl.localhost\Arch\home");
        assert_eq!(normalize("C:/a/b"), r"C:\a\b");
        assert_eq!(normalize(r"\\?\c:\a\b"), r"C:\a\b");
        assert_eq!(normalize(r"\\?\UNC\server\share\x"), r"\\server\share\x");
    }

    #[test]
    fn parents() {
        assert_eq!(parent(r"C:\a\b").as_deref(), Some(r"C:\a"));
        assert_eq!(parent(r"C:\a").as_deref(), Some(r"C:\"));
        assert_eq!(parent(r"C:\"), None);
        assert_eq!(parent(r"\\wsl.localhost\Arch\home").as_deref(), Some(r"\\wsl.localhost\Arch"));
        assert_eq!(parent(r"\\wsl.localhost\Arch"), None);
    }

    #[test]
    fn keys() {
        assert_eq!(key(r"C:\Users\Me"), r"c:\users\me");
        assert_eq!(key(r"\\wsl$\Arch\Home"), r"\\wsl.localhost\Arch\Home");
        assert_eq!(for_io(r"\\wsl.localhost\Arch"), r"\\wsl.localhost\Arch\");
        assert_eq!(key(r"\\ssh:\home.local\home\Me"), r"\\ssh:\home.local\home\Me");
    }

    #[test]
    fn remote_paths() {
        assert!(is_remote(r"\\ssh:\home.local\home"));
        assert!(!is_remote(r"\\server\share"));
        assert_eq!(remote_parts(r"\\ssh:\home.local\home\me"), Some(("home.local", "/home/me".to_string())));
        assert_eq!(remote_parts(r"\\ssh:\home.local"), Some(("home.local", "/".to_string())));
        assert_eq!(remote_parts(r"\\ssh:\"), None);
        assert_eq!(remote_path("home.local", "/home/me"), r"\\ssh:\home.local\home\me");
        assert_eq!(remote_path("home.local", "/"), r"\\ssh:\home.local");
        assert_eq!(normalize(r"\\ssh:\home.local\home\"), r"\\ssh:\home.local\home");
        assert_eq!(parent(r"\\ssh:\home.local\home\me").as_deref(), Some(r"\\ssh:\home.local\home"));
        assert_eq!(parent(r"\\ssh:\home.local\home").as_deref(), Some(r"\\ssh:\home.local"));
        assert_eq!(parent(r"\\ssh:\home.local"), None);
        // Windows turns it down without asking the network.
        let t = std::time::Instant::now();
        assert!(std::fs::metadata(r"\\ssh:\home.local\home").is_err());
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
}

/// Expands 8.3 short names (`MELTIN~1`) to the real long path for local paths
/// that exist (or whose parent exists). UNC/WSL paths are returned as-is.
pub fn long_form(p: &str) -> String {
    let n = normalize(p);
    if n.starts_with(r"\\") || !n.contains('~') {
        return n;
    }
    if let Ok(c) = std::fs::canonicalize(for_io(&n)) {
        let s = c.to_string_lossy();
        return normalize(s.strip_prefix(r"\\?\").unwrap_or(&s));
    }
    match (parent(&n), n.rsplit('\\').next()) {
        (Some(par), Some(name)) if par != n => {
            let lp = long_form(&par);
            if lp.ends_with('\\') { format!("{lp}{name}") } else { format!("{lp}\\{name}") }
        }
        _ => n,
    }
}
