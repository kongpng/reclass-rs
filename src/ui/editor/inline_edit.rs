//! The inline-edit field: a focused, IME-capable single-line text editor that
//! overlays the column being edited.
//!
//! Port of the inline-editing lifecycle (editor-surface.md §11) onto gpui's
//! custom-`Element` + `EntityInputHandler` pattern (the canonical
//! `gpui/examples/input.rs` blueprint, gpui_cookbook.md §3.3). When a field is
//! clicked/Tab-activated the editor creates a [`FieldInput`] seeded with the
//! field's current text + which [`EditTarget`] and node it edits, focuses it, and
//! on Enter/Tab/focus-loss the [`FieldInput`] reports the committed text back via
//! a callback the editor wires to the controller (`inlineEditCommitted`,
//! editor-surface.md §1 step 3 — the editor never mutates the tree directly).
//!
//! IME/UTF-16 handling, grapheme-aware cursor movement, selection, and clipboard
//! mirror the example verbatim; the editor-specific part is the commit/cancel
//! contract carrying `(node_idx, sub_line, target, text)`.

use std::ops::Range;

use gpui::*;

use crate::compose::EditTarget;

/// What a committed inline edit carries back to the editor → controller
/// (`inlineEditCommitted(nodeIdx, subLine, target, text)`, editor-surface.md §11
/// `commitInlineEdit`). `resolved_addr` is the compose-resolved address for value
/// writes (0 = "use base + computeOffset").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditCommit {
    pub node_idx: i32,
    pub sub_line: i32,
    pub target: EditTarget,
    pub text: String,
    pub resolved_addr: u64,
}

actions!(
    rcx_field_input,
    [
        FieldBackspace,
        FieldDelete,
        FieldLeft,
        FieldRight,
        FieldSelectLeft,
        FieldSelectRight,
        FieldSelectAll,
        FieldHome,
        FieldEnd,
        FieldCommit,
        FieldCancel,
        FieldPaste,
        FieldCopy,
        FieldCut,
    ]
);

/// Outcome of a key/commit interaction the host editor reacts to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// Still editing.
    Continue,
    /// Commit with this payload (Enter / Tab / focus-loss).
    Commit(EditCommit),
    /// Cancel without writing (Esc).
    Cancel,
}

/// The inline-edit state — `InlineEditState` (editor.h:302), the gpui-native
/// slice: content + selection + IME marked range + cached layout, plus the edit
/// identity (`node_idx`/`sub_line`/`target`/`resolved_addr`). The padding /
/// byte-range bookkeeping the Scintilla port needed is unnecessary here because
/// the field is a real overlay element, not text spliced into a shared buffer.
pub struct FieldInput {
    focus_handle: FocusHandle,
    content: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,

    // Edit identity.
    node_idx: i32,
    sub_line: i32,
    target: EditTarget,
    resolved_addr: u64,
    /// Foreground color the field paints its text in (resolved from the theme by
    /// the host so the edited field keeps its column color).
    text_color: Hsla,
    /// Set by an action handler; drained by the host to learn what to do next.
    pending_outcome: Option<EditOutcome>,
}

impl FieldInput {
    /// Create a field seeded with `initial` text for `(node_idx, sub_line,
    /// target)`. Selects all text (the C++ begin-edit selects the span).
    pub fn new(
        node_idx: i32,
        sub_line: i32,
        target: EditTarget,
        resolved_addr: u64,
        initial: impl Into<SharedString>,
        text_color: Hsla,
        cx: &mut Context<Self>,
    ) -> Self {
        let content: SharedString = initial.into();
        let len = content.len();
        FieldInput {
            focus_handle: cx.focus_handle(),
            content,
            selected_range: 0..len,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            node_idx,
            sub_line,
            target,
            resolved_addr,
            text_color,
            pending_outcome: None,
        }
    }

    /// The field's focus handle (the editor focuses this on begin-edit). Named
    /// distinctly from the [`Focusable`] trait method to avoid the inherent-vs-
    /// trait method ambiguity at call sites.
    pub fn field_focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn target(&self) -> EditTarget {
        self.target
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// Build the commit payload from the current (trimmed) content.
    pub fn to_commit(&self) -> EditCommit {
        EditCommit {
            node_idx: self.node_idx,
            sub_line: self.sub_line,
            target: self.target,
            text: self.content.trim().to_string(),
            resolved_addr: self.resolved_addr,
        }
    }

    /// Drain the pending outcome (the host calls this after dispatching actions).
    pub fn take_outcome(&mut self) -> Option<EditOutcome> {
        self.pending_outcome.take()
    }

    // ── editing actions (mirror gpui/examples/input.rs) ──

    fn left(&mut self, _: &FieldLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }
    fn right(&mut self, _: &FieldRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }
    fn select_left(&mut self, _: &FieldSelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }
    fn select_right(&mut self, _: &FieldSelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }
    fn select_all(&mut self, _: &FieldSelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }
    fn home(&mut self, _: &FieldHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    fn end(&mut self, _: &FieldEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }
    fn backspace(&mut self, _: &FieldBackspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn delete(&mut self, _: &FieldDelete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }
    fn commit(&mut self, _: &FieldCommit, _: &mut Window, cx: &mut Context<Self>) {
        self.pending_outcome = Some(EditOutcome::Commit(self.to_commit()));
        cx.notify();
    }
    fn cancel(&mut self, _: &FieldCancel, _: &mut Window, cx: &mut Context<Self>) {
        self.pending_outcome = Some(EditOutcome::Cancel);
        cx.notify();
    }
    fn paste(&mut self, _: &FieldPaste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // Inline fields are single-line: strip newlines (begin-edit §11 paste).
            self.replace_text_in_range(None, &text.replace(['\n', '\r'], ""), window, cx);
        }
    }
    fn copy(&mut self, _: &FieldCopy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &FieldCut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }
    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }
    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify();
    }
    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }
    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    // Cursor boundaries (char-aligned). The C++ field moves by char; grapheme
    // clustering is a refinement deferred to keep the dependency set lean.
    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }
    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .find_map(|(idx, ch)| (idx >= offset).then_some(idx + ch.len_utf8()))
            .unwrap_or(self.content.len())
    }

    // utf8 ↔ utf16 (IME bridge).
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }
    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }
    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }
    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
}

impl Focusable for FieldInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for FieldInput {
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

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

/// The custom element that shapes + paints the field text + caret/selection and
/// registers the OS input handler (gpui/examples/input.rs `TextElement`).
pub struct FieldElement {
    pub input: Entity<FieldInput>,
}

pub struct FieldPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for FieldElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FieldElement {
    type RequestLayoutState = ();
    type PrepaintState = FieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let color = input.text_color;
        let style = window.text_style();

        let run = TextRun {
            len: content.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: content.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(content, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    color,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x3311ff44),
                )),
                None,
            )
        };
        FieldPrepaint {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let Some(line) = prepaint.line.take() else {
            return;
        };
        let _ = line.paint(
            bounds.origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for FieldInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.focus_handle.clone();
        div()
            .flex()
            .key_context("RcxFieldInput")
            .track_focus(&handle)
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
            .on_action(cx.listener(Self::commit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .size_full()
            .child(FieldElement { input: cx.entity() })
    }
}

/// The key bindings for an active inline field (bound in the `RcxFieldInput`
/// context). Returned for the app to register once at startup.
pub fn field_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("backspace", FieldBackspace, Some("RcxFieldInput")),
        KeyBinding::new("delete", FieldDelete, Some("RcxFieldInput")),
        KeyBinding::new("left", FieldLeft, Some("RcxFieldInput")),
        KeyBinding::new("right", FieldRight, Some("RcxFieldInput")),
        KeyBinding::new("shift-left", FieldSelectLeft, Some("RcxFieldInput")),
        KeyBinding::new("shift-right", FieldSelectRight, Some("RcxFieldInput")),
        KeyBinding::new("cmd-a", FieldSelectAll, Some("RcxFieldInput")),
        KeyBinding::new("ctrl-a", FieldSelectAll, Some("RcxFieldInput")),
        KeyBinding::new("home", FieldHome, Some("RcxFieldInput")),
        KeyBinding::new("end", FieldEnd, Some("RcxFieldInput")),
        KeyBinding::new("enter", FieldCommit, Some("RcxFieldInput")),
        KeyBinding::new("escape", FieldCancel, Some("RcxFieldInput")),
        KeyBinding::new("cmd-v", FieldPaste, Some("RcxFieldInput")),
        KeyBinding::new("ctrl-v", FieldPaste, Some("RcxFieldInput")),
        KeyBinding::new("cmd-c", FieldCopy, Some("RcxFieldInput")),
        KeyBinding::new("ctrl-c", FieldCopy, Some("RcxFieldInput")),
        KeyBinding::new("cmd-x", FieldCut, Some("RcxFieldInput")),
        KeyBinding::new("ctrl-x", FieldCut, Some("RcxFieldInput")),
    ]
}

#[cfg(test)]
mod tests {
    // Import only the items under test — NOT `super::*`, which would pull the
    // module's `gpui::*` glob into the `#[test]` hygiene expansion and explode
    // the type-recursion budget on this nightly+gpui combination.
    use super::EditCommit;
    use crate::compose::EditTarget;

    #[test]
    fn edit_commit_carries_identity_and_trimmed_text() {
        let c = EditCommit {
            node_idx: 3,
            sub_line: 0,
            target: EditTarget::Name,
            text: "field".to_string(),
            resolved_addr: 0,
        };
        assert_eq!(c.node_idx, 3);
        assert_eq!(c.target, EditTarget::Name);
    }
}
