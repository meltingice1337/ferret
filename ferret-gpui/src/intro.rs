//! The intro tour (Intro.tsx, lib/intro.ts): a welcome screen, then a step for each thing worth
//! knowing, with a small animated demo and its keys. Shown once on a first start; afterwards only
//! steps added since. Settings and the command palette bring it back.

use std::time::Duration;

use gpui::{Animation, AnimationExt, AnyElement, Context, FontWeight, Window, div, prelude::*, px, relative};
use serde_json::json;

use crate::dialogs::Dialog;
use crate::theme as t;
use crate::ui::{self, icon};
use crate::view::Ferret;

pub struct Step {
    pub id: &'static str,
    pub title: &'static str,
    pub text: &'static str,
    pub keys: &'static [(&'static str, &'static str)],
    pub icon: &'static str,
    /// "files" | "commands" | "network"
    pub try_it: Option<&'static str>,
}

pub const STEPS: &[Step] = &[
    Step { id: "search", title: "Search every drive", text: "Find any file by name, narrow it down with filters, or look for text inside files. Turn on the drive index in the search window for instant results on NTFS drives.", keys: &[("Ctrl+K", "Search"), ("?", "Every filter"), ("Alt+Enter", "Show in folder")], icon: "search", try_it: Some("files") },
    Step { id: "preview", title: "Look inside files", text: "Word documents, PDF, pictures, video, audio, Markdown, fonts and highlighted code, without opening another app.", keys: &[("Space", "Quick Look"), ("Alt+P", "Preview pane")], icon: "eye", try_it: None },
    Step { id: "archives", title: "Zips open like folders", text: "Browse and preview what's inside zip, 7z, rar and tar files without unpacking them. Then extract all of it, choose where with Extract to, or take only what you've selected.", keys: &[("Enter", "Open it"), ("Backspace", "Back out")], icon: "file-archive", try_it: None },
    Step { id: "tabs", title: "Tabs and split view", text: "Keep your folders open in tabs. Split a tab to see two folders side by side, and copy or move files across.", keys: &[("Ctrl+T", "New tab"), ("Ctrl+\\", "Split view"), ("Tab", "Other pane")], icon: "columns-2", try_it: None },
    Step { id: "jobs", title: "Work while files move", text: "Copies and moves run in the background with live progress, and undo takes them back. Where there's no Recycle Bin, as in WSL and on SSH servers, Ferret asks before deleting for good.", keys: &[("Ctrl+Z", "Undo"), ("Ctrl+Y", "Redo")], icon: "copy", try_it: None },
    Step { id: "dropzone", title: "Dropzone", text: "A parking spot at the bottom of the sidebar: drag files in, browse to where they belong, then drag them out to copy or move them. Nothing moves until then.", keys: &[], icon: "inbox", try_it: None },
    Step { id: "wsl", title: "Your WSL distros", text: "They're listed under Linux in the sidebar, with nothing to set up. Click one and its files open like any other folder.", keys: &[("Ctrl+`", "Terminal here")], icon: "terminal", try_it: None },
    Step { id: "network", title: "NAS and network folders", text: "Computers Windows finds show up under Network in the sidebar. If yours isn't there, press + and type its address, like \\\\nas\\media or 192.168.0.10. Ferret asks for a user name and password when it needs them.", keys: &[("Ctrl+L", "Type a path")], icon: "network", try_it: Some("network") },
    Step { id: "servers", title: "Connect to SSH servers", text: "Add one under Servers in the sidebar, from your ~/.ssh/config or by its address. Its folders browse like any other, a file you edit uploads when you save, and it only connects when you click it.", keys: &[], icon: "server", try_it: None },
    Step { id: "commands", title: "Every command, one list", text: "Forgot a shortcut? Type what you want to do. The palette runs it and shows you the key for next time.", keys: &[("Ctrl+Shift+P", "Command palette")], icon: "command", try_it: Some("commands") },
    Step { id: "setup", title: "Make it yours", text: "Pick a color and a theme, and the window follows as you choose.", keys: &[], icon: "palette", try_it: None },
];

pub struct Tour {
    pub steps: Vec<usize>,
    pub at: usize,
    pub welcome: bool,
    pub replay: bool,
    pub seq: u64,
    /// The demo's frame, and the (seq, frame) its timer was set for.
    pub frame: usize,
    pub timer: Option<(u64, usize)>,
}

/// A step's demo starts at its first frame, or holds its telling one when nothing moves.
fn first_frame(step: usize) -> usize {
    if t::animate() { 0 } else { crate::introdemos::frames(STEPS[step].id).1 }
}

impl Ferret {
    /// On a plain first start, the whole tour; afterwards only steps added since.
    pub fn maybe_intro(&mut self, cx: &mut Context<Self>) {
        if std::env::var_os("FERRET_GPUI_NO_INTRO").is_some() || self.bench.is_some() || self.elevated || std::env::var_os("FERRET_GPUI_OPEN").is_some() {
            return;
        }
        let seen = self.settings.strings("introSeen");
        let unseen: Vec<usize> = STEPS.iter().enumerate().filter(|(_, s)| !seen.iter().any(|x| x == s.id)).map(|(i, _)| i).collect();
        if unseen.is_empty() {
            return;
        }
        let frame = first_frame(unseen[0]);
        self.dialog = Some(Dialog::Intro(Tour { welcome: seen.is_empty(), steps: unseen, at: 0, replay: false, seq: 0, frame, timer: None }));
        cx.notify();
    }

    pub fn start_intro(&mut self, replay: bool, cx: &mut Context<Self>) {
        self.dialog = Some(Dialog::Intro(Tour { steps: (0..STEPS.len()).collect(), at: 0, welcome: false, replay, seq: 0, frame: first_frame(0), timer: None }));
        cx.notify();
    }

    /// Skipping counts as seen, so a skipped step never comes back by itself.
    fn finish_intro(&mut self, skipped: bool, window: &mut Window, cx: &mut Context<Self>) {
        let replay = matches!(&self.dialog, Some(Dialog::Intro(t)) if t.replay);
        let all: Vec<String> = STEPS.iter().map(|s| s.id.to_string()).collect();
        let mut seen = self.settings.strings("introSeen");
        for id in all {
            if !seen.contains(&id) {
                seen.push(id);
            }
        }
        self.settings.set("introSeen", json!(seen));
        self.dialog = None;
        window.focus(&self.focus, cx);
        if skipped && !replay {
            self.toast("The tour is in Settings → About, or the command palette", cx);
        }
        cx.notify();
    }

    fn intro_go(&mut self, to: isize, cx: &mut Context<Self>) {
        if let Some(Dialog::Intro(t)) = self.dialog.as_mut() {
            t.welcome = false;
            t.at = (to.max(0) as usize).min(t.steps.len() - 1);
            t.seq += 1;
            t.frame = first_frame(t.steps[t.at]);
            cx.notify();
        }
    }

    /// The search demo's chapters: straight to one.
    pub fn intro_jump(&mut self, frame: usize, cx: &mut Context<Self>) {
        if let Some(Dialog::Intro(t)) = self.dialog.as_mut() {
            t.frame = frame;
            t.timer = None;
            cx.notify();
        }
    }

    /// The demo's next frame, unless the step or frame changed meanwhile; the last loops to the
    /// first.
    fn intro_tick(&mut self, seq: u64, frame: usize, cx: &mut Context<Self>) {
        if let Some(Dialog::Intro(t)) = self.dialog.as_mut() {
            if t.welcome || t.seq != seq || t.frame != frame {
                return;
            }
            let (n, _) = crate::introdemos::frames(STEPS[t.steps[t.at]].id);
            t.frame = (frame + 1) % n.max(1);
            cx.notify();
        }
    }

    pub fn intro_key(&mut self, key: &str, m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Dialog::Intro(tour)) = self.dialog.as_ref() else { return false };
        let (at, n, welcome) = (tour.at as isize, tour.steps.len() as isize, tour.welcome);
        match key {
            "escape" => self.finish_intro(true, window, cx),
            "right" | "down" | "pagedown" | "enter" => {
                if welcome {
                    self.intro_go(0, cx);
                } else if at + 1 >= n {
                    self.finish_intro(false, window, cx);
                } else {
                    self.intro_go(at + 1, cx);
                }
            }
            "left" | "up" | "pageup" => self.intro_go(at - 1, cx),
            "home" => self.intro_go(0, cx),
            "end" => self.intro_go(n - 1, cx),
            "k" if m.control => self.open_palette(None, window, cx),
            "p" if m.control && m.shift => self.open_palette(Some(">".into()), window, cx),
            _ => return false,
        }
        true
    }

    pub fn intro_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(Dialog::Intro(tour)) = self.dialog.as_mut() else { return div().into_any_element() };
        let (welcome, at, seq, replay, frame) = (tour.welcome, tour.at, tour.seq, tour.replay, tour.frame);
        let steps = tour.steps.clone();
        // The demo's clock: one timer per frame shown.
        if !welcome && t::animate() && crate::introdemos::frames(STEPS[steps[at]].id).0 > 1 && tour.timer != Some((seq, frame)) {
            tour.timer = Some((seq, frame));
            let ms = crate::introdemos::ms(STEPS[steps[at]].id, frame);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                this.update(cx, |f, cx| f.intro_tick(seq, frame, cx)).ok();
            })
            .detach();
        }
        let vw = window.viewport_size();
        let w = (vw.width.as_f32() - 80.).clamp(560., 980.);
        let h = (vw.height.as_f32() - 80.).clamp(420., 620.);
        let card = ui::modal_card(w).h(px(h)).flex_row().id("intro").role(gpui::Role::Dialog).aria_label(if welcome { "Welcome to Ferret" } else { "Ferret tour" });
        let body: AnyElement = if welcome {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(16.))
                .p(px(40.))
                .child(ui::logo(104.))
                .child(div().id("intro-welcome").role(gpui::Role::Heading).aria_label("Welcome to Ferret").aria_description("A fast, keyboard-first file manager. Here's what it can do, in about two minutes.").text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).child("Welcome to Ferret"))
                .child(div().text_color(t::fg2()).text_center().max_w(px(460.)).child("A fast, keyboard-first file manager. Here's what it can do, in about two minutes."))
                .child(div().flex().flex_wrap().justify_center().gap(px(6.)).max_w(px(560.)).children(steps.iter().map(|&i| {
                    div().flex().items_center().gap(px(5.)).px(px(8.)).py(px(3.)).rounded(px(6.)).bg(t::hover()).text_size(px(12.)).text_color(t::fg2()).child(icon(STEPS[i].icon, 12., t::accent_text())).child(STEPS[i].title)
                })))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .mt(px(8.))
                        .child(ui::button("intro-skip", "Skip", false).on_click(cx.listener(|f, _, window, cx| f.finish_intro(true, window, cx))))
                        .child(ui::button("intro-start", "Show me around", true).on_click(cx.listener(|f, _, _, cx| f.intro_go(0, cx)))),
                )
                .into_any_element()
        } else {
            let s = &STEPS[steps[at]];
            let last = at + 1 == steps.len();
            let list = div().w(px(230.)).flex_none().h_full().p(px(14.)).border_r_1().border_color(t::line()).flex().flex_col().gap(px(2.)).children(steps.iter().enumerate().map(|(k, &i)| {
                let on = k == at;
                div()
                    .id(("intro-step", k))
                    .role(gpui::Role::Tab)
                    .aria_label(STEPS[i].title)
                    .aria_selected(on)
                    .h(px(30.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(7.))
                    .text_color(if on { t::fg() } else { t::fg2() })
                    .when(on, |d| d.bg(t::accent_soft()))
                    .when(!on, |d| d.hover(|s| s.bg(t::hover())))
                    .on_click(cx.listener(move |f, _, _, cx| f.intro_go(k as isize, cx)))
                    .child(icon(STEPS[i].icon, 14., if on { t::accent_text() } else { t::fg3() }))
                    .child(div().truncate().child(STEPS[i].title))
            }));
            let content = div()
                .flex_1()
                .min_w_0()
                .h_full()
                .p(px(28.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(div().text_size(px(12.)).text_color(t::fg3()).child(format!("{} of {}", at + 1, steps.len())))
                .child(
                    div()
                        .id(("intro-title", at))
                        .role(gpui::Role::Heading)
                        .aria_label(format!("{}, {} of {}", s.title, at + 1, steps.len()))
                        .aria_description(s.text)
                        .text_size(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(s.title),
                )
                .child(div().text_color(t::fg2()).line_height(relative(1.5)).child(s.text))
                .child(div().flex_1().min_h(px(220.)).child(crate::introdemos::view(s.id, frame, cx)))
                .child(div().flex().flex_wrap().gap(px(12.)).children(s.keys.iter().map(|(k, d)| div().flex().items_center().gap(px(6.)).child(ui::kbd(*k)).child(div().text_size(px(12.)).text_color(t::fg2()).child(*d)))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::button("intro-close", if replay { "Close" } else { "Skip" }, false).on_click(cx.listener(|f, _, window, cx| f.finish_intro(true, window, cx))))
                        .child(div().flex_1())
                        .when_some(s.try_it, |d, what| {
                            d.child(ui::button("intro-try", if what == "network" { "Add one" } else { "Try it" }, false).on_click(cx.listener(move |f, _, window, cx| match what {
                                "files" => f.open_palette(None, window, cx),
                                "commands" => f.open_palette(Some(">".into()), window, cx),
                                _ => f.open_network_form(window, cx),
                            })))
                        })
                        .when(at > 0, |d| d.child(ui::button("intro-back", "Back", false).on_click(cx.listener(move |f, _, _, cx| f.intro_go(at as isize - 1, cx)))))
                        .child(ui::button("intro-next", if last { "Done" } else { "Next" }, true).on_click(cx.listener(move |f, _, window, cx| {
                            if last { f.finish_intro(false, window, cx) } else { f.intro_go(at as isize + 1, cx) }
                        }))),
                );
            let content = if t::animate() {
                content.with_animation(("intro-in", seq as usize), Animation::new(Duration::from_millis(220)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k).ml(px((1. - k) * 12.))).into_any_element()
            } else {
                content.into_any_element()
            };
            div().size_full().flex().child(list).child(content).into_any_element()
        };
        let el = div().id("intro-scrim").absolute().top_0().left_0().size_full().flex().items_center().justify_center().bg(t::scrim()).occlude().child(card.child(body));
        if t::animate() {
            el.with_animation("intro-scrim-in", Animation::new(Duration::from_millis(180)), |d, k| d.opacity(k)).into_any_element()
        } else {
            el.into_any_element()
        }
    }
}
