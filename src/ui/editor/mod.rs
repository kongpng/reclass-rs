//! The bespoke structured-editor surface — a custom raw-gpui view + `Element`.
//!
//! Faithful port of `src/editor.{h,cpp}` (the QScintilla-backed grid;
//! editor-surface.md). gpui-component has no equivalent (confirmed,
//! ARCHITECTURE.md §5), so this is hand-rolled on raw gpui per the cookbook
//! (gpui_cookbook.md §3): a **virtualized** [`uniform_list`] of styled monospace
//! rows produced by [`compose`](crate::compose), with per-span coloring
//! (the indicator passes → [`geometry::style_runs`]), inline-editable column
//! regions resolved by hit-testing ([`hit_test`]), Tab-cycling between fields
//! ([`tab_cycle`]), node multi-select + cross-row selection (forwarded to the
//! controller's `handle_node_click`), fold/expand of structs/arrays, per-byte
//! hex selection + heat overlays ([`selection`], [`element`]), and inline edit
//! through an [`EntityInputHandler`](inline_edit::FieldInput).
//!
//! ## Architecture
//! The view **owns** the [`RcxController`](crate::controller::RcxController) (the
//! engine) and renders its latest [`ComposeResult`]. It never mutates the
//! [`NodeTree`](crate::core::NodeTree) directly: clicks/edits are translated into
//! controller calls (`handle_node_click`, `toggle_collapse`, `change_node_kind`,
//! `rename_node`, `set_node_value`, `apply_type_text`, `undo`/`redo`), after which
//! the view re-`refresh`es and re-renders (editor-surface.md §1: "the editor
//! itself never mutates the NodeTree"). All pure view logic (row→span layout,
//! hit-test math, selection model, tab order) lives in the sibling modules and is
//! unit-tested headlessly; this file is the gpui glue.

pub mod element;
pub mod geometry;
pub mod hit_test;
pub mod inline_edit;
pub mod minimap;
pub mod palette;
pub mod selection;
pub mod tab_cycle;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, IconName, WindowExt as _};

use crate::compose::EditTarget;
use crate::controller::{Modifiers as CtrlMods, RcxController, RcxDocument};
use crate::core::linemeta::K_COMMAND_ROW_ID;
use crate::core::{is_hex_preview, ComposeResult, LineKind, LineMeta, NodeKind};
use crate::ui::findbar::{FindBar, FindEvent};
use crate::ui::sourcechooser::{SourceChooserEvent, SourceChooserPopup};
use crate::ui::{design, tooltip};

use element::{RowElement, RowPaint};
use geometry::CellMetrics;
use inline_edit::{EditCommit, EditOutcome, FieldInput};
use palette::EditorPalette;
use selection::ByteSelection;

// Editor surface key actions (editor-surface.md §10 `handleNormalKey`). Bound in
// the `RcxEditor` key context; the data-mutating ones route through the
// controller. The full key vocabulary (type shortcuts, byte-selection nav, node
// move/duplicate) is layered on in later UI workflows; this stage wires the
// load-bearing editor keys: Tab-cycle, Esc, undo/redo.
actions!(
    rcx_editor,
    [
        EditorTab,
        EditorTabPrev,
        EditorEscape,
        EditorUndo,
        EditorRedo,
        // Node context-menu / accelerator actions (the C++ node right-click menu,
        // reclass_right_click_on_address.png). Each dispatches against the focused
        // `RcxEditor` and acts on the current context-target node (the row that was
        // right-clicked, else the primary-selected row). The PopupMenu items below
        // are wired to these same actions so menu-click and keyboard agree.
        EditorNewClass,
        EditorPtrToNewClass,
        EditorCycleTypePrev,
        EditorCycleTypeNext,
        EditorRename,
        EditorChangeType,
        EditorInsertBelow,
        EditorInsertAbove,
        EditorConvertPtr,
        // Item 6 / B4: command-row keyword right-click → convert the root class
        // keyword struct↔class (the C++ `keywordConvertRequested`).
        EditorConvertRootToStruct,
        EditorConvertRootToClass,
        EditorToggleBigEndian,
        EditorDuplicate,
        EditorDelete,
        EditorFold,
        EditorCopyCStruct,
        // Find bar (Ctrl+F) — mounts the `FindBar` over the editor (items 4/91/92).
        EditorFind,
        // Normal-mode quick keys (editor.cpp `handleNormalKey`, item 12). Quick
        // type changes (P/F/S/U), hex sizing (Space / 1-5), node navigation
        // (Up/Down/PageUp/Down/Home/End), value edit (Enter), insert (Insert), and
        // comment edit (`;`).
        EditorQuickPointer,
        EditorQuickFloat,
        EditorQuickSigned,
        EditorQuickUnsigned,
        EditorHexCycleNext,
        EditorHexCyclePrev,
        EditorHex8,
        EditorHex16,
        EditorHex32,
        EditorHex64,
        EditorHex128,
        EditorNavUp,
        EditorNavDown,
        // Item 12: Ctrl+Up / Ctrl+Down — navigate to the next node AND toggle it
        // into the multi-selection (the C++ additive nav; nodeClicked with the
        // ControlModifier set). Distinct from plain Up/Down (REPLACE) and
        // Ctrl+Shift+Up/Down (reorder).
        EditorNavAddUp,
        EditorNavAddDown,
        // Ctrl+Shift+Up/Down — reorder the active field among its siblings (the
        // C++ `moveNodeRequested`). Distinct from plain Up/Down navigation.
        EditorMoveUp,
        EditorMoveDown,
        EditorNavPageUp,
        EditorNavPageDown,
        EditorNavHome,
        EditorNavEnd,
        EditorBeginValueEdit,
        EditorInsertHex64,
        EditorInsertHex32,
        EditorCommentEdit,
        EditorCycleLeft,
        EditorCycleRight,
        EditorGoToDefinition,
        // Collapse-all / expand-all (item 19).
        EditorCollapseAll,
        EditorExpandAll,
        // Shift+Left / Shift+Right — collapse / expand the current foldable node
        // (a nested class/struct instance, a pointer-to-class, an array of
        // structs). Plain Left/Right stay type-cycle (C++ parity), so the fold
        // keys take the Shift chord to avoid clobbering it.
        EditorCollapseNode,
        EditorExpandNode,
        // Shift+Up/Down — extend the node multi-selection (range-select, the C++
        // `nodeClicked(.., ShiftModifier)` keyboard path). Distinct from plain
        // Up/Down (which REPLACE the selection) and Ctrl+Shift+Up/Down (reorder).
        EditorSelectUp,
        EditorSelectDown,
        // Shift+PageUp/Down/Home/End — extend the selection to the page/document
        // bound (the modified-nav variants; editor.cpp handleNormalKey passes the
        // live modifiers into nodeClicked for PageUp/Down/Home/End too).
        EditorSelectPageUp,
        EditorSelectPageDown,
        EditorSelectHome,
        EditorSelectEnd,
        // Ctrl+A — select all sibling nodes of the current node (editor.cpp
        // `Key_A` + ControlModifier → range-select first..last).
        EditorSelectAll,
        // Ctrl+C / Ctrl+X / Ctrl+V — node-level clipboard (copy/cut/paste the
        // selected nodes as a portable `rcx-clipboard/v1` blob). The editor-surface
        // ctrl-c/x/v (the field-input ones are scoped to RcxFieldInput).
        EditorCopyNodes,
        EditorCutNodes,
        EditorPasteNodes,
        // Ctrl+Shift+C — copy the current node's address (`0x{addr:X}`) as text.
        EditorCopyAddress,
        // Item 27: offset/address-margin right-click → checkable Relative (+0x) /
        // Absolute address mode.
        EditorOffsetsRelative,
        EditorOffsetsAbsolute,
        // Item 9: the Convert submenu quick-converts (per-size int/uint/float,
        // ptr/fnptr, Split to hexN, Convert to Hex). Each sets the target kind on
        // the context-target node via `change_node_kind` (or `split_hex_node`).
        EditorConvUInt,
        EditorConvInt,
        EditorConvFloat,
        EditorConvPtr64,
        EditorConvPtr32,
        EditorConvFnPtr64,
        EditorConvFnPtr32,
        EditorConvHex,
        EditorConvSplitHex,
        // Item 7: open the in-place hex byte / ASCII overwrite editor on the
        // context-target hex node's value.
        EditorEditBytesHex,
        EditorEditBytesAscii,
        // Item 17: Copy submenu — address / offset / line / all-as-text.
        EditorCopyOffset,
        EditorCopyLine,
        EditorCopyAllText,
        // Gap 20: the Tracking submenu — toggle live value-change tracking, and
        // clear all recorded change history.
        EditorTrackToggle,
        EditorTrackClear,
        // Item 17: Append bytes… — append a single field at the end of the
        // view-root struct (the C++ Insert-submenu tail / no-node menu row). Falls
        // back to the view root when there is no current node.
        EditorAppendBytes,
        // Item 44: editor text zoom (Ctrl+=/Ctrl+-/Ctrl+0), the QScintilla
        // Ctrl+wheel zoom analogue.
        EditorZoomIn,
        EditorZoomOut,
        EditorZoomReset,
        // Item 6: enum / bitfield MEMBER row context-menu actions (Add Member
        // Above/Below, Remove Member, Toggle Bit). They act on the context target's
        // node_id + sub_line (the member index).
        EditorMemberAddAbove,
        EditorMemberAddBelow,
        EditorMemberRemove,
        EditorMemberToggleBit,
        // Item 7/18/48: group the multi-selection into a Union (controller
        // `group_into_union`).
        EditorGroupIntoUnion,
        // Item 8: the Static submenu actions — add a Hex64 child / a static field,
        // edit the static expression, or dissolve a union member.
        EditorStaticAddChild,
        EditorStaticAddField,
        EditorStaticEditExpr,
        EditorStaticDissolveUnion,
        // Item 11: the no-node (empty-area) menu's "Add Static Field" — adds a
        // static field to the current VIEW ROOT struct/array (the C++ `!hasNode`
        // branch's `insertStaticField(rootId)`, controller.cpp:3904). Distinct from
        // `EditorStaticAddField`, which targets the right-clicked node.
        EditorRootAddStaticField,
        // Item 11: type-inference quick-convert (the C++ `Convert to <type>` /
        // `Split into <type>xN`). The suggested kind(s) are stashed in
        // `pending_hint_convert` when the menu opens (a parameterless gpui action
        // can't carry the dynamic kind), and these read it back.
        EditorHintConvert,
        EditorHintSplit,
    ]
);

/// Default monospace cell width as a fraction of the row height (overwritten with
/// the measured glyph advance once the first frame shapes a line). A 0.6 ratio is
/// the typical width:height of a monospace cell and keeps hit-testing sane before
/// the first measurement.
const DEFAULT_CELL_RATIO: f32 = 0.6;

/// Editor line-height multiple (× font size). Item 24: the local `1.5` const that
/// shadowed the canonical token is deleted; the editor now uses the single
/// canonical [`design::tokens::font::EDITOR_LINE_HEIGHT`] (~1.4) so the grid's
/// leading matches the C++ (12px cap + small extraAscent/Descent) instead of the
/// looser 1.5 it read at. Both the painted rows and the hit-test metrics derive
/// from this one source.
const EDITOR_LINE_HEIGHT: f32 = design::tokens::font::EDITOR_LINE_HEIGHT;

/// The key bindings for the editor surface (bound in the `RcxEditor` context).
/// Returned so the app can register them once at startup alongside the inline
/// field bindings ([`inline_edit::field_key_bindings`]).
pub fn editor_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", EditorTab, Some("RcxEditor")),
        KeyBinding::new("shift-tab", EditorTabPrev, Some("RcxEditor")),
        KeyBinding::new("escape", EditorEscape, Some("RcxEditor")),
        KeyBinding::new("cmd-z", EditorUndo, Some("RcxEditor")),
        KeyBinding::new("ctrl-z", EditorUndo, Some("RcxEditor")),
        KeyBinding::new("cmd-shift-z", EditorRedo, Some("RcxEditor")),
        KeyBinding::new("ctrl-shift-z", EditorRedo, Some("RcxEditor")),
        KeyBinding::new("ctrl-y", EditorRedo, Some("RcxEditor")),
        // Node context-menu accelerators (the C++ menu's right-aligned hints:
        // Rename=F2, Change Type=T, Duplicate=Ctrl+D, Delete=Delete).
        KeyBinding::new("f2", EditorRename, Some("RcxEditor")),
        KeyBinding::new("t", EditorChangeType, Some("RcxEditor")),
        KeyBinding::new("cmd-d", EditorDuplicate, Some("RcxEditor")),
        KeyBinding::new("ctrl-d", EditorDuplicate, Some("RcxEditor")),
        KeyBinding::new("delete", EditorDelete, Some("RcxEditor")),
        // Find bar (Ctrl+F / Cmd+F).
        KeyBinding::new("ctrl-f", EditorFind, Some("RcxEditor")),
        KeyBinding::new("cmd-f", EditorFind, Some("RcxEditor")),
        // Normal-mode quick keys (item 12). The letter keys are bare (no modifier)
        // to match the C++ `handleNormalKey` accelerators.
        KeyBinding::new("p", EditorQuickPointer, Some("RcxEditor")),
        KeyBinding::new("f", EditorQuickFloat, Some("RcxEditor")),
        KeyBinding::new("s", EditorQuickSigned, Some("RcxEditor")),
        KeyBinding::new("u", EditorQuickUnsigned, Some("RcxEditor")),
        KeyBinding::new("space", EditorHexCycleNext, Some("RcxEditor")),
        KeyBinding::new("shift-space", EditorHexCyclePrev, Some("RcxEditor")),
        KeyBinding::new("1", EditorHex8, Some("RcxEditor")),
        KeyBinding::new("2", EditorHex16, Some("RcxEditor")),
        KeyBinding::new("3", EditorHex32, Some("RcxEditor")),
        KeyBinding::new("4", EditorHex64, Some("RcxEditor")),
        KeyBinding::new("5", EditorHex128, Some("RcxEditor")),
        KeyBinding::new("up", EditorNavUp, Some("RcxEditor")),
        KeyBinding::new("down", EditorNavDown, Some("RcxEditor")),
        // Item 12: Ctrl+Up / Ctrl+Down — additive nav (move caret to the next node
        // and toggle it into the selection). gpui exact-modifier match means the
        // bare `up`/`down` above never fire for the ctrl chord, so these are
        // required for the chord to work at all.
        KeyBinding::new("ctrl-up", EditorNavAddUp, Some("RcxEditor")),
        KeyBinding::new("ctrl-down", EditorNavAddDown, Some("RcxEditor")),
        KeyBinding::new("cmd-up", EditorNavAddUp, Some("RcxEditor")),
        KeyBinding::new("cmd-down", EditorNavAddDown, Some("RcxEditor")),
        // Ctrl+Shift+Up/Down reorder the field; gpui matches this more-specific
        // chord over the bare `up`/`down` navigation bindings above.
        KeyBinding::new("ctrl-shift-up", EditorMoveUp, Some("RcxEditor")),
        KeyBinding::new("ctrl-shift-down", EditorMoveDown, Some("RcxEditor")),
        KeyBinding::new("pageup", EditorNavPageUp, Some("RcxEditor")),
        KeyBinding::new("pagedown", EditorNavPageDown, Some("RcxEditor")),
        KeyBinding::new("home", EditorNavHome, Some("RcxEditor")),
        KeyBinding::new("end", EditorNavEnd, Some("RcxEditor")),
        KeyBinding::new("enter", EditorBeginValueEdit, Some("RcxEditor")),
        KeyBinding::new("insert", EditorInsertHex64, Some("RcxEditor")),
        KeyBinding::new("shift-insert", EditorInsertHex32, Some("RcxEditor")),
        KeyBinding::new("semicolon", EditorCommentEdit, Some("RcxEditor")),
        // Left/Right cycle same-size type variants (item 18).
        KeyBinding::new("left", EditorCycleLeft, Some("RcxEditor")),
        KeyBinding::new("right", EditorCycleRight, Some("RcxEditor")),
        // Shift+Left / Shift+Right collapse / expand the current foldable node
        // (nested class/struct instance, pointer-to-class, array of structs).
        // The inline-edit field binds shift-left/right in its deeper
        // `RcxFieldInput` context (text selection), so it shadows these while
        // editing — same pattern as up/down. Plain Left/Right stay type-cycle.
        KeyBinding::new("shift-left", EditorCollapseNode, Some("RcxEditor")),
        KeyBinding::new("shift-right", EditorExpandNode, Some("RcxEditor")),
        // F12 Go To Definition (item 20).
        KeyBinding::new("f12", EditorGoToDefinition, Some("RcxEditor")),
        // Collapse-all / expand-all (item 19).
        KeyBinding::new("ctrl-shift-[", EditorCollapseAll, Some("RcxEditor")),
        KeyBinding::new("ctrl-shift-]", EditorExpandAll, Some("RcxEditor")),
        // Item 44: editor text zoom (Ctrl+=/Ctrl++/Ctrl+-/Ctrl+0).
        KeyBinding::new("ctrl-=", EditorZoomIn, Some("RcxEditor")),
        KeyBinding::new("ctrl-+", EditorZoomIn, Some("RcxEditor")),
        KeyBinding::new("cmd-=", EditorZoomIn, Some("RcxEditor")),
        KeyBinding::new("ctrl--", EditorZoomOut, Some("RcxEditor")),
        KeyBinding::new("cmd--", EditorZoomOut, Some("RcxEditor")),
        KeyBinding::new("ctrl-0", EditorZoomReset, Some("RcxEditor")),
        KeyBinding::new("cmd-0", EditorZoomReset, Some("RcxEditor")),
        // Shift+arrow / Shift+page / Shift+Home/End — extend the node selection.
        // The more-specific shift chords take priority over the plain nav
        // bindings above (gpui longest-modifier-match), and over `ctrl-shift-up`
        // (reorder) since that adds ctrl.
        KeyBinding::new("shift-up", EditorSelectUp, Some("RcxEditor")),
        KeyBinding::new("shift-down", EditorSelectDown, Some("RcxEditor")),
        KeyBinding::new("shift-pageup", EditorSelectPageUp, Some("RcxEditor")),
        KeyBinding::new("shift-pagedown", EditorSelectPageDown, Some("RcxEditor")),
        KeyBinding::new("shift-home", EditorSelectHome, Some("RcxEditor")),
        KeyBinding::new("shift-end", EditorSelectEnd, Some("RcxEditor")),
        // Ctrl+A select-all siblings.
        KeyBinding::new("ctrl-a", EditorSelectAll, Some("RcxEditor")),
        KeyBinding::new("cmd-a", EditorSelectAll, Some("RcxEditor")),
        // Node clipboard (copy/cut/paste). Ctrl+Shift+C (copy address) is bound
        // BEFORE ctrl-c so the more-specific chord matches first.
        KeyBinding::new("ctrl-shift-c", EditorCopyAddress, Some("RcxEditor")),
        KeyBinding::new("cmd-shift-c", EditorCopyAddress, Some("RcxEditor")),
        KeyBinding::new("ctrl-c", EditorCopyNodes, Some("RcxEditor")),
        KeyBinding::new("cmd-c", EditorCopyNodes, Some("RcxEditor")),
        KeyBinding::new("ctrl-x", EditorCutNodes, Some("RcxEditor")),
        KeyBinding::new("cmd-x", EditorCutNodes, Some("RcxEditor")),
        KeyBinding::new("ctrl-v", EditorPasteNodes, Some("RcxEditor")),
        KeyBinding::new("cmd-v", EditorPasteNodes, Some("RcxEditor")),
    ]
}

/// Events the editor surface emits up to its host (the window/tab shell). The C++
/// `RcxEditor` raised Qt signals the `MainWindow` connected to; gpui's
/// [`EventEmitter`] is the analogue. The host subscribes via `cx.subscribe`.
#[derive(Clone, Debug)]
pub enum RcxEditorEvent {
    /// Ctrl+Click on a navigable type/name token requests opening that node's
    /// referenced struct (`ref_id`) in a NEW editor tab (item 11, the C++
    /// `openTypeInNewTabRequested`). The host creates the tab + sets its view root.
    OpenTypeInNewTab { ref_id: u64 },
    /// A transient app-status message the editor wants surfaced (the C++
    /// `setAppStatus(...)`, e.g. "Copied C struct to clipboard"). The host
    /// status bar reads this; if unconsumed it is harmless.
    Status { message: String },
    /// Item 12: an in-editor View-option toggle the user flipped from WITHIN the
    /// editor surface (the offset-margin double-click or the right-click
    /// Relative/Absolute actions), which must propagate like the menu toggle: the
    /// host persists the setting, sets the View-menu checkmark, and pushes the value
    /// to every open editor / split pane (the C++ `setRelativeOffsets` emits
    /// `relativeOffsetsChanged`, editor.cpp:2754). The editor has already applied
    /// the value locally; this event asks the host to mirror it everywhere.
    ViewOptionToggled {
        option: EditorViewOption,
        value: bool,
    },
}

/// Item 12: the editor-originated View options that can be toggled from within the
/// editor surface and must propagate to the host (currently only Relative Offsets;
/// kept as an enum so further in-editor toggles can join without a new event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorViewOption {
    RelativeOffsets,
}

/// The bespoke editor surface view.
///
/// Holds the engine [`RcxController`] plus the purely-visual state the C++
/// `RcxEditor` owned (editor-surface.md §1): the active inline-edit field, the
/// byte selection, the hovered line, the last Tab target, and the cached cell
/// metrics for hit-testing.
pub struct RcxEditor {
    controller: RcxController,
    /// The active inline-edit field overlay, if editing.
    editing: Option<EditingField>,
    /// The per-byte hex selection (address-based, survives refresh; §12).
    byte_sel: ByteSelection,
    /// The line the mouse is hovering (for the hover-row background; §7).
    hovered_line: Option<usize>,
    /// Item 9: the NODE id under the pointer (the C++ `m_hoveredNodeId`). The hover
    /// band lights every display line whose `node_id` matches this (so a multi-line
    /// header/value highlights as a unit), and is suppressed when the node is
    /// already selected. 0 = no hovered node. Cleared on viewport-leave / kbd nav.
    hovered_node_id: u64,
    /// The open hover popup (item 13), if the cursor is over a qualifying value
    /// column (heated value / func-or-void pointer / typed pointer). Cleared when
    /// the cursor leaves the value region or moves to a non-qualifying row.
    hover_popup: Option<HoverPopupState>,
    /// The moving end of a keyboard range-selection (Shift+arrows/page/home/end).
    /// The C++ tracks the Scintilla caret line; here we mirror it so Shift-nav
    /// extends from the last caret position rather than from `first_selected_line`
    /// (which would otherwise collapse the moving end to the lowest selected row).
    /// Reset to the landed line on any plain (non-shift) navigation/click.
    caret_line: Option<usize>,
    /// The row a left-button drag started on (set on row mouse-down), and whether
    /// that press landed on the hex byte grid. When the drag did NOT start on the
    /// byte grid, a drag onto another row range-selects the NODES between the
    /// anchor and the current row (item 8) instead of extending the byte
    /// selection. `None` clears on mouse-up / the next non-drag interaction.
    drag_anchor_line: Option<usize>,
    drag_on_byte_grid: bool,
    /// The text-local X (px) the drag press landed on, and whether the ~8px drag
    /// threshold has been crossed yet (item 28). A button-held move within the
    /// dead-zone does NOT start extending the node/byte selection; only once the
    /// pointer travels past `DRAG_DEAD_ZONE_PX` from the anchor does drag-select
    /// begin (then `drag_started` latches so the rest of the drag extends freely).
    drag_anchor_x: f32,
    drag_started: bool,
    /// Item 4: the char-column of the most recent value click, consumed by the next
    /// `begin_inline_edit` to narrow a Vec2/3/4/Mat4x4 Value edit to the clicked
    /// comma-separated component. `None` ⇒ no narrowing (keyboard-initiated edit /
    /// non-vector node).
    pending_click_col: Option<i32>,
    /// Item 20: the modifiers held when the drag press landed (the C++
    /// `m_dragInitMods`). A Ctrl+drag must ADD the dragged range to the selection
    /// rather than replace it, so Mode-2 drag-select ORs in `ctrl: drag_init_mods`.
    drag_init_mods: Modifiers,
    /// Item 20: a deferred plain click on an already-selected node within a >1
    /// selection (the C++ `m_pendingClickNodeId`/`Line`/`Mods`). The selection
    /// collapse is postponed to mouse-RELEASE so a drag from the group keeps the
    /// whole group; on a plain release (no drag) the pending click fires and
    /// collapses to the clicked node. `(line, node_id, mods)`.
    pending_click: Option<(usize, u64, Modifiers)>,
    /// The node-clipboard payload (the `rcx-clipboard/v1` blob written by
    /// EditorCopy/Cut). gpui's clipboard is text-only here, so the serialized blob
    /// also lands on the system clipboard; this field is the in-process fast path
    /// the paste handler reads first (and the test seam).
    node_clipboard: Option<String>,
    /// The background refresh-pump task (item 1). A spawned gpui loop that ticks
    /// `pump_refresh` on the controller's adaptive interval and recomposes when
    /// memory changed. Held so it is cancelled when the view is dropped.
    _refresh_task: Task<()>,
    /// `m_lastTabTarget` — persists across edit-begins so Tab continues the cycle.
    last_tab_target: Option<EditTarget>,
    /// `m_relativeOffsets` (editor.h:244) — the address-margin mode. **Defaults to
    /// `true`** (relative `"+<HEX>"` offsets, PIC4/PIC5), matching the C++; the
    /// user toggles to absolute addresses (PIC1/PIC2) via the margin double-click /
    /// context menu. Purely-visual editor state, so it lives on the view.
    relative_offsets: bool,
    /// Shadow of the controller's `compactColumns` flag. The controller owns the
    /// flag (it threads it into `compose(..)`) but exposes no getter, so the view
    /// mirrors the last value it set for `compact_columns()` to read back.
    compact_columns: bool,
    /// View-option toggle: gate the hovered-line background (the per-row hover
    /// wash). Purely-visual editor state (the C++ `m_hoverEffects`), defaults on;
    /// the View menu's "Hover effects" item flips it. When off, no hover band is
    /// drawn (the row still tracks the pointer for other affordances).
    hover_effects: bool,
    /// View-option toggle: render the right-side minimap overview column (the Zed
    /// minimap / the C++ purple overview block, data_options.png). Purely-visual
    /// editor state, defaults off; the View menu's "Minimap" item flips it.
    minimap: bool,
    /// Measured monospace cell metrics (re-measured each frame in `render` from
    /// the real shaped glyph advance so column math matches the painted grid).
    metrics: CellMetrics,
    /// The node-row context menu (`editor.cpp` `customContextMenuRequested`,
    /// reclass_right_click_on_address.png). When `Some`, a built [`PopupMenu`] is
    /// shown anchored at `context_menu_pos`; the right-clicked node is recorded in
    /// `context_target` so the menu's actions act on the correct row. Cleared on
    /// dismiss / after a menu action runs.
    context_menu: Option<Entity<gpui_component::menu::PopupMenu>>,
    context_menu_pos: Point<Pixels>,
    _context_menu_sub: Option<Subscription>,
    /// The node the context menu / accelerators target — the right-clicked node's
    /// `(display-line, node_idx, node_id)`. Set on right-mouse-down over a row;
    /// keyboard accelerators fall back to the primary-selected row when this is
    /// `None`.
    context_target: Option<ContextTarget>,
    /// The open TypeSelector popup subscription (Change Type / `T` / type-token
    /// click → the menus-agent `TypeSelectorPopup`, consumed via the contract).
    _type_selector_sub: Option<Subscription>,
    /// The open SourceChooser popup subscription (click the class-header `source▾`
    /// chip → the data-source picker, consumed via the contract; items 1/5).
    _source_chooser_sub: Option<Subscription>,
    /// The mounted FindBar entity (Ctrl+F), and the active find-match highlight the
    /// editor paints over the matched line (items 4/91/92). `find_bar` is `Some`
    /// while the bar is open; `find_match` is the current navigated match.
    find_bar: Option<Entity<crate::ui::findbar::FindBar>>,
    find_match: Option<crate::ui::findbar::FindMatch>,
    /// Item 31: the FULL match set, cached for the paint path (which has no `cx` to
    /// read the find-bar entity). Refreshed on Navigate and on every recompose
    /// (`sync_find_bar_lines`) so the painted IND_FIND bands track the layout.
    find_matches: Vec<crate::ui::findbar::FindMatch>,
    _find_bar_sub: Option<Subscription>,
    /// Item 33: the last find query, persisted across hide/show so re-opening the
    /// bar (Ctrl+F) resumes the prior search rather than starting blank (the C++
    /// `hideFindBar` keeps `m_findPos`; here we keep the query string).
    last_find_query: String,
    /// The open EnumPicker / HexToolbar popup subscriptions (items 8/9).
    _enum_picker_sub: Option<Subscription>,
    _hex_toolbar_sub: Option<Subscription>,
    /// Item 71/72: the live inline-edit validation state, recomputed on every
    /// field change. `Some` while editing; carries the error (empty = valid) so the
    /// row paints the red `M_ERR` band + a hint comment ('Enter=Save Esc=Cancel' on
    /// valid, '! <error>' on error), and suppresses the selection marker on error.
    edit_validation: Option<EditValidation>,
    /// Item 68/73: the floating expression-result popup shown while editing a
    /// BaseAddress / Value whose text contains an arithmetic operator. `Some` holds
    /// the evaluated `→ 0xHEX` / `Result: <value>` string + the line to anchor near.
    expr_result: Option<ExprResult>,
    /// Item 81/74: presentation mode (smooth animated scroll + a pulsing focus
    /// glow). Off by default; the window's "Presentation Mode" toggle flips it.
    presentation_mode: bool,
    /// Item 81/74: the AI/MCP focus node — its row(s) pulse with the `M_FOCUS` glow
    /// while set. 0 = no focus. Driven by `set_focus_node` / `clear_focus_node`.
    focus_node_id: u64,
    /// Item 74: the focus-glow pulse phase (bumped ~every 30ms by the glow timer);
    /// `0.5 + 0.5*sin(phase*PI/12)` modulates the glow alpha (the C++ `m_glowPhase`).
    focus_glow_phase: u32,
    /// The focus-glow pulse timer task (a ~30ms foreground loop). Held so it is
    /// cancelled when the view drops / focus clears.
    _focus_glow_task: Task<()>,
    /// Item 23: the presentation-mode smooth-scroll animation task (the C++
    /// `m_scrollAnim`, a `QVariantAnimation` with an OutExpo curve). Held so a new
    /// `smooth_scroll_to_node_id` cancels any in-flight glide.
    _scroll_anim_task: Task<()>,
    /// Item 75: the third per-pane surface (`VM_Debug`). When on, the editor renders
    /// the DEBUG dump (each composed line's margin + annotated text + per-line
    /// LineMeta) instead of the structured grid. Off by default; toggled by the
    /// view-mode cycle (window) via `set_debug_view` / `cycle_view_mode`.
    debug_view: bool,
    /// Item 13: the editor surface font family. `None` ⇒ the canonical
    /// `design::tokens::font::mono_family()` (the OnceLock default); `Some(name)`
    /// is the user's View > Font selection. The window's `set_editor_font` calls
    /// [`set_font_family`](RcxEditor::set_font_family) on each open editor + split
    /// pane so the surface actually re-renders with the chosen family (it
    /// previously stored the font but kept rendering with `mono_family()`).
    font_family: Option<SharedString>,
    /// Item 44: a per-editor zoom delta (points) added to the base editor font
    /// size, driven by Ctrl+wheel / Ctrl+=/Ctrl+-. Clamped so the grid stays sane.
    zoom_delta: f32,
    /// Item 20: the open "Cycle type" undo macro coalescing window. `cycle_macro_at`
    /// is the timestamp of the last ←/→ press; presses within 800ms stay in the
    /// open macro. `cycle_macro_open` tracks whether `begin_macro` is currently
    /// unmatched. `_cycle_macro_task` is the deferred 800ms close timer (the C++
    /// `m_cycleMacroTimer`); reassigning it cancels the prior pending close.
    cycle_macro_at: Option<std::time::Instant>,
    cycle_macro_open: bool,
    _cycle_macro_task: Task<()>,
    /// The most-recently-picked type display names, most-recent-first, capped at 8
    /// and deduped-to-front (the C++ `RcxController::m_recentTypeNames` /
    /// `pushRecentType`). Surfaces as the Type Selector's "Recent" section. The C++
    /// keeps this on the controller; the read-only file controller here exposes no
    /// such list, so the view owns it (purely a UI affordance).
    recent_type_names: Vec<String>,
    /// Item 13: set while the cursor is INSIDE the floating hover popup card. While
    /// set, `dispatch_row_hover` suppresses popup dismissal so moving onto the card
    /// (e.g. to click a value-history 'Set' button) does not clear it first (the
    /// C++ `m_hoverInside` / geometry-contains guard, editor.cpp:2815/4531).
    popup_cursor_inside: bool,
    /// Item 11: the type-inference quick-convert payload captured when the node
    /// context menu is built — `(node_id, [hint kinds])`. The `EditorHintConvert` /
    /// `EditorHintSplit` actions read this so the dynamic suggested kind(s) survive
    /// the trip through a parameterless gpui action (the C++ captures them in the
    /// menu-action lambda, controller.cpp:3478/3487).
    pending_hint_convert: Option<(u64, Vec<NodeKind>)>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
}

/// Item 71/72: the live inline-edit validation snapshot. Recomputed on each field
/// change (the C++ `validateEditLive`, editor.cpp:4891). `error` empty ⇒ valid.
#[derive(Clone, Debug, Default)]
struct EditValidation {
    /// The display line the edit overlays (the row that gets the `M_ERR` band).
    line: usize,
    /// The validator error message, or empty when the current text is valid.
    error: String,
}

/// Item 68/73: the floating expression-result popup state (the C++
/// `m_exprResultLabel` / `updateExprResultPopup`, editor.cpp:4923).
#[derive(Clone, Debug)]
struct ExprResult {
    /// The display line the edit is on (anchors the popup above the edit span).
    line: usize,
    /// The char-column the edit span starts at (horizontal anchor).
    col: i32,
    /// The rendered result text (`→ 0x1A2B` for an address / `Result: 42`).
    text: String,
}

/// The node a context-menu / accelerator action targets — captured on
/// right-mouse-down over a row so the menu's wired controller ops act on the
/// right node regardless of the current multi-selection.
#[derive(Copy, Clone, Debug)]
struct ContextTarget {
    /// The display line that was right-clicked (for inline-rename positioning).
    line: usize,
    /// The node's index into the tree (`controller_mut()` op argument).
    node_idx: usize,
    /// The node's stable id (`convert_to_typed_pointer` argument).
    node_id: u64,
    /// The node's current kind (for the quick type-cycler label + Change Type seed).
    kind: NodeKind,
    /// Item 6: the right-clicked row's `sub_line` (the C++ `subLine`) — the enum /
    /// bitfield MEMBER index when the row is a member line, else the row's own
    /// sub_line (0 for a plain node). Drives the member-specific context menu.
    sub_line: i32,
}

/// The kind of hover popup shown over a row's value column (item 13). The C++
/// `applyHoverCursor` opens one of three popups depending on the node:
/// value-history (heated changed values), disasm/hex-dump (func/void pointers),
/// or struct-preview (typed pointer).
#[derive(Clone, Debug)]
enum HoverPopupKind {
    /// A changed-value history list (newest → oldest), the heat graph analogue.
    /// Each entry carries the value text + a relative-age label ('now'/'12s ago'/
    /// '3m ago'/'1h ago'). `node_idx`/`sub_line`/`resolved_addr` + `set_buttons`
    /// drive the edit-time 'Set' buttons (item 68): when `set_buttons` is true the
    /// popup is shown during an active edit and each row gets a Set button that
    /// writes the value back into the node.
    ValueHistory {
        entries: Vec<(String, String)>,
        node_idx: i32,
        sub_line: i32,
        resolved_addr: u64,
        set_buttons: bool,
    },
    /// Disassembly of the code at a function pointer's target (title "Disassembly")
    /// or a hex dump at a void pointer's target (title "Hex Dump").
    TitleBody { title: String, body: String },
}

/// An open hover popup (item 13): which row anchored it, the cursor position to
/// float it near, and its content.
#[derive(Clone, Debug)]
struct HoverPopupState {
    line: usize,
    pos: Point<Pixels>,
    kind: HoverPopupKind,
}

/// The active inline-edit: the field entity + the line it overlays (so the row
/// builder can swap in the editable element on the right line/column).
struct EditingField {
    field: Entity<FieldInput>,
    line: usize,
    /// Char-column start of the edited span (where the overlay is positioned).
    col_start: i32,
    /// Char-column end of the edited span — the overlay's opaque band spans
    /// `[col_start, col_end)` so it covers exactly the edited column (and occludes
    /// the static glyphs beneath it), item 3.
    col_end: i32,
    /// Item 5: this edit is an ASCII byte-overwrite ('Edit ASCII'). The Value
    /// commit must pass `is_ascii = true` to `set_node_value` so the text is
    /// written per-byte as ASCII (the field opens as a Value edit then switches to
    /// `HexOverwrite::Ascii`, so the commit target is Value with no other signal).
    ascii_overwrite: bool,
    _subscription: Subscription,
}

impl RcxEditor {
    /// Build an editor over a fresh document (empty tree). The app replaces the
    /// document via [`set_document`] when opening a project.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = window;
        let mut controller = RcxController::new(RcxDocument::new());
        controller.refresh();
        RcxEditor {
            controller,
            editing: None,
            byte_sel: ByteSelection::new(),
            hovered_line: None,
            hovered_node_id: 0,
            hover_popup: None,
            caret_line: None,
            drag_anchor_line: None,
            drag_on_byte_grid: false,
            drag_anchor_x: 0.0,
            drag_started: false,
            pending_click_col: None,
            drag_init_mods: Modifiers::default(),
            pending_click: None,
            node_clipboard: None,
            _refresh_task: Self::spawn_refresh_loop(cx),
            last_tab_target: None,
            relative_offsets: true,
            compact_columns: false,
            hover_effects: true,
            minimap: false,
            metrics: CellMetrics::new(8.0, 16.0),
            context_menu: None,
            context_menu_pos: Point::default(),
            _context_menu_sub: None,
            context_target: None,
            _type_selector_sub: None,
            _source_chooser_sub: None,
            find_bar: None,
            find_match: None,
            find_matches: Vec::new(),
            last_find_query: String::new(),
            _find_bar_sub: None,
            _enum_picker_sub: None,
            _hex_toolbar_sub: None,
            edit_validation: None,
            expr_result: None,
            presentation_mode: false,
            focus_node_id: 0,
            focus_glow_phase: 0,
            _focus_glow_task: Task::ready(()),
            _scroll_anim_task: Task::ready(()),
            debug_view: false,
            font_family: None,
            zoom_delta: 0.0,
            cycle_macro_at: None,
            cycle_macro_open: false,
            _cycle_macro_task: Task::ready(()),
            recent_type_names: Vec::new(),
            popup_cursor_inside: false,
            pending_hint_convert: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`] (the form a panel/dock holds).
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| RcxEditor::new(window, cx))
    }

    /// Replace the controlled document and recompose (opening/switching a tab).
    ///
    /// Picks a sensible **default view root** before the first compose: when the
    /// document declares top-level structs (a multi-struct `.rcx`), compose with
    /// `view_root_id == 0` would otherwise stack *every* root in the grid and the
    /// class header would name whichever top-level struct happens to sort first
    /// (e.g. `_LIST_ENTRY` ahead of the project's `_EPROCESS`). Instead we focus
    /// the **first declared top-level struct** — the project's main root — so the
    /// editor opens on one struct with a correct header (the C++ rebinds the active
    /// tab's view root the same way; window.rs relies on this "picks a view root").
    pub fn set_document(&mut self, doc: RcxDocument, cx: &mut Context<Self>) {
        self.controller = RcxController::new(doc);
        if self.controller.view_root_id() == 0 {
            if let Some(root_id) = self.default_view_root_id() {
                self.controller.set_view_root_id(root_id);
            }
        }
        self.controller.refresh();
        self.editing = None;
        self.byte_sel.clear();
        self.close_context_menu(cx);
        self.context_target = None;
        cx.notify();
    }

    /// The id of the project's main root struct — the **first declared top-level
    /// `Struct`** (`children_of(0)` is the root list in declaration/offset order,
    /// the exact set `compose` walks). `None` when the tree has no top-level
    /// struct, in which case the view falls back to the all-roots default.
    fn default_view_root_id(&self) -> Option<u64> {
        let tree = self.controller.tree();
        for &idx in tree.children_of(0).iter() {
            let n = &tree.nodes[idx];
            if n.kind == crate::core::NodeKind::Struct {
                return Some(n.id);
            }
        }
        None
    }

    /// Read access to the engine (status bar / tests).
    pub fn controller(&self) -> &RcxController {
        &self.controller
    }

    /// Mutable engine access (the app wires sources/options through this).
    pub fn controller_mut(&mut self) -> &mut RcxController {
        &mut self.controller
    }

    /// The latest composed document being rendered.
    pub fn last_result(&self) -> &ComposeResult {
        self.controller.last_result()
    }

    /// `applyDocument` analogue — force a recompose + repaint.
    pub fn apply_document(&mut self, cx: &mut Context<Self>) {
        self.controller.refresh();
        cx.notify();
    }

    /// The default base refresh cadence (ms) before the adaptive engine widens it.
    /// Matches the C++ default timer interval; the engine backs off to
    /// `refresh_interval_max_ms` when idle / blurred (`apply_adaptive_interval`).
    const REFRESH_BASE_MS: u64 = 200;

    /// Spawn the live value-refresh loop (item 1). A self-rescheduling foreground
    /// timer that drives the controller's adaptive refresh engine
    /// (`pump_refresh` → `on_refresh_tick`/`read_pages`/`on_read_complete`) without
    /// any user input, then recomposes + repaints whenever a read landed (so typed
    /// values, ASCII/hex previews, float interps, enum labels, and the changed-byte
    /// heat all track live process/file memory). The C++ `RcxEditor` is driven by a
    /// `QTimer` the controller owns; gpui has no widget timer, so the editor view
    /// owns the loop and ticks the engine itself. No-op for dead/snapshot sources
    /// (`pump_refresh` returns `false` quickly — only live providers read).
    fn spawn_refresh_loop(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let interval = this
                    .read_with(cx, |this, _| {
                        let ms = this.controller.refresh_interval_ms();
                        if ms > 0 {
                            ms as u64
                        } else {
                            Self::REFRESH_BASE_MS
                        }
                    })
                    .unwrap_or(Self::REFRESH_BASE_MS);
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(interval.max(1)))
                    .await;
                // Tick the engine on the entity; recompose only when a read landed.
                let alive = this.update(cx, |this, cx| this.refresh_tick(cx)).is_ok();
                if !alive {
                    break; // the editor entity was dropped — stop the loop.
                }
            }
        })
    }

    /// One refresh tick (item 1): pump the engine and, if memory changed, re-apply
    /// the composed document. Skips work while inline-editing (the engine itself
    /// also early-returns then, but recomposing under an active edit would yank the
    /// overlay). Returns nothing; called from the spawned loop.
    fn refresh_tick(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            return;
        }
        if self.controller.pump_refresh() {
            // A read landed and the snapshot/heat changed — recompose + repaint so
            // the live values + changed-byte heat appear without user input.
            self.controller.refresh();
            let _ = self.controller.take_events();
            cx.notify();
        }
    }

    pub fn byte_selection(&self) -> &ByteSelection {
        &self.byte_sel
    }

    /// Whether the address margin shows relative `"+<HEX>"` offsets (the reclass
    /// default) vs absolute addresses (`m_relativeOffsets`).
    pub fn relative_offsets(&self) -> bool {
        self.relative_offsets
    }

    /// `setRelativeOffsets(rel)` (editor.h:115) — switch the margin between
    /// relative offsets (PIC4/PIC5) and absolute addresses (PIC1/PIC2). The margin
    /// double-click / context menu calls this in a later input-wiring workflow.
    pub fn set_relative_offsets(&mut self, relative: bool, cx: &mut Context<Self>) {
        if self.relative_offsets != relative {
            self.relative_offsets = relative;
            cx.notify();
        }
    }

    // ── View-option toggles (EDITOR SETTER CONTRACT) ──
    //
    // The four compose-affecting toggles (`compact_columns`, `tree_lines`,
    // `type_hints`, `show_comments`) are owned by the controller, which already
    // threads them into `compose(..)` through `refresh()` (controller.rs). The
    // editor setters delegate to the controller so a flip recomposes the document;
    // the getters read the controller's live value. `hover_effects` and `minimap`
    // are purely-visual editor state (no recompose) and live on the view, mirroring
    // `relative_offsets`.

    /// Whether columns are tightened (`compactColumns`, controller-backed; the
    /// view shadows the flag since the controller exposes no getter).
    pub fn compact_columns(&self) -> bool {
        self.compact_columns
    }
    /// `setCompactColumns(v)` — tighten/loosen the type/name column spacing. Routes
    /// to the controller, which recomposes with the new column geometry.
    pub fn set_compact_columns(&mut self, v: bool, cx: &mut Context<Self>) {
        self.compact_columns = v;
        self.controller.set_compact_columns(v);
        self.after_mutation(cx);
    }

    /// Whether the tree-line connectors are drawn (`treeLines`, controller-backed).
    /// The controller exposes no getter, but the composed [`LayoutInfo`] carries
    /// the flag it last composed with, so read it back from there.
    pub fn tree_lines(&self) -> bool {
        self.controller.last_result().layout.tree_lines
    }
    /// `setTreeLines(v)` — toggle the indent tree-connector glyphs; recomposes.
    pub fn set_tree_lines(&mut self, v: bool, cx: &mut Context<Self>) {
        self.controller.set_tree_lines(v);
        self.after_mutation(cx);
    }

    /// Whether type-inference hint chips are shown (`typeHints`, controller-backed).
    pub fn type_hints(&self) -> bool {
        self.controller.type_hints()
    }
    /// `setTypeHints(v)` — toggle the dim type-inference chips; recomposes.
    pub fn set_type_hints(&mut self, v: bool, cx: &mut Context<Self>) {
        self.controller.set_type_hints(v);
        self.after_mutation(cx);
    }

    /// Whether comment chips are shown (`showComments`, controller-backed).
    pub fn show_comments(&self) -> bool {
        self.controller.show_comments()
    }

    /// Whether in-place byte editing is allowed (items 42/7): the active provider
    /// is writable AND the read-only override is off. A File-backed / read-only
    /// source must not offer Edit Bytes / Edit ASCII.
    fn provider_writable(&self) -> bool {
        self.controller.document().provider.is_writable() && !self.controller.read_only_override()
    }
    /// `setShowComments(v)` — toggle the green comment chips; recomposes.
    pub fn set_show_comments(&mut self, v: bool, cx: &mut Context<Self>) {
        self.controller.set_show_comments(v);
        self.after_mutation(cx);
    }

    /// Whether the hovered-line background wash is drawn (view-only).
    pub fn hover_effects(&self) -> bool {
        self.hover_effects
    }
    /// `setHoverEffects(v)` — gate the per-row hover band (no recompose).
    pub fn set_hover_effects(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.hover_effects != v {
            self.hover_effects = v;
            cx.notify();
        }
    }

    /// Whether the right-side minimap overview column is shown (view-only).
    pub fn minimap(&self) -> bool {
        self.minimap
    }
    /// `setMinimap(v)` — show/hide the scaled structure overview column on the
    /// right edge (the Zed minimap / C++ purple overview; no recompose).
    pub fn set_minimap(&mut self, v: bool, cx: &mut Context<Self>) {
        if self.minimap != v {
            self.minimap = v;
            cx.notify();
        }
    }

    // ── Row text helpers ──

    /// Slice line `idx`'s text out of the composed document via `line_starts`
    /// (editor-surface.md §5 step 12). `line_starts` are **UTF-16 unit** offsets
    /// (the composer's internal buffer is UTF-16, mirroring Scintilla), so they
    /// are converted to UTF-8 byte offsets before slicing the `String`. Trailing
    /// `\n` is stripped.
    fn line_text(&self, idx: usize) -> &str {
        let result = self.controller.last_result();
        let starts = &result.line_starts;
        if idx >= starts.len() {
            return "";
        }
        let begin = geometry::utf16_to_byte(&result.text, starts[idx]);
        let end = if idx + 1 < starts.len() {
            geometry::utf16_to_byte(&result.text, starts[idx + 1])
        } else {
            result.text.len()
        };
        if end <= begin {
            return "";
        }
        result.text[begin..end].trim_end_matches('\n')
    }

    fn line_meta(&self, idx: usize) -> Option<&LineMeta> {
        self.controller.last_result().meta.get(idx)
    }

    /// Row text as an owned `String`, with the **command row substituted** by the
    /// controller's live [`build_command_row`](RcxController::build_command_row).
    ///
    /// The composed line-0 text is a fixed `"[▸] source▾  0x0  struct Untitled {"`
    /// stub; the real root struct name (`_EPROCESS`), keyword (`struct`/`class`),
    /// data-source label, and base address live on the controller. Since every
    /// command-row span helper (`command_row_*_span`) is a pure text scan for the
    /// `▾` arrow / `0x` / `struct ` keyword / ` {`, swapping in the live string
    /// keeps the exact `[▸] <src>▾  <addr>  <kw> <name> {` shape, so hit-testing,
    /// span coloring, and inline-edit all resolve against the same text the painter
    /// renders. Non-command rows fall through to the sliced composed text.
    fn line_text_owned(&self, idx: usize) -> String {
        if self
            .line_meta(idx)
            .is_some_and(|lm| lm.line_kind == LineKind::CommandRow)
        {
            return self.controller.build_command_row();
        }
        self.line_text(idx).to_string()
    }

    // ── Click routing (editor-surface.md §9) ──

    /// Handle a left mouse press on row `line` at pixel-relative X `rel_x`.
    fn on_row_mouse_down(
        &mut self,
        line: usize,
        rel_x: f32,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Grab keyboard focus for the editor surface so its key bindings fire
        // after a click (arrow-key node navigation, Down-at-end → append a field,
        // Ctrl+Shift+Up/Down reorder, F2/T/Delete, etc.). Without this, clicking a
        // node selected it but left the editor unfocused, so the keyboard did
        // nothing. A subsequent `begin_inline_edit` re-focuses the field input.
        window.focus(&self.focus_handle, cx);

        // A click elsewhere commits any active edit first (§9 "click elsewhere
        // → commitInlineEdit").
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }

        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        let text = self.line_text_owned(line);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let hit = hit_test::hit_test_row(&lm, &text, rel_x, self.metrics, type_w, name_w);

        // Plain LMB clears the byte selection (Shift/Ctrl preserve it; §9).
        if !modifiers.shift && !modifiers.control {
            self.byte_sel.clear();
        }

        // Record the drag anchor for row drag-select (item 8). Whether the press
        // landed on the hex byte grid decides the drag mode: byte-selection extend
        // vs node range-select.
        let on_byte_grid = self.byte_addr_for_hit(&lm, &text, hit.col).is_some();
        self.drag_anchor_line = Some(line);
        self.drag_on_byte_grid = on_byte_grid;
        // Item 28: record the press X + reset the drag-threshold latch; a held move
        // must travel past the dead-zone before drag-select begins.
        self.drag_anchor_x = rel_x;
        self.drag_started = false;
        // Item 20: remember the press modifiers so a Ctrl+drag ADDS the dragged
        // range to the selection (the C++ `m_dragInitMods`).
        self.drag_init_mods = modifiers;
        self.pending_click = None;

        // Fold-prefix click → materialize a CYCLE/self-ref head's children, else
        // toggle collapse (the C++ `handleMarginClick`, controller.cpp:5893): a fold
        // head whose `M_CYCLE` marker bit is set has no children of its own, so
        // expanding it must clone the referenced struct's children inline so they
        // become navigable (BUG #2). After the mutation, seed the nav anchor on the
        // toggled head's row so a subsequent Down descends into the new children.
        if hit.in_fold_col {
            if lm.node_idx >= 0 && lm.fold_head {
                let node_idx = lm.node_idx as usize;
                let node_id = lm.node_id;
                if (lm.marker_mask & (1u32 << crate::core::linemeta::M_CYCLE)) != 0 {
                    self.controller.materialize_ref_children(node_idx);
                } else {
                    self.controller.toggle_collapse(node_idx);
                }
                // Seed the nav anchor on the toggled head (the C++
                // setCursorPosition on the fold head) so Up/Down descends into the
                // freshly-materialized/expanded children rather than the first
                // ref-expanded occurrence of a repeated node_id (BUG #2 part A).
                self.controller
                    .handle_node_click(line as i64, node_id, CtrlMods::NONE);
                self.caret_line = Some(line);
                self.after_mutation(cx);
            }
            return;
        }

        // Hex byte click → byte selection (Shift extends; §9).
        if let Some(addr) = self.byte_addr_for_hit(&lm, &text, hit.col) {
            if modifiers.shift {
                self.byte_sel.shift_extend_to(addr);
            } else {
                self.byte_sel.arm(addr);
            }
            cx.notify();
            return;
        }

        // Footer pill click (item 10): the add-bytes / Top pills dispatch their op.
        if lm.line_kind == LineKind::Footer {
            if self.on_footer_click(&lm, &text, hit.col, cx) {
                return;
            }
        }

        let node_id = lm.node_id;
        let already_selected = node_id != 0
            && node_id != K_COMMAND_ROW_ID
            && self
                .controller
                .selected_ids()
                .iter()
                .any(|&id| crate::controller::strip_sel_pub(id) == node_id);

        // Item 11: Ctrl+Click (ctrl WITHOUT shift) on a Type/Name/PointerTarget
        // token of a navigable Header row → open the referenced struct in a NEW
        // tab. Restricted to Header lines (the parent row that has children) so a
        // child member row under an expanded parent doesn't fire (matches the C++
        // `openTypeInNewTabRequested` guard). Emits up to the host shell, which
        // owns tab creation. Falls through to plain Ctrl-toggle selection when the
        // node has no struct ref.
        if modifiers.control && !modifiers.shift {
            if let Some(target) = hit.target {
                if matches!(
                    target,
                    EditTarget::Type | EditTarget::Name | EditTarget::PointerTarget
                ) && lm.line_kind == LineKind::Header
                    && lm.node_idx >= 0
                {
                    // Item 10: resolve the open-in-new-tab target exactly like the
                    // C++ `openTypeInNewTabRequested` (controller.cpp:834): the
                    // node's `ref_id` if set, an array-of-struct's `ref_id`, else a
                    // PLAIN embedded `Struct` (no ref) opens its OWN subtree (its own
                    // id). Previously this only fired when `ref_id != 0`, so a plain
                    // embedded struct header fell through to Ctrl-toggle selection.
                    let target = {
                        let tree = self.controller.tree();
                        match tree.nodes.get(lm.node_idx as usize) {
                            Some(n) if n.ref_id != 0 => n.ref_id,
                            Some(n)
                                if n.kind == NodeKind::Array
                                    && n.element_kind == NodeKind::Struct
                                    && n.ref_id != 0 =>
                            {
                                n.ref_id
                            }
                            Some(n) if n.kind == NodeKind::Struct && n.parent_id != 0 => n.id,
                            _ => 0,
                        }
                    };
                    if target != 0 && self.controller.tree().index_of_id(target) >= 0 {
                        cx.emit(RcxEditorEvent::OpenTypeInNewTab { ref_id: target });
                        return;
                    }
                }
            }
        }

        // Picker-target interception (the C++ `beginInlineEdit` early-returns for
        // these, emitting a popup request instead — editor.cpp:3535-3573). These
        // fire on a PLAIN click (no Shift/Ctrl) and, for the command-row
        // chevron/source chip, regardless of node selection (the command row has no
        // selectable node). Without this, the hit-test target falls into
        // `begin_inline_edit` → `resolved_span_for` and starts a plain text edit on
        // the chip/chevron instead of opening the picker (items 1/2/5/6).
        if let Some(target) = hit.target {
            if !modifiers.shift && !modifiers.control {
                match target {
                    // Class-header SOURCE chip → the Data Source picker (item 1).
                    EditTarget::Source if lm.line_kind == LineKind::CommandRow => {
                        self.open_source_chooser(window, cx);
                        return;
                    }
                    // Class-header CHEVRON → the Root-mode Type Selector (item 2).
                    EditTarget::TypeSelector if lm.line_kind == LineKind::CommandRow => {
                        self.open_root_type_selector(window, cx);
                        return;
                    }
                    // Enum-value click → the EnumPickerPopup (item 8): an enum
                    // field's Value column opens the member picker (pre-selecting
                    // the current member) instead of a plain numeric edit.
                    EditTarget::Value
                        if already_selected
                            && lm.node_idx >= 0
                            && self.node_is_enum(lm.node_idx as usize) =>
                    {
                        self.open_enum_picker(line, lm.node_idx as usize, window, cx);
                        return;
                    }
                    // Hex node Type token → the Hex size toolbar (item 9), not the
                    // generic type selector: hex nodes pick a SIZE (8/16/32/64/128)
                    // + join/split, which the toolbar drives.
                    EditTarget::Type
                        if already_selected && lm.node_idx >= 0 && is_hex_preview(lm.node_kind) =>
                    {
                        self.open_hex_toolbar(lm.node_idx as usize, window, cx);
                        return;
                    }
                    // Field Type token / array element type / pointer target →
                    // the Type Selector in the matching mode (item 6). Only on a
                    // real node row that is already selected (matches the C++
                    // "click already-selected token → picker" affordance).
                    EditTarget::Type | EditTarget::ArrayElementType | EditTarget::PointerTarget
                        if already_selected && lm.node_idx >= 0 =>
                    {
                        let ctx = ContextTarget {
                            line,
                            node_idx: lm.node_idx as usize,
                            node_id: lm.node_id,
                            kind: lm.node_kind,
                            sub_line: lm.sub_line,
                        };
                        self.open_type_selector_in_mode(ctx, target, window, cx);
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Click on an editable target of an already-selected node → begin edit
        // (§9 "Click on already-selected (plain) → beginInlineEdit").
        if let Some(target) = hit.target {
            if (already_selected || lm.line_kind == LineKind::CommandRow)
                && !modifiers.shift
                && !modifiers.control
            {
                // Item 4: record the clicked column so a Vec/Mat Value edit narrows
                // to the clicked comma-component. Cleared by begin_inline_edit.
                self.pending_click_col = Some(hit.col);
                self.begin_inline_edit(line, target, window, cx);
                return;
            }
        }

        // Item 20: a PLAIN click on an already-selected node within a >1 selection
        // DEFERS the selection collapse to mouse-release (the C++
        // `m_pendingClickNodeId` path) so a drag that starts on the group keeps the
        // whole group. The pending click fires (collapsing to this node) on a plain
        // release; a drag flushes it as a shift-extend instead.
        let plain = !modifiers.control && !modifiers.shift;
        let multi = self.controller.selected_ids().len() > 1;
        if plain && multi && already_selected && node_id != 0 && node_id != K_COMMAND_ROW_ID {
            self.pending_click = Some((line, node_id, modifiers));
            self.caret_line = Some(line);
            return;
        }

        // Otherwise: selection (the controller owns Ctrl/Shift/cross-row logic).
        let mods = CtrlMods {
            ctrl: modifiers.control,
            shift: modifiers.shift,
        };
        self.controller
            .handle_node_click(line as i64, node_id, mods);
        // Track the moving caret so a subsequent Shift+arrow/page/home/end extends
        // from THIS click (items 2/3). A shift-click moves the caret to the clicked
        // row; a plain click reseeds it there.
        if node_id != 0 && node_id != K_COMMAND_ROW_ID {
            self.caret_line = Some(line);
        }
        self.after_mutation(cx);
    }

    /// The hex-byte address under a column on this row, if any (§12 `byteAddrAt`).
    fn byte_addr_for_hit(&self, lm: &LineMeta, text: &str, col: i32) -> Option<u64> {
        if !is_hex_preview(lm.node_kind) {
            return None;
        }
        let (type_w, name_w) = geometry::effective_widths(lm);
        let vs = crate::compose::value_span_for(lm, type_w, name_w);
        let count = if lm.line_byte_count > 0 {
            lm.line_byte_count
        } else {
            crate::core::size_for_kind(lm.node_kind)
        };
        let _ = text;
        selection::byte_addr_at(lm, vs, count, col)
    }

    /// Footer-pill click dispatch (item 10). Returns `true` when a pill was hit and
    /// its op ran. `Top` scrolls to the top; `+10h/+100h/+1000h` append that many
    /// bytes (as `Hex64` fields) to the footer's struct. `Trim`/`+10` need
    /// controller ops not exposed here; they hit-test but no-op gracefully.
    /// The struct id whose tail a footer's add/trim pills should grow. Normally
    /// the footer's own node, but a typed pointer-to-class fold and an embedded
    /// struct instance (a `Struct` with a `ref_id` and no own children) both grow
    /// the REFERENCED class definition — so the appended bytes land in the shared
    /// class, not on the pointer/instance node (which would orphan the ref view).
    fn footer_grow_target(&self, lm: &LineMeta) -> u64 {
        let tree = self.controller.tree();
        let idx = tree.index_of_id(lm.node_id);
        if idx < 0 {
            return lm.node_id;
        }
        let n = &tree.nodes[idx as usize];
        let is_ptr = matches!(n.kind, NodeKind::Pointer32 | NodeKind::Pointer64);
        let is_embedded_ref = n.kind == NodeKind::Struct && tree.children_of(n.id).is_empty();
        if n.ref_id != 0 && (is_ptr || is_embedded_ref) {
            return n.ref_id;
        }
        lm.node_id
    }

    fn on_footer_click(
        &mut self,
        lm: &LineMeta,
        text: &str,
        col: i32,
        cx: &mut Context<Self>,
    ) -> bool {
        // Identify which pill token the column lands in.
        let chars: Vec<char> = text.chars().collect();
        let mut hit_tok: Option<&str> = None;
        for span in geometry::footer_pill_spans(text) {
            if col >= span.start && col < span.end {
                let s = span.start.max(0) as usize;
                let e = (span.end.max(0) as usize).min(chars.len());
                let tok: String = chars[s..e].iter().collect();
                hit_tok = match tok.as_str() {
                    "Top" => Some("Top"),
                    "+10h" => Some("+10h"),
                    "+100h" => Some("+100h"),
                    "+1000h" => Some("+1000h"),
                    "Trim" => Some("Trim"),
                    "+10" => Some("+10"),
                    "+1" => Some("+1"),
                    _ => None,
                };
                break;
            }
        }
        let Some(tok) = hit_tok else {
            return false;
        };
        // The struct whose tail the add/trim pills grow. For a typed pointer-to-
        // class fold or an embedded struct instance (a struct with a refId and no
        // own children), the bytes belong to the REFERENCED class definition, not
        // the pointer/instance node itself — so resolve through `ref_id`.
        let grow_id = self.footer_grow_target(lm);
        match tok {
            "Top" => {
                self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
                true
            }
            "+10h" | "+100h" | "+1000h" => {
                let bytes = match tok {
                    "+10h" => 0x10,
                    "+100h" => 0x100,
                    _ => 0x1000,
                };
                self.append_bytes_to_struct(grow_id, bytes, cx);
                true
            }
            // `+1` single-add pill (the C++ `appendSingleFieldRequested`,
            // controller.cpp:962): append one Hex64 field at the container tail
            // (with the embedded-struct refId redirect), OR one auto-numbered enum
            // member when the footer's container is an enum. `append_single_field`
            // walks up to the enclosing Struct/Array/Enum and does exactly this.
            "+1" => {
                if grow_id != 0 && grow_id != K_COMMAND_ROW_ID {
                    self.controller.append_single_field(grow_id);
                    self.apply_document(cx);
                }
                true
            }
            // `+10` enum pill (the C++ `appendEnumMembersRequested`,
            // controller.cpp:1088): bulk-append 10 auto-numbered members.
            "+10" => {
                self.append_enum_members(lm.node_id, 10, cx);
                true
            }
            // `Trim` pill (the C++ `trimHexRequested`, controller.cpp:1047):
            // drop trailing hex padding fields from the struct.
            "Trim" => {
                self.trim_trailing_padding(grow_id, cx);
                true
            }
            _ => true,
        }
    }

    /// Bulk-append `count` auto-numbered enum members to the enum identified by
    /// `enum_id` (the footer `+10` pill — the C++ `appendEnumMembersRequested`,
    /// controller.cpp:1088). One undoable `ChangeEnumMembers` command swaps the old
    /// member list for the extended one; new members continue the value sequence
    /// from `last().value + 1` (or 0 for an empty enum), named `MemberN`.
    fn append_enum_members(&mut self, enum_id: u64, count: i32, cx: &mut Context<Self>) {
        if enum_id == 0 || enum_id == K_COMMAND_ROW_ID || count <= 0 {
            return;
        }
        let (node_id, old_members, new_members) = {
            let tree = self.controller.tree();
            let ni = tree.index_of_id(enum_id);
            if ni < 0 {
                return;
            }
            let node = &tree.nodes[ni as usize];
            if !node.is_enum() {
                return;
            }
            let old_members = node.enum_members.clone();
            let next_val = old_members.last().map(|(_, v)| v + 1).unwrap_or(0);
            let mut new_members = old_members.clone();
            for i in 0..count as i64 {
                let v = next_val + i;
                new_members.push((format!("Member{v}"), v));
            }
            (node.id, old_members, new_members)
        };
        self.controller
            .push_command(crate::core::Command::ChangeEnumMembers {
                node_id,
                old_members,
                new_members,
            });
        self.apply_document(cx);
    }

    /// Trim trailing hex padding fields from the struct `struct_id` (the footer
    /// `Trim` pill — the C++ `trimHexRequested`, controller.cpp:1047). Faithful
    /// port:
    /// - **Union**: a no-op — union members all overlap at offset 0, so there is
    ///   no "trailing" padding to remove.
    /// - **Embedded-struct refId redirect**: an embedded placeholder with no
    ///   children but a `ref_id` operates on the referenced root class instead.
    /// - Sort the target's children by offset **descending**, then collect the
    ///   leading run of `Hex8/16/32/64`/`Hex128` nodes (the trailing padding) and
    ///   remove them inside one undo macro.
    fn trim_trailing_padding(&mut self, struct_id: u64, cx: &mut Context<Self>) {
        if struct_id == 0 || struct_id == K_COMMAND_ROW_ID {
            return;
        }
        // Resolve the target container + the ids of trailing hex nodes to remove.
        let to_remove: Vec<u64> = {
            let tree = self.controller.tree();
            let si = tree.index_of_id(struct_id);
            if si < 0 {
                return;
            }
            // Unions have no trailing padding (all members overlap at offset 0).
            if tree.nodes[si as usize].is_union() {
                return;
            }
            // Embedded struct with a refId (virtual children) → operate on the
            // referenced root class definition instead.
            let mut children = tree.children_of(struct_id);
            if children.is_empty() && tree.nodes[si as usize].ref_id != 0 {
                let target_id = tree.nodes[si as usize].ref_id;
                children = tree.children_of(target_id);
            }
            if children.is_empty() {
                return;
            }
            // Sort children by offset DESCENDING to find the trailing run.
            children.sort_by(|&a, &b| tree.nodes[b].offset.cmp(&tree.nodes[a].offset));
            let mut ids = Vec::new();
            for ci in children {
                let n = &tree.nodes[ci];
                if !crate::core::is_hex_node(n.kind) {
                    break;
                }
                ids.push(n.id);
            }
            ids
        };
        if to_remove.is_empty() {
            return;
        }
        // One undo macro for the whole trim (the C++ beginMacro/endMacro group).
        self.controller
            .begin_macro(format!("Trim {} trailing hex nodes", to_remove.len()));
        for nid in to_remove {
            let idx = self.controller.tree().index_of_id(nid);
            if idx >= 0 {
                self.controller.remove_node(idx as usize);
            }
        }
        self.controller.end_macro();
        self.apply_document(cx);
    }

    /// Append `bytes` worth of `Hex64` fields to the struct identified by the
    /// footer row's `node_id` (item 10, the `+Nh` pills). Inserts at the struct's
    /// tail offset; rounds the byte count up to a whole `Hex64`.
    fn append_bytes_to_struct(&mut self, struct_id: u64, bytes: i32, cx: &mut Context<Self>) {
        if struct_id == 0 || struct_id == K_COMMAND_ROW_ID {
            return;
        }
        let count = (bytes + 7) / 8; // Hex64 = 8 bytes each
        if count <= 0 {
            return;
        }
        // The tail offset of the struct = max(child.offset + size) over its
        // members. For container children (struct/array) `size_for_kind` is 0,
        // so use the container-aware `struct_span` to measure past them — else
        // the appended field would land on top of the last child.
        let tail = {
            let tree = self.controller.tree();
            tree.children_of(struct_id)
                .iter()
                .map(|&ci| {
                    let c = &tree.nodes[ci];
                    let sz = if crate::core::is_container_kind(c.kind) {
                        tree.struct_span(c.id)
                    } else {
                        crate::core::size_for_kind(c.kind).max(0)
                    };
                    c.offset + sz
                })
                .max()
                .unwrap_or(0)
        };
        for i in 0..count {
            self.controller
                .insert_node(struct_id, tail + i * 8, NodeKind::Hex64, "");
        }
        self.apply_document(cx);
    }

    // ── Inline editing (editor-surface.md §11) ──

    /// Begin inline editing `target` on `line`. Picker targets (Type / element
    /// type / pointer target / source / type-selector) are popup-driven in the
    /// C++; here they fall back to inline text editing of the resolved span (a
    /// faithful subset — the filtered picker overlay is a later UI workflow).
    fn begin_inline_edit(
        &mut self,
        line: usize,
        target: EditTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        let text = self.line_text_owned(line);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        // Hex VALUE editing is a fixed-length per-byte overwrite over the byte grid
        // (armed as `hex_overwrite_mode` below). `resolved_span_for` deliberately
        // returns an invalid span for a hex Value (it is not a plain inline edit),
        // so resolve the byte-grid value span directly here — otherwise the early
        // `!span.valid` return meant "Edit Bytes (Hex)", "Edit ASCII", and Enter on
        // a hex node all silently did nothing.
        let span =
            if target == EditTarget::Value && lm.node_idx >= 0 && is_hex_preview(lm.node_kind) {
                crate::compose::value_span_for(&lm, type_w, name_w)
            } else {
                geometry::resolved_span_for(&lm, &text, target, type_w, name_w)
            };
        if !span.valid || span.end <= span.start {
            return;
        }

        // Seed the field with the current span text (trimmed).
        //
        // The command-row spans (`command_row_*_span` → RootClassName / RootClassType
        // / BaseAddress / Source) are produced by **UTF-16-unit** text scans, so
        // their `start`/`end` are unit offsets; every other line's resolved span is a
        // display column (= char index, the painter's cell space). Convert each with
        // the matching mapping so the seed is the exact token under the click even
        // when an earlier token carries a non-BMP glyph (e.g. an astral char in the
        // data-source label would otherwise shift a char-based slice and seed the
        // wrong substring — the `byte_for_col`-vs-unit-span hazard).
        let (start_byte, end_byte) = if lm.line_kind == LineKind::CommandRow {
            (
                geometry::utf16_to_byte(&text, span.start),
                geometry::utf16_to_byte(&text, span.end),
            )
        } else {
            (
                geometry::byte_for_col(&text, span.start),
                geometry::byte_for_col(&text, span.end),
            )
        };
        let raw_span = text.get(start_byte..end_byte).unwrap_or("");

        // Item 7: editing the VALUE of a hex node is a fixed-length per-byte
        // overwrite (hex digits, space-separated). Detect it here and seed the
        // field with the UNTRIMMED, fixed-length `"NN NN …"` string so the
        // overwrite positions line up with the rendered bytes. Other targets seed
        // the trimmed token as before.
        let hex_overwrite_mode =
            if target == EditTarget::Value && lm.node_idx >= 0 && is_hex_preview(lm.node_kind) {
                let byte_count = if lm.line_byte_count > 0 {
                    lm.line_byte_count as usize
                } else {
                    crate::core::size_for_kind(lm.node_kind).max(0) as usize
                };
                if byte_count > 0 {
                    Some(inline_edit::HexOverwrite::Hex { byte_count })
                } else {
                    None
                }
            } else {
                None
            };

        // Both ordinary tokens and the hex-overwrite seed are the trimmed span: the
        // hex VALUE column is exactly `"NN NN …"` (single inter-byte spaces), so
        // trimming only the outer column padding preserves the fixed-length string
        // the overwrite positions index against.
        let mut initial = raw_span.trim().to_string();

        let resolved_addr = lm.offset_addr;
        let node_idx = lm.node_idx;
        let mut sub_line = lm.sub_line;

        // Item 4: editing a single component of a Vec2/Vec3/Vec4/Mat4x4 VALUE by
        // clicking it. The value renders as a comma-joined `"x, y, z"` (Mat4x4:
        // 16 floats flat). Narrow the seed to the clicked comma-separated component
        // and set `sub_line` to its index so the controller's `set_node_value`
        // routes the write to `addr + sub_line*4` as a Float (controller.rs:1469).
        // No narrowing for keyboard edits (no click column) — the whole string is
        // seeded and the C++ caret defaults to component 0.
        let click_col = self.pending_click_col.take();
        if target == EditTarget::Value
            && lm.node_idx >= 0
            && matches!(
                lm.node_kind,
                NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4 | NodeKind::Mat4x4
            )
        {
            let comps: Vec<&str> = raw_span.split(',').collect();
            if comps.len() > 1 {
                // Map the clicked display column to a component index by walking the
                // value span char-by-char and counting commas before the click.
                let comp = if let Some(col) = click_col {
                    // `col` is a display column; the value span starts at span.start.
                    let rel = (col - span.start).max(0) as usize;
                    let span_chars: Vec<char> = raw_span.chars().collect();
                    let upto = rel.min(span_chars.len());
                    span_chars[..upto].iter().filter(|&&c| c == ',').count()
                } else {
                    0
                };
                let comp = comp.min(comps.len() - 1);
                initial = comps[comp].trim().to_string();
                sub_line = comp as i32;
            }
        }
        let palette = EditorPalette::from_theme(cx);
        let color = palette.text;
        // The inline-edit text-selection fill — the Zed text-selection token (NOT
        // an ad-hoc hex), so the edit selection reads on-palette and retints with a
        // theme switch.
        let selection_color = design::color::selection_bg(cx);

        let field = cx.new(|cx| {
            FieldInput::new(
                node_idx,
                sub_line,
                target,
                resolved_addr,
                initial,
                color,
                selection_color,
                cx,
            )
        });

        // React to the field's commit/cancel outcome AND re-render the host on
        // every field change. The editable `FieldElement` is embedded in this
        // view's `render_row` (not in the field's own `Render`), so a keystroke —
        // which notifies the field — would otherwise leave the row's painted caret
        // stale until an unrelated editor repaint (mouse-move) ran. Observing the
        // field and `cx.notify()`-ing here re-prepaints the row on every keystroke,
        // cursor move, and blink tick, so the caret stays solid while typing and
        // is recomputed at the live cursor offset (BUG 2).
        // `observe_in` (not `observe`) so the callback receives a `Window`: when the
        // field commits/cancels (Enter/Esc) it is dropped, orphaning keyboard focus,
        // so we must return focus to the editor surface — which needs a window.
        let subscription =
            cx.observe_in(&field, window, |this: &mut RcxEditor, field, window, cx| {
                let outcome = field.update(cx, |f, _| f.take_outcome());
                if let Some(outcome) = outcome {
                    this.resolve_edit_outcome(outcome, window, cx);
                } else {
                    // Item 71/72/68/73: while still editing, re-validate the live text
                    // and refresh the expression-result popup on every change.
                    this.update_edit_validation(cx);
                }
                cx.notify();
            });

        self.last_tab_target = Some(target);
        self.editing = Some(EditingField {
            field: field.clone(),
            line,
            col_start: span.start,
            col_end: span.end,
            ascii_overwrite: false,
            _subscription: subscription,
        });
        // Focus the field AFTER its element is in the render tree. Focusing a
        // handle whose element has not yet painted is dropped at the frame boundary
        // — the edit box appeared but never received keyboard input (ALL inline
        // edits were un-typeable: rename, value, hex). Popups don't hit this because
        // the dialog layer paints them immediately. Deferring focuses the field once
        // this notify's re-render has placed the editing overlay in the tree.
        let handle = field.read(cx).field_focus_handle();
        window.defer(cx, move |window, cx| window.focus(&handle, cx));
        // Arm the caret blink with a solid caret so the field shows an immediate,
        // continuously-visible cursor the moment editing begins (BUG 2).
        field.update(cx, |f, cx| f.arm_caret(cx));
        // Item 7: switch the field into fixed-length hex/ASCII overwrite mode AFTER
        // arming (so the begin-edit select-all is replaced by a caret at offset 0).
        if let Some(mode) = hex_overwrite_mode {
            field.update(cx, |f, _cx| f.set_hex_overwrite(mode));
        }
        // Item 71/72: seed the validation/hint state for the freshly-opened edit so
        // the green 'Enter=Save Esc=Cancel' hint shows immediately (and any seeded
        // error paints right away).
        self.update_edit_validation(cx);
        // Item 19: when editing a heated changed VALUE that has >1 distinct samples,
        // proactively float the value-history popup anchored to the edit field (the
        // C++ recreates the popup with 'Set' buttons the moment editing starts,
        // editor.cpp:3579) — independent of the mouse position, which is where the
        // Rust path previously only showed it.
        self.arm_edit_value_history_popup(line, target, cx);
        cx.notify();
    }

    /// Item 19: open the value-history popup (with edit-time 'Set' buttons) anchored
    /// to the active edit field when editing a heated VALUE with >1 distinct value
    /// samples. No-op for non-Value targets / unheated values / a single sample.
    fn arm_edit_value_history_popup(
        &mut self,
        line: usize,
        target: EditTarget,
        _cx: &mut Context<Self>,
    ) {
        if target != EditTarget::Value {
            return;
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        if lm.node_idx < 0
            || lm.node_id == 0
            || lm.node_id == K_COMMAND_ROW_ID
            || lm.heat_level == 0
        {
            return;
        }
        let Some(hist) = self.controller.value_history().get(&lm.node_id) else {
            return;
        };
        if hist.unique_count() <= 1 {
            return;
        }
        let now = Self::now_millis();
        let mut entries: Vec<(String, String)> = Vec::new();
        hist.for_each_with_time(|v, t| {
            if entries.len() < crate::core::value_history::K_CAPACITY {
                entries.push((v.to_string(), Self::relative_age(now, t)));
            }
        });
        if entries.len() <= 1 {
            return;
        }
        // Anchor near the edited row: float at the row's top-left in surface space
        // (the deferred anchor offsets by (+12,+16) for the cursor cards; here we
        // anchor to the field row, the C++ field-anchored popup). Use the measured
        // line height to place it at the row baseline.
        let y = (line as f32) * self.metrics.line_height;
        let pos = point(px(0.0), px(y));
        self.hover_popup = Some(HoverPopupState {
            line,
            pos,
            kind: HoverPopupKind::ValueHistory {
                entries,
                node_idx: lm.node_idx,
                sub_line: lm.sub_line,
                resolved_addr: lm.offset_addr,
                set_buttons: true,
            },
        });
    }

    /// Item 71/72/68/73: recompute the live inline-edit validation + expression-
    /// result popup from the active field's current text (the C++ `validateEditLive`
    /// + `updateExprResultPopup`, editor.cpp:4891/4923). No-op when not editing.
    ///
    /// - Validation: `validate_base_address` for a BaseAddress edit, else
    ///   `validate_value(kind, text)` for a Value edit (other targets — name /
    ///   comment / type — are never invalid here, so they validate clean).
    /// - Expression result: when editing a BaseAddress, or a Value whose text
    ///   contains an arithmetic operator (`+ - * / << >> & | ^ ~`), evaluate the
    ///   text via `parse_base_address` and float a `→ 0xHEX` / `Result: N` popup.
    fn update_edit_validation(&mut self, _cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            self.edit_validation = None;
            self.expr_result = None;
            return;
        };
        let line = editing.line;
        let col = editing.col_start;
        let target = editing.field.read(_cx).target();
        let text = editing.field.read(_cx).content().trim().to_string();
        let is_overwrite = editing.field.read(_cx).is_hex_overwrite();

        // The kind the value is validated against (the edited node's kind).
        let kind = self
            .line_meta(line)
            .map(|lm| lm.node_kind)
            .unwrap_or(NodeKind::Hex64);

        // ── Validation (editor.cpp:4891) ──
        let error = match target {
            EditTarget::BaseAddress => crate::format::validate_base_address(&text),
            EditTarget::Value if !is_overwrite => crate::format::validate_value(kind, &text),
            // Hex/ASCII overwrite, names, comments, types, etc. are not range-
            // validated inline (they are always structurally valid here).
            _ => String::new(),
        };
        self.edit_validation = Some(EditValidation { line, error });

        // ── Expression-result popup (editor.cpp:4923) ──
        let is_addr = target == EditTarget::BaseAddress;
        let is_val = target == EditTarget::Value && !is_overwrite;
        let has_operator = text
            .chars()
            .any(|c| matches!(c, '+' | '-' | '*' | '/' | '<' | '>' | '&' | '|' | '^' | '~'));
        // Address edits always show the resolved value; value edits only when the
        // text reads as an expression (otherwise it is a plain literal).
        if (is_addr || (is_val && has_operator)) && !text.is_empty() {
            let base = self.controller.tree().base_address;
            let (value, formula) = parse_base_address(&text, base);
            // A pure formula we cannot resolve numerically (no provider) keeps the
            // fallback base; only float a result when the text actually evaluated to
            // a number (formula empty) OR resolved to a non-fallback value.
            let resolved = formula.is_empty() || value != base;
            if resolved {
                let label = if is_addr { "→" } else { "Result:" };
                self.expr_result = Some(ExprResult {
                    line,
                    col,
                    text: format!("{label} 0x{value:X}"),
                });
            } else {
                self.expr_result = None;
            }
        } else {
            self.expr_result = None;
        }
    }

    /// Apply a committed/cancelled inline edit (the `inlineEditCommitted`/
    /// `inlineEditCancelled` round-trip, editor-surface.md §11).
    fn resolve_edit_outcome(
        &mut self,
        outcome: EditOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            EditOutcome::Commit(commit) => {
                // Item 5: capture the ASCII-overwrite flag before clearing the edit
                // state so the Value commit writes per-byte ASCII.
                let ascii = self
                    .editing
                    .as_ref()
                    .map(|e| e.ascii_overwrite)
                    .unwrap_or(false);
                self.editing = None;
                self.edit_validation = None;
                self.expr_result = None;
                self.apply_commit(&commit, ascii, cx);
                // Return keyboard focus to the editor surface. The committed field
                // entity is now dropped, so without this the focus is orphaned and
                // the next keystroke (e.g. Enter to re-edit) is swallowed until the
                // user clicks back into the editor.
                window.focus(&self.focus_handle, cx);
            }
            EditOutcome::Cancel => {
                self.editing = None;
                self.edit_validation = None;
                self.expr_result = None;
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            EditOutcome::Continue => {}
        }
    }

    /// Commit whatever the active field holds (called on click-away / focus-out).
    fn commit_active_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        self.edit_validation = None;
        self.expr_result = None;
        let ascii = editing.ascii_overwrite;
        let commit = editing.field.read(cx).to_commit();
        self.apply_commit(&commit, ascii, cx);
    }

    /// The stable id of the node at tree index `idx` (0 if out of range).
    fn node_id_at(&self, idx: usize) -> u64 {
        self.controller
            .tree()
            .nodes
            .get(idx)
            .map(|n| n.id)
            .unwrap_or(0)
    }

    /// Item 6: does index `idx` name an enum whose member `sub_line` is in range?
    fn node_is_enum_member(&self, idx: usize, sub_line: i32) -> bool {
        let tree = self.controller.tree();
        match tree.nodes.get(idx) {
            Some(n) if n.is_enum() => sub_line >= 0 && (sub_line as usize) < n.enum_members.len(),
            _ => false,
        }
    }

    /// Item 6: is the node at `idx` a hex-preview node (ASCII byte editing)?
    fn node_is_hex(&self, idx: usize) -> bool {
        self.controller
            .tree()
            .nodes
            .get(idx)
            .map(|n| is_hex_preview(n.kind))
            .unwrap_or(false)
    }

    /// Route a committed edit to the matching controller mutation
    /// (editor-surface.md §1: the controller recomposes, then we refresh).
    /// `ascii` (item 5) marks a Value commit that came from the 'Edit ASCII'
    /// overwrite editor, so it is written per-byte as ASCII rather than parsed hex.
    fn apply_commit(&mut self, commit: &EditCommit, ascii: bool, cx: &mut Context<Self>) {
        if commit.node_idx < 0 {
            // Command-row edits act on the *active/root class*, which has no row
            // `node_idx` of its own (the command row is synthetic, node_idx = -1).
            // Route them to the controller against the view-root node / tree base
            // so a left-click → type → Enter on the base address / class name / the
            // `struct`/`class` keyword actually persists (BUG 1: previously these
            // recomposed without writing, so "typing did nothing").
            self.apply_command_row_commit(commit, cx);
            self.after_mutation(cx);
            return;
        }
        let idx = commit.node_idx as usize;
        match commit.target {
            EditTarget::RootClassName => {
                self.controller.rename_node(idx, &commit.text);
            }
            // Item 6: a Name edit must branch on the row's sub_line + node kind
            // (controller.cpp:1138):
            //   * enum member sub-line → `rename_member` (renames the MEMBER, not
            //     the enum node),
            //   * hex node → `set_node_value(isAscii=true)` ASCII byte-write,
            //   * otherwise → `rename_node`.
            // (Empty text is a no-op for the Name target, matching the C++ guard.)
            EditTarget::Name => {
                if commit.text.is_empty() {
                    // no-op (C++: `if (text.isEmpty()) break;`)
                } else if self.node_is_enum_member(idx, commit.sub_line) {
                    self.controller.rename_member(
                        self.node_id_at(idx),
                        commit.sub_line as usize,
                        &commit.text,
                    );
                } else if self.node_is_hex(idx) {
                    self.controller.set_node_value(
                        idx,
                        commit.sub_line,
                        &commit.text,
                        /* is_ascii */ true,
                        commit.resolved_addr,
                    );
                } else {
                    self.controller.rename_node(idx, &commit.text);
                }
            }
            EditTarget::Type => {
                self.controller.apply_type_text(idx, &commit.text);
            }
            // Item 5: editing an Array's ELEMENT TYPE must swap only `element_kind`
            // (keeping the node an Array + its length), NOT route through
            // `apply_type_text` whose bare-name branch would `change_node_kind` the
            // whole array into a scalar. Mirrors controller.cpp:1306 (a bare type
            // name → `ChangeArrayMeta{ element_kind: new }`).
            EditTarget::ArrayElementType => {
                self.commit_array_element_type(idx, commit.text.trim(), cx);
            }
            // Item 6: a Value edit on an enum member sub-line sets the MEMBER value
            // (controller.cpp:1222) via `set_member_value`, not the node's bytes;
            // otherwise it writes the node value (`isAscii=false`).
            EditTarget::Value => {
                if self.node_is_enum_member(idx, commit.sub_line) {
                    self.controller.set_member_value(
                        self.node_id_at(idx),
                        commit.sub_line as usize,
                        commit.text.trim(),
                    );
                } else {
                    // Item 5: an 'Edit ASCII' overwrite commit writes per-byte ASCII
                    // (is_ascii=true) so the text isn't mis-parsed as hex.
                    self.controller.set_node_value(
                        idx,
                        commit.sub_line,
                        &commit.text,
                        ascii,
                        commit.resolved_addr,
                    );
                }
            }
            // Comment / pointer-target / array-element-count / static-expr commits
            // (item 15): the prior `_ => {}` arm silently dropped these. Write each
            // to the tree via its undoable `Command`. The node id is resolved from
            // `idx` so the command survives an index shift on undo/redo.
            EditTarget::Comment => {
                self.commit_comment(idx, commit.text.trim(), cx);
            }
            EditTarget::PointerTarget => {
                self.commit_pointer_target(idx, commit.text.trim(), cx);
            }
            EditTarget::ArrayElementCount | EditTarget::ArrayCount => {
                self.commit_array_count(idx, commit.text.trim(), cx);
            }
            EditTarget::StaticExpr => {
                self.commit_static_expr(idx, commit.text.trim(), cx);
            }
            _ => {}
        }
        self.after_mutation(cx);
    }

    /// Write a committed comment edit (item 15) via the undoable `ChangeComment`.
    fn commit_comment(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
        let node_id = n.id;
        let old_comment = n.comment.clone();
        if old_comment == text {
            return;
        }
        self.controller
            .push_command(crate::core::Command::ChangeComment {
                node_id,
                old_comment,
                new_comment: text.to_string(),
            });
    }

    /// Write a committed pointer-target type edit (item 15). The typed text names
    /// the struct the pointer should reference; resolve it to a struct id and push
    /// `ChangePointerRef`. An unrecognized name falls back to `apply_type_text`
    /// (which the controller parses for primitive/`*` forms).
    fn commit_pointer_target(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let node_id = tree.nodes[idx].id;
        let old_ref_id = tree.nodes[idx].ref_id;
        // Find a struct whose type name matches the typed target.
        let new_ref_id = tree
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Struct && n.struct_type_name == text)
            .map(|n| n.id);
        match new_ref_id {
            Some(new_ref_id) if new_ref_id != old_ref_id => {
                self.controller
                    .push_command(crate::core::Command::ChangePointerRef {
                        node_id,
                        old_ref_id,
                        new_ref_id,
                    });
            }
            _ => {
                // Not a known struct: let the controller's type parser handle it.
                self.controller.apply_type_text(idx, text);
            }
        }
    }

    /// Write a committed array element count (item 15) via `ChangeArrayMeta`,
    /// keeping the element kind and setting the new length from the typed number.
    /// Item 23: parse STRICTLY as decimal (`text.toInt`, controller.cpp:1325) so
    /// `"ff"`/`"0x10"` are no-ops, and reject unless `0 < count <= 100000` (the
    /// C++ upper bound that guards against an OOM compose).
    fn commit_array_count(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let count: i32 = match text.parse::<i32>() {
            Ok(c) if c > 0 && c <= 100_000 => c,
            _ => return,
        };
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
        if n.kind != NodeKind::Array {
            return;
        }
        let node_id = n.id;
        let old_element_kind = n.element_kind;
        let old_array_len = n.array_len;
        if old_array_len == count {
            return;
        }
        self.controller
            .push_command(crate::core::Command::ChangeArrayMeta {
                node_id,
                old_element_kind,
                new_element_kind: old_element_kind,
                old_array_len,
                new_array_len: count,
            });
    }

    /// Item 5: write a committed array ELEMENT TYPE edit (controller.cpp:1306).
    /// Requires the node to be an Array; a recognized bare type name swaps only
    /// `element_kind` via `ChangeArrayMeta` (keeping `array_len`). An unrecognized
    /// name / unchanged kind is a no-op (the array is NOT collapsed to a scalar).
    fn commit_array_element_type(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
        if n.kind != NodeKind::Array {
            return;
        }
        let node_id = n.id;
        let old_element_kind = n.element_kind;
        let old_array_len = n.array_len;
        let (elem_kind, ok) = crate::core::kind_from_type_name(text);
        if !ok || elem_kind == old_element_kind {
            return;
        }
        self.controller
            .push_command(crate::core::Command::ChangeArrayMeta {
                node_id,
                old_element_kind,
                new_element_kind: elem_kind,
                old_array_len,
                new_array_len: old_array_len,
            });
    }

    /// Write a committed static-expression edit (item 15) via `ChangeOffsetExpr`.
    fn commit_static_expr(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
        // Item 7: only a STATIC node gets an offsetExpr written (the C++
        // `EditTarget::StaticExpr` guard `if (node.isStatic && text != ...)`,
        // controller.cpp:1405). A non-static node must NOT acquire an offsetExpr
        // (which would quietly make it relative on the next compose).
        if !n.is_static {
            return;
        }
        let node_id = n.id;
        let old_expr = n.offset_expr.clone();
        if old_expr == text {
            return;
        }
        self.controller
            .push_command(crate::core::Command::ChangeOffsetExpr {
                node_id,
                old_expr,
                new_expr: text.to_string(),
            });
    }

    /// Apply a committed **command-row** edit (the synthetic class-header row,
    /// `node_idx < 0`) against the active/root class (BUG 1):
    /// - `BaseAddress` → parse the typed hex/expression and push the controller's
    ///   `ChangeBase` command (undoable). A bare hex literal sets the numeric base
    ///   and clears the formula; anything else is kept as the base-address
    ///   *formula* string (`app.exe + 0x1A0`, `[app.exe + 0x58]`, …) so the
    ///   command row redisplays it verbatim (the address tooltip documents these).
    /// - `RootClassName` → rename the view-root struct node via the existing
    ///   `rename_node` op.
    /// - other command-row targets (source / keyword / chevron) have no plain-text
    ///   controller op here, so they recompose without writing.
    fn apply_command_row_commit(&mut self, commit: &EditCommit, cx: &mut Context<Self>) {
        match commit.target {
            EditTarget::BaseAddress => {
                let text = commit.text.trim();
                let old_base = self.controller.tree().base_address;
                let old_formula = self.controller.document().tree.base_address_formula.clone();
                let (new_base, new_formula) = parse_base_address(text, old_base);
                if new_base != old_base || new_formula != old_formula {
                    self.controller
                        .push_command(crate::core::Command::ChangeBase {
                            old_base,
                            new_base,
                            old_formula,
                            new_formula,
                        });
                }
            }
            EditTarget::RootClassName => {
                // B3 / item 4: rename the viewed root struct's `struct_type_name`
                // (NOT its `name`) — the command-row display reads `struct_type_name`
                // first (see `build_command_row`), so renaming `name` left the
                // displayed name unchanged. `rename_root_class` mirrors the C++
                // `EditTarget::RootClassName` commit (`ChangeStructTypeName`), so the
                // recomposed command row reflects the new name.
                self.controller.rename_root_class(commit.text.trim());
            }
            // Gap 21: inline-edit the root class KEYWORD (`struct`/`class`/`union`/
            // `enum`). The C++ `EditTarget::RootClassType` commit routes to
            // `selectKeyword`/keyword conversion; `set_root_class_keyword` parses the
            // typed keyword and pushes a `ChangeClassKeyword` command (no-op for an
            // unrecognized word).
            EditTarget::RootClassType => {
                self.controller.set_root_class_keyword(commit.text.trim());
            }
            // Gap 22: the Source-dropdown inline commit. The C++ `EditTarget::Source`
            // commit selects a saved source by display-name (`selectSource`). Resolve
            // the typed name to a saved-source index and switch to it.
            EditTarget::Source => {
                self.select_source(commit.text.trim());
            }
            _ => {}
        }
        let _ = cx;
    }

    /// Select a saved data source by its display name (gap 22, the C++
    /// `selectSource` / `EditTarget::Source` commit). Resolves `name` to a
    /// saved-source slot index (case-insensitive on the display name) and switches
    /// the controller to it via `switch_to_saved_source`. No-op for an unknown name
    /// or when it is already active. The actual byte-provider attach is the
    /// controller's job; this only drives the source selection.
    fn select_source(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let idx = self
            .controller
            .saved_sources()
            .iter()
            .position(|s| s.display_name.eq_ignore_ascii_case(name));
        if let Some(idx) = idx {
            if idx as i32 != self.controller.active_source_index() {
                self.controller.switch_to_saved_source(idx as i32);
            }
        }
    }

    /// Tab to the next editable field in the current row (or first field if not
    /// editing), wrapping (editor-surface.md §10 Tab branch).
    fn tab_to_next_field(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.editing.as_ref().map(|e| e.line).or_else(|| {
            // Not editing: start from the primary selected line, else line 1.
            self.first_selected_line()
        });
        let Some(line) = line else {
            return;
        };
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        // Shift+Tab walks the cycle backward (item 17); Tab advances forward. Both
        // start from `m_lastTabTarget` and skip inapplicable targets.
        let next = if backward {
            tab_cycle::prev_tab_target(&lm, self.last_tab_target)
        } else {
            tab_cycle::next_tab_target(&lm, self.last_tab_target)
        };
        if let Some(target) = next {
            self.begin_inline_edit(line, target, window, cx);
        }
    }

    fn first_selected_line(&self) -> Option<usize> {
        let result = self.controller.last_result();
        let sel = self.controller.selected_ids();
        if sel.is_empty() {
            // First data line.
            return (result.meta.len() > 1).then_some(1);
        }
        for (i, lm) in result.meta.iter().enumerate() {
            if lm.node_id != 0
                && sel
                    .iter()
                    .any(|&id| crate::controller::strip_sel_pub(id) == lm.node_id)
            {
                return Some(i);
            }
        }
        None
    }

    /// After any controller mutation: drain events (status hints etc. are read by
    /// the app shell) and recompose. The controller's mutators recompose
    /// internally via `refresh`, but draining keeps the event queue bounded.
    fn after_mutation(&mut self, cx: &mut Context<Self>) {
        let _events = self.controller.take_events();
        self.sync_find_bar_lines(cx);
        cx.notify();
    }

    /// Re-feed the find bar the current line texts after a recompose (item 4) so the
    /// search set tracks the document. No-op when the bar is closed.
    fn sync_find_bar_lines(&mut self, cx: &mut Context<Self>) {
        if self.find_bar.is_some() {
            let lines = self.current_line_texts();
            if let Some(bar) = self.find_bar.clone() {
                bar.update(cx, |b, cx| b.set_lines(lines, cx));
                // Item 31: a recompose re-derives the match positions; refresh both
                // the cached full match set AND the current-match shadow from the
                // bar so the painted bands track the new layout (previously only
                // updated on Navigate).
                let st = bar.read(cx).state();
                self.find_matches = st.matches().to_vec();
                self.find_match = st.current_match();
            }
        }
    }

    // ── Action handlers (editor-surface.md §10) ──

    fn action_tab(&mut self, _: &EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_to_next_field(false, window, cx);
    }
    fn action_tab_prev(&mut self, _: &EditorTabPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_to_next_field(true, window, cx);
    }
    fn action_escape(&mut self, _: &EditorEscape, window: &mut Window, cx: &mut Context<Self>) {
        // Two-stage Esc (§10): close the find bar first, then drop byte selection,
        // else clear node selection. An active edit is cancelled by the field's own
        // Esc binding.
        if self.find_bar.is_some() {
            self.close_find_bar(cx);
            return;
        }
        if self.editing.is_some() {
            // Cancel the active edit without writing.
            self.editing = None;
            cx.notify();
            return;
        }
        if self.byte_sel.is_active() {
            self.byte_sel.clear();
            cx.notify();
            return;
        }
        let _ = window;
        self.controller.clear_selection();
        self.after_mutation(cx);
    }
    fn action_undo(&mut self, _: &EditorUndo, _window: &mut Window, cx: &mut Context<Self>) {
        self.undo(cx);
    }
    fn action_redo(&mut self, _: &EditorRedo, _window: &mut Window, cx: &mut Context<Self>) {
        self.redo(cx);
    }

    // ── Find bar (Ctrl+F, items 4/91/92) ──

    fn action_find(&mut self, _: &EditorFind, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find_bar(window, cx);
    }

    /// Mount the [`FindBar`] over the editor (item 4). Builds it over the current
    /// rendered line texts, subscribes to [`FindEvent`] (Navigate → scroll +
    /// highlight, Close → dismiss), focuses the input, and toggles it shut if it is
    /// already open. Without this the Ctrl+F binding fired but no bar existed.
    fn open_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Item 33: Ctrl+F ALWAYS shows + selects-all — it does NOT toggle closed
        // (the C++ `showFindBar` re-focuses + selectAll on every press). If the bar
        // is already open, just re-focus its input and keep the query.
        if let Some(bar) = self.find_bar.clone() {
            let focus = bar.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            cx.notify();
            return;
        }
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let lines = self.current_line_texts();
        let bar = cx.new(|cx| FindBar::new(lines, window, cx));
        // Item 33: resume the persisted query (preserves the search across
        // hide/show) instead of opening blank.
        if !self.last_find_query.is_empty() {
            let q = self.last_find_query.clone();
            bar.update(cx, |b, cx| b.set_query(&q, window, cx));
            let st = bar.read(cx).state();
            self.find_matches = st.matches().to_vec();
            self.find_match = st.current_match();
        }
        let focus = bar.read(cx).focus_handle(cx);
        self._find_bar_sub = Some(cx.subscribe_in(
            &bar,
            window,
            move |this, _b, ev: &FindEvent, _window, cx| match ev {
                FindEvent::Navigate(m) => {
                    this.find_match = Some(*m);
                    // Item 31: refresh the cached full match set so every hit paints.
                    if let Some(bar) = this.find_bar.as_ref() {
                        this.find_matches = bar.read(cx).state().matches().to_vec();
                    }
                    // Scroll the matched line into view + repaint the highlight.
                    this.scroll.scroll_to_item(m.line, ScrollStrategy::Center);
                    cx.notify();
                }
                FindEvent::Close => {
                    this.close_find_bar(cx);
                }
            },
        ));
        self.find_bar = Some(bar);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn close_find_bar(&mut self, cx: &mut Context<Self>) {
        // Item 33: remember the query so a later Ctrl+F resumes the search.
        if let Some(bar) = self.find_bar.as_ref() {
            self.last_find_query = bar.read(cx).query().to_string();
        }
        self.find_bar = None;
        self._find_bar_sub = None;
        // Item 14: KEEP the IND_FIND highlights + the current/all match set after
        // the bar hides (the C++ `hideFindBar` deliberately preserves them and
        // `m_findPos` so the user still sees the hits and can resume; editor.cpp:1771).
        // Only a fresh query clears them. (Previously this cleared `find_match` /
        // `find_matches`, contradicting the bar's own on_close preservation comment.)
        cx.notify();
    }

    /// The rendered text of every composed line (the find bar searches these — the
    /// same strings the rows paint, including the live command-row substitution).
    fn current_line_texts(&self) -> Vec<String> {
        let count = self.controller.last_result().meta.len();
        (0..count).map(|i| self.line_text_owned(i)).collect()
    }

    // ── Normal-mode quick keys (editor.cpp `handleNormalKey`, item 12) ──

    /// The `(line, LineMeta)` of the current node — the row under the CARET
    /// (`currentNodeIndex` reads `m_sci->getCursorPosition`, editor.cpp:1742), so
    /// caret-targeted ops (P/F/S/U/1-5/Space/Left-Right type-cycle, Ctrl+Shift+Up/
    /// Down move, F2 rename, T type-edit, Enter value-edit, F12 go-to-def) act on
    /// the *caret* row rather than `first_selected_line` (raw gap 6 — the latter
    /// returns the FIRST occurrence of a node_id, which is wrong once ref-expanded
    /// children reuse the definition's node_ids). Falls back to the first selected
    /// line when no caret is set (initial state). Skips chrome rows. `None` when no
    /// node is current.
    fn current_node(&self) -> Option<(usize, LineMeta)> {
        let line = self
            .caret_data_line()
            .or_else(|| self.first_selected_line())?;
        let lm = self.line_meta(line)?.clone();
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return None;
        }
        Some((line, lm))
    }

    /// The caret line, validated as a real navigable data row against the current
    /// meta (node_idx >= 0, node_id != 0, not the command row, not a continuation/
    /// footer). `None` when the caret is unset or now points at a chrome row (a
    /// recompose may have shifted lines under it). Used as the primary anchor for
    /// caret-targeted ops + arrow navigation (the C++ cursor line).
    fn caret_data_line(&self) -> Option<usize> {
        let line = self.caret_line?;
        let lm = self.line_meta(line)?;
        if lm.node_idx < 0
            || lm.node_id == 0
            || lm.node_id == K_COMMAND_ROW_ID
            || lm.is_continuation
            || lm.line_kind == LineKind::Footer
        {
            return None;
        }
        Some(line)
    }

    /// Change the current node's kind via the controller (the `quickTypeChange`
    /// helper the P/F/S/U/1-5/Space handlers funnel through). Mirrors the C++
    /// `quickTypeChangeRequested` handler (controller.cpp:665):
    ///   * Item 3: when >1 real nodes are selected, apply to EVERY selected node
    ///     via `batch_change_kind` and return (no hex-join in the multi case).
    ///   * Item 4: single node, hex→bigger-hex routes through `join_hex_nodes`
    ///     (absorbs the following hex sibling(s), net size unchanged) rather than
    ///     `change_node_kind` (which would grow + shift siblings down). Shrink and
    ///     non-hex conversions keep `change_node_kind`.
    fn quick_change_kind(&mut self, new_kind: NodeKind, cx: &mut Context<Self>) {
        let Some((_line, lm)) = self.current_node() else {
            return;
        };
        // Multi-selection: batch over every selected real node.
        if self.controller.selected_ids().len() > 1 {
            let idxs = self.selected_node_indices_ordered();
            if idxs.len() > 1 {
                self.controller.batch_change_kind(&idxs, new_kind);
                self.apply_document(cx);
                return;
            }
        }
        // Single node.
        let node_idx = lm.node_idx as usize;
        let cur_kind = lm.node_kind;
        if is_hex_preview(new_kind) && is_hex_preview(cur_kind) {
            let cur_sz = crate::core::size_for_kind(cur_kind);
            let tgt_sz = crate::core::size_for_kind(new_kind);
            if tgt_sz > cur_sz {
                // Grow: consume adjacent hex sibling(s) to fill the target size.
                let node_id = lm.node_id;
                self.controller.join_hex_nodes(node_id, new_kind);
                self.apply_document(cx);
                return;
            }
            // Shrink / same: changeNodeKind (inserts hex padding for freed bytes).
        }
        self.controller.change_node_kind(node_idx, new_kind);
        self.apply_document(cx);
    }

    fn action_quick_pointer(
        &mut self,
        _: &EditorQuickPointer,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // P → pointer (size ≥ 4): 8-byte → Pointer64, else Pointer32 (editor.cpp).
        if let Some((_l, lm)) = self.current_node() {
            let sz = crate::core::size_for_kind(lm.node_kind);
            if sz < 4 {
                return;
            }
            let target = if sz >= 8 {
                NodeKind::Pointer64
            } else {
                NodeKind::Pointer32
            };
            self.quick_change_kind(target, cx);
        }
    }

    fn action_quick_float(
        &mut self,
        _: &EditorQuickFloat,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            match crate::core::size_for_kind(lm.node_kind) {
                4 => self.quick_change_kind(NodeKind::Float, cx),
                8 => self.quick_change_kind(NodeKind::Double, cx),
                _ => {}
            }
        }
    }

    fn action_quick_signed(
        &mut self,
        _: &EditorQuickSigned,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            let target = match crate::core::size_for_kind(lm.node_kind) {
                1 => NodeKind::Int8,
                2 => NodeKind::Int16,
                4 => NodeKind::Int32,
                8 => NodeKind::Int64,
                _ => return,
            };
            if target != lm.node_kind {
                self.quick_change_kind(target, cx);
            }
        }
    }

    fn action_quick_unsigned(
        &mut self,
        _: &EditorQuickUnsigned,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            let target = match crate::core::size_for_kind(lm.node_kind) {
                1 => NodeKind::UInt8,
                2 => NodeKind::UInt16,
                4 => NodeKind::UInt32,
                8 => NodeKind::UInt64,
                _ => return,
            };
            if target != lm.node_kind {
                self.quick_change_kind(target, cx);
            }
        }
    }

    /// The 4-step hex cycle used by Space/Shift+Space.
    const HEX_CYCLE: [NodeKind; 4] = [
        NodeKind::Hex8,
        NodeKind::Hex16,
        NodeKind::Hex32,
        NodeKind::Hex64,
    ];

    fn hex_cycle(&mut self, dir: i32, cx: &mut Context<Self>) {
        let Some((_l, lm)) = self.current_node() else {
            return;
        };
        let sz = crate::core::size_for_kind(lm.node_kind);
        if sz <= 0 {
            return; // containers
        }
        // Non-hex node: convert to the hex of the same size first.
        if !is_hex_preview(lm.node_kind) {
            if let Some(hk) = Self::HEX_CYCLE
                .iter()
                .find(|&&hk| crate::core::size_for_kind(hk) == sz)
            {
                self.quick_change_kind(*hk, cx);
            }
            return;
        }
        let Some(cur) = Self::HEX_CYCLE.iter().position(|&k| k == lm.node_kind) else {
            return; // hex128 / unknown — not in the 4-cycle
        };
        let n = Self::HEX_CYCLE.len() as i32;
        let next = Self::HEX_CYCLE[(((cur as i32 + dir) % n + n) % n) as usize];
        self.quick_change_kind(next, cx);
    }

    fn action_hex_cycle_next(
        &mut self,
        _: &EditorHexCycleNext,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hex_cycle(1, cx);
    }
    fn action_hex_cycle_prev(
        &mut self,
        _: &EditorHexCyclePrev,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hex_cycle(-1, cx);
    }

    /// 1-5 → Hex8/16/32/64/128 on any non-container node.
    fn hex_size(&mut self, kind: NodeKind, cx: &mut Context<Self>) {
        if let Some((_l, lm)) = self.current_node() {
            if crate::core::size_for_kind(lm.node_kind) > 0 {
                self.quick_change_kind(kind, cx);
            }
        }
    }
    fn action_hex8(&mut self, _: &EditorHex8, _w: &mut Window, cx: &mut Context<Self>) {
        self.hex_size(NodeKind::Hex8, cx);
    }
    fn action_hex16(&mut self, _: &EditorHex16, _w: &mut Window, cx: &mut Context<Self>) {
        self.hex_size(NodeKind::Hex16, cx);
    }
    fn action_hex32(&mut self, _: &EditorHex32, _w: &mut Window, cx: &mut Context<Self>) {
        self.hex_size(NodeKind::Hex32, cx);
    }
    fn action_hex64(&mut self, _: &EditorHex64, _w: &mut Window, cx: &mut Context<Self>) {
        self.hex_size(NodeKind::Hex64, cx);
    }
    fn action_hex128(&mut self, _: &EditorHex128, _w: &mut Window, cx: &mut Context<Self>) {
        self.hex_size(NodeKind::Hex128, cx);
    }

    /// Navigate to the next/prev selectable data node from the current line, in
    /// `dir` (±1). Skips chrome + continuation rows. On a forward walk off the end,
    /// auto-appends a hex field to the last node's struct (the "+1" keyboard
    /// affordance). Selects the landed node via `handle_node_click`.
    fn navigate_node(&mut self, dir: i32, step: usize, cx: &mut Context<Self>) {
        self.navigate_node_mode(dir, step, false, cx);
    }

    /// `navigate_node` with an explicit `page` flag. For plain arrows (`page ==
    /// false`) `step` is a node-skip count and the scan walks `start + dir*step`
    /// then continues in `dir` to the next navigable node — which descends INTO an
    /// expanded nested class/struct's child rows exactly as the C++ cursor walk does
    /// (BUG #2: the child LineMeta sit between the header and footer, so a forward
    /// scan from the header lands on the first child). For page nav (`page ==
    /// true`) `step` is a screenful of LINES: the C++ computes `target = clamp(line +
    /// dir*linesOnScreen)` then linear-scans from `target` in `dir` to the nearest
    /// navigable node (raw gap 9), so the page step lands correctly even when the
    /// screenful spans collapsed/continuation/footer rows.
    fn navigate_node_mode(&mut self, dir: i32, step: usize, page: bool, cx: &mut Context<Self>) {
        // Keyboard navigation clears any open hover popup/band (item 9 / the C++
        // dismissAllPopups on caret move).
        self.clear_hover_state(cx);
        let result = self.controller.last_result();
        let count = result.meta.len();
        if count == 0 {
            return;
        }
        // Anchor on the CARET line first (the C++ `getCursorPosition`), then the
        // primary selection; this descends into ref-expanded children whose
        // node_ids repeat the definition's (BUG #2 / raw gap 12).
        let start = self
            .caret_data_line()
            .or_else(|| self.first_selected_line())
            .map(|l| l as i64)
            .unwrap_or(if dir > 0 { 0 } else { count as i64 });
        // Page nav: jump a screenful of LINES to a clamped target, then scan from
        // there in `dir` for the nearest node. Plain nav: scan `start + dir*step`.
        let mut i = if page {
            (start + dir as i64 * step.max(1) as i64).clamp(0, count as i64 - 1)
        } else {
            start + dir as i64 * step.max(1) as i64
        };
        let mut found: Option<(usize, u64)> = None;
        while i >= 0 && (i as usize) < count {
            let lm = &result.meta[i as usize];
            // The closing footer row carries the struct id but is NOT a navigable
            // field — skip it so Down past the last field falls off the end and
            // GROWS (rather than parking the cursor on the footer, which made the
            // next Down navigate instead of append: the "press Down twice to
            // re-expand" bug). The cursor then chases the freshly-appended field.
            if lm.node_id != 0
                && lm.node_id != K_COMMAND_ROW_ID
                && lm.line_kind != LineKind::Footer
                && !lm.is_continuation
            {
                found = Some((i as usize, lm.node_id));
                break;
            }
            i += dir as i64;
        }
        if let Some((line, node_id)) = found {
            self.controller
                .handle_node_click(line as i64, node_id, CtrlMods::NONE);
            self.caret_line = Some(line);
            // Item 10: scroll MINIMALLY into view (the C++ `ensureLineVisible`) — no
            // re-centering on every arrow key.
            self.ensure_line_visible(line);
            self.after_mutation(cx);
            return;
        }
        // Forward walk fell off the end → auto-append ONE Hex64 field to the
        // ENCLOSING container of the last visible data row (C++
        // `appendSingleFieldRequested` → `append_single_field`). The handler
        // walks the last row's leaf id UP to its Struct/Array/Enum container,
        // appends a Hex64 at the container's aligned tail (so the struct visibly
        // grows past an array/struct-tail child, the array-end case), or appends
        // an auto-numbered enum member, then MOVES the selection to the new node.
        // Plain Up-at-top (dir < 0) is a silent no-op.
        if dir > 0 {
            // Item 25: pass the last visible LEAF's OWN id to `append_single_field`
            // (controller.rs:1835), which ALREADY walks up to the enclosing
            // Struct/Array/Enum container. The prior code pre-walked to the leaf's
            // PARENT, which appended as a SIBLING after a container-tail row instead
            // of INSIDE the container the C++ targets (`appendSingleFieldRequested(
            // lm.nodeId)` with the leaf's own id). With no last row, fall back to
            // the view root.
            let last_node_id = self
                .controller
                .last_result()
                .meta
                .iter()
                .rev()
                .find(|lm| {
                    lm.node_id != 0
                        && lm.node_id != K_COMMAND_ROW_ID
                        && lm.line_kind != LineKind::Footer
                        && !lm.is_continuation
                })
                .map(|lm| lm.node_id);
            let view_root = self.controller.view_root_id();
            let target = last_node_id.unwrap_or(view_root);
            if target != 0 {
                if let Some(new_id) = self.controller.append_single_field(target) {
                    self.apply_document(cx);
                    // Scroll to the freshly-selected new field's line so the cursor
                    // chases the new tail (the next Down grows again).
                    if let Some(line) = self
                        .controller
                        .last_result()
                        .meta
                        .iter()
                        .position(|lm| lm.node_id == new_id && !lm.is_continuation)
                    {
                        self.scroll.scroll_to_item(line, ScrollStrategy::Center);
                    }
                } else {
                    self.apply_document(cx);
                }
            }
        }
    }

    fn action_nav_up(&mut self, _: &EditorNavUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.navigate_node(-1, 1, cx);
    }
    fn action_nav_down(&mut self, _: &EditorNavDown, _w: &mut Window, cx: &mut Context<Self>) {
        self.navigate_node(1, 1, cx);
    }

    fn action_nav_add_up(&mut self, _: &EditorNavAddUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.navigate_node_add(-1, cx);
    }
    fn action_nav_add_down(
        &mut self,
        _: &EditorNavAddDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_add(1, cx);
    }

    /// Item 12: Ctrl+Up/Ctrl+Down additive navigation — move the caret to the next
    /// navigable node in `dir` and TOGGLE it into the multi-selection (the C++
    /// `nodeClicked(.., ControlModifier)` keyboard path), rather than replacing the
    /// selection. Does not auto-append at the end.
    fn navigate_node_add(&mut self, dir: i32, cx: &mut Context<Self>) {
        self.clear_hover_state(cx);
        let count = self.controller.last_result().meta.len();
        if count == 0 {
            return;
        }
        let start = self
            .caret_data_line()
            .or_else(|| self.first_selected_line())
            .map(|l| l as i64)
            .unwrap_or(if dir > 0 { 0 } else { count as i64 });
        let mut i = start + dir as i64;
        let mut found: Option<(usize, u64)> = None;
        while i >= 0 && (i as usize) < count {
            let lm = &self.controller.last_result().meta[i as usize];
            if lm.node_id != 0
                && lm.node_id != K_COMMAND_ROW_ID
                && lm.line_kind != LineKind::Footer
                && !lm.is_continuation
            {
                found = Some((i as usize, lm.node_id));
                break;
            }
            i += dir as i64;
        }
        if let Some((line, node_id)) = found {
            self.controller.handle_node_click(
                line as i64,
                node_id,
                CtrlMods {
                    ctrl: true,
                    shift: false,
                },
            );
            self.caret_line = Some(line);
            self.ensure_line_visible(line);
            self.after_mutation(cx);
        }
    }

    /// Ctrl+Shift+Up — reorder the active field one slot up among its siblings.
    fn action_move_up(&mut self, _: &EditorMoveUp, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some((_l, lm)) = self.current_node() {
            self.controller.move_node(lm.node_idx as usize, -1);
            self.apply_document(cx);
        }
    }

    /// Ctrl+Shift+Down — reorder the active field one slot down among its siblings.
    fn action_move_down(&mut self, _: &EditorMoveDown, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some((_l, lm)) = self.current_node() {
            self.controller.move_node(lm.node_idx as usize, 1);
            self.apply_document(cx);
        }
    }
    fn action_nav_page_up(&mut self, _: &EditorNavPageUp, _w: &mut Window, cx: &mut Context<Self>) {
        // Item 11: page nav jumps a screenful of LINES then snaps to the nearest
        // node (page mode), not a node-skip count.
        self.navigate_node_mode(-1, self.page_step(), true, cx);
    }
    fn action_nav_page_down(
        &mut self,
        _: &EditorNavPageDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_mode(1, self.page_step(), true, cx);
    }

    /// Item 10: scroll `line` MINIMALLY into view (the C++ `ensureLineVisible`):
    /// only scroll when the target row is OUTSIDE the current viewport, and then
    /// just to the nearest edge (Top when above, Bottom when below) rather than
    /// re-centering on every keypress. When the row is already on-screen this is a
    /// no-op so the viewport stays put.
    fn ensure_line_visible(&self, line: usize) {
        let (first, last) = self.visible_line_range();
        // Before the first layout (no measured range) fall back to a top scroll so
        // the row is at least brought on-screen.
        if last <= first {
            self.scroll.scroll_to_item(line, ScrollStrategy::Top);
            return;
        }
        if line < first {
            self.scroll.scroll_to_item(line, ScrollStrategy::Top);
        } else if line > last {
            self.scroll.scroll_to_item(line, ScrollStrategy::Bottom);
        }
        // Already visible → leave the viewport untouched (the C++ ensureLineVisible
        // does nothing when the line is in range).
    }

    /// The inclusive `(first_visible, last_visible)` row range from the scroll
    /// handle's measured viewport. `(0, 0)` before the first layout (caller treats
    /// an empty range as "unknown → bring on-screen").
    fn visible_line_range(&self) -> (usize, usize) {
        let state = self.scroll.0.borrow();
        let view_h = f32::from(state.base_handle.bounds().size.height);
        let offset_y = f32::from(state.base_handle.offset().y);
        if self.metrics.line_height <= 0.0 || view_h <= 0.0 {
            return (0, 0);
        }
        // The list scrolls content UP by a negative offset; first visible row is
        // floor(-offset / line_height).
        let first = ((-offset_y).max(0.0) / self.metrics.line_height).floor() as usize;
        let rows = (view_h / self.metrics.line_height).floor().max(1.0) as usize;
        (first, first + rows.saturating_sub(1))
    }

    /// Item 9: clear the hover band + node id and dismiss any open hover popup
    /// (called on keyboard nav and when the pointer leaves the viewport). The C++
    /// caret-move / mouse-leave path runs `dismissAllPopups` + drops the hover line.
    fn clear_hover_state(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if self.hovered_line.is_some() {
            self.hovered_line = None;
            changed = true;
        }
        if self.hovered_node_id != 0 {
            self.hovered_node_id = 0;
            changed = true;
        }
        if self.hover_popup.is_some() {
            self.hover_popup = None;
            changed = true;
        }
        // Item 13: a full hover clear (viewport leave) also drops the
        // cursor-inside-popup guard so a stale flag can't suppress the next popup.
        self.popup_cursor_inside = false;
        if changed {
            cx.notify();
        }
    }

    /// One screenful of rows for PageUp/Down (the measured view height / line
    /// height; falls back to 20 before the first layout).
    fn page_step(&self) -> usize {
        let state = self.scroll.0.borrow();
        let view_h = f32::from(state.base_handle.bounds().size.height);
        if self.metrics.line_height > 0.0 && view_h > 0.0 {
            ((view_h / self.metrics.line_height).floor() as usize).max(1)
        } else {
            20
        }
    }

    fn action_nav_home(&mut self, _: &EditorNavHome, _w: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_bound(false, false, cx);
    }

    fn action_nav_end(&mut self, _: &EditorNavEnd, _w: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_bound(true, false, cx);
    }

    /// Home/End (and the Shift-extending variants). `to_end` picks the last data
    /// node (excluding footers) vs the first; `extend` ranges the selection from
    /// the anchor instead of replacing it (item 3). Mirrors editor.cpp `Key_Home`/
    /// `Key_End` (which pass `NoModifier`) plus the Shift+Home/End extension.
    fn jump_to_bound(&mut self, to_end: bool, extend: bool, cx: &mut Context<Self>) {
        self.clear_hover_state(cx);
        let result = self.controller.last_result();
        let n = result.meta.len();
        let indices: Box<dyn Iterator<Item = usize>> = if to_end {
            Box::new((0..n).rev())
        } else {
            Box::new(0..n)
        };
        for i in indices {
            let lm = &result.meta[i];
            if lm.node_id != 0
                && lm.node_id != K_COMMAND_ROW_ID
                && !lm.is_continuation
                && lm.line_kind != LineKind::Footer
            {
                let node_id = lm.node_id;
                let mods = if extend {
                    CtrlMods {
                        ctrl: false,
                        shift: true,
                    }
                } else {
                    CtrlMods::NONE
                };
                self.controller.handle_node_click(i as i64, node_id, mods);
                self.caret_line = Some(i);
                // Item 10: minimal scroll (the C++ `ensureLineVisible`).
                self.ensure_line_visible(i);
                self.after_mutation(cx);
                return;
            }
        }
    }

    /// Shift+arrow / Shift+page navigation (items 2/3): walk `dir * step` from the
    /// moving caret to the next navigable node and EXTEND the selection to it (the
    /// C++ `nodeClicked(.., ShiftModifier)` keyboard path) rather than replacing it.
    /// Does NOT auto-append a field at the end (that is the plain-Down affordance).
    fn navigate_node_extend(&mut self, dir: i32, step: usize, cx: &mut Context<Self>) {
        self.clear_hover_state(cx);
        let count = self.controller.last_result().meta.len();
        if count == 0 {
            return;
        }
        // Start from the moving caret if we have one, else the primary selection.
        let start = self
            .caret_data_line()
            .or_else(|| self.first_selected_line())
            .map(|l| l as i64)
            .unwrap_or(if dir > 0 { 0 } else { count as i64 });
        let mut i = start + dir as i64 * step.max(1) as i64;
        // Clamp into range so a big page-step still lands on the nearest node.
        if i < 0 {
            i = 0;
        }
        if i as usize >= count {
            i = count as i64 - 1;
        }
        let mut found: Option<(usize, u64)> = None;
        // Search toward the bound from the (clamped) target; if the clamped row
        // is not navigable, walk back toward the caret.
        let probe_dir = if dir > 0 { 1 } else { -1 };
        let mut j = i;
        while j >= 0 && (j as usize) < count {
            let lm = &self.controller.last_result().meta[j as usize];
            if lm.node_id != 0
                && lm.node_id != K_COMMAND_ROW_ID
                && lm.line_kind != LineKind::Footer
                && !lm.is_continuation
            {
                found = Some((j as usize, lm.node_id));
                break;
            }
            j -= probe_dir as i64;
            // Don't walk past the caret origin.
            if (probe_dir > 0 && j < start) || (probe_dir < 0 && j > start) {
                break;
            }
        }
        if let Some((line, node_id)) = found {
            self.controller.handle_node_click(
                line as i64,
                node_id,
                CtrlMods {
                    ctrl: false,
                    shift: true,
                },
            );
            self.caret_line = Some(line);
            // Item 10: minimal scroll (the C++ `ensureLineVisible`).
            self.ensure_line_visible(line);
            self.after_mutation(cx);
        }
    }

    fn action_select_up(&mut self, _: &EditorSelectUp, _w: &mut Window, cx: &mut Context<Self>) {
        self.navigate_node_extend(-1, 1, cx);
    }
    fn action_select_down(
        &mut self,
        _: &EditorSelectDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(1, 1, cx);
    }
    fn action_select_page_up(
        &mut self,
        _: &EditorSelectPageUp,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(-1, self.page_step(), cx);
    }
    fn action_select_page_down(
        &mut self,
        _: &EditorSelectPageDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(1, self.page_step(), cx);
    }
    fn action_select_home(
        &mut self,
        _: &EditorSelectHome,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.jump_to_bound(false, true, cx);
    }
    fn action_select_end(&mut self, _: &EditorSelectEnd, _w: &mut Window, cx: &mut Context<Self>) {
        self.jump_to_bound(true, true, cx);
    }

    /// Ctrl+A — select all sibling nodes of the current node (item 5). Mirrors
    /// editor.cpp `Key_A`: plain-click the first navigable data row to seed the
    /// anchor, then Shift-click the last to range-select everything between
    /// (continuation/footer/command rows are skipped by `insert_range`/click).
    fn action_select_all(&mut self, _: &EditorSelectAll, _w: &mut Window, cx: &mut Context<Self>) {
        let result = self.controller.last_result();
        let mut first: Option<(usize, u64)> = None;
        let mut last: Option<(usize, u64)> = None;
        for (i, lm) in result.meta.iter().enumerate() {
            if lm.node_id == 0
                || lm.node_id == K_COMMAND_ROW_ID
                || lm.is_continuation
                || lm.line_kind == LineKind::Footer
            {
                continue;
            }
            if first.is_none() {
                first = Some((i, lm.node_id));
            }
            last = Some((i, lm.node_id));
        }
        let (Some((fi, fid)), Some((li, lid))) = (first, last) else {
            return;
        };
        self.controller
            .handle_node_click(fi as i64, fid, CtrlMods::NONE);
        if li != fi {
            self.controller.handle_node_click(
                li as i64,
                lid,
                CtrlMods {
                    ctrl: false,
                    shift: true,
                },
            );
        }
        self.caret_line = Some(li);
        self.after_mutation(cx);
    }

    // ── Node clipboard (item 4) ──

    /// The set of selected `node_idx` in stable offset/declaration order (so a
    /// copy preserves the on-screen ordering of a multi-selection). Built from the
    /// composed meta (which is already in display order) intersected with the
    /// controller's selection set.
    fn selected_node_indices_ordered(&self) -> Vec<usize> {
        let result = self.controller.last_result();
        let sel = self.controller.selected_ids();
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for lm in result.meta.iter() {
            if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
                continue;
            }
            if !seen.insert(lm.node_id) {
                continue;
            }
            if sel
                .iter()
                .any(|&id| crate::controller::strip_sel_pub(id) == lm.node_id)
            {
                out.push(lm.node_idx as usize);
            }
        }
        out
    }

    /// The selected node IDS, in display order, with selection bits stripped — the
    /// C++ `selectedRootIds` lambda (controller.cpp:511). Each is a real tree id
    /// (de-duplicated, skips the command row / array-elem / member synthetics).
    fn selected_root_ids(&self) -> Vec<u64> {
        let result = self.controller.last_result();
        let sel = self.controller.selected_ids();
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for lm in result.meta.iter() {
            if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
                continue;
            }
            if !seen.insert(lm.node_id) {
                continue;
            }
            if sel
                .iter()
                .any(|&id| crate::controller::strip_sel_pub(id) == lm.node_id)
            {
                out.push(lm.node_id);
            }
        }
        out
    }

    /// Serialize the selected nodes (PLUS their whole subtrees) into the portable
    /// `application/x-reclass-nodes-v1` blob via the faithful core codec
    /// ([`core::clipboard::serialize`], the C++ `ClipboardCodec::serialize`,
    /// clipboard.h:58). Unlike the old per-node flatten, this collects every
    /// descendant of each selected root (so struct/array/pointer subtrees keep
    /// their contents) and clears the parent link only on the selected roots
    /// (`clear_parent_for`) so they re-anchor cleanly under the paste target.
    /// Returns `None` when nothing node-like is selected.
    ///
    /// Returns both the portable JSON blob AND the human-readable plain-text dump
    /// (e.g. `+0x08 uint32_t health`). Item 32: the C++ puts the
    /// readable listing on the plain-text clipboard path and the JSON under an
    /// `application/x-reclass-nodes` MIME type, so pasting into a text editor
    /// shows the dump rather than the raw JSON blob.
    fn serialize_selected_nodes_full(&self) -> Option<(String, String)> {
        let roots = self.selected_root_ids();
        if roots.is_empty() {
            return None;
        }
        let clear_parent_for: std::collections::HashSet<u64> = roots.iter().copied().collect();
        let (bytes, plain) =
            crate::core::clipboard::serialize(self.controller.tree(), &roots, &clear_parent_for);
        if bytes.is_empty() {
            return None;
        }
        let blob = String::from_utf8(bytes).ok()?;
        Some((blob, plain))
    }

    fn action_copy_nodes(&mut self, _: &EditorCopyNodes, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some((blob, plain)) = self.serialize_selected_nodes_full() {
            self.node_clipboard = Some(blob.clone());
            // Item 32: visible text = readable dump, JSON blob as metadata.
            cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(plain, blob));
        }
    }

    fn action_cut_nodes(&mut self, _: &EditorCutNodes, _w: &mut Window, cx: &mut Context<Self>) {
        let Some((blob, plain)) = self.serialize_selected_nodes_full() else {
            return;
        };
        self.node_clipboard = Some(blob.clone());
        // Item 32: visible text = readable dump, JSON blob as metadata.
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(plain, blob));
        // Item 2 (blocker): after writing the clipboard, delete the cut nodes
        // through `batch_remove_nodes` for the multi-node case (single undo
        // macro + `normalize_prefer_ancestors`); single-node falls to
        // `remove_node`. Mirrors the corrected `action_delete`.
        let idxs = self.selected_node_indices_ordered();
        if idxs.is_empty() {
            return;
        }
        if idxs.len() > 1 {
            self.controller.batch_remove_nodes(&idxs);
        } else {
            self.controller.remove_node(idxs[0]);
        }
        self.controller.clear_selection();
        self.context_target = None;
        self.apply_document(cx);
    }

    fn action_paste_nodes(
        &mut self,
        _: &EditorPasteNodes,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Prefer the in-process blob; fall back to the system clipboard so a copy
        // from another window/instance also pastes. Item 32: the JSON blob now
        // rides on the clipboard *metadata* (visible text is the readable dump),
        // so read metadata first and only fall back to the text payload (older
        // copies / a raw blob pasted as text).
        let blob = self.node_clipboard.clone().or_else(|| {
            cx.read_from_clipboard().and_then(|item| {
                item.metadata()
                    .cloned()
                    .or_else(|| item.text().map(|t| t.to_string()))
            })
        });
        let Some(blob) = blob else {
            return;
        };
        // Faithful core decode (the C++ `ClipboardCodec::deserialize`,
        // clipboard.h:97): parse the `application/x-reclass-nodes-v1` blob, remap
        // every id/parent/ref across the WHOLE captured subtree to fresh
        // non-colliding ids, and return the new root ids. This is what makes a
        // pasted struct/array/pointer keep its children (the old flat path dropped
        // them, set `collapsed=true`, and never re-wired the subtree).
        let paste =
            crate::core::clipboard::deserialize(self.controller.tree_mut(), blob.as_bytes());
        if paste.nodes.is_empty() {
            return;
        }
        let root_set: std::collections::HashSet<u64> = paste.root_ids.iter().copied().collect();

        // Paste-below-selection (controller.cpp:558): drop the pasted roots right
        // after the selected node with the greatest end offset, pushing later
        // siblings down. With nothing selected, fall back to append-at-end.
        let mut target_parent = self.controller.view_root_id();
        let mut anchor_end: i32 = -1;
        for nid in self.selected_root_ids() {
            let tree = self.controller.tree();
            let ai = tree.index_of_id(nid);
            if ai < 0 {
                continue;
            }
            let a = &tree.nodes[ai as usize];
            let asz = if crate::core::is_container_kind(a.kind) {
                tree.struct_span(a.id)
            } else {
                crate::core::size_for_kind(a.kind).max(0)
            };
            let end = a.offset + asz;
            if end > anchor_end {
                anchor_end = end;
                target_parent = a.parent_id;
            }
        }
        if target_parent == 0 {
            return;
        }

        // Total span the pasted roots will occupy (with inter-root alignment), so
        // we know how far to shift existing siblings (controller.cpp:601).
        let mut paste_total = 0i32;
        for &r in &paste.root_ids {
            if let Some(n) = paste.nodes.iter().find(|n| n.id == r) {
                let align = crate::core::alignment_for(n.kind);
                paste_total =
                    (paste_total + align - 1) / align * align + Self::pasted_span(&paste.nodes, r);
            }
        }

        // Shift existing siblings at/after the anchor down by `paste_total`, on the
        // FIRST root's Insert command (so one undo reverses the whole paste).
        let mut shift: Vec<crate::core::OffsetAdj> = Vec::new();
        if anchor_end >= 0 && paste_total > 0 {
            let tree = self.controller.tree();
            for si in tree.children_of(target_parent) {
                let s = &tree.nodes[si];
                if s.offset >= anchor_end {
                    shift.push(crate::core::OffsetAdj {
                        node_id: s.id,
                        old_offset: s.offset,
                        new_offset: s.offset + paste_total,
                    });
                }
            }
        }

        // One undo macro for the whole paste (the C++ beginMacro/endMacro group).
        self.controller.begin_macro("Paste nodes");
        let mut placed_base = anchor_end; // -1 ⇒ append-at-end fallback
        let mut first_root = true;
        // `paste.nodes` carries the whole subtree; placing only re-anchors the
        // ROOTS (their captured children keep their relative offsets + remapped
        // parent links, so the subtree re-wires itself on insert).
        for n in &paste.nodes {
            let mut node = n.clone();
            if root_set.contains(&node.id) {
                node.parent_id = target_parent;
                let align = crate::core::alignment_for(node.kind);
                if placed_base >= 0 {
                    node.offset = (placed_base + align - 1) / align * align;
                    placed_base = node.offset + Self::pasted_span(&paste.nodes, n.id);
                } else {
                    // Append path: after all current siblings of the target parent.
                    let max_end = self.container_tail(target_parent);
                    node.offset = (max_end + align - 1) / align * align;
                }
            }
            let off_adjs = if first_root && root_set.contains(&n.id) {
                first_root = false;
                std::mem::take(&mut shift)
            } else {
                Vec::new()
            };
            self.controller
                .push_command(crate::core::Command::Insert { node, off_adjs });
        }
        self.controller.end_macro();
        self.apply_document(cx);
    }

    /// The byte span a to-be-pasted node will occupy, computed from the deserialized
    /// `nodes` list (NOT yet in the tree) — the C++ `pastedSpan` lambda
    /// (controller.cpp:582). Struct/array roots recurse over their captured
    /// children (max child end); leaves use their kind size.
    fn pasted_span(nodes: &[crate::core::Node], id: u64) -> i32 {
        let Some(n) = nodes.iter().find(|n| n.id == id) else {
            return 0;
        };
        if !crate::core::is_container_kind(n.kind) {
            return crate::core::size_for_kind(n.kind).max(0);
        }
        let mut max_end = 0i32;
        for c in nodes.iter().filter(|c| c.parent_id == id) {
            let cend = c.offset + Self::pasted_span(nodes, c.id);
            if cend > max_end {
                max_end = cend;
            }
        }
        max_end
    }

    /// The aligned tail offset of a container (max child end). Used as the paste
    /// anchor when there is no cursor node.
    fn container_tail(&self, parent_id: u64) -> i32 {
        let tree = self.controller.tree();
        tree.children_of(parent_id)
            .iter()
            .map(|&ci| {
                let c = &tree.nodes[ci];
                c.offset + crate::core::size_for_kind(c.kind).max(0)
            })
            .max()
            .unwrap_or(0)
    }

    /// Ctrl+Shift+C — copy the current node's offset address as `0x{addr:X}`
    /// (item 10, editor.cpp `Key_C` + Ctrl+Shift). No-op when no node / addr 0.
    fn action_copy_address(
        &mut self,
        _: &EditorCopyAddress,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            if lm.offset_addr != 0 {
                cx.write_to_clipboard(ClipboardItem::new_string(format!("0x{:X}", lm.offset_addr)));
            }
        }
    }

    fn action_begin_value_edit(
        &mut self,
        _: &EditorBeginValueEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((line, _lm)) = self.current_node() {
            self.begin_inline_edit(line, EditTarget::Value, window, cx);
        }
    }

    fn action_insert_hex64(
        &mut self,
        _: &EditorInsertHex64,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_field_above_or_append(NodeKind::Hex64, cx);
    }

    fn action_insert_hex32(
        &mut self,
        _: &EditorInsertHex32,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_field_above_or_append(NodeKind::Hex32, cx);
    }

    /// Item 24: `Key_Insert` always inserts (the C++ emits `insertAboveRequested`
    /// whenever the selection is non-empty; with `nodeIdx < 0` it appends a field
    /// at the end of the view-root struct). With a current node, insert ABOVE it;
    /// otherwise append into the view root. The inserted field's name is `"field"`
    /// (the C++ default), not the empty string.
    fn insert_field_above_or_append(&mut self, kind: NodeKind, cx: &mut Context<Self>) {
        if let Some((_l, lm)) = self.current_node() {
            self.controller
                .insert_node_above(lm.node_idx as usize, kind, "field");
        } else {
            let root = self.controller.view_root_id();
            if root == 0 {
                return;
            }
            self.controller.insert_node(root, -1, kind, "field");
        }
        self.apply_document(cx);
    }

    fn action_comment_edit(
        &mut self,
        _: &EditorCommentEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Item 27: the C++ `;` comment action + accelerator only exist when
        // `showComments` is on (otherwise the comment chips are invisible, so the
        // editor is a no-op).
        if !self.show_comments() {
            return;
        }
        if let Some((line, _lm)) = self.current_node() {
            self.begin_inline_edit(line, EditTarget::Comment, window, cx);
        }
    }

    /// Left/Right → cycle same-size type variants on the focused node (item 18).
    /// Reuses the menu's forward/back kind cyclers.
    fn action_cycle_left(&mut self, _: &EditorCycleLeft, _w: &mut Window, cx: &mut Context<Self>) {
        self.cycle_same_size(-1, cx);
    }
    fn action_cycle_right(
        &mut self,
        _: &EditorCycleRight,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_same_size(1, cx);
    }

    /// Shift+Left → collapse / Shift+Right → expand the current foldable node
    /// (a nested class/struct instance, a pointer-to-class, an array of structs —
    /// any row that shows a fold chevron). Plain Left/Right stay type-cycle, so
    /// the fold keys take the Shift chord.
    fn action_collapse_node(
        &mut self,
        _: &EditorCollapseNode,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fold_current(false, cx);
    }
    fn action_expand_node(
        &mut self,
        _: &EditorExpandNode,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fold_current(true, cx);
    }

    /// Collapse (`expand=false`) or expand (`expand=true`) the current node if it
    /// is a fold head, and only when that changes its state (so Shift+Left on an
    /// already-collapsed node, or Shift+Right on an expanded one, is a no-op).
    /// Mirrors the chevron-click path (`on_row_mouse_down`): ref/pointer/cycle
    /// heads materialize their referenced children on expand so they become real
    /// navigable rows; plain containers toggle. The nav anchor is seeded on the
    /// head so a following Down descends into the children.
    fn fold_current(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some((line, lm)) = self.current_node() else {
            return;
        };
        if lm.node_idx < 0 || !lm.fold_head {
            return; // leaf / non-expandable row
        }
        let node_idx = lm.node_idx as usize;
        let node_id = lm.node_id;
        let is_collapsed = self
            .controller
            .tree()
            .nodes
            .get(node_idx)
            .map(|n| n.collapsed)
            .unwrap_or(true);
        // Already in the requested state → nothing to do.
        if expand != is_collapsed {
            return;
        }
        if expand && (lm.marker_mask & (1u32 << crate::core::linemeta::M_CYCLE)) != 0 {
            self.controller.materialize_ref_children(node_idx);
        } else {
            self.controller.toggle_collapse(node_idx);
        }
        self.controller
            .handle_node_click(line as i64, node_id, CtrlMods::NONE);
        self.caret_line = Some(line);
        self.after_mutation(cx);
    }

    /// `cycleSameSizeTypeRequested` (controller.cpp:709). Item 3: apply the cycle
    /// to EVERY selected same-size node via `batch_change_kind`, not just the
    /// focused one. Item 20: rapid ←/→ presses within an 800ms window coalesce
    /// into a single "Cycle type" undo macro (the C++ `m_cycleMacroTimer`).
    fn cycle_same_size(&mut self, dir: i32, cx: &mut Context<Self>) {
        let Some((_l, lm)) = self.current_node() else {
            return;
        };
        let sz = crate::core::size_for_kind(lm.node_kind);
        if sz <= 0 {
            return; // skip Struct/Array
        }
        let target = if dir > 0 {
            alt_kind_for(lm.node_kind)
        } else {
            prev_kind_for(lm.node_kind)
        };

        // Item 20: 800ms macro-coalescing window. Consecutive presses inside the
        // window stay in the open macro; a press after the window (or the first
        // press) ends any stale macro and opens a fresh one.
        let now = std::time::Instant::now();
        let coalesce = self
            .cycle_macro_at
            .map(|t| now.duration_since(t).as_millis() <= 800)
            .unwrap_or(false);
        if !coalesce {
            if self.cycle_macro_open {
                self.controller.end_macro();
            }
            self.controller.begin_macro("Cycle type");
            self.cycle_macro_open = true;
        }
        self.cycle_macro_at = Some(now);

        // Multi-selection: cycle every selected same-size node (item 3).
        if self.controller.selected_ids().len() > 1 {
            let idxs: Vec<usize> = self
                .selected_node_indices_ordered()
                .into_iter()
                .filter(|&i| {
                    let t = self.controller.tree();
                    i < t.nodes.len() && crate::core::size_for_kind(t.nodes[i].kind) == sz
                })
                .collect();
            if idxs.len() > 1 {
                self.controller.batch_change_kind(&idxs, target);
                self.apply_document(cx);
                self.arm_cycle_macro_close(cx);
                return;
            }
        }
        self.controller
            .change_node_kind(lm.node_idx as usize, target);
        self.apply_document(cx);
        self.arm_cycle_macro_close(cx);
    }

    /// Item 20: (re)arm the deferred 800ms close of the open "Cycle type" undo
    /// macro (the C++ `m_cycleMacroTimer->start()`). Reassigning the task handle
    /// cancels any prior pending close, so a fresh press extends the window.
    fn arm_cycle_macro_close(&mut self, cx: &mut Context<Self>) {
        const WINDOW: std::time::Duration = std::time::Duration::from_millis(800);
        self._cycle_macro_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WINDOW).await;
            let _ = this.update(cx, |this, _cx| {
                // Only close if no fresh press extended the window meanwhile.
                let stale = this
                    .cycle_macro_at
                    .map(|t| t.elapsed() >= WINDOW)
                    .unwrap_or(true);
                if this.cycle_macro_open && stale {
                    this.controller.end_macro();
                    this.cycle_macro_open = false;
                    this.cycle_macro_at = None;
                }
            });
        });
    }

    /// F12 Go To Definition (item 20): resolve the focused node's referenced struct
    /// (pointer `ref_id` / struct `ref_id` / array element struct) and switch the
    /// view root to it.
    fn action_go_to_definition(
        &mut self,
        _: &EditorGoToDefinition,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_l, lm)) = self.current_node() else {
            return;
        };
        let idx = lm.node_idx as usize;
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
        // Resolve the navigation target the way the C++ `goToDefinitionRequested`
        // (controller.cpp:851) does, in order:
        //   1. a typed pointer / embedded-struct-ref carries `ref_id`,
        //   2. an Array of structs chases the array's `ref_id`,
        //   3. (item 15) a PLAIN embedded Struct field with no ref re-roots to its
        //      OWN id so the user views its subtree.
        let target = if n.ref_id != 0 {
            n.ref_id
        } else if n.kind == NodeKind::Array && n.element_kind == NodeKind::Struct && n.ref_id != 0 {
            n.ref_id
        } else if n.kind == NodeKind::Struct && n.parent_id != 0 {
            n.id
        } else {
            0
        };
        if target != 0 && tree.index_of_id(target) >= 0 {
            let ref_id = target;
            self.controller.set_view_root_id(ref_id);
            self.controller.clear_selection();
            self.apply_document(cx);
            // Item 12: don't just re-root — NAVIGATE to the definition. After the
            // recompose, find the landed struct's header row, select it, and scroll
            // it into view (the C++ smoothScrollToNodeId + setFocusNode). The view
            // root is line 0's command row; the struct's first data row is the
            // definition body, so land on the first navigable node under the new
            // root.
            let target_line = self.controller.last_result().meta.iter().position(|lm| {
                lm.node_id != 0
                    && lm.node_id != K_COMMAND_ROW_ID
                    && !lm.is_continuation
                    && lm.line_kind != LineKind::Footer
            });
            if let Some(line) = target_line {
                let node_id = self.controller.last_result().meta[line].node_id;
                self.controller
                    .handle_node_click(line as i64, node_id, CtrlMods::NONE);
                self.caret_line = Some(line);
                self.scroll.scroll_to_item(line, ScrollStrategy::Center);
                self.after_mutation(cx);
            }
        }
    }

    /// Collapse / expand every container node (item 19). Iterates the tree's
    /// container nodes and toggles those whose `collapsed` state differs from the
    /// target, via the existing `toggle_collapse` op (no dedicated bulk op).
    fn set_all_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let targets: Vec<usize> = self
            .controller
            .tree()
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| crate::core::is_container_kind(n.kind) && n.collapsed != collapsed)
            .map(|(i, _)| i)
            .collect();
        if targets.is_empty() {
            return;
        }
        // Item 13: wrap the N toggles in ONE undo macro so undoing a bulk fold takes
        // a single press (the C++ Collapse/Expand All is one undoable op), not N.
        self.controller.begin_macro(if collapsed {
            "Collapse all"
        } else {
            "Expand all"
        });
        for idx in targets {
            // Re-check by re-reading (toggle_collapse may recompose between calls,
            // but indices are stable for collapse toggles in this controller).
            if let Some(n) = self.controller.tree().nodes.get(idx) {
                if crate::core::is_container_kind(n.kind) && n.collapsed != collapsed {
                    self.controller.toggle_collapse(idx);
                }
            }
        }
        self.controller.end_macro();
        self.apply_document(cx);
    }

    fn action_collapse_all(
        &mut self,
        _: &EditorCollapseAll,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_all_collapsed(true, cx);
    }
    fn action_expand_all(&mut self, _: &EditorExpandAll, _w: &mut Window, cx: &mut Context<Self>) {
        self.set_all_collapsed(false, cx);
    }

    // ── Undo / redo (editor-surface.md §1: routed through the controller) ──

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.controller.undo();
        self.after_mutation(cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.controller.redo();
        self.after_mutation(cx);
    }

    // ── Presentation mode + focus glow + scroll-to-node (items 74/81) ──
    //
    // The public navigation/focus API the controller (and the AI/MCP layer) drives
    // for "show me node N" interactions, plus the presentation-mode chrome (smooth
    // animated scroll + a pulsing focus glow). Faithful port of the C++
    // `RcxEditor` methods (editor.cpp:1780-1913).

    /// Item 13: the effective editor font family — the user's View > Font
    /// selection, or the canonical `mono_family()` default.
    fn editor_font_family(&self) -> SharedString {
        self.font_family
            .clone()
            .unwrap_or_else(|| design::tokens::font::mono_family().into())
    }

    /// Item 13: set the editor surface font family (the window's `set_editor_font`
    /// propagates the View > Font selection into each open editor). Recomputes the
    /// cell metrics on the next frame (the render path re-measures with the new
    /// family) and repaints. `None` resets to the mono default.
    pub fn set_font_family(&mut self, family: Option<SharedString>, cx: &mut Context<Self>) {
        if self.font_family == family {
            return;
        }
        self.font_family = family;
        cx.notify();
    }

    /// Item 44: the effective editor font SIZE — the base `EDITOR_SIZE` plus the
    /// per-editor zoom delta (Ctrl+wheel / Ctrl+=/Ctrl+-), clamped to a sane range.
    fn editor_font_size(&self) -> f32 {
        (design::tokens::font::EDITOR_SIZE + self.zoom_delta).clamp(6.0, 48.0)
    }

    /// Item 44: bump the zoom delta by `points` (a Ctrl+= / Ctrl+- step). Clamped so
    /// the grid stays legible; recomputes metrics on the next frame.
    fn zoom_by(&mut self, points: f32, cx: &mut Context<Self>) {
        let base = design::tokens::font::EDITOR_SIZE;
        self.zoom_delta = (self.zoom_delta + points).clamp(6.0 - base, 48.0 - base);
        cx.notify();
    }

    fn action_zoom_in(&mut self, _: &EditorZoomIn, _w: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0, cx);
    }
    fn action_zoom_out(&mut self, _: &EditorZoomOut, _w: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(-1.0, cx);
    }
    fn action_zoom_reset(&mut self, _: &EditorZoomReset, _w: &mut Window, cx: &mut Context<Self>) {
        if self.zoom_delta != 0.0 {
            self.zoom_delta = 0.0;
            cx.notify();
        }
    }

    /// `setPresentationMode(on)` (editor.h:41) — enable smooth animated scroll +
    /// the focus-glow pulse. When off, `smooth_scroll_to_node_id` snaps instantly
    /// and the glow is not painted (the focus node is still tracked, but inert).
    pub fn set_presentation_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.presentation_mode == on {
            return;
        }
        self.presentation_mode = on;
        if !on {
            // Leaving presentation mode stops the glow pulse (the glow band is gated
            // on `presentation_mode` in render_row, but stop the timer too).
            self._focus_glow_task = Task::ready(());
        } else if self.focus_node_id != 0 {
            // Re-arm the pulse if a focus node is already set.
            self.arm_focus_glow(cx);
        }
        cx.notify();
    }

    /// Whether presentation mode is active.
    pub fn presentation_mode(&self) -> bool {
        self.presentation_mode
    }

    /// `setFocusNode(nodeId)` (editor.cpp:1863) — mark `node_id` as the AI/MCP focus
    /// node. Its row(s) pulse with the `M_FOCUS` glow (in presentation mode) via a
    /// ~30ms timer. `node_id == 0` clears the focus (same as `clear_focus_node`).
    pub fn set_focus_node(&mut self, node_id: u64, cx: &mut Context<Self>) {
        if node_id == self.focus_node_id && node_id != 0 {
            return;
        }
        self.focus_node_id = node_id;
        self.focus_glow_phase = 0;
        if node_id == 0 {
            self._focus_glow_task = Task::ready(());
        } else {
            self.arm_focus_glow(cx);
        }
        cx.notify();
    }

    /// `clearFocusNode()` (editor.cpp:1908) — stop the glow pulse + drop the focus.
    pub fn clear_focus_node(&mut self, cx: &mut Context<Self>) {
        self._focus_glow_task = Task::ready(());
        self.focus_node_id = 0;
        self.focus_glow_phase = 0;
        cx.notify();
    }

    /// `isFocusGlowActive()` (editor.h:40) — a focus node is set.
    pub fn is_focus_glow_active(&self) -> bool {
        self.focus_node_id != 0
    }

    /// Arm the ~30ms focus-glow pulse timer (the C++ `m_focusGlowTimer`,
    /// editor.cpp:1892). Each tick bumps `focus_glow_phase` and repaints so the
    /// glow band's alpha advances along the `0.5 + 0.5*sin(phase*PI/12)` curve.
    /// Self-reschedules until the focus is cleared / the entity drops.
    fn arm_focus_glow(&mut self, cx: &mut Context<Self>) {
        const GLOW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);
        self._focus_glow_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(GLOW_INTERVAL).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if this.focus_node_id == 0 || !this.presentation_mode {
                            return false; // focus cleared / left presentation — stop.
                        }
                        this.focus_glow_phase = this.focus_glow_phase.wrapping_add(1);
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
    }

    /// `scrollToNodeId(nodeId)` (editor.cpp:1780) — resolve the node's first
    /// non-footer display line and snap it into view (instant). The public
    /// non-animated navigation the controller drives. Also selects the row so the
    /// landed node is the active selection (the C++ `setCursorPosition`).
    pub fn scroll_to_node_id(&mut self, node_id: u64, cx: &mut Context<Self>) {
        let Some(line) = self.line_for_node(node_id) else {
            return;
        };
        self.scroll.scroll_to_item(line, ScrollStrategy::Center);
        self.caret_line = Some(line);
        cx.notify();
    }

    /// `smoothScrollToNodeId(nodeId)` (editor.cpp:1792) — in presentation mode,
    /// animate the scroll toward centering the node with a snap-close for long jumps
    /// then an OutExpo glide; otherwise fall back to the instant `scroll_to_node_id`.
    /// Item 23: drives the uniform-list `base_handle` scroll OFFSET per-frame via a
    /// ~16ms foreground timer with an OutExpo ease toward the target offset (the C++
    /// `QVariantAnimation` + `QEasingCurve::OutExpo`, 400ms). If the target is
    /// already on-screen it just sets the caret; if it is very far (> ~50 rows) it
    /// snaps close first (leaving the last stretch to animate).
    pub fn smooth_scroll_to_node_id(&mut self, node_id: u64, cx: &mut Context<Self>) {
        if !self.presentation_mode {
            self.scroll_to_node_id(node_id, cx);
            return;
        }
        let Some(line) = self.line_for_node(node_id) else {
            return;
        };
        self.caret_line = Some(line);

        let lh = self.metrics.line_height;
        if lh <= 0.0 {
            // No measured metrics yet — snap.
            self.scroll.scroll_to_item(line, ScrollStrategy::Center);
            cx.notify();
            return;
        }
        let (view_h, current_y, max_y) = {
            let state = self.scroll.0.borrow();
            (
                f32::from(state.base_handle.bounds().size.height),
                f32::from(state.base_handle.offset().y),
                f32::from(state.base_handle.max_offset().y),
            )
        };
        if view_h <= 0.0 {
            self.scroll.scroll_to_item(line, ScrollStrategy::Center);
            cx.notify();
            return;
        }
        // Target offset that centers the line. The list scrolls content UP via a
        // NEGATIVE y offset; offset = -(line*lh - (view_h - lh)/2), clamped to the
        // valid scroll range [-max_y, 0].
        let target_top = (line as f32) * lh - (view_h - lh) * 0.5;
        let target_y = (-target_top).clamp(-max_y.max(0.0), 0.0);

        // Already on-screen → no scroll (the C++ "already visible" branch).
        let first = ((-current_y).max(0.0) / lh).floor() as usize;
        let rows = (view_h / lh).floor().max(1.0) as usize;
        if line >= first && line < first + rows {
            cx.notify();
            return;
        }

        // Long jump: snap close (within ~30 rows of the target) then glide the rest.
        let mut start_y = current_y;
        let distance_rows = ((target_y - current_y).abs() / lh) as i64;
        if distance_rows > 50 {
            let snap = if target_y < current_y {
                target_y + 30.0 * lh
            } else {
                target_y - 30.0 * lh
            };
            start_y = snap.clamp(-max_y.max(0.0), 0.0);
            self.set_scroll_offset_y(start_y);
        }

        // Drive an OutExpo glide from start_y → target_y over ~400ms (~16ms/frame).
        const DURATION_MS: f32 = 400.0;
        const FRAME_MS: u64 = 16;
        let scroll = self.scroll.clone();
        self._scroll_anim_task = cx.spawn(async move |this, cx| {
            let begin = std::time::Instant::now();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(FRAME_MS))
                    .await;
                let elapsed = begin.elapsed().as_millis() as f32;
                let t = (elapsed / DURATION_MS).clamp(0.0, 1.0);
                // OutExpo: 1 - 2^(-10 t).
                let eased = if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f32.powf(-10.0 * t)
                };
                let y = start_y + (target_y - start_y) * eased;
                let keep = this
                    .update(cx, |this, cx| {
                        if !this.presentation_mode {
                            return false;
                        }
                        this.set_scroll_offset_y_on(&scroll, y);
                        cx.notify();
                        t < 1.0
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        cx.notify();
    }

    /// Item 23: set the uniform-list vertical scroll offset to `y` (px, ≤ 0).
    fn set_scroll_offset_y(&self, y: f32) {
        self.set_scroll_offset_y_on(&self.scroll, y);
    }

    /// Item 23: set a given handle's vertical scroll offset to `y` (keeps x).
    fn set_scroll_offset_y_on(&self, handle: &UniformListScrollHandle, y: f32) {
        let state = handle.0.borrow();
        let x = state.base_handle.offset().x;
        state.base_handle.set_offset(point(x, px(y)));
    }

    /// Resolve a node id to its first non-footer / non-continuation display line
    /// (the C++ `m_nodeLineIndex` first entry, used by scroll-to-node). `None` when
    /// the node is not currently composed (collapsed away / filtered).
    fn line_for_node(&self, node_id: u64) -> Option<usize> {
        if node_id == 0 {
            return None;
        }
        self.controller.last_result().meta.iter().position(|lm| {
            lm.node_id == node_id && lm.line_kind != LineKind::Footer && !lm.is_continuation
        })
    }

    // ── Debug view (item 75, `VM_Debug`) ──

    /// Whether the editor is showing the DEBUG surface (each line's margin +
    /// annotated text + LineMeta) instead of the structured grid.
    pub fn debug_view(&self) -> bool {
        self.debug_view
    }

    /// Turn the DEBUG surface on/off (`VM_Debug`). The window's view-mode toggle
    /// drives this; the structured grid is restored when off.
    pub fn set_debug_view(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.debug_view != on {
            self.debug_view = on;
            // Leaving an inline edit when entering the read-only debug dump.
            if on {
                self.editing = None;
            }
            cx.notify();
        }
    }

    /// Build the DEBUG dump lines for the current compose result (the C++
    /// `generateDebugText`, main.cpp:5534) — one `margin|text  ## meta` string per
    /// composed line. The comment / type-hint columns are derived from the line's
    /// chips (Rust uses chips where the C++ kept `commentStart`/`typeHintStart`).
    fn build_debug_lines(&self) -> Vec<String> {
        let result = self.controller.last_result();
        let mut out = Vec::with_capacity(result.meta.len());
        for (i, lm) in result.meta.iter().enumerate() {
            let margin = lm.offset_text.clone();
            let text = self.line_text_owned(i);
            // Comment / type-hint chip start columns (or -1 if absent).
            let comment_col = lm
                .chips
                .iter()
                .find(|c| {
                    matches!(
                        c.kind,
                        crate::core::linemeta::ChipKind::Comment
                            | crate::core::linemeta::ChipKind::AddComment
                    )
                })
                .map(|c| c.start_col)
                .unwrap_or(-1);
            let hint_col = lm
                .chips
                .iter()
                .find(|c| c.kind == crate::core::linemeta::ChipKind::TypeHint)
                .map(|c| c.start_col)
                .unwrap_or(-1);
            out.push(geometry::debug_line(
                &margin,
                &text,
                lm,
                i,
                comment_col,
                hint_col,
            ));
        }
        out
    }

    /// Render the DEBUG surface (item 75): a virtualized monospace list of the
    /// debug dump lines, styled in the dim editor mono palette. Read-only.
    fn render_debug_surface(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = EditorPalette::from_theme(cx);
        let lines = self.build_debug_lines();
        let line_h = self.metrics.line_height;
        let col_count = lines.len();
        let lines_rc = std::rc::Rc::new(lines);
        div()
            .id("rcx-debug-surface")
            .size_full()
            .bg(palette.paper)
            .text_color(palette.dim)
            .text_size(px(self.editor_font_size()))
            .font_family(self.editor_font_family())
            .child(
                uniform_list(
                    "rcx-debug-rows",
                    col_count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                        let lines = lines_rc.clone();
                        range
                            .map(|ix| {
                                let text = lines.get(ix).cloned().unwrap_or_default();
                                div()
                                    .h(px(line_h))
                                    .px(px(4.0))
                                    .whitespace_nowrap()
                                    .child(SharedString::from(text))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    // ── Row rendering ──

    /// Build the `RowPaint` for line `idx`: text + colored runs + overlays.
    fn build_row_paint(&self, idx: usize, palette: EditorPalette) -> RowPaint {
        let lm = self.line_meta(idx).cloned().unwrap_or_default();
        let text = self.line_text_owned(idx);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let runs = geometry::style_runs(&lm, &text, type_w, name_w);

        let mut overlays: Vec<(i32, i32, Hsla)> = Vec::new();

        // Per-byte change heat is now a *glyph* recolor folded into `style_runs`
        // (editor-surface.md §3: `IND_HEAT_*` are TEXTFORE), matching the
        // orange→red changed digits in the screenshots. Only the byte-SELECTION
        // overlay remains a (soft) background highlight here.

        // Byte-selection digit highlight (intersect the row with [lo,hi); §12).
        if let Some(sel) = self.byte_sel.range() {
            if is_hex_preview(lm.node_kind) && lm.line_kind == LineKind::Field {
                let count = if lm.line_byte_count > 0 {
                    lm.line_byte_count
                } else {
                    crate::core::size_for_kind(lm.node_kind)
                };
                if let Some((first, last)) = selection::row_byte_overlap(lm.offset_addr, count, sel)
                {
                    let vs = crate::compose::value_span_for(&lm, type_w, name_w);
                    if vs.valid {
                        let s = vs.start + first * 3;
                        let e = vs.start + (last - 1) * 3 + 2;
                        overlays.push((s, e, with_alpha(palette.byte_sel, 0.35)));
                    }
                }
            }
        }

        // Find-match highlight (items 4/31): the C++ `applyFindHighlights` paints
        // `IND_FIND` at EVERY match (re-derived on every refresh), with the current
        // navigated match emphasized. Paint a faint band over each cached match on
        // this line, and a stronger band on the current match. Char columns map
        // straight onto the overlay column space. The match set is cached in
        // `find_matches` (refreshed on Navigate / recompose) so this paint path
        // needs no `cx`.
        for m in &self.find_matches {
            if m.line == idx && m.end > m.start {
                let is_current = self
                    .find_match
                    .is_some_and(|c| c.line == m.line && c.start == m.start && c.end == m.end);
                let alpha = if is_current { 0.40 } else { 0.18 };
                overlays.push((
                    m.start as i32,
                    m.end as i32,
                    with_alpha(palette.accent, alpha),
                ));
            }
        }

        // Rounded chip backgrounds: footer add-bytes/Trim pills and the
        // command-row chevron/source chips (editor-surface.md §5 step 14;
        // PIC4/PIC5). Drawn as subtle Zed buttons (soft fill + 1px border).
        let mut pills: Vec<element::PillPaint> = Vec::new();
        match lm.line_kind {
            LineKind::Footer => {
                for s in geometry::footer_pill_spans(&text) {
                    pills.push(element::PillPaint {
                        start: s.start,
                        end: s.end,
                        fill: palette.pill_bg,
                        border: with_alpha(palette.border, 0.6),
                    });
                }
            }
            LineKind::CommandRow => {
                for s in geometry::command_row_pill_spans(&text) {
                    pills.push(element::PillPaint {
                        start: s.start,
                        end: s.end,
                        fill: palette.pill_bg,
                        border: with_alpha(palette.border, 0.6),
                    });
                }
            }
            _ => {}
        }

        RowPaint {
            text: text.into(),
            runs,
            overlays,
            pills,
            palette,
            metrics: self.metrics,
        }
    }

    /// Item 80: resolve the inline local-offset overlay for `lm` (the C++
    /// `IND_LOCAL_OFF` Pass 2, editor.cpp:1430). Resolves the `parent_addr` the
    /// local offset is measured from — ptrBase for a pointer-expanded child,
    /// else the compose-precomputed `LineMeta::parent_addr`, else the view base —
    /// then defers the column/slot/text math to the pure
    /// [`geometry::local_offset_overlay`]. Returns `None` when the row doesn't
    /// qualify or the slot is too tight.
    fn local_offset_overlay_for(&self, lm: &LineMeta) -> Option<(i32, i32, String)> {
        let base = self.controller.last_result().layout.base_address;
        let parent_addr = if lm.ptr_base != 0 {
            lm.ptr_base
        } else if lm.parent_addr != 0 {
            lm.parent_addr
        } else {
            base
        };
        geometry::local_offset_overlay(lm, self.relative_offsets, self.tree_lines(), parent_addr)
    }

    /// Build the [`minimap::Minimap`] element from the current compose result: one
    /// proportional bar per composed line, colored by node kind / line role, plus
    /// the viewport indicator from the live scroll offset (item 4). Cheap — it only
    /// reads the existing `LineMeta`s and the scroll handle; no re-layout.
    fn build_minimap(&self, palette: EditorPalette, cx: &App) -> minimap::Minimap {
        let result = self.controller.last_result();
        let rows: Vec<minimap::MinimapRow> = result
            .meta
            .iter()
            .map(|lm| minimap_row_for(lm, &palette))
            .collect();
        let total = rows.len();
        let (visible_start, visible_end) = self.minimap_visible_range(total);
        let t = cx.theme();
        minimap::Minimap {
            rows,
            chrome: minimap::MinimapChrome {
                // A faint panel a hair darker than the paper, with the muted-blue
                // overview tint the C++ purple block suggests (kept subtle / on
                // palette via the primary accent, low alpha).
                bg: with_alpha(t.primary, 0.06),
                border: palette.border,
                viewport: with_alpha(t.primary, 0.16),
                viewport_border: with_alpha(t.primary, 0.45),
            },
            total,
            visible_start,
            visible_end,
        }
    }

    /// The `[start, end)` composed-line range currently visible in the editor, for
    /// the minimap viewport indicator. Derived from the uniform-list scroll offset
    /// (the handle's tuple field + `base_handle` are public) and the measured row
    /// height; clamps to `[0, total]` and degrades to "all visible" when the scroll
    /// state is not yet populated (first frame).
    fn minimap_visible_range(&self, total: usize) -> (usize, usize) {
        if total == 0 || self.metrics.line_height <= 0.0 {
            return (0, total);
        }
        let state = self.scroll.0.borrow();
        let offset_y = f32::from(state.base_handle.offset().y); // <= 0 when scrolled down
        let view_h = f32::from(state.base_handle.bounds().size.height);
        if view_h <= 0.0 {
            return (0, total);
        }
        let lh = self.metrics.line_height;
        let start = ((-offset_y) / lh).floor().max(0.0) as usize;
        let visible = (view_h / lh).ceil() as usize + 1;
        let start = start.min(total);
        let end = (start + visible).min(total);
        (start, end)
    }

    /// Whether row `idx` is selected (any selection-id maps to its node, matching
    /// the line type for footer/array-elem/member rows; §7 `applySelectionOverlay`).
    fn is_row_selected(&self, lm: &LineMeta) -> bool {
        if lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return false;
        }
        self.controller
            .selected_ids()
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == lm.node_id)
    }

    /// Render one row: background (selection/hover) + text element, with the row
    /// element handling row-local click routing. Editable rows embed the field
    /// overlay positioned at the edited column.
    fn render_row(&self, idx: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = EditorPalette::from_theme(cx);
        let lm = self.line_meta(idx).cloned().unwrap_or_default();
        let mut selected = self.is_row_selected(&lm);
        // The hover band is gated by the `hover_effects` view toggle (item 3): when
        // off, the row still tracks the pointer but paints no hover wash. Item 9:
        // the band covers ALL display lines of the hovered NODE (multi-line
        // headers/values) by matching `hovered_node_id`, and is SUPPRESSED when the
        // node is already selected (the C++ `applyHoverHighlight` skips selected
        // rows). A hovered row with no real node (node_id 0 — chrome) still lights
        // the single row it sits on so empty rows keep a hover affordance.
        let hovered = self.hover_effects
            && !selected
            && ((self.hovered_node_id != 0 && lm.node_id == self.hovered_node_id)
                || (self.hovered_node_id == 0 && self.hovered_line == Some(idx)));

        let editing_here = self
            .editing
            .as_ref()
            .filter(|e| e.line == idx)
            .map(|e| (e.field.clone(), e.col_start, e.col_end));
        // The "active line" (Zed's active-line bg / reclass's highlighted current
        // row): the row currently being edited, even when it is not part of the
        // multi-selection. A selected row already carries the louder accent fill.
        let active_line = editing_here.is_some();

        // Item 71: this row carries a LIVE inline-edit error (the `M_ERR` band) when
        // the active edit is on it AND validation failed. On error, the C++
        // suppresses `M_SELECTED` (it sits above M_ERR in priority) so the red band
        // is unambiguous — mirror that by clearing `selected` here.
        let edit_error = self
            .edit_validation
            .as_ref()
            .filter(|v| v.line == idx && !v.error.is_empty())
            .is_some();
        if edit_error {
            selected = false;
        }

        // Item 74: this row participates in the presentation-mode focus glow when
        // the AI/MCP focus node maps to it. The pulsing alpha is derived from the
        // glow phase (the C++ `m_glowPhase` sine pulse).
        let focus_glow = self.presentation_mode
            && self.focus_node_id != 0
            && lm.node_id == self.focus_node_id
            && lm.node_id != 0
            && lm.line_kind != LineKind::Footer;

        // Row background precedence (§7): the red error band wins (an invalid live
        // edit), then the accent-tinted selection fill, then the active-line band,
        // then the focus glow, then the hover overlay. Each is a distinct surface.
        let bg = if edit_error {
            Some(palette.error_bg)
        } else if selected {
            Some(palette.selection_bg)
        } else if active_line {
            Some(palette.active_line_bg)
        } else if focus_glow {
            // Pulse the glow alpha: t = 0.5 + 0.5*sin(phase*PI/12), blended onto the
            // focus-glow base (the C++ dim↔bright pulse, editor.cpp:1897).
            let t =
                0.5 + 0.5 * ((self.focus_glow_phase as f32) * std::f32::consts::PI / 12.0).sin();
            let alpha = 0.18 + 0.22 * t; // dim 0.18 → bright 0.40
            Some(with_alpha(palette.focus_glow, alpha))
        } else if hovered {
            Some(palette.hover_bg)
        } else {
            None
        };

        let mut row = div()
            .id(("rcx-row", idx))
            .relative()
            .w_full()
            .h(px(self.metrics.line_height))
            .flex()
            .flex_row()
            // Hover tracking (the row background tracks the cursor; §7). Item 9:
            // also track the hovered NODE id so the band covers every line of a
            // multi-line node.
            .on_mouse_move(cx.listener(move |this, _e: &MouseMoveEvent, _w, cx| {
                let node_id = this.line_meta(idx).map(|lm| lm.node_id).unwrap_or(0);
                if this.hovered_line != Some(idx) || this.hovered_node_id != node_id {
                    this.hovered_line = Some(idx);
                    this.hovered_node_id = node_id;
                    cx.notify();
                }
            }));

        if let Some(c) = bg {
            row = row.bg(c);
        }
        // Left accent bar (`M_ACCENT`) for the selected row — a 2px Zed accent
        // edge. Reserve the 2px on EVERY row (transparent when unselected) so the
        // gutter + text never jitter sideways as the selection moves.
        row = row.border_l_2().border_color(if selected {
            palette.accent
        } else {
            gpui::transparent_black()
        });

        // Address/offset margin — the Zed-gutter left column (PIC4/PIC5's muted
        // "+0 +8 +10 …" address column). Fixed width from `offset_hex_digits` so
        // EVERY row's main text starts at the same column. The *value* is computed
        // here per-row from the row's resolved `offset_addr` (NOT a precomputed
        // string that repeated the base on every row): relative `"+<HEX>"` offsets
        // from the view base in `m_relativeOffsets` mode (the reclass default,
        // PIC4/PIC5), or full absolute addresses when toggled (PIC1/PIC2). Compose's
        // own blank-vs-filled policy is preserved (footers / element-separators /
        // static-body lines stay blank) by only rendering rows whose composed
        // `offset_text` is non-blank. Low-contrast gutter: a hair-darker bg, muted
        // blue-gray digits, right-aligned with a small trailing pad.
        let layout = &self.controller.last_result().layout;
        let addr_cols = layout.offset_hex_digits.max(0);
        let base_address = layout.base_address;
        // `m_relativeOffsets` — relative `"+<HEX>"` offsets by default (PIC4/PIC5),
        // absolute addresses when the user toggles (PIC1/PIC2).
        let relative = self.relative_offsets;
        if addr_cols > 0 {
            // The offset gutter is BLANK on the class-header (command) row — where
            // the `[▸] source▾ <addr> struct Name {` lives — and on the closing
            // footer row (`}; … // 0xNN`). The reference shows offsets only on the
            // field rows between them (the header carries the base address inline,
            // and the footer carries the total size inline, so a left "+NN" there is
            // redundant). Field/continuation rows keep their per-row offset.
            let margin_text = if lm.offset_text.trim().is_empty()
                || lm.line_kind == LineKind::CommandRow
                || lm.line_kind == LineKind::Footer
            {
                String::new()
            } else {
                geometry::fmt_margin_text(
                    lm.offset_addr,
                    base_address,
                    lm.ptr_base,
                    addr_cols,
                    lm.is_continuation,
                    relative,
                )
            };
            row = row.child(
                div()
                    .id(("rcx-margin", idx))
                    .flex_shrink_0()
                    .w(px((addr_cols as f32 + 2.0) * self.metrics.cell_width))
                    .h(px(self.metrics.line_height))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .pr(px(self.metrics.cell_width))
                    .bg(palette.gutter_bg)
                    .text_color(palette.gutter_fg)
                    // Double-click the offset margin → flip relative/absolute
                    // offsets (item 25; the C++ `MouseButtonDblClick` over margin 0).
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                            if e.click_count >= 2 {
                                cx.stop_propagation();
                                // Item 12: flip + propagate (persist / View-menu ✓ /
                                // push to every editor) via the host event.
                                let rel = this.relative_offsets;
                                this.toggle_relative_offsets(!rel, cx);
                            }
                        }),
                    )
                    // Item 27: right-click the offset margin → a small checkable
                    // Relative Offsets (+0x) / Absolute Addresses menu (the C++
                    // margin-0 right-click), in addition to the left double-click.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_offset_mode_menu(e.position, window, cx);
                        }),
                    )
                    .child(SharedString::from(margin_text)),
            );
        }

        // No node-kind icon gutter (B1 / items 2-3): C++ Reclass has NO icon column
        // and NO per-row SVG type glyph. The fold disclosure arrow (`▸`/`▾`) is a
        // TEXT glyph the compose layer bakes into the fold-prefix (column 1 of the
        // K_FOLD_COL=3 region), painted in the crisp `FoldChevron` role on the row's
        // own baseline (see `palette.fold_chevron`); leaf rows show only the dim
        // tree connector or a blank prefix. Removing the invented `ICON_CELLS=2`
        // gutter shifts the whole type/name/value/offset grid left to match C++ and
        // text-baselines the chevron at the fold prefix instead of SVG-centering it.
        //
        // The row-text element + every absolute overlay (the inline-edit field and
        // the command-row hover strips) live inside ONE `relative` wrapper
        // (`text_region`) that begins right after the address margin. The wrapper's
        // absolute origin coincides with the painted text origin, so an overlay at
        // `left = col*cell` lands on the exact same pixel as painted text column
        // `col` — no icon-gutter term to add.
        let cell = self.metrics.cell_width;
        let mut text_region = div()
            .relative()
            .flex_grow()
            .h(px(self.metrics.line_height))
            .flex()
            .flex_row();

        // The text element (static) — always painted as the base layer; it owns
        // the hitbox + row-local click routing back into the view.
        let row_paint = self.build_row_paint(idx, palette);
        text_region = text_region.child(RowElement {
            row: row_paint,
            editor: cx.entity().downgrade(),
            line: idx,
        });

        // Item 80: inline LOCAL-OFFSET overlay (the C++ `IND_LOCAL_OFF` Pass 2). In
        // relative-offset mode, a child row at depth>1 shows a dim `+XX` local
        // offset (from the enclosing parent / ptrBase / array-element base) in the
        // indent area before the type column. Painted as an absolute overlay inside
        // `text_region` (origin == painted-text origin), so its `left` is just the
        // per-column offset — the same coordinate space the inline-edit field uses.
        if let Some((start_col, slot_w, off_text)) = self.local_offset_overlay_for(&lm) {
            let left = px(start_col.max(0) as f32 * cell);
            let width = px((slot_w.max(1) as f32) * cell);
            text_region = text_region.child(
                div()
                    .absolute()
                    .top_0()
                    .left(left)
                    .h(px(self.metrics.line_height))
                    .w(width)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .text_size(px(self.editor_font_size()))
                    .font_family(self.editor_font_family())
                    // The faint/dim text role (the C++ `theme.textFaint`,
                    // editor.cpp:802) so the local offset reads as a quiet hint.
                    .text_color(palette.dim)
                    .child(SharedString::from(off_text)),
            );
        }

        // Address-format hover popover (reclass_address_hover.png + PIC5 "Base
        // Address"): on the class-header command row, an invisible interactive
        // overlay sits over the base-address span and shows the address-format card
        // on hover. Positioned in the row's own coordinate space, so it clears the
        // address margin + the kind-icon gutter (both precede the row text) before
        // the per-column offset — exactly like the inline-edit overlay.
        // Suppress the hover overlay while this command row's base address is being
        // inline-edited — otherwise its interactive hitbox sits ON TOP of the edit
        // field, occluding it (the field opens but is hidden behind the tooltip
        // strip and never receives the click/focus). Skipping it while editing lets
        // the `BaseAddress` field render + focus.
        let editing_this_row = self.editing.as_ref().map(|e| e.line) == Some(idx);
        if lm.line_kind == LineKind::CommandRow && !editing_this_row {
            let text = self.line_text_owned(idx);
            let addr = crate::compose::command_row_addr_span(&text);
            if addr.valid && addr.end > addr.start {
                // The hover strip lives INSIDE `text_region` (after the address
                // margin), so its `left` is just the per-column offset — NO
                // margin/border/gutter term. The wrapper's absolute origin == the
                // text origin, so this lands exactly on the painted address span
                // (the same alignment the inline-edit field uses).
                let left = px(addr.start.max(0) as f32 * cell);
                let width = px(((addr.end - addr.start).max(1) as f32) * cell);
                let base_address = self.controller.last_result().layout.base_address;
                let module: SharedString = self.controller.document().provider.name().into();
                // Forward a left mouse-down on the overlay back into the normal row
                // click routing: the overlay's hitbox occludes the row-text hitbox,
                // so without this the base-address inline edit (the `BaseAddress`
                // hit-test target) would stop working under the tooltip strip.
                //
                // `on_row_mouse_down` → `hit_test_row` resolves the column from a
                // **row-text-local** X (column 0 = the first char of the composed
                // command-row string). Forward a **text-local** X (the address
                // span's first column + half a cell) so the hit test lands inside
                // the address span and the base-address edit actually begins.
                let addr_click_x = (addr.start.max(0) as f32 + 0.5) * cell;
                text_region = text_region.child(
                    div()
                        .id(("rcx-addr-hover", idx))
                        .absolute()
                        .top_0()
                        .left(left)
                        .h(px(self.metrics.line_height))
                        .w(width)
                        // Item 22: the editable base-address strip shows the text
                        // I-beam cursor (the C++ Qt::IBeamCursor over the address) so
                        // it reads as click-to-edit, not a button.
                        .cursor(CursorStyle::IBeam)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                // Stop the row-text element's global mouse handler from
                                // ALSO running for this same click — otherwise it
                                // re-enters `on_row_mouse_down` with the active edit set
                                // and commits/closes the base-address field the instant
                                // it opens (the address edit appeared to "do nothing").
                                cx.stop_propagation();
                                this.on_row_mouse_down(idx, addr_click_x, e.modifiers, window, cx);
                            }),
                        )
                        .tooltip(move |_window, cx| {
                            cx.new(|_| AddressFormatTooltip {
                                base_address,
                                module: module.clone(),
                            })
                            .into()
                        }),
                );
            }

            // Source-chip / chevron / class-name hover affordances (items 5/22): a
            // pointing-hand cursor + a TITLED hover card over each interactive
            // command-row span. Item 22: each card carries the C++ title + body
            // (rcxtooltip / editor.cpp:4730): Data Source, Switch View, and the
            // root Class-Name span (which previously had NO hover affordance at
            // all). Transparent hover hitboxes living INSIDE `text_region` (after
            // the address margin). Each forwards its click back to the normal row
            // routing so the source/type-selector/rename flow still fires.
            for (hover_id, span, title, body) in [
                (
                    "rcx-src-hover",
                    crate::compose::command_row_src_span(&text),
                    "Data Source",
                    "Click to change the attached\nmemory source (process, file)",
                ),
                (
                    "rcx-chevron-hover",
                    crate::compose::command_row_chevron_span(&text),
                    "Switch View",
                    "View a different struct in this tab",
                ),
                (
                    "rcx-classname-hover",
                    crate::compose::command_row_root_name_span(&text),
                    "Class Name",
                    "Click to rename this type",
                ),
            ] {
                if span.valid && span.end > span.start {
                    let left = px(span.start.max(0) as f32 * cell);
                    let width = px(((span.end - span.start).max(1) as f32) * cell);
                    let title: SharedString = title.into();
                    let body: SharedString = body.into();
                    // The hover hitbox occludes the row-text element, so forward its
                    // click back into the normal row routing (text-local X inside the
                    // span) — exactly like the address strip — so the source/chevron/
                    // class-name click still reaches `on_row_mouse_down` → the popup
                    // / rename flow (items 1/2/22).
                    let click_x = (span.start.max(0) as f32 + 0.5) * cell;
                    text_region = text_region.child(
                        div()
                            .id((hover_id, idx))
                            .absolute()
                            .top_0()
                            .left(left)
                            .h(px(self.metrics.line_height))
                            .w(width)
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.on_row_mouse_down(idx, click_x, e.modifiers, window, cx);
                                }),
                            )
                            .tooltip(move |_window, cx| {
                                let title = title.clone();
                                let body = body.clone();
                                cx.new(|_| TitledTooltip { title, body }).into()
                            }),
                    );
                }
            }
        }

        // Inline-edit overlay positioned EXACTLY over the edited column.
        //
        // The field lives inside `text_region`, whose absolute origin coincides
        // with the painted text origin (the wrapper sits after the address margin;
        // its first/only flex child is the row text). So the overlay-left is just
        // the per-column offset — there is NO border/margin/gutter term to guess.
        // This makes the box land directly over the clicked token (e.g. the root
        // class NAME on the command row), not after the `{` and not on the `struct`
        // keyword (items 1/2): painted text column `c` and overlay-left `c*cell` are
        // the same pixel by construction.
        if let Some((field, col_start, col_end)) = editing_here {
            let left = px(col_start.max(0) as f32 * cell);
            // The opaque band spans the edited column `[col_start, col_end)` (a
            // generous minimum so short seeds still get a visible field box).
            let editing_width = ((col_end - col_start).max(0) as f32).max(6.0);
            // The inline field paints over the static row text. Give it a FULLY
            // OPAQUE editor-paper band (not the semi-transparent active-line fill,
            // which let the column's static glyphs — the type token / pre-edit name —
            // bleed through behind the seeded text and read as garbled overlap
            // "hexChex64"/"int64_teateTime"). A 1px accent ring + slight rounding make
            // it read as a Zed inline input.
            text_region = text_region.child(
                div()
                    .absolute()
                    .top_0()
                    .left(left)
                    .h(px(self.metrics.line_height))
                    .w(px((editing_width + 1.0) * cell))
                    .bg(palette.paper)
                    .border_1()
                    .border_color(palette.accent)
                    .rounded_sm()
                    // The field entity's own `Render` carries the focus/key-context
                    // wrapper (`.track_focus` + `.key_context("RcxFieldInput")` +
                    // every `.on_action(..)` field handler). Embedding the entity —
                    // not the raw `FieldElement` — is what establishes the
                    // `RcxFieldInput` key context on the focused element so keystrokes
                    // route through `window.handle_input` and the action bindings
                    // fire (items 1 & 2). The raw `FieldElement` had no key context,
                    // so typing / backspace / arrows / enter / escape were all inert
                    // and the caret never painted (its paint is gated on focus).
                    .child(field.clone()),
            );

            // Item 71/72: the inline-edit HINT comment — a green
            // 'Enter=Save Esc=Cancel' on a valid edit, or a red '! <error>' on an
            // invalid one (the C++ `setEditComment`, editor.cpp:4915/4919). Painted
            // just past the line text so it sits where the row's `//` comment would.
            if let Some(v) = self.edit_validation.as_ref().filter(|v| v.line == idx) {
                let (hint, color) = if v.error.is_empty() {
                    ("Enter=Save Esc=Cancel".to_string(), palette.comment_green)
                } else {
                    (format!("! {}", v.error), palette.error_fg)
                };
                // Anchor a couple cells past the longer of the line text / edited
                // span so the hint clears both the static text and the edit box.
                let text_cols = self.line_text_owned(idx).chars().count() as i32;
                let hint_col = text_cols.max(col_end) + 2;
                let left = px(hint_col.max(0) as f32 * cell);
                text_region = text_region.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(left)
                        .h(px(self.metrics.line_height))
                        .flex()
                        .flex_row()
                        .items_center()
                        .text_size(px(self.editor_font_size()))
                        .font_family(self.editor_font_family())
                        .text_color(color)
                        .child(SharedString::from(format!("// {hint}"))),
                );
            }

            // Item 68/73: the floating expression-RESULT popup ('→ 0xHEX' /
            // 'Result: 0xHEX') above the edited span when the text is an expression
            // (the C++ `m_exprResultLabel`, editor.cpp:4923). A small elevated card
            // anchored at the edit-span column, lifted one row up.
            if let Some(r) = self.expr_result.as_ref().filter(|r| r.line == idx) {
                let left = px(r.col.max(0) as f32 * cell);
                let result_text = r.text.clone();
                text_region = text_region.child(
                    div()
                        .absolute()
                        // One row UP from the edit line (a floating tooltip-style card).
                        .top(px(-self.metrics.line_height - 2.0))
                        .left(left)
                        .px(px(6.0))
                        .py(px(2.0))
                        .bg(palette.paper)
                        .border_1()
                        .border_color(palette.border)
                        .rounded_sm()
                        .text_size(px(self.editor_font_size()))
                        .font_family(self.editor_font_family())
                        .text_color(palette.value_fg)
                        .child(SharedString::from(result_text)),
                );
            }
        }

        // Mount the text region (icon gutter + row text + all absolute overlays)
        // after the address margin. Its origin == the painted-text origin, which is
        // what makes the inline-edit / hover overlays pixel-aligned with the text.
        row = row.child(text_region);

        row.into_any_element()
    }

    /// Row-local click entry point invoked by [`RowElement`] (row-local X). Routes
    /// the click through [`on_row_mouse_down`](Self::on_row_mouse_down).
    pub(crate) fn dispatch_row_click(
        &mut self,
        line: usize,
        rel_x: f32,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_row_mouse_down(line, rel_x, modifiers, window, cx);
    }

    /// Double-click-to-edit (item 26): select the clicked node first, then begin the
    /// edit/picker for the token under the cursor (the C++ `MouseButtonDblClick`
    /// path — narrow selection to the node, then `beginInlineEdit`). Falls back to a
    /// single-click route when the column resolves no editable target.
    pub(crate) fn dispatch_row_double_click(
        &mut self,
        line: usize,
        rel_x: f32,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        let text = self.line_text_owned(line);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let hit = hit_test::hit_test_row(&lm, &text, rel_x, self.metrics, type_w, name_w);
        // Select the node first (single-select), so the edit acts on it.
        if lm.node_id != 0 && lm.node_id != K_COMMAND_ROW_ID {
            self.controller
                .handle_node_click(line as i64, lm.node_id, CtrlMods::NONE);
            let _ = self.controller.take_events();
        }
        // Now route the token under the cursor exactly as a click on an
        // already-selected node would (pickers + inline edits).
        if hit.target.is_some() {
            self.on_row_mouse_down(line, rel_x, modifiers, window, cx);
        } else {
            cx.notify();
        }
    }

    /// Byte-selection drag (item 11): extend the armed hex byte selection to the
    /// byte under the dragged cursor. No-op when no byte selection is armed or the
    /// row column maps to no byte.
    pub(crate) fn dispatch_row_drag(
        &mut self,
        line: usize,
        rel_x: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A drag never fires while an inline edit owns the surface.
        if self.editing.is_some() {
            return;
        }
        // Item 20/28: drag dead-zone. The C++ gates on VERTICAL travel (`|dy| >= 8`)
        // — node range-select is a vertical gesture, so a horizontal jitter on the
        // same row must NOT start a drag. Here the vertical signal is "the pointer
        // has crossed onto a DIFFERENT row" (each row is `line_height` ≥ 8px tall,
        // so a row change is unambiguous vertical travel). A same-row move never
        // starts a node drag (it would only matter for a byte-grid drag, which the
        // armed-selection path below handles). Once crossed, `drag_started` latches
        // for the rest of the gesture.
        if !self.drag_started {
            let crossed_row = self.drag_anchor_line.map(|a| a != line).unwrap_or(false);
            // A same-row byte-grid drag still arms once the pointer leaves the press
            // column meaningfully (the byte selection extends within the row).
            const DRAG_DEAD_ZONE_PX: f32 = 8.0;
            let byte_drag_armed = self.drag_on_byte_grid
                && self.byte_sel.is_active()
                && (rel_x - self.drag_anchor_x).abs() >= DRAG_DEAD_ZONE_PX;
            if !crossed_row && !byte_drag_armed {
                return;
            }
            self.drag_started = true;
        }
        // Mode 1 — the drag started on the hex byte grid: extend the armed byte
        // selection to the byte under the cursor (the original behavior).
        if self.drag_on_byte_grid && self.byte_sel.is_active() {
            let Some(lm) = self.line_meta(line).cloned() else {
                return;
            };
            let text = self.line_text_owned(line);
            let col = self.metrics.col_containing_x(rel_x);
            if let Some(addr) = self.byte_addr_for_hit(&lm, &text, col) {
                self.byte_sel.shift_extend_to(addr);
                cx.notify();
            }
            return;
        }

        // Item 20: a drag past the dead-zone flushes any DEFERRED click (the C++
        // `m_pendingClickNodeId`) BEFORE extending — but as a no-op for the
        // selection because the drag is about to repaint the range anyway. Crucially
        // we DROP the pending click so the mouse-up does not later collapse the
        // group the drag just (re)selected.
        self.pending_click = None;

        // Mode 2 — the drag started OFF the byte grid (item 8): range-select the
        // NODES between the anchor row and the current row. Shift-clicking the
        // landed row with the anchor already set drives the controller's
        // `insert_range` (the same path mouse Shift-click and Shift-arrow use), so
        // a drag paints a contiguous node multi-selection.
        let Some(anchor) = self.drag_anchor_line else {
            return;
        };
        if line == anchor {
            return;
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        if lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID || lm.is_continuation {
            return;
        }
        // Seed the anchor with a plain click on the anchor row's node (if the
        // selection lost it), then shift-extend to the dragged row so the range
        // spans [anchor, line]. `handle_node_click` keys the range off the
        // controller's `anchor_line`, which the initial mouse-down already set, so
        // a single shift-extend here paints the full range. Item 20: a Ctrl+drag
        // ADDS the range to the existing selection (ctrl from the press mods)
        // rather than replacing it.
        let node_id = lm.node_id;
        self.controller.handle_node_click(
            line as i64,
            node_id,
            CtrlMods {
                ctrl: self.drag_init_mods.control,
                shift: true,
            },
        );
        self.caret_line = Some(line);
        self.after_mutation(cx);
    }

    /// Item 20: flush a deferred plain click on mouse-RELEASE (the C++
    /// `m_pendingClickNodeId` release path). When no drag started, the deferred
    /// click fires as a plain `handle_node_click`, collapsing the multi-selection to
    /// the clicked node. A drag already cleared the pending click, so this is a
    /// no-op after a drag.
    fn flush_pending_click(&mut self, cx: &mut Context<Self>) {
        self.drag_started = false;
        let Some((line, node_id, modifiers)) = self.pending_click.take() else {
            return;
        };
        self.controller.handle_node_click(
            line as i64,
            node_id,
            CtrlMods {
                ctrl: modifiers.control,
                shift: modifiers.shift,
            },
        );
        if node_id != 0 && node_id != K_COMMAND_ROW_ID {
            self.caret_line = Some(line);
        }
        self.after_mutation(cx);
    }

    // ── Hover popups (item 13, editor.cpp applyHoverCursor) ──

    /// Resolve the hover popup for the row/column under the cursor (item 13).
    /// Called on every non-dragging mouse-move over a row. Updates `hovered_line`
    /// (the row hover band) and, when the cursor is over the VALUE column of a
    /// qualifying node, opens one of three popups: a value-history list for a
    /// heated changed value, a disasm/hex-dump for a function/void pointer, or a
    /// struct-preview for a collapsed typed pointer. Otherwise the popup is cleared.
    pub(crate) fn dispatch_row_hover(
        &mut self,
        line: usize,
        rel_x: f32,
        pos: Point<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Item 9: track the hovered NODE id (not just the row) so the hover band
        // lights every line of a multi-line node. Chrome rows (node_id 0) fall back
        // to single-row hover.
        let node_id = self.line_meta(line).map(|lm| lm.node_id).unwrap_or(0);
        let changed_band = self.hovered_line != Some(line) || self.hovered_node_id != node_id;
        if changed_band {
            self.hovered_line = Some(line);
            self.hovered_node_id = node_id;
        }
        // Hover popups are gated by the hover-effects toggle. Item 68: the
        // value-history popup is NOT suppressed while editing — when an edit is
        // active it is shown WITH 'Set' buttons + relative timestamps so the user
        // can click a previous value into the field (the C++ recreates the popup
        // with Set buttons once editing starts; editor.cpp:3579). The other popups
        // (disasm/struct-preview) are still suppressed while editing because the
        // field owns the surface; `compute_hover_popup` only returns the
        // value-history variant when `editing` is active.
        // Item 13: while the cursor is INSIDE the floating hover card, suppress
        // popup dismissal/replacement entirely (the C++ keeps the popup while the
        // cursor is over its geometry; editor.cpp:2815). Moving onto the card to
        // click a value-history 'Set' button would otherwise re-fire hover for the
        // row under the card and clear the popup before the click lands. The hover
        // band still tracks the row for other affordances.
        if self.popup_cursor_inside {
            if changed_band {
                cx.notify();
            }
            return;
        }
        let want = if self.hover_effects {
            self.compute_hover_popup(line, rel_x, pos)
        } else {
            None
        };
        let changed_popup = match (&self.hover_popup, &want) {
            (None, None) => false,
            (Some(a), Some(b)) => a.line != b.line || !hover_kind_eq(&a.kind, &b.kind),
            _ => true,
        };
        if changed_popup {
            self.hover_popup = want;
        }
        if changed_band || changed_popup {
            cx.notify();
        }
    }

    /// Compute the hover popup (if any) for the value column under `(line, rel_x)`.
    /// Pure-ish (reads the controller's tree/provider/value-history); returns the
    /// popup state to show, or `None`. Mirrors the kind selection in editor.cpp
    /// `applyHoverCursor` (value history vs disasm/hex vs struct preview).
    fn compute_hover_popup(
        &self,
        line: usize,
        rel_x: f32,
        pos: Point<Pixels>,
    ) -> Option<HoverPopupState> {
        let lm = self.line_meta(line)?.clone();
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return None;
        }
        if lm.line_kind == LineKind::Footer {
            return None;
        }
        // Only fire when the cursor is over the VALUE column.
        let text = self.line_text_owned(line);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let vs = crate::compose::value_span_for(&lm, type_w, name_w);
        if !vs.valid {
            return None;
        }
        // Item 28: gate strictly on the value span `[vs.start, vs.end)` — the C++
        // checks `col >= vs.start && col < vs.end`. The prior guard returned None
        // only for `col < vs.start`, so a cursor PAST the value column end fell
        // through and showed the popup over the trailing comment area.
        let col = self.metrics.col_containing_x(rel_x);
        if col < vs.start || col >= vs.end {
            return None;
        }
        let _ = text;

        let kind = lm.node_kind;
        let is_fp = crate::core::is_func_ptr(kind);
        let is_void_ptr = matches!(kind, NodeKind::Pointer32 | NodeKind::Pointer64)
            && lm.pointer_target_name.is_empty();

        // While an inline edit owns the surface, ONLY the value-history popup is
        // shown (with Set buttons) — the disasm / struct-preview cards are
        // suppressed so they don't fight the edit field (item 68).
        let editing = self.editing.is_some();

        if !editing {
            // 1) Function / void pointer → disasm / hex-dump of the TARGET (item 13).
            if is_fp || is_void_ptr {
                // Item 8 (66/69/81): for the void-ptr (hex-dump) branch the C++
                // additionally requires `node.refId == 0` (a TYPED-but-unnamed
                // pointer, refId != 0, must NOT show a hex dump), and narrows the
                // trigger to the pointer-ADDRESS chip span (before the first chip)
                // rather than the full value span. The disasm (func-ptr) branch is
                // unconditional. `node.refId` is read from the live tree by id (the
                // C++ reads it off `m_disasmTree->nodes[lm.nodeIdx]`).
                if is_void_ptr && !is_fp {
                    let ref_id = {
                        let idx = self.controller.tree().index_of_id(lm.node_id);
                        if idx >= 0 {
                            self.controller.tree().nodes[idx as usize].ref_id
                        } else {
                            0
                        }
                    };
                    if ref_id != 0 {
                        return None;
                    }
                    // Narrow to the pointer-address chip span: the value column up
                    // to the first chip (the C++ `narrowPtrValueSpan`).
                    let narrowed = geometry::narrow_value_at_first_chip(&lm, vs);
                    if !narrowed.valid || col < narrowed.start || col >= narrowed.end {
                        return None;
                    }
                }
                if let Some(state) = self.pointer_disasm_popup(&lm, is_fp, pos) {
                    return Some(state);
                }
                // No readable target — fall through (no popup).
                return None;
            }

            // 1b) Collapsed TYPED pointer → struct-preview card (item 13): the first
            // few lines of the referenced struct composed at the pointer's target.
            let is_typed_ptr = matches!(kind, NodeKind::Pointer32 | NodeKind::Pointer64)
                && !lm.pointer_target_name.is_empty();
            if is_typed_ptr && lm.fold_collapsed {
                if let Some(state) = self.struct_preview_popup(&lm, pos) {
                    return Some(state);
                }
            }
        }

        // 2) Heated changed value with >1 distinct sample → value-history list. When
        // an edit is active on THIS row, the popup gets 'Set' buttons (item 68) so a
        // previous value can be clicked back into the field.
        if lm.heat_level > 0 {
            if let Some(hist) = self.controller.value_history().get(&lm.node_id) {
                if hist.unique_count() > 1 {
                    // Capture (value, relative-age) pairs newest→oldest.
                    let now = Self::now_millis();
                    let mut entries: Vec<(String, String)> = Vec::new();
                    hist.for_each_with_time(|v, t| {
                        if entries.len() < crate::core::value_history::K_CAPACITY {
                            entries.push((v.to_string(), Self::relative_age(now, t)));
                        }
                    });
                    if entries.len() > 1 {
                        let set_buttons = self.editing.as_ref().map(|e| e.line) == Some(line);
                        return Some(HoverPopupState {
                            line,
                            pos,
                            kind: HoverPopupKind::ValueHistory {
                                entries,
                                node_idx: lm.node_idx,
                                sub_line: lm.sub_line,
                                resolved_addr: lm.offset_addr,
                                set_buttons,
                            },
                        });
                    }
                }
            }
        }
        None
    }

    /// Current wall-clock time in milliseconds since the Unix epoch (for the
    /// value-history relative-age labels). Falls back to 0 if the clock is before
    /// the epoch (which would make every age read 'now').
    fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Format a value-history timestamp as a relative age ('now' / 'Ns ago' /
    /// 'Nm ago' / 'Nh ago') — the C++ `ValueHistoryPopup::populate` time string
    /// (editor.cpp:252). A non-positive timestamp (untracked) yields an empty label.
    fn relative_age(now: i64, then: i64) -> String {
        if then <= 0 {
            return String::new();
        }
        let elapsed = (now - then).max(0);
        if elapsed < 1000 {
            "now".to_string()
        } else if elapsed < 60_000 {
            format!("{}s ago", elapsed / 1000)
        } else if elapsed < 3_600_000 {
            format!("{}m ago", elapsed / 60_000)
        } else {
            format!("{}h ago", elapsed / 3_600_000)
        }
    }

    /// Build the disasm/hex-dump popup for a function/void pointer node by reading
    /// the pointer value, then the bytes at the target (item 13). `None` when the
    /// pointer is null/unreadable. Reads through the controller's live provider.
    fn pointer_disasm_popup(
        &self,
        lm: &LineMeta,
        is_fp: bool,
        pos: Point<Pixels>,
    ) -> Option<HoverPopupState> {
        let prov = &self.controller.document().provider;
        let is64 = matches!(lm.node_kind, NodeKind::FuncPtr64 | NodeKind::Pointer64);
        let ptr_val = if is64 {
            prov.read_u64(lm.offset_addr)
        } else {
            u64::from(prov.read_u32(lm.offset_addr))
        };
        if ptr_val == 0 || ptr_val == u64::MAX || (!is64 && ptr_val == 0xFFFF_FFFF) {
            return None;
        }
        const MAX_READ: i32 = 128;
        let bytes = prov.read_bytes(ptr_val, MAX_READ);
        // Item 29: only bail on an EMPTY read. The C++ shows the popup whenever the
        // read succeeds and the rendered body is non-empty (a valid pointer into a
        // zero-filled page still gets a hex dump); the `all-zero` short-circuit was
        // a Rust-only divergence.
        if bytes.is_empty() {
            return None;
        }
        let (title, mut body) = if is_fp {
            (
                "Disassembly".to_string(),
                crate::disasm::disassemble(&bytes, ptr_val, if is64 { 64 } else { 32 }, MAX_READ),
            )
        } else {
            (
                "Hex Dump".to_string(),
                crate::disasm::hex_dump(&bytes, ptr_val, MAX_READ),
            )
        };
        // Cap at 6 lines so the popup stays compact (the C++ kMaxLines).
        const MAX_LINES: usize = 6;
        if body.lines().count() > MAX_LINES {
            let kept: Vec<&str> = body.lines().take(MAX_LINES).collect();
            body = format!("{}\n...", kept.join("\n"));
        }
        if body.trim().is_empty() {
            return None;
        }
        Some(HoverPopupState {
            line: 0,
            pos,
            kind: HoverPopupKind::TitleBody { title, body },
        })
    }

    /// Build the struct-preview popup for a collapsed typed pointer (item 13):
    /// compose the referenced struct at the pointer target and show its first few
    /// data lines (skipping the command row). `None` when the pointer has no valid
    /// struct ref. Mirrors editor.cpp's struct-preview popup.
    fn struct_preview_popup(&self, lm: &LineMeta, pos: Point<Pixels>) -> Option<HoverPopupState> {
        let ref_id = {
            let tree = self.controller.tree();
            let n = tree.nodes.get(lm.node_idx as usize)?;
            n.ref_id
        };
        if ref_id == 0 || self.controller.tree().index_of_id(ref_id) < 0 {
            return None;
        }
        // Compose the referenced struct (same flags the live view uses for layout).
        let cr = self.controller.document().compose(
            ref_id,
            self.compact_columns,
            self.tree_lines(),
            true,
            self.type_hints(),
            self.show_comments(),
        );
        // Skip line 0 (the command row); take the first few non-empty data lines.
        const MAX_LINES: usize = 5;
        let body: String = cr
            .text
            .split('\n')
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .take(MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        if body.trim().is_empty() {
            return None;
        }
        Some(HoverPopupState {
            line: 0,
            pos,
            kind: HoverPopupKind::TitleBody {
                title: lm.pointer_target_name.clone(),
                body,
            },
        })
    }

    /// Item 68: write a value from the value-history popup's 'Set' button back into
    /// the node (the C++ `ValueHistoryPopup::m_onSet`). Routes through the
    /// controller's `set_node_value` (the same path an inline Value commit uses),
    /// closes any active edit + the popup, and recomposes.
    fn set_value_from_history(
        &mut self,
        node_idx: usize,
        sub_line: i32,
        value: &str,
        resolved_addr: u64,
        cx: &mut Context<Self>,
    ) {
        // Drop the active edit (the Set click replaces whatever was being typed).
        self.editing = None;
        self.edit_validation = None;
        self.expr_result = None;
        self.controller
            .set_node_value(node_idx, sub_line, value, false, resolved_addr);
        self.hover_popup = None;
        self.after_mutation(cx);
    }

    /// Render the open hover popup (item 13) as a small elevated card anchored near
    /// the cursor, using [`design`] tokens (no ad-hoc hex). Value-history lists the
    /// changed values newest-first; the title/body card shows disasm / hex-dump.
    fn render_hover_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.hover_popup.as_ref()?;
        let palette = EditorPalette::from_theme(cx);
        let card = match &state.kind {
            HoverPopupKind::ValueHistory {
                entries,
                node_idx,
                sub_line,
                resolved_addr,
                set_buttons,
            } => {
                let node_idx = *node_idx;
                let sub_line = *sub_line;
                let resolved_addr = *resolved_addr;
                let set_buttons = *set_buttons;
                let rows: Vec<AnyElement> = entries
                    .iter()
                    .enumerate()
                    .map(|(i, (v, age))| {
                        let mut row = div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(design::tokens::space::SM))
                            .text_size(px(self.editor_font_size()))
                            .font_family(self.editor_font_family())
                            // Newest sample reads in the bright value hue; older
                            // samples fade to the dim text (the heat-history graph).
                            .child(
                                div()
                                    .flex_grow()
                                    .text_color(if i == 0 { palette.text } else { palette.dim })
                                    .child(SharedString::from(v.clone())),
                            );
                        // Relative-age label (item 68): 'now' / 'Ns ago' / ….
                        if !age.is_empty() {
                            row = row.child(
                                div()
                                    .text_size(px(design::tokens::font::UI_XS))
                                    .text_color(palette.dim)
                                    .child(SharedString::from(age.clone())),
                            );
                        }
                        // Edit-time 'Set' button (item 68): writes this value back
                        // into the node via the controller's `set_node_value`.
                        if set_buttons && node_idx >= 0 {
                            let val = v.clone();
                            row = row.child(
                                div()
                                    .id(("vh-set", i))
                                    .px(px(4.0))
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .text_size(px(design::tokens::font::UI_XS))
                                    .text_color(palette.dim)
                                    .hover(|s| s.text_color(palette.text).bg(palette.hover_bg))
                                    .child("Set")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                                            cx.stop_propagation();
                                            this.set_value_from_history(
                                                node_idx as usize,
                                                sub_line,
                                                &val,
                                                resolved_addr,
                                                cx,
                                            );
                                        }),
                                    ),
                            );
                        }
                        row.into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(design::tokens::space::XS))
                    .child(
                        // Item 19: the "Previous Values" title carries an HLine
                        // separator beneath it (the C++ popup divider, editor.cpp:222)
                        // — a thin bottom border in the popup border hue.
                        div()
                            .text_size(px(design::tokens::font::UI_XS))
                            .text_color(palette.dim)
                            .pb(px(design::tokens::space::XS))
                            .border_b_1()
                            .border_color(palette.border)
                            // The C++ title is "Previous Values" (editor.cpp:222).
                            .child("Previous Values"),
                    )
                    .children(rows)
            }
            HoverPopupKind::TitleBody { title, body } => {
                let body_rows: Vec<AnyElement> = body
                    .lines()
                    .map(|l| {
                        div()
                            .text_size(px(self.editor_font_size()))
                            .font_family(self.editor_font_family())
                            .text_color(palette.number)
                            .child(l.to_string())
                            .into_any_element()
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(design::tokens::space::XS))
                    .child(
                        div()
                            .text_size(px(design::tokens::font::UI_XS))
                            .text_color(palette.dim)
                            .child(title.clone()),
                    )
                    .children(body_rows)
            }
        };
        Some(
            deferred(
                anchored()
                    .position(state.pos + point(px(12.0), px(16.0)))
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        card.id("rcx-hover-popup-card")
                            .bg(palette.gutter_bg)
                            .border_1()
                            .border_color(palette.border)
                            .rounded(px(design::tokens::radius::MD))
                            .px(px(design::tokens::space::SM))
                            .py(px(design::tokens::space::XS))
                            .shadow_md()
                            // Item 13: containment guard — while the cursor is over
                            // the card, set `popup_cursor_inside` so the row-level
                            // hover handler beneath does NOT dismiss the popup before
                            // a click (notably the value-history 'Set' buttons) lands.
                            // Cleared when the cursor leaves the card.
                            .on_hover(cx.listener(|this, inside: &bool, _w, _cx| {
                                this.popup_cursor_inside = *inside;
                            })),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    // ── Node context menu (reclass `customContextMenuRequested`) ──

    /// Right-mouse-down entry point invoked by [`RowElement`] (window-space
    /// position). Records the right-clicked row as the menu's [`ContextTarget`],
    /// commits any active edit, ensures the node is selected, then opens the Zed
    /// [`PopupMenu`](gpui_component::menu::PopupMenu) anchored at the cursor
    /// (reclass_right_click_on_address.png). Rows with no real node (command row /
    /// footer / synthetic) do not open a node menu.
    pub(crate) fn dispatch_row_context_menu(
        &mut self,
        line: usize,
        rel_x: f32,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        // Item 6 / B4: right-click on the command-row root struct/class KEYWORD
        // opens a "Convert to Struct" / "Convert to Class" menu (the C++ intended
        // replacement for the removed left-click keyword edit). enum has no
        // conversion. The keyword span is resolved from the composed command-row
        // text; `rel_x` is text-local (the RowElement's origin == text origin).
        if lm.line_kind == LineKind::CommandRow && lm.node_id == K_COMMAND_ROW_ID {
            let text = self.line_text_owned(line);
            let col = self.metrics.col_containing_x(rel_x);
            let rts = crate::compose::command_row_root_type_span(&text);
            if rts.valid && col >= rts.start && col < rts.end {
                let kw = text
                    .get(
                        geometry::utf16_to_byte(&text, rts.start)
                            ..geometry::utf16_to_byte(&text, rts.end),
                    )
                    .unwrap_or("")
                    .trim()
                    .to_string();
                self.open_root_convert_menu(&kw, pos, window, cx);
            } else {
                // Item 17: a command-row click OFF the keyword falls through to the
                // no-node Insert menu (the C++ no-node menu: Insert 4 / Insert 8 /
                // Append bytes…), instead of returning with no menu at all.
                self.context_target = None;
                self.open_empty_area_menu(pos, window, cx);
            }
            return;
        }
        // Only real node rows get the node menu (command/footer/synthetic rows
        // have their own affordances and no node ops). Item 17: an empty-area /
        // no-node row — including FOOTER rows — gets the C++ no-node menu (Insert 4
        // / Insert 8 / Append bytes…); the keyboard Insert actions already append at
        // the view root when there is no current node, so the menu rows reuse them.
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            self.context_target = None;
            self.open_empty_area_menu(pos, window, cx);
            return;
        }
        let target = ContextTarget {
            line,
            node_idx: lm.node_idx as usize,
            node_id: lm.node_id,
            kind: lm.node_kind,
            sub_line: lm.sub_line,
        };
        self.context_target = Some(target);

        // Right-click selects the node (single-select) if it is not already part
        // of the selection — matches the reclass behaviour where the menu acts on
        // the clicked node.
        let already_selected = self
            .controller
            .selected_ids()
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == lm.node_id);

        // Item 7/22/46: when MORE THAN ONE node is selected and the right-clicked
        // node is part of that selection, open the BATCH menu (Change to <type> for
        // N nodes, Group into Union, Delete N nodes, …) instead of the single-node
        // menu — and do NOT collapse the multi-selection to the clicked node.
        let sel_count = self.controller.selected_ids().len();
        if sel_count > 1 && already_selected {
            self.open_batch_context_menu(sel_count, pos, window, cx);
            return;
        }

        if !already_selected {
            self.controller
                .handle_node_click(line as i64, lm.node_id, CtrlMods::NONE);
            let _ = self.controller.take_events();
        }

        // Item 6: a right-click on an enum / bitfield MEMBER row opens the
        // member-specific menu (Add Member Above/Below + Remove Member for enum
        // members; Toggle Bit for bitfield members) ahead of the always-available
        // node actions. Resolve member-ness from the row's `is_member_line` +
        // `sub_line` against the node's enum/bitfield kind.
        if lm.is_member_line && target.sub_line >= 0 {
            let (is_enum_member, is_bitfield_member, bit_width) = {
                let tree = self.controller.tree();
                match tree.nodes.get(target.node_idx) {
                    Some(n) => {
                        let sl = target.sub_line as usize;
                        let is_bf = n.is_bitfield() && sl < n.bitfield_members.len();
                        // Item 9: the member's bit width — Toggle Bit is offered ONLY
                        // for a single-bit member (bitWidth == 1); a multi-bit member
                        // gets Edit Value… instead (the C++ `bm.bitWidth == 1`
                        // branch, controller.cpp:3350).
                        let bw = if is_bf {
                            n.bitfield_members[sl].bit_width
                        } else {
                            0
                        };
                        (n.is_enum() && sl < n.enum_members.len(), is_bf, bw)
                    }
                    None => (false, false, 0),
                }
            };
            if is_enum_member || is_bitfield_member {
                self.open_member_context_menu(
                    target,
                    is_enum_member,
                    is_bitfield_member,
                    bit_width,
                    pos,
                    window,
                    cx,
                );
                return;
            }
        }

        self.open_context_menu(target, pos, window, cx);
    }

    /// Item 6: the enum / bitfield MEMBER row context menu (the C++
    /// `showContextMenu` member-line branch, controller.cpp:3315). Enum members get
    /// Add Member Above / Add Member Below / Remove Member; bitfield members get
    /// Toggle Bit. Both fall through to Edit Value (the always-available member
    /// edit). Wired to the existing controller member mutators.
    fn open_member_context_menu(
        &mut self,
        // The member-menu actions read `self.context_target` (set by the caller), so
        // the menu items only need the enum/bitfield flags; the target is implicit.
        _target: ContextTarget,
        is_enum_member: bool,
        is_bitfield_member: bool,
        // Item 9: the bitfield member's bit width — gates Toggle Bit (width == 1) vs
        // Edit Value… (multi-bit). Unused for enum members.
        bit_width: u8,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, _mw, _mcx| {
            let mut menu = menu.min_w(px(200.0)).action_context(editor_focus.clone());
            if is_enum_member {
                menu = menu
                    .menu_with_icon(
                        "Add Member Above",
                        IconName::Plus,
                        Box::new(EditorMemberAddAbove),
                    )
                    .menu_with_icon(
                        "Add Member Below",
                        IconName::Plus,
                        Box::new(EditorMemberAddBelow),
                    )
                    .menu_with_icon(
                        "Remove Member",
                        IconName::Delete,
                        Box::new(EditorMemberRemove),
                    )
                    .separator()
                    // Always-available member edits: Edit Value… sets the member's
                    // integer value (the C++ member-line Value edit).
                    .menu_with_icon(
                        "Edit Value\tEnter",
                        IconName::SquareTerminal,
                        Box::new(EditorBeginValueEdit),
                    );
            }
            if is_bitfield_member {
                // Item 9: Toggle Bit ONLY for a single-bit member; a multi-bit member
                // gets Edit Value… instead (mutually exclusive — the C++
                // `bm.bitWidth == 1 ? "Toggle Bit" : "Edit Value..."`,
                // controller.cpp:3350). The C++ does NOT gate this on writability.
                if bit_width == 1 {
                    menu = menu.menu_with_icon(
                        "Toggle Bit",
                        IconName::Check,
                        Box::new(EditorMemberToggleBit),
                    );
                } else {
                    menu = menu.menu_with_icon(
                        "Edit Value...\tEnter",
                        IconName::SquareTerminal,
                        Box::new(EditorBeginValueEdit),
                    );
                }
            }
            menu
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 7/18/22/46/48: the multi-selection BATCH context menu (the C++
    /// `showContextMenu` `selCount > 1` branch). Offers Change to <type>… (the quick
    /// type-cycler applied to every selected node), Group into Union (controller
    /// `group_into_union`), Insert Above, Duplicate N, Delete N, and Copy Address.
    /// Reuses the existing batch-aware controller mutators (`quick_change_kind`,
    /// `action_duplicate`, `action_delete` already iterate the whole selection).
    fn open_batch_context_menu(
        &mut self,
        count: usize,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let show_comment = self.show_comments();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            menu.min_w(px(220.0))
                .action_context(editor_focus.clone())
                // "Change type of N nodes…" → opens the Change Type picker (applies
                // to every selected node via the batch-aware controller path).
                .menu_with_icon(
                    SharedString::from(format!("Change type of {count} nodes\u{2026}")),
                    IconName::Frame,
                    Box::new(EditorChangeType),
                )
                // Quick "Change to <hexN>" rows the C++ batch menu lists.
                .submenu("Change to", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Hex8", IconName::Frame, Box::new(EditorHex8))
                        .menu_with_icon("Hex16", IconName::Frame, Box::new(EditorHex16))
                        .menu_with_icon("Hex32", IconName::Frame, Box::new(EditorHex32))
                        .menu_with_icon("Hex64", IconName::Frame, Box::new(EditorHex64))
                        .menu_with_icon(
                            "Pointer",
                            IconName::ArrowRight,
                            Box::new(EditorQuickPointer),
                        )
                })
                .separator()
                // Item 18/48: Group into Union — wraps the selected nodes into a
                // union (controller `group_into_union`).
                .menu_with_icon(
                    "Group into Union",
                    IconName::Frame,
                    Box::new(EditorGroupIntoUnion),
                )
                .menu_with_icon("Insert Above", IconName::Plus, Box::new(EditorInsertAbove))
                .separator()
                .when(show_comment, |menu| {
                    menu.menu_with_icon(
                        SharedString::from(format!("Comment {count} nodes")),
                        IconName::SquareTerminal,
                        Box::new(EditorCommentEdit),
                    )
                })
                .menu_with_icon(
                    SharedString::from(format!("Duplicate {count} nodes\tCtrl+D")),
                    IconName::Copy,
                    Box::new(EditorDuplicate),
                )
                .menu_with_icon(
                    SharedString::from(format!("Delete {count} nodes\tDelete")),
                    IconName::Delete,
                    Box::new(EditorDelete),
                )
                .separator()
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Copy Address\tCtrl+C",
                        IconName::Copy,
                        Box::new(EditorCopyAddress),
                    )
                })
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 11/17: the no-node (empty area) context menu (the C++ `!hasNode`
    /// branch, controller.cpp:3882). Insert ▸ (Insert 4 / Insert 8 / Append bytes…),
    /// then "Add Static Field" when the view root is a Struct/Array, then the
    /// always-appended Fold / Copy / Tracking submenus the C++ adds after the
    /// hasNode/!hasNode split (controller.cpp:3913-3974). The empty-area Copy has no
    /// Address/Offset group (no node) — only Copy Line / Copy All as Text.
    fn open_empty_area_menu(
        &mut self,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        // "Add Static Field" appears only when the view root is a Struct/Array.
        let root_is_container = {
            let root_id = self.controller.view_root_id();
            let tree = self.controller.tree();
            let idx = tree.index_of_id(root_id);
            root_id != 0
                && idx >= 0
                && matches!(
                    tree.nodes[idx as usize].kind,
                    NodeKind::Struct | NodeKind::Array
                )
        };
        let track_values = self.controller.track_values();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            menu.min_w(px(200.0))
                .action_context(editor_focus.clone())
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Insert 4", IconName::Plus, Box::new(EditorInsertHex32))
                        .menu_with_icon("Insert 8", IconName::Plus, Box::new(EditorInsertHex64))
                        .separator()
                        .menu_with_icon(
                            "Append bytes…",
                            IconName::Plus,
                            Box::new(EditorAppendBytes),
                        )
                })
                // Add Static Field to the current view root (Struct/Array only).
                .when(root_is_container, |menu| {
                    menu.menu_with_icon(
                        "Add Static Field",
                        IconName::Plus,
                        Box::new(EditorRootAddStaticField),
                    )
                })
                .separator()
                // Fold ▸ — Collapse All / Expand All (whole tree).
                .submenu("Fold", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Collapse All\tCtrl+Shift+[",
                        IconName::ChevronRight,
                        Box::new(EditorCollapseAll),
                    )
                    .menu_with_icon(
                        "Expand All\tCtrl+Shift+]",
                        IconName::ChevronDown,
                        Box::new(EditorExpandAll),
                    )
                })
                // Copy ▸ — Copy Line / Copy All as Text (no node ⇒ no Address/Offset).
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Copy Line\tCtrl+X",
                        IconName::Copy,
                        Box::new(EditorCopyLine),
                    )
                    .menu_with_icon(
                        "Copy All as Text",
                        IconName::Copy,
                        Box::new(EditorCopyAllText),
                    )
                })
                // Tracking ▸ — Track Value Changes (checkable) / Clear All History.
                .submenu("Tracking", mw, mcx, move |sub, _w, _cx| {
                    sub.menu_with_check(
                        "Track Value Changes",
                        track_values,
                        Box::new(EditorTrackToggle),
                    )
                    .menu_with_icon(
                        "Clear All History",
                        IconName::Delete,
                        Box::new(EditorTrackClear),
                    )
                })
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Build + show the node context menu at `pos`. Each item dispatches one of
    /// the `Editor*` actions (handled on this view), so menu-click and the bound
    /// accelerators share one code path. Item layout mirrors the C++ menu
    /// (reclass_right_click_on_address.png): New Class · Ptr to New Class · the
    /// `← cur ↔ alt →` quick type-cycler · Rename · Change Type · Insert ▸ ·
    /// Convert ▸ · Big endian · Static ▸ · Duplicate · Delete · Fold ▸ · Copy ▸ ·
    /// Tracking ▸ · Copy as C Struct, with leading SVG icons + accelerator hints.
    fn open_context_menu(
        &mut self,
        target: ContextTarget,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cur_name = crate::core::kind_to_string(target.kind);
        let alt_name = crate::core::kind_to_string(alt_kind_for(target.kind));
        let cycle_label = format!("\u{2190} {cur_name}  \u{2194}  {alt_name} \u{2192}");
        let is_container = crate::core::is_container_kind(target.kind);
        // Item 14: label the Fold entry by the container's live collapsed state —
        // 'Expand' when collapsed, 'Collapse' when expanded (the C++ `&Expand` /
        // `&Collapse`). State-agnostic 'Toggle Fold' before.
        let fold_collapsed = {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(target.node_id);
            idx >= 0 && tree.nodes[idx as usize].collapsed
        };
        let fold_label = if fold_collapsed { "Expand" } else { "Collapse" };
        // Item 7/9: hex-node-only menu entries (Edit Bytes / Split to hexN) + the
        // node's live big-endian state for the checkable toggle (item 13).
        let is_hex_ctx = is_hex_preview(target.kind);
        let big_endian = {
            let idx = self.controller.tree().index_of_id(target.node_id);
            idx >= 0 && self.controller.tree().nodes[idx as usize].big_endian
        };
        // ── Item 11: inference-based quick-convert (the C++ `Convert to <type>` /
        // `Split into <type>xN`, controller.cpp:3471) ──
        //   For a HEX node, read the row's TypeHint chip `type_hint_kinds` (populated
        //   by compose). A single hint → "Convert to <name>" (changeNodeKind); a
        //   multi-kind hint → "Split into <name>xN". The dynamic kind(s) are stashed
        //   in `pending_hint_convert` so the parameterless menu actions can read them
        //   on click.
        let hint_kinds: Vec<NodeKind> = if is_hex_ctx {
            self.line_meta(target.line)
                .and_then(|lm| {
                    lm.chips
                        .iter()
                        .find(|c| c.kind == crate::core::linemeta::ChipKind::TypeHint)
                        .map(|c| c.type_hint_kinds.clone())
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let hint_label: Option<String> = match hint_kinds.len() {
            0 => None,
            1 => Some(format!(
                "Convert to {}",
                crate::core::kind_to_string(hint_kinds[0])
            )),
            n => Some(format!(
                "Split into {}\u{00D7}{}",
                crate::core::kind_to_string(hint_kinds[0]),
                n
            )),
        };
        let hint_is_split = hint_kinds.len() > 1;
        // Stash for the action handlers (cleared by them via `.take()`).
        self.pending_hint_convert = if hint_kinds.is_empty() {
            None
        } else {
            Some((target.node_id, hint_kinds))
        };
        // ── Menu-item gates (items 16/17/42), computed against the C++ rules ──
        let byte_size = crate::core::size_for_kind(target.kind);
        // Item 16: New Class only for NON-container kinds; Ptr to New Class also
        // requires a 4- or 8-byte node; Rename omitted for hex nodes; Big endian
        // only for scalar numeric kinds.
        let show_new_class = !is_container;
        let show_ptr_new_class = !is_container && (byte_size == 4 || byte_size == 8);
        let show_rename = !is_hex_ctx;
        let show_big_endian = is_scalar_numeric_kind(target.kind);
        // Item 17: Edit Value for writable, non-hex, non-container nodes; Comment
        // only when the Comments toggle is on.
        let writable = self.provider_writable();
        let show_edit_value = writable && !is_hex_ctx && !is_container;
        let show_comment = self.show_comments();
        // ── Item 9: Convert-submenu gates, computed by kind (controller.cpp:3631) ──
        use crate::core::NodeKind as NK;
        let k = target.kind;
        let conv_uint_label = match k {
            NK::Hex64 => Some("uint64_t\tU"),
            NK::Hex32 => Some("uint32_t\tU"),
            _ => None,
        };
        let conv_float_label = match k {
            NK::Hex64 => Some("double\tF"),
            NK::Hex32 => Some("float\tF"),
            _ => None,
        };
        // Hex16 → int16_t (S). (No uint/float quick-row for hex16 in C++.)
        let conv_int16 = matches!(k, NK::Hex16);
        // ptr\tP only when size >= 4.
        let conv_ptr = byte_size >= 4;
        let conv_fnptr64 = matches!(k, NK::Hex64 | NK::Pointer64);
        let conv_fnptr32 = matches!(k, NK::Hex32 | NK::Pointer32);
        let conv_ptr64_back = matches!(k, NK::FuncPtr64);
        let conv_ptr32_back = matches!(k, NK::FuncPtr32);
        // "Change to ptr*" — typed pointer, for 4/8-byte non-container nodes that
        // are not ALREADY a typed pointer.
        let already_typed_ptr = {
            let idx = self.controller.tree().index_of_id(target.node_id);
            idx >= 0
                && matches!(k, NK::Pointer32 | NK::Pointer64)
                && self.controller.tree().nodes[idx as usize].ref_id != 0
        };
        let conv_ptr_star =
            (byte_size == 4 || byte_size == 8) && !is_container && !already_typed_ptr;
        // Per-size Split labels (Hex128→hex64+hex64 … Hex16→hex8+hex8).
        let conv_split_label = match k {
            NK::Hex128 => Some("Split to hex64+hex64"),
            NK::Hex64 => Some("Split to hex32+hex32"),
            NK::Hex32 => Some("Split to hex16+hex16"),
            NK::Hex16 => Some("Split to hex8+hex8"),
            _ => None,
        };
        // Convert to Hex only for non-hex non-container.
        let conv_to_hex = !is_hex_ctx && !is_container;
        // Disable the whole submenu when no conversion applies.
        let convert_enabled = conv_uint_label.is_some()
            || conv_float_label.is_some()
            || conv_int16
            || conv_ptr
            || conv_fnptr64
            || conv_fnptr32
            || conv_ptr64_back
            || conv_ptr32_back
            || conv_ptr_star
            || conv_split_label.is_some()
            || conv_to_hex;
        // Gap 20: the live value-change tracking flag (drives the Tracking submenu
        // check). Read once here so the menu closure can capture it by value.
        let track_values = self.controller.track_values();

        // ── Item 8: Static-submenu gates (the C++ `Static` submenu,
        // controller.cpp:3782) ──
        //   * Add Child (Hex64) + Add Static Field: container (Struct/Array) heads.
        //   * Add Static Field (sibling): a non-container child of a Struct/Array.
        //   * Edit Expression: the node is a static field.
        //   * Dissolve Union: the node is a union, or its parent is a union.
        let (
            static_add_child,
            static_add_field_self,
            static_add_field_sibling,
            static_edit_expr,
            static_dissolve_union,
        ) = {
            let tree = self.controller.tree();
            match tree.nodes.get(target.node_idx) {
                Some(n) => {
                    let is_container_node = matches!(n.kind, NodeKind::Struct | NodeKind::Array);
                    let parent_is_container = n.parent_id != 0
                        && tree
                            .nodes
                            .get(tree.index_of_id(n.parent_id).max(0) as usize)
                            .map(|p| matches!(p.kind, NodeKind::Struct | NodeKind::Array))
                            .unwrap_or(false);
                    let add_field_sibling = !is_container_node && parent_is_container;
                    let dissolve = if n.kind == NodeKind::Struct && n.is_union() {
                        true
                    } else if n.parent_id != 0 {
                        tree.nodes
                            .get(tree.index_of_id(n.parent_id).max(0) as usize)
                            .map(|p| p.kind == NodeKind::Struct && p.is_union())
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    (
                        is_container_node,
                        is_container_node,
                        add_field_sibling,
                        n.is_static,
                        dissolve,
                    )
                }
                None => (false, false, false, false, false),
            }
        };
        let static_has_any = static_add_child
            || static_add_field_self
            || static_add_field_sibling
            || static_edit_expr
            || static_dissolve_union;

        let editor_focus = self.focus_handle.clone();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            menu.min_w(px(220.0))
                // Dispatch the menu's actions to the editor's focus context (the
                // `RcxEditor` key context that registers the `Editor*` handlers).
                .action_context(editor_focus.clone())
                // Item 16: New Class only for non-container kinds; Ptr to New Class
                // additionally requires a 4/8-byte node.
                .when(show_new_class, |menu| {
                    menu.menu_with_icon("New Class", IconName::Frame, Box::new(EditorNewClass))
                })
                .when(show_ptr_new_class, |menu| {
                    menu.menu_with_icon(
                        "Ptr to New Class",
                        IconName::ArrowRight,
                        Box::new(EditorPtrToNewClass),
                    )
                })
                .separator()
                // The "← <curType> ↔ <altType> →" quick type-cycler row: clicking
                // it cycles the node's kind forward (the C++ in-place type stepper).
                .menu_with_icon(
                    cycle_label.clone(),
                    IconName::ChevronDown,
                    Box::new(EditorCycleTypeNext),
                )
                .separator()
                // Item 17: Edit Value (Enter) for writable, non-hex, non-container.
                .when(show_edit_value, |menu| {
                    menu.menu_with_icon(
                        "Edit Value\tEnter",
                        IconName::SquareTerminal,
                        Box::new(EditorBeginValueEdit),
                    )
                })
                // Item 16/19: Rename omitted for hex nodes; F2 hint appended.
                .when(show_rename, |menu| {
                    menu.menu_with_icon(
                        "Rename\tF2",
                        IconName::SquareTerminal,
                        Box::new(EditorRename),
                    )
                })
                .menu_with_icon(
                    "Change Type\tT",
                    IconName::Frame,
                    Box::new(EditorChangeType),
                )
                // Item 17: Comment (;) only when the Comments toggle is on.
                .when(show_comment, |menu| {
                    menu.menu_with_icon(
                        "Comment\t;",
                        IconName::SquareTerminal,
                        Box::new(EditorCommentEdit),
                    )
                })
                .separator()
                // Item 15: the C++ Insert submenu offers Insert 4 Above (Hex32,
                // Shift+Ins) / Insert 8 Above (Hex64, Ins) — the keyboard already
                // maps Insert/Shift+Insert to those — plus Insert Below.
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Insert 8 Above (Hex64)\tIns",
                        IconName::Plus,
                        Box::new(EditorInsertHex64),
                    )
                    .menu_with_icon(
                        "Insert 4 Above (Hex32)\tShift+Ins",
                        IconName::Plus,
                        Box::new(EditorInsertHex32),
                    )
                    .separator()
                    .menu_with_icon("Insert Below", IconName::Plus, Box::new(EditorInsertBelow))
                    .menu_with_icon("Insert Above", IconName::Plus, Box::new(EditorInsertAbove))
                    // Item 17: the C++ Insert submenu ends with "Append bytes…".
                    .separator()
                    .menu_with_icon(
                        "Append bytes…",
                        IconName::Plus,
                        Box::new(EditorAppendBytes),
                    )
                })
                // Item 9: the C++ Convert submenu — SIZE-SPECIFIC labels by kind
                // (controller.cpp:3631), only the applicable rows, with U/F/S/P
                // hints, fnptr/ptr toggles, per-size Split labels, Convert-to-Hex
                // gating, and the whole submenu disabled when nothing applies.
                .when(!convert_enabled, |menu| {
                    menu.submenu("Convert", mw, mcx, |sub, _w, _cx| {
                        sub.label("(no conversion)")
                    })
                })
                .when(convert_enabled, |menu| {
                    menu.submenu("Convert", mw, mcx, move |mut sub, _w, _cx| {
                        if let Some(lbl) = conv_uint_label {
                            sub =
                                sub.menu_with_icon(lbl, IconName::Frame, Box::new(EditorConvUInt));
                        }
                        if let Some(lbl) = conv_float_label {
                            sub =
                                sub.menu_with_icon(lbl, IconName::Frame, Box::new(EditorConvFloat));
                        }
                        if conv_int16 {
                            sub = sub.menu_with_icon(
                                "int16_t\tS",
                                IconName::Frame,
                                Box::new(EditorConvInt),
                            );
                        }
                        if conv_ptr {
                            // Size-aware pointer (P key path: 8→Pointer64, else 32).
                            sub = sub.menu_with_icon(
                                "ptr\tP",
                                IconName::ArrowRight,
                                Box::new(EditorQuickPointer),
                            );
                        }
                        if conv_fnptr64 {
                            sub = sub.menu_with_icon(
                                "fnptr64",
                                IconName::SquareTerminal,
                                Box::new(EditorConvFnPtr64),
                            );
                        }
                        if conv_fnptr32 {
                            sub = sub.menu_with_icon(
                                "fnptr32",
                                IconName::SquareTerminal,
                                Box::new(EditorConvFnPtr32),
                            );
                        }
                        if conv_ptr64_back {
                            sub = sub.menu_with_icon(
                                "ptr64",
                                IconName::ArrowRight,
                                Box::new(EditorConvPtr64),
                            );
                        }
                        if conv_ptr32_back {
                            sub = sub.menu_with_icon(
                                "ptr32",
                                IconName::ArrowRight,
                                Box::new(EditorConvPtr32),
                            );
                        }
                        if conv_ptr_star {
                            sub = sub.separator().menu_with_icon(
                                "Change to ptr*",
                                IconName::ArrowRight,
                                Box::new(EditorConvertPtr),
                            );
                        }
                        if let Some(lbl) = conv_split_label {
                            sub = sub.menu_with_icon(
                                lbl,
                                IconName::Frame,
                                Box::new(EditorConvSplitHex),
                            );
                        }
                        if conv_to_hex {
                            sub = sub.menu_with_icon(
                                "Convert to Hex",
                                IconName::Frame,
                                Box::new(EditorConvHex),
                            );
                        }
                        sub
                    })
                })
                // Item 11: inference-based quick-convert — "Convert to <type>"
                // (single hint) or "Split into <type>xN" (multi). Shown only for a
                // hex node with a TypeHint (the C++ `lm.typeHintKinds` block,
                // controller.cpp:3471), with a trailing separator.
                .when(hint_label.is_some(), |menu| {
                    let label = SharedString::from(hint_label.clone().unwrap_or_default());
                    let action: Box<dyn gpui::Action> = if hint_is_split {
                        Box::new(EditorHintSplit)
                    } else {
                        Box::new(EditorHintConvert)
                    };
                    menu.menu_with_icon(label, IconName::Frame, action)
                        .separator()
                })
                // Item 16: Big endian only for scalar numeric kinds.
                .when(show_big_endian, |menu| {
                    menu.menu_with_check("Big endian", big_endian, Box::new(EditorToggleBigEndian))
                })
                // Item 7/42: in-place hex / ASCII overwrite editor entry points,
                // shown for hex nodes — but ONLY when the provider is writable (a
                // read-only/File-backed source must not offer in-place byte edits).
                .when(is_hex_ctx && writable, |menu| {
                    menu.menu_with_icon(
                        "Edit Bytes (Hex)",
                        IconName::SquareTerminal,
                        Box::new(EditorEditBytesHex),
                    )
                    .menu_with_icon(
                        "Edit ASCII",
                        IconName::SquareTerminal,
                        Box::new(EditorEditBytesAscii),
                    )
                })
                // Item 8: the Static submenu — real entries wired to the controller
                // static-field / StaticExpr / dissolve-union mutators (the C++
                // `Static` submenu, controller.cpp:3782). Shown only when at least
                // one entry applies; a placeholder otherwise.
                .submenu("Static", mw, mcx, move |mut sub, _w, _cx| {
                    if !static_has_any {
                        return sub.label("(no static address)");
                    }
                    if static_add_child {
                        sub = sub.menu_with_icon(
                            "Add Child",
                            IconName::Plus,
                            Box::new(EditorStaticAddChild),
                        );
                    }
                    if static_add_field_self || static_add_field_sibling {
                        sub = sub.menu_with_icon(
                            "Add Static Field",
                            IconName::Plus,
                            Box::new(EditorStaticAddField),
                        );
                    }
                    if static_edit_expr {
                        sub = sub.menu_with_icon(
                            "Edit Expression",
                            IconName::SquareTerminal,
                            Box::new(EditorStaticEditExpr),
                        );
                    }
                    if static_dissolve_union {
                        sub = sub.menu_with_icon(
                            "Dissolve Union",
                            IconName::Frame,
                            Box::new(EditorStaticDissolveUnion),
                        );
                    }
                    sub
                })
                .separator()
                .menu_with_icon(
                    "Duplicate\tCtrl+D",
                    IconName::Copy,
                    Box::new(EditorDuplicate),
                )
                .menu_with_icon("Delete\tDelete", IconName::Delete, Box::new(EditorDelete))
                .separator()
                // Item 18: Fold submenu — Toggle Fold + Collapse All / Expand All
                // (whole-tree), with the keyboard hints the C++ uses.
                .submenu("Fold", mw, mcx, move |sub, _w, _cx| {
                    // Item 14: 'Expand' / 'Collapse' per the container's state.
                    sub.menu_with_icon_and_disabled(
                        fold_label,
                        IconName::ChevronRight,
                        Box::new(EditorFold),
                        !is_container,
                    )
                    .separator()
                    .menu_with_icon(
                        "Collapse All\tCtrl+Shift+[",
                        IconName::ChevronRight,
                        Box::new(EditorCollapseAll),
                    )
                    .menu_with_icon(
                        "Expand All\tCtrl+Shift+]",
                        IconName::ChevronDown,
                        Box::new(EditorExpandAll),
                    )
                })
                // Item 17/19: the Copy submenu — Copy Address / Offset · Line / All
                // as Text, with a separator between the address/offset group and the
                // line/all group (the C++ separator), plus Ctrl+C/Ctrl+X hints.
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Copy Address\tCtrl+C",
                        IconName::Copy,
                        Box::new(EditorCopyAddress),
                    )
                    .menu_with_icon("Copy Offset", IconName::Copy, Box::new(EditorCopyOffset))
                    .separator()
                    .menu_with_icon(
                        "Copy Line\tCtrl+X",
                        IconName::Copy,
                        Box::new(EditorCopyLine),
                    )
                    .menu_with_icon(
                        "Copy All as Text",
                        IconName::Copy,
                        Box::new(EditorCopyAllText),
                    )
                })
                .submenu("Tracking", mw, mcx, move |sub, _w, _cx| {
                    // Gap 20: live value-change tracking toggle + clear-history. The
                    // controller already owns `set_track_values` / `reset_change_
                    // tracking`; these wire them (was a dead "(tracking)" label). The
                    // check reflects the current `track_values` flag.
                    sub.menu_with_check(
                        "Track Value Changes",
                        track_values,
                        Box::new(EditorTrackToggle),
                    )
                    .menu_with_icon(
                        "Clear All History",
                        IconName::Delete,
                        Box::new(EditorTrackClear),
                    )
                })
                .menu_with_icon(
                    "Copy as C Struct",
                    IconName::SquareTerminal,
                    Box::new(EditorCopyCStruct),
                )
        });

        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Mount a built [`PopupMenu`](gpui_component::menu::PopupMenu) at `pos`: wire
    /// its dismiss subscription, record it, and keep editor focus so dispatched
    /// actions land in the `RcxEditor` key context. Shared by the node menu and the
    /// command-row root-keyword convert menu (item 6).
    fn show_context_menu_at(
        &mut self,
        menu: Entity<gpui_component::menu::PopupMenu>,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Dismiss (click-away / Esc on the menu) closes + clears state.
        self._context_menu_sub =
            Some(cx.subscribe(&menu, |this, _menu, _ev: &DismissEvent, cx| {
                this.context_menu = None;
                this._context_menu_sub = None;
                cx.notify();
            }));
        self.context_menu_pos = pos;
        self.context_menu = Some(menu);
        // Keep editor focus so the menu's dispatched actions land in the
        // `RcxEditor` key context (the menu builds its actions to dispatch up the
        // focus tree; the editor is the focused element).
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Close the context menu (if any) and drop its subscription.
    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            self._context_menu_sub = None;
            cx.notify();
        }
    }

    /// The node a context-menu / accelerator action targets: the recorded
    /// right-click target, else the primary-selected row (so the accelerators work
    /// from the keyboard alone). Re-validates the recorded `node_idx` against the
    /// current tree (a prior mutation may have shifted indices).
    fn action_target(&self) -> Option<ContextTarget> {
        if let Some(t) = self.context_target {
            // Re-validate the index → id mapping still holds.
            let tree = self.controller.tree();
            if (t.node_idx) < tree.nodes.len() && tree.nodes[t.node_idx].id == t.node_id {
                return Some(t);
            }
            // Index moved: recover by id.
            let idx = tree.index_of_id(t.node_id);
            if idx >= 0 {
                let n = &tree.nodes[idx as usize];
                return Some(ContextTarget {
                    line: t.line,
                    node_idx: idx as usize,
                    node_id: t.node_id,
                    kind: n.kind,
                    sub_line: t.sub_line,
                });
            }
        }
        // Item 3: fall back to the CARET line's node (the C++ accelerators read the
        // cursor), then the primary-selected line.
        let line = self
            .caret_data_line()
            .or_else(|| self.first_selected_line())?;
        let lm = self.line_meta(line)?;
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return None;
        }
        Some(ContextTarget {
            line,
            node_idx: lm.node_idx as usize,
            node_id: lm.node_id,
            kind: lm.node_kind,
            sub_line: lm.sub_line,
        })
    }

    // ── Context-menu / accelerator action handlers ──

    fn action_new_class(&mut self, _: &EditorNewClass, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        // "New Class" (C++ controller.cpp:3390): create a populated `NewClass[_N]`
        // definition (8×Hex64) and embed THIS node as an instance of it. The old
        // path inserted a bare childless `Struct`, so expanding the new class
        // showed an EMPTY body and the arrow keys had no child rows to descend
        // into — the user-reported "it doesn't expand on that class". Resolves to
        // the caret/selected node (or 0 → new populated class as the view root).
        let node_id = self.action_target().map(|t| t.node_id).unwrap_or(0);
        self.controller.new_class_on_node(node_id);
        self.apply_document(cx);
    }

    fn action_ptr_to_new_class(
        &mut self,
        _: &EditorPtrToNewClass,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        // "Ptr to New Class": convert the target node into a typed pointer to a
        // fresh class (the C++ `convertToTypedPointer`).
        if let Some(t) = self.action_target() {
            self.controller.convert_to_typed_pointer(t.node_id);
            self.apply_document(cx);
        }
    }

    fn action_cycle_type_next(
        &mut self,
        _: &EditorCycleTypeNext,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, alt_kind_for(t.kind));
            self.apply_document(cx);
        }
    }

    fn action_cycle_type_prev(
        &mut self,
        _: &EditorCycleTypePrev,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, prev_kind_for(t.kind));
            self.apply_document(cx);
        }
    }

    fn action_rename(&mut self, _: &EditorRename, window: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        // Rename (F2): begin an inline rename on the target row's Name span (the
        // C++ menu's Rename opens the in-place name edit).
        if let Some(t) = self.action_target() {
            self.begin_inline_edit(t.line, EditTarget::Name, window, cx);
        }
    }

    fn action_change_type(
        &mut self,
        _: &EditorChangeType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.open_type_selector(t, window, cx);
        }
    }

    fn action_insert_below(
        &mut self,
        _: &EditorInsertBelow,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let (parent_id, offset) = self.insert_anchor(t.node_idx);
            self.controller
                .insert_node(parent_id, offset, NodeKind::Hex64, "");
            self.apply_document(cx);
        }
    }

    fn action_insert_above(
        &mut self,
        _: &EditorInsertAbove,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .insert_node_above(t.node_idx, NodeKind::Hex64, "");
            self.apply_document(cx);
        }
    }

    fn action_convert_ptr(
        &mut self,
        _: &EditorConvertPtr,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller.convert_to_typed_pointer(t.node_id);
            self.apply_document(cx);
        }
    }

    fn action_toggle_big_endian(
        &mut self,
        _: &EditorToggleBigEndian,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Item 13: flip the node's endianness via the undoable `ToggleBigEndian`
        // command (was a no-op stub). Resolve the node by id so the command is
        // index-shift safe, read its current `big_endian`, and push the flip.
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let idx = self.controller.tree().index_of_id(t.node_id);
            if idx >= 0 {
                let old_val = self.controller.tree().nodes[idx as usize].big_endian;
                self.controller
                    .push_command(crate::core::Command::ToggleBigEndian {
                        node_id: t.node_id,
                        old_val,
                        new_val: !old_val,
                    });
                self.after_mutation(cx);
            }
        }
    }

    /// Item 27: open the checkable Relative/Absolute offset-mode menu at `pos`.
    fn open_offset_mode_menu(
        &mut self,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let relative = self.relative_offsets;
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, _mw, _mcx| {
            menu.min_w(px(220.0))
                .action_context(editor_focus.clone())
                .menu_with_check(
                    "Relative Offsets (+0x)",
                    relative,
                    Box::new(EditorOffsetsRelative),
                )
                .menu_with_check(
                    "Absolute Addresses",
                    !relative,
                    Box::new(EditorOffsetsAbsolute),
                )
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    /// Item 27: switch the offset margin to relative `+0x` mode.
    fn action_offsets_relative(
        &mut self,
        _: &EditorOffsetsRelative,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.toggle_relative_offsets(true, cx);
    }

    /// Item 27: switch the offset margin to absolute-address mode.
    fn action_offsets_absolute(
        &mut self,
        _: &EditorOffsetsAbsolute,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.toggle_relative_offsets(false, cx);
    }

    /// Item 12: set the relative-offsets margin mode from WITHIN the editor (the
    /// margin double-click / the right-click Relative/Absolute actions), then emit
    /// [`RcxEditorEvent::ViewOptionToggled`] so the host persists the setting, sets
    /// the View-menu checkmark, and pushes the value to every open editor / split
    /// pane (the C++ `setRelativeOffsets` → `relativeOffsetsChanged`,
    /// editor.cpp:2754). No-op (and no emit) when the value is unchanged.
    fn toggle_relative_offsets(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.relative_offsets == value {
            return;
        }
        self.relative_offsets = value;
        cx.emit(RcxEditorEvent::ViewOptionToggled {
            option: EditorViewOption::RelativeOffsets,
            value,
        });
        cx.notify();
    }

    /// Item 6 / B4: convert the root class keyword to `struct` (the C++
    /// `keywordConvertRequested("struct")`). Dispatched from the command-row
    /// right-click "Convert to Struct" menu item.
    fn action_convert_root_to_struct(
        &mut self,
        _: &EditorConvertRootToStruct,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.controller.convert_root_keyword("struct");
        self.after_mutation(cx);
    }

    /// Item 6 / B4: convert the root class keyword to `class` (the C++
    /// `keywordConvertRequested("class")`).
    fn action_convert_root_to_class(
        &mut self,
        _: &EditorConvertRootToClass,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.controller.convert_root_keyword("class");
        self.after_mutation(cx);
    }

    // ── Item 9: Convert submenu quick-converts ──

    /// Change the context-target node's kind, mapping the requested family to the
    /// variant of the SAME byte size as the node (so a uint32 → "float" lands on the
    /// 4-byte float, a uint64 → "int" lands on int64, etc.). Falls back to the
    /// 4-byte member of the family when the node's size has no member.
    fn convert_target_family(&mut self, family: SizeFamily, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let size = crate::core::size_for_kind(t.kind).max(0);
            let kind = family.kind_for_size(size);
            self.controller.change_node_kind(t.node_idx, kind);
            self.after_mutation(cx);
        }
    }

    fn action_conv_uint(&mut self, _: &EditorConvUInt, _w: &mut Window, cx: &mut Context<Self>) {
        self.convert_target_family(SizeFamily::UInt, cx);
    }
    fn action_conv_int(&mut self, _: &EditorConvInt, _w: &mut Window, cx: &mut Context<Self>) {
        self.convert_target_family(SizeFamily::Int, cx);
    }
    fn action_conv_float(&mut self, _: &EditorConvFloat, _w: &mut Window, cx: &mut Context<Self>) {
        self.convert_target_family(SizeFamily::Float, cx);
    }
    fn action_conv_ptr64(&mut self, _: &EditorConvPtr64, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, NodeKind::Pointer64);
            self.after_mutation(cx);
        }
    }
    fn action_conv_ptr32(&mut self, _: &EditorConvPtr32, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, NodeKind::Pointer32);
            self.after_mutation(cx);
        }
    }
    fn action_conv_fnptr64(
        &mut self,
        _: &EditorConvFnPtr64,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, NodeKind::FuncPtr64);
            self.after_mutation(cx);
        }
    }
    fn action_conv_fnptr32(
        &mut self,
        _: &EditorConvFnPtr32,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .change_node_kind(t.node_idx, NodeKind::FuncPtr32);
            self.after_mutation(cx);
        }
    }
    fn action_conv_hex(&mut self, _: &EditorConvHex, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            // Convert to the hex equivalent of the node's current byte size.
            let kind = hex_kind_for_size(crate::core::size_for_kind(t.kind).max(1));
            self.controller.change_node_kind(t.node_idx, kind);
            self.after_mutation(cx);
        }
    }
    fn action_conv_split_hex(
        &mut self,
        _: &EditorConvSplitHex,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller.split_hex_node(t.node_id);
            self.apply_document(cx);
        }
    }

    // ── Item 11: type-inference quick-convert (Convert to / Split into) ──

    /// "Convert to <type>" (single type hint): change the captured hex node to the
    /// single suggested kind (the C++ `changeNodeKind(ni, suggested)`,
    /// controller.cpp:3478). Reads the kind stashed in `pending_hint_convert`.
    fn action_hint_convert(
        &mut self,
        _: &EditorHintConvert,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some((node_id, kinds)) = self.pending_hint_convert.take() {
            if let Some(&k) = kinds.first() {
                let idx = self.controller.tree().index_of_id(node_id);
                if idx >= 0 {
                    self.controller.change_node_kind(idx as usize, k);
                    self.after_mutation(cx);
                }
            }
        }
    }

    /// "Split into <type>xN" (multiple type hints): change the captured hex node to
    /// the first kind, then for each remaining kind change the NEXT sibling node if
    /// it is still a hex node (the C++ split loop, controller.cpp:3487). Re-resolves
    /// the node index each step (a kind change can shift indices).
    fn action_hint_split(&mut self, _: &EditorHintSplit, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        let Some((node_id, kinds)) = self.pending_hint_convert.take() else {
            return;
        };
        if kinds.is_empty() {
            return;
        }
        // Mirror the C++ split loop (controller.cpp:3487): change the node to the
        // first kind, then for each remaining kind change the NEXT sibling node iff
        // it is still a hex node. Re-resolve the node index each step (a kind change
        // can shift indices).
        let idx = self.controller.tree().index_of_id(node_id);
        if idx >= 0 {
            self.controller.change_node_kind(idx as usize, kinds[0]);
        }
        for k in kinds.iter().skip(1) {
            let ni = self.controller.tree().index_of_id(node_id);
            if ni < 0 {
                break;
            }
            let next = ni as usize + 1;
            let next_is_hex = self
                .controller
                .tree()
                .nodes
                .get(next)
                .map(|n| is_hex_preview(n.kind))
                .unwrap_or(false);
            if next_is_hex {
                self.controller.change_node_kind(next, *k);
            } else {
                break;
            }
        }
        self.after_mutation(cx);
    }

    // ── Item 7: in-place hex / ASCII overwrite editor entry points ──

    fn action_edit_bytes_hex(
        &mut self,
        _: &EditorEditBytesHex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            // begin_inline_edit detects a hex Value and arms HexOverwrite::Hex.
            self.begin_inline_edit(t.line, EditTarget::Value, window, cx);
        }
    }
    fn action_edit_bytes_ascii(
        &mut self,
        _: &EditorEditBytesAscii,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.begin_ascii_overwrite_edit(t.line, t.node_idx as i32, window, cx);
        }
    }

    // ── Item 17: Copy submenu ──

    fn action_copy_offset(
        &mut self,
        _: &EditorCopyOffset,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            // Item 4: copy the node's LOCAL `.offset` field (its offset within its
            // parent), formatted "+0x" + uppercase-hex right-justified to 4 digits —
            // NOT the absolute composed offset (the C++ `Copy &Offset`,
            // controller.cpp:3940). e.g. node.offset 8 → "+0x0008".
            let idx = self.controller.tree().index_of_id(t.node_id);
            if idx >= 0 {
                let off = self.controller.tree().nodes[idx as usize].offset;
                cx.write_to_clipboard(ClipboardItem::new_string(format!("+0x{off:04X}")));
            }
        }
    }
    fn action_copy_line(&mut self, _: &EditorCopyLine, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let text = self.line_text_owned(t.line);
            cx.write_to_clipboard(ClipboardItem::new_string(text.trim_end().to_string()));
        }
    }
    fn action_copy_all_text(
        &mut self,
        _: &EditorCopyAllText,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        // Compose every line's text into one block (the C++ "Copy All as Text").
        let result = self.controller.last_result();
        let mut out = String::new();
        for i in 0..result.meta.len() {
            out.push_str(self.line_text_owned(i).trim_end());
            out.push('\n');
        }
        cx.write_to_clipboard(ClipboardItem::new_string(out));
    }

    /// Gap 20: toggle live value-change tracking (the Tracking submenu "Track Value
    /// Changes" check). Flips the controller's `track_values` flag.
    fn action_track_toggle(
        &mut self,
        _: &EditorTrackToggle,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let on = !self.controller.track_values();
        self.controller.set_track_values(on);
        self.after_mutation(cx);
    }

    /// Gap 20: clear all recorded value-change history (the Tracking submenu "Clear
    /// All History"). Resets the controller's per-node change tracking.
    fn action_track_clear(
        &mut self,
        _: &EditorTrackClear,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.controller.reset_change_tracking();
        self.after_mutation(cx);
    }

    // ── Item 6: enum / bitfield member menu actions ──

    /// Add an enum member ABOVE the context-target member (the C++ "Add Member
    /// Above": insert before `sub_line`).
    fn action_member_add_above(
        &mut self,
        _: &EditorMemberAddAbove,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.context_target {
            if t.sub_line >= 0
                && self
                    .controller
                    .add_member(t.node_id, Some(t.sub_line as usize))
            {
                self.after_mutation(cx);
            }
        }
    }

    /// Add an enum member BELOW the context-target member (insert at `sub_line+1`).
    fn action_member_add_below(
        &mut self,
        _: &EditorMemberAddBelow,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.context_target {
            if t.sub_line >= 0
                && self
                    .controller
                    .add_member(t.node_id, Some(t.sub_line as usize + 1))
            {
                self.after_mutation(cx);
            }
        }
    }

    /// Remove the context-target enum member (the C++ "Remove Member").
    fn action_member_remove(
        &mut self,
        _: &EditorMemberRemove,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.context_target {
            if t.sub_line >= 0
                && self
                    .controller
                    .delete_member(t.node_id, t.sub_line as usize)
            {
                self.after_mutation(cx);
            }
        }
    }

    /// Toggle one bit of the context-target bitfield member (the C++ "Toggle Bit" →
    /// `toggleBitfieldBit`).
    fn action_member_toggle_bit(
        &mut self,
        _: &EditorMemberToggleBit,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.context_target {
            if t.sub_line >= 0 {
                self.controller
                    .toggle_bitfield_bit(t.node_id, t.sub_line as usize);
                self.after_mutation(cx);
            }
        }
    }

    /// Item 7/18/48: group the current multi-selection into a Union (the C++
    /// `group_into_union`). No-op for a selection of fewer than 2 nodes.
    fn action_group_into_union(
        &mut self,
        _: &EditorGroupIntoUnion,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let ids: std::collections::HashSet<u64> = self.selected_root_ids().into_iter().collect();
        if ids.len() >= 2 {
            self.controller.group_into_union(&ids);
            self.apply_document(cx);
        }
    }

    // ── Item 8: Static submenu actions ──

    /// "Add Child" — insert a Hex64 child at the container head's offset 0 (the C++
    /// `insertNode(nodeId, 0, Hex64, "newField")`).
    fn action_static_add_child(
        &mut self,
        _: &EditorStaticAddChild,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller
                .insert_node(t.node_id, 0, NodeKind::Hex64, "newField");
            self.apply_document(cx);
        }
    }

    /// "Add Static Field" — add a static field to the target container (or, for a
    /// non-container child of a struct/array, to its parent), the C++
    /// `insertStaticField`.
    fn action_static_add_field(
        &mut self,
        _: &EditorStaticAddField,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let parent = {
                let tree = self.controller.tree();
                match tree.nodes.get(t.node_idx) {
                    Some(n) if matches!(n.kind, NodeKind::Struct | NodeKind::Array) => n.id,
                    Some(n) => n.parent_id,
                    None => 0,
                }
            };
            if parent != 0 {
                self.controller.insert_static_field(parent);
                self.apply_document(cx);
            }
        }
    }

    /// Item 11: "Add Static Field" from the no-node (empty-area) menu — add a
    /// static field to the current VIEW ROOT, when it is a Struct/Array (the C++
    /// `!hasNode` branch, controller.cpp:3904).
    fn action_root_add_static_field(
        &mut self,
        _: &EditorRootAddStaticField,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let root_id = self.controller.view_root_id();
        if root_id == 0 {
            return;
        }
        let is_container = {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(root_id);
            idx >= 0
                && matches!(
                    tree.nodes[idx as usize].kind,
                    NodeKind::Struct | NodeKind::Array
                )
        };
        if is_container {
            self.controller.insert_static_field(root_id);
            self.apply_document(cx);
        }
    }

    /// "Edit Expression" — open the inline StaticExpr edit on the static field's
    /// row (the C++ `beginInlineEdit(StaticExpr, line)`).
    fn action_static_edit_expr(
        &mut self,
        _: &EditorStaticEditExpr,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.begin_inline_edit(t.line, EditTarget::StaticExpr, window, cx);
        }
    }

    /// "Dissolve Union" — flatten the target union (or the target's parent union)
    /// back into its parent scope (the C++ `dissolveUnion`).
    fn action_static_dissolve_union(
        &mut self,
        _: &EditorStaticDissolveUnion,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            let union_id = {
                let tree = self.controller.tree();
                match tree.nodes.get(t.node_idx) {
                    Some(n) if n.kind == NodeKind::Struct && n.is_union() => n.id,
                    Some(n) if n.parent_id != 0 => {
                        let pi = tree.index_of_id(n.parent_id);
                        if pi >= 0
                            && tree.nodes[pi as usize].kind == NodeKind::Struct
                            && tree.nodes[pi as usize].is_union()
                        {
                            n.parent_id
                        } else {
                            0
                        }
                    }
                    _ => 0,
                }
            };
            if union_id != 0 {
                self.controller.dissolve_union(union_id);
                self.apply_document(cx);
            }
        }
    }

    /// Item 17: Append bytes… — append a single field to the enclosing container
    /// of the current/target node, falling back to the view-root struct when no
    /// node is selected (the C++ `appendSingleFieldRequested` / the no-node
    /// "Append bytes" row).
    fn action_append_bytes(
        &mut self,
        _: &EditorAppendBytes,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let anchor = self
            .action_target()
            .map(|t| t.node_id)
            .or_else(|| self.current_node().map(|(_, lm)| lm.node_id))
            .unwrap_or_else(|| self.controller.view_root_id());
        if anchor != 0 {
            self.controller.append_single_field(anchor);
        } else {
            self.controller.insert_node(
                self.controller.view_root_id(),
                -1,
                NodeKind::Hex64,
                "field",
            );
        }
        self.context_target = None;
        self.after_mutation(cx);
    }

    /// Item 7: open the ASCII overwrite editor on the hex node at `(line,
    /// node_idx)`. Seeds the field with the node's ASCII preview (one printable
    /// char per byte) and arms [`HexOverwrite::Ascii`].
    fn begin_ascii_overwrite_edit(
        &mut self,
        line: usize,
        node_idx: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if node_idx < 0 {
            return;
        }
        let Some(lm) = self.line_meta(line).cloned() else {
            return;
        };
        if !is_hex_preview(lm.node_kind) {
            return;
        }
        let byte_count = if lm.line_byte_count > 0 {
            lm.line_byte_count as usize
        } else {
            crate::core::size_for_kind(lm.node_kind).max(0) as usize
        };
        if byte_count == 0 {
            return;
        }
        // Read the live bytes for the ASCII seed.
        let (addr, _ok) = self.controller.tree().absolute_address(node_idx);
        let data = self
            .controller
            .document()
            .provider
            .read_bytes(addr, byte_count as i32);
        let seed: String = (0..byte_count)
            .map(|i| {
                let b = data.get(i).copied().unwrap_or(0);
                if (0x20..=0x7E).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        // The value span resolves the overlay column; reuse begin_inline_edit's
        // span resolution by opening a plain Value edit then re-seeding + switching
        // to ASCII overwrite mode.
        self.begin_inline_edit(line, EditTarget::Value, window, cx);
        if let Some(editing) = self.editing.as_mut() {
            let field = editing.field.clone();
            // Item 5: mark this edit as an ASCII byte-overwrite so the Value commit
            // writes per-byte ASCII (is_ascii=true) rather than parsing the text as
            // a hex value (which silently failed before).
            editing.ascii_overwrite = true;
            field.update(cx, |f, _cx| {
                f.set_ascii_overwrite(&seed, byte_count);
            });
            cx.notify();
        }
    }

    /// Item 6 / B4: build + show the root-keyword conversion menu at `pos`. The
    /// menu offers exactly the C++ option set: a `struct` root offers "Convert to
    /// Class"; a `class` root offers "Convert to Struct"; an `enum` root offers
    /// nothing (no menu shown).
    fn open_root_convert_menu(
        &mut self,
        keyword: &str,
        pos: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor_focus = self.focus_handle.clone();
        let kw = keyword.to_string();
        // enum: no conversion options — do not open a menu.
        if kw != "struct" && kw != "class" {
            return;
        }
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, _mw, _mcx| {
            let menu = menu.min_w(px(200.0)).action_context(editor_focus.clone());
            if kw == "class" {
                menu.menu_with_icon(
                    "Convert to Struct",
                    IconName::Frame,
                    Box::new(EditorConvertRootToStruct),
                )
            } else {
                menu.menu_with_icon(
                    "Convert to Class",
                    IconName::Frame,
                    Box::new(EditorConvertRootToClass),
                )
            }
        });
        self.show_context_menu_at(menu, pos, window, cx);
    }

    fn action_duplicate(&mut self, _: &EditorDuplicate, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        // Gap 82 / the C++ `duplicateSelectedRequested` (controller.cpp:496):
        // duplicate EVERY selected node, not just the single target. Resolve the
        // selection to node ids first (duplicate_node shifts indices as it
        // inserts, so re-resolve each id → index right before duplicating). A
        // right-click context target OUTSIDE the selection still duplicates just
        // that one row (the menu acts on the clicked node).
        let mut ids: Vec<u64> = self.selected_root_ids();
        if let Some(t) = self.context_target {
            if !ids.contains(&t.node_id) {
                ids = vec![t.node_id];
            }
        }
        if ids.is_empty() {
            if let Some(t) = self.action_target() {
                self.controller.duplicate_node(t.node_idx);
                self.apply_document(cx);
            }
            return;
        }
        for nid in ids {
            let idx = self.controller.tree().index_of_id(nid);
            if idx >= 0 {
                self.controller.duplicate_node(idx as usize);
            }
        }
        self.apply_document(cx);
    }

    fn action_delete(&mut self, _: &EditorDelete, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        // Item 6: delete the ENTIRE multi-selection, not just the single target.
        // The C++ `Key_Delete` emits `deleteSelectedRequested` whenever the
        // selection set is non-empty (iterating every selected node). Gather all
        // selected node indices and remove them highest-first so earlier indices
        // stay valid across the splices. A right-click context target that is NOT
        // part of the selection still deletes just that node (the menu acts on the
        // clicked row).
        let mut idxs = self.selected_node_indices_ordered();
        if idxs.is_empty() {
            // Fall back to the single context/primary target (menu Delete on an
            // unselected right-clicked row).
            if let Some(t) = self.action_target() {
                idxs.push(t.node_idx);
            }
        } else if let Some(t) = self.context_target {
            // A right-click on a row OUTSIDE the current selection targets just
            // that node (dispatch_row_context_menu only auto-selects when the row
            // was unselected, so if it is not in the set, honor the single target).
            if !idxs.contains(&t.node_idx) {
                idxs = vec![t.node_idx];
            }
        }
        if idxs.is_empty() {
            return;
        }
        // Item 2 (blocker): a multi-node delete must go through
        // `batch_remove_nodes` (controller.rs:3777), which normalizes the set
        // (`normalize_prefer_ancestors`, so selecting a parent struct AND its
        // child does not double-remove via a now-stale child index), wraps the
        // removals in a single "Delete N nodes" undo macro, and clears the
        // selection. The hand-rolled descending loop produced N undo entries and
        // skipped normalization.
        if idxs.len() > 1 {
            self.controller.batch_remove_nodes(&idxs);
        } else {
            self.controller.remove_node(idxs[0]);
        }
        self.controller.clear_selection();
        self.context_target = None;
        self.apply_document(cx);
    }

    fn action_fold(&mut self, _: &EditorFold, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            // BUG #2: a CYCLE/self-ref head materializes its referenced struct's
            // children inline; a plain container head toggles collapse (the C++
            // `handleMarginClick`, controller.cpp:5893). Resolve M_CYCLE from the
            // targeted row's LineMeta.
            let is_cycle = self
                .line_meta(t.line)
                .map(|lm| (lm.marker_mask & (1u32 << crate::core::linemeta::M_CYCLE)) != 0)
                .unwrap_or(false);
            if is_cycle {
                self.controller.materialize_ref_children(t.node_idx);
            } else {
                self.controller.toggle_collapse(t.node_idx);
            }
            self.caret_line = Some(t.line);
            self.after_mutation(cx);
        }
    }

    fn action_copy_c_struct(
        &mut self,
        _: &EditorCopyCStruct,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        // Item 1 (blocker): render the view-root struct via the C++ `renderCpp`
        // generator and put the C source on the clipboard. main.cpp:2955 resolves
        // `viewRootId()`, falling back to the first top-level (`parentId==0`)
        // `Struct` when the view root is unset, then `renderCpp(tree, rootId,
        // aliases)` and `setAppStatus("Copied C struct to clipboard")`.
        let mut root = self.controller.view_root_id();
        if root == 0 {
            let tree = self.controller.tree();
            for n in &tree.nodes {
                if n.parent_id == 0 && n.kind == NodeKind::Struct {
                    root = n.id;
                    break;
                }
            }
        }
        if root == 0 {
            return;
        }
        let source = {
            let tree = self.controller.tree();
            let aliases = &self.controller.document().type_aliases;
            let aliases = if aliases.is_empty() {
                None
            } else {
                Some(aliases)
            };
            crate::generator::render_cpp(tree, root, aliases, /* emit_asserts */ false)
        };
        if source.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(source));
        cx.emit(RcxEditorEvent::Status {
            message: "Copied C struct to clipboard".to_string(),
        });
    }

    /// The `(parent_id, offset)` to insert a sibling *after* `node_idx`: same
    /// parent, offset just past the node so the new member lands right below it.
    fn insert_anchor(&self, node_idx: usize) -> (u64, i32) {
        let tree = self.controller.tree();
        if node_idx >= tree.nodes.len() {
            return (self.controller.view_root_id(), -1);
        }
        let n = &tree.nodes[node_idx];
        let size = crate::core::size_for_kind(n.kind).max(0);
        (n.parent_id, n.offset + size)
    }

    // ── Change Type → TypeSelector popup (contract: menus agent PROVIDES) ──

    /// Open the [`TypeSelectorPopup`](crate::ui::typeselectorpopup::TypeSelectorPopup)
    /// over `target`'s current kind and subscribe to its outcome. On
    /// [`Chosen`](crate::ui::typeselectorpopup::TypeSelectorEvent::Chosen) apply the
    /// kind via `change_node_kind` then the chosen [`Modifier`]
    /// (pointer/array/etc.) via the matching controller ops + `apply_document`; on
    /// Cancel close the dialog. Opened through `window.open_dialog` (the same
    /// pattern window.rs uses for the command palette).
    fn open_type_selector(
        &mut self,
        target: ContextTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Change-Type / `T` is the field-type flow (modifiers allowed).
        self.open_type_selector_in_mode(target, EditTarget::Type, window, cx);
    }

    /// The full type catalogue: the built-in primitives + every user-declared
    /// composite (struct/class/enum) in the tree (item 6 — "composites + user
    /// structs"). Composites are appended after the primitives, mirroring the C++
    /// catalogue. `exclude_id` drops a struct from the list (so a struct cannot
    /// reference itself).
    ///
    /// In [`TypePopupMode::PointerTarget`] mode the synthetic **void** entry is
    /// prepended (item 43): a `Hex8`-backed primitive named "void" so a Ctrl+Click
    /// pointer-retarget can pick void (the C++ `PointerTarget` `voidEntry`,
    /// controller.cpp:4724). It applies as `refId = 0` via the PointerTarget branch
    /// of `apply_type_popup_result` (a primitive entry ⇒ refId 0).
    fn full_type_entries(
        &self,
        exclude_id: u64,
        mode: crate::ui::typeselectorpopup::TypePopupMode,
    ) -> Vec<crate::ui::typeselectorpopup::TypeEntry> {
        use crate::ui::typeselectorpopup::{default_type_entries, TypeEntry, TypePopupMode};
        let mut entries: Vec<TypeEntry> = Vec::new();
        if mode == TypePopupMode::PointerTarget {
            // Synthetic "void" target — a Hex8-backed primitive applied as refId 0.
            let mut void = TypeEntry::primitive(NodeKind::Hex8, "void");
            void.enabled = true;
            entries.push(void);
        }
        entries.extend(default_type_entries());
        let tree = self.controller.tree();
        let mut composites: Vec<TypeEntry> = Vec::new();
        for n in tree.nodes.iter() {
            // Named composite declarations (a struct with a type name), excluding
            // the self-reference target.
            if n.kind == NodeKind::Struct && !n.struct_type_name.is_empty() && n.id != exclude_id {
                // Composite size is the struct's actual byte extent (sum/extent of
                // its children) — matching C++ `e.sizeBytes = structSpan(n.id)`
                // (controller.cpp:4555) which feeds the popup size bar/preview
                // (typeselectorpopup.cpp:1515-1557), *not* the flat `size_for_kind`.
                let size = tree.struct_span(n.id).max(0);
                let keyword = if n.class_keyword.is_empty() {
                    "struct"
                } else {
                    n.class_keyword.as_str()
                };
                composites.push(TypeEntry::composite(
                    n.id,
                    &n.struct_type_name,
                    keyword,
                    size,
                ));
            }
        }
        // Dedup composites by type name (the same struct can appear via several
        // pointer refs); keep the first occurrence.
        composites.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        composites.dedup_by(|a, b| a.display_name == b.display_name);
        entries.extend(composites);
        entries
    }

    /// Open the Type Selector for `target` in the mode implied by `edit_target`
    /// (item 6): `Type` → FieldType (modifiers), `ArrayElementType` → ArrayElement
    /// (modifiers), `PointerTarget` → PointerTarget (no modifiers). The catalogue
    /// includes composites + primitives.
    fn open_type_selector_in_mode(
        &mut self,
        target: ContextTarget,
        edit_target: EditTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::typeselectorpopup::TypePopupMode;
        let mode = match edit_target {
            EditTarget::ArrayElementType => TypePopupMode::ArrayElement,
            EditTarget::PointerTarget => TypePopupMode::PointerTarget,
            _ => TypePopupMode::FieldType,
        };
        let entries = self.full_type_entries(target.node_id, mode);
        self.spawn_type_selector(entries, target, mode, window, cx);
    }

    /// Open the Root-mode Type Selector (item 2): the class-header chevron switches
    /// the *viewed* struct. We list every declared composite (Root mode hides the
    /// `*`/`[]` modifiers); on Chosen the kind is applied to the root node via
    /// [`apply_type_choice`].
    fn open_root_type_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::typeselectorpopup::TypePopupMode;
        let root_id = self.controller.view_root_id();
        let root_idx = self.controller.tree().index_of_id(root_id);
        let kind = if root_idx >= 0 {
            self.controller.tree().nodes[root_idx as usize].kind
        } else {
            NodeKind::Struct
        };
        let target = ContextTarget {
            line: 0,
            node_idx: root_idx.max(0) as usize,
            node_id: root_id,
            kind,
            sub_line: 0,
        };
        // Root mode lists every declared composite so the user can re-root onto a
        // different struct (do NOT exclude the current root — it may be re-picked).
        let entries = self.full_type_entries(0, TypePopupMode::Root);
        self.spawn_type_selector(entries, target, TypePopupMode::Root, window, cx);
    }

    /// Shared opener: build the [`TypeSelectorPopup`] over `entries`, set `mode`,
    /// subscribe to its outcome (apply via [`apply_type_choice`]), and float it
    /// through `window.open_dialog`.
    fn spawn_type_selector(
        &mut self,
        entries: Vec<crate::ui::typeselectorpopup::TypeEntry>,
        target: ContextTarget,
        mode: crate::ui::typeselectorpopup::TypePopupMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::typeselectorpopup::{TypePopupMode, TypeSelectorEvent, TypeSelectorPopup};
        // The popup opens pre-highlighting the node's ACTUAL current type (the C++
        // `setTypes(.., &currentEntry)`): for a composite that means the referenced
        // struct id (pre-select by structId), for a primitive the kind. Also compute
        // the C++ footer-size baseline (`nodeSize = sizeForKind(node.kind)`, or the
        // ELEMENT kind in ArrayElement mode) + the tree's pointer size, and feed the
        // recent-type names so the "Recent" section appears (items 38/39/40/41).
        let (node_size, ptr_size, cur_struct_id) = {
            let tree = self.controller.tree();
            let ps = tree.pointer_size;
            let idx = tree.index_of_id(target.node_id);
            if idx >= 0 {
                let n = &tree.nodes[idx as usize];
                let sz = if mode == TypePopupMode::ArrayElement {
                    crate::core::size_for_kind(n.element_kind)
                } else {
                    crate::core::size_for_kind(n.kind)
                };
                // The node already references a composite when its `ref_id` is set
                // (typed pointer / embedded struct / array-of-struct).
                (sz, ps, n.ref_id)
            } else {
                (crate::core::size_for_kind(target.kind), ps, 0u64)
            }
        };
        let recent = self.recent_type_names.clone();
        let popup = cx.new(|cx| {
            let mut p = TypeSelectorPopup::new_with_current(entries, target.kind, window, cx);
            p.set_mode(mode, cx);
            p.set_sizes(node_size, ptr_size);
            p.set_recent_names(recent, cx);
            // Pre-highlight the composite the node already references, by structId
            // (the C++ `m_currentEntry.entryKind == Composite` branch). For a plain
            // primitive node this is 0 and the kind pre-select (in new_with_current)
            // stands.
            if cur_struct_id != 0 {
                p.set_current_struct(cur_struct_id, cx);
            }
            p
        });
        let focus = popup.read(cx).focus_handle(cx);
        let node_id = target.node_id;
        self._type_selector_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &TypeSelectorEvent, window, cx| match ev {
                TypeSelectorEvent::Chosen {
                    kind,
                    modifier,
                    create_new,
                    entry_kind,
                    struct_id,
                    display_name,
                } => {
                    window.close_dialog(cx);
                    this._type_selector_sub = None;
                    if *create_new {
                        // "+ New": create a populated NewClass[_N] (8×Hex64) and
                        // embed THIS node as an instance of it (same as the editor
                        // "New Class" action) — not a bare empty struct.
                        this.controller.new_class_on_node(node_id);
                        this.apply_document(cx);
                    } else {
                        this.apply_type_choice(
                            mode,
                            node_id,
                            *kind,
                            *modifier,
                            *entry_kind,
                            *struct_id,
                            display_name,
                            cx,
                        );
                    }
                }
                TypeSelectorEvent::Cancel => {
                    window.close_dialog(cx);
                    this._type_selector_sub = None;
                }
            },
        ));
        let popup_for_modal = popup.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .w(px(420.))
                .margin_top(px(120.))
                .close_button(false)
                .child(popup_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Apply a TypeSelector choice by routing the WHOLE pick through the
    /// controller's `apply_type_popup_result` with a faithful `TypePopupChoice`
    /// (items 35/36/37). This is the single C++-parity apply path: it carries the
    /// composite identity (`struct_id` / `display_name`) so selecting an EXISTING
    /// composite references it by id (not a bare empty Struct); routes a primitive
    /// `*` through `is_valid_primitive_ptr_target` (element_kind + ptr_depth, NOT
    /// `convert_to_typed_pointer`); and keeps the `refId` for a composite `[]`
    /// array. The optional `modifier` (`*`/`**`/`[N]`) becomes the `full_text`
    /// suffix that `apply_type_popup_result` parses into a `TypeSpec`.
    #[allow(clippy::too_many_arguments)]
    fn apply_type_choice(
        &mut self,
        mode: crate::ui::typeselectorpopup::TypePopupMode,
        node_id: u64,
        kind: NodeKind,
        modifier: Option<crate::ui::typeselectorpopup::Modifier>,
        entry_kind: crate::ui::typeselectorpopup::EntryKind,
        struct_id: u64,
        display_name: &str,
        cx: &mut Context<Self>,
    ) {
        use crate::controller::{TypeEntryKind, TypePopupChoice, TypePopupMode as CMode};
        use crate::ui::typeselectorpopup::{EntryKind, Modifier, TypePopupMode};

        // Map the popup's mode → the controller's mode (same four cases).
        let cmode = match mode {
            TypePopupMode::Root => CMode::Root,
            TypePopupMode::FieldType => CMode::FieldType,
            TypePopupMode::ArrayElement => CMode::ArrayElement,
            TypePopupMode::PointerTarget => CMode::PointerTarget,
        };

        // The base display name: a primitive uses its canonical kind name, a
        // composite the struct/enum display name carried from the popup row.
        let base_name = if entry_kind == EntryKind::Composite && !display_name.is_empty() {
            display_name.to_string()
        } else {
            crate::core::kind_to_string(kind).to_string()
        };
        // The modifier suffix (`*` / `**` / `[N]`) → the `full_text` the controller
        // parses (`acceptCurrent` `fullText`); empty modifier ⇒ derive from name.
        let full_text = match modifier {
            Some(m @ (Modifier::Pointer | Modifier::PointerPointer | Modifier::Array(_))) => {
                format!("{}{}", base_name, m.suffix())
            }
            Some(Modifier::None) | None => String::new(),
        };

        let choice = TypePopupChoice {
            entry_kind: if entry_kind == EntryKind::Composite {
                TypeEntryKind::Composite
            } else {
                TypeEntryKind::Primitive
            },
            primitive_kind: kind,
            struct_id,
            display_name: base_name.clone(),
            full_text,
            create_new: false,
        };

        // Record the pick in the recent-types list (the C++ `pushRecentType` on
        // apply) so a subsequent open surfaces it in the "Recent" section.
        self.push_recent_type(&base_name);

        self.controller
            .apply_type_popup_result(cmode, node_id, choice);
        self.apply_document(cx);
    }

    /// Push a picked type `display_name` to the front of the recent-types list,
    /// dedup-to-front and capped at 8 (the C++ `RcxController::pushRecentType`,
    /// controller.cpp:4819). Empty names are ignored.
    fn push_recent_type(&mut self, display_name: &str) {
        push_recent_type_into(&mut self.recent_type_names, display_name);
    }

    // ── Data-source picker (SourceChooserPopup; items 1/5, contract CONSUMES) ──

    /// Open the [`SourceChooserPopup`] over the controller's saved sources +
    /// providers (the class-header `source▾` chip click, item 1). Subscribes to the
    /// [`SourceChooserEvent`] and applies the pick through the controller's
    /// data-source API: `SourceSelected` → `switch_to_saved_source`,
    /// `ClearRequested` → `clear_sources`. Provider selection needs the app shell's
    /// file/attach dialogs (out of the editor's ownership), so it closes cleanly
    /// (the documented stub the controller itself uses for plugin sources). Opened
    /// through `window.open_dialog`, mirroring the type selector + command palette.
    fn open_source_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Build the `(name, kind_label, active)` recent tuples from the controller's
        // saved sources, flagging the active one with its checkmark (PIC4).
        let active = self.controller.active_source_index();
        let recent: Vec<(String, String, bool)> = self
            .controller
            .saved_sources()
            .iter()
            .enumerate()
            .map(|(i, s)| (s.display_name.clone(), s.kind.clone(), i as i32 == active))
            .collect();
        let popup = SourceChooserPopup::view(recent, window, cx);
        let focus = popup.read(cx).focus_handle(cx);
        self._source_chooser_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &SourceChooserEvent, window, cx| {
                use crate::ui::sourcechooser::SourcePick;
                window.close_dialog(cx);
                this._source_chooser_sub = None;
                match ev {
                    SourceChooserEvent::Pick(SourcePick::SavedSource(idx)) => {
                        this.controller.switch_to_saved_source(*idx);
                        this.after_mutation(cx);
                    }
                    SourceChooserEvent::Clear => {
                        this.controller.clear_sources();
                        this.after_mutation(cx);
                    }
                    // Provider activation (Open File / Kernel / Process / …) drives
                    // the app shell's file/attach dialogs, which the editor does not
                    // own; close cleanly (documented stub, like plugin sources).
                    SourceChooserEvent::Pick(SourcePick::Provider(_))
                    | SourceChooserEvent::OpenFile
                    | SourceChooserEvent::Cancel => {
                        cx.notify();
                    }
                }
            },
        ));
        let popup_for_modal = popup.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .w(px(360.))
                .margin_top(px(80.))
                .close_button(false)
                .child(popup_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    // ── Enum-value picker (EnumPickerPopup; item 8) ──

    /// Whether node `idx` is an enum (its resolved class keyword is `enum`).
    fn node_is_enum(&self, idx: usize) -> bool {
        self.controller
            .tree()
            .nodes
            .get(idx)
            .is_some_and(|n| n.is_enum())
    }

    /// Open the [`EnumPickerPopup`] for the enum field at `idx` (item 8). Builds the
    /// member list from the node's `enum_members`, pre-selecting the current value,
    /// and on Chosen writes the value back through `set_node_value`.
    fn open_enum_picker(
        &mut self,
        line: usize,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::enumpicker::{EnumPickerEvent, Member};
        let (enum_name, members, current) = {
            let n = &self.controller.tree().nodes[idx];
            let members: Vec<Member> = n
                .enum_members
                .iter()
                .map(|(name, value)| Member::new(name, *value))
                .collect();
            // The current value: read it through the value-history snapshot if any,
            // else default to 0 (the picker pre-selects the nearest member).
            (n.struct_type_name.clone(), members, 0i64)
        };
        if members.is_empty() {
            // No members → fall back to a plain inline value edit.
            self.begin_inline_edit(line, EditTarget::Value, window, cx);
            return;
        }
        let resolved_addr = self.line_meta(line).map(|lm| lm.offset_addr).unwrap_or(0);
        let sub_line = self.line_meta(line).map(|lm| lm.sub_line).unwrap_or(0);
        let popup = cx.new(|cx| {
            crate::ui::enumpicker::EnumPickerPopup::new(&enum_name, members, current, window, cx)
        });
        let focus = popup.read(cx).focus_handle(cx);
        self._enum_picker_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &EnumPickerEvent, window, cx| match ev {
                EnumPickerEvent::Chosen(value) => {
                    window.close_dialog(cx);
                    this._enum_picker_sub = None;
                    this.controller.set_node_value(
                        idx,
                        sub_line,
                        &value.to_string(),
                        false,
                        resolved_addr,
                    );
                    this.after_mutation(cx);
                }
                EnumPickerEvent::Dismissed => {
                    window.close_dialog(cx);
                    this._enum_picker_sub = None;
                }
            },
        ));
        let popup_for_modal = popup.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .w(px(360.))
                .margin_top(px(120.))
                .close_button(false)
                .child(popup_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    // ── Hex size toolbar (HexToolbarPopup; item 9) ──

    /// Build the [`HexPopupContext`] for the hex node at `idx`: its current kind +
    /// raw bytes + up to 15 adjacent same-parent hex nodes (for join previews).
    fn build_hex_context(&self, idx: usize) -> Option<crate::ui::hextoolbar::HexPopupContext> {
        use crate::ui::hextoolbar::{Adjacent, HexPopupContext};
        let tree = self.controller.tree();
        let n = tree.nodes.get(idx)?;
        if !is_hex_preview(n.kind) {
            return None;
        }
        let node_id = n.id;
        let kind = n.kind;
        let parent_id = n.parent_id;
        let size = crate::core::size_for_kind(kind).max(0);
        let (addr, _ok) = tree.absolute_address(idx as i32);
        let provider = &self.controller.document().provider;
        let data = provider.read_bytes(addr, size);
        // Adjacent same-parent hex siblings after this node (for join previews).
        let siblings = tree.children_of(parent_id);
        let mut nexts: Vec<Adjacent> = Vec::new();
        if let Some(pos) = siblings.iter().position(|&s| s == idx) {
            for &sib in siblings.iter().skip(pos + 1).take(15) {
                let sn = &tree.nodes[sib];
                if !is_hex_preview(sn.kind) {
                    break;
                }
                let sz = crate::core::size_for_kind(sn.kind).max(0);
                let (saddr, _) = tree.absolute_address(sib as i32);
                let bytes = provider.read_bytes(saddr, sz);
                nexts.push(Adjacent {
                    exists: true,
                    kind: sn.kind,
                    data: bytes,
                });
            }
        }
        Some(HexPopupContext {
            node_id,
            current_kind: kind,
            data,
            nexts,
            ..HexPopupContext::default()
        })
    }

    /// Open the [`HexToolbarPopup`] for the hex node at `idx` (item 9). On
    /// `SizeSelected` apply the size change via `split_hex_node` (smaller) or
    /// `join_hex_nodes` (larger); Insert above/below + dismiss route accordingly.
    fn open_hex_toolbar(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        use crate::ui::hextoolbar::{HexToolbarEvent, HexToolbarPopup};
        let Some(ctx) = self.build_hex_context(idx) else {
            return;
        };
        let node_id = ctx.node_id;
        let popup = cx.new(|cx| HexToolbarPopup::new(ctx, window, cx));
        let focus = popup.read(cx).focus_handle(cx);
        self._hex_toolbar_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &HexToolbarEvent, window, cx| match ev {
                HexToolbarEvent::SizeSelected(id, kind)
                | HexToolbarEvent::SuggestKind(id, kind) => {
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    this.apply_hex_size(*id, *kind, cx);
                }
                HexToolbarEvent::InsertAbove(id) => {
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    let i = this.controller.tree().index_of_id(*id);
                    if i >= 0 {
                        this.controller
                            .insert_node_above(i as usize, NodeKind::Hex64, "");
                        this.apply_document(cx);
                    }
                }
                HexToolbarEvent::InsertBelow(id) => {
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    let i = this.controller.tree().index_of_id(*id);
                    if i >= 0 {
                        let (parent_id, offset) = this.insert_anchor(i as usize);
                        this.controller
                            .insert_node(parent_id, offset, NodeKind::Hex64, "");
                        this.apply_document(cx);
                    }
                }
                HexToolbarEvent::JoinSelected => {
                    // Item 10: actually JOIN the contiguous hex run starting at the
                    // toolbar's anchor node (was a close-only no-op). Sum the
                    // anchor's size plus its consecutive same-parent hex siblings,
                    // pick the largest hex kind that fits the total, and join. The
                    // anchor is `node_id` captured when the toolbar opened.
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    this.join_hex_run(node_id, cx);
                }
                HexToolbarEvent::FillToOffset(id, offset) => {
                    // Item 11: actually FILL the gap from the node's end up to the
                    // typed offset with padding hex nodes (was a close-only no-op).
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    this.fill_to_offset(*id, *offset, cx);
                }
                HexToolbarEvent::Dismissed => {
                    window.close_dialog(cx);
                    this._hex_toolbar_sub = None;
                    cx.notify();
                }
            },
        ));
        let popup_for_modal = popup.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .w(px(320.))
                .margin_top(px(140.))
                .close_button(false)
                .child(popup_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Item 10: join the contiguous hex run starting at `anchor_id`. Sums the
    /// anchor's byte size plus its consecutive same-parent hex siblings (the run the
    /// toolbar's "join selected" affordance acts on), picks the largest hex
    /// [`NodeKind`] whose size fits the accumulated total, and routes to the
    /// controller `join_hex_nodes`. A single hex node (no following hex sibling) is a
    /// no-op (nothing to join).
    fn join_hex_run(&mut self, anchor_id: u64, cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        let ni = tree.index_of_id(anchor_id);
        if ni < 0 {
            return;
        }
        let anchor = &tree.nodes[ni as usize];
        if !is_hex_preview(anchor.kind) {
            return;
        }
        let parent_id = anchor.parent_id;
        let mut total = crate::core::size_for_kind(anchor.kind).max(0);
        let mut next_off = anchor.offset + total;
        // Walk consecutive same-parent hex siblings by offset.
        loop {
            let found = tree.nodes.iter().enumerate().find(|(_, s)| {
                s.parent_id == parent_id
                    && s.offset == next_off
                    && crate::core::kind::is_hex_node(s.kind)
            });
            let Some((_, s)) = found else { break };
            let sz = crate::core::size_for_kind(s.kind).max(0);
            if sz <= 0 {
                break;
            }
            total += sz;
            next_off += sz;
        }
        // Largest hex kind that fits the accumulated run.
        let target = hex_kind_for_size(total);
        let target_sz = crate::core::size_for_kind(target);
        if target_sz <= crate::core::size_for_kind(self.controller.tree().nodes[ni as usize].kind) {
            return; // nothing larger to join into
        }
        self.controller.join_hex_nodes(anchor_id, target);
        self.apply_document(cx);
    }

    /// Item 11: fill the gap from the hex node's end offset up to `target_offset`
    /// with padding hex nodes. Resolves the node, computes `cur_end = offset + size`,
    /// then inserts `Hex64`/`Hex8` fields (hex64 chunks then an 8-byte tail split as
    /// needed) into the parent at increasing offsets until the gap is covered. A
    /// non-positive gap is a no-op.
    fn fill_to_offset(&mut self, node_id: u64, target_offset: u64, cx: &mut Context<Self>) {
        let (parent_id, mut cur) = {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(node_id);
            if idx < 0 {
                return;
            }
            let n = &tree.nodes[idx as usize];
            let size = crate::core::size_for_kind(n.kind).max(0) as u64;
            (n.parent_id, n.offset as u64 + size)
        };
        if target_offset <= cur || parent_id == 0 {
            return;
        }
        // Insert padding hex nodes covering [cur, target_offset). Prefer 8-byte
        // Hex64 chunks; the remainder is filled with Hex8 single bytes. Guard the
        // loop count so a pathological gap can't spin forever.
        let mut guard = 0;
        while cur < target_offset && guard < 4096 {
            let remaining = target_offset - cur;
            let (kind, step) = if remaining >= 8 {
                (NodeKind::Hex64, 8u64)
            } else {
                (NodeKind::Hex8, 1u64)
            };
            self.controller.insert_node(parent_id, cur as i32, kind, "");
            cur += step;
            guard += 1;
        }
        self.apply_document(cx);
    }

    /// Apply a hex-toolbar size choice (item 9): a smaller target splits the node,
    /// a larger target joins it with the following same-kind siblings, same size is
    /// a no-op. Routes to the controller's `split_hex_node` / `join_hex_nodes`.
    fn apply_hex_size(&mut self, node_id: u64, target: NodeKind, cx: &mut Context<Self>) {
        let idx = self.controller.tree().index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let cur = self.controller.tree().nodes[idx as usize].kind;
        let cur_sz = crate::core::size_for_kind(cur);
        let tgt_sz = crate::core::size_for_kind(target);
        if tgt_sz == cur_sz {
            // Same size (e.g. a suggested non-hex kind): change kind directly.
            if target != cur {
                self.controller.change_node_kind(idx as usize, target);
            }
        } else if tgt_sz < cur_sz {
            self.controller.split_hex_node(node_id);
            // After a split the node becomes the next smaller hex; if the target is
            // smaller still, the user can split again from the refreshed toolbar.
        } else {
            self.controller.join_hex_nodes(node_id, target);
        }
        self.apply_document(cx);
    }
}

/// Reduce a composed [`LineMeta`] to a minimap bar: a fill color (by node kind /
/// line role) plus indent + width fractions so the overview reads the tree shape
/// (item 4). Chrome rows (command/footer) render as faint full-width bars; node
/// rows tint by kind (struct/array/pointer/fnptr/hex/value) and inset by depth.
/// Pure (palette in, bar out) — unit-tested.
fn minimap_row_for(lm: &LineMeta, palette: &EditorPalette) -> minimap::MinimapRow {
    use crate::core::NodeKind::*;
    // Depth → left indent fraction (cap so very deep rows still show a bar).
    let indent = (lm.depth.max(0) as f32 * 0.08).min(0.5);
    let (color, width) = match lm.line_kind {
        LineKind::CommandRow => (with_alpha(palette.class_name, 0.85), 0.9),
        LineKind::Footer => (with_alpha(palette.dim, 0.5), 0.5),
        LineKind::Header => {
            // Struct/array container headers: the loud type hue, near-full width.
            let c = match lm.node_kind {
                Array => palette.type_fg,
                _ => palette.class_name,
            };
            (c, 0.85)
        }
        _ => {
            // Field rows: color by kind, matching the gutter icon semantics.
            let c = match lm.node_kind {
                Struct => palette.class_name,
                Array => palette.type_fg,
                Pointer32 | Pointer64 => palette.keyword,
                FuncPtr32 | FuncPtr64 => palette.fnptr_fg,
                Hex8 | Hex16 | Hex32 | Hex64 | Hex128 => palette.dim,
                _ => palette.value_fg,
            };
            (c, 0.7)
        }
    };
    minimap::MinimapRow {
        color: with_alpha(color, color.a.max(0.7)),
        indent,
        width: (width - indent * 0.5).max(0.15),
    }
}

/// A scalar type family for the Convert submenu (item 9): the requested int /
/// uint / float, resolved to the variant matching the node's current byte size.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SizeFamily {
    Int,
    UInt,
    Float,
}

impl SizeFamily {
    /// The family member of `size` bytes; falls back to the 4-byte member when the
    /// size has no member (e.g. float has no 1-byte variant).
    fn kind_for_size(self, size: i32) -> NodeKind {
        use NodeKind::*;
        match self {
            SizeFamily::Int => match size {
                1 => Int8,
                2 => Int16,
                4 => Int32,
                8 => Int64,
                16 => Int128,
                _ => Int32,
            },
            SizeFamily::UInt => match size {
                1 => UInt8,
                2 => UInt16,
                4 => UInt32,
                8 => UInt64,
                16 => UInt128,
                _ => UInt32,
            },
            SizeFamily::Float => match size {
                2 => Float16,
                4 => Float,
                8 => Double,
                _ => Float,
            },
        }
    }
}

/// The largest hex [`NodeKind`] whose byte size is `<= bytes` (item 10 join). Hex
/// kinds are 1/2/4/8/16 bytes (Hex8/16/32/64/128); a run total maps to the biggest
/// hex that fits so a join collapses the run into the widest hex cell.
fn hex_kind_for_size(bytes: i32) -> NodeKind {
    match bytes {
        b if b >= 16 => NodeKind::Hex128,
        b if b >= 8 => NodeKind::Hex64,
        b if b >= 4 => NodeKind::Hex32,
        b if b >= 2 => NodeKind::Hex16,
        _ => NodeKind::Hex8,
    }
}

/// The ordered list of kinds that share `kind`'s byte size (item 16): the
/// same-size variant ring the quick type-cycler steps through. Only primitive,
/// fixed-size scalar/pointer kinds participate — containers (Struct/Array) and
/// dynamic-size kinds (size 0) are excluded so the cycler never lands on a
/// container or a string/dynamic type of a different footprint. Table order is
/// preserved so the ring is stable.
fn same_size_variants(kind: NodeKind) -> Vec<NodeKind> {
    let size = crate::core::size_for_kind(kind);
    if size <= 0 {
        return vec![kind];
    }
    let ring: Vec<NodeKind> = crate::core::K_KIND_META
        .iter()
        .map(|m| m.kind)
        .filter(|&k| {
            !matches!(k, NodeKind::Struct | NodeKind::Array)
                && crate::core::size_for_kind(k) == size
        })
        .collect();
    if ring.is_empty() {
        vec![kind]
    } else {
        ring
    }
}

/// The "alternate" kind for the quick type-cycler (`← cur ↔ alt →`) and the
/// forward `T`-less cycle (item 16): steps to the NEXT same-byte-size variant,
/// wrapping. Cycling between equal-footprint primitives (e.g. int32_t → uint32_t →
/// float → hex32 …) never lands on a different size / container / string — the C++
/// in-place stepper keeps the node's byte layout fixed.
fn alt_kind_for(kind: NodeKind) -> NodeKind {
    let ring = same_size_variants(kind);
    let pos = ring.iter().position(|&k| k == kind).unwrap_or(0);
    ring[(pos + 1) % ring.len()]
}

/// Item 16: whether the "Big endian" checkable item applies — only scalar numeric
/// kinds, exactly the C++ set (controller.cpp:3763): `Hex16..=Hex128`,
/// `Int16..=UInt128`, `Float16`, `Float`, `Double`. Never Hex8, bool, ptr/fnptr,
/// struct/array/enum/bitfield/string/vector.
/// Push a picked type `display_name` to the FRONT of `list`, removing any prior
/// occurrence (dedup-to-front) and capping the list at 8 entries — the C++
/// `RcxController::pushRecentType` (controller.cpp:4819). Empty names are ignored.
/// Free so it is unit-testable without a gpui view (item 3/11 recent-types list).
fn push_recent_type_into(list: &mut Vec<String>, display_name: &str) {
    if display_name.is_empty() {
        return;
    }
    list.retain(|n| n != display_name);
    list.insert(0, display_name.to_string());
    list.truncate(8);
}

fn is_scalar_numeric_kind(kind: NodeKind) -> bool {
    use crate::core::NodeKind::*;
    matches!(
        kind,
        Hex16
            | Hex32
            | Hex64
            | Hex128
            | Int16
            | Int32
            | Int64
            | Int128
            // Item 6: UInt8 sits between Int128 and UInt16 in the C++ enum order,
            // so the C++ predicate `node.kind in [Int16 .. UInt128]` SPANS UInt8 —
            // the Big-endian item shows for a UInt8 node. (Int8, below Int16, is
            // NOT in range and stays excluded, matching the C++.)
            | UInt8
            | UInt16
            | UInt32
            | UInt64
            | UInt128
            | Float16
            | Float
            | Double
    )
}

/// The previous same-size variant (the `←` half of the cycler), wrapping (item 16).
fn prev_kind_for(kind: NodeKind) -> NodeKind {
    let ring = same_size_variants(kind);
    let pos = ring.iter().position(|&k| k == kind).unwrap_or(0);
    ring[(pos + ring.len() - 1) % ring.len()]
}

/// Parse a typed base-address string into `(numeric_base, formula)` for the
/// command-row `ChangeBase` commit (BUG 1). A pure hex literal (`0x7FF6...`,
/// `7FF6...`, or a plain decimal) sets the numeric base and clears the formula;
/// anything containing an operator / module name (`app.exe + 0x1A0`,
/// `[app.exe + 0x58]`, `ntdll!Sym`) is kept as a *formula* string (the numeric
/// base is left at `fallback` until a live source resolves it). Empty input
/// resets to base 0 with no formula. Pure (no gpui) — unit-tested below.
fn parse_base_address(text: &str, fallback: u64) -> (u64, String) {
    let t = text.trim();
    if t.is_empty() {
        return (0, String::new());
    }
    // A bare numeric literal: hex (`0x…`/`…h`/plain hex) or decimal.
    if let Some(n) = parse_pure_number(t) {
        return (n, String::new());
    }
    // Otherwise treat the whole thing as a base-address formula; keep the current
    // numeric base so the gutter math stays sane until a source resolves it.
    (fallback, t.to_string())
}

/// Parse a *bare* numeric literal as a `u64`, or `None` if it is not a plain
/// number (so the caller can treat it as a formula). Accepts `0x`-prefixed hex, a
/// trailing-`h` hex, all-hex-digit strings, and plain decimal.
fn parse_pure_number(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    if let Some(hex) = t.strip_suffix('h').or_else(|| t.strip_suffix('H')) {
        return u64::from_str_radix(hex, 16).ok();
    }
    // All hex digits (no operators / module chars) → hex; else plain decimal.
    if t.chars().all(|c| c.is_ascii_hexdigit()) {
        // Prefer hex when any a-f digit is present; otherwise it is ambiguous
        // decimal/hex — addresses are hex by convention (the tooltip says "all
        // numbers are hexadecimal"), so parse as hex.
        return u64::from_str_radix(t, 16).ok();
    }
    None
}

impl Focusable for RcxEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<RcxEditorEvent> for RcxEditor {}

impl Render for RcxEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Measure the monospace cell once per frame so hit-test/overlay column math
        // matches the painted glyph grid EXACTLY. Two things were wrong before and
        // each shifted the inline-edit box / mouse hit-test right of the painted
        // token (worse on the name/value columns since the error accumulates):
        //   1) the font: the rows below are painted with the editor MONO font at
        //      `EDITOR_SIZE` (see `.font_family(MONO_FAMILY).text_size(..)`), but the
        //      metric was measured against `window.text_style()` — the ambient UI
        //      font at a larger size — so every cell was ~0.6px too wide.
        //   2) the sample: a lone `shape_line("0")` reports the glyph's full width
        //      incl. side-bearing, not the run advance the painter uses; measure a
        //      RUN of 10 and divide.
        // Measure with the SAME explicit font + size the rows use, and take the line
        // height from the editor's own value (not the ambient `window.line_height`).
        // Items 13/44: measure against the EFFECTIVE font family + the zoomed size
        // so the cell metrics (and thus hit-testing) track the chosen View > Font
        // and the Ctrl+wheel zoom.
        const PROBE: &str = "0000000000";
        let font_family = self.editor_font_family();
        let font_size = self.editor_font_size();
        let editor_font_size = px(font_size);
        let line_height = font_size * EDITOR_LINE_HEIGHT;
        let cell_width = {
            let run = TextRun {
                len: PROBE.len(),
                font: gpui::font(font_family.clone()),
                color: cx.theme().foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                PROBE.into(),
                editor_font_size,
                std::slice::from_ref(&run),
                None,
            );
            let w = f32::from(shaped.width()) / PROBE.len() as f32;
            if w > 0.0 {
                w
            } else {
                line_height * DEFAULT_CELL_RATIO
            }
        };
        self.metrics = CellMetrics::new(cell_width, line_height);

        let count = self.controller.last_result().meta.len();
        let palette = EditorPalette::from_theme(cx);

        div()
            .id("rcx-editor")
            .track_focus(&self.focus_handle)
            .key_context("RcxEditor")
            .relative()
            .size_full()
            .bg(palette.paper)
            .text_color(palette.text)
            // A real monospace at the comfortable Zed editor size + generous
            // leading (the prior build was cramped). The measured `line_height`
            // above derives from exactly these so hit-testing stays aligned.
            .font_family(font_family.clone())
            .text_size(px(font_size))
            .line_height(px(font_size * EDITOR_LINE_HEIGHT))
            .on_action(cx.listener(Self::action_tab))
            .on_action(cx.listener(Self::action_tab_prev))
            .on_action(cx.listener(Self::action_escape))
            .on_action(cx.listener(Self::action_undo))
            .on_action(cx.listener(Self::action_redo))
            // Node context-menu / accelerator action handlers (menu-click and the
            // bound F2/T/Ctrl+D/Delete accelerators dispatch the same actions).
            .on_action(cx.listener(Self::action_new_class))
            .on_action(cx.listener(Self::action_ptr_to_new_class))
            .on_action(cx.listener(Self::action_cycle_type_next))
            .on_action(cx.listener(Self::action_cycle_type_prev))
            .on_action(cx.listener(Self::action_rename))
            .on_action(cx.listener(Self::action_change_type))
            .on_action(cx.listener(Self::action_insert_below))
            .on_action(cx.listener(Self::action_insert_above))
            .on_action(cx.listener(Self::action_convert_ptr))
            .on_action(cx.listener(Self::action_convert_root_to_struct))
            .on_action(cx.listener(Self::action_convert_root_to_class))
            .on_action(cx.listener(Self::action_offsets_relative))
            .on_action(cx.listener(Self::action_offsets_absolute))
            .on_action(cx.listener(Self::action_conv_uint))
            .on_action(cx.listener(Self::action_conv_int))
            .on_action(cx.listener(Self::action_conv_float))
            .on_action(cx.listener(Self::action_conv_ptr64))
            .on_action(cx.listener(Self::action_conv_ptr32))
            .on_action(cx.listener(Self::action_conv_fnptr64))
            .on_action(cx.listener(Self::action_conv_fnptr32))
            .on_action(cx.listener(Self::action_conv_hex))
            .on_action(cx.listener(Self::action_conv_split_hex))
            .on_action(cx.listener(Self::action_edit_bytes_hex))
            .on_action(cx.listener(Self::action_edit_bytes_ascii))
            .on_action(cx.listener(Self::action_copy_offset))
            .on_action(cx.listener(Self::action_copy_line))
            .on_action(cx.listener(Self::action_copy_all_text))
            .on_action(cx.listener(Self::action_track_toggle))
            .on_action(cx.listener(Self::action_track_clear))
            .on_action(cx.listener(Self::action_append_bytes))
            .on_action(cx.listener(Self::action_zoom_in))
            .on_action(cx.listener(Self::action_zoom_out))
            .on_action(cx.listener(Self::action_zoom_reset))
            // Item 6: enum/bitfield member menu actions.
            .on_action(cx.listener(Self::action_member_add_above))
            .on_action(cx.listener(Self::action_member_add_below))
            .on_action(cx.listener(Self::action_member_remove))
            .on_action(cx.listener(Self::action_member_toggle_bit))
            // Item 7/18/48: group multi-selection into a union.
            .on_action(cx.listener(Self::action_group_into_union))
            // Item 8: Static submenu actions.
            .on_action(cx.listener(Self::action_static_add_child))
            .on_action(cx.listener(Self::action_static_add_field))
            .on_action(cx.listener(Self::action_static_edit_expr))
            .on_action(cx.listener(Self::action_static_dissolve_union))
            .on_action(cx.listener(Self::action_root_add_static_field))
            .on_action(cx.listener(Self::action_hint_convert))
            .on_action(cx.listener(Self::action_hint_split))
            .on_action(cx.listener(Self::action_toggle_big_endian))
            .on_action(cx.listener(Self::action_duplicate))
            .on_action(cx.listener(Self::action_delete))
            .on_action(cx.listener(Self::action_fold))
            .on_action(cx.listener(Self::action_copy_c_struct))
            .on_action(cx.listener(Self::action_find))
            // Normal-mode quick keys (item 12) + same-size cycle (18) + F12 (20).
            .on_action(cx.listener(Self::action_quick_pointer))
            .on_action(cx.listener(Self::action_quick_float))
            .on_action(cx.listener(Self::action_quick_signed))
            .on_action(cx.listener(Self::action_quick_unsigned))
            .on_action(cx.listener(Self::action_hex_cycle_next))
            .on_action(cx.listener(Self::action_hex_cycle_prev))
            .on_action(cx.listener(Self::action_hex8))
            .on_action(cx.listener(Self::action_hex16))
            .on_action(cx.listener(Self::action_hex32))
            .on_action(cx.listener(Self::action_hex64))
            .on_action(cx.listener(Self::action_hex128))
            .on_action(cx.listener(Self::action_nav_up))
            .on_action(cx.listener(Self::action_nav_down))
            .on_action(cx.listener(Self::action_nav_add_up))
            .on_action(cx.listener(Self::action_nav_add_down))
            .on_action(cx.listener(Self::action_move_up))
            .on_action(cx.listener(Self::action_move_down))
            .on_action(cx.listener(Self::action_nav_page_up))
            .on_action(cx.listener(Self::action_nav_page_down))
            .on_action(cx.listener(Self::action_nav_home))
            .on_action(cx.listener(Self::action_nav_end))
            .on_action(cx.listener(Self::action_begin_value_edit))
            .on_action(cx.listener(Self::action_insert_hex64))
            .on_action(cx.listener(Self::action_insert_hex32))
            .on_action(cx.listener(Self::action_comment_edit))
            .on_action(cx.listener(Self::action_cycle_left))
            .on_action(cx.listener(Self::action_cycle_right))
            .on_action(cx.listener(Self::action_go_to_definition))
            .on_action(cx.listener(Self::action_collapse_all))
            .on_action(cx.listener(Self::action_expand_all))
            .on_action(cx.listener(Self::action_collapse_node))
            .on_action(cx.listener(Self::action_expand_node))
            // Shift-extending navigation (items 2/3) + Ctrl+A (5) + node
            // clipboard (4) + copy-address (10).
            .on_action(cx.listener(Self::action_select_up))
            .on_action(cx.listener(Self::action_select_down))
            .on_action(cx.listener(Self::action_select_page_up))
            .on_action(cx.listener(Self::action_select_page_down))
            .on_action(cx.listener(Self::action_select_home))
            .on_action(cx.listener(Self::action_select_end))
            .on_action(cx.listener(Self::action_select_all))
            .on_action(cx.listener(Self::action_copy_nodes))
            .on_action(cx.listener(Self::action_cut_nodes))
            .on_action(cx.listener(Self::action_paste_nodes))
            .on_action(cx.listener(Self::action_copy_address))
            .on_mouse_down_out(cx.listener(|this, _e: &MouseDownEvent, window, cx| {
                // Clicking outside the editor commits an active edit.
                if this.editing.is_some() {
                    this.commit_active_edit(window, cx);
                }
            }))
            // Item 20: on left mouse-up, flush any DEFERRED click (a plain click on
            // an already-selected node within a multi-selection) — collapsing to
            // the clicked node — unless a drag already consumed it. Also clears the
            // drag latch so the next gesture starts fresh.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _e: &MouseUpEvent, _w, cx| {
                    this.flush_pending_click(cx);
                }),
            )
            // BUG #1 (item 1): Ctrl/Cmd + mouse-wheel zooms the editor font (the
            // QScintilla-native Ctrl+wheel zoom). Wheel-up = zoom-in, ±1pt per
            // notch, clamped 6..48 by `zoom_by`. When Ctrl/Cmd is held we consume
            // the event (stop_propagation) so it does NOT also scroll the list;
            // otherwise we return WITHOUT stopping so the uniform_list keeps its
            // native vertical scroll.
            .on_scroll_wheel(cx.listener(|this, ev: &gpui::ScrollWheelEvent, _w, cx| {
                if !(ev.modifiers.control || ev.modifiers.platform) {
                    return; // plain wheel → let the list scroll natively.
                }
                let dy = match ev.delta {
                    gpui::ScrollDelta::Lines(p) => p.y,
                    gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
                };
                if dy > 0.0 {
                    this.zoom_by(1.0, cx);
                } else if dy < 0.0 {
                    this.zoom_by(-1.0, cx);
                }
                cx.stop_propagation();
            }))
            // Item 9: when the pointer leaves the editor surface, clear the hover
            // band + node id and dismiss any hover popup (the C++ mouse-leave /
            // `dismissAllPopups`). `on_hover` fires `false` on leave. Without this
            // the band sticks on the last hovered row.
            .on_hover(cx.listener(|this, inside: &bool, _w, cx| {
                if !*inside {
                    this.clear_hover_state(cx);
                }
            }))
            // The find bar (Ctrl+F) floats over the top of the editor when open
            // (item 4): a thin overlaid strip that takes input focus, navigates to
            // matches, and drives the per-line highlight painted in build_row_paint.
            .when_some(self.find_bar.clone(), |this, bar| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .mt(px(design::tokens::space::SM))
                        .mr(px(design::tokens::space::MD))
                        .child(bar),
                )
            })
            // Body: the virtualized row list (flex-1) and, when toggled, the
            // right-side minimap overview column (item 4). A flex row keeps the
            // minimap pinned to the right edge without overlapping the rows.
            // Item 75: in DEBUG view the row grid is replaced by the read-only debug
            // dump surface (margin + annotated text + per-line LineMeta).
            .when(self.debug_view, |this| {
                this.child(self.render_debug_surface(cx))
            })
            .when(!self.debug_view, |this| {
                this.child(
                    div()
                        .size_full()
                        .flex()
                        .flex_row()
                        .child(
                            uniform_list(
                                "rcx-rows",
                                count,
                                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                                    range.map(|ix| this.render_row(ix, cx)).collect::<Vec<_>>()
                                }),
                            )
                            .flex_grow()
                            .h_full()
                            .track_scroll(&self.scroll),
                        )
                        .when(self.minimap, |this| {
                            this.child(self.build_minimap(palette, cx))
                        }),
                )
            })
            // The node context menu: a Zed PopupMenu floated at the right-click
            // position via the deferred-overlay pattern (gpui_cookbook.md §"deferred
            // overlays"), so it draws above the rows and snaps within the window.
            .when_some(self.context_menu.clone(), |this, menu| {
                let pos = self.context_menu_pos;
                this.child(
                    deferred(
                        anchored()
                            .position(pos)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(1),
                )
            })
            // Hover popups (item 13): value-history / disasm / hex-dump card,
            // floated near the cursor over the hovered value column.
            .children(self.render_hover_popup(cx))
    }
}

/// Apply an alpha to an `Hsla` (heat/byte-sel overlays are translucent fills).
fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}

/// Structural equality for two hover popup kinds (item 13) — used to avoid
/// re-notifying when the cursor moves within the same popup target.
fn hover_kind_eq(a: &HoverPopupKind, b: &HoverPopupKind) -> bool {
    match (a, b) {
        (
            HoverPopupKind::ValueHistory {
                entries: la,
                set_buttons: sa,
                ..
            },
            HoverPopupKind::ValueHistory {
                entries: lb,
                set_buttons: sb,
                ..
            },
        ) => {
            // Compare only the VALUE column (ignore the relative-age labels, which
            // tick) + the Set-button mode — the C++ `vals == m_values` test. This
            // avoids constant popup re-creation as the '12s ago' labels advance.
            sa == sb
                && la.len() == lb.len()
                && la.iter().zip(lb.iter()).all(|((va, _), (vb, _))| va == vb)
        }
        (
            HoverPopupKind::TitleBody {
                title: ta,
                body: ba,
            },
            HoverPopupKind::TitleBody {
                title: tb,
                body: bb,
            },
        ) => ta == tb && ba == bb,
        _ => false,
    }
}

/// The address-format hover popover (reclass_address_hover.png + PIC5 "Base
/// Address"): a small Zed elevated card listing the accepted base-address formats
/// + the operator/hex hints. Built inline with [`design`] tokens (no ad-hoc hex);
/// shown when the user hovers the class-header base-address region. Rendered as a
/// gpui tooltip view (the `.tooltip(..)` closure returns this entity).
pub struct AddressFormatTooltip {
    /// The current base address (shown as the worked "hex address" example).
    base_address: u64,
    /// The active data-source / module label (e.g. `app.exe`), for the module
    /// examples; empty falls back to a generic `<module>`.
    module: SharedString,
}

impl AddressFormatTooltip {
    fn module_name(&self) -> String {
        let m = self.module.trim();
        if m.is_empty() {
            "<module>".to_string()
        } else {
            m.to_string()
        }
    }
}

impl Render for AddressFormatTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use design::{color, tokens};
        let module = self.module_name();
        // Item 22: the C++ `BaseAddress` tooltip uses a FIXED literal example
        // (`0x7FF61234ABCD`), not the live base, and module placeholders
        // (`<app.exe>`). The format rows: each is (example, dim explanation).
        let _ = self.base_address;
        let rows: Vec<(String, &'static str)> = vec![
            ("0x7FF61234ABCD".to_string(), "hex address"),
            (format!("<{module}>"), "module base"),
            (format!("<{module}> + 0x1A0"), "module + offset"),
            (format!("[<{module}> + 0x58]"), "follow pointer"),
            ("ntdll!SymbolName".to_string(), "PDB symbol"),
        ];

        let number = color::syntax_number(cx);
        let muted = color::text_muted(cx);

        let mut card = design::elevated_surface(cx)
            .p(px(tokens::space::LG))
            .flex()
            .flex_col()
            .gap(px(tokens::space::XS))
            .max_w(px(tooltip::MAX_W))
            .text_size(px(tokens::font::UI_SM));

        // Item 22: prepend the bold "Base Address" title + a separator (the C++
        // `RcxTooltip` always paints a bold title above a separator above the body).
        card = card
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(color::text(cx))
                    .child("Base Address"),
            )
            .child(
                div()
                    .pb(px(tokens::space::XS))
                    .mb(px(tokens::space::XS))
                    .border_b_1()
                    .border_color(color::border(cx)),
            );

        for (example, explain) in rows {
            card = card.child(
                gpui_component::h_flex()
                    .w_full()
                    .gap(px(tokens::space::MD))
                    .justify_between()
                    .child(
                        div()
                            .font_family(tokens::font::mono_family())
                            .text_color(number)
                            .child(SharedString::from(example)),
                    )
                    .child(
                        div()
                            .text_color(muted)
                            .child(SharedString::from(format!("\u{2014} {explain}"))),
                    ),
            );
        }

        // The operator + hex hints (PIC5 footer of the address tooltip).
        card = card
            .child(
                div()
                    .pt(px(tokens::space::SM))
                    .mt(px(tokens::space::XS))
                    .border_t_1()
                    .border_color(color::border(cx))
                    .text_color(muted)
                    .child("Operators: + - * << >> & | ^"),
            )
            .child(div().text_color(muted).child("All numbers are hexadecimal"));

        card
    }
}

/// Item 22: a simple titled hover card (bold title + separator + muted body), the
/// Rust analogue of the C++ `RcxTooltip::populate(title, body)` used for the
/// command-row Data Source / Class Name / Switch View affordances. Multi-line
/// bodies (`\n`) render one muted line each.
pub struct TitledTooltip {
    title: SharedString,
    body: SharedString,
}

impl Render for TitledTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use design::{color, tokens};
        let mut card = design::elevated_surface(cx)
            .p(px(tokens::space::LG))
            .flex()
            .flex_col()
            .gap(px(tokens::space::XS))
            .max_w(px(tooltip::MAX_W))
            .text_size(px(tokens::font::UI_SM))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(color::text(cx))
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .pb(px(tokens::space::XS))
                    .mb(px(tokens::space::XS))
                    .border_b_1()
                    .border_color(color::border(cx)),
            );
        let muted = color::text_muted(cx);
        for line in self.body.split('\n') {
            card = card.child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(line.to_string())),
            );
        }
        card
    }
}

#[cfg(test)]
mod tests {
    // Import only the engine items under test — NOT `super::*`, which would pull
    // the module's `gpui::*` glob into the `#[test]` hygiene expansion and
    // overflow the type-recursion budget on this nightly+gpui combination. These
    // tests exercise pure glue (line slicing + selection-id matching) over a real
    // controller and need no gpui types.
    use crate::controller::{Modifiers as CtrlMods, RcxController, RcxDocument};
    use crate::core::linemeta::K_COMMAND_ROW_ID;
    use crate::core::LineKind;

    // The view-side logic is unit-tested in the sibling modules (geometry,
    // hit_test, selection, tab_cycle, inline_edit). Here we cover the small glue
    // helpers that do not require a gpui Window: line slicing + selection-id
    // matching over a real ComposeResult from the controller.

    fn editor_with_struct() -> RcxController {
        use crate::core::{Node, NodeKind};
        let mut doc = RcxDocument::new();
        // Build a tiny tree: a root struct with two fields (one int, one hex).
        let s_idx = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Player".into(),
            struct_type_name: "Player".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let s_id = doc.tree.nodes[s_idx].id;
        doc.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "health".into(),
            parent_id: s_id,
            offset: 0,
            ..Node::default()
        });
        doc.tree.add_node(Node {
            kind: NodeKind::Hex64,
            name: String::new(),
            parent_id: s_id,
            offset: 4,
            ..Node::default()
        });
        let mut c = RcxController::new(doc);
        c.set_view_root_id(s_id);
        c.refresh();
        c
    }

    #[test]
    fn line_slicing_matches_line_starts() {
        // `line_starts` are UTF-16 unit offsets; slicing the UTF-8 text requires
        // converting each to a byte offset (geometry::utf16_to_byte). The command
        // row contains multi-byte glyphs (▸/▾), so a naive byte slice would split
        // a line mid-glyph — this asserts the conversion yields clean single lines.
        use super::geometry::utf16_to_byte;
        let c = editor_with_struct();
        let result = c.last_result();
        assert!(!result.meta.is_empty());
        // The command row really does contain a multi-byte glyph (else this test
        // would not exercise the conversion).
        assert!(result.text.chars().any(|ch| ch.len_utf8() > 1));
        for i in 0..result.meta.len() {
            let begin = utf16_to_byte(&result.text, result.line_starts[i]);
            let end = if i + 1 < result.line_starts.len() {
                utf16_to_byte(&result.text, result.line_starts[i + 1])
            } else {
                result.text.len()
            };
            // Byte offsets must be valid char boundaries and ordered.
            assert!(result.text.is_char_boundary(begin));
            assert!(result.text.is_char_boundary(end));
            assert!(begin <= end);
            let slice = result.text[begin..end].trim_end_matches('\n');
            assert!(!slice.contains('\n'), "row text must be a single line");
        }
    }

    #[test]
    fn command_row_is_first_line() {
        let c = editor_with_struct();
        let result = c.last_result();
        assert_eq!(result.meta[0].line_kind, LineKind::CommandRow);
        assert_eq!(result.meta[0].node_id, K_COMMAND_ROW_ID);
    }

    #[test]
    fn command_row_override_uses_real_struct_name() {
        // Issue 3: the composed command-row stub says "struct Untitled"; the editor
        // substitutes `controller.build_command_row()`, which carries the real
        // struct type name. Verify (a) the live string names the real struct and
        // NOT "Untitled", and (b) the command-row span helpers resolve the class
        // name against that live string (so hit-testing stays consistent with what
        // the editor paints).
        use crate::compose::{command_row_root_name_span, command_row_root_type_span};
        let c = editor_with_struct();
        let row = c.build_command_row();
        assert!(row.contains("Player"), "row={row:?}");
        assert!(!row.contains("Untitled"), "row={row:?}");
        // The composed line-0 stub, by contrast, is the placeholder.
        assert!(c
            .last_result()
            .text
            .lines()
            .next()
            .unwrap()
            .contains("Untitled"));
        // Spans resolve on the live string.
        let name = command_row_root_name_span(&row);
        assert!(name.valid, "root-name span must resolve on the live row");
        let chars: Vec<char> = row.chars().collect();
        let got: String = chars[name.start as usize..name.end as usize]
            .iter()
            .collect();
        assert_eq!(got, "Player");
        // The keyword span resolves too ("struct"/"class").
        assert!(command_row_root_type_span(&row).valid);
    }

    #[test]
    fn relative_offsets_default_on_and_margin_increments() {
        // Issue 2: the margin must increment per row (not repeat the base) and
        // default to relative "+<HEX>" offsets. We test the pure formatter against
        // the controller's real per-row addresses.
        use super::geometry::fmt_margin_text;
        let c = editor_with_struct();
        let result = c.last_result();
        let base = result.layout.base_address;
        let digits = result.layout.offset_hex_digits;
        // Collect the rendered relative margin for each data row that carries an
        // offset, and confirm they are not all identical (the regression).
        let mut margins = Vec::new();
        for lm in &result.meta {
            if lm.line_kind == LineKind::Field && !lm.offset_text.trim().is_empty() {
                margins.push(fmt_margin_text(
                    lm.offset_addr,
                    base,
                    lm.ptr_base,
                    digits,
                    lm.is_continuation,
                    true,
                ));
            }
        }
        assert!(
            margins.len() >= 2,
            "need ≥2 field rows; got {}",
            margins.len()
        );
        let all_same = margins.iter().all(|m| m == &margins[0]);
        assert!(!all_same, "margins must differ per row: {margins:?}");
        // The first field is at relative +0.
        assert!(
            margins[0].trim_start().ends_with("+0"),
            "first={:?}",
            margins[0]
        );
    }

    #[test]
    fn selection_round_trips_through_controller() {
        let mut c = editor_with_struct();
        let result = c.last_result().clone();
        // Find a real data line (a field).
        let data_line = result
            .meta
            .iter()
            .position(|m| m.line_kind == LineKind::Field && m.node_id != 0)
            .expect("a field row exists");
        let node_id = result.meta[data_line].node_id;
        c.handle_node_click(data_line as i64, node_id, CtrlMods::NONE);
        // The node is now selected; strip_sel_pub recovers the bare id.
        assert!(c
            .selected_ids()
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == node_id));
    }

    #[test]
    fn parse_base_address_hex_decimal_and_formula() {
        // BUG 1: the base-address commit parses the typed string into
        // (numeric_base, formula). Pure number → numeric base, empty formula; an
        // expression / module reference → formula kept, numeric base falls back.
        use super::parse_base_address;
        let fallback = 0x1000u64;
        // 0x-prefixed hex.
        assert_eq!(
            parse_base_address("0x7FF60BF02B80", fallback),
            (0x7FF6_0BF0_2B80, String::new())
        );
        // Trailing-h hex.
        assert_eq!(
            parse_base_address("400000h", fallback),
            (0x40_0000, String::new())
        );
        // Bare hex digits (addresses are hex by convention).
        assert_eq!(
            parse_base_address("ABCD", fallback),
            (0xABCD, String::new())
        );
        // Empty → reset to 0, no formula.
        assert_eq!(parse_base_address("", fallback), (0, String::new()));
        assert_eq!(parse_base_address("   ", fallback), (0, String::new()));
        // A formula (operator / module) → kept verbatim, base = fallback.
        assert_eq!(
            parse_base_address("app.exe + 0x1A0", fallback),
            (fallback, "app.exe + 0x1A0".to_string())
        );
        assert_eq!(
            parse_base_address("[app.exe + 0x58]", fallback),
            (fallback, "[app.exe + 0x58]".to_string())
        );
        assert_eq!(
            parse_base_address("ntdll!Sym", fallback),
            (fallback, "ntdll!Sym".to_string())
        );
    }

    #[test]
    fn type_cycle_stays_within_the_same_byte_size() {
        // Item 16: the quick type-cycler (`← cur ↔ alt →`, Left/Right) steps to a
        // same-byte-size variant, never a different size / container / string. From
        // any fixed-size primitive, both alt (next) and prev land on a kind of the
        // SAME byte size, and the ring closes (cycling len times returns home).
        use super::{alt_kind_for, prev_kind_for, same_size_variants};
        use crate::core::{size_for_kind, NodeKind};
        for &k in &[
            NodeKind::Int8,
            NodeKind::Int32,
            NodeKind::UInt32,
            NodeKind::Float,
            NodeKind::Int64,
            NodeKind::Pointer64,
        ] {
            let size = size_for_kind(k);
            let next = alt_kind_for(k);
            let prev = prev_kind_for(k);
            assert_eq!(
                size_for_kind(next),
                size,
                "alt of {k:?} must keep the byte size"
            );
            assert_eq!(
                size_for_kind(prev),
                size,
                "prev of {k:?} must keep the byte size"
            );
            assert!(
                !matches!(next, NodeKind::Struct | NodeKind::Array),
                "cycle never lands on a container"
            );
            // Cycling forward `ring.len()` times returns to the start.
            let ring = same_size_variants(k);
            let mut cur = k;
            for _ in 0..ring.len() {
                cur = alt_kind_for(cur);
            }
            assert_eq!(cur, k, "forward cycle of {k:?} closes the ring");
        }
    }

    #[test]
    fn default_view_root_picks_first_top_level_struct() {
        // Issue 3 (default root): a multi-struct .rcx with several top-level
        // structs must open focused on the FIRST declared root struct (the
        // project's main struct), not stack every root / name a stray one. This
        // mirrors `RcxEditor::default_view_root_id` — the first `children_of(0)`
        // node whose kind is Struct.
        use crate::core::{Node, NodeKind};
        let mut doc = RcxDocument::new();
        // First declared top-level struct — the intended default root.
        let main_idx = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "_EPROCESS".into(),
            struct_type_name: "_EPROCESS".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let main_id = doc.tree.nodes[main_idx].id;
        // A second top-level struct that must NOT become the default root.
        doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "_LIST_ENTRY".into(),
            struct_type_name: "_LIST_ENTRY".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let c = RcxController::new(doc);
        // Replicate the editor's default-root selection over the same accessors.
        let picked = c
            .tree()
            .children_of(0)
            .iter()
            .map(|&i| &c.tree().nodes[i])
            .find(|n| n.kind == NodeKind::Struct)
            .map(|n| n.id);
        assert_eq!(picked, Some(main_id), "must pick the first declared root");
    }

    // ── Node clipboard (item 4) ──

    #[test]
    fn clipboard_codec_preserves_subtrees_and_remaps_ids() {
        // Feature 3: the editor now serializes via core::clipboard (subtree
        // collection) and deserializes via the codec (whole-subtree id remap), so
        // a copied struct keeps its CHILDREN and the pasted copy gets fresh,
        // non-colliding ids. This is the copy→paste fidelity contract that the old
        // flat envelope broke (it dropped children + reset collapsed/ref).
        use crate::core::clipboard::{deserialize, serialize};
        use crate::core::{Node, NodeKind, NodeTree};
        use std::collections::HashSet;

        let mut tree = NodeTree::new();
        let s = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Outer".into(),
            ..Node::default()
        });
        let sid = tree.nodes[s].id;
        let _c1 = tree.add_node(Node {
            parent_id: sid,
            kind: NodeKind::Int32,
            name: "a".into(),
            offset: 0,
            ..Node::default()
        });
        let _c2 = tree.add_node(Node {
            parent_id: sid,
            kind: NodeKind::Hex64,
            name: "b".into(),
            offset: 8,
            ..Node::default()
        });

        let roots = [sid];
        let clear: HashSet<u64> = roots.iter().copied().collect();
        let (blob, _plain) = serialize(&tree, &roots, &clear);
        assert!(!blob.is_empty());

        // Paste into a DIFFERENT tree: ids must be re-minted; the struct + both
        // children must all come across (subtree preserved).
        let mut dest = NodeTree::new();
        let res = deserialize(&mut dest, &blob);
        assert_eq!(res.nodes.len(), 3, "struct + 2 children");
        assert_eq!(res.root_ids.len(), 1);
        let new_root = res.root_ids[0];
        // The two children re-parent onto the remapped root (not id 0 / the old id).
        let kids: Vec<&Node> = res
            .nodes
            .iter()
            .filter(|n| n.parent_id == new_root)
            .collect();
        assert_eq!(kids.len(), 2, "both children re-wired under the new root");
        // The cleared root's parent link is 0 (re-anchors under the paste target).
        let root_node = res.nodes.iter().find(|n| n.id == new_root).unwrap();
        assert_eq!(root_node.parent_id, 0);
    }

    #[test]
    fn clipboard_pasted_span_recurses_over_children() {
        // The pasted-span helper (controller.cpp:582 `pastedSpan`) measures a
        // container root by the max child end, not its (zero) kind size — so the
        // offset-shift + placement math leaves room for the whole subtree.
        use crate::core::{Node, NodeKind};
        let root_id = 100u64;
        let nodes = vec![
            Node {
                id: root_id,
                kind: NodeKind::Struct,
                ..Node::default()
            },
            Node {
                id: 101,
                parent_id: root_id,
                kind: NodeKind::Hex64,
                offset: 0,
                ..Node::default()
            },
            Node {
                id: 102,
                parent_id: root_id,
                kind: NodeKind::Hex64,
                offset: 8,
                ..Node::default()
            },
        ];
        // Two Hex64 children at 0 and 8 → span 16.
        assert_eq!(super::RcxEditor::pasted_span(&nodes, root_id), 16);
        // A leaf child uses its kind size (Hex64 = 8).
        assert_eq!(super::RcxEditor::pasted_span(&nodes, 101), 8);
    }

    // ── Hover popup equality (item 13) ──

    #[test]
    fn hover_kind_eq_distinguishes_content_and_variant() {
        use super::{hover_kind_eq, HoverPopupKind};
        let mk = |vals: &[&str], set_buttons: bool| HoverPopupKind::ValueHistory {
            entries: vals.iter().map(|v| (v.to_string(), "now".into())).collect(),
            node_idx: 0,
            sub_line: 0,
            resolved_addr: 0,
            set_buttons,
        };
        let a = mk(&["1", "2"], false);
        let a2 = mk(&["1", "2"], false);
        // Same VALUES but different age labels must still compare equal (the age
        // labels tick; only the value column drives popup identity, item 68).
        let a3 = HoverPopupKind::ValueHistory {
            entries: vec![("1".into(), "5s ago".into()), ("2".into(), "9s ago".into())],
            node_idx: 0,
            sub_line: 0,
            resolved_addr: 0,
            set_buttons: false,
        };
        let b = mk(&["1", "3"], false);
        let with_buttons = mk(&["1", "2"], true);
        let t = HoverPopupKind::TitleBody {
            title: "Disassembly".into(),
            body: "nop".into(),
        };
        assert!(hover_kind_eq(&a, &a2), "same content compares equal");
        assert!(hover_kind_eq(&a, &a3), "differing age labels still equal");
        assert!(!hover_kind_eq(&a, &b), "different values differ");
        assert!(!hover_kind_eq(&a, &with_buttons), "Set-button mode differs");
        assert!(!hover_kind_eq(&a, &t), "different variants differ");
    }

    #[test]
    fn relative_age_buckets_match_cpp() {
        use super::RcxEditor;
        let now = 10_000_000i64;
        assert_eq!(RcxEditor::relative_age(now, 0), "", "untracked → empty");
        assert_eq!(RcxEditor::relative_age(now, now - 500), "now");
        assert_eq!(RcxEditor::relative_age(now, now - 12_000), "12s ago");
        assert_eq!(RcxEditor::relative_age(now, now - 180_000), "3m ago");
        assert_eq!(RcxEditor::relative_age(now, now - 7_200_000), "2h ago");
    }

    // ── Item 4: Vec/Mat value-component narrowing (the column→component map) ──

    /// The pure narrowing math `begin_inline_edit` applies for a Vec/Mat Value
    /// click: split the comma-joined value, count commas before the clicked column,
    /// and that index is both the seeded component and the write `sub_line` (which
    /// `set_node_value` routes to `addr + sub_line*4` as a Float).
    fn vec_component_for_click(raw_span: &str, span_start: i32, click_col: i32) -> (usize, String) {
        let comps: Vec<&str> = raw_span.split(',').collect();
        if comps.len() <= 1 {
            return (0, raw_span.trim().to_string());
        }
        let rel = (click_col - span_start).max(0) as usize;
        let span_chars: Vec<char> = raw_span.chars().collect();
        let upto = rel.min(span_chars.len());
        let comp = span_chars[..upto]
            .iter()
            .filter(|&&c| c == ',')
            .count()
            .min(comps.len() - 1);
        (comp, comps[comp].trim().to_string())
    }

    #[test]
    fn vec3_value_click_narrows_to_clicked_component() {
        // "1.0, 2.5, 3.0" starting at column 10: clicking inside "2.5" (the second
        // component) must yield sub_line 1 and seed "2.5", not the whole string.
        let raw = "1.0, 2.5, 3.0";
        // Component 0 ("1.0") spans rel cols [0,3); commas at rel 3 and 8.
        assert_eq!(vec_component_for_click(raw, 10, 10), (0, "1.0".to_string()));
        // Click at rel col 6 (inside "2.5", after the first comma) → component 1.
        assert_eq!(vec_component_for_click(raw, 10, 16), (1, "2.5".to_string()));
        // Click at rel col 11 (inside "3.0", after both commas) → component 2.
        assert_eq!(vec_component_for_click(raw, 10, 21), (2, "3.0".to_string()));
        // A click past the end clamps to the last component.
        assert_eq!(vec_component_for_click(raw, 10, 99), (2, "3.0".to_string()));
    }

    #[test]
    fn scalar_value_click_is_not_narrowed() {
        // A single-component value (no comma) is seeded whole at sub_line 0.
        assert_eq!(vec_component_for_click("42", 5, 6), (0, "42".to_string()));
    }

    // ── Item 5: 'Edit ASCII' writes per-byte ASCII (not parsed as hex) ──

    #[test]
    fn ascii_value_parse_is_literal_not_hex() {
        // The is_ascii path in set_node_value parses the text via
        // `format::parse_ascii_value` (literal bytes), NOT `parse_value` (hex
        // number). For "ABCD" into a 4-byte field that means the bytes A,B,C,D —
        // whereas a hex parse of "ABCD" would be the 2-byte value 0xABCD. This pins
        // the branch the editor's 'Edit ASCII' commit selects (item 5).
        let ascii = crate::format::parse_ascii_value("ABCD", 4).expect("ascii parse");
        assert_eq!(ascii, vec![b'A', b'B', b'C', b'D'], "literal ASCII bytes");
        // Sanity: the value would be different if parsed as hex (proves the branch
        // matters). A hex parse of "ABCD" is NOT the ASCII byte string.
        assert_ne!(ascii, vec![0xCD, 0xAB, 0x00, 0x00]);
    }

    #[test]
    fn ascii_value_write_through_buffer_provider() {
        use crate::core::{Node, NodeKind};
        use crate::provider::BufferProvider;
        use std::sync::Arc;
        // End-to-end: a hex node whose Value is edited as ASCII writes the literal
        // ASCII bytes through a writable BufferProvider (base 0). set_node_value
        // with is_ascii=true is the editor's ASCII-overwrite commit path (item 5).
        let mut doc = RcxDocument::new();
        let s = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "S".into(),
            struct_type_name: "S".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let sid = doc.tree.nodes[s].id;
        let h = doc.tree.add_node(Node {
            kind: NodeKind::Hex32,
            name: String::new(),
            parent_id: sid,
            offset: 0,
            ..Node::default()
        });
        let mut c = RcxController::new(doc);
        c.set_view_root_id(sid);
        // 4-byte zeroed writable buffer at base 0.
        let prov = Arc::new(BufferProvider::new(vec![0u8; 4], "ram"));
        c.attach_provider(prov, false);
        c.tree_mut().base_address = 0;
        c.refresh();
        // ASCII-write "ABCD" into the 4-byte hex field at offset 0 (addr 0).
        c.set_node_value(h, 0, "ABCD", /* is_ascii */ true, 0);
        let bytes = c.document().provider.read_bytes(0, 4);
        assert_eq!(
            bytes,
            vec![b'A', b'B', b'C', b'D'],
            "ASCII bytes written verbatim, not parsed as hex"
        );
    }

    // ── Item 13: Collapse/Expand All is a SINGLE undoable macro ──

    #[test]
    fn collapse_all_is_one_undo() {
        use crate::core::{Node, NodeKind};
        // Three nested expanded structs; collapsing all under one macro must undo in
        // a SINGLE step (the C++ Collapse All begin/endMacro), restoring every
        // container's prior collapsed state at once.
        let mut doc = RcxDocument::new();
        let root = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Root".into(),
            struct_type_name: "Root".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let root_id = doc.tree.nodes[root].id;
        // Two child sub-structs (containers), each EXPLICITLY expanded, each with a
        // child field so they are real expandable containers.
        for (i, nm) in ["A", "B"].iter().enumerate() {
            let sub = doc.tree.add_node(Node {
                kind: NodeKind::Struct,
                name: (*nm).into(),
                struct_type_name: (*nm).into(),
                parent_id: root_id,
                offset: (i as i32) * 8,
                collapsed: false,
                ..Node::default()
            });
            let sub_id = doc.tree.nodes[sub].id;
            doc.tree.add_node(Node {
                kind: NodeKind::Int32,
                name: format!("f{i}"),
                parent_id: sub_id,
                offset: 0,
                ..Node::default()
            });
        }
        let mut c = RcxController::new(doc);
        c.set_view_root_id(root_id);
        c.refresh();
        // Collapse every EXPANDED container in ONE macro (mirrors set_all_collapsed).
        let targets: Vec<usize> = c
            .tree()
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| crate::core::is_container_kind(n.kind) && !n.collapsed)
            .map(|(i, _)| i)
            .collect();
        assert!(
            targets.len() >= 2,
            "need ≥2 expanded containers, got {}",
            targets.len()
        );
        c.begin_macro("Collapse all");
        for idx in &targets {
            c.toggle_collapse(*idx);
        }
        c.end_macro();
        // Every targeted container is now collapsed.
        assert!(targets.iter().all(|&i| c.tree().nodes[i].collapsed));
        // ONE undo restores all of them (not N undos).
        c.undo();
        assert!(
            targets.iter().all(|&i| !c.tree().nodes[i].collapsed),
            "a single undo must re-expand all containers"
        );
    }

    // ── Item 12: Ctrl-additive nav toggles a node into the selection ──

    #[test]
    fn ctrl_click_toggles_node_into_selection() {
        // The additive-nav variant (Ctrl+Up/Down) calls handle_node_click with
        // ctrl=true, which TOGGLES the node into the multi-selection rather than
        // replacing it. Two ctrl-clicks on distinct rows leaves both selected.
        let mut c = editor_with_struct();
        let result = c.last_result().clone();
        let fields: Vec<(usize, u64)> = result
            .meta
            .iter()
            .enumerate()
            .filter(|(_, m)| m.line_kind == LineKind::Field && m.node_id != 0)
            .map(|(i, m)| (i, m.node_id))
            .collect();
        assert!(fields.len() >= 2, "need ≥2 fields");
        let ctrl = CtrlMods {
            ctrl: true,
            shift: false,
        };
        c.handle_node_click(fields[0].0 as i64, fields[0].1, ctrl);
        c.handle_node_click(fields[1].0 as i64, fields[1].1, ctrl);
        let sel = c.selected_ids();
        assert!(sel
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == fields[0].1));
        assert!(sel
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == fields[1].1));
        // A third ctrl-click on the first row REMOVES it (toggle off).
        c.handle_node_click(fields[0].0 as i64, fields[0].1, ctrl);
        assert!(!c
            .selected_ids()
            .iter()
            .any(|&id| crate::controller::strip_sel_pub(id) == fields[0].1));
    }

    // ── Item 15: F12 go-to-definition target resolution ──

    /// The pure target-resolution rule `action_go_to_definition` applies (the C++
    /// `goToDefinitionRequested` ordering): ref_id → array-of-structs ref_id → a
    /// plain embedded Struct field's OWN id → none.
    fn goto_def_target(n: &crate::core::Node) -> u64 {
        use crate::core::NodeKind;
        if n.ref_id != 0 {
            n.ref_id
        } else if n.kind == NodeKind::Array && n.element_kind == NodeKind::Struct && n.ref_id != 0 {
            n.ref_id
        } else if n.kind == NodeKind::Struct && n.parent_id != 0 {
            n.id
        } else {
            0
        }
    }

    #[test]
    fn goto_definition_plain_struct_field_reroots_to_own_id() {
        use crate::core::{Node, NodeKind};
        // A typed pointer / ref carries ref_id → that wins.
        let ptr = Node {
            kind: NodeKind::Pointer64,
            ref_id: 42,
            parent_id: 1,
            id: 7,
            ..Node::default()
        };
        assert_eq!(goto_def_target(&ptr), 42);
        // Item 15: a PLAIN embedded struct field with NO ref re-roots to its OWN id
        // (was previously a no-op when ref_id == 0).
        let embedded = Node {
            kind: NodeKind::Struct,
            ref_id: 0,
            parent_id: 1,
            id: 99,
            ..Node::default()
        };
        assert_eq!(goto_def_target(&embedded), 99);
        // A top-level struct (parent 0) with no ref has no definition to jump to.
        let top = Node {
            kind: NodeKind::Struct,
            ref_id: 0,
            parent_id: 0,
            id: 5,
            ..Node::default()
        };
        assert_eq!(goto_def_target(&top), 0);
        // A plain scalar with no ref → none.
        let scalar = Node {
            kind: NodeKind::Int32,
            ref_id: 0,
            parent_id: 1,
            id: 3,
            ..Node::default()
        };
        assert_eq!(goto_def_target(&scalar), 0);
    }

    #[test]
    fn ctrl_click_new_tab_target_matches_goto_definition_resolution() {
        // Item 10: the Ctrl+Click "open in new tab" target resolves IDENTICALLY to
        // go-to-definition: a typed ref wins, else a plain embedded struct opens its
        // OWN subtree (its id) — previously the Ctrl+Click branch only fired for
        // `ref_id != 0`, so a plain embedded struct header fell through to selection.
        use crate::core::{Node, NodeKind};
        let embedded = Node {
            kind: NodeKind::Struct,
            ref_id: 0,
            parent_id: 1,
            id: 77,
            ..Node::default()
        };
        // The new-tab branch must NOT yield 0 for a plain embedded struct.
        assert_eq!(goto_def_target(&embedded), 77);
        assert_ne!(goto_def_target(&embedded), 0);
    }

    #[test]
    fn big_endian_eligibility_spans_uint8_but_not_int8() {
        // Item 6: the C++ predicate `kind in [Int16 .. UInt128]` SPANS UInt8 in the
        // enum ordering (Int8 < Int16 .. < UInt8 < .. < UInt128), so UInt8 IS
        // eligible for the Big-endian item; Int8 (below Int16) is NOT.
        use crate::core::NodeKind;
        assert!(super::is_scalar_numeric_kind(NodeKind::UInt8));
        assert!(!super::is_scalar_numeric_kind(NodeKind::Int8));
        // Hex8 is excluded (only Hex16..Hex128 are scalar), Hex16 included.
        assert!(!super::is_scalar_numeric_kind(NodeKind::Hex8));
        assert!(super::is_scalar_numeric_kind(NodeKind::Hex16));
        // Containers / pointers are never scalar-numeric.
        assert!(!super::is_scalar_numeric_kind(NodeKind::Struct));
        assert!(!super::is_scalar_numeric_kind(NodeKind::Pointer64));
        // Spot-check the rest of the spanned range.
        assert!(super::is_scalar_numeric_kind(NodeKind::Int16));
        assert!(super::is_scalar_numeric_kind(NodeKind::UInt128));
        assert!(super::is_scalar_numeric_kind(NodeKind::Double));
    }

    #[test]
    fn copy_offset_format_is_plus_0x_uppercase_zero_padded_4() {
        // Item 4: Copy Offset copies the node's LOCAL `.offset` field formatted
        // "+0x" + uppercase-hex right-justified to 4 digits (the C++
        // `"+0x" + QString::number(off,16).toUpper().rightJustified(4,'0')`).
        let fmt = |off: i32| format!("+0x{off:04X}");
        assert_eq!(fmt(8), "+0x0008");
        assert_eq!(fmt(0), "+0x0000");
        assert_eq!(fmt(0x1a), "+0x001A");
        // A wide offset is NOT truncated (rightJustified only pads).
        assert_eq!(fmt(0x12345), "+0x12345");
    }

    #[test]
    fn recent_types_dedup_to_front_and_cap_at_8() {
        // Item 3/11: pushRecentType moves a re-picked name to the front (dedup) and
        // caps the list at 8, most-recent-first.
        let mut list: Vec<String> = Vec::new();
        for i in 0..10 {
            super::push_recent_type_into(&mut list, &format!("T{i}"));
        }
        // Capped at 8, newest first.
        assert_eq!(list.len(), 8);
        assert_eq!(list[0], "T9");
        assert_eq!(list[7], "T2");
        // Re-picking an existing name moves it to the front (no duplicate).
        super::push_recent_type_into(&mut list, "T4");
        assert_eq!(list[0], "T4");
        assert_eq!(list.iter().filter(|n| *n == "T4").count(), 1);
        assert_eq!(list.len(), 8);
        // Empty names are ignored.
        super::push_recent_type_into(&mut list, "");
        assert_eq!(list[0], "T4");
    }

    #[test]
    fn composite_type_entry_reports_computed_struct_extent() {
        // Parity: the type-selector catalogue must set a composite's `size_bytes`
        // to the struct's actual byte extent (C++ `e.sizeBytes = structSpan(n.id)`,
        // controller.cpp:4555) — NOT the flat `size_for_kind(Struct)`, which is 0
        // and would render every struct as "dyn" in the popup size bar/preview
        // (typeselectorpopup.cpp:1515-1557).
        //
        // The `editor_with_struct` Player has Int32@0 (4B) + Hex64@4 (8B), so its
        // extent is 12. `full_type_entries` itself needs a gpui Window, so we
        // exercise the same two pieces it composes: the extent source
        // (`tree.struct_span`) and the `TypeEntry::composite` carry-through.
        use crate::ui::typeselectorpopup::{EntryKind, TypeEntry};
        let c = editor_with_struct();
        let tree = c.tree();
        let player_idx = tree
            .nodes
            .iter()
            .position(|n| n.struct_type_name == "Player")
            .expect("Player struct present");
        let player = &tree.nodes[player_idx];

        // The flat size used by the old code is the "dyn" sentinel.
        assert_eq!(crate::core::size_for_kind(player.kind), 0);
        // The real extent (what compose uses for struct extents) is the sum of
        // the children's footprints: 4 (Int32@0) + 8 (Hex64@4) = 12.
        let extent = tree.struct_span(player.id).max(0);
        assert_eq!(extent, 12, "Player extent = Int32@0 + Hex64@4 = 12B");

        // The composite entry must carry that extent, not 0.
        let entry = TypeEntry::composite(player.id, &player.struct_type_name, "struct", extent);
        assert_eq!(entry.entry_kind, EntryKind::Composite);
        assert_eq!(entry.size_bytes, 12);
        assert_ne!(
            entry.size_bytes, 0,
            "composite must not report dyn for a sized struct"
        );
    }
}
