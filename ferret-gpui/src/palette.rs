//! Ctrl+K (SearchPalette.tsx): search every drive, WSL distro and connected server by name
//! through ferret-core's search (the indexer service, or the walk index), or inside files with
//! `content:"…"`. Results come as one ranked list (no "In <folder>" group: the user's choice;
//! "This folder" keeps to the open one), loose matches last;
//! filters (`type:`, `ext:`, `size:`, `modified:`, `in:`, `content:`, `!word`) autocomplete as
//! they're typed, a word naming a type offers to become the filter, and `?` lists them all. With
//! an empty box: saved searches, then recent ones. `>` (or Ctrl+Shift+P) lists commands instead.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ferret_core::search::{self, Hit, SearchResults};
use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, Point, ScrollHandle, SharedString, StyledText, Subscription, Window, div, prelude::*, px,
};
use regex::Regex;

use crate::bench;
use crate::input::{self, TextInput};
use crate::kinds;
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util;

pub enum PaletteEvent {
    /// Enter or a click: a folder (or archive) opens here, a file in its app.
    Open { path: String, is_dir: bool },
    /// Ctrl+Enter, Ctrl+click or a middle click: the result's folder (a folder or archive
    /// itself) in a new tab.
    OpenTab(String),
    /// Alt+Enter, Shift+Enter or the row's button: a file selected in its folder.
    Reveal(String),
    /// Run the command at this index of the list the palette was given.
    Command(usize),
    /// Closed, with the search it had (reopening soon brings it back).
    Dismiss(String),
    /// Ctrl+S, the star: save the search to the sidebar; `back` is what Ctrl+K reopens with if
    /// the naming is cancelled.
    Save { query: String, back: String },
    /// Delete (or ×) on a recent search.
    Forget(String),
    /// × on a saved search.
    RemoveSaved(String),
    /// A search that led somewhere (for the history).
    Searched(String),
    /// The result menu, by this result.
    Menu { path: String, is_dir: bool, at: Point<Pixels> },
    /// The search as a tab of all its matches (Ctrl+Shift+Enter, "Open in a tab").
    SearchTab(String),
    /// "Enable instant drive index" / "Update drive index".
    InstallIndex,
    CopyPath(String),
    /// A result pressed and moved: a shell drag of it (into a folder, Explorer, another app).
    Drag(String),
}

/// A command the window offers: what it says, its keys, a note (where it goes…).
pub struct Command {
    pub label: SharedString,
    pub keys: Option<&'static str>,
    pub hint: Option<SharedString>,
}

/// What opens the palette: where it is, what it remembers.
pub struct Init {
    pub query: Option<String>,
    /// The query is the last one, brought back: selected, so typing replaces it.
    pub select_all: bool,
    /// The open folder (the This folder scope, searches inside files).
    pub folder: Option<String>,
    /// Tests keep every search inside one folder.
    pub forced_scope: Option<String>,
    pub commands: Vec<Command>,
    pub history: Vec<String>,
    pub saved: Vec<(String, String)>,
    pub recent_folders: Vec<String>,
    /// Folders used often and lately rank their matches higher (recents' visits and age).
    pub frecency: HashMap<String, f32>,
    pub bench: Option<PathBuf>,
}

// ───────────────────────────── what the box understands ─────────────────────────────

/// Type chips under the search box, in Alt+0…9 order (None = all).
const CHIPS: [Option<&str>; 10] =
    [None, Some("folder"), Some("image"), Some("video"), Some("audio"), Some("doc"), Some("text"), Some("code"), Some("archive"), Some("app")];

/// Results asked for.
const LIMIT: u32 = 60;
/// Most matches a search tab lists (ferret-core's TAB_LIMIT).
const TAB_LIMIT: u64 = 2000;

/// "This folder" was the last scope picked (in this window).
static LAST_SCOPED: AtomicBool = AtomicBool::new(false);

/// The help's filters: what goes in the box, the part selected to type over, what it does.
const FILTERS: [(&str, &str, &str); 11] = [
    ("ext:pdf,docx", "pdf,docx", "These extensions"),
    ("size:>100mb", ">100mb", "Bigger than 100 MB. Also <1mb, 1mb..1gb, empty"),
    ("modified:<7d", "<7d", "Changed in the last 7 days. Also today, >1y, 2026-01-01..2026-03-31"),
    ("dm:2026-03-04", "2026-03-04", "Changed on this day"),
    ("in:C:\\dev", "C:\\dev", "Only inside this folder"),
    ("content:\"some text\"", "some text", "Files with this text inside (in this folder, or in:)"),
    ("!node_modules", "node_modules", "Leave out names with this"),
    ("!C:\\Windows", "C:\\Windows", "Leave out a folder"),
    ("\"two words\"", "two words", "These words together"),
    ("*.log", "*.log", "Wildcards: * is any text, ? one character"),
    ("type:file", "file", "Files only (type:folder for folders)"),
];

/// Filters offered while their name is typed ("ty" → type:).
const FILTER_KEYS: [(&str, &str, &str); 6] = [
    ("type", "shapes", "Images, videos, documents…"),
    ("ext", "hash", "An extension, like pdf"),
    ("size", "ruler", "Bigger or smaller than"),
    ("modified", "calendar-clock", "When it last changed"),
    ("in", "folder-search", "Only inside a folder"),
    ("content", "text-search", "Text inside files, in this folder"),
];
const SIZES: [(&str, &str); 5] =
    [(">100mb", "Bigger than 100 MB"), (">1gb", "Bigger than 1 GB"), ("<1mb", "Smaller than 1 MB"), ("1mb..100mb", "Between 1 and 100 MB"), ("empty", "Empty files")];
const DATES: [(&str, &str); 6] = [
    ("today", "Today"),
    ("yesterday", "Yesterday"),
    ("<7d", "In the last 7 days"),
    ("<30d", "In the last 30 days"),
    ("<1y", "In the last year"),
    (">1y", "More than a year ago"),
];
/// Extensions people look for most, first when several match.
const COMMON_EXTS: [&str; 32] = [
    "pdf", "docx", "xlsx", "pptx", "jpg", "png", "gif", "mp4", "mp3", "zip", "txt", "md", "exe", "msi", "csv", "json", "py", "js", "ts",
    "html", "mov", "mkv", "wav", "doc", "xls", "rar", "7z", "iso", "svg", "webp", "heic", "log",
];

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect($pat));
    };
}
re!(CONTENT, r#"(?i)(?:^|\s)content:(?:"[^"]+|[^\s"]+)"#);
re!(CONTENT_TEXT, r#"(?i)(?:^|\s)content:(?:"([^"]*)"?|(\S+))"#);
re!(IN_FILTER, r"(?i)(?:^|\s)(?:in|scope):\S");
re!(IN_PATH, r#"(?i)(?:^|\s)(?:in|scope):(?:"([^"]+)("?)|(\S+))"#);
re!(IN_PATH_TAIL, r"\s+!?\w+:.*$");
re!(TOKEN, r#""[^"]*"?|\S+"#);
re!(TYPE_TOKEN, r"(?i)^(?:type|kind):(.*)$");
re!(FILTER, r#"(?i)(^|\s)(?:type|kind|ext|size|modified|mod|dm|date|datemodified|in|scope|content):(?:"[^"]*"?|\S*)"#);
re!(AWAITING, r#"(?i)(?:^|\s)!?(?:type|kind|ext|size|modified|mod|dm|date|datemodified|in|scope|content):(?:[<>]=?|=|")?$"#);
re!(FILTER_WORD, r"(?i)^(type|kind|ext|size|modified|mod|dm|date|in|content):(.*)$");
re!(SIZE, r"(?i)^(?:\d+\.?\d*|\.\d+)\s*(?:b|k|kb|kib|m|mb|mib|g|gb|gib|t|tb|tib)?$");
re!(AGE, r"(?i)^\d+(?:s|secs?|m|mins?|h|hrs?|hours?|d|days?|w|wk|weeks?|mo|months?|y|yr|years?)$");
re!(COMPARE, r"^(?:>=|<=|>|<|=)\s*");
re!(BAD_KEY, r"(?i)^(size|modified|mod|dm|date|datemodified):(.*)$");

/// A file type as search knows it: `type:image`, its other names, its extensions.
#[derive(Clone)]
struct TypeInfo {
    id: String,
    label: String,
    words: Vec<String>,
    suggest: Vec<String>,
    exts: Vec<String>,
}

fn folders() -> TypeInfo {
    TypeInfo { id: "folder".into(), label: "Folders".into(), words: kinds::FOLDER_WORDS.iter().map(|s| s.to_string()).collect(), suggest: vec![], exts: vec![] }
}

fn file_types() -> Vec<TypeInfo> {
    kinds::file_types()
        .iter()
        .map(|t| TypeInfo { id: t.id.clone(), label: t.label.clone(), words: t.words.clone(), suggest: t.suggest.clone(), exts: t.exts.clone() })
        .collect()
}

/// Kinds that aren't types of their own but work in `type:` too.
fn kind_label(kind: &str) -> String {
    match kind {
        "pdf" => "PDFs".into(),
        "sheet" => "Spreadsheets".into(),
        "exe" => "Programs and shortcuts".into(),
        k => k.to_uppercase(),
    }
}

/// A type by its id or any of its words ("photos"), an icon kind ("pdf"), or folders.
fn find_type(word: &str) -> Option<TypeInfo> {
    let w = word.trim().to_lowercase();
    let w = w.trim_start_matches('.');
    if kinds::FOLDER_WORDS.contains(&w) {
        return Some(folders());
    }
    if let Some(t) = file_types().into_iter().find(|t| t.id == w || t.words.iter().any(|x| x == w)) {
        return Some(t);
    }
    let exts = kinds::kind_exts(w);
    (!exts.is_empty()).then(|| TypeInfo { id: w.to_string(), label: kind_label(w), words: vec![], suggest: vec![], exts })
}

/// Does `type:` understand this word?
fn known_type(word: &str) -> bool {
    find_type(word).is_some() || kinds::FILE_WORDS.contains(&word.trim().to_lowercase().as_str())
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(ca != b[j - 1]));
            prev = cur;
        }
    }
    row[b.len()]
}

/// The type closest to a misspelled word ("imgz", "vidoes"), by its main name, if any is close.
fn did_you_mean(word: &str) -> Option<String> {
    let w = word.trim().to_lowercase();
    if w.chars().count() < 2 {
        return None;
    }
    let mut names: Vec<(String, String)> = Vec::new();
    for t in file_types() {
        names.push((t.id.clone(), t.id.clone()));
        names.extend(t.words.iter().map(|n| (n.clone(), t.id.clone())));
    }
    for n in kinds::FOLDER_WORDS {
        names.push((n.into(), "folder".into()));
    }
    for n in kinds::FILE_WORDS {
        names.push((n.into(), "file".into()));
    }
    for n in ["pdf", "sheet", "exe"] {
        names.push((n.into(), n.into()));
    }
    let mut best: Option<String> = None;
    let mut best_d = f32::INFINITY;
    for (n, main) in names {
        let d = if n.starts_with(&w) { 0.5 } else { distance(&w, &n) as f32 };
        if d < best_d {
            best = Some(main);
            best_d = d;
        }
    }
    let allowed = if w.chars().count() <= 4 { 1. } else { 2. };
    best.filter(|_| best_d <= allowed)
}

/// Extensions shown for a type, short: "jpg, png, gif +19".
fn ext_preview(t: &TypeInfo, n: usize) -> String {
    let shown = t.exts.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if t.exts.len() > n { format!("{shown} +{}", t.exts.len() - n) } else { shown }
}

/// The icon of a file type (chips, help, suggestions): typeIcons.ts.
fn type_icon(id: &str) -> &'static str {
    match id {
        "folder" => "folder",
        "image" => "image",
        "video" => "film",
        "audio" => "music",
        "doc" | "pdf" => "file-text",
        "text" => "text-align-start",
        "code" => "code",
        "archive" => "archive",
        "app" | "exe" => "app-window",
        "data" => "braces",
        "font" => "type",
        "sheet" => "sheet",
        _ => "shapes",
    }
}

/// "Images" → "images", keeping "PDFs"; a type as files in a sentence: "images", "text files".
fn files_word(label: &str) -> String {
    let lower: String = label
        .split(' ')
        .map(|w| {
            let mut c = w.chars();
            match (c.next(), c.next()) {
                (Some(a), Some(b)) if a.is_uppercase() && b.is_lowercase() => format!("{}{}", a.to_lowercase(), &w[a.len_utf8()..]),
                _ => w.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if label.ends_with('s') { lower } else { format!("{lower} files") }
}

/// `type:` and `kind:` filters in a query: each as typed, with its values. Quoted text doesn't count.
fn type_tokens(query: &str) -> Vec<(String, Vec<String>)> {
    TOKEN
        .find_iter(query)
        .filter_map(|m| {
            let c = TYPE_TOKEN.captures(m.as_str())?;
            let values = c[1].split([',', ';', '|']).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).collect();
            Some((m.as_str().to_string(), values))
        })
        .collect()
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The query without its `type:` and `kind:` filters.
fn without_types(query: &str) -> String {
    let mut q = query.to_string();
    for (raw, _) in type_tokens(query) {
        q = q.replacen(&raw, " ", 1);
    }
    collapse(&q)
}

/// The box without its filters: what "Remove filters" leaves.
fn without_filters(query: &str) -> String {
    collapse(&FILTER.replace_all(query, "${1}"))
}

/// The box ends with a filter that has no value yet ("ext:", "size:>", `in:"`).
fn awaiting_value(query: &str) -> bool {
    AWAITING.is_match(query.trim())
}

/// The folder an `in:` names. A quote still open runs to the next filter, if there is one.
fn in_folder_path(query: &str) -> Option<String> {
    let c = IN_PATH.captures(query)?;
    let dir = match (c.get(3), c.get(1)) {
        (Some(bare), _) => bare.as_str().to_string(),
        (None, Some(q)) if c.get(2).is_some_and(|m| !m.as_str().is_empty()) => q.as_str().to_string(),
        (None, Some(q)) => IN_PATH_TAIL.replace(q.as_str(), "").into_owned(),
        _ => return None,
    };
    let dir = dir.trim().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// The folder an `in:` names, as it reads in a chip or a sentence.
fn in_folder_name(query: &str) -> Option<String> {
    let dir = in_folder_path(query)?;
    let dir = dir.trim_end_matches(['\\', '/']);
    if dir.is_empty() {
        return None;
    }
    Some(dir.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(dir).to_string())
}

/// The text a search inside files looks for, as typed.
fn content_text(query: &str) -> String {
    CONTENT_TEXT.captures(query).and_then(|c| c.get(1).or(c.get(2))).map(|m| m.as_str().to_string()).unwrap_or_default()
}

/// What a query looks like in a sentence: it may be pasted, and very long.
fn quoted(q: &str) -> String {
    if q.chars().count() > 60 { format!("“{}…”", q.chars().take(60).collect::<String>()) } else { format!("“{q}”") }
}

/// A loose hit: some word of the search is neither in the name nor in the path, only as
/// scattered letters. Search ranks these last. Wildcards and paths are matched exactly.
fn is_loose(path: &str, query: &str) -> bool {
    let where_ = crate::filter::fold(path);
    without_filters(query)
        .split_whitespace()
        .filter(|w| !w.starts_with('!') && !w.chars().any(|c| matches!(c, '*' | '?' | '"' | '\\' | '/')))
        .any(|w| !where_.contains(&crate::filter::fold(w)))
}

fn valid_date(v: &str) -> bool {
    let parts: Vec<&str> = v.trim().split(['-', '/', '.']).collect();
    if parts.len() > 3 || !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    let n: Vec<u32> = parts.iter().map(|p| p.parse().unwrap_or(0)).collect();
    let (y, m, d) = (n[0], n.get(1).copied().unwrap_or(1), n.get(2).copied().unwrap_or(1));
    if y < 1601 || !(1..=12).contains(&m) || d < 1 {
        return false;
    }
    let days = match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    d <= days
}

fn valid_size(value: &str) -> bool {
    let v = value.trim();
    if v.eq_ignore_ascii_case("empty") {
        return true;
    }
    if let Some((a, b)) = v.split_once("..") {
        return SIZE.is_match(a.trim()) && SIZE.is_match(b.trim());
    }
    SIZE.is_match(&COMPARE.replace(v, ""))
}

fn valid_date_filter(value: &str) -> bool {
    let v = value.trim().to_lowercase();
    if ["today", "yesterday", "week", "thisweek", "month", "thismonth", "year", "thisyear"].contains(&v.as_str()) {
        return true;
    }
    if let Some((a, b)) = v.split_once("..") {
        return valid_date(a) && valid_date(b);
    }
    let rest = COMPARE.replace(&v, "");
    AGE.is_match(&rest) || valid_date(&rest)
}

/// A `size:` or `modified:` filter search can't read (it would ignore it): as typed, whether it's
/// a size, and one that works. A value still empty ("size:", "size:>") is one being typed.
struct BadFilter {
    raw: String,
    size: bool,
    example: String,
}

fn bad_filter(query: &str) -> Option<BadFilter> {
    for raw in query.split_whitespace() {
        let Some(c) = BAD_KEY.captures(raw) else { continue };
        if COMPARE.replace(&c[2], "").is_empty() {
            continue;
        }
        if c[1].eq_ignore_ascii_case("size") {
            if !valid_size(&c[2]) {
                return Some(BadFilter { raw: raw.into(), size: true, example: "size:>100mb".into() });
            }
        } else if !valid_date_filter(&c[2]) {
            return Some(BadFilter { raw: raw.into(), size: false, example: format!("{}:<7d", &c[1]) });
        }
    }
    None
}

/// The command list's scorer: a run of the letters first, then letters in order (Tauri's `fuzzy`).
/// Returns the score and the byte offsets matched.
fn fuzzy(label: &str, query: &str) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let l: Vec<(usize, char)> = label.char_indices().map(|(i, c)| (i, c.to_lowercase().next().unwrap_or(c))).collect();
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let lower: Vec<char> = l.iter().map(|(_, c)| *c).collect();
    if let Some(at) = (0..lower.len()).find(|&i| lower[i..].starts_with(&q)) {
        return Some((1000 - at as i32, (at..at + q.len()).map(|k| l[k].0).collect()));
    }
    let mut hits = Vec::new();
    let mut i = 0;
    for ch in q {
        let found = (i..lower.len()).find(|&k| lower[k] == ch)?;
        hits.push(found);
        i = found + 1;
    }
    let span = (hits[hits.len() - 1] - hits[0]) as i32;
    Some((500 - span, hits.into_iter().map(|k| l[k].0).collect()))
}

// ───────────────────────────── rows ─────────────────────────────

/// A piece of a suggestion's text.
enum Seg {
    Text(String),
    /// The label (the main words).
    Label(String),
    /// Filter syntax, in a code chip.
    Code(String),
    /// A word that matters, in the accent.
    Bold(String),
    /// A remembered search, as typed.
    Query(String),
}

/// What choosing a suggestion does.
#[derive(Clone)]
enum Apply {
    /// Puts this in place of the word being typed (then a space, unless false).
    Complete(String, bool),
    /// The box says this instead.
    SetText(String),
    /// The box says `rest` and the type chip is `id`.
    TypeChip { rest: String, id: String },
    /// Opens the help.
    Help,
}

struct Suggestion {
    icon: &'static str,
    main: Vec<Seg>,
    detail: String,
    apply: Apply,
    /// Tab takes this row: it finishes what's being typed (and shows the Tab badge).
    completes: bool,
    /// The filter value this row fills in: typed out in full, it has nothing left to offer.
    fills: Option<String>,
}

/// A row above the results.
enum Row {
    Suggest(Suggestion),
    Saved(String, String),
    Recent(String),
    /// Every match as a search tab (Ctrl+Shift+Enter): what it says.
    OpenAll(String),
}

/// A result, and whether it's loose (a word of the search only as scattered letters).
struct Shown {
    hit: Hit,
    loose: bool,
}

/// The results as listed: one list in search's own order (frecency included), loose matches
/// last. **Differs from the app on purpose (user's request, 2026-11-11):** the app lists the open
/// folder's matches first under "In <folder>" (`mergeNear`); here "Everywhere" is one ranked list,
/// and only the "This folder" scope keeps to the open folder.
fn ranked(results: &[Hit], query: &str) -> Vec<Shown> {
    let (firm, loose): (Vec<Shown>, Vec<Shown>) =
        results.iter().map(|h| Shown { loose: is_loose(&h.path, query), hit: h.clone() }).partition(|s| !s.loose);
    firm.into_iter().chain(loose).collect()
}

/// A button the keyboard can be on (SearchPalette.tsx's `focusable`): Tab belongs to the box
/// (it completes, or switches the scope), so these are reached with Shift+Tab, and Tab /
/// Shift+Tab go round them and the box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    /// The chosen type, in the box.
    Pill,
    Save,
    Help,
    Scope,
    Close,
    /// A type chip under the box (its Alt+number).
    Chip(usize),
    /// The help's rows: a type (folders first), then a filter.
    HelpType(usize),
    HelpFilter(usize),
}

impl Btn {
    fn name(self) -> String {
        match self {
            Btn::Pill => "pill".into(),
            Btn::Save => "save".into(),
            Btn::Help => "help".into(),
            Btn::Scope => "scope".into(),
            Btn::Close => "close".into(),
            Btn::Chip(i) => format!("chip:{i}"),
            Btn::HelpType(i) => format!("help-type:{i}"),
            Btn::HelpFilter(i) => format!("help-filter:{i}"),
        }
    }
}

/// The keyboard's ring on a palette button (`button:focus-visible`: 2 px of accent text color,
/// 2 px outside the button). The button must be `relative`.
fn focus_ring(radius: f32) -> gpui::Div {
    div()
        .absolute()
        .top(px(-4.))
        .left(px(-4.))
        .right(px(-4.))
        .bottom(px(-4.))
        .rounded(px(radius + 4.))
        .border_2()
        .border_color(t::accent_text())
}

/// Where the palette goes narrow (palette.css's `@media (max-width: 640px)`): each result in two
/// lines (name over its folder), the type chips wrapping. The window's narrowest (its 640 px
/// minimum) measures 642 here, which has to count: otherwise no window would ever get it.
const NARROW: f32 = 642.;

/// The numbers of the last answer.
struct Answer {
    total: u64,
    len: usize,
    took_us: u64,
    source: String,
    partial: bool,
    more: bool,
    scanned: u64,
}

pub struct Palette {
    input: Entity<TextInput>,
    commands: Vec<Command>,
    /// Commands matching the text: (index, score, matched byte offsets).
    matched: Vec<(usize, i32, Vec<usize>)>,
    /// Rows above the results (suggestions, saved and recent searches, "Open all in a tab").
    sugg: Vec<Row>,
    /// Results, in search's order, loose matches last (`ranked`).
    shown: Vec<Shown>,
    selected: usize,
    scroll: ScrollHandle,
    folder: Option<String>,
    forced_scope: Option<String>,
    /// Search only the open folder (Tab switches).
    scoped: bool,
    type_chip: Option<String>,
    help_open: bool,
    history: Vec<String>,
    saved: Vec<(String, String)>,
    recent_folders: Vec<String>,
    frecency: HashMap<String, f32>,
    res: Option<Answer>,
    /// The query `res` and `shown` answer.
    answered: Option<String>,
    failed: Option<String>,
    /// An `in:` folder that can't be opened, once a search in it found nothing.
    missing: Option<String>,
    index: Option<search::SearchStatus>,
    token: u64,
    bench: Option<PathBuf>,
    /// A result pressed (where), until it's released or dragged; and whether it was dragged
    /// (its click is then not an open).
    press: Option<(usize, Point<Pixels>)>,
    dragged: bool,
    /// A search inside files is running (closing the palette or a newer query stops it).
    reading: bool,
    /// A search is running (the next one waits for it).
    searching: bool,
    /// Text changed while one ran: search again when it's done.
    queued: bool,
    /// A search taking long enough to say so (most take milliseconds).
    slow: bool,
    /// The index's state is being asked every 0.7 s.
    polling: bool,
    /// The keyboard is on one of the buttons (`kbd`) instead of the box: the palette's own focus
    /// handle has the keys then, so typing doesn't reach the box (as on the app's buttons).
    own_focus: FocusHandle,
    kbd: Option<Btn>,
    /// The type chips' row, scrolled to the chip the keyboard is on.
    chips_scroll: ScrollHandle,
    /// A help row just got the keyboard: it's scrolled into view when next drawn.
    reveal_kbd: std::rc::Rc<std::cell::Cell<bool>>,
    /// The window's width as last drawn (for tests: the narrow layout).
    drawn_w: std::cell::Cell<f32>,
    _changes: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Drop for Palette {
    fn drop(&mut self) {
        if self.reading {
            search::search_cancel();
        }
    }
}

impl Palette {
    pub fn new(init: Init, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search files and folders everywhere…", cx).passes_idle_edits());
        let changes = cx.subscribe(&input, |this, _, _: &input::Changed, cx| {
            this.help_open = false;
            this.edited(cx);
        });
        let focus: FocusHandle = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        // The scope picked last time, where there's a folder to search.
        let scoped = LAST_SCOPED.load(Ordering::Relaxed) && init.folder.is_some();
        let mut this = Self {
            input: input.clone(),
            commands: init.commands,
            matched: Vec::new(),
            sugg: Vec::new(),
            shown: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            folder: init.folder,
            forced_scope: init.forced_scope,
            scoped,
            type_chip: None,
            help_open: false,
            history: init.history,
            saved: init.saved,
            recent_folders: init.recent_folders,
            frecency: init.frecency,
            res: None,
            answered: None,
            failed: None,
            missing: None,
            index: None,
            token: 0,
            bench: init.bench,
            press: None,
            dragged: false,
            reading: false,
            searching: false,
            queued: false,
            slow: false,
            polling: false,
            own_focus: cx.focus_handle(),
            kbd: None,
            chips_scroll: ScrollHandle::new(),
            reveal_kbd: Default::default(),
            drawn_w: Default::default(),
            _changes: changes,
        };
        this.poll_index(cx);
        match init.query {
            Some(q) => input.update(cx, |i, cx| {
                i.set_text(&q, cx);
                if init.select_all {
                    i.select_everything(cx);
                }
            }),
            None => this.edited(cx),
        }
        this
    }

    /// The drive index's state, again every 0.7 s while it's being built ("Indexing 40 %").
    /// Also asked again when results say "still indexing" (a WSL distro or server listed for the
    /// first time starts after the palette opened); once the index is built, a search that was
    /// answered while it wasn't runs again, so the new matches show without retyping.
    fn poll_index(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.polling, true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                let status = cx.background_executor().spawn(async { ferret_core::rt::block_on(search::search_status()).ok() }).await;
                let again = this
                    .update(cx, |p, cx| {
                        let was = p.index.as_ref().is_some_and(|s| s.building);
                        p.index = status;
                        let building = p.index.as_ref().is_none_or(|s| s.building);
                        if was && !building && p.res.as_ref().is_some_and(|r| r.partial) {
                            p.answered = None;
                            p.run(cx);
                        }
                        if !building {
                            p.polling = false;
                        }
                        cx.notify();
                        building
                    })
                    .unwrap_or(false);
                if !again {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(700)).await;
            }
        })
        .detach();
    }

    fn text(&self, cx: &Context<Self>) -> String {
        self.input.read(cx).content.to_string()
    }

    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_text(text, cx));
    }

    fn command_mode(&self, cx: &Context<Self>) -> bool {
        self.text(cx).starts_with('>')
    }

    /// What's typed (without the `>` of commands).
    fn query(&self, cx: &Context<Self>) -> String {
        let t = self.text(cx);
        if let Some(rest) = t.strip_prefix('>') { rest.trim().to_string() } else { t.trim().to_string() }
    }

    /// What's searched: the box plus the chosen type.
    fn full_query(&self, cx: &Context<Self>) -> String {
        let q = self.query(cx);
        match (&self.type_chip, self.command_mode(cx)) {
            (Some(t), false) => format!("{q} type:{t}").trim().to_string(),
            _ => q,
        }
    }

    fn show_help(&self, cx: &Context<Self>) -> bool {
        !self.command_mode(cx) && (self.help_open || self.text(cx).trim() == "?")
    }

    /// A search inside files (`content:"…"` with something to find).
    fn content_search(&self, cx: &Context<Self>) -> bool {
        !self.command_mode(cx) && CONTENT.is_match(&self.full_query(cx))
    }

    /// An `in:` in the box names the folder itself; "This folder" has nothing to add to it.
    fn own_folder(&self, cx: &Context<Self>) -> bool {
        !self.command_mode(cx) && IN_FILTER.is_match(&self.full_query(cx))
    }

    /// The folder a search inside files reads when no `in:` names one: the open one.
    fn content_folder(&self, cx: &Context<Self>) -> Option<String> {
        if self.content_search(cx) && !self.own_folder(cx) { self.folder.clone() } else { None }
    }

    fn search_folder(&self, cx: &Context<Self>) -> Option<String> {
        if self.own_folder(cx) {
            None
        } else if self.scoped {
            self.folder.clone()
        } else {
            self.content_folder(cx)
        }
    }

    /// The scope a search runs with; tests keep it inside their folder.
    fn scope_for_search(&self, cx: &Context<Self>) -> Option<String> {
        let want = self.search_folder(cx);
        match &self.forced_scope {
            None => want,
            Some(f) => Some(want.filter(|w| util::within(w, f)).unwrap_or_else(|| f.clone())),
        }
    }

    /// The search as a tab sees it: "This folder" (or the folder a search inside files reads)
    /// becomes an `in:` filter. (Tests' searches stay in their folder in a tab too.)
    fn tab_query(&self, cx: &Context<Self>) -> String {
        let full = self.full_query(cx);
        let q = match self.search_folder(cx) {
            Some(dir) => format!("{full} in:\"{dir}\""),
            None => full,
        };
        match &self.forced_scope {
            Some(f) if !q.is_empty() && !IN_FILTER.is_match(&q) => format!("{q} in:\"{f}\""),
            _ => q.trim().to_string(),
        }
    }

    /// Tab can switch the scope: an `in:` or a search inside files has already picked the folder.
    fn can_switch_scope(&self, cx: &Context<Self>) -> bool {
        !self.command_mode(cx) && !self.own_folder(cx) && self.content_folder(cx).is_none() && self.folder.is_some()
    }

    fn set_scoped(&mut self, on: bool, cx: &mut Context<Self>) {
        LAST_SCOPED.store(on, Ordering::Relaxed);
        self.scoped = on;
        self.update_placeholder(cx);
        self.requery(cx);
    }

    /// What the empty box says: commands, the folder searched, or everywhere.
    fn update_placeholder(&mut self, cx: &mut Context<Self>) {
        let placeholder = if self.command_mode(cx) {
            "Type a command…".to_string()
        } else if self.scoped {
            format!("Search in {}…", self.folder.as_deref().map(util::folder_name).unwrap_or_default())
        } else {
            "Search files and folders everywhere…".to_string()
        };
        self.input.update(cx, |i, cx| i.set_placeholder(placeholder, cx));
    }

    /// The text changed: commands, or a search and the rows above it.
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.update_placeholder(cx);
        if self.command_mode(cx) {
            self.match_commands(cx);
            self.selected = 0;
            cx.notify();
            return;
        }
        self.requery(cx);
    }

    fn requery(&mut self, cx: &mut Context<Self>) {
        self.run(cx);
        self.refresh(cx);
        cx.notify();
    }

    fn match_commands(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        let mut m: Vec<(usize, i32, Vec<usize>)> =
            self.commands.iter().enumerate().filter_map(|(i, c)| fuzzy(&c.label, &q).map(|(s, h)| (i, s, h))).collect();
        m.sort_by(|a, b| b.1.cmp(&a.1));
        m.truncate(40);
        self.matched = m;
    }

    // ───────────────────────────── searching ─────────────────────────────

    /// The latest query, one request at a time.
    fn run(&mut self, cx: &mut Context<Self>) {
        if self.command_mode(cx) {
            return;
        }
        let q = self.full_query(cx);
        if q.is_empty() || self.text(cx).trim() == "?" {
            self.res = None;
            self.shown.clear();
            self.answered = None;
            self.failed = None;
            self.missing = None;
            return;
        }
        if self.searching {
            // A search inside files can take a while: a newer query stops it.
            if self.reading && !self.queued {
                search::search_cancel();
            }
            self.queued = true;
            return;
        }
        self.start(q, cx);
    }

    fn start(&mut self, q: String, cx: &mut Context<Self>) {
        self.searching = true;
        self.reading = CONTENT.is_match(&q);
        self.failed = None;
        self.token += 1;
        let token = self.token;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            this.update(cx, |p, cx| {
                if p.searching && p.token == token {
                    p.slow = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        let scope = self.scope_for_search(cx);
        let frecency = (!self.frecency.is_empty()).then(|| self.frecency.clone());
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let q2 = q.clone();
            let found = cx.background_executor().spawn(async move { ferret_core::rt::block_on(search::search_query(q2, Some(LIMIT), scope, frecency)) }).await;
            let took = started.elapsed();
            this.update(cx, |p, cx| p.answer(q, found, took, cx)).ok();
        })
        .detach();
    }

    fn answer(&mut self, q: String, found: Result<SearchResults, String>, took: Duration, cx: &mut Context<Self>) {
        self.searching = false;
        self.reading = false;
        self.slow = false;
        if q == self.full_query(cx) && !self.command_mode(cx) {
            match found {
                Ok(r) => {
                    if let Some(out) = &self.bench {
                        bench::write_to(out, "palette_results", r.results.len());
                        bench::write_to(out, "palette_total", r.total);
                        bench::write_to(out, "palette_ms", format!("{:.0}", took.as_secs_f64() * 1000.));
                        bench::write_to(out, "palette_source", &r.source);
                    }
                    self.shown = ranked(&r.results, &q);
                    self.res = Some(Answer {
                        total: r.total,
                        len: r.results.len(),
                        took_us: r.took_us,
                        source: r.source,
                        partial: r.partial,
                        more: r.more,
                        scanned: r.scanned,
                    });
                    self.failed = None;
                }
                Err(e) => {
                    self.res = None;
                    self.shown.clear();
                    self.failed = Some(e);
                }
            }
            self.answered = Some(q);
            // Still indexing (a distro listed for the first time): follow it until it's done.
            if self.res.as_ref().is_some_and(|r| r.partial) {
                self.poll_index(cx);
            }
        }
        if std::mem::take(&mut self.queued) {
            let next = self.full_query(cx);
            if !self.command_mode(cx) && !next.is_empty() && self.text(cx).trim() != "?" {
                self.start(next, cx);
            }
        }
        self.refresh(cx);
        self.check_missing(cx);
        cx.notify();
    }

    /// The results on screen answer what's in the box.
    fn settled(&self, cx: &Context<Self>) -> bool {
        !self.searching && self.answered.as_deref() == Some(self.full_query(cx).as_str())
    }

    /// An `in:` folder that can't be opened, once a search in it found nothing.
    fn check_missing(&mut self, cx: &mut Context<Self>) {
        let q = self.full_query(cx);
        let dir = (self.settled(cx) && self.shown.is_empty() && !self.content_search(cx) && !awaiting_value(&q)).then(|| in_folder_path(&q)).flatten();
        let Some(dir) = dir.filter(|d| !ferret_core::paths::is_remote(d)) else {
            self.missing = None;
            return;
        };
        cx.spawn(async move |this, cx| {
            let d2 = dir.clone();
            let ok = cx
                .background_executor()
                .spawn(async move { std::fs::metadata(ferret_core::paths::for_io(&d2)).is_ok_and(|m| m.is_dir()) })
                .await;
            this.update(cx, |p, cx| {
                if p.full_query(cx) == q {
                    p.missing = (!ok).then_some(dir);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── rows above the results ─────────────────────────────

    /// Suggestions for what's typed, and the row to pick: the first when a filter is being
    /// finished (or there are no results), else the first result.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (rows, first) = self.suggestions(cx);
        self.sugg = rows;
        self.selected = if first || self.shown.is_empty() { 0 } else { self.sugg.len() };
        self.scroll.scroll_to_item(0);
    }

    fn typing(text: &str) -> Option<String> {
        if text.ends_with(char::is_whitespace) {
            return None;
        }
        text.split_whitespace().last().map(str::to_string)
    }

    fn typed_types(&self, cx: &Context<Self>) -> Vec<(String, Vec<String>)> {
        if self.command_mode(cx) { Vec::new() } else { type_tokens(&self.query(cx)) }
    }

    fn type_active(&self, id: Option<&str>, cx: &Context<Self>) -> bool {
        let ids: Vec<String> = self.typed_types(cx).iter().flat_map(|(_, v)| v.iter().filter_map(|v| find_type(v).map(|t| t.id))).collect();
        match id {
            None => self.type_chip.is_none() && ids.is_empty(),
            Some(id) => self.type_chip.as_deref() == Some(id) || ids.iter().any(|i| i == id),
        }
    }

    fn history_rows(&self) -> Vec<Row> {
        let saved_q = |q: &str| self.saved.iter().any(|(_, s)| s.trim().eq_ignore_ascii_case(q.trim()));
        let mut rows: Vec<Row> = self.saved.iter().map(|(n, q)| Row::Saved(n.clone(), q.clone())).collect();
        rows.extend(self.history.iter().filter(|q| !saved_q(q)).take(8).map(|q| Row::Recent(q.clone())));
        rows
    }

    fn suggestions(&self, cx: &Context<Self>) -> (Vec<Row>, bool) {
        if self.command_mode(cx) || self.show_help(cx) {
            return (Vec::new(), false);
        }
        let text = self.text(cx);
        // An empty box offers saved searches, then recent ones.
        if text.trim().is_empty() && self.type_chip.is_none() {
            return (self.history_rows(), true);
        }
        let word = Self::typing(&text);
        let filter = word.as_deref().filter(|w| !w.contains('"')).and_then(|w| FILTER_WORD.captures(w)).map(|c| (c[1].to_string(), c[2].to_string()));
        if let Some((key, value)) = &filter {
            // Values for the filter being typed come first (Enter or Tab takes one). A value
            // already typed out in full has nothing to finish: Enter is for the results then.
            let rows = self.filter_values(key, value);
            let last = value.rsplit(',').next().unwrap_or("").replace('"', "").to_lowercase();
            if !rows.is_empty() && !rows.iter().any(|r| r.fills.as_deref().map(str::to_lowercase).as_deref() == Some(last.as_str())) {
                return (rows.into_iter().map(Row::Suggest).collect(), true);
            }
        }
        // A `type:` word search doesn't know, and the closest one it does.
        for (raw, values) in self.typed_types(cx) {
            if let Some(bad) = values.iter().find(|v| !known_type(v)) {
                let fix = did_you_mean(bad);
                let mut main = vec![Seg::Text(format!("Unknown type “{bad}”. "))];
                let apply = match &fix {
                    Some(f) => {
                        main.extend([Seg::Text("Did you mean ".into()), Seg::Bold(f.clone()), Seg::Text("?".into())]);
                        Apply::SetText(text.replacen(&raw, &raw.replacen(bad.as_str(), f, 1), 1))
                    }
                    None => {
                        main.push(Seg::Text("See the types".into()));
                        Apply::Help
                    }
                };
                let completes = fix.is_some();
                return (vec![Row::Suggest(Suggestion { icon: "info", main, detail: String::new(), apply, completes, fills: None })], true);
            }
        }
        let mut rows = Vec::new();
        // Search ignores a value it can't read, so the results aren't limited the way it looks.
        if let Some(bad) = bad_filter(&self.query(cx)) {
            rows.push(Row::Suggest(Suggestion {
                icon: "info",
                main: vec![
                    Seg::Text("Ignoring ".into()),
                    Seg::Code(bad.raw.clone()),
                    Seg::Text(format!(": not a {}. Try ", if bad.size { "size" } else { "date" })),
                    Seg::Code(bad.example.clone()),
                ],
                detail: String::new(),
                apply: Apply::SetText(text.replacen(&bad.raw, &bad.example, 1)),
                completes: false,
                fills: None,
            }));
        }
        let w = word.as_deref().unwrap_or("").to_lowercase();
        if w.chars().count() >= 2 && filter.is_none() && !w.contains([':', '!', '"', '\\']) {
            for (key, icon_name, what) in FILTER_KEYS {
                if key.starts_with(&w) {
                    rows.push(Row::Suggest(Suggestion {
                        icon: icon_name,
                        main: vec![Seg::Code(format!("{key}:")), Seg::Text(format!(" {what}"))],
                        detail: String::new(),
                        apply: Apply::Complete(format!("{key}:"), false),
                        completes: true,
                        fills: None,
                    }));
                }
            }
        }
        if let Some((ty, rest)) = self.word_type(cx) {
            let mut main = vec![Seg::Text(if rest.is_empty() { "All ".into() } else { "Only ".into() }), Seg::Bold(files_word(&ty.label))];
            if !rest.is_empty() {
                main.push(Seg::Text(format!(" named “{rest}”")));
            }
            rows.push(Row::Suggest(Suggestion {
                icon: type_icon(&ty.id),
                main,
                detail: ext_preview(&ty, 4),
                apply: Apply::TypeChip { rest, id: ty.id.clone() },
                completes: false,
                fills: None,
            }));
        }
        // Last, so the first result stays picked: every match as a tab.
        if let Some(r) = self.res.as_ref().filter(|r| r.len > 0) {
            let loose = self.shown.iter().filter(|s| s.loose).count() as u64;
            let firm = self.shown.len() as u64 - loose;
            let loose_note = if loose > 0 && !r.more { format!(", {} of them loose", kinds::group_digits(r.total.saturating_sub(firm) as usize)) } else { String::new() };
            let label = if r.more {
                "Open every match in a tab".to_string()
            } else if r.total > TAB_LIMIT {
                format!("Open the first 2,000 of {} matches in a tab", kinds::group_digits(r.total as usize))
            } else if r.total == 1 {
                "Open the match in a tab".to_string()
            } else {
                format!("Open all {} matches in a tab{loose_note}", kinds::group_digits(r.total as usize))
            };
            rows.push(Row::OpenAll(label));
        }
        (rows, false)
    }

    /// "vacation videos", "image" or "text files": the type word, to offer it as a filter.
    fn word_type(&self, cx: &Context<Self>) -> Option<(TypeInfo, String)> {
        if self.command_mode(cx) || self.type_chip.is_some() || !self.typed_types(cx).is_empty() || self.text(cx).contains('"') {
            return None;
        }
        let q = self.query(cx);
        let words: Vec<&str> = q.split_whitespace().collect();
        let types = file_types();
        for i in 0..words.len() {
            let w = words[i].to_lowercase();
            let pair = i + 1 < words.len() && matches!(words[i + 1].to_lowercase().as_str(), "file" | "files");
            let ty = if pair { find_type(&w) } else { None }.or_else(|| {
                if kinds::FOLDER_WORDS.contains(&w.as_str()) { Some(folders()) } else { types.iter().find(|t| t.suggest.contains(&w)).cloned() }
            });
            if let Some(ty) = ty {
                let pair = pair && find_type(&w).is_some();
                let rest = words.iter().enumerate().filter(|(j, _)| *j != i && !(pair && *j == i + 1)).map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
                return Some((ty, rest));
            }
        }
        None
    }

    /// Values for a filter being typed (`type:im` → Images; `ext:p` → pdf, png…), each finishing it.
    fn filter_values(&self, key: &str, value: &str) -> Vec<Suggestion> {
        let k = key.to_lowercase();
        // Lists (`type:image,vi`) complete their last item.
        let (before, cur) = match value.rfind(',') {
            Some(i) => (&value[..=i], value[i + 1..].to_lowercase()),
            None => ("", value.to_lowercase()),
        };
        let finish = |v: &str| Apply::Complete(format!("{key}:{before}{v}"), true);
        match k.as_str() {
            "type" | "kind" => {
                let file = TypeInfo { id: "file".into(), label: "Files".into(), words: vec![], suggest: vec![], exts: vec![] };
                let mut types = vec![folders(), file];
                types.extend(file_types());
                types.extend(find_type("pdf"));
                types.extend(find_type("sheet"));
                types
                    .into_iter()
                    .filter(|t| cur.is_empty() || t.id.starts_with(&cur) || t.words.iter().any(|w| w.starts_with(&cur)))
                    .map(|t| Suggestion {
                        icon: type_icon(&t.id),
                        main: vec![Seg::Label(if t.id == "file" { "Files only".into() } else { t.label.clone() }), Seg::Text(" ".into()), Seg::Code(format!("type:{}", t.id))],
                        detail: ext_preview(&t, 4),
                        apply: finish(&t.id),
                        completes: true,
                        fills: Some(t.id.clone()),
                    })
                    .collect()
            }
            "content" => vec![Suggestion {
                icon: "text-search",
                main: vec![
                    Seg::Code("content:\"…\"".into()),
                    Seg::Text(format!(" Text inside files, in {}", self.folder.as_deref().map(util::folder_name).filter(|n| !n.is_empty()).unwrap_or_else(|| "this folder".into()))),
                ],
                detail: String::new(),
                apply: Apply::Complete("content:\"".into(), false),
                completes: true,
                fills: None,
            }],
            "ext" => {
                let mut all: Vec<String> = COMMON_EXTS.iter().map(|s| s.to_string()).collect();
                let json: serde_json::Value = serde_json::from_str(include_str!("../../src/lib/filetypes.json")).unwrap_or_default();
                for exts in json["kinds"].as_object().into_iter().flat_map(|m| m.values()) {
                    for e in exts.as_str().unwrap_or("").split_whitespace() {
                        if !all.iter().any(|a| a == e) {
                            all.push(e.to_string());
                        }
                    }
                }
                let rank = |e: &str| COMMON_EXTS.iter().position(|c| *c == e).unwrap_or(100 + e.len());
                let mut exts: Vec<String> = all.into_iter().filter(|e| e.starts_with(&cur)).collect();
                exts.sort_by_key(|e| rank(e));
                exts.into_iter()
                    .take(8)
                    .map(|e| {
                        let kind = kinds::kind_of(false, &e);
                        Suggestion {
                            icon: kinds::icon(kind),
                            main: vec![Seg::Code(format!(".{e}"))],
                            detail: kinds::group_label(kind).into(),
                            apply: finish(&e),
                            completes: true,
                            fills: Some(e),
                        }
                    })
                    .collect()
            }
            "size" | "modified" | "mod" | "date" | "dm" => {
                let (list, icon_name): (&[(&str, &str)], &'static str) = if k == "size" { (&SIZES, "ruler") } else { (&DATES, "calendar-clock") };
                list.iter()
                    .filter(|(v, _)| v.starts_with(&cur))
                    .map(|(v, what)| Suggestion {
                        icon: icon_name,
                        main: vec![Seg::Code(format!("{key}:{v}")), Seg::Text(format!(" {what}"))],
                        detail: String::new(),
                        apply: finish(v),
                        completes: true,
                        fills: Some(v.to_string()),
                    })
                    .collect()
            }
            _ => {
                // in: this folder, then recent ones.
                let mut dirs: Vec<String> = Vec::new();
                for d in self.folder.iter().chain(self.recent_folders.iter()) {
                    if !dirs.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                        dirs.push(d.clone());
                    }
                }
                let v = value.trim_matches('"').to_lowercase();
                dirs.into_iter()
                    .filter(|p| v.is_empty() || p.to_lowercase().contains(&v))
                    .take(6)
                    .map(|p| Suggestion {
                        icon: "folder-search",
                        main: vec![Seg::Label({
                            let n = util::file_name(&p);
                            if n.is_empty() { p.clone() } else { n }
                        })],
                        detail: p.clone(),
                        apply: Apply::Complete(if p.contains(char::is_whitespace) { format!("in:\"{p}\"") } else { format!("in:{p}") }, true),
                        completes: true,
                        fills: Some(p),
                    })
                    .collect()
            }
        }
    }

    /// Puts `token` in place of the word being typed.
    fn complete(&mut self, token: &str, space: bool, cx: &mut Context<Self>) {
        let t = self.text(cx);
        let base = if t.ends_with(char::is_whitespace) { t.as_str() } else { t.trim_end_matches(|c: char| !c.is_whitespace()) };
        let next = format!("{base}{token}{}", if space { " " } else { "" });
        self.help_open = false;
        self.set_text(&next, cx);
    }

    fn apply(&mut self, apply: Apply, cx: &mut Context<Self>) {
        match apply {
            Apply::Complete(token, space) => self.complete(&token, space, cx),
            Apply::SetText(text) => self.set_text(&text, cx),
            Apply::TypeChip { rest, id } => {
                self.type_chip = Some(id);
                if rest == self.text(cx) { self.requery(cx) } else { self.set_text(&rest, cx) }
            }
            Apply::Help => {
                self.help_open = true;
                cx.notify();
            }
        }
    }

    /// A remembered search goes in the box (it carries its own folder: the scope picked for the
    /// next Ctrl+K stays as it was).
    fn use_query(&mut self, q: String, cx: &mut Context<Self>) {
        self.type_chip = None;
        self.scoped = false;
        self.set_text(&q, cx);
    }

    /// A chip replaces any type typed in the box (and closes the help).
    fn pick_type(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let rest = without_types(if text.trim() == "?" { "" } else { &text });
        self.type_chip = match id {
            Some(id) if self.type_chip.as_deref() != Some(id) => Some(id.to_string()),
            _ => None,
        };
        self.help_open = false;
        if rest != text { self.set_text(&rest, cx) } else { self.requery(cx) }
    }

    /// Adds a filter from the help to the box, its value selected to type over.
    fn insert_filter(&mut self, token: &str, value: &str, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let base = if text.trim() == "?" { String::new() } else { text.trim_end().to_string() };
        let next = if base.is_empty() { token.to_string() } else { format!("{base} {token}") };
        self.help_open = false;
        self.set_text(&next, cx);
        if let Some(at) = next.rfind(value) {
            self.input.update(cx, |i, cx| i.select(at..at + value.len(), cx));
        }
    }

    // ───────────────────────────── choosing ─────────────────────────────

    fn count(&self, cx: &Context<Self>) -> usize {
        if self.command_mode(cx) { self.matched.len() } else { self.sugg.len() + self.shown.len() }
    }

    fn hit(&self, ix: usize) -> Option<&Hit> {
        ix.checked_sub(self.sugg.len()).and_then(|i| self.shown.get(i)).map(|s| &s.hit)
    }

    /// The result the arrow keys are on.
    fn picked_hit(&self, cx: &Context<Self>) -> Option<&Hit> {
        if self.command_mode(cx) { None } else { self.hit(self.selected) }
    }

    /// The suggestion Tab takes: a completion, the picked one or else the first.
    fn tab_row(&self) -> Option<usize> {
        let completes = |r: &Row| matches!(r, Row::Suggest(s) if s.completes);
        match self.sugg.get(self.selected) {
            Some(r) if completes(r) => Some(self.selected),
            _ => self.sugg.iter().position(completes),
        }
    }

    fn open_as_tab(&mut self, cx: &mut Context<Self>) {
        let q = self.tab_query(cx);
        if q.is_empty() {
            return;
        }
        cx.emit(PaletteEvent::Searched(q.clone()));
        cx.emit(PaletteEvent::SearchTab(q));
    }

    fn ask_to_save(&mut self, query: String, back: String, cx: &mut Context<Self>) {
        cx.emit(PaletteEvent::Save { query, back });
    }

    fn forget(&mut self, q: String, cx: &mut Context<Self>) {
        self.history.retain(|h| !h.trim().eq_ignore_ascii_case(q.trim()));
        cx.emit(PaletteEvent::Forget(q));
        self.refresh(cx);
        cx.notify();
    }

    fn remove_saved(&mut self, q: String, cx: &mut Context<Self>) {
        self.saved.retain(|(_, s)| !s.trim().eq_ignore_ascii_case(q.trim()));
        cx.emit(PaletteEvent::RemoveSaved(q));
        self.refresh(cx);
        cx.notify();
    }

    /// Enter, a click: a row above the results does what it offers; a result opens (`tab`: its
    /// folder in a new tab; `reveal`: shown in its folder).
    fn activate(&mut self, ix: usize, tab: bool, reveal: bool, cx: &mut Context<Self>) {
        if self.command_mode(cx) {
            if let Some((i, _, _)) = self.matched.get(ix) {
                cx.emit(PaletteEvent::Command(*i));
            }
            return;
        }
        if ix < self.sugg.len() {
            match &self.sugg[ix] {
                Row::Suggest(s) => {
                    let a = s.apply.clone();
                    self.apply(a, cx);
                }
                Row::Saved(_, q) | Row::Recent(q) => {
                    let q = q.clone();
                    self.use_query(q, cx);
                }
                Row::OpenAll(_) => self.open_as_tab(cx),
            }
            return;
        }
        let Some(h) = self.hit(ix) else { return };
        let (path, is_dir) = (h.path.clone(), h.is_dir);
        cx.emit(PaletteEvent::Searched(self.tab_query(cx)));
        // Archives open as folders, as in the list.
        let archive = !is_dir && ferret_core::archive::archive_kind(&path).is_some() && !ferret_core::paths::is_remote(&path);
        if tab {
            cx.emit(PaletteEvent::OpenTab(if is_dir || archive { path } else { util::parent(&path).unwrap_or(path) }));
        } else if is_dir {
            cx.emit(PaletteEvent::Open { path, is_dir });
        } else if reveal {
            cx.emit(PaletteEvent::Reveal(path));
        } else {
            cx.emit(PaletteEvent::Open { path, is_dir });
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let q = self.full_query(cx);
        let keep = !self.command_mode(cx) && !q.is_empty() && q != "?";
        cx.emit(PaletteEvent::Dismiss(if keep { q } else { String::new() }));
    }

    /// Where row `ix` is among the list's children (headings sit between rows).
    fn child_index(&self, ix: usize, cx: &Context<Self>) -> usize {
        if self.command_mode(cx) {
            return ix;
        }
        (0..=ix).filter(|&i| self.heading(i, cx).is_some()).count() + ix
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        let child = self.child_index(ix, cx);
        self.scroll.scroll_to_item(child);
    }

    // ───────────────────────────── the buttons, by keyboard ─────────────────────────────

    /// The buttons Tab goes round after the box, in the app's order: those in the box's row
    /// (a disabled scope left out), the type chips, then the help's rows while it shows.
    fn focusable(&self, cx: &Context<Self>) -> Vec<Btn> {
        let commands = self.command_mode(cx);
        let mut out = Vec::new();
        if !commands {
            if self.type_chip.is_some() {
                out.push(Btn::Pill);
            }
            if !self.tab_query(cx).is_empty() {
                out.push(Btn::Save);
            }
            out.push(Btn::Help);
            if !self.own_folder(cx) && self.content_folder(cx).is_none() {
                out.push(Btn::Scope);
            }
        }
        out.push(Btn::Close);
        if !commands {
            out.extend((0..CHIPS.len()).map(Btn::Chip));
            if self.show_help(cx) {
                out.extend((0..1 + file_types().len()).map(Btn::HelpType));
                out.extend((0..FILTERS.len()).map(Btn::HelpFilter));
            }
        }
        out
    }

    /// The keyboard is on button `b` (`kbd` is only shown while the palette's own handle has it).
    fn focus_btn(&mut self, b: Btn, window: &mut Window, cx: &mut Context<Self>) {
        self.kbd = Some(b);
        self.reveal_kbd.set(matches!(b, Btn::HelpType(_) | Btn::HelpFilter(_)));
        if let Btn::Chip(i) = b {
            self.chips_scroll.scroll_to_item(i);
        }
        window.focus(&self.own_focus, cx);
        cx.notify();
    }

    /// The keyboard goes back to the box.
    fn focus_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kbd = None;
        let focus = self.input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// A button pressed, by a click or by Enter / Space on it. As the app, the keyboard is back
    /// in the box afterwards (the star and × close the palette).
    fn press(&mut self, b: Btn, window: &mut Window, cx: &mut Context<Self>) {
        match b {
            Btn::Pill => {
                self.type_chip = None;
                self.requery(cx);
            }
            Btn::Save => {
                let q = self.tab_query(cx);
                if !q.is_empty() {
                    let back = self.full_query(cx);
                    self.ask_to_save(q, back, cx);
                }
                return;
            }
            Btn::Help => {
                let open = self.show_help(cx);
                if self.text(cx).trim() == "?" {
                    self.set_text("", cx);
                }
                self.help_open = !open;
            }
            Btn::Scope => {
                if self.folder.is_some() && !self.own_folder(cx) && self.content_folder(cx).is_none() {
                    let on = !self.scoped;
                    self.set_scoped(on, cx);
                }
            }
            Btn::Close => {
                self.dismiss(cx);
                return;
            }
            Btn::Chip(i) => self.pick_type(CHIPS[i], cx),
            Btn::HelpType(i) => {
                let id = if i == 0 { folders().id } else { file_types().get(i - 1).map(|t| t.id.clone()).unwrap_or_default() };
                self.pick_type(Some(&id), cx);
            }
            Btn::HelpFilter(i) => {
                let (token, value, _) = FILTERS[i];
                self.insert_filter(token, value, cx);
            }
        }
        self.focus_input(window, cx);
    }

    /// Keys while the keyboard is on a button (the app's `onPanelKey`): Tab / Shift+Tab go round
    /// the buttons and the box, Enter or Space presses it, Esc goes back to the box, and the
    /// arrows go through the help's rows (past either end, to the box).
    fn button_key(&mut self, at: Btn, key: &str, m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "escape" => self.focus_input(window, cx),
            "tab" => {
                let all = self.focusable(cx);
                // 0 is the box, the buttons follow.
                let n = all.len() + 1;
                let i = all.iter().position(|b| *b == at).map_or(0, |p| p + 1);
                let j = if m.shift { (i + n - 1) % n } else { (i + 1) % n };
                match j {
                    0 => self.focus_input(window, cx),
                    _ => self.focus_btn(all[j - 1], window, cx),
                }
            }
            "up" | "down" if matches!(at, Btn::HelpType(_) | Btn::HelpFilter(_)) => {
                let rows: Vec<Btn> = self.focusable(cx).into_iter().filter(|b| matches!(b, Btn::HelpType(_) | Btn::HelpFilter(_))).collect();
                let i = rows.iter().position(|b| *b == at).unwrap_or(0);
                let next = if key == "down" { rows.get(i + 1) } else { i.checked_sub(1).and_then(|i| rows.get(i)) };
                match next {
                    Some(&b) => self.focus_btn(b, window, cx),
                    None => self.focus_input(window, cx),
                }
            }
            "enter" | "space" if !m.control && !m.alt => self.press(at, window, cx),
            _ => {}
        }
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        // On a button: its keys only (the rest do nothing there, as in the app).
        if let Some(at) = self.kbd {
            if self.own_focus.is_focused(window) {
                self.button_key(at, key, m, window, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            self.kbd = None;
        }
        let n = self.count(cx);
        let commands = self.command_mode(cx);
        let recent = match self.sugg.get(self.selected) {
            Some(Row::Recent(q)) if !commands => Some(q.clone()),
            _ => None,
        };
        match key {
            k if m.alt && !commands && k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => {
                self.pick_type(CHIPS[(k.as_bytes()[0] - b'0') as usize], cx);
            }
            "escape" => {
                if self.help_open {
                    self.help_open = false;
                } else {
                    self.dismiss(cx);
                }
            }
            // At the start of the box (the field passes it on): the type chip goes.
            "backspace" if self.type_chip.is_some() && self.input.read(cx).selection() == (0..0) => {
                self.type_chip = None;
                self.requery(cx);
            }
            "c" if m.control && m.shift => {
                if let Some(h) = self.picked_hit(cx) {
                    cx.emit(PaletteEvent::CopyPath(h.path.clone()));
                }
            }
            "f10" if m.shift => self.menu_for(self.selected, None, window, cx),
            "menu" | "contextmenu" | "apps" => self.menu_for(self.selected, None, window, cx),
            "s" if m.control && !commands => {
                // On a recent search, that one; otherwise what's in the box.
                if let Some(q) = recent {
                    self.ask_to_save(q, String::new(), cx);
                } else {
                    let q = self.tab_query(cx);
                    if !q.is_empty() {
                        let back = self.full_query(cx);
                        self.ask_to_save(q, back, cx);
                    }
                }
            }
            "/" if m.control && !commands => {
                let open = self.show_help(cx);
                if self.text(cx).trim() == "?" {
                    self.set_text("", cx);
                }
                self.help_open = !open;
            }
            // An empty box (the field passes it on) on a recent search: forget it.
            "delete" if self.text(cx).is_empty() && recent.is_some() => self.forget(recent.unwrap_or_default(), cx),
            "p" if m.control && m.shift => {
                let next = if commands { "" } else { ">" };
                self.set_text(next, cx);
            }
            // Tab is the box's: the buttons around it are reached backwards.
            "tab" if m.shift => {
                if let Some(&last) = self.focusable(cx).last() {
                    self.focus_btn(last, window, cx);
                }
            }
            // The help's rows are a list of buttons: down goes into them.
            "down" if !commands && self.show_help(cx) => self.focus_btn(Btn::HelpType(0), window, cx),
            "tab" if self.tab_row().is_some() => {
                if let Some(Row::Suggest(s)) = self.tab_row().and_then(|i| self.sugg.get(i)) {
                    let a = s.apply.clone();
                    self.apply(a, cx);
                }
            }
            "down" => {
                if n > 0 {
                    self.select((self.selected + 1) % n, cx);
                }
            }
            "up" => {
                if n > 0 {
                    self.select((self.selected + n - 1) % n, cx);
                }
            }
            "pagedown" if n > 0 => self.select((self.selected + 8).min(n - 1), cx),
            "pageup" => self.select(self.selected.saturating_sub(8), cx),
            "tab" => {
                if self.can_switch_scope(cx) {
                    let on = !self.scoped;
                    self.set_scoped(on, cx);
                }
            }
            "enter" if m.control && m.shift && !commands => self.open_as_tab(cx),
            "enter" => self.activate(self.selected, m.control, m.alt || m.shift, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn menu_for(&mut self, ix: usize, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = self.hit(ix) else { return };
        let at = at.unwrap_or_else(|| gpui::point(window.viewport_size().width / 2. - px(200.), px(150. + 36. * ix.min(10) as f32)));
        cx.emit(PaletteEvent::Menu { path: h.path.clone(), is_dir: h.is_dir, at });
    }

    /// A row as a screen reader says it.
    fn spoken(&self, ix: usize, commands: bool) -> String {
        if commands {
            return match self.matched.get(ix) {
                Some((i, _, _)) => {
                    let c = &self.commands[*i];
                    match c.keys {
                        Some(k) => format!("{}, {k}", c.label),
                        None => c.label.to_string(),
                    }
                }
                None => String::new(),
            };
        }
        if let Some(h) = self.hit(ix) {
            return spoken_hit(h);
        }
        match self.sugg.get(ix) {
            Some(Row::Recent(q)) => format!("Recent search: {q}"),
            Some(Row::Saved(n, q)) => format!("Saved search {n}: {q}"),
            Some(Row::Suggest(s)) => format!("{}{}", seg_text(&s.main), if s.completes { ", Tab to use" } else { "" }),
            Some(Row::OpenAll(label)) => format!("{label}, Ctrl+Shift+Enter"),
            None => String::new(),
        }
    }

    /// The section title above row `ix`.
    fn heading(&self, ix: usize, cx: &Context<Self>) -> Option<String> {
        if self.command_mode(cx) {
            return None;
        }
        if ix < self.sugg.len() {
            let prev = ix.checked_sub(1).and_then(|i| self.sugg.get(i));
            return match (&self.sugg[ix], prev) {
                (Row::Saved(..), None) => Some("Saved searches".into()),
                (Row::Recent(_), None | Some(Row::Saved(..))) => Some("Recent searches".into()),
                _ => None,
            };
        }
        let i = ix - self.sugg.len();
        // One ranked list (no "In <folder>" / "Everywhere" sections: see `ranked`), loose last.
        let s = self.shown.get(i)?;
        (s.loose && !self.shown.get(i.wrapping_sub(1)).is_some_and(|p| p.loose)).then(|| "Loose matches".into())
    }

    // ───────────────────────────── drawing ─────────────────────────────

    fn hit_row(&self, ix: usize, s: &Shown, name_w: f32, narrow: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hit = &s.hit;
        let active = ix == self.selected;
        let ext = if hit.is_dir { String::new() } else { hit.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default() };
        let kind = kinds::kind_of(hit.is_dir, &ext);
        // The letters that matched, in the accent and bold (offsets are UTF-16). A loose hit's are
        // scattered, and a name that matches whole has nothing to pick out.
        let mark = HighlightStyle {
            color: Some(if active { t::mix(t::accent_hex(), if t::is_dark() { 0xffffff } else { 0x1e0a19 }, 0.25) } else { t::accent_text() }),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let mut marks = Vec::new();
        let units: usize = hit.name.encode_utf16().count();
        if !s.loose && hit.matches.len() < units {
            let mut utf16 = 0u32;
            for (i, ch) in hit.name.char_indices() {
                if hit.matches.contains(&utf16) {
                    marks.push((i..i + ch.len_utf8(), mark));
                }
                utf16 += ch.len_utf16() as u32;
            }
        }
        let folder = util::parent(&hit.path).unwrap_or_default();
        let (place, rest) = location(&folder);
        let size = match (hit.is_dir, hit.size) {
            (false, Some(s)) => kinds::format_size(s),
            _ => String::new(),
        };
        let date = kinds::format_date(hit.modified).get(..10).unwrap_or("").to_string();
        let (path, is_dir) = (hit.path.clone(), hit.is_dir);
        let p2 = path.clone();
        let line = hit.line.as_ref().map(|l| {
            // The line with the match marked (UTF-16 offsets), and how many more there are.
            let (mut a, mut b) = (l.text.len(), l.text.len());
            let mut u = 0u32;
            for (i, ch) in l.text.char_indices() {
                if u == l.start {
                    a = i;
                }
                if u == l.end {
                    b = i;
                }
                u += ch.len_utf16() as u32;
            }
            let hl = vec![(
                a.min(b)..b,
                HighlightStyle {
                    background_color: Some(if active { t::accent() } else { t::accent_soft() }),
                    color: Some(if active { t::on_accent() } else { t::fg() }),
                    ..Default::default()
                },
            )];
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .pl(px(30.))
                .text_size(px(11.5))
                .text_color(t::fg3())
                .child(div().flex_none().min_w(px(26.)).text_right().child(l.line.to_string()))
                .child(div().flex_1().min_w_0().truncate().font_family(t::MONO).text_color(t::fg2()).child(StyledText::new(l.text.clone()).with_highlights(hl)))
                .when(l.count > 1, |d| {
                    d.child(
                        div()
                            .id(("more-lines", ix))
                            .flex_none()
                            .px(px(5.))
                            .rounded(px(4.))
                            .bg(t::hover())
                            .tooltip(ui::tip(format!("{} matches in this file", kinds::group_digits(l.count as usize))))
                            .child(format!("+{}", kinds::group_digits(l.count as usize - 1))),
                    )
                })
        });
        let has_line = line.is_some();
        div()
            .id(("hit", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(spoken_hit(hit))
            .aria_selected(active)
            .group("hit")
            .flex_none()
            // Two lines in a narrow window (min 42 px, 4 px above and below), as the app's.
            .when(!has_line && !narrow, |d| d.h(px(36.)))
            .when(!has_line && narrow, |d| d.min_h(px(42.)).py(px(4.)))
            .when(has_line, |d| d.min_h(px(36.)).py(px(6.)).gap(px(2.)))
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .flex_col()
            .justify_center()
            .text_color(if active { t::fg() } else { t::fg2() })
            .when(active, |d| d.bg(t::accent_soft()))
            .tooltip(ui::tip(path.clone()))
            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                this.press = None;
                if std::mem::take(&mut this.dragged) {
                    return;
                }
                this.selected = ix;
                let m = ev.modifiers();
                this.activate(ix, m.control, m.alt, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, _| {
                this.press = Some((ix, ev.position));
                this.dragged = false;
            }))
            .on_mouse_move(cx.listener({
                let p = p2.clone();
                move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                    if this.selected != ix && ev.pressed_button.is_none() {
                        this.selected = ix;
                        cx.notify();
                    }
                    let Some((at, from)) = this.press else { return };
                    if at != ix || ev.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    let d = ev.position - from;
                    if d.x.as_f32().hypot(d.y.as_f32()) > 5. {
                        this.press = None;
                        this.dragged = true;
                        cx.emit(PaletteEvent::Searched(this.tab_query(cx)));
                        cx.emit(PaletteEvent::Drag(p.clone()));
                    }
                }
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, _, cx| this.activate(ix, true, false, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                this.selected = ix;
                this.menu_for(ix, Some(ev.position), window, cx);
            }))
            .child({
                let icon_col = div().w(px(20.)).flex_none().flex().justify_center().child(icon(kinds::icon(kind), 16., kinds::color(kind)));
                let name = div().overflow_hidden().whitespace_nowrap().text_ellipsis_middle().text_color(t::fg()).child(StyledText::new(hit.name.clone()).with_highlights(marks));
                let folder = div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(place)
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().text_size(px(12.)).text_color(t::fg2()).child(rest));
                let meta = |w: Option<f32>, text: String| {
                    div()
                        .flex_none()
                        .when_some(w, |d, w| d.w(px(w)))
                        .text_right()
                        .whitespace_nowrap()
                        .text_size(px(11.5))
                        .text_color(if active { t::fg2() } else { t::fg3() })
                        .child(text)
                };
                let reveal = div()
                    .id(("reveal", ix))
                    .role(gpui::Role::Button)
                    .aria_label(if is_dir { "Open folder" } else { "Show in folder" })
                    .group("reveal")
                    .flex_none()
                    .w(px(26.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(t::hover())
                    .border_1()
                    .border_color(t::line())
                    .when(!active, |d| d.invisible())
                    .hover(|s| s.bg(t::accent()).border_color(t::accent()))
                    .tooltip(ui::tip(if is_dir { "Open folder" } else { "Show in folder (Alt+Enter)" }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.activate(ix, false, true, cx);
                    }))
                    .child(ui::hover_icon("folder-open", 14., t::fg2(), t::on_accent(), "reveal"));
                let row = div().flex().items_center().gap(px(10.)).child(icon_col);
                if narrow {
                    // palette.css's narrow rows: the name over its folder, then the size; no date.
                    row.child(div().flex_1().min_w_0().flex().flex_col().gap(px(if has_line { 2. } else { 1. })).child(name).child(folder))
                        .child(meta(None, size))
                        .child(reveal)
                } else {
                    row.child(name.flex_none().w(px(name_w)))
                        .child(folder.flex_1())
                        .child(meta(Some(58.), size))
                        .child(meta(Some(70.), date))
                        .child(reveal)
                }
            })
            .children(line)
            .into_any_element()
    }

    /// A row above the results: a suggestion, a saved or recent search, "Open all in a tab".
    fn other_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = ix == self.selected;
        let tab = self.tab_row() == Some(ix);
        let (icon_name, main, detail): (&str, Vec<Seg>, String) = match row {
            Row::Suggest(s) => (s.icon, seg_clone(&s.main), s.detail.clone()),
            Row::Saved(n, q) => {
                let mut main = vec![Seg::Label(n.clone())];
                if n != q {
                    main.extend([Seg::Text("  ".into()), Seg::Code(q.clone())]);
                }
                ("star", main, String::new())
            }
            Row::Recent(q) => ("rotate-ccw-clock", vec![Seg::Query(q.clone())], String::new()),
            Row::OpenAll(label) => ("folder-search", vec![Seg::Text(label.clone())], "Ctrl+Shift+Enter".into()),
        };
        // Small buttons at the row's end.
        let actions: Vec<(&'static str, &'static str, u8)> = match row {
            Row::Saved(..) => vec![("folder-search", "Open as a tab", 0), ("x", "Remove from the sidebar", 1)],
            Row::Recent(_) => vec![("star", "Save to the sidebar (Ctrl+S)", 2), ("x", "Forget this search (Delete)", 3)],
            _ => vec![],
        };
        let query = match row {
            Row::Saved(_, q) | Row::Recent(q) => q.clone(),
            _ => String::new(),
        };
        div()
            .id(("other", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(self.spoken(ix, false))
            .aria_selected(active)
            .group("sugg")
            .flex_none()
            .h(px(36.))
            .mb(px(2.))
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(px(13.))
            .text_color(t::fg())
            .when(active, |d| d.bg(t::accent_soft()))
            .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                if this.selected != ix && ev.pressed_button.is_none() {
                    this.selected = ix;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = ix;
                this.activate(ix, false, false, cx);
            }))
            .child(icon(icon_name, 15., t::accent_text()))
            .child(div().flex_none().max_w(gpui::relative(0.62)).overflow_hidden().whitespace_nowrap().flex().items_center().children(main.into_iter().map(seg_el)))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(if active { t::fg2() } else { t::fg3() }).child(detail))
            .children(actions.into_iter().map(|(icon_name, title, what)| {
                let q = query.clone();
                div()
                    .id(SharedString::from(format!("act-{ix}-{what}")))
                    .role(gpui::Role::Button)
                    .aria_label(title)
                    .group("act")
                    .flex_none()
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .when(!active, |d| d.invisible().group_hover("sugg", |s| s.visible()))
                    .hover(|s| s.bg(t::hover()))
                    .tooltip(ui::tip(title))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        match what {
                            0 => cx.emit(PaletteEvent::SearchTab(q.clone())),
                            1 => this.remove_saved(q.clone(), cx),
                            2 => this.ask_to_save(q.clone(), String::new(), cx),
                            _ => this.forget(q.clone(), cx),
                        }
                    }))
                    .child(ui::hover_icon(icon_name, 13., t::fg3(), t::fg(), "act"))
            }))
            .when(tab, |d| d.child(ui::kbd("Tab")))
            .into_any_element()
    }

    fn command_row(&self, ix: usize, (i, _, hits): &(usize, i32, Vec<usize>), cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = ix == self.selected;
        let c = &self.commands[*i];
        let mark = HighlightStyle { color: Some(t::accent_text()), font_weight: Some(FontWeight::BOLD), ..Default::default() };
        let marks: Vec<_> = hits.iter().map(|&b| (b..b + c.label[b..].chars().next().map_or(1, char::len_utf8), mark)).collect();
        div()
            .id(("cmd", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(match c.keys {
                Some(k) => format!("{}, {k}", c.label),
                None => c.label.to_string(),
            })
            .aria_selected(active)
            .flex_none()
            .h(px(36.))
            .px(px(10.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .text_color(if active { t::fg() } else { t::fg2() })
            .when(active, |d| d.bg(t::accent_soft()))
            .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                if this.selected != ix && ev.pressed_button.is_none() {
                    this.selected = ix;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = ix;
                this.activate(ix, false, false, cx);
            }))
            .child(div().w(px(20.)).flex_none().flex().justify_center().child(icon("command", 14., t::fg3())))
            .child(div().flex_none().text_color(t::fg()).child(StyledText::new(c.label.clone()).with_highlights(marks)))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().text_size(px(12.)).text_color(t::fg3()).children(c.hint.clone()))
            .children(c.keys.map(ui::kbd))
            .into_any_element()
    }

    /// `?`: every type and filter, each one click away.
    /// The keyboard's ring on a help row, which also scrolls the row into view the first time
    /// it's drawn there (the app's `focus()` does).
    fn help_ring(&self) -> gpui::Div {
        let (reveal, scroll) = (self.reveal_kbd.clone(), self.scroll.clone());
        focus_ring(7.).child(
            gpui::canvas(
                move |b, window, _| {
                    if !reveal.replace(false) {
                        return;
                    }
                    let view = scroll.bounds();
                    let mut off = scroll.offset();
                    if b.top() < view.top() {
                        off.y += view.top() - b.top();
                    } else if b.bottom() > view.bottom() {
                        off.y -= b.bottom() - view.bottom();
                    } else {
                        return;
                    }
                    scroll.set_offset(off);
                    window.on_next_frame(|window, _| window.refresh());
                },
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }

    fn help_view(&self, kbd: Option<Btn>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let title = |s: &str| div().px(px(8.)).pt(px(8.)).pb(px(4.)).child(ui::heading(s.to_string()));
        let mut types = vec![folders()];
        types.extend(file_types());
        let type_rows = types.into_iter().enumerate().map(|(i, ty)| {
            let tip = {
                let also: Vec<&String> = ty.words.iter().filter(|w| **w != ty.id).collect();
                let mut lines = Vec::new();
                if ty.words.len() > 1 {
                    lines.push(format!("Also: {}", also.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
                }
                if !ty.exts.is_empty() {
                    lines.push(ty.exts.join(" "));
                }
                lines.join("\n")
            };
            div()
                .id(("help-t", i))
                .role(gpui::Role::Button)
                .aria_label(format!("{}, type:{}", ty.label, ty.id))
                .group("help-row")
                .relative()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(30.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(7.))
                .text_size(px(13.))
                .text_color(t::fg2())
                .hover(|s| s.bg(t::accent_soft()).text_color(t::fg()))
                .when(!tip.is_empty(), |d| d.tooltip(ui::tip(tip)))
                .on_click(cx.listener(move |this, _, window, cx| this.press(Btn::HelpType(i), window, cx)))
                .child(icon(type_icon(&ty.id), 14., t::fg3()))
                .child(div().w(px(100.)).flex_none().truncate().text_color(t::fg()).child(ty.label.clone()))
                .child(div().w(px(96.)).flex_none().child(code(format!("type:{}", ty.id))))
                .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(t::fg3()).child(ext_preview(&ty, 4)))
                .when(kbd == Some(Btn::HelpType(i)), |d| d.child(self.help_ring()))
        });
        let filter_rows = FILTERS.iter().enumerate().map(|(i, (token, _, what))| {
            let shown = token.to_string();
            div()
                .id(("help-f", i))
                .role(gpui::Role::Button)
                .aria_label(format!("{token}: {what}"))
                .relative()
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(7.))
                .text_color(t::fg2())
                .hover(|s| s.bg(t::accent_soft()).text_color(t::fg()))
                .on_click(cx.listener(move |this, _, window, cx| this.press(Btn::HelpFilter(i), window, cx)))
                .child(div().flex_none().child(code(shown)))
                .child(div().flex_1().min_w_0().text_size(px(11.5)).line_height(px(16.)).text_color(t::fg3()).child(*what))
                .when(kbd == Some(Btn::HelpFilter(i)), |d| d.child(self.help_ring()))
        });
        let keys = div()
            .px(px(8.))
            .pt(px(10.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(11.5))
            .text_color(t::fg3())
            .child(div().flex().items_center().gap(px(6.)).child("Filters combine:").child(code("report type:doc modified:<30d".into())))
            .child("Alt+0…9 picks a type · Tab switches this folder or everywhere")
            .child("Ctrl+S saves · Ctrl+Shift+C copies the path · Shift+F10 opens the menu")
            .child(div().flex().items_center().gap(px(6.)).child("Shift+Tab reaches the buttons ·").child(code(">".into())).child("for commands"));
        div()
            .flex()
            .gap(px(16.))
            .px(px(4.))
            .pt(px(2.))
            .pb(px(8.))
            .child(div().flex_1().min_w_0().flex().flex_col().child(title("File types")).children(type_rows))
            .child(div().w(gpui::relative(0.45)).flex_none().flex().flex_col().child(title("Filters")).children(filter_rows).child(keys))
            .into_any_element()
    }

    /// What the list says when it has no rows.
    fn empty_message(&self, cx: &Context<Self>) -> Vec<Seg> {
        let full = self.full_query(cx);
        if full.is_empty() {
            let indexed: u64 = self.index.as_ref().map(|s| s.volumes.iter().map(|v| v.entries).sum()).unwrap_or(0);
            return vec![
                Seg::Text(format!(
                    "Type to search {}, or pick a type above. ",
                    if indexed > 0 { format!("{} indexed items", kinds::group_digits(indexed as usize)) } else { "your drives".into() }
                )),
                Seg::Code("?".into()),
                Seg::Text(" shows every filter · ".into()),
                Seg::Code(">".into()),
                Seg::Text(" for commands".into()),
            ];
        }
        let folder_name = |p: Option<String>| p.as_deref().map(util::folder_name).filter(|n| !n.is_empty());
        if self.content_search(cx) {
            let dir = self.search_folder(cx);
            let text = if self.searching {
                format!("Reading the files in {}…", folder_name(dir).unwrap_or_else(|| "this folder".into()))
            } else if dir.is_none() && !self.own_folder(cx) {
                return vec![Seg::Text("Searching inside files needs a folder: open one, or add ".into()), Seg::Code("in:C:\\folder".into())];
            } else if self.res.as_ref().is_some_and(|r| r.source == "content-unsupported") {
                "This server only allows file access (SFTP), so Ferret can't search inside its files".into()
            } else {
                let name = if self.own_folder(cx) { in_folder_name(&full) } else { folder_name(dir) };
                format!("No file in {} contains {}", name.unwrap_or_else(|| "the folder".into()), quoted(&content_text(&full)))
            };
            return vec![Seg::Text(text)];
        }
        if let Some(e) = &self.failed {
            return vec![Seg::Text(format!("Search failed: {e}"))];
        }
        if let Some(m) = &self.missing {
            return vec![Seg::Text(format!("Folder not found, or it can't be opened: {m}"))];
        }
        // No verdict until there's an answer: most searches take milliseconds.
        if !self.settled(cx) {
            return vec![Seg::Text(if self.slow { "Searching…".into() } else { String::new() })];
        }
        if self.index.as_ref().is_some_and(|s| s.building) {
            return vec![Seg::Text("Still indexing, so more results will appear…".into())];
        }
        let where_ = if self.own_folder(cx) {
            in_folder_name(&full).map(|n| format!(" in {n}")).unwrap_or_default()
        } else if self.scoped {
            format!(" in {}", self.folder.as_deref().map(util::folder_name).unwrap_or_default())
        } else {
            String::new()
        };
        let ty = self.type_chip.as_deref().and_then(find_type).map(|t| t.label);
        let q = self.query(cx);
        if q.is_empty() {
            if let Some(ty) = &ty {
                return vec![Seg::Text(format!("No {} found{where_}", files_word(ty)))];
            }
        }
        // The filters are in the box and on the chips: the sentence only needs the words.
        let words = without_filters(&q);
        vec![Seg::Text(format!(
            "Nothing matches {}{}{where_}",
            if words.is_empty() { "these filters".into() } else { quoted(&words) },
            ty.map(|t| format!(" in {}", files_word(&t))).unwrap_or_default()
        ))]
    }

    /// What's in the box can't be judged yet: a filter with no value, or only a value search ignores.
    fn no_verdict(&self, cx: &Context<Self>) -> bool {
        let q = self.query(cx);
        self.shown.is_empty() && (awaiting_value(&self.full_query(cx)) || (bad_filter(&q).is_some() && without_filters(&q).is_empty()))
    }

    fn footer_main(&self, cx: &Context<Self>) -> gpui::AnyElement {
        if self.command_mode(cx) {
            return div().child("↑↓ choose · Enter run · Esc close").into_any_element();
        }
        if self.slow {
            return div().child("Searching…").into_any_element();
        }
        match &self.res {
            Some(r) if !self.no_verdict(cx) => {
                let took = if r.took_us < 10_000 { format!("{:.1} ms", r.took_us as f64 / 1000.) } else { format!("{:.0} ms", r.took_us as f64 / 1000.) };
                let text = if r.source == "content" {
                    format!(
                        "{}{} {} it · {} files read · {took}",
                        kinds::group_digits(r.total as usize),
                        if r.more { "+" } else { "" },
                        if r.total == 1 && !r.more { "file has" } else { "files have" },
                        kinds::group_digits(r.scanned as usize)
                    )
                } else {
                    format!(
                        "{} · {took}{}",
                        if r.total == 1 { "1 match".to_string() } else { format!("{} matches", kinds::group_digits(r.total as usize)) },
                        if r.partial { " · still indexing" } else { "" }
                    )
                };
                div().child(text).into_any_element()
            }
            _ => div()
                .flex()
                .child("↑↓ choose · Enter open · ")
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t::fg2()).child("Alt+Enter show in folder"))
                .child(format!(" · Ctrl+Enter new tab · ? filters{}", if self.can_switch_scope(cx) { " · Tab scope" } else { "" }))
                .into_any_element(),
        }
    }

    /// The drive index: building, offered, or on.
    fn footer_status(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let s = self.index.as_ref()?;
        let progress = if s.volumes.is_empty() { 0. } else { s.volumes.iter().map(|v| v.progress).sum::<f32>() / s.volumes.len() as f32 };
        let install = |label: &'static str, tip: &'static str, cx: &mut Context<Self>| {
            div()
                .id(label)
                .role(gpui::Role::Button)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(6.))
                .text_color(t::accent_text())
                .hover(|s| s.bg(t::accent_soft()))
                .tooltip(ui::tip(tip))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::InstallIndex)))
                .child(icon("shield-check", 12., t::accent_text()))
                .child(label)
                .into_any_element()
        };
        let mut parts: Vec<gpui::AnyElement> = Vec::new();
        if s.building {
            parts.push(ui::spinner(12., t::fg3()).into_any_element());
            parts.push(div().child(format!("Indexing {:.0}%", progress * 100.)).into_any_element());
        }
        if !s.service_installed && !s.building {
            parts.push(install(
                "Enable instant drive index",
                "Reads the NTFS file table for instant, always-current results across all drives. Needs one admin approval.",
                cx,
            ));
        }
        if s.service_running {
            if s.service_outdated {
                parts.push(install(
                    "Update drive index",
                    "This version of Ferret ships a newer drive index service. It updates itself; one installed by an older Ferret needs one admin approval, once.",
                    cx,
                ));
            } else {
                parts.push(icon("shield-check", 12., t::fg3()).into_any_element());
                parts.push(div().child("Drive index").into_any_element());
            }
        }
        Some(div().flex_none().flex().items_center().gap(px(6.)).children(parts).into_any_element())
    }

    /// For tests: what the palette shows.
    pub fn state(&self, cx: &gpui::App) -> serde_json::Value {
        let text = self.input.read(cx).content.to_string();
        serde_json::json!({
            "text": text,
            "chip": self.type_chip,
            "scoped": self.scoped,
            "help": self.help_open || text.trim() == "?",
            // The button the keyboard is on (Shift+Tab), or null for the box.
            "kbd": self.kbd.map(Btn::name),
            "windowWidth": self.drawn_w.get(),
            "narrow": self.drawn_w.get() <= NARROW,
            "selected": self.selected,
            "rows": self.sugg.iter().map(|r| match r {
                Row::Suggest(s) => format!("suggest: {}", seg_text(&s.main)),
                Row::Saved(n, q) => format!("saved: {n} = {q}"),
                Row::Recent(q) => format!("recent: {q}"),
                Row::OpenAll(l) => format!("all: {l}"),
            }).collect::<Vec<_>>(),
            "results": self.shown.iter().take(12).map(|s| format!("{}{}{}", if s.loose { "loose " } else { "" }, s.hit.path, s.hit.line.as_ref().map(|l| format!(" @{}", l.line)).unwrap_or_default())).collect::<Vec<_>>(),
            "total": self.res.as_ref().map(|r| r.total),
            "failed": self.failed,
            "missing": self.missing,
            "commands": self.matched.len(),
        })
    }
}

/// Where a folder lives: a WSL distro (and its Linux path), a server (its name, the path there),
/// a drive (the letter, the rest of the path), or elsewhere (a network share).
fn location(dir: &str) -> (Option<gpui::AnyElement>, String) {
    if let Some(distro) = ferret_core::paths::wsl_distro(dir) {
        let root = ferret_core::paths::wsl_root(&distro);
        let rest = dir.get(root.len()..).unwrap_or("").replace('\\', "/");
        let rest = rest.trim_end_matches('/');
        let rest = if rest.is_empty() { "/".to_string() } else { rest.to_string() };
        return (Some(place_badge("square-terminal", distro.clone(), format!("In WSL ({distro})"))), rest);
    }
    if let Some((id, linux)) = ferret_core::paths::remote_parts(dir) {
        let name = ferret_core::remote::name_of(id);
        return (Some(place_badge("server", name.clone(), format!("On {name}"))), linux);
    }
    let b = dir.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let letter = (b[0] as char).to_ascii_uppercase();
        let rest = dir.get(2..).unwrap_or("").trim_start_matches('\\').trim_end_matches('\\').to_string();
        let badge = div()
            .id(SharedString::from(format!("drive-{letter}")))
            .flex_none()
            .tooltip(ui::tip(format!("On drive {letter}:")))
            .child(ui::drive_badge(letter, false))
            .into_any_element();
        return (Some(badge), rest);
    }
    (None, dir.to_string())
}

/// The WSL / server badge in a result (`.loc-wsl`).
fn place_badge(icon_name: &str, label: String, tip: String) -> gpui::AnyElement {
    let (fg, bg, line) = if t::is_dark() {
        (t::rgba(0x7ee0a5, 1.), t::rgba(0x7ee0a5, 0.12), t::rgba(0x7ee0a5, 0.35))
    } else {
        (t::rgba(0x15803d, 1.), t::rgba(0x15803d, 0.08), t::rgba(0x15803d, 0.3))
    };
    div()
        .id(SharedString::from(format!("loc-{label}")))
        .flex_none()
        .h(px(18.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(5.))
        .bg(bg)
        .border_1()
        .border_color(line)
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(fg)
        .tooltip(ui::tip(tip))
        .child(icon(icon_name, 11., fg))
        .child(label)
        .into_any_element()
}

/// Filter syntax in a small chip.
fn code(text: String) -> gpui::Div {
    div().flex_none().px(px(6.)).py(px(1.)).rounded(px(5.)).bg(t::hover()).font_family(t::MONO).text_size(px(11.5)).text_color(t::fg2()).child(text)
}

fn seg_el(s: Seg) -> gpui::AnyElement {
    match s {
        Seg::Text(x) => div().flex_none().whitespace_nowrap().child(x).into_any_element(),
        Seg::Label(x) => div().flex_none().whitespace_nowrap().text_color(t::fg()).child(x).into_any_element(),
        Seg::Code(x) => code(x).into_any_element(),
        Seg::Bold(x) => div().flex_none().whitespace_nowrap().font_weight(FontWeight::SEMIBOLD).text_color(t::accent_text()).child(x).into_any_element(),
        Seg::Query(x) => div().flex_none().whitespace_nowrap().font_family(t::MONO).text_size(px(12.)).text_color(t::fg()).child(x).into_any_element(),
    }
}

fn seg_text(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Text(x) | Seg::Label(x) | Seg::Code(x) | Seg::Bold(x) | Seg::Query(x) => x.as_str(),
        })
        .collect()
}

fn seg_clone(segs: &[Seg]) -> Vec<Seg> {
    segs.iter()
        .map(|s| match s {
            Seg::Text(x) => Seg::Text(x.clone()),
            Seg::Label(x) => Seg::Label(x.clone()),
            Seg::Code(x) => Seg::Code(x.clone()),
            Seg::Bold(x) => Seg::Bold(x.clone()),
            Seg::Query(x) => Seg::Query(x.clone()),
        })
        .collect()
}

/// One width for every result's name, so the paths line up: the middle one of the bigger names
/// (estimated from their length), between 120 and 260 px.
fn name_column(names: &[&str]) -> f32 {
    if names.is_empty() {
        return 120.;
    }
    let mut w: Vec<f32> = names.iter().map(|n| n.chars().count() as f32 * 7.3 + 2.).collect();
    w.sort_by(|a, b| a.total_cmp(b));
    w[((w.len() - 1) as f32 * 0.75) as usize].clamp(120., 260.)
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let commands = self.command_mode(cx);
        let help = self.show_help(cx);
        let (vw, vh) = (window.viewport_size().width.as_f32(), window.viewport_size().height.as_f32());
        let narrow = vw <= NARROW;
        self.drawn_w.set(vw);
        // The button the keyboard is on (only while it is: a click puts it back in the box).
        let kbd = self.kbd.filter(|_| self.own_focus.is_focused(window));
        let mut list: Vec<gpui::AnyElement> = Vec::new();
        // Where "nothing found" (or the idle hint) goes: under the suggestions, as the app's.
        let mut after_sugg = 0;
        if commands {
            list = (0..self.matched.len()).map(|i| self.command_row(i, &self.matched[i], cx)).collect();
        } else if help {
            list.push(self.help_view(kbd, cx));
        } else {
            let heading = |title: String| div().px(px(14.)).pt(px(10.)).pb(px(4.)).child(ui::heading(title)).into_any_element();
            let n = self.sugg.len();
            for i in 0..n {
                if let Some(h) = self.heading(i, cx) {
                    list.push(heading(h));
                }
                list.push(self.other_row(i, &self.sugg[i], cx));
            }
            after_sugg = list.len();
            let name_w = name_column(&self.shown.iter().map(|s| s.hit.name.as_str()).collect::<Vec<_>>());
            for j in 0..self.shown.len() {
                let ix = n + j;
                if let Some(h) = self.heading(ix, cx) {
                    list.push(heading(h));
                }
                list.push(self.hit_row(ix, &self.shown[j], name_w, narrow, cx));
            }
            // The list stops at the limit: say so, and offer the rest.
            if let Some(r) = self.res.as_ref().filter(|r| !r.more && r.total > r.len as u64 && !self.shown.is_empty()) {
                list.push(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .mt(px(4.))
                        .px(px(10.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(t::line())
                        .text_size(px(12.))
                        .text_color(t::fg3())
                        .child(format!("Showing the top {} of {} matches", r.len, kinds::group_digits(r.total as usize)))
                        .child(ui::button("p-tab", "Open in a tab", false).h(px(28.)).tooltip(ui::tip("Ctrl+Shift+Enter")).on_click(cx.listener(|this, _, _, cx| this.open_as_tab(cx))))
                        .into_any_element(),
                );
            }
        }
        let empty = if commands { self.matched.is_empty() } else { !help && self.shown.is_empty() };
        let query = self.full_query(cx);
        let hidden_empty = !commands && self.no_verdict(cx);
        let empty_el = if !empty || hidden_empty {
            None
        } else if commands {
            Some(div().px(px(16.)).py(px(22.)).text_size(px(13.)).text_color(t::fg3()).child("No matching command").into_any_element())
        } else {
            let msg = self.empty_message(cx);
            let settled = self.settled(cx);
            let mut actions: Vec<gpui::AnyElement> = Vec::new();
            if !query.is_empty() && settled && self.failed.is_none() && !self.content_search(cx) && !self.index.as_ref().is_some_and(|s| s.building) {
                if self.scoped {
                    actions.push(ui::button("p-everywhere", "Search everywhere", false).on_click(cx.listener(|this, _, _, cx| this.set_scoped(false, cx))).into_any_element());
                }
                if self.type_chip.is_some() {
                    actions.push(ui::button("p-alltypes", "All types", false).on_click(cx.listener(|this, _, _, cx| this.pick_type(None, cx))).into_any_element());
                }
                let text = self.text(cx);
                let bare = without_filters(&text);
                if bare != text.trim() && !bare.is_empty() {
                    actions.push(ui::button("p-nofilters", "Remove filters", false).on_click(cx.listener(move |this, _, _, cx| this.set_text(&bare, cx))).into_any_element());
                }
            }
            Some(
                div()
                    .px(px(16.))
                    .py(px(22.))
                    .flex()
                    .flex_col()
                    .text_size(px(13.))
                    .line_height(px(22.))
                    .text_color(t::fg3())
                    .child(div().flex().flex_wrap().items_center().children(msg.into_iter().map(seg_el)))
                    .when(!actions.is_empty(), |d| d.child(div().mt(px(10.)).flex().flex_wrap().gap(px(6.)).children(actions)))
                    .into_any_element(),
            )
        };
        if let Some(el) = empty_el {
            list.insert(after_sugg.min(list.len()), el);
        }
        let chip = |i: usize, id: Option<&'static str>, on: bool, cx: &mut Context<Self>| {
            let ty = id.and_then(find_type);
            let label = ty.as_ref().map(|t| t.label.clone()).unwrap_or_else(|| "All".into());
            let tip = match &ty {
                Some(t) if !t.exts.is_empty() => format!("{}: {} (Alt+{i})", t.label, ext_preview(t, 8)),
                Some(t) => format!("{} (Alt+{i})", t.label),
                None => format!("All types (Alt+{i})"),
            };
            div()
                .id(("type", i))
                .role(gpui::Role::RadioButton)
                .aria_label(label.clone())
                .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
                .group("chip")
                .relative()
                .flex_none()
                .h(px(24.))
                .px(px(9.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(12.))
                .text_size(px(12.))
                .border_1()
                .text_color(if on { t::accent_text() } else { t::fg2() })
                .border_color(if on { t::accent_line() } else { gpui::transparent_black() })
                .when(on, |d| d.bg(t::accent_softer()))
                .when(!on, |d| d.hover(|s| s.bg(t::hover()).text_color(t::fg())))
                .tooltip(ui::tip(tip))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.press(Btn::Chip(i), window, cx)))
                .children(id.map(|id| if on { icon(type_icon(id), 13., t::accent_text()) } else { ui::hover_icon(type_icon(id), 13., t::fg2(), t::fg(), "chip") }))
                .child(label)
                .when(kbd == Some(Btn::Chip(i)), |d| d.child(focus_ring(12.)))
        };
        let chips: Vec<gpui::AnyElement> = CHIPS.iter().enumerate().map(|(i, id)| {
            let on = self.type_active(*id, cx);
            chip(i, *id, on, cx).into_any_element()
        }).collect();
        // The scope: an `in:` names the folder, a search inside files reads the open one.
        let own = self.own_folder(cx);
        let content_dir = self.content_folder(cx).is_some();
        let scope_on = self.scoped || own || content_dir;
        let scope_fixed = own || content_dir;
        let (scope_icon, scope_label) = if own {
            ("folder-search", format!("in {}", in_folder_name(&query).unwrap_or_default()))
        } else if self.scoped || content_dir {
            ("folder-search", "This folder".to_string())
        } else {
            ("hard-drive", "Everywhere".to_string())
        };
        let scope_tip = if own {
            "The in: filter picks the folder"
        } else if content_dir {
            "Searches inside files read this folder (or the one named with in:)"
        } else if self.scoped {
            "Search everywhere (Tab)"
        } else {
            // Everywhere is one list: this folder's matches only come by themselves from here.
            "Search only this folder (Tab)"
        };
        let tab_q = self.tab_query(cx);
        let saved = !tab_q.is_empty() && self.saved.iter().any(|(_, q)| q.trim().eq_ignore_ascii_case(tab_q.trim()));
        let picked = self.picked_hit(cx).map(|h| ferret_core::paths::remote_parts(&h.path).map(|(_, l)| l).unwrap_or_else(|| h.path.clone()));
        let announce = if commands || help {
            if commands { self.spoken(self.selected, true) } else { String::new() }
        } else if self.settled(cx) && self.shown.is_empty() && self.sugg.is_empty() && !self.no_verdict(cx) {
            if let Some(e) = &self.failed {
                format!("Search failed: {e}")
            } else if let Some(m) = &self.missing {
                format!("Folder not found: {m}")
            } else if query.is_empty() {
                String::new()
            } else {
                "No matches".into()
            }
        } else {
            self.spoken(self.selected, false)
        };
        let small_btn = |id: &'static str, icon_name: &'static str, on: bool, tip: String, ringed: bool| {
            div()
                .id(id)
                .role(gpui::Role::Button)
                .aria_label(tip.clone())
                .group(id)
                .relative()
                .flex_none()
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .when(on, |d| d.bg(t::accent_softer()))
                .hover(|s| s.bg(t::hover()))
                .tooltip(ui::tip(tip))
                .child(if on { icon(icon_name, 15., t::accent_text()) } else { ui::hover_icon(icon_name, 15., t::fg3(), t::fg(), id) })
                .when(ringed, |d| d.child(focus_ring(6.)))
        };
        // The palette's own focus target, for while the keyboard is on a button: nothing to see
        // or click (a focusable element under the pointer would take the focus on a press).
        let focus_target = div().absolute().size_0().track_focus(&self.own_focus);
        let card = div()
            .id("palette")
            .role(gpui::Role::Dialog)
            .aria_label(if commands { "Command palette" } else { "Search" })
            .child(ui::live("palette-live", announce, false))
            // As the app: 9 % from the top, 760 wide and 72 % of the window high at most.
            .mt(px((vh * 0.09).max(12.)))
            .w(px(760f32.min(vw - 32.)))
            .max_h(px(vh * 0.72))
            .flex()
            .flex_col()
            .bg(t::raised())
            .border_1()
            .border_color(t::line2())
            .rounded(px(14.))
            .shadow(t::shadow())
            .overflow_hidden()
            // A press anywhere puts the keyboard back in the box (the app's buttons hand it back
            // when clicked).
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                if this.kbd.take().is_some() && this.own_focus.is_focused(window) {
                    this.focus_input(window, cx);
                }
            }))
            .on_key_down(cx.listener(Self::key))
            .child(focus_target)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(16.))
                    .pr(px(12.))
                    .py(px(12.))
                    .border_b_1()
                    .border_color(t::line())
                    .child(icon(if commands { "command" } else { "search" }, 17., t::fg3()))
                    .children(self.type_chip.clone().filter(|_| !commands).map(|id| {
                        div()
                            .id("type-pill")
                            .role(gpui::Role::Button)
                            .aria_label(format!("{}, show all types", find_type(&id).map(|t| t.label).unwrap_or_else(|| id.clone())))
                            .relative()
                            .flex_none()
                            .h(px(24.))
                            .pl(px(8.))
                            .pr(px(6.))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .rounded(px(12.))
                            .text_size(px(12.5))
                            .text_color(t::accent_text())
                            .bg(t::accent_softer())
                            .border_1()
                            .border_color(t::accent_line())
                            .hover(|s| s.bg(t::accent_soft()))
                            .tooltip(ui::tip("Show all types (Backspace)"))
                            .on_click(cx.listener(|this, _, window, cx| this.press(Btn::Pill, window, cx)))
                            .child(icon(type_icon(&id), 13., t::accent_text()))
                            .child(find_type(&id).map(|t| t.label).unwrap_or_else(|| id.clone()))
                            .child(icon("x", 12., t::accent_text()))
                            .when(kbd == Some(Btn::Pill), |d| d.child(focus_ring(12.)))
                    }))
                    .child(self.input.clone())
                    .when(self.searching, |d| d.child(ui::spinner(15., t::fg3())))
                    .when(!commands && !tab_q.is_empty(), |d| {
                        d.child(
                            small_btn(
                                "p-save",
                                if saved { "star-fill" } else { "star" },
                                saved,
                                if saved { "Saved in the sidebar: rename (Ctrl+S)".into() } else { "Save this search to the sidebar (Ctrl+S)".into() },
                                kbd == Some(Btn::Save),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.press(Btn::Save, window, cx))),
                        )
                    })
                    .when(!commands, |d| {
                        d.child(
                            small_btn("p-help", "circle-question-mark", help, "File types and filters (Ctrl+/)".into(), kbd == Some(Btn::Help))
                                .on_click(cx.listener(|this, _, window, cx| this.press(Btn::Help, window, cx))),
                        )
                        .child(
                            div()
                                .id("scope")
                                .role(gpui::Role::Button)
                                .aria_label(format!("Scope: {scope_label}"))
                                .relative()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .h(px(26.))
                                .px(px(10.))
                                .rounded(px(13.))
                                .text_size(px(12.))
                                .border_1()
                                .text_color(if scope_on { t::accent_text() } else { t::fg2() })
                                .bg(if scope_on { t::accent_softer() } else { t::hover() })
                                .border_color(if scope_on { t::accent_line() } else { t::line() })
                                .when(!scope_fixed, |d| d.hover(|s| s.text_color(if scope_on { t::accent_text() } else { t::fg() }).border_color(if scope_on { t::accent_line() } else { t::line2() })))
                                .tooltip(ui::tip(scope_tip))
                                .on_click(cx.listener(|this, _, window, cx| this.press(Btn::Scope, window, cx)))
                                .child(icon(scope_icon, 13., if scope_on { t::accent_text() } else { t::fg2() }))
                                .child(div().max_w(px(140.)).truncate().child(scope_label))
                                .when(kbd == Some(Btn::Scope), |d| d.child(focus_ring(13.))),
                        )
                    })
                    .child(small_btn("p-close", "x", false, "Close (Esc)".into(), kbd == Some(Btn::Close)).on_click(cx.listener(|this, _, window, cx| this.press(Btn::Close, window, cx)))),
            )
            .when(!commands, |d| {
                // A narrow window: the chips wrap instead of running off the edge.
                let row = div().id("palette-types").flex_none().flex().gap(px(3.)).px(px(12.)).py(px(6.)).border_b_1().border_color(t::line());
                d.child(if narrow { row.flex_wrap() } else { row.overflow_x_scroll().track_scroll(&self.chips_scroll) }.children(chips))
            })
            .child(
                div()
                    .id("palette-list")
                    .role(if help { gpui::Role::Group } else { gpui::Role::ListBox })
                    .aria_label(if commands { "Commands" } else if help { "Types and filters" } else { "Results" })
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(6.))
                    .flex()
                    .flex_col()
                    .children(list),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(t::line())
                    .text_size(px(11.5))
                    .text_color(t::fg3())
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().child(self.footer_main(cx)))
                    .when(!commands, |d| {
                        d.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().text_color(t::fg2()).children(picked))
                            .children(self.footer_status(cx))
                    }),
            );
        div()
            .id("palette-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .bg(t::scrim())
            .flex()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.dismiss(cx)))
            .child(card)
    }
}

fn spoken_hit(hit: &Hit) -> String {
    let folder = Path::new(&hit.path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let mut s = format!("{}, {}{folder}", hit.name, if hit.is_dir { "folder in " } else { "" });
    if let (false, Some(size)) = (hit.is_dir, hit.size) {
        s.push_str(&format!(", {}", kinds::format_size(size)));
    }
    if let Some(l) = &hit.line {
        s.push_str(&format!(", line {}: {}", l.line, l.text.trim()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_match_runs_then_letters() {
        assert!(fuzzy("Toggle preview pane", "tpp").is_some());
        assert!(fuzzy("Go up", "gu").is_some());
        assert!(fuzzy("Refresh", "xyz").is_none());
        // A run of the letters beats scattered ones.
        assert!(fuzzy("Refresh", "ref").unwrap().0 > fuzzy("Go to Desktop", "gtd").unwrap().0);
    }

    #[test]
    fn filters_search_cant_read_are_named() {
        assert!(bad_filter("report size:>100mb").is_none());
        assert!(bad_filter("size:huge").is_some_and(|b| b.size && b.example == "size:>100mb"));
        assert!(bad_filter("modified:<7d").is_none());
        assert!(bad_filter("modified:2024-05").is_none());
        assert!(bad_filter("modified:2024-02-30").is_some());
        assert!(bad_filter("modified:soon").is_some_and(|b| !b.size && b.example == "modified:<7d"));
        // Still being typed.
        assert!(bad_filter("size:>").is_none());
    }

    #[test]
    fn unknown_types_get_a_guess() {
        assert!(known_type("image") && known_type("photos") && known_type("pdf") && known_type("files"));
        assert!(!known_type("imgz"));
        assert_eq!(did_you_mean("imgz").as_deref(), Some("image"));
        assert_eq!(did_you_mean("vidoes").as_deref(), Some("video"));
    }

    #[test]
    fn loose_matches_miss_a_word() {
        assert!(!is_loose("C:\\photos\\beach.jpg", "beach"));
        assert!(is_loose("C:\\x\\bxexaxcxh.jpg", "beach"));
        assert!(!is_loose("C:\\x\\beach.jpg", "beach type:image"));
    }

    #[test]
    fn everywhere_is_one_list_in_search_order() {
        let hit = |path: &str| Hit {
            path: path.into(),
            name: path.rsplit('\\').next().unwrap_or(path).into(),
            is_dir: false,
            size: None,
            modified: None,
            score: 0.,
            matches: Vec::new(),
            line: None,
        };
        // Search's order is kept whatever folder is open (no "In <folder>" group pulled to the
        // top); only a loose match goes last, under its own heading.
        let results = [hit(r"C:\elsewhere\report.txt"), hit(r"C:\x\r_e_p_o_r_t.txt"), hit(r"C:\open\report draft.txt"), hit(r"C:\open\sub\report.md")];
        let shown = ranked(&results, "report");
        let paths: Vec<&str> = shown.iter().map(|s| s.hit.path.as_str()).collect();
        assert_eq!(paths, [r"C:\elsewhere\report.txt", r"C:\open\report draft.txt", r"C:\open\sub\report.md", r"C:\x\r_e_p_o_r_t.txt"]);
        assert_eq!(shown.iter().map(|s| s.loose).collect::<Vec<_>>(), [false, false, false, true]);
    }

    #[test]
    fn query_helpers() {
        assert_eq!(without_filters("report type:doc modified:<30d"), "report");
        assert_eq!(without_filters(r#"notes in:"C:\My Files" draft"#), "notes draft");
        assert_eq!(without_types("beach type:image,video kind:doc"), "beach");
        assert!(awaiting_value("report ext:"));
        assert!(awaiting_value("size:>"));
        assert!(!awaiting_value("size:>1mb"));
        assert_eq!(in_folder_path(r#"x in:"C:\My Files""#).as_deref(), Some(r"C:\My Files"));
        assert_eq!(in_folder_path(r"x in:C:\dev y").as_deref(), Some(r"C:\dev"));
        assert_eq!(in_folder_path(r#"x in:"C:\My Files type:doc"#).as_deref(), Some(r"C:\My Files"));
        assert_eq!(in_folder_name(r"in:C:\dev\proj\").as_deref(), Some("proj"));
        assert!(CONTENT.is_match(r#"content:"budget"#));
        assert!(!CONTENT.is_match("content:"));
        assert_eq!(content_text(r#"x content:"two words""#), "two words");
        assert_eq!(files_word("Images"), "images");
        assert_eq!(files_word("Text"), "text files");
        assert_eq!(files_word("PDFs"), "PDFs");
    }

    #[test]
    fn places_read_as_the_app_shows_them() {
        assert_eq!(location(r"\\wsl.localhost\Ubuntu\home\me").1, "/home/me");
        assert_eq!(location(r"\\wsl.localhost\Ubuntu").1, "/");
        // A server's folder: its path there (the badge has the connection's name).
        assert_eq!(location(r"\\ssh:\home.local\home\me\ferret-qa-remote").1, "/home/me/ferret-qa-remote");
        assert_eq!(location(r"C:\Users\me").1, r"Users\me");
        assert_eq!(location(r"\\nas\share").1, r"\\nas\share");
    }
}
