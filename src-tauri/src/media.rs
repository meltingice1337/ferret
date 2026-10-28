//! Video and picture compression with Windows' own codecs, so there is nothing to
//! bundle or download: the Media Foundation transcoder (hardware H.264 / HEVC) for
//! video, WIC for pictures. Formats Windows itself can't read (HEVC or HEIC without
//! Microsoft's extensions) fail with a plain explanation.
//!
//! Results are written next to the source under a temporary name and renamed into
//! place when complete, so a cancelled or failed run never leaves a half file.

use crate::jobs;
use crate::shell::ComGuard;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};
use windows::core::{HSTRING, w};
use windows::Foundation::TimeSpan;
use windows::Media::MediaProperties::{AudioEncodingProperties, MediaEncodingProfile, VideoEncodingQuality};
use windows::Media::Transcoding::{MediaTranscoder, TranscodeFailureReason};
use windows::Storage::{CreationCollisionOption, StorageFile, StorageFolder};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2, PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IStream, STATFLAG_NONAME, STREAM_SEEK_SET};
use windows::Win32::System::Variant::{VARIANT, VT_R4, VT_UI2};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows_future::{AsyncActionProgressHandler, AsyncStatus};

fn hr(e: windows::core::Error) -> String {
    let m = e.message();
    if m.is_empty() { format!("{:#010x}", e.code().0) } else { m.trim().to_string() }
}

fn file_name(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// `dest` if it's free, else `name (2).ext`, … in the same folder.
fn free_dest(dest: &str) -> Result<PathBuf, String> {
    let p = Path::new(dest);
    let dir = p.parent().ok_or("Invalid destination")?.to_string_lossy().to_string();
    let name = p.file_name().ok_or("Invalid destination")?.to_string_lossy().to_string();
    crate::fileops::validate_name(&name)?;
    Ok(Path::new(&dir).join(crate::fileops::unique_name(&dir, &name, false)))
}

/// Hidden working file next to the destination, renamed into place when done.
fn part_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    dest.with_file_name(format!("~{name}.ferret-part"))
}

fn remove_retrying(p: &Path) {
    // The transcoder releases its output a moment after it stops.
    for _ in 0..20 {
        if std::fs::remove_file(p).is_ok() || !p.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log::warn!("couldn't remove {}", p.display());
}

// ───────────────────────────── video ─────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub path: String,
    pub size: u64,
    /// Stored frame size; `rotation` says how players turn it.
    pub width: u32,
    pub height: u32,
    pub rotation: u32,
    pub duration_ms: u64,
    pub fps: f64,
    /// bits per second of the video stream (0 = unknown)
    pub video_bitrate: u32,
    pub has_audio: bool,
    pub audio_bitrate: u32,
    pub codec: String,
}

fn codec_label(subtype: &str) -> String {
    match subtype.to_ascii_uppercase().as_str() {
        "H264" | "AVC1" => "H.264".into(),
        "HEVC" | "H265" | "HVC1" | "HEV1" => "HEVC (H.265)".into(),
        "VP90" | "VP9" => "VP9".into(),
        "AV01" => "AV1".into(),
        "MPG2" | "MPEG2" => "MPEG-2".into(),
        "WMV3" | "WVC1" => "Windows Media".into(),
        "" => "Unknown".into(),
        s => s.to_string(),
    }
}

fn open_file(path: &str) -> Result<StorageFile, String> {
    StorageFile::GetFileFromPathAsync(&HSTRING::from(path))
        .and_then(|op| op.join())
        .map_err(|e| format!("Couldn't open {}: {}", file_name(path), hr(e)))
}

fn probe_video(path: &str) -> Result<VideoInfo, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    let file = open_file(path)?;
    let profile = MediaEncodingProfile::CreateFromFileAsync(&file)
        .and_then(|op| op.join())
        .map_err(|_| "Windows can't read this video. If it's HEVC (most iPhone videos), install “HEVC Video Extensions” from the Microsoft Store.".to_string())?;
    let v = profile.Video().map_err(|_| "This file has no video track".to_string())?;
    let fr = v.FrameRate().map_err(hr)?;
    let (num, den) = (fr.Numerator().unwrap_or(0), fr.Denominator().unwrap_or(1).max(1));
    let audio = profile.Audio().ok();
    let props = file.Properties().and_then(|p| p.GetVideoPropertiesAsync()).and_then(|op| op.join()).ok();
    let duration_ms = props.as_ref().and_then(|p| p.Duration().ok()).map_or(0, |d| (d.Duration / 10_000).max(0) as u64);
    let rotation = props.as_ref().and_then(|p| p.Orientation().ok()).map_or(0, |o| o.0 as u32);
    let mut video_bitrate = v.Bitrate().unwrap_or(0);
    let audio_bitrate = audio.as_ref().and_then(|a| a.Bitrate().ok()).unwrap_or(0);
    if video_bitrate == 0 && duration_ms > 0 {
        video_bitrate = ((size * 8 * 1000 / duration_ms) as u32).saturating_sub(audio_bitrate);
    }
    // The stream's size can be the coded size (HEVC pads 1206×2622 to 1208×2624);
    // the file's properties have the real one when it's close.
    let (mut width, mut height) = (v.Width().unwrap_or(0), v.Height().unwrap_or(0));
    if let Some(p) = props.as_ref() {
        let (pw, ph) = (p.Width().unwrap_or(0), p.Height().unwrap_or(0));
        if pw > 0 && ph > 0 && pw.abs_diff(width) <= 16 && ph.abs_diff(height) <= 16 {
            (width, height) = (pw, ph);
        }
    }
    Ok(VideoInfo {
        path: path.to_string(),
        size,
        width,
        height,
        rotation,
        duration_ms,
        fps: if num > 0 { num as f64 / den as f64 } else { 0.0 },
        video_bitrate,
        has_audio: audio.is_some(),
        audio_bitrate,
        codec: codec_label(&v.Subtype().map(|s| s.to_string()).unwrap_or_default()),
    })
}

#[tauri::command]
pub async fn video_info(path: String) -> Result<VideoInfo, String> {
    blocking(move || probe_video(&path)).await
}

/// A song's tags as Windows reads them (ID3, MP4, FLAC, WMA…), for the preview's details.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioInfo {
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    year: u32,
    track: u32,
    /// Bits per second.
    bitrate: u32,
    duration_ms: u64,
}

#[tauri::command]
pub async fn audio_info(path: String) -> Result<AudioInfo, String> {
    blocking(move || {
        let file = open_file(&path)?;
        let m = file.Properties().and_then(|p| p.GetMusicPropertiesAsync()).and_then(|op| op.join()).map_err(hr)?;
        let text = |r: windows::core::Result<HSTRING>| r.map(|h| h.to_string().trim().to_string()).unwrap_or_default();
        Ok(AudioInfo {
            title: text(m.Title()),
            artist: text(m.Artist()),
            album: text(m.Album()),
            album_artist: text(m.AlbumArtist()),
            year: m.Year().unwrap_or(0),
            track: m.TrackNumber().unwrap_or(0),
            bitrate: m.Bitrate().unwrap_or(0),
            duration_ms: m.Duration().map_or(0, |d| (d.Duration / 10_000).max(0) as u64),
        })
    })
    .await
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VideoJob {
    pub source: String,
    pub dest: String,
    /// Output frame size (stored orientation, even numbers).
    pub width: u32,
    pub height: u32,
    /// 0 keeps the source's frame rate.
    pub fps: u32,
    pub video_bitrate: u32,
    /// 0 drops the sound.
    pub audio_bitrate: u32,
    pub hevc: bool,
    pub trim_start_ms: Option<u64>,
    pub trim_end_ms: Option<u64>,
}

fn failure_text(r: TranscodeFailureReason, hevc: bool) -> String {
    match r {
        TranscodeFailureReason::CodecNotFound if hevc =>
            "This PC can't encode HEVC. Pick H.264 instead.".into(),
        TranscodeFailureReason::CodecNotFound =>
            "Windows has no decoder for this video. For HEVC (iPhone) videos, install “HEVC Video Extensions” from the Microsoft Store.".into(),
        TranscodeFailureReason::InvalidProfile => "Windows' encoder doesn't support these settings. Try a smaller size or H.264.".into(),
        _ => "Windows couldn't convert this video".into(),
    }
}

/// One transcode. `tick` gets the progress (0–1) about ten times a second and
/// returns false to cancel.
/// How long a transcode may go without progress before it counts as stuck.
const TRANSCODE_STALL: Duration = Duration::from_secs(15);

fn transcode(j: &VideoJob, info: &VideoInfo, dest: &Path, hardware: bool, tick: &mut dyn FnMut(f64) -> bool) -> Result<(), String> {
    let src = open_file(&j.source)?;
    let dir = dest.parent().ok_or("Invalid destination")?;
    let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(dir.as_os_str()))
        .and_then(|op| op.join())
        .map_err(|e| format!("Can't write to {}: {}", dir.display(), hr(e)))?;
    let part = part_path(dest);
    let out = folder
        .CreateFileAsync(&HSTRING::from(part.file_name().unwrap()), CreationCollisionOption::ReplaceExisting)
        .and_then(|op| op.join())
        .map_err(|e| format!("Can't create the output file: {}", hr(e)))?;

    let result = (|| -> Result<(), String> {
        let quality = VideoEncodingQuality::HD1080p;
        let profile = if j.hevc { MediaEncodingProfile::CreateHevc(quality) } else { MediaEncodingProfile::CreateMp4(quality) }.map_err(hr)?;
        let v = profile.Video().map_err(hr)?;
        v.SetWidth(j.width.max(2) & !1).map_err(hr)?;
        v.SetHeight(j.height.max(2) & !1).map_err(hr)?;
        v.SetBitrate(j.video_bitrate.max(50_000)).map_err(hr)?;
        let fr = v.FrameRate().map_err(hr)?;
        let (num, den) = if j.fps > 0 {
            (j.fps, 1)
        } else if info.fps > 0.0 {
            ((info.fps * 1000.0).round() as u32, 1000)
        } else {
            (30, 1)
        };
        fr.SetNumerator(num).map_err(hr)?;
        fr.SetDenominator(den).map_err(hr)?;
        if j.audio_bitrate == 0 || !info.has_audio {
            profile.SetAudio(None::<&AudioEncodingProperties>).map_err(hr)?;
        } else {
            profile.Audio().and_then(|a| a.SetBitrate(j.audio_bitrate)).map_err(hr)?;
        }

        let t = MediaTranscoder::new().map_err(hr)?;
        t.SetHardwareAccelerationEnabled(hardware).map_err(hr)?;
        let start = j.trim_start_ms.unwrap_or(0).min(info.duration_ms);
        if start > 0 {
            t.SetTrimStartTime(TimeSpan { Duration: start as i64 * 10_000 }).map_err(hr)?;
        }
        // Despite its documentation ("time to trim from the end"), this is where to stop.
        if let Some(end) = j.trim_end_ms.filter(|&e| e > start && e < info.duration_ms) {
            t.SetTrimStopTime(TimeSpan { Duration: end as i64 * 10_000 }).map_err(hr)?;
        }
        let prep_op = t.PrepareFileTranscodeAsync(&src, &out, &profile).map_err(hr)?;
        let asked = std::time::Instant::now();
        while prep_op.Status().map_err(hr)? == AsyncStatus::Started {
            if asked.elapsed() > TRANSCODE_STALL {
                let _ = prep_op.Cancel();
                return Err("Windows' video encoder stopped responding".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let prep = prep_op.GetResults().map_err(hr)?;
        if !prep.CanTranscode().map_err(hr)? {
            return Err(failure_text(prep.FailureReason().map_err(hr)?, j.hevc));
        }
        let op = prep.TranscodeAsync().map_err(hr)?;
        let pct = Arc::new(AtomicU64::new(0f64.to_bits()));
        let p2 = pct.clone();
        op.SetProgress(&AsyncActionProgressHandler::<f64>::new(move |_, p| {
            p2.store((*p).to_bits(), Ordering::Relaxed);
            Ok(())
        }))
        .map_err(hr)?;
        // Now and then Windows' transcoder stops making progress (seen on short samples with
        // the hardware encoder): give up after a while, so the caller retries in software
        // instead of waiting forever.
        let mut seen = pct.load(Ordering::Relaxed);
        let mut moved = std::time::Instant::now();
        loop {
            let now = pct.load(Ordering::Relaxed);
            if now != seen {
                seen = now;
                moved = std::time::Instant::now();
            } else if moved.elapsed() > TRANSCODE_STALL {
                let _ = op.Cancel();
                return Err("Windows' video encoder stopped responding".into());
            }
            match op.Status().map_err(hr)? {
                AsyncStatus::Completed => break,
                AsyncStatus::Canceled => return Err("Cancelled".into()),
                AsyncStatus::Error => {
                    let code = op.ErrorCode().map(|c| c.0).unwrap_or(0);
                    return Err(hr(windows::core::Error::from_hresult(windows::core::HRESULT(code))));
                }
                _ => {}
            }
            if !tick(f64::from_bits(pct.load(Ordering::Relaxed)) / 100.0) {
                let _ = op.Cancel();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = op.Close();
        tick(1.0);
        Ok(())
    })();
    drop(out);
    match result {
        Ok(()) => {
            std::fs::rename(&part, dest).map_err(|e| {
                remove_retrying(&part);
                format!("Couldn't save {}: {e}", dest.display())
            })?;
            Ok(())
        }
        Err(e) => {
            remove_retrying(&part);
            Err(e)
        }
    }
}

/// Starts one job converting every video in `items`; returns its id.
#[tauri::command]
pub fn compress_videos(app: AppHandle, items: Vec<VideoJob>) -> Result<u64, String> {
    if items.is_empty() {
        return Err("Nothing to convert".into());
    }
    log::info!("compress videos: {}", items.iter().map(|i| format!("\"{}\" → \"{}\" {}x{} {} b/s", i.source, i.dest, i.width, i.height, i.video_bitrate)).collect::<Vec<_>>().join(", "));
    let label = if items.len() == 1 { format!("Compressing {}", file_name(&items[0].source)) } else { format!("Compressing {} videos", items.len()) };
    Ok(jobs::spawn(&app, "media", &label, move |ctx| {
        let infos = items.iter().map(|i| probe_video(&i.source)).collect::<Result<Vec<_>, _>>()?;
        ctx.set_totals(infos.iter().map(|i| i.size).sum(), items.len() as u64);
        for (j, info) in items.iter().zip(&infos) {
            ctx.check()?;
            ctx.set_current(file_name(&j.source));
            let dest = free_dest(&j.dest)?;
            let started = std::time::Instant::now();
            // Progress is counted in bytes of the source; a retry doesn't count twice.
            let mut reported = 0u64;
            let mut tick = |f: f64| {
                let done = (f.clamp(0.0, 1.0) * info.size as f64) as u64;
                if done > reported {
                    ctx.add_bytes(done - reported);
                    reported = done;
                }
                !ctx.is_cancelled()
            };
            // Some GPUs' encoders refuse odd sizes or settings the software encoder takes.
            if let Err(e) = transcode(j, info, &dest, true, &mut tick) {
                if ctx.is_cancelled() {
                    return Err(e);
                }
                log::warn!("hardware transcode of {} failed ({e}); retrying in software", j.source);
                transcode(j, info, &dest, false, &mut tick)?;
            }
            log::info!("compressed {} in {:?}: {} → {} bytes", j.source, started.elapsed(), info.size, std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
            ctx.created(dest.to_string_lossy().to_string());
            ctx.file_done();
        }
        Ok(())
    }))
}

/// Samples the editor made; deleted when it closes.
fn sample_dir() -> PathBuf {
    std::env::temp_dir().join("ferret-media")
}

/// Bumped by every sample request: an older sample still encoding gives up.
static SAMPLE_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSample {
    pub path: String,
    pub bytes: u64,
    pub duration_ms: u64,
}

/// Encodes `length_ms` of `job.source` from `at_ms` with the job's settings, so the
/// editor can show the real quality and measure the real size. `None` when a newer
/// sample request replaced this one.
/// Progress of the sample being encoded: `media-sample-progress` events with 0–1.
#[tauri::command]
pub async fn video_sample(app: AppHandle, job: VideoJob, at_ms: u64, length_ms: u64) -> Result<Option<VideoSample>, String> {
    let generation = SAMPLE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    blocking(move || {
        let mut last = std::time::Instant::now();
        let mut tick = |f: f64| {
            if last.elapsed() >= Duration::from_millis(100) {
                last = std::time::Instant::now();
                let _ = app.emit("media-sample-progress", f);
            }
            SAMPLE_GEN.load(Ordering::SeqCst) == generation
        };
        let info = probe_video(&job.source)?;
        let start = at_ms.min(info.duration_ms.saturating_sub(500));
        let end = (start + length_ms).min(info.duration_ms);
        let dir = sample_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let dest = dir.join(format!("sample-{generation}.mp4"));
        let j = VideoJob { trim_start_ms: Some(start), trim_end_ms: Some(end), ..job };
        let res = transcode(&j, &info, &dest, true, &mut tick);
        if SAMPLE_GEN.load(Ordering::SeqCst) != generation {
            let _ = std::fs::remove_file(&dest);
            return Ok(None);
        }
        if let Err(e) = res {
            log::warn!("hardware sample failed ({e}); retrying in software");
            transcode(&j, &info, &dest, false, &mut tick)?;
        }
        let bytes = std::fs::metadata(&dest).map_err(|e| e.to_string())?.len();
        Ok(Some(VideoSample { path: dest.to_string_lossy().to_string(), bytes, duration_ms: end - start }))
    })
    .await
}

// ───────────────────────────── pictures ─────────────────────────────

/// A decoded picture, upright, as premultiplied BGRA.
struct Decoded {
    key: (String, Option<SystemTime>),
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    alpha: bool,
    format: String,
}

/// The last picture decoded: the editor re-encodes it on every settings change.
static LAST: Mutex<Option<Arc<Decoded>>> = Mutex::new(None);

fn factory() -> Result<IWICImagingFactory, String> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.map_err(hr)
}

fn container_name(g: &windows::core::GUID) -> &'static str {
    match *g {
        x if x == GUID_ContainerFormatJpeg => "JPEG",
        x if x == GUID_ContainerFormatPng => "PNG",
        x if x == GUID_ContainerFormatHeif => "HEIF",
        x if x == GUID_ContainerFormatWebp => "WebP",
        x if x == GUID_ContainerFormatBmp => "BMP",
        x if x == GUID_ContainerFormatTiff => "TIFF",
        x if x == GUID_ContainerFormatGif => "GIF",
        x if x == GUID_ContainerFormatIco => "ICO",
        x if x == GUID_ContainerFormatDds => "DDS",
        x if x == GUID_ContainerFormatWmp => "JPEG XR",
        _ => "Image",
    }
}

/// EXIF orientation (1–8) of a frame, 1 when it has none.
fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    let Ok(reader) = (unsafe { frame.GetMetadataQueryReader() }) else { return 1 };
    for q in [w!("System.Photo.Orientation"), w!("/app1/ifd/{ushort=274}"), w!("/ifd/{ushort=274}")] {
        let mut pv = PROPVARIANT::default();
        if unsafe { reader.GetMetadataByName(q, &mut pv) }.is_ok() {
            let v = unsafe {
                let inner = &pv.Anonymous.Anonymous;
                if inner.vt == VT_UI2 { Some(inner.Anonymous.uiVal) } else { None }
            };
            let _ = unsafe { PropVariantClear(&mut pv) };
            if let Some(v @ 1..=8) = v {
                return v;
            }
        }
    }
    1
}

fn transform_for(o: u16) -> WICBitmapTransformOptions {
    // WIC rotates (clockwise) first, then flips.
    match o {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

fn decode(path: &str) -> Result<Arc<Decoded>, String> {
    let key = (path.to_string(), std::fs::metadata(path).and_then(|m| m.modified()).ok());
    if let Some(d) = LAST.lock().unwrap().as_ref().filter(|d| d.key == key) {
        return Ok(d.clone());
    }
    let _com = ComGuard::init();
    let f = factory()?;
    let unreadable = |_| {
        let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "heic" | "heif" => "Windows can't read HEIC pictures without “HEIF Image Extensions” (and “HEVC Video Extensions”) from the Microsoft Store.".to_string(),
            "webp" => "Windows can't read WebP pictures without “WebP Image Extensions” from the Microsoft Store.".to_string(),
            "avif" => "Windows can't read AVIF pictures without “AV1 Video Extension” from the Microsoft Store.".to_string(),
            _ => "Windows can't read this picture".to_string(),
        }
    };
    let decoder = unsafe { f.CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand) }.map_err(unreadable)?;
    let format = container_name(&unsafe { decoder.GetContainerFormat() }.unwrap_or_default()).to_string();
    let frame = unsafe { decoder.GetFrame(0) }.map_err(unreadable)?;
    let o = orientation(&frame);
    let mut source: IWICBitmapSource = frame.cast_source()?;
    if o != 1 {
        // Decode once first: the rotator reads its source in strips, and a decoder would
        // decode the whole picture again for each one (half a minute for a phone photo).
        source = unsafe { f.CreateBitmapFromSource(&source, WICBitmapCacheOnLoad) }.map_err(unreadable)?.cast_source()?;
        let rot = unsafe { f.CreateBitmapFlipRotator() }.map_err(hr)?;
        unsafe { rot.Initialize(&source, transform_for(o)) }.map_err(hr)?;
        source = rot.cast_source()?;
    }
    let conv = unsafe { f.CreateFormatConverter() }.map_err(hr)?;
    unsafe { conv.Initialize(&source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.map_err(hr)?;
    let (mut w, mut h) = (0, 0);
    unsafe { conv.GetSize(&mut w, &mut h) }.map_err(hr)?;
    if w == 0 || h == 0 || (w as u64 * h as u64) > 200_000_000 {
        return Err("This picture is too large to process".into());
    }
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    unsafe { conv.CopyPixels(std::ptr::null(), w * 4, &mut pixels) }.map_err(unreadable)?;
    let alpha = pixels.chunks_exact(4).any(|p| p[3] != 255);
    let d = Arc::new(Decoded { key, width: w, height: h, pixels, alpha, format });
    *LAST.lock().unwrap() = Some(d.clone());
    Ok(d)
}

trait CastSource {
    fn cast_source(&self) -> Result<IWICBitmapSource, String>;
}
impl<T: windows::core::Interface> CastSource for T {
    fn cast_source(&self) -> Result<IWICBitmapSource, String> {
        self.cast().map_err(hr)
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct ImageOpts {
    /// PNG instead of JPEG.
    #[serde(default)]
    pub png: bool,
    /// Decide per picture: PNG when it has transparency, JPEG otherwise (overrides `png`).
    #[serde(default)]
    pub auto: bool,
    /// JPEG quality, 1–100.
    pub quality: u32,
    /// Longest side in pixels; 0 keeps the size.
    pub max_side: u32,
    /// PNG: down to 256 colours when the picture has more (see `encode_indexed`).
    #[serde(default)]
    pub palette: bool,
}

fn out_size(d: &Decoded, max_side: u32) -> (u32, u32) {
    let long = d.width.max(d.height);
    if max_side == 0 || long <= max_side {
        return (d.width, d.height);
    }
    let k = max_side as f64 / long as f64;
    (((d.width as f64 * k).round() as u32).max(1), ((d.height as f64 * k).round() as u32).max(1))
}

fn read_stream(s: &IStream) -> Result<Vec<u8>, String> {
    let mut st = Default::default();
    unsafe { s.Stat(&mut st, STATFLAG_NONAME) }.map_err(hr)?;
    unsafe { s.Seek(0, STREAM_SEEK_SET, None) }.map_err(hr)?;
    let mut buf = vec![0u8; st.cbSize as usize];
    let mut read = 0u32;
    unsafe { s.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut read)) }.ok().map_err(hr)?;
    buf.truncate(read as usize);
    Ok(buf)
}

/// Encodes a decoded picture; returns the file bytes and the picture's size.
fn encode(d: &Decoded, o: ImageOpts) -> Result<(Vec<u8>, u32, u32), String> {
    let _com = ComGuard::init();
    let f = factory()?;
    let (w, h) = out_size(d, o.max_side);
    let full = unsafe { f.CreateBitmapFromMemory(d.width, d.height, &GUID_WICPixelFormat32bppPBGRA, d.width * 4, &d.pixels) }.map_err(hr)?;
    let mut src: IWICBitmapSource = full.cast_source()?;
    if (w, h) != (d.width, d.height) {
        let sc = unsafe { f.CreateBitmapScaler() }.map_err(hr)?;
        unsafe { sc.Initialize(&src, w, h, WICBitmapInterpolationModeHighQualityCubic) }.map_err(hr)?;
        src = sc.cast_source()?;
    }
    if o.png {
        if let Some(bytes) = encode_indexed(&f, &src, w, h, o.palette)? {
            return Ok((bytes, w, h));
        }
    }
    // JPEG has no transparency: transparent parts become white, not black.
    let keep_alpha = o.png && d.alpha;
    if !o.png && d.alpha {
        let mut px = vec![0u8; w as usize * h as usize * 4];
        unsafe { src.CopyPixels(std::ptr::null(), w * 4, &mut px) }.map_err(hr)?;
        for p in px.chunks_exact_mut(4) {
            let a = 255 - p[3];
            p[0] = p[0].saturating_add(a);
            p[1] = p[1].saturating_add(a);
            p[2] = p[2].saturating_add(a);
            p[3] = 255;
        }
        src = unsafe { f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, &px) }.map_err(hr)?.cast_source()?;
    }
    let target = if keep_alpha { GUID_WICPixelFormat32bppBGRA } else { GUID_WICPixelFormat24bppBGR };
    let conv = unsafe { f.CreateFormatConverter() }.map_err(hr)?;
    unsafe { conv.Initialize(&src, &target, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.map_err(hr)?;

    let stream = unsafe { SHCreateMemStream(None) }.ok_or("Out of memory")?;
    let container = if o.png { GUID_ContainerFormatPng } else { GUID_ContainerFormatJpeg };
    let enc = unsafe { f.CreateEncoder(&container, std::ptr::null()) }.map_err(hr)?;
    unsafe { enc.Initialize(&stream, WICBitmapEncoderNoCache) }.map_err(hr)?;
    let mut frame = None;
    let mut bag: Option<IPropertyBag2> = None;
    unsafe { enc.CreateNewFrame(&mut frame, &mut bag) }.map_err(hr)?;
    let frame = frame.ok_or("Encoder failed")?;
    if let (false, Some(bag)) = (o.png, bag.as_ref()) {
        let name = PROPBAG2 { pstrName: windows::core::PWSTR(w!("ImageQuality").as_ptr() as *mut _), ..Default::default() };
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_R4;
            inner.Anonymous.fltVal = o.quality.clamp(1, 100) as f32 / 100.0;
            bag.Write(1, &name, &v).map_err(hr)?;
        }
    }
    unsafe { frame.Initialize(bag.as_ref()) }.map_err(hr)?;
    unsafe { frame.SetSize(w, h) }.map_err(hr)?;
    let mut fmt = target;
    unsafe { frame.SetPixelFormat(&mut fmt) }.map_err(hr)?;
    let conv_src: IWICBitmapSource = if fmt == target {
        conv.cast_source()?
    } else {
        let c2 = unsafe { f.CreateFormatConverter() }.map_err(hr)?;
        unsafe { c2.Initialize(&conv, &fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.map_err(hr)?;
        c2.cast_source()?
    };
    unsafe { frame.WriteSource(&conv_src, std::ptr::null()) }.map_err(hr)?;
    unsafe { frame.Commit() }.map_err(hr)?;
    unsafe { enc.Commit() }.map_err(hr)?;
    Ok((read_stream(&stream)?, w, h))
}

/// PNG with a palette of at most 256 colours, transparency included. Exact (lossless) when the
/// picture has that few (logos, icons, flat screenshots); with `reduce`, picked to fit the picture
/// (NeuQuant) and dithered, which is what pngquant and TinyPNG do: often a third of the size.
/// None when the picture has more colours and `reduce` is off (full colour then).
fn encode_indexed(f: &IWICImagingFactory, src: &IWICBitmapSource, w: u32, h: u32, reduce: bool) -> Result<Option<Vec<u8>>, String> {
    let conv = unsafe { f.CreateFormatConverter() }.map_err(hr)?;
    unsafe { conv.Initialize(src, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.map_err(hr)?;
    let mut px = vec![0u8; w as usize * h as usize * 4];
    unsafe { conv.CopyPixels(std::ptr::null(), w * 4, &mut px) }.map_err(hr)?;
    let (colors, indices) = match exact_palette(&px) {
        Some(found) => found,
        None if reduce => quantize(&px, w as usize, h as usize),
        None => return Ok(None),
    };
    let palette = unsafe { f.CreatePalette() }.map_err(hr)?;
    unsafe { palette.InitializeCustom(&colors) }.map_err(hr)?;
    let bmp = unsafe { f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat8bppIndexed, w, &indices) }.map_err(hr)?;
    unsafe { bmp.SetPalette(&palette) }.map_err(hr)?;

    let stream = unsafe { SHCreateMemStream(None) }.ok_or("Out of memory")?;
    let enc = unsafe { f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()) }.map_err(hr)?;
    unsafe { enc.Initialize(&stream, WICBitmapEncoderNoCache) }.map_err(hr)?;
    let mut frame = None;
    let mut bag: Option<IPropertyBag2> = None;
    unsafe { enc.CreateNewFrame(&mut frame, &mut bag) }.map_err(hr)?;
    let frame = frame.ok_or("Encoder failed")?;
    unsafe { frame.Initialize(bag.as_ref()) }.map_err(hr)?;
    unsafe { frame.SetSize(w, h) }.map_err(hr)?;
    let mut fmt = GUID_WICPixelFormat8bppIndexed;
    unsafe { frame.SetPixelFormat(&mut fmt) }.map_err(hr)?;
    if fmt != GUID_WICPixelFormat8bppIndexed {
        return Ok(None);
    }
    unsafe { frame.SetPalette(&palette) }.map_err(hr)?;
    unsafe { frame.WriteSource(&bmp.cast_source()?, std::ptr::null()) }.map_err(hr)?;
    unsafe { frame.Commit() }.map_err(hr)?;
    unsafe { enc.Commit() }.map_err(hr)?;
    Ok(Some(read_stream(&stream)?))
}

/// The picture's colours (WIC's ARGB, which is BGRA bytes read as one number) and each pixel's
/// index into them, if there are at most 256. Fully transparent pixels count as one colour.
fn exact_palette(bgra: &[u8]) -> Option<(Vec<u32>, Vec<u8>)> {
    let mut seen: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
    let mut colors: Vec<u32> = Vec::new();
    let mut indices = Vec::with_capacity(bgra.len() / 4);
    let mut last: Option<(u32, u8)> = None;
    for p in bgra.chunks_exact(4) {
        let c = if p[3] == 0 { 0 } else { u32::from_le_bytes([p[0], p[1], p[2], p[3]]) };
        let k = match last {
            Some((lc, lk)) if lc == c => lk,
            _ => match seen.get(&c) {
                Some(&k) => k,
                None if colors.len() == 256 => return None,
                None => {
                    colors.push(c);
                    seen.insert(c, (colors.len() - 1) as u8);
                    (colors.len() - 1) as u8
                }
            },
        };
        last = Some((c, k));
        indices.push(k);
    }
    Some((colors, indices))
}

/// 256 colours for a picture with more (NeuQuant), and each pixel's index, with Floyd–Steinberg
/// dithering (damped a little, as pngquant does) so gradients and soft shadows don't band. It
/// works on premultiplied colour, as pngquant does, so how see-through a pixel is counts for more
/// than the colour of what barely shows. Opaque and see-through pixels get colours of their own
/// (and one clear entry): opaque ones stay opaque, and soft edges get enough shades of alpha. For
/// see-through ones colour counts for a third (SOFT): otherwise an even veil, all one alpha, comes
/// out in whichever entry matches its colour best, however see-through that one is.
fn quantize(bgra: &[u8], w: usize, h: usize) -> (Vec<u32>, Vec<u8>) {
    use color_quant::NeuQuant;
    const SOFT: u8 = 3;
    let pm: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| {
        let k = |c: u8| ((c as u32 * p[3] as u32 + 127) / 255) as u8;
        [k(p[0]), k(p[1]), k(p[2]), p[3]]
    }).collect();
    let (mut solid, mut soft) = (Vec::new(), Vec::new());
    for p in pm.chunks_exact(4) {
        match p[3] {
            255 => solid.extend_from_slice(p),
            0 => {}
            _ => soft.extend_from_slice(&[p[0] / SOFT, p[1] / SOFT, p[2] / SOFT, p[3]]),
        }
    }
    let (ns, nf) = (solid.len() / 4, soft.len() / 4);
    // A see-through pixel varies in alpha too, so it gets about three times an opaque one's share.
    let soft_n = if nf == 0 { 0 } else if ns == 0 { 255 } else { (255 * nf * 3 / (ns + nf * 3)).clamp(16, 160) };
    let nets = [(soft_n < 255).then(|| NeuQuant::new(10, 255 - soft_n, &solid)), (soft_n > 0).then(|| NeuQuant::new(10, soft_n, &soft))];
    let mut maps: Vec<Vec<u8>> = nets.iter().map(|n| n.as_ref().map(|n| n.color_map_rgba()).unwrap_or_default()).collect();
    maps[0].chunks_exact_mut(4).for_each(|c| c[3] = 255);
    maps[1].chunks_exact_mut(4).for_each(|c| {
        c[3] = c[3].clamp(1, 254);
        for ch in 0..3 {
            c[ch] = (c[ch] as u32 * SOFT as u32).min(c[3] as u32) as u8;
        }
    });
    let offset = [0, maps[0].len() / 4];
    let clear = (maps[0].len() + maps[1].len()) / 4;
    let colors = maps.concat().chunks_exact(4).map(|c| {
        let k = |v: u8| ((v as u32 * 255 + c[3] as u32 / 2) / c[3] as u32).min(255) as u8;
        u32::from_le_bytes([k(c[0]), k(c[1]), k(c[2]), c[3]])
    }).chain(Some(0)).collect();
    let mut indices = vec![0u8; w * h];
    let mut cur = vec![[0f32; 4]; w + 2];
    let mut next = vec![[0f32; 4]; w + 2];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let a = pm[i + 3];
            if a == 0 {
                indices[y * w + x] = clear as u8;
                continue; // nothing shows, so no error to carry
            }
            let e = cur[x + 1];
            let mut want = [0u8; 4];
            for ch in 0..4 {
                want[ch] = (pm[i + ch] as f32 + e[ch]).round().clamp(0.0, 255.0) as u8;
            }
            let set = (a < 255) as usize;
            want[3] = if set == 0 { 255 } else { want[3].clamp(1, 254) };
            // Premultiplied colour can't be brighter than its alpha.
            for ch in 0..3 {
                want[ch] = want[ch].min(want[3]);
            }
            let key = if set == 0 { want } else { [want[0] / SOFT, want[1] / SOFT, want[2] / SOFT, want[3]] };
            let k = nets[set].as_ref().map(|n| n.index_of(&key)).unwrap_or(0);
            indices[y * w + x] = (offset[set] + k) as u8;
            let got = &maps[set][k * 4..k * 4 + 4];
            for ch in 0..4 {
                let d = (want[ch] as f32 - got[ch] as f32) * 0.875;
                cur[x + 2][ch] += d * 7.0 / 16.0;
                next[x][ch] += d * 3.0 / 16.0;
                next[x + 1][ch] += d * 5.0 / 16.0;
                next[x + 2][ch] += d / 16.0;
            }
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|e| *e = [0.0; 4]);
    }
    (colors, indices)
}

fn data_url(bytes: &[u8], png: bool) -> String {
    format!("data:image/{};base64,{}", if png { "png" } else { "jpeg" }, base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub path: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub format: String,
    /// The original, upright, at most 2048 px (WebView2 can't show HEIC and friends).
    pub preview: String,
}

#[tauri::command]
pub async fn image_info(path: String) -> Result<ImageInfo, String> {
    blocking(move || {
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        let d = decode(&path)?;
        let (bytes, ..) = encode(&d, ImageOpts { png: d.alpha, auto: false, quality: 92, max_side: 2048, palette: false })?;
        Ok(ImageInfo { path: path.clone(), size, width: d.width, height: d.height, alpha: d.alpha, format: d.format.clone(), preview: data_url(&bytes, d.alpha) })
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePreview {
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub data_url: String,
}

/// The picture exactly as "Compress" would save it, and its size in bytes.
#[tauri::command]
pub async fn image_preview(path: String, opts: ImageOpts) -> Result<ImagePreview, String> {
    blocking(move || {
        let d = decode(&path)?;
        let opts = if opts.auto { ImageOpts { png: d.alpha, ..opts } } else { opts };
        let (bytes, width, height) = encode(&d, opts)?;
        Ok(ImagePreview { bytes: bytes.len() as u64, width, height, data_url: data_url(&bytes, opts.png) })
    })
    .await
}

#[derive(Deserialize)]
pub struct ImageJob {
    pub source: String,
    pub dest: String,
}

/// Starts one job compressing every picture in `items` with the same settings; returns its id.
#[tauri::command]
pub fn compress_images(app: AppHandle, items: Vec<ImageJob>, opts: ImageOpts) -> Result<u64, String> {
    if items.is_empty() {
        return Err("Nothing to compress".into());
    }
    log::info!("compress {} pictures (png {}, q {}, max {})", items.len(), opts.png, opts.quality, opts.max_side);
    let label = if items.len() == 1 { format!("Compressing {}", file_name(&items[0].source)) } else { format!("Compressing {} pictures", items.len()) };
    Ok(jobs::spawn(&app, "media", &label, move |ctx| {
        let sizes: Vec<u64> = items.iter().map(|i| std::fs::metadata(&i.source).map(|m| m.len()).unwrap_or(0)).collect();
        ctx.set_totals(sizes.iter().sum(), items.len() as u64);
        for (i, size) in items.iter().zip(sizes) {
            ctx.check()?;
            ctx.set_current(file_name(&i.source));
            let d = decode(&i.source)?;
            let o = if opts.auto { ImageOpts { png: d.alpha, ..opts } } else { opts };
            let (bytes, ..) = encode(&d, o)?;
            // "Auto" picks the format here, so the name's extension follows it.
            let dest = if opts.auto {
                free_dest(&Path::new(&i.dest).with_extension(if o.png { "png" } else { "jpg" }).to_string_lossy())?
            } else {
                free_dest(&i.dest)?
            };
            let part = part_path(&dest);
            std::fs::write(&part, &bytes).map_err(|e| format!("Couldn't save {}: {e}", dest.display()))?;
            if let Err(e) = std::fs::rename(&part, &dest) {
                let _ = std::fs::remove_file(&part);
                return Err(format!("Couldn't save {}: {e}", dest.display()));
            }
            ctx.created(dest.to_string_lossy().to_string());
            ctx.add_bytes(size);
            ctx.file_done();
        }
        // Don't hold a big decoded picture once the editor is closed.
        *LAST.lock().unwrap() = None;
        Ok(())
    }))
}

/// The editor closed: frees its decoded picture and deletes its video samples.
#[tauri::command]
pub fn media_release() {
    *LAST.lock().unwrap() = None;
    SAMPLE_GEN.fetch_add(1, Ordering::SeqCst);
    let dir = sample_dir();
    std::thread::spawn(move || {
        // A sample that was still encoding lets go of its file shortly.
        std::thread::sleep(Duration::from_millis(1500));
        let _ = std::fs::remove_dir_all(dir);
    });
}

#[cfg(test)]
mod tests {
    //! Run against fixtures in `%USERPROFILE%\FerretQA\media` (made with ffmpeg / WPF);
    //! each test is skipped when its fixture is missing.
    use super::*;

    fn fixture(name: &str) -> Option<String> {
        let p = dirs::home_dir()?.join("FerretQA").join("media").join(name);
        p.exists().then(|| p.to_string_lossy().to_string())
    }

    fn job(source: &str, info: &VideoInfo, width: u32, height: u32) -> VideoJob {
        VideoJob {
            source: source.into(),
            dest: String::new(),
            width,
            height,
            fps: 30,
            video_bitrate: 1_000_000,
            audio_bitrate: 96_000,
            hevc: false,
            trim_start_ms: None,
            trim_end_ms: Some(info.duration_ms.min(3000)),
        }
    }

    #[test]
    fn video_probe_and_trimmed_transcode() {
        let Some(src) = fixture("landscape-1080p.mp4") else { return };
        let info = probe_video(&src).unwrap();
        assert_eq!((info.width, info.height, info.rotation), (1920, 1080, 0));
        assert!(info.has_audio && (11_500..12_500).contains(&info.duration_ms), "{}", info.duration_ms);
        assert!((info.fps - 30.0).abs() < 0.1);
        let dir = std::env::temp_dir().join("ferret-media-test");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("landscape.mp4");
        let _ = std::fs::remove_file(&dest);
        let mut last = 0.0;
        let mut j = job(&src, &info, 1280, 720);
        (j.trim_start_ms, j.trim_end_ms) = (Some(2000), Some(5000));
        transcode(&j, &info, &dest, true, &mut |f| { last = f; true }).unwrap();
        assert_eq!(last, 1.0);
        let out = probe_video(&dest.to_string_lossy()).unwrap();
        assert_eq!((out.width, out.height), (1280, 720));
        assert!((2_800..3_300).contains(&out.duration_ms), "trimmed to {} ms", out.duration_ms);
        assert!(out.has_audio);
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn rotated_video_comes_out_upright_and_cancel_cleans_up() {
        let Some(src) = fixture("portrait-rotated.mp4") else { return };
        let info = probe_video(&src).unwrap();
        assert_eq!((info.width, info.height), (1920, 1080));
        assert!(info.rotation == 90 || info.rotation == 270, "rotation {}", info.rotation);
        let dir = std::env::temp_dir().join("ferret-media-test");
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("portrait.mp4");
        let _ = std::fs::remove_file(&dest);
        // Size is given in stored orientation; Windows turns the picture upright.
        let mut j = job(&src, &info, 1280, 720);
        j.audio_bitrate = 0;
        transcode(&j, &info, &dest, true, &mut |_| true).unwrap();
        let out = probe_video(&dest.to_string_lossy()).unwrap();
        assert_eq!((out.width, out.height, out.rotation), (720, 1280, 0));
        assert!(!out.has_audio, "muted output has no sound track");

        let cancelled = dir.join("cancelled.mp4");
        let err = transcode(&job(&src, &info, 1280, 720), &info, &cancelled, true, &mut |_| false).unwrap_err();
        assert_eq!(err, "Cancelled");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!cancelled.exists() && !part_path(&cancelled).exists());
    }

    #[test]
    fn exif_orientation_is_applied() {
        let Some(src) = fixture("photo-exif-rotated.jpg") else { return };
        let d = decode(&src).unwrap();
        // Stored 3200×2400 on its side, tagged to be shown upright.
        assert_eq!((d.width, d.height, d.alpha), (2400, 3200, false));
        let (bytes, w, h) = encode(&d, ImageOpts { png: false, auto: false, quality: 70, max_side: 1600, palette: false }).unwrap();
        assert_eq!((w, h), (1200, 1600));
        assert!(bytes.starts_with(&[0xFF, 0xD8]) && bytes.len() < std::fs::metadata(&src).unwrap().len() as usize);
        // Lower quality → smaller file.
        let (small, ..) = encode(&d, ImageOpts { png: false, auto: false, quality: 30, max_side: 1600, palette: false }).unwrap();
        assert!(small.len() < bytes.len());
    }

    #[test]
    fn transparency_kept_in_png_and_white_in_jpeg() {
        let Some(src) = fixture("logo-transparent.png") else { return };
        let d = decode(&src).unwrap();
        assert!(d.alpha);
        let (png, ..) = encode(&d, ImageOpts { png: true, auto: false, quality: 0, max_side: 0, palette: false }).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let (jpg, w, h) = encode(&d, ImageOpts { png: false, auto: false, quality: 90, max_side: 0, palette: false }).unwrap();
        assert_eq!((w, h), (1600, 1000));
        // Decode the JPEG back: the transparent corner must be white, not black.
        let _com = ComGuard::init();
        let f = factory().unwrap();
        let stream = unsafe { SHCreateMemStream(Some(&jpg)) }.unwrap();
        let dec = unsafe { f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand) }.unwrap();
        let frame = unsafe { dec.GetFrame(0) }.unwrap();
        let mut px = [0u8; 4];
        let conv = unsafe { f.CreateFormatConverter() }.unwrap();
        unsafe { conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.unwrap();
        let rc = WICRect { X: 0, Y: h as i32 - 1, Width: 1, Height: 1 };
        unsafe { conv.CopyPixels(&rc, 4, &mut px) }.unwrap();
        assert!(px[0] > 240 && px[1] > 240 && px[2] > 240, "corner {px:?}");
    }
    /// BGRA (straight alpha) of an encoded picture.
    fn pixels_of(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
        let _com = ComGuard::init();
        let f = factory().unwrap();
        let stream = unsafe { SHCreateMemStream(Some(bytes)) }.unwrap();
        let dec = unsafe { f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand) }.unwrap();
        let frame = unsafe { dec.GetFrame(0) }.unwrap();
        let (mut w, mut h) = (0, 0);
        unsafe { frame.GetSize(&mut w, &mut h) }.unwrap();
        let conv = unsafe { f.CreateFormatConverter() }.unwrap();
        unsafe { conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom) }.unwrap();
        let mut px = vec![0u8; (w * h * 4) as usize];
        unsafe { conv.CopyPixels(std::ptr::null(), w * 4, &mut px) }.unwrap();
        (px, w, h)
    }

    /// A PNG with few colours keeps them exactly in a palette; one with many goes down to 256
    /// colours when asked, smaller, with its soft transparency kept.
    #[test]
    fn palette_pngs() {
        let full = ImageOpts { png: true, auto: false, quality: 0, max_side: 0, palette: false };
        // Three colours and transparency: exact either way.
        let (w, h) = (64u32, 48u32);
        let flat: Vec<u8> = (0..w * h).flat_map(|i| match i % 3 { 0 => [0, 0, 255, 255], 1 => [0, 128, 0, 255], _ => [0, 0, 0, 0] }).collect();
        let d = Decoded { key: (String::new(), None), width: w, height: h, pixels: flat.clone(), alpha: true, format: "PNG".into() };
        let (bytes, ..) = encode(&d, full).unwrap();
        assert_eq!(pixels_of(&bytes).0, flat, "exact palette is lossless");

        let Some(src) = fixture("logo-transparent.png") else { return };
        let d = decode(&src).unwrap();
        let (big, ..) = encode(&d, full).unwrap();
        let t = std::time::Instant::now();
        let (small, w, h) = encode(&d, ImageOpts { palette: true, ..full }).unwrap();
        println!("logo: full colour {} KB, 256 colours {} KB in {:?}", big.len() / 1024, small.len() / 1024, t.elapsed());
        assert!(small.len() * 10 < big.len() * 7, "at least 30% smaller");
        let (a, ..) = pixels_of(&big);
        let (b, ..) = pixels_of(&small);
        let soft = a.chunks_exact(4).filter(|p| p[3] > 0 && p[3] < 255).count();
        let soft_kept = b.chunks_exact(4).filter(|p| p[3] > 0 && p[3] < 255).count();
        assert!(soft == 0 || soft_kept * 2 > soft, "soft edges kept: {soft_kept} of {soft}");
        // Opaque and clear pixels stay so; colour and (over 4 × 4 blocks, as dithering spreads
        // it) alpha come out close.
        let (mut err, mut n) = (0u64, 0u64);
        for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            assert!(!(p[3] == 255 || p[3] == 0) || p[3] == q[3], "alpha {} → {}", p[3], q[3]);
            if p[3] == 255 {
                err += (0..3).map(|c| (p[c] as i32 - q[c] as i32).unsigned_abs() as u64).sum::<u64>();
                n += 3;
            }
        }
        let (w, h) = (w as usize, h as usize);
        let block = |px: &[u8], bx: usize, by: usize| (0..16).map(|k| px[((by * 4 + k / 4) * w + bx * 4 + k % 4) * 4 + 3] as i32).sum::<i32>() / 16;
        let (mut blocks, mut off, mut far, mut worst) = (0, 0, 0, 0);
        for by in 0..h / 4 {
            for bx in 0..w / 4 {
                let d = (block(&a, bx, by) - block(&b, bx, by)).abs();
                blocks += 1;
                off += (d > 12) as usize;
                far += (d > 24) as usize;
                worst = worst.max(d);
            }
        }
        println!("  mean colour error {:.2}; 4 × 4 blocks with alpha off by > 12: {off}, > 24: {far} of {blocks} (worst {worst})", err as f64 / n.max(1) as f64);
        assert!(err < n * 8 && off * 100 < blocks && far * 1000 < blocks);
    }

    #[test]
    fn rotated_photo_decodes_quickly() {
        // Rotating straight off the decoder took ~28 s for this 7.7 MP picture (debug build).
        let Some(src) = fixture("photo-exif-rotated.jpg") else { return };
        *LAST.lock().unwrap() = None;
        let t = std::time::Instant::now();
        let d = decode(&src).unwrap();
        encode(&d, ImageOpts { png: false, auto: false, quality: 82, max_side: 2560, palette: false }).unwrap();
        assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
    }
}
