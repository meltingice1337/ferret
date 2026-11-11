//! The window's chrome (TitleBar.tsx, Toolbar.tsx, StatusBar.tsx): the tab strip with the window
//! buttons, the toolbar (history, breadcrumbs and their subfolder menus, path editing with
//! suggestions, the filter), and the status bar (counts, sort, group, view switches).

use std::time::Duration;

use gpui::{
    AccessibleAction, AnyElement, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, Pixels, Point, Role, SharedString,
    Stateful, Window, WindowControlArea, div, prelude::*, px, relative,
};

use crate::files::plural;
use crate::input::TextInput;
use crate::kinds;
use crate::menu::{MenuItem, heading, item, sep};
use crate::sortgroup::{GroupBy, Mode, SortBy, SortDir};
use crate::theme as t;
use crate::ui::{self, hover_icon, icon};
use crate::util::{self, same};
use crate::view::{Ferret, PathEdit, action};

/// A tab being dragged to a new place in the strip.
#[derive(Clone)]
pub struct TabDrag {
    pub ix: usize,
    pub label: SharedString,
}

impl Render for TabDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(11.))
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(7.))
            .rounded(px(9.))
            .bg(t::raised())
            .border_1()
            .border_color(t::accent_line())
            .shadow(t::shadow())
            .font_family(t::FONT)
            .text_size(px(13.))
            .text_color(t::fg())
            .child(icon("folder", 14., t::folder()))
            .child(self.label.clone())
    }
}

impl Ferret {
    // ───────────────────────────── title bar and tabs ─────────────────────────────

    pub fn titlebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = window.viewport_size().width.as_f32() < 560.;
        let ring = match self.ring_area(window) {
            crate::view::Area::Tabs(i) => Some(i),
            _ => None,
        };
        // As the app: the "all tabs" button shows once the strip can't hold every tab.
        let (widths, overflow) = self.tab_widths(narrow, window);
        let tabs: Vec<AnyElement> = self.tabs.iter().enumerate().map(|(i, tab)| self.tab_view(i, tab, widths[i], ring == Some(i), cx)).collect();
        // The tab in use (or the keyboard's) is scrolled into view when it changes.
        let shown = (ring.unwrap_or(self.active), self.tabs.len());
        if self.tab_scroll_seen.get() != shown {
            self.tab_scroll_seen.set(shown);
            self.tab_scroll.scroll_to_item(shown.0);
        }
        // The tab bar's width, for the next frame's tab widths (redrawn when it changes).
        let bar_w = self.tab_bar_w.clone();
        let me = cx.entity().downgrade();
        let measure = gpui::canvas(
            move |b, window, _| {
                let w = b.size.width.as_f32();
                if (bar_w.get() - w).abs() > 0.5 {
                    bar_w.set(w);
                    window.on_next_frame(move |_, cx| {
                        let _ = me.update(cx, |_, cx| cx.notify());
                    });
                }
            },
            |_, _, _, _| {},
        );
        div()
            .h(px(40.))
            .flex_none()
            .flex()
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .items_end()
                    .min_w_0()
                    .pl(px(8.))
                    .pt(px(6.))
                    .child(measure.absolute().top_0().left_0().size_full())
                    .child({
                        // Tabs hidden past an edge: the strip fades out there (the app's mask-image).
                        // GPUI can't mask, so a gradient into the panel's own color lies over it.
                        let strip = self.tab_scroll.bounds();
                        let off = -self.tab_scroll.offset().x.as_f32();
                        let max = self.tab_scroll.max_offset().x.as_f32();
                        let vw = window.viewport_size();
                        let seen = |x: Pixels| {
                            t::panel_seen((x / vw.width).clamp(0., 1.), ((strip.origin.y + strip.size.height / 2.) / vw.height).clamp(0., 1.))
                        };
                        let fade = |angle: f32, c: gpui::Hsla| {
                            div().absolute().top_0().bottom_0().w(px(28.)).bg(gpui::linear_gradient(
                                angle,
                                gpui::linear_color_stop(gpui::Hsla { a: 0., ..c }, 0.),
                                gpui::linear_color_stop(c, 1.),
                            ))
                        };
                        div()
                            .relative()
                            .min_w_0()
                            .flex()
                            .child(
                                div()
                                    .id("tab-strip")
                                    .role(Role::TabList)
                                    .aria_label("Tabs")
                                    .track_scroll(&self.tab_scroll)
                                    .flex()
                                    .items_end()
                                    .gap(px(2.))
                                    .min_w_0()
                                    .overflow_x_scroll()
                                    .children(tabs),
                            )
                            .when(off > 1., |d| d.child(fade(270., seen(strip.origin.x)).left_0()))
                            .when(max > 1. && off < max - 1., |d| d.child(fade(90., seen(strip.origin.x + strip.size.width)).right_0()))
                    })
                    .child(
                        div()
                            .id("tab-new")
                            .role(Role::Button)
                            .aria_label("New tab")
                            .aria_keyshortcuts("Ctrl+T")
                            .group("tabnew")
                            .self_center()
                            .mt(px(6.))
                            .ml(px(4.))
                            .p(px(6.))
                            .rounded(px(7.))
                            .hover(|s| s.bg(t::hover()))
                            .tooltip(ui::tip("New tab (Ctrl+T)"))
                            .on_click(cx.listener(|f, _, _, cx| {
                                let home = f.places.home.clone();
                                f.new_tab(home, cx);
                            }))
                            .child(hover_icon("plus", 16., t::fg3(), t::fg(), "tabnew")),
                    )
                    // The app's `.tab-list` (TitleBar.tsx): after the new-tab button, "⌄ N", a menu
                    // of every tab opening under it.
                    .when(overflow, |d| {
                        let at = self.tab_list_at.clone();
                        d.child(
                            div()
                                .id("tab-list")
                                .role(Role::Button)
                                .aria_label(format!("All tabs ({})", self.tabs.len()))
                                .group("tablist")
                                .relative()
                                .flex_none()
                                .self_center()
                                .mt(px(6.))
                                .ml(px(4.))
                                .p(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .rounded(px(7.))
                                .text_color(t::fg3())
                                .text_size(px(11.))
                                .hover(|s| s.bg(t::hover()).text_color(t::fg()))
                                .tooltip(ui::tip(format!("All tabs ({})", self.tabs.len())))
                                .on_click(cx.listener(|f, _: &ClickEvent, window, cx| f.tab_list_menu(window, cx)))
                                .child(gpui::canvas(move |b, _, _| at.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
                                .child(hover_icon("chevron-down", 15., t::fg3(), t::fg(), "tablist"))
                                .child(div().line_height(px(15.)).child(self.tabs.len().to_string())),
                        )
                    })
                    // The empty part moves the window (double-click maximizes). It reaches up over
                    // the bar's top padding too.
                    .child(div().flex_1().min_w(px(TAB_BAR_DRAG_MIN)).h(px(40.)).window_control_area(WindowControlArea::Drag)),
            )
            .when(self.elevated, |d| {
                d.child(
                    div()
                        .flex_none()
                        .self_center()
                        .mr(px(8.))
                        .px(px(8.))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .rounded(px(6.))
                        .bg(t::rgba(0xf59e0b, 0.15))
                        .text_color(t::warn())
                        .text_size(px(11.5))
                        .child(icon("shield", 12., t::warn()))
                        .child("Administrator"),
                )
            })
            .child(
                div()
                    .flex()
                    .h_full()
                    .child(win_button("min", "minus", WindowControlArea::Min, false))
                    .child(win_button("max", if window.is_maximized() { "copy" } else { "square" }, WindowControlArea::Max, false))
                    .child(win_button("close", "x", WindowControlArea::Close, true)),
            )
    }

    /// A tab's name: its folder's, or a saved search's.
    fn tab_label(&self, tab: &crate::pane::Tab) -> SharedString {
        let p = tab.pane();
        if p.is_search() {
            self.settings
                .saved_searches()
                .into_iter()
                .find(|(_, q)| Some(q.as_str()) == ferret_core::search::search_query_of(&p.path))
                .map(|(n, _)| n)
                .unwrap_or_else(|| util::folder_name(&p.path))
                .into()
        } else {
            let n = util::folder_name(&p.path);
            if n.is_empty() { "…".into() } else { n.into() }
        }
    }

    /// Every tab's width, and whether the strip overflows (then it scrolls and the "all tabs"
    /// button shows, as the app's). Each tab shows its full name while they all fit, as in
    /// Chrome; when the bar runs out of room the longest are cut (…) to one equal width first, so
    /// short names stay whole. Below the minimum width the strip scrolls.
    fn tab_widths(&self, narrow: bool, window: &Window) -> (Vec<f32>, bool) {
        let min = if narrow { 84. } else { 96. };
        // Padding, icon, name, split mark, close button.
        let full: Vec<f32> = self
            .tabs
            .iter()
            .map(|tab| (64. + if tab.split.is_some() { 19. } else { 0. } + text_width(&self.tab_label(tab), 13., window)).max(min))
            .collect();
        let bar = self.tab_bar_w.get();
        if bar <= 0. {
            return (full, false);
        }
        // Left padding, the new-tab button, the window's drag area, gaps.
        let room = bar - 8. - 32. - TAB_BAR_DRAG_MIN - 2. * self.tabs.len().saturating_sub(1) as f32;
        let widths = fit_widths(&full, room, min);
        if widths.iter().sum::<f32>() <= room + 0.5 {
            return (widths, false);
        }
        // They don't fit even at the minimum: the "all tabs" button takes its room too (its
        // margin, padding, chevron, gap, count).
        let list = 4. + 12. + 15. + 2. + text_width(&self.tabs.len().to_string().into(), 11., window);
        (fit_widths(&full, room - list, min), true)
    }

    fn tab_view(&self, i: usize, tab: &crate::pane::Tab, width: f32, ringed: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = tab.pane();
        let active = i == self.active;
        let bin = p.is_bin();
        let search = p.is_search();
        let label = self.tab_label(tab);
        let drop_path = (!search).then(|| p.path.clone());
        let drag = TabDrag { ix: i, label: label.clone() };
        let tip = p.path.clone();
        div()
            .id(("tab", tab.id as usize))
            .role(Role::Tab)
            .aria_label(label.clone())
            .aria_selected(active)
            .on_a11y_action(AccessibleAction::Click, crate::view::on_a11y(cx, move |f, window, cx| {
                f.switch_tab(i, cx);
                window.focus(&f.focus, cx);
            }))
            .group("tab")
            .relative()
            .flex()
            .items_center()
            .gap(px(7.))
            .h(px(30.))
            // Its share of the bar (`tab_widths`).
            .flex_none()
            .w(px(width))
            .pl(px(11.))
            .pr(px(6.))
            .rounded_tl(px(9.))
            .rounded_tr(px(9.))
            .text_color(if active { t::fg() } else { t::fg2() })
            .when(active, |d| {
                d.bg(t::accent_softer()).child(div().absolute().left_0().right_0().bottom_0().h(px(2.)).bg(t::accent()))
            })
            .when(!active, |d| d.hover(|s| s.bg(t::hover()).text_color(t::fg())))
            .tooltip(ui::tip(tip))
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, _, window, cx| {
                f.switch_tab(i, cx);
                window.focus(&f.focus, cx);
            }))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, _, _, cx| f.close_tab(i, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                f.tab_menu(i, ev.position, window, cx);
                cx.stop_propagation();
            }))
            .on_drag(drag, |d: &TabDrag, _, _, cx| cx.new(|_| d.clone()))
            .drag_over::<TabDrag>(|s, _, _, _| s.border_l_2().border_color(t::accent()))
            .on_drop(cx.listener(move |f, d: &TabDrag, _, cx| f.move_tab(d.ix, i, cx)))
            // Files dragged over a tab and held there switch to it (650 ms), to drop into its folder.
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(move |f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                let inside = ev.bounds.contains(&ev.event.position);
                if !inside {
                    if f.tab_hover.is_some_and(|(j, _)| j == i) {
                        f.tab_hover = None;
                    }
                    return;
                }
                if i == f.active || f.tab_hover.is_some_and(|(j, _)| j == i) {
                    return;
                }
                let at = std::time::Instant::now();
                f.tab_hover = Some((i, at));
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(650)).await;
                    let _ = this.update(cx, |f, cx| {
                        if f.tab_hover == Some((i, at)) {
                            f.tab_hover = None;
                            f.switch_tab(i, cx);
                        }
                    });
                })
                .detach();
            }))
            .when_some(drop_path, |d, path| {
                crate::view::drag_target(d, path.clone(), 3, cx)
                    .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()))
                    .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| {
                        f.drop_paths(paths.paths().to_vec(), path.clone(), window, cx)
                    }))
            })
            .child(if bin {
                icon("trash-2", 14., t::fg2())
            } else if search {
                icon("search", 14., t::fg2())
            } else {
                icon(if ferret_core::archive::split_virtual(&p.path).is_some() && !std::path::Path::new(&p.path).is_dir() { "file-archive" } else { "folder" }, 14., t::folder())
            })
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when(tab.split.is_some(), |d| d.child(icon("columns-2", 12., t::fg3())))
            .child(
                div()
                    .id(("tab-close", tab.id as usize))
                    .role(Role::Button)
                    .aria_label("Close tab")
                    .flex_none()
                    .p(px(3.))
                    .rounded(px(5.))
                    .when(!active, |d| d.invisible().group_hover("tab", |s| s.visible()))
                    .hover(|s| s.bg(t::press()))
                    .tooltip(ui::tip("Close tab (Ctrl+W)"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |f, _, _, cx| f.close_tab(i, cx)))
                    .child(icon("x", 13., t::fg3())),
            )
            .when(ringed, |d| d.child(ui::ring(9.)))
            .into_any_element()
    }

    fn tab_menu(&mut self, i: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(i) else { return };
        let path = tab.pane().path.clone();
        let n = self.tabs.len();
        let mut items = self.place_items(&path);
        items.push(sep());
        let dup = path.clone();
        items.push(item("Duplicate tab", action(move |f, _, cx| f.new_tab(dup.clone(), cx))));
        items.push(item("Refresh", action(move |f, _, cx| {
            f.switch_tab(i, cx);
            f.refresh(cx);
        })));
        items.push(item("Close tab", action(move |f, _, cx| f.close_tab(i, cx))).keys("Ctrl+W").disabled(n < 2));
        items.push(item("Close other tabs", action(move |f, _, cx| f.close_other_tabs(i, cx))).disabled(n < 2));
        items.push(item("Reopen closed tab", action(|f, _, cx| f.reopen_tab(cx))).keys("Ctrl+Shift+T").disabled(self.closed.is_empty()));
        // As the app: Windows' own Properties and menu for the tab's folder (not a server's).
        if !ferret_core::paths::is_remote(&path) && ferret_core::archive::split_virtual(&path).is_none() {
            items.push(sep());
            let pp = path.clone();
            items.push(item("Properties", action(move |_, window, cx| {
                let Some(h) = crate::view::hwnd(window) else { return };
                let paths = vec![pp.clone()];
                let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::shell::show_properties(h, paths)) });
                cx.spawn(async move |this, cx| {
                    if let Err(e) = task.await {
                        this.update(cx, |f, cx| f.toast_error(e, cx)).ok();
                    }
                })
                .detach();
            }))
            .icon("info"));
            let sm = path.clone();
            items.push(item("Show more options", action(move |f, window, cx| f.native_menu(vec![sm.clone()], false, window, cx))).icon("ellipsis"));
        }
        self.open_menu(at, items, window, cx);
    }

    /// The "all tabs" menu (TitleBar.tsx's `.tab-list`): "N tabs", then each tab as "name · the
    /// folder it's in", the one in use checked; it opens under the button.
    fn tab_list_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = vec![heading(format!("{} tabs", self.tabs.len()))];
        items.extend(self.tabs.iter().enumerate().map(|(i, tab)| {
            let path = &tab.pane().path;
            let name = match util::folder_name(path) {
                n if n.is_empty() => path.clone(),
                n => n,
            };
            item(format!("{name}  ·  {}", util::parent(path).unwrap_or_default()), action(move |f, _, cx| f.switch_tab(i, cx))).checked(i == self.active)
        }));
        let at = self.tab_list_at.get().map(|b| gpui::point(b.origin.x, b.origin.y + b.size.height + px(4.))).unwrap_or_else(|| window.mouse_position());
        self.open_menu(at, items, window, cx);
    }

    // ───────────────────────────── toolbar ─────────────────────────────

    pub fn toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = window.viewport_size().width.as_f32();
        let p = self.pane();
        let can_back = !p.back.is_empty();
        let can_fwd = !p.fwd.is_empty();
        let can_up = util::parent(&p.path).is_some();
        let loading = p.loading;
        let fav = self.favorites.iter().any(|f| same(&f.path, &p.path));
        let path_area: AnyElement = if self.path_edit.is_some() { self.path_editor(window, cx) } else { self.breadcrumbs(window, cx) };
        let filter = self.filter_view(window, cx);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .border_t_1()
            .border_b_1()
            .border_color(t::line())
            .when(width < 860., |d| {
                d.child(ui::icon_btn("side-toggle", "panel-left", true).tooltip(ui::tip("Sidebar (Ctrl+B)")).on_click(cx.listener(|f, _, window, cx| f.toggle_sidebar(window, cx))))
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(2.))
                    .child(
                        ui::icon_btn("back", "arrow-left", can_back)
                            .tooltip(ui::tip("Back (Alt+←) · right-click for history"))
                            .on_click(cx.listener(|f, _, _, cx| f.go_back(cx)))
                            .on_mouse_down(MouseButton::Right, cx.listener(|f, ev: &MouseDownEvent, window, cx| f.history_menu(true, ev.position, window, cx))),
                    )
                    .child(
                        ui::icon_btn("fwd", "arrow-right", can_fwd)
                            .tooltip(ui::tip("Forward (Alt+→) · right-click for history"))
                            .on_click(cx.listener(|f, _, _, cx| f.go_forward(cx)))
                            .on_mouse_down(MouseButton::Right, cx.listener(|f, ev: &MouseDownEvent, window, cx| f.history_menu(false, ev.position, window, cx))),
                    )
                    .child(ui::icon_btn("up", "arrow-up", can_up).tooltip(ui::tip("Up (Alt+↑)")).on_click(cx.listener(|f, _, _, cx| f.go_up(cx))))
                    .when(width >= 480., |d| {
                        d.child(
                            div()
                                .id("refresh")
                                .role(Role::Button)
                                .aria_label("Refresh")
                                .aria_keyshortcuts("F5")
                                .group("refresh")
                                .flex_none()
                                .w(px(30.))
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .hover(|s| s.bg(t::hover()))
                                .tooltip(ui::tip("Refresh (F5)"))
                                .on_click(cx.listener(|f, _, _, cx| f.refresh(cx)))
                                .child(if loading {
                                    ui::spinner(16., t::fg2()).into_any_element()
                                } else {
                                    hover_icon("rotate-cw", 16., t::fg2(), t::fg(), "refresh").into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .id("star")
                                .role(Role::Button)
                                .aria_label(if fav { "Remove from favorites" } else { "Add to favorites" })
                                .aria_keyshortcuts("Ctrl+D")
                                .group("star")
                                .flex_none()
                                .w(px(30.))
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(7.))
                                .hover(|s| s.bg(t::hover()))
                                .tooltip(ui::tip(if fav { "Remove from favorites (Ctrl+D)" } else { "Add to favorites (Ctrl+D)" }))
                                .on_click(cx.listener(|f, _, _, cx| {
                                    let path = f.pane().path.clone();
                                    f.toggle_favorite(path, true, cx);
                                }))
                                .child(if fav { icon("star", 16., t::accent_text()) } else { hover_icon("star", 16., t::fg2(), t::fg(), "star") }),
                        )
                    }),
            )
            .child(path_area)
            .child(filter)
            .child(
                ui::icon_btn("search", "search", true)
                    .tooltip(ui::tip("Search (Ctrl+K)"))
                    .on_click(cx.listener(|f, _, window, cx| f.open_palette(None, window, cx))),
            )
    }

    fn history_menu(&mut self, back: bool, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.pane();
        let list: Vec<String> = if back { p.back.iter().rev().take(15).cloned().collect() } else { p.fwd.iter().rev().take(15).cloned().collect() };
        if list.is_empty() {
            return;
        }
        let items = list
            .into_iter()
            .enumerate()
            .map(|(i, path)| {
                item(util::folder_name(&path), action(move |f, _, cx| f.go_history(back, i + 1, cx)))
                    .icon("folder")
                    .detail(util::parent(&path).map(|p| util::folder_name(&p)).unwrap_or_default())
            })
            .collect();
        self.open_menu(at, items, window, cx);
    }

    /// The path as clickable parts. Leading parts fold into "…" when there isn't room.
    fn breadcrumbs(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let p = self.pane();
        let parts = util::crumbs(&p.path);
        let n = parts.len();
        // Room for the parts: the window's width less the docked sidebar, the toolbar's buttons
        // (history, refresh, star; filter and search) and the filter box when it's open. The
        // toolbar runs across the preview pane, so the preview takes nothing from it (it used to
        // be subtracted, folding the path into "…" with half the bar empty).
        let w = window.viewport_size().width.as_f32();
        let docked = !self.sidebar_hidden && w >= 860.;
        let room = w
            - if docked { self.sidebar_w + 27. } else { 10. }
            - 273.
            - if w < 860. { 40. } else { 0. }
            - if self.filter_box.is_some() { 230. } else { 0. };
        let widths: Vec<f32> = parts.iter().map(|(l, _)| l.chars().count() as f32 * 7.2 + 14. + 18.).collect();
        let mut folded = 0;
        while folded + 1 < n && widths[folded..].iter().sum::<f32>() + if folded > 0 { 34. } else { 0. } > room.max(120.) {
            folded += 1;
        }
        let remote_or_wsl = ferret_core::paths::is_wsl(&p.path) || ferret_core::paths::is_remote(&p.path);
        // The keyboard's crumb (F6): a folded one rings the "…".
        let ring = match self.ring_area(window) {
            crate::view::Area::Crumbs(i) => Some(i.min(n.saturating_sub(1))),
            _ => None,
        };
        let mut bar = div()
            .id("crumbs")
            .role(Role::Navigation)
            .aria_label("Path")
            .on_a11y_action(AccessibleAction::Focus, crate::view::on_a11y(cx, |f, window, cx| f.edit_path(window, cx)))
            .flex_1()
            .min_w_0()
            .h(px(32.))
            .flex()
            .items_center()
            .px(px(6.))
            .rounded(px(8.))
            .bg(t::hover())
            .border_1()
            .border_color(t::rgba(0, 0.))
            .hover(|s| s.border_color(t::line2()))
            .overflow_hidden()
            .cursor(gpui::CursorStyle::IBeam)
            .on_click(cx.listener(|f, _, window, cx| f.edit_path(window, cx)));
        if folded > 0 {
            let hidden: Vec<(SharedString, String)> = parts[..folded].to_vec();
            bar = bar.child(
                div()
                    .id("crumb-fold")
                    .role(Role::Button)
                    .aria_label("Folders above")
                    .relative()
                    .when(ring.is_some_and(|r| r < folded), |d| d.child(ui::ring(5.)))
                    .flex_none()
                    .h(px(24.))
                    .px(px(7.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_color(t::fg2())
                    .hover(|s| s.bg(t::press()).text_color(t::fg()))
                    .tooltip(ui::tip("Folders above"))
                    .on_click(cx.listener(move |f, ev: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        let items = hidden
                            .iter()
                            .rev()
                            .map(|(label, path)| {
                                let path = path.clone();
                                item(label.clone(), action(move |f, _, cx| f.go(path.clone(), cx))).icon("folder")
                            })
                            .collect();
                        f.open_menu(ev.position(), items, window, cx);
                    }))
                    .child("…"),
            );
        }
        for (i, (label, path)) in parts.into_iter().enumerate().skip(folded) {
            let is_last = i + 1 == n;
            let drive = (label.len() == 2 && label.ends_with(':')).then(|| label.chars().next().unwrap_or('?'));
            if i > folded || folded > 0 {
                let sib = util::parent(&path).unwrap_or_default();
                bar = bar.child(self.crumb_sep(("sep", i), sib, cx));
            }
            let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
            bar = bar.child(
                div()
                    .id(("crumb", i))
                    .role(Role::Link)
                    .aria_label(label.clone())
                    .relative()
                    .flex_none()
                    .h(px(24.))
                    .px(px(if drive.is_some() { 4. } else { 7. }))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .whitespace_nowrap()
                    .when(is_last, |d| d.min_w(px(40.)).truncate())
                    .text_color(if i == 0 && remote_or_wsl {
                        if t::is_dark() { t::rgba(0x7ee0a5, 1.) } else { t::rgba(0x15803d, 1.) }
                    } else if is_last {
                        t::fg()
                    } else {
                        t::fg2()
                    })
                    .when(is_last, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .hover(|s| s.bg(t::press()).text_color(t::fg()))
                    .cursor(gpui::CursorStyle::PointingHand)
                    .on_click(cx.listener(move |f, _, _, cx| {
                        cx.stop_propagation();
                        f.go(p1.clone(), cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        f.crumb_menu(p2.clone(), ev.position, window, cx);
                    }))
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |f, _, _, cx| {
                        cx.stop_propagation();
                        let at = f.tabs.len();
                        f.push_tab(p3.clone(), None, Some(at), cx);
                        cx.notify();
                    }))
                    .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()))
                    .on_drag_move::<gpui::ExternalPaths>({
                        let path = path.clone();
                        cx.listener(move |f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                            if ev.bounds.contains(&ev.event.position) {
                                f.note_drag_target(ev.event.position, &path, 3, cx);
                            }
                        })
                    })
                    .on_drop(cx.listener({
                        let path = path.clone();
                        move |f, paths: &gpui::ExternalPaths, window, cx| f.drop_paths(paths.paths().to_vec(), path.clone(), window, cx)
                    }))
                    .child(match drive {
                        Some(letter) => ui::drive_badge(letter, false).into_any_element(),
                        None => label.into_any_element(),
                    })
                    .when(ring == Some(i), |d| d.child(ui::ring(5.))),
            );
        }
        // A trailing chevron lists the current folder's subfolders.
        bar = bar.child(self.crumb_sep("sep-tail", p.path.clone(), cx));
        bar.into_any_element()
    }

    fn crumb_sep(&self, id: impl Into<gpui::ElementId>, folder: String, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(format!("Subfolders of {}", util::folder_name(&folder)))
            .group("csep")
            .flex_none()
            .h(px(24.))
            .w(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .hover(|s| s.bg(t::press()))
            .tooltip(ui::tip("Subfolders"))
            .on_click(cx.listener(move |f, ev: &ClickEvent, window, cx| {
                cx.stop_propagation();
                f.subfolder_menu(folder.clone(), ev.position(), window, cx);
            }))
            .child(hover_icon("chevron-right", 14., t::fg3(), t::fg(), "csep"))
    }

    /// A breadcrumb's chevron: the folder's subfolders (up to 80, hidden ones left out), the one
    /// on the current path checked.
    fn subfolder_menu(&mut self, folder: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let cur = self.pane().path.clone();
        let task = cx.background_executor().spawn({
            let folder = folder.clone();
            async move { ferret_core::rt::block_on(ferret_core::browse::list_dir(folder)).map(|l| l.entries) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let entries = task.await.unwrap_or_default();
            this.update_in(cx, |f, window, cx| {
                let mut dirs: Vec<_> = entries.into_iter().filter(|e| e.is_dir && (crate::pane::show_hidden() || !e.hidden)).collect();
                dirs.sort_by(|a, b| crate::sortgroup::collate(&a.name, &b.name));
                let mut items: Vec<MenuItem> = dirs
                    .into_iter()
                    .take(80)
                    .map(|e| {
                        let on = util::within(&cur, &e.path);
                        let path = e.path.clone();
                        item(e.name, action(move |f, _, cx| f.go(path.clone(), cx))).icon("folder").checked(on)
                    })
                    .collect();
                if items.is_empty() {
                    items.push(item("No subfolders", action(|_, _, _| {})).disabled(true));
                }
                // A long list opens scrolled to the folder on the current path.
                let on = items.iter().position(|i| i.checked == Some(true));
                f.open_menu(at, items, window, cx);
                if let Some(ix) = on {
                    f.scroll_menu_to(ix, window);
                }
            })
            .ok();
        })
        .detach();
    }

    fn crumb_menu(&mut self, path: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let fav = self.favorites.iter().any(|f| same(&f.path, &path));
        let (p1, p2, p3, p4) = (path.clone(), path.clone(), path.clone(), path.clone());
        let items = vec![
            item("Open in new tab", action(move |f, _, cx| f.new_tab(p1.clone(), cx))).icon("plus"),
            item("Open in new window", action(move |f, _, cx| f.new_window(Some(p2.clone()), cx))).icon("app-window"),
            item("Copy path", action(move |f, _, cx| f.copy_paths(std::slice::from_ref(&p3), false, cx))).icon("copy"),
            sep(),
            item(if fav { "Remove from favorites" } else { "Add to favorites" }, action(move |f, _, cx| f.toggle_favorite(p4.clone(), true, cx))).icon("star"),
        ];
        self.open_menu(at, items, window, cx);
    }

    // ───────────────────────────── path editing ─────────────────────────────

    pub fn edit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kbd = crate::view::Area::List;
        if let Some(pe) = &self.path_edit {
            // Like a browser's address bar: Ctrl+L again selects the whole path.
            pe.input.update(cx, |i, cx| i.select_everything(cx));
            crate::input::focus(&pe.input, window, cx);
            return;
        }
        let path = self.pane().path.clone();
        let shown = ferret_core::paths::remote_parts(&path).map(|(id, l)| format!("{id}:{l}")).unwrap_or(path);
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Type a path", cx).sized(12.5, 20.);
            i.set_text(&shown, cx);
            i.select_everything(cx);
            i
        });
        let sub = cx.subscribe(&input, |f, input, _: &crate::input::Changed, cx| {
            let text = input.read(cx).text().to_string();
            f.suggest_paths(text, cx);
        });
        crate::input::focus(&input, window, cx);
        self.path_edit = Some(PathEdit { input, suggestions: Vec::new(), pick: None, _sub: sub });
        cx.notify();
    }

    /// Up to 8 subfolders of what's typed so far that start with its last part.
    fn suggest_paths(&mut self, text: String, cx: &mut Context<Self>) {
        // Once typing pauses (90 ms, as the app's `suggest`): every key listed a folder before,
        // on a server too (`/`, `/home`, … while a path was being typed).
        use std::sync::atomic::Ordering;
        let seq = self.suggest_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let latest = self.suggest_gen.clone();
        let pause = cx.background_executor().timer(std::time::Duration::from_millis(90));
        let task = cx.background_executor().spawn(async move {
            pause.await;
            // Another key came meanwhile: that one lists instead.
            if latest.load(Ordering::Acquire) != seq {
                return Vec::new();
            }
            let t = text.trim().trim_matches('"').to_string();
            let (dir, part) = match t.rfind(['\\', '/']) {
                Some(i) => (t[..=i].to_string(), t[i + 1..].to_lowercase()),
                None => return Vec::new(),
            };
            let resolved = ferret_core::rt::block_on(ferret_core::browse::resolve_input(dir.clone(), None)).ok();
            let Some(r) = resolved.filter(|r| r.kind == "dir") else { return Vec::new() };
            let Ok(l) = ferret_core::rt::block_on(ferret_core::browse::list_dir(r.path)) else { return Vec::new() };
            let mut out: Vec<String> = l
                .entries
                .into_iter()
                .filter(|e| e.is_dir && e.name.to_lowercase().starts_with(&part) && !e.name.starts_with('$'))
                .map(|e| format!("{dir}{}", e.name))
                .collect();
            out.sort_by_key(|a| a.to_lowercase());
            out.truncate(8);
            out
        });
        cx.spawn(async move |this, cx| {
            let list = task.await;
            this.update(cx, |f, cx| {
                if f.suggest_gen.load(std::sync::atomic::Ordering::Acquire) != seq {
                    return;
                }
                if let Some(pe) = f.path_edit.as_mut() {
                    pe.suggestions = list;
                    pe.pick = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn path_editor(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let pe = self.path_edit.as_ref().unwrap();
        let focused = pe.input.read(cx).is_focused(window);
        div()
            .flex_1()
            .min_w_0()
            .relative()
            .child(
                div()
                    .id("path-edit")
                    .h(px(32.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(8.))
                    .bg(t::raised())
                    .border_1()
                    .border_color(t::accent())
                    .font_family(t::MONO)
                    .on_mouse_down_out(cx.listener(move |f, _, window, cx| {
                        if f.path_edit.is_some() && !focused {
                            f.close_path_edit(window, cx);
                        }
                    }))
                    .child(pe.input.clone()),
            )
            .child(
                gpui::deferred(
                    div()
                        .absolute()
                        .top(px(36.))
                        .left_0()
                        .right_0()
                        .p(px(5.))
                        .rounded(px(10.))
                        .bg(t::raised())
                        .border_1()
                        .border_color(t::line2())
                        .shadow(t::shadow())
                        .children(pe.suggestions.iter().enumerate().map(|(i, s)| {
                            let s2 = s.clone();
                            div()
                                .id(("suggest", i))
                                .h(px(28.))
                                .px(px(9.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(6.))
                                .when(pe.pick == Some(i), |d| d.bg(t::accent_soft()))
                                .hover(|s| s.bg(t::accent_soft()))
                                .on_click(cx.listener(move |f, _, window, cx| f.submit_path(s2.clone(), window, cx)))
                                .child(icon("folder", 14., t::folder()))
                                .child(div().truncate().child(util::file_name(s)))
                        }))
                        .child(div().px(px(9.)).pt(px(6.)).pb(px(2.)).text_size(px(11.)).text_color(t::fg3()).child("~ /linux %VARS% work · ↑↓ choose · Tab completes")),
                )
                .with_priority(1),
            )
            .into_any_element()
    }

    pub fn close_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path_edit = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Goes where the path box says: a folder, or a file's folder (the file selected and opened).
    pub fn submit_path(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.close_path_edit(window, cx);
        let current = Some(self.pane().path.clone());
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::browse::resolve_input(text, current)) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            this.update(cx, |f, cx| match r {
                Ok(r) if r.kind == "dir" => f.go(r.path, cx),
                Ok(r) if r.kind == "file" => {
                    f.reveal(&r.path, cx);
                    f.open_file(r.path, false, cx);
                }
                Ok(_) => f.toast_error("Path not found", cx),
                Err(e) => f.toast_error(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Keys while typing in a field: rename, the path box, the filter. True if handled.
    pub fn field_key(&mut self, key: &str, m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // A PDF's password, in the preview: Enter unlocks, Esc gives the keys back to the files.
        if self.pdf_unlock.as_ref().is_some_and(|u| u.input.read(cx).is_focused(window)) {
            match key {
                "enter" => self.submit_pdf_password(window, cx),
                "escape" => window.focus(&self.focus, cx),
                _ => return false,
            }
            return true;
        }
        if self.rename.as_ref().is_some_and(|r| r.input.read(cx).is_focused(window)) {
            match key {
                "enter" | "tab" => self.commit_rename(cx),
                "escape" => self.cancel_rename(window, cx),
                "n" if m.control && m.shift => {
                    self.commit_rename(cx);
                    self.new_folder(cx);
                }
                _ => return false,
            }
            if key != "n" {
                window.focus(&self.focus, cx);
            }
            return true;
        }
        if let Some(pe) = self.path_edit.as_mut() {
            if pe.input.read(cx).is_focused(window) {
                let n = pe.suggestions.len();
                match key {
                    "escape" => self.close_path_edit(window, cx),
                    "enter" => {
                        let text = match pe.pick {
                            Some(i) => pe.suggestions[i].clone(),
                            None => pe.input.read(cx).text().to_string(),
                        };
                        self.submit_path(text, window, cx);
                    }
                    "down" if n > 0 => {
                        pe.pick = Some(pe.pick.map_or(0, |i| (i + 1) % n));
                        cx.notify();
                    }
                    "up" if n > 0 => {
                        pe.pick = Some(pe.pick.map_or(n - 1, |i| (i + n - 1) % n));
                        cx.notify();
                    }
                    "tab" if n > 0 => {
                        let s = format!("{}\\", pe.suggestions[pe.pick.unwrap_or(0)]);
                        pe.input.update(cx, |i, cx| i.set_text(&s, cx));
                    }
                    "l" if m.control => self.edit_path(window, cx),
                    _ => return false,
                }
                return true;
            }
        }
        if let Some(fb) = &self.filter_box {
            if fb.input.read(cx).is_focused(window) {
                match key {
                    "escape" => self.close_filter(window, cx),
                    "enter" | "down" => {
                        window.focus(&self.focus, cx);
                        let first = self.pane().visible_rows().next();
                        if let Some(first) = first {
                            self.select(first, cx);
                        }
                    }
                    "f" if m.control => self.open_filter(window, cx),
                    _ => return false,
                }
                return true;
            }
        }
        if key == "escape" {
            window.focus(&self.focus, cx);
            return true;
        }
        false
    }

    // ───────────────────────────── filter ─────────────────────────────

    fn filter_view(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let id = self.pane().id;
        let Some(fb) = self.filter_box.as_ref().filter(|f| f.pane == id) else {
            return ui::icon_btn("filter", "list-filter", true)
                .tooltip(ui::tip("Filter this folder (Ctrl+F)"))
                .on_click(cx.listener(|f, _, window, cx| f.open_filter(window, cx)))
                .into_any_element();
        };
        let _ = window;
        let typed = crate::filter::type_of(&self.pane().filter).is_some();
        div()
            .flex_none()
            .w(px(250.))
            .h(px(32.))
            .flex()
            .items_center()
            .gap(px(4.))
            .pl(px(10.))
            .pr(px(4.))
            .rounded(px(8.))
            .bg(t::raised())
            .border_1()
            .border_color(t::accent_line())
            .child(icon("list-filter", 14., t::fg3()))
            .child(div().flex_1().min_w_0().child(fb.input.clone()))
            .child(
                div()
                    .id("filter-types")
                    .flex_none()
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .when(typed, |d| d.bg(t::accent_softer()))
                    .hover(|s| s.bg(t::hover()))
                    .tooltip(ui::tip("Show only…"))
                    .on_click(cx.listener(|f, ev: &ClickEvent, window, cx| f.show_only_menu(ev.position(), window, cx)))
                    .child(icon("shapes", 14., if typed { t::accent_text() } else { t::fg3() })),
            )
            .child(
                div()
                    .id("filter-clear")
                    .flex_none()
                    .w(px(24.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .hover(|s| s.bg(t::hover()))
                    .tooltip(ui::tip("Clear filter (Esc)"))
                    .on_click(cx.listener(|f, _, window, cx| f.close_filter(window, cx)))
                    .child(icon("x", 14., t::fg3())),
            )
            .into_any_element()
    }

    /// "Show only": the types present in this folder, with how many of each.
    fn show_only_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.pane();
        let current = crate::filter::type_of(&p.filter);
        let mut items = vec![item(format!("All types ({})", kinds::group_digits(p.unfiltered)), action(|f, window, cx| f.set_filter_type(None, window, cx))).checked(current.is_none())];
        for (id, label) in kinds::show_only_types() {
            let n = p
                .entries
                .iter()
                .filter(|e| crate::pane::show_hidden() || !e.hidden)
                .filter(|e| match kinds::find_type(&id) {
                    Some(kinds::Found::Folders) => e.is_dir,
                    Some(kinds::Found::Exts(x)) => !e.is_dir && x.contains(&e.ext.to_lowercase()),
                    _ => false,
                })
                .count();
            if n == 0 {
                continue;
            }
            let on = current.as_deref() == Some(id.as_str());
            let id2 = id.clone();
            items.push(item(format!("{label} ({})", kinds::group_digits(n)), action(move |f, window, cx| f.set_filter_type(Some(&id2), window, cx))).checked(on));
        }
        self.open_menu(at, items, window, cx);
    }

    // ───────────────────────────── status bar ─────────────────────────────

    pub fn status_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.pane();
        let n = p.rows.len();
        let width = window.viewport_size().width.as_f32();
        let filtered = !p.filter.trim().is_empty();
        let sel_rows = p.marked.iter().filter_map(|&i| p.rows.get(i)).collect::<Vec<_>>();
        let mut bytes: u64 = 0;
        let mut unknown = false;
        // Some selected item has a size to show (the app's `anyFile`): a lone folder not counted
        // yet read "1 selected (0 B+)".
        let mut known = false;
        for r in &sel_rows {
            if r.e.is_dir {
                match self.sizes.get(&ferret_core::paths::key(&r.e.path)) {
                    Some(s) if s.done => {
                        bytes += s.bytes;
                        known = true;
                    }
                    _ => unknown = true,
                }
            } else {
                bytes += r.e.size;
                known = true;
            }
        }
        let v = &p.view;
        let sort_label: SharedString = match v.sort_by {
            SortBy::Name => "Name",
            SortBy::Modified => "Date modified",
            SortBy::Created => "Date created",
            SortBy::Size => "Size",
            SortBy::Type => "Type",
        }
        .into();
        let group_label: SharedString = match v.group_by {
            GroupBy::None => "No grouping",
            GroupBy::Date => "By date",
            GroupBy::Type => "By type",
            GroupBy::Size => "By size",
            GroupBy::Letter => "By first letter",
        }
        .into();
        // ferret-core's count of folders on screen still waiting or being counted (the app's
        // `sizeActivity.left`). Counting every size not finished, anywhere, kept "1 left" up for
        // good once one folder never finished (it was skipped, or another tab's).
        let counting = self.sizes_left;
        let save_view = !self.settings.bool("rememberPerFolder", true) && p.view_changed;
        div()
            .h(px(36.))
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(10.))
            .pl(px(14.))
            .pr(px(8.))
            .border_t_1()
            .border_color(t::line())
            .text_size(px(12.))
            .text_color(t::fg2())
            .child(
                div()
                    // To a screen reader: the line as one status ("35 items, 1 hidden, 2 selected").
                    .id("status-text")
                    .role(Role::Status)
                    .aria_label({
                        let mut s = if filtered { format!("{} of {}", kinds::group_digits(n), plural(p.unfiltered, "item")) } else { plural(n, "item") };
                        if p.hidden > 0 && !crate::pane::show_hidden() {
                            s.push_str(&format!(", {} hidden", kinds::group_digits(p.hidden)));
                        }
                        if !p.marked.is_empty() {
                            s.push_str(&format!(", {} selected", kinds::group_digits(p.marked.len())));
                            if bytes > 0 {
                                s.push_str(&format!(", {}", kinds::format_size(bytes)));
                            }
                        }
                        s
                    })
                    .flex()
                    .min_w_0()
                    .overflow_hidden()
                    .items_center()
                    .gap(px(6.))
                    .whitespace_nowrap()
                    .when(p.loading, |d| d.child(ui::spinner(13., t::fg3())))
                    .child(if filtered {
                        format!("{} of {}", kinds::group_digits(n), plural(p.unfiltered, "item"))
                    } else {
                        plural(n, "item")
                    })
                    .when(p.hidden > 0 && !crate::pane::show_hidden(), |d| d.child("·").child(format!("{} hidden", kinds::group_digits(p.hidden))))
                    .when(!p.marked.is_empty(), |d| {
                        d.child("·").child(div().text_color(t::accent_text()).child(format!(
                            "{} selected{}",
                            kinds::group_digits(p.marked.len()),
                            if known { format!(" ({}{})", kinds::format_size(bytes), if unknown { "+" } else { "" }) } else { String::new() }
                        )))
                    })
                    .when(counting > 0, |d| {
                        d.child("·").child(div().text_color(t::fg3()).child(format!("Counting folder sizes · {} left", kinds::group_digits(counting))))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        ui::status_btn("sb-sort", Some(if v.sort_dir == SortDir::Asc { "arrow-up-narrow-wide" } else { "arrow-down-wide-narrow" }), Some(sort_label), false)
                            .tooltip(ui::tip("Sort"))
                            .on_click(cx.listener(|f, ev: &ClickEvent, window, cx| f.sort_menu(ev.position(), window, cx))),
                    )
                    .child(
                        ui::status_btn("sb-group", Some("layers"), Some(group_label), v.group_by != GroupBy::None)
                            .tooltip(ui::tip("Group (Ctrl+G)"))
                            .on_click(cx.listener(|f, ev: &ClickEvent, window, cx| f.group_menu(ev.position(), window, cx))),
                    )
                    .child(
                        ui::status_btn("sb-hidden", Some(if crate::pane::show_hidden() { "eye" } else { "eye-off" }), None, crate::pane::show_hidden())
                            .tooltip(ui::tip(if crate::pane::show_hidden() { "Hide hidden items (Ctrl+H)" } else { "Show hidden items (Ctrl+H)" }))
                            .on_click(cx.listener(|f, _, _, cx| f.toggle_hidden(cx))),
                    )
                    .when(width >= 480., |d| {
                        d.child(
                            ui::status_btn("sb-split", Some("columns-2"), None, self.tab().split.is_some())
                                .tooltip(ui::tip("Split view (Ctrl+\\)"))
                                .on_click(cx.listener(|f, _, _, cx| f.toggle_split(cx))),
                        )
                        .child(
                            ui::status_btn("sb-preview", Some("panel-right"), None, self.preview_open)
                                .tooltip(ui::tip("Preview pane (Alt+P)"))
                                .on_click(cx.listener(|f, _, window, cx| f.toggle_preview(window, cx))),
                        )
                    })
                    .when(v.mode == Mode::Grid, |d| d.child(self.icon_slider(v.icon_size, cx)))
                    .child(
                        ui::seg()
                            .child(ui::seg_btn("sb-list", Some("list"), None, v.mode == Mode::List).tooltip(ui::tip("List (Ctrl+1)")).on_click(cx.listener(|f, _, _, cx| f.set_mode(Mode::List, cx))))
                            .child(ui::seg_btn("sb-grid", Some("layout-grid"), None, v.mode == Mode::Grid).tooltip(ui::tip("Grid (Ctrl+2)")).on_click(cx.listener(|f, _, _, cx| f.set_mode(Mode::Grid, cx)))),
                    ),
            )
            .when(save_view, |d| {
                d.child(
                    div()
                        .id("save-view")
                        .role(Role::Button)
                        .aria_label("Save view for this folder")
                        .absolute()
                        .right(px(10. + if self.preview_open { self.preview_w } else { 0. }))
                        .bottom(px(44.))
                        .px(px(12.))
                        .py(px(7.))
                        .rounded(px(8.))
                        .bg(t::accent())
                        .text_color(t::on_accent())
                        .shadow(t::shadow())
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_click(cx.listener(|f, _, _, cx| f.save_view_here(false, cx)))
                        .child("Save view for this folder"),
                )
            })
    }

    /// The grid's icon size: drag along it (48–256, steps of 8).
    fn icon_slider(&self, size: u32, cx: &mut Context<Self>) -> impl IntoElement {
        let f = (size as f32 - 48.) / (256. - 48.);
        let bounds = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::<Pixels>::default()));
        let b2 = bounds.clone();
        let set = move |f: &mut Ferret, x: Pixels, cx: &mut Context<Ferret>| {
            let b = b2.get();
            if b.size.width.as_f32() <= 0. {
                return;
            }
            let k = ((x - b.origin.x).as_f32() / b.size.width.as_f32()).clamp(0., 1.);
            let s = (48. + k * 208.) / 8.;
            f.set_icon_size((s.round() * 8.) as u32, cx);
        };
        let set2 = set.clone();
        div()
            .id("icon-size")
            .role(Role::Slider)
            .aria_label("Icon size")
            .aria_numeric_value(size as f64)
            .aria_min_numeric_value(48.)
            .aria_max_numeric_value(256.)
            .aria_numeric_value_step(8.)
            .on_a11y_action(AccessibleAction::Increment, crate::view::on_a11y(cx, move |f, _, cx| f.set_icon_size((size + 16).min(256), cx)))
            .on_a11y_action(AccessibleAction::Decrement, crate::view::on_a11y(cx, move |f, _, cx| f.set_icon_size(size.saturating_sub(16).max(48), cx)))
            .w(px(90.))
            .h(px(20.))
            .mx(px(4.))
            .flex()
            .items_center()
            .relative()
            .tooltip(ui::tip(format!("Icon size: {size}px (Ctrl+wheel)")))
            .child(gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().size_full())
            .child(div().w_full().h(px(4.)).rounded(px(2.)).bg(t::line2()).child(div().h_full().w(relative(f)).rounded(px(2.)).bg(t::accent())))
            .child(div().absolute().left(relative(f)).ml(px(-6.)).size(px(12.)).rounded(px(6.)).bg(t::accent()).border_2().border_color(t::raised()))
            .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                set(f, ev.position.x, cx);
            }))
            .on_mouse_move(cx.listener(move |f, ev: &gpui::MouseMoveEvent, _, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    set2(f, ev.position.x, cx);
                }
            }))
    }

    fn sort_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.sort_items();
        self.open_menu(at, items, window, cx);
    }

    fn group_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = self.group_items();
        items.push(sep());
        items.push(item("Expand all groups", action(|f, _, cx| f.expand_all_groups(cx))).disabled(self.pane().collapsed.is_empty()));
        self.open_menu(at, items, window, cx);
    }

    pub fn sort_items(&self) -> Vec<MenuItem> {
        let v = self.pane().view.clone();
        let by = |label: &'static str, k: SortBy| {
            item(label, action(move |f, _, cx| {
                f.set_view(|v| v.sort_by = k, cx);
            }))
            .checked(v.sort_by == k)
            .keep_open()
        };
        vec![
            by("Name", SortBy::Name),
            by("Date modified", SortBy::Modified),
            by("Date created", SortBy::Created),
            by("Size", SortBy::Size),
            by("Type", SortBy::Type),
            sep(),
            item("Ascending", action(|f, _, cx| f.set_view(|v| v.sort_dir = SortDir::Asc, cx))).checked(v.sort_dir == SortDir::Asc).keep_open(),
            item("Descending", action(|f, _, cx| f.set_view(|v| v.sort_dir = SortDir::Desc, cx))).checked(v.sort_dir == SortDir::Desc).keep_open(),
            sep(),
            item("Folders first", action(|f, _, cx| f.set_view(|v| v.folders_first = !v.folders_first, cx))).checked(v.folders_first).keep_open(),
        ]
    }

    pub fn group_items(&self) -> Vec<MenuItem> {
        let v = self.pane().view.clone();
        let by = |label: &'static str, g: GroupBy| item(label, action(move |f, _, cx| f.set_view(|v| v.group_by = g, cx))).checked(v.group_by == g);
        vec![
            by("None", GroupBy::None),
            by("Date", GroupBy::Date),
            by("Type", GroupBy::Type),
            by("Size", GroupBy::Size),
            by("First letter", GroupBy::Letter),
        ]
    }
}

fn win_button(id: &'static str, icon_name: &'static str, area: WindowControlArea, close: bool) -> Stateful<Div> {
    let hover = if close { t::rgba(0xffffff, 1.) } else { t::fg() };
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(match area {
            WindowControlArea::Min => "Minimize",
            WindowControlArea::Max => "Maximize",
            _ => "Close",
        })
        .on_a11y_action(AccessibleAction::Click, move |_, window, _| match area {
            WindowControlArea::Min => window.minimize_window(),
            WindowControlArea::Max => window.zoom_window(),
            _ => window.remove_window(),
        })
        .group("win")
        .w(px(46.))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .window_control_area(area)
        .when(close, |d| d.rounded_tr(px(11.)).hover(|s| s.bg(t::rgba(0xe81123, 1.))))
        .when(!close, |d| d.hover(|s| s.bg(t::hover())))
        .child(hover_icon(icon_name, 15., t::fg2(), hover, "win"))
}

/// The least room left free beside the tabs for moving the window.
const TAB_BAR_DRAG_MIN: f32 = 80.;

/// How wide a line of text is in the app's font, rounded up so it never truncates by a hair.
fn text_width(text: &SharedString, size: f32, window: &Window) -> f32 {
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::font(t::FONT),
        color: t::fg(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(text.clone(), px(size), &[run], None).width.as_f32().ceil() + 1.
}

/// Widths for tabs whose full widths are `full`, in `room`: all full when they fit; otherwise
/// the space is shared out evenly, the narrow ones keeping their full width and the rest cut to
/// one equal width, never below `min` (then the strip scrolls).
fn fit_widths(full: &[f32], room: f32, min: f32) -> Vec<f32> {
    if full.iter().sum::<f32>() <= room {
        return full.to_vec();
    }
    let mut sorted = full.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mut left = room;
    let mut cap = f32::INFINITY;
    for (k, w) in sorted.iter().enumerate() {
        let share = left / (sorted.len() - k) as f32;
        if *w <= share {
            left -= w;
        } else {
            cap = share;
            break;
        }
    }
    full.iter().map(|w| w.min(cap).floor().max(min)).collect()
}

#[cfg(test)]
mod tab_tests {
    use super::fit_widths;

    #[test]
    fn full_names_while_they_fit() {
        assert_eq!(fit_widths(&[100., 300., 96.], 600., 96.), vec![100., 300., 96.]);
    }

    #[test]
    fn longest_cut_first_to_one_width() {
        // 500 of room: 100 and 120 stay whole, the two long ones share the other 280.
        assert_eq!(fit_widths(&[100., 400., 120., 300.], 500., 96.), vec![100., 140., 120., 140.]);
    }

    #[test]
    fn never_below_the_minimum() {
        assert_eq!(fit_widths(&[200., 200., 200.], 150., 96.), vec![96., 96., 96.]);
    }
}

#[allow(dead_code)]
fn _unused(_: Duration) {}
