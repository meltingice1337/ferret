//! Windows / UNC / WSL path helpers. Paths are passed around as display strings
//! (`C:\Users\me`, `\\wsl.localhost\Arch\home`) and keyed case-insensitively,
//! except WSL paths which keep their case.

const WSL_PREFIX: &str = r"\\wsl.localhost\";

pub fn is_wsl(p: &str) -> bool {
    let l = p.to_ascii_lowercase();
    l.starts_with(r"\\wsl.localhost\") || l.starts_with(r"\\wsl$\")
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
    if is_wsl(&n) {
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
