//! What the preview shows for each kind of file (renderers.tsx, office.tsx, PdfPages.tsx):
//! read off the UI thread into `Content`, then drawn. Code is highlighted (syntect, GitHub
//! themes), Markdown rendered, JSON a tree, CSV a table, workbooks a grid with sheet tabs, decks
//! their slides, archives their contents, fonts a sample, PDFs their pages (Windows' own PDF
//! renderer), programs their version info, anything else its bytes.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, Context, FontStyle, FontWeight, HighlightStyle, Hsla, Image, ImageFormat, ListHorizontalSizingBehavior, ObjectFit,
    SharedString, StrikethroughStyle, StyledText, UnderlineStyle, UniformListScrollHandle, div, prelude::*, px, relative,
    uniform_list,
};

use crate::imgmem::img;

use crate::selectable::{Builder, Shared, hbar};

use crate::kinds::Kind;
use crate::theme as t;
use crate::ui::icon;
use crate::view::Ferret;

pub const TEXT_MAX: u64 = 300 * 1024;
/// Longest line shown whole (longer ones are cut, unhighlighted): long enough for a long JSON
/// value or log line, wrapped or scrolled to the side.
pub const LINE_MAX: usize = 10_000;
/// JSON files read whole up to this size (for the tree).
pub const JSON_MAX: u64 = 5 * 1024 * 1024;

/// One highlighted line: its text and colored ranges.
pub struct Line {
    pub text: SharedString,
    pub colors: Vec<(Range<usize>, u32, bool, bool)>,
}

pub enum Md {
    Heading(u8, Vec<Span>),
    Para(Vec<Span>),
    Code(Arc<Vec<Line>>),
    Item { depth: usize, marker: String, spans: Vec<Span> },
    Quote(Vec<Span>),
    Rule,
    Image(String),
    Table(Vec<Vec<String>>),
}

#[derive(Clone, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

pub enum Content {
    Picture { image: Arc<Image>, w: u32, h: u32 },
    Video(ferret_core::media::VideoInfo),
    Audio(ferret_core::media::AudioInfo, Option<Arc<Image>>),
    Word,
    Text { lines: Arc<Vec<Line>>, truncated: bool, encoding: String, language: &'static str, total_lines: u64 },
    Markdown { blocks: Vec<Md>, source: Arc<Vec<Line>>, words: usize },
    Json { value: Arc<serde_json::Value>, source: Arc<Vec<Line>> },
    BadJson { error: String, source: Arc<Vec<Line>> },
    Csv { rows: Arc<Vec<Vec<String>>>, delimiter: char },
    Sheet(ferret_core::office::SheetPreview),
    /// The deck, each slide's background picture, and its shapes drawn ahead (turned, vertical).
    Deck(Arc<ferret_core::office::Deck>, Vec<Option<Arc<Image>>>, Arc<Vec<Vec<Option<crate::slidedraw::Drawn>>>>),
    Archive(ferret_core::archive::ArchiveInfo),
    Font { family: SharedString, bytes: Option<Vec<u8>> },
    /// Each page (up to 200): its picture once rendered (the first few at once, the rest as they
    /// scroll near), and its height for a width of 1; `real`: the file read (an archive entry's or
    /// server file's local copy), for more pages and its text.
    Pdf { pages: Vec<(Option<Arc<Image>>, f32)>, count: u32, real: String },
    /// A PDF that wants a password none of this session's opens (`tried` of them were tried).
    PdfLocked { real: String, tried: usize },
    Program(ferret_core::details::FileDetails, Option<Arc<Image>>),
    Shell(Arc<Image>),
    Hex(Vec<u8>),
    Empty,
    Nothing(String),
    /// A document's text (paragraphs; true for headings), for formats with no renderer.
    DocText(Vec<(bool, String)>),
}

/// How the preview's options are set (wrap, source/rendered, raw JSON, CSV sort, open nodes).
#[derive(Default)]
pub struct View {
    pub wrap: bool,
    pub source: bool,
    pub raw: bool,
    pub hex: bool,
    pub sort: Option<(usize, bool)>,
    pub open: HashSet<String>,
    pub zoom: f32,
    pub zoom_at: (f32, f32),
    /// The Word preview's Ctrl+wheel zoom (0 = 100 %).
    pub word_zoom: f32,
}

/// What a screen reader reads of a preview: its text, for the kinds that are text (up to 64 KB).
pub fn spoken_text(content: &Content) -> Option<String> {
    const MAX: usize = 64 * 1024;
    let mut out = String::new();
    let mut push = |line: &str| {
        if out.len() < MAX {
            out.push_str(line);
            out.push('\n');
        }
    };
    let spans = |s: &[Span]| s.iter().map(|s| s.text.as_str()).collect::<String>();
    match content {
        Content::Text { lines, .. } | Content::Json { source: lines, .. } | Content::BadJson { source: lines, .. } => lines.iter().for_each(|l| push(&l.text)),
        Content::Markdown { blocks, .. } => {
            for b in blocks {
                match b {
                    Md::Heading(_, s) | Md::Para(s) | Md::Quote(s) => push(&spans(s)),
                    Md::Item { marker, spans: s, .. } => push(&format!("{marker} {}", spans(s))),
                    Md::Code(lines) => lines.iter().for_each(|l| push(&l.text)),
                    Md::Table(rows) => rows.iter().for_each(|r| push(&r.join(", "))),
                    Md::Rule | Md::Image(_) => {}
                }
            }
        }
        Content::DocText(paras) => paras.iter().for_each(|(_, t)| push(t)),
        Content::Csv { rows, .. } => rows.iter().take(500).for_each(|r| push(&r.join(", "))),
        Content::Nothing(why) => push(why),
        _ => return None,
    }
    Some(out)
}
/// The highlighter's language tables, and when they were last used.
static SYNTAXES: std::sync::Mutex<Option<(Arc<syntect::parsing::SyntaxSet>, std::time::Instant)>> = std::sync::Mutex::new(None);

/// The highlighter's language tables: loaded when a code or text file is first previewed (tens
/// of ms, off the UI thread), and let go again once unused for a while (`forget_syntaxes`).
/// Kept for good, they were the biggest thing a window still held after a session: about 40 MB
/// in some 120,000 small blocks (each language's patterns are built as it's first used), for
/// the rest of the window's life after one look at a few code files.
fn syntaxes() -> Arc<syntect::parsing::SyntaxSet> {
    let mut s = SYNTAXES.lock().unwrap_or_else(|e| e.into_inner());
    let (set, used) = s.get_or_insert_with(|| (Arc::new(syntect::parsing::SyntaxSet::load_defaults_newlines()), std::time::Instant::now()));
    *used = std::time::Instant::now();
    set.clone()
}

/// Lets go of the highlighter's tables when no preview used them for `idle`. True if it did.
pub fn forget_syntaxes(idle: std::time::Duration) -> bool {
    let mut s = SYNTAXES.lock().unwrap_or_else(|e| e.into_inner());
    // Not while a preview is being highlighted (it holds them).
    if s.as_ref().is_some_and(|(set, used)| used.elapsed() >= idle && Arc::strong_count(set) == 1) {
        *s = None;
        return true;
    }
    false
}

pub fn language(ext: &str, name: &str) -> &'static str {
    match ext {
        "rs" => "Rust",
        "py" | "pyw" => "Python",
        "js" | "mjs" | "cjs" | "jsx" => "JavaScript",
        "ts" | "mts" | "cts" | "tsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "cs" => "C#",
        "c" | "h" => "C",
        "cpp" | "cc" | "hpp" | "cxx" => "C++",
        "rb" => "Ruby",
        "php" => "PHP",
        "lua" => "Lua",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" => "PowerShell",
        "bat" | "cmd" => "Batch",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" | "scss" | "less" => "CSS",
        "json" | "jsonc" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "xml" | "svg" | "csproj" | "props" => "XML",
        "md" | "markdown" => "Markdown",
        "ini" | "cfg" | "conf" => "INI",
        "swift" => "Swift",
        "dart" => "Dart",
        "scala" => "Scala",
        "hs" => "Haskell",
        "ex" | "exs" => "Elixir",
        "r" => "R",
        "pl" => "Perl",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "zig" => "Zig",
        "diff" | "patch" => "Diff",
        "log" => "Log",
        _ => match name.to_lowercase().as_str() {
            "dockerfile" => "Dockerfile",
            "makefile" => "Makefile",
            ".gitignore" | ".gitattributes" | ".editorconfig" => "Config",
            _ => "Plain text",
        },
    }
}

/// Lines with GitHub dark/light colors (syntect's InspiredGitHub stands in for light).
pub fn highlight(text: &str, ext: &str, name: &str) -> Vec<Line> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::FontStyle as Fs;
    let ss = syntaxes();
    let ss = &*ss;
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_extension(match ext {
            "ts" | "tsx" | "mts" | "cts" => "js",
            "jsonc" => "json",
            "ps1" | "psm1" => "sh",
            "toml" | "ini" | "cfg" | "conf" => "yaml",
            "vue" | "svelte" => "html",
            "kt" | "kts" => "java",
            "scss" | "less" => "css",
            _ => "",
        }))
        .or_else(|| ss.find_syntax_by_token(&name.to_lowercase()))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    // The app's colours: GitHub's dark / light themes (as its shiki highlighter).
    static GITHUB: OnceLock<[syntect::highlighting::Theme; 2]> = OnceLock::new();
    let themes = GITHUB.get_or_init(|| [crate::codetheme::github(true), crate::codetheme::github(false)]);
    let theme = &themes[if t::is_dark() { 0 } else { 1 }];
    let mut h = HighlightLines::new(syntax, theme);
    let started = std::time::Instant::now();
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(text) {
        // Highlighting that takes too long (a huge minified file) stops: the rest is plain.
        let plain = started.elapsed().as_millis() > 200;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        // A very long line (minified JSON, a log on one line) is cut for display and not
        // highlighted: laying out hundreds of kilobytes of one line would stop the window.
        if trimmed.len() > LINE_MAX {
            let mut end = LINE_MAX;
            while !trimmed.is_char_boundary(end) {
                end -= 1;
            }
            out.push(Line { text: format!("{}  … ({} more characters)", &trimmed[..end], trimmed.len() - end).into(), colors: Vec::new() });
            continue;
        }
        if plain {
            out.push(Line { text: trimmed.to_string().into(), colors: Vec::new() });
            continue;
        }
        let mut colors = Vec::new();
        let mut at = 0;
        if let Ok(spans) = h.highlight_line(line, ss) {
            for (style, s) in spans {
                let end = (at + s.len()).min(trimmed.len());
                if end > at {
                    let c = style.foreground;
                    colors.push((at..end, (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32, style.font_style.contains(Fs::BOLD), style.font_style.contains(Fs::ITALIC)));
                }
                at += s.len();
            }
        }
        out.push(Line { text: trimmed.to_string().into(), colors });
    }
    out
}

/// A picture made to fit `long` px on its longest side, turned as its EXIF says (the copy has
/// none): JPEG (quality 92) unless it has transparency (PNG, fast). GPUI keeps what it draws
/// decoded, so a big photo drawn small cost its full size in memory (48 MB for 12 MP).
fn smaller_picture(bytes: &[u8], long: u32) -> Option<Image> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    let small = img.resize(long, long, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    if small.color().has_alpha() {
        let enc = image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive);
        small.write_with_encoder(enc).ok()?;
        Some(Image::from_bytes(ImageFormat::Png, out))
    } else {
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
        image::DynamicImage::ImageRgb8(small.to_rgb8()).write_with_encoder(enc).ok()?;
        Some(Image::from_bytes(ImageFormat::Jpeg, out))
    }
}

/// Reads what the preview of `path` needs (runs off the UI thread).
/// `px`: the width (device pixels) pages are drawn at, so they show 1:1 (PDFs).
pub fn load(path: &str, ext: &str, kind: Kind, size: u64, px: u32) -> Content {
    let name = crate::util::file_name(path);
    match kind {
        Kind::Image if matches!(ext, "png" | "jpg" | "jpeg" | "jfif" | "gif" | "webp" | "bmp" | "ico" | "svg") => {
            // GPUI decodes these itself; the picture's size comes from its header.
            let bytes = std::fs::read(path).ok();
            match bytes {
                Some(b) => {
                    let format = match ext {
                        "png" => ImageFormat::Png,
                        "gif" => ImageFormat::Gif,
                        "webp" => ImageFormat::Webp,
                        "bmp" => ImageFormat::Bmp,
                        "ico" => ImageFormat::Ico,
                        "svg" => ImageFormat::Svg,
                        _ => ImageFormat::Jpeg,
                    };
                    // As shown: a photo turned by its EXIF orientation has its sides swapped.
                    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&b))
                        .with_guessed_format()
                        .ok()
                        .and_then(|r| r.into_decoder().ok())
                        .map(|mut d| {
                            use image::ImageDecoder;
                            let (w, h) = d.dimensions();
                            let turned = matches!(
                                d.orientation(),
                                Ok(image::metadata::Orientation::Rotate90
                                    | image::metadata::Orientation::Rotate270
                                    | image::metadata::Orientation::Rotate90FlipH
                                    | image::metadata::Orientation::Rotate270FlipH)
                            );
                            if turned { (h, w) } else { (w, h) }
                        })
                        .unwrap_or((0, 0));
                    // Much bigger than it's shown: a copy at that size instead (`px`: its longest
                    // side; 0 for the original, zoomed in). Animated GIFs, icons and SVGs as they are.
                    if px > 0 && !matches!(ext, "gif" | "svg" | "ico") && w.max(h) as f32 > px as f32 * 1.15 {
                        if let Some(small) = smaller_picture(&b, px) {
                            return Content::Picture { image: Arc::new(small), w, h };
                        }
                    }
                    Content::Picture { image: Arc::new(Image::from_bytes(format, b)), w, h }
                }
                None => Content::Nothing("Couldn't read this picture".into()),
            }
        }
        // HEIC, TIFF, RAW, PSD…: Windows' own decoder, as a thumbnail.
        Kind::Image => match ferret_core::shell::thumbnail_png(path, 1024, false) {
            Ok(png) => Content::Picture { image: Arc::new(Image::from_bytes(ImageFormat::Png, png)), w: 0, h: 0 },
            Err(_) => Content::Nothing("No preview for this picture".into()),
        },
        Kind::Video => match ferret_core::rt::block_on(ferret_core::media::video_info(path.to_string())) {
            Ok(v) => Content::Video(v),
            Err(_) => Content::Nothing("Open it in your player".into()),
        },
        Kind::Audio => {
            let cover = ferret_core::shell::thumbnail_png(path, 256, false).ok().map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
            match ferret_core::rt::block_on(ferret_core::media::audio_info(path.to_string())) {
                Ok(a) => Content::Audio(a, cover),
                Err(e) => Content::Nothing(e),
            }
        }
        Kind::Doc if matches!(ext, "docx" | "docm" | "dotx") => Content::Word,
        Kind::Doc if matches!(ext, "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppsm") => match ferret_core::rt::block_on(ferret_core::office::slides_preview(path.to_string())) {
            Ok(d) => {
                let pics = d.slides.iter().map(|s| s.background_image.as_ref().and_then(|p| std::fs::read(p).ok()).map(|b| Arc::new(Image::from_bytes(guess(&b), b)))).collect();
                // Turned shapes and vertical text, drawn ahead (GPUI can't turn them).
                let drawn = Arc::new(crate::slidedraw::draw_deck(&d));
                Content::Deck(Arc::new(d), pics, drawn)
            }
            Err(e) => Content::Nothing(e),
        },
        Kind::Sheet if matches!(ext, "csv" | "tsv") => match ferret_core::rt::block_on(ferret_core::fileops::read_text(path.to_string(), Some(2 * 1024 * 1024))) {
            Ok(t) => {
                let d = detect_delimiter(&t.text, ext);
                Content::Csv { rows: Arc::new(parse_csv(&t.text, d)), delimiter: d }
            }
            Err(e) => Content::Nothing(e),
        },
        Kind::Sheet if matches!(ext, "xlsx" | "xlsm" | "xltx" | "xltm" | "xlsb" | "xls" | "xla" | "ods") => match ferret_core::rt::block_on(ferret_core::office::sheet_preview(path.to_string(), None)) {
            Ok(s) => Content::Sheet(s),
            Err(e) => Content::Nothing(e),
        },
        Kind::Pdf => match pdf_pages(path, 200, px) {
            Ok((pages, count)) => Content::Pdf { pages, count, real: path.to_string() },
            Err(PdfError::Locked(tried)) => Content::PdfLocked { real: path.to_string(), tried },
            Err(PdfError::Other(e)) => Content::Nothing(e),
        },
        Kind::Archive => match ferret_core::rt::block_on(ferret_core::archive::archive_info(path.to_string())) {
            Ok(a) => Content::Archive(a),
            Err(e) => Content::Nothing(e),
        },
        Kind::Font => match std::fs::read(path) {
            Ok(bytes) => match font_family(&bytes) {
                Some(family) => Content::Font { family: family.into(), bytes: Some(bytes) },
                None => Content::Nothing("Couldn't read this font".into()),
            },
            Err(e) => Content::Nothing(e.to_string()),
        },
        Kind::Exe if matches!(ext, "exe" | "dll" | "sys" | "ocx" | "cpl" | "scr" | "mui" | "efi" | "winmd") => {
            // Big enough to stay sharp at 88 px on a scaled screen (the app asks for 256).
            let icon = ferret_core::shell::thumbnail_png(path, 256, true).ok().map(|b| Arc::new(Image::from_bytes(ImageFormat::Png, b)));
            match ferret_core::rt::block_on(ferret_core::details::file_details(path.to_string())) {
                Ok(d) => Content::Program(d, icon),
                Err(e) => Content::Nothing(e),
            }
        }
        Kind::Sheet if matches!(ext, "ods" | "ots") => match crate::doctext::ods_rows(path) {
            Ok(rows) => Content::Sheet(ferret_core::office::SheetPreview {
                sheets: vec!["Sheet1".into()],
                sheet: 0,
                total_rows: rows.len(),
                total_cols: rows.iter().map(|r| r.len()).max().unwrap_or(0),
                rows,
                first_row: 0,
                first_col: 0,
            }),
            Err(e) => Content::Nothing(e),
        },
        // Documents with no renderer of their own: their text (read by Windows, or from the file),
        // else Windows' thumbnail.
        Kind::Doc if !matches!(ext, "key" | "pages") => match crate::doctext::doc_text(path, ext) {
            Ok(paras) => Content::DocText(paras),
            Err(_) => match ferret_core::shell::thumbnail_png(path, 512, false) {
                Ok(png) => Content::Shell(Arc::new(Image::from_bytes(ImageFormat::Png, png))),
                Err(_) => Content::Nothing(format!("No preview for {}", crate::kinds::type_label(false, ext))),
            },
        },
        Kind::Doc | Kind::Exe => match ferret_core::shell::thumbnail_png(path, 512, false) {
            Ok(png) => Content::Shell(Arc::new(Image::from_bytes(ImageFormat::Png, png))),
            // No thumbnail from Windows: shown as text if it is text (RTF), else said so.
            Err(_) => match ferret_core::rt::block_on(ferret_core::fileops::read_text(path.to_string(), Some(TEXT_MAX))) {
                Ok(t) if !t.binary && size > 0 => text_content(&t.text, ext, &name, t.truncated, t.encoding, t.lines),
                _ => Content::Nothing(format!("No preview for {}", crate::kinds::type_label(false, ext))),
            },
        },
        _ => {
            if size == 0 {
                return Content::Empty;
            }
            match ferret_core::rt::block_on(ferret_core::fileops::read_text(path.to_string(), Some(if ext == "json" { JSON_MAX } else { TEXT_MAX }))) {
                Ok(t) if !t.binary => text_content(&t.text, ext, &name, t.truncated, t.encoding, t.lines),
                // Binary: its first bytes.
                _ => match ferret_core::rt::block_on(ferret_core::fileops::read_bytes(path.to_string(), Some(512))) {
                    Ok(b) => Content::Hex(b),
                    Err(e) => Content::Nothing(e),
                },
            }
        }
    }
}

fn guess(b: &[u8]) -> ImageFormat {
    match b.get(..4) {
        Some([0x89, b'P', b'N', b'G']) => ImageFormat::Png,
        Some([b'G', b'I', b'F', _]) => ImageFormat::Gif,
        Some([b'R', b'I', b'F', b'F']) => ImageFormat::Webp,
        Some([b'B', b'M', _, _]) => ImageFormat::Bmp,
        _ => ImageFormat::Jpeg,
    }
}

fn text_content(text: &str, ext: &str, name: &str, truncated: bool, encoding: String, lines: Option<u64>) -> Content {
    let total = lines.unwrap_or(text.lines().count() as u64);
    match ext {
        "md" | "markdown" => {
            let source = Arc::new(highlight(text, "md", name));
            Content::Markdown { blocks: markdown(text), source, words: text.split_whitespace().count() }
        }
        "json" => {
            // The tree from the whole file; its source only from the first 300 KB.
            let mut cut = (TEXT_MAX as usize).min(text.len());
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let source = Arc::new(highlight(&text[..cut], "json", name));
            match serde_json::from_str::<serde_json::Value>(text) {
                Ok(v) => Content::Json { value: Arc::new(v), source },
                Err(e) => Content::BadJson { error: e.to_string(), source },
            }
        }
        _ => Content::Text { lines: Arc::new(highlight(text, ext, name)), truncated, encoding, language: language(ext, name), total_lines: total },
    }
}

fn detect_delimiter(text: &str, ext: &str) -> char {
    if ext == "tsv" {
        return '\t';
    }
    let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    [',', ';', '\t', '|'].into_iter().max_by_key(|d| head.matches(*d).count()).unwrap_or(',')
}

pub fn parse_csv(text: &str, d: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == d {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' {
            row.push(std::mem::take(&mut cell).trim_end_matches('\r').to_string());
            rows.push(std::mem::take(&mut row));
            if rows.len() >= 20_000 {
                break;
            }
        } else {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

pub fn markdown(text: &str) -> Vec<Md> {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
    let mut out = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut style = Span::default();
    let mut list: Vec<Option<u64>> = Vec::new();
    let mut code: Option<(String, String)> = None;
    let mut heading: Option<u8> = None;
    let mut quote = 0;
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut cell = String::new();
    let flush = |spans: &mut Vec<Span>, out: &mut Vec<Md>, heading: Option<u8>, list: &[Option<u64>], quote: usize| {
        if spans.iter().all(|s| s.text.trim().is_empty()) {
            spans.clear();
            return;
        }
        let s = std::mem::take(spans);
        out.push(if let Some(h) = heading {
            Md::Heading(h, s)
        } else if let Some(last) = list.last() {
            Md::Item { depth: list.len() - 1, marker: last.map(|n| format!("{n}.")).unwrap_or_else(|| "•".into()), spans: s }
        } else if quote > 0 {
            Md::Quote(s)
        } else {
            Md::Para(s)
        });
    };
    for ev in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    _ => 4,
                })
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut spans, &mut out, heading, &list, quote);
                heading = None;
            }
            Event::End(TagEnd::Paragraph) => flush(&mut spans, &mut out, None, &list, quote),
            Event::Start(Tag::List(start)) => {
                flush(&mut spans, &mut out, None, &list, quote);
                list.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut spans, &mut out, None, &list, quote);
                list.pop();
            }
            Event::End(TagEnd::Item) => {
                flush(&mut spans, &mut out, None, &list, quote);
                if let Some(Some(n)) = list.last_mut() {
                    *n += 1;
                }
            }
            Event::Start(Tag::BlockQuote(_)) => quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut spans, &mut out, None, &list, quote);
                quote -= 1;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                    _ => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, src)) = code.take() {
                    out.push(Md::Code(Arc::new(highlight(src.trim_end(), &lang, ""))));
                }
            }
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strike = true,
            Event::End(TagEnd::Strikethrough) => style.strike = false,
            Event::Start(Tag::Link { dest_url, .. }) => style.link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => style.link = None,
            Event::Start(Tag::Image { dest_url, .. }) => {
                flush(&mut spans, &mut out, heading, &list, quote);
                out.push(Md::Image(dest_url.to_string()));
            }
            Event::Start(Tag::Table(_)) => table = Some(Vec::new()),
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    out.push(Md::Table(t));
                }
            }
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.push(Vec::new());
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(t) = table.as_mut() {
                    if let Some(r) = t.last_mut() {
                        r.push(std::mem::take(&mut cell));
                    }
                }
            }
            Event::Text(s) => {
                if let Some((_, src)) = code.as_mut() {
                    src.push_str(&s);
                } else if table.is_some() {
                    cell.push_str(&s);
                } else {
                    spans.push(Span { text: s.to_string(), ..style.clone() });
                }
            }
            Event::Code(s) => {
                if table.is_some() {
                    cell.push_str(&s);
                } else {
                    spans.push(Span { text: s.to_string(), code: true, ..style.clone() });
                }
            }
            Event::SoftBreak => spans.push(Span { text: " ".into(), ..style.clone() }),
            Event::HardBreak => spans.push(Span { text: "\n".into(), ..style.clone() }),
            Event::Rule => out.push(Md::Rule),
            Event::TaskListMarker(done) => spans.push(Span { text: if done { "☑ ".into() } else { "☐ ".into() }, ..Span::default() }),
            _ => {}
        }
    }
    flush(&mut spans, &mut out, None, &list, quote);
    out
}

/// Adds a previewed font file to GPUI's text system, once a session per file. GPUI can't take a
/// font out again, and each add copied the file into DirectWrite's in-memory loader and rebuilt
/// its font set with every font added so far: previewing the same font again (after it left the
/// preview cache) grew the app each time.
pub fn add_font_once(bytes: Vec<u8>, cx: &mut gpui::App) {
    static ADDED: std::sync::Mutex<Option<HashSet<u64>>> = std::sync::Mutex::new(None);
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    };
    let new = ADDED.lock().map(|mut a| a.get_or_insert_with(HashSet::new).insert(id)).unwrap_or(true);
    if new {
        let _ = cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
    }
}

/// A font file's family name (its `name` table), once it's added to GPUI.
fn font_family(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    face.names()
        .into_iter()
        .filter(|n| n.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY || n.name_id == ttf_parser::name_id::FAMILY)
        .filter(|n| n.is_unicode())
        .find_map(|n| n.to_string())
}

/// Pages rendered when a PDF is first shown; the rest follow as they scroll near. Two: what
/// the pane shows of a PDF at first (it was eight: each a decoded picture kept for a minute, so
/// stepping past a few PDFs cost hundreds of MB, and each took four times as long to show).
pub const PDF_FIRST: usize = 2;

/// Why a PDF didn't open.
pub enum PdfError {
    /// It wants a password: none, nor any of the `n` typed this session, opens it.
    Locked(usize),
    Other(String),
}

/// Windows.Data.Pdf's answer to a missing or wrong password (ERROR_WRONG_PASSWORD).
fn wrong_password(e: &windows::core::Error) -> bool {
    e.code().0 as u32 == 0x8007_052B
}

fn pdf_file(path: &str) -> Result<windows::Storage::StorageFile, PdfError> {
    let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
    let err = |e: windows::core::Error| PdfError::Other(e.message().to_string());
    windows::Storage::StorageFile::GetFileFromPathAsync(&windows::core::HSTRING::from(path)).map_err(err)?.join().map_err(err)
}

fn pdf_load(file: &windows::Storage::StorageFile, password: Option<&str>) -> windows::core::Result<windows::Data::Pdf::PdfDocument> {
    use windows::Data::Pdf::PdfDocument;
    match password {
        Some(p) => PdfDocument::LoadFromFileWithPasswordAsync(file, &windows::core::HSTRING::from(p))?.join(),
        None => PdfDocument::LoadFromFileAsync(file)?.join(),
    }
}

/// The passwords to try on a PDF, in order: the one that opened it before this session, none,
/// then each password typed this session. And how many of them are passwords (for "tried n").
fn pdf_tries(path: &str) -> (Vec<Option<String>>, usize) {
    let own = crate::pdftext::password_for(path);
    let others: Vec<String> = crate::pdftext::typed_passwords().into_iter().filter(|p| Some(p) != own.as_ref()).collect();
    let typed = others.len() + own.is_some() as usize;
    (own.into_iter().map(Some).chain([None]).chain(others.into_iter().map(Some)).collect(), typed)
}

/// Opens a PDF with the first of `tries` that works: which one, and the document. None: it's
/// locked and none of them opens it.
fn pdf_open_here(path: &str, tries: &[Option<String>]) -> Result<Option<(usize, windows::Data::Pdf::PdfDocument)>, String> {
    let file = pdf_file(path).map_err(|e| match e {
        PdfError::Locked(_) => "it needs its password".to_string(),
        PdfError::Other(e) => e,
    })?;
    for (i, password) in tries.iter().enumerate() {
        match pdf_load(&file, password.as_deref()) {
            Ok(doc) => return Ok(Some((i, doc))),
            Err(e) if wrong_password(&e) => continue,
            Err(e) => return Err(e.message().to_string()),
        }
    }
    Ok(None)
}

/// A PDF's page count, every page's shape (up to `max`) and its first `first` pages drawn `px`
/// wide (PNG), opened with the first of `tries` that works (which one comes back too). None:
/// locked. This is the work itself, with Windows' PDF renderer: done in the helper
/// (`ocrhelper.rs`), which is where the renderer then stays loaded; in Ferret only when the
/// helper can't be reached.
#[allow(clippy::type_complexity)]
pub(crate) fn pdf_pages_here(path: &str, tries: &[Option<String>], px: u32, first: u32, max: u32) -> Result<Option<(usize, u32, Vec<f32>, Vec<(u32, Vec<u8>)>)>, String> {
    let err = |e: windows::core::Error| e.message().to_string();
    let Some((with, doc)) = pdf_open_here(path, tries)? else { return Ok(None) };
    let count = doc.PageCount().map_err(err)?;
    let (mut shapes, mut drawn) = (Vec::new(), Vec::new());
    for i in 0..count.min(max) {
        let size = doc.GetPage(i).and_then(|p| p.Size()).map_err(err)?;
        shapes.push(size.Height / size.Width.max(1.));
        if i < first {
            drawn.push((i, pdf_render(&doc, i, px)?));
        }
    }
    Ok(Some((with, count, shapes, drawn)))
}

/// More of a PDF's pages drawn (see `pdf_pages_here`).
pub(crate) fn pdf_more_here(path: &str, password: Option<&str>, which: &[u32], px: u32) -> Result<Vec<(u32, Vec<u8>)>, String> {
    let doc = pdf_open_with(path, password)?;
    Ok(which.iter().filter_map(|&i| pdf_render(&doc, i, px).ok().map(|png| (i, png))).collect())
}

/// Opens a PDF with this password (or none): the OCR helper's way in (`ocrhelper.rs`), which
/// doesn't share Ferret's memory of the passwords typed this session (Ferret passes the one).
pub(crate) fn pdf_open_with(path: &str, password: Option<&str>) -> Result<windows::Data::Pdf::PdfDocument, String> {
    let file = pdf_file(path).map_err(|e| match e {
        PdfError::Locked(_) => "it needs its password".to_string(),
        PdfError::Other(e) => e,
    })?;
    pdf_load(&file, password).map_err(|e| if wrong_password(&e) { "it needs its password".to_string() } else { e.message().to_string() })
}

/// Tries a typed password on a PDF; kept for the session if it opens it. Err(None): wrong
/// password; Err(Some(why)): it couldn't be opened for another reason.
pub fn pdf_unlock(path: &str, password: &str) -> Result<(), Option<String>> {
    use crate::ocrhelper::Failed;
    let opened = match crate::ocrhelper::unlock(path, password) {
        Ok(opened) => opened,
        Err(Failed::Pdf(e)) => return Err(Some(e)),
        Err(Failed::Helper(e)) => {
            log::warn!(target: "ferret::ocr", "trying a PDF's password in Ferret itself: {e}");
            pdf_pages_here(path, &[Some(password.to_string())], 0, 0, 0).map_err(Some)?.is_some()
        }
    };
    if !opened {
        return Err(None);
    }
    crate::pdftext::remember(path, password);
    Ok(())
}

/// One page as a picture `px` wide, a PNG (Windows.Data.Pdf).
fn pdf_render(doc: &windows::Data::Pdf::PdfDocument, i: u32, px: u32) -> Result<Vec<u8>, String> {
    use windows::Data::Pdf::PdfPageRenderOptions;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    let err = |e: windows::core::Error| e.message().to_string();
    let page = doc.GetPage(i).map_err(err)?;
    let size = page.Size().map_err(err)?;
    let opts = PdfPageRenderOptions::new().map_err(err)?;
    // Rendered at the width it is shown at: shrinking a page's text afterwards blurs it.
    let w = px.clamp(200, 4000);
    opts.SetDestinationWidth(w).map_err(err)?;
    opts.SetDestinationHeight((w as f32 * size.Height / size.Width.max(1.)) as u32).map_err(err)?;
    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(err)?.join().map_err(err)?;
    let n = stream.Size().map_err(err)? as u32;
    stream.Seek(0).map_err(err)?;
    let reader = DataReader::CreateDataReader(&stream).map_err(err)?;
    reader.LoadAsync(n).map_err(err)?.join().map_err(err)?;
    let mut bytes = vec![0u8; n as usize];
    reader.ReadBytes(&mut bytes).map_err(err)?;
    Ok(bytes)
}

fn page_image(png: Vec<u8>) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Png, png))
}

/// A PDF's pages as pictures: every page's shape (up to `max`), the first few drawn. Drawn in
/// the helper process; the password that opened it is kept for this file.
fn pdf_pages(path: &str, max: u32, px: u32) -> Result<(Vec<(Option<Arc<Image>>, f32)>, u32), PdfError> {
    use crate::ocrhelper::{Failed, Pages};
    let (tries, typed) = pdf_tries(path);
    let found = match crate::ocrhelper::pages(path, &tries, px, PDF_FIRST as u32, max) {
        Ok(Pages::Locked) => None,
        Ok(Pages::Opened { with, count, shapes, drawn }) => Some((with, count, shapes, drawn)),
        Err(Failed::Pdf(e)) => return Err(PdfError::Other(e)),
        Err(Failed::Helper(e)) => {
            log::warn!(target: "ferret::ocr", "drawing a PDF in Ferret itself: {e}");
            pdf_pages_here(path, &tries, px, PDF_FIRST as u32, max).map_err(PdfError::Other)?
        }
    };
    let Some((with, count, shapes, drawn)) = found else { return Err(PdfError::Locked(typed)) };
    // A typed password that opened another file opens this one too: kept for it.
    if let Some(Some(p)) = tries.get(with).filter(|_| with > 0 || crate::pdftext::password_for(path).is_none()) {
        crate::pdftext::remember(path, p);
    }
    let mut drawn: std::collections::HashMap<u32, Vec<u8>> = drawn.into_iter().collect();
    let pages = shapes.into_iter().enumerate().map(|(i, shape)| (drawn.remove(&(i as u32)).map(page_image), shape)).collect();
    Ok((pages, count))
}

/// More pages of a PDF (`which`), `px` wide, as they scroll near.
pub fn pdf_more(path: &str, which: Vec<u32>, px: u32) -> Vec<(u32, Arc<Image>)> {
    let password = crate::pdftext::password_for(path);
    let drawn = match crate::ocrhelper::more(path, password.as_deref(), &which, px) {
        Ok(drawn) => drawn,
        Err(crate::ocrhelper::Failed::Pdf(_)) => Vec::new(),
        Err(crate::ocrhelper::Failed::Helper(e)) => {
            log::warn!(target: "ferret::ocr", "drawing a PDF's pages in Ferret itself: {e}");
            pdf_more_here(path, password.as_deref(), &which, px).unwrap_or_default()
        }
    };
    drawn.into_iter().map(|(i, png)| (i, page_image(png))).collect()
}

// ───────────────────────────── drawing ─────────────────────────────

fn rgb(c: u32) -> Hsla {
    t::rgba(c, 1.)
}

pub fn line_text(l: &Line) -> StyledText {
    let hl: Vec<(Range<usize>, HighlightStyle)> = l
        .colors
        .iter()
        .map(|(r, c, b, i)| {
            (r.clone(), HighlightStyle { color: Some(rgb(*c)), font_weight: b.then_some(FontWeight::BOLD), font_style: i.then_some(FontStyle::Italic), ..Default::default() })
        })
        .collect();
    StyledText::new(l.text.clone()).with_highlights(hl)
}

/// A code line: selectable as line `i` of `sel` (each line is a segment), or plain.
fn code_line(l: &Line, i: usize, sel: Option<&Shared>) -> AnyElement {
    match sel {
        Some(s) => crate::selectable::SelText::new(line_text(l), i, s).into_any_element(),
        None => line_text(l).into_any_element(),
    }
}

/// Code: numbered rows, drawn only where visible; wrapped, or scrolling sideways (as far as the
/// longest line) with a scrollbar under it. Its text is selectable when `sel` is given (the caller
/// puts it in a `selectable::area` over `selectable::Lines`); `scroll`: the handle it scrolls
/// with (`Sel::list_scroller`), kept when Wrap switches.
pub fn code_view(id: &'static str, lines: Arc<Vec<Line>>, wrap: bool, sel: Option<&Shared>, scroll: Option<UniformListScrollHandle>, cx: &mut Context<Ferret>) -> AnyElement {
    let n = lines.len();
    let digits = n.to_string().len().max(2) as f32;
    let sel = sel.cloned();
    let base = scroll.as_ref().map(|s| s.0.borrow().base_handle.clone());
    if wrap {
        return div()
            .id(id)
            .when_some(base, |d, h| d.track_scroll(&h))
            .size_full()
            .overflow_y_scroll()
            .font_family(t::MONO)
            .text_size(px(12.))
            .children(lines.iter().enumerate().take(4000).map(|(i, l)| {
                div()
                    .flex()
                    .gap(px(12.))
                    .child(div().flex_none().w(px(digits * 7.5)).text_right().text_color(t::fg3()).child((i + 1).to_string()))
                    .child(div().flex_1().min_w_0().child(code_line(l, i, sel.as_ref())))
            }))
            .into_any_element();
    }
    let _ = cx;
    // The longest line (in a monospace font, the widest) sets how far it scrolls sideways.
    let longest = lines.iter().enumerate().max_by_key(|(_, l)| l.text.len()).map(|(i, _)| i);
    let list = uniform_list(id, n, move |range: Range<usize>, _, _| {
        range
            .map(|i| {
                div()
                    .h(px(18.))
                    .flex()
                    .gap(px(12.))
                    .pr(px(12.))
                    .whitespace_nowrap()
                    .child(div().flex_none().w(px(digits * 7.5)).text_right().text_color(t::fg3()).child((i + 1).to_string()))
                    .child(div().flex_none().child(code_line(&lines[i], i, sel.as_ref())))
            })
            .collect::<Vec<_>>()
    })
    .when_some(scroll, |l, h| l.track_scroll(&h))
    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    .with_width_from_item(longest)
    .size_full()
    // Room for the scrollbar under the last line.
    .pb(px(10.))
    .font_family(t::MONO)
    .text_size(px(12.));
    div().relative().size_full().child(list).children(base.map(|h| hbar(&h))).into_any_element()
}

/// Text for a view of many blocks: the builder's next selectable segment, or plain.
fn block_text(b: &mut Option<&mut Builder>, styled: StyledText, plain: SharedString, join: &'static str) -> AnyElement {
    match b {
        Some(b) => b.text(styled, plain, join),
        None => styled.into_any_element(),
    }
}

fn spans_plain(spans: &[Span]) -> SharedString {
    spans.iter().map(|s| s.text.as_str()).collect::<String>().into()
}

fn span_text(spans: &[Span], size: f32) -> StyledText {
    let mut text = String::new();
    let mut hl = Vec::new();
    for s in spans {
        let start = text.len();
        text.push_str(&s.text);
        let mut h = HighlightStyle::default();
        if s.bold {
            h.font_weight = Some(FontWeight::BOLD);
        }
        if s.italic {
            h.font_style = Some(FontStyle::Italic);
        }
        if s.strike {
            h.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
        }
        if s.code {
            h.background_color = Some(t::hover());
            h.font_weight = Some(FontWeight::MEDIUM);
        }
        if s.link.is_some() {
            h.color = Some(t::accent_text());
            h.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
        }
        hl.push((start..text.len(), h));
    }
    let _ = size;
    StyledText::new(text).with_highlights(hl)
}

/// Markdown, rendered; its text selectable across blocks when `sel` builds it (each heading,
/// paragraph, item, code line and table cell is a segment). Code blocks wrap with `wrap`, else
/// each scrolls sideways (as the app's `pre`).
pub fn markdown_view(blocks: &[Md], dir: &str, mut sel: Option<&mut Builder>, wrap: bool) -> AnyElement {
    let scroll = sel.as_ref().map(|b| b.scroller());
    let b = &mut sel;
    let mut out: Vec<AnyElement> = Vec::with_capacity(blocks.len());
    let mut code_blocks = 0;
    for block in blocks {
        out.push(match block {
            Md::Heading(level, spans) => div()
                // Padding, not a margin: a scroll area doesn't count its first child's margin.
                .pt(px(6.))
                .text_size(px(match level {
                    1 => 22.,
                    2 => 18.,
                    3 => 15.5,
                    _ => 14.,
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .when(*level <= 2, |d| d.pb(px(4.)).border_b_1().border_color(t::line()))
                .child(block_text(b, span_text(spans, 0.), spans_plain(spans), "\n"))
                .into_any_element(),
            Md::Para(spans) => div().text_size(px(13.)).child(block_text(b, span_text(spans, 13.), spans_plain(spans), "\n")).into_any_element(),
            Md::Quote(spans) => div().pl(px(12.)).border_l_2().border_color(t::line2()).text_color(t::fg2()).child(block_text(b, span_text(spans, 13.), spans_plain(spans), "\n")).into_any_element(),
            Md::Item { depth, marker, spans } => div()
                .pl(px(*depth as f32 * 18.))
                .flex()
                .gap(px(8.))
                .child(div().flex_none().min_w(px(14.)).text_color(t::fg3()).child(marker.clone()))
                .child(div().flex_1().min_w_0().child(block_text(b, span_text(spans, 13.), spans_plain(spans), "\n")))
                .into_any_element(),
            Md::Code(lines) if wrap => div()
                .p(px(10.))
                .rounded(px(8.))
                .bg(t::hover())
                .font_family(t::MONO)
                .text_size(px(12.))
                .line_height(relative(1.4))
                .children(lines.iter().map(|l| div().child(block_text(b, line_text(l), l.text.clone(), "\n"))).collect::<Vec<_>>())
                .into_any_element(),
            // Not wrapped: the block scrolls sideways on its own (Shift+wheel, its scrollbar).
            Md::Code(lines) => {
                let i = code_blocks;
                code_blocks += 1;
                let h = b.as_ref().map(|b| b.block_scroller(i));
                let rows: Vec<AnyElement> = lines.iter().map(|l| div().flex_none().child(block_text(b, line_text(l), l.text.clone(), "\n")).into_any_element()).collect();
                div()
                    .relative()
                    .rounded(px(8.))
                    .bg(t::hover())
                    .overflow_hidden()
                    .child(
                        div()
                            .id(("md-code", i))
                            .when_some(h.clone(), |d, h| d.track_scroll(&h))
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .p(px(10.))
                            .font_family(t::MONO)
                            .text_size(px(12.))
                            .line_height(relative(1.4))
                            .whitespace_nowrap()
                            .flex()
                            .flex_col()
                            .items_start()
                            .children(rows),
                    )
                    .children(h.map(|h| hbar(&h)))
                    .into_any_element()
            }
            Md::Rule => div().h(px(1.)).bg(t::line2()).my(px(6.)).into_any_element(),
            Md::Image(src) => {
                let p = if src.contains("://") || std::path::Path::new(src).is_absolute() { src.clone() } else { format!("{}\\{}", dir.trim_end_matches('\\'), src.replace('/', "\\")) };
                if src.starts_with("http") {
                    div().text_color(t::fg3()).child(format!("[image: {src}]")).into_any_element()
                } else {
                    img(std::path::PathBuf::from(p)).max_w_full().object_fit(ObjectFit::Contain).into_any_element()
                }
            }
            Md::Table(rows) => div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(t::line2())
                .rounded(px(6.))
                .text_size(px(12.))
                .children(
                    rows.iter()
                        .enumerate()
                        .map(|(i, r)| {
                            // Cells copy as a tab-separated row.
                            let cells: Vec<AnyElement> = r
                                .iter()
                                .enumerate()
                                .map(|(j, c)| {
                                    let plain = SharedString::from(c.clone());
                                    let join = if j + 1 == r.len() { "\n" } else { "\t" };
                                    div().flex_1().min_w_0().px(px(6.)).py(px(3.)).child(block_text(b, StyledText::new(plain.clone()), plain, join)).into_any_element()
                                })
                                .collect();
                            div().flex().when(i == 0, |d| d.font_weight(FontWeight::SEMIBOLD).bg(t::hover())).border_b_1().border_color(t::line()).children(cells)
                        })
                        .collect::<Vec<_>>(),
                )
                .into_any_element(),
        });
    }
    div()
        .id("md")
        .when_some(scroll, |d, h| d.track_scroll(&h))
        .size_full()
        .overflow_y_scroll()
        // The wheel scrolls it up and down only (Shift+wheel is for a code block's sideways).
        .restrict_scroll_to_axis()
        .px(px(4.))
        // GPUI scrolls as far as its children's boxes reach (margins aren't counted), so the
        // end of the text gets room of its own instead of being cut off when scrolled down.
        .pb(px(16.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .line_height(relative(1.5))
        .children(out)
        .into_any_element()
}

/// Children shown per level (the rest: "… N more").
const JSON_LIMIT: usize = 500;
/// Longest string shown whole in the tree (the rest is copied, not shown).
const JSON_STR_MAX: usize = 10_000;

/// New lines with the next row's indent (2 spaces a level), and with a comma before them:
/// what's copied between rows, so a copied part of the tree is JSON as `to_string_pretty` writes.
static NL: &str = concat!("\n", "                                                                                                                                ");
static COMMA_NL: &str = concat!(",\n", "                                                                                                                                ");

fn json_join(comma: bool, depth: usize) -> &'static str {
    let n = (depth * 2).min(128);
    if comma { &COMMA_NL[..2 + n] } else { &NL[..1 + n] }
}

/// `v` as `to_string_pretty` writes it, its lines after the first indented to `depth`.
fn json_pretty(v: &serde_json::Value, depth: usize) -> String {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    if depth == 0 {
        return text;
    }
    let pad = " ".repeat(depth * 2);
    text.replace('\n', &format!("\n{pad}"))
}

/// A path in a JSON document, as written in code: `items[2].name`, `["odd key"].x`.
pub fn json_path_text(path: &[JsonKey]) -> String {
    let mut out = String::new();
    for k in path {
        match k {
            JsonKey::Index(i) => out.push_str(&format!("[{i}]")),
            JsonKey::Key(k) if !k.is_empty() && k.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') && k.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            JsonKey::Key(k) => out.push_str(&format!("[{}]", serde_json::to_string(k).unwrap_or_default())),
        }
    }
    out
}

/// A step into a JSON value.
#[derive(Clone, Debug)]
pub enum JsonKey {
    Key(String),
    Index(usize),
}

/// The value at `path` in `v`.
pub fn json_at<'a>(v: &'a serde_json::Value, path: &[JsonKey]) -> Option<&'a serde_json::Value> {
    path.iter().try_fold(v, |v, k| match k {
        JsonKey::Key(k) => v.get(k),
        JsonKey::Index(i) => v.get(*i),
    })
}

/// What drawing the tree needs to know.
struct JsonCx<'a, 'b> {
    open: &'a HashSet<String>,
    wrap: bool,
    big: bool,
    root: Arc<serde_json::Value>,
    b: Option<&'a mut Builder>,
    rows: Vec<AnyElement>,
    /// For each segment: its depth, and whether a comma follows it when it's not the last.
    ends: Vec<(usize, bool)>,
    cx: &'a mut Context<'b, Ferret>,
}

fn json_colors() -> (Hsla, Hsla, Hsla, Hsla) {
    if t::is_dark() { (rgb(0xa5d6ff), rgb(0x79c0ff), rgb(0xff7b72), t::fg3()) } else { (rgb(0x0a3069), rgb(0x0550ae), rgb(0xcf222e), t::fg3()) }
}

/// A row's text (key, then value or bracket), coloured: selectable as the next segment.
fn json_text(j: &mut JsonCx, key: Option<&str>, value: &str, color: Hsla, depth: usize, comma: bool) -> AnyElement {
    let mut text = String::new();
    let mut hl = Vec::new();
    if let Some(k) = key {
        text.push_str(&serde_json::to_string(k).unwrap_or_default());
        hl.push((0..text.len(), HighlightStyle { color: Some(t::fg2()), ..Default::default() }));
        text.push_str(": ");
    }
    let start = text.len();
    text.push_str(value);
    hl.push((start..text.len(), HighlightStyle { color: Some(color), ..Default::default() }));
    let plain = SharedString::from(text);
    let styled = StyledText::new(plain.clone()).with_highlights(hl);
    match j.b.as_mut() {
        Some(b) => {
            j.ends.push((depth, comma));
            b.text(styled, plain, "\n")
        }
        None => styled.into_any_element(),
    }
}

/// JSON as a tree: two levels open, up to 500 items per level. Its rows read as the JSON they
/// stand for (keys in quotes; commas and indents are added when copied), so a selection copies
/// as JSON, a folded node selected whole copies all of it, and everything (Ctrl+A) copies the
/// whole document. Long strings wrap under their key with `wrap`; else the tree scrolls sideways.
/// A right-click on a row adds Copy value and Copy path (`big`: Quick Look's).
pub fn json_view(v: &Arc<serde_json::Value>, open: &HashSet<String>, wrap: bool, sel: Option<&mut Builder>, big: bool, cx: &mut Context<Ferret>) -> AnyElement {
    let scroll = sel.as_ref().map(|b| b.scroller());
    let mut j = JsonCx { open, wrap, big, root: v.clone(), b: sel, rows: Vec::new(), ends: Vec::new(), cx };
    json_rows(&mut j, &v.clone(), None, Vec::new(), "$", 0, false);
    // What follows each row when copied: a comma if a value ends there and another follows, a
    // new line, and the next row's indent.
    if let Some(b) = j.b.as_mut() {
        let first = b.next() - j.ends.len();
        for i in 0..j.ends.len() {
            let next_depth = j.ends.get(i + 1).map(|e| e.0).unwrap_or(0);
            b.segs.set_join(first + i, json_join(j.ends[i].1, next_depth));
        }
        let root = j.root.clone();
        b.segs.all = Some(Box::new(move || serde_json::to_string_pretty(&*root).unwrap_or_default()));
    }
    let rows = std::mem::take(&mut j.rows);
    let tree = div()
        .id("json")
        .when_some(scroll.clone(), |d, h| d.track_scroll(&h))
        .size_full()
        .font_family(t::MONO)
        .text_size(px(12.))
        .line_height(relative(1.5))
        .pb(px(10.))
        .flex()
        .flex_col()
        .children(rows);
    if wrap {
        return tree.overflow_y_scroll().into_any_element();
    }
    // Not wrapped: rows as long as they are, the tree scrolling sideways too (its scrollbar under it).
    div()
        .relative()
        .size_full()
        .child(tree.overflow_scroll().items_start().whitespace_nowrap())
        .children(scroll.map(|h| hbar(&h)))
        .into_any_element()
}

/// A tree row: indented, then what leads it (a node's chevron) and an array item's place.
fn json_row<E: Styled + ParentElement>(d: E, pad: gpui::Pixels, wrap: bool, lead: Option<AnyElement>, index: Option<usize>) -> E {
    let d = d.pl(pad).flex().gap(px(6.));
    let d = if wrap { d.items_start() } else { d };
    let d = match lead {
        Some(l) => d.child(l),
        None => d,
    };
    match index {
        Some(i) => d.child(div().flex_none().min_w(px(22.)).text_right().text_color(t::fg3()).opacity(0.7).child(format!("{i}"))),
        None => d,
    }
}

/// The rows of `v` (under `key`, at `at` in the document; `path`: its key for what's open), and
/// whether a sibling follows it (`more`: a comma after it when copied).
fn json_rows(j: &mut JsonCx, v: &serde_json::Value, key: Option<JsonKey>, at: Vec<JsonKey>, path: &str, depth: usize, more: bool) {
    use serde_json::Value as V;
    let (str_c, num_c, bool_c, null_c) = json_colors();
    let pad = px(depth as f32 * 14.);
    let wrap = j.wrap;
    let big = j.big;
    // An object's member shows its key in quotes (as JSON); an array's item its place, dimmed and
    // not part of the text.
    let (name, index) = match &key {
        Some(JsonKey::Key(k)) => (Some(k.clone()), None),
        Some(JsonKey::Index(i)) => (None, Some(*i)),
        None => (None, None),
    };
    let menu_at = at.clone();
    let root = j.root.clone();
    // A right-click on the row: Copy / Select all, and this value's JSON and path.
    let on_menu = j.cx.listener(move |f, ev: &gpui::MouseDownEvent, window, cx| {
        cx.stop_propagation();
        f.json_menu(big, ev.position, root.clone(), menu_at.clone(), window, cx);
    });
    let text_box = |el: AnyElement| div().when(wrap, |d| d.flex_initial().min_w_0()).when(!wrap, |d| d.flex_none()).child(el);
    let leaf = pad + px(18.);
    match v {
        V::Object(_) | V::Array(_) if v.as_object().map(|m| m.is_empty()).unwrap_or(false) || v.as_array().map(|a| a.is_empty()).unwrap_or(false) => {
            let text = if v.is_object() { "{}" } else { "[]" };
            let t = json_text(j, name.as_deref(), text, null_c, depth, more);
            let el = json_row(div().on_mouse_down(gpui::MouseButton::Right, on_menu), leaf, wrap, None, index).child(text_box(t));
            j.rows.push(el.into_any_element());
        }
        V::Object(_) | V::Array(_) => {
            let is_open = if depth < 2 { !j.open.contains(&format!("!{path}")) } else { j.open.contains(path) };
            let (n, brackets, what) = match v {
                V::Object(m) => (m.len(), ("{", "}"), if m.len() == 1 { "key" } else { "keys" }),
                V::Array(a) => (a.len(), ("[", "]"), if a.len() == 1 { "item" } else { "items" }),
                _ => unreachable!(),
            };
            let p = path.to_string();
            let d = depth;
            let shown = if is_open { brackets.0.to_string() } else { format!("{}…{}", brackets.0, brackets.1) };
            let seg = j.b.as_ref().map(|b| b.next());
            let t = json_text(j, name.as_deref(), &shown, t::fg3(), depth, !is_open && more);
            // Folded and selected whole: all of it is copied.
            if let (false, Some(seg), Some(b)) = (is_open, seg, j.b.as_mut()) {
                let mut whole = String::new();
                if let Some(k) = &name {
                    whole.push_str(&serde_json::to_string(k).unwrap_or_default());
                    whole.push_str(": ");
                }
                whole.push_str(&json_pretty(v, depth));
                b.segs.whole.insert(seg, whole);
            }
            let toggle = j.cx.listener(move |f, ev: &gpui::ClickEvent, _, cx| {
                // A drag over the row selects its text, and Shift extends: neither folds. A double
                // click picks a word: its second click puts the node back as it was.
                if let gpui::ClickEvent::Mouse(m) = ev {
                    let moved = (m.up.position.x - m.down.position.x).abs() + (m.up.position.y - m.down.position.y).abs();
                    if moved > px(3.) || m.down.modifiers.shift || m.up.click_count > 2 {
                        return;
                    }
                }
                let key = if d < 2 { format!("!{p}") } else { p.clone() };
                if !f.preview_view.open.remove(&key) {
                    f.preview_view.open.insert(key);
                }
                cx.notify();
            });
            let chevron = div().flex_none().pt(px(3.)).child(icon(if is_open { "chevron-down" } else { "chevron-right" }, 12., t::fg3())).into_any_element();
            let el = json_row(
                div().id(SharedString::from(format!("j{path}"))).cursor(gpui::CursorStyle::PointingHand).hover(|s| s.bg(t::hover())).on_click(toggle).on_mouse_down(gpui::MouseButton::Right, on_menu),
                pad,
                wrap,
                Some(chevron),
                index,
            )
            .child(text_box(t))
            .when(!is_open, |d| d.child(div().flex_none().text_color(t::fg3()).opacity(0.75).child(format!("{n} {what}"))));
            j.rows.push(el.into_any_element());
            if is_open {
                let push_more = |j: &mut JsonCx, rest: Vec<String>| {
                    if rest.is_empty() {
                        return;
                    }
                    let shown = format!("… {} more", crate::kinds::group_digits(rest.len()));
                    let seg = j.b.as_ref().map(|b| b.next());
                    let t = json_text(j, None, &shown, t::fg3(), depth + 1, false);
                    if let (Some(seg), Some(b)) = (seg, j.b.as_mut()) {
                        b.segs.whole.insert(seg, rest.join(json_join(true, depth + 1)));
                    }
                    j.rows.push(div().pl(px((depth + 1) as f32 * 14. + 18.)).child(t).into_any_element());
                };
                match v {
                    V::Object(m) => {
                        let total = m.len();
                        for (i, (k, child)) in m.iter().enumerate().take(JSON_LIMIT) {
                            let mut at = at.clone();
                            at.push(JsonKey::Key(k.clone()));
                            json_rows(j, child, Some(JsonKey::Key(k.clone())), at, &format!("{path}.{k}"), depth + 1, i + 1 < total);
                        }
                        let rest = m.iter().skip(JSON_LIMIT).map(|(k, c)| format!("{}: {}", serde_json::to_string(k).unwrap_or_default(), json_pretty(c, depth + 1))).collect();
                        push_more(j, rest);
                    }
                    V::Array(a) => {
                        let total = a.len();
                        for (i, child) in a.iter().enumerate().take(JSON_LIMIT) {
                            let mut at = at.clone();
                            at.push(JsonKey::Index(i));
                            json_rows(j, child, Some(JsonKey::Index(i)), at, &format!("{path}[{i}]"), depth + 1, i + 1 < total);
                        }
                        let rest = a.iter().skip(JSON_LIMIT).map(|c| json_pretty(c, depth + 1)).collect();
                        push_more(j, rest);
                    }
                    _ => {}
                }
                let t = json_text(j, None, brackets.1, t::fg3(), depth, more);
                j.rows.push(div().pl(pad + px(18.)).child(t).into_any_element());
            }
        }
        _ => {
            let (text, color) = match v {
                V::String(s) => {
                    let quoted = serde_json::to_string(s).unwrap_or_default();
                    (quoted, str_c)
                }
                V::Number(n) => (n.to_string(), num_c),
                V::Bool(b) => (b.to_string(), bool_c),
                _ => ("null".to_string(), null_c),
            };
            // A very long string shows its start; selected whole, all of it is copied.
            let cut = text.len() > JSON_STR_MAX;
            let shown = if cut {
                let mut end = JSON_STR_MAX;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &text[..end])
            } else {
                text.clone()
            };
            let seg = j.b.as_ref().map(|b| b.next());
            let t = json_text(j, name.as_deref(), &shown, color, depth, more);
            if let (true, Some(seg), Some(b)) = (cut, seg, j.b.as_mut()) {
                let key = name.as_ref().map(|k| format!("{}: ", serde_json::to_string(k).unwrap_or_default())).unwrap_or_default();
                b.segs.whole.insert(seg, format!("{key}{text}"));
            }
            let el = json_row(div().on_mouse_down(gpui::MouseButton::Right, on_menu), leaf, wrap, None, index).child(text_box(t));
            j.rows.push(el.into_any_element());
        }
    }
}

/// A table (CSV, a workbook's sheet): headings, sortable columns, rows drawn where visible,
/// scrolling sideways when wider than the view (Shift+wheel, its scrollbar). With `sel` (and the
/// key it's shown as, and the view drawing it) its cells are selectable and copy as
/// tab-separated rows; a CSV's headings come first in Ctrl+A.
pub fn table_view(id: &'static str, rows: Arc<Vec<Vec<String>>>, numbered: Option<(u32, u32)>, sort: Option<(usize, bool)>, sel: Option<(&Shared, &str)>, cx: &mut Context<Ferret>) -> AnyElement {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0).min(60);
    let (head, body): (Vec<String>, Vec<usize>) = if numbered.is_some() {
        ((0..cols).map(|c| col_letter(c as u32 + numbered.map(|n| n.1).unwrap_or(0))).collect(), (0..rows.len()).collect())
    } else {
        (rows.first().cloned().unwrap_or_default(), (1..rows.len()).collect())
    };
    let mut order = body;
    if let Some((c, asc)) = sort {
        order.sort_by(|&a, &b| {
            let (x, y) = (rows[a].get(c).map(String::as_str).unwrap_or(""), rows[b].get(c).map(String::as_str).unwrap_or(""));
            let o = match (x.trim().parse::<f64>(), y.trim().parse::<f64>()) {
                (Ok(p), Ok(q)) => p.total_cmp(&q),
                _ => crate::sortgroup::collate(x, y),
            };
            if asc { o } else { o.reverse() }
        });
    }
    let order = Arc::new(order);
    let col_w = 120.;
    let first = numbered.map(|n| n.0).unwrap_or(0);
    let r2 = rows.clone();
    // Selectable: the cells, as shown (sorted), are the segments.
    let me = cx.entity_id();
    let head_row = numbered.is_none();
    let cells = sel.map(|(s, key)| {
        let mut st = s.borrow_mut();
        let list = st.list_scroller(key);
        let side = st.block_scroller(key, 0);
        st.begin(key, std::rc::Rc::new(crate::selectable::Cells { rows: rows.clone(), order: order.clone(), cols, head: head_row }), me);
        (s.clone(), list, side)
    });
    let width = 44. + cols as f32 * col_w;
    let o2 = order.clone();
    let cell_sel = cells.as_ref().map(|c| c.0.clone());
    let list = uniform_list(id, order.len(), move |range: Range<usize>, _, _| {
        range
            .map(|i| {
                let r = &r2[o2[i]];
                div()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(t::line())
                    .child(div().w(px(44.)).flex_none().px(px(6.)).text_right().text_color(t::fg3()).child(if numbered.is_some() { (first as usize + o2[i] + 1).to_string() } else { (i + 1).to_string() }))
                    .children((0..cols).map(|c| {
                        let text = SharedString::from(r.get(c).cloned().unwrap_or_default());
                        let el: AnyElement = match &cell_sel {
                            Some(s) => crate::selectable::SelText::new(StyledText::new(text), crate::selectable::Cells::seg(head_row, cols, i, c), s).into_any_element(),
                            None => text.into_any_element(),
                        };
                        div().w(px(col_w)).flex_none().px(px(6.)).truncate().child(el)
                    }))
            })
            .collect::<Vec<_>>()
    })
    .when_some(cells.as_ref().map(|c| c.1.clone()), |l, h| l.track_scroll(&h))
    .flex_1();
    // The wheel scrolls the rows up and down only (Shift+wheel: the table sideways).
    let mut list = list;
    list.style().restrict_scroll_to_axis = Some(true);
    let side = cells.as_ref().map(|c| c.2.clone());
    let table = div()
        .w(px(width))
        .h_full()
        .flex()
        .flex_col()
        .text_size(px(12.))
        .child(
            div()
                .flex()
                .flex_none()
                .bg(t::hover())
                .border_b_1()
                .border_color(t::line2())
                .font_weight(FontWeight::SEMIBOLD)
                // Row numbers: a sheet's own, or (CSV, as the app) the row's place as shown.
                .child(div().w(px(44.)).flex_none().px(px(6.)).py(px(4.)).text_right().text_color(t::fg3()).when(numbered.is_none(), |d| d.child("#")))
                .children(head.into_iter().take(cols).enumerate().map(|(c, h)| {
                    let on = sort.map(|s| s.0) == Some(c);
                    div()
                        .id(("th", c))
                        .w(px(col_w))
                        .flex_none()
                        .px(px(6.))
                        .py(px(4.))
                        .flex()
                        .gap(px(3.))
                        .truncate()
                        .text_color(if on { t::accent_text() } else { t::fg2() })
                        .hover(|s| s.bg(t::press()))
                        .on_click(cx.listener(move |f, _, _, cx| {
                            f.preview_view.sort = match f.preview_view.sort {
                                Some((x, asc)) if x == c => Some((c, !asc)),
                                _ => Some((c, true)),
                            };
                            cx.notify();
                        }))
                        .child(div().truncate().child(h))
                        .when(on, |d| d.child(icon(if sort.map(|s| s.1).unwrap_or(true) { "arrow-up" } else { "arrow-down" }, 11., t::accent_text())))
                })),
        )
        .child(list);
    div()
        .relative()
        .size_full()
        .child(div().id((id, 1usize)).when_some(side.clone(), |d, h| d.track_scroll(&h)).size_full().overflow_x_scroll().restrict_scroll_to_axis().child(table))
        .children(side.map(|h| hbar(&h)))
        .into_any_element()
}

pub fn col_letter(mut n: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push((b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s.iter().rev().collect()
}

/// A slide, rebuilt from its shapes, scaled to `w`. With `sel`, its text is selectable (each
/// paragraph, table cell and line of a turned shape is a segment; a blank line after the slide
/// when copied).
pub fn slide_view(deck: &ferret_core::office::Deck, s: &ferret_core::office::Slide, bg: Option<Arc<Image>>, drawn: &[Option<crate::slidedraw::Drawn>], w: f32, mut sel: Option<&mut Builder>) -> AnyElement {
    let b = &mut sel;
    let k = w / deck.width.max(1) as f32;
    let h = deck.height as f32 * k;
    let color = |c: &Option<String>| c.as_deref().and_then(|x| t::parse_hex(x.trim_start_matches('#'))).map(|v| t::rgba(v, 1.));
    let slide = div()
        .w(px(w))
        .h(px(h))
        .relative()
        .overflow_hidden()
        .rounded(px(4.))
        .bg(color(&s.background).unwrap_or(t::rgba(0xffffff, 1.)))
        .when(s.hidden, |d| d.opacity(0.55))
        .children(bg.map(|b| img(b).absolute().size_full().object_fit(ObjectFit::Cover)))
        .children(s.shapes.iter().enumerate().map(|(si, sh)| -> AnyElement {
            // Drawn ahead (turned or vertical): its picture, where the turned shape shows.
            if let Some(Some(d)) = drawn.get(si) {
                let [x, y, dw, dh] = d.rect.map(|v| v as f32 * k);
                // Its text, turned with it, over the picture.
                let layer = match (b.as_mut(), &d.text) {
                    (Some(b), Some(text)) => Some(b.layer(text.clone(), 0, &d.joins)),
                    _ => None,
                };
                return div().absolute().left(px(x)).top(px(y)).w(px(dw)).h(px(dh)).child(img(d.image.clone()).size_full().object_fit(ObjectFit::Fill)).children(layer).into_any_element();
            }
            let mut d = div()
                .absolute()
                .left(px(sh.x as f32 * k))
                .top(px(sh.y as f32 * k))
                .w(px(sh.w as f32 * k))
                .h(px(sh.h as f32 * k))
                .overflow_hidden()
                .flex()
                .flex_col()
                .when(sh.anchor == "ctr", |d| d.justify_center())
                .when(sh.anchor == "b", |d| d.justify_end());
            if let Some(c) = color(&sh.fill) {
                d = d.bg(c);
            }
            if let Some(c) = color(&sh.line) {
                d = d.border_1().border_color(c);
            }
            if sh.geom == "ellipse" {
                d = d.rounded_full();
            } else if sh.geom == "roundRect" {
                d = d.rounded(px(8. * k * 9000.));
            }
            if let Some(p) = &sh.image {
                d = d.child(img(std::path::PathBuf::from(p)).size_full().object_fit(ObjectFit::Cover));
            }
            if let Some(table) = &sh.table {
                let rows: Vec<AnyElement> = table
                    .iter()
                    .map(|r| {
                        let n = r.len();
                        let cells: Vec<AnyElement> = r
                            .iter()
                            .enumerate()
                            .map(|(ci, c)| {
                                let plain = SharedString::from(c.clone());
                                let text = block_text(b, StyledText::new(plain.clone()), plain, if ci + 1 == n { "\n" } else { "\t" });
                                div().flex_1().border_1().border_color(t::rgba(0x999999, 1.)).text_size(px((10. * k * 12700.).max(4.))).text_color(t::rgba(0x222222, 1.)).child(text).into_any_element()
                            })
                            .collect();
                        div().flex().flex_1().children(cells).into_any_element()
                    })
                    .collect();
                d = d.child(div().flex().flex_col().size_full().children(rows));
            }
            if let Some(l) = &sh.label {
                d = d.items_center().justify_center().bg(t::rgba(0xeeeeee, 1.)).text_color(t::rgba(0x666666, 1.)).text_size(px(10.)).child(l.clone());
            }
            let pad = |v: i64| px(v as f32 * k);
            let paras: Vec<AnyElement> = sh.paras.iter().map(|p| {
                let size = p.runs.first().map(|r| r.size).unwrap_or(p.size).max(1.) as f32 * 12700. * k;
                let mut text = String::new();
                let mut hl = Vec::new();
                if let Some(b) = &p.bullet {
                    text.push_str(b);
                    text.push(' ');
                }
                for r in &p.runs {
                    let start = text.len();
                    let s = match r.caps.as_deref() {
                        Some(_) => r.text.to_uppercase(),
                        None => r.text.clone(),
                    };
                    text.push_str(&s);
                    hl.push((
                        start..text.len(),
                        HighlightStyle {
                            color: color(&r.color),
                            font_weight: r.bold.then_some(FontWeight::BOLD),
                            font_style: r.italic.then_some(FontStyle::Italic),
                            underline: r.underline.then_some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                            ..Default::default()
                        },
                    ));
                }
                div()
                    .pl(px(p.level as f32 * 16. * k * 12700.))
                    .text_size(px(size.max(3.)))
                    .line_height(relative(p.line.max(0.8) as f32 * 1.15))
                    .text_color(t::rgba(0x1f1f1f, 1.))
                    .when(p.align == "ctr", |d| d.text_center())
                    .when(p.align == "r", |d| d.text_right())
                    .child(block_text(b, StyledText::new(text.clone()).with_highlights(hl), text.into(), "\n"))
                    .into_any_element()
            }).collect();
            d.pl(pad(sh.inset[0])).pt(pad(sh.inset[1])).pr(pad(sh.inset[2])).pb(pad(sh.inset[3])).children(paras).into_any_element()
        }).collect::<Vec<_>>())
        .into_any_element();
    // A blank line between slides when copied.
    if let Some(b) = b.as_mut() {
        b.segs.end_with("\n\n");
    }
    slide
}

/// The first bytes, in rows of 16: offset, hex, text.
pub fn hex_view(bytes: &[u8], width: f32) -> AnyElement {
    // 16 bytes a row when there's room, else 8 (the text column must fit too).
    let per = if width >= 560. { 16 } else { 8 };
    div()
        .id("hex")
        .size_full()
        .overflow_y_scroll()
        .font_family(t::MONO)
        .text_size(px(11.5))
        .children(bytes.chunks(per).enumerate().map(|(i, row)| {
            let hex: String = row.iter().map(|b| format!("{b:02x} ")).collect();
            let text: String = row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '·' }).collect();
            div()
                .flex()
                .gap(px(10.))
                .whitespace_nowrap()
                .child(div().text_color(t::fg3()).child(format!("{:08x}", i * per)))
                .child(div().w(px(per as f32 * 21.)).child(hex))
                .child(div().text_color(t::fg2()).child(text))
        }))
        .into_any_element()
}

/// The facts a renderer adds to the details list.
pub fn facts(c: &Content) -> Vec<(&'static str, String)> {
    use crate::kinds::{format_size, group_digits};
    let dur = |ms: u64| {
        let s = ms / 1000;
        if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
    };
    match c {
        Content::Picture { w, h, .. } if *w > 0 => vec![("Dimensions", format!("{w} × {h}"))],
        Content::Video(v) => {
            let (w, h) = if v.rotation % 180 == 90 { (v.height, v.width) } else { (v.width, v.height) };
            let mut out = vec![("Dimensions", format!("{w} × {h}")), ("Duration", dur(v.duration_ms)), ("Frame rate", format!("{:.2} fps", v.fps).replace(".00", "")), ("Codec", v.codec.clone())];
            if v.video_bitrate > 0 {
                out.push(("Bitrate", format!("{:.1} Mbps", v.video_bitrate as f64 / 1e6)));
            }
            out.push(("Audio", if v.has_audio { if v.audio_bitrate > 0 { format!("{} kbps", v.audio_bitrate / 1000) } else { "Yes".into() } } else { "None".into() }));
            out
        }
        Content::Audio(a, _) => {
            let mut out = Vec::new();
            for (k, v) in [("Title", &a.title), ("Artist", &a.artist), ("Album", &a.album), ("Album artist", &a.album_artist)] {
                if !v.is_empty() {
                    out.push((k, v.clone()));
                }
            }
            if a.year > 0 {
                out.push(("Year", a.year.to_string()));
            }
            if a.track > 0 {
                out.push(("Track", a.track.to_string()));
            }
            out.push(("Duration", dur(a.duration_ms)));
            if a.bitrate > 0 {
                out.push(("Bitrate", format!("{} kbps", a.bitrate / 1000)));
            }
            out
        }
        Content::Text { lines, encoding, language, total_lines, .. } => vec![("Lines", group_digits(*total_lines as usize).max(group_digits(lines.len()))), ("Encoding", encoding.clone()), ("Language", language.to_string())],
        Content::Markdown { words, .. } => vec![("Words", group_digits(*words))],
        Content::Json { value, .. } => vec![(
            "Top level",
            match &**value {
                serde_json::Value::Object(m) => format!("Object, {} keys", m.len()),
                serde_json::Value::Array(a) => format!("Array, {} items", a.len()),
                _ => "Value".into(),
            },
        )],
        Content::Csv { rows, delimiter } => vec![
            ("Rows", group_digits(rows.len().saturating_sub(1))),
            ("Columns", rows.first().map(|r| r.len()).unwrap_or(0).to_string()),
            ("Delimiter", match delimiter {
                '\t' => "Tab".into(),
                ';' => "Semicolon".into(),
                '|' => "Pipe".into(),
                _ => "Comma".into(),
            }),
        ],
        Content::Sheet(s) => vec![
            ("Sheets", s.sheets.len().to_string()),
            ("Rows", if s.total_rows > s.rows.len() { format!("{} (first {} shown)", group_digits(s.total_rows), group_digits(s.rows.len())) } else { group_digits(s.total_rows) }),
            ("Columns", s.total_cols.to_string()),
        ],
        Content::Deck(d, ..) => vec![("Slides", d.total.to_string()), ("Slide size", format!("{:.1} × {:.1} in", d.width as f64 / 914400., d.height as f64 / 914400.))],
        Content::Archive(a) => {
            // As the app: how much smaller than its contents, or that it isn't.
            let saved = if a.total_size == 0 {
                "–".to_string()
            } else if a.compressed_size < a.total_size {
                format!("{:.0}% smaller than the contents", (1. - a.compressed_size as f64 / a.total_size as f64) * 100.)
            } else {
                "None: the archive is larger than its contents".to_string()
            };
            let mut out = vec![
                ("Format", a.format.clone()),
                ("Contents", format!("{}, {}", crate::files::plural(a.files as usize, "file"), crate::files::plural(a.dirs as usize, "folder"))),
                ("Unpacked", format_size(a.total_size)),
                ("Saved", saved),
            ];
            if a.encrypted {
                out.push(("Encrypted", "Yes".into()));
            }
            out
        }
        Content::Pdf { count, .. } => vec![("Pages", count.to_string())],
        Content::PdfLocked { .. } => vec![("Protected", "Password".to_string())],
        Content::DocText(p) => vec![("Words", group_digits(p.iter().map(|(_, t)| t.split_whitespace().count()).sum())), ("Paragraphs", group_digits(p.len()))],
        Content::Program(d, _) => {
            let mut out: Vec<(&'static str, String)> = d.version.iter().map(|(k, v)| (Box::leak(k.clone().into_boxed_str()) as &'static str, v.clone())).collect();
            if let Some(a) = &d.arch {
                out.push(("Architecture", a.clone()));
            }
            out
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_paths_read_as_code() {
        let path = [JsonKey::Key("items".into()), JsonKey::Index(2), JsonKey::Key("name".into())];
        assert_eq!(json_path_text(&path), "items[2].name");
        let odd = [JsonKey::Key("odd key".into()), JsonKey::Key("a.b".into())];
        assert_eq!(json_path_text(&odd), r#"["odd key"]["a.b"]"#);
        assert_eq!(json_path_text(&[JsonKey::Index(0), JsonKey::Key("x".into())]), "[0].x");
        let v: serde_json::Value = serde_json::json!({"items": [1, 2, {"name": "n"}]});
        assert_eq!(json_at(&v, &path), Some(&serde_json::json!("n")));
    }

    #[test]
    fn copied_json_is_indented_as_pretty_printed() {
        let v = serde_json::json!({"a": [1, 2]});
        // A node at depth 2: its own lines indented to it.
        assert_eq!(json_pretty(&v, 2), "{\n      \"a\": [\n        1,\n        2\n      ]\n    }");
        assert_eq!(json_join(true, 1), ",\n  ");
        assert_eq!(json_join(false, 0), "\n");
        assert_eq!(json_join(false, 200).len(), 1 + 128);
    }
}
