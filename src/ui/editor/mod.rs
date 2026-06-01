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
        EditorConvFnPtr64,
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
        // F12 Go To Definition (item 20).
        KeyBinding::new("f12", EditorGoToDefinition, Some("RcxEditor")),
        // Collapse-all / expand-all (item 19).
        KeyBinding::new("ctrl-shift-[", EditorCollapseAll, Some("RcxEditor")),
        KeyBinding::new("ctrl-shift-]", EditorExpandAll, Some("RcxEditor")),
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
    _find_bar_sub: Option<Subscription>,
    /// The open EnumPicker / HexToolbar popup subscriptions (items 8/9).
    _enum_picker_sub: Option<Subscription>,
    _hex_toolbar_sub: Option<Subscription>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
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
}

/// The kind of hover popup shown over a row's value column (item 13). The C++
/// `applyHoverCursor` opens one of three popups depending on the node:
/// value-history (heated changed values), disasm/hex-dump (func/void pointers),
/// or struct-preview (typed pointer).
#[derive(Clone, Debug)]
enum HoverPopupKind {
    /// A changed-value history list (newest → oldest), the heat graph analogue.
    ValueHistory { lines: Vec<String> },
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
            hover_popup: None,
            caret_line: None,
            drag_anchor_line: None,
            drag_on_byte_grid: false,
            drag_anchor_x: 0.0,
            drag_started: false,
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
            _find_bar_sub: None,
            _enum_picker_sub: None,
            _hex_toolbar_sub: None,
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

        // Fold-prefix click → toggle collapse via the controller.
        if hit.in_fold_col {
            if lm.node_idx >= 0 {
                self.controller.toggle_collapse(lm.node_idx as usize);
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
                    let ref_id = {
                        let tree = self.controller.tree();
                        tree.nodes
                            .get(lm.node_idx as usize)
                            .map(|n| n.ref_id)
                            .unwrap_or(0)
                    };
                    if ref_id != 0 && self.controller.tree().index_of_id(ref_id) >= 0 {
                        cx.emit(RcxEditorEvent::OpenTypeInNewTab { ref_id });
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
                self.begin_inline_edit(line, target, window, cx);
                return;
            }
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
                    _ => None,
                };
                break;
            }
        }
        let Some(tok) = hit_tok else {
            return false;
        };
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
                self.append_bytes_to_struct(lm.node_id, bytes, cx);
                true
            }
            // Trim trailing hex / +10 enum members need controller ops not exposed
            // here; consume the click without mutating (the pill still highlights).
            _ => true,
        }
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
        let span = geometry::resolved_span_for(&lm, &text, target, type_w, name_w);
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
        let initial = raw_span.trim().to_string();

        let resolved_addr = lm.offset_addr;
        let node_idx = lm.node_idx;
        let sub_line = lm.sub_line;
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
        let subscription = cx.observe(&field, |this: &mut RcxEditor, field, cx| {
            let outcome = field.update(cx, |f, _| f.take_outcome());
            if let Some(outcome) = outcome {
                this.resolve_edit_outcome(outcome, cx);
            }
            cx.notify();
        });

        self.last_tab_target = Some(target);
        self.editing = Some(EditingField {
            field: field.clone(),
            line,
            col_start: span.start,
            col_end: span.end,
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
        cx.notify();
    }

    /// Apply a committed/cancelled inline edit (the `inlineEditCommitted`/
    /// `inlineEditCancelled` round-trip, editor-surface.md §11).
    fn resolve_edit_outcome(&mut self, outcome: EditOutcome, cx: &mut Context<Self>) {
        match outcome {
            EditOutcome::Commit(commit) => {
                self.editing = None;
                self.apply_commit(&commit, cx);
            }
            EditOutcome::Cancel => {
                self.editing = None;
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
        let commit = editing.field.read(cx).to_commit();
        self.apply_commit(&commit, cx);
    }

    /// Route a committed edit to the matching controller mutation
    /// (editor-surface.md §1: the controller recomposes, then we refresh).
    fn apply_commit(&mut self, commit: &EditCommit, cx: &mut Context<Self>) {
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
            EditTarget::Name | EditTarget::RootClassName => {
                self.controller.rename_node(idx, &commit.text);
            }
            EditTarget::Type | EditTarget::ArrayElementType => {
                self.controller.apply_type_text(idx, &commit.text);
            }
            EditTarget::Value => {
                self.controller.set_node_value(
                    idx,
                    commit.sub_line,
                    &commit.text,
                    false,
                    commit.resolved_addr,
                );
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
    fn commit_array_count(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let count: i32 = match text
            .trim_start_matches("0x")
            .parse::<i32>()
            .ok()
            .or_else(|| i32::from_str_radix(text.trim_start_matches("0x"), 16).ok())
        {
            Some(c) if c > 0 => c,
            _ => return,
        };
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
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

    /// Write a committed static-expression edit (item 15) via `ChangeOffsetExpr`.
    fn commit_static_expr(&mut self, idx: usize, text: &str, _cx: &mut Context<Self>) {
        let tree = self.controller.tree();
        if idx >= tree.nodes.len() {
            return;
        }
        let n = &tree.nodes[idx];
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
            _ => {}
        }
        let _ = cx;
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
        // Toggle: a second Ctrl+F closes the bar.
        if self.find_bar.is_some() {
            self.close_find_bar(cx);
            return;
        }
        if self.editing.is_some() {
            self.commit_active_edit(window, cx);
        }
        let lines = self.current_line_texts();
        let bar = cx.new(|cx| FindBar::new(lines, window, cx));
        let focus = bar.read(cx).focus_handle(cx);
        self._find_bar_sub = Some(cx.subscribe_in(
            &bar,
            window,
            move |this, _b, ev: &FindEvent, _window, cx| match ev {
                FindEvent::Navigate(m) => {
                    this.find_match = Some(*m);
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
        self.find_bar = None;
        self._find_bar_sub = None;
        self.find_match = None;
        cx.notify();
    }

    /// The rendered text of every composed line (the find bar searches these — the
    /// same strings the rows paint, including the live command-row substitution).
    fn current_line_texts(&self) -> Vec<String> {
        let count = self.controller.last_result().meta.len();
        (0..count).map(|i| self.line_text_owned(i)).collect()
    }

    // ── Normal-mode quick keys (editor.cpp `handleNormalKey`, item 12) ──

    /// The `(line, LineMeta)` of the current node — the primary-selected data row
    /// (`currentNodeIndex`). Skips chrome rows. `None` when no node is selected.
    fn current_node(&self) -> Option<(usize, LineMeta)> {
        let line = self.first_selected_line()?;
        let lm = self.line_meta(line)?.clone();
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return None;
        }
        Some((line, lm))
    }

    /// Change the current node's kind via the controller (the `quickTypeChange`
    /// helper the P/F/S/U/1-5/Space handlers funnel through).
    fn quick_change_kind(&mut self, new_kind: NodeKind, cx: &mut Context<Self>) {
        if let Some((_line, lm)) = self.current_node() {
            self.controller
                .change_node_kind(lm.node_idx as usize, new_kind);
            self.apply_document(cx);
        }
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
        let result = self.controller.last_result();
        let count = result.meta.len();
        if count == 0 {
            return;
        }
        let start = self
            .first_selected_line()
            .map(|l| l as i64)
            .unwrap_or(if dir > 0 { 0 } else { count as i64 });
        let mut i = start + dir as i64 * step.max(1) as i64;
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
            self.scroll.scroll_to_item(line, ScrollStrategy::Center);
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
            // Grow the ENCLOSING STRUCT of the last visible field, NOT the field /
            // array itself. The last visible row's PARENT is that struct (a
            // top-level array's parent is the view-root struct), so appending there
            // grows the struct PAST the array — appending to the *array node* would
            // instead grow the array's element count (+1 byte: the "0x80 → 0x81"
            // bug). append_single_field appends one Hex64 at the container's aligned
            // tail and SELECTS it, so holding Down keeps growing (the cursor chases
            // the freshly-appended last row). Plain Up-at-top is a silent no-op.
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
            let target = match last_node_id {
                Some(id) => {
                    let tree = self.controller.tree();
                    let idx = tree.index_of_id(id);
                    if idx >= 0 {
                        let p = tree.nodes[idx as usize].parent_id;
                        if p != 0 {
                            p
                        } else {
                            view_root
                        }
                    } else {
                        view_root
                    }
                }
                None => view_root,
            };
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
        self.navigate_node(-1, self.page_step(), cx);
    }
    fn action_nav_page_down(
        &mut self,
        _: &EditorNavPageDown,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_node(1, self.page_step(), cx);
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
                self.scroll.scroll_to_item(
                    i,
                    if to_end {
                        ScrollStrategy::Center
                    } else {
                        ScrollStrategy::Top
                    },
                );
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
        let count = self.controller.last_result().meta.len();
        if count == 0 {
            return;
        }
        // Start from the moving caret if we have one, else the primary selection.
        let start = self
            .caret_line
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
            self.scroll.scroll_to_item(line, ScrollStrategy::Center);
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

    /// Serialize the selected nodes into a portable `rcx-clipboard/v1` blob. Each
    /// node is flattened via [`Node::to_json`] (the same per-node schema the `.rcx`
    /// saver uses), wrapped in a versioned envelope. Returns `None` when nothing
    /// node-like is selected. Pure helper (unit-tested).
    fn serialize_selected_nodes(&self) -> Option<String> {
        let idxs = self.selected_node_indices_ordered();
        if idxs.is_empty() {
            return None;
        }
        let tree = self.controller.tree();
        let nodes: Vec<serde_json::Value> = idxs
            .iter()
            .filter_map(|&i| tree.nodes.get(i))
            .map(|n| n.to_json())
            .collect();
        if nodes.is_empty() {
            return None;
        }
        let envelope = serde_json::json!({
            "format": "rcx-clipboard/v1",
            "nodes": nodes,
        });
        serde_json::to_string(&envelope).ok()
    }

    /// Parse an `rcx-clipboard/v1` blob into the list of [`Node`]s it carries.
    /// Returns `None` for any non-clipboard / malformed text (so a plain text
    /// clipboard does not spuriously paste nodes). Pure helper (unit-tested).
    fn parse_clipboard_nodes(blob: &str) -> Option<Vec<crate::core::Node>> {
        let v: serde_json::Value = serde_json::from_str(blob).ok()?;
        if v.get("format").and_then(|f| f.as_str()) != Some("rcx-clipboard/v1") {
            return None;
        }
        let arr = v.get("nodes")?.as_array()?;
        let nodes: Vec<crate::core::Node> = arr.iter().map(crate::core::Node::from_json).collect();
        (!nodes.is_empty()).then_some(nodes)
    }

    fn action_copy_nodes(&mut self, _: &EditorCopyNodes, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some(blob) = self.serialize_selected_nodes() {
            self.node_clipboard = Some(blob.clone());
            cx.write_to_clipboard(ClipboardItem::new_string(blob));
        }
    }

    fn action_cut_nodes(&mut self, _: &EditorCutNodes, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(blob) = self.serialize_selected_nodes() else {
            return;
        };
        self.node_clipboard = Some(blob.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(blob));
        // Delete the cut nodes (highest idx first so earlier indices stay valid).
        let mut idxs = self.selected_node_indices_ordered();
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        for idx in idxs {
            self.controller.remove_node(idx);
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
        // from another window/instance also pastes.
        let blob = self.node_clipboard.clone().or_else(|| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .map(|t| t.to_string())
        });
        let Some(blob) = blob else {
            return;
        };
        let Some(nodes) = Self::parse_clipboard_nodes(&blob) else {
            return;
        };
        // Insert under the current node's PARENT at its tail (so paste lands next
        // to the cursor), falling back to the view-root struct.
        let (parent_id, base_off) = match self.current_node() {
            Some((_l, lm)) => {
                let tree = self.controller.tree();
                let idx = tree.index_of_id(lm.node_id);
                if idx >= 0 {
                    let n = &tree.nodes[idx as usize];
                    let sz = crate::core::size_for_kind(n.kind).max(0);
                    (n.parent_id, n.offset + sz)
                } else {
                    (self.controller.view_root_id(), -1)
                }
            }
            None => (self.controller.view_root_id(), -1),
        };
        if parent_id == 0 {
            return;
        }
        // Lay the pasted nodes out contiguously from `base_off` (or the parent's
        // tail when base_off < 0), reparented under `parent_id` with fresh ids.
        let mut cursor = if base_off >= 0 {
            base_off
        } else {
            self.container_tail(parent_id)
        };
        for src in &nodes {
            let mut n = src.clone();
            n.id = self.controller.tree_mut().reserve_id();
            n.parent_id = parent_id;
            n.offset = cursor;
            // Children are not carried by the flat v1 blob; drop any dangling
            // ref/children state so the pasted node is self-contained.
            n.ref_id = 0;
            n.collapsed = true;
            let sz = crate::core::size_for_kind(n.kind).max(0);
            cursor += sz.max(1);
            self.controller.push_command(crate::core::Command::Insert {
                node: n,
                off_adjs: Vec::new(),
            });
        }
        self.apply_document(cx);
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
        if let Some((_l, lm)) = self.current_node() {
            self.controller
                .insert_node_above(lm.node_idx as usize, NodeKind::Hex64, "");
            self.apply_document(cx);
        }
    }

    fn action_insert_hex32(
        &mut self,
        _: &EditorInsertHex32,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            self.controller
                .insert_node_above(lm.node_idx as usize, NodeKind::Hex32, "");
            self.apply_document(cx);
        }
    }

    fn action_comment_edit(
        &mut self,
        _: &EditorCommentEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((line, _lm)) = self.current_node() {
            self.begin_inline_edit(line, EditTarget::Comment, window, cx);
        }
    }

    /// Left/Right → cycle same-size type variants on the focused node (item 18).
    /// Reuses the menu's forward/back kind cyclers.
    fn action_cycle_left(&mut self, _: &EditorCycleLeft, _w: &mut Window, cx: &mut Context<Self>) {
        if let Some((_l, lm)) = self.current_node() {
            if crate::core::size_for_kind(lm.node_kind) > 0 {
                self.controller
                    .change_node_kind(lm.node_idx as usize, prev_kind_for(lm.node_kind));
                self.apply_document(cx);
            }
        }
    }
    fn action_cycle_right(
        &mut self,
        _: &EditorCycleRight,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((_l, lm)) = self.current_node() {
            if crate::core::size_for_kind(lm.node_kind) > 0 {
                self.controller
                    .change_node_kind(lm.node_idx as usize, alt_kind_for(lm.node_kind));
                self.apply_document(cx);
            }
        }
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
        // The referenced struct id: a typed pointer/struct carries `ref_id`.
        let ref_id = n.ref_id;
        if ref_id != 0 && tree.index_of_id(ref_id) >= 0 {
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
        for idx in targets {
            // Re-check by re-reading (toggle_collapse may recompose between calls,
            // but indices are stable for collapse toggles in this controller).
            if let Some(n) = self.controller.tree().nodes.get(idx) {
                if crate::core::is_container_kind(n.kind) && n.collapsed != collapsed {
                    self.controller.toggle_collapse(idx);
                }
            }
        }
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

        // Find-match highlight (item 4): paint a translucent accent band over the
        // current navigated match's char range on its line. The match's `[start,
        // end)` are char columns into the line text (the same text the row paints),
        // so they map straight onto the overlay column space.
        if let Some(m) = self.find_match {
            if m.line == idx && m.end > m.start {
                overlays.push((
                    m.start as i32,
                    m.end as i32,
                    with_alpha(palette.accent, 0.35),
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
        let selected = self.is_row_selected(&lm);
        // The hover band is gated by the `hover_effects` view toggle (item 3): when
        // off, the row still tracks the pointer but paints no hover wash.
        let hovered = self.hover_effects && self.hovered_line == Some(idx);

        let editing_here = self
            .editing
            .as_ref()
            .filter(|e| e.line == idx)
            .map(|e| (e.field.clone(), e.col_start, e.col_end));
        // The "active line" (Zed's active-line bg / reclass's highlighted current
        // row): the row currently being edited, even when it is not part of the
        // multi-selection. A selected row already carries the louder accent fill.
        let active_line = editing_here.is_some();

        // Row background precedence (§7): the accent-tinted selection fill wins,
        // then the subtle active-line band, then the hover overlay. Each is a
        // distinct, visible surface against the dark editor paper.
        let bg = if selected {
            Some(palette.selection_bg)
        } else if active_line {
            Some(palette.active_line_bg)
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
            // Hover tracking (the row background tracks the cursor; §7).
            .on_mouse_move(cx.listener(move |this, _e: &MouseMoveEvent, _w, cx| {
                if this.hovered_line != Some(idx) {
                    this.hovered_line = Some(idx);
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
                                let rel = this.relative_offsets;
                                this.relative_offsets = !rel;
                                cx.notify();
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

            // Source-chip + chevron hover affordances (item 5): a pointing-hand
            // cursor + a one-line tooltip ('Data Source' / 'Switch View') over each
            // interactive command-row chip. Transparent hover hitboxes living INSIDE
            // `text_region` (after the address margin), so their `left` is just the
            // per-column offset — the same alignment the inline-edit field uses. Each
            // forwards its click back to the normal row routing so the source/type-
            // selector popup still opens (items 1/2/5).
            for (hover_id, span, tip) in [
                (
                    "rcx-src-hover",
                    crate::compose::command_row_src_span(&text),
                    "Data Source",
                ),
                (
                    "rcx-chevron-hover",
                    crate::compose::command_row_chevron_span(&text),
                    "Switch View",
                ),
            ] {
                if span.valid && span.end > span.start {
                    let left = px(span.start.max(0) as f32 * cell);
                    let width = px(((span.end - span.start).max(1) as f32) * cell);
                    let tip: SharedString = tip.into();
                    // The hover hitbox occludes the row-text element, so forward its
                    // click back into the normal row routing (text-local X inside the
                    // span) — exactly like the address strip — so the source/chevron
                    // click still reaches `on_row_mouse_down` → the popup (items 1/2).
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
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
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
        // Item 28: drag dead-zone. Until the pointer has traveled past ~8px from
        // the press anchor (same row), or the drag has already crossed onto another
        // row, treat the move as a not-yet-drag and do nothing — a small jitter
        // while clicking must not begin extending the node/byte selection. Once the
        // threshold is crossed the `drag_started` latch stays set for the rest of
        // the gesture so subsequent moves extend freely.
        const DRAG_DEAD_ZONE_PX: f32 = 8.0;
        if !self.drag_started {
            let crossed_row = self.drag_anchor_line.map(|a| a != line).unwrap_or(false);
            if !crossed_row && (rel_x - self.drag_anchor_x).abs() < DRAG_DEAD_ZONE_PX {
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
        // a single shift-click here paints the full range.
        let node_id = lm.node_id;
        self.controller.handle_node_click(
            line as i64,
            node_id,
            CtrlMods {
                ctrl: false,
                shift: true,
            },
        );
        self.caret_line = Some(line);
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
        let changed_band = self.hovered_line != Some(line);
        if changed_band {
            self.hovered_line = Some(line);
        }
        // Hover popups are gated by the same toggle as the hover band, and
        // suppressed while editing (the field owns the surface then).
        let want = if self.hover_effects && self.editing.is_none() {
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
        let col = self.metrics.col_containing_x(rel_x);
        if col < vs.start || col >= vs.end {
            // The value column may run to the end of a long line; allow hovering
            // anywhere from the value start to the line end.
            if col < vs.start {
                return None;
            }
        }
        let _ = text;

        let kind = lm.node_kind;
        let is_fp = crate::core::is_func_ptr(kind);
        let is_void_ptr = matches!(kind, NodeKind::Pointer32 | NodeKind::Pointer64)
            && lm.pointer_target_name.is_empty();

        // 1) Function / void pointer → disasm / hex-dump of the TARGET (item 13).
        if is_fp || is_void_ptr {
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

        // 2) Heated changed value with >1 distinct sample → value-history list.
        if lm.heat_level > 0 {
            if let Some(hist) = self.controller.value_history().get(&lm.node_id) {
                if hist.unique_count() > 1 {
                    let mut lines: Vec<String> = Vec::new();
                    hist.for_each_with_time(|v, _t| {
                        if lines.len() < crate::core::value_history::K_CAPACITY {
                            lines.push(v.to_string());
                        }
                    });
                    if lines.len() > 1 {
                        return Some(HoverPopupState {
                            line,
                            pos,
                            kind: HoverPopupKind::ValueHistory { lines },
                        });
                    }
                }
            }
        }
        None
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
        if bytes.is_empty() || bytes.iter().all(|&b| b == 0) {
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

    /// Render the open hover popup (item 13) as a small elevated card anchored near
    /// the cursor, using [`design`] tokens (no ad-hoc hex). Value-history lists the
    /// changed values newest-first; the title/body card shows disasm / hex-dump.
    fn render_hover_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.hover_popup.as_ref()?;
        let palette = EditorPalette::from_theme(cx);
        let card = match &state.kind {
            HoverPopupKind::ValueHistory { lines } => {
                let rows: Vec<AnyElement> = lines
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        div()
                            .text_size(px(design::tokens::font::EDITOR_SIZE))
                            .font_family(design::tokens::font::mono_family())
                            // Newest sample reads in the bright value hue; older
                            // samples fade to the dim text (the heat-history graph).
                            .text_color(if i == 0 { palette.text } else { palette.dim })
                            .child(v.clone())
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
                            .child("Value history"),
                    )
                    .children(rows)
            }
            HoverPopupKind::TitleBody { title, body } => {
                let body_rows: Vec<AnyElement> = body
                    .lines()
                    .map(|l| {
                        div()
                            .text_size(px(design::tokens::font::EDITOR_SIZE))
                            .font_family(design::tokens::font::mono_family())
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
                        card.bg(palette.gutter_bg)
                            .border_1()
                            .border_color(palette.border)
                            .rounded(px(design::tokens::radius::MD))
                            .px(px(design::tokens::space::SM))
                            .py(px(design::tokens::space::XS))
                            .shadow_md(),
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
            }
            return;
        }
        // Only real node rows get the node menu (command/footer/synthetic rows
        // have their own affordances and no node ops).
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return;
        }
        let target = ContextTarget {
            line,
            node_idx: lm.node_idx as usize,
            node_id: lm.node_id,
            kind: lm.node_kind,
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
        if !already_selected {
            self.controller
                .handle_node_click(line as i64, lm.node_id, CtrlMods::NONE);
            let _ = self.controller.take_events();
        }

        self.open_context_menu(target, pos, window, cx);
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
        // Item 7/9: hex-node-only menu entries (Edit Bytes / Split to hexN) + the
        // node's live big-endian state for the checkable toggle (item 13).
        let is_hex_ctx = is_hex_preview(target.kind);
        let big_endian = {
            let idx = self.controller.tree().index_of_id(target.node_id);
            idx >= 0 && self.controller.tree().nodes[idx as usize].big_endian
        };

        let editor_focus = self.focus_handle.clone();
        let menu = gpui_component::menu::PopupMenu::build(window, cx, move |menu, mw, mcx| {
            menu.min_w(px(220.0))
                // Dispatch the menu's actions to the editor's focus context (the
                // `RcxEditor` key context that registers the `Editor*` handlers).
                .action_context(editor_focus.clone())
                .menu_with_icon("New Class", IconName::Frame, Box::new(EditorNewClass))
                .menu_with_icon(
                    "Ptr to New Class",
                    IconName::ArrowRight,
                    Box::new(EditorPtrToNewClass),
                )
                .separator()
                // The "← <curType> ↔ <altType> →" quick type-cycler row: clicking
                // it cycles the node's kind forward (the C++ in-place type stepper).
                .menu_with_icon(
                    cycle_label.clone(),
                    IconName::ChevronDown,
                    Box::new(EditorCycleTypeNext),
                )
                .separator()
                .menu_with_icon("Rename", IconName::SquareTerminal, Box::new(EditorRename))
                .menu_with_icon("Change Type", IconName::Frame, Box::new(EditorChangeType))
                .separator()
                // Item 15: the C++ Insert submenu offers Insert 4 Above (Hex32,
                // Shift+Ins) / Insert 8 Above (Hex64, Ins) — the keyboard already
                // maps Insert/Shift+Insert to those — plus Insert Below.
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "Insert 8 Above (Hex64)",
                        IconName::Plus,
                        Box::new(EditorInsertHex64),
                    )
                    .menu_with_icon(
                        "Insert 4 Above (Hex32)",
                        IconName::Plus,
                        Box::new(EditorInsertHex32),
                    )
                    .separator()
                    .menu_with_icon("Insert Below", IconName::Plus, Box::new(EditorInsertBelow))
                    .menu_with_icon(
                        "Insert Above",
                        IconName::Plus,
                        Box::new(EditorInsertAbove),
                    )
                })
                // Item 9: the C++ quick-convert set — per-size int/uint/float,
                // ptr/fnptr, Split to hexN, Convert to Hex, plus the New-Class ptr.
                .submenu("Convert", mw, mcx, move |sub, _w, _cx| {
                    let mut sub = sub
                        .menu_with_icon(
                            "To Pointer (New Class)",
                            IconName::ArrowRight,
                            Box::new(EditorConvertPtr),
                        )
                        .separator()
                        .menu_with_icon("uint", IconName::Frame, Box::new(EditorConvUInt))
                        .menu_with_icon("int", IconName::Frame, Box::new(EditorConvInt))
                        .menu_with_icon("float", IconName::Frame, Box::new(EditorConvFloat))
                        .separator()
                        .menu_with_icon("ptr", IconName::ArrowRight, Box::new(EditorConvPtr64))
                        .menu_with_icon(
                            "fnptr",
                            IconName::SquareTerminal,
                            Box::new(EditorConvFnPtr64),
                        )
                        .separator()
                        .menu_with_icon("Convert to Hex", IconName::Frame, Box::new(EditorConvHex));
                    if is_hex_ctx {
                        sub = sub.menu_with_icon(
                            "Split to hexN + hexN",
                            IconName::Frame,
                            Box::new(EditorConvSplitHex),
                        );
                    }
                    sub
                })
                .menu_with_check("Big endian", big_endian, Box::new(EditorToggleBigEndian))
                .when(is_hex_ctx, |menu| {
                    // Item 7: in-place hex / ASCII overwrite editor entry points,
                    // shown for hex nodes (the value is a fixed-length byte string).
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
                .submenu("Static", mw, mcx, |sub, _w, _cx| {
                    // Static-address submenu (graceful stub — no logic op yet).
                    sub.label("(no static address)")
                })
                .separator()
                .menu_with_icon("Duplicate", IconName::Copy, Box::new(EditorDuplicate))
                .menu_with_icon("Delete", IconName::Delete, Box::new(EditorDelete))
                .separator()
                .submenu("Fold", mw, mcx, move |sub, _w, _cx| {
                    sub.menu_with_icon_and_disabled(
                        "Toggle Fold",
                        IconName::ChevronRight,
                        Box::new(EditorFold),
                        !is_container,
                    )
                })
                // Item 17: the Copy submenu — Copy Address / Offset / Line / All as
                // Text (was a dead "(copy)" label).
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Copy Address", IconName::Copy, Box::new(EditorCopyAddress))
                        .menu_with_icon("Copy Offset", IconName::Copy, Box::new(EditorCopyOffset))
                        .menu_with_icon("Copy Line", IconName::Copy, Box::new(EditorCopyLine))
                        .menu_with_icon(
                            "Copy All as Text",
                            IconName::Copy,
                            Box::new(EditorCopyAllText),
                        )
                })
                .submenu("Tracking", mw, mcx, |sub, _w, _cx| sub.label("(tracking)"))
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
                });
            }
        }
        // Fall back to the primary-selected line's node.
        let line = self.first_selected_line()?;
        let lm = self.line_meta(line)?;
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            return None;
        }
        Some(ContextTarget {
            line,
            node_idx: lm.node_idx as usize,
            node_id: lm.node_id,
            kind: lm.node_kind,
        })
    }

    // ── Context-menu / accelerator action handlers ──

    fn action_new_class(&mut self, _: &EditorNewClass, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        // "New Class": insert a new struct member after the target (or at the
        // parent tail when no target). Uses the parent + offset of the target.
        if let Some(t) = self.action_target() {
            let (parent_id, offset) = self.insert_anchor(t.node_idx);
            self.controller
                .insert_node(parent_id, offset, NodeKind::Struct, "NewClass");
        } else {
            self.controller.insert_node(
                self.controller.view_root_id(),
                -1,
                NodeKind::Struct,
                "NewClass",
            );
        }
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
        if !self.relative_offsets {
            self.relative_offsets = true;
            cx.notify();
        }
    }

    /// Item 27: switch the offset margin to absolute-address mode.
    fn action_offsets_absolute(
        &mut self,
        _: &EditorOffsetsAbsolute,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);
        if self.relative_offsets {
            self.relative_offsets = false;
            cx.notify();
        }
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
            if let Some(lm) = self.line_meta(t.line).cloned() {
                let base = self.controller.last_result().layout.base_address;
                let off = lm.offset_addr.saturating_sub(base);
                cx.write_to_clipboard(ClipboardItem::new_string(format!("0x{:X}", off)));
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
        if let Some(editing) = self.editing.as_ref() {
            let field = editing.field.clone();
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
        if let Some(t) = self.action_target() {
            self.controller.duplicate_node(t.node_idx);
            self.apply_document(cx);
        }
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
        idxs.sort_unstable_by(|a, b| b.cmp(a));
        idxs.dedup();
        for idx in idxs {
            self.controller.remove_node(idx);
        }
        self.controller.clear_selection();
        self.context_target = None;
        self.apply_document(cx);
    }

    fn action_fold(&mut self, _: &EditorFold, _w: &mut Window, cx: &mut Context<Self>) {
        self.close_context_menu(cx);
        if let Some(t) = self.action_target() {
            self.controller.toggle_collapse(t.node_idx);
            self.after_mutation(cx);
        }
    }

    fn action_copy_c_struct(
        &mut self,
        _: &EditorCopyCStruct,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Graceful stub: the C-struct serializer is a later workflow; close cleanly.
        self.close_context_menu(cx);
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
    fn full_type_entries(&self, exclude_id: u64) -> Vec<crate::ui::typeselectorpopup::TypeEntry> {
        use crate::ui::typeselectorpopup::{default_type_entries, TypeEntry};
        let mut entries = default_type_entries();
        let tree = self.controller.tree();
        let mut composites: Vec<TypeEntry> = Vec::new();
        for n in tree.nodes.iter() {
            // Named composite declarations (a struct with a type name), excluding
            // the self-reference target.
            if n.kind == NodeKind::Struct && !n.struct_type_name.is_empty() && n.id != exclude_id {
                let size = crate::core::size_for_kind(n.kind).max(0);
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
        let entries = self.full_type_entries(target.node_id);
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
        };
        // Root mode lists every declared composite so the user can re-root onto a
        // different struct (do NOT exclude the current root — it may be re-picked).
        let entries = self.full_type_entries(0);
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
        use crate::ui::typeselectorpopup::{TypeSelectorEvent, TypeSelectorPopup};
        let popup = cx.new(|cx| {
            let mut p = TypeSelectorPopup::new(entries, window, cx);
            p.set_mode(mode, cx);
            p
        });
        let focus = popup.read(cx).focus_handle(cx);
        let node_idx = target.node_idx;
        let node_id = target.node_id;
        self._type_selector_sub = Some(cx.subscribe_in(
            &popup,
            window,
            move |this, _p, ev: &TypeSelectorEvent, window, cx| match ev {
                TypeSelectorEvent::Chosen { kind, modifier } => {
                    window.close_dialog(cx);
                    this._type_selector_sub = None;
                    this.apply_type_choice(node_idx, node_id, *kind, *modifier, cx);
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

    /// Apply a TypeSelector choice: set the base kind, then the chosen modifier
    /// (the existing `typeselectorpopup::Modifier`: pointer/double-pointer/array)
    /// via the controller ops, then recompose. Resolves the node by id first so a
    /// kind change that shifts indices does not desync the modifier step.
    fn apply_type_choice(
        &mut self,
        node_idx: usize,
        node_id: u64,
        kind: NodeKind,
        modifier: Option<crate::ui::typeselectorpopup::Modifier>,
        cx: &mut Context<Self>,
    ) {
        use crate::ui::typeselectorpopup::Modifier;
        // Base kind first.
        self.controller.change_node_kind(node_idx, kind);
        // Then the modifier (pointer / double-pointer / array) via the controller
        // ops, re-resolving the node id (change_node_kind may have shifted indices).
        match modifier {
            // `*` single pointer to a fresh class.
            Some(Modifier::Pointer) => {
                self.controller.convert_to_typed_pointer(node_id);
            }
            // `**` double pointer (item 7): the C++ keeps a distinct pointer DEPTH.
            // We have no dedicated double-pointer op, so make the node a typed
            // pointer to a fresh class AND retarget that class's first field as a
            // pointer too — the on-disk `**` shape (a pointer whose pointee is a
            // pointer), rather than silently collapsing `**` to a single `*`.
            Some(Modifier::PointerPointer) => {
                self.controller.convert_to_typed_pointer(node_id);
                // Resolve the new pointee struct's first child and make it a pointer
                // as well, giving the second level of indirection.
                let inner_kind = if self.controller.tree().pointer_size >= 8 {
                    NodeKind::Pointer64
                } else {
                    NodeKind::Pointer32
                };
                if let Some(first_child_idx) = self.first_pointee_field_idx(node_id) {
                    self.controller
                        .convert_to_typed_pointer(self.controller.tree().nodes[first_child_idx].id);
                    let _ = inner_kind;
                }
            }
            // `[N]` array with the chosen element COUNT (item 7): change to Array,
            // then push a `ChangeArrayMeta` carrying the element kind + the count so
            // the `[N]` is not dropped.
            Some(Modifier::Array(count)) => {
                let idx = self.controller.tree().index_of_id(node_id);
                if idx >= 0 {
                    self.controller
                        .change_node_kind(idx as usize, NodeKind::Array);
                    self.set_array_meta(node_id, kind, count.max(1));
                }
            }
            Some(Modifier::None) | None => {}
        }
        self.apply_document(cx);
    }

    /// The tree index of the FIRST child field of the struct a typed pointer
    /// (`node_id`) references (its `ref_id`'s first child), if any — used to apply
    /// the second level of a `**` double pointer (item 7).
    fn first_pointee_field_idx(&self, node_id: u64) -> Option<usize> {
        let tree = self.controller.tree();
        let pi = tree.index_of_id(node_id);
        if pi < 0 {
            return None;
        }
        let ref_id = tree.nodes[pi as usize].ref_id;
        if ref_id == 0 {
            return None;
        }
        tree.children_of(ref_id).first().copied()
    }

    /// Push an undoable `ChangeArrayMeta` setting the array element kind + length
    /// (item 7). Reads the node's current array meta for the undo half. No-op when
    /// the node id no longer resolves.
    fn set_array_meta(&mut self, node_id: u64, element_kind: NodeKind, count: i32) {
        let idx = self.controller.tree().index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let n = &self.controller.tree().nodes[idx as usize];
        let old_element_kind = n.element_kind;
        let old_array_len = n.array_len;
        if old_element_kind == element_kind && old_array_len == count {
            return;
        }
        self.controller
            .push_command(crate::core::Command::ChangeArrayMeta {
                node_id,
                old_element_kind,
                new_element_kind: element_kind,
                old_array_len,
                new_array_len: count,
            });
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
        const PROBE: &str = "0000000000";
        let editor_font_size = px(design::tokens::font::EDITOR_SIZE);
        let line_height = design::tokens::font::EDITOR_SIZE * EDITOR_LINE_HEIGHT;
        let cell_width = {
            let run = TextRun {
                len: PROBE.len(),
                font: gpui::font(design::tokens::font::mono_family()),
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
            .font_family(design::tokens::font::mono_family())
            .text_size(px(design::tokens::font::EDITOR_SIZE))
            .line_height(px(design::tokens::font::EDITOR_SIZE * EDITOR_LINE_HEIGHT))
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
            .on_action(cx.listener(Self::action_conv_fnptr64))
            .on_action(cx.listener(Self::action_conv_hex))
            .on_action(cx.listener(Self::action_conv_split_hex))
            .on_action(cx.listener(Self::action_edit_bytes_hex))
            .on_action(cx.listener(Self::action_edit_bytes_ascii))
            .on_action(cx.listener(Self::action_copy_offset))
            .on_action(cx.listener(Self::action_copy_line))
            .on_action(cx.listener(Self::action_copy_all_text))
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
            .child(
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
            HoverPopupKind::ValueHistory { lines: la },
            HoverPopupKind::ValueHistory { lines: lb },
        ) => la == lb,
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
        // The format rows: each is (example, dim explanation). The example reads in
        // the editor mono number hue, the explanation in muted UI text.
        let rows: Vec<(String, &'static str)> = vec![
            (format!("0x{:X}", self.base_address), "hex address"),
            (module.clone(), "module base"),
            (format!("{module} + 0x1A0"), "module + offset"),
            (format!("[{module} + 0x58]"), "follow pointer"),
            ("ntdll!Symbol".to_string(), "PDB symbol"),
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
    fn clipboard_blob_round_trips_node_fields() {
        // A serialized `rcx-clipboard/v1` blob parses back into the same node
        // fields (kind/name/offset/comment) — the copy→paste fidelity contract.
        use crate::core::{Node, NodeKind};
        let n = Node {
            kind: NodeKind::Int32,
            name: "health".into(),
            offset: 8,
            comment: "hp".into(),
            ..Node::default()
        };
        let envelope = serde_json::json!({
            "format": "rcx-clipboard/v1",
            "nodes": [n.to_json()],
        });
        let blob = serde_json::to_string(&envelope).unwrap();
        let parsed = super::RcxEditor::parse_clipboard_nodes(&blob).expect("parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, NodeKind::Int32);
        assert_eq!(parsed[0].name, "health");
        assert_eq!(parsed[0].offset, 8);
        assert_eq!(parsed[0].comment, "hp");
    }

    #[test]
    fn clipboard_rejects_non_rcx_text() {
        // A plain-text clipboard (or wrong format tag) must NOT parse as nodes, so
        // a generic copy does not spuriously paste structure.
        assert!(super::RcxEditor::parse_clipboard_nodes("just some text").is_none());
        assert!(super::RcxEditor::parse_clipboard_nodes("{\"format\":\"other\"}").is_none());
        assert!(
            super::RcxEditor::parse_clipboard_nodes("{\"format\":\"rcx-clipboard/v1\"}").is_none(),
            "missing nodes array"
        );
    }

    // ── Hover popup equality (item 13) ──

    #[test]
    fn hover_kind_eq_distinguishes_content_and_variant() {
        use super::{hover_kind_eq, HoverPopupKind};
        let a = HoverPopupKind::ValueHistory {
            lines: vec!["1".into(), "2".into()],
        };
        let a2 = HoverPopupKind::ValueHistory {
            lines: vec!["1".into(), "2".into()],
        };
        let b = HoverPopupKind::ValueHistory {
            lines: vec!["1".into(), "3".into()],
        };
        let t = HoverPopupKind::TitleBody {
            title: "Disassembly".into(),
            body: "nop".into(),
        };
        assert!(hover_kind_eq(&a, &a2), "same content compares equal");
        assert!(!hover_kind_eq(&a, &b), "different lines differ");
        assert!(!hover_kind_eq(&a, &t), "different variants differ");
    }
}
