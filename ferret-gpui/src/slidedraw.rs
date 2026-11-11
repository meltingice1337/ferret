//! Slide shapes GPUI can't draw as they are: turned (`rot`), flipped pictures, and vertical text
//! (`vert`, `vert270`). GPUI has no transforms for text or pictures, so each such shape is drawn
//! whole (fill, outline, picture, text) with Direct2D and DirectWrite into a picture of its
//! turned bounding box, which the slide then places where the shape shows (as the app's CSS
//! `rotate()` and `writing-mode` do). Their text is laid over the picture as a text layer (where
//! DirectWrite put each character, turned as the shape is), so it can be selected and copied as a
//! PDF's can (selectable.rs).

use std::sync::Arc;

use ferret_core::office::{Deck, Shape};

use crate::pdftext::{PdfLine, PdfText, Turn, UPRIGHT};
use gpui::{Image, ImageFormat};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_SMALL_CAPITALS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
    DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_JUSTIFIED, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE, DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{HSTRING, Interface};
use windows_numerics::{Matrix3x2, Vector2};

/// A shape drawn into a picture, and where that picture goes on the slide (EMU: x, y, w, h); its
/// text's lines as a layer over the picture (one page, places as fractions of the picture), and
/// what follows each line when copied (a wrapped line runs on, a paragraph's last a new line).
#[derive(Clone)]
pub struct Drawn {
    pub image: Arc<Image>,
    pub rect: [f64; 4],
    pub text: Option<Arc<PdfText>>,
    pub joins: Vec<&'static str>,
}

/// A line of a shape's text as DirectWrite laid it out (in the layout's own place): its text, each
/// character's left and right, its top and bottom, and whether a paragraph ends with it.
struct Laid {
    text: String,
    chars: Vec<(usize, f32, f32)>,
    top: f32,
    bottom: f32,
    ends_para: bool,
}

/// Shapes are drawn as for a slide this many pixels wide (sharp in Quick Look, scaled down in
/// the pane).
const SLIDE_PX: f32 = 1600.;
/// Points to EMU.
const EMU_PER_PT: f32 = 12700.;

/// Whether GPUI can't draw the shape as it is.
fn needs(sh: &Shape) -> bool {
    let turned = (sh.rot.rem_euclid(360.)).min(360. - sh.rot.rem_euclid(360.)) > 0.05;
    let flipped_picture = (sh.flip_h || sh.flip_v) && sh.image.is_some();
    let vertical_text = sh.vert.is_some() && sh.paras.iter().any(|p| p.runs.iter().any(|r| !r.text.trim().is_empty()));
    sh.table.is_none() && sh.label.is_none() && (turned || flipped_picture || vertical_text)
}

/// The shapes of each slide that are drawn here (None: drawn by GPUI as usual, or drawing failed).
pub fn draw_deck(deck: &Deck) -> Vec<Vec<Option<Drawn>>> {
    if !deck.slides.iter().any(|s| s.shapes.iter().any(needs)) {
        return deck.slides.iter().map(|s| vec![None; s.shapes.len()]).collect();
    }
    let k = SLIDE_PX / deck.width.max(1) as f32;
    let Ok(tools) = Tools::new() else {
        return deck.slides.iter().map(|s| vec![None; s.shapes.len()]).collect();
    };
    deck.slides.iter().map(|s| s.shapes.iter().map(|sh| if needs(sh) { tools.draw(sh, k).ok() } else { None }).collect()).collect()
}

struct Tools {
    wic: IWICImagingFactory,
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
}

fn color(hex: &Option<String>) -> Option<D2D1_COLOR_F> {
    let v = u32::from_str_radix(hex.as_deref()?.trim_start_matches('#').get(..6)?, 16).ok()?;
    Some(D2D1_COLOR_F { r: ((v >> 16) & 255) as f32 / 255., g: ((v >> 8) & 255) as f32 / 255., b: (v & 255) as f32 / 255., a: 1. })
}

impl Tools {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            // A background thread may have no COM yet (WIC needs it).
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            Ok(Self {
                wic: CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?,
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dwrite: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
            })
        }
    }

    fn draw(&self, sh: &Shape, k: f32) -> windows::core::Result<Drawn> {
        let fail = || windows::core::Error::from(windows::Win32::Foundation::E_FAIL);
        let (w, h) = (sh.w as f32 * k, sh.h as f32 * k);
        if w < 1. || h < 1. {
            return Err(fail());
        }
        // The turned shape's bounding box, and the picture that holds it.
        let a = (sh.rot as f32).to_radians();
        let (bw, bh) = (w * a.cos().abs() + h * a.sin().abs(), w * a.sin().abs() + h * a.cos().abs());
        let (pw, ph) = ((bw.ceil() as u32 + 2).min(4096), (bh.ceil() as u32 + 2).min(4096));
        let (cx, cy) = (sh.x as f64 + sh.w as f64 / 2., sh.y as f64 + sh.h as f64 / 2.);
        let rect = [cx - pw as f64 / 2. / k as f64, cy - ph as f64 / 2. / k as f64, pw as f64 / k as f64, ph as f64 / k as f64];
        let mid = Matrix3x2::translation(pw as f32 / 2., ph as f32 / 2.);
        unsafe {
            let bitmap = self.wic.CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.,
                dpiY: 96.,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = self.d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F { r: 0., g: 0., b: 0., a: 0. }));
            // The shape itself: centred, flipped, turned, then into the picture's middle.
            let flip = Matrix3x2::scale(if sh.flip_h { -1. } else { 1. }, if sh.flip_v { -1. } else { 1. });
            rt.SetTransform(&(Matrix3x2::translation(-w / 2., -h / 2.) * flip * Matrix3x2::rotation(sh.rot as f32) * mid));
            self.shape_body(&rt, sh, w, h, k)?;
            // Its text, never mirrored; vertical text runs along the box, turned a quarter.
            let mut layer = None;
            if sh.paras.iter().any(|p| p.runs.iter().any(|r| !r.text.is_empty())) {
                let quarter = match sh.vert.as_deref() {
                    Some("vert270") => 270.,
                    Some(_) => 90.,
                    None => 0.,
                };
                let (lw, lh) = if quarter != 0. { (h, w) } else { (w, h) };
                rt.SetTransform(&(Matrix3x2::translation(-lw / 2., -lh / 2.) * Matrix3x2::rotation(sh.rot as f32 + quarter) * mid));
                let laid = self.text(&rt, sh, lw, lh, k)?;
                layer = text_layer(laid, (sh.rot as f32 + quarter).to_radians(), (lw, lh), (pw as f32, ph as f32));
            }
            rt.EndDraw(None, None)?;
            let (text, joins) = match layer {
                Some((t, j)) => (Some(Arc::new(t)), j),
                None => (None, Vec::new()),
            };
            Ok(Drawn { image: Arc::new(Image::from_bytes(ImageFormat::Png, png(&bitmap, pw, ph)?)), rect, text, joins })
        }
    }

    /// Fill, picture and outline, in the shape's own coordinates (0..w, 0..h).
    unsafe fn shape_body(&self, rt: &ID2D1RenderTarget, sh: &Shape, w: f32, h: f32, k: f32) -> windows::core::Result<()> {
        unsafe {
            let r = D2D_RECT_F { left: 0., top: 0., right: w, bottom: h };
            let ellipse = D2D1_ELLIPSE { point: Vector2::new(w / 2., h / 2.), radiusX: w / 2., radiusY: h / 2. };
            let rounded = D2D1_ROUNDED_RECT { rect: r, radiusX: w.min(h) / 6., radiusY: w.min(h) / 6. };
            if let Some(c) = color(&sh.fill) {
                let brush = rt.CreateSolidColorBrush(&c, None)?;
                match sh.geom.as_str() {
                    "ellipse" => rt.FillEllipse(&ellipse, &brush),
                    "roundRect" => rt.FillRoundedRectangle(&rounded, &brush),
                    "line" => {}
                    _ => rt.FillRectangle(&r, &brush),
                }
            }
            if let Some(path) = &sh.image {
                // A picture Windows can decode (not EMF/WMF); the shown part of it (crop).
                if let Ok(decoder) = self.wic.CreateDecoderFromFilename(&HSTRING::from(path.as_str()), None, windows::Win32::Foundation::GENERIC_READ, WICDecodeMetadataCacheOnDemand) {
                    let frame = decoder.GetFrame(0)?;
                    let converter = self.wic.CreateFormatConverter()?;
                    converter.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0., WICBitmapPaletteTypeMedianCut)?;
                    let picture = rt.CreateBitmapFromWicBitmap(&converter, None)?;
                    let size = picture.GetSize();
                    let [cl, ct, cr, cb] = sh.crop.unwrap_or([0.; 4]).map(|v| v as f32);
                    let source = D2D_RECT_F { left: size.width * cl, top: size.height * ct, right: size.width * (1. - cr), bottom: size.height * (1. - cb) };
                    rt.DrawBitmap(&picture, Some(&r), 1., D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, Some(&source));
                }
            }
            if let Some(c) = color(&sh.line) {
                let brush = rt.CreateSolidColorBrush(&c, None)?;
                let stroke = (sh.line_w as f32 * EMU_PER_PT * k).max(1.);
                match sh.geom.as_str() {
                    "ellipse" => rt.DrawEllipse(&ellipse, &brush, stroke, None),
                    "roundRect" => rt.DrawRoundedRectangle(&rounded, &brush, stroke, None),
                    "line" => rt.DrawLine(Vector2::new(0., 0.), Vector2::new(w, h), &brush, stroke, None),
                    _ => rt.DrawRectangle(&r, &brush, stroke, None),
                }
            }
            Ok(())
        }
    }

    /// The shape's paragraphs laid out in a `lw`×`lh` box (insets inside), runs styled, and drawn;
    /// where its lines and characters went (in the box).
    unsafe fn text(&self, rt: &ID2D1RenderTarget, sh: &Shape, lw: f32, lh: f32, k: f32) -> windows::core::Result<Vec<Laid>> {
        unsafe {
            let pt = |v: f64| v as f32 * EMU_PER_PT * k;
            // The text, paragraph by paragraph, and each run's range in it (UTF-16).
            let mut text: Vec<u16> = Vec::new();
            let mut runs = Vec::new();
            for (i, p) in sh.paras.iter().enumerate() {
                if i > 0 {
                    text.push('\n' as u16);
                }
                if let Some(b) = &p.bullet {
                    text.extend(format!("{b} ").encode_utf16());
                }
                for r in &p.runs {
                    // All caps: upper case; small caps: the font's own (below), as PowerPoint.
                    let s = if r.caps.as_deref() == Some("all") { r.text.to_uppercase() } else { r.text.clone() };
                    let start = text.len() as u32;
                    text.extend(s.encode_utf16());
                    runs.push((start, text.len() as u32 - start, r));
                }
            }
            let first = sh.paras.iter().flat_map(|p| p.runs.first()).next();
            let family = first.and_then(|r| r.font.clone()).unwrap_or_else(|| "Calibri".into());
            let size = pt(first.map(|r| r.size).or(sh.paras.first().map(|p| p.size)).unwrap_or(18.).max(1.)).max(1.);
            let format = self.dwrite.CreateTextFormat(
                &HSTRING::from(family.as_str()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("en-us"),
            )?;
            format.SetTextAlignment(match sh.paras.first().map(|p| p.align.as_str()) {
                Some("ctr") => DWRITE_TEXT_ALIGNMENT_CENTER,
                Some("r") => DWRITE_TEXT_ALIGNMENT_TRAILING,
                Some("just") => DWRITE_TEXT_ALIGNMENT_JUSTIFIED,
                _ => DWRITE_TEXT_ALIGNMENT_LEADING,
            })?;
            format.SetParagraphAlignment(match sh.anchor.as_str() {
                "ctr" => DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                "b" => DWRITE_PARAGRAPH_ALIGNMENT_FAR,
                _ => DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
            })?;
            let [il, it, ir, ib] = sh.inset.map(|v| v as f32 * k);
            let (bw, bh) = ((lw - il - ir).max(1.), (lh - it - ib).max(1.));
            let layout = self.dwrite.CreateTextLayout(&text, &format, bw, bh)?;
            for (start, len, r) in runs {
                let range = DWRITE_TEXT_RANGE { startPosition: start, length: len };
                if r.size > 0. {
                    layout.SetFontSize(pt(r.size).max(1.), range)?;
                }
                if r.bold {
                    layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range)?;
                }
                if r.italic {
                    layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, range)?;
                }
                if r.underline {
                    layout.SetUnderline(true, range)?;
                }
                if r.caps.as_deref() == Some("small") {
                    let typography = self.dwrite.CreateTypography()?;
                    typography.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_SMALL_CAPITALS, parameter: 1 })?;
                    layout.SetTypography(&typography, range)?;
                }
                if let Some(f) = &r.font {
                    layout.SetFontFamilyName(&HSTRING::from(f.as_str()), range)?;
                }
                if let Some(c) = color(&r.color) {
                    let brush = rt.CreateSolidColorBrush(&c, None)?;
                    layout.SetDrawingEffect(&brush.cast::<windows::core::IUnknown>()?, range)?;
                }
            }
            let ink = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.12, g: 0.12, b: 0.12, a: 1. }, None)?;
            rt.DrawTextLayout(Vector2::new(il, it), &layout, &ink, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
            Ok(laid_lines(&layout, &text, il, it).unwrap_or_default())
        }
    }
}

/// Each line of a laid-out text, and where its characters are (`(il, it)`: where it was drawn).
unsafe fn laid_lines(layout: &IDWriteTextLayout, text: &[u16], il: f32, it: f32) -> windows::core::Result<Vec<Laid>> {
    unsafe {
        let mut n = 0u32;
        // Asked with no room first: it says how many lines there are.
        let _ = layout.GetLineMetrics(None, &mut n);
        let mut metrics = vec![DWRITE_LINE_METRICS::default(); n as usize];
        layout.GetLineMetrics(Some(&mut metrics), &mut n)?;
        let mut out = Vec::new();
        let mut pos = 0u32;
        for m in metrics.iter().take(n as usize) {
            let end = (pos + m.length - m.newlineLength).min(text.len() as u32);
            let mut line = Laid { text: String::new(), chars: Vec::new(), top: f32::MAX, bottom: f32::MIN, ends_para: m.newlineLength > 0 };
            let mut i = pos;
            while i < end {
                let (mut x, mut y) = (0f32, 0f32);
                let mut hm = DWRITE_HIT_TEST_METRICS::default();
                layout.HitTestTextPosition(i, false, &mut x, &mut y, &mut hm)?;
                // A cluster (a surrogate pair, a letter and its marks) at a time.
                let len = hm.length.max(1).min(end - i);
                let s = String::from_utf16_lossy(&text[i as usize..(i + len) as usize]);
                line.chars.push((line.text.len(), il + hm.left, il + hm.left + hm.width));
                line.text.push_str(&s);
                line.top = line.top.min(it + hm.top);
                line.bottom = line.bottom.max(it + hm.top + hm.height);
                i += len;
            }
            pos += m.length;
            if !line.text.trim().is_empty() {
                out.push(line);
            }
        }
        if let Some(last) = out.last_mut() {
            last.ends_para = true;
        }
        Ok(out)
    }
}

/// A shape's laid-out lines over its picture: turned by `angle` (radians) about the middle of the
/// `box_size` they were laid out in, which sits in the middle of the `pic`-sized picture (as
/// `draw` draws them). Places become fractions of the picture (a turned line in its own frame).
fn text_layer(laid: Vec<Laid>, angle: f32, box_size: (f32, f32), pic: (f32, f32)) -> Option<(PdfText, Vec<&'static str>)> {
    if laid.is_empty() || pic.0 < 1. || pic.1 < 1. {
        return None;
    }
    let (pw, ph) = pic;
    let aspect = ph / pw;
    let angle = (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    let upright = angle.abs() < UPRIGHT;
    // A point of the layout box in the line's frame (picture widths): the box's middle is the
    // picture's, turned by `angle`.
    let (sin, cos) = angle.sin_cos();
    let (ca, cc) = (0.5 * cos + 0.5 * aspect * sin, -0.5 * sin + 0.5 * aspect * cos);
    let frame = |x: f32, y: f32| ((x - box_size.0 / 2.) / pw + ca, (y - box_size.1 / 2.) / pw + cc);
    let mut lines = Vec::new();
    let mut joins = Vec::new();
    for l in laid {
        let (start, top) = frame(l.chars.first().map(|c| c.1).unwrap_or(0.), l.top);
        let (end, bottom) = frame(l.chars.last().map(|c| c.2).unwrap_or(0.), l.bottom);
        let chars: Vec<(usize, f32, f32)> = l.chars.iter().map(|c| (c.0, frame(c.1, 0.).0, frame(c.2, 0.).0)).collect();
        let turn = Turn { angle, aspect, start, end, top, bottom };
        let line = if upright {
            // Upright (a flipped picture's text): its box as fractions of the picture.
            PdfLine { text: l.text, chars, left: start, right: end, top: top / aspect, bottom: bottom / aspect, row_goes_on: false, turn: None }
        } else {
            let (left, right, t, b) = turn.page_box();
            PdfLine { text: l.text, chars, left, right, top: t, bottom: b, row_goes_on: false, turn: Some(turn) }
        };
        lines.push(line);
        joins.push(if l.ends_para { "\n" } else { "" });
    }
    Some((PdfText { pages: vec![lines] }, joins))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quarter_turned_shapes_text_runs_down_its_picture() {
        // A 100×20 box turned 90° in a 22×102 picture: "ab" across the box's top, 10 px a letter.
        let laid = vec![Laid { text: "ab".into(), chars: vec![(0, 0., 10.), (1, 10., 20.)], top: 0., bottom: 10., ends_para: true }];
        let (text, joins) = text_layer(laid, std::f32::consts::FRAC_PI_2, (100., 20.), (22., 102.)).unwrap();
        assert_eq!(joins, ["\n"]);
        let l = &text.pages[0][0];
        let t = l.turn.expect("turned");
        // The box's top left goes to the picture's top right, its top edge running down.
        let (x, y) = t.to_page(l.chars[0].1, t.top);
        assert!((x * 22. - 21.).abs() < 0.01 && (y * 102. - 1.).abs() < 0.01, "{x} {y}");
        let (x, y) = t.to_page(l.chars[1].2, t.top);
        assert!((x * 22. - 21.).abs() < 0.01 && (y * 102. - 21.).abs() < 0.01, "{x} {y}");
        // And across: 10 px to the left.
        let (x, _) = t.to_page(l.chars[0].1, t.bottom);
        assert!((x * 22. - 11.).abs() < 0.01, "{x}");
    }
}

/// The drawn picture (premultiplied BGRA) as a PNG.
unsafe fn png(bitmap: &IWICBitmap, w: u32, h: u32) -> windows::core::Result<Vec<u8>> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    unsafe { bitmap.CopyPixels(std::ptr::null(), w * 4, &mut px)? };
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        let (b, g, r) = (p[0] as u32, p[1] as u32, p[2] as u32);
        let un = |c: u32| if a == 0 { 0 } else { ((c * 255 + a / 2) / a).min(255) as u8 };
        p[0] = un(r);
        p[1] = un(g);
        p[2] = un(b);
    }
    let image = image::RgbaImage::from_raw(w, h, px).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    let mut out = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    Ok(out)
}
