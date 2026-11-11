//! Self-update from GitHub Releases, as the Tauri app's (tauri-plugin-updater, src/lib/updater.ts):
//! the same feed (`latest.json`), the same release key and the same installer arguments, so a
//! Tauri copy updates into this one, and this one onward.
//!
//! Ferret checks shortly after it starts and every few hours, then offers the update; nothing
//! installs without a click. The installer is downloaded, checked against Ferret's release key
//! (ferret-core's `search::signed`, the key the Tauri updater checked with) and run in passive
//! mode with `/UPDATE /R`: it closes Ferret, installs over it and starts it again.

use std::path::{Path, PathBuf};

/// Where releases are announced (scripts/release.sh publishes it with every release).
pub const FEED: &str = "https://github.com/meltingice1337/ferret/releases/latest/download/latest.json";

/// FERRET_UPDATE_FEED points elsewhere (tests: a local server). Whatever it offers still has to be
/// signed with the release key.
fn feed() -> String {
    std::env::var("FERRET_UPDATE_FEED").ok().filter(|f| !f.is_empty()).unwrap_or_else(|| FEED.to_string())
}

/// The key installers (and add-ons, addons.rs) must be signed with. Development builds may take
/// another (FERRET_UPDATE_KEY, for testing with a throwaway key); release builds never do.
pub(crate) fn key() -> String {
    #[cfg(debug_assertions)]
    if let Ok(k) = std::env::var("FERRET_UPDATE_KEY") {
        return k;
    }
    ferret_core::search::signed::RELEASE_KEY.to_string()
}

/// A newer version on offer.
#[derive(Clone, Debug)]
pub struct Offer {
    pub version: String,
    /// Release notes: this version's and every older one's, each under "## vX.Y.Z" (release.sh).
    pub notes: String,
    /// YYYY-MM-DD.
    pub date: String,
    url: String,
    signature: String,
}

impl Offer {
    /// The notes of the versions newer than this copy (a copy that skipped some sees them all),
    /// without the changelog link.
    pub fn new_notes(&self) -> String {
        let mine = parse(env!("CARGO_PKG_VERSION"));
        let mut out = Vec::new();
        for part in self.notes.split("\n## ") {
            let part = part.trim_start_matches("## ");
            let (head, body) = part.split_once('\n').unwrap_or((part, ""));
            let Some(v) = parse(head.trim()) else { continue };
            if mine.is_some_and(|m| v <= m) {
                continue;
            }
            let body: Vec<&str> = body.lines().filter(|l| !l.starts_with("**Full changelog:**")).collect();
            out.push(format!("## {}\n{}", head.trim(), body.join("\n").trim()));
        }
        if out.is_empty() { self.notes.clone() } else { out.join("\n\n") }
    }
}

/// "1.2.3" or "v1.2.3" (a pre-release or build suffix ignored).
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    let r = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    it.next().is_none().then_some(r)
}

/// Asks the feed; an offer when it has a version newer than this copy.
pub fn check() -> Result<Option<Offer>, String> {
    let body = get(&feed(), &|_, _| true).map_err(|e| e.say("There's no release to update to yet"))?;
    let feed: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("the update feed can't be read ({e})"))?;
    let version = feed["version"].as_str().ok_or("the update feed has no version")?.trim_start_matches('v').to_string();
    let newer = match (parse(&version), parse(env!("CARGO_PKG_VERSION"))) {
        (Some(new), Some(mine)) => new > mine,
        _ => return Err(format!("the update feed's version “{version}” can't be read")),
    };
    if !newer {
        return Ok(None);
    }
    let platform = &feed["platforms"]["windows-x86_64"];
    let url = platform["url"].as_str().ok_or("the update has no Windows installer")?.to_string();
    let signature = platform["signature"].as_str().ok_or("the update isn't signed")?.to_string();
    Ok(Some(Offer {
        version,
        notes: feed["notes"].as_str().unwrap_or("").to_string(),
        date: feed["pub_date"].as_str().unwrap_or("").chars().take(10).collect(),
        url,
        signature,
    }))
}

/// Downloads the offer's installer (`progress`: bytes so far, total when known) and checks its
/// signature; the installer, saved in a folder of its own under %TEMP%.
pub fn download(offer: &Offer, progress: &dyn Fn(u64, u64)) -> Result<PathBuf, String> {
    let data = get(&offer.url, &|got, total| {
        progress(got, total);
        true
    })
    .map_err(|e| e.say("The installer isn't on the server"))?;
    ferret_core::search::signed::verify(&data, &offer.signature, &key()).map_err(|e| format!("The download was refused: {e}"))?;
    let dir = std::env::temp_dir().join(format!("ferret-update-{}", offer.version));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = offer.url.rsplit('/').next().filter(|n| n.to_lowercase().ends_with(".exe")).unwrap_or("Ferret-setup.exe");
    let path = dir.join(name);
    std::fs::write(&path, &data).map_err(|e| format!("The installer couldn't be saved ({e})"))?;
    Ok(path)
}

/// Runs the installer as the Tauri updater does (passive: a progress window, no questions;
/// `/UPDATE`; `/R`: start Ferret again). The caller quits Ferret right after, which the installer
/// waits for.
pub fn install(installer: &Path) -> Result<(), String> {
    // Tests: no installer window at all (FERRET_UPDATE_QUIET).
    let quiet = std::env::var_os("FERRET_UPDATE_QUIET").is_some();
    std::process::Command::new(installer)
        .args([if quiet { "/S" } else { "/P" }, "/UPDATE", "/R"])
        // The Ferret it starts again doesn't run this one's test script.
        .env_remove("FERRET_GPUI_SCRIPT")
        .env_remove("FERRET_GPUI_SCRIPT_OUT")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("The installer couldn't start ({e})"))
}

/// Why `get` failed: what to say, and the server's answer when that was the reason (404…).
#[derive(Debug)]
pub(crate) struct GetError {
    pub status: u32,
    pub message: String,
}

impl GetError {
    fn new(message: impl Into<String>) -> Self {
        GetError { status: 0, message: message.into() }
    }

    /// What to tell the user; `missing` says what a 404 means to the caller ("There's no release
    /// to update to yet").
    pub(crate) fn say(self, missing: &str) -> String {
        if self.status == 404 { format!("{missing} (the server said 404)") } else { self.message }
    }
}

/// A GET over WinHTTP (Windows' own HTTP stack: its proxy settings and certificate store, and no
/// TLS library to ship). Follows redirects (GitHub's downloads are one). `progress` gets the bytes
/// so far and the total (0 when the server doesn't say); returning false stops the download
/// ("Cancelled"). The updater and add-ons (addons.rs) download with it.
pub(crate) fn get(url: &str, progress: &dyn Fn(u64, u64) -> bool) -> Result<Vec<u8>, GetError> {
    use windows::Win32::Networking::WinHttp::*;
    use windows::core::{HSTRING, PCWSTR, w};

    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }
    let wide: Vec<u16> = url.encode_utf16().collect();
    let mut host = vec![0u16; 256];
    let mut path = vec![0u16; 4096];
    let mut extra = vec![0u16; 2048];
    let mut parts = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        lpszHostName: windows::core::PWSTR(host.as_mut_ptr()),
        dwHostNameLength: host.len() as u32,
        lpszUrlPath: windows::core::PWSTR(path.as_mut_ptr()),
        dwUrlPathLength: path.len() as u32,
        lpszExtraInfo: windows::core::PWSTR(extra.as_mut_ptr()),
        dwExtraInfoLength: extra.len() as u32,
        ..Default::default()
    };
    unsafe { WinHttpCrackUrl(&wide, 0, &mut parts) }.map_err(|_| GetError::new(format!("“{url}” isn't a web address")))?;
    let host_name = String::from_utf16_lossy(&host[..parts.dwHostNameLength as usize]);
    // WinHTTP's own errors (12xxx) have no text in Windows' message table.
    let failed = |what: &str| {
        let e = std::io::Error::last_os_error();
        let why = match e.raw_os_error().unwrap_or(0) as u32 {
            ERROR_WINHTTP_TIMEOUT => "it took too long to answer".to_string(),
            ERROR_WINHTTP_NAME_NOT_RESOLVED => "the address wasn't found: are you online?".to_string(),
            ERROR_WINHTTP_CANNOT_CONNECT => "the connection failed".to_string(),
            ERROR_WINHTTP_CONNECTION_ERROR => "the connection was reset".to_string(),
            ERROR_WINHTTP_SECURE_FAILURE => "its secure connection couldn't be checked".to_string(),
            _ => format!("{what}: {e}"),
        };
        GetError::new(format!("Couldn't reach {host_name} ({why})"))
    };
    let host = HSTRING::from_wide(&host[..parts.dwHostNameLength as usize]);
    // The path, then its query.
    let mut object: Vec<u16> = path[..parts.dwUrlPathLength as usize].to_vec();
    object.extend_from_slice(&extra[..parts.dwExtraInfoLength as usize]);
    let object = HSTRING::from_wide(&object);
    let secure = url.to_ascii_lowercase().starts_with("https:");

    let agent = HSTRING::from(format!("Ferret/{}", env!("CARGO_PKG_VERSION")));
    let session = Handle(unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0) });
    if session.0.is_null() {
        return Err(failed("open"));
    }
    // Resolve, connect, send: 15 s each; a stalled download: 60 s.
    let _ = unsafe { WinHttpSetTimeouts(session.0, 15_000, 15_000, 15_000, 60_000) };
    let connect = Handle(unsafe { WinHttpConnect(session.0, &host, parts.nPort, 0) });
    if connect.0.is_null() {
        return Err(failed("connect"));
    }
    let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
    let request = Handle(unsafe { WinHttpOpenRequest(connect.0, w!("GET"), &object, PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags) });
    if request.0.is_null() {
        return Err(failed("request"));
    }
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.map_err(|_| failed("send"))?;
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }.map_err(|_| failed("response"))?;
    let number = |what: u32| -> Option<u32> {
        let mut n = 0u32;
        let mut size = 4u32;
        unsafe {
            WinHttpQueryHeaders(request.0, what | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut n as *mut u32 as _), &mut size, std::ptr::null_mut())
        }
        .ok()
        .map(|_| n)
    };
    let status = number(WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
    if status != 200 {
        // The caller says what a 404 means for it (`GetError::say`).
        return Err(GetError { status, message: format!("{host_name} answered {status}") });
    }
    let total = number(WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;
    // What the server announces isn't trusted with memory up front.
    let mut out = Vec::with_capacity(total.min(64 << 20) as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut available = 0u32;
        unsafe { WinHttpQueryDataAvailable(request.0, &mut available) }.map_err(|_| failed("download"))?;
        if available == 0 {
            break;
        }
        let mut read = 0u32;
        let want = (available as usize).min(buf.len()) as u32;
        unsafe { WinHttpReadData(request.0, buf.as_mut_ptr() as _, want, &mut read) }.map_err(|_| failed("download"))?;
        if read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..read as usize]);
        if !progress(out.len() as u64, total) {
            return Err(GetError::new("Cancelled"));
        }
    }
    if total > 0 && (out.len() as u64) < total {
        return Err(GetError::new("The download stopped partway"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse("v1.10.0"), Some((1, 10, 0)));
        assert!(parse("1.10.0") > parse("1.9.12"));
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3-beta.1"), Some((1, 2, 3)));
    }

    #[test]
    fn only_newer_notes_are_shown() {
        let offer = Offer {
            version: "99.0.1".into(),
            notes: "## v99.0.1\n- newest\n\n## v99.0.0\n- newer\n\n## v0.0.1\n- old\n\n**Full changelog:** https://x".into(),
            date: String::new(),
            url: String::new(),
            signature: String::new(),
        };
        let shown = offer.new_notes();
        assert!(shown.contains("- newest") && shown.contains("- newer"));
        assert!(!shown.contains("- old") && !shown.contains("Full changelog"));
    }
}
