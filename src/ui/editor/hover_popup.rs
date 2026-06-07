//! The editor's hover popups — the value-history / disasm / struct-preview cards
//! over a row's value column (item 13) — with their state types and the
//! hover_kind_eq comparator, extracted from editor/mod.rs into a second
//! impl RcxEditor. A child module of editor, so it keeps access to RcxEditor's
//! private fields and methods.

use super::*;
use gpui::*;

/// The kind of hover popup shown over a row's value column (item 13). The C++
/// `applyHoverCursor` opens one of three popups depending on the node:
/// value-history (heated changed values), disasm/hex-dump (func/void pointers),
/// or struct-preview (typed pointer).
#[derive(Clone, Debug)]
pub(super) enum HoverPopupKind {
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
pub(super) struct HoverPopupState {
    pub(super) line: usize,
    pub(super) pos: Point<Pixels>,
    pub(super) kind: HoverPopupKind,
}

/// Structural equality for two hover popup kinds (item 13) — used to avoid
/// re-notifying when the cursor moves within the same popup target.
pub(super) fn hover_kind_eq(a: &HoverPopupKind, b: &HoverPopupKind) -> bool {
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

impl super::RcxEditor {
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
        // No hover tracking while a context menu is open. The C++ context menus run
        // a nested modal loop (QMenu::exec), so the editor viewport sees no
        // mouse-move and hover dwell never fires. Here the menu is a non-occluding
        // anchored overlay, so without this guard a move over any row the menu does
        // not physically cover keeps re-spawning hover cards / tooltips behind it —
        // the leak the menu-open state must suppress.
        if self.context_menu.is_some() {
            return;
        }
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
    pub(super) fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Format a value-history timestamp as a relative age ('now' / 'Ns ago' /
    /// 'Nm ago' / 'Nh ago') — the C++ `ValueHistoryPopup::populate` time string
    /// (editor.cpp:252). A non-positive timestamp (untracked) yields an empty label.
    pub(super) fn relative_age(now: i64, then: i64) -> String {
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
            None,
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
    pub(super) fn render_hover_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Belt-and-suspenders with the `dispatch_row_hover` guard: never paint a
        // hover card while a context menu is up, regardless of how `hover_popup`
        // came to be set.
        if self.context_menu.is_some() {
            return None;
        }
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
}
