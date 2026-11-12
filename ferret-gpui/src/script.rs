//! A test driver: FERRET_GPUI_SCRIPT runs steps through GPUI's own input dispatch (keys, typing,
//! clicks, drags, the wheel), so tests never need the window in front or the real keyboard and
//! mouse (which the user may be using). Steps are separated by `;` or new lines:
//!
//!   key ctrl-t | type some text | click x y | rclick x y | dclick x y | tclick x y (triple) | mclick x y | move x y
//!   sclick x y (Shift+click) | hscroll x y dx (sideways, as Shift+wheel or a trackpad)
//!   drag x0 y0 x1 y1 | scroll x y dy | zoom x y dy (Ctrl+wheel) | wait ms | shot name | state | quit
//!   down x y | hold x y | up x y (the left button in steps; mdown/mhold/mup: the middle one)
//!   fenter x y path[|path] | fmove x y | fexit | fdrop x y (files dragged in) | owndrag on|off
//!   settings [sidebar|addons|about] | addon download|cancel|remove|keep|verify|loaded
//!   searchable [path] (a searchable copy of the PDF read with OCR)
//!
//! `shot` writes `shot <name>` to FERRET_GPUI_SCRIPT_OUT and waits for the harness to capture
//! the window (PrintWindow). `state` writes what the window shows as JSON, to check without
//! looking. Positions are in window pixels (logical).

use std::io::Write;
use std::time::Duration;

use gpui::{
    AsyncWindowContext, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput,
    ScrollDelta, ScrollEvent, WeakEntity, point, px,
};

use crate::view::Ferret;

/// Busy on the UI thread for `d` (the `stall` step), in a function of its own so the watchdog's
/// log can be checked for its name.
#[inline(never)]
fn script_stall_for_the_watchdog(d: std::time::Duration) {
    let t = std::time::Instant::now();
    let mut x = 0u64;
    while t.elapsed() < d {
        x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
    }
    std::hint::black_box(x);
}

fn out(line: &str) {
    if let Ok(path) = std::env::var("FERRET_GPUI_SCRIPT_OUT") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn keystroke(s: &str) -> Option<Keystroke> {
    Keystroke::parse(Box::leak(s.to_string().into_boxed_str())).ok()
}

pub fn start(this: WeakEntity<Ferret>, window: &mut gpui::Window, cx: &mut gpui::Context<Ferret>) {
    let Ok(script) = std::env::var("FERRET_GPUI_SCRIPT") else { return };
    let script = std::fs::read_to_string(&script).unwrap_or(script).trim_start_matches('\u{feff}').to_string();
    cx.spawn_in(window, async move |_, cx| {
        cx.background_executor().timer(Duration::from_millis(900)).await;
        for step in script.split(['\n', ';']).map(str::trim).filter(|s| !s.is_empty() && !s.starts_with('#')) {
            run(step, &this, cx).await;
            cx.background_executor().timer(Duration::from_millis(60)).await;
        }
        out("end");
    })
    .detach();
}

async fn run(step: &str, this: &WeakEntity<Ferret>, cx: &mut AsyncWindowContext) {
    let (cmd, rest) = step.split_once(' ').unwrap_or((step, ""));
    let nums: Vec<f32> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    let at = |i: usize| point(px(nums.get(i).copied().unwrap_or(0.)), px(nums.get(i + 1).copied().unwrap_or(0.)));
    let dispatch = |cx: &mut AsyncWindowContext, ev: PlatformInput| {
        let _ = cx.update(|window, cx| {
            window.dispatch_event(ev, cx);
        });
    };
    let click_with = |cx: &mut AsyncWindowContext, button: MouseButton, count: usize, modifiers: Modifiers| {
        let p = at(0);
        dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: None, modifiers }));
        for n in 1..=count {
            dispatch(cx, PlatformInput::MouseDown(MouseDownEvent { button, position: p, modifiers, click_count: n, first_mouse: false }));
            dispatch(cx, PlatformInput::MouseUp(MouseUpEvent { button, position: p, modifiers, click_count: n }));
        }
    };
    let click = |cx: &mut AsyncWindowContext, button: MouseButton, count: usize| click_with(cx, button, count, Modifiers::default());
    match cmd {
        "key" => {
            for k in rest.split_whitespace() {
                if let Some(ks) = keystroke(k) {
                    let _ = cx.update(|window, cx| window.dispatch_keystroke(ks, cx));
                }
            }
        }
        "type" => {
            for c in rest.chars() {
                let s = if c == ' ' { "space-> ".to_string() } else { format!("{c}->{c}") };
                if let Some(ks) = keystroke(&s) {
                    let _ = cx.update(|window, cx| window.dispatch_keystroke(ks, cx));
                }
            }
        }
        "click" => click(cx, MouseButton::Left, 1),
        "dclick" => click(cx, MouseButton::Left, 2),
        "tclick" => click(cx, MouseButton::Left, 3),
        "rclick" => click(cx, MouseButton::Right, 1),
        "mclick" => click(cx, MouseButton::Middle, 1),
        "sclick" => click_with(cx, MouseButton::Left, 1, Modifiers { shift: true, ..Default::default() }),
        "move" => dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: at(0), pressed_button: None, modifiers: Modifiers::default() })),
        "drag" => {
            let (a, b) = (at(0), at(2));
            dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: a, pressed_button: None, modifiers: Modifiers::default() }));
            dispatch(cx, PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: a, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }));
            for i in 1..=12 {
                let k = i as f32 / 12.;
                let p = point(a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k);
                dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }));
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
            dispatch(cx, PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: b, modifiers: Modifiers::default(), click_count: 1 }));
        }
        // A button pressed, moved with it held, and let go (separate steps, to wait in between).
        "down" | "mdown" => {
            let button = if cmd == "down" { MouseButton::Left } else { MouseButton::Middle };
            dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: at(0), pressed_button: None, modifiers: Modifiers::default() }));
            dispatch(cx, PlatformInput::MouseDown(MouseDownEvent { button, position: at(0), modifiers: Modifiers::default(), click_count: 1, first_mouse: false }));
        }
        "hold" | "mhold" => {
            let button = if cmd == "hold" { MouseButton::Left } else { MouseButton::Middle };
            dispatch(cx, PlatformInput::MouseMove(MouseMoveEvent { position: at(0), pressed_button: Some(button), modifiers: Modifiers::default() }));
        }
        "up" | "mup" => {
            let button = if cmd == "up" { MouseButton::Left } else { MouseButton::Middle };
            dispatch(cx, PlatformInput::MouseUp(MouseUpEvent { button, position: at(0), modifiers: Modifiers::default(), click_count: 1 }));
        }
        // Files dragged in from outside: they come over the window at x y (`fenter x y path`,
        // several separated by `|`), move (`fmove x y`) and leave again (`fexit`), or are
        // dropped at x y (`fdrop x y`: a real drop, so aim it only at the Dropzone in tests).
        "fenter" => {
            let path = rest.split_whitespace().skip(2).collect::<Vec<_>>().join(" ");
            let paths = gpui::ExternalPaths(path.split('|').map(|p| std::path::PathBuf::from(p.trim())).collect::<Vec<_>>().into());
            dispatch(cx, PlatformInput::FileDrop(gpui::FileDropEvent::Entered { position: at(0), paths }));
        }
        "fmove" => dispatch(cx, PlatformInput::FileDrop(gpui::FileDropEvent::Pending { position: at(0) })),
        "fexit" => dispatch(cx, PlatformInput::FileDrop(gpui::FileDropEvent::Exited)),
        "fdrop" => dispatch(cx, PlatformInput::FileDrop(gpui::FileDropEvent::Submit { position: at(0) })),
        // Files dragged from the window's own list come back over it as the same OLE drag
        // (`fenter`…), marked as its own by `drag_out` (copy/move label by the pointer):
        // `owndrag on|off` marks them so, without a real shell drag (it would follow the real mouse).
        "owndrag" => {
            let _ = this.update(cx, |f, cx| {
                f.own_drag = rest.trim() != "off";
                cx.notify();
            });
        }
        "scroll" | "zoom" => dispatch(
            cx,
            PlatformInput::Scroll(ScrollEvent {
                position: at(0),
                delta: ScrollDelta::Pixels(point(px(0.), px(nums.get(2).copied().unwrap_or(-100.)))),
                // `zoom`: with Ctrl held (Word pages, pictures).
                modifiers: Modifiers { control: cmd == "zoom", ..Default::default() },
                touch_phase: Default::default(),
                source: Default::default(),
            }),
        ),
        "hscroll" => dispatch(
            cx,
            PlatformInput::Scroll(ScrollEvent {
                position: at(0),
                delta: ScrollDelta::Pixels(point(px(nums.get(2).copied().unwrap_or(-100.)), px(0.))),
                modifiers: Modifiers::default(),
                touch_phase: Default::default(),
                source: Default::default(),
            }),
        ),
        "wait" => cx.background_executor().timer(Duration::from_millis(nums.first().copied().unwrap_or(300.) as u64)).await,
        "shot" => {
            out(&format!("shot {rest}"));
            cx.background_executor().timer(Duration::from_millis(700)).await;
        }
        // A named point in the run, for a harness filming the window (film.ps1) to line up with.
        "mark" => out(&format!("mark {rest}")),
        "state" => {
            let s = this.update(cx, |f, cx| f.state_json(cx)).unwrap_or_default();
            out(&format!("state {rest} {s}"));
        }
        "quit" => {
            let _ = cx.update(|_, cx| cx.quit());
        }
        // The updater (`update check|whatsnew|install`), as Settings' button, the sidebar's dot
        // and "Update now" do.
        "update" => {
            let _ = this.update(cx, |f, cx| match rest {
                "check" => f.check_for_update(true, cx),
                "whatsnew" => f.show_whats_new(cx),
                "install" => f.install_update(cx),
                _ => {}
            });
        }
        // Settings opened (`settings`, or at a card: `settings sidebar|addons|about`).
        "settings" => {
            let tab = match rest {
                "sidebar" => crate::settings_view::SettingsTab::Sidebar,
                "addons" => crate::settings_view::SettingsTab::Addons,
                "about" => crate::settings_view::SettingsTab::About,
                _ => crate::settings_view::SettingsTab::Top,
            };
            let _ = this.update(cx, |f, cx| f.open_settings_at(tab, cx));
        }
        // The text recognition add-on (`addon download|cancel|remove|keep|verify`), as Settings'
        // buttons and ocr.rs do; `addon loaded` says its DLL was loaded (removal then waits).
        "addon" => {
            let _ = cx.update(|_, cx| match rest {
                "download" => crate::addons::start_ocr_download(cx),
                "cancel" => crate::addons::cancel_ocr_download(),
                "remove" => out(&format!("addon remove {:?}", crate::addons::remove_ocr_pack())),
                "keep" => crate::addons::keep_ocr_pack(),
                "verify" => out(&format!("addon verify {:?}", crate::addons::verify_ocr_pack())),
                "loaded" => crate::addons::mark_ocr_loaded(),
                _ => out(&format!("unknown step: {step}")),
            });
            let _ = this.update(cx, |_, cx| cx.notify());
        }
        // A job's question answered, as its card's buttons do (`answer skip`: retry, skip,
        // skipAll, cancel); every job waiting gets it.
        "answer" => {
            let waiting = this.update(cx, |f, _| f.jobs.iter().filter(|j| j.question.is_some()).map(|j| j.id).collect::<Vec<_>>()).unwrap_or_default();
            out(&format!("answered {} with {rest}", waiting.len()));
            for id in waiting {
                ferret_core::jobs::answer_job(id, rest.to_string());
            }
        }
        // "Save as searchable PDF" (searchable.rs), as the scanned PDF note's button does: of the
        // PDF given, else the one whose text the preview read.
        "searchable" => {
            let _ = this.update(cx, |f, cx| {
                let path = if rest.is_empty() { f.ocr.as_ref().map(|o| o.path.clone()) } else { Some(rest.to_string()) };
                match path {
                    Some(p) => f.save_searchable_pdf(p, cx),
                    None => out("searchable: no PDF"),
                }
            });
        }
        // The GPU device lost, as after a driver reset: GPUI's recovery runs for real.
        "devicelost" => gpui_windows::ferret::simulate_device_lost(),
        // Holds the UI thread busy for <ms> (the watchdog's test: it should log where).
        "stall" => script_stall_for_the_watchdog(std::time::Duration::from_millis(nums.first().copied().unwrap_or(1500.) as u64)),
        // The window moved or resized (`resize x y w h`, physical pixels, like a user's drag).
        "resize" => {
            let _ = cx.update(|window, _| {
                if let Some(h) = crate::view::hwnd(window) {
                    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};
                    let n = |i: usize| nums.get(i).copied().unwrap_or(0.) as i32;
                    // Posted, not sent from inside the app's update: see `apply_placement`.
                    let (x, y, w, hh) = (n(0), n(1), n(2), n(3));
                    std::thread::spawn(move || unsafe {
                        let _ = SetWindowPos(windows::Win32::Foundation::HWND(h as *mut _), None, x, y, w, hh, SWP_NOZORDER | SWP_NOACTIVATE);
                    });
                }
            });
        }
        _ => out(&format!("unknown step: {step}")),
    }
}

impl Ferret {
    /// What the window shows, for tests: tabs, the pane in use, selection, view, open overlays.
    pub fn state_json(&self, cx: &gpui::App) -> String {
        let p = self.pane();
        let sel: Vec<String> = p.marked_rows().iter().map(|r| r.e.name.clone()).collect();
        let rows: Vec<String> = p.visible_rows().take(12).map(|i| p.rows[i].e.name.clone()).collect();
        let dialog = self.dialog.as_ref().map(|d| match d {
            crate::dialogs::Dialog::Confirm { title, .. } => format!("confirm: {title}"),
            crate::dialogs::Dialog::Conflicts { .. } => "conflicts".into(),
            crate::dialogs::Dialog::Admin { .. } => "admin".into(),
            crate::dialogs::Dialog::WhatsNew => "whats-new".into(),
            crate::dialogs::Dialog::Properties { .. } => "properties".into(),
            crate::dialogs::Dialog::BatchRename(_) => "batch-rename".into(),
            crate::dialogs::Dialog::Prompt { title, .. } => format!("prompt: {title}"),
            crate::dialogs::Dialog::Remote { title, .. } => format!("remote: {title}"),
            crate::dialogs::Dialog::Settings => "settings".into(),
            crate::dialogs::Dialog::Server(_) => "server-form".into(),
            crate::dialogs::Dialog::NetworkAdd(_) => "network-form".into(),
            crate::dialogs::Dialog::Intro(_) => "intro".into(),
            crate::dialogs::Dialog::Image(_) => "image-editor".into(),
        });
        let col_widths: Vec<f32> =
            [crate::settings::Column::Modified, crate::settings::Column::Type, crate::settings::Column::Size].into_iter().map(|c| self.settings.column_width(c)).collect();
        let menu: Option<Vec<String>> = self.menu.as_ref().map(|m| m.levels.last().map(|l| l.items.iter().filter(|i| !i.label.is_empty()).map(|i| i.label.to_string()).collect()).unwrap_or_default());
        let mut v = serde_json::json!({
            "tabs": self.tabs.iter().map(|t| t.pane().path.clone()).collect::<Vec<_>>(),
            "active": self.active,
            "split": self.tab().split.is_some(),
            "path": p.path,
            "items": p.rows.len(),
            "groups": p.groups.iter().map(|g| format!("{} ({})", g.label, g.len)).collect::<Vec<_>>(),
            "first": rows,
            "cursor": p.cursor_row().map(|r| r.e.name.clone()),
            "selected": sel,
            "view": p.view.to_json(),
            "filter": p.filter,
            "error": p.error,
            "dialog": dialog,
            "menu": menu,
            "palette": self.palette.is_some(),
            "pal": self.palette.as_ref().map(|(p, _)| p.read(cx).state(cx)),
            "saved_searches": self.settings.saved_searches(),
            "history": self.settings.strings("searchHistory"),
            "quicklook": self.quicklook,
            "renaming": self.rename.as_ref().map(|r| r.input.read(cx).text().to_string()),
            "toasts": self.toasts.iter().map(|t| t.text.to_string()).collect::<Vec<_>>(),
            "jobs": self.jobs.iter().map(|j| format!("{} {}", j.label, j.status)).collect::<Vec<_>>(),
            "undo": self.undo.last().map(|s| s.label.clone()),
            "favorites": self.favorites.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
            "shelf": self.shelf,
            "preview": self.preview_data.as_ref().map(|l| l.key.split('|').next().unwrap_or("").to_string()),
            "kbd": format!("{:?}", self.kbd),
            "side_items": self.side_targets.borrow().len(),
            "sidebar_w": self.sidebar_w,
            "preview_w": self.preview_w,
            "col_widths": col_widths,
            "autoscroll": self.autoscroll.as_ref().map(|a| if a.sticky { "sticky" } else { "held" }),
            "scroll_y": self.scroll_y(p.id),
            "pdf_pages": self.preview_data.as_ref().and_then(|l| match &l.content {
                crate::render::Content::Pdf { pages, count, .. } => Some(format!("{}/{}/{count}", pages.iter().filter(|p| p.0.is_some()).count(), pages.len())),
                _ => None,
            }),
            "word_zoom": self.preview_view.word_zoom,
            "preview_wrap": self.preview_view.wrap,
            "pdf_scroll": self.pdf_scroll.iter().map(|h| format!("{:.0}@{:.0}", -h.offset().y.as_f32(), h.bounds().size.height.as_f32())).collect::<Vec<_>>(),
            "pdf_pending": self.pdf_pending,
            "audio": self.audio.as_ref().map(|(_, pl)| format!("{:.1}/{:.1}", pl.position(), pl.duration())),
        });
        // A PDF asking for its password (and what its field says), and its text layer's lines.
        v["pdf_locked"] = serde_json::json!(self.preview_data.as_ref().is_some_and(|l| matches!(l.content, crate::render::Content::PdfLocked { .. })));
        v["pdf_unlock"] = serde_json::json!(self.pdf_unlock.as_ref().map(|u| serde_json::json!({
            "error": u.error.as_ref().map(|e| e.to_string()),
            "busy": u.busy,
            "typed": u.input.read(cx).text().chars().count(),
        })));
        v["pdf_text_lines"] = serde_json::json!(self.pdf_text.as_ref().and_then(|(_, t)| t.as_ref().map(|t| t.lines())));
        // Text recognition of its scanned pages (ocr.rs), and the text layer's start (copy order).
        v["ocr"] = serde_json::json!(self.ocr.as_ref().map(|o| serde_json::json!({
            "total": o.total,
            "done": o.done,
            "reader": format!("{:?}", o.reader),
            "found": o.found_any,
            "problem": o.problem,
        })));
        v["pdf_text"] = serde_json::json!(self.pdf_text.as_ref().and_then(|(_, t)| t.as_ref()).map(|t| {
            let mut s = String::new();
            for l in t.pages.iter().flatten() {
                s.push_str(&l.text);
                s.push(if l.row_goes_on { '\t' } else { '\n' });
            }
            s.chars().take(1200).collect::<String>()
        }));
        // The preview's selected text (the pane's, Quick Look's), and the text copied last.
        v["text_sel"] = serde_json::json!(self.text_sel.iter().map(|s| s.borrow().describe()).collect::<Vec<_>>());
        v["text_copied"] = serde_json::json!(self.text_copied.as_ref().map(|t| t.chars().take(400).collect::<String>()));
        // The text recognition add-on (addons.rs).
        v["ocr_pack"] = serde_json::json!(format!("{:?}", crate::addons::ocr_pack()));
        v.to_string()
    }
}
