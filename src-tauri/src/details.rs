//! Extra facts about files that have no visual preview: the version resource
//! of executables and libraries (description, company, version…) and the CPU
//! architecture from the PE header.

use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};

use crate::paths;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileDetails {
    /// (label, value) pairs from the version resource, in display order.
    version: Vec<(String, String)>,
    /// "64-bit (x64)", "32-bit (x86)", "ARM64"… for PE files.
    arch: Option<String>,
}

/// Machine field of a PE image (`MZ` → `PE\0\0` → IMAGE_FILE_HEADER.Machine).
fn pe_arch(path: &str) -> Option<String> {
    let mut f = std::fs::File::open(paths::for_io(path)).ok()?;
    let mut mz = [0u8; 64];
    f.read_exact(&mut mz).ok()?;
    if &mz[..2] != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(mz[60..64].try_into().ok()?) as u64;
    f.seek(SeekFrom::Start(pe)).ok()?;
    let mut hdr = [0u8; 6];
    f.read_exact(&mut hdr).ok()?;
    if &hdr[..4] != b"PE\0\0" {
        return None;
    }
    Some(
        match u16::from_le_bytes([hdr[4], hdr[5]]) {
            0x8664 => "64-bit (x64)",
            0x014c => "32-bit (x86)",
            0xaa64 => "ARM64",
            0x01c4 => "32-bit (ARM)",
            _ => "Unknown CPU",
        }
        .to_string(),
    )
}

#[cfg(windows)]
fn version_info(path: &str) -> Vec<(String, String)> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let file = HSTRING::from(paths::for_io(path));
    unsafe {
        let size = GetFileVersionInfoSizeW(&file, None);
        if size == 0 {
            return vec![];
        }
        let mut buf = vec![0u8; size as usize];
        if GetFileVersionInfoW(&file, None, size, buf.as_mut_ptr().cast()).is_err() {
            return vec![];
        }
        let query = |sub: &str| -> Option<(*const u8, u32)> {
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            VerQueryValueW(buf.as_ptr().cast(), PCWSTR(HSTRING::from(sub).as_ptr()), &mut ptr, &mut len)
                .as_bool()
                .then_some((ptr as *const u8, len))
        };
        // First language/code page pair, falling back to US English / Unicode.
        let lang = query(r"\VarFileInfo\Translation")
            .filter(|(_, len)| *len >= 4)
            .map(|(p, _)| {
                let w = std::slice::from_raw_parts(p as *const u16, 2);
                format!("{:04x}{:04x}", w[0], w[1])
            })
            .unwrap_or_else(|| "040904b0".into());
        let text = |key: &str| -> Option<String> {
            let (p, len) = query(&format!(r"\StringFileInfo\{lang}\{key}"))?;
            if len == 0 {
                return None;
            }
            let units = std::slice::from_raw_parts(p as *const u16, len as usize);
            let s = String::from_utf16_lossy(units).trim_end_matches('\0').trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        [
            ("Description", "FileDescription"),
            ("Product", "ProductName"),
            ("Company", "CompanyName"),
            ("File version", "FileVersion"),
            ("Product version", "ProductVersion"),
            ("Copyright", "LegalCopyright"),
            ("Original name", "OriginalFilename"),
        ]
        .into_iter()
        .filter_map(|(label, key)| text(key).map(|v| (label.to_string(), v)))
        .collect()
    }
}

#[cfg(not(windows))]
fn version_info(_path: &str) -> Vec<(String, String)> {
    vec![]
}

#[tauri::command]
pub async fn file_details(path: String) -> Result<FileDetails, String> {
    tauri::async_runtime::spawn_blocking(move || FileDetails { version: version_info(&path), arch: pe_arch(&path) })
        .await
        .map_err(|e| e.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn reads_notepad_details() {
        let d = super::version_info(r"C:\Windows\System32\notepad.exe");
        println!("{d:?} {:?}", super::pe_arch(r"C:\Windows\System32\notepad.exe"));
        assert!(d.iter().any(|(k, _)| k == "File version"));
        assert_eq!(super::pe_arch(r"C:\Windows\System32\kernel32.dll").as_deref(), Some("64-bit (x64)"));
    }
}
