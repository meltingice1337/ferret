//! The sidebar (Sidebar.tsx): Favorites, saved Searches, Places, Linux distros, Servers,
//! Network, Drives, Recent folders and the Dropzone, each section collapsible and hideable; the
//! footer with Settings and the version. Items are drop targets; the Recycle Bin deletes.

use std::path::PathBuf;

use ferret_core::places::Drive;
use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, MouseButton, MouseDownEvent, Pixels, Point, Role, SharedString,
    Stateful, Window, div, prelude::*, px,
};
use serde_json::json;

use crate::files::plural;
use crate::kinds;
use crate::menu::{MenuItem, item, sep};
use crate::theme as t;
use crate::ui::{self, hover_icon, icon};
use crate::util::{self, same, within};
use crate::view::{Ferret, action};

pub const FAVORITE_COLORS: [u32; 7] = [0xef4444, 0xf59e0b, 0x22c55e, 0x06b6d4, 0x3b82f6, 0xa855f7, 0xec4899];

const GENERIC: [&str; 29] = [
    "src", "lib", "bin", "obj", "build", "dist", "out", "target", "debug", "release", "deps", "docs", "doc", "test", "tests", "assets",
    "public", "static", "data", "temp", "tmp", "config", "scripts", "images", "img", "node_modules", "app", "packages", "",
];

/// A favorite being dragged to a new place.
#[derive(Clone)]
pub struct FavDrag {
    pub path: String,
    pub name: SharedString,
}

impl Render for FavDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(9.))
            .h(px(30.))
            .flex()
            .items_center()
            .gap(px(9.))
            .rounded(px(7.))
            .bg(t::raised())
            .border_1()
            .border_color(t::accent_line())
            .font_family(t::FONT)
            .text_size(px(13.))
            .text_color(t::fg())
            .child(icon("star", 15., t::accent_text()))
            .child(self.name.clone())
    }
}

/// A Dropzone item dragged out: copies (or moves, with Shift) like files from Explorer.
impl Ferret {
    pub fn sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cur = self.pane().path.clone();
        // The items are listed as they're drawn, for the keyboard (F6, then ↑↓ Enter).
        self.side_targets.borrow_mut().clear();
        self.side_mark.set(0);
        self.side_ring.set(match self.ring_area(window) {
            crate::view::Area::Sidebar(i) => Some(i),
            _ => None,
        });
        let mut sections: Vec<AnyElement> = Vec::new();

        // Favorites
        if !self.settings.hidden_section("favorites") {
            let favs: Vec<AnyElement> = if self.favorites.is_empty() {
                vec![hint("Ctrl+D pins the selected item (or the folder you're in) here").into_any_element()]
            } else {
                self.favorites
                    .clone()
                    .into_iter()
                    .enumerate()
                    .map(|(i, fav)| {
                        let color = fav.color.as_deref().and_then(t::parse_hex).map(|c| t::rgba(c, 1.));
                        let icon_name = if fav.is_dir { "folder" } else { kinds::icon(kinds::kind_of(false, &fav.path.rsplit('.').next().unwrap_or("").to_lowercase())) };
                        let path = fav.path.clone();
                        let p2 = path.clone();
                        let drag = FavDrag { path: fav.path.clone(), name: fav.name.clone().into() };
                        let active = fav.is_dir && same(&fav.path, &cur);
                        let mut el = self
                            .side_item(("fav", i), icon_name, fav.name.clone().into(), fav.path.clone(), active, color.map(|c| div().size(px(8.)).rounded(px(4.)).bg(c).into_any_element()), fav.is_dir, cx)
                            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                f.favorite_menu(p2.clone(), ev.position, window, cx);
                            }))
                            .on_drag(drag, |d: &FavDrag, _, _, cx| cx.new(|_| d.clone()))
                            .drag_over::<FavDrag>(|s, _, _, _| s.border_t_2().border_color(t::accent()))
                            .on_drop(cx.listener(move |f, d: &FavDrag, _, cx| f.move_favorite(&d.path, i, cx)));
                        if !fav.is_dir {
                            // A pinned file opens with its app.
                            el = el.on_click(cx.listener(move |f, _, _, cx| f.open_file(path.clone(), false, cx)));
                        }
                        el.into_any_element()
                    })
                    .collect()
            };
            sections.push(self.section("favorites", "Favorites", favs, None, cx));
        }

        // Saved searches
        let searches = self.settings.saved_searches();
        if !searches.is_empty() && !self.settings.hidden_section("searches") {
            let items = searches
                .into_iter()
                .enumerate()
                .map(|(i, (name, q))| {
                    let path = format!("{}{q}", ferret_core::search::SEARCH_PREFIX);
                    let active = same(&path, &cur);
                    let (q1, q2) = (q.clone(), q.clone());
                    let tip = format!("{name}\n{q}\nAlways lists the current matches");
                    self.side_item(("search", i), "search", name.into(), path, active, None, false, cx)
                        .tooltip(ui::tip(tip))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            f.saved_search_menu(q1.clone(), ev.position, window, cx);
                        }))
                        .child(
                            div()
                                .id(("search-x", i))
                                .role(Role::Button)
                                .aria_label("Remove saved search")
                                .invisible()
                                .group_hover("side", |s| s.visible())
                                .p(px(2.))
                                .rounded(px(4.))
                                .hover(|s| s.bg(t::press()))
                                .on_click(cx.listener(move |f, _, _, cx| {
                                    cx.stop_propagation();
                                    f.remove_saved_search(&q2, cx);
                                }))
                                .child(icon("x", 12., t::fg3())),
                        )
                        .into_any_element()
                })
                .collect();
            sections.push(self.section("searches", "Searches", items, None, cx));
        }

        // Places
        if !self.settings.hidden_section("places") {
            let mut known: Vec<AnyElement> = self
                .places
                .known
                .clone()
                .into_iter()
                .filter(|k| !self.settings.hidden_place(k.id))
                .map(|k| {
                    let icon_name = match k.id {
                        "home" => "house",
                        "desktop" => "monitor",
                        "documents" => "file-text",
                        "downloads" => "download",
                        "pictures" => "image",
                        "music" => "music",
                        "videos" => "video",
                        _ => "folder-open",
                    };
                    let id = SharedString::from(format!("place-{}", k.id));
                    let (path, kid) = (k.path.clone(), k.id);
                    self.side_item(id, icon_name, k.name.into(), k.path.clone(), same(&k.path, &cur), None, true, cx)
                        .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            let mut items = f.place_items(&path);
                            items.push(sep());
                            items.push(item("Hide from sidebar", action(move |f, _, cx| {
                                f.settings.toggle_in("hiddenPlaces", kid);
                                cx.notify();
                            })));
                            f.open_menu(ev.position, items, window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            if !self.settings.hidden_place("recycle") {
                let bin = self.bin.unwrap_or_default();
                let badge = (bin.items > 0).then(|| ui::badge(kinds::group_digits(bin.items as usize)).into_any_element());
                known.push(
                    self.side_item("place-recycle", "trash-2", "Recycle Bin".into(), ferret_core::recycle::PATH.into(), ferret_core::recycle::is_recycle(&cur), badge, true, cx)
                        .tooltip(ui::tip(if bin.items > 0 { format!("{} · {}", plural(bin.items as usize, "item"), kinds::format_size(bin.bytes)) } else { "Empty".into() }))
                        .on_mouse_down(MouseButton::Right, cx.listener(|f, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            let n = f.bin.map(|b| b.items).unwrap_or(0);
                            let items = vec![
                                item("Open", action(|f, _, cx| f.go(ferret_core::recycle::PATH.into(), cx))).icon("folder-open"),
                                item("Open in new tab", action(|f, _, cx| f.new_tab(ferret_core::recycle::PATH.into(), cx))).icon("plus"),
                                sep(),
                                item("Empty Recycle Bin", action(|f, _, cx| f.empty_bin(cx))).icon("trash-2").danger().disabled(n == 0),
                                sep(),
                                item("Hide from sidebar", action(|f, _, cx| {
                                    f.settings.toggle_in("hiddenPlaces", "recycle");
                                    cx.notify();
                                })),
                            ];
                            f.open_menu(ev.position, items, window, cx);
                        }))
                        .into_any_element(),
                );
            }
            sections.push(self.section("places", "Places", known, None, cx));
        }

        // Linux
        if !self.places.wsl.is_empty() && !self.settings.hidden_section("linux") {
            let wsl: Vec<AnyElement> = self
                .places
                .wsl
                .clone()
                .into_iter()
                .map(|d| {
                    let id = SharedString::from(format!("wsl-{}", d.name));
                    let badge = d.is_default.then(|| ui::badge("default").into_any_element());
                    let active = within(&cur, &d.path);
                    self.side_item(id, "terminal", d.name.clone().into(), d.path.clone(), active, badge, true, cx).into_any_element()
                })
                .collect();
            sections.push(self.section("linux", "Linux", wsl, None, cx));
        }

        // Servers
        if !self.settings.hidden_section("servers") {
            let conns = ferret_core::remote::remote_connections();
            let states = ferret_core::remote::remote_status();
            let items: Vec<AnyElement> = if conns.is_empty() {
                vec![add_hint("add-server-hint", "Add a server to browse its files over SSH", cx, |f, window, cx| f.open_server_form(None, window, cx)).into_any_element()]
            } else {
                conns
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let path = ferret_core::paths::remote_path(&c.id, "/");
                        let connected = states.iter().any(|s| s.id == c.id && s.connected);
                        let dot = div().size(px(7.)).rounded(px(4.)).bg(if connected { t::ok() } else { t::line2() }).into_any_element();
                        let id = c.id.clone();
                        let to = c.id.clone();
                        let open = std::rc::Rc::new(move |f: &mut Ferret, new_tab: bool, window: &mut Window, cx: &mut Context<Ferret>| {
                            f.open_connection(to.clone(), new_tab, window, cx)
                        });
                        self.side_item_open(("server", i), "server", c.name.clone().into(), path.clone(), within(&cur, &path), Some(dot), true, open, cx)
                            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                f.server_menu(id.clone(), connected, ev.position, window, cx);
                            }))
                            .into_any_element()
                    })
                    .collect()
            };
            let add = self.section_button("add-server", "Add a server", cx, |f, window, cx| f.open_server_form(None, window, cx));
            sections.push(self.section("servers", "Servers", items, Some(add), cx));
        }

        // Network
        if !self.settings.hidden_section("network") {
            let mine = self.settings.network_places("networkPlaces");
            let found = self.settings.network_places("networkFound");
            let mut items: Vec<AnyElement> = Vec::new();
            for (i, (path, name)) in mine.iter().enumerate() {
                let label: SharedString = if name.is_empty() { path.clone().into() } else { name.clone().into() };
                let p2 = path.clone();
                items.push(
                    self.side_item(("net", i), "folder-symlink", label, path.clone(), within(&cur, path), None, true, cx)
                        .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            f.network_menu(p2.clone(), true, ev.position, window, cx);
                        }))
                        .into_any_element(),
                );
            }
            for (i, (path, name)) in found.iter().enumerate().filter(|(_, (p, _))| !mine.iter().any(|(m, _)| same(m, p))) {
                let label: SharedString = if name.is_empty() { path.clone().into() } else { name.clone().into() };
                let p2 = path.clone();
                items.push(
                    self.side_item(("found", i), "monitor", label, path.clone(), within(&cur, path), None, true, cx)
                        .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            f.network_menu(p2.clone(), false, ev.position, window, cx);
                        }))
                        .into_any_element(),
                );
            }
            if items.is_empty() {
                items.push(add_hint("add-net-hint", "Add a NAS or shared folder", cx, |f, window, cx| f.open_network_form(window, cx)).into_any_element());
            }
            items.push(
                div()
                    .id("net-scan")
                    .role(Role::Button)
                    .aria_label("Look for computers again")
                    .px(px(10.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .text_color(t::fg3())
                    .hover(|s| s.text_color(t::accent_text()))
                    .on_click(cx.listener(|f, _, _, cx| f.scan_network(cx)))
                    .child("Look for computers again")
                    .into_any_element(),
            );
            let add = self.section_button("add-net", "Add a network folder", cx, |f, window, cx| f.open_network_form(window, cx));
            sections.push(self.section("network", "Network", items, Some(add), cx));
        }

        // Drives
        if !self.settings.hidden_section("drives") {
            let drives: Vec<AnyElement> = self.places.drives.clone().iter().map(|d| self.drive_item(d, within(&cur, &d.path), cx).into_any_element()).collect();
            sections.push(self.section("drives", "Drives", drives, None, cx));
        }

        // Recent
        if !self.settings.hidden_section("recents") {
            let list: Vec<_> = self.recents.iter().filter(|r| !same(&r.path, &cur)).take(8).cloned().collect();
            let names: Vec<String> = list.iter().map(|r| util::file_name(&r.path).to_lowercase()).collect();
            let items: Vec<AnyElement> = list
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let name = util::folder_name(&r.path);
                    let lower = name.to_lowercase();
                    // Generic or repeated names show their parent folder too.
                    let detail = (GENERIC.contains(&lower.as_str()) || names.iter().filter(|n| **n == lower).count() > 1)
                        .then(|| util::parent(&r.path).map(|p| util::folder_name(&p)))
                        .flatten();
                    let p2 = r.path.clone();
                    self.side_item(("recent", i), "clock", name.into(), r.path.clone(), false, detail.map(|d| div().text_size(px(11.)).text_color(t::fg3()).truncate().max_w(px(90.)).child(d).into_any_element()), true, cx)
                        .child(
                            div()
                                .id(("recent-x", i))
                                .role(Role::Button)
                                .aria_label("Remove from recent")
                                .invisible()
                                .group_hover("side", |s| s.visible())
                                .p(px(2.))
                                .rounded(px(4.))
                                .hover(|s| s.bg(t::press()))
                                .tooltip(ui::tip("Remove from recent"))
                                .on_click(cx.listener(move |f, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Ok(r) = ferret_core::browse::remove_recent(p2.clone()) {
                                        f.recents = r;
                                    }
                                    cx.notify();
                                }))
                                .child(icon("x", 12., t::fg3())),
                        )
                        .into_any_element()
                })
                .collect();
            let clear = (!list.is_empty()).then(|| self.section_button("clear-recent", "Clear recents", cx, |f, _, cx| {
                if let Ok(r) = ferret_core::browse::clear_recents() {
                    f.recents = r;
                }
                cx.notify();
            }));
            if !items.is_empty() {
                sections.push(self.section("recents", "Recent", items, clear, cx));
            }
        }

        // The Dropzone: pinned under the list, above Settings (as the app's), so it's always there
        // to drop on, however long the list.
        let dropzone = (!self.settings.hidden_section("dropzone")).then(|| self.dropzone(cx)).flatten();

        let version = env!("CARGO_PKG_VERSION");
        // A copy run from `target\` (not installed) says so, as the app's DEV pill.
        let dev = !crate::appdata::installed();
        let waiting = self.update_waiting().map(str::to_string);
        // The list fades out above the footer, so a section cut off there reads as "more below"
        // (it ended in a hard line through a drive's name). The fade lies over the list's bottom
        // padding once it's scrolled to its end. GPUI can't mask: a gradient into the colour the
        // panel shows there.
        let vs = window.viewport_size();
        let fade_to = if self.sidebar_overlay && vs.width.as_f32() < 860. {
            t::panel_over_solid()
        } else {
            t::panel_seen((self.sidebar_w / 2. / vs.width.as_f32()).clamp(0., 1.), ((vs.height.as_f32() - 50.) / vs.height.as_f32()).clamp(0., 1.))
        };
        ui::panel()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Moving a favorite or a tab isn't dragging files: a hint left by files dragged over
            // and away (not dropped) would open the Dropzone's target meanwhile.
            .on_drag_move::<FavDrag>(cx.listener(|f, _: &gpui::DragMoveEvent<FavDrag>, _, cx| {
                if f.drag_hint.take().is_some() {
                    cx.notify();
                }
            }))
            .on_drag_move::<crate::chrome::TabDrag>(cx.listener(|f, _: &gpui::DragMoveEvent<crate::chrome::TabDrag>, _, cx| {
                if f.drag_hint.take().is_some() {
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("side-scroll")
                            .role(Role::Navigation)
                            .aria_label("Sidebar")
                            .size_full()
                            .overflow_y_scroll()
                            .px(px(8.))
                            .pt(px(10.))
                            .pb(px(14.))
                            .flex()
                            .flex_col()
                            .children(sections),
                    )
                    .child(div().absolute().left_0().right_0().bottom_0().h(px(16.)).bg(gpui::linear_gradient(
                        180.,
                        gpui::linear_color_stop(gpui::Hsla { a: 0., ..fade_to }, 0.),
                        gpui::linear_color_stop(fade_to, 1.),
                    ))),
            )
            .children(dropzone)
            .child(
                // The status bar's height, so Settings sits on the same line as its items and the
                // two top borders meet (with the app's 6 + 8 px padding it sat 5 px higher).
                div()
                    .flex_none()
                    .h(px(36.))
                    .px(px(8.))
                    .border_t_1()
                    .border_color(t::line())
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .id("settings")
                            .role(Role::Button)
                            .aria_label("Settings")
                            .aria_keyshortcuts("Ctrl+,")
                            .group("settings")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .h(px(30.))
                            .px(px(9.))
                            .rounded(px(7.))
                            .text_color(t::fg2())
                            .hover(|s| s.bg(t::hover()).text_color(t::fg()))
                            .tooltip(ui::tip("Settings (Ctrl+,)"))
                            .on_click(cx.listener(|f, _, _, cx| f.open_settings(cx)))
                            .child(hover_icon("settings", 15., t::fg2(), t::fg(), "settings"))
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .id("version")
                            .role(Role::Button)
                            .aria_label(match &waiting {
                                Some(v) => format!("Ferret {v} is available: see what's new"),
                                None => format!("About Ferret {version}"),
                            })
                            .px(px(7.))
                            .h(px(24.))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .rounded(px(6.))
                            .text_size(px(11.))
                            .text_color(t::fg3())
                            .hover(|s| s.bg(t::hover()).text_color(t::fg()))
                            .tooltip(ui::tip(match &waiting {
                                Some(v) => format!("Ferret {v} is available: see what's new"),
                                None if dev => format!("Ferret {version} (a development build)"),
                                None => format!("About Ferret {version}"),
                            }))
                            .on_click(cx.listener(|f, _, _, cx| {
                                if f.update_waiting().is_some() {
                                    f.show_whats_new(cx);
                                } else {
                                    f.open_settings_at(crate::settings_view::SettingsTab::About, cx);
                                }
                            }))
                            .child(format!("v{version}"))
                            // A dot while an update waits, as the app's.
                            .when(waiting.is_some(), |d| d.child(div().size(px(7.)).rounded(px(4.)).bg(t::accent())))
                            .when(dev, |d| d.child(ui::badge("DEV"))),
                    ),
            )
    }

    /// A section: its title (click to fold, right-click to hide or customize), its items.
    fn section(&self, id: &'static str, title: &'static str, items: Vec<AnyElement>, extra: Option<AnyElement>, cx: &mut Context<Self>) -> AnyElement {
        let collapsed = self.collapsed_sections.contains(id);
        // A collapsed section's items aren't shown, so the keyboard skips them.
        if collapsed {
            self.side_targets.borrow_mut().truncate(self.side_mark.get());
        }
        self.side_mark.set(self.side_targets.borrow().len());
        div()
            .mb(px(8.))
            .flex()
            .flex_col()
            .child(
                div()
                    .id(SharedString::from(format!("sec-{id}")))
                    .role(Role::Button)
                    .aria_label(title)
                    .aria_expanded(!collapsed)
                    .group("sec")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(8.))
                    .pt(px(6.))
                    .pb(px(4.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t::fg3())
                    .hover(|s| s.text_color(t::fg2()))
                    .on_click(cx.listener(move |f, _, _, cx| {
                        if !f.collapsed_sections.remove(id) {
                            f.collapsed_sections.insert(id.to_string());
                        }
                        f.save_session();
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        let collapsed = f.collapsed_sections.contains(id);
                        let items = vec![
                            item(if collapsed { "Expand" } else { "Collapse" }, action(move |f, _, cx| {
                                if !f.collapsed_sections.remove(id) {
                                    f.collapsed_sections.insert(id.to_string());
                                }
                                cx.notify();
                            })),
                            item(format!("Hide {title}"), action(move |f, _, cx| {
                                f.settings.toggle_in("hiddenSections", id);
                                cx.notify();
                            })),
                            sep(),
                            item("Customize sidebar…", action(|f, _, cx| f.open_settings_at(crate::settings_view::SettingsTab::Sidebar, cx))),
                        ];
                        f.open_menu(ev.position, items, window, cx);
                    }))
                    .child(div().flex_1().child(title.to_uppercase()))
                    .children(extra)
                    .child(icon(if collapsed { "chevron-right" } else { "chevron-down" }, 13., t::fg3())),
            )
            .when(!collapsed, |d| d.children(items))
            .into_any_element()
    }

    fn section_button(&self, id: &'static str, tip: &'static str, cx: &mut Context<Self>, run: impl Fn(&mut Ferret, &mut Window, &mut Context<Ferret>) + 'static) -> AnyElement {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(tip)
            .p(px(2.))
            .rounded(px(4.))
            .hover(|s| s.bg(t::press()))
            .tooltip(ui::tip(tip))
            .on_click(cx.listener(move |f, _, window, cx| {
                cx.stop_propagation();
                run(f, window, cx);
            }))
            .child(icon(if id.starts_with("clear") { "trash-2" } else { "plus" }, 12., t::fg3()))
            .into_any_element()
    }

    /// Lists a sidebar item for the keyboard; true if it's the one the keyboard is at.
    fn side_target(&self, path: &str) -> bool {
        let mut list = self.side_targets.borrow_mut();
        list.push(path.to_string());
        self.side_ring.get() == Some(list.len() - 1)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn side_item(
        &self,
        id: impl Into<ElementId>,
        icon_name: &str,
        label: SharedString,
        path: String,
        active: bool,
        trailing: Option<AnyElement>,
        drop: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let p = path.clone();
        let open = std::rc::Rc::new(move |f: &mut Ferret, new_tab: bool, _: &mut Window, cx: &mut Context<Ferret>| {
            if new_tab {
                let at = f.tabs.len();
                f.push_tab(p.clone(), None, Some(at), cx);
                cx.notify();
            } else {
                f.go(p.clone(), cx);
            }
        });
        self.side_item_open(id, icon_name, label, path, active, trailing, drop, open, cx)
    }

    /// A sidebar item that opens with `open` (true: in a new tab, the middle button) instead of
    /// going to `path`: a server connects first and opens where it opens in.
    #[allow(clippy::too_many_arguments)]
    pub fn side_item_open(
        &self,
        id: impl Into<ElementId>,
        icon_name: &str,
        label: SharedString,
        path: String,
        active: bool,
        trailing: Option<AnyElement>,
        drop: bool,
        open: std::rc::Rc<dyn Fn(&mut Ferret, bool, &mut Window, &mut Context<Ferret>)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let ringed = self.side_target(&path);
        let p3 = path.clone();
        let open2 = open.clone();
        div()
            .id(id)
            .role(Role::Link)
            .aria_label(label.clone())
            .when(active, |d| d.aria_description("Open now"))
            .group("side")
            .relative()
            .when(ringed, |d| d.child(ui::ring(7.)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(9.))
            .h(px(30.))
            .px(px(9.))
            .rounded(px(7.))
            .text_color(if active { t::fg() } else { t::fg2() })
            .when(active, |d| d.bg(t::accent_soft()).child(div().absolute().left_0().top(px(8.)).bottom(px(8.)).w(px(3.)).rounded(px(3.)).bg(t::accent())))
            .when(!active, |d| d.hover(|s| s.bg(t::hover()).text_color(t::fg())))
            .on_click(cx.listener(move |f, _, window, cx| open(f, false, window, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, _, window, cx| open2(f, true, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                let items = f.place_items(&p3);
                f.open_menu(ev.position, items, window, cx);
            }))
            .when(drop, |d| {
                let target = path.clone();
                crate::view::drag_target(d, target.clone(), 3, cx)
                    .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()).border_1().border_color(t::accent()))
                    .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| f.drop_paths(paths.paths().to_vec(), target.clone(), window, cx)))
            })
            .child(if active { icon(icon_name, 15., t::fg()) } else { hover_icon(icon_name, 15., t::fg2(), t::fg(), "side") })
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(trailing)
    }

    fn drive_item(&self, d: &Drive, active: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let letter = d.path.chars().next().unwrap_or('?');
        let used = if d.total > 0 { 1. - d.free as f32 / d.total as f32 } else { 0. };
        let label: SharedString = if d.label.is_empty() { "Local Disk".into() } else { d.label.clone().into() };
        let (p1, p2, p3, p4) = (d.path.clone(), d.path.clone(), d.path.clone(), d.path.clone());
        let tip = format!("{} ({letter}:)\n{} free of {}", label, kinds::format_size(d.free), kinds::format_size(d.total));
        let ringed = self.side_target(&d.path);
        div()
            .id(SharedString::from(format!("drive-{letter}")))
            .role(Role::Link)
            .aria_label(format!("{label} ({letter}:), {} free of {}", kinds::format_size(d.free), kinds::format_size(d.total)))
            .when(active, |d| d.aria_description("Open now"))
            .group("drive")
            .relative()
            .when(ringed, |el| el.child(ui::ring(7.)))
            .flex_none()
            .w_full()
            .px(px(9.))
            .pt(px(6.))
            .pb(px(7.))
            .rounded(px(7.))
            .text_color(if active { t::fg() } else { t::fg2() })
            .when(active, |el| el.bg(t::accent_softer()))
            .when(!active, |el| el.hover(|s| s.bg(t::hover()).text_color(t::fg())))
            .tooltip(ui::tip(tip))
            .on_click(cx.listener(move |f, _, _, cx| f.go(p1.clone(), cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(move |f, _, _, cx| {
                let at = f.tabs.len();
                f.push_tab(p2.clone(), None, Some(at), cx);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                let items = f.place_items(&p3);
                f.open_menu(ev.position, items, window, cx);
            }))
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| s.bg(t::accent_soft()))
            .on_drag_move::<gpui::ExternalPaths>({
                let p5 = d.path.clone();
                cx.listener(move |f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    if ev.bounds.contains(&ev.event.position) {
                        f.note_drag_target(ev.event.position, &p5, 3, cx);
                    }
                })
            })
            .on_drop(cx.listener(move |f, paths: &gpui::ExternalPaths, window, cx| f.drop_paths(paths.paths().to_vec(), p4.clone(), window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .child({
                        let name = if d.removable { "usb" } else { "hard-drive" };
                        if active { icon(name, 15., t::fg()) } else { hover_icon(name, 15., t::fg2(), t::fg(), "drive") }
                    })
                    .child(ui::drive_badge(letter, active))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(div().flex_none().text_size(px(11.)).text_color(t::fg3()).whitespace_nowrap().child(format!("{} free", kinds::format_size(d.free)))),
            )
            .child(div().mt(px(6.)).ml(px(24.)).child(ui::bar(used, if used > 0.9 { t::rgba(0xef4444, 1.) } else { t::accent() })))
    }

    /// The Dropzone (Sidebar.tsx `Shelf`): park files here from anywhere, browse elsewhere, then
    /// drag them out. Pinned above Settings, it takes room only when it's of use (the user found
    /// the permanent box too big):
    /// - empty, nothing at all, until files are dragged over the window (from its own list or
    ///   another app): then a drop target, which grows a little under the pointer;
    /// - with items, a compact box: a head with the count, "Drag all" and × (clear), the items,
    ///   and while files are dragged, a slot under them to drop more on.
    fn dropzone(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.shelf.len();
        // Files dragged over the window: GPUI's drag of them, as the window's label follows it
        // (`note_drag`). Moving a tab or a favorite is a drag too, but not of files.
        let dragging = cx.has_active_drag() && self.drag_hint.is_some();
        let over = dragging && self.drag_hint.as_ref().and_then(|h| h.target.as_ref()).is_some_and(|(p, _)| p == "dropzone");
        let anim = self.dz_slot_anim(dragging, over, n == 0);
        if n == 0 && !dragging {
            return None;
        }
        let target = |el: Stateful<Div>, cx: &mut Context<Self>| {
            el.on_drag_move::<gpui::ExternalPaths>(cx.listener(|f, ev: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                if ev.bounds.contains(&ev.event.position) {
                    f.note_drag_target(ev.event.position, "dropzone", 3, cx);
                }
            }))
            .on_drop(cx.listener(|f, paths: &gpui::ExternalPaths, window, cx| f.drop_paths(paths.paths().to_vec(), "dropzone".into(), window, cx)))
        };
        if n == 0 {
            // Only the drop target, as one box.
            let el = target(dz_slot("dropzone", true, over).role(Role::Group).aria_label("Dropzone: drop files here to keep them for later").mx(px(8.)).mb(px(8.)), cx);
            return Some(dz_animate(el, true, anim));
        }
        let all = self.shelf.clone();
        let head = div()
            .id("dz-head")
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(22.))
            .pl(px(3.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t::fg3())
            // Its menu: hide it, the sidebar's settings, and clearing it.
            .on_mouse_down(MouseButton::Right, cx.listener(move |f, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let mut items = Vec::new();
                if !f.shelf.is_empty() {
                    items.push(item("Clear the Dropzone", action(|f, _, cx| f.clear_shelf(cx))).icon("x"));
                    items.push(sep());
                }
                items.push(item("Hide Dropzone", action(|f, _, cx| {
                    f.settings.toggle_in("hiddenSections", "dropzone");
                    cx.notify();
                })).icon("eye-off"));
                items.push(item("Customize sidebar…", action(|f, _, cx| f.open_settings_at(crate::settings_view::SettingsTab::Sidebar, cx))).icon("settings"));
                f.open_menu(ev.position, items, window, cx);
            }))
            .child(icon("inbox", 13., t::fg3()))
            .child("DROPZONE")
            .child(
                div()
                    .flex_none()
                    .px(px(6.))
                    .rounded(px(8.))
                    .bg(t::accent())
                    .text_size(px(10.5))
                    .text_color(t::on_accent())
                    .child(n.to_string()),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("dz-all")
                    .role(Role::Button)
                    .aria_label(format!("Drag all {n} items out"))
                    .flex_none()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(6.))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(t::accent_text())
                    .hover(|s| s.bg(t::accent_soft()))
                    .cursor(gpui::CursorStyle::OpenHand)
                    .tooltip(ui::tip("Drag all items out"))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, _| f.shelf_press = Some((all.clone(), ev.position))))
                    .on_mouse_move(cx.listener(Self::shelf_move))
                    .child("Drag all"),
            )
            .child(
                div()
                    .id("dz-clear")
                    .role(Role::Button)
                    .aria_label("Clear the Dropzone")
                    .group("dz-clear")
                    .flex_none()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .hover(|s| s.bg(t::hover()))
                    .tooltip(ui::tip("Clear the Dropzone"))
                    .on_click(cx.listener(|f, _, _, cx| {
                        cx.stop_propagation();
                        f.clear_shelf(cx);
                    }))
                    .child(hover_icon("x", 12., t::fg3(), t::fg(), "dz-clear")),
            );
        let items = self.shelf.iter().enumerate().map(|(i, p)| {
            let (p1, p2, p3) = (p.clone(), p.clone(), p.clone());
            let name = util::file_name(p);
            // As the app: no extension reads as a folder (no disk access while drawing).
            let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
            let kind = kinds::kind_of(ext.is_empty(), &ext);
            div()
                .id(("dz", i))
                .role(Role::ListItem)
                .aria_label(name.clone())
                .group("dz-item")
                .flex_none()
                .h(px(24.))
                .pl(px(5.))
                .pr(px(3.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded(px(6.))
                .text_color(t::fg2())
                .hover(|s| s.bg(t::press()).text_color(t::fg()))
                .cursor(gpui::CursorStyle::OpenHand)
                .tooltip(ui::tip(format!("{p}\nDrag out to copy/move · double-click to show")))
                .on_click(cx.listener(move |f, ev: &ClickEvent, _, cx| {
                    if ev.click_count() >= 2 {
                        f.shelf_open(&p1, cx);
                    }
                }))
                .on_mouse_down(MouseButton::Left, cx.listener(move |f, ev: &MouseDownEvent, _, _| f.shelf_press = Some((vec![p2.clone()], ev.position))))
                .on_mouse_move(cx.listener(Self::shelf_move))
                .child(icon(kinds::icon(kind), 13., kinds::color(kind)))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(name))
                .child(
                    div()
                        .id(("dz-x", i))
                        .role(Role::Button)
                        .aria_label("Remove from the Dropzone")
                        .flex_none()
                        .p(px(2.))
                        .rounded(px(4.))
                        .invisible()
                        .group_hover("dz-item", |s| s.visible())
                        .hover(|s| s.bg(t::press()))
                        .tooltip(ui::tip("Remove from the Dropzone"))
                        .on_click(cx.listener(move |f, _, _, cx| {
                            cx.stop_propagation();
                            f.shelf.retain(|s| !same(s, &p3));
                            f.save_session();
                            cx.notify();
                        }))
                        .child(icon("x", 12., t::fg3())),
                )
        });
        // About four and a half rows, then it scrolls (the half row says there's more).
        let list = div().id("dz-list").mt(px(2.)).max_h(px(112.)).overflow_y_scroll().flex().flex_col().gap(px(1.)).children(items);
        let el = target(
            div()
                .id("dropzone")
                .role(Role::Group)
                .aria_label(format!("Dropzone, {n} item{}", if n == 1 { "" } else { "s" }))
                .flex_none()
                .mx(px(8.))
                .mb(px(8.))
                .px(px(6.))
                .pt(px(4.))
                .pb(px(6.))
                .rounded(px(10.))
                .border_1()
                .border_color(if over { t::accent() } else { t::line2() })
                .bg(if over { t::accent_softer() } else { t::hover() }),
            cx,
        )
        .child(head)
        .child(list)
        .when(dragging, |d| d.child(dz_animate(dz_slot("dz-slot", false, over).mt(px(4.)), false, anim)));
        Some(el.into_any_element())
    }

    /// The drop slot's animation: its number (a new one each time the slot shows, hides or goes
    /// under the pointer), the height it starts from (the last state's, 0 as it shows) and the
    /// one it ends at.
    fn dz_slot_anim(&self, shown: bool, over: bool, empty: bool) -> (usize, f32, f32) {
        let (base, grown) = if empty { (DZ_EMPTY, DZ_EMPTY + DZ_GROW) } else { (DZ_SLOT, DZ_SLOT + DZ_GROW) };
        let mut s = self.dz_slot.get();
        if s.shown != shown || (shown && s.over != over) {
            s.from = if !s.shown { 0. } else if s.over { grown } else { base };
            s.seq = s.seq.wrapping_add(1);
            s.shown = shown;
            s.over = over;
            self.dz_slot.set(s);
        }
        (s.seq, s.from, if over { grown } else { base })
    }

    fn clear_shelf(&mut self, cx: &mut Context<Self>) {
        self.shelf.clear();
        self.save_session();
        cx.notify();
    }

    /// A Dropzone item double-clicked: as the app's `goTo`, a folder opens, a file is shown in
    /// its folder and opened; one that's gone says so.
    fn shelf_open(&mut self, path: &str, cx: &mut Context<Self>) {
        match std::fs::metadata(ferret_core::paths::for_io(path)) {
            Ok(m) if m.is_dir() => self.go(path.to_string(), cx),
            Ok(_) => {
                self.reveal(path, cx);
                self.open_file(path.to_string(), false, cx);
            }
            Err(_) => self.toast_error(format!("“{}” no longer exists", util::file_name(path)), cx),
        }
    }

    fn shelf_move(&mut self, ev: &gpui::MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((paths, at)) = &self.shelf_press else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.shelf_press = None;
            return;
        }
        if (ev.position.x - at.x).abs().as_f32().max((ev.position.y - at.y).abs().as_f32()) < 6. {
            return;
        }
        let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        self.shelf_press = None;
        let Some(hwnd) = crate::view::hwnd(window).and_then(std::num::NonZeroIsize::new) else { return };
        let hwnd = crate::view::Hwnd(hwnd);
        cx.spawn(async move |_, _| {
            let first = paths[0].to_string_lossy().into_owned();
            let image = ferret_core::shell::thumbnail_png(&first, 96, false).or_else(|_| ferret_core::shell::thumbnail_png(&first, 64, true)).unwrap_or_default();
            // Let go while the picture was being made: no drag. Started anyway, it went on with
            // the button up, as a stray drag that dropped wherever the pointer went next (a real
            // drag test copied a file into another folder that way, 2026-11-11).
            if !crate::files::left_button_held() {
                return;
            }
            let _ = drag::start_drag(&hwnd, drag::DragItem::Files(paths), drag::Image::Raw(image), |_, _| {}, drag::Options::default());
        })
        .detach();
    }

    // ───────────────────────────── menus ─────────────────────────────

    fn favorite_menu(&mut self, path: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fav) = self.favorites.iter().find(|f| f.path == path).cloned() else { return };
        let (p1, p2, p3, p4) = (path.clone(), path.clone(), path.clone(), path.clone());
        let name = fav.name.clone();
        let mut colors = vec![item("No color", action({
            let (p, n) = (path.clone(), name.clone());
            move |f, _, cx| f.color_favorite(p.clone(), n.clone(), None, cx)
        }))
        .checked(fav.color.is_none())];
        for c in FAVORITE_COLORS {
            let (p, n) = (path.clone(), name.clone());
            let hex = format!("#{c:06x}");
            let on = fav.color.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(&hex));
            colors.push(item(hex.clone(), action(move |f, _, cx| f.color_favorite(p.clone(), n.clone(), Some(hex.clone()), cx))).chip(t::rgba(c, 1.)).checked(on));
        }
        let mut items: Vec<MenuItem> = Vec::new();
        if fav.is_dir {
            items.push(item("Open", action(move |f, _, cx| f.go(p1.clone(), cx))).icon("folder-open"));
            items.push(item("Open in new tab", action(move |f, _, cx| f.new_tab(p2.clone(), cx))).icon("plus"));
        } else {
            items.push(item("Open", action(move |f, _, cx| f.open_file(p1.clone(), false, cx))).icon("external-link"));
            items.push(item("Show in folder", action(move |f, _, cx| f.reveal(&p2, cx))).icon("folder-open"));
        }
        items.push(sep());
        items.push(item("Rename…", action(move |f, _, cx| f.rename_favorite(p3.clone(), cx))).icon("text-cursor-input"));
        items.push(crate::menu::submenu("Color", colors).icon("palette"));
        items.push(sep());
        items.push(item("Remove from favorites", action(move |f, _, cx| f.toggle_favorite(p4.clone(), true, cx))).icon("star-off"));
        self.open_menu(at, items, window, cx);
    }

    fn color_favorite(&mut self, path: String, name: String, color: Option<String>, cx: &mut Context<Self>) {
        if let Ok(list) = ferret_core::browse::update_favorite(path, name, color) {
            self.favorites = list;
        }
        cx.notify();
    }

    fn rename_favorite(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(fav) = self.favorites.iter().find(|f| f.path == path).cloned() else { return };
        self.prompt(
            "Rename favorite",
            &fav.path,
            &fav.name,
            "Rename",
            false,
            std::rc::Rc::new(move |f, name, _, cx| {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    if let Ok(list) = ferret_core::browse::update_favorite(path.clone(), name, fav.color.clone()) {
                        f.favorites = list;
                    }
                }
                f.dialog = None;
                cx.notify();
            }),
            cx,
        );
    }

    fn move_favorite(&mut self, path: &str, to: usize, cx: &mut Context<Self>) {
        let mut order: Vec<String> = self.favorites.iter().map(|f| f.path.clone()).collect();
        let Some(from) = order.iter().position(|p| p == path) else { return };
        let p = order.remove(from);
        order.insert(to.min(order.len()), p);
        if let Ok(list) = ferret_core::browse::reorder_favorites(order) {
            self.favorites = list;
        }
        cx.notify();
    }

    fn saved_search_menu(&mut self, query: String, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let path = format!("{}{query}", ferret_core::search::SEARCH_PREFIX);
        let (p1, p2, q1, q2, q3) = (path.clone(), path, query.clone(), query.clone(), query);
        let items = vec![
            item("Open", action(move |f, _, cx| f.go(p1.clone(), cx))).icon("search"),
            item("Open in new tab", action(move |f, _, cx| f.new_tab(p2.clone(), cx))).icon("plus"),
            sep(),
            item("Edit search…", action(move |f, window, cx| f.open_palette(Some(q1.clone()), window, cx))).icon("pencil"),
            item("Rename…", action(move |f, _, cx| f.prompt_save_search(q2.clone(), cx))).icon("text-cursor-input"),
            item("Remove from sidebar", action(move |f, _, cx| f.remove_saved_search(&q3, cx))).icon("x"),
        ];
        self.open_menu(at, items, window, cx);
    }

    pub(crate) fn remove_saved_search(&mut self, query: &str, cx: &mut Context<Self>) {
        let mut list: Vec<serde_json::Value> = self.settings.0["savedSearches"].as_array().cloned().unwrap_or_default();
        list.retain(|s| !s["query"].as_str().is_some_and(|q| q.trim().eq_ignore_ascii_case(query.trim())));
        self.settings.set("savedSearches", serde_json::Value::Array(list));
        cx.notify();
    }

    fn server_menu(&mut self, id: String, connected: bool, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let (c1, c2, i1, i2, i3) = (id.clone(), id.clone(), id.clone(), id.clone(), id);
        let items = vec![
            item(if connected { "Open" } else { "Connect" }, action(move |f, window, cx| f.open_connection(c1.clone(), false, window, cx))).icon("plug"),
            item("Open in new tab", action(move |f, window, cx| f.open_connection(c2.clone(), true, window, cx))).icon("plus"),
            sep(),
            item("Edit…", action(move |f, window, cx| f.open_server_form(Some(i1.clone()), window, cx))).icon("pencil"),
            item("Disconnect", action(move |f, _, cx| {
                let id = i2.clone();
                cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::remote::remote_disconnect(id)) }).detach();
                f.toast("Disconnected", cx);
            }))
            .icon("unplug")
            .disabled(!connected),
            item("Remove…", action(move |f, _, cx| {
                let id = i3.clone();
                // As the app's `removeConnection`: named, and saying what's forgotten.
                f.confirm(
                    format!("Remove “{}”?", ferret_core::remote::name_of(&id)),
                    "Ferret forgets this connection (and a password it remembered for it). Nothing changes on the server, or in ~/.ssh.".into(),
                    "Remove",
                    true,
                    crate::view::action(move |f, _, cx| {
                        let id = id.clone();
                        let _ = ferret_core::rt::block_on(ferret_core::remote::remote_delete_connection(id));
                        cx.notify();
                        let _ = f;
                    }),
                    cx,
                );
            }))
            .icon("trash-2")
            .danger(),
        ];
        self.open_menu(at, items, window, cx);
    }

    fn network_menu(&mut self, path: String, mine: bool, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let mut items = self.place_items(&path);
        items.push(sep());
        let p1 = path.clone();
        items.push(item("Sign out and forget password", action(move |f, _, cx| {
            match ferret_core::network::network_forget(p1.clone()) {
                Ok(()) => f.toast("Signed out", cx),
                Err(e) => f.toast_error(e, cx),
            }
        }))
        .icon("log-out"));
        if mine {
            let p2 = path.clone();
            items.push(item("Remove from sidebar", action(move |f, _, cx| {
                let mut list: Vec<serde_json::Value> = f.settings.0["networkPlaces"].as_array().cloned().unwrap_or_default();
                list.retain(|v| v["path"].as_str().is_none_or(|p| !same(p, &p2)));
                f.settings.set("networkPlaces", serde_json::Value::Array(list));
                cx.notify();
            }))
            .icon("x"));
        }
        self.open_menu(at, items, window, cx);
    }

    /// Computers Windows finds on the network (kept for the next start).
    pub fn scan_network(&mut self, cx: &mut Context<Self>) {
        let task = cx.background_executor().spawn(async { ferret_core::rt::block_on(ferret_core::network::network_computers()) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            this.update(cx, |f, cx| match found {
                Ok(list) => {
                    let v: Vec<serde_json::Value> = list.into_iter().map(|c| json!({ "path": c.path, "name": c.name })).collect();
                    let n = v.len();
                    f.settings.set("networkFound", serde_json::Value::Array(v));
                    f.toast(if n == 0 { "No computers found on the network".to_string() } else { format!("Found {}", plural(n, "computer")) }, cx);
                }
                Err(e) => f.toast_error(e, cx),
            })
            .ok();
        })
        .detach();
    }
}

/// The Dropzone's drop target's heights: alone (an empty Dropzone), under its items, and how much
/// either grows under the pointer.
const DZ_EMPTY: f32 = 38.;
const DZ_SLOT: f32 = 28.;
const DZ_GROW: f32 = 10.;

/// The Dropzone's drop target as last drawn (shown while files are dragged, under the pointer),
/// and its animation (`Ferret::dz_slot_anim`).
#[derive(Clone, Copy, Default, Debug)]
pub struct DzSlot {
    shown: bool,
    over: bool,
    seq: usize,
    from: f32,
}

/// Where files dragged over the window go into the Dropzone: the whole Dropzone while it's empty,
/// a line under its items otherwise. Dashed; under the pointer, solid accent.
fn dz_slot(id: &'static str, empty: bool, over: bool) -> Stateful<Div> {
    let color = if over { t::accent_text() } else { t::fg2() };
    div()
        .id(id)
        .flex_none()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .rounded(px(if empty { 10. } else { 7. }))
        .border_1()
        .when(!over, |d| d.border_dashed())
        .border_color(if over { t::accent() } else { t::accent_line() })
        .bg(if over { t::accent_softer() } else if empty { t::hover() } else { t::rgba(0, 0.) })
        .text_size(px(12.))
        .text_color(color)
        .child(icon(if empty { "inbox" } else { "plus" }, 14., color))
        .child(if empty { "Drop here to keep for later" } else { "Drop to add" })
}

/// The drop target at its height, eased from its last one (`Ferret::dz_slot_anim`): it opens
/// as files come over the window and grows a little under the pointer. Without animations, at
/// once.
fn dz_animate(el: Stateful<Div>, empty: bool, (seq, from, to): (usize, f32, f32)) -> AnyElement {
    let el = el.h(px(to));
    if !t::animate() {
        return el.into_any_element();
    }
    use gpui::AnimationExt;
    el.with_animation(("dz-slot", seq), gpui::Animation::new(std::time::Duration::from_millis(150)).with_easing(gpui::ease_out_quint()), move |d, k| {
        let d = d.h(px(from + (to - from) * k));
        if from > 0. {
            return d;
        }
        // Opening: its margin too, so nothing jumps.
        let d = d.opacity(k);
        if empty { d.mb(px(8. * k)) } else { d.mt(px(4. * k)) }
    })
    .into_any_element()
}

fn hint(text: &'static str) -> gpui::Stateful<Div> {
    crate::ui::note(text, text).px(px(10.)).pt(px(4.)).pb(px(6.)).text_size(px(12.)).text_color(t::fg3())
}

/// An empty section's hint that is also its add button, as the app's `.side-empty.side-add`.
fn add_hint(
    id: &'static str,
    text: &'static str,
    cx: &mut Context<Ferret>,
    run: impl Fn(&mut Ferret, &mut Window, &mut Context<Ferret>) + 'static,
) -> gpui::Stateful<Div> {
    crate::ui::note(id, text)
        .px(px(10.))
        .pt(px(4.))
        .pb(px(6.))
        .text_size(px(12.))
        .text_color(t::fg3())
        .role(Role::Button)
        .aria_label(text)
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(t::hover()).text_color(t::fg()))
        .on_click(cx.listener(move |f, _, window, cx| run(f, window, cx)))
}
