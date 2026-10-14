//! Explorer redirect. Folder double-clicks and Win+E reach Ferret through the registry
//! (integration.rs), but programs that run `explorer.exe /select,<file>` ("Show in
//! folder", "Open containing folder"…) always get an Explorer window: nothing in the
//! registry can change that. While Ferret is the default file manager, a small background
//! copy of Ferret (`--explorer-redirect`, started at sign-in) watches for new Explorer
//! windows, reads their folder and selected item, closes them and opens Ferret there.
//!
//! It only listens to window events (nothing is loaded into other processes), runs with
//! the user's normal rights, leaves Control Panel and other non-folder windows alone, lets
//! Ferret's own "Open in Explorer" through, and exits once Ferret stops being the default.

use std::time::{Duration, Instant};

/// Command-line switch that runs this process as the redirector instead of a window.
pub const ARG: &str = "--explorer-redirect";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FerretExplorerRedirect";
/// Where Ferret says "the next Explorer window is meant to be Explorer" (a deadline, ms).
const PASS_KEY: &str = r"Software\Ferret";
const PASS_VALUE: &str = "ExplorerPassUntil";

/// Starts the redirector at every sign-in, and now.
#[cfg(windows)]
pub fn install(exe: &str) -> Result<(), String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY).map_err(|e| e.to_string())?;
    key.set_value(RUN_VALUE, &format!("\"{exe}\" {ARG}")).map_err(|e| e.to_string())?;
    start(exe);
    Ok(())
}

/// Starts the redirector unless one is already running (it checks that itself).
#[cfg(windows)]
pub fn start(exe: &str) {
    if let Err(e) = std::process::Command::new(exe).arg(ARG).spawn() {
        log::warn!("can't start the Explorer redirect: {e}");
    }
}

/// Stops starting it at sign-in. A running one notices Ferret isn't the default and exits.
#[cfg(windows)]
pub fn uninstall() {
    use winreg::{enums::*, RegKey};
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
        let _ = key.delete_value(RUN_VALUE);
    }
}

/// Lets the next Explorer window (opened within a few seconds) stay Explorer.
#[cfg(windows)]
pub fn let_explorer_through() {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(PASS_KEY) {
        let _ = key.set_value(PASS_VALUE, &(now_ms() + 10_000));
    }
}

#[cfg(not(windows))]
pub fn let_explorer_through() {}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Whether Ferret asked to let this window through (used up once it is).
#[cfg(windows)]
fn take_pass() -> bool {
    use winreg::{enums::*, RegKey};
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(PASS_KEY, KEY_READ | KEY_SET_VALUE) else { return false };
    let Ok(until) = key.get_value::<u64, _>(PASS_VALUE) else { return false };
    let _ = key.delete_value(PASS_VALUE);
    now_ms() < until
}

/// What an Explorer window showed, in Ferret's command-line terms.
#[derive(Debug, PartialEq)]
enum Found {
    /// Not a folder Ferret opens (Control Panel, Home, libraries, Network…): leave it.
    Keep,
    Open { folder: String, select: Option<String> },
}

const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// Explorer's parsing name for its folder → whether (and where) Ferret takes over.
fn classify(folder: &str, select: Option<String>) -> Found {
    let f = folder.trim();
    let drive = f.len() >= 3 && f.as_bytes()[0].is_ascii_alphabetic() && &f[1..3] == ":\\";
    if drive || (f.starts_with(r"\\") && !f.starts_with(r"\\?\")) {
        return Found::Open { folder: f.to_string(), select };
    }
    if f.eq_ignore_ascii_case(THIS_PC) {
        return Found::Open { folder: THIS_PC.into(), select: None };
    }
    if crate::recycle::is_recycle(f) {
        return Found::Open { folder: crate::recycle::PATH.into(), select: None };
    }
    Found::Keep
}

/// Ferret's arguments for what was found.
fn ferret_args(found: &Found) -> Vec<String> {
    match found {
        Found::Keep => vec![],
        Found::Open { select: Some(item), .. } => vec![format!("/select,{item}")],
        Found::Open { folder, select: None } => vec![folder.clone()],
    }
}

// ---------- the background process ----------

/// Runs until Ferret stops being the default file manager.
#[cfg(windows)]
pub fn run() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
    use windows::Win32::UI::Accessibility::SetWinEventHook;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // One per sign-in session.
    let name: Vec<u16> = "Local\\FerretExplorerRedirect".encode_utf16().chain(Some(0)).collect();
    let _mutex = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(h) if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS => h,
        _ => return,
    };
    if crate::integration::default_target().is_none() {
        return;
    }
    say("started");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        MAIN_THREAD.store(GetCurrentThreadId(), std::sync::atomic::Ordering::Relaxed);
        // Create (hide it before it's ever drawn), destroy (forget it), show (take it over).
        let hook = SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        if hook.is_invalid() {
            say("can't watch windows");
            return;
        }
        // Checks now and then that Ferret is still the default.
        SetTimer(None, 0, 5_000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_TIMER if crate::integration::default_target().is_none() => break,
                WM_EXPLORER_SHOWN => handle(HWND(msg.lParam.0 as *mut _)),
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    say("stopped: Ferret is no longer the default file manager");
}

#[cfg(not(windows))]
pub fn run() {}

#[cfg(windows)]
static MAIN_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(windows)]
const WM_EXPLORER_SHOWN: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

// Explorer windows between "created" and "handled" (event callbacks and the main loop run
// on the same thread).
#[cfg(windows)]
thread_local! {
    /// Hidden at creation, waiting to be shown and read.
    static VEILED: std::cell::RefCell<std::collections::HashMap<isize, Veil>> = Default::default();
    /// Opened by Ferret's "Open in Explorer": stay Explorer.
    static PASSED: std::cell::RefCell<std::collections::HashSet<isize>> = Default::default();
}

/// Window events for Explorer folder windows. A new one is made invisible right when it's
/// created, before Windows draws or animates it; once shown it's read on the main loop
/// (that takes COM calls, which shouldn't run inside the event callback).
#[cfg(windows)]
unsafe extern "system" fn on_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: windows::Win32::Foundation::HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let key = hwnd.0 as isize;
    if event == EVENT_OBJECT_DESTROY {
        // Too late to check its class; drop whatever we kept for it.
        VEILED.with_borrow_mut(|v| v.remove(&key).map(Veil::keep));
        PASSED.with_borrow_mut(|p| p.remove(&key));
        return;
    }
    let mut class = [0u16; 32];
    let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    if String::from_utf16_lossy(&class[..n]) != "CabinetWClass" {
        return;
    }
    if event == EVENT_OBJECT_CREATE {
        if crate::integration::default_target().is_none() {
            return;
        }
        if take_pass() {
            PASSED.with_borrow_mut(|p| p.insert(key));
        } else {
            VEILED.with_borrow_mut(|v| v.insert(key, Veil::put(hwnd)));
        }
        return;
    }
    if event == EVENT_OBJECT_SHOW {
        let tid = MAIN_THREAD.load(std::sync::atomic::Ordering::Relaxed);
        let _ = unsafe { PostThreadMessageW(tid, WM_EXPLORER_SHOWN, WPARAM(0), LPARAM(key)) };
    }
}

#[cfg(windows)]
fn handle(hwnd: windows::Win32::Foundation::HWND) {
    let key = hwnd.0 as isize;
    if PASSED.with_borrow_mut(|p| p.remove(&key)) {
        say("Explorer window opened by Ferret: left alone");
        return;
    }
    // Shown again later (an Explorer window can be hidden and shown): nothing to do.
    let Some(veil) = VEILED.with_borrow_mut(|v| v.remove(&key)) else { return };
    let Some(ferret) = crate::integration::default_target() else { return };
    let Some(browser) = find_browser(hwnd, Duration::from_secs(3)) else {
        say("new Explorer window, but no folder in it: left alone");
        return;
    };
    let found = read_window(&browser, Duration::from_millis(2_500));
    match &found {
        Some(Found::Open { .. }) => {}
        other => {
            say(&format!("Explorer window left alone: {other:?}"));
            return;
        }
    }
    let found = found.unwrap();
    let args = ferret_args(&found);
    match std::process::Command::new(&ferret).args(&args).spawn() {
        Ok(_) => {
            say(&format!("opened in Ferret: {args:?}"));
            veil.keep();
            let _ = unsafe { browser.Quit() };
        }
        Err(e) => say(&format!("can't start Ferret ({ferret}): {e}")),
    }
}

/// Makes a window fully transparent and keeps it off the taskbar (a tool window gets no
/// button); puts it back on drop unless told to keep it so.
#[cfg(windows)]
struct Veil {
    hwnd: windows::Win32::Foundation::HWND,
    ex_style: isize,
    keep: bool,
}

#[cfg(windows)]
impl Veil {
    fn put(hwnd: windows::Win32::Foundation::HWND) -> Veil {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | (WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0) as isize);
            let _ = SetLayeredWindowAttributes(hwnd, Default::default(), 0, LWA_ALPHA);
        }
        Veil { hwnd, ex_style, keep: false }
    }

    fn keep(mut self) {
        self.keep = true;
    }
}

#[cfg(windows)]
impl Drop for Veil {
    fn drop(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::*;
        if self.keep {
            return;
        }
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd, Default::default(), 255, LWA_ALPHA);
            // The taskbar only notices the style change when the window is shown again.
            let visible = IsWindowVisible(self.hwnd).as_bool();
            if visible {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, self.ex_style);
            if visible {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }
    }
}

/// The shell's automation object for the Explorer window `hwnd` (it registers a moment
/// after the window shows).
#[cfg(windows)]
fn find_browser(hwnd: windows::Win32::Foundation::HWND, timeout: Duration) -> Option<windows::Win32::UI::Shell::IWebBrowserApp> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{IShellWindows, IWebBrowserApp, ShellWindows};
    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }.ok()?;
    let start = Instant::now();
    while start.elapsed() < timeout {
        let count = unsafe { windows.Count() }.unwrap_or(0);
        for i in 0..count {
            let Ok(disp) = (unsafe { windows.Item(&VARIANT::from(i)) }) else { continue };
            let Ok(app) = disp.cast::<IWebBrowserApp>() else { continue };
            if unsafe { app.HWND() }.map(|h| h.0 as isize == hwnd.0 as isize).unwrap_or(false) {
                return Some(app);
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    None
}

/// The window's folder and selected item. `/select` applies its selection once the folder
/// is listed, so this waits a little for one before settling for the folder alone.
#[cfg(windows)]
fn read_window(app: &windows::Win32::UI::Shell::IWebBrowserApp, timeout: Duration) -> Option<Found> {
    const SETTLE: Duration = Duration::from_millis(350);
    let start = Instant::now();
    let mut folder: Option<(String, Instant)> = None;
    while start.elapsed() < timeout {
        if let Some(view) = folder_view(app) {
            if let Some(path) = view_folder(&view) {
                if matches!(classify(&path, None), Found::Keep) {
                    return Some(Found::Keep);
                }
                let since = folder.get_or_insert_with(|| (path.clone(), Instant::now())).1;
                if let Some(sel) = first_selected(&view) {
                    return Some(classify(&path, Some(sel)));
                }
                if since.elapsed() >= SETTLE {
                    return Some(classify(&path, None));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    folder.map(|(p, _)| classify(&p, None))
}

#[cfg(windows)]
fn folder_view(app: &windows::Win32::UI::Shell::IWebBrowserApp) -> Option<windows::Win32::UI::Shell::IFolderView> {
    use windows::core::Interface;
    use windows::Win32::System::Com::IServiceProvider;
    use windows::Win32::UI::Shell::{IFolderView, IShellBrowser, SID_STopLevelBrowser};
    let sp: IServiceProvider = app.cast().ok()?;
    let browser: IShellBrowser = unsafe { sp.QueryService(&SID_STopLevelBrowser) }.ok()?;
    unsafe { browser.QueryActiveShellView() }.ok()?.cast::<IFolderView>().ok()
}

#[cfg(windows)]
fn view_folder(view: &windows::Win32::UI::Shell::IFolderView) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{IPersistFolder2, SHGetNameFromIDList, SIGDN_DESKTOPABSOLUTEPARSING};
    let pf: IPersistFolder2 = unsafe { view.GetFolder() }.ok()?;
    let pidl = unsafe { pf.GetCurFolder() }.ok()?;
    let name = unsafe { SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING) };
    unsafe { CoTaskMemFree(Some(pidl as *const _)) };
    let name = name.ok()?;
    let s = unsafe { name.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    s.filter(|s| !s.is_empty())
}

#[cfg(windows)]
fn first_selected(view: &windows::Win32::UI::Shell::IFolderView) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{IShellItemArray, SIGDN_DESKTOPABSOLUTEPARSING, SVGIO_SELECTION};
    let items: IShellItemArray = unsafe { view.Items(SVGIO_SELECTION) }.ok()?;
    if unsafe { items.GetCount() }.ok()? == 0 {
        return None;
    }
    let name = unsafe { items.GetItemAt(0).ok()?.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }.ok()?;
    let s = unsafe { name.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
    s
}

/// The redirector has no window and no Tauri logger: it keeps a small log of its own
/// next to Ferret's.
fn say(msg: &str) {
    use std::io::Write;
    let Some(dir) = dirs::data_local_dir() else { return };
    let dir = dir.join(if cfg!(debug_assertions) { "com.ferret.dev" } else { "com.ferret.app" }).join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("explorer-redirect.log");
    if std::fs::metadata(&file).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&file, dir.join("explorer-redirect.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        let _ = writeln!(f, "{} [{}] {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), std::process::id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_windows_ferret_takes_over() {
        let open = |f: &str, s: Option<&str>| Found::Open { folder: f.into(), select: s.map(Into::into) };
        assert_eq!(classify(r"C:\Users", None), open(r"C:\Users", None));
        assert_eq!(classify(r"D:\", Some(r"D:\a.txt".into())), open(r"D:\", Some(r"D:\a.txt")));
        assert_eq!(classify(r"\\wsl.localhost\Arch\home", None), open(r"\\wsl.localhost\Arch\home", None));
        assert_eq!(classify(THIS_PC, None), open(THIS_PC, None));
        assert_eq!(classify("::{645FF040-5081-101B-9F08-00AA002F954E}", None), open(crate::recycle::PATH, None));
        // Control Panel, Home, libraries and Network stay in Explorer.
        assert_eq!(classify("::{26EE0668-A00A-44D7-9371-BEB064C98683}\\0", None), Found::Keep);
        assert_eq!(classify("::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}", None), Found::Keep);
        assert_eq!(classify("::{031E4825-7B94-4DC3-B131-E946B44C8DD5}\\Documents.library-ms", None), Found::Keep);
        assert_eq!(ferret_args(&open(r"C:\x", Some(r"C:\x\a b.txt"))), vec![r"/select,C:\x\a b.txt".to_string()]);
        assert_eq!(ferret_args(&open(r"C:\x", None)), vec![r"C:\x".to_string()]);
    }
}
