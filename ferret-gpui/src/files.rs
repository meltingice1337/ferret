//! The file view (src/components/FileView.tsx): column headings, the list and the grid, group
//! headings, the rubber band, inline rename, folder sizes, thumbnails, the bars of the Recycle
//! Bin, archives and search tabs, and a split pane's header.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferret_core::paths;
use gpui::AnimationExt;
use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Div, FontWeight, Image, ImageFormat, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, Pixels, Point, SharedString, Stateful, Window, canvas, div, list, point, prelude::*,
    px, relative, uniform_list,
};

use crate::imgmem::img;

use crate::kinds::{self, Kind};
use crate::pane::{HEADER_H, Item, Nav, Pane};
use crate::settings::Column;
use crate::sortgroup::{GroupBy, Mode, SortBy, SortDir};
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util;
use crate::view::{Band, Ferret, Press, Resize, Thumb};

/// Where a list's scrollbar thumb is (`scroll_geometry`).
struct ScrollGeometry {
    /// The list's top, in window px, and its height.
    origin_y: f32,
    h: f32,
    /// How far the list scrolls.
    max: f32,
    /// The thumb's height, and its top within the list.
    thumb: f32,
    top: f32,
}

impl Ferret {
    /// One pane of the active tab (`second`: the split one).
    pub fn pane_view(&mut self, second: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let Some(p) = self.slot(second) else { return div().into_any_element() };
        let id = p.id;
        let split = self.tab().split.is_some();
        let in_use = !split || self.tab().split_focused == second;
        // Width changes (window resized, preview opened) lay the grid out again.
        let w = p.bounds.get().size.width.as_f32();
        if (w - p.width).abs() > 0.5 && w > 0. {
            if let Some(p) = self.pane_by(id) {
                p.width = w;
                if p.view.mode == Mode::Grid {
                    p.relayout();
                }
            }
        }
        let p = self.slot(second).unwrap();
        let header = split.then(|| self.pane_header(p, second, in_use, cx));
        let bar = self.special_bar(p, cx);
        let heads = (p.view.mode == Mode::List).then(|| self.col_heads(p, cx));
        let body = self.file_body(id, in_use, window, cx);
        div()
            .id(("pane", id as usize))
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, _, _, cx| f.focus_pane(second, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, _, _, cx| f.focus_pane(second, cx)))
            .children(header)
            .children(bar)
            .children(heads)
            .child(body)
            .into_any_element()
    }

    fn pane_header(&self, p: &Pane, second: bool, in_use: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(("pane-head", second as usize))
            .flex_none()
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(7.))
            .pl(px(12.))
            .pr(px(8.))
            .text_size(px(12.))
            .text_color(t::fg3())
            .border_b_1()
            .border_color(t::line())
            .relative()
            .when(in_use, |d| d.child(div().absolute().left_0().right_0().bottom_0().h(px(2.)).bg(t::accent())))
            .tooltip(ui::tip(p.path.clone()))
            .on_click(cx.listener(move |f, _, _, cx| f.focus_pane(second, cx)))
            .child(icon(if p.is_bin() { "trash-2" } else { "folder" }, 14., if in_use { t::accent_text() } else { t::fg3() }))
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if in_use { t::accent_text() } else { t::fg2() })
                    .child(util::folder_name(&p.path)),
            )
            .child(div().flex_1().min_w_0().text_ellipsis_start().overflow_hidden().whitespace_nowrap().child(p.path.clone()))
            .when(second, |d| {
                d.child(
                    ui::icon_btn("pane-close", "x", true)
                        .w(px(22.))
                        .h(px(22.))
                        .tooltip(ui::tip("Close split view (Ctrl+\\)"))
                        .on_click(cx.listener(|f, _, _, cx| {
                            f.tabs[f.active].split_focused = true;
                            f.close_split_keep_main(cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The × on the second pane closes it and keeps the first.
    pub fn close_split_keep_main(&mut self, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        tab.split = None;
        tab.split_focused = false;
        self.save_session();
        cx.notify();
    }

    /// The Recycle Bin's, an archive's or a search tab's bar above the list.
    fn special_bar(&self, p: &Pane, cx: &mut Context<Self>) -> Option<AnyElement> {
        let frame = || {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .mx(px(8.))
                .mt(px(6.))
                .mb(px(2.))
                .pl(px(12.))
                .pr(px(10.))
                .py(px(8.))
                .rounded(px(9.))
                .bg(t::hover())
                .text_color(t::fg2())
        };
        let count = |s: String| div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(t::fg()).whitespace_nowrap().child(s);
        let hint = |s: &'static str| div().flex_1().min_w_0().text_size(px(12.)).text_color(t::fg3()).truncate().child(s);
        let btn = |id: &'static str, icon_name: &'static str, label: String| {
            div()
                .id(id)
                .flex_none()
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(7.))
                .bg(t::hover())
                .border_1()
                .border_color(t::line2())
                .text_color(t::fg())
                .text_size(px(12.5))
                .hover(|s| s.bg(t::press()))
                .child(icon(icon_name, 14., t::fg2()))
                .child(label)
        };
        if p.is_bin() {
            let n = p.entries.len();
            let bytes: u64 = p.entries.iter().map(|e| e.size).sum();
            let sel = p.marked.len();
            return Some(
                frame()
                    .child(icon("trash-2", 15., t::fg2()))
                    .child(count(if n > 0 { format!("{} · {}", plural(n, "item"), kinds::format_size(bytes)) } else { "Empty".into() }))
                    .child(hint("Kept until you empty the bin"))
                    .when(n > 0, |d| {
                        d.child(
                            btn("bin-restore", "archive-restore", if sel > 0 { format!("Restore {}", plural(sel, "item")) } else { "Restore all".into() })
                                .on_click(cx.listener(|f, _, _, cx| {
                                    let p = f.pane();
                                    let items = if p.marked.is_empty() { p.rows.iter().map(|r| r.e.path.clone()).collect() } else { p.marked_paths() };
                                    f.restore_from_bin(items, cx);
                                })),
                        )
                        .child(
                            btn("bin-empty", "trash-2", "Empty Recycle Bin".into())
                                .text_color(t::danger())
                                .on_click(cx.listener(|f, _, _, cx| f.empty_bin(cx))),
                        )
                    })
                    .into_any_element(),
            );
        }
        if let Some(q) = ferret_core::search::search_query_of(&p.path) {
            let n = p.entries.len();
            let saved = self.settings.saved_searches().iter().any(|(_, s)| s == q);
            let q = q.to_string();
            let q2 = q.clone();
            return Some(
                frame()
                    .child(icon("search", 15., t::fg2()))
                    .child(count(if p.loading { "Searching…".into() } else if n >= 2000 { "First 2,000 matches".into() } else { plural(n, "match") }))
                    .child(
                        div()
                            .min_w_0()
                            .max_w(relative(0.4))
                            .truncate()
                            .font_family(t::MONO)
                            .text_size(px(12.))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .bg(t::accent_a(0.16))
                            .text_color(t::fg())
                            .child(q.clone()),
                    )
                    .child(hint("Always current · F5 searches again"))
                    .child(btn("search-edit", "pencil", "Edit search".into()).on_click(cx.listener(move |f, _, window, cx| {
                        f.open_palette(Some(q.clone()), window, cx)
                    })))
                    // Filled once saved, as the app's.
                    .child(btn("search-save", if saved { "star-fill" } else { "star" }, if saved { "Rename".into() } else { "Save to sidebar".into() }).on_click(
                        cx.listener(move |f, _, _, cx| f.prompt_save_search(q2.clone(), cx)),
                    ))
                    .into_any_element(),
            );
        }
        let root = ferret_core::archive::split_virtual(&p.path).map(|(a, _)| a.to_string());
        if let Some(archive) = root.filter(|_| !p.loading && (p.entries.is_empty() || p.entries.iter().any(|e| e.in_archive))) {
            let sel = p.marked.len();
            let (a1, a2, a3) = (archive.clone(), archive.clone(), archive.clone());
            return Some(
                frame()
                    .child(icon("file-archive", 15., t::fg2()))
                    .child(count(kinds::type_label(false, &util::file_name(&archive).rsplit('.').next().unwrap_or("").to_lowercase())))
                    .child(hint("Read-only: extract it to make changes"))
                    .when(sel > 0, |d| {
                        d.child(btn("arc-sel", "package-open", format!("Extract {}…", plural(sel, "selected item"))).on_click(cx.listener(
                            move |f, _, _, cx| {
                                let entries = f.pane().marked_paths();
                                f.extract_to_picked(a1.clone(), Some(entries), cx)
                            },
                        )))
                    })
                    .child(btn("arc-to", "folder-open", "Extract to…".into()).on_click(cx.listener(move |f, _, _, cx| {
                        f.extract_to_picked(a2.clone(), None, cx)
                    })))
                    .child(
                        btn("arc-all", "package-open", "Extract all".into())
                            .bg(t::accent())
                            .text_color(t::on_accent())
                            .border_color(t::accent())
                            .on_click(cx.listener(move |f, _, _, cx| f.extract_smart(a3.clone(), cx))),
                    )
                    .into_any_element(),
            );
        }
        None
    }

    /// The list's headings: click to sort (again to reverse), drag the line between two to
    /// resize, right-click to hide columns.
    fn col_heads(&self, p: &Pane, cx: &mut Context<Self>) -> AnyElement {
        let v = p.view.clone();
        let bin = p.is_bin();
        let search = p.is_search();
        let w = p.width.max(1.);
        let show = |c: Column| -> bool {
            !self.settings.column_hidden(c)
                && match c {
                    Column::Type => w >= 600. && !bin,
                    Column::Modified => w >= 430.,
                    Column::Size => true,
                }
        };
        // The heading the keyboard is on (Shift+Tab from the files), in the pane in use.
        let ring_by = match self.kbd {
            crate::view::Area::Heads(i) if p.id == self.pane().id => self.visible_heads().get(i).map(|h| h.0),
            _ => None,
        };
        let head = |key: SortBy, label: &'static str, right: bool, grip: Option<Column>, cx: &mut Context<Self>| {
            let active = v.sort_by == key;
            let mut d = div()
                .id(SharedString::from(format!("head-{label}")))
                .role(gpui::Role::Button)
                .aria_label(if active { format!("Sort by {label}, sorted {}", if v.sort_dir == SortDir::Asc { "ascending" } else { "descending" }) } else { format!("Sort by {label}") })
                .relative()
                .when(ring_by == Some(key), |d| d.child(ui::ring(6.)))
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(26.))
                .px(px(8.))
                .rounded(px(6.))
                .text_color(if active { t::accent_text() } else { t::fg3() })
                .hover(|s| s.bg(t::hover()).text_color(if active { t::accent_text() } else { t::fg2() }))
                .when(right, |d| d.justify_end())
                .on_click(cx.listener(move |f, _, _, cx| f.sort_by(key, cx)))
                .child(div().truncate().child(label))
                .when(active, |d| d.child(icon(if v.sort_dir == SortDir::Asc { "arrow-up" } else { "arrow-down" }, 12., t::accent_text())));
            if let Some(col) = grip {
                d = d.child(
                    div()
                        .id(SharedString::from(format!("grip-{label}")))
                        .absolute()
                        .left(px(-6.))
                        .top(px(2.))
                        .bottom(px(2.))
                        .w(px(11.))
                        .cursor(gpui::CursorStyle::ResizeLeftRight)
                        .group("grip")
                        .child(div().absolute().left(px(5.)).top(px(5.)).bottom(px(5.)).w(px(1.)).bg(t::line2()).group_hover("grip", |s| s.bg(t::accent()).w(px(2.))))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                                f.resizing = Some(Resize::Column { col, from: ev.position.x.as_f32(), start: f.settings.column_width(col) });
                                cx.stop_propagation();
                            }),
                        )
                        .on_click(cx.listener(move |f, ev: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if ev.click_count() >= 2 {
                                f.settings.set_column_width(col, None);
                                cx.notify();
                            }
                        })),
                );
            }
            d
        };
        let date_label = if bin { "Date deleted" } else { "Modified" };
        let extra = if bin && w >= 600. { Some("Original location") } else if search && w >= 600. { Some("Folder") } else { None };
        div()
            .id("colheads")
            .h(px(32.))
            .flex_none()
            .flex()
            .items_center()
            .pl(px(10.))
            .pr(px(18.))
            .border_b_1()
            .border_color(t::line())
            .text_size(px(12.))
            .on_mouse_down(MouseButton::Right, cx.listener(|f, ev: &MouseDownEvent, window, cx| {
                f.column_menu(ev.position, window, cx);
                cx.stop_propagation();
            }))
            .child(head(SortBy::Name, "Name", false, None, cx).flex_1().min_w(px(160.)))
            .when_some(extra, |d, label| d.child(div().flex_none().w(px(200.)).px(px(8.)).text_color(t::fg3()).child(label)))
            .when(show(Column::Modified), |d| {
                d.child(head(SortBy::Modified, date_label, false, Some(Column::Modified), cx).flex_none().w(px(self.settings.column_width(Column::Modified))))
            })
            .when(show(Column::Type), |d| {
                d.child(head(SortBy::Type, "Type", false, Some(Column::Type), cx).flex_none().w(px(self.settings.column_width(Column::Type))))
            })
            .when(show(Column::Size), |d| {
                d.child(head(SortBy::Size, "Size", true, Some(Column::Size), cx).flex_none().w(px(self.settings.column_width(Column::Size))))
            })
            .into_any_element()
    }

    fn file_body(&mut self, id: u64, in_use: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.pane_ref(id).unwrap();
        let bounds = p.bounds.clone();
        let root = cx.entity_id();
        let count = p.items.len();
        let empty_text: Option<String> = if let Some(err) = &p.error {
            Some(explain(err))
        } else if p.loading && !p.loaded_once {
            None
        } else if p.rows.is_empty() {
            Some(if p.is_bin() {
                "The Recycle Bin is empty".into()
            } else if p.is_search() {
                "Nothing matches this search right now".into()
            } else if !p.filter.is_empty() && p.unfiltered > 0 {
                format!("Nothing here matches “{}”", p.filter.trim())
            } else if p.hidden > 0 && !crate::pane::show_hidden() {
                "Only hidden items here. Press Ctrl+H to show them.".into()
            } else {
                "This folder is empty".into()
            })
        } else {
            None
        };
        let nav = p.nav;
        let nav_seq = p.nav_seq;
        let uniform = p.uniform();
        let band = self.band.as_ref().filter(|b| b.pane == id).map(|b| (b.from, b.to));
        let scroll_y = self.scroll_y(id);
        let list_el: AnyElement = if count == 0 {
            div().into_any_element()
        } else if uniform {
            uniform_list(
                ("files", id as usize),
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    range.map(|ix| this.item_view(id, ix, in_use, window, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.pane_ref(id).unwrap().scroll)
            .size_full()
            .pl(px(6.))
            .pr(px(18.))
            .pt(px(4.))
            .into_any_element()
        } else {
            let state = self.pane_ref(id).unwrap().list.clone();
            list(state, cx.processor(move |this, ix: usize, window, cx| this.item_view(id, ix, in_use, window, cx).into_any_element()))
                .size_full()
                .pl(px(6.))
                .pr(px(18.))
                .into_any_element()
        };
        let sticky = self.sticky_header(id, cx);
        let animate = t::animate() && nav_seq > 1;
        let list_name = self.pane_ref(id).map(|p| format!("{} files", util::folder_name(&p.path))).unwrap_or_default();
        let here = self.pane_ref(id).map(|p| p.path.clone()).unwrap_or_default();
        let body = div()
            .id(("files-body", id as usize))
            .role(gpui::Role::ListBox)
            .aria_label(list_name)
            .a11y_synthetic_children(|b| b.parent_node().set_multiselectable())
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            // Files dragged in (from another app, or from here): the list lights up; a drop
            // copies or moves them here.
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_softer()))
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(move |f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                if ev.bounds.contains(&ev.event.position) {
                    f.note_drag_target(ev.event.position, &here, 1, cx);
                }
            }))
            .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| {
                let dest = f.pane_ref(id).map(|p| p.path.clone()).unwrap_or_default();
                f.drop_paths(paths.paths().to_vec(), dest, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, window, cx| f.empty_press(id, ev, window, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, ev: &MouseDownEvent, _, cx| f.start_autoscroll(id, ev.position, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                f.background_menu(ev.position, window, cx);
            }))
            .on_scroll(cx.listener(move |f, ev: &gpui::ScrollEvent, _, cx| {
                // Ctrl+wheel: icon size (and list → grid).
                if ev.modifiers.control {
                    let dy = ev.delta.pixel_delta(px(20.)).y.as_f32();
                    if dy != 0. {
                        if f.pane().view.mode == Mode::List {
                            if dy > 0. {
                                f.set_view(|v| { v.mode = Mode::Grid; v.icon_size = 64; }, cx);
                            }
                        } else {
                            let s = f.pane().view.icon_size as i32 + if dy > 0. { 16 } else { -16 };
                            f.set_icon_size(s.clamp(48, 256) as u32, cx);
                        }
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                canvas(
                    move |b: Bounds<Pixels>, _, cx| {
                        // A new width (window resized, split, preview opened) lays the grid and the
                        // columns out again: the view is cached, so it's asked to draw again.
                        let old = bounds.get();
                        bounds.set(b);
                        if (old.size.width - b.size.width).abs() > px(0.5) {
                            cx.defer(move |cx| cx.notify(root));
                        }
                    },
                    |_, _, _, _| {},
                )
                    .absolute()
                    .size_full(),
            )
            .child(list_el)
            .children(empty_text.map(|text| {
                div()
                    .id("empty-text")
                    .role(gpui::Role::Label)
                    .aria_label(text.clone())
                    .aria_value(text.clone())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .items_center()
                    .justify_center()
                    .text_color(t::fg3())
                    .child(text.clone())
                    .when(text.starts_with("Not connected"), |d| {
                        d.child(ui::button("connect", "Connect", true).on_click(cx.listener(move |f, _, _, cx| {
                            let p = f.pane_ref(id).map(|p| (p.path.clone(), p.error.clone().unwrap_or_default()));
                            match p {
                                Some((path, e)) if e.starts_with(ferret_core::network::LOGIN_PREFIX) => f.network_sign_in(path, cx),
                                _ => f.refresh(cx),
                            }
                        })))
                    })
            }))
            .children(sticky)
            .children(self.scrollbar(id, cx))
            .children(band.map(|(a, b)| {
                // The band is kept in the list's content (it scrolls with it).
                let o = self.pane_ref(id).map(|p| p.bounds.get().origin).unwrap_or_default();
                div()
                    .absolute()
                    .left(a.x.min(b.x) - o.x)
                    .top(a.y.min(b.y) - px(scroll_y) - o.y)
                    .w((a.x - b.x).abs())
                    .h((a.y - b.y).abs().max(px(1.)))
                    .border_1()
                    .border_color(t::accent())
                    .bg(t::accent_a(0.12))
                    .rounded(px(3.))
            }));
        if animate && matches!(nav, Nav::Into | Nav::Out | Nav::Other) {
            let dir = match nav {
                Nav::Into => 1.,
                Nav::Out => -1.,
                Nav::Other => 0.,
            };
            let _ = window;
            body.with_animation(
                ("nav", (id as usize) << 20 | nav_seq as usize),
                gpui::Animation::new(Duration::from_millis(if dir == 0. { 120 } else { 170 })).with_easing(gpui::ease_out_quint()),
                move |el, d| el.left(px((1. - d) * 14. * dir)).opacity(0.35 + 0.65 * d),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        }
    }

    /// One laid-out item of a pane: a group heading, a list row or a grid row.
    fn item_view(&mut self, id: u64, ix: usize, in_use: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(p) = self.pane_ref(id) else { return div().id(ix) };
        match p.items.get(ix).cloned() {
            Some(Item::Header(g)) => self.group_head(p, g, false, cx).id(("ghead", ix)),
            Some(Item::Row(r)) => self.row(id, r, in_use, window, cx),
            Some(Item::Cells(range)) => {
                let a11y = window.is_a11y_active();
                let cells: Vec<AnyElement> = range.map(|r| self.cell(id, r, in_use, a11y, cx).into_any_element()).collect();
                let p = self.pane_ref(id).unwrap();
                let slot = ((p.width - 18.) / p.cols.max(1) as f32).max(p.cell_w());
                div()
                    .id(("grow", ix))
                    .h(px(p.view.icon_size as f32 + 58.))
                    .w_full()
                    .flex()
                    .pt(px(4.))
                    .children(cells.into_iter().map(|c| div().flex_none().w(px(slot)).px(px(3.)).child(c)))
            }
            None => div().id(ix),
        }
    }

    fn group_head(&self, p: &Pane, g: usize, sticky: bool, cx: &mut Context<Self>) -> Div {
        let group = &p.groups[g];
        let label = group.label.clone();
        let collapsed = p.collapsed.contains(&label);
        let (l1, l2) = (label.clone(), label.clone());
        let range = group.start..group.start + group.len;
        div()
            .h(px(if sticky { 30. } else { HEADER_H }))
            .w_full()
            .flex()
            .items_end()
            .child(
                div()
                    .id(SharedString::from(format!("group-{label}-{sticky}")))
                    .role(gpui::Role::Button)
                    .aria_label(format!("{label}, {} items", group.len))
                    .aria_expanded(!collapsed)
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .px(px(10.))
                    .pb(px(4.))
                    .when(sticky, |d| {
                        d.py(px(7.)).bg(if t::is_dark() { t::rgba(0x1c131c, 1.) } else { t::rgba(0xfbf7fa, 1.) }).rounded_b(px(9.)).border_b_1().border_color(t::line())
                    })
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t::fg2())
                    .hover(|s| s.text_color(t::fg()))
                    .tooltip(ui::tip("Click to collapse (← and → from the list; Ctrl+Shift+← → in a grid) · double-click to select the group"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |f, ev: &ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            let p = f.pane_mut();
                            p.marked = range.clone().collect();
                            p.selected = Some(range.start);
                            cx.notify();
                        } else {
                            f.toggle_group(&l1, cx);
                        }
                    }))
                    .child(icon(if collapsed { "chevron-right" } else { "chevron-down" }, 14., t::fg3()))
                    .child(l2)
                    .child(
                        div()
                            .px(px(6.))
                            .rounded(px(8.))
                            .bg(t::hover())
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t::fg3())
                            .child(group.len.to_string()),
                    )
                    .child(div().flex_1().h(px(1.)).ml(px(4.)).bg(t::line())),
            )
    }

    /// The heading of the group at the top of the view stays there while its items scroll.
    fn sticky_header(&self, id: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.pane_ref(id)?;
        if p.view.group_by == GroupBy::None || p.uniform() {
            return None;
        }
        let top = p.list.logical_scroll_top();
        if top.item_ix == 0 && top.offset_in_item.as_f32() < 1. {
            return None;
        }
        let g = p.items[..=top.item_ix.min(p.items.len().saturating_sub(1))].iter().rev().find_map(|it| match it {
            Item::Header(g) => Some(*g),
            _ => None,
        })?;
        Some(div().absolute().top_0().left(px(6.)).right(px(18.)).child(self.group_head(p, g, true, cx)).into_any_element())
    }

    /// How far the pane's list is scrolled (px).
    pub fn scroll_y(&self, id: u64) -> f32 {
        let Some(p) = self.pane_ref(id) else { return 0. };
        if p.uniform() {
            -p.scroll.0.borrow().base_handle.offset().y.as_f32()
        } else {
            let top = p.list.logical_scroll_top();
            p.tops.get(top.item_ix).copied().unwrap_or(0.) + top.offset_in_item.as_f32()
        }
    }

    /// The list scrolls without a native bar; this draws Ferret's thin thumb over it.
    /// The scrollbar's geometry: the list's height and top (window px), how far it scrolls, the
    /// thumb's height and its top within the list.
    fn scroll_geometry(&self, id: u64) -> Option<ScrollGeometry> {
        let p = self.pane_ref(id)?;
        let b = p.bounds.get();
        let h = b.size.height.as_f32();
        let content = if p.uniform() { p.items.len() as f32 * t::row_h() + 4. } else { p.height };
        let max = content - h;
        if max <= 0. || h <= 0. {
            return None;
        }
        let off = self.scroll_y(id).clamp(0., max);
        let thumb = (h * h / content).max(24.);
        Some(ScrollGeometry { origin_y: b.origin.y.as_f32(), h, max, thumb, top: off / max * (h - thumb) })
    }

    /// A real scrollbar, as the app's (the browser's): the thumb drags the list, a press above or
    /// below it pages. Its strip takes the press, so it no longer reaches the rows under it (it
    /// used to select them, or start a rubber band).
    fn scrollbar(&self, id: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let g = self.scroll_geometry(id)?;
        let dragging = self.thumb_drag.is_some_and(|(p, _)| p == id);
        Some(
            div()
                .id(("scrollbar", id as usize))
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(14.))
                .occlude()
                .group("scrollbar")
                .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    f.scrollbar_press(id, ev.position.y.as_f32(), cx);
                }))
                // The strip covers the window under it, so the window's own handlers don't hear
                // the mouse while it's here: the drag is followed and ended here too (and by the
                // window once the mouse leaves the strip).
                .on_mouse_move(cx.listener(|f, ev: &MouseMoveEvent, _, cx| {
                    if f.thumb_drag.is_some() {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            f.scrollbar_drag(ev.position.y.as_f32(), cx);
                        } else {
                            f.thumb_drag = None;
                            cx.notify();
                        }
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|f, _, _, cx| {
                    if f.thumb_drag.take().is_some() {
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .absolute()
                        .top(px(g.top + 3.))
                        .right(px(3.))
                        .w(px(if dragging { 8. } else { 6. }))
                        .h(px(g.thumb - 6.))
                        .rounded(px(8.))
                        .bg(if dragging { t::fg3() } else { t::line2() })
                        .group_hover("scrollbar", |s| s.bg(t::fg3())),
                )
                .into_any_element(),
        )
    }

    /// A press on the scrollbar: on the thumb it starts dragging it; above or below it, the list
    /// moves by a page that way.
    fn scrollbar_press(&mut self, id: u64, y: f32, cx: &mut Context<Self>) {
        let Some(g) = self.scroll_geometry(id) else { return };
        let at = y - g.origin_y;
        if at >= g.top && at <= g.top + g.thumb {
            self.thumb_drag = Some((id, at - g.top));
        } else {
            let page = (g.h - 40.).max(40.);
            self.scroll_pane_by(id, if at < g.top { -page } else { page });
        }
        cx.notify();
    }

    /// The thumb follows the mouse while it's held (`on_resize_move`).
    pub fn scrollbar_drag(&mut self, y: f32, cx: &mut Context<Self>) {
        let Some((id, grab)) = self.thumb_drag else { return };
        let Some(g) = self.scroll_geometry(id) else { return };
        let room = (g.h - g.thumb).max(1.);
        let top = (y - g.origin_y - grab).clamp(0., room);
        let to = top / room * g.max;
        let now = self.scroll_y(id);
        self.scroll_pane_by(id, to - now);
        cx.notify();
    }

    // ───────────────────────────── rows ─────────────────────────────

    fn size_text(&self, r: &crate::pane::Row) -> (SharedString, bool, bool) {
        if !r.e.is_dir {
            return (r.size.clone(), false, false);
        }
        if !self.settings.bool("folderSizes", true) {
            return (SharedString::default(), false, false);
        }
        match self.sizes.get(&paths::key(&r.e.path)) {
            Some(s) if s.done => (kinds::format_size(s.bytes).into(), false, s.refreshing),
            Some(s) if s.bytes > 0 => (kinds::format_size(s.bytes).into(), true, false),
            Some(_) => ("…".into(), true, false),
            None => (SharedString::default(), false, false),
        }
    }

    /// A folder size's tooltip: how many files, whether some couldn't be read, and when it was counted.
    fn size_tip(&self, r: &crate::pane::Row) -> Option<String> {
        if !r.e.is_dir || !self.settings.bool("folderSizes", true) {
            return None;
        }
        let s = self.sizes.get(&paths::key(&r.e.path))?;
        if !s.done {
            return Some("Counting…".into());
        }
        let mut parts = Vec::new();
        if s.refreshing {
            parts.push("Updating…".to_string());
        }
        if let Some(n) = s.files {
            parts.push(plural(n as usize, "file"));
        }
        if s.partial {
            parts.push("some folders inside couldn't be read".into());
        }
        if s.counted_at > 0 {
            parts.push(format!("counted {}", util::ago(s.counted_at)));
        }
        Some(parts.join(" · "))
    }

    fn row(&mut self, id: u64, ix: usize, in_use: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let renaming = self.rename.as_ref().filter(|r| r.pane == id).map(|r| (r.path.clone(), r.input.clone(), r.error));
        // The icon first (a program's own icon may need loading, which needs `self` mutable).
        let icon_el = {
            let p = self.pane_ref(id).unwrap();
            let r = &p.rows[ix];
            let on = p.marked.contains(&ix) && in_use;
            let (path, ext, kind, dir, link, inside, modified) = (r.e.path.clone(), r.e.ext.clone(), r.kind, r.e.is_dir, r.e.is_symlink, r.e.in_archive, r.e.modified);
            self.row_icon(&path, &ext, kind, dir, link, inside, modified, if on { t::on_accent() } else { kinds::color(kind) }, cx)
        };
        let p = self.pane_ref(id).unwrap();
        let r = &p.rows[ix];
        let is_sel = |i: usize| p.marked.contains(&i);
        let sel = is_sel(ix);
        // Selected rows next to each other form one block (a heading in between breaks it).
        let item = p.item_of[ix];
        let prev = item.checked_sub(1).and_then(|i| p.items.get(i)).and_then(|it| if let Item::Row(r) = it { Some(*r) } else { None });
        let next = p.items.get(item + 1).and_then(|it| if let Item::Row(r) = it { Some(*r) } else { None });
        let (join_up, join_down) = (sel && prev.is_some_and(is_sel), sel && next.is_some_and(is_sel));
        let soft = sel && !in_use;
        let on = sel && !soft;
        let cursor = p.selected == Some(ix) && !sel;
        let cut = self.cut.contains(&paths::key(&r.e.path));
        // A hidden item, as Explorer shows one: its icon ghosted and its name greyed.
        let ghost = r.e.hidden && !cut;
        let bin = p.is_bin();
        let search = p.is_search();
        let width = p.width;
        let show = |c: Column| -> bool {
            !self.settings.column_hidden(c)
                && match c {
                    Column::Type => width >= 600. && !bin,
                    Column::Modified => width >= 430.,
                    Column::Size => true,
                }
        };
        let text3 = if on { t::on_accent() } else { t::fg3() };
        let cell = |w: f32, text: SharedString, right: bool| {
            div().flex_none().w(px(w)).px(px(8.)).truncate().when(right, |d| d.text_right()).text_color(text3).child(text)
        };
        let (size, counting, refreshing) = self.size_text(r);
        let size_tip = self.size_tip(r);
        let path = r.e.path.clone();
        let is_dir = r.e.is_dir;
        let rename_here = renaming.as_ref().filter(|(p, _, _)| util::same(p, &path));
        let name_el: AnyElement = match rename_here {
            Some((_, input, err)) => div()
                .flex_1()
                .min_w_0()
                .relative()
                .child(
                    div()
                        .h(px(t::row_h() - 6.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .bg(t::raised())
                        .border_1()
                        .border_color(if err.is_some() { t::danger() } else { t::accent() })
                        .text_color(t::fg())
                        .child(input.clone()),
                )
                // Drawn over the rows below it (it used to be painted under the next row's name).
                .children(err.map(|e| {
                    gpui::deferred(
                        div()
                            .absolute()
                            .top(px(t::row_h() - 2.))
                            .left_0()
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(t::raised())
                            .border_1()
                            .border_color(t::danger())
                            .shadow(t::shadow())
                            .text_size(px(12.))
                            .text_color(t::danger())
                            .whitespace_nowrap()
                            .child(e.to_string()),
                    )
                    .with_priority(1)
                }))
                .into_any_element(),
            None => div().min_w_0().truncate().child(r.name.clone()).into_any_element(),
        };
        let date = if bin { r.date.clone() } else { r.date.clone() };
        let origin: Option<SharedString> = if bin && width >= 600. {
            Some(r.e.origin.clone().unwrap_or_default().into())
        } else if search && width >= 600. {
            Some(util::parent(&r.e.path).unwrap_or_default().into())
        } else {
            None
        };
        let w_mod = self.settings.column_width(Column::Modified);
        let w_type = self.settings.column_width(Column::Type);
        let w_size = self.settings.column_width(Column::Size);
        let _ = window;
        // To a screen reader: "name, type, size, modified date" (and where, in search results),
        // the item under the keyboard announced as focused.
        // (Only while a screen reader listens: rows are drawn often.)
        let spoken = if !window.is_a11y_active() {
            String::new()
        } else {
            let mut s = format!("{}, {}", r.name, r.ty);
            if !is_dir && !size.is_empty() {
                s.push_str(&format!(", {size}"));
            }
            if !r.date.is_empty() {
                s.push_str(&format!(", modified {}", r.date));
            }
            if search {
                s.push_str(&format!(", in {}", util::parent(&r.e.path).unwrap_or_default()));
            }
            s
        };
        let claim = in_use && p.selected == Some(ix) && self.menu.is_none();
        let count = p.rows.len();
        div()
            .id(("row", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(spoken)
            .aria_selected(sel)
            .aria_position_in_set(ix + 1)
            .aria_size_of_set(count)
            .when(claim, |d| d.aria_active_descendant())
            .on_a11y_action(gpui::AccessibleAction::Click, crate::view::on_a11y(cx, move |f, _, cx| {
                f.focus_pane_of(id, cx);
                f.select(ix, cx);
            }))
            .w_full()
            .h(px(t::row_h()))
            .flex()
            .items_center()
            .px(px(4.))
            .rounded(px(7.))
            .text_size(px(t::row_text()))
            .text_color(if on { t::on_accent() } else { t::fg2() })
            .when(join_up, |d| d.rounded_tl(px(0.)).rounded_tr(px(0.)).border_t_1().border_color(t::rgba(0xffffff, 0.14)))
            .when(join_down, |d| d.rounded_bl(px(0.)).rounded_br(px(0.)))
            .when(on, |d| d.bg(t::accent()))
            .when(soft, |d| d.bg(t::accent_soft()))
            .when(!sel, |d| d.hover(|s| s.bg(t::hover()).text_color(t::fg())))
            .when(cursor, |d| d.border_1().border_color(if in_use && self.focus.is_focused(window) { t::accent_text() } else { t::accent_line() }))
            // Cut items fade until they're pasted.
            .when(cut, |d| d.opacity(0.45))
            .when(is_dir, |d| {
                let target = path.clone();
                crate::view::drag_target(d, target.clone(), 3, cx)
                    .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()).border_1().border_color(t::accent()))
                    .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| {
                        cx.stop_propagation();
                        f.drop_paths(paths.paths().to_vec(), target.clone(), window, cx);
                    }))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                f.row_press(id, ix, ev, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                f.row_menu(id, ix, ev.position, window, cx);
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                f.middle_open(id, ix, ev.position, cx);
            }))
            .on_mouse_move(cx.listener(Self::press_move))
            .on_click(cx.listener(move |f, ev: &ClickEvent, window, cx| f.row_click(id, ix, ev, window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(160.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .text_color(if on { t::on_accent() } else if ghost { t::fg3() } else { t::fg() })
                    .child(div().flex_none().flex().items_center().when(ghost, |d| d.opacity(0.45)).child(icon_el))
                    .child(name_el),
            )
            .when_some(origin, |d, o| d.child(div().flex_none().w(px(200.)).px(px(8.)).truncate().text_color(text3).child(o)))
            .when(show(Column::Modified), |d| d.child(cell(w_mod, date, false)))
            .when(show(Column::Type), |d| d.child(cell(w_type, r.ty.clone(), false)))
            .when(show(Column::Size), |d| {
                d.child(
                    div()
                        .id(("size", ix))
                        .flex_none()
                        .w(px(w_size))
                        .px(px(8.))
                        .flex()
                        .justify_end()
                        .items_center()
                        .gap(px(5.))
                        .text_color(text3)
                        .when(counting, |d| d.opacity(0.5))
                        .when(refreshing, |d| d.child(ui::spinner(11., text3)))
                        .when_some(size_tip, |d, tip| d.tooltip(ui::tip(tip)))
                        .child(div().truncate().child(size)),
                )
            })
    }

    /// A row's icon: the kind's Lucide icon, or a program's own icon.
    #[allow(clippy::too_many_arguments)]
    fn row_icon(&mut self, path: &str, ext: &str, kind: Kind, is_dir: bool, link: bool, inside: bool, modified: Option<i64>, color: gpui::Hsla, cx: &mut Context<Self>) -> AnyElement {
        if matches!(ext, "exe" | "lnk" | "ico" | "msi" | "appx") && !inside && !paths::is_remote(path) {
            if let Some(image) = self.thumb(path, 32, true, modified, cx) {
                return img(image).size(px(16.)).flex_none().into_any_element();
            }
        }
        let name = if is_dir && link { "folder-symlink" } else { kinds::icon(kind) };
        icon(name, 16., color).into_any_element()
    }

    fn cell(&mut self, id: u64, ix: usize, in_use: bool, a11y: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let renaming = self.rename.as_ref().filter(|r| r.pane == id).map(|r| (r.path.clone(), r.input.clone(), r.error));
        let (path, kind, ext, modified, is_dir, name, hidden, in_archive, tip) = {
            let p = self.pane_ref(id).unwrap();
            let r = &p.rows[ix];
            let mut tip = format!("{}\n{}", r.ty, r.date);
            if !r.e.is_dir {
                tip = format!("{}\n{}\n{}", r.ty, r.size, r.date);
            }
            if p.is_search() {
                tip.push_str(&format!("\nIn {}", util::parent(&r.e.path).unwrap_or_default()));
            }
            if let Some(o) = &r.e.origin {
                tip.push_str(&format!("\nFrom {o}"));
            }
            (r.e.path.clone(), r.kind, r.e.ext.clone(), r.e.modified, r.e.is_dir, r.name.clone(), r.e.hidden, r.e.in_archive, tip)
        };
        let p = self.pane_ref(id).unwrap();
        let sel = p.marked.contains(&ix);
        let cursor = p.selected == Some(ix) && !sel;
        let claim = in_use && p.selected == Some(ix) && self.menu.is_none();
        let count = p.rows.len();
        let size = p.view.icon_size as f32;
        let cut = self.cut.contains(&paths::key(&path));
        // As the app (THUMB_KINDS): audio shows its cover art, spreadsheets their first sheet
        // (Windows only gives real thumbnails: no cover, the type's icon).
        let wants_thumb = matches!(kind, Kind::Image | Kind::Video | Kind::Pdf | Kind::Doc | Kind::Sheet | Kind::Font | Kind::Audio)
            || matches!(ext.as_str(), "exe" | "lnk" | "ico")
            || is_dir;
        // Windows has no thumbnails for SVG: GPUI draws the file itself.
        let svg = ext == "svg" && !in_archive && !paths::is_remote(&path);
        let bucket = [64u32, 96, 128, 192, 256, 512].into_iter().find(|b| *b as f32 >= size * 1.25).unwrap_or(512);
        // Server files: pictures only (copied to make one); others show their type's icon.
        // In an archive, pictures and videos (extracted to the cache first, as the app).
        let mut thumb = if !svg && wants_thumb && (!paths::is_remote(&path) || kind == Kind::Image) && (!in_archive || matches!(kind, Kind::Image | Kind::Video)) && !is_dir {
            self.thumb(&path, bucket, false, modified, cx)
        } else {
            None
        };
        // Very wide or tall pictures (beyond 5:1, a toolbar's screenshot, a panorama) show as a
        // strip: fetched sharp enough for its short side (about 30 thumbnail pixels), never
        // thinner than 12 px, cropped from its left or top end (FileView.tsx `checkShape`).
        let ratio = thumb.as_ref().and_then(|i| png_size(i.bytes())).map(|(w, h)| w as f32 / h.max(1) as f32).filter(|r| *r > 5. || *r < 0.2);
        if let Some(r) = ratio.filter(|_| size >= 48.) {
            let long = 30. * r.max(1. / r);
            let want = [512u32, 768, 1024].into_iter().find(|b| *b as f32 >= long).unwrap_or(1024);
            if want > bucket {
                if let Some(sharp) = self.thumb(&path, want, false, modified, cx) {
                    thumb = Some(sharp);
                }
            }
        }
        let has_thumb = thumb.is_some() || svg;
        let graphic: AnyElement = match thumb {
            Some(image) if ratio.is_some() => {
                let r = ratio.unwrap();
                // The box: at most size × size, at least 12 px across; the picture covers it.
                let (bw, bh) = if r > 1. { (size, (size / r).max(12.)) } else { ((size * r).max(12.), size) };
                let (iw, ih) = if r > 1. { (bh * r, bh) } else { (bw, bw / r) };
                div()
                    .relative()
                    .w(px(bw))
                    .h(px(bh))
                    .overflow_hidden()
                    .rounded(px(4.))
                    .child(img(image).absolute().top_0().left_0().w(px(iw)).h(px(ih)))
                    .into_any_element()
            }
            Some(image) => img(image).max_w(px(size)).max_h(px(size)).object_fit(ObjectFit::Contain).rounded(px(4.)).into_any_element(),
            None if svg => img(std::path::PathBuf::from(&path)).size(px(size * 0.86)).object_fit(ObjectFit::Contain).into_any_element(),
            None => icon(if is_dir { "folder-fill" } else { kinds::icon(kind) }, size * 0.62, kinds::color(kind)).into_any_element(),
        };
        let badge = (!is_dir && !has_thumb && !ext.is_empty()).then(|| {
            div()
                .absolute()
                .bottom(relative(0.08))
                .right(relative(0.14))
                .px(px(5.))
                .py(px(1.))
                .rounded(px(4.))
                .bg(t::raised())
                .border_1()
                .border_color(t::line2())
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(t::fg2())
                .child(ext.to_uppercase())
        });
        let play = (kind == Kind::Video && has_thumb).then(|| {
            div()
                .absolute()
                .size(px(30.))
                .rounded(px(15.))
                .bg(t::rgba(0, 0.55))
                .flex()
                .items_center()
                .justify_center()
                .child(icon("play", 14., t::rgba(0xffffff, 1.)))
        });
        let rename_here = renaming.filter(|(p, _, _)| util::same(p, &path));
        let target = path.clone();
        let spoken = if a11y { format!("{name}, {}", tip.replace('\n', ", ")) } else { String::new() };
        div()
            .id(("cell", ix))
            .role(gpui::Role::ListBoxOption)
            .aria_label(spoken)
            .aria_selected(sel)
            .aria_position_in_set(ix + 1)
            .aria_size_of_set(count)
            .when(claim, |d| d.aria_active_descendant())
            .on_a11y_action(gpui::AccessibleAction::Click, crate::view::on_a11y(cx, move |f, _, cx| {
                f.focus_pane_of(id, cx);
                f.select(ix, cx);
            }))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .pt(px(6.))
            .px(px(4.))
            .pb(px(4.))
            .rounded(px(10.))
            .border_1()
            .border_color(t::rgba(0, 0.))
            .text_color(t::fg2())
            .when(sel, |d| d.bg(if in_use { t::accent_soft() } else { t::accent_softer() }).border_color(if in_use { t::accent() } else { t::accent_line() }).text_color(t::fg()))
            .when(!sel, |d| d.hover(|s| s.bg(t::hover()).text_color(t::fg())))
            .when(cursor, |d| d.border_color(t::accent_line()))
            .when(cut, |d| d.opacity(0.45))
            .tooltip(ui::tip(tip))
            .when(is_dir, |d| {
                crate::view::drag_target(d, target.clone(), 3, cx)
                    .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()).border_color(t::accent()))
                    .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| {
                        cx.stop_propagation();
                        f.drop_paths(paths.paths().to_vec(), target.clone(), window, cx);
                    }))
            })
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                f.row_press(id, ix, ev, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                f.row_menu(id, ix, ev.position, window, cx);
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                f.middle_open(id, ix, ev.position, cx);
            }))
            .on_mouse_move(cx.listener(Self::press_move))
            .on_click(cx.listener(move |f, ev: &ClickEvent, window, cx| f.row_click(id, ix, ev, window, cx)))
            .child(
                div()
                    .w_full()
                    .h(px(size))
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    // A hidden item, as Explorer shows one: ghosted, its name greyed.
                    .when(hidden && !cut, |d| d.opacity(0.45))
                    .child(graphic)
                    .children(badge)
                    .children(play),
            )
            .child(match rename_here {
                // Wraps over as many lines as the name needs, drawn over the next row.
                Some((_, input, err)) => gpui::deferred(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .w_full()
                                .px(px(4.))
                                .py(px(2.))
                                .rounded(px(5.))
                                .bg(t::raised())
                                .border_1()
                                .border_color(if err.is_some() { t::danger() } else { t::accent() })
                                .shadow(t::shadow())
                                .text_size(px(12.))
                                .child(input),
                        )
                        // Why the name can't be used (the grid only reddened the box before).
                        .children(err.map(|e| {
                            div()
                                .w_full()
                                .px(px(6.))
                                .py(px(3.))
                                .rounded(px(5.))
                                .bg(t::raised())
                                .border_1()
                                .border_color(t::danger())
                                .shadow(t::shadow())
                                .text_size(px(11.5))
                                .text_color(t::danger())
                                .child(e.to_string())
                        })),
                )
                .with_priority(1)
                .into_any_element(),
                None => div()
                    .w_full()
                    .text_size(px(12.))
                    .text_center()
                    .line_height(relative(1.3))
                    .line_clamp(2)
                    .overflow_hidden()
                    .when(hidden && !cut && !sel, |d| d.text_color(t::fg3()))
                    .child(name)
                    .into_any_element(),
            })
    }

    /// A thumbnail (or a program's icon) for `path` at about `size` px: made in the background,
    /// a few at a time, and kept (the key carries the file's date, so an edited file gets a new one).
    pub fn thumb(&mut self, path: &str, size: u32, icon_only: bool, modified: Option<i64>, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        let key = format!("{}|{size}|{}|{}", paths::key(path), modified.unwrap_or(0), icon_only as u8);
        match self.thumbs.get(&key) {
            Some(Thumb::Ready(i)) => return Some(i.clone()),
            Some(_) => return None,
            None => {}
        }
        self.thumbs.insert(key.clone(), Thumb::Loading);
        self.thumb_queue.push((key, path.to_string(), size | if icon_only { 1 << 31 } else { 0 }));
        self.pump_thumbs(cx);
        None
    }

    fn pump_thumbs(&mut self, cx: &mut Context<Self>) {
        // Eight at a time: Windows' thumbnail cache serves them in parallel.
        while self.thumbs_running < 8 {
            // Newest requests first: what's on screen now.
            let Some((key, path, size)) = self.thumb_queue.pop() else { return };
            self.thumbs_running += 1;
            let icon_only = size & (1 << 31) != 0;
            let size = size & !(1 << 31);
            let task = cx.background_executor().spawn(async move { thumb_png(&path, size, icon_only) });
            cx.spawn(async move |this, cx| {
                let png = task.await;
                this.update(cx, |f, cx| {
                    f.thumbs_running -= 1;
                    f.thumbs.insert(
                        key,
                        match png {
                            Ok(bytes) => Thumb::Ready(Arc::new(Image::from_bytes(ImageFormat::Png, bytes))),
                            Err(_) => Thumb::None,
                        },
                    );
                    f.pump_thumbs(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    // ───────────────────────────── mouse ─────────────────────────────

    fn row_press(&mut self, id: u64, ix: usize, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        // In split view a press in the other pane makes it the one in use first (the row stops
        // the press from reaching the pane): otherwise the click selected the same row in the
        // pane that was in use, and the next copy took the wrong item.
        self.focus_pane_of(id, cx);
        self.press = Some(Press { pane: id, row: ix, at: ev.position });
        // Explorer selects on press (so a drag takes what you pressed on), except a press on a
        // selected item, which may start dragging the whole selection.
        let p = self.pane();
        if p.id == id && !p.marked.contains(&ix) && !ev.modifiers.control && !ev.modifiers.shift {
            self.select(ix, cx);
        }
    }

    pub(crate) fn press_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(press) = &self.press else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.press = None;
        } else if (ev.position.x - press.at.x).abs().as_f32().max((ev.position.y - press.at.y).abs().as_f32()) > 6. {
            let (id, row) = (press.pane, press.row);
            self.press = None;
            self.start_drag(id, row, window, cx);
        }
    }

    fn row_click(&mut self, id: u64, ix: usize, ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = ev.modifiers();
        let single_open = !self.settings.bool("doubleClickOpen", true);
        if (ev.click_count() >= 2 || single_open) && !m.control && !m.shift {
            self.last_click = None;
            if self.pane().id == id {
                self.open_row(ix, false, cx);
            }
            return;
        }
        // A second, slow click on the name of the only selected item renames it.
        let p = self.pane();
        let only = p.marked.len() == 1 && p.marked.contains(&ix);
        if let Some((pid, row, at)) = self.last_click {
            if only && pid == id && row == ix && !m.control && !m.shift && at.elapsed() > Duration::from_millis(500) && at.elapsed() < Duration::from_secs(4) {
                self.last_click = None;
                self.start_rename(cx);
                return;
            }
        }
        self.last_click = Some((id, ix, Instant::now()));
        if m.control || m.shift {
            self.select_with(ix, m.shift, m.control, cx);
        } else if !only {
            // A click on one of several selected items narrows the selection to it.
            self.select(ix, cx);
        }
        let _ = window;
    }

    /// Middle-click: a folder (or archive) opens in a new tab; on a file it starts autoscroll.
    fn middle_open(&mut self, id: u64, ix: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(r) = self.pane_ref(id).and_then(|p| p.rows.get(ix)) else { return };
        if r.e.is_dir || ferret_core::archive::archive_kind(&r.e.path).is_some() {
            let path = r.e.path.clone();
            let at = self.tabs.len();
            self.push_tab(path, None, Some(at), cx);
            cx.notify();
        } else {
            self.start_autoscroll(id, at, cx);
        }
    }

    /// Autoscroll, like Explorer and browsers (lib/autoscroll.ts): press the wheel over the list
    /// and move away from that point to scroll, faster the further away. Hold and move (it stops
    /// when the wheel is let go), or click once (it follows the mouse until the next click, a
    /// wheel turn or Esc).
    pub fn start_autoscroll(&mut self, id: u64, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.autoscroll.is_some() {
            return;
        }
        self.autoscroll = Some(crate::view::AutoScroll { pane: id, origin: at, at, began: std::time::Instant::now(), sticky: false });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut last = std::time::Instant::now();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                let go = this
                    .update(cx, |f, cx| {
                        let Some(a) = &f.autoscroll else { return false };
                        let dt = last.elapsed().as_secs_f32().min(0.05);
                        last = std::time::Instant::now();
                        let v = autoscroll_speed((a.at.y - a.origin.y).as_f32());
                        if v != 0. {
                            let id = a.pane;
                            f.scroll_pane_by(id, v * dt);
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn stop_autoscroll(&mut self, cx: &mut Context<Self>) {
        if self.autoscroll.take().is_some() {
            cx.notify();
        }
    }

    /// The wheel let go: a quick click without moving keeps following the mouse; else it stops.
    pub fn autoscroll_up(&mut self, cx: &mut Context<Self>) {
        let Some(a) = self.autoscroll.as_mut() else { return };
        if a.sticky {
            return;
        }
        let moved = (a.at.x - a.origin.x).as_f32().hypot((a.at.y - a.origin.y).as_f32());
        if a.began.elapsed() < std::time::Duration::from_millis(350) && moved < AUTOSCROLL_DEAD {
            a.sticky = true;
        } else {
            self.stop_autoscroll(cx);
        }
    }

    /// The marker where autoscroll started: a dot with arrows up and down.
    pub fn autoscroll_mark(&self) -> Option<AnyElement> {
        let a = self.autoscroll.as_ref()?;
        Some(
            div()
                .absolute()
                .left(a.origin.x - px(16.))
                .top(a.origin.y - px(16.))
                .size(px(32.))
                .rounded_full()
                .bg(t::raised())
                .border_1()
                .border_color(t::line2())
                .shadow(t::shadow())
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(icon("chevron-up", 11., t::fg2()))
                .child(div().size(px(4.)).rounded_full().bg(t::fg2()))
                .child(icon("chevron-down", 11., t::fg2()))
                .into_any_element(),
        )
    }

    /// The cursor while autoscrolling: which way it's going, or both ways near the start.
    pub fn autoscroll_cursor(&self) -> Option<gpui::CursorStyle> {
        let a = self.autoscroll.as_ref()?;
        let v = autoscroll_speed((a.at.y - a.origin.y).as_f32());
        Some(if v < 0. {
            gpui::CursorStyle::ResizeUp
        } else if v > 0. {
            gpui::CursorStyle::ResizeDown
        } else {
            gpui::CursorStyle::ResizeUpDown
        })
    }

    /// A press on empty space: clears the selection (unless Ctrl) and starts the rubber band.
    fn empty_press(&mut self, id: u64, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.commit_rename(cx);
        let base = if ev.modifiers.control { self.pane().marked.clone() } else { Default::default() };
        if !ev.modifiers.control {
            self.clear_selection(cx);
        }
        let y = self.scroll_y(id);
        let at = point(ev.position.x, ev.position.y + px(y));
        self.band = Some(Band { pane: id, from: at, to: at, base });
    }

    pub fn band_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.band.is_none() {
            return;
        }
        if ev.pressed_button != Some(MouseButton::Left) {
            self.band = None;
            cx.notify();
            return;
        }
        self.band_at = ev.position;
        self.band_update(cx);
        // Near the list's top or bottom edge, it scrolls on its own (also while the mouse rests).
        if !self.band_scrolling && self.band_edge() != 0. {
            self.band_scrolling = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                    let go = this
                        .update(cx, |f, cx| {
                            let speed = f.band_edge();
                            let Some(id) = f.band.as_ref().map(|b| b.pane).filter(|_| speed != 0.) else {
                                f.band_scrolling = false;
                                return false;
                            };
                            f.scroll_pane_by(id, speed);
                            f.band_update(cx);
                            true
                        })
                        .unwrap_or(false);
                    if !go {
                        break;
                    }
                }
            })
            .detach();
        }
    }

    /// How fast the rubber band scrolls the list (px a frame; up is negative): faster nearer and
    /// past the edge, 0 away from it.
    fn band_edge(&self) -> f32 {
        let Some(b) = &self.band else { return 0. };
        let Some(p) = self.pane_ref(b.pane) else { return 0. };
        let r = p.bounds.get();
        let (top, bottom, y) = (r.origin.y.as_f32(), (r.origin.y + r.size.height).as_f32(), self.band_at.y.as_f32());
        const ZONE: f32 = 36.;
        if y < top + ZONE {
            -((top + ZONE - y) / ZONE * 14.).clamp(2., 28.)
        } else if y > bottom - ZONE {
            ((y - (bottom - ZONE)) / ZONE * 14.).clamp(2., 28.)
        } else {
            0.
        }
    }

    /// Scrolls a pane's list by `dy` px (down is positive), within its content.
    pub fn scroll_pane_by(&mut self, id: u64, dy: f32) {
        let Some(p) = self.pane_ref(id) else { return };
        if p.uniform() {
            let h = p.scroll.0.borrow().base_handle.clone();
            let max = h.max_offset().y.as_f32();
            let y = (-h.offset().y.as_f32() + dy).clamp(0., max.max(0.));
            h.set_offset(point(px(0.), px(-y)));
        } else {
            p.list.scroll_by(px(dy));
        }
    }

    /// The rubber band's selection, from where it started to where the mouse is now.
    fn band_update(&mut self, cx: &mut Context<Self>) {
        let Some(band) = &self.band else { return };
        let id = band.pane;
        let y = self.scroll_y(id);
        let to = point(self.band_at.x, self.band_at.y + px(y));
        let from = band.from;
        let base = band.base.clone();
        let Some(p) = self.pane_ref(id) else { return };
        let o = p.bounds.get().origin;
        let (x0, x1) = ((from.x.min(to.x) - o.x).as_f32(), (from.x.max(to.x) - o.x).as_f32());
        let (y0, y1) = ((from.y.min(to.y) - o.y).as_f32() - 4., (from.y.max(to.y) - o.y).as_f32() - 4.);
        let ih = t::row_h();
        let mut hits = base;
        let slot = ((p.width - 18.) / p.cols.max(1) as f32).max(1.);
        for (i, it) in p.items.iter().enumerate() {
            let top = if p.uniform() { i as f32 * ih } else { p.tops[i] };
            let h = match it {
                Item::Header(_) => HEADER_H,
                Item::Row(_) => ih,
                Item::Cells(_) => p.view.icon_size as f32 + 58.,
            };
            if top + h < y0 || top > y1 {
                continue;
            }
            match it {
                Item::Row(r) => {
                    hits.insert(*r);
                }
                Item::Cells(range) => {
                    for (k, r) in range.clone().enumerate() {
                        let cx0 = 6. + k as f32 * slot;
                        if cx0 + slot > x0 && cx0 < x1 {
                            hits.insert(r);
                        }
                    }
                }
                Item::Header(_) => {}
            }
        }
        let p = self.pane_by(id).unwrap();
        p.marked = hits;
        if let Some(b) = self.band.as_mut() {
            b.to = to;
        }
        cx.notify();
    }

    // ───────────────────────────── dragging out ─────────────────────────────

    /// A row pressed and moved: a real shell drag of the selection (Explorer, the desktop, any app
    /// that takes files, and folders here), with its thumbnail as the drag image.
    pub fn start_drag(&mut self, id: u64, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = crate::view::hwnd(window).and_then(std::num::NonZeroIsize::new) else { return };
        let hwnd = crate::view::Hwnd(hwnd);
        let out = self.bench.as_ref().map(|b| b.out.clone());
        let Some(p) = self.pane_by(id) else { return };
        let Some(r) = p.rows.get(ix) else { return };
        let first = r.e.path.clone();
        // Dragging a selected item takes the whole selection with it.
        if !p.marked.contains(&ix) {
            p.marked = [ix].into();
            p.anchor = ix;
        }
        p.selected = Some(ix);
        let items: Vec<(String, bool)> = p.marked.iter().filter_map(|&i| p.rows.get(i)).map(|r| (r.e.path.clone(), r.e.in_archive || paths::is_remote(&r.e.path))).collect();
        cx.notify();
        self.drag_out(hwnd, first, items, out, cx);
    }

    /// Ctrl+K's results dragged out: into a folder here, Explorer, or any app that takes files.
    pub fn drag_paths(&mut self, list: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = crate::view::hwnd(window).and_then(std::num::NonZeroIsize::new) else { return };
        let Some(first) = list.first().cloned() else { return };
        let items = list
            .into_iter()
            .map(|p| {
                let virt = paths::is_remote(&p) || (ferret_core::archive::split_virtual(&p).is_some() && !std::path::Path::new(&p).exists());
                (p, virt)
            })
            .collect();
        let out = self.bench.as_ref().map(|b| b.out.clone());
        self.drag_out(crate::view::Hwnd(hwnd), first, items, out, cx);
    }

    /// A shell drag of `items` (path, and whether it must be made a real file first: archive
    /// entries, server files), with `first`'s thumbnail as the drag image.
    fn drag_out(&mut self, hwnd: crate::view::Hwnd, first: String, items: Vec<(String, bool)>, out: Option<std::path::PathBuf>, cx: &mut Context<Self>) {
        // DoDragDrop runs its own message loop until the drop. It starts from a task, outside
        // GPUI's event handling, so GPUI keeps drawing and handling messages meanwhile.
        cx.spawn(async move |this, cx| {
            this.update(cx, |f, _| f.own_drag = true).ok();
            // Archive entries and server files become real files first.
            let mut real = Vec::new();
            for (path, virt) in items {
                if !virt {
                    real.push(std::path::PathBuf::from(path));
                } else if paths::is_remote(&path) {
                    if let Ok(p) = ferret_core::rt::block_on(ferret_core::remote::remote_materialize(path)) {
                        real.push(p.into());
                    }
                } else if let Ok(p) = ferret_core::rt::block_on(ferret_core::archive::archive_materialize(path, None)) {
                    real.push(p.into());
                }
            }
            if real.is_empty() {
                this.update(cx, |f, _| f.own_drag = false).ok();
                return;
            }
            let image = ferret_core::shell::thumbnail_png(&first, 96, false)
                .or_else(|_| ferret_core::shell::thumbnail_png(&first, 64, true))
                .unwrap_or_default();
            // Let go while the files were made ready: no drag (see `left_button_held`).
            if !left_button_held() {
                this.update(cx, |f, _| f.own_drag = false).ok();
                return;
            }
            let done = out.clone();
            // A result dragged out of Ctrl+K and dropped closes the palette, as the app's;
            // a cancelled drag shows it again.
            let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let dropped_in = dropped.clone();
            let started = drag::start_drag(
                &hwnd,
                drag::DragItem::Files(real),
                drag::Image::Raw(image),
                move |result, at| {
                    if matches!(result, drag::DragResult::Dropped) {
                        dropped_in.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    if let Some(out) = &done {
                        let what = match result {
                            drag::DragResult::Dropped => "dropped",
                            drag::DragResult::Cancel => "cancelled",
                        };
                        crate::bench::write_to(out, "drag_result", format!("{what} at {},{}", at.x, at.y));
                    }
                },
                drag::Options::default(),
            );
            if let (Some(out), Err(e)) = (&out, started) {
                crate::bench::write_to(out, "drag_error", e);
            }
            this.update(cx, |f, cx| {
                f.own_drag = false;
                f.drag_hint = None;
                if dropped.load(std::sync::atomic::Ordering::Relaxed) && f.palette.is_some() {
                    f.palette = None;
                    f.pending_focus = Some(f.focus.clone());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Pictures on a server up to this size get thumbnails (from a cached copy); other server files
/// show their type's icon.
const REMOTE_THUMB_MAX: u64 = 16 << 20;

/// A thumbnail as the app's `thumb://` protocol makes it (protocol.rs): server pictures from a
/// local copy, and every thumbnail kept on disk, keyed by the file's path, size, date and length
/// (the prototype's own folder, `%LOCALAPPDATA%\ferret-gpui\thumbs`), so it comes back at once
/// the next time the folder opens.
fn thumb_png(path: &str, size: u32, icon_only: bool) -> Result<Vec<u8>, String> {
    let mut path = path.to_string();
    // A picture or video in an archive: from its copy in the extraction cache (useThumbPath).
    if ferret_core::archive::split_virtual(&path).is_some_and(|(_, inner)| !inner.is_empty()) {
        let kind = kinds::kind_of(false, &path.rsplit('.').next().unwrap_or("").to_lowercase());
        if !matches!(kind, Kind::Image | Kind::Video) || icon_only {
            return Err("no thumbnail".into());
        }
        path = ferret_core::rt::block_on(ferret_core::archive::archive_materialize(path, None))?;
    } else if paths::is_remote(&path) {
        let picture = matches!(kinds::kind_of(false, &path.rsplit('.').next().unwrap_or("").to_lowercase()), Kind::Image);
        if !picture || icon_only {
            return Err("no thumbnail".into());
        }
        path = ferret_core::remote::block(ferret_core::remote::fs::materialize(&path, REMOTE_THUMB_MAX))?;
    }
    let io = std::path::PathBuf::from(paths::for_io(&path));
    let meta = std::fs::metadata(&io).ok();
    let stamp = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let key = format!("{}|{}|{}|{}|{}", paths::key(&path), size, icon_only, stamp, meta.map(|m| m.len()).unwrap_or(0));
    let cache = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("ferret-gpui").join("thumbs")).and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        // FNV-1a: stable across runs.
        let h = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
        Some(d.join(format!("{h:016x}.png")))
    });
    if let Some(bytes) = cache.as_ref().and_then(|c| std::fs::read(c).ok()) {
        return Ok(bytes);
    }
    let bytes = ferret_core::shell::thumbnail_png(&path, size, icon_only)?;
    if let Some(c) = &cache {
        let _ = std::fs::write(c, &bytes);
    }
    Ok(bytes)
}

/// Within this distance of where autoscroll started nothing scrolls.
const AUTOSCROLL_DEAD: f32 = 12.;

/// Autoscroll's speed (px a second) for a distance from where it started: slow near it, faster
/// further away, at most 6000.
fn autoscroll_speed(d: f32) -> f32 {
    let a = d.abs() - AUTOSCROLL_DEAD;
    if a <= 0. { 0. } else { d.signum() * (0.9 * a.powf(1.6)).min(6000.) }
}

pub fn plural(n: usize, what: &str) -> String {
    let word = if n == 1 {
        what.to_string()
    } else if what.ends_with("ch") || what.ends_with('s') {
        format!("{what}es")
    } else {
        format!("{what}s")
    };
    format!("{} {word}", kinds::group_digits(n))
}

/// A folder that couldn't be listed, said plainly.
fn explain(err: &str) -> String {
    if let Some(why) = err.strip_prefix(ferret_core::network::LOGIN_PREFIX) {
        return format!("Not connected: this network folder wants a user name and password{}", if why.is_empty() { String::new() } else { format!(" ({why})") });
    }
    if let Some(rest) = err.strip_prefix("denied:") {
        let _ = rest;
        return "Windows doesn't let you open this folder".into();
    }
    if err.contains("not connected") || err.contains("Not connected") {
        return err.to_string();
    }
    format!("Can't open this folder: {}", crate::ops::plain_error(err))
}

/// A PNG's width and height, from its header.
pub(crate) fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[1..4] != b"PNG" || &b[12..16] != b"IHDR" {
        return None;
    }
    Some((u32::from_be_bytes(b[16..20].try_into().ok()?), u32::from_be_bytes(b[20..24].try_into().ok()?)))
}
/// The left mouse button is down right now (Windows' own state, not GPUI's last event). A drag
/// starts only after its files and picture are ready, off the UI thread; if the button was let
/// go meanwhile, starting it anyway made a stray drag that went on with the button up and
/// dropped wherever the pointer went next (real drag test, 2026-11-11).
pub fn left_button_held() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
    // GetAsyncKeyState reads the physical buttons: with them swapped (left-handed), the primary
    // button is the right one.
    let primary = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { 0x02 } else { 0x01 };
    unsafe { GetAsyncKeyState(primary) < 0 }
}
