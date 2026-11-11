//! Compress videos: the app's media editor (MediaEditor.tsx, `VideoEditor`), on ferret-core's
//! media code, in the shared look of `mx.rs`.
//! - The original plays on a dark stage, fitted, with zoom (wheel, double-click, Fit / 100%: the
//!   GPU scales the part shown from the decoded frame), pan, and full screen (F).
//! - A preview encodes a few seconds at the playhead (`media::video_sample`), by itself when the
//!   compressor opens and after the settings change; then
//!   Original / Compare / Compressed views, a divider to drag, and the quality check of that
//!   sample (quality.rs) with its verdict.
//! - A filmstrip timeline with trim handles (I / O), the settings column (presets with the sizes
//!   they come out at, by quality or by file size, resolution, frame rate, format, sound, name),
//!   and the size it will come out at in the footer.
//! - Compress hands the jobs to the window (`EditorEvent::Compress`): progress on the jobs card,
//!   a toast with Undo at the end, as the app.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use ferret_core::media::{self, VideoInfo, VideoJob, VideoSample};
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Image, ImageFormat,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, Pixels, Point, RenderImage, SharedString,
    Subscription, Window, canvas, div, img, prelude::*, px, relative,
};

use crate::bench;
use crate::input::{Changed, TextInput};
use crate::kinds;
use crate::mx::{self, On};
use crate::quality::{self, Level};
use crate::theme as t;
use crate::ui::{self, icon};
use crate::video::Player;

pub enum EditorEvent {
    Close,
    /// Compress these (the window runs the job, shows its progress and records the undo).
    Compress { jobs: Vec<VideoJob>, before: u64, estimate: u64 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// Short side in pixels (0 keeps the size).
    pub res: u32,
    /// 0 keeps the frame rate.
    pub fps: u32,
    /// Aim for `target_mb` instead of a quality.
    pub by_size: bool,
    /// 0–100.
    pub quality: f64,
    pub target_mb: f64,
    pub hevc: bool,
    /// kbps.
    pub audio: u32,
}

pub struct Preset {
    pub label: &'static str,
    pub hint: &'static str,
    pub s: Settings,
}

/// The app's presets (MediaEditor.tsx).
pub const PRESETS: [Preset; 4] = [
    Preset { label: "Small", hint: "720p · 30 fps", s: Settings { res: 720, fps: 30, by_size: false, quality: 28., target_mb: 10., hevc: false, audio: 96 } },
    Preset { label: "Balanced", hint: "1080p · 30 fps", s: Settings { res: 1080, fps: 30, by_size: false, quality: 50., target_mb: 10., hevc: false, audio: 128 } },
    Preset { label: "High quality", hint: "Full size", s: Settings { res: 0, fps: 0, by_size: false, quality: 75., target_mb: 10., hevc: false, audio: 160 } },
    Preset { label: "Fit 10 MB", hint: "Discord, chats", s: Settings { res: 720, fps: 30, by_size: true, quality: 50., target_mb: 10., hevc: false, audio: 96 } },
];

fn quality_label(q: f64) -> &'static str {
    if q < 20. { "Smallest" } else if q < 40. { "Small" } else if q < 62. { "Balanced" } else if q < 82. { "High" } else { "Best" }
}

/// What the encoder is asked for, and the size that should come out (`plan()` in MediaEditor.tsx).
pub struct Plan {
    /// The picture upright, as shown (`dw` × `dh`), and as it comes out (`ow` × `oh`).
    pub dw: u32,
    pub dh: u32,
    pub ow: u32,
    pub oh: u32,
    /// As stored (a turned video stays on its side, with its rotation flag).
    pub sw: u32,
    pub sh: u32,
    pub fps: u32,
    pub vb: u32,
    pub audio: u32,
    pub dur_ms: f64,
    pub bytes: f64,
    /// "By file size" can't get that small: the bitrate hit its floor.
    pub too_small: bool,
}

fn even(x: f64) -> u32 {
    ((x / 2.).round() * 2.).max(2.) as u32
}

pub fn plan(info: &VideoInfo, s: &Settings, keep_audio: bool, dur_ms: f64) -> Plan {
    let turned = info.rotation == 90 || info.rotation == 270;
    let (dw, dh) = if turned { (info.height, info.width) } else { (info.width, info.height) };
    let short = dw.min(dh) as f64;
    let k = if s.res > 0 && (s.res as f64) < short { s.res as f64 / short } else { 1. };
    let (ow, oh) = (even(dw as f64 * k), even(dh as f64 * k));
    let fps = if s.fps > 0 && info.fps > 0. && (s.fps as f64) < info.fps - 0.5 { s.fps } else { 0 };
    let dur_ms = dur_ms.max(1.);
    // No more audio than the original had, in a step Windows' AAC encoder takes (96–192 kbps).
    let src_audio = if info.audio_bitrate > 0 {
        [96u32, 128, 160, 192].into_iter().filter(|k| (*k as f64) * 1000. <= info.audio_bitrate as f64 * 1.05).max().unwrap_or(96)
    } else {
        s.audio
    };
    let audio = if info.has_audio && keep_audio { s.audio.min(src_audio) * 1000 } else { 0 };
    let rate = if fps > 0 { fps as f64 } else if info.fps > 0. { info.fps } else { 30. };
    let mut too_small = false;
    let mut vb = if s.by_size {
        // Room for the container and the encoder overshooting a little.
        let bits = s.target_mb * 1_048_576. * 8. * 0.93 - 64_000. * 8.;
        let v = bits / (dur_ms / 1000.) - audio as f64;
        if v < 200_000. {
            too_small = true;
        }
        v.max(200_000.)
    } else {
        0.01 * 12f64.powf(s.quality / 100.) * ow as f64 * oh as f64 * rate * if s.hevc { 0.62 } else { 1. }
    };
    // Never more than the original had; by quality, less, so each step still saves.
    let src_video = if info.video_bitrate > 0 {
        info.video_bitrate as f64
    } else if info.duration_ms > 0 {
        info.size as f64 * 8. / (info.duration_ms as f64 / 1000.)
            - if info.has_audio { if info.audio_bitrate > 0 { info.audio_bitrate as f64 } else { 128_000. } } else { 0. }
    } else {
        0.
    };
    let keep = if s.by_size { 1. } else { 0.42 + s.quality / 200. };
    if src_video > 0. {
        vb = vb.min(src_video * keep * if s.hevc { 0.8 } else { 1. } * ((ow as f64 * oh as f64) / (dw as f64 * dh as f64) * 1.15).min(1.));
    }
    let vb = vb.max(100_000.).round() as u32;
    let bytes = (vb as f64 + audio as f64) * dur_ms / 8000. * 1.01 + 24_000.;
    Plan { dw, dh, ow, oh, sw: if turned { oh } else { ow }, sh: if turned { ow } else { oh }, fps, vb, audio, dur_ms, bytes, too_small }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Split,
    Screen,
    TrimStart,
    TrimEnd,
    Playhead,
    Quality,
}

/// What the stage shows once there's a sample: the original, both with a divider, or the sample.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Orig,
    Split,
    Sample,
}

/// An open drop-down of the settings column.
#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Res,
    Audio,
}

/// What a sample was made with: the same means it still shows these settings.
type Key = (u32, u32, u32, u32, u32, bool);

/// How long the settings must stay put before an automatic preview starts (a slider being
/// dragged doesn't start one per step).
const AUTO_SAMPLE_AFTER_MS: u64 = 700;

/// Full screen: how long the controls stay after the pointer last moved, while playing.
const FULL_CONTROLS_FOR: std::time::Duration = std::time::Duration::from_millis(2500);

struct Sampled {
    player: Player,
    sample: VideoSample,
    /// Where in the original it starts, seconds.
    at: f64,
    key: Key,
    /// Which video of the batch.
    source: usize,
}

enum Check {
    Running,
    Done { score: f64, at: f64, level: Level, text: &'static str },
    Failed,
}

/// The preview sample (made by itself: `schedule_auto_sample`).
const SAMPLE_S: f64 = 4.;
const RESOLUTIONS: [u32; 7] = [2160, 1440, 1080, 720, 540, 480, 360];
const SAMPLE_FRAMES: usize = 8;

pub struct Editor {
    paths: Vec<String>,
    current: usize,
    infos: Vec<Option<Result<VideoInfo, String>>>,
    /// Per video, seconds.
    trims: Vec<(f64, f64)>,
    focus: FocusHandle,
    original: Option<Result<Player, String>>,
    sampled: Option<Sampled>,
    /// GPUI's device generation the players were opened for (see `reopen_after_reset`).
    device_gen: u64,
    sampling: bool,
    sample_gen: u64,
    sample_len: f64,
    /// The sample being encoded, 0–1 (`media-sample-progress`).
    progress: f32,
    check: Option<Check>,
    error: Option<String>,
    settings: Settings,
    keep_audio: bool,
    /// Seconds into the original.
    playhead: f64,
    /// Asked to play (a sample loops; the original loops inside the trim).
    want_play: bool,
    view: View,
    /// The view picked: outside the sample the original shows, and this comes back inside it.
    wanted: View,
    /// The divider, 0–1 across the screen.
    split: f32,
    /// None fits the screen; else pixels on screen per pixel of the picture.
    zoom: Option<f32>,
    pan: (f32, f32),
    drag: Drag,
    /// Where a press on the screen started, the pan then, and whether it moved (a pan, not a click).
    press: Option<(Point<Pixels>, (f32, f32), bool)>,
    screen_bounds: Rc<Cell<Bounds<Pixels>>>,
    timeline_bounds: Rc<Cell<Bounds<Pixels>>>,
    slider_bounds: Rc<Cell<Bounds<Pixels>>>,
    name: Entity<TextInput>,
    target: Entity<TextInput>,
    _subs: Vec<Subscription>,
    polling: bool,
    /// Images that went out of use, dropped from GPUI's cache at the next frame.
    dropped: Vec<Arc<RenderImage>>,
    muted: bool,
    full: bool,
    /// Full screen: when the pointer last moved (its controls fade out a moment after, while
    /// playing), and which fade is due.
    full_moved: std::time::Instant,
    full_gen: u64,
    pick: Option<Pick>,
    /// The timeline's frames, for which video.
    strip: Option<(usize, Option<Arc<Image>>)>,
    strip_busy: bool,
    follow_gen: u64,
    /// The settings (and video) the last automatic preview was asked for, and that request (a
    /// newer change cancels it): the preview follows the settings by itself (user request,
    /// 2026-11-10).
    auto_key: Option<(usize, Key)>,
    auto_gen: u64,
    bench: Option<PathBuf>,
    /// Benchmarks: compress as soon as the videos are read.
    auto_compress: bool,
}

impl EventEmitter<EditorEvent> for Editor {}

fn stem(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

impl Editor {
    pub fn new(paths: Vec<String>, bench: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let first = paths.first().cloned().unwrap_or_default();
        let single = paths.len() == 1;
        // Test switches: a preset, HEVC, "by size" with a target.
        let mut settings = PRESETS[std::env::var("FERRET_GPUI_EDITOR_PRESET").ok().and_then(|p| p.parse().ok()).filter(|p: &usize| *p < PRESETS.len()).unwrap_or(1)].s;
        if std::env::var_os("FERRET_GPUI_EDITOR_HEVC").is_some() {
            settings.hevc = true;
        }
        if let Some(mb) = std::env::var("FERRET_GPUI_EDITOR_BYSIZE").ok().and_then(|m| m.parse().ok()) {
            settings.by_size = true;
            settings.target_mb = mb;
        }
        let name = cx.new(|cx| {
            let mut i = TextInput::new("", cx).sized(13., 20.).named(if single { "Save as" } else { "Name ending" });
            i.set_text(&if single { format!("{} (compressed)", stem(&first)) } else { " (compressed)".into() }, cx);
            i
        });
        let target_mb = settings.target_mb;
        let target = cx.new(|cx| {
            let mut i = TextInput::new("", cx).sized(13., 20.).named("Size in MB");
            i.set_text(&format!("{target_mb}"), cx);
            i
        });
        let subs = vec![
            cx.subscribe(&name, |_, _, _: &Changed, cx| cx.notify()),
            cx.subscribe(&target, |e, input, _: &Changed, cx| {
                if let Ok(v) = input.read(cx).text().trim().parse::<f64>() {
                    if v > 0. {
                        e.settings.target_mb = v;
                    }
                }
                cx.notify();
            }),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let n = paths.len();
        let this = Self {
            device_gen: gpui_windows::ferret::device_generation(),
            original: Some(Player::open(&first, true)),
            paths: paths.clone(),
            current: 0,
            infos: (0..n).map(|_| None).collect(),
            trims: vec![(0., 0.); n],
            focus,
            sampled: None,
            sampling: false,
            sample_gen: 0,
            sample_len: SAMPLE_S,
            progress: 0.,
            check: None,
            error: None,
            settings,
            keep_audio: true,
            playhead: 0.,
            want_play: false,
            view: View::Orig,
            wanted: View::Orig,
            split: 0.5,
            zoom: None,
            pan: (0., 0.),
            drag: Drag::None,
            press: None,
            screen_bounds: Rc::default(),
            timeline_bounds: Rc::default(),
            slider_bounds: Rc::default(),
            name,
            target,
            _subs: subs,
            polling: false,
            dropped: Vec::new(),
            muted: true,
            full: false,
            full_moved: std::time::Instant::now(),
            full_gen: 0,
            pick: None,
            strip: None,
            strip_busy: false,
            follow_gen: 0,
            auto_key: None,
            auto_gen: 0,
            bench,
            auto_compress: std::env::var_os("FERRET_GPUI_EDITOR_COMPRESS").is_some(),
        };
        for (i, path) in paths.into_iter().enumerate() {
            let info = cx.background_executor().spawn(async move { ferret_core::rt::block_on(media::video_info(path)) });
            cx.spawn(async move |this, cx| {
                let info = info.await;
                this.update(cx, |e, cx| {
                    if let Ok(v) = &info {
                        e.trims[i] = (0., v.duration_ms as f64 / 1000.);
                    }
                    e.infos[i] = Some(info);
                    if i == e.current {
                        e.seek(0.);
                    }
                    if e.auto_compress && e.infos.iter().all(Option::is_some) {
                        e.auto_compress = false;
                        e.submit(cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        this
    }

    fn single(&self) -> bool {
        self.paths.len() == 1
    }

    fn info(&self) -> Option<&VideoInfo> {
        self.info_of(self.current)
    }

    fn info_of(&self, i: usize) -> Option<&VideoInfo> {
        self.infos.get(i).and_then(|o| o.as_ref()).and_then(|r| r.as_ref().ok())
    }

    fn duration(&self) -> f64 {
        self.info().map_or(0., |i| i.duration_ms as f64 / 1000.)
    }

    fn trim(&self) -> (f64, f64) {
        self.trims[self.current]
    }

    fn trimmed(&self) -> bool {
        let (a, b) = self.trim();
        self.single() && self.info().is_some() && (a > 0.001 || b < self.duration() - 0.001)
    }

    /// The trim of video `i` if it has one (only a single video is trimmed).
    fn trim_of(&self, i: usize) -> Option<(f64, f64)> {
        (self.single() && i == self.current && self.trimmed()).then(|| self.trims[i])
    }

    fn plan_with(&self, i: usize, s: &Settings, trim: bool) -> Option<Plan> {
        let info = self.info_of(i)?;
        let dur = match self.trim_of(i).filter(|_| trim) {
            Some((a, b)) => (b - a) * 1000.,
            None => info.duration_ms as f64,
        };
        Some(plan(info, s, self.keep_audio, dur))
    }

    fn plan_of(&self, i: usize) -> Option<Plan> {
        self.plan_with(i, &self.settings, true)
    }

    fn preset(&self) -> Option<usize> {
        PRESETS.iter().position(|p| p.s == self.settings)
    }

    /// A preset card's second line: the size it gives and the size it comes out at.
    fn preset_hint(&self, p: &Preset) -> String {
        let known: Vec<usize> = (0..self.paths.len()).filter(|i| self.info_of(*i).is_some()).collect();
        let Some(&first) = known.first() else { return p.hint.into() };
        let c = if self.info().is_some() { self.current } else { first };
        let Some(pl) = self.plan_with(c, &p.s, true) else { return p.hint.into() };
        let res = format!("{}p", pl.ow.min(pl.oh));
        let before: u64 = known.iter().filter_map(|i| self.info_of(*i)).map(|i| i.size).sum();
        if p.s.by_size {
            let fits = known.iter().filter_map(|i| self.info_of(*i)).all(|i| i.size as f64 <= p.s.target_mb * 1_048_576.);
            return if fits { format!("{res} · {} under {} MB", if known.len() > 1 { "all" } else { "already" }, p.s.target_mb) } else { format!("{res} · {}", p.hint) };
        }
        let after: f64 = known.iter().filter_map(|i| self.plan_with(*i, &p.s, true)).map(|p| p.bytes).sum();
        if after >= before as f64 * 0.9 { format!("{res} · about the same size") } else { format!("{res} · ≈ {}", kinds::format_size(after as u64)) }
    }

    fn dest_for(&self, i: usize, cx: &Context<Self>) -> String {
        let p = &self.paths[i];
        let dir = Path::new(p).parent().map(Path::to_path_buf).unwrap_or_default();
        let typed = self.name.read(cx).text().to_string();
        let name = if self.single() { typed.trim().to_string() } else { format!("{}{typed}", stem(p)) };
        dir.join(format!("{name}.mp4")).to_string_lossy().into_owned()
    }

    fn job_spec(&self, i: usize, dest: String) -> Option<VideoJob> {
        let p = self.plan_of(i)?;
        let t = self.trim_of(i);
        Some(VideoJob {
            source: self.paths[i].clone(),
            dest,
            width: p.sw,
            height: p.sh,
            fps: p.fps,
            video_bitrate: p.vb,
            audio_bitrate: p.audio,
            hevc: self.settings.hevc,
            trim_start_ms: t.map(|(a, _)| (a * 1000.).round() as u64),
            trim_end_ms: t.map(|(_, b)| (b * 1000.).round() as u64),
        })
    }

    fn key_of(&self, i: usize) -> Option<Key> {
        let p = self.plan_with(i, &self.settings, false)?;
        Some((p.sw, p.sh, p.fps, p.vb, p.audio, self.settings.hevc))
    }

    fn sample_here(&self) -> bool {
        self.sampled.as_ref().is_some_and(|s| s.source == self.current)
    }

    /// The sample still shows these settings.
    fn sample_fresh(&self) -> bool {
        self.sampled.as_ref().is_some_and(|s| s.source == self.current && Some(s.key) == self.key_of(self.current))
    }

    /// The sample is on screen (alone, or beside the original).
    fn in_sample(&self) -> bool {
        self.sample_here() && self.view != View::Orig
    }

    fn showing_sample(&self) -> bool {
        self.sample_here() && self.view == View::Sample
    }

    fn sample_end(&self) -> Option<(f64, f64)> {
        self.sampled.as_ref().filter(|s| s.source == self.current).map(|s| (s.at, s.at + s.sample.duration_ms as f64 / 1000.))
    }

    fn unplayable(&self) -> bool {
        matches!(&self.original, Some(Err(_))) || matches!(&self.original, Some(Ok(p)) if p.error().is_some())
    }

    // ───────────────────────────── playing ─────────────────────────────

    /// Sound from the original, or from the sample when it shows alone; none when muted.
    fn apply_mute(&mut self) {
        let alone = self.showing_sample();
        let muted = self.muted;
        if let Some(Ok(p)) = self.original.as_mut() {
            p.set_muted(muted || alone);
        }
        if let Some(s) = self.sampled.as_mut() {
            s.player.set_muted(muted || !alone);
        }
    }

    fn playing(&self) -> bool {
        if self.in_sample() {
            self.sampled.as_ref().is_some_and(|s| s.player.playing())
        } else {
            matches!(&self.original, Some(Ok(p)) if p.playing())
        }
    }

    fn pause_all(&mut self) {
        if let Some(Ok(p)) = self.original.as_mut() {
            p.pause();
        }
        if let Some(s) = self.sampled.as_mut() {
            s.player.pause();
        }
        self.want_play = false;
    }

    /// Seconds into the original. Inside the sample the comparison shows; outside it, only the
    /// original exists.
    fn seek(&mut self, secs: f64) {
        let secs = secs.clamp(0., self.duration().max(0.));
        let cur = self.current;
        let mut view = self.view;
        let wanted = self.wanted;
        if let Some(s) = self.sampled.as_mut().filter(|s| s.source == cur) {
            let len = s.sample.duration_ms as f64 / 1000.;
            if secs >= s.at && secs <= s.at + len {
                if view == View::Orig && wanted != View::Orig {
                    view = wanted;
                }
                s.player.seek((secs - s.at).min(len - 0.05).max(0.));
            } else if view != View::Orig {
                s.player.pause();
                view = View::Orig;
            }
        }
        self.view = view;
        if let Some(Ok(p)) = self.original.as_mut() {
            p.seek(secs);
        }
        self.playhead = secs;
        self.apply_mute();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.unplayable() && !self.in_sample() {
            return;
        }
        if self.playing() {
            self.pause_all();
        } else {
            if self.in_sample() {
                let split = self.view == View::Split;
                if let Some(s) = self.sampled.as_mut() {
                    s.player.play();
                }
                if split {
                    if let Some(Ok(p)) = self.original.as_mut() {
                        p.play();
                    }
                }
            } else {
                let (a, b) = self.trim();
                if self.single() && (self.playhead < a || self.playhead >= b - 0.05) {
                    self.seek(a);
                }
                if let Some(Ok(p)) = self.original.as_mut() {
                    p.play();
                }
            }
            self.want_play = true;
        }
        cx.notify();
    }

    fn switch_view(&mut self, to: View, cx: &mut Context<Self>) {
        if !self.sample_here() || to == self.view {
            return;
        }
        let was = self.playing();
        let t = self.playhead;
        self.pause_all();
        self.view = to;
        self.wanted = to;
        if let Some(s) = self.sampled.as_mut() {
            let len = s.sample.duration_ms as f64 / 1000.;
            if to != View::Orig {
                s.player.seek((t - s.at).clamp(0., (len - 0.05).max(0.)));
            }
        }
        if let Some(Ok(p)) = self.original.as_mut() {
            p.seek(t);
        }
        self.apply_mute();
        if was {
            self.toggle_play(cx);
        }
        cx.notify();
    }

    fn set_start(&mut self) {
        let i = self.current;
        self.trims[i].0 = self.playhead.min(self.trims[i].1 - 0.5).max(0.);
    }

    fn set_end(&mut self) {
        let i = self.current;
        self.trims[i].1 = self.playhead.max(self.trims[i].0 + 0.5).min(self.duration());
    }

    fn toggle_full(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.full = !self.full;
        if window.is_fullscreen() != self.full {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    /// The pointer moved in full screen: the controls show, and fade a moment after it rests.
    fn full_pointer_moved(&mut self, cx: &mut Context<Self>) {
        let was_hidden = self.playing() && self.full_moved.elapsed() >= FULL_CONTROLS_FOR;
        self.full_moved = std::time::Instant::now();
        self.full_gen += 1;
        let g = self.full_gen;
        if was_hidden {
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FULL_CONTROLS_FOR + std::time::Duration::from_millis(50)).await;
            this.update(cx, |e, cx| {
                if e.full_gen == g && e.full {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The wheel over the timeline: through the video a second a notch (Shift: 5 s), forward
    /// when scrolling down or right. A comparison follows once it rests, as after the arrow keys.
    fn wheel_scrub(&mut self, ev: &gpui::ScrollEvent, cx: &mut Context<Self>) {
        let d = ev.delta.pixel_delta(px(20.));
        let (dx, dy) = (d.x.as_f32(), d.y.as_f32());
        let amount = if dx.abs() > dy.abs() { -dx } else { -dy };
        if amount.abs() < 0.5 {
            return;
        }
        // A notch is about 20 px; a trackpad sends small steps that add up the same way.
        let step = if ev.modifiers.shift { 5. } else { 1. };
        self.seek(self.playhead + (amount / 20.) as f64 * step);
        self.follow_gen += 1;
        let g = self.follow_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
            this.update(cx, |e, cx| if e.follow_gen == g { e.follow_with_sample(cx) }).ok();
        })
        .detach();
        cx.notify();
    }

    /// Another video of the batch.
    fn show(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.paths.len() || i == self.current {
            return;
        }
        self.pause_all();
        if let Some(Ok(p)) = self.original.take() {
            self.dropped.extend(p.image());
        }
        if let Some(s) = self.sampled.take() {
            self.dropped.extend(s.player.image());
        }
        self.current = i;
        self.original = Some(Player::open(&self.paths[i], true));
        self.check = None;
        self.view = View::Orig;
        self.wanted = View::Orig;
        self.zoom = None;
        self.pan = (0., 0.);
        self.seek(0.);
        cx.notify();
    }

    // ───────────────────────────── the sample ─────────────────────────────

    /// A preview: encodes a few seconds from the playhead with these settings, to
    /// compare, check the quality and measure the size (a newer request replaces it).
    fn make_sample(&mut self, cx: &mut Context<Self>) {
        self.request_sample(false, cx);
    }

    /// A sample; `auto` (it followed the settings): it doesn't start playing, and a view the user
    /// picked stays.
    fn request_sample(&mut self, auto: bool, cx: &mut Context<Self>) {
        let i = self.current;
        let (Some(job), Some(key)) = (self.job_spec(i, String::new()), self.key_of(i)) else { return };
        let (lo, hi) = if self.single() { self.trim() } else { (0., self.duration()) };
        let at = self.playhead.clamp(lo, (hi - SAMPLE_S).max(lo));
        // A trim shorter than a sample gets a sample of just the trim.
        let len = (hi - at).min(SAMPLE_S).max(0.5);
        self.sample_len = len;
        self.sample_gen += 1;
        let generation = self.sample_gen;
        self.sampling = true;
        self.progress = 0.;
        self.error = None;
        let job = VideoJob { trim_start_ms: None, trim_end_ms: None, ..job };
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(media::video_sample(job, (at * 1000.) as u64, (len * 1000.) as u64)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |e, cx| {
                if e.sample_gen != generation {
                    return;
                }
                e.sampling = false;
                if e.current != i {
                    return;
                }
                match result {
                    Ok(Some(sample)) => match Player::open(&sample.path, true) {
                        Ok(player) => {
                            e.pause_all();
                            if let Some(old) = e.sampled.take() {
                                e.dropped.extend(old.player.image());
                            }
                            if let Some(out) = &e.bench {
                                bench::write_to(out, "editor_sample_bytes", sample.bytes);
                                bench::write_to(out, "editor_sample_ms", sample.duration_ms);
                            }
                            e.start_check(&sample, at, generation, cx);
                            e.sampled = Some(Sampled { player, sample, at, key, source: i });
                            // Compare (an automatic one keeps "Compressed" when that was picked).
                            if !(auto && e.wanted == View::Sample) {
                                e.view = View::Split;
                                e.wanted = View::Split;
                            } else {
                                e.view = View::Sample;
                            }
                            // Both start together at the sample's first frame; asked for, they play.
                            e.seek(at);
                            if !auto {
                                e.toggle_play(cx);
                            }
                        }
                        Err(err) => e.error = Some(format!("Couldn't show the preview: {err}")),
                    },
                    Ok(None) => {}
                    Err(err) => e.error = Some(format!("Couldn't make a preview: {err}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The preview follows the settings by itself: once the compressor shows a video, and a
    /// moment after its settings last changed (quality, preset, size, frame rate, format), a new
    /// sample is made unless the one shown already has these settings.
    fn schedule_auto_sample(&mut self, cx: &mut Context<Self>) {
        let want = self.key_of(self.current).map(|k| (self.current, k));
        if want.is_none() || want == self.auto_key {
            return;
        }
        self.auto_key = want;
        if self.sample_fresh() {
            return;
        }
        self.auto_gen += 1;
        let g = self.auto_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(AUTO_SAMPLE_AFTER_MS)).await;
            this.update(cx, |e, cx| {
                if e.auto_gen == g && !e.sample_fresh() {
                    e.request_sample(true, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The quality check of a new sample, off the UI thread.
    fn start_check(&mut self, sample: &VideoSample, at: f64, generation: u64, cx: &mut Context<Self>) {
        let Some(info) = self.info() else { return };
        let (original, rotation, fps) = (self.paths[self.current].clone(), info.rotation, info.fps);
        let plan_fps = self.plan_of(self.current).map(|p| p.fps).unwrap_or(0);
        let fps = if plan_fps > 0 { plan_fps as f64 } else { fps };
        let (sample_path, dur) = (sample.path.clone(), sample.duration_ms as f64 / 1000.);
        self.check = Some(Check::Running);
        let started = std::time::Instant::now();
        let task = cx.background_executor().spawn(async move { quality::check(&original, rotation, &sample_path, at, dur, fps) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |e, cx| {
                if e.sample_gen != generation {
                    return;
                }
                e.check = Some(match result {
                    Ok(c) => {
                        let (level, text) = quality::verdict(c.score);
                        if let Some(out) = &e.bench {
                            bench::write_to(out, "editor_check_score", format!("{:.4}", c.score));
                            bench::write_to(out, "editor_check_ms", started.elapsed().as_millis());
                        }
                        Check::Done { score: c.score, at: c.at, level, text }
                    }
                    Err(_) => Check::Failed,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Pauses on the sample's weakest moment, side by side, to look at it closely.
    fn show_worst(&mut self, cx: &mut Context<Self>) {
        if let Some(Check::Done { at, .. }) = self.check {
            self.pause_all();
            if self.view == View::Orig {
                self.switch_view(View::Split, cx);
            }
            self.seek(at);
            cx.notify();
        }
    }

    /// Let go of the timeline outside the sample while comparing: a new sample there.
    fn follow_with_sample(&mut self, cx: &mut Context<Self>) {
        if !self.sample_here() || self.wanted == View::Orig {
            return;
        }
        if let Some((a, b)) = self.sample_end() {
            if self.playhead >= a && self.playhead <= b {
                return;
            }
        }
        self.make_sample(cx);
    }

    pub fn sample_progress(&mut self, f: f64, cx: &mut Context<Self>) {
        self.progress = f.clamp(0., 1.) as f32;
        cx.notify();
    }

    // ───────────────────────────── totals, compress ─────────────────────────────

    /// Before, after (measured from a fresh sample for the video shown, else worked out), and
    /// whether it was measured.
    fn totals(&self) -> (u64, f64, bool) {
        let (mut before, mut after, mut measured) = (0u64, 0f64, false);
        for i in 0..self.paths.len() {
            let Some(info) = self.info_of(i) else { continue };
            before += info.size;
            let Some(p) = self.plan_of(i) else { continue };
            match self.sampled.as_ref().filter(|s| s.source == i && i == self.current && self.sample_fresh()) {
                Some(s) if s.sample.duration_ms > 0 => {
                    after += s.sample.bytes as f64 / s.sample.duration_ms as f64 * p.dur_ms + 16_000.;
                    measured = true;
                }
                _ => after += p.bytes,
            }
        }
        (before, after, measured)
    }

    fn name_error(&self, cx: &Context<Self>) -> Option<&'static str> {
        mx::name_error(self.name.read(cx).text(), self.single())
    }

    fn can_submit(&self, cx: &Context<Self>) -> bool {
        (0..self.paths.len()).any(|i| self.info_of(i).is_some()) && self.name_error(cx).is_none()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let jobs: Vec<VideoJob> = (0..self.paths.len()).filter(|i| self.info_of(*i).is_some()).filter_map(|i| self.job_spec(i, self.dest_for(i, cx))).collect();
        let (before, after, _) = self.totals();
        self.pause_all();
        cx.emit(EditorEvent::Compress { jobs, before, estimate: after as u64 });
    }

    // ───────────────────────────── frames ─────────────────────────────

    /// The picture's own size, upright.
    fn nat(&self) -> (f32, f32) {
        match self.plan_of(self.current) {
            Some(p) => (p.dw as f32, p.dh as f32),
            None => (16., 9.),
        }
    }

    fn box_size(&self) -> (f32, f32) {
        let b = self.screen_bounds.get();
        (b.size.width.as_f32().max(1.), b.size.height.as_f32().max(1.))
    }

    fn fit(&self) -> f32 {
        let ((bw, bh), (nw, nh)) = (self.box_size(), self.nat());
        (bw / nw).min(bh / nh)
    }

    fn scale(&self) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit())
    }

    /// The whole picture on the screen: left, top, width, height.
    fn frame(&self) -> (f32, f32, f32, f32) {
        let ((bw, bh), (nw, nh), k) = (self.box_size(), self.nat(), self.scale());
        let (w, h) = (nw * k, nh * k);
        (bw / 2. - w / 2. + self.pan.0, bh / 2. - h / 2. + self.pan.1, w, h)
    }

    /// The part of the picture that's on the screen (x, y, w, h) and which part of the frame
    /// that is (0–1), for the GPU to scale just that.
    fn visible(&self) -> (f32, f32, f32, f32, Option<[f32; 4]>) {
        let (bw, bh) = self.box_size();
        let (l, t, w, h) = self.frame();
        let (x0, y0, x1, y1) = (l.max(0.), t.max(0.), (l + w).min(bw), (t + h).min(bh));
        if self.zoom.is_none() || (x0 <= l + 0.5 && y0 <= t + 0.5 && x1 >= l + w - 0.5 && y1 >= t + h - 0.5) {
            return (l, t, w, h, None);
        }
        (x0, y0, (x1 - x0).max(1.), (y1 - y0).max(1.), Some([(x0 - l) / w, (y0 - t) / h, (x1 - l) / w, (y1 - t) / h]))
    }

    fn clamp_pan(&self, p: (f32, f32), k: f32) -> (f32, f32) {
        let ((bw, bh), (nw, nh)) = (self.box_size(), self.nat());
        let (mx, my) = (((nw * k - bw) / 2.).max(0.), ((nh * k - bh) / 2.).max(0.));
        (p.0.clamp(-mx, mx), p.1.clamp(-my, my))
    }

    /// Zooms keeping the point (cx, cy) from the screen's middle in place; None fits the screen.
    fn zoom_to(&mut self, next: Option<f32>, c: (f32, f32)) {
        let (old, fit) = (self.scale(), self.fit());
        match next.map(|n| n.clamp(fit.min(1.), 8.)) {
            Some(n) if (n - fit).abs() > 1e-3 => {
                let p = self.pan;
                self.zoom = Some(n);
                self.pan = self.clamp_pan((c.0 - (c.0 - p.0) * n / old, c.1 - (c.1 - p.1) * n / old), n);
            }
            _ => {
                self.zoom = None;
                self.pan = (0., 0.);
            }
        }
    }

    /// Both players' frames, on the GPU. True when there's something new to draw.
    /// After a GPU reset the players decode on the old device, whose frames GPUI's new atlas can't
    /// take (the stage went blank): both open again on the new one, where they were, playing if
    /// they were. As the preview's `VideoView::reopen_if_device_changed`.
    fn reopen_after_reset(&mut self) -> bool {
        let now = gpui_windows::ferret::device_generation();
        if now == self.device_gen {
            return false;
        }
        self.device_gen = now;
        let again = |path: &str, at: f64, playing: bool| {
            let mut p = Player::open(path, true);
            if let Ok(p) = p.as_mut() {
                if at > 0. {
                    p.seek(at);
                }
                if playing {
                    p.play();
                }
            }
            p
        };
        let (at, playing) = match self.original.as_ref() {
            Some(Ok(p)) => (p.position(), p.playing()),
            _ => (0., false),
        };
        if let Some(Ok(p)) = self.original.take() {
            self.dropped.extend(p.image());
        }
        self.original = Some(again(&self.paths[self.current], at, playing));
        if let Some(s) = self.sampled.take() {
            let (at, playing) = (s.player.position(), s.player.playing());
            self.dropped.extend(s.player.image());
            if let Ok(player) = again(&s.sample.path, at, playing) {
                self.sampled = Some(Sampled { player, ..s });
            }
        }
        self.apply_mute();
        true
    }

    fn update_players(&mut self, window: &mut Window) -> bool {
        let reopened = self.reopen_after_reset();
        for image in self.dropped.drain(..) {
            let _ = window.drop_image(image);
        }
        let scale = window.scale_factor();
        let (_, _, w, h, src) = self.visible();
        let (dw, dh) = ((w * scale).round().max(2.) as u32, (h * scale).round().max(2.) as u32);
        let mut changed = false;
        let mut pull = |p: &mut Player, window: &mut Window| {
            let before = (p.frames, p.image().is_some());
            if let Some(old) = p.update_part(dw, dh, src) {
                let _ = window.drop_image(old);
                changed = true;
            }
            changed |= before != (p.frames, p.image().is_some());
        };
        if let Some(Ok(p)) = self.original.as_mut() {
            pull(p, window);
        }
        if let Some(s) = self.sampled.as_mut() {
            pull(&mut s.player, window);
        }
        let (single, trim, in_sample, split, want) = (self.single(), self.trim(), self.in_sample(), self.view == View::Split, self.want_play);
        if in_sample {
            if let Some(s) = self.sampled.as_mut() {
                let len = s.sample.duration_ms as f64 / 1000.;
                let pos = s.player.position();
                self.playhead = s.at + pos;
                // The sample loops.
                if want && !s.player.playing() && pos >= len - 0.1 {
                    s.player.seek(0.);
                    s.player.play();
                }
                // The original follows it, beside it.
                if split {
                    if let Some(Ok(o)) = self.original.as_mut() {
                        let target = s.at + s.player.position();
                        if (o.position() - target).abs() > 0.12 {
                            o.seek(target);
                        }
                        if s.player.playing() != o.playing() {
                            if s.player.playing() { o.play() } else { o.pause() }
                        }
                    }
                }
                changed = true;
            }
        } else if let Some(Ok(o)) = self.original.as_mut() {
            if o.playing() {
                self.playhead = o.position();
                // Playback stays inside the trimmed part.
                if single && self.playhead >= trim.1 - 0.03 {
                    o.seek(trim.0);
                    self.playhead = trim.0;
                }
                changed = true;
            }
        }
        changed || reopened
    }

    fn schedule_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wants = matches!(&self.original, Some(Ok(p)) if p.needs_frames())
            || self.sampled.as_ref().is_some_and(|s| s.player.needs_frames())
            || !self.dropped.is_empty()
            || self.want_play;
        if self.polling || !wants {
            return;
        }
        self.polling = true;
        let this = cx.entity();
        window.on_next_frame(move |window, cx| {
            this.update(cx, |e, cx| {
                e.polling = false;
                if e.update_players(window) {
                    cx.notify();
                }
                e.schedule_poll(window, cx);
            })
        });
    }

    /// The timeline's frames, once it knows its width.
    fn ensure_strip(&mut self, window: &Window, cx: &mut Context<Self>) {
        let i = self.current;
        if self.strip_busy || self.strip.as_ref().is_some_and(|(j, _)| *j == i) {
            return;
        }
        let b = self.timeline_bounds.get();
        let Some(info) = self.info() else { return };
        if b.size.width.as_f32() < 20. {
            return;
        }
        let k = window.scale_factor();
        let (w, h) = ((b.size.width.as_f32() * k) as usize, (b.size.height.as_f32() * k) as usize);
        let (path, rotation, dur) = (self.paths[i].clone(), info.rotation, info.duration_ms as f64 / 1000.);
        self.strip_busy = true;
        let task = cx.background_executor().spawn(async move { quality::filmstrip(&path, rotation, dur, w, h) });
        cx.spawn(async move |this, cx| {
            let png = task.await;
            this.update(cx, |e, cx| {
                e.strip_busy = false;
                // No strip for what the source reader can't decode (the timeline stays plain).
                e.strip = Some((i, png.ok().map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)))));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── input ─────────────────────────────

    fn in_field(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.name.read(cx).focus_handle(cx).is_focused(window) || self.target.read(cx).focus_handle(cx).is_focused(window)
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = ev.keystroke.key.as_str();
        let m = ev.keystroke.modifiers;
        let field = self.in_field(window, cx);
        match k {
            "escape" => {
                if self.full {
                    self.toggle_full(window, cx);
                } else if self.pick.is_some() {
                    self.pick = None;
                    cx.notify();
                } else {
                    self.close(window, cx);
                }
            }
            "enter" if m.control || field => self.submit(cx),
            _ if field => return,
            "space" => self.toggle_play(cx),
            "i" if self.single() => {
                self.set_start();
                cx.notify();
            }
            "o" if self.single() => {
                self.set_end();
                cx.notify();
            }
            "left" | "right" => {
                let step = if m.shift { 5. } else if m.alt { 1. / 30. } else { 1. };
                self.seek(self.playhead + if k == "left" { -step } else { step });
                // Once the keys stop, a comparison follows as it does after a drag.
                self.follow_gen += 1;
                let g = self.follow_gen;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
                    this.update(cx, |e, cx| if e.follow_gen == g { e.follow_with_sample(cx) }).ok();
                })
                .detach();
                cx.notify();
            }
            "m" => {
                self.muted = !self.muted;
                self.apply_mute();
                cx.notify();
            }
            "f" => self.toggle_full(window, cx),
            "pagedown" if self.paths.len() > 1 => self.show(self.current + 1, cx),
            "pageup" if self.paths.len() > 1 => self.show(self.current.saturating_sub(1), cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pause_all();
        if self.full && window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.emit(EditorEvent::Close);
    }

    fn screen_down(&mut self, at: Point<Pixels>) {
        let b = self.screen_bounds.get();
        let x = (at.x - b.origin.x).as_f32();
        if self.sample_here() && self.view == View::Split && (x - self.split * b.size.width.as_f32()).abs() < 12. {
            self.drag = Drag::Split;
        } else {
            self.drag = Drag::Screen;
            self.press = Some((at, self.pan, false));
        }
    }

    fn timeline_down(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let b = self.timeline_bounds.get();
        let d = self.duration();
        if d <= 0. || b.size.width.as_f32() <= 0. {
            return;
        }
        let x = (at.x - b.origin.x).as_f32();
        let to_x = |s: f64| (s / d) as f32 * b.size.width.as_f32();
        let (a, e) = self.trim();
        let single = self.single();
        self.drag = if single && (x - to_x(a)).abs() < 9. {
            Drag::TrimStart
        } else if single && (x - to_x(e)).abs() < 9. {
            Drag::TrimEnd
        } else {
            Drag::Playhead
        };
        if let Some(Ok(p)) = self.original.as_mut() {
            p.pause();
        }
        self.want_play = false;
        self.drag_to(at, cx);
    }

    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        match self.drag {
            Drag::None => return,
            Drag::Split => {
                let b = self.screen_bounds.get();
                self.split = ((at.x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
            }
            Drag::Screen => {
                let Some((from, pan, _)) = self.press else { return };
                let (dx, dy) = ((at.x - from.x).as_f32(), (at.y - from.y).as_f32());
                if dx.abs() + dy.abs() > 4. {
                    self.press = Some((from, pan, true));
                }
                if self.zoom.is_some() && self.press.is_some_and(|p| p.2) {
                    self.pan = self.clamp_pan((pan.0 + dx, pan.1 + dy), self.scale());
                }
            }
            Drag::Quality => {
                let b = self.slider_bounds.get();
                let f = ((at.x - b.origin.x).as_f32() / b.size.width.as_f32().max(1.)).clamp(0., 1.) as f64;
                self.settings.quality = (f * 100.).round();
            }
            Drag::TrimStart | Drag::TrimEnd | Drag::Playhead => {
                let b = self.timeline_bounds.get();
                let d = self.duration();
                let s = (((at.x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.) as f64) * d;
                let i = self.current;
                match self.drag {
                    Drag::TrimStart => {
                        self.trims[i].0 = s.min(self.trims[i].1 - 0.5).max(0.);
                        self.seek(self.trims[i].0);
                    }
                    Drag::TrimEnd => {
                        self.trims[i].1 = s.max(self.trims[i].0 + 0.5).min(d);
                        self.seek(self.trims[i].1);
                    }
                    _ => self.seek(s),
                }
            }
        }
        cx.notify();
    }

    fn drag_end(&mut self, cx: &mut Context<Self>) {
        let was = std::mem::replace(&mut self.drag, Drag::None);
        let press = self.press.take();
        match was {
            // A click that didn't move plays or pauses.
            Drag::Screen if !press.is_some_and(|p| p.2) => self.toggle_play(cx),
            Drag::Playhead => self.follow_with_sample(cx),
            _ => {}
        }
        cx.notify();
    }

    // ───────────────────────────── drawing ─────────────────────────────

    /// `.mx-screen`: the picture, the comparison, the zoom bar and what's going on.
    fn screen(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tag_bottom = if self.full { 200. } else { 12. };
        let (vx, vy, vw, vh, _) = self.visible();
        let (bw, _) = self.box_size();
        let orig = match &self.original {
            Some(Ok(p)) if !self.showing_sample() => p.image(),
            _ => None,
        };
        let sample = self.sampled.as_ref().filter(|_| self.in_sample()).and_then(|s| s.player.image());
        let split = self.view == View::Split && self.sample_here();
        let split_x = self.split * bw;
        let bounds = self.screen_bounds.clone();
        let fresh = self.sample_fresh();
        let unplayable = self.unplayable();
        // The views, a note on the sample, and the quality check's verdict (`.mx-ab`).
        let ab = self.sample_here().then(|| {
            let opt = |label: &'static str, v: View| {
                let on = self.view == v;
                div()
                    .id(label)
                    .role(gpui::Role::RadioButton)
                    .aria_label(label)
                    .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
                    .h(px(24.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .when(on, |d| d.bg(t::accent()).text_color(t::on_accent()))
                    .when(!on, |d| d.text_color(t::rgba(0xffffff, 0.7)).hover(|s| s.text_color(t::rgba(0xffffff, 1.))))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |e, _: &ClickEvent, _, cx| e.switch_view(v, cx)))
                    .child(label)
            };
            let note = |text: String, stale: bool| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(9.))
                    .py(px(3.))
                    .rounded_full()
                    .bg(t::rgba(0, 0.55))
                    .text_size(px(11.5))
                    .text_color(if stale { t::rgba(0xfbbf24, 1.) } else { t::rgba(0xffffff, 1.) })
                    .child(text)
            };
            let sm = self.sampled.as_ref().unwrap();
            let len_ms = sm.sample.duration_ms as f64;
            let verdict = match &self.check {
                Some(Check::Done { score, at, level, text }) if fresh => {
                    let (color, ok) = match level {
                        Level::Same | Level::Good => (t::rgba(0x6ee7a8, 1.), true),
                        Level::Ok => (t::rgba(0xfbbf24, 1.), false),
                        Level::Poor => (t::rgba(0xf87171, 1.), false),
                    };
                    Some(
                        div()
                            .id("mx-quality")
                            .role(gpui::Role::Button)
                            .aria_label(*text)
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .h(px(22.))
                            .px(px(10.))
                            .rounded_full()
                            .bg(t::rgba(0, 0.6))
                            .hover(|s| s.bg(t::rgba(0, 0.78)))
                            .text_size(px(11.5))
                            .text_color(color)
                            .tooltip(ui::tip(format!(
                                "Compared frame by frame with the original ({SAMPLE_FRAMES} frames). Its weakest moment, at {}, scores {score:.3} (1 is identical): click to pause there side by side.",
                                mx::clock(at * 1000., true)
                            )))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|e, _: &ClickEvent, _, cx| e.show_worst(cx)))
                            .child(icon(if ok { "circle-check" } else { "triangle-alert" }, 12., color))
                            .child(*text)
                            .into_any_element(),
                    )
                }
                _ => None,
            };
            div()
                .absolute()
                .top(px(10.))
                .left(px(10.))
                .max_w(px((bw - 200.).max(160.)))
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .child(div().flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(t::rgba(0, 0.6)).child(opt("Original", View::Orig)).child(opt("Compare", View::Split)).child(opt("Compressed", View::Sample)))
                .when(self.in_sample(), |d| {
                    d.child(if fresh {
                        note(format!("{} sample from {}", mx::seconds_label(len_ms), mx::clock(sm.at * 1000., false)), false)
                    } else {
                        note("Settings changed since this sample".into(), true)
                    })
                })
                .when(self.view == View::Orig && self.wanted != View::Orig, |d| {
                    d.child(note(format!("Outside the {} sample: the original, until one is made here", mx::seconds_label(len_ms)), false))
                })
                .when(matches!(self.check, Some(Check::Running)) && fresh, |d| {
                    d.child(div().flex().items_center().gap(px(5.)).px(px(9.)).py(px(3.)).rounded_full().bg(t::rgba(0, 0.55)).text_size(px(11.5)).text_color(t::rgba(0xffffff, 1.)).child(ui::spinner(11., t::rgba(0xffffff, 1.))).child("Checking quality…"))
                })
                .children(verdict)
        });
        let full = self.full;
        let scale = self.scale();
        let zoom = (!unplayable || self.in_sample()).then(|| {
            mx::zoom_bar()
                .top(px(10.))
                .right(px(10.))
                .child(mx::zoom_btn("vz-fit", "Fit", self.zoom.is_none(), Box::new(cx.listener(|e, _: &ClickEvent, _, cx| {
                    e.zoom_to(None, (0., 0.));
                    cx.notify();
                }))).tooltip(ui::tip("Fit the screen")))
                .child(mx::zoom_btn("vz-100", "100%", self.zoom.is_some_and(|z| (z - 1.).abs() < 1e-3), Box::new(cx.listener(|e, _: &ClickEvent, _, cx| {
                    e.zoom_to(Some(1.), (0., 0.));
                    cx.notify();
                }))).tooltip(ui::tip("Actual pixels, to look for blocks and smearing (double-click; scroll to zoom)")))
                .child(mx::zoom_pct(scale))
                .child(mx::zoom_sep())
                .child(mx::zoom_icon(
                    "vz-full",
                    if full { "minimize-2" } else { "maximize-2" },
                    if full { "Leave full screen (F or Esc)" } else { "Full screen (F)" },
                    Box::new(cx.listener(|e, _: &ClickEvent, window, cx| e.toggle_full(window, cx))),
                ))
        });
        let busy = self.sampling.then(|| {
            div()
                .absolute()
                .bottom(px(14.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(360.))
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .items_center()
                        .gap(px(8.))
                        .px(px(14.))
                        .py(px(7.))
                        .rounded(px(18.))
                        .bg(t::rgba(0, 0.7))
                        .text_size(px(12.5))
                        .text_color(t::rgba(0xffffff, 1.))
                        .child(ui::spinner(16., t::rgba(0xffffff, 1.)))
                        .child(format!("Encoding a {} sample…", mx::seconds_label(self.sample_len * 1000.)))
                        .child(div().min_w(px(34.)).font_weight(FontWeight::BOLD).child(format!("{}%", (self.progress * 100.).round())))
                        .child(div().w_full().h(px(3.)).rounded(px(2.)).bg(t::rgba(0xffffff, 0.2)).child(div().h_full().w(relative(self.progress)).rounded(px(2.)).bg(t::accent()))),
                )
        });
        let error = self.error.clone().map(|e| {
            div().absolute().bottom(px(14.)).left(px(14.)).right(px(14.)).flex().justify_center().child(
                div().flex().items_center().gap(px(6.)).px(px(12.)).py(px(6.)).rounded(px(16.)).bg(t::rgba(0, 0.7)).text_size(px(12.)).text_color(t::rgba(0xfbbf24, 1.)).child(icon("triangle-alert", 13., t::rgba(0xfbbf24, 1.))).child(e),
            )
        });
        div()
            .id("mx-screen")
            .role(gpui::Role::Group)
            .aria_label("Video")
            .flex_1()
            .min_h_0()
            .relative()
            .rounded(px(if full { 0. } else { 10. }))
            .overflow_hidden()
            .bg(t::rgba(0, 1.))
            .cursor(if self.zoom.is_some() { gpui::CursorStyle::OpenHand } else { gpui::CursorStyle::PointingHand })
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .children(orig.map(|i| img(i).absolute().left(px(vx)).top(px(vy)).w(px(vw)).h(px(vh))))
            .when_some(sample, |d, s| {
                if split {
                    // The compressed picture right of the divider: the same place, clipped.
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(split_x))
                            .right_0()
                            .overflow_hidden()
                            .child(img(s).absolute().left(px(vx - split_x)).top(px(vy)).w(px(vw)).h(px(vh))),
                    )
                } else {
                    d.child(img(s).absolute().left(px(vx)).top(px(vy)).w(px(vw)).h(px(vh)))
                }
            })
            // In full screen the controls cover the bottom edge: the tags sit above them.
            .when(split, |d| d.child(mx::divider(split_x)).child(mx::tag("Original").left(px(12.)).bottom(px(tag_bottom))).child(mx::tag("Compressed").right(px(12.)).bottom(px(tag_bottom))))
            .when(unplayable && !self.in_sample(), |d| {
                d.child(div().absolute().size_full().flex().items_center().justify_center().text_size(px(12.5)).text_color(t::rgba(0xdddddd, 1.)).child("This format can't play here, but it can still be compressed."))
            })
            .children(ab)
            .children(zoom)
            .children(busy)
            .children(error)
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                e.pick = None;
                if ev.click_count >= 2 {
                    // Double-click: 100% at the pointer, or back to fit.
                    let b = e.screen_bounds.get();
                    let c = ((ev.position.x - b.origin.x).as_f32() - b.size.width.as_f32() / 2., (ev.position.y - b.origin.y).as_f32() - b.size.height.as_f32() / 2.);
                    let next = if e.zoom.is_none() { Some(1.) } else { None };
                    e.zoom_to(next, c);
                }
                e.screen_down(ev.position);
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|e, ev: &MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    e.drag_to(ev.position, cx)
                } else if e.drag != Drag::None {
                    e.drag_end(cx)
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|e, _, _, cx| e.drag_end(cx)))
            .on_scroll(cx.listener(|e, ev: &gpui::ScrollEvent, _, cx| {
                if e.unplayable() && !e.in_sample() {
                    return;
                }
                let dy = ev.delta.pixel_delta(px(20.)).y.as_f32();
                if dy == 0. {
                    return;
                }
                let b = e.screen_bounds.get();
                let c = ((ev.position.x - b.origin.x).as_f32() - b.size.width.as_f32() / 2., (ev.position.y - b.origin.y).as_f32() - b.size.height.as_f32() / 2.);
                let next = e.scale() * if dy > 0. { 1.2 } else { 1. / 1.2 };
                e.zoom_to(Some(next), c);
                cx.stop_propagation();
                cx.notify();
            }))
    }

    /// `.mx-transport`: play, sound and the time. (No "Preview quality" button: the preview follows
    /// the settings and the playhead by itself.)
    fn transport(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let playing = self.playing();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                mx::stage_icon("mx-play", if playing { "pause" } else { "play" }, if playing { "Pause" } else { "Play" }, !self.unplayable() || self.in_sample())
                    .tooltip(ui::tip(if playing { "Pause (Space)" } else { "Play (Space)" }))
                    .on_click(cx.listener(|e, _: &ClickEvent, _, cx| e.toggle_play(cx))),
            )
            .child(
                mx::stage_icon("mx-mute", if self.muted { "volume-x" } else { "volume-2" }, if self.muted { "Sound on" } else { "Sound off" }, true)
                    .tooltip(ui::tip(if self.muted { "Sound on (M)" } else { "Sound off (M)" }))
                    .on_click(cx.listener(|e, _: &ClickEvent, _, cx| {
                        e.muted = !e.muted;
                        e.apply_mute();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .ml(px(6.))
                    .flex()
                    .gap(px(4.))
                    .text_size(px(12.5))
                    .child(mx::clock(self.playhead * 1000., true))
                    .child(div().text_color(mx::stage_fg3()).child(format!("/ {}", mx::clock(self.duration() * 1000., true)))),
            )
    }

    /// `.mx-tl`: the filmstrip, the trim, the sample and the playhead.
    fn timeline(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let d = self.duration().max(0.001);
        let f = |s: f64| relative((s / d).clamp(0., 1.) as f32);
        let bounds = self.timeline_bounds.clone();
        let single = self.single();
        let (a, b) = self.trim();
        let strip = self.strip.as_ref().filter(|(i, _)| *i == self.current).and_then(|(_, s)| s.clone());
        let fresh = self.sample_fresh();
        let sample = self.sample_end();
        let handle = |end: bool| {
            div()
                .absolute()
                .top(px(-3.))
                .bottom(px(-3.))
                .w(px(12.))
                .bg(t::accent())
                .flex()
                .items_center()
                .justify_center()
                .when(!end, |d| d.left_0().rounded_l(px(6.)))
                .when(end, |d| d.right_0().rounded_r(px(6.)))
                .child(div().w(px(2.)).h(px(16.)).rounded(px(1.)).bg(t::on_accent()).opacity(0.85))
        };
        div()
            .id("mx-tl")
            .role(gpui::Role::Slider)
            .aria_label("Timeline")
            .aria_value(mx::clock(self.playhead * 1000., true))
            .flex_none()
            .relative()
            .h(px(54.))
            .rounded(px(8.))
            .bg(if t::is_dark() { t::hover() } else { t::rgba(0xffffff, 0.06) })
            .overflow_hidden()
            .cursor(gpui::CursorStyle::PointingHand)
            .child(canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .children(strip.map(|s| img(s).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill).opacity(0.85)))
            .when(single, |dv| {
                dv.child(div().absolute().top_0().bottom_0().left_0().w(f(a)).bg(t::rgba(0, 0.62)))
                    .child(div().absolute().top_0().bottom_0().left(f(b)).right_0().bg(t::rgba(0, 0.62)))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(f(a))
                            .w(f(b - a))
                            .child(div().absolute().top_0().left_0().right_0().h(px(3.)).bg(t::accent()))
                            .child(div().absolute().bottom_0().left_0().right_0().h(px(3.)).bg(t::accent()))
                            .child(handle(false))
                            .child(handle(true)),
                    )
            })
            .children(sample.map(|(s, e)| {
                div()
                    .absolute()
                    .bottom(px(3.))
                    .h(px(4.))
                    .left(f(s))
                    .w(f(e - s))
                    .rounded(px(2.))
                    .bg(if fresh { t::rgba(0x34d399, 1.) } else { t::rgba(0xfbbf24, 1.) })
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(f(self.playhead))
                    .ml(px(-1.))
                    .w(px(2.))
                    .bg(t::rgba(0xffffff, 1.))
                    .child(div().absolute().top(px(-1.)).left(px(-4.)).child(icon("ix-caret-down", 10., t::rgba(0xffffff, 1.)))),
            )
            .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| e.timeline_down(ev.position, cx)))
            // The wheel over the timeline moves through the video: a second a notch (Shift: 5 s);
            // a trackpad's sideways swipe moves it too.
            .on_scroll(cx.listener(|e, ev: &gpui::ScrollEvent, _, cx| e.wheel_scrub(ev, cx)))
            .on_mouse_move(cx.listener(|e, ev: &MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    e.drag_to(ev.position, cx)
                } else if e.drag != Drag::None {
                    e.drag_end(cx)
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|e, _, _, cx| e.drag_end(cx)))
    }

    /// `.mx-trimbar`.
    fn trim_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (a, b) = self.trim();
        let trimmed = self.trimmed();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_h(px(26.))
            .text_size(px(12.5))
            .child(div().flex_1().min_w_0().truncate().child(if trimmed {
                div()
                    .flex()
                    .gap(px(4.))
                    .child("Keeping")
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(mx::clock(a * 1000., true)))
                    .child("–")
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(mx::clock(b * 1000., true)))
                    .child(div().text_color(mx::stage_fg3()).child(format!("({})", mx::clock((b - a) * 1000., true))))
                    .into_any_element()
            } else {
                // The keys (I, O) are in the buttons' tooltips: spelled out here they read as "I/O".
                div().text_color(mx::stage_fg3()).child("Drag the ends of the timeline to trim").into_any_element()
            }))
            .child(mx::stage_btn("mx-start", "Start here", true, None).tooltip(ui::tip("Start the video at the playhead (I)")).on_click(cx.listener(|e, _: &ClickEvent, _, cx| {
                e.set_start();
                cx.notify();
            })))
            .child(mx::stage_btn("mx-end", "End here", true, None).tooltip(ui::tip("End the video at the playhead (O)")).on_click(cx.listener(|e, _: &ClickEvent, _, cx| {
                e.set_end();
                cx.notify();
            })))
            .when(trimmed, |d| {
                d.child(mx::stage_btn("mx-reset", "Reset", true, Some(icon("rotate-ccw", 13., mx::stage_fg2()).into_any_element())).tooltip(ui::tip("Keep the whole video")).on_click(cx.listener(|e, _: &ClickEvent, _, cx| {
                    let i = e.current;
                    e.trims[i] = (0., e.duration());
                    cx.notify();
                })))
            })
    }

    /// The pager of a batch (`.mx-pager`).
    fn pager(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let n = self.paths.len();
        let i = self.current;
        let arrow = |id: &'static str, name: &'static str, to: Option<usize>| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(if id == "mx-prev" { "Previous" } else { "Next" })
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .when(to.is_none(), |d| d.opacity(0.32))
                .when_some(to, |d, j| d.hover(|s| s.bg(t::rgba(0xffffff, 0.1))).on_click(cx.listener(move |e, _: &ClickEvent, _, cx| e.show(j, cx))))
                .child(icon(name, 14., mx::stage_fg2()))
        };
        div()
            .flex_none()
            .self_center()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(4.))
            .py(px(3.))
            .rounded_full()
            .bg(if t::is_dark() { t::hover() } else { t::rgba(0xffffff, 0.08) })
            .text_size(px(12.))
            .text_color(mx::stage_fg2())
            .child(arrow("mx-prev", "chevron-left", i.checked_sub(1)))
            .child(div().flex().gap(px(4.)).child(div().font_weight(FontWeight::BOLD).child(format!("{}", i + 1))).child(format!("of {n} · {}", file_name(&self.paths[i]))))
            .child(arrow("mx-next", "chevron-right", (i + 1 < n).then_some(i + 1)))
    }

    fn stage(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.infos.get(self.current) {
            None | Some(None) => {
                return div().flex_1().flex().flex_col().items_center().justify_center().gap(px(10.)).text_color(mx::stage_fg3()).child(ui::spinner(22., mx::stage_fg3())).child("Reading video…").into_any_element();
            }
            Some(Some(Err(e))) => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .p(px(30.))
                    .text_color(mx::stage_fg2())
                    .child(icon("triangle-alert", 26., t::rgba(0xf59e0b, 1.)))
                    .child(div().max_w(px(420.)).text_center().child(e.clone()))
                    .into_any_element();
            }
            _ => {}
        }
        let single = self.single();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .px(px(14.))
            .py(px(12.))
            .gap(px(10.))
            .when(!single, |d| d.child(self.pager(cx)))
            .child(self.screen(cx))
            .child(self.transport(cx))
            .child(self.timeline(cx))
            .when(single, |d| d.child(self.trim_bar(cx)))
            .into_any_element()
    }

    fn side(&self, cx: &mut Context<Self>) -> AnyElement {
        let side = mx::side("mx-side");
        let Some(info) = self.info().cloned_info() else { return side.into_any_element() };
        let s = self.settings;
        let single = self.single();
        let plan = self.plan_of(self.current);
        let set = |f: Box<dyn Fn(&mut Settings)>| -> On {
            Box::new(cx.listener(move |e: &mut Self, _: &ClickEvent, _, cx| {
                f(&mut e.settings);
                e.pick = None;
                cx.notify();
            }))
        };
        let picked = self.preset();
        let presets = mx::presets(
            PRESETS
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let st = p.s;
                    let target = self.target.clone();
                    let click: On = Box::new(cx.listener(move |e: &mut Self, _: &ClickEvent, _, cx| {
                        e.settings = st;
                        target.update(cx, |t, cx| t.set_text(&format!("{}", st.target_mb), cx));
                        cx.notify();
                    }));
                    (SharedString::from(p.label), SharedString::from(self.preset_hint(p)), picked == Some(i), click)
                })
                .collect(),
        );
        let mode = mx::seg(
            "mx-mode",
            vec![("By quality".into(), !s.by_size, set(Box::new(|s| s.by_size = false))), ("By file size".into(), s.by_size, set(Box::new(|s| s.by_size = true)))],
            true,
        );
        let size_body: AnyElement = if s.by_size {
            let chips = [8., 10., 25., 50.].into_iter().map(|mb: f64| {
                let on = (s.target_mb - mb).abs() < 1e-6;
                let target = self.target.clone();
                div()
                    .id(("mx-chip", mb as usize))
                    .role(gpui::Role::Button)
                    .aria_label(format!("{mb} MB"))
                    .h(px(26.))
                    .min_w(px(32.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(13.))
                    .border_1()
                    .text_size(px(12.))
                    .when(on, |d| d.bg(t::accent()).border_color(t::accent()).text_color(t::on_accent()))
                    .when(!on, |d| d.border_color(t::line2()).text_color(t::fg2()).hover(|s| s.border_color(t::accent_line()).text_color(t::fg())))
                    .on_click(cx.listener(move |e, _: &ClickEvent, _, cx| {
                        e.settings.target_mb = mb;
                        target.update(cx, |t, cx| t.set_text(&format!("{mb}"), cx));
                        cx.notify();
                    }))
                    .child(format!("{mb}"))
            });
            let dur = plan.as_ref().map_or(0., |p| p.dur_ms);
            div()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().w(px(74.)).flex_none().h(px(30.)).px(px(8.)).flex().items_center().rounded(px(7.)).bg(t::raised()).border_1().border_color(t::line2()).child(self.target.clone()))
                        .child(div().text_color(t::fg3()).child("MB"))
                        .children(chips),
                )
                .child(match &plan {
                    Some(p) if p.too_small => mx::warn(format!("Too small for {} of video. Trim it, lower the resolution, or allow more MB.", mx::clock(dur, false))),
                    Some(p) => mx::hint(format!(
                        "{}Bitrate {:.2} Mbps for {}{}",
                        if single { "" } else { "Each video at most this size. " },
                        p.vb as f64 / 1e6,
                        mx::clock(dur, false),
                        if single { "" } else { " (this one)" }
                    )),
                    None => div(),
                })
                .into_any_element()
        } else {
            let bounds = self.slider_bounds.clone();
            div()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            mx::slider("mx-quality", (s.quality / 100.) as f32, bounds)
                                .aria_label("Quality")
                                .on_mouse_down(MouseButton::Left, cx.listener(|e, ev: &MouseDownEvent, _, cx| {
                                    e.drag = Drag::Quality;
                                    e.drag_to(ev.position, cx);
                                }))
                                .on_mouse_move(cx.listener(|e, ev: &MouseMoveEvent, _, cx| {
                                    if ev.pressed_button == Some(MouseButton::Left) && e.drag == Drag::Quality {
                                        e.drag_to(ev.position, cx)
                                    }
                                }))
                                .on_mouse_up(MouseButton::Left, cx.listener(|e, _, _, cx| e.drag_end(cx))),
                        )
                        .child(div().min_w(px(64.)).flex().justify_end().font_weight(FontWeight::SEMIBOLD).text_color(t::accent_text()).child(quality_label(s.quality))),
                )
                .child(mx::hint(format!(
                    "{:.2} Mbps video{}",
                    plan.as_ref().map_or(0., |p| p.vb as f64 / 1e6),
                    if info.video_bitrate > 0 { format!(" · original {:.1} Mbps", info.video_bitrate as f64 / 1e6) } else { String::new() }
                )))
                .into_any_element()
        };
        // Resolution: keep, or a smaller short side.
        let short = info.width.min(info.height);
        let res_label = match &plan {
            Some(p) if s.res > 0 && s.res < short => format!("{}p · {} × {}", s.res, p.ow, p.oh),
            Some(p) => format!("Keep {} × {}", p.dw, p.dh),
            None => "Keep".into(),
        };
        let res_menu = (self.pick == Some(Pick::Res)).then(|| {
            let mut options: Vec<(SharedString, bool, On)> = Vec::new();
            if let Some(p) = &plan {
                options.push((format!("Keep {} × {}", p.dw, p.dh).into(), !(s.res > 0 && s.res < short), set(Box::new(|s| s.res = 0))));
            }
            for r in RESOLUTIONS.into_iter().filter(|r| *r < short) {
                let pl = self.plan_with(self.current, &Settings { res: r, ..s }, false);
                let label = pl.map(|p| format!("{r}p · {} × {}", p.ow, p.oh)).unwrap_or_else(|| format!("{r}p"));
                options.push((label.into(), s.res == r, set(Box::new(move |s| s.res = r))));
            }
            mx::select_menu("mx-res-list", options)
        });
        let res = mx::select("mx-res", res_label, self.pick == Some(Pick::Res), res_menu, Box::new(cx.listener(|e, _: &ClickEvent, _, cx| {
            e.pick = if e.pick == Some(Pick::Res) { None } else { Some(Pick::Res) };
            cx.notify();
        })));
        // Frame rate: keep, or lower (only to rates below the source's).
        let now = plan.as_ref().map_or(0, |p| p.fps);
        let mut fps: Vec<(SharedString, bool, On)> = vec![(if info.fps > 0. { format!("Keep {:.0} fps", info.fps) } else { "Keep".into() }.into(), now == 0, set(Box::new(|s| s.fps = 0)))];
        for f in [30u32, 24] {
            if (f as f64) < info.fps - 0.5 {
                fps.push((format!("{f} fps").into(), now == f, set(Box::new(move |s| s.fps = f))));
            }
        }
        let format = mx::seg("mx-codec", vec![("H.264".into(), !s.hevc, set(Box::new(|s| s.hevc = false))), ("HEVC".into(), s.hevc, set(Box::new(|s| s.hevc = true)))], false);
        let has_audio = info.has_audio;
        let sound: AnyElement = if has_audio {
            let audio_menu = (self.pick == Some(Pick::Audio)).then(|| {
                mx::select_menu("mx-kbps-list", [64u32, 96, 128, 160, 192].into_iter().map(|k| (SharedString::from(format!("{k} kbps")), s.audio == k, set(Box::new(move |s| s.audio = k)))).collect())
            });
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().flex().child(mx::seg(
                    "mx-sound",
                    vec![
                        ("Keep".into(), self.keep_audio, Box::new(cx.listener(|e: &mut Self, _: &ClickEvent, _, cx| {
                            if !e.keep_audio {
                                e.keep_audio = true;
                                e.settings.audio = 128;
                            }
                            cx.notify();
                        })) as On),
                        ("Remove".into(), !self.keep_audio, Box::new(cx.listener(|e: &mut Self, _: &ClickEvent, _, cx| {
                            e.keep_audio = false;
                            cx.notify();
                        })) as On),
                    ],
                    false,
                )))
                .when(self.keep_audio, |d| {
                    d.child(div().w(px(110.)).flex_none().child(mx::select("mx-kbps", format!("{} kbps", s.audio), self.pick == Some(Pick::Audio), audio_menu, Box::new(cx.listener(|e, _: &ClickEvent, _, cx| {
                        e.pick = if e.pick == Some(Pick::Audio) { None } else { Some(Pick::Audio) };
                        cx.notify();
                    })))))
                })
                .into_any_element()
        } else {
            mx::hint("This video has no sound.").into_any_element()
        };
        // Save as (one) or the name ending (several).
        let err = self.name_error(cx);
        let dest = (single && err.is_none()).then(|| self.dest_for(0, cx));
        let taken = dest.as_ref().is_some_and(|d| Path::new(d).exists());
        let name_hint = match err {
            Some(e) => div().mt(px(6.)).text_size(px(11.5)).text_color(t::danger()).child(e),
            None if single && taken => mx::hint("There's already a file with this name, so this one gets “ (2)” added. Nothing is replaced."),
            None if single => mx::hint("Saved next to the original, which stays as it is."),
            None => mx::hint(format!("e.g. {}{}.mp4", stem(&self.paths[0]), self.name.read(cx).text())),
        };
        let unreadable: Vec<String> = self.infos.iter().enumerate().filter(|(_, r)| matches!(r, Some(Err(_)))).map(|(i, _)| file_name(&self.paths[i])).collect();
        side.child(mx::section("Preset", None, false).child(presets))
            .child(mx::section("Size", Some(mode.into_any_element()), false).child(size_body))
            .child(mx::section("Resolution", None, false).child(res))
            .child(mx::section("Frame rate", None, false).child(mx::seg("mx-fps", fps, false)))
            .child(mx::section("Format", None, false).child(format).child(mx::hint(if s.hevc {
                "Smaller files; some older devices and websites can't play HEVC."
            } else {
                "MP4 that plays on every device and website."
            })))
            .child(mx::section("Sound", None, false).child(sound))
            .child(mx::section(if single { "Save as" } else { "Name ending" }, None, unreadable.is_empty()).child(mx::name_field(self.name.clone(), ".mp4", err.is_some())).child(name_hint))
            .when(!unreadable.is_empty(), |d| {
                d.child(mx::warn(format!("{} can't be read and will be skipped: {}", crate::files::plural(unreadable.len(), "video"), unreadable.join(", "))))
            })
            .into_any_element()
    }
}

/// `Option<&VideoInfo>` → an owned copy of what the side panel reads.
trait CloneInfo {
    fn cloned_info(self) -> Option<VideoInfo>;
}

impl CloneInfo for Option<&VideoInfo> {
    fn cloned_info(self) -> Option<VideoInfo> {
        self.map(|i| VideoInfo {
            path: i.path.clone(),
            size: i.size,
            width: i.width,
            height: i.height,
            rotation: i.rotation,
            duration_ms: i.duration_ms,
            fps: i.fps,
            video_bitrate: i.video_bitrate,
            has_audio: i.has_audio,
            audio_bitrate: i.audio_bitrate,
            codec: i.codec.clone(),
        })
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_players(window);
        self.schedule_auto_sample(cx);
        self.schedule_poll(window, cx);
        self.ensure_strip(window, cx);
        // Full screen: the picture as it would be watched, with its overlays, and the controls
        // over its bottom edge (play, time, the timeline to scrub and trim): they fade out while
        // it plays and the pointer rests, and come back when it moves (user request, 2026-11-11;
        // it used to be the picture alone, no way to move through the video).
        if self.full {
            let shown = !self.playing() || self.full_moved.elapsed() < FULL_CONTROLS_FOR;
            let single = self.single();
            return div()
                .id("editor-full")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::key))
                .on_mouse_move(cx.listener(|e, _: &MouseMoveEvent, _, cx| e.full_pointer_moved(cx)))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .bg(t::rgba(0, 1.))
                .flex()
                .flex_col()
                .child(self.screen(cx))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .px(px(24.))
                        .pt(px(36.))
                        .pb(px(18.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .bg(gpui::linear_gradient(0., gpui::linear_color_stop(t::rgba(0, 0.78), 0.), gpui::linear_color_stop(t::rgba(0, 0.), 1.)))
                        .when(!shown, |d| d.opacity(0.))
                        .child(self.transport(cx))
                        .child(self.timeline(cx))
                        .when(single, |d| d.child(self.trim_bar(cx))),
                )
                .into_any_element();
        }
        let (w, h) = mx::card_size(window);
        let n = self.paths.len();
        let title = if n > 1 { format!("Compress {n} videos") } else { "Compress video".to_string() };
        let sub = match (self.info(), self.plan_of(self.current)) {
            (Some(i), Some(p)) => format!("{} · {} · {} × {} · {:.0} fps · {}", file_name(&self.paths[self.current]), i.codec, p.dw, p.dh, i.fps, mx::clock(i.duration_ms as f64, false)),
            _ => file_name(&self.paths[self.current]),
        };
        let (before, after, measured) = self.totals();
        let known = (0..n).any(|i| self.info_of(i).is_some());
        let note = if measured {
            "Measured from the preview sample"
        } else if known {
            if self.sampling { "Estimate · measuring it on a preview…" } else { "Estimate" }
        } else {
            ""
        };
        let summary = mx::summary(before, known.then_some(after), true, note);
        let can = self.can_submit(cx);
        let foot = mx::foot(
            summary,
            Box::new(cx.listener(|e, _: &ClickEvent, window, cx| e.close(window, cx))),
            can.then(|| Box::new(cx.listener(|e, _: &ClickEvent, _, cx| e.submit(cx))) as On),
            "Compress",
        );
        let stage = self.stage(cx);
        let side = self.side(cx);
        div()
            .id("editor-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(t::scrim())
            .flex()
            .items_center()
            .justify_center()
            .child(
                mx::card(w, h)
                    .id("editor")
                    .role(gpui::Role::Dialog)
                    .aria_label(title.clone())
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::key))
                    // A click anywhere closes an open drop-down (its own items stop the click).
                    .on_mouse_down(MouseButton::Left, cx.listener(|e, _, _, cx| {
                        if e.pick.is_some() {
                            e.pick = None;
                            cx.notify();
                        }
                    }))
                    .child(mx::head("clapperboard", title, sub, Box::new(cx.listener(|e, _: &ClickEvent, window, cx| e.close(window, cx)))))
                    .child(mx::main(stage, side))
                    .child(foot),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> VideoInfo {
        VideoInfo {
            path: String::new(),
            size: 18_000_000,
            width: 1920,
            height: 1080,
            rotation: 0,
            duration_ms: 12_000,
            fps: 30.,
            video_bitrate: 12_000_000,
            has_audio: true,
            audio_bitrate: 128_000,
            codec: "H.264".into(),
        }
    }

    #[test]
    fn presets_plan_like_the_app() {
        let p = plan(&info(), &PRESETS[0].s, true, 12_000.);
        assert_eq!((p.sw, p.sh, p.fps), (1280, 720, 0));
        assert!(p.vb >= 100_000 && p.vb < 12_000_000);
        // HEVC asks for less at the same quality.
        let mut hevc = PRESETS[1].s;
        hevc.hevc = true;
        assert!(plan(&info(), &hevc, true, 12_000.).vb < plan(&info(), &PRESETS[1].s, true, 12_000.).vb);
        // By size: 10 MB over 12 s, sound included, lands under 10 MB.
        let fit = plan(&info(), &PRESETS[3].s, true, 12_000.);
        assert!(fit.bytes < 10. * 1_048_576. && fit.bytes > 7. * 1_048_576., "{}", fit.bytes);
        // A target too small for the length says so.
        let mut tiny = PRESETS[3].s;
        tiny.target_mb = 1.;
        let long = plan(&info(), &tiny, true, 600_000.);
        assert!(long.too_small);
    }
}
