//! Text in scanned PDFs: a page with no text of its own (a picture of a page, as scanners make)
//! is read with text recognition (OCR), and what's read is laid over the page as the PDF's own
//! text would be (pdftext.rs), so it can be selected, copied and found with Ctrl+F.
//!
//! Two readers:
//! - Windows' own (Windows.Media.Ocr), always there. Its languages are the ones Windows has
//!   installed; a page is read in a Latin-script language other than English first when there is
//!   one (English drops every diacritic: "ș" reads "s"), and again in English if nothing in it
//!   needed that language. A page it can only read as gibberish is tried turned (a page scanned
//!   sideways).
//! - PaddleOCR (PP-OCRv5 mobile, run by Microsoft's ONNX Runtime), when the add-on is installed
//!   (addons.rs): better with tables, faxes, poor scans and unusual fonts, and it finds which way
//!   up a page is by itself.
//!
//! Both read a page drawn at 200 dpi. Lines come back in reading order (straightened first when
//! the scan is tilted: text columns one after the other, a table row by row), each character
//! where it is on the page. Measured on test scans with known text: Windows ~0.05 s a page,
//! PaddleOCR ~0.25 s (the comparison: research/PARITY.md "Text in scanned PDFs", local notes).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{Buffer, DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::core::HSTRING;

use crate::pdftext::{PdfLine, Turn, UPRIGHT};

/// Which reader read a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reader {
    Windows,
    Paddle,
}

/// Pages are drawn at this many dots per inch to be read (more doesn't read better: measured).
const DPI: f32 = 200.;

/// A page drawn for reading: BGRA, `w` × `h`.
struct Picture {
    w: usize,
    h: usize,
    bgra: Vec<u8>,
    /// The decoder's own bitmap, read by Windows as it is (one rebuilt from the bytes read a word
    /// differently in testing).
    bmp: Option<SoftwareBitmap>,
}

fn err(e: windows::core::Error) -> String {
    e.message().to_string()
}

fn draw(doc: &PdfDocument, i: u32) -> Result<Picture, String> {
    let page = doc.GetPage(i).map_err(err)?;
    let size = page.Size().map_err(err)?;
    // Page sizes are in 1/96 inch; the longest side kept within 4000 px.
    let k = (DPI / 96.).min(4000. / size.Width.max(size.Height).max(1.));
    let (w, h) = ((size.Width * k).round().max(1.) as u32, (size.Height * k).round().max(1.) as u32);
    let opts = PdfPageRenderOptions::new().map_err(err)?;
    opts.SetDestinationWidth(w).map_err(err)?;
    opts.SetDestinationHeight(h).map_err(err)?;
    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(err)?.join().map_err(err)?;
    stream.Seek(0).map_err(err)?;
    let decoder = BitmapDecoder::CreateAsync(&stream).map_err(err)?.join().map_err(err)?;
    let bmp = decoder.GetSoftwareBitmapConvertedAsync(BitmapPixelFormat::Bgra8, BitmapAlphaMode::Premultiplied).map_err(err)?.join().map_err(err)?;
    let (bw, bh) = (bmp.PixelWidth().map_err(err)? as usize, bmp.PixelHeight().map_err(err)? as usize);
    let buf = Buffer::Create((bw * bh * 4) as u32).map_err(err)?;
    bmp.CopyToBuffer(&buf).map_err(err)?;
    let reader = DataReader::FromBuffer(&buf).map_err(err)?;
    let mut bgra = vec![0u8; buf.Length().map_err(err)? as usize];
    reader.ReadBytes(&mut bgra).map_err(err)?;
    Ok(Picture { w: bw, h: bh, bgra, bmp: Some(bmp) })
}

impl Picture {
    /// Turned `q` quarter turns clockwise.
    fn turned(&self, q: u32) -> Picture {
        let (mut w, mut h, mut src) = (self.w, self.h, self.bgra.clone());
        for _ in 0..q % 4 {
            let mut out = vec![0u8; src.len()];
            for y in 0..h {
                for x in 0..w {
                    let (nx, ny) = (h - 1 - y, x);
                    let (si, di) = ((y * w + x) * 4, (ny * h + nx) * 4);
                    out[di..di + 4].copy_from_slice(&src[si..si + 4]);
                }
            }
            src = out;
            std::mem::swap(&mut w, &mut h);
        }
        Picture { w, h, bgra: src, bmp: None }
    }

    fn bitmap(&self) -> Result<SoftwareBitmap, String> {
        if let Some(b) = &self.bmp {
            return Ok(b.clone());
        }
        let bmp = SoftwareBitmap::CreateWithAlpha(BitmapPixelFormat::Bgra8, self.w as i32, self.h as i32, BitmapAlphaMode::Premultiplied).map_err(err)?;
        let writer = DataWriter::new().map_err(err)?;
        writer.WriteBytes(&self.bgra).map_err(err)?;
        bmp.CopyFromBuffer(&writer.DetachBuffer().map_err(err)?).map_err(err)?;
        Ok(bmp)
    }
}

// ───────────────────────────── what's found ─────────────────────────────

/// A line (or a piece of one: a table's cell) as read, in the pixels of the picture read: its
/// middle, its direction (radians, clockwise as shown), its half length and half height, and each
/// character with where it starts and ends along it (from its middle).
#[derive(Clone, Debug)]
struct Found {
    cx: f32,
    cy: f32,
    angle: f32,
    hw: f32,
    hh: f32,
    chars: Vec<(String, f32, f32)>,
}

impl Found {
    fn text(&self) -> String {
        self.chars.iter().map(|c| c.0.as_str()).collect()
    }

    /// Back on the page as shown, from a picture of it turned `q` quarter turns clockwise (`w` × `h`:
    /// the page's picture, unturned).
    fn unturned(mut self, q: u32, w: f32, h: f32) -> Found {
        let (x, y) = (self.cx, self.cy);
        (self.cx, self.cy) = match q % 4 {
            1 => (y, h - x),
            2 => (w - x, h - y),
            3 => (w - y, x),
            _ => (x, y),
        };
        self.angle = wrap(self.angle - q as f32 * std::f32::consts::FRAC_PI_2);
        self
    }
}

/// An angle in (-π, π].
fn wrap(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    let a = a.rem_euclid(t);
    if a > std::f32::consts::PI { a - t } else { a }
}

// ───────────────────────────── Windows' reader ─────────────────────────────

/// Latin-script languages: read in one of these, an English page loses nothing but a page in it
/// keeps its accents.
const LATIN: &[&str] = &[
    "ro", "de", "fr", "es", "it", "pt", "pl", "cs", "sk", "hu", "nl", "sv", "da", "nb", "nn", "no", "fi", "tr", "hr", "sl", "et", "lv", "lt", "ca", "gl", "eu",
    "is", "ga", "sq", "bs", "sr-latn", "ms", "id", "vi", "af",
];

fn tag_of(e: &OcrEngine) -> String {
    e.RecognizerLanguage().and_then(|l| l.LanguageTag()).map(|t| t.to_string().to_lowercase()).unwrap_or_default()
}

fn english(tag: &str) -> bool {
    tag.starts_with("en")
}

/// The engines to try, in order (see the module's note): the language Windows reads for this
/// user, an accented Latin language first when that's English, English last.
fn windows_engines() -> Vec<OcrEngine> {
    let mut out: Vec<OcrEngine> = Vec::new();
    let Ok(primary) = OcrEngine::TryCreateFromUserProfileLanguages() else {
        return out;
    };
    let ptag = tag_of(&primary);
    let mut available: Vec<String> = Vec::new();
    if let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() {
        for l in langs {
            if let Ok(t) = l.LanguageTag() {
                available.push(t.to_string());
            }
        }
    }
    let make = |tag: &str| Language::CreateLanguage(&HSTRING::from(tag)).and_then(|l| OcrEngine::TryCreateFromLanguage(&l)).ok();
    let latin = |t: &str| {
        let t = t.to_lowercase();
        LATIN.iter().any(|l| t == *l || t.starts_with(&format!("{l}-")))
    };
    if english(&ptag) {
        if let Some(e) = available.iter().find(|t| latin(t)).and_then(|t| make(t)) {
            out.push(e);
        }
        out.push(primary);
    } else {
        out.push(primary);
        if let Some(e) = available.iter().find(|t| english(&t.to_lowercase())).and_then(|t| make(t)) {
            out.push(e);
        }
    }
    out
}

/// Letters outside plain ASCII, per word: whether a page read in an accented language needed it.
fn accented(t: &str) -> bool {
    let n = t.chars().filter(|c| c.is_alphabetic() && !c.is_ascii()).count();
    n * 50 >= t.split_whitespace().count().max(1)
}

/// (word-like tokens, all tokens): letters or digits, two or more, a vowel or a digit among them.
fn wordlike(t: &str) -> (usize, usize) {
    let mut good = 0;
    let mut total = 0;
    for w in t.split_whitespace() {
        total += 1;
        let core = w.trim_matches(|c: char| !c.is_alphanumeric());
        if core.chars().count() >= 2
            && core.chars().all(|c| c.is_alphanumeric() || "-'./@,".contains(c))
            && core.chars().any(|c| c.is_ascii_digit() || "aeiouyAEIOUYăâîéèêàáíóúäöü".contains(c))
        {
            good += 1;
        }
    }
    (good, total)
}

/// One engine's lines. Windows reads a tilted page straightened: its words' boxes are where they
/// would be with the page turned upright about its middle (by `TextAngle`, clockwise), so they're
/// turned back onto the page here.
fn windows_read(engine: &OcrEngine, pic: &Picture) -> Result<Vec<Found>, String> {
    let res = engine.RecognizeAsync(&pic.bitmap()?).map_err(err)?.join().map_err(err)?;
    let angle = res.TextAngle().and_then(|a| a.Value()).map(|d| (d as f32).to_radians()).unwrap_or(0.);
    let (sin, cos) = angle.sin_cos();
    let (mx, my) = (pic.w as f32 / 2., pic.h as f32 / 2.);
    // Each line's words in the page's frame (along the text, across it): (along, across, length,
    // height, text).
    let mut lines: Vec<Vec<(f32, f32, f32, f32, String)>> = Vec::new();
    for l in res.Lines().map_err(err)? {
        let mut words = Vec::new();
        for w in l.Words().map_err(err)? {
            let r = w.BoundingRect().map_err(err)?;
            let (ux, uy) = (r.X + r.Width / 2. - mx, r.Y + r.Height / 2. - my);
            // on the page: turned by the text's angle about the middle
            let (x, y) = (mx + ux * cos - uy * sin, my + ux * sin + uy * cos);
            words.push((x * cos + y * sin, -x * sin + y * cos, r.Width, r.Height, w.Text().map_err(err)?.to_string()));
        }
        if !words.is_empty() {
            lines.push(words);
        }
    }
    let mut out = Vec::new();
    for ws in lines {
        let start = ws.iter().map(|w| w.0 - w.2 / 2.).fold(f32::MAX, f32::min);
        let end = ws.iter().map(|w| w.0 + w.2 / 2.).fold(f32::MIN, f32::max);
        let across = ws.iter().map(|w| w.1).sum::<f32>() / ws.len() as f32;
        let hh = ws.iter().map(|w| w.3).fold(0f32, f32::max) / 2.;
        let mid = (start + end) / 2.;
        let mut chars = Vec::new();
        let mut prev_end: Option<f32> = None;
        for (along, _, len, _, text) in &ws {
            let (a0, a1) = (along - len / 2. - mid, along + len / 2. - mid);
            if let Some(pe) = prev_end {
                chars.push((" ".to_string(), pe, a0.max(pe)));
            }
            let n = text.chars().count().max(1) as f32;
            for (i, ch) in text.chars().enumerate() {
                let k0 = a0 + (a1 - a0) * i as f32 / n;
                chars.push((ch.to_string(), k0, k0 + (a1 - a0) / n));
            }
            prev_end = Some(a1);
        }
        out.push(Found { cx: mid * cos - across * sin, cy: mid * sin + across * cos, angle, hw: (end - start) / 2., hh, chars });
    }
    Ok(out)
}

/// A page read by Windows: the language that suits it, turned if it reads as gibberish upright.
fn windows_page(engines: &[OcrEngine], pic: &Picture) -> Result<Vec<Found>, String> {
    let read = |p: &Picture| -> Result<Vec<Found>, String> {
        let mut last = Vec::new();
        for (i, e) in engines.iter().enumerate() {
            let found = windows_read(e, p)?;
            let text: String = found.iter().map(|f| f.text() + "\n").collect();
            // An accented language is kept if the page needed it; English (or the last) as it is.
            if english(&tag_of(e)) || i + 1 == engines.len() || accented(&text) {
                return Ok(found);
            }
            last = found;
        }
        Ok(last)
    };
    let found = read(pic)?;
    let text: String = found.iter().map(|f| f.text() + "\n").collect();
    let (good, total) = wordlike(&text);
    if total >= 5 && good * 10 >= total * 7 {
        return Ok(found);
    }
    // Gibberish or nothing: maybe it's sideways or upside down.
    let mut best = (good, found);
    for q in [1u32, 3, 2] {
        let turned = pic.turned(q);
        let f = read(&turned)?;
        let g = wordlike(&f.iter().map(|f| f.text() + "\n").collect::<String>()).0;
        if g > best.0 {
            best = (g, f.into_iter().map(|f| f.unturned(q, pic.w as f32, pic.h as f32)).collect());
        }
    }
    Ok(best.1)
}

// ───────────────────────────── PaddleOCR ─────────────────────────────

mod paddle {
    use super::{Found, Picture};
    use ort::session::Session;
    use ort::value::Tensor;
    use std::path::Path;

    pub struct Paddle {
        det: Session,
        rec: Session,
        ori: Session,
        dict: Vec<String>,
    }

    fn s<E: std::fmt::Display>(e: E) -> String {
        e.to_string()
    }

    /// The text detector looks at the page this big (its longest side): measured as the best
    /// trade (at 960 a phone photo lost words; above 1280, only time).
    const DET_LONG: usize = 1280;
    /// Lines recognised two at a time: less padding to waste than more (measured faster, and
    /// less memory).
    const REC_BATCH: usize = 2;

    /// Its character list (the recognizer's inference.yml: `character_dict`).
    fn dict(yml: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut on = false;
        for line in yml.lines() {
            if line.trim_end() == "  character_dict:" {
                on = true;
                continue;
            }
            if !on {
                continue;
            }
            let Some(v) = line.strip_prefix("  - ") else { break };
            out.push(if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') { v[1..v.len() - 1].replace("''", "'") } else { v.to_string() });
        }
        out
    }

    impl Paddle {
        pub fn load(env: &ort::environment::Environment, dir: &Path) -> Result<Paddle, String> {
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(2).clamp(1, 4);
            let open = |name: &str| -> Result<Session, String> {
                Session::builder(env)
                    .map_err(s)?
                    .with_intra_threads(threads)
                    .map_err(s)?
                    // pages and lines differ in size every time: no per-size memory plan
                    .with_memory_pattern(false)
                    .map_err(s)?
                    .commit_from_file(dir.join(name).join("inference.onnx"))
                    .map_err(s)
            };
            let dict = dict(&std::fs::read_to_string(dir.join("rec").join("inference.yml")).map_err(s)?);
            if dict.len() < 50 {
                return Err("the recognizer's character list is missing".into());
            }
            Ok(Paddle { det: open("det")?, rec: open("rec")?, ori: open("ori")?, dict })
        }

        /// Area-average shrink to (dw, dh), as planar BGR 0–255.
        fn shrink(p: &Picture, dw: usize, dh: usize) -> Vec<f32> {
            let mut out = vec![0f32; 3 * dw * dh];
            let (sx, sy) = (p.w as f32 / dw as f32, p.h as f32 / dh as f32);
            for y in 0..dh {
                let y0 = (y as f32 * sy) as usize;
                let y1 = (((y + 1) as f32 * sy) as usize).max(y0 + 1).min(p.h);
                for x in 0..dw {
                    let x0 = (x as f32 * sx) as usize;
                    let x1 = (((x + 1) as f32 * sx) as usize).max(x0 + 1).min(p.w);
                    let mut acc = [0u32; 3];
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let i = (yy * p.w + xx) * 4;
                            acc[0] += p.bgra[i] as u32;
                            acc[1] += p.bgra[i + 1] as u32;
                            acc[2] += p.bgra[i + 2] as u32;
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)).max(1) as f32;
                    for c in 0..3 {
                        out[c * dw * dh + y * dw + x] = acc[c] as f32 / n;
                    }
                }
            }
            out
        }

        /// Quarter turns clockwise that put the page upright (the orientation classifier).
        pub fn upright(&mut self, p: &Picture) -> Result<u32, String> {
            let sc = 256. / p.w.min(p.h).max(1) as f32;
            let (rw, rh) = (((p.w as f32 * sc).round() as usize).max(224), ((p.h as f32 * sc).round() as usize).max(224));
            let small = Self::shrink(p, rw, rh);
            let (ox, oy) = ((rw - 224) / 2, (rh - 224) / 2);
            let (mean, std) = ([0.485f32, 0.456, 0.406], [0.229f32, 0.224, 0.225]);
            let mut x = vec![0f32; 3 * 224 * 224];
            for c in 0..3 {
                for y in 0..224 {
                    for xx in 0..224 {
                        let v = small[c * rw * rh + (y + oy) * rw + xx + ox] / 255.;
                        x[c * 224 * 224 + y * 224 + xx] = (v - mean[c]) / std[c];
                    }
                }
            }
            let t = Tensor::from_array(([1usize, 3, 224, 224], x)).map_err(s)?;
            let out = self.ori.run(ort::inputs![t]).map_err(s)?;
            let (_, probs) = out[0].try_extract_tensor::<f32>().map_err(s)?;
            let best = (0..probs.len()).max_by(|&a, &b| probs[a].total_cmp(&probs[b])).unwrap_or(0);
            // label k ("0", "90", "180", "270"): turning the picture (4 - k) quarter turns clockwise
            // puts it upright (measured: a page scanned a quarter turn clockwise reads "90")
            Ok((4 - best as u32) % 4)
        }

        /// Text boxes: (centre x, centre y, unit vector along the text, half length, half height).
        fn detect(&mut self, p: &Picture) -> Result<Vec<Rect>, String> {
            let long = p.w.max(p.h);
            let sc = DET_LONG.min(long) as f32 / long as f32;
            let dw = ((((p.w as f32 * sc) / 32.).round() as usize) * 32).max(32);
            let dh = ((((p.h as f32 * sc) / 32.).round() as usize) * 32).max(32);
            let mut x = Self::shrink(p, dw, dh);
            let (mean, std) = ([0.485f32, 0.456, 0.406], [0.229f32, 0.224, 0.225]);
            for c in 0..3 {
                for v in &mut x[c * dw * dh..(c + 1) * dw * dh] {
                    *v = (*v / 255. - mean[c]) / std[c];
                }
            }
            let t = Tensor::from_array(([1usize, 3, dh, dw], x)).map_err(s)?;
            let out = self.det.run(ort::inputs![t]).map_err(s)?;
            let (shape, prob) = out[0].try_extract_tensor::<f32>().map_err(s)?;
            let (mh, mw) = (shape[2] as usize, shape[3] as usize);
            let prob = prob.to_vec();
            drop(out);
            Ok(boxes(&prob, mw, mh, p.w as f32 / mw as f32, p.h as f32 / mh as f32))
        }

        /// Each box read: its characters, each with the step (of `steps`) it was read at.
        fn recognise(&mut self, p: &Picture, rects: &[Rect]) -> Result<Vec<Option<Vec<(String, f32)>>>, String> {
            struct Crop {
                i: usize,
                w: usize,
                data: Vec<f32>,
            }
            let mut crops = Vec::new();
            for (i, r) in rects.iter().enumerate() {
                let (ux, uy, hw, hh) = (r.ux, r.uy, r.hw, r.hh);
                let (vx, vy) = (-uy, ux);
                let cw = ((48. * hw / hh).round() as usize).clamp(4, 3200);
                let mut data = vec![0f32; 3 * 48 * cw];
                for yy in 0..48 {
                    for xx in 0..cw {
                        let mut acc = [0f32; 3];
                        for (sx, sy) in [(0.25f32, 0.25f32), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                            let u = ((xx as f32 + sx) / cw as f32 * 2. - 1.) * hw;
                            let v = ((yy as f32 + sy) / 48. * 2. - 1.) * hh;
                            let (px, py) = (r.cx + u * ux + v * vx, r.cy + u * uy + v * vy);
                            for (c, a) in acc.iter_mut().enumerate() {
                                *a += sample(p, px, py, c);
                            }
                        }
                        for c in 0..3 {
                            data[c * 48 * cw + yy * cw + xx] = (acc[c] / 4. / 255. - 0.5) / 0.5;
                        }
                    }
                }
                crops.push(Crop { i, w: cw, data });
            }
            crops.sort_by_key(|c| c.w);
            let mut out: Vec<Option<Vec<(String, f32)>>> = vec![None; rects.len()];
            for batch in crops.chunks(REC_BATCH) {
                let bw = batch.iter().map(|c| c.w).max().unwrap_or(4).max(320);
                let n = batch.len();
                let mut x = vec![0f32; n * 3 * 48 * bw];
                for (b, c) in batch.iter().enumerate() {
                    for ch in 0..3 {
                        for yy in 0..48 {
                            let src = &c.data[ch * 48 * c.w + yy * c.w..ch * 48 * c.w + yy * c.w + c.w];
                            let d0 = b * 3 * 48 * bw + ch * 48 * bw + yy * bw;
                            x[d0..d0 + c.w].copy_from_slice(src);
                        }
                    }
                }
                let t = Tensor::from_array(([n, 3usize, 48, bw], x)).map_err(s)?;
                let res = self.rec.run(ort::inputs![t]).map_err(s)?;
                let (shape, probs) = res[0].try_extract_tensor::<f32>().map_err(s)?;
                let (steps, classes) = (shape[1] as usize, shape[2] as usize);
                for (b, c) in batch.iter().enumerate() {
                    let mut chars = Vec::new();
                    let mut last = 0usize;
                    for t in 0..steps {
                        let row = &probs[(b * steps + t) * classes..(b * steps + t + 1) * classes];
                        let (mut bi, mut bp) = (0usize, f32::MIN);
                        for (j, &v) in row.iter().enumerate() {
                            if v > bp {
                                bp = v;
                                bi = j;
                            }
                        }
                        if bi != 0 && bi != last {
                            let ch = self.dict.get(bi - 1).cloned().unwrap_or_else(|| " ".into());
                            // where along the crop (0–1 of its own width) this step looked
                            let at = (t as f32 + 0.5) / steps as f32 * bw as f32 / c.w as f32;
                            chars.push((ch, at));
                        }
                        last = bi;
                    }
                    out[c.i] = Some(chars);
                }
            }
            Ok(out)
        }

        /// A page's text pieces (read upright; the caller turns the page first).
        pub fn read(&mut self, p: &Picture) -> Result<Vec<Found>, String> {
            let rects = self.detect(p)?;
            let read = self.recognise(p, &rects)?;
            let mut out = Vec::new();
            for (r, chars) in rects.iter().zip(read) {
                let Some(mut chars) = chars else { continue };
                // spaces at either end aren't part of it
                while chars.last().is_some_and(|c| c.0.trim().is_empty()) {
                    chars.pop();
                }
                while chars.first().is_some_and(|c| c.0.trim().is_empty()) {
                    chars.remove(0);
                }
                if chars.is_empty() {
                    continue;
                }
                // each character from halfway after the one before to halfway before the next,
                // along the box (-hw..hw)
                let pos: Vec<f32> = chars.iter().map(|c| (c.1.clamp(0., 1.) * 2. - 1.) * r.hw).collect();
                let n = pos.len();
                let spans: Vec<(String, f32, f32)> = (0..n)
                    .map(|i| {
                        let a0 = if i == 0 { (-r.hw).max(pos[0] - (pos.get(1).map_or(r.hw, |p| p - pos[0])) / 2.) } else { (pos[i - 1] + pos[i]) / 2. };
                        let a1 = if i + 1 == n { r.hw.min(pos[i] + (if i > 0 { pos[i] - pos[i - 1] } else { r.hw - pos[i] }) / 2.) } else { (pos[i] + pos[i + 1]) / 2. };
                        (chars[i].0.clone(), a0, a1.max(a0))
                    })
                    .collect();
                // the box grows around the text (DB's "unclip"): the glyphs are about 0.64 of its height
                out.push(Found { cx: r.cx, cy: r.cy, angle: r.uy.atan2(r.ux), hw: r.hw, hh: r.hh * 0.64, chars: spans });
            }
            Ok(out)
        }
    }

    /// Bilinear sample of channel c (0 B, 1 G, 2 R) at (x, y), edges held.
    fn sample(p: &Picture, x: f32, y: f32, c: usize) -> f32 {
        let x = x.clamp(0., (p.w - 1) as f32);
        let y = y.clamp(0., (p.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(p.w - 1), (y0 + 1).min(p.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let g = |xx: usize, yy: usize| p.bgra[(yy * p.w + xx) * 4 + c] as f32;
        (g(x0, y0) * (1. - fx) + g(x1, y0) * fx) * (1. - fy) + (g(x0, y1) * (1. - fx) + g(x1, y1) * fx) * fy
    }

    #[derive(Clone, Copy, Debug)]
    pub struct Rect {
        pub cx: f32,
        pub cy: f32,
        pub ux: f32,
        pub uy: f32,
        pub hw: f32,
        pub hh: f32,
    }

    fn hull(mut pts: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
        pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        pts.dedup();
        if pts.len() < 3 {
            return pts;
        }
        let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
        let mut lo: Vec<(f32, f32)> = Vec::new();
        for &p in &pts {
            while lo.len() >= 2 && cross(lo[lo.len() - 2], lo[lo.len() - 1], p) <= 0. {
                lo.pop();
            }
            lo.push(p);
        }
        let mut up: Vec<(f32, f32)> = Vec::new();
        for &p in pts.iter().rev() {
            while up.len() >= 2 && cross(up[up.len() - 2], up[up.len() - 1], p) <= 0. {
                up.pop();
            }
            up.push(p);
        }
        lo.pop();
        up.pop();
        lo.extend(up);
        lo
    }

    /// The smallest rectangle around the points (rotating calipers), its length within 45° of x.
    fn min_rect(h: &[(f32, f32)]) -> Option<Rect> {
        let mut best: Option<(f32, Rect)> = None;
        for i in 0..h.len() {
            let (a, b) = (h[i], h[(i + 1) % h.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-6 {
                continue;
            }
            let (ux, uy) = (dx / len, dy / len);
            let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for &(x, y) in h {
                let (u, v) = (x * ux + y * uy, -x * uy + y * ux);
                u0 = u0.min(u);
                u1 = u1.max(u);
                v0 = v0.min(v);
                v1 = v1.max(v);
            }
            let area = (u1 - u0) * (v1 - v0);
            if best.as_ref().is_none_or(|(a, _)| area < *a) {
                let (cu, cv) = ((u0 + u1) / 2., (v0 + v1) / 2.);
                best = Some((area, Rect { cx: cu * ux - cv * uy, cy: cu * uy + cv * ux, ux, uy, hw: (u1 - u0) / 2., hh: (v1 - v0) / 2. }));
            }
        }
        let mut r = best?.1;
        for _ in 0..4 {
            if r.ux >= std::f32::consts::FRAC_1_SQRT_2 - 1e-6 {
                break;
            }
            let (ux, uy) = (r.uy, -r.ux);
            r = Rect { ux, uy, hw: r.hh, hh: r.hw, ..r };
        }
        Some(r)
    }

    /// The detector's map (`mw` × `mh`, 0–1) to boxes on the picture (`fx`, `fy`: its scale):
    /// PaddleOCR's DB post-processing (threshold 0.3, box score 0.6, unclip 1.5).
    fn boxes(prob: &[f32], mw: usize, mh: usize, fx: f32, fy: f32) -> Vec<Rect> {
        let on: Vec<bool> = prob.iter().map(|&v| v > 0.3).collect();
        let mut seen = vec![false; on.len()];
        let mut out = Vec::new();
        for start in 0..on.len() {
            if !on[start] || seen[start] {
                continue;
            }
            let mut stack = vec![start];
            seen[start] = true;
            let mut pts = Vec::new();
            while let Some(i) = stack.pop() {
                let (x, y) = ((i % mw) as f32, (i / mw) as f32);
                pts.extend([(x, y), (x + 1., y), (x, y + 1.), (x + 1., y + 1.)]);
                let (xi, yi) = ((i % mw) as i32, (i / mw) as i32);
                for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (nx, ny) = (xi + dx, yi + dy);
                    if nx < 0 || ny < 0 || nx >= mw as i32 || ny >= mh as i32 {
                        continue;
                    }
                    let j = ny as usize * mw + nx as usize;
                    if on[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
            if pts.len() < 16 {
                continue;
            }
            let Some(r) = min_rect(&hull(pts)) else { continue };
            if r.hw.min(r.hh) * 2. < 3. {
                continue;
            }
            // box score: the mean probability inside the rectangle
            let ext = r.hw.max(r.hh) * 1.5;
            let (x0, x1) = ((r.cx - ext).floor().max(0.) as usize, ((r.cx + ext).ceil().max(0.) as usize).min(mw));
            let (y0, y1) = ((r.cy - ext).floor().max(0.) as usize, ((r.cy + ext).ceil().max(0.) as usize).min(mh));
            let (mut sum, mut n) = (0f32, 0usize);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let (px, py) = (xx as f32 + 0.5 - r.cx, yy as f32 + 0.5 - r.cy);
                    if (px * r.ux + py * r.uy).abs() <= r.hw && (-px * r.uy + py * r.ux).abs() <= r.hh {
                        sum += prob[yy * mw + xx];
                        n += 1;
                    }
                }
            }
            if n == 0 || sum / (n as f32) < 0.6 {
                continue;
            }
            // unclip: grown by area · 1.5 / perimeter
            let (w2, h2) = (r.hw * 2., r.hh * 2.);
            let d = w2 * h2 * 1.5 / (2. * (w2 + h2));
            let r = Rect { hw: r.hw + d, hh: r.hh + d, ..r };
            if r.hw.min(r.hh) * 2. < 5. {
                continue;
            }
            // back to the picture's pixels (the map may be scaled differently in x and y)
            let (ux, uy) = (r.ux * fx, r.uy * fy);
            let ul = (ux * ux + uy * uy).sqrt();
            let (vx, vy) = (-r.uy * fx, r.ux * fy);
            let vl = (vx * vx + vy * vy).sqrt();
            out.push(Rect { cx: r.cx * fx, cy: r.cy * fy, ux: ux / ul, uy: uy / ul, hw: r.hw * ul, hh: r.hh * vl });
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_smallest_rectangle_follows_a_tilted_line() {
            // a 100×10 bar tilted 10°
            let a = 10f32.to_radians();
            let (c, s) = (a.cos(), a.sin());
            let pts: Vec<(f32, f32)> = [(-50., -5.), (50., -5.), (50., 5.), (-50., 5.)].iter().map(|&(x, y): &(f32, f32)| (200. + x * c - y * s, 100. + x * s + y * c)).collect();
            let r = min_rect(&hull(pts)).unwrap();
            assert!((r.hw - 50.).abs() < 0.5 && (r.hh - 5.).abs() < 0.5, "{r:?}");
            assert!((r.uy.atan2(r.ux) - a).abs() < 0.01);
            assert!((r.cx - 200.).abs() < 0.5 && (r.cy - 100.).abs() < 0.5);
        }

        #[test]
        fn the_character_list_is_read_from_the_model_settings() {
            let d = dict("PostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - '0'\n  - a\n  - ''''\n  - ș\n");
            assert_eq!(d, ["0", "a", "'", "ș"]);
        }
    }
}

/// The add-on's reader, loaded on first use and let go after a minute unused (its memory, about
/// 250 MB while reading, goes back).
struct Loaded {
    reader: paddle::Paddle,
    used: Instant,
}

static PADDLE: LazyLock<Mutex<Option<Loaded>>> = LazyLock::new(|| Mutex::new(None));
/// ONNX Runtime, loaded once (a DLL stays loaded until Ferret closes).
static ORT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
/// Why the add-on couldn't be used, and which copy of it (its manifest's time) that was: a pack
/// downloaded again since (a damaged one is deleted, "Try again" brings a whole one) is tried.
static PADDLE_BROKEN: Mutex<Option<(String, Option<std::time::SystemTime>)>> = Mutex::new(None);

fn pack_stamp() -> Option<std::time::SystemTime> {
    let dir = crate::addons::ocr_pack_dir()?;
    std::fs::metadata(dir.join("manifest.json")).and_then(|m| m.modified()).ok()
}

/// Why the add-on's reader couldn't start (shown with the note over the page), if it couldn't.
pub fn addon_problem() -> Option<String> {
    // ONNX Runtime itself not loading is for the session (a DLL loads once).
    if let Some(Err(e)) = ORT.get() {
        return Some(e.clone());
    }
    let mut b = PADDLE_BROKEN.lock().ok()?;
    let (why, stamp) = b.as_ref()?;
    if *stamp == pack_stamp() {
        return Some(why.clone());
    }
    *b = None;
    None
}

fn with_paddle<T>(f: impl FnOnce(&mut paddle::Paddle) -> Result<T, String>) -> Option<Result<T, String>> {
    let dir = crate::addons::ocr_pack_dir()?;
    if addon_problem().is_some() {
        return None;
    }
    let mut slot = PADDLE.lock().ok()?;
    if slot.is_none() {
        let stamp = pack_stamp();
        let loaded = (|| {
            crate::addons::verify_ocr_pack()?;
            let env = ORT
                .get_or_init(|| {
                    crate::addons::mark_ocr_loaded();
                    ort::init_from(dir.join("onnxruntime.dll")).map_err(|e| e.to_string())?.build().map_err(|e| e.to_string())
                })
                .as_ref()
                .map_err(|e| e.clone())?;
            paddle::Paddle::load(env, &dir)
        })();
        match loaded {
            Ok(reader) => *slot = Some(Loaded { reader, used: Instant::now() }),
            Err(e) => {
                log::warn!("ocr: the add-on's reader didn't start: {e}");
                if let Ok(mut b) = PADDLE_BROKEN.lock() {
                    *b = Some((e, stamp));
                }
                return None;
            }
        }
    }
    let l = slot.as_mut()?;
    l.used = Instant::now();
    Some(f(&mut l.reader))
}

/// Lets the add-on's reader go once it's been unused for a minute.
fn release_later() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(61));
        if let Ok(mut slot) = PADDLE.lock() {
            if slot.as_ref().is_some_and(|l| l.used.elapsed() >= Duration::from_secs(60)) {
                *slot = None;
            }
        }
    });
}

/// A page read by the add-on: turned upright first if it's sideways or upside down.
fn paddle_page(pic: &Picture) -> Option<Result<Vec<Found>, String>> {
    with_paddle(|p| {
        let q = p.upright(pic)?;
        if q == 0 {
            return p.read(pic);
        }
        let turned = pic.turned(q);
        // found on the turned picture: back on the page
        Ok(p.read(&turned)?.into_iter().map(|f| f.unturned(q, pic.w as f32, pic.h as f32)).collect())
    })
}

// ───────────────────────────── reading order ─────────────────────────────

#[derive(Clone)]
struct Item {
    i: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Gaps across x (or y) that nothing crosses: (size, where).
fn gaps(items: &[Item], along_y: bool) -> Vec<(f32, f32)> {
    let mut v: Vec<(f32, f32)> = items.iter().map(|i| if along_y { (i.y0, i.y1) } else { (i.x0, i.x1) }).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut end = f32::MIN;
    for (a, b) in v {
        if end > f32::MIN && a > end {
            out.push((a - end, (a + end) / 2.));
        }
        end = end.max(b);
    }
    out
}

/// Lines of prose: a few, long (many words each), mostly filling their column. A table's cells
/// aren't (nor a "Total / 424.44" pair).
fn prose(side: &[Item]) -> bool {
    if side.len() < 2 {
        return false;
    }
    let (x0, x1) = side.iter().fold((f32::MAX, f32::MIN), |(a, b), i| (a.min(i.x0), b.max(i.x1)));
    let w = median(side.iter().map(|i| i.x1 - i.x0).collect());
    w >= 0.45 * (x1 - x0) && w >= 8. * median(side.iter().map(|i| i.y1 - i.y0).collect())
}

/// Recursive XY cut: columns of prose first (left to right), else at the widest gap between rows,
/// else it's one row, left to right.
fn cut(items: Vec<Item>, rows: &mut Vec<Vec<usize>>) {
    if items.is_empty() {
        return;
    }
    let mh = median(items.iter().map(|i| i.y1 - i.y0).collect());
    let mut gx = gaps(&items, false);
    gx.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (g, at) in gx {
        if g < 0.8 * mh {
            break;
        }
        let (l, r): (Vec<Item>, Vec<Item>) = items.iter().cloned().partition(|i| (i.x0 + i.x1) / 2. < at);
        if prose(&l) && prose(&r) {
            cut(l, rows);
            cut(r, rows);
            return;
        }
    }
    if let Some(&(_, at)) = gaps(&items, true).iter().max_by(|a, b| a.0.total_cmp(&b.0)) {
        let (t, b): (Vec<Item>, Vec<Item>) = items.into_iter().partition(|i| (i.y0 + i.y1) / 2. < at);
        cut(t, rows);
        cut(b, rows);
        return;
    }
    let mut row = items;
    row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    rows.push(row.into_iter().map(|i| i.i).collect());
}

/// The pieces in reading order, as rows, measured square to the page's text (its middle slant).
fn order(found: &[Found]) -> Vec<Vec<usize>> {
    let mut slants: Vec<f32> = found.iter().filter(|f| f.hw > 3. * f.hh).map(|f| f.angle).collect();
    slants.sort_by(f32::total_cmp);
    let a = slants.get(slants.len() / 2).copied().unwrap_or(0.);
    let (sin, cos) = a.sin_cos();
    let items = found
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let (along, across) = (f.cx * cos + f.cy * sin, -f.cx * sin + f.cy * cos);
            Item { i, x0: along - f.hw, x1: along + f.hw, y0: across - f.hh, y1: across + f.hh }
        })
        .collect();
    let mut rows = Vec::new();
    cut(items, &mut rows);
    rows
}

// ───────────────────────────── clean-up ─────────────────────────────

/// A Romanian page (by its ș, ț and ă).
fn romanian(t: &str) -> bool {
    let signals = t.chars().filter(|c| "șțȘȚşţŞŢăĂ".contains(*c)).count();
    signals * 50 >= t.split_whitespace().count().max(1)
}

/// On a Romanian page, PaddleOCR's look-alikes of ă and î put right (ā, à, ä… aren't Romanian), an
/// î at a word's start read as an apostrophe ('nsă), and the old cedilla ş/ţ written as ș/ț.
fn romanian_fixes(found: &mut [Found]) {
    let text: String = found.iter().map(|f| f.text() + " ").collect();
    if !romanian(&text) {
        return;
    }
    for f in found {
        let n = f.chars.len();
        for i in 0..n {
            let fixed = match f.chars[i].0.as_str() {
                "ā" | "à" | "ä" | "ã" | "á" | "å" => "ă",
                "Ā" | "À" | "Ä" | "Ã" | "Á" | "Å" => "Ă",
                "ì" | "ī" | "ï" | "í" => "î",
                "ş" => "ș",
                "ţ" => "ț",
                "Ş" => "Ș",
                "Ţ" => "Ț",
                "'" if (i == 0 || f.chars[i - 1].0 == " ") && i + 1 < n && (f.chars[i + 1].0 == "n" || f.chars[i + 1].0 == "m") => "î",
                _ => continue,
            };
            f.chars[i].0 = fixed.to_string();
        }
    }
}

// ───────────────────────────── to the text layer ─────────────────────────────

/// A piece read on a `w` × `h` picture of the page, as a line of its text layer.
fn line_of(f: &Found, w: f32, h: f32) -> PdfLine {
    let mut text = String::new();
    let mut chars = Vec::new();
    let a = wrap(f.angle);
    let upright = a.abs() < UPRIGHT;
    let (sin, cos) = a.sin_cos();
    let along = (f.cx * cos + f.cy * sin) / w;
    let across = (-f.cx * sin + f.cy * cos) / w;
    for (ch, a0, a1) in &f.chars {
        let (s0, s1) = if upright { ((f.cx + a0) / w, (f.cx + a1) / w) } else { (along + a0 / w, along + a1 / w) };
        chars.push((text.len(), s0, s1));
        text.push_str(ch);
    }
    if upright {
        return PdfLine {
            text,
            chars,
            left: (f.cx - f.hw) / w,
            right: (f.cx + f.hw) / w,
            top: (f.cy - f.hh) / h,
            bottom: (f.cy + f.hh) / h,
            row_goes_on: false,
            turn: None,
        };
    }
    let turn = Turn { angle: a, aspect: h / w, start: along - f.hw / w, end: along + f.hw / w, top: across - f.hh / w, bottom: across + f.hh / w };
    let (left, right, top, bottom) = turn.page_box();
    PdfLine { text, chars, left, right, top, bottom, row_goes_on: false, turn: Some(turn) }
}

/// The pieces as the page's text layer: in reading order, a row's pieces copied with tabs between.
fn lines_of(mut found: Vec<Found>, w: f32, h: f32, reader: Reader) -> Vec<PdfLine> {
    found.retain(|f| f.chars.iter().any(|c| !c.0.trim().is_empty()));
    if reader == Reader::Paddle {
        romanian_fixes(&mut found);
    }
    let mut out = Vec::new();
    for row in order(&found) {
        let n = row.len();
        for (k, i) in row.into_iter().enumerate() {
            let mut l = line_of(&found[i], w, h);
            l.row_goes_on = k + 1 < n;
            out.push(l);
        }
    }
    out
}

// ───────────────────────────── reading pages ─────────────────────────────

/// What was read, kept for the session: a file (path and modified time) page by page, with the
/// reader that read it.
type Key = (String, u64);
static READ: LazyLock<Mutex<HashMap<Key, (Reader, HashMap<u32, Arc<Vec<PdfLine>>>)>>> = LazyLock::new(Default::default);

fn key_of(path: &str) -> Key {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    (path.to_lowercase(), modified)
}

/// The reader pages are read with now: the add-on's when it's installed and works.
pub fn reader() -> Reader {
    if crate::addons::ocr_pack_dir().is_some() && addon_problem().is_none() { Reader::Paddle } else { Reader::Windows }
}

/// Pages of this file already read with `reader`.
pub fn known(path: &str, reader: Reader) -> HashMap<u32, Arc<Vec<PdfLine>>> {
    READ.lock().ok().and_then(|m| m.get(&key_of(path)).filter(|(r, _)| *r == reader).map(|(_, p)| p.clone())).unwrap_or_default()
}

/// The preview's view of reading a PDF's scanned pages.
pub struct OcrView {
    pub path: String,
    /// The PDF's own text (pages it has none on are the ones read).
    pub base: Arc<crate::pdftext::PdfText>,
    /// Pages to read, and how many are done.
    pub total: usize,
    pub done: usize,
    /// The reader this run reads with (a page the add-on fails on is read by Windows).
    pub reader: Reader,
    pub run: Arc<Run>,
    /// Text was found on some page read.
    pub found_any: bool,
    /// Why a page couldn't be read, if one couldn't.
    pub problem: Option<String>,
}

impl OcrView {
    pub fn running(&self) -> bool {
        self.done < self.total && !self.run.cancel.load(Ordering::Relaxed)
    }
}

/// A run reading some of a file's pages, shared with the UI.
pub struct Run {
    pub cancel: AtomicBool,
    /// The page shown (read next, then the ones nearest it).
    pub near: AtomicU32,
}

/// Reads `pages` of the PDF at `path` in the background (the one nearest `run.near` next each
/// time), calling `each` with every page read (or why it couldn't be), on that thread.
pub fn read_pages(path: String, pages: Vec<u32>, run: Arc<Run>, each: impl Fn(u32, Result<(Reader, Arc<Vec<PdfLine>>), String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("ocr".into())
        .spawn(move || {
            let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
            let doc = match crate::render::pdf_open_for_reading(&path) {
                Ok(d) => d,
                Err(e) => {
                    for p in pages {
                        each(p, Err(e.clone()));
                    }
                    return;
                }
            };
            let engines = windows_engines();
            let mut left = pages;
            let key = key_of(&path);
            let mut used_paddle = false;
            while !left.is_empty() && !run.cancel.load(Ordering::Relaxed) {
                let near = run.near.load(Ordering::Relaxed);
                let k = (0..left.len()).min_by_key(|&k| (left[k] as i64 - near as i64).abs()).unwrap_or(0);
                let page = left.remove(k);
                let t = Instant::now();
                let result = (|| -> Result<(Reader, Vec<PdfLine>), String> {
                    let pic = draw(&doc, page)?;
                    let (found, reader) = match paddle_page(&pic) {
                        Some(Ok(f)) => {
                            used_paddle = true;
                            (f, Reader::Paddle)
                        }
                        Some(Err(e)) => {
                            log::warn!("ocr: the add-on couldn't read page {page}: {e}; Windows reads it");
                            (windows_page(&engines, &pic)?, Reader::Windows)
                        }
                        None if engines.is_empty() => return Err("Windows has no text recognition languages installed".into()),
                        None => (windows_page(&engines, &pic)?, Reader::Windows),
                    };
                    Ok((reader, lines_of(found, pic.w as f32, pic.h as f32, reader)))
                })();
                log::info!("ocr: page {page} of {path} in {} ms", t.elapsed().as_millis());
                match result {
                    Ok((reader, lines)) => {
                        let lines = Arc::new(lines);
                        if let Ok(mut m) = READ.lock() {
                            // a few files are kept
                            if m.len() > 8 && !m.contains_key(&key) {
                                m.clear();
                            }
                            let entry = m.entry(key.clone()).or_insert_with(|| (reader, HashMap::new()));
                            if entry.0 != reader {
                                *entry = (reader, HashMap::new());
                            }
                            entry.1.insert(page, lines.clone());
                        }
                        each(page, Ok((reader, lines)));
                    }
                    Err(e) => each(page, Err(e)),
                }
            }
            if used_paddle {
                release_later();
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(text: &str, cx: f32, cy: f32, hw: f32) -> Found {
        let n = text.chars().count() as f32;
        let chars = text.chars().enumerate().map(|(i, c)| (c.to_string(), -hw + 2. * hw * i as f32 / n, -hw + 2. * hw * (i + 1) as f32 / n)).collect();
        Found { cx, cy, angle: 0., hw, hh: 8., chars }
    }

    fn texts(found: &[Found]) -> Vec<String> {
        order(found).into_iter().map(|row| row.into_iter().map(|i| found[i].text()).collect::<Vec<_>>().join(" | ")).collect()
    }

    #[test]
    fn columns_of_prose_read_one_after_the_other_and_tables_by_row() {
        // a heading, two columns of long lines, a footer
        let mut f = vec![piece("Heading across the page", 500., 50., 300.)];
        for i in 0..4 {
            f.push(piece(&format!("left column line {i} with many words"), 250., 120. + i as f32 * 30., 200.));
            f.push(piece(&format!("right column line {i} with many words"), 750., 120. + i as f32 * 30., 200.));
        }
        f.push(piece("Page 3 of 12", 500., 400., 80.));
        let t = texts(&f);
        assert_eq!(t[0], "Heading across the page");
        assert!(t[1].starts_with("left column line 0"));
        assert!(t[4].starts_with("left column line 3"));
        assert!(t[5].starts_with("right column line 0"));
        assert_eq!(t.last().unwrap(), "Page 3 of 12");

        // a table: short cells in columns; and a "Total  424.44" pair
        let f = vec![
            piece("Item", 100., 100., 40.),
            piece("Qty", 400., 100., 20.),
            piece("Total", 600., 100., 30.),
            piece("Flour", 100., 130., 40.),
            piece("12", 400., 130., 15.),
            piece("220.80", 600., 130., 35.),
            piece("Total", 100., 200., 40.),
            piece("424.44", 600., 200., 35.),
        ];
        assert_eq!(texts(&f), ["Item | Qty | Total", "Flour | 12 | 220.80", "Total | 424.44"]);
    }

    #[test]
    fn tilted_lines_stay_apart() {
        // lines 1000 px long, 30 px apart, tilted 2.5°: their boxes on the page overlap, square to
        // the text they don't
        let a = 2.5f32.to_radians();
        let f: Vec<Found> = (0..5)
            .map(|i| {
                let mut p = piece(&format!("line number {i} of a tilted page of text"), 600., 200. + i as f32 * 30., 500.);
                p.angle = a;
                // the middle moves with the tilt
                p.cy += 600. * a.tan();
                p
            })
            .collect();
        let t = texts(&f);
        assert_eq!(t.len(), 5);
        assert!(t[2].starts_with("line number 2"));
    }

    #[test]
    fn a_found_line_lands_where_it_was_read() {
        let f = piece("Hello", 500., 300., 100.);
        let l = line_of(&f, 1000., 2000.);
        assert!(l.turn.is_none());
        assert!((l.left - 0.4).abs() < 1e-4 && (l.right - 0.6).abs() < 1e-4);
        assert!((l.top - 292. / 2000.).abs() < 1e-4);
        assert_eq!(l.chars.len(), 5);
        assert!((l.chars[1].1 - 0.44).abs() < 1e-4);
        // read on the page turned a quarter clockwise: back where it is on the page
        let g = Found { cx: 300., cy: 500., angle: 0., ..piece("Hello", 0., 0., 100.) }.unturned(1, 1000., 2000.);
        assert!((g.cx - 500.).abs() < 1e-3 && (g.cy - 1700.).abs() < 1e-3);
        assert!((g.angle + std::f32::consts::FRAC_PI_2).abs() < 1e-4);
        let l = line_of(&g, 1000., 2000.);
        assert!(l.turn.is_some());
    }

    #[test]
    fn romanian_look_alikes_are_put_right() {
        let mut f = vec![piece("înșă 'nsă tabloul nu e bunā", 0., 0., 100.), piece("ţigle și ş", 0., 30., 100.)];
        romanian_fixes(&mut f);
        assert_eq!(f[0].text(), "înșă însă tabloul nu e bună");
        assert_eq!(f[1].text(), "țigle și ș");
        // not Romanian: left alone
        let mut f = vec![piece("café à la carte 'n stuff", 0., 0., 100.)];
        romanian_fixes(&mut f);
        assert_eq!(f[0].text(), "café à la carte 'n stuff");
    }

    #[test]
    fn gibberish_is_told_from_words() {
        assert_eq!(wordlike("The total cost was 18,450.75 EUR"), (6, 6));
        let (g, t) = wordlike("'I ko +ovna+oesl Sa+mrdaq ::");
        assert!(g * 10 < t * 7);
        assert!(accented("Raport de inspecție tehnică în urma"));
        assert!(!accented("The quick brown fox jumps over the lazy dog"));
    }
}
