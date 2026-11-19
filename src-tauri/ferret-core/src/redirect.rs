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
//!
//! It also answers Ferret windows that Windows' folder command opened: which file a program's
//! "Show in folder" wanted selected there (the last part of this file).

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
/// A test's redirector (`TEST`), which runs next to the user's own.
const TEST_MUTEX: &str = "Local\\FerretExplorerRedirectTest";

/// FERRET_REDIRECT_TEST=<log file>: a redirector for tests of "Show in folder" through the
/// folder command (below). It runs next to the user's own, writes its log to that file, and
/// only answers Ferret windows that ask it which file to select: it opens no Ferret, keeps
/// none ready, and hides and closes no Explorer window but the ones it started itself.
const TEST: &str = "FERRET_REDIRECT_TEST";

fn test_log() -> Option<std::path::PathBuf> {
    std::env::var_os(TEST).filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

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
    let testing = test_log().is_some();
    let name: Vec<u16> = if testing { TEST_MUTEX } else { MUTEX }.encode_utf16().chain(Some(0)).collect();
    let _mutex = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(h) if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS => h,
        _ => return,
    };
    if !testing && crate::integration::default_target().is_none() {
        return;
    }
    say(if testing { "started (a test's: it only answers which file to select)" } else { "started" });
    answer_show_in_folder();
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
            if msg.message == WM_TIMER && !testing {
                let Some(ferret) = crate::integration::default_target() else { break };
                keep_one_ready(&ferret);
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
        let testing = test_log().is_some();
        if !testing && crate::integration::default_target().is_none() {
            return;
        }
        if !testing && take_pass() {
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
        // (A test's redirector hides nothing: the user's own does.)
        let veil = if test_log().is_some() { Veil::none(hwnd) } else { Veil::left_behind(hwnd).unwrap_or_else(|| Veil::put(hwnd)) };
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
    let browser = find_browser(hwnd, Duration::from_secs(3));
    let found = browser.as_ref().and_then(|b| read_window(b, Duration::from_millis(2_500)));
    // A window this helper started itself, for a Ferret window that asked which file a program
    // wanted shown in its folder (below): the selection goes to that window, and this one closes.
    let folder = match &found {
        Some(Found::Open { folder, .. }) => Some(folder.as_str()),
        _ => None,
    };
    if let Some(asked) = take_asked(folder) {
        let select = match &found {
            Some(Found::Open { folder, select }) if same_folder(folder, &asked.folder) => select.clone(),
            _ => None,
        };
        say(&match &select {
            Some(item) => format!("a program wanted {item} shown: told the Ferret window, {} ms after Explorer was started for it", asked.since.elapsed().as_millis()),
            None => format!("nothing to select in {} ({} ms after Explorer was started for it)", asked.folder, asked.since.elapsed().as_millis()),
        });
        // Windows brings the window it selects in to the front, and this one is about to go.
        let in_front = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } == hwnd;
        let _ = asked.tell.send(Answer { select, in_front });
        veil.keep();
        close_explorer(browser.as_ref(), hwnd);
        return;
    }
    if test_log().is_some() {
        return;
    }
    let Some(ferret) = crate::integration::default_target() else { return };
    let Some(browser) = browser else {
        say("new Explorer window, but no folder in it: left alone");
        return;
    };
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
    close_explorer(Some(&browser), hwnd);
}

/// Closes an Explorer window this helper is done with.
#[cfg(windows)]
fn close_explorer(browser: Option<&windows::Win32::UI::Shell::IWebBrowserApp>, hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    let asked = match browser {
        Some(browser) => unsafe { browser.Quit() }.map_err(|e| e.to_string()),
        None => Err("it has no folder".to_string()),
    };
    if let Err(e) = asked {
        say(&format!("Explorer didn't close when asked ({e}), closing its window"));
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

// ---------- "Show in folder" through Windows' folder command ----------
//
// A program's "Show in folder" usually asks Windows for it (SHOpenFolderAndSelectItems: Chrome,
// VS Code, most Electron apps). While Ferret is the default file manager, Windows answers by
// running Ferret's folder command with the folder alone, then waits, up to 20 seconds, for an
// Explorer window on that folder to select the file in. A Ferret window doesn't count (Windows
// never asks windows that aren't Explorer's, tried 2026-10-14). So the file wasn't selected,
// and the program's request hung for those 20 seconds: in Chrome the next "Show in folder"
// waits behind it, and a second press opened its window 15 to 20 seconds late (user report,
// 2026-11-18).
//
// What ends that wait is an Explorer window on the folder that appears meanwhile: Windows
// selects the file in it at once and lets the program go (measured: 26 ms after the window
// showed). It has to be a new window: one that was already there, navigated to the folder
// (Explorer's own "open", which reuses the window it keeps ready), isn't looked at.
//
// So the Ferret window that the folder command opened asks this helper which file was meant
// (`item_to_select`), and the helper starts Explorer on the same folder: out of sight, as every
// Explorer window it takes over. Windows finds that window and selects the file; the helper
// reads the selection, tells the Ferret window, and closes the Explorer one. The folder is in
// Ferret at once, as before; the file is selected about a second later (the time Explorer takes
// to start).
//
// What it costs: Windows starts an Explorer process of its own for a window opened this way
// (1.25 s of processor time, 120 MB), which leaves by itself a minute after the window closed.
// So only for folders a program had Windows open: one opened from the desktop, the taskbar or
// the Start menu doesn't ask (`integration::started_by_the_shell`). Other ways to get that
// window were tried and don't end the wait: Explorer's own "open" for the folder (it reuses a
// window it keeps ready, already known to Windows under another folder), and a window made
// through Explorer's automation object (hidden, and not looked at either).

/// The helper's pipe for these questions (standby.rs names pipes per exe and session).
const ASK_PIPE: &str = "shown";
/// Windows' own limit for the program that asked: nothing waits for an answer after it.
const ASK_WAIT: Duration = Duration::from_secs(20);
/// How long an Explorer window started for a question is still known as that (a folder on a
/// slow server shows late; taken for an ordinary window it would open Ferret a second time).
const ASKED_KEPT: Duration = Duration::from_secs(90);

/// What a Ferret window asks: the folder Windows opened it on.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct AskShown {
    folder: String,
}

/// The answer: the file to select there, if a program wanted one shown.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Default)]
struct Shown {
    select: Option<String>,
}

/// Asked by a window that Windows' folder command opened on `folder`
/// (`integration::shell_folder`): the file some program wanted shown there, if any. It waits
/// for Explorer (a second, usually; 20 at most), so not on the UI thread. None too when no
/// redirect of this exe runs (Ferret isn't the default file manager, or it's another copy's).
#[cfg(windows)]
pub fn item_to_select(exe: &str, folder: &str) -> Option<String> {
    use crate::standby::win::{connect, read_line, write_all};
    let (pipe, _) = connect(ASK_PIPE, exe)?;
    let mut line = serde_json::to_string(&AskShown { folder: folder.to_string() }).ok()?;
    line.push('\n');
    // On a thread of its own: a helper that hangs must not keep the caller for good.
    let (tell, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The whole value moves here (a field alone is a raw handle, which a thread won't take).
        let pipe = pipe;
        let answer = if write_all(pipe.0, line.as_bytes()) { read_line(pipe.0) } else { None };
        let _ = tell.send(answer);
    });
    let answer = heard.recv_timeout(ASK_WAIT + Duration::from_secs(3)).ok().flatten()?;
    serde_json::from_str::<Shown>(&answer).ok()?.select
}

#[cfg(not(windows))]
pub fn item_to_select(_exe: &str, _folder: &str) -> Option<String> {
    None
}

/// An Explorer window this helper started for a question, until it shows.
#[cfg(windows)]
struct Asked {
    folder: String,
    tell: std::sync::mpsc::Sender<Answer>,
    since: Instant,
}

/// What that window showed: the file selected in it, and whether it had taken the front.
#[cfg(windows)]
struct Answer {
    select: Option<String>,
    in_front: bool,
}

/// Every Explorer window there is, shown or not.
#[cfg(windows)]
fn explorer_windows() -> Vec<isize> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    unsafe extern "system" fn look(hwnd: HWND, found: LPARAM) -> BOOL {
        if is_explorer_window(hwnd) {
            unsafe { &mut *(found.0 as *mut Vec<isize>) }.push(hwnd.0 as isize);
        }
        TRUE
    }
    let mut found: Vec<isize> = Vec::new();
    let _ = unsafe { EnumWindows(Some(look), LPARAM(&mut found as *mut Vec<isize> as isize)) };
    found
}

/// The Explorer that runs the taskbar and the desktop (0 when there's none).
#[cfg(windows)]
fn shell_process() -> u32 {
    // SAFETY: a plain call.
    process_of(unsafe { windows::Win32::UI::WindowsAndMessaging::GetShellWindow() })
}

/// Whether Ferret asked for the next Explorer window to stay Explorer (`let_explorer_through`),
/// without using that up.
#[cfg(windows)]
fn pass_pending() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(PASS_KEY).and_then(|k| k.get_value::<u64, _>(PASS_VALUE)).is_ok_and(|until| now_ms() < until)
}

#[cfg(windows)]
static ASKED: std::sync::Mutex<Vec<Asked>> = std::sync::Mutex::new(Vec::new());

/// The questions still waiting for their Explorer window.
#[cfg(windows)]
fn asked() -> std::sync::MutexGuard<'static, Vec<Asked>> {
    let mut all = ASKED.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|a| a.since.elapsed() < ASKED_KEPT);
    all
}

/// The same folder, however each side writes it (Explorer's name for it, Ferret's).
fn same_folder(a: &str, b: &str) -> bool {
    let plain = |p: &str| p.trim().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    plain(a) == plain(b)
}

#[cfg(windows)]
fn process_of(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: a plain call with an out-parameter.
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The question an Explorer window answers: the oldest one about the folder it shows. (By the
/// folder, not by the process: Windows doesn't show the folder in the `explorer.exe` that was
/// started, but in another one it starts for it.)
#[cfg(windows)]
fn take_asked(folder: Option<&str>) -> Option<Asked> {
    let folder = folder?;
    let mut all = asked();
    let at = all.iter().position(|a| same_folder(&a.folder, folder))?;
    Some(all.remove(at))
}

/// Explorer's arguments for opening `folder` in a window of its own. Quoted: Explorer splits
/// its command line at commas too. (A drive's root needs none, and its closing backslash
/// would swallow the quote.)
fn explorer_args(folder: &str) -> String {
    let folder = folder.trim();
    let root = folder.len() <= 3 && folder.ends_with(":\\");
    if root { folder.to_string() } else { format!("\"{}\"", folder.trim_end_matches('\\')) }
}

/// Answers Ferret windows' questions, each on a thread of its own, for as long as this helper runs.
#[cfg(windows)]
fn answer_show_in_folder() {
    use crate::standby::win::{name_of, wide, Owned};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    let Ok(me) = std::env::current_exe() else { return };
    let me = me.to_string_lossy().into_owned();
    std::thread::spawn(move || {
        let name = wide(&name_of(ASK_PIPE, &me));
        loop {
            // One instance per window that asks: the next is made as soon as one is taken.
            // SAFETY: a NUL-terminated name; the handle returned is owned below.
            let h = unsafe {
                CreateNamedPipeW(PCWSTR(name.as_ptr()), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS, PIPE_UNLIMITED_INSTANCES, 4096, 4096, 0, None)
            };
            if h.is_invalid() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
            let pipe = Owned(h);
            // SAFETY: the pipe is this thread's until it's handed on; the call is synchronous.
            let connected = match unsafe { ConnectNamedPipe(pipe.0, None) } {
                Ok(()) => true,
                Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
            };
            if !connected {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let me = me.clone();
            std::thread::spawn(move || answer_one(pipe, &me));
        }
    });
}

/// One window's question: read, answered once Explorer has shown the folder, and done.
#[cfg(windows)]
fn answer_one(pipe: crate::standby::win::Owned, me: &str) {
    use crate::standby::win::{exe_of, read_line, same_file, write_all};
    use windows::Win32::Storage::FileSystem::FlushFileBuffers;
    use windows::Win32::System::Pipes::{DisconnectNamedPipe, GetNamedPipeClientProcessId};
    let mut asker = 0u32;
    // Only this Ferret's own windows: the answer starts Explorer, which isn't for any program to ask.
    // SAFETY: an out-parameter for a pipe handle this function owns.
    if unsafe { GetNamedPipeClientProcessId(pipe.0, &mut asker) }.is_err() || !exe_of(asker).is_some_and(|by| same_file(&by, me)) {
        return;
    }
    let Some(ask) = read_line(pipe.0).and_then(|l| serde_json::from_str::<AskShown>(&l).ok()) else { return };
    let select = shown_item(&ask.folder, asker);
    if let Ok(mut line) = serde_json::to_string(&Shown { select }) {
        line.push('\n');
        write_all(pipe.0, line.as_bytes());
        // SAFETY: the pipe is this function's; the calls are synchronous.
        unsafe {
            let _ = FlushFileBuffers(pipe.0);
            let _ = DisconnectNamedPipe(pipe.0);
        }
    }
}

/// Starts Explorer on `folder` and waits for what Windows selects in its window (`take_asked`,
/// from the window's own thread, sends it): the file a program wanted shown, or None.
#[cfg(windows)]
fn shown_item(folder: &str, asker: u32) -> Option<String> {
    use std::os::windows::process::CommandExt;
    // A folder on a drive or a server, as Explorer shows it under the same name.
    let fits = matches!(classify(folder, None), Found::Open { folder: f, .. } if same_folder(&f, folder) && !f.starts_with("::"));
    if !fits || !std::path::Path::new(&crate::paths::for_io(folder)).is_dir() {
        return None;
    }
    let explorer = std::path::PathBuf::from(std::env::var_os("SystemRoot")?).join("explorer.exe");
    let (tell, heard) = std::sync::mpsc::channel();
    let since = Instant::now();
    // The Explorer windows there are now: the one that comes next is the one started here.
    let known: std::collections::HashSet<isize> = explorer_windows().into_iter().collect();
    let shell = shell_process();
    {
        // Known before its window can be made (the lock is held across the start).
        let mut all = asked();
        std::process::Command::new(&explorer).raw_arg(explorer_args(folder)).spawn().ok()?;
        all.push(Asked { folder: folder.to_string(), tell, since });
    }
    say(&format!("Windows opened {folder} in Ferret: started Explorer there, out of sight, for the file a program may want shown"));
    // Its window is hidden the moment it exists, well before Explorer shows it. The event that
    // tells of a new window can come late (seen: after the window had shown, which was on
    // screen for a frame), and this one is opened whenever a program has Windows open a folder.
    // Not one in the Explorer that runs the desktop (its windows wait there, hidden, to be
    // reused), nor while Ferret's own "Open in Explorer" has a window coming, which must show.
    let mut hidden = false;
    let answer = loop {
        match heard.recv_timeout(Duration::from_millis(if hidden { 50 } else { 4 })) {
            Ok(answer) => break Some(answer),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if since.elapsed() >= ASK_WAIT => break None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !hidden && since.elapsed() < Duration::from_secs(5) {
                    let new = explorer_windows().into_iter().filter(|w| !known.contains(w)).map(|w| windows::Win32::Foundation::HWND(w as *mut _)).find(|w| process_of(*w) != shell);
                    if let Some(hwnd) = new.filter(|_| !pass_pending()) {
                        Veil::put(hwnd).keep();
                        hidden = true;
                    }
                }
            }
        }
    };
    let answer = answer?;
    if (answer.select.is_some() || answer.in_front) && test_log().is_none() {
        // The Explorer window had the front (Windows gives it to the window it selects in)
        // and is gone: Ferret's takes its place.
        front_once_shown(asker);
    }
    answer.select
}

/// A Ferret that has done its start-up waits, without a window, to be handed the next launch
/// (standby.rs): one is kept while the setting is on (it is unless turned off), and sent away
/// when it's off. Called on the timer, so the one a launch used up is replaced within seconds.
#[cfg(windows)]
fn keep_one_ready(ferret: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !crate::standby::wanted() {
        crate::standby::dismiss(ferret);
    } else if !crate::standby::waiting(ferret) {
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            say("keeping a Ferret ready for the next window");
        }
        crate::standby::ensure(ferret);
    }
}

/// Whether the redirect runs in this sign-in session (the Ferret kept ready exits when not).
#[cfg(windows)]
pub fn running() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name: Vec<u16> = MUTEX.encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated name; the handle is closed at once.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) } {
        Ok(h) => {
            let _ = unsafe { CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(windows))]
pub fn running() -> bool {
    false
}

/// The process a window was opened in: started for it, or the one kept ready, which took it.
#[cfg(windows)]
enum Window {
    Started(std::process::Child),
    Ready(u32),
}

#[cfg(windows)]
impl Window {
    fn id(&self) -> u32 {
        match self {
            Window::Started(child) => child.id(),
            Window::Ready(pid) => *pid,
        }
    }

    fn still_open(&mut self) -> bool {
        match self {
            Window::Started(child) => matches!(child.try_wait(), Ok(None)),
            Window::Ready(pid) => crate::standby::alive(*pid),
        }
    }
}

/// The last window opened, so pressing "Show in folder" again doesn't stack up windows.
#[cfg(windows)]
struct Opened {
    args: Vec<String>,
    window: Window,
    at: Instant,
}

#[cfg(windows)]
static LAST_OPENED: std::sync::Mutex<Option<Opened>> = std::sync::Mutex::new(None);

/// Opens a Ferret window (true), or brings up the one just opened for the same thing (false).
#[cfg(windows)]
fn open_or_raise(ferret: &str, args: &[String]) -> std::io::Result<bool> {
    let mut last = LAST_OPENED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(o) = last.as_mut() {
        let still_open = o.window.still_open();
        if is_repeat(&o.args, o.at.elapsed(), still_open, args) {
            raise_window(o.window.id());
            return Ok(false);
        }
    }
    // The Ferret kept ready takes it when there is one: its window is there in a fraction of
    // the time a start takes. Else a start, as ever.
    let launch = crate::standby::Launch { args: args.to_vec(), cwd: String::new() };
    let window = match crate::standby::hand_over(ferret, &launch) {
        Some(pid) => {
            say("handed to the Ferret kept ready");
            let ferret = ferret.to_string();
            std::thread::spawn(move || {
                front_once_shown(pid);
                // The next one, once this window has had a moment to itself (the timer would
                // take up to five seconds, and a second press may come sooner).
                std::thread::sleep(Duration::from_millis(1200));
                keep_one_ready(&ferret);
            });
            Window::Ready(pid)
        }
        None => Window::Started(std::process::Command::new(ferret).args(args).spawn()?),
    };
    *last = Some(Opened { args: args.to_vec(), window, at: Instant::now() });
    Ok(true)
}

/// A process started for a window may take the front; one that was waiting in the background
/// may not, and this helper can't always give it leave. So once its window is there and isn't
/// in front, it's brought there (`raise_window` knows how). Gives up after two seconds.
#[cfg(windows)]
fn front_once_shown(pid: u32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let since = Instant::now();
    while since.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(25));
        if window_of(pid).is_none() {
            continue;
        }
        let mut front = 0;
        // SAFETY: plain calls with an out-parameter.
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front)) };
        if front != pid {
            // A moment for it to take the front itself, as it does when it may.
            std::thread::sleep(Duration::from_millis(60));
            unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut front)) };
            if front != pid {
                say("its window wasn't in front: brought there");
                raise_window(pid);
            }
        }
        return;
    }
}

/// Brings a process's window to the front: clicking the app that asked again may have covered
/// it. With no window yet there's nothing to do, it takes the front itself when it appears.
#[cfg(windows)]
fn raise_window(pid: u32) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    let Some(hwnd) = window_of(pid) else { return };
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

/// A process's window: shown, with a title.
#[cfg(windows)]
fn window_of(pid: u32) -> Option<windows::Win32::Foundation::HWND> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{FALSE, HWND, LPARAM, TRUE};
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
    search.found
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

    /// No veil at all: the window is left as it is, now and when this is dropped.
    fn none(hwnd: windows::Win32::Foundation::HWND) -> Veil {
        Veil { hwnd, ex_style: 0, keep: true }
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
    // A test's redirector keeps its own log (`TEST`).
    if let Some(file) = test_log() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
            let _ = writeln!(f, "{} [{}] {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), std::process::id());
        }
        return;
    }
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
    #[test]
    fn a_question_and_its_answer_are_one_line_of_json() {
        let ask = serde_json::to_string(&AskShown { folder: r"D:\Down loads".into() }).unwrap();
        assert!(!ask.contains('\n'));
        assert_eq!(serde_json::from_str::<AskShown>(&ask).unwrap(), AskShown { folder: r"D:\Down loads".into() });
        let shown = Shown { select: Some(r"D:\Down loads\a, b.txt".into()) };
        assert_eq!(serde_json::from_str::<Shown>(&serde_json::to_string(&shown).unwrap()).unwrap(), shown);
        // Nothing to select, however the helper says so.
        assert_eq!(serde_json::from_str::<Shown>("{\"select\":null}").unwrap(), Shown::default());
        assert_eq!(serde_json::from_str::<Shown>("{}").unwrap(), Shown::default());
    }

    #[test]
    fn explorer_is_started_on_the_folder_in_quotes() {
        assert_eq!(explorer_args(r"D:\Downloads"), r#""D:\Downloads""#);
        // A comma would split Explorer's command line; a closing backslash would swallow the quote.
        assert_eq!(explorer_args(r"C:\a, b\c\"), r#""C:\a, b\c""#);
        assert_eq!(explorer_args(r"\\server\share\"), r#""\\server\share""#);
        assert_eq!(explorer_args(r"D:\"), r"D:\");
    }

    #[test]
    fn a_folder_is_the_same_however_it_is_written() {
        assert!(same_folder(r"D:\Downloads", r"d:\downloads\"));
        assert!(same_folder(r"D:/Downloads/mm", r"D:\Downloads\mm"));
        assert!(same_folder(r"D:\", "d:"));
        assert!(!same_folder(r"D:\Downloads", r"D:\Downloads\mm"));
    }

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
