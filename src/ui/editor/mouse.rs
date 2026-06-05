//! The editor's left-mouse row-interaction cluster — on_row_mouse_down + the
//! footer/byte-address hit helpers + the dispatch_row_* click/double/drag routers
//! — extracted from the oversized editor/mod.rs into a second `impl RcxEditor`. A
//! child module of `editor`, it keeps full access to RcxEditor's private state.

use super::*;
use gpui::*;

impl super::RcxEditor {
    /// Handle a left mouse press on row `line` at pixel-relative X `rel_x`.
    pub(super) fn on_row_mouse_down(
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

        // The array-element → parent-header redirect (below, after the
        // fold/byte/footer early-returns) rebinds `line`/`lm`/`hit` to the parent
        // array header so every downstream branch targets the array node. `text`
        // and the column widths are only consumed by `hit_test_row`/`on_footer_click`
        // above the redirect, so they stay immutable.
        let mut line = line;
        let mut lm = lm;
        let mut hit = hit;

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

        // Array element → parent array-header redirect (C++ `hitTestTarget`,
        // editor.cpp:2480-2492): a click on an array *element*'s Type or Name token
        // is not editable on the element itself — it must open the element-type
        // picker on the PARENT array header. Rewrite the hit to `ArrayElementType`
        // and re-point `line`/`lm`/`hit` at the array header (the nearest preceding
        // line with `is_array_header` and the SAME `node_idx`) so every downstream
        // branch (selection, Ctrl+click open-in-tab, the type-selector picker)
        // targets the array node, not the synthetic element row. If no parent header
        // is found, the click resolves to nothing (the C++ `return false`).
        if lm.is_array_element
            && matches!(hit.target, Some(EditTarget::Type) | Some(EditTarget::Name))
        {
            if let Some(hdr_line) = self.parent_array_header_line(line, lm.node_idx) {
                let Some(hdr_lm) = self.line_meta(hdr_line).cloned() else {
                    return;
                };
                line = hdr_line;
                lm = hdr_lm;
                // Keep the originally-hit column; only the resolved TARGET and line
                // change (matches the C++, which leaves `outCol` from the element
                // click and only overrides `outTarget`/`outLine`). The picker
                // positions itself from the header's type span regardless.
                hit = hit_test::HitInfo {
                    target: Some(EditTarget::ArrayElementType),
                    ..hit
                };
            } else {
                // No parent header (should not happen for a real element row):
                // the click resolves to no edit target.
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
    pub(super) fn flush_pending_click(&mut self, cx: &mut Context<Self>) {
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
}
