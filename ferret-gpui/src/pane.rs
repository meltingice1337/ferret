//! A pane: one folder in a tab (a split tab has two), with its own history, view, filter and
//! selection, as src/lib/state.ts's Tab. Rows are worked out off the UI thread: sorted, grouped,
//! filtered, with what the list shows already formatted.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Instant;

use ferret_core::listing::Entry;
use gpui::{ListAlignment, ListState, SharedString, UniformListScrollHandle, px};

use crate::kinds::{self, Kind};
use crate::sortgroup::{self, Group, GroupBy, Mode, ViewSettings};

/// One file or folder, with what the list shows already worked out.
pub struct Row {
    pub e: Entry,
    pub kind: Kind,
    pub name: SharedString,
    pub date: SharedString,
    pub ty: SharedString,
    pub size: SharedString,
    pub folded: Box<[char]>,
    /// A folder's counted size, once known (sorting by size uses it).
    pub dir_bytes: Option<u64>,
}

impl sortgroup::Item for Row {
    fn is_dir(&self) -> bool { self.e.is_dir }
    fn name(&self) -> &str { &self.e.name }
    fn folded(&self) -> &[char] { &self.folded }
    fn modified(&self) -> Option<i64> { self.e.modified }
    fn created(&self) -> Option<i64> { self.e.created }
    fn size(&self) -> u64 { if self.e.is_dir { self.dir_bytes.unwrap_or(0) } else { self.e.size } }
    fn type_label(&self) -> &str { &self.ty }
    fn kind_label(&self) -> &str { kinds::group_label(self.kind) }
}

impl Row {
    pub fn new(e: Entry, bin: bool) -> Self {
        let kind = kinds::kind_of(e.is_dir, &e.ext);
        Row {
            kind,
            name: e.name.clone().into(),
            date: kinds::format_date(e.modified).into(),
            ty: kinds::type_label(e.is_dir, &e.ext).into(),
            size: if e.is_dir { SharedString::default() } else { kinds::format_size(e.size).into() },
            folded: sortgroup::fold(&e.name).into_boxed_slice(),
            dir_bytes: None,
            e: if bin { e } else { e },
        }
    }
}

/// What the file view lays out: group headings, list rows, grid rows (a run of cells).
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Header(usize),
    Row(usize),
    Cells(std::ops::Range<usize>),
}

pub const HEADER_H: f32 = 34.;

/// A folder's rows after the view (hidden items, filter, sort, groups), worked out in the background.
pub struct Prepared {
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
    pub hidden: usize,
    pub folders: usize,
    /// Items before the filter (what "M of N" counts).
    pub unfiltered: usize,
    /// The 8 most recently changed items, newest first (indexes into `rows`).
    pub recent: Vec<usize>,
}

/// Whether hidden items show, in every folder at once like Explorer's "Hidden items" box: one
/// setting ("showHiddenItems", on unless turned off), not part of a folder's remembered view.
/// Kept here so lists prepared in the background read it too.
static SHOW_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn show_hidden() -> bool {
    SHOW_HIDDEN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_show_hidden(on: bool) {
    SHOW_HIDDEN.store(on, std::sync::atomic::Ordering::Relaxed)
}

pub fn prepare(entries: &[Entry], view: &ViewSettings, filter: &str, bin: bool, sizes: &dyn Fn(&str) -> Option<u64>) -> Prepared {
    let hidden = entries.iter().filter(|e| e.hidden).count();
    let matcher = crate::filter::Filter::parse(filter);
    let mut unfiltered = 0;
    let mut rows: Vec<Row> = entries
        .iter()
        .filter(|e| show_hidden() || !e.hidden)
        .filter(|_| {
            unfiltered += 1;
            true
        })
        .filter(|e| matcher.as_ref().is_none_or(|m| m.matches(e)))
        .map(|e| {
            let mut r = Row::new(e.clone(), bin);
            if e.is_dir {
                r.dir_bytes = sizes(&e.path);
            }
            r
        })
        .collect();
    // In the Recycle Bin the newest deletions come first, folders or not.
    let v = if bin { ViewSettings { folders_first: false, ..view.clone() } } else { view.clone() };
    sortgroup::sort(&mut rows, &v);
    let (rows, groups) = sortgroup::group(rows, &v);
    let folders = rows.iter().filter(|r| r.e.is_dir).count();
    let mut recent: Vec<usize> = (0..rows.len()).collect();
    recent.sort_by_key(|&i| std::cmp::Reverse(rows[i].e.modified.unwrap_or(0)));
    recent.truncate(8);
    Prepared { rows, groups, hidden, folders, unfiltered, recent }
}

/// Which way the last move went: the list slides in from that side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nav {
    Into,
    Out,
    Other,
}

pub struct Pane {
    pub id: u64,
    pub path: String,
    /// Everything the folder holds, as listed.
    pub entries: Arc<Vec<Entry>>,
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
    /// What the view lays out (headings, rows or grid rows), and each one's top.
    pub items: Vec<Item>,
    pub tops: Vec<f32>,
    pub height: f32,
    /// Where each row is in `items`.
    pub item_of: Vec<usize>,
    pub collapsed: HashSet<String>,
    pub view: ViewSettings,
    /// Where the view came from: "folder" | "inherited" | "profile" | "global".
    pub source: String,
    pub source_path: Option<String>,
    pub profile: Option<String>,
    /// The view was changed here and isn't saved for the folder (the "Save view" button).
    pub view_changed: bool,
    pub hidden: usize,
    pub folders: usize,
    pub unfiltered: usize,
    pub recent: Vec<usize>,
    /// The cursor (what the preview shows), every selected item, and where a Shift range starts.
    pub selected: Option<usize>,
    pub marked: BTreeSet<usize>,
    pub anchor: usize,
    pub error: Option<String>,
    pub loading: bool,
    pub loaded_once: bool,
    pub back: Vec<String>,
    pub fwd: Vec<String>,
    pub token: u64,
    pub scroll: UniformListScrollHandle,
    pub list: ListState,
    pub cols: usize,
    pub width: f32,
    /// Where the list was drawn last frame (rubber band, grid columns), set while painting.
    pub bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// The folder filter (Ctrl+F): its text, and whether its bar is open.
    pub filter: String,
    pub filter_open: bool,
    /// Items to select once the folder is listed (by path).
    pub pending_select: Option<Vec<String>>,
    /// Rename the (first) pending item once listed: a new folder goes straight into rename.
    pub pending_rename: bool,
    pub nav: Nav,
    pub nav_seq: u64,
    /// Type-to-jump: the letters typed, and when the last one was.
    pub typed: (String, Instant),
    /// When this folder was opened (it counts as recent after 3 s).
    pub opened_at: Instant,
}

impl Pane {
    pub fn new(id: u64, path: String) -> Self {
        Pane {
            id,
            path,
            entries: Arc::new(Vec::new()),
            rows: Vec::new(),
            groups: Vec::new(),
            items: Vec::new(),
            tops: Vec::new(),
            height: 0.,
            item_of: Vec::new(),
            collapsed: HashSet::new(),
            view: ViewSettings::default(),
            source: "global".into(),
            source_path: None,
            profile: None,
            view_changed: false,
            hidden: 0,
            folders: 0,
            unfiltered: 0,
            recent: Vec::new(),
            selected: None,
            marked: BTreeSet::new(),
            anchor: 0,
            error: None,
            loading: true,
            loaded_once: false,
            back: Vec::new(),
            fwd: Vec::new(),
            token: 0,
            scroll: UniformListScrollHandle::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            cols: 1,
            width: 0.,
            bounds: Default::default(),
            filter: String::new(),
            filter_open: false,
            pending_select: None,
            pending_rename: false,
            nav: Nav::Other,
            nav_seq: 0,
            typed: (String::new(), Instant::now()),
            opened_at: Instant::now(),
        }
    }

    pub fn is_bin(&self) -> bool {
        ferret_core::recycle::is_recycle(&self.path)
    }

    pub fn is_search(&self) -> bool {
        ferret_core::search::search_query_of(&self.path).is_some()
    }

    pub fn is_archive(&self) -> bool {
        ferret_core::archive::split_virtual(&self.path).is_some() && !std::path::Path::new(&self.path).is_dir()
    }

    /// Takes new rows, keeping the selection on the same items where they still exist.
    pub fn apply(&mut self, p: Prepared) {
        let keep: Vec<String> = self.marked.iter().filter_map(|&i| self.rows.get(i)).map(|r| r.e.path.clone()).collect();
        let cursor = self.selected.and_then(|i| self.rows.get(i)).map(|r| r.e.path.clone());
        self.rows = p.rows;
        self.groups = p.groups;
        self.hidden = p.hidden;
        self.folders = p.folders;
        self.unfiltered = p.unfiltered;
        self.recent = p.recent;
        let index: std::collections::HashMap<&str, usize> = self.rows.iter().enumerate().map(|(i, r)| (r.e.path.as_str(), i)).collect();
        self.marked = keep.iter().filter_map(|p| index.get(p.as_str()).copied()).collect();
        self.selected = cursor.and_then(|p| index.get(p.as_str()).copied());
        self.relayout();
    }

    /// Lays out headings and rows (or grid rows) for the current width and view.
    pub fn relayout(&mut self) {
        let grid = self.view.mode == Mode::Grid;
        let grouped = self.view.group_by != GroupBy::None;
        let ih = crate::theme::row_h();
        let gh = self.view.icon_size as f32 + 58.;
        let cols = if grid { self.grid_cols() } else { 1 };
        self.cols = cols;
        let mut items = Vec::new();
        let mut tops = Vec::new();
        let mut item_of = vec![usize::MAX; self.rows.len()];
        let mut y = 0.;
        for (gi, g) in self.groups.iter().enumerate() {
            if grouped {
                items.push(Item::Header(gi));
                tops.push(y);
                y += HEADER_H;
                if self.collapsed.contains(&g.label) {
                    continue;
                }
            }
            if grid {
                let mut i = g.start;
                while i < g.start + g.len {
                    let end = (i + cols).min(g.start + g.len);
                    for r in i..end {
                        item_of[r] = items.len();
                    }
                    items.push(Item::Cells(i..end));
                    tops.push(y);
                    y += gh;
                    i = end;
                }
            } else {
                for r in g.start..g.start + g.len {
                    item_of[r] = items.len();
                    items.push(Item::Row(r));
                    tops.push(y);
                    y += ih;
                }
            }
        }
        let changed = items.len() != self.items.len() || items != self.items;
        self.items = items;
        self.tops = tops;
        self.height = y;
        self.item_of = item_of;
        if changed {
            let at = self.list.logical_scroll_top();
            self.list.reset(self.items.len());
            if at.item_ix < self.items.len() {
                self.list.scroll_to(at);
            }
        }
    }

    /// Plain list with no groups: a uniform list (fastest, and what the benchmark measured).
    pub fn uniform(&self) -> bool {
        self.view.mode == Mode::List && self.view.group_by == GroupBy::None
    }

    pub fn cell_w(&self) -> f32 {
        self.view.icon_size as f32 + 34.
    }

    pub fn grid_cols(&self) -> usize {
        if self.width <= 0. {
            return 4;
        }
        (((self.width - 24.) / self.cell_w()).floor() as usize).max(1)
    }

    /// The visible rows in order (what arrows walk through): collapsed groups left out.
    pub fn visible_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.items.iter().flat_map(|it| match it {
            Item::Row(r) => *r..*r + 1,
            Item::Cells(range) => range.clone(),
            Item::Header(_) => 0..0,
        })
    }

    pub fn scroll_to(&self, row: usize) {
        if self.uniform() {
            self.scroll.scroll_to_item(row, gpui::ScrollStrategy::Nearest);
        } else if let Some(&it) = self.item_of.get(row) {
            if it == usize::MAX {
                return;
            }
            // GPUI's own "reveal" only knows the heights of items it has drawn (far ones count
            // as 0, so End in a big grid didn't move); every item's place is known here.
            let (Some(&top), Some(item)) = (self.tops.get(it), self.items.get(it)) else {
                self.list.scroll_to_reveal_item(it);
                return;
            };
            let h = self.item_height(item);
            let view_h = self.bounds.get().size.height.as_f32();
            let now = self.list.logical_scroll_top();
            let cur = self.tops.get(now.item_ix).copied().unwrap_or(0.) + now.offset_in_item.as_f32();
            if top < cur || view_h <= h {
                self.list.scroll_to(gpui::ListOffset { item_ix: it, offset_in_item: px(0.) });
            } else if top + h > cur + view_h {
                let goal = top + h - view_h;
                let j = self.tops.partition_point(|&t| t <= goal).saturating_sub(1);
                self.list.scroll_to(gpui::ListOffset { item_ix: j, offset_in_item: px(goal - self.tops[j]) });
            }
        }
    }

    /// An item's height in the list (headings, rows, a grid row of cells).
    pub fn item_height(&self, item: &Item) -> f32 {
        match item {
            Item::Header(_) => HEADER_H,
            Item::Row(_) => crate::theme::row_h(),
            Item::Cells(_) => self.view.icon_size as f32 + 58.,
        }
    }

    pub fn scroll_top(&self) {
        if self.uniform() {
            self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        } else {
            self.list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.) });
        }
    }

    pub fn marked_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = self.marked.iter().filter_map(|&i| self.rows.get(i)).map(|r| r.e.path.clone()).collect();
        if out.is_empty() {
            out.extend(self.selected.and_then(|i| self.rows.get(i)).map(|r| r.e.path.clone()));
        }
        out
    }

    pub fn marked_rows(&self) -> Vec<&Row> {
        let rows: Vec<&Row> = self.marked.iter().filter_map(|&i| self.rows.get(i)).collect();
        if rows.is_empty() { self.selected.and_then(|i| self.rows.get(i)).into_iter().collect() } else { rows }
    }

    pub fn cursor_row(&self) -> Option<&Row> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    pub fn index_of(&self, path: &str) -> Option<usize> {
        self.rows.iter().position(|r| crate::util::same(&r.e.path, path))
    }
}

/// A tab: a pane, and a second one when split.
pub struct Tab {
    pub id: u64,
    pub main: Pane,
    pub split: Option<Pane>,
    /// The split pane has the keyboard.
    pub split_focused: bool,
}

impl Tab {
    pub fn pane(&self) -> &Pane {
        match (&self.split, self.split_focused) {
            (Some(s), true) => s,
            _ => &self.main,
        }
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        match (&mut self.split, self.split_focused) {
            (Some(s), true) => s,
            _ => &mut self.main,
        }
    }

    pub fn other(&self) -> Option<&Pane> {
        let s = self.split.as_ref()?;
        Some(if self.split_focused { &self.main } else { s })
    }

    pub fn by_id(&mut self, id: u64) -> Option<&mut Pane> {
        if self.main.id == id {
            return Some(&mut self.main);
        }
        self.split.as_mut().filter(|s| s.id == id)
    }

    pub fn has(&self, id: u64) -> bool {
        self.main.id == id || self.split.as_ref().is_some_and(|s| s.id == id)
    }
}
