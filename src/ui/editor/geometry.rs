//! Row geometry + the row→styled-span layout (pure, headlessly testable).
//!
//! The QScintilla editor worked in **UTF-8 byte positions** and converted to/from
//! **display columns** (char indices) constantly via `SCI_FINDCOLUMN` /
//! `SCI_GETCOLUMN` (editor-surface.md §1 "Coordinate systems"). gpui's
//! `ShapedLine` indexes by **UTF-8 byte offset**, while the `compose`
//! [`ColumnSpan`](crate::compose::ColumnSpan)s are in **char (display) columns**.
//! This module owns that conversion plus the monospace x↔column mapping, and the
//! decomposition of a `LineMeta` + line text into the ordered list of colored
//! [`SpanStyle`] runs the editor paints (the Rust analogue of the Scintilla
//! indicator passes, editor-surface.md §3 & §5: hex-dim, class-name teal, chip
//! colors, tree-connector tint, comment green).
//!
//! All of this is pure (no gpui types), so it is unit-tested without a display.

use crate::compose::{self, ColumnSpan, EditTarget, LineGeometry};
use crate::core::{find_chip, is_hex_preview, ChipKind, LineKind, LineMeta};

/// A monospace cell metric: every glyph advances by `cell_width` pixels and the
/// row is `line_height` tall. The editor shapes real glyphs for painting but uses
/// this fixed grid for hit-testing math (the C++ grid is strictly monospace, so a
/// column is `floor(x / cell_width)`; editor-surface.md §8 `hitTest`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CellMetrics {
    pub cell_width: f32,
    pub line_height: f32,
}

impl CellMetrics {
    pub fn new(cell_width: f32, line_height: f32) -> Self {
        CellMetrics {
            cell_width,
            line_height,
        }
    }

    /// Pixel X (relative to the row's left edge) of the *start* of display
    /// column `col`.
    pub fn x_for_col(&self, col: i32) -> f32 {
        col.max(0) as f32 * self.cell_width
    }

    /// Display column under pixel X (relative to row left). Rounds to the nearest
    /// cell boundary like Scintilla's `POSITIONFROMPOINTCLOSE` (the cursor snaps
    /// to the closer edge of the glyph it's over).
    pub fn col_for_x(&self, x: f32) -> i32 {
        if self.cell_width <= 0.0 {
            return 0;
        }
        (x / self.cell_width).round().max(0.0) as i32
    }

    /// Display column that *contains* pixel X (floor) — used to decide which
    /// glyph the cursor is over (chip-click / byte-click routing needs the cell
    /// the pixel falls inside, not the nearest boundary).
    pub fn col_containing_x(&self, x: f32) -> i32 {
        if self.cell_width <= 0.0 {
            return 0;
        }
        (x / self.cell_width).floor().max(0.0) as i32
    }

    /// Document line under pixel Y (relative to the content top, scroll already
    /// applied). The fallback path of `hitTest` when the click is past the text.
    pub fn line_for_y(&self, y: f32) -> i32 {
        if self.line_height <= 0.0 {
            return 0;
        }
        (y / self.line_height).floor().max(0.0) as i32
    }
}

// ── char (display column) ↔ utf-8 byte conversions (`SCI_FINDCOLUMN` / `GETCOLUMN`) ──

/// Display column → UTF-8 byte offset within `text`. A display column is a *char*
/// index (counting the multi-byte glyphs ▸▾├│└·→ as one each), so this walks
/// chars and accumulates `len_utf8`. Clamps to the end of the string.
pub fn byte_for_col(text: &str, col: i32) -> usize {
    if col <= 0 {
        return 0;
    }
    let col = col as usize;
    let mut chars = 0usize;
    for (byte_idx, _) in text.char_indices() {
        if chars == col {
            return byte_idx;
        }
        chars += 1;
    }
    text.len()
}

/// UTF-8 byte offset → display column (char index). Inverse of [`byte_for_col`].
pub fn col_for_byte(text: &str, byte: usize) -> i32 {
    let byte = byte.min(text.len());
    text[..byte].chars().count() as i32
}

/// Number of display columns (chars) in `text`.
pub fn col_len(text: &str) -> i32 {
    text.chars().count() as i32
}

/// UTF-16 code-unit offset → UTF-8 byte offset within `text`.
///
/// [`ComposeResult::line_starts`](crate::core::ComposeResult) are **UTF-16 unit**
/// offsets (the composer builds the document in a UTF-16 buffer, mirroring
/// Scintilla; editor-surface.md §1). gpui's `String`/`ShapedLine` are UTF-8, so
/// every line slice must map the UTF-16 line-start to a byte offset. Walks chars
/// accumulating both encodings; clamps to the string end.
pub fn utf16_to_byte(text: &str, utf16_units: i32) -> usize {
    if utf16_units <= 0 {
        return 0;
    }
    let target = utf16_units as usize;
    let mut units = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if units >= target {
            return byte_idx;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// A `[start,end)` byte range for a clickable/colored span, derived from a
/// char-column [`ColumnSpan`]. Returns `None` for invalid/empty spans.
pub fn byte_range_for_span(text: &str, span: ColumnSpan) -> Option<std::ops::Range<usize>> {
    if !span.valid || span.end <= span.start {
        return None;
    }
    let start = byte_for_col(text, span.start);
    let end = byte_for_col(text, span.end);
    if end <= start {
        return None;
    }
    Some(start..end)
}

// ── Row → colored span decomposition (the indicator passes, §5) ──

/// A semantic role driving a span's color, mirroring the Scintilla indicator
/// vocabulary that mattered for static rendering (editor-surface.md §3). The
/// concrete `Hsla` is resolved from the theme at paint time (so theme switches
/// are instant); this keeps the layout logic pure and testable.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpanRole {
    /// Default text (the lexer's normal foreground).
    Text,
    /// Type-name column (C++ keyword/type coloring).
    Type,
    /// Field-name column.
    Name,
    /// A formatted value (numbers green-ish).
    Value,
    /// Dimmed hex bytes / fold arrows / braces / footer (`IND_HEX_DIM`).
    Dim,
    /// The teal root class name (`IND_CLASS_NAME`).
    ClassName,
    /// Green comment / symbol / add-comment chip (`IND_HINT_GREEN`).
    CommentGreen,
    /// The dim type-inference chip (`IND_TYPE_HINT`).
    TypeHint,
    /// The amber RTTI chip (`IND_RTTI_HINT`).
    RttiHint,
    /// The enum chip (link-blue, `IND_HOVER_SPAN`).
    EnumChip,
    /// The innermost tree connector glyph tint (`IND_TREE_CONN`).
    TreeConn,
}

/// One colored run over a `[start,end)` **char-column** range with a role.
/// Non-overlapping and ordered by `start`; the painter shapes them into
/// `TextRun`s after resolving role→color.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SpanStyle {
    pub start: i32,
    pub end: i32,
    pub role: SpanRole,
}

/// Decompose a rendered line into ordered, non-overlapping colored runs.
///
/// This is the static-coloring half of `applyDocument` (editor-surface.md §5
/// steps 11–15) expressed as pure column math: it lays a base `Text` run over the
/// whole line, then paints the higher-priority roles on top (later wins, exactly
/// like Scintilla's z-ordered indicators), and finally flattens the stack into a
/// disjoint run list. Heat / selection / hover / byte-selection are *dynamic*
/// overlays applied separately by the view; this covers the layout that depends
/// only on `LineMeta` + text.
pub fn style_runs(lm: &LineMeta, text: &str, type_w: i32, name_w: i32) -> Vec<SpanStyle> {
    let n = col_len(text);
    if n == 0 {
        return Vec::new();
    }

    // Priority-ordered stack: each later push overrides earlier ones where it
    // overlaps (z-order = push order), matching the indicator-slot composite.
    let mut layers: Vec<SpanStyle> = vec![SpanStyle {
        start: 0,
        end: n,
        role: SpanRole::Text,
    }];

    let push = |layers: &mut Vec<SpanStyle>, span: ColumnSpan, role: SpanRole| {
        if span.valid && span.end > span.start {
            layers.push(SpanStyle {
                start: span.start.max(0),
                end: span.end.min(n),
                role,
            });
        }
    };

    match lm.line_kind {
        LineKind::CommandRow => {
            // Root class name teal; the rest stays default/dim (the view dims the
            // chevron/source/addr separately as hover affordances).
            push(
                &mut layers,
                compose::command_row_root_name_span(text),
                SpanRole::ClassName,
            );
        }
        LineKind::Footer => {
            // Whole footer is dim (`applyHexDimming`: footer text dim).
            layers.push(SpanStyle {
                start: 0,
                end: n,
                role: SpanRole::Dim,
            });
        }
        _ => {
            let is_hex = is_hex_preview(lm.node_kind);
            if !lm.is_member_line && !lm.is_continuation {
                push(
                    &mut layers,
                    compose::type_span_for(lm, type_w),
                    SpanRole::Type,
                );
                if !is_hex {
                    // Hex rows hold the ASCII preview in the name column, not a
                    // field name (editor-surface.md §2 valueSpanFor note).
                    push(
                        &mut layers,
                        compose::name_span_for(lm, type_w, name_w),
                        SpanRole::Name,
                    );
                }
            }
            // Value column.
            let vs = compose::value_span_for(lm, type_w, name_w);
            if is_hex {
                // Dim the hex byte run (`IND_HEX_DIM` over the whole preview).
                push(&mut layers, vs, SpanRole::Dim);
            } else {
                // Clip the value at the first chip so trailing chips keep their
                // own color (`narrowPtrValueSpan`, §8).
                push(
                    &mut layers,
                    narrow_value_at_first_chip(lm, vs),
                    SpanRole::Value,
                );
            }

            // Tree-connector tint: the innermost K_TREE_INDENT chars of the indent
            // region for depth>0 rows (`applyHexDimming` tail, §5).
            if lm.depth > 0 && !matches!(lm.line_kind, LineKind::CommandRow | LineKind::Footer) {
                let ind_end = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
                let conn_start = (ind_end - compose::K_TREE_INDENT).max(0);
                push(
                    &mut layers,
                    ColumnSpan {
                        start: conn_start,
                        end: ind_end,
                        valid: true,
                    },
                    SpanRole::TreeConn,
                );
            }
        }
    }

    // Brace dimming (trailing `{` on header lines, `braceCol`).
    if lm.brace_col >= 0 && lm.brace_col < n {
        push(
            &mut layers,
            ColumnSpan {
                start: lm.brace_col,
                end: lm.brace_col + 1,
                valid: true,
            },
            SpanRole::Dim,
        );
    }

    // Chips (highest priority — they are tail annotations with explicit color).
    for chip in &lm.chips {
        let role = match chip.kind {
            ChipKind::Enum => SpanRole::EnumChip,
            ChipKind::TypeHint => SpanRole::TypeHint,
            ChipKind::Rtti => SpanRole::RttiHint,
            ChipKind::Symbol | ChipKind::Comment | ChipKind::AddComment => SpanRole::CommentGreen,
        };
        push(
            &mut layers,
            ColumnSpan {
                start: chip.start_col,
                end: chip.end_col,
                valid: true,
            },
            role,
        );
    }

    flatten(&layers, n)
}

/// Clip a value span at the first chip's `start_col` (so the editable/colored
/// value text excludes trailing RTTI/Symbol/Comment chips). Port of
/// `narrowPtrValueSpan` (editor-surface.md §8), column-only path.
pub fn narrow_value_at_first_chip(lm: &LineMeta, vs: ColumnSpan) -> ColumnSpan {
    if !vs.valid {
        return vs;
    }
    let mut end = vs.end;
    for chip in &lm.chips {
        if chip.start_col >= vs.start && chip.start_col < end {
            end = chip.start_col;
        }
    }
    ColumnSpan {
        start: vs.start,
        end,
        valid: end > vs.start,
    }
}

/// Flatten a priority-ordered (later-wins) layer stack into a disjoint,
/// `start`-sorted run list covering `[0,n)`. For every column the winning role is
/// the last layer that covers it (Scintilla composites highest indicator slot).
fn flatten(layers: &[SpanStyle], n: i32) -> Vec<SpanStyle> {
    if n <= 0 {
        return Vec::new();
    }
    // Per-column winning role (n is a small display-line width).
    let mut role_at: Vec<SpanRole> = vec![SpanRole::Text; n as usize];
    for layer in layers {
        let s = layer.start.max(0);
        let e = layer.end.min(n);
        for slot in role_at.iter_mut().take(e as usize).skip(s as usize) {
            *slot = layer.role;
        }
    }
    // Coalesce equal adjacent roles into runs.
    let mut out: Vec<SpanStyle> = Vec::new();
    let mut start = 0i32;
    for col in 1..=n {
        if col == n || role_at[col as usize] != role_at[(col - 1) as usize] {
            out.push(SpanStyle {
                start,
                end: col,
                role: role_at[(col - 1) as usize],
            });
            start = col;
        }
    }
    out
}

/// The char-column [`ColumnSpan`] for an [`EditTarget`] on a given line, mirroring
/// the editor's authoritative `resolvedSpanFor` (editor-surface.md §8) dispatch
/// onto the `compose` span helpers. Returns an invalid span when the target is
/// not editable on this line (the caller treats that as "no field here").
///
/// This is the single seam between a resolved edit target and the column math, so
/// hit-testing, tab-cycling, and edit-begin all agree on where a field lives.
pub fn resolved_span_for(
    lm: &LineMeta,
    text: &str,
    target: EditTarget,
    type_w: i32,
    name_w: i32,
) -> ColumnSpan {
    use EditTarget::*;
    let line_len = col_len(text);
    match target {
        Type => {
            if lm.line_kind == LineKind::Header {
                if lm.is_array_header {
                    // Array header Type → element-type span (picker).
                    compose::array_elem_type_span_for(lm, text)
                } else {
                    compose::command_row_root_type_span(text)
                        .valid_or(|| header_type_span(lm, text, type_w))
                }
            } else {
                compose::type_span_for(lm, type_w)
            }
        }
        Name => {
            if lm.line_kind == LineKind::Header {
                header_name_span(lm, text, type_w, name_w)
            } else if lm.is_member_line {
                compose::member_name_span_for(lm, text)
            } else if is_hex_preview(lm.node_kind) {
                ColumnSpan::default() // hex rows block Name editing (§17.2)
            } else {
                compose::name_span_for(lm, type_w, name_w)
            }
        }
        Value => {
            if lm.is_member_line {
                compose::member_value_span_for(lm, text)
            } else if is_hex_preview(lm.node_kind) {
                ColumnSpan::default() // hex value → overwrite mode, not inline edit
            } else {
                narrow_value_at_first_chip(lm, compose::value_span_for(lm, type_w, name_w))
            }
        }
        Comment => {
            // Existing comment chip span, else end-of-line fallback for creation.
            if let Some(chip) = find_chip(lm, ChipKind::Comment) {
                ColumnSpan {
                    start: chip.start_col,
                    end: chip.end_col,
                    valid: true,
                }
            } else {
                compose::comment_span_for(lm, line_len, type_w, name_w)
            }
        }
        BaseAddress => compose::command_row_addr_span(text),
        Source => compose::command_row_src_span(text),
        RootClassType => compose::command_row_root_type_span(text),
        RootClassName => compose::command_row_root_name_span(text),
        ArrayElementType => compose::array_elem_type_span_for(lm, text),
        ArrayElementCount => compose::array_elem_count_click_span_for(lm, text),
        ArrayIndex => compose::array_index_span_for(lm, text),
        ArrayCount => compose::array_count_span_for(lm, text),
        PointerTarget => compose::pointer_target_span_for(lm, text),
        StaticExpr => compose::static_expr_span_for(text),
        TypeSelector => compose::command_row_chevron_span(text),
    }
}

/// Header type-name span: the type column on a header line, rejecting anonymous
/// `struct/union/class`/`[N]` names (editor-surface.md §8 header fallbacks). A
/// minimal column-based approximation of the editor-local `headerTypeNameSpan`.
fn header_type_span(lm: &LineMeta, _text: &str, type_w: i32) -> ColumnSpan {
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    ColumnSpan {
        start: ind,
        end: ind + type_w,
        valid: true,
    }
}

/// Header field-name span: the name column on a header line.
fn header_name_span(lm: &LineMeta, _text: &str, type_w: i32, name_w: i32) -> ColumnSpan {
    let ind = compose::K_FOLD_COL + lm.depth * compose::K_TREE_INDENT;
    let start = ind + type_w + compose::K_SEP_WIDTH;
    ColumnSpan {
        start,
        end: start + name_w,
        valid: true,
    }
}

/// Effective type/name column widths for a line (`LineGeometry::forLine`).
pub fn effective_widths(lm: &LineMeta) -> (i32, i32) {
    let g = LineGeometry::for_line(lm);
    (g.type_column_width, g.name_column_width)
}

// Small extension to chain a span fallback when invalid.
trait ColumnSpanExt {
    fn valid_or(self, f: impl FnOnce() -> ColumnSpan) -> ColumnSpan;
}
impl ColumnSpanExt for ColumnSpan {
    fn valid_or(self, f: impl FnOnce() -> ColumnSpan) -> ColumnSpan {
        if self.valid && self.end > self.start {
            self
        } else {
            f()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeKind;

    fn field_line(depth: i32, kind: NodeKind) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Field,
            node_kind: kind,
            depth,
            ..LineMeta::default()
        }
    }

    #[test]
    fn cell_metrics_x_and_col_round_trip() {
        let m = CellMetrics::new(8.0, 16.0);
        assert_eq!(m.x_for_col(0), 0.0);
        assert_eq!(m.x_for_col(10), 80.0);
        // Nearest-boundary snap: 79px is closer to col 10 (80) than col 9 (72).
        assert_eq!(m.col_for_x(79.0), 10);
        assert_eq!(m.col_for_x(76.0), 10); // 76 rounds to 9.5→10
        assert_eq!(m.col_for_x(75.0), 9);
        // Containing cell is a floor.
        assert_eq!(m.col_containing_x(79.0), 9);
        assert_eq!(m.col_containing_x(80.0), 10);
        assert_eq!(m.line_for_y(33.0), 2);
    }

    #[test]
    fn col_for_x_safe_when_zero_width() {
        let m = CellMetrics::new(0.0, 0.0);
        assert_eq!(m.col_for_x(100.0), 0);
        assert_eq!(m.line_for_y(100.0), 0);
    }

    #[test]
    fn ascii_col_byte_conversions_are_identity() {
        let s = "hello world";
        assert_eq!(byte_for_col(s, 0), 0);
        assert_eq!(byte_for_col(s, 5), 5);
        assert_eq!(byte_for_col(s, 100), s.len());
        assert_eq!(col_for_byte(s, 5), 5);
        assert_eq!(col_len(s), 11);
    }

    #[test]
    fn multibyte_glyphs_count_as_one_column() {
        // ▾ (U+25BE, 3 bytes) + " 0x10" — column 1 is just past the arrow.
        let s = "\u{25BE} 0x10";
        assert_eq!(col_len(s), 6);
        // Column 0 is the arrow start (byte 0); column 1 is the space (byte 3).
        assert_eq!(byte_for_col(s, 1), 3);
        assert_eq!(col_for_byte(s, 3), 1);
        // A span covering the arrow only (cols 0..1) is bytes 0..3.
        let r = byte_range_for_span(
            s,
            ColumnSpan {
                start: 0,
                end: 1,
                valid: true,
            },
        )
        .unwrap();
        assert_eq!(r, 0..3);
    }

    #[test]
    fn byte_range_for_invalid_span_is_none() {
        let s = "abc";
        assert_eq!(byte_range_for_span(s, ColumnSpan::default()), None);
        assert_eq!(
            byte_range_for_span(
                s,
                ColumnSpan {
                    start: 2,
                    end: 2,
                    valid: true
                }
            ),
            None
        );
    }

    #[test]
    fn style_runs_are_disjoint_sorted_and_cover_line() {
        // Build a depth-0 line whose columns match compose's geometry exactly:
        // fold(3) prefix, type[3..17], sep, name[18..40], sep, value[41..].
        use compose::{K_FOLD_COL, K_SEP_WIDTH};
        let lm = field_line(0, NodeKind::Int32);
        let type_w = 14;
        let name_w = 22;
        let value_start = (K_FOLD_COL + type_w + name_w + 2 * K_SEP_WIDTH) as usize;
        let mut text = String::new();
        text.push_str("   "); // fold prefix
        text.push_str("int32"); // type
        while col_len(&text) < (K_FOLD_COL + type_w + K_SEP_WIDTH) {
            text.push(' ');
        }
        text.push_str("field"); // name
        while col_len(&text) < value_start as i32 {
            text.push(' ');
        }
        text.push_str("100"); // value — sits exactly at value_start
        let runs = style_runs(&lm, &text, type_w, name_w);
        assert!(!runs.is_empty());
        // Sorted, contiguous, covering [0, len).
        assert_eq!(runs.first().unwrap().start, 0);
        assert_eq!(runs.last().unwrap().end, col_len(&text));
        for w in runs.windows(2) {
            assert_eq!(w[0].end, w[1].start, "runs must be contiguous");
            assert!(w[0].end > w[0].start, "runs non-empty");
        }
        // Type column colored Type; value column colored Value.
        assert!(runs.iter().any(|r| r.role == SpanRole::Type));
        assert!(runs.iter().any(|r| r.role == SpanRole::Value));
    }

    #[test]
    fn hex_row_value_column_is_dim_not_value() {
        let mut lm = field_line(0, NodeKind::Hex64);
        lm.node_kind = NodeKind::Hex64;
        let text = "hex64         AB CD EF 01 23 45 67 89   ........";
        let runs = style_runs(&lm, text, 14, 22);
        // Hex rows dim the byte run; there is no green Value role.
        assert!(runs.iter().any(|r| r.role == SpanRole::Dim));
        assert!(!runs.iter().any(|r| r.role == SpanRole::Value));
    }

    #[test]
    fn empty_line_yields_no_runs() {
        let lm = field_line(0, NodeKind::Int32);
        assert!(style_runs(&lm, "", 14, 22).is_empty());
    }

    #[test]
    fn narrow_value_clips_at_first_chip() {
        let mut lm = field_line(0, NodeKind::Pointer64);
        lm.chips.push(crate::core::LineChip {
            kind: ChipKind::Rtti,
            start_col: 40,
            end_col: 50,
            ..Default::default()
        });
        let vs = ColumnSpan {
            start: 30,
            end: 96,
            valid: true,
        };
        let narrowed = narrow_value_at_first_chip(&lm, vs);
        assert_eq!(narrowed.start, 30);
        assert_eq!(narrowed.end, 40);
        assert!(narrowed.valid);
    }

    #[test]
    fn resolved_span_hex_blocks_name_and_value() {
        let lm = field_line(0, NodeKind::Hex32);
        let text = "hex32         AB CD EF 01   ....";
        assert!(!resolved_span_for(&lm, text, EditTarget::Name, 14, 22).valid);
        assert!(!resolved_span_for(&lm, text, EditTarget::Value, 14, 22).valid);
        // Type is editable on a hex row.
        assert!(resolved_span_for(&lm, text, EditTarget::Type, 14, 22).valid);
    }

    #[test]
    fn resolved_span_field_name_and_value_valid() {
        let lm = field_line(1, NodeKind::Int32);
        let text = "   int32         field                  100";
        let name = resolved_span_for(&lm, text, EditTarget::Name, 14, 22);
        let value = resolved_span_for(&lm, text, EditTarget::Value, 14, 22);
        assert!(name.valid && name.end > name.start);
        assert!(value.valid && value.end > value.start);
        // Name precedes value in column order.
        assert!(name.end <= value.start);
    }
}
