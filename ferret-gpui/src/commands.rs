//! The command palette's list (Ctrl+Shift+P, or `>` in Ctrl+K): App.tsx `paletteCommands`, each
//! with its keys and a hint.

use gpui::SharedString;

use crate::palette::Command;
use crate::sortgroup::{GroupBy, Mode, SortBy, SortDir};
use crate::util::same;
use crate::view::{Action, Ferret, action};

impl Ferret {
    pub fn commands(&self) -> Vec<(Command, Action)> {
        fn cmd(label: impl Into<SharedString>, keys: Option<&'static str>, hint: Option<String>, run: Action) -> (Command, Action) {
            (Command { label: label.into(), keys, hint: hint.map(Into::into) }, run)
        }
        let p = self.pane();
        let split = self.tab().split.is_some();
        let other = self.tab().other().map(|o| o.path.clone());
        let fav = self.favorites.iter().any(|f| same(&f.path, &p.path));
        let mut out = vec![
            cmd("Search everywhere", Some("Ctrl+K"), None, action(|f, window, cx| f.open_palette(None, window, cx))),
            cmd("New tab", Some("Ctrl+T"), None, action(|f, _, cx| {
                let p = f.pane().path.clone();
                f.new_tab(p, cx);
            })),
            cmd("Close tab", Some("Ctrl+W"), None, action(|f, _, cx| f.close_tab(f.active, cx))),
            cmd(if split { "Close split view" } else { "Split view (two folders side by side)" }, Some("Ctrl+\\"), None, action(|f, _, cx| f.toggle_split(cx))),
        ];
        if let Some(o) = other {
            out.push(cmd("Switch pane", Some("Tab"), None, action(|f, _, cx| f.switch_pane(cx))));
            out.push(cmd("Copy to the other pane", None, Some(o.clone()), action(|f, window, cx| f.to_other_pane("copy", window, cx))));
            out.push(cmd("Move to the other pane", None, Some(o), action(|f, window, cx| f.to_other_pane("move", window, cx))));
        }
        out.extend([
            cmd("Reopen closed tab", Some("Ctrl+Shift+T"), None, action(|f, _, cx| f.reopen_tab(cx))),
            cmd("New folder", Some("Ctrl+Shift+N"), None, action(|f, _, cx| f.new_folder(cx))),
            cmd("New text document", None, None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.new_file(d, "New Text Document.txt", None, cx);
            })),
            cmd("New Markdown file", None, None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.new_file(d, "New Document.md", None, cx);
            })),
            cmd("Paste", Some("Ctrl+V"), None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.paste(d, cx);
            })),
            cmd("Undo", Some("Ctrl+Z"), self.undo.last().map(|s| s.label.clone()), action(|f, _, cx| f.undo(cx))),
            cmd("Redo", Some("Ctrl+Y"), self.redo.last().map(|s| s.label.clone()), action(|f, _, cx| f.redo(cx))),
            cmd("Refresh", Some("F5"), None, action(|f, _, cx| f.refresh(cx))),
            cmd("Go up", Some("Alt+↑"), None, action(|f, _, cx| f.go_up(cx))),
            cmd("Edit path", Some("Ctrl+L"), None, action(|f, window, cx| f.edit_path(window, cx))),
            cmd("Filter this folder", Some("Ctrl+F"), None, action(|f, window, cx| f.open_filter(window, cx))),
            cmd(if crate::pane::show_hidden() { "Hide hidden items" } else { "Show hidden items" }, Some("Ctrl+H"), None, action(|f, _, cx| f.toggle_hidden(cx))),
            cmd("View: list", Some("Ctrl+1"), None, action(|f, _, cx| f.set_mode(Mode::List, cx))),
            cmd("View: grid", Some("Ctrl+2"), None, action(|f, _, cx| f.set_mode(Mode::Grid, cx))),
            cmd("Group by date", None, None, action(|f, _, cx| f.set_view(|v| v.group_by = GroupBy::Date, cx))),
            cmd("Group by type", None, None, action(|f, _, cx| f.set_view(|v| v.group_by = GroupBy::Type, cx))),
            cmd("Group by size", None, None, action(|f, _, cx| f.set_view(|v| v.group_by = GroupBy::Size, cx))),
            cmd("No grouping", None, None, action(|f, _, cx| f.set_view(|v| v.group_by = GroupBy::None, cx))),
            cmd("Sort by name", None, None, action(|f, _, cx| f.set_view(|v| { v.sort_by = SortBy::Name; v.sort_dir = SortDir::Asc; }, cx))),
            cmd("Sort by date modified", None, None, action(|f, _, cx| f.set_view(|v| { v.sort_by = SortBy::Modified; v.sort_dir = SortDir::Desc; }, cx))),
            cmd("Sort by size", None, None, action(|f, _, cx| f.set_view(|v| { v.sort_by = SortBy::Size; v.sort_dir = SortDir::Desc; }, cx))),
            cmd("Reset view to default", None, None, action(|f, _, cx| f.reset_view(cx))),
            cmd("Toggle preview pane", Some("Alt+P"), None, action(|f, window, cx| f.toggle_preview(window, cx))),
            cmd("Toggle sidebar", Some("Ctrl+B"), None, action(|f, window, cx| f.toggle_sidebar(window, cx))),
            cmd("Open in Terminal", Some("Ctrl+`"), None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.open_terminal(d, cx);
            })),
            cmd("Open in VS Code", None, None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.open_vscode(d, cx);
            })),
            cmd("New window", Some("Ctrl+N"), None, action(|f, _, cx| {
                let d = f.pane().path.clone();
                f.new_window(Some(d), cx);
            })),
        ]);
        if !self.elevated {
            out.push(cmd("Run this window as administrator", None, Some("Same tabs, after a UAC prompt".into()), action(|f, window, cx| f.restart_elevated(window, cx))));
        }
        out.push(cmd("Copy folder path", None, None, action(|f, _, cx| {
            let d = f.pane().path.clone();
            f.copy_paths(std::slice::from_ref(&d), false, cx);
        })));
        out.push(cmd(if fav { "Remove folder from favorites" } else { "Add folder to favorites" }, Some("Ctrl+D"), None, action(|f, _, cx| {
            let d = f.pane().path.clone();
            f.toggle_favorite(d, true, cx);
        })));
        if self.video.is_some() || p.marked_rows().iter().any(|r| r.kind == crate::kinds::Kind::Video) {
            out.push(cmd("Compress videos…", None, None, action(|f, window, cx| f.open_batch_editor(window, cx))));
        }
        for k in &self.places.known {
            let path = k.path.clone();
            out.push(cmd(format!("Go to {}", k.name), None, Some(k.path.clone()), action(move |f, _, cx| f.go(path.clone(), cx))));
        }
        for d in &self.places.drives {
            let path = d.path.clone();
            let label = if d.label.is_empty() { "Local Disk".to_string() } else { d.label.clone() };
            out.push(cmd(format!("Go to {label} ({})", d.path.trim_end_matches('\\')), None, None, action(move |f, _, cx| f.go(path.clone(), cx))));
        }
        for d in &self.places.wsl {
            let path = d.path.clone();
            out.push(cmd(format!("Go to {} (WSL)", d.name), None, None, action(move |f, _, cx| f.go(path.clone(), cx))));
        }
        let bin = self.bin.map(|b| b.items).unwrap_or(0);
        out.extend([
            cmd("Go to Recycle Bin", None, Some(if bin > 0 { format!("{bin} items") } else { "empty".into() }), action(|f, _, cx| f.go(ferret_core::recycle::PATH.into(), cx))),
            cmd("Empty Recycle Bin", None, None, action(|f, _, cx| f.empty_bin(cx))),
            cmd("Theme: dark", None, None, action(|f, _, cx| f.set_theme("dark", cx))),
            cmd("Theme: light", None, None, action(|f, _, cx| f.set_theme("light", cx))),
            cmd("Settings", Some("Ctrl+,"), None, action(|f, _, cx| f.open_settings(cx))),
            cmd("Tutorial: intro tour of what Ferret can do", None, None, action(|f, _, cx| f.start_intro(true, cx))),
            cmd("Add a network folder or NAS", None, None, action(|f, window, cx| f.open_network_form(window, cx))),
            cmd("Look for computers on the network", None, None, action(|f, _, cx| f.scan_network(cx))),
        ]);
        out
    }

    pub fn set_theme(&mut self, theme: &str, cx: &mut gpui::Context<Self>) {
        self.settings.set("theme", serde_json::json!(theme));
        self.settings.apply_theme();
        cx.refresh_windows();
        cx.notify();
    }

    /// Split view: the selection copied or moved into the other pane's folder.
    pub fn to_other_pane(&mut self, op: &'static str, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        let Some(dest) = self.tab().other().map(|o| o.path.clone()) else { return };
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        if same(&dest, &self.pane().path) {
            self.toast("Both panes show the same folder", cx);
            return;
        }
        self.transfer_to(op, paths, dest, window, cx);
    }
}

impl Ferret {
    /// A search that led somewhere joins the history (newest first, 15 kept; not as administrator).
    pub fn remember_search(&mut self, query: String) {
        let q = query.trim().to_string();
        if q.is_empty() || self.elevated {
            return;
        }
        let mut list = self.settings.strings("searchHistory");
        // No repeats, whatever their case (as the app's `sameQuery`).
        list.retain(|h| !h.trim().eq_ignore_ascii_case(&q));
        list.insert(0, q);
        list.truncate(15);
        self.settings.set("searchHistory", serde_json::json!(list));
    }

    /// Right-click (or Shift+F10) on a search result.
    pub fn search_result_menu(&mut self, path: String, is_dir: bool, at: gpui::Point<gpui::Pixels>, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        use crate::menu::{item, sep};
        let close = |f: &mut Ferret, window: &mut gpui::Window, cx: &mut gpui::Context<Ferret>| {
            f.palette = None;
            window.focus(&f.focus, cx);
        };
        let (p1, p2, p3, p4, p5, p6, p7) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
        let mut items = vec![
            item(if is_dir { "Open folder" } else { "Open" }, action(move |f, window, cx| {
                close(f, window, cx);
                if is_dir { f.go(p1.clone(), cx) } else { f.open_file(p1.clone(), false, cx) }
            }))
            .icon("external-link")
            .keys("Enter"),
            item(if is_dir { "Show in parent folder" } else { "Show in folder" }, action(move |f, window, cx| {
                close(f, window, cx);
                f.reveal(&p2, cx);
            }))
            .icon("folder-open")
            .keys("Alt+Enter"),
        ];
        // A file's folder in a new tab (an archive opens as one), as the app's menu.
        let remote = ferret_core::paths::is_remote(&path);
        let tab_dir = if is_dir || (ferret_core::archive::archive_kind(&path).is_some() && !remote) { path.clone() } else { crate::util::parent(&path).unwrap_or_else(|| path.clone()) };
        items.push(
            item("Open in new tab", action(move |f, window, cx| {
                close(f, window, cx);
                f.new_tab(tab_dir.clone(), cx);
            }))
            .icon("folder-open")
            .keys("Ctrl+Enter"),
        );
        if !is_dir && !remote {
            items.push(item("Open with…", action(move |f, window, cx| {
                close(f, window, cx);
                f.open_with_dialog(p3.clone(), window, cx);
            }))
            .icon("app-window"));
        }
        items.push(sep());
        items.push(item("Copy path", action(move |f, _, cx| f.copy_paths(std::slice::from_ref(&p4), true, cx))).icon("clipboard-copy").keys("Ctrl+Shift+C"));
        items.push(item("Copy name", action(move |f, _, cx| f.copy_names(std::slice::from_ref(&p5), cx))).icon("type"));
        items.push(sep());
        items.push(item("Properties", action(move |f, window, cx| {
            close(f, window, cx);
            f.show_properties(vec![p6.clone()], cx);
        }))
        .icon("info"));
        let _ = p7;
        self.open_menu(at, items, window, cx);
    }
}

impl Ferret {
    /// Favorites, recents and settings, as other windows may have left them.
    pub fn reread_shared(&mut self, cx: &mut gpui::Context<Self>) {
        if let Ok(s) = ferret_core::browse::shared_state() {
            self.favorites = s.favorites;
            self.recents = s.recents;
            self.settings = crate::settings::Settings(s.settings);
            self.settings.apply_theme();
            cx.notify();
        }
    }
}
