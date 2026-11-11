//! Small pieces drawn the way src/styles.css draws them: icons, buttons, badges, the segmented
//! control, toggles, tooltips, panels.

use gpui::{
    AnyView, App, Context, Div, ElementId, FontWeight, Hsla, IntoElement, ParentElement, Render, Role, SharedString, Stateful,
    Styled, Svg, Toggled, Window, div, prelude::*, px, svg,
};

use crate::theme as t;

/// Ferret's logo, as the app's About and intro show it (public/ferret-256.png, the app icon's
/// picture), `size` px square.
pub fn logo(size: f32) -> gpui::Img {
    static LOGO: std::sync::LazyLock<std::sync::Arc<gpui::Image>> =
        std::sync::LazyLock::new(|| std::sync::Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, include_bytes!("../../public/ferret-256.png").to_vec())));
    crate::imgmem::img(LOGO.clone()).size(px(size)).flex_none()
}

/// Plain text a screen reader can reach: GPUI only puts elements that have a role into the
/// accessibility tree, so a message, hint or error drawn as bare text would be silent.
pub fn note(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    // A label's text is its value to AccessKit (`label_comes_from_value`): with only
    // `aria_label`, UI Automation read every note as an empty Text.
    div().id(id).role(Role::Label).aria_label(text.clone()).aria_value(text.clone()).child(text)
}

pub fn panel() -> Div {
    div().bg(t::panel()).border_1().border_color(t::line()).rounded(px(12.))
}

/// A Lucide icon. GPUI doesn't pass the text color down to SVGs, so each icon names its own.
pub fn icon(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).flex_none().text_color(color)
}

/// An icon that takes `hover` while the `group` around it is hovered (CSS `:hover` on the parent).
pub fn hover_icon(name: &str, size: f32, color: Hsla, hover: Hsla, group: &'static str) -> Svg {
    icon(name, size, color).group_hover(group, move |s| s.text_color(hover))
}

pub fn badge(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(5.))
        .border_1()
        .border_color(t::line2())
        .text_size(px(11.))
        .text_color(t::fg3())
        .child(text.into())
}

/// A drive letter in a small box (sidebar, path bar, search results).
pub fn drive_badge(letter: char, active: bool) -> Div {
    div()
        .flex_none()
        .w(px(19.))
        .h(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .border_1()
        .border_color(if active { t::accent_line() } else { t::line2() })
        .bg(if active { t::accent_softer() } else { t::hover() })
        .text_size(px(10.5))
        .font_weight(FontWeight::BOLD)
        .text_color(if active { t::fg() } else { t::fg2() })
        .child(letter.to_string())
}

/// What an icon-only button is called to a screen reader (callers with a better name set their
/// own `aria_label`, which wins).
pub fn icon_label(name: &str) -> &'static str {
    match name {
        "panel-left" => "Sidebar",
        "panel-right" => "Preview pane",
        "arrow-left" => "Back",
        "arrow-right" => "Forward",
        "arrow-up" => "Up",
        "search" => "Search",
        "list-filter" => "Filter",
        "x" => "Close",
        "chevron-left" => "Previous",
        "chevron-right" => "Next",
        "external-link" => "Open",
        "maximize-2" => "Quick Look",
        "circle-help" | "circle-question-mark" => "Help",
        "trash-2" => "Forget",
        "eye" | "eye-off" => "Hidden items",
        "columns-2" => "Split view",
        "list" => "List",
        "layout-grid" => "Grid",
        "plus" => "New",
        "rotate-cw" => "Refresh",
        "star" => "Favorite",
        "layers" => "Group",
        "contrast" => "Differences",
        "zap" => "Drive index",
        "server" => "Server",
        _ => "Button",
    }
}

/// `.icon-btn`: a 30px square button with an icon.
pub fn icon_btn(id: impl Into<ElementId>, name: &'static str, enabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(icon_label(name))
        .group("ibtn")
        .flex_none()
        .w(px(30.))
        .h(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .when(!enabled, |d| d.opacity(0.32))
        .when(enabled, |d| d.hover(|s| s.bg(t::hover())).active(|s| s.bg(t::press())))
        .child(if enabled { hover_icon(name, 16., t::fg2(), t::fg(), "ibtn") } else { icon(name, 16., t::fg2()) })
}

/// `.btn`: a text button; `primary` is the accent one.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .rounded(px(8.))
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |d| d.bg(t::accent()).text_color(t::on_accent()).hover(|s| s.opacity(0.92)))
        .when(!primary, |d| {
            d.bg(t::hover()).border_1().border_color(t::line2()).text_color(t::fg()).hover(|s| s.bg(t::press()))
        })
        .child(label)
}

pub fn danger_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .h(px(32.))
        .px(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .font_weight(FontWeight::MEDIUM)
        .bg(t::rgba(0xdc2626, 1.))
        .text_color(t::rgba(0xffffff, 1.))
        .hover(|s| s.opacity(0.92))
        .child(label)
}

/// `.sb-btn`: the status bar's buttons.
pub fn status_btn(id: impl Into<ElementId>, icon_name: Option<&'static str>, label: Option<SharedString>, on: bool) -> Stateful<Div> {
    let name: SharedString = label.clone().unwrap_or_else(|| icon_label(icon_name.unwrap_or_default()).into());
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(name)
        // An icon alone is an on/off switch (hidden items, split, preview).
        .when(label.is_none(), |d| d.aria_toggled(if on { Toggled::True } else { Toggled::False }))
        .group("sbb")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .px(px(if label.is_some() { 9. } else { 7. }))
        .rounded(px(6.))
        .whitespace_nowrap()
        .text_color(if on { t::accent_text() } else { t::fg2() })
        .hover(|s| s.bg(t::hover()).text_color(if on { t::accent_text() } else { t::fg() }))
        .children(icon_name.map(|n| {
            if on { icon(n, 14., t::accent_text()) } else { hover_icon(n, 14., t::fg2(), t::fg(), "sbb") }
        }))
        .children(label)
}

/// `.seg`: a segmented control's frame; its buttons come from `seg_btn`.
pub fn seg() -> Div {
    div().flex().flex_none().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(t::hover())
}

pub fn seg_btn(id: impl Into<ElementId>, icon_name: Option<&'static str>, label: Option<SharedString>, on: bool) -> Stateful<Div> {
    let name: SharedString = label.clone().unwrap_or_else(|| icon_label(icon_name.unwrap_or_default()).into());
    div()
        .id(id)
        .role(Role::RadioButton)
        .aria_label(name)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .h(px(24.))
        .min_w(px(28.))
        .px(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(6.))
        .text_color(if on { t::fg() } else { t::fg3() })
        .when(on, |d| {
            d.bg(t::raised()).shadow(vec![gpui::BoxShadow {
                color: t::rgba(0, 0.25),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(3.),
                spread_radius: px(0.),
                inset: false,
            }])
        })
        .when(!on, |d| d.hover(|s| s.text_color(t::fg())))
        .children(icon_name.map(|n| icon(n, 14., if on { t::fg() } else { t::fg3() })))
        .children(label)
}

/// A switch (`.toggle`).
pub fn toggle(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Switch)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .rounded(px(10.))
        .p(px(2.))
        .flex()
        .when(on, |d| d.justify_end().bg(t::accent()))
        .when(!on, |d| d.justify_start().bg(t::press()).border_1().border_color(t::line2()))
        .child(div().size(px(if on { 16. } else { 14. })).rounded(px(8.)).bg(if on { t::on_accent() } else { t::fg2() }))
}

/// A checkbox with its label.
pub fn checkbox(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label.clone())
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .flex()
        .items_center()
        .gap(px(8.))
        .text_color(t::fg())
        .child(
            div()
                .flex_none()
                .size(px(16.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .when(on, |d| d.bg(t::accent()).child(icon("check", 12., t::on_accent())))
                .when(!on, |d| d.border_1().border_color(t::fg3())),
        )
        .child(label)
}

/// A node a screen reader announces when its text changes (a live region, `aria-live`):
/// toasts, the palette's choice, progress. Takes no space.
pub fn live(id: impl Into<ElementId>, text: impl Into<SharedString>, assertive: bool) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Status)
        .aria_label(text)
        .absolute()
        .size_0()
        .a11y_synthetic_children(move |b| b.parent_node().set_live(if assertive { gpui::accesskit::Live::Assertive } else { gpui::accesskit::Live::Polite }))
}

/// A text field's frame (`.field`), holding a TextInput.
pub fn field(focused: bool) -> Div {
    div()
        .h(px(32.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(8.))
        .bg(t::hover())
        .border_1()
        .border_color(if focused { t::accent_line() } else { t::line2() })
}

pub fn heading(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t::fg3()).child(text.into().to_uppercase())
}

pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(t::hover())
        .border_1()
        .border_color(t::line2())
        .text_size(px(11.))
        .text_color(t::fg2())
        .child(text.into())
}

/// A spinning loader icon.
/// The shape of what's loading, for `skeleton`.
pub enum Skeleton {
    /// Lines of text (documents, code, data).
    Lines,
    /// A page `width` wide, `aspect` (height / width) tall.
    Page { width: f32, aspect: f32 },
    /// A picture or a video's frame, `w` × `h`.
    Picture { w: f32, h: f32 },
    /// A list of items (an archive's files).
    Rows,
}

/// What a preview is going to look like, while it loads: soft shapes that breathe. They appear
/// only after a moment (a quick load never shows them, so nothing blinks) and fade in.
pub fn skeleton(id: impl Into<ElementId>, shape: Skeleton) -> gpui::AnyElement {
    use gpui::{Animation, AnimationExt, relative};
    let fill = t::rgba(0x808080, 0.17);
    let bar = move |w: f32, h: f32| div().h(px(h)).w(relative(w)).rounded(px(h / 2.)).bg(fill);
    let body = match shape {
        Skeleton::Lines => div().w_full().pt(px(6.)).flex().flex_col().gap(px(11.)).children([0.92, 0.78, 0.86, 0.64, 0.9, 0.7, 0.82, 0.55, 0.88, 0.74, 0.6].map(|w| bar(w, 9.))),
        Skeleton::Rows => div().w_full().pt(px(4.)).flex().flex_col().gap(px(13.)).children([0.6, 0.45, 0.7, 0.5, 0.66, 0.4, 0.56].map(|w| {
            div().flex().items_center().gap(px(9.)).child(div().flex_none().size(px(15.)).rounded(px(4.)).bg(fill)).child(bar(w, 9.))
        })),
        Skeleton::Page { width, aspect } => div()
            .w(px(width))
            .h(px(width * aspect))
            .rounded(px(3.))
            .bg(t::rgba(0xffffff, 0.08))
            .border_1()
            .border_color(t::rgba(0x808080, 0.12))
            .p(px((width * 0.09).max(10.)))
            .flex()
            .flex_col()
            .gap(px((width * 0.025).max(6.)))
            .children([0.55, 0.92, 0.86, 0.9, 0.7, 0.88, 0.8, 0.5].map(|w| bar(w, (width * 0.018).clamp(5., 8.)))),
        Skeleton::Picture { w, h } => div().w(px(w)).h(px(h)).rounded(px(8.)).bg(fill).flex().items_center().justify_center().child(spinner(20., t::fg3())),
    };
    let id: ElementId = id.into();
    // Full width (lines are a share of it), what has a size of its own centred in it.
    div()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .child(body.with_animation(
            ElementId::NamedChild(std::sync::Arc::new(id.clone()), "breathe".into()),
            Animation::new(std::time::Duration::from_millis(1600)).repeat(),
            // between 55 and 100%, smoothly
            |d, k| d.opacity(0.55 + 0.45 * (0.5 + 0.5 * (k * std::f32::consts::TAU).cos())),
        ))
        // nothing for the first 150 ms, then a 300 ms fade in
        .with_animation(ElementId::NamedChild(std::sync::Arc::new(id), "appear".into()), Animation::new(std::time::Duration::from_millis(450)), |d, k| d.opacity(((k - 0.33) / 0.67).clamp(0., 1.)))
        .into_any_element()
}

pub fn spinner(size: f32, color: Hsla) -> impl IntoElement {
    use gpui::{Animation, AnimationExt, Transformation, percentage};
    icon("loader-circle", size, color).with_animation(
        "spin",
        Animation::new(std::time::Duration::from_millis(900)).repeat(),
        |s, d| s.with_transformation(Transformation::rotate(percentage(d))),
    )
}

/// A tooltip's view (GPUI shows tooltips as views).
pub struct Tip(pub SharedString, pub bool);

static MENU_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by the window as it draws: while one of its menus is open, tooltips draw nothing.
pub fn set_menu_open(open: bool) {
    MENU_OPEN.store(open, std::sync::atomic::Ordering::Relaxed);
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // `.1`: a tooltip inside the menu itself (its icon row), which stays.
        if !self.1 && MENU_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
            return div();
        }
        div()
            .max_w(px(420.))
            .px(px(9.))
            .py(px(5.))
            .rounded(px(7.))
            .bg(t::raised())
            .border_1()
            .border_color(t::line2())
            .shadow(t::shadow())
            .font_family(t::FONT)
            .text_size(px(12.))
            .text_color(t::fg())
            .child(self.0.clone())
    }
}

/// The keyboard's focus ring (`:focus-visible`), laid over an item (which must be `relative`).
pub fn ring(radius: f32) -> Div {
    div().absolute().top_0().left_0().size_full().rounded(px(radius)).border_2().border_color(t::accent())
}

pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone(), false)).into()
}

/// A tooltip on something inside an open menu (shown while the menu is open).
pub fn menu_tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone(), true)).into()
}

/// A modal's card (`.modal`), centered over a scrim.
pub fn modal_card(width: f32) -> Div {
    div()
        .w(px(width))
        .max_w_full()
        .flex()
        .flex_col()
        .rounded(px(14.))
        .bg(t::raised())
        .border_1()
        .border_color(t::line2())
        .shadow(t::shadow())
        .overflow_hidden()
}

/// A small horizontal bar (capacity, progress).
pub fn bar(fraction: f32, color: Hsla) -> Div {
    div()
        .h(px(4.))
        .rounded(px(4.))
        .bg(t::line2())
        .overflow_hidden()
        .child(div().h_full().w(gpui::relative(fraction.clamp(0., 1.))).rounded(px(4.)).bg(color))
}
