//! Add-ons: optional parts Ferret downloads only when asked, kept out of the installer. One so
//! far: better text recognition for scanned PDFs (PaddleOCR's PP-OCRv5 models run by Microsoft's
//! ONNX Runtime), about 23 MB, which ocr.rs uses instead of Windows' own text recognition.
//!
//! The pack is a zip on a GitHub release (scripts/build-ocr-pack.sh makes it), signed with
//! Ferret's release key as the installer is (`tauri signer sign`: a `.sig` beside it). Getting it
//! is a job like a copy, so its card shows the download and can cancel it:
//! 1. the signature, then the zip (WinHTTP: update.rs's `get`);
//! 2. the zip's signature is checked against the release key (update.rs's `key`);
//! 3. it's unpacked into a folder beside the final one (`ocr-1.part-…`): every file must be listed
//!    in the pack's manifest.json with its SHA-256 and match it, no name may lead out of the folder,
//!    and nothing listed may be missing;
//! 4. only then is that folder renamed into place, so a folder at the final place is a whole pack.
//!
//! Where: `appdata::local_dir()\addons\ocr-1` (installed: %LOCALAPPDATA%\com.ferret.app\addons;
//! programs, so not the roaming folder). The manifest stays in it: `verify_ocr_pack` checks every
//! file again before ocr.rs loads anything from it.
//!
//! Removing: Windows keeps a loaded DLL from being deleted, so once ocr.rs has loaded
//! onnxruntime.dll (`mark_ocr_loaded`) the pack stays until Ferret closes: a marker
//! (`ocr-1.remove`) has the next start delete it (`startup`, early in main.rs). A pack is always
//! renamed aside (`ocr-1.removing-…`) before it's deleted, so its place is never left half
//! deleted; what can't be deleted yet (a DLL another Ferret window still has loaded) stays in that
//! folder until a later start clears it. Windows does allow the rename while the DLL is loaded.
//!
//! Redrawing: the state lives here (`ocr_pack()` is cheap: no disk access after the first call);
//! while a download runs, a task redraws every window five times a second, and the job's own
//! `job-update` events redraw the jobs card.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::update;

/// About how much the download is (Settings, the PDF preview's offer).
pub const OCR_PACK_MB: u32 = 23;

/// Where the pack is published (scripts/build-ocr-pack.sh prints the command that does it); the
/// signature is the same address plus `.sig`. FERRET_OCR_PACK_URL points elsewhere (tests: a
/// local server); whatever it serves still has to be signed with the release key.
pub const OCR_PACK_URL: &str = "https://github.com/meltingice1337/ferret/releases/download/ocr-pack-1/ferret-ocr-pack-1.zip";

/// The pack's folder under `addons`, and the manifest's `pack` number it must have.
const FOLDER: &str = "ocr-1";
const FORMAT: u64 = 1;
/// What ocr.rs loads: a pack without these is refused.
const REQUIRED: [&str; 5] = ["onnxruntime.dll", "det/inference.onnx", "rec/inference.onnx", "rec/inference.yml", "ori/inference.onnx"];
/// Far above the real pack (23 MB zipped, 37 MB unpacked): anything bigger isn't it.
const MAX_DOWNLOAD: u64 = 128 << 20;
const MAX_UNPACKED: u64 = 256 << 20;

/// The add-on as the UI sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcrPack {
    Missing,
    /// Bytes so far and in all (0 until the server says). `got == total` while it's checked and
    /// unpacked.
    Downloading { got: u64, total: u64 },
    /// Installed and usable: its folder.
    Ready(PathBuf),
    /// The last try failed (why, said plainly). Downloading again is offered.
    Failed(String),
    /// Removed at the next start (it was in use when the user removed it).
    RemoveAtRestart,
}

#[derive(Default)]
struct State {
    /// Bumped by every download started and cancelled: a download's thread only reports while
    /// it's the current one.
    generation: u64,
    download: Option<Download>,
    failed: Option<String>,
    /// What's on disk, read once (`read_disk`) and kept up to date here after.
    read: bool,
    installed: bool,
    removing: bool,
    /// The installed pack's size on disk, once known.
    size: Option<u64>,
    /// ocr.rs loaded its DLL: the folder stays until Ferret closes.
    loaded: bool,
    /// Its files were checked against the manifest in this session.
    verified: bool,
}

struct Download {
    generation: u64,
    job: u64,
    got: u64,
    total: u64,
}

impl State {
    fn read_disk(&mut self) {
        if !self.read {
            self.read = true;
            self.installed = pack_dir().join("manifest.json").is_file();
            self.removing = marker().exists();
        }
    }
}

fn state() -> MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn addons_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| crate::appdata::local_dir().join("addons")).clone()
}

fn pack_dir() -> PathBuf {
    addons_dir().join(FOLDER)
}

/// Its presence says: delete the pack at the next start.
fn marker() -> PathBuf {
    addons_dir().join(format!("{FOLDER}.remove"))
}

fn url() -> String {
    std::env::var("FERRET_OCR_PACK_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| OCR_PACK_URL.to_string())
}

/// Development builds: FERRET_OCR_PACK_DIR is an unpacked pack used as it is (ocr.rs's own tests).
/// Ferret never downloads into it, re-checks it or removes it.
fn test_dir() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var_os("FERRET_OCR_PACK_DIR").map(PathBuf::from).filter(|d| d.join("onnxruntime.dll").is_file()) {
        return Some(d);
    }
    None
}

/// The add-on's state. Cheap (no disk access after the first call): fine to call while drawing.
pub fn ocr_pack() -> OcrPack {
    if let Some(d) = test_dir() {
        return OcrPack::Ready(d);
    }
    let mut s = state();
    s.read_disk();
    if s.installed && s.removing {
        OcrPack::RemoveAtRestart
    } else if let Some(d) = &s.download {
        OcrPack::Downloading { got: d.got, total: d.total }
    } else if let Some(e) = &s.failed {
        OcrPack::Failed(e.clone())
    } else if s.installed {
        OcrPack::Ready(pack_dir())
    } else {
        OcrPack::Missing
    }
}

/// The pack's folder (onnxruntime.dll, det/, rec/, ori/…), only when it's ready to use.
pub fn ocr_pack_dir() -> Option<PathBuf> {
    match ocr_pack() {
        OcrPack::Ready(d) => Some(d),
        _ => None,
    }
}

/// Settings' explanation of the add-on.
pub fn ocr_pack_blurb() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| format!("Reads tables, faxes and unusual fonts more accurately than Windows' built-in text recognition. About {OCR_PACK_MB} MB."))
}

/// The installed pack's size on disk (Settings), worked out once.
pub fn ocr_pack_size() -> Option<u64> {
    let dir = ocr_pack_dir()?;
    if let Some(n) = state().size {
        return Some(n);
    }
    let n = folder_size(&dir);
    state().size = Some(n);
    Some(n)
}

fn folder_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => folder_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

/// Downloads, checks and installs the pack in the background, as a job (its card in the jobs
/// tray shows the progress and can cancel it). Nothing happens while it downloads or once it's
/// ready; asked again while a removal waits for a restart, it keeps the pack instead.
pub fn start_ocr_download(cx: &mut gpui::App) {
    if test_dir().is_some() {
        return;
    }
    let generation = {
        let mut s = state();
        s.read_disk();
        if s.download.is_some() {
            return;
        }
        if s.installed {
            if s.removing {
                keep(&mut s);
                cx.refresh_windows();
                return;
            }
            if s.failed.is_none() {
                return;
            }
            // A copy that failed its check (`verify_ocr_pack`) and couldn't go then: it goes now.
            if let Err(e) = delete_pack(&pack_dir()) {
                s.failed = Some(format!("The damaged copy is still in use ({e}): restart Ferret and try again"));
                cx.refresh_windows();
                return;
            }
            s.installed = false;
            s.size = None;
        }
        s.generation += 1;
        s.failed = None;
        s.verified = false;
        s.download = Some(Download { generation: s.generation, job: 0, got: 0, total: 0 });
        s.generation
    };
    let url = url();
    log::info!("add-on: downloading better text recognition from {url}");
    let job = ferret_core::jobs::spawn("download", "Better text recognition", move |ctx| run(generation, &url, ctx));
    if let Some(d) = state().download.as_mut().filter(|d| d.generation == generation) {
        d.job = job;
    }
    // Its card says how it went; so does a toast (it may have been started from a PDF's offer).
    crate::ops::when_done(job, |f, done, cx| match done.status.as_str() {
        "done" => f.toast("Better text recognition is ready", cx),
        "error" => f.toast_error(format!("Couldn't get better text recognition: {}", done.error.unwrap_or_default()), cx),
        _ => {}
    });
    // Every window shows the progress (Settings, the PDF preview's offer), 5 times a second.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            cx.refresh();
            if !state().download.as_ref().is_some_and(|d| d.generation == generation) {
                break;
            }
        }
    })
    .detach();
    cx.refresh_windows();
}

/// Stops a download (Settings' Cancel; the job card's × does the same through the job).
pub fn cancel_ocr_download() {
    let job = {
        let mut s = state();
        let Some(d) = s.download.take() else { return };
        s.generation += 1;
        d.job
    };
    log::info!("add-on: download cancelled");
    ferret_core::jobs::cancel_job(job);
}

/// Removes the pack: Ok(true) when it's gone now, Ok(false) when it goes at the next start
/// (ocr.rs has loaded its DLL, or the folder couldn't be renamed away).
pub fn remove_ocr_pack() -> Result<bool, String> {
    if test_dir().is_some() {
        return Err("FERRET_OCR_PACK_DIR is set: that folder is left alone".into());
    }
    cancel_ocr_download();
    // The text reader (a helper process) may have the add-on's DLL loaded: stopped first.
    crate::ocrhelper::stop();
    let mut s = state();
    s.read_disk();
    s.failed = None;
    if !s.installed {
        keep(&mut s);
        return Ok(true);
    }
    if !s.loaded {
        match delete_pack(&pack_dir()) {
            Ok(()) => {
                keep(&mut s);
                s.installed = false;
                s.size = None;
                s.verified = false;
                log::info!("add-on: better text recognition removed");
                return Ok(true);
            }
            Err(e) => log::info!("add-on: better text recognition can't be removed now ({e}): at the next start"),
        }
    }
    fs::write(marker(), b"").map_err(|e| format!("Couldn't mark it for removal ({e})"))?;
    s.removing = true;
    Ok(false)
}

/// Keeps a pack that was to be removed at the next start.
pub fn keep_ocr_pack() {
    keep(&mut state());
}

fn keep(s: &mut State) {
    if s.removing || marker().exists() {
        let _ = fs::remove_file(marker());
    }
    s.removing = false;
}

/// ocr.rs loaded onnxruntime.dll from the pack: Windows keeps it locked until the process ends, so
/// a removal now waits for the next start. Reading happens in the text reader (ocrhelper.rs), which
/// `remove_ocr_pack` stops first, so in a window this only stays set by the `addon loaded` step.
pub fn mark_ocr_loaded() {
    state().loaded = true;
}

/// Checks every file of the installed pack against its manifest (SHA-256), once per session
/// (a pack installed in this session was checked as it was unpacked). Call it before loading
/// anything from the pack, off the UI thread: it reads the whole pack (37 MB, tens of ms in a
/// release build). A damaged pack is removed and reported as Failed, so "Try again" downloads a
/// whole one.
pub fn verify_ocr_pack() -> Result<(), String> {
    if test_dir().is_some() {
        return Ok(());
    }
    let dir = match ocr_pack() {
        OcrPack::Ready(dir) => dir,
        OcrPack::RemoveAtRestart => return Err("Better text recognition is being removed".into()),
        _ => return Err("Better text recognition isn't installed".into()),
    };
    if state().verified {
        return Ok(());
    }
    match check_folder(&dir) {
        Ok(size) => {
            let mut s = state();
            s.verified = true;
            s.size = Some(size);
            Ok(())
        }
        Err(e) => {
            log::warn!("add-on: the installed text recognition failed its check: {e}");
            let mut s = state();
            // Not loaded yet (this runs before it is), so it can go.
            if !s.loaded && delete_pack(&dir).is_ok() {
                s.installed = false;
                s.size = None;
            }
            let msg = format!("its files are damaged ({e}): download it again");
            s.failed = Some(msg.clone());
            Err(msg)
        }
    }
}

/// Early in main, before anything can load the pack: finishes a removal that had to wait for a
/// restart, and clears what an install or a removal cut short left behind.
pub fn startup() {
    let dir = addons_dir();
    let Ok(entries) = fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Another Ferret window may be unpacking right now: only an old part folder is a leftover.
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(600));
        if name.starts_with(&format!("{FOLDER}.removing-")) || (name.starts_with(&format!("{FOLDER}.part-")) && old) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
    if marker().exists() {
        match delete_pack(&pack_dir()) {
            Ok(()) => {
                let _ = fs::remove_file(marker());
                log::info!("add-on: better text recognition removed (asked for before the restart)");
            }
            Err(e) => log::info!("add-on: better text recognition can't be removed yet ({e}): it goes at the next start"),
        }
    }
}

/// The download job: the work, then what it means for the state.
fn run(generation: u64, url: &str, ctx: &ferret_core::jobs::JobCtx) -> Result<(), String> {
    // Should the work panic (the job catches it), the add-on isn't left "downloading" for good.
    struct Ended(u64);
    impl Drop for Ended {
        fn drop(&mut self) {
            let mut s = state();
            if s.download.as_ref().is_some_and(|d| d.generation == self.0) {
                s.download = None;
                s.failed = Some("Something went wrong (the log says what)".into());
            }
        }
    }
    let _ended = Ended(generation);
    let result = fetch(generation, url, ctx);
    let mut s = state();
    let current = s.download.as_ref().is_some_and(|d| d.generation == generation);
    // Put in place under the lock: a cancel can't slip in between.
    let result = match result {
        Ok((staged, size)) if current && !ctx.is_cancelled() => place(&staged, &pack_dir()).map(|_| size),
        Ok((staged, _)) => {
            let _ = fs::remove_dir_all(&staged);
            Err("Cancelled".to_string())
        }
        Err(e) => Err(e),
    };
    if current {
        s.download = None;
        match &result {
            Ok(size) => {
                s.installed = true;
                s.size = Some(*size);
                s.verified = true;
                keep(&mut s);
            }
            Err(_) if ctx.is_cancelled() => {}
            Err(e) => s.failed = Some(e.clone()),
        }
    }
    drop(s);
    match &result {
        Ok(size) => log::info!("add-on: better text recognition installed ({size} bytes) in {}", pack_dir().display()),
        Err(_) if ctx.is_cancelled() || !current => {}
        Err(e) => log::warn!("add-on: better text recognition wasn't installed: {e}"),
    }
    result.map(|_| ())
}

/// Downloads the signature and the pack, checks and unpacks it beside its final place; that
/// folder and the pack's size.
fn fetch(generation: u64, url: &str, ctx: &ferret_core::jobs::JobCtx) -> Result<(PathBuf, u64), String> {
    // Said after "Couldn't get better text recognition: " (toasts, the PDF preview's offer).
    let missing = "It isn't available for download yet";
    let host = url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("");
    let going = |got: Option<(u64, u64)>| {
        let mut s = state();
        match s.download.as_mut().filter(|d| d.generation == generation) {
            Some(d) => {
                if let Some((got, total)) = got {
                    (d.got, d.total) = (got, total);
                }
                !ctx.is_cancelled()
            }
            None => false,
        }
    };
    ctx.set_note(&format!("Connecting to {host}…"));
    let sig = update::get(&format!("{url}.sig"), &|_, _| going(None)).map_err(|e| e.say(missing))?;
    let sig = String::from_utf8(sig).map_err(|_| "Its signature can't be read".to_string())?;
    if !going(None) {
        return Err("Cancelled".into());
    }
    ctx.set_note(&format!("Downloading from {host}"));
    let sent = std::cell::Cell::new(0u64);
    let announced = std::cell::Cell::new(false);
    let too_big = std::cell::Cell::new(false);
    let zip = update::get(url, &|got, total| {
        if got > MAX_DOWNLOAD || total > MAX_DOWNLOAD {
            too_big.set(true);
            return false;
        }
        if total > 0 && !announced.get() {
            announced.set(true);
            ctx.set_totals(total, 0);
        }
        ctx.add_bytes(got.saturating_sub(sent.get()));
        sent.set(got);
        going(Some((got, total)))
    })
    .map_err(|e| if too_big.get() { "The download is far bigger than the add-on: refused".to_string() } else { e.say(missing) })?;
    if !going(Some((zip.len() as u64, zip.len() as u64))) {
        return Err("Cancelled".into());
    }
    ctx.set_note("Checking it…");
    let staged = addons_dir().join(format!("{FOLDER}.part-{}-{generation}", std::process::id()));
    let _ = fs::remove_dir_all(&staged);
    match install(&zip, &sig, &update::key(), &staged) {
        Ok(size) => Ok((staged, size)),
        Err(e) => {
            let _ = fs::remove_dir_all(&staged);
            Err(e)
        }
    }
}

/// Checks the pack's signature against `key`, then unpacks it into `dest` (checking every file
/// against its manifest); the size unpacked.
fn install(zip: &[u8], sig: &str, key: &str, dest: &Path) -> Result<u64, String> {
    ferret_core::search::signed::verify(zip, sig, key).map_err(|e| format!("The download was refused: {e}"))?;
    unpack(zip, dest)
}

/// Moves a checked folder into place (something left there first goes, whole).
fn place(staged: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        delete_pack(dest).map_err(|e| {
            let _ = fs::remove_dir_all(staged);
            format!("The copy already there is in use ({e}): restart Ferret and try again")
        })?;
    }
    fs::rename(staged, dest).map_err(|e| {
        let _ = fs::remove_dir_all(staged);
        format!("It couldn't be put in place ({e})")
    })
}

/// Takes a pack's folder away whole: renamed aside first (an error only when something has a file
/// in it open without sharing), then deleted. What can't be deleted yet (a DLL another Ferret
/// window has loaded: Windows allows renaming it, not deleting it) goes at a later start
/// (`startup`).
fn delete_pack(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        return Ok(());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let aside = dir.with_file_name(format!("{name}.removing-{}-{stamp}", std::process::id()));
    fs::rename(dir, &aside).map_err(|e| format!("it's in use: {e}"))?;
    if let Err(e) = fs::remove_dir_all(&aside) {
        log::warn!("add-on: {} couldn't be deleted yet ({e})", aside.display());
    }
    Ok(())
}

/// The manifest's files (relative path → SHA-256, lower-case hex), checked: the right format,
/// names that stay inside the folder, a checksum for each, and every file ocr.rs needs.
fn parse_manifest(text: &str) -> Result<BTreeMap<String, String>, String> {
    let bad = |why: String| format!("Its manifest {why}");
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| bad("can't be read".into()))?;
    if v["pack"].as_u64() != Some(FORMAT) {
        return Err(bad("is for another version of Ferret".into()));
    }
    let files = v["files"].as_object().ok_or_else(|| bad("lists no files".into()))?;
    let mut out = BTreeMap::new();
    // Windows' names ignore case: two names that differ only by it would be one file.
    let mut folded = HashSet::new();
    for (name, hash) in files {
        safe_path(name)?;
        if name == "manifest.json" || !folded.insert(name.to_ascii_lowercase()) {
            return Err(bad(format!("lists “{name}” twice")));
        }
        let hash = hash.as_str().filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(|| bad(format!("has no checksum for “{name}”")))?;
        out.insert(name.clone(), hash.to_ascii_lowercase());
    }
    if let Some(m) = REQUIRED.iter().find(|r| !out.contains_key(**r)) {
        return Err(bad(format!("doesn't list “{m}”")));
    }
    Ok(out)
}

/// A name from the pack as a relative path, or why not: only plain names (letters, digits, `.`,
/// `-`, `_`) separated by `/`, so nothing absolute, no `..`, no drive, no `\`, no `:` (alternate
/// streams), no names Windows reserves for devices.
fn safe_path(name: &str) -> Result<PathBuf, String> {
    let plain = |part: &str| {
        let stem = part.split('.').next().unwrap_or("").to_ascii_lowercase();
        let device = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
            || (stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit());
        !part.is_empty()
            && part.len() <= 100
            && !part.starts_with('.')
            && !part.ends_with('.')
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            && !device
    };
    if name.len() > 240 || !name.split('/').all(plain) {
        return Err(format!("The download has a file whose name isn't allowed or leads out of its folder (“{name}”)"));
    }
    Ok(name.split('/').collect())
}

/// Copies `from` into `to` (at most `limit` bytes) and hashes it on the way: bytes, SHA-256 hex.
fn copy_hashed(from: &mut impl Read, to: &mut impl Write, limit: u64) -> Result<(u64, String), String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut n = 0u64;
    loop {
        let k = from.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        n += k as u64;
        if n > limit {
            return Err("it's far bigger than the add-on".into());
        }
        hasher.update(&buf[..k]);
        to.write_all(&buf[..k]).map_err(|e| e.to_string())?;
    }
    Ok((n, hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()))
}

/// Unpacks a pack into `dest` (made here), checking each file as it's written; the size unpacked.
fn unpack(zip: &[u8], dest: &Path) -> Result<u64, String> {
    let damaged = |e: zip::result::ZipError| format!("The download is damaged ({e})");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| format!("The download isn't the add-on's zip ({e})"))?;
    let manifest = {
        let entry = archive.by_name("manifest.json").map_err(|_| "The download has no manifest".to_string())?;
        let mut text = String::new();
        entry.take(1 << 20).read_to_string(&mut text).map_err(|e| format!("Its manifest can't be read ({e})"))?;
        text
    };
    let wanted = parse_manifest(&manifest)?;
    fs::create_dir_all(dest).map_err(|e| format!("Its folder couldn't be made ({e})"))?;
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(damaged)?;
        let name = entry.name().map_err(damaged)?.into_owned();
        if entry.is_dir() {
            // Folders are made for the files in them; their names are checked all the same.
            safe_path(name.trim_end_matches('/'))?;
            continue;
        }
        if name == "manifest.json" {
            continue;
        }
        let rel = safe_path(&name)?;
        if entry.is_symlink() {
            return Err(format!("“{name}” in the download is a link"));
        }
        let want = wanted.get(&name).ok_or_else(|| format!("“{name}” in the download isn't in its manifest"))?;
        if !seen.insert(name.clone()) {
            return Err(format!("“{name}” is in the download twice"));
        }
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("A folder for “{name}” couldn't be made ({e})"))?;
        }
        let mut out = fs::File::create(&path).map_err(|e| format!("“{name}” couldn't be written ({e})"))?;
        let (n, hash) = copy_hashed(&mut entry, &mut out, MAX_UNPACKED.saturating_sub(total)).map_err(|e| format!("“{name}” couldn't be unpacked ({e})"))?;
        total += n;
        if hash != *want {
            return Err(format!("“{name}” in the download is damaged (its checksum doesn't match the manifest)"));
        }
    }
    if let Some(gone) = wanted.keys().find(|k| !seen.contains(*k)) {
        return Err(format!("“{gone}” is missing from the download"));
    }
    // Last: a folder with its manifest is a whole pack.
    fs::write(dest.join("manifest.json"), &manifest).map_err(|e| format!("Its manifest couldn't be written ({e})"))?;
    Ok(total + manifest.len() as u64)
}

/// Checks an installed pack's files against its manifest again; its size.
fn check_folder(dir: &Path) -> Result<u64, String> {
    let manifest = fs::read_to_string(dir.join("manifest.json")).map_err(|_| "its manifest is missing".to_string())?;
    let wanted = parse_manifest(&manifest)?;
    let mut total = manifest.len() as u64;
    for (name, want) in &wanted {
        let mut file = fs::File::open(dir.join(safe_path(name)?)).map_err(|_| format!("“{name}” is missing"))?;
        let (n, hash) = copy_hashed(&mut file, &mut std::io::sink(), MAX_UNPACKED).map_err(|e| format!("“{name}” can't be read ({e})"))?;
        if hash != *want {
            return Err(format!("“{name}” has changed"));
        }
        total += n;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn sha(data: &[u8]) -> String {
        Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The files of a small pack: every name ocr.rs needs, with made-up contents.
    fn files() -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = REQUIRED.iter().map(|n| (n.to_string(), format!("contents of {n}\n").into_bytes())).collect();
        v.push(("NOTICE.txt".into(), b"notice\n".to_vec()));
        v
    }

    fn manifest_of(files: &[(String, Vec<u8>)]) -> String {
        let map: serde_json::Map<String, serde_json::Value> = files.iter().map(|(n, d)| (n.clone(), serde_json::Value::String(sha(d)))).collect();
        serde_json::json!({ "pack": 1, "files": map }).to_string()
    }

    /// A zip of `entries` (name, contents), with `manifest` as manifest.json when given. Stored
    /// (not compressed), so a test can change a file's bytes inside it.
    fn zip_of(entries: &[(String, Vec<u8>)], manifest: Option<&str>) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(m) = manifest {
            w.start_file("manifest.json", opts).unwrap();
            w.write_all(m.as_bytes()).unwrap();
        }
        for (name, data) in entries {
            w.start_file(name.as_str(), opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferret-addons-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_whole_pack_unpacks_and_checks_again() {
        let f = files();
        let m = manifest_of(&f);
        let dest = temp("whole");
        let size = unpack(&zip_of(&f, Some(&m)), &dest).unwrap();
        for (name, data) in &f {
            assert_eq!(&fs::read(dest.join(name)).unwrap(), data, "{name}");
        }
        assert!(dest.join("manifest.json").is_file());
        assert_eq!(size, folder_size(&dest));
        assert_eq!(check_folder(&dest), Ok(size));
        // A file changed after it was installed is caught by the check before use.
        fs::write(dest.join("rec/inference.onnx"), b"swapped").unwrap();
        assert!(check_folder(&dest).unwrap_err().contains("rec/inference.onnx"));
        fs::remove_file(dest.join("det/inference.onnx")).unwrap();
        fs::write(dest.join("rec/inference.onnx"), &f[2].1).unwrap();
        assert!(check_folder(&dest).unwrap_err().contains("missing"));
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn the_manifest_is_checked() {
        let f = files();
        assert!(parse_manifest(&manifest_of(&f)).is_ok());
        assert!(parse_manifest("not json").is_err());
        assert!(parse_manifest(&manifest_of(&f).replace("\"pack\":1", "\"pack\":2")).unwrap_err().contains("another version"));
        assert!(parse_manifest(r#"{"pack":1}"#).is_err(), "no files");
        // Every file ocr.rs loads must be in it.
        let short: Vec<_> = f.iter().filter(|(n, _)| n != "ori/inference.onnx").cloned().collect();
        assert!(parse_manifest(&manifest_of(&short)).unwrap_err().contains("ori/inference.onnx"));
        // A checksum that isn't one.
        let m = manifest_of(&f).replace(&sha(&f[0].1), "1234");
        assert!(parse_manifest(&m).unwrap_err().contains("checksum"));
        // Two names for one Windows file.
        let mut twice = f.clone();
        twice.push(("NOTICE.TXT".into(), b"x".to_vec()));
        assert!(parse_manifest(&manifest_of(&twice)).unwrap_err().contains("twice"));
        // No manifest at all, or files it doesn't list, or listed files missing.
        assert!(unpack(&zip_of(&f, None), &temp("nomanifest")).unwrap_err().contains("no manifest"));
        let mut extra = f.clone();
        extra.push(("extra.dll".into(), b"x".to_vec()));
        let dest = temp("extra");
        assert!(unpack(&zip_of(&extra, Some(&manifest_of(&f))), &dest).unwrap_err().contains("“extra.dll” in the download isn't in its manifest"));
        let dest2 = temp("missing");
        assert!(unpack(&zip_of(&f[1..], Some(&manifest_of(&f))), &dest2).unwrap_err().contains("missing"));
        let _ = fs::remove_dir_all(dest);
        let _ = fs::remove_dir_all(dest2);
    }

    #[test]
    fn names_leading_out_of_the_folder_are_refused() {
        for ok in ["onnxruntime.dll", "det/inference.onnx", "a/b/c.txt", "LICENSE-onnxruntime.txt"] {
            assert!(safe_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "../evil.dll", "det/../../evil.dll", "/abs.dll", "C:/Windows/evil.dll", "C:evil.dll", "det\\..\\evil.dll", "\\\\server\\share\\x", "a//b",
            "./x", "x/.", "file.txt:stream", "con", "CON.txt", "det/nul", "lpt1", "com9.dll", "trailing.", ".hidden", "", "space name.txt", "ü.txt",
        ] {
            assert!(safe_path(bad).is_err(), "{bad:?} was allowed");
        }
        // In the manifest, and as a zip entry the manifest doesn't even list.
        let mut f = files();
        f.push(("../evil.dll".into(), b"x".to_vec()));
        assert!(parse_manifest(&manifest_of(&f)).unwrap_err().contains("leads out of its folder"));
        let good = files();
        let dest = temp("escape");
        let err = unpack(&zip_of(&f, Some(&manifest_of(&good))), &dest).unwrap_err();
        assert!(err.contains("leads out of its folder"), "{err}");
        assert!(!dest.parent().unwrap().join("evil.dll").exists());
        let _ = fs::remove_dir_all(dest);
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let f = files();
        let m = manifest_of(&f);
        let mut bad = f.clone();
        bad[0].1 = b"not the real onnxruntime.dll".to_vec();
        let dest = temp("damaged");
        let err = unpack(&zip_of(&bad, Some(&m)), &dest).unwrap_err();
        assert!(err.contains("“onnxruntime.dll” in the download is damaged"), "{err}");
        let _ = fs::remove_dir_all(dest);
        // A byte of a file changed inside the zip (after it was made).
        let mut z = zip_of(&f, Some(&m));
        let needle = b"contents of rec/inference.onnx";
        let at = z.windows(needle.len()).position(|w| w == needle).unwrap();
        z[at + 3] ^= 0x20;
        let flipped = temp("flipped");
        assert!(unpack(&z, &flipped).is_err());
        assert!(!flipped.join("manifest.json").exists(), "a refused pack has no manifest");
        assert!(unpack(b"PK\x03\x04 not really", &temp("junk")).is_err());
        let _ = fs::remove_dir_all(flipped);
        let _ = fs::remove_dir_all(temp("junk"));
    }

    #[test]
    fn a_pack_unpacks_only_when_signed_with_the_key() {
        let z = hex(SIGNED_ZIP);
        let dest = temp("signed");
        // Signed with the (throwaway) key: installs.
        let size = install(&z, SIGNED_SIG, TEST_KEY, &dest).unwrap();
        assert_eq!(check_folder(&dest), Ok(size));
        let _ = fs::remove_dir_all(&dest);
        // Another key, a changed byte, another file's signature, no signature: refused before
        // anything is unpacked.
        for (data, sig, key) in [(&z[..], SIGNED_SIG, OTHER_KEY), (&z[..z.len() - 1], SIGNED_SIG, TEST_KEY), (&z[..], "", TEST_KEY), (&z[..], "bm90IGEgc2lnbmF0dXJl", TEST_KEY)] {
            let err = install(data, sig, key, &dest).unwrap_err();
            assert!(err.starts_with("The download was refused"), "{err}");
            assert!(!dest.exists());
        }
        let mut flipped = z.clone();
        flipped[40] ^= 1;
        assert!(install(&flipped, SIGNED_SIG, TEST_KEY, &dest).unwrap_err().starts_with("The download was refused"));
        assert!(!dest.exists());
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: Vec<u8> = s.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
        s.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect()
    }

    // A small pack (the required names, made-up contents; zipped by Windows' tar.exe, as
    // scripts/build-ocr-pack.sh zips the real one) signed with `tauri signer sign` and a throwaway
    // key made for this test, not Ferret's. OTHER_KEY is another throwaway key.
    const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQ1OTVDOUZFNjAwRDREMDMKUldRRFRRMWcvc21WUmJSZDBLdjZUdFNhZzZ1WmN5MWVydk9sQWZqNFFxd3ZFcFJDRStERTV4ZWIK";
    const OTHER_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEMwQkMyOUQ3MEM2MTcwQjcKUldTM2NHRU0xeW04d0dlLzU4MHBXUjBCeEh3aitqR2VtK0JTWkF3L3lYcVZvVXQ3U0FwZWtaVjYK";
    const SIGNED_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRRFRRMWcvc21WUlFjcUJoUU9NZU5lY0VkcThaVk5rL2thK0ZKY2FxeGhDdDBrNDdaQUNvYjBxeWhLenppU1JFYWxjNTVvS2VjSk4ra21rbWh1QkpLM3pDM1VScVFGTndVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk0MzkxMjAxCWZpbGU6dGVzdC1wYWNrLnppcApIVFlCRU9saXFaV0VRb2ZMeWhkbkQxdmpPdFp6T2ZaSjlsM3BXWWZQWTVleEJUVGliR0Rzb24rMTVIUVFtRi81djBlREE3R0NHMkl0aldtMHBnc3JCUT09Cg==";
    const SIGNED_ZIP: &str = "
        504b03041400080008007b5f6b5d0000000000000000000000000d0020006d616e69666573742e6a736f6e75780b0001
        0400000000040000000055540d00079a3cf46a9a3cf46a9a3cf46a65903b6e1b301044fb1c83b5a1703fe4926a031769
        922617d81f0121b21cc80ae0c0f0dd43d56ab9c49b37f351fea8ff2e47782aeb74ceb772fc28af97cbfbf5efe5767ac9
        439ccfe5581c38074e564025331ee24bb48f346c75ce1c7db8021bd4ba860ac062136bee03a587acf254226f5f4f9795
        d7bc781eee119b8b2c5215a3491a8de9b63a5ac0987d36e8be2f63203ba6253382366895bdee1cf44a1e9b7b4d7fe44a
        0251534f1c263152c1d025362d9c63b44eab4f69aae4e6bc3c9cbc93b339cefd2f1fb8ff5eee3350455995365138d752
        2423e54a516787fd4e88ddac4d414e582a19aeca588754dae5eebaafd7d3a36edba070db9e8d156b759ab5cd85c02018
        9e93307b179b7bcceacacd602d089dba1c76c8e6fef8f9ebfbb7e7c3edfd76e74592187bc7ca1d16da9e7381afbe2392
        7a4a546ca1838895fadcc533e6361dda76fbaee5f3f3cb7f504b0708f6f5d84d4d01000019020000504b030414000800
        08007b5f6b5d0000000000000000000000000f0020006f6e6e7872756e74696d652e646c6c75780b0001040000000004
        0000000055540d00079a3cf46a9a3cf46a9a3cf46a4b4bcc4e55c8cfcbab282acd2bc9cc4de50200504b07080a881be1
        1300000011000000504b03041400080008007b5f6b5d000000000000000000000000120020006465742f696e66657265
        6e63652e6f6e6e7875780b00010400000000040000000055540d00079a3cf46a9a3cf46a9a3cf46a4b4bcc4e5548492d
        e10200504b070824d9a6940b00000009000000504b03041400080008007b5f6b5d000000000000000000000000120020
        007265632f696e666572656e63652e6f6e6e7875780b00010400000000040000000055540d00079a3cf46a9a3cf46a9a
        3cf46a4b4bcc4e55284a4de60200504b0708f15557e40b00000009000000504b03041400080008007b5f6b5d00000000
        0000000000000000110020007265632f696e666572656e63652e796d6c75780b00010400000000040000000055540d00
        079a3cf46a9a3cf46a9a3cf46a2b4a4db652a8cccde10200504b070845768c010b00000009000000504b030414000800
        08007b5f6b5d000000000000000000000000120020006f72692f696e666572656e63652e6f6e6e7875780b0001040000
        0000040000000055540d00079a3cf46a9a3cf46a9a3cf46a4b4bcc4e55c82fcae40200504b0708cc87a2a50b00000009
        000000504b03041400080008007b5f6b5d0000000000000000000000000a0020004e4f544943452e74787475780b0001
        0400000000040000000055540d00079a3cf46a9a3cf46a9a3cf46acbcb2fc94c4ee50200504b07084aa8f54709000000
        07000000504b010214031400080008007b5f6b5df6f5d84d4d010000190200000d0018000000000000000000b6810000
        00006d616e69666573742e6a736f6e75780b00010400000000040000000055540500019a3cf46a504b01021403140008
        0008007b5f6b5d0a881be113000000110000000f0018000000000000000000b681a80100006f6e6e7872756e74696d65
        2e646c6c75780b00010400000000040000000055540500019a3cf46a504b010214031400080008007b5f6b5d24d9a694
        0b00000009000000120018000000000000000000b681180200006465742f696e666572656e63652e6f6e6e7875780b00
        010400000000040000000055540500019a3cf46a504b010214031400080008007b5f6b5df15557e40b00000009000000
        120018000000000000000000b681830200007265632f696e666572656e63652e6f6e6e7875780b000104000000000400
        00000055540500019a3cf46a504b010214031400080008007b5f6b5d45768c010b000000090000001100180000000000
        00000000b681ee0200007265632f696e666572656e63652e796d6c75780b00010400000000040000000055540500019a
        3cf46a504b010214031400080008007b5f6b5dcc87a2a50b00000009000000120018000000000000000000b681580300
        006f72692f696e666572656e63652e6f6e6e7875780b00010400000000040000000055540500019a3cf46a504b010214
        031400080008007b5f6b5d4aa8f54709000000070000000a0018000000000000000000b681c30300004e4f544943452e
        74787475780b00010400000000040000000055540500019a3cf46a504b05060000000007000700570200002404000000
        00";
}
