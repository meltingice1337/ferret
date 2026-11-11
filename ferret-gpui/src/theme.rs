//! Ferret's themes, from src/styles.css: dark and light, any accent, and the window backdrop.
//! CSS mixes accent colors in oklab; mixing in sRGB here is close enough. Set at runtime (Settings
//! changes them); every color is read when drawn.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use gpui::{Hsla, Rgba};

/// The app's default accent (`--accent`).
pub const DEFAULT_ACCENT: u32 = 0xe2457f;

pub const FONT: &str = "Segoe UI Variable Text";
pub const MONO: &str = "Cascadia Mono";

static ACCENT: AtomicU32 = AtomicU32::new(DEFAULT_ACCENT);
static DARK: AtomicBool = AtomicBool::new(true);
/// 0 Mica, 1 Acrylic, 2 Solid.
static BACKDROP: AtomicU8 = AtomicU8::new(0);
static COMPACT: AtomicBool = AtomicBool::new(false);
static ANIMATE: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backdrop {
    Mica,
    Acrylic,
    Solid,
}

pub fn set(accent: u32, dark: bool, backdrop: Backdrop, compact: bool, animate: bool) {
    ACCENT.store(accent, Ordering::Relaxed);
    DARK.store(dark, Ordering::Relaxed);
    BACKDROP.store(backdrop as u8, Ordering::Relaxed);
    COMPACT.store(compact, Ordering::Relaxed);
    ANIMATE.store(animate, Ordering::Relaxed);
}

pub fn accent_hex() -> u32 { ACCENT.load(Ordering::Relaxed) }
pub fn is_dark() -> bool { DARK.load(Ordering::Relaxed) }
pub fn backdrop() -> Backdrop {
    match BACKDROP.load(Ordering::Relaxed) {
        1 => Backdrop::Acrylic,
        2 => Backdrop::Solid,
        _ => Backdrop::Mica,
    }
}
pub fn compact() -> bool { COMPACT.load(Ordering::Relaxed) }
/// Settings → Animations (off also when Windows' animation effects are off).
pub fn animate() -> bool { ANIMATE.load(Ordering::Relaxed) }

/// A list row's height: Comfortable 30px, Compact 24px.
pub fn row_h() -> f32 { if compact() { 24. } else { 30. } }
pub fn row_text() -> f32 { if compact() { 12.5 } else { 13. } }

pub fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok(),
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            Some((r * 17) << 16 | (g * 17) << 8 | b * 17)
        }
        _ => None,
    }
}

pub fn rgba(hex: u32, a: f32) -> Hsla {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.,
        g: ((hex >> 8) & 0xff) as f32 / 255.,
        b: (hex & 0xff) as f32 / 255.,
        a,
    }
    .into()
}

/// `color-mix(in srgb, a share, b)`.
pub fn mix(a: u32, b: u32, share: f32) -> Hsla {
    let ch = |hex: u32, s: u32| ((hex >> s) & 0xff) as f32 / 255.;
    let m = |s| ch(a, s) * share + ch(b, s) * (1. - share);
    Rgba { r: m(16), g: m(8), b: m(0), a: 1. }.into()
}

fn lum(hex: u32) -> f32 {
    let ch = |s: u32| {
        let c = ((hex >> s) & 0xff) as f32 / 255.;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// Whichever of black and white contrasts more with the accent (format.ts `onColor`).
pub fn on_accent() -> Hsla {
    let l = lum(accent_hex());
    let white = 1.05 / (l + 0.05);
    let black = (l + 0.05) / 0.05;
    if white >= black { rgba(0xffffff, 1.) } else { rgba(0x111111, 1.) }
}

pub fn accent() -> Hsla { rgba(accent_hex(), 1.) }
pub fn accent_a(a: f32) -> Hsla { rgba(accent_hex(), a) }
pub fn accent_soft() -> Hsla { rgba(accent_hex(), 0.20) }
pub fn accent_softer() -> Hsla { rgba(accent_hex(), 0.11) }
pub fn accent_line() -> Hsla { rgba(accent_hex(), 0.55) }
/// Selected text in previews (the app's `::selection`, a little stronger so it shows on white pages).
pub fn selection() -> Hsla { rgba(accent_hex(), if is_dark() { 0.32 } else { 0.28 }) }
pub fn accent_text() -> Hsla {
    if is_dark() { mix(accent_hex(), 0xffffff, 0.8) } else { mix(accent_hex(), 0x000000, 0.58) }
}

fn ink() -> u32 { if is_dark() { 0xf4eef3 } else { 0x23171f } }
pub fn fg() -> Hsla { rgba(ink(), 1.) }
pub fn fg2() -> Hsla { rgba(ink(), if is_dark() { 0.72 } else { 0.76 }) }
pub fn fg3() -> Hsla { rgba(ink(), if is_dark() { 0.55 } else { 0.64 }) }

pub fn panel() -> Hsla {
    match (backdrop(), is_dark()) {
        (Backdrop::Solid, true) => rgba(0x1c131c, 1.),
        (Backdrop::Solid, false) => rgba(0xfbf7fa, 1.),
        (_, true) => rgba(0x160e16, 0.7),
        (_, false) => rgba(0xffffff, 0.7),
    }
}
/// A panel that must hide what's under it (`--panel-solid`): the sidebar sliding over the files.
pub fn panel_solid() -> Hsla {
    if is_dark() { rgba(0x1c131c, 1.) } else { rgba(0xfbf7fa, 1.) }
}
/// The panel as it shows over `panel_solid` (the sidebar overlay), as one opaque colour.
pub fn panel_over_solid() -> Hsla {
    let (p, s): (Rgba, Rgba) = (panel().into(), panel_solid().into());
    let mix = |a: f32, b: f32| a * p.a + b * (1. - p.a);
    Rgba { r: mix(p.r, s.r), g: mix(p.g, s.g), b: mix(p.b, s.b), a: 1. }.into()
}
/// Behind the panels: the app's glass base (`--glass-base`) over Mica or Acrylic, opaque when
/// solid. (Leaving Mica bare showed it greyer than the app, the light theme most.)
pub fn window_bg() -> Hsla {
    match (backdrop(), is_dark()) {
        (Backdrop::Mica | Backdrop::Acrylic, true) => rgba(0x0c080d, 0.62),
        (Backdrop::Mica | Backdrop::Acrylic, false) => rgba(0xf6f0f5, 0.55),
        (Backdrop::Solid, true) => rgba(0x120b12, 1.),
        (Backdrop::Solid, false) => rgba(0xefe6ec, 1.),
    }
}
/// Menus, dialogs, cards (`--raised`).
pub fn raised() -> Hsla { if is_dark() { rgba(0x2e202e, 0.97) } else { rgba(0xffffff, 0.98) } }
pub fn scrim() -> Hsla { if is_dark() { rgba(0x000000, 0.42) } else { rgba(0x2a1424, 0.22) } }
pub fn line() -> Hsla { if is_dark() { rgba(0xffffff, 0.07) } else { rgba(0x1e0a19, 0.08) } }
pub fn line2() -> Hsla { if is_dark() { rgba(0xffffff, 0.12) } else { rgba(0x1e0a19, 0.14) } }
pub fn hover() -> Hsla { if is_dark() { rgba(0xffffff, 0.055) } else { rgba(0x1e0a19, 0.05) } }
pub fn press() -> Hsla { if is_dark() { rgba(0xffffff, 0.09) } else { rgba(0x1e0a19, 0.09) } }
pub fn shadow_color() -> Hsla { if is_dark() { rgba(0, 0.5) } else { rgba(0x3c1432, 0.22) } }

pub fn warn() -> Hsla { if is_dark() { rgba(0xf59e0b, 1.) } else { rgba(0xb45309, 1.) } }
pub fn danger() -> Hsla { if is_dark() { rgba(0xf87171, 1.) } else { rgba(0xb91c1c, 1.) } }
pub fn ok() -> Hsla { if is_dark() { rgba(0x34d399, 1.) } else { rgba(0x047857, 1.) } }

/// Folder icons: the accent, lightened (`.ficon.k-folder`).
pub fn folder() -> Hsla {
    if is_dark() { mix(accent_hex(), 0xffd2e4, 0.7) } else { mix(accent_hex(), 0x000000, 0.85) }
}

/// A drop shadow like `--shadow`.
pub fn shadow() -> Vec<gpui::BoxShadow> {
    use gpui::{point, px};
    vec![
        gpui::BoxShadow { color: shadow_color(), offset: point(px(0.), px(18.)), blur_radius: px(50.), spread_radius: px(0.), inset: false },
        gpui::BoxShadow { color: rgba(0, if is_dark() { 0.35 } else { 0.12 }), offset: point(px(0.), px(2.)), blur_radius: px(8.), spread_radius: px(0.), inset: false },
    ]
}

/// The accent glow's strength at a point of the window (0..1 across and down).
fn glow_alpha(u: f32, v: f32) -> f32 {
    let (ta, tb) = if is_dark() { (0.30, 0.14) } else { (0.22, 0.10) };
    let d1 = (((u - 1.) / 1.2).powi(2) + (v / 0.9).powi(2)).sqrt();
    let a1 = ta * (1. - d1 / 0.6).max(0.);
    let d2 = ((u / 0.9).powi(2) + ((v - 1.) / 0.8).powi(2)).sqrt();
    let a2 = tb * (1. - d2 / 0.7).max(0.);
    a1 + a2 * (1. - a1)
}

/// What a panel looks like at a point of the window, as one opaque color: the panel over the
/// glow over the backdrop (Mica's own tone assumed). For fades into a panel (GPUI has no masks).
pub fn panel_seen(u: f32, v: f32) -> Hsla {
    let over = |top: [f32; 3], a: f32, under: [f32; 3]| [0, 1, 2].map(|i| top[i] * a + under[i] * (1. - a));
    let rgb = |c: u32| [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32];
    let dark = is_dark();
    let mut c = match backdrop() {
        Backdrop::Solid => rgb(if dark { 0x120b12 } else { 0xefe6ec }),
        // Mica (or Acrylic) under the app's glass base.
        _ => over(rgb(if dark { 0x0c080d } else { 0xf6f0f5 }), if dark { 0.62 } else { 0.55 }, rgb(if dark { 0x202020 } else { 0xf3f3f3 })),
    };
    c = over(rgb(accent_hex()), glow_alpha(u, v), c);
    let p = panel();
    let pr: Rgba = p.into();
    c = over([pr.r * 255., pr.g * 255., pr.b * 255.], pr.a, c);
    rgba(((c[0].round() as u32) << 16) | ((c[1].round() as u32) << 8) | c[2].round() as u32, 1.)
}

/// The two radial glows behind the panels (`.app` background), as straight-alpha RGBA pixels of
/// the accent. Drawn small and stretched over the window: both gradients are smooth.
pub fn glow_png(w: u32, h: u32) -> Vec<u8> {
    let accent = accent_hex();
    let (r, g, b) = ((accent >> 16) as u8, (accent >> 8) as u8, accent as u8);
    let (ta, tb) = if is_dark() { (0.30, 0.14) } else { (0.22, 0.10) };
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let u = (x as f32 + 0.5) / w as f32;
        let v = (y as f32 + 0.5) / h as f32;
        // radial-gradient(120% 90% at 100% 0%, accent 30%, transparent 60%)
        let d1 = (((u - 1.) / 1.2).powi(2) + (v / 0.9).powi(2)).sqrt();
        let a1 = ta * (1. - d1 / 0.6).max(0.);
        // radial-gradient(90% 80% at 0% 100%, accent 14%, transparent 70%)
        let d2 = ((u / 0.9).powi(2) + ((v - 1.) / 0.8).powi(2)).sqrt();
        let a2 = tb * (1. - d2 / 0.7).max(0.);
        let a = a1 + a2 * (1. - a1);
        *px = image::Rgba([r, g, b, (a * 255.).round() as u8]);
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encoding the glow");
    out
}
