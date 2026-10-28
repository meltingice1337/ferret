//! Previews of Office files. Spreadsheets (xlsx, xlsm, xlsb, xls, ods) come as a grid of cell
//! values (calamine). PowerPoint decks (pptx) come as slides laid out from their XML: text boxes,
//! shapes' fills and outlines, pictures and tables where they sit, placeholders placed and styled
//! by their layout and master. Charts, SmartArt, theme artwork and effects aren't drawn: the
//! preview shows what's on each slide, not a pixel-exact copy.

use roxmltree::Node;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::PathBuf;

use crate::paths;

// ───────────────────────────── spreadsheets ─────────────────────────────

/// What a sheet preview reads at most.
const MAX_ROWS: usize = 5000;
const MAX_COLS: usize = 200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetPreview {
    /// Every sheet's name, in workbook order.
    sheets: Vec<String>,
    /// The sheet shown.
    sheet: usize,
    /// Cell text row by row, starting at `first_row` / `first_col` (0-based) of the sheet.
    rows: Vec<Vec<String>>,
    first_row: u32,
    first_col: u32,
    /// The sheet's used size (more than `rows` holds when it's cut).
    total_rows: usize,
    total_cols: usize,
}

#[tauri::command]
pub async fn sheet_preview(path: String, sheet: Option<usize>) -> Result<SheetPreview, String> {
    tauri::async_runtime::spawn_blocking(move || read_sheet(&path, sheet.unwrap_or(0))).await.map_err(|e| e.to_string())?
}

fn read_sheet(path: &str, sheet: usize) -> Result<SheetPreview, String> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(paths::for_io(path)).map_err(|e| format!("Couldn't read this workbook: {e}"))?;
    let sheets: Vec<String> = wb.sheet_names().to_vec();
    if sheets.is_empty() {
        return Err("This workbook has no sheets".into());
    }
    let sheet = sheet.min(sheets.len() - 1);
    let range = wb.worksheet_range(&sheets[sheet]).map_err(|e| format!("Couldn't read “{}”: {e}", sheets[sheet]))?;
    let (total_rows, total_cols) = range.get_size();
    let (first_row, first_col) = range.start().unwrap_or((0, 0));
    let rows = range.rows().take(MAX_ROWS).map(|r| r.iter().take(MAX_COLS).map(cell_text).collect()).collect();
    Ok(SheetPreview { sheets, sheet, rows, first_row, first_col, total_rows, total_cols })
}

/// A cell as the sheet would show it, near enough: numbers without float noise, dates as dates.
/// Number formats (currency, percent, decimals) aren't applied.
fn cell_text(c: &calamine::Data) -> String {
    use calamine::Data;
    match c {
        Data::Empty => String::new(),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => number(*f),
        Data::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).into(),
        Data::Error(e) => format!("{e}"),
        Data::DateTime(d) => {
            if d.is_duration() {
                let secs = (d.as_f64() * 86_400.0).round() as i64;
                return format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60);
            }
            match d.as_datetime() {
                Some(t) => {
                    use chrono::Timelike;
                    let time_only = t.date() <= chrono::NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
                    let has_time = t.time().num_seconds_from_midnight() != 0;
                    let secs = if t.second() != 0 { ":%S" } else { "" };
                    if time_only {
                        t.format(&format!("%H:%M{secs}")).to_string()
                    } else if has_time {
                        t.format(&format!("%Y-%m-%d %H:%M{secs}")).to_string()
                    } else {
                        t.format("%Y-%m-%d").to_string()
                    }
                }
                None => number(d.as_f64()),
            }
        }
    }
}

/// 3 for 3.0, 0.3 for 0.30000000000000004.
fn number(f: f64) -> String {
    if !f.is_finite() {
        return f.to_string();
    }
    if f.fract() == 0.0 && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    if f.abs() >= 1e15 || f.abs() < 1e-9 {
        return format!("{f:e}");
    }
    let digits = 10 - (f.abs().log10().floor() as i32 + 1).max(0);
    let k = 10f64.powi(digits.max(0));
    format!("{}", (f * k).round() / k)
}

// ───────────────────────────── slide decks ─────────────────────────────

/// Slides a deck preview lays out at most.
const MAX_SLIDES: usize = 300;
/// EMU (the unit of every position and size in a deck) per point.
const EMU_PER_PT: f64 = 12_700.0;

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Deck {
    /// Slide size in EMU (914 400 to the inch).
    width: i64,
    height: i64,
    slides: Vec<Slide>,
    /// Slides in the deck (more than `slides` holds when it's cut).
    total: usize,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Slide {
    /// CSS colour.
    background: Option<String>,
    background_image: Option<String>,
    shapes: Vec<Shape>,
    /// Hidden in the slide show.
    hidden: bool,
}

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Shape {
    /// Position and size in EMU.
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    /// Degrees, clockwise.
    rot: f64,
    flip_h: bool,
    flip_v: bool,
    /// Preset geometry ("rect", "ellipse", "roundRect", "line"…).
    geom: String,
    fill: Option<String>,
    line: Option<String>,
    /// Outline width in points.
    line_w: f64,
    /// Picture file, extracted to the cache.
    image: Option<String>,
    /// The part of the picture shown: left, top, right, bottom cut off, as fractions.
    crop: Option<[f64; 4]>,
    paras: Vec<Para>,
    /// Vertical anchor of the text: "t", "ctr" or "b".
    anchor: String,
    /// Text insets in EMU: left, top, right, bottom.
    inset: [i64; 4],
    /// Table cells' text, row by row.
    table: Option<Vec<Vec<String>>>,
    /// What stands in for content not drawn ("Chart", "SmartArt").
    label: Option<String>,
    /// Vertical text: "vert" (top to bottom), "vert270" (bottom to top).
    vert: Option<String>,
}

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Para {
    /// "l", "ctr", "r" or "just".
    align: String,
    level: u32,
    bullet: Option<String>,
    runs: Vec<Run>,
    /// Size (pt) of an empty paragraph's line.
    size: f64,
    /// Line spacing, as a multiple of single spacing.
    line: f64,
    /// Space above, in points (not for a text box's first paragraph).
    before: f64,
}

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    text: String,
    /// Points.
    size: f64,
    bold: bool,
    italic: bool,
    underline: bool,
    color: Option<String>,
    font: Option<String>,
    /// "all" or "small" capitals.
    caps: Option<String>,
}

#[tauri::command]
pub async fn slides_preview(path: String) -> Result<Deck, String> {
    tauri::async_runtime::spawn_blocking(move || read_deck(&path)).await.map_err(|e| e.to_string())?
}

/// A deck: a zip of XML parts, linked by `_rels` files.
struct Pkg {
    zip: zip::ZipArchive<BufReader<File>>,
    /// Where its pictures are extracted.
    cache: PathBuf,
}

impl Pkg {
    fn text(&mut self, part: &str) -> Option<String> {
        let mut f = self.zip.by_name(part).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    }

    /// rId → part name, for the relationships of `part`.
    fn rels(&mut self, part: &str) -> HashMap<String, Rel> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let Some(xml) = self.text(&format!("{dir}/_rels/{file}.rels")) else { return HashMap::new() };
        let Ok(doc) = roxmltree::Document::parse(&xml) else { return HashMap::new() };
        doc.root_element()
            .children()
            .filter(|n| n.has_tag_name_local("Relationship") && n.attribute("TargetMode") != Some("External"))
            .filter_map(|n| {
                let ty = n.attribute("Type")?.rsplit('/').next()?.to_string();
                Some((n.attribute("Id")?.to_string(), Rel { ty, target: resolve(dir, n.attribute("Target")?) }))
            })
            .collect()
    }

    /// The first relationship of `part` of type `ty` ("slideLayout", "slideMaster", "theme").
    fn related(&mut self, part: &str, ty: &str) -> Option<String> {
        self.rels(part).into_values().find(|r| r.ty == ty).map(|r| r.target)
    }

    /// A picture as a file the page can show (browsers can't draw EMF, WMF or TIFF).
    fn picture(&mut self, part: &str) -> Option<String> {
        let name = part.rsplit('/').next()?;
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "ico") {
            return None;
        }
        let out = self.cache.join(name);
        if !out.exists() {
            fs::create_dir_all(&self.cache).ok()?;
            let mut f = self.zip.by_name(part).ok()?;
            let mut bytes = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut bytes).ok()?;
            // Written aside first, so a half-written file is never shown.
            let tmp = out.with_extension("part");
            fs::write(&tmp, bytes).ok()?;
            fs::rename(&tmp, &out).ok()?;
        }
        Some(paths::normalize(&out.to_string_lossy()))
    }
}

struct Rel {
    ty: String,
    target: String,
}

/// A relationship target, relative to the folder of the part that names it, as a part name.
fn resolve(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

trait LocalName {
    fn has_tag_name_local(&self, name: &str) -> bool;
}
impl LocalName for Node<'_, '_> {
    fn has_tag_name_local(&self, name: &str) -> bool {
        self.is_element() && self.tag_name().name() == name
    }
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name_local(name))
}

fn kids<'a, 'i: 'a>(n: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    n.children().filter(move |c| c.has_tag_name_local(name))
}

/// Follows a path of child element names.
fn at<'a, 'i>(n: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
    path.iter().try_fold(n, |n, name| child(n, name))
}

/// An attribute by local name, whatever its namespace (`r:embed`).
fn attr<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

fn num(n: Node, name: &str) -> Option<i64> {
    attr(n, name)?.parse().ok()
}

fn flag(n: Node, name: &str) -> Option<bool> {
    attr(n, name).map(|v| v == "1" || v == "true")
}

// ---------- colours ----------

/// The theme's colours by scheme name (`accent1`, `dk1`, and `tx1`, `bg1`… through the master's map),
/// and its heading and body fonts.
#[derive(Default, Clone)]
struct Theme {
    colors: HashMap<String, [u8; 3]>,
    major: Option<String>,
    minor: Option<String>,
}

fn read_theme(xml: &str, map: &HashMap<String, String>) -> Theme {
    let mut t = Theme::default();
    let Ok(doc) = roxmltree::Document::parse(xml) else { return t };
    let root = doc.root_element();
    if let Some(scheme) = at(root, &["themeElements", "clrScheme"]) {
        for c in scheme.children().filter(|c| c.is_element()) {
            let rgb = c.children().find(|x| x.is_element()).and_then(|x| match x.tag_name().name() {
                "srgbClr" => attr(x, "val").and_then(hex),
                "sysClr" => attr(x, "lastClr").and_then(hex),
                _ => None,
            });
            if let Some(rgb) = rgb {
                t.colors.insert(c.tag_name().name().to_string(), rgb);
            }
        }
    }
    // tx1 → dk1 and so on, as the master maps them (light text on dark slides swaps them).
    let defaults = [("tx1", "dk1"), ("bg1", "lt1"), ("tx2", "dk2"), ("bg2", "lt2")];
    for (alias, base) in defaults {
        let base = map.get(alias).map(String::as_str).unwrap_or(base);
        if let Some(c) = t.colors.get(base).copied() {
            t.colors.insert(alias.to_string(), c);
        }
    }
    let font = |kind: &str| at(root, &["themeElements", "fontScheme", kind, "latin"]).and_then(|n| attr(n, "typeface")).map(str::to_string);
    t.major = font("majorFont");
    t.minor = font("minorFont");
    t
}

fn hex(v: &str) -> Option<[u8; 3]> {
    let v = v.trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&v[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// The colour inside a fill or colour element (`a:solidFill`, `a:fontRef`…), with its
/// lighter/darker and transparency changes applied, as CSS.
fn color(n: Node, theme: &Theme) -> Option<String> {
    let c = n.children().find(|c| c.is_element() && matches!(c.tag_name().name(), "srgbClr" | "schemeClr" | "sysClr" | "prstClr" | "scrgbClr"))?;
    let mut rgb = match c.tag_name().name() {
        "srgbClr" => hex(attr(c, "val")?)?,
        "sysClr" => hex(attr(c, "lastClr").unwrap_or("000000"))?,
        "schemeClr" => {
            let v = attr(c, "val")?;
            // phClr: the colour the shape's style passes in; the text colour is the usual one.
            let v = if v == "phClr" { "tx1" } else { v };
            *theme.colors.get(v)?
        }
        "scrgbClr" => {
            let p = |k: &str| (num(c, k).unwrap_or(0) as f64 / 100_000.0 * 255.0).round().clamp(0.0, 255.0) as u8;
            [p("r"), p("g"), p("b")]
        }
        _ => match attr(c, "val")? {
            "white" => [255, 255, 255],
            "red" => [255, 0, 0],
            "green" => [0, 128, 0],
            "blue" => [0, 0, 255],
            "yellow" => [255, 255, 0],
            "gray" | "grey" => [128, 128, 128],
            _ => [0, 0, 0],
        },
    };
    let mut alpha = 1.0;
    let (mut lum_mod, mut lum_off) = (None, None);
    for m in c.children().filter(|m| m.is_element()) {
        let v = num(m, "val").unwrap_or(100_000) as f64 / 100_000.0;
        match m.tag_name().name() {
            "alpha" => alpha = v,
            "lumMod" => lum_mod = Some(v),
            "lumOff" => lum_off = Some(v),
            "tint" => rgb = rgb.map(|x| (x as f64 * v + 255.0 * (1.0 - v)).round() as u8),
            "shade" => rgb = rgb.map(|x| (x as f64 * v).round() as u8),
            _ => {}
        }
    }
    if lum_mod.is_some() || lum_off.is_some() {
        let (h, s, l) = to_hsl(rgb);
        rgb = from_hsl(h, s, (l * lum_mod.unwrap_or(1.0) + lum_off.unwrap_or(0.0)).clamp(0.0, 1.0));
    }
    Some(if alpha < 1.0 {
        format!("rgba({},{},{},{:.2})", rgb[0], rgb[1], rgb[2], alpha)
    } else {
        format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
    })
}

fn to_hsl([r, g, b]: [u8; 3]) -> (f64, f64, f64) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r { (g - b) / d + if g < b { 6.0 } else { 0.0 } } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    (h / 6.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> [u8; 3] {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 { p + (q - p) * 6.0 * t } else if t < 0.5 { q } else if t < 2.0 / 3.0 { p + (q - p) * (2.0 / 3.0 - t) * 6.0 } else { p };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

/// A shape's fill: solid, the first stop of a gradient, none (`Some(None)`), or not set (`None`).
fn fill(sp_pr: Node, theme: &Theme) -> Option<Option<String>> {
    for c in sp_pr.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "noFill" => return Some(None),
            "solidFill" => return Some(color(c, theme)),
            "gradFill" => return Some(at(c, &["gsLst", "gs"]).and_then(|gs| color(gs, theme))),
            _ => {}
        }
    }
    None
}

// ---------- text styles ----------

/// One list level's text defaults (`a:lvl1pPr`…).
#[derive(Default, Clone)]
struct Style {
    size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<String>,
    font: Option<String>,
    align: Option<String>,
    /// Some("") for "no bullet".
    bullet: Option<String>,
    /// "all" or "small" capitals.
    caps: Option<String>,
    /// Line spacing as a fraction of single (0.9 for 90%).
    line: Option<f64>,
    /// Space above: points, or a fraction of the font size.
    before_pt: Option<f64>,
    before_pct: Option<f64>,
}

impl Style {
    /// Fills what this one leaves unset from `other`.
    fn or(mut self, other: &Style) -> Style {
        self.size = self.size.or(other.size);
        self.bold = self.bold.or(other.bold);
        self.italic = self.italic.or(other.italic);
        self.color = self.color.or_else(|| other.color.clone());
        self.font = self.font.or_else(|| other.font.clone());
        self.align = self.align.or_else(|| other.align.clone());
        self.bullet = self.bullet.or_else(|| other.bullet.clone());
        self.caps = self.caps.or_else(|| other.caps.clone());
        self.line = self.line.or(other.line);
        if self.before_pt.is_none() && self.before_pct.is_none() {
            self.before_pt = other.before_pt;
            self.before_pct = other.before_pct;
        }
        self
    }
}

/// Text defaults of a paragraph (`a:pPr`, `a:lvl1pPr`…) and its run properties (`a:defRPr`, `a:rPr`).
fn para_style(p_pr: Option<Node>, theme: &Theme) -> Style {
    let mut s = Style::default();
    let Some(p) = p_pr else { return s };
    s.align = attr(p, "algn").map(str::to_string);
    for c in p.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "buNone" => s.bullet = Some(String::new()),
            "buChar" => s.bullet = attr(c, "char").map(str::to_string),
            "buAutoNum" => s.bullet = Some("#".into()),
            "lnSpc" => s.line = child(c, "spcPct").and_then(|v| num(v, "val")).map(|v| v as f64 / 100_000.0),
            "spcBef" => {
                s.before_pt = child(c, "spcPts").and_then(|v| num(v, "val")).map(|v| v as f64 / 100.0);
                s.before_pct = child(c, "spcPct").and_then(|v| num(v, "val")).map(|v| v as f64 / 100_000.0);
            }
            _ => {}
        }
    }
    if let Some(r) = child(p, "defRPr") {
        s = run_style(r, theme).or(&s);
    }
    s
}

fn run_style(r: Node, theme: &Theme) -> Style {
    Style {
        size: num(r, "sz").map(|v| v as f64 / 100.0),
        bold: flag(r, "b"),
        italic: flag(r, "i"),
        color: child(r, "solidFill").and_then(|f| color(f, theme)),
        font: child(r, "latin").and_then(|n| attr(n, "typeface")).map(|f| font_name(f, theme)),
        caps: attr(r, "cap").map(str::to_string),
        ..Default::default()
    }
}

/// `+mj-lt` / `+mn-lt` are the theme's heading and body fonts.
fn font_name(f: &str, theme: &Theme) -> String {
    match f {
        x if x.starts_with("+mj") => theme.major.clone().unwrap_or_default(),
        x if x.starts_with("+mn") => theme.minor.clone().unwrap_or_default(),
        x => x.to_string(),
    }
}

/// The nine levels of a list style (`a:lstStyle`, `p:titleStyle`…).
fn levels(n: Option<Node>, theme: &Theme) -> Vec<Style> {
    (1..=9).map(|i| para_style(n.and_then(|n| child(n, &format!("lvl{i}pPr"))), theme)).collect()
}

// ---------- placeholders ----------

#[derive(Clone, Copy, Default)]
struct Xfrm {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    rot: f64,
    flip_h: bool,
    flip_v: bool,
}

fn xfrm(n: Option<Node>) -> Option<Xfrm> {
    let n = n?;
    let off = child(n, "off")?;
    let ext = child(n, "ext")?;
    Some(Xfrm {
        x: num(off, "x")?,
        y: num(off, "y")?,
        w: num(ext, "cx")?,
        h: num(ext, "cy")?,
        rot: num(n, "rot").unwrap_or(0) as f64 / 60_000.0,
        flip_h: flag(n, "flipH").unwrap_or(false),
        flip_v: flag(n, "flipV").unwrap_or(false),
    })
}

/// What a layout or master says about one placeholder: where it sits, its text defaults.
#[derive(Clone, Default)]
struct Ph {
    xfrm: Option<Xfrm>,
    levels: Vec<Style>,
    anchor: Option<String>,
    vert: Option<String>,
    fill: Option<Option<String>>,
}

/// A placeholder's type, the way masters name them: title, body, dt, ftr, sldNum…
fn ph_kind(ty: &str) -> &str {
    match ty {
        "ctrTitle" | "title" => "title",
        "subTitle" | "obj" | "body" | "" => "body",
        t => t,
    }
}

/// The placeholders of a layout or master, by `idx:N` and `type:T`.
fn placeholders(root: Node, theme: &Theme) -> HashMap<String, Ph> {
    let mut out = HashMap::new();
    let Some(tree) = at(root, &["cSld", "spTree"]) else { return out };
    for sp in tree.descendants().filter(|n| n.has_tag_name_local("sp")) {
        let Some(ph) = at(sp, &["nvSpPr", "nvPr", "ph"]) else { continue };
        let body = child(sp, "txBody");
        let info = Ph {
            xfrm: xfrm(child(sp, "spPr").and_then(|s| child(s, "xfrm"))),
            levels: levels(body.and_then(|b| child(b, "lstStyle")), theme),
            anchor: body.and_then(|b| child(b, "bodyPr")).and_then(|b| attr(b, "anchor")).map(str::to_string),
            vert: body.and_then(|b| child(b, "bodyPr")).and_then(|b| attr(b, "vert")).map(str::to_string),
            fill: child(sp, "spPr").and_then(|s| fill(s, theme)),
        };
        if let Some(i) = attr(ph, "idx") {
            out.insert(format!("idx:{i}"), info.clone());
        }
        out.insert(format!("type:{}", ph_kind(attr(ph, "type").unwrap_or(""))), info);
    }
    out
}

fn find_ph<'a>(phs: &'a HashMap<String, Ph>, idx: Option<&str>, ty: &str) -> Option<&'a Ph> {
    idx.and_then(|i| phs.get(&format!("idx:{i}"))).or_else(|| phs.get(&format!("type:{}", ph_kind(ty))))
}

/// A master: its theme, placeholders, background, the text styles of titles, body text and
/// everything else, and the shapes of its own drawn under every slide (logos, bands, pictures).
struct Master {
    theme: Theme,
    phs: HashMap<String, Ph>,
    background: Bg,
    title: Vec<Style>,
    body: Vec<Style>,
    other: Vec<Style>,
    decor: Vec<Shape>,
}

/// A layout: its placeholders, background and shapes of its own, over its master's.
struct Layout {
    phs: HashMap<String, Ph>,
    background: Bg,
    master: String,
    decor: Vec<Shape>,
    /// Its slides show the master's shapes too ("Hide background graphics" is off).
    master_shapes: bool,
}

#[derive(Clone, Default)]
struct Bg {
    color: Option<String>,
    image: Option<String>,
}

fn background(root: Node, rels: &HashMap<String, Rel>, pkg: &mut Pkg, theme: &Theme) -> Bg {
    let Some(bg) = at(root, &["cSld", "bg"]) else { return Bg::default() };
    if let Some(pr) = child(bg, "bgPr") {
        if let Some(blip) = at(pr, &["blipFill", "blip"]) {
            let image = attr(blip, "embed").and_then(|id| rels.get(id)).and_then(|r| pkg.picture(&r.target.clone()));
            return Bg { color: None, image };
        }
        return Bg { color: fill(pr, theme).flatten(), image: None };
    }
    // A theme background: its colour is the one given with the reference.
    Bg { color: child(bg, "bgRef").and_then(|r| color(r, theme)), image: None }
}

fn read_master(pkg: &mut Pkg, part: &str) -> Option<Master> {
    let xml = pkg.text(part)?;
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let root = doc.root_element();
    let map: HashMap<String, String> =
        child(root, "clrMap").map(|m| m.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect()).unwrap_or_default();
    let theme = pkg.related(part, "theme").and_then(|t| pkg.text(&t)).map(|x| read_theme(&x, &map)).unwrap_or_default();
    let rels = pkg.rels(part);
    let styles = child(root, "txStyles");
    let mut m = Master {
        phs: placeholders(root, &theme),
        background: background(root, &rels, pkg, &theme),
        title: levels(styles.and_then(|s| child(s, "titleStyle")), &theme),
        body: levels(styles.and_then(|s| child(s, "bodyStyle")), &theme),
        other: levels(styles.and_then(|s| child(s, "otherStyle")), &theme),
        theme,
        decor: Vec::new(),
    };
    m.decor = decor(pkg, root, &rels, None, &m);
    Some(m)
}

/// The shapes of a layout or master that aren't placeholders: what it draws on its slides.
fn decor(pkg: &mut Pkg, root: Node, rels: &HashMap<String, Rel>, layout_phs: Option<&HashMap<String, Ph>>, master: &Master) -> Vec<Shape> {
    let mut ctx = Ctx { pkg, rels, layout_phs, master, decor: true, shapes: Vec::new() };
    if let Some(tree) = at(root, &["cSld", "spTree"]) {
        ctx.tree(tree, Tf::default());
    }
    ctx.shapes
}

fn read_layout(pkg: &mut Pkg, part: &str, masters: &mut HashMap<String, Master>) -> Option<Layout> {
    let master = pkg.related(part, "slideMaster")?;
    if !masters.contains_key(&master) {
        let m = read_master(pkg, &master)?;
        masters.insert(master.clone(), m);
    }
    let theme = masters[&master].theme.clone();
    let xml = pkg.text(part)?;
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let root = doc.root_element();
    let rels = pkg.rels(part);
    let phs = placeholders(root, &theme);
    let decor = decor(pkg, root, &rels, Some(&phs), &masters[&master]);
    Some(Layout { background: background(root, &rels, pkg, &theme), phs, master, decor, master_shapes: attr(root, "showMasterSp") != Some("0") })
}

// ---------- slides ----------

fn read_deck(path: &str) -> Result<Deck, String> {
    let io = paths::for_io(path);
    let meta = fs::metadata(&io).map_err(|e| e.to_string())?;
    let stamp = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let key = crate::archive::fnv1a(format!("{}|{stamp}|{}", paths::key(path), meta.len()).as_bytes());
    let zip = zip::ZipArchive::new(BufReader::new(File::open(&io).map_err(|e| e.to_string())?))
        .map_err(|_| "This isn't a PowerPoint (.pptx) file, or it's damaged".to_string())?;
    let mut pkg = Pkg { zip, cache: crate::archive::cache_base().join(format!("slides-{:012x}", key & 0xFFFF_FFFF_FFFF)) };

    let xml = pkg.text("ppt/presentation.xml").ok_or("This isn't a PowerPoint (.pptx) file, or it's damaged")?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("Couldn't read this deck: {e}"))?;
    let root = doc.root_element();
    let size = child(root, "sldSz");
    let (width, height) = (size.and_then(|s| num(s, "cx")).unwrap_or(12_192_000), size.and_then(|s| num(s, "cy")).unwrap_or(6_858_000));
    let rels = pkg.rels("ppt/presentation.xml");
    // Each slide's `r:id` (the plain `id` is a number of its own).
    let order: Vec<String> = child(root, "sldIdLst")
        .map(|l| {
            kids(l, "sldId")
                .filter_map(|s| s.attributes().find(|a| a.name() == "id" && a.namespace().is_some()).map(|a| a.value()))
                .filter_map(|id| rels.get(id).map(|r| r.target.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut masters: HashMap<String, Master> = HashMap::new();
    let mut layouts: HashMap<String, Option<Layout>> = HashMap::new();
    let mut slides = Vec::new();
    for part in order.iter().take(MAX_SLIDES) {
        slides.push(read_slide(&mut pkg, part, &mut layouts, &mut masters).unwrap_or_default());
    }
    Ok(Deck { width, height, total: order.len(), slides })
}

fn read_slide(pkg: &mut Pkg, part: &str, layouts: &mut HashMap<String, Option<Layout>>, masters: &mut HashMap<String, Master>) -> Option<Slide> {
    let xml = pkg.text(part)?;
    let doc = roxmltree::Document::parse(&xml).ok()?;
    let root = doc.root_element();
    let rels = pkg.rels(part);
    let layout_part = rels.values().find(|r| r.ty == "slideLayout").map(|r| r.target.clone())?;
    if !layouts.contains_key(&layout_part) {
        let l = read_layout(pkg, &layout_part, masters);
        layouts.insert(layout_part.clone(), l);
    }
    let layout = layouts.get(&layout_part)?.as_ref()?;
    let master = masters.get(&layout.master)?;
    let theme = &master.theme;

    let own = background(root, &rels, pkg, theme);
    let bg = [own, layout.background.clone(), master.background.clone()]
        .into_iter()
        .find(|b| b.color.is_some() || b.image.is_some())
        .unwrap_or_default();
    // Under the slide's own shapes: its layout's, and its master's.
    let mut under = Vec::new();
    if attr(root, "showMasterSp") != Some("0") {
        if layout.master_shapes {
            under.extend(master.decor.iter().cloned());
        }
        under.extend(layout.decor.iter().cloned());
    }
    let mut ctx = Ctx { pkg, rels: &rels, layout_phs: Some(&layout.phs), master, decor: false, shapes: under };
    if let Some(tree) = at(root, &["cSld", "spTree"]) {
        ctx.tree(tree, Tf::default());
    }
    Some(Slide {
        background: bg.color.or_else(|| theme.colors.get("bg1").map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))),
        background_image: bg.image,
        shapes: ctx.shapes,
        hidden: attr(root, "show") == Some("0"),
    })
}

/// Maps a group's child coordinates to the slide's: p → o + (p − c) · s.
#[derive(Clone, Copy)]
struct Tf {
    ox: f64,
    oy: f64,
    cx: f64,
    cy: f64,
    sx: f64,
    sy: f64,
}

impl Default for Tf {
    fn default() -> Self {
        Tf { ox: 0.0, oy: 0.0, cx: 0.0, cy: 0.0, sx: 1.0, sy: 1.0 }
    }
}

impl Tf {
    fn apply(&self, x: &Xfrm) -> Xfrm {
        Xfrm {
            x: (self.ox + (x.x as f64 - self.cx) * self.sx).round() as i64,
            y: (self.oy + (x.y as f64 - self.cy) * self.sy).round() as i64,
            w: (x.w as f64 * self.sx).round() as i64,
            h: (x.h as f64 * self.sy).round() as i64,
            ..*x
        }
    }
}

struct Ctx<'p, 'l> {
    pkg: &'p mut Pkg,
    rels: &'l HashMap<String, Rel>,
    /// The placeholders of the slide's layout (none while drawing a layout or master).
    layout_phs: Option<&'l HashMap<String, Ph>>,
    master: &'l Master,
    /// Drawing a layout's or master's own shapes: its placeholders are left out.
    decor: bool,
    shapes: Vec<Shape>,
}

impl Ctx<'_, '_> {
    fn tree(&mut self, tree: Node, tf: Tf) {
        for n in tree.children().filter(|c| c.is_element()) {
            match n.tag_name().name() {
                "sp" | "cxnSp" => self.shape(n, tf),
                "pic" => self.pic(n, tf),
                "graphicFrame" => self.frame(n, tf),
                "grpSp" => {
                    let Some(g) = child(n, "grpSpPr").and_then(|p| child(p, "xfrm")) else { continue };
                    let (Some(at_), Some(ext), Some(choff), Some(chext)) = (xfrm(Some(g)), child(g, "ext"), child(g, "chOff"), child(g, "chExt")) else {
                        self.tree(n, tf);
                        continue;
                    };
                    let (cw, ch) = (num(chext, "cx").unwrap_or(1).max(1) as f64, num(chext, "cy").unwrap_or(1).max(1) as f64);
                    let (gw, gh) = (num(ext, "cx").unwrap_or(0) as f64, num(ext, "cy").unwrap_or(0) as f64);
                    let origin = tf.apply(&at_);
                    let inner = Tf {
                        ox: origin.x as f64,
                        oy: origin.y as f64,
                        cx: num(choff, "x").unwrap_or(0) as f64,
                        cy: num(choff, "y").unwrap_or(0) as f64,
                        sx: tf.sx * gw / cw,
                        sy: tf.sy * gh / ch,
                    };
                    self.tree(n, inner);
                }
                // Content for newer PowerPoint versions, with a fallback for others.
                "AlternateContent" => {
                    if let Some(alt) = child(n, "Fallback").or_else(|| child(n, "Choice")) {
                        self.tree(alt, tf);
                    }
                }
                _ => {}
            }
        }
    }

    fn shape(&mut self, sp: Node, tf: Tf) {
        let ph = at(sp, &["nvSpPr", "nvPr", "ph"]).or_else(|| at(sp, &["nvCxnSpPr", "nvPr", "ph"]));
        if self.decor && ph.is_some() {
            return;
        }
        let (idx, ty) = (ph.and_then(|p| attr(p, "idx")), ph.map(|p| attr(p, "type").unwrap_or("")).unwrap_or(""));
        let lay = ph.and_then(|_| self.layout_phs.and_then(|l| find_ph(l, idx, ty)));
        let mas = ph.and_then(|_| find_ph(&self.master.phs, None, ty));
        let sp_pr = child(sp, "spPr");
        let Some(place) = xfrm(sp_pr.and_then(|s| child(s, "xfrm"))).or(lay.and_then(|p| p.xfrm)).or(mas.and_then(|p| p.xfrm)) else { return };
        let place = tf.apply(&place);
        let theme = &self.master.theme;
        let style = child(sp, "style");
        let fill_ = sp_pr
            .and_then(|s| fill(s, theme))
            .or_else(|| lay.and_then(|p| p.fill.clone()))
            .unwrap_or_else(|| style.and_then(|s| child(s, "fillRef")).filter(|r| attr(*r, "idx") != Some("0")).and_then(|r| color(r, theme)));
        let ln = sp_pr.and_then(|s| child(s, "ln"));
        let line = match ln.and_then(|l| fill(l, theme)) {
            Some(c) => c,
            None => style.and_then(|s| child(s, "lnRef")).filter(|r| attr(*r, "idx") != Some("0")).and_then(|r| color(r, theme)),
        };
        let geom = sp_pr.and_then(|s| child(s, "prstGeom")).and_then(|g| attr(g, "prst")).unwrap_or(if sp.tag_name().name() == "cxnSp" { "line" } else { "rect" });

        // Text defaults: the shape's own list style, then its layout's and master's placeholder,
        // then the master's style for titles, body text or other text.
        let kind = if ph.is_none() { "other" } else { ph_kind(ty) };
        let master_levels = match kind {
            "title" => &self.master.title,
            "body" => &self.master.body,
            _ => &self.master.other,
        };
        let body = child(sp, "txBody");
        let own = levels(body.and_then(|b| child(b, "lstStyle")), theme);
        let chain: Vec<&Vec<Style>> = [Some(&own), lay.map(|p| &p.levels), mas.map(|p| &p.levels), Some(master_levels)].into_iter().flatten().collect();
        let font_ref = style.and_then(|s| child(s, "fontRef")).and_then(|r| color(r, theme));
        let fallback_font = if kind == "title" { theme.major.clone() } else { theme.minor.clone() };
        let default_size = match kind {
            "title" => 44.0,
            "body" => 28.0,
            _ => 18.0,
        };
        let body_pr = body.and_then(|b| child(b, "bodyPr"));
        // "Shrink text on overflow": the fonts and line spacing PowerPoint settled on.
        let autofit = body_pr.and_then(|b| child(b, "normAutofit"));
        let scale = autofit.and_then(|a| num(a, "fontScale")).map(|v| v as f64 / 100_000.0).unwrap_or(1.0);
        let tighter = 1.0 - autofit.and_then(|a| num(a, "lnSpcReduction")).map(|v| v as f64 / 100_000.0).unwrap_or(0.0);
        let mut paras = Vec::new();
        let mut numbering = 0;
        for p in body.into_iter().flat_map(|b| kids(b, "p")) {
            let p_pr = child(p, "pPr");
            let level = p_pr.and_then(|x| num(x, "lvl")).unwrap_or(0).clamp(0, 8) as usize;
            let mut s = para_style(p_pr, theme);
            for lv in &chain {
                s = s.or(&lv[level]);
            }
            let base = Style {
                color: s.color.clone().or_else(|| font_ref.clone()).or_else(|| theme.colors.get("tx1").map(|c| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))),
                font: s.font.clone().filter(|f| !f.is_empty()).or_else(|| fallback_font.clone()),
                size: Some(s.size.unwrap_or(default_size)),
                ..s.clone()
            };
            let mut runs = Vec::new();
            for r in p.children().filter(|c| c.is_element()) {
                let text = match r.tag_name().name() {
                    "r" | "fld" => child(r, "t").and_then(|t| t.text()).unwrap_or("").to_string(),
                    "br" => "\n".to_string(),
                    _ => continue,
                };
                let rs = child(r, "rPr").map(|x| run_style(x, theme)).unwrap_or_default().or(&base);
                let underline = child(r, "rPr").and_then(|x| attr(x, "u")).is_some_and(|u| u != "none");
                runs.push(Run {
                    text,
                    size: rs.size.unwrap_or(default_size) * scale,
                    bold: rs.bold.unwrap_or(false),
                    italic: rs.italic.unwrap_or(false),
                    underline,
                    color: rs.color,
                    font: rs.font.filter(|f| !f.is_empty()),
                    caps: rs.caps.filter(|c| c != "none"),
                });
            }
            let has_text = runs.iter().any(|r| !r.text.trim().is_empty());
            let bullet = match base.bullet.as_deref() {
                Some("") | None => None,
                _ if !has_text => None,
                Some("#") => {
                    numbering += 1;
                    Some(format!("{numbering}."))
                }
                Some(b) => Some(b.to_string()),
            };
            let end = child(p, "endParaRPr").and_then(|x| num(x, "sz")).map(|v| v as f64 / 100.0);
            let size = end.or(base.size).unwrap_or(default_size) * scale;
            paras.push(Para {
                align: base.align.clone().unwrap_or_else(|| "l".into()),
                level: level as u32,
                bullet,
                line: base.line.unwrap_or(1.0) * tighter,
                before: base.before_pt.map(|p| p * scale).or(base.before_pct.map(|k| k * size)).unwrap_or(0.0),
                size,
                runs,
            });
        }
        // An empty placeholder shows only in the editor ("Click to add title").
        if paras.iter().all(|p| p.runs.iter().all(|r| r.text.trim().is_empty())) {
            paras.clear();
        }
        let inset = |k: &str, d: i64| body_pr.and_then(|b| num(b, k)).unwrap_or(d);
        let shape = Shape {
            x: place.x,
            y: place.y,
            w: place.w,
            h: place.h,
            rot: place.rot,
            flip_h: place.flip_h,
            flip_v: place.flip_v,
            geom: geom.to_string(),
            fill: fill_,
            line_w: ln.and_then(|l| num(l, "w")).map(|w| w as f64 / EMU_PER_PT).unwrap_or(if line.is_some() { 0.75 } else { 0.0 }),
            line,
            paras,
            vert: body_pr
                .and_then(|b| attr(b, "vert"))
                .map(str::to_string)
                .or_else(|| lay.and_then(|p| p.vert.clone()))
                .or_else(|| mas.and_then(|p| p.vert.clone()))
                .filter(|v| v != "horz")
                .map(|v| if v == "vert270" { v } else { "vert".into() }),
            anchor: body_pr.and_then(|b| attr(b, "anchor")).map(str::to_string).or_else(|| lay.and_then(|p| p.anchor.clone())).or_else(|| mas.and_then(|p| p.anchor.clone())).unwrap_or_else(|| "t".into()),
            inset: [inset("lIns", 91_440), inset("tIns", 45_720), inset("rIns", 91_440), inset("bIns", 45_720)],
            ..Default::default()
        };
        if shape.paras.is_empty() && shape.fill.is_none() && shape.line.is_none() {
            return;
        }
        self.shapes.push(shape);
    }

    fn pic(&mut self, pic: Node, tf: Tf) {
        if self.decor && at(pic, &["nvPicPr", "nvPr", "ph"]).is_some() {
            return;
        }
        let Some(place) = xfrm(child(pic, "spPr").and_then(|s| child(s, "xfrm"))) else { return };
        let place = tf.apply(&place);
        let fill = child(pic, "blipFill");
        let target = fill.and_then(|f| child(f, "blip")).and_then(|b| attr(b, "embed")).and_then(|id| self.rels.get(id)).map(|r| r.target.clone());
        let image = target.and_then(|t| self.pkg.picture(&t));
        let crop = fill.and_then(|f| child(f, "srcRect")).map(|r| {
            let k = |n: &str| num(r, n).unwrap_or(0) as f64 / 100_000.0;
            [k("l"), k("t"), k("r"), k("b")]
        });
        self.shapes.push(Shape {
            x: place.x,
            y: place.y,
            w: place.w,
            h: place.h,
            rot: place.rot,
            flip_h: place.flip_h,
            flip_v: place.flip_v,
            geom: "rect".into(),
            label: image.is_none().then(|| "Picture".to_string()),
            image,
            crop,
            ..Default::default()
        });
    }

    /// Tables (their cells' text), charts and SmartArt (a labelled box).
    fn frame(&mut self, fr: Node, tf: Tf) {
        if self.decor && at(fr, &["nvGraphicFramePr", "nvPr", "ph"]).is_some() {
            return;
        }
        let Some(place) = xfrm(child(fr, "xfrm")) else { return };
        let place = tf.apply(&place);
        let Some(data) = at(fr, &["graphic", "graphicData"]) else { return };
        let uri = attr(data, "uri").unwrap_or("");
        let mut shape = Shape { x: place.x, y: place.y, w: place.w, h: place.h, geom: "rect".into(), ..Default::default() };
        if let Some(tbl) = child(data, "tbl") {
            let rows = kids(tbl, "tr")
                .map(|tr| {
                    kids(tr, "tc")
                        .filter(|tc| attr(*tc, "hMerge").is_none() && attr(*tc, "vMerge").is_none())
                        .map(|tc| {
                            tc.descendants().filter(|n| n.has_tag_name_local("p")).map(|p| p.descendants().filter(|t| t.has_tag_name_local("t")).filter_map(|t| t.text()).collect::<String>()).collect::<Vec<_>>().join("\n")
                        })
                        .collect()
                })
                .collect();
            shape.table = Some(rows);
        } else {
            shape.label = Some(if uri.ends_with("/chart") { "Chart" } else if uri.ends_with("/diagram") { "SmartArt" } else { "Object" }.to_string());
        }
        self.shapes.push(shape);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn write_zip(path: &Path, parts: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, body) in parts {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

    fn rels(items: &[(&str, &str, &str)]) -> String {
        let body: String = items.iter().map(|(id, ty, target)| format!(r#"<Relationship Id="{id}" Type="{REL}/{ty}" Target="{target}"/>"#)).collect();
        format!(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#)
    }

    /// A small deck: a title placeholder placed by its layout, a text box in a group, a picture
    /// and a table.
    #[test]
    fn lays_out_a_deck() {
        let dir = std::env::temp_dir().join(format!("ferret-office-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let deck = dir.join("deck.pptx");
        let presentation = format!(r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#);
        let theme = format!(r#"<a:theme {NS}><a:themeElements><a:clrScheme name="x"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F2937"/></a:dk2><a:lt2><a:srgbClr val="EEEEEE"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1></a:clrScheme><a:fontScheme name="f"><a:majorFont><a:latin typeface="Aptos Display"/></a:majorFont><a:minorFont><a:latin typeface="Aptos"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#);
        let master = format!(r#"<p:sldMaster {NS}><p:cSld><p:bg><p:bgPr><a:solidFill><a:schemeClr val="bg1"/></a:solidFill></p:bgPr></p:bg><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="1" y="1"/><a:ext cx="2" cy="2"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2"/><p:txStyles><p:titleStyle><a:lvl1pPr algn="ctr"><a:defRPr sz="4400" b="1"><a:solidFill><a:schemeClr val="tx2"/></a:solidFill><a:latin typeface="+mj-lt"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr><a:buChar char="•"/><a:defRPr sz="2800"/></a:lvl1pPr></p:bodyStyle></p:txStyles></p:sldMaster>"#);
        let layout = format!(r#"<p:sldLayout {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="t"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="365125"/><a:ext cx="10515600" cy="1325563"/></a:xfrm></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="band"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="6000000"/><a:ext cx="12192000" cy="858000"/></a:xfrm><a:solidFill><a:srgbClr val="996633"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#);
        let slide = format!(
            r#"<p:sld {NS}><p:cSld><p:spTree>
              <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/>
                <p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang="en-US"/><a:t>Quarterly review</a:t></a:r></a:p></p:txBody></p:sp>
              <p:grpSp><p:nvGrpSpPr><p:cNvPr id="3" name="g"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>
                <p:grpSpPr><a:xfrm><a:off x="1000000" y="2000000"/><a:ext cx="2000000" cy="1000000"/><a:chOff x="0" y="0"/><a:chExt cx="1000000" cy="500000"/></a:xfrm></p:grpSpPr>
                <p:sp><p:nvSpPr><p:cNvPr id="4" name="box"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr>
                  <p:spPr><a:xfrm><a:off x="100000" y="100000"/><a:ext cx="500000" cy="200000"/></a:xfrm><a:prstGeom prst="ellipse"/><a:solidFill><a:schemeClr val="accent1"><a:lumMod val="75000"/></a:schemeClr></a:solidFill></p:spPr>
                  <p:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="r"/><a:r><a:rPr sz="1200" i="1"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:rPr><a:t>Hello</a:t></a:r><a:br/><a:r><a:t>there</a:t></a:r></a:p></p:txBody></p:sp>
              </p:grpSp>
              <p:pic><p:nvPicPr><p:cNvPr id="5" name="pic"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/><a:srcRect l="10000" r="10000"/></p:blipFill>
                <p:spPr><a:xfrm><a:off x="6000000" y="2000000"/><a:ext cx="3000000" cy="2000000"/></a:xfrm></p:spPr></p:pic>
              <p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="6" name="tbl"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm><a:off x="1000000" y="4500000"/><a:ext cx="4000000" cy="1000000"/></p:xfrm>
                <a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tr h="1"><a:tc><a:txBody><a:p><a:r><a:t>Region</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>Sales</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>
            </p:spTree></p:cSld></p:sld>"#
        );
        // 1 × 1 PNG.
        let png = "\u{89}PNG";
        write_zip(&deck, &[
            ("ppt/presentation.xml", &presentation),
            ("ppt/_rels/presentation.xml.rels", &rels(&[("rId1", "slideMaster", "slideMasters/slideMaster1.xml"), ("rId2", "slide", "slides/slide1.xml")])),
            ("ppt/slides/slide1.xml", &slide),
            ("ppt/slides/_rels/slide1.xml.rels", &rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"), ("rId2", "image", "../media/image1.png")])),
            ("ppt/slideLayouts/slideLayout1.xml", &layout),
            ("ppt/slideLayouts/_rels/slideLayout1.xml.rels", &rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml")])),
            ("ppt/slideMasters/slideMaster1.xml", &master),
            ("ppt/slideMasters/_rels/slideMaster1.xml.rels", &rels(&[("rId1", "theme", "../theme/theme1.xml")])),
            ("ppt/theme/theme1.xml", &theme),
            ("ppt/media/image1.png", png),
        ]);
        let d = read_deck(&deck.to_string_lossy()).unwrap();
        assert_eq!((d.width, d.height, d.total, d.slides.len()), (12_192_000, 6_858_000, 1, 1));
        let s = &d.slides[0];
        assert_eq!(s.background.as_deref(), Some("#ffffff"));
        assert_eq!(s.shapes.len(), 5, "the layout's band, then title, text box, picture, table");
        assert_eq!((s.shapes[0].y, s.shapes[0].fill.as_deref()), (6_000_000, Some("#996633")), "the layout's own shape, under the slide's");
        let s = &crate::office::Slide { shapes: s.shapes[1..].to_vec(), ..Default::default() };

        let title = &s.shapes[0];
        assert_eq!((title.x, title.y, title.w), (838_200, 365_125, 10_515_600), "placed by its layout");
        let r = &title.paras[0].runs[0];
        assert_eq!(r.text, "Quarterly review");
        assert_eq!((r.size, r.bold, r.color.as_deref(), r.font.as_deref()), (44.0, true, Some("#1f2937"), Some("Aptos Display")), "styled by the master");
        assert_eq!(title.paras[0].align, "ctr");

        let bx = &s.shapes[1];
        assert_eq!((bx.x, bx.y, bx.w, bx.h), (1_200_000, 2_200_000, 1_000_000, 400_000), "mapped out of its group");
        assert_eq!((bx.geom.as_str(), bx.anchor.as_str()), ("ellipse", "ctr"));
        assert_eq!(bx.fill.as_deref(), Some("#2f5597"), "accent1 25% darker");
        let p = &bx.paras[0];
        assert_eq!(p.align, "r");
        assert_eq!(p.runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Hello", "\n", "there"]);
        assert_eq!((p.runs[0].size, p.runs[0].italic, p.runs[0].color.as_deref()), (12.0, true, Some("#ff0000")));
        assert_eq!(p.runs[2].font.as_deref(), Some("Aptos"), "body text in the theme's body font");

        let pic = &s.shapes[2];
        assert!(pic.image.as_deref().is_some_and(|i| i.ends_with("image1.png") && Path::new(i).exists()));
        assert_eq!(pic.crop, Some([0.1, 0.0, 0.1, 0.0]));

        let tbl = &s.shapes[3];
        assert_eq!(tbl.table, Some(vec![vec!["Region".to_string(), "Sales".to_string()]]));

        let _ = fs::remove_dir_all(dir);
        let _ = fs::remove_dir_all(Path::new(pic.image.as_deref().unwrap()).parent().unwrap());
    }

    /// A workbook's cells come out as the sheet shows them.
    #[test]
    fn reads_a_sheet() {
        let dir = std::env::temp_dir().join(format!("ferret-office-sheet-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let book = dir.join("book.xlsx");
        let ct = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;
        let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let wb = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sales" sheetId="1" r:id="rId1"/><sheet name="Notes" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
        let wb_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
        let strings = r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="3" uniqueCount="3"><si><t>Region</t></si><si><t>Total</t></si><si><t>North</t></si></sst>"#;
        // Style 1: a date format, so 45000 reads as 2023-03-15.
        let styles = r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#;
        let sheet1 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="2"><c r="B2" t="s"><v>0</v></c><c r="C2" t="s"><v>1</v></c><c r="D2"/></row><row r="3"><c r="B3" t="s"><v>2</v></c><c r="C3"><v>0.30000000000000004</v></c><c r="D3" s="1"><v>45000</v></c></row></sheetData></worksheet>"#;
        let sheet2 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="b"><v>1</v></c></row></sheetData></worksheet>"#;
        write_zip(&book, &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", root_rels),
            ("xl/workbook.xml", wb),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/sharedStrings.xml", strings),
            ("xl/styles.xml", styles),
            ("xl/worksheets/sheet1.xml", sheet1),
            ("xl/worksheets/sheet2.xml", sheet2),
        ]);
        let p = book.to_string_lossy().to_string();
        let s = read_sheet(&p, 0).unwrap();
        assert_eq!(s.sheets, ["Sales", "Notes"]);
        assert_eq!((s.first_row, s.first_col), (1, 1), "the used range starts at B2");
        assert_eq!(s.rows[0], ["Region", "Total", ""]);
        assert_eq!(s.rows[1], ["North", "0.3", "2023-03-15"]);
        let n = read_sheet(&p, 1).unwrap();
        assert_eq!((n.sheet, n.rows[0][0].as_str()), (1, "TRUE"));
        let _ = fs::remove_dir_all(dir);
    }

    /// Real files, if there are any in ~/FerretQA/office (Office's own templates are good ones):
    /// each reads, and what came out is printed. `cargo test office_samples -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn office_samples() {
        let Some(dir) = dirs::home_dir().map(|h| h.join("FerretQA").join("office")) else { return };
        let Ok(rd) = fs::read_dir(&dir) else { return };
        for f in rd.flatten() {
            let p = f.path().to_string_lossy().to_string();
            let t = std::time::Instant::now();
            if p.ends_with(".pptx") {
                let d = read_deck(&p).unwrap_or_else(|e| panic!("{p}: {e}"));
                let shapes: usize = d.slides.iter().map(|s| s.shapes.len()).sum();
                let pics = d.slides.iter().flat_map(|s| &s.shapes).filter(|s| s.image.is_some()).count();
                let texts: Vec<String> = d.slides.iter().take(3).map(|s| s.shapes.iter().flat_map(|sh| &sh.paras).flat_map(|p| &p.runs).map(|r| r.text.as_str()).collect::<String>().chars().take(60).collect()).collect();
                let empty = d.slides.iter().filter(|s| s.shapes.is_empty()).count();
                println!("{}: {} slides ({empty} empty), {shapes} shapes, {pics} pictures in {:?}\n    {texts:?}", f.file_name().to_string_lossy(), d.total, t.elapsed());
            } else {
                let s = read_sheet(&p, 0).unwrap_or_else(|e| panic!("{p}: {e}"));
                let sample: Vec<String> = s.rows.iter().filter(|r| r.iter().any(|c| !c.is_empty())).take(3).map(|r| r.iter().filter(|c| !c.is_empty()).take(5).cloned().collect::<Vec<_>>().join(" | ")).collect();
                println!("{}: sheets {:?}, {} × {} from {:?} in {:?}\n    {sample:?}", f.file_name().to_string_lossy(), s.sheets, s.total_rows, s.total_cols, (s.first_row, s.first_col), t.elapsed());
            }
        }
    }

    #[test]
    fn numbers_read_cleanly() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(0.1 + 0.2), "0.3");
        assert_eq!(number(-1234.5), "-1234.5");
        assert_eq!(number(1.0 / 3.0), "0.3333333333");
    }

    #[test]
    fn resolves_relationship_targets() {
        assert_eq!(resolve("ppt/slides", "../media/image1.png"), "ppt/media/image1.png");
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve("ppt/slides", "/ppt/media/x.png"), "ppt/media/x.png");
    }
}
