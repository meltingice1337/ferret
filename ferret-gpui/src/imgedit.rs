//! The picture compressor (MediaEditor.tsx, `ImageEditor` and `Compare`), in the shared look of
//! `mx.rs`: presets, JPEG or PNG (or chosen per picture), 256-colour PNGs, quality, the longest
//! side; a before/after view with a divider to drag, wheel or double-click zoom with panning, and
//! the differences brightened; the exact output size in the footer; a pager for several pictures.
//! Windows' WIC does the work in ferret-core.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Entity, Focusable, FontWeight, Image, ImageFormat, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Point, SharedString, Window, div, prelude::*, px,
};

use crate::imgmem::img;

use crate::dialogs::Dialog;
use crate::input::TextInput;
use crate::kinds;
use crate::mx::{self, On};
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util;
use crate::view::Ferret;

/// The app's presets: label, quality, longest side, 256 colours.
const PRESETS: [(&str, &str, u32, u32, bool); 4] = [
    ("Balanced", "2560 px · 82%", 82, 2560, true),
    ("Web", "1920 px · 78%", 78, 1920, true),
    ("Small", "1280 px · 70%", 70, 1280, true),
    ("Best quality", "Full size · 92%", 92, 0, false),
];
const SIDES: [u32; 7] = [3840, 2560, 1920, 1600, 1280, 1024, 800];

pub struct ImgEdit {
    pub paths: Vec<String>,
    pub at: usize,
    /// "auto" | "jpeg" | "png"
    pub fmt: &'static str,
    pub quality: u32,
    pub max_side: u32,
    pub palette: bool,
    pub name: Entity<TextInput>,
    pub info: Option<ferret_core::media::ImageInfo>,
    /// The original at the output size (near-lossless), and its bytes.
    pub before: Option<Arc<Image>>,
    /// The picture as it will be saved: its image, bytes, size, and the settings it was made with.
    pub after: Option<(Arc<Image>, u64, u32, u32)>,
    pub after_opts: Option<(&'static str, u32, u32, bool)>,
    pub loading: bool,
    pub split: f32,
    pub diff: bool,
    /// The differences image, for which `after` (its bytes' address).
    pub diff_img: Option<(usize, Arc<Image>)>,
    pub diff_busy: bool,
    pub zoom: Option<f32>,
    pub pan: (f32, f32),
    /// A press on the picture: where, the pan then, and whether it pans (else it moves the divider).
    pub press: Option<(Point<Pixels>, (f32, f32), bool)>,
    pub size_open: bool,
    pub quality_drag: bool,
    pub seq: u64,
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    pub slider: Rc<Cell<Bounds<Pixels>>>,
}

impl ImgEdit {
    fn opts(&self) -> ferret_core::media::ImageOpts {
        ferret_core::media::ImageOpts { png: self.fmt == "png", auto: self.fmt == "auto", quality: self.quality, max_side: self.max_side, palette: self.palette }
    }

    fn key(&self) -> (&'static str, u32, u32, bool) {
        (self.fmt, self.quality, self.max_side, self.palette)
    }

    /// The picture on screen comes out as PNG.
    fn png(&self) -> bool {
        self.fmt == "png" || (self.fmt == "auto" && self.info.as_ref().is_some_and(|i| i.alpha))
    }

    fn single(&self) -> bool {
        self.paths.len() == 1
    }

    /// The size it comes out at.
    fn out_size(&self) -> Option<(u32, u32)> {
        let i = self.info.as_ref()?;
        let long = i.width.max(i.height);
        let k = if self.max_side > 0 && long > self.max_side { self.max_side as f64 / long as f64 } else { 1. };
        Some((((i.width as f64 * k).round() as u32).max(1), ((i.height as f64 * k).round() as u32).max(1)))
    }

    fn box_size(&self) -> (f32, f32) {
        let b = self.bounds.get();
        (b.size.width.as_f32().max(1.), b.size.height.as_f32().max(1.))
    }

    /// Fit never enlarges past 100% (`Compare`'s fit).
    fn fit(&self) -> f32 {
        let (bw, bh) = self.box_size();
        let (w, h) = self.out_size().unwrap_or((16, 9));
        ((bw - 24.) / w as f32).min((bh - 24.) / h as f32).min(1.)
    }

    fn scale(&self) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit())
    }

    fn frame(&self) -> (f32, f32, f32, f32) {
        let (bw, bh) = self.box_size();
        let (w, h) = self.out_size().unwrap_or((16, 9));
        let k = self.scale();
        let (dw, dh) = (w as f32 * k, h as f32 * k);
        (bw / 2. - dw / 2. + self.pan.0, bh / 2. - dh / 2. + self.pan.1, dw, dh)
    }

    fn clamp_pan(&self, p: (f32, f32), k: f32) -> (f32, f32) {
        let (bw, bh) = self.box_size();
        let (w, h) = self.out_size().unwrap_or((16, 9));
        let (mx, my) = (((w as f32 * k - bw) / 2. + 12.).max(0.), ((h as f32 * k - bh) / 2. + 12.).max(0.));
        (p.0.clamp(-mx, mx), p.1.clamp(-my, my))
    }

    fn zoom_to(&mut self, next: Option<f32>, c: (f32, f32)) {
        let (old, fit) = (self.scale(), self.fit());
        match next {
            Some(n) if n > fit + 1e-3 => {
                let n = n.min(8.);
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

    /// A point on the picture area, from its middle.
    fn from_middle(&self, at: Point<Pixels>) -> (f32, f32) {
        let b = self.bounds.get();
        ((at.x - b.origin.x).as_f32() - b.size.width.as_f32() / 2., (at.y - b.origin.y).as_f32() - b.size.height.as_f32() / 2.)
    }
}

fn decode(url: &str) -> Option<Arc<Image>> {
    let bytes = crate::menu::decode_data_url(url)?;
    let fmt = if url.contains("image/png") { ImageFormat::Png } else { ImageFormat::Jpeg };
    Some(Arc::new(Image::from_bytes(fmt, bytes)))
}

/// How much each pixel of `after` differs from `before` (`drawDifferences`): a dimmed grey copy
/// of the original with the changes glowing on top, amplified ×8 so mild JPEG loss shows. A PNG.
fn differences(before: &[u8], after: &[u8]) -> Option<Vec<u8>> {
    let b = image::load_from_memory(after).ok()?.to_rgba8();
    let (w, h) = b.dimensions();
    let mut a = image::load_from_memory(before).ok()?.to_rgba8();
    if a.dimensions() != (w, h) {
        a = image::imageops::resize(&a, w, h, image::imageops::FilterType::Triangle);
    }
    let mut out = image::RgbaImage::new(w, h);
    for ((pa, pb), po) in a.pixels().zip(b.pixels()).zip(out.pixels_mut()) {
        let d = ((pa[0] as f32 - pb[0] as f32).abs() + (pa[1] as f32 - pb[1] as f32).abs() + (pa[2] as f32 - pb[2] as f32).abs()) * (8. / 3.);
        let grey = (pa[0] as f32 * 0.3 + pa[1] as f32 * 0.59 + pa[2] as f32 * 0.11) * 0.22;
        *po = image::Rgba([(grey + d).min(255.) as u8, (grey + d * 0.55).min(255.) as u8, (grey + d * 0.15).min(255.) as u8, 255]);
    }
    let mut png = std::io::Cursor::new(Vec::new());
    out.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

impl Ferret {
    pub fn show_image_editor(&mut self, paths: Vec<String>, _window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let single = paths.len() == 1;
        let name = cx.new(|cx| {
            let mut i = TextInput::new("", cx).sized(13., 20.).named(if single { "Save as" } else { "Name ending" });
            if !single {
                i.set_text(" (compressed)", cx);
            }
            i
        });
        // The name is checked as it's typed.
        cx.observe(&name, |_, _, cx| cx.notify()).detach();
        self.dialog = Some(Dialog::Image(Box::new(ImgEdit {
            paths,
            at: 0,
            fmt: "auto",
            quality: 82,
            max_side: 2560,
            palette: true,
            name,
            info: None,
            before: None,
            after: None,
            after_opts: None,
            loading: true,
            split: 0.5,
            diff: false,
            diff_img: None,
            diff_busy: false,
            zoom: None,
            pan: (0., 0.),
            press: None,
            size_open: false,
            quality_drag: false,
            seq: 0,
            bounds: Default::default(),
            slider: Default::default(),
        })));
        self.img_load(cx);
        cx.notify();
    }

    fn img_edit(&mut self) -> Option<&mut ImgEdit> {
        match self.dialog.as_mut() {
            Some(Dialog::Image(e)) => Some(e),
            _ => None,
        }
    }

    /// Reads the picture and makes the before and after previews, 180 ms after the settings
    /// stop changing.
    fn img_load(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.img_edit() else { return };
        e.seq += 1;
        e.loading = true;
        let seq = e.seq;
        let path = e.paths[e.at].clone();
        let opts = e.opts();
        let key = e.key();
        let need_info = e.info.as_ref().is_none_or(|i| i.path != path);
        let max = e.max_side;
        let name_input = e.name.clone();
        let single = e.single();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            if this.update(cx, |f, _| f.img_edit().is_some_and(|e| e.seq == seq)).ok() != Some(true) {
                return;
            }
            let p2 = path.clone();
            let task = cx.background_executor().spawn(async move {
                let info = if need_info { ferret_core::rt::block_on(ferret_core::media::image_info(p2.clone())).ok() } else { None };
                let alpha = info.as_ref().map(|i| i.alpha);
                (info, alpha, p2)
            });
            let (info, alpha, p2) = task.await;
            let alpha = match alpha {
                Some(a) => a,
                None => this.update(cx, |f, _| f.img_edit().and_then(|e| e.info.as_ref().map(|i| i.alpha)).unwrap_or(false)).unwrap_or(false),
            };
            let task = cx.background_executor().spawn(async move {
                // The original at the same size, near-lossless, so zooming in compares like for like.
                let before = ferret_core::rt::block_on(ferret_core::media::image_preview(p2.clone(), ferret_core::media::ImageOpts { png: alpha, auto: false, quality: 100, max_side: max, palette: false })).ok();
                let after = ferret_core::rt::block_on(ferret_core::media::image_preview(p2, opts));
                (before, after)
            });
            let (before, after) = task.await;
            this.update(cx, |f, cx| {
                let Some(e) = f.img_edit() else { return };
                if e.seq != seq {
                    return;
                }
                if let Some(i) = info {
                    if single {
                        let stem = util::file_name(&path);
                        let stem = stem.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(stem);
                        name_input.update(cx, |n, cx| n.set_text(&format!("{stem} (compressed)"), cx));
                    }
                    let Some(e) = f.img_edit() else { return };
                    e.info = Some(i);
                    e.zoom = None;
                    e.pan = (0., 0.);
                }
                let Some(e) = f.img_edit() else { return };
                if let Some(b) = before.and_then(|b| decode(&b.data_url)) {
                    e.before = Some(b);
                }
                match after {
                    Ok(a) => {
                        e.after = decode(&a.data_url).map(|i| (i, a.bytes, a.width, a.height));
                        e.after_opts = Some(key);
                    }
                    Err(err) => {
                        e.after = None;
                        f.toast_error(err, cx);
                    }
                }
                if let Some(e) = f.img_edit() {
                    e.loading = false;
                }
                f.img_diff(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The differences image for the current before and after, when it's asked for.
    fn img_diff(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.img_edit() else { return };
        let (Some(b), Some((a, ..))) = (e.before.clone(), e.after.clone()) else { return };
        let id = Arc::as_ptr(&a) as usize;
        if !e.diff || e.diff_busy || e.diff_img.as_ref().is_some_and(|(k, _)| *k == id) {
            return;
        }
        e.diff_busy = true;
        let task = cx.background_executor().spawn(async move { differences(b.bytes(), a.bytes()) });
        cx.spawn(async move |this, cx| {
            let png = task.await;
            this.update(cx, |f, cx| {
                if let Some(e) = f.img_edit() {
                    e.diff_busy = false;
                    if let Some(png) = png {
                        e.diff_img = Some((id, Arc::new(Image::from_bytes(ImageFormat::Png, png))));
                    }
                }
                // Newer pictures came meanwhile: their differences.
                f.img_diff(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn img_set(&mut self, change: impl FnOnce(&mut ImgEdit), cx: &mut Context<Self>) {
        if let Some(e) = self.img_edit() {
            change(e);
            e.size_open = false;
        }
        self.img_load(cx);
        cx.notify();
    }

    fn img_compress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Image(e)) = self.dialog.as_ref() else { return };
        let typed = e.name.read(cx).text().to_string();
        if e.info.is_none() || mx::name_error(&typed, e.single()).is_some() {
            return;
        }
        let opts = e.opts();
        let single = e.single();
        let png = e.png();
        let jobs: Vec<ferret_core::media::ImageJob> = e
            .paths
            .iter()
            .map(|p| {
                let dir = util::parent(p).unwrap_or_default();
                let stem = util::file_name(p);
                let stem = stem.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(stem);
                let base = if single { typed.trim().to_string() } else { format!("{stem}{typed}") };
                // With "Auto" each picture's own format sets its extension (in ferret-core).
                let ext = if png && single { "png" } else { "jpg" };
                let file = util::unique_name(&dir, &format!("{base}.{ext}"));
                ferret_core::media::ImageJob { source: p.clone(), dest: format!("{}\\{file}", dir.trim_end_matches('\\')) }
            })
            .collect();
        let before: u64 = e.paths.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let n = e.paths.len();
        match ferret_core::media::compress_images(jobs, opts) {
            Ok(id) => {
                self.close_dialog(window, cx);
                crate::ops::when_done(id, move |f, u, cx| {
                    if u.status != "done" {
                        return;
                    }
                    let created = u.created.clone();
                    let after: u64 = created.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
                    let what = if created.len() == 1 { util::file_name(&created[0]) } else { crate::files::plural(n, "picture") };
                    let msg = if after == 0 {
                        format!("Saved {what}")
                    } else if after >= before {
                        format!("{what} came out {}, not smaller than the original", kinds::format_size(after))
                    } else {
                        format!("{what}: {} → {}, {}% smaller", kinds::format_size(before), kinds::format_size(after), ((1. - after as f64 / before.max(1) as f64) * 100.).round())
                    };
                    let (d1, d2) = (created.clone(), created.clone());
                    f.record(format!("Compress {what}"), move |f, cx| f.recycle_quiet(d1.clone(), cx), move |f, cx| f.unrecycle(d2.clone(), cx), Some(msg), cx);
                    f.pane_mut().pending_select = Some(created.clone());
                    let dir = f.pane().path.clone();
                    f.reload_dir(&dir, cx);
                });
            }
            Err(e) => self.toast_error(e, cx),
        }
    }

    // ───────────────────────────── drawing ─────────────────────────────

    /// `Compare`: the original left of the divider, the result (or its differences) right of it.
    fn img_compare(&self, e: &ImgEdit, cx: &mut Context<Self>) -> AnyElement {
        let (l, tp, w, h) = e.frame();
        let (bw, _) = e.box_size();
        let split_x = e.split * bw;
        let right = if e.diff { e.diff_img.as_ref().map(|(_, i)| i.clone()) } else { e.after.as_ref().map(|a| a.0.clone()) };
        let bounds = e.bounds.clone();
        let size = e.info.as_ref().map_or(0, |i| i.size);
        let right_label = if e.diff {
            "Differences, brightened".to_string()
        } else {
            match &e.after {
                Some((_, bytes, ..)) => format!("{} · {}", if e.png() { if e.palette { "PNG · 256 colours".to_string() } else { "PNG".into() } } else { format!("JPEG {}%", e.quality) }, kinds::format_size(*bytes)),
                None => "Compressed".into(),
            }
        };
        let busy = e.loading || e.diff_busy;
        let zoomed = e.zoom.is_some();
        let n = e.paths.len();
        let at = e.at;
        let zoom = mx::zoom_bar()
            .right(px(12.))
            .bottom(px(12.))
            .child(
                mx::zoom_btn("iz-diff", "Differences", e.diff, Box::new(cx.listener(|f, _: &ClickEvent, _, cx| {
                    if let Some(e) = f.img_edit() {
                        e.diff = !e.diff;
                    }
                    f.img_diff(cx);
                    cx.notify();
                })))
                .tooltip(ui::tip("Highlight what compression changed (bright = more change)")),
            )
            .child(mx::zoom_sep())
            .child(mx::zoom_btn("iz-fit", "Fit", !zoomed, Box::new(cx.listener(|f, _: &ClickEvent, _, cx| {
                if let Some(e) = f.img_edit() {
                    e.zoom_to(None, (0., 0.));
                }
                cx.notify();
            }))).tooltip(ui::tip("Fit the window")))
            .child(mx::zoom_btn("iz-100", "100%", e.zoom.is_some_and(|z| (z - 1.).abs() < 1e-3), Box::new(cx.listener(|f, _: &ClickEvent, _, cx| {
                if let Some(e) = f.img_edit() {
                    e.zoom_to(Some(1.), (0., 0.));
                }
                cx.notify();
            }))).tooltip(ui::tip("Actual pixels (double-click)")))
            .child(mx::zoom_pct(e.scale()));
        let pager = (n > 1).then(|| {
            let arrow = |id: &'static str, name: &'static str, to: Option<usize>| {
                div()
                    .id(id)
                    .role(gpui::Role::Button)
                    .aria_label(if id == "ip-prev" { "Previous" } else { "Next" })
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .when(to.is_none(), |d| d.opacity(0.32))
                    .when_some(to, |d, j| {
                        d.hover(|s| s.bg(t::rgba(0xffffff, 0.12))).on_click(cx.listener(move |f, _: &ClickEvent, _, cx| f.img_set(|e| {
                            e.at = j;
                            e.info = None;
                            e.before = None;
                            e.after = None;
                        }, cx)))
                    })
                    .child(icon(name, 14., t::rgba(0xffffff, 1.)))
            };
            div()
                .absolute()
                .top(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(4.))
                        .py(px(3.))
                        .rounded_full()
                        .bg(t::rgba(0, 0.55))
                        .text_size(px(12.))
                        .text_color(t::rgba(0xffffff, 1.))
                        .occlude()
                        .child(arrow("ip-prev", "chevron-left", at.checked_sub(1)))
                        .child(div().flex().gap(px(4.)).child(div().font_weight(FontWeight::BOLD).child(format!("{}", at + 1))).child(format!("of {n} · {}", util::file_name(&e.paths[at]))))
                        .child(arrow("ip-next", "chevron-right", (at + 1 < n).then_some(at + 1))),
                )
        });
        let tags_top = px(if n > 1 { 48. } else { 12. });
        div()
            .id("mx-compare")
            .role(gpui::Role::Group)
            .aria_label("Before and after")
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .cursor(if zoomed { gpui::CursorStyle::OpenHand } else { gpui::CursorStyle::ResizeLeftRight })
            .child(gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(
                div()
                    .absolute()
                    .left(px(l))
                    .top(px(tp))
                    .w(px(w))
                    .h(px(h))
                    .shadow(vec![gpui::BoxShadow { color: t::rgba(0, 0.45), offset: gpui::point(px(0.), px(6.)), blur_radius: px(30.), spread_radius: px(0.), inset: false }])
                    .children(e.before.clone().map(|b| img(b).size_full())),
            )
            .children(right.map(|r| {
                div().absolute().top_0().bottom_0().left(px(split_x)).right_0().overflow_hidden().child(img(r).absolute().left(px(l - split_x)).top(px(tp)).w(px(w)).h(px(h)))
            }))
            .child(mx::divider(split_x))
            .child(mx::tag(format!("Original · {}", kinds::format_size(size))).top(tags_top).left(px(12.)))
            .child(mx::tag(right_label).top(tags_top).right(px(12.)).when(busy, |d| d.child(ui::spinner(12., t::rgba(0xffffff, 1.)))))
            .when(!zoomed && !e.diff, |d| d.child(mx::tag("Drag to compare · scroll or double-click to zoom in on details").bottom(px(12.)).left(px(12.)).text_color(t::rgba(0xffffff, 0.8))))
            .children(pager)
            .child(zoom)
            .on_mouse_down(MouseButton::Left, cx.listener(|f, ev: &MouseDownEvent, _, cx| {
                let Some(e) = f.img_edit() else { return };
                e.size_open = false;
                if ev.click_count >= 2 {
                    let c = e.from_middle(ev.position);
                    let next = if e.zoom.is_none() { Some(1.) } else { None };
                    e.zoom_to(next, c);
                    cx.notify();
                    return;
                }
                let b = e.bounds.get();
                let on_handle = ((ev.position.x - b.origin.x).as_f32() - e.split * b.size.width.as_f32()).abs() < 10.;
                let panning = e.zoom.is_some() && !on_handle;
                e.press = Some((ev.position, e.pan, panning));
                if !panning {
                    e.split = ((ev.position.x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
                }
                cx.notify();
            }))
            .on_mouse_move(cx.listener(|f, ev: &MouseMoveEvent, _, cx| {
                let Some(e) = f.img_edit() else { return };
                let Some((from, pan, panning)) = e.press else { return };
                if ev.pressed_button != Some(MouseButton::Left) {
                    e.press = None;
                    return;
                }
                if panning {
                    let k = e.scale();
                    e.pan = e.clamp_pan((pan.0 + (ev.position.x - from.x).as_f32(), pan.1 + (ev.position.y - from.y).as_f32()), k);
                } else {
                    let b = e.bounds.get();
                    e.split = ((ev.position.x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
                }
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|f, _, _, _| {
                if let Some(e) = f.img_edit() {
                    e.press = None;
                }
            }))
            .on_scroll(cx.listener(|f, ev: &gpui::ScrollEvent, _, cx| {
                let Some(e) = f.img_edit() else { return };
                let dy = ev.delta.pixel_delta(px(20.)).y.as_f32();
                if dy == 0. {
                    return;
                }
                let c = e.from_middle(ev.position);
                let next = e.scale() * if dy > 0. { 1.2 } else { 1. / 1.2 };
                e.zoom_to(Some(next), c);
                cx.stop_propagation();
                cx.notify();
            }))
            .into_any_element()
    }

    fn img_side(&self, e: &ImgEdit, cx: &mut Context<Self>) -> AnyElement {
        let side = mx::side("img-side");
        let Some(i) = e.info.as_ref() else { return side.into_any_element() };
        let single = e.single();
        let png = e.png();
        let long = i.width.max(i.height);
        let set = |f: Box<dyn Fn(&mut ImgEdit)>| -> On { Box::new(cx.listener(move |me: &mut Ferret, _: &ClickEvent, _, cx| me.img_set(|e| f(e), cx))) };
        let presets = mx::presets(
            PRESETS
                .iter()
                .map(|&(label, hint, q, side, pal)| {
                    let on = e.quality == q && e.max_side == side && e.palette == pal;
                    (SharedString::from(label), SharedString::from(hint), on, set(Box::new(move |e| {
                        e.quality = q;
                        e.max_side = side;
                        e.palette = pal;
                    })))
                })
                .collect(),
        );
        let fmt = mx::seg(
            "img-fmt",
            [("auto", "Auto"), ("jpeg", "JPEG"), ("png", "PNG")].into_iter().map(|(v, l)| (SharedString::from(l), e.fmt == v, set(Box::new(move |e| e.fmt = v)))).collect(),
            false,
        );
        let fmt_hint = match e.fmt {
            "auto" if single => if i.alpha { "PNG: this picture has transparency." } else { "JPEG: no transparency to keep." },
            "auto" => "Each picture: PNG when it has transparency, otherwise JPEG.",
            "png" => "Keeps transparency and sharp edges.",
            _ => "Much smaller; best for photos and screenshots.",
        };
        let show_palette = if single { png } else { e.fmt != "jpeg" };
        let show_quality = if single { !png } else { e.fmt != "png" };
        let quality = show_quality.then(|| {
            let v = (e.quality as f32 - 10.) / 90.;
            mx::section("Quality", Some(div().font_weight(FontWeight::SEMIBOLD).text_color(t::accent_text()).child(format!("{}%", e.quality)).into_any_element()), false)
                .child(
                    div().flex().child(
                        mx::slider("img-quality", v, e.slider.clone())
                            .aria_label("Quality")
                            .on_mouse_down(MouseButton::Left, cx.listener(|f, ev: &MouseDownEvent, _, cx| f.img_quality_at(ev.position, true, cx)))
                            .on_mouse_move(cx.listener(|f, ev: &MouseMoveEvent, _, cx| {
                                if ev.pressed_button == Some(MouseButton::Left) {
                                    f.img_quality_at(ev.position, false, cx)
                                }
                            }))
                            .on_mouse_up(MouseButton::Left, cx.listener(|f, _, _, _| {
                                if let Some(e) = f.img_edit() {
                                    e.quality_drag = false;
                                }
                            })),
                    ),
                )
                .child(mx::hint(match e.quality {
                    90.. => "Visually the same as the original",
                    75..=89 => "Hard to tell apart; much smaller",
                    55..=74 => "Small; fine detail softens",
                    _ => "Smallest; visible blockiness",
                }))
        });
        let size_label = if e.max_side > 0 && e.max_side < long {
            let k = e.max_side as f64 / long as f64;
            format!("{} px · {} × {}", e.max_side, (i.width as f64 * k).round(), (i.height as f64 * k).round())
        } else {
            format!("Original · {} × {}", i.width, i.height)
        };
        let size_menu = e.size_open.then(|| {
            let mut options: Vec<(SharedString, bool, On)> = vec![(format!("Original · {} × {}", i.width, i.height).into(), !(e.max_side > 0 && e.max_side < long), set(Box::new(|e| e.max_side = 0)))];
            for x in SIDES.into_iter().filter(|x| *x < long) {
                let k = x as f64 / long as f64;
                options.push((format!("{x} px · {} × {}", (i.width as f64 * k).round(), (i.height as f64 * k).round()).into(), e.max_side == x, set(Box::new(move |e| e.max_side = x))));
            }
            mx::select_menu("img-size-list", options)
        });
        let size = mx::select("img-size", size_label, e.size_open, size_menu, Box::new(cx.listener(|f, _: &ClickEvent, _, cx| {
            if let Some(e) = f.img_edit() {
                e.size_open = !e.size_open;
            }
            cx.notify();
        })));
        let typed = e.name.read(cx).text().to_string();
        let err = mx::name_error(&typed, single);
        let ext = if !single && e.fmt == "auto" { ".jpg / .png" } else if png { ".png" } else { ".jpg" };
        let dest = (single && err.is_none()).then(|| format!("{}\\{}{ext}", util::parent(&e.paths[0]).unwrap_or_default().trim_end_matches('\\'), typed.trim()));
        let taken = dest.as_ref().is_some_and(|d| std::path::Path::new(d).exists());
        let name_hint = match err {
            Some(m) => div().mt(px(6.)).text_size(px(11.5)).text_color(t::danger()).child(m),
            None if single && taken => mx::hint("There's already a file with this name, so this one gets “ (2)” added. Nothing is replaced."),
            None if single => mx::hint("Saved next to the original, which stays as it is."),
            None => {
                let stem = util::file_name(&e.paths[e.at]);
                let stem = stem.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(stem);
                mx::hint(format!("e.g. {stem}{typed}{}", if png { ".png" } else { ".jpg" }))
            }
        };
        side.child(mx::section("Preset", None, false).child(presets))
            .child(
                mx::section("Format", None, false)
                    .child(fmt)
                    .child(mx::hint(fmt_hint))
                    .when(e.fmt == "jpeg" && i.alpha, |d| d.child(mx::warn("Transparent parts become white in JPEG."))),
            )
            .when(show_palette, |d| {
                d.child(
                    mx::section("PNG colours", None, false)
                        .child(mx::seg("img-pal", vec![("256".into(), e.palette, set(Box::new(|e| e.palette = true))), ("All".into(), !e.palette, set(Box::new(|e| e.palette = false)))], false))
                        .child(mx::hint(if e.palette {
                            "Like TinyPNG: about half the size. Transparency is kept; gradients are dithered."
                        } else {
                            "Lossless: only resizing makes it smaller (a picture with few colours still shrinks)."
                        })),
                )
            })
            .children(quality)
            .child(mx::section("Size", None, false).child(size).child(mx::hint("Longest side. Phones and most screens show 1920–2560 px.")))
            .child(mx::section(if single { "Save as" } else { "Name ending" }, None, true).child(mx::name_field(e.name.clone(), ext, err.is_some())).child(name_hint))
            .child(div().mt(px(2.)).pt(px(10.)).border_t_1().border_color(t::line()).text_size(px(11.5)).text_color(t::fg3()).child("Location, camera and other details stored in the file aren't copied."))
            .into_any_element()
    }

    fn img_quality_at(&mut self, at: Point<Pixels>, start: bool, cx: &mut Context<Self>) {
        let Some(e) = self.img_edit() else { return };
        if !start && !e.quality_drag {
            return;
        }
        e.quality_drag = true;
        let b = e.slider.get();
        let f = ((at.x - b.origin.x).as_f32() / b.size.width.as_f32().max(1.)).clamp(0., 1.);
        let q = (10. + f * 90.).round() as u32;
        if q != e.quality {
            self.img_set(|e| e.quality = q, cx);
        }
    }

    pub fn image_editor_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(Dialog::Image(e)) = self.dialog.as_ref() else { return div().into_any_element() };
        let (w, h) = mx::card_size(window);
        let n = e.paths.len();
        let title = if n > 1 { format!("Compress {n} pictures") } else { "Compress picture".to_string() };
        let sub = match &e.info {
            Some(i) => format!("{} · {} · {} × {}", util::file_name(&i.path), i.format, i.width, i.height),
            None => util::file_name(&e.paths[e.at]),
        };
        let stage: AnyElement = match (&e.info, e.out_size()) {
            (Some(_), Some(_)) => self.img_compare(e, cx),
            _ => div().flex_1().flex().flex_col().items_center().justify_center().gap(px(10.)).text_color(mx::stage_fg3()).child(ui::spinner(22., mx::stage_fg3())).child("Opening picture…").into_any_element(),
        };
        let side = self.img_side(e, cx);
        let before = e.info.as_ref().map_or(0, |i| i.size);
        let fresh = e.after_opts == Some(e.key()) && !e.loading;
        let after = e.after.as_ref().map(|a| a.1 as f64);
        let note = match e.out_size() {
            Some((ow, oh)) => format!("{}{ow} × {oh} px · exact size", if n > 1 { format!("This picture ({} of {n}) · ", e.at + 1) } else { String::new() }),
            None => String::new(),
        };
        let can = e.info.is_some() && mx::name_error(e.name.read(cx).text(), e.single()).is_none();
        let foot = mx::foot(
            mx::summary(before, after, !fresh, note),
            Box::new(cx.listener(|f, _: &ClickEvent, window, cx| f.close_dialog(window, cx))),
            can.then(|| Box::new(cx.listener(|f, _: &ClickEvent, window, cx| f.img_compress(window, cx))) as On),
            "Compress",
        );
        div()
            .id("img-scrim")
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
                    .id("img-editor")
                    .role(gpui::Role::Dialog)
                    .aria_label(title.clone())
                    .on_mouse_down(MouseButton::Left, cx.listener(|f, _, _, cx| {
                        if let Some(e) = f.img_edit() {
                            if e.size_open {
                                e.size_open = false;
                                cx.notify();
                            }
                        }
                    }))
                    .child(mx::head("image-down", title, sub, Box::new(cx.listener(|f, _: &ClickEvent, window, cx| f.close_dialog(window, cx)))))
                    .child(mx::main(stage, side))
                    .child(foot),
            )
            .into_any_element()
    }

    /// Ctrl+Enter (or Enter in the name) compresses; Esc closes an open list, then the editor.
    pub fn image_editor_key(&mut self, key: &str, ctrl: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(e) = self.img_edit() else { return false };
        match key {
            "escape" if e.size_open => {
                e.size_open = false;
                cx.notify();
                true
            }
            "enter" if ctrl || e.name.read(cx).focus_handle(cx).is_focused(window) => {
                self.img_compress(window, cx);
                true
            }
            _ => false,
        }
    }
}
