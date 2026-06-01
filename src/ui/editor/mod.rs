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
use gpui_component::{ActiveTheme, Icon, IconName, WindowExt as _};

use crate::compose::EditTarget;
use crate::controller::{Modifiers as CtrlMods, RcxController, RcxDocument};
use crate::core::linemeta::K_COMMAND_ROW_ID;
use crate::core::{is_hex_preview, ComposeResult, LineKind, LineMeta, NodeKind};
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
        EditorToggleBigEndian,
        EditorDuplicate,
        EditorDelete,
        EditorFold,
        EditorCopyCStruct,
    ]
);

/// Default monospace cell width as a fraction of the row height (overwritten with
/// the measured glyph advance once the first frame shapes a line). A 0.6 ratio is
/// the typical width:height of a monospace cell and keeps hit-testing sane before
/// the first measurement.
const DEFAULT_CELL_RATIO: f32 = 0.6;

/// Editor line-height multiple (× font size). The shared token is 1.4; the grid
/// reads more comfortably (and matches the reclass screenshots' generous leading)
/// at 1.5, so the editor surface uses a slightly looser leading than dense UI
/// lists. Both the painted rows and the hit-test metrics derive from this.
const EDITOR_LINE_HEIGHT: f32 = 1.5;

/// Width (in monospace cells) of the node-kind icon gutter rendered between the
/// address margin and the row text. It sits OUTSIDE the composed text columns, so
/// it never disturbs hit-testing/fold/inline-edit column math (those resolve
/// against the row-text element's own left edge); the inline-edit overlay simply
/// adds this offset alongside the address-margin offset.
const ICON_CELLS: f32 = 2.0;

/// Width (px) of the per-row left accent border (`border_l_2`). The border lives
/// inside the row's border-box, so an `absolute`-positioned child's `left(0)` is
/// the padding-box left — one border-width *short* of where the RowElement paints
/// text column 0. The inline-edit overlay adds this so its seeded text lands on
/// the resolved column instead of sliding one column left onto the type token.
const BORDER_L_PX: f32 = 2.0;

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
    ]
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
    /// Measured monospace cell metrics (updated each frame from the font).
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

        let node_id = lm.node_id;
        let already_selected = node_id != 0
            && node_id != K_COMMAND_ROW_ID
            && self
                .controller
                .selected_ids()
                .iter()
                .any(|&id| crate::controller::strip_sel_pub(id) == node_id);

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
        let start_byte = geometry::byte_for_col(&text, span.start);
        let end_byte = geometry::byte_for_col(&text, span.end);
        let initial = text
            .get(start_byte..end_byte)
            .unwrap_or("")
            .trim()
            .to_string();

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
        let handle = field.read(cx).field_focus_handle();
        window.focus(&handle, cx);
        // Arm the caret blink with a solid caret so the field shows an immediate,
        // continuously-visible cursor the moment editing begins (BUG 2).
        field.update(cx, |f, cx| f.arm_caret(cx));
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
            // Comment / array-count / pointer-target / static-expr edits have
            // controller paths that the later edit-wiring workflow connects; for
            // now recompose so the overlay clears without corrupting the tree.
            _ => {}
        }
        self.after_mutation(cx);
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
                let root_id = self.controller.view_root_id();
                let idx = self.controller.tree().index_of_id(root_id);
                if idx >= 0 {
                    self.controller
                        .rename_node(idx as usize, commit.text.trim());
                }
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
        // `backward` (Shift+Tab) is accepted; full reverse-order cycling is a
        // later refinement — for now both directions advance the forward cycle
        // from `m_lastTabTarget`, matching the common Tab path (§10).
        let _ = backward;
        if let Some(target) = tab_cycle::next_tab_target(&lm, self.last_tab_target) {
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
        cx.notify();
    }

    // ── Action handlers (editor-surface.md §10) ──

    fn action_tab(&mut self, _: &EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_to_next_field(false, window, cx);
    }
    fn action_tab_prev(&mut self, _: &EditorTabPrev, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_to_next_field(true, window, cx);
    }
    fn action_escape(&mut self, _: &EditorEscape, window: &mut Window, cx: &mut Context<Self>) {
        // Two-stage Esc (§10): drop byte selection first, else clear node
        // selection. An active edit is cancelled by the field's own Esc binding.
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
            // Preserve compose's per-row decision of whether this row carries an
            // offset (footers / separators are blank there).
            let margin_text = if lm.offset_text.trim().is_empty() {
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
                    .child(SharedString::from(margin_text)),
            );
        }

        // Node-kind icon gutter — a small Zed-outline-style kind glyph (struct ◆ /
        // pointer → / array ▦ / fnptr ƒ / hex # / value •) prefixing each node row,
        // distinguishing node types at a glance like the reclass project tree
        // (PIC2's per-node icons) + Zed outline. Leaf rows paint a loud kind-tinted
        // glyph; expandable container rows paint the SAME marker in the quiet
        // `KindIconDim` role so the gutter is never empty, while their crisp fold
        // disclosure triangle (painted in the row text) stays the interactive fold
        // affordance. Reserved on EVERY row (a fixed `ICON_CELLS` width) so the row
        // text starts at the same column whether or not a glyph is present — the
        // icon lives OUTSIDE the composed text columns, so hit-testing/fold math is
        // untouched.
        //
        // Round-3: the round-2 text-glyph markers (◆ ▦ → # • ƒ) + the fold caret
        // are now crisp SVGs (design::icon_* / `IconName`, registered by the
        // Assets stage). Fold-head rows paint the disclosure chevron here
        // (`ChevronRight` collapsed / `ChevronDown` expanded) — the row text's own
        // baked caret is rendered transparent via `palette.fold_chevron` so only
        // this SVG shows, while fold-col hit-testing (column-based) is untouched.
        // Leaf node rows paint their per-kind icon. The icon size is clamped to the
        // gutter cell box so the column math is unchanged.
        {
            let icon_px = (self.metrics.line_height * 0.62).clamp(10.0, 18.0);
            let mut gutter = div()
                .flex_shrink_0()
                .w(px(ICON_CELLS * self.metrics.cell_width))
                .h(px(self.metrics.line_height))
                .flex()
                .flex_row()
                .items_center()
                .justify_center();
            if lm.fold_head {
                // The crisp disclosure chevron (the real fold affordance).
                let name = if lm.fold_collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                };
                gutter = gutter.child(
                    Icon::new(name)
                        .size(px(icon_px))
                        .text_color(palette.fold_chevron_icon),
                );
            } else if let Some(kg) = geometry::kind_glyph(&lm) {
                // The per-kind SVG marker, tinted by the glyph's semantic role so a
                // theme switch retints it (same role→color mapping as before).
                gutter = gutter.child(
                    Icon::new(kind_icon_name(lm.node_kind))
                        .size(px(icon_px))
                        .text_color(palette.role_color(kg.role)),
                );
            }
            row = row.child(gutter);
        }

        // The text element (static) — always painted as the base layer; it owns
        // the hitbox + row-local click routing back into the view.
        let row_paint = self.build_row_paint(idx, palette);
        row = row.child(RowElement {
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
                let hex_digits = self
                    .controller
                    .last_result()
                    .layout
                    .offset_hex_digits
                    .max(0) as f32;
                let margin = if hex_digits > 0.0 {
                    hex_digits + 2.0
                } else {
                    0.0
                };
                let cell = self.metrics.cell_width;
                let left = px((margin + addr.start.max(0) as f32) * cell);
                let width = px(((addr.end - addr.start).max(1) as f32) * cell);
                let base_address = self.controller.last_result().layout.base_address;
                let module: SharedString = self.controller.document().provider.name().into();
                // Forward a left mouse-down on the overlay back into the normal row
                // click routing: the overlay's hitbox occludes the row-text hitbox,
                // so without this the base-address inline edit (the `BaseAddress`
                // hit-test target) would stop working under the tooltip strip.
                //
                // BUG 1: `on_row_mouse_down` → `hit_test_row` resolves the column
                // from a **row-text-local** X (column 0 = the first char of the
                // composed command-row string, AFTER the address margin + icon
                // gutter). `left` above is in the *row's* coordinate space (it
                // includes the `margin + ICON_CELLS` lead-in), so forwarding it
                // verbatim shifted the hit-test column right by the gutter width and
                // the `BaseAddress` span never matched — clicking the address did
                // nothing. Forward a **text-local** X (the address span's first
                // column + half a cell) so the hit test lands inside the address
                // span and the base-address edit actually begins.
                let addr_click_x = (addr.start.max(0) as f32 + 0.5) * cell;
                row = row.child(
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
        }

        // Inline-edit overlay positioned at the edited column — offset by the
        // address-margin width so it lands over the field, not the margin.
        if let Some((field, col_start, col_end)) = editing_here {
            let hex_digits = self
                .controller
                .last_result()
                .layout
                .offset_hex_digits
                .max(0) as f32;
            let margin = if hex_digits > 0.0 {
                hex_digits + 2.0
            } else {
                0.0
            };
            let cell = self.metrics.cell_width;
            // The overlay is positioned in the row's own coordinate space, so it
            // must clear BOTH the address margin and the kind-icon gutter (both
            // precede the row-text element) before the per-column offset. The
            // RowElement's text column 0 sits past the 2px left accent border too
            // (the border is inside the row's border-box, so an `absolute` child's
            // `left(0)` is the *padding-box* left — one border-width short of the
            // text). Add `BORDER_L_PX` so the seeded edit text lands exactly on the
            // NAME column and does not slide one column left onto the type token
            // (item 3: the earlier overlay covered only the address margin, not the
            // per-target column nor the accent border).
            let left = px(BORDER_L_PX + (margin + col_start.max(0) as f32) * cell);
            // The opaque band spans the edited column `[col_start, col_end)` (a
            // generous minimum so short seeds still get a visible field box).
            let editing_width = ((col_end - col_start).max(0) as f32).max(6.0);
            // The inline field paints over the static row text. Give it a FULLY
            // OPAQUE editor-paper band (not the semi-transparent active-line fill,
            // which let the column's static glyphs — the type token / pre-edit name —
            // bleed through behind the seeded text and read as garbled overlap
            // "hexChex64"/"int64_teateTime"). A 1px accent ring + slight rounding make
            // it read as a Zed inline input.
            row = row.child(
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
                .submenu("Insert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon("Insert Below", IconName::Plus, Box::new(EditorInsertBelow))
                        .menu_with_icon("Insert Above", IconName::Plus, Box::new(EditorInsertAbove))
                })
                .submenu("Convert", mw, mcx, |sub, _w, _cx| {
                    sub.menu_with_icon(
                        "To Pointer (New Class)",
                        IconName::ArrowRight,
                        Box::new(EditorConvertPtr),
                    )
                })
                .menu_with_check("Big endian", false, Box::new(EditorToggleBigEndian))
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
                .submenu("Copy", mw, mcx, |sub, _w, _cx| {
                    // Copy variants beyond C-struct are graceful stubs.
                    sub.label("(copy)")
                })
                .submenu("Tracking", mw, mcx, |sub, _w, _cx| sub.label("(tracking)"))
                .menu_with_icon(
                    "Copy as C Struct",
                    IconName::SquareTerminal,
                    Box::new(EditorCopyCStruct),
                )
        });

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
        // Graceful stub: no controller op for endianness in this stage; close
        // cleanly (the item renders + closes, the C++ contract for un-wired items).
        self.close_context_menu(cx);
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
        if let Some(t) = self.action_target() {
            self.controller.remove_node(t.node_idx);
            self.context_target = None;
            self.apply_document(cx);
        }
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
        use crate::ui::typeselectorpopup::{TypeSelectorEvent, TypeSelectorPopup};
        let popup = TypeSelectorPopup::view(target.kind, window, cx);
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
        // Then the modifier (pointer/array) via the existing controller ops,
        // re-resolving the node id (change_node_kind may have shifted indices).
        match modifier {
            Some(Modifier::Pointer) | Some(Modifier::PointerPointer) => {
                self.controller.convert_to_typed_pointer(node_id);
            }
            Some(Modifier::Array(count)) => {
                let idx = self.controller.tree().index_of_id(node_id);
                if idx >= 0 {
                    self.controller
                        .change_node_kind(idx as usize, NodeKind::Array);
                    let _ = count; // element count is applied via the array header edit path
                }
            }
            Some(Modifier::None) | None => {}
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

/// The "alternate" kind for the quick type-cycler (`← cur ↔ alt →`) and the
/// forward `T`-less cycle: steps to the next kind in the [`NodeKind`] table,
/// wrapping. This is the in-place type stepper the C++ menu's `← type ↔ type →`
/// row drives (a quick toggle between adjacent primitive types).
fn alt_kind_for(kind: NodeKind) -> NodeKind {
    let i = kind as u8 as usize;
    let n = crate::core::K_KIND_META.len();
    crate::core::K_KIND_META[(i + 1) % n].kind
}

/// The previous kind in the table (the `←` half of the cycler), wrapping.
fn prev_kind_for(kind: NodeKind) -> NodeKind {
    let i = kind as u8 as usize;
    let n = crate::core::K_KIND_META.len();
    crate::core::K_KIND_META[(i + n - 1) % n].kind
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

/// The SVG icon ([`IconName`]) for a node-kind gutter marker — the crisp
/// replacement for the round-2 text glyphs (`geometry::kind_glyph`'s ◆ ▦ → # • ƒ).
/// Driven by node kind so struct/array/pointer/fnptr/hex/value each read at a
/// glance, matching the reclass per-node tree icons (PIC2) + a Zed outline.
fn kind_icon_name(kind: NodeKind) -> IconName {
    use NodeKind::*;
    match kind {
        Struct => IconName::Frame,
        Array => IconName::LayoutDashboard,
        Pointer32 | Pointer64 => IconName::ArrowRight,
        FuncPtr32 | FuncPtr64 => IconName::SquareTerminal,
        Hex8 | Hex16 | Hex32 | Hex64 | Hex128 => IconName::MemoryStick,
        _ => IconName::Dash,
    }
}

impl Focusable for RcxEditor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RcxEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Measure the monospace cell once per frame from the active font so
        // hit-testing math matches the painted glyph advance.
        let line_height = f32::from(window.line_height());
        // The advance of '0' in the editor font (monospace ⇒ uniform).
        let cell_width = {
            let style = window.text_style();
            let font = style.font();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let run = TextRun {
                len: 1,
                font,
                color: cx.theme().foreground,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window.text_system().shape_line(
                "0".into(),
                font_size,
                std::slice::from_ref(&run),
                None,
            );
            let w = f32::from(shaped.width());
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
            .size_full()
            .bg(palette.paper)
            .text_color(palette.text)
            // A real monospace at the comfortable Zed editor size + generous
            // leading (the prior build was cramped). The measured `line_height`
            // above derives from exactly these so hit-testing stays aligned.
            .font_family(design::tokens::font::MONO_FAMILY)
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
            .on_action(cx.listener(Self::action_toggle_big_endian))
            .on_action(cx.listener(Self::action_duplicate))
            .on_action(cx.listener(Self::action_delete))
            .on_action(cx.listener(Self::action_fold))
            .on_action(cx.listener(Self::action_copy_c_struct))
            .on_mouse_down_out(cx.listener(|this, _e: &MouseDownEvent, window, cx| {
                // Clicking outside the editor commits an active edit.
                if this.editing.is_some() {
                    this.commit_active_edit(window, cx);
                }
            }))
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
    }
}

/// Apply an alpha to an `Hsla` (heat/byte-sel overlays are translucent fills).
fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
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
                            .font_family(tokens::font::MONO_FAMILY)
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
}
