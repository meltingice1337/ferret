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

/// Empties the Recycle Bin on every drive. The UI asks for confirmation first.
#[tauri::command]
pub async fn empty_recycle_bin() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            use windows::core::PCWSTR;
            use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
            // Already empty reports E_UNEXPECTED on some systems; that's fine.
            if info().items == 0 {
                return Ok(());
            }
            unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND) }
                .map_err(|e| e.message().to_string())
        }
        #[cfg(not(windows))]
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
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
