//! The compress editors' shared look (MediaEditor.tsx's `Shell` and its parts, src/media.css's
//! `.mx-*` rules): a big card with a header, a dark stage on the left, the settings in a column
//! of ruled sections on the right, and the size it will come out at in the footer.

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, Entity, FontWeight, Hsla, SharedString, Stateful, Window, div, point,
    prelude::*, px, relative,
};

use crate::input::TextInput;
use crate::kinds;
use crate::theme as t;
use crate::ui::{self, icon};

/// A click handler (from `cx.listener`, boxed so the parts don't care whose it is).
pub type On = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// `--panel-solid`: the modal's own color.
pub fn panel_solid() -> Hsla {
    if t::is_dark() { t::rgba(0x1c131c, 1.) } else { t::rgba(0xfbf7fa, 1.) }
}

/// The stage behind the picture: darker than the panel, and dark in the light theme too
/// (pictures and video read best on it).
pub fn stage_bg() -> Hsla {
    if t::is_dark() { t::mix(0x1c131c, 0x000000, 0.55) } else { t::rgba(0x2a2228, 1.) }
}

/// Text on the stage (it stays dark in the light theme).
pub fn stage_fg() -> Hsla {
    if t::is_dark() { t::fg() } else { t::rgba(0xf4eef3, 1.) }
}
pub fn stage_fg2() -> Hsla {
    if t::is_dark() { t::fg2() } else { t::rgba(0xf4eef3, 0.8) }
}
pub fn stage_fg3() -> Hsla {
    if t::is_dark() { t::fg3() } else { t::rgba(0xf4eef3, 0.55) }
}

/// `.modal.mx`: min(1180, viewport − 40) × min(820, viewport − 40).
pub fn card_size(window: &Window) -> (f32, f32) {
    let vw = window.viewport_size();
    ((vw.width.as_f32() - 40.).min(1180.), (vw.height.as_f32() - 40.).min(820.))
}

pub fn card(w: f32, h: f32) -> Div {
    div()
        .w(px(w))
        .h(px(h))
        .flex()
        .flex_col()
        .bg(panel_solid())
        .border_1()
        .border_color(t::line2())
        .rounded(px(14.))
        .shadow(t::shadow())
        .overflow_hidden()
}

/// `.modal-head.mx-head`: the icon in an accent tile, the title and what it is, and ×.
pub fn head(icon_name: &'static str, title: impl Into<SharedString>, sub: impl Into<SharedString>, close: On) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .pl(px(18.))
        .pr(px(16.))
        .pt(px(14.))
        .pb(px(10.))
        .border_b_1()
        .border_color(t::line())
        .child(div().flex_none().size(px(34.)).rounded(px(10.)).flex().items_center().justify_center().bg(t::accent_soft()).child(icon(icon_name, 18., t::accent_text())))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(title.into()))
                .child(div().text_size(px(12.)).text_color(t::fg3()).truncate().child(sub.into())),
        )
        .child(ui::icon_btn("mx-close", "x", true).aria_label("Close").tooltip(ui::tip("Close (Esc)")).on_click(close))
}

/// `.mx-main`: the stage and the settings column side by side.
pub fn main(stage: impl IntoElement, side: impl IntoElement) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(div().flex_1().min_w_0().h_full().flex().flex_col().bg(stage_bg()).text_color(stage_fg()).child(stage))
        .child(side)
}

/// `.mx-side`: 316 px, scrolls.
pub fn side(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(316.))
        .flex_none()
        .h_full()
        .overflow_y_scroll()
        .px(px(16.))
        .pt(px(6.))
        .pb(px(18.))
        .border_l_1()
        .border_color(t::line())
        .flex()
        .flex_col()
}

/// `.mx-sec`: a ruled section with its title (and something on the title's right).
pub fn section(title: &str, aside: Option<AnyElement>, last: bool) -> Div {
    div()
        .flex_none()
        .py(px(12.))
        .when(!last, |d| d.border_b_1().border_color(t::line()))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.))
                .min_h(px(26.))
                .mb(px(8.))
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t::fg3()).child(title.to_uppercase()))
                .children(aside),
        )
}

/// `.mx-hint`.
pub fn hint(text: impl Into<SharedString>) -> Div {
    div().mt(px(6.)).text_size(px(11.5)).line_height(relative(1.4)).text_color(t::fg3()).child(text.into())
}

/// `.mx-hint.warn`: with a warning sign.
pub fn warn(text: impl Into<SharedString>) -> Div {
    div()
        .mt(px(6.))
        .flex()
        .items_start()
        .gap(px(6.))
        .text_size(px(11.5))
        .line_height(relative(1.4))
        .text_color(t::warn())
        .child(div().pt(px(1.)).child(icon("triangle-alert", 13., t::warn())))
        .child(div().flex_1().child(text.into()))
}

/// `.mx-presets`: two columns of cards, the one in use ringed in the accent.
pub fn presets(items: Vec<(SharedString, SharedString, bool, On)>) -> Div {
    div().grid().grid_cols(2).gap(px(6.)).children(items.into_iter().enumerate().map(|(i, (label, sub, on, click))| {
        div()
            .id(("mx-preset", i))
            .role(gpui::Role::RadioButton)
            .aria_label(format!("{label}, {sub}"))
            .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
            .flex()
            .flex_col()
            .items_start()
            .gap(px(1.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(9.))
            .border_1()
            .when(on, |d| {
                d.border_color(t::accent()).bg(t::accent_softer()).shadow(vec![BoxShadow {
                    color: t::accent(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(1.),
                    inset: true,
                }])
            })
            .when(!on, |d| d.border_color(t::line2()).bg(t::hover()).hover(|s| s.border_color(t::accent_line())))
            .on_click(click)
            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(t::fg()).child(label))
            .child(div().text_size(px(11.)).text_color(if on { t::fg2() } else { t::fg3() }).child(sub))
    }))
}

/// `.mx-seg`: a segmented control across the column (`small`: the compact one beside a title).
pub fn seg(id: &'static str, options: Vec<(SharedString, bool, On)>, small: bool) -> Div {
    ui::seg().when(!small, |d| d.flex_1().w_full()).children(options.into_iter().enumerate().map(|(i, (label, on, click))| {
        ui::seg_btn((id, i), None, Some(label), on)
            .when(!small, |d| d.flex_1().h(px(28.)))
            .when(small, |d| d.flex_none().h(px(24.)).text_size(px(12.)))
            .whitespace_nowrap()
            .on_click(click)
    }))
}

/// `.mx-select`: looks like a drop-down; `menu` is its list, open below it.
pub fn select(id: impl Into<ElementId>, label: impl Into<SharedString>, open: bool, menu: Option<AnyElement>, click: On) -> Div {
    let label = label.into();
    div()
        .relative()
        .w_full()
        .child(
            div()
                .id(id)
                .role(gpui::Role::ComboBox)
                .aria_label(label.clone())
                .aria_expanded(open)
                .h(px(30.))
                .w_full()
                .px(px(8.))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(6.))
                .rounded(px(7.))
                .bg(t::raised())
                .border_1()
                .border_color(if open { t::accent() } else { t::line2() })
                .text_size(px(13.))
                .text_color(t::fg())
                .on_click(click)
                .child(div().flex_1().min_w_0().truncate().child(label))
                .child(icon("chevron-down", 14., t::fg2())),
        )
        .children(menu)
}

/// The list of a `select`, drawn over everything after it.
pub fn select_menu(id: &'static str, options: Vec<(SharedString, bool, On)>) -> AnyElement {
    gpui::deferred(
        div()
            .id(id)
            .role(gpui::Role::ListBox)
            .absolute()
            .top(px(33.))
            .left_0()
            .w_full()
            .p(px(4.))
            .rounded(px(9.))
            .bg(t::raised())
            .border_1()
            .border_color(t::line2())
            .shadow(t::shadow())
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex()
            .flex_col()
            .children(options.into_iter().enumerate().map(|(i, (label, on, click))| {
                div()
                    .id((id, i))
                    .role(gpui::Role::ListBoxOption)
                    .aria_label(label.clone())
                    .aria_selected(on)
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .text_color(t::fg())
                    .hover(|s| s.bg(t::accent()).text_color(t::on_accent()))
                    .on_click(click)
                    .child(div().w(px(14.)).flex_none().when(on, |d| d.child(icon("check", 13., t::fg2()))))
                    .child(div().flex_1().truncate().child(label))
            })),
    )
    .with_priority(3)
    .into_any_element()
}

/// `.mx-slider`: a range with the accent fill and a round thumb; `bounds` learns where it is
/// (the owner turns a drag into a value).
pub fn slider(id: &'static str, value: f32, bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>) -> Stateful<Div> {
    let v = value.clamp(0., 1.);
    div()
        .id(id)
        .role(gpui::Role::Slider)
        .aria_numeric_value((v * 100.).round() as f64)
        .flex_1()
        .relative()
        .h(px(22.))
        .flex()
        .items_center()
        .child(gpui::canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
        .child(div().w_full().h(px(5.)).rounded(px(3.)).bg(if t::is_dark() { t::rgba(0xefefef, 0.92) } else { t::rgba(0xdcd6da, 1.) }).child(div().h_full().w(relative(v)).rounded(px(3.)).bg(t::accent())))
        .child(div().absolute().left(relative(v)).ml(px(-8.)).size(px(16.)).rounded_full().bg(t::accent()).border_2().border_color(t::rgba(0xffffff, 1.)))
}

/// `.mx-name`: the name and its extension, in one box.
pub fn name_field(input: Entity<TextInput>, ext: impl Into<SharedString>, invalid: bool) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(32.))
        .rounded(px(7.))
        .bg(t::raised())
        .border_1()
        .border_color(if invalid { t::rgba(0xef4444, 1.) } else { t::line2() })
        .child(div().flex_1().min_w_0().px(px(8.)).flex().items_center().child(input))
        .child(div().flex_none().pl(px(2.)).pr(px(10.)).font_family(t::MONO).text_size(px(12.)).text_color(t::fg3()).child(ext.into()))
}

/// `.mx-sum`: "18 MB → ≈ 3.3 MB  82% smaller", how far it shrinks, and where the number comes from.
pub fn summary(before: u64, after: Option<f64>, approx: bool, note: impl Into<SharedString>) -> Div {
    let ratio = after.filter(|_| before > 0).map(|a| a / before as f64);
    let pct = ratio.map(|r| ((1. - r) * 100.).round() as i64);
    let (good, amber) = (t::rgba(0x34d399, 1.), t::rgba(0xf59e0b, 1.));
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(10.))
                .whitespace_nowrap()
                .child(div().text_size(px(14.)).text_color(t::fg3()).child(kinds::format_size(before)))
                .child(div().text_color(t::fg3()).child("→"))
                .child(div().text_size(px(20.)).font_weight(FontWeight(650.)).text_color(t::fg()).child(match after {
                    None => "…".to_string(),
                    Some(a) => format!("{}{}", if approx { "≈ " } else { "" }, kinds::format_size(a as u64)),
                }))
                .children(pct.map(|p| {
                    let ok = p >= 10;
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded_full()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if ok { good } else { amber })
                        .bg(if ok { t::rgba(0x10b981, 0.14) } else { t::rgba(0xf59e0b, 0.14) })
                        .child(if p <= 0 { "not smaller".to_string() } else if p < 10 { format!("only {p}% smaller") } else { format!("{p}% smaller") })
                })),
        )
        .child(
            div().h(px(5.)).max_w(px(420.)).rounded(px(3.)).bg(t::line2()).overflow_hidden().child(
                div()
                    .h_full()
                    .rounded(px(3.))
                    .w(relative((ratio.unwrap_or(1.) * 100.).clamp(1.5, 100.) as f32 / 100.))
                    .bg(if ratio.is_some_and(|r| r >= 1.) { amber } else { t::accent() }),
            ),
        )
        .child(div().text_size(px(11.5)).text_color(t::fg3()).child(note.into()))
}

/// `.mx-foot`: the summary, then Cancel and Compress (Ctrl+Enter).
pub fn foot(summary: Div, cancel: On, submit: Option<On>, label: impl Into<SharedString>) -> Div {
    let can = submit.is_some();
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(18.))
        .pl(px(20.))
        .pr(px(16.))
        .py(px(12.))
        .border_t_1()
        .border_color(t::line())
        .child(summary)
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(8.))
                .child(ui::button("mx-cancel", "Cancel", false).h(px(34.)).px(px(18.)).on_click(cancel))
                .child(
                    ui::button("mx-go", label, true)
                        .h(px(34.))
                        .px(px(18.))
                        .tooltip(ui::tip("Ctrl+Enter"))
                        .when(!can, |d| d.opacity(0.45))
                        .when_some(submit, |d, s| d.on_click(s)),
                ),
        )
}

/// `.btn` (and `.btn.small`), as it looks on the stage.
pub fn stage_btn(id: impl Into<ElementId>, label: impl Into<SharedString>, small: bool, lead: Option<AnyElement>) -> Stateful<Div> {
    let label = label.into();
    let bg = if t::is_dark() { t::press() } else { t::rgba(0xffffff, 0.1) };
    let hover = if t::is_dark() { t::line2() } else { t::rgba(0xffffff, 0.16) };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .h(px(if small { 26. } else { 30. }))
        .px(px(if small { 10. } else { 12. }))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(7.))
        .bg(bg)
        .hover(move |s| s.bg(hover))
        .text_size(px(if small { 12. } else { 13. }))
        .text_color(stage_fg())
        .children(lead)
        .child(label)
}

/// An icon button on the stage (`.icon-btn`).
pub fn stage_icon(id: impl Into<ElementId>, name: &'static str, label: &'static str, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .when(!enabled, |d| d.opacity(0.32))
        .when(enabled, |d| d.hover(|s| s.bg(t::rgba(0xffffff, 0.08))))
        .child(icon(name, 16., stage_fg2()))
}

/// `.mx-zoom`: the dark bar of zoom buttons over the picture.
pub fn zoom_bar() -> Div {
    div()
        .absolute()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(3.))
        .rounded(px(9.))
        .bg(t::rgba(0, 0.6))
        .text_color(t::rgba(0xffffff, 1.))
        .occlude()
}

pub fn zoom_btn(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool, click: On) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(if on { t::rgba(0xffffff, 1.) } else { t::rgba(0xffffff, 0.75) })
        .when(on, |d| d.bg(t::rgba(0xffffff, 0.2)))
        .when(!on, |d| d.hover(|s| s.bg(t::rgba(0xffffff, 0.12)).text_color(t::rgba(0xffffff, 1.))))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click)
        .child(label)
}

pub fn zoom_icon(id: impl Into<ElementId>, name: &'static str, label: &'static str, click: On) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .tooltip(ui::tip(label))
        .h(px(24.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .hover(|s| s.bg(t::rgba(0xffffff, 0.12)))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(click)
        .child(icon(name, 13., t::rgba(0xffffff, 0.85)))
}

pub fn zoom_pct(scale: f32) -> Div {
    div().min_w(px(44.)).text_size(px(11.5)).text_color(t::rgba(0xffffff, 0.8)).flex().justify_center().child(format!("{}%", (scale * 100.).round()))
}

pub fn zoom_sep() -> Div {
    div().w(px(1.)).h(px(16.)).mx(px(3.)).bg(t::rgba(0xffffff, 0.2))
}

/// `.mx-tag`: a dark pill of white text over the picture.
pub fn tag(text: impl Into<SharedString>) -> Div {
    div()
        .absolute()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(10.))
        .py(px(4.))
        .rounded_full()
        .text_size(px(12.))
        .text_color(t::rgba(0xffffff, 1.))
        .bg(t::rgba(0, 0.6))
        .whitespace_nowrap()
        .child(text.into())
}

/// The compare divider (`.mx-split` / `.mx-vsplit`) at `x` px, with its ‹› knob.
pub fn divider(x: f32) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(x - 1.))
        .w(px(2.))
        .bg(t::rgba(0xffffff, 1.))
        .shadow(vec![BoxShadow { color: t::rgba(0, 0.6), offset: point(px(0.), px(0.)), blur_radius: px(6.), spread_radius: px(0.), inset: false }])
        .child(
            div()
                .absolute()
                .top(relative(0.5))
                .left(px(-15.))
                .mt(px(-15.))
                .h(px(30.))
                .px(px(4.))
                .flex()
                .items_center()
                .rounded(px(15.))
                .bg(t::rgba(0xffffff, 1.))
                .shadow(vec![BoxShadow { color: t::rgba(0, 0.5), offset: point(px(0.), px(2.)), blur_radius: px(10.), spread_radius: px(0.), inset: false }])
                .child(icon("chevron-left", 13., t::rgba(0x111111, 1.)))
                .child(icon("chevron-right", 13., t::rgba(0x111111, 1.))),
        )
}

/// Names the app refuses (`BAD_NAME`): what to say, or None.
pub fn name_error(name: &str, single: bool) -> Option<&'static str> {
    let n = if single { name.trim() } else { name };
    if single && n.is_empty() {
        return Some("Enter a name");
    }
    let bad = n.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20) || n.ends_with('.') || n.ends_with(' ');
    bad.then_some("Names can't contain < > : \" / \\ | ? * or end with a dot or space")
}

/// "4-second", "1.2-second".
pub fn seconds_label(ms: f64) -> String {
    let s = (ms / 100.).round() / 10.;
    if s.fract() == 0. { format!("{s:.0}-second") } else { format!("{s:.1}-second") }
}

/// `fmtTime`: "0:03.4", "1:02:03.4" (with tenths) or "0:03".
pub fn clock(ms: f64, tenths: bool) -> String {
    let s = ms.max(0.) / 1000.;
    let h = (s / 3600.).floor() as u64;
    let m = ((s % 3600.) / 60.).floor() as u64;
    let sec = s % 60.;
    let ss = if tenths { format!("{:04.1}", sec) } else { format!("{:02}", sec.floor() as u64) };
    if h > 0 { format!("{h}:{m:02}:{ss}") } else { format!("{m}:{ss}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_names_like_the_app() {
        assert_eq!(clock(3400., true), "0:03.4");
        assert_eq!(clock(12_000., false), "0:12");
        assert_eq!(clock(3_723_400., true), "1:02:03.4");
        assert_eq!(seconds_label(4000.), "4-second");
        assert_eq!(seconds_label(1200.), "1.2-second");
        assert_eq!(name_error("  ", true), Some("Enter a name"));
        assert!(name_error("a:b", true).is_some());
        assert!(name_error("clip (compressed)", true).is_none());
        assert!(name_error(" (compressed)", false).is_none());
    }
}
