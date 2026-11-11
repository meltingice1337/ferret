//! Word documents (.docx) without a web view: reads the document's XML and lays it out natively.
//! Paragraphs with their styles (headings, title), runs (bold, italic, underline, strike, color,
//! highlight, size), alignment and indents, bulleted and numbered lists, links, tables (with cell
//! shading and merged columns), pictures, page breaks, the first section's header and footer, and
//! footnotes (numbered in the order they're referenced, listed at the end). Fonts, floating
//! shapes, comments and tracked-change marks aren't drawn: the preview shows what's in the
//! document, not a pixel-exact page.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use roxmltree::{Document, Node};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
/// Most blocks read: a preview, not a reader for book-length files.
const MAX_BLOCKS: usize = 600;

pub struct Doc {
    pub blocks: Vec<Block>,
    pub truncated: bool,
    /// The first section's default header and footer.
    pub header: Vec<Block>,
    pub footer: Vec<Block>,
    /// Footnotes in the order they're referenced: their number and their content.
    pub notes: Vec<(usize, Vec<Block>)>,
}

pub enum Block {
    Para(Para),
    Table(Vec<Vec<Cell>>),
    PageBreak,
}

pub struct Cell {
    pub paras: Vec<Para>,
    pub shade: Option<u32>,
    pub span: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Default)]
pub struct Para {
    pub runs: Vec<Run>,
    pub pictures: Vec<Picture>,
    pub align: Align,
    /// Points.
    pub size: f32,
    /// 1–6 for headings, 0 for the title style too (it has its own flag), else 0.
    pub heading: u8,
    pub title: bool,
    /// "•", "1.", "a)"…
    pub marker: Option<String>,
    /// Points.
    pub indent: f32,
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<u32>,
    pub highlight: Option<u32>,
    /// Points.
    pub size: Option<f32>,
    pub link: bool,
}

pub struct Run {
    pub text: String,
    pub style: Style,
}

pub struct Picture {
    /// Ready for GPUI (made once here, so redraws don't decode it again).
    pub image: Arc<gpui::Image>,
    /// Points.
    pub w: f32,
    pub h: f32,
}

impl Para {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

#[derive(Clone, Default)]
struct StyleDef {
    based_on: Option<String>,
    name: String,
    run: Style,
    align: Option<Align>,
}

struct Ctx<'a> {
    styles: HashMap<String, StyleDef>,
    default_size: f32,
    /// numId → (ilvl → (format, text)).
    numbering: HashMap<String, HashMap<u32, (String, String)>>,
    counters: HashMap<(String, u32), u32>,
    rels: HashMap<String, String>,
    zip: &'a mut zip::ZipArchive<std::fs::File>,
    /// Footnote ids in the order they're first referenced (their number is the place + 1).
    notes: Vec<String>,
    /// Inside a footnote's own text, its reference mark isn't a new reference.
    in_note: bool,
}

/// A number as superscript digits (a footnote's mark in the text).
fn superscript(n: usize) -> String {
    n.to_string().chars().map(|c| ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'][c.to_digit(10).unwrap_or(0) as usize]).collect()
}

fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attribute((W, name))
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().namespace() == Some(W) && c.tag_name().name() == name)
}

fn is(n: Node, name: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(W) && n.tag_name().name() == name
}

/// `<w:b/>` is on; `<w:b w:val="0"/>` or "false" is off.
fn on(n: Option<Node>) -> Option<bool> {
    n.map(|n| !matches!(attr(n, "val"), Some("0") | Some("false") | Some("none")))
}

fn hex(s: &str) -> Option<u32> {
    if s.eq_ignore_ascii_case("auto") { None } else { u32::from_str_radix(s, 16).ok() }
}

fn highlight(name: &str) -> Option<u32> {
    Some(match name {
        "yellow" => 0xffff00, "green" => 0x00ff00, "cyan" => 0x00ffff, "magenta" => 0xff00ff,
        "blue" => 0x0000ff, "red" => 0xff0000, "darkBlue" => 0x000080, "darkCyan" => 0x008080,
        "darkGreen" => 0x008000, "darkMagenta" => 0x800080, "darkRed" => 0x800000,
        "darkYellow" => 0x808000, "darkGray" => 0x808080, "lightGray" => 0xc0c0c0, "black" => 0x000000,
        _ => return None,
    })
}

/// Reads `w:rPr` over `base`.
fn run_props(rpr: Option<Node>, mut s: Style) -> Style {
    let Some(rpr) = rpr else { return s };
    if let Some(b) = on(child(rpr, "b")) { s.bold = b; }
    if let Some(i) = on(child(rpr, "i")) { s.italic = i; }
    if let Some(u) = child(rpr, "u") { s.underline = !matches!(attr(u, "val"), Some("none")); }
    if let Some(x) = on(child(rpr, "strike")).or(on(child(rpr, "dstrike"))) { s.strike = x; }
    if let Some(c) = child(rpr, "color").and_then(|c| attr(c, "val")) { s.color = hex(c); }
    if let Some(h) = child(rpr, "highlight").and_then(|h| attr(h, "val")) { s.highlight = highlight(h); }
    if let Some(sh) = child(rpr, "shd").and_then(|h| attr(h, "fill")).and_then(hex) { s.highlight = Some(sh); }
    if let Some(sz) = child(rpr, "sz").and_then(|z| attr(z, "val")).and_then(|v| v.parse::<f32>().ok()) {
        s.size = Some(sz / 2.);
    }
    s
}

fn align_of(ppr: Option<Node>) -> Option<Align> {
    match child(ppr?, "jc").and_then(|j| attr(j, "val"))? {
        "center" => Some(Align::Center),
        "right" | "end" => Some(Align::Right),
        _ => Some(Align::Left),
    }
}

impl Ctx<'_> {
    /// A style with everything it's based on applied.
    fn resolve(&self, id: &str) -> StyleDef {
        let mut chain = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if chain.len() > 10 { break; }
            let Some(def) = self.styles.get(&c) else { break };
            chain.push(def.clone());
            cur = def.based_on.clone();
        }
        let mut out = StyleDef::default();
        for def in chain.iter().rev() {
            out.run = merge(&out.run, &def.run);
            out.align = def.align.or(out.align);
        }
        out.name = chain.first().map(|d| d.name.clone()).unwrap_or_default();
        out
    }

    fn marker(&mut self, num_id: &str, ilvl: u32) -> Option<String> {
        let (fmt, text) = self.numbering.get(num_id)?.get(&ilvl)?.clone();
        // A list item restarts the counters of the levels under it.
        self.counters.retain(|(id, l), _| !(id == num_id && *l > ilvl));
        let n = {
            let c = self.counters.entry((num_id.to_string(), ilvl)).or_insert(0);
            *c += 1;
            *c
        };
        Some(match fmt.as_str() {
            "bullet" => if text.is_empty() || text.chars().all(|c| (c as u32) >= 0xF000) { "•".into() } else { text },
            "none" => String::new(),
            _ => {
                let mut s = text.clone();
                for lvl in 0..=ilvl {
                    let count = if lvl == ilvl { n } else { *self.counters.get(&(num_id.to_string(), lvl)).unwrap_or(&1) };
                    let lvl_fmt = self.numbering.get(num_id).and_then(|m| m.get(&lvl)).map(|(f, _)| f.as_str()).unwrap_or("decimal");
                    s = s.replace(&format!("%{}", lvl + 1), &number(count, lvl_fmt));
                }
                s
            }
        })
    }

    fn picture(&mut self, drawing: Node) -> Option<Picture> {
        let blip = drawing.descendants().find(|n| n.tag_name().namespace() == Some(A) && n.tag_name().name() == "blip")?;
        let id = blip.attribute((R, "embed"))?;
        let target = self.rels.get(id)?.trim_start_matches('/');
        let path = if target.starts_with("word/") { target.to_string() } else { format!("word/{target}") };
        let mut bytes = Vec::new();
        self.zip.by_name(&path).ok()?.read_to_end(&mut bytes).ok()?;
        let extent = drawing.descendants().find(|n| n.tag_name().namespace() == Some(WP) && n.tag_name().name() == "extent");
        let emu = |k: &str| extent.and_then(|e| e.attribute(k)).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.) / 12700.;
        let format = match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
            "png" => gpui::ImageFormat::Png,
            "jpg" | "jpeg" => gpui::ImageFormat::Jpeg,
            "gif" => gpui::ImageFormat::Gif,
            "bmp" => gpui::ImageFormat::Bmp,
            "webp" => gpui::ImageFormat::Webp,
            "tif" | "tiff" => gpui::ImageFormat::Tiff,
            // EMF/WMF drawings aren't decoded.
            _ => return None,
        };
        Some(Picture { image: Arc::new(gpui::Image::from_bytes(format, bytes)), w: emu("cx"), h: emu("cy") })
    }

    fn para(&mut self, p: Node, out: &mut Vec<Block>) {
        let ppr = child(p, "pPr");
        let style = ppr.and_then(|n| child(n, "pStyle")).and_then(|s| attr(s, "val")).map(|id| self.resolve(id));
        let base_run = style.as_ref().map(|s| s.run.clone()).unwrap_or_default();
        let name = style.as_ref().map(|s| s.name.to_ascii_lowercase()).unwrap_or_default();
        let heading = name.strip_prefix("heading ").and_then(|n| n.trim().parse::<u8>().ok()).unwrap_or(0);
        let mut para = Para {
            align: align_of(ppr).or(style.as_ref().and_then(|s| s.align)).unwrap_or_default(),
            heading,
            title: name == "title",
            ..Default::default()
        };
        if let Some(num) = ppr.and_then(|n| child(n, "numPr")) {
            let ilvl = child(num, "ilvl").and_then(|n| attr(n, "val")).and_then(|v| v.parse().ok()).unwrap_or(0);
            if let Some(id) = child(num, "numId").and_then(|n| attr(n, "val")) {
                para.marker = self.marker(id, ilvl).filter(|m| !m.is_empty());
                para.indent = 18. * ilvl as f32;
            }
        } else if let Some(left) = ppr.and_then(|n| child(n, "ind")).and_then(|i| attr(i, "left").or(attr(i, "start"))) {
            para.indent = left.parse::<f32>().unwrap_or(0.) / 20.;
        }
        let mut page_break = false;
        self.inline(p, &base_run, false, &mut para, &mut page_break);
        // GPUI can't mix font sizes inside one text, so a paragraph takes the size most of its
        // characters have.
        let mut by_size: Vec<(f32, usize)> = Vec::new();
        for r in para.runs.iter().filter(|r| !r.text.trim().is_empty()) {
            let s = r.style.size.or(base_run.size).unwrap_or(self.default_size);
            match by_size.iter_mut().find(|(k, _)| *k == s) {
                Some((_, n)) => *n += r.text.chars().count(),
                None => by_size.push((s, r.text.chars().count())),
            }
        }
        para.size = by_size.iter().max_by_key(|(_, n)| *n).map(|(s, _)| *s).or(base_run.size).unwrap_or(self.default_size);
        out.push(Block::Para(para));
        if page_break {
            out.push(Block::PageBreak);
        }
    }

    /// Runs, links, insertions and content controls inside a paragraph, in order.
    fn inline(&mut self, n: Node, base: &Style, link: bool, para: &mut Para, page_break: &mut bool) {
        for c in n.children().filter(|c| c.is_element()) {
            if is(c, "r") {
                let char_style = child(c, "rPr").and_then(|r| child(r, "rStyle")).and_then(|s| attr(s, "val")).map(|id| self.resolve(id).run);
                let mut style = run_props(child(c, "rPr"), char_style.map(|s| merge(base, &s)).unwrap_or_else(|| base.clone()));
                style.link |= link;
                let mut text = String::new();
                for part in c.children().filter(|x| x.is_element()) {
                    if is(part, "t") {
                        text.push_str(part.text().unwrap_or(""));
                    } else if is(part, "tab") {
                        text.push_str("    ");
                    } else if is(part, "br") || is(part, "cr") {
                        if attr(part, "type") == Some("page") { *page_break = true } else { text.push('\n') }
                    } else if is(part, "noBreakHyphen") {
                        text.push('-');
                    } else if is(part, "footnoteReference") && !self.in_note {
                        if let Some(id) = attr(part, "id") {
                            let n = match self.notes.iter().position(|x| x == id) {
                                Some(i) => i + 1,
                                None => {
                                    self.notes.push(id.to_string());
                                    self.notes.len()
                                }
                            };
                            text.push_str(&superscript(n));
                        }
                    } else if is(part, "drawing") || is(part, "pict") {
                        if let Some(pic) = self.picture(part) {
                            para.pictures.push(pic);
                        }
                    }
                }
                if !text.is_empty() {
                    para.runs.push(Run { text, style });
                }
            } else if is(c, "hyperlink") {
                self.inline(c, base, true, para, page_break);
            } else if is(c, "ins") || is(c, "smartTag") || is(c, "fldSimple") || is(c, "customXml") {
                self.inline(c, base, link, para, page_break);
            } else if is(c, "sdt") {
                if let Some(content) = child(c, "sdtContent") {
                    self.inline(content, base, link, para, page_break);
                }
            }
        }
    }

    fn body(&mut self, n: Node, out: &mut Vec<Block>) -> bool {
        for c in n.children().filter(|c| c.is_element()) {
            if out.len() >= MAX_BLOCKS {
                return true;
            }
            if is(c, "p") {
                self.para(c, out);
            } else if is(c, "tbl") {
                let mut rows = Vec::new();
                for tr in c.children().filter(|x| is(*x, "tr")) {
                    let mut cells = Vec::new();
                    for tc in tr.children().filter(|x| is(*x, "tc") || is(*x, "sdt")) {
                        let tc = if is(tc, "sdt") { child(tc, "sdtContent").and_then(|s| child(s, "tc")).unwrap_or(tc) } else { tc };
                        let tcpr = child(tc, "tcPr");
                        let shade = tcpr.and_then(|p| child(p, "shd")).and_then(|s| attr(s, "fill")).and_then(hex);
                        let span = tcpr.and_then(|p| child(p, "gridSpan")).and_then(|g| attr(g, "val")).and_then(|v| v.parse().ok()).unwrap_or(1);
                        let mut inner = Vec::new();
                        self.body(tc, &mut inner);
                        let paras = inner.into_iter().filter_map(|b| if let Block::Para(p) = b { Some(p) } else { None }).collect();
                        cells.push(Cell { paras, shade, span });
                    }
                    rows.push(cells);
                }
                out.push(Block::Table(rows));
            } else if is(c, "sdt") {
                if let Some(content) = child(c, "sdtContent") {
                    if self.body(content, out) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn merge(base: &Style, over: &Style) -> Style {
    let d = Style::default();
    Style {
        bold: if over.bold != d.bold { over.bold } else { base.bold },
        italic: if over.italic != d.italic { over.italic } else { base.italic },
        underline: over.underline || base.underline,
        strike: over.strike || base.strike,
        color: over.color.or(base.color),
        highlight: over.highlight.or(base.highlight),
        size: over.size.or(base.size),
        link: over.link || base.link,
    }
}

fn number(n: u32, fmt: &str) -> String {
    let alpha = |n: u32, upper: bool| {
        let mut s = String::new();
        let mut n = n;
        while n > 0 {
            n -= 1;
            s.insert(0, (b'a' + (n % 26) as u8) as char);
            n /= 26;
        }
        if upper { s.to_uppercase() } else { s }
    };
    let roman = |mut n: u32, upper: bool| {
        let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
        let mut s = String::new();
        for (v, r) in table {
            while n >= v {
                s.push_str(r);
                n -= v;
            }
        }
        if upper { s.to_uppercase() } else { s }
    };
    match fmt {
        "lowerLetter" => alpha(n, false),
        "upperLetter" => alpha(n, true),
        "lowerRoman" => roman(n, false),
        "upperRoman" => roman(n, true),
        _ => n.to_string(),
    }
}

fn read(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let mut s = String::new();
    zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

pub fn open(path: &str) -> Result<Doc, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| "Not a Word document (it isn't a zip package)".to_string())?;
    let document = read(&mut zip, "word/document.xml").ok_or("Not a Word document (no word/document.xml)")?;
    let styles_xml = read(&mut zip, "word/styles.xml");
    let numbering_xml = read(&mut zip, "word/numbering.xml");
    let rels_xml = read(&mut zip, "word/_rels/document.xml.rels");

    let mut styles = HashMap::new();
    let mut default_size = 11.;
    if let Some(xml) = &styles_xml {
        if let Ok(doc) = Document::parse(xml) {
            if let Some(sz) = doc
                .descendants()
                .find(|n| is(*n, "rPrDefault"))
                .and_then(|n| n.descendants().find(|c| is(*c, "sz")))
                .and_then(|s| attr(s, "val"))
                .and_then(|v| v.parse::<f32>().ok())
            {
                default_size = sz / 2.;
            }
            for s in doc.descendants().filter(|n| is(*n, "style")) {
                let Some(id) = attr(s, "styleId") else { continue };
                styles.insert(id.to_string(), StyleDef {
                    based_on: child(s, "basedOn").and_then(|b| attr(b, "val")).map(str::to_string),
                    name: child(s, "name").and_then(|n| attr(n, "val")).unwrap_or(id).to_string(),
                    run: run_props(child(s, "rPr"), Style::default()),
                    align: align_of(child(s, "pPr")),
                });
            }
        }
    }
    let mut numbering = HashMap::new();
    if let Some(xml) = &numbering_xml {
        if let Ok(doc) = Document::parse(xml) {
            let mut abstracts: HashMap<String, HashMap<u32, (String, String)>> = HashMap::new();
            for a in doc.descendants().filter(|n| is(*n, "abstractNum")) {
                let Some(id) = attr(a, "abstractNumId") else { continue };
                let mut levels = HashMap::new();
                for l in a.children().filter(|n| is(*n, "lvl")) {
                    let ilvl = attr(l, "ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let fmt = child(l, "numFmt").and_then(|n| attr(n, "val")).unwrap_or("decimal").to_string();
                    let text = child(l, "lvlText").and_then(|n| attr(n, "val")).unwrap_or("").to_string();
                    levels.insert(ilvl, (fmt, text));
                }
                abstracts.insert(id.to_string(), levels);
            }
            for n in doc.descendants().filter(|n| is(*n, "num")) {
                let (Some(id), Some(abs)) = (attr(n, "numId"), child(n, "abstractNumId").and_then(|a| attr(a, "val"))) else { continue };
                if let Some(levels) = abstracts.get(abs) {
                    numbering.insert(id.to_string(), levels.clone());
                }
            }
        }
    }
    let mut rels = HashMap::new();
    if let Some(xml) = &rels_xml {
        if let Ok(doc) = Document::parse(xml) {
            for r in doc.descendants().filter(|n| n.tag_name().name() == "Relationship") {
                if let (Some(id), Some(target)) = (r.attribute("Id"), r.attribute("Target")) {
                    rels.insert(id.to_string(), target.to_string());
                }
            }
        }
    }

    let doc = Document::parse(&document).map_err(|e| format!("The document's XML is broken: {e}"))?;
    let body = doc.descendants().find(|n| is(*n, "body")).ok_or("The document has no body")?;
    // The first section's header and footer: its own (a section's properties sit in its last
    // paragraph), else the document's last section's; the default one, else the first page's.
    let sect = doc.descendants().find(|n| is(*n, "sectPr"));
    let part_of = |kind: &str| -> Option<String> {
        let refs: Vec<Node> = sect?.children().filter(|c| is(*c, kind)).collect();
        let pick = refs.iter().find(|r| attr(**r, "type") == Some("default")).or(refs.first())?;
        let id = pick.attribute((R, "id"))?;
        let target = rels.get(id)?.trim_start_matches('/');
        Some(if target.starts_with("word/") { target.to_string() } else { format!("word/{target}") })
    };
    let (header_part, footer_part) = (part_of("headerReference"), part_of("footerReference"));
    let mut ctx = Ctx { styles, default_size, numbering, counters: HashMap::new(), rels, zip: &mut zip, notes: Vec::new(), in_note: false };
    let mut blocks = Vec::new();
    let truncated = ctx.body(body, &mut blocks);
    // Trailing empty paragraphs and page breaks add nothing.
    let trim = |blocks: &mut Vec<Block>| {
        while matches!(blocks.last(), Some(Block::Para(p)) if p.runs.is_empty() && p.pictures.is_empty()) || matches!(blocks.last(), Some(Block::PageBreak)) {
            blocks.pop();
        }
    };
    trim(&mut blocks);
    // A header or footer part: its paragraphs and tables, as the body's.
    let part_blocks = |ctx: &mut Ctx, name: Option<String>| -> Vec<Block> {
        let Some(xml) = name.and_then(|n| read(ctx.zip, &n)) else { return Vec::new() };
        let Ok(d) = Document::parse(&xml) else { return Vec::new() };
        let mut out = Vec::new();
        ctx.body(d.root_element(), &mut out);
        trim(&mut out);
        out.retain(|b| !matches!(b, Block::Para(p) if p.runs.iter().all(|r| r.text.trim().is_empty()) && p.pictures.is_empty()));
        out
    };
    let header = part_blocks(&mut ctx, header_part);
    let footer = part_blocks(&mut ctx, footer_part);
    // Footnotes, in the order the text refers to them.
    let mut notes = Vec::new();
    if !ctx.notes.is_empty() {
        if let Some(xml) = read(ctx.zip, "word/footnotes.xml") {
            if let Ok(d) = Document::parse(&xml) {
                ctx.in_note = true;
                for (i, id) in ctx.notes.clone().iter().enumerate() {
                    let Some(note) = d.descendants().find(|n| is(*n, "footnote") && attr(*n, "id") == Some(id.as_str())) else { continue };
                    let mut out = Vec::new();
                    ctx.body(note, &mut out);
                    trim(&mut out);
                    notes.push((i + 1, out));
                }
            }
        }
    }
    Ok(Doc { blocks, truncated, header, footer, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_letters() {
        assert_eq!(number(3, "decimal"), "3");
        assert_eq!(number(28, "lowerLetter"), "ab");
        assert_eq!(number(14, "upperRoman"), "XIV");
    }

    #[test]
    fn opens_the_showcase_report() {
        let path = r"C:\Users\meltingice\FerretQA\showcase\Quarterly report.docx";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let doc = open(path).unwrap();
        assert!(doc.blocks.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(doc.blocks.iter().any(|b| matches!(b, Block::Para(p) if p.runs.iter().any(|r| r.style.bold))));
    }
}
