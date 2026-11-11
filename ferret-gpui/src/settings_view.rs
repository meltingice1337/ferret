//! Settings (SettingsDialog.tsx): Appearance, Default view, Remembered views, Behavior,
//! Sidebar, Windows integration, Keyboard shortcuts and About, as cards in one scrolling dialog.

use gpui::{AnyElement, Context, FontWeight, ScrollHandle, SharedString, Window, div, prelude::*, px};
use serde_json::json;

use crate::dialogs::{Dialog, modal, scrim};
use crate::settings::ACCENTS;
use crate::sortgroup::{GroupBy, Mode, SortBy, SortDir, ViewSettings};
use crate::theme as t;
use crate::ui::{self, icon};
use crate::view::Ferret;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettingsTab {
    Top,
    Sidebar,
    /// The add-ons (better text recognition).
    Addons,
    About,
}

const KEYS: &[(&str, &str)] = &[
    ("Ctrl+K or /", "Search everywhere"),
    ("Ctrl+Shift+P", "Command palette"),
    ("Ctrl+L", "Edit path"),
    ("Ctrl+F", "Filter folder"),
    ("Type letters", "Jump to file"),
    ("Alt+← → ↑", "Back / forward / up"),
    ("Backspace", "Up"),
    ("F5", "Refresh"),
    ("Ctrl+T / Ctrl+W", "New / close tab"),
    ("Ctrl+Shift+T", "Reopen closed tab"),
    ("Ctrl+Tab", "Next tab"),
    ("Ctrl+\\", "Split view (two folders side by side)"),
    ("Tab", "Other pane (in split view)"),
    ("Ctrl+N", "New window"),
    ("Space", "Quick Look"),
    ("Alt+P", "Preview pane"),
    ("F2", "Rename"),
    ("Ctrl+Shift+N", "New folder"),
    ("Del / Shift+Del", "Recycle / delete permanently"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("Alt+Enter", "Properties"),
    ("Ctrl+Shift+C", "Copy path"),
    ("Ctrl+D", "Pin the selected item (or this folder) to favorites"),
    ("Ctrl+1 / Ctrl+2", "List / grid"),
    ("Ctrl+G", "Cycle grouping"),
    ("← / →", "Collapse / expand a group"),
    ("Ctrl+H", "Show hidden"),
    ("Ctrl+wheel", "Grid icon size"),
    ("Ctrl+,", "Settings"),
];

const SECTIONS: &[(&str, &str)] = &[
    ("favorites", "Favorites"),
    ("searches", "Saved searches"),
    ("places", "Places"),
    ("linux", "Linux (WSL)"),
    ("servers", "Servers"),
    ("network", "Network"),
    ("drives", "Drives"),
    ("recents", "Recent"),
    ("dropzone", "Dropzone"),
];

impl Ferret {
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.open_settings_at(SettingsTab::Top, cx);
    }

    pub fn open_settings_at(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        self.menu = None;
        self.settings_scroll = ScrollHandle::new();
        self.folder_views = ferret_core::browse::list_folder_views().unwrap_or_default();
        self.default_status = Some(ferret_core::integration::default_manager_status());
        self.dialog = Some(Dialog::Settings);
        let target = match tab {
            SettingsTab::Top => None,
            // The cards' places in the body (`settings_view`).
            SettingsTab::Sidebar => Some(4),
            SettingsTab::Addons => Some(6),
            SettingsTab::About => Some(8),
        };
        if let Some(ix) = target {
            let handle = self.settings_scroll.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
                handle.scroll_to_item(ix);
                let _ = cx.update(|cx| cx.refresh_windows());
            })
            .detach();
        }
        cx.notify();
    }

    pub fn settings_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if key == "escape" {
            self.close_dialog(window, cx);
            return true;
        }
        false
    }

    /// Changes a setting, saves it and applies what it changes.
    fn put(&mut self, key: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        self.settings.set(key, value);
        self.settings.apply_theme();
        if matches!(key, "density") {
            for t in &mut self.tabs {
                t.main.relayout();
                if let Some(s) = t.split.as_mut() {
                    s.relayout();
                }
            }
        }
        cx.refresh_windows();
        cx.notify();
    }

    fn set_global_view(&mut self, change: impl FnOnce(&mut ViewSettings), cx: &mut Context<Self>) {
        let mut v = self.settings.global_view();
        change(&mut v);
        self.settings.0["globalView"] = v.to_json();
        self.settings.save();
        // Folders using the default view take the new one.
        self.reload_all(cx);
        cx.notify();
    }

    pub fn settings_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let s = &self.settings;
        let theme = s.str("theme").to_string();
        let backdrop = s.str("backdrop").to_string();
        let density = s.str("density").to_string();
        let accent = s.accent();
        let gv = s.global_view();
        let animations = s.bool("animations", true);
        let remember = s.bool("rememberPerFolder", true);
        let inherit = s.bool("inheritToSubfolders", false);
        let dbl = s.bool("doubleClickOpen", true);
        let restore = s.bool("restoreSession", true);
        let sizes = s.bool("folderSizes", true);
        let profiles: Vec<(&str, &str, &str, bool)> = vec![
            ("media", "Media", "Folders mostly of pictures and videos: grid, newest first, grouped by date", s.0["profiles"]["media"]["enabled"].as_bool().unwrap_or(true)),
            ("code", "Code", "Projects (.git, package.json, Cargo.toml…): list by name, hidden items shown", s.0["profiles"]["code"]["enabled"].as_bool().unwrap_or(true)),
            ("downloads", "Downloads", "The Downloads folder: newest first, grouped by date", s.0["profiles"]["downloads"]["enabled"].as_bool().unwrap_or(true)),
        ];
        let places: Vec<(String, String)> = self.places.known.iter().map(|k| (k.id.to_string(), k.name.to_string())).chain(std::iter::once(("recycle".to_string(), "Recycle Bin".to_string()))).collect();

        let card = |title: &'static str, desc: Option<&'static str>| {
            div()
                .id(title)
                .role(gpui::Role::Group)
                .aria_label(title)
                .when_some(desc, |d, t| d.aria_description(t))
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(16.))
                .rounded(px(12.))
                .bg(t::hover())
                .border_1()
                .border_color(t::line())
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                .children(desc.map(|d| div().text_size(px(12.)).text_color(t::fg3()).child(d)))
        };
        // A setting's row: to a screen reader, a group named by its label (and hint) holding
        // the switch or choice.
        let field = |label: &'static str, hint: Option<&'static str>, control: AnyElement| {
            div()
                .id(label)
                .role(gpui::Role::Group)
                .aria_label(label)
                .when_some(hint, |d, h| d.aria_description(h))
                .flex()
                .items_center()
                .gap(px(16.))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().child(label)).children(hint.map(|h| div().text_size(px(11.5)).text_color(t::fg3()).child(h))))
                .child(div().flex_none().flex().items_center().gap(px(8.)).child(control))
        };
        let seg = |id: &'static str, options: &[(&'static str, &'static str)], value: &str, cx: &mut Context<Self>, on: std::rc::Rc<dyn Fn(&mut Ferret, &'static str, &mut Context<Ferret>)>| {
            ui::seg()
                .children(options.iter().enumerate().map(|(i, (v, label))| {
                    let (v, on) = (*v, on.clone());
                    ui::seg_btn((id, i), None, Some(SharedString::from(*label)), value == v).h(px(28.)).px(px(12.)).on_click(cx.listener(move |f, _, _, cx| on(f, v, cx)))
                }))
                .into_any_element()
        };
        let toggle = |label: &'static str, id: &'static str, on: bool, cx: &mut Context<Self>, run: std::rc::Rc<dyn Fn(&mut Ferret, bool, &mut Context<Ferret>)>| {
            ui::toggle(id, on).aria_label(label).on_click(cx.listener(move |f, _, _, cx| run(f, !on, cx))).into_any_element()
        };

        // Appearance
        let mut swatches = div().flex().flex_wrap().gap(px(8.));
        for (i, (name, hex)) in ACCENTS.iter().enumerate() {
            let on = accent == *hex;
            let hex = *hex;
            swatches = swatches.child(
                div()
                    .id(("swatch", i))
                    .role(gpui::Role::RadioButton)
                    .aria_label(*name)
                    .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
                    .size(px(26.))
                    .rounded(px(13.))
                    .bg(t::rgba(hex, 1.))
                    .border_2()
                    .border_color(if on { t::fg() } else { t::rgba(0, 0.) })
                    .tooltip(ui::tip(*name))
                    .on_click(cx.listener(move |f, _, _, cx| f.put("accent", json!(format!("#{hex:06x}")), cx))),
            );
        }
        let custom = !ACCENTS.iter().any(|(_, h)| *h == accent);
        swatches = swatches.child(
            div()
                .id("swatch-custom")
                .role(gpui::Role::Button)
                .aria_label("Custom color")
                .size(px(26.))
                .rounded(px(13.))
                .flex()
                .items_center()
                .justify_center()
                .bg(if custom { t::rgba(accent, 1.) } else { t::press() })
                .border_2()
                .border_color(if custom { t::fg() } else { t::line2() })
                .tooltip(ui::tip("Custom color"))
                .on_click(cx.listener(|f, _, _, cx| {
                    let cur = format!("#{:06x}", f.settings.accent());
                    f.prompt(
                        "Custom accent color",
                        "A color as #rrggbb.",
                        &cur,
                        "Use it",
                        false,
                        std::rc::Rc::new(|f, text, _, cx| {
                            if let Some(hex) = t::parse_hex(&text) {
                                f.dialog = None;
                                f.put("accent", json!(format!("#{hex:06x}")), cx);
                                f.open_settings(cx);
                            } else if let Some(Dialog::Prompt { error, .. }) = f.dialog.as_mut() {
                                *error = Some("That isn't a color like #e2457f".into());
                                cx.notify();
                            }
                        }),
                        cx,
                    );
                }))
                .child(icon("palette", 13., if custom { t::on_accent() } else { t::fg2() })),
        );
        let appearance = card("Appearance", None)
            .child(field("Accent color", None, swatches.into_any_element()))
            .child(field("Theme", None, seg("theme", &[("dark", "Dark"), ("light", "Light"), ("auto", "System")], &theme, cx, std::rc::Rc::new(|f, v, cx| f.put("theme", json!(v), cx)))))
            .child(field(
                "Window background",
                Some("Mica / Acrylic need Windows 11"),
                seg("backdrop", &[("mica", "Mica"), ("acrylic", "Acrylic"), ("solid", "Solid")], &backdrop, cx, std::rc::Rc::new(|f, v, cx| {
                    f.put("backdrop", json!(v), cx);
                    f.backdrop_changed = true;
                })),
            ))
            .child(field("Density", None, seg("density", &[("comfortable", "Comfortable"), ("compact", "Compact")], &density, cx, std::rc::Rc::new(|f, v, cx| f.put("density", json!(v), cx)))))
            .child(field("Animations", Some("Also off when Windows' animation effects are off"), toggle("Animations", "anim", animations, cx, std::rc::Rc::new(|f, v, cx| f.put("animations", json!(v), cx)))));

        // Default view
        let sort_opts: &[(&'static str, &'static str)] = &[("name", "Name"), ("modified", "Modified"), ("created", "Created"), ("size", "Size"), ("type", "Type")];
        let group_opts: &[(&'static str, &'static str)] = &[("none", "None"), ("date", "Date"), ("type", "Type"), ("size", "Size"), ("letter", "Letter")];
        let sort_now = gv.to_json()["sortBy"].as_str().unwrap_or("name").to_string();
        let group_now = gv.to_json()["groupBy"].as_str().unwrap_or("none").to_string();
        let default_view = card("Default view", Some("Used for every folder that has no remembered view or matching profile."))
            .child(field("Layout", None, seg("gv-mode", &[("list", "List"), ("grid", "Grid")], if gv.mode == Mode::Grid { "grid" } else { "list" }, cx, std::rc::Rc::new(|f, v, cx| f.set_global_view(|g| g.mode = if v == "grid" { Mode::Grid } else { Mode::List }, cx)))))
            .child(field("Sort by", None, seg("gv-sort", sort_opts, &sort_now, cx, std::rc::Rc::new(|f, v, cx| {
                f.set_global_view(|g| g.sort_by = match v { "modified" => SortBy::Modified, "created" => SortBy::Created, "size" => SortBy::Size, "type" => SortBy::Type, _ => SortBy::Name }, cx)
            }))))
            .child(field("Direction", None, seg("gv-dir", &[("asc", "Asc"), ("desc", "Desc")], if gv.sort_dir == SortDir::Desc { "desc" } else { "asc" }, cx, std::rc::Rc::new(|f, v, cx| f.set_global_view(|g| g.sort_dir = if v == "desc" { SortDir::Desc } else { SortDir::Asc }, cx)))))
            .child(field("Group by", None, seg("gv-group", group_opts, &group_now, cx, std::rc::Rc::new(|f, v, cx| {
                f.set_global_view(|g| g.group_by = match v { "date" => GroupBy::Date, "type" => GroupBy::Type, "size" => GroupBy::Size, "letter" => GroupBy::Letter, _ => GroupBy::None }, cx)
            }))))
            .child(field("Folders first", None, toggle("Folders first", "gv-ff", gv.folders_first, cx, std::rc::Rc::new(|f, v, cx| f.set_global_view(|g| g.folders_first = v, cx)))))
            .child(field("Show hidden items", None, toggle("Show hidden items", "gv-hidden", crate::pane::show_hidden(), cx, std::rc::Rc::new(|f, v, cx| f.set_show_hidden(v, cx)))));

        // Remembered views
        let mut remembered = card("Remembered views", Some("Order of priority: this folder's own view → a parent applied to subfolders → folder-type profile → default view."))
            .child(field("Remember view per folder", Some("Changing sort, grouping or layout in a folder saves it for that folder"), toggle("Remember view per folder", "rem", remember, cx, std::rc::Rc::new(|f, v, cx| f.put("rememberPerFolder", json!(v), cx)))))
            .child(field("Subfolders inherit", Some("Folders without their own view use their nearest parent's"), toggle("Subfolders inherit", "inh", inherit, cx, std::rc::Rc::new(|f, v, cx| {
                f.put("inheritToSubfolders", json!(v), cx);
                f.reload_all(cx);
            }))));
        for (id, name, desc, on) in profiles {
            let pid = id.to_string();
            let label: &'static str = Box::leak(format!("{name} profile").into_boxed_str());
            remembered = remembered.child(field(
                label,
                Some(desc),
                toggle(label, Box::leak(format!("prof-{id}").into_boxed_str()), on, cx, std::rc::Rc::new(move |f, v, cx| {
                    f.settings.0["profiles"][&pid]["enabled"] = json!(v);
                    f.settings.save();
                    f.reload_all(cx);
                    cx.notify();
                })),
            ));
        }
        // The folders with a view of their own: one folded line (none, nothing), the list only
        // when asked, so a long one doesn't fill Settings. Folded again in a new window.
        let n = self.folder_views.len();
        let open = self.folder_views_open && n > 0;
        let count = format!("{n} folder{}", if n == 1 { "" } else { "s" });
        remembered = remembered.when(n > 0, |card| {
            card.child(
                div()
                    .id("views-row")
                    .role(gpui::Role::Group)
                    .aria_label(format!("Remembered folder views, {count}"))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_none().child("Remembered folder views"))
                            .child(div().min_w_0().truncate().text_color(t::fg3()).child(format!("· {count}"))),
                    )
                    .child(
                        div()
                            .id("views-toggle")
                            .role(gpui::Role::Button)
                            .aria_label("Remembered folder views")
                            .aria_expanded(open)
                            .group("views-toggle")
                            .flex_none()
                            .h(px(28.))
                            .pl(px(10.))
                            .pr(px(7.))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .rounded(px(7.))
                            .text_size(px(12.5))
                            .text_color(t::fg2())
                            .hover(|s| s.bg(t::hover()).text_color(t::fg()))
                            .on_click(cx.listener(|f, _, _, cx| {
                                f.folder_views_open = !f.folder_views_open;
                                cx.notify();
                            }))
                            .child(if open { "Hide" } else { "Show" })
                            .child(ui::hover_icon(if open { "chevron-down" } else { "chevron-right" }, 13., t::fg3(), t::fg(), "views-toggle")),
                    )
                    .child(ui::button("views-clear", "Forget all", false).h(px(28.)).px(px(12.)).text_color(t::danger()).on_click(cx.listener(|f, _, _, cx| {
                        let _ = ferret_core::browse::clear_all_folder_views();
                        f.folder_views.clear();
                        f.folder_views_open = false;
                        f.reload_all(cx);
                        f.toast("Forgot all remembered folder views", cx);
                    }))),
            )
        });
        remembered = remembered.when(open, |card| card.child(div().id("views-list").max_h(px(300.)).overflow_y_scroll().flex().flex_col().gap(px(4.)).children(self.folder_views.iter().enumerate().map(|(i, v)| {
            let path = v.path.clone();
            let vs = ViewSettings::from_json(&v.view);
            let summary = format!(
                "{} · {:?} {}{}{}",
                if vs.mode == Mode::Grid { "grid" } else { "list" },
                vs.sort_by,
                if vs.sort_dir == SortDir::Desc { "↓" } else { "↑" },
                if vs.group_by != GroupBy::None { format!(" · grouped by {:?}", vs.group_by).to_lowercase() } else { String::new() },
                if v.recursive { " · applies to subfolders" } else { "" }
            );
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(8.))
                .bg(t::hover())
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().truncate().child(v.path.clone())).child(div().text_size(px(11.5)).text_color(t::fg3()).child(summary)))
                .child(ui::icon_btn(("forget", i), "trash-2", true).tooltip(ui::tip("Forget")).on_click(cx.listener(move |f, _, _, cx| {
                    let _ = ferret_core::browse::clear_folder_view(path.clone());
                    f.folder_views = ferret_core::browse::list_folder_views().unwrap_or_default();
                    f.reload_all(cx);
                })))
        }))));

        // Behavior
        let behavior = card("Behavior", None)
            .child(field("Open items with", None, seg("open-with", &[("double", "Double-click"), ("single", "Single click")], if dbl { "double" } else { "single" }, cx, std::rc::Rc::new(|f, v, cx| f.put("doubleClickOpen", json!(v == "double"), cx)))))
            .child(field("Restore tabs on launch", None, toggle("Restore tabs on launch", "restore", restore, cx, std::rc::Rc::new(|f, v, cx| f.put("restoreSession", json!(v), cx)))))
            .child(field("Folder sizes", Some("Counted in the background at low priority and shown in the Size column (not on network shares)"), toggle("Folder sizes", "sizes", sizes, cx, std::rc::Rc::new(|f, v, cx| f.put("folderSizes", json!(v), cx)))));

        // Sidebar
        let mut place_grid = div().flex().flex_wrap().gap(px(8.));
        for (id, name) in places {
            let shown = !self.settings.hidden_place(&id);
            let id2 = id.clone();
            place_grid = place_grid.child(
                div()
                    .w(px(220.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .bg(t::hover())
                    .child(name.clone())
                    .child(ui::toggle(SharedString::from(format!("pl-{id}")), shown).aria_label(format!("Show {name}")).on_click(cx.listener(move |f, _, _, cx| {
                        f.settings.toggle_in("hiddenPlaces", &id2);
                        cx.notify();
                    }))),
            );
        }
        let mut sec_grid = div().flex().flex_wrap().gap(px(8.));
        for (id, name) in SECTIONS {
            let shown = !self.settings.hidden_section(id);
            sec_grid = sec_grid.child(
                div()
                    .w(px(220.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .bg(t::hover())
                    .child(*name)
                    .child(ui::toggle(SharedString::from(format!("sec-t-{id}")), shown).aria_label(format!("Show {name}")).on_click(cx.listener(move |f, _, _, cx| {
                        f.settings.toggle_in("hiddenSections", id);
                        cx.notify();
                    }))),
            );
        }
        let sidebar = card("Sidebar", Some("Choose what the sidebar shows. You can also right-click a place or a section title to hide it."))
            .child(place_grid)
            .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(t::fg2()).child("Sections"))
            .child(sec_grid);

        // Windows integration
        let status = self.default_status.clone();
        let enabled = status.as_ref().is_some_and(|s| s.enabled);
        let dev = status.as_ref().is_some_and(|s| s.dev_build);
        // Folders may open in another copy of Ferret (the installed app). The registry keys are
        // shared, and that copy holds the backup of how Explorer was: switching it off from here
        // would undo its setup without putting Explorer back properly. It's changed over there.
        let this_exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_lowercase());
        let other = status.as_ref().and_then(|s| s.target.clone()).filter(|t| Some(t.to_lowercase()) != this_exe);
        let control = match &other {
            Some(_) => ui::badge("Another copy").into_any_element(),
            None => toggle("Use Ferret instead of File Explorer", "default-fm", enabled, cx, std::rc::Rc::new(|f, v, cx| match ferret_core::integration::set_default_manager(v) {
                Ok(s) => {
                    f.default_status = Some(s);
                    f.toast(if v { "Ferret now opens folders and Win+E" } else { "Explorer is the default again" }, cx);
                }
                Err(e) => f.toast_error(e, cx),
            }))
            .into_any_element(),
        };
        let integration = card("Windows integration", None)
            .child(field(
                "Use Ferret instead of File Explorer",
                Some("Win+E, folders opened from the desktop and other apps open in Ferret. Turning it off puts Explorer back exactly as it was."),
                control,
            ))
            .when_some(other, |d, target| {
                d.child(ui::note("fm-other", format!("Folders open in another copy of Ferret ({target}). Change this setting there.")).text_size(px(12.)).text_color(t::fg3()))
            })
            .when(dev && !enabled, |d| d.child(ui::note("fm-dev", "This copy is a development or test build, run from the project folder. Turn this on from the installed Ferret.").text_size(px(12.)).text_color(t::fg3())))
            .child(ui::note("fm-limits", "Windows still uses Explorer for the taskbar, the desktop, file Open/Save dialogs and some apps' “Show in folder”.").text_size(px(12.)).text_color(t::fg3()));

        // Add-ons: parts downloaded only when asked (addons.rs).
        use crate::addons::OcrPack;
        let pack = crate::addons::ocr_pack();
        let (status, status_color) = match &pack {
            OcrPack::Missing => ("Not installed".to_string(), t::fg3()),
            OcrPack::Downloading { got, total } if *total > 0 && got >= total => ("Installing…".to_string(), t::fg2()),
            OcrPack::Downloading { got, total } if *total > 0 => (format!("Downloading {:.0}%", *got as f64 * 100. / *total as f64), t::fg2()),
            OcrPack::Downloading { .. } => ("Downloading…".to_string(), t::fg2()),
            OcrPack::Ready(_) => (crate::addons::ocr_pack_size().map(|n| format!("Installed · {}", crate::kinds::format_size(n))).unwrap_or_else(|| "Installed".into()), t::ok()),
            // Said in full on the line below.
            OcrPack::Failed(_) => (String::new(), t::danger()),
            OcrPack::RemoveAtRestart => ("Removes when Ferret restarts".to_string(), t::warn()),
        };
        let action = match &pack {
            OcrPack::Missing | OcrPack::Failed(_) => ui::button("ocr-get", if matches!(pack, OcrPack::Missing) { "Download" } else { "Try again" }, true).on_click(cx.listener(|_, _, _, cx| {
                crate::addons::start_ocr_download(cx);
                cx.notify();
            })),
            OcrPack::Downloading { .. } => ui::button("ocr-cancel", "Cancel", false).on_click(cx.listener(|_, _, _, cx| {
                crate::addons::cancel_ocr_download();
                cx.notify();
            })),
            OcrPack::Ready(_) => ui::button("ocr-remove", "Remove", false).on_click(cx.listener(|f, _, _, cx| match crate::addons::remove_ocr_pack() {
                Ok(true) => f.toast("Better text recognition removed", cx),
                Ok(false) => f.toast("Better text recognition is in use: it will be removed when Ferret restarts", cx),
                Err(e) => f.toast_error(format!("Couldn't remove better text recognition: {e}"), cx),
            })),
            OcrPack::RemoveAtRestart => ui::button("ocr-keep", "Keep it", false).on_click(cx.listener(|_, _, _, cx| {
                crate::addons::keep_ocr_pack();
                cx.notify();
            })),
        };
        let addons = card("Add-ons", Some("Optional parts Ferret downloads only when you ask, checked against Ferret's signature."))
            .child(field(
                "Better text recognition for scanned PDFs",
                Some(crate::addons::ocr_pack_blurb()),
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .when(!status.is_empty(), |d| d.child(ui::note("ocr-status", status).text_size(px(12.)).text_color(status_color)))
                    .child(action)
                    .into_any_element(),
            ))
            .when_some(match &pack {
                OcrPack::Downloading { got, total } => Some(if *total > 0 { *got as f32 / *total as f32 } else { 0. }),
                _ => None,
            }, |d, frac| d.child(ui::bar(frac, t::accent())))
            .when_some(match &pack {
                OcrPack::Failed(why) => Some(why.clone()),
                _ => None,
            }, |d, why| d.child(ui::note("ocr-failed", format!("Failed: {why}")).text_size(px(12.)).text_color(t::danger())));

        // Shortcuts
        let shortcuts = card("Keyboard shortcuts", None).child(div().flex().flex_wrap().gap_y(px(6.)).children(KEYS.iter().map(|(k, d)| {
            // A long description wraps within its column (it ran into the next one's keys).
            div().w(px(330.)).pr(px(10.)).flex().items_center().gap(px(10.)).child(div().w(px(120.)).flex_none().child(ui::kbd(*k))).child(div().flex_1().min_w_0().text_color(t::fg2()).text_size(px(12.5)).child(*d))
        })));

        // About
        let installed = crate::appdata::installed();
        let update_line = self.update_status();
        let checking = matches!(self.update.phase, crate::updateview::Phase::Checking | crate::updateview::Phase::Downloading | crate::updateview::Phase::Installing);
        let waiting = self.update_waiting().is_some();
        let about = card("About", None)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(ui::logo(56.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .items_center()
                                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Ferret"))
                                    .child(div().text_color(t::fg3()).child(env!("CARGO_PKG_VERSION")))
                                    .when(!installed, |d| d.child(ui::badge("DEV"))),
                            )
                            .child(ui::note("about-desc", "A fast, keyboard-first file manager for Windows.").text_size(px(12.)).text_color(t::fg3())),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("about-tour", "Intro tour", false).on_click(cx.listener(|f, _, window, cx| {
                                f.close_dialog(window, cx);
                                f.start_intro(true, cx);
                            })))
                            .child(ui::button("about-logs", "Open logs", false).on_click(cx.listener(|f, _, window, cx| {
                                f.close_dialog(window, cx);
                                let dir = crate::appdata::log_dir().to_string_lossy().into_owned();
                                f.new_tab(dir, cx);
                            }))),
                    ),
            )
            // Updates: what it knows, and asking now (as the app's Settings).
            .child(
                div()
                    .mt(px(12.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(t::line())
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(if waiting { "sparkles" } else { "refresh-cw" }, 15., if waiting { t::accent_text() } else { t::fg3() }))
                    .child(ui::note("about-update", update_line).flex_1().min_w_0().text_color(t::fg2()))
                    .child(if waiting {
                        ui::button("about-whats-new", "What's new", true).on_click(cx.listener(|f, _, _, cx| f.show_whats_new(cx)))
                    } else {
                        ui::button("about-check", if checking { "Checking…" } else { "Check for updates" }, false)
                            .when(checking, |d| d.opacity(0.6))
                            .on_click(cx.listener(|f, _, _, cx| f.check_for_update(true, cx)))
                    }),
            );

        let body = div()
            .id("settings-body")
            .track_scroll(&self.settings_scroll)
            .max_h(px((window.viewport_size().height.as_f32() - 140.).max(300.)))
            .overflow_y_scroll()
            .px(px(22.))
            .py(px(16.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(appearance)
            .child(default_view)
            .child(remembered)
            .child(behavior)
            .child(sidebar)
            .child(integration)
            .child(addons)
            .child(shortcuts)
            .child(about);
        let card = modal("Settings".into(), 760., cx).child(body).into_any_element();
        scrim(card, cx)
    }
}
