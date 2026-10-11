//! Running programs inside WSL distros. Walking a distro's files is fast from Linux and slow
//! through `\\wsl.localhost` from Windows, so folder sizes and search ask Linux directly.

use std::process::Command;

use crate::paths;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `\\wsl.localhost\Distro\home\me` → `("Distro", "/home/me")`.
pub fn linux_path(p: &str) -> Option<(String, String)> {
    let n = paths::normalize(p);
    let distro = paths::wsl_distro(&n)?;
    let rest = &n[paths::wsl_root(&distro).len()..];
    let linux = if rest.trim_matches('\\').is_empty() { "/".to_string() } else { rest.replace('\\', "/") };
    Some((distro, linux))
}

/// `program` inside `distro` (no shell, no console window), as the distro's default user.
pub fn command(distro: &str, program: &str) -> Command {
    let mut c = Command::new("wsl.exe");
    c.args(["-d", distro, "--exec", program]);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Distros running right now. Never starts one.
pub fn running() -> Vec<String> {
    let mut c = Command::new("wsl.exe");
    c.args(["--list", "--quiet", "--running"]);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    let Ok(out) = c.output() else { return vec![] };
    // wsl.exe writes UTF-16; with nothing running it prints a message, so only names of
    // installed distros count.
    let units: Vec<u16> = out.stdout.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    let installed: Vec<String> = crate::places::wsl_distros().into_iter().map(|d| d.name).collect();
    text.lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}').trim_matches('\0').trim())
        .filter(|l| installed.iter().any(|d| d == l))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn linux_paths() {
        assert_eq!(super::linux_path(r"\\wsl.localhost\Arch"), Some(("Arch".into(), "/".into())));
        assert_eq!(super::linux_path(r"\\wsl$\Arch\home\me"), Some(("Arch".into(), "/home/me".into())));
        assert_eq!(super::linux_path(r"C:\Users"), None);
    }
}
