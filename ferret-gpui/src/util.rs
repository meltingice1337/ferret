//! Paths as the window shows them: comparing, the folder's name, the path bar's parts.

use std::path::{Component, Path, PathBuf, Prefix};

use gpui::SharedString;

fn trimmed(p: &str) -> &str {
    let t = p.trim_end_matches(['\\', '/']);
    if t.len() == 2 && t.ends_with(':') { p } else { t }
}

pub fn same(a: &str, b: &str) -> bool {
    trimmed(a).eq_ignore_ascii_case(trimmed(b))
}

/// `path` is `root` or inside it.
pub fn within(path: &str, root: &str) -> bool {
    let (p, r) = (trimmed(path).to_lowercase(), trimmed(root).to_lowercase());
    p == r || p.starts_with(&format!("{}\\", r.trim_end_matches('\\')))
}

pub fn parent(path: &str) -> Option<String> {
    if ferret_core::recycle::is_recycle(path) || ferret_core::search::search_query_of(path).is_some() {
        return None;
    }
    if ferret_core::paths::is_remote(path) {
        return ferret_core::paths::parent(path);
    }
    Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).filter(|p| !p.is_empty() && p != "\\\\")
}

pub fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// What a tab and the window title call a folder: its name, a drive as "Label (C:)", the bin…
pub fn folder_name(path: &str) -> String {
    if ferret_core::recycle::is_recycle(path) {
        return "Recycle Bin".into();
    }
    if let Some(q) = ferret_core::search::search_query_of(path) {
        return format!("“{q}”");
    }
    let p = Path::new(path);
    match p.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => crumbs(path).last().map(|(l, _)| l.to_string()).unwrap_or_else(|| path.to_string()),
    }
}

/// The path bar's parts: (label, the path it opens).
pub fn crumbs(path: &str) -> Vec<(SharedString, String)> {
    if ferret_core::recycle::is_recycle(path) {
        return vec![("Recycle Bin".into(), path.to_string())];
    }
    if let Some(q) = ferret_core::search::search_query_of(path) {
        return vec![(format!("Search: {q}").into(), path.to_string())];
    }
    if ferret_core::paths::is_remote(path) {
        let mut out = Vec::new();
        let mut cur = Some(path.to_string());
        while let Some(p) = cur {
            let label = ferret_core::paths::parent(&p)
                .map(|_| file_name(&p.replace('/', "\\")))
                .unwrap_or_else(|| ferret_core::paths::remote_parts(&p).map(|(id, _)| format!("{id}:/")).unwrap_or_default());
            out.push((label.into(), p.clone()));
            cur = ferret_core::paths::parent(&p);
        }
        out.reverse();
        return out;
    }
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    let mut prefix: Option<String> = None;
    for comp in Path::new(path).components() {
        match comp {
            Component::Prefix(p) => {
                acc.push(comp.as_os_str());
                prefix = Some(match p.kind() {
                    Prefix::Disk(l) | Prefix::VerbatimDisk(l) => format!("{}:", l as char),
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        let server = server.to_string_lossy();
                        if server.eq_ignore_ascii_case("wsl.localhost") || server.eq_ignore_ascii_case("wsl$") {
                            format!("{} (Linux)", share.to_string_lossy())
                        } else {
                            format!("{} on {server}", share.to_string_lossy())
                        }
                    }
                    _ => comp.as_os_str().to_string_lossy().into_owned(),
                });
            }
            Component::RootDir => {
                acc.push(comp.as_os_str());
                if let Some(label) = prefix.take() {
                    out.push((label.into(), acc.to_string_lossy().into_owned()));
                }
            }
            Component::Normal(n) => {
                acc.push(n);
                out.push((n.to_string_lossy().into_owned().into(), acc.to_string_lossy().into_owned()));
            }
            _ => {}
        }
    }
    if out.is_empty() {
        out.push((path.to_string().into(), path.to_string()));
    }
    out
}

/// The name a new copy gets beside the original: "name (2).ext", "name (3).ext"…
pub fn unique_name(dir: &str, name: &str) -> String {
    let p = Path::new(dir).join(name);
    if !p.exists() {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|n| !Path::new(dir).join(n).exists()).unwrap()
}

/// Where the name ends and the extension starts (rename selects only the name).
pub fn stem_len(name: &str, is_dir: bool) -> usize {
    if is_dir {
        return name.len();
    }
    match name.rfind('.') {
        Some(0) | None => name.len(),
        Some(i) => i,
    }
}

/// Characters Windows refuses in names.
pub fn bad_name(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("A name can't be empty");
    }
    if name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 32) {
        return Some("A name can't contain < > : \" / \\ | ? *");
    }
    None
}

/// Time since a moment, as "2 minutes ago".
pub fn ago(ms: i64) -> String {
    let secs = ((chrono::Utc::now().timestamp_millis() - ms) / 1000).max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86_399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86_400, "day"),
    }
}

fn plural(n: i64, what: &str) -> String {
    format!("{n} {what}{} ago", if n == 1 { "" } else { "s" })
}
