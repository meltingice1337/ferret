//! Ferret patch (not upstream GPUI): video without CPU copies. GPUI's Windows renderer has no
//! external surfaces (`draw_surfaces` is empty; only macOS gets them), so the app decodes on the
//! very device GPUI renders with, and copies each frame GPU-to-GPU over the atlas tile of an
//! image it shows. Everything here runs on the UI thread.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::{Arc, Weak};

use gpui::ImageId;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};
use windows::core::Interface;

use crate::directx_atlas::DirectXAtlas;

/// Start-up timing: the app says where a step of the platform's start is noted (its log), and
/// the platform calls `mark` at each.
static MARK: std::sync::OnceLock<fn(&'static str)> = std::sync::OnceLock::new();

pub fn on_mark(note: fn(&'static str)) {
    let _ = MARK.set(note);
}

/// See its call in window.rs.
pub(crate) fn note_frame_size(primitives: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("FERRET_GPUI_HITCH_MS").is_some()) {
        return;
    }
    thread_local! {
        static LAST: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let last = LAST.with(|l| l.replace(primitives));
    if last > 200 && primitives * 4 < last {
        log::info!(target: "ferret::trace", "thin frame: {primitives} things drawn, after a frame with {last}");
    }
}

pub(crate) fn mark(step: &'static str) {
    if let Some(note) = MARK.get() {
        note(step);
    }
}

/// Start-up: making the graphics device takes over half of it (some 130 ms of 220, measured),
/// and nothing else needs it until the first window. So the app has it made on a thread of its
/// own from the first line of `main`, while the rest of the start goes on, and the platform
/// takes it when it attaches (`attach_gpu`), waiting only for what's left of it.
static EARLY_DEVICES: std::sync::Mutex<Option<std::thread::JoinHandle<anyhow::Result<crate::directx_devices::DirectXDevices>>>> = std::sync::Mutex::new(None);

pub fn make_devices_early() {
    if let Ok(mut early) = EARLY_DEVICES.lock() {
        if early.is_none() {
            *early = std::thread::Builder::new().name("ferret-early-device".into()).spawn(crate::directx_devices::DirectXDevices::new).ok();
        }
    }
}

/// The devices made early, once (None: not asked for, or it failed and they're made as usual).
pub(crate) fn early_devices() -> Option<crate::directx_devices::DirectXDevices> {
    let made = EARLY_DEVICES.lock().ok()?.take()?.join().ok()?;
    match made {
        Ok(devices) => {
            // Made on another thread: this one, where video decoders are made, knows them too.
            note_device(&devices.device, &devices.device_context);
            Some(devices)
        }
        Err(e) => {
            log::warn!("the graphics device made early failed ({e:#}): making it again");
            None
        }
    }
}

thread_local! {
    static DEVICE: RefCell<Option<(ID3D11Device, ID3D11DeviceContext)>> = const { RefCell::new(None) };
    static ATLAS: RefCell<Option<Weak<DirectXAtlas>>> = const { RefCell::new(None) };
}

/// Counts the devices made: after a GPU reset (a driver update or crash) GPUI renders on a new
/// one, and whatever was made on the old one (a video's decoder and frames) must be made again.
static DEVICE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Called where the devices are made (the vsync thread, after a reset) and again on the UI
/// thread once it takes them over (`handle_device_lost`), so `shared_device` there is current.
pub(crate) fn note_device(device: &ID3D11Device, context: &ID3D11DeviceContext) {
    DEVICE.with(|d| *d.borrow_mut() = Some((device.clone(), context.clone())));
    DEVICE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// Changes whenever GPUI's device does: compare with the value seen when something was made on
/// `shared_device`, and make it again when it differs.
pub fn device_generation() -> u64 {
    DEVICE_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn note_atlas(atlas: &Arc<DirectXAtlas>) {
    ATLAS.with(|a| *a.borrow_mut() = Some(Arc::downgrade(atlas)));
}

/// The renderer's `ID3D11Device` and immediate `ID3D11DeviceContext`, as raw COM pointers that
/// each carry a reference the caller owns (`from_raw`). The device was made with video support
/// and is multithread-protected, so Media Foundation can decode on it. None before a window.
pub fn shared_device() -> Option<(*mut c_void, *mut c_void)> {
    DEVICE.with(|d| d.borrow().as_ref().map(|(dev, ctx)| (dev.clone().into_raw(), ctx.clone().into_raw())))
}

thread_local! {
    /// What the app files are dragged from allows done with them (DROPEFFECT bits), for the
    /// drag over this app's windows. GPUI's FileDropEvent doesn't carry it.
    static DROP_ALLOWED: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
}

pub(crate) fn note_drop_allowed(effects: u32) {
    DROP_ALLOWED.with(|c| c.set(effects));
}

/// The app the files are dragged from lets them be moved. Explorer does; many apps (mail,
/// browsers, editors) only offer a copy, and moving their file away would take it from them.
pub fn drop_allows_move() -> bool {
    DROP_ALLOWED.with(|c| c.get() & windows::Win32::System::Ole::DROPEFFECT_MOVE.0 != 0)
}

/// Tests: the GPU device counts as lost at the next vsync (see `simulate_device_lost`).
static SIMULATE_DEVICE_LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tests (the `devicelost` script step): the vsync thread treats the GPU device as lost, as after
/// a driver reset, so the recovery runs for real: new devices, atlas and swap chains, and every
/// window drawn again from scratch.
pub fn simulate_device_lost() {
    SIMULATE_DEVICE_LOST.store(true, std::sync::atomic::Ordering::Release);
    crate::wake_vsync_thread();
}

pub(crate) fn take_simulated_device_lost() -> bool {
    SIMULATE_DEVICE_LOST.swap(false, std::sync::atomic::Ordering::AcqRel)
}

thread_local! {
    /// Benchmarks: see `frame_stats`.
    static STATS: std::cell::Cell<[u64; 6]> = const { std::cell::Cell::new([0; 6]) };
}

pub(crate) const STAT_FRAMES: usize = 0;
pub(crate) const STAT_FRAME_NS: usize = 1;
pub(crate) const STAT_DRAWS: usize = 2;
pub(crate) const STAT_DRAW_NS: usize = 3;
pub(crate) const STAT_PRIMITIVES: usize = 4;
pub(crate) const STAT_DRAWN_FRAME_NS: usize = 5;

pub(crate) fn add_stat(i: usize, v: u64) {
    STATS.with(|s| {
        let mut a = s.get();
        a[i] = if i == STAT_PRIMITIVES { v } else { a[i] + v };
        s.set(a)
    });
}

/// Benchmarks: frames GPUI was asked for (WM_PAINT), UI-thread time in them (ns), scenes drawn,
/// time encoding and presenting those (ns), the last scene's primitive count, and the UI-thread
/// time of the frames that drew (ns).
pub fn frame_stats() -> [u64; 6] {
    STATS.with(|s| s.get())
}

/// Shows the window's last frame again at the next display refresh, after `copy_to_image`
/// changed an image's pixels and nothing else changed. GPUI presents the scene it already has, so
/// this costs an encode and a present (about 0.1 ms) instead of drawing the window's views again.
pub fn present_again(hwnd: isize) {
    let hwnd = windows::Win32::Foundation::HWND(hwnd as *mut c_void);
    if let Some(inner) = crate::window::window_from_hwnd(hwnd) {
        inner.state.present_wanted.set(true);
        inner.state.frame_demand.want();
    }
}

/// Copies `texture` (an `ID3D11Texture2D` on the shared device: B8G8R8A8, exactly the image's
/// size) over the atlas tile of `image`, on the GPU. False until GPUI has painted the image once.
pub fn copy_to_image(image: ImageId, texture: *mut c_void) -> bool {
    ATLAS.with(|a| {
        let Some(atlas) = a.borrow().as_ref().and_then(Weak::upgrade) else { return false };
        atlas.copy_texture_to_image(image, texture)
    })
}
