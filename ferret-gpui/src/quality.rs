//! The compress editor's quality check, as the app does it (src/lib/quality.ts and checkSample in
//! MediaEditor.tsx): frames of the sample against the same moments of the original, compared by
//! SSIM on brightness at about 960 px wide, so grain the encoder smoothed away doesn't count while
//! blocks and smearing do. The worst of 8 frames decides.
//!
//! Frames are read with Media Foundation's source reader (decoded, converted to RGB), on a
//! background thread.

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFMediaType, IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL,
    MFMediaType_Video, MFSTARTUP_FULL, MFStartup, MFVideoFormat_RGB32, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::core::{GUID, HSTRING, Interface};

/// Brightness at a reduced size.
pub struct Luma {
    pub y: Vec<f32>,
    pub w: usize,
    pub h: usize,
}

/// `b` with its brightness curve matched to `a`'s: for each of 32 levels of `b`, the average of
/// `a` where `b` is at that level (kept rising), applied with interpolation. A shift in levels,
/// contrast or a lifted black (another range or transfer curve) then doesn't count, while detail
/// lost to blur, blocks or banding still does. Without it a dark screen whose black came out at
/// 4 instead of 0 scored 0.30, "Poor", with nothing blurred (user report, 2026-11-10).
pub fn tone_matched(a: &Luma, b: &Luma) -> Luma {
    const BINS: usize = 32;
    let (w, h) = (a.w.min(b.w), a.h.min(b.h));
    let width = 256. / BINS as f32;
    let bin = |v: f32| ((v.clamp(0., 255.) / width) as usize).min(BINS - 1);
    let (mut sum, mut n) = ([0f64; BINS], [0u32; BINS]);
    for y in 0..h {
        for x in 0..w {
            let i = bin(b.y[y * b.w + x]);
            sum[i] += a.y[y * a.w + x] as f64;
            n[i] += 1;
        }
    }
    // Levels with too few pixels to trust keep their own value, moved as the nearest trusted one.
    let total: u32 = n.iter().sum();
    let enough = (total / 10_000).max(8);
    let known: Vec<usize> = (0..BINS).filter(|&i| n[i] >= enough).collect();
    if known.is_empty() {
        return Luma { y: b.y.clone(), w: b.w, h: b.h };
    }
    let center = |i: usize| (i as f32 + 0.5) * width;
    let mut lut = [0f32; BINS];
    for i in 0..BINS {
        let k = *known.iter().min_by_key(|&&k| k.abs_diff(i)).unwrap();
        let mean = (sum[k] / n[k] as f64) as f32;
        lut[i] = if k == i { mean } else { center(i) + (mean - center(k)) };
    }
    for i in 1..BINS {
        lut[i] = lut[i].max(lut[i - 1]);
    }
    let y = b
        .y
        .iter()
        .map(|&v| {
            let p = (v.clamp(0., 255.) / width - 0.5).max(0.);
            let i0 = (p as usize).min(BINS - 1);
            let i1 = (i0 + 1).min(BINS - 1);
            let t = (p - i0 as f32).clamp(0., 1.);
            lut[i0] * (1. - t) + lut[i1] * t
        })
        .collect();
    Luma { y, w: b.w, h: b.h }
}

/// Mean SSIM over 8 × 8 windows: 1 is identical (quality.ts).
pub fn ssim(a: &Luma, b: &Luma) -> f64 {
    let (c1, c2) = ((0.01f64 * 255.).powi(2), (0.03f64 * 255.).powi(2));
    let (w, h) = (a.w.min(b.w), a.h.min(b.h));
    let (mut sum, mut n) = (0f64, 0usize);
    let mut by = 0;
    while by + 8 <= h {
        let mut bx = 0;
        while bx + 8 <= w {
            let (mut ma, mut mb, mut va, mut vb, mut cv) = (0f64, 0f64, 0f64, 0f64, 0f64);
            for y in by..by + 8 {
                for x in bx..bx + 8 {
                    let (pa, pb) = (a.y[y * a.w + x] as f64, b.y[y * b.w + x] as f64);
                    ma += pa;
                    mb += pb;
                    va += pa * pa;
                    vb += pb * pb;
                    cv += pa * pb;
                }
            }
            ma /= 64.;
            mb /= 64.;
            va = va / 64. - ma * ma;
            vb = vb / 64. - mb * mb;
            cv = cv / 64. - ma * mb;
            sum += ((2. * ma * mb + c1) * (2. * cv + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            n += 1;
            bx += 8;
        }
        by += 8;
    }
    if n == 0 { 1. } else { sum / n as f64 }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Same,
    Good,
    Ok,
    Poor,
}

/// What the worst frame's score means for watching the video (quality.ts).
pub fn verdict(score: f64) -> (Level, &'static str) {
    if score >= 0.985 {
        (Level::Same, "Looks the same as the original")
    } else if score >= 0.965 {
        (Level::Good, "Good: a little softer, easy to watch")
    } else if score >= 0.94 {
        (Level::Ok, "Watchable, but softer, with some smearing")
    } else {
        (Level::Poor, "Poor: visible blocks or smearing")
    }
}

/// The window's own graphics device, for the check's readers to decode on (as the players do).
///
/// Windows' software decoder decodes far ahead of what's read and keeps every frame: the check,
/// which reads a few seconds of the original and of the sample straight through, had some 250
/// decoded frames in memory at once (770 MB at 1080p; opening the editor peaked at 1.1 GB for a
/// moment, and four times that at 4K). The graphics card keeps its frames in its own memory, a
/// handful at a time, and only the frames compared are copied out. What it decodes is the same
/// picture, bit for bit (FERRET_GPUI_CHECK_BOTH=1 runs the check both ways and logs how far the
/// scores are apart). Where it can't (a size or format the card doesn't decode, a device lost
/// meanwhile), the check runs in software as before.
pub struct Gpu(windows::Win32::Graphics::Direct3D11::ID3D11Device);

// SAFETY: a Direct3D 11 device may be used from any thread, and GPUI's is made with multithread
// protection on (Media Foundation already decodes the players' video on it from its own threads).
unsafe impl Send for Gpu {}

/// The window's device, for a video of `width` × `height` whose frames are worth decoding on it:
/// bigger than 1080p. Up to there the processor is quicker (a 1080p check takes 0.5–0.8 s on it
/// and 1.2 s on the card, measured) and what its decoder piles up passes in under a second; at
/// 4K the card is as quick (1.5 s either way, 1.8 against 2.1 for HEVC at 60 fps) and the
/// editor's open peaks 700–860 MB lower. On the UI thread only (GPUI's backend knows the device
/// there); None before a window. Tests comparing the two: FERRET_GPUI_CHECK_SOFTWARE=1 never
/// uses the card, FERRET_GPUI_CHECK_CARD=1 always.
pub fn gpu_for(width: u32, height: u32) -> Option<Gpu> {
    let set = |name: &str| std::env::var_os(name).is_some();
    if set("FERRET_GPUI_CHECK_SOFTWARE") || (width as u64 * height as u64 <= 1920 * 1088 && !set("FERRET_GPUI_CHECK_CARD")) {
        return None;
    }
    let (device, context) = gpui_windows::ferret::shared_device()?;
    // SAFETY: each pointer carries a reference this now owns (`shared_device`).
    unsafe {
        drop(windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext::from_raw(context));
        Some(Gpu(windows::Win32::Graphics::Direct3D11::ID3D11Device::from_raw(device)))
    }
}

/// "Not on the graphics card": the caller reads in software.
fn not_this_way() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
}

/// Reads frames of one video: decoded RGB, by time.
struct Reader {
    reader: IMFSourceReader,
    w: usize,
    h: usize,
    /// Bytes from one row to the next (negative: bottom-up), for buffers without 2D access.
    stride: i32,
    /// Diagnostics: the row spacing used for a plain buffer (0: its rows came through 2D access).
    path: std::cell::Cell<isize>,
    /// Diagnostics: a frame came from the graphics card's memory (asked to decode there, Windows
    /// may still decode in software: a size or format the card doesn't take).
    on_card: std::cell::Cell<bool>,
    /// The picture inside the decoded frame (x, y, w, h): decoders round the frame up to their
    /// block size and say what's shown (a phone's 1206 × 2622 recording decodes as 1216 × 2624).
    /// Comparing the whole frame squeezed the original about 1% against a compressed copy made
    /// from the picture alone, and every screen recording scored "Poor" (user report, 2026-11-10).
    crop: (usize, usize, usize, usize),
    /// Brightness read straight from the decoded picture (NV12), not after Windows turned it into
    /// RGB, and `full_range`: the file says its brightness runs 0–255 rather than 16–235.
    /// Windows' HEVC decoder labels a phone's full-range video 16–235, so its own conversion to
    /// RGB crushed the blacks and clipped the whites of every original the check looked at
    /// (2026-11-11); the decoded values themselves are right, read by the file's own range.
    raw: bool,
    full_range: bool,
    /// YUV to RGB with BT.601's matrix (else BT.709's), for `nv12_bgra`.
    bt601: bool,
}

/// The shown part of a frame of `w` × `h`, from a media type's display aperture.
fn aperture(t: &IMFMediaType, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    use windows::Win32::Media::MediaFoundation::{MF_MT_GEOMETRIC_APERTURE, MF_MT_MINIMUM_DISPLAY_APERTURE, MFVideoArea};
    for key in [&MF_MT_MINIMUM_DISPLAY_APERTURE, &MF_MT_GEOMETRIC_APERTURE] {
        let mut area = MFVideoArea::default();
        let mut got = 0u32;
        let bytes = unsafe { std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, std::mem::size_of::<MFVideoArea>()) };
        if unsafe { t.GetBlob(key, bytes, Some(&mut got)) }.is_err() || got as usize != std::mem::size_of::<MFVideoArea>() {
            continue;
        }
        let (x, y) = (area.OffsetX.value.max(0) as usize, area.OffsetY.value.max(0) as usize);
        let (aw, ah) = (area.Area.cx.max(0) as usize, area.Area.cy.max(0) as usize);
        if aw > 0 && ah > 0 && x + aw <= w && y + ah <= h {
            return Some((x, y, aw, ah));
        }
    }
    None
}

impl Reader {
    /// Frames as RGB (the filmstrip's pictures). With `gpu`, decoded by the graphics card and
    /// turned into RGB here (`nv12_bgra`); an error then means "not this way".
    fn open(path: &str, gpu: Option<&Gpu>) -> windows::core::Result<Self> {
        Self::open_as(path, false, gpu)
    }

    /// Frames as brightness, read from the decoded picture by the file's own range (the check).
    /// With `gpu`, decoded by the graphics card (see `Gpu`); an error then means "not this way".
    fn open_luma(path: &str, gpu: Option<&Gpu>) -> windows::core::Result<Self> {
        Self::open_as(path, true, gpu)
    }

    fn open_as(path: &str, raw: bool, gpu: Option<&Gpu>) -> windows::core::Result<Self> {
        use windows::Win32::Media::MediaFoundation::{
            MF_LOW_LATENCY, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_SOURCE_READER_D3D_MANAGER, MFCreateDXGIDeviceManager, MFVideoFormat_NV12,
            MFVideoTransferMatrix_BT601,
        };
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let open = |low_latency: bool| -> windows::core::Result<IMFSourceReader> {
            unsafe {
                let mut attrs = None;
                MFCreateAttributes(&mut attrs, 2)?;
                let attrs = attrs.unwrap();
                match gpu {
                    // The graphics card decodes, into its own memory (the reader keeps the manager).
                    Some(gpu) => {
                        let (mut token, mut manager) = (0u32, None);
                        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                        let manager = manager.ok_or_else(not_this_way)?;
                        manager.ResetDevice(&gpu.0, token)?;
                        attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &manager)?;
                    }
                    // Lets the reader convert to RGB (or to NV12 from another decoded format).
                    None => attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?,
                }
                if low_latency {
                    attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
                }
                let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path), &attrs)?;
                reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
                reader.SetStreamSelection(video, true)?;
                Ok(reader)
            }
        };
        unsafe {
            // The filmstrip's decoder works in low-latency mode: it hands over each frame as it's
            // decoded. Otherwise Windows' decoder decodes far ahead of what's read and keeps it
            // all: every seek waited for those frames (18 s instead of 0.4 for 22 tiles of a
            // 103 s 60 fps recording) and they took some 400 MB for a moment at 1080p.
            // Not the check's readers: they read a few seconds straight through, where that mode
            // is up to 4.5 times slower (one thread decodes; measured 4.5 s against 1.0). What
            // their decoder holds ahead (some 250 frames, 770 MB at 1080p) is why the check
            // decodes on the graphics card when it can (`Gpu`).
            let reader = open(!raw)?;
            // The range the file says it has (not the decoder's label: see `raw`). Unsaid is
            // 16–235, as video usually is.
            let native = reader.GetNativeMediaType(video, 0).ok();
            let full_range = native.as_ref().and_then(|n| n.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE).ok()) == Some(1);
            // The filmstrip's pictures of a full-range video are read as NV12 too and turned into
            // RGB here (`nv12_bgra`): Windows' conversion trusts the decoder's label and crushed
            // them (stored 0, 8, 16 all black, 28 as 14, 235 and 255 both white). The graphics
            // card's frames come as NV12 anyway.
            let raw = raw || full_range || gpu.is_some();
            let t: IMFMediaType = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, if raw { &MFVideoFormat_NV12 } else { &MFVideoFormat_RGB32 })?;
            // NV12 not on offer (rare): brightness from RGB as before.
            let raw = raw && reader.SetCurrentMediaType(video, None, &t).is_ok();
            // The graphics card's frames are only read as they are: no conversion there.
            if !raw && gpu.is_some() {
                return Err(not_this_way());
            }
            if !raw {
                t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                reader.SetCurrentMediaType(video, None, &t)?;
            }
            let current = reader.GetCurrentMediaType(video)?;
            let size = current.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
            // RGB without a stated stride is bottom-up; NV12 is top-down, a byte a pixel.
            let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(if raw { w as i32 } else { -(w as i32 * 4) });
            // The shown picture: the converted type's aperture, else the file's own.
            let crop = aperture(&current, w, h).or_else(|| native.as_ref().and_then(|n| aperture(n, w, h))).unwrap_or((0, 0, w, h));
            // The matrix the file says, else BT.601 for standard definition and BT.709 above.
            let bt601 = match native.as_ref().and_then(|n| n.GetUINT32(&MF_MT_YUV_MATRIX).ok()) {
                Some(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => true,
                Some(1) => false,
                _ => crop.2.max(crop.3) <= 1024 && crop.2.min(crop.3) <= 576,
            };
            Ok(Self { reader, w, h, stride, path: std::cell::Cell::new(0), on_card: std::cell::Cell::new(false), crop, raw, full_range, bt601 })
        }
    }

    /// The frames showing at `times` (seconds, any order), as brightness scaled to `tw` × `th`.
    /// One seek, then a straight read: seeking for each frame would decode from a key frame
    /// every time.
    ///
    /// Reading only copies each wanted frame's brightness plane out; turning the copies into
    /// scaled brightness (4 ms a frame at 1080p) is other threads' work, so this one keeps up
    /// with the decoder. Done here between reads, the decoder ran far ahead meanwhile and kept
    /// what it had decoded: some 250 frames at once, 770 MB at 1080p, for most of a second
    /// (the editor's open peaked at 1.1 GB, measured).
    fn frames(&mut self, times: &[f64], tw: usize, th: usize) -> windows::core::Result<Vec<Luma>> {
        let mut order: Vec<usize> = (0..times.len()).collect();
        order.sort_by(|a, b| times[*a].total_cmp(&times[*b]));
        let out: std::sync::Mutex<Vec<Option<Luma>>> = std::sync::Mutex::new((0..times.len()).map(|_| None).collect());
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let (cw, ch, full) = (self.crop.2, self.crop.3, self.full_range);
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(times.len().max(1));
        // A few copies may wait for a free thread, no more (8 MB each at 4K): reading waits then.
        let (planes, copied) = std::sync::mpsc::sync_channel::<(usize, Vec<u8>)>(workers);
        let copied = std::sync::Mutex::new(copied);
        let read = std::thread::scope(|scope| -> windows::core::Result<()> {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        // The lock is held only to take the next copy.
                        let next = copied.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok((i, plane)) = next else { break };
                        let luma = plane_luma(plane, cw, ch, full, tw, th);
                        out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(luma);
                    }
                });
            }
            // Dropped when this returns, however it does: the workers finish what's queued and stop.
            let planes = planes;
            unsafe {
                let first = times[order[0]].max(0.);
                let mut pos: PROPVARIANT = std::mem::zeroed();
                (*pos.Anonymous.Anonymous).vt = VT_I8;
                (*pos.Anonymous.Anonymous).Anonymous.hVal = (first * 1e7) as i64;
                self.reader.SetCurrentPosition(&GUID::zeroed(), &pos)?;
                let mut next = 0;
                let mut last: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;
                // A seek lands on the key frame before: read on, keeping the frame showing at each
                // time asked for (the last one starting before it, within about half a frame).
                while next < order.len() {
                    let (mut flags, mut ts, mut sample) = (0u32, 0i64, None);
                    self.reader.ReadSample(video, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                    let ended = flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0;
                    while next < order.len() && (ended || ts + 1 >= (times[order[next]] * 1e7) as i64 - 170_000) {
                        if let Some(s) = sample.as_ref().filter(|_| !ended).or(last.as_ref()) {
                            if self.raw {
                                let _ = planes.send((order[next], self.raw_plane(s)?));
                            } else {
                                // Brightness from RGB (NV12 wasn't on offer: rare), as it comes.
                                let luma = self.luma(s, tw, th)?;
                                out.lock().unwrap_or_else(|e| e.into_inner())[order[next]] = Some(luma);
                            }
                        }
                        next += 1;
                    }
                    if ended {
                        break;
                    }
                    if sample.is_some() {
                        last = sample;
                    }
                }
            }
            Ok(())
        });
        read?;
        let out = out.into_inner().unwrap_or_else(|e| e.into_inner());
        out.into_iter().map(|l| l.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32)))).collect()
    }

    fn luma(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample, tw: usize, th: usize) -> windows::core::Result<Luma> {
        if self.raw {
            return Ok(plane_luma(self.raw_plane(sample)?, self.crop.2, self.crop.3, self.full_range, tw, th));
        }
        unsafe {
            let (w, h) = (self.w, self.h);
            let mut full = vec![0f32; w * h];
            let mut read = |row0: *const u8, pitch: isize| {
                for y in 0..h {
                    let line = row0.offset(y as isize * pitch);
                    for x in 0..w {
                        let p = line.add(x * 4);
                        // B, G, R, X
                        full[y * w + x] = 0.114 * *p as f32 + 0.587 * *p.add(1) as f32 + 0.299 * *p.add(2) as f32;
                    }
                }
            };
            // A 2D buffer says where its first row is and how far apart rows are; a plain one
            // is laid out as the media type's stride says (negative: bottom-up, as RGB usually is).
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let (mut row0, mut pitch) = (std::ptr::null_mut(), 0i32);
                two_d.Lock2D(&mut row0, &mut pitch)?;
                read(row0, pitch as isize);
                two_d.Unlock2D()?;
            } else {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut len))?;
                // Decoders pad pictures to 16-pixel blocks: a 1080-wide one has rows 1088 pixels
                // apart (a 1080-high one gets extra rows at the bottom, which don't matter).
                let padded = (w.div_ceil(16) * 16 * 4) as isize;
                let row = if len as isize >= padded * h as isize { padded } else { (w * 4) as isize };
                self.path.set(row);
                if len as isize >= row * h as isize {
                    if self.stride < 0 { read(data.offset(row * (h as isize - 1)), -row) } else { read(data, row) }
                }
                buffer.Unlock()?;
            }
            // Only the shown picture (not the decoder's padding).
            let (cx, cy, cw, ch) = self.crop;
            let shown = if (cx, cy, cw, ch) == (0, 0, w, h) {
                full
            } else {
                (cy..cy + ch).flat_map(|y| full[y * w + cx..y * w + cx + cw].iter().copied()).collect()
            };
            Ok(resize(&Luma { y: shown, w: cw, h: ch }, tw, th))
        }
    }

    /// The shown part of an NV12 picture's first plane (brightness as stored: one byte a pixel,
    /// top-down), copied out. Quick: `plane_luma` does the rest, on another thread.
    fn raw_plane(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> windows::core::Result<Vec<u8>> {
        let (w, h) = (self.w, self.h);
        let (cx, cy, cw, ch) = self.crop;
        let mut shown = vec![0u8; cw * ch];
        let mut read = |row0: *const u8, pitch: isize| unsafe {
            for y in 0..ch {
                let line = row0.offset((y + cy) as isize * pitch).add(cx);
                std::ptr::copy_nonoverlapping(line, shown.as_mut_ptr().add(y * cw), cw);
            }
        };
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if buffer.cast::<windows::Win32::Media::MediaFoundation::IMFDXGIBuffer>().is_ok() {
                self.on_card.set(true);
            }
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let (mut row0, mut pitch) = (std::ptr::null_mut(), 0i32);
                two_d.Lock2D(&mut row0, &mut pitch)?;
                read(row0, pitch as isize);
                two_d.Unlock2D()?;
            } else {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut len))?;
                // The brightness plane is the first h rows; rows may be padded to 16 bytes.
                let padded = w.div_ceil(16) * 16;
                let stated = self.stride.max(0) as usize;
                let row = [stated, padded, w].into_iter().find(|&r| r >= w && len as usize >= r * h * 3 / 2).unwrap_or(w);
                self.path.set(row as isize);
                if len as usize >= row * h {
                    read(data, row as isize);
                }
                buffer.Unlock()?;
            }
        }
        Ok(shown)
    }
}

/// Brightness from a picture's stored brightness plane (`Reader::raw_plane`: `w` × `h`, a byte a
/// pixel), turned into 0–255 by the file's own range (16–235 is stretched, 0–255 taken as it
/// is) and scaled to `tw` × `th`.
///
/// `resize`'s sums, taken straight from the bytes (the same numbers in the same order, so the
/// same result to the last bit) without the picture as floats in between: 33 MB a frame at 4K,
/// in as many threads as convert at once.
fn plane_luma(plane: Vec<u8>, w: usize, h: usize, full: bool, tw: usize, th: usize) -> Luma {
    let levels: [f32; 256] = std::array::from_fn(|v| if full { v as f32 } else { ((v as f32 - 16.) * (255. / 219.)).clamp(0., 255.) });
    let mut out = vec![0f32; tw * th];
    let (sx, sy) = (w as f32 / tw as f32, h as f32 / th as f32);
    for ty in 0..th {
        let (y0, y1) = ((ty as f32 * sy) as usize, (((ty + 1) as f32 * sy).ceil() as usize).min(h).max(1));
        for tx in 0..tw {
            let (x0, x1) = ((tx as f32 * sx) as usize, (((tx + 1) as f32 * sx).ceil() as usize).min(w).max(1));
            let (mut s, mut n) = (0f32, 0f32);
            for y in y0..y1.max(y0 + 1) {
                for x in x0..x1.max(x0 + 1) {
                    s += levels[plane[y.min(h - 1) * w + x.min(w - 1)] as usize];
                    n += 1.;
                }
            }
            out[ty * tw + tx] = s / n;
        }
    }
    Luma { y: out, w: tw, h: th }
}

impl Reader {
    /// The key frame at or before `t` (seconds): one decode, fast, enough for a filmstrip tile.
    fn near(&mut self, t: f64) -> windows::core::Result<Option<windows::Win32::Media::MediaFoundation::IMFSample>> {
        let video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        unsafe {
            let mut pos: PROPVARIANT = std::mem::zeroed();
            (*pos.Anonymous.Anonymous).vt = VT_I8;
            (*pos.Anonymous.Anonymous).Anonymous.hVal = (t.max(0.) * 1e7) as i64;
            self.reader.SetCurrentPosition(&GUID::zeroed(), &pos)?;
            for _ in 0..8 {
                let (mut flags, mut sample) = (0u32, None);
                self.reader.ReadSample(video, 0, None, Some(&mut flags), None, Some(&mut sample))?;
                if sample.is_some() {
                    return Ok(sample);
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            Ok(None)
        }
    }

    /// A decoded frame as top-down BGRA rows (w × h × 4).
    fn bgra(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> windows::core::Result<Vec<u8>> {
        if self.raw {
            return self.nv12_bgra(sample);
        }
        unsafe {
            let (w, h) = (self.w, self.h);
            let mut out = vec![0u8; w * h * 4];
            let mut read = |row0: *const u8, pitch: isize| {
                for y in 0..h {
                    std::ptr::copy_nonoverlapping(row0.offset(y as isize * pitch), out.as_mut_ptr().add(y * w * 4), w * 4);
                }
            };
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let (mut row0, mut pitch) = (std::ptr::null_mut(), 0i32);
                two_d.Lock2D(&mut row0, &mut pitch)?;
                read(row0, pitch as isize);
                two_d.Unlock2D()?;
            } else {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut len))?;
                let padded = (w.div_ceil(16) * 16 * 4) as isize;
                let row = if len as isize >= padded * h as isize { padded } else { (w * 4) as isize };
                if len as isize >= row * h as isize {
                    if self.stride < 0 { read(data.offset(row * (h as isize - 1)), -row) } else { read(data, row) }
                }
                buffer.Unlock()?;
            }
            Ok(out)
        }
    }

    /// A decoded NV12 frame as top-down BGRA rows (w × h × 4), turned into RGB by the file's own
    /// range and matrix (Windows' conversion would take the decoder's word for the range).
    fn nv12_bgra(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> windows::core::Result<Vec<u8>> {
        let (w, h) = (self.w, self.h);
        let mut out = vec![0u8; w * h * 4];
        // Stored bytes to Y' 0–1 and Cb/Cr ±0.5.
        let (y_off, y_scale, c_scale) = if self.full_range { (0., 1. / 255., 1. / 255.) } else { (16., 1. / 219., 1. / 224.) };
        let (kr, kb) = if self.bt601 { (0.299f64, 0.114f64) } else { (0.2126, 0.0722) };
        let kg = 1. - kr - kb;
        let (r_cr, g_cb, g_cr, b_cb) = (2. * (1. - kr), -2. * kb * (1. - kb) / kg, -2. * kr * (1. - kr) / kg, 2. * (1. - kb));
        // Each stored byte's part of R, G and B (0–255 in 16.16 fixed point): a whole frame
        // (3 megapixels for a phone recording) takes milliseconds, not a tenth of a second.
        let table = |f: &dyn Fn(f64) -> f64| -> [i32; 256] { std::array::from_fn(|v| (f(v as f64) * 255. * 65536.).round() as i32) };
        let ys = table(&|v| (v - y_off) * y_scale);
        let (rv, gu, gv, bu) = (
            table(&|v| r_cr * (v - 128.) * c_scale),
            table(&|v| g_cb * (v - 128.) * c_scale),
            table(&|v| g_cr * (v - 128.) * c_scale),
            table(&|v| b_cb * (v - 128.) * c_scale),
        );
        let byte = |v: i32| ((v + 32768) >> 16).clamp(0, 255) as u8;
        // Luma rows from `y0`, the half-height chroma plane (Cb, Cr pairs) from `uv0`.
        let mut convert = |y0: *const u8, uv0: *const u8, pitch: isize| unsafe {
            for row in 0..h {
                let luma = std::slice::from_raw_parts(y0.offset(row as isize * pitch), w);
                let chroma = std::slice::from_raw_parts(uv0.offset((row / 2) as isize * pitch), w.div_ceil(2) * 2);
                let line = &mut out[row * w * 4..(row + 1) * w * 4];
                for (x, (px, &y)) in line.chunks_exact_mut(4).zip(luma).enumerate() {
                    let (cb, cr) = (chroma[x & !1] as usize, chroma[(x & !1) + 1] as usize);
                    let y = ys[y as usize];
                    px[0] = byte(y + bu[cb]);
                    px[1] = byte(y + gu[cb] + gv[cr]);
                    px[2] = byte(y + rv[cr]);
                    px[3] = 255;
                }
            }
        };
        unsafe {
            let buffer = sample.GetBufferByIndex(0)?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let (mut row0, mut pitch) = (std::ptr::null_mut(), 0i32);
                two_d.Lock2D(&mut row0, &mut pitch)?;
                // The chroma plane follows the luma plane's h rows.
                convert(row0, row0.offset(pitch as isize * h as isize), pitch as isize);
                two_d.Unlock2D()?;
            } else {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
                buffer.Lock(&mut data, None, Some(&mut len))?;
                // Rows may be padded to 16 bytes (as in `raw_luma`).
                let padded = w.div_ceil(16) * 16;
                let stated = self.stride.max(0) as usize;
                let row = [stated, padded, w].into_iter().find(|&r| r >= w && len as usize >= r * h * 3 / 2).unwrap_or(w);
                if len as usize >= row * h * 3 / 2 {
                    convert(data, data.add(row * h), row as isize);
                }
                buffer.Unlock()?;
            }
        }
        Ok(out)
    }
}

/// Frames along a video for the compress editor's timeline (`drawFilmstrip` in MediaEditor.tsx):
/// tiles at the picture's own shape (turned upright) filling `w` × `h` px, each showing the moment
/// at its middle. A PNG. Blocking: run it off the UI thread.
///
/// With `gpu` (a big picture: `gpu_for`), decoded on the graphics card: the processor's decoder
/// keeps some 35 frames while it works, 435 MB at 4K. In software where the card can't.
pub fn filmstrip(path: &str, rotation: u32, dur: f64, w: usize, h: usize, gpu: Option<Gpu>) -> Result<Vec<u8>, String> {
    if let Some(gpu) = &gpu {
        match strip(path, rotation, dur, w, h, Some(gpu)) {
            Ok(png) => return Ok(png),
            Err(why) => log::info!(target: "ferret::perf", "filmstrip: not on the graphics card ({why}), in software"),
        }
    }
    strip(path, rotation, dur, w, h, None)
}

fn strip(path: &str, rotation: u32, dur: f64, w: usize, h: usize, gpu: Option<&Gpu>) -> Result<Vec<u8>, String> {
    let e = |e: windows::core::Error| e.to_string();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e)?;
    }
    let mut r = Reader::open(path, gpu).map_err(e)?;
    let turned = rotation == 90 || rotation == 270;
    // The shown picture (the decoder may pad the frame).
    let (cx, cy, cw, ch) = r.crop;
    let (vw, vh) = if turned { (ch, cw) } else { (cw, ch) };
    if w == 0 || h == 0 || vw == 0 || vh == 0 {
        return Err("no picture".into());
    }
    let tw = (h as f64 * vw as f64 / vh as f64).max(1.);
    let n = (w as f64 / tw).ceil() as usize;
    let mut canvas = vec![0u8; w * h * 4];
    for i in 0..n {
        let t = ((i as f64 + 0.5) * tw / w as f64 * dur).min((dur - 0.05).max(0.));
        let Some(sample) = r.near(t).map_err(e)? else { continue };
        let px = r.bgra(&sample).map_err(e)?;
        let x0 = (i as f64 * tw) as usize;
        let x1 = (((i + 1) as f64 * tw) as usize).min(w);
        for oy in 0..h {
            for ox in x0..x1 {
                // Where this tile pixel is in the upright picture, then in the stored one.
                let ux = ((ox - x0) as f64 + 0.5) / tw * vw as f64;
                let uy = (oy as f64 + 0.5) / h as f64 * vh as f64;
                let (ux, uy) = ((ux as usize).min(vw - 1), (uy as usize).min(vh - 1));
                let (sx, sy) = match rotation {
                    90 => (uy, ch - 1 - ux),
                    270 => (cw - 1 - uy, ux),
                    180 => (cw - 1 - ux, ch - 1 - uy),
                    _ => (ux, uy),
                };
                let s = ((sy + cy) * r.w + sx + cx) * 4;
                let d = (oy * w + ox) * 4;
                // BGRA → RGBA.
                canvas[d] = px[s + 2];
                canvas[d + 1] = px[s + 1];
                canvas[d + 2] = px[s];
                canvas[d + 3] = 255;
            }
        }
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, canvas).ok_or("bad size")?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(png.into_inner())
}
/// Area-average scaling (box filter), so detail finer than the target size averages out.
fn resize(src: &Luma, tw: usize, th: usize) -> Luma {
    let mut out = vec![0f32; tw * th];
    let (sx, sy) = (src.w as f32 / tw as f32, src.h as f32 / th as f32);
    for ty in 0..th {
        let (y0, y1) = ((ty as f32 * sy) as usize, (((ty + 1) as f32 * sy).ceil() as usize).min(src.h).max(1));
        for tx in 0..tw {
            let (x0, x1) = ((tx as f32 * sx) as usize, (((tx + 1) as f32 * sx).ceil() as usize).min(src.w).max(1));
            let (mut s, mut n) = (0f32, 0f32);
            for y in y0..y1.max(y0 + 1) {
                for x in x0..x1.max(x0 + 1) {
                    s += src.y[y.min(src.h - 1) * src.w + x.min(src.w - 1)];
                    n += 1.;
                }
            }
            out[ty * tw + tx] = s / n;
        }
    }
    Luma { y: out, w: tw, h: th }
}

/// Turns stored-orientation brightness upright (a phone video's original is stored on its side;
/// the transcoder writes the sample upright).
fn rotate(src: Luma, degrees: u32) -> Luma {
    match degrees {
        90 | 270 => {
            let (w, h) = (src.h, src.w);
            let mut y = vec![0f32; w * h];
            for oy in 0..h {
                for ox in 0..w {
                    let (sx, sy) = if degrees == 90 { (oy, src.h - 1 - ox) } else { (src.w - 1 - oy, ox) };
                    y[oy * w + ox] = src.y[sy * src.w + sx];
                }
            }
            Luma { y, w, h }
        }
        180 => Luma { y: src.y.into_iter().rev().collect(), w: src.w, h: src.h },
        _ => src,
    }
}

pub struct Check {
    /// The worst frame's SSIM.
    pub score: f64,
    /// When that frame is, in the original (seconds).
    pub at: f64,
    /// Frame sizes, rotation and the alignment found (diagnostics).
    pub detail: String,
}

const SAMPLE_FRAMES: usize = 8;

/// How far either side of the expected frame a sample frame is looked for, in frames. Phone
/// recordings have a variable frame rate, so a sample re-timed to a steady one drifts against
/// them by a frame here and there; while the screen scrolls, one frame off scored as "Poor"
/// although nothing was blurred (user report, 2026-11-10).
const NEAR: i32 = 8;
/// Steps between the frames looked at, in the sample's frames: half of one, so a 60 fps original
/// behind a 30 fps sample has every frame tried.
const STEP: f64 = 0.5;

/// One sample frame against the original.
#[derive(Clone, Debug)]
pub struct FrameScore {
    /// When, in the sample (seconds).
    pub t: f64,
    /// SSIM against the best of the original's frames within `NEAR` of it (the one expected at
    /// that moment included).
    pub best: f64,
    /// Which one that was (frames from the expected one).
    pub offset: i32,
}

/// The sample at `sample` (encoded from `at` seconds of `original`, `dur` long, `fps` frames a
/// second) against the original. Blocking: run it off the UI thread.
pub fn check(original: &str, rotation: u32, sample: &str, at: f64, dur: f64, fps: f64, gpu: Option<Gpu>) -> Result<Check, String> {
    let (scores, detail) = frame_scores(original, rotation, sample, at, dur, fps, SAMPLE_FRAMES, gpu.as_ref())?;
    let mut worst = Check { score: 1., at, detail };
    for f in scores {
        if f.best < worst.score {
            worst.score = f.best;
            worst.at = at + f.t;
        }
    }
    Ok(worst)
}

/// `frames` frames spread over the sample, each scored against the original's frame at the same
/// moment and against its neighbours (`FrameScore`). The whole sample is first lined up on its
/// first frame (a sample can start a frame or two off); then each frame is matched on its own.
///
/// Decoded on the graphics card when `gpu` is given and it can (see `Gpu`), else in software.
#[allow(clippy::too_many_arguments)]
pub fn frame_scores(original: &str, rotation: u32, sample: &str, at: f64, dur: f64, fps: f64, frames: usize, gpu: Option<&Gpu>) -> Result<(Vec<FrameScore>, String), String> {
    let Some(gpu) = gpu else { return scored(original, rotation, sample, at, dur, fps, frames, None) };
    let on_card = scored(original, rotation, sample, at, dur, fps, frames, Some(gpu));
    // Tests: both ways, and how far apart they are.
    if std::env::var_os("FERRET_GPUI_CHECK_BOTH").is_some() {
        if let (Ok((card, _)), Ok((plain, _))) = (&on_card, scored(original, rotation, sample, at, dur, fps, frames, None)) {
            let apart = card.iter().zip(&plain).map(|(a, b)| (a.best - b.best).abs()).fold(0f64, f64::max);
            let moved = card.iter().zip(&plain).filter(|(a, b)| a.offset != b.offset).count();
            log::info!(target: "ferret::perf", "quality check both ways: scores at most {apart:.6} apart, {moved} of {} frames matched another neighbour", card.len());
        }
    }
    match on_card {
        Ok(scored) => Ok(scored),
        Err(why) => {
            log::info!(target: "ferret::perf", "quality check: not on the graphics card ({why}), in software");
            scored(original, rotation, sample, at, dur, fps, frames, None)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scored(original: &str, rotation: u32, sample: &str, at: f64, dur: f64, fps: f64, frames: usize, gpu: Option<&Gpu>) -> Result<(Vec<FrameScore>, String), String> {
    let e = |e: windows::core::Error| e.to_string();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(e)?;
    }
    // Where the time goes, for the log line.
    let mut lap = std::time::Instant::now();
    let mut took = |what: &str, laps: &mut Vec<String>| {
        laps.push(format!("{what} {} ms", lap.elapsed().as_millis()));
        lap = std::time::Instant::now();
    };
    let mut laps = Vec::new();
    let mut o = Reader::open_luma(original, gpu).map_err(e)?;
    let mut s = Reader::open_luma(sample, gpu).map_err(e)?;
    took("opening", &mut laps);
    // Compare at the sample's picture size, about 960 px wide.
    let (tw, th) = {
        let (sw, sh) = (s.crop.2, s.crop.3);
        let k = 960f64.min(sw as f64) / sw as f64;
        (((sw as f64 * k) as usize).max(8), ((sh as f64 * k) as usize).max(8))
    };
    let turned = rotation == 90 || rotation == 270;
    let (ow, oh) = if turned { (th, tw) } else { (tw, th) };
    let originals = |o: &mut Reader, times: &[f64]| -> Result<Vec<Luma>, String> {
        Ok(o.frames(times, ow, oh).map_err(e)?.into_iter().map(|l| rotate(l, rotation)).collect())
    };
    let fps = if fps > 0. { fps } else { 30. };
    let frames = frames.max(1);
    let frame_at = |k: usize| ((((k as f64 + 0.5) / frames as f64) * dur * fps).floor() + 0.5) / fps;
    let sample_times: Vec<f64> = (0..frames).map(frame_at).collect();
    let samples = s.frames(&sample_times, tw, th).map_err(e)?;
    took("the sample's frames", &mut laps);
    // Each frame against the original's frames around the same moment (one pass: the times are
    // read in order after one seek). The match is found at half size, which is quicker, then
    // scored at full size. No shift for the whole sample, found on its first frame: on a phone
    // recording that one was ambiguous and set every other frame 0.1 s off (2026-11-10).
    let span = 2 * NEAR as usize + 1;
    let wanted: Vec<f64> = sample_times.iter().flat_map(|t| (-NEAR..=NEAR).map(move |d| at + t + d as f64 * STEP / fps)).collect();
    let matching = originals(&mut o, &wanted)?;
    took("the original's frames", &mut laps);
    let half = |l: &Luma| resize(l, (l.w / 2).max(8), (l.h / 2).max(8));
    let score = |a: &Luma, b: &Luma| ssim(a, &tone_matched(a, b));
    let mut scores = Vec::with_capacity(frames);
    for (k, t) in sample_times.iter().enumerate() {
        let near = &matching[k * span..(k + 1) * span];
        let small = half(&samples[k]);
        // The expected frame wins a tie: a neighbour must be clearly closer.
        let mut pick = (score(&half(&near[NEAR as usize]), &small), NEAR as usize);
        for (i, l) in near.iter().enumerate() {
            let s = score(&half(l), &small);
            if s > pick.0 + 1e-3 {
                pick = (s, i);
            }
        }
        let expected = score(&near[NEAR as usize], &samples[k]);
        let best = if pick.1 == NEAR as usize { expected } else { score(&near[pick.1], &samples[k]).max(expected) };
        scores.push(FrameScore { t: *t, best, offset: pick.1 as i32 - NEAR });
    }
    took("scoring", &mut laps);
    let by = |r: &Reader| if r.on_card.get() { "the graphics card" } else { "the processor" };
    laps.push(format!("original decoded by {}, sample by {}", by(&o), by(&s)));
    let moved = scores.iter().filter(|f| f.offset != 0).count();
    let detail = format!(
        "original {}x{} (shows {}x{}) rot {rotation} rows {}, sample {}x{} rows {}, compared {tw}x{th}, {moved} of {frames} frames matched a neighbour; {}",
        o.w,
        o.h,
        o.crop.2,
        o.crop.3,
        o.path.get(),
        s.w,
        s.h,
        s.path.get(),
        laps.join(", ")
    );
    Ok((scores, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's bytes scaled in one go give exactly what the two steps gave (floats first, then
    /// `resize`): the check's scores must not move by a digit.
    #[test]
    fn a_plane_scales_to_the_same_brightness_as_its_floats_did() {
        let (w, h) = (1206, 654);
        let plane: Vec<u8> = (0..w * h).map(|i| ((i * 7919 + (i / w) * 31) % 256) as u8).collect();
        for full in [false, true] {
            let floats: Vec<f32> = plane.iter().map(|&v| if full { v as f32 } else { ((v as f32 - 16.) * (255. / 219.)).clamp(0., 255.) }).collect();
            for (tw, th) in [(960, 520), (603, 327), (w, h), (8, 8)] {
                let two_steps = resize(&Luma { y: floats.clone(), w, h }, tw, th);
                let one = plane_luma(plane.clone(), w, h, full, tw, th);
                assert_eq!((one.w, one.h), (tw, th));
                assert!(one.y.iter().zip(&two_steps.y).all(|(a, b)| a.to_bits() == b.to_bits()), "{tw}x{th}, full range {full}");
            }
        }
    }

    #[test]
    fn identical_pictures_score_one_and_noise_scores_lower() {
        let a = Luma { y: (0..64 * 64).map(|i| (i % 255) as f32).collect(), w: 64, h: 64 };
        let same = Luma { y: a.y.clone(), w: 64, h: 64 };
        assert!((ssim(&a, &same) - 1.).abs() < 1e-9);
        // Noise on a flat grey is all the picture has: it scores low.
        let flat = Luma { y: vec![128.; 64 * 64], w: 64, h: 64 };
        let noisy = Luma { y: (0..64 * 64).map(|i| 128. + if i % 3 == 0 { 40. } else { -25. }).collect(), w: 64, h: 64 };
        assert!(ssim(&flat, &noisy) < 0.5, "{}", ssim(&flat, &noisy));
        assert_eq!(verdict(0.99).0, Level::Same);
        assert_eq!(verdict(0.9).0, Level::Poor);
    }

    /// A detailed picture: text-like strokes over a gradient (deterministic).
    fn detailed(w: usize, h: usize) -> Luma {
        let mut seed = 12345u32;
        let y = (0..w * h)
            .map(|i| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let (x, y) = (i % w, i / w);
                let stroke = if (x / 3 + y / 5) % 4 == 0 { 140. } else { 0. };
                20. + (y as f32 / h as f32) * 60. + stroke + (seed >> 28) as f32
            })
            .collect();
        Luma { y, w, h }
    }

    #[test]
    fn a_brightness_shift_is_forgiven_but_blur_and_blocks_are_not() {
        let a = detailed(128, 128);
        // Lifted blacks and a little less contrast (another range or curve): the same picture.
        let lifted = Luma { y: a.y.iter().map(|v| v * 0.9 + 16.).collect(), w: 128, h: 128 };
        let s = ssim(&a, &tone_matched(&a, &lifted));
        assert!(s > 0.99, "matched: {s}");
        // On a dark screen plain SSIM calls a lifted black "Poor"; matched, it's the same.
        let dark = Luma { y: a.y.iter().map(|v| v * 0.08).collect(), w: 128, h: 128 };
        let dark_lifted = Luma { y: dark.y.iter().map(|v| v + 8.).collect(), w: 128, h: 128 };
        assert_eq!(verdict(ssim(&dark, &dark_lifted)).0, Level::Poor, "plain: {}", ssim(&dark, &dark_lifted));
        let s = ssim(&dark, &tone_matched(&dark, &dark_lifted));
        assert!(s > 0.99, "dark, matched: {s}");
        // Smeared (a 5 × 5 average): detail is lost, whatever the levels.
        let blurred = {
            let mut y = vec![0f32; 128 * 128];
            for py in 0..128usize {
                for px in 0..128usize {
                    let (mut sum, mut n) = (0., 0.);
                    for dy in py.saturating_sub(2)..(py + 3).min(128) {
                        for dx in px.saturating_sub(2)..(px + 3).min(128) {
                            sum += a.y[dy * 128 + dx];
                            n += 1.;
                        }
                    }
                    y[py * 128 + px] = sum / n;
                }
            }
            Luma { y, w: 128, h: 128 }
        };
        let s = ssim(&a, &tone_matched(&a, &blurred));
        assert_eq!(verdict(s).0, Level::Poor, "blurred: {s}");
        // Blocks: each 8 × 8 square one flat value.
        let blocky = Luma {
            y: (0..128 * 128)
                .map(|i| {
                    let (bx, by) = ((i % 128) / 8 * 8, (i / 128) / 8 * 8);
                    (0..8).flat_map(|dy| (0..8).map(move |dx| (dy, dx))).map(|(dy, dx)| a.y[(by + dy) * 128 + bx + dx]).sum::<f32>() / 64.
                })
                .collect(),
            w: 128,
            h: 128,
        };
        let s = ssim(&a, &tone_matched(&a, &blocky));
        assert_eq!(verdict(s).0, Level::Poor, "blocky: {s}");
    }

    #[test]
    fn rotation_turns_a_quarter() {
        // 3 wide, 2 high: 0 1 2 / 3 4 5. Turned 90° clockwise: 2 wide, 3 high: 3 0 / 4 1 / 5 2.
        let l = Luma { y: vec![0., 1., 2., 3., 4., 5.], w: 3, h: 2 };
        let r = rotate(l, 90);
        assert_eq!((r.w, r.h), (2, 3));
        assert_eq!(r.y, vec![3., 0., 4., 1., 5., 2.]);
    }
}
