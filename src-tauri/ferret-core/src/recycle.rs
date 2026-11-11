//! The Recycle Bin as a browsable folder. Items are listed through the shell,
//! and each item's original path, size and deletion time come from its `$I`
//! record next to the `$R` data (exact even when Explorer hides extensions).
//! Restore / delete permanently run through `shellops`.

use serde::Serialize;
use std::fs;
use std::path::Path;

use crate::listing::Entry;
use crate::paths;

/// Virtual path of the Recycle Bin in tabs, history and the path bar.
pub const PATH: &str = "shell:RecycleBinFolder";

/// Accepts what people type or paste for the Recycle Bin.
pub fn is_recycle(p: &str) -> bool {
    let t = p.trim().trim_matches('"').trim_end_matches('\\');
    ["shell:RecycleBinFolder", "Recycle Bin", "::{645FF040-5081-101B-9F08-00AA002F954E}", "shell:::{645FF040-5081-101B-9F08-00AA002F954E}"]
        .iter()
        .any(|a| t.eq_ignore_ascii_case(a))
}

pub const DRIVE_FIXED: u32 = 3;
pub const DRIVE_REMOTE: u32 = 4;
/// Starts the error of a recycle that was refused because it can't go to the Recycle Bin: Ferret
/// asks (never Windows, in its own dialog), then deletes for good if that's what's wanted. The rest
/// of the text says why.
pub const NO_BIN: &str = "nobin:";

fn drive_letter(path: &str) -> Option<u8> {
    let n = paths::normalize(path);
    match *n.as_bytes() {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => Some(letter.to_ascii_uppercase()),
        _ => None,
    }
}

fn type_of_drive(letter: u8) -> u32 {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    let root: Vec<u16> = format!("{}:\\", letter as char).encode_utf16().chain(Some(0)).collect();
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }
}

/// The type of drive a path with a drive letter is on.
pub fn drive_type(path: &str) -> Option<u32> {
    drive_letter(path).map(type_of_drive)
}

/// What Windows gives a Recycle Bin until someone sets it: 10% of the first 40 GB and 5% of the
/// rest (C: here: 930.5 GB → 49,691 MB, exactly what it stored).
fn default_bin_limit(volume: u64) -> u64 {
    const FIRST: u64 = 40 << 30;
    if volume <= FIRST { volume / 10 } else { FIRST / 10 + (volume - FIRST) / 20 }
}

/// What the Recycle Bin on a fixed drive holds, in bytes. None: it's set to delete right away.
/// Not knowing means no limit: the check in `shellops` (`State::recycling`) still catches it.
fn bin_limit(letter: u8) -> Option<u64> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeNameForVolumeMountPointW};
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let root = HSTRING::from(format!("{}:\\", letter as char));
    let mut name = [0u16; 64];
    let guid = unsafe { GetVolumeNameForVolumeMountPointW(PCWSTR(root.as_ptr()), &mut name) }.ok().and_then(|()| {
        let name = String::from_utf16_lossy(&name);
        Some(name[name.find('{')?..=name.find('}')?].to_string())
    });
    let settings = guid.and_then(|g| {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{g}")).ok()
    });
    if let Some(key) = &settings {
        if key.get_value::<u32, _>("NukeOnDelete").is_ok_and(|n| n != 0) {
            return None;
        }
        if let Ok(mb) = key.get_value::<u32, _>("MaxCapacity") {
            return Some(u64::from(mb) << 20);
        }
    }
    let mut volume = 0u64;
    match unsafe { GetDiskFreeSpaceExW(PCWSTR(root.as_ptr()), None, Some(&mut volume), None) } {
        Ok(()) => Some(default_bin_limit(volume)),
        Err(_) => Some(u64::MAX),
    }
}

/// Whether Windows has a Recycle Bin for this path: only fixed drives do, and only when it's not
/// set to delete right away. Elsewhere (WSL, network folders, USB sticks) recycling deletes for
/// good and reports that it recycled.
pub fn has_bin(path: &str) -> bool {
    drive_letter(path).is_some_and(|l| type_of_drive(l) == DRIVE_FIXED && bin_limit(l).is_some())
}

/// Which of these a delete can't recycle, for asking first. Off the main thread: asking about a
/// mapped drive that's gone can take a while.
pub fn no_recycle_bin(paths: Vec<String>) -> Vec<String> {
    let mut known = std::collections::HashMap::new();
    paths
        .into_iter()
        .filter(|p| match drive_letter(p) {
            Some(l) => !*known.entry(l).or_insert_with(|| has_bin(p)),
            None => true,
        })
        .collect()
}

/// Items that are too big for their Recycle Bin, and why, for a person.
#[derive(Debug)]
pub struct TooBig {
    pub items: Vec<String>,
    pub why: String,
}

/// "148 GB", "48.5 GB", "512 MB".
fn size_text(bytes: u64) -> String {
    let (value, unit) = if bytes >= 1 << 40 {
        (bytes as f64 / (1u64 << 40) as f64, "TB")
    } else if bytes >= 1 << 30 {
        (bytes as f64 / (1u64 << 30) as f64, "GB")
    } else {
        (bytes as f64 / (1u64 << 20) as f64, "MB")
    };
    if value >= 100.0 { format!("{value:.0} {unit}") } else { format!("{value:.1} {unit}") }
}

/// Answers from the drive index for some folders, in the order asked (see `service_folder_sizes`).
type Index<'a> = &'a dyn Fn(&[String]) -> Option<Vec<Option<crate::search::ipc::FolderTotal>>>;

/// How big `items` are together, as far as it matters: it stops counting once they're over `limit`
/// (the bool says the number is the whole size). None once `stop` says so. Links count for
/// nothing: what they point to doesn't go. Folders come from the drive index when it knows them,
/// else they're walked.
fn measure(items: &[&String], limit: u64, index: Index, stop: &dyn Fn() -> bool, seen: &mut dyn FnMut(u64)) -> Option<(u64, bool)> {
    let mut bytes = 0u64;
    let mut folders: Vec<String> = Vec::new();
    for p in items {
        let Ok(meta) = fs::symlink_metadata(paths::for_io(p)) else { continue };
        if meta.is_dir() {
            folders.push((*p).clone());
        } else if !meta.file_type().is_symlink() {
            bytes += meta.len();
        }
    }
    if bytes > limit {
        return Some((bytes, folders.is_empty()));
    }
    let answers = if folders.is_empty() { None } else { index(&folders) };
    let mut to_walk = Vec::new();
    for (i, folder) in folders.iter().enumerate() {
        match answers.as_ref().and_then(|a| a.get(i)).and_then(|a| a.as_ref()) {
            Some(total) => bytes += total.bytes,
            None => to_walk.push(std::path::PathBuf::from(paths::for_io(folder))),
        }
    }
    let mut count = 0u64;
    while let Some(dir) = to_walk.pop() {
        if bytes > limit {
            return Some((bytes, false));
        }
        if stop() {
            return None;
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                to_walk.push(entry.path());
            } else if !kind.is_symlink() {
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            count += 1;
            if count % 4096 == 0 {
                seen(count);
            }
        }
    }
    Some((bytes, true))
}

/// What of `items` won't fit its Recycle Bin, for Ferret to ask about before anything starts.
/// Windows would delete such a thing for good, file by file, while the job said "Recycling". All
/// selected on a drive count together: one by one, the last ones would push the first ones out for
/// good. Err once `stop` says so.
pub fn too_big(items: &[String], stop: &dyn Fn() -> bool, seen: &mut dyn FnMut(u64)) -> Result<Option<TooBig>, ()> {
    too_big_with(items, &crate::search::service_folder_sizes, &|letter| (type_of_drive(letter) == DRIVE_FIXED).then(|| bin_limit(letter)).flatten(), stop, seen)
}

fn too_big_with(items: &[String], index: Index, limit_of: &dyn Fn(u8) -> Option<u64>, stop: &dyn Fn() -> bool, seen: &mut dyn FnMut(u64)) -> Result<Option<TooBig>, ()> {
    let mut drives: std::collections::BTreeMap<u8, Vec<&String>> = Default::default();
    for p in items {
        if let Some(letter) = drive_letter(p) {
            drives.entry(letter).or_default().push(p);
        }
    }
    let mut found: Option<TooBig> = None;
    for (letter, group) in drives {
        let Some(limit) = limit_of(letter) else { continue };
        let (bytes, whole) = measure(&group, limit, index, stop, seen).ok_or(())?;
        if bytes <= limit {
            continue;
        }
        let name = |p: &str| paths::normalize(p).rsplit('\\').next().unwrap_or_default().to_string();
        let what = match group.as_slice() {
            [one] => format!("“{}” is", name(one)),
            many => format!("These {} items are", crate::jobs::grouped(many.len() as u64)),
        };
        let bin = size_text(limit);
        let why = if whole {
            format!("{what} {}, too big for the Recycle Bin on {}: (it holds {bin})", size_text(bytes), letter as char)
        } else {
            format!("{what} too big for the Recycle Bin on {}: (it holds {bin})", letter as char)
        };
        let found = found.get_or_insert_with(|| TooBig { items: Vec::new(), why });
        found.items.extend(group.into_iter().cloned());
    }
    Ok(found)
}

/// For a job about to recycle `items`: ends it if they don't fit, saying which and why, before
/// anything has happened to them.
pub fn refuse_too_big(ctx: &crate::jobs::JobCtx, items: &[String]) -> Result<(), String> {
    let big = too_big(items, &|| ctx.is_cancelled(), &mut |n| ctx.set_note(&format!("Checking the size… {} items", crate::jobs::grouped(n))))
        .map_err(|()| "Cancelled".to_string())?;
    ctx.set_note("");
    let Some(big) = big else { return Ok(()) };
    log::info!("too big to recycle: {}", big.why);
    for p in &big.items {
        ctx.failed(p.clone(), big.why.clone());
    }
    Err(format!("{NO_BIN}{}", big.why))
}

/// Original full path, size (whole tree for folders) and deletion time (ms) from an item's `$I` record.
fn read_info(r_path: &Path) -> Option<(String, u64, i64)> {
    let name = r_path.file_name()?.to_string_lossy().into_owned();
    let bytes = fs::read(r_path.with_file_name(format!("$I{}", name.get(2..)?))).ok()?;
    if bytes.len() < 28 {
        return None;
    }
    let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
    let utf16 = |b: &[u8]| -> String {
        let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        String::from_utf16_lossy(&units)
    };
    let original = match u64_at(0) {
        1 => utf16(&bytes[24..]),
        2 => {
            let len = u32::from_le_bytes(bytes[24..28].try_into().unwrap()) as usize;
            utf16(&bytes[28..(28 + len * 2).min(bytes.len())])
        }
        _ => return None,
    };
    // FILETIME (100 ns since 1601) → Unix ms.
    let deleted = (u64_at(16) as i64 - 116_444_736_000_000_000) / 10_000;
    Some((original, u64_at(8), deleted))
}

/// Recycled items as folder entries: `path` is the item in `$Recycle.Bin`, `origin`
/// the folder it came from, and `modified` the deletion time (what sorting by date
/// and grouping by date use in this view).
#[cfg(windows)]
pub fn list() -> Result<Vec<Entry>, String> {
    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|it| {
            let path = paths::normalize(&it.id.to_string_lossy());
            let io = paths::for_io(&path);
            let meta = fs::symlink_metadata(&io).ok();
            let (original, size, deleted) = read_info(Path::new(&io)).unwrap_or_else(|| {
                let p = it.original_path().to_string_lossy().into_owned();
                (p, meta.as_ref().map(|m| m.len()).unwrap_or(0), it.time_deleted * 1000)
            });
            let original = paths::normalize(&original);
            let name = original.rsplit('\\').next().unwrap_or(&original).to_string();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let ext = if is_dir {
                String::new()
            } else {
                name.rsplit_once('.').filter(|(s, _)| !s.is_empty()).map(|(_, e)| e.to_lowercase()).unwrap_or_default()
            };
            Entry {
                origin: paths::parent(&original),
                path,
                is_dir,
                is_symlink: false,
                hidden: false,
                size,
                modified: Some(deleted),
                created: meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64),
                ext,
                in_archive: false,
                name,
            }
        })
        .collect())
}

#[cfg(not(windows))]
pub fn list() -> Result<Vec<Entry>, String> {
    Ok(vec![])
}

#[derive(Serialize, Default, Clone, Copy)]
pub struct RecycleInfo {
    pub items: u64,
    pub bytes: u64,
}

/// Item count and total size on the fixed drives (no enumeration). Removable drives are
/// skipped: asking about them can wake a sleeping USB disk and stall for seconds.
#[cfg(windows)]
pub fn info() -> RecycleInfo {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut total = RecycleInfo::default();
    for d in crate::places::drives().into_iter().filter(|d| !d.removable) {
        let mut q = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
        if unsafe { SHQueryRecycleBinW(&HSTRING::from(d.path.as_str()), &mut q) }.is_ok() {
            total.items += q.i64NumItems.max(0) as u64;
            total.bytes += q.i64Size.max(0) as u64;
        }
    }
    total
}

#[cfg(not(windows))]
pub fn info() -> RecycleInfo {
    RecycleInfo::default()
}

pub async fn recycle_info() -> Result<RecycleInfo, String> {
    crate::rt::spawn_blocking(info).await.map_err(|e| e.to_string())
}

/// Empties the Recycle Bin on every drive as a job (the UI asks for confirmation first): item
/// by item, so it shows progress (by size, so a big folder moves the bar too), names what it's
/// on, can be cancelled between items, and asks Try again / Skip / Skip all about an item it
/// can't delete instead of failing as a whole. Returns the job id.
pub fn empty_recycle_bin() -> Result<u64, String> {
    let items = list()?;
    log::info!("emptying the Recycle Bin: {} items", items.len());
    Ok(crate::jobs::spawn("delete", "Emptying the Recycle Bin", move |ctx| {
        let total: u64 = items.iter().map(|e| e.size).sum();
        ctx.set_totals(total, items.len() as u64);
        let started = std::time::Instant::now();
        let mut skip_all = false;
        for e in &items {
            ctx.check()?;
            ctx.set_current(&e.name);
            let item_start = std::time::Instant::now();
            loop {
                let Err(why) = purge(&e.path) else { break };
                let why = format!("Couldn't delete “{}”: {why}", e.name);
                if skip_all {
                    ctx.failed(e.path.clone(), why);
                    break;
                }
                match ctx.ask(why.clone()).as_str() {
                    "retry" => continue,
                    "skip" => ctx.failed(e.path.clone(), why),
                    "skipAll" => {
                        skip_all = true;
                        ctx.failed(e.path.clone(), why);
                    }
                    _ => {
                        ctx.mark_cancelled();
                        return Err("Cancelled".into());
                    }
                }
                break;
            }
            if item_start.elapsed() > std::time::Duration::from_secs(1) {
                log::info!("empty bin: “{}” took {:?}", e.name, item_start.elapsed());
            }
            ctx.add_bytes(e.size);
            ctx.file_done();
        }
        log::info!("emptied the Recycle Bin: {} items in {:?}", items.len(), started.elapsed());
        refresh_bin_icon();
        Ok(())
    }))
}

/// Deletes one recycled item for good: its `$R` file or folder and its `$I` record. Directly
/// first (fast); what that can't remove (read-only files, odd permissions) goes through the
/// Recycle Bin's own delete.
fn purge(r_path: &str) -> Result<(), String> {
    let io = paths::for_io(r_path);
    let r = Path::new(&io);
    let info_file = r.file_name().and_then(|n| n.to_str()).and_then(|n| n.get(2..)).map(|rest| r.with_file_name(format!("$I{rest}")));
    let direct = match fs::symlink_metadata(r) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(r),
        Ok(_) => fs::remove_file(r),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    };
    if direct.is_err() {
        #[cfg(windows)]
        {
            let id = std::ffi::OsString::from(r_path);
            if let Ok(item) = trash::os_limited::list().map(|all| all.into_iter().find(|i| i.id == id)) {
                if let Some(item) = item {
                    trash::os_limited::purge_all([item]).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
        }
        return direct.map_err(|e| e.to_string());
    }
    if let Some(i) = info_file {
        let _ = fs::remove_file(i);
    }
    Ok(())
}

/// The Recycle Bin icon (desktop, Explorer) shows empty/full from its own cache.
fn refresh_bin_icon() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_UPDATEDIR, SHCNF_PATHW};
        for dir in bin_dirs() {
            let w: Vec<u16> = dir.encode_utf16().chain(Some(0)).collect();
            SHChangeNotify(SHCNE_UPDATEDIR, SHCNF_PATHW, Some(w.as_ptr() as *const _), None);
        }
    }
}

/// The current user's `X:\$Recycle.Bin\<SID>` folders that exist (for change watching).
#[cfg(windows)]
pub fn bin_dirs() -> Vec<String> {
    let Some(sid) = user_sid() else { return vec![] };
    crate::places::drives()
        .into_iter()
        .filter(|d| !d.removable)
        .map(|d| format!("{}\\$Recycle.Bin\\{sid}", d.path.trim_end_matches('\\')))
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

#[cfg(not(windows))]
pub fn bin_dirs() -> Vec<String> {
    vec![]
}

#[cfg(all(test, windows))]
mod tests {
    use super::{default_bin_limit, has_bin, size_text, too_big_with};
    use crate::search::ipc::FolderTotal;
    use std::fs;

    #[test]
    fn default_limit_is_what_windows_stores() {
        const MB: u64 = 1 << 20;
        // C: on the machine this was measured on: 930.5 GB, and Windows stored 49,691 MB.
        assert_eq!(default_bin_limit(952_860 * MB) / MB, 49_691);
        assert_eq!(default_bin_limit(20 << 30), 2 << 30);
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(148 << 30), "148 GB");
        assert_eq!(size_text(49_691 << 20), "48.5 GB");
        assert_eq!(size_text(512 << 20), "512 MB");
        assert_eq!(size_text(2 << 40), "2.0 TB");
    }

    #[test]
    fn only_fixed_drives_have_a_bin() {
        assert!(!has_bin(r"\\server\share\x"));
        assert!(!has_bin(r"\\wsl.localhost\Ubuntu\home"));
        assert!(!has_bin("shell:RecycleBinFolder"));
    }

    #[test]
    fn too_big_counts_a_selection_together() {
        let root = std::env::temp_dir().join(format!("ferret-too-big-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dir/sub")).unwrap();
        fs::write(root.join("dir/a"), vec![0u8; 1000]).unwrap();
        fs::write(root.join("dir/sub/b"), vec![0u8; 1000]).unwrap();
        fs::write(root.join("c.txt"), vec![0u8; 500]).unwrap();
        let s = |p: &str| root.join(p).to_string_lossy().to_string();
        let (dir, file) = (s("dir"), s("c.txt"));
        let no_index = |_: &[String]| None;
        let no_stop = || false;
        let check = |items: &[String], limit: u64| too_big_with(items, &no_index, &|_| Some(limit), &no_stop, &mut |_| {}).unwrap();

        // Exactly full still fits.
        assert!(check(std::slice::from_ref(&dir), 2000).is_none());
        let big = check(std::slice::from_ref(&dir), 1999).unwrap();
        assert_eq!(big.items, vec![dir.clone()]);
        assert!(big.why.starts_with("“dir” is ") && big.why.contains("too big for the Recycle Bin on "), "{}", big.why);
        // Each fits alone, together they don't.
        let both = [dir.clone(), file.clone()];
        assert!(check(std::slice::from_ref(&file), 2200).is_none());
        let big = check(&both, 2200).unwrap();
        assert_eq!(big.items, both.to_vec());
        assert!(big.why.starts_with("These 2 items are"), "{}", big.why);
        // No Recycle Bin to fit in: not this check's business.
        assert!(too_big_with(&both, &no_index, &|_| None, &no_stop, &mut |_| {}).unwrap().is_none());
        // The drive index's answer is used instead of walking, and gives the whole size.
        let index = |_: &[String]| Some(vec![Some(FolderTotal { bytes: 5 << 30, files: 1, partial: false })]);
        let big = too_big_with(std::slice::from_ref(&dir), &index, &|_| Some(1 << 30), &no_stop, &mut |_| {}).unwrap().unwrap();
        assert!(big.why.contains("is 5.0 GB, too big"), "{}", big.why);
        // Stopped while walking.
        assert!(too_big_with(std::slice::from_ref(&dir), &no_index, &|_| Some(1 << 30), &|| true, &mut |_| {}).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    /// Read-only: lists the real Recycle Bin and checks every item resolved its origin.
    #[test]
    fn lists_recycle_bin() {
        let t = std::time::Instant::now();
        let items = super::list().unwrap();
        println!("{} items in {:?}; info {:?}", items.len(), t.elapsed(), (super::info().items, super::info().bytes));
        for e in items.iter().take(5) {
            println!("  {} | from {:?} | {} bytes | dir={} | {}", e.name, e.origin, e.size, e.is_dir, e.path);
        }
        let bad: Vec<_> = items.iter().filter(|e| e.name.is_empty() || e.origin.is_none() || !e.path.to_lowercase().contains("$recycle.bin")).collect();
        for e in &bad {
            println!("  BAD {:?} | from {:?} | {}", e.name, e.origin, e.path);
        }
        assert!(bad.is_empty());
        println!("bins: {:?}", super::bin_dirs());
    }
}

#[cfg(windows)]
pub(crate) fn user_sid() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        ok.ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
}

#[cfg(all(test, windows))]
mod purge_tests {
    use super::*;

    /// Emptying deletes an item's `$R` data and its `$I` record, and only that item.
    #[test]
    fn purges_one_recycled_item() {
        let dir = std::env::temp_dir().join(format!("ferret-purge-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("folder")).unwrap();
        let file = dir.join("folder").join("bye.txt");
        fs::write(&file, "x").unwrap();
        let folder = dir.join("folder");
        trash::delete(&folder).unwrap();
        // The Recycle Bin records long paths (not 8.3 names like MELTIN~1).
        let want = paths::key(&paths::long_form(&paths::normalize(&dir.to_string_lossy()))) + "\\folder";
        let find = || trash::os_limited::list().unwrap().into_iter().find(|i| paths::key(&i.original_path().to_string_lossy()) == want);
        let item = find().expect("recycled folder is in the Recycle Bin");
        let r = paths::normalize(&item.id.to_string_lossy());
        let name = Path::new(&r).file_name().unwrap().to_string_lossy().into_owned();
        let info = Path::new(&r).with_file_name(format!("$I{}", &name[2..]));
        assert!(info.exists(), "its $I record exists before");
        purge(&r).unwrap();
        assert!(!Path::new(&r).exists() && !info.exists(), "both $R and $I are gone");
        assert!(find().is_none(), "no longer listed in the Recycle Bin");
        let _ = fs::remove_dir_all(&dir);
        // Leftovers of earlier failed runs of this test (and only those).
        for i in trash::os_limited::list().unwrap() {
            if i.original_path().to_string_lossy().contains("ferret-purge-test-") {
                let _ = purge(&paths::normalize(&i.id.to_string_lossy()));
            }
        }
    }
}

#[cfg(test)]
mod bin_tests {
    #[test]
    fn only_fixed_drives_have_a_bin() {
        use super::has_bin;
        for p in [r"\wsl.localhost\Ubuntu\home\me", r"\server\share\x", r"\localhost\C$\Users", r"\ssh:\home.local\srv", "relative", ""] {
            assert!(!has_bin(p), "{p}");
        }
        #[cfg(windows)]
        {
            let system = std::env::var("SystemDrive").unwrap();
            assert!(has_bin(&format!(r"{system}\Users")));
        }
    }
}
