//! Context menus (ContextMenu.tsx, menus.ts): Windows 11-style menus with an icon row, headings,
//! separators, checks, shortcut hints, details, submenus (some filled in when opened), keyboard
//! use, and the menus for items, folder backgrounds, columns and sidebar places. Other apps'
//! Explorer entries (7-Zip, Git, VS Code…) are merged in, and "Show more options" opens
//! Explorer's own menu.

use std::sync::Arc;
use std::time::Duration;

use ferret_core::paths;
use gpui::AnimationExt;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, Image, MouseButton, Pixels, Point, SharedString, Window, div, img, point,
    prelude::*, px,
};

use crate::kinds::Kind;
use crate::settings::Column;
use crate::sortgroup::Mode;
use crate::theme as t;
use crate::ui::icon;
use crate::util::{self, same};
use crate::view::{Action, Ferret, action};

#[derive(Clone, PartialEq)]
pub enum Kind2 {
    Item,
    Separator,
    Heading,
    /// The row of icon buttons at the top (Cut, Copy, Rename, Delete).
    Icons,
}

#[derive(Clone)]
pub enum Sub {
    Items(Vec<MenuItem>),
    /// Filled in when it opens (Open with ▸, New ▸ with Windows' own types…).
    Lazy(Rc<dyn Fn(&mut Ferret, usize, &mut Context<Ferret>)>),
    Loading,
    Failed(SharedString),
}

use std::rc::Rc;

#[derive(Clone)]
pub struct MenuItem {
    pub kind: Kind2,
    pub label: SharedString,
    pub icon: Option<&'static str>,
    pub image: Option<Arc<Image>>,
    pub keys: Option<SharedString>,
    pub detail: Option<SharedString>,
    pub checked: Option<bool>,
    pub disabled: bool,
    pub danger: bool,
    pub keep_open: bool,
    pub chip: Option<Hsla>,
    pub action: Option<Action>,
    pub sub: Option<Sub>,
    /// For the icon row: its buttons.
    pub icons: Vec<MenuItem>,
}

pub fn item(label: impl Into<SharedString>, run: Action) -> MenuItem {
    MenuItem {
        kind: Kind2::Item,
        label: label.into(),
        icon: None,
        image: None,
        keys: None,
        detail: None,
        checked: None,
        disabled: false,
        danger: false,
        keep_open: false,
        chip: None,
        action: Some(run),
        sub: None,
        icons: Vec::new(),
    }
}

pub fn submenu(label: impl Into<SharedString>, items: Vec<MenuItem>) -> MenuItem {
    MenuItem { action: None, sub: Some(Sub::Items(items)), ..item(label, action(|_, _, _| {})) }
}

pub fn lazy(label: impl Into<SharedString>, load: impl Fn(&mut Ferret, usize, &mut Context<Ferret>) + 'static) -> MenuItem {
    MenuItem { action: None, sub: Some(Sub::Lazy(Rc::new(load))), ..item(label, action(|_, _, _| {})) }
}

pub fn sep() -> MenuItem {
    MenuItem { kind: Kind2::Separator, action: None, ..item("", action(|_, _, _| {})) }
}

pub fn heading(label: impl Into<SharedString>) -> MenuItem {
    MenuItem { kind: Kind2::Heading, action: None, ..item(label, action(|_, _, _| {})) }
}

pub fn icon_row(buttons: Vec<MenuItem>) -> MenuItem {
    MenuItem { kind: Kind2::Icons, action: None, icons: buttons, ..item("", action(|_, _, _| {})) }
}

impl MenuItem {
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon = Some(name);
        self
    }
    pub fn image(mut self, image: Option<Arc<Image>>) -> Self {
        self.image = image;
        self
    }
    pub fn keys(mut self, k: impl Into<SharedString>) -> Self {
        self.keys = Some(k.into());
        self
    }
    pub fn detail(mut self, d: impl Into<SharedString>) -> Self {
        let d = d.into();
        if !d.is_empty() {
            self.detail = Some(d);
        }
        self
    }
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self
    }
    pub fn disabled(mut self, off: bool) -> Self {
        self.disabled = off;
        self
    }
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
    pub fn keep_open(mut self) -> Self {
        self.keep_open = true;
        self
    }
    pub fn chip(mut self, c: Hsla) -> Self {
        self.chip = Some(c);
        self
    }
}

const ITEM_H: f32 = 28.;
const SEP_H: f32 = 7.;
const HEAD_H: f32 = 25.;
const ICONS_H: f32 = 37.;
const WIDTH: f32 = 260.;

fn height_of(i: &MenuItem) -> f32 {
    match i.kind {
        Kind2::Item => ITEM_H,
        Kind2::Separator => SEP_H,
        Kind2::Heading => HEAD_H,
        Kind2::Icons => ICONS_H,
    }
}

/// One level of an open menu: where it is, what it holds, which item the keyboard is on.
pub struct Level {
    pub at: Point<Pixels>,
    pub items: Vec<MenuItem>,
    pub focus: Option<usize>,
    /// Which item's submenu is open (it is the next level).
    pub open: Option<usize>,
    /// A level taller than the window scrolls (a folder's 80 subfolders, Open with ▸…).
    pub scroll: gpui::ScrollHandle,
}

impl Level {
    fn new(at: Point<Pixels>, items: Vec<MenuItem>) -> Self {
        Level { at, items, focus: None, open: None, scroll: gpui::ScrollHandle::new() }
    }
}

/// The tallest a menu level gets in a window `vh` high (it scrolls beyond that).
fn max_h(vh: f32) -> f32 {
    (vh - 12.).max(80.)
}

pub struct Menu {
    pub levels: Vec<Level>,
    pub opened: std::time::Instant,
    /// The pointer's last position over the menu (to tell "heading for the open submenu" from
    /// "browsing this menu"), and the count that cancels a pending switch away from a submenu.
    pub last: Point<Pixels>,
    pub prev: Point<Pixels>,
    pub pending: u64,
}

/// Is point p inside triangle abc?
fn in_triangle(p: Point<Pixels>, a: Point<Pixels>, b: Point<Pixels>, c: Point<Pixels>) -> bool {
    let s = |p1: Point<Pixels>, p2: Point<Pixels>, p3: Point<Pixels>| {
        (p1.x - p3.x).as_f32() * (p2.y - p3.y).as_f32() - (p2.x - p3.x).as_f32() * (p1.y - p3.y).as_f32()
    };
    let (d1, d2, d3) = (s(p, a, b), s(p, b, c), s(p, c, a));
    !((d1 < 0. || d2 < 0. || d3 < 0.) && (d1 > 0. || d2 > 0. || d3 > 0.))
}

impl Ferret {
    pub fn open_menu(&mut self, at: Point<Pixels>, items: Vec<MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        let items = tidy(items);
        if items.is_empty() {
            return;
        }
        let at = fit(at, &items, window);
        self.menu = Some(Menu { levels: vec![Level::new(at, items)], opened: std::time::Instant::now(), last: at, prev: at, pending: 0 });
        cx.notify();
    }

    /// Opens the first level scrolled so item `ix` sits in the middle (when the menu scrolls).
    /// Set as an offset from the items' known heights: GPUI's `scroll_to_item` does nothing on a
    /// list's first frame.
    pub fn scroll_menu_to(&mut self, ix: usize, window: &Window) {
        let Some(level) = self.menu.as_mut().and_then(|m| m.levels.first_mut()) else { return };
        let total: f32 = level.items.iter().map(height_of).sum::<f32>() + 10.;
        let shown = max_h(window.viewport_size().height.as_f32()).min(total);
        let at: f32 = level.items.iter().take(ix).map(height_of).sum::<f32>() + ITEM_H / 2.;
        let y = (at - shown / 2.).clamp(0., (total - shown).max(0.));
        level.scroll.set_offset(point(px(0.), px(-y)));
    }

    /// The pointer, at `p`, is travelling from where it last was toward level `depth`'s open
    /// submenu (inside the triangle from there to the submenu's near edge).
    fn aiming_at_sub(&self, depth: usize, p: Point<Pixels>) -> bool {
        let Some(m) = &self.menu else { return false };
        let Some(sub) = m.levels.get(depth + 1) else { return false };
        let h: f32 = sub.items.iter().map(height_of).sum::<f32>() + 12.;
        let (left, right) = (sub.at.x, sub.at.x + px(WIDTH));
        // Where it came from: the last position before this one.
        let from = if m.last == p { m.prev } else { m.last };
        if from == p {
            return false;
        }
        let edge = if left >= from.x { left } else { right };
        in_triangle(p, from, point(edge, sub.at.y - px(8.)), point(edge, sub.at.y + px(h + 8.)))
    }

    /// The pointer came onto item `ix` of level `depth`: it takes the highlight, and opens its
    /// submenu (or closes another one).
    fn hover_item(&mut self, depth: usize, ix: usize, has_sub: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.menu.as_mut() else { return };
        m.pending += 1;
        if let Some(l) = m.levels.get_mut(depth) {
            l.focus = Some(ix);
        }
        let open = m.levels.get(depth).and_then(|l| l.open);
        if has_sub && open != Some(ix) {
            self.open_sub(depth, ix, window, cx);
        } else if !has_sub && open.is_some() {
            if let Some(m) = self.menu.as_mut() {
                m.levels.truncate(depth + 1);
                m.levels[depth].open = None;
            }
            cx.notify();
        }
    }

    pub fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens item `ix` of level `depth`'s submenu (loading it if need be).
    fn open_sub(&mut self, depth: usize, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_mut() else { return };
        menu.levels.truncate(depth + 1);
        let level = &mut menu.levels[depth];
        let Some(it) = level.items.get(ix) else { return };
        let Some(sub) = it.sub.clone() else {
            level.open = None;
            cx.notify();
            return;
        };
        level.open = Some(ix);
        // Beside its item, wherever a scrolled level has it now (the offset is negative).
        let y: f32 = level.items[..ix].iter().map(height_of).sum::<f32>() + 5. + level.scroll.offset().y.as_f32();
        let at = point(level.at.x + px(WIDTH - 6.), level.at.y + px(y - 5.));
        let (items, load) = match sub {
            Sub::Items(items) => (tidy(items), None),
            Sub::Lazy(load) => (vec![loading_item()], Some(load)),
            Sub::Loading => (vec![loading_item()], None),
            Sub::Failed(e) => (vec![item(e, action(|_, _, _| {})).disabled(true)], None),
        };
        self.place_level(depth + 1, at, items, window);
        // Filled in once the level exists ("Loading…" until then): a loader that has its items
        // at once (New ▸) replaces it straight away.
        if let Some(load) = load {
            load(self, depth + 1, cx);
            self.refit_level(depth + 1, window);
        }
        cx.notify();
    }

    /// Keeps a level inside the window after its items changed.
    fn refit_level(&mut self, depth: usize, window: &Window) {
        let Some(menu) = self.menu.as_mut() else { return };
        let Some(level) = menu.levels.get(depth) else { return };
        let (at, items) = (level.at, level.items.clone());
        let vw = window.viewport_size();
        let h: f32 = items.iter().map(height_of).sum::<f32>() + 12.;
        let y = at.y.min(px((vw.height.as_f32() - h - 6.).max(6.)));
        if let Some(l) = self.menu.as_mut().and_then(|m| m.levels.get_mut(depth)) {
            l.at.y = y;
        }
    }

    fn place_level(&mut self, depth: usize, at: Point<Pixels>, items: Vec<MenuItem>, window: &Window) {
        let Some(menu) = self.menu.as_mut() else { return };
        let vw = window.viewport_size();
        let h: f32 = items.iter().map(height_of).sum::<f32>() + 12.;
        // A submenu that doesn't fit on the right opens on the left of its menu.
        let mut x = at.x;
        if (x + px(WIDTH)).as_f32() > vw.width.as_f32() - 6. {
            let parent_x = menu.levels.get(depth - 1).map(|l| l.at.x).unwrap_or(at.x);
            x = (parent_x - px(WIDTH - 6.)).max(px(6.));
        }
        let y = at.y.min(px((vw.height.as_f32() - h - 6.).max(6.)));
        menu.levels.truncate(depth);
        menu.levels.push(Level::new(point(x, y), items));
    }

    /// A lazily loaded submenu's items arrived.
    pub fn fill_sub(&mut self, depth: usize, items: Vec<MenuItem>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_ref() else { return };
        let Some(level) = menu.levels.get(depth) else { return };
        let at = level.at;
        let items = if items.is_empty() { vec![item("Nothing here", action(|_, _, _| {})).disabled(true)] } else { tidy(items) };
        self.place_level(depth, at, items, window);
        cx.notify();
    }

    fn run_item(&mut self, depth: usize, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(it) = self.menu.as_ref().and_then(|m| m.levels.get(depth)).and_then(|l| l.items.get(ix)).cloned() else { return };
        if it.disabled || it.kind != Kind2::Item {
            return;
        }
        if it.sub.is_some() {
            self.open_sub(depth, ix, window, cx);
            return;
        }
        if !it.keep_open {
            self.close_menu(window, cx);
        }
        if let Some(run) = it.action {
            run(self, window, cx);
        }
        // A kept-open menu (checks that toggle) shows the new state: rebuilt by the caller's
        // next open; here, its checks follow the item that was clicked.
        if it.keep_open {
            if let Some(m) = self.menu.as_mut() {
                if let Some(l) = m.levels.get_mut(depth) {
                    if l.items[ix].checked.is_some() {
                        // Radio groups: a checked item among siblings separated by lines.
                        let (mut a, mut b) = (ix, ix);
                        while a > 0 && l.items[a - 1].kind == Kind2::Item {
                            a -= 1;
                        }
                        while b + 1 < l.items.len() && l.items[b + 1].kind == Kind2::Item {
                            b += 1;
                        }
                        let toggle = b == a;
                        if toggle {
                            let c = l.items[ix].checked.as_mut().unwrap();
                            *c = !*c;
                        } else {
                            for k in a..=b {
                                if let Some(c) = l.items[k].checked.as_mut() {
                                    *c = k == ix;
                                }
                            }
                        }
                    }
                }
            }
            cx.notify();
        }
    }

    pub fn menu_key(&mut self, key: &str, _m: gpui::Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_mut() else { return };
        let depth = menu.levels.len() - 1;
        let level = &mut menu.levels[depth];
        let n = level.items.len();
        let usable = |i: &MenuItem| i.kind == Kind2::Item && !i.disabled;
        let step = |from: Option<usize>, dir: isize, items: &[MenuItem]| -> Option<usize> {
            let mut i = from.map_or(if dir > 0 { -1 } else { n as isize }, |x| x as isize);
            for _ in 0..n {
                i = (i + dir).rem_euclid(n as isize);
                if usable(&items[i as usize]) {
                    return Some(i as usize);
                }
            }
            None
        };
        match key {
            "escape" => {
                if depth > 0 {
                    menu.levels.pop();
                    menu.levels[depth - 1].open = None;
                    cx.notify();
                } else {
                    self.close_menu(window, cx);
                }
            }
            "down" | "up" | "home" | "end" => {
                level.focus = match key {
                    "down" => step(level.focus, 1, &level.items),
                    "up" => step(level.focus, -1, &level.items),
                    "home" => step(None, 1, &level.items),
                    _ => step(None, -1, &level.items),
                };
                // In a level that scrolls, the item the keyboard is on stays in view.
                if let Some(f) = level.focus {
                    level.scroll.scroll_to_item(f);
                }
                cx.notify();
            }
            "left" if depth > 0 => {
                menu.levels.pop();
                menu.levels[depth - 1].open = None;
                cx.notify();
            }
            "right" => {
                if let Some(f) = level.focus.filter(|&f| level.items[f].sub.is_some()) {
                    self.open_sub(depth, f, window, cx);
                    if let Some(m) = self.menu.as_mut() {
                        if let Some(l) = m.levels.get_mut(depth + 1) {
                            l.focus = l.items.iter().position(usable);
                        }
                    }
                }
            }
            "enter" | "space" => {
                if let Some(f) = level.focus {
                    self.run_item(depth, f, window, cx);
                    if let Some(m) = self.menu.as_mut() {
                        if let Some(l) = m.levels.get_mut(depth + 1) {
                            l.focus = l.items.iter().position(usable);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn menu_view(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let vh = window.viewport_size().height.as_f32();
        let levels: Vec<AnyElement> = menu.levels.iter().enumerate().map(|(depth, level)| self.level_view(depth, level, vh, cx)).collect();
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(MouseButton::Left, cx.listener(|f, _, window, cx| f.close_menu(window, cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(|f, _, window, cx| f.close_menu(window, cx)))
                .children(levels)
                .into_any_element(),
        )
    }

    fn level_view(&self, depth: usize, level: &Level, vh: f32, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = level.items.iter().enumerate().map(|(ix, it)| self.menu_item_view(depth, ix, it, level, cx)).collect();
        let el = div()
            .id(("menu", depth))
            .role(gpui::Role::Menu)
            .absolute()
            .left(level.at.x)
            .top(level.at.y)
            .w(px(WIDTH))
            // No taller than the window: the rest scrolls (a long menu ran off the window's
            // bottom, its last items out of reach).
            .max_h(px(max_h(vh)))
            .overflow_y_scroll()
            .track_scroll(&level.scroll)
            .p(px(5.))
            .rounded(px(10.))
            .bg(t::raised())
            .border_1()
            .border_color(t::line2())
            .shadow(t::shadow())
            .text_size(px(13.))
            .text_color(t::fg())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            // Where the pointer last was (after the items' own hover handling, which runs first).
            .on_mouse_move(cx.listener(|f, ev: &gpui::MouseMoveEvent, _, _| {
                if let Some(m) = f.menu.as_mut() {
                    if m.last != ev.position {
                        m.prev = m.last;
                        m.last = ev.position;
                    }
                }
            }))
            // The pointer reached this submenu: its parent keeps it open (no pending switch).
            .on_hover(cx.listener(move |f, hovered: &bool, _, cx| {
                if !*hovered || depth == 0 {
                    return;
                }
                if let Some(m) = f.menu.as_mut() {
                    m.pending += 1;
                    if let Some(parent) = m.levels.get_mut(depth - 1) {
                        parent.focus = parent.open;
                    }
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .children(rows);
        if t::animate() {
            let sub = depth > 0;
            el.with_animation(
                ("menu-in", depth),
                gpui::Animation::new(Duration::from_millis(130)).with_easing(gpui::ease_out_quint()),
                move |el, d| if sub { el.opacity(d).ml(px((1. - d) * -6.)) } else { el.opacity(d).mt(px((1. - d) * -6.)) },
            )
            .into_any_element()
        } else {
            el.into_any_element()
        }
    }

    fn menu_item_view(&self, depth: usize, ix: usize, it: &MenuItem, level: &Level, cx: &mut Context<Self>) -> AnyElement {
        match it.kind {
            Kind2::Separator => div().flex_none().h(px(1.)).my(px(3.)).mx(px(6.)).bg(t::line2()).into_any_element(),
            Kind2::Heading => div()
                .flex_none()
                .pt(px(7.))
                .pb(px(3.))
                .px(px(10.))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t::fg3())
                .child(it.label.to_uppercase())
                .into_any_element(),
            Kind2::Icons => div()
                .flex_none()
                .flex()
                .gap(px(2.))
                .pt(px(2.))
                .px(px(2.))
                .pb(px(3.))
                .children(it.icons.iter().enumerate().map(|(k, b)| {
                    let run = b.action.clone();
                    let disabled = b.disabled;
                    let danger = b.danger;
                    div()
                        .id(("menu-act", k))
                        .role(gpui::Role::MenuItem)
                        .aria_label(b.label.clone())
                        .flex_1()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .when(disabled, |d| d.opacity(0.35))
                        .when(!disabled, |d| d.hover(move |s| if danger { s.bg(t::rgba(0xdc2626, 0.16)) } else { s.bg(t::hover()) }))
                        .tooltip(crate::ui::menu_tip(b.label.clone()))
                        .on_click(cx.listener(move |f, _, window, cx| {
                            if disabled {
                                return;
                            }
                            f.close_menu(window, cx);
                            if let Some(run) = &run {
                                run(f, window, cx);
                            }
                        }))
                        .child(icon(b.icon.unwrap_or("circle"), 16., if danger { t::danger() } else { t::fg2() }))
                }))
                .into_any_element(),
            Kind2::Item => {
                let focused = level.focus == Some(ix);
                let sub_open = level.open == Some(ix);
                let has_sub = it.sub.is_some();
                let disabled = it.disabled;
                let danger = it.danger;
                let any_check = level.items.iter().any(|i| i.checked.is_some());
                let lead: AnyElement = if let Some(image) = &it.image {
                    img(image.clone()).size(px(16.)).rounded(px(3.)).into_any_element()
                } else if let Some(name) = it.icon {
                    icon(name, 15., t::fg2()).group_hover("mi", |s| s.text_color(t::on_accent())).into_any_element()
                } else if let Some(c) = it.chip {
                    div().size(px(12.)).rounded(px(6.)).bg(c).into_any_element()
                } else if it.checked == Some(true) {
                    icon("check", 14., t::fg2()).group_hover("mi", |s| s.text_color(t::on_accent())).into_any_element()
                } else {
                    div().into_any_element()
                };
                // The keyboard's item is the one a screen reader announces (the deepest open
                // level's, while a submenu has it).
                let claim = focused && self.menu.as_ref().and_then(|m| m.levels.iter().rposition(|l| l.focus.is_some())) == Some(depth);
                div()
                    .id(("menu-item", depth << 10 | ix))
                    .role(if it.checked.is_some() { gpui::Role::MenuItemCheckBox } else { gpui::Role::MenuItem })
                    .aria_label(it.label.clone())
                    .when_some(it.checked, |d, c| d.aria_toggled(if c { gpui::Toggled::True } else { gpui::Toggled::False }))
                    .when(has_sub, |d| d.aria_expanded(sub_open))
                    .when_some(it.keys.clone(), |d, k| d.aria_keyshortcuts(k))
                    .when_some(it.detail.clone(), |d, k| d.aria_description(k))
                    .aria_position_in_set(ix + 1)
                    .aria_size_of_set(level.items.len())
                    .when(disabled, |d| d.a11y_synthetic_children(|b| b.parent_node().set_disabled()))
                    .when(claim, |d| d.aria_active_descendant())
                    .group("mi")
                    .w_full()
                    .h(px(ITEM_H))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(9.))
                    .pl(px(6.))
                    .pr(px(10.))
                    .rounded(px(6.))
                    .when(disabled, |d| d.opacity(0.38))
                    .when((focused || sub_open) && !disabled, |d| d.bg(t::accent_soft()))
                    .when(!disabled, |d| {
                        d.hover(move |s| {
                            if danger { s.bg(t::rgba(0xdc2626, 1.)).text_color(t::rgba(0xffffff, 1.)) } else { s.bg(t::accent()).text_color(t::on_accent()) }
                        })
                    })
                    .on_hover(cx.listener(move |f, hovered: &bool, window, cx| {
                        if !*hovered {
                            return;
                        }
                        // Crossing other items on the way to the open submenu doesn't switch away
                        // from it (unless the pointer stays 300 ms).
                        let open = f.menu.as_ref().and_then(|m| m.levels.get(depth)).and_then(|l| l.open);
                        if open.is_some_and(|o| o != ix) && f.aiming_at_sub(depth, window.mouse_position()) {
                            let Some(m) = f.menu.as_mut() else { return };
                            m.pending += 1;
                            let token = m.pending;
                            cx.spawn_in(window, async move |this, cx| {
                                cx.background_executor().timer(Duration::from_millis(300)).await;
                                this.update_in(cx, |f, window, cx| {
                                    if f.menu.as_ref().is_some_and(|m| m.pending == token) {
                                        f.hover_item(depth, ix, has_sub, window, cx);
                                    }
                                })
                                .ok();
                            })
                            .detach();
                            return;
                        }
                        f.hover_item(depth, ix, has_sub, window, cx);
                    }))
                    .on_click(cx.listener(move |f, _, window, cx| f.run_item(depth, ix, window, cx)))
                    .child(div().w(px(18.)).flex_none().flex().items_center().justify_center().child(lead))
                    .when(any_check && it.checked.is_some() && (it.icon.is_some() || it.image.is_some()), |d| {
                        // Items with both an icon and a check show the check on the right.
                        d
                    })
                    .child(div().flex_1().min_w_0().truncate().child(it.label.clone()))
                    .children(it.detail.clone().map(|d| div().max_w(px(120.)).truncate().text_size(px(11.5)).text_color(t::fg3()).child(d)))
                    .children(it.keys.clone().map(|k| div().flex_none().text_size(px(11.)).opacity(0.55).whitespace_nowrap().child(k)))
                    .when(it.checked == Some(true) && (it.icon.is_some() || it.image.is_some()), |d| d.child(icon("check", 13., t::fg2())))
                    .when(has_sub, |d| d.child(icon("chevron-right", 13., t::fg3())))
                    .into_any_element()
            }
        }
    }

    // ───────────────────────────── the menus ─────────────────────────────

    /// Right-click on a file or folder.
    pub fn row_menu(&mut self, id: u64, ix: usize, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // Right-click selects the item if it isn't already, in the pane it's in (split view).
        self.focus_pane_of(id, cx);
        if self.pane().id != id {
            return;
        }
        if !self.pane().marked.contains(&ix) {
            self.select(ix, cx);
        }
        let items = self.item_menu_items(window);
        let paths = self.targets();
        self.open_menu(at, items, window, cx);
        self.merge_shell_items(paths, false, window, cx);
    }

    /// Shift+F10 or the Menu key: the focused item's menu by it, or the folder's.
    pub fn keyboard_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let b = self.pane().bounds.get();
        let at = point(b.origin.x + px(60.), b.origin.y + px(60.));
        if let Some(ix) = self.pane().selected {
            let id = self.pane().id;
            self.row_menu(id, ix, at, window, cx);
        } else {
            self.background_menu(at, window, cx);
        }
    }

    pub fn item_menu_items(&self, window: &Window) -> Vec<MenuItem> {
        let p = self.pane();
        let rows = p.marked_rows();
        let n = rows.len();
        let bin = p.is_bin();
        let inside = rows.iter().any(|r| r.e.in_archive);
        let remote = rows.iter().any(|r| paths::is_remote(&r.e.path));
        let shift = window.modifiers().shift;
        let paths: Vec<String> = rows.iter().map(|r| r.e.path.clone()).collect();
        let first = rows.first().map(|r| (r.e.path.clone(), r.e.is_dir, r.e.ext.clone()));
        let Some((path, is_dir, ext)) = first else { return Vec::new() };
        if bin {
            let (p1, p2, p3) = (paths.clone(), paths.clone(), paths.clone());
            let origins: Vec<String> = rows.iter().filter_map(|r| r.e.origin.clone()).collect();
            let o1 = origins.clone();
            let origin = rows.first().and_then(|r| r.e.origin.clone());
            return vec![
                item(if n > 1 { format!("Restore {n} items") } else { "Restore".into() }, action(move |f, _, cx| f.restore_from_bin(p1.clone(), cx))).icon("archive-restore"),
                item("Preview", action(|f, _, cx| {
                    f.quicklook = true;
                    cx.notify();
                }))
                .icon("eye")
                .keys("Space"),
                item("Open original location", action(move |f, _, cx| {
                    if let Some(o) = &origin {
                        f.new_tab(o.clone(), cx);
                    }
                }))
                .icon("folder-open")
                .disabled(n != 1),
                sep(),
                item("Delete permanently", action(move |f, _, cx| f.purge_from_bin(p2.clone(), cx))).icon("trash-2").danger(),
                sep(),
                item(if n > 1 { "Copy original paths" } else { "Copy original path" }, action(move |f, _, cx| f.copy_paths(&o1, true, cx))).icon("copy"),
                item(if n > 1 { "Copy names" } else { "Copy name" }, action(move |f, _, cx| f.copy_names(&p3, cx))).icon("clipboard-copy"),
            ];
        }
        let can_change = !inside && !p.is_search() || p.is_search();
        let (c1, c2, c3, c4) = (paths.clone(), paths.clone(), paths.clone(), paths.clone());
        let mut out = vec![icon_row(vec![
            item("Cut (Ctrl+X)", action(move |f, _, cx| f.clipboard(c1.clone(), true, cx))).icon("scissors").disabled(inside),
            item("Copy (Ctrl+C)", action(move |f, _, cx| f.clipboard(c2.clone(), false, cx))).icon("copy"),
            item("Rename (F2)", action(|f, _, cx| f.start_rename(cx))).icon("text-cursor-input").disabled(inside || !can_change),
            item("Delete (Del)", action(move |f, _, cx| f.delete_items(c3.clone(), false, cx))).icon("trash-2").danger().disabled(inside),
        ])];
        let open_path = path.clone();
        out.push(item("Open", action(move |f, _, cx| f.open_selected(cx))).icon("external-link").keys("Enter"));
        let _ = open_path;
        if is_dir && n == 1 {
            let p = path.clone();
            out.push(item("Open in new tab", action(move |f, _, cx| f.new_tab(p.clone(), cx))).icon("plus").keys("Ctrl+Enter"));
        }
        if !is_dir && n == 1 && !remote {
            let p = path.clone();
            out.push(lazy("Open with", move |f, depth, cx| f.load_open_with(p.clone(), depth, cx)).icon("app-window"));
        }
        if !is_dir && n == 1 {
            out.push(item("Preview", action(|f, _, cx| {
                f.quicklook = true;
                cx.notify();
            }))
            .icon("eye")
            .keys("Space"));
        }
        let archive = !is_dir && ferret_core::archive::archive_kind(&path).is_some();
        if archive && n == 1 {
            let p = path.clone();
            out.push(item("Browse archive", action(move |f, _, cx| f.go(p.clone(), cx))).icon("folder-open"));
        }
        // Open in ▸
        let folder = if is_dir { path.clone() } else { util::parent(&path).unwrap_or_default() };
        let (f1, f2, f3, f4, f5) = (folder.clone(), folder.clone(), folder.clone(), folder.clone(), path.clone());
        let mut open_in = vec![
            item("New window", action(move |f, _, cx| f.new_window(Some(f1.clone()), cx))).icon("app-window"),
            item("Terminal", action(move |f, _, cx| f.open_terminal(f2.clone(), cx))).icon("terminal").keys("Ctrl+`"),
            item("VS Code", action(move |f, _, cx| f.open_vscode(f3.clone(), cx))).icon("code-xml"),
        ];
        if !remote {
            open_in.push(item("Explorer", action(move |f, _, cx| f.reveal_in_explorer(f5.clone(), cx))).icon("folder-open"));
            open_in.insert(1, item("New window as administrator", action(move |f, _, cx| f.run_as_admin(Some(f4.clone()), cx))).icon("shield"));
        }
        out.push(submenu("Open in", open_in).icon("folder-output"));
        out.push(sep());
        // Organize
        if is_dir && n == 1 && !inside {
            let p = path.clone();
            out.push(item("Paste into folder", action(move |f, _, cx| f.paste(p.clone(), cx))).icon("clipboard-paste"));
        }
        out.push(submenu("Copy to", self.destination_items(false, paths.clone())).icon("copy"));
        if !inside {
            out.push(submenu("Move to", self.destination_items(true, paths.clone())).icon("folder-input"));
            out.push(item("Duplicate", action(move |f, _, cx| f.duplicate(c4.clone(), cx))).icon("copy-plus"));
        }
        // Archives
        if archive && n == 1 && !remote {
            let (a1, a2, a3) = (path.clone(), path.clone(), path.clone());
            let stem = util::file_name(&path);
            let stem = stem.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(stem);
            out.push(sep());
            out.push(item("Extract here", action(move |f, _, cx| f.extract_here(a1.clone(), cx))).icon("package-open"));
            out.push(item(format!("Extract to “{stem}\\”"), action(move |f, _, cx| f.extract_smart(a2.clone(), cx))).icon("folder-output"));
            out.push(item("Extract to…", action(move |f, _, cx| f.extract_to_picked(a3.clone(), None, cx))).icon("folder-open"));
        } else if inside {
            let entries = paths.clone();
            let archive_root = ferret_core::archive::split_virtual(&path).map(|(a, _)| a.to_string()).unwrap_or_default();
            out.push(sep());
            out.push(item("Extract to…", action(move |f, _, cx| f.extract_to_picked(archive_root.clone(), Some(entries.clone()), cx))).icon("package-open"));
        } else if !remote {
            let z = paths.clone();
            out.push(sep());
            out.push(item(format!("Compress to “{}”", crate::ops::zip_name_for(&paths)), action(move |f, _, cx| f.compress_zip(z.clone(), cx))).icon("archive"));
        }
        // Media
        let videos = rows.iter().filter(|r| r.kind == Kind::Video).count();
        let pictures = rows.iter().filter(|r| crate::ops::is_compressible_picture(&r.e.ext)).count();
        if videos > 0 && !inside && !remote {
            out.push(item(if videos > 1 { format!("Compress {videos} videos…") } else { "Compress video…".into() }, action(|f, window, cx| f.open_batch_editor(window, cx))).icon("clapperboard"));
        }
        if pictures > 0 && !inside && !remote {
            let pics: Vec<String> = rows.iter().filter(|r| crate::ops::is_compressible_picture(&r.e.ext)).map(|r| r.e.path.clone()).collect();
            out.push(
                item(if pictures > 1 { format!("Compress {pictures} pictures…") } else { "Compress picture…".into() }, action(move |f, window, cx| f.open_image_editor(pics.clone(), window, cx)))
                    .icon("image-down"),
            );
        }
        out.push(sep());
        // Paths and places
        let (q1, q2) = (paths.clone(), paths.clone());
        out.push(item(if n > 1 { "Copy as paths" } else { "Copy as path" }, action(move |f, _, cx| f.copy_paths(&q1, true, cx))).icon("clipboard-copy").keys("Ctrl+Shift+C"));
        out.push(item(if n > 1 { "Copy names" } else { "Copy name" }, action(move |f, _, cx| f.copy_names(&q2, cx))).icon("type"));
        if n == 1 {
            let fav = self.favorites.iter().any(|f| same(&f.path, &path));
            let p = path.clone();
            out.push(item(if fav { "Remove from favorites" } else { "Add to favorites" }, action(move |f, _, cx| f.toggle_favorite(p.clone(), is_dir, cx))).icon("star").keys("Ctrl+D"));
        }
        out.push(sep());
        if shift && !inside {
            let d = paths.clone();
            out.push(item("Delete permanently", action(move |f, _, cx| f.delete_items(d.clone(), true, cx))).icon("trash-2").danger().keys("Shift+Del"));
        }
        let pr = paths.clone();
        out.push(item("Properties", action(move |f, _, cx| f.show_properties(pr.clone(), cx))).icon("info").keys("Alt+Enter"));
        if !inside && !remote {
            let sm = paths.clone();
            out.push(item("Show more options", action(move |f, window, cx| f.native_menu(sm.clone(), false, window, cx))).icon("ellipsis"));
        }
        let _ = ext;
        out
    }

    /// Copy to ▸ / Move to ▸: the other pane, favorite folders, recent folders, a picker.
    fn destination_items(&self, mv: bool, paths: Vec<String>) -> Vec<MenuItem> {
        let mut out = Vec::new();
        let op = if mv { "move" } else { "copy" };
        if let Some(other) = self.tab().other() {
            let dest = other.path.clone();
            let ps = paths.clone();
            out.push(item("Other pane", action(move |f, window, cx| f.transfer_to(op, ps.clone(), dest.clone(), window, cx))).icon("columns-2").detail(util::folder_name(&other.path)));
        }
        let favs: Vec<_> = self.favorites.iter().filter(|f| f.is_dir).collect();
        if !favs.is_empty() {
            out.push(heading("Favorites"));
            for fav in favs {
                let (dest, ps) = (fav.path.clone(), paths.clone());
                out.push(item(fav.name.clone(), action(move |f, window, cx| f.transfer_to(op, ps.clone(), dest.clone(), window, cx))).icon("star"));
            }
        }
        let recents: Vec<_> = self.recents.iter().filter(|r| !same(&r.path, &self.pane().path)).take(5).collect();
        if !recents.is_empty() {
            out.push(heading("Recent"));
            for r in recents {
                let (dest, ps) = (r.path.clone(), paths.clone());
                out.push(item(util::folder_name(&r.path), action(move |f, window, cx| f.transfer_to(op, ps.clone(), dest.clone(), window, cx))).icon("folder").detail(util::parent(&r.path).map(|p| util::folder_name(&p)).unwrap_or_default()));
            }
        }
        out.push(sep());
        out.push(item("Choose folder…", action(move |f, window, cx| f.transfer_with_picker(op, paths.clone(), window, cx))).icon("folder-open"));
        out
    }

    /// Right-click on empty space in a folder.
    pub fn background_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let p = self.pane();
        let path = p.path.clone();
        let v = p.view.clone();
        let bin = p.is_bin();
        let search = p.is_search();
        let archive = ferret_core::archive::split_virtual(&path).filter(|_| p.entries.is_empty() || p.entries.iter().any(|e| e.in_archive)).map(|(a, _)| a.to_string());
        let remote = paths::is_remote(&path);
        let view_items = {
            let size = |label: &'static str, s: u32| item(label, action(move |f, _, cx| f.set_view(|v| { v.mode = Mode::Grid; v.icon_size = s; }, cx))).checked(v.mode == Mode::Grid && v.icon_size == s);
            vec![
                item("List", action(|f, _, cx| f.set_mode(Mode::List, cx))).icon("list").checked(v.mode == Mode::List).keys("Ctrl+1"),
                item("Grid", action(|f, _, cx| f.set_mode(Mode::Grid, cx))).icon("layout-grid").checked(v.mode == Mode::Grid).keys("Ctrl+2"),
                sep(),
                size("Small icons", 64),
                size("Medium icons", 96),
                size("Large icons", 128),
                size("Extra large icons", 192),
                size("Huge icons", 256),
                sep(),
                item("Preview pane", action(|f, window, cx| f.toggle_preview(window, cx))).checked(self.preview_open).keys("Alt+P"),
            ]
        };
        let mut out = Vec::new();
        if let Some(a) = archive.clone() {
            let (a1, a2) = (a.clone(), a);
            out.push(item("Extract all", action(move |f, _, cx| f.extract_smart(a1.clone(), cx))).icon("package-open"));
            out.push(item("Extract to…", action(move |f, _, cx| f.extract_to_picked(a2.clone(), None, cx))).icon("folder-open"));
            out.push(sep());
        }
        out.push(submenu("View", view_items).icon("layout-grid"));
        out.push(submenu("Sort by", self.sort_items()).icon("arrow-up-down"));
        out.push(submenu("Group by", self.group_items()).icon("layers"));
        out.push(item("Show hidden items", action(|f, _, cx| f.toggle_hidden(cx))).checked(crate::pane::show_hidden()).keys("Ctrl+H"));
        out.push(sep());
        out.push(item("Refresh", action(|f, _, cx| f.refresh(cx))).icon("rotate-cw").keys("F5"));
        if bin {
            let n = p.entries.len();
            out.push(item("Restore all items", action(|f, _, cx| {
                let all = f.pane().rows.iter().map(|r| r.e.path.clone()).collect();
                f.restore_from_bin(all, cx);
            }))
            .icon("archive-restore")
            .disabled(n == 0));
            out.push(item("Empty Recycle Bin", action(|f, _, cx| f.empty_bin(cx))).icon("trash-2").danger().disabled(n == 0));
            out.push(item(self.undo_label(), action(|f, _, cx| f.undo(cx))).icon("undo-2").keys("Ctrl+Z").disabled(self.undo.is_empty()));
            out.push(sep());
            out.push(item("Select all", action(|f, _, cx| f.select_all(cx))).keys("Ctrl+A"));
            out.push(item("Open in Explorer", action(|f, _, cx| f.reveal_in_explorer("shell:RecycleBinFolder".into(), cx))).icon("folder-open"));
            self.open_menu(at, out, window, cx);
            return;
        }
        if !search && archive.is_none() {
            let dest = path.clone();
            out.push(item("Paste", action(move |f, _, cx| f.paste(dest.clone(), cx))).icon("clipboard-paste").keys("Ctrl+V"));
        }
        out.push(item(self.undo_label(), action(|f, _, cx| f.undo(cx))).icon("undo-2").keys("Ctrl+Z").disabled(self.undo.is_empty()));
        out.push(item(self.redo_label(), action(|f, _, cx| f.redo(cx))).icon("redo-2").keys("Ctrl+Y").disabled(self.redo.is_empty()));
        out.push(sep());
        if !search && archive.is_none() {
            let dir = path.clone();
            out.push(lazy("New", move |f, depth, cx| f.load_new_items(dir.clone(), depth, cx)).icon("plus"));
        }
        out.push(item("Select all", action(|f, _, cx| f.select_all(cx))).keys("Ctrl+A"));
        out.push(item("Invert selection", action(|f, _, cx| f.invert_selection(cx))));
        out.push(sep());
        if !search {
            let (d1, d2, d3, d4) = (path.clone(), path.clone(), path.clone(), path.clone());
            out.push(item("Open in Terminal", action(move |f, _, cx| f.open_terminal(d1.clone(), cx))).icon("terminal").keys("Ctrl+`"));
            out.push(item("Open in VS Code", action(move |f, _, cx| f.open_vscode(d2.clone(), cx))).icon("code-xml"));
            if !remote {
                out.push(item("Open in Explorer", action(move |f, _, cx| f.reveal_in_explorer(d3.clone(), cx))).icon("folder-open"));
                out.push(item("Run as administrator", action(move |f, _, cx| f.run_as_admin(Some(d4.clone()), cx))).icon("shield"));
            }
            out.push(sep());
            let (c1, c2) = (path.clone(), path.clone());
            out.push(item("Copy folder path", action(move |f, _, cx| f.copy_paths(std::slice::from_ref(&c1), false, cx))).icon("clipboard-copy"));
            let fav = self.favorites.iter().any(|f| same(&f.path, &path));
            out.push(item(if fav { "Remove folder from favorites" } else { "Add folder to favorites" }, action(move |f, _, cx| f.toggle_favorite(c2.clone(), true, cx))).icon("star"));
            out.push(submenu(
                "Remembered view",
                vec![
                    item("Save for this folder", action(|f, _, cx| f.save_view_here(false, cx))),
                    item("Apply to this folder and all subfolders", action(|f, _, cx| f.save_view_here(true, cx))),
                    item("Reset to default", action(|f, _, cx| f.reset_view(cx))),
                ],
            )
            .icon("bookmark-plus"));
            out.push(sep());
            let pp = path.clone();
            out.push(item("Properties", action(move |f, _, cx| f.show_properties(vec![pp.clone()], cx))).icon("info"));
            if !remote && archive.is_none() {
                let sm = path.clone();
                out.push(item("Show more options", action(move |f, window, cx| f.native_menu(vec![sm.clone()], true, window, cx))).icon("ellipsis"));
            }
        }
        self.open_menu(at, out, window, cx);
        if !search && !remote && archive.is_none() && !paths::is_wsl(&path) {
            self.merge_shell_items(vec![path], true, window, cx);
        }
    }

    /// Right-click on the column headings: hide columns, reset widths.
    pub fn column_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let col = |label: &'static str, c: Column, s: &crate::settings::Settings| {
            item(label, action(move |f, _, cx| {
                f.settings.toggle_column(c);
                cx.notify();
            }))
            .checked(!s.column_hidden(c))
            .keep_open()
        };
        let items = vec![
            heading("Columns"),
            item("Name", action(|_, _, _| {})).checked(true).disabled(true),
            col("Modified", Column::Modified, &self.settings),
            col("Type", Column::Type, &self.settings),
            col("Size", Column::Size, &self.settings),
            sep(),
            item("Reset column widths", action(|f, _, cx| {
                for c in [Column::Modified, Column::Type, Column::Size] {
                    f.settings.set_column_width(c, None);
                }
                cx.notify();
            })),
        ];
        self.open_menu(at, items, window, cx);
    }

    /// A sidebar place's or tab's items: open, new tab, paste, terminal, VS Code, copy path.
    pub fn place_items(&self, path: &str) -> Vec<MenuItem> {
        let (p1, p2, p3, p4, p5, p6) = (path.to_string(), path.to_string(), path.to_string(), path.to_string(), path.to_string(), path.to_string());
        let mut out = vec![
            item("Open", action(move |f, _, cx| f.go(p1.clone(), cx))).icon("folder-open"),
            item("Open in new tab", action(move |f, _, cx| f.new_tab(p2.clone(), cx))).icon("plus"),
        ];
        if !ferret_core::recycle::is_recycle(path) && ferret_core::search::search_query_of(path).is_none() {
            out.push(item("Paste into folder", action(move |f, _, cx| f.paste(p3.clone(), cx))).icon("clipboard-paste"));
            out.push(sep());
            out.push(item("Open in Terminal", action(move |f, _, cx| f.open_terminal(p4.clone(), cx))).icon("terminal"));
            out.push(item("Open in VS Code", action(move |f, _, cx| f.open_vscode(p5.clone(), cx))).icon("code-xml"));
            out.push(item("Copy as path", action(move |f, _, cx| f.copy_paths(std::slice::from_ref(&p6), true, cx))).icon("clipboard-copy"));
        }
        out
    }

    // ───────────────────────────── shell menus ─────────────────────────────

    /// Other apps' Explorer entries for these items, merged in above Properties (as they come).
    fn merge_shell_items(&mut self, paths: Vec<String>, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() || self.pane().is_bin() || paths.iter().any(|p| paths::is_remote(p) || paths::is_wsl(p) || ferret_core::archive::split_virtual(p).is_some() && !std::path::Path::new(p).exists()) {
            return;
        }
        let Some(hwnd) = crate::view::hwnd(window) else { return };
        let opened = self.menu.as_ref().map(|m| m.opened);
        let task = cx.background_executor().spawn(async move {
            ferret_core::rt::block_on(ferret_core::shellmenu::shell_menu_items(hwnd, paths, background))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(menu) = task.await else { return };
            this.update_in(cx, |f, window, cx| {
                if f.menu.as_ref().map(|m| m.opened) != opened || menu.items.is_empty() {
                    return;
                }
                let token = menu.token;
                let Some(m) = f.menu.as_mut() else { return };
                let level = &mut m.levels[0];
                let mut items = level.items.clone();
                let key = |l: &str| l.trim_end_matches('…').trim().to_lowercase();
                // Windows' "New ▸" adds its extra types (Office documents, shortcuts…) to Ferret's
                // own New menu (filled in when it opens: its loader then adds them after its own).
                let shell_new = menu.items.iter().find(|it| !it.separator && key(&it.label) == "new" && !it.children.is_empty());
                if let (Some(sn), Some(own)) = (shell_new, items.iter().position(|it| key(&it.label) == "new" && it.sub.is_some())) {
                    const OURS: [&str; 5] = ["folder", "text document", "markdown file", "json file", "empty file"];
                    let add: Vec<MenuItem> = sn.children.iter().filter(|c| !c.separator && !OURS.contains(&key(&c.label).as_str())).map(|c| shell_item(c, token)).collect();
                    if !add.is_empty() {
                        match items[own].sub.clone() {
                            Some(Sub::Lazy(load)) => {
                                items[own].sub = Some(Sub::Lazy(Rc::new(move |f: &mut Ferret, depth, cx: &mut Context<Ferret>| {
                                    load(f, depth, cx);
                                    f.append_sub(depth, add.clone(), cx);
                                })));
                            }
                            Some(Sub::Items(mut list)) => {
                                list.push(sep());
                                list.extend(add);
                                items[own].sub = Some(Sub::Items(list));
                            }
                            _ => {}
                        }
                    }
                }
                // Skip what Ferret's menu already has (e.g. "Open in Terminal").
                let mut own: std::collections::HashSet<String> = items.iter().map(|it| key(&it.label)).filter(|k| !k.is_empty()).collect();
                if let Some(open_in) = items.iter().position(|it| it.label.as_ref() == "Open in") {
                    if let Some(Sub::Items(kids)) = &items[open_in].sub {
                        for k in kids {
                            own.insert(format!("open in {}", key(&k.label)));
                        }
                    }
                    // An app's own "Open with Code" stands in for Ferret's VS Code item.
                    let code = menu.items.iter().any(|it| {
                        let k = key(&it.label);
                        ["open with code", "open in code", "open with vs code", "open in vs code"].contains(&k.as_str())
                    });
                    if code {
                        if let Some(Sub::Items(kids)) = items[open_in].sub.as_mut() {
                            kids.retain(|x| x.label.as_ref() != "VS Code");
                        }
                    }
                }
                let kept: Vec<&ferret_core::shellmenu::ShellMenuItem> = menu
                    .items
                    .iter()
                    .filter(|it| !shell_new.is_some_and(|sn| std::ptr::eq(*it, sn)) && (it.separator || !own.contains(&key(&it.label))))
                    .collect();
                // App launchers ("Open with Zed", "Edit in Notepad", at most 4) and cascades (7-Zip,
                // Send to) stay in view; the rest of the apps' one-off verbs share "More actions".
                let launcher = |it: &ferret_core::shellmenu::ShellMenuItem| {
                    let l = it.label.trim().to_lowercase();
                    !it.separator && it.children.is_empty() && (l.starts_with("open ") || l.starts_with("edit ") || l == "open" || l == "edit")
                };
                let launchers: Vec<*const ferret_core::shellmenu::ShellMenuItem> = kept.iter().filter(|it| launcher(it)).take(4).map(|it| *it as *const _).collect();
                let in_view = |it: &ferret_core::shellmenu::ShellMenuItem| it.separator || !it.children.is_empty() || launchers.contains(&(it as *const _));
                let others: Vec<&ferret_core::shellmenu::ShellMenuItem> = kept.iter().copied().filter(|it| !in_view(it)).collect();
                let grouped = others.len() >= 2;
                let mut extra: Vec<MenuItem> = kept.iter().filter(|it| !grouped || in_view(it)).map(|it| shell_item(it, token)).collect();
                if grouped {
                    extra.push(sep());
                    extra.push(submenu("More actions", others.iter().map(|it| shell_item(it, token)).collect()).icon("blocks"));
                }
                // Above "Delete permanently" or Properties (and the separator before it).
                let at = items.iter().position(|i| matches!(i.label.as_ref(), "Delete permanently" | "Properties"));
                let at = match at {
                    Some(a) if a > 0 && items[a - 1].kind == Kind2::Separator => a - 1,
                    Some(a) => a,
                    None => items.len(),
                };
                let mut merged = items[..at].to_vec();
                if !extra.is_empty() {
                    merged.push(sep());
                    merged.extend(extra);
                }
                merged.extend(items[at..].iter().cloned());
                let pos = level.at;
                level.items = tidy(merged);
                let fitted = fit(pos, &level.items, window);
                level.at = fitted;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Explorer's own menu ("Show more options"), at the mouse.
    pub fn native_menu(&mut self, paths: Vec<String>, background: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hwnd) = crate::view::hwnd(window) else { return };
        let (sx, sy) = screen_point(window, window.mouse_position());
        let task = cx.background_executor().spawn(async move {
            if background {
                ferret_core::rt::block_on(ferret_core::shell::show_native_background_menu(hwnd, paths[0].clone(), sx, sy))
            } else {
                ferret_core::rt::block_on(ferret_core::shell::show_native_menu(hwnd, paths, sx, sy))
            }
        });
        cx.spawn(async move |this, cx| {
            let verb = task.await;
            this.update(cx, |f, cx| match verb {
                // Its Rename is Ferret's own.
                Ok(Some(v)) if v == "rename" => f.start_rename(cx),
                Ok(_) => {
                    let p = f.pane().path.clone();
                    f.reload_dir(&p, cx);
                }
                Err(e) => f.toast_error(e, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Open with ▸: the apps for this file (with their icons, the default marked), then "Choose another app…".
    fn load_open_with(&mut self, path: String, depth: usize, cx: &mut Context<Self>) {
        let p2 = path.clone();
        let task = cx.background_executor().spawn(async move { ferret_core::rt::block_on(ferret_core::shell::list_open_with(p2)) });
        cx.spawn(async move |this, cx| {
            let apps = task.await.unwrap_or_default();
            let icons: Vec<Option<Arc<Image>>> = apps
                .iter()
                .map(|a| a.icon.as_ref().and_then(|b64| decode_data_url(b64)).map(|png| Arc::new(Image::from_bytes(gpui::ImageFormat::Png, png))))
                .collect();
            this.update(cx, |f, cx| {
                let mut items: Vec<MenuItem> = apps
                    .into_iter()
                    .zip(icons)
                    .map(|(a, icon)| {
                        let (p, id) = (path.clone(), a.id.clone());
                        let label = if a.is_default { format!("{} (default)", a.name) } else { a.name.clone() };
                        item(label, action(move |f, _, cx| f.open_with(p.clone(), id.clone(), cx))).image(icon).icon("app-window")
                    })
                    .collect();
                items.push(sep());
                let p = path.clone();
                items.push(item("Choose another app…", action(move |f, window, cx| f.open_with_dialog(p.clone(), window, cx))));
                f.fill_sub_now(depth, items, cx);
            })
            .ok();
        })
        .detach();
    }

    /// New ▸: folder, text, Markdown, JSON, an empty file, and Windows' own new types.
    fn load_new_items(&mut self, dir: String, depth: usize, cx: &mut Context<Self>) {
        let (d1, d2, d3, d4) = (dir.clone(), dir.clone(), dir.clone(), dir.clone());
        let items = vec![
            item("Folder", action(|f, _, cx| f.new_folder(cx))).icon("folder-plus").keys("Ctrl+Shift+N"),
            sep(),
            item("Text document", action(move |f, _, cx| f.new_file(d1.clone(), "New text document.txt", None, cx))).icon("file-text"),
            item("Markdown file", action(move |f, _, cx| f.new_file(d2.clone(), "New document.md", None, cx))).icon("file-text"),
            item("JSON file", action(move |f, _, cx| f.new_file(d3.clone(), "New file.json", Some("{}\n".into()), cx))).icon("braces"),
            item("Empty file", action(move |f, _, cx| f.new_file(d4.clone(), "New file", None, cx))).icon("file"),
        ];
        self.fill_sub_now(depth, items, cx);
    }

    /// Adds items (after a separator) to a submenu that was just filled in.
    pub fn append_sub(&mut self, depth: usize, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_mut() else { return };
        let Some(level) = menu.levels.get_mut(depth) else { return };
        let mut all = std::mem::take(&mut level.items);
        all.push(sep());
        all.extend(items);
        level.items = tidy(all);
        cx.notify();
    }

    /// Like fill_sub, without a window at hand (the next frame places it).
    pub fn fill_sub_now(&mut self, depth: usize, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.as_mut() else { return };
        let Some(level) = menu.levels.get_mut(depth) else { return };
        level.items = if items.is_empty() { vec![item("Nothing here", action(|_, _, _| {})).disabled(true)] } else { tidy(items) };
        cx.notify();
    }
}

fn loading_item() -> MenuItem {
    item("Loading…", action(|_, _, _| {})).disabled(true).icon("loader-circle")
}

/// No separators at the ends or twice in a row.
fn tidy(items: Vec<MenuItem>) -> Vec<MenuItem> {
    let mut out: Vec<MenuItem> = Vec::new();
    for i in items {
        if i.kind == Kind2::Separator && out.last().is_none_or(|l| l.kind == Kind2::Separator) {
            continue;
        }
        out.push(i);
    }
    while out.last().is_some_and(|l| l.kind == Kind2::Separator) {
        out.pop();
    }
    out
}

/// Keeps a menu inside the window.
fn fit(at: Point<Pixels>, items: &[MenuItem], window: &Window) -> Point<Pixels> {
    let vw = window.viewport_size();
    let h: f32 = items.iter().map(height_of).sum::<f32>() + 12.;
    let x = at.x.min(px((vw.width.as_f32() - WIDTH - 6.).max(6.)));
    let y = if (at.y + px(h)).as_f32() > vw.height.as_f32() - 6. { (at.y - px(h)).max(px(6.)) } else { at.y };
    point(x, y)
}

/// A point in the window, on the screen (physical pixels), for Windows' own menus.
pub fn screen_point(window: &Window, at: Point<Pixels>) -> (i32, i32) {
    let b = window.bounds();
    let s = window.scale_factor() as f64;
    ((((b.origin.x + at.x).as_f32() as f64) * s) as i32, (((b.origin.y + at.y).as_f32() as f64) * s) as i32)
}

fn shell_item(i: &ferret_core::shellmenu::ShellMenuItem, token: u64) -> MenuItem {
    if i.separator {
        return sep();
    }
    let icon = i.icon.as_ref().and_then(|s| decode_data_url(s)).map(|png| Arc::new(Image::from_bytes(gpui::ImageFormat::Png, png)));
    if !i.children.is_empty() {
        let kids: Vec<MenuItem> = i.children.iter().map(|c| shell_item(c, token)).collect();
        return submenu(i.label.clone(), kids).image(icon).disabled(i.disabled);
    }
    let id = i.id.clone();
    item(i.label.clone(), action(move |f, window, cx| f.invoke_shell_item(token, id.clone(), window, cx))).image(icon).disabled(i.disabled)
}

/// `data:image/png;base64,…` → PNG bytes.
pub fn decode_data_url(s: &str) -> Option<Vec<u8>> {
    let b64 = s.split_once("base64,").map(|(_, b)| b).unwrap_or(s);
    base64_decode(b64)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}
