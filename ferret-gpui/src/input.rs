//! A text field (one line, or wrapping: `wrapping()`): typing (with IME composition), selection by mouse and keyboard, clipboard.
//! GPUI has no text field of its own; this is adapted from GPUI's `examples/input.rs` (Zed
//! Industries, Apache-2.0), styled for Ferret and telling its owner when the text changes.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextRun,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme as t;

actions!(text_input, [Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy]);

/// Keys the field handles while it has focus.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("TextInput");
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
        KeyBinding::new("ctrl-v", Paste, ctx),
        KeyBinding::new("ctrl-c", Copy, ctx),
        KeyBinding::new("ctrl-x", Cut, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
    ]);
}

/// The text changed.
pub struct Changed;

pub struct TextInput {
    focus_handle: FocusHandle,
    pub content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// How far the text is scrolled left (a long name keeps the cursor in view).
    scroll_x: Pixels,
    is_selecting: bool,
    /// Text size and line height (px).
    size: f32,
    line: f32,
    /// Long text wraps onto more lines (the grid's rename box) instead of scrolling sideways.
    wrap: bool,
    last_wrapped: Option<WrappedLine>,
    /// A password: shown as dots, never copied out, its value hidden from screen readers.
    secret: bool,
    /// What a screen reader calls the field, when its placeholder doesn't say (rename, a form's field).
    name: Option<SharedString>,
    /// Backspace at the start and Delete at the end (nothing to delete) go on to the field's
    /// owner as plain keys: Ctrl+K's Backspace drops the type chip, Delete forgets a recent search.
    pass_idle_edits: bool,
}

/// A password's dot, for each character (3 bytes in UTF-8: offsets map by character count).
const DOT: &str = "•";

impl EventEmitter<Changed> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
            size: 16.,
            line: 24.,
            wrap: false,
            last_wrapped: None,
            secret: false,
            name: None,
            pass_idle_edits: false,
        }
    }

    /// Backspace and Delete with nothing to delete reach the owner's key handler (`pass_idle_edits`).
    pub fn passes_idle_edits(mut self) -> Self {
        self.pass_idle_edits = true;
        self
    }

    /// A password field (`secret`): dots instead of the text, no copying it out.
    pub fn masked(mut self, secret: bool) -> Self {
        self.secret = secret;
        self
    }

    /// The field's name to a screen reader (its placeholder otherwise).
    pub fn named(mut self, name: impl Into<SharedString>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The text as drawn: the dots of a password.
    fn shown(&self) -> SharedString {
        if self.secret { DOT.repeat(self.content.chars().count()).into() } else { self.content.clone() }
    }

    /// A text offset as an offset in the drawn text (they differ for a password's dots).
    fn to_shown(&self, offset: usize) -> usize {
        if self.secret { self.content[..offset.min(self.content.len())].chars().count() * DOT.len() } else { offset }
    }

    /// An offset in the drawn text back to one in the text.
    fn from_shown(&self, offset: usize) -> usize {
        if !self.secret {
            return offset;
        }
        let n = offset / DOT.len();
        self.content.char_indices().nth(n).map_or(self.content.len(), |(i, _)| i)
    }

    /// Text size and line height, for fields smaller than the palette's.
    pub fn sized(mut self, size: f32, line: f32) -> Self {
        self.size = size;
        self.line = line;
        self
    }

    /// Wraps long text onto more lines, as the app's grid rename box (a textarea) does.
    pub fn wrapping(mut self) -> Self {
        self.wrap = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Selects a byte range of the text (rename selects the name without its extension).
    pub fn select(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let len = self.content.len();
        self.selected_range = range.start.min(len)..range.end.min(len);
        self.selection_reversed = false;
        cx.notify();
    }

    pub fn select_everything(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    /// The selection (the caret when empty), as byte offsets.
    pub fn selection(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    /// What the empty field says (Ctrl+K's changes with its scope and mode).
    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        if self.placeholder != text {
            self.placeholder = text;
            cx.notify();
        }
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string().into();
        self.selected_range = text.len()..text.len();
        self.marked_range = None;
        cx.emit(Changed);
        cx.notify();
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), window, cx);
        } else {
            self.move_to(self.selected_range.start, window, cx)
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), window, cx);
        } else {
            self.move_to(self.selected_range.end, window, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx)
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                if self.pass_idle_edits {
                    cx.propagate();
                }
                return;
            }
            self.select_to(prev, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                if self.pass_idle_edits {
                    cx.propagate();
                }
                return;
            }
            self.select_to(next, window, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), window, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if self.wrap {
            let (Some(bounds), Some(wrapped)) = (self.last_bounds.as_ref(), self.last_wrapped.as_ref()) else { return 0 };
            let local = point((position.x - bounds.left()).max(px(0.)), (position.y - bounds.top()).clamp(px(0.), (bounds.size.height - px(1.)).max(px(0.))));
            return wrapped.closest_index_for_position(local, px(self.line)).unwrap_or_else(|i| i).min(self.content.len());
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else { return 0 };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.from_shown(line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn set_selected_text_range(&mut self, range_utf16: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        let start16 = self.offset_to_utf16(range.start);
        let len16 = new_text.encode_utf16().count();
        self.selected_range = new_selected_range_utf16
            .map(|s| self.range_from_utf16(&(start16 + s.start.min(len16)..start16 + s.end.min(len16))))
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.selection_reversed = false;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.wrap {
            // The IME's candidate window goes by the range's start.
            let wrapped = self.last_wrapped.as_ref()?;
            let range = self.range_from_utf16(&range_utf16);
            let at = wrapped.position_for_index(range.start, px(self.line)).unwrap_or_default();
            return Some(Bounds::new(bounds.origin + at, size(px(2.), px(self.line))));
        }
        let line = self.last_layout.as_ref()?;
        if line.text != self.shown() {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(self.to_shown(range.start)) - self.scroll_x, bounds.top()),
            point(bounds.left() + line.x_for_index(self.to_shown(range.end)) - self.scroll_x, bounds.bottom()),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx).into_iter().collect()
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        if self.wrap {
            return Some(self.offset_to_utf16(self.index_for_mouse_position(p)));
        }
        let local = self.last_bounds?.localize(&p)?;
        let line = self.last_layout.as_ref()?;
        if line.text != self.shown() {
            return None;
        }
        Some(self.offset_to_utf16(self.from_shown(line.closest_index_for_x(local.x + self.scroll_x))))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    wrapped: Option<WrappedLine>,
    scroll: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

/// The display text and its runs (the placeholder when empty; IME text underlined).
fn display_runs(input: &TextInput, style: &gpui::TextStyle) -> (SharedString, Vec<TextRun>) {
    let content = input.shown();
    let (display, color) = if content.is_empty() { (input.placeholder.clone(), t::fg3()) } else { (content, style.color) };
    let run = TextRun { len: display.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
    let marked = input.marked_range.as_ref().filter(|_| !input.content.is_empty()).map(|r| input.to_shown(r.start)..input.to_shown(r.end));
    let runs = if let Some(marked) = marked.as_ref() {
        vec![
            TextRun { len: marked.start, ..run.clone() },
            TextRun {
                len: marked.end - marked.start,
                underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }),
                ..run.clone()
            },
            TextRun { len: display.len() - marked.end, ..run },
        ]
        .into_iter()
        .filter(|r| r.len > 0)
        .collect()
    } else {
        vec![run]
    };
    (display, runs)
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let input = self.input.read(cx);
        if !input.wrap {
            style.size.height = window.line_height().into();
            return (window.request_layout(style, [], cx), ());
        }
        // Wrapping: as tall as the lines the text takes at the width it's given.
        let text_style = window.text_style();
        let (display, runs) = display_runs(input, &text_style);
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let layout = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.unwrap_or(match available.width {
                gpui::AvailableSpace::Definite(w) => w,
                _ => px(10_000.),
            });
            let lines = window.text_system().shape_text(display.clone(), font_size, &runs, Some(width), None).map(|l| l.iter().map(|w| w.size(line_height).height).sum::<Pixels>()).unwrap_or(line_height);
            size(width, lines.max(line_height))
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.to_shown(input.selected_range.start)..input.to_shown(input.selected_range.end);
        let cursor = input.to_shown(input.cursor_offset());
        let style = window.text_style();
        let (display, runs) = display_runs(input, &style);
        let font_size = style.font_size.to_pixels(window.rem_size());
        if input.wrap {
            // Wrapped: the cursor and selection follow the lines; nothing scrolls.
            let lh = window.line_height();
            let Some(wrapped) = window.text_system().shape_text(display, font_size, &runs, Some(bounds.size.width), None).ok().and_then(|l| l.into_iter().next()) else {
                return PrepaintState { line: None, wrapped: None, scroll: px(0.), cursor: None, selection: Vec::new() };
            };
            let at = |i: usize| bounds.origin + wrapped.position_for_index(i, lh).unwrap_or_else(|| point(wrapped.width(), wrapped.size(lh).height - lh));
            let mut selection = Vec::new();
            let mut caret = None;
            if selected_range.is_empty() {
                caret = Some(fill(Bounds::new(at(cursor), size(px(2.), lh)), t::accent()));
            } else {
                let (a, b) = (at(selected_range.start), at(selected_range.end));
                let shade = t::accent_a(0.55);
                if a.y == b.y {
                    selection.push(fill(Bounds::from_corners(a, point(b.x, b.y + lh)), shade));
                } else {
                    selection.push(fill(Bounds::from_corners(a, point(bounds.right(), a.y + lh)), shade));
                    if b.y > a.y + lh {
                        selection.push(fill(Bounds::from_corners(point(bounds.left(), a.y + lh), point(bounds.right(), b.y)), shade));
                    }
                    selection.push(fill(Bounds::from_corners(point(bounds.left(), b.y), point(b.x, b.y + lh)), shade));
                }
            }
            return PrepaintState { line: None, wrapped: Some(wrapped), scroll: px(0.), cursor: caret, selection };
        }
        let line = window.text_system().shape_line(display, font_size, &runs, None);
        // Scrolled so the cursor stays in view; none when it all fits.
        let width = bounds.size.width;
        let cursor_x = line.x_for_index(cursor);
        let mut scroll = input.scroll_x;
        if cursor_x - scroll > width - px(4.) {
            scroll = cursor_x - width + px(4.);
        }
        if cursor_x < scroll {
            scroll = cursor_x;
        }
        scroll = scroll.min((line.width - width + px(4.)).max(px(0.))).max(px(0.));
        let left = bounds.left() - scroll;
        let (selection, cursor) = if selected_range.is_empty() {
            (Vec::new(), Some(fill(Bounds::new(point(left + cursor_x, bounds.top()), size(px(2.), bounds.bottom() - bounds.top())), t::accent())))
        } else {
            (
                vec![fill(
                    Bounds::from_corners(
                        point(left + line.x_for_index(selected_range.start), bounds.top()),
                        point(left + line.x_for_index(selected_range.end), bounds.bottom()),
                    ),
                    t::accent_a(0.55),
                )],
                None,
            )
        };
        PrepaintState { line: Some(line), wrapped: None, scroll, cursor, selection }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        if let Some(wrapped) = prepaint.wrapped.take() {
            let lh = window.line_height();
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            let _ = wrapped.paint(bounds.origin, lh, gpui::TextAlign::Left, None, window, cx);
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
            self.input.update(cx, |input, _| {
                input.last_wrapped = Some(wrapped);
                input.last_bounds = Some(bounds);
            });
            return;
        }
        let Some(line) = prepaint.line.take() else { return };
        let scroll = prepaint.scroll;
        // Clipped to the field: a long name doesn't run out of its box.
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selection in prepaint.selection.drain(..) {
                window.paint_quad(selection)
            }
            line.paint(point(bounds.left() - scroll, bounds.top()), window.line_height(), gpui::TextAlign::Left, None, window, cx).unwrap();
            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });
        self.input.update(cx, |input, _| {
            input.scroll_x = scroll;
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // To a screen reader: an edit field named by its placeholder, holding the text as one
        // run, with the caret and selection in it (so moving through it is read out).
        let text = self.shown().to_string();
        let lens: Vec<u8> = text.chars().map(|c| c.len_utf8() as u8).collect();
        let at = |b: usize| self.content[..b.min(self.content.len())].chars().count();
        let (anchor, head) = if self.selection_reversed {
            (self.selected_range.end, self.selected_range.start)
        } else {
            (self.selected_range.start, self.selected_range.end)
        };
        let (anchor, head) = (at(anchor), at(head));
        let run_text = text.clone();
        div()
            .id("text-input")
            .role(if self.secret { gpui::Role::PasswordInput } else { gpui::Role::TextInput })
            .aria_label(self.name.clone().unwrap_or_else(|| self.placeholder.clone()))
            .aria_value(text)
            .a11y_synthetic_children(move |b| {
                let mut run = gpui::accesskit::Node::new(gpui::Role::TextRun);
                run.set_value(run_text);
                run.set_character_lengths(lens);
                let id = b.synthetic_node_id(0);
                b.push_child(id, run);
                let pos = |i: usize| gpui::accesskit::TextPosition { node: id, character_index: i };
                b.parent_node().set_text_selection(gpui::accesskit::TextSelection { anchor: pos(anchor), focus: pos(head) });
            })
            .flex()
            .flex_1()
            .min_w_0()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .line_height(px(self.line))
            .text_size(px(self.size))
            .text_color(t::fg())
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Gives a field the keyboard.
pub fn focus(input: &Entity<TextInput>, window: &mut Window, cx: &mut App) {
    let handle = input.read(cx).focus_handle.clone();
    window.focus(&handle, cx);
}
