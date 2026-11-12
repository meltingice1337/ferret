//! Picture memory: what the window's pictures cost stays bounded, and is given back once they're
//! no longer shown.
//!
//! GPUI keeps a decoded copy (width × height × 4 bytes) of every `Arc<Image>` or picture file drawn
//! with `img(..)` for as long as the app runs, and a tile of its texture atlas on the GPU. A 4K
//! screenshot is 33 MB of each, so arrowing through a folder of them grew the app by gigabytes.
//! Ferret's own thumbnails and cached previews had no bound either. Here:
//! - `img` stands in for `gpui::img`: it notes each picture as drawn now, and what its decoded copy
//!   takes (from the picture's header).
//! - A sweep runs every 15 s, and a second after the folder or the preview changes or the decoded
//!   pictures pass their budget. It lets go of GPUI's decoded copy and atlas tile of every picture
//!   not drawn for a minute, then of the least recently drawn ones (big ones before thumbnails)
//!   while the total is over 256 MB. Thumbnails not asked for in a minute go the same way (the next request reads them
//!   from the thumbnail cache on disk), and cached previews not shown for two minutes or beyond
//!   their size cap. After big pictures went, the GPU driver is asked to give their memory back
//!   (`IDXGIDevice3::Trim`, at most every 30 s): it keeps it otherwise.
//! - What's on screen is never let go. The sidebar and the file lists are cached views, which
//!   reuse their last frame without drawing it again, so drawing alone doesn't say what's shown:
//!   a sweep first has the whole window drawn again (`refresh`) and only lets go of what that
//!   frame didn't draw (nor the one before). A window that doesn't draw (minimized) keeps it all
//!   until it draws again.
//!
//! Every two minutes of use the log gets a line with the process's memory and these counts
//! (target `ferret::mem`), and each sweep that let go of something says what.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gpui::{AsyncWindowContext, Context, Image, ImageFormat, ImageSource, Img, ImgResourceLoader, Resource, WeakEntity, Window};

use crate::preview::Loaded;
use crate::render::Content;
use crate::view::{Ferret, Thumb};

/// How often a sweep runs while nothing else asks for one.
const SWEEP_EVERY: Duration = Duration::from_secs(15);
/// A picture not drawn (a thumbnail not asked for) this long is let go.
const KEEP: Duration = Duration::from_secs(60);
/// Decoded pictures beyond this total are let go, least recently drawn first.
const BUDGET: u64 = 256 << 20;
/// Over budget, pictures smaller than this (thumbnails, icons) go only after the bigger ones.
const SMALL: u64 = 1 << 20;
/// Thumbnails (as PNG) beyond this total are let go, least recently asked for first.
const THUMBS_MAX: u64 = 64 << 20;
/// A cached preview (not the one shown) is let go after this long, or while the cache is bigger
/// than `PREVIEWS_MAX`, oldest first.
const PREVIEW_KEEP: Duration = Duration::from_secs(120);
/// The code highlighter's tables are let go after this long without a code or text preview.
const SYNTAX_KEEP: Duration = Duration::from_secs(120);
/// The WSL distros' search index is let go after this long without a search in this window
/// (FERRET_GPUI_WSL_KEEP=<seconds> for tests). Long: listing a big distro again takes seconds,
/// during which its files are missing from the results.
fn wsl_index_keep() -> Duration {
    static KEEP: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *KEEP.get_or_init(|| Duration::from_secs(std::env::var("FERRET_GPUI_WSL_KEEP").ok().and_then(|v| v.parse().ok()).unwrap_or(30 * 60)))
}
const PREVIEWS_MAX: u64 = 96 << 20;
/// A sweep that let go of this much asks the GPU driver to give memory back (`trim_gpu`), at
/// most this often (a trim makes the driver allocate again for what comes next).
const TRIM_AFTER: u64 = 16 << 20;
const TRIM_EVERY: Duration = Duration::from_secs(30);
/// The memory line in the log, while the window is used.
const LOG_EVERY: Duration = Duration::from_secs(120);
/// From a sweep's redraw to looking at what it drew.
const SETTLE: Duration = Duration::from_millis(300);
/// From something that asks for a sweep (another folder, another preview, over budget) to the sweep.
const SOON: Duration = Duration::from_secs(1);

const MB: f64 = (1u64 << 20) as f64;

/// What GPUI holds a decoded copy of: a picture in memory (by its id, a hash of its bytes) or a
/// picture file.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Image(u64),
    File(Resource),
}

struct Drawn {
    /// The picture while something still holds it (to read its decoded size at a sweep when its
    /// header didn't say).
    image: Option<Weak<Image>>,
    /// The frame and the time it was last drawn.
    frame: u64,
    at: Instant,
    /// What its decoded copy takes, once known.
    bytes: Option<u64>,
}

#[derive(Default)]
struct Reg {
    /// Frames drawn (the root view's renders).
    frame: u64,
    drawn: HashMap<Key, Drawn>,
    /// The known sizes of `drawn`, added up, and that total as the last sweep left it (pictures
    /// shown can keep it over budget: only growth past that asks for another sweep).
    total: u64,
    total_after: u64,
    started: bool,
    /// A sweep is coming (asked for) or running.
    soon: bool,
    sweeping: bool,
    /// What the last frame showed: the folder and the preview (a change asks for a sweep).
    path: String,
    preview: Option<String>,
    /// Cached previews: their size, and when a sweep first saw them in the cache.
    preview_sizes: HashMap<String, u64>,
    preview_seen: HashMap<String, Instant>,
    /// Frames drawn for the user (not a sweep's own redraw), and the memory line: when it was
    /// written, and that count then.
    used: u64,
    logged: Option<(Instant, u64)>,
    /// The last GPU trim, and pictures let go of since (`maybe_trim`).
    trimmed: Option<Instant>,
    trim_wanted: bool,
}

thread_local! {
    static REG: RefCell<Reg> = RefCell::new(Reg::default());
}

/// The count of frames drawn (the root view's renders).
pub fn current_frame() -> u64 {
    REG.with_borrow(|r| r.frame)
}

impl Reg {
    /// Over budget by more than the last sweep left.
    fn over_budget(&self) -> bool {
        self.total > BUDGET && self.total > self.total_after
    }
}

/// `gpui::img`, noting the picture as drawn now. Every picture drawn from an `Arc<Image>` or a file
/// goes through here (render images, such as video frames, aren't kept by GPUI and pass through).
pub fn img(source: impl Into<ImageSource>) -> Img {
    let source = source.into();
    match &source {
        ImageSource::Image(image) => note(Key::Image(image.id), Some(image)),
        ImageSource::Resource(res) => note(Key::File(res.clone()), None),
        _ => {}
    }
    gpui::img(source)
}

/// Whether GPUI has `image` decoded, so that drawing it shows it. GPUI decodes a picture the
/// first time it's asked for, in the background, and `img` draws nothing meanwhile: a picture
/// swapped in before it's decoded is an empty frame or two (a flash). Not decoded yet, it's
/// asked for here and the view being drawn is drawn again once it is; the caller draws what it
/// had meanwhile. Noted like a drawn picture, so a sweep lets go of it when it's no longer used.
pub fn decoded(image: &Arc<Image>, window: &mut Window, cx: &mut gpui::App) -> bool {
    note(Key::Image(image.id), Some(image));
    image.clone().use_render_image(window, cx).is_some()
}

fn note(key: Key, image: Option<&Arc<Image>>) {
    REG.with_borrow_mut(|r| {
        let Reg { frame, drawn, total, .. } = r;
        let now = Instant::now();
        match drawn.entry(key) {
            Entry::Occupied(mut e) => {
                let d = e.get_mut();
                d.frame = *frame;
                d.at = now;
                // Another copy of the same picture (same bytes, same id): follow the live one.
                if let Some(i) = image {
                    if d.image.as_ref().is_none_or(|w| w.strong_count() == 0) {
                        d.image = Some(Arc::downgrade(i));
                    }
                }
            }
            Entry::Vacant(e) => {
                let bytes = image.and_then(|i| decoded_size(i));
                *total += bytes.unwrap_or(0);
                e.insert(Drawn { image: image.map(Arc::downgrade), frame: *frame, at: now, bytes });
            }
        }
    });
}

/// What GPUI's decoded copy of a picture takes, from its header (BGRA, 4 bytes a pixel). Unknown
/// for SVG and for GIF and WebP, which may have many frames: read from the decoded copy instead.
fn decoded_size(image: &Image) -> Option<u64> {
    let (w, h) = match image.format {
        ImageFormat::Png => crate::files::png_size(&image.bytes)?,
        ImageFormat::Svg | ImageFormat::Gif | ImageFormat::Webp => return None,
        _ => image::ImageReader::new(std::io::Cursor::new(&image.bytes)).with_guessed_format().ok()?.into_dimensions().ok()?,
    };
    Some(w as u64 * h as u64 * 4)
}

fn render_size(r: &gpui::RenderImage) -> u64 {
    (0..r.frame_count()).map(|i| r.as_bytes(i).map_or(0, |b| b.len() as u64)).sum()
}

/// Called as the root view starts drawing a frame: counts it, starts the sweeps, and asks for one
/// soon when the folder or the preview changed or the pictures are over budget.
pub fn frame(f: &Ferret, window: &mut Window, cx: &mut Context<Ferret>) {
    let preview = f.preview_data.as_ref().map(|l| l.key.as_str());
    let path = f.pane().path.as_str();
    let (start, soon) = REG.with_borrow_mut(|r| {
        r.frame += 1;
        if !r.sweeping {
            r.used += 1;
        }
        let start = !std::mem::replace(&mut r.started, true);
        let mut changed = false;
        if r.path != path {
            r.path = path.to_string();
            changed = true;
        }
        if r.preview.as_deref() != preview {
            r.preview = preview.map(str::to_string);
            changed = true;
        }
        let soon = !start && !r.soon && (changed || r.over_budget());
        if soon {
            r.soon = true;
        }
        (start, soon)
    });
    if start {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(SWEEP_EVERY).await;
                if sweep(&this, cx).await.is_err() {
                    break;
                }
                maybe_trim();
                if this.update(cx, |f, _| log_memory(f)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }
    if soon {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SOON).await;
            REG.with_borrow_mut(|r| r.soon = false);
            let _ = sweep(&this, cx).await;
        })
        .detach();
    }
}

/// One sweep: when there's something it may let go of, the window is drawn again whole, and what
/// that frame didn't draw is let go of (`finish`).
/// Err when the window is gone.
async fn sweep(this: &WeakEntity<Ferret>, cx: &mut AsyncWindowContext) -> Result<(), ()> {
    // The code highlighter's tables, unused for a while (freed off the UI thread: some 120,000
    // small blocks).
    cx.background_executor()
        .spawn(async {
            if crate::render::forget_syntaxes(SYNTAX_KEEP) {
                log::info!(target: "ferret::mem", "let go of the code highlighter's tables (unused for {} s)", SYNTAX_KEEP.as_secs());
            }
            // The WSL distros' file lists (Ctrl+K), once this window hasn't searched for a while.
            if ferret_core::search::wsl::forget_unused(wsl_index_keep()) {
                log::info!(target: "ferret::mem", "let go of the WSL search index (no search for {} s)", wsl_index_keep().as_secs());
            }
        })
        .detach();
    if REG.with_borrow(|r| r.sweeping) {
        return Ok(());
    }
    let epoch = this.update_in(cx, |f, window, cx| begin(f, window, cx)).map_err(|_| ())?;
    let Some(epoch) = epoch else { return Ok(()) };
    REG.with_borrow_mut(|r| r.sweeping = true);
    cx.background_executor().timer(SETTLE).await;
    let done = this.update_in(cx, |f, window, cx| finish(f, epoch, window, cx));
    REG.with_borrow_mut(|r| r.sweeping = false);
    done.map_err(|_| ())
}

/// Learns the sizes the headers didn't give, then, if anything may be let go, redraws the window
/// whole and returns the frame count before that redraw.
fn begin(f: &mut Ferret, window: &mut Window, cx: &mut Context<Ferret>) -> Option<u64> {
    let unknown: Vec<Key> = REG.with_borrow(|r| r.drawn.iter().filter(|(_, d)| d.bytes.is_none()).map(|(k, _)| k.clone()).collect());
    for key in unknown {
        let image = REG.with_borrow(|r| r.drawn.get(&key).and_then(|d| d.image.as_ref().and_then(Weak::upgrade)));
        // Only pictures GPUI has (they were drawn): asking for one it doesn't have would decode it.
        let render = match &key {
            Key::Image(_) => image.and_then(|i| i.get_render_image(window, cx)),
            Key::File(res) => window.get_asset::<ImgResourceLoader>(res, cx).and_then(|r| r.ok()),
        };
        if let Some(render) = render {
            let size = render_size(&render);
            REG.with_borrow_mut(|r| {
                if let Some(d) = r.drawn.get_mut(&key) {
                    if d.bytes.is_none() {
                        d.bytes = Some(size);
                        r.total += size;
                    }
                }
            });
        }
    }
    let now = Instant::now();
    let pictures = REG.with_borrow(|r| r.over_budget() || r.drawn.values().any(|d| now.duration_since(d.at) >= KEEP));
    let thumbs = f.thumbs.may_let_go(now);
    let previews = !f.preview_cache.is_empty();
    if !(pictures || thumbs || previews) {
        return None;
    }
    // Cached previews aren't drawn: no redraw needed for them alone.
    if !(pictures || thumbs) {
        let (n, bytes) = trim_previews(f, now);
        if n > 0 {
            log::info!(target: "ferret::mem", "freed {n} cached previews, {:.1} MB", bytes as f64 / MB);
        }
        return None;
    }
    let epoch = current_frame();
    window.refresh();
    Some(epoch)
}

/// Lets go of the pictures and thumbnails the redraw (frames after `epoch`) didn't draw, and of
/// cached previews past their time or size.
fn finish(f: &mut Ferret, epoch: u64, window: &mut Window, cx: &mut Context<Ferret>) {
    let frame = current_frame();
    if frame <= epoch {
        // Not drawn since (minimized): what it shows isn't known, so nothing goes.
        return;
    }
    let now = Instant::now();
    let shown = |d_frame: u64| d_frame > epoch || d_frame + 1 >= frame;
    let (gone, freed) = REG.with_borrow_mut(|r| {
        let mut gone: Vec<(Key, Drawn)> = Vec::new();
        let stale: Vec<Key> = r.drawn.iter().filter(|(_, d)| !shown(d.frame) && now.duration_since(d.at) >= KEEP).map(|(k, _)| k.clone()).collect();
        for k in stale {
            if let Some(d) = r.drawn.remove(&k) {
                gone.push((k, d));
            }
        }
        let mut total: u64 = r.drawn.values().filter_map(|d| d.bytes).sum();
        if total > BUDGET {
            // Big pictures (previews) first, least recently drawn first: thumbnails cost little
            // and make going back to a folder instant.
            let mut lru: Vec<(bool, Instant, Key)> =
                r.drawn.iter().filter(|(_, d)| !shown(d.frame)).map(|(k, d)| (d.bytes.unwrap_or(0) < SMALL, d.at, k.clone())).collect();
            lru.sort_by_key(|(small, at, _)| (*small, *at));
            for (_, _, k) in lru {
                if total <= BUDGET {
                    break;
                }
                if let Some(d) = r.drawn.remove(&k) {
                    total -= d.bytes.unwrap_or(0);
                    gone.push((k, d));
                }
            }
        }
        r.total = total;
        r.total_after = total;
        let freed: u64 = gone.iter().filter_map(|(_, d)| d.bytes).sum();
        (gone, freed)
    });
    let pictures = gone.len();
    for (key, d) in gone {
        let_go(key, d, window, cx);
    }
    if freed >= TRIM_AFTER {
        // The next timed sweep asks the GPU driver for the memory back (`maybe_trim`), once
        // Direct3D has destroyed the dropped textures.
        REG.with_borrow_mut(|r| r.trim_wanted = true);
    }
    let (thumbs, thumb_bytes) = f.thumbs.let_go(epoch, frame, now);
    let (previews, preview_bytes) = trim_previews(f, now);
    if pictures + thumbs + previews > 0 {
        log::info!(
            target: "ferret::mem",
            "freed {pictures} pictures, {:.1} MB; {thumbs} thumbnails, {:.1} MB; {previews} cached previews, {:.1} MB",
            freed as f64 / MB,
            thumb_bytes as f64 / MB,
            preview_bytes as f64 / MB
        );
    }
}

/// Trims the GPU memory (`trim_gpu`) when big pictures were let go of since the last trim, at most
/// every `TRIM_EVERY`. Called by the timed sweeps, so the textures are gone by then.
fn maybe_trim() {
    let now = Instant::now();
    let due = REG.with_borrow_mut(|r| {
        let due = r.trim_wanted && r.trimmed.is_none_or(|t| now.duration_since(t) >= TRIM_EVERY);
        if due {
            r.trim_wanted = false;
            r.trimmed = Some(now);
        }
        due
    });
    if due {
        trim_gpu();
    }
}

/// `IDXGIDevice3::Trim` on GPUI's device: the driver gives back the video memory it keeps for
/// reuse. Pictures dropped from the atlas left their textures' memory charged to the process
/// (about 330 MB after arrowing through 4K pictures) until this.
fn trim_gpu() {
    use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice3;
    use windows::core::Interface;
    let Some((device, context)) = gpui_windows::ferret::shared_device() else { return };
    // SAFETY: each pointer carries a reference we now own (`shared_device`).
    let (device, context) = unsafe { (ID3D11Device::from_raw(device), ID3D11DeviceContext::from_raw(context)) };
    unsafe {
        context.Flush();
        if let Ok(dxgi) = device.cast::<IDXGIDevice3>() {
            dxgi.Trim();
        }
    }
}

/// Lets go of GPUI's decoded copy of a picture and of its atlas tile. Only pictures GPUI has are
/// asked for (they were drawn), so this never decodes one; a picture whose bytes nothing holds any
/// more is found by its id alone.
fn let_go(key: Key, d: Drawn, window: &mut Window, cx: &mut Context<Ferret>) {
    match key {
        Key::Image(id) => {
            let image = d.image.and_then(|w| w.upgrade()).unwrap_or_else(|| Arc::new(Image { format: ImageFormat::Png, bytes: Vec::new(), id }));
            let render = image.clone().get_render_image(window, cx);
            image.remove_asset(cx);
            if let Some(render) = render {
                cx.drop_image(render, Some(window));
            }
        }
        Key::File(res) => {
            let render = window.get_asset::<ImgResourceLoader>(&res, cx).and_then(|r| r.ok());
            cx.remove_asset::<ImgResourceLoader>(&res);
            if let Some(render) = render {
                cx.drop_image(render, Some(window));
            }
        }
    }
}

/// Cached previews (never the one shown): those in the cache for `PREVIEW_KEEP` go, then the
/// oldest while the cache is over `PREVIEWS_MAX`.
fn trim_previews(f: &mut Ferret, now: Instant) -> (usize, u64) {
    REG.with_borrow_mut(|r| {
        let Reg { preview_sizes, preview_seen, .. } = r;
        for l in &f.preview_cache {
            preview_sizes.entry(l.key.clone()).or_insert_with(|| content_size(&l.content));
            preview_seen.entry(l.key.clone()).or_insert(now);
        }
        let size = |l: &Loaded| preview_sizes.get(&l.key).copied().unwrap_or(0);
        let mut total: u64 = f.preview_cache.iter().map(size).sum();
        let (mut n, mut freed) = (0, 0);
        let mut i = 0;
        while i < f.preview_cache.len() {
            let l = &f.preview_cache[i];
            let old = preview_seen.get(&l.key).is_some_and(|t| now.duration_since(*t) >= PREVIEW_KEEP);
            if old || total > PREVIEWS_MAX {
                let s = size(l);
                total -= s;
                freed += s;
                n += 1;
                f.preview_cache.remove(i);
            } else {
                i += 1;
            }
        }
        let live = |k: &String| f.preview_cache.iter().any(|l| l.key == *k);
        preview_sizes.retain(|k, _| live(k));
        preview_seen.retain(|k, _| live(k));
        (n, freed)
    })
}

/// About what a preview's content takes (its own copy: decoded pictures are counted above).
fn content_size(c: &Content) -> u64 {
    fn image(i: &Arc<Image>) -> u64 {
        i.bytes.len() as u64
    }
    fn lines(l: &[crate::render::Line]) -> u64 {
        l.iter().map(|l| l.text.len() as u64 + l.colors.len() as u64 * 32 + 48).sum()
    }
    fn rows(r: &[Vec<String>]) -> u64 {
        r.iter().map(|r| r.iter().map(|c| c.len() as u64 + 24).sum::<u64>() + 24).sum()
    }
    match c {
        Content::Picture { image: i, .. } | Content::Shell(i) => image(i),
        Content::Audio(_, i) | Content::Program(_, i) => i.as_ref().map_or(0, image),
        Content::Text { lines: l, .. } => lines(l),
        // The parsed blocks or tree take about as much again as the text.
        Content::Markdown { source, .. } | Content::Json { source, .. } => 2 * lines(source),
        Content::BadJson { source, .. } => lines(source),
        Content::Csv { rows: r, .. } => rows(r),
        Content::Sheet(s) => rows(&s.rows),
        Content::Deck(_, pics, drawn) => {
            pics.iter().flatten().map(image).sum::<u64>() + drawn.iter().flatten().flatten().map(|d| image(&d.image)).sum::<u64>()
        }
        Content::Font { bytes, .. } => bytes.as_ref().map_or(0, |b| b.len() as u64),
        Content::Pdf { pages, .. } => pages.iter().filter_map(|(i, _)| i.as_ref()).map(image).sum(),
        Content::Hex(b) => b.len() as u64,
        Content::DocText(p) => p.iter().map(|(_, t)| t.len() as u64 + 32).sum(),
        _ => 0,
    }
}

/// Ferret's thumbnails, by key (path, size, date): when each was last asked for, so the ones no
/// longer shown can go.
#[derive(Default)]
pub struct Thumbs {
    map: HashMap<String, Kept>,
    /// The PNGs held, added up, and as the last sweep left them.
    bytes: u64,
    bytes_after: u64,
}

struct Kept {
    thumb: Thumb,
    frame: u64,
    at: Instant,
}

fn thumb_bytes(t: &Thumb) -> u64 {
    match t {
        Thumb::Ready(i) => i.bytes.len() as u64,
        _ => 0,
    }
}

impl Thumbs {
    /// The thumbnail for `key`, noted as asked for now.
    pub fn get(&mut self, key: &str) -> Option<&Thumb> {
        let frame = current_frame();
        let k = self.map.get_mut(key)?;
        k.frame = frame;
        k.at = Instant::now();
        Some(&k.thumb)
    }

    pub fn insert(&mut self, key: String, thumb: Thumb) {
        self.bytes += thumb_bytes(&thumb);
        let kept = Kept { thumb, frame: current_frame(), at: Instant::now() };
        if let Some(old) = self.map.insert(key, kept) {
            self.bytes -= thumb_bytes(&old.thumb);
        }
    }

    fn may_let_go(&self, now: Instant) -> bool {
        (self.bytes > THUMBS_MAX && self.bytes > self.bytes_after) || self.map.values().any(|k| !matches!(k.thumb, Thumb::Loading) && now.duration_since(k.at) >= KEEP)
    }

    /// Lets go of the thumbnails the redraw (frames after `epoch`) didn't ask for: those not asked
    /// for in `KEEP`, then the oldest while over `THUMBS_MAX`. Ones being made stay.
    fn let_go(&mut self, epoch: u64, frame: u64, now: Instant) -> (usize, u64) {
        let can_go = |k: &Kept| !matches!(k.thumb, Thumb::Loading) && k.frame <= epoch && k.frame + 1 < frame;
        let before = (self.map.len(), self.bytes);
        self.map.retain(|_, k| !(can_go(k) && now.duration_since(k.at) >= KEEP));
        self.bytes = self.map.values().map(|k| thumb_bytes(&k.thumb)).sum();
        if self.bytes > THUMBS_MAX {
            let mut lru: Vec<(Instant, String)> = self.map.iter().filter(|(_, k)| can_go(k)).map(|(key, k)| (k.at, key.clone())).collect();
            lru.sort_by_key(|(at, _)| *at);
            for (_, key) in lru {
                if self.bytes <= THUMBS_MAX {
                    break;
                }
                if let Some(k) = self.map.remove(&key) {
                    self.bytes -= thumb_bytes(&k.thumb);
                }
            }
        }
        self.bytes_after = self.bytes;
        (before.0 - self.map.len(), before.1.saturating_sub(self.bytes))
    }

    fn ready(&self) -> usize {
        self.map.values().filter(|k| matches!(k.thumb, Thumb::Ready(_))).count()
    }
}

/// The process's private bytes and working set.
fn process_memory() -> (u64, u64) {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32) };
    if ok.is_err() {
        return (0, 0);
    }
    (c.PrivateUsage as u64, c.WorkingSetSize as u64)
}

/// The memory line: every `LOG_EVERY` while the window is used (an idle window logs once more,
/// then waits for use; a sweep's own redraw isn't use).
fn log_memory(f: &Ferret) {
    let now = Instant::now();
    let due = REG.with_borrow(|r| match r.logged {
        None => true,
        Some((at, used)) => now.duration_since(at) >= LOG_EVERY && used != r.used,
    });
    if !due {
        return;
    }
    let (private, working) = process_memory();
    let (pictures, decoded) = REG.with_borrow_mut(|r| {
        r.logged = Some((now, r.used));
        (r.drawn.len(), r.total)
    });
    let previews: u64 = f.preview_cache.iter().map(|l| content_size(&l.content)).sum::<u64>() + f.preview_data.as_ref().map_or(0, |l| content_size(&l.content));
    let preview_n = f.preview_cache.len() + f.preview_data.is_some() as usize;
    log::info!(
        target: "ferret::mem",
        "memory: private {:.0} MB, working set {:.0} MB; thumbnails {} ({:.1} MB); decoded pictures {} ({:.1} MB); previews {} ({:.1} MB)",
        private as f64 / MB,
        working as f64 / MB,
        f.thumbs.ready(),
        f.thumbs.bytes as f64 / MB,
        pictures,
        decoded as f64 / MB,
        preview_n,
        previews as f64 / MB
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbaImage::new(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn decoded_size_from_headers() {
        assert_eq!(decoded_size(&Image::from_bytes(ImageFormat::Png, png(300, 200))), Some(300 * 200 * 4));
        let mut jpg = Vec::new();
        image::RgbImage::new(64, 48).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
        assert_eq!(decoded_size(&Image::from_bytes(ImageFormat::Jpeg, jpg)), Some(64 * 48 * 4));
        assert_eq!(decoded_size(&Image::from_bytes(ImageFormat::Svg, b"<svg/>".to_vec())), None);
    }

    #[test]
    fn thumbnails_go_when_not_asked_for() {
        let mut t = Thumbs::default();
        let img = |n: u8| Thumb::Ready(Arc::new(Image::from_bytes(ImageFormat::Png, vec![n; 100])));
        t.insert("a".into(), img(1));
        t.insert("b".into(), img(2));
        t.insert("c".into(), Thumb::Loading);
        assert_eq!(t.bytes, 200);
        let later = Instant::now() + KEEP + Duration::from_secs(1);
        // "a" asked for in the redraw (frame 6 > epoch 5): it stays; "b" goes; "c" is being made.
        t.map.get_mut("a").unwrap().frame = 6;
        let (n, bytes) = t.let_go(5, 7, later);
        assert_eq!((n, bytes), (1, 100));
        assert!(t.map.contains_key("a") && t.map.contains_key("c") && !t.map.contains_key("b"));
        // Replacing an entry keeps the byte count right.
        t.insert("a".into(), img(3));
        assert_eq!(t.bytes, 100);
    }
}
