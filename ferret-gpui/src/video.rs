//! Video playback without a web view, all on the GPU:
//! - Media Foundation's media engine decodes (DXVA, hardware) on the same Direct3D 11 device
//!   GPUI renders with (shared by the patched gpui-pre-windows, see vendor/).
//! - Each new frame is scaled by the GPU's video processor into a texture of the size shown.
//! - That texture is copied GPU-to-GPU over the atlas tile of the image GPUI draws.
//!
//! No pixel goes through the CPU. Sound plays through the media engine itself. Everything here
//! runs on the UI thread (these COM objects aren't Send); the media engine runs its own threads.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use gpui::RenderImage;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MFMediaEngineClassFactory, IMFAttributes, IMFDXGIDeviceManager, IMFMediaEngine,
    IMFMediaEngineClassFactory, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MF_MEDIA_ENGINE_CALLBACK,
    MF_MEDIA_ENGINE_DXGI_MANAGER, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MFCreateAttributes,
    MFCreateDXGIDeviceManager, MFSTARTUP_FULL, MFStartup, MFVideoNormalizedRect, MF_VERSION,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{BSTR, Interface, implement};

use windows::Win32::Media::MediaFoundation::{
    MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR, MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA,
};
const EVENT_LOADEDMETADATA: u32 = MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 as u32;
const EVENT_ERROR: u32 = MF_MEDIA_ENGINE_EVENT_ERROR.0 as u32;
const EVENT_ENDED: u32 = MF_MEDIA_ENGINE_EVENT_ENDED.0 as u32;

/// What the media engine reported, set from its threads.
#[derive(Default)]
struct Events {
    metadata: AtomicBool,
    ended: AtomicBool,
    error: AtomicU32,
}

#[implement(IMFMediaEngineNotify)]
struct Notify(Arc<Events>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, param1: usize, _param2: u32) -> windows::core::Result<()> {
        match event {
            EVENT_LOADEDMETADATA => self.0.metadata.store(true, Ordering::SeqCst),
            EVENT_ENDED => self.0.ended.store(true, Ordering::SeqCst),
            EVENT_ERROR => self.0.error.store(param1.max(1) as u32, Ordering::SeqCst),
            _ => {}
        }
        Ok(())
    }
}

fn startup() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string()) }).clone()
}

/// GPUI's own device (the patched backend hands it out once a window exists).
fn gpui_device() -> Result<ID3D11Device, String> {
    let (device, context) = gpui_windows::ferret::shared_device().ok_or("GPUI's Direct3D device isn't ready")?;
    // SAFETY: each pointer carries a reference we now own.
    unsafe {
        drop(windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext::from_raw(context));
        Ok(ID3D11Device::from_raw(device))
    }
}

/// The frame shown: a texture the video processor scales into, and the GPUI image whose atlas
/// tile it's copied over.
struct Screen {
    texture: ID3D11Texture2D,
    image: Arc<RenderImage>,
    w: u32,
    h: u32,
}

pub struct Player {
    engine: IMFMediaEngine,
    _manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    events: Arc<Events>,
    screen: Option<Screen>,
    last_pts: Option<i64>,
    /// A frame is in the texture but not yet in GPUI's atlas (the image wasn't painted yet).
    unsynced: bool,
    /// Frames that reached GPUI's atlas (benchmarks).
    pub frames: u64,
    /// The last thing that failed while showing frames.
    pub last_error: Option<String>,
    /// The part of the frame shown (a zoom), or all of it.
    src: Option<[f32; 4]>,
}

impl Player {
    pub fn open(path: &str, muted: bool) -> Result<Self, String> {
        startup()?;
        let e = |e: windows::core::Error| e.to_string();
        let device = gpui_device()?;
        unsafe {
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(e)?;
            let manager: IMFDXGIDeviceManager = manager.ok_or("no device manager")?;
            manager.ResetDevice(&device, token).map_err(e)?;

            let events = Arc::new(Events::default());
            let notify: IMFMediaEngineNotify = Notify(events.clone()).into();
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 3).map_err(e)?;
            let attrs: IMFAttributes = attrs.ok_or("no attributes")?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &manager).map_err(e)?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify).map_err(e)?;
            attrs.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32).map_err(e)?;

            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER).map_err(e)?;
            // No window given: frame-server mode, we pull the frames.
            let engine = factory.CreateInstance(0, &attrs).map_err(e)?;
            engine.SetMuted(muted).map_err(e)?;
            engine.SetSource(&BSTR::from(path)).map_err(e)?;
            Ok(Self { engine, _manager: manager, device, events, screen: None, last_pts: None, unsynced: false, frames: 0, last_error: None, src: None })
        }
    }

    pub fn error(&self) -> Option<u32> {
        Some(self.events.error.load(Ordering::SeqCst)).filter(|c| *c != 0)
    }

    /// Frame size as shown (rotation applied by the media engine), once it has read the file.
    pub fn native_size(&self) -> Option<(u32, u32)> {
        // HAVE_METADATA (1) or later; the event says the same, whichever comes first.
        if !self.events.metadata.load(Ordering::SeqCst) && unsafe { self.engine.GetReadyState() } < 1 {
            return None;
        }
        let (mut w, mut h) = (0u32, 0u32);
        unsafe { self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).ok()? };
        (w > 0 && h > 0).then_some((w, h))
    }

    /// Seconds (NaN until known).
    pub fn duration(&self) -> f64 {
        unsafe { self.engine.GetDuration() }
    }

    pub fn muted(&self) -> bool {
        unsafe { self.engine.GetMuted().as_bool() }
    }

    /// 0–1.
    pub fn volume(&self) -> f64 {
        unsafe { self.engine.GetVolume() }
    }

    pub fn position(&self) -> f64 {
        unsafe { self.engine.GetCurrentTime() }
    }

    /// Playing, and not at the end yet. (The media engine follows the older HTML rules: a video
    /// that played to its end isn't "paused", it's "ended".)
    pub fn playing(&self) -> bool {
        unsafe { !self.engine.IsPaused().as_bool() && !self.engine.IsEnded().as_bool() }
    }

    /// Played to its end (and not sought back since).
    pub fn ended(&self) -> bool {
        unsafe { self.engine.IsEnded().as_bool() }
    }

    pub fn play(&mut self) {
        if self.events.ended.swap(false, Ordering::SeqCst) {
            self.seek(0.);
        }
        unsafe {
            let _ = self.engine.Play();
        }
    }

    pub fn pause(&mut self) {
        unsafe {
            let _ = self.engine.Pause();
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        unsafe {
            let _ = self.engine.SetMuted(muted);
        }
    }

    /// 0–1.
    pub fn set_volume(&mut self, volume: f64) {
        unsafe {
            let _ = self.engine.SetVolume(volume.clamp(0., 1.));
        }
    }

    pub fn seek(&mut self, secs: f64) {
        unsafe {
            let _ = self.engine.SetCurrentTime(secs.max(0.));
        }
        self.events.ended.store(false, Ordering::SeqCst);
        self.last_pts = None;
    }

    /// The image to draw (its pixels are replaced on the GPU as frames come).
    pub fn image(&self) -> Option<Arc<RenderImage>> {
        self.screen.as_ref().map(|s| s.image.clone())
    }

    /// Brings the shown frame up to date at `w`×`h` device pixels. Returns an image that was
    /// replaced (a new size), for the caller to drop from GPUI's cache. Call every frame while
    /// playing; `needs_frames` says when to keep asking for frames.
    pub fn update(&mut self, w: u32, h: u32) -> Option<Arc<RenderImage>> {
        self.update_part(w, h, None)
    }

    /// Like `update`, showing only `src` of the frame (left, top, right, bottom, 0–1): a zoom. The
    /// GPU's video processor scales that part up from the decoded frame's own pixels.
    pub fn update_part(&mut self, w: u32, h: u32, src: Option<[f32; 4]>) -> Option<Arc<RenderImage>> {
        if self.src != src {
            self.src = src;
            // The same frame again, framed differently.
            self.last_pts = None;
        }
        if w == 0 || h == 0 || self.native_size().is_none() {
            return None;
        }
        let mut dropped = None;
        if !self.screen.as_ref().is_some_and(|s| s.w == w && s.h == h) {
            match self.make_screen(w, h) {
                Ok(s) => dropped = self.screen.replace(s).map(|old| old.image),
                Err(e) => {
                    self.last_error = Some(format!("texture {w}x{h}: {e}"));
                    return None;
                }
            }
            self.last_pts = None;
        }
        let screen = self.screen.as_ref().unwrap();
        match unsafe { self.engine.OnVideoStreamTick() } {
            Ok(pts) if self.last_pts != Some(pts) => {
                let rect = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
                let part = self.src.map(|[left, top, right, bottom]| MFVideoNormalizedRect { left, top, right, bottom });
                match unsafe { self.engine.TransferVideoFrame(&screen.texture, part.as_ref().map(|p| p as *const _), &rect, None) } {
                    Ok(()) => {
                        self.last_pts = Some(pts);
                        self.unsynced = true;
                    }
                    Err(e) => self.last_error = Some(format!("transfer: {e}")),
                }
            }
            Ok(_) => {}
            Err(e) => self.last_error = Some(format!("tick: {e}")),
        }
        if self.unsynced {
            if gpui_windows::ferret::copy_to_image(screen.image.id, screen.texture.as_raw()) {
                self.unsynced = false;
                self.frames += 1;
            } else {
                self.last_error = Some("frame not in the atlas yet".into());
            }
        }
        dropped
    }

    /// The shown frame's time stamp (100 ns units), for the frame trace.
    pub fn pts(&self) -> Option<i64> {
        self.last_pts
    }

    /// Frames are still wanted: playing, or one waiting to reach the atlas or to arrive.
    pub fn needs_frames(&self) -> bool {
        self.playing() || self.unsynced || self.last_pts.is_none()
    }

    fn make_screen(&self, w: u32, h: u32) -> windows::core::Result<Screen> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        // A blank image of the same size: GPUI puts it in its atlas once, then the GPU copies
        // every frame over it.
        let blank = image::RgbaImage::new(w, h);
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(blank)]));
        Ok(Screen { texture: texture.unwrap(), image, w, h })
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        unsafe {
            let _ = self.engine.Shutdown();
        }
    }
}

/// "1:05" or "1:02:03".
pub fn clock(secs: f64) -> String {
    if !secs.is_finite() {
        return "0:00".into();
    }
    let s = secs.max(0.) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}
