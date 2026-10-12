//! Windows integration: what to open from the command line (Explorer-style
//! arguments), windows (every launch is its own window; any window can restart
//! itself as administrator in place), and the opt-in "default file manager"
//! switch, which points Win+E and folder double-clicks at Ferret through per-user
//! registry keys and can put everything back exactly as it was.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::State;

use crate::commands::AppState;
use crate::{db, paths, recycle};

type Res<T> = Result<T, String>;

// ---------- command line ----------

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTarget {
    /// Folder to open; empty = the default location (Win+E, "This PC").
    pub path: String,
    /// Item to select in it (`/select,` or a file path).
    pub select: Option<String>,
    /// The tab to show first (a window restarted as administrator).
    pub active: bool,
}

fn target(path: impl Into<String>, select: Option<String>) -> LaunchTarget {
    LaunchTarget { path: path.into(), select, active: false }
}

const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// `"C:\"` reaches us as `C:"` (the backslash escapes the quote); undo that and stray quotes.
fn clean(arg: &str) -> String {
    let mut s = arg.trim().trim_matches('"').to_string();
    if s.ends_with(':') && s.len() == 2 {
        s.push('\\');
    }
    s
}

/// Explorer-compatible arguments → folders to open. Understands plain paths (files
/// open their folder with the file selected), `/select,<path>`, `/e,` `/n,` `/root,`,
/// the Recycle Bin and This PC. Relative paths are resolved against `cwd`.
pub fn parse_args(args: &[String], cwd: &str) -> Vec<LaunchTarget> {
    let mut out = Vec::new();
    let mut select_next = false;
    let mut i = 0;
    while i < args.len() {
        let mut a = args[i].trim().to_string();
        i += 1;
        if a.is_empty() || a.starts_with("--") {
            continue;
        }
        // Flags may carry their value after the comma, or in the next argument.
        let mut select = select_next;
        select_next = false;
        loop {
            let lower = a.to_ascii_lowercase();
            let Some(flag) = ["/select,", "/e,", "/n,", "/root,", "/separate,"].iter().find(|f| lower.starts_with(**f)) else {
                break;
            };
            select |= *flag == "/select,";
            a = a[flag.len()..].trim().to_string();
        }
        if ["/e", "/n", "/separate", "/select", "/root"].contains(&a.to_ascii_lowercase().trim_end_matches(',')) {
            select_next = a.to_ascii_lowercase().starts_with("/select");
            continue;
        }
        let a = clean(&a);
        if a.is_empty() {
            if select {
                select_next = true;
            }
            continue;
        }
        if recycle::is_recycle(&a) {
            out.push(target(recycle::PATH, None));
            continue;
        }
        if a.eq_ignore_ascii_case(THIS_PC) || a.eq_ignore_ascii_case("shell:MyComputerFolder") {
            out.push(target("", None));
            continue;
        }
        let p = std::path::Path::new(&a);
        let full = if p.is_absolute() || a.starts_with("\\\\") { a.clone() } else { std::path::Path::new(cwd).join(p).to_string_lossy().into_owned() };
        let full = paths::long_form(&paths::normalize(&full));
        let is_dir = std::fs::metadata(paths::for_io(&full)).map(|m| m.is_dir()).unwrap_or(false);
        let is_archive = crate::archive::archive_kind(&full).is_some();
        if select || (!is_dir && !is_archive && std::path::Path::new(&paths::for_io(&full)).exists()) {
            match paths::parent(&full) {
                Some(dir) => out.push(target(dir, Some(full))),
                None => out.push(target(full, None)),
            }
        } else {
            out.push(target(full, None));
        }
    }
    out
}

/// This process's own arguments, handed out once (a page reload doesn't reopen them).
static LAUNCH: Mutex<Option<Vec<LaunchTarget>>> = Mutex::new(None);
/// Window placement handed over by a window that restarted itself as administrator.
static PLACEMENT: Mutex<Option<Restore>> = Mutex::new(None);

/// What a window passes to its elevated replacement (`--restore <base64 JSON>`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Restore {
    tabs: Vec<String>,
    active: usize,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    maximized: bool,
}

pub fn init_launch_targets() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let (targets, placement) = from_args(args, &cwd);
    *LAUNCH.lock().unwrap() = Some(targets);
    *PLACEMENT.lock().unwrap() = placement;
}

/// A window handed over by `restart_elevated` (`--restore …`) reopens its tabs and
/// placement; anything else is Explorer-style arguments.
fn from_args(mut args: Vec<String>, cwd: &str) -> (Vec<LaunchTarget>, Option<Restore>) {
    if let Some(i) = args.iter().position(|a| a == "--restore") {
        let payload = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        let restore = base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .ok()
            .and_then(|b| serde_json::from_slice::<Restore>(&b).ok());
        if let Some(r) = restore {
            let tabs = r.tabs.iter().enumerate().map(|(i, p)| LaunchTarget { path: p.clone(), select: None, active: i == r.active }).collect();
            return (tabs, Some(r));
        }
    }
    (parse_args(&args, cwd), None)
}

#[tauri::command]
pub fn launch_targets() -> Vec<LaunchTarget> {
    LAUNCH.lock().unwrap().take().unwrap_or_default()
}

/// Puts a restarted window exactly where the one it replaces was.
pub fn apply_placement(window: &tauri::WebviewWindow) {
    let Some(r) = PLACEMENT.lock().unwrap().clone() else { return };
    let _ = window.set_size(tauri::PhysicalSize::new(r.w, r.h));
    let _ = window.set_position(tauri::PhysicalPosition::new(r.x, r.y));
    if r.maximized {
        let _ = window.maximize();
    }
}

// ---------- default file manager ----------

const CLASSES: &str = r"Software\Classes";
/// "File Explorer" (Win+E opens a new window through this class).
const EXPLORER_CLSID: &str = r"Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";
const VERB: &str = "ferret";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultStatus {
    enabled: bool,
    /// The exe the registry points at (when enabled).
    target: Option<String>,
    /// This build runs from `target\debug` and needs the dev server: it can't be the default.
    dev_build: bool,
}

fn exe() -> Res<String> {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).map_err(|e| e.to_string())
}

#[cfg(windows)]
mod reg {
    use super::*;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    fn get(path: &str, name: &str) -> Option<String> {
        hkcu().open_subkey(path).ok()?.get_value::<String, _>(name).ok()
    }

    fn set(path: &str, name: &str, value: &str) -> Res<()> {
        let (k, _) = hkcu().create_subkey(path).map_err(|e| e.to_string())?;
        k.set_value(name, &value).map_err(|e| e.to_string())
    }

    fn restore(path: &str, name: &str, old: &Value) -> Res<()> {
        match old.as_str() {
            Some(v) => set(path, name, v),
            None => match hkcu().open_subkey_with_flags(path, winreg::enums::KEY_SET_VALUE) {
                Ok(k) => match k.delete_value(name) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.to_string()),
                },
                Err(_) => Ok(()),
            },
        }
    }

    fn delete_tree(path: &str) {
        let _ = hkcu().delete_subkey_all(path);
    }

    fn command(kind: &str) -> String {
        format!(r"{CLASSES}\{kind}\shell\{VERB}\command")
    }

    /// The exe our verb currently launches, if Ferret is the default.
    pub fn current_target() -> Option<String> {
        let def = get(&format!(r"{CLASSES}\Directory\shell"), "")?;
        if !def.eq_ignore_ascii_case(VERB) {
            return None;
        }
        let cmd = get(&command("Directory"), "")?;
        let exe = cmd.trim_start().strip_prefix('"')?.split('"').next()?.to_string();
        Some(exe)
    }

    /// Writes the keys and returns what they replaced (to restore later).
    pub fn enable(exe: &str, previous: Option<&Value>) -> Res<Value> {
        // Keep the very first backup if we're only refreshing the exe path.
        let backup = match previous {
            Some(b) => b.clone(),
            None => json!({
                "directory": get(&format!(r"{CLASSES}\Directory\shell"), ""),
                "drive": get(&format!(r"{CLASSES}\Drive\shell"), ""),
                "clsidExisted": hkcu().open_subkey(EXPLORER_CLSID).is_ok(),
                "clsidCommand": get(&format!(r"{EXPLORER_CLSID}\shell\opennewwindow\command"), ""),
                "clsidDelegate": get(&format!(r"{EXPLORER_CLSID}\shell\opennewwindow\command"), "DelegateExecute"),
            }),
        };
        for kind in ["Directory", "Drive"] {
            let verb = format!(r"{CLASSES}\{kind}\shell\{VERB}");
            set(&verb, "", "Open in Ferret")?;
            set(&verb, "Icon", &format!("\"{exe}\",0"))?;
            set(&command(kind), "", &format!("\"{exe}\" \"%1\""))?;
            set(&format!(r"{CLASSES}\{kind}\shell"), "", VERB)?;
        }
        let win_e = format!(r"{EXPLORER_CLSID}\shell\opennewwindow\command");
        set(&win_e, "", &format!("\"{exe}\""))?;
        set(&win_e, "DelegateExecute", "")?;
        Ok(backup)
    }

    pub fn disable(backup: Option<&Value>) -> Res<()> {
        let b = backup.cloned().unwrap_or(Value::Null);
        restore(&format!(r"{CLASSES}\Directory\shell"), "", &b["directory"])?;
        restore(&format!(r"{CLASSES}\Drive\shell"), "", &b["drive"])?;
        for kind in ["Directory", "Drive"] {
            delete_tree(&format!(r"{CLASSES}\{kind}\shell\{VERB}"));
        }
        if b["clsidExisted"].as_bool().unwrap_or(false) {
            let win_e = format!(r"{EXPLORER_CLSID}\shell\opennewwindow\command");
            restore(&win_e, "", &b["clsidCommand"])?;
            restore(&win_e, "DelegateExecute", &b["clsidDelegate"])?;
        } else {
            delete_tree(EXPLORER_CLSID);
        }
        Ok(())
    }

    /// Tells Explorer that file associations changed.
    pub fn notify() {
        use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

const BACKUP_KEY: &str = "explorerBackup";

fn status() -> DefaultStatus {
    #[cfg(windows)]
    let target = reg::current_target();
    #[cfg(not(windows))]
    let target = None;
    DefaultStatus { enabled: target.is_some(), target, dev_build: cfg!(debug_assertions) }
}

#[tauri::command]
pub fn default_manager_status() -> DefaultStatus {
    status()
}

#[tauri::command]
pub fn set_default_manager(state: State<'_, AppState>, enable: bool) -> Res<DefaultStatus> {
    log::info!("set default file manager: {enable}");
    #[cfg(windows)]
    {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let backup = db::kv_get(&conn, BACKUP_KEY);
        if enable {
            if cfg!(debug_assertions) {
                return Err("This is a development build. Build Ferret for release (npm run tauri build) and turn this on from that copy".into());
            }
            let saved = reg::enable(&exe()?, if status().enabled { backup.as_ref() } else { None })?;
            db::kv_set(&conn, BACKUP_KEY, &saved).map_err(|e| e.to_string())?;
        } else {
            reg::disable(backup.as_ref())?;
        }
        reg::notify();
    }
    #[cfg(not(windows))]
    let _ = (state, enable);
    Ok(status())
}

/// When Ferret is the default but the registry points at another copy of it (moved or
/// updated install), re-point it at this one. Dev builds never take over.
pub fn refresh_registration(state: &AppState) {
    #[cfg(windows)]
    {
        if cfg!(debug_assertions) {
            return;
        }
        let (Some(target), Ok(me)) = (reg::current_target(), exe()) else { return };
        if target.eq_ignore_ascii_case(&me) {
            return;
        }
        let Ok(conn) = state.db.lock() else { return };
        let backup = db::kv_get(&conn, BACKUP_KEY);
        if reg::enable(&me, backup.as_ref()).is_ok() {
            reg::notify();
        }
    }
    #[cfg(not(windows))]
    let _ = state;
}

// ---------- windows & administrator rights ----------
//
// Like Windows itself: every launch is a new, normal window; a window can restart
// itself as administrator in place (same tabs and position, after a UAC prompt), and
// windows opened from an elevated one are elevated too (children inherit the token).
// Ferret never keeps an elevated helper around and never changes permissions.

/// This window runs as administrator (badge, own WebView2 profile, no session saving).
pub fn admin_mode() -> bool {
    crate::shellops::is_elevated()
}

/// Whether no other Ferret window was open when this one started (only the first
/// window restores the last session). The mutex lives as long as the process and is
/// named per app identifier, so a dev build and the installed app don't count each other.
static FIRST_WINDOW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn init_window_registry(identifier: &str) {
    FIRST_WINDOW.get_or_init(|| {
        #[cfg(windows)]
        unsafe {
            use windows::core::PCWSTR;
            use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
            use windows::Win32::System::Threading::CreateMutexW;
            let name: Vec<u16> = format!("Local\\FerretWindows-{identifier}").encode_utf16().chain(Some(0)).collect();
            match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
                // Kept open on purpose: the name must exist until this process exits.
                Ok(_handle) => GetLastError() != ERROR_ALREADY_EXISTS,
                // Created by an elevated window and not openable from here: one exists.
                Err(_) => false,
            }
        }
        #[cfg(not(windows))]
        true
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    elevated: bool,
    first_window: bool,
}

#[tauri::command]
pub fn window_info() -> WindowInfo {
    WindowInfo { elevated: admin_mode(), first_window: *FIRST_WINDOW.get().unwrap_or(&true) }
}

/// Opens another Ferret window (elevated if this one is, like any program it starts).
#[tauri::command]
pub fn new_window(path: Option<String>) -> Res<()> {
    let mut cmd = std::process::Command::new(exe()?);
    if let Some(p) = path {
        cmd.arg(paths::for_io(&p));
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Restarts this window as administrator: Windows shows the UAC prompt, then an elevated
/// copy opens with the same tabs in the same place. Returns once it's on screen, so the
/// caller can close this one; "Cancelled" if the prompt was declined.
#[tauri::command]
pub async fn restart_elevated(window: tauri::WebviewWindow, tabs: Vec<String>, active: usize) -> Res<()> {
    let maximized = window.is_maximized().unwrap_or(false);
    let pos = window.outer_position().map_err(|e| e.to_string())?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    let restore = Restore { tabs, active, x: pos.x, y: pos.y, w: size.width, h: size.height, maximized };
    let payload = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&restore).map_err(|e| e.to_string())?);
    log::info!("restarting as administrator");
    let owner = window.hwnd().ok().map(|h| h.0 as isize);
    tauri::async_runtime::spawn_blocking(move || -> Res<()> {
        let pid = run_elevated(&format!("--restore {payload}"), owner)?;
        wait_for_window(pid, std::time::Duration::from_secs(30));
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    // The elevated copy is on screen in the same place: this one goes away, so it looks
    // like the same window becoming administrator.
    let _ = window.destroy();
    Ok(())
}

/// Starts this exe elevated (UAC prompt) and returns its process id.
#[cfg(windows)]
fn run_elevated(params: &str, owner: Option<isize>) -> Res<u32> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::GetProcessId;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file, params) = (HSTRING::from("runas"), HSTRING::from(exe()?), HSTRING::from(params));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS,
        hwnd: owner.map(|h| HWND(h as *mut _)).unwrap_or_default(),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| {
        // ERROR_CANCELLED: the UAC prompt was declined.
        if e.code().0 as u32 == 0x8007_04C7 { "Cancelled".to_string() } else { e.message().to_string() }
    })?;
    if info.hProcess.is_invalid() {
        return Err("Windows didn't start the administrator window".into());
    }
    let pid = unsafe { GetProcessId(info.hProcess) };
    unsafe {
        let _ = CloseHandle(info.hProcess);
    }
    Ok(pid)
}

#[cfg(not(windows))]
fn run_elevated(_params: &str, _owner: Option<isize>) -> Res<u32> {
    Err("Not supported".into())
}

/// Waits until process `pid` shows a window (or `timeout` passes).
#[cfg(windows)]
fn wait_for_window(pid: u32, timeout: std::time::Duration) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};
    unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lp.0 as *mut (u32, bool)) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if pid == state.0 && unsafe { IsWindowVisible(h) }.as_bool() {
            state.1 = true;
            return BOOL(0);
        }
        BOOL(1)
    }
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let mut state = (pid, false);
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut state as *mut _ as isize));
        }
        if state.1 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
fn wait_for_window(_pid: u32, _timeout: std::time::Duration) {}

/// "Open in Explorer" must really open Explorer, even when Ferret owns folder double-clicks.
#[tauri::command]
pub fn open_in_explorer(path: String) -> Res<()> {
    let target = if recycle::is_recycle(&path) { "shell:RecycleBinFolder".to_string() } else { paths::for_io(&path).trim_start_matches(r"\\?\").to_string() };
    std::process::Command::new("explorer.exe").arg(target).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(path: &str, select: Option<&str>) -> LaunchTarget {
        target(path, select.map(Into::into))
    }

    #[test]
    fn explorer_style_arguments() {
        let dir = std::env::temp_dir().join("ferret-args-test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.txt");
        std::fs::write(&file, "x").unwrap();
        let d = paths::long_form(&paths::normalize(&dir.to_string_lossy()));
        let f = paths::long_form(&paths::normalize(&file.to_string_lossy()));
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(parse_args(&args(&[&d]), "C:\\"), vec![t(&d, None)]);
        assert_eq!(parse_args(&args(&[&f]), "C:\\"), vec![t(&d, Some(&f))]);
        assert_eq!(parse_args(&args(&[&format!("/select,{f}")]), "C:\\"), vec![t(&d, Some(&f))]);
        assert_eq!(parse_args(&args(&["/select,", &f]), "C:\\"), vec![t(&d, Some(&f))]);
        assert_eq!(parse_args(&args(&[&format!("/e,/root,{d}")]), "C:\\"), vec![t(&d, None)]);
        assert_eq!(parse_args(&args(&["C:\""]), "C:\\"), vec![t("C:\\", None)]);
        assert_eq!(parse_args(&args(&["a b.txt"]), &d), vec![t(&d, Some(&f))]);
        assert_eq!(parse_args(&args(&["shell:RecycleBinFolder"]), "C:\\"), vec![t(recycle::PATH, None)]);
        assert_eq!(parse_args(&args(&[THIS_PC]), "C:\\"), vec![t("", None)]);
        assert_eq!(parse_args(&args(&["--no-default-features"]), "C:\\"), vec![]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A window restarted as administrator gets back its tabs, active tab and placement.
    #[test]
    fn restart_handoff_round_trip() {
        let r = Restore { tabs: vec![r"C:\Users".into(), r"C:\Program Files".into()], active: 1, x: -8, y: 20, w: 1320, h: 820, maximized: true };
        let payload = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&r).unwrap());
        let (targets, placement) = from_args(vec!["--restore".into(), payload], "C:\\");
        assert_eq!(placement, Some(r));
        assert_eq!(targets.iter().map(|t| (t.path.as_str(), t.active)).collect::<Vec<_>>(), vec![(r"C:\Users", false), (r"C:\Program Files", true)]);
        // A broken payload falls back to plain arguments instead of failing.
        let (targets, placement) = from_args(vec!["--restore".into(), "not base64!".into()], "C:\\");
        assert!(placement.is_none() && targets.is_empty());
    }
}
