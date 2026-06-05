//! View-side selection state: the byte-selection range model (address-based,
//! half-open) and small selection-id helpers.
//!
//! Node multi-select itself lives in the controller (`handle_node_click` already
//! implements Ctrl/Shift/cross-row range selection, controller.rs:2518); the
//! editor view only *captures the click* and forwards `(line, node_id, mods)`.
//! What the editor **owns** is the per-byte **hex selection** over the hex-preview
//! rows (editor-surface.md §12): an absolute-address, half-open `[lo, hi)` range
//! that survives refresh/tab-switch and naturally spans multiple hex rows, plus
//! its drag anchor. Those are pure value logic and unit-tested here.

use crate::compose::ColumnSpan;
use crate::core::{is_hex_preview, LineMeta};

/// The per-byte hex selection — `m_byteSel` (editor-surface.md §12). A half-open
/// absolute-address range `[lo, hi)`; address-based so it is stable across
/// recomposition and spans rows transparently.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct ByteSelection {
    range: Option<(u64, u64)>,
    /// Drag anchor address (`m_byteSelAnchor`); set on press, cleared on release.
    anchor: Option<u64>,
    /// Whether a byte-drag is in progress (`m_byteSelDragging`).
    dragging: bool,
}

impl ByteSelection {
    pub fn new() -> Self {
        ByteSelection::default()
    }

    /// The current `[lo, hi)` range, if any.
    pub fn range(&self) -> Option<(u64, u64)> {
        self.range
    }

    /// `true` when a selection is active.
    pub fn is_active(&self) -> bool {
        self.range.is_some()
    }

    /// `byteSelection().has_value()` byte count, 0 if inactive.
    pub fn len(&self) -> u64 {
        self.range
            .map(|(lo, hi)| hi.saturating_sub(lo))
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `setByteSelection(lo, hi)` — rejects `hi <= lo` (editor-surface.md §12 API
    /// contract: `setByteSelection` enforces `lo < hi`).
    pub fn set(&mut self, lo: u64, hi: u64) -> bool {
        if hi <= lo {
            return false;
        }
        self.range = Some((lo, hi));
        true
    }

    /// `clearByteSelection()` — also drops the drag anchor/flag.
    pub fn clear(&mut self) {
        self.range = None;
        self.anchor = None;
        self.dragging = false;
    }

    /// Arm a drag at `addr` (mouse press on a hex byte without modifiers).
    pub fn arm(&mut self, addr: u64) {
        self.anchor = Some(addr);
        self.dragging = false;
        // A press selects the single byte under the cursor.
        self.range = Some((addr, addr + 1));
    }

    /// Whether a drag anchor is armed.
    pub fn anchor(&self) -> Option<u64> {
        self.anchor
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// Extend the drag to `addr` (half-open). The selection spans the anchor and
    /// `addr` inclusive of the byte under `addr` (`[min, max+1)`); flips the
    /// `dragging` flag on. No-op if no anchor is armed.
    pub fn drag_to(&mut self, addr: u64) {
        let Some(anchor) = self.anchor else {
            return;
        };
        self.dragging = true;
        let lo = anchor.min(addr);
        let hi = anchor.max(addr) + 1;
        self.range = Some((lo, hi));
    }

    /// Shift+Click extend from the existing low edge to `addr` (`[lo, addr+1)`),
    /// keeping `lo` as the anchor. Used by Shift+Click on a hex byte
    /// (editor-surface.md §9 "Shift+Click on hex byte"). If no selection exists,
    /// behaves like a fresh single-byte select at `addr`.
    pub fn shift_extend_to(&mut self, addr: u64) {
        // The anchor is the existing low edge; the new range spans the anchor and
        // the clicked byte inclusive: `[min(addr, anchor), max(addr, anchor) + 1)`.
        // (Anchor preserved so a backward extend keeps the anchor as the high end,
        // matching `byteAddrAt` anchor=lo, half-open hi; editor-surface.md §9.)
        let anchor = self.range.map(|(lo, _)| lo).unwrap_or(addr);
        let lo = anchor.min(addr);
        let hi = anchor.max(addr) + 1;
        self.range = Some((lo, hi));
    }

    /// `extendByteSelection(dByte)` — grow/shrink the high edge by `d_byte`
    /// bytes, keeping `lo` fixed; never lets `hi` drop to/below `lo`
    /// (editor-surface.md §12: shrink clamps at `lo+1`).
    pub fn extend_by(&mut self, d_byte: i64) {
        let Some((lo, hi)) = self.range else {
            return;
        };
        let new_hi = (hi as i64 + d_byte).max(lo as i64 + 1) as u64;
        self.range = Some((lo, new_hi));
    }

    /// Finalize a drag (mouse release): persist the range, drop anchor + flag.
    pub fn finish_drag(&mut self) {
        self.anchor = None;
        self.dragging = false;
    }
}

/// `byteAddrAt(line, col)` (editor-surface.md §12): the absolute address of the
/// hex byte under a display column, valid only on hex-preview field rows inside
/// the value column. Each byte renders as `"XX "` = 3 columns, so
/// `byteIdx = (col - value_start) / 3`, bounds-checked to `[0, size)`.
///
/// `value_span` is the row's value [`ColumnSpan`] (from `compose::value_span_for`)
/// and `byte_count` is `LineMeta::line_byte_count` (or `size_for_kind`).
pub fn byte_addr_at(
    lm: &LineMeta,
    value_span: ColumnSpan,
    byte_count: i32,
    col: i32,
) -> Option<u64> {
    if !is_hex_preview(lm.node_kind) || lm.line_kind != crate::core::LineKind::Field {
        return None;
    }
    if !value_span.valid || col < value_span.start {
        return None;
    }
    let byte_idx = (col - value_span.start) / 3;
    if byte_idx < 0 || byte_idx >= byte_count {
        return None;
    }
    Some(lm.offset_addr.wrapping_add(byte_idx as u64))
}

/// The display-column span `[start, end]` covering hex bytes `[first, last)` of a
/// row — the inverse of [`byte_addr_at`]'s `col → byte` mapping (each byte is the
/// 3-column `"XX "` cell). `value_span` is the row's value [`ColumnSpan`].
pub fn byte_cols_in_row(value_span: ColumnSpan, first: i32, last: i32) -> (i32, i32) {
    (
        value_span.start + first * 3,
        value_span.start + (last - 1) * 3 + 2,
    )
}

/// The intersection of a hex row `[row_lo, row_lo + count)` with a byte
/// selection `[lo, hi)`, returned as the `[first_byte, last_byte)` *byte indices*
/// within the row to highlight (`applyByteSelectionOverlay`, editor-surface.md
/// §12). `None` when the row does not overlap the selection.
pub fn row_byte_overlap(row_lo: u64, count: i32, sel: (u64, u64)) -> Option<(i32, i32)> {
    if count <= 0 {
        return None;
    }
    let row_hi = row_lo + count as u64;
    let (lo, hi) = sel;
    let inter_lo = lo.max(row_lo);
    let inter_hi = hi.min(row_hi);
    if inter_hi <= inter_lo {
        return None;
    }
    let first = (inter_lo - row_lo) as i32;
    let last = (inter_hi - row_lo) as i32;
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{LineKind, NodeKind};

    fn hex_row(addr: u64) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Field,
            node_kind: NodeKind::Hex64,
            offset_addr: addr,
            line_byte_count: 8,
            ..LineMeta::default()
        }
    }

    #[test]
    fn set_rejects_empty_or_inverted() {
        let mut b = ByteSelection::new();
        assert!(!b.set(10, 10));
        assert!(!b.set(10, 5));
        assert!(!b.is_active());
        assert!(b.set(10, 14));
        assert!(b.is_active());
        assert_eq!(b.range(), Some((10, 14)));
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn arm_selects_single_byte_then_drag_extends() {
        let mut b = ByteSelection::new();
        b.arm(0x1000);
        assert_eq!(b.range(), Some((0x1000, 0x1001)));
        assert!(!b.is_dragging());
        // Drag forward.
        b.drag_to(0x1004);
        assert!(b.is_dragging());
        assert_eq!(b.range(), Some((0x1000, 0x1005)));
        // Drag backward past the anchor.
        b.drag_to(0x0FFC);
        assert_eq!(b.range(), Some((0x0FFC, 0x1001)));
        b.finish_drag();
        assert!(!b.is_dragging());
        assert_eq!(b.anchor(), None);
        // Range persists after the drag ends.
        assert_eq!(b.range(), Some((0x0FFC, 0x1001)));
    }

    #[test]
    fn shift_extend_keeps_low_edge() {
        let mut b = ByteSelection::new();
        b.set(0x100, 0x104);
        b.shift_extend_to(0x108);
        assert_eq!(b.range(), Some((0x100, 0x109)));
        // Extend backward before lo moves lo.
        b.shift_extend_to(0x0F0);
        assert_eq!(b.range(), Some((0x0F0, 0x101)));
    }

    #[test]
    fn extend_by_clamps_at_one_byte() {
        let mut b = ByteSelection::new();
        b.set(0x10, 0x14); // 4 bytes
        b.extend_by(2);
        assert_eq!(b.range(), Some((0x10, 0x16)));
        b.extend_by(-100); // would invert → clamp to lo+1
        assert_eq!(b.range(), Some((0x10, 0x11)));
    }

    #[test]
    fn clear_resets_everything() {
        let mut b = ByteSelection::new();
        b.arm(0x10);
        b.drag_to(0x20);
        b.clear();
        assert!(!b.is_active());
        assert_eq!(b.anchor(), None);
        assert!(!b.is_dragging());
    }

    #[test]
    fn byte_addr_at_maps_columns_to_addresses() {
        let lm = hex_row(0x2000);
        // value column starts after fold(3)+type(14)+sep+name(22)+sep on a
        // depth-0 row; use compose's value_span_for to get the real start.
        let vs = crate::compose::value_span_for(&lm, 14, 22);
        let count = lm.line_byte_count;
        // Byte 0 lives at columns [start, start+3): start, start+1 map to byte 0.
        assert_eq!(byte_addr_at(&lm, vs, count, vs.start), Some(0x2000));
        assert_eq!(byte_addr_at(&lm, vs, count, vs.start + 1), Some(0x2000));
        // Byte 3 is at start + 9.
        assert_eq!(byte_addr_at(&lm, vs, count, vs.start + 9), Some(0x2003));
        // Past the last byte (8) → None.
        assert_eq!(byte_addr_at(&lm, vs, count, vs.start + 8 * 3), None);
        // Before the value column → None.
        assert_eq!(byte_addr_at(&lm, vs, count, vs.start - 1), None);
    }

    #[test]
    fn byte_addr_at_only_on_hex_field_rows() {
        let mut lm = hex_row(0x100);
        lm.node_kind = NodeKind::Int32; // not hex
        let vs = ColumnSpan {
            start: 10,
            end: 33,
            valid: true,
        };
        assert_eq!(byte_addr_at(&lm, vs, 4, 11), None);

        let mut lm2 = hex_row(0x100);
        lm2.line_kind = LineKind::Header; // not a field
        let vs2 = crate::compose::value_span_for(&lm2, 14, 22);
        assert_eq!(byte_addr_at(&lm2, vs2, 8, vs2.start), None);
    }

    #[test]
    fn row_overlap_intersects_addresses_to_byte_indices() {
        // Row covers [0x1000, 0x1008). Selection [0x1002, 0x1005) → bytes 2..5.
        assert_eq!(row_byte_overlap(0x1000, 8, (0x1002, 0x1005)), Some((2, 5)));
        // Selection entirely before the row.
        assert_eq!(row_byte_overlap(0x1000, 8, (0x0F00, 0x1000)), None);
        // Selection entirely after the row.
        assert_eq!(row_byte_overlap(0x1000, 8, (0x1008, 0x100A)), None);
        // Selection spanning the whole row and beyond → full row 0..8.
        assert_eq!(row_byte_overlap(0x1000, 8, (0x0F00, 0x2000)), Some((0, 8)));
        // Zero-count row.
        assert_eq!(row_byte_overlap(0x1000, 0, (0x1000, 0x1004)), None);
    }
}
