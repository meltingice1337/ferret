//! Dialogs (Dialogs.tsx): confirm, name clashes, administrator permission, Properties, batch
//! rename, the extension question, text prompts (passwords, saved search names), SSH sign-in
//! prompts, and Settings (settings_view.rs).

use std::rc::Rc;
use std::time::Duration;

use ferret_core::fileops::{Conflict, ItemInfo, Policy, Stats};
use gpui::AnimationExt;
use gpui::{AnyElement, Context, Entity, FontWeight, MouseButton, SharedString, Subscription, Window, div, prelude::*, px};

use crate::imgmem::img;

use crate::files::plural;
use crate::input::TextInput;
use crate::kinds;
use crate::theme as t;
use crate::ui::{self, icon};
use crate::util::{self};
use crate::view::{Action, Ferret, action};

/// What a text prompt does with the text.
pub type Submit = Rc<dyn Fn(&mut Ferret, String, &mut Window, &mut Context<Ferret>)>;

pub enum Dialog {
    /// `on_ok`: the button Enter presses (a dangerous one starts on Cancel; Tab or the arrows move).
    Confirm { title: SharedString, body: SharedString, ok: SharedString, danger: bool, on_ok: bool, run: Action },
    Conflicts { conflicts: Vec<Conflict>, op: &'static str, dest: String, sources: Vec<String>, no_bin: bool, focus: usize },
    /// `why`: Windows' reason, when it gave one; `skip`: the operation stopped partway, and
    /// "Skip" / "Skip all" carry on with what's left (Esc or × stops there).
    Admin { what: String, why: Option<String>, skip: Option<Action>, just_this: Action },
    /// The offered update's changes (updateview.rs).
    WhatsNew,
    Properties { paths: Vec<String>, info: Option<ItemInfo>, stats: Option<Stats>, thumb: Option<std::sync::Arc<gpui::Image>> },
    BatchRename(Box<BatchRename>),
    Prompt { title: SharedString, body: SharedString, input: Entity<TextInput>, ok: SharedString, error: Option<SharedString>, submit: Submit, _sub: Subscription },
    Remote { id: u64, title: SharedString, message: SharedString, fields: Vec<(SharedString, Entity<TextInput>, bool)>, fingerprint: Option<SharedString>, can_remember: bool, remember: bool, ok: SharedString, danger: bool },
    Settings,
    Server(Box<crate::remote::ServerForm>),
    NetworkAdd(Box<crate::remote::NetworkForm>),
    Intro(crate::intro::Tour),
    Image(Box<crate::imgedit::ImgEdit>),
}

pub struct BatchRename {
    pub paths: Vec<String>,
    pub find: Entity<TextInput>,
    pub replace: Entity<TextInput>,
    pub template: Entity<TextInput>,
    pub start: Entity<TextInput>,
    pub regex: bool,
    pub match_case: bool,
    /// 0 keep, 1 lower, 2 UPPER, 3 Title.
    pub casing: u8,
    pub _subs: Vec<Subscription>,
}

struct Preview {
    rows: Vec<(String, String, String)>,
    error: Option<String>,
    dupes: std::collections::HashSet<String>,
    changed: usize,
}

impl BatchRename {
    fn preview(&self, cx: &gpui::App) -> Preview {
        let find = self.find.read(cx).text().to_string();
        let replace = self.replace.read(cx).text().to_string();
        let template = self.template.read(cx).text().to_string();
        let start: i64 = self.start.read(cx).text().trim().parse().unwrap_or(1);
        let mut error = None;
        let re = if find.is_empty() {
            None
        } else {
            let pat = if self.regex { find.clone() } else { regex::escape(&find) };
            match regex::RegexBuilder::new(&pat).case_insensitive(!self.match_case).build() {
                Ok(r) => Some(r),
                Err(e) => {
                    error = Some(e.to_string());
                    None
                }
            }
        };
        let width = (start + self.paths.len() as i64 - 1).to_string().len();
        let rows: Vec<(String, String, String)> = self
            .paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = util::file_name(p);
                let dot = name.rfind('.').filter(|&d| d > 0);
                let (stem, ext) = match dot {
                    Some(d) => (&name[..d], &name[d..]),
                    None => (name.as_str(), ""),
                };
                let mut next = if template.is_empty() {
                    name.clone()
                } else {
                    let n = start + i as i64;
                    let mut s = template.replace("{name}", stem).replace("{ext}", ext);
                    // {n} and {n:3}
                    while let Some(at) = s.find("{n") {
                        let Some(end) = s[at..].find('}') else { break };
                        let spec = &s[at + 2..at + end];
                        let pad = spec.strip_prefix(':').and_then(|x| x.parse::<usize>().ok()).unwrap_or(width);
                        s = format!("{}{:0pad$}{}", &s[..at], n, &s[at + end + 1..], pad = pad);
                    }
                    s
                };
                if let Some(re) = &re {
                    let rep = if self.regex { replace.clone() } else { replace.replace('$', "$$") };
                    next = re.replace_all(&next, rep.as_str()).into_owned();
                }
                next = match self.casing {
                    1 => next.to_lowercase(),
                    2 => next.to_uppercase(),
                    3 => {
                        let mut out = String::new();
                        let mut prev = ' ';
                        for c in next.chars() {
                            out.extend(if !prev.is_alphanumeric() && c.is_alphabetic() { c.to_uppercase().collect::<Vec<_>>() } else { vec![c] });
                            prev = c;
                        }
                        out
                    }
                    _ => next,
                };
                (p.clone(), name, next)
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        let mut dupes = std::collections::HashSet::new();
        for (_, _, to) in &rows {
            if !seen.insert(to.to_lowercase()) {
                dupes.insert(to.to_lowercase());
            }
        }
        let changed = rows.iter().filter(|(_, a, b)| a != b).count();
        Preview { rows, error, dupes, changed }
    }
}

impl Ferret {
    pub fn confirm(&mut self, title: String, body: String, ok: &str, danger: bool, run: Action, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Confirm { title: title.into(), body: body.into(), ok: ok.to_string().into(), danger, on_ok: !danger, run });
        cx.notify();
    }

    pub fn ask_conflicts(&mut self, conflicts: Vec<Conflict>, op: &'static str, dest: String, sources: Vec<String>, no_bin: bool, cx: &mut Context<Self>) {
        // The safe choice ("Keep both") has the focus, so Enter never overwrites anything.
        self.dialog = Some(Dialog::Conflicts { conflicts, op, dest, sources, no_bin, focus: 1 });
        cx.notify();
    }

    fn pick_conflict(&mut self, policy: Option<Policy>, cx: &mut Context<Self>) {
        let Some(Dialog::Conflicts { op, dest, sources, .. }) = self.dialog.take() else { return };
        if let Some(p) = policy {
            self.run_transfer(op, sources, dest, p, cx);
        }
        cx.notify();
    }

    /// Properties (Alt+Enter): what it is, where, how big, its dates and attributes.
    pub fn show_properties(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        self.dialog = Some(Dialog::Properties { paths: paths.clone(), info: None, stats: None, thumb: None });
        self.load_properties(paths, cx);
        cx.notify();
    }

    fn load_properties(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let single = (paths.len() == 1).then(|| paths[0].clone());
        let p2 = paths.clone();
        let task = cx.background_executor().spawn(async move {
            let info = single.clone().and_then(|p| ferret_core::rt::block_on(ferret_core::fileops::item_info(p)).ok());
            let thumb = single.and_then(|p| ferret_core::shell::thumbnail_png(&p, 96, false).or_else(|_| ferret_core::shell::thumbnail_png(&p, 64, true)).ok());
            let stats = ferret_core::rt::block_on(ferret_core::fileops::folder_stats(p2)).ok();
            (info, stats, thumb)
        });
        cx.spawn(async move |this, cx| {
            let (i, s, th) = task.await;
            this.update(cx, |f, cx| {
                if let Some(Dialog::Properties { paths: ps, info, stats, thumb }) = f.dialog.as_mut() {
                    if *ps == paths {
                        *info = i;
                        *stats = s;
                        *thumb = th.map(|b| std::sync::Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, b)));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn open_batch_rename(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let mk = |placeholder: &str, text: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut i = TextInput::new(placeholder.to_string(), cx).sized(13., 20.);
                i.set_text(text, cx);
                i
            })
        };
        let find = mk("Text or pattern", "", cx);
        let replace = mk("", "", cx);
        let template = mk("e.g. Trip_{n:3}{ext}", "", cx);
        let start = mk("", "1", cx);
        let subs = [&find, &replace, &template, &start].into_iter().map(|e| cx.subscribe(e, |_, _, _: &crate::input::Changed, cx| cx.notify())).collect();
        self.dialog = Some(Dialog::BatchRename(Box::new(BatchRename { paths, find: find.clone(), replace, template, start, regex: false, match_case: false, casing: 0, _subs: subs })));
        focus_later(find, cx);
        cx.notify();
    }

    fn apply_batch_rename(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::BatchRename(b)) = &self.dialog else { return };
        let p = b.preview(cx);
        if p.error.is_some() || !p.dupes.is_empty() || p.changed == 0 {
            return;
        }
        self.dialog = None;
        let pairs: Vec<(String, String)> = p.rows.into_iter().filter(|(_, a, b)| a != b).map(|(from, _, to)| (from, to)).collect();
        let task = cx.background_executor().spawn(async move {
            let mut done = Vec::new();
            let mut errors = Vec::new();
            for (from, to) in pairs {
                match ferret_core::rt::block_on(ferret_core::fileops::rename_item(from.clone(), to.clone())) {
                    Ok(new) => done.push((from, new)),
                    Err(e) => errors.push(format!("{}: {e}", util::file_name(&from))),
                }
            }
            (done, errors)
        });
        cx.spawn(async move |this, cx| {
            let (done, errors) = task.await;
            this.update(cx, |f, cx| {
                for e in errors {
                    f.toast_error(e, cx);
                }
                if !done.is_empty() {
                    let (u, r) = (done.clone(), done.clone());
                    let n = done.len();
                    f.record(
                        format!("Rename {}", plural(n, "item")),
                        move |_, _| {
                            for (from, to) in u.iter().rev() {
                                let _ = ferret_core::rt::block_on(ferret_core::fileops::rename_item(to.clone(), util::file_name(from)));
                            }
                        },
                        move |_, _| {
                            for (from, to) in &r {
                                let _ = ferret_core::rt::block_on(ferret_core::fileops::rename_item(from.clone(), util::file_name(to)));
                            }
                        },
                        Some(format!("Renamed {}", plural(n, "item"))),
                        cx,
                    );
                    f.pane_mut().pending_select = Some(done.into_iter().map(|(_, to)| to).collect());
                }
                f.reload_all(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// "Change the file extension?": use the new one, or keep the old.
    pub fn ask_extension(&mut self, path: String, new_name: String, path2: String, keep_name: String, new_ext: String, cx: &mut Context<Self>) {
        let title = if new_ext.is_empty() { "Remove the file extension?".to_string() } else { "Change the file extension?".to_string() };
        let ok = if new_ext.is_empty() { "Remove it".to_string() } else { format!("Use .{new_ext}") };
        self.dialog = Some(Dialog::Confirm {
            title: title.into(),
            body: format!("The file may stop opening in the right app. Keep the name as “{keep_name}” instead?").into(),
            ok: ok.into(),
            danger: false,
            on_ok: true,
            run: action(move |f, _, cx| f.rename_to(path.clone(), new_name.clone(), cx)),
        });
        let _ = path2;
        cx.notify();
    }

    /// A one-line text question (a password, a name).
    pub fn prompt(&mut self, title: &str, body: &str, initial: &str, ok: &str, secret: bool, submit: Submit, cx: &mut Context<Self>) {
        let name = title.to_string();
        let input = cx.new(|cx| {
            let mut i = TextInput::new("", cx).sized(13., 20.).masked(secret).named(name);
            i.set_text(initial, cx);
            i.select_everything(cx);
            i
        });
        let sub = cx.subscribe(&input, |f, _, _: &crate::input::Changed, cx| {
            if let Some(Dialog::Prompt { error, .. }) = f.dialog.as_mut() {
                *error = None;
            }
            cx.notify();
        });
        self.dialog = Some(Dialog::Prompt {
            title: title.to_string().into(),
            body: body.to_string().into(),
            input: input.clone(),
            ok: ok.to_string().into(),
            error: None,
            submit,
            _sub: sub,
        });
        focus_later(input, cx);
        cx.notify();
    }

    /// An encrypted archive: its password, then what was asked (open, extract).
    pub fn ask_password(&mut self, archive: String, extract: Option<(String, Option<Vec<String>>)>, cx: &mut Context<Self>) {
        let name = util::file_name(&archive);
        self.prompt(
            &format!("“{name}” is protected"),
            "Type its password to open it.",
            "",
            "Unlock",
            true,
            Rc::new(move |f, pw, _, cx| {
                let (a, ex) = (archive.clone(), extract.clone());
                let task = cx.background_executor().spawn({
                    let a = a.clone();
                    let pw = pw.clone();
                    async move { ferret_core::rt::block_on(ferret_core::archive::archive_unlock(a, pw)) }
                });
                cx.spawn(async move |this, cx| {
                    let r = task.await;
                    this.update(cx, |f, cx| match r {
                        Ok(()) => {
                            f.dialog = None;
                            match ex {
                                Some((dest, entries)) => f.extract_into(a, dest, entries, cx),
                                None => f.reload_all(cx),
                            }
                        }
                        Err(_) => {
                            // "Wrong password": try again.
                            if let Some(Dialog::Prompt { error, .. }) = f.dialog.as_mut() {
                                *error = Some("Wrong password".into());
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
                let _ = f;
            }),
            cx,
        );
    }

    /// Ctrl+S in search, or "Save to sidebar": the search gets a name and a place in the sidebar.
    pub fn prompt_save_search(&mut self, query: String, cx: &mut Context<Self>) {
        // As the app's `promptSaveSearch`: queries compare without case; a saved one is renamed
        // where it is in the list; an empty name keeps the query as the name.
        let query = query.trim().to_string();
        if query.is_empty() {
            return;
        }
        self.palette_back = None;
        let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        let existing = self.settings.saved_searches().into_iter().find(|(_, q)| same(q, &query)).map(|(n, _)| n);
        let initial = existing.clone().unwrap_or_else(|| query.clone());
        let renaming = existing.is_some();
        self.prompt(
            if renaming { "Rename saved search" } else { "Save search" },
            &format!("“{query}” goes in the sidebar under Searches. It opens as a tab that always lists the current matches."),
            &initial,
            if renaming { "Rename" } else { "Save" },
            false,
            Rc::new(move |f, name, _, cx| {
                let name = name.trim().to_string();
                let name = if name.is_empty() { query.clone() } else { name };
                let mut list: Vec<serde_json::Value> = f.settings.0["savedSearches"].as_array().cloned().unwrap_or_default();
                match list.iter_mut().find(|s| s["query"].as_str().is_some_and(|q| same(q, &query))) {
                    Some(s) => s["name"] = serde_json::json!(name),
                    None => list.push(serde_json::json!({ "name": name, "query": query })),
                }
                f.settings.set("savedSearches", serde_json::Value::Array(list));
                f.dialog = None;
                f.palette_back = None;
                if !renaming {
                    f.toast(format!("Saved “{name}” to the sidebar"), cx);
                }
                cx.notify();
            }),
            cx,
        );
    }

    /// An SSH server asks something: a password, a key's passphrase, whether to trust its key.
    pub fn remote_prompt(&mut self, p: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(id) = p["id"].as_u64() else { return };
        let fields: Vec<(SharedString, Entity<TextInput>, bool)> = p["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                let label: SharedString = f["label"].as_str().unwrap_or("").to_string().into();
                let secret = f["secret"].as_bool().unwrap_or(false);
                let name = label.clone();
                (label, cx.new(|cx| TextInput::new("", cx).sized(13., 20.).masked(secret).named(name)), secret)
            })
            .collect();
        if let Some((_, first, _)) = fields.first() {
            focus_later(first.clone(), cx);
        }
        self.dialog = Some(Dialog::Remote {
            id,
            title: p["title"].as_str().unwrap_or("Sign in").to_string().into(),
            message: p["message"].as_str().unwrap_or("").to_string().into(),
            fields,
            fingerprint: p["fingerprint"].as_str().map(|s| s.to_string().into()),
            can_remember: p["canRemember"].as_bool().unwrap_or(false),
            remember: false,
            ok: p["confirmLabel"].as_str().unwrap_or("Sign in").to_string().into(),
            danger: p["danger"].as_bool().unwrap_or(false),
        });
        cx.notify();
    }

    fn answer_remote(&mut self, ok: bool, cx: &mut Context<Self>) {
        let Some(Dialog::Remote { id, fields, remember, .. }) = self.dialog.take() else { return };
        let values: Option<Vec<String>> = ok.then(|| fields.iter().map(|(_, i, _)| i.read(cx).text().to_string()).collect());
        if id == 0 {
            // A network folder's sign-in (remote.rs).
            match values {
                Some(v) => self.answer_network(v, remember, cx),
                None => self.network_pending = None,
            }
        } else {
            ferret_core::remote::prompt::remote_answer(id, values, remember);
        }
        cx.notify();
    }

    pub fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dialog::Remote { .. }) = &self.dialog {
            self.answer_remote(false, cx);
        }
        let prompt = matches!(self.dialog, Some(Dialog::Prompt { .. }));
        self.dialog = None;
        window.focus(&self.focus, cx);
        // Saving a search from Ctrl+K, cancelled: back to that search.
        if let Some(back) = self.palette_back.take().filter(|_| prompt) {
            self.open_palette(Some(back), window, cx);
        }
        cx.notify();
    }

    /// Keys while a dialog is open. True if handled (the rest go to its fields).
    pub fn dialog_key(&mut self, key: &str, m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.dialog.as_mut() {
            Some(Dialog::Settings) => return self.settings_key(key, window, cx),
            Some(Dialog::Intro(_)) => return self.intro_key(key, m, window, cx),
            Some(Dialog::Image(_)) => {
                if self.image_editor_key(key, m.control, window, cx) {
                    return true;
                }
                if key == "escape" {
                    self.close_dialog(window, cx);
                    return true;
                }
                return false;
            }
            Some(Dialog::Server(_)) | Some(Dialog::NetworkAdd(_)) => {
                if key == "escape" {
                    self.close_dialog(window, cx);
                    return true;
                }
                if key == "enter" {
                    self.submit_remote_form(window, cx);
                    return true;
                }
                return false;
            }
            _ => {}
        }
        if key == "escape" {
            self.close_dialog(window, cx);
            return true;
        }
        match self.dialog.as_mut() {
            Some(Dialog::Conflicts { focus, .. }) => match key {
                "down" | "tab" => {
                    *focus = (*focus + 1) % 3;
                    cx.notify();
                }
                "up" => {
                    *focus = (*focus + 2) % 3;
                    cx.notify();
                }
                "enter" => {
                    let p = [Policy::Replace, Policy::KeepBoth, Policy::Skip][*focus];
                    self.pick_conflict(Some(p), cx);
                    window.focus(&self.focus, cx);
                }
                _ => return false,
            },
            Some(Dialog::Confirm { on_ok, .. }) => match key {
                // Irreversible actions start on Cancel (Enter alone never destroys anything);
                // Tab or the arrows move to the other button, ringed so it's plain which.
                "tab" | "left" | "right" => {
                    *on_ok = !*on_ok;
                    cx.notify();
                }
                "enter" => {
                    if !*on_ok {
                        self.close_dialog(window, cx);
                    } else if let Some(Dialog::Confirm { run, .. }) = self.dialog.take() {
                        window.focus(&self.focus, cx);
                        run(self, window, cx);
                    }
                }
                _ => return false,
            },
            Some(Dialog::Prompt { .. }) => {
                if key == "enter" {
                    self.submit_prompt(window, cx);
                } else {
                    return false;
                }
            }
            Some(Dialog::BatchRename(_)) => {
                if key == "enter" {
                    self.apply_batch_rename(cx);
                    window.focus(&self.focus, cx);
                } else {
                    return false;
                }
            }
            Some(Dialog::Remote { .. }) => {
                if key == "enter" {
                    self.answer_remote(true, cx);
                    window.focus(&self.focus, cx);
                } else {
                    return false;
                }
            }
            Some(Dialog::Properties { .. }) | Some(Dialog::Admin { .. }) => {
                if key == "enter" {
                    self.close_dialog(window, cx);
                } else {
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Prompt { input, submit, .. }) = &self.dialog else { return };
        let (text, submit) = (input.read(cx).text().to_string(), submit.clone());
        submit(self, text, window, cx);
        if self.dialog.is_none() {
            window.focus(&self.focus, cx);
        }
    }

    // ───────────────────────────── drawing ─────────────────────────────

    pub fn dialog_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card: AnyElement = match self.dialog.as_ref()? {
            Dialog::Settings => return Some(self.settings_view(window, cx)),
            Dialog::Intro(_) => return Some(self.intro_view(window, cx)),
            Dialog::Image(_) => return Some(self.image_editor_view(window, cx)),
            Dialog::WhatsNew => self.whats_new_view(window, cx),
            Dialog::Server(_) | Dialog::NetworkAdd(_) => self.remote_form_view(window, cx),
            Dialog::Confirm { title, body, ok, danger, on_ok, .. } => {
                let (danger, on_ok) = (*danger, *on_ok);
                // The button Enter presses: ringed (a dangerous dialog starts on Cancel).
                let ring = |d: gpui::Stateful<gpui::Div>, on: bool| d.when(on, |d| d.border_2().border_color(t::fg()));
                let hint = if on_ok { None } else { Some("Enter cancels · Tab switches") };
                modal(title.clone(), 440., cx)
                    .aria_description(body.clone())
                    .child(
                        body_div()
                            .child(div().text_color(t::fg2()).child(body.clone()))
                            .child(
                                actions()
                                    .when_some(hint.filter(|_| danger), |d, h| d.child(div().flex_1().min_w_0().text_size(px(12.)).text_color(t::fg3()).child(h)))
                                    .child(ring(cancel_btn(cx), !on_ok))
                                    .child(ring(if danger { ui::danger_button("dlg-ok", ok.clone()) } else { ui::button("dlg-ok", ok.clone(), true) }, on_ok).on_click(cx.listener(
                                        |f, _, window, cx| {
                                            if let Some(Dialog::Confirm { run, .. }) = f.dialog.take() {
                                                window.focus(&f.focus, cx);
                                                run(f, window, cx);
                                            }
                                        },
                                    ))),
                            ),
                    )
                    .into_any_element()
            }
            Dialog::Admin { what, why, skip, .. } => modal("Administrator permission needed".into(), 480., cx)
                .child(
                    body_div()
                        .child(
                            div()
                                .flex()
                                .gap(px(12.))
                                .child(icon("shield-check", 28., t::warn()))
                                // Windows' reason when there is one, as the app's `withAdmin` says it.
                                .child(ui::note("dlg-msg", match (why, skip.is_some()) {
                                    // Said in full by the operation (what went, what's left).
                                    (Some(why), true) => why.clone(),
                                    (Some(why), false) => format!("{why}. “Just this action” asks Windows once, for this operation only. “Elevate window” restarts this window as administrator (same tabs); then do it again there."),
                                    (None, _) => format!("Windows protects this folder. {what} needs an administrator's permission."),
                                })
                                // Beside the icon, it wraps within the dialog (it ran out of it).
                                .flex_1()
                                .min_w_0()
                                .text_color(t::fg2())),
                        )
                        .child(
                            actions()
                                // The rest went: Skip leaves these where they are.
                                .when_some(skip.clone(), |d, skip| {
                                    d.child(ui::button("adm-skip", "Skip", false).on_click(cx.listener(move |f, _, window, cx| {
                                        f.close_dialog(window, cx);
                                        skip(f, window, cx);
                                    })))
                                })
                                .when(skip.is_none(), |d| d.child(ui::button("dlg-cancel", "Cancel", false).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx)))))
                                .child(ui::button("adm-window", "Elevate window", false).on_click(cx.listener(|f, _, window, cx| {
                                    f.close_dialog(window, cx);
                                    f.restart_elevated(window, cx);
                                })))
                                .child(ui::button("adm-once", "Just this action", true).on_click(cx.listener(|f, _, window, cx| {
                                    if let Some(Dialog::Admin { just_this, .. }) = f.dialog.take() {
                                        window.focus(&f.focus, cx);
                                        just_this(f, window, cx);
                                    }
                                }))),
                        ),
                )
                .into_any_element(),
            Dialog::Conflicts { conflicts, op, dest, no_bin, focus, .. } => {
                let n = conflicts.len();
                let title = if n == 1 { format!("“{}” already exists", conflicts[0].name) } else { format!("{n} items already exist") };
                let choice = |ix: usize, id: &'static str, title: &'static str, sub: String, warn: Option<&'static str>, policy: Policy, cx: &mut Context<Self>| {
                    div()
                        .id(id)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .px(px(12.))
                        .py(px(9.))
                        .rounded(px(9.))
                        .bg(t::hover())
                        .border_1()
                        .border_color(if *focus == ix { t::accent() } else { t::line() })
                        .hover(|s| s.bg(t::press()))
                        .on_click(cx.listener(move |f, _, window, cx| {
                            f.pick_conflict(Some(policy), cx);
                            window.focus(&f.focus, cx);
                        }))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(12.)).text_color(t::fg3()).child(sub))
                        .children(warn.map(|w| div().text_size(px(12.)).text_color(t::warn()).child(w)))
                };
                let one = if n == 1 { "item" } else { "items" };
                modal(title.into(), 560., cx)
                    .child(
                        body_div()
                            .child(ui::note("dlg-msg", format!("The destination {} already has {} with the same name.", util::folder_name(dest), if n == 1 { "an item" } else { "items" })).text_color(t::fg2()))
                            .child(div().flex().flex_col().gap(px(6.)).children(conflicts.iter().take(6).map(|c| {
                                let same = !c.is_dir && c.source_size == c.target_size && c.source_modified.map(|m| m / 1000) == c.target_modified.map(|m| m / 1000);
                                let older = !c.is_dir && c.source_modified.unwrap_or(0) > c.target_modified.unwrap_or(0);
                                let col = |label: &'static str, size: u64, date: Option<i64>, older: bool| {
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .text_size(px(12.))
                                        .text_color(t::fg2())
                                        .child(div().text_size(px(10.5)).text_color(t::fg3()).child(label.to_uppercase()))
                                        .child(div().flex().gap(px(6.)).child(format!("{} · {}", if c.is_dir { "Folder".into() } else { kinds::format_size(size) }, kinds::format_date(date))).when(older, |d| d.child(div().text_color(t::warn()).child("older"))))
                                };
                                div()
                                    .px(px(10.))
                                    .py(px(8.))
                                    .rounded(px(8.))
                                    .bg(t::hover())
                                    .child(div().flex().gap(px(8.)).mb(px(4.)).child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(c.name.clone())).when(same, |d| d.child(div().text_size(px(10.5)).text_color(t::fg3()).child("same size and date"))))
                                    .child(div().flex().gap(px(10.)).child(col("Incoming", c.source_size, c.source_modified, false)).child(col("Existing", c.target_size, c.target_modified, older)))
                            })))
                            .when(n > 6, |d| d.child(div().text_size(px(12.)).text_color(t::fg3()).child(format!("…and {} more", n - 6))))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.))
                                    .child(choice(0, "c-replace", "Replace", format!("Overwrite the existing {one}; folders are merged"), no_bin.then_some("There's no Recycle Bin here: what's overwritten can't be brought back"), Policy::Replace, cx))
                                    .child(choice(1, "c-both", "Keep both", "Incoming items get a numbered name, e.g. “name (2)”".into(), None, Policy::KeepBoth, cx))
                                    .child(choice(2, "c-skip", "Skip", format!("Leave the existing {one} untouched"), None, Policy::Skip, cx)),
                            )
                            .child(actions().child(ui::button("c-cancel", format!("Cancel {op}"), false).on_click(cx.listener(|f, _, window, cx| {
                                f.pick_conflict(None, cx);
                                window.focus(&f.focus, cx);
                            })))),
                    )
                    .into_any_element()
            }
            Dialog::Properties { paths, info, stats, thumb } => {
                let single = paths.len() == 1;
                let first = paths[0].clone();
                let remote = ferret_core::paths::is_remote(&first);
                let title = if single { format!("{} properties", util::file_name(&first)) } else { format!("{} items", paths.len()) };
                let row = |label: &'static str, value: AnyElement| {
                    div().flex().gap(px(12.)).child(div().flex_none().w(px(110.)).text_color(t::fg3()).child(label)).child(div().flex_1().min_w_0().child(value))
                };
                let text = |s: String| div().child(s).into_any_element();
                let mut grid = div().flex().flex_col().gap(px(8.)).py(px(12.)).border_t_1().border_color(t::line());
                if remote {
                    grid = grid.child(row("Server", text(ferret_core::paths::remote_parts(&first).map(|(id, _)| id.to_string()).unwrap_or_default())));
                }
                let parent = util::parent(&first).unwrap_or_default();
                let reveal = first.clone();
                grid = grid.child(row(
                    "Location",
                    div()
                        .id("prop-loc")
                        .text_color(t::accent_text())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|s| s.underline())
                        .on_click(cx.listener(move |f, _, window, cx| {
                            f.close_dialog(window, cx);
                            f.reveal(&reveal, cx);
                        }))
                        .child(parent)
                        .into_any_element(),
                ));
                grid = grid.child(row(
                    "Size",
                    match stats {
                        Some(s) => text(format!("{} ({} bytes){}", kinds::format_size(s.bytes), kinds::group_digits(s.bytes as usize), if s.partial { " · some items unreadable" } else { "" })),
                        None => div().text_color(t::fg3()).child("Calculating…").into_any_element(),
                    },
                ));
                if stats.as_ref().is_some_and(|s| s.dirs + s.files > 1) || info.as_ref().is_some_and(|i| i.is_dir) {
                    grid = grid.child(row("Contains", text(stats.as_ref().map(|s| format!("{}, {}", plural(s.files as usize, "file"), plural(s.dirs as usize, "folder"))).unwrap_or_else(|| "…".into()))));
                }
                if let Some(i) = info {
                    if let Some(tg) = &i.link_target {
                        grid = grid.child(row("Target", text(tg.clone())));
                    }
                    if !remote {
                        grid = grid.child(row("Created", text(kinds::format_date(i.created))));
                    }
                    grid = grid.child(row("Modified", text(kinds::format_date(i.modified)))).child(row("Accessed", text(kinds::format_date(i.accessed))));
                    if !remote {
                        let (ro, hid, sys) = (i.readonly, i.hidden, i.system);
                        let path = i.path.clone();
                        let p2 = path.clone();
                        grid = grid.child(row(
                            "Attributes",
                            div()
                                .flex()
                                .gap(px(14.))
                                .child(ui::checkbox("attr-ro", "Read-only", ro).on_click(cx.listener(move |f, _, _, cx| f.set_attr(path.clone(), !ro, hid, cx))))
                                .child(ui::checkbox("attr-hid", "Hidden", hid).on_click(cx.listener(move |f, _, _, cx| f.set_attr(p2.clone(), ro, !hid, cx))))
                                .when(sys, |d| d.child(div().text_color(t::fg3()).child("System")))
                                .into_any_element(),
                        ));
                    }
                }
                let kind = info.as_ref().map(|i| kinds::type_label(i.is_dir, &i.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default())).unwrap_or_default();
                let (r1, r2) = (paths.clone(), first.clone());
                modal(title.into(), 520., cx)
                    .child(
                        body_div()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(14.))
                                    .child(
                                        div()
                                            .size(px(72.))
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(12.))
                                            .bg(t::hover())
                                            .child(match (single, thumb) {
                                                (true, Some(th)) => img(th.clone()).max_w(px(64.)).max_h(px(64.)).into_any_element(),
                                                (true, None) => icon(if info.as_ref().is_some_and(|i| i.is_dir) { "folder" } else { "file" }, 34., t::folder()).into_any_element(),
                                                (false, _) => div().text_size(px(22.)).font_weight(FontWeight::BOLD).text_color(t::accent_text()).child(paths.len().to_string()).into_any_element(),
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(if single { util::file_name(&first) } else { format!("{} items", paths.len()) }))
                                            .child(div().text_color(t::fg3()).child(if single { kind } else { format!("in {}", util::folder_name(&util::parent(&first).unwrap_or_default())) })),
                                    ),
                            )
                            .child(grid)
                            .child(
                                actions()
                                    .when(!remote, |d| {
                                        d.child(ui::button("prop-explorer", "Show in Explorer", false).on_click(cx.listener(move |f, _, _, cx| f.reveal_in_explorer(r2.clone(), cx)))).child(
                                            ui::button("prop-win", "Windows properties…", false).on_click(cx.listener(move |f, _, window, cx| {
                                                let Some(h) = crate::view::hwnd(window) else { return };
                                                let paths = r1.clone();
                                                let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::shell::show_properties(h, paths)) });
                                                cx.spawn(async move |this, cx| {
                                                    if let Err(e) = task.await {
                                                        this.update(cx, |f, cx| f.toast_error(e, cx)).ok();
                                                    }
                                                })
                                                .detach();
                                                let _ = f;
                                            })),
                                        )
                                    })
                                    .child(ui::button("prop-close", "Close", true).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx)))),
                            ),
                    )
                    .into_any_element()
            }
            Dialog::BatchRename(b) => {
                let pv = b.preview(cx);
                let (regex, mcase, casing) = (b.regex, b.match_case, b.casing);
                let field = |label: &'static str, input: &Entity<TextInput>| {
                    div().flex_1().flex().flex_col().gap(px(4.)).child(div().text_size(px(12.)).text_color(t::fg3()).child(label)).child(ui::field(false).child(input.clone()))
                };
                let ok = pv.error.is_none() && pv.dupes.is_empty() && pv.changed > 0;
                modal(format!("Rename {} items", b.paths.len()).into(), 720., cx)
                    .child(
                        body_div()
                            .child(div().flex().gap(px(10.)).child(field("Find", &b.find)).child(field("Replace with", &b.replace)))
                            .child(div().flex().gap(px(10.)).child(field("Template", &b.template)).child(div().w(px(110.)).child(field("Start at", &b.start))))
                            // Under the template it belongs to (beside the case buttons it ran out of the dialog).
                            .child(div().mt(px(-6.)).text_size(px(12.)).text_color(t::fg3()).child("Template tokens: {name} {ext} {n} {n:3}"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.))
                                    .child(ui::checkbox("br-re", "Regular expression", regex).on_click(cx.listener(|f, _, _, cx| {
                                        if let Some(Dialog::BatchRename(b)) = f.dialog.as_mut() {
                                            b.regex = !b.regex;
                                        }
                                        cx.notify();
                                    })))
                                    .child(ui::checkbox("br-case", "Match case", mcase).on_click(cx.listener(|f, _, _, cx| {
                                        if let Some(Dialog::BatchRename(b)) = f.dialog.as_mut() {
                                            b.match_case = !b.match_case;
                                        }
                                        cx.notify();
                                    })))
                                    .child(ui::seg().ml_auto().children(["Keep case", "lowercase", "UPPERCASE", "Title Case"].into_iter().enumerate().map(|(i, l)| {
                                        ui::seg_btn(("br-casing", i), None, Some(l.into()), casing == i as u8).on_click(cx.listener(move |f, _, _, cx| {
                                            if let Some(Dialog::BatchRename(b)) = f.dialog.as_mut() {
                                                b.casing = i as u8;
                                            }
                                            cx.notify();
                                        }))
                                    }))),
                            )
                            .children(pv.error.clone().map(|e| div().flex().gap(px(6.)).text_color(t::danger()).child(icon("triangle-alert", 14., t::danger())).child(e)))
                            .child(
                                div()
                                    .id("br-list")
                                    .max_h(px(260.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .rounded(px(8.))
                                    .bg(t::hover())
                                    .p(px(6.))
                                    .children(pv.rows.iter().map(|(_, from, to)| {
                                        let same = from == to;
                                        let dupe = pv.dupes.contains(&to.to_lowercase());
                                        div()
                                            .flex()
                                            .gap(px(8.))
                                            .text_size(px(12.5))
                                            .when(same, |d| d.opacity(0.5))
                                            .child(div().flex_1().min_w_0().truncate().text_color(t::fg2()).child(from.clone()))
                                            .child(div().text_color(t::fg3()).child("→"))
                                            .child(div().flex_1().min_w_0().truncate().text_color(if dupe { t::danger() } else { t::fg() }).child(to.clone()))
                                    })),
                            )
                            .child(
                                actions()
                                    .child(div().mr_auto().text_color(t::fg3()).child(if pv.dupes.is_empty() { format!("{} of {} will change", pv.changed, b.paths.len()) } else { "Some new names collide".into() }))
                                    .child(cancel_btn(cx))
                                    .child(ui::button("br-ok", "Rename", true).when(!ok, |d| d.opacity(0.4)).on_click(cx.listener(|f, _, window, cx| {
                                        f.apply_batch_rename(cx);
                                        window.focus(&f.focus, cx);
                                    }))),
                            ),
                    )
                    .into_any_element()
            }
            Dialog::Prompt { title, body, input, ok, error, .. } => modal(title.clone(), 440., cx)
                .child(
                    body_div()
                        .when(!body.is_empty(), |d| d.child(ui::note("dlg-msg", body.clone()).text_color(t::fg2())))
                        .child(ui::field(true).child(input.clone()))
                        .children(error.clone().map(|e| ui::note("dlg-error", e).role(gpui::Role::Alert).text_color(t::danger()).text_size(px(12.))))
                        .child(actions().child(cancel_btn(cx)).child(ui::button("p-ok", ok.clone(), true).on_click(cx.listener(|f, _, window, cx| f.submit_prompt(window, cx))))),
                )
                .into_any_element(),
            Dialog::Remote { title, message, fields, fingerprint, can_remember, remember, ok, danger, .. } => {
                let (rem, danger) = (*remember, *danger);
                modal(title.clone(), 460., cx)
                    .child(
                        body_div()
                            .child(ui::note("dlg-msg", message.clone()).text_color(t::fg2()))
                            .children(fingerprint.clone().map(|fp| ui::note("dlg-fingerprint", fp).font_family(t::MONO).text_size(px(12.)).px(px(10.)).py(px(8.)).rounded(px(8.)).bg(t::hover())))
                            .children(fields.iter().map(|(label, input, _)| div().flex().flex_col().gap(px(4.)).child(div().text_size(px(12.)).text_color(t::fg3()).child(label.clone())).child(ui::field(false).child(input.clone()))))
                            .when(*can_remember, |d| {
                                d.child(ui::checkbox("r-remember", "Remember (in Windows Credential Manager)", rem).on_click(cx.listener(|f, _, _, cx| {
                                    if let Some(Dialog::Remote { remember, .. }) = f.dialog.as_mut() {
                                        *remember = !*remember;
                                    }
                                    cx.notify();
                                })))
                            })
                            .child(actions().child(cancel_btn(cx)).child(
                                (if danger { ui::danger_button("r-ok", ok.clone()) } else { ui::button("r-ok", ok.clone(), true) }).on_click(cx.listener(|f, _, window, cx| {
                                    f.answer_remote(true, cx);
                                    window.focus(&f.focus, cx);
                                })),
                            )),
                    )
                    .into_any_element()
            }
        };
        Some(scrim(card, cx))
    }

    fn set_attr(&mut self, path: String, readonly: bool, hidden: bool, cx: &mut Context<Self>) {
        if let Err(e) = ferret_core::fileops::set_attributes(path.clone(), readonly, hidden) {
            self.toast_error(e, cx);
            return;
        }
        if let Some(Dialog::Properties { paths, .. }) = &self.dialog {
            let paths = paths.clone();
            self.load_properties(paths, cx);
        }
        let dir = self.pane().path.clone();
        self.reload_dir(&dir, cx);
    }
}

/// The modal's backdrop: clicks outside close it.
pub fn scrim(card: AnyElement, cx: &mut Context<Ferret>) -> AnyElement {
    let el = div()
        .id("scrim")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t::scrim())
        .occlude()
        .on_mouse_down(MouseButton::Left, cx.listener(|f, _, window, cx| f.close_dialog(window, cx)))
        .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(card));
    if t::animate() {
        el.with_animation("scrim-in", gpui::Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k)).into_any_element()
    } else {
        el.into_any_element()
    }
}

/// A modal's card with its title and close button.
pub fn modal(title: SharedString, width: f32, cx: &mut Context<Ferret>) -> gpui::Stateful<gpui::Div> {
    ui::modal_card(width).id("dialog").role(gpui::Role::Dialog).aria_label(title.clone()).child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(22.))
            .pr(px(16.))
            .pt(px(14.))
            .pb(px(10.))
            .border_b_1()
            .border_color(t::line())
            .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(ui::icon_btn("dlg-x", "x", true).tooltip(ui::tip("Close (Esc)")).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx)))),
    )
}

pub fn body_div() -> gpui::Div {
    div().px(px(22.)).pt(px(14.)).pb(px(20.)).flex().flex_col().gap(px(14.))
}

pub fn actions() -> gpui::Div {
    div().flex().justify_end().items_center().gap(px(8.)).mt(px(4.))
}

fn cancel_btn(cx: &mut Context<Ferret>) -> gpui::Stateful<gpui::Div> {
    ui::button("dlg-cancel", "Cancel", false).on_click(cx.listener(|f, _, window, cx| f.close_dialog(window, cx)))
}

/// Focuses a field once it's drawn.
pub fn focus_later(input: Entity<TextInput>, cx: &mut Context<Ferret>) {
    // The window gives it the keyboard as it draws next (it may not be the active window).
    let handle = gpui::Focusable::focus_handle(input.read(cx), cx);
    cx.spawn(async move |this, cx| {
        this.update(cx, |f, cx| {
            f.pending_focus = Some(handle);
            cx.notify();
        })
        .ok();
    })
    .detach();
}
