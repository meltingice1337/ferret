//! File kinds, type names, sizes and dates, as src/lib/format.ts shows them. The kinds come from
//! the same table the app's search uses (src/lib/filetypes.json).

use gpui::Hsla;
use std::collections::HashMap;
use std::sync::OnceLock;

use crate::theme::{self, rgba};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Folder, Image, Video, Audio, Code, Text, Doc, Sheet, Pdf, Archive, Exe, Font, Data, Other,
}

fn table() -> &'static HashMap<String, Kind> {
    static T: OnceLock<HashMap<String, Kind>> = OnceLock::new();
    T.get_or_init(|| {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../src/lib/filetypes.json")).expect("filetypes.json");
        let mut map = HashMap::new();
        for (kind, exts) in json["kinds"].as_object().into_iter().flatten() {
            let kind = match kind.as_str() {
                "image" => Kind::Image, "video" => Kind::Video, "audio" => Kind::Audio, "code" => Kind::Code,
                "text" => Kind::Text, "doc" => Kind::Doc, "sheet" => Kind::Sheet, "pdf" => Kind::Pdf,
                "archive" => Kind::Archive, "exe" => Kind::Exe, "font" => Kind::Font, "data" => Kind::Data,
                _ => continue,
            };
            for ext in exts.as_str().unwrap_or("").split_whitespace() {
                map.insert(ext.to_string(), kind);
            }
        }
        map
    })
}

pub fn kind_of(is_dir: bool, ext: &str) -> Kind {
    if is_dir {
        return Kind::Folder;
    }
    table().get(ext).copied().unwrap_or(Kind::Other)
}

pub fn icon(kind: Kind) -> &'static str {
    match kind {
        // Filled at 22 %, as the app's FileIcon draws folders (an outline looked thinner).
        Kind::Folder => "folder-fill",
        Kind::Image => "file-image",
        Kind::Video => "file-video",
        Kind::Audio => "file-audio",
        Kind::Code => "file-code-2",
        Kind::Data => "file-json",
        Kind::Text | Kind::Doc | Kind::Pdf => "file-text",
        Kind::Sheet => "file-spreadsheet",
        Kind::Archive => "file-archive",
        Kind::Exe => "app-window",
        Kind::Font => "file-type",
        Kind::Other => "file",
    }
}

pub fn color(kind: Kind) -> Hsla {
    match kind {
        Kind::Folder => theme::folder(),
        Kind::Image => rgba(0x4fc3f7, 1.),
        Kind::Video => rgba(0xb388ff, 1.),
        Kind::Audio => rgba(0xff8a65, 1.),
        Kind::Code => rgba(0x69f0ae, 1.),
        Kind::Data => rgba(0xffd54f, 1.),
        Kind::Text | Kind::Doc => rgba(0x90caf9, 1.),
        Kind::Pdf => rgba(0xff6b6b, 1.),
        Kind::Sheet => rgba(0x66bb6a, 1.),
        Kind::Archive => rgba(0xffb74d, 1.),
        Kind::Exe => rgba(0x80deea, 1.),
        Kind::Font => rgba(0xf48fb1, 1.),
        Kind::Other => theme::fg2(),
    }
}

pub fn group_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Folder => "Folder", Kind::Image => "Image", Kind::Video => "Video", Kind::Audio => "Audio",
        Kind::Code => "Source code", Kind::Text => "Text", Kind::Doc => "Document", Kind::Sheet => "Spreadsheet",
        Kind::Pdf => "PDF", Kind::Archive => "Archive", Kind::Exe => "Application", Kind::Font => "Font",
        Kind::Data => "Data", Kind::Other => "File",
    }
}

fn ext_label(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "md" | "markdown" => "Markdown", "txt" => "Text document", "log" => "Log file", "pdf" => "PDF document",
        "csv" => "CSV file", "tsv" => "TSV file", "zip" => "ZIP archive", "7z" => "7-Zip archive", "rar" => "RAR archive",
        "exe" => "Application", "msi" => "Installer", "lnk" => "Shortcut", "url" => "Internet shortcut", "dll" => "Library",
        "svg" => "SVG image", "ico" => "Icon", "ttf" => "TrueType font", "otf" => "OpenType font", "woff2" => "Web font",
        "docx" | "doc" => "Word document", "xlsx" | "xls" => "Excel workbook", "pptx" | "ppt" => "PowerPoint deck",
        "ini" => "Settings file", "iso" => "Disc image",
        _ => return None,
    })
}

fn language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust", "py" => "Python", "js" | "mjs" | "cjs" => "JavaScript", "jsx" => "JavaScript (JSX)",
        "ts" => "TypeScript", "tsx" => "TypeScript (TSX)", "go" => "Go", "java" => "Java", "kt" => "Kotlin",
        "cs" => "C#", "c" => "C", "h" => "C header", "cpp" | "cc" => "C++", "hpp" => "C++ header",
        "swift" => "Swift", "rb" => "Ruby", "php" => "PHP", "lua" => "Lua", "sh" => "Shell script",
        "ps1" => "PowerShell", "bat" | "cmd" => "Batch", "sql" => "SQL", "html" | "htm" => "HTML", "css" => "CSS",
        "scss" => "SCSS", "vue" => "Vue", "svelte" => "Svelte", "dart" => "Dart", "zig" => "Zig", "scala" => "Scala",
        "r" => "R", "pl" => "Perl", "ex" => "Elixir", "hs" => "Haskell",
        _ => return None,
    })
}

pub fn type_label(is_dir: bool, ext: &str) -> String {
    if !is_dir {
        if let Some(label) = ext_label(ext) {
            return label.into();
        }
    }
    let k = kind_of(is_dir, ext);
    let format = match ext {
        "jpg" | "jpeg" | "jfif" => "JPEG".to_string(),
        "tif" | "tiff" => "TIFF".to_string(),
        "m4a" => "AAC".to_string(),
        _ => ext.to_uppercase(),
    };
    match k {
        Kind::Image | Kind::Video | Kind::Audio => format!("{format} {}", group_label(k).to_lowercase()),
        Kind::Other if ext.is_empty() => "File".into(),
        Kind::Other => format!("{} file", ext.to_uppercase()),
        Kind::Code if language(ext).is_some() => format!("{} source", language(ext).unwrap()),
        Kind::Code => format!("{} source", ext.to_uppercase()),
        Kind::Data => format!("{} file", ext.to_uppercase()),
        _ => group_label(k).into(),
    }
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut v = bytes as f64 / 1024.;
    let mut i = 0;
    while v >= 1024. && i < units.len() - 1 {
        v /= 1024.;
        i += 1;
    }
    if v < 10. { format!("{v:.1} {}", units[i]) } else { format!("{} {}", v.round(), units[i]) }
}

pub fn format_date(ms: Option<i64>) -> String {
    use chrono::TimeZone;
    let Some(ms) = ms else { return String::new() };
    match chrono::Local.timestamp_millis_opt(ms).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

pub fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A search/filter type (filetypes.json `types`, plus folders): its id, label, words and extensions.
pub struct FileType {
    pub id: String,
    pub label: String,
    pub words: Vec<String>,
    /// Plain search words that offer to become this type ("vacation photos").
    pub suggest: Vec<String>,
    pub exts: Vec<String>,
}

pub const FOLDER_WORDS: [&str; 5] = ["folder", "folders", "dir", "dirs", "directory"];
pub const FILE_WORDS: [&str; 2] = ["file", "files"];

pub fn file_types() -> &'static [FileType] {
    static T: OnceLock<Vec<FileType>> = OnceLock::new();
    T.get_or_init(|| {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../src/lib/filetypes.json")).expect("filetypes.json");
        let kind_exts = |k: &str| -> Vec<String> {
            json["kinds"][k].as_str().unwrap_or("").split_whitespace().map(str::to_string).collect()
        };
        let strings = |v: &serde_json::Value| -> Vec<String> {
            v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_string)).collect()
        };
        json["types"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                let mut exts: Vec<String> = strings(&t["kinds"]).iter().flat_map(|k| kind_exts(k)).collect();
                exts.extend(t["also"].as_str().unwrap_or("").split_whitespace().map(str::to_string));
                // Each extension once (a type's kinds can share some, as the app's Set does).
                let mut seen = std::collections::HashSet::new();
                exts.retain(|e| seen.insert(e.clone()));
                FileType {
                    id: t["id"].as_str().unwrap_or("").into(),
                    label: t["label"].as_str().unwrap_or("").into(),
                    words: strings(&t["words"]),
                    suggest: strings(&t["suggest"]),
                    exts,
                }
            })
            .collect()
    })
}

/// Extensions of an icon kind ("pdf", "sheet"…), which also work in `type:`.
pub fn kind_exts(kind: &str) -> Vec<String> {
    let json: serde_json::Value = serde_json::from_str(include_str!("../../src/lib/filetypes.json")).expect("filetypes.json");
    json["kinds"][kind].as_str().unwrap_or("").split_whitespace().map(str::to_string).collect()
}

/// The type a word names: "folder", a type id or one of its words ("photos"), or an icon kind.
pub enum Found {
    Folders,
    Files,
    Exts(Vec<String>),
}

pub fn find_type(word: &str) -> Option<Found> {
    let w = word.trim().to_lowercase();
    let w = w.trim_start_matches('.');
    if FOLDER_WORDS.contains(&w) {
        return Some(Found::Folders);
    }
    if FILE_WORDS.contains(&w) {
        return Some(Found::Files);
    }
    if let Some(t) = file_types().iter().find(|t| t.id == w || t.words.iter().any(|x| x == w)) {
        return Some(Found::Exts(t.exts.clone()));
    }
    let exts = kind_exts(w);
    (!exts.is_empty()).then_some(Found::Exts(exts))
}

/// The filter's "Show only" types, in the app's order: (id, label).
pub fn show_only_types() -> Vec<(String, String)> {
    let mut out = vec![("folder".to_string(), "Folders".to_string())];
    out.extend(file_types().iter().map(|t| (t.id.clone(), t.label.clone())));
    out
}
