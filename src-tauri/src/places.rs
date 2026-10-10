use serde::Serialize;

use crate::paths;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownFolder {
    id: &'static str,
    name: &'static str,
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    pub path: String,
    label: String,
    total: u64,
    free: u64,
    pub removable: bool,
    fs: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WslDistro {
    name: String,
    path: String,
    is_default: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Places {
    home: String,
    known: Vec<KnownFolder>,
    drives: Vec<Drive>,
    wsl: Vec<WslDistro>,
}

pub fn known() -> Vec<KnownFolder> {
    let list: [(&str, &str, Option<std::path::PathBuf>); 7] = [
        ("home", "Home", dirs::home_dir()),
        ("desktop", "Desktop", dirs::desktop_dir()),
        ("documents", "Documents", dirs::document_dir()),
        ("downloads", "Downloads", dirs::download_dir()),
        ("pictures", "Pictures", dirs::picture_dir()),
        ("music", "Music", dirs::audio_dir()),
        ("videos", "Videos", dirs::video_dir()),
    ];
    list.into_iter()
        .filter_map(|(id, name, p)| {
            p.map(|p| KnownFolder { id, name, path: paths::normalize(&p.to_string_lossy()) })
        })
        .collect()
}

pub fn drives() -> Vec<Drive> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<Drive> = disks
        .list()
        .iter()
        .map(|d| Drive {
            path: paths::normalize(&d.mount_point().to_string_lossy()),
            label: d.name().to_string_lossy().into_owned(),
            total: d.total_space(),
            free: d.available_space(),
            removable: d.is_removable(),
            fs: d.file_system().to_string_lossy().into_owned(),
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

#[cfg(windows)]
pub fn wsl_distros() -> Vec<WslDistro> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let Ok(lxss) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Lxss") else {
        return vec![];
    };
    let default: String = lxss.get_value("DefaultDistribution").unwrap_or_default();
    let mut out: Vec<WslDistro> = lxss
        .enum_keys()
        .flatten()
        .filter_map(|guid| {
            let k = lxss.open_subkey(&guid).ok()?;
            let name: String = k.get_value("DistributionName").ok()?;
            if name.starts_with("docker-desktop") {
                return None;
            }
            Some(WslDistro { path: paths::wsl_root(&name), is_default: guid.eq_ignore_ascii_case(&default), name })
        })
        .collect();
    out.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    out
}

#[cfg(not(windows))]
pub fn wsl_distros() -> Vec<WslDistro> {
    vec![]
}

pub fn default_distro() -> Option<String> {
    wsl_distros().into_iter().next().map(|d| d.name)
}

pub fn all() -> Places {
    Places {
        home: dirs::home_dir().map(|p| paths::normalize(&p.to_string_lossy())).unwrap_or_default(),
        known: known(),
        drives: drives(),
        wsl: wsl_distros(),
    }
}
