use serde::Serialize;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::paths;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub ext: String,
    /// Lives inside an archive (virtual path like `C:\a.zip\dir\file`).
    pub in_archive: bool,
    /// Recycle Bin items: the folder the item was deleted from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

fn ms(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

#[cfg(windows)]
fn hidden_attr(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn hidden_attr(_m: &fs::Metadata) -> bool {
    false
}

pub fn list(dir: &str) -> Result<Vec<Entry>, String> {
    let wsl = paths::is_wsl(dir);
    let rd = fs::read_dir(paths::for_io(dir)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(meta) = item.metadata() else { continue };
        let is_symlink = meta.file_type().is_symlink();
        // Follow links/junctions only to learn whether they point at a folder.
        let is_dir = if is_symlink {
            fs::metadata(item.path()).map(|m| m.is_dir()).unwrap_or(false)
        } else {
            meta.is_dir()
        };
        let ext = if is_dir {
            String::new()
        } else {
            name.rsplit_once('.')
                .filter(|(stem, _)| !stem.is_empty())
                .map(|(_, e)| e.to_lowercase())
                .unwrap_or_default()
        };
        out.push(Entry {
            hidden: hidden_attr(&meta) || (wsl && name.starts_with('.')),
            path: join(dir, &name),
            size: if is_dir { 0 } else { meta.len() },
            modified: ms(meta.modified()),
            created: ms(meta.created()),
            name,
            is_dir,
            is_symlink,
            ext,
            in_archive: false,
            origin: None,
        });
    }
    Ok(out)
}

/// Lists `dir`, returning the folder actually listed. Windows' compatibility junctions
/// ("Local Settings", "My Documents", "Application Data"…) refuse listing to everyone,
/// administrators included, so for a link that can't be listed we open where it points.
/// A folder that is really off-limits fails with `denied:<path>` (the UI says so; Ferret
/// never changes permissions).
pub fn list_following(dir: &str) -> Result<(String, Vec<Entry>), String> {
    let denied = |p: &str| matches!(fs::read_dir(paths::for_io(p)), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied);
    if !denied(dir) {
        return list(dir).map(|e| (dir.to_string(), e));
    }
    if let Ok(target) = fs::read_link(paths::for_io(dir)) {
        let t = target.to_string_lossy();
        let t = match t.strip_prefix(r"\\?\UNC\") {
            Some(unc) => format!(r"\\{unc}"),
            None => t.strip_prefix(r"\\?\").unwrap_or(&t).to_string(),
        };
        let t = paths::normalize(&t);
        if paths::key(&t) != paths::key(dir) && !denied(&t) {
            return list(&t).map(|e| (t, e));
        }
    }
    Err(format!("denied:{}", paths::normalize(dir)))
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

const CODE_MARKERS: &[&str] = &[
    ".git", "package.json", "cargo.toml", "pyproject.toml", "go.mod", "pom.xml",
    "build.gradle", "makefile", "cmakelists.txt", "deno.json", "composer.json",
];
const MEDIA_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "bmp", "tif", "tiff", "raw", "cr2",
    "nef", "arw", "dng", "svg", "mp4", "mkv", "mov", "avi", "webm", "m4v", "wmv",
];

/// Which folder-type profile a listing looks like.
pub fn detect_profile(dir: &str, entries: &[Entry]) -> Option<&'static str> {
    if let Some(dl) = dirs::download_dir() {
        if paths::key(&dl.to_string_lossy()) == paths::key(dir) {
            return Some("downloads");
        }
    }
    if entries
        .iter()
        .any(|e| CODE_MARKERS.contains(&e.name.to_lowercase().as_str()))
    {
        return Some("code");
    }
    let files: Vec<&Entry> = entries.iter().filter(|e| !e.is_dir && !e.hidden).collect();
    let media = files.iter().filter(|e| MEDIA_EXT.contains(&e.ext.as_str())).count();
    if files.len() >= 4 && media * 10 >= files.len() * 6 {
        return Some("media");
    }
    None
}

#[cfg(all(test, windows))]
mod tests {
    use crate::paths;

    /// "Local Settings" denies listing to everyone; Ferret opens AppData\Local instead.
    #[test]
    fn follows_compatibility_junctions() {
        let j = dirs::home_dir().unwrap().join("Local Settings");
        if !j.exists() {
            return;
        }
        let (p, entries) = super::list_following(&paths::normalize(&j.to_string_lossy())).unwrap();
        assert_eq!(paths::key(&p), paths::key(&dirs::data_local_dir().unwrap().to_string_lossy()));
        assert!(!entries.is_empty());
    }

    /// A real folder that refuses listing reports "denied:<path>".
    #[test]
    fn denied_folder_says_denied() {
        let dir = std::env::temp_dir().join(format!("ferret-denied-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = paths::normalize(&dir.to_string_lossy());
        let sid = crate::recycle::user_sid().unwrap();
        let icacls = |args: &[&str]| std::process::Command::new("icacls").arg(&d).args(args).output().unwrap().status.success();
        assert!(icacls(&["/deny", &format!("*{sid}:(RD)")]));
        let res = super::list_following(&d);
        assert!(icacls(&["/remove:d", &format!("*{sid}")]));
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(res.err(), Some(format!("denied:{d}")));
    }
}
