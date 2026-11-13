//! The window: tabs (each one or two panes), sidebar, toolbar, file view, preview pane, status
//! bar, menus, dialogs and toasts, drawn after src/styles.css. Everything it shows comes straight
//! from ferret-core: no web view, no IPC, no JSON between them.
//!
//! The parts live in their own files as more `impl Ferret` blocks: chrome.rs (tab strip, toolbar,
//! status bar), files.rs (list and grid), sidebar.rs, preview.rs, ops.rs (file operations, undo),
//! menu.rs (context menus), dialogs.rs, settings_view.rs.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferret_core::places::Places;
use ferret_core::{browse, db, paths};
use gpui::AnimationExt;
use gpui::{
    AnyElement, App, AsyncApp, Context, Entity, FocusHandle, Image, ImageFormat, KeyDownEvent, ObjectFit, SharedString, StyleRefinement,
    Subscription, WeakEntity, Window, div, prelude::*, px,
};

use crate::imgmem::img;
use serde_json::json;

use crate::bench::{self, Bench};
use crate::docx;
use crate::editor::{Editor, EditorEvent};
use crate::input::TextInput;
use crate::link;
use crate::pane::{self, Nav, Pane, Tab};
use crate::palette::{Command, Palette, PaletteEvent};
use crate::settings::Settings;
use crate::sortgroup::{GroupBy, Mode, SortBy, SortDir, ViewSettings};
use crate::theme as t;
use crate::util::{self, same};
use crate::videoview::{VideoEvent, VideoView};

/// What a command, a menu item or a toast button does.
pub type Action = Rc<dyn Fn(&mut Ferret, &mut Window, &mut Context<Ferret>)>;

/// The session (tabs, panes) written off the UI thread, the latest one only. It's saved at every
/// folder opened, and the database is shared with folder-size counting, whose work can hold it
/// for a while: the window doesn't wait for that.
/// The same recent folders in the same order (what the sidebar shows of them).
fn same_recents(a: &[db::Recent], b: &[db::Recent]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.path == b.path)
}

fn save_session_soon(session: serde_json::Value) {
    static PENDING: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);
    let start = {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let idle = p.is_none();
        *p = Some(session);
        idle
    };
    if start {
        std::thread::spawn(|| {
            loop {
                let next = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
                let Some(s) = next else { break };
                let t = Instant::now();
                let _ = browse::save_session(s);
                if t.elapsed() >= Duration::from_millis(250) {
                    log::info!(target: "ferret::perf", "saving the session waited {} ms for the database", t.elapsed().as_millis());
                }
            }
        });
    }
}

pub fn action(f: impl Fn(&mut Ferret, &mut Window, &mut Context<Ferret>) + 'static) -> Action {
    Rc::new(f)
}

/// One job as ferret-core reports it (`job-update`).
pub struct JobCard {
    pub id: u64,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub done: u64,
    pub total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub current: String,
    pub note: String,
    pub question: Option<String>,
    pub error: Option<String>,
    pub started: Instant,
    pub current_since: Instant,
    pub finished_at: Option<Instant>,
    /// Shown once it has run 0.6 s (quick jobs never show a card).
    pub shown: bool,
}

pub struct Toast {
    pub id: u64,
    pub text: SharedString,
    pub error: bool,
    pub action: Option<(SharedString, Action)>,
    pub at: Instant,
}

/// A part of the window drawn as a cached view: GPUI reuses its last frame until the window's
/// state changes (it observes the window). When the video's controls change (videoview.rs), the
/// root and the video's view are drawn again, never the sidebar or the file lists.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Sidebar,
    /// The active tab's first pane, or its second when split.
    Files(bool),
}

pub struct Part {
    root: WeakEntity<Ferret>,
    which: Which,
    _observe: Subscription,
}

impl Part {
    fn new(root: &Entity<Ferret>, which: Which, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(root, |_, _, cx| cx.notify());
        Self { root: root.downgrade(), which, _observe: observe }
    }
}

impl Render for Part {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let which = self.which;
        bench::rendered(if which == Which::Sidebar { bench::SIDEBAR } else { bench::FILES });
        let Some(root) = self.root.upgrade() else { return div().into_any_element() };
        root.update(cx, |f, cx| match which {
            Which::Sidebar => f.sidebar(window, cx).into_any_element(),
            Which::Files(second) => f.pane_view(second, window, cx).into_any_element(),
        })
    }
}

/// A row pressed: it may become a drag (moved 6 px), a click, or a rename (a slow second click).
pub struct Press {
    pub pane: u64,
    pub row: usize,
    pub at: gpui::Point<gpui::Pixels>,
}

/// The rubber band: where it started (in the list's content), where the mouse is, and what was
/// selected before (Ctrl adds to it).
pub struct Band {
    pub pane: u64,
    pub from: gpui::Point<gpui::Pixels>,
    pub to: gpui::Point<gpui::Pixels>,
    pub base: std::collections::BTreeSet<usize>,
}

/// Middle-click autoscroll: the list, where it started, where the mouse is, when, and whether it
/// follows the mouse until the next click (a quick click) rather than while the wheel is held.
pub struct AutoScroll {
    pub pane: u64,
    pub origin: gpui::Point<gpui::Pixels>,
    pub at: gpui::Point<gpui::Pixels>,
    pub began: Instant,
    pub sticky: bool,
}

/// Something being dragged to resize: the sidebar, the preview pane, or a list column.
#[derive(Clone, Copy)]
pub enum Resize {
    Sidebar { from: f32, start: f32 },
    Preview { from: f32, start: f32 },
    Column { col: crate::settings::Column, from: f32, start: f32 },
}

/// Inline rename: which pane and item, its field, and what's wrong with the name so far.
pub struct Rename {
    pub pane: u64,
    pub path: String,
    pub is_dir: bool,
    pub input: Entity<TextInput>,
    pub error: Option<&'static str>,
    pub _sub: Subscription,
}

pub struct PathEdit {
    pub input: Entity<TextInput>,
    pub suggestions: Vec<String>,
    pub pick: Option<usize>,
    pub _sub: Subscription,
}

pub struct FilterBox {
    pub pane: u64,
    pub input: Entity<TextInput>,
    pub _sub: Subscription,
}

/// A thumbnail: being made, made, or not available.
pub enum Thumb {
    Loading,
    Ready(Arc<Image>),
    None,
}

pub struct Ferret {
    pub places: Places,
    pub favorites: Vec<db::Favorite>,
    pub recents: Vec<db::Recent>,
    pub settings: Settings,
    /// The Recycle Bin's count and size (sidebar badge, tooltip).
    pub bin: Option<ferret_core::recycle::RecycleInfo>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// Closed tabs (Ctrl+Shift+T): where they were, their folder and split.
    pub closed: Vec<(usize, String, Option<String>)>,
    pub next_id: u64,
    pub focus: FocusHandle,
    pub glow: Arc<Image>,
    glow_key: (u32, bool),
    pub bench: Option<Bench>,
    parts: Option<(Entity<Part>, Entity<Part>, Entity<Part>)>,
    pub press: Option<Press>,
    pub band: Option<Band>,
    /// The mouse, while drawing the band (the edge auto-scroll uses it), and whether that runs.
    pub band_at: gpui::Point<gpui::Pixels>,
    pub band_scrolling: bool,
    pub autoscroll: Option<AutoScroll>,
    /// The tab files are being dragged over, and since when (held there, it switches to it).
    pub tab_hover: Option<(usize, Instant)>,
    /// The tab strip's scrolling, and the (tab, count) last scrolled into view.
    pub tab_scroll: gpui::ScrollHandle,
    pub tab_scroll_seen: std::cell::Cell<(usize, usize)>,
    /// The tab bar's width as last drawn, which the tabs share out (`tab_widths`).
    pub tab_bar_w: std::rc::Rc<std::cell::Cell<f32>>,
    /// Where the "all tabs" button was drawn (its menu opens under it).
    pub tab_list_at: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    pub resizing: Option<Resize>,
    pub menu: Option<crate::menu::Menu>,
    pub dialog: Option<crate::dialogs::Dialog>,
    pub toasts: Vec<Toast>,
    pub toast_seq: u64,
    pub jobs: Vec<JobCard>,
    /// When a path in the preview's details was last copied (preview.rs `note_copied`).
    pub meta_copied: Option<Instant>,
    /// Self-update (updateview.rs).
    pub update: crate::updateview::UpdateState,
    /// The selected video's player (its own view).
    pub video: Option<(Entity<VideoView>, Subscription)>,
    /// The video shown last, as a still: on screen while what comes next is on its way, then
    /// fading out under it (`preview::VideoStill`); and whether a task is watching to let it go.
    pub video_still: Option<crate::preview::VideoStill>,
    still_watched: bool,
    /// The selected Word document: its path, and the document once read (off the UI thread).
    pub word: Option<(String, Option<Result<Arc<docx::Doc>, String>>)>,
    /// The document of the selection before, kept while the next item is read (preview.rs `held`).
    pub word_before: Option<(String, Arc<docx::Doc>)>,
    pub palette: Option<(Entity<Palette>, Subscription)>,
    pub palette_actions: Vec<Action>,
    pub preview_open: bool,
    pub preview_w: f32,
    pub sidebar_w: f32,
    pub sidebar_hidden: bool,
    /// Below 860px the sidebar is an overlay, open or not.
    pub sidebar_overlay: bool,
    pub collapsed_sections: HashSet<String>,
    pub path_edit: Option<PathEdit>,
    pub filter_box: Option<FilterBox>,
    pub rename: Option<Rename>,
    /// Items cut to the clipboard (shown dimmed until pasted or something else is copied).
    pub cut: HashSet<String>,
    pub clip_seq: u32,
    /// A server's files copied or cut in this window (paths, cut, Windows' clipboard count then).
    pub server_clip: Option<(Vec<String>, bool, u32)>,
    pub undo: Vec<crate::ops::Step>,
    pub redo: Vec<crate::ops::Step>,
    /// Counted folder sizes, by `paths::key`.
    pub sizes: HashMap<String, ferret_core::foldersize::FolderSize>,
    pub thumbs: crate::imgmem::Thumbs,
    pub thumbs_running: usize,
    pub thumb_queue: Vec<(String, String, u32)>,
    pub quicklook: bool,
    /// The last click on a row, for slow-click rename and double clicks.
    pub last_click: Option<(u64, usize, Instant)>,
    /// The compress editor, while open.
    pub editor: Option<(Entity<Editor>, Subscription)>,
    /// The selected folder's own listing, for its preview (pictures, recently changed, counts);
    /// `None` inside while it's being read.
    pub folder_peek: Option<(String, Option<Result<Rc<crate::preview::Peek>, String>>)>,
    /// The folder selected before that one, once read (see `peek_folder`).
    pub folder_peek_before: Option<(String, Option<Result<Rc<crate::preview::Peek>, String>>)>,
    /// FERRET_GPUI_EDITOR: open the editor once the selected video is ready (tests).
    editor_on_start: bool,
    /// The folders being watched (changes reload them).
    watched: Vec<String>,
    title: String,
    /// The Dropzone's items.
    pub shelf: Vec<String>,
    /// A press on a Dropzone item that may become a drag out.
    pub shelf_press: Option<(Vec<String>, gpui::Point<gpui::Pixels>)>,
    pub elevated: bool,
    /// The preview: what it waits to read (after 140 ms), what it read, and its options.
    pub preview_want: Option<(String, Instant)>,
    pub preview_data: Option<crate::preview::Loaded>,
    /// The item the preview is for, and since when (what it showed before stays that long: see
    /// `preview::HOLD`), and the item a redraw at the end of that time is booked for.
    pub preview_since: Option<(String, Instant)>,
    pub preview_hold_wake: Option<String>,
    /// The fade bringing another item's content in (the pane's, Quick Look's).
    pub preview_fade: [Option<crate::preview::Fade>; 2],
    pub fade_seq: usize,
    /// The folder summary just drawn is a spinner: its listing is still being read.
    pub summary_pending: bool,
    /// The picture the preview last drew (the pane's, Quick Look's) and at which frame, kept on
    /// screen while the next one's stand-in or decoded copy isn't there yet; and the item whose
    /// content it last drew. Only what the frame before showed is kept on (not something from
    /// before the pane was closed or showed a folder).
    pub held_pic: [Option<(Arc<Image>, (u32, u32), u64)>; 2],
    pub preview_showing: [Option<(crate::preview::Shown, u64)>; 2],
    /// The pictures next to the one previewed: their thumbnails are decoded ahead.
    pub preview_near: Vec<String>,
    /// When the preview last started waiting for another item (stepping fast reads none on the way).
    pub preview_stepped: Option<Instant>,
    /// Pictures Quick Look is reading ahead (their preview keys).
    pub warming: std::collections::HashSet<String>,
    /// Files being dragged over the window: what, where the pointer is, and the drop target
    /// under it (its path and how specific it is), for the label the app shows.
    pub drag_hint: Option<DragHint>,
    /// The drag over the window is one this window started (its label says copy or move).
    pub own_drag: bool,
    /// Paths file operations touched since their jobs started: the folders holding them are
    /// counted again when a job ends (`job_update`, `sizes_changed`).
    pub touched_sizes: Vec<String>,
    /// The path box's latest suggestion request (an older one, overtaken while typing, is dropped).
    pub suggest_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Folders on screen still waiting for their size or being counted (ferret-core's report).
    pub sizes_left: usize,
    /// A scrollbar thumb being dragged: the pane, and where on the thumb it was grabbed (px).
    pub thumb_drag: Option<(u64, f32)>,
    /// A re-sort by size is waiting (see `resort_by_size`), and when the last one ran.
    size_sort_due: bool,
    size_sorted_at: Instant,
    /// Previews read lately (PDFs drawn for the pane and for Quick Look), to swap back without reading again.
    pub preview_cache: Vec<crate::preview::Loaded>,
    pub preview_view: crate::render::View,
    pub picture_bounds: [std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>; 2],
    /// The PDF preview's scrolling (the pane's, Quick Look's), and whether more pages are being drawn.
    pub pdf_scroll: [gpui::ScrollHandle; 2],
    pub pdf_pending: bool,
    /// The selected text in the preview's text (the pane's, Quick Look's): selectable.rs.
    pub text_sel: [crate::selectable::Shared; 2],
    /// Preview text copied last (tests read it from `state`; the clipboard may be off).
    pub text_copied: Option<String>,
    /// A PDF that wants a password: its field in the preview (preview.rs).
    pub pdf_unlock: Option<crate::preview::PdfUnlock>,
    /// The previewed PDF's text layer: the file read, and its text once extracted.
    pub pdf_text: Option<(String, Option<Arc<crate::pdftext::PdfText>>)>,
    /// Reading the shown PDF's scanned pages (ocr.rs), if it has any.
    pub ocr: Option<crate::ocr::OcrView>,
    /// The audio seek bar, measured so a click maps to a time.
    pub audio_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// An audio file playing in the preview.
    pub audio: Option<(String, crate::video::Player)>,
    /// The audio preview's volume (0–1) and mute, for this session.
    pub audio_volume: f32,
    pub audio_muted: bool,
    pub volume_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    pub settings_scroll: gpui::ScrollHandle,
    pub folder_views: Vec<db::FolderView>,
    /// Settings' list of remembered folder views is shown (this window's session; starts folded).
    pub folder_views_open: bool,
    /// The Dropzone's drop slot as last drawn (sidebar.rs `dropzone`), for its animations.
    pub dz_slot: std::cell::Cell<crate::sidebar::DzSlot>,
    pub default_status: Option<ferret_core::integration::DefaultStatus>,
    /// Settings changed the window backdrop: applied at the next frame.
    pub backdrop_changed: bool,
    /// The dark mode last given to the window's Mica (None: give it again).
    pub dwm_dark: Option<bool>,
    /// A network folder waiting for its sign-in.
    pub network_pending: Option<String>,
    /// Ctrl+K's last text, and when it closed (reopened within 8 s, it comes back).
    pub last_query: Option<(String, Instant)>,
    /// A search Ctrl+K asked to save: cancelling its naming reopens Ctrl+K with this.
    pub palette_back: Option<String>,
    /// A field to give the keyboard to as the window draws next.
    pub pending_focus: Option<FocusHandle>,
    /// Letters typed while a new folder or file is being made (they become its name).
    pub type_ahead: Option<(String, Instant)>,
    /// The window's scale and width, as of its last frame (PDF pages are drawn to fit them).
    pub scale: f32,
    pub viewport_w: f32,
    /// Where the keyboard is (F6 steps through the areas).
    pub kbd: Area,
    /// The sidebar's items in order, as last drawn (the keyboard steps through them), where the
    /// current section's items start, and which one shows the keyboard's ring.
    pub side_targets: std::cell::RefCell<Vec<String>>,
    pub side_mark: std::cell::Cell<usize>,
    pub side_ring: std::cell::Cell<Option<usize>>,
}

/// Where the keyboard is while the window (not a text field) has it: the files, or one of the
/// areas F6 / Shift+F6 step through, at an item there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Area {
    #[default]
    List,
    Tabs(usize),
    Crumbs(usize),
    Sidebar(usize),
    Preview,
    /// The list's column headings (Shift+Tab from the files): Enter sorts, Shift+←/→ resizes.
    Heads(usize),
}

/// GPUI's window, for the drag crate and shell menus (they only need the Win32 handle).
pub struct Hwnd(pub std::num::NonZeroIsize);

impl raw_window_handle::HasWindowHandle for Hwnd {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(self.0));
        // SAFETY: the window outlives the drag (DoDragDrop returns before the window can close).
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

pub fn hwnd(window: &Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// The window's immersive dark mode (Mica's tone, the system parts of the frame).
fn set_dwm_dark(hwnd: isize, dark: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let value = windows::core::BOOL::from(dark);
    unsafe {
        let _ = DwmSetWindowAttribute(HWND(hwnd as *mut _), DWMWA_USE_IMMERSIVE_DARK_MODE, &value as *const _ as _, std::mem::size_of_val(&value) as u32);
    }
}

/// F1–F24.
fn fkey(key: &str) -> bool {
    key.len() >= 2 && key.starts_with('f') && key[1..].chars().all(|c| c.is_ascii_digit())
}

/// Where a window is on screen (physical pixels; its normal size if maximized), for the
/// elevated copy that replaces it.
pub fn placement_of(hwnd: isize, maximized: bool) -> Option<ferret_core::integration::Placement> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, GetWindowRect, WINDOWPLACEMENT};
    let h = HWND(hwnd as *mut _);
    let r = if maximized {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe { GetWindowPlacement(h, &mut wp) }.ok()?;
        wp.rcNormalPosition
    } else {
        let mut r = RECT::default();
        unsafe { GetWindowRect(h, &mut r) }.ok()?;
        r
    };
    Some(ferret_core::integration::Placement { x: r.left, y: r.top, w: (r.right - r.left).max(0) as u32, h: (r.bottom - r.top).max(0) as u32, maximized })
}

/// A window restarted as administrator goes exactly where the one it replaces was.
fn apply_placement(hwnd: isize, p: ferret_core::integration::Placement) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow};
    let h = HWND(hwnd as *mut _);
    unsafe {
        // Twice: crossing onto a monitor with another scale makes Windows rescale the window on
        // the way (WM_DPICHANGED); the second move, on the right monitor, sets the exact size.
        let _ = SetWindowPos(h, None, p.x, p.y, p.w as i32, p.h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = SetWindowPos(h, None, p.x, p.y, p.w as i32, p.h as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        if p.maximized {
            let _ = ShowWindow(h, SW_MAXIMIZE);
        }
    }
}

impl Ferret {
    pub fn new(
        bench: Option<Bench>,
        mut core_events: futures::channel::mpsc::UnboundedReceiver<link::Msg>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // ferret-core's events (job progress, folder changes, sizes…) and UI-thread work.
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(msg) = core_events.next().await {
                match msg {
                    link::Msg::Run(f) => f(),
                    link::Msg::Event(name, payload) => {
                        if this.update(cx, |v, cx| v.core_event(&name, &payload, cx)).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .detach();
        let settings = Settings::load();
        crate::pane::set_show_hidden(settings.bool("showHiddenItems", true));
        settings.apply_theme();
        let session = ferret_core::app::with_db(|c| db::kv_get(c, "session")).flatten();
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut this = Self {
            places: Places { home: home.clone(), known: Vec::new(), drives: Vec::new(), wsl: Vec::new() },
            favorites: ferret_core::app::with_db(|c| db::favorites(c).unwrap_or_default()).unwrap_or_default(),
            recents: db::recents_present(ferret_core::app::with_db(|c| db::recents(c, db::RECENTS_KEPT).unwrap_or_default()).unwrap_or_default(), 12),
            bin: None,
            tabs: Vec::new(),
            active: 0,
            closed: Vec::new(),
            next_id: 1,
            focus,
            glow: Arc::new(Image::from_bytes(ImageFormat::Png, t::glow_png(320, 200))),
            glow_key: (t::accent_hex(), t::is_dark()),
            bench,
            parts: None,
            press: None,
            band: None,
            band_at: Default::default(),
            band_scrolling: false,
            autoscroll: None,
            tab_hover: None,
            tab_scroll: gpui::ScrollHandle::new(),
            tab_scroll_seen: std::cell::Cell::new((usize::MAX, 0)),
            tab_bar_w: Default::default(),
            tab_list_at: Default::default(),
            resizing: None,
            menu: None,
            dialog: None,
            toasts: Vec::new(),
            toast_seq: 0,
            jobs: Vec::new(),
            meta_copied: None,
            update: Default::default(),
            video: None,
            video_still: None,
            still_watched: false,
            word: None,
            word_before: None,
            palette: None,
            palette_actions: Vec::new(),
            preview_open: true,
            preview_w: 360.,
            sidebar_w: 248.,
            sidebar_hidden: false,
            sidebar_overlay: false,
            collapsed_sections: HashSet::new(),
            path_edit: None,
            filter_box: None,
            rename: None,
            cut: HashSet::new(),
            clip_seq: ferret_core::shell::clipboard_sequence(),
            server_clip: None,
            undo: Vec::new(),
            redo: Vec::new(),
            sizes: HashMap::new(),
            thumbs: Default::default(),
            thumbs_running: 0,
            thumb_queue: Vec::new(),
            quicklook: false,
            last_click: None,
            editor: None,
            folder_peek: None,
            folder_peek_before: None,
            editor_on_start: std::env::var_os("FERRET_GPUI_EDITOR").is_some(),
            watched: Vec::new(),
            title: String::new(),
            shelf: Vec::new(),
            shelf_press: None,
            preview_want: None,
            preview_data: None,
            preview_since: None,
            preview_hold_wake: None,
            preview_fade: Default::default(),
            fade_seq: 0,
            summary_pending: false,
            held_pic: Default::default(),
            preview_showing: Default::default(),
            preview_near: Vec::new(),
            preview_stepped: None,
            warming: Default::default(),
            drag_hint: None,
            own_drag: false,
            touched_sizes: Vec::new(),
            thumb_drag: None,
            sizes_left: 0,
            suggest_gen: Default::default(),
            size_sort_due: false,
            size_sorted_at: Instant::now(),
            preview_cache: Vec::new(),
            preview_view: Default::default(),
            picture_bounds: Default::default(),
            audio_bounds: Default::default(),
            pdf_scroll: [gpui::ScrollHandle::new(), gpui::ScrollHandle::new()],
            pdf_pending: false,
            text_sel: Default::default(),
            text_copied: None,
            pdf_unlock: None,
            pdf_text: None,
            ocr: None,
            audio: None,
            audio_volume: 1.,
            audio_muted: std::env::var_os("FERRET_GPUI_MUTE").is_some(),
            volume_bounds: Default::default(),
            settings_scroll: gpui::ScrollHandle::new(),
            folder_views: Vec::new(),
            folder_views_open: false,
            dz_slot: Default::default(),
            default_status: None,
            backdrop_changed: true,
            dwm_dark: None,
            network_pending: None,
            last_query: None,
            palette_back: None,
            pending_focus: None,
            type_ahead: None,
            kbd: Area::List,
            side_targets: Default::default(),
            side_mark: Default::default(),
            side_ring: Default::default(),
            scale: window.scale_factor(),
            viewport_w: window.viewport_size().width.as_f32(),
            settings,
            elevated: ferret_core::integration::window_info().elevated,
        };
        // FERRET_GPUI_OPEN opens one folder; otherwise the last session's tabs (if restoring),
        // or home.
        let start = std::env::var("FERRET_GPUI_OPEN").ok();
        // Explorer-style arguments (a folder, `/select,<file>`, `/e,`, `shell:` names): their
        // folders open as tabs, with the item selected, instead of the last session.
        // A window restarted as administrator (`--restore …`) gets its tabs and its place back.
        ferret_core::integration::init_launch_targets();
        let targets = if start.is_none() { ferret_core::integration::launch_targets() } else { Vec::new() };
        if let (Some(p), Some(h)) = (ferret_core::integration::placement(), hwnd(window)) {
            // Not now: moving the window onto another monitor makes Windows report the new
            // display from inside SetWindowPos, and GPUI handles that by borrowing the app, which
            // is still busy building this window (it aborted the elevated restart). A task runs
            // once this update is over.
            cx.spawn(async move |_, _| apply_placement(h, p)).detach();
        }
        for t in &targets {
            let path = if t.path.is_empty() { home.clone() } else { t.path.clone() };
            let ix = this.push_tab(path, None, None, cx);
            if let Some(sel) = &t.select {
                this.tabs[ix].main.pending_select = Some(vec![sel.clone()]);
            }
            if t.active {
                this.active = ix;
            }
        }
        let restore = start.is_none() && targets.is_empty() && this.settings.bool("restoreSession", true);
        let mut opened = false;
        if let (true, Some(s)) = (restore, session.as_ref()) {
            // As the app's `boot`: every tab in the strip at once, the one in use opened first,
            // the others after it in order; a server's tab waits with Connect.
            let first = this.tabs.len();
            let mut todo: Vec<Vec<(u64, String)>> = Vec::new();
            for tab in s["tabs"].as_array().into_iter().flatten() {
                if let Some(p) = tab["path"].as_str() {
                    let split = tab["split"].as_str().map(str::to_string);
                    todo.push(this.restore_tab(p.to_string(), split));
                    if tab["splitFocused"].as_bool().unwrap_or(false) {
                        if let Some(t) = this.tabs.last_mut() {
                            t.split_focused = t.split.is_some();
                        }
                    }
                    opened = true;
                }
            }
            this.active = s["active"].as_u64().unwrap_or(0) as usize;
            this.active = this.active.min(this.tabs.len().saturating_sub(1));
            let active = this.active.saturating_sub(first);
            for (id, path) in todo.get(active).cloned().unwrap_or_default() {
                this.open_restored(id, path, cx);
            }
            let rest: Vec<(u64, String)> = todo.into_iter().enumerate().filter(|(i, _)| *i != active).flat_map(|(_, v)| v).collect();
            if !rest.is_empty() {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(60)).await;
                    for (id, path) in rest {
                        if this.update(cx, |f, cx| f.open_restored(id, path, cx)).is_err() {
                            break;
                        }
                        cx.background_executor().timer(Duration::from_millis(15)).await;
                    }
                })
                .detach();
            }
            if let Some(w) = s["sidebarWidth"].as_f64() {
                this.sidebar_w = (w as f32).clamp(190., 420.);
            }
            if let Some(w) = s["previewWidth"].as_f64() {
                this.preview_w = (w as f32).max(260.);
            }
            this.preview_open = s["previewOpen"].as_bool().unwrap_or(true);
            this.sidebar_hidden = s["sidebarHidden"].as_bool().unwrap_or(false);
            this.collapsed_sections =
                s["collapsedSections"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        // The Dropzone comes back in every window, as the app's `boot` (also one opened on a
        // folder: it used to start empty and its next save emptied the saved one).
        if let Some(s) = session.as_ref() {
            this.shelf = s["shelf"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        if !opened && targets.is_empty() {
            this.push_tab(start.unwrap_or(home), None, None, cx);
        }
        if let Ok(q) = std::env::var("FERRET_GPUI_PALETTE") {
            this.open_palette(Some(q), window, cx);
        }
        this.maybe_intro(cx);
        this.start_update_checks(cx);
        // Other windows may have changed favorites, recents or settings: read again on focus.
        cx.observe_window_activation(window, |f, window, cx| {
            if window.is_window_active() {
                f.reread_shared(cx);
            }
        })
        .detach();
        // GPUI sets the window's dark mode from Windows' theme: the app's own is put back after.
        cx.observe_window_appearance(window, |f, _, cx| {
            f.dwm_dark = None;
            cx.notify();
        })
        .detach();
        crate::script::start(cx.entity().downgrade(), window, cx);
        // Drives, known folders and WSL distros, as the Tauri app's bootstrap gets them; the
        // drives again every minute (a USB stick comes or goes).
        cx.spawn(async move |this, cx| {
            loop {
                let places = cx.background_executor().spawn(async { ferret_core::places::all() }).await;
                let bin = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::recycle::recycle_info()).ok() }).await;
                if this
                    .update(cx, |this, cx| {
                        this.places = places;
                        this.bin = bin;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(60)).await;
            }
        })
        .detach();
        // Folders deleted from outside on shares and WSL may not raise watcher events: the open one
        // is checked every 4 s (a server's isn't: checking would connect). Recent drops folders
        // that no longer exist, now and every 2 minutes (servers' are kept).
        cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                let check = this
                    .update(cx, |f, _| {
                        let p = f.pane();
                        let open = (!p.loading && !p.is_bin() && !p.is_search() && !p.is_archive() && !ferret_core::paths::is_remote(&p.path)).then(|| (p.id, p.path.clone()));
                        let recents: Vec<String> =
                            if tick % 30 == 0 { f.recents.iter().map(|r| r.path.clone()).filter(|p| !ferret_core::paths::is_remote(p)).collect() } else { Vec::new() };
                        (open, recents)
                    })
                    .ok();
                let Some((open, recents)) = check else { break };
                let missing = |path: String| async move {
                    ferret_core::rt::block_on(ferret_core::browse::resolve_input(path, None)).is_ok_and(|r| r.kind == "missing")
                };
                if let Some((id, path)) = open {
                    let gone = cx.background_executor().spawn(missing(path.clone())).await;
                    if gone {
                        let _ = this.update(cx, |f, cx| {
                            if f.pane_ref(id).is_some_and(|p| same(&p.path, &path)) {
                                f.load(id, path, Nav::Other, cx);
                            }
                        });
                    }
                }
                for path in recents {
                    if cx.background_executor().spawn(missing(path.clone())).await {
                        let list = cx.background_executor().spawn(async move { ferret_core::browse::remove_recent(path) }).await;
                        if let Ok(list) = list {
                            let _ = this.update(cx, |f, cx| {
                                f.recents = list;
                                cx.notify();
                            });
                        }
                    }
                }
                tick += 1;
                cx.background_executor().timer(Duration::from_secs(4)).await;
            }
        })
        .detach();
        // Toasts leave on their own; job cards appear after 0.6 s.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        this
    }

    /// Housekeeping four times a second: toasts and finished jobs leave, jobs show their card,
    /// a cut made here is forgotten when something else was copied.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let before = self.toasts.len();
        self.toasts.retain(|t| t.at.elapsed() < if t.error { Duration::from_secs(6) } else { Duration::from_millis(2600) } || t.action.is_some() && t.at.elapsed() < Duration::from_secs(6));
        changed |= before != self.toasts.len();
        for j in &mut self.jobs {
            if !j.shown && (j.started.elapsed() > Duration::from_millis(600) || j.question.is_some()) && j.finished_at.is_none() {
                j.shown = true;
                changed = true;
            }
        }
        let before = self.jobs.len();
        self.jobs.retain(|j| match (&j.finished_at, j.status.as_str()) {
            (Some(t), "error") => t.elapsed() < Duration::from_secs(8),
            (Some(t), _) => t.elapsed() < Duration::from_millis(1500),
            _ => true,
        });
        changed |= before != self.jobs.len();
        if self.jobs.iter().any(|j| j.finished_at.is_none() && j.shown) {
            changed = true; // speed and time left move on
        }
        if !self.cut.is_empty() {
            let seq = ferret_core::shell::clipboard_sequence();
            if seq != self.clip_seq {
                self.clip_seq = seq;
                self.cut.clear();
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    // ───────────────────────────── tabs and panes ─────────────────────────────

    /// Adds a tab (at `at`, or the end) for `path`, with a second pane at `split`.
    pub fn push_tab(&mut self, path: String, split: Option<String>, at: Option<usize>, cx: &mut Context<Self>) -> usize {
        let main = Pane::new(self.id(), path.clone());
        let main_id = main.id;
        let split_pane = split.map(|s| Pane::new(self.id(), s));
        let split_id = split_pane.as_ref().map(|p| (p.id, p.path.clone()));
        let tab = Tab { id: self.id(), main, split: split_pane, split_focused: false };
        let ix = at.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(ix, tab);
        if ix <= self.active && self.tabs.len() > 1 && at.is_some() {
            // the active tab moved right
        }
        self.load(main_id, path, Nav::Other, cx);
        if let Some((id, p)) = split_id {
            self.load(id, p, Nav::Other, cx);
        }
        ix
    }

    /// A tab brought back from the last session: made now, opened by `open_restored`.
    fn restore_tab(&mut self, path: String, split: Option<String>) -> Vec<(u64, String)> {
        let main = Pane::new(self.id(), path.clone());
        let mut todo = vec![(main.id, path)];
        let split_pane = split.map(|s| {
            let p = Pane::new(self.id(), s.clone());
            todo.push((p.id, s));
            p
        });
        let id = self.id();
        self.tabs.push(Tab { id, main, split: split_pane, split_focused: false });
        todo
    }

    /// Opens a restored pane. A server's doesn't connect by itself (it would, unasked, at every
    /// start): it waits with "Not connected" and a Connect button, as the app's `showOffline`.
    fn open_restored(&mut self, id: u64, path: String, cx: &mut Context<Self>) {
        if paths::is_remote(&path) && !ferret_core::paths::remote_parts(&path).is_some_and(|(n, _)| ferret_core::remote::ssh::connected(n).is_some()) {
            // The server's name as the user gave it, not its id.
            let name = ferret_core::paths::remote_parts(&path).map(|(n, _)| ferret_core::remote::name_of(n)).unwrap_or_else(|| "this server".into());
            if let Some(p) = self.pane_by(id) {
                p.path = path;
                p.error = Some(format!("Not connected to {name}"));
                p.loading = false;
                p.loaded_once = true;
                p.relayout();
            }
            cx.notify();
        } else {
            self.load(id, path, Nav::Other, cx);
        }
    }

    pub fn new_tab(&mut self, path: String, cx: &mut Context<Self>) {
        let ix = self.push_tab(path, None, Some(self.active + 1), cx);
        self.active = ix;
        self.after_tab_change(cx);
    }

    pub fn close_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.len() <= 1 || ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        self.closed.push((ix, tab.main.path.clone(), tab.split.as_ref().map(|s| s.path.clone())));
        if self.closed.len() > 20 {
            self.closed.remove(0);
        }
        if self.active > ix || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.after_tab_change(cx);
    }

    pub fn close_other_tabs(&mut self, keep: usize, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self.tabs.iter().enumerate().filter(|(i, _)| *i != keep).map(|(_, t)| t.id).collect();
        for id in ids.into_iter().rev() {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.close_tab(i, cx);
            }
        }
    }

    pub fn reopen_tab(&mut self, cx: &mut Context<Self>) {
        let Some((ix, path, split)) = self.closed.pop() else { return };
        let ix = self.push_tab(path, split, Some(ix), cx);
        self.active = ix;
        self.after_tab_change(cx);
    }

    pub fn switch_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            self.after_tab_change(cx);
        }
    }

    pub fn cycle_tab(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        self.switch_tab(((self.active as isize + by).rem_euclid(n)) as usize, cx);
    }

    pub fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.tabs.len() || to > self.tabs.len() || from == to {
            return;
        }
        let active_id = self.tabs[self.active].id;
        let tab = self.tabs.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.tabs.insert(to.min(self.tabs.len()), tab);
        self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        self.save_session();
        cx.notify();
    }

    fn after_tab_change(&mut self, cx: &mut Context<Self>) {
        self.rename = None;
        self.path_edit = None;
        self.filter_box = None;
        self.save_session();
        self.watch(cx);
        cx.notify();
    }

    pub fn toggle_split(&mut self, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
        if tab.split.is_some() {
            self.close_split(cx);
            return;
        }
        let path = tab.main.path.clone();
        let p = Pane::new(self.id(), path.clone());
        let id = p.id;
        let tab = &mut self.tabs[self.active];
        tab.split = Some(p);
        tab.split_focused = true;
        self.load(id, path, Nav::Other, cx);
        self.after_tab_change(cx);
    }

    /// Closing the split keeps the pane in use: if it was the second, it becomes the tab's.
    pub fn close_split(&mut self, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if let Some(split) = tab.split.take() {
            if tab.split_focused {
                tab.main = split;
            }
            tab.split_focused = false;
        }
        self.after_tab_change(cx);
    }

    pub fn switch_pane(&mut self, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if tab.split.is_some() {
            tab.split_focused = !tab.split_focused;
            self.rename = None;
            self.save_session();
            cx.notify();
        }
    }

    pub fn focus_pane(&mut self, second: bool, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if tab.split.is_some() && tab.split_focused != second {
            tab.split_focused = second;
            cx.notify();
        }
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn pane(&self) -> &Pane {
        self.tabs[self.active].pane()
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        self.tabs[self.active].pane_mut()
    }

    pub fn pane_by(&mut self, id: u64) -> Option<&mut Pane> {
        self.tabs.iter_mut().find_map(|t| t.by_id(id))
    }

    pub fn pane_ref(&self, id: u64) -> Option<&Pane> {
        self.tabs.iter().find_map(|t| if t.main.id == id { Some(&t.main) } else { t.split.as_ref().filter(|s| s.id == id) })
    }

    /// The pane shown in a slot of the active tab.
    pub fn slot(&self, second: bool) -> Option<&Pane> {
        let tab = &self.tabs[self.active];
        if second { tab.split.as_ref() } else { Some(&tab.main) }
    }

    // ───────────────────────────── navigation ─────────────────────────────

    /// Lists `path` into pane `id` (in the background), then shows it.
    pub fn load(&mut self, id: u64, path: String, nav: Nav, cx: &mut Context<Self>) {
        let sizes: HashMap<String, u64> = self.sizes.iter().filter(|(_, s)| s.done).map(|(k, s)| (k.clone(), s.bytes)).collect();
        let Some(p) = self.pane_by(id) else { return };
        p.token += 1;
        let token = p.token;
        p.loading = true;
        let moving = !same(&p.path, &path) || !p.loaded_once;
        if moving {
            p.filter.clear();
            p.filter_open = false;
        }
        let keep_view = (!moving && p.view_changed).then(|| p.view.clone());
        let filter = p.filter.clone();
        let came_from = (nav == Nav::Out).then(|| p.path.clone());
        let started = Instant::now();
        if moving {
            crate::watchdog::act(format!("opened folder {path} ({nav:?}, tab pane {id})"));
        }
        let task = cx.background_executor().spawn({
            let path = path.clone();
            async move {
                let t0 = Instant::now();
                let listed = ferret_core::rt::block_on(browse::list_dir(path));
                let list_ms = t0.elapsed().as_secs_f64() * 1000.;
                listed.map(|l| {
                    let view = keep_view.unwrap_or_else(|| ViewSettings::from_json(&l.resolved.view));
                    let bin = ferret_core::recycle::is_recycle(&l.path);
                    let t1 = Instant::now();
                    let prepared = pane::prepare(&l.entries, &view, &filter, bin, &|p| sizes.get(&paths::key(p)).copied());
                    (l, view, prepared, list_ms, t1.elapsed().as_secs_f64() * 1000.)
                })
            }
        });
        cx.spawn(async move |this, cx| {
            let res = task.await;
            this.update(cx, |f, cx| f.listed(id, token, path, nav, moving, came_from, res, started, cx)).ok();
        })
        .detach();
        cx.notify();
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn listed(
        &mut self,
        id: u64,
        token: u64,
        requested: String,
        nav: Nav,
        moving: bool,
        came_from: Option<String>,
        res: Result<(browse::DirListing, ViewSettings, pane::Prepared, f64, f64), String>,
        started: Instant,
        cx: &mut Context<Self>,
    ) {
        let bench_dir = self.bench.as_ref().and_then(|b| b.dir.clone());
        bench::mark(bench::LISTED);
        // A network folder whose server wants a user name and password: asked in Ferret's own box.
        let requested_copy = requested.clone();
        let wants_login = moving && matches!(&res, Err(e) if e.starts_with(ferret_core::network::LOGIN_PREFIX));
        // An archive whose file list is encrypted: its password, then it's listed (as the app).
        let wants_password = moving && matches!(&res, Err(e) if e == ferret_core::archive::PASSWORD_REQUIRED || e == ferret_core::archive::WRONG_PASSWORD);
        let Some(p) = self.pane_by(id) else { return };
        if p.token != token {
            return;
        }
        p.loading = false;
        let mut listed_ok = None;
        match res {
            Ok((l, view, prepared, list_ms, prep_ms)) => {
                if moving {
                    p.selected = None;
                    p.marked.clear();
                    p.collapsed.clear();
                    p.nav = nav;
                    p.nav_seq += 1;
                    p.opened_at = Instant::now();
                    p.view_changed = false;
                }
                p.path = l.path;
                p.entries = Arc::new(l.entries);
                p.view = view;
                p.source = l.resolved.source.to_string();
                p.source_path = l.resolved.source_path.clone();
                p.profile = l.resolved.profile.map(str::to_string);
                p.error = None;
                p.apply(prepared);
                p.loaded_once = true;
                if moving {
                    p.scroll_top();
                }
                if let Some(paths) = p.pending_select.take().or_else(|| {
                    // FERRET_GPUI_SELECT=<name>|<name>… (screenshots and tests; the first is the cursor).
                    std::env::var("FERRET_GPUI_SELECT").ok().filter(|_| moving).map(|names| {
                        names.split('|').map(|n| format!("{}\\{n}", p.path.trim_end_matches('\\'))).collect()
                    })
                }) {
                    let hits: Vec<usize> = paths.iter().filter_map(|x| p.index_of(x)).collect();
                    if let Some(&ix) = hits.first() {
                        p.selected = Some(ix);
                        p.marked = hits.iter().copied().collect();
                        p.anchor = ix;
                        p.scroll_to(ix);
                    }
                } else if let Some(from) = came_from {
                    // Going up selects the folder you came from.
                    if let Some(ix) = p.index_of(&from) {
                        p.selected = Some(ix);
                        p.marked = [ix].into();
                        p.anchor = ix;
                        p.scroll_to(ix);
                    }
                }
                let rename_now = std::mem::take(&mut p.pending_rename);
                listed_ok = Some((p.path.clone(), p.rows.len(), list_ms, prep_ms, rename_now, moving));
            }
            // The folder shown was deleted (or moved) from outside: the tab moves up to the nearest
            // folder that's still there.
            Err(_) if !moving && p.loaded_once && !std::path::Path::new(&requested).exists() && !paths::is_remote(&requested) && !requested.starts_with("\\\\") => {
                let mut up = util::parent(&requested);
                while let Some(u) = up.clone() {
                    if std::path::Path::new(&u).is_dir() {
                        break;
                    }
                    up = util::parent(&u);
                }
                if let Some(u) = up {
                    let name = util::folder_name(&requested);
                    self.toast(format!("“{name}” isn't there anymore: showing {}", util::folder_name(&u)), cx);
                    self.load(id, u, Nav::Out, cx);
                }
                return;
            }
            Err(e) => {
                // A server that couldn't be reached, or whose password was cancelled: "Not
                // connected to …" with a Connect button, as the app's `showOffline` (it said
                // "Can't open this folder: Cancelled", with no way to try again).
                let offline = ferret_core::paths::remote_parts(&requested)
                    .filter(|(n, _)| ferret_core::remote::ssh::connected(n).is_none())
                    .map(|(n, _)| ferret_core::remote::name_of(n));
                let e = match offline {
                    Some(name) => {
                        if e != "Cancelled" && !e.ends_with("Cancelled") {
                            self.toast_error(format!("Couldn't connect to {name}: {}", crate::ops::plain_error(&e)), cx);
                        }
                        format!("Not connected to {name}")
                    }
                    None => e,
                };
                let Some(p) = self.pane_by(id) else { return };
                p.path = requested;
                p.entries = Arc::new(Vec::new());
                p.rows.clear();
                p.groups.clear();
                p.selected = None;
                p.marked.clear();
                p.hidden = 0;
                p.folders = 0;
                p.unfiltered = 0;
                p.recent.clear();
                p.error = Some(e);
                p.relayout();
                p.loaded_once = true;
            }
        }
        if wants_login && self.network_pending.is_none() && self.dialog.is_none() {
            self.network_sign_in(requested_copy.clone(), cx);
        }
        if wants_password && self.dialog.is_none() {
            let archive = ferret_core::archive::split_virtual(&requested_copy).map(|(a, _)| a.to_string()).unwrap_or_else(|| requested_copy.clone());
            self.ask_password(archive, None, cx);
        }
        if let Some((path, items, list_ms, prep_ms, rename_now, moving)) = listed_ok {
            // A slow listing is worth a line (a network folder, a huge one).
            let total = started.elapsed().as_millis();
            if total >= 300 {
                log::info!(target: "ferret::perf", "listed {path}: {items} items in {total} ms (reading {list_ms:.0} ms, sorting {prep_ms:.0} ms)");
            }
            if let Some(b) = self.bench.as_mut() {
                b.write("step_list_ms", format!("{list_ms:.0}"));
                b.write("step_prepare_ms", format!("{prep_ms:.0}"));
                if bench_dir.as_deref().is_some_and(|d| same(d, &path)) {
                    b.listed = Some((started, started.elapsed()));
                    let _ = items;
                }
            }
            if rename_now {
                self.start_rename(cx);
            }
            if moving {
                self.request_sizes(id, cx);
                self.remember_recent_later(id, path, cx);
            }
        }
        self.watch(cx);
        self.save_session();
        cx.notify();
    }

    /// Goes to `path` in the focused pane (history kept).
    pub fn go(&mut self, path: String, cx: &mut Context<Self>) {
        let id = self.pane().id;
        self.go_in(id, path, cx);
    }

    pub fn go_in(&mut self, id: u64, path: String, cx: &mut Context<Self>) {
        self.sidebar_overlay = false;
        let Some(p) = self.pane_by(id) else { return };
        let nav = if util::within(&path, &p.path) && !same(&path, &p.path) {
            Nav::Into
        } else if util::within(&p.path, &path) && !same(&path, &p.path) {
            Nav::Out
        } else {
            Nav::Other
        };
        if !same(&path, &p.path) {
            let cur = p.path.clone();
            p.back.push(cur);
            p.fwd.clear();
        }
        self.rename = None;
        self.load(id, path, nav, cx);
    }

    /// Opens a folder and selects an item in it (search results, "show in folder").
    pub fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(parent) = util::parent(path) else { return };
        if same(&parent, &self.pane().path) {
            if let Some(ix) = self.pane().index_of(path) {
                self.select(ix, cx);
                return;
            }
        }
        self.pane_mut().pending_select = Some(vec![path.to_string()]);
        self.go(parent, cx);
    }

    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if let Some(prev) = p.back.pop() {
            let cur = p.path.clone();
            p.fwd.push(cur.clone());
            let nav = if util::within(&cur, &prev) { Nav::Out } else { Nav::Other };
            let id = p.id;
            self.load(id, prev, nav, cx);
        }
    }

    pub fn go_forward(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if let Some(next) = p.fwd.pop() {
            let cur = p.path.clone();
            p.back.push(cur.clone());
            let nav = if util::within(&next, &cur) { Nav::Into } else { Nav::Other };
            let id = p.id;
            self.load(id, next, nav, cx);
        }
    }

    /// Jumps several steps back or forward (the history menus).
    pub fn go_history(&mut self, back: bool, steps: usize, cx: &mut Context<Self>) {
        for _ in 0..steps.saturating_sub(1) {
            let p = self.pane_mut();
            let next = if back { p.back.pop() } else { p.fwd.pop() };
            if let Some(x) = next {
                let cur = std::mem::replace(&mut p.path, x);
                if back { p.fwd.push(cur) } else { p.back.push(cur) }
            }
        }
        if back { self.go_back(cx) } else { self.go_forward(cx) }
    }

    pub fn go_up(&mut self, cx: &mut Context<Self>) {
        if let Some(parent) = util::parent(&self.pane().path) {
            self.go(parent, cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        let (id, path) = (p.id, p.path.clone());
        self.load(id, path.clone(), Nav::Other, cx);
        // Refresh also counts folder sizes again, everything below this folder (the app's
        // `refresh`). Marked out of date first, then asked for again: before, the listing could
        // ask before they were marked, and nothing asked after.
        self.sizes_changed(vec![path], true, cx);
    }

    /// Something changed inside these folders (with `deep`, anywhere below them): their sizes
    /// and their parents' are counted again, and every open folder asks for them again (the
    /// app's `invalidateFolderSizes` and `sizesEpoch`).
    pub fn sizes_changed(&mut self, paths: Vec<String>, deep: bool, cx: &mut Context<Self>) {
        if paths.is_empty() || !self.settings.bool("folderSizes", true) {
            return;
        }
        let task = cx.background_executor().spawn(async move { ferret_core::foldersize::folder_sizes_invalidate(paths, deep) });
        cx.spawn(async move |this, cx| {
            task.await;
            this.update(cx, |f, cx| {
                let ids: Vec<u64> = f.tabs.iter().flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref())).map(|p| p.id).collect();
                for id in ids {
                    f.request_sizes(id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reloads every pane showing `dir` (after a file operation or a change on disk).
    pub fn reload_dir(&mut self, dir: &str, cx: &mut Context<Self>) {
        let ids: Vec<(u64, String)> = self
            .tabs
            .iter()
            .flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref()))
            .filter(|p| same(&p.path, dir) || (p.is_search() && !dir.is_empty()))
            .map(|p| (p.id, p.path.clone()))
            .collect();
        for (id, path) in ids {
            self.load(id, path, Nav::Other, cx);
        }
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        self.folder_peek = None;
        self.folder_peek_before = None;
        let ids: Vec<(u64, String)> =
            self.tabs.iter().flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref())).map(|p| (p.id, p.path.clone())).collect();
        for (id, path) in ids {
            self.load(id, path, Nav::Other, cx);
        }
    }

    /// Opens a row: folders (and archives) in place, files with their app.
    pub fn open_row(&mut self, ix: usize, new_tab: bool, cx: &mut Context<Self>) {
        let Some(r) = self.pane().rows.get(ix) else { return };
        let (path, is_dir, ext, in_archive) = (r.e.path.clone(), r.e.is_dir, r.e.ext.clone(), r.e.in_archive);
        let archive = !is_dir && ferret_core::archive::archive_kind(&path).is_some();
        if self.pane().is_bin() {
            if is_dir {
                self.toast_action("Restore it to open it", "Restore", action(move |f, _, cx| f.restore_from_bin(vec![path.clone()], cx)), cx);
            } else {
                self.quicklook = true;
                cx.notify();
            }
            return;
        }
        if is_dir || archive {
            if new_tab { self.new_tab(path, cx) } else { self.go(path, cx) }
            return;
        }
        let _ = ext;
        self.open_file(path, in_archive, cx);
    }

    /// Opens the selected items: up to 10 files with their apps, folders as tabs.
    pub fn open_selected(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        let picked: Vec<usize> = if p.marked.len() > 1 { p.marked.iter().copied().collect() } else { p.selected.into_iter().collect() };
        if picked.len() == 1 {
            self.open_row(picked[0], false, cx);
            return;
        }
        let rows: Vec<(String, bool, bool)> = picked.iter().filter_map(|&i| p.rows.get(i)).map(|r| (r.e.path.clone(), r.e.is_dir, r.e.in_archive)).collect();
        for (path, is_dir, inside) in rows.iter().filter(|r| !r.1).take(10) {
            let _ = is_dir;
            self.open_file(path.clone(), *inside, cx);
        }
        for (path, _, _) in rows.iter().filter(|r| r.1) {
            let ix = self.push_tab(path.clone(), None, Some(self.tabs.len()), cx);
            let _ = ix;
        }
        cx.notify();
    }

    pub fn open_file(&mut self, path: String, in_archive: bool, cx: &mut Context<Self>) {
        crate::watchdog::act(format!("opened file {path}"));
        let dir = util::parent(&path).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let real = if in_archive {
                        ferret_core::rt::block_on(ferret_core::archive::archive_materialize(path, None))?
                    } else if paths::is_remote(&path) {
                        return ferret_core::rt::block_on(ferret_core::remote::edit::remote_open(path));
                    } else {
                        path
                    };
                    browse::open_path(real)
                })
                .await;
            match res {
                Err(e) => {
                    this.update(cx, |f, cx| f.toast_error(format!("Couldn't open it: {e}"), cx)).ok();
                }
                // A folder counts as recent once you open a file in it.
                Ok(()) if !dir.is_empty() => Self::push_recent(&this, dir, cx).await,
                Ok(()) => {}
            }
        })
        .detach();
    }

    // ───────────────────────────── selection ─────────────────────────────

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.select_with(ix, false, false, cx);
    }

    /// Makes the pane with this id the one in use (a screen reader picked an item in it).
    pub fn focus_pane_of(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.tab().split.is_some() {
            let second = self.slot(true).is_some_and(|p| p.id == id);
            self.focus_pane(second, cx);
        }
    }

    /// Selects `ix`: alone, toggled (Ctrl), or the range from the anchor (Shift).
    pub fn select_with(&mut self, ix: usize, extend: bool, toggle: bool, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if ix >= p.rows.len() {
            return;
        }
        if extend {
            // The range in the order shown (groups can reorder rows).
            let order: Vec<usize> = p.visible_rows().collect();
            let a = order.iter().position(|&r| r == p.anchor).unwrap_or(0);
            let b = order.iter().position(|&r| r == ix).unwrap_or(0);
            let (lo, hi) = (a.min(b), a.max(b));
            let range: std::collections::BTreeSet<usize> = order[lo..=hi].iter().copied().collect();
            if toggle { p.marked.extend(range) } else { p.marked = range }
        } else if toggle {
            if !p.marked.remove(&ix) {
                p.marked.insert(ix);
            }
            p.anchor = ix;
        } else {
            p.marked = [ix].into();
            p.anchor = ix;
        }
        p.selected = Some(ix);
        p.scroll_to(ix);
        cx.notify();
    }

    /// Moves the cursor only (Ctrl+arrows), leaving the selection.
    pub fn move_cursor(&mut self, ix: usize, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if ix < p.rows.len() {
            p.selected = Some(ix);
            p.scroll_to(ix);
            cx.notify();
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        p.marked = p.visible_rows().collect();
        cx.notify();
    }

    pub fn invert_selection(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        let all: std::collections::BTreeSet<usize> = p.visible_rows().collect();
        p.marked = all.difference(&p.marked).copied().collect();
        p.selected = p.marked.iter().next().copied();
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        p.marked.clear();
        cx.notify();
    }

    // ───────────────────────────── view ─────────────────────────────

    /// Changes the focused pane's view: sorting, grouping, layout, hidden items, icon size. Saved
    /// for the folder when "Remember view per folder" is on.
    pub fn set_view(&mut self, change: impl FnOnce(&mut ViewSettings), cx: &mut Context<Self>) {
        let remember = self.settings.bool("rememberPerFolder", true);
        let p = self.pane_mut();
        let before = p.view.clone();
        change(&mut p.view);
        if p.view == before {
            return;
        }
        let only_layout = before.mode != p.view.mode || before.icon_size != p.view.icon_size;
        p.view_changed = true;
        let (path, view) = (p.path.clone(), p.view.clone());
        if remember && !p.is_search() {
            let _ = browse::set_folder_view(path, view.to_json(), false);
            self.pane_mut().source = "folder".into();
            self.pane_mut().view_changed = false;
        }
        if only_layout && before.show_hidden == view.show_hidden && before.sort_by == view.sort_by
            && before.sort_dir == view.sort_dir && before.group_by == view.group_by && before.folders_first == view.folders_first
        {
            let p = self.pane_mut();
            p.relayout();
            if let Some(ix) = p.selected {
                p.scroll_to(ix);
            }
        } else {
            let id = self.pane().id;
            self.reprepare(id, cx);
        }
        cx.notify();
    }

    /// Works the rows out again from what was listed (view or filter changed). Small folders at
    /// once; big ones in the background.
    pub fn reprepare(&mut self, id: u64, cx: &mut Context<Self>) {
        let sizes: HashMap<String, u64> = self.sizes.iter().filter(|(_, s)| s.done).map(|(k, s)| (k.clone(), s.bytes)).collect();
        let Some(p) = self.pane_by(id) else { return };
        let (entries, view, filter, bin) = (p.entries.clone(), p.view.clone(), p.filter.clone(), p.is_bin());
        if entries.len() <= 4000 {
            let prepared = pane::prepare(&entries, &view, &filter, bin, &|x| sizes.get(&paths::key(x)).copied());
            p.apply(prepared);
            cx.notify();
            return;
        }
        p.token += 1;
        let token = p.token;
        let task = cx
            .background_executor()
            .spawn(async move { pane::prepare(&entries, &view, &filter, bin, &|x| sizes.get(&paths::key(x)).copied()) });
        cx.spawn(async move |this, cx| {
            let prepared = task.await;
            this.update(cx, |f, cx| {
                if let Some(p) = f.pane_by(id) {
                    if p.token == token {
                        p.apply(prepared);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn sort_by(&mut self, by: SortBy, cx: &mut Context<Self>) {
        let cur = self.pane().view.clone();
        self.set_view(
            |v| {
                if cur.sort_by == by {
                    v.sort_dir = if v.sort_dir == SortDir::Asc { SortDir::Desc } else { SortDir::Asc };
                } else {
                    v.sort_by = by;
                    // Name and Type start A→Z; dates and sizes newest/biggest first.
                    v.sort_dir = if matches!(by, SortBy::Name | SortBy::Type) { SortDir::Asc } else { SortDir::Desc };
                }
            },
            cx,
        );
    }

    pub fn cycle_group(&mut self, cx: &mut Context<Self>) {
        let next = match self.pane().view.group_by {
            GroupBy::None => GroupBy::Date,
            GroupBy::Date => GroupBy::Type,
            GroupBy::Type => GroupBy::Size,
            GroupBy::Size => GroupBy::Letter,
            GroupBy::Letter => GroupBy::None,
        };
        self.set_view(|v| v.group_by = next, cx);
        // Pressing it again replaces the last one (four in a row stacked four toasts).
        self.toasts.retain(|t| !(t.text.starts_with("Grouped by") || t.text == "Grouping off"));
        self.toast(
            match next {
                GroupBy::None => "Grouping off",
                GroupBy::Date => "Grouped by date",
                GroupBy::Type => "Grouped by type",
                GroupBy::Size => "Grouped by size",
                GroupBy::Letter => "Grouped by first letter",
            },
            cx,
        );
    }

    /// Shows or hides hidden items in every folder (Explorer's "Hidden items" box), remembered.
    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.set_show_hidden(!crate::pane::show_hidden(), cx);
    }

    pub fn set_show_hidden(&mut self, on: bool, cx: &mut Context<Self>) {
        crate::pane::set_show_hidden(on);
        self.settings.set("showHiddenItems", serde_json::Value::Bool(on));
        let ids: Vec<u64> = self.tabs.iter().flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref())).map(|p| p.id).collect();
        for id in ids {
            self.reprepare(id, cx);
        }
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.set_view(|v| v.mode = mode, cx);
    }

    pub fn set_icon_size(&mut self, size: u32, cx: &mut Context<Self>) {
        self.set_view(|v| v.icon_size = size.clamp(48, 256), cx);
    }

    pub fn reset_view(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        let (id, path) = (p.id, p.path.clone());
        let _ = browse::clear_folder_view(path.clone());
        if let Some(p) = self.pane_by(id) {
            p.view_changed = false;
            p.loaded_once = true;
        }
        self.load(id, path, Nav::Other, cx);
        self.toast("View reset to the default", cx);
    }

    pub fn save_view_here(&mut self, recursive: bool, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        let _ = browse::set_folder_view(p.path.clone(), p.view.to_json(), recursive);
        p.view_changed = false;
        p.source = "folder".into();
        let msg = if recursive { "View applied to this folder and its subfolders" } else { "View saved for this folder" };
        self.toast(msg, cx);
    }

    pub fn toggle_group(&mut self, label: &str, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if !p.collapsed.remove(label) {
            p.collapsed.insert(label.to_string());
        }
        p.relayout();
        cx.notify();
    }

    pub fn expand_all_groups(&mut self, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        p.collapsed.clear();
        p.relayout();
        cx.notify();
    }

    // ───────────────────────────── filter ─────────────────────────────

    pub fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kbd = Area::List;
        let id = self.pane().id;
        if let Some(fb) = &self.filter_box {
            if fb.pane == id {
                fb.input.update(cx, |i, cx| i.select_everything(cx));
                crate::input::focus(&fb.input, window, cx);
                return;
            }
        }
        let text = self.pane().filter.clone();
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Filter this folder", cx).sized(13., 20.);
            i.set_text(&text, cx);
            i.select_everything(cx);
            i
        });
        let sub = cx.subscribe(&input, move |f, input, _: &crate::input::Changed, cx| {
            let text = input.read(cx).text().to_string();
            if let Some(p) = f.pane_by(id) {
                p.filter = text;
            }
            f.reprepare(id, cx);
        });
        crate::input::focus(&input, window, cx);
        self.pane_mut().filter_open = true;
        self.filter_box = Some(FilterBox { pane: id, input, _sub: sub });
        cx.notify();
    }

    pub fn close_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fb) = self.filter_box.take() else { return };
        if let Some(p) = self.pane_by(fb.pane) {
            p.filter.clear();
            p.filter_open = false;
        }
        self.reprepare(fb.pane, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub fn set_filter_type(&mut self, ty: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_box.is_none() {
            self.open_filter(window, cx);
        }
        let text = crate::filter::with_type(&self.pane().filter, ty);
        if let Some(fb) = &self.filter_box {
            fb.input.update(cx, |i, cx| i.set_text(&text, cx));
        }
    }

    // ───────────────────────────── palette ─────────────────────────────

    pub fn open_palette(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        crate::watchdog::act(format!("Ctrl+K opened{}", query.as_deref().map(|q| format!(" with “{q}”")).unwrap_or_default()));
        self.menu = None;
        // Reopened within 8 seconds: the last text comes back.
        let restored = query.is_none();
        let query = query.or_else(|| self.last_query.take().filter(|(_, at)| at.elapsed() < Duration::from_secs(8)).map(|(q, _)| q));
        let select_all = restored && query.is_some();
        let p = self.pane();
        let folder = (!p.is_search() && !p.is_bin()).then(|| p.path.clone());
        // Folders used often and lately rank their matches higher (as the app's `frecency`).
        let now = ferret_core::db::now();
        let frecency = self
            .recents
            .iter()
            .map(|r| {
                let age = (now - r.last_opened) as f32 / (14. * 86_400_000.);
                (r.path.clone(), (r.count as f32 / 10.).min(1.) * 0.5 + (1. - age).max(0.) * 0.5)
            })
            .collect();
        let init = crate::palette::Init {
            query,
            select_all,
            folder,
            forced_scope: std::env::var("FERRET_GPUI_SCOPE").ok(),
            commands: Vec::new(),
            history: self.settings.strings("searchHistory"),
            saved: self.settings.saved_searches().into_iter().collect(),
            recent_folders: self.recents.iter().map(|r| r.path.clone()).take(5).collect(),
            frecency,
            bench: self.bench.as_ref().map(|b| b.out.clone()),
        };
        let (commands, actions): (Vec<Command>, Vec<Action>) = self.commands().into_iter().unzip();
        self.palette_actions = actions;
        let palette = cx.new(|cx| Palette::new(crate::palette::Init { commands, ..init }, window, cx));
        let sub = cx.subscribe_in(&palette, window, |this, _, event: &PaletteEvent, window, cx| {
            // Events that leave it open.
            match event {
                PaletteEvent::Searched(q) => {
                    this.remember_search(q.clone());
                    return;
                }
                PaletteEvent::Forget(q) => {
                    let list: Vec<String> = this.settings.strings("searchHistory").into_iter().filter(|h| !h.trim().eq_ignore_ascii_case(q.trim())).collect();
                    this.settings.set("searchHistory", json!(list));
                    return;
                }
                PaletteEvent::RemoveSaved(q) => {
                    this.remove_saved_search(q, cx);
                    return;
                }
                PaletteEvent::CopyPath(p) => {
                    this.copy_paths(std::slice::from_ref(p), true, cx);
                    return;
                }
                PaletteEvent::Drag(p) => {
                    this.drag_paths(vec![p.clone()], window, cx);
                    return;
                }
                PaletteEvent::Save { query, back } => {
                    // Cancelling the name brings the search back (as the app's `askToSave`).
                    this.prompt_save_search(query.clone(), cx);
                    this.palette_back = Some(back.clone());
                }
                PaletteEvent::Menu { path, is_dir, at } => {
                    this.search_result_menu(path.clone(), *is_dir, *at, window, cx);
                    return;
                }
                _ => {}
            }
            this.palette = None;
            window.focus(&this.focus, cx);
            match event {
                PaletteEvent::Open { path, is_dir } => {
                    crate::watchdog::act(format!("Ctrl+K: picked {path}"));
                    if let Some(b) = this.bench.as_ref() {
                        b.write("palette_opened", path);
                    }
                    // As the app's `activate`: a folder or archive opens here, a file in its app
                    // (the folder stays where it was).
                    if *is_dir || (ferret_core::archive::archive_kind(path).is_some() && !ferret_core::paths::is_remote(path)) {
                        this.go(path.clone(), cx);
                    } else {
                        this.open_file(path.clone(), false, cx);
                    }
                }
                PaletteEvent::OpenTab(path) => this.new_tab(path.clone(), cx),
                PaletteEvent::Reveal(path) => this.reveal(path, cx),
                PaletteEvent::SearchTab(q) => this.new_tab(format!("{}{q}", ferret_core::search::SEARCH_PREFIX), cx),
                PaletteEvent::Command(i) => {
                    let actions = std::mem::take(&mut this.palette_actions);
                    if let Some(b) = this.bench.as_ref() {
                        b.write("palette_command", i);
                    }
                    if let Some(run) = actions.get(*i) {
                        run(this, window, cx);
                    }
                }
                PaletteEvent::Dismiss(text) => {
                    if !text.trim().is_empty() {
                        this.last_query = Some((text.clone(), Instant::now()));
                    }
                }
                PaletteEvent::InstallIndex => this.confirm(
                    "Enable the instant drive index?".into(),
                    "Ferret installs its indexer service, which reads each NTFS drive's file table: search results come instantly and stay current. Windows asks for administrator permission once.".into(),
                    "Install",
                    false,
                    action(|f, _, cx| {
                        let task = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::search::search_install_service()) });
                        cx.spawn(async move |this, cx| {
                            let r = task.await;
                            this.update(cx, |f, cx| match r {
                                Ok(()) => f.toast("The drive index is on", cx),
                                Err(e) => f.toast_error(e, cx),
                            })
                            .ok();
                        })
                        .detach();
                        let _ = f;
                    }),
                    cx,
                ),
                _ => {}
            }
            cx.notify();
        });
        self.palette = Some((palette, sub));
        cx.notify();
    }

    // ───────────────────────────── toasts ─────────────────────────────

    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.push_toast(text.into(), false, None, cx);
    }

    pub fn toast_error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        // Errors shown to the user are kept in a log too (Settings → About → Open logs).
        log_line(&text);
        self.push_toast(crate::ops::plain_error(&text).into(), true, None, cx);
    }

    pub fn toast_action(&mut self, text: impl Into<SharedString>, label: &str, run: Action, cx: &mut Context<Self>) {
        self.push_toast(text.into(), false, Some((label.to_string().into(), run)), cx);
    }

    fn push_toast(&mut self, text: SharedString, error: bool, action: Option<(SharedString, Action)>, cx: &mut Context<Self>) {
        // Deleting again replaces the last "… to the Recycle Bin" toast instead of stacking
        // them (the app's `toast`).
        if action.is_some() && text.ends_with("Recycle Bin") {
            self.toasts.retain(|t| !(t.action.is_some() && t.text.ends_with("Recycle Bin")));
        }
        self.toast_seq += 1;
        self.toasts.push(Toast { id: self.toast_seq, text, error, action, at: Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        if let Some(b) = self.bench.as_ref() {
            b.write("toast", self.toasts.last().map(|t| t.text.to_string()).unwrap_or_default());
        }
        cx.notify();
    }

    // ───────────────────────────── events from the core ─────────────────────────────

    fn core_event(&mut self, name: &str, payload: &serde_json::Value, cx: &mut Context<Self>) {
        match name {
            "job-update" => self.job_update(payload, cx),
            "fs-changed" => {
                let dirs: Vec<String> = payload.as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                // While a file job runs, folders reload once a second instead of on every change.
                let busy = self.jobs.iter().any(|j| j.finished_at.is_none());
                let delay = if busy { Duration::from_millis(1000) } else { Duration::from_millis(180) };
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(delay).await;
                    this.update(cx, |f, cx| {
                        for d in &dirs {
                            f.reload_dir(d, cx);
                        }
                        // The selected folder's preview is read again when it changed.
                        if f.folder_peek_before.as_ref().is_some_and(|(p, _)| dirs.iter().any(|d| same(d, p))) {
                            f.folder_peek_before = None;
                        }
                        if f.folder_peek.as_ref().is_some_and(|(p, _)| dirs.iter().any(|d| same(d, p))) {
                            f.folder_peek = None;
                            cx.notify();
                        }
                        if dirs.iter().any(|d| ferret_core::recycle::is_recycle(d)) {
                            f.refresh_bin(cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
            "folder-sizes-activity" => {
                let left = payload["left"].as_u64().unwrap_or(0) as usize;
                if left != self.sizes_left {
                    self.sizes_left = left;
                    cx.notify();
                }
            }
            "folder-size" => {
                if let Some(s) = folder_size_of(payload) {
                    let done = s.done;
                    self.sizes.insert(paths::key(&s.path), s);
                    if done {
                        self.resort_by_size(cx);
                    }
                    cx.notify();
                }
            }
            "media-sample-progress" => {
                if let Some((editor, _)) = &self.editor {
                    let f = payload.as_f64().unwrap_or(0.);
                    editor.update(cx, |e, cx| e.sample_progress(f, cx));
                }
            }
            "remote-prompt" => self.remote_prompt(payload, cx),
            "remote-status" | "remote-connections" => cx.notify(),
            "remote-synced" => {
                if let Some(name) = payload["name"].as_str() {
                    self.toast(format!("Saved “{name}” to the server"), cx);
                }
            }
            _ => {}
        }
    }

    pub fn refresh_bin(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let bin = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::recycle::recycle_info()).ok() }).await;
            this.update(cx, |f, cx| {
                f.bin = bin;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Watches every open folder (and its parent, so a folder deleted from outside is noticed).
    fn watch(&mut self, cx: &mut Context<Self>) {
        let mut want: Vec<String> = Vec::new();
        for t in &self.tabs {
            for p in std::iter::once(&t.main).chain(t.split.as_ref()) {
                if p.is_search() || p.path.is_empty() {
                    continue;
                }
                if !want.iter().any(|w| same(w, &p.path)) {
                    want.push(p.path.clone());
                }
                if let Some(parent) = util::parent(&p.path) {
                    if !want.iter().any(|w| same(w, &parent)) {
                        want.push(parent);
                    }
                }
            }
        }
        want.sort();
        if want == self.watched {
            return;
        }
        self.watched = want.clone();
        cx.background_executor().spawn(async move { browse::watch_dirs(want) }).detach();
    }

    /// The folder counts as recent after 3 seconds there.
    fn remember_recent_later(&mut self, id: u64, path: String, cx: &mut Context<Self>) {
        if self.elevated || path.is_empty() || ferret_core::recycle::is_recycle(&path) || ferret_core::search::search_query_of(&path).is_some() {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let still_there = this.update(cx, |f, _| f.pane_ref(id).is_some_and(|p| same(&p.path, &path))).unwrap_or(false);
            if still_there {
                Self::push_recent(&this, path, cx).await;
            }
        })
        .detach();
    }

    /// `dir` becomes the most recent folder: saved, and the list read again, off the UI thread
    /// (it writes to the database and looks which recent folders are still on the disk).
    async fn push_recent(this: &WeakEntity<Self>, dir: String, cx: &mut AsyncApp) {
        let list = cx.background_executor().spawn(async move { browse::push_recent(dir) }).await;
        if let Ok(list) = list {
            this.update(cx, |f, cx| {
                if !same_recents(&f.recents, &list) {
                    f.recents = list;
                    cx.notify();
                }
            })
            .ok();
        }
    }

    /// Asks for the pane's folder sizes (counted in the background; they arrive as events).
    pub fn request_sizes(&mut self, id: u64, cx: &mut Context<Self>) {
        if !self.settings.bool("folderSizes", true) {
            return;
        }
        let Some(p) = self.pane_ref(id) else { return };
        if p.is_search() || p.is_bin() || p.path.starts_with("\\\\") && !paths::is_wsl(&p.path) && !paths::is_remote(&p.path) {
            return;
        }
        let dirs: Vec<String> = p.entries.iter().filter(|e| e.is_dir && !e.in_archive && (crate::pane::show_hidden() || !e.hidden)).map(|e| e.path.clone()).collect();
        let dir = Some(p.path.clone());
        let requester = (id & 0xffff_ffff) as u32;
        let task = cx.background_executor().spawn(async move { ferret_core::foldersize::folder_sizes(requester, dirs, false, dir) });
        cx.spawn(async move |this, cx| {
            let known = task.await;
            this.update(cx, |f, cx| {
                let any_done = known.iter().any(|s| s.done);
                for s in known {
                    f.sizes.insert(paths::key(&s.path), s);
                }
                if any_done {
                    f.resort_by_size(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Finished folder sizes arrived: lists sorted or grouped by size put their folders in order
    /// again. At most once a second, so rows don't keep jumping while sizes come in (the app's
    /// `resort`). Before, the order stayed as it was when the folder opened, before its sizes.
    fn resort_by_size(&mut self, cx: &mut Context<Self>) {
        if self.size_sort_due {
            return;
        }
        self.size_sort_due = true;
        let wait = Duration::from_millis(1000).saturating_sub(self.size_sorted_at.elapsed());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |f, cx| {
                f.size_sort_due = false;
                f.size_sorted_at = Instant::now();
                let ids: Vec<u64> = f
                    .tabs
                    .iter()
                    .flat_map(|t| std::iter::once(&t.main).chain(t.split.as_ref()))
                    .filter(|p| p.view.sort_by == SortBy::Size || p.view.group_by == GroupBy::Size)
                    .map(|p| p.id)
                    .collect();
                for id in ids {
                    f.reprepare(id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── session ─────────────────────────────

    pub fn save_session(&self) {
        if self.elevated || self.tabs.is_empty() {
            return;
        }
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            .map(|t| match &t.split {
                Some(s) => json!({ "path": t.main.path, "split": s.path, "splitFocused": t.split_focused }),
                None => json!({ "path": t.main.path }),
            })
            .collect();
        let session = json!({
            "tabs": tabs,
            "active": self.active,
            "sidebarWidth": self.sidebar_w,
            "sidebarHidden": self.sidebar_hidden,
            "collapsedSections": self.collapsed_sections.iter().collect::<Vec<_>>(),
            "previewOpen": self.preview_open,
            "previewWidth": self.preview_w,
            "shelf": self.shelf,
        });
        save_session_soon(session);
    }

    // ───────────────────────────── keys ─────────────────────────────

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The palette and the compress editor have the keys while open.
        if self.palette.is_some() || self.editor.is_some() {
            return;
        }
        let key = ev.keystroke.key.as_str();
        let m = ev.keystroke.modifiers;
        let ctrl = m.control;
        if self.autoscroll.is_some() && key == "escape" {
            self.stop_autoscroll(cx);
            cx.stop_propagation();
            return;
        }
        if self.menu.is_some() {
            self.menu_key(key, m, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.dialog.is_some() {
            if self.dialog_key(key, m, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        // Ctrl+F in the preview or Quick Look finds in its text (preview.rs); elsewhere it filters.
        if ctrl && !m.shift && !m.alt && key == "f" && self.open_preview_find(window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.quicklook {
            // Typing in its find bar: the keys are the field's (Enter, Esc: the bar's own).
            if self.preview_find_focused(window, cx).is_some() {
                return;
            }
            // Typing a PDF's password in Quick Look: Enter unlocks, Esc closes, the rest is typing.
            if self.pdf_unlock.as_ref().is_some_and(|u| u.input.read(cx).is_focused(window)) {
                match key {
                    "enter" => self.submit_pdf_password(window, cx),
                    "escape" => {
                        self.quicklook_key(key, window, cx);
                        window.focus(&self.focus, cx);
                    }
                    _ => return,
                }
                cx.stop_propagation();
                return;
            }
            // Ctrl+A / Ctrl+C on its text.
            if ctrl && !m.shift && !m.alt && matches!(key, "a" | "c") && self.preview_text_key(key, cx) {
                cx.stop_propagation();
                return;
            }
            self.quicklook_key(key, window, cx);
            cx.stop_propagation();
            return;
        }
        // F5 and Ctrl+R refresh wherever the focus is.
        if key == "f5" || (ctrl && key == "r") {
            if !ev.is_held {
                self.refresh(cx);
            }
            cx.stop_propagation();
            return;
        }
        // F6 / Shift+F6 step through the window's areas, from a text field too.
        if key == "f6" && !ctrl && !m.alt {
            self.cycle_area(if m.shift { -1 } else { 1 }, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.kbd != Area::List && self.focus.is_focused(window) && !ctrl && !m.alt && self.area_key(key, m, window, cx) {
            cx.stop_propagation();
            return;
        }
        // Typing in a field (rename, path, filter): only its own keys.
        if !self.focus.is_focused(window) {
            if self.field_key(key, m, window, cx) {
                cx.stop_propagation();
            }
            if !ctrl {
                return;
            }
        }
        let handled = self.global_key(key, m, ev.is_held, window, cx) || (self.focus.is_focused(window) && self.list_key(ev, window, cx));
        if handled {
            cx.stop_propagation();
        }
    }

    /// The area the keyboard ring is in, if the window (not a text field) has the keyboard.
    pub fn ring_area(&self, window: &Window) -> Area {
        if self.focus.is_focused(window) { self.kbd } else { Area::List }
    }

    /// F6 / Shift+F6, as in Explorer: the tabs, the path, the sidebar, the files and the preview
    /// in turn, each at the item that matters there (the current tab, folder or place).
    pub fn cycle_area(&mut self, dir: i32, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = !self.sidebar_hidden && window.viewport_size().width.as_f32() >= 860. || self.sidebar_overlay;
        let preview = self.preview_open && window.viewport_size().width.as_f32() >= 900.;
        let mut areas = vec![0, 1];
        if sidebar {
            areas.push(2);
        }
        areas.push(3);
        if preview {
            areas.push(4);
        }
        let typing = !self.focus.is_focused(window);
        let here = if typing && self.path_edit.is_some() {
            1
        } else {
            match self.kbd {
                Area::Tabs(_) => 0,
                Area::Crumbs(_) => 1,
                Area::Sidebar(_) => 2,
                Area::List | Area::Heads(_) => 3,
                Area::Preview => 4,
            }
        };
        let at = areas.iter().position(|&a| a == here).unwrap_or(areas.len() - 2) as i32;
        let next = areas[(at + dir).rem_euclid(areas.len() as i32) as usize];
        if self.path_edit.is_some() {
            self.close_path_edit(window, cx);
        }
        let cur = self.pane().path.clone();
        self.kbd = match next {
            0 => Area::Tabs(self.active),
            1 => Area::Crumbs(util::crumbs(&cur).len().saturating_sub(1)),
            2 => Area::Sidebar(self.side_targets.borrow().iter().position(|p| util::same(p, &cur)).unwrap_or(0)),
            4 => Area::Preview,
            _ => Area::List,
        };
        // A PDF asking for its password: the keyboard goes into its field (preview.rs).
        if next == 4 && self.focus_pdf_password(window, cx) {
            return;
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The list's headings as shown (sort key, and the column whose left line can move).
    pub fn visible_heads(&self) -> Vec<(SortBy, Option<crate::settings::Column>)> {
        use crate::settings::Column;
        let p = self.pane();
        let w = p.width.max(1.);
        let mut out = vec![(SortBy::Name, None)];
        for (by, col, shown) in [
            (SortBy::Modified, Column::Modified, w >= 430.),
            (SortBy::Type, Column::Type, w >= 600. && !p.is_bin()),
            (SortBy::Size, Column::Size, true),
        ] {
            if shown && !self.settings.column_hidden(col) {
                out.push((by, Some(col)));
            }
        }
        out
    }

    /// Keys for the area the keyboard is in (other than the files). True if handled: the tabs,
    /// path, sidebar and headings keep other plain keys (letters, Delete, F2 don't reach the
    /// files behind them); F-keys go on to the window.
    fn area_key(&mut self, key: &str, m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let step = |i: usize, n: usize, key: &str, back: &str, fwd: &str| -> Option<usize> {
            if n == 0 {
                return None;
            }
            Some(match key {
                k if k == back => i.saturating_sub(1),
                k if k == fwd => (i + 1).min(n - 1),
                "home" => 0,
                "end" => n - 1,
                _ => return None,
            })
        };
        if key == "escape" {
            self.kbd = Area::List;
            cx.notify();
            return true;
        }
        match self.kbd {
            Area::List => return false,
            Area::Tabs(i) => {
                let n = self.tabs.len();
                if let Some(j) = step(i, n, key, "left", "right") {
                    self.kbd = Area::Tabs(j);
                } else {
                    match key {
                        "enter" | "space" => {
                            self.switch_tab(i.min(n - 1), cx);
                            self.kbd = Area::List;
                        }
                        "delete" => {
                            self.close_tab(i, cx);
                            self.kbd = Area::Tabs(i.min(self.tabs.len().saturating_sub(1)));
                        }
                        "down" => self.kbd = Area::List,
                        _ => return !fkey(key),
                    }
                }
            }
            Area::Crumbs(i) => {
                let parts = util::crumbs(&self.pane().path);
                if let Some(j) = step(i, parts.len(), key, "left", "right") {
                    self.kbd = Area::Crumbs(j);
                } else {
                    match key {
                        "enter" | "space" => {
                            if let Some((_, path)) = parts.get(i.min(parts.len().saturating_sub(1))) {
                                let path = path.clone();
                                self.kbd = Area::List;
                                self.go(path, cx);
                            }
                        }
                        "down" => self.kbd = Area::List,
                        _ => return !fkey(key),
                    }
                }
            }
            Area::Sidebar(i) => {
                let targets = self.side_targets.borrow().clone();
                if m.shift && matches!(key, "left" | "right") {
                    // Shift+←/→ resizes the sidebar (190–420).
                    self.sidebar_w = (self.sidebar_w + if key == "left" { -16. } else { 16. }).clamp(190., 420.);
                    self.save_session();
                } else if let Some(j) = step(i, targets.len(), key, "up", "down") {
                    self.kbd = Area::Sidebar(j);
                } else {
                    match key {
                        "enter" | "space" => {
                            let Some(path) = targets.get(i.min(targets.len().saturating_sub(1))).cloned() else { return true };
                            if m.shift {
                                let at = self.tabs.len();
                                self.push_tab(path, None, Some(at), cx);
                            } else if std::path::Path::new(&path).is_file() {
                                self.open_file(path, false, cx);
                            } else {
                                self.go(path, cx);
                            }
                        }
                        "right" => self.kbd = Area::List,
                        _ => return !fkey(key),
                    }
                }
            }
            Area::Heads(i) => {
                let heads = self.visible_heads();
                let i = i.min(heads.len().saturating_sub(1));
                if m.shift && matches!(key, "left" | "right") {
                    // Moves the line on this column's left (the Name column has none).
                    if let Some((_, Some(col))) = heads.get(i) {
                        let w = self.settings.column_width(*col) + if key == "left" { 16. } else { -16. };
                        self.settings.set_column_width(*col, Some(w.max(64.)));
                    }
                } else if let Some(j) = step(i, heads.len(), key, "left", "right") {
                    self.kbd = Area::Heads(j);
                } else {
                    match key {
                        "enter" | "space" => {
                            if let Some((by, _)) = heads.get(i) {
                                self.sort_by(*by, cx);
                            }
                        }
                        "tab" | "down" => self.kbd = Area::List,
                        _ => return !fkey(key),
                    }
                }
            }
            // Shift+←/→ resizes the preview (← widens it, as its edge is on its left).
            Area::Preview if m.shift && matches!(key, "left" | "right") => {
                let max = window.viewport_size().width.as_f32() * 0.6;
                self.preview_w = (self.preview_w + if key == "left" { 16. } else { -16. }).clamp(260., max.max(260.));
                self.save_session();
            }
            Area::Preview => match key {
                "enter" => {
                    if let Some(r) = self.pane().cursor_row() {
                        let path = r.e.path.clone();
                        self.open_file(path, false, cx);
                    }
                }
                "space" => {
                    let row = self.pane().cursor_row().map(|r| (r.e.path.clone(), r.kind));
                    match (row, self.video.as_ref()) {
                        (_, Some((v, _))) => v.update(cx, |v, cx| v.toggle_play(cx)),
                        (Some((p, crate::kinds::Kind::Audio)), None) => self.toggle_audio(p, cx),
                        // Nothing to play: Space is Quick Look, as in the files.
                        _ => return false,
                    }
                }
                // ↑↓ and the like stay with the files (the preview follows them).
                _ => return false,
            },
        }
        let _ = window;
        cx.notify();
        true
    }

    /// Keys anywhere in the window (Ctrl shortcuts, Alt+arrows…). True if handled.
    fn global_key(&mut self, key: &str, m: gpui::Modifiers, held: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if key == "escape" && self.sidebar_overlay {
            self.sidebar_overlay = false;
            cx.notify();
            return true;
        }
        let ctrl = m.control;
        let typing = !self.focus.is_focused(window);
        if m.alt && !ctrl {
            match key {
                "left" => self.go_back(cx),
                "right" => self.go_forward(cx),
                "up" => self.go_up(cx),
                "p" => self.toggle_preview(window, cx),
                "enter" if !typing => {
                    let paths = self.targets();
                    let paths = if paths.is_empty() { vec![self.pane().path.clone()] } else { paths };
                    self.show_properties(paths, cx);
                }
                _ => return false,
            }
            return true;
        }
        if key == "tab" && !ctrl && !m.alt && !typing && self.tab().split.is_some() {
            self.switch_pane(cx);
            return true;
        }
        if !typing {
            match key {
                "delete" if !ctrl => {
                    let paths = self.targets();
                    self.delete_items(paths, m.shift, cx);
                    return true;
                }
                "f2" => {
                    self.start_rename(cx);
                    return true;
                }
                "f10" if m.shift => {
                    self.keyboard_menu(window, cx);
                    return true;
                }
                "menu" | "contextmenu" | "apps" => {
                    self.keyboard_menu(window, cx);
                    return true;
                }
                "/" if !ctrl => {
                    self.open_palette(None, window, cx);
                    return true;
                }
                _ => {}
            }
        }
        // The preview's text, after a press on it (or F6 to the preview): Ctrl+A selects all of
        // it, Ctrl+C copies what's selected (nothing selected: the files, as before), Esc lets go.
        if !typing && !m.shift && !m.alt && ((ctrl && matches!(key, "a" | "c")) || (!ctrl && key == "escape")) && self.preview_text_key(key, cx) {
            return true;
        }
        if !ctrl {
            return false;
        }
        match key {
            "k" => self.open_palette(None, window, cx),
            "p" if m.shift => self.open_palette(Some(">".into()), window, cx),
            "tab" => self.cycle_tab(if m.shift { -1 } else { 1 }, cx),
            "t" if m.shift => self.reopen_tab(cx),
            "t" => {
                let p = self.pane().path.clone();
                self.new_tab(p, cx);
            }
            "n" if !m.shift => self.new_window(Some(self.pane().path.clone()), cx),
            "w" => {
                if self.tab().split.is_some() && self.tab().split_focused {
                    self.close_split(cx);
                } else {
                    self.close_tab(self.active, cx);
                }
            }
            "\\" => self.toggle_split(cx),
            "l" => self.edit_path(window, cx),
            "f" => self.open_filter(window, cx),
            "," => self.open_settings(cx),
            "b" => self.toggle_sidebar(window, cx),
            "`" => {
                let p = self.pane().path.clone();
                self.open_terminal(p, cx);
            }
            _ if typing => return false,
            "c" if m.shift => {
                let paths = self.targets();
                let paths = if paths.is_empty() { vec![self.pane().path.clone()] } else { paths };
                self.copy_paths(&paths, true, cx);
            }
            "c" => {
                let paths = self.targets();
                self.clipboard(paths, false, cx);
            }
            "x" => {
                let paths = self.targets();
                self.clipboard(paths, true, cx);
            }
            "v" => {
                let dest = self.pane().path.clone();
                self.paste(dest, cx);
            }
            "z" if !m.shift => self.undo(cx),
            "y" | "z" => self.redo(cx),
            "n" if m.shift => {
                if !held {
                    self.new_folder(cx);
                }
            }
            "d" => {
                let rows = self.pane().marked_rows();
                let (path, is_dir) = if rows.len() == 1 {
                    (rows[0].e.path.clone(), rows[0].e.is_dir)
                } else {
                    (self.pane().path.clone(), true)
                };
                self.toggle_favorite(path, is_dir, cx);
            }
            "h" => self.toggle_hidden(cx),
            "1" => self.set_mode(Mode::List, cx),
            "2" => self.set_mode(Mode::Grid, cx),
            "g" => self.cycle_group(cx),
            "=" | "+" | "-" if self.pane().view.mode == Mode::Grid => {
                let size = self.pane().view.icon_size as i32 + if key == "-" { -16 } else { 16 };
                self.set_icon_size(size.clamp(48, 256) as u32, cx);
            }
            "a" => self.select_all(cx),
            _ => return false,
        }
        true
    }

    /// The file list's keys: arrows, Home/End, pages, Enter, Space, Backspace, Esc, letters.
    fn list_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = ev.keystroke.key.as_str();
        let m = ev.keystroke.modifiers;
        let p = self.pane();
        let order: Vec<usize> = p.visible_rows().collect();
        let n = order.len();
        let pos = p.selected.and_then(|s| order.iter().position(|&r| r == s));
        let grid = p.view.mode == Mode::Grid;
        let cols = if grid { p.cols.max(1) } else { 1 };
        let page = {
            let h = if p.uniform() { p.scroll.0.borrow().base_handle.bounds().size.height.as_f32() } else { 500. };
            let ih = if grid { p.view.icon_size as f32 + 58. } else { t::row_h() };
            (((h / ih) as usize).saturating_sub(1).max(1)) * cols
        };
        let to = |f: &mut Self, at: usize, cx: &mut Context<Self>| {
            if n == 0 {
                return;
            }
            let ix = order[at.min(n - 1)];
            if m.control && !m.shift {
                f.move_cursor(ix, cx);
            } else {
                f.select_with(ix, m.shift, m.control && m.shift, cx);
            }
        };
        match key {
            "down" => to(self, pos.map_or(0, |i| (i + cols).min(n.saturating_sub(1))), cx),
            "up" => to(self, pos.map_or(0, |i| i.saturating_sub(cols)), cx),
            // In the grid, Ctrl+Shift+←/→ fold and unfold the group the cursor is in.
            "left" | "right" if grid && m.control && m.shift && self.pane().view.group_by != GroupBy::None => {
                if let Some(label) = self.cursor_group() {
                    let collapsed = self.pane().collapsed.contains(&label);
                    if (key == "left") != collapsed {
                        self.toggle_group(&label, cx);
                    }
                }
            }
            "right" if grid => to(self, pos.map_or(0, |i| (i + 1).min(n.saturating_sub(1))), cx),
            "left" if grid => to(self, pos.map_or(0, |i| i.saturating_sub(1)), cx),
            // In the list, ←/→ fold and unfold the group the cursor is in.
            "left" | "right" if !grid && self.pane().view.group_by != GroupBy::None => {
                if let Some(label) = self.cursor_group() {
                    let collapsed = self.pane().collapsed.contains(&label);
                    if (key == "left") != collapsed {
                        self.toggle_group(&label, cx);
                    }
                }
            }
            // Shift+Tab: up to the column headings (the list view has them).
            "tab" if m.shift && !grid => {
                self.kbd = Area::Heads(self.visible_heads().len().saturating_sub(1));
                cx.notify();
            }
            "pagedown" => to(self, pos.map_or(0, |i| i + page), cx),
            "pageup" => to(self, pos.map_or(0, |i| i.saturating_sub(page)), cx),
            "home" => to(self, 0, cx),
            "end" => to(self, n.saturating_sub(1), cx),
            "enter" => {
                if m.control {
                    if let Some(r) = self.pane().cursor_row().filter(|r| r.e.is_dir) {
                        let path = r.e.path.clone();
                        let ix = self.push_tab(path, None, Some(self.active + 1), cx);
                        let _ = ix;
                    }
                } else {
                    self.open_selected(cx);
                }
            }
            "space" if m.control => {
                if let Some(ix) = self.pane().selected {
                    self.select_with(ix, false, true, cx);
                }
            }
            // Part of a name being typed to jump to it ("a very…"), as the app; else Quick Look.
            "space" if !self.pane().typed.0.is_empty() && self.pane().typed.1.elapsed() < Duration::from_millis(800) => self.type_jump(" ", cx),
            "space" => {
                if let Some((v, _)) = &self.video {
                    v.update(cx, |v, cx| v.toggle_play(cx));
                } else if self.pane().selected.is_some() {
                    self.quicklook = true;
                    cx.notify();
                }
            }
            "backspace" => self.go_up(cx),
            "escape" => {
                if self.filter_box.is_some() {
                    self.close_filter(window, cx);
                } else {
                    self.clear_selection(cx);
                }
            }
            "a" if m.control => self.select_all(cx),
            _ => {
                // Type-to-jump: letters select the next name starting with what was typed.
                let Some(ch) = ev.keystroke.key_char.as_deref().filter(|c| c.chars().count() == 1 && !m.control && !m.alt) else {
                    return false;
                };
                if ch == " " && self.type_ahead.is_none() {
                    return false;
                }
                // A new item's name being typed before its rename box shows: kept for it.
                if let Some((text, at)) = self.type_ahead.as_mut() {
                    if at.elapsed() < Duration::from_secs(3) {
                        text.push_str(ch);
                        return true;
                    }
                    self.type_ahead = None;
                }
                self.type_jump(ch, cx);
            }
        }
        true
    }

    fn type_jump(&mut self, ch: &str, cx: &mut Context<Self>) {
        let p = self.pane_mut();
        if p.typed.1.elapsed() > Duration::from_millis(800) {
            p.typed.0.clear();
        }
        p.typed.0.push_str(&ch.to_lowercase());
        p.typed.1 = Instant::now();
        let typed = p.typed.0.clone();
        let order: Vec<usize> = p.visible_rows().collect();
        let start = p.selected.and_then(|s| order.iter().position(|&r| r == s)).unwrap_or(0);
        // One letter again and again steps through the names starting with it.
        let step = typed.chars().count() == 1 || typed.chars().all(|c| Some(c) == typed.chars().next());
        let needle: String = if step { typed.chars().take(1).collect() } else { typed.clone() };
        let from = if step { start + 1 } else { start };
        let hit = (0..order.len())
            .map(|k| order[(from + k) % order.len().max(1)])
            .find(|&r| crate::filter::fold(&p.rows[r].e.name).starts_with(&needle));
        if let Some(ix) = hit {
            self.select(ix, cx);
        }
    }

    fn cursor_group(&self) -> Option<String> {
        let p = self.pane();
        let ix = p.selected?;
        p.groups.iter().find(|g| (g.start..g.start + g.len).contains(&ix)).map(|g| g.label.clone())
    }

    /// The items a command acts on: the selection, or the cursor.
    pub fn targets(&self) -> Vec<String> {
        self.pane().marked_paths()
    }

    /// Ctrl+B, as the app's `toggleSidebar`: below 860 px the sidebar isn't docked, so the key
    /// slides it over the files (it used to flip the docked setting, with nothing to see).
    pub fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.viewport_size().width.as_f32() < 860. {
            self.sidebar_overlay = !self.sidebar_overlay;
        } else {
            self.sidebar_hidden = !self.sidebar_hidden;
            self.sidebar_overlay = false;
            self.save_session();
        }
        cx.notify();
    }

    pub fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview_open && window.viewport_size().width.as_f32() < 900. {
            self.toast("The window is too narrow for the preview pane. Widen it or use Quick Look (Space)", cx);
            return;
        }
        self.preview_open = !self.preview_open;
        self.save_session();
        cx.notify();
    }

    pub fn new_window(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        // Another window of this prototype, at the folder.
        let exe = std::env::current_exe().ok();
        if let Some(exe) = exe {
            let mut cmd = std::process::Command::new(exe);
            if let Some(p) = path {
                cmd.env("FERRET_GPUI_OPEN", p);
            }
            cmd.env_remove("FERRET_GPUI_BENCH");
            if let Err(e) = cmd.spawn() {
                self.toast_error(format!("Couldn't open a new window: {e}"), cx);
            }
        }
    }

    // ───────────────────────────── editor ─────────────────────────────

    pub fn open_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((video, _)) = self.video.as_ref() else { return };
        if self.editor.is_some() {
            return;
        }
        let path = video.read(cx).path.clone();
        video.update(cx, |v, cx| {
            v.pause();
            cx.notify();
        });
        self.show_editor(vec![path], window, cx);
    }

    /// The selected videos, in one editor with a pager.
    pub fn open_batch_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let videos: Vec<String> =
            self.pane().marked_rows().into_iter().filter(|r| r.kind == crate::kinds::Kind::Video).map(|r| r.e.path.clone()).collect();
        if !videos.is_empty() {
            self.show_editor(videos, window, cx);
        }
    }

    pub fn show_editor(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        if let Some((v, _)) = &self.video {
            v.update(cx, |v, cx| {
                v.pause();
                cx.notify();
            });
        }
        let out = self.bench.as_ref().map(|b| b.out.clone());
        let editor = cx.new(|cx| Editor::new(paths, out, window, cx));
        let sub = cx.subscribe_in(&editor, window, |this, editor, event: &EditorEvent, window, cx| {
            for picture in editor.update(cx, |e, _| e.pictures()) {
                let _ = window.drop_image(picture);
            }
            this.editor = None;
            // The samples it made are deleted with it.
            ferret_core::media::media_release();
            window.focus(&this.focus, cx);
            if let EditorEvent::Compress { jobs, before, estimate } = event {
                this.compress_videos(jobs.clone(), *before, *estimate, cx);
            }
            cx.notify();
        });
        self.editor = Some((editor, sub));
        cx.notify();
    }

    /// The editor's Compress (`compressMedia` in ops.ts): one job on the jobs card; at the end
    /// the new files selected, the sizes on a toast, and an undo step that recycles them.
    fn compress_videos(&mut self, jobs: Vec<ferret_core::media::VideoJob>, before: u64, estimate: u64, cx: &mut Context<Self>) {
        let n = jobs.len();
        let bench = self.bench.as_ref().map(|b| b.out.clone());
        match ferret_core::media::compress_videos(jobs) {
            Ok(id) => crate::ops::when_done(id, move |f, u, cx| {
                if u.status != "done" {
                    return;
                }
                let created = u.created.clone();
                let after: u64 = created.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
                if let Some(out) = &bench {
                    bench::write_to(out, "editor_output", created.join("|"));
                    bench::write_to(out, "editor_output_bytes", after);
                    bench::write_to(out, "editor_estimate_bytes", estimate);
                }
                let what = if created.len() == 1 { util::file_name(&created[0]) } else { crate::files::plural(n, "video") };
                let msg = if after == 0 {
                    format!("Saved {what}")
                } else if after >= before {
                    format!("{what} came out {}, not smaller than the original", crate::kinds::format_size(after))
                } else {
                    format!("{what}: {} → {}, {}% smaller", crate::kinds::format_size(before), crate::kinds::format_size(after), ((1. - after as f64 / before.max(1) as f64) * 100.).round())
                };
                let (c1, c2) = (created.clone(), created.clone());
                f.record(format!("Compress {what}"), move |f, cx| f.recycle_quiet(c1.clone(), cx), move |f, cx| f.unrecycle(c2.clone(), cx), Some(msg), cx);
                f.pane_mut().pending_select = Some(created.clone());
                let dir = f.pane().path.clone();
                f.reload_dir(&dir, cx);
            }),
            Err(e) => self.toast_error(format!("Compress failed: {e}"), cx),
        }
    }

    // ───────────────────────────── benchmark ─────────────────────────────

    fn bench_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.bench.as_mut() else { return };
        if !b.first_frame_seen {
            b.first_frame_seen = true;
            let out = b.out.clone();
            window.on_next_frame(move |_, _| {
                bench::write_to(&out, "first_frame_ms", format!("{:.0}", bench::ms_since_process_start()));
            });
            if let Some(dir) = b.dir.clone() {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(1000)).await;
                    this.update(cx, |this, cx| this.go(dir, cx)).ok();
                })
                .detach();
            }
        }
        if let Some((started, took)) = b.listed.take() {
            let out = b.out.clone();
            let items = self.pane().rows.len();
            window.on_next_frame(move |_, _| {
                bench::write_to(&out, "folder_items", items);
                bench::write_to(&out, "folder_list_ms", format!("{:.0}", took.as_secs_f64() * 1000.));
                bench::write_to(&out, "folder_shown_ms", format!("{:.0}", started.elapsed().as_secs_f64() * 1000.));
            });
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1000)).await;
                this.update(cx, |this, cx| {
                    if let Some(b) = this.bench.as_mut() {
                        b.scroll = Some(bench::Scroll::new());
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let scroll = self.tabs[self.active].pane().scroll.clone();
        let Some(b) = self.bench.as_mut() else { return };
        if let Some(s) = b.scroll.as_mut() {
            let now = Instant::now();
            s.frames.push(now);
            if now - s.start < Duration::from_secs(3) {
                let state = scroll.0.borrow();
                let max = state.base_handle.max_offset().y.as_f32();
                s.y = if s.y + 300. > max { 0. } else { s.y + 300. };
                state.base_handle.set_offset(gpui::point(px(0.), px(-s.y)));
                window.request_animation_frame();
            } else {
                for (k, v) in s.report() {
                    b.write(k, v);
                }
                b.write("done", 1);
                b.scroll = None;
            }
        }
    }

    // ───────────────────────────── Word ─────────────────────────────

    /// Reads the selected .docx in the background (once per selection).
    fn sync_word(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        let want = p.selected.filter(|_| p.marked.len() <= 1).and_then(|i| p.rows.get(i)).filter(|r| crate::preview::is_word(&r.e.ext)).map(|r| r.e.path.clone());
        if self.word.as_ref().map(|(p, _)| p) == want.as_ref() {
            return;
        }
        // The one shown stays on screen while the next item is read (`held`): kept until then.
        if let Some((old, Some(Ok(doc)))) = self.word.take() {
            self.word_before = Some((old, doc));
        }
        self.word = want.clone().map(|p| (p, None));
        let Some(path) = want else { return };
        let task = cx.background_executor().spawn({
            let path = path.clone();
            async move { docx::open(&path).map(Arc::new) }
        });
        cx.spawn(async move |this, cx| {
            let doc = task.await;
            this.update(cx, |this, cx| {
                if let Some((p, slot)) = this.word.as_mut() {
                    if *p == path {
                        *slot = Some(doc);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    // ───────────────────────────── video ─────────────────────────────

    /// Opens a player view when a video is selected, and lets go of it (and its GPU image) when not.
    fn sync_video(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.pane();
        let want = if self.preview_open || self.quicklook {
            p.selected
                .filter(|_| p.marked.len() <= 1)
                .and_then(|i| p.rows.get(i))
                .filter(|r| r.kind == crate::kinds::Kind::Video && !r.e.in_archive && !paths::is_remote(&r.e.path))
                .map(|r| r.e.path.clone())
        } else {
            None
        };
        let current = self.video.as_ref().map(|(v, _)| v.read(cx).path.clone());
        if current == want {
            return;
        }
        // (Leaving a video for something that isn't read yet, its player used to stay, paused,
        // so the pane wasn't empty meanwhile. Its still does that now, where the picture was:
        // the player was centred again in the room the next item's details left it, a jump.)
        // The next video's poster (its thumbnail) stands in when its first frame is slow to
        // come; until then the last video's picture stays (`VideoStill`). The ones either side
        // are asked for now, so arrowing on has them.
        let mut poster = None;
        if let Some(path) = want.clone() {
            poster = self.thumb(&path, 512, false, None, cx);
            let around: Vec<String> = {
                let p = self.pane();
                p.selected
                    .map(|i| [i.wrapping_sub(1), i + 1])
                    .into_iter()
                    .flatten()
                    .filter_map(|i| p.rows.get(i))
                    .filter(|r| r.kind == crate::kinds::Kind::Video && !r.e.in_archive && !paths::is_remote(&r.e.path))
                    .map(|r| r.e.path.clone())
                    .collect()
            };
            for near in around {
                let _ = self.thumb(&near, 512, false, None, cx);
            }
        }
        // The space the last video had: the next one in the same pane starts from it.
        let mut area = None;
        if let Some((old, _)) = self.video.take() {
            let (still, image) = {
                let v = old.read(cx);
                area = Some(v.area());
                (v.still(), v.image())
            };
            // What it showed stays as a still; a picture of its own that isn't that still goes.
            let kept = still.as_ref().and_then(|(s, _)| s.frame());
            if let Some(image) = image.filter(|i| !kept.as_ref().is_some_and(|k| Arc::ptr_eq(k, i))) {
                let _ = window.drop_image(image);
            }
            // (One that showed nothing of its own yet leaves the still before it as it is: that
            // was what was on screen.)
            if let Some((still, at)) = still {
                crate::watchdog::trace("video", || format!("its still is kept, at {:.0},{:.0} {:.0}x{:.0} of the preview", at.origin.x.as_f32(), at.origin.y.as_f32(), at.size.width.as_f32(), at.size.height.as_f32()));
                self.release_still(window);
                self.video_still = Some(crate::preview::VideoStill { still, at, big: self.quicklook, frame: crate::imgmem::current_frame(), since: Instant::now() });
                self.watch_still(window, cx);
            }
        }
        if let Some(path) = want {
            let out = self.bench.as_ref().map(|b| b.out.clone());
            let view = cx.new(|_| VideoView::new(path, self.preview_w - 28., out));
            if let Some(a) = area {
                view.read(cx).set_area(a);
            }
            if let Some(t) = poster {
                view.update(cx, |v, _| v.poster = Some(t));
            }
            let sub = cx.subscribe_in(&view, window, |this, _, event: &VideoEvent, window, cx| match event {
                VideoEvent::Compress => this.open_editor(window, cx),
            });
            self.video = Some((view, sub));
        }
    }

    /// Lets go of the last video's still: nothing draws it any more, and its frame leaves the GPU.
    fn release_still(&mut self, window: &mut Window) {
        let Some(frame) = self.video_still.take().and_then(|s| s.still.frame()) else { return };
        crate::watchdog::trace("video", || "a still's frame is let go".to_string());
        for fade in self.preview_fade.iter_mut().flatten() {
            fade.forget_frame(&frame);
        }
        let _ = window.drop_image(frame);
    }

    /// A still goes once it hasn't been on screen for a while (the fade it's part of is long
    /// over by then). Checked by a task, since nothing may draw the window again by itself.
    fn watch_still(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.still_watched, true) {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let more = this.update_in(cx, |f, window, _| {
                    if f.video_still.as_ref().is_some_and(|s| s.since.elapsed() >= Duration::from_millis(900)) {
                        f.release_still(window);
                    }
                    f.still_watched = f.video_still.is_some();
                    f.still_watched
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn sync_title(&mut self, window: &mut Window) {
        let p = self.pane();
        let mut title = format!("{} - Ferret", util::folder_name(&p.path));
        if self.elevated {
            title.push_str(" (Administrator)");
        }
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
    }

    fn sync_glow(&mut self) {
        let key = (t::accent_hex(), t::is_dark());
        if key != self.glow_key {
            self.glow_key = key;
            self.glow = Arc::new(Image::from_bytes(ImageFormat::Png, t::glow_png(320, 200)));
        }
    }
}

impl Render for Ferret {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        bench::rendered(bench::ROOT);
        crate::imgmem::frame(self, window, cx);
        if self.parts.is_none() {
            let root = cx.entity();
            let sidebar = cx.new(|cx| Part::new(&root, Which::Sidebar, cx));
            let main = cx.new(|cx| Part::new(&root, Which::Files(false), cx));
            let split = cx.new(|cx| Part::new(&root, Which::Files(true), cx));
            self.parts = Some((sidebar, main, split));
        }
        self.scale = window.scale_factor();
        self.viewport_w = window.viewport_size().width.as_f32();
        // A preview's selectable text says it's drawn as it's built (below).
        for s in &self.text_sel {
            s.borrow_mut().shown = false;
        }
        // Tooltips stay away while a menu is open (the button that opened it is still under the
        // pointer, and its tooltip came up over the menu's items).
        crate::ui::set_menu_open(self.menu.is_some());
        if let Some(h) = self.pending_focus.take() {
            window.focus(&h, cx);
        }
        self.sync_glow();
        self.sync_title(window);
        // Mica (and the title bar's own parts) follow the app's theme, as the app's setBackdrop does.
        if self.dwm_dark != Some(t::is_dark()) {
            self.dwm_dark = Some(t::is_dark());
            if let Some(h) = hwnd(window) {
                set_dwm_dark(h, t::is_dark());
            }
        }
        if std::mem::take(&mut self.backdrop_changed) {
            window.set_background_appearance(match t::backdrop() {
                t::Backdrop::Mica => gpui::WindowBackgroundAppearance::MicaBackdrop,
                t::Backdrop::Acrylic => gpui::WindowBackgroundAppearance::Blurred,
                t::Backdrop::Solid => gpui::WindowBackgroundAppearance::Opaque,
            });
        }
        self.bench_frame(window, cx);
        bench::first_frame(window);
        self.sync_video(window, cx);
        self.sync_word(cx);
        self.sync_pdf_unlock(window, cx);
        // FERRET_GPUI_EDITOR asked for the editor: for the selected videos, once listed.
        if self.editor_on_start {
            // A picture selected: its compressor (tests).
            let picture = self.pane().cursor_row().filter(|r| crate::ops::is_compressible_picture(&r.e.ext)).map(|r| r.e.path.clone());
            if let Some(p) = picture {
                self.editor_on_start = false;
                self.show_image_editor(vec![p], window, cx);
            } else if self.pane().marked.len() > 1 {
                self.editor_on_start = false;
                self.open_batch_editor(window, cx);
            } else if self.video.as_ref().is_some_and(|(v, _)| v.read(cx).is_ok()) {
                self.editor_on_start = false;
                self.open_editor(window, cx);
            }
        }
        let width = window.viewport_size().width.as_f32();
        // Below 860px the sidebar is an overlay (opened from the toolbar).
        let docked = width >= 860. && !self.sidebar_hidden;
        let preview = self.preview_open && width >= 900.;
        let (sidebar, main, split) = self.parts.clone().unwrap();
        let split_open = self.tab().split.is_some();
        let sidebar_w = self.sidebar_w;
        div()
            .id("root")
            .track_focus(&self.focus)
            // To a screen reader the window is one group holding the focus; the item under the
            // keyboard is its active descendant (see `row`).
            .role(gpui::Role::Group)
            .aria_label(self.a11y_name())
            .on_key_down(cx.listener(Self::key))
            // A click anywhere gives the keyboard back to the files (from the area F6 put it in).
            // While autoscroll follows the mouse, the next press only stops it.
            .capture_any_mouse_down(cx.listener(|f, ev: &gpui::MouseDownEvent, window, cx| {
                if f.autoscroll.as_ref().is_some_and(|a| a.sticky) {
                    f.stop_autoscroll(cx);
                    cx.stop_propagation();
                    return;
                }
                // A press anywhere outside the rename box renames (as Explorer). Only a press on
                // empty space in the list did: one on another row, the sidebar or the preview
                // left the box open, still taking the keys (user report, 2026-11-13).
                let outside = f.rename.as_ref().is_some_and(|r| {
                    r.input.read(cx).bounds().is_none_or(|b| !b.dilate(px(8.)).contains(&ev.position))
                });
                if outside {
                    f.commit_rename(cx);
                    // The keys come back to the files (the box had them); a field pressed takes
                    // them after this.
                    window.focus(&f.focus, cx);
                }
                // A press anywhere lets go of the preview's selected text (a press on that text
                // picks again, after this); Ctrl+C is for the files again.
                // (A press while a menu is open picks from it: Copy needs the selection.)
                if ev.button == gpui::MouseButton::Left && f.menu.is_none() {
                    for s in &f.text_sel {
                        let mut s = s.borrow_mut();
                        if s.has_selection() && !ev.modifiers.shift {
                            s.clear();
                            cx.notify();
                        }
                        s.active = false;
                    }
                }
                if f.kbd != Area::List {
                    f.kbd = Area::List;
                    cx.notify();
                }
            }))
            .on_mouse_move(cx.listener(Self::on_resize_move))
            .on_mouse_move(cx.listener(|f, ev: &gpui::MouseMoveEvent, _, cx| {
                if let Some(a) = f.autoscroll.as_mut() {
                    a.at = ev.position;
                    cx.notify();
                }
            }))
            .on_mouse_up(gpui::MouseButton::Middle, cx.listener(|f, _, _, cx| f.autoscroll_up(cx)))
            // Files dragged over the window: their label follows the pointer.
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(|f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                let paths = ev.drag(cx).paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
                f.note_drag(ev.event.position, paths, cx);
            }))
            .on_scroll(cx.listener(|f, _, _, cx| f.stop_autoscroll(cx)))
            .when_some(self.autoscroll_cursor(), |d, c| d.cursor(c))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|f, _, _, cx| f.end_mouse(cx)))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|f, _, _, cx| f.end_mouse(cx)))
            .on_mouse_down(gpui::MouseButton::Navigate(gpui::NavigationDirection::Back), cx.listener(|f, _, _, cx| f.go_back(cx)))
            .on_mouse_down(gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward), cx.listener(|f, _, _, cx| f.go_forward(cx)))
            .size_full()
            .relative()
            .bg(t::window_bg())
            .font_family(t::FONT)
            .text_size(px(13.))
            .text_color(t::fg())
            .when(self.resizing.is_some(), |d| d.cursor(gpui::CursorStyle::ResizeLeftRight))
            // `.app`'s accent glow behind the panels.
            .child(img(self.glow.clone()).absolute().top_0().left_0().size_full().object_fit(ObjectFit::Fill))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .gap(px(8.))
                    .p(px(if window.is_maximized() { 0. } else { 8. }))
                    // The sidebar: a cached view, like the file lists.
                    .when(docked, |d| {
                        d.child(embed(sidebar.clone(), StyleRefinement::default().w(px(sidebar_w)).h_full().flex_none(), window))
                            .child(self.resize_handle(true, cx))
                    })
                    .child(self.main_panel(main, split, split_open, preview, window, cx)),
            )
            // A narrow window: the sidebar slides in over a scrim (the toolbar's button; Esc or a click away closes it).
            .when(width < 860. && self.sidebar_overlay, |d| {
                let side = sidebar.clone();
                d.child(
                    div()
                        .id("side-scrim")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .bg(t::scrim())
                        .occlude()
                        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|f, _, _, cx| {
                            f.sidebar_overlay = false;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .absolute()
                                .top(px(8.))
                                .left(px(8.))
                                .bottom(px(8.))
                                .w(px(280.))
                                // Solid, as the app's `.sidebar.overlay` (the glass panel let the
                                // file names show through it).
                                .rounded(px(12.))
                                .bg(t::panel_solid())
                                .shadow(t::shadow())
                                .overflow_hidden()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(embed(side, StyleRefinement::default().size_full(), window))
                                .with_animation("side-in", gpui::Animation::new(Duration::from_millis(if t::animate() { 160 } else { 1 })).with_easing(gpui::ease_out_quint()), |d, k| d.ml(px((1. - k) * -24.)).opacity(k)),
                        ),
                )
            })
            .children(self.quicklook_view(window, cx))
            // Jobs and toasts over Quick Look (a "Saved …" from it showed only once it closed),
            // under dialogs and menus.
            .children(self.jobs_card(cx))
            .children(self.toasts_view(cx))
            // Menus, dialogs, Ctrl+K and the editor over everything (the video included: GPUI
            // draws it like any other element).
            .children(self.dialog_view(window, cx))
            // Out of the way while a result is dragged, to show the folders it can go to.
            .children(self.palette.as_ref().filter(|_| !self.own_drag).map(|(p, _)| p.clone()))
            .children(self.editor.as_ref().map(|(e, _)| e.clone()))
            .children(self.menu_view(window, cx))
            .children(self.autoscroll_mark())
            .children(self.drag_label(cx))
    }
}

/// Files dragged over the window (`drag.ts`'s ghost and `externalDrag` banner).
pub struct DragHint {
    pub at: gpui::Point<gpui::Pixels>,
    pub paths: Vec<String>,
    /// The drop target under the pointer, and how specific it is (a folder row beats its list).
    pub target: Option<(String, u8)>,
}

/// Lets a drop target say it's under a drag (for the drag's label).
pub fn drag_target<E: gpui::InteractiveElement>(el: E, path: String, rank: u8, cx: &Context<Ferret>) -> E {
    el.on_drag_move::<gpui::ExternalPaths>(cx.listener(move |f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
        if ev.bounds.contains(&ev.event.position) {
            f.note_drag_target(ev.event.position, &path, rank, cx);
        }
    }))
}

/// A handler for a screen reader's action (Click, Focus…) that runs on Ferret.
pub fn on_a11y(
    cx: &Context<Ferret>,
    f: impl Fn(&mut Ferret, &mut Window, &mut Context<Ferret>) + 'static,
) -> impl FnMut(Option<&gpui::accesskit::ActionData>, &mut Window, &mut App) + 'static {
    let this = cx.entity().downgrade();
    move |_, window, cx| {
        let _ = this.update(cx, |me, cx| f(me, window, cx));
    }
}
/// A part of the window (sidebar, file list) as a cached view, reused until its state changes.
/// While a screen reader listens it's drawn every frame instead: GPUI builds the accessibility
/// tree as it draws, and a reused view would leave its nodes out of it.
fn embed<T: Render>(part: Entity<T>, style: StyleRefinement, window: &Window) -> AnyElement {
    if window.is_a11y_active() {
        let mut d = div();
        *d.style() = style;
        d.child(part).into_any_element()
    } else {
        part.cached(style).into_any_element()
    }
}

impl Ferret {
    /// A drag moved over the window: where, what (a new spot forgets the target under the old one).
    fn note_drag(&mut self, at: gpui::Point<gpui::Pixels>, paths: Vec<String>, cx: &mut Context<Self>) {
        match self.drag_hint.as_mut() {
            Some(h) if h.at == at => h.paths = paths,
            _ => self.drag_hint = Some(DragHint { at, paths, target: None }),
        }
        cx.notify();
    }

    pub fn note_drag_target(&mut self, at: gpui::Point<gpui::Pixels>, path: &str, rank: u8, cx: &mut Context<Self>) {
        let h = self.drag_hint.get_or_insert_with(|| DragHint { at, paths: Vec::new(), target: None });
        if h.at != at {
            h.at = at;
            h.target = None;
        }
        if h.target.as_ref().is_none_or(|(_, r)| rank >= *r) {
            h.target = Some((path.to_string(), rank));
            cx.notify();
        }
    }

    /// The label under the pointer while files are dragged over the window: for its own drags,
    /// "Move 2 items to Documents" next to the pointer (copy or move as it would drop, Ctrl and
    /// Shift read as they change); for other apps', a banner, "Drop 3 items to copy into Downloads".
    fn drag_label(&self, cx: &App) -> Option<AnyElement> {
        let h = self.drag_hint.as_ref().filter(|_| cx.has_active_drag())?;
        let n = h.paths.len();
        let what = if n == 1 { util::file_name(&h.paths[0]) } else { format!("{n} items") };
        let target = h.target.as_ref().map(|(p, _)| p.clone());
        let name = |p: &str| if p == "dropzone" { "the Dropzone".to_string() } else { util::folder_name(p) };
        if !self.own_drag {
            let into = target.as_deref().map(name).unwrap_or_else(|| "this folder".into());
            // What the drop will do: copy, or move when the other app allows it (Explorer, on
            // the same drive), as `drop_paths` decides it.
            let dest = target.clone().unwrap_or_else(|| self.pane().path.clone());
            // The Dropzone keeps them where they are (the app's banner: "…to add to the Dropzone").
            let (verb, into) = if dest == "dropzone" {
                ("add", "the Dropzone".to_string())
            } else {
                (self.drop_op(&h.paths, &dest, gpui::Modifiers::default()), into)
            };
            return Some(
                div()
                    .absolute()
                    .top(px(14.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px(px(16.))
                            .py(px(9.))
                            .rounded(px(10.))
                            .bg(t::accent())
                            .text_color(t::on_accent())
                            .shadow(t::shadow())
                            .flex()
                            .gap(px(4.))
                            .child(format!("Drop {} to {verb} {}", if n > 1 { format!("{n} items") } else { "here".into() }, if verb == "add" { "to" } else { "into" }))
                            .child(div().font_weight(gpui::FontWeight::BOLD).child(into)),
                    )
                    .into_any_element(),
            );
        }
        let text = match target.as_deref() {
            None => what.clone(),
            Some("dropzone") => format!("Add {what} to the Dropzone"),
            Some(p) if ferret_core::recycle::is_recycle(p) => format!("Move {what} to the Recycle Bin"),
            Some(p) => {
                let (ctrl, shift) = crate::ops::held_keys();
                let archive = h.paths.iter().any(|s| ferret_core::archive::split_virtual(s).is_some_and(|(_, inner)| !inner.is_empty()));
                let same_volume = h.paths.iter().all(|s| crate::ops::volume(s) == crate::ops::volume(p));
                let verb = if archive { "Extract" } else if ctrl { "Copy" } else if shift || same_volume { "Move" } else { "Copy" };
                format!("{verb} {what} to {}", name(p))
            }
        };
        Some(
            div()
                .absolute()
                .left(h.at.x + px(14.))
                .top(h.at.y + px(12.))
                .max_w(px(360.))
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(6.))
                .pr(px(12.))
                .py(px(6.))
                .rounded(px(9.))
                .bg(t::raised())
                .border_1()
                .border_color(t::accent_line())
                .shadow(t::shadow())
                .text_size(px(12.))
                .child(
                    div()
                        .flex_none()
                        .min_w(px(22.))
                        .h(px(22.))
                        .px(px(6.))
                        .rounded(px(11.))
                        .bg(t::accent())
                        .text_color(t::on_accent())
                        .font_weight(gpui::FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(n.to_string()),
                )
                .child(div().min_w_0().truncate().child(text))
                .into_any_element(),
        )
    }

    /// The window's name to a screen reader: the folder shown.
    fn a11y_name(&self) -> String {
        let p = self.pane();
        format!("{} - Ferret", util::folder_name(&p.path))
    }

    fn main_panel(
        &mut self,
        main: Entity<Part>,
        split: Entity<Part>,
        split_open: bool,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let preview_el: Option<AnyElement> = preview.then(|| self.preview(window, cx));
        crate::ui::panel()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .when(window.is_maximized(), |d| d.rounded(px(0.)).border_0())
            .child(self.titlebar(window, cx))
            .child(self.toolbar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            // The file lists: cached views, reused until the window's state changes.
                            .child(embed(main, StyleRefinement::default().flex_1().min_w_0().h_full(), window))
                            .when(split_open, |d| {
                                d.child(div().w(px(1.)).h_full().bg(t::line()))
                                    .child(embed(split, StyleRefinement::default().flex_1().min_w_0().h_full(), window))
                            }),
                    )
                    .when_some(preview_el, |d, el| {
                        // F6 to the preview: Enter opens the item, Space plays or pauses.
                        let ringed = self.ring_area(window) == Area::Preview;
                        // To a screen reader: a region named for the item, its text readable.
                        let name = self.pane().cursor_row().map(|r| r.name.to_string()).unwrap_or_default();
                        let text = window.is_a11y_active().then(|| self.preview_data.as_ref().and_then(|l| crate::render::spoken_text(&l.content))).flatten();
                        d.child(self.resize_handle(false, cx)).child(
                            div()
                                .id("preview")
                                .role(gpui::Role::Complementary)
                                .aria_label(if name.is_empty() { "Preview".to_string() } else { format!("Preview of {name}") })
                                .relative()
                                .h_full()
                                .flex_none()
                                .child(el)
                                .children(text.map(|t| div().id("preview-text").role(gpui::Role::Document).aria_label(format!("Text of {name}")).aria_value(t).absolute().size_0()))
                                .when(ringed, |d| d.child(crate::ui::ring(6.))),
                        )
                    }),
            )
            .child(self.status_bar(window, cx))
    }

    /// The thin grip between the sidebar (or preview) and the files: drag to resize.
    fn resize_handle(&self, sidebar: bool, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(if sidebar { "side-grip" } else { "preview-grip" })
            .flex_none()
            .w(px(if sidebar { 0. } else { 1. }))
            .h_full()
            .relative()
            .when(!sidebar, |d| d.bg(t::line()))
            .child(
                div()
                    .id(if sidebar { "side-grip-hit" } else { "preview-grip-hit" })
                    .absolute()
                    .top_0()
                    .bottom_0()
                    // The preview's grip lies on its own side of the line: over the list it took
                    // presses meant for the list's scrollbar (and resized the pane instead).
                    .left(px(if sidebar { -6. } else { 0. }))
                    .w(px(8.))
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .hover(|s| s.bg(t::accent_a(0.25)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |f, ev: &gpui::MouseDownEvent, _, cx| {
                            let x = ev.position.x.as_f32();
                            f.resizing = Some(if sidebar {
                                Resize::Sidebar { from: x, start: f.sidebar_w }
                            } else {
                                Resize::Preview { from: x, start: f.preview_w }
                            });
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |f, ev: &gpui::ClickEvent, _, cx| {
                        // Double-click puts it back.
                        if ev.click_count() >= 2 {
                            if sidebar { f.sidebar_w = 248. } else { f.preview_w = 360. }
                            f.save_session();
                            cx.notify();
                        }
                    })),
            )
    }

    fn on_resize_move(&mut self, ev: &gpui::MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A press on a row becomes a drag from here too, not only from the row's own handler:
        // that one only hears moves while GPUI has the row as hovered, and on the click that
        // activates the window it didn't, so dragging out of an inactive window only selected
        // (Explorer drags in one go; real drag test, 2026-11-11).
        if self.press.is_some() {
            self.press_move(ev, window, cx);
        }
        if let Some(r) = self.resizing {
            let x = ev.position.x.as_f32();
            let width = window.viewport_size().width.as_f32();
            match r {
                Resize::Sidebar { from, start } => self.sidebar_w = (start + x - from).clamp(190., 420.),
                Resize::Preview { from, start } => self.preview_w = (start - (x - from)).clamp(260., width * 0.6),
                Resize::Column { col, from, start } => {
                    let w = (start - (x - from)).max(64.);
                    self.settings.0["columnWidths"][col.key()] = json!(w.round());
                }
            }
            cx.notify();
            return;
        }
        if self.thumb_drag.is_some() {
            if ev.pressed_button == Some(gpui::MouseButton::Left) {
                self.scrollbar_drag(ev.position.y.as_f32(), cx);
                return;
            }
            // Let go outside the window.
            self.thumb_drag = None;
            cx.notify();
        }
        self.band_move(ev, window, cx);
    }

    fn end_mouse(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = self.resizing.take() {
            if let Resize::Column { .. } = r {
                self.settings.save();
            } else {
                self.save_session();
            }
            cx.notify();
        }
        if self.band.take().is_some() {
            cx.notify();
        }
        if self.thumb_drag.take().is_some() {
            cx.notify();
        }
        self.press = None;
    }

    fn toasts_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.toasts.is_empty() {
            return None;
        }
        // As the app: over the files, clear of the sidebar and above the status bar.
        let docked = self.viewport_w >= 860. && !self.sidebar_hidden;
        Some(
            div()
                .absolute()
                .left(px(if docked { self.sidebar_w + 30. } else { 18. }))
                .bottom(px(52.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(self.toasts.iter().map(|toast| {
                    let id = toast.id;
                    // Read out as it appears (errors interrupt).
                    let error = toast.error;
                    let el = div()
                        .id(("toast", id as usize))
                        .role(gpui::Role::Status)
                        .aria_label(toast.text.clone())
                        .a11y_synthetic_children(move |b| b.parent_node().set_live(if error { gpui::accesskit::Live::Assertive } else { gpui::accesskit::Live::Polite }))
                        .max_w(px(460.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .pl(px(14.))
                        .pr(px(if toast.action.is_some() { 6. } else { 14. }))
                        .py(px(8.))
                        .rounded(px(10.))
                        .bg(t::raised())
                        .border_1()
                        .border_color(if toast.error { t::danger() } else { t::line2() })
                        .shadow(t::shadow())
                        .text_size(px(12.5))
                        .child(div().flex_1().min_w_0().child(toast.text.clone()))
                        .children(toast.action.as_ref().map(|(label, run)| {
                            let run = run.clone();
                            div()
                                .id(("toast-act", id as usize))
                                .role(gpui::Role::Button)
                                .aria_label(label.clone())
                                .flex_none()
                                .px(px(10.))
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .rounded(px(6.))
                                .text_color(t::accent_text())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .hover(|s| s.bg(t::hover()))
                                .on_click(cx.listener(move |f, _, window, cx| {
                                    f.toasts.retain(|t| t.id != id);
                                    run(f, window, cx);
                                    cx.notify();
                                }))
                                .child(label.clone())
                        }));
                    if t::animate() {
                        el.with_animation(
                            ("toast-in", id as usize),
                            gpui::Animation::new(Duration::from_millis(180)).with_easing(gpui::ease_out_quint()),
                            |el, d| el.opacity(d).mt(px((1. - d) * 10.)),
                        )
                        .into_any_element()
                    } else {
                        el.into_any_element()
                    }
                }))
                .into_any_element(),
        )
    }

    pub fn jobs_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        crate::ops::jobs_view(self, cx)
    }
}

/// A `folder-size` event's size.
fn folder_size_of(v: &serde_json::Value) -> Option<ferret_core::foldersize::FolderSize> {
    Some(ferret_core::foldersize::FolderSize {
        path: v["path"].as_str()?.to_string(),
        bytes: v["bytes"].as_u64().unwrap_or(0),
        files: v["files"].as_u64(),
        partial: v["partial"].as_bool().unwrap_or(false),
        done: v["done"].as_bool().unwrap_or(false),
        refreshing: v["refreshing"].as_bool().unwrap_or(false),
        counted_at: v["countedAt"].as_i64().unwrap_or(0),
    })
}

/// Appends a line to the prototype's log (`%TEMP%\ferret-gpui\ferret-gpui.log`).
pub fn log_line(text: &str) {
    use std::io::Write;
    let dir = std::env::temp_dir().join("ferret-gpui");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("ferret-gpui.log")) {
        let _ = writeln!(f, "{} {text}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}
