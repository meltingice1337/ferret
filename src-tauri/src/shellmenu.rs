//! Explorer's third-party context-menu entries, for Ferret's own menu.
//!
//! Two sources, merged into one list (modern first, de-duplicated by label):
//! * **classic** — the shell's `IContextMenu` (registry verbs and shell
//!   extensions: 7-Zip, Git, Defender, Send to, …), built exactly like the
//!   native menu in `shell.rs` and read back item by item from a hidden
//!   `HMENU`. Entries Ferret has its own version of (open, cut, copy,
//!   rename, …) are dropped.
//! * **modern** — Windows 11 `IExplorerCommand` entries that packaged and
//!   sparse-package apps declare in their `AppxManifest.xml`
//!   (`windows.fileExplorerContextMenus`): Notepad, Terminal, VS Code, Zed,
//!   PowerToys, … The manifest scan is cached; see [`warm_up`].
//!
//! Threading and lifetime:
//! * Classic menus are built and invoked on the main thread, like the native
//!   menu. Modern commands live on a dedicated STA thread with a message pump
//!   ([`menu_thread`]): cold activation of a packaged COM server starts a
//!   `dllhost.exe` and must not stall the UI. Both halves run in parallel.
//! * The objects behind the last [`KEEP_MENUS`] menus stay alive on their
//!   thread, keyed by the token handed to the UI, so `shell_menu_invoke`
//!   runs an entry on the same instance that produced it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Serialize;
use windows::core::{Interface, GUID, HRESULT, HSTRING, PWSTR};
use windows::Management::Deployment::{PackageManager, PackageTypes};
use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, IExplorerCommand, IShellItemArray,
    SHCreateShellItemArrayFromIDLists, ECF_HASSUBCOMMANDS, ECF_ISSEPARATOR, ECF_SEPARATORAFTER,
    ECF_SEPARATORBEFORE, ECS_DISABLED, ECS_HIDDEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DispatchMessageW, GetCursorPos, GetMenuItemCount, GetMenuItemInfoW,
    MsgWaitForMultipleObjectsEx, PeekMessageW, PostThreadMessageW, TranslateMessage, HMENU,
    MENUITEMINFOW, MFS_DISABLED, MFT_SEPARATOR, MIIM_BITMAP, MIIM_CHECKMARKS, MIIM_FTYPE, MIIM_ID,
    MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MSG, MWMO_INPUTAVAILABLE, PM_NOREMOVE, PM_REMOVE,
    QS_ALLINPUT, WM_INITMENUPOPUP, WM_NULL,
};

use crate::paths;
use crate::shell::{
    background_context_menu, bgra_to_rgba, bitmap_bgra, command_verb, encode_png, hr_msg,
    hwnd_from_raw, icon_png_from_location, invoke_command, item_context_menu, on_main_thread,
    populate_menu, raw_hwnd, take_pwstr, ComGuard, MenuGuard, MenuKind, Pidl, Verb, ID_CMD_FIRST,
    ID_CMD_LAST,
};

/// Menus whose COM objects are kept alive for `shell_menu_invoke`.
const KEEP_MENUS: usize = 4;
/// How old the package manifest scan may get before a right-click triggers a
/// background rescan.
const CATALOG_TTL: Duration = Duration::from_secs(5 * 60);
/// Longest a right-click waits for the modern half; a hung packaged COM
/// server must not hold back the classic entries.
const MODERN_TIMEOUT: Duration = Duration::from_millis(2500);
/// Longest `shell_menu_invoke` waits for a packaged command's `Invoke`.
const INVOKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Modern icons are requested at 2x the 16 px menu size (HiDPI).
const MODERN_ICON_PX: u32 = 32;
/// Submenu nesting limit (both sources).
const MAX_DEPTH: usize = 4;
const E_PENDING: HRESULT = HRESULT(0x8000_000A_u32 as i32);

// ---------------------------------------------------------------------------
// Public types and commands
// ---------------------------------------------------------------------------

#[derive(Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShellMenu {
    /// Pass back to `shell_menu_invoke`. `0` = nothing to invoke.
    pub token: u64,
    pub items: Vec<ShellMenuItem>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShellMenuItem {
    /// Opaque (`c:<n>` / `m:<n>`); empty for separators and submenu parents.
    pub id: String,
    pub label: String,
    /// `data:image/png;base64,…`
    pub icon: Option<String>,
    pub disabled: bool,
    pub separator: bool,
    pub children: Vec<ShellMenuItem>,
    /// `"classic"` or `"modern"`.
    pub source: &'static str,
}

impl ShellMenuItem {
    fn separator(source: &'static str) -> Self {
        ShellMenuItem {
            id: String::new(),
            label: String::new(),
            icon: None,
            disabled: false,
            separator: true,
            children: Vec::new(),
            source,
        }
    }
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Explorer's extra context-menu entries for `paths` (selected items, all in
/// one folder for the classic part), or for the background of folder
/// `paths[0]` when `background`. Entries Ferret already offers are left out.
#[tauri::command]
pub async fn shell_menu_items(
    window: tauri::WebviewWindow,
    paths: Vec<String>,
    background: bool,
) -> Result<ShellMenu, String> {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| paths::normalize(p))
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        return Ok(ShellMenu::default());
    }
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let raw = raw_hwnd(&window)?;
    // Skipped while a packaged server hangs the menu thread; the classic
    // menu still lists most packaged entries then (as `{CLSID}` verbs).
    let modern = (!menu_thread_stuck()).then(|| {
        let paths = paths.clone();
        on_menu_thread(move || modern_menu(token, &paths, background))
    });
    let classic = on_main_thread(&window, move || {
        let _com = ComGuard::init();
        classic_menu(token, hwnd_from_raw(raw), &paths, background)
    })
    .await;
    let modern = match modern {
        Some(rx) => tauri::async_runtime::spawn_blocking(move || recv_modern(&rx))
            .await
            .map_err(|e| e.to_string())?,
        None => Err("the shell menu thread is busy".into()),
    };
    let items = match (classic, modern) {
        (Err(e), Err(_)) => return Err(e),
        (classic, modern) => merge(modern.unwrap_or_default(), classic.unwrap_or_default()),
    };
    Ok(ShellMenu { token, items })
}

/// Runs entry `id` of the menu `token` from [`shell_menu_items`]. Returns the
/// canonical verb when the UI should act itself (e.g. `"rename"`), else
/// `null` once the shell has run the command.
#[tauri::command]
pub async fn shell_menu_invoke(
    window: tauri::WebviewWindow,
    token: u64,
    id: String,
) -> Result<Option<String>, String> {
    log::info!("shell menu: invoking {id}");
    match parse_id(&id).ok_or_else(|| format!("Unknown menu entry: {id}"))? {
        (Source::Classic, offset) => {
            let raw = raw_hwnd(&window)?;
            on_main_thread(&window, move || {
                let _com = ComGuard::init();
                classic_invoke(token, offset, hwnd_from_raw(raw))
            })
            .await
        }
        (Source::Modern, index) => {
            let rx = on_menu_thread(move || modern_invoke(token, index).map(|()| None));
            tauri::async_runtime::spawn_blocking(move || {
                rx.recv_timeout(INVOKE_TIMEOUT)
                    .map_err(|_| "The app did not respond".to_string())?
            })
            .await
            .map_err(|e| e.to_string())?
        }
    }
}

/// Starts the package manifest scan and the modern-menu thread, and loads
/// the shell extensions classic menus use, all in the background. Call once
/// at startup; the scan then refreshes on its own.
pub fn warm_up() {
    refresh_catalog();
    let _ = menu_thread();
    let _ = prewarm_classic();
}

/// Builds (and drops) a few classic menus on a throwaway STA thread, so the
/// first right-click doesn't pay on the UI thread for loading shell
/// extensions and the "New" menu data (~0.2–0.5 s cold, measured).
fn prewarm_classic() -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ferret-menu-prewarm".into())
        .spawn(|| {
            let _com = ComGuard::init();
            let mut targets: Vec<(String, bool)> = Vec::new();
            if let Some(home) = dirs::home_dir() {
                let home = paths::normalize(&home.display().to_string());
                targets.push((home.clone(), true));
                targets.push((home, false));
            }
            if let Ok(exe) = std::env::current_exe() {
                targets.push((paths::normalize(&exe.display().to_string()), false));
            }
            for (path, background) in targets {
                // Dropped right here, before COM is uninitialised.
                let _ = build_classic(HWND::default(), std::slice::from_ref(&path), background);
            }
        })
        .ok()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Classic,
    Modern,
}

fn parse_id(id: &str) -> Option<(Source, usize)> {
    let (source, n) = id.split_once(':')?;
    let source = match source {
        "c" => Source::Classic,
        "m" => Source::Modern,
        _ => return None,
    };
    Some((source, n.parse().ok()?))
}

// ---------------------------------------------------------------------------
// Labels, filtering, merging
// ---------------------------------------------------------------------------

/// Menu text → label: drops the `\t` shortcut column, `(&X)` accelerator
/// suffixes (CJK menus) and `&` markers (`&&` → `&`).
fn clean_label(raw: &str) -> String {
    let text = raw.split('\t').next().unwrap_or("");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' if chars.get(i + 1) == Some(&'&') && chars.get(i + 3) == Some(&')') => i += 4,
            '&' if chars.get(i + 1) == Some(&'&') => {
                out.push('&');
                i += 2;
            }
            '&' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim().to_string()
}

/// Comparison key: case-insensitive, without a trailing ellipsis.
fn label_key(label: &str) -> String {
    label
        .trim()
        .trim_end_matches('…')
        .trim_end_matches("...")
        .trim()
        .to_lowercase()
}

/// Canonical verbs of entries Ferret has its own version of. The UI handles
/// these itself if one is ever invoked.
const BUILTIN_VERBS: &[&str] = &[
    "open",
    "opennewwindow",
    "opennewprocess",
    "opennewtab",
    "explore",
    "openas",
    "cut",
    "copy",
    "paste",
    "pastelink",
    "delete",
    "rename",
    "properties",
    "link",
    "copyaspath",
    "pintohome",
    "pintohomefile",
    "unpinfromhome",
    "pintostartscreen",
    "unpinfromstartscreen",
    "previousversions",
    "windows.modernshare",
    "share",
];

/// Labels of entries Ferret has (for items without a canonical verb).
const BUILTIN_LABELS: &[&str] = &[
    "open",
    "open with",
    "open in new window",
    "open in new tab",
    "open in new process",
    "explore",
    "cut",
    "copy",
    "paste",
    "paste shortcut",
    "delete",
    "rename",
    "properties",
    "create shortcut",
    "copy as path",
    "extract all",
    "pin to quick access",
    "unpin from quick access",
    "pin to start",
    "unpin from start",
    "pin to home",
    "unpin from home",
    "add to favorites",
    "remove from favorites",
    "include in library",
    "restore previous versions",
    "give access to",
    "share",
    "share with",
];

fn is_builtin(verb: &str, label: &str) -> bool {
    let verb = verb.trim();
    if !verb.is_empty() && BUILTIN_VERBS.iter().any(|v| v.eq_ignore_ascii_case(verb)) {
        return true;
    }
    let key = label_key(label);
    BUILTIN_LABELS.contains(&key.as_str())
}

/// Drops leading, trailing and repeated separators.
fn collapse_separators(items: Vec<ShellMenuItem>) -> Vec<ShellMenuItem> {
    let mut out: Vec<ShellMenuItem> = Vec::with_capacity(items.len());
    for item in items {
        if item.separator && out.last().is_none_or(|l| l.separator) {
            continue;
        }
        out.push(item);
    }
    if out.last().is_some_and(|l| l.separator) {
        out.pop();
    }
    out
}

/// Modern entries, a separator, then the classic ones whose label no modern
/// entry already has.
fn merge(modern: Vec<ShellMenuItem>, classic: Vec<ShellMenuItem>) -> Vec<ShellMenuItem> {
    let taken: HashSet<String> = modern
        .iter()
        .filter(|i| !i.separator)
        .map(|i| label_key(&i.label))
        .collect();
    let mut items = modern;
    items.push(ShellMenuItem::separator("classic"));
    items.extend(
        classic
            .into_iter()
            .filter(|i| i.separator || !taken.contains(&label_key(&i.label))),
    );
    collapse_separators(items)
}

fn data_url(png: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

// ---------------------------------------------------------------------------
// Classic entries (IContextMenu) — main thread
// ---------------------------------------------------------------------------

/// A classic menu kept alive for invocation. Fields drop in order: the menu
/// goes before the handler that filled it.
struct ClassicMenu {
    token: u64,
    _hmenu: MenuGuard,
    cm: IContextMenu,
    _pidls: Vec<Pidl>,
    dir: Option<String>,
}

thread_local! {
    static CLASSIC: RefCell<VecDeque<ClassicMenu>> = const { RefCell::new(VecDeque::new()) };
    static MODERN: RefCell<VecDeque<ModernMenu>> = const { RefCell::new(VecDeque::new()) };
}

fn keep<T>(cache: &RefCell<VecDeque<T>>, entry: T) {
    let evicted: Vec<T> = {
        let mut c = cache.borrow_mut();
        c.push_back(entry);
        let excess = c.len().saturating_sub(KEEP_MENUS);
        c.drain(..excess).collect()
    };
    // Released outside the borrow: a handler's Release may pump messages.
    drop(evicted);
}

/// Builds the shell menu for `paths_in` (or the background of `paths_in[0]`),
/// reads its entries and caches it under `token`. Main (STA) thread.
fn classic_menu(
    token: u64,
    hwnd: HWND,
    paths_in: &[String],
    background: bool,
) -> Result<Vec<ShellMenuItem>, String> {
    let (mut menu, items) = build_classic(hwnd, paths_in, background)?;
    menu.token = token;
    CLASSIC.with(|c| keep(c, menu));
    Ok(items)
}

/// The shell menu and its filtered entries (any STA thread).
fn build_classic(
    hwnd: HWND,
    paths_in: &[String],
    background: bool,
) -> Result<(ClassicMenu, Vec<ShellMenuItem>), String> {
    let folder = paths_in.first().ok_or("no items")?;
    let (cm, pidls, dir, kind) = if background {
        match background_context_menu(hwnd, folder) {
            Ok(cm) => (cm, Vec::new(), Some(folder.clone()), MenuKind::Background),
            Err(_) => {
                let (cm, p, d) = item_context_menu(hwnd, std::slice::from_ref(folder))?;
                (cm, p, d, MenuKind::FolderItem)
            }
        }
    } else {
        let (cm, p, d) = item_context_menu(hwnd, paths_in)?;
        (cm, p, d, MenuKind::Items)
    };
    let hmenu = MenuGuard(unsafe { CreatePopupMenu() }.map_err(hr_msg)?);
    populate_menu(&cm, hmenu.0, kind)?;
    let walker = ClassicWalker {
        cm: &cm,
        cm2: cm.cast().ok(),
        cm3: cm.cast().ok(),
    };
    let items = collapse_separators(walker.walk(hmenu.0, 0));
    let menu = ClassicMenu {
        token: 0,
        _hmenu: hmenu,
        cm,
        _pidls: pidls,
        dir,
    };
    Ok((menu, items))
}

/// Runs command `offset` of cached menu `token`. Main (STA) thread.
fn classic_invoke(token: u64, offset: usize, hwnd: HWND) -> Result<Option<String>, String> {
    // Clone out of the cache: InvokeCommand may pump messages, and another
    // menu request could then run on this thread and touch the cache.
    let (cm, dir) = CLASSIC
        .with(|c| {
            c.borrow()
                .iter()
                .find(|m| m.token == token)
                .map(|m| (m.cm.clone(), m.dir.clone()))
        })
        .ok_or("This menu has expired")?;
    let verb = command_verb(&cm, offset);
    if !verb.is_empty() && BUILTIN_VERBS.iter().any(|v| v.eq_ignore_ascii_case(&verb)) {
        return Ok(Some(verb.to_ascii_lowercase()));
    }
    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    invoke_command(&cm, hwnd, Verb::Offset(offset), pt, dir.as_deref())?;
    Ok(None)
}

/// One menu item as `GetMenuItemInfoW` reports it.
struct RawItem {
    text: String,
    id: u32,
    submenu: Option<HMENU>,
    separator: bool,
    disabled: bool,
    bitmap: HBITMAP,
    unchecked: HBITMAP,
}

fn menu_item(hmenu: HMENU, pos: u32) -> Option<RawItem> {
    let mut mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE
            | MIIM_STATE
            | MIIM_ID
            | MIIM_SUBMENU
            | MIIM_STRING
            | MIIM_BITMAP
            | MIIM_CHECKMARKS,
        ..Default::default()
    };
    unsafe { GetMenuItemInfoW(hmenu, pos, true, &mut mii) }.ok()?;
    let separator = mii.fType.0 & MFT_SEPARATOR.0 != 0;
    let mut text = String::new();
    if !separator && mii.cch > 0 {
        let mut buf = vec![0u16; mii.cch as usize + 1];
        let mut s = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_STRING,
            dwTypeData: PWSTR(buf.as_mut_ptr()),
            cch: buf.len() as u32,
            ..Default::default()
        };
        if unsafe { GetMenuItemInfoW(hmenu, pos, true, &mut s) }.is_ok() {
            let n = (s.cch as usize).min(buf.len());
            text = String::from_utf16_lossy(&buf[..n]);
        }
    }
    Some(RawItem {
        text,
        id: mii.wID,
        submenu: (!mii.hSubMenu.is_invalid()).then_some(mii.hSubMenu),
        separator,
        disabled: mii.fState.0 & MFS_DISABLED.0 != 0,
        bitmap: mii.hbmpItem,
        unchecked: mii.hbmpUnchecked,
    })
}

/// Menu item bitmap → PNG data URL. Skips `HBMMENU_CALLBACK` (owner-drawn)
/// and the `HBMMENU_*` system bitmaps.
fn bitmap_icon(hbm: HBITMAP) -> Option<String> {
    if (-1..=11).contains(&(hbm.0 as isize)) {
        return None;
    }
    let (w, h, bgra) = bitmap_bgra(hbm).ok()?;
    if w > 256 || h > 256 {
        return None;
    }
    // Shell menu bitmaps are premultiplied 32-bpp DIBs; bgra_to_rgba detects
    // the (older, straight-alpha or opaque) exceptions.
    let png = encode_png(w, h, &bgra_to_rgba(&bgra, true)).ok()?;
    Some(data_url(&png))
}

struct ClassicWalker<'a> {
    cm: &'a IContextMenu,
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
}

impl ClassicWalker<'_> {
    fn walk(&self, hmenu: HMENU, depth: usize) -> Vec<ShellMenuItem> {
        let count = unsafe { GetMenuItemCount(Some(hmenu)) }.max(0) as u32;
        let mut out = Vec::new();
        for pos in 0..count {
            let Some(raw) = menu_item(hmenu, pos) else {
                continue;
            };
            if raw.separator {
                out.push(ShellMenuItem::separator("classic"));
                continue;
            }
            let label = clean_label(&raw.text);
            if label.is_empty() {
                continue; // owner-drawn or bitmap-only
            }
            let in_range = (ID_CMD_FIRST..=ID_CMD_LAST).contains(&raw.id);
            let verb = match raw.submenu {
                Some(_) => String::new(),
                None if in_range => command_verb(self.cm, (raw.id - ID_CMD_FIRST) as usize),
                None => continue, // not a shell command (shell.rs's own Paste items)
            };
            if is_builtin(&verb, &label) {
                continue;
            }
            let children = match raw.submenu {
                Some(sub) if depth < MAX_DEPTH => {
                    self.init_popup(sub, pos);
                    let c = collapse_separators(self.walk(sub, depth + 1));
                    if c.is_empty() {
                        continue;
                    }
                    c
                }
                Some(_) => continue,
                None => Vec::new(),
            };
            let icon = bitmap_icon(raw.bitmap).or_else(|| bitmap_icon(raw.unchecked));
            out.push(ShellMenuItem {
                id: if children.is_empty() {
                    format!("c:{}", raw.id - ID_CMD_FIRST)
                } else {
                    String::new()
                },
                label,
                icon,
                disabled: raw.disabled,
                separator: false,
                children,
                source: "classic",
            });
        }
        out
    }

    /// What the menu owner forwards when a submenu opens: lazily filled
    /// submenus (Send to, some extensions) add their items now.
    fn init_popup(&self, sub: HMENU, pos: u32) {
        let (w, l) = (WPARAM(sub.0 as usize), LPARAM(pos as isize));
        unsafe {
            if let Some(cm3) = &self.cm3 {
                let mut r = LRESULT(0);
                let _ = cm3.HandleMenuMsg2(WM_INITMENUPOPUP, w, l, Some(&mut r));
            } else if let Some(cm2) = &self.cm2 {
                let _ = cm2.HandleMenuMsg(WM_INITMENUPOPUP, w, l);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Modern entries (IExplorerCommand) — dedicated STA thread
// ---------------------------------------------------------------------------

/// A modern menu kept alive for invocation: the item array the commands were
/// queried with, and every invokable command (index = `m:<n>`).
struct ModernMenu {
    token: u64,
    array: IShellItemArray,
    cmds: Vec<IExplorerCommand>,
}

/// What a context-menu verb in a manifest applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ItemKind {
    /// A file, with its lower-case `.ext`.
    File(Option<String>),
    Directory,
    Drive,
    Background,
}

impl ItemKind {
    fn of(path: &str) -> ItemKind {
        let p = paths::normalize(path);
        if p.len() == 3 && p.ends_with(":\\") {
            return ItemKind::Drive;
        }
        if std::fs::metadata(paths::for_io(&p)).is_ok_and(|m| m.is_dir()) {
            return ItemKind::Directory;
        }
        let ext = std::path::Path::new(&p)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| !e.is_empty())
            .map(|e| format!(".{}", e.to_lowercase()));
        ItemKind::File(ext)
    }
}

/// One `<Verb>` of a package's `windows.fileExplorerContextMenus`, with all
/// the `ItemType`s it is declared under (lower-case).
#[derive(Clone, Debug)]
struct PackageVerb {
    package: String,
    verb_id: String,
    clsid: GUID,
    types: Vec<String>,
}

impl PackageVerb {
    fn applies_to(&self, kind: &ItemKind) -> bool {
        self.types.iter().any(|t| {
            let t = t.as_str();
            match kind {
                ItemKind::File(ext) => {
                    t == "*" || t == "allfilesystemobjects" || ext.as_deref() == Some(t)
                }
                ItemKind::Directory => {
                    t == "directory" || t == "folder" || t == "allfilesystemobjects"
                }
                ItemKind::Drive => t == "drive" || t == "folder",
                ItemKind::Background => t == "directory\\background",
            }
        })
    }
}

/// Queries the packaged commands that apply to the selection and caches them
/// under `token`. Runs on the menu thread.
fn modern_menu(
    token: u64,
    paths_in: &[String],
    background: bool,
) -> Result<Vec<ShellMenuItem>, String> {
    let Some(catalog) = catalog() else {
        return Ok(Vec::new()); // first scan still running
    };
    let kinds: Vec<ItemKind> = if background {
        vec![ItemKind::Background]
    } else {
        paths_in.iter().map(|p| ItemKind::of(p)).collect()
    };
    let failed = failed_clsids();
    let verbs: Vec<&PackageVerb> = catalog
        .verbs
        .iter()
        .filter(|v| kinds.iter().all(|k| v.applies_to(k)))
        .filter(|v| !failed.contains(&v.clsid.to_u128()))
        .collect();
    if verbs.is_empty() {
        return Ok(Vec::new());
    }
    // Explorer passes the folder itself for Directory\Background verbs.
    let targets = if background { &paths_in[..1] } else { paths_in };
    let array = shell_item_array(targets)?;
    let mut cmds = Vec::new();
    let mut items = Vec::new();
    for v in verbs {
        match unsafe {
            CoCreateInstance::<_, IExplorerCommand>(
                &v.clsid,
                None,
                CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER,
            )
        } {
            Ok(cmd) => describe(&cmd, &array, &mut cmds, &mut items, 0),
            Err(e) => note_failure(v, &e),
        }
    }
    MODERN.with(|m| keep(m, ModernMenu { token, array, cmds }));
    Ok(collapse_separators(items))
}

fn shell_item_array(paths_in: &[String]) -> Result<IShellItemArray, String> {
    let pidls = paths_in
        .iter()
        .map(|p| Pidl::parse(p))
        .collect::<Result<Vec<_>, _>>()?;
    let ptrs: Vec<*const ITEMIDLIST> = pidls.iter().map(Pidl::ptr).collect();
    // The array keeps its own copies of the IDLists.
    unsafe { SHCreateShellItemArrayFromIDLists(&ptrs) }.map_err(hr_msg)
}

/// Appends `cmd` (and its subcommands) to `out` unless it is hidden;
/// invokable commands are collected in `cmds`.
fn describe(
    cmd: &IExplorerCommand,
    array: &IShellItemArray,
    cmds: &mut Vec<IExplorerCommand>,
    out: &mut Vec<ShellMenuItem>,
    depth: usize,
) {
    let flags = unsafe { cmd.GetFlags() }.unwrap_or(0);
    if flags & ECF_ISSEPARATOR.0 as u32 != 0 {
        out.push(ShellMenuItem::separator("modern"));
        return;
    }
    let state = match unsafe { cmd.GetState(array, false) } {
        Err(e) if e.code() == E_PENDING => unsafe { cmd.GetState(array, true) },
        r => r,
    };
    let state = match state {
        Ok(s) => s,
        Err(e) if e.code() == E_NOTIMPL => 0,
        Err(_) => return,
    };
    if state & ECS_HIDDEN.0 as u32 != 0 {
        return;
    }
    let label = unsafe { cmd.GetTitle(array) }
        .map(|t| clean_label(&take_pwstr(t)))
        .unwrap_or_default();
    if label.is_empty() {
        return;
    }
    let mut children = Vec::new();
    if flags & ECF_HASSUBCOMMANDS.0 as u32 != 0 {
        if depth >= MAX_DEPTH {
            return;
        }
        if let Ok(subs) = unsafe { cmd.EnumSubCommands() } {
            loop {
                let mut slot: [Option<IExplorerCommand>; 1] = [None];
                let mut fetched = 0u32;
                if unsafe { subs.Next(&mut slot, Some(&mut fetched)) }.is_err() || fetched == 0 {
                    break;
                }
                if let Some(sub) = slot[0].take() {
                    describe(&sub, array, cmds, &mut children, depth + 1);
                }
                if children.len() > 64 {
                    break;
                }
            }
        }
        children = collapse_separators(children);
        if children.is_empty() {
            return;
        }
    }
    let icon = unsafe { cmd.GetIcon(array) }
        .ok()
        .map(take_pwstr)
        .and_then(|loc| resource_icon(&loc));
    let id = if children.is_empty() {
        cmds.push(cmd.clone());
        format!("m:{}", cmds.len() - 1)
    } else {
        String::new()
    };
    if flags & ECF_SEPARATORBEFORE.0 as u32 != 0 {
        out.push(ShellMenuItem::separator("modern"));
    }
    out.push(ShellMenuItem {
        id,
        label,
        icon,
        disabled: state & ECS_DISABLED.0 as u32 != 0,
        separator: false,
        children,
        source: "modern",
    });
    if flags & ECF_SEPARATORAFTER.0 as u32 != 0 {
        out.push(ShellMenuItem::separator("modern"));
    }
}

/// Runs modern command `index` of cached menu `token`. Menu thread.
fn modern_invoke(token: u64, index: usize) -> Result<(), String> {
    let (cmd, array) = MODERN
        .with(|m| {
            m.borrow()
                .iter()
                .find(|e| e.token == token)
                .and_then(|e| Some((e.cmds.get(index)?.clone(), e.array.clone())))
        })
        .ok_or("This menu has expired")?;
    unsafe { cmd.Invoke(&array, None) }.map_err(hr_msg)
}

/// `GetIcon` result (`path,index`, `path,-resid` or an `@{…}` resource
/// string) → PNG data URL, cached by location.
fn resource_icon(loc: &str) -> Option<String> {
    static ICONS: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);
    let loc = loc.trim();
    if loc.is_empty() {
        return None;
    }
    if let Some(hit) = ICONS
        .lock()
        .ok()
        .and_then(|c| c.as_ref().and_then(|m| m.get(loc).cloned()))
    {
        return hit;
    }
    let (file, index) = split_icon_location(loc);
    let url = icon_png_from_location(file, index, MODERN_ICON_PX).map(|png| data_url(&png));
    if let Ok(mut c) = ICONS.lock() {
        let map = c.get_or_insert_with(HashMap::new);
        if map.len() > 256 {
            map.clear();
        }
        map.insert(loc.to_string(), url.clone());
    }
    url
}

fn split_icon_location(loc: &str) -> (&str, i32) {
    if loc.starts_with('@') {
        return (loc, 0);
    }
    match loc.rsplit_once(',') {
        Some((file, index)) => match index.trim().parse::<i32>() {
            Ok(i) => (file.trim().trim_matches('"'), i),
            Err(_) => (loc.trim_matches('"'), 0),
        },
        None => (loc.trim_matches('"'), 0),
    }
}

// ---------------------------------------------------------------------------
// Activation failures
// ---------------------------------------------------------------------------

/// CLSIDs that could not be activated since the last manifest scan, with the
/// reason; skipped on later right-clicks.
static FAILED: Mutex<Vec<(u128, String)>> = Mutex::new(Vec::new());

fn failed_clsids() -> HashSet<u128> {
    FAILED
        .lock()
        .map(|f| f.iter().map(|(c, _)| *c).collect())
        .unwrap_or_default()
}

fn note_failure(v: &PackageVerb, e: &windows::core::Error) {
    let why = format!(
        "{} / {} {{{:?}}}: {}",
        v.package,
        v.verb_id,
        v.clsid,
        hr_msg(e.clone())
    );
    log::warn!("cannot activate {why}");
    if let Ok(mut f) = FAILED.lock() {
        f.push((v.clsid.to_u128(), why));
    }
}

/// Packaged commands that failed to activate.
#[cfg(test)]
fn activation_failures() -> Vec<String> {
    FAILED
        .lock()
        .map(|f| f.iter().map(|(_, w)| w.clone()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Package manifest catalog
// ---------------------------------------------------------------------------

struct Catalog {
    scanned: Instant,
    verbs: Vec<PackageVerb>,
}

static CATALOG: RwLock<Option<Arc<Catalog>>> = RwLock::new(None);
static SCANNING: AtomicBool = AtomicBool::new(false);

/// The current catalog (possibly stale; `None` before the first scan ends).
/// Starts a background rescan when it is missing or older than the TTL.
fn catalog() -> Option<Arc<Catalog>> {
    let current = CATALOG.read().ok().and_then(|c| c.clone());
    if current
        .as_ref()
        .is_none_or(|c| c.scanned.elapsed() > CATALOG_TTL)
    {
        refresh_catalog();
    }
    current
}

fn refresh_catalog() {
    if SCANNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("ferret-appx-scan".into())
        .spawn(|| {
            let verbs = scan_packages().unwrap_or_else(|e| {
                log::warn!("package scan failed: {e}");
                Vec::new()
            });
            if let Ok(mut c) = CATALOG.write() {
                *c = Some(Arc::new(Catalog {
                    scanned: Instant::now(),
                    verbs,
                }));
            }
            if let Ok(mut f) = FAILED.lock() {
                f.clear();
            }
            SCANNING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        SCANNING.store(false, Ordering::SeqCst);
    }
}

/// Multithreaded-apartment COM for the scan thread.
struct MtaGuard(bool);

impl MtaGuard {
    fn init() -> Self {
        MtaGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for MtaGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

/// Context-menu verbs of the current user's packages (main, optional and
/// sparse), from each package's `AppxManifest.xml`.
fn scan_packages() -> Result<Vec<PackageVerb>, String> {
    let _com = MtaGuard::init();
    let pm = PackageManager::new().map_err(hr_msg)?;
    let packages = pm
        .FindPackagesByUserSecurityIdWithPackageTypes(
            &HSTRING::new(),
            PackageTypes(PackageTypes::Main.0 | PackageTypes::Optional.0),
        )
        .map_err(hr_msg)?;
    let mut out = Vec::new();
    for pkg in packages {
        let Ok(dir) = pkg.InstalledPath() else {
            continue;
        };
        let manifest = PathBuf::from(dir.to_string()).join("AppxManifest.xml");
        let Ok(xml) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if !xml.contains("windows.fileExplorerContextMenus") {
            continue;
        }
        let name = pkg
            .Id()
            .and_then(|id| id.FullName())
            .map(|n| n.to_string())
            .unwrap_or_default();
        out.extend(parse_manifest(&xml, &name));
    }
    Ok(out)
}

/// `<Extension Category="windows.fileExplorerContextMenus">` →
/// `<ItemType Type="…">` → `<Verb Id="…" Clsid="…"/>`, in any namespace
/// (`desktop4`, `desktop5`, `desktop10`, …). Verbs are merged by (CLSID, Id).
fn parse_manifest(xml: &str, package: &str) -> Vec<PackageVerb> {
    use quick_xml::events::{BytesStart, Event};

    fn attr(e: &BytesStart, name: &str) -> Option<String> {
        e.attributes()
            .with_checks(false)
            .flatten()
            .find(|a| a.key.local_name().as_ref() == name)
            .map(|a| a.value.trim().to_string())
    }

    let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
    let mut depth = 0usize;
    let mut menus_at: Option<usize> = None;
    let mut item_type: Option<(usize, String)> = None;
    let mut out: Vec<PackageVerb> = Vec::new();
    let mut add_verb = |e: &BytesStart, ty: &str| {
        let (Some(id), Some(clsid)) = (attr(e, "Id"), attr(e, "Clsid")) else {
            return;
        };
        let Ok(clsid) = GUID::try_from(clsid.trim_matches(|c| c == '{' || c == '}')) else {
            return;
        };
        let ty = ty.to_lowercase();
        match out
            .iter_mut()
            .find(|v| v.clsid == clsid && v.verb_id == id)
        {
            Some(v) if !v.types.contains(&ty) => v.types.push(ty),
            Some(_) => {}
            None => out.push(PackageVerb {
                package: package.to_string(),
                verb_id: id,
                clsid,
                types: vec![ty],
            }),
        }
    };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                let name = e.local_name();
                match name.as_ref() {
                    "Extension"
                        if menus_at.is_none()
                            && attr(&e, "Category").as_deref()
                                == Some("windows.fileExplorerContextMenus") =>
                    {
                        menus_at = Some(depth)
                    }
                    "ItemType" if menus_at.is_some() => {
                        item_type = attr(&e, "Type").map(|t| (depth, t));
                    }
                    "Verb" => {
                        if let Some((_, ty)) = &item_type {
                            add_verb(&e, ty);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                if e.local_name().as_ref() == "Verb" {
                    if let Some((_, ty)) = &item_type {
                        add_verb(&e, ty);
                    }
                }
            }
            Ok(Event::End(_)) => {
                if item_type.as_ref().is_some_and(|(d, _)| *d == depth) {
                    item_type = None;
                }
                if menus_at == Some(depth) {
                    menus_at = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The modern-menu STA thread
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// When the job running on the menu thread started ([`now_ms`]); 0 = idle.
static JOB_STARTED_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// Has the current menu-thread job (a COM call into some packaged server)
/// been running for longer than a right-click may wait?
fn menu_thread_stuck() -> bool {
    let started = JOB_STARTED_MS.load(Ordering::SeqCst);
    started != 0 && now_ms().saturating_sub(started) > MODERN_TIMEOUT.as_millis() as u64
}

struct MenuThread {
    jobs: Mutex<mpsc::Sender<Job>>,
    thread_id: u32,
}

/// A persistent STA thread that pumps messages between jobs, so packaged COM
/// objects created on it stay usable (and their servers warm) across calls.
fn menu_thread() -> &'static MenuThread {
    static THREAD: OnceLock<MenuThread> = OnceLock::new();
    THREAD.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let (id_tx, id_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("ferret-shellmenu".into())
            .spawn(move || {
                let _com = ComGuard::init();
                let mut msg = MSG::default();
                unsafe {
                    // Create the message queue before anyone posts to it.
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                    let _ = id_tx.send(GetCurrentThreadId());
                }
                loop {
                    unsafe {
                        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                            let _ = TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                    loop {
                        match rx.try_recv() {
                            Ok(job) => {
                                JOB_STARTED_MS.store(now_ms(), Ordering::SeqCst);
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                                JOB_STARTED_MS.store(0, Ordering::SeqCst);
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => return,
                        }
                    }
                    // Posted after the drain above → still queued → no lost wake-up.
                    unsafe {
                        MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
                    };
                }
            });
        let thread_id = match spawned {
            Ok(_) => id_rx.recv().unwrap_or(0),
            Err(_) => 0,
        };
        MenuThread {
            jobs: Mutex::new(tx),
            thread_id,
        }
    })
}

/// Queues `f` on the menu thread; the receiver gets its result.
fn on_menu_thread<T, F>(f: F) -> mpsc::Receiver<Result<T, String>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let thread = menu_thread();
    let job: Job = Box::new(move || {
        let _ = tx.send(f());
    });
    let queued = thread
        .jobs
        .lock()
        .map(|jobs| jobs.send(job).is_ok())
        .unwrap_or(false);
    if queued && thread.thread_id != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread.thread_id, WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
    rx
}

/// Waits (bounded) for the modern half of a menu.
fn recv_modern(
    rx: &mpsc::Receiver<Result<Vec<ShellMenuItem>, String>>,
) -> Result<Vec<ShellMenuItem>, String> {
    match rx.recv_timeout(MODERN_TIMEOUT) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => Err("packaged menu entries timed out".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("The shell menu thread stopped".into()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> ShellMenuItem {
        ShellMenuItem {
            id: format!("c:{label}"),
            label: label.into(),
            icon: None,
            disabled: false,
            separator: false,
            children: Vec::new(),
            source: "classic",
        }
    }

    #[test]
    fn labels() {
        assert_eq!(clean_label("&Open with Code"), "Open with Code");
        assert_eq!(clean_label("Cu&t\tCtrl+X"), "Cut");
        assert_eq!(clean_label("Tom && Jerry"), "Tom & Jerry");
        assert_eq!(clean_label("開く(&O)"), "開く");
        assert_eq!(clean_label("  Send to "), "Send to");
        assert_eq!(label_key("Extract All…"), "extract all");
        assert_eq!(label_key("Open with..."), "open with");
    }

    #[test]
    fn builtin_filter() {
        assert!(is_builtin("rename", "Whatever"));
        assert!(is_builtin("", &clean_label("Create &shortcut")));
        assert!(is_builtin("", "Give access to"));
        assert!(is_builtin("", "Extract All..."));
        assert!(is_builtin("Windows.ModernShare", "Share"));
        assert!(!is_builtin("", "Send to"));
        assert!(!is_builtin("", "Scan with Microsoft Defender..."));
        assert!(!is_builtin("git_shell", "Open Git Bash here"));
    }

    #[test]
    fn separators_and_merge() {
        let sep = ShellMenuItem::separator("classic");
        let v = vec![
            sep.clone(),
            item("a"),
            sep.clone(),
            sep.clone(),
            item("b"),
            sep.clone(),
        ];
        let labels: Vec<String> = collapse_separators(v)
            .iter()
            .map(|i| if i.separator { "-".into() } else { i.label.clone() })
            .collect();
        assert_eq!(labels, ["a", "-", "b"]);

        let mut modern = item("Open with Code");
        modern.source = "modern";
        let merged = merge(
            vec![modern],
            vec![item("open with code"), item("7-Zip"), sep.clone()],
        );
        let labels: Vec<(&str, &str)> = merged
            .iter()
            .map(|i| (if i.separator { "-" } else { i.label.as_str() }, i.source))
            .collect();
        assert_eq!(
            labels,
            [("Open with Code", "modern"), ("-", "classic"), ("7-Zip", "classic")]
        );
        assert!(merge(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn ids() {
        assert_eq!(parse_id("c:12"), Some((Source::Classic, 12)));
        assert_eq!(parse_id("m:0"), Some((Source::Modern, 0)));
        assert_eq!(parse_id(""), None);
        assert_eq!(parse_id("x:1"), None);
        assert_eq!(parse_id("c:"), None);
    }

    #[test]
    fn icon_locations() {
        assert_eq!(
            split_icon_location(r"C:\a\Code.exe,0"),
            (r"C:\a\Code.exe", 0)
        );
        assert_eq!(
            split_icon_location(r#""C:\a b\x.dll",-101"#),
            (r"C:\a b\x.dll", -101)
        );
        assert_eq!(split_icon_location(r"C:\a\b.ico"), (r"C:\a\b.ico", 0));
        assert_eq!(
            split_icon_location("@{Pkg?ms-resource://x,y}"),
            ("@{Pkg?ms-resource://x,y}", 0)
        );
    }

    const MANIFEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
  xmlns:desktop4="http://schemas.microsoft.com/appx/manifest/desktop/windows10/4"
  xmlns:desktop5="http://schemas.microsoft.com/appx/manifest/desktop/windows10/5"
  xmlns:desktop10="http://schemas.microsoft.com/appx/manifest/desktop/windows10/10">
  <Applications><Application Id="App"><Extensions>
    <desktop4:Extension Category="windows.fileExplorerContextMenus">
      <desktop4:FileExplorerContextMenus>
        <desktop4:ItemType Type="*">
          <desktop4:Verb Id="Edit" Clsid="CA6CC9F1-867A-481E-951E-A28C5E4F01EA"/>
        </desktop4:ItemType>
        <desktop5:ItemType Type="Directory">
          <desktop5:Verb Id="Edit" Clsid="{CA6CC9F1-867A-481E-951E-A28C5E4F01EA}"></desktop5:Verb>
        </desktop5:ItemType>
        <desktop5:ItemType Type="Directory\Background">
          <desktop5:Verb Id="Other" Clsid="9f156763-7844-4dc4-b2b1-901f640f5155" />
        </desktop5:ItemType>
      </desktop4:FileExplorerContextMenus>
    </desktop4:Extension>
    <desktop10:Extension Category="windows.fileExplorerContextMenus">
      <desktop10:FileExplorerContextMenus>
        <desktop10:ItemType Type=".TXT">
          <desktop10:Verb Id="Txt" Clsid="11111111-2222-3333-4444-555555555555" />
        </desktop10:ItemType>
      </desktop10:FileExplorerContextMenus>
    </desktop10:Extension>
    <Extension Category="windows.other"><ItemType Type="*"><Verb Id="No" Clsid="11111111-2222-3333-4444-666666666666"/></ItemType></Extension>
  </Extensions></Application></Applications>
</Package>"#;

    #[test]
    fn manifest_parsing() {
        let verbs = parse_manifest(MANIFEST, "Pkg");
        let summary: Vec<(String, Vec<String>)> = verbs
            .iter()
            .map(|v| (v.verb_id.clone(), v.types.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                ("Edit".to_string(), vec!["*".to_string(), "directory".to_string()]),
                ("Other".to_string(), vec!["directory\\background".to_string()]),
                ("Txt".to_string(), vec![".txt".to_string()]),
            ]
        );
        assert_eq!(
            verbs[0].clsid,
            GUID::from_u128(0xCA6CC9F1_867A_481E_951E_A28C5E4F01EA)
        );
        let edit = &verbs[0];
        assert!(edit.applies_to(&ItemKind::File(Some(".md".into()))));
        assert!(edit.applies_to(&ItemKind::File(None)));
        assert!(edit.applies_to(&ItemKind::Directory));
        assert!(!edit.applies_to(&ItemKind::Background));
        assert!(!edit.applies_to(&ItemKind::Drive));
        assert!(verbs[1].applies_to(&ItemKind::Background));
        assert!(verbs[2].applies_to(&ItemKind::File(Some(".txt".into()))));
        assert!(!verbs[2].applies_to(&ItemKind::File(Some(".md".into()))));
    }

    #[test]
    fn item_kinds() {
        assert_eq!(ItemKind::of(r"C:\"), ItemKind::Drive);
        assert_eq!(ItemKind::of("c:"), ItemKind::Drive);
        let windir = std::env::var("WINDIR").unwrap_or(r"C:\Windows".into());
        assert_eq!(ItemKind::of(&windir), ItemKind::Directory);
        assert_eq!(
            ItemKind::of(&format!(r"{windir}\notepad.EXE")),
            ItemKind::File(Some(".exe".into()))
        );
        assert_eq!(ItemKind::of(r"C:\no\such\Makefile"), ItemKind::File(None));
    }

    #[test]
    fn json_shape() {
        let menu = ShellMenu {
            token: 7,
            items: vec![item("7-Zip")],
        };
        let v = serde_json::to_value(&menu).unwrap();
        assert_eq!(v["token"], 7);
        let i = &v["items"][0];
        for key in ["id", "label", "icon", "disabled", "separator", "children", "source"] {
            assert!(i.get(key).is_some(), "missing {key}");
        }
        assert_eq!(i["source"], "classic");
    }
}

/// Live checks against this machine's shell. Run with
/// `cargo test --lib shellmenu::live -- --ignored --nocapture --test-threads=1`.
/// Everything happens in a fresh temp folder that is removed afterwards.
#[cfg(test)]
mod live {
    use super::*;
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, EnumWindows, GetClassNameW, GetWindowTextW,
        IsWindowVisible, PostMessageW, WINDOW_EX_STYLE, WM_CLOSE, WS_POPUP,
    };

    /// `%TEMP%\ferret-shellmenu-<pid>-<n>` with `notes.txt`, `second.txt`
    /// and `sub\`; removed on drop.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Fixture {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            let root = std::env::temp_dir()
                .join(format!("ferret-shellmenu-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(root.join("sub")).unwrap();
            std::fs::write(root.join("notes.txt"), "ferret shell menu test\r\n").unwrap();
            std::fs::write(root.join("second.txt"), "second\r\n").unwrap();
            // An empty zip archive (end-of-central-directory record only).
            let mut zip = b"PK\x05\x06".to_vec();
            zip.resize(22, 0);
            std::fs::write(root.join("archive.zip"), zip).unwrap();
            let root = PathBuf::from(paths::long_form(&root.display().to_string()));
            Fixture { root }
        }

        fn path(&self, rel: &str) -> String {
            let p = if rel.is_empty() {
                self.root.clone()
            } else {
                self.root.join(rel)
            };
            paths::normalize(&p.display().to_string())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for _ in 0..20 {
                if std::fs::remove_dir_all(&self.root).is_ok() || !self.root.exists() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            println!("WARNING: could not remove {}", self.root.display());
        }
    }

    fn hidden_window() -> HWND {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                &HSTRING::from("STATIC"),
                &HSTRING::from("ferret-shellmenu-test"),
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

    /// Scans synchronously (like warm_up, but waited for).
    fn ensure_catalog() -> Duration {
        let t = Instant::now();
        let verbs = scan_packages().expect("package scan");
        *CATALOG.write().unwrap() = Some(Arc::new(Catalog {
            scanned: Instant::now(),
            verbs,
        }));
        t.elapsed()
    }

    fn print_items(items: &[ShellMenuItem], depth: usize) {
        for i in items {
            let pad = "    ".repeat(depth + 1);
            if i.separator {
                println!("{pad}----");
                continue;
            }
            let icon = match &i.icon {
                Some(url) => {
                    let b64 = url.trim_start_matches("data:image/png;base64,");
                    let png = base64::engine::general_purpose::STANDARD
                        .decode(b64)
                        .unwrap_or_default();
                    if png.len() > 24 {
                        let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
                        let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
                        format!("icon {w}x{h}")
                    } else {
                        "icon ?".into()
                    }
                }
                None => "no icon".into(),
            };
            println!(
                "{pad}[{}] {:?} id={:?} {icon}{}{}",
                i.source,
                i.label,
                i.id,
                if i.disabled { " DISABLED" } else { "" },
                if i.children.is_empty() { "" } else { " >" }
            );
            print_items(&i.children, depth + 1);
        }
    }

    /// Builds a menu the way `shell_menu_items` does (modern half on the menu
    /// thread in parallel, classic half here). Returns it with timings.
    fn build(
        hwnd: HWND,
        paths_in: &[String],
        background: bool,
    ) -> (ShellMenu, Duration, Duration, Duration) {
        let t = Instant::now();
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let rx = {
            let p = paths_in.to_vec();
            on_menu_thread(move || {
                let t = Instant::now();
                modern_menu(token, &p, background).map(|v| (v, t.elapsed()))
            })
        };
        let tc = Instant::now();
        let classic = classic_menu(token, hwnd, paths_in, background);
        let classic_time = tc.elapsed();
        let (modern, modern_time) = match rx.recv().unwrap() {
            Ok((v, d)) => (Ok(v), d),
            Err(e) => (Err(e), Duration::ZERO),
        };
        if let Err(e) = &classic {
            println!("  classic error: {e}");
        }
        if let Err(e) = &modern {
            println!("  modern error: {e}");
        }
        let items = merge(modern.unwrap_or_default(), classic.unwrap_or_default());
        (ShellMenu { token, items }, t.elapsed(), classic_time, modern_time)
    }

    /// Raw classic menu (before filtering) with verbs, for reference.
    fn dump_raw(hwnd: HWND, paths_in: &[String], background: bool) {
        let (cm, _p, kind) = if background {
            (
                background_context_menu(hwnd, &paths_in[0]).unwrap(),
                Vec::new(),
                MenuKind::Background,
            )
        } else {
            let (cm, p, _d) = item_context_menu(hwnd, paths_in).unwrap();
            (cm, p, MenuKind::Items)
        };
        let hmenu = MenuGuard(unsafe { CreatePopupMenu() }.unwrap());
        populate_menu(&cm, hmenu.0, kind).unwrap();
        let count = unsafe { GetMenuItemCount(Some(hmenu.0)) };
        for pos in 0..count as u32 {
            let Some(raw) = menu_item(hmenu.0, pos) else {
                continue;
            };
            if raw.separator {
                println!("      ----");
                continue;
            }
            let verb = if (ID_CMD_FIRST..=ID_CMD_LAST).contains(&raw.id) && raw.submenu.is_none()
            {
                command_verb(&cm, (raw.id - ID_CMD_FIRST) as usize)
            } else {
                String::new()
            };
            println!(
                "      {:?} verb={verb:?}{}{}",
                raw.text,
                if raw.submenu.is_some() { " >" } else { "" },
                if is_builtin(&verb, &clean_label(&raw.text)) {
                    "   (excluded)"
                } else {
                    ""
                }
            );
            // Verbs inside third-party submenus (7-Zip), for the filter.
            if let (Some(sub), "7-Zip") = (raw.submenu, raw.text.as_str()) {
                let n = unsafe { GetMenuItemCount(Some(sub)) };
                for i in 0..n.max(0) as u32 {
                    if let Some(c) = menu_item(sub, i) {
                        if (ID_CMD_FIRST..=ID_CMD_LAST).contains(&c.id) && c.submenu.is_none() {
                            println!(
                                "          {:?} verb={:?}",
                                c.text,
                                command_verb(&cm, (c.id - ID_CMD_FIRST) as usize)
                            );
                        }
                    }
                }
            }
        }
    }

    fn report(title: &str, hwnd: HWND, paths_in: &[String], background: bool) -> ShellMenu {
        println!("\n=== {title}");
        println!("  raw classic menu:");
        dump_raw(hwnd, paths_in, background);
        let (menu, cold, c1, m1) = build(hwnd, paths_in, background);
        let (menu2, warm, c2, m2) = build(hwnd, paths_in, background);
        println!("  result (token {}):", menu2.token);
        print_items(&menu2.items, 0);
        println!(
            "  timing: first {cold:?} (classic {c1:?}, modern {m1:?}); again {warm:?} (classic {c2:?}, modern {m2:?})"
        );
        assert_eq!(menu.items.len(), menu2.items.len(), "unstable menu");
        menu2
    }

    #[test]
    #[ignore]
    fn menus() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        let t = Instant::now();
        let scan = ensure_catalog();
        println!("package scan: {scan:?} (total {:?})", t.elapsed());

        let txt = fx.path("notes.txt");
        let sub = fx.path("sub");
        let second = fx.path("second.txt");
        let file_menu = report(".txt file", hwnd, std::slice::from_ref(&txt), false);
        report("folder", hwnd, std::slice::from_ref(&sub), false);
        report("folder background", hwnd, std::slice::from_ref(&sub), true);
        report("two files", hwnd, &[txt.clone(), second.clone()], false);
        report("file + folder", hwnd, &[txt.clone(), sub.clone()], false);
        report(".zip file", hwnd, &[fx.path("archive.zip")], false);
        report("drive root", hwnd, &[r"C:\".to_string()], false);

        let json = serde_json::to_string_pretty(&ShellMenu {
            token: file_menu.token,
            items: file_menu.items.iter().take(3).cloned().collect(),
        })
        .unwrap();
        let short: String = json
            .lines()
            .map(|l| {
                if l.len() > 140 {
                    format!("{}…\",", &l[..120])
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!("\nJSON example (icons truncated):\n{short}");
        let failures = activation_failures();
        println!("\nactivation failures: {failures:#?}");
        assert!(!file_menu.items.is_empty());
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    /// Every packaged context-menu verb on this machine, and whether its COM
    /// class activates from a non-Explorer process (no command is invoked).
    #[test]
    #[ignore]
    fn catalog_and_activation() {
        let fx = Fixture::new();
        let scan = ensure_catalog();
        let catalog = catalog().unwrap();
        println!("scan {scan:?}: {} verbs", catalog.verbs.len());
        let (txt, sub) = (fx.path("notes.txt"), fx.path("sub"));
        let rx = on_menu_thread({
            let verbs = catalog.verbs.clone();
            move || {
                let mut lines = Vec::new();
                let txt_array = shell_item_array(std::slice::from_ref(&txt))?;
                let dir_array = shell_item_array(std::slice::from_ref(&sub))?;
                for v in &verbs {
                    // What the command reports for a .txt file / a folder.
                    if let Ok(cmd) = unsafe {
                        CoCreateInstance::<_, IExplorerCommand>(
                            &v.clsid,
                            None,
                            CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER,
                        )
                    } {
                        for (what, array) in [("txt", &txt_array), ("dir", &dir_array)] {
                            let state = unsafe { cmd.GetState(array, true) };
                            let title = unsafe { cmd.GetTitle(array) }.map(take_pwstr);
                            let icon = unsafe { cmd.GetIcon(array) }.map(take_pwstr);
                            let flags = unsafe { cmd.GetFlags() };
                            lines.push(format!(
                                "    {} on {what}: state={:?} title={:?} flags={:?} icon={:?}",
                                v.verb_id,
                                state.map_err(hr_msg),
                                title.map_err(hr_msg),
                                flags.map_err(hr_msg),
                                icon.map_err(hr_msg),
                            ));
                        }
                    }
                }
                for v in &verbs {
                    let t = Instant::now();
                    let r = unsafe {
                        CoCreateInstance::<_, IExplorerCommand>(
                            &v.clsid,
                            None,
                            CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER,
                        )
                    };
                    let first = t.elapsed();
                    let t2 = Instant::now();
                    let again = unsafe {
                        CoCreateInstance::<_, IExplorerCommand>(
                            &v.clsid,
                            None,
                            CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER,
                        )
                    }
                    .is_ok();
                    lines.push(format!(
                        "{} / {} {:?} types={:?}: {} ({first:?}, again {:?} ok={again})",
                        v.package,
                        v.verb_id,
                        v.clsid,
                        v.types,
                        match &r {
                            Ok(_) => "activates".to_string(),
                            Err(e) => format!("FAILS {}", hr_msg(e.clone())),
                        },
                        t2.elapsed()
                    ));
                    drop(r);
                }
                Ok(lines)
            }
        });
        for l in rx.recv().unwrap().unwrap() {
            println!("  {l}");
        }
        for name in [
            "Microsoft.WindowsNotepad",
            "ZedIndustries.Zed",
            "Microsoft.VisualStudioCode",
            "Microsoft.WindowsTerminal",
            "PowerToys",
        ] {
            let found = catalog.verbs.iter().any(|v| v.package.contains(name));
            println!("{name}: {}", if found { "declares entries" } else { "not found" });
        }
    }

    /// Visible top-level windows of class `class` (any process).
    fn windows_of_class(class: &str) -> Vec<(isize, String)> {
        struct Ctx {
            class: String,
            out: Vec<(isize, String)>,
        }
        unsafe extern "system" fn each(h: HWND, lp: LPARAM) -> windows::core::BOOL {
            let ctx = unsafe { &mut *(lp.0 as *mut Ctx) };
            let (mut t, mut c) = ([0u16; 256], [0u16; 256]);
            let m = unsafe { GetClassNameW(h, &mut c) } as usize;
            if unsafe { IsWindowVisible(h) }.as_bool()
                && String::from_utf16_lossy(&c[..m]) == ctx.class
            {
                let n = unsafe { GetWindowTextW(h, &mut t) } as usize;
                ctx.out.push((h.0 as isize, String::from_utf16_lossy(&t[..n])));
            }
            true.into()
        }
        let mut ctx = Ctx {
            class: class.into(),
            out: Vec::new(),
        };
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut ctx as *mut Ctx as isize));
        }
        ctx.out
    }

    fn pump_for(ms: u64, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(ms) {
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
            std::thread::sleep(Duration::from_millis(50));
        }
        done()
    }

    /// Waits for a new window of `class`, then closes it.
    fn close_new_window(class: &str, before: &[(isize, String)]) -> Option<String> {
        let mut found: Option<(isize, String)> = None;
        pump_for(15000, || {
            found = windows_of_class(class)
                .into_iter()
                .find(|w| !before.iter().any(|b| b.0 == w.0));
            found.is_some()
        });
        let (h, title) = found?;
        std::thread::sleep(Duration::from_millis(1500)); // let it finish starting
        unsafe {
            let _ = PostMessageW(
                Some(HWND(h as *mut std::ffi::c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
        }
        let closed = pump_for(10000, || !windows_of_class(class).iter().any(|w| w.0 == h));
        println!("  window {title:?} ({class}) closed={closed}");
        Some(title)
    }

    fn find<'a>(items: &'a [ShellMenuItem], label: &str, source: &str) -> Option<&'a ShellMenuItem> {
        items.iter().find(|i| {
            !i.separator && i.source == source && i.label.eq_ignore_ascii_case(label)
        })
    }

    /// Modern invoke: "Open in Terminal" on the temp folder's background
    /// opens a Windows Terminal window there, which is then closed.
    #[test]
    #[ignore]
    fn invoke_modern_terminal() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        ensure_catalog();
        let folder = fx.path("sub");
        let (menu, ..) = build(hwnd, std::slice::from_ref(&folder), true);
        print_items(&menu.items, 0);
        let entry = find(&menu.items, "Open in Terminal", "modern").expect("Open in Terminal");
        let (_, index) = parse_id(&entry.id).unwrap();
        let class = "CASCADIA_HOSTING_WINDOW_CLASS";
        let before = windows_of_class(class);
        let token = menu.token;
        let t = Instant::now();
        on_menu_thread(move || modern_invoke(token, index))
            .recv()
            .unwrap()
            .expect("invoke");
        println!("  Invoke returned after {:?}", t.elapsed());
        let title = close_new_window(class, &before).expect("no Terminal window appeared");
        println!("  terminal opened: {title:?}");
        // Expired tokens are reported, not crashed on.
        let r = on_menu_thread(move || modern_invoke(u64::MAX, 0)).recv().unwrap();
        assert!(r.is_err());
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    /// Classic invoke: "Open Git Bash here" on the temp folder's background
    /// opens a mintty window there, which is then closed.
    #[test]
    #[ignore]
    fn invoke_classic_git_bash() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        let folder = fx.path("sub");
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let items = classic_menu(token, hwnd, std::slice::from_ref(&folder), true).unwrap();
        let entry = find(&items, "Open Git Bash here", "classic").expect("Open Git Bash here");
        let (_, offset) = parse_id(&entry.id).unwrap();
        let before = windows_of_class("mintty");
        let t = Instant::now();
        let r = classic_invoke(token, offset, hwnd).expect("invoke");
        println!("  InvokeCommand returned {r:?} after {:?}", t.elapsed());
        assert_eq!(r, None);
        let title = close_new_window("mintty", &before).expect("no Git Bash window appeared");
        println!("  git bash opened: {title:?}");
        assert!(classic_invoke(u64::MAX, offset, hwnd).is_err());
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    /// The app's flow: `warm_up` at startup (scan + pre-warm in the
    /// background), then right-clicks. Prints first and warm timings.
    #[test]
    #[ignore]
    fn startup_flow_timings() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        let t = Instant::now();
        refresh_catalog();
        let _ = menu_thread();
        let prewarm = prewarm_classic().unwrap();
        prewarm.join().unwrap();
        let prewarm_done = t.elapsed();
        while CATALOG.read().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
        println!(
            "warm_up: classic pre-warm {prewarm_done:?}, package scan done after {:?}",
            t.elapsed()
        );
        let (txt, second, sub) = (fx.path("notes.txt"), fx.path("second.txt"), fx.path("sub"));
        let cases: Vec<(&str, Vec<String>, bool)> = vec![
            (".txt file", vec![txt.clone()], false),
            ("folder", vec![sub.clone()], false),
            ("background", vec![sub.clone()], true),
            ("two files", vec![txt, second], false),
        ];
        for (name, p, background) in &cases {
            let (menu, first, c1, m1) = build(hwnd, p, *background);
            let mut warm = Vec::new();
            for _ in 0..5 {
                let (_, total, c, m) = build(hwnd, p, *background);
                warm.push((total, c, m));
            }
            let avg = |f: fn(&(Duration, Duration, Duration)) -> Duration| {
                warm.iter().map(f).sum::<Duration>() / warm.len() as u32
            };
            println!(
                "{name:11} {} items | first: total {first:?} (classic {c1:?}, modern {m1:?}) | warm avg of 5: total {:?} (classic {:?}, modern {:?})",
                menu.items.len(),
                avg(|w| w.0),
                avg(|w| w.1),
                avg(|w| w.2),
            );
        }
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    /// Where classic time goes: building the menu, `QueryContextMenu`, and
    /// filling each submenu. First pass is this process's first menu.
    #[test]
    #[ignore]
    fn classic_timing_breakdown() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        let cases: Vec<(&str, Vec<String>, bool)> = vec![
            ("background", vec![fx.path("sub")], true),
            (".txt", vec![fx.path("notes.txt")], false),
            ("folder", vec![fx.path("sub")], false),
        ];
        for pass in ["first", "second", "third"] {
            for (name, p, background) in &cases {
                let t = Instant::now();
                let (cm, _pidls, kind) = if *background {
                    (
                        background_context_menu(hwnd, &p[0]).unwrap(),
                        Vec::new(),
                        MenuKind::Background,
                    )
                } else {
                    let (cm, pidls, _) = item_context_menu(hwnd, p).unwrap();
                    (cm, pidls, MenuKind::Items)
                };
                let build = t.elapsed();
                let hmenu = MenuGuard(unsafe { CreatePopupMenu() }.unwrap());
                let t = Instant::now();
                populate_menu(&cm, hmenu.0, kind).unwrap();
                let query = t.elapsed();
                let walker = ClassicWalker {
                    cm: &cm,
                    cm2: cm.cast().ok(),
                    cm3: cm.cast().ok(),
                };
                let mut subs = Vec::new();
                let count = unsafe { GetMenuItemCount(Some(hmenu.0)) } as u32;
                for pos in 0..count {
                    if let Some(raw) = menu_item(hmenu.0, pos) {
                        if let Some(sub) = raw.submenu {
                            let t = Instant::now();
                            walker.init_popup(sub, pos);
                            subs.push(format!("{} {:?}", clean_label(&raw.text), t.elapsed()));
                        }
                    }
                }
                let hmenu2 = MenuGuard(unsafe { CreatePopupMenu() }.unwrap());
                populate_menu(&cm, hmenu2.0, kind).unwrap();
                let t = Instant::now();
                let n = walker.walk(hmenu2.0, 0).len();
                let walk = t.elapsed();
                println!(
                    "{pass:6} {name:10} build {build:?}, QueryContextMenu {query:?}, filtered walk {walk:?} ({n} items); submenu init (all): {subs:?}"
                );
            }
        }
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    /// Classic invoke without any window: background "New ▸ Text Document"
    /// creates a file in the temp folder (and only there).
    #[test]
    #[ignore]
    fn invoke_classic_new_text_document() {
        unsafe { OleInitialize(None) }.unwrap();
        let hwnd = hidden_window();
        let fx = Fixture::new();
        let folder = fx.path("sub");
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let items = classic_menu(token, hwnd, std::slice::from_ref(&folder), true).unwrap();
        let new = find(&items, "New", "classic").expect("New submenu");
        let entry = find(&new.children, "Text Document", "classic").expect("Text Document");
        let (_, offset) = parse_id(&entry.id).unwrap();
        let r = classic_invoke(token, offset, hwnd).expect("invoke");
        println!("  InvokeCommand returned {r:?}");
        let ok = pump_for(5000, || {
            std::fs::read_dir(&folder)
                .map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "txt")))
                .unwrap_or(false)
        });
        for e in std::fs::read_dir(&folder).unwrap().flatten() {
            println!("  created: {:?}", e.file_name());
        }
        assert!(ok, "no text document was created");
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }
}
