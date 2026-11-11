//! The preview pane and Quick Look (PreviewPane.tsx, Preview.tsx): the selected item drawn by
//! its renderer (render.rs), its details, the folder's summary when nothing is selected, a
//! summary of several items, and Quick Look over the window (Space) stepping through them.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::AnimationExt;
use gpui::{
    AnyElement, Context, Div, Entity, FontWeight, MouseButton, ObjectFit, SharedString, StyledText, Window, div, img, prelude::*, px,
    relative,
};

use crate::docx;
use crate::input::TextInput;
use crate::kinds::{self, Kind};
use crate::render::{self, Content};
use crate::selectable::{self, Builder, Lines};
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util;
use crate::view::Ferret;

/// A PDF that wants a password: the field it's typed in, under the lock in the preview.
pub struct PdfUnlock {
    /// The item previewed, and the file read (its local copy for archive entries and servers).
    pub path: String,
    pub real: String,
    pub input: Entity<TextInput>,
    pub error: Option<SharedString>,
    pub busy: bool,
    _sub: gpui::Subscription,
}

/// The previewed item's content: which item (path + date + size), and what was read.
pub struct Loaded {
    pub key: String,
    pub content: Content,
    pub at: Instant,
    /// Another item than before: it lifts in. The same one again (drawn for another size) just swaps.
    pub lift: bool,
}

impl Ferret {
    /// Quick Look reads the pictures either side of the one shown ahead (PreviewPane.tsx's
    /// look-ahead: one next, one previous, wrapping around), so stepping to them is instant.
    fn warm_neighbours(&mut self, cx: &mut Context<Self>) {
        let p = self.pane();
        let order: Vec<usize> = p.visible_rows().collect();
        let Some(at) = p.selected.and_then(|s| order.iter().position(|&r| r == s)) else { return };
        let n = order.len();
        if n < 2 {
            return;
        }
        let mut todo = Vec::new();
        for j in [(at + 1) % n, (at + n - 1) % n] {
            let Some(r) = p.rows.get(order[j]) else { continue };
            if r.e.is_dir || r.e.in_archive || r.kind != Kind::Image || ferret_core::paths::is_remote(&r.e.path) {
                continue;
            }
            let key = format!("{}|{}|{}|0", r.e.path, r.e.modified.unwrap_or(0), r.e.size);
            let known = self.preview_data.as_ref().is_some_and(|l| l.key == key) || self.preview_cache.iter().any(|l| l.key == key) || self.warming.contains(&key);
            if !known {
                todo.push((key, r.e.path.clone(), r.e.ext.clone(), r.kind, r.e.size));
            }
        }
        for (key, path, ext, kind, size) in todo {
            self.warming.insert(key.clone());
            cx.spawn(async move |this, cx| {
                let content = cx.background_executor().spawn(async move { render::load(&path, &ext, kind, size, 0) }).await;
                this.update(cx, |f, _| {
                    f.warming.remove(&key);
                    if !f.preview_cache.iter().any(|l| l.key == key) && f.preview_data.as_ref().is_none_or(|l| l.key != key) {
                        f.preview_cache.push(Loaded { key, content, at: Instant::now(), lift: true });
                        if f.preview_cache.len() > 6 {
                            f.preview_cache.remove(0);
                        }
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Starts reading the cursor's item for the preview, 140 ms after it settles (holding an
    /// arrow key doesn't read every file it passes).
    pub fn sync_preview(&mut self, cx: &mut Context<Self>) {
        if !self.preview_open && !self.quicklook {
            return;
        }
        if self.quicklook {
            self.warm_neighbours(cx);
        }
        let p = self.pane();
        let Some(r) = p.cursor_row().filter(|_| p.marked.len() <= 1) else {
            self.preview_want = None;
            return;
        };
        if r.e.is_dir {
            self.preview_want = None;
            return;
        }
        // PDFs are drawn for the width they're shown at (the pane, or Quick Look): another width reads them again.
        let shown_w = if self.quicklook { (self.viewport_w - 120.).max(300.) - 50. } else { self.preview_w - 28. };
        let page_px = if r.kind == Kind::Pdf { ((shown_w - 12.).max(80.) * self.scale).round() as u32 } else { 0 };
        let key = format!("{}|{}|{}|{page_px}", r.e.path, r.e.modified.unwrap_or(0), r.e.size);
        if self.preview_data.as_ref().is_some_and(|l| l.key == key) || self.preview_want.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        // Drawn before (the pane and Quick Look draw PDFs at their own sizes): swapped back at once.
        if let Some(i) = self.preview_cache.iter().position(|l| l.key == key) {
            let mut cached = self.preview_cache.remove(i);
            cached.lift = false;
            if let Some(old) = self.preview_data.replace(cached) {
                self.preview_cache.push(old);
            }
            self.preview_want = None;
            return;
        }
        let (path, ext, kind, size, inside) = (r.e.path.clone(), r.e.ext.clone(), r.kind, r.e.size, r.e.in_archive || ferret_core::paths::is_remote(&r.e.path));
        self.preview_want = Some((key.clone(), Instant::now()));
        let delay = if self.quicklook { 0 } else { 140 };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(delay)).await;
            let still = this.update(cx, |f, _| f.preview_want.as_ref().is_some_and(|(k, _)| *k == key)).unwrap_or(false);
            if !still {
                return;
            }
            let content = cx
                .background_executor()
                .spawn(async move {
                    // Archive entries and server files are read from a local copy.
                    let real = if inside {
                        if ferret_core::paths::is_remote(&path) {
                            ferret_core::rt::block_on(ferret_core::remote::remote_materialize(path.clone())).unwrap_or(path.clone())
                        } else {
                            ferret_core::rt::block_on(ferret_core::archive::archive_materialize(path.clone(), None)).unwrap_or(path.clone())
                        }
                    } else {
                        path.clone()
                    };
                    render::load(&real, &ext, kind, size, page_px)
                })
                .await;
            this.update(cx, |f, cx| {
                if f.preview_want.as_ref().is_some_and(|(k, _)| *k == key) {
                    f.preview_want = None;
                    if let Content::Font { bytes, .. } = &content {
                        if let Some(b) = bytes.clone() {
                            let _ = cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(b)]);
                        }
                    }
                    let path = key.split('|').next().unwrap_or("").to_string();
                    let same = f.preview_data.as_ref().is_some_and(|l| l.key.starts_with(&format!("{path}|")));
                    if !same {
                        f.preview_view = Default::default();
                        // JSON and Markdown start wrapped (long strings under their key, as the app's
                        // tree; prose lines in the source); code and logs start unwrapped (as the
                        // app), scrolling sideways.
                        f.preview_view.wrap = matches!(content, Content::Json { .. } | Content::BadJson { .. } | Content::Markdown { .. });
                        f.pdf_scroll = [gpui::ScrollHandle::new(), gpui::ScrollHandle::new()];
                    }
                    if let Some(old) = f.preview_data.replace(Loaded { key, content, at: Instant::now(), lift: !same }) {
                        f.preview_cache.push(old);
                        if f.preview_cache.len() > 6 {
                            f.preview_cache.remove(0);
                        }
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn header(&self, icon_el: AnyElement, title: SharedString, sub: SharedString, open: Option<String>, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .pt(px(16.))
            .pb(px(14.))
            .child(div().size(px(36.)).flex_none().flex().items_center().justify_center().child(icon_el))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().truncate().text_size(px(12.5)).text_color(t::fg3()).child(sub)),
            )
            .when_some(open, |d, path| {
                let p2 = path.clone();
                d.child(ui::icon_btn("pv-open", "external-link", true).tooltip(ui::tip("Open (Enter)")).on_click(cx.listener(move |f, _, _, cx| {
                    if std::path::Path::new(&path).is_dir() { f.go(path.clone(), cx) } else { f.open_file(path.clone(), false, cx) }
                })))
                .child(ui::icon_btn("pv-ql", "maximize-2", true).tooltip(ui::tip("Quick Look (Space)")).on_click(cx.listener(move |f, _, _, cx| {
                    let _ = &p2;
                    f.quicklook = true;
                    cx.notify();
                })))
            })
    }

    pub fn preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_preview(cx);
        let w = self.preview_w;
        let pane = div().w(px(w)).flex_none().h_full().flex().flex_col().overflow_hidden();
        let p = self.pane();
        // Several selected: what they are, how big together, and the batch compress for videos.
        if p.marked.len() > 1 {
            let picked = p.marked_rows();
            let folders = picked.iter().filter(|r| r.e.is_dir).count();
            let files = picked.len() - folders;
            // Folders by their counted size, as the status bar adds them up ("+" while some
            // aren't counted yet): files alone said "0 B" for a selection of folders.
            let mut bytes: u64 = 0;
            let mut uncounted = false;
            for r in &picked {
                if r.e.is_dir {
                    match self.sizes.get(&ferret_core::paths::key(&r.e.path)) {
                        Some(s) if s.done => bytes += s.bytes,
                        _ => uncounted = true,
                    }
                } else {
                    bytes += r.e.size;
                }
            }
            let total = format!("{}{}", kinds::format_size(bytes), if uncounted { "+" } else { "" });
            let videos = picked.iter().filter(|r| r.kind == Kind::Video).count();
            let pictures = picked.iter().filter(|r| crate::ops::is_compressible_picture(&r.e.ext)).count();
            let mut seen: Vec<(Kind, usize)> = Vec::new();
            for r in &picked {
                match seen.iter_mut().find(|(k, _)| *k == r.kind) {
                    Some((_, n)) => *n += 1,
                    None => seen.push((r.kind, 1)),
                }
            }
            seen.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            let n = picked.len();
            let dir = p.path.clone();
            let action_btn = |id: &'static str, icon_name: &'static str, label: String| {
                div()
                    .id(id)
                    .mx(px(16.))
                    .mt(px(10.))
                    .h(px(32.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .bg(t::accent_softer())
                    .border_1()
                    .border_color(t::accent_line())
                    .text_color(t::accent_text())
                    .hover(|s| s.bg(t::accent_soft()))
                    .child(icon(icon_name, 15., t::accent_text()))
                    .child(label)
            };
            let pics: Vec<String> = picked.iter().filter(|r| crate::ops::is_compressible_picture(&r.e.ext)).map(|r| r.e.path.clone()).collect();
            return pane
                .child(self.header(div().text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(t::accent_text()).child(n.to_string()).into_any_element(), format!("{n} items selected").into(), total.clone().into(), None, cx))
                .child(div().px(px(16.)).flex().flex_wrap().gap(px(6.)).children(seen.into_iter().map(|(k, n)| {
                    div().flex().items_center().gap(px(5.)).px(px(8.)).py(px(3.)).rounded(px(6.)).bg(t::hover()).text_size(px(12.)).text_color(t::fg2()).child(icon(kinds::icon(k), 13., kinds::color(k))).child(format!("{n}"))
                })))
                .when(videos > 0, |d| d.child(action_btn("batch-compress", "clapperboard", format!("Compress {videos} video{}…", if videos == 1 { "" } else { "s" })).on_click(cx.listener(|f, _, window, cx| f.open_batch_editor(window, cx)))))
                .when(pictures > 0, |d| d.child(action_btn("batch-pics", "image-down", format!("Compress {pictures} picture{}…", if pictures == 1 { "" } else { "s" })).on_click(cx.listener(move |f, _, window, cx| f.open_image_editor(pics.clone(), window, cx)))))
                .child(div().flex_1())
                .child(meta(vec![("Selected", format!("{} files, {} folders", kinds::group_digits(files), kinds::group_digits(folders)).into()), ("Size", total.into()), ("In", dir.into())], self.just_copied(), cx))
                .into_any_element();
        }
        match p.cursor_row() {
            Some(r) => {
                let (path, kind, is_dir, ext) = (r.e.path.clone(), r.kind, r.e.is_dir, r.e.ext.clone());
                let mut facts: Vec<(&'static str, SharedString)> = vec![("Type", r.ty.clone())];
                if !is_dir {
                    facts.push(("Size", format!("{} ({} bytes)", r.size, kinds::group_digits(r.e.size as usize)).into()));
                }
                let bin = p.is_bin();
                let origin = r.e.origin.clone();
                let (date, created) = (r.date.clone(), kinds::format_date(r.e.created));
                let name = r.name.clone();
                let ty = r.ty.clone();
                let header_icon = self.thumb_or_icon(&path, kind, is_dir, r.e.modified, cx);
                let header = self.header(header_icon, name, ty, Some(path.clone()), cx);
                let body: AnyElement = if is_dir {
                    let body = self.folder_summary_for(&path, cx);
                    // What's in it, as the app's FolderView adds to the details.
                    if let Some((_, Some(Ok(entries)))) = self.folder_peek.as_ref().filter(|(p, _)| util::same(p, &path)) {
                        let shown: Vec<_> = entries.iter().filter(|e| crate::pane::show_hidden() || !e.hidden).collect();
                        let files = shown.iter().filter(|e| !e.is_dir).count();
                        let bytes: u64 = shown.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
                        let hidden = entries.iter().filter(|e| e.hidden).count();
                        facts.push(("Contains", format!("{}, {}", crate::files::plural(files, "file"), crate::files::plural(shown.len() - files, "folder")).into()));
                        if files > 0 {
                            facts.push(("Size of files", kinds::format_size(bytes).into()));
                        }
                        if hidden > 0 {
                            facts.push(("Hidden", kinds::group_digits(hidden).into()));
                        }
                    }
                    body
                } else {
                    let key_ok = self.preview_data.as_ref().is_some_and(|l| l.key.starts_with(&format!("{path}|")));
                    let loaded = key_ok && !matches!(self.preview_data.as_ref().unwrap().content, render::Content::Loading);
                    if key_ok {
                        let extra = render::facts(&self.preview_data.as_ref().unwrap().content);
                        facts.extend(extra.into_iter().map(|(k, v)| (k, SharedString::from(v))));
                    }
                    // While it loads, the rows it will have are there already ("…"), so the
                    // details don't jump down and back up on every switch between pictures
                    // (user report, 2026-11-11).
                    if !loaded {
                        let pending: &[&'static str] = match kind {
                            Kind::Image => &["Dimensions"],
                            Kind::Pdf => &["Pages"],
                            _ => &[],
                        };
                        for label in pending {
                            if !facts.iter().any(|(k, _)| k == label) {
                                facts.push((label, "…".into()));
                            }
                        }
                    }
                    self.content_view(&path, &ext, kind, w - 28., false, window, cx)
                };
                if bin {
                    facts.push(("Deleted", date));
                    facts.push(("From", origin.unwrap_or_default().into()));
                } else {
                    facts.push(("Modified", date));
                    if !created.is_empty() {
                        facts.push(("Created", created.into()));
                    }
                    facts.push(("Path", path.into()));
                }
                let el = pane.child(header).child(div().flex_1().min_h_0().overflow_hidden().flex().flex_col().child(body)).child(meta(facts, self.just_copied(), cx));
                el.into_any_element()
            }
            None => {
                let path = p.path.clone();
                let summary = self.folder_summary_for(&path, cx);
                let p = self.pane();
                let files = p.rows.len() - p.folders;
                let bytes: u64 = p.rows.iter().filter(|r| !r.e.is_dir).map(|r| r.e.size).sum();
                let bin = p.is_bin();
                let copied = self.just_copied();
                pane.child(self.header(icon(if bin { "trash-2" } else { "folder-fill" }, 30., t::folder()).into_any_element(), util::folder_name(&path).into(), if bin { "Recycle Bin".into() } else { "Current folder".into() }, None, cx))
                    .child(div().flex_1().min_h_0().overflow_hidden().flex().flex_col().child(summary))
                    .child(meta(
                        vec![
                            ("Contains", format!("{} files, {} folders", kinds::group_digits(files), kinds::group_digits(p.folders)).into()),
                            ("Size of files", kinds::format_size(bytes).into()),
                            ("Hidden", kinds::group_digits(p.hidden).into()),
                            ("Path", path.into()),
                        ],
                        copied,
                        cx,
                    ))
                    .into_any_element()
            }
        }
    }

    fn thumb_or_icon(&mut self, path: &str, kind: Kind, is_dir: bool, modified: Option<i64>, cx: &mut Context<Self>) -> AnyElement {
        if !is_dir && matches!(kind, Kind::Image | Kind::Video | Kind::Exe) && !ferret_core::paths::is_remote(path) {
            if let Some(i) = self.thumb(path, 64, kind == Kind::Exe, modified, cx) {
                return img(i).max_w(px(36.)).max_h(px(36.)).rounded(px(4.)).into_any_element();
            }
        }
        icon(if is_dir { "folder-fill" } else { kinds::icon(kind) }, 30., kinds::color(kind)).into_any_element()
    }

    /// The selected folder's own listing (read in the background, kept until something in it
    /// changes). `None` while it's being read.
    fn peek_folder(&mut self, path: &str, cx: &mut Context<Self>) -> Option<Result<std::rc::Rc<Vec<ferret_core::listing::Entry>>, String>> {
        if let Some((p, r)) = &self.folder_peek {
            if util::same(p, path) {
                return r.clone();
            }
        }
        self.folder_peek = Some((path.to_string(), None));
        let (p2, key) = (path.to_string(), path.to_string());
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::browse::list_dir(p2)).map(|l| l.entries) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| {
                if f.folder_peek.as_ref().is_some_and(|(p, _)| util::same(p, &key)) {
                    f.folder_peek = Some((key.clone(), Some(r.map(std::rc::Rc::new).map_err(|e| e.to_string()))));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        None
    }

    /// A folder's summary: a mosaic of its pictures and what changed lately in it (or was deleted
    /// lately, in the bin). For the folder being shown it comes from the list; for a selected
    /// folder, from its own listing (it showed the folder around it instead: the parent's pictures
    /// and files, for an empty folder too), as the app's FolderView.
    fn folder_summary_for(&mut self, path: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = self.pane();
        let bin = p.is_bin();
        let here = util::same(path, &p.path);
        // (key, path, name, kind, date, row in the list when it's the folder shown)
        type Recent = (usize, String, SharedString, Kind, SharedString, Option<usize>);
        let (pics, recent): (Vec<(String, Option<i64>)>, Vec<Recent>) = if here {
            (
                p.rows.iter().filter(|r| matches!(r.kind, Kind::Image | Kind::Video) && !r.e.in_archive).take(4).map(|r| (r.e.path.clone(), r.e.modified)).collect(),
                p.recent
                    .iter()
                    .filter_map(|&ix| p.rows.get(ix).map(|r| (ix, r.e.path.clone(), r.name.clone(), r.kind, SharedString::from(r.date.get(..10).unwrap_or("").to_string()), Some(ix))))
                    .collect(),
            )
        } else {
            // A server's folder is read only while it's connected, and a WSL distro picked from
            // outside it isn't started by a preview (the app's FolderView rules).
            let offline = ferret_core::paths::is_remote(path) && !ferret_core::paths::is_remote(&p.path);
            let distro = ferret_core::paths::is_wsl(path) && util::parent(path).is_none() && !ferret_core::paths::is_wsl(&p.path);
            if bin || offline || distro {
                let note = if distro { "Open the distro to see what's inside" } else { "" };
                return div().when(!note.is_empty(), |d| d.child(div().px(px(16.)).pt(px(8.)).child(ui::heading(note)))).into_any_element();
            }
            match self.peek_folder(path, cx) {
                None => return div().flex_1().flex().items_center().justify_center().child(ui::spinner(18., t::fg3())).into_any_element(),
                Some(Err(e)) => {
                    return ui::note("peek-error", crate::ops::plain_error(&e)).px(px(16.)).pt(px(8.)).text_size(px(12.)).text_color(t::fg3()).into_any_element();
                }
                Some(Ok(entries)) => {
                    let shown: Vec<&ferret_core::listing::Entry> = entries.iter().filter(|e| crate::pane::show_hidden() || !e.hidden).collect();
                    let pics = shown
                        .iter()
                        .filter(|e| !e.is_dir && !e.in_archive && matches!(kinds::kind_of(false, &e.ext), Kind::Image | Kind::Video))
                        .take(4)
                        .map(|e| (e.path.clone(), e.modified))
                        .collect();
                    let mut by_date = shown.clone();
                    by_date.sort_by_key(|e| std::cmp::Reverse(e.modified.unwrap_or(0)));
                    let recent = by_date
                        .into_iter()
                        .take(8)
                        .enumerate()
                        .map(|(i, e)| {
                            let date = kinds::format_date(e.modified);
                            (1_000_000 + i, e.path.clone(), SharedString::from(e.name.clone()), kinds::kind_of(e.is_dir, &e.ext), SharedString::from(date.get(..10).unwrap_or("").to_string()), None)
                        })
                        .collect();
                    (pics, recent)
                }
            }
        };
        let tiles: Vec<AnyElement> = pics
            .into_iter()
            .map(|(path, m)| match if path.to_lowercase().ends_with(".svg") { None } else { self.thumb(&path, 192, false, m, cx) } {
                Some(i) => div().flex_1().h(px(90.)).rounded(px(8.)).overflow_hidden().bg(t::hover()).child(img(i).size_full().object_fit(ObjectFit::Cover)).into_any_element(),
                // No thumbnails for SVG from Windows: the file itself.
                None if path.to_lowercase().ends_with(".svg") => div()
                    .flex_1()
                    .h(px(90.))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .bg(t::hover())
                    .p(px(8.))
                    .child(img(std::path::PathBuf::from(&path)).size_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                None => div().flex_1().h(px(90.)).rounded(px(8.)).bg(t::hover()).into_any_element(),
            })
            .collect();
        // The pictures and the heading keep their place; the list scrolls in what's left above
        // the details (it ran over them in a short window).
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(!tiles.is_empty(), |d| d.child(div().flex_none().mx(px(16.)).mb(px(10.)).flex().gap(px(6.)).children(tiles)))
            .when(!recent.is_empty(), |d| {
                d.child(div().flex_none().px(px(16.)).pt(px(8.)).pb(px(6.)).child(ui::heading(if bin { "Recently deleted" } else { "Recently changed" }))).child(
                    div().id("recent-list").flex_1().min_h_0().overflow_y_scroll().pb(px(6.)).children(recent.into_iter().map(|(key, item, name, kind, date, row)| {
                    let is_dir = kind == Kind::Folder;
                    div()
                        .id(("recent", key))
                        .h(px(28.))
                        .mx(px(8.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_color(t::fg2())
                        .hover(|s| s.bg(t::hover()).text_color(t::fg()))
                        .tooltip(ui::tip(item.clone()))
                        .on_click(cx.listener(move |f, ev: &gpui::ClickEvent, _, cx| match row {
                            // In the folder shown: a click selects it, a double click opens it.
                            Some(ix) => {
                                if ev.click_count() >= 2 { f.open_row(ix, false, cx) } else { f.select(ix, cx) }
                            }
                            // Inside the selected folder: a double click goes there (into a
                            // folder; to a file, selected in its folder).
                            None if ev.click_count() >= 2 => {
                                if is_dir { f.go(item.clone(), cx) } else { f.reveal(&item, cx) }
                            }
                            None => {}
                        }))
                        .child(icon(kinds::icon(kind), 15., kinds::color(kind)))
                        .child(div().flex_1().min_w_0().truncate().child(name))
                        .child(div().flex_none().text_size(px(12.)).text_color(t::fg3()).child(date))
                })),
                )
            })
            .into_any_element()
    }

    /// The item's content, as its kind is drawn (the pane, or Quick Look when `big`).
    fn content_view(&mut self, path: &str, ext: &str, kind: Kind, width: f32, big: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let frame = |el: AnyElement| div().flex_1().min_h_0().px(px(14.)).pb(px(10.)).flex().flex_col().child(el).into_any_element();
        if kind == Kind::Video {
            if let Some(v) = self.video.as_ref().map(|(v, _)| v.clone()) {
                // Its thumbnail for the moment before the first frame (as the grid shows it).
                if v.read(cx).poster.is_none() {
                    if let Some(t) = self.thumb(path, 512, false, None, cx) {
                        v.update(cx, |v, cx| {
                            v.poster = Some(t);
                            cx.notify();
                        });
                    }
                }
                return div().flex_1().min_h_0().overflow_hidden().child(v).into_any_element();
            }
        }
        if matches!(ext, "docx" | "docm" | "dotx") {
            let word = self.word_selectable(path, width, big, cx);
            return frame(self.with_find_bar(word, big, window, cx));
        }
        // Still loading: a picture shows its thumbnail meanwhile, where and as big as it will be
        // (a spinner on an empty pane flashed on every switch between pictures, 2026-11-11).
        let loading = self.preview_data.as_ref().filter(|l| l.key.starts_with(&format!("{path}|"))).is_none_or(|l| matches!(l.content, Content::Loading));
        if loading {
            if let Some(stand_in) = self.picture_stand_in(path, kind, big, cx) {
                return frame(stand_in);
            }
        }
        let Some(loaded) = self.preview_data.as_ref().filter(|l| l.key.starts_with(&format!("{path}|"))) else {
            return frame(div().flex_1().flex().items_center().justify_center().child(ui::spinner(18., t::fg3())).into_any_element());
        };
        let fresh = loaded.lift && loaded.at.elapsed() < Duration::from_millis(180) && t::animate();
        let wrap = self.preview_view.wrap;
        let source = self.preview_view.source;
        let raw = self.preview_view.raw;
        let hexed = self.preview_view.hex;
        let dir = util::parent(path).unwrap_or_default();
        // PDF pages to draw next (they scrolled near), and how wide; the PDF whose text to read.
        let mut want_pdf: Option<(Vec<u32>, u32)> = None;
        let mut want_text: Option<(String, usize)> = None;
        let me = cx.entity_id();
        let el: AnyElement = match &loaded.content {
            Content::Loading => ui::spinner(18., t::fg3()).into_any_element(),
            Content::Picture { image, w, h } => self.picture_view(image.clone(), (*w, *h), big, cx),
            Content::Video(_) => message("Open it in your player"),
            Content::Audio(_, cover) => self.audio_view(path, cover.clone(), cx),
            Content::Word => self.word_selectable(path, width, big, cx),
            Content::Text { lines, truncated, .. } if !hexed => {
                let lines = lines.clone();
                let truncated = *truncated;
                let code = self.code_selectable("pv-code", path, "text", lines, wrap, big, cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.toggles(&[("Wrap", wrap, 0), ("Show bytes", false, 3)], cx))
                    .child(div().flex_1().min_h_0().rounded(px(8.)).bg(t::hover()).p(px(8.)).child(code))
                    .when(truncated, |d| d.child(div().text_size(px(11.5)).text_color(t::fg3()).child("Showing the first 300 KB")))
                    .into_any_element()
            }
            Content::Markdown { blocks, source: src, .. } => {
                // Wrap: the source's lines, or the code blocks (else each scrolls sideways).
                let has_code = blocks.iter().any(|b| matches!(b, render::Md::Code(_)));
                let view = if source {
                    self.code_selectable("pv-md-src", path, "md-src", src.clone(), wrap, big, cx)
                } else {
                    // Selectable across its blocks.
                    let sel = self.text_sel[big as usize].clone();
                    let mut b = Builder::new(&sel, &format!("{path}#md"));
                    let view = render::markdown_view(blocks, &dir, Some(&mut b), wrap);
                    b.finish(me);
                    self.text_area(big, cx).size_full().child(view).into_any_element()
                };
                let mut switches = vec![(if source { "Rendered" } else { "Source" }, false, 1)];
                if source || has_code {
                    switches.insert(0, ("Wrap", wrap, 0));
                }
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.toggles(&switches, cx))
                    // The view fills what's left under the toggle (`fill`): at the pane's full height
                    // it ran off the bottom by the toggle's height, so its end couldn't be scrolled to.
                    .child(fill(view))
                    .into_any_element()
            }
            Content::Json { value, source: src } => {
                // The tree's text is selectable too, and copies as JSON (render::json_view).
                let view = if raw {
                    self.code_selectable("pv-json-raw", path, "json-raw", src.clone(), wrap, big, cx)
                } else {
                    let sel = self.text_sel[big as usize].clone();
                    let mut b = Builder::new(&sel, &format!("{path}#json"));
                    let tree = render::json_view(value, &self.preview_view.open, wrap, Some(&mut b), big, cx);
                    b.finish(me);
                    self.text_area(big, cx).size_full().child(tree).into_any_element()
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.toggles(&[("Wrap", wrap, 0), (if raw { "Tree" } else { "Raw" }, false, 2)], cx))
                    .child(fill(view))
                    .into_any_element()
            }
            Content::BadJson { error, source: src } => {
                let error = error.clone();
                let view = self.code_selectable("pv-json-bad", path, "json-bad", src.clone(), wrap, big, cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().px(px(10.)).py(px(6.)).rounded(px(7.)).bg(t::rgba(0xf59e0b, 0.12)).text_color(t::warn()).text_size(px(12.)).child(format!("Not valid JSON: {error}")))
                    .child(self.toggles(&[("Wrap", wrap, 0)], cx))
                    .child(fill(view))
                    .into_any_element()
            }
            // Tables: cells selectable (copied as tab-separated rows), sideways scrolling.
            Content::Csv { rows, .. } => {
                let sel = self.text_sel[big as usize].clone();
                let table = render::table_view("pv-csv", rows.clone(), None, self.preview_view.sort, Some((&sel, &format!("{path}#csv"))), cx);
                self.text_area(big, cx).flex_1().min_h_0().size_full().child(table).into_any_element()
            }
            Content::Sheet(s) => {
                let rows = Arc::new(s.rows.clone());
                let names = s.sheets.clone();
                let cur = s.sheet;
                let p = path.to_string();
                let sel = self.text_sel[big as usize].clone();
                let table = render::table_view("pv-sheet", rows, Some((s.first_row, s.first_col)), self.preview_view.sort, Some((&sel, &format!("{path}#sheet{cur}"))), cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(self.text_area(big, cx).flex_1().min_h_0().w_full().child(table))
                    .when(names.len() > 1, |d| {
                        d.child(div().flex().gap(px(2.)).overflow_hidden().children(names.into_iter().enumerate().map(|(i, n)| {
                            let p = p.clone();
                            ui::seg_btn(("sheet", i), None, Some(n.into()), i == cur).on_click(cx.listener(move |f, _, _, cx| f.load_sheet(p.clone(), i, cx)))
                        })))
                    })
                    .into_any_element()
            }
            Content::Deck(deck, bgs, drawn) => {
                let deck = deck.clone();
                let drawn = drawn.clone();
                let w = width - 8.;
                // The slides' text is real text (turned shapes: a layer over their pictures):
                // selectable across slides.
                let sel = self.text_sel[big as usize].clone();
                let mut b = Builder::new(&sel, &format!("{path}#deck"));
                let scroll = b.scroller();
                let slides: Vec<AnyElement> = deck
                    .slides
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let slide = render::slide_view(&deck, s, bgs.get(i).cloned().flatten(), drawn.get(i).map(Vec::as_slice).unwrap_or(&[]), w, Some(&mut b));
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(slide)
                            .child(div().text_size(px(11.)).text_color(t::fg3()).child(if s.hidden { format!("{} · hidden", i + 1) } else { (i + 1).to_string() }))
                            .into_any_element()
                    })
                    .collect();
                b.finish(me);
                let scroller = div().id("pv-deck").track_scroll(&scroll).flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(px(10.)).children(slides);
                self.text_area(big, cx).flex_1().min_h_0().flex().flex_col().child(scroller).into_any_element()
            }
            Content::Archive(a) => {
                let p1 = path.to_string();
                let p2 = path.to_string();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("arc-browse", "Browse", true).on_click(cx.listener(move |f, _, _, cx| f.go(p1.clone(), cx))))
                            .child(ui::button("arc-here", "Extract here", false).on_click(cx.listener(move |f, _, _, cx| f.extract_here(p2.clone(), cx)))),
                    )
                    .child(div().id("arc-list").max_h(px(320.)).overflow_y_scroll().flex().flex_col().children(a.top_level.iter().map(|n| {
                        let dir = n.ends_with('/');
                        let k = kinds::kind_of(dir, &n.rsplit('.').next().unwrap_or("").to_lowercase());
                        div().h(px(24.)).flex().items_center().gap(px(8.)).text_color(t::fg2()).child(icon(kinds::icon(k), 14., kinds::color(k))).child(div().truncate().child(n.trim_end_matches('/').to_string()))
                    })))
                    .children(a.comment.clone().map(|c| div().text_size(px(12.)).text_color(t::fg3()).child(c)))
                    .into_any_element()
            }
            Content::Font { family, .. } => div()
                .id("pv-font")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(10.))
                .font_family(family.clone())
                .children([40., 28., 18., 13.].into_iter().map(|s| div().text_size(px(s)).line_height(relative(1.2)).child("The quick brown fox jumps over the lazy dog 0123456789")))
                .into_any_element(),
            Content::PdfLocked { real, tried } => {
                let (real, tried) = (real.clone(), *tried);
                self.pdf_lock_view(path, real, tried, big, window, cx)
            }
            Content::Pdf { pages, count, real } => {
                // Its text, over each page (pdftext.rs), once read: selectable across pages. Pages
                // with none (scans) are read with text recognition (ocr.rs).
                want_text = Some((real.clone(), pages.len()));
                let text = self.pdf_text.as_ref().filter(|(p, _)| p == real).and_then(|(_, t)| t.clone());
                let sel = self.text_sel[big as usize].clone();
                let firsts: Vec<usize> = match &text {
                    Some(t) => {
                        let lines = selectable::PdfLines::new(t.clone());
                        let firsts = (0..pages.len()).map(|p| lines.first_of(p)).collect();
                        sel.borrow_mut().begin(&format!("{path}#pdf"), Rc::new(lines), me);
                        firsts
                    }
                    None => {
                        sel.borrow_mut().begin(&format!("{path}#pdf"), Rc::new(selectable::Segs::default()), me);
                        Vec::new()
                    }
                };
                let pw = (width - 12.).max(80.);
                let scroll = self.pdf_scroll[big as usize].clone();
                // A drag past the edge and Ctrl+F scroll the pages.
                sel.borrow_mut().scrolls_with(&scroll);
                // Pages within about a screen of the view that aren't drawn yet: drawn next, a few at
                // a time (one request at a time).
                let top = -scroll.offset().y.as_f32();
                let view_h = scroll.bounds().size.height.as_f32().max(400.);
                let mut y = 0.;
                let mut want = Vec::new();
                // The page in view: scanned pages are read nearest it first.
                if let Some(o) = self.ocr.as_ref().filter(|o| o.path == *real) {
                    let mut yy = 0.;
                    let shown_page = pages.iter().position(|(_, aspect)| {
                        yy += pw * aspect + 27.;
                        yy > top + view_h * 0.3
                    });
                    o.run.near.store(shown_page.unwrap_or(0) as u32, std::sync::atomic::Ordering::Relaxed);
                }
                for (i, (page, aspect)) in pages.iter().enumerate() {
                    let h = pw * aspect + 27.;
                    if page.is_none() && y < top + view_h * 2. && y + h > top - view_h {
                        want.push(i as u32);
                        if want.len() == 4 {
                            break;
                        }
                    }
                    y += h;
                }
                if !want.is_empty() {
                    want_pdf = Some((want, (pw * self.scale).round() as u32));
                }
                let shown = pages.len();
                let scroller = div()
                    .id(("pv-pdf", big as usize))
                    .track_scroll(&scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .children(pages.iter().enumerate().map(|(i, (page, aspect))| {
                        // Sized from the page's own shape (a picture with no height draws nothing).
                        let page_el = match page {
                            Some(p) => img(p.clone()).w(px(pw)).h(px(pw * aspect)).object_fit(ObjectFit::Contain).rounded(px(3.)).into_any_element(),
                            None => div()
                                .w(px(pw))
                                .h(px(pw * aspect))
                                .rounded(px(3.))
                                .bg(t::rgba(0xffffff, 0.92))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(ui::spinner(16., t::rgba(0x9a9a9a, 1.)))
                                .into_any_element(),
                        };
                        // The page's text lies over its picture (invisible but for the selection).
                        let layer = text.clone().zip(firsts.get(i).copied()).map(|(t, first)| selectable::pdf_layer(&sel, t, i, first, false));
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(div().relative().w(px(pw)).h(px(pw * aspect)).child(page_el).children(layer))
                            .child(div().text_size(px(11.)).text_color(t::fg3()).child(format!("{} / {count}", i + 1)))
                    }))
                    .when((*count as usize) > shown, |d| {
                        d.child(div().pb(px(6.)).text_size(px(11.5)).text_color(t::fg3()).child(format!("The preview shows the first {shown} pages; open it to read on")))
                    });
                let note = self.ocr_note(real, big, cx);
                self.text_area(big, cx).flex_1().min_h_0().flex().flex_col().children(note).child(scroller).into_any_element()
            }
            Content::Program(..) if hexed => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(self.toggles(&[("Hide bytes", true, 3)], cx))
                .child(div().flex_1().min_h_0().child(self.hex_for(path, width, cx)))
                .into_any_element(),
            // As the app's NoPreview: a big icon, what it is, and Open with… / Show bytes (the
            // version details are in the list under it).
            Content::Program(_, icon_img) => {
                let p = path.to_string();
                let size_text = self.pane().cursor_row().filter(|r| r.e.size > 0).map(|r| format!("{} · ", r.size)).unwrap_or_default();
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.))
                    .child(match icon_img {
                        Some(i) => img(i.clone()).size(px(88.)).object_fit(ObjectFit::Contain).into_any_element(),
                        None => icon("app-window", 72., t::fg3()).into_any_element(),
                    })
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(crate::kinds::type_label(false, ext)))
                    .child(div().text_size(px(12.)).text_color(t::fg3()).child(format!("{size_text}No preview for this kind of file")))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("prog-openwith", "Open with…", false).on_click(cx.listener(move |f, _, window, cx| f.open_with_dialog(p.clone(), window, cx))))
                            .child(ui::button("prog-bytes", "Show bytes", false).on_click(cx.listener(|f, _, _, cx| {
                                f.preview_view.hex = true;
                                cx.notify();
                            }))),
                    )
                    .into_any_element()
            }
            Content::Shell(i) => img(i.clone()).w_full().flex_1().object_fit(ObjectFit::Contain).into_any_element(),
            Content::Hex(b) => render::hex_view(b, width),
            // A document read as text: a white page of paragraphs, headings larger, selectable
            // across paragraphs.
            Content::DocText(paras) => {
                let sel = self.text_sel[big as usize].clone();
                let mut b = Builder::new(&sel, &format!("{path}#doc"));
                let scroll = b.scroller();
                let children: Vec<AnyElement> = paras
                    .iter()
                    .take(1500)
                    .map(|(heading, text)| {
                        let plain = SharedString::from(text.clone());
                        div()
                            .text_size(px(if *heading { 17. } else { 13. }))
                            .line_height(relative(1.35))
                            .when(*heading, |d| d.mt(px(6.)).font_weight(FontWeight::SEMIBOLD))
                            .child(b.text(StyledText::new(plain.clone()), plain, "\n"))
                            .into_any_element()
                    })
                    .collect();
                b.finish(me);
                self.text_area(big, cx)
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("pv-doctext")
                            .track_scroll(&scroll)
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .rounded(px(6.))
                            .bg(t::rgba(0xffffff, 1.))
                            .px(px(18.))
                            .py(px(20.))
                            .font_family("Calibri")
                            .text_color(t::rgba(0x1f1f1f, 1.))
                            .flex()
                            .flex_col()
                            .gap(px(7.))
                            .children(children),
                    )
                    .into_any_element()
            }
            // A text file shown as bytes keeps its switch back to the text.
            Content::Text { .. } => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(self.toggles(&[("Show text", true, 3)], cx))
                .child(div().flex_1().min_h_0().child(self.hex_for(path, width, cx)))
                .into_any_element(),
            Content::Empty => message("Empty file"),
            Content::Nothing(_) if hexed => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(self.toggles(&[("Hide bytes", true, 3)], cx))
                .child(div().flex_1().min_h_0().child(self.hex_for(path, width, cx)))
                .into_any_element(),
            Content::Nothing(why) => {
                let p = path.to_string();
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(icon(kinds::icon(kind), 64., kinds::color(kind)))
                    .child(div().text_color(t::fg3()).text_center().child(why.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("nothing-openwith", "Open with…", false).on_click(cx.listener(move |f, _, window, cx| f.open_with_dialog(p.clone(), window, cx))))
                            .child(ui::button("nothing-bytes", "Show bytes", false).on_click(cx.listener(|f, _, _, cx| {
                                f.preview_view.hex = true;
                                cx.notify();
                            }))),
                    )
                    .into_any_element()
            }
        };
        let _ = window;
        if let Some((which, page_px)) = want_pdf {
            self.more_pdf_pages(path.to_string(), which, page_px, cx);
        }
        if let Some((real, shown)) = want_text {
            self.sync_pdf_text(real, shown, cx);
        } else if !big {
            // Away from the PDF: its scanned pages stop being read (what's read is kept).
            if let Some(o) = self.ocr.as_ref() {
                o.run.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let el = self.with_find_bar(el, big, window, cx);
        let el = frame(el);
        if fresh {
            // The preview lifts in (5px) as it appears.
            div().flex_1().min_h_0().flex().flex_col().child(el).with_animation(("pv-in", path.len()), gpui::Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k).mt(px((1. - k) * 5.))).into_any_element()
        } else {
            el
        }
    }

    /// Draws more of a PDF's pages (as they scroll near) into the preview shown for it.
    fn more_pdf_pages(&mut self, path: String, which: Vec<u32>, page_px: u32, cx: &mut Context<Self>) {
        if self.pdf_pending {
            return;
        }
        let Some(key) = self.preview_data.as_ref().map(|l| l.key.clone()) else { return };
        self.pdf_pending = true;
        let task = cx.background_executor().spawn(async move {
            // Archive entries and server files: their local copy (made when it was first shown).
            let real = if ferret_core::paths::is_remote(&path) {
                ferret_core::rt::block_on(ferret_core::remote::remote_materialize(path.clone())).unwrap_or(path)
            } else if ferret_core::archive::split_virtual(&path).is_some() && !std::path::Path::new(&path).exists() {
                ferret_core::rt::block_on(ferret_core::archive::archive_materialize(path.clone(), None)).unwrap_or(path)
            } else {
                path
            };
            render::pdf_more(&real, which, page_px)
        });
        cx.spawn(async move |this, cx| {
            let drawn = task.await;
            this.update(cx, |f, cx| {
                f.pdf_pending = false;
                // Put into the preview it was for, wherever that is now (shown or cached).
                let target = f.preview_data.iter_mut().chain(f.preview_cache.iter_mut()).find(|l| l.key == key);
                if let Some(Loaded { content: Content::Pdf { pages, .. }, .. }) = target {
                    for (i, image) in drawn {
                        if let Some(slot) = pages.get_mut(i as usize) {
                            slot.0 = Some(image);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn hex_for(&mut self, path: &str, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let bytes = std::fs::read(path).map(|mut b| {
            b.truncate(512);
            b
        });
        let _ = cx;
        match bytes {
            Ok(b) => render::hex_view(&b, width),
            Err(e) => message(&e.to_string()),
        }
    }

    /// Small switches above some previews (Wrap, Source, Raw, Show bytes).
    fn toggles(&self, list: &[(&'static str, bool, u8)], cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .gap(px(4.))
            .children(list.iter().map(|(label, on, which)| {
                let which = *which;
                let on = *on;
                // A pill, as the app's `.pv-chip` (as flat text they read as labels, not buttons);
                // a switch that's on (Wrap) takes the accent.
                ui::status_btn(("pv-toggle", which as usize), None, Some((*label).into()), on)
                    .when(which == 0, |d| d.aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False }))
                    .h(px(24.))
                    .px(px(10.))
                    .rounded(px(12.))
                    .text_size(px(11.5))
                    .bg(t::raised())
                    .border_1()
                    .border_color(if on { t::accent() } else { t::line2() })
                    .hover(move |s| s.border_color(t::accent()).text_color(if on { t::accent_text() } else { t::fg() }))
                    .on_click(cx.listener(move |f, _, _, cx| {
                    let v = &mut f.preview_view;
                    match which {
                        0 => v.wrap = !v.wrap,
                        1 => v.source = !v.source,
                        2 => v.raw = !v.raw,
                        _ => v.hex = !v.hex,
                    }
                    cx.notify();
                }))
            }))
            .into_any_element()
    }

    fn load_sheet(&mut self, path: String, sheet: usize, cx: &mut Context<Self>) {
        let p2 = path.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::office::sheet_preview(p2, Some(sheet))) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| {
                if let (Ok(s), Some(l)) = (r, f.preview_data.as_mut()) {
                    if l.key.starts_with(&format!("{path}|")) {
                        l.content = Content::Sheet(s);
                        f.preview_view.sort = None;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// A picture: click zooms in where you clicked (and it follows the mouse); the wheel zooms
    /// up to 16× around the pointer.
    fn picture_view(&mut self, image: Arc<gpui::Image>, size: (u32, u32), big: bool, cx: &mut Context<Self>) -> AnyElement {
        let zoom = self.preview_view.zoom.max(1.);
        let (zx, zy) = self.preview_view.zoom_at;
        let bounds = self.picture_bounds[big as usize].clone();
        let b2 = bounds.clone();
        // The whole picture fits the area measured last frame (never cropped; small pictures
        // aren't blown up), centred; zoomed, it grows around the point zoomed at.
        let area = bounds.get().size;
        let (aw, ah) = (area.width.as_f32(), area.height.as_f32());
        let (pw, ph) = (size.0 as f32, size.1 as f32);
        let fit = if aw > 0. && ah > 0. && pw > 0. && ph > 0. { (aw / pw).min(ah / ph).min(if big { 1. } else { f32::MAX }) } else { 0. };
        let picture: AnyElement = if fit > 0. {
            let (w, h) = (pw * fit * zoom, ph * fit * zoom);
            div()
                .absolute()
                .left(px((aw - w) / 2. - (w - pw * fit) * (zx - 0.5)))
                .top(px((ah - h) / 2. - (h - ph * fit) * (zy - 0.5)))
                .w(px(w))
                .h(px(h))
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        } else {
            div()
                .absolute()
                .left(relative(-(zoom - 1.) * zx))
                .top(relative(-(zoom - 1.) * zy))
                .w(relative(zoom))
                .h(relative(zoom))
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element()
        };
        div()
            .id(if big { "ql-pic" } else { "pv-pic" })
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .overflow_hidden()
            .cursor(if zoom > 1. { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::PointingHand })
            // Measured for the next frame; a new size draws again at once (the picture fits it).
            .child(
                gpui::canvas(
                    move |b, window, _| {
                        if bounds.get().size != b.size {
                            window.refresh();
                        }
                        bounds.set(b)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(picture)
            .when(zoom > 1., |d| {
                d.child(div().absolute().top(px(8.)).right(px(8.)).px(px(7.)).py(px(2.)).rounded(px(6.)).bg(t::rgba(0, 0.55)).text_color(t::rgba(0xffffff, 1.)).text_size(px(11.)).child(format!("{zoom:.1}×")))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &gpui::MouseDownEvent, _, cx| {
                let b = b2.get();
                let v = &mut f.preview_view;
                if v.zoom > 1. {
                    v.zoom = 1.;
                } else {
                    v.zoom = 3.;
                    v.zoom_at = (((ev.position.x - b.origin.x) / b.size.width).clamp(0., 1.), ((ev.position.y - b.origin.y) / b.size.height).clamp(0., 1.));
                }
                cx.notify();
            }))
            .on_mouse_move(cx.listener({
                let b3 = self.picture_bounds[big as usize].clone();
                move |f, ev: &gpui::MouseMoveEvent, _, cx| {
                    if f.preview_view.zoom > 1. {
                        let b = b3.get();
                        f.preview_view.zoom_at = (((ev.position.x - b.origin.x) / b.size.width).clamp(0., 1.), ((ev.position.y - b.origin.y) / b.size.height).clamp(0., 1.));
                        cx.notify();
                    }
                }
            }))
            .on_scroll(cx.listener({
                let b4 = self.picture_bounds[big as usize].clone();
                move |f, ev: &gpui::ScrollEvent, _, cx| {
                    let dy = ev.delta.pixel_delta(px(20.)).y.as_f32();
                    let b = b4.get();
                    let v = &mut f.preview_view;
                    v.zoom = (v.zoom.max(1.) * if dy > 0. { 1.2 } else { 1. / 1.2 }).clamp(1., 16.);
                    v.zoom_at = (((ev.position.x - b.origin.x) / b.size.width).clamp(0., 1.), ((ev.position.y - b.origin.y) / b.size.height).clamp(0., 1.));
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn audio_view(&mut self, path: &str, cover: Option<Arc<gpui::Image>>, cx: &mut Context<Self>) -> AnyElement {
        // A track that played to its end shows ▶ again.
        let playing = self.audio.as_ref().is_some_and(|(p, pl)| p == path && pl.playing() && !audio_ended(pl));
        let p = path.to_string();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .child(match cover {
                Some(c) => img(c).size(px(180.)).rounded(px(10.)).object_fit(ObjectFit::Cover).into_any_element(),
                None => div().size(px(180.)).rounded(px(10.)).bg(t::hover()).flex().items_center().justify_center().child(icon("music", 64., t::fg3())).into_any_element(),
            })
            .child(
                div()
                    .id("audio-play")
                    .size(px(44.))
                    .rounded(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t::accent())
                    .hover(|s| s.opacity(0.9))
                    .on_click(cx.listener(move |f, _, _, cx| f.toggle_audio(p.clone(), cx)))
                    .child(icon(if playing { "pause" } else { "play" }, 20., t::on_accent())),
            )
            .child(self.audio_bar(path, cx))
            .into_any_element()
    }

    /// Where the audio is, and where it can go: click or drag along the bar to seek.
    fn audio_bar(&self, path: &str, cx: &mut Context<Self>) -> AnyElement {
        let (pos, dur) = match &self.audio {
            Some((p, pl)) if p == path => (pl.position(), pl.duration()),
            _ => (0., 0.),
        };
        let frac = if dur.is_finite() && dur > 0. { (pos / dur).clamp(0., 1.) as f32 } else { 0. };
        let bounds = self.audio_bounds.clone();
        let b1 = bounds.clone();
        let seek = move |f: &mut Ferret, x: gpui::Pixels, cx: &mut Context<Ferret>| {
            let b = b1.get();
            if b.size.width.as_f32() <= 0. {
                return;
            }
            let k = ((x - b.origin.x) / b.size.width).clamp(0., 1.) as f64;
            if let Some((_, pl)) = f.audio.as_mut() {
                let d = pl.duration();
                if d.is_finite() && d > 0. {
                    pl.seek(d * k);
                }
            }
            cx.notify();
        };
        let seek2 = seek.clone();
        div()
            .w(px(300.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(11.5))
            .text_color(t::fg3())
            .child(crate::video::clock(pos))
            .child(
                div()
                    .id("audio-seek")
                    .flex_1()
                    .h(px(14.))
                    .relative()
                    .flex()
                    .items_center()
                    .child(gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
                    .child(div().w_full().h(px(4.)).rounded(px(2.)).bg(t::line2()).child(div().h_full().w(relative(frac)).rounded(px(2.)).bg(t::accent())))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &gpui::MouseDownEvent, _, cx| seek(f, ev.position.x, cx)))
                    .on_mouse_move(cx.listener(move |f, ev: &gpui::MouseMoveEvent, _, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            seek2(f, ev.position.x, cx);
                        }
                    })),
            )
            .child(if dur.is_finite() && dur > 0. { crate::video::clock(dur) } else { String::new() })
            .child(self.volume_control(cx))
            .into_any_element()
    }

    /// Sound on or off, and how loud (the audio element's own controls in the app).
    fn volume_control(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted = self.audio_muted;
        let v = if muted { 0. } else { self.audio_volume };
        let bounds = self.volume_bounds.clone();
        let b1 = bounds.clone();
        let set = move |f: &mut Ferret, x: gpui::Pixels, cx: &mut Context<Ferret>| {
            let b = b1.get();
            if b.size.width.as_f32() <= 0. {
                return;
            }
            f.audio_volume = ((x - b.origin.x) / b.size.width).clamp(0., 1.);
            f.audio_muted = f.audio_volume == 0.;
            let (vol, mute) = (f.audio_volume, f.audio_muted);
            if let Some((_, pl)) = f.audio.as_mut() {
                pl.set_volume(vol as f64);
                pl.set_muted(mute);
            }
            cx.notify();
        };
        let set2 = set.clone();
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .id("audio-mute")
                    .role(gpui::Role::Button)
                    .aria_label(if muted { "Sound on" } else { "Mute" })
                    .aria_toggled(if muted { gpui::Toggled::True } else { gpui::Toggled::False })
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .hover(|s| s.bg(t::hover()))
                    .tooltip(ui::tip(if muted { "Sound on" } else { "Mute" }))
                    .on_click(cx.listener(|f, _, _, cx| {
                        f.audio_muted = !f.audio_muted;
                        if !f.audio_muted && f.audio_volume == 0. {
                            f.audio_volume = 0.5;
                        }
                        let (vol, mute) = (f.audio_volume, f.audio_muted);
                        if let Some((_, pl)) = f.audio.as_mut() {
                            pl.set_volume(vol as f64);
                            pl.set_muted(mute);
                        }
                        cx.notify();
                    }))
                    .child(icon(if muted || v == 0. { "volume-x" } else { "volume-2" }, 14., t::fg2())),
            )
            .child(
                div()
                    .id("audio-volume")
                    .role(gpui::Role::Slider)
                    .aria_label("Volume")
                    .aria_numeric_value((v * 100.).round() as f64)
                    .w(px(52.))
                    .h(px(14.))
                    .relative()
                    .flex()
                    .items_center()
                    .child(gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
                    .child(div().w_full().h(px(4.)).rounded(px(2.)).bg(t::line2()).child(div().h_full().w(relative(v)).rounded(px(2.)).bg(t::fg2())))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &gpui::MouseDownEvent, _, cx| set(f, ev.position.x, cx)))
                    .on_mouse_move(cx.listener(move |f, ev: &gpui::MouseMoveEvent, _, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            set2(f, ev.position.x, cx);
                        }
                    })),
            )
    }

    /// Plays or pauses an audio file (Media Foundation, as the video player).
    pub fn toggle_audio(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some((p, player)) = &self.audio {
            if *p == path {
                let ended = audio_ended(player);
                let playing = player.playing() && !ended;
                if let Some((_, player)) = self.audio.as_mut() {
                    if playing {
                        player.pause()
                    } else {
                        // Played to the end: from the start again.
                        if ended {
                            player.seek(0.);
                        }
                        player.play()
                    }
                }
                cx.notify();
                return;
            }
        }
        match crate::video::Player::open(&path, self.audio_muted) {
            Ok(mut player) => {
                player.set_volume(self.audio_volume as f64);
                player.play();
                self.audio = Some((path, player));
                // Redraw while it plays (the play/pause button follows it).
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(500)).await;
                        if this.update(cx, |f, cx| { cx.notify(); f.audio.is_some() }).unwrap_or(false) == false {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(e) => self.toast_error(format!("Couldn't play it: {e}"), cx),
        }
        cx.notify();
    }

    /// Ctrl+wheel over a Word page zooms it 50–400 %, caught before the page scrolls.
    fn word_zoomable(&self, page: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let zoom = if self.preview_view.word_zoom > 0. { self.preview_view.word_zoom } else { 1. };
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(page)
            .child(
                gpui::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let me = me.clone();
                        window.on_mouse_event(move |ev: &gpui::ScrollEvent, phase, _, cx| {
                            if phase != gpui::DispatchPhase::Capture || !ev.modifiers.control || !bounds.contains(&ev.position) {
                                return;
                            }
                            let dy = ev.delta.pixel_delta(px(20.)).y.as_f32();
                            if dy == 0. {
                                return;
                            }
                            cx.stop_propagation();
                            let _ = me.update(cx, |f, cx| {
                                let z = if f.preview_view.word_zoom > 0. { f.preview_view.word_zoom } else { 1. };
                                f.preview_view.word_zoom = (z * if dy > 0. { 1.1 } else { 1. / 1.1 }).clamp(0.5, 4.);
                                cx.notify();
                            });
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .when((zoom - 1.).abs() > 0.01, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(8.))
                        .right(px(10.))
                        .px(px(7.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .bg(t::raised())
                        .border_1()
                        .border_color(t::line())
                        .text_size(px(11.))
                        .text_color(t::fg2())
                        .child(format!("{:.0} %", zoom * 100.)),
                )
            })
            .into_any_element()
    }

    /// A Word document, its text selectable across paragraphs (Ctrl+wheel zooms it).
    fn word_selectable(&mut self, path: &str, width: f32, big: bool, cx: &mut Context<Self>) -> AnyElement {
        let sel = self.text_sel[big as usize].clone();
        let mut b = Builder::new(&sel, &format!("{path}#word"));
        let page = self.word_view(width, &mut b);
        b.finish(cx.entity_id());
        let area = self.text_area(big, cx).flex_1().min_h_0().flex().flex_col().child(page).into_any_element();
        self.word_zoomable(area, cx)
    }

    /// Code (a text file, Markdown's or JSON's source), its lines selectable.
    fn code_selectable(&self, id: &'static str, path: &str, mode: &str, lines: Arc<Vec<render::Line>>, wrap: bool, big: bool, cx: &mut Context<Self>) -> AnyElement {
        let sel = self.text_sel[big as usize].clone();
        let key = format!("{path}#{mode}");
        let scroll = sel.borrow_mut().list_scroller(&key);
        if wrap {
            // Wrapped, it's a scrolled div (every line drawn): Ctrl+F finds its lines where drawn.
            sel.borrow_mut().scroller(&key);
        }
        sel.borrow_mut().begin(&key, Rc::new(Lines(lines.clone())), cx.entity_id());
        let view = render::code_view(id, lines, wrap, Some(&sel), Some(scroll), cx);
        self.text_area(big, cx).size_full().child(view).into_any_element()
    }

    pub fn word_view(&self, width: f32, b: &mut Builder) -> AnyElement {
        let Some((_, state)) = &self.word else { return div().into_any_element() };
        let doc = match state {
            None => return message("Reading the document…"),
            Some(Err(e)) => return message(&format!("Can't show this document: {e}")),
            Some(Ok(doc)) => doc,
        };
        // Points to pixels: a little larger than print, so text reads at the pane's width; Ctrl+wheel
        // zooms 50–400 %.
        let k = 1.15 * if self.preview_view.word_zoom > 0. { self.preview_view.word_zoom } else { 1. };
        let width = width - 36.;
        let ink = t::rgba(0x1f1f1f, 1.);
        let mut page = div().id("word-page").track_scroll(&b.scroller()).flex_1().min_h_0().overflow_y_scroll().rounded(px(6.)).bg(t::rgba(0xffffff, 1.)).px(px(18.)).py(px(20.)).font_family("Calibri").text_color(ink).flex().flex_col();
        let rule = t::rgba(0xd4d4d4, 1.);
        // The header, smaller and lighter, over a thin line.
        if !doc.header.is_empty() {
            let header: Vec<AnyElement> = doc.header.iter().map(|x| word_block(x, k * 0.85, width, b)).collect();
            page = page.child(div().mb(px(12.)).pb(px(6.)).border_b_1().border_color(rule).opacity(0.65).children(header));
        }
        for block in &doc.blocks {
            page = page.child(word_block(block, k, width, b));
        }
        if doc.truncated {
            page = page.child(div().pt(px(8.)).text_size(px(11.)).text_color(t::rgba(0x777777, 1.)).child("The preview stops here: the document goes on."));
        }
        // Footnotes under a short line, numbered as in the text.
        if !doc.notes.is_empty() {
            page = page.child(div().mt(px(16.)).mb(px(6.)).w(px(140.)).h(px(1.)).bg(t::rgba(0x9a9a9a, 1.)));
            for (n, blocks) in &doc.notes {
                let note: Vec<AnyElement> = blocks.iter().map(|x| word_block(x, k * 0.85, width - 24., b)).collect();
                page = page.child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(div().flex_none().w(px(18.)).text_size(px(9. * k)).text_color(t::rgba(0x555555, 1.)).child(format!("{n}.")))
                        .child(div().flex_1().min_w_0().children(note)),
                );
            }
        }
        // The footer, as the header, at the end.
        if !doc.footer.is_empty() {
            let footer: Vec<AnyElement> = doc.footer.iter().map(|x| word_block(x, k * 0.85, width, b)).collect();
            page = page.child(div().mt(px(14.)).pt(px(6.)).border_t_1().border_color(rule).opacity(0.65).children(footer));
        }
        page.into_any_element()
    }

    // ───────────────────────────── Quick Look ─────────────────────────────

    pub fn quicklook_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.quicklook {
            return None;
        }
        self.sync_preview(cx);
        let p = self.pane();
        let order: Vec<usize> = p.visible_rows().collect();
        let Some(r) = p.cursor_row() else {
            self.quicklook = false;
            return None;
        };
        let pos = order.iter().position(|&i| Some(i) == p.selected).unwrap_or(0);
        let (path, ext, kind, name, ty, size) = (r.e.path.clone(), r.e.ext.clone(), r.kind, r.name.clone(), r.ty.clone(), r.size.clone());
        let vw = window.viewport_size();
        let w = (vw.width.as_f32() - 120.).max(300.);
        // The width inside the card: its padding (10 a side), border, and the content frame's 14 a side.
        let content = if r.e.is_dir { self.folder_summary_for(&path, cx) } else { self.content_view(&path, &ext, kind, w - 50., true, window, cx) };
        let dims = match self.preview_data.as_ref().filter(|l| l.key.starts_with(&format!("{path}|"))).map(|l| &l.content) {
            Some(Content::Picture { w, h, .. }) if *w > 0 => Some((*w, *h)),
            _ => None,
        };
        let p2 = path.clone();
        let el = div()
            .id("ql")
            .role(gpui::Role::Dialog)
            .aria_label("Quick Look")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(t::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|f, _, _, cx| {
                f.quicklook = false;
                cx.notify();
            }))
            .child(
                ui::modal_card(w)
                    .h(relative(0.88))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px(px(16.))
                            .py(px(10.))
                            .border_b_1()
                            .border_color(t::line())
                            .child(icon(kinds::icon(kind), 18., kinds::color(kind)))
                            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
                            .child(div().text_color(t::fg3()).text_size(px(12.)).child(match dims {
                                Some((w, h)) => format!("{ty} · {size} · {w} × {h}"),
                                None => format!("{ty} · {size}"),
                            }))
                            .child(div().text_color(t::fg3()).text_size(px(12.)).child(format!("{} / {}", pos + 1, order.len())))
                            // ‹ › step through the folder (as ← →).
                            .child(ui::icon_btn("ql-prev", "chevron-left", true).tooltip(ui::tip("Previous (←)")).on_click(cx.listener(|f, _, window, cx| f.quicklook_key("left", window, cx))))
                            .child(ui::icon_btn("ql-next", "chevron-right", true).tooltip(ui::tip("Next (→)")).on_click(cx.listener(|f, _, window, cx| f.quicklook_key("right", window, cx))))
                            .child(ui::button("ql-open", "Open", true).h(px(28.)).on_click(cx.listener(move |f, _, _, cx| {
                                f.quicklook = false;
                                f.open_file(p2.clone(), false, cx);
                            })))
                            .child(ui::icon_btn("ql-close", "x", true).on_click(cx.listener(|f, _, _, cx| {
                                f.quicklook = false;
                                cx.notify();
                            }))),
                    )
                    .child(div().flex_1().min_h_0().flex().flex_col().p(px(10.)).child(content)),
            );
        Some(if t::animate() {
            el.with_animation("ql-in", gpui::Animation::new(Duration::from_millis(160)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k)).into_any_element()
        } else {
            el.into_any_element()
        })
    }

    pub fn quicklook_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        // A PDF asking for its password: F6 or Tab put the keyboard in its field.
        if matches!(key, "f6" | "tab") && self.focus_pdf_password(window, cx) {
            return;
        }
        let p = self.pane();
        let order: Vec<usize> = p.visible_rows().collect();
        let n = order.len();
        let pos = order.iter().position(|&i| Some(i) == p.selected).unwrap_or(0);
        match key {
            "escape" | "space" => self.quicklook = false,
            "left" | "up" if n > 0 => self.select(order[(pos + n - 1) % n], cx),
            "right" | "down" if n > 0 => self.select(order[(pos + 1) % n], cx),
            "enter" => {
                self.quicklook = false;
                if let Some(r) = self.pane().cursor_row() {
                    let (path, inside) = (r.e.path.clone(), r.e.in_archive);
                    self.open_file(path, inside, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }
}

/// The details list: label and value; the path copies when clicked. At most 40% of the pane's
/// height, scrolling past that, as the app's `.pv-meta` (a video or picture keeps the rest: with
/// eleven details a video was drawn a third of the pane's width).
pub fn meta(facts: Vec<(&'static str, SharedString)>, copied: bool, cx: &mut Context<Ferret>) -> Div {
    div().flex_none().max_h(relative(0.4)).flex().flex_col().border_t_1().border_color(t::line()).child(
        div()
            .id("meta-scroll")
            .min_h_0()
            .flex_shrink(1.)
            .overflow_y_scroll()
            .px(px(14.))
            .pt(px(10.))
            .pb(px(12.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .text_size(px(12.))
            .children(facts.into_iter().map(|(label, value)| {
                let path = label == "Path" || label == "In";
                let v2 = value.clone();
                // Just copied: said right there, in green, instead of a toast at the bottom.
                let done = path && copied;
                div()
                    .id(SharedString::from(format!("meta-{label}")))
                    .flex_none()
                    .flex()
                    .gap(px(10.))
                    .when(path, |d| {
                        d.cursor(gpui::CursorStyle::PointingHand)
                            .mx(px(-5.))
                            .px(px(5.))
                            .my(px(-2.))
                            .py(px(2.))
                            .rounded(px(5.))
                            .hover(|s| s.bg(t::hover()))
                            .when(!done, |d| d.tooltip(ui::tip("Click to copy")))
                            .on_click(cx.listener(move |f, _, _, cx| {
                                // Tests leave the user's clipboard alone (FERRET_GPUI_NO_CLIPBOARD).
                                if std::env::var_os("FERRET_GPUI_NO_CLIPBOARD").is_none() {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(v2.to_string()));
                                }
                                f.note_copied(cx);
                            }))
                    })
                    .child(div().flex_none().w(px(82.)).text_color(t::fg3()).child(label))
                    .child({
                        // Paths may wrap after any backslash.
                        let text = if path { value.replace('\\', "\\\u{200B}") } else { value.to_string() };
                        let cell = div().flex_1().min_w_0().text_color(t::fg()).when(path, |d| d.font_family(t::MONO));
                        if done {
                            // "✓ Copied" right after the path, in the text's own flow (it wraps
                            // with it), in green.
                            let start = text.len() + 2;
                            let shown = format!("{text}  ✓\u{a0}Copied");
                            let end = shown.len();
                            let green = gpui::HighlightStyle { color: Some(t::ok()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
                            cell.child(gpui::StyledText::new(shown).with_highlights(vec![(start..end, green)]))
                        } else {
                            cell.child(text)
                        }
                    })
            })),
    )
}

/// One Word paragraph: its runs' styles as highlights over one text, list marker, pictures.
/// The track played to its end (Media Foundation keeps it "playing" there).
fn audio_ended(pl: &crate::video::Player) -> bool {
    let d = pl.duration();
    d.is_finite() && d > 0. && pl.position() >= d - 0.05
}

/// A block of a Word document: a paragraph, a table, or a page break (`k`: pixels per point).
fn word_block(block: &docx::Block, k: f32, width: f32, b: &mut Builder) -> AnyElement {
    match block {
        docx::Block::Para(p) => word_para(p, k, width, b).into_any_element(),
        docx::Block::PageBreak => div().my(px(10.)).border_t_1().border_dashed().border_color(t::rgba(0x9a9a9a, 1.)).into_any_element(),
        docx::Block::Table(rows) => {
            let cols = rows.iter().map(|r| r.iter().map(|c| c.span.max(1)).sum::<usize>()).max().unwrap_or(1).max(1);
            let line = t::rgba(0xbfbfbf, 1.);
            let mut out: Vec<AnyElement> = Vec::new();
            for row in rows {
                let mut cells: Vec<AnyElement> = Vec::new();
                for (j, cell) in row.iter().enumerate() {
                    let before = b.next();
                    let paras: Vec<Div> = cell.paras.iter().map(|p| word_para(p, k * 0.9, width / cols as f32, b)).collect();
                    // Cells copy as a tab-separated row.
                    if b.next() > before {
                        b.segs.end_with(if j + 1 == row.len() { "\n" } else { "\t" });
                    }
                    cells.push(
                        div()
                            .w(relative(cell.span.max(1) as f32 / cols as f32))
                            .p(px(4.))
                            .border_r_1()
                            .border_color(line)
                            .when_some(cell.shade, |d, c| d.bg(t::rgba(c, 1.)))
                            .children(paras)
                            .into_any_element(),
                    );
                }
                out.push(div().flex().border_b_1().border_color(line).children(cells).into_any_element());
            }
            div().mb(px(8.)).flex().flex_col().border_1().border_color(line).children(out).into_any_element()
        }
    }
}

fn word_para(p: &docx::Para, k: f32, width: f32, b: &mut Builder) -> Div {
    use gpui::{FontStyle, HighlightStyle, StrikethroughStyle, UnderlineStyle};
    let size = (p.size * k).max(7.);
    let mut ranges = Vec::new();
    let mut at = 0;
    for r in &p.runs {
        let s = &r.style;
        let mut h = HighlightStyle::default();
        if s.bold {
            h.font_weight = Some(FontWeight::BOLD);
        }
        if s.italic {
            h.font_style = Some(FontStyle::Italic);
        }
        if s.link {
            h.color = Some(t::rgba(0x0563c1, 1.));
            h.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
        } else if let Some(c) = s.color {
            h.color = Some(t::rgba(c, 1.));
        }
        if s.underline {
            h.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
        }
        if s.strike {
            h.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
        }
        if let Some(b) = s.highlight {
            h.background_color = Some(t::rgba(b, 1.));
        }
        ranges.push((at..at + r.text.len(), h));
        at += r.text.len();
    }
    let plain = SharedString::from(p.text());
    let text = b.text(StyledText::new(plain.clone()).with_highlights(ranges), plain, "\n");
    let line = div().flex_1().min_w_0().when(p.align == docx::Align::Center, |d| d.text_center()).when(p.align == docx::Align::Right, |d| d.text_right()).child(text);
    div()
        .flex_none()
        .min_h(px(size * 1.3))
        .mb(px(size * if p.heading > 0 || p.title { 0.45 } else { 0.35 }))
        .when(p.heading > 0 || p.title, |d| d.mt(px(size * 0.4)).font_weight(FontWeight::SEMIBOLD))
        .pl(px(p.indent * k * 0.6))
        .text_size(px(size))
        .line_height(relative(1.3))
        .flex()
        .flex_col()
        .child(div().flex().gap(px(6.)).children(p.marker.clone().map(|m| div().flex_none().min_w(px(14.)).child(m))).child(line))
        .children(p.pictures.iter().map(|pic| {
            let w = (pic.w * k).min(width).max(16.);
            let h = if pic.w > 0. { w * pic.h / pic.w } else { w };
            div().mt(px(4.)).child(img(pic.image.clone()).w(px(w)).h(px(h)))
        }))
}

fn message(text: &str) -> AnyElement {
    div().flex_1().flex().items_center().justify_center().text_color(t::fg3()).child(text.to_string()).into_any_element()
}

/// A view that fills (`size_full`) whatever space is left under a toolbar row: the box is that
/// space, so the view's own scrolling reaches its end.
fn fill(view: AnyElement) -> AnyElement {
    div().flex_1().min_h_0().w_full().child(view).into_any_element()
}

#[allow(dead_code)]
fn _p(_: PathBuf) {}

impl Ferret {
    /// A path in the details was copied in the last moment (it says "Copied" in green there).
    pub fn just_copied(&self) -> bool {
        self.meta_copied.is_some_and(|t| t.elapsed() < COPIED_FOR)
    }

    /// Says "Copied" by the path for a moment.
    pub fn note_copied(&mut self, cx: &mut Context<Self>) {
        self.meta_copied = Some(std::time::Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}

/// How long "Copied" shows.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1600);

// ───────────────────────────── selected text ─────────────────────────────

impl Ferret {
    /// Ctrl+A, Ctrl+C and Esc for the preview's text (Quick Look's while it's open), after a
    /// press on it or F6 to the preview. False if they aren't for it (Ctrl+C with nothing
    /// selected copies the files, as before).
    pub fn preview_text_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let sel = self.text_sel[self.quicklook as usize].clone();
        let mut s = sel.borrow_mut();
        let f6 = !self.quicklook && self.kbd == crate::view::Area::Preview;
        if !s.shown || !(s.active || f6) {
            return false;
        }
        match key {
            "a" => {
                s.select_all();
                drop(s);
                cx.notify();
                true
            }
            "c" => {
                let Some(text) = s.selected_text() else { return false };
                drop(s);
                self.copy_preview_text(text, cx);
                true
            }
            "escape" if s.has_selection() => {
                s.clear();
                drop(s);
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// The area around a preview's selectable text (the pane's, or Quick Look's when `big`):
    /// presses and drags select (selectable.rs), a right-click opens Copy / Select all.
    fn text_area(&self, big: bool, cx: &mut Context<Self>) -> Div {
        selectable::area(&self.text_sel[big as usize]).on_mouse_down(
            MouseButton::Right,
            cx.listener(move |f, ev: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                f.text_menu(big, ev.position, window, cx);
            }),
        )
    }

    /// The menu on preview text: Copy (what's selected) and Select all, as the app's other menus.
    fn text_menu(&mut self, big: bool, at: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.text_menu_with(big, at, Vec::new(), window, cx);
    }

    /// The menu on a JSON tree's row: Copy and Select all, then that value's JSON (pretty-printed)
    /// and its path (\`items[2].name\`).
    pub fn json_menu(&mut self, big: bool, at: gpui::Point<gpui::Pixels>, root: Arc<serde_json::Value>, path: Vec<render::JsonKey>, window: &mut Window, cx: &mut Context<Self>) {
        use crate::menu::{item, sep};
        use crate::view::action;
        let r2 = root.clone();
        let p2 = path.clone();
        let mut extra = vec![
            sep(),
            item(
                "Copy value",
                action(move |f, _, cx| {
                    if let Some(v) = render::json_at(&r2, &p2) {
                        f.copy_preview_text(serde_json::to_string_pretty(v).unwrap_or_default(), cx);
                    }
                }),
            )
            .icon("braces"),
        ];
        if !path.is_empty() {
            let text = render::json_path_text(&path);
            extra.push(
                item(
                    "Copy path",
                    action(move |f, _, cx| {
                        f.copy_preview_text(text.clone(), cx);
                    }),
                )
                .icon("link"),
            );
        }
        let _ = root;
        self.text_menu_with(big, at, extra, window, cx);
    }

    fn text_menu_with(&mut self, big: bool, at: gpui::Point<gpui::Pixels>, extra: Vec<crate::menu::MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        let has = {
            let mut s = self.text_sel[big as usize].borrow_mut();
            s.active = true;
            s.has_selection()
        };
        use crate::menu::item;
        use crate::view::action;
        let mut items = vec![
            item(
                "Copy",
                action(move |f, _, cx| {
                    let text = f.text_sel[big as usize].borrow().selected_text();
                    if let Some(text) = text {
                        f.copy_preview_text(text, cx);
                    }
                }),
            )
            .icon("copy")
            .keys("Ctrl+C")
            .disabled(!has),
            item(
                "Select all",
                action(move |f, _, cx| {
                    f.text_sel[big as usize].borrow_mut().select_all();
                    cx.notify();
                }),
            )
            .keys("Ctrl+A"),
        ];
        items.extend(extra);
        self.open_menu(at, items, window, cx);
    }

    /// Puts preview text on the clipboard (tests leave it alone: FERRET_GPUI_NO_CLIPBOARD).
    fn copy_preview_text(&mut self, text: String, cx: &mut Context<Self>) {
        if std::env::var_os("FERRET_GPUI_NO_CLIPBOARD").is_none() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
        }
        // Only how much: a document's text doesn't belong in the log.
        log::info!("copied {} characters of preview text", text.chars().count());
        self.text_copied = Some(text);
    }

    // ───────────────────────────── finding (Ctrl+F) ─────────────────────────────

    /// Ctrl+F: finds in the preview's text (Quick Look's while it's open) when the keyboard is in
    /// the preview: F6 put it there, the last press was on its text, or its find bar has it. False
    /// otherwise (Ctrl+F filters the folder, as before) or when the preview shows no text (a locked
    /// PDF: unlock it first).
    pub fn open_preview_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let big = self.quicklook;
        let sel = self.text_sel[big as usize].clone();
        let in_password = self.pdf_unlock.as_ref().is_some_and(|u| u.input.read(cx).is_focused(window));
        // (Its find bar still open counts too: Ctrl+F goes back to it.)
        let here = big || in_password || (self.kbd == crate::view::Area::Preview && self.focus.is_focused(window)) || sel.borrow().active || sel.borrow().find.is_some();
        if !here {
            return false;
        }
        if !sel.borrow().shown {
            // No text to find in: a locked PDF is unlocked first (its field gets the keyboard).
            return in_password || self.focus_pdf_password(window, cx);
        }
        let open = sel.borrow().find.as_ref().and_then(|f| f.input.clone());
        if let Some(input) = open {
            // Already open: back to its field, what's typed selected to type over.
            input.update(cx, |i, cx| i.select_everything(cx));
            crate::input::focus(&input, window, cx);
            cx.notify();
            return true;
        }
        let input = cx.new(|cx| TextInput::new("Find in this file", cx).sized(12.5, 18.).named("Find in preview"));
        let sub = cx.subscribe(&input, move |f, input, _: &crate::input::Changed, cx| {
            let text = input.read(cx).text().to_string();
            f.text_sel[big as usize].borrow_mut().find_text(&text);
            cx.notify();
        });
        sel.borrow_mut().open_find(input.clone(), sub);
        // A word or so selected is what's looked for first (as in browsers and editors), else
        // what was looked for last.
        let last = sel.borrow().last_find.clone();
        let picked = sel.borrow().selected_text().filter(|t| !t.contains('\n') && !t.trim().is_empty() && t.chars().count() <= 100).or((!last.is_empty()).then_some(last));
        if let Some(t) = picked {
            input.update(cx, |i, cx| {
                i.set_text(&t, cx);
                i.select_everything(cx);
            });
        }
        crate::input::focus(&input, window, cx);
        cx.notify();
        true
    }

    /// Which find bar has the keyboard (Quick Look's: true), if one does.
    pub fn preview_find_focused(&self, window: &Window, cx: &gpui::App) -> Option<bool> {
        (0..2).find(|&i| self.text_sel[i].borrow().find.as_ref().and_then(|f| f.input.as_ref()).is_some_and(|input| input.read(cx).is_focused(window))).map(|i| i == 1)
    }

    /// Esc (or ×) closes the find bar: the matches go, the keyboard goes back where it was.
    pub fn close_preview_find(&mut self, big: bool, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.preview_find_focused(window, cx) == Some(big);
        let typed = self.text_sel[big as usize].borrow().find.as_ref().and_then(|f| f.input.as_ref()).map(|i| i.read(cx).text().to_string());
        {
            let mut s = self.text_sel[big as usize].borrow_mut();
            if let Some(t) = typed {
                s.last_find = t;
            }
            s.close_find();
        }
        if focused {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The next match (or the one before).
    fn preview_find_step(&mut self, big: bool, back: bool, cx: &mut Context<Self>) {
        self.text_sel[big as usize].borrow_mut().find_step(back);
        cx.notify();
    }

    /// The find bar over the preview's text, while it's open: the field, "3 of 12", previous,
    /// next, close. Enter / Shift+Enter (and F3 / Shift+F3) step through the matches, Esc closes.
    fn find_bar(&self, big: bool, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (input, (cur, n)) = {
            let s = self.text_sel[big as usize].borrow();
            (s.find.as_ref()?.input.clone()?, s.find_status()?)
        };
        let typed = !input.read(cx).text().is_empty();
        let focused = input.read(cx).is_focused(window);
        let count = if !typed {
            String::new()
        } else if n == 0 {
            "No matches".to_string()
        } else {
            format!("{cur} of {}{}", kinds::group_digits(n), if n >= selectable::MAX_HITS { "+" } else { "" })
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(4.))
                .on_key_down(cx.listener(move |f, ev: &gpui::KeyDownEvent, window, cx| match ev.keystroke.key.as_str() {
                    "enter" | "f3" => {
                        f.preview_find_step(big, ev.keystroke.modifiers.shift, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        f.close_preview_find(big, window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }))
                .child(ui::field(focused).flex_1().min_w_0().flex().items_center().gap(px(6.)).child(icon("search", 13., t::fg3())).child(div().flex_1().min_w_0().child(input)))
                .child(div().flex_none().min_w(px(56.)).text_right().text_size(px(11.5)).text_color(if typed && n == 0 { t::danger() } else { t::fg3() }).child(count))
                .child(ui::icon_btn(("find-prev", big as usize), "arrow-up", n > 0).tooltip(ui::tip("Previous match (Shift+Enter)")).on_click(cx.listener(move |f, _, _, cx| f.preview_find_step(big, true, cx))))
                .child(ui::icon_btn(("find-next", big as usize), "arrow-down", n > 0).tooltip(ui::tip("Next match (Enter)")).on_click(cx.listener(move |f, _, _, cx| f.preview_find_step(big, false, cx))))
                .child(ui::icon_btn(("find-close", big as usize), "x", true).tooltip(ui::tip("Close (Esc)")).on_click(cx.listener(move |f, _, window, cx| f.close_preview_find(big, window, cx))))
                .into_any_element(),
        )
    }

    /// The preview's content with the find bar over it while it's open; a preview that shows no
    /// text (a picture, a locked PDF) closes it.
    fn with_find_bar(&mut self, el: AnyElement, big: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (open, shown) = {
            let s = self.text_sel[big as usize].borrow();
            (s.find.is_some(), s.shown)
        };
        if !open {
            return el;
        }
        if !shown {
            self.close_preview_find(big, window, cx);
            return el;
        }
        match self.find_bar(big, window, cx) {
            Some(bar) => div().flex_1().min_h_0().flex().flex_col().gap(px(6.)).child(bar).child(div().flex_1().min_h_0().flex().flex_col().child(el)).into_any_element(),
            None => el,
        }
    }

    /// The keyboard came to a preview asking for a PDF's password (F6 to the preview, F6 or Tab in
    /// Quick Look): into its field, so typing goes there and Enter unlocks. False if there's none.
    pub fn focus_pdf_password(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let locked = self.preview_data.as_ref().is_some_and(|l| matches!(l.content, Content::PdfLocked { .. }));
        let Some(input) = self.pdf_unlock.as_ref().filter(|_| locked).map(|u| u.input.clone()) else { return false };
        crate::input::focus(&input, window, cx);
        cx.notify();
        true
    }

    // ───────────────────────────── PDF passwords ─────────────────────────────

    /// A PDF that wants a password: a lock, what it is, and the field to type it in (Enter or
    /// Unlock). The pane under Quick Look shows the lock only: the field is Quick Look's.
    fn pdf_lock_view(&mut self, path: &str, real: String, tried: usize, big: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let interactive = big || !self.quicklook;
        if interactive && self.pdf_unlock.as_ref().is_none_or(|u| u.real != real) {
            let input = cx.new(|cx| TextInput::new("Password", cx).sized(13., 20.).masked(true).named("PDF password"));
            let sub = cx.subscribe(&input, |f, _, _: &crate::input::Changed, cx| {
                if let Some(u) = f.pdf_unlock.as_mut() {
                    u.error = None;
                }
                cx.notify();
            });
            self.pdf_unlock = Some(PdfUnlock { path: path.to_string(), real: real.clone(), input, error: None, busy: false, _sub: sub });
        }
        let msg = if tried > 0 { "None of the passwords typed this session open it. Type its password to see it." } else { "Type its password to see it." };
        let card = div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .px(px(16.))
            .child(div().size(px(64.)).rounded_full().bg(t::hover()).border_1().border_color(t::line()).flex().items_center().justify_center().child(icon("lock", 28., t::fg2())))
            .child(ui::note("pdf-lock-title", "This PDF is password-protected").mt(px(2.)).font_weight(FontWeight::SEMIBOLD).text_center())
            .child(ui::note("pdf-lock-msg", msg).max_w(px(300.)).text_size(px(12.5)).text_color(t::fg3()).text_center());
        let Some(u) = self.pdf_unlock.as_ref().filter(|_| interactive) else { return card.into_any_element() };
        let focused = u.input.read(cx).is_focused(window);
        let busy = u.busy;
        card
            // A press beside the field puts the cursor in it.
            .on_mouse_down(MouseButton::Left, cx.listener(|f, _, window, cx| {
                if let Some(input) = f.pdf_unlock.as_ref().map(|u| u.input.clone()) {
                    crate::input::focus(&input, window, cx);
                    window.prevent_default();
                }
            }))
            .child(
                div()
                    .w_full()
                    .max_w(px(300.))
                    .mt(px(4.))
                    .flex()
                    .gap(px(8.))
                    .child(ui::field(focused).flex_1().min_w_0().child(u.input.clone()))
                    .child(
                        ui::button("pdf-unlock", if busy { "Unlocking…" } else { "Unlock" }, true)
                            .when(busy, |d| d.opacity(0.6))
                            .on_click(cx.listener(|f, _, window, cx| f.submit_pdf_password(window, cx))),
                    ),
            )
            .children(u.error.clone().map(|e| ui::note("pdf-lock-error", e).role(gpui::Role::Alert).text_size(px(12.)).text_color(t::danger())))
            .into_any_element()
    }

    /// Tries the typed password: it opens the PDF (and is kept for this session, in memory only),
    /// or the field says it's wrong and keeps it, selected, to type again.
    pub fn submit_pdf_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(u) = self.pdf_unlock.as_mut() else { return };
        let password = u.input.read(cx).text().to_string();
        if password.is_empty() || u.busy {
            return;
        }
        u.busy = true;
        u.error = None;
        let real = u.real.clone();
        let r2 = real.clone();
        let task = cx.background_executor().spawn(async move { render::pdf_unlock(&r2, &password) });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            this.update_in(cx, |f, window, cx| {
                if f.pdf_unlock.as_ref().is_none_or(|u| u.real != real) {
                    return;
                }
                match r {
                    Ok(()) => f.pdf_unlocked(window, cx),
                    Err(why) => {
                        if let Some(u) = f.pdf_unlock.as_mut() {
                            u.busy = false;
                            u.error = Some(match why {
                                None => "Wrong password".into(),
                                Some(w) => format!("Couldn't open it: {w}").into(),
                            });
                            u.input.update(cx, |i, cx| i.select_everything(cx));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// A password opened a PDF: previews that were locked are read again (it may open them too).
    fn pdf_unlocked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pdf_unlock = None;
        let locked = |l: &Loaded| matches!(l.content, Content::PdfLocked { .. });
        self.preview_cache.retain(|l| !locked(l));
        if self.preview_data.as_ref().is_some_and(locked) {
            self.preview_data = None;
        }
        self.preview_want = None;
        window.focus(&self.focus, cx);
        self.sync_preview(cx);
        cx.notify();
    }

    /// The password field goes with the PDF it was for (and the keyboard back to the files, if
    /// it was in it).
    pub fn sync_pdf_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(u) = self.pdf_unlock.as_ref() else { return };
        let p = self.pane();
        let current = p.cursor_row().filter(|_| p.marked.len() <= 1).map(|r| r.e.path.as_str());
        if current == Some(u.path.as_str()) && (self.preview_open || self.quicklook) {
            return;
        }
        let focused = u.input.read(cx).is_focused(window);
        self.pdf_unlock = None;
        if focused {
            window.focus(&self.focus, cx);
        }
    }

    /// Reads the shown PDF's text (once per file, in the background) for its text layer; then its
    /// pages with no text of their own (`shown`: how many the preview has) are read with text
    /// recognition (again when the better reader arrives, or when the preview comes back to it).
    fn sync_pdf_text(&mut self, real: String, shown: usize, cx: &mut Context<Self>) {
        if self.pdf_text.as_ref().is_some_and(|(p, _)| *p == real) {
            let loaded = self.pdf_text.as_ref().is_some_and(|(_, t)| t.is_some());
            let restart = match self.ocr.as_ref().filter(|o| o.path == real) {
                None => loaded,
                // stopped while away, or a better reader since
                Some(o) => o.total > 0 && ((o.done < o.total && o.run.cancel.load(std::sync::atomic::Ordering::Relaxed)) || o.reader != crate::ocr::reader()),
            };
            if restart {
                self.start_ocr(real, cx);
            }
            return;
        }
        if let Some(o) = self.ocr.take() {
            o.run.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.pdf_text = Some((real.clone(), None));
        let r2 = real.clone();
        let task = cx.background_executor().spawn(async move { crate::pdftext::extract(&r2, 200).map(Arc::new) });
        cx.spawn(async move |this, cx| {
            let text = task.await;
            this.update(cx, |f, cx| {
                if let Some((p, slot)) = f.pdf_text.as_mut() {
                    if *p == real {
                        // A PDF whose text can't be read at all: its pages are read as pictures.
                        *slot = Some(text.unwrap_or_else(|| Arc::new(crate::pdftext::PdfText { pages: vec![Vec::new(); shown] })));
                        f.start_ocr(real.clone(), cx);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reads the shown PDF's pages that have (almost) no text of their own, in the background,
    /// each laid over its page as it's read. Pages read before (this session, by the same reader)
    /// come back at once.
    fn start_ocr(&mut self, real: String, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let Some(text) = self.pdf_text.as_ref().filter(|(p, _)| *p == real).and_then(|(_, t)| t.clone()) else { return };
        let base = match self.ocr.take() {
            Some(o) if o.path == real => {
                o.run.cancel.store(true, Ordering::Relaxed);
                o.base
            }
            Some(o) => {
                o.run.cancel.store(true, Ordering::Relaxed);
                text
            }
            None => text,
        };
        // A scanned page: no text, or a few characters (a stamp, a page number).
        let pages: Vec<u32> = (0..base.pages.len()).filter(|&i| base.pages[i].iter().map(|l| l.text.trim().chars().count()).sum::<usize>() < 20).map(|i| i as u32).collect();
        let reader = crate::ocr::reader();
        let known = crate::ocr::known(&real, reader);
        let mut merged = (*base).clone();
        for (&p, lines) in &known {
            if pages.contains(&p) {
                if let Some(slot) = merged.pages.get_mut(p as usize) {
                    *slot = (**lines).clone();
                }
            }
        }
        let todo: Vec<u32> = pages.iter().copied().filter(|p| !known.contains_key(p)).collect();
        let run = Arc::new(crate::ocr::Run { cancel: AtomicBool::new(false), near: AtomicU32::new(0) });
        self.ocr = Some(crate::ocr::OcrView {
            path: real.clone(),
            base,
            total: pages.len(),
            done: pages.len() - todo.len(),
            reader,
            run: run.clone(),
            found_any: pages.iter().any(|p| known.get(p).is_some_and(|l| !l.is_empty())),
            problem: None,
        });
        if !pages.is_empty() {
            self.pdf_text = Some((real.clone(), Some(Arc::new(merged))));
        }
        cx.notify();
        if todo.is_empty() {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        crate::ocr::read_pages(real.clone(), todo, run.clone(), move |page, result| {
            let _ = tx.unbounded_send((page, result));
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some((page, result)) = rx.next().await {
                let going = this
                    .update(cx, |f, cx| {
                        let Some(o) = f.ocr.as_mut().filter(|o| o.path == real && Arc::ptr_eq(&o.run, &run)) else { return false };
                        o.done += 1;
                        match result {
                            Ok((_, lines)) => {
                                o.found_any |= !lines.is_empty();
                                if let Some((p, Some(t))) = f.pdf_text.as_mut() {
                                    if *p == real {
                                        let mut next = (**t).clone();
                                        if let Some(slot) = next.pages.get_mut(page as usize) {
                                            *slot = (*lines).clone();
                                        }
                                        *t = Arc::new(next);
                                    }
                                }
                            }
                            Err(e) => o.problem = Some(e),
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    /// The note over a scanned PDF's pages: reading it, or that its text was recognised (and by
    /// Windows: the offer of the better reader, the add-on).
    fn ocr_note(&self, real: &str, big: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let o = self.ocr.as_ref().filter(|o| o.path == real && o.total > 0)?;
        let running = o.running();
        let windows = o.reader == crate::ocr::Reader::Windows;
        let (icon_el, said): (AnyElement, String) = if running {
            let n = if o.total > 1 { format!(" · page {} of {}", (o.done + 1).min(o.total), o.total) } else { String::new() };
            let how = if !windows && crate::ocr::known(real, crate::ocr::Reader::Windows).len() > 0 { "Reading the text again, better" } else { "Reading the text in this scan" };
            (ui::spinner(12., t::fg3()).into_any_element(), format!("{how}…{n}"))
        } else if o.found_any {
            (icon("text-search", 13., t::fg3()).into_any_element(), if o.total > 1 { format!("Text recognised on {} scanned pages", o.total) } else { "Text recognised from the scan".to_string() })
        } else if o.done < o.total {
            (icon("text-search", 13., t::fg3()).into_any_element(), "Reading the text paused".to_string())
        } else {
            (icon("text-search", 13., t::fg3()).into_any_element(), "No text found in this scan".to_string())
        };
        let problem = o.problem.clone().filter(|_| !running);
        let id = big as usize;
        // Small buttons: the note sits in a narrow pane.
        let small = |b: gpui::Stateful<gpui::Div>| b.h(px(26.)).px(px(10.)).text_size(px(12.)).rounded(px(6.));
        // The better reader: offered where Windows read it, until it's taken or put off.
        let line = |icon_name: &'static str, color: gpui::Hsla, text: String| {
            div().flex().items_start().gap(px(6.)).child(div().pt(px(1.)).flex_none().child(icon(icon_name, 13., color))).child(div().flex_1().min_w_0().text_color(t::fg2()).child(text))
        };
        let addon: Option<AnyElement> = if !windows {
            None
        } else if let Some(why) = crate::ocr::addon_problem() {
            Some(line("circle-x", t::fg3(), format!("Better text recognition couldn't start: {why}")).into_any_element())
        } else {
            match crate::addons::ocr_pack() {
                // One line where there's room (Quick Look); the buttons under it in the pane.
                crate::addons::OcrPack::Missing if !self.settings.bool("ocrOfferDismissed", false) => Some(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x(px(12.))
                        .gap_y(px(6.))
                        .child(line("sparkles", t::accent(), "Better recognition for tables, faxes and poor scans".to_string()))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .pl(px(19.))
                                .child(small(ui::button(("ocr-get", id), format!("Get it · {} MB", crate::addons::OCR_PACK_MB), true)).on_click(cx.listener(|_, _, _, cx| {
                                    crate::addons::start_ocr_download(cx);
                                    cx.notify();
                                })))
                                .child(small(ui::button(("ocr-later", id), "Not now", false)).on_click(cx.listener(|f, _, _, cx| {
                                    f.settings.set("ocrOfferDismissed", serde_json::Value::Bool(true));
                                    cx.notify();
                                }))),
                        )
                        .into_any_element(),
                ),
                crate::addons::OcrPack::Downloading { got, total } => {
                    let frac = if total > 0 { got as f32 / total as f32 } else { 0. };
                    // all of it here: being checked and unpacked
                    let said = if total > 0 && got >= total {
                        "Checking and installing better text recognition…".to_string()
                    } else if total > 0 {
                        format!("Getting better text recognition… {}%", got * 100 / total)
                    } else {
                        "Getting better text recognition…".to_string()
                    };
                    Some(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(line("download", t::accent(), said))
                            .child(div().pl(px(19.)).child(ui::bar(frac, t::accent())))
                            .into_any_element(),
                    )
                }
                crate::addons::OcrPack::Failed(why) => Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(line("circle-x", t::fg3(), format!("Couldn't get better text recognition: {why}")))
                        .child(div().pl(px(19.)).child(small(ui::button(("ocr-retry", id), "Try again", false)).on_click(cx.listener(|_, _, _, cx| {
                            crate::addons::start_ocr_download(cx);
                            cx.notify();
                        }))))
                        .into_any_element(),
                ),
                _ => None,
            }
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .mb(px(8.))
                .px(px(10.))
                .py(px(7.))
                .rounded(px(7.))
                .bg(t::rgba(0x808080, 0.08))
                .text_size(px(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_color(t::fg3())
                        .child(div().flex_none().child(icon_el))
                        .child(div().flex_1().min_w_0().child(said)),
                )
                .children(problem.map(|p| line("circle-x", t::fg3(), format!("A page couldn't be read: {p}"))))
                .children(addon)
                .into_any_element(),
        )
    }
}

impl Ferret {
    /// A picture still loading: its thumbnail (the sharp one when made, else the small one the
    /// header has), laid out as the picture will be, so switching between pictures doesn't flash
    /// an empty pane with a spinner.
    fn picture_stand_in(&mut self, path: &str, kind: Kind, big: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        if kind != Kind::Image || path.to_lowercase().ends_with(".svg") {
            return None;
        }
        let thumb = self.thumb(path, 512, false, None, cx).or_else(|| self.thumb(path, 64, false, None, cx))?;
        let (w, h) = crate::files::png_size(thumb.bytes())?;
        // Quick Look doesn't blow small pictures up: there the thumbnail stands for a big one.
        let k = if big { 8 } else { 1 };
        Some(self.picture_view(thumb, (w * k, h * k), big, cx))
    }
}
