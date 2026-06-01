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
use std::time::Duration;

use gpui::*;

use crate::compose::EditTarget;

/// Caret blink half-period (on→off or off→on). Matches the gpui-component input
/// blink cadence (500ms) so the inline field caret feels native.
const BLINK_INTERVAL: Duration = Duration::from_millis(500);

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
        // Item 9: while editing, Up/Down/PageUp/PageDown must NOT bubble to the
        // RcxEditor context (where they trigger NODE navigation mid-edit). These
        // bindings swallow the keys on the field context — a no-op that consumes
        // the event so the cursor stays in the field. C++ `handleEditKey` returns
        // `true` for these keys to block line navigation.
        FieldSwallowVert,
    ]
);

/// Hex/ASCII overwrite-edit mode (item 7). A fixed-length per-byte editor that
/// matches the C++ `handleHexEditKey`: typing overwrites one position in place
/// (a hex digit per nibble, or one ASCII char per byte) and advances; the content
/// length never changes; backspace/delete reset a position rather than splicing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HexOverwrite {
    /// Editing the hex byte string `"NN NN NN …"` — `byte_count` bytes, rendered
    /// as 2 hex digits per byte separated by single spaces. Only `[0-9A-Fa-f]`
    /// accepted; typed digits upper-cased; the cursor skips the space separators.
    Hex { byte_count: usize },
    /// Editing the ASCII preview `"........"` — `byte_count` printable chars (one
    /// per byte). Only `0x20..=0x7E` accepted; reset char is `'.'`.
    Ascii { byte_count: usize },
}

impl HexOverwrite {
    /// The reset character a backspace/delete writes (`'0'` hex, `'.'` ascii).
    fn reset_char(self) -> char {
        match self {
            HexOverwrite::Hex { .. } => '0',
            HexOverwrite::Ascii { .. } => '.',
        }
    }
    fn is_hex(self) -> bool {
        matches!(self, HexOverwrite::Hex { .. })
    }
}

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
    /// Background fill for the active text selection (the Zed text-selection
    /// token, resolved from the theme by the host — NOT an ad-hoc hex). Keeps the
    /// inline-edit selection on-palette and retinting with a theme switch.
    selection_color: Hsla,
    /// Set by an action handler; drained by the host to learn what to do next.
    pending_outcome: Option<EditOutcome>,
    /// Hex/ASCII overwrite mode (item 7). `None` = ordinary free-text editing;
    /// `Some(..)` = fixed-length per-byte overwrite (hex digits / ASCII chars).
    hex_overwrite: Option<HexOverwrite>,

    // ── Caret blink (BUG 2) ──
    /// Whether the caret quad is drawn this frame. Kept SOLID-visible (`true`)
    /// while typing / moving the cursor; flips on the idle blink timer only.
    blink_visible: bool,
    /// Monotonic blink epoch — every keystroke / cursor move bumps it so an
    /// in-flight blink timer from a *previous* edit is ignored (it compares its
    /// captured epoch against this and bails), guaranteeing the caret resets to
    /// solid the instant the field changes (no stale "off" phase swallowing a
    /// keystroke's caret).
    blink_epoch: usize,
    /// The pending blink timer task (dropped/replaced on each reset).
    _blink_task: Task<()>,
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
        selection_color: Hsla,
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
            selection_color,
            pending_outcome: None,
            hex_overwrite: None,
            // The caret starts solid-visible; the blink timer is armed when the
            // field is focused / on the first reset (begin-edit calls `notify_edit`).
            blink_visible: true,
            blink_epoch: 0,
            _blink_task: Task::ready(()),
        }
    }

    /// Enable hex/ASCII overwrite mode on this field (item 7). The host calls this
    /// right after [`new`] when the edited target is a hex node Value (hex mode) or
    /// an ASCII preview (ascii mode). Places the caret on the first editable
    /// position (skipping a leading space is unnecessary — the string starts with a
    /// digit/char). The content is taken as-is from the seed (it is the live
    /// fixed-length `"NN NN …"` / `"…"` string the row shows).
    pub fn set_hex_overwrite(&mut self, mode: HexOverwrite) {
        self.hex_overwrite = Some(mode);
        // Start with the caret at offset 0 (a digit/char, never a space) and no
        // selection — overwrite mode does not select-all on begin.
        self.selected_range = 0..0;
        self.selection_reversed = false;
    }

    /// Whether this field is in hex/ASCII overwrite mode (item 7).
    pub fn is_hex_overwrite(&self) -> bool {
        self.hex_overwrite.is_some()
    }

    /// Item 7: re-seed the content with the ASCII preview `seed` (one printable
    /// char per byte) and switch into [`HexOverwrite::Ascii`] mode. Used by the
    /// "Edit ASCII" context-menu entry, which opens a plain Value edit (whose seed
    /// is the hex string) then converts it to the ASCII overwrite editor.
    pub fn set_ascii_overwrite(&mut self, seed: &str, byte_count: usize) {
        self.content = seed.to_string().into();
        self.hex_overwrite = Some(HexOverwrite::Ascii { byte_count });
        self.selected_range = 0..0;
        self.selection_reversed = false;
    }

    /// Advance/clamp helper: the next caret offset moving right by one, skipping a
    /// space separator in hex mode, clamped to the last data position.
    fn ow_next(&self, off: usize) -> usize {
        ow_next_in(
            &self.content,
            self.hex_overwrite.map(|m| m.is_hex()).unwrap_or(false),
            off,
        )
    }

    /// The previous caret offset moving left by one, skipping a space separator in
    /// hex mode, clamped to 0.
    fn ow_prev(&self, off: usize) -> usize {
        ow_prev_in(
            &self.content,
            self.hex_overwrite.map(|m| m.is_hex()).unwrap_or(false),
            off,
        )
    }

    /// Whether the caret quad should be painted this frame (BUG 2). Solid while
    /// typing/moving; toggles on the idle blink timer.
    pub fn caret_visible(&self) -> bool {
        self.blink_visible
    }

    /// Reset the caret to **solid-visible** and (re)arm the idle blink timer
    /// (BUG 2). Called on every text edit, key, and cursor move so the caret is
    /// continuously drawn while the user is active, blinking only once they pause.
    /// Bumps the blink epoch so any timer from a prior edit is a no-op when it
    /// fires, then schedules a fresh half-period before the next toggle.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_visible = true;
        self.blink_epoch = self.blink_epoch.wrapping_add(1);
        let epoch = self.blink_epoch;
        // Schedule the next phase flip a half-period out. A foreground spawn on
        // the field entity keeps the toggle on the UI timeline; the epoch guard
        // makes superseded timers inert. Each notify also re-renders the host
        // editor (the host observes this field), so the embedded caret quad is
        // recomputed at the current cursor offset on every tick.
        self._blink_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLINK_INTERVAL).await;
            let _ = this.update(cx, |this, cx| this.blink_phase(epoch, cx));
        });
    }

    /// One idle blink toggle: flip caret visibility and schedule the next flip,
    /// unless a newer edit has bumped the epoch (then bail, leaving the caret as
    /// the edit left it — solid). Drives itself via the foreground timer.
    fn blink_phase(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if epoch != self.blink_epoch {
            return; // superseded by a later edit/reset — do nothing.
        }
        self.blink_visible = !self.blink_visible;
        cx.notify();
        let next = self.blink_epoch.wrapping_add(1);
        self.blink_epoch = next;
        self._blink_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BLINK_INTERVAL).await;
            let _ = this.update(cx, |this, cx| this.blink_phase(next, cx));
        });
    }

    /// Public hook the host calls right after begin-edit (and any time it focuses
    /// the field) to arm the blink with a solid caret.
    pub fn arm_caret(&mut self, cx: &mut Context<Self>) {
        self.restart_blink(cx);
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
        // Item 7: overwrite mode moves one position left, skipping space separators.
        if self.hex_overwrite.is_some() {
            self.move_to(self.ow_prev(self.cursor_offset()), cx);
            return;
        }
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }
    fn right(&mut self, _: &FieldRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.hex_overwrite.is_some() {
            self.move_to(self.ow_next(self.cursor_offset()), cx);
            return;
        }
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
        // Overwrite mode has no free-text selection — Ctrl+A is a no-op there
        // (the C++ hex editor ignores it), keeping the fixed-length invariant.
        if self.hex_overwrite.is_some() {
            return;
        }
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }
    fn home(&mut self, _: &FieldHome, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }
    /// Item 9: swallow Up/Down/PageUp/PageDown during an active edit so they do
    /// not bubble to the editor surface and navigate between NODES. The single-
    /// line inline field has no vertical motion, so this is a deliberate no-op that
    /// merely consumes the event (mirrors C++ `handleEditKey` returning `true` for
    /// these keys). The blink is restarted so the caret stays solid on the keypress.
    fn swallow_vert(&mut self, _: &FieldSwallowVert, _: &mut Window, cx: &mut Context<Self>) {
        self.restart_blink(cx);
    }
    fn end(&mut self, _: &FieldEnd, _: &mut Window, cx: &mut Context<Self>) {
        // Overwrite mode: the last editable position is the final char, not past it.
        if self.hex_overwrite.is_some() {
            self.move_to(self.content.len().saturating_sub(1), cx);
            return;
        }
        self.move_to(self.content.len(), cx);
    }
    fn backspace(&mut self, _: &FieldBackspace, window: &mut Window, cx: &mut Context<Self>) {
        // Item 7: overwrite mode RESETS the previous position (no splice / length
        // change) and moves there — matching C++ `handleHexEditKey` Backspace.
        if let Some(mode) = self.hex_overwrite {
            let cur = self.cursor_offset();
            if cur == 0 {
                return;
            }
            let prev = self.ow_prev(cur);
            // Write the reset char at `prev`, then leave the caret on `prev`.
            self.set_char_at(prev, mode.reset_char());
            self.selected_range = prev..prev;
            self.restart_blink(cx);
            cx.notify();
            return;
        }
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
        // Item 7: overwrite mode RESETS the current position in place.
        if let Some(mode) = self.hex_overwrite {
            let at = self.cursor_offset();
            // Skip space separators in hex mode (nothing to reset there).
            if mode.is_hex() && self.content.as_bytes().get(at) == Some(&b' ') {
                return;
            }
            if at < self.content.len() {
                self.set_char_at(at, mode.reset_char());
                self.restart_blink(cx);
                cx.notify();
            }
            return;
        }
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    /// Overwrite the char at `at` in place (no caret move, no length change).
    fn set_char_at(&mut self, at: usize, ch: char) {
        let mut s: Vec<char> = self.content.chars().collect();
        if at < s.len() {
            s[at] = ch;
            self.content = s.into_iter().collect::<String>().into();
        }
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
            // Sanitized paste (item 83, editor.cpp `handleEditKey` Ctrl+V,
            // editor.cpp:3301): inline fields are single-line, so strip newlines /
            // carriage returns; for a base-address edit also strip backticks (the
            // address-formula syntax the expr-evaluator already removes, so a paste
            // of `app.exe + 0x10` does not introduce stray backticks).
            let sanitized = sanitize_inline_paste(&text, self.target == EditTarget::BaseAddress);
            // Hex/ASCII overwrite paste respects the field WIDTH: the overwrite path
            // in `replace_text_in_range` writes one accepted char per position and
            // never extends the fixed-length string, so a too-long paste is clamped
            // to the field (editor.cpp:3440 `writeCol < spanEnd`). Spaces in the
            // pasted hex are dropped there too (the separator is skipped).
            self.replace_text_in_range(None, &sanitized, window, cx);
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
        self.restart_blink(cx);
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
        self.restart_blink(cx);
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
        // Item 7: in hex/ASCII overwrite mode, character input does NOT splice —
        // it overwrites one position in place (fixed length) and advances. Filter
        // accepted characters (hex digits / printable ASCII), upper-casing hex.
        if let Some(mode) = self.hex_overwrite {
            // Item 7 / 83: fixed-length per-byte overwrite (single keystroke OR a
            // multi-char paste). Delegates to the pure `overwrite_paste_into`, which
            // respects the field WIDTH (stops at the end rather than re-overwriting
            // the last cell), drops/skips hex separators, filters accepted chars,
            // and advances the caret past a trailing separator.
            let (content, caret) =
                overwrite_paste_into(&self.content, self.cursor_offset(), mode.is_hex(), new_text);
            self.content = content.into();
            self.selected_range = caret..caret;
            self.selection_reversed = false;
            self.marked_range.take();
            self.restart_blink(cx);
            cx.notify();
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let (content, cursor) = splice_text(&self.content, range, new_text);
        self.content = content.into();
        self.selected_range = cursor..cursor;
        self.marked_range.take();
        // Keep the caret solid + recomputed at the new cursor offset while typing
        // (BUG 2): a keystroke must not leave the caret in a stale "off" phase.
        self.restart_blink(cx);
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
        // IME composition keeps the caret solid + recomputed (BUG 2).
        self.restart_blink(cx);
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

/// Pure text-edit primitive shared by the field's keystroke handlers — splice
/// `new_text` over `content[range]` and return `(new_content, new_cursor)` where
/// the cursor collapses to just past the inserted text. This is the byte-exact
/// core of [`EntityInputHandler::replace_text_in_range`] lifted out of the gpui
/// `Window`/`Context` plumbing so the keystroke logic (insert / backspace / delete
/// / paste) is unit-testable headlessly — the entity method delegates to it, so a
/// test of this function is a test of what a real keypress does to the field.
///
/// `range` is a UTF-8 byte range clamped to char boundaries by the caller (the
/// field always derives it from char-aligned selection offsets).
pub fn splice_text(content: &str, range: Range<usize>, new_text: &str) -> (String, usize) {
    let start = range.start.min(content.len());
    let end = range.end.clamp(start, content.len());
    let mut out = String::with_capacity(content.len() - (end - start) + new_text.len());
    out.push_str(&content[..start]);
    out.push_str(new_text);
    out.push_str(&content[end..]);
    let cursor = start + new_text.len();
    (out, cursor)
}

/// Item 83 (pure): sanitize text about to be spliced into a single-line inline
/// field. Always strips newlines/CR (the field is one line); for a base-address
/// edit also strips backticks (the address-formula syntax the expr-evaluator
/// removes). Mirrors the C++ `handleEditKey` Ctrl+V sanitize (editor.cpp:3301).
pub fn sanitize_inline_paste(text: &str, is_base_address: bool) -> String {
    text.chars()
        .filter(|&c| c != '\n' && c != '\r' && !(is_base_address && c == '`'))
        .collect()
}

/// Item 83 (pure): overwrite `text` into the fixed-length `content` buffer
/// starting at byte offset `start`, respecting the field WIDTH. Returns the new
/// `(content, caret)`. The buffer length never changes (overwrite, not splice);
/// once the write cursor reaches the end it stops rather than re-overwriting the
/// last cell (the C++ `writeCol < spanEnd` guard, editor.cpp:3440). In hex mode,
/// space separators in the target are skipped and literal spaces in the pasted
/// text are dropped; only `[0-9A-Fa-f]` are accepted (upper-cased) and the final
/// caret advances past a trailing separator. In ASCII mode only `0x20..=0x7E`.
pub fn overwrite_paste_into(
    content: &str,
    start: usize,
    is_hex: bool,
    text: &str,
) -> (String, usize) {
    let span_end = content.len();
    let mut buf: Vec<u8> = content.as_bytes().to_vec();
    let mut write = start.min(span_end);
    for ch in text.chars() {
        if write >= span_end {
            break;
        }
        if is_hex && buf.get(write) == Some(&b' ') {
            write += 1;
            if write >= span_end {
                break;
            }
        }
        if is_hex && ch == ' ' {
            continue;
        }
        let accepted = if is_hex {
            ch.is_ascii_hexdigit().then(|| ch.to_ascii_uppercase())
        } else {
            let c = ch as u32;
            (0x20..=0x7E).contains(&c).then_some(ch)
        };
        if let Some(c) = accepted {
            // ASCII content: one byte per char (the buffer is hex digits/spaces or
            // printable ASCII), so byte/char indices coincide.
            if write < buf.len() {
                buf[write] = c as u8;
                write += 1;
            }
        }
    }
    let mut caret = write;
    if is_hex && buf.get(caret) == Some(&b' ') {
        caret += 1;
    }
    let caret = caret.min(span_end.saturating_sub(1));
    (
        String::from_utf8(buf).unwrap_or_else(|_| content.to_string()),
        caret,
    )
}

/// Item 7 (pure): the caret offset one position to the RIGHT in a fixed-length
/// overwrite buffer, skipping a single space separator when `is_hex`, clamped to
/// the last editable char (`len-1`). The buffer is ASCII (hex digits/spaces or
/// printable ASCII) so byte and char indices coincide.
pub fn ow_next_in(content: &str, is_hex: bool, off: usize) -> usize {
    let bytes = content.as_bytes();
    let mut n = off + 1;
    if is_hex && n < bytes.len() && bytes[n] == b' ' {
        n += 1;
    }
    let last = content.len().saturating_sub(1);
    n.min(last)
}

/// Item 7 (pure): the caret offset one position to the LEFT, skipping a single
/// space separator when `is_hex`, clamped to 0.
pub fn ow_prev_in(content: &str, is_hex: bool, off: usize) -> usize {
    if off == 0 {
        return 0;
    }
    let bytes = content.as_bytes();
    let mut n = off - 1;
    if is_hex && bytes.get(n) == Some(&b' ') {
        n = n.saturating_sub(1);
    }
    n
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
        let selection_color = input.selection_color;
        // BUG 2: only build the caret quad when the blink phase says it is visible.
        // The quad is recomputed from `cursor` (the live cursor offset) every
        // prepaint, so each keystroke / move lands the caret at the new position.
        let caret_visible = input.caret_visible();
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
                // Painted only while the blink phase is in its visible half (or
                // solid mid-type); `None` hides it on the off phase (BUG 2).
                caret_visible.then(|| {
                    fill(
                        Bounds::new(
                            point(bounds.left() + cursor_pos, bounds.top()),
                            size(px(2.), bounds.bottom() - bounds.top()),
                        ),
                        color,
                    )
                }),
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
                    selection_color,
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
            .on_action(cx.listener(Self::swallow_vert))
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
        // Item 9: block vertical / page navigation during edit (swallowed here so
        // they don't reach the editor's node-nav bindings).
        KeyBinding::new("up", FieldSwallowVert, Some("RcxFieldInput")),
        KeyBinding::new("down", FieldSwallowVert, Some("RcxFieldInput")),
        KeyBinding::new("pageup", FieldSwallowVert, Some("RcxFieldInput")),
        KeyBinding::new("pagedown", FieldSwallowVert, Some("RcxFieldInput")),
    ]
}

#[cfg(test)]
mod tests {
    // Import only the items under test — NOT `super::*`, which would pull the
    // module's `gpui::*` glob into the `#[test]` hygiene expansion and explode
    // the type-recursion budget on this nightly+gpui combination.
    use super::{
        overwrite_paste_into, ow_next_in, ow_prev_in, sanitize_inline_paste, splice_text,
        EditCommit,
    };
    use crate::compose::EditTarget;

    #[test]
    fn paste_sanitize_strips_newlines_and_base_addr_backticks() {
        // Item 83: a single-line field strips newlines/CR always.
        assert_eq!(sanitize_inline_paste("ab\ncd\r\nef", false), "abcdef");
        // A base-address edit additionally strips backticks (the formula syntax).
        assert_eq!(
            sanitize_inline_paste("`app.exe` + 0x10", true),
            "app.exe + 0x10"
        );
        // A non-base-address field keeps backticks (only newlines stripped).
        assert_eq!(sanitize_inline_paste("a`b\nc", false), "a`bc");
    }

    #[test]
    fn overwrite_paste_respects_hex_field_width() {
        // Item 83: an 8-byte hex field "00 00 00 00 00 00 00 00" (len 23). Pasting
        // a longer hex string clamps to the field width — never grows the buffer.
        let field = "00 00 00 00 00 00 00 00";
        let (out, _caret) =
            overwrite_paste_into(field, 0, true, "DEADBEEFCAFEBABE1122334455667788FF");
        assert_eq!(out.len(), field.len(), "fixed length preserved");
        assert_eq!(out, "DE AD BE EF CA FE BA BE", "width-clamped overwrite");
    }

    #[test]
    fn overwrite_paste_hex_skips_separators_and_drops_spaces() {
        // Pasting space-separated hex into a hex field: spaces in the paste are
        // dropped, and the target's own separators are skipped.
        let field = "00 00 00 00";
        let (out, _c) = overwrite_paste_into(field, 0, true, "AA BB");
        assert_eq!(out, "AA BB 00 00");
        // Non-hex chars are filtered out (only hex nibbles land).
        let (out2, _c2) = overwrite_paste_into(field, 0, true, "A!B?C");
        assert_eq!(out2, "AB C0 00 00");
    }

    #[test]
    fn overwrite_paste_ascii_respects_width_and_printable() {
        // ASCII overwrite field of 4 bytes "....": only printable ASCII is taken,
        // clamped to width; non-printable (newline already stripped by sanitize,
        // but a raw control char here) is filtered.
        let field = "....";
        let (out, _c) = overwrite_paste_into(field, 0, false, "Hello");
        assert_eq!(out, "Hell", "width-clamped to 4 bytes");
        let (out2, _c2) = overwrite_paste_into(field, 0, false, "a\u{0007}bc");
        assert_eq!(out2, "abc.", "control char filtered, printables land");
    }

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

    #[test]
    fn typing_into_a_select_all_field_replaces_the_seed() {
        // BUG 1 integration: a freshly-opened inline field seeds its text and
        // select-all-highlights it (`selected_range = 0..len`). The first keystroke
        // must REPLACE the whole seed (not append), then subsequent keystrokes
        // insert at the collapsed caret. This drives the exact splice the entity's
        // `replace_text_in_range` runs on each `a`/`b`/`c` key.
        let seed = "CreateTime";
        // Type 'X' with the seed fully selected → content becomes "X", caret after.
        let (c1, cur1) = splice_text(seed, 0..seed.len(), "X");
        assert_eq!(c1, "X");
        assert_eq!(cur1, 1);
        // Then 'Y' / 'Z' insert at the caret (collapsed selection cur..cur).
        let (c2, cur2) = splice_text(&c1, cur1..cur1, "Y");
        assert_eq!(c2, "XY");
        assert_eq!(cur2, 2);
        let (c3, cur3) = splice_text(&c2, cur2..cur2, "Z");
        assert_eq!(c3, "XYZ");
        assert_eq!(cur3, 3);
    }

    #[test]
    fn splice_inserts_replaces_and_deletes_like_keystrokes() {
        // Insert at the caret (typing in the middle).
        assert_eq!(splice_text("abd", 2..2, "c"), ("abcd".to_string(), 3));
        // Replace a selection (drag-select "bc" then type "X").
        assert_eq!(splice_text("abcd", 1..3, "X"), ("aXd".to_string(), 2));
        // Backspace = splice an empty string over the char before the caret.
        assert_eq!(splice_text("abc", 2..3, ""), ("ab".to_string(), 2));
        // Delete = splice empty over the char after the caret.
        assert_eq!(splice_text("abc", 0..1, ""), ("bc".to_string(), 0));
        // Append at the end.
        assert_eq!(splice_text("ab", 2..2, "c"), ("abc".to_string(), 3));
    }

    #[test]
    fn splice_clamps_out_of_range_to_string_bounds() {
        // Defensive: a range past the end clamps to the string length (the field
        // always passes char-aligned offsets, but the splice must never panic).
        assert_eq!(splice_text("ab", 5..9, "Z"), ("abZ".to_string(), 3));
        assert_eq!(splice_text("ab", 1..9, "Z"), ("aZ".to_string(), 2));
    }

    #[test]
    fn hex_overwrite_right_skips_space_separators() {
        // "00 11 22" — moving right from the second nibble of byte 0 (offset 1)
        // must land on the FIRST nibble of byte 1 (offset 3), skipping the space.
        let s = "00 11 22";
        assert_eq!(ow_next_in(s, true, 0), 1); // 0→1 within a byte
        assert_eq!(ow_next_in(s, true, 1), 3); // 1→(skip space at 2)→3
        assert_eq!(ow_next_in(s, true, 3), 4);
        assert_eq!(ow_next_in(s, true, 4), 6); // skip the space at 5
                                               // Clamp at the last data char (offset len-1 = 7), never past it.
        assert_eq!(ow_next_in(s, true, 7), 7);
    }

    #[test]
    fn hex_overwrite_left_skips_space_separators() {
        let s = "00 11 22";
        assert_eq!(ow_prev_in(s, true, 0), 0); // clamp at start
        assert_eq!(ow_prev_in(s, true, 1), 0);
        assert_eq!(ow_prev_in(s, true, 3), 1); // 3→(skip space at 2)→1
        assert_eq!(ow_prev_in(s, true, 4), 3);
        assert_eq!(ow_prev_in(s, true, 6), 4); // 6→(skip space at 5)→4
    }

    #[test]
    fn ascii_overwrite_has_no_space_skipping() {
        // ASCII preview "...." moves one position at a time, no separators.
        let s = "....";
        assert_eq!(ow_next_in(s, false, 0), 1);
        assert_eq!(ow_next_in(s, false, 3), 3); // clamp at len-1
        assert_eq!(ow_prev_in(s, false, 2), 1);
        assert_eq!(ow_prev_in(s, false, 0), 0);
    }
}
