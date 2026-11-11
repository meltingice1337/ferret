//! Selectable text in previews. GPUI has none outside Zed's editor, so this is Ferret's own: like
//! the text field (input.rs), it maps points to text through GPUI's text layout.
//!
//! A preview's text is a list of segments in reading order: a code line, a paragraph, a table
//! cell, a line on a PDF page. Each segment drawn says where it is ([`SelText`] for text GPUI lays
//! out, [`pdf_layer`] for a page's text over its picture, turned lines too); the [`area`] around
//! them turns presses and drags into a selection that runs across segments (double-click: a word,
//! triple-click: the line or paragraph, Shift+click: extends), drawn in the theme's selection
//! colour. A drag past the top or bottom of the view scrolls it, faster the further past, and the
//! selection follows. Ctrl+A and Ctrl+C come from the window's keys ([`Sel::select_all`],
//! [`Sel::selected_text`]), which read the text from the [`Source`], so segments scrolled out of
//! view are copied too. Ctrl+F finds in the same text ([`Find`]): every match drawn, the current
//! one stronger and scrolled to.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Div, Element, ElementId, Entity, EntityId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Path,
    Pixels, Point, ScrollHandle, ScrollStrategy, SharedString, StyledText, Subscription, TextAlign, TextLayout,
    UniformListScrollHandle, Window, WrappedLineLayout, canvas, div, fill, point, prelude::*, px, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::pdftext::{PdfLine, PdfText};
use crate::theme as t;

/// A place in the text: a segment, and a byte offset in it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub seg: usize,
    pub ix: usize,
}

impl Pos {
    pub fn new(seg: usize, ix: usize) -> Self {
        Pos { seg, ix }
    }
}

/// What the segments say, for copying, Ctrl+A and finding.
pub trait Source {
    fn count(&self) -> usize;
    fn text(&self, seg: usize) -> &str;
    /// What goes between this segment and the next in copied text.
    fn join(&self, _seg: usize) -> &str {
        "\n"
    }
    /// What to copy for the segment when it's selected whole, when that's more than it shows (a
    /// folded JSON node: all of it).
    fn whole(&self, _seg: usize) -> Option<String> {
        None
    }
    /// What to copy when everything is selected, when that's more than the segments (a JSON
    /// document, pretty-printed in full).
    fn all(&self) -> Option<String> {
        None
    }
    /// The list item the segment is drawn in (views drawn as a `uniform_list`), to scroll to it.
    fn item(&self, _seg: usize) -> Option<usize> {
        None
    }
    /// The page the segment is on and how far down it (a PDF's lines), to scroll to it.
    fn place(&self, _seg: usize) -> Option<(usize, f32)> {
        None
    }
}

/// Segments of their own: Markdown's blocks, a Word document's paragraphs, a JSON tree's rows.
#[derive(Default)]
pub struct Segs {
    pub list: Vec<(SharedString, &'static str)>,
    /// Copied instead of a segment's text when it's selected whole ([`Source::whole`]).
    pub whole: HashMap<usize, String>,
    /// Copied instead when everything is selected ([`Source::all`]).
    pub all: Option<Box<dyn Fn() -> String>>,
}

impl Segs {
    /// Adds a segment (`join`: what follows it when copied); its number.
    pub fn push(&mut self, text: SharedString, join: &'static str) -> usize {
        self.list.push((text, join));
        self.list.len() - 1
    }

    /// The last segment is followed by `join` instead (a table row's last cell ends the row).
    pub fn end_with(&mut self, join: &'static str) {
        if let Some(last) = self.list.last_mut() {
            last.1 = join;
        }
    }

    /// What follows segment `seg` when copied.
    pub fn set_join(&mut self, seg: usize, join: &'static str) {
        if let Some(s) = self.list.get_mut(seg) {
            s.1 = join;
        }
    }
}

impl Source for Segs {
    fn count(&self) -> usize {
        self.list.len()
    }
    fn text(&self, seg: usize) -> &str {
        self.list.get(seg).map(|s| s.0.as_ref()).unwrap_or("")
    }
    fn join(&self, seg: usize) -> &str {
        self.list.get(seg).map(|s| s.1).unwrap_or("\n")
    }
    fn whole(&self, seg: usize) -> Option<String> {
        self.whole.get(&seg).cloned()
    }
    fn all(&self) -> Option<String> {
        self.all.as_ref().map(|f| f())
    }
}

/// A code view's lines.
pub struct Lines(pub Arc<Vec<crate::render::Line>>);

impl Source for Lines {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn text(&self, seg: usize) -> &str {
        self.0.get(seg).map(|l| l.text.as_ref()).unwrap_or("")
    }
    fn item(&self, seg: usize) -> Option<usize> {
        Some(seg)
    }
}

/// A table's cells, row after row as shown (sorted), a tab between cells and a new line after
/// each row. `head`: its first row (a CSV's headings) comes first, then the rows in `order`.
pub struct Cells {
    pub rows: Arc<Vec<Vec<String>>>,
    pub order: Arc<Vec<usize>>,
    pub cols: usize,
    pub head: bool,
}

impl Cells {
    /// The segment of the cell in shown row `i` (of `order`), column `c`.
    pub fn seg(head: bool, cols: usize, i: usize, c: usize) -> usize {
        (i + head as usize) * cols + c
    }
}

impl Source for Cells {
    fn count(&self) -> usize {
        (self.order.len() + self.head as usize) * self.cols
    }
    fn text(&self, seg: usize) -> &str {
        if self.cols == 0 {
            return "";
        }
        let (r, c) = (seg / self.cols, seg % self.cols);
        let row = match (self.head, r) {
            (true, 0) => self.rows.first(),
            (true, r) => self.order.get(r - 1).and_then(|&i| self.rows.get(i)),
            (false, r) => self.order.get(r).and_then(|&i| self.rows.get(i)),
        };
        row.and_then(|row| row.get(c)).map(String::as_str).unwrap_or("")
    }
    fn join(&self, seg: usize) -> &str {
        if self.cols == 0 || seg % self.cols + 1 == self.cols { "\n" } else { "\t" }
    }
    fn item(&self, seg: usize) -> Option<usize> {
        let r = seg / self.cols.max(1);
        r.checked_sub(self.head as usize)
    }
}

/// A PDF's lines, page after page (a blank line between pages when copied).
pub struct PdfLines {
    pub text: Arc<PdfText>,
    /// The first segment of each page.
    starts: Vec<usize>,
}

impl PdfLines {
    pub fn new(text: Arc<PdfText>) -> Self {
        let mut starts = Vec::with_capacity(text.pages.len());
        let mut n = 0;
        for p in &text.pages {
            starts.push(n);
            n += p.len();
        }
        PdfLines { text, starts }
    }

    pub fn first_of(&self, page: usize) -> usize {
        self.starts.get(page).copied().unwrap_or(0)
    }

    fn locate(&self, seg: usize) -> Option<(usize, usize)> {
        let page = self.starts.partition_point(|&s| s <= seg).checked_sub(1)?;
        let line = seg - self.starts[page];
        (line < self.text.pages[page].len()).then_some((page, line))
    }
}

impl Source for PdfLines {
    fn count(&self) -> usize {
        self.text.lines()
    }
    fn text(&self, seg: usize) -> &str {
        self.locate(seg).map(|(p, l)| self.text.pages[p][l].text.as_str()).unwrap_or("")
    }
    fn join(&self, seg: usize) -> &str {
        match self.locate(seg) {
            Some((p, l)) if l + 1 == self.text.pages[p].len() => "\n\n",
            // A table's next cell on the same row.
            Some((p, l)) if self.text.pages[p][l].row_goes_on => "\t",
            _ => "\n",
        }
    }
    fn place(&self, seg: usize) -> Option<(usize, f32)> {
        let (p, l) = self.locate(seg)?;
        Some((p, self.text.pages[p][l].top))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Unit {
    #[default]
    Char,
    Word,
    Line,
}

/// How a drawn segment maps points to text and text to places.
#[derive(Clone)]
enum Geom {
    /// Laid out by GPUI (a `StyledText`), with the alignment it was drawn with.
    Laid { layout: TextLayout, align: TextAlign },
    /// A line of a page's text layer drawn at `at` (its characters' places are fractions of it).
    Pdf { text: Arc<PdfText>, page: usize, line: usize, at: Bounds<Pixels> },
}

impl Geom {
    /// A point on this segment (for a turned line, inside its own turned box, not just the box
    /// around that).
    fn holds(&self, p: Point<Pixels>) -> bool {
        match self {
            Geom::Pdf { text, page, line, at } => {
                let Some(l) = text.pages.get(*page).and_then(|pg| pg.get(*line)) else { return false };
                let Some(turn) = &l.turn else { return true };
                let (fx, fy) = frac(p, *at);
                let (a, c) = turn.from_page(fx, fy);
                let pad = (turn.bottom - turn.top) * 0.25;
                a >= turn.start - pad && a <= turn.end + pad && c >= turn.top - pad && c <= turn.bottom + pad
            }
            Geom::Laid { .. } => true,
        }
    }
}

/// `p` as fractions of `at`.
fn frac(p: Point<Pixels>, at: Bounds<Pixels>) -> (f32, f32) {
    (((p.x - at.left()) / at.size.width.max(px(1.))) as f32, ((p.y - at.top()) / at.size.height.max(px(1.))) as f32)
}

struct Painted {
    seg: usize,
    bounds: Bounds<Pixels>,
    /// The part of the window it could be seen in (scrolled out of view: nothing).
    clip: Bounds<Pixels>,
    geom: Geom,
}

impl Painted {
    fn seen(&self) -> Bounds<Pixels> {
        self.bounds.intersect(&self.clip)
    }
}

/// A part of the text to fill: a box, or (a turned line) four corners.
#[derive(Clone, Debug)]
enum Mark {
    Rect(Bounds<Pixels>),
    Quad([Point<Pixels>; 4]),
}

impl Mark {
    fn bounds(&self) -> Bounds<Pixels> {
        match self {
            Mark::Rect(b) => *b,
            Mark::Quad(q) => {
                let (mut lo, mut hi) = (q[0], q[0]);
                for p in &q[1..] {
                    lo = point(lo.x.min(p.x), lo.y.min(p.y));
                    hi = point(hi.x.max(p.x), hi.y.max(p.y));
                }
                Bounds::from_corners(lo, hi)
            }
        }
    }

    fn paint(&self, window: &mut Window, color: gpui::Hsla) {
        match self {
            Mark::Rect(b) => window.paint_quad(fill(*b, color)),
            Mark::Quad(q) => {
                let mut path = Path::new(q[0]);
                for p in &q[1..] {
                    path.line_to(*p);
                }
                window.paint_path(path, color);
            }
        }
    }
}

/// Matches of what Ctrl+F looks for: every one, and the current one stronger.
fn hit_color(current: bool) -> gpui::Hsla {
    if current { t::rgba(0xff9632, if t::is_dark() { 0.62 } else { 0.55 }) } else { t::rgba(0xffd400, if t::is_dark() { 0.32 } else { 0.42 }) }
}

/// Finding in a preview's text (Ctrl+F): the field it's typed in, and what it found.
pub struct Find {
    pub input: Option<Entity<crate::input::TextInput>>,
    _sub: Option<Subscription>,
    /// What's looked for, folded (case and accents ignored).
    query: Vec<char>,
    hits: Vec<(Pos, Pos)>,
    current: Option<usize>,
    /// The text the hits were found in (its key, how many segments, a sample of their lengths):
    /// found again when it changes (a PDF's text arriving, a JSON node opened).
    stamp: (String, usize, usize),
}

/// At most this many matches are kept (the count then says "10,000+").
pub const MAX_HITS: usize = 10_000;

/// A preview's selection, shared by the elements that draw its text and the area around them.
pub struct Sel {
    /// What is shown (the file and how): another one starts with nothing selected.
    key: String,
    source: Rc<dyn Source>,
    /// Where the press was (the word or line it picked, for double and triple clicks).
    anchor: Option<(Pos, Pos)>,
    /// Where the pointer is (or was let go).
    head: Option<Pos>,
    unit: Unit,
    dragging: bool,
    /// The last press was on this text: Ctrl+A and Ctrl+C are for it (not the files).
    pub active: bool,
    /// Drawn in the window's last frame.
    pub shown: bool,
    painted: Vec<Painted>,
    area: Bounds<Pixels>,
    owner: Option<EntityId>,
    /// The view the text scrolls in (the element that scrolls tracks it), for the key it's for:
    /// another key gets fresh ones, at the top. `scroll` is `list`'s own unless the view has one
    /// of its own (a PDF's); `blocks`: blocks that scroll sideways (a table, code in Markdown).
    list: UniformListScrollHandle,
    scroll: ScrollHandle,
    blocks: Vec<ScrollHandle>,
    scroll_key: String,
    /// The view is a `uniform_list` (only the rows in view are drawn).
    list_used: bool,
    /// Where the pointer is while a drag selects, and whether the view is scrolling on (the
    /// pointer past its edge).
    pointer: Option<Point<Pixels>>,
    rolling: bool,
    /// Ctrl+F, while its bar is open; what was looked for last (the bar opens with it again).
    pub find: Option<Find>,
    pub last_find: String,
    /// Scrolls to the current match after the next frame is drawn (tries left: a far one may need
    /// a second look once it's drawn).
    jump: u8,
    jump_queued: bool,
}

impl Default for Sel {
    fn default() -> Self {
        let list = UniformListScrollHandle::new();
        let scroll = list.0.borrow().base_handle.clone();
        Sel {
            key: String::new(),
            source: Rc::new(Segs::default()),
            anchor: None,
            head: None,
            unit: Unit::Char,
            dragging: false,
            active: false,
            shown: false,
            painted: Vec::new(),
            area: Bounds::default(),
            owner: None,
            list,
            scroll,
            blocks: Vec::new(),
            scroll_key: String::new(),
            list_used: false,
            pointer: None,
            rolling: false,
            find: None,
            last_find: String::new(),
            jump: 0,
            jump_queued: false,
        }
    }
}

pub type Shared = Rc<RefCell<Sel>>;

impl Sel {
    /// This frame draws `source`, shown as `key` (another key starts afresh), for the view `owner`.
    pub fn begin(&mut self, key: &str, source: Rc<dyn Source>, owner: EntityId) {
        let new_key = self.key != key;
        if new_key {
            self.key = key.to_string();
            self.clear();
            self.active = false;
        }
        self.source = source;
        self.painted.clear();
        self.shown = true;
        self.owner = Some(owner);
        // What's found is found again in other text (the next file, a node opened).
        if self.find.is_some() {
            let stamp = self.stamp();
            if self.find.as_ref().is_some_and(|f| f.stamp != stamp) {
                let keep = !new_key;
                self.search(keep);
                if new_key {
                    self.jump = 3;
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.dragging = false;
        self.unit = Unit::Char;
    }

    pub fn select_all(&mut self) {
        let n = self.source.count();
        if n == 0 {
            return;
        }
        self.anchor = Some((Pos::new(0, 0), Pos::new(0, 0)));
        self.head = Some(Pos::new(n - 1, self.source.text(n - 1).len()));
        self.unit = Unit::Char;
        self.active = true;
    }

    /// The selection, start before end (None when nothing is selected).
    fn range(&self) -> Option<(Pos, Pos)> {
        let (a0, a1) = self.anchor?;
        let h = self.head?;
        let (s, e) = match self.unit {
            Unit::Char => {
                if h < a0 {
                    (h, a0)
                } else {
                    (a0, h)
                }
            }
            // A word or line picked: dragging takes whole words or lines.
            _ if h < a0 => (self.unit_start(h), a1),
            _ => (a0, self.unit_end(h).max(a1)),
        };
        (s != e).then_some((s, e))
    }

    pub fn has_selection(&self) -> bool {
        self.range().is_some()
    }

    fn unit_start(&self, p: Pos) -> Pos {
        match self.unit {
            Unit::Line => Pos::new(p.seg, 0),
            _ => Pos::new(p.seg, word_at(self.source.text(p.seg), p.ix).start),
        }
    }

    fn unit_end(&self, p: Pos) -> Pos {
        let text = self.source.text(p.seg);
        match self.unit {
            Unit::Line => Pos::new(p.seg, text.len()),
            // At a word's start (its boundary): up to there.
            _ if p.ix == 0 || text.split_word_bound_indices().any(|(i, _)| i == p.ix) => p,
            _ => Pos::new(p.seg, word_at(text, p.ix).end),
        }
    }

    /// The selected text, segments joined as the source says (a segment selected whole copies
    /// what it stands for, everything selected the whole document, when the source says so).
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.range()?;
        let n = self.source.count();
        if n == 0 {
            return None;
        }
        let last = n - 1;
        if s == Pos::new(0, 0) && e.seg >= last && e.ix >= self.source.text(last).len() {
            if let Some(all) = self.source.all() {
                return Some(all);
            }
        }
        let mut out = String::new();
        for seg in s.seg..=e.seg.min(last) {
            let text = self.source.text(seg);
            let from = if seg == s.seg { s.ix } else { 0 };
            let to = if seg == e.seg { e.ix } else { text.len() };
            match self.source.whole(seg).filter(|_| from == 0 && to >= text.len()) {
                Some(w) => out.push_str(&w),
                None => out.push_str(slice(text, from, to)),
            }
            if seg < e.seg {
                out.push_str(self.source.join(seg));
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// The segment at `p` (on it, or with `nearest` the closest one drawn: the same line first).
    fn hit(&self, p: Point<Pixels>, nearest: bool) -> Option<Pos> {
        let seen = || self.painted.iter().filter(|e| e.clip.intersects(&e.bounds));
        if let Some(e) = seen().filter(|e| e.seen().contains(&p) && e.geom.holds(p)).last() {
            return Some(Pos::new(e.seg, self.index_at(e, p)));
        }
        if !nearest {
            return None;
        }
        let dist = |v: Pixels, lo: Pixels, hi: Pixels| if v < lo { lo - v } else if v > hi { v - hi } else { px(0.) };
        let e = seen().min_by(|a, b| {
            let (sa, sb) = (a.seen(), b.seen());
            let ka = (dist(p.y, sa.top(), sa.bottom()), dist(p.x, sa.left(), sa.right()));
            let kb = (dist(p.y, sb.top(), sb.bottom()), dist(p.x, sb.left(), sb.right()));
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        })?;
        Some(Pos::new(e.seg, self.index_at(e, p)))
    }

    /// The offset at `p` in a drawn segment. Past the end of text GPUI cut short (a table cell
    /// ending in "…"): the end of the segment's whole text.
    fn index_at(&self, e: &Painted, p: Point<Pixels>) -> usize {
        let ix = index_at(&e.geom, p);
        match &e.geom {
            Geom::Laid { layout, .. } if ix >= layout.len() => self.source.text(e.seg).len().max(ix),
            _ => ix,
        }
    }

    /// The selected part of a segment drawn as `geom`, and the matches found in it.
    fn marks(&self, seg: usize, geom: &Geom) -> (Vec<Mark>, Vec<(Mark, bool)>) {
        let mut sel = Vec::new();
        if let Some((s, e)) = self.range().filter(|(s, e)| seg >= s.seg && seg <= e.seg) {
            let len = self.source.text(seg).len();
            let from = if seg == s.seg { s.ix } else { 0 };
            let to = if seg == e.seg { e.ix.min(len) } else { len };
            sel = marks_of(geom, from..to, seg < e.seg);
        }
        let mut found = Vec::new();
        if let Some(f) = &self.find {
            let first = f.hits.partition_point(|h| h.0.seg < seg);
            for (i, (s, e)) in f.hits.iter().enumerate().skip(first).take_while(|(_, h)| h.0.seg == seg) {
                let cur = f.current == Some(i);
                found.extend(marks_of(geom, s.ix..e.ix, false).into_iter().map(|m| (m, cur)));
            }
        }
        (sel, found)
    }

    /// Picks what a press at `p` picks; false if no text is drawn.
    fn press(&mut self, p: Point<Pixels>, clicks: usize, shift: bool) -> bool {
        let Some(pos) = self.hit(p, true) else { return false };
        self.active = true;
        self.dragging = true;
        self.pointer = Some(p);
        if clicks <= 1 && shift && self.anchor.is_some() {
            self.head = Some(pos);
            return true;
        }
        let text = self.source.text(pos.seg);
        let (unit, a0, a1) = match clicks {
            0 | 1 => (Unit::Char, pos, pos),
            2 => {
                let w = word_at(text, pos.ix);
                (Unit::Word, Pos::new(pos.seg, w.start), Pos::new(pos.seg, w.end))
            }
            _ => (Unit::Line, Pos::new(pos.seg, 0), Pos::new(pos.seg, text.len())),
        };
        self.unit = unit;
        self.anchor = Some((a0, a1));
        self.head = Some(a1);
        true
    }

    /// The pointer moved while the button is down: the selection follows it (kept to the area).
    fn drag(&mut self, p: Point<Pixels>) -> bool {
        let a = self.area;
        let p = if a.size.width > px(0.) { point(p.x.clamp(a.left(), a.right() - px(1.)), p.y.clamp(a.top(), a.bottom() - px(1.))) } else { p };
        match self.hit(p, true) {
            Some(pos) if self.head != Some(pos) => {
                self.head = Some(pos);
                true
            }
            _ => false,
        }
    }

    // ───────────────────────────── scrolling ─────────────────────────────

    fn keyed(&mut self, key: &str) {
        if self.scroll_key != key {
            self.scroll_key = key.to_string();
            self.list = UniformListScrollHandle::new();
            self.scroll = self.list.0.borrow().base_handle.clone();
            self.blocks.clear();
        }
    }

    /// The scroll handle for a view (a `div`) showing `key`: the element that scrolls tracks it,
    /// so a drag past its edge and Ctrl+F can scroll it. Another key gets a fresh one (at the top).
    pub fn scroller(&mut self, key: &str) -> ScrollHandle {
        self.keyed(key);
        self.scroll = self.list.0.borrow().base_handle.clone();
        self.list_used = false;
        self.scroll.clone()
    }

    /// The same for a view drawn as a `uniform_list` (it shares the offset with [`Self::scroller`]:
    /// a code view keeps its place when Wrap switches between the two).
    pub fn list_scroller(&mut self, key: &str) -> UniformListScrollHandle {
        self.keyed(key);
        self.scroll = self.list.0.borrow().base_handle.clone();
        self.list_used = true;
        self.list.clone()
    }

    /// The view scrolls with a handle of its own (a PDF's, which also says which pages to draw).
    pub fn scrolls_with(&mut self, h: &ScrollHandle) {
        self.scroll = h.clone();
        self.scroll_key.clear();
        self.list_used = false;
    }

    /// A handle for block `i` of the view showing `key` that scrolls sideways on its own (a code
    /// block in Markdown, a wide table).
    pub fn block_scroller(&mut self, key: &str, i: usize) -> ScrollHandle {
        self.keyed(key);
        while self.blocks.len() <= i {
            self.blocks.push(ScrollHandle::new());
        }
        self.blocks[i].clone()
    }

    /// How fast to scroll on (px/s each way, negative: up or left) with the pointer at `p` while
    /// dragging: the further past the view's edge, the faster.
    fn edge_speed(&self, p: Point<Pixels>) -> (f32, f32) {
        let b = self.scroll.bounds();
        let b = if b.size.height > px(0.) { b } else { self.area };
        if b.size.height <= px(0.) {
            return (0., 0.);
        }
        let past = |v: Pixels, lo: Pixels, hi: Pixels| if v < lo { (v - lo).as_f32() } else if v > hi { (v - hi).as_f32() } else { 0. };
        let speed = |d: f32| if d == 0. { 0. } else { (60. + d.abs() * 16.).min(4000.) * d.signum() };
        let max = self.scroll.max_offset();
        let x = if max.x > px(0.) { speed(past(p.x, b.left(), b.right())) } else { 0. };
        let y = if max.y > px(0.) { speed(past(p.y, b.top(), b.bottom())) } else { 0. };
        (x, y)
    }

    /// One step of scrolling on, `dt` seconds after the last: None when it should stop, else
    /// whether anything changed.
    fn roll(&mut self, dt: f32) -> Option<bool> {
        let p = self.pointer.filter(|_| self.dragging)?;
        let (vx, vy) = self.edge_speed(p);
        if vx == 0. && vy == 0. {
            return None;
        }
        let h = self.scroll.clone();
        let max = h.max_offset();
        let off = h.offset();
        let next = point((off.x - px(vx * dt)).clamp(-max.x.max(px(0.)), px(0.)), (off.y - px(vy * dt)).clamp(-max.y.max(px(0.)), px(0.)));
        let moved = next != off;
        if moved {
            h.set_offset(next);
        }
        let followed = self.drag(p);
        Some(moved || followed)
    }

    // ───────────────────────────── finding ─────────────────────────────

    /// Ctrl+F's bar opened, with its field (`sub`: what follows its typing).
    pub fn open_find(&mut self, input: Entity<crate::input::TextInput>, sub: Subscription) {
        self.find = Some(Find { input: Some(input), _sub: Some(sub), query: Vec::new(), hits: Vec::new(), current: None, stamp: Default::default() });
    }

    pub fn close_find(&mut self) {
        self.find = None;
    }

    /// Looks for `query` (case and accents ignored), from where the view is.
    pub fn find_text(&mut self, query: &str) {
        let Some(f) = self.find.as_mut() else { return };
        f.query = crate::sortgroup::fold(query);
        self.search(false);
        self.jump = 3;
    }

    /// The next match (or `back`: the one before), round to the start (end) again.
    pub fn find_step(&mut self, back: bool) {
        let Some(f) = self.find.as_mut() else { return };
        let n = f.hits.len();
        if n == 0 {
            return;
        }
        f.current = Some(match f.current {
            None => 0,
            Some(i) if back => (i + n - 1) % n,
            Some(i) => (i + 1) % n,
        });
        self.jump = 3;
    }

    /// Where finding is: the current match (from 1) and how many.
    pub fn find_status(&self) -> Option<(usize, usize)> {
        let f = self.find.as_ref()?;
        Some((f.current.map(|i| i + 1).unwrap_or(0), f.hits.len()))
    }

    fn stamp(&self) -> (String, usize, usize) {
        let n = self.source.count();
        let sample: usize = (0..n.min(32)).chain(n.saturating_sub(32).max(32)..n).map(|i| self.source.text(i).len()).sum();
        (self.key.clone(), n, sample)
    }

    /// Finds the query in the text again. `keep`: the same text changed (keeps the current match
    /// where it was); else the first one from the top of the view.
    fn search(&mut self, keep: bool) {
        let stamp = self.stamp();
        let top = self.painted.iter().filter(|e| e.clip.intersects(&e.bounds) && e.bounds.bottom() > e.clip.top()).map(|e| e.seg).min().unwrap_or(0);
        let source = self.source.clone();
        let Some(f) = self.find.as_mut() else { return };
        let old = f.current.and_then(|i| f.hits.get(i).copied());
        f.stamp = stamp;
        f.hits.clear();
        if !f.query.is_empty() {
            for seg in 0..source.count() {
                for r in find_in(source.text(seg), &f.query) {
                    f.hits.push((Pos::new(seg, r.start), Pos::new(seg, r.end)));
                }
                if f.hits.len() >= MAX_HITS {
                    f.hits.truncate(MAX_HITS);
                    break;
                }
            }
        }
        f.current = if f.hits.is_empty() {
            None
        } else if let Some(o) = old.filter(|_| keep) {
            Some(f.hits.partition_point(|h| h.0 < o.0).min(f.hits.len() - 1))
        } else {
            Some(f.hits.iter().position(|h| h.0.seg >= top).unwrap_or(0))
        };
    }

    /// Scrolls so the current match shows (a third of the way down), from where things were drawn
    /// in the last frame. True if it was a guess (a list row or PDF page not drawn yet): it looks
    /// again once that's drawn.
    fn jump_to_current(&mut self) -> bool {
        let Some((s, e)) = self.find.as_ref().and_then(|f| f.current.and_then(|i| f.hits.get(i).copied())) else { return false };
        let h = self.scroll.clone();
        let view = h.bounds();
        if view.size.height <= px(0.) {
            return false;
        }
        let max = h.max_offset();
        let off = h.offset();
        let clamp_y = |y: Pixels| y.clamp(-max.y.max(px(0.)), px(0.));
        // Drawn in the last frame (anywhere in a view that draws it all).
        if let Some(p) = self.painted.iter().find(|p| p.seg == s.seg) {
            let marks = marks_of(&p.geom, s.ix..e.ix, false);
            let b = marks.iter().map(Mark::bounds).reduce(|a, b| a.union(&b)).unwrap_or(p.bounds);
            let inside_y = b.top() >= view.top() + px(4.) && b.bottom() <= view.bottom() - px(4.);
            let inside_x = b.left() >= view.left() && b.right() <= view.right();
            if inside_y && inside_x {
                return false;
            }
            let y = if inside_y { off.y } else { clamp_y(off.y - (b.top() - view.top() - view.size.height * 0.35)) };
            let x = if inside_x { off.x } else { (off.x - (b.left() - view.left() - view.size.width * 0.2)).clamp(-max.x.max(px(0.)), px(0.)) };
            h.set_offset(point(x, y));
            // In a view that's drawn only where it shows (a list), the row is in place once it's drawn.
            return false;
        }
        if self.list_used {
            if let Some(i) = self.source.item(s.seg) {
                self.list.scroll_to_item(i, ScrollStrategy::Center);
                return true;
            }
        }
        // A PDF page not in view: to its place (the page's lines aren't drawn till it shows).
        if let Some((page, top)) = self.source.place(s.seg) {
            if let Some(cb) = h.bounds_for_item(page) {
                // The page's box (without its "n / N" under it), unscrolled.
                let y = cb.top() - view.top() + (cb.size.height - px(17.)).max(px(0.)) * top;
                h.set_offset(point(off.x, clamp_y(-(y - view.size.height * 0.35))));
                return true;
            }
        }
        false
    }

    /// A short description for tests: what's selected, how long, what's found.
    pub fn describe(&self) -> serde_json::Value {
        let text = self.selected_text();
        serde_json::json!({
            "key": self.key,
            "active": self.active,
            "shown": self.shown,
            "len": text.as_ref().map(|t| t.chars().count()).unwrap_or(0),
            "text": text.map(|t| t.chars().take(400).collect::<String>()),
            "segments": self.source.count(),
            "drawn": self.painted.iter().filter(|e| e.clip.intersects(&e.bounds)).count(),
            "scroll": [self.scroll.offset().x.as_f32().round(), self.scroll.offset().y.as_f32().round()],
            "max_scroll": [self.scroll.max_offset().x.as_f32().round(), self.scroll.max_offset().y.as_f32().round()],
            "find": self.find.as_ref().map(|f| serde_json::json!({
                "hits": f.hits.len(),
                "current": f.current.map(|i| i + 1),
                "at": f.current.and_then(|i| f.hits.get(i)).map(|h| self.source.text(h.0.seg).get(h.0.ix..h.1.ix).unwrap_or("").to_string()),
                "seg": f.current.and_then(|i| f.hits.get(i)).map(|h| h.0.seg),
                "visible": f.current.and_then(|i| f.hits.get(i)).map(|h| self.painted.iter().any(|p| p.seg == h.0.seg && p.clip.intersects(&p.bounds))),
            })),
        })
    }
}

/// `text[from..to]`, kept to character boundaries.
fn slice(text: &str, from: usize, to: usize) -> &str {
    let mut a = from.min(text.len());
    let mut b = to.min(text.len()).max(a);
    while a > 0 && !text.is_char_boundary(a) {
        a -= 1;
    }
    while b < text.len() && !text.is_char_boundary(b) {
        b += 1;
    }
    &text[a..b]
}

/// The word (or run of spaces or punctuation) at `ix`.
fn word_at(text: &str, ix: usize) -> Range<usize> {
    let mut last = 0..0;
    for (i, w) in text.split_word_bound_indices() {
        let r = i..i + w.len();
        if r.contains(&ix) {
            return r;
        }
        last = r;
    }
    last
}

/// Where `needle` (folded: `sortgroup::fold`) is in `text`, case and accents ignored: byte ranges
/// of `text`.
fn find_in(text: &str, needle: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle.is_empty() || text.is_empty() {
        return out;
    }
    // Each folded character, and the bytes of `text` it came from.
    let mut folded: Vec<char> = Vec::with_capacity(text.len());
    let mut from: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    let mut buf = [0u8; 4];
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        if c.is_ascii() {
            folded.push(c.to_ascii_lowercase());
            from.push((i, end));
        } else {
            for f in crate::sortgroup::fold(c.encode_utf8(&mut buf)) {
                folded.push(f);
                from.push((i, end));
            }
        }
    }
    let n = needle.len();
    let mut k = 0;
    while k + n <= folded.len() {
        if folded[k] == needle[0] && folded[k..k + n] == *needle {
            out.push(from[k].0..from[k + n - 1].1);
            k += n;
        } else {
            k += 1;
        }
    }
    out
}

// ───────────────────────────── geometry ─────────────────────────────

/// A row of text as drawn (a line, or part of a wrapped one).
struct Row {
    layout: Arc<WrappedLineLayout>,
    /// Byte offset of its line in the whole text.
    base: usize,
    /// Its offsets in its line, and where they are in the line unwrapped.
    start: usize,
    end: usize,
    x0: Pixels,
    x1: Pixels,
    top: Pixels,
    /// Its shift for centred or right-aligned text.
    off: Pixels,
    /// The last row of its line (a newline follows it, unless it's the text's last).
    last: bool,
}

impl Row {
    fn x(&self, ix_in_line: usize, left: Pixels) -> Pixels {
        left + self.off + self.layout.unwrapped_layout.x_for_index(ix_in_line).clamp(self.x0, self.x1) - self.x0
    }
}

fn rows(layout: &TextLayout, align: TextAlign) -> (Bounds<Pixels>, Pixels, Vec<Row>) {
    let bounds = layout.bounds();
    let lh = layout.line_height();
    let mut rows = Vec::new();
    let mut base = 0;
    let mut top = bounds.top();
    for line in layout.line_layouts() {
        let un = &line.unwrapped_layout;
        let breaks: Vec<(usize, Pixels)> = line
            .wrap_boundaries
            .iter()
            .filter_map(|b| un.runs.get(b.run_ix).and_then(|r| r.glyphs.get(b.glyph_ix)).map(|g| (g.index, g.position.x)))
            .collect();
        let mut from = (0, px(0.));
        for k in 0..=breaks.len() {
            let to = breaks.get(k).copied().unwrap_or((un.len, un.width));
            let w = to.1 - from.1;
            let off = match align {
                TextAlign::Left => px(0.),
                TextAlign::Center => (bounds.size.width - w) / 2.,
                TextAlign::Right => bounds.size.width - w,
            };
            rows.push(Row { layout: line.clone(), base, start: from.0, end: to.0, x0: from.1, x1: to.1, top, off, last: k == breaks.len() });
            top += lh;
            from = to;
        }
        base += un.len + 1;
    }
    (bounds, lh, rows)
}

/// Where along a page's line `p` is: an upright line's x as a fraction of the page's width, a
/// turned one's place along it (its own frame).
fn along(l: &PdfLine, p: Point<Pixels>, at: Bounds<Pixels>) -> f32 {
    let (fx, fy) = frac(p, at);
    match &l.turn {
        Some(t) => t.from_page(fx, fy).0,
        None => fx,
    }
}

/// The text offset nearest to `p` (above or below the text: on its first or last row).
fn index_at(geom: &Geom, p: Point<Pixels>) -> usize {
    match geom {
        Geom::Laid { layout, align } => {
            let (bounds, _, rows) = rows(layout, *align);
            let Some(row) = rows.iter().rev().find(|r| p.y >= r.top).or(rows.first()) else { return 0 };
            let x = p.x - bounds.left() - row.off + row.x0;
            let ix = if x <= row.x0 {
                row.start
            } else if x >= row.x1 {
                row.end
            } else {
                row.layout.unwrapped_layout.closest_index_for_x(x).clamp(row.start, row.end)
            };
            row.base + ix
        }
        Geom::Pdf { text, page, line, at } => {
            let Some(l) = text.pages.get(*page).and_then(|p| p.get(*line)) else { return 0 };
            let a = along(l, p, *at);
            l.chars.iter().find(|c| a < (c.1 + c.2) / 2.).map(|c| c.0).unwrap_or(l.text.len())
        }
    }
}

/// What covers `range` of a segment; `more`: the selection goes on past its end (a newline's
/// width is shown after its last row).
fn marks_of(geom: &Geom, range: Range<usize>, more: bool) -> Vec<Mark> {
    let mut out = Vec::new();
    match geom {
        Geom::Laid { layout, align } => {
            let (bounds, lh, rows) = rows(layout, *align);
            let n = rows.len();
            for (i, row) in rows.iter().enumerate() {
                let (rs, re) = (row.base + row.start, row.base + row.end);
                // A newline after this row is selected: the next line's start is, or the
                // selection goes on to the next segment.
                let newline = row.last && (range.end > re || (i + 1 == n && more));
                let (s, e) = (range.start.max(rs), range.end.min(re));
                if s > e || (s == e && !(newline && range.start <= re)) {
                    continue;
                }
                let left = bounds.left();
                let x0 = if s > rs { row.x(s - row.base, left) } else { left + row.off };
                let mut x1 = if e < re { row.x(e - row.base, left) } else { left + row.off + (row.x1 - row.x0) };
                if newline {
                    x1 += (lh * 0.35).max(px(3.));
                }
                if x1 > x0 {
                    out.push(Mark::Rect(Bounds::from_corners(point(x0, row.top), point(x1, row.top + lh))));
                }
            }
        }
        Geom::Pdf { text, page, line, at } => {
            let Some(l) = text.pages.get(*page).and_then(|p| p.get(*line)) else { return out };
            let (lo, hi) = match &l.turn {
                Some(t) => (t.start, t.end),
                None => (l.left, l.right),
            };
            let edge = |ix: usize, right: bool| -> f32 {
                if ix >= l.text.len() {
                    return hi;
                }
                match l.chars.iter().find(|c| c.0 >= ix) {
                    Some(c) => c.1,
                    None if right => hi,
                    None => lo,
                }
            };
            let x0 = if range.start == 0 { lo } else { edge(range.start, false) };
            let x1 = if range.end >= l.text.len() { hi } else { edge(range.end, true) };
            if x1 > x0 {
                let (w, h) = (at.size.width, at.size.height);
                let px_of = |(fx, fy): (f32, f32)| point(at.left() + w * fx, at.top() + h * fy);
                match &l.turn {
                    // Along the line, turned as it is.
                    Some(t) => out.push(Mark::Quad([
                        px_of(t.to_page(x0, t.top)),
                        px_of(t.to_page(x1, t.top)),
                        px_of(t.to_page(x1, t.bottom)),
                        px_of(t.to_page(x0, t.bottom)),
                    ])),
                    None => out.push(Mark::Rect(Bounds::from_corners(px_of((x0, l.top)), px_of((x1, l.bottom))))),
                }
            }
        }
    }
    out
}

// ───────────────────────────── elements ─────────────────────────────

/// Text GPUI lays out (a `StyledText`), selectable as segment `seg` of `sel`.
pub struct SelText {
    text: StyledText,
    seg: usize,
    sel: Shared,
}

impl SelText {
    pub fn new(text: StyledText, seg: usize, sel: &Shared) -> Self {
        SelText { text, seg, sel: sel.clone() }
    }
}

impl IntoElement for SelText {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.text.prepaint(None, inspector_id, bounds, state, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        let layout = self.text.layout().clone();
        let geom = Geom::Laid { layout: layout.clone(), align: window.text_style().text_align };
        let clip = window.content_mask().bounds;
        // Out of view: only where it is (a long document's every paragraph is painted).
        let (sel, found) = if clip.intersects(&bounds) { self.sel.borrow().marks(self.seg, &geom) } else { Default::default() };
        // Run backgrounds (code spans, highlights), what Ctrl+F found, the selection, then the
        // glyphs on top.
        let _ = layout.paint_background(window, cx);
        for (m, cur) in &found {
            m.paint(window, hit_color(*cur));
        }
        for m in &sel {
            m.paint(window, t::selection());
        }
        let _ = layout.paint_foreground(window, cx);
        self.sel.borrow_mut().painted.push(Painted { seg: self.seg, bounds, clip, geom });
    }
}

/// A page's text, over its picture (fill the page's box with it): nothing drawn but the selection
/// and matches, an I-beam over the lines, and the lines' places for the area's presses and drags.
/// `first`: the page's first line's segment. Out of view a page takes no part (a long PDF has
/// thousands of lines), unless `always` (a slide's turned shape: Ctrl+F finds its place).
pub fn pdf_layer(sel: &Shared, text: Arc<PdfText>, page: usize, first: usize, always: bool) -> impl IntoElement {
    let (s1, s2) = (sel.clone(), sel.clone());
    let t1 = text.clone();
    canvas(
        move |bounds, window, _| {
            let clip = window.content_mask().bounds;
            if !clip.intersects(&bounds) {
                return (always, Vec::new());
            }
            let Some(lines) = t1.pages.get(page) else { return (false, Vec::new()) };
            let (w, h) = (bounds.size.width, bounds.size.height);
            let boxes = lines
                .iter()
                .map(|l| {
                    let b = Bounds::from_corners(point(bounds.left() + w * l.left, bounds.top() + h * l.top), point(bounds.left() + w * l.right, bounds.top() + h * l.bottom));
                    window.insert_hitbox(b, HitboxBehavior::Normal)
                })
                .collect::<Vec<_>>();
            (true, boxes)
        },
        move |bounds, (take_part, hitboxes), window, _| {
            if !take_part {
                return;
            }
            let clip = window.content_mask().bounds;
            let (w, h) = (bounds.size.width, bounds.size.height);
            let Some(lines) = text.pages.get(page) else { return };
            let mut sel_marks = Vec::new();
            let mut found = Vec::new();
            let mut painted = Vec::new();
            {
                let s = s1.borrow();
                for (i, l) in lines.iter().enumerate() {
                    let geom = Geom::Pdf { text: text.clone(), page, line: i, at: bounds };
                    let line_box = match hitboxes.get(i) {
                        Some(hb) => {
                            window.set_cursor_style(CursorStyle::IBeam, hb);
                            let (m, f) = s.marks(first + i, &geom);
                            sel_marks.extend(m);
                            found.extend(f);
                            hb.bounds
                        }
                        None => Bounds::from_corners(point(bounds.left() + w * l.left, bounds.top() + h * l.top), point(bounds.left() + w * l.right, bounds.top() + h * l.bottom)),
                    };
                    painted.push(Painted { seg: first + i, bounds: line_box, clip, geom });
                }
            }
            for (m, cur) in found {
                m.paint(window, hit_color(cur));
            }
            for m in sel_marks {
                m.paint(window, t::selection());
            }
            s2.borrow_mut().painted.extend(painted);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Scrolls on while a drag is past the view's edge (until it's back inside or let go).
fn roll_on(sel: &Shared, window: &mut Window, cx: &mut App) {
    let s = sel.clone();
    window
        .spawn(cx, async move |cx| {
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let dt = last.elapsed().as_secs_f32().min(0.05);
                last = Instant::now();
                let (step, owner) = {
                    let mut st = s.borrow_mut();
                    let step = st.roll(dt);
                    if step.is_none() {
                        st.rolling = false;
                    }
                    (step, st.owner)
                };
                match step {
                    None => break,
                    Some(true) => {
                        if let Some(o) = owner {
                            if cx.update(|_, cx| cx.notify(o)).is_err() {
                                break;
                            }
                        }
                    }
                    Some(false) => {}
                }
            }
        })
        .detach();
}

/// The area holding selectable text: a press there starts a selection (Shift extends it), a
/// drag (also outside the area) extends it, and past the top or bottom of the view scrolls it.
/// Size it as the view it holds.
pub fn area(sel: &Shared) -> Div {
    let (s1, s2) = (sel.clone(), sel.clone());
    div()
        .relative()
        .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, cx| {
            let owner = {
                let mut s = s1.borrow_mut();
                if !s.press(ev.position, ev.click_count, ev.modifiers.shift) {
                    return;
                }
                s.owner
            };
            if let Some(o) = owner {
                cx.notify(o);
            }
        })
        .child(
            canvas(
                move |b, _, _| b,
                move |b, _, window, cx| {
                    s2.borrow_mut().area = b;
                    let s = s2.clone();
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let (owner, roll) = {
                            let mut st = s.borrow_mut();
                            if !st.dragging {
                                return;
                            }
                            if ev.pressed_button != Some(MouseButton::Left) {
                                st.dragging = false;
                                st.pointer = None;
                                return;
                            }
                            st.pointer = Some(ev.position);
                            let (vx, vy) = st.edge_speed(ev.position);
                            let roll = !st.rolling && (vx != 0. || vy != 0.);
                            if roll {
                                st.rolling = true;
                            }
                            (st.drag(ev.position).then_some(st.owner).flatten(), roll)
                        };
                        if roll {
                            roll_on(&s, window, cx);
                        }
                        if let Some(o) = owner {
                            cx.notify(o);
                        }
                    });
                    let s = s2.clone();
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, _| {
                        if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                            let mut st = s.borrow_mut();
                            st.dragging = false;
                            st.pointer = None;
                        }
                    });
                    // Ctrl+F asked to show a match: once this frame is drawn (where everything is).
                    let queue = {
                        let mut st = s2.borrow_mut();
                        let q = st.jump > 0 && !st.jump_queued;
                        if q {
                            st.jump_queued = true;
                        }
                        q
                    };
                    if queue {
                        let s = s2.clone();
                        window
                            .spawn(cx, async move |cx| {
                                cx.background_executor().timer(Duration::from_millis(1)).await;
                                let owner = {
                                    let mut st = s.borrow_mut();
                                    st.jump_queued = false;
                                    let again = st.jump_to_current();
                                    st.jump = if again { st.jump.saturating_sub(1) } else { 0 };
                                    st.owner
                                };
                                if let Some(o) = owner {
                                    let _ = cx.update(|_, cx| cx.notify(o));
                                }
                            })
                            .detach();
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

thread_local! {
    /// A scrollbar's thumb being dragged: its handle, where the press was, and the offset then.
    static HDRAG: RefCell<Option<(ScrollHandle, Pixels, Pixels)>> = const { RefCell::new(None) };
}

/// A horizontal scrollbar for a view that scrolls sideways (`h` tracked by it), along the bottom
/// of the box it's put in: drawn only when there's more to the side. Its thumb drags the view, a
/// press beside it pages.
pub fn hbar(h: &ScrollHandle) -> impl IntoElement + use<> {
    let (h1, h2) = (h.clone(), h.clone());
    let hovered = Rc::new(Cell::new(false));
    canvas(
        move |bounds, window, _| {
            let view = h1.bounds();
            let max = h1.max_offset().x;
            if max <= px(1.) || view.size.width <= px(0.) {
                return None;
            }
            let tw = (bounds.size.width * (view.size.width / (view.size.width + max))).max(px(28.)).min(bounds.size.width);
            let k = (-h1.offset().x / max).clamp(0., 1.);
            let thumb = Bounds::new(point(bounds.left() + (bounds.size.width - tw) * k, bounds.top()), size(tw, bounds.size.height));
            Some((thumb, window.insert_hitbox(bounds, HitboxBehavior::BlockMouse)))
        },
        move |bounds, state, window, _| {
            let Some((thumb, hitbox)) = state else { return };
            let active = hitbox.is_hovered(window) || HDRAG.with(|d| d.borrow().is_some());
            hovered.set(active);
            let bar = Bounds::new(point(thumb.left(), thumb.top() + px(if active { 1. } else { 2. })), size(thumb.size.width, thumb.size.height - px(if active { 2. } else { 4. })));
            window.paint_quad(fill(bar, if active { t::fg3() } else { t::line2() }).corner_radii(px(4.)));
            window.set_cursor_style(CursorStyle::Arrow, &hitbox);
            let owner = window.current_view();
            let (h3, h4) = (h2.clone(), h2.clone());
            let max = h2.max_offset().x;
            let track = bounds.size.width - thumb.size.width;
            window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || ev.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                cx.stop_propagation();
                let off = h3.offset();
                if thumb.contains(&ev.position) {
                    HDRAG.with(|d| *d.borrow_mut() = Some((h3.clone(), ev.position.x, off.x)));
                } else {
                    // A page to that side.
                    let page = h3.bounds().size.width * 0.9;
                    let x = if ev.position.x < thumb.left() { off.x + page } else { off.x - page };
                    h3.set_offset(point(x.clamp(-max, px(0.)), off.y));
                }
                cx.notify(owner);
            });
            window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let drag = HDRAG.with(|d| d.borrow().clone());
                let Some((h, x0, off0)) = drag.filter(|(h, ..)| h.bounds() == h4.bounds()) else { return };
                if ev.pressed_button != Some(MouseButton::Left) {
                    HDRAG.with(|d| *d.borrow_mut() = None);
                    cx.notify(owner);
                    return;
                }
                let dx = ev.position.x - x0;
                let x = off0 - if track > px(0.) { dx * (max / track) } else { px(0.) };
                let off = h.offset();
                h.set_offset(point(x.clamp(-max, px(0.)), off.y));
                cx.notify(owner);
            });
            window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && HDRAG.with(|d| d.borrow_mut().take()).is_some() {
                    cx.notify(owner);
                }
            });
        },
    )
    .absolute()
    .left_0()
    .right_0()
    .bottom_0()
    .h(px(9.))
}

/// Builds a view of many blocks: each text it's given becomes the next segment.
pub struct Builder {
    pub sel: Shared,
    pub segs: Segs,
    key: String,
}

impl Builder {
    /// A view showing `key` (the file and how it's shown).
    pub fn new(sel: &Shared, key: &str) -> Self {
        Builder { sel: sel.clone(), segs: Segs::default(), key: key.to_string() }
    }

    /// The handle the view's scrolling element tracks (see [`Sel::scroller`]).
    pub fn scroller(&self) -> ScrollHandle {
        self.sel.borrow_mut().scroller(&self.key)
    }

    /// A handle for the view's block `i` that scrolls sideways on its own.
    pub fn block_scroller(&self, i: usize) -> ScrollHandle {
        self.sel.borrow_mut().block_scroller(&self.key, i)
    }

    /// `styled`, drawing `plain` (its text): the next segment, followed by `join` when copied.
    pub fn text(&mut self, styled: StyledText, plain: SharedString, join: &'static str) -> AnyElement {
        let seg = self.segs.push(plain, join);
        SelText::new(styled, seg, &self.sel).into_any_element()
    }

    /// The next segment's number.
    pub fn next(&self) -> usize {
        self.segs.list.len()
    }

    /// A page of text drawn as a picture (a turned slide shape): its lines become the next
    /// segments (`joins`: what follows each when copied), laid over the picture's box.
    pub fn layer(&mut self, text: Arc<PdfText>, page: usize, joins: &[&'static str]) -> AnyElement {
        let first = self.next();
        if let Some(lines) = text.pages.get(page) {
            for (i, l) in lines.iter().enumerate() {
                self.segs.push(SharedString::from(l.text.clone()), joins.get(i).copied().unwrap_or("\n"));
            }
        }
        pdf_layer(&self.sel, text, page, first, true).into_any_element()
    }

    /// The view is built: its segments are what Ctrl+A, Ctrl+C and Ctrl+F read.
    pub fn finish(self, owner: EntityId) {
        self.sel.borrow_mut().begin(&self.key, Rc::new(self.segs), owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(texts: &[&str]) -> Sel {
        let mut s = Sel::default();
        let mut segs = Segs::default();
        for t in texts {
            segs.push(SharedString::from(t.to_string()), "\n");
        }
        s.source = Rc::new(segs);
        s
    }

    #[test]
    fn selection_runs_across_segments_and_copies_with_joins() {
        let mut s = sel(&["Hello world", "Second line", "Third"]);
        s.anchor = Some((Pos::new(0, 6), Pos::new(0, 6)));
        s.head = Some(Pos::new(2, 3));
        assert_eq!(s.selected_text().as_deref(), Some("world\nSecond line\nThi"));
        // Backwards gives the same.
        s.anchor = Some((Pos::new(2, 3), Pos::new(2, 3)));
        s.head = Some(Pos::new(0, 6));
        assert_eq!(s.selected_text().as_deref(), Some("world\nSecond line\nThi"));
    }

    #[test]
    fn select_all_and_empty() {
        let mut s = sel(&["a", "", "bc"]);
        assert!(s.selected_text().is_none());
        s.select_all();
        assert_eq!(s.selected_text().as_deref(), Some("a\n\nbc"));
        let mut e = sel(&[]);
        e.select_all();
        assert!(e.selected_text().is_none());
    }

    #[test]
    fn words_and_lines_extend_by_whole_units() {
        let mut s = sel(&["alpha beta gamma", "delta"]);
        let w = word_at("alpha beta gamma", 7);
        assert_eq!(w, 6..10);
        s.unit = Unit::Word;
        s.anchor = Some((Pos::new(0, 6), Pos::new(0, 10)));
        s.head = Some(Pos::new(0, 13));
        assert_eq!(s.selected_text().as_deref(), Some("beta gamma"));
        s.head = Some(Pos::new(0, 2));
        assert_eq!(s.selected_text().as_deref(), Some("alpha beta"));
        s.unit = Unit::Line;
        s.anchor = Some((Pos::new(0, 0), Pos::new(0, 16)));
        s.head = Some(Pos::new(1, 1));
        assert_eq!(s.selected_text().as_deref(), Some("alpha beta gamma\ndelta"));
    }

    #[test]
    fn a_new_key_starts_afresh() {
        let mut s = sel(&["one"]);
        s.select_all();
        let mut segs = Segs::default();
        segs.push("two".into(), "\n");
        let src: Rc<dyn Source> = Rc::new(segs);
        let id = EntityId::from(1u64);
        s.begin("b", src.clone(), id);
        assert!(s.selected_text().is_none() && !s.active);
        s.select_all();
        s.begin("b", src, id);
        assert_eq!(s.selected_text().as_deref(), Some("two"));
    }

    #[test]
    fn slices_keep_to_characters() {
        assert_eq!(slice("héllo", 0, 2), "hé");
        assert_eq!(slice("héllo", 2, 99), "éllo");
    }

    #[test]
    fn whole_segments_and_everything_copy_what_they_stand_for() {
        let mut s = Sel::default();
        let mut segs = Segs::default();
        segs.push("{".into(), "\n  ");
        segs.push("\"a\": {…}".into(), "\n");
        segs.push("}".into(), "\n");
        segs.whole.insert(1, "\"a\": {\n    \"b\": 1\n  }".into());
        segs.all = Some(Box::new(|| "EVERYTHING".into()));
        s.source = Rc::new(segs);
        // The folded node selected whole: all of it.
        s.anchor = Some((Pos::new(0, 0), Pos::new(0, 0)));
        s.head = Some(Pos::new(1, "\"a\": {…}".len()));
        assert_eq!(s.selected_text().as_deref(), Some("{\n  \"a\": {\n    \"b\": 1\n  }"));
        // Part of it: what it shows.
        s.head = Some(Pos::new(1, 4));
        assert_eq!(s.selected_text().as_deref(), Some("{\n  \"a\":"));
        s.select_all();
        assert_eq!(s.selected_text().as_deref(), Some("EVERYTHING"));
    }

    #[test]
    fn table_cells_copy_as_tab_separated_rows() {
        let rows = Arc::new(vec![vec!["Name".to_string(), "Age".into()], vec!["Bo".into(), "7".into()], vec!["Al".into(), "30".into()]]);
        // Sorted by name: Al before Bo.
        let cells = Cells { rows, order: Arc::new(vec![2, 1]), cols: 2, head: true };
        assert_eq!(cells.count(), 6);
        assert_eq!(cells.text(Cells::seg(true, 2, 0, 0)), "Al");
        assert_eq!(cells.item(Cells::seg(true, 2, 1, 1)), Some(1));
        assert_eq!(cells.item(0), None);
        let mut s = Sel::default();
        s.source = Rc::new(cells);
        s.select_all();
        assert_eq!(s.selected_text().as_deref(), Some("Name\tAge\nAl\t30\nBo\t7"));
    }

    #[test]
    fn finding_ignores_case_and_accents() {
        let q = crate::sortgroup::fold("cafe");
        assert_eq!(find_in("Le CAFÉ et le café", &q), vec![3..8, 15..20]);
        assert!(find_in("caf", &q).is_empty());
        let mut s = sel(&["Alpha beta", "no", "BETA beta"]);
        s.find = Some(Find { input: None, _sub: None, query: Vec::new(), hits: Vec::new(), current: None, stamp: Default::default() });
        s.find_text("beta");
        assert_eq!(s.find_status(), Some((1, 3)));
        s.find_step(true);
        assert_eq!(s.find_status(), Some((3, 3)));
        s.find_step(false);
        s.find_step(false);
        assert_eq!(s.find_status(), Some((2, 3)));
        // The text changes (a node opened): found again, the current one kept.
        let mut segs = Segs::default();
        for t in ["new", "Alpha beta", "no", "BETA beta"] {
            segs.push(SharedString::from(t), "
");
        }
        let src: Rc<dyn Source> = Rc::new(segs);
        s.begin("", src, EntityId::from(1u64));
        assert_eq!(s.find_status(), Some((2, 3)));
        s.close_find();
        assert!(s.find_status().is_none());
    }

    #[test]
    fn turned_marks_follow_the_line() {
        use crate::pdftext::Turn;
        // A line running straight down a square page from (0.5, 0.1), 0.4 long.
        let turn = Turn { angle: std::f32::consts::FRAC_PI_2, aspect: 1., start: 0.1, end: 0.5, top: -0.52, bottom: -0.48 };
        let l = PdfLine { text: "abcd".into(), chars: vec![(0, 0.1, 0.2), (1, 0.2, 0.3), (2, 0.3, 0.4), (3, 0.4, 0.5)], left: 0.48, right: 0.52, top: 0.1, bottom: 0.5, row_goes_on: false, turn: Some(turn) };
        let text = Arc::new(PdfText { pages: vec![vec![l]] });
        let at = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        let geom = Geom::Pdf { text, page: 0, line: 0, at };
        // "bc": from 20 to 40 px down, around x = 50.
        let m = marks_of(&geom, 1..3, false);
        let b = m[0].bounds();
        assert!((b.top().as_f32() - 20.).abs() < 0.5 && (b.bottom().as_f32() - 40.).abs() < 0.5, "{b:?}");
        assert!((b.left().as_f32() - 48.).abs() < 0.5 && (b.right().as_f32() - 52.).abs() < 0.5, "{b:?}");
        // A press 35 px down is in "c".
        assert_eq!(index_at(&geom, point(px(50.), px(33.))), 2);
        assert!(geom.holds(point(px(50.), px(30.))) && !geom.holds(point(px(70.), px(30.))));
    }
}
