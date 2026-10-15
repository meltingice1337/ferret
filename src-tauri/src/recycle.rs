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
    items: u64,
    bytes: u64,
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

#[tauri::command]
pub async fn recycle_info() -> Result<RecycleInfo, String> {
    tauri::async_runtime::spawn_blocking(info).await.map_err(|e| e.to_string())
}

/// Empties the Recycle Bin on every drive as a job (the UI asks for confirmation first): item
/// by item, so it shows progress (by size, so a big folder moves the bar too), names what it's
/// on, can be cancelled between items, and asks Try again / Skip / Skip all about an item it
/// can't delete instead of failing as a whole. Returns the job id.
#[tauri::command(async)]
pub fn empty_recycle_bin(app: tauri::AppHandle) -> Result<u64, String> {
    let items = list()?;
    log::info!("emptying the Recycle Bin: {} items", items.len());
    Ok(crate::jobs::spawn(&app, "delete", "Emptying the Recycle Bin", move |ctx| {
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
