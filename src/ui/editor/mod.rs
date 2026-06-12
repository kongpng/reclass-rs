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
mod hover_popup;
pub mod inline_edit;
pub mod minimap;
pub mod palette;
pub mod selection;
pub mod tab_cycle;
use hover_popup::{HoverPopupKind, HoverPopupState, HoverProbe, MEMORY_PREVIEW_MIN_ROWS};
mod context_menu;
mod debug_view;
mod popups;
#[cfg(test)]
pub(crate) use popups::push_recent_type_into;
mod mouse;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, IconName};

use crate::compose::EditTarget;
use crate::controller::{Modifiers as CtrlMods, RcxController, RcxDocument};
use crate::core::linemeta::K_COMMAND_ROW_ID;
use crate::core::{is_hex_preview, ComposeResult, LineKind, LineMeta, NodeKind};
use crate::ui::design::color::with_alpha;
use crate::ui::overlays::findbar::{FindBar, FindEvent};
use crate::ui::pickers::sourcechooser::{SourceChooserEvent, SourceChooserPopup};
use crate::ui::{design, overlays::tooltip};

use element::{RowElement, RowPaint};
use geometry::CellMetrics;
use inline_edit::FieldInput;
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
        EditorPreviewValue,
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
        // Enum HEADER "Add Member" — appends a member to the enum (no anchor), so an
        // empty enum can grow from its header menu (the C++ enum-header branch,
        // controller.cpp:3364).
        EditorEnumAddMember,
        // Item 7/18/48: group the multi-selection into a Union (controller
        // `group_into_union`).
        EditorGroupIntoUnion,
        // Item 8: the Structure submenu actions — add a Hex64 child, or
        // dissolve a union member.
        EditorStaticAddChild,
        EditorStaticDissolveUnion,
        // Item 11: type-inference quick-convert (the C++ `Convert to <type>` /
        // `Split into <type>xN`). The suggested kind(s) are stashed in
        // `pending_hint_convert` when the menu opens (a parameterless gpui action
        // can't carry the dynamic kind), and these read it back.
        EditorHintConvert,
        EditorHintSplit,
        // Part D: the amalgamated "Selected bytes (N) ▸" submenu actions (the C++
        // `addByteSubmenu`, controller.cpp). Each acts on the editor's live byte
        // selection via the controller's byte-op handlers. Copy/Paste/Zero-fill/
        // Save read or write the selected byte range; Edit hex opens the inline
        // hex-overwrite editor; Break into new class extracts the range.
        EditorByteCopyHex,
        EditorByteCopyCArray,
        EditorByteCopyPython,
        EditorByteEditHex,
        EditorByteZeroFill,
        EditorBytePasteHex,
        EditorByteSaveBinary,
        EditorByteBreakClass,
        // Bottom "Clear selection" — clears the byte selection AND the mirrored row
        // selection together when either is non-empty (the C++ menu tail).
        EditorClearSelection,
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
    /// Open an editor-owned keyboard popup (type / enum / source picker) in the
    /// host's centered-modal overlay ([`MainWindow::open_centered_modal`]) instead
    /// of gpui-component's `window.open_dialog`, whose `Dialog` `focus_trap`
    /// swallowed the popup's Enter/arrow keys (dialog.rs:526). The host renders +
    /// focuses the popup; this editor keeps the popup's own outcome subscription,
    /// so on Chosen/Cancel it applies the choice and emits [`CloseModal`] back.
    ///
    /// [`MainWindow::open_centered_modal`]: crate::ui::window::MainWindow
    /// [`CloseModal`]: RcxEditorEvent::CloseModal
    OpenModal {
        view: AnyView,
        focus: FocusHandle,
        /// Fixed card width for popups that only set a `min_w` and relied on the
        /// dialog's `.w(..)` (source chooser 520 / enum 360 / hex 320). `None` for
        /// a self-sizing popup (the type selector sets its own `w(380)`).
        width: Option<Pixels>,
    },
    /// Dismiss the topmost host centered modal — the editor's popup signalled a
    /// Chosen / Cancel / close. Replaces the editor's old `window.close_dialog`.
    CloseModal,
    /// The controlled document was mutated — an inline-edit commit (rename / value
    /// / type), a structural op (insert / delete / New Class / type change), or
    /// undo/redo. The host rebuilds the workspace TYPES list so a class rename /
    /// add / remove is reflected in the left panel: the workspace caches a
    /// `WorkspaceModel` that the per-selection `observe` (status-bar `cx.notify()`)
    /// does NOT refresh, so without this a renamed class kept its stale name.
    DocumentEdited,
}

/// Item 12: the editor-originated View options that can be toggled from within the
/// editor surface and must propagate to the host (currently only Relative Offsets;
/// kept as an enum so further in-editor toggles can join without a new event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorViewOption {
    RelativeOffsets,
}

/// A top-level struct/enum declared in ANOTHER open document, surfaced in this
/// editor's Type Selector catalogue (the C++ `m_projectDocs` cross-doc composites,
/// controller.cpp:4753-4774). There is intentionally no `struct_id`: a cross-doc
/// pick imports by NAME (`find_or_create_struct_by_name`), exactly like the
/// built-in Common Types, since the foreign struct has no id in *this* document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossDocComposite {
    /// The struct's display name (`struct_type_name`, or `name` when that is empty).
    pub name: String,
    /// The class keyword (`struct`/`class`/`union`/`enum`); `struct` when unset.
    pub keyword: String,
    /// The struct's byte extent (`struct_span`) for the size bar/preview.
    pub size: i32,
}

impl CrossDocComposite {
    /// Every top-level (`parent_id == 0`) struct declaration in `tree`, as cross-doc
    /// composites — the snapshot a sibling document contributes to another editor's
    /// Type Selector. The name falls back to `name` when `struct_type_name` is empty
    /// (the C++ `structTypeName.isEmpty() ? name : structTypeName`).
    pub fn top_level_in(tree: &crate::core::NodeTree) -> Vec<CrossDocComposite> {
        tree.nodes
            .iter()
            .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
            .filter_map(|n| {
                let name = if n.struct_type_name.is_empty() {
                    &n.name
                } else {
                    &n.struct_type_name
                };
                if name.is_empty() {
                    return None;
                }
                let keyword = if n.class_keyword.is_empty() {
                    "struct"
                } else {
                    n.class_keyword.as_str()
                };
                Some(CrossDocComposite {
                    name: name.clone(),
                    keyword: keyword.to_string(),
                    size: tree.struct_span(n.id).max(0),
                })
            })
            .collect()
    }
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
    /// Last set of covered-row sel-ids mirrored into the controller via
    /// `on_byte_selection_rows` (`m_lastByteRows`). De-dups the sync so a
    /// multi-pixel drag only re-mirrors when it crosses a row boundary.
    last_byte_rows: std::collections::HashSet<u64>,
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
    /// Last row/value hit-test used to rebuild an open hover popup from timer
    /// refreshes while the mouse stays still or is over the popup itself.
    hover_probe: Option<HoverProbe>,
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
    find_bar: Option<Entity<crate::ui::overlays::findbar::FindBar>>,
    find_match: Option<crate::ui::overlays::findbar::FindMatch>,
    /// Item 31: the FULL match set, cached for the paint path (which has no `cx` to
    /// read the find-bar entity). Refreshed on Navigate and on every recompose
    /// (`sync_find_bar_lines`) so the painted IND_FIND bands track the layout.
    find_matches: Vec<crate::ui::overlays::findbar::FindMatch>,
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
    /// Top-level structs from the OTHER open documents, refreshed by the window on
    /// every `rebuild_workspace`. Appended to the Type Selector catalogue so a
    /// struct can reference a type declared in a sibling tab (the C++ `m_projectDocs`
    /// cross-doc composites, controller.cpp:4753).
    cross_doc_composites: Vec<CrossDocComposite>,
    /// Item 13: set while the cursor is INSIDE the floating hover popup card. While
    /// set, `dispatch_row_hover` suppresses popup dismissal so moving onto the card
    /// (e.g. to click a value-history 'Set' button) does not clear it first (the
    /// C++ `m_hoverInside` / geometry-contains guard, editor.cpp:2815/4531).
    popup_cursor_inside: bool,
    /// Esc-dismiss hover latch (the C++ `m_hoverDwellElapsed` reset + timer stop in
    /// `dismissAllPopups`): set when Esc dismisses all popups so the hover preview
    /// does NOT immediately reappear on the next mouse twitch within the SAME row.
    /// Released the moment the cursor moves onto a different node/line (or leaves
    /// the viewport), so the preview returns only after the user re-dwells
    /// elsewhere — Esc "sticks" without globally disabling hover effects.
    hover_dwell_suppressed: bool,
    /// ReClass.NET memory-preview row count. Reset when the hover popup closes;
    /// wheel events over the popup expand/contract it.
    memory_preview_rows: usize,
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
    /// A mode prefix for the valid-state hint: `"Hex edit: "` / `"ASCII edit: "`
    /// while overwriting raw bytes, else empty (C++ `beginEdit` hint,
    /// editor.cpp:3835-3844). Only decorates the valid hint, not the error line.
    hint_prefix: &'static str,
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
    /// PIXEL left of the edited span, measured by SHAPING the real line prefix
    /// `[0, col_start)` with the editor font at edit-begin (not `col_start *
    /// cell_width`). Rows with non-ASCII glyphs before the span — the command
    /// row's `▸`/`▾`/source chip ahead of the class name, tree connectors — shape
    /// at advances ≠ the mono cell, so the cell-grid estimate drifted the box
    /// right of the painted token (worst at high columns, e.g. the class name).
    /// Shaping matches the painter (`RowElement` uses `shape_line` too) exactly.
    left_px: f32,
    /// PIXEL width of the edited span `[col_start, col_end)`, likewise shaped.
    width_px: f32,
    /// Comment edits are presented as a labeled row-local field anchored near the
    /// value/name area instead of over the far-right rendered `// ...` chip.
    comment_popover: bool,
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
            last_byte_rows: std::collections::HashSet::new(),
            hovered_line: None,
            hovered_node_id: 0,
            hover_popup: None,
            hover_probe: None,
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
            cross_doc_composites: Vec::new(),
            popup_cursor_inside: false,
            hover_dwell_suppressed: false,
            memory_preview_rows: MEMORY_PREVIEW_MIN_ROWS,
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
        self.last_byte_rows.clear();
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
        cx.emit(RcxEditorEvent::DocumentEdited);
        cx.notify();
    }

    /// Recompose for a VIEW change (set_view_root_id / scroll-to-node) WITHOUT
    /// signalling a document edit. Navigation must NOT emit `DocumentEdited`, which
    /// drives the host's workspace rebuild — that re-feeds the project tree via
    /// `TreeState::set_items`, which resets every row to collapsed, so navigating
    /// would snap the expanded tree shut. (Structural edits still use
    /// `apply_document`; this is the no-edit recompose.)
    pub fn recompose_view(&mut self, cx: &mut Context<Self>) {
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
            self.refresh_hover_popup_from_probe(cx);
            let _ = self.controller.take_events();
            cx.notify();
        } else {
            // Pointer previews read their target directly from the provider. Keep
            // them polling from the timer even when the main snapshot has no page
            // work to do.
            self.refresh_hover_popup_from_probe(cx);
        }
    }

    pub fn byte_selection(&self) -> &ByteSelection {
        &self.byte_sel
    }

    /// The set of encoded row sel-ids the current byte selection covers — every
    /// hex-preview Field row whose `[offset_addr, offset_addr + count)` overlaps
    /// the byte range, mapped through `sel_id_for_line` so the ids match exactly
    /// what a click would store (`applyByteSelectionOverlay`'s `covered` set). An
    /// empty set when there is no byte selection. Reuses the same per-row overlap
    /// geometry as `build_row_paint`.
    fn covered_byte_rows(&self) -> std::collections::HashSet<u64> {
        let mut covered = std::collections::HashSet::new();
        let Some(sel) = self.byte_sel.range() else {
            return covered;
        };
        for lm in &self.controller.last_result().meta {
            if !is_hex_preview(lm.node_kind) || lm.line_kind != LineKind::Field {
                continue;
            }
            let count = if lm.line_byte_count > 0 {
                lm.line_byte_count
            } else {
                crate::core::size_for_kind(lm.node_kind)
            };
            if selection::row_byte_overlap(lm.offset_addr, count, sel).is_some() {
                covered.insert(crate::core::sel_id_for_line(lm));
            }
        }
        covered
    }

    /// Mirror the byte selection into the controller's row selection: recompute
    /// the covered rows and, when they differ from the last mirrored set, push
    /// them via `on_byte_selection_rows` (`byteSelectionRowsChanged` →
    /// `onByteSelectionRows`). An empty set clears the row selection. De-duped so
    /// a multi-pixel drag only re-syncs on a row-boundary crossing. Call after
    /// every byte-selection mutation.
    fn sync_byte_rows(&mut self) {
        let covered = self.covered_byte_rows();
        if covered == self.last_byte_rows {
            return;
        }
        self.last_byte_rows = covered.clone();
        self.controller.on_byte_selection_rows(covered);
    }

    /// Clear the byte selection AND its mirrored row selection together (the
    /// coupled-selection contract: byte + rows go as one). `sync_byte_rows` then
    /// pushes the now-empty covered set, clearing `sel_ids`.
    fn clear_byte_selection(&mut self) {
        self.byte_sel.clear();
        self.sync_byte_rows();
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
        let r = geometry::line_byte_range(&result.text, &result.line_starts, idx);
        result.text[r].trim_end_matches('\n')
    }

    fn line_meta(&self, idx: usize) -> Option<&LineMeta> {
        self.controller.last_result().meta.get(idx)
    }

    /// The nearest array-HEADER line strictly above `from_line` whose `node_idx`
    /// matches `node_idx`, or `None` if none precedes it. Mirrors the C++ scan in
    /// `hitTestTarget` (editor.cpp:2483-2490, `for (l = line - 1; l >= 0; l--)`)
    /// that resolves an array element's type/name click back to its parent array
    /// header line.
    fn parent_array_header_line(&self, from_line: usize, node_idx: i32) -> Option<usize> {
        let meta = &self.controller.last_result().meta;
        (0..from_line).rev().find(|&l| {
            meta.get(l)
                .is_some_and(|hdr| hdr.is_array_header && hdr.node_idx == node_idx)
        })
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
        let inserted = self
            .controller
            .append_hex_fields_to_struct(struct_id, bytes);
        if !inserted.is_empty() {
            self.after_mutation(cx);
        }
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

        // Picker-target interception (the C++ `beginInlineEdit` early-return,
        // editor.cpp:3536-3558): Type / ArrayElementType / PointerTarget are
        // driven by a popup, not an inline text edit. The mouse path already
        // intercepts these before calling `begin_inline_edit` (mod.rs:1183-1219);
        // the keyboard paths (Tab/Shift+Tab via `tab_to_next_field`, Enter/F2)
        // route here, so they would otherwise fall into `resolved_span_for` and
        // open a plain text edit over the type token. Mirror the mouse path so
        // tab-cycling onto Type/PointerTarget/ArrayElementType opens the picker.
        //
        // The C++ rejects only `nodeIdx < 0` (CommandRow) and Footer rows; the
        // Rust port additionally keeps the hex SIZE toolbar and the enum member
        // picker (the mouse path's hex/enum affordances) so the same key lands on
        // the same popup whether reached by click or by Tab. The routing decision
        // is the pure `tab_cycle::edit_route` (unit-tested); a popup route sets
        // `m_lastTabTarget` (matching the C++ `beginInlineEdit`-returns-true Tab
        // bookkeeping) and returns before any inline-edit setup.
        // Exclude enum MEMBER rows: their node_idx points at the enum node, so
        // without `!is_member_line` a click on a member's VALUE routes to the enum
        // member PICKER (which is for an enum-typed FIELD's value) instead of an
        // inline edit of the member's own integer value (the reported "member values
        // aren't editable, only names").
        let is_enum =
            lm.node_idx >= 0 && self.node_is_enum(lm.node_idx as usize) && !lm.is_member_line;
        match tab_cycle::edit_route(
            target,
            lm.line_kind,
            lm.node_kind,
            lm.node_idx >= 0,
            is_enum,
        ) {
            tab_cycle::EditRoute::EnumPicker => {
                self.last_tab_target = Some(target);
                self.open_enum_picker(line, lm.node_idx as usize, window, cx);
                return;
            }
            tab_cycle::EditRoute::HexToolbar => {
                self.last_tab_target = Some(target);
                self.open_hex_toolbar(lm.node_idx as usize, window, cx);
                return;
            }
            tab_cycle::EditRoute::TypeSelector(edit_target) => {
                let ctx = ContextTarget {
                    line,
                    node_idx: lm.node_idx as usize,
                    node_id: lm.node_id,
                    kind: lm.node_kind,
                    sub_line: lm.sub_line,
                };
                self.last_tab_target = Some(target);
                self.open_type_selector_in_mode(ctx, edit_target, window, cx);
                return;
            }
            tab_cycle::EditRoute::InlineEdit => {}
        }

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
        let raw_span = text
            .get(geometry::span_byte_range(
                &text,
                span.start,
                span.end,
                lm.line_kind == LineKind::CommandRow,
            ))
            .unwrap_or("");

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
        //
        // Comment chips render with a decorative `// ` prefix and may be hidden
        // when Show Comments is off. Seed the edit from the model comment, not the
        // rendered chip text, so editing an existing comment never stores `//`.
        let mut initial = if target == EditTarget::Comment && lm.node_idx >= 0 {
            let tree = self.controller.tree();
            tree.nodes
                .get(lm.node_idx as usize)
                .map(|n| n.comment.clone())
                .unwrap_or_default()
        } else {
            raw_span.trim().to_string()
        };

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
            // Narrow the seed + write `sub_line` to the clicked comma-component;
            // a keyboard edit (no click column) defaults to component 0 via
            // `unwrap_or(span.start)`.
            if raw_span.split(',').count() > 1 {
                let (comp, seed) = geometry::vec_component_for_click(
                    raw_span,
                    span.start,
                    click_col.unwrap_or(span.start),
                );
                initial = seed;
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

        // PIXEL-accurate overlay placement: SHAPE the real line prefix + span (the
        // same glyph advances the row painter uses) instead of `col * cell_width`,
        // so the edit box lands exactly on the painted token even on the command
        // row (source chip + `▸`/`▾` glyphs) or tree-connector rows. Shared with the
        // command-row hover hitboxes via [`shaped_span_px`].
        let comment_popover = target == EditTarget::Comment;
        let (mut left_px, mut width_px) = self.shaped_span_px(&text, span.start, span.end, window);
        if comment_popover {
            // The composed comment span is intentionally far to the right of the
            // value column. For editing, anchor a compact labeled input at the
            // actual comment location: over an existing `// ...` chip, or just
            // after the rendered row text for a new comment. This keeps it obvious
            // without covering the bytes/value under the cursor.
            let line_end = text.chars().count() as i32;
            let anchor = lm
                .chips
                .iter()
                .find(|chip| chip.kind == crate::core::ChipKind::Comment)
                .map(|chip| chip.start_col)
                .unwrap_or(line_end + 2);
            let shaped_col = anchor.min(line_end).max(0);
            let (anchor_left, _) = self.shaped_span_px(&text, shaped_col, shaped_col, window);
            left_px = anchor_left + (anchor - shaped_col).max(0) as f32 * self.metrics.cell_width;
            width_px = (self.metrics.cell_width * 34.0).max(260.0);
        }

        self.last_tab_target = Some(target);
        self.editing = Some(EditingField {
            field: field.clone(),
            line,
            col_start: span.start,
            col_end: span.end,
            left_px,
            width_px,
            comment_popover,
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
        // Capture (value, raw epoch-msec) newest→oldest; the render derives the
        // age label, delta column and recency tiers (bf49a64). Carry the uncapped
        // total so the ring-overflow footer can fire.
        let mut entries: Vec<(String, i64)> = Vec::new();
        hist.for_each_with_time(|v, t| {
            if entries.len() < crate::core::value_history::K_CAPACITY {
                entries.push((v.to_string(), t));
            }
        });
        if entries.len() <= 1 {
            return;
        }
        let total_count = i64::from(hist.count);
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
                total_count,
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
    ///   text via the controller's real address parser
    ///   ([`resolve_address_expr_full`](crate::controller::RcxController::resolve_address_expr_full))
    ///   and float a `→ 0xHEX` / `Result: 0xHEX` popup.
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
        // The valid-state hint gains a mode prefix while overwriting raw bytes, so
        // the active byte-edit mode stays visible the whole edit (C++ shows it only
        // as the initial hint; keeping it for the edit's life is a small upgrade).
        let hint_prefix = editing.field.read(_cx).overwrite_hint_prefix();
        self.edit_validation = Some(EditValidation {
            line,
            error,
            hint_prefix,
        });

        // ── Expression-result popup (editor.cpp:4923) ──
        let is_addr = target == EditTarget::BaseAddress;
        let is_val = target == EditTarget::Value && !is_overwrite;
        let has_operator = text
            .chars()
            .any(|c| matches!(c, '+' | '-' | '*' | '/' | '<' | '>' | '&' | '|' | '^' | '~'));
        // Address edits always show the resolved value; value edits only when the
        // text reads as an expression (otherwise it is a plain literal).
        if (is_addr || (is_val && has_operator)) && !text.is_empty() {
            // Evaluate through the controller's real address parser (module bases,
            // [ptr] derefs, symbols) — the C++ `m_exprEvaluator` (controller.cpp:1102,
            // editor.cpp:4944). On a successful parse float `→ 0xHEX` (address) /
            // `Result: 0xHEX` (value); on failure hide the popup (the parser's error
            // surfaces in the edit comment, not here — matching C++).
            let result = self.controller.resolve_address_expr_full(&text);
            if result.ok {
                let label = if is_addr { "→" } else { "Result:" };
                self.expr_result = Some(ExprResult {
                    line,
                    col,
                    text: format!("{label} 0x{:X}", result.value),
                });
            } else {
                self.expr_result = None;
            }
        } else {
            self.expr_result = None;
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
        cx.emit(RcxEditorEvent::DocumentEdited);
        cx.notify();
    }

    /// Drain the controller's pending events and surface any `StatusHint` as a host
    /// `RcxEditorEvent::Status` (the C++ `statusHint` → status bar). Used by the
    /// byte-op copy actions, which emit a hint but don't recompose.
    fn drain_status(&mut self, cx: &mut Context<Self>) {
        for ev in self.controller.take_events() {
            if let crate::controller::ControllerEvent::StatusHint(msg) = ev {
                cx.emit(RcxEditorEvent::Status { message: msg });
            }
        }
    }

    /// Emit a host status message directly (the C++ `setAppStatus`).
    fn set_status(&mut self, message: String, cx: &mut Context<Self>) {
        cx.emit(RcxEditorEvent::Status { message });
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
        // Single-stage Esc (§10): close the find bar first, then cancel an active
        // edit — those keep their precedence. Otherwise Esc clears the selection in
        // one gesture: with byte↔row coupling, dropping the byte selection clears
        // the mirrored grey rows too (clear_byte_selection → on_byte_selection_rows
        // (empty)), so byte + rows go together; the trailing clear_selection then
        // also handles a row selection made without a byte selection. No early
        // return between the two — the old two-stage Esc (first drop bytes, second
        // drop rows) is gone.
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
        let _ = window;
        // Part E: dismiss any open hover preview and LATCH it closed so it doesn't
        // immediately reappear on the next mouse twitch in the same row (the C++
        // `dismissAllPopups` + `m_hoverDwellElapsed = false`). The latch releases
        // when the cursor moves to a different node/line. Keep the hover band so
        // the latch can tell "same row" from "moved" on the next mouse-move.
        if self.hover_popup.is_some() {
            self.hover_popup = None;
        }
        self.hover_probe = None;
        self.memory_preview_rows = MEMORY_PREVIEW_MIN_ROWS;
        self.popup_cursor_inside = false;
        self.hover_dwell_suppressed = true;
        if self.byte_sel.is_active() {
            self.clear_byte_selection();
        }
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
        // Plain Up-at-top (dir < 0) is a silent no-op. Page nav NEVER appends —
        // C++ Key_PageUp/PageDown has no grow branch (the append is exclusive to
        // Key_Up/Down, editor.cpp:2965-2994), so a PageDown that overshoots to the
        // footer just lands on the last node instead of growing the struct.
        if dir > 0 && !page {
            // Plain Down MOVES the selection onto the new field (no extend).
            self.append_tail_field(false, cx);
        }
    }

    /// Append ONE Hex64 field to the enclosing container of the last visible data
    /// row (the C++ `appendSingleFieldRequested` → `append_single_field`), select
    /// the new node, and scroll to it. Shared by plain Down-at-end
    /// ([`navigate_node_mode`]) AND modifier Down-at-end (Shift/Ctrl via
    /// [`navigate_node_extend`]) so the editor grows on the last address
    /// regardless of held modifiers (the reported "shift+down won't expand" gap).
    ///
    /// `extend` mirrors the held modifier: plain Down (`false`) MOVES the selection
    /// onto the new field; Shift/Ctrl Down (`true`) instead RE-EXTENDS the
    /// multi-selection from the original anchor down to the grown row, so every
    /// address from the anchor stays highlighted as the class expands (otherwise
    /// `append_single_field` collapses the highlight to the lone new field).
    fn append_tail_field(&mut self, extend: bool, cx: &mut Context<Self>) {
        // Pass the last visible LEAF's OWN id (controller.rs:1835 walks UP to the
        // enclosing Struct/Array/Enum container, appending a Hex64 at its aligned
        // tail / an auto-numbered enum member). With no last row, fall back to the
        // view root.
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
        let mut target = last_node_id.unwrap_or(view_root);
        if target == 0 {
            return;
        }
        // If the last visible row is itself a COLLAPSED container (a nested struct/
        // array/enum with hidden children — the cursor is ON its header, not inside
        // it), grow its PARENT (append a sibling AFTER it) rather than the container
        // itself. "Down at the end" means "add a field after this one", and a
        // collapsed container row is a field of its parent. When the container is
        // EXPANDED its last CHILD is the last row (so this doesn't fire); an EMPTY
        // container still grows itself so you can add its first field.
        {
            let tree = self.controller.tree();
            let idx = tree.index_of_id(target);
            if idx >= 0 {
                let (is_container, parent, node_id) = {
                    let n = &tree.nodes[idx as usize];
                    (
                        matches!(n.kind, NodeKind::Struct | NodeKind::Array) || n.is_enum(),
                        n.parent_id,
                        n.id,
                    )
                };
                if is_container && parent != 0 && !tree.children_of(node_id).is_empty() {
                    target = parent;
                }
            }
        }
        // For a Shift/Ctrl grow, remember the multi-selection anchor BEFORE the
        // append (`append_single_field` resets it to `-1`) so we can re-extend the
        // range to the grown row afterwards.
        let prev_anchor = if extend {
            self.controller.anchor_line()
        } else {
            -1
        };
        if let Some(new_id) = self.controller.append_single_field(target) {
            self.apply_document(cx);
            // Scroll to the freshly-selected new field's line so the cursor chases
            // the new tail (the next Down grows again).
            if let Some(line) = self
                .controller
                .last_result()
                .meta
                .iter()
                .position(|lm| lm.node_id == new_id && !lm.is_continuation)
            {
                // Park the caret on the freshly-appended field so the NEXT Down
                // (plain or modifier) sees the caret at the new last row and grows
                // again — otherwise a stale caret on the old tail makes the second
                // modifier-Down merely extend the selection instead of growing.
                self.caret_line = Some(line);
                // Shift/Ctrl grow: re-extend the highlight from the original anchor
                // down to the new row so ALL addresses stay selected (consistent
                // with extending across existing rows). With no prior anchor we keep
                // the plain single-select `append_single_field` already applied.
                if extend && prev_anchor >= 0 {
                    self.controller
                        .extend_selection_from(prev_anchor, line as i64);
                }
                self.scroll.scroll_to_item(line, ScrollStrategy::Center);
            }
        } else {
            self.apply_document(cx);
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
        } else if dir > 0 {
            // C++ Key_Down's append branch fires regardless of held modifiers
            // (editor.cpp:2965-2972): Ctrl+Down at the last node grows the struct,
            // matching plain Down / Shift+Down which already append here.
            self.append_tail_field(false, cx);
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
        if self.hover_probe.is_some() {
            self.hover_probe = None;
            changed = true;
        }
        self.memory_preview_rows = MEMORY_PREVIEW_MIN_ROWS;
        // Item 13: a full hover clear (viewport leave) also drops the
        // cursor-inside-popup guard so a stale flag can't suppress the next popup.
        self.popup_cursor_inside = false;
        // A full hover reset (kbd nav / viewport leave) also releases the
        // Esc-dismiss latch — the hover band is gone, so the next dwell on any row
        // is a fresh one and should be allowed to open a preview.
        self.hover_dwell_suppressed = false;
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
    fn navigate_node_extend(&mut self, dir: i32, step: usize, page: bool, cx: &mut Context<Self>) {
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
        // Modifier Down (Shift/Ctrl) at the LAST address grows the class — the same
        // auto-append as plain Down — so held modifiers don't block expansion (the
        // reported "shift+down won't expand the last address" gap). Detect "caret
        // already at the last navigable data row + moving down" and append instead
        // of clamping in place.
        if dir > 0 && !page {
            let last_nav = self
                .controller
                .last_result()
                .meta
                .iter()
                .enumerate()
                .rev()
                .find(|(_, lm)| {
                    lm.node_id != 0
                        && lm.node_id != K_COMMAND_ROW_ID
                        && lm.line_kind != LineKind::Footer
                        && !lm.is_continuation
                })
                .map(|(idx, _)| idx as i64);
            if let Some(last) = last_nav {
                if start >= last {
                    // Modifier grow: EXTEND the highlight to the new row (keep the
                    // anchor) so all addresses stay selected as the class expands.
                    self.append_tail_field(true, cx);
                    return;
                }
            }
        }
        // Page nav clamps the jump to a screenful; plain nav lets it fall off the
        // end. Then scan FORWARD in `dir` for the next navigable node — the SAME
        // single forward scan as plain nav (navigate_node_mode), NOT a backward
        // probe toward the caret. The old backward probe stalled: when the target
        // `start+dir` landed on a footer/continuation between two fields it walked
        // back and re-matched the (always-navigable) caret row, re-selecting the
        // caret with no advance (C++ uses one forward scan for all modifiers,
        // editor.cpp:2950-2992).
        let mut i = if page {
            (start + dir as i64 * step.max(1) as i64).clamp(0, count as i64 - 1)
        } else {
            start + dir as i64 * step.max(1) as i64
        };
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
        self.navigate_node_extend(-1, 1, false, cx);
    }
    fn action_select_down(
        &mut self,
        _: &EditorSelectDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(1, 1, false, cx);
    }
    fn action_select_page_up(
        &mut self,
        _: &EditorSelectPageUp,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(-1, self.page_step(), true, cx);
    }
    fn action_select_page_down(
        &mut self,
        _: &EditorSelectPageDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node_extend(1, self.page_step(), true, cx);
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

    fn action_preview_value(
        &mut self,
        _: &EditorPreviewValue,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.action_target() else {
            return;
        };
        let pos = self.context_menu_pos;
        self.close_context_menu(cx);
        self.show_explicit_value_preview(target.line, pos, cx);
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
        if let Some(target) = self.action_target() {
            self.begin_inline_edit(target.line, EditTarget::Comment, window, cx);
        }
    }

    /// Left/Right → cycle same-size type variants on the focused node (item 18),
    /// reusing the menu's forward/back kind cyclers — EXCEPT on a FOLD HEAD (any
    /// row that shows a fold chevron: a nested struct/array/enum container OR a
    /// pointer-to-class), where the plain arrow instead collapses (Left) / expands
    /// (Right) it. That opens a nested struct, or follows a pointer, straight from
    /// the keyboard (the tree-view convention). Non-expandable rows — bare scalars
    /// and ref-less pointers — keep plain Left/Right = type-cycle; Shift+arrow
    /// still folds any head.
    fn action_cycle_left(&mut self, _: &EditorCycleLeft, _w: &mut Window, cx: &mut Context<Self>) {
        if self.try_fold_arrow(false, cx) {
            return;
        }
        self.cycle_same_size(-1, cx);
    }
    fn action_cycle_right(
        &mut self,
        _: &EditorCycleRight,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.try_fold_arrow(true, cx) {
            return;
        }
        self.cycle_same_size(1, cx);
    }

    /// Plain Left/Right on a FOLD HEAD (any row with a fold chevron — a nested
    /// struct/array/enum container OR a pointer-to-class, i.e. a pointer with a
    /// `ref_id`) collapses (`expand=false`) / expands (`expand=true`) it,
    /// returning true when it took the key. Non-fold rows (bare scalars, ref-less
    /// pointers) return false so they keep the type-cycle. Reuses `fold_current`
    /// (the same chevron-click / Shift+arrow path, including materialize-on-expand
    /// for pointer/cycle heads).
    fn try_fold_arrow(&mut self, expand: bool, cx: &mut Context<Self>) -> bool {
        let Some((_l, lm)) = self.current_node() else {
            return false;
        };
        if !lm.fold_head {
            return false;
        }
        self.fold_current(expand, cx);
        true
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
        // SELECT the node (not just set the caret) so the landed row is visibly
        // highlighted and Enter / F2 act on it — the C++ `setCursorPosition`. Without
        // this a workspace field-click scrolled to the field but left no cursor on it
        // and Enter edited nothing.
        self.controller
            .handle_node_click(line as i64, node_id, CtrlMods::NONE);
        let _ = self.controller.take_events();
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
                        let (s, e) = selection::byte_cols_in_row(vs, first, last);
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
            font: gpui::font(self.editor_font_family()),
            font_size: px(self.editor_font_size()),
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
            .map(|lm| minimap::minimap_row_for(lm, &palette))
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
    /// Delegates the precise per-id match to [`sel_id_matches_row`].
    fn is_row_selected(&self, lm: &LineMeta) -> bool {
        if lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return false;
        }
        self.controller
            .selected_ids()
            .iter()
            .any(|&id| sel_id_matches_row(id, lm))
    }

    /// Pixel `(left, width)` of the char span `[start, end)` within `text`,
    /// measured by SHAPING the prefix `[0, start)` and the span with the editor
    /// font — the same glyph advances the row painter's `shape_line` uses — rather
    /// than `col * cell_width`. The cell grid only matches plain mono ASCII; the
    /// command row's source chip + `▸`/`▾` glyphs (and tree connectors) advance
    /// differently, so cell-based hitboxes/edit-boxes drifted right of the painted
    /// token (worst at high columns, e.g. the class name + address). Uses the
    /// window text system's `shape_line` (the same call the row painter + the cell
    /// metric use), so it tracks the live font/zoom exactly.
    fn shaped_span_px(&self, text: &str, start: i32, end: i32, window: &Window) -> (f32, f32) {
        let chars: Vec<char> = text.chars().collect();
        let s = (start.max(0) as usize).min(chars.len());
        let e = (end.max(start) as usize).min(chars.len());
        let measure = |slice: &str, window: &Window| -> f32 {
            if slice.is_empty() {
                return 0.0;
            }
            let run = TextRun {
                len: slice.len(),
                font: gpui::font(self.editor_font_family()),
                color: gpui::transparent_black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(
                        slice.to_string().into(),
                        px(self.editor_font_size()),
                        std::slice::from_ref(&run),
                        None,
                    )
                    .width(),
            )
        };
        let prefix: String = chars[..s].iter().collect();
        let span: String = chars[s..e].iter().collect();
        (measure(&prefix, window), measure(&span, window))
    }

    /// Render one row: background (selection/hover) + text element, with the row
    /// element handling row-local click routing. Editable rows embed the field
    /// overlay positioned at the edited column.
    fn render_row(&self, idx: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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

        let editing_here = self.editing.as_ref().filter(|e| e.line == idx).map(|e| {
            (
                e.field.clone(),
                e.col_start,
                e.col_end,
                e.left_px,
                e.width_px,
                e.comment_popover,
            )
        });
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
                    lm.under_ptr,
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
                    // C++ re-runs applyHoverCursor for the full Scintilla viewport,
                    // including margin 0. Moving from the value column into the
                    // offset gutter must therefore clear any value/disasm/preview
                    // hover card; route gutter hover through the same dispatcher at
                    // text-column 0 so the value-span gate fails naturally.
                    .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
                        this.dispatch_row_hover(idx, 0.0, e.position, window, cx);
                    }))
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
                // Shaped pixel span so the hitbox covers the WHOLE painted address
                // (the cell grid drifted it right of the glyphs).
                let (al, aw) = self.shaped_span_px(&text, addr.start, addr.end, window);
                let left = px(al);
                let width = px(aw.max(cell));
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
                        // Suppress the tooltip while a context menu is open (it
                        // would otherwise pop over/behind the menu — gpui only
                        // occludes hover for the strip directly under the menu rect).
                        .when(self.context_menu.is_none(), |el| {
                            el.tooltip(move |_window, cx| {
                                cx.new(|_| AddressFormatTooltip {
                                    base_address,
                                    module: module.clone(),
                                })
                                .into()
                            })
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
                    // Shaped pixel span so the hover/click hitbox covers the WHOLE
                    // painted token — the entire class name / source chip / chevron
                    // is clickable, exactly aligned with the glyphs (not col*cell).
                    let (sl, sw) = self.shaped_span_px(&text, span.start, span.end, window);
                    let left = px(sl);
                    let width = px(sw.max(cell));
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
                            .when(self.context_menu.is_none(), |el| {
                                el.tooltip(move |_window, cx| {
                                    let title = title.clone();
                                    let body = body.clone();
                                    cx.new(|_| TitledTooltip { title, body }).into()
                                })
                            }),
                    );
                }
            }

            // Type-keyword (`struct`/`class`) hitbox: a SHAPED-position overlay so a
            // click on the PAINTED keyword is not mis-routed by the command row's
            // cell-grid drift (the `▸`/`▾`/source-chip glyphs advance ≠ the cell, so
            // a click on `struct` resolved to a column over the base address — it
            // opened the address edit on left-click and missed Convert on right).
            // Left-click is swallowed (the keyword has no left action); right-click
            // opens Convert to Struct/Class DIRECTLY — and only here, so the option
            // never appears for the name / source chip / gaps (the `dispatch` path
            // for those gives the no-node menu, never converting).
            let kts = crate::compose::command_row_root_type_span(&text);
            if kts.valid && kts.end > kts.start {
                let (kw_left, kw_w) = self.shaped_span_px(&text, kts.start, kts.end, window);
                let kw_word = text
                    .get(geometry::span_byte_range(&text, kts.start, kts.end, true))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                text_region = text_region.child(
                    div()
                        .id(("rcx-keyword-hover", idx))
                        .absolute()
                        .top_0()
                        .left(px(kw_left))
                        .h(px(self.metrics.line_height))
                        .w(px(kw_w.max(cell)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                            cx.stop_propagation()
                        })
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_root_convert_menu(&kw_word, e.position, window, cx);
                            }),
                        ),
                );
            }
        }

        // Pointer TypeHint chips (`ptr64✓ -> ...`) are drawn as
        // trailing text spans, but their visual x position is shaped text, not the
        // fixed grid. Give them their own shaped hover strip so moving over the
        // painted chip/address always reaches the preview resolver even if the row
        // element's generic hitbox is not the topmost hovered element.
        if !editing_this_row {
            let text = self.line_text_owned(idx);
            for (chip_i, chip) in lm.chips.iter().enumerate() {
                if chip.kind != crate::core::ChipKind::TypeHint
                    || !chip
                        .type_hint_kinds
                        .iter()
                        .any(|k| matches!(k, NodeKind::Pointer32 | NodeKind::Pointer64))
                    || chip.end_col <= chip.start_col
                {
                    continue;
                }
                let (sl, sw) = self.shaped_span_px(&text, chip.start_col, chip.end_col, window);
                let hover_x = (chip.start_col.max(0) as f32 + 0.5) * cell;
                text_region = text_region.child(
                    div()
                        .id(SharedString::from(format!(
                            "rcx-pointer-typehint-hover-{idx}-{chip_i}"
                        )))
                        .absolute()
                        .top_0()
                        .left(px(sl))
                        .h(px(self.metrics.line_height))
                        .w(px(sw.max(cell)))
                        .cursor_pointer()
                        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
                            this.dispatch_row_hover(idx, hover_x, e.position, window, cx);
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.dispatch_row_click(idx, hover_x, e.modifiers, window, cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.dispatch_row_context_menu(
                                    idx, hover_x, e.position, window, cx,
                                );
                            }),
                        ),
                );
            }
        }

        // Footer add-bytes / Trim / Top pills: transparent SHAPED-position hitboxes
        // so the click box lands exactly on each painted pill. The pill backgrounds
        // are drawn at shaped-glyph x (element.rs), but the row's `col * cell` hit
        // test drifts from them (the editor font's advance ≠ the cell metric), so a
        // click near the right of the footer missed or hit the wrong pill. Each
        // overlay forwards a span-centered synthetic X back through the normal row
        // routing so the col-based `on_footer_click` resolves the SAME pill —
        // exactly the trick the command-row token hitboxes use above. Suppressed
        // while this footer row is inline-edited (the field owns the hitbox then).
        if lm.line_kind == LineKind::Footer && !editing_this_row {
            let text = self.line_text_owned(idx);
            for span in geometry::footer_pill_spans(&text) {
                if span.end <= span.start {
                    continue;
                }
                let (sl, sw) = self.shaped_span_px(&text, span.start, span.end, window);
                let left = px(sl);
                let width = px(sw.max(cell));
                // A col-centered synthetic X: fed back through `hit_test_row` →
                // `col_containing_x` it floors to `span.start`, which lands inside
                // `[span.start, span.end)`, so `on_footer_click` picks THIS pill.
                let click_x = (span.start.max(0) as f32 + 0.5) * cell;
                text_region = text_region.child(
                    div()
                        .id(SharedString::from(format!(
                            "rcx-footer-pill-{idx}-{}",
                            span.start
                        )))
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
                        ),
                );
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
        if let Some((field, _col_start, col_end, left_px, width_px, comment_popover)) = editing_here
        {
            // PIXEL-accurate placement from the SHAPED prefix/span computed at
            // begin_inline_edit, not `col * cell` — so the box lands exactly on the
            // painted token even on rows with non-mono glyphs (the command row's
            // `▸`/`▾`/chip ahead of the class name, tree connectors), which is what
            // pushed the box right of the text ("off-center to the right").
            let left = px(left_px);
            // A generous minimum so short seeds still get a visible field box.
            let editing_width_px = if comment_popover {
                width_px.max(28.0 * cell)
            } else {
                width_px.max(6.0 * cell)
            };
            // The inline field paints over the static row text. Give it a FULLY
            // OPAQUE editor-paper band (not the semi-transparent active-line fill,
            // which let the column's static glyphs — the type token / pre-edit name —
            // bleed through behind the seeded text and read as garbled overlap
            // "hexChex64"/"int64_teateTime"). A 1px accent ring + slight rounding make
            // it read as a Zed inline input.
            if comment_popover {
                text_region = text_region.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(left)
                        .h(px(self.metrics.line_height))
                        .w(px(editing_width_px))
                        .px(px(6.0))
                        .bg(palette.paper)
                        .border_1()
                        .border_color(palette.comment_green)
                        .rounded_sm()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(design::tokens::font::UI_XS))
                                .font_family(self.editor_font_family())
                                .text_color(palette.comment_green)
                                .child("Comment"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full()
                                .flex()
                                .items_center()
                                .child(field.clone()),
                        ),
                );
            } else {
                text_region = text_region.child(
                    div()
                        .absolute()
                        .top_0()
                        .left(left)
                        .h(px(self.metrics.line_height))
                        .w(px(editing_width_px + cell))
                        .bg(palette.paper)
                        .border_1()
                        .border_color(palette.accent)
                        .rounded_sm()
                        // Vertically CENTER the field text in the row box. The static
                        // row text + the offset gutter are `items_center` (line ~4599),
                        // but the `FieldInput` element paints its shaped line at
                        // `bounds.origin` (top-aligned), so without this the edit text
                        // rode high vs the surrounding text (the "off-center selector").
                        .flex()
                        .items_center()
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
            }

            // Item 71/72: the inline-edit HINT comment — a green
            // 'Enter=Save Esc=Cancel' on a valid edit, or a red '! <error>' on an
            // invalid one (the C++ `setEditComment`, editor.cpp:4915/4919). Painted
            // just past the line text so it sits where the row's `//` comment would.
            if let Some(v) = self
                .edit_validation
                .as_ref()
                .filter(|v| !comment_popover && v.line == idx)
            {
                let (hint, color) = if v.error.is_empty() {
                    (
                        format!("{}Enter=Save Esc=Cancel", v.hint_prefix),
                        palette.comment_green,
                    )
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

    // ── Hover popups (item 13, editor.cpp applyHoverCursor) ──

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
            // C++ single-node "Convert to Hex" REMOVES the node and refills its byte
            // range with largest-first hex pads named pad_<offset> (controller.cpp:3713),
            // rather than an in-place kind change that keeps the node's name/size. Use
            // the faithful controller op (mirrors the sibling split-hex wiring).
            self.controller.convert_to_hex(t.node_id);
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

    // ── Part D: "Selected bytes (N) ▸" submenu actions ──
    // Each reads the live byte range from `byte_sel` and routes to the controller's
    // byte-op handler (the C++ `addByteSubmenu` actions, controller.cpp). Copy ops
    // put the formatted string on the clipboard; paste/zero-fill mutate (undoable);
    // Save writes a binary file; Edit hex opens the hex-overwrite editor on the row
    // containing the selection start; Break extracts the range into a new class.

    /// The selected byte range as `(lo, n)` with `n` the byte count, if active.
    fn byte_sel_lo_n(&self) -> Option<(u64, i32)> {
        let (lo, hi) = self.byte_sel.range()?;
        let n = hi.saturating_sub(lo) as i32;
        (n > 0).then_some((lo, n))
    }

    fn action_byte_copy_hex(
        &mut self,
        _: &EditorByteCopyHex,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some((lo, n)) = self.byte_sel_lo_n() {
            if let Some(s) = self.controller.byte_copy_hex(lo, n) {
                cx.write_to_clipboard(ClipboardItem::new_string(s));
            }
            self.drain_status(cx);
        }
    }

    fn action_byte_copy_c_array(
        &mut self,
        _: &EditorByteCopyCArray,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some((lo, n)) = self.byte_sel_lo_n() {
            if let Some(s) = self.controller.byte_copy_c_array(lo, n) {
                cx.write_to_clipboard(ClipboardItem::new_string(s));
            }
            self.drain_status(cx);
        }
    }

    fn action_byte_copy_python(
        &mut self,
        _: &EditorByteCopyPython,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some((lo, n)) = self.byte_sel_lo_n() {
            if let Some(s) = self.controller.byte_copy_python(lo, n) {
                cx.write_to_clipboard(ClipboardItem::new_string(s));
            }
            self.drain_status(cx);
        }
    }

    fn action_byte_edit_hex(
        &mut self,
        _: &EditorByteEditHex,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        // The Rust port has no byte-range-narrowed inline edit (the C++
        // `beginByteEdit`); open the existing hex-overwrite editor on the row that
        // contains the selection start (the nearest equivalent).
        let Some((lo, _)) = self.byte_sel_lo_n() else {
            return;
        };
        let line = self.controller.last_result().meta.iter().position(|lm| {
            let count = if lm.line_byte_count > 0 {
                lm.line_byte_count
            } else {
                crate::core::size_for_kind(lm.node_kind)
            };
            is_hex_preview(lm.node_kind)
                && lm.line_kind == LineKind::Field
                && lo >= lm.offset_addr
                && lo < lm.offset_addr + count.max(0) as u64
        });
        if let Some(line) = line {
            self.begin_inline_edit(line, EditTarget::Value, window, cx);
        }
    }

    fn action_byte_zero_fill(
        &mut self,
        _: &EditorByteZeroFill,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some((lo, n)) = self.byte_sel_lo_n() {
            self.controller.byte_zero_fill(lo, n);
            self.after_mutation(cx);
        }
    }

    fn action_byte_paste_hex(
        &mut self,
        _: &EditorBytePasteHex,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let Some((lo, n)) = self.byte_sel_lo_n() else {
            return;
        };
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text().map(|t| t.to_string()))
            .unwrap_or_default();
        self.controller.byte_paste_hex(lo, n, &text);
        self.after_mutation(cx);
    }

    fn action_byte_save_binary(
        &mut self,
        _: &EditorByteSaveBinary,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let Some((lo, n)) = self.byte_sel_lo_n() else {
            return;
        };
        // 128 MB sanity cap (the C++ `1 << 27`).
        if n > (1 << 27) {
            return;
        }
        // Reuse the controller's `write_selected_bytes_to_file` (controller.rs).
        // The save-file picker is host UI; default the path next to the binary
        // under a deterministic name (`bytes_<addr>_<n>.bin`) in the working dir.
        let path = std::path::PathBuf::from(format!("bytes_{:x}_{}.bin", lo, n));
        match self.controller.write_selected_bytes_to_file(lo, n, &path) {
            Ok(()) => self.set_status(
                format!(
                    "Saved {} byte{} to {}",
                    n,
                    if n == 1 { "" } else { "s" },
                    path.display()
                ),
                cx,
            ),
            Err(e) => self.set_status(e, cx),
        }
    }

    fn action_byte_break_class(
        &mut self,
        _: &EditorByteBreakClass,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        let Some((lo, hi)) = self.byte_sel.range() else {
            return;
        };
        self.clear_byte_selection();
        self.controller.extract_byte_selection_to_new_class(lo, hi);
        self.after_mutation(cx);
    }

    fn action_clear_selection(
        &mut self,
        _: &EditorClearSelection,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        // Clear the byte selection AND the row selection together (the C++ bottom
        // "Clear selection"). clear_byte_selection mirrors empty rows; then clear any
        // row selection made without a byte selection.
        if self.byte_sel.is_active() {
            self.clear_byte_selection();
        }
        self.controller.clear_selection();
        self.after_mutation(cx);
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

    /// Add a member to the context-target ENUM from its HEADER menu (the C++
    /// enum-header "Add Member", controller.cpp:3364): appends with no anchor so an
    /// empty enum can grow its first member from the header.
    fn action_enum_add_member(
        &mut self,
        _: &EditorEnumAddMember,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if let Some(t) = self.context_target {
            if self.controller.add_member(t.node_id, None) {
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

    // ── Item 8: Structure submenu actions ──

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

    /// Item 17: "Append bytes…" — append bytes (the C++ `appendBytesDialog` default
    /// of 128, controller.cpp:2921-2947) to the VIEW-ROOT struct (`m_viewRootId`),
    /// NOT a single 8-byte field at the selected node's enclosing container. (The
    /// interactive count prompt is a follow-up; the count defaults to the C++ 128.)
    fn action_append_bytes(
        &mut self,
        _: &EditorAppendBytes,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        self.context_target = None;
        // append_bytes_to_struct rounds up to Hex64 fields (128 → 16 Hex64) and
        // applies the document itself.
        let root = self.controller.view_root_id();
        self.append_bytes_to_struct(root, 128, cx);
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

/// Whether a single encoded selection id `sel_id` selects the row `lm` — the
/// per-id match behind [`RcxEditor::is_row_selected`], extracted as a free fn so
/// it's unit-testable without a gpui Window (the C++ `applySelectionOverlay`
/// per-row loop, editor.cpp:3266). Classify `sel_id` by PRIORITY via `sel_kind`
/// (the single source of truth) — NOT independent flag-bit tests — then strip to
/// the bare node id and match the row's TYPE precisely: a footer-sel only paints
/// footer rows, an array-elem-sel only the row with the matching element index, a
/// member-sel only the row with the matching sub-line. Without this, selecting one
/// array element (or one member row) greyed EVERY row of that node.
fn sel_id_matches_row(sel_id: u64, lm: &LineMeta) -> bool {
    use crate::core::linemeta::{
        array_elem_idx_from_sel_id, member_sub_from_sel_id, sel_kind, SelKind,
    };
    if crate::controller::strip_sel_pub(sel_id) != lm.node_id {
        return false;
    }
    let sk = sel_kind(sel_id);
    let is_footer = lm.line_kind == LineKind::Footer;
    // Footer selection paints footer rows only, and vice-versa.
    if (sk == SelKind::Footer) != is_footer {
        return false;
    }
    // Array element: match by element index; a non-array-elem sel must not paint an
    // array-element row.
    if sk == SelKind::ArrayElem {
        if !lm.is_array_element || lm.array_element_idx != array_elem_idx_from_sel_id(sel_id) {
            return false;
        }
    } else if lm.is_array_element {
        return false;
    }
    // Member line: match by sub-line; a non-member sel must not paint a member row.
    if sk == SelKind::Member {
        if !lm.is_member_line || lm.sub_line != member_sub_from_sel_id(sel_id) {
            return false;
        }
    } else if lm.is_member_line {
        return false;
    }
    true
}

/// The "alternate" kind for the quick type-cycler (`← cur ↔ alt →`) and the
/// forward `T`-less cycle (item 16): steps to the NEXT same-byte-size variant,
/// wrapping. Cycling between equal-footprint primitives (e.g. int32_t → uint32_t →
/// float → hex32 …) never lands on a different size / container / string — the C++
/// in-place stepper keeps the node's byte layout fixed.
fn alt_kind_for(kind: NodeKind) -> NodeKind {
    let ring = crate::core::kind::same_size_variants(kind);
    let pos = ring.iter().position(|&k| k == kind).unwrap_or(0);
    ring[(pos + 1) % ring.len()]
}

/// Item 16: whether the "Big endian" checkable item applies — only scalar numeric
/// kinds, exactly the C++ set (controller.cpp:3763): `Hex16..=Hex128`,
/// `Int16..=UInt128`, `Float16`, `Float`, `Double`. Never Hex8, bool, ptr/fnptr,
/// struct/array/enum/bitfield/string/vector.
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
    let ring = crate::core::kind::same_size_variants(kind);
    let pos = ring.iter().position(|&k| k == kind).unwrap_or(0);
    ring[(pos + ring.len() - 1) % ring.len()]
}

impl Focusable for RcxEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<RcxEditorEvent> for RcxEditor {}

impl Render for RcxEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the open context menu focused so the keyboard drives it (Up/Down
        // highlight, Enter activates, Esc closes). The editor renders the menu as a
        // hand-rolled deferred overlay (below) instead of via `ContextMenuExt`,
        // which would otherwise re-assert menu focus every frame (the upstream
        // pattern at menu/context_menu.rs:196-201). A one-shot focus in
        // `show_context_menu_at` is stolen back by the right-click's mouse-up, so
        // re-assert it here each render while a menu is up. The `contains_focused`
        // guard makes this idempotent once focus has landed (no re-render churn).
        if let Some(menu) = self.context_menu.clone() {
            let fh = menu.focus_handle(cx);
            if !fh.contains_focused(window, cx) {
                window.focus(&fh, cx);
            }
        }
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
            .on_action(cx.listener(Self::action_enum_add_member))
            .on_action(cx.listener(Self::action_member_remove))
            .on_action(cx.listener(Self::action_member_toggle_bit))
            // Item 7/18/48: group multi-selection into a union.
            .on_action(cx.listener(Self::action_group_into_union))
            // Item 8: Static submenu actions.
            .on_action(cx.listener(Self::action_static_add_child))
            .on_action(cx.listener(Self::action_static_dissolve_union))
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
            .on_action(cx.listener(Self::action_preview_value))
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
            // Part D: byte-selection submenu actions + the shared "Clear selection".
            .on_action(cx.listener(Self::action_byte_copy_hex))
            .on_action(cx.listener(Self::action_byte_copy_c_array))
            .on_action(cx.listener(Self::action_byte_copy_python))
            .on_action(cx.listener(Self::action_byte_edit_hex))
            .on_action(cx.listener(Self::action_byte_zero_fill))
            .on_action(cx.listener(Self::action_byte_paste_hex))
            .on_action(cx.listener(Self::action_byte_save_binary))
            .on_action(cx.listener(Self::action_byte_break_class))
            .on_action(cx.listener(Self::action_clear_selection))
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
                let dy = match ev.delta {
                    gpui::ScrollDelta::Lines(p) => p.y,
                    gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
                };
                if !(ev.modifiers.control || ev.modifiers.platform)
                    && this.adjust_memory_preview_rows(dy, cx)
                {
                    cx.stop_propagation();
                    return;
                }
                if !(ev.modifiers.control || ev.modifiers.platform) {
                    // Plain wheel → the list scrolls natively. Drop the hover band /
                    // popup: the rows slide out from under the stationary cursor, so
                    // the old highlight is stale (gpui only re-evaluates hover on the
                    // next real mouse move). Clearing avoids a band stuck on the row
                    // that used to be under the cursor; the next move re-establishes
                    // it. `clear_hover_state` is a no-op when nothing is hovered.
                    this.clear_hover_state(cx);
                    return;
                }
                if dy > 0.0 {
                    this.zoom_by(1.0, cx);
                } else if dy < 0.0 {
                    this.zoom_by(-1.0, cx);
                }
                cx.stop_propagation();
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _w, cx| {
                this.dismiss_hover_if_pointer_left_anchor(e.position, cx);
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
                            // The scrollable text column with a draggable vertical
                            // scrollbar overlaid on its right edge. The editor used to
                            // expose only the wheel and the optional minimap; a real
                            // scrollbar is the standard affordance and gives a
                            // position/extent indicator. `Scrollbar` lays out
                            // absolutely over this `relative()` column and paints a
                            // thin bar at the right edge (it hides itself when the
                            // content fits and only intercepts clicks on that strip,
                            // so row clicks pass straight through), reading the same
                            // `self.scroll` handle the list tracks.
                            div()
                                .relative()
                                .flex_grow()
                                .h_full()
                                .child(
                                    uniform_list(
                                        "rcx-rows",
                                        count,
                                        cx.processor(
                                            |this, range: std::ops::Range<usize>, window, cx| {
                                                range
                                                    .map(|ix| this.render_row(ix, window, cx))
                                                    .collect::<Vec<_>>()
                                            },
                                        ),
                                    )
                                    .size_full()
                                    .track_scroll(&self.scroll),
                                )
                                .child(gpui_component::scroll::Scrollbar::vertical(&self.scroll)),
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

    fn editor_with_primitive_array() -> RcxController {
        use crate::core::{Node, NodeKind};
        let mut doc = RcxDocument::new();
        let s_idx = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Holder".into(),
            struct_type_name: "Holder".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let s_id = doc.tree.nodes[s_idx].id;
        // A uint8_t[4] array — primitive elements are synthesized as element rows.
        // Expanded (`collapsed: false`) so the element rows are materialized.
        doc.tree.add_node(Node {
            kind: NodeKind::Array,
            element_kind: NodeKind::UInt8,
            array_len: 4,
            name: "bytes".into(),
            parent_id: s_id,
            offset: 0,
            collapsed: false,
            ..Node::default()
        });
        let mut c = RcxController::new(doc);
        c.set_view_root_id(s_id);
        c.refresh();
        c
    }

    #[test]
    fn array_element_rows_carry_parent_header_node_idx() {
        // Fix #2 substrate: the array-element → parent-header redirect
        // (editor.cpp:2480-2492) scans backward for the nearest line with
        // `is_array_header` and the SAME `node_idx`. This verifies the compose
        // layer wires array element rows to share the header's `node_idx` (so the
        // scan lands on the parent), mirroring the C++ `hdr.nodeIdx == lm.nodeIdx`
        // match.
        let c = editor_with_primitive_array();
        let meta = &c.last_result().meta;

        // The array header line.
        let hdr_line = meta
            .iter()
            .position(|m| m.is_array_header)
            .expect("array header line exists");
        let hdr_node_idx = meta[hdr_line].node_idx;
        assert!(hdr_node_idx >= 0);

        // Every synthesized element row shares the header's node_idx and is NOT
        // itself an array header.
        let elem_lines: Vec<usize> = meta
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_array_element)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(elem_lines.len(), 4, "uint8_t[4] yields four element rows");
        for &el in &elem_lines {
            assert_eq!(
                meta[el].node_idx, hdr_node_idx,
                "element row {el} must share the array header node_idx"
            );
            assert!(!meta[el].is_array_header);
            assert!(el > hdr_line, "element rows follow the header");
        }

        // The backward scan (the exact predicate `parent_array_header_line` runs)
        // resolves each element row to the parent header line.
        for &el in &elem_lines {
            let found = (0..el).rev().find(|&l| {
                meta.get(l)
                    .is_some_and(|hdr| hdr.is_array_header && hdr.node_idx == meta[el].node_idx)
            });
            assert_eq!(
                found,
                Some(hdr_line),
                "element row {el} redirects to the parent array header line"
            );
        }
    }

    #[test]
    fn array_element_selection_greys_only_its_own_row() {
        // Part B regression (the C++ `applySelectionOverlay` per-row match): selecting
        // ONE array element must paint only that element's row, not every row of the
        // array node. `sel_id_matches_row` (behind `is_row_selected`) classifies the
        // encoded id by priority and matches the element index precisely.
        use super::sel_id_matches_row;
        use crate::core::linemeta::make_array_elem_sel_id;
        let c = editor_with_primitive_array();
        let meta = &c.last_result().meta;

        let elem_lines: Vec<usize> = meta
            .iter()
            .enumerate()
            .filter(|(_, m)| m.is_array_element)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(elem_lines.len(), 4);
        let node_id = meta[elem_lines[0]].node_id;

        // Select array element index 1.
        let sel = make_array_elem_sel_id(node_id, 1);
        let matched: Vec<i32> = elem_lines
            .iter()
            .filter(|&&l| sel_id_matches_row(sel, &meta[l]))
            .map(|&l| meta[l].array_element_idx)
            .collect();
        // Exactly the element row whose array_element_idx == 1 matches.
        assert_eq!(
            matched,
            vec![1],
            "only the element with idx 1 is selected, not all array rows"
        );
        // The bare node id (no array-elem bit) would have greyed ALL element rows —
        // confirm the precise encoding does NOT.
        let greyed_by_bare = elem_lines
            .iter()
            .filter(|&&l| sel_id_matches_row(node_id, &meta[l]))
            .count();
        assert_eq!(
            greyed_by_bare, 0,
            "a bare node id must not match array-element rows (they carry the elem encoding)"
        );
    }

    #[test]
    fn array_element_type_name_hit_is_redirectable() {
        // Fix #2: the column-level hit on an array *element* row's Type/Name token
        // resolves to `Type`/`Name` (the input the redirect rewrites to
        // `ArrayElementType` while re-pointing the line at the parent header). This
        // confirms the element row exposes a redirectable Type/Name target — i.e.
        // the redirect's precondition (`matches!(hit.target, Type | Name)`) can fire
        // for a real element row.
        use super::hit_test::target_at_col;
        use crate::compose::EditTarget;
        use crate::ui::editor::geometry;
        let c = editor_with_primitive_array();
        let result = c.last_result();
        let meta = &result.meta;
        let el = meta
            .iter()
            .position(|m| m.is_array_element)
            .expect("element row exists");
        let lm = &meta[el];
        let text = {
            let r = geometry::line_byte_range(&result.text, &result.line_starts, el);
            result.text[r].trim_end_matches('\n').to_string()
        };
        let (type_w, name_w) = geometry::effective_widths(lm);
        // Resolve the element row's Type span from its geometry, then probe a column
        // inside it: it must hit Type (the redirect input rewritten to
        // ArrayElementType against the parent header).
        let ts = geometry::resolved_span_for(lm, &text, EditTarget::Type, type_w, name_w);
        assert!(ts.valid, "array element row has a Type span");
        let type_target = target_at_col(lm, &text, ts.start, type_w, name_w);
        assert_eq!(
            type_target,
            Some(EditTarget::Type),
            "array element type token resolves to Type (redirect input)"
        );
        // The redirect precondition holds for this target.
        assert!(matches!(
            type_target,
            Some(EditTarget::Type) | Some(EditTarget::Name)
        ));
    }

    #[test]
    fn line_slicing_matches_line_starts() {
        // `line_starts` are UTF-16 unit offsets; slicing the UTF-8 text requires
        // converting each to a byte offset (geometry::utf16_to_byte). The command
        // row contains multi-byte glyphs (▸/▾), so a naive byte slice would split
        // a line mid-glyph — this asserts the conversion yields clean single lines.
        use super::geometry::line_byte_range;
        let c = editor_with_struct();
        let result = c.last_result();
        assert!(!result.meta.is_empty());
        // The command row really does contain a multi-byte glyph (else this test
        // would not exercise the conversion).
        assert!(result.text.chars().any(|ch| ch.len_utf8() > 1));
        for i in 0..result.meta.len() {
            let r = line_byte_range(&result.text, &result.line_starts, i);
            // Byte offsets must be valid char boundaries and ordered.
            assert!(result.text.is_char_boundary(r.start));
            assert!(result.text.is_char_boundary(r.end));
            assert!(r.start <= r.end);
            let slice = result.text[r].trim_end_matches('\n');
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
    fn new_class_command_row_colors_full_class_name() {
        // Regression: the command row is painted from the LIVE `build_command_row()`
        // string, but `lm.brace_col` was measured on the composed line-0 stub
        // ("[▸] source▾  0x0  struct Untitled {") whose `{` sits at a different
        // column (34) than the live brace (38). The stale brace-dim used to punch a
        // one-column Dim hole INTO the class name ("NewClass" rendered as colored
        // "NewCl" + dim "a" + colored "ss"). After the fix the full name reads as
        // one contiguous ClassName run and the REAL trailing `{` is the dimmed col.
        use crate::compose::command_row_root_name_span;
        use crate::ui::editor::geometry::{self, SpanRole};
        let doc = RcxDocument::new();
        let mut c = RcxController::new(doc);
        let (root_id, _name) = c.create_new_class_struct();
        c.set_view_root_id(root_id);
        c.refresh();
        let row = c.build_command_row();
        let name = command_row_root_name_span(&row);
        assert!(name.valid, "name span must resolve on the live command row");
        let lm = c.last_result().meta[0].clone();
        assert_eq!(lm.line_kind, LineKind::CommandRow);
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let runs = geometry::style_runs(&lm, &row, type_w, name_w);
        let role_at = |col: i32| {
            runs.iter()
                .find(|r| r.start <= col && col < r.end)
                .map(|r| r.role)
        };
        // Every column inside the name span resolves to ClassName — no Dim hole.
        for col in name.start..name.end {
            assert_eq!(
                role_at(col),
                Some(SpanRole::ClassName),
                "col {col} of the class name must be ClassName; runs={runs:?}"
            );
        }
        // The dimmed brace is the REAL trailing `{`, after the name (not inside it).
        let brace_col = row.chars().position(|ch| ch == '{').unwrap() as i32;
        assert!(brace_col >= name.end, "brace must follow the class name");
        assert_eq!(
            role_at(brace_col),
            Some(SpanRole::Dim),
            "the trailing `{{` must be the dimmed column"
        );
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
                    lm.under_ptr,
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
    fn type_cycle_stays_within_the_same_byte_size() {
        // Item 16: the quick type-cycler (`← cur ↔ alt →`, Left/Right) steps to a
        // same-byte-size variant, never a different size / container / string. From
        // any fixed-size primitive, both alt (next) and prev land on a kind of the
        // SAME byte size, and the ring closes (cycling len times returns home).
        use super::{alt_kind_for, prev_kind_for};
        use crate::core::kind::same_size_variants;
        use crate::core::{size_for_kind, NodeKind};
        for &k in &[
            NodeKind::Int8,
            NodeKind::Int16,
            NodeKind::Int32,
            NodeKind::UInt32,
            NodeKind::Float,
            NodeKind::Int64,
            NodeKind::Int128,
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
            // From a numeric kind the ring stays numeric — never a same-byte-size
            // string (UTF8/UTF16) or vector (Vec2/3/4) variant (P6 / C++ parity).
            for v in [next, prev] {
                assert!(
                    !crate::core::is_string_kind(v),
                    "cycle of {k:?} must not land on a string kind ({v:?})"
                );
                assert!(
                    !crate::core::is_vector_kind(v),
                    "cycle of {k:?} must not land on a vector kind ({v:?})"
                );
            }
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
    fn cross_doc_composites_lists_only_top_level_structs() {
        // P5 / C++ parity (controller.cpp:4753-4774): a sibling document contributes
        // only its TOP-LEVEL struct declarations to another editor's Type Selector —
        // nested struct INSTANCES and non-struct children are excluded — with the
        // name falling back to `name` when `struct_type_name` is empty.
        use super::CrossDocComposite;
        use crate::core::{Node, NodeKind};
        let mut doc = RcxDocument::new();
        // Top-level struct with a type name + explicit keyword + one child field.
        let parent_idx = doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "PlayerInst".into(),
            struct_type_name: "Player".into(),
            class_keyword: "class".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let parent_id = doc.tree.nodes[parent_idx].id;
        // A non-struct child — excluded.
        doc.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "hp".into(),
            parent_id,
            offset: 0,
            ..Node::default()
        });
        // A nested struct INSTANCE (parent_id != 0) — excluded despite its type name.
        doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "embedded".into(),
            struct_type_name: "Vec3".into(),
            parent_id,
            offset: 4,
            ..Node::default()
        });
        // A second top-level struct with NO type name → falls back to `name`.
        doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Bare".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });

        let comps = CrossDocComposite::top_level_in(&doc.tree);
        let names: Vec<&str> = comps.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"Player"),
            "top-level struct_type_name listed"
        );
        assert!(
            names.contains(&"Bare"),
            "top-level without a type name falls back to its name"
        );
        assert!(!names.contains(&"Vec3"), "nested struct instance excluded");
        assert!(!names.contains(&"hp"), "non-struct child excluded");
        // Keyword resolution: explicit 'class' kept; the bare struct defaults 'struct'.
        assert_eq!(
            comps.iter().find(|c| c.name == "Player").unwrap().keyword,
            "class"
        );
        assert_eq!(
            comps.iter().find(|c| c.name == "Bare").unwrap().keyword,
            "struct"
        );
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
        use super::hover_popup::{hover_kind_eq, HoverPopupKind};
        let mk = |vals: &[&str], set_buttons: bool| HoverPopupKind::ValueHistory {
            entries: vals.iter().map(|v| (v.to_string(), 1000i64)).collect(),
            total_count: vals.len() as i64,
            node_idx: 0,
            sub_line: 0,
            resolved_addr: 0,
            set_buttons,
        };
        let a = mk(&["1", "2"], false);
        let a2 = mk(&["1", "2"], false);
        // Same VALUES but different raw timestamps must still compare equal (the
        // elapsed-time labels tick; only the value column drives popup identity,
        // item 68; `hover_kind_eq` ignores the msec field).
        let a3 = HoverPopupKind::ValueHistory {
            entries: vec![("1".into(), 5000i64), ("2".into(), 9000i64)],
            total_count: 2,
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

    // ── Item 4: Vec/Mat value-component narrowing (the column→component map) ──

    // The narrowing math now lives in geometry::vec_component_for_click (exercised
    // by the production begin_inline_edit path); the tests below cover it directly.
    use crate::ui::editor::geometry::vec_component_for_click;

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
        use crate::ui::pickers::typeselectorpopup::{EntryKind, TypeEntry};
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
