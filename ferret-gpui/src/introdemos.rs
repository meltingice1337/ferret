//! The intro tour's demos (IntroDemos.tsx): a few seconds of Ferret each, drawn with the app's
//! own colors so they follow the accent and the theme. They only show; nothing here touches a
//! file. Each runs through numbered frames (`timing`), as the app's `useFrames`; with animations
//! off it holds its most telling frame.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, FontWeight, HighlightStyle, Hsla, Image, ImageFormat, ObjectFit, StyledText, div,
    prelude::*, px, relative,
};

use crate::imgmem::img;

use crate::kinds;
use crate::theme as t;
use crate::ui::icon;
use crate::view::Ferret;

// ───────────────────────────── timing ─────────────────────────────

/// A demo's frames: how many, and the one it holds when nothing moves.
pub fn frames(id: &str) -> (usize, usize) {
    match id {
        "search" => (scenes().len(), last_of(0)),
        "preview" => (SHOWN.len(), SHOWN.len() - 1),
        "archives" => (6, 5),
        "tabs" => (4, 3),
        "jobs" => (PROGRESS.len() + 1, PROGRESS.len()),
        "dropzone" => (POINTER.len(), POINTER.len() - 1),
        "wsl" => (4, 2),
        "network" => (7, NET_SHARE),
        "servers" => (4, 3),
        "commands" => (typed().len(), typed().len() - 1),
        _ => (1, 0),
    }
}

/// How long frame `i` shows (ms).
pub fn ms(id: &str, i: usize) -> u64 {
    let at = |list: &[u64]| list.get(i).copied().unwrap_or(1500);
    match id {
        "search" => scenes().get(i).map_or(1000, |s| s.ms),
        "preview" => if i == SHOWN.len() - 1 { 3200 } else { 2100 },
        "archives" => at(&[1900, 2300, 2500, 2600, 1500, 3400]),
        "tabs" => at(&[1400, 1200, 1000, 3000]),
        "jobs" => if i < PROGRESS.len() - 1 { 420 } else if i == PROGRESS.len() - 1 { 500 } else { 3000 },
        "dropzone" => at(&[1200, 900, 900, 800, 900, 800, 900, 3200]),
        "wsl" => at(&[1500, 2000, 1500, 3400]),
        "network" => at(&[1500, 2800, 1000, 1700, 1800, 3000, 3000]),
        "servers" => at(&[1300, 2600, 1300, 3400]),
        "commands" => {
            let t = typed();
            if t.get(i) == t.get(i + 1) { 1900 } else if t.get(i).is_some_and(|s| s.is_empty()) { 700 } else { 90 }
        }
        _ => 60_000,
    }
}

/// The demo for a step, at a frame.
pub fn view(id: &str, frame: usize, cx: &mut Context<Ferret>) -> AnyElement {
    match id {
        "search" => search(frame, cx),
        "preview" => preview(frame),
        "archives" => zip(frame),
        "tabs" => tabs(frame),
        "jobs" => jobs(frame),
        "dropzone" => dropzone(frame),
        "wsl" => wsl(frame),
        "network" => network(frame),
        "servers" => servers(frame),
        "commands" => commands(frame),
        _ => setup(),
    }
}

// ───────────────────────────── parts ─────────────────────────────

fn win() -> Div {
    div().size_full().relative().overflow_hidden().rounded(px(10.)).bg(t::hover()).border_1().border_color(t::line2()).flex().flex_col().text_size(px(12.5)).text_color(t::fg())
}

fn file_icon(name: &str, dir: bool, size: f32) -> AnyElement {
    if dir {
        return icon("folder", size, t::folder()).into_any_element();
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let kind = kinds::kind_of(false, &ext);
    icon(kinds::icon(kind), size, kinds::color(kind)).into_any_element()
}

fn row(name: &str, dir: bool, size: Option<&str>, on: bool) -> Div {
    div()
        .h(px(26.))
        .flex_none()
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(6.))
        .when(on, |d| d.bg(t::accent_soft()))
        .child(file_icon(name, dir, 14.))
        .child(div().flex_1().min_w_0().truncate().child(name.to_string()))
        .children(size.map(|s| div().flex_none().text_size(px(11.5)).text_color(t::fg3()).child(s.to_string())))
}

/// Something that just arrived: it fades and slides in once.
fn arrives(el: Div, key: &'static str, frame: usize) -> AnyElement {
    if !t::animate() {
        return el.into_any_element();
    }
    el.with_animation((key, frame), Animation::new(Duration::from_millis(320)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k).ml(px((1. - k) * -8.)))
        .into_any_element()
}

/// A popup appearing (a form, a menu): fades and grows in.
/// (On the element itself, not a wrapper: an absolute overlay must stay the demo's child.)
fn pops(el: Div, key: &'static str, frame: usize) -> AnyElement {
    if !t::animate() {
        return el.into_any_element();
    }
    el.with_animation((key, frame), Animation::new(Duration::from_millis(220)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k).mt(px((1. - k) * 6.)))
        .into_any_element()
}

fn crumbs(parts: &[&str], marked: bool) -> Div {
    let n = parts.len();
    div()
        .h(px(32.))
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(5.))
        .border_b_1()
        .border_color(t::line())
        .text_size(px(12.))
        .children(parts.iter().enumerate().map(|(i, p)| {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .when(i > 0, |d| d.child(div().text_color(t::fg3()).child("›")))
                .child(
                    div()
                        .text_color(if i == 0 && marked { t::ok() } else if i + 1 == n { t::fg() } else { t::fg2() })
                        .when(i + 1 == n, |d| d.font_weight(FontWeight::SEMIBOLD))
                        .child(p.to_string()),
                )
        }))
}

/// The mouse: `x` is a share of the demo's width (%), `y` pixels from its top, or (`mid`) from
/// its middle.
fn pointer(x: f32, y: f32, mid: bool, ghost: Option<String>) -> AnyElement {
    let at = div().absolute().left(relative(x / 100.));
    let at = if mid { at.top(relative(0.5)).mt(px(y)) } else { at.top(px(y)) };
    at.child(
        div()
            .relative()
            .child(icon("ix-pointer", 20., t::rgba(0x111111, 1.)))
            .child(div().absolute().left(px(1.6)).top(px(2.2)).child(icon("ix-pointer", 15., t::rgba(0xffffff, 1.))))
            .children(ghost.map(|g| {
                div()
                    .absolute()
                    .left(px(16.))
                    .top(px(18.))
                    .px(px(7.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .bg(t::raised())
                    .border_1()
                    .border_color(t::accent_line())
                    .shadow(t::shadow())
                    .whitespace_nowrap()
                    .text_size(px(11.5))
                    .child(g)
            })),
    )
    .into_any_element()
}

fn btn(label: &str, accent: bool) -> Div {
    div()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .text_size(px(12.))
        .when(accent, |d| d.bg(t::accent()).text_color(t::on_accent()))
        .when(!accent, |d| d.bg(t::hover()).border_1().border_color(t::line2()))
        .child(label.to_string())
}

/// A small dialog over the demo's middle: a title, label/value rows, and its buttons.
fn form(title: &str, rows: &[(&str, &str)], extra: Option<AnyElement>, busy: bool, ok: &str) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t::scrim())
        .child(
            div()
                .w(px(300.))
                .p(px(14.))
                .rounded(px(10.))
                .bg(t::raised())
                .border_1()
                .border_color(t::line2())
                .shadow(t::shadow())
                .flex()
                .flex_col()
                .gap(px(7.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child(title.to_string()))
                .children(rows.iter().map(|(l, v)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().w(px(110.)).flex_none().text_size(px(11.5)).text_color(t::fg3()).child(l.to_string()))
                        .child(div().flex_1().min_w_0().truncate().px(px(8.)).py(px(3.)).rounded(px(5.)).bg(t::hover()).border_1().border_color(t::line()).child(v.to_string()))
                }))
                .children(extra)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(px(6.))
                        .mt(px(4.))
                        .when(busy, |d| d.child(div().flex().items_center().gap(px(5.)).text_size(px(11.5)).text_color(t::fg2()).child(crate::ui::spinner(13., t::fg2())).child("Connecting…")))
                        .child(btn("Cancel", false))
                        .child(btn(ok, true)),
                ),
        )
}

fn job(icon_name: &'static str, label: &str, pct: u32) -> Div {
    div()
        .w(px(270.))
        .p(px(10.))
        .rounded(px(10.))
        .bg(t::raised())
        .border_1()
        .border_color(t::line2())
        .shadow(t::shadow())
        .flex()
        .flex_col()
        .gap(px(7.))
        .child(div().flex().items_center().gap(px(7.)).child(icon(icon_name, 14., t::accent_text())).child(div().flex_1().font_weight(FontWeight::SEMIBOLD).text_size(px(12.)).child(label.to_string())).child(icon("x", 12., t::fg3())))
        .child(div().h(px(5.)).w_full().rounded(px(3.)).bg(t::line2()).child(div().h_full().w(relative(pct as f32 / 100.)).rounded(px(3.)).bg(t::accent())))
        .child(div().text_size(px(11.)).text_color(t::fg3()).child(format!("{pct}%")))
}

fn toast(text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(14.))
        .px(px(12.))
        .py(px(7.))
        .rounded(px(9.))
        .bg(t::raised())
        .border_1()
        .border_color(t::line2())
        .shadow(t::shadow())
        .text_size(px(12.))
        .child(text.to_string())
        .child(div().text_color(t::accent_text()).font_weight(FontWeight::SEMIBOLD).child("Undo"))
}

/// Bottom right of the demo (job cards, toasts).
fn corner(el: Div, key: &'static str, frame: usize) -> AnyElement {
    div().absolute().right(px(12.)).bottom(px(12.)).child(pops(el, key, frame)).into_any_element()
}

fn side_item(icon_name: &'static str, name: &str, on: bool, badge: Option<&str>) -> Div {
    div()
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(7.))
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(if on { t::fg() } else { t::fg2() })
        .when(on, |d| d.bg(t::accent_soft()))
        .child(icon(icon_name, 13., if on { t::fg() } else { t::fg2() }))
        .child(div().flex_1().min_w_0().truncate().child(name.to_string()))
        .children(badge.map(|b| div().px(px(5.)).rounded(px(4.)).bg(t::hover()).border_1().border_color(t::line2()).text_size(px(10.)).text_color(t::fg3()).child(b.to_string())))
}

fn side_title(title: &str) -> Div {
    div().px(px(8.)).pt(px(8.)).pb(px(3.)).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(t::fg3()).child(title.to_uppercase())
}

/// A sidebar on the left and a pane on the right.
fn split(side: Div, pane: impl IntoElement) -> Div {
    win().flex_row().child(side.w(px(170.)).flex_none().h_full().p(px(6.)).border_r_1().border_color(t::line()).flex().flex_col().gap(px(1.))).child(
        div().flex_1().min_w_0().h_full().flex().flex_col().child(pane),
    )
}

fn list(items: &[(&str, bool, Option<&str>)], on: Option<usize>) -> Div {
    div().p(px(6.)).flex().flex_col().gap(px(1.)).children(items.iter().enumerate().map(|(i, (n, d, s))| row(n, *d, *s, on == Some(i))))
}

fn days_ago(n: i64) -> String {
    (chrono::Local::now() - chrono::Duration::days(n)).format("%Y-%m-%d").to_string()
}

fn ferret_png() -> Arc<Image> {
    static P: OnceLock<Arc<Image>> = OnceLock::new();
    P.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../public/ferret-256.png").to_vec()))).clone()
}

fn mark() -> HighlightStyle {
    HighlightStyle { color: Some(t::accent_text()), font_weight: Some(FontWeight::BOLD), ..Default::default() }
}

fn kbd(k: &str) -> Div {
    div().px(px(6.)).py(px(1.)).rounded(px(5.)).border_1().border_color(t::line2()).bg(t::raised()).text_size(px(11.)).text_color(t::fg2()).child(k.to_string())
}

// ───────────────────────────── search ─────────────────────────────

struct Hit {
    name: &'static str,
    drive: &'static str,
    path: &'static str,
    size: &'static str,
    days: i64,
}
const HITS: [Hit; 5] = [
    Hit { name: "invoice-november.pdf", drive: "E", path: "Archive\\Finance", size: "240 KB", days: 2 },
    Hit { name: "invoice-october.pdf", drive: "D", path: "Finance\\2026", size: "212 KB", days: 5 },
    Hit { name: "invoices.xlsx", drive: "C", path: "Users\\you\\Documents", size: "64 KB", days: 10 },
    Hit { name: "invoice-april.pdf", drive: "D", path: "Finance\\2026", size: "198 KB", days: 200 },
    Hit { name: "invoice-template.docx", drive: "C", path: "Users\\you\\Templates", size: "31 KB", days: 330 },
];
struct Line {
    name: &'static str,
    path: &'static str,
    size: &'static str,
    days: i64,
    no: u32,
    before: &'static str,
    found: &'static str,
    after: &'static str,
    more: u32,
}
const LINES: [Line; 3] = [
    Line { name: "budget-2026.csv", path: "Finance", size: "18 KB", days: 3, no: 41, before: "Studio rent,", found: "Total due", after: ",2 400", more: 0 },
    Line { name: "march-notes.md", path: "Notes", size: "4 KB", days: 40, no: 12, before: "- ", found: "Total due", after: " goes out on the 30th", more: 2 },
    Line { name: "reminder.txt", path: "Notes", size: "1 KB", days: 90, no: 7, before: "", found: "TOTAL DUE", after: ": 1,280.00", more: 0 },
];
const CHIPS: [(&str, &str); 8] =
    [("", "All"), ("folder", "Folders"), ("image", "Images"), ("video", "Videos"), ("music", "Audio"), ("file-text", "Documents"), ("code-xml", "Code"), ("file-archive", "Archives")];
const CHAPTERS: [&str; 4] = ["By name", "With filters", "Inside files", "As a tab"];

struct Scene {
    chapter: usize,
    query: String,
    ms: u64,
    view: Option<&'static str>,
    all_row: bool,
}

fn scenes() -> &'static [Scene] {
    static S: OnceLock<Vec<Scene>> = OnceLock::new();
    S.get_or_init(|| {
        let s = |chapter: usize, query: &str, ms: u64| Scene { chapter, query: query.into(), ms, view: None, all_row: false };
        let typing = |out: &mut Vec<Scene>, chapter: usize, from: &str, text: &str| {
            let chars: Vec<char> = text.chars().collect();
            for i in 0..chars.len() {
                out.push(s(chapter, &format!("{from}{}", chars[..=i].iter().collect::<String>()), 70));
            }
        };
        let mut v = vec![Scene { view: Some("keys"), ..s(0, "", 1200) }, s(0, "", 900)];
        typing(&mut v, 0, "", "invoice");
        v.push(s(0, "invoice", 2200));
        typing(&mut v, 1, "invoice", " ex");
        v.push(s(1, "invoice ex", 1500));
        v.push(s(1, "invoice ext:", 500));
        typing(&mut v, 1, "invoice ext:", "pdf");
        v.push(s(1, "invoice ext:pdf", 1500));
        typing(&mut v, 1, "invoice ext:pdf", " modified:<7d");
        v.push(s(1, "invoice ext:pdf modified:<7d", 2400));
        typing(&mut v, 2, "", "content:\"total due\"");
        v.push(s(2, "content:\"total due\"", 3400));
        v.push(Scene { all_row: true, ..s(3, "invoice", 2200) });
        v.push(Scene { view: Some("tab"), ..s(3, "invoice", 3600) });
        v
    })
}

fn first_of(chapter: usize) -> usize {
    scenes().iter().position(|s| s.chapter == chapter).unwrap_or(0)
}

fn last_of(chapter: usize) -> usize {
    scenes().iter().rposition(|s| s.chapter == chapter).unwrap_or(0)
}

/// What's typed, its filters picked out.
fn typed_query(q: &str) -> AnyElement {
    let re = regex::Regex::new(r#"(?:ext|modified):\S*|content:"[^"]*"?"#).unwrap();
    let hl: Vec<_> = re.find_iter(q).map(|m| (m.range(), HighlightStyle { color: Some(t::accent_text()), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() })).collect();
    StyledText::new(q.to_string()).with_highlights(hl).into_any_element()
}

fn caret() -> Div {
    div().w(px(1.5)).h(px(16.)).bg(t::accent())
}

fn search(frame: usize, cx: &mut Context<Ferret>) -> AnyElement {
    let scene = &scenes()[frame.min(scenes().len() - 1)];
    // The chapters: clicked, the demo goes there (the arrow keys move through the tour).
    let chapters = div().flex().gap(px(3.)).p(px(2.)).rounded(px(8.)).bg(t::hover()).self_start().children(CHAPTERS.iter().enumerate().map(|(i, c)| {
        let on = scene.chapter == i;
        div()
            .id(("ix-chapter", i))
            .px(px(10.))
            .h(px(24.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .text_size(px(12.))
            .text_color(if on { t::fg() } else { t::fg3() })
            .when(on, |d| d.bg(t::press()))
            .on_click(cx.listener(move |f, _, _, cx| f.intro_jump(if t::animate() { first_of(i) } else { last_of(i) }, cx)))
            .child(*c)
    }));
    let body: AnyElement = match scene.view {
        Some("keys") => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .child(div().px(px(18.)).py(px(10.)).rounded(px(10.)).border_1().border_color(t::line2()).bg(t::raised()).shadow(t::shadow()).text_size(px(22.)).child("Ctrl"))
            .child(div().text_size(px(20.)).text_color(t::fg3()).child("+"))
            .child(div().px(px(18.)).py(px(10.)).rounded(px(10.)).border_1().border_color(t::line2()).bg(t::raised()).shadow(t::shadow()).text_size(px(22.)).child("K"))
            .into_any_element(),
        Some(_) => search_tab(frame),
        None => fake_palette(scene, frame),
    };
    div().size_full().flex().flex_col().gap(px(8.)).child(chapters).child(div().flex_1().min_h_0().child(body)).into_any_element()
}

fn fake_palette(scene: &Scene, frame: usize) -> AnyElement {
    let q = scene.query.as_str();
    let in_files = q.starts_with("content:");
    let word = if in_files { "" } else { q.split(' ').next().unwrap_or("") };
    let hits: Vec<&Hit> = if in_files || word.len() < 2 {
        Vec::new()
    } else {
        HITS.iter().filter(|h| h.name.starts_with(word) && (!q.contains("ext:pdf") || h.name.ends_with(".pdf")) && (!q.contains("modified:<7d") || h.days < 7)).collect()
    };
    let lines: &[Line] = if q == "content:\"total due\"" { &LINES } else { &[] };
    let suggest = q.ends_with(" ex");
    let input = div()
        .h(px(42.))
        .flex_none()
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(9.))
        .border_b_1()
        .border_color(t::line())
        .child(icon("search", 17., t::fg3()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(1.))
                .text_size(px(14.))
                .child(if q.is_empty() { div().text_color(t::fg3()).child("Search files and folders everywhere…").into_any_element() } else { typed_query(q) })
                .child(caret()),
        )
        .child(icon("star", 14., t::fg3()))
        .child(icon("circle-question-mark", 14., t::fg3()))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .h(px(24.))
                .rounded(px(12.))
                .border_1()
                .border_color(if in_files { t::accent_line() } else { t::line2() })
                .text_size(px(11.5))
                .text_color(if in_files { t::accent_text() } else { t::fg2() })
                .child(icon(if in_files { "folder-search" } else { "hard-drive" }, 12., if in_files { t::accent_text() } else { t::fg2() }))
                .child(if in_files { "This folder" } else { "Everywhere" }),
        );
    let chips = div().flex_none().flex().gap(px(3.)).px(px(10.)).py(px(5.)).border_b_1().border_color(t::line()).overflow_hidden().children(CHIPS.iter().enumerate().map(|(i, (ic, label))| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(8.))
            .h(px(22.))
            .rounded(px(11.))
            .text_size(px(11.5))
            .when(i == 0, |d| d.bg(t::accent_soft()).text_color(t::accent_text()))
            .when(i > 0, |d| d.border_1().border_color(t::line2()).text_color(t::fg2()))
            .when(!ic.is_empty(), |d| d.child(icon(ic, 12., t::fg3())))
            .child(*label)
    }));
    let suggestion = |ic: &'static str, main: AnyElement, key: &'static str| {
        div().h(px(30.)).px(px(10.)).flex().items_center().gap(px(9.)).rounded(px(7.)).bg(t::accent_soft()).child(icon(ic, 14., t::accent_text())).child(div().flex_1().min_w_0().child(main)).child(kbd(key))
    };
    let mut list = div().flex_1().min_h_0().p(px(6.)).flex().flex_col().gap(px(1.)).overflow_hidden();
    if suggest {
        list = list.child(suggestion("hash", div().flex().gap(px(6.)).child(div().font_family(t::MONO).text_color(t::accent_text()).child("ext:")).child("An extension, like pdf").into_any_element(), "Tab"));
    }
    if scene.all_row {
        list = list.child(suggestion("folder-search", div().child(format!("Open all {} matches in a tab", hits.len())).into_any_element(), "Ctrl+Shift+Enter"));
    }
    if q.is_empty() {
        list = list.child(div().p(px(14.)).text_color(t::fg3()).text_size(px(12.)).child("Type to search your drives, or pick a type above. ? shows every filter · > for commands"));
    }
    for (i, h) in hits.iter().enumerate() {
        let on = i == 0 && !scene.all_row && !suggest;
        list = list.child(
            div()
                .h(px(30.))
                .flex_none()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(9.))
                .rounded(px(7.))
                .when(on, |d| d.bg(t::accent_soft()))
                .child(file_icon(h.name, false, 15.))
                .child(StyledText::new(h.name).with_highlights(vec![(0..word.len().min(h.name.len()), mark())]))
                .child(crate::ui::drive_badge(h.drive.chars().next().unwrap_or('C'), false))
                .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(t::fg3()).child(h.path))
                .child(div().text_size(px(11.5)).text_color(t::fg3()).child(h.size))
                .child(div().text_size(px(11.5)).text_color(t::fg3()).child(days_ago(h.days)))
                .child(icon("folder-open", 13., t::fg3())),
        );
    }
    for (i, l) in lines.iter().enumerate() {
        let text = format!("{}{}{}", l.before, l.found, l.after);
        let at = l.before.len();
        list = list.child(arrives(
            div()
                .px(px(10.))
                .py(px(4.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .rounded(px(7.))
                .when(i == 0, |d| d.bg(t::accent_soft()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .child(file_icon(l.name, false, 15.))
                        .child(l.name)
                        .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(t::fg3()).child(l.path))
                        .child(div().text_size(px(11.5)).text_color(t::fg3()).child(l.size))
                        .child(div().text_size(px(11.5)).text_color(t::fg3()).child(days_ago(l.days))),
                )
                .child(
                    div()
                        .pl(px(24.))
                        .flex()
                        .gap(px(8.))
                        .text_size(px(11.5))
                        .text_color(t::fg2())
                        .child(div().text_color(t::fg3()).child(format!("{}", l.no)))
                        .child(StyledText::new(text).with_highlights(vec![(at..at + l.found.len(), mark())]))
                        .when(l.more > 0, |d| d.child(div().px(px(5.)).rounded(px(4.)).bg(t::hover()).child(format!("+{}", l.more)))),
                ),
            "ix-line",
            frame * 10 + i,
        ));
    }
    win()
        .bg(t::raised())
        .shadow(t::shadow())
        .child(input)
        .child(chips)
        .child(list)
        .child(div().h(px(28.)).flex_none().px(px(12.)).flex().items_center().border_t_1().border_color(t::line()).text_size(px(11.)).text_color(t::fg3()).child("↑↓ choose · Enter open · Alt+Enter show in folder · ? filters"))
        .into_any_element()
}

fn search_tab(frame: usize) -> AnyElement {
    let tabs = div()
        .h(px(34.))
        .flex_none()
        .px(px(8.))
        .flex()
        .items_end()
        .gap(px(6.))
        .border_b_1()
        .border_color(t::line())
        .child(div().h(px(28.)).px(px(10.)).flex().items_center().gap(px(6.)).rounded_tl(px(8.)).rounded_tr(px(8.)).bg(t::accent_softer()).border_b_2().border_color(t::accent()).child(icon("folder-search", 13., t::fg2())).child("“invoice”"))
        .child(div().pb(px(7.)).child(icon("plus", 13., t::fg3())));
    let rows = div().flex_1().min_h_0().p(px(6.)).flex().flex_col().gap(px(1.)).children(HITS.iter().enumerate().map(|(i, h)| {
        row(h.name, false, Some(h.size), i == 0).child(div().w(px(160.)).flex_none().truncate().text_size(px(11.5)).text_color(t::fg3()).child(format!("{}:\\{}", h.drive, h.path)))
    }));
    pops(
        win()
            .bg(t::raised())
            .shadow(t::shadow())
            .child(tabs)
            .child(rows)
            .child(div().h(px(28.)).flex_none().px(px(12.)).flex().items_center().border_t_1().border_color(t::line()).text_size(px(11.)).text_color(t::fg3()).child("Lists the matches like a folder and stays current · Ctrl+S saves the search to the sidebar")),
        "ix-searchtab",
        frame,
    )
}

// ───────────────────────────── preview ─────────────────────────────

const SHOWN: [(&str, &str); 5] = [("ferret.png", "42 KB"), ("main.rs", "2 KB"), ("manual.pdf", "1.4 MB"), ("notes.md", "1 KB"), ("report.docx", "86 KB")];

fn picture() -> AnyElement {
    div().size_full().flex().flex_col().items_center().justify_center().gap(px(8.)).child(img(ferret_png()).size(px(110.)).object_fit(ObjectFit::Contain)).child(div().text_size(px(11.5)).text_color(t::fg3()).child("256 × 256")).into_any_element()
}

fn code() -> AnyElement {
    let (kw, st) = if t::is_dark() { (t::rgba(0xff7b72, 1.), t::rgba(0xa5d6ff, 1.)) } else { (t::rgba(0xcf222e, 1.), t::rgba(0x0a3069, 1.)) };
    let lines = ["fn main() {", "    let name = \"ferret\";", "    println!(\"hello, {name}\");", "}"];
    div()
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .font_family(t::MONO)
        .text_size(px(12.))
        .children(lines.iter().map(move |l| {
            let mut hl = Vec::new();
            for word in ["fn", "let"] {
                if let Some(at) = l.find(&format!("{word} ")) {
                    hl.push((at..at + word.len(), HighlightStyle { color: Some(kw), ..Default::default() }));
                }
            }
            if let (Some(a), Some(b)) = (l.find('"'), l.rfind('"')) {
                if b > a {
                    hl.push((a..b + 1, HighlightStyle { color: Some(st), ..Default::default() }));
                }
            }
            hl.sort_by_key(|(r, _)| r.start);
            StyledText::new(l.to_string()).with_highlights(hl)
        }))
        .into_any_element()
}

fn page() -> Div {
    div().bg(t::rgba(0xffffff, 1.)).text_color(t::rgba(0x1f1f1f, 1.)).rounded(px(4.)).p(px(12.)).flex().flex_col().gap(px(5.)).text_size(px(11.)).shadow(t::shadow())
}

fn pdf() -> AnyElement {
    let num = |n: &str| div().self_end().text_size(px(10.)).text_color(t::rgba(0x888888, 1.)).child(n.to_string());
    div()
        .p(px(10.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(page().child(div().font_weight(FontWeight::BOLD).text_size(px(13.)).child("Field guide")).child("Ferrets sleep for up to eighteen hours a day, and spend the rest of it looking for things to hide.").child("Keep their water fresh, and check behind the sofa before moving it.").child(num("1")))
        .child(page().child(div().font_weight(FontWeight::SEMIBOLD).child("Chapter 2")).child("A ferret will fit through any gap that its head fits through.").child(num("2")))
        .into_any_element()
}

fn notes() -> AnyElement {
    div()
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().font_weight(FontWeight::BOLD).text_size(px(15.)).child("Trip notes"))
        .child("Things to sort before Friday:")
        .child(div().pl(px(10.)).child("•  Book the train"))
        .child(div().pl(px(10.)).child("•  Scan the passports"))
        .into_any_element()
}

fn word() -> AnyElement {
    let cell = |s: &str, head: bool| div().flex_1().px(px(5.)).py(px(2.)).border_1().border_color(t::rgba(0xbfbfbf, 1.)).when(head, |d| d.font_weight(FontWeight::SEMIBOLD).bg(t::rgba(0xeef2f7, 1.))).child(s.to_string());
    let trow = |a: &str, b: &str, c: &str, head: bool| div().flex().child(cell(a, head)).child(cell(b, head)).child(cell(c, head));
    div()
        .p(px(10.))
        .child(
            page()
                .child(div().font_weight(FontWeight::BOLD).text_size(px(13.)).text_color(t::rgba(0x1f3864, 1.)).child("Quarterly report"))
                .child("Sales grew in all three regions, led by the north. The new warehouse opened on time.")
                .child(div().flex().flex_col().child(trow("Region", "Q2", "Q3", true)).child(trow("North", "1,240", "1,510", false)).child(trow("Central", "980", "1,040", false)).child(trow("South", "760", "815", false)))
                .child("Next quarter the focus moves to delivery times and to the two stores opening in spring."),
        )
        .into_any_element()
}

fn preview(frame: usize) -> AnyElement {
    let f = frame.min(SHOWN.len() - 1);
    let list = div()
        .w(px(210.))
        .flex_none()
        .h_full()
        .p(px(6.))
        .border_r_1()
        .border_color(t::line())
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(row("assets", true, None, false))
        .child(row("src", true, None, false))
        .children(SHOWN.iter().enumerate().map(|(i, (n, s))| row(n, false, Some(s), i == f)))
        .child(row("todo.txt", false, Some("1 KB"), false));
    let body = match f {
        0 => picture(),
        1 => code(),
        2 => pdf(),
        3 => notes(),
        _ => word(),
    };
    win()
        .flex_row()
        .child(list)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .child(div().h(px(32.)).flex_none().px(px(12.)).flex().items_center().gap(px(8.)).border_b_1().border_color(t::line()).child(file_icon(SHOWN[f].0, false, 14.)).child(div().font_weight(FontWeight::SEMIBOLD).child(SHOWN[f].0)))
                .child(div().flex_1().min_h_0().overflow_hidden().child(arrives(div().size_full().child(body), "ix-preview", f))),
        )
        .into_any_element()
}

// ───────────────────────────── archives ─────────────────────────────

fn zip(frame: usize) -> AnyElement {
    // 0 right-clicked in its folder · 1 opened · 2 a file picked · 3 Extract to… · 4 extracting · 5 done
    let inside = (1..=4).contains(&frame);
    let picked = frame >= 2;
    let path: &[&str] = if frame == 5 { &["projects"] } else if inside { &["Downloads", "brand.zip"] } else { &["Downloads"] };
    let mut w = win().child(crumbs(path, false));
    if inside {
        w = w.child(
            div()
                .h(px(34.))
                .flex_none()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(8.))
                .bg(t::accent_softer())
                .border_b_1()
                .border_color(t::line())
                .child(icon("file-archive", 14., t::accent_text()))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(12.)).child("ZIP archive"))
                .child(div().flex_1().text_size(px(11.5)).text_color(t::fg3()).child(if picked { "" } else { "Read-only: extract it to make changes" }))
                .when(picked, |d| d.child(btn("Extract 1 selected item…", false)))
                .child(btn("Extract to…", false).when(frame == 3, |d| d.border_color(t::accent()).bg(t::accent_soft())))
                .child(btn("Extract all", true)),
        );
    }
    let body: AnyElement = if inside {
        let files = list(&[("fonts", true, None), ("ferret.png", false, Some("42 KB")), ("guidelines.docx", false, Some("310 KB")), ("logo-dark.svg", false, Some("6 KB"))], if picked { Some(1) } else { None });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(div().w(relative(0.5)).h_full().border_r_1().border_color(t::line()).child(files))
            .child(div().flex_1().h_full().flex().flex_col().child(if picked {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(div().h(px(30.)).flex_none().px(px(10.)).flex().items_center().border_b_1().border_color(t::line()).font_weight(FontWeight::SEMIBOLD).child("ferret.png"))
                    .child(div().flex_1().min_h_0().child(picture()))
                    .into_any_element()
            } else {
                div().size_full().flex().items_center().justify_center().p(px(16.)).text_center().text_color(t::fg3()).text_size(px(12.)).child("Nothing unpacked: it's all still in the zip").into_any_element()
            }))
            .into_any_element()
    } else if frame == 5 {
        div().p(px(6.)).child(arrives(row("brand", true, None, true), "ix-zip-brand", frame)).child(list(&[("ferret", true, None), ("website", true, None), ("notes.md", false, Some("1 KB"))], None).p_0()).into_any_element()
    } else {
        list(&[("drivers", true, None), ("brand.zip", false, Some("1.2 MB")), ("setup.exe", false, Some("88 MB")), ("statement.pdf", false, Some("212 KB")), ("todo.txt", false, Some("1 KB"))], Some(1)).into_any_element()
    };
    w = w.child(body);
    if frame == 0 {
        // Its menu, by the right-clicked brand.zip.
        w = w.child(pops(
            div()
                .absolute()
                .top(px(96.))
                .left(px(110.))
                .w(px(190.))
                .p(px(4.))
                .rounded(px(9.))
                .bg(t::raised())
                .border_1()
                .border_color(t::line2())
                .shadow(t::shadow())
                .flex()
                .flex_col()
                .children(["Browse archive", "Extract here", "Extract to “brand\\”", "Extract to…"].iter().enumerate().map(|(i, l)| div().h(px(26.)).px(px(9.)).flex().items_center().rounded(px(6.)).text_size(px(12.)).when(i == 0, |d| d.bg(t::accent()).text_color(t::on_accent())).child(*l))),
            "ix-zip-menu",
            frame,
        ));
    }
    if frame == 3 {
        w = w.child(pops(
            div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().bg(t::scrim()).child(
                div()
                    .w(px(260.))
                    .p(px(12.))
                    .rounded(px(10.))
                    .bg(t::raised())
                    .border_1()
                    .border_color(t::line2())
                    .shadow(t::shadow())
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).mb(px(4.)).child("Extract brand.zip to…"))
                    .child(row("Documents", true, None, false))
                    .child(row("Downloads", true, None, false))
                    .child(row("projects", true, None, true))
                    .child(div().mt(px(6.)).flex().justify_end().gap(px(6.)).child(btn("Cancel", false)).child(btn("Select folder", true))),
            ),
            "ix-zip-form",
            frame,
        ));
    }
    if frame == 4 {
        w = w.child(corner(job("package-open", "Extracting brand.zip", 64), "ix-zip-job", frame));
    }
    if frame == 5 {
        w = w.child(corner(toast("Extracted to projects"), "ix-zip-toast", frame));
    }
    w.into_any_element()
}

// ───────────────────────────── tabs ─────────────────────────────

fn tabs(frame: usize) -> AnyElement {
    let tab = |name: &str, on: bool| {
        div()
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded_tl(px(8.))
            .rounded_tr(px(8.))
            .when(on, |d| d.bg(t::accent_softer()).border_b_2().border_color(t::accent()))
            .text_color(if on { t::fg() } else { t::fg2() })
            .child(icon("folder", 13., t::folder()))
            .child(name.to_string())
    };
    let head = |name: &str, on: bool| div().h(px(26.)).px(px(10.)).flex().items_center().text_size(px(11.5)).font_weight(FontWeight::SEMIBOLD).text_color(if on { t::accent_text() } else { t::fg3() }).child(name.to_string());
    let left = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(4.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(head("projects", true))
        .child(row("ferret", true, None, false))
        .child(row("website", true, None, false))
        .child(row("notes.md", false, None, false))
        .child(row("logo.png", false, None, frame >= 2))
        .child(row("build.log", false, None, false))
        .child(row("todo.txt", false, None, false));
    let right = div()
        .flex_1()
        .min_w_0()
        .h_full()
        .p(px(4.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .border_l_1()
        .border_color(t::line())
        .child(head("backup", false))
        .child(row("old-site", true, None, false))
        .child(row("2025.zip", false, None, false))
        .when(frame == 3, |d| d.child(arrives(row("logo.png", false, None, false).opacity(0.75), "ix-tabs-copy", frame)));
    let split = frame > 0;
    let panes = div().flex_1().min_h_0().flex().child(left).when(split, |d| {
        d.child(if t::animate() && frame == 1 {
            div().flex_1().h_full().child(right).with_animation("ix-split", Animation::new(Duration::from_millis(500)).with_easing(gpui::ease_out_quint()), |d, k| d.opacity(k)).into_any_element()
        } else {
            div().flex_1().h_full().child(right).into_any_element()
        })
    });
    win()
        .child(div().h(px(36.)).flex_none().px(px(8.)).flex().items_end().gap(px(4.)).border_b_1().border_color(t::line()).child(tab("projects", true)).child(tab("Downloads", false)).child(div().pb(px(7.)).child(icon("plus", 13., t::fg3()))))
        .child(panes)
        .into_any_element()
}

// ───────────────────────────── jobs ─────────────────────────────

const PROGRESS: [u32; 10] = [0, 8, 19, 31, 44, 52, 67, 78, 91, 100];
const JOB_FILES: [&str; 7] = ["brand", "exports", "invoices", "photos", "raw", "site", "videos"];

fn jobs(frame: usize) -> AnyElement {
    let done = frame >= PROGRESS.len();
    let pct = PROGRESS[frame.min(PROGRESS.len() - 1)];
    let cursor = (frame / 2).min(JOB_FILES.len() - 1);
    win()
        .child(div().flex_1().p(px(6.)).flex().flex_col().gap(px(1.)).children(JOB_FILES.iter().enumerate().map(|(i, f)| row(f, true, None, i == cursor))))
        .child(if done { corner(toast("Copied 1,204 items to backup"), "ix-job-toast", frame) } else { div().absolute().right(px(12.)).bottom(px(12.)).child(job("copy", "Copying 1,204 items to backup", pct)).into_any_element() })
        .into_any_element()
}

// ───────────────────────────── dropzone ─────────────────────────────

const PARKED: [(&str, &str); 2] = [("logo.png", "42 KB"), ("notes.md", "1 KB")];
/// The pointer for each frame: on the two files, dragged to the Dropzone, dropped, another
/// folder, "Drag all", over the list, dropped.
const POINTER: [(f32, f32); 8] = [(66., 100.), (15., 112.), (15., 112.), (14., 20.), (14., 20.), (29., 86.), (60., 150.), (60., 150.)];

fn dropzone(frame: usize) -> AnyElement {
    let parked = frame >= 2;
    let in_projects = frame >= 4;
    let shelf = div()
        .mt(px(8.))
        .p(px(6.))
        .rounded(px(8.))
        .border_1()
        .when(!parked, |d| d.border_dashed())
        .border_color(if frame == 1 { t::accent() } else { t::line2() })
        .when(frame == 1, |d| d.bg(t::accent_soft()))
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child(icon("inbox", 13., t::fg2()))
                .child("Dropzone")
                .when(parked, |d| d.child(div().px(px(5.)).rounded(px(4.)).bg(t::accent_soft()).text_color(t::accent_text()).child("2")).child(div().flex_1()).child(div().text_color(t::accent_text()).font_weight(FontWeight::NORMAL).child("Drag all"))),
        )
        .child(if parked {
            div().flex().flex_col().gap(px(2.)).children(PARKED.iter().map(|(n, _)| arrives(div().flex().items_center().gap(px(6.)).text_size(px(11.5)).child(file_icon(n, false, 13.)).child(n.to_string()), "ix-shelf", frame))).into_any_element()
        } else {
            div().text_size(px(11.)).text_color(t::fg3()).child("Drop files here and browse freely").into_any_element()
        });
    let side = div().child(side_item("folder", "projects", in_projects, None)).child(side_item("folder", "Downloads", !in_projects, None)).child(shelf);
    let pane = div()
        .size_full()
        .flex()
        .flex_col()
        .child(crumbs(if in_projects { &["projects"] } else { &["Downloads"] }, false))
        .child(if in_projects {
            div()
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(row("ferret", true, None, false))
                .child(row("website", true, None, false))
                .when(frame == POINTER.len() - 1, |d| d.children(PARKED.iter().map(|(n, s)| arrives(row(n, false, Some(s), false).opacity(0.8), "ix-drop-land", frame))))
                .child(row("README.md", false, Some("3 KB"), false))
                .into_any_element()
        } else {
            div()
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(row("drivers", true, None, false))
                .children(PARKED.iter().map(|(n, s)| row(n, false, Some(s), frame < 2)))
                .child(row("statement.pdf", false, Some("212 KB"), false))
                .child(row("todo.txt", false, Some("1 KB"), false))
                .into_any_element()
        });
    let (x, y) = POINTER[frame.min(POINTER.len() - 1)];
    let ghost = match frame {
        1 => Some("2  Add 2 items to the Dropzone".to_string()),
        6 => Some("2  Copy 2 items to projects".to_string()),
        _ => None,
    };
    split(side, pane).child(pointer(x, y, false, ghost)).into_any_element()
}

// ───────────────────────────── WSL ─────────────────────────────

fn wsl(frame: usize) -> AnyElement {
    let opened = frame >= 1;
    let in_code = frame >= 2;
    let side = div()
        .child(side_title("Linux"))
        .child(side_item("terminal", "Ubuntu-26.04", opened, Some("default")))
        .child(side_item("terminal", "Debian", false, None))
        .child(side_title("Drives"))
        .child(side_item("hard-drive", "Local Disk (C:)", !opened, None));
    let path: &[&str] = if !opened { &["Local Disk (C:)", "Users", "you"] } else if in_code { &["Ubuntu-26.04", "home", "you", "code"] } else { &["Ubuntu-26.04", "home", "you"] };
    let items: AnyElement = if in_code {
        arrives(list(&[("ferret", true, None), ("website", true, None), ("Makefile", false, Some("1 KB")), ("notes.md", false, Some("2 KB"))], None), "ix-wsl-code", frame)
    } else if opened {
        arrives(list(&[("code", true, None), (".config", true, None), ("venv", true, None), (".bashrc", false, Some("4 KB")), ("deploy.sh", false, Some("2 KB"))], Some(0)), "ix-wsl-home", frame)
    } else {
        list(&[("Documents", true, None), ("Downloads", true, None), ("Pictures", true, None)], None).into_any_element()
    };
    let mut w = split(side, div().size_full().flex().flex_col().child(crumbs(path, opened)).child(items));
    if frame == 0 {
        w = w.child(pointer(13., 46., false, None));
    }
    if frame == 3 {
        let (user, dir) = (t::rgba(0x7ee787, 1.), t::rgba(0x79c0ff, 1.));
        let prompt = |cmd: &str| {
            let text = format!("you@pc:~/code$ {cmd}");
            StyledText::new(text).with_highlights(vec![(0..6, HighlightStyle { color: Some(user), font_weight: Some(FontWeight::BOLD), ..Default::default() }), (7..13, HighlightStyle { color: Some(dir), ..Default::default() })])
        };
        w = w.child(pops(
            div()
                .absolute()
                .right(px(14.))
                .bottom(px(14.))
                .w(px(300.))
                .rounded(px(9.))
                .overflow_hidden()
                .bg(t::rgba(0x0c0c0c, 0.96))
                .border_1()
                .border_color(t::line2())
                .shadow(t::shadow())
                .text_color(t::rgba(0xcccccc, 1.))
                .font_family(t::MONO)
                .text_size(px(11.5))
                .child(div().h(px(26.)).px(px(9.)).flex().items_center().gap(px(6.)).bg(t::rgba(0x1f1f1f, 1.)).child(icon("terminal", 12., t::rgba(0xcccccc, 1.))).child("Ubuntu-26.04"))
                .child(div().p(px(9.)).flex().flex_col().gap(px(2.)).child(prompt("ls")).child("ferret  Makefile  notes.md  website").child(div().flex().items_center().child(prompt("")).child(caret().h(px(13.))))),
            "ix-term",
            frame,
        ));
    }
    w.into_any_element()
}

// ───────────────────────────── network ─────────────────────────────

const NET_SHARE: usize = 4;

fn network(frame: usize) -> AnyElement {
    // 0 found in the sidebar · 1 it wants an account · 2 connecting · 3 its shares · 4 one of them
    // · 5 not in the list: + and its address · 6 added
    let signed_in = (3..5).contains(&frame);
    let added = frame == 6;
    let side = div()
        .child(
            div()
                .flex()
                .items_center()
                .child(side_title("Network"))
                .child(div().flex_1())
                .child(icon("rotate-cw", 11., t::fg3()))
                .child(div().ml(px(6.)).mr(px(4.)).p(px(1.)).rounded(px(4.)).when(frame == 5, |d| d.bg(t::accent_soft())).child(icon("plus", 12., if frame == 5 { t::accent_text() } else { t::fg3() }))),
        )
        .when(added, |d| d.child(arrives(side_item("network", "backup", true, None), "ix-net-added", frame)))
        .child(side_item("monitor", "NAS", signed_in, None))
        .child(side_item("monitor", "OFFICE-PC", false, None))
        .child(side_title("Drives"))
        .child(side_item("hard-drive", "Local Disk (C:)", !signed_in && !added, None));
    let path: &[&str] = if added { &["\\\\192.168.0.10\\backup"] } else if frame == NET_SHARE { &["\\\\NAS\\media"] } else if frame == 3 { &["\\\\NAS"] } else { &["Local Disk (C:)", "Users", "you"] };
    let items: AnyElement = match frame {
        3 => arrives(list(&[("backup", true, None), ("media", true, None), ("photos", true, None)], None), "ix-net-shares", frame),
        NET_SHARE => arrives(list(&[("Movies", true, None), ("Music", true, None), ("holiday.mp4", false, Some("1.8 GB")), ("playlist.m3u", false, Some("2 KB"))], Some(0)), "ix-net-share", frame),
        6 => arrives(list(&[("2025", true, None), ("2026", true, None), ("laptop.zip", false, Some("48 GB")), ("phone.zip", false, Some("12 GB"))], None), "ix-net-backup", frame),
        _ => list(&[("Documents", true, None), ("Downloads", true, None), ("Pictures", true, None)], None).opacity(0.5).into_any_element(),
    };
    let mut w = split(side, div().size_full().flex().flex_col().child(crumbs(path, false)).child(items));
    if frame == 1 || frame == 2 {
        let check = div().flex().items_center().gap(px(7.)).text_size(px(11.5)).child(div().size(px(13.)).rounded(px(3.)).bg(t::accent()).flex().items_center().justify_center().child(icon("check", 10., t::on_accent()))).child("Remember (in Windows Credential Manager)").into_any_element();
        w = w.child(pops(form("Sign in to NAS", &[("User name", "you"), ("Password", "••••••••••")], Some(check), frame == 2, "Connect"), "ix-net-login", 1));
    }
    if frame == 5 {
        w = w.child(pops(form("Add a network folder", &[("Address", "\\\\192.168.0.10\\backup"), ("Name", "backup")], None, false, "Add and connect"), "ix-net-add", frame));
    }
    match frame {
        0 => w = w.child(pointer(13., 46., false, None)),
        1 => w = w.child(pointer(72., 66., true, None)),
        3 => w = w.child(pointer(44., 74., false, None)),
        _ => {}
    }
    w.into_any_element()
}

// ───────────────────────────── servers ─────────────────────────────

fn servers(frame: usize) -> AnyElement {
    let added = frame == 3;
    let side = div()
        .child(div().flex().items_center().child(side_title("Servers")).child(div().flex_1()).child(div().mr(px(4.)).p(px(1.)).rounded(px(4.)).when(frame == 0, |d| d.bg(t::accent_soft())).child(icon("plus", 12., if frame == 0 { t::accent_text() } else { t::fg3() }))))
        .child(if added {
            arrives(side_item("server", "my-server", true, None), "ix-srv", frame)
        } else {
            div().px(px(8.)).py(px(4.)).text_size(px(11.)).text_color(t::fg3()).child("Add a server to browse its files over SSH").into_any_element()
        })
        .child(side_title("Drives"))
        .child(side_item("hard-drive", "Local Disk (C:)", false, None));
    let path: &[&str] = if added { &["my-server", "home", "you"] } else { &["Local Disk (C:)", "Users", "you"] };
    let items: AnyElement = if added {
        arrives(
            list(&[("projects", true, None), (".config", true, None), ("backup.tar.gz", false, Some("1.2 GB")), ("deploy.sh", false, Some("2 KB")), ("nginx.conf", false, Some("3 KB")), ("access.log", false, Some("48 MB"))], None),
            "ix-srv-files",
            frame,
        )
    } else {
        list(&[("Documents", true, None), ("Downloads", true, None), ("projects", true, None)], None).opacity(0.5).into_any_element()
    };
    let mut w = split(side, div().size_full().flex().flex_col().child(crumbs(path, added)).child(items));
    if frame == 1 || frame == 2 {
        w = w.child(pops(
            form(
                "Add a server",
                &[("From ~/.ssh/config", "my-server"), ("Name", "my-server"), ("Server", "192.168.0.10"), ("User", "you"), ("Sign in with", "Keys, then password")],
                None,
                frame == 2,
                "Add and connect",
            ),
            "ix-srv-form",
            1,
        ));
    }
    match frame {
        0 => w = w.child(pointer(29., 18., false, None)),
        1 => w = w.child(pointer(72., 104., true, None)),
        _ => {}
    }
    w.into_any_element()
}

// ───────────────────────────── commands ─────────────────────────────

const COMMANDS: [(&str, &str); 7] = [
    ("Split view (two folders side by side)", "Ctrl+\\"),
    ("Show hidden items", "Ctrl+H"),
    ("Reopen closed tab", "Ctrl+Shift+T"),
    ("Toggle preview pane", "Alt+P"),
    ("Undo", "Ctrl+Z"),
    ("Open in Terminal", "Ctrl+`"),
    ("New window", "Ctrl+N"),
];

/// What gets typed, a frame each: "split", "hidden", "reopen" letter by letter, each held.
fn typed() -> &'static [String] {
    static T: OnceLock<Vec<String>> = OnceLock::new();
    T.get_or_init(|| {
        let mut out = Vec::new();
        for q in ["split", "hidden", "reopen"] {
            for n in 0..=q.len() {
                out.push(q[..n].to_string());
            }
            out.push(q.to_string());
        }
        out
    })
}

fn commands(frame: usize) -> AnyElement {
    let q = typed()[frame.min(typed().len() - 1)].as_str();
    let shown: Vec<_> = COMMANDS.iter().filter(|(l, _)| l.to_lowercase().contains(q)).collect();
    win()
        .bg(t::raised())
        .shadow(t::shadow())
        .child(
            div()
                .h(px(42.))
                .flex_none()
                .px(px(12.))
                .flex()
                .items_center()
                .gap(px(9.))
                .border_b_1()
                .border_color(t::line())
                .text_size(px(14.))
                .child(icon("command", 17., t::fg3()))
                .child(if q.is_empty() { div().text_color(t::fg3()).child("Type a command…").into_any_element() } else { div().child(q.to_string()).into_any_element() })
                .child(caret()),
        )
        .child(div().flex_1().min_h_0().p(px(6.)).flex().flex_col().gap(px(1.)).children(shown.iter().enumerate().map(|(i, (label, keys))| {
            let at = label.to_lowercase().find(q).unwrap_or(0);
            div()
                .h(px(30.))
                .flex_none()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(9.))
                .rounded(px(7.))
                .when(i == 0, |d| d.bg(t::accent_soft()))
                .child(icon("command", 13., t::fg3()))
                .child(div().flex_1().child(StyledText::new(label.to_string()).with_highlights(if q.is_empty() { vec![] } else { vec![(at..at + q.len(), mark())] })))
                .child(kbd(keys))
        })))
        .child(div().h(px(28.)).flex_none().px(px(12.)).flex().items_center().border_t_1().border_color(t::line()).text_size(px(11.)).text_color(t::fg3()).child("↑↓ choose · Enter run · Esc close"))
        .into_any_element()
}

// ───────────────────────────── setup ─────────────────────────────

/// Still: it follows the color picked in Settings (the accent and theme are live).
fn setup() -> AnyElement {
    let side = div().child(side_item("folder", "projects", true, None)).child(side_item("hard-drive", "Local Disk (C:)", false, None));
    let pane = div()
        .p(px(6.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .child(row("ferret", true, None, false))
        .child(row("notes.md", false, Some("1 KB"), true))
        .child(row("logo.png", false, Some("42 KB"), false))
        .child(div().mt(px(10.)).mx(px(8.)).h(px(5.)).rounded(px(3.)).bg(t::line2()).child(div().h_full().w(relative(0.62)).rounded(px(3.)).bg(t::accent())))
        .child(
            div().mt(px(14.)).mx(px(8.)).flex().gap(px(8.)).children(crate::settings::ACCENTS.iter().map(|(_, c)| {
                let on = *c == t::accent_hex();
                div().size(px(20.)).rounded(px(10.)).bg(t::rgba(*c, 1.)).border_2().border_color(if on { t::fg() } else { Hsla::transparent_black() })
            })),
        );
    split(side, pane).into_any_element()
}
