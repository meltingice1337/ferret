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
//!
//! Hiding and reading a window both wait on Explorer, seconds for a big folder, and people
//! press "Show in folder" again meanwhile. So the event callback never waits: each new window
//! gets a thread of its own, the callback only tells it what happened, and the same item asked
//! for again brings up the window already opened for it instead of another one.

use std::time::{Duration, Instant};

/// Command-line switch that runs this process as the redirector instead of a window.
pub const ARG: &str = "--explorer-redirect";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FerretExplorerRedirect";
/// Where Ferret says "the next Explorer window is meant to be Explorer" (a deadline, ms).
const PASS_KEY: &str = r"Software\Ferret";
const PASS_VALUE: &str = "ExplorerPassUntil";
/// Held by the running redirector (one per sign-in session).
const MUTEX: &str = "Local\\FerretExplorerRedirect";

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

/// Waits for a running redirector to exit (it checks every few seconds whether Ferret is
/// still the default).
#[cfg(windows)]
pub fn wait_until_stopped(timeout: Duration) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name: Vec<u16> = MUTEX.encode_utf16().chain(Some(0)).collect();
    let start = Instant::now();
    while start.elapsed() < timeout {
        match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) } {
            Ok(h) => unsafe {
                let _ = CloseHandle(h);
            },
            Err(_) => return,
        }
        std::thread::sleep(Duration::from_millis(250));
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
    /// Not a folder Ferret opens (Control Panel, libraries, Network…): leave it.
    Keep,
    Open { folder: String, select: Option<String> },
    /// Explorer's Home: what Win+E and the taskbar's Explorer open. Ferret starts as it does on
    /// its own. Win+E is also set to start Ferret directly (the registry's `opennewwindow`
    /// command), but Windows 26300.9974 (2026-11-12) opened Explorer's Home regardless, with
    /// the registry as it should be: this catches it either way.
    Start,
}

const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
const HOME: &str = "::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}";

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
    if f.eq_ignore_ascii_case(HOME) {
        return Found::Start;
    }
    Found::Keep
}

/// Ferret's arguments for what was found.
fn ferret_args(found: &Found) -> Vec<String> {
    match found {
        Found::Keep | Found::Start => vec![],
        Found::Open { select: Some(item), .. } => vec![format!("/select,{item}")],
        Found::Open { folder, select: None } => vec![folder.clone()],
    }
}

/// The same thing asked for again this soon means "where is it?", not "open another one".
const REPEAT_WITHIN: Duration = Duration::from_secs(10);

/// Whether this request repeats the last one, whose window is still open. Never with no
/// arguments: that's Win+E (Explorer's Home), and each press is another window, as it was
/// when Windows started Ferret for it directly.
fn is_repeat(last: &[String], age: Duration, still_open: bool, args: &[String]) -> bool {
    !args.is_empty() && still_open && age < REPEAT_WITHIN && last == args
}

// ---------- the background process ----------

/// Runs until Ferret stops being the default file manager.
#[cfg(windows)]
pub fn run() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::Accessibility::SetWinEventHook;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // One per sign-in session.
    let name: Vec<u16> = MUTEX.encode_utf16().chain(Some(0)).collect();
    let _mutex = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(h) if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS => h,
        _ => return,
    };
    if crate::integration::default_target().is_none() {
        return;
    }
    say("started");
    unsafe {
        // Create (hide it before it's ever drawn), destroy (forget it), show (take it over).
        let hook = SetWinEventHook(EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        if hook.is_invalid() {
            say("can't watch windows");
            return;
        }
        note_existing_windows();
        // Checks now and then that Ferret is still the default.
        SetTimer(None, 0, 5_000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER && crate::integration::default_target().is_none() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Windows Explorer keeps hidden for later must not stay invisible once nobody watches them:
    // their threads put them back when they hear nothing more.
    drop(WATCHED.with_borrow_mut(std::mem::take));
    std::thread::sleep(Duration::from_millis(500));
    say("stopped: Ferret is no longer the default file manager");
}

#[cfg(not(windows))]
pub fn run() {}

/// What a window's thread hears from the event callback.
#[cfg(windows)]
enum Heard {
    Shown,
    Gone,
}

// Explorer windows on their way through (only the event callback touches these, on the thread
// that set the hook).
#[cfg(windows)]
thread_local! {
    /// Each with a thread of its own, until they're gone.
    static WATCHED: std::cell::RefCell<std::collections::HashMap<isize, std::sync::mpsc::Sender<Heard>>> = Default::default();
    /// Stay Explorer until they're gone: opened by Ferret's "Open in Explorer", or already open
    /// when this run started.
    static PASSED: std::cell::RefCell<std::collections::HashSet<isize>> = Default::default();
}

#[cfg(windows)]
fn is_explorer_window(hwnd: windows::Win32::Foundation::HWND) -> bool {
    let mut class = [0u16; 32];
    let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class) } as usize;
    String::from_utf16_lossy(&class[..n]) == "CabinetWClass"
}

/// Explorer windows from before this run. Open ones the user can see are theirs and stay. A
/// hidden one is Explorer's, kept for later, and is treated like a new one when it's shown. An
/// open one that's fully transparent is of no use to anyone: a version of this helper that
/// lost track of it left it behind.
#[cfg(windows)]
fn note_existing_windows() {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, TRUE, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe extern "system" fn look(hwnd: HWND, _: LPARAM) -> BOOL {
        if !is_explorer_window(hwnd) {
            return TRUE;
        }
        let key = hwnd.0 as isize;
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            watch(key);
        } else if let Some(veil) = Veil::left_behind(hwnd) {
            say("closing an invisible Explorer window left behind earlier");
            veil.keep();
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        } else {
            PASSED.with_borrow_mut(|p| p.insert(key));
        }
        TRUE
    }
    let _ = unsafe { EnumWindows(Some(look), LPARAM(0)) };
}

/// Window events for Explorer folder windows. Windows doesn't keep them in order: with a few
/// windows opening at once, "shown" has arrived 0.8 s before "created". So whichever comes first
/// starts the window's thread, which hides it right away, before it's drawn if it can.
///
/// Nothing here may wait on another program. While a call does (hiding a window sends it
/// messages), Windows can call this again with the next event or hold it back.
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
    use windows::Win32::UI::WindowsAndMessaging::*;
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let key = hwnd.0 as isize;
    if event == EVENT_OBJECT_DESTROY {
        // Too late to check its class; drop whatever we kept for it.
        if let Some(thread) = WATCHED.with_borrow_mut(|w| w.remove(&key)) {
            let _ = thread.send(Heard::Gone);
        }
        PASSED.with_borrow_mut(|p| p.remove(&key));
        return;
    }
    if !is_explorer_window(hwnd) || PASSED.with_borrow(|p| p.contains(&key)) {
        return;
    }
    if !WATCHED.with_borrow(|w| w.contains_key(&key)) {
        if crate::integration::default_target().is_none() {
            return;
        }
        if take_pass() {
            PASSED.with_borrow_mut(|p| p.insert(key));
            say("Explorer window opened by Ferret: left alone");
            return;
        }
        watch(key);
    }
    if event == EVENT_OBJECT_SHOW {
        // Shown again later (an Explorer window can be hidden and shown): its thread has moved on
        // and this changes nothing.
        WATCHED.with_borrow(|w| w.get(&key).map(|thread| thread.send(Heard::Shown)));
    }
}

/// Starts a window's thread: it hides the window, and takes it over once it's been shown.
#[cfg(windows)]
fn watch(key: isize) {
    let seen = Instant::now();
    let (tell, heard) = std::sync::mpsc::channel();
    WATCHED.with_borrow_mut(|w| w.insert(key, tell));
    std::thread::spawn(move || {
        let hwnd = windows::Win32::Foundation::HWND(key as *mut _);
        // One an earlier run hid is hidden already, and is put back to what it was before that.
        let veil = Veil::left_behind(hwnd).unwrap_or_else(|| Veil::put(hwnd));
        match heard.recv() {
            Ok(Heard::Shown) => take_over(veil, seen),
            Ok(Heard::Gone) => veil.keep(),
            // This helper is stopping: dropping the veil puts the window back.
            Err(_) => {}
        }
    });
}

/// Reads a hidden Explorer window, opens Ferret there and closes it.
#[cfg(windows)]
fn take_over(veil: Veil, seen: Instant) {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    read_and_open(veil, seen);
    unsafe { CoUninitialize() };
}

#[cfg(windows)]
fn read_and_open(veil: Veil, seen: Instant) {
    let start = Instant::now();
    let hwnd = veil.hwnd;
    let Some(ferret) = crate::integration::default_target() else { return };
    let Some(browser) = find_browser(hwnd, Duration::from_secs(3)) else {
        say("new Explorer window, but no folder in it: left alone");
        return;
    };
    let found = read_window(&browser, Duration::from_millis(2_500));
    match &found {
        Some(Found::Open { .. } | Found::Start) => {}
        other => {
            say(&format!("Explorer window left alone: {other:?}"));
            return;
        }
    }
    let args = ferret_args(&found.unwrap());
    // Where the time went, for when "Show in folder" feels slow: Explorer shows a window once
    // it has loaded the folder.
    let took = format!("{} ms after Explorer made the window, {} ms after it showed it", seen.elapsed().as_millis(), start.elapsed().as_millis());
    match open_or_raise(&ferret, &args) {
        Ok(true) => say(&format!("opened in Ferret {took}: {args:?}")),
        Ok(false) => say(&format!("asked again, brought up the window already open {took}: {args:?}")),
        Err(e) => {
            say(&format!("can't start Ferret ({ferret}): {e}"));
            return;
        }
    }
    veil.keep();
    if let Err(e) = unsafe { browser.Quit() } {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        say(&format!("Explorer didn't close when asked ({e}), closing its window"));
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

/// The last window opened, so pressing "Show in folder" again doesn't stack up windows.
#[cfg(windows)]
struct Opened {
    args: Vec<String>,
    window: std::process::Child,
    at: Instant,
}

#[cfg(windows)]
static LAST_OPENED: std::sync::Mutex<Option<Opened>> = std::sync::Mutex::new(None);

/// Opens a Ferret window (true), or brings up the one just opened for the same thing (false).
#[cfg(windows)]
fn open_or_raise(ferret: &str, args: &[String]) -> std::io::Result<bool> {
    let mut last = LAST_OPENED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = last.as_mut() {
        let still_open = matches!(o.window.try_wait(), Ok(None));
        if is_repeat(&o.args, o.at.elapsed(), still_open, args) {
            raise_window(o.window.id());
            return Ok(false);
        }
    }
    let window = std::process::Command::new(ferret).args(args).spawn()?;
    *last = Some(Opened { args: args.to_vec(), window, at: Instant::now() });
    Ok(true)
}

/// Brings a process's window to the front: clicking the app that asked again may have covered
/// it. With no window yet there's nothing to do, it takes the front itself when it appears.
#[cfg(windows)]
fn raise_window(pid: u32) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{FALSE, HWND, LPARAM, TRUE};
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn look(hwnd: HWND, search: LPARAM) -> BOOL {
        let search = unsafe { &mut *(search.0 as *mut Search) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != search.pid || !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { GetWindowTextLengthW(hwnd) } == 0 {
            return TRUE;
        }
        search.found = Some(hwnd);
        FALSE
    }
    let mut search = Search { pid, found: None };
    // Err only says the search stopped early, which is finding it.
    let _ = unsafe { EnumWindows(Some(look), LPARAM(&mut search as *mut Search as isize)) };
    let Some(hwnd) = search.found else { return };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        // Windows only lets the app used last hand out the front, and a key press makes this
        // one that app. Alt does nothing on its own (the window library does the same).
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_LMENU, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        };
        SendInput(&[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(hwnd);
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

    /// A window that's fully transparent, which only this helper does to Explorer windows: an
    /// earlier run hid it (an update replaces the run) and nobody else would ever show it again.
    fn left_behind(hwnd: windows::Win32::Foundation::HWND) -> Option<Veil> {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        if ex_style & WS_EX_LAYERED.0 as isize == 0 {
            return None;
        }
        let (mut alpha, mut flags) = (255u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
        unsafe { GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)) }.ok()?;
        (flags.contains(LWA_ALPHA) && alpha == 0).then(|| Veil { hwnd, ex_style: ex_style & !((WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0) as isize), keep: false })
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
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;
    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }.ok()?;
    let start = Instant::now();
    while start.elapsed() < timeout && unsafe { IsWindow(Some(hwnd)) }.as_bool() {
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
                    // Which place it was, for when a window Ferret should have taken stays.
                    say(&format!("Explorer window shows {path}"));
                    return Some(Found::Keep);
                }
                // Home (Win+E) has no item to wait for: Ferret starts at once (the wait for a
                // selection made Win+E a third of a second slower than it need be).
                if matches!(classify(&path, None), Found::Start) {
                    return Some(Found::Start);
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
        // Home (Win+E, the taskbar's Explorer): Ferret starts as it does on its own.
        assert_eq!(classify("::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}", None), Found::Start);
        assert_eq!(classify("::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}", Some("x".into())), Found::Start);
        assert!(ferret_args(&Found::Start).is_empty());
        // Control Panel, libraries and Network stay in Explorer.
        assert_eq!(classify("::{26EE0668-A00A-44D7-9371-BEB064C98683}\\0", None), Found::Keep);
        assert_eq!(classify("::{031E4825-7B94-4DC3-B131-E946B44C8DD5}\\Documents.library-ms", None), Found::Keep);
        assert_eq!(ferret_args(&open(r"C:\x", Some(r"C:\x\a b.txt"))), vec![r"/select,C:\x\a b.txt".to_string()]);
        assert_eq!(ferret_args(&open(r"C:\x", None)), vec![r"C:\x".to_string()]);
    }

    #[test]
    fn asking_again_is_a_repeat_only_for_the_same_thing_soon_while_its_window_is_open() {
        let a = vec![r"/select,D:\Downloads\a.mp4".to_string()];
        let b = vec![r"/select,D:\Downloads\b.mp4".to_string()];
        let soon = Duration::from_secs(2);
        assert!(is_repeat(&a, soon, true, &a));
        assert!(is_repeat(&a, REPEAT_WITHIN - Duration::from_millis(1), true, &a));
        // Another file, a while later, or its window closed: a new window.
        assert!(!is_repeat(&a, soon, true, &b));
        assert!(!is_repeat(&a, REPEAT_WITHIN, true, &a));
        assert!(!is_repeat(&a, soon, false, &a));
        // Win+E (no arguments): every press opens a window.
        assert!(!is_repeat(&[], soon, true, &[]));
    }

    /// Only windows made fully transparent count as hidden by an earlier run.
    #[cfg(windows)]
    #[test]
    fn left_behind_finds_only_fully_transparent_windows() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::*;
        // Never shown, so nothing appears on screen.
        let make = |ex_style| unsafe { CreateWindowExW(ex_style, w!("STATIC"), w!("ferret test"), WS_POPUP, 0, 0, 10, 10, None, None, None, None) }.unwrap();
        let plain = make(WINDOW_EX_STYLE(0));
        assert!(Veil::left_behind(plain).is_none());

        let opaque = make(WS_EX_LAYERED);
        unsafe { SetLayeredWindowAttributes(opaque, Default::default(), 255, LWA_ALPHA) }.unwrap();
        assert!(Veil::left_behind(opaque).is_none());

        let hidden = make(WINDOW_EX_STYLE(0));
        let style = unsafe { GetWindowLongPtrW(hidden, GWL_EXSTYLE) };
        let veil = Veil::put(hidden);
        veil.keep();
        let found = Veil::left_behind(hidden).expect("it was made transparent");
        assert_eq!(found.ex_style, style, "putting it back removes what hiding it added");
        drop(found);
        assert!(Veil::left_behind(hidden).is_none(), "it's been put back");
        assert_eq!(unsafe { GetWindowLongPtrW(hidden, GWL_EXSTYLE) }, style);

        for w in [plain, opaque, hidden] {
            unsafe { DestroyWindow(w) }.unwrap();
        }
    }
}
