//! Windows shell integration: the real Explorer context menus (item and
//! folder-background), Properties / Open-with dialogs, "Open with" handler
//! enumeration, CF_HDROP file clipboard interop, shell thumbnails/icons,
//! terminal / VS Code launchers and "reveal in Explorer".
//!
//! Threading:
//! * Menus and the Properties sheet run on the main (UI) thread — the async
//!   commands hop there with `run_on_main_thread`, so the modal menu loop runs
//!   from the event loop rather than inside a WebView2 IPC callback. The main
//!   thread already has OLE initialised by WebView2.
//! * Everything else runs on a blocking worker thread with a per-call
//!   apartment-threaded COM initialisation ([`ComGuard`]).

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use serde::Serialize;
use windows::core::{Interface, HRESULT, HSTRING, PCSTR, PCWSTR, PSTR, PWSTR};
use windows::Win32::Foundation::{
    GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, POINTL, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IDataObject, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
};
use windows::Win32::System::Ole::{
    IDropTarget, OleGetClipboard, DROPEFFECT_LINK as OLE_DROPEFFECT_LINK,
};
use windows::Win32::System::SystemServices::{
    MK_CONTROL, MK_LBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    AssocQueryStringW, BHID_DataObject, BHID_SFUIObject, DefSubclassProc, DragQueryFileW,
    IAssocHandler, IContextMenu, IContextMenu2, IContextMenu3, ILFindLastID, ILFree, ILGetSize,
    IShellFolder, IShellItem, IShellItemArray, IShellItemImageFactory, RemoveWindowSubclass,
    SHAssocEnumHandlers, SHBindToParent, SHCreateItemFromParsingName,
    SHCreateShellItemArrayFromIDLists, SHDefExtractIconW, SHGetDesktopFolder, SHLoadIndirectString,
    SHMultiFileProperties, SHObjectProperties, SHOpenFolderAndSelectItems, SHOpenWithDialog,
    SHParseDisplayName, SetWindowSubclass, ShellExecuteExW, ASSOCF_INIT_IGNOREUNKNOWN,
    ASSOCF_NOTRUNCATE, ASSOCSTR, ASSOCSTR_EXECUTABLE, ASSOCSTR_FRIENDLYAPPNAME,
    ASSOC_FILTER_RECOMMENDED, CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_ITEMMENU, CMF_NORMAL,
    CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO,
    CMINVOKECOMMANDINFOEX, GCS_VERBW, HDROP, OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHOP_FILEPATH, SIIGBF,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyIcon, DestroyMenu, GetIconInfo, InsertMenuW, PostMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HICON, HMENU, ICONINFO, MF_BYPOSITION, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM,
    WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WM_NULL,
};

use crate::paths;

// ---------------------------------------------------------------------------
// Small Win32 / COM helpers
// ---------------------------------------------------------------------------

/// `CMIC_MASK_UNICODE` (== `SEE_MASK_UNICODE`); not exported under that name.
const CMIC_MASK_UNICODE: u32 = 0x0000_4000;
/// HRESULT_FROM_WIN32(ERROR_CANCELLED)
const E_CANCELLED: HRESULT = HRESULT(0x8007_04C7_u32 as i32);
const E_PENDING: HRESULT = HRESULT(0x8000_000A_u32 as i32);
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;
const DROPEFFECT_LINK: u32 = 4;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Size of the DROPFILES header (pFiles, pt.x, pt.y, fNC, fWide).
const DROPFILES_HEADER: usize = 20;

pub(crate) fn hr_msg(e: windows::core::Error) -> String {
    let m = e.message();
    let m = m.trim();
    if m.is_empty() {
        format!("{:#010x}", e.code().0)
    } else {
        m.to_string()
    }
}

/// Balances a per-call `CoInitializeEx`. `S_OK`/`S_FALSE` both need a
/// matching `CoUninitialize`; `RPC_E_CHANGED_MODE` (apartment already set up
/// differently) does not, and COM is usable in that case anyway.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub(crate) fn init() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Owned absolute PIDL.
pub(crate) struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    pub(crate) fn parse(path: &str) -> Result<Pidl, String> {
        fn try_parse(p: &str) -> windows::core::Result<*mut ITEMIDLIST> {
            let mut pidl = std::ptr::null_mut();
            unsafe { SHParseDisplayName(&HSTRING::from(p), None, &mut pidl, 0, None)? };
            Ok(pidl)
        }
        match try_parse(path) {
            Ok(p) if !p.is_null() => Ok(Pidl(p)),
            first => {
                // Share roots (`\\wsl.localhost\Arch`) sometimes want a trailing `\`.
                let alt = paths::for_io(path);
                if alt != path {
                    if let Ok(p) = try_parse(&alt) {
                        if !p.is_null() {
                            return Ok(Pidl(p));
                        }
                    }
                }
                Err(match first {
                    Err(e) => format!("{path}: {}", hr_msg(e)),
                    Ok(_) => format!("{path}: cannot resolve"),
                })
            }
        }
    }

    pub(crate) fn ptr(&self) -> *const ITEMIDLIST {
        self.0
    }

    fn is_empty(&self) -> bool {
        unsafe { (*self.0).mkid.cb == 0 }
    }

    /// Serialized PIDL including its terminator.
    fn to_bytes(&self) -> Vec<u8> {
        unsafe {
            let size = ILGetSize(Some(self.0)) as usize;
            std::slice::from_raw_parts(self.0 as *const u8, size).to_vec()
        }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.0)) };
    }
}

/// Reads and frees a `CoTaskMemAlloc`ed wide string.
pub(crate) fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

fn key_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

/// Raw HWND (pointer-sized) so it can cross threads.
pub(crate) fn raw_hwnd(window: &tauri::WebviewWindow) -> Result<isize, String> {
    let h = window.hwnd().map_err(|e| e.to_string())?;
    Ok(h.0 as isize)
}

pub(crate) fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

/// Tauri only sets the window's small icon. Without a big one the taskbar and
/// Alt+Tab fall back to the exe icon via the shell icon cache, which can stay
/// stale for a long time. Set ICON_BIG from the exe's own icon resource
/// (id 32512, embedded by tauri-build) at the window's DPI.
pub fn set_big_icon(window: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{
        LoadImageW, SendMessageW, ICON_BIG, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, WM_SETICON,
    };
    let Ok(raw) = raw_hwnd(window) else { return };
    let hwnd = hwnd_from_raw(raw);
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else { return };
        let size = GetSystemMetricsForDpi(SM_CXICON, GetDpiForWindow(hwnd)).max(32);
        let resource = PCWSTR(32512 as *const u16);
        let image = LoadImageW(
            Some(HINSTANCE(module.0)),
            resource,
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        );
        if let Ok(icon) = image {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
    }
}

/// CSS-pixel client coordinates of the webview → physical screen coordinates.
fn screen_point(window: &tauri::WebviewWindow, x: f64, y: f64) -> (i32, i32) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let origin = window
        .inner_position()
        .map(|p| (p.x, p.y))
        .unwrap_or((0, 0));
    (
        origin.0 + (x * scale).round() as i32,
        origin.1 + (y * scale).round() as i32,
    )
}

/// Runs `f` on the main (UI) thread and waits for its result without blocking
/// the async runtime.
pub(crate) async fn on_main_thread<T, F>(window: &tauri::WebviewWindow, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .run_on_main_thread(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "main-thread task was dropped".to_string())?
}

/// Runs `f` on a blocking worker thread with COM (STA) initialised.
async fn blocking_com<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let _com = ComGuard::init();
        f()
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Native context menus
// ---------------------------------------------------------------------------

pub(crate) const ID_CMD_FIRST: u32 = 1;
pub(crate) const ID_CMD_LAST: u32 = 0x7FFF;
const MENU_SUBCLASS_ID: usize = 0x00FE_77E7;
/// Guards against a second menu being tracked from inside the first's loop.
static MENU_ACTIVE: AtomicBool = AtomicBool::new(false);

/// What the subclass proc forwards menu messages to.
struct MenuHook {
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
}

impl MenuHook {
    fn new(cm: &IContextMenu) -> Self {
        MenuHook {
            cm2: cm.cast().ok(),
            cm3: cm.cast().ok(),
        }
    }
}

/// Forwards owner-draw / popup-init messages to the shell's IContextMenu2/3 so
/// that icons and lazily-filled submenus (Send to, Open with, New) work.
unsafe extern "system" fn menu_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    let forward = match msg {
        WM_INITMENUPOPUP | WM_MENUCHAR => true,
        // wParam == 0 → the item belongs to a menu, not a control.
        WM_DRAWITEM | WM_MEASUREITEM => wparam.0 == 0,
        _ => false,
    };
    if forward && data != 0 {
        let hook = unsafe { &*(data as *const MenuHook) };
        if let Some(cm3) = &hook.cm3 {
            let mut result = LRESULT(0);
            if unsafe { cm3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result)) }.is_ok() {
                return match msg {
                    WM_DRAWITEM | WM_MEASUREITEM => LRESULT(1),
                    _ => result,
                };
            }
        } else if let Some(cm2) = &hook.cm2 {
            if msg != WM_MENUCHAR && unsafe { cm2.HandleMenuMsg(msg, wparam, lparam) }.is_ok() {
                return if msg == WM_INITMENUPOPUP {
                    LRESULT(0)
                } else {
                    LRESULT(1)
                };
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// Installs [`menu_subclass_proc`] for the lifetime of the guard.
struct Subclass(HWND);

impl Subclass {
    fn install(hwnd: HWND, hook: &MenuHook) -> Option<Subclass> {
        let ok = unsafe {
            SetWindowSubclass(
                hwnd,
                Some(menu_subclass_proc),
                MENU_SUBCLASS_ID,
                hook as *const MenuHook as usize,
            )
        };
        ok.as_bool().then_some(Subclass(hwnd))
    }
}

impl Drop for Subclass {
    fn drop(&mut self) {
        unsafe {
            let _ = RemoveWindowSubclass(self.0, Some(menu_subclass_proc), MENU_SUBCLASS_ID);
        }
    }
}

pub(crate) struct MenuGuard(pub(crate) HMENU);

impl Drop for MenuGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

/// Which Explorer menu is being shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuKind {
    /// Selected items: "Rename" is offered and handed back to the UI.
    Items,
    /// Folder background. The folder's view-object menu lacks the items that
    /// Explorer's view (DefView) adds itself, so "Paste" / "Paste shortcut"
    /// are added here and run through the shell's own `paste` / `pastelink`.
    Background,
    /// The folder's own item menu, used when it has no background menu
    /// (no "Rename": the UI expects a background menu).
    FolderItem,
}

/// Fills `hmenu` from the context menu (`QueryContextMenu`).
pub(crate) fn populate_menu(cm: &IContextMenu, hmenu: HMENU, kind: MenuKind) -> Result<(), String> {
    let mut flags = CMF_NORMAL;
    match kind {
        MenuKind::Items => flags |= CMF_CANRENAME | CMF_ITEMMENU,
        MenuKind::FolderItem => flags |= CMF_ITEMMENU,
        MenuKind::Background => {}
    }
    if key_down(VK_SHIFT.0) {
        flags |= CMF_EXTENDEDVERBS;
    }
    unsafe { cm.QueryContextMenu(hmenu, 0, ID_CMD_FIRST, ID_CMD_LAST, flags) }
        .ok()
        .map_err(hr_msg)?;
    if kind == MenuKind::Background {
        add_paste_items(hmenu);
    }
    Ok(())
}

/// Command ids for our own background-menu items (outside the shell's range).
const ID_PASTE: u32 = ID_CMD_LAST + 1;
const ID_PASTE_LINK: u32 = ID_CMD_LAST + 2;

/// Can the clipboard be pasted as files (Explorer's rule, roughly)?
fn clipboard_has_pasteable() -> (bool, bool) {
    let has = |name: &str| unsafe {
        let f = RegisterClipboardFormatW(&HSTRING::from(name));
        f != 0 && IsClipboardFormatAvailable(f).is_ok()
    };
    let hdrop = unsafe { IsClipboardFormatAvailable(CF_HDROP) }.is_ok();
    let idlist = has("Shell IDList Array");
    let virtual_files = has("FileGroupDescriptorW");
    (hdrop || idlist || virtual_files, idlist)
}

/// `OleGetClipboard`, retried while another process (clipboard history,
/// managers) briefly holds the clipboard.
fn ole_get_clipboard() -> Result<IDataObject, String> {
    let mut attempt = 0u64;
    loop {
        match unsafe { OleGetClipboard() } {
            Ok(d) => return Ok(d),
            Err(e) if attempt < 12 => {
                let _ = e;
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
            }
            Err(e) => return Err(hr_msg(e)),
        }
    }
}

/// "Paste shortcut": drop the clipboard's data object on the folder with
/// DROPEFFECT_LINK — what Explorer's view does (there is no such verb on the
/// folder menu). Needs an OLE thread (the main thread).
fn paste_link_into(folder: &str, at: POINT) -> Result<(), String> {
    let data = ole_get_clipboard()?;
    let item = shell_item(folder)?;
    let target: IDropTarget =
        unsafe { item.BindToHandler(None, &BHID_SFUIObject) }.map_err(hr_msg)?;
    // Behave like a left-button Ctrl+Shift drag (= "create link"): the
    // button is down during DragEnter and released at Drop. Without
    // MK_LBUTTON the target treats it as a right-drag and pops its
    // "Copy here / Move here / Create shortcuts here" menu.
    let enter_keys = MODIFIERKEYS_FLAGS(MK_LBUTTON.0 | MK_CONTROL.0 | MK_SHIFT.0);
    let drop_keys = MODIFIERKEYS_FLAGS(MK_CONTROL.0 | MK_SHIFT.0);
    let pt = POINTL { x: at.x, y: at.y };
    let mut effect = OLE_DROPEFFECT_LINK;
    unsafe { target.DragEnter(&data, enter_keys, pt, &mut effect) }.map_err(hr_msg)?;
    if effect.0 & OLE_DROPEFFECT_LINK.0 == 0 {
        let _ = unsafe { target.DragLeave() };
        return Err("Shortcuts cannot be created here".into());
    }
    let mut effect = OLE_DROPEFFECT_LINK;
    unsafe { target.Drop(&data, drop_keys, pt, &mut effect) }.map_err(hr_msg)
}

fn add_paste_items(hmenu: HMENU) {
    let (paste, link) = clipboard_has_pasteable();
    let flag = |enabled: bool| {
        if enabled {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        }
    };
    unsafe {
        let _ = InsertMenuW(
            hmenu,
            0,
            MF_BYPOSITION | flag(paste),
            ID_PASTE as usize,
            &HSTRING::from("&Paste"),
        );
        let _ = InsertMenuW(
            hmenu,
            1,
            MF_BYPOSITION | flag(link),
            ID_PASTE_LINK as usize,
            &HSTRING::from("Paste &shortcut"),
        );
        let _ = InsertMenuW(hmenu, 2, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// Language-independent verb of a menu command (`GCS_VERBW`); "" if none.
pub(crate) fn command_verb(cm: &IContextMenu, offset: usize) -> String {
    let mut buf = [0u16; 512];
    let r = unsafe {
        cm.GetCommandString(
            offset,
            GCS_VERBW,
            None,
            PSTR(buf.as_mut_ptr() as *mut u8),
            buf.len() as u32,
        )
    };
    if r.is_err() {
        return String::new();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// A command to invoke: a menu offset (`MAKEINTRESOURCE`) or a string verb.
pub(crate) enum Verb<'a> {
    Offset(usize),
    Name(&'a str),
}

pub(crate) fn invoke_command(
    cm: &IContextMenu,
    hwnd: HWND,
    verb: Verb,
    pt: POINT,
    directory: Option<&str>,
) -> Result<(), String> {
    let dir = directory.map(HSTRING::from);
    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
    if key_down(VK_SHIFT.0) {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if key_down(VK_CONTROL.0) {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    // Keep the string-verb buffers alive across the call.
    let (verb_a, verb_w): (Vec<u8>, HSTRING) = match verb {
        Verb::Name(v) => {
            let mut a: Vec<u8> = v.bytes().filter(u8::is_ascii).collect();
            a.push(0);
            (a, HSTRING::from(v))
        }
        Verb::Offset(_) => (Vec::new(), HSTRING::new()),
    };
    let (lp_verb, lp_verb_w) = match verb {
        // MAKEINTRESOURCE(offset)
        Verb::Offset(o) => (PCSTR(o as *const u8), PCWSTR(o as *const u16)),
        Verb::Name(_) => (PCSTR(verb_a.as_ptr()), PCWSTR(verb_w.as_ptr())),
    };
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd,
        lpVerb: lp_verb,
        lpVerbW: lp_verb_w,
        lpDirectoryW: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: pt,
        ..Default::default()
    };
    match unsafe {
        cm.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO)
    } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == E_CANCELLED => Ok(()),
        Err(e) => Err(hr_msg(e)),
    }
}

/// Shows `cm` as a popup menu at screen point `pt`, invokes the chosen command
/// and returns its verb. For [`MenuKind::Items`], "rename" is returned without
/// being invoked (the UI renames inline).
fn track_context_menu(
    hwnd: HWND,
    cm: &IContextMenu,
    pt: POINT,
    directory: Option<&str>,
    kind: MenuKind,
) -> Result<Option<String>, String> {
    let hmenu = unsafe { CreatePopupMenu() }.map_err(hr_msg)?;
    let _menu = MenuGuard(hmenu);
    populate_menu(cm, hmenu, kind)?;

    let hook = MenuHook::new(cm);
    let cmd = {
        let _subclass = Subclass::install(hwnd, &hook);
        unsafe {
            let _ = SetForegroundWindow(hwnd);
            let r = TrackPopupMenuEx(
                hmenu,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                pt.x,
                pt.y,
                hwnd,
                None,
            );
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            r.0 as u32
        }
    };
    run_menu_command(hwnd, cm, cmd, pt, directory, kind)
}

/// Acts on the command id returned by `TrackPopupMenuEx` (0 = dismissed).
fn run_menu_command(
    hwnd: HWND,
    cm: &IContextMenu,
    cmd: u32,
    pt: POINT,
    directory: Option<&str>,
    kind: MenuKind,
) -> Result<Option<String>, String> {
    if kind == MenuKind::Background && cmd == ID_PASTE {
        // The folder menu still implements the canonical "paste" verb.
        invoke_command(cm, hwnd, Verb::Name("paste"), pt, directory)?;
        return Ok(Some("paste".into()));
    }
    if kind == MenuKind::Background && cmd == ID_PASTE_LINK {
        paste_link_into(directory.ok_or("no folder")?, pt)?;
        return Ok(Some("pastelink".into()));
    }
    if !(ID_CMD_FIRST..=ID_CMD_LAST).contains(&cmd) {
        return Ok(None);
    }
    let offset = (cmd - ID_CMD_FIRST) as usize;
    let verb = command_verb(cm, offset);
    if kind == MenuKind::Items && verb.eq_ignore_ascii_case("rename") {
        return Ok(Some("rename".into()));
    }
    // "Open"/"Explore" on folders should navigate Ferret, not spawn Explorer.
    if kind == MenuKind::Items
        && ITEMS_ARE_FOLDERS.load(Ordering::SeqCst)
        && (verb.eq_ignore_ascii_case("open") || verb.eq_ignore_ascii_case("explore"))
    {
        return Ok(Some("open".into()));
    }
    invoke_command(cm, hwnd, Verb::Offset(offset), pt, directory)?;
    Ok(Some(verb))
}

/// Set while an item menu for folder(s) is open (see `run_menu_command`).
static ITEMS_ARE_FOLDERS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Grouping key for "do these items share a parent folder?" — drive roots
/// share "This PC", share roots share their server.
fn container_key(p: &str) -> String {
    match paths::parent(p) {
        Some(par) => paths::key(&par),
        None => match p.strip_prefix(r"\\") {
            Some(rest) => format!(
                r"\\{}",
                rest.split('\\').next().unwrap_or("").to_lowercase()
            ),
            None => "::this-pc".into(),
        },
    }
}

/// Item context menu for `paths` (all in one folder). Returns the menu, the
/// parsed PIDLs (kept alive for the menu's lifetime) and the parent folder.
pub(crate) fn item_context_menu(
    hwnd: HWND,
    paths_in: &[String],
) -> Result<(IContextMenu, Vec<Pidl>, Option<String>), String> {
    let norm: Vec<String> = paths_in.iter().map(|p| paths::normalize(p)).collect();
    let first = norm.first().ok_or("no items")?;
    let group = container_key(first);
    if norm.iter().any(|p| container_key(p) != group) {
        return Err("All items must be in the same folder".into());
    }
    let pidls = norm
        .iter()
        .map(|p| Pidl::parse(p))
        .collect::<Result<Vec<_>, _>>()?;
    let folder: IShellFolder = unsafe { SHBindToParent(pidls[0].ptr(), None) }.map_err(hr_msg)?;
    let children: Vec<*const ITEMIDLIST> = pidls
        .iter()
        .map(|p| unsafe { ILFindLastID(p.ptr()) } as *const ITEMIDLIST)
        .collect();
    let cm: IContextMenu =
        unsafe { folder.GetUIObjectOf(hwnd, &children, None) }.map_err(hr_msg)?;
    Ok((cm, pidls, paths::parent(first)))
}

/// Folder-background menu (New ▸, Paste, Open in Terminal, …).
pub(crate) fn background_context_menu(hwnd: HWND, folder: &str) -> Result<IContextMenu, String> {
    let pidl = Pidl::parse(folder)?;
    let desktop = unsafe { SHGetDesktopFolder() }.map_err(hr_msg)?;
    let sf: IShellFolder = if pidl.is_empty() {
        desktop
    } else {
        unsafe { desktop.BindToObject(pidl.ptr(), None) }.map_err(hr_msg)?
    };
    unsafe { sf.CreateViewObject(hwnd) }.map_err(hr_msg)
}

fn with_menu_lock<T>(f: impl FnOnce() -> Result<Option<T>, String>) -> Result<Option<T>, String> {
    if MENU_ACTIVE.swap(true, Ordering::SeqCst) {
        return Ok(None);
    }
    let r = f();
    MENU_ACTIVE.store(false, Ordering::SeqCst);
    r
}

/// Shows the real Explorer context menu for `paths` (same folder) at the
/// webview client position (`x`, `y` in CSS px). Returns the invoked verb
/// (`""` when the command has none), `"rename"` without invoking it, or `null`
/// when dismissed.
#[tauri::command]
pub async fn show_native_menu(
    window: tauri::WebviewWindow,
    paths: Vec<String>,
    x: f64,
    y: f64,
) -> Result<Option<String>, String> {
    if paths.is_empty() {
        return Ok(None);
    }
    let raw = raw_hwnd(&window)?;
    let (sx, sy) = screen_point(&window, x, y);
    let all_dirs = paths.iter().all(|p| std::path::Path::new(&paths::for_io(p)).is_dir());
    on_main_thread(&window, move || {
        with_menu_lock(|| {
            ITEMS_ARE_FOLDERS.store(all_dirs, Ordering::SeqCst);
            let hwnd = hwnd_from_raw(raw);
            let _com = ComGuard::init();
            let (cm, _pidls, dir) = item_context_menu(hwnd, &paths)?;
            track_context_menu(
                hwnd,
                &cm,
                POINT { x: sx, y: sy },
                dir.as_deref(),
                MenuKind::Items,
            )
        })
    })
    .await
}

/// Explorer's folder-background menu for `folder`. If the folder has no
/// background menu (`CreateViewObject` fails — rare, e.g. some namespace
/// roots), falls back to the folder item's own context menu.
#[tauri::command]
pub async fn show_native_background_menu(
    window: tauri::WebviewWindow,
    folder: String,
    x: f64,
    y: f64,
) -> Result<Option<String>, String> {
    let raw = raw_hwnd(&window)?;
    let (sx, sy) = screen_point(&window, x, y);
    on_main_thread(&window, move || {
        with_menu_lock(|| {
            let hwnd = hwnd_from_raw(raw);
            let pt = POINT { x: sx, y: sy };
            let _com = ComGuard::init();
            let folder = paths::normalize(&folder);
            match background_context_menu(hwnd, &folder) {
                Ok(cm) => track_context_menu(hwnd, &cm, pt, Some(&folder), MenuKind::Background),
                Err(_) => {
                    let (cm, _pidls, dir) = item_context_menu(hwnd, std::slice::from_ref(&folder))?;
                    track_context_menu(hwnd, &cm, pt, dir.as_deref(), MenuKind::FolderItem)
                }
            }
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// Properties / Open with dialogs
// ---------------------------------------------------------------------------

/// Native Properties sheet for one item (`SHObjectProperties`) or a merged
/// sheet for several (`SHMultiFileProperties`). The sheets are modeless and
/// run on their own shell thread, so this returns immediately.
#[tauri::command]
pub async fn show_properties(
    window: tauri::WebviewWindow,
    paths: Vec<String>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let raw = raw_hwnd(&window)?;
    on_main_thread(&window, move || {
        let hwnd = hwnd_from_raw(raw);
        let _com = ComGuard::init();
        let norm: Vec<String> = paths.iter().map(|p| paths::normalize(p)).collect();
        if norm.len() == 1 {
            let ok = unsafe {
                SHObjectProperties(
                    Some(hwnd),
                    SHOP_FILEPATH,
                    &HSTRING::from(norm[0].as_str()),
                    PCWSTR::null(),
                )
            };
            return if ok.as_bool() {
                Ok(())
            } else {
                Err(format!("Cannot show properties for {}", norm[0]))
            };
        }
        let pidls = norm
            .iter()
            .map(|p| Pidl::parse(p))
            .collect::<Result<Vec<_>, _>>()?;
        let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(Pidl::ptr).collect();
        let array: IShellItemArray =
            unsafe { SHCreateShellItemArrayFromIDLists(&ptrs) }.map_err(hr_msg)?;
        let data: IDataObject =
            unsafe { array.BindToHandler(None, &BHID_DataObject) }.map_err(hr_msg)?;
        unsafe { SHMultiFileProperties(&data, 0) }.map_err(hr_msg)
    })
    .await
}

/// The system "Open with" chooser for `path` (runs on a worker thread so the
/// UI keeps painting; the dialog is owned by the app window).
#[tauri::command]
pub async fn open_with_dialog(window: tauri::WebviewWindow, path: String) -> Result<(), String> {
    let raw = raw_hwnd(&window)?;
    blocking_com(move || {
        let file = HSTRING::from(paths::normalize(&path));
        let info = OPENASINFO {
            pcszFile: PCWSTR(file.as_ptr()),
            pcszClass: PCWSTR::null(),
            oaifInFlags: OAIF_EXEC | OAIF_ALLOW_REGISTRATION,
        };
        match unsafe { SHOpenWithDialog(Some(hwnd_from_raw(raw)), &info) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == E_CANCELLED => Ok(()),
            Err(e) => Err(hr_msg(e)),
        }
    })
    .await
}

// ---------------------------------------------------------------------------
// Open with: handler enumeration
// ---------------------------------------------------------------------------

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OpenWithApp {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub icon: Option<String>,
}

const MAX_OPEN_WITH: usize = 15;

/// `.ext` of a file path; `None` for folders and extension-less names.
fn file_extension(path: &str) -> Option<String> {
    if std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) {
        return None;
    }
    let ext = Path::new(path).extension()?.to_str()?;
    if ext.is_empty() {
        return None;
    }
    Some(format!(".{ext}"))
}

fn assoc_string(kind: ASSOCSTR, ext: &str) -> Option<String> {
    let ext = HSTRING::from(ext);
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            kind,
            &ext,
            PCWSTR::null(),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
    };
    if hr.is_err() {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    (!s.is_empty()).then_some(s)
}

fn enum_handlers(ext: &str) -> Result<Vec<IAssocHandler>, String> {
    let en = unsafe { SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_RECOMMENDED) }
        .map_err(hr_msg)?;
    let mut out = Vec::new();
    loop {
        let mut slot: [Option<IAssocHandler>; 1] = [None];
        let mut fetched = 0u32;
        if unsafe { en.Next(&mut slot, Some(&mut fetched)) }.is_err() || fetched == 0 {
            break;
        }
        if let Some(h) = slot[0].take() {
            out.push(h);
        }
        if out.len() > 64 {
            break;
        }
    }
    Ok(out)
}

fn handler_name(h: &IAssocHandler) -> String {
    unsafe { h.GetName() }.map(take_pwstr).unwrap_or_default()
}

fn handler_ui_name(h: &IAssocHandler) -> String {
    unsafe { h.GetUIName() }.map(take_pwstr).unwrap_or_default()
}

fn handler_icon(h: &IAssocHandler) -> Option<String> {
    let mut loc = PWSTR::null();
    let mut index = 0i32;
    unsafe { h.GetIconLocation(&mut loc, &mut index) }.ok()?;
    let loc = take_pwstr(loc);
    let png = icon_png_from_location(&loc, index, 32)?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// Expands `%VAR%` references (registry icon paths often use them).
fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// PNG for an icon location as returned by `IAssocHandler::GetIconLocation`:
/// either `path,index` style (exe/dll/ico) or an indirect `@{Package?ms-resource://…}`
/// string for packaged apps.
pub(crate) fn icon_png_from_location(loc: &str, index: i32, size: u32) -> Option<Vec<u8>> {
    if loc.is_empty() {
        return None;
    }
    if loc.starts_with('@') {
        let mut buf = vec![0u16; 2048];
        unsafe { SHLoadIndirectString(&HSTRING::from(loc), &mut buf, None) }.ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let file = String::from_utf16_lossy(&buf[..n]);
        if file.to_ascii_lowercase().ends_with(".png") {
            if let Ok(bytes) = std::fs::read(&file) {
                return Some(bytes);
            }
        }
        return shell_image_png(&file, size, SIIGBF_RESIZETOFIT).ok();
    }
    let file = expand_env(loc.trim_matches('"'));
    unsafe {
        let mut hicon = HICON::default();
        let hr = SHDefExtractIconW(
            &HSTRING::from(file.as_str()),
            index,
            0,
            Some(&mut hicon),
            None,
            size,
        );
        if hr.is_ok() && !hicon.is_invalid() {
            let png = hicon_to_png(hicon);
            let _ = DestroyIcon(hicon);
            if png.is_some() {
                return png;
            }
        }
    }
    shell_image_png(&file, size, SIIGBF_ICONONLY).ok()
}

fn list_open_with_blocking(path: &str) -> Result<Vec<OpenWithApp>, String> {
    let path = paths::normalize(path);
    let Some(ext) = file_extension(&path) else {
        return Ok(Vec::new());
    };
    let handlers = enum_handlers(&ext)?;
    let default_exe = assoc_string(ASSOCSTR_EXECUTABLE, &ext).map(|s| s.to_lowercase());
    let default_name = assoc_string(ASSOCSTR_FRIENDLYAPPNAME, &ext);

    let mut apps: Vec<(OpenWithApp, IAssocHandler)> = Vec::new();
    for h in handlers {
        let id = handler_name(&h);
        if id.is_empty() || apps.iter().any(|(a, _)| a.id.eq_ignore_ascii_case(&id)) {
            continue;
        }
        let name = match handler_ui_name(&h) {
            n if n.is_empty() => id.clone(),
            n => n,
        };
        apps.push((
            OpenWithApp {
                id,
                name,
                is_default: false,
                icon: None,
            },
            h,
        ));
    }

    // The default handler: match the associated executable first, then the
    // friendly app name (packaged apps have no executable path as their id).
    let default_idx = default_exe
        .as_ref()
        .and_then(|exe| apps.iter().position(|(a, _)| a.id.to_lowercase() == *exe))
        .or_else(|| {
            default_name.as_ref().and_then(|n| {
                apps.iter()
                    .position(|(a, _)| a.name.eq_ignore_ascii_case(n))
            })
        });
    if let Some(i) = default_idx {
        let mut entry = apps.remove(i);
        entry.0.is_default = true;
        apps.insert(0, entry);
    }
    apps.truncate(MAX_OPEN_WITH);
    Ok(apps
        .into_iter()
        .map(|(mut app, h)| {
            app.icon = handler_icon(&h);
            app
        })
        .collect())
}

/// Recommended "Open with" apps for `path` (default first, max 15). Folders
/// and extension-less files → `[]`.
#[tauri::command]
pub async fn list_open_with(path: String) -> Result<Vec<OpenWithApp>, String> {
    blocking_com(move || list_open_with_blocking(&path)).await
}

fn open_with_blocking(path: &str, app_id: &str) -> Result<(), String> {
    let path = paths::normalize(path);
    let ext = file_extension(&path).ok_or("No handlers for this item")?;
    let handler = enum_handlers(&ext)?
        .into_iter()
        .find(|h| handler_name(h).eq_ignore_ascii_case(app_id))
        .ok_or_else(|| format!("App not found: {app_id}"))?;
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_str()), None) }
            .map_err(hr_msg)?;
    let data: IDataObject =
        unsafe { item.BindToHandler(None, &BHID_DataObject) }.map_err(hr_msg)?;
    unsafe { handler.Invoke(&data) }.map_err(hr_msg)
}

/// Opens `path` with the handler whose id (from `list_open_with`) is `app_id`.
#[tauri::command]
pub async fn open_with(path: String, app_id: String) -> Result<(), String> {
    blocking_com(move || open_with_blocking(&path, &app_id)).await
}

// ---------------------------------------------------------------------------
// File clipboard (CF_HDROP + "Preferred DropEffect"), Explorer compatible
// ---------------------------------------------------------------------------

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardFiles {
    pub paths: Vec<String>,
    pub cut: bool,
}

/// DROPFILES header (`fWide = TRUE`) + NUL-terminated wide paths + final NUL.
fn build_dropfiles(paths_in: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DROPFILES_HEADER + paths_in.len() * 64);
    out.extend_from_slice(&(DROPFILES_HEADER as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0i32.to_le_bytes()); // fNC
    out.extend_from_slice(&1i32.to_le_bytes()); // fWide
    for p in paths_in {
        for unit in p.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Inverse of [`build_dropfiles`] (wide lists only).
#[cfg(test)]
fn parse_dropfiles(bytes: &[u8]) -> Vec<String> {
    let off = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let units: Vec<u16> = bytes[off..]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    units
        .split(|&u| u == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn drop_effect_bytes(cut: bool) -> [u8; 4] {
    let effect = if cut {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY | DROPEFFECT_LINK
    };
    effect.to_le_bytes()
}

fn is_cut_effect(effect: u32) -> bool {
    effect & DROPEFFECT_MOVE != 0 && effect & DROPEFFECT_COPY == 0
}

fn preferred_drop_effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(&HSTRING::from("Preferred DropEffect")) }
}

struct ClipboardGuard;

impl ClipboardGuard {
    /// `OpenClipboard` fails while another process holds it; retry briefly.
    fn open() -> Result<Self, String> {
        for attempt in 0..12u64 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(ClipboardGuard);
            }
            std::thread::sleep(std::time::Duration::from_millis(10 + attempt * 10));
        }
        Err("The clipboard is busy".into())
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Puts `bytes` on the (open) clipboard as `format`; ownership passes to the
/// system on success.
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let mem: HGLOBAL =
            GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len().max(1)).map_err(hr_msg)?;
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(mem);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(hr_msg(e));
        }
    }
    Ok(())
}

/// Copy of the clipboard data for `format`, if present.
fn get_clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        IsClipboardFormatAvailable(format).ok()?;
        let h = GetClipboardData(format).ok()?;
        let mem = HGLOBAL(h.0);
        let size = GlobalSize(mem);
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(mem);
        Some(v)
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    let mut out: Vec<u8> = s.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.extend_from_slice(&[0, 0]);
    out
}

fn clipboard_write_blocking(paths_in: &[String], cut: bool) -> Result<(), String> {
    let list: Vec<String> = paths_in.iter().map(|p| paths::normalize(p)).collect();
    if list.is_empty() {
        return Err("Nothing to put on the clipboard".into());
    }
    let effect_fmt = preferred_drop_effect_format();
    // Shell IDList Array as well, like Explorer's own copy: it is what the
    // shell needs for "Paste shortcut". Optional (needs COM; skipped if any
    // path can't be parsed).
    let idlist_fmt = unsafe { RegisterClipboardFormatW(&HSTRING::from("Shell IDList Array")) };
    let idlist = {
        let _com = ComGuard::init();
        list.iter()
            .map(|p| Pidl::parse(p).map(|pidl| pidl.to_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .ok()
            .map(|pidls| build_cida(&pidls))
    };
    let _clip = ClipboardGuard::open()?;
    unsafe { EmptyClipboard() }.map_err(hr_msg)?;
    set_clipboard_bytes(CF_HDROP, &build_dropfiles(&list))?;
    if effect_fmt != 0 {
        set_clipboard_bytes(effect_fmt, &drop_effect_bytes(cut))?;
    }
    if let (Some(cida), true) = (idlist, idlist_fmt != 0) {
        let _ = set_clipboard_bytes(idlist_fmt, &cida);
    }
    set_clipboard_bytes(CF_UNICODETEXT, &utf16z(&list.join("\r\n")))?;
    Ok(())
}

/// CIDA ("Shell IDList Array"): `cidl`, `cidl + 1` offsets, then the parent
/// PIDL (the desktop: empty) and the items' absolute PIDLs (each with its
/// 2-byte terminator).
fn build_cida(pidls: &[Vec<u8>]) -> Vec<u8> {
    let header = 4 + 4 * (pidls.len() + 1);
    let mut offsets = Vec::with_capacity(pidls.len() + 1);
    let mut body: Vec<u8> = vec![0, 0]; // empty parent PIDL
    offsets.push(header as u32);
    for p in pidls {
        offsets.push((header + body.len()) as u32);
        body.extend_from_slice(p);
    }
    let mut out = Vec::with_capacity(header + body.len());
    out.extend_from_slice(&(pidls.len() as u32).to_le_bytes());
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&body);
    out
}

fn clipboard_read_blocking() -> Result<Option<ClipboardFiles>, String> {
    let effect_fmt = preferred_drop_effect_format();
    let _clip = ClipboardGuard::open()?;
    let paths_out = unsafe {
        if IsClipboardFormatAvailable(CF_HDROP).is_err() {
            return Ok(None);
        }
        let Ok(h) = GetClipboardData(CF_HDROP) else {
            return Ok(None);
        };
        let hdrop = HDROP(h.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut v = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf));
            if got > 0 {
                v.push(String::from_utf16_lossy(&buf[..got as usize]));
            }
        }
        v
    };
    if paths_out.is_empty() {
        return Ok(None);
    }
    let cut = (effect_fmt != 0)
        .then(|| get_clipboard_bytes(effect_fmt))
        .flatten()
        .filter(|b| b.len() >= 4)
        .map(|b| is_cut_effect(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        .unwrap_or(false);
    Ok(Some(ClipboardFiles {
        paths: paths_out,
        cut,
    }))
}

/// Puts files on the clipboard the way Explorer does (CF_HDROP + Preferred
/// DropEffect + text), so Ctrl+V in Explorer pastes (or moves, if `cut`).
#[tauri::command]
pub async fn clipboard_write_files(paths: Vec<String>, cut: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || clipboard_write_blocking(&paths, cut))
        .await
        .map_err(|e| e.to_string())?
}

/// Files currently on the clipboard (from Explorer or us); `null` if none.
#[tauri::command]
pub async fn clipboard_read_files() -> Result<Option<ClipboardFiles>, String> {
    tauri::async_runtime::spawn_blocking(clipboard_read_blocking)
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Thumbnails and icons
// ---------------------------------------------------------------------------

/// 32-bpp BGRA (possibly premultiplied) → straight RGBA. A bitmap whose alpha
/// is all zero is treated as opaque. Data that is not actually premultiplied
/// (some channel > alpha) is passed through unchanged.
pub(crate) fn bgra_to_rgba(bgra: &[u8], premultiplied: bool) -> Vec<u8> {
    let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
    let premul = premultiplied
        && has_alpha
        && bgra
            .chunks_exact(4)
            .all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]);
    let mut out = Vec::with_capacity(bgra.len());
    for p in bgra.chunks_exact(4) {
        let (b, g, r, a) = (p[0], p[1], p[2], p[3]);
        if !has_alpha {
            out.extend_from_slice(&[r, g, b, 255]);
        } else if premul && a != 255 {
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let un = |c: u8| ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
                out.extend_from_slice(&[un(r), un(g), un(b), a]);
            }
        } else {
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

/// Top-down 32-bpp BGRA pixels of a bitmap → (width, height, bytes).
pub(crate) fn bitmap_bgra(hbm: HBITMAP) -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            hbm.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut c_void),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
            return Err("invalid bitmap".into());
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negative → top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; w as usize * h as usize * 4];
        let hdc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            hdc,
            hbm,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(hdc);
        if lines == 0 {
            return Err("GetDIBits failed".into());
        }
        Ok((w as u32, h as u32, buf))
    }
}

pub(crate) fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// HICON → PNG. Icon colour bitmaps carry straight (non-premultiplied) alpha;
/// legacy icons without alpha get it from the AND mask.
fn hicon_to_png(hicon: HICON) -> Option<Vec<u8>> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(hicon, &mut ii).ok()?;
        let color = (!ii.hbmColor.is_invalid()).then(|| bitmap_bgra(ii.hbmColor));
        let mask = (!ii.hbmMask.is_invalid()).then(|| bitmap_bgra(ii.hbmMask));
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
        let (w, h, mut bgra) = color?.ok()?;
        if !bgra.chunks_exact(4).any(|p| p[3] != 0) {
            if let Some(Ok((mw, mh, m))) = mask {
                if mw == w && mh >= h {
                    for (px, mp) in bgra.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
                        // AND mask: white (1) = transparent.
                        px[3] = if mp[0] > 127 { 0 } else { 255 };
                    }
                }
            }
        }
        encode_png(w, h, &bgra_to_rgba(&bgra, false)).ok()
    }
}

fn shell_item(path: &str) -> Result<IShellItem, String> {
    let try_create = |p: &str| unsafe {
        SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(p), None)
    };
    match try_create(path) {
        Ok(item) => Ok(item),
        Err(e) => {
            let alt = paths::for_io(path);
            if alt != path {
                if let Ok(item) = try_create(&alt) {
                    return Ok(item);
                }
            }
            Err(hr_msg(e))
        }
    }
}

/// Concurrent icon extraction with a cold icon cache sometimes hands back the
/// generic placeholder icon (observed for drives and folders), so icon-only
/// requests are serialised. Icons are cheap; thumbnails stay parallel.
static ICON_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `IShellItemImageFactory::GetImage` → PNG. COM must be initialised.
fn shell_image_png(path: &str, size: u32, flags: SIIGBF) -> Result<Vec<u8>, String> {
    let item = shell_item(path)?;
    let factory: IShellItemImageFactory = item.cast().map_err(hr_msg)?;
    let side = SIZE {
        cx: size.clamp(1, 2560) as i32,
        cy: size.clamp(1, 2560) as i32,
    };
    // The icon cache answers E_PENDING while another thread is extracting the
    // same icon; it settles within a few ms.
    let mut attempt = 0u64;
    let hbm = loop {
        match unsafe { factory.GetImage(side, flags) } {
            Ok(hbm) => break hbm,
            Err(e) if e.code() == E_PENDING && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(5 * attempt));
            }
            Err(e) => return Err(hr_msg(e)),
        }
    };
    let pixels = bitmap_bgra(hbm);
    unsafe {
        let _ = DeleteObject(hbm.into());
    }
    let (w, h, bgra) = pixels?;
    encode_png(w, h, &bgra_to_rgba(&bgra, true))
}

/// PNG thumbnail (`icon_only = false`) or shell icon (`icon_only = true`) of
/// `path`, roughly `size`×`size` (may be larger; aspect ratio preserved).
/// Thread-safe: initialises COM for the call. With `icon_only = false`, items
/// without a thumbnail handler yield `Err("no-thumbnail")`.
pub fn thumbnail_png(path: &str, size: u32, icon_only: bool) -> Result<Vec<u8>, String> {
    let _com = ComGuard::init();
    let path = paths::normalize(path);
    if icon_only {
        let _serial = ICON_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        shell_image_png(&path, size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
    } else {
        shell_image_png(&path, size, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK)
            .map_err(|_| "no-thumbnail".to_string())
    }
}

// ---------------------------------------------------------------------------
// Terminal / VS Code / Explorer
// ---------------------------------------------------------------------------

/// `\\wsl.localhost\Distro\home\me` → `("Distro", "/home/me")`.
fn wsl_linux_path(p: &str) -> Option<(String, String)> {
    let n = paths::normalize(p);
    let distro = paths::wsl_distro(&n)?;
    let rest = &n[paths::wsl_root(&distro).len()..];
    let linux = if rest.trim_matches('\\').is_empty() {
        "/".to_string()
    } else {
        rest.replace('\\', "/")
    };
    Some((distro, linux))
}

/// `wt.exe` splits its command line on `;` — escape it.
fn escape_wt(s: &str) -> String {
    s.replace(';', r"\;")
}

/// PowerShell single-quoted literal (also doubles the typographic quotes
/// PowerShell treats as single quotes).
fn ps_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

fn find_wt() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    let p = PathBuf::from(local).join(r"Microsoft\WindowsApps\wt.exe");
    // App execution aliases are reparse points that can't be followed by
    // metadata(); just check the link exists.
    std::fs::symlink_metadata(&p).is_ok().then_some(p)
}

/// `ShellExecuteExW` without std-handle inheritance — used to open a fresh
/// console program (it gets its own console window).
fn shell_execute(file: &str, params: &str, dir: Option<&str>) -> Result<(), String> {
    let file = HSTRING::from(file);
    let params = HSTRING::from(params);
    let dir = dir.map(HSTRING::from);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
        nShow: SW_SHOWNORMAL.0,
        ..unsafe { std::mem::zeroed() }
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(hr_msg)
}

fn spawn_hidden(mut cmd: Command) -> std::io::Result<()> {
    cmd.creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn open_terminal_blocking(path: &str) -> Result<(), String> {
    let path = paths::normalize(path);
    let wsl = wsl_linux_path(&path);
    if let Some(wt) = find_wt() {
        let mut cmd = Command::new(&wt);
        match &wsl {
            Some((distro, linux)) => {
                cmd.args([
                    "wsl.exe",
                    "-d",
                    &escape_wt(distro),
                    "--cd",
                    &escape_wt(linux),
                ]);
            }
            None => {
                cmd.arg("-d").arg(escape_wt(&paths::for_io(&path)));
            }
        }
        if spawn_hidden(cmd).is_ok() {
            return Ok(());
        }
    }
    match wsl {
        Some((distro, linux)) => shell_execute(
            "wsl.exe",
            &format!("-d \"{distro}\" --cd \"{linux}\""),
            None,
        ),
        None => shell_execute(
            "powershell.exe",
            &format!(
                "-NoExit -Command \"Set-Location -LiteralPath {}\"",
                ps_quote(&paths::for_io(&path))
            ),
            Some(&paths::for_io(&path)),
        ),
    }
}

/// Opens a terminal in folder `path` (Windows Terminal if installed, else a
/// PowerShell console; WSL paths open a shell in that distro).
#[tauri::command]
pub async fn open_terminal(path: String) -> Result<(), String> {
    blocking_com(move || open_terminal_blocking(&path)).await
}

fn find_code_cmd() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join("code.cmd");
            if c.is_file() {
                return Some(c);
            }
        }
    }
    let mut candidates = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join(r"Programs\Microsoft VS Code\bin\code.cmd"));
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(pf) = std::env::var_os(var) {
            candidates.push(PathBuf::from(pf).join(r"Microsoft VS Code\bin\code.cmd"));
        }
    }
    candidates.into_iter().find(|c| c.is_file())
}

/// Arguments for `code` (`--remote wsl+<distro> /path` for WSL paths).
fn vscode_args(path: &str) -> Vec<String> {
    match wsl_linux_path(path) {
        Some((distro, linux)) => vec!["--remote".into(), format!("wsl+{distro}"), linux],
        None => vec![paths::for_io(&paths::normalize(path))],
    }
}

/// `"…"` per the MSVC argv rules: trailing backslashes are doubled so `C:\`
/// doesn't escape the closing quote. (Paths never contain `"`.)
fn quote_arg(a: &str) -> String {
    let trailing = a.len() - a.trim_end_matches('\\').len();
    format!("\"{a}{}\"", "\\".repeat(trailing))
}

/// `cmd /d /s /c ""script.cmd" "arg" …"` — `/s` strips only the outer quotes,
/// so the script path and arguments may contain spaces.
fn cmd_script_command(script: &Path, args: &[String]) -> Command {
    let quoted: Vec<String> = args.iter().map(|a| quote_arg(a)).collect();
    let mut cmd = Command::new("cmd.exe");
    cmd.raw_arg(format!(
        "/d /s /c \"\"{}\" {}\"",
        script.display(),
        quoted.join(" ")
    ));
    cmd
}

fn open_in_vscode_blocking(path: &str) -> Result<(), String> {
    let code =
        find_code_cmd().ok_or("VS Code is not installed (the `code` command was not found)")?;
    spawn_hidden(cmd_script_command(&code, &vscode_args(path)))
        .map_err(|e| format!("Cannot start VS Code: {e}"))
}

/// Opens `path` (file or folder) in VS Code.
#[tauri::command]
pub async fn open_in_vscode(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || open_in_vscode_blocking(&path))
        .await
        .map_err(|e| e.to_string())?
}

fn reveal_blocking(path: &str) -> Result<(), String> {
    let path = paths::normalize(path);
    let shell = Pidl::parse(&path).and_then(|pidl| {
        unsafe { SHOpenFolderAndSelectItems(pidl.ptr(), None, 0) }.map_err(hr_msg)
    });
    if shell.is_ok() {
        return Ok(());
    }
    let mut cmd = Command::new("explorer.exe");
    cmd.raw_arg(format!("/select,\"{path}\""));
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("Cannot open Explorer: {e}"))
}

/// Opens an Explorer window on the parent folder with `path` selected.
#[tauri::command]
pub async fn reveal_in_explorer(path: String) -> Result<(), String> {
    blocking_com(move || reveal_blocking(&path)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropfiles_round_trip() {
        let list = vec![
            r"C:\Users\me\file.txt".to_string(),
            r"C:\".to_string(),
            r"\\wsl.localhost\Ubuntu\home\me\ünï cødé.md".to_string(),
        ];
        let bytes = build_dropfiles(&list);
        assert_eq!(&bytes[0..4], &20u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &1i32.to_le_bytes(), "fWide");
        assert!(bytes.ends_with(&[0, 0, 0, 0]), "double NUL terminated");
        assert_eq!(parse_dropfiles(&bytes), list);
    }

    #[test]
    fn cida_layout() {
        let a = vec![4u8, 0, 0xAA, 0xBB, 0, 0]; // one 4-byte id + terminator
        let b = vec![3u8, 0, 0xCC, 0, 0];
        let c = build_cida(&[a.clone(), b.clone()]);
        let u = |i: usize| u32::from_le_bytes(c[i..i + 4].try_into().unwrap()) as usize;
        assert_eq!(u(0), 2); // cidl
        let (parent, first, second) = (u(4), u(8), u(12));
        assert_eq!(parent, 16);
        assert_eq!(&c[parent..parent + 2], &[0, 0]);
        assert_eq!(&c[first..first + a.len()], &a[..]);
        assert_eq!(&c[second..], &b[..]);
    }

    #[test]
    fn drop_effects() {
        assert_eq!(drop_effect_bytes(true), 2u32.to_le_bytes());
        assert_eq!(drop_effect_bytes(false), 5u32.to_le_bytes());
        assert!(is_cut_effect(2));
        assert!(!is_cut_effect(5));
        assert!(!is_cut_effect(3));
        assert!(!is_cut_effect(1));
    }

    #[test]
    fn wsl_paths() {
        assert_eq!(
            wsl_linux_path(r"\\wsl.localhost\Ubuntu\home\me"),
            Some(("Ubuntu".into(), "/home/me".into()))
        );
        assert_eq!(
            wsl_linux_path(r"\\wsl$\Arch\etc\"),
            Some(("Arch".into(), "/etc".into()))
        );
        assert_eq!(
            wsl_linux_path(r"\\wsl.localhost\Ubuntu"),
            Some(("Ubuntu".into(), "/".into()))
        );
        assert_eq!(wsl_linux_path(r"C:\Users"), None);
        assert_eq!(
            vscode_args(r"\\wsl.localhost\Ubuntu\home\me\My Proj"),
            vec!["--remote", "wsl+Ubuntu", "/home/me/My Proj"]
        );
        assert_eq!(vscode_args(r"C:\a b"), vec![r"C:\a b"]);
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_arg(r"C:\a b"), r#""C:\a b""#);
        assert_eq!(quote_arg(r"C:\"), r#""C:\\""#);
        assert_eq!(quote_arg(r"\\srv\share\"), r#""\\srv\share\\""#);
        assert_eq!(escape_wt(r"C:\a;b"), r"C:\a\;b");
        assert_eq!(ps_quote(r"C:\it's"), r"'C:\it''s'");
        assert_eq!(expand_env("%NO_SUCH_VAR_X%\\a"), "%NO_SUCH_VAR_X%\\a");
        // SAFETY: test-only variable that no other test reads.
        unsafe { std::env::set_var("FERRET_TEST_VAR", "X") };
        assert_eq!(expand_env("%FERRET_TEST_VAR%\\a%"), "X\\a%");
    }

    #[test]
    fn containers() {
        assert_eq!(container_key(r"C:\a\b"), container_key(r"c:\A\c"));
        assert_eq!(container_key(r"C:\"), container_key(r"D:\"));
        assert_ne!(
            container_key(r"C:\"),
            container_key(r"\\wsl.localhost\Ubuntu")
        );
        assert_ne!(container_key(r"C:\a\b"), container_key(r"C:\b\c"));
    }

    #[test]
    fn rgba_conversion() {
        // all-zero alpha → opaque, channels swapped
        assert_eq!(bgra_to_rgba(&[1, 2, 3, 0], true), vec![3, 2, 1, 255]);
        // premultiplied half-transparent white → straight white @ 128
        assert_eq!(
            bgra_to_rgba(&[128, 128, 128, 128, 0, 0, 0, 0], true),
            vec![255, 255, 255, 128, 0, 0, 0, 0]
        );
        // channel > alpha → not premultiplied, pass through
        assert_eq!(
            bgra_to_rgba(&[200, 10, 10, 100], true),
            vec![10, 10, 200, 100]
        );
        // straight input stays straight
        assert_eq!(
            bgra_to_rgba(&[50, 60, 70, 128], false),
            vec![70, 60, 50, 128]
        );
        // opaque untouched
        assert_eq!(bgra_to_rgba(&[9, 8, 7, 255], true), vec![7, 8, 9, 255]);
    }
}

/// Live checks against the real shell. Run with
/// `cargo test --lib shell::live -- --ignored --nocapture --test-threads=1`.
/// `FERRET_SHELL_OUT` = output dir for PNGs, `FERRET_THUMB_FILES` = `|`-separated
/// files to thumbnail.
#[cfg(test)]
mod live {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetMenuItemCount, GetMenuItemID, GetMenuStringW,
        GetSubMenu, SendMessageW, MF_BYPOSITION, WINDOW_EX_STYLE, WS_POPUP,
    };

    fn out_dir() -> PathBuf {
        let d = std::env::var_os("FERRET_SHELL_OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("ferret-shell-test"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_dims(png: &[u8]) -> (u32, u32) {
        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
        (w, h)
    }

    #[test]
    #[ignore]
    fn thumbnails() {
        let out = out_dir();
        let windir = std::env::var("WINDIR").unwrap_or(r"C:\Windows".into());
        let mut cases: Vec<(String, String, u32, bool)> = vec![
            (
                "explorer-icon".into(),
                format!(r"{windir}\explorer.exe"),
                48,
                true,
            ),
            ("folder-icon".into(), windir.clone(), 48, true),
            ("drive-icon".into(), r"C:\".into(), 48, true),
            (
                "explorer-thumb".into(),
                format!(r"{windir}\explorer.exe"),
                256,
                false,
            ),
        ];
        if let Ok(files) = std::env::var("FERRET_THUMB_FILES") {
            for (i, f) in files.split('|').filter(|s| !s.is_empty()).enumerate() {
                cases.push((format!("file{i}-thumb"), f.to_string(), 256, false));
                cases.push((format!("file{i}-icon"), f.to_string(), 32, true));
            }
        }
        // Concurrency from arbitrary threads.
        let handles: Vec<_> = cases
            .into_iter()
            .map(|(name, path, size, icon)| {
                let out = out.clone();
                std::thread::spawn(move || {
                    let r = thumbnail_png(&path, size, icon);
                    match &r {
                        Ok(png) => {
                            std::fs::write(out.join(format!("{name}.png")), png).unwrap();
                            println!("{name}: {:?} {} bytes  <- {path}", png_dims(png), png.len());
                        }
                        Err(e) => println!("{name}: ERR {e}  <- {path}"),
                    }
                    (name, r.is_ok())
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for (name, ok) in &results {
            if name.ends_with("-icon") {
                assert!(ok, "{name} failed");
            }
        }
        // exe has no thumbnail handler → caller falls back to the icon.
        assert_eq!(
            thumbnail_png(&format!(r"{windir}\explorer.exe"), 256, false),
            Err("no-thumbnail".to_string())
        );
    }

    /// Concurrent icon extraction with a cold icon cache must give the same
    /// result as sequential extraction (no placeholder icons).
    #[test]
    #[ignore]
    fn icons_concurrent() {
        let windir = std::env::var("WINDIR").unwrap();
        let mut list: Vec<String> = vec![
            r"C:\".into(),
            r"D:\".into(),
            r"E:\".into(),
            windir.clone(),
            format!(r"{windir}\explorer.exe"),
            format!(r"{windir}\notepad.exe"),
            format!(r"{windir}\regedit.exe"),
            format!(r"{windir}\System32"),
            format!(r"{windir}\System32\cmd.exe"),
            format!(r"{windir}\System32\mspaint.exe"),
        ];
        if let Ok(home) = std::env::var("USERPROFILE") {
            for sub in [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Videos",
                "Music",
            ] {
                list.push(format!(r"{home}\{sub}"));
            }
        }
        let handles: Vec<_> = list
            .iter()
            .cloned()
            .flat_map(|p| (0..3).map(move |_| p.clone()))
            .map(|p| std::thread::spawn(move || (p.clone(), thumbnail_png(&p, 48, true))))
            .collect();
        let concurrent: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let mut bad = 0;
        for (p, r) in &concurrent {
            let reference = thumbnail_png(p, 48, true);
            if r != &reference {
                bad += 1;
                println!(
                    "MISMATCH {p}: concurrent={:?} sequential={:?}",
                    r.as_ref().map(|v| v.len()),
                    reference.as_ref().map(|v| v.len())
                );
            }
        }
        println!("{} lookups, {bad} mismatches", concurrent.len());
        assert_eq!(bad, 0);
    }

    /// The cmd.exe wrapper used for `code.cmd` passes paths with spaces intact
    /// (script path with spaces too); launchers are found on this machine.
    #[test]
    #[ignore]
    fn launchers() {
        let dir = std::env::temp_dir().join("ferret cmd test");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("echo args.cmd");
        std::fs::write(&script, "@echo off\r\necho [%1] [%2] [%3]\r\n").unwrap();
        let out = cmd_script_command(
            &script,
            &vscode_args(r"\\wsl.localhost\Ubuntu\home\me\My Proj & co"),
        )
        .output()
        .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        println!("cmd output: {text}");
        assert_eq!(
            text.trim(),
            r#"["--remote"] ["wsl+Ubuntu"] ["/home/me/My Proj & co"]"#
        );
        let out = cmd_script_command(&script, &vscode_args(r"C:\"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            r#"["C:\\"] [] []"#
        );
        let _ = std::fs::remove_dir_all(&dir);
        println!("wt: {:?}\ncode: {:?}", find_wt(), find_code_cmd());
        assert!(find_wt().is_some());
        assert!(find_code_cmd().is_some());
    }

    /// Visible top-level windows of this process: (hwnd, title, class).
    fn own_windows() -> Vec<(isize, String, String)> {
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        };
        unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
            let out = unsafe { &mut *(lp.0 as *mut Vec<(isize, String, String)>) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            if pid == std::process::id() && unsafe { IsWindowVisible(h) }.as_bool() {
                let (mut t, mut c) = ([0u16; 256], [0u16; 256]);
                let n = unsafe { GetWindowTextW(h, &mut t) } as usize;
                let m = unsafe { GetClassNameW(h, &mut c) } as usize;
                out.push((
                    h.0 as isize,
                    String::from_utf16_lossy(&t[..n]),
                    String::from_utf16_lossy(&c[..m]),
                ));
            }
            true.into()
        }
        let mut v: Vec<(isize, String, String)> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut v as *mut _ as isize));
        }
        v
    }

    /// SHObjectProperties / SHMultiFileProperties return immediately (the sheet
    /// lives on its own thread). The sheets are closed again right away.
    #[test]
    #[ignore]
    fn properties_do_not_block() {
        use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let dir = std::env::temp_dir();
        let a = dir.join("ferret-props-a.txt");
        let b = dir.join("ferret-props-b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let close_sheets = |expect: &str| {
            let found = pump_until(5000, || {
                own_windows()
                    .iter()
                    .any(|(_, t, c)| c == "#32770" && t.contains(expect))
            });
            let wins = own_windows();
            for (h, t, c) in &wins {
                if c == "#32770" {
                    println!("  sheet window: {t:?}");
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(*h as *mut c_void)),
                            WM_CLOSE,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
            pump_until(3000, || {
                !own_windows().iter().any(|(_, _, c)| c == "#32770")
            });
            found
        };

        let t = std::time::Instant::now();
        let ok = unsafe {
            SHObjectProperties(
                Some(hwnd),
                SHOP_FILEPATH,
                &HSTRING::from(a.display().to_string()),
                PCWSTR::null(),
            )
        };
        println!(
            "SHObjectProperties returned {} after {:?}",
            ok.as_bool(),
            t.elapsed()
        );
        assert!(ok.as_bool());
        assert!(t.elapsed().as_millis() < 1500, "blocked");
        assert!(
            close_sheets("ferret-props-a"),
            "single sheet never appeared"
        );

        let pidls: Vec<Pidl> = [&a, &b]
            .iter()
            .map(|p| Pidl::parse(&p.display().to_string()).unwrap())
            .collect();
        let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(Pidl::ptr).collect();
        let array: IShellItemArray = unsafe { SHCreateShellItemArrayFromIDLists(&ptrs) }.unwrap();
        let data: IDataObject = unsafe { array.BindToHandler(None, &BHID_DataObject) }.unwrap();
        let t = std::time::Instant::now();
        unsafe { SHMultiFileProperties(&data, 0) }.unwrap();
        println!("SHMultiFileProperties returned after {:?}", t.elapsed());
        assert!(t.elapsed().as_millis() < 1500, "blocked");
        assert!(close_sheets(""), "multi sheet never appeared");

        unsafe { DestroyWindow(hwnd) }.unwrap();
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    #[ignore]
    fn clipboard_round_trip() {
        // Preserve whatever text the user had on the clipboard.
        let saved_text = {
            let _c = ClipboardGuard::open().unwrap();
            get_clipboard_bytes(CF_UNICODETEXT)
        };
        let dir = std::env::temp_dir();
        let a = dir.join("ferret clip ä.txt");
        let b = dir.join("ferret-clip-b.txt");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let list = vec![a.display().to_string(), b.display().to_string()];

        clipboard_write_blocking(&list, true).unwrap();
        let got = clipboard_read_blocking()
            .unwrap()
            .expect("files on clipboard");
        assert_eq!(
            got,
            ClipboardFiles {
                paths: list.clone(),
                cut: true
            }
        );
        {
            let _c = ClipboardGuard::open().unwrap();
            let text = get_clipboard_bytes(CF_UNICODETEXT).unwrap();
            let units: Vec<u16> = text
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let n = units.iter().position(|&u| u == 0).unwrap();
            assert_eq!(String::from_utf16_lossy(&units[..n]), list.join("\r\n"));
        }
        clipboard_write_blocking(&list[..1], false).unwrap();
        let got = clipboard_read_blocking().unwrap().unwrap();
        assert_eq!(
            got,
            ClipboardFiles {
                paths: list[..1].to_vec(),
                cut: false
            }
        );
        println!("clipboard round trip ok: {got:?}");

        // Restore.
        {
            let _c = ClipboardGuard::open().unwrap();
            unsafe { EmptyClipboard() }.unwrap();
            if let Some(t) = saved_text {
                set_clipboard_bytes(CF_UNICODETEXT, &t).unwrap();
            }
        }
        assert_eq!(clipboard_read_blocking().unwrap(), None);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    #[ignore]
    fn open_with_lists() {
        let _com = ComGuard::init();
        let out = out_dir();
        let dir = std::env::temp_dir();
        for ext in ["txt", "png", "pdf", "zip"] {
            let f = dir.join(format!("ferret-openwith.{ext}"));
            std::fs::write(&f, b"x").unwrap();
            println!(
                "--- .{ext}  exe={:?} friendly={:?}",
                assoc_string(ASSOCSTR_EXECUTABLE, &format!(".{ext}")),
                assoc_string(ASSOCSTR_FRIENDLYAPPNAME, &format!(".{ext}"))
            );
            let apps = list_open_with_blocking(&f.display().to_string()).unwrap();
            for (i, a) in apps.iter().enumerate() {
                println!(
                    "  default={} id={:?} name={:?} icon={}",
                    a.is_default,
                    a.id,
                    a.name,
                    a.icon
                        .as_ref()
                        .map_or("none".to_string(), |s| format!("{} chars", s.len()))
                );
                if let Some(icon) = &a.icon {
                    let b64 = icon.trim_start_matches("data:image/png;base64,");
                    let png = base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .unwrap();
                    std::fs::write(out.join(format!("openwith-{ext}-{i}.png")), png).unwrap();
                }
            }
            if ext == "txt" || ext == "png" {
                assert!(!apps.is_empty(), "no apps for .{ext}");
            }
            let _ = std::fs::remove_file(f);
        }
        assert!(list_open_with_blocking(&dir.display().to_string())
            .unwrap()
            .is_empty());
    }

    fn hidden_window() -> HWND {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &HSTRING::from("STATIC"),
                &HSTRING::from("ferret-test"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap()
    }

    fn pump_until(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        done()
    }

    /// Builds the menu like `track_context_menu`, then acts as if the user
    /// clicked the (enabled) item `id`. Returns what the command would return.
    fn choose(cm: &IContextMenu, hwnd: HWND, kind: MenuKind, id: u32, dir: &str) -> Option<String> {
        use windows::Win32::UI::WindowsAndMessaging::{GetMenuState, MF_BYCOMMAND};
        let hmenu = unsafe { CreatePopupMenu() }.unwrap();
        let _g = MenuGuard(hmenu);
        populate_menu(cm, hmenu, kind).unwrap();
        let state = unsafe { GetMenuState(hmenu, id, MF_BYCOMMAND) };
        assert!(state != u32::MAX, "item {id} missing");
        assert!(state & MF_GRAYED.0 == 0, "item {id} disabled");
        run_menu_command(hwnd, cm, id, POINT::default(), Some(dir), kind).unwrap()
    }

    fn find_id_by_verb(cm: &IContextMenu, hmenu: HMENU, verb: &str) -> Option<u32> {
        let count = unsafe { GetMenuItemCount(Some(hmenu)) };
        (0..count)
            .map(|i| unsafe { GetMenuItemID(hmenu, i) })
            .find(|&id| {
                id != u32::MAX
                    && (ID_CMD_FIRST..=ID_CMD_LAST).contains(&id)
                    && command_verb(cm, (id - ID_CMD_FIRST) as usize) == verb
            })
    }

    /// Selection handling on the item menu: "rename" is handed back without
    /// invoking; a normal command ("copyaspath") is invoked by offset;
    /// dismissal → None.
    #[test]
    #[ignore]
    fn item_menu_commands() {
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let saved_text = {
            let _c = ClipboardGuard::open().unwrap();
            get_clipboard_bytes(CF_UNICODETEXT)
        };
        let hwnd = hidden_window();
        let file = std::env::temp_dir().join("ferret-cmd-test.txt");
        std::fs::write(&file, "x").unwrap();
        let file = paths::normalize(&file.display().to_string());
        let (cm, _p, dir) = item_context_menu(hwnd, std::slice::from_ref(&file)).unwrap();
        let hmenu = unsafe { CreatePopupMenu() }.unwrap();
        let _g = MenuGuard(hmenu);
        populate_menu(&cm, hmenu, MenuKind::Items).unwrap();

        let rename = find_id_by_verb(&cm, hmenu, "rename").expect("rename item");
        assert_eq!(
            run_menu_command(
                hwnd,
                &cm,
                rename,
                POINT::default(),
                dir.as_deref(),
                MenuKind::Items
            )
            .unwrap()
            .as_deref(),
            Some("rename")
        );
        assert!(std::path::Path::new(&file).exists());
        assert_eq!(
            run_menu_command(
                hwnd,
                &cm,
                0,
                POINT::default(),
                dir.as_deref(),
                MenuKind::Items
            )
            .unwrap(),
            None
        );

        let cap = find_id_by_verb(&cm, hmenu, "copyaspath").expect("copy as path item");
        assert_eq!(
            run_menu_command(
                hwnd,
                &cm,
                cap,
                POINT::default(),
                dir.as_deref(),
                MenuKind::Items
            )
            .unwrap()
            .as_deref(),
            Some("copyaspath")
        );
        let ok = pump_until(3000, || {
            let Ok(_c) = ClipboardGuard::open() else {
                return false;
            };
            get_clipboard_bytes(CF_UNICODETEXT).is_some_and(|t| {
                let u: Vec<u16> = t
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&c| c != 0)
                    .collect();
                let s = String::from_utf16_lossy(&u);
                println!("clipboard text: {s}");
                s.starts_with('"') && s.ends_with("ferret-cmd-test.txt\"")
            })
        });
        assert!(ok, "copy as path did not put the path on the clipboard");
        println!("rename intercepted, dismiss → None, copyaspath invoked ok");

        {
            let _c = ClipboardGuard::open().unwrap();
            unsafe { EmptyClipboard() }.unwrap();
            if let Some(t) = saved_text {
                set_clipboard_bytes(CF_UNICODETEXT, &t).unwrap();
            }
        }
        unsafe { DestroyWindow(hwnd) }.unwrap();
        let _ = std::fs::remove_file(&file);
    }

    /// Our clipboard → the shell's own "paste" verb (what Ctrl+V in Explorer
    /// runs) copies, and after a cut, moves.
    #[test]
    #[ignore]
    fn explorer_paste() {
        unsafe { windows::Win32::System::Ole::OleInitialize(None) }.unwrap();
        let saved_text = {
            let _c = ClipboardGuard::open().unwrap();
            get_clipboard_bytes(CF_UNICODETEXT)
        };
        let root = std::env::temp_dir().join(format!("ferret-paste-{}", std::process::id()));
        let (src, dst_copy, dst_move) = (root.join("src"), root.join("copy"), root.join("move"));
        for d in [&src, &dst_copy, &dst_move] {
            std::fs::create_dir_all(d).unwrap();
        }
        let files: Vec<String> = ["a.txt", "b ü.txt"]
            .iter()
            .map(|n| {
                let p = src.join(n);
                std::fs::write(&p, n).unwrap();
                p.display().to_string()
            })
            .collect();
        let hwnd = hidden_window();

        // Copy.
        clipboard_write_blocking(&files, false).unwrap();
        let dst = paths::normalize(&dst_copy.display().to_string());
        let bg = background_context_menu(hwnd, &dst).unwrap();
        assert_eq!(
            choose(&bg, hwnd, MenuKind::Background, ID_PASTE, &dst).as_deref(),
            Some("paste")
        );
        let ok = pump_until(15000, || {
            dst_copy.join("a.txt").exists() && dst_copy.join("b ü.txt").exists()
        });
        assert!(ok, "copy-paste did not produce files");
        assert!(src.join("a.txt").exists(), "copy must keep the source");
        println!("paste (copy) ok");

        // Paste shortcut.
        let dst_link = root.join("link");
        std::fs::create_dir_all(&dst_link).unwrap();
        let dst = paths::normalize(&dst_link.display().to_string());
        let bg = background_context_menu(hwnd, &dst).unwrap();
        assert_eq!(
            choose(&bg, hwnd, MenuKind::Background, ID_PASTE_LINK, &dst).as_deref(),
            Some("pastelink")
        );
        let ok = pump_until(15000, || {
            std::fs::read_dir(&dst_link)
                .map(|d| {
                    d.filter_map(Result::ok)
                        .filter(|e| e.path().extension().is_some_and(|x| x == "lnk"))
                        .count()
                        == 2
                })
                .unwrap_or(false)
        });
        if !ok {
            for e in std::fs::read_dir(&dst_link).unwrap().flatten() {
                println!("  link dir has: {:?}", e.file_name());
            }
        }
        assert!(ok, "paste shortcut did not create 2 .lnk files");
        println!("paste shortcut ok");

        // Cut → move.
        clipboard_write_blocking(&files, true).unwrap();
        let dst = paths::normalize(&dst_move.display().to_string());
        let bg = background_context_menu(hwnd, &dst).unwrap();
        assert_eq!(
            choose(&bg, hwnd, MenuKind::Background, ID_PASTE, &dst).as_deref(),
            Some("paste")
        );
        let ok = pump_until(15000, || {
            dst_move.join("a.txt").exists()
                && dst_move.join("b ü.txt").exists()
                && !src.join("a.txt").exists()
        });
        assert!(ok, "cut-paste did not move files");
        println!("paste (move) ok");

        unsafe { DestroyWindow(hwnd) }.unwrap();
        {
            let _c = ClipboardGuard::open().unwrap();
            unsafe { EmptyClipboard() }.unwrap();
            if let Some(t) = saved_text {
                set_clipboard_bytes(CF_UNICODETEXT, &t).unwrap();
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        unsafe { windows::Win32::System::Ole::OleUninitialize() };
    }

    fn dump_menu(
        hmenu: HMENU,
        cm: &IContextMenu,
        depth: usize,
        hwnd: HWND,
        hook_window: bool,
    ) -> usize {
        let count = unsafe { GetMenuItemCount(Some(hmenu)) };
        let mut total = 0;
        for i in 0..count {
            let mut buf = [0u16; 256];
            let n = unsafe { GetMenuStringW(hmenu, i as u32, Some(&mut buf), MF_BYPOSITION) };
            let text = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            let id = unsafe { GetMenuItemID(hmenu, i) };
            let sub = unsafe { GetSubMenu(hmenu, i) };
            let verb = if id != u32::MAX && id >= ID_CMD_FIRST && id <= ID_CMD_LAST {
                command_verb(cm, (id - ID_CMD_FIRST) as usize)
            } else {
                String::new()
            };
            println!(
                "{}{:?} id={} verb={:?}{}",
                "  ".repeat(depth + 1),
                text,
                id as i32,
                verb,
                if sub.is_invalid() { "" } else { " ▸" }
            );
            total += 1;
            if !sub.is_invalid() && depth < 1 && hook_window {
                // Same path as during TrackPopupMenuEx: WM_INITMENUPOPUP goes
                // to the owner window → our subclass → HandleMenuMsg2.
                unsafe {
                    SendMessageW(
                        hwnd,
                        WM_INITMENUPOPUP,
                        Some(WPARAM(sub.0 as usize)),
                        Some(LPARAM(i as isize)),
                    )
                };
                total += dump_menu(sub, cm, depth + 1, hwnd, hook_window);
            }
        }
        total
    }

    #[test]
    #[ignore]
    fn menus_populate() {
        let _com = ComGuard::init();
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &HSTRING::from("STATIC"),
                &HSTRING::from("ferret-test"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                None,
                None,
            )
        }
        .unwrap();
        let file = std::env::temp_dir().join("ferret-menu-test.txt");
        std::fs::write(&file, "x").unwrap();
        let file = file.display().to_string();

        // Item menu.
        let (cm, _pidls, dir) = item_context_menu(hwnd, std::slice::from_ref(&file)).unwrap();
        assert_eq!(
            dir.as_deref(),
            paths::parent(&paths::normalize(&file)).as_deref()
        );
        let hmenu = unsafe { CreatePopupMenu() }.unwrap();
        let _g = MenuGuard(hmenu);
        populate_menu(&cm, hmenu, MenuKind::Items).unwrap();
        let hook = MenuHook::new(&cm);
        assert!(hook.cm3.is_some() || hook.cm2.is_some());
        let sub = Subclass::install(hwnd, &hook);
        assert!(sub.is_some(), "subclass");
        println!("ITEM MENU for {file}:");
        let n = dump_menu(hmenu, &cm, 0, hwnd, true);
        drop(sub);
        assert!(n > 5);

        // Background menu.
        let folder = paths::normalize(&std::env::temp_dir().display().to_string());
        let bg = background_context_menu(hwnd, &folder).unwrap();
        let hmenu2 = unsafe { CreatePopupMenu() }.unwrap();
        let _g2 = MenuGuard(hmenu2);
        populate_menu(&bg, hmenu2, MenuKind::Background).unwrap();
        let hook2 = MenuHook::new(&bg);
        let sub2 = Subclass::install(hwnd, &hook2);
        println!("BACKGROUND MENU for {folder}:");
        let n2 = dump_menu(hmenu2, &bg, 0, hwnd, true);
        drop(sub2);
        assert!(n2 > 1);

        // Drive root + multi-select.
        let (cm3, _p3, dir3) = item_context_menu(hwnd, &[r"C:\".to_string()]).unwrap();
        assert_eq!(dir3, None);
        let hmenu3 = unsafe { CreatePopupMenu() }.unwrap();
        let _g3 = MenuGuard(hmenu3);
        populate_menu(&cm3, hmenu3, MenuKind::Items).unwrap();
        println!("DRIVE MENU items: {}", unsafe {
            GetMenuItemCount(Some(hmenu3))
        });
        let windir = std::env::var("WINDIR").unwrap();
        assert!(item_context_menu(
            hwnd,
            &[
                format!(r"{windir}\explorer.exe"),
                format!(r"{windir}\notepad.exe")
            ]
        )
        .is_ok());
        assert!(
            item_context_menu(hwnd, &[format!(r"{windir}\explorer.exe"), file.clone()]).is_err()
        );

        unsafe { DestroyWindow(hwnd) }.unwrap();
        let _ = std::fs::remove_file(&file);
    }
}
