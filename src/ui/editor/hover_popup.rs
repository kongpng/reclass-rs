//! The editor's hover popups — the value-history / disasm / struct-preview cards
//! over a row's value column (item 13) — with their state types and the
//! hover_kind_eq comparator, extracted from editor/mod.rs into a second
//! impl RcxEditor. A child module of editor, so it keeps access to RcxEditor's
//! private fields and methods.

use crate::core::{format_hint, infer_strong_types, ChipKind, InferHints};
use crate::provider::{MemoryRegion, ModuleEntry, Provider, RegionType};

use super::*;
use gpui::*;
use std::cell::Cell;
use std::sync::{Arc, Weak};

pub(super) const MEMORY_PREVIEW_MIN_ROWS: usize = 10;
pub(super) const MEMORY_PREVIEW_MAX_ROWS: usize = 64;

/// Last row/value location that produced, or could refresh, a hover popup.
#[derive(Clone, Debug)]
pub(super) struct HoverProbe {
    pub(super) line: usize,
    pub(super) rel_x: f32,
    pub(super) pos: Point<Pixels>,
}

pub(super) struct HoverMemoryMaps {
    result_revision: u64,
    provider: Weak<dyn Provider + Send + Sync>,
    regions: Vec<MemoryRegion>,
    modules: Vec<ModuleEntry>,
    region_order: MemoryPreviewLookupOrder,
    module_order: MemoryPreviewLookupOrder,
    modules_loaded: bool,
}

impl HoverMemoryMaps {
    fn with_regions(
        result_revision: u64,
        provider: &Arc<dyn Provider + Send + Sync>,
        regions: Vec<MemoryRegion>,
    ) -> Self {
        let region_order = region_lookup_order(&regions);
        let module_order = module_lookup_order(&[]);
        Self {
            result_revision,
            provider: Arc::downgrade(provider),
            regions,
            modules: Vec::new(),
            region_order,
            module_order,
            modules_loaded: false,
        }
    }

    fn with_regions_modules(
        result_revision: u64,
        provider: &Arc<dyn Provider + Send + Sync>,
        regions: Vec<MemoryRegion>,
        modules: Vec<ModuleEntry>,
    ) -> Self {
        let region_order = region_lookup_order(&regions);
        let module_order = module_lookup_order(&modules);
        Self {
            result_revision,
            provider: Arc::downgrade(provider),
            regions,
            modules,
            region_order,
            module_order,
            modules_loaded: true,
        }
    }

    fn from_provider_with_modules(
        result_revision: u64,
        provider: &Arc<dyn Provider + Send + Sync>,
    ) -> Self {
        let (modules, regions) = provider.enumerate_modules_and_regions();
        Self::with_regions_modules(result_revision, provider, regions, modules)
    }

    fn matches(&self, result_revision: u64, provider: &Arc<dyn Provider + Send + Sync>) -> bool {
        self.result_revision == result_revision
            && self
                .provider
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, provider))
    }

    fn set_modules(&mut self, modules: Vec<ModuleEntry>) {
        self.module_order = module_lookup_order(&modules);
        self.modules = modules;
        self.modules_loaded = true;
    }

    fn lookup(&self) -> MemoryPreviewLookup<'_> {
        MemoryPreviewLookup::with_orders(
            &self.regions,
            &self.modules,
            &self.region_order,
            &self.module_order,
        )
    }
}

pub(super) struct HoverMemoryPreviewCache {
    result_revision: u64,
    provider: Weak<dyn Provider + Send + Sync>,
    target_addr: u64,
    pointer_size: i32,
    row_count: usize,
    bytes: Vec<u8>,
    rows: Arc<[MemoryPreviewRow]>,
}

impl HoverMemoryPreviewCache {
    fn new(
        result_revision: u64,
        provider: &Arc<dyn Provider + Send + Sync>,
        target_addr: u64,
        pointer_size: i32,
        row_count: usize,
        bytes: Vec<u8>,
        rows: Arc<[MemoryPreviewRow]>,
    ) -> Self {
        Self {
            result_revision,
            provider: Arc::downgrade(provider),
            target_addr,
            pointer_size,
            row_count,
            bytes,
            rows,
        }
    }

    fn matches(
        &self,
        result_revision: u64,
        provider: &Arc<dyn Provider + Send + Sync>,
        target_addr: u64,
        pointer_size: i32,
        row_count: usize,
        bytes: &[u8],
    ) -> bool {
        self.result_revision == result_revision
            && self.target_addr == target_addr
            && self.pointer_size == pointer_size
            && self.row_count == row_count
            && self.bytes == bytes
            && self
                .provider
                .upgrade()
                .is_some_and(|cached| Arc::ptr_eq(&cached, provider))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MemoryPreviewRow {
    offset: u64,
    kind: &'static str,
    ascii: String,
    hex: String,
    type_hint: Option<String>,
    pointer_note: Option<String>,
}

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
    /// ReClass.NET-style memory preview for pointer targets: a compact Hex32/Hex64
    /// row grid that can be expanded with the mouse wheel while the popup is open.
    MemoryPreview {
        target_addr: u64,
        pointer_size: i32,
        row_count: usize,
        rows: Arc<[MemoryPreviewRow]>,
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
        (
            HoverPopupKind::MemoryPreview {
                target_addr: ta,
                pointer_size: pa,
                row_count: ra,
                rows: ra_rows,
            },
            HoverPopupKind::MemoryPreview {
                target_addr: tb,
                pointer_size: pb,
                row_count: rb,
                rows: rb_rows,
            },
        ) => {
            ta == tb
                && pa == pb
                && ra == rb
                && (Arc::ptr_eq(ra_rows, rb_rows) || ra_rows.as_ref() == rb_rows.as_ref())
        }
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
    let (hex_body, is_hex) =
        if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
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
            match (
                try_parse_int(&entries[k].0),
                try_parse_int(&entries[k + 1].0),
            ) {
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
        out.push_str(&format!(
            " · since {}",
            fmt_elapsed_compact(now_ms - oldest)
        ));
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
        let probe = HoverProbe { line, rel_x, pos };
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
        if self.hover_popup.is_some() {
            self.hover_probe = Some(probe);
        } else {
            self.hover_probe = None;
            self.hover_memory_maps = None;
            self.hover_memory_preview_cache = None;
            self.memory_preview_rows = MEMORY_PREVIEW_MIN_ROWS;
        }
        if changed_band || changed_popup {
            cx.notify();
        }
    }

    /// Open a hover-style value preview from an explicit command/menu action.
    /// Unlike passive hover, this ignores the Hover Effects toggle and shows a
    /// diagnostic card when the row has no previewable value so the command never
    /// feels like a dead click.
    pub(super) fn show_explicit_value_preview(
        &mut self,
        line: usize,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.hover_dwell_suppressed = false;
        self.popup_cursor_inside = false;

        let reason = match self.line_meta(line).cloned() {
            None => Some("This row is no longer available.".to_string()),
            Some(lm) if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID => {
                Some("This row has no node value to preview.".to_string())
            }
            Some(lm) if lm.line_kind == LineKind::Footer => {
                Some("Footer rows do not have a value preview.".to_string())
            }
            Some(lm) => {
                let (type_w, name_w) = geometry::effective_widths(&lm);
                let vs = crate::compose::value_span_for(&lm, type_w, name_w);
                if !vs.valid || vs.end <= vs.start {
                    Some("This row has no value column to preview.".to_string())
                } else {
                    let rel_x = self.metrics.x_for_col(vs.start) + self.metrics.cell_width * 0.5;
                    if let Some(state) = self.compute_hover_popup(line, rel_x, pos) {
                        self.hover_popup = Some(state);
                        self.hover_probe = Some(HoverProbe { line, rel_x, pos });
                        cx.notify();
                        return;
                    }
                    Some(
                        "No readable pointer target or value history is available for this value."
                            .to_string(),
                    )
                }
            }
        };

        self.hover_probe = None;
        self.hover_memory_maps = None;
        self.hover_memory_preview_cache = None;
        self.memory_preview_rows = MEMORY_PREVIEW_MIN_ROWS;
        self.hover_popup = Some(HoverPopupState {
            line,
            pos,
            kind: HoverPopupKind::TitleBody {
                title: "Value Preview".to_string(),
                body: reason.unwrap_or_else(|| "No preview is available.".to_string()),
            },
        });
        cx.notify();
    }

    /// Rebuild the currently-open hover popup from the last row/value probe. This
    /// is used by the live refresh timer so value history and memory preview cards
    /// keep polling even when the mouse is stationary or inside the popup.
    pub(super) fn refresh_hover_popup_from_probe(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.is_some()
            || self.hover_dwell_suppressed
            || !self.hover_effects
            || self.hover_popup.is_none()
        {
            return;
        }
        let Some(probe) = self.hover_probe.clone() else {
            return;
        };
        let want = self.compute_hover_popup(probe.line, probe.rel_x, probe.pos);
        let changed = match (&self.hover_popup, &want) {
            (None, None) => false,
            (Some(a), Some(b)) => a.line != b.line || !hover_kind_eq(&a.kind, &b.kind),
            _ => true,
        };
        if changed {
            self.hover_popup = want;
            if self.hover_popup.is_none() {
                self.hover_probe = None;
                self.hover_memory_maps = None;
                self.hover_memory_preview_cache = None;
                self.memory_preview_rows = MEMORY_PREVIEW_MIN_ROWS;
                self.popup_cursor_inside = false;
            }
            cx.notify();
        } else if matches!(
            self.hover_popup.as_ref().map(|s| &s.kind),
            Some(HoverPopupKind::ValueHistory { .. } | HoverPopupKind::MemoryPreview { .. })
        ) {
            // Keep relative timestamps and live preview reads painting while open.
            cx.notify();
        }
    }

    /// Expand/contract an open ReClass-style memory preview. Returns true when the
    /// wheel event was consumed by the popup.
    pub(super) fn adjust_memory_preview_rows(
        &mut self,
        wheel_delta_y: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.popup_cursor_inside {
            return false;
        }
        let Some(HoverPopupState {
            kind: HoverPopupKind::MemoryPreview { .. },
            ..
        }) = self.hover_popup.as_ref()
        else {
            return false;
        };
        if wheel_delta_y == 0.0 {
            return true;
        }
        let next = if wheel_delta_y < 0.0 {
            self.memory_preview_rows.saturating_add(1)
        } else {
            self.memory_preview_rows.saturating_sub(1)
        };
        self.memory_preview_rows = next.clamp(MEMORY_PREVIEW_MIN_ROWS, MEMORY_PREVIEW_MAX_ROWS);
        self.refresh_hover_popup_from_probe(cx);
        cx.notify();
        true
    }

    /// Row hit-testing only fires while the cursor is over a rendered row. If the
    /// cursor moves into empty editor space, close the active hover card unless it
    /// is currently being held open by the popup's own hover guard.
    pub(super) fn dismiss_hover_if_pointer_left_anchor(
        &mut self,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.hover_popup.is_none() || self.popup_cursor_inside || self.context_menu.is_some() {
            return;
        }
        let Some(probe) = self.hover_probe.as_ref() else {
            self.clear_hover_state(cx);
            return;
        };
        let dy = f32::from(pos.y - probe.pos.y).abs();
        if dy <= self.metrics.line_height {
            return;
        }
        self.clear_hover_state(cx);
    }

    /// Compute the hover popup (if any) for the value column under `(line, rel_x)`.
    /// Pure-ish (reads the controller's tree/provider/value-history); returns the
    /// popup state to show, or `None`. Mirrors the kind selection in editor.cpp
    /// `applyHoverCursor` (value history vs disasm/hex vs struct preview).
    fn compute_hover_popup(
        &mut self,
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
        let in_value_span = col >= vs.start && col < vs.end;
        let hex_pointer_value_active = hex_pointer_value_active(&lm, col, vs);
        // Item 28: gate rich hovers to the value span, plus the explicit
        // pointer TypeHint chip span for inferred Hex32/Hex64 pointers.
        if !in_value_span && !hex_pointer_value_active {
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
                if is_fp {
                    if let Some(state) = self.pointer_disasm_popup(&lm, pos) {
                        return Some(state);
                    }
                } else if let Some(state) = self.pointer_memory_preview_popup(&lm, pos) {
                    return Some(state);
                }
                // No readable target — fall through (no popup).
                return None;
            }

            // 1a) Hex32/Hex64 value or pointer TypeHint chip → memory preview
            // when the inferred/stored target is actually readable.
            if hex_pointer_value_active {
                if let Some(state) = self.pointer_memory_preview_popup(&lm, pos) {
                    return Some(state);
                }
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

    fn read_pointer_value(&self, lm: &LineMeta) -> Option<(u64, i32)> {
        self.read_pointer_value_with_regions(lm, None)
    }

    fn read_pointer_value_with_regions(
        &self,
        lm: &LineMeta,
        regions: Option<&[MemoryRegion]>,
    ) -> Option<(u64, i32)> {
        self.read_pointer_value_with_lookup(lm, None, regions)
    }

    fn read_pointer_value_with_lookup(
        &self,
        lm: &LineMeta,
        lookup: Option<&MemoryPreviewLookup<'_>>,
        regions: Option<&[MemoryRegion]>,
    ) -> Option<(u64, i32)> {
        let prov = &self.controller.document().provider;
        let (is64, size) = match lm.node_kind {
            NodeKind::Pointer64 | NodeKind::FuncPtr64 | NodeKind::Hex64 => (true, 8),
            NodeKind::Pointer32 | NodeKind::FuncPtr32 | NodeKind::Hex32 => (false, 4),
            _ => return None,
        };
        if !provider_can_read_with_lookup(&**prov, lookup, regions, lm.offset_addr, size) {
            return None;
        }
        let mut ptr_val = if is64 {
            prov.read_u64(lm.offset_addr)
        } else {
            u64::from(prov.read_u32(lm.offset_addr))
        };
        if ptr_val == 0 || ptr_val == u64::MAX || (!is64 && ptr_val == 0xFFFF_FFFF) {
            return None;
        }
        let idx = self.controller.tree().index_of_id(lm.node_id);
        if idx >= 0 && self.controller.tree().nodes[idx as usize].is_relative {
            ptr_val = ptr_val.wrapping_add(self.controller.tree().base_address);
        }
        Some((ptr_val, size))
    }

    /// Build the disasm popup for a function pointer by reading the pointer value,
    /// then the bytes at the target. Memory previews for ordinary pointers use
    /// `pointer_memory_preview_popup`.
    fn pointer_disasm_popup(&self, lm: &LineMeta, pos: Point<Pixels>) -> Option<HoverPopupState> {
        let prov = &self.controller.document().provider;
        let (ptr_val, ptr_size) = self.read_pointer_value(lm)?;
        const MAX_READ: i32 = 128;
        let bytes = read_provider_bytes(&**prov, ptr_val, MAX_READ)?;
        if bytes.is_empty() {
            return None;
        }
        let (title, mut body): (String, String) = {
            #[cfg(not(feature = "disasm"))]
            {
                (String::new(), String::new())
            }
            #[cfg(feature = "disasm")]
            {
                (
                    "Disassembly".to_string(),
                    crate::disasm::disassemble(&bytes, ptr_val, ptr_size * 8, MAX_READ),
                )
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

    fn pointer_memory_preview_popup(
        &mut self,
        lm: &LineMeta,
        pos: Point<Pixels>,
    ) -> Option<HoverPopupState> {
        let prov = self.controller.document().provider.clone();
        let result_revision = self.controller.result_revision();
        let needs_maps = self
            .hover_memory_maps
            .as_ref()
            .is_none_or(|maps| !maps.matches(result_revision, &prov));
        if needs_maps {
            self.hover_memory_maps = Some(HoverMemoryMaps::from_provider_with_modules(
                result_revision,
                &prov,
            ));
            self.hover_memory_preview_cache = None;
        } else if self
            .hover_memory_maps
            .as_ref()
            .is_some_and(|maps| !maps.modules_loaded)
        {
            let modules = prov.enumerate_modules();
            if let Some(maps) = self.hover_memory_maps.as_mut() {
                maps.set_modules(modules);
            }
        }
        let maps = self.hover_memory_maps.as_ref()?;
        let lookup = maps.lookup();
        let (target_addr, pointer_size) =
            self.read_pointer_value_with_lookup(lm, Some(&lookup), Some(&maps.regions))?;
        if target_addr == 0 {
            return None;
        }
        let row_count = self
            .memory_preview_rows
            .clamp(MEMORY_PREVIEW_MIN_ROWS, MEMORY_PREVIEW_MAX_ROWS);
        let len = row_count.saturating_mul(pointer_size.max(1) as usize);
        let bytes = read_provider_bytes_best_effort_with_lookup(
            &*prov,
            Some(&lookup),
            Some(&maps.regions),
            target_addr,
            len,
            1,
        )?;
        if bytes.is_empty() {
            return None;
        }
        if let Some(cache) = self.hover_memory_preview_cache.as_ref() {
            if cache.matches(
                result_revision,
                &prov,
                target_addr,
                pointer_size,
                row_count,
                &bytes,
            ) {
                return Some(HoverPopupState {
                    line: lm.node_idx.max(0) as usize,
                    pos,
                    kind: HoverPopupKind::MemoryPreview {
                        target_addr,
                        pointer_size,
                        row_count,
                        rows: cache.rows.clone(),
                    },
                });
            }
        }
        let rows: Arc<[MemoryPreviewRow]> =
            memory_preview_rows_with_lookup(&*prov, pointer_size, row_count, &bytes, &lookup)
                .into();
        if rows.is_empty() {
            return None;
        }
        self.hover_memory_preview_cache = Some(HoverMemoryPreviewCache::new(
            result_revision,
            &prov,
            target_addr,
            pointer_size,
            row_count,
            bytes,
            rows.clone(),
        ));
        Some(HoverPopupState {
            line: lm.node_idx.max(0) as usize,
            pos,
            kind: HoverPopupKind::MemoryPreview {
                target_addr,
                pointer_size,
                row_count,
                rows,
            },
        })
    }

    /// Build the struct-preview popup for a collapsed typed pointer (item 13):
    /// compose the referenced struct at the pointer target and show its first few
    /// data lines (skipping the command row). `None` when the pointer has no valid
    /// struct ref. Mirrors editor.cpp's struct-preview popup.
    fn struct_preview_popup(
        &mut self,
        lm: &LineMeta,
        pos: Point<Pixels>,
    ) -> Option<HoverPopupState> {
        let ref_id = {
            let tree = self.controller.tree();
            let n = tree.nodes.get(lm.node_idx as usize)?;
            n.ref_id
        };
        if ref_id == 0 || self.controller.tree().index_of_id(ref_id) < 0 {
            return None;
        }
        let prov = self.controller.document().provider.clone();
        let result_revision = self.controller.result_revision();
        let needs_regions = self
            .hover_memory_maps
            .as_ref()
            .is_none_or(|maps| !maps.matches(result_revision, &prov));
        if needs_regions {
            let regions = prov.enumerate_regions();
            self.hover_memory_maps = Some(HoverMemoryMaps::with_regions(
                result_revision,
                &prov,
                regions,
            ));
            self.hover_memory_preview_cache = None;
        }
        let maps = self.hover_memory_maps.as_ref()?;
        let lookup = maps.lookup();
        let (target_addr, _) =
            self.read_pointer_value_with_lookup(lm, Some(&lookup), Some(&maps.regions))?;
        if !target_is_readable_with_lookup(
            &*prov,
            Some(&lookup),
            Some(&maps.regions),
            target_addr,
            1,
        ) {
            return None;
        }
        // Compose the referenced struct at the pointer target.
        let cr = crate::compose::compose_preview_with_symbols_at_base(
            self.controller.tree(),
            &*prov,
            ref_id,
            target_addr,
            self.compact_columns,
            self.tree_lines(),
            true,
            self.type_hints(),
            self.show_comments(),
            None,
            true,
            true,
            MAX_LINES + 1,
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
                    let idx_color = if i == 0 {
                        palette.accent
                    } else {
                        tier_color(tier)
                    };

                    // Value — bright on the newest, fading on older rows via the
                    // recency tier. Rows that REPEAT the newest value fade an extra
                    // notch (the data point is already shown on row 0).
                    let mut val_color = if i == 0 {
                        palette.text
                    } else {
                        tier_color(tier)
                    };
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
            HoverPopupKind::MemoryPreview {
                target_addr,
                pointer_size,
                row_count,
                rows,
            } => {
                let font_size = self.editor_font_size();
                let font_family = self.editor_font_family();
                let stride = (*pointer_size).max(1) as usize;
                let ch = font_size * 0.62;
                let offset_w = px(ch * 5.0);
                let type_w = px(ch * 6.0);
                let ascii_w = px(ch * stride.max(4) as f32);
                let hex_w = px(ch * (stride.saturating_mul(3).saturating_sub(1)).max(8) as f32);
                let body_rows: Vec<AnyElement> = rows
                    .iter()
                    .cloned()
                    .map(|row| {
                        let mut line = div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(design::tokens::space::XS))
                            .text_size(px(font_size))
                            .font_family(font_family.clone())
                            .child(
                                div()
                                    .flex_none()
                                    .w(offset_w)
                                    .text_color(palette.number)
                                    .child(format!("+{:X}", row.offset)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(type_w)
                                    .text_color(palette.type_fg)
                                    .child(row.kind),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(ascii_w)
                                    .text_color(palette.ascii)
                                    .child(row.ascii),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(hex_w)
                                    .text_color(palette.dim)
                                    .child(row.hex),
                            );
                        if let Some(type_hint) = row.type_hint {
                            let hint_children = semantic_hint_children(&type_hint, false, &palette);
                            line = line.child(
                                div()
                                    .flex_none()
                                    .max_w(px(ch * 28.0))
                                    .truncate()
                                    .children(hint_children),
                            );
                        }
                        if let Some(pointer_note) = row.pointer_note {
                            let note_children = pointer_note_children(&pointer_note, &palette);
                            line = line.child(
                                div()
                                    .flex_grow()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .children(note_children),
                            );
                        }
                        line.into_any_element()
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
                            .child(format!(
                                "Memory Preview  0x{target_addr:016X}  {row_count} rows"
                            )),
                    )
                    .children(body_rows)
                    .child(
                        div()
                            .pt(px(design::tokens::space::XS))
                            .text_size(px(design::tokens::font::UI_XS))
                            .text_color(palette.dim)
                            .child("Wheel over preview to resize"),
                    )
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

fn provider_can_read(provider: &dyn Provider, addr: u64, len: i32) -> bool {
    provider_can_read_cached(provider, None, addr, len)
}

fn provider_can_read_cached(
    provider: &dyn Provider,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    len: i32,
) -> bool {
    if len <= 0 {
        return len == 0;
    }
    if let Some(regions) = regions {
        if !regions.is_empty() {
            return regions
                .iter()
                .any(|r| region_contains_readable(r, addr, len));
        }
    }
    if provider.is_readable(addr, len) {
        return true;
    }
    provider.enumerate_regions().into_iter().any(|r| {
        r.readable
            && addr >= r.base
            && addr
                .checked_add(len as u64)
                .is_some_and(|end| end <= r.base.saturating_add(r.size))
    })
}

fn provider_can_read_with_lookup(
    provider: &dyn Provider,
    lookup: Option<&MemoryPreviewLookup<'_>>,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    len: i32,
) -> bool {
    if len <= 0 {
        return len == 0;
    }
    if let Some(lookup) = lookup {
        if !lookup.regions.is_empty() {
            return lookup.is_readable(addr, len);
        }
    }
    provider_can_read_cached(provider, regions, addr, len)
}

fn hex_pointer_value_active(lm: &LineMeta, col: i32, vs: crate::compose::ColumnSpan) -> bool {
    if !matches!(lm.node_kind, NodeKind::Hex32 | NodeKind::Hex64) {
        return false;
    }
    let narrowed = geometry::narrow_value_at_first_chip(lm, vs);
    let span = if narrowed.valid { narrowed } else { vs };
    (col >= span.start && col < span.end) || pointer_type_hint_chip_active(lm, col)
}

fn pointer_type_hint_chip_active(lm: &LineMeta, col: i32) -> bool {
    lm.chips.iter().any(|chip| {
        chip.kind == ChipKind::TypeHint
            && col >= chip.start_col
            && col < chip.end_col
            && chip
                .type_hint_kinds
                .iter()
                .any(|k| matches!(k, NodeKind::Pointer32 | NodeKind::Pointer64))
    })
}

fn read_provider_bytes(provider: &dyn Provider, addr: u64, len: i32) -> Option<Vec<u8>> {
    if !provider_can_read(provider, addr, len) {
        return None;
    }
    let mut bytes = vec![0u8; len.max(0) as usize];
    if !provider.read(addr, &mut bytes) {
        return None;
    }
    Some(bytes)
}

fn readable_tail_len_cached(
    provider: &dyn Provider,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    preferred_len: usize,
) -> usize {
    if preferred_len == 0 {
        return 0;
    }
    let from_regions = |regions: &[MemoryRegion]| {
        regions.iter().find_map(|region| {
            if !region.readable || addr < region.base {
                return None;
            }
            let end = region.base.checked_add(region.size)?;
            if addr >= end {
                return None;
            }
            Some((end - addr).min(preferred_len as u64) as usize)
        })
    };
    if let Some(regions) = regions {
        if let Some(len) = from_regions(regions) {
            return len;
        }
    }
    from_regions(&provider.enumerate_regions()).unwrap_or(preferred_len)
}

fn readable_tail_len_with_lookup(
    provider: &dyn Provider,
    lookup: Option<&MemoryPreviewLookup<'_>>,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    preferred_len: usize,
) -> usize {
    if preferred_len == 0 {
        return 0;
    }
    if let Some(lookup) = lookup {
        if !lookup.regions.is_empty() {
            return lookup.readable_tail_len(addr, preferred_len).unwrap_or(0);
        }
    }
    readable_tail_len_cached(provider, regions, addr, preferred_len)
}

#[cfg(test)]
fn read_provider_bytes_best_effort(
    provider: &dyn Provider,
    addr: u64,
    preferred_len: usize,
    min_len: usize,
) -> Option<Vec<u8>> {
    read_provider_bytes_best_effort_cached(provider, None, addr, preferred_len, min_len)
}

#[cfg(test)]
fn read_provider_bytes_best_effort_cached(
    provider: &dyn Provider,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    preferred_len: usize,
    min_len: usize,
) -> Option<Vec<u8>> {
    let cap = readable_tail_len_cached(provider, regions, addr, preferred_len);
    read_provider_bytes_best_effort_capped(provider, addr, cap, min_len)
}

fn read_provider_bytes_best_effort_with_lookup(
    provider: &dyn Provider,
    lookup: Option<&MemoryPreviewLookup<'_>>,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    preferred_len: usize,
    min_len: usize,
) -> Option<Vec<u8>> {
    let cap = readable_tail_len_with_lookup(provider, lookup, regions, addr, preferred_len);
    read_provider_bytes_best_effort_capped(provider, addr, cap, min_len)
}

fn read_provider_bytes_best_effort_capped(
    provider: &dyn Provider,
    addr: u64,
    cap: usize,
    min_len: usize,
) -> Option<Vec<u8>> {
    if cap == 0 {
        return None;
    }
    let min_len = min_len.max(1).min(cap);
    let mut scratch = vec![0u8; cap];
    if provider.read(addr, &mut scratch) {
        return Some(scratch);
    }
    let mut lo = min_len;
    let mut hi = cap.saturating_sub(1);
    let mut best_len = 0usize;
    while lo <= hi {
        let len = lo + (hi - lo) / 2;
        if provider.read(addr, &mut scratch[..len]) {
            best_len = len;
            lo = len + 1;
        } else if len == 0 {
            break;
        } else {
            hi = len - 1;
        }
    }
    if best_len >= min_len {
        scratch.truncate(best_len);
        Some(scratch)
    } else {
        None
    }
}

fn target_is_readable(provider: &dyn Provider, addr: u64, min_len: i32) -> bool {
    target_is_readable_cached(provider, None, addr, min_len)
}

fn target_is_readable_cached(
    provider: &dyn Provider,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    min_len: i32,
) -> bool {
    target_is_readable_with_lookup(provider, None, regions, addr, min_len)
}

fn target_is_readable_with_lookup(
    provider: &dyn Provider,
    lookup: Option<&MemoryPreviewLookup<'_>>,
    regions: Option<&[MemoryRegion]>,
    addr: u64,
    min_len: i32,
) -> bool {
    if addr == 0 {
        return false;
    }
    let len = min_len.max(1);
    provider_can_read_with_lookup(provider, lookup, regions, addr, len) && {
        let mut probe = vec![0u8; len as usize];
        provider.read(addr, &mut probe)
    }
}

#[cfg(test)]
fn memory_preview_rows(
    provider: &dyn Provider,
    pointer_size: i32,
    row_count: usize,
    bytes: &[u8],
) -> Vec<MemoryPreviewRow> {
    let regions = provider.enumerate_regions();
    let modules = provider.enumerate_modules();
    memory_preview_rows_with_maps(provider, pointer_size, row_count, bytes, &regions, &modules)
}

fn memory_preview_rows_with_maps(
    provider: &dyn Provider,
    pointer_size: i32,
    row_count: usize,
    bytes: &[u8],
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
) -> Vec<MemoryPreviewRow> {
    let region_order = region_lookup_order(regions);
    let module_order = module_lookup_order(modules);
    let lookup = MemoryPreviewLookup::with_orders(regions, modules, &region_order, &module_order);
    memory_preview_rows_with_lookup(provider, pointer_size, row_count, bytes, &lookup)
}

fn memory_preview_rows_with_lookup(
    provider: &dyn Provider,
    pointer_size: i32,
    row_count: usize,
    bytes: &[u8],
    lookup: &MemoryPreviewLookup<'_>,
) -> Vec<MemoryPreviewRow> {
    let stride = pointer_size.max(1) as usize;
    let kind = if pointer_size <= 4 { "hex32" } else { "hex64" };
    let mut type_hint_cache = LastTypeHint::default();
    let mut symbol_cache = LastSymbolName::default();
    (0..row_count)
        .map(|row| {
            let start = row.saturating_mul(stride);
            let chunk = bytes
                .get(start..start.saturating_add(stride).min(bytes.len()))
                .unwrap_or(&[]);
            let mut hex = String::with_capacity(stride * 3);
            let mut ascii = String::with_capacity(stride);
            for i in 0..stride {
                if let Some(b) = chunk.get(i).copied() {
                    if i > 0 {
                        hex.push(' ');
                    }
                    push_hex_byte(&mut hex, b);
                    ascii.push(if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    });
                } else {
                    if i > 0 {
                        hex.push(' ');
                    }
                    hex.push_str("  ");
                    ascii.push(' ');
                }
            }
            let type_hint = type_hint_cache.get(chunk, pointer_size);
            let pointer_note = memory_preview_pointer_note(
                provider,
                lookup,
                chunk,
                pointer_size,
                &mut symbol_cache,
            );
            MemoryPreviewRow {
                offset: start as u64,
                kind,
                ascii,
                hex: pad_hex_field(hex, stride * 3 - 1),
                type_hint,
                pointer_note,
            }
        })
        .collect()
}

#[doc(hidden)]
pub fn bench_memory_preview_rows_with_maps(
    provider: &dyn Provider,
    pointer_size: i32,
    row_count: usize,
    bytes: &[u8],
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
) -> usize {
    memory_preview_rows_with_maps(provider, pointer_size, row_count, bytes, regions, modules)
        .into_iter()
        .map(|row| {
            row.hex.len()
                + row.ascii.len()
                + row.type_hint.as_deref().map_or(0, str::len)
                + row.pointer_note.as_deref().map_or(0, str::len)
        })
        .sum()
}

#[doc(hidden)]
pub struct BenchMemoryPreviewLookup {
    region_order: MemoryPreviewLookupOrder,
    module_order: MemoryPreviewLookupOrder,
}

#[doc(hidden)]
pub struct BenchPointerMemoryPreviewRowCache {
    target_addr: u64,
    pointer_size: i32,
    row_count: usize,
    bytes: Vec<u8>,
    rows: Arc<[MemoryPreviewRow]>,
}

#[doc(hidden)]
pub fn bench_memory_preview_lookup_orders(
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
) -> BenchMemoryPreviewLookup {
    BenchMemoryPreviewLookup {
        region_order: region_lookup_order(regions),
        module_order: module_lookup_order(modules),
    }
}

#[doc(hidden)]
pub fn bench_memory_preview_rows_with_cached_lookup(
    provider: &dyn Provider,
    pointer_size: i32,
    row_count: usize,
    bytes: &[u8],
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
    orders: &BenchMemoryPreviewLookup,
) -> usize {
    let lookup = MemoryPreviewLookup::with_orders(
        regions,
        modules,
        &orders.region_order,
        &orders.module_order,
    );
    memory_preview_rows_with_lookup(provider, pointer_size, row_count, bytes, &lookup)
        .into_iter()
        .map(|row| memory_preview_row_score(&row))
        .sum()
}

#[doc(hidden)]
pub fn bench_pointer_memory_preview_with_maps(
    provider: &dyn Provider,
    pointer_addr: u64,
    pointer_size: i32,
    row_count: usize,
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
) -> usize {
    let region_order = region_lookup_order(regions);
    let module_order = module_lookup_order(modules);
    let lookup = MemoryPreviewLookup::with_orders(regions, modules, &region_order, &module_order);
    if !provider_can_read_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        pointer_addr,
        pointer_size,
    ) {
        return 0;
    }
    let target = if pointer_size <= 4 {
        u64::from(provider.read_u32(pointer_addr))
    } else {
        provider.read_u64(pointer_addr)
    };
    if target == 0 {
        return 0;
    }
    let len = row_count.saturating_mul(pointer_size.max(1) as usize);
    let Some(bytes) = read_provider_bytes_best_effort_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        target,
        len,
        1,
    ) else {
        return 0;
    };
    memory_preview_rows_with_lookup(provider, pointer_size, row_count, &bytes, &lookup)
        .into_iter()
        .map(|row| memory_preview_row_score(&row))
        .sum()
}

#[doc(hidden)]
pub fn bench_pointer_memory_preview_with_cached_lookup(
    provider: &dyn Provider,
    pointer_addr: u64,
    pointer_size: i32,
    row_count: usize,
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
    orders: &BenchMemoryPreviewLookup,
) -> usize {
    let lookup = MemoryPreviewLookup::with_orders(
        regions,
        modules,
        &orders.region_order,
        &orders.module_order,
    );
    if !provider_can_read_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        pointer_addr,
        pointer_size,
    ) {
        return 0;
    }
    let target = if pointer_size <= 4 {
        u64::from(provider.read_u32(pointer_addr))
    } else {
        provider.read_u64(pointer_addr)
    };
    if target == 0 {
        return 0;
    }
    let len = row_count.saturating_mul(pointer_size.max(1) as usize);
    let Some(bytes) = read_provider_bytes_best_effort_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        target,
        len,
        1,
    ) else {
        return 0;
    };
    memory_preview_rows_with_lookup(provider, pointer_size, row_count, &bytes, &lookup)
        .into_iter()
        .map(|row| memory_preview_row_score(&row))
        .sum()
}

#[doc(hidden)]
pub fn bench_pointer_memory_preview_with_row_cache(
    provider: &dyn Provider,
    pointer_addr: u64,
    pointer_size: i32,
    row_count: usize,
    regions: &[MemoryRegion],
    modules: &[ModuleEntry],
    orders: &BenchMemoryPreviewLookup,
    cache: &mut Option<BenchPointerMemoryPreviewRowCache>,
) -> usize {
    let lookup = MemoryPreviewLookup::with_orders(
        regions,
        modules,
        &orders.region_order,
        &orders.module_order,
    );
    if !provider_can_read_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        pointer_addr,
        pointer_size,
    ) {
        return 0;
    }
    let target = if pointer_size <= 4 {
        u64::from(provider.read_u32(pointer_addr))
    } else {
        provider.read_u64(pointer_addr)
    };
    if target == 0 {
        return 0;
    }
    let len = row_count.saturating_mul(pointer_size.max(1) as usize);
    let Some(bytes) = read_provider_bytes_best_effort_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        target,
        len,
        1,
    ) else {
        return 0;
    };
    if let Some(cache) = cache.as_ref() {
        if cache.target_addr == target
            && cache.pointer_size == pointer_size
            && cache.row_count == row_count
            && cache.bytes == bytes
        {
            return memory_preview_rows_score(&cache.rows);
        }
    }
    let rows: Arc<[MemoryPreviewRow]> =
        memory_preview_rows_with_lookup(provider, pointer_size, row_count, &bytes, &lookup).into();
    let total = memory_preview_rows_score(&rows);
    *cache = Some(BenchPointerMemoryPreviewRowCache {
        target_addr: target,
        pointer_size,
        row_count,
        bytes,
        rows,
    });
    total
}

#[inline]
fn memory_preview_rows_score(rows: &[MemoryPreviewRow]) -> usize {
    rows.iter().map(memory_preview_row_score).sum()
}

#[inline]
fn memory_preview_row_score(row: &MemoryPreviewRow) -> usize {
    row.hex.len()
        + row.ascii.len()
        + row.type_hint.as_deref().map_or(0, str::len)
        + row.pointer_note.as_deref().map_or(0, str::len)
}

#[doc(hidden)]
pub fn bench_struct_preview_preflight_reenumerate(
    provider: &dyn Provider,
    pointer_addr: u64,
    pointer_size: i32,
) -> usize {
    if !provider_can_read(provider, pointer_addr, pointer_size) {
        return 0;
    }
    let target = if pointer_size <= 4 {
        u64::from(provider.read_u32(pointer_addr))
    } else {
        provider.read_u64(pointer_addr)
    };
    if !target_is_readable(provider, target, 1) {
        return 0;
    }
    target as usize
}

#[doc(hidden)]
pub fn bench_struct_preview_preflight_with_regions(
    provider: &dyn Provider,
    pointer_addr: u64,
    pointer_size: i32,
    regions: &[MemoryRegion],
) -> usize {
    let region_order = region_lookup_order(regions);
    let modules: &[ModuleEntry] = &[];
    let module_order = module_lookup_order(modules);
    let lookup = MemoryPreviewLookup::with_orders(regions, modules, &region_order, &module_order);
    if !provider_can_read_with_lookup(
        provider,
        Some(&lookup),
        Some(regions),
        pointer_addr,
        pointer_size,
    ) {
        return 0;
    }
    let target = if pointer_size <= 4 {
        u64::from(provider.read_u32(pointer_addr))
    } else {
        provider.read_u64(pointer_addr)
    };
    if target == 0
        || !provider_can_read_with_lookup(provider, Some(&lookup), Some(regions), target, 1)
    {
        return 0;
    }
    let mut probe = [0u8; 1];
    if !provider.read(target, &mut probe) {
        return 0;
    }
    (target as usize).wrapping_add(probe[0] as usize)
}

fn memory_preview_type_hint(chunk: &[u8], pointer_size: i32) -> Option<String> {
    if chunk.is_empty() || chunk.iter().all(|&b| b == 0) {
        return None;
    }
    let hints = InferHints {
        ptr_size: pointer_size,
        ..Default::default()
    };
    let suggestions = infer_strong_types(chunk, &hints, 2);
    let mut out = String::new();
    let mut emitted = 0usize;
    for suggestion in &suggestions {
        if emitted >= 2 {
            break;
        }
        if emitted > 0 {
            out.push_str(" | ");
        }
        let type_name = format_hint(suggestion);
        let preview = memory_preview_format_hint(chunk, chunk.len() as i32, &suggestion.kinds);
        if preview.is_empty() {
            out.push('[');
            out.push_str(&type_name);
            out.push(']');
        } else {
            out.push_str(&preview);
            out.push_str(" [");
            out.push_str(&type_name);
            out.push(']');
        }
        emitted += 1;
    }
    (emitted > 0).then_some(out)
}

#[derive(Default)]
struct LastTypeHint {
    pointer_size: i32,
    len: usize,
    key: u128,
    value: Option<String>,
    valid: bool,
    cached: bool,
}

impl LastTypeHint {
    fn get(&mut self, chunk: &[u8], pointer_size: i32) -> Option<String> {
        let key = Self::key(chunk);
        if self.valid
            && self.pointer_size == pointer_size
            && self.len == chunk.len()
            && key.is_some_and(|key| self.key == key)
        {
            if self.cached {
                return self.value.clone();
            }

            let value = memory_preview_type_hint(chunk, pointer_size);
            self.value = value.clone();
            self.cached = true;
            return value;
        }

        let value = memory_preview_type_hint(chunk, pointer_size);
        if let Some(key) = key {
            self.pointer_size = pointer_size;
            self.len = chunk.len();
            self.key = key;
            self.value = None;
            self.valid = true;
            self.cached = false;
        } else {
            self.valid = false;
            self.value = None;
            self.cached = false;
        }
        value
    }

    fn key(chunk: &[u8]) -> Option<u128> {
        match chunk.len() {
            0 => Some(0),
            1..=8 => {
                let mut bytes = [0u8; 8];
                bytes[..chunk.len()].copy_from_slice(chunk);
                Some(u64::from_le_bytes(bytes) as u128)
            }
            9..=16 => {
                let mut bytes = [0u8; 16];
                bytes[..chunk.len()].copy_from_slice(chunk);
                Some(u128::from_le_bytes(bytes))
            }
            _ => None,
        }
    }
}

fn memory_preview_pointer_note(
    provider: &dyn Provider,
    lookup: &MemoryPreviewLookup<'_>,
    chunk: &[u8],
    pointer_size: i32,
    symbol_cache: &mut LastSymbolName,
) -> Option<String> {
    let target = match pointer_size {
        4 if chunk.len() >= 4 => {
            u64::from(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        }
        8 if chunk.len() >= 8 => u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]),
        _ => return None,
    };
    if target == 0 || target == u64::MAX || (pointer_size == 4 && target == 0xFFFF_FFFF) {
        return None;
    }
    if !lookup.is_readable(target, 1) {
        return None;
    }
    Some(memory_preview_named_address(
        provider,
        lookup,
        target,
        symbol_cache,
    ))
}

fn region_contains_readable(region: &MemoryRegion, addr: u64, len: i32) -> bool {
    region.readable
        && len >= 0
        && addr >= region.base
        && addr
            .checked_add(len as u64)
            .is_some_and(|end| end <= region.base.saturating_add(region.size))
}

fn memory_preview_named_address(
    provider: &dyn Provider,
    lookup: &MemoryPreviewLookup<'_>,
    addr: u64,
    symbol_cache: &mut LastSymbolName,
) -> String {
    let symbol = symbol_cache.get(provider, addr);
    if !symbol.is_empty() {
        return prefixed_pointer_note(symbol);
    }
    if let Some(module) = lookup.find_module(addr) {
        let name = if module.name.is_empty() {
            "module"
        } else {
            module.name.as_str()
        };
        return format!("-> {name}+0x{:X}", addr.saturating_sub(module.base));
    }
    if let Some(region) = lookup.find_region(addr, 1) {
        let tag = match region.region_type {
            RegionType::Image => "<DATA>",
            RegionType::Mapped => "<MAPPED>",
            RegionType::Private => {
                if region.writable {
                    "<HEAP>"
                } else {
                    "<PRIVATE>"
                }
            }
        };
        if region.module_name.is_empty() {
            return format!("-> {tag}0x{addr:X}");
        }
        return format!("-> {tag}{}.0x{addr:X}", region.module_name);
    }
    format!("-> 0x{addr:X}")
}

#[derive(Default)]
struct LastSymbolName {
    addr: u64,
    symbol: String,
    valid: bool,
}

impl LastSymbolName {
    fn get(&mut self, provider: &dyn Provider, addr: u64) -> &str {
        if self.valid && self.addr == addr {
            return &self.symbol;
        }
        self.addr = addr;
        self.symbol = provider.get_symbol(addr);
        self.valid = true;
        &self.symbol
    }
}

fn prefixed_pointer_note(label: &str) -> String {
    let mut out = String::with_capacity(label.len() + 3);
    out.push_str("-> ");
    out.push_str(label);
    out
}

fn push_hex_byte(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0F) as usize] as char);
}

fn pad_hex_field(mut hex: String, width: usize) -> String {
    if hex.len() < width {
        hex.extend(std::iter::repeat_n(' ', width - hex.len()));
    }
    hex
}

struct MemoryPreviewLookup<'a> {
    regions: &'a [MemoryRegion],
    modules: &'a [ModuleEntry],
    region_order: &'a MemoryPreviewLookupOrder,
    module_order: &'a MemoryPreviewLookupOrder,
    last_region_pos: Cell<usize>,
    last_module_pos: Cell<usize>,
}

enum MemoryPreviewLookupOrder {
    Input,
    Sorted(Vec<usize>),
    Linear,
}

impl<'a> MemoryPreviewLookup<'a> {
    const NO_POS: usize = usize::MAX;

    fn with_orders(
        regions: &'a [MemoryRegion],
        modules: &'a [ModuleEntry],
        region_order: &'a MemoryPreviewLookupOrder,
        module_order: &'a MemoryPreviewLookupOrder,
    ) -> Self {
        Self {
            regions,
            modules,
            region_order,
            module_order,
            last_region_pos: Cell::new(Self::NO_POS),
            last_module_pos: Cell::new(Self::NO_POS),
        }
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        self.find_region(addr, len).is_some()
    }

    fn find_region(&self, addr: u64, len: i32) -> Option<&'a MemoryRegion> {
        match &self.region_order {
            MemoryPreviewLookupOrder::Input => {
                if let Some(region) = self.find_region_near_cursor(addr, len) {
                    return Some(region);
                }
                let pos = self.regions.partition_point(|r| r.base <= addr);
                let pos = pos.checked_sub(1)?;
                let region = self.regions.get(pos)?;
                if region_contains_readable(region, addr, len) {
                    self.last_region_pos.set(pos);
                    Some(region)
                } else {
                    None
                }
            }
            MemoryPreviewLookupOrder::Sorted(indices) => {
                if let Some(region) = self.find_region_near_cursor(addr, len) {
                    return Some(region);
                }
                let pos = indices.partition_point(|&i| self.regions[i].base <= addr);
                let pos = pos.checked_sub(1)?;
                let region = indices.get(pos).and_then(|&i| self.regions.get(i))?;
                if region_contains_readable(region, addr, len) {
                    self.last_region_pos.set(pos);
                    Some(region)
                } else {
                    None
                }
            }
            MemoryPreviewLookupOrder::Linear => self
                .regions
                .iter()
                .find(|region| region_contains_readable(region, addr, len)),
        }
    }

    fn readable_tail_len(&self, addr: u64, preferred_len: usize) -> Option<usize> {
        let region = self.find_region(addr, 1)?;
        let end = region.base.checked_add(region.size)?;
        (addr < end).then(|| (end - addr).min(preferred_len as u64) as usize)
    }

    fn find_module(&self, addr: u64) -> Option<&'a ModuleEntry> {
        match &self.module_order {
            MemoryPreviewLookupOrder::Input => {
                if let Some(module) = self.find_module_near_cursor(addr) {
                    return Some(module);
                }
                let pos = self.modules.partition_point(|m| m.base <= addr);
                let pos = pos.checked_sub(1)?;
                let module = self.modules.get(pos)?;
                if module_contains(module, addr) {
                    self.last_module_pos.set(pos);
                    Some(module)
                } else {
                    None
                }
            }
            MemoryPreviewLookupOrder::Sorted(indices) => {
                if let Some(module) = self.find_module_near_cursor(addr) {
                    return Some(module);
                }
                let pos = indices.partition_point(|&i| self.modules[i].base <= addr);
                let pos = pos.checked_sub(1)?;
                let module = indices.get(pos).and_then(|&i| self.modules.get(i))?;
                if module_contains(module, addr) {
                    self.last_module_pos.set(pos);
                    Some(module)
                } else {
                    None
                }
            }
            MemoryPreviewLookupOrder::Linear => self
                .modules
                .iter()
                .find(|module| module_contains(module, addr)),
        }
    }

    fn find_region_near_cursor(&self, addr: u64, len: i32) -> Option<&'a MemoryRegion> {
        let pos = self.last_region_pos.get();
        let Some(region) = self.region_at_order_pos(pos) else {
            return None;
        };
        if region_contains_readable(region, addr, len) {
            return Some(region);
        }
        if addr < region.base {
            self.last_region_pos.set(Self::NO_POS);
            return None;
        }
        if addr >= region.base.saturating_add(region.size) {
            let next_pos = pos.saturating_add(1);
            let next = self.region_at_order_pos(next_pos)?;
            if region_contains_readable(next, addr, len) {
                self.last_region_pos.set(next_pos);
                return Some(next);
            }
        }
        None
    }

    fn region_at_order_pos(&self, pos: usize) -> Option<&'a MemoryRegion> {
        match self.region_order {
            MemoryPreviewLookupOrder::Input => self.regions.get(pos),
            MemoryPreviewLookupOrder::Sorted(indices) => {
                indices.get(pos).and_then(|&i| self.regions.get(i))
            }
            MemoryPreviewLookupOrder::Linear => None,
        }
    }

    fn find_module_near_cursor(&self, addr: u64) -> Option<&'a ModuleEntry> {
        let pos = self.last_module_pos.get();
        let Some(module) = self.module_at_order_pos(pos) else {
            return None;
        };
        if module_contains(module, addr) {
            return Some(module);
        }
        if addr < module.base {
            self.last_module_pos.set(Self::NO_POS);
            return None;
        }
        if addr >= module.base.saturating_add(module.size) {
            let next_pos = pos.saturating_add(1);
            let next = self.module_at_order_pos(next_pos)?;
            if module_contains(next, addr) {
                self.last_module_pos.set(next_pos);
                return Some(next);
            }
        }
        None
    }

    fn module_at_order_pos(&self, pos: usize) -> Option<&'a ModuleEntry> {
        match self.module_order {
            MemoryPreviewLookupOrder::Input => self.modules.get(pos),
            MemoryPreviewLookupOrder::Sorted(indices) => {
                indices.get(pos).and_then(|&i| self.modules.get(i))
            }
            MemoryPreviewLookupOrder::Linear => None,
        }
    }
}

fn region_lookup_order(regions: &[MemoryRegion]) -> MemoryPreviewLookupOrder {
    if memory_regions_non_overlapping_in_order(regions) {
        return MemoryPreviewLookupOrder::Input;
    }
    let mut indices: Vec<usize> = (0..regions.len()).collect();
    indices.sort_unstable_by_key(|&i| regions[i].base);
    if memory_regions_non_overlapping_by_index(regions, &indices) {
        MemoryPreviewLookupOrder::Sorted(indices)
    } else {
        MemoryPreviewLookupOrder::Linear
    }
}

fn module_lookup_order(modules: &[ModuleEntry]) -> MemoryPreviewLookupOrder {
    if modules_non_overlapping_in_order(modules) {
        return MemoryPreviewLookupOrder::Input;
    }
    let mut indices: Vec<usize> = (0..modules.len()).collect();
    indices.sort_unstable_by_key(|&i| modules[i].base);
    if modules_non_overlapping_by_index(modules, &indices) {
        MemoryPreviewLookupOrder::Sorted(indices)
    } else {
        MemoryPreviewLookupOrder::Linear
    }
}

fn memory_regions_non_overlapping_in_order(regions: &[MemoryRegion]) -> bool {
    regions.windows(2).all(|pair| {
        let left_end = pair[0].base.saturating_add(pair[0].size);
        left_end <= pair[1].base
    })
}

fn memory_regions_non_overlapping_by_index(regions: &[MemoryRegion], indices: &[usize]) -> bool {
    indices.windows(2).all(|pair| {
        let left = &regions[pair[0]];
        let right = &regions[pair[1]];
        left.base.saturating_add(left.size) <= right.base
    })
}

fn modules_non_overlapping_in_order(modules: &[ModuleEntry]) -> bool {
    modules.windows(2).all(|pair| {
        let left_end = pair[0].base.saturating_add(pair[0].size);
        left_end <= pair[1].base
    })
}

fn modules_non_overlapping_by_index(modules: &[ModuleEntry], indices: &[usize]) -> bool {
    indices.windows(2).all(|pair| {
        let left = &modules[pair[0]];
        let right = &modules[pair[1]];
        left.base.saturating_add(left.size) <= right.base
    })
}

fn module_contains(module: &ModuleEntry, addr: u64) -> bool {
    addr >= module.base
        && addr
            .checked_sub(module.base)
            .is_some_and(|rel| rel < module.size)
}

fn memory_preview_format_hint(data: &[u8], len: i32, kinds: &[NodeKind]) -> String {
    use crate::format as fmt;

    let Some(&k) = kinds.first() else {
        return String::new();
    };
    let len = len.max(0).min(data.len() as i32);

    let load_u16 =
        |d: &[u8]| -> Option<u16> { (d.len() >= 2).then(|| u16::from_le_bytes([d[0], d[1]])) };
    let load_u32 = |d: &[u8]| -> Option<u32> {
        (d.len() >= 4).then(|| u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    };
    let load_u64 = |d: &[u8]| -> Option<u64> {
        (d.len() >= 8).then(|| u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]))
    };

    if kinds.len() == 1 {
        return match k {
            NodeKind::Float => load_u32(data)
                .map(|v| fmt::fmt_float(f32::from_bits(v)))
                .unwrap_or_default(),
            NodeKind::Double => load_u64(data)
                .map(|v| fmt::fmt_double(f64::from_bits(v)))
                .unwrap_or_default(),
            NodeKind::Int32 => load_u32(data)
                .map(|v| fmt::fmt_int32(v as i32))
                .unwrap_or_default(),
            NodeKind::UInt32 => load_u32(data).map(fmt::fmt_uint32).unwrap_or_default(),
            NodeKind::Int16 => load_u16(data)
                .map(|v| fmt::fmt_int16(v as i16))
                .unwrap_or_default(),
            NodeKind::UInt16 => load_u16(data).map(fmt::fmt_uint16).unwrap_or_default(),
            NodeKind::Int64 => load_u64(data)
                .map(|v| fmt::fmt_int64(v as i64))
                .unwrap_or_default(),
            NodeKind::UInt64 => load_u64(data).map(fmt::fmt_uint64).unwrap_or_default(),
            NodeKind::Pointer64 => load_u64(data).map(fmt::fmt_pointer64).unwrap_or_default(),
            NodeKind::Pointer32 => load_u32(data).map(fmt::fmt_pointer32).unwrap_or_default(),
            NodeKind::Bool => data.first().map(|&v| fmt::fmt_bool(v)).unwrap_or_default(),
            NodeKind::UTF8 => {
                let n = len.min(8) as usize;
                let mut s = String::new();
                for &c in data.iter().take(n) {
                    if (0x20..=0x7E).contains(&c) {
                        s.push(c as char);
                    } else {
                        break;
                    }
                }
                if s.is_empty() {
                    String::new()
                } else {
                    format!("\"{s}\"")
                }
            }
            _ => String::new(),
        };
    }

    let part_sz = len / kinds.len() as i32;
    if part_sz <= 0 {
        return String::new();
    }
    let part_sz_usize = part_sz as usize;
    let mut out = String::new();
    for (i, &lane) in kinds.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let start = i.saturating_mul(part_sz_usize);
        let end = start.saturating_add(part_sz_usize).min(data.len());
        let slice = data.get(start..end).unwrap_or(&[]);
        out.push_str(&memory_preview_format_hint(
            slice,
            part_sz,
            std::slice::from_ref(&lane),
        ));
    }
    out
}

fn semantic_hint_children(
    text: &str,
    pointer_hint: bool,
    palette: &EditorPalette,
) -> Vec<AnyElement> {
    let spans = super::geometry::type_hint_semantic_spans(text, pointer_hint);
    styled_hint_children(text, &spans, palette)
}

fn pointer_note_children(text: &str, palette: &EditorPalette) -> Vec<AnyElement> {
    semantic_hint_children(text, true, palette)
}

fn styled_hint_children(
    text: &str,
    spans: &[super::geometry::SpanStyle],
    palette: &EditorPalette,
) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for span in spans {
        let start = super::geometry::byte_for_col(text, span.start);
        let end = super::geometry::byte_for_col(text, span.end);
        if end <= start {
            continue;
        }
        out.push(
            div()
                .text_color(palette.role_color(span.role))
                .child(text[start..end].to_string())
                .into_any_element(),
        );
    }
    if out.is_empty() && !text.is_empty() {
        out.push(
            div()
                .text_color(palette.type_hint)
                .child(text.to_string())
                .into_any_element(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        compact_number, fmt_elapsed_compact, hex_pointer_value_active, history_header,
        hover_kind_eq, memory_preview_rows, memory_preview_rows_with_maps,
        memory_preview_type_hint, read_provider_bytes, read_provider_bytes_best_effort,
        read_provider_bytes_best_effort_cached, recency_tier, target_is_readable,
        target_is_readable_cached, try_parse_int, HoverMemoryMaps, HoverMemoryPreviewCache,
        HoverPopupKind, LastTypeHint, MemoryPreviewRow,
    };
    use crate::compose::ColumnSpan;
    use crate::core::{ChipKind, LineChip, LineMeta, NodeKind};
    use crate::provider::{MemoryRegion, ModuleEntry, Provider, RegionType};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const S: i64 = 1000;
    const M: i64 = 60 * S;
    const H: i64 = 60 * M;
    const D: i64 = 24 * H;
    const W: i64 = 7 * D;

    struct TestProvider {
        base: u64,
        data: Vec<u8>,
        fail_reads: bool,
    }

    impl Provider for TestProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if self.fail_reads {
                return false;
            }
            let Some(start) = addr.checked_sub(self.base).map(|v| v as usize) else {
                return false;
            };
            if start + buf.len() > self.data.len() {
                return false;
            }
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
            true
        }

        fn size(&self) -> i32 {
            self.data.len() as i32
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            vec![MemoryRegion {
                base: self.base,
                size: self.data.len() as u64,
                readable: true,
                writable: false,
                executable: false,
                module_name: "test".into(),
                region_type: RegionType::Private,
            }]
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            false
        }
    }

    struct CountingProvider {
        inner: TestProvider,
        read_calls: Cell<usize>,
        region_calls: Cell<usize>,
        module_calls: Cell<usize>,
    }

    impl CountingProvider {
        fn new(base: u64, data: Vec<u8>) -> Self {
            CountingProvider {
                inner: TestProvider {
                    base,
                    data,
                    fail_reads: false,
                },
                read_calls: Cell::new(0),
                region_calls: Cell::new(0),
                module_calls: Cell::new(0),
            }
        }
    }

    struct CombinedCountingProvider {
        inner: TestProvider,
        region_calls: AtomicUsize,
        module_calls: AtomicUsize,
        combined_calls: AtomicUsize,
    }

    impl CombinedCountingProvider {
        fn new(base: u64, data: Vec<u8>) -> Self {
            Self {
                inner: TestProvider {
                    base,
                    data,
                    fail_reads: false,
                },
                region_calls: AtomicUsize::new(0),
                module_calls: AtomicUsize::new(0),
                combined_calls: AtomicUsize::new(0),
            }
        }

        fn modules(&self) -> Vec<ModuleEntry> {
            vec![ModuleEntry {
                name: "combinedmod".into(),
                full_path: String::new(),
                base: self.inner.base,
                size: self.inner.data.len() as u64,
            }]
        }
    }

    struct SymbolCountingProvider {
        inner: TestProvider,
        symbol_calls: Cell<usize>,
    }

    impl SymbolCountingProvider {
        fn new(base: u64, data: Vec<u8>) -> Self {
            Self {
                inner: TestProvider {
                    base,
                    data,
                    fail_reads: false,
                },
                symbol_calls: Cell::new(0),
            }
        }
    }

    impl Provider for SymbolCountingProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.inner.read(addr, buf)
        }

        fn size(&self) -> i32 {
            self.inner.size()
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.inner.enumerate_regions()
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            vec![ModuleEntry {
                name: "symmod".into(),
                full_path: String::new(),
                base: self.inner.base,
                size: self.inner.data.len() as u64,
            }]
        }

        fn get_symbol(&self, addr: u64) -> String {
            self.symbol_calls.set(self.symbol_calls.get() + 1);
            format!("sym_{:X}", addr.saturating_sub(self.inner.base))
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            false
        }
    }

    impl Provider for CountingProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.read_calls.set(self.read_calls.get() + 1);
            self.inner.read(addr, buf)
        }

        fn size(&self) -> i32 {
            self.inner.size()
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.region_calls.set(self.region_calls.get() + 1);
            self.inner.enumerate_regions()
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.module_calls.set(self.module_calls.get() + 1);
            vec![ModuleEntry {
                name: "benchmod".into(),
                full_path: String::new(),
                base: self.inner.base,
                size: self.inner.data.len() as u64,
            }]
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            false
        }
    }

    impl Provider for CombinedCountingProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.inner.read(addr, buf)
        }

        fn size(&self) -> i32 {
            self.inner.size()
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.region_calls.fetch_add(1, Ordering::Relaxed);
            self.inner.enumerate_regions()
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.module_calls.fetch_add(1, Ordering::Relaxed);
            self.modules()
        }

        fn enumerate_modules_and_regions(&self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
            self.combined_calls.fetch_add(1, Ordering::Relaxed);
            (self.modules(), self.inner.enumerate_regions())
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            false
        }
    }

    struct PartialTailProvider {
        data: Vec<u8>,
        read_calls: Cell<usize>,
    }

    impl PartialTailProvider {
        fn new(len: usize) -> Self {
            let mut data = vec![0u8; len];
            for (i, b) in data.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(11).wrapping_add(7);
            }
            Self {
                data,
                read_calls: Cell::new(0),
            }
        }
    }

    impl Provider for PartialTailProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.read_calls.set(self.read_calls.get() + 1);
            let start = addr as usize;
            let Some(end) = start.checked_add(buf.len()) else {
                return false;
            };
            if end > self.data.len() {
                return false;
            }
            buf.copy_from_slice(&self.data[start..end]);
            true
        }

        fn size(&self) -> i32 {
            self.data.len() as i32
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            Vec::new()
        }
    }

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
    fn target_readability_requires_region_and_real_read_success() {
        let ok = TestProvider {
            base: 0x1000,
            data: vec![1, 2, 3, 4],
            fail_reads: false,
        };
        assert!(target_is_readable(&ok, 0x1000, 4));
        assert_eq!(read_provider_bytes(&ok, 0x1001, 2), Some(vec![2, 3]));
        assert!(!target_is_readable(&ok, 0x0FFF, 1));
        assert_eq!(read_provider_bytes(&ok, 0x0FFF, 1), None);

        let failing = TestProvider {
            base: 0x1000,
            data: vec![1, 2, 3, 4],
            fail_reads: true,
        };
        assert!(!target_is_readable(&failing, 0x1000, 1));
        assert_eq!(read_provider_bytes(&failing, 0x1000, 1), None);
    }

    #[test]
    fn preview_read_clamps_to_readable_region_tail() {
        let provider = TestProvider {
            base: 0x2000,
            data: vec![0xAA, 0xBB, 0xCC],
            fail_reads: false,
        };
        assert!(target_is_readable(&provider, 0x2002, 1));
        assert_eq!(read_provider_bytes(&provider, 0x2002, 8), None);
        assert_eq!(
            read_provider_bytes_best_effort(&provider, 0x2002, 8, 1),
            Some(vec![0xCC])
        );
    }

    #[test]
    fn hex_pointer_preview_uses_value_span_and_pointer_type_hint_chip() {
        let mut lm = LineMeta {
            node_kind: NodeKind::Hex64,
            ..Default::default()
        };
        let vs = ColumnSpan {
            start: 10,
            end: 24,
            valid: true,
        };
        assert!(
            hex_pointer_value_active(&lm, 12, vs),
            "hex value span qualifies"
        );
        assert!(
            !hex_pointer_value_active(&lm, 30, vs),
            "trailing text outside the value span is ignored"
        );

        lm.chips.push(LineChip {
            kind: ChipKind::TypeHint,
            start_col: 24,
            end_col: 39,
            text: "14, 20 [int32_t×2]".into(),
            type_hint_kinds: vec![NodeKind::Int32, NodeKind::Int32],
            ..Default::default()
        });
        assert!(
            hex_pointer_value_active(&lm, 12, vs),
            "hex value span still qualifies when a non-pointer chip follows"
        );
        assert!(
            !hex_pointer_value_active(&lm, 30, vs),
            "non-pointer TypeHint chip span is not a pointer preview target"
        );

        lm.chips.push(LineChip {
            kind: ChipKind::TypeHint,
            start_col: 40,
            end_col: 57,
            text: "ptr64\u{2713} -> module+0x10".into(),
            type_hint_kinds: vec![NodeKind::Pointer64],
            ..Default::default()
        });
        assert!(
            hex_pointer_value_active(&lm, 45, vs),
            "pointer TypeHint chip span qualifies as a preview target"
        );
    }

    #[test]
    fn memory_preview_type_hint_renders_pointer_predictions() {
        let data = [0x00, 0x10, 0xB0, 0xA0, 0xF6, 0x7F, 0x00, 0x00];
        let hint = memory_preview_type_hint(&data, 8);
        assert!(
            hint.as_deref().unwrap_or_default().contains("ptr64"),
            "memory preview should render passive pointer prediction: {hint:?}"
        );
    }

    #[test]
    fn memory_preview_type_hint_renders_generic_split_ints() {
        let mut data = [0u8; 8];
        data[0..4].copy_from_slice(&14i32.to_le_bytes());
        data[4..8].copy_from_slice(&20i32.to_le_bytes());
        let hint = memory_preview_type_hint(&data, 8);
        assert!(
            hint.as_deref().unwrap_or_default().contains("int32_t×2")
                && hint.as_deref().unwrap_or_default().contains("uint32_t×2"),
            "memory preview should render top-2 split-int predictions: {hint:?}"
        );
    }

    #[test]
    fn memory_preview_type_hint_cache_reuses_identical_chunks() {
        let data = 0x0000_7FF6_A0B0_1000u64.to_le_bytes();
        let mut cache = LastTypeHint::default();

        let first = cache.get(&data, 8);
        assert!(
            first.as_deref().unwrap_or_default().contains("ptr64"),
            "sanity check the cached value is a real pointer hint: {first:?}"
        );

        cache.value = Some("cached hint".to_string());
        cache.cached = true;
        assert_eq!(cache.get(&data, 8).as_deref(), Some("cached hint"));
        assert_ne!(cache.get(&data, 4).as_deref(), Some("cached hint"));
    }

    #[test]
    fn memory_preview_rows_are_pointer_sized() {
        let provider = TestProvider {
            base: 0x1000,
            data: vec![0u8; 0x100],
            fail_reads: false,
        };
        let rows = memory_preview_rows(&provider, 4, 2, &[0x41, 0x42, 0, 0x7F, 1, 2, 3, 4]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, 0);
        assert_eq!(rows[0].kind, "hex32");
        assert_eq!(rows[0].hex, "41 42 00 7F");
        assert_eq!(rows[0].ascii, "AB..");
        assert_eq!(rows[1].offset, 4);
        assert_eq!(rows[1].hex, "01 02 03 04");
    }

    #[test]
    fn memory_preview_rows_note_readable_pointer_values() {
        let provider = TestProvider {
            base: 0x2000,
            data: vec![0xAA],
            fail_reads: false,
        };
        let rows = memory_preview_rows(&provider, 8, 1, &0x2000u64.to_le_bytes());
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].pointer_note.as_deref(),
            Some("-> <PRIVATE>test.0x2000")
        );
    }

    #[test]
    fn memory_preview_pointer_notes_do_not_probe_each_live_target() {
        let provider = CountingProvider::new(0x2000, vec![0xAA]);
        let regions = provider.enumerate_regions();
        let modules = provider.enumerate_modules();

        let rows = memory_preview_rows_with_maps(
            &provider,
            8,
            1,
            &0x2000u64.to_le_bytes(),
            &regions,
            &modules,
        );

        assert_eq!(rows[0].pointer_note.as_deref(), Some("-> benchmod+0x0"));
        assert_eq!(
            provider.read_calls.get(),
            0,
            "pointer-note labels should use the fresh region/module maps, not per-row target reads"
        );
    }

    #[test]
    fn memory_preview_pointer_notes_memoize_duplicate_symbol_names() {
        let provider = SymbolCountingProvider::new(0x2000, vec![0xAA]);
        let regions = provider.enumerate_regions();
        let modules = provider.enumerate_modules();
        let mut bytes = Vec::new();
        for _ in 0..4 {
            bytes.extend_from_slice(&0x2000u64.to_le_bytes());
        }

        let rows = memory_preview_rows_with_maps(&provider, 8, 4, &bytes, &regions, &modules);

        assert_eq!(rows.len(), 4);
        assert!(rows
            .iter()
            .all(|row| row.pointer_note.as_deref() == Some("-> sym_0")));
        assert_eq!(
            provider.symbol_calls.get(),
            1,
            "duplicate pointer targets in one hover preview should share one symbol lookup"
        );
    }

    #[test]
    fn memory_preview_cached_helpers_reuse_region_and_module_maps() {
        let provider = CountingProvider::new(0x2000, vec![0xAA, 0xBB, 0xCC, 0xDD]);
        let regions = provider.enumerate_regions();
        let modules = provider.enumerate_modules();
        assert_eq!(provider.region_calls.get(), 1);
        assert_eq!(provider.module_calls.get(), 1);

        assert!(target_is_readable_cached(
            &provider,
            Some(&regions),
            0x2000,
            1
        ));
        assert_eq!(
            read_provider_bytes_best_effort_cached(&provider, Some(&regions), 0x2002, 8, 1),
            Some(vec![0xCC, 0xDD])
        );
        let rows = memory_preview_rows_with_maps(
            &provider,
            8,
            1,
            &0x2000u64.to_le_bytes(),
            &regions,
            &modules,
        );
        assert_eq!(rows[0].pointer_note.as_deref(), Some("-> benchmod+0x0"));
        assert_eq!(provider.region_calls.get(), 1);
        assert_eq!(provider.module_calls.get(), 1);
    }

    #[test]
    fn hover_memory_maps_are_scoped_to_provider_and_result_revision() {
        let provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x2000,
            data: vec![0xAA],
            fail_reads: false,
        });
        let other_provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x3000,
            data: vec![0xBB],
            fail_reads: false,
        });
        let regions = vec![
            MemoryRegion {
                base: 0x4000,
                size: 0x100,
                readable: true,
                writable: false,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            },
            MemoryRegion {
                base: 0x2000,
                size: 0x100,
                readable: true,
                writable: false,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            },
        ];
        let modules = vec![
            ModuleEntry {
                name: "late.dll".into(),
                full_path: String::new(),
                base: 0x8000,
                size: 0x100,
            },
            ModuleEntry {
                name: "early.dll".into(),
                full_path: String::new(),
                base: 0x6000,
                size: 0x100,
            },
        ];
        let maps = HoverMemoryMaps::with_regions_modules(11, &provider, regions, modules);

        assert!(maps.matches(11, &provider));
        assert!(!maps.matches(12, &provider));
        assert!(!maps.matches(11, &other_provider));

        let lookup = maps.lookup();
        assert!(lookup.is_readable(0x2008, 1));
        assert_eq!(
            lookup.find_module(0x6008).map(|m| m.name.as_str()),
            Some("early.dll")
        );
    }

    #[test]
    fn hover_memory_maps_use_combined_provider_snapshot_when_modules_are_needed() {
        let counted = Arc::new(CombinedCountingProvider::new(0x2000, vec![0xAA]));
        let provider: Arc<dyn Provider + Send + Sync> = counted.clone();

        let maps = HoverMemoryMaps::from_provider_with_modules(11, &provider);

        assert_eq!(counted.combined_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            counted.region_calls.load(Ordering::Relaxed),
            0,
            "fresh pointer hovers should not enumerate regions separately"
        );
        assert_eq!(
            counted.module_calls.load(Ordering::Relaxed),
            0,
            "fresh pointer hovers should not enumerate modules separately"
        );
        assert!(maps.modules_loaded);
        let lookup = maps.lookup();
        assert!(lookup.is_readable(0x2000, 1));
        assert_eq!(
            lookup.find_module(0x2000).map(|m| m.name.as_str()),
            Some("combinedmod")
        );
    }

    #[test]
    fn memory_preview_lookup_cursor_handles_sorted_region_and_module_maps() {
        let provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x1000,
            data: vec![0xAA],
            fail_reads: false,
        });
        let regions = vec![
            MemoryRegion {
                base: 0x3000,
                size: 0x100,
                readable: true,
                writable: false,
                executable: false,
                module_name: "three".into(),
                region_type: RegionType::Private,
            },
            MemoryRegion {
                base: 0x1000,
                size: 0x100,
                readable: true,
                writable: false,
                executable: false,
                module_name: "one".into(),
                region_type: RegionType::Private,
            },
            MemoryRegion {
                base: 0x2000,
                size: 0x100,
                readable: true,
                writable: false,
                executable: false,
                module_name: "two".into(),
                region_type: RegionType::Private,
            },
        ];
        let modules = vec![
            ModuleEntry {
                name: "three.dll".into(),
                full_path: String::new(),
                base: 0x3000,
                size: 0x100,
            },
            ModuleEntry {
                name: "one.dll".into(),
                full_path: String::new(),
                base: 0x1000,
                size: 0x100,
            },
            ModuleEntry {
                name: "two.dll".into(),
                full_path: String::new(),
                base: 0x2000,
                size: 0x100,
            },
        ];
        let maps = HoverMemoryMaps::with_regions_modules(11, &provider, regions, modules);
        let lookup = maps.lookup();

        assert_eq!(lookup.find_region(0x1008, 1).map(|r| r.base), Some(0x1000));
        assert_eq!(lookup.last_region_pos.get(), 0);
        assert_eq!(lookup.find_region(0x2008, 1).map(|r| r.base), Some(0x2000));
        assert_eq!(lookup.last_region_pos.get(), 1);
        assert_eq!(
            lookup.find_module(0x1008).map(|m| m.name.as_str()),
            Some("one.dll")
        );
        assert_eq!(lookup.last_module_pos.get(), 0);
        assert_eq!(
            lookup.find_module(0x2008).map(|m| m.name.as_str()),
            Some("two.dll")
        );
        assert_eq!(lookup.last_module_pos.get(), 1);
    }

    #[test]
    fn hover_memory_maps_can_upgrade_region_cache_with_modules() {
        let provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x2000,
            data: vec![0xAA],
            fail_reads: false,
        });
        let regions = vec![MemoryRegion {
            base: 0x2000,
            size: 0x100,
            readable: true,
            writable: false,
            executable: false,
            module_name: String::new(),
            region_type: RegionType::Private,
        }];
        let modules = vec![ModuleEntry {
            name: "target.dll".into(),
            full_path: String::new(),
            base: 0x2000,
            size: 0x100,
        }];
        let mut maps = HoverMemoryMaps::with_regions(11, &provider, regions);

        assert!(maps.matches(11, &provider));
        assert!(!maps.modules_loaded);

        maps.set_modules(modules);

        assert!(maps.modules_loaded);
        let lookup = maps.lookup();
        assert!(lookup.is_readable(0x2008, 1));
        assert_eq!(
            lookup.find_module(0x2008).map(|m| m.name.as_str()),
            Some("target.dll")
        );
    }

    #[test]
    fn hover_memory_preview_cache_is_scoped_to_live_identity_and_bytes() {
        let provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x2000,
            data: vec![0xAA],
            fail_reads: false,
        });
        let other_provider: Arc<dyn Provider + Send + Sync> = Arc::new(TestProvider {
            base: 0x2000,
            data: vec![0xAA],
            fail_reads: false,
        });
        let rows: Arc<[MemoryPreviewRow]> = vec![preview_row("AA")].into();
        let cache = HoverMemoryPreviewCache::new(11, &provider, 0x2000, 8, 10, vec![0xAA], rows);

        assert!(cache.matches(11, &provider, 0x2000, 8, 10, &[0xAA]));
        assert!(!cache.matches(12, &provider, 0x2000, 8, 10, &[0xAA]));
        assert!(!cache.matches(11, &other_provider, 0x2000, 8, 10, &[0xAA]));
        assert!(!cache.matches(11, &provider, 0x2008, 8, 10, &[0xAA]));
        assert!(!cache.matches(11, &provider, 0x2000, 8, 10, &[0xBB]));
    }

    #[test]
    fn best_effort_preview_finds_partial_tail_without_linear_retry() {
        let provider = PartialTailProvider::new(64);
        let bytes = read_provider_bytes_best_effort(&provider, 0, 4096, 1).unwrap();
        assert_eq!(bytes.len(), 64);
        assert!(
            provider.read_calls.get() <= 16,
            "expected logarithmic retry bound, got {} reads",
            provider.read_calls.get()
        );
    }

    #[test]
    fn memory_preview_lookup_handles_unsorted_region_and_module_maps() {
        let provider = TestProvider {
            base: 0x5000,
            data: vec![0xAA],
            fail_reads: false,
        };
        let regions = vec![
            MemoryRegion {
                base: 0x9000,
                size: 0x1000,
                readable: true,
                writable: false,
                executable: false,
                module_name: "later".into(),
                region_type: RegionType::Private,
            },
            MemoryRegion {
                base: 0x5000,
                size: 0x1000,
                readable: true,
                writable: false,
                executable: false,
                module_name: "target".into(),
                region_type: RegionType::Private,
            },
        ];
        let modules = vec![
            ModuleEntry {
                name: "later.dll".into(),
                full_path: String::new(),
                base: 0x9000,
                size: 0x1000,
            },
            ModuleEntry {
                name: "target.dll".into(),
                full_path: String::new(),
                base: 0x5000,
                size: 0x1000,
            },
        ];
        let rows = memory_preview_rows_with_maps(
            &provider,
            8,
            1,
            &0x5000u64.to_le_bytes(),
            &regions,
            &modules,
        );

        assert_eq!(rows[0].pointer_note.as_deref(), Some("-> target.dll+0x0"));
    }

    #[test]
    fn memory_preview_lookup_preserves_linear_order_for_overlapping_modules() {
        let provider = TestProvider {
            base: 0x5000,
            data: vec![0xAA],
            fail_reads: false,
        };
        let regions = vec![
            MemoryRegion {
                base: 0x4000,
                size: 0x3000,
                readable: true,
                writable: false,
                executable: false,
                module_name: "wide".into(),
                region_type: RegionType::Private,
            },
            MemoryRegion {
                base: 0x5000,
                size: 0x1000,
                readable: true,
                writable: false,
                executable: false,
                module_name: "specific".into(),
                region_type: RegionType::Private,
            },
        ];
        let modules = vec![
            ModuleEntry {
                name: "wide.dll".into(),
                full_path: String::new(),
                base: 0x4000,
                size: 0x3000,
            },
            ModuleEntry {
                name: "specific.dll".into(),
                full_path: String::new(),
                base: 0x5000,
                size: 0x1000,
            },
        ];

        let rows = memory_preview_rows_with_maps(
            &provider,
            8,
            1,
            &0x5000u64.to_le_bytes(),
            &regions,
            &modules,
        );

        assert_eq!(rows[0].pointer_note.as_deref(), Some("-> wide.dll+0x1000"));
    }

    #[test]
    fn memory_preview_identity_changes_when_live_bytes_change() {
        let a = HoverPopupKind::MemoryPreview {
            target_addr: 0x1000,
            pointer_size: 8,
            row_count: 10,
            rows: vec![preview_row("01 02 03")].into(),
        };
        let b = HoverPopupKind::MemoryPreview {
            target_addr: 0x1000,
            pointer_size: 8,
            row_count: 10,
            rows: vec![preview_row("01 02 03")].into(),
        };
        let changed = HoverPopupKind::MemoryPreview {
            target_addr: 0x1000,
            pointer_size: 8,
            row_count: 10,
            rows: vec![preview_row("01 02 04")].into(),
        };
        assert!(hover_kind_eq(&a, &b));
        assert!(!hover_kind_eq(&a, &changed));
    }

    fn preview_row(hex: &str) -> MemoryPreviewRow {
        MemoryPreviewRow {
            offset: 0,
            kind: "hex64",
            ascii: String::new(),
            hex: hex.into(),
            type_hint: None,
            pointer_note: None,
        }
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
