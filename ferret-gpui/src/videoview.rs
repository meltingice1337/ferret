//! The preview pane's video, as its own GPUI view. A new frame is copied on the GPU into the
//! image's atlas tile and the window presents its last scene again: no view is drawn. The view
//! is drawn again only when its controls change (the clock, the seek bar up to 4 times a second,
//! ▶/⏸), and then the sidebar and the file lists are reused from the last frame (they're cached
//! views). That's what keeps playback cheap on the CPU.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt, Bounds, ClickEvent, Context, EventEmitter, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point,
    RenderImage, Window, canvas, div, prelude::*, px, relative,
};

// Posters are let go once not drawn for a while (imgmem.rs); frames pass through as they are.
use crate::imgmem::img;

use crate::bench;
use crate::theme as t;
use crate::video::{self, Player};
use crate::ui::icon;

pub enum VideoEvent {
    /// The compress button.
    Compress,
}

/// How long a poster takes to fade out over the first frame that arrived under it.
const POSTER_OUT: Duration = Duration::from_millis(170);

/// What a video showed last, as a still picture: the preview keeps it on screen while what
/// comes next is on its way, then fades it out under that.
#[derive(Clone)]
pub enum Still {
    /// Its last frame (the player's image, which stays on the GPU until it's dropped).
    Frame(Arc<RenderImage>),
    /// Its poster: no frame had arrived.
    Poster(Arc<gpui::Image>),
}

impl Still {
    pub fn element(&self) -> gpui::Img {
        match self {
            Still::Frame(image) => img(image.clone()),
            Still::Poster(image) => img(image.clone()).object_fit(gpui::ObjectFit::Contain),
        }
    }

    /// The player's image, to drop from GPUI's cache once the still isn't drawn any more.
    pub fn frame(&self) -> Option<Arc<RenderImage>> {
        match self {
            Still::Frame(image) => Some(image.clone()),
            Still::Poster(_) => None,
        }
    }
}

pub struct VideoView {
    pub path: String,
    player: Result<Player, String>,
    /// Inner width it fits into, logical pixels.
    width: f32,
    /// Shown size, logical pixels.
    size: (f32, f32),
    bar: Rc<Cell<Bounds<Pixels>>>,
    /// The space it has (it fits the video into it, both ways), as of the last frame.
    area: Rc<Cell<Bounds<Pixels>>>,
    scrubbing: bool,
    polling: bool,
    /// Playing as of the last check (it ending redraws the ▶ button).
    was_playing: bool,
    /// The controls as last drawn, and when. A new frame that leaves them as they are is
    /// presented again without drawing any view.
    shown: Controls,
    drawn_at: Instant,
    bench: Option<PathBuf>,
    /// Benchmark: frames that reached the screen, since when, and the views drawn and the
    /// window's frame statistics by then.
    counted: Option<(Instant, u64, [u64; 4], [u64; 6])>,
    /// Benchmark (FERRET_GPUI_VIDEO_TRACE): every check for a frame: when (ms), how long the
    /// update took (ms), the frame's time stamp, and whether it was new.
    trace: Option<Vec<(f64, f64, i64, bool)>>,
    /// GPUI's device generation the player was opened for (see `reopen_if_device_changed`).
    generation: u64,
    /// The video's thumbnail, shown until its first frame is ready (the preview sets it).
    pub poster: Option<std::sync::Arc<gpui::Image>>,
    /// The preview is keeping what it showed before on screen while this video's first frame is
    /// on its way (`content_view`): the view is drawn over that, showing nothing itself yet. A
    /// poster shown at once was a third picture between two videos: the last video, the next
    /// one's thumbnail (Windows takes it from further into the video), then its first frame.
    pub quiet: bool,
    /// Where the picture is drawn in the window, as of the last frame.
    screen: Rc<Cell<Bounds<Pixels>>>,
    opened: Instant,
    /// The poster was drawn (the first frame took longer than the preview waits for it).
    poster_shown: bool,
    /// When the first frame arrived under a poster that was showing: the poster fades out over
    /// it instead of being cut to it.
    poster_out: Option<Instant>,
}

impl EventEmitter<VideoEvent> for VideoView {}

impl VideoView {
    pub fn new(path: String, width: f32, bench: Option<PathBuf>) -> Self {
        let muted = std::env::var_os("FERRET_GPUI_MUTE").is_some();
        let generation = gpui_windows::ferret::device_generation();
        let mut player = Player::open(&path, muted);
        if std::env::var_os("FERRET_GPUI_AUTOPLAY").is_some() {
            if let Ok(p) = player.as_mut() {
                p.play();
            }
        }
        let trace = std::env::var_os("FERRET_GPUI_VIDEO_TRACE").map(|_| Vec::new());
        Self {
            path,
            player,
            width,
            size: (0., 0.),
            bar: Rc::default(),
            area: Rc::default(),
            scrubbing: false,
            polling: false,
            was_playing: false,
            shown: Controls::default(),
            drawn_at: Instant::now(),
            bench,
            counted: None,
            trace,
            generation,
            poster: None,
            quiet: false,
            screen: Rc::default(),
            opened: Instant::now(),
            poster_shown: false,
            poster_out: None,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.player.is_ok()
    }

    /// Its first frame is on screen, or there never will be one (it can't be played).
    pub fn settled(&self) -> bool {
        match &self.player {
            Ok(p) => p.frames > 0 || p.error().is_some(),
            Err(_) => true,
        }
    }

    /// What it shows now as a still, and where in the space it has. None while it shows
    /// nothing of its own (no frame yet, and no poster drawn).
    pub fn still(&self) -> Option<(Still, Bounds<Pixels>)> {
        let (screen, area) = (self.screen.get(), self.area.get());
        if screen.size.width <= px(0.) || screen.size.height <= px(0.) {
            return None;
        }
        let still = match self.player.as_ref().ok() {
            Some(p) if p.frames > 0 => Still::Frame(p.image()?),
            _ if self.poster_shown => Still::Poster(self.poster.clone()?),
            _ => return None,
        };
        Some((still, Bounds::new(screen.origin - area.origin, screen.size)))
    }

    /// The image GPUI holds for the current frame, to drop from its cache when this view goes.
    /// The space the picture had (switching to another video starts from it, so it's sized on
    /// its first frame instead of after a measuring one).
    pub fn area(&self) -> Bounds<Pixels> {
        self.area.get()
    }

    pub fn set_area(&self, b: Bounds<Pixels>) {
        self.area.set(b);
    }

    pub fn image(&self) -> Option<Arc<RenderImage>> {
        self.player.as_ref().ok().and_then(Player::image)
    }

    pub fn pause(&mut self) {
        if let Ok(p) = self.player.as_mut() {
            p.pause();
        }
    }

    pub fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if let Ok(p) = self.player.as_mut() {
            if p.playing() { p.pause() } else { p.play() }
            cx.notify();
        }
    }

    /// While frames are wanted, checks for a new one at every display refresh. That check is
    /// cheap (0.02 ms). A new frame is already on the GPU, in the image's atlas tile, so the window
    /// only presents its last scene again (0.1 ms). Drawing the window again (its root, the
    /// toolbar, the preview pane: about 1.3 ms) waits for the controls to change: the clock each
    /// second, the seek bar at most 4 times a second, the ▶ button.
    fn schedule_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.polling || !self.player.as_ref().is_ok_and(Player::needs_frames) {
            return;
        }
        self.polling = true;
        // Weak: a strong handle here kept a video the preview had let go of alive (and playing,
        // its frames never reaching the atlas again, so it kept asking), one per video passed.
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |v, cx| {
                v.polling = false;
                match v.update_frame(window) {
                    Show::Redraw => cx.notify(),
                    Show::Present => match crate::view::hwnd(window) {
                        Some(hwnd) => gpui_windows::ferret::present_again(hwnd),
                        None => cx.notify(),
                    },
                    Show::Nothing => {}
                }
                v.schedule_poll(window, cx);
            });
        });
    }

    /// After a GPU reset the player decodes on the old device, whose frames GPUI's new atlas
    /// can't take (the picture went blank). Opens the video again on the new device, where it
    /// was and playing if it was.
    fn reopen_if_device_changed(&mut self, window: &mut Window) -> bool {
        let now = gpui_windows::ferret::device_generation();
        if now == self.generation {
            return false;
        }
        self.generation = now;
        // A player that failed to open (on a device being reset) is tried again too.
        let (at, playing, muted, volume) = match self.player.as_ref() {
            Ok(p) => (p.position(), p.playing(), p.muted(), p.volume()),
            Err(_) => (0., false, std::env::var_os("FERRET_GPUI_MUTE").is_some(), 1.),
        };
        if let Some(old) = self.player.as_ref().ok().and_then(Player::image) {
            let _ = window.drop_image(old);
        }
        let mut fresh = Player::open(&self.path, muted);
        if let Ok(n) = fresh.as_mut() {
            n.set_volume(volume);
            if at > 0. {
                n.seek(at);
            }
            if playing {
                n.play();
            }
        }
        self.player = fresh;
        true
    }

    /// Sizes the picture to the space it has (less the controls), keeping its shape (`shape`: the
    /// video's own once it's open, else its thumbnail's): a tall phone recording mustn't run over
    /// what's above and below it. Only once that space is known: guessed (180 px tall), the
    /// thumbnail showed cropped to the guess, then the picture jumped to its real size, a
    /// zoom-in-and-out flash on every switch between videos (user report, 2026-11-11).
    fn fit(&mut self, shape: Option<(u32, u32)>) {
        let shape = shape.or_else(|| self.poster.as_ref().and_then(|i| crate::files::png_size(i.bytes())));
        let area = self.area.get().size;
        if let Some((w, h)) = shape.filter(|_| area.width.as_f32() > 0.) {
            let (aw, ah) = ((area.width.as_f32() - 28.).max(40.), (area.height.as_f32() - 50.).max(60.));
            let fit = (aw / w as f32).min(ah / h as f32);
            self.size = ((w as f32 * fit).round(), (h as f32 * fit).round());
        }
    }

    /// Brings the shown frame up to date (on the GPU), and says what that needs shown.
    fn update_frame(&mut self, window: &mut Window) -> Show {
        if self.reopen_if_device_changed(window) {
            return Show::Redraw;
        }
        let Ok(p) = self.player.as_ref() else { return Show::Nothing };
        let sized = self.size.0 > 0.;
        // The picture's shape: the video's own once it's open, its thumbnail's until then.
        let shape = p.native_size();
        self.fit(shape);
        let Ok(p) = self.player.as_mut() else { return Show::Nothing };
        let scale = window.scale_factor();
        let (dw, dh) = ((self.size.0 * scale).round() as u32, (self.size.1 * scale).round() as u32);
        let before = p.frames;
        let had_image = p.image().is_some();
        let began = Instant::now();
        // Tests of what shows while a video's first frame is slow to come: FERRET_GPUI_SLOW_VIDEO=<ms>.
        if before == 0 && slow_video().is_some_and(|ms| self.opened.elapsed() < ms) {
            return Show::Nothing;
        }
        let replaced = p.update(dw, dh);
        if let (Some(trace), Some((since, ..))) = (self.trace.as_mut(), self.counted) {
            let at = began.duration_since(since).as_secs_f64() * 1000.;
            trace.push((at, began.elapsed().as_secs_f64() * 1000., p.pts().unwrap_or(-1), p.frames != before));
        }
        let playing = p.playing();
        let new_frame = p.frames != before;
        if before == 0 && new_frame {
            let (path, took, quiet) = (&self.path, self.opened.elapsed().as_millis(), self.quiet);
            crate::watchdog::trace("video", || format!("{path}: first frame {took} ms after it was opened{}", if quiet { ", what was there kept until now" } else { "" }));
        }
        // The controls move along with the frames, so they're checked when one arrives. The clock
        // is drawn when its second changes, the seek bar's fill at most 4 times a second (as a
        // browser's player does, on `timeupdate`).
        let controls_moved = new_frame && {
            let now = Controls::at(p.position(), p.duration(), self.bar.get(), scale);
            (now.position, now.duration) != (self.shown.position, self.shown.duration)
                || (now.filled_px != self.shown.filled_px && self.drawn_at.elapsed() >= Duration::from_millis(250))
        };
        // (The first frame: the preview stops keeping what it showed before, and a poster goes.)
        let redraw = replaced.is_some()
            || (before == 0 && p.frames > 0)
            || had_image != p.image().is_some()
            || sized != (self.size.0 > 0.)
            || playing != std::mem::replace(&mut self.was_playing, playing)
            || controls_moved;
        let show = if redraw {
            Show::Redraw
        } else if new_frame {
            Show::Present
        } else {
            Show::Nothing
        };
        if let Some(old) = replaced {
            let _ = window.drop_image(old);
        }
        if let (Some(out), Some(err)) = (self.bench.as_ref(), p.last_error.take()) {
            bench::write_to(out, "video_error", format!("{err} (image {})", p.image().is_some()));
        }
        // Benchmark: how many frames reached the screen over FERRET_GPUI_VIDEO_SECS seconds, or
        // until the video ended if that came first (it plays once; `video_ended` says so).
        if let (Some(out), Some(secs)) = (self.bench.as_ref(), std::env::var("FERRET_GPUI_VIDEO_SECS").ok().and_then(|s| s.parse::<f64>().ok())) {
            if p.frames > before && self.counted.is_none() {
                self.counted = Some((Instant::now(), p.frames, bench::renders(), gpui_windows::ferret::frame_stats()));
            }
            if let Some((since, first, drawn, stats)) = self.counted {
                let took = since.elapsed().as_secs_f64();
                let ended = p.ended();
                if (took >= secs && p.playing()) || ended {
                    self.counted = None;
                    if ended {
                        bench::write_to(out, "video_ended", 1);
                    }
                    bench::write_to(out, "video_frames", p.frames - first);
                    bench::write_to(out, "video_secs", format!("{took:.2}"));
                    bench::write_to(out, "video_fps", format!("{:.1}", (p.frames - first) as f64 / took));
                    bench::write_to(out, "video_size", format!("{dw}x{dh}"));
                    // What a new frame redrew: views drawn while counting (a frame should draw
                    // the video and the root, not the sidebar or the file lists).
                    let now = bench::renders();
                    let d = |i: usize| now[i] - drawn[i];
                    bench::write_to(
                        out,
                        "video_renders",
                        format!("root:{} sidebar:{} files:{} video:{}", d(bench::ROOT), d(bench::SIDEBAR), d(bench::FILES), d(bench::VIDEO)),
                    );
                    bench::write_to(out, "video_a11y", window.is_a11y_active());
                    // The window's frames meanwhile: how many GPUI was asked for and their UI-thread
                    // time, how many it drew and the time encoding and presenting those, and the
                    // last scene's size.
                    let s = gpui_windows::ferret::frame_stats();
                    let (requests, draws) = (s[0] - stats[0], s[2] - stats[2]);
                    let ms = |ns: u64, n: u64| ns as f64 / 1e6 / n.max(1) as f64;
                    bench::write_to(
                        out,
                        "video_frame_stats",
                        format!(
                            "requests:{requests} ms_each:{:.3} drawing_ms_each:{:.3} idle_ms_each:{:.3} draws:{draws} encode_present_ms_each:{:.3} primitives:{}",
                            ms(s[1] - stats[1], requests),
                            ms(s[5] - stats[5], draws),
                            ms((s[1] - stats[1]) - (s[5] - stats[5]), requests - draws),
                            ms(s[3] - stats[3], draws),
                            s[4]
                        ),
                    );
                    if let (Some(trace), Some(file)) = (self.trace.take(), std::env::var_os("FERRET_GPUI_VIDEO_TRACE")) {
                        let lines: Vec<String> = trace.iter().map(|(at, took, pts, new)| format!("{at:.2}\t{took:.3}\t{pts}\t{}", *new as u8)).collect();
                        let _ = std::fs::write(file, lines.join("\n"));
                    }
                    bench::write_to(out, "video_done", 1);
                    p.pause();
                }
            }
        }
        show
    }

    fn scrub(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let b = self.bar.get();
        let Ok(p) = self.player.as_mut() else { return };
        let duration = p.duration();
        if b.size.width.as_f32() <= 0. || !duration.is_finite() {
            return;
        }
        let f = ((at.x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
        p.seek(f as f64 * duration);
        cx.notify();
    }
}

fn slow_video() -> Option<Duration> {
    static SLOW: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *SLOW.get_or_init(|| std::env::var("FERRET_GPUI_SLOW_VIDEO").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis))
}

/// What a check for a new frame needs shown.
enum Show {
    Nothing,
    /// A new frame in the image's atlas tile, nothing else: present the last scene again.
    Present,
    /// The view looks different: draw it again.
    Redraw,
}

/// What the controls show: the clock's whole seconds (position, duration) and how much of the
/// seek bar is filled, in device pixels.
#[derive(Clone, Copy, Default, PartialEq)]
struct Controls {
    position: u64,
    duration: u64,
    filled_px: i32,
}

impl Controls {
    /// At `position` of `duration` (seconds), with the seek bar at `bar` (as last laid out).
    fn at(position: f64, duration: f64, bar: Bounds<Pixels>, scale: f32) -> Self {
        let secs = |s: f64| if s.is_finite() { s.max(0.) as u64 } else { 0 };
        Self {
            position: secs(position),
            duration: secs(duration),
            filled_px: (progress(position, duration) * bar.size.width.as_f32() * scale).round() as i32,
        }
    }
}

/// How far into the video, 0–1.
fn progress(position: f64, duration: f64) -> f32 {
    if duration.is_finite() && duration > 0. { (position / duration).clamp(0., 1.) as f32 } else { 0. }
}

fn button(id: &'static str, icon_name: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group("vbtn")
        .flex_none()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .hover(|s| s.bg(t::hover()))
        .active(|s| s.bg(t::press()))
        .child(icon(icon_name, 16., t::fg2()).group_hover("vbtn", |s| s.text_color(t::fg())))
}

impl Render for VideoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        bench::rendered(bench::VIDEO);
        self.update_frame(window);
        self.schedule_poll(window, cx);
        let message = |text: String| div().px(px(14.)).py(px(20.)).text_color(t::fg3()).child(text).into_any_element();
        let p = match &self.player {
            Ok(p) => p,
            Err(e) => return message(format!("Can't play this video: {e}")),
        };
        if let Some(code) = p.error() {
            return message(format!("Windows can't play this video (media error {code})"));
        }
        // Sized once both the picture's shape and its space are known (`update_frame`); until
        // then nothing is shown in a guessed box (one frame, usually).
        let ready = self.size.0 > 0.;
        let (w, h) = if ready { self.size } else { (self.width, 180.) };
        let playing = p.playing();
        let duration = p.duration();
        let position = p.position();
        let frac = progress(position, duration);
        self.shown = Controls::at(position, duration, self.bar.get(), window.scale_factor());
        self.drawn_at = Instant::now();
        let has_frame = p.frames > 0;
        // Quiet: nothing of its own shows until the first frame (the preview draws what it
        // showed before underneath). The frame's picture is still drawn: it's empty until then,
        // and has to be drawn once to get its place on the GPU.
        let quiet = self.quiet && !has_frame && p.error().is_none();
        if has_frame && self.poster_shown && self.poster_out.is_none() {
            self.poster_out = Some(Instant::now());
        }
        let poster_fading = t::animate() && self.poster_out.is_some_and(|at| at.elapsed() < POSTER_OUT);
        let screen = match (p.image(), self.poster.clone().filter(|_| !quiet)) {
            (Some(image), None) => img(image).w(px(w)).h(px(h)).rounded(px(8.)).into_any_element(),
            (Some(image), Some(_)) if has_frame && !poster_fading => img(image).w(px(w)).h(px(h)).rounded(px(8.)).into_any_element(),
            // The frame's picture exists before a frame is in it (it has to be drawn once to get
            // its place on the GPU, which the frames are then copied into): the poster covers it
            // until the first one is there, or it showed empty for a frame or two (2026-11-11);
            // then it fades out over that frame.
            (Some(image), Some(poster)) => {
                self.poster_shown = true;
                let over = img(poster).absolute().top_0().left_0().w(px(w)).h(px(h)).rounded(px(8.)).object_fit(gpui::ObjectFit::Contain);
                div()
                    .relative()
                    .w(px(w))
                    .h(px(h))
                    .child(img(image).absolute().top_0().left_0().w(px(w)).h(px(h)).rounded(px(8.)))
                    .child(if has_frame {
                        over.with_animation("poster-out", gpui::Animation::new(POSTER_OUT), |d, k| d.opacity(1. - k)).into_any_element()
                    } else {
                        over.into_any_element()
                    })
                    .into_any_element()
            }
            // Until the first frame: the video's thumbnail (the app's poster), not an empty
            // stage with a play button floating on it.
            (None, Some(poster)) => {
                self.poster_shown = true;
                img(poster).w(px(w)).h(px(h)).rounded(px(8.)).object_fit(gpui::ObjectFit::Contain).into_any_element()
            }
            (None, None) if quiet => div().w(px(w)).h(px(h)).into_any_element(),
            // Nothing to show yet: the picture's place, breathing, with a turning spinner (the
            // loader icon stood still).
            (None, None) => crate::ui::skeleton("video-loading", crate::ui::Skeleton::Picture { w, h }),
        };
        // The play button follows the picture: a quarter of its height, 36 to 50 px.
        let play = (h * 0.26).clamp(36., 50.);
        let bar = self.bar.clone();
        let area = self.area.clone();
        let screen_at = self.screen.clone();
        let me = cx.entity_id();
        div()
            .size_full()
            .relative()
            .px(px(14.))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(8.))
            .child(
                gpui::canvas(
                    move |b, _, cx| {
                        let old = area.get();
                        area.set(b);
                        if (old.size.width - b.size.width).abs() > px(0.5) || (old.size.height - b.size.height).abs() > px(0.5) {
                            cx.defer(move |cx| cx.notify(me));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                // From the view's own corner (without, it starts after the padding, and a still
                // placed from it was 14 px to the left of where the picture had been).
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                // GPUI draws over the video like anything else (a web view in a window can't).
                div()
                    .id("video-screen")
                    .relative()
                    // Centred across (a portrait video is narrower than the pane).
                    .self_center()
                    .w(px(w))
                    .h(px(h))
                    .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.toggle_play(cx)))
                    .when(!ready, |d| d.opacity(0.))
                    .child(canvas(move |b, _, _| screen_at.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                    .child(screen)
                    .when(!playing && ready && !quiet, |d| {
                        d.child(
                            // The app's play button (.pv-play): the theme's accent, a filled
                            // triangle nudged right to look centred, a soft shadow; fuller on hover.
                            div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().child(
                                div()
                                    .id("video-play")
                                    .size(px(play))
                                    .rounded_full()
                                    .bg({
                                        let mut a = t::accent();
                                        a.a = 0.85;
                                        a
                                    })
                                    .hover(|s| s.bg(t::accent()))
                                    .shadow(vec![gpui::BoxShadow {
                                        color: t::rgba(0, 0.5),
                                        offset: gpui::point(px(0.), px(6.)),
                                        blur_radius: px(24.),
                                        spread_radius: px(0.),
                                        inset: false,
                                    }])
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .pl(px(play * 0.06))
                                    .child(icon("play-fill", play * 0.4, t::on_accent())),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(quiet, |d| d.invisible())
                    .child(button("play-pause", if playing { "pause" } else { "play" }).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.toggle_play(cx))))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(t::fg2())
                            .child(format!("{} / {}", video::clock(position), video::clock(duration))),
                    )
                    .child(button("compress", "file-down").on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(VideoEvent::Compress))))
                    .child(
                        div()
                            .id("seek")
                            .relative()
                            .flex_1()
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .child(canvas(move |b, _, _| bar.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                            .child(
                                div().w_full().h(px(4.)).rounded(px(4.)).bg(t::line2()).child(
                                    div().h_full().w(relative(frac)).rounded(px(4.)).bg(t::accent()),
                                ),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|v, ev: &MouseDownEvent, _, cx| {
                                    v.scrubbing = true;
                                    v.scrub(ev.position, cx)
                                }),
                            )
                            .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
                                v.scrubbing &= ev.pressed_button == Some(MouseButton::Left);
                                if v.scrubbing {
                                    v.scrub(ev.position, cx)
                                }
                            })),
                    ),
            )
            .into_any_element()
    }
}
