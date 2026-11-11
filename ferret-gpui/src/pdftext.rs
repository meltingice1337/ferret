//! PDFs beyond the pictures Windows draws of their pages (render.rs): the passwords typed this
//! session, and each page's text with where every character is, laid over the page so it can be
//! selected and copied (Windows.Data.Pdf has no text API; this is what pdf.js's text layer does).
//!
//! Passwords live in this process's memory only: never saved, never logged, gone when Ferret
//! closes. A file keeps the one that opened it (revisits, Quick Look, more pages, its text), and
//! the ones typed this session are tried on other protected PDFs before asking (bank statements
//! usually share one).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Passwords {
    /// The password that opened each file (by its path, without case).
    by_file: HashMap<String, String>,
    /// Every password that opened something this session, the latest first.
    typed: Vec<String>,
}

static PASSWORDS: LazyLock<Mutex<Passwords>> = LazyLock::new(Default::default);

fn file_key(path: &str) -> String {
    path.to_lowercase()
}

/// The password that opened this file before, if any.
pub fn password_for(path: &str) -> Option<String> {
    PASSWORDS.lock().ok()?.by_file.get(&file_key(path)).cloned()
}

/// The passwords typed this session (the latest first), to try on a file that wants one.
pub fn typed_passwords() -> Vec<String> {
    PASSWORDS.lock().map(|p| p.typed.clone()).unwrap_or_default()
}

/// A password opened this file: kept for the session, for it and to try on others.
pub fn remember(path: &str, password: &str) {
    if let Ok(mut p) = PASSWORDS.lock() {
        p.by_file.insert(file_key(path), password.to_string());
        p.typed.retain(|t| t != password);
        p.typed.insert(0, password.to_string());
        p.typed.truncate(16);
    }
}

// ───────────────────────────── text layer ─────────────────────────────

/// A document's text, page by page (as many pages as the preview shows).
#[derive(Default, Clone)]
pub struct PdfText {
    pub pages: Vec<Vec<PdfLine>>,
}

/// A line of text on a page (a run of characters on one baseline). Places are fractions of the
/// page as Windows shows it (cropped and turned), 0–1 from its top left.
#[derive(Clone, Debug)]
pub struct PdfLine {
    pub text: String,
    /// Each character: its byte offset in `text`, and where it starts and ends along the line: an
    /// upright line's left and right edges, a turned one's places in its own frame (`turn`).
    pub chars: Vec<(usize, f32, f32)>,
    /// The box it takes on the page (a turned line: the box around it).
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    /// The next line in reading order is on the same row (a table's next cell): copied with a tab
    /// between them instead of a new line.
    pub row_goes_on: bool,
    /// A line that isn't upright (vertical text, a label at an angle): its own frame, so it's
    /// selected along the text instead of by the box around it.
    pub turn: Option<Turn>,
}

/// A turned line's frame: the page turned by `angle` (radians, clockwise as shown) about its top
/// left, measured in page widths (the page is `aspect` of them tall). The line runs from `start`
/// to `end` along it and from `top` to `bottom` across it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub angle: f32,
    pub aspect: f32,
    pub start: f32,
    pub end: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Turn {
    /// A point of the frame (along, across) as a fraction of the page.
    pub fn to_page(&self, a: f32, c: f32) -> (f32, f32) {
        let (sin, cos) = self.angle.sin_cos();
        (a * cos - c * sin, (a * sin + c * cos) / self.aspect.max(1e-6))
    }

    /// A place on the page (fractions) in the frame: (along, across).
    pub fn from_page(&self, fx: f32, fy: f32) -> (f32, f32) {
        let (x, y) = (fx, fy * self.aspect);
        let (sin, cos) = self.angle.sin_cos();
        (x * cos + y * sin, -x * sin + y * cos)
    }

    /// The box around the line on the page (fractions): left, right, top, bottom.
    pub fn page_box(&self) -> (f32, f32, f32, f32) {
        let corners = [(self.start, self.top), (self.end, self.top), (self.end, self.bottom), (self.start, self.bottom)].map(|(a, c)| self.to_page(a, c));
        let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for (x, y) in corners {
            l = l.min(x);
            r = r.max(x);
            t = t.min(y);
            b = b.max(y);
        }
        (l, r, t, b)
    }
}

/// Lines turned less than this (radians, about a degree) are taken as upright.
pub const UPRIGHT: f32 = 0.017;

impl PdfLine {
    fn mid(&self) -> f32 {
        (self.top + self.bottom) / 2.
    }
    fn height(&self) -> f32 {
        (self.bottom - self.top).max(1e-4)
    }
}

impl PdfText {
    pub fn lines(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
    }
}

/// The page box shown (CropBox, else MediaBox: llx lly urx ury) and its turn (/Rotate, degrees).
struct Frame {
    bx: [f64; 4],
    rotate: i64,
}

impl Frame {
    /// The page's height as shown over its width.
    fn aspect(&self) -> f32 {
        let [llx, lly, urx, ury] = self.bx;
        let (w, h) = ((urx - llx).abs().max(1e-6), (ury - lly).abs().max(1e-6));
        (if self.rotate.rem_euclid(180) == 90 { w / h } else { h / w }) as f32
    }

    /// A point in the page's user space as a fraction of the page as shown, from its top left.
    fn place(&self, x: f64, y: f64) -> (f32, f32) {
        let [llx, lly, urx, ury] = self.bx;
        let fx = (x - llx) / (urx - llx).abs().max(1e-6);
        let fy = (ury - y) / (ury - lly).abs().max(1e-6);
        let (u, v) = match self.rotate.rem_euclid(360) {
            90 => (1. - fy, fx),
            180 => (1. - fx, 1. - fy),
            270 => (fy, 1. - fx),
            _ => (fx, fy),
        };
        (u as f32, v as f32)
    }
}

/// One character as drawn: its text and box (fractions of the page shown); turned, also its
/// direction and its place in the frame turned that way (`Turn`): along (`a0`–`a1`) and across
/// (`c0`–`c1`). Upright, those are its box.
struct Glyph {
    s: String,
    l: f32,
    r: f32,
    t: f32,
    b: f32,
    angle: f32,
    aspect: f32,
    a0: f32,
    a1: f32,
    c0: f32,
    c1: f32,
}

impl Glyph {
    fn upright(s: String, l: f32, r: f32, t: f32, b: f32) -> Self {
        Glyph { s, l, r, t, b, angle: 0., aspect: 1., a0: l, a1: r, c0: t, c1: b }
    }
}

struct Collect {
    frames: HashMap<u32, Frame>,
    page: u32,
    glyphs: Vec<Glyph>,
    pages: Vec<Vec<PdfLine>>,
}

impl pdf_extract::OutputDev for Collect {
    fn begin_page(&mut self, page_num: u32, media_box: &pdf_extract::MediaBox, _: Option<(f64, f64, f64, f64)>) -> Result<(), pdf_extract::OutputError> {
        self.page = page_num;
        self.frames.entry(page_num).or_insert(Frame { bx: [media_box.llx, media_box.lly, media_box.urx, media_box.ury], rotate: 0 });
        self.glyphs.clear();
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), pdf_extract::OutputError> {
        let glyphs = std::mem::take(&mut self.glyphs);
        self.pages.push(reading_order(lines_of(glyphs)));
        Ok(())
    }

    fn output_character(&mut self, trm: &pdf_extract::Transform, width: f64, _spacing: f64, font_size: f64, s: &str) -> Result<(), pdf_extract::OutputError> {
        let s = clean(s);
        if s.is_empty() {
            return Ok(());
        }
        let Some(frame) = self.frames.get(&self.page) else { return Ok(()) };
        // The text rendering matrix (without the font size): the glyph's origin, its advance and
        // its height in user space. Boxes reach a little under the baseline and most of the way up.
        let advance = if width > 0. { width * font_size } else { font_size * 0.5 };
        let (ox, oy) = (trm.m31, trm.m32);
        let (ax, ay) = (advance * trm.m11, advance * trm.m12);
        let (ux, uy) = (font_size * trm.m21, font_size * trm.m22);
        let corners = [(-0.22, false), (-0.22, true), (0.82, false), (0.82, true)].map(|(k, end)| {
            let (x, y) = (ox + k * ux + if end { ax } else { 0. }, oy + k * uy + if end { ay } else { 0. });
            frame.place(x, y)
        });
        let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for (u, v) in corners {
            l = l.min(u);
            r = r.max(u);
            t = t.min(v);
            b = b.max(v);
        }
        if !(l.is_finite() && r.is_finite() && t.is_finite() && b.is_finite()) {
            return Ok(());
        }
        // Which way it runs as shown (in page widths both ways, so the angle is true).
        let aspect = frame.aspect();
        let (o, e) = (frame.place(ox, oy), frame.place(ox + ax, oy + ay));
        let angle = ((e.1 - o.1) * aspect).atan2(e.0 - o.0);
        if !angle.is_finite() || angle.abs() < UPRIGHT {
            self.glyphs.push(Glyph::upright(s, l, r, t, b));
            return Ok(());
        }
        // Turned: its place along and across its own direction.
        let turn = Turn { angle, aspect, start: 0., end: 0., top: 0., bottom: 0. };
        let (a0, _) = turn.from_page(o.0, o.1);
        let (a1, _) = turn.from_page(e.0, e.1);
        let (mut c0, mut c1) = (f32::MAX, f32::MIN);
        for (u, v) in corners {
            let c = turn.from_page(u, v).1;
            c0 = c0.min(c);
            c1 = c1.max(c);
        }
        self.glyphs.push(Glyph { s, l, r, t, b, angle, aspect, a0: a0.min(a1), a1: a0.max(a1), c0, c1 });
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_word(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
    fn end_line(&mut self) -> Result<(), pdf_extract::OutputError> {
        Ok(())
    }
}

/// A character as copied: zero-width and invisible marks dropped (zero-width spaces and joiners,
/// soft hyphens, byte-order marks, controls), odd spaces made plain ones, ligatures (ﬁ, ﬂ…) spelled out.
fn clean(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}' | '\u{FFFD}' => {}
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\t' => out.push(' '),
            '\u{FB00}'..='\u{FB06}' => out.extend(c.nfkc()),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A page's lines in reading order. The order a page draws its text in is often not the order
/// it's read in (a statement may draw its table column by column), so: top to bottom, and left
/// to right along a row (a row's cells copy with tabs between them). A page set in two columns
/// (long lines either side of a gap running down most of the page) reads one column after the
/// other, between the lines that span both (a title).
fn reading_order(lines: Vec<PdfLine>) -> Vec<PdfLine> {
    if lines.len() < 2 {
        return lines;
    }
    let mut lines = lines;
    // Two columns: a gap (x) that few lines cross, with long lines either side.
    let gutter = (5..=15).map(|k| k as f32 * 0.05).filter_map(|x| {
        let crossing: f32 = lines.iter().filter(|l| l.left < x && l.right > x).map(|l| l.height()).sum();
        let total: f32 = lines.iter().map(|l| l.height()).sum();
        let (left, right): (Vec<&PdfLine>, Vec<&PdfLine>) = lines.iter().filter(|l| !(l.left < x && l.right > x)).partition(|l| l.right <= x);
        let long = |side: &[&PdfLine]| {
            let mut w: Vec<f32> = side.iter().map(|l| l.right - l.left).collect();
            w.sort_by(|a, b| a.total_cmp(b));
            side.len() >= 4 && w[w.len() / 2] >= 0.22
        };
        (crossing <= total * 0.15 && long(&left) && long(&right)).then_some((crossing, x))
    })
    .min_by(|a, b| a.0.total_cmp(&b.0))
    .map(|(_, x)| x);
    // Rows, top to bottom: lines whose middles are within half a line of each other.
    let rows_of = |mut ls: Vec<PdfLine>| -> Vec<PdfLine> {
        ls.sort_by(|a, b| a.mid().total_cmp(&b.mid()));
        let mut rows: Vec<Vec<PdfLine>> = Vec::new();
        for l in ls {
            match rows.last_mut() {
                Some(row) if (l.mid() - row[0].mid()).abs() < 0.5 * l.height().min(row[0].height()) => row.push(l),
                _ => rows.push(vec![l]),
            }
        }
        let mut out = Vec::new();
        for mut row in rows {
            row.sort_by(|a, b| a.left.total_cmp(&b.left));
            let n = row.len();
            for (i, mut l) in row.into_iter().enumerate() {
                l.row_goes_on = i + 1 < n;
                out.push(l);
            }
        }
        out
    };
    let Some(x) = gutter else { return rows_of(lines) };
    // Two columns: between lines that span both, the left column, then the right.
    lines.sort_by(|a, b| a.mid().total_cmp(&b.mid()));
    let mut out = Vec::new();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for l in lines {
        if l.left < x && l.right > x {
            out.extend(rows_of(std::mem::take(&mut left)));
            out.extend(rows_of(std::mem::take(&mut right)));
            out.extend(rows_of(vec![l]));
        } else if l.right <= x {
            left.push(l);
        } else {
            right.push(l);
        }
    }
    out.extend(rows_of(left));
    out.extend(rows_of(right));
    out
}

/// Characters (in the order the page draws them) joined into lines: the next one in the same
/// direction on the same baseline, a little after the last, continues the line (a gap becomes a
/// space, as copied). Upright lines are measured on the page, turned ones in their own frame.
fn lines_of(glyphs: Vec<Glyph>) -> Vec<PdfLine> {
    /// A line being joined: the line, its direction, and where it is in its frame.
    struct Cur {
        line: PdfLine,
        angle: f32,
        aspect: f32,
        start: f32,
        end: f32,
        top: f32,
        bottom: f32,
    }
    let mut out: Vec<PdfLine> = Vec::new();
    let mut cur: Option<Cur> = None;
    let finish = |c: Option<Cur>, out: &mut Vec<PdfLine>| {
        let Some(c) = c else { return };
        let mut l = c.line;
        // Spaces at its end aren't part of what's selected or copied.
        while l.text.ends_with(' ') {
            l.text.pop();
            l.chars.pop();
        }
        if l.text.trim().is_empty() {
            return;
        }
        let end = l.chars.last().map(|ch| ch.2).unwrap_or(c.end);
        if c.angle == 0. {
            l.right = end;
        } else {
            let turn = Turn { angle: c.angle, aspect: c.aspect, start: c.start, end, top: c.top, bottom: c.bottom };
            (l.left, l.right, l.top, l.bottom) = turn.page_box();
            l.turn = Some(turn);
        }
        out.push(l);
    };
    // How far apart two directions are (radians, either way round).
    let apart = |a: f32, b: f32| ((a - b + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI).abs();
    for g in glyphs {
        let h = (g.c1 - g.c0).max(1e-4);
        let blank = g.s.trim().is_empty();
        let same = cur.as_ref().is_some_and(|c| {
            let lh = (c.bottom - c.top).max(1e-4);
            let mid = (g.c0 + g.c1) / 2.;
            let lmid = (c.top + c.bottom) / 2.;
            apart(g.angle, c.angle) < UPRIGHT && (mid - lmid).abs() < 0.5 * lh.max(h) && g.a0 >= c.end - 0.6 * h && g.a0 - c.end < 3. * h
        });
        if !same {
            finish(cur.take(), &mut out);
            if blank {
                continue;
            }
            cur = Some(Cur {
                line: PdfLine { text: String::new(), chars: Vec::new(), left: g.l, right: g.r, top: g.t, bottom: g.b, row_goes_on: false, turn: None },
                angle: g.angle,
                aspect: g.aspect,
                start: g.a0,
                end: g.a1,
                top: g.c0,
                bottom: g.c1,
            });
        }
        let c = cur.as_mut().unwrap();
        // A gap with no space drawn in it (text placed by position) reads as one.
        if !blank && !c.line.text.is_empty() && !c.line.text.ends_with(' ') && g.a0 - c.end > 0.18 * h {
            c.line.chars.push((c.line.text.len(), c.end, g.a0));
            c.line.text.push(' ');
        }
        if blank && c.line.text.ends_with(' ') {
            continue;
        }
        let s = if blank { " ".to_string() } else { g.s };
        c.line.chars.push((c.line.text.len(), g.a0, g.a1));
        c.line.text.push_str(&s);
        c.end = c.end.max(g.a1);
        c.start = c.start.min(g.a0);
        c.top = c.top.min(g.c0);
        c.bottom = c.bottom.max(g.c1);
        let l = &mut c.line;
        l.right = l.right.max(g.r);
        l.left = l.left.min(g.l);
        l.top = l.top.min(g.t);
        l.bottom = l.bottom.max(g.b);
    }
    finish(cur, &mut out);
    out
}

/// A value from a page's dictionary, or one of its parents' (pages inherit their boxes and turn).
fn inherited<'a>(doc: &'a pdf_extract::Document, mut dict: &'a pdf_extract::Dictionary, key: &[u8]) -> Option<&'a pdf_extract::Object> {
    for _ in 0..32 {
        if let Ok(v) = dict.get(key) {
            return Some(v);
        }
        let parent = dict.get(b"Parent").ok()?.as_reference().ok()?;
        dict = doc.get_dictionary(parent).ok()?;
    }
    None
}

fn number(o: &pdf_extract::Object) -> Option<f64> {
    match o {
        pdf_extract::Object::Integer(i) => Some(*i as f64),
        pdf_extract::Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

fn rect(doc: &pdf_extract::Document, o: &pdf_extract::Object) -> Option<[f64; 4]> {
    let o = match o {
        pdf_extract::Object::Reference(id) => doc.get_object(*id).ok()?,
        o => o,
    };
    let a = o.as_array().ok()?;
    let v: Vec<f64> = a.iter().filter_map(number).collect();
    (v.len() == 4).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

/// The text of the first `max_pages` pages of the PDF at `path` (with the password that opened
/// it this session, if it has one). None if it can't be read (or has no pages).
pub fn extract(path: &str, max_pages: u32) -> Option<PdfText> {
    // The whole file is read into memory: a huge one (scans, mostly pictures) goes without.
    if std::fs::metadata(path).ok()?.len() > 200 * 1024 * 1024 {
        return None;
    }
    let password = password_for(path);
    // pdf-extract panics on some malformed files: a preview without its text then, not a crash.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut doc = match &password {
            Some(p) => pdf_extract::Document::load_with_password(path, p).ok()?,
            None => pdf_extract::Document::load(path).ok()?,
        };
        if doc.is_encrypted() {
            doc.decrypt(password.as_deref().unwrap_or("")).ok()?;
        }
        let pages = doc.get_pages();
        let mut frames = HashMap::new();
        for (&n, &id) in pages.iter().take(max_pages as usize) {
            let Ok(dict) = doc.get_dictionary(id) else { continue };
            let bx = inherited(&doc, dict, b"CropBox").and_then(|o| rect(&doc, o)).or_else(|| inherited(&doc, dict, b"MediaBox").and_then(|o| rect(&doc, o)));
            let rotate = inherited(&doc, dict, b"Rotate").and_then(number).unwrap_or(0.) as i64;
            if let Some(bx) = bx {
                frames.insert(n, Frame { bx, rotate });
            }
        }
        let mut out = Collect { frames, page: 0, glyphs: Vec::new(), pages: Vec::new() };
        for &n in pages.keys().take(max_pages as usize) {
            let before = out.pages.len();
            // Page by page: one it can't read (or panics on) leaves the others' text.
            let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pdf_extract::output_doc_page(&doc, &mut out, n).is_ok())).unwrap_or(false);
            if !ok {
                out.glyphs.clear();
            }
            // A page that failed still has its place (pages line up with the pictures).
            out.pages.truncate(before + 1);
            if out.pages.len() == before {
                out.pages.push(Vec::new());
            }
        }
        (!out.pages.is_empty()).then(|| PdfText { pages: out.pages })
    }))
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(s: &str, l: f32, r: f32, t: f32) -> Glyph {
        Glyph::upright(s.into(), l, r, t, t + 0.02)
    }

    #[test]
    fn turned_glyphs_join_along_their_own_direction() {
        // "Up" written bottom to top (turned -90°) on a square page at x = 0.3, from y = 0.6 up.
        let angle = -std::f32::consts::FRAC_PI_2;
        let t = Turn { angle, aspect: 1., start: 0., end: 0., top: 0., bottom: 0. };
        let glyph = |s: &str, y0: f32, y1: f32| {
            let (a0, c0) = t.from_page(0.29, y0);
            let (a1, c1) = t.from_page(0.31, y1);
            Glyph { s: s.into(), l: 0.29, r: 0.31, t: y1, b: y0, angle, aspect: 1., a0: a0.min(a1), a1: a0.max(a1), c0: c0.min(c1), c1: c0.max(c1) }
        };
        let lines = lines_of(vec![glyph("U", 0.6, 0.58), glyph("p", 0.58, 0.565), g("x", 0.5, 0.51, 0.1)]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Up");
        let turn = lines[0].turn.expect("turned");
        assert!((turn.angle - angle).abs() < 1e-4);
        assert!((lines[0].top - 0.565).abs() < 1e-3 && (lines[0].bottom - 0.6).abs() < 1e-3, "{} {}", lines[0].top, lines[0].bottom);
        assert!(lines[1].turn.is_none());
        // Back and forth between the page and the frame.
        let (a, c) = turn.from_page(0.3, 0.59);
        let (x, y) = turn.to_page(a, c);
        assert!((x - 0.3).abs() < 1e-5 && (y - 0.59).abs() < 1e-5);
    }

    #[test]
    fn glyphs_join_into_lines_with_spaces() {
        let lines = lines_of(vec![
            g("H", 0.10, 0.11, 0.1),
            g("i", 0.11, 0.115, 0.1),
            // a gap with no space drawn
            g("y", 0.13, 0.14, 0.1),
            g("o", 0.14, 0.15, 0.1),
            // the next line, further down
            g("N", 0.10, 0.11, 0.2),
            g(" ", 0.11, 0.115, 0.2),
            g(" ", 0.115, 0.12, 0.2),
        ]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Hi yo");
        assert_eq!(lines[0].chars.len(), 5);
        assert_eq!(lines[1].text, "N");
        assert_eq!(lines[1].chars.len(), 1);
    }

    fn line(text: &str, left: f32, right: f32, top: f32) -> PdfLine {
        PdfLine { text: text.into(), chars: Vec::new(), left, right, top, bottom: top + 0.015, row_goes_on: false, turn: None }
    }

    #[test]
    fn copied_text_is_clean() {
        assert_eq!(clean("a\u{200B}b\u{00AD}c\u{FEFF}"), "abc");
        assert_eq!(clean("\u{FB01}ne\u{00A0}day"), "fine day");
        assert_eq!(clean("\u{200B}"), "");
    }

    #[test]
    fn a_table_drawn_column_by_column_reads_row_by_row() {
        // Drawn: the dates, then the descriptions, then the amounts.
        let out = reading_order(vec![
            line("10/01", 0.1, 0.18, 0.30),
            line("10/03", 0.1, 0.18, 0.33),
            line("Opening balance", 0.3, 0.5, 0.30),
            line("Coffee shop", 0.3, 0.45, 0.33),
            line("1,250.00", 0.7, 0.8, 0.30),
            line("-4.50", 0.72, 0.8, 0.33),
        ]);
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["10/01", "Opening balance", "1,250.00", "10/03", "Coffee shop", "-4.50"]);
        assert!(out[0].row_goes_on && out[1].row_goes_on && !out[2].row_goes_on);
    }

    #[test]
    fn two_columns_read_one_after_the_other() {
        let mut lines = vec![line("Title across both columns", 0.1, 0.9, 0.05)];
        for i in 0..5 {
            let y = 0.2 + i as f32 * 0.03;
            lines.push(line(&format!("left {i}"), 0.08, 0.46, y));
            lines.push(line(&format!("right {i}"), 0.54, 0.92, y));
        }
        let out = reading_order(lines);
        let texts: Vec<&str> = out.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts[0], "Title across both columns");
        assert_eq!(&texts[1..6], ["left 0", "left 1", "left 2", "left 3", "left 4"]);
        assert_eq!(texts[6], "right 0");
    }

    #[test]
    fn turned_pages_map_to_what_is_shown() {
        let f = Frame { bx: [0., 0., 100., 200.], rotate: 90 };
        // The unturned page's top left shows at the top right.
        assert_eq!(f.place(0., 200.), (1., 0.));
        let f = Frame { bx: [0., 0., 100., 200.], rotate: 0 };
        assert_eq!(f.place(50., 50.), (0.5, 0.75));
    }

    #[test]
    fn passwords_are_kept_per_file_and_tried_on_others() {
        remember("C:\\T\\A.pdf", "one");
        remember("C:\\T\\B.pdf", "two");
        assert_eq!(password_for("c:\\t\\a.pdf").as_deref(), Some("one"));
        let typed = typed_passwords();
        assert_eq!(typed.first().map(String::as_str), Some("two"));
        assert!(typed.iter().any(|p| p == "one"));
    }
}
