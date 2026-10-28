//! File operations through the Windows shell copy engine (`IFileOperation`).
//!
//! `fileops.rs` works with `std::fs`, which fails with "Access denied" in
//! protected folders such as `C:\Program Files`. Before starting, `fileops`
//! can ask [`needs_admin_write`] / [`needs_admin_delete`]; when the answer is
//! yes the UI shows its own "Administrator permission needed" dialog and, if
//! the user agrees, re-runs the operation here with `elevate = true`. The shell
//! then shows the UAC consent prompt itself and does the work in an elevated
//! copy-engine broker. The Recycle Bin view uses the same engine, without
//! elevation, to restore (`Restore`) and permanently delete (`Purge`) items.
//!
//! Everything runs on a dedicated thread with its own single-threaded COM
//! apartment (`IFileOperation` doesn't work in an MTA, and pooled threads may
//! already have one). The core, [`perform`], doesn't need Tauri; it reports
//! through the small [`Report`] trait, which `JobCtx` implements.
//!
//! What each operation reports (`created` / `pair(from, to)`):
//! * Transfer: each new top-level item → created + pair(source, new path). The
//!   engine also calls back for everything inside a copied folder; that is only
//!   reported when merging into an existing folder (Replace): new items inside
//!   → created + pair, overwritten files → pair only (nothing to delete on undo).
//!   KeepBoth names come from the shell: "a - Copy.txt", "a - Copy (2).txt".
//! * Delete to the Recycle Bin: pair(original, `$R…` path) (feed to Restore);
//!   permanent delete and Purge report nothing but progress.
//! * Rename, MoveBack, Restore: created(new path) + pair(old path, new path).
//! * NewFolder / NewFile: created(new path).
//!
//! Cancellation: an error from a `Pre*` callback fails that item with
//! ERROR_CANCELLED and FOFX_EARLYFAILURE stops the rest; an error from
//! `UpdateProgress` stops the engine mid-file (it removes the partial copy).
//! A declined UAC prompt (or any "no" in shell UI) ends the job as cancelled.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Deserialize;
use tauri::AppHandle;
use windows::core::{implement, Interface, Ref, HRESULT, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, E_ABORT, E_POINTER, HANDLE, HWND, PROPERTYKEY};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CreateBindCtx, BIND_OPTS, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, STGM_CREATE,
};
use windows::Win32::UI::Shell::{
    FileOperation, IEnumShellItems, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IFileSystemBindData, IFileSystemBindData_Impl, IShellItem, IShellItem2, SHCreateItemFromParsingName,
    SHGetKnownFolderItem, BHID_EnumItems, FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, STR_FILE_SYS_BIND_DATA,
    COPYENGINE_E_ACCESS_DENIED_DEST, COPYENGINE_E_ACCESS_DENIED_SRC, COPYENGINE_E_ALREADY_EXISTS_FOLDER,
    COPYENGINE_E_ALREADY_EXISTS_NORMAL, COPYENGINE_E_ALREADY_EXISTS_READONLY, COPYENGINE_E_ALREADY_EXISTS_SYSTEM,
    COPYENGINE_E_CANCELLED, COPYENGINE_E_CANNOT_MOVE_FROM_RECYCLE_BIN, COPYENGINE_E_DEST_SAME_TREE,
    COPYENGINE_E_DEST_SUBTREE, COPYENGINE_E_DISK_FULL, COPYENGINE_E_DISK_FULL_CLEAN, COPYENGINE_E_FILE_IS_FLD_DEST,
    COPYENGINE_E_FILE_TOO_LARGE, COPYENGINE_E_FLD_IS_FILE_DEST, COPYENGINE_E_PATH_NOT_FOUND_DEST,
    COPYENGINE_E_PATH_NOT_FOUND_SRC, COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, COPYENGINE_E_RECYCLE_FORCE_NUKE,
    COPYENGINE_E_RECYCLE_PATH_TOO_LONG, COPYENGINE_E_RECYCLE_SIZE_TOO_BIG, COPYENGINE_E_RECYCLE_UNKNOWN_ERROR,
    COPYENGINE_E_REQUIRES_ELEVATION, COPYENGINE_E_SAME_FILE, COPYENGINE_E_SHARING_VIOLATION_DEST,
    COPYENGINE_E_SHARING_VIOLATION_SRC, COPYENGINE_E_USER_CANCELLED, FILEOPERATION_FLAGS, FOFX_EARLYFAILURE,
    FOFX_PRESERVEFILEEXTENSIONS, FOFX_RECYCLEONDELETE, FOFX_REQUIREELEVATION, FOFX_SHOWELEVATIONPROMPT,
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI, FOF_RENAMEONCOLLISION, FOF_SILENT,
    FOF_WANTNUKEWARNING, PID_DISPLACED_FROM, PSGUID_DISPLACED, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_FILESYSPATH, SIGDN_PARENTRELATIVE,
};

use crate::fileops::{self, Policy};
use crate::jobs::{self, JobCtx};
use crate::listing::join;
use crate::paths;

type Res<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ShellOp {
    /// Copy or move `sources` into `dest`.
    Transfer { op: String /* "copy" | "move" */, sources: Vec<String>, dest: String, policy: Policy },
    /// To the Recycle Bin, or permanently.
    Delete { items: Vec<String>, permanent: bool },
    Rename { path: String, new_name: String },
    NewFolder { dir: String, name: String },
    NewFile { dir: String, name: String },
    /// Undo of a move: moves each `to` back to `from` (pairs are (from, to)).
    MoveBack { pairs: Vec<(String, String)> },
    /// Recycle Bin items (their `C:\$Recycle.Bin\<SID>\$R…` paths) → back to their original location.
    Restore { items: Vec<String> },
    /// Recycle Bin items → deleted permanently.
    Purge { items: Vec<String> },
}

/// Runs `op` through the shell copy engine as a background job and returns its id.
/// With `elevate`, the shell may show the UAC consent prompt (owned by `window`);
/// declining it ends the job as "cancelled".
#[tauri::command]
pub fn shell_op(app: AppHandle, window: tauri::WebviewWindow, op: ShellOp, elevate: bool) -> Res<u64> {
    validate(&op)?;
    let owner = window.hwnd().ok().map(|h| h.0 as isize);
    let (kind, label) = describe(&op);
    log::info!("shell op (elevate={elevate}): {op:?}");
    Ok(jobs::spawn(&app, kind, &label, move |ctx| perform(&op, owner.map(hwnd_from_raw), elevate, ctx)))
}

/// Progress / result sink for [`perform`]. `JobCtx` implements it for real jobs.
pub trait Report: Sync {
    fn cancelled(&self) -> bool;
    /// The operation was cancelled from outside the app (e.g. UAC declined).
    fn mark_cancelled(&self);
    fn totals(&self, bytes: u64, files: u64);
    fn current(&self, name: &str);
    fn add_bytes(&self, n: u64);
    fn file_done(&self);
    fn created(&self, path: &str);
    fn pair(&self, from: &str, to: &str);
    fn skipped(&self);
    /// Debug trace of every shell callback (tests only).
    fn wants_trace(&self) -> bool {
        false
    }
    fn trace(&self, _line: &str) {}
}

impl Report for JobCtx {
    fn cancelled(&self) -> bool {
        self.is_cancelled()
    }
    fn mark_cancelled(&self) {
        JobCtx::mark_cancelled(self)
    }
    fn totals(&self, bytes: u64, files: u64) {
        self.set_totals(bytes, files)
    }
    fn current(&self, name: &str) {
        self.set_current(name)
    }
    fn add_bytes(&self, n: u64) {
        JobCtx::add_bytes(self, n)
    }
    fn file_done(&self) {
        JobCtx::file_done(self)
    }
    fn created(&self, path: &str) {
        JobCtx::created(self, path)
    }
    fn pair(&self, from: &str, to: &str) {
        JobCtx::pair(self, from, to)
    }
    fn skipped(&self) {
        JobCtx::skipped(self)
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// HRESULT_FROM_WIN32(ERROR_CANCELLED)
const E_CANCELLED: HRESULT = HRESULT(0x8007_04C7_u32 as i32);
const NEW_FILE_ATTRIBUTES: u32 = 0x80; // FILE_ATTRIBUTE_NORMAL
const NEW_FOLDER_ATTRIBUTES: u32 = 0x10; // FILE_ATTRIBUTE_DIRECTORY

fn hwnd_from_raw(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn file_name(p: &str) -> String {
    let n = paths::normalize(p);
    match n.rsplit('\\').next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => n,
    }
}

fn quoted_or_count(items: &[String], name: impl Fn(&str) -> String) -> String {
    if items.len() == 1 {
        format!("“{}”", name(&items[0]))
    } else {
        format!("{} items", items.len())
    }
}

fn exists(p: &str) -> bool {
    fs::symlink_metadata(paths::for_io(p)).is_ok()
}

/// Balances a per-thread `CoInitializeEx` (see `shell.rs`).
struct ComGuard(bool);

impl ComGuard {
    fn init() -> Self {
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

fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

fn pcwstr(p: &PCWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { p.to_string() }.ok().filter(|s| !s.is_empty())
}

fn display_name(item: &IShellItem, sigdn: SIGDN) -> Option<String> {
    unsafe { item.GetDisplayName(sigdn) }.ok().map(take_pwstr).filter(|s| !s.is_empty())
}

/// Ferret-style path of a shell item (file-system path, else its parsing name).
fn item_path(item: &IShellItem) -> Option<String> {
    display_name(item, SIGDN_FILESYSPATH)
        .or_else(|| display_name(item, SIGDN_DESKTOPABSOLUTEPARSING))
        .map(|p| paths::normalize(&p))
}

fn hr_text(e: &windows::core::Error) -> String {
    let m = e.message();
    let m = m.trim().trim_end_matches('.');
    if m.is_empty() {
        format!("error {:#010x}", e.code().0 as u32)
    } else {
        m.to_string()
    }
}

/// A plain parsing name → shell item (share roots may need a trailing `\`).
fn shell_item(path: &str) -> Res<IShellItem> {
    let parse = |p: &str| unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(p), None) };
    match parse(path) {
        Ok(i) => Ok(i),
        Err(e) => {
            let alt = paths::for_io(path);
            if alt != path {
                if let Ok(i) = parse(&alt) {
                    return Ok(i);
                }
            }
            Err(if exists(path) {
                format!("“{}”: {}", file_name(path), hr_text(&e))
            } else {
                format!("“{}” no longer exists", file_name(path))
            })
        }
    }
}

/// Find data handed to the file-system parser so it can make an item for a
/// path that doesn't exist (yet).
#[implement(IFileSystemBindData)]
struct FolderFindData(WIN32_FIND_DATAW);

#[allow(non_snake_case)]
impl IFileSystemBindData_Impl for FolderFindData_Impl {
    fn SetFindData(&self, _pfd: *const WIN32_FIND_DATAW) -> windows::core::Result<()> {
        Ok(())
    }
    fn GetFindData(&self, pfd: *mut WIN32_FIND_DATAW) -> windows::core::Result<()> {
        if pfd.is_null() {
            return Err(windows::core::Error::from_hresult(E_POINTER));
        }
        unsafe { *pfd = self.0 };
        Ok(())
    }
}

/// Shell item for a destination folder that may not exist any more (move back,
/// restore). A missing folder is parsed with file-system bind data marking it a
/// directory, and the copy engine creates it (FOF_NOCONFIRMMKDIR), elevated if
/// the operation is.
fn folder_item(path: &str, may_be_missing: bool) -> Res<IShellItem> {
    if !may_be_missing || exists(path) {
        return shell_item(path);
    }
    unsafe {
        let make = || -> windows::core::Result<IShellItem> {
            let bc = CreateBindCtx(0)?;
            let opts = BIND_OPTS {
                cbStruct: std::mem::size_of::<BIND_OPTS>() as u32,
                grfFlags: 0,
                grfMode: STGM_CREATE.0,
                dwTickCountDeadline: 0,
            };
            bc.SetBindOptions(&opts)?;
            let fd = WIN32_FIND_DATAW { dwFileAttributes: FILE_ATTRIBUTE_DIRECTORY.0, ..Default::default() };
            let data: IFileSystemBindData = FolderFindData(fd).into();
            bc.RegisterObjectParam(STR_FILE_SYS_BIND_DATA, &data)?;
            SHCreateItemFromParsingName(&HSTRING::from(path), &bc)
        };
        make().map_err(|e| format!("“{path}”: {}", hr_text(&e)))
    }
}


fn is_cancel_code(hr: HRESULT) -> bool {
    hr == E_CANCELLED || hr == COPYENGINE_E_USER_CANCELLED || hr == COPYENGINE_E_CANCELLED || hr == E_ABORT
}

/// Readable reason for a failed item.
fn reason(hr: HRESULT) -> String {
    let known = [
        (COPYENGINE_E_REQUIRES_ELEVATION, "administrator permission is needed"),
        (COPYENGINE_E_ACCESS_DENIED_SRC, "access denied"),
        (COPYENGINE_E_ACCESS_DENIED_DEST, "access denied"),
        (HRESULT(0x8007_0005_u32 as i32), "access denied"),
        (COPYENGINE_E_SHARING_VIOLATION_SRC, "it is open in another program"),
        (COPYENGINE_E_SHARING_VIOLATION_DEST, "the destination is open in another program"),
        (HRESULT(0x8007_0020_u32 as i32), "it is open in another program"),
        (COPYENGINE_E_ALREADY_EXISTS_NORMAL, "an item with that name already exists"),
        (COPYENGINE_E_ALREADY_EXISTS_READONLY, "a read-only item with that name already exists"),
        (COPYENGINE_E_ALREADY_EXISTS_SYSTEM, "a system item with that name already exists"),
        (COPYENGINE_E_ALREADY_EXISTS_FOLDER, "a folder with that name already exists"),
        (HRESULT(0x8007_00B7_u32 as i32), "an item with that name already exists"),
        (COPYENGINE_E_DISK_FULL, "there isn't enough free space"),
        (COPYENGINE_E_DISK_FULL_CLEAN, "there isn't enough free space"),
        (COPYENGINE_E_PATH_NOT_FOUND_SRC, "it no longer exists"),
        (COPYENGINE_E_PATH_NOT_FOUND_DEST, "the destination folder no longer exists"),
        (COPYENGINE_E_SAME_FILE, "the source and destination are the same"),
        (COPYENGINE_E_DEST_SUBTREE, "the destination is inside the source folder"),
        (COPYENGINE_E_DEST_SAME_TREE, "the destination is inside the source folder"),
        (COPYENGINE_E_FLD_IS_FILE_DEST, "a file with that name is in the way"),
        (COPYENGINE_E_FILE_IS_FLD_DEST, "a folder with that name is in the way"),
        (COPYENGINE_E_FILE_TOO_LARGE, "the file is too large for the destination drive"),
        (COPYENGINE_E_RECYCLE_SIZE_TOO_BIG, "it is too big for the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_PATH_TOO_LONG, "its path is too long for the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, "this drive has no Recycle Bin"),
        (COPYENGINE_E_RECYCLE_FORCE_NUKE, "it can't be moved to the Recycle Bin"),
        (COPYENGINE_E_RECYCLE_UNKNOWN_ERROR, "it can't be moved to the Recycle Bin"),
        (COPYENGINE_E_CANNOT_MOVE_FROM_RECYCLE_BIN, "it can't be moved out of the Recycle Bin"),
    ];
    match known.iter().find(|(k, _)| *k == hr) {
        Some((_, m)) => m.to_string(),
        None => hr_text(&windows::core::Error::from_hresult(hr)),
    }
}

// ---------------------------------------------------------------------------
// Validation, labels
// ---------------------------------------------------------------------------

fn validate(op: &ShellOp) -> Res<()> {
    match op {
        ShellOp::Transfer { op, sources, dest, .. } => {
            if op != "copy" && op != "move" {
                return Err(format!("Unknown transfer “{op}”"));
            }
            if sources.is_empty() {
                return Err("Nothing to transfer".into());
            }
            let dk = paths::key(dest);
            for s in sources {
                let k = paths::key(s);
                if dk == k || dk.starts_with(&format!("{k}\\")) {
                    return Err(format!("Can't {op} “{}” into itself", file_name(s)));
                }
            }
            Ok(())
        }
        ShellOp::Rename { new_name, .. } => fileops::validate_name(new_name.trim()),
        ShellOp::NewFolder { name, .. } | ShellOp::NewFile { name, .. } => fileops::validate_name(name.trim()),
        ShellOp::Restore { items } | ShellOp::Purge { items } => {
            match items.iter().find(|p| !is_recycle_item(p)) {
                Some(p) => Err(format!("“{p}” isn't in the Recycle Bin")),
                None => Ok(()),
            }
        }
        ShellOp::Delete { .. } | ShellOp::MoveBack { .. } => Ok(()),
    }
}

fn not_in_bin(p: &str) -> String {
    let name = recycled_original(p).map(|o| file_name(&o)).unwrap_or_else(|_| file_name(p));
    format!("“{name}” is no longer in the Recycle Bin")
}

fn is_recycle_item(p: &str) -> bool {
    let k = paths::key(p);
    k.contains("\\$recycle.bin\\") && file_name(&k).starts_with("$r")
}

/// (job kind, label) in the style of `fileops.rs`.
fn describe(op: &ShellOp) -> (&'static str, String) {
    match op {
        ShellOp::Transfer { op, sources, dest, .. } => {
            let moving = op == "move";
            (
                if moving { "move" } else { "copy" },
                format!(
                    "{} {} to {}",
                    if moving { "Moving" } else { "Copying" },
                    quoted_or_count(sources, file_name),
                    file_name(dest)
                ),
            )
        }
        ShellOp::Delete { items, permanent } => (
            if *permanent { "delete" } else { "recycle" },
            format!("{} {}", if *permanent { "Deleting" } else { "Recycling" }, quoted_or_count(items, file_name)),
        ),
        ShellOp::Rename { path, new_name } => {
            ("rename", format!("Renaming “{}” to “{}”", file_name(path), new_name.trim()))
        }
        ShellOp::NewFolder { name, .. } => ("new", format!("Creating folder “{}”", name.trim())),
        ShellOp::NewFile { name, .. } => ("new", format!("Creating “{}”", name.trim())),
        ShellOp::MoveBack { pairs } => (
            "move",
            if pairs.len() == 1 {
                format!("Moving “{}” back", file_name(&pairs[0].0))
            } else {
                format!("Moving {} items back", pairs.len())
            },
        ),
        ShellOp::Restore { items } => (
            "restore",
            format!("Restoring {}", quoted_or_count(items, |p| recycled_original(p).map(|o| file_name(&o)).unwrap_or_else(|_| file_name(p)))),
        ),
        ShellOp::Purge { items } => (
            "delete",
            format!(
                "Permanently deleting {}",
                quoted_or_count(items, |p| recycled_original(p).map(|o| file_name(&o)).unwrap_or_else(|_| file_name(p)))
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// Recycle Bin items
// ---------------------------------------------------------------------------

/// Original full path of a Recycle Bin item (`…\$Recycle.Bin\<SID>\$Rxxxxxx.ext`),
/// read from its `$I` companion file, or else from the shell's displaced-item
/// properties.
pub fn recycled_original(item: &str) -> Res<String> {
    let item = paths::normalize(item);
    if let Some(info) = info_file(&item) {
        if let Some(p) = fs::read(paths::for_io(&info)).ok().and_then(|b| parse_info_file(&b)) {
            return Ok(paths::normalize(&p));
        }
    }
    displaced_original(&item).ok_or_else(|| format!("Can't tell where “{}” came from", file_name(&item)))
}

/// `…\$Rxxxxxx.ext` → `…\$Ixxxxxx.ext` (the companion holding the original path).
fn info_file(recycled: &str) -> Option<String> {
    let name = file_name(recycled);
    let dir = paths::parent(recycled)?;
    (name.len() > 2 && name.is_char_boundary(2) && name[..2].eq_ignore_ascii_case("$R"))
        .then(|| join(&dir, &format!("$I{}", &name[2..])))
}

/// The shell can't parse or create a path this long (MAX_PATH, counting the NUL), though
/// Ferret and the Recycle Bin can hold items there.
pub fn too_long_for_shell(p: &str) -> bool {
    p.encode_utf16().count() >= 260
}

/// Puts a recycled item back without the shell, for an original path too long for it: renames
/// the `$R` data to where it came from (the Recycle Bin is on the same drive) and removes its
/// `$I` record, which is what the Recycle Bin's own restore does. Recreates a missing folder and
/// keeps both (“ - Copy”) when the name is taken, as the shell's restore does. Returns the new path.
pub fn restore_direct(recycled: &str, original: &str) -> Res<String> {
    let dir = paths::parent(original).ok_or_else(|| format!("Can't restore to “{original}”"))?;
    let name = file_name(original);
    fs::create_dir_all(paths::for_io(&dir)).map_err(|e| format!("“{name}”: {e}"))?;
    let to = join(&dir, &fileops::unique_name(&dir, &name, true));
    fs::rename(paths::for_io(recycled), paths::for_io(&to)).map_err(|e| format!("“{name}”: {e}"))?;
    drop_info_files(&[recycled.to_string()]);
    Ok(to)
}

/// The copy engine moves/deletes only the `$R` item and leaves its `$I`
/// companion behind (also for the Recycle Bin's own shell items), so remove
/// the `$I` files of items that are gone. Best effort: an orphaned `$I` file
/// is ignored by the Recycle Bin.
fn drop_info_files(recycled: &[String]) {
    for r in recycled {
        if !exists(r) {
            if let Some(info) = info_file(r) {
                let _ = fs::remove_file(paths::for_io(&info));
            }
        }
    }
}

/// `$I` file: i64 version, i64 size, FILETIME deleted, then the original path —
/// v1 (Vista–8): 260 UTF-16 units; v2 (10+): u32 length (incl. NUL) + UTF-16.
fn parse_info_file(b: &[u8]) -> Option<String> {
    if b.len() < 28 {
        return None;
    }
    let version = i64::from_le_bytes(b[0..8].try_into().ok()?);
    let text = match version {
        1 => &b[24..b.len().min(24 + 520)],
        2 => {
            let n = u32::from_le_bytes(b[24..28].try_into().ok()?) as usize;
            &b[28..b.len().min(28 + n * 2)]
        }
        _ => return None,
    };
    let units: Vec<u16> =
        text.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16(&units).ok().filter(|s| !s.is_empty())
}

/// Fallback for a missing/unreadable `$I` file: the displaced-from property of
/// the Recycle Bin's own item (only items enumerated from the Recycle Bin folder
/// carry it; a parsing-name item for the same `$R` path doesn't).
fn displaced_original(item: &str) -> Option<String> {
    let _com = ComGuard::init();
    let want = paths::key(item);
    unsafe {
        let bin: IShellItem = SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None).ok()?;
        let items: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems).ok()?;
        let found = loop {
            let mut one = [None];
            let mut n = 0u32;
            if items.Next(&mut one, Some(&mut n)).is_err() || n == 0 {
                return None;
            }
            let it = one[0].take()?;
            if display_name(&it, SIGDN_DESKTOPABSOLUTEPARSING).is_some_and(|p| paths::key(&p) == want) {
                break it;
            }
        };
        let si: IShellItem2 = found.cast().ok()?;
        let key = PROPERTYKEY { fmtid: PSGUID_DISPLACED, pid: PID_DISPLACED_FROM };
        let parent = take_pwstr(si.GetString(&key).ok()?);
        let mut name = display_name(&found, SIGDN_PARENTRELATIVE)?;
        // The friendly name may hide a known extension; the `$R` name keeps it.
        if let Some(ext) = Path::new(item).extension().map(|e| format!(".{}", e.to_string_lossy())) {
            if !name.to_lowercase().ends_with(&ext.to_lowercase()) {
                name.push_str(&ext);
            }
        }
        (!parent.is_empty()).then(|| join(&paths::normalize(&parent), &name))
    }
}

// ---------------------------------------------------------------------------
// Probes (used by fileops.rs before doing any work)
// ---------------------------------------------------------------------------

fn access_denied(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(5) || (e.raw_os_error().is_none() && e.kind() == std::io::ErrorKind::PermissionDenied)
}

/// True if creating a file in `dir` is denied (so the operation needs
/// administrator rights). Creates and removes a uniquely named probe file.
/// Network/WSL paths always say false: elevation doesn't help there.
pub fn needs_admin_write(dir: &str) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    let dir = paths::normalize(dir);
    if dir.starts_with(r"\\") {
        return false;
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let probe = join(&dir, &format!(".ferret-probe-{}-{nanos:x}.tmp", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .attributes(FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_TEMPORARY)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&probe)
    {
        Ok(f) => {
            drop(f);
            let _ = fs::remove_file(&probe);
            false
        }
        Err(e) => access_denied(&e),
    }
}

/// True if the item can't be opened for deletion because of its permissions.
/// Read-only files and files open in another program don't count (those fail
/// differently and elevation wouldn't help).
pub fn needs_admin_delete(path: &str) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let p = paths::normalize(path);
    if p.starts_with(r"\\") {
        return false;
    }
    let io = paths::for_io(&p);
    let Ok(meta) = fs::symlink_metadata(&io) else { return false };
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if meta.is_dir() {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    match fs::OpenOptions::new().access_mode(DELETE).share_mode(FILE_SHARE_ALL).custom_flags(flags).open(&io) {
        Ok(_) => false,
        Err(e) => access_denied(&e),
    }
}

/// True if this process runs elevated (then "needs admin" means "denied even to admins").
pub fn is_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut e as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && e.TokenIsElevated != 0
    }
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Mode {
    #[default]
    Transfer,
    Delete,
    Rename,
    New,
    MoveBack,
    Restore,
    Purge,
}

#[derive(Debug)]
enum Verb {
    Copy,
    Move,
    Rename,
    NewFolder,
    NewFile,
    Delete,
}

#[derive(Debug)]
struct Action {
    verb: Verb,
    /// Item to act on (caller's path).
    src: Option<String>,
    /// Destination folder (copy/move/new).
    dest: Option<String>,
    /// Explicit new name.
    name: Option<String>,
    /// Destination folder may be missing (move back / restore): the engine creates it.
    dest_may_be_missing: bool,
}

struct Plan {
    mode: Mode,
    flags: FILEOPERATION_FLAGS,
    actions: Vec<Action>,
    /// Items that couldn't be queued (move back onto an occupied path).
    blocked: Vec<String>,
}

fn act(verb: Verb, src: Option<String>, dest: Option<String>, name: Option<String>) -> Action {
    Action { verb, src, dest, name, dest_may_be_missing: false }
}

fn plan(op: &ShellOp, report: &dyn Report) -> Res<Plan> {
    let mut actions = Vec::new();
    let mut blocked = Vec::new();
    let (mode, flags) = match op {
        ShellOp::Transfer { op, sources, dest, policy } => {
            let moving = op == "move";
            let dest = paths::normalize(dest);
            if !Path::new(&paths::for_io(&dest)).is_dir() {
                return Err(format!("{dest} is not a folder"));
            }
            for s in sources {
                let s = paths::normalize(s);
                let same_dir = paths::parent(&s).map(|p| paths::key(&p)) == Some(paths::key(&dest));
                if moving && same_dir {
                    continue; // moving onto itself: nothing to do
                }
                let name = file_name(&s);
                let mut new_name = None;
                if !moving && same_dir {
                    // Duplicate in place: "x - Copy.ext", like fileops.
                    new_name = Some(fileops::unique_name(&dest, &name, true));
                } else if *policy == Policy::Skip && exists(&join(&dest, &name)) {
                    report.skipped();
                    continue;
                }
                let verb = if moving { Verb::Move } else { Verb::Copy };
                actions.push(act(verb, Some(s), Some(dest.clone()), new_name));
            }
            let extra = match policy {
                Policy::Replace => FOF_NOCONFIRMATION,
                Policy::KeepBoth => FOF_RENAMEONCOLLISION | FOFX_PRESERVEFILEEXTENSIONS,
                Policy::Skip => FILEOPERATION_FLAGS(0),
            };
            (Mode::Transfer, extra)
        }
        ShellOp::Delete { items, permanent } => {
            for p in items {
                actions.push(act(Verb::Delete, Some(paths::normalize(p)), None, None));
            }
            let extra = if *permanent {
                FOF_NOCONFIRMATION
            } else {
                // WANTNUKEWARNING: if an item can't be recycled the shell asks
                // before deleting it for good instead of doing so silently.
                FOF_NOCONFIRMATION | FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_WANTNUKEWARNING
            };
            (Mode::Delete, extra)
        }
        ShellOp::Rename { path, new_name } => {
            let new_name = new_name.trim().to_string();
            let src = paths::normalize(path);
            let dir = paths::parent(&src).ok_or("Can't rename a drive")?;
            let dst = join(&dir, &new_name);
            if dst != src {
                // Case-only renames are allowed; anything else must not clobber.
                if paths::key(&dst) != paths::key(&src) && exists(&dst) {
                    return Err(format!("“{new_name}” already exists here"));
                }
                actions.push(act(Verb::Rename, Some(src), None, Some(new_name)));
            }
            (Mode::Rename, FILEOPERATION_FLAGS(0))
        }
        ShellOp::NewFolder { dir, name } | ShellOp::NewFile { dir, name } => {
            let dir = paths::normalize(dir);
            let name = fileops::unique_name(&dir, name.trim(), false);
            let verb = if matches!(op, ShellOp::NewFolder { .. }) { Verb::NewFolder } else { Verb::NewFile };
            actions.push(act(verb, None, Some(dir), Some(name)));
            (Mode::New, FOF_RENAMEONCOLLISION)
        }
        ShellOp::MoveBack { pairs } => {
            for (from, to) in pairs.iter().rev() {
                let (from, to) = (paths::normalize(from), paths::normalize(to));
                if !exists(&to) {
                    continue; // already gone (moved/deleted since)
                }
                if exists(&from) {
                    blocked.push(file_name(&from));
                    continue;
                }
                let Some(dir) = paths::parent(&from) else { continue };
                let mut a = act(Verb::Move, Some(to), Some(dir), Some(file_name(&from)));
                a.dest_may_be_missing = true;
                actions.push(a);
            }
            (Mode::MoveBack, FILEOPERATION_FLAGS(0))
        }
        ShellOp::Restore { items } => {
            for p in items {
                let p = paths::normalize(p);
                if !exists(&p) {
                    return Err(not_in_bin(&p));
                }
                let original = recycled_original(&p)?;
                let dir = paths::parent(&original).ok_or_else(|| format!("Can't restore to “{original}”"))?;
                let mut a = act(Verb::Move, Some(p), Some(dir), Some(file_name(&original)));
                a.dest_may_be_missing = true;
                actions.push(a);
            }
            (Mode::Restore, FOF_RENAMEONCOLLISION | FOFX_PRESERVEFILEEXTENSIONS)
        }
        ShellOp::Purge { items } => {
            for p in items {
                let p = paths::normalize(p);
                if !exists(&p) {
                    return Err(not_in_bin(&p));
                }
                actions.push(act(Verb::Delete, Some(p), None, None));
            }
            (Mode::Purge, FOF_NOCONFIRMATION)
        }
    };
    Ok(Plan { mode, flags, actions, blocked })
}

/// Bytes and files under `p` (best effort; unreadable folders count as empty).
fn walk_totals(p: &Path, bytes: &mut u64, files: &mut u64, report: &dyn Report) -> Res<()> {
    if report.cancelled() {
        return Err("Cancelled".into());
    }
    let Ok(m) = fs::symlink_metadata(p) else { return Ok(()) };
    if m.is_dir() {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                walk_totals(&e.path(), bytes, files, report)?;
            }
        }
    } else {
        *bytes += m.len();
        *files += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Progress sink
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Ev {
    Copy,
    Move,
    Rename,
    Delete,
    New,
}

#[derive(Default)]
struct State {
    /// Valid only while `perform` runs (cleared by `Detach` before it returns).
    report: Option<&'static dyn Report>,
    mode: Mode,
    /// key(shell path of each queued item) → the caller's path for it.
    top: HashMap<String, String>,
    /// Targets (keys) that already existed when the engine was about to write them.
    existed: HashSet<String>,
    /// Existing folders (keys) that a transfer merges into.
    merged: HashSet<String>,
    /// Failed items (key of the caller's path → error), in the order they failed. The
    /// engine fails an item first and retries it elevated after the UAC prompt, so a
    /// later success for the same item clears its entry.
    errors: Vec<(String, ItemError)>,
    /// Queued items (keys of the caller's paths) the engine reported as done.
    confirmed: HashSet<String>,
    /// Real byte total of a transfer; progress is the shell's fraction of it.
    bytes_total: u64,
    bytes_sent: u64,
}

type Shared = Arc<Mutex<State>>;

fn lock(s: &Shared) -> MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clears the report pointer when `perform` exits, however it exits.
struct Detach(Shared);

impl Drop for Detach {
    fn drop(&mut self) {
        lock(&self.0).report = None;
    }
}

#[implement(IFileOperationProgressSink)]
struct Sink {
    shared: Shared,
}

fn cancel_err() -> windows::core::Error {
    windows::core::Error::from_hresult(E_CANCELLED)
}

impl Sink {
    fn pre(&self, ev: Ev, item: Option<&IShellItem>, dest: Option<&IShellItem>, name: &PCWSTR) -> windows::core::Result<()> {
        let src = item.and_then(item_path);
        let dest = dest.and_then(item_path);
        let new_name = pcwstr(name);
        let mut st = lock(&self.shared);
        let Some(rep) = st.report else { return Ok(()) };
        if rep.wants_trace() {
            rep.trace(&format!("pre {ev:?} src={src:?} dest={dest:?} name={new_name:?}"));
        }
        // An error from a Pre* callback fails this item with ERROR_CANCELLED, and
        // FOFX_EARLYFAILURE then stops the whole operation.
        if rep.cancelled() {
            return Err(cancel_err());
        }
        let shown = new_name.clone().or_else(|| src.as_deref().map(file_name)).unwrap_or_default();
        rep.current(&shown);
        if matches!(ev, Ev::Copy | Ev::Move) {
            if let Some(d) = dest {
                let target = join(&d, &shown);
                if exists(&target) {
                    st.existed.insert(paths::key(&target));
                }
            }
        }
        Ok(())
    }

    fn post(
        &self,
        ev: Ev,
        item: Option<&IShellItem>,
        dest: Option<&IShellItem>,
        name: &PCWSTR,
        hr: HRESULT,
        new: Option<&IShellItem>,
    ) -> windows::core::Result<()> {
        let src = item.and_then(item_path);
        let dest = dest.and_then(item_path);
        let new = new.and_then(item_path);
        let new_name = pcwstr(name);
        let mut st = lock(&self.shared);
        let Some(rep) = st.report else { return Ok(()) };
        if rep.wants_trace() {
            rep.trace(&format!("post {ev:?} hr={:#010x} src={src:?} dest={dest:?} name={new_name:?} new={new:?}", hr.0 as u32));
        }
        // Only items we queued are in `top`; the engine also calls back for
        // everything inside a copied (or merged) folder.
        let top = src.as_ref().and_then(|s| st.top.get(&paths::key(s)).cloned());
        let key = paths::key(top.as_deref().or(src.as_deref()).unwrap_or_default());
        if hr.is_err() {
            if !st.errors.iter().any(|(k, _)| *k == key) {
                let what = new_name
                    .clone()
                    .or_else(|| top.as_deref().or(src.as_deref()).map(file_name))
                    .unwrap_or_default();
                let verb = match (st.mode, ev) {
                    (Mode::Restore, _) => "restore",
                    (Mode::Purge, _) => "delete",
                    (_, Ev::Copy) => "copy",
                    (_, Ev::Move) => "move",
                    (_, Ev::Rename) => "rename",
                    (_, Ev::Delete) => "delete",
                    (_, Ev::New) => "create",
                };
                st.errors.push((key, ItemError { hr, verb, what }));
            }
            return Ok(());
        }
        // A retry succeeded (e.g. elevated, after the UAC prompt): forget the earlier failure.
        st.errors.retain(|(k, _)| *k != key);
        if top.is_some() {
            st.confirmed.insert(key);
        }
        match st.mode {
            Mode::Transfer => {
                if !matches!(ev, Ev::Copy | Ev::Move) {
                    return Ok(());
                }
                let Some(new) = new else { return Ok(()) };
                let is_dir = Path::new(&paths::for_io(&new)).is_dir();
                // files_total is the walked file count when there are bytes, else the item count.
                if (st.bytes_total > 0 && !is_dir) || (st.bytes_total == 0 && top.is_some()) {
                    rep.file_done();
                }
                let in_merge = dest.as_ref().is_some_and(|d| st.merged.contains(&paths::key(d)));
                if top.is_none() && !in_merge {
                    return Ok(()); // inside a brand-new folder: only the folder itself is reported
                }
                let from = top.or(src).unwrap_or_default();
                let nk = paths::key(&new);
                if st.existed.contains(&nk) {
                    if is_dir {
                        st.merged.insert(nk); // merged into an existing folder: report what's new inside
                    } else {
                        rep.pair(&from, &new); // overwritten in place: nothing to delete on undo
                    }
                } else {
                    rep.created(&new);
                    rep.pair(&from, &new);
                }
            }
            Mode::Delete | Mode::Purge => {
                if let Some(top) = top {
                    // Recycled: `new` is the item in the Recycle Bin (for Restore).
                    if let Some(new) = new {
                        rep.pair(&top, &new);
                    }
                    rep.file_done();
                }
            }
            Mode::Rename | Mode::MoveBack | Mode::Restore => {
                if let (Some(top), Some(new)) = (top, new) {
                    rep.created(&new);
                    rep.pair(&top, &new);
                    rep.file_done();
                }
            }
            Mode::New => {
                if let Some(new) = new {
                    rep.created(&new);
                    rep.file_done();
                }
            }
        }
        Ok(())
    }

    fn progress(&self, total: u32, sofar: u32) -> windows::core::Result<()> {
        let mut st = lock(&self.shared);
        let Some(rep) = st.report else { return Ok(()) };
        if rep.wants_trace() {
            rep.trace(&format!("progress {sofar}/{total}"));
        }
        // An error here stops the engine mid-file (it removes the partial copy);
        // the item fails with COPYENGINE_E_USER_CANCELLED.
        if rep.cancelled() {
            return Err(cancel_err());
        }
        // The shell's work units aren't bytes (≈32 KB per unit plus a little per
        // item, and the total grows as it discovers items), so report its
        // fraction of the real byte total, never going backwards.
        if st.bytes_total > 0 && total > 0 {
            let target = (st.bytes_total as u128 * sofar.min(total) as u128 / total as u128) as u64;
            if target > st.bytes_sent {
                rep.add_bytes(target - st.bytes_sent);
                st.bytes_sent = target;
            }
        }
        Ok(())
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _hr: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _f: u32, item: Ref<IShellItem>, name: &PCWSTR) -> windows::core::Result<()> {
        self.pre(Ev::Rename, item.as_ref(), None, name)
    }
    fn PostRenameItem(&self, _f: u32, item: Ref<IShellItem>, name: &PCWSTR, hr: HRESULT, new: Ref<IShellItem>) -> windows::core::Result<()> {
        self.post(Ev::Rename, item.as_ref(), None, name, hr, new.as_ref())
    }
    fn PreMoveItem(&self, _f: u32, item: Ref<IShellItem>, dest: Ref<IShellItem>, name: &PCWSTR) -> windows::core::Result<()> {
        self.pre(Ev::Move, item.as_ref(), dest.as_ref(), name)
    }
    fn PostMoveItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        dest: Ref<IShellItem>,
        name: &PCWSTR,
        hr: HRESULT,
        new: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(Ev::Move, item.as_ref(), dest.as_ref(), name, hr, new.as_ref())
    }
    fn PreCopyItem(&self, _f: u32, item: Ref<IShellItem>, dest: Ref<IShellItem>, name: &PCWSTR) -> windows::core::Result<()> {
        self.pre(Ev::Copy, item.as_ref(), dest.as_ref(), name)
    }
    fn PostCopyItem(
        &self,
        _f: u32,
        item: Ref<IShellItem>,
        dest: Ref<IShellItem>,
        name: &PCWSTR,
        hr: HRESULT,
        new: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(Ev::Copy, item.as_ref(), dest.as_ref(), name, hr, new.as_ref())
    }
    fn PreDeleteItem(&self, _f: u32, item: Ref<IShellItem>) -> windows::core::Result<()> {
        self.pre(Ev::Delete, item.as_ref(), None, &PCWSTR::null())
    }
    fn PostDeleteItem(&self, _f: u32, item: Ref<IShellItem>, hr: HRESULT, new: Ref<IShellItem>) -> windows::core::Result<()> {
        self.post(Ev::Delete, item.as_ref(), None, &PCWSTR::null(), hr, new.as_ref())
    }
    fn PreNewItem(&self, _f: u32, dest: Ref<IShellItem>, name: &PCWSTR) -> windows::core::Result<()> {
        self.pre(Ev::New, None, dest.as_ref(), name)
    }
    fn PostNewItem(
        &self,
        _f: u32,
        dest: Ref<IShellItem>,
        name: &PCWSTR,
        _template: &PCWSTR,
        _attrs: u32,
        hr: HRESULT,
        new: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        self.post(Ev::New, None, dest.as_ref(), name, hr, new.as_ref())
    }
    fn UpdateProgress(&self, total: u32, sofar: u32) -> windows::core::Result<()> {
        self.progress(total, sofar)
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Core
// ---------------------------------------------------------------------------

/// Runs `op` through `IFileOperation` and reports into `report`. Returns Err
/// on failure or cancellation (after `report.mark_cancelled()` when the user
/// declined something in shell UI, e.g. the UAC prompt).
pub fn perform(op: &ShellOp, owner: Option<HWND>, elevate: bool, report: &dyn Report) -> Res<()> {
    validate(op)?;
    let owner = owner.map(|h| h.0 as isize);
    // A fresh thread guarantees a clean single-threaded apartment.
    std::thread::scope(|s| {
        s.spawn(|| {
            let _com = ComGuard::init();
            run(op, owner.map(hwnd_from_raw), elevate, report)
        })
        .join()
        .unwrap_or_else(|_| Err("Internal error".into()))
    })
}

fn run(op: &ShellOp, owner: Option<HWND>, elevate: bool, report: &dyn Report) -> Res<()> {
    let mut plan = plan(op, report)?;
    let n = plan.actions.len() as u64;

    // Totals: real bytes for copy/move (the shell's own units aren't bytes), else items.
    let mut bytes_total = 0;
    if plan.mode == Mode::Transfer {
        let mut files = 0;
        for a in &plan.actions {
            if let Some(s) = &a.src {
                walk_totals(Path::new(&paths::for_io(s)), &mut bytes_total, &mut files, report)?;
            }
        }
        report.totals(bytes_total, if bytes_total > 0 { files } else { n });
    } else {
        report.totals(0, n);
    }
    if plan.mode == Mode::Restore {
        restore_long(&mut plan, report)?;
    }
    if plan.actions.is_empty() {
        return blocked_result(&plan);
    }

    let mut flags = FOF_SILENT | FOF_NOCONFIRMMKDIR | FOF_NOERRORUI | FOFX_EARLYFAILURE | plan.flags;
    if elevate {
        // FOF_NOERRORUI would suppress the elevation prompt; REQUIREELEVATION skips
        // the shell's own "You'll need to provide administrator permission" step
        // (the UI already asked). The UAC consent prompt still appears.
        flags = flags | FOFX_SHOWELEVATIONPROMPT | FOFX_REQUIREELEVATION;
    }

    let fo: IFileOperation =
        unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }.map_err(|e| format!("Shell file operations unavailable: {}", hr_text(&e)))?;
    let shared: Shared = Arc::new(Mutex::new(State { mode: plan.mode, bytes_total, ..Default::default() }));
    // SAFETY: the sink only uses `report` while `State::report` is Some, and
    // `Detach` clears it (under the same lock) before this function returns.
    let rep: &'static dyn Report = unsafe { std::mem::transmute::<&dyn Report, &'static dyn Report>(report) };
    lock(&shared).report = Some(rep);
    let _detach = Detach(shared.clone());

    unsafe {
        if let Some(h) = owner {
            let _ = fo.SetOwnerWindow(h);
        }
        fo.SetOperationFlags(flags).map_err(|e| hr_text(&e))?;
    }
    let sink: IFileOperationProgressSink = Sink { shared: shared.clone() }.into();
    let cookie = unsafe { fo.Advise(&sink) }.map_err(|e| hr_text(&e))?;

    let queued = queue(&fo, &plan, &shared);
    let result = match queued {
        Ok(()) => {
            let hr = unsafe { fo.PerformOperations() };
            let aborted = unsafe { fo.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
            Ok((hr, aborted))
        }
        Err(e) => Err(e),
    };
    unsafe {
        let _ = fo.Unadvise(cookie);
    }
    drop(_detach);
    if matches!(plan.mode, Mode::Restore | Mode::Purge) {
        drop_info_files(&plan.actions.iter().filter_map(|a| a.src.clone()).collect::<Vec<_>>());
    }

    let (hr, aborted) = result?;
    if hr.is_ok() && !aborted {
        confirm_on_disk(&plan, &shared, report);
    }
    let first_error = lock(&shared).errors.first().map(|(_, e)| e.clone());
    let failed = hr.err().map(|e| e.code());
    if report.wants_trace() {
        report.trace(&format!("done hr={:?} aborted={aborted}", failed.map(|h| format!("{:#010x}", h.0 as u32))));
    }
    // A denial without elevation becomes an "elevate:" error (the convention
    // fileops uses), so the UI can offer to retry with `elevate = true`.
    let offer_elevation = !elevate && !is_elevated();
    match conclude(report.cancelled(), failed, aborted, first_error, offer_elevation) {
        Outcome::Done => blocked_result(&plan),
        Outcome::Cancelled => Err("Cancelled".into()),
        Outcome::Declined => {
            report.mark_cancelled();
            Err("Cancelled".into())
        }
        Outcome::Failed(msg) => Err(msg),
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Done,
    /// Cancelled through `Report::cancelled` (the job's Cancel button).
    Cancelled,
    /// Declined in shell UI: the UAC prompt, a "delete permanently?" warning…
    Declined,
    Failed(String),
}

/// The first item the engine reported as failed.
#[derive(Clone, Debug)]
struct ItemError {
    hr: HRESULT,
    verb: &'static str,
    what: String,
}

impl ItemError {
    fn message(&self, offer_elevation: bool) -> String {
        if offer_elevation && needs_elevation(self.hr) {
            format!("elevate:You'll need administrator permission to {} “{}”", self.verb, self.what)
        } else {
            format!("Couldn't {} “{}”: {}", self.verb, self.what, reason(self.hr))
        }
    }
}

/// Failures that administrator rights may fix.
fn needs_elevation(hr: HRESULT) -> bool {
    hr == COPYENGINE_E_REQUIRES_ELEVATION
        || hr == COPYENGINE_E_ACCESS_DENIED_DEST
        || hr == COPYENGINE_E_ACCESS_DENIED_SRC
        || hr == HRESULT(0x8007_0005_u32 as i32)
}

/// What `PerformOperations` (its error, `GetAnyOperationsAborted`, and the first
/// failing item from the sink) amounts to.
fn conclude(
    we_cancelled: bool,
    failed: Option<HRESULT>,
    aborted: bool,
    first_error: Option<ItemError>,
    offer_elevation: bool,
) -> Outcome {
    if we_cancelled {
        return Outcome::Cancelled;
    }
    let cancel_code = failed.is_some_and(is_cancel_code) || first_error.as_ref().is_some_and(|e| is_cancel_code(e.hr));
    // Aborted with no failing item and no error: the user said no to something.
    if cancel_code || (aborted && first_error.is_none() && failed.is_none()) {
        return Outcome::Declined;
    }
    match (first_error, failed) {
        (Some(e), _) => Outcome::Failed(e.message(offer_elevation)),
        (None, Some(hr)) => {
            let r = reason(hr);
            let mut c = r.chars();
            Outcome::Failed(c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or(r))
        }
        (None, None) => Outcome::Done,
    }
}

/// When the engine says the whole operation succeeded but an item's only report was a
/// failure (its elevated retry's callbacks didn't reach us), check the result on disk:
/// if the item is where it should be, it succeeded, so report it and drop the error.
fn confirm_on_disk(plan: &Plan, shared: &Shared, report: &dyn Report) {
    let mut st = lock(shared);
    if st.errors.is_empty() {
        return;
    }
    for a in &plan.actions {
        let Some(src) = &a.src else { continue };
        let key = paths::key(src);
        if st.confirmed.contains(&key) || !st.errors.iter().any(|(k, _)| *k == key) {
            continue;
        }
        let target = match a.verb {
            Verb::Delete => None,
            Verb::Rename => paths::parent(src).map(|d| join(&d, a.name.as_deref().unwrap_or_default())),
            _ => a.dest.as_ref().map(|d| join(d, &a.name.clone().unwrap_or_else(|| file_name(src)))),
        };
        let done = match (&a.verb, &target) {
            (Verb::Delete, _) => !exists(src),
            (Verb::Copy, Some(t)) => exists(t),
            (Verb::Move | Verb::Rename, Some(t)) => exists(t) && (!exists(src) || paths::key(t) == key),
            _ => false,
        };
        if !done {
            continue;
        }
        st.errors.retain(|(k, _)| *k != key);
        if let Some(t) = target {
            if !st.existed.contains(&paths::key(&t)) {
                report.created(&t);
            }
            report.pair(src, &t);
        }
        report.file_done();
    }
}

/// Restores the items whose original path is too long for the shell directly, and leaves the
/// rest of the plan to it.
fn restore_long(plan: &mut Plan, report: &dyn Report) -> Res<()> {
    let target = |a: &Action| join(a.dest.as_deref().unwrap_or_default(), a.name.as_deref().unwrap_or_default());
    let (long, rest): (Vec<Action>, Vec<Action>) = std::mem::take(&mut plan.actions).into_iter().partition(|a| too_long_for_shell(&target(a)));
    plan.actions = rest;
    for a in &long {
        let src = a.src.clone().unwrap_or_default();
        let to = restore_direct(&src, &target(a))?;
        report.created(&to);
        report.pair(&src, &to);
        report.file_done();
    }
    Ok(())
}

fn blocked_result(plan: &Plan) -> Res<()> {
    if plan.blocked.is_empty() {
        Ok(())
    } else {
        Err(format!("Something with the same name is back at the original location: {}", plan.blocked.join(", ")))
    }
}

fn queue(fo: &IFileOperation, plan: &Plan, shared: &Shared) -> Res<()> {
    for a in &plan.actions {
        let name = a.name.as_deref().map(HSTRING::from);
        let pname = name.as_ref().map(|h| PCWSTR(h.as_ptr())).unwrap_or(PCWSTR::null());
        let src = match &a.src {
            Some(s) => {
                let item = shell_item(s)?;
                if let Some(p) = item_path(&item) {
                    lock(shared).top.insert(paths::key(&p), s.clone());
                }
                Some(item)
            }
            None => None,
        };
        let dest = match &a.dest {
            Some(d) => Some(folder_item(d, a.dest_may_be_missing)?),
            None => None,
        };
        let r = unsafe {
            match (&a.verb, &src, &dest) {
                (Verb::Copy, Some(s), Some(d)) => fo.CopyItem(s, d, pname, None),
                (Verb::Move, Some(s), Some(d)) => fo.MoveItem(s, d, pname, None),
                (Verb::Rename, Some(s), _) => fo.RenameItem(s, pname, None),
                (Verb::Delete, Some(s), _) => fo.DeleteItem(s, None),
                (Verb::NewFolder, _, Some(d)) => fo.NewItem(d, NEW_FOLDER_ATTRIBUTES, pname, PCWSTR::null(), None),
                (Verb::NewFile, _, Some(d)) => fo.NewItem(d, NEW_FILE_ATTRIBUTES, pname, PCWSTR::null(), None),
                _ => return Err(format!("Bad action {a:?}")),
            }
        };
        r.map_err(|e| format!("“{}”: {}", a.src.as_deref().map(file_name).or(a.name.clone()).unwrap_or_default(), hr_text(&e)))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (real files under %TEMP%\ferret-shellops-test-*; Recycle Bin tests only
// touch items they recycled themselves). SHELLOPS_TRACE=1 prints every callback.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default, Debug, Clone)]
    struct Got {
        created: Vec<String>,
        pairs: Vec<(String, String)>,
        skipped: u64,
        files_done: u64,
        bytes_done: u64,
        totals: (u64, u64),
        marked_cancelled: bool,
        trace: Vec<String>,
    }

    #[derive(Default)]
    struct Collect {
        got: Mutex<Got>,
        cancel: AtomicBool,
        /// Cancel once this many trace lines starting with the prefix were seen.
        cancel_on: Option<(&'static str, usize)>,
        seen: AtomicUsize,
    }

    impl Report for Collect {
        fn cancelled(&self) -> bool {
            self.cancel.load(Ordering::SeqCst)
        }
        fn mark_cancelled(&self) {
            self.got.lock().unwrap().marked_cancelled = true;
            self.cancel.store(true, Ordering::SeqCst);
        }
        fn totals(&self, bytes: u64, files: u64) {
            self.got.lock().unwrap().totals = (bytes, files);
        }
        fn current(&self, _name: &str) {}
        fn add_bytes(&self, n: u64) {
            self.got.lock().unwrap().bytes_done += n;
        }
        fn file_done(&self) {
            self.got.lock().unwrap().files_done += 1;
        }
        fn created(&self, path: &str) {
            self.got.lock().unwrap().created.push(path.to_string());
        }
        fn pair(&self, from: &str, to: &str) {
            self.got.lock().unwrap().pairs.push((from.to_string(), to.to_string()));
        }
        fn skipped(&self) {
            self.got.lock().unwrap().skipped += 1;
        }
        fn wants_trace(&self) -> bool {
            true
        }
        fn trace(&self, line: &str) {
            if let Some((prefix, n)) = self.cancel_on {
                if line.starts_with(prefix) && self.seen.fetch_add(1, Ordering::SeqCst) + 1 >= n {
                    self.cancel.store(true, Ordering::SeqCst);
                }
            }
            self.got.lock().unwrap().trace.push(line.to_string());
        }
    }

    fn temp_base() -> String {
        paths::long_form(&std::env::temp_dir().to_string_lossy())
    }

    /// Shortens test paths for printing.
    fn short(s: &str) -> String {
        s.replace(&temp_base(), "%TEMP%")
    }

    fn run_op(op: ShellOp) -> (Res<()>, Got) {
        run_with(op, Collect::default(), false)
    }

    fn run_with(op: ShellOp, c: Collect, elevate: bool) -> (Res<()>, Got) {
        let r = perform(&op, None, elevate, &c);
        let got = c.got.lock().unwrap().clone();
        let kind = format!("{op:?}");
        println!("--- {}", short(kind.split(" {").next().unwrap_or(&kind)));
        println!("    result:   {r:?}");
        println!("    created:  {:?}", got.created.iter().map(|p| short(p)).collect::<Vec<_>>());
        println!("    pairs:    {:?}", got.pairs.iter().map(|(a, b)| (short(a), short(b))).collect::<Vec<_>>());
        println!(
            "    progress: files {}/{} bytes {}/{} skipped {}",
            got.files_done, got.totals.1, got.bytes_done, got.totals.0, got.skipped
        );
        if std::env::var_os("SHELLOPS_TRACE").is_some() {
            for t in &got.trace {
                println!("      {}", short(t));
            }
        }
        (r, got)
    }

    struct TempDir(String);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = join(&temp_base(), &format!("ferret-shellops-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(paths::normalize(&p))
        }
        fn p(&self, rel: &str) -> String {
            join(&self.0, rel)
        }
        fn write(&self, rel: &str, content: &str) -> String {
            let p = self.p(rel);
            if let Some(d) = Path::new(&p).parent() {
                fs::create_dir_all(d).unwrap();
            }
            fs::write(&p, content).unwrap();
            p
        }
        fn mkdir(&self, rel: &str) -> String {
            let p = self.p(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Purges, at the end of a test, any Recycle Bin item the test itself
    /// recycled and didn't restore/purge (e.g. after a failed assertion).
    #[derive(Default)]
    struct BinGuard(Mutex<Vec<String>>);

    impl BinGuard {
        fn track(&self, recycled: &str) -> String {
            assert!(is_recycle_item(recycled), "not a Recycle Bin path: {recycled}");
            self.0.lock().unwrap().push(recycled.to_string());
            recycled.to_string()
        }
    }

    impl Drop for BinGuard {
        fn drop(&mut self) {
            let left: Vec<String> = self.0.lock().unwrap().iter().filter(|p| exists(p)).cloned().collect();
            if !left.is_empty() {
                println!("    (purging leftover test items {left:?})");
                let _ = perform(&ShellOp::Purge { items: left }, None, false, &Collect::default());
            }
        }
    }

    fn read(p: &str) -> String {
        fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    fn ls(dir: &str) -> Vec<String> {
        let mut v: Vec<String> =
            fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn keys(v: &[String]) -> Vec<String> {
        let mut k: Vec<String> = v.iter().map(|p| paths::key(p)).collect();
        k.sort();
        k
    }

    fn pair_keys(v: &[(String, String)]) -> Vec<(String, String)> {
        let mut k: Vec<(String, String)> = v.iter().map(|(a, b)| (paths::key(a), paths::key(b))).collect();
        k.sort();
        k
    }

    fn same(got: &[String], want: &[String]) {
        assert_eq!(keys(got), keys(want));
    }

    fn same_pairs(got: &[(String, String)], want: &[(String, String)]) {
        assert_eq!(pair_keys(got), pair_keys(want));
    }

    fn transfer(op: &str, sources: Vec<String>, dest: &str, policy: Policy) -> ShellOp {
        ShellOp::Transfer { op: op.into(), sources, dest: dest.into(), policy }
    }

    /// Recycle Bin entries whose original path is `orig` (read-only listing).
    fn in_bin(orig: &str) -> Vec<trash::TrashItem> {
        let k = paths::key(orig);
        trash::os_limited::list()
            .unwrap()
            .into_iter()
            .filter(|i| paths::key(&i.original_path().to_string_lossy()) == k)
            .collect()
    }

    // ----- copy -----

    #[test]
    fn copy_keep_both() {
        let t = TempDir::new("copy-keepboth");
        let a = t.write("src\\a.txt", "new a");
        let folder = t.mkdir("src\\folder");
        t.write("src\\folder\\inner.txt", "new inner");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");
        t.write("dest\\folder\\inner.txt", "old inner");

        let (r, g) = run_op(transfer("copy", vec![a.clone(), folder.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        let (a2, f2) = (t.p("dest\\a - Copy.txt"), t.p("dest\\folder - Copy"));
        same(&g.created, &[a2.clone(), f2.clone()]);
        same_pairs(&g.pairs, &[(a.clone(), a2.clone()), (folder.clone(), f2.clone())]);
        assert_eq!(read(&a2), "new a");
        assert_eq!(read(&t.p("dest\\a.txt")), "old a");
        assert_eq!(read(&t.p("dest\\folder - Copy\\inner.txt")), "new inner");
        assert_eq!(read(&t.p("dest\\folder\\inner.txt")), "old inner");
        assert_eq!((g.files_done, g.totals.1), (2, 2));
        assert_eq!(g.bytes_done, g.totals.0);

        // Next collision.
        let (r, g) = run_op(transfer("copy", vec![a.clone(), folder.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        same(&g.created, &[t.p("dest\\a - Copy (2).txt"), t.p("dest\\folder - Copy (2)")]);
    }

    #[test]
    fn copy_replace_overwrites_and_merges() {
        let t = TempDir::new("copy-replace");
        let a = t.write("src\\a.txt", "new a");
        let folder = t.mkdir("src\\folder");
        let inner = t.write("src\\folder\\inner.txt", "new inner");
        let extra = t.write("src\\folder\\extra.txt", "extra");
        let sub = t.mkdir("src\\folder\\sub");
        t.write("src\\folder\\sub\\deep.txt", "deep");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");
        t.write("dest\\folder\\inner.txt", "old inner");
        t.write("dest\\folder\\keep.txt", "keep");

        let (r, g) = run_op(transfer("copy", vec![a.clone(), folder.clone()], &dest, Policy::Replace));
        r.unwrap();
        // Only what didn't exist before is "created" (undo deletes those); overwritten
        // files only get a pair; the merged folder itself is neither.
        same(&g.created, &[t.p("dest\\folder\\extra.txt"), t.p("dest\\folder\\sub")]);
        same_pairs(
            &g.pairs,
            &[
                (a.clone(), t.p("dest\\a.txt")),
                (inner.clone(), t.p("dest\\folder\\inner.txt")),
                (extra.clone(), t.p("dest\\folder\\extra.txt")),
                (sub.clone(), t.p("dest\\folder\\sub")),
            ],
        );
        assert_eq!(read(&t.p("dest\\a.txt")), "new a");
        assert_eq!(read(&t.p("dest\\folder\\inner.txt")), "new inner");
        assert_eq!(read(&t.p("dest\\folder\\keep.txt")), "keep");
        assert_eq!(read(&t.p("dest\\folder\\sub\\deep.txt")), "deep");
        assert_eq!((g.files_done, g.totals.1), (4, 4));
    }

    #[test]
    fn copy_skip() {
        let t = TempDir::new("copy-skip");
        let a = t.write("src\\a.txt", "new a");
        let b = t.write("src\\b.txt", "b");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");

        let (r, g) = run_op(transfer("copy", vec![a.clone(), b.clone()], &dest, Policy::Skip));
        r.unwrap();
        assert_eq!(g.skipped, 1);
        same(&g.created, &[t.p("dest\\b.txt")]);
        same_pairs(&g.pairs, &[(b.clone(), t.p("dest\\b.txt"))]);
        assert_eq!(read(&t.p("dest\\a.txt")), "old a");

        // Everything already there: nothing queued.
        let (r, g) = run_op(transfer("copy", vec![a, b], &dest, Policy::Skip));
        r.unwrap();
        assert_eq!((g.skipped, g.created.len()), (2, 0));
    }

    #[test]
    fn copy_into_same_folder_duplicates() {
        let t = TempDir::new("copy-dup");
        let a = t.write("a.txt", "a");
        let (r, g) = run_op(transfer("copy", vec![a.clone()], &t.0, Policy::Replace));
        r.unwrap();
        same(&g.created, &[t.p("a - Copy.txt")]);
        let (r, g) = run_op(transfer("copy", vec![a.clone()], &t.0, Policy::KeepBoth));
        r.unwrap();
        same(&g.created, &[t.p("a - Copy (2).txt")]);
        assert_eq!(read(&a), "a");
    }

    #[test]
    fn short_name_inputs_report_callers_paths() {
        // %TEMP% as given by the environment uses an 8.3 name here (MELTIN~1); the
        // shell reports long paths, sources keep the caller's spelling.
        let t = TempDir::new("shortnames");
        t.write("src\\a.txt", "a");
        let dest = t.mkdir("dest");
        let short_base = paths::normalize(&std::env::temp_dir().to_string_lossy());
        let short_src = t.p("src\\a.txt").replace(&temp_base(), &short_base);
        println!("    source as given: {short_src}");
        let (r, g) = run_op(transfer("copy", vec![short_src.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        assert_eq!(g.pairs, vec![(short_src, t.p("dest\\a.txt"))]);
    }

    // ----- move -----

    #[test]
    fn move_keep_both() {
        let t = TempDir::new("move-keepboth");
        let a = t.write("src\\a.txt", "new a");
        let folder = t.mkdir("src\\folder");
        t.write("src\\folder\\inner.txt", "new inner");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");
        t.write("dest\\folder\\inner.txt", "old inner");

        let (r, g) = run_op(transfer("move", vec![a.clone(), folder.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        let (a2, f2) = (t.p("dest\\a - Copy.txt"), t.p("dest\\folder - Copy"));
        same(&g.created, &[a2.clone(), f2.clone()]);
        same_pairs(&g.pairs, &[(a.clone(), a2.clone()), (folder.clone(), f2.clone())]);
        assert!(!exists(&a) && !exists(&folder));
        assert_eq!(read(&a2), "new a");
        assert_eq!(read(&t.p("dest\\a.txt")), "old a");
        assert_eq!(read(&t.p("dest\\folder - Copy\\inner.txt")), "new inner");
    }

    #[test]
    fn move_replace_overwrites_and_merges() {
        let t = TempDir::new("move-replace");
        let a = t.write("src\\a.txt", "new a");
        let folder = t.mkdir("src\\folder");
        let inner = t.write("src\\folder\\inner.txt", "new inner");
        let extra = t.write("src\\folder\\extra.txt", "extra");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");
        t.write("dest\\folder\\inner.txt", "old inner");
        t.write("dest\\folder\\keep.txt", "keep");

        let (r, g) = run_op(transfer("move", vec![a.clone(), folder.clone()], &dest, Policy::Replace));
        r.unwrap();
        same(&g.created, &[t.p("dest\\folder\\extra.txt")]);
        same_pairs(
            &g.pairs,
            &[
                (a.clone(), t.p("dest\\a.txt")),
                (inner.clone(), t.p("dest\\folder\\inner.txt")),
                (extra.clone(), t.p("dest\\folder\\extra.txt")),
            ],
        );
        assert!(!exists(&a) && !exists(&folder), "sources should be gone: {:?}", ls(&t.p("src")));
        assert_eq!(read(&t.p("dest\\a.txt")), "new a");
        assert_eq!(read(&t.p("dest\\folder\\inner.txt")), "new inner");
        assert_eq!(read(&t.p("dest\\folder\\keep.txt")), "keep");
    }

    #[test]
    fn move_skip() {
        let t = TempDir::new("move-skip");
        let a = t.write("src\\a.txt", "new a");
        let b = t.write("src\\b.txt", "b");
        let dest = t.mkdir("dest");
        t.write("dest\\a.txt", "old a");

        let (r, g) = run_op(transfer("move", vec![a.clone(), b.clone()], &dest, Policy::Skip));
        r.unwrap();
        assert_eq!(g.skipped, 1);
        same(&g.created, &[t.p("dest\\b.txt")]);
        same_pairs(&g.pairs, &[(b.clone(), t.p("dest\\b.txt"))]);
        assert_eq!(read(&a), "new a");
        assert_eq!(read(&t.p("dest\\a.txt")), "old a");
        assert!(!exists(&b));
    }

    #[test]
    fn move_back_restores_moved_items() {
        let t = TempDir::new("move-back");
        let a = t.write("src\\a.txt", "a");
        let folder = t.mkdir("src\\folder");
        t.write("src\\folder\\inner.txt", "inner");
        let dest = t.mkdir("dest");
        let (r, g) = run_op(transfer("move", vec![a.clone(), folder.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        let moved = g.pairs.clone();
        assert_eq!(moved.len(), 2);
        // The original parent is gone too: move back recreates it.
        fs::remove_dir(t.p("src")).unwrap();

        let (r, g) = run_op(ShellOp::MoveBack { pairs: moved.clone() });
        r.unwrap();
        same(&g.created, &[a.clone(), folder.clone()]);
        same_pairs(&g.pairs, &moved.iter().map(|(f, t)| (t.clone(), f.clone())).collect::<Vec<_>>());
        assert_eq!(read(&a), "a");
        assert_eq!(read(&t.p("src\\folder\\inner.txt")), "inner");
        assert!(ls(&dest).is_empty());

        // Blocked: something new sits at the original path.
        let (r, g) = run_op(transfer("move", vec![a.clone()], &dest, Policy::KeepBoth));
        r.unwrap();
        t.write("src\\a.txt", "newcomer");
        let (r, _) = run_op(ShellOp::MoveBack { pairs: g.pairs.clone() });
        assert!(r.unwrap_err().contains("same name is back"));
        assert_eq!(read(&a), "newcomer");
        assert_eq!(read(&t.p("dest\\a.txt")), "a");
    }

    // ----- rename / new -----

    #[test]
    fn rename_new_folder_new_file() {
        let t = TempDir::new("rename-new");
        let a = t.write("a.txt", "a");
        let (r, g) = run_op(ShellOp::Rename { path: a.clone(), new_name: "b.txt".into() });
        r.unwrap();
        let b = t.p("b.txt");
        same(&g.created, &[b.clone()]);
        same_pairs(&g.pairs, &[(a.clone(), b.clone())]);
        assert_eq!(read(&b), "a");

        // Case-only rename is fine; a clash is refused up front.
        let (r, g) = run_op(ShellOp::Rename { path: b.clone(), new_name: "B.txt".into() });
        r.unwrap();
        assert_eq!(file_name(&g.created[0]), "B.txt");
        t.write("c.txt", "c");
        let (r, _) = run_op(ShellOp::Rename { path: t.p("B.txt"), new_name: "c.txt".into() });
        assert!(r.unwrap_err().contains("already exists"));

        let (r, g) = run_op(ShellOp::NewFolder { dir: t.0.clone(), name: "New folder".into() });
        r.unwrap();
        same(&g.created, &[t.p("New folder")]);
        assert!(Path::new(&t.p("New folder")).is_dir());
        let (r, g) = run_op(ShellOp::NewFolder { dir: t.0.clone(), name: "New folder".into() });
        r.unwrap();
        same(&g.created, &[t.p("New folder (2)")]);

        let (r, g) = run_op(ShellOp::NewFile { dir: t.0.clone(), name: "notes.txt".into() });
        r.unwrap();
        same(&g.created, &[t.p("notes.txt")]);
        assert_eq!(fs::metadata(t.p("notes.txt")).unwrap().len(), 0);
        let (r, g) = run_op(ShellOp::NewFile { dir: t.0.clone(), name: "notes.txt".into() });
        r.unwrap();
        same(&g.created, &[t.p("notes (2).txt")]);
        assert!(g.pairs.is_empty());

        assert!(matches!(validate(&ShellOp::NewFile { dir: t.0.clone(), name: "a:b".into() }), Err(_)));
    }

    // ----- Recycle Bin -----

    fn unique(tag: &str) -> String {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!("{tag}-{}-{n:x}", std::process::id())
    }

    #[test]
    fn recycle_then_restore() {
        let bin = BinGuard::default();
        let t = TempDir::new("restore");
        let f = t.write(&format!("{}.txt", unique("ferret-restore")), "keep me");

        let (r, g) = run_op(ShellOp::Delete { items: vec![f.clone()], permanent: false });
        r.unwrap();
        assert!(!exists(&f));
        assert!(g.created.is_empty());
        assert_eq!(g.pairs.len(), 1);
        assert_eq!(paths::key(&g.pairs[0].0), paths::key(&f));
        let recycled = bin.track(&g.pairs[0].1);
        assert!(exists(&recycled));

        // Same item as the Recycle Bin listing shows for that original path.
        let listed = in_bin(&f);
        assert_eq!(listed.len(), 1, "expected exactly one bin entry for {f}");
        assert_eq!(paths::key(&listed[0].id.to_string_lossy()), paths::key(&recycled));
        assert_eq!(paths::key(&recycled_original(&recycled).unwrap()), paths::key(&f));
        // The fallback (shell displaced-item properties) agrees with the $I file.
        assert_eq!(displaced_original(&recycled).map(|p| paths::key(&p)), Some(paths::key(&f)));
        let info = info_file(&recycled).unwrap();
        assert!(exists(&info));

        let (r, g) = run_op(ShellOp::Restore { items: vec![recycled.clone()] });
        r.unwrap();
        same(&g.created, &[f.clone()]);
        same_pairs(&g.pairs, &[(recycled.clone(), f.clone())]);
        assert_eq!(read(&f), "keep me");
        assert!(!exists(&recycled), "$R file should be gone");
        assert!(!exists(&info), "$I file should be gone");
        assert!(in_bin(&f).is_empty());
    }

    /// An item whose path is too long for the shell goes to the Recycle Bin and back, by
    /// Restore and by undo, also when its folders are gone.
    #[test]
    fn restores_long_paths() {
        let bin = BinGuard::default();
        let t = TempDir::new("restore-long");
        let top = "L".repeat(150);
        let f = t.write(&format!("{top}\\{}\\{}.txt", "M".repeat(150), unique("ferret-long")), "deep");
        assert!(too_long_for_shell(&f));
        for undo in [false, true] {
            trash::delete(paths::for_io(&f)).unwrap();
            assert!(!exists(&f));
            let listed = in_bin(&f);
            assert_eq!(listed.len(), 1, "the bin lists it under its own path");
            let recycled = bin.track(&paths::normalize(&listed[0].id.to_string_lossy()));
            fs::remove_dir_all(t.p(&top)).unwrap();
            if undo {
                let back = tauri::async_runtime::block_on(fileops::restore_from_trash(vec![f.clone()])).unwrap();
                same(&back, &[f.clone()]);
            } else {
                let (r, g) = run_op(ShellOp::Restore { items: vec![recycled.clone()] });
                r.unwrap();
                same(&g.created, &[f.clone()]);
            }
            assert_eq!(read(&f), "deep");
            assert!(!exists(&recycled) && !exists(&info_file(&recycled).unwrap()));
        }
    }

    #[test]
    fn restore_recreates_folder_and_keeps_both() {
        let bin = BinGuard::default();
        let t = TempDir::new("restore2");
        let name = format!("{}.txt", unique("ferret-restore2"));
        let f = t.write(&format!("gone\\{name}"), "one");

        // Original folder no longer exists → recreated.
        let (r, g) = run_op(ShellOp::Delete { items: vec![f.clone()], permanent: false });
        r.unwrap();
        let r1 = bin.track(&g.pairs[0].1);
        fs::remove_dir(t.p("gone")).unwrap();
        let (r, g) = run_op(ShellOp::Restore { items: vec![r1.clone()] });
        r.unwrap();
        same(&g.created, &[f.clone()]);
        assert_eq!(read(&f), "one");
        assert!(!exists(&r1) && !exists(&info_file(&r1).unwrap()));

        // Name taken at the original location → keep both.
        let (r, g) = run_op(ShellOp::Delete { items: vec![f.clone()], permanent: false });
        r.unwrap();
        let r2 = bin.track(&g.pairs[0].1);
        fs::write(&f, "newcomer").unwrap();
        let (r, g) = run_op(ShellOp::Restore { items: vec![r2.clone()] });
        r.unwrap();
        assert_eq!(g.created.len(), 1);
        let restored = g.created[0].clone();
        println!("    keep-both restore name: {}", file_name(&restored));
        assert_ne!(paths::key(&restored), paths::key(&f));
        assert_eq!(paths::parent(&restored).map(|p| paths::key(&p)), paths::parent(&f).map(|p| paths::key(&p)));
        assert_eq!(file_name(&restored), name.replace(".txt", " - Copy.txt"));
        assert_eq!(read(&restored), "one");
        assert_eq!(read(&f), "newcomer");
        assert!(!exists(&r2) && !exists(&info_file(&r2).unwrap()));
        assert!(in_bin(&f).is_empty());
    }

    #[test]
    fn recycle_then_purge() {
        let bin = BinGuard::default();
        let t = TempDir::new("purge");
        let f = t.write(&format!("{}.txt", unique("ferret-purge")), "bye");
        let (r, g) = run_op(ShellOp::Delete { items: vec![f.clone()], permanent: false });
        r.unwrap();
        let recycled = bin.track(&g.pairs[0].1);
        assert_eq!(in_bin(&f).len(), 1);
        let info = info_file(&recycled).unwrap();
        assert!(exists(&info));

        let (r, g) = run_op(ShellOp::Purge { items: vec![recycled.clone()] });
        r.unwrap();
        assert!(g.created.is_empty() && g.pairs.is_empty());
        assert_eq!((g.files_done, g.totals.1), (1, 1));
        assert!(!exists(&recycled));
        assert!(!exists(&info), "$I file should be gone");
        // Already gone: a clear error, nothing else touched.
        let (r, _) = run_op(ShellOp::Purge { items: vec![recycled.clone()] });
        assert!(r.unwrap_err().contains("no longer in the Recycle Bin"));
        assert!(in_bin(&f).is_empty());
        assert!(!exists(&f));

        // Purge/Restore refuse anything that isn't a Recycle Bin item.
        let other = t.write("other.txt", "x");
        assert!(perform(&ShellOp::Purge { items: vec![other.clone()] }, None, false, &Collect::default()).is_err());
        assert!(exists(&other));
    }

    #[test]
    fn delete_permanently() {
        let t = TempDir::new("delete");
        let folder = t.mkdir(&unique("ferret-delete"));
        t.write(&format!("{}\\a.txt", file_name(&folder)), "a");
        let f = t.write("b.txt", "b");
        let (r, g) = run_op(ShellOp::Delete { items: vec![folder.clone(), f.clone()], permanent: true });
        r.unwrap();
        assert!(!exists(&folder) && !exists(&f));
        assert!(g.created.is_empty() && g.pairs.is_empty());
        assert_eq!((g.files_done, g.totals.1), (2, 2));
        assert!(in_bin(&folder).is_empty() && in_bin(&f).is_empty());
    }

    // ----- cancellation / outcomes -----

    #[test]
    fn cancel_between_items() {
        let t = TempDir::new("cancel-items");
        let srcs: Vec<String> = (1..=5).map(|i| t.write(&format!("src\\f{i}.txt"), "x")).collect();
        let dest = t.mkdir("dest");
        // Cancel when the engine is about to start the second file.
        let c = Collect { cancel_on: Some(("pre Copy", 2)), ..Default::default() };
        let (r, g) = run_with(transfer("copy", srcs.clone(), &dest, Policy::KeepBoth), c, false);
        assert_eq!(r, Err("Cancelled".to_string()));
        assert!(!g.marked_cancelled);
        assert_eq!(ls(&dest), vec!["f1.txt"]);
        same(&g.created, &[t.p("dest\\f1.txt")]);
    }

    #[test]
    fn cancel_mid_file() {
        let t = TempDir::new("cancel-big");
        let big = t.p("big.bin");
        {
            use std::io::Write;
            let mut f = fs::File::create(&big).unwrap();
            let chunk = vec![7u8; 1 << 20];
            for _ in 0..256 {
                f.write_all(&chunk).unwrap();
            }
        }
        let dest = t.mkdir("dest");
        // Cancel from UpdateProgress a few MB into the file.
        let c = Collect { cancel_on: Some(("progress", 8)), ..Default::default() };
        let (r, g) = run_with(transfer("copy", vec![big.clone()], &dest, Policy::KeepBoth), c, false);
        assert_eq!(r, Err("Cancelled".to_string()));
        assert!(g.bytes_done < g.totals.0, "should stop before the end");
        assert!(ls(&dest).is_empty(), "partial copy should be removed: {:?}", ls(&dest));
        assert!(g.trace.iter().any(|l| l.contains("hr=0x80270000")), "item fails with COPYENGINE_E_USER_CANCELLED");
    }

    #[test]
    fn outcomes() {
        let item = |hr: HRESULT| Some(ItemError { hr, verb: "copy", what: "a.txt".into() });
        let denied = COPYENGINE_E_ACCESS_DENIED_DEST;
        assert_eq!(conclude(false, None, false, None, true), Outcome::Done);
        assert_eq!(conclude(true, Some(E_CANCELLED), true, None, true), Outcome::Cancelled);
        // UAC declined / user said no in shell UI.
        assert_eq!(conclude(false, Some(E_CANCELLED), true, None, false), Outcome::Declined);
        assert_eq!(conclude(false, Some(COPYENGINE_E_USER_CANCELLED), true, None, false), Outcome::Declined);
        assert_eq!(conclude(false, Some(E_CANCELLED), false, item(E_CANCELLED), false), Outcome::Declined);
        assert_eq!(conclude(false, None, true, None, false), Outcome::Declined);
        // Real failures; a denial without elevation asks for it.
        assert_eq!(
            conclude(false, Some(denied), false, item(denied), false),
            Outcome::Failed("Couldn't copy “a.txt”: access denied".into())
        );
        assert_eq!(
            conclude(false, Some(denied), false, item(denied), true),
            Outcome::Failed("elevate:You'll need administrator permission to copy “a.txt”".into())
        );
        assert_eq!(
            conclude(false, Some(COPYENGINE_E_SHARING_VIOLATION_SRC), false, item(COPYENGINE_E_SHARING_VIOLATION_SRC), true),
            Outcome::Failed("Couldn't copy “a.txt”: it is open in another program".into())
        );
        assert_eq!(conclude(false, Some(HRESULT(0x8007_0005_u32 as i32)), false, None, true), Outcome::Failed("Access denied".into()));
    }

    #[test]
    fn denied_folder_fails_without_prompt() {
        // A folder that denies us write access (deny ACE on a temp folder; no UAC
        // involved because elevate = false keeps FOF_NOERRORUI in charge).
        let t = TempDir::new("denied");
        let a = t.write("a.txt", "a");
        let locked = t.mkdir("locked");
        let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap());
        let icacls = |args: &[&str]| {
            std::process::Command::new("icacls").arg(&locked).args(args).output().unwrap().status.success()
        };
        assert!(icacls(&["/deny", &format!("{user}:(OI)(CI)(W)")]));
        let probe = needs_admin_write(&locked);
        let (r, g) = run_op(transfer("copy", vec![a.clone()], &locked, Policy::KeepBoth));
        assert!(icacls(&["/remove:d", &user]));
        assert!(probe, "needs_admin_write should see the denial");
        let e = r.unwrap_err();
        if is_elevated() {
            assert_eq!(e, "Couldn't copy “a.txt”: access denied");
        } else {
            assert_eq!(e, "elevate:You'll need administrator permission to copy “a.txt”");
        }
        assert!(g.created.is_empty());
        assert!(ls(&locked).is_empty());
    }

    // ----- probes -----

    #[test]
    fn probes() {
        let elevated = is_elevated();
        println!("    process elevated: {elevated}");
        if elevated {
            println!("    (skipping the Program Files probes: they would write there when elevated)");
        } else {
            assert!(needs_admin_write("C:\\Program Files"));
            assert!(needs_admin_delete("C:\\Program Files\\Common Files"));
        }
        let t = TempDir::new("probes");
        assert!(!needs_admin_write(&t.0));
        assert!(ls(&t.0).is_empty(), "probe file left behind");
        assert!(!needs_admin_write(&t.p("missing")));

        let ro = t.write("readonly.txt", "r");
        let mut perm = fs::metadata(&ro).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&ro, perm.clone()).unwrap();
        assert!(!needs_admin_delete(&ro), "read-only isn't an admin problem");
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&ro, perm).unwrap();

        // Open in "another program" without FILE_SHARE_DELETE.
        let busy = t.write("busy.txt", "b");
        {
            use std::os::windows::fs::OpenOptionsExt;
            let _h = fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2).open(&busy).unwrap();
            assert!(!needs_admin_delete(&busy), "an open file isn't an admin problem");
        }
        assert!(!needs_admin_delete(&t.mkdir("folder")));
        assert!(!needs_admin_delete(&t.p("missing")));
    }

    #[test]
    fn elevate_flag_without_need_does_not_prompt() {
        // FOFX_REQUIREELEVATION | FOFX_SHOWELEVATIONPROMPT on an unprotected folder:
        // no access is denied, so nothing is elevated and no prompt appears.
        let t = TempDir::new("elevate");
        let a = t.write("a.txt", "a");
        let dest = t.mkdir("dest");
        let (r, g) = run_with(transfer("copy", vec![a], &dest, Policy::KeepBoth), Collect::default(), true);
        r.unwrap();
        same(&g.created, &[t.p("dest\\a.txt")]);
    }

    // ----- pure helpers -----

    #[test]
    fn info_file_formats() {
        let path = "C:\\Users\\me\\Desktop\\ünï.txt";
        let wide: Vec<u16> = path.encode_utf16().collect();
        let mut v2 = Vec::new();
        v2.extend(2i64.to_le_bytes());
        v2.extend(123i64.to_le_bytes());
        v2.extend(0i64.to_le_bytes());
        v2.extend(((wide.len() + 1) as u32).to_le_bytes());
        wide.iter().chain([0u16].iter()).for_each(|u| v2.extend(u.to_le_bytes()));
        assert_eq!(parse_info_file(&v2).as_deref(), Some(path));
        let mut v1 = Vec::new();
        v1.extend(1i64.to_le_bytes());
        v1.extend(123i64.to_le_bytes());
        v1.extend(0i64.to_le_bytes());
        (0..260).for_each(|i| v1.extend(wide.get(i).copied().unwrap_or(0).to_le_bytes()));
        assert_eq!(parse_info_file(&v1).as_deref(), Some(path));
        assert_eq!(parse_info_file(&[0u8; 10]), None);
        assert!(is_recycle_item("C:\\$Recycle.Bin\\S-1-5-21-1\\$RABC123.txt"));
        assert!(!is_recycle_item("C:\\$Recycle.Bin\\S-1-5-21-1\\$IABC123.txt"));
        assert!(!is_recycle_item("C:\\Users\\me\\$RABC.txt"));
    }

    #[test]
    fn json_shapes_match_the_frontend() {
        let de = |s: &str| serde_json::from_str::<ShellOp>(s).unwrap();
        assert!(matches!(
            de(r#"{"kind":"transfer","op":"copy","sources":["C:\\a"],"dest":"C:\\b","policy":"keepBoth"}"#),
            ShellOp::Transfer { policy: Policy::KeepBoth, .. }
        ));
        assert!(matches!(de(r#"{"kind":"delete","items":["C:\\a"],"permanent":true}"#), ShellOp::Delete { permanent: true, .. }));
        assert!(matches!(de(r#"{"kind":"rename","path":"C:\\a","newName":"b"}"#), ShellOp::Rename { new_name, .. } if new_name == "b"));
        assert!(matches!(de(r#"{"kind":"newFolder","dir":"C:\\","name":"x"}"#), ShellOp::NewFolder { .. }));
        assert!(matches!(de(r#"{"kind":"newFile","dir":"C:\\","name":"x.txt"}"#), ShellOp::NewFile { .. }));
        assert!(matches!(de(r#"{"kind":"moveBack","pairs":[["C:\\a","C:\\b\\a"]]}"#), ShellOp::MoveBack { pairs } if pairs[0].1 == "C:\\b\\a"));
        assert!(matches!(de(r#"{"kind":"restore","items":[]}"#), ShellOp::Restore { .. }));
        assert!(matches!(de(r#"{"kind":"purge","items":[]}"#), ShellOp::Purge { .. }));
        assert_eq!(
            describe(&transfer("copy", vec!["C:\\x\\a.txt".into()], "C:\\Program Files", Policy::Replace)).1,
            "Copying “a.txt” to Program Files"
        );
        assert_eq!(describe(&ShellOp::Delete { items: vec!["C:\\a".into(), "C:\\b".into()], permanent: false }).1, "Recycling 2 items");
    }
}

