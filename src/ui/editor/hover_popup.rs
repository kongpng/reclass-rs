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
    /// Each entry carries the value text + the RAW epoch-msec it was recorded at
    /// (0 = untracked); the render derives the relative-age label, the signed
    /// delta column and the recency tiers from the timestamp (upstream bf49a64's
    /// `buildValueHistoryBody` polish). `total_count` is the uncapped number of
    /// values ever recorded for this node (`ValueHistory::count`) — the visible
    /// list is capped at the ring window, so a larger total drives the
    /// ring-overflow footer. `node_idx`/`sub_line`/`resolved_addr` + `set_buttons`
    /// drive the edit-time 'Set' buttons (item 68): when `set_buttons` is true the
    /// popup is shown during an active edit and each row gets a Set button that
    /// writes the value back into the node.
    ValueHistory {
        entries: Vec<(String, i64)>,
        total_count: i64,
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
            // Compare only the VALUE column (ignore the raw msec timestamps, which
            // are constant per entry but irrelevant to identity) + the Set-button
            // mode — the C++ `vals == m_values` test. This avoids constant popup
            // re-creation as the elapsed-time labels advance each frame.
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

// ── Value-history popup pure helpers (bf49a64 `buildValueHistoryBody`) ──
//
// These are GPUI-free so they can be unit-tested headlessly. The render arm
// (`render_hover_popup`) composes them into the GPUI rows.

/// Format an elapsed duration as a compact single token — "12s" / "5m" / "3h" /
/// "2d" / "1w" — tiered by magnitude, with NO "ago" suffix (the C++
/// `formatElapsed` lambda). Used for the right-aligned time column and the
/// header "since …" / "~…/Δ" tokens. Sub-second deltas read "now".
pub(super) fn fmt_elapsed_compact(delta_ms: i64) -> String {
    let d = delta_ms.max(0);
    if d < 1000 {
        "now".to_string()
    } else if d < 60_000 {
        format!("{}s", d / 1000)
    } else if d < 3_600_000 {
        format!("{}m", d / 60_000)
    } else if d < 86_400_000 {
        format!("{}h", d / 3_600_000)
    } else if d < 604_800_000 {
        format!("{}d", d / 86_400_000)
    } else {
        format!("{}w", d / 604_800_000)
    }
}

/// Compact a magnitude with K/M/G suffixes for the delta column (the C++
/// `compactNumber` lambda) so a large step doesn't spill into the time column.
/// `n` is treated as a magnitude (sign is applied by the caller). Below 10 000
/// the full number is printed; at/above the thresholds it switches to a
/// 3-significant-digit K/M/G form (e.g. `1234 → "1.23K"`, `2_000_000 → "2M"`).
pub(super) fn compact_number(n: i64) -> String {
    let v = n.unsigned_abs();
    if v >= 1_000_000_000 {
        trim_sig3(v as f64 / 1_000_000_000.0, 'G')
    } else if v >= 1_000_000 {
        trim_sig3(v as f64 / 1_000_000.0, 'M')
    } else if v >= 10_000 {
        trim_sig3(v as f64 / 1_000.0, 'K')
    } else {
        v.to_string()
    }
}

/// Render `value` to ~3 significant digits (Qt's `'g', 3`), strip any trailing
/// `.0`/zeros, and append `suffix`. Keeps the delta column narrow ("1.23K",
/// "2M", "1.5G") without trailing-zero noise.
fn trim_sig3(value: f64, suffix: char) -> String {
    // 3 significant figures: choose decimals so total sig digits ≈ 3.
    let s = if value >= 100.0 {
        format!("{:.0}", value)
    } else if value >= 10.0 {
        format!("{:.1}", value)
    } else {
        format!("{:.2}", value)
    };
    // Trim trailing zeros / dot so "2.00" → "2", "1.20" → "1.2".
    let trimmed = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    format!("{trimmed}{suffix}")
}

/// Parse a DISPLAYED value string as a signed integer when it looks numeric and
/// small enough that a delta would be meaningful (the C++ `tryParseAsInt`).
/// Accepts decimal (optionally signed) and `0x`/`0X` hex. Returns `None` for
/// non-numeric values (pointers/strings/floats) so their delta is skipped.
/// Pointer-sized hex (>= 9 hex digits = up to a full 64-bit address) is rejected
/// — deltas between heap addresses are noise. Decimal magnitudes that overflow
/// `i64` also yield `None`.
pub(super) fn try_parse_int(value_str: &str) -> Option<i64> {
    let t = value_str.trim();
    if t.is_empty() {
        return None;
    }
    let (hex_body, is_hex) = if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))
    {
        (rest, true)
    } else {
        (t, false)
    };
    if is_hex {
        if hex_body.is_empty() || hex_body.len() >= 9 {
            return None; // skip pointer-ish (and empty "0x")
        }
        return u64::from_str_radix(hex_body, 16).ok().map(|u| u as i64);
    }
    // Signed decimal.
    t.parse::<i64>().ok()
}

/// A per-entry recency tier (0 = newest/hottest … 3 = oldest/coldest) derived
/// from the entry's age, mirroring the C++ `timeTierColor` bands. The render
/// maps the tier to a color/emphasis so the newest value reads brightest:
///   0: < 1s   (just changed — accent)
///   1: < 1m   (strong / full text)
///   2: < 1h   (muted)
///   3: ≥ 1h   (deepest dim)
pub(super) fn recency_tier(delta_ms: i64) -> u8 {
    let d = delta_ms.max(0);
    if d < 1000 {
        0
    } else if d < 60_000 {
        1
    } else if d < 3_600_000 {
        2
    } else {
        3
    }
}

/// Build the value-history summary header (the C++ body-header line), e.g.
/// `"5 entries · 3 unique · ↑ rising · since 12m · ~30s/Δ · stale"`. Computed
/// entirely from the captured `(value, msec)` list (newest-first) plus `now_ms`:
///   - `N entr{y|ies}` — the visible (capped) entry count.
///   - `· M unique` — distinct values in the window, only when fewer than N.
///   - `· ↑ rising` / `· ↓ falling` — when every consecutive numeric delta
///     shares a sign (monotonic). Omitted for non-numeric or mixed sequences.
///   - `· since X` — age of the oldest timestamped entry.
///   - `· ~Y/Δ` — average inter-change interval (total span ÷ gaps), only when
///     ≥ 3 entries and the span is ≥ 2 s.
///   - `· stale` — when the newest entry is itself older than 5 min.
/// Tolerant: any token whose data doesn't apply is silently omitted.
pub(super) fn history_header(entries: &[(String, i64)], now_ms: i64) -> String {
    let unique = entries.len();
    let mut out = format!("{unique} entr{}", if unique == 1 { "y" } else { "ies" });

    // Distinct value count in the visible window.
    let distinct = {
        let mut seen: Vec<&str> = Vec::with_capacity(unique);
        for (v, _) in entries {
            if !seen.iter().any(|s| *s == v.as_str()) {
                seen.push(v.as_str());
            }
        }
        seen.len()
    };
    if distinct < unique {
        out.push_str(&format!(" · {distinct} unique"));
    }

    // Monotonic-direction hint over consecutive numeric deltas (newest-first:
    // entries[k] is newer than entries[k+1]).
    {
        let mut pos = 0i32;
        let mut neg = 0i32;
        let mut all_numeric = true;
        for k in 0..entries.len().saturating_sub(1) {
            match (try_parse_int(&entries[k].0), try_parse_int(&entries[k + 1].0)) {
                (Some(a), Some(b)) => {
                    let d = a - b;
                    if d > 0 {
                        pos += 1;
                    } else if d < 0 {
                        neg += 1;
                    }
                }
                _ => {
                    all_numeric = false;
                    break;
                }
            }
        }
        if all_numeric {
            if pos > 0 && neg == 0 {
                out.push_str(" · ↑ rising");
            } else if neg > 0 && pos == 0 {
                out.push_str(" · ↓ falling");
            }
        }
    }

    // Oldest timestamped entry → "since X".
    let oldest_ms = entries.iter().rev().map(|(_, t)| *t).find(|t| *t > 0);
    if let Some(oldest) = oldest_ms {
        out.push_str(&format!(" · since {}", fmt_elapsed_compact(now_ms - oldest)));
    }

    // Average change interval: span ÷ gaps, when there are ≥ 3 entries and the
    // span is at least 2 s.
    let newest_ms = entries.first().map(|(_, t)| *t).filter(|t| *t > 0);
    if let (Some(newest), Some(oldest)) = (newest_ms, oldest_ms) {
        if entries.len() >= 3 {
            let span = newest - oldest;
            let gaps = (entries.len() - 1) as i64;
            if span >= 2000 && gaps > 0 {
                out.push_str(&format!(" · ~{}/Δ", fmt_elapsed_compact(span / gaps)));
            }
        }
    }

    // Stale-newest hint: most recent entry is itself > 5 min old.
    if let Some(newest) = newest_ms {
        if now_ms - newest > 5 * 60 * 1000 {
            out.push_str(" · stale");
        }
    }

    out
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
            // Moving onto a DIFFERENT node/line releases the Esc-dismiss hover
            // latch — the preview may dwell again here. While the cursor stays on
            // the same row the latch holds (below), so Esc "sticks".
            self.hover_dwell_suppressed = false;
        }
        // Esc dismissed the popups; suppress re-opening one until the cursor moves
        // to a different row (the C++ `m_hoverDwellElapsed` reset). Still track the
        // hover band so other affordances update.
        if self.hover_dwell_suppressed {
            if changed_band {
                cx.notify();
            }
            return;
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
                    // Capture (value, raw epoch-msec) pairs newest→oldest. The
                    // render derives the relative-age label, signed delta and
                    // recency tiers from the raw timestamp (bf49a64). Also carry
                    // the UNCAPPED total (`count`) so the render can surface the
                    // ring-overflow footer when older values were evicted.
                    let mut entries: Vec<(String, i64)> = Vec::new();
                    hist.for_each_with_time(|v, t| {
                        if entries.len() < crate::core::value_history::K_CAPACITY {
                            entries.push((v.to_string(), t));
                        }
                    });
                    if entries.len() > 1 {
                        let set_buttons = self.editing.as_ref().map(|e| e.line) == Some(line);
                        return Some(HoverPopupState {
                            line,
                            pos,
                            kind: HoverPopupKind::ValueHistory {
                                entries,
                                total_count: i64::from(hist.count),
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

    /// Current wall-clock time in milliseconds since the Unix epoch (the value-
    /// history clock — feeds the compact time column + the header age tokens, the
    /// C++ `QDateTime::currentMSecsSinceEpoch()` recomputed per popup rebuild).
    /// Falls back to 0 if the clock is before the epoch.
    pub(super) fn now_millis() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
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
        let (title, mut body): (String, String) = {
            #[cfg(not(feature = "disasm"))]
            {
                let _ = is_fp;
                (String::new(), String::new())
            }
            #[cfg(feature = "disasm")]
            {
                if is_fp {
                    (
                        "Disassembly".to_string(),
                        crate::disasm::disassemble(
                            &bytes,
                            ptr_val,
                            if is64 { 64 } else { 32 },
                            MAX_READ,
                        ),
                    )
                } else {
                    (
                        "Hex Dump".to_string(),
                        crate::disasm::hex_dump(&bytes, ptr_val, MAX_READ),
                    )
                }
            }
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
                total_count,
                node_idx,
                sub_line,
                resolved_addr,
                set_buttons,
            } => {
                let node_idx = *node_idx;
                let sub_line = *sub_line;
                let resolved_addr = *resolved_addr;
                let set_buttons = *set_buttons;
                let total_count = *total_count;
                // Recompute "now" fresh each frame so the time column stays live
                // (the C++ recomputes `now` in the builder). `hover_kind_eq` ignores
                // the raw msec, so this does NOT churn the popup identity.
                let now = Self::now_millis();
                let font_size = self.editor_font_size();
                let font_family = self.editor_font_family();
                let unique_shown = entries.len();

                // Fixed monospace column widths (≈0.62 ch advance) so values stay
                // vertically aligned as the elapsed counters tick — the C++ locks
                // these with QFontMetrics. Index = 2 ch, delta = "+1.23K" (~6 ch),
                // time = "999w" (~4 ch).
                let ch = font_size * 0.62;
                let idx_w = px(ch * 2.0);
                let delta_w = px(ch * 6.0 + 2.0);
                let time_w = px(ch * 4.0 + 2.0);

                // Recency-tier → color: newest/just (0) = accent, recent (1) =
                // full text, minutes (2) = dim, hours+ (3) = deeper-faded dim.
                let tier_color = |tier: u8| -> Hsla {
                    match tier {
                        0 => palette.accent,
                        1 => palette.text,
                        2 => palette.dim,
                        _ => with_alpha(palette.dim, 0.7),
                    }
                };

                let newest_value = entries.first().map(|(v, _)| v.clone()).unwrap_or_default();

                let mut rows: Vec<AnyElement> = Vec::with_capacity(unique_shown + 2);
                for (i, (v, msec)) in entries.iter().enumerate() {
                    let msec = *msec;
                    let age_ms = if msec > 0 { (now - msec).max(0) } else { 0 };
                    let tier = recency_tier(age_ms);

                    // Index column — "▸" accent caret on the newest row; older rows
                    // numbered from 1, colored by their own recency tier so the
                    // index + time columns form a matching pair of recency cues.
                    let idx_text = if i == 0 {
                        "▸".to_string()
                    } else {
                        (i + 1).to_string()
                    };
                    let idx_color = if i == 0 { palette.accent } else { tier_color(tier) };

                    // Value — bright on the newest, fading on older rows via the
                    // recency tier. Rows that REPEAT the newest value fade an extra
                    // notch (the data point is already shown on row 0).
                    let mut val_color = if i == 0 { palette.text } else { tier_color(tier) };
                    if i > 0 && *v == newest_value {
                        val_color = with_alpha(val_color, 0.6);
                    }

                    // Signed delta from the next-older numeric value. Skipped for
                    // non-numeric (pointers/strings) or a zero step. Positive →
                    // number hue, negative → warm (heat-hot) so direction reads
                    // without parsing the sign; magnitude compacted (K/M/G).
                    let mut delta_text = String::new();
                    let mut delta_color = palette.dim;
                    if let Some(cur) = try_parse_int(v) {
                        if let Some((older, _)) = entries.get(i + 1) {
                            if let Some(prev) = try_parse_int(older) {
                                let diff = cur - prev;
                                if diff != 0 {
                                    let mag = compact_number(diff);
                                    delta_text = if diff > 0 {
                                        format!("+{mag}")
                                    } else {
                                        // U+2212 MINUS SIGN reads cleaner than '-'.
                                        format!("\u{2212}{mag}")
                                    };
                                    delta_color = if diff > 0 {
                                        palette.number
                                    } else {
                                        palette.heat_hot
                                    };
                                }
                            }
                        }
                    }

                    let mut row = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(design::tokens::space::SM))
                        .text_size(px(font_size))
                        .font_family(font_family.clone())
                        // Index column.
                        .child(
                            div()
                                .flex_none()
                                .w(idx_w)
                                .text_right()
                                .text_color(idx_color)
                                .child(SharedString::from(idx_text)),
                        )
                        // Value column — grows, truncates so a long value can't
                        // blow the card width; the time/delta columns stay pinned.
                        .child(
                            div()
                                .flex_grow()
                                .min_w(px(0.0))
                                .truncate()
                                .text_color(val_color)
                                .child(SharedString::from(v.clone())),
                        )
                        // Delta column (fixed width; empty placeholder keeps the
                        // value column from shifting when there's no numeric step).
                        .child(
                            div()
                                .flex_none()
                                .w(delta_w)
                                .text_right()
                                .text_size(px(design::tokens::font::UI_XS))
                                .text_color(delta_color)
                                .child(SharedString::from(delta_text)),
                        )
                        // Time column — compact, right-aligned, recency-tinted.
                        .child(
                            div()
                                .flex_none()
                                .w(time_w)
                                .text_right()
                                .text_size(px(design::tokens::font::UI_XS))
                                .text_color(tier_color(tier))
                                .child(SharedString::from(if msec > 0 {
                                    fmt_elapsed_compact(age_ms)
                                } else {
                                    String::new()
                                })),
                        );

                    // Edit-time 'Set' button (item 68): writes this value back into
                    // the node via the controller's `set_node_value`.
                    if set_buttons && node_idx >= 0 {
                        let val = v.clone();
                        row = row.child(
                            div()
                                .id(("vh-set", i))
                                .flex_none()
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
                    rows.push(row.into_any_element());

                    // Thin newest-vs-rest separator, drawn once after row 0 so the
                    // eye locks onto "current vs previous" at a glance.
                    if i == 0 && unique_shown > 1 {
                        rows.push(
                            div()
                                .h(px(1.0))
                                .my(px(2.0))
                                .bg(with_alpha(palette.border, 0.5))
                                .into_any_element(),
                        );
                    }
                }

                // Ring-overflow footer — ValueHistory caps at K_CAPACITY entries;
                // when the uncapped total exceeds the visible window, earlier values
                // were evicted. Surface the drop count so the popup doesn't read as
                // a complete log when it's a sliding window.
                let discarded = total_count - unique_shown as i64;
                let footer: Option<AnyElement> = if discarded > 0 {
                    Some(
                        div()
                            .pt(px(design::tokens::space::XS))
                            .text_size(px(design::tokens::font::UI_XS))
                            .text_color(palette.dim)
                            .italic()
                            .child(SharedString::from(format!(
                                "+ {discarded} earlier value{} discarded",
                                if discarded == 1 { "" } else { "s" }
                            )))
                            .into_any_element(),
                    )
                } else {
                    None
                };

                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap(px(design::tokens::space::XS))
                    .child(
                        // Summary header (the C++ body header) replacing the bare
                        // "Previous Values" title — entry/unique counts, monotonic
                        // direction, age, avg interval, stale hint. Carries the
                        // HLine divider beneath it (the popup divider).
                        div()
                            .text_size(px(design::tokens::font::UI_XS))
                            .text_color(palette.dim)
                            .pb(px(design::tokens::space::XS))
                            .border_b_1()
                            .border_color(palette.border)
                            .child(SharedString::from(history_header(entries, now))),
                    )
                    // Height-capped scroll body: a tall history scrolls instead of
                    // crushing each row (the C++ QScrollArea wrapper). ~14 rows tall.
                    .child(
                        div()
                            .id("rcx-vh-scroll")
                            .flex()
                            .flex_col()
                            .max_h(px(font_size * 14.0 + 56.0))
                            .overflow_y_scroll()
                            .children(rows),
                    );
                if let Some(footer) = footer {
                    card = card.child(footer);
                }
                card
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

#[cfg(test)]
mod tests {
    use super::{
        compact_number, fmt_elapsed_compact, history_header, recency_tier, try_parse_int,
    };

    const S: i64 = 1000;
    const M: i64 = 60 * S;
    const H: i64 = 60 * M;
    const D: i64 = 24 * H;
    const W: i64 = 7 * D;

    #[test]
    fn fmt_elapsed_compact_tiers() {
        assert_eq!(fmt_elapsed_compact(0), "now");
        assert_eq!(fmt_elapsed_compact(-5), "now"); // clamped
        assert_eq!(fmt_elapsed_compact(999), "now");
        assert_eq!(fmt_elapsed_compact(12 * S), "12s");
        assert_eq!(fmt_elapsed_compact(59 * S), "59s");
        assert_eq!(fmt_elapsed_compact(5 * M), "5m");
        assert_eq!(fmt_elapsed_compact(3 * H), "3h");
        assert_eq!(fmt_elapsed_compact(2 * D), "2d");
        assert_eq!(fmt_elapsed_compact(1 * W), "1w");
        assert_eq!(fmt_elapsed_compact(3 * W), "3w");
    }

    #[test]
    fn compact_number_kmg() {
        // Below 10_000 → full number.
        assert_eq!(compact_number(0), "0");
        assert_eq!(compact_number(42), "42");
        assert_eq!(compact_number(9999), "9999");
        assert_eq!(compact_number(-9999), "9999"); // magnitude only
                                                    // K tier.
        assert_eq!(compact_number(10_000), "10K");
        assert_eq!(compact_number(1_234_000), "1.23M");
        // M tier.
        assert_eq!(compact_number(2_000_000), "2M");
        assert_eq!(compact_number(-2_000_000), "2M");
        // G tier.
        assert_eq!(compact_number(1_500_000_000), "1.5G");
    }

    #[test]
    fn try_parse_int_hex_dec_signed_and_none() {
        // Decimal, signed.
        assert_eq!(try_parse_int("42"), Some(42));
        assert_eq!(try_parse_int("-7"), Some(-7));
        assert_eq!(try_parse_int("  100 "), Some(100));
        // Hex (case-insensitive prefix).
        assert_eq!(try_parse_int("0x10"), Some(16));
        assert_eq!(try_parse_int("0X1f"), Some(31));
        // Pointer-ish hex (>= 9 digits) rejected.
        assert_eq!(try_parse_int("0x100000000"), None);
        assert_eq!(try_parse_int("0x"), None);
        // Non-numeric → None.
        assert_eq!(try_parse_int(""), None);
        assert_eq!(try_parse_int("hello"), None);
        assert_eq!(try_parse_int("3.14"), None); // float
        assert_eq!(try_parse_int("0xZZ"), None);
    }

    #[test]
    fn recency_tier_bands() {
        assert_eq!(recency_tier(0), 0);
        assert_eq!(recency_tier(500), 0);
        assert_eq!(recency_tier(5 * S), 1);
        assert_eq!(recency_tier(5 * M), 2);
        assert_eq!(recency_tier(5 * H), 3);
    }

    #[test]
    fn history_header_counts_and_direction() {
        // now-relative timestamps, newest-first. A rising counter: 30, 20, 10.
        let now = 10_000_000i64;
        let entries = vec![
            ("30".to_string(), now - 10 * S),
            ("20".to_string(), now - 40 * S),
            ("10".to_string(), now - 70 * S),
        ];
        let h = history_header(&entries, now);
        // Visible entry count.
        assert!(h.contains("3 entries"), "header = {h:?}");
        // Monotonic rising direction (newest 30 > older 20 > 10).
        assert!(h.contains("↑ rising"), "header = {h:?}");
        // Oldest-age token.
        assert!(h.contains("since "), "header = {h:?}");
    }

    #[test]
    fn history_header_unique_and_mixed_no_direction() {
        let now = 10_000_000i64;
        // A bouncing value A→B→A: 3 entries, 2 unique, mixed direction (non-numeric
        // here so no direction token at all).
        let entries = vec![
            ("ptrA".to_string(), now - 1 * S),
            ("ptrB".to_string(), now - 2 * S),
            ("ptrA".to_string(), now - 3 * S),
        ];
        let h = history_header(&entries, now);
        assert!(h.contains("3 entries"), "header = {h:?}");
        assert!(h.contains("2 unique"), "header = {h:?}");
        assert!(
            !h.contains("rising") && !h.contains("falling"),
            "non-numeric → no direction hint: {h:?}"
        );
    }

    #[test]
    fn history_header_stale_when_newest_old() {
        let now = 10_000_000i64;
        // Newest entry is 10 min old → stale.
        let entries = vec![
            ("1".to_string(), now - 10 * M),
            ("2".to_string(), now - 20 * M),
        ];
        let h = history_header(&entries, now);
        assert!(h.contains("stale"), "header = {h:?}");
    }
}
