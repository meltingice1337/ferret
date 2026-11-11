//! "Save as searchable PDF": a copy of a scanned PDF with the text recognised in it (ocr.rs) laid
//! over each scanned page, invisible, so other apps (Edge, Chrome, Acrobat, and Ferret itself) can
//! select, copy and find it. The pages look exactly as they did.
//!
//! How (Tesseract's way, which every reader handles):
//! - The copy is the original file byte for byte, plus an incremental update (as Acrobat saves):
//!   each scanned page's dictionary again, with one more content stream and a font added to its
//!   own copy of its resources (a page sharing or inheriting them leaves the shared ones alone).
//!   Nothing else is rewritten, so nothing else can change. A password-protected file stays
//!   protected: its /Encrypt and /ID are kept and the new objects are encrypted with its key, so
//!   the copy opens with the same password and has the same permissions.
//! - The text is drawn in render mode 3 (invisible) in a tiny embedded TrueType font that draws
//!   nothing ("GlyphLessFont", as Tesseract's): a Type0 font, Identity-H, each character's code
//!   its UTF-16 value, mapped back to itself by the ToUnicode CMap; every glyph half an em wide,
//!   with a box (but no ink) as tall as the line, so readers measure each word as it was read.
//! - Each word is placed where it was read: its baseline at its start, the font as tall as its
//!   line, stretched (`Tz`) to the word's width; after it, a space stretched over the gap to the
//!   next word, so it copies with spaces. A turned line gets a turned text matrix, and the page's
//!   own frame (CropBox, else MediaBox, and /Rotate) is undone (pdftext::Frame::unplace).
//! - Lines go in reading order (as read), so readers that go by the order text is drawn in
//!   (find in pdf.js, copying) get it right.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex};

use gpui::Context;
use pdf_extract::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};

use crate::pdftext::{Frame, PdfLine};
use crate::util;
use crate::view::{Ferret, action};

/// The font's name (also its PostScript name: tools that strip an OCR layer look for it).
const FONT: &str = "GlyphLessFont";
/// The font's ascent and descent (of 1000 units), its glyph's box too. The baseline sits this far
/// up from the bottom of the line as read, so readers that box characters by their glyph's box
/// (PDFium: Chrome, Edge) or the font's ascent and descent (Acrobat, pdf.js) box each word exactly
/// where it was read (Ferret's own text layer, by fixed proportions, a tenth of a line lower).
const ASCENT: i64 = 900;
const DESCENT: i64 = -100;
/// Each glyph's width (of 1000 units).
const WIDTH: i64 = 500;

// ───────────────────────────── placing the words ─────────────────────────────

/// A word laid on the page: where its baseline starts (user space), which way it runs and which
/// way is up (unit vectors, user space), how long it is and how tall its line is (user space
/// units), and its text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placed {
    pub origin: (f64, f64),
    pub along: (f64, f64),
    pub up: (f64, f64),
    pub width: f64,
    pub size: f64,
    pub text: String,
}

/// A line's words: each one's text and where it starts and ends along the line (as `PdfLine::chars`).
fn words_of(line: &PdfLine) -> Vec<(String, f64, f64)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, f64, f64)> = None;
    for (i, &(off, s0, s1)) in line.chars.iter().enumerate() {
        let end = line.chars.get(i + 1).map(|c| c.0).unwrap_or(line.text.len());
        let s = line.text.get(off..end).unwrap_or("");
        if s.trim().is_empty() {
            out.extend(cur.take());
            continue;
        }
        match cur.as_mut() {
            Some(w) => {
                w.0.push_str(s.trim());
                w.1 = w.1.min(s0 as f64);
                w.2 = w.2.max(s1 as f64);
            }
            None => cur = Some((s.trim().to_string(), s0 as f64, s1 as f64)),
        }
    }
    out.extend(cur);
    out
}

/// A line's words placed in the page's user space (see `Placed`).
pub(crate) fn place_line(frame: &Frame, line: &PdfLine) -> Vec<Placed> {
    let (wd, hd) = frame.shown_size();
    let words = words_of(line);
    if wd <= 0. || hd <= 0. || words.is_empty() {
        return Vec::new();
    }
    // A point of the line's own frame (along it, across it) on the page as shown, in user space
    // units from its top left. Upright: fractions of the page's width and height; turned: page
    // widths in the turned frame (pdftext::Turn).
    let turn = line.turn;
    let at = |a: f64, c: f64| -> (f64, f64) {
        match turn {
            Some(t) => {
                let (sin, cos) = (t.angle as f64).sin_cos();
                let aspect = (t.aspect as f64).max(1e-6);
                ((a * cos - c * sin) * wd, (a * sin + c * cos) / aspect * hd)
            }
            None => (a * wd, c * hd),
        }
    };
    let (top, bottom) = match turn {
        Some(t) => (t.top as f64, t.bottom as f64),
        None => (line.top as f64, line.bottom as f64),
    };
    let base = bottom + (bottom - top) * DESCENT as f64 / (ASCENT - DESCENT) as f64;
    let sub = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0, p.1 - q.1);
    let dot = |p: (f64, f64), q: (f64, f64)| p.0 * q.0 + p.1 * q.1;
    // Which way the line runs (as shown, y down) and which way is up: the whole line's, so all its
    // words share one.
    let (first, last) = (words[0].1, words[words.len() - 1].2);
    let d = sub(at(last, base), at(first, base));
    let len = d.0.hypot(d.1);
    let dir = if len > 1e-9 {
        (d.0 / len, d.1 / len)
    } else {
        let a = turn.map(|t| t.angle as f64).unwrap_or(0.);
        (a.cos(), a.sin())
    };
    let up = (dir.1, -dir.0);
    let tall = sub(at(first, bottom), at(first, top));
    let size = tall.0.hypot(tall.1);
    let user = |p: (f64, f64)| frame.unplace(p.0 / wd, p.1 / hd);
    let mut out = Vec::new();
    for (text, a0, a1) in &words {
        let o = at(*a0, base);
        let e = at(*a1, base);
        let ou = user(o);
        let along = sub(user((o.0 + dir.0, o.1 + dir.1)), ou);
        let upu = sub(user((o.0 + up.0, o.1 + up.1)), ou);
        out.push(Placed { origin: ou, along, up: upu, width: dot(sub(e, o), dir), size, text: text.clone() });
    }
    out
}

/// A number for a content stream: three decimals at most, no exponent.
fn num(x: f64) -> String {
    let s = format!("{:.3}", if x.is_finite() { x } else { 0. });
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

/// A text's character codes: its UTF-16 values (characters outside the Basic Multilingual Plane,
/// which OCR doesn't produce, are left out).
fn codes(text: &str) -> Vec<u16> {
    text.chars().filter(|c| (*c as u32) < 0x10000).map(|c| c as u32 as u16).collect()
}

/// The content stream drawing a page's lines invisibly in the font named `font` (its resource
/// name). `after`: it follows the page's own content (which a `q` put before it keeps from
/// leaving its state behind: closed here). None if no word could be placed.
///
/// A word to a text object (as Tesseract writes them): placed at its start, stretched (`Tz`) to
/// its width, so each character's box is where it was read, and a word never comes apart. (One
/// `TJ` to a line, its glyphs moved into place, was tried: words wider than the line's average
/// came apart into letters in PDFium and in Ferret's reader, which read wide gaps as spaces.)
pub(crate) fn text_layer(frame: &Frame, lines: &[PdfLine], font: &str, after: bool) -> Option<Vec<u8>> {
    let mut s = String::new();
    if after {
        s.push_str("Q\n");
    }
    s.push_str("q\nBT\n3 Tr\n");
    let mut size_now = String::new();
    let mut any = false;
    let advance = WIDTH as f64 / 1000.;
    for line in lines {
        let words = place_line(frame, line);
        for (k, w) in words.iter().enumerate() {
            let cs = codes(&w.text);
            if cs.is_empty() || w.size < 0.5 || w.width <= 0.01 {
                continue;
            }
            let size = num(w.size);
            if size != size_now {
                let _ = writeln!(s, "/{font} {size} Tf");
                size_now = size;
            }
            let _ = writeln!(s, "{} {} {} {} {} {} Tm", num(w.along.0), num(w.along.1), num(w.up.0), num(w.up.1), num(w.origin.0), num(w.origin.1));
            let tz = (100. * w.width / (cs.len() as f64 * advance * w.size)).clamp(1., 100_000.);
            let _ = write!(s, "{} Tz\n<", num(tz));
            for c in &cs {
                let _ = write!(s, "{c:04X}");
            }
            s.push_str("> Tj\n");
            // The space after it (but the line's last), stretched over the gap to the next word.
            // Words read touching or overlapping get a narrow one just before the next word (still
            // after this one's start): PDFium puts a line's pieces in order by where they start.
            if let Some(next) = words.get(k + 1) {
                let next_at = (next.origin.0 - w.origin.0) * w.along.0 + (next.origin.1 - w.origin.1) * w.along.1;
                let narrow = 0.1 * w.size;
                let (at, wide) = if next_at - w.width >= narrow { (w.width, next_at - w.width) } else { ((next_at - narrow).max(next_at / 2.), narrow.min(next_at / 2.)) };
                if (at - w.width).abs() > 1e-3 {
                    let o = (w.origin.0 + w.along.0 * at, w.origin.1 + w.along.1 * at);
                    let _ = writeln!(s, "{} {} {} {} {} {} Tm", num(w.along.0), num(w.along.1), num(w.up.0), num(w.up.1), num(o.0), num(o.1));
                }
                let tz = (100. * wide.max(0.01) / (advance * w.size)).clamp(1., 100_000.);
                let _ = writeln!(s, "{} Tz\n<0020> Tj", num(tz));
            }
            any = true;
        }
    }
    s.push_str("ET\nQ\n");
    any.then(|| s.into_bytes())
}

// ───────────────────────────── the font ─────────────────────────────

/// A TrueType font that draws nothing (as Tesseract's pdf.ttf): each glyph half an em wide, ascent
/// 900 and descent 100 of 1000. Built here (about 600 bytes) rather than shipped.
///
/// Unlike Tesseract's, glyph 1 (every character's) isn't empty: its outline is a line from its box's
/// bottom left to its top right and back, which encloses nothing (nothing is filled, even by a
/// reader that ignored render mode 3) but gives it the box [0 −100 500 900]. PDFium (Chrome, Edge)
/// measures text by its glyphs' boxes: with empty glyphs it dropped one-letter words and every
/// turned line (their text objects had no width), and selected text had no highlight.
pub(crate) fn glyphless_ttf() -> Vec<u8> {
    fn be(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    fn be32(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }
    let i = |x: i64| x as i16 as u16;
    let (asc, desc, wid) = (ASCENT, DESCENT, WIDTH);
    let mut head = be32(&[0x0001_0000, 0x0001_0000, 0, 0x5F0F_3CF5]);
    // flags (baseline at 0, lsb at 0, integer scaling), units per em, created and modified (0),
    // the box, mac style, smallest size, direction hint, short loca, glyph format
    head.extend(be(&[0x000B, 1000]));
    head.extend([0u8; 16]);
    head.extend(be(&[0, i(desc), i(wid), i(asc), 0, 8, 2, 0, 0]));
    let mut hhea = be32(&[0x0001_0000]);
    // … advance width max, min left and right side bearings, x max extent …
    hhea.extend(be(&[i(asc), i(desc), 0, i(wid), 0, 0, i(wid), 1, 0, 0, 0, 0, 0, 0, 0, 2]));
    let mut maxp = be32(&[0x0001_0000]);
    // two glyphs, at most 2 points in 1 contour
    maxp.extend(be(&[2, 2, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0]));
    let hmtx = be(&[i(wid), 0, i(wid), 0]);
    // Glyph 0 (.notdef) is empty; glyph 1: one contour (ending at point 1), its box, no
    // instructions, two points on the curve (16-bit coordinates, as deltas).
    let glyf = be(&[1, 0, i(desc), i(wid), i(asc), 1, 0, 0x0101, 0, i(wid), i(desc), i(asc - desc)]);
    let loca = be(&[0, 0, glyf.len() as u16 / 2]);
    // A Windows Unicode cmap: the space is glyph 1, nothing else (PDF readers go by CIDToGIDMap).
    let mut cmap = be(&[0, 1, 3, 1]);
    cmap.extend(be32(&[12]));
    cmap.extend(be(&[4, 32, 0, 4, 4, 1, 0, 0x20, 0xFFFF, 0, 0x20, 0xFFFF, i(1 - 0x20), 1, 0, 0]));
    let names = [(1u16, FONT), (2, "Regular"), (3, FONT), (4, FONT), (5, "Version 1.0"), (6, FONT)];
    let mut name = be(&[0, names.len() as u16, 6 + 12 * names.len() as u16]);
    let mut strings = Vec::new();
    for (id, s) in names {
        let utf16: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        name.extend(be(&[3, 1, 0x409, id, utf16.len() as u16, strings.len() as u16]));
        strings.extend(utf16);
    }
    name.extend(strings);
    let mut os2 = be(&[4, i(wid), 400, 5, 0, 650, 600, 0, 75, 650, 600, 0, 350, 50, 300, 0]);
    os2.extend([0u8; 10]);
    os2.extend(be32(&[1, 0, 0, 0]));
    os2.extend(b"NONE");
    os2.extend(be(&[0x0040, 0x20, 0x20, i(asc), i(desc), 0, i(asc), i(-desc)]));
    os2.extend(be32(&[1, 0]));
    os2.extend(be(&[500, 700, 0, 0x20, 1]));
    let mut post = be32(&[0x0003_0000, 0]);
    post.extend(be(&[i(-100), 50]));
    post.extend(be32(&[1, 0, 0, 0, 0]));
    let tables: [(&[u8; 4], Vec<u8>); 10] =
        [(b"OS/2", os2), (b"cmap", cmap), (b"glyf", glyf), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx), (b"loca", loca), (b"maxp", maxp), (b"name", name), (b"post", post)];
    let sum = |d: &[u8]| -> u32 {
        d.chunks(4).fold(0u32, |acc, c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            acc.wrapping_add(u32::from_be_bytes(b))
        })
    };
    let n = tables.len() as u16;
    let sel = 15 - n.leading_zeros() as u16;
    let mut out = be32(&[0x0001_0000]);
    out.extend(be(&[n, 16 << sel, sel, n * 16 - (16 << sel)]));
    let mut offset = 12 + 16 * tables.len();
    let mut data = Vec::new();
    let mut head_at = 0;
    for (tag, t) in &tables {
        out.extend(*tag);
        out.extend(be32(&[sum(t), offset as u32, t.len() as u32]));
        if *tag == b"head" {
            head_at = offset;
        }
        let padded = t.len().div_ceil(4) * 4;
        data.extend(t);
        data.resize(data.len() + padded - t.len(), 0);
        offset += padded;
    }
    out.extend(data);
    let adjust = 0xB1B0_AFBAu32.wrapping_sub(sum(&out));
    out[head_at + 8..head_at + 12].copy_from_slice(&adjust.to_be_bytes());
    out
}

/// The ToUnicode CMap: each two-byte code is the UTF-16 value it stands for.
fn to_unicode() -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    // A range per high byte (at most 100 to a block), the surrogates left out.
    let ranges: Vec<u32> = (0..=0xFFu32).filter(|h| !(0xD8..=0xDF).contains(h)).collect();
    for block in ranges.chunks(100) {
        let _ = writeln!(s, "{} beginbfrange", block.len());
        for h in block {
            let _ = writeln!(s, "<{h:02X}00> <{h:02X}FF> <{h:02X}00>");
        }
        s.push_str("endbfrange\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn compressed(dict: Dictionary, content: Vec<u8>) -> Object {
    let mut s = Stream::new(dict, content);
    let _ = s.compress();
    Object::Stream(s)
}

/// The font's objects, numbered from `first` (the Type0 font, which pages refer to, is `first`).
fn font_objects(first: u32) -> Vec<(ObjectId, Object)> {
    let id = |k: u32| Object::Reference((first + k, 0));
    let mut type0 = Dictionary::new();
    type0.set("Type", name("Font"));
    type0.set("Subtype", name("Type0"));
    type0.set("BaseFont", name(FONT));
    type0.set("Encoding", name("Identity-H"));
    type0.set("DescendantFonts", Object::Array(vec![id(1)]));
    type0.set("ToUnicode", id(5));
    let mut info = Dictionary::new();
    info.set("Registry", Object::String(b"Adobe".to_vec(), StringFormat::Literal));
    info.set("Ordering", Object::String(b"Identity".to_vec(), StringFormat::Literal));
    info.set("Supplement", Object::Integer(0));
    let mut cid = Dictionary::new();
    cid.set("Type", name("Font"));
    cid.set("Subtype", name("CIDFontType2"));
    cid.set("BaseFont", name(FONT));
    cid.set("CIDSystemInfo", Object::Dictionary(info));
    cid.set("FontDescriptor", id(2));
    cid.set("DW", Object::Integer(WIDTH));
    cid.set("CIDToGIDMap", id(4));
    let mut desc = Dictionary::new();
    desc.set("Type", name("FontDescriptor"));
    desc.set("FontName", name(FONT));
    // fixed pitch, symbolic
    desc.set("Flags", Object::Integer(5));
    desc.set("FontBBox", Object::Array([0, DESCENT, WIDTH, ASCENT].into_iter().map(Object::Integer).collect()));
    desc.set("ItalicAngle", Object::Integer(0));
    desc.set("Ascent", Object::Integer(ASCENT));
    desc.set("Descent", Object::Integer(DESCENT));
    desc.set("CapHeight", Object::Integer(700));
    desc.set("StemV", Object::Integer(80));
    desc.set("FontFile2", id(3));
    let ttf = glyphless_ttf();
    let mut file = Dictionary::new();
    file.set("Length1", Object::Integer(ttf.len() as i64));
    // Every character code (CID) is glyph 1.
    let gids: Vec<u8> = (0..0x10000).flat_map(|_| [0u8, 1u8]).collect();
    vec![
        ((first, 0), Object::Dictionary(type0)),
        ((first + 1, 0), Object::Dictionary(cid)),
        ((first + 2, 0), Object::Dictionary(desc)),
        ((first + 3, 0), compressed(file, ttf)),
        ((first + 4, 0), compressed(Dictionary::new(), gids)),
        ((first + 5, 0), compressed(Dictionary::new(), to_unicode())),
    ]
}

// ───────────────────────────── the page ─────────────────────────────

fn dict_of(doc: &Document, o: &Object) -> Option<Dictionary> {
    match o {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(d) => Some(d.clone()),
        _ => None,
    }
}

/// The page's resources (its own or inherited), as a copy of its own.
fn resources(doc: &Document, page: &Dictionary) -> Dictionary {
    crate::pdftext::inherited(doc, page, b"Resources").and_then(|o| dict_of(doc, o)).unwrap_or_default()
}

/// The page already has a layer like this one (a searchable copy's page, or one from Tesseract or
/// OCRmyPDF): it's left alone.
fn has_layer(doc: &Document, res: &Dictionary) -> bool {
    let Some(fonts) = res.get(b"Font").ok().and_then(|o| dict_of(doc, o)) else { return false };
    fonts.iter().any(|(_, f)| dict_of(doc, f).and_then(|f| f.get(b"BaseFont").ok().and_then(|b| b.as_name().ok()).map(|b| b.ends_with(FONT.as_bytes()))).unwrap_or(false))
}

/// The page's content streams.
fn contents_of(doc: &Document, page: &Dictionary) -> Vec<Object> {
    match page.get(b"Contents") {
        Ok(Object::Reference(id)) => match doc.get_object(*id) {
            Ok(Object::Array(a)) => a.clone(),
            Ok(_) => vec![Object::Reference(*id)],
            Err(_) => Vec::new(),
        },
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

// ───────────────────────────── the file ─────────────────────────────

fn find_from(h: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    h.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// The integer after `key` in a dictionary's text (and the next one, for a reference).
fn ints_after(dict: &[u8], key: &[u8]) -> Vec<u64> {
    let mut at = 0;
    while let Some(p) = find_from(dict, key, at) {
        let rest = &dict[p + key.len()..];
        // `/Encrypt`, not `/EncryptMetadata`
        if rest.first().is_some_and(|c| c.is_ascii_alphanumeric()) {
            at = p + key.len();
            continue;
        }
        // up to two numbers, each ending where its digits do (`/Size 12/Root …`)
        let mut out = Vec::new();
        let mut i = 0;
        while out.len() < 2 {
            while rest.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            let n = rest[i.min(rest.len())..].iter().take_while(|c| c.is_ascii_digit()).count();
            match std::str::from_utf8(&rest[i..i + n]).ok().and_then(|s| s.parse().ok()) {
                Some(v) if n > 0 => out.push(v),
                _ => break,
            }
            i += n;
        }
        return out;
    }
    Vec::new()
}

/// The file's last cross-reference section: where it starts (as `startxref` says), whether it's
/// a stream (PDF 1.5) rather than a table, and its trailer's /Encrypt and /Size.
struct Tail {
    prev: u64,
    stream: bool,
    encrypt: Option<ObjectId>,
    size: u32,
}

fn tail(bytes: &[u8], header: usize) -> Option<Tail> {
    let from = bytes.len().saturating_sub(64 * 1024);
    let sx = bytes[from..].windows(9).rposition(|w| w == b"startxref")? + from;
    let digits: Vec<u8> = bytes[sx + 9..].iter().copied().skip_while(|c| c.is_ascii_whitespace()).take_while(|c| c.is_ascii_digit()).collect();
    let prev: u64 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
    let at = header + prev as usize;
    let rest = bytes.get(at..)?;
    let start = rest.iter().position(|c| !c.is_ascii_whitespace())?;
    let stream = !rest[start..].starts_with(b"xref");
    let dict = if stream {
        &rest[..find_from(rest, b"stream", 0)?]
    } else {
        let t = find_from(rest, b"trailer", 0)?;
        &rest[t..find_from(rest, b"startxref", t).unwrap_or(rest.len())]
    };
    let encrypt = match ints_after(dict, b"/Encrypt")[..] {
        [n, g] => Some((n as u32, g as u16)),
        _ => None,
    };
    let encrypted = find_from(dict, b"/Encrypt", 0).is_some();
    if encrypted && encrypt.is_none() {
        // a direct /Encrypt dictionary: not kept by this writer
        return None;
    }
    Some(Tail { prev, stream, encrypt, size: ints_after(dict, b"/Size").first().copied().unwrap_or(0) as u32 })
}

/// An object as PDF (strings in hex, so their bytes go as they are).
fn write_object(out: &mut Vec<u8>, o: &Object) {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        // The shortest form that reads back as the same number (no exponent in Rust's Display).
        Object::Real(r) => out.extend_from_slice(if r.is_finite() { r.to_string() } else { "0".into() }.as_bytes()),
        Object::Name(n) => {
            out.push(b'/');
            for &b in n {
                if (0x21..=0x7E).contains(&b) && !b"#()<>[]{}/%".contains(&b) {
                    out.push(b);
                } else {
                    out.extend_from_slice(format!("#{b:02X}").as_bytes());
                }
            }
        }
        Object::String(s, _) => {
            out.push(b'<');
            for b in s {
                out.extend_from_slice(format!("{b:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Array(a) => {
            out.push(b'[');
            for (k, o) in a.iter().enumerate() {
                if k > 0 {
                    out.push(b' ');
                }
                write_object(out, o);
            }
            out.push(b']');
        }
        Object::Dictionary(d) => write_dict(out, d),
        Object::Stream(s) => {
            let mut d = s.dict.clone();
            d.set("Length", Object::Integer(s.content.len() as i64));
            write_dict(out, &d);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&s.content);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Reference((n, g)) => out.extend_from_slice(format!("{n} {g} R").as_bytes()),
    }
}

fn write_dict(out: &mut Vec<u8>, d: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        write_object(out, &Object::Name(k.clone()));
        out.push(b' ');
        write_object(out, v);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

/// Picture data isn't needed to add text (only the pages' dictionaries are): left out of what's
/// loaded, so a big scan isn't held twice. (Objects in object streams are never streams.)
fn without_pictures(id: ObjectId, o: &mut Object) -> Option<(ObjectId, Object)> {
    if let Object::Stream(s) = o {
        if s.dict.get(b"Subtype").ok().and_then(|n| n.as_name().ok()) == Some(b"Image".as_slice()) {
            // not empty: lopdf reads empty streams again
            s.content = vec![0];
        }
    }
    Some((id, o.clone()))
}

/// The incremental update giving the PDF in `bytes` its pages' text (`pages`: by page index, from
/// 0, as ocr.rs reads them), and how many pages got text. `password`: the one that opened it.
pub(crate) fn update(bytes: &[u8], pages: &BTreeMap<u32, Arc<Vec<PdfLine>>>, password: Option<&str>) -> Result<(Vec<u8>, usize), String> {
    let header = bytes[..bytes.len().min(1024)].windows(5).position(|w| w == b"%PDF-").ok_or("it isn't a PDF")?;
    let tail = tail(bytes, header).ok_or("its structure couldn't be read")?;
    let opts = pdf_extract::LoadOptions { password: password.map(str::to_string), filter: Some(without_pictures), strict: false };
    let doc = Document::load_mem_with_options(bytes, opts).map_err(|e| format!("it couldn't be read ({e})"))?;
    let state = doc.encryption_state.clone();
    match (&state, tail.encrypt) {
        (Some(_), None) => return Err("its encryption couldn't be kept".into()),
        (None, Some(_)) => return Err("it needs its password (open it in the preview first)".into()),
        _ => {}
    }
    let root = doc.trailer.get(b"Root").ok().cloned().ok_or("it has no catalog")?;
    let page_ids = doc.get_pages();
    let mut next = (doc.max_id + 1).max(tail.size);
    let font = next;
    next += 6;
    let q = (next, 0);
    next += 1;
    let mut objects: Vec<(ObjectId, Object)> = font_objects(font);
    objects.push((q, Object::Stream(Stream::new(Dictionary::new(), b"q\n".to_vec()))));
    let mut done = 0;
    for (&i, lines) in pages {
        let Some(&page_id) = page_ids.get(&(i + 1)) else { continue };
        let Ok(page) = doc.get_dictionary(page_id) else { continue };
        let mut res = resources(&doc, page);
        if has_layer(&doc, &res) {
            continue;
        }
        let Some(frame) = Frame::of_page(&doc, page) else { continue };
        let mut fonts = res.get(b"Font").ok().and_then(|o| dict_of(&doc, o)).unwrap_or_default();
        let fname = (0..).map(|k| if k == 0 { FONT.to_string() } else { format!("{FONT}{k}") }).find(|n| !fonts.has(n.as_bytes())).unwrap_or_default();
        let contents = contents_of(&doc, page);
        let Some(layer) = text_layer(&frame, lines, &fname, !contents.is_empty()) else { continue };
        fonts.set(fname.as_bytes().to_vec(), Object::Reference((font, 0)));
        res.set("Font", Object::Dictionary(fonts));
        if let Ok(Object::Array(sets)) = res.get_mut(b"ProcSet") {
            if !sets.contains(&name("Text")) {
                sets.push(name("Text"));
            }
        }
        let layer_id = (next, 0);
        next += 1;
        objects.push((layer_id, compressed(Dictionary::new(), layer)));
        let mut page = page.clone();
        let mut all = Vec::new();
        if !contents.is_empty() {
            all.push(Object::Reference(q));
            all.extend(contents);
        }
        all.push(Object::Reference(layer_id));
        page.set("Contents", Object::Array(all));
        page.set("Resources", Object::Dictionary(res));
        objects.push((page_id, Object::Dictionary(page)));
        done += 1;
    }
    if done == 0 {
        return Err("no page had recognised text to add".into());
    }
    if let Some(state) = &state {
        for (id, o) in objects.iter_mut() {
            pdf_extract::encryption::encrypt_object(state, *id, o).map_err(|e| format!("its encryption couldn't be kept ({e})"))?;
        }
    }
    // Offsets count from the header, as the file's own do.
    let base = bytes.len() - header;
    let mut out = Vec::new();
    if !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
        out.push(b'\n');
    }
    let mut xref: Vec<(ObjectId, usize)> = Vec::new();
    for (id, o) in &objects {
        xref.push((*id, base + out.len()));
        out.extend_from_slice(format!("{} {} obj\n", id.0, id.1).as_bytes());
        write_object(&mut out, o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let mut trailer = Dictionary::new();
    trailer.set("Root", root);
    for key in [b"Info".as_slice(), b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            trailer.set(key.to_vec(), v.clone());
        }
    }
    if let Some(e) = tail.encrypt {
        trailer.set("Encrypt", Object::Reference(e));
    }
    trailer.set("Prev", Object::Integer(tail.prev as i64));
    // Subsections of consecutive object numbers.
    let sections = |xref: &mut Vec<(ObjectId, usize)>| -> Vec<(u32, Vec<(ObjectId, usize)>)> {
        xref.sort_by_key(|(id, _)| id.0);
        let mut out: Vec<(u32, Vec<(ObjectId, usize)>)> = Vec::new();
        for &(id, off) in xref.iter() {
            match out.last_mut() {
                Some((start, v)) if *start + v.len() as u32 == id.0 => v.push((id, off)),
                _ => out.push((id.0, vec![(id, off)])),
            }
        }
        out
    };
    let xref_at = base + out.len();
    if tail.stream {
        // A cross-reference stream, as the file's own last section is.
        let xid = next;
        next += 1;
        xref.push(((xid, 0), xref_at));
        let w = if xref_at > u32::MAX as usize { 8 } else { 4 };
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (start, v) in sections(&mut xref) {
            index.push(Object::Integer(start as i64));
            index.push(Object::Integer(v.len() as i64));
            for (id, off) in v {
                data.push(1u8);
                data.extend_from_slice(&(off as u64).to_be_bytes()[8 - w..]);
                data.extend_from_slice(&id.1.to_be_bytes());
            }
        }
        trailer.set("Type", name("XRef"));
        trailer.set("Size", Object::Integer(next as i64));
        trailer.set("Index", Object::Array(index));
        trailer.set("W", Object::Array(vec![Object::Integer(1), Object::Integer(w as i64), Object::Integer(2)]));
        out.extend_from_slice(format!("{xid} 0 obj\n").as_bytes());
        write_object(&mut out, &Object::Stream(Stream::new(trailer, data)));
        out.extend_from_slice(b"\nendobj\n");
    } else {
        out.extend_from_slice(b"xref\n");
        for (start, v) in sections(&mut xref) {
            out.extend_from_slice(format!("{start} {}\n", v.len()).as_bytes());
            for (id, off) in v {
                out.extend_from_slice(format!("{off:010} {:05} n\r\n", id.1).as_bytes());
            }
        }
        trailer.set("Size", Object::Integer(next as i64));
        out.extend_from_slice(b"trailer\n");
        write_dict(&mut out, &trailer);
        out.push(b'\n');
    }
    out.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    Ok((out, done))
}

/// Writes `<name> (searchable).pdf` beside the PDF at `path` (numbered if that's taken): the PDF
/// with its recognised pages' text. The new file's path and how many pages got text.
pub fn save_copy(path: &str, pages: &BTreeMap<u32, Arc<Vec<PdfLine>>>, password: Option<&str>) -> Result<(String, usize), String> {
    use std::io::Write;
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let (more, n) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| update(&bytes, pages, password))).map_err(|_| "it couldn't be read".to_string())??;
    let dir = util::parent(path).ok_or("it has no folder")?;
    let stem = util::file_name(path);
    let stem = stem.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(stem);
    for _ in 0..16 {
        let file = util::unique_name(&dir, &format!("{stem} (searchable).pdf"));
        let dest = format!("{}\\{file}", dir.trim_end_matches('\\'));
        let mut f = match std::fs::File::create_new(&dest) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        };
        let wrote = f.write_all(&bytes).and_then(|_| f.write_all(&more)).and_then(|_| f.sync_all());
        drop(f);
        return match wrote {
            Ok(()) => Ok((dest, n)),
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                Err(e.to_string())
            }
        };
    }
    Err("no free name for the copy".into())
}

// ───────────────────────────── in the app ─────────────────────────────

/// Files a copy is being written of (by path, without case).
static SAVING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

/// A searchable copy of this file is being written.
pub fn saving(path: &str) -> bool {
    SAVING.lock().is_ok_and(|s| s.contains(&path.to_lowercase()))
}

impl Ferret {
    /// "Save as searchable PDF" (the scanned PDF note's button, its file's menu): the copy is
    /// written in the background, then a toast says so, with Show (it selected in its folder).
    pub fn save_searchable_pdf(&mut self, path: String, cx: &mut Context<Self>) {
        let pages = crate::ocr::recognised(&path);
        if pages.is_empty() {
            self.toast_error("No text was recognised in this PDF this session", cx);
            return;
        }
        if !SAVING.lock().map(|mut s| s.insert(path.to_lowercase())).unwrap_or(false) {
            return;
        }
        cx.notify();
        let password = crate::pdftext::password_for(&path);
        let p = path.clone();
        let task = cx.background_executor().spawn(async move { save_copy(&p, &pages, password.as_deref()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if let Ok(mut s) = SAVING.lock() {
                s.remove(&path.to_lowercase());
            }
            this.update(cx, |f, cx| {
                match result {
                    Ok((dest, n)) => {
                        crate::watchdog::act(format!("saved searchable PDF {dest} (text on {n} page{})", if n == 1 { "" } else { "s" }));
                        // a protected file's copy opens with the same password
                        if let Some(p) = crate::pdftext::password_for(&path) {
                            crate::pdftext::remember(&dest, &p);
                        }
                        let shown = util::file_name(&dest);
                        f.toast_action(format!("Saved {shown}"), "Show", action(move |f, _, cx| f.reveal(&dest, cx)), cx);
                    }
                    Err(e) => {
                        crate::watchdog::act(format!("couldn't save searchable PDF of {path}: {e}"));
                        f.toast_error(format!("Couldn't save a searchable copy: {e}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdftext::Turn;

    fn line(text: &str, left: f32, right: f32, top: f32, bottom: f32) -> PdfLine {
        // characters spread evenly over the line, as Windows' reader gives them
        let n = text.chars().count() as f32;
        let mut chars = Vec::new();
        for (k, (off, _)) in text.char_indices().enumerate() {
            let k = k as f32;
            chars.push((off, left + (right - left) * k / n, left + (right - left) * (k + 1.) / n));
        }
        PdfLine { text: text.into(), chars, left, right, top, bottom, row_goes_on: false, turn: None }
    }

    /// A line turned by `angle` (radians, clockwise as shown) on a page `aspect` widths tall,
    /// starting at `start` along its frame and `top`–`bottom` across (page widths).
    fn turned(text: &str, angle: f32, aspect: f32, start: f32, end: f32, top: f32, bottom: f32) -> PdfLine {
        let turn = Turn { angle, aspect, start, end, top, bottom };
        let mut l = line(text, start, end, 0., 0.);
        (l.left, l.right, l.top, l.bottom) = turn.page_box();
        l.turn = Some(turn);
        l
    }

    #[test]
    fn the_font_is_a_real_truetype_font() {
        let ttf = glyphless_ttf();
        let face = ttf_parser::Face::parse(&ttf, 0).expect("parses");
        assert_eq!(face.number_of_glyphs(), 2);
        assert_eq!(face.units_per_em(), 1000);
        assert_eq!(face.ascender(), 900);
        assert_eq!(face.descender(), -100);
        assert_eq!(face.glyph_hor_advance(ttf_parser::GlyphId(1)), Some(500));
        assert_eq!(face.glyph_index(' '), Some(ttf_parser::GlyphId(1)));
        assert!(face.outline_glyph(ttf_parser::GlyphId(0), &mut Points(Vec::new())).is_none(), ".notdef is empty");
        // glyph 1: the em box, and an outline that encloses nothing
        let mut pts = Points(Vec::new());
        let b = face.outline_glyph(ttf_parser::GlyphId(1), &mut pts).expect("a box");
        assert_eq!((b.x_min, b.y_min, b.x_max, b.y_max), (0, -100, 500, 900));
        let p = &pts.0;
        let area: f32 = (0..p.len()).map(|k| p[k].0 * p[(k + 1) % p.len()].1 - p[(k + 1) % p.len()].0 * p[k].1).sum();
        assert_eq!(area, 0., "{p:?}");
        // the whole font's checksum comes out as the spec's magic number
        let sum = ttf.chunks(4).fold(0u32, |a, c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            a.wrapping_add(u32::from_be_bytes(b))
        });
        assert_eq!(sum, 0xB1B0_AFBA);
    }

    /// An outline's points (it has only straight lines).
    struct Points(Vec<(f32, f32)>);
    impl ttf_parser::OutlineBuilder for Points {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
            panic!("curves")
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
            panic!("curves")
        }
        fn close(&mut self) {}
    }

    #[test]
    fn unplace_undoes_place_for_every_turn() {
        for rotate in [0, 90, 180, 270, -90, 450] {
            let f = Frame { bx: [10., 20., 610., 820.], rotate };
            for (x, y) in [(10., 20.), (610., 820.), (100., 700.), (333.3, 21.5)] {
                let (u, v) = f.place(x, y);
                let (x2, y2) = f.unplace(u as f64, v as f64);
                assert!((x - x2).abs() < 1e-3 && (y - y2).abs() < 1e-3, "rotate {rotate}: ({x}, {y}) → ({u}, {v}) → ({x2}, {y2})");
            }
        }
        // Turned a quarter clockwise, the page's bottom left shows at the top left; its size swaps.
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 90 };
        assert_eq!(f.unplace(0., 0.), (0., 0.));
        assert_eq!(f.unplace(1., 0.), (0., 800.));
        assert_eq!(f.shown_size(), (800., 600.));
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 270 };
        assert_eq!(f.unplace(0., 0.), (600., 800.));
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 180 };
        assert_eq!(f.unplace(0., 0.), (600., 0.));
    }

    #[test]
    fn words_are_placed_where_they_were_read() {
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 0 };
        // "Hello world" across 0.1–0.5 of the width, 0.25–0.27 down
        let l = line("Hello world", 0.1, 0.5, 0.25, 0.27);
        let w = place_line(&f, &l);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text, "Hello");
        let size = 0.02 * 800.;
        assert!((w[0].size - size).abs() < 1e-3);
        // the baseline a tenth of the line up from its bottom
        let base = 800. - (0.27 * 800. - 0.1 * size);
        assert!((w[0].origin.0 - 60.).abs() < 1e-3 && (w[0].origin.1 - base).abs() < 1e-3, "{:?}", w[0].origin);
        assert_eq!((w[0].along, w[0].up), ((1., 0.), (0., 1.)));
        // 5 of 11 characters wide; the next word after the space's width
        assert!((w[0].width - 240. * 5. / 11.).abs() < 1e-3);
        assert!((w[1].origin.0 - (60. + 240. * 6. / 11.)).abs() < 1e-3);

        // The same page turned a quarter clockwise: the text runs up the unturned page.
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 90 };
        let w = place_line(&f, &l);
        assert!((w[0].along.0).abs() < 1e-9 && (w[0].along.1 - 1.).abs() < 1e-9);
        assert!((w[0].up.0 + 1.).abs() < 1e-9 && w[0].up.1.abs() < 1e-9);
        let (u, v) = f.place(w[0].origin.0, w[0].origin.1);
        assert!((u - 0.1).abs() < 1e-5 && (v - (0.27 - 0.002)).abs() < 1e-5, "({u}, {v})");

        // A line written bottom to top (turned -90° as shown) on an upright page.
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 0 };
        let aspect = 800. / 600.;
        let l = turned("Up", -std::f32::consts::FRAC_PI_2, aspect, -0.9, -0.5, 0.28, 0.3);
        let w = place_line(&f, &l);
        assert_eq!(w.len(), 1);
        // runs up the page, its top to the left
        assert!(w[0].along.0.abs() < 1e-6 && (w[0].along.1 - 1.).abs() < 1e-6, "{:?}", w[0].along);
        assert!((w[0].up.0 + 1.).abs() < 1e-6 && w[0].up.1.abs() < 1e-6, "{:?}", w[0].up);
        assert!((w[0].width - 0.4 * 600.).abs() < 1e-3 && (w[0].size - 0.02 * 600.).abs() < 1e-3);
    }

    #[test]
    fn the_layer_is_invisible_text_in_the_font() {
        let f = Frame { bx: [0., 0., 600., 800.], rotate: 0 };
        let s = String::from_utf8(text_layer(&f, &[line("Brașov 424.44", 0.1, 0.5, 0.25, 0.27), line("Total", 0.1, 0.2, 0.3, 0.32)], "GlyphLessFont", true).unwrap()).unwrap();
        assert!(s.starts_with("Q\nq\nBT\n3 Tr\n/GlyphLessFont 16 Tf\n1 0 0 1 60 585.6 Tm\n"), "{s}");
        assert!(s.ends_with("ET\nQ\n"));
        // a word to a Tj, each as wide as it was read (6 of the 13 characters' 240 points: 6
        // glyphs half of 16 wide at 100%), a space between the words only
        let tjs: Vec<&str> = s.lines().filter(|l| l.ends_with("> Tj")).collect();
        assert_eq!(tjs, ["<0042007200610219006F0076> Tj", "<0020> Tj", "<003400320034002E00340034> Tj", "<0054006F00740061006C> Tj"], "{s}");
        let tz = 100. * (240. * 6. / 13.) / (6. * 8.);
        assert!(s.contains(&format!("{} Tz\n<0042", num(tz))), "{s}");
        // words read overlapping: the space between them still starts before the second
        let mut l = line("WA.", 0.1, 0.13, 0.25, 0.27);
        l.text = "W A.".into();
        l.chars = vec![(0, 0.1, 0.112), (1, 0.112, 0.112), (2, 0.11, 0.12), (3, 0.12, 0.13)];
        let s = String::from_utf8(text_layer(&f, &[l], "F", false).unwrap()).unwrap();
        let tms: Vec<f64> = s.lines().filter(|l| l.ends_with(" Tm")).map(|l| l.split(' ').nth(4).unwrap().parse().unwrap()).collect();
        assert_eq!(tms.len(), 3, "{s}");
        assert!(tms[0] < tms[1] && tms[1] < tms[2], "{tms:?}");
        assert!(text_layer(&f, &[], "F", false).is_none());
        assert_eq!(num(1.23456), "1.235");
        assert_eq!(num(-0.0001), "0");
        assert_eq!(num(100.), "100");
    }

    /// A one-page PDF with `rotate`, a crop box and some content (a grey box), written by lopdf.
    fn sample(rotate: i64, objstm: bool, encrypt: Option<u8>) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        // a classic cross-reference table, unless it's to have object streams (lopdf's default is
        // a cross-reference stream)
        if !objstm {
            doc.reference_table.cross_reference_type = pdf_extract::xref::XrefType::CrossReferenceTable;
        }
        let content = doc.add_object(Stream::new(Dictionary::new(), b"q 0.5 g 50 60 100 80 re f Q 0.9 g 0 0 10 10 re f".to_vec()));
        let res = doc.add_object(Dictionary::from_iter([("ProcSet", Object::Array(vec![name("PDF")]))]));
        let pages = doc.new_object_id();
        let mut page = Dictionary::new();
        page.set("Type", name("Page"));
        page.set("Parent", Object::Reference(pages));
        page.set("Contents", Object::Reference(content));
        page.set("Resources", Object::Reference(res));
        page.set("CropBox", Object::Array([20., 30., 580., 790.].into_iter().map(Object::Real).collect()));
        page.set("Rotate", Object::Integer(rotate));
        let page = doc.add_object(page);
        // a second page sharing the resources, left without text
        let mut other = Dictionary::new();
        other.set("Type", name("Page"));
        other.set("Parent", Object::Reference(pages));
        other.set("Contents", Object::Reference(content));
        let other = doc.add_object(other);
        let mut tree = Dictionary::new();
        tree.set("Type", name("Pages"));
        tree.set("Kids", Object::Array(vec![Object::Reference(page), Object::Reference(other)]));
        tree.set("Count", Object::Integer(2));
        tree.set("MediaBox", Object::Array([0, 0, 600, 800].into_iter().map(Object::Integer).collect()));
        tree.set("Resources", Object::Reference(res));
        doc.objects.insert(pages, Object::Dictionary(tree));
        let catalog = doc.add_object(Dictionary::from_iter([("Type", name("Catalog")), ("Pages", Object::Reference(pages))]));
        doc.trailer.set("Root", Object::Reference(catalog));
        doc.trailer.set("ID", Object::Array(vec![Object::String(b"0123456789abcdef".to_vec(), StringFormat::Hexadecimal); 2]));
        if let Some(v) = encrypt {
            use pdf_extract::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
            let version = match v {
                2 => pdf_extract::EncryptionVersion::V2 { document: &doc, owner_password: "owner", user_password: "secret", key_length: 128, permissions: Default::default() },
                4 => {
                    let cf: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
                    pdf_extract::EncryptionVersion::V4 {
                        document: &doc,
                        encrypt_metadata: true,
                        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), cf)]),
                        stream_filter: b"StdCF".to_vec(),
                        string_filter: b"StdCF".to_vec(),
                        owner_password: "owner",
                        user_password: "secret",
                        permissions: Default::default(),
                    }
                }
                _ => {
                    let cf: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
                    pdf_extract::EncryptionVersion::V5 {
                        encrypt_metadata: true,
                        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), cf)]),
                        file_encryption_key: &[7u8; 32],
                        stream_filter: b"StdCF".to_vec(),
                        string_filter: b"StdCF".to_vec(),
                        owner_password: "owner",
                        user_password: "secret",
                        permissions: Default::default(),
                    }
                }
            };
            let state = pdf_extract::EncryptionState::try_from(version).unwrap();
            doc.encrypt(&state).unwrap();
        }
        let mut out = Vec::new();
        if objstm {
            doc.save_modern(&mut out).unwrap();
        } else {
            doc.save_to(&mut out).unwrap();
        }
        out
    }

    /// What reading the sample's first page might give; `aspect`: its height over its width as shown.
    fn ocr_page(aspect: f32) -> BTreeMap<u32, Arc<Vec<PdfLine>>> {
        BTreeMap::from([(
            0,
            Arc::new(vec![
                line("Fresh Bakery Ltd", 0.1, 0.6, 0.1, 0.13),
                line("Total 424.44", 0.5, 0.9, 0.5, 0.52),
                line("Brașov, România", 0.2, 0.7, 0.8, 0.82),
                // a line written up the page (as shown), from 0.85 of the way down to 0.55
                turned("Sideways", -std::f32::consts::FRAC_PI_2, aspect, -0.85 * aspect, -0.55 * aspect, 0.88, 0.92),
            ]),
        )])
    }

    /// The copy's text, read back by Ferret's own reader (pdf-extract, a different code path
    /// from the writer's), lands where it was read.
    fn check_reads_back(bytes: &[u8], password: Option<&str>, rotate: i64) {
        // the sample's crop box is 560 × 760
        let pages = ocr_page(if rotate % 180 == 0 { 760. / 560. } else { 560. / 760. });
        let frame_rotated = rotate;
        let (more, n) = update(bytes, &pages, password).unwrap();
        assert_eq!(n, 1);
        let mut copy = bytes.to_vec();
        copy.extend(&more);
        // the original, byte for byte, then the update
        assert!(copy.starts_with(bytes));
        let text = crate::pdftext::extract_mem(&copy, password, 10).expect("reads");
        assert_eq!(text.pages.len(), 2);
        assert!(text.pages[1].is_empty(), "the other page has no text");
        let got: Vec<&str> = text.pages[0].iter().map(|l| l.text.as_str()).collect();
        for want in ["Fresh Bakery Ltd", "Total 424.44", "Brașov, România", "Sideways"] {
            assert!(got.contains(&want), "{want:?} in {got:?} (rotated {frame_rotated})");
        }
        for l in ["Fresh Bakery Ltd", "Total 424.44", "Brașov, România"] {
            let back = text.pages[0].iter().find(|x| x.text == l).unwrap();
            let orig = pages[&0].iter().find(|x| x.text == l).unwrap();
            // within a percent of the page across, a quarter of the line up and down
            let h = orig.bottom - orig.top;
            assert!((back.left - orig.left).abs() < 0.01 && (back.right - orig.right).abs() < 0.01, "{l}: {} {} vs {} {}", back.left, back.right, orig.left, orig.right);
            assert!(((back.top + back.bottom) / 2. - (orig.top + orig.bottom) / 2.).abs() < 0.25 * h, "{l}: {} {} vs {} {}", back.top, back.bottom, orig.top, orig.bottom);
            // "424.44": each character where it was read
            if let Some(at) = back.text.find("424.44") {
                let k = back.chars.iter().position(|c| c.0 == at).unwrap();
                let ok = orig.chars.iter().position(|c| c.0 == orig.text.find("424.44").unwrap()).unwrap();
                assert!((back.chars[k].1 - orig.chars[ok].1).abs() < 0.005);
            }
        }
        let side = text.pages[0].iter().find(|x| x.text == "Sideways").unwrap();
        let t = side.turn.expect("turned");
        assert!((t.angle + std::f32::consts::FRAC_PI_2).abs() < 0.01, "{}", t.angle);
        let orig = pages[&0].iter().find(|x| x.text == "Sideways").unwrap();
        assert!((side.left - orig.left).abs() < 0.02 && (side.top - orig.top).abs() < 0.02 && (side.bottom - orig.bottom).abs() < 0.02, "{side:?} vs {orig:?}");
    }

    #[test]
    fn copies_read_back_on_every_turn_and_crop() {
        for rotate in [0, 90, 180, 270] {
            check_reads_back(&sample(rotate, false, None), None, rotate);
        }
        // cross-reference and object streams (PDF 1.5)
        check_reads_back(&sample(90, true, None), None, 90);
        let bytes = sample(0, true, None);
        assert!(tail(&bytes, 0).unwrap().stream);
        assert!(!tail(&sample(0, false, None), 0).unwrap().stream);
    }

    #[test]
    fn the_update_leaves_shared_resources_alone() {
        let bytes = sample(0, false, None);
        let (more, _) = update(&bytes, &ocr_page(760. / 560.), None).unwrap();
        let mut copy = bytes.clone();
        copy.extend(&more);
        let doc = Document::load_mem(&copy).unwrap();
        let pages = doc.get_pages();
        let p1 = doc.get_dictionary(pages[&1]).unwrap();
        let p2 = doc.get_dictionary(pages[&2]).unwrap();
        // the first page has its own resources, with the font and the shared ones' entries
        let res = p1.get(b"Resources").unwrap().as_dict().unwrap();
        assert!(res.get(b"Font").unwrap().as_dict().unwrap().has(b"GlyphLessFont"));
        assert!(res.has(b"ProcSet"));
        // the shared resources (and the second page, inheriting them) are as they were
        let shared = doc.get_dictionary(doc.get_dictionary(pages[&1]).unwrap().get(b"Parent").unwrap().as_reference().unwrap()).unwrap();
        let shared = doc.get_dictionary(shared.get(b"Resources").unwrap().as_reference().unwrap()).unwrap();
        assert!(!shared.has(b"Font"));
        assert!(!p2.has(b"Resources"));
        // q before the page's own content, the text after
        let contents = p1.get(b"Contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 3);
        // saved again from the copy: its pages already have text, nothing is added
        assert!(update(&copy, &ocr_page(760. / 560.), None).is_err());
    }

    #[test]
    fn protected_copies_stay_protected() {
        for v in [2, 4, 5] {
            let bytes = sample(0, false, Some(v));
            assert!(update(&bytes, &ocr_page(760. / 560.), None).is_err(), "V{v}: no password");
            check_reads_back(&bytes, Some("secret"), 0);
            let (more, _) = update(&bytes, &ocr_page(760. / 560.), Some("secret")).unwrap();
            let mut copy = bytes.clone();
            copy.extend(&more);
            // the copy wants the password: without it (or with a wrong one) there's nothing to read
            assert!(crate::pdftext::extract_mem(&copy, None, 10).is_none(), "V{v}: needs its password");
            assert!(crate::pdftext::extract_mem(&copy, Some("wrong"), 10).is_none());
            // the new section points at the file's own /Encrypt
            let t = tail(&copy, 0).unwrap();
            assert_eq!(t.encrypt, tail(&bytes, 0).unwrap().encrypt);
        }
    }

    /// End to end on real scans (ignored: they need the files, Windows' text recognition and, for
    /// the independent reader, PDFium). FERRET_QA_SCANS: a folder of test scans, where the
    /// fixtures and the copies are written; PDFIUM_DLL: a pdfium.dll to read the copies with.
    ///
    ///   cargo test --bin ferret-gpui qa_ -- --ignored --nocapture --test-threads 1
    mod qa {
        use super::*;
        use std::ffi::c_void;

        fn dir() -> String {
            std::env::var("FERRET_QA_SCANS").expect("FERRET_QA_SCANS: the folder of test scans")
        }

        /// Test files made from the scans: turned and cropped pages, password-protected copies,
        /// one with object and cross-reference streams.
        #[test]
        #[ignore]
        fn qa_make_fixtures() {
            let d = dir();
            for rotate in [90, 180, 270] {
                let mut doc = Document::load(format!("{d}\\invoice.pdf")).unwrap();
                let id = doc.get_pages()[&1];
                let page = doc.get_dictionary_mut(id).unwrap();
                page.set("Rotate", Object::Integer(rotate));
                page.set("CropBox", Object::Array([12., 20., 590., 830.].into_iter().map(Object::Real).collect()));
                doc.reference_table.cross_reference_type = pdf_extract::xref::XrefType::CrossReferenceTable;
                doc.save(format!("{d}\\invoice-rot{rotate}.pdf")).unwrap();
            }
            let mut doc = Document::load(format!("{d}\\multi.pdf")).unwrap();
            let mut f = std::fs::File::create(format!("{d}\\multi-xrefstream.pdf")).unwrap();
            doc.save_modern(&mut f).unwrap();
            for (src, out, v) in [("invoice", "invoice-locked-aes", 4), ("ro_scanner", "ro-locked-rc4", 2)] {
                use pdf_extract::encryption::crypt_filters::{Aes128CryptFilter, CryptFilter};
                let mut doc = Document::load(format!("{d}\\{src}.pdf")).unwrap();
                doc.trailer.set("ID", Object::Array(vec![Object::String(b"ferret-qa-scan-1".to_vec(), StringFormat::Hexadecimal); 2]));
                let cf: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
                let version = if v == 4 {
                    pdf_extract::EncryptionVersion::V4 {
                        document: &doc,
                        encrypt_metadata: true,
                        crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), cf)]),
                        stream_filter: b"StdCF".to_vec(),
                        string_filter: b"StdCF".to_vec(),
                        owner_password: "qa-owner",
                        user_password: "scan-pass",
                        permissions: Default::default(),
                    }
                } else {
                    pdf_extract::EncryptionVersion::V2 { document: &doc, owner_password: "qa-owner", user_password: "scan-pass", key_length: 128, permissions: Default::default() }
                };
                let state = pdf_extract::EncryptionState::try_from(version).unwrap();
                doc.encrypt(&state).unwrap();
                doc.reference_table.cross_reference_type = pdf_extract::xref::XrefType::CrossReferenceTable;
                doc.save(format!("{d}\\{out}.pdf")).unwrap();
            }
        }

        /// The text layer written on a copy's first page (QA_DUMP: the copy's file name).
        #[test]
        #[ignore]
        fn qa_dump() {
            let path = format!("{}\\{}", dir(), std::env::var("QA_DUMP").unwrap());
            let doc = Document::load(&path).unwrap();
            let page = doc.get_dictionary(doc.get_pages()[&1]).unwrap();
            println!("page: {:?}", page);
            let last = page.get(b"Contents").unwrap().as_array().unwrap().last().unwrap().as_reference().unwrap();
            let s = doc.get_object(last).unwrap().as_stream().unwrap().decompressed_content().unwrap();
            println!("{}", String::from_utf8_lossy(&s[..s.len().min(3000)]));
        }

        /// A page drawn by Windows.Data.Pdf as render.rs draws pages: RGBA, `scale` × its size.
        fn render_all(path: &str, password: Option<&str>, scale: f32) -> Result<Vec<(u32, u32, Vec<u8>)>, String> {
            use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
            use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
            use windows::core::HSTRING;
            let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
            let err = |e: windows::core::Error| e.message().to_string();
            let file = windows::Storage::StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).map_err(err)?.join().map_err(err)?;
            let doc = match password {
                Some(p) => PdfDocument::LoadFromFileWithPasswordAsync(&file, &HSTRING::from(p)).map_err(err)?.join().map_err(err)?,
                None => PdfDocument::LoadFromFileAsync(&file).map_err(err)?.join().map_err(err)?,
            };
            let mut out = Vec::new();
            for i in 0..doc.PageCount().map_err(err)? {
                let page = doc.GetPage(i).map_err(err)?;
                let size = page.Size().map_err(err)?;
                let opts = PdfPageRenderOptions::new().map_err(err)?;
                opts.SetDestinationWidth((size.Width * scale).round() as u32).map_err(err)?;
                opts.SetDestinationHeight((size.Height * scale).round() as u32).map_err(err)?;
                let stream = InMemoryRandomAccessStream::new().map_err(err)?;
                page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(err)?.join().map_err(err)?;
                let n = stream.Size().map_err(err)? as u32;
                stream.Seek(0).map_err(err)?;
                let reader = DataReader::CreateDataReader(&stream).map_err(err)?;
                reader.LoadAsync(n).map_err(err)?.join().map_err(err)?;
                let mut png = vec![0u8; n as usize];
                reader.ReadBytes(&mut png).map_err(err)?;
                let img = image::load_from_memory(&png).map_err(|e| e.to_string())?.to_rgba8();
                out.push((img.width(), img.height(), img.into_raw()));
            }
            Ok(out)
        }

        /// Pages read with OCR as the preview reads them (those with almost no text of their own).
        fn read(path: &str) -> BTreeMap<u32, Arc<Vec<PdfLine>>> {
            let base = crate::pdftext::extract(path, 200).expect("reads");
            let pages: Vec<u32> = (0..base.pages.len()).filter(|&i| base.pages[i].iter().map(|l| l.text.trim().chars().count()).sum::<usize>() < 20).map(|i| i as u32).collect();
            let run = Arc::new(crate::ocr::Run { cancel: Default::default(), near: Default::default() });
            let (tx, rx) = std::sync::mpsc::channel();
            crate::ocr::read_pages(path.to_string(), pages.clone(), run, move |p, r| {
                let _ = tx.send((p, r));
            });
            let mut out = BTreeMap::new();
            for (p, r) in rx {
                match r {
                    Ok((reader, lines)) => {
                        println!("  page {} read by {reader:?}: {} lines", p + 1, lines.len());
                        if !lines.is_empty() {
                            out.insert(p, lines);
                        }
                    }
                    Err(e) => println!("  page {} couldn't be read: {e}", p + 1),
                }
            }
            out
        }

        /// Where `word` is on a page (the first line it's in): its box, as fractions of the page shown.
        fn find(lines: &[PdfLine], word: &str) -> Option<(f32, f32, f32, f32)> {
            for l in lines {
                let Some(at) = l.text.find(word) else { continue };
                let k0 = l.chars.iter().position(|c| c.0 >= at)?;
                let k1 = l.chars.iter().rposition(|c| c.0 < at + word.len())?;
                let (a0, a1) = (l.chars[k0].1, l.chars[k1].2);
                return Some(match l.turn {
                    None => (a0, a1, l.top, l.bottom),
                    Some(t) => {
                        let pts = [(a0, t.top), (a1, t.top), (a0, t.bottom), (a1, t.bottom)].map(|(a, c)| t.to_page(a, c));
                        let xs = pts.map(|p| p.0);
                        let ys = pts.map(|p| p.1);
                        (xs.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max), ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max))
                    }
                });
            }
            None
        }

        fn words(lines: &[PdfLine]) -> Vec<String> {
            lines.iter().flat_map(|l| l.text.split_whitespace().map(str::to_string).collect::<Vec<_>>()).collect()
        }

        /// The longest common subsequence's length (how much of the order is kept).
        fn lcs(a: &[String], b: &[String]) -> usize {
            let mut row = vec![0usize; b.len() + 1];
            for x in a {
                let mut prev = 0;
                for (j, y) in b.iter().enumerate() {
                    let cur = row[j + 1];
                    row[j + 1] = if x == y { prev + 1 } else { row[j + 1].max(row[j]) };
                    prev = cur;
                }
            }
            row[b.len()]
        }

        /// PDFium (Chrome's and Edge's PDF engine), loaded from PDFIUM_DLL.
        struct Pdfium {
            lib: windows::Win32::Foundation::HMODULE,
        }

        impl Pdfium {
            fn load() -> Option<Pdfium> {
                let path = std::env::var("PDFIUM_DLL").ok()?;
                let lib = unsafe { windows::Win32::System::LibraryLoader::LoadLibraryW(&windows::core::HSTRING::from(path)) }.ok()?;
                let p = Pdfium { lib };
                unsafe { p.f::<unsafe extern "C" fn()>("FPDF_InitLibrary")() };
                Some(p)
            }

            unsafe fn f<T: Copy>(&self, name: &str) -> T {
                let c = std::ffi::CString::new(name).unwrap();
                let f = unsafe { windows::Win32::System::LibraryLoader::GetProcAddress(self.lib, windows::core::PCSTR(c.as_ptr() as *const u8)) }.unwrap_or_else(|| panic!("{name}"));
                assert_eq!(std::mem::size_of::<T>(), std::mem::size_of_val(&f));
                unsafe { std::mem::transmute_copy(&f) }
            }

            /// Each page's text, and where each word (searched with PDFium's own find, as Chrome's
            /// Ctrl+F does) is: its selection rectangles' box, as fractions of the page shown.
            fn read(&self, path: &str, password: Option<&str>, find: &[&str]) -> Result<Vec<(String, Vec<(String, (f32, f32, f32, f32))>)>, String> {
                type P = *mut c_void;
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                let pw = password.map(|p| std::ffi::CString::new(p).unwrap());
                unsafe {
                    let load: unsafe extern "C" fn(*const u8, i32, *const i8) -> P = self.f("FPDF_LoadMemDocument");
                    let last_error: unsafe extern "C" fn() -> u32 = self.f("FPDF_GetLastError");
                    let count: unsafe extern "C" fn(P) -> i32 = self.f("FPDF_GetPageCount");
                    let load_page: unsafe extern "C" fn(P, i32) -> P = self.f("FPDF_LoadPage");
                    let width: unsafe extern "C" fn(P) -> f32 = self.f("FPDF_GetPageWidthF");
                    let height: unsafe extern "C" fn(P) -> f32 = self.f("FPDF_GetPageHeightF");
                    let text_page: unsafe extern "C" fn(P) -> P = self.f("FPDFText_LoadPage");
                    let chars: unsafe extern "C" fn(P) -> i32 = self.f("FPDFText_CountChars");
                    let get_text: unsafe extern "C" fn(P, i32, i32, *mut u16) -> i32 = self.f("FPDFText_GetText");
                    let find_start: unsafe extern "C" fn(P, *const u16, u32, i32) -> P = self.f("FPDFText_FindStart");
                    let find_next: unsafe extern "C" fn(P) -> i32 = self.f("FPDFText_FindNext");
                    let sch_index: unsafe extern "C" fn(P) -> i32 = self.f("FPDFText_GetSchResultIndex");
                    let sch_count: unsafe extern "C" fn(P) -> i32 = self.f("FPDFText_GetSchCount");
                    let find_close: unsafe extern "C" fn(P) = self.f("FPDFText_FindClose");
                    let count_rects: unsafe extern "C" fn(P, i32, i32) -> i32 = self.f("FPDFText_CountRects");
                    let get_rect: unsafe extern "C" fn(P, i32, *mut f64, *mut f64, *mut f64, *mut f64) -> i32 = self.f("FPDFText_GetRect");
                    let to_device: unsafe extern "C" fn(P, i32, i32, i32, i32, i32, f64, f64, *mut i32, *mut i32) -> i32 = self.f("FPDF_PageToDevice");
                    let close_text: unsafe extern "C" fn(P) = self.f("FPDFText_ClosePage");
                    let close_page: unsafe extern "C" fn(P) = self.f("FPDF_ClosePage");
                    let close_doc: unsafe extern "C" fn(P) = self.f("FPDF_CloseDocument");
                    let doc = load(bytes.as_ptr(), bytes.len() as i32, pw.as_ref().map(|p| p.as_ptr()).unwrap_or(std::ptr::null()));
                    if doc.is_null() {
                        return Err(format!("PDFium couldn't open it (error {})", last_error()));
                    }
                    let mut out = Vec::new();
                    for i in 0..count(doc) {
                        let page = load_page(doc, i);
                        let tp = text_page(page);
                        let n = chars(tp);
                        let mut buf = vec![0u16; n as usize + 1];
                        get_text(tp, 0, n, buf.as_mut_ptr());
                        let text = String::from_utf16_lossy(&buf[..n as usize]);
                        // the page shown, in hundredths of a point
                        let (w, h) = ((width(page) * 100.).round() as i32, (height(page) * 100.).round() as i32);
                        let mut hits = Vec::new();
                        for word in find {
                            let wide: Vec<u16> = word.encode_utf16().chain([0]).collect();
                            let s = find_start(tp, wide.as_ptr(), 0, 0);
                            if find_next(s) != 0 {
                                let (start, len) = (sch_index(s), sch_count(s));
                                if std::env::var("QA_DEBUG").is_ok() {
                                    let (mut x0, mut y0, mut x1, mut y1) = (0f64, 0f64, 0f64, 0f64);
                                    let nr = count_rects(tp, start, len);
                                    let ok = get_rect(tp, 0, &mut x0, &mut y0, &mut x1, &mut y1);
                                    println!("      PDFium “{word}”: chars {start}+{len}, {nr} rects, first {ok}: {x0:.1} {y0:.1} {x1:.1} {y1:.1}; page {w}×{h}");
                                }
                                let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
                                for k in 0..count_rects(tp, start, len) {
                                    let (mut x0, mut y0, mut x1, mut y1) = (0f64, 0f64, 0f64, 0f64);
                                    get_rect(tp, k, &mut x0, &mut y0, &mut x1, &mut y1);
                                    for (x, y) in [(x0, y0), (x1, y1), (x0, y1), (x1, y0)] {
                                        let (mut dx, mut dy) = (0, 0);
                                        to_device(page, 0, 0, w, h, 0, x, y, &mut dx, &mut dy);
                                        let (u, v) = (dx as f32 / w as f32, dy as f32 / h as f32);
                                        (l, r, t, b) = (l.min(u), r.max(u), t.min(v), b.max(v));
                                    }
                                }
                                hits.push((word.to_string(), (l, r, t, b)));
                            }
                            find_close(s);
                        }
                        out.push((text, hits));
                        close_text(tp);
                        close_page(page);
                    }
                    close_doc(doc);
                    Ok(out)
                }
            }
        }

        /// The test scans: name, password, words to find (each must be on some page).
        const SCANS: &[(&str, Option<&str>, &[&str])] = &[
            ("invoice.pdf", None, &["Bakery", "424.44"]),
            ("ro_scanner.pdf", None, &["Brașov"]),
            ("sideways_invoice.pdf", None, &["Bakery", "424.44"]),
            ("multi.pdf", None, &[]),
            ("long27.pdf", None, &[]),
            ("invoice-rot90.pdf", None, &["Bakery", "424.44"]),
            ("invoice-rot180.pdf", None, &["Bakery", "424.44"]),
            ("invoice-rot270.pdf", None, &["Bakery", "424.44"]),
            ("multi-xrefstream.pdf", None, &[]),
            ("invoice-locked-aes.pdf", Some("scan-pass"), &["Bakery", "424.44"]),
            ("ro-locked-rc4.pdf", Some("scan-pass"), &["Brașov"]),
        ];

        #[test]
        #[ignore]
        fn qa_searchable_copies() {
            let d = dir();
            let only = std::env::var("QA_ONLY").ok();
            let pdfium = Pdfium::load();
            println!("independent reader: {}", if pdfium.is_some() { "PDFium (PDFIUM_DLL)" } else { "none (set PDFIUM_DLL)" });
            let mut failures = Vec::new();
            for &(name, password, finds) in SCANS {
                if only.as_deref().is_some_and(|o| !name.contains(o)) {
                    continue;
                }
                let path = format!("{d}\\{name}");
                if !std::path::Path::new(&path).exists() {
                    println!("== {name}: missing, skipped");
                    continue;
                }
                println!("== {name}");
                if let Some(p) = password {
                    crate::pdftext::remember(&path, p);
                }
                let t = std::time::Instant::now();
                let lines = read(&path);
                println!("  read with OCR in {} ms: {} pages with text", t.elapsed().as_millis(), lines.len());
                let t = std::time::Instant::now();
                let (copy, n) = save_copy(&path, &lines, password).unwrap();
                if let Some(p) = password {
                    crate::pdftext::remember(&copy, p);
                }
                println!("  saved {} in {} ms ({n} pages with text, {} → {} bytes)", util::file_name(&copy), t.elapsed().as_millis(), std::fs::metadata(&path).unwrap().len(), std::fs::metadata(&copy).unwrap().len());
                // (a) the pages draw exactly as before
                let (before, after) = (render_all(&path, password, 2.).unwrap(), render_all(&copy, password, 2.).unwrap());
                assert_eq!(before.len(), after.len());
                let mut same = true;
                for (i, (b, a)) in before.iter().zip(&after).enumerate() {
                    let diff = b.2.iter().zip(&a.2).filter(|(x, y)| x != y).count();
                    if (b.0, b.1) != (a.0, a.1) || diff > 0 {
                        same = false;
                        failures.push(format!("{name} page {}: {}×{} vs {}×{}, {diff} bytes differ", i + 1, b.0, b.1, a.0, a.1));
                    }
                }
                println!("  (a) {} pages drawn by Windows at 2×: {}", before.len(), if same { "pixel-identical" } else { "DIFFERENT" });
                if password.is_some() {
                    let locked = render_all(&copy, None, 1.).is_err();
                    println!("  the copy without its password: {}", if locked { "refused (still protected)" } else { "OPENS" });
                    if !locked {
                        failures.push(format!("{name}: opens without its password"));
                    }
                }
                // (b) its own text now: the same words, in the same order; enough that it isn't read again
                let back = crate::pdftext::extract(&copy, 200).expect("the copy reads");
                for (&p, ocr) in &lines {
                    let (want, got) = (words(ocr), words(&back.pages[p as usize]));
                    let (mut ws, mut gs) = (want.clone(), got.clone());
                    ws.sort();
                    gs.sort();
                    let order = lcs(&want, &got) as f32 / want.len().max(1) as f32;
                    let own: usize = back.pages[p as usize].iter().map(|l| l.text.trim().chars().count()).sum();
                    println!("  (b) page {}: {} words recognised, {} read back, same words: {}, order kept: {:.1}%, own text {own} chars", p + 1, want.len(), got.len(), ws == gs, order * 100.);
                    if ws != gs {
                        failures.push(format!("{name} page {}: words differ", p + 1));
                    }
                    if own < 20 && want.iter().map(|w| w.chars().count()).sum::<usize>() >= 20 {
                        failures.push(format!("{name} page {}: would be read again", p + 1));
                    }
                }
                if let Some(first) = lines.iter().next() {
                    let text: String = back.pages[*first.0 as usize].iter().take(6).map(|l| format!("{}{}", l.text, if l.row_goes_on { " | " } else { " / " })).collect();
                    println!("      page {} reads: {text}…", first.0 + 1);
                }
                // (c) found where it was read; (d) PDFium reads and finds it too
                let theirs = pdfium.as_ref().map(|p| p.read(&copy, password, finds));
                if let Some(Ok(pages)) = &theirs {
                    for (&p, ocr) in &lines {
                        let want = words(ocr);
                        let text = &pages[p as usize].0;
                        let got: Vec<String> = text.split_whitespace().map(str::to_string).collect();
                        let missing = want.iter().filter(|w| !got.contains(w)).count();
                        let order = lcs(&want, &got) as f32 / want.len().max(1) as f32;
                        println!("  (d) PDFium page {}: {} chars, {missing} of {} words missing, order kept: {:.1}%", p + 1, text.chars().count(), want.len(), order * 100.);
                        if std::env::var("QA_DEBUG").is_ok() {
                            println!("      missing: {:?}", want.iter().filter(|w| !got.contains(w)).collect::<Vec<_>>());
                            println!("      PDFium text: {:?}", text.chars().take(400).collect::<String>());
                        }
                        if missing > 0 {
                            failures.push(format!("{name} page {}: PDFium misses {missing} words", p + 1));
                        }
                    }
                } else if let Some(Err(e)) = &theirs {
                    failures.push(format!("{name}: PDFium: {e}"));
                }
                for word in finds {
                    let Some((&p, ocr)) = lines.iter().find(|(_, l)| find(l, word).is_some()) else {
                        failures.push(format!("{name}: “{word}” wasn't recognised"));
                        continue;
                    };
                    let o = find(ocr, word).unwrap();
                    let mid = |b: (f32, f32, f32, f32)| ((b.0 + b.1) / 2., (b.2 + b.3) / 2.);
                    let say = |b: (f32, f32, f32, f32)| format!("x {:.3}–{:.3} y {:.3}–{:.3}", b.0, b.1, b.2, b.3);
                    let tall = (o.3 - o.2).min(o.1 - o.0);
                    let near = |b: (f32, f32, f32, f32)| {
                        let (m, n) = (mid(o), mid(b));
                        (m.0 - n.0).abs() < 0.01 + 0.3 * tall && (m.1 - n.1).abs() < 0.01 + 0.3 * tall
                    };
                    match find(&back.pages[p as usize], word) {
                        Some(b) => {
                            println!("  (c) “{word}” page {}: read at {}, Ferret finds it at {} {}", p + 1, say(o), say(b), if near(b) { "✓" } else { "✗" });
                            if !near(b) {
                                failures.push(format!("{name}: Ferret finds “{word}” elsewhere"));
                            }
                        }
                        None => failures.push(format!("{name}: Ferret doesn't find “{word}”")),
                    }
                    if let Some(Ok(pages)) = &theirs {
                        match pages[p as usize].1.iter().find(|(w, _)| w == word) {
                            Some((_, b)) => {
                                println!("      PDFium finds it at {} {}", say(*b), if near(*b) { "✓" } else { "✗" });
                                if !near(*b) {
                                    failures.push(format!("{name}: PDFium finds “{word}” elsewhere"));
                                }
                            }
                            None => failures.push(format!("{name}: PDFium doesn't find “{word}”")),
                        }
                    }
                }
            }
            assert!(failures.is_empty(), "{failures:#?}");
        }
    }
}
