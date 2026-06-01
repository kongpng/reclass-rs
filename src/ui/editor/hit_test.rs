//! Hit-testing: map a pixel position (within a row) to the semantic
//! [`EditTarget`] under it, in the editor's authoritative priority order.
//!
//! Port of `hitTestTarget` (editor-surface.md §8 line 4888) — the ordered
//! probe that decides which clickable region a pixel lands on. The ordering is
//! load-bearing (more specific regions win): CommandRow sub-spans first, then
//! pointer/array sub-spans, then the generic type/name/value/comment columns,
//! with the documented redirects (array-header Type → ArrayElementType; array
//! element type/name → ArrayElementType; hex rows block Name/Value). Pure column
//! math (given the cell metrics), so it is unit-tested without a display.

use crate::compose::{self, ColumnSpan, EditTarget};
use crate::core::{is_hex_preview, LineKind, LineMeta, NodeKind};

use super::geometry;

/// The result of a hit test on a row: the display column and, if the column lands
/// on a clickable region, its [`EditTarget`]. `in_fold_col` flags a click in the
/// fold-glyph prefix of a fold-head line (`hitTest::inFoldCol`, §8).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HitInfo {
    pub col: i32,
    pub target: Option<EditTarget>,
    pub in_fold_col: bool,
}

#[inline]
fn span_contains(span: ColumnSpan, col: i32) -> bool {
    span.valid && col >= span.start && col < span.end
}

/// Resolve the [`EditTarget`] under display column `col` on a line, in
/// `hitTestTarget` priority order. Returns `None` when the column is over no
/// clickable region.
pub fn target_at_col(
    lm: &LineMeta,
    text: &str,
    col: i32,
    type_w: i32,
    name_w: i32,
) -> Option<EditTarget> {
    // ── CommandRow: chevron → src → addr → root-name (line 0). ──
    if lm.line_kind == LineKind::CommandRow {
        if span_contains(compose::command_row_chevron_span(text), col) {
            return Some(EditTarget::TypeSelector);
        }
        if span_contains(compose::command_row_src_span(text), col) {
            return Some(EditTarget::Source);
        }
        if span_contains(compose::command_row_addr_span(text), col) {
            return Some(EditTarget::BaseAddress);
        }
        if span_contains(compose::command_row_root_name_span(text), col) {
            return Some(EditTarget::RootClassName);
        }
        // B4 / item 1: the root struct/class/enum KEYWORD is NOT left-click
        // editable. C++ `hitTestTarget` returns only `RootClassName` for the name
        // span; the keyword resolves to `None` (left-click does nothing) and is
        // converted via the RIGHT-click "Convert to Struct/Class" menu instead.
        // Returning `RootClassType` here opened a highlighted text box that
        // committed to nothing (a dead box) — and the same on double-click. Drop it.
        return None;
    }

    let is_ptr = matches!(lm.node_kind, NodeKind::Pointer32 | NodeKind::Pointer64);
    let is_hex = is_hex_preview(lm.node_kind);

    // ── Pointer target sub-span (before generic type, since `Type*` overlaps). ──
    if is_ptr && span_contains(compose::pointer_target_span_for(lm, text), col) {
        return Some(EditTarget::PointerTarget);
    }

    // ── Array header: count-click and element-type. ──
    if lm.line_kind == LineKind::Header && lm.is_array_header {
        if span_contains(compose::array_elem_count_click_span_for(lm, text), col) {
            return Some(EditTarget::ArrayElementCount);
        }
        if span_contains(compose::array_elem_type_span_for(lm, text), col) {
            return Some(EditTarget::ArrayElementType);
        }
        // `<idx/count>` nav arrows.
        if span_contains(compose::array_index_span_for(lm, text), col) {
            return Some(EditTarget::ArrayIndex);
        }
        if span_contains(compose::array_count_span_for(lm, text), col) {
            return Some(EditTarget::ArrayCount);
        }
    }

    // ── Member rows (enum/bitfield): name = value, text-scan spans. ──
    if lm.is_member_line {
        if span_contains(compose::member_value_span_for(lm, text), col) {
            return Some(EditTarget::Value);
        }
        if span_contains(compose::member_name_span_for(lm, text), col) {
            return Some(EditTarget::Name);
        }
        return None;
    }

    // ── Static-field expression rows. ──
    if lm.is_static_line && span_contains(compose::static_expr_span_for(text), col) {
        return Some(EditTarget::StaticExpr);
    }

    // ── Generic type / name / value / comment columns. ──
    // Header redirect: array header Type → ArrayElementType (handled above);
    // plain header Type opens the picker, Name editable.
    if span_contains(
        geometry::resolved_span_for(lm, text, EditTarget::Type, type_w, name_w),
        col,
    ) {
        return Some(EditTarget::Type);
    }
    if !is_hex {
        if span_contains(
            geometry::resolved_span_for(lm, text, EditTarget::Name, type_w, name_w),
            col,
        ) {
            return Some(EditTarget::Name);
        }
        if span_contains(
            geometry::resolved_span_for(lm, text, EditTarget::Value, type_w, name_w),
            col,
        ) {
            return Some(EditTarget::Value);
        }
    }
    if span_contains(
        geometry::resolved_span_for(lm, text, EditTarget::Comment, type_w, name_w),
        col,
    ) {
        return Some(EditTarget::Comment);
    }

    None
}

/// Full hit test: pixel X (relative to the row left) + the cell metrics + the
/// line → [`HitInfo`]. `is_fold_head` lets the caller mark fold-prefix clicks.
pub fn hit_test_row(
    lm: &LineMeta,
    text: &str,
    rel_x: f32,
    metrics: geometry::CellMetrics,
    type_w: i32,
    name_w: i32,
) -> HitInfo {
    let col = metrics.col_containing_x(rel_x);
    let in_fold_col = lm.fold_head && col >= 0 && col < compose::K_FOLD_COL;
    let target = if in_fold_col {
        None // a fold-prefix click toggles the fold, it is not an edit target
    } else {
        target_at_col(lm, text, col, type_w, name_w)
    };
    HitInfo {
        col,
        target,
        in_fold_col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> geometry::CellMetrics {
        geometry::CellMetrics::new(8.0, 16.0)
    }

    fn field(kind: NodeKind, depth: i32) -> LineMeta {
        LineMeta {
            line_kind: LineKind::Field,
            node_kind: kind,
            depth,
            ..LineMeta::default()
        }
    }

    #[test]
    fn click_in_type_column_hits_type() {
        let lm = field(NodeKind::Int32, 0);
        let text = "int32         field                  100";
        // Type column starts at fold(3); a column inside it → Type.
        let t = target_at_col(&lm, text, compose::K_FOLD_COL + 1, 14, 22);
        assert_eq!(t, Some(EditTarget::Type));
    }

    #[test]
    fn click_in_name_then_value_columns() {
        let lm = field(NodeKind::Int32, 0);
        let text = "int32         field                  100";
        // Name column begins after fold+type+sep = 3+14+1 = 18.
        let name = target_at_col(&lm, text, 19, 14, 22);
        assert_eq!(name, Some(EditTarget::Name));
        // Value column begins after fold+type+sep+name+sep = 18+22+1 = 41.
        let value = target_at_col(&lm, text, 42, 14, 22);
        assert_eq!(value, Some(EditTarget::Value));
    }

    #[test]
    fn hex_row_blocks_name_and_value_but_not_type() {
        let lm = field(NodeKind::Hex64, 0);
        let text = "hex64         AB CD EF 01 23 45 67 89   ........";
        // Type still hittable.
        assert_eq!(
            target_at_col(&lm, text, compose::K_FOLD_COL + 1, 14, 22),
            Some(EditTarget::Type)
        );
        // A column in the (would-be) value region is NOT a Value target on hex.
        let v = target_at_col(&lm, text, 42, 14, 22);
        assert_ne!(v, Some(EditTarget::Value));
        assert_ne!(v, Some(EditTarget::Name));
    }

    #[test]
    fn command_row_spans_route_in_order() {
        let lm = LineMeta {
            line_kind: LineKind::CommandRow,
            ..LineMeta::default()
        };
        // "[▸] file ▾ 0x1000 struct Foo {" — chevron at col 0.
        let text = "[\u{25B8}] file \u{25BE} 0x1000 struct Foo {";
        assert_eq!(
            target_at_col(&lm, text, 0, 14, 22),
            Some(EditTarget::TypeSelector)
        );
        // 'file' source label.
        let src = compose::command_row_src_span(text);
        assert!(src.valid);
        assert_eq!(
            target_at_col(&lm, text, src.start, 14, 22),
            Some(EditTarget::Source)
        );
        // address.
        let addr = compose::command_row_addr_span(text);
        assert!(addr.valid);
        assert_eq!(
            target_at_col(&lm, text, addr.start, 14, 22),
            Some(EditTarget::BaseAddress)
        );
        // root class name.
        let name = compose::command_row_root_name_span(text);
        assert!(name.valid);
        assert_eq!(
            target_at_col(&lm, text, name.start, 14, 22),
            Some(EditTarget::RootClassName)
        );
    }

    #[test]
    fn command_row_keyword_resolves_to_no_target() {
        // B4 / item 1 regression: the root struct/class/enum KEYWORD is NOT
        // left-click editable — clicking it must resolve to `None` (C++
        // `hitTestTarget` returns only `RootClassName` for the name span; the
        // keyword is converted via the RIGHT-click menu). Returning a target here is
        // what opened the dead, un-typeable edit box.
        let lm = LineMeta {
            line_kind: LineKind::CommandRow,
            ..LineMeta::default()
        };
        let text = "[\u{25B8}] file \u{25BE} 0x1000 struct Foo {";
        let ty = compose::command_row_root_type_span(text);
        assert!(ty.valid, "keyword span resolves");
        // The first column of the keyword span resolves to NO edit target.
        assert_eq!(target_at_col(&lm, text, ty.start, 14, 22), None);
        // And a mid-keyword column likewise.
        assert_eq!(
            target_at_col(&lm, text, (ty.start + ty.end) / 2, 14, 22),
            None
        );
    }

    // B1/items 2-3: the kind-icon gutter is REMOVED — C++ has no icon column. The
    // inline-edit overlay-left is now just `col_start*cell` (no gutter/border/margin
    // term) because the overlay lives inside the text-region wrapper whose origin ==
    // the painted text origin. Kept as `0.0` so the overlay-left assertions below
    // read as the no-gutter math `0 + col*cell`.
    const ICON_CELLS: f32 = 0.0;

    #[test]
    fn command_row_name_click_seeds_name_and_lands_overlay_on_the_name() {
        // BUG 1 regression: clicking the root class NAME on the command header row
        // must (a) resolve through the FULL pixel hit test to `RootClassName` (NOT
        // `RootClassType`/`struct`), (b) seed the edit with the class name (NOT the
        // `struct` keyword), and (c) position the inline-edit overlay directly over
        // the NAME column — `ICON_CELLS*cell + name.start*cell` — not ~2 cells right
        // onto the first letters and not on the type keyword.
        let m = metrics(); // cell_width = 8.0
        let lm = LineMeta {
            line_kind: LineKind::CommandRow,
            node_idx: -1,
            ..LineMeta::default()
        };
        // The live command row shape for a real file, e.g. png.rcx.
        let text = "[\u{25B8}] 'png.rcx'\u{25BE}  0x0  struct PNG_Header {";

        let name = compose::command_row_root_name_span(text);
        let ty = compose::command_row_root_type_span(text);
        assert!(name.valid && ty.valid);
        // Sanity: the spans are disjoint and the type keyword precedes the name.
        assert!(
            ty.end <= name.start,
            "type {ty:?} must precede name {name:?}"
        );

        // (a) A pixel at the NAME span's first cell resolves to RootClassName via
        // the exact path `on_row_mouse_down` runs (`hit_test_row`, text-local X).
        let name_x = (name.start as f32 + 0.5) * m.cell_width;
        let hit = hit_test_row(&lm, text, name_x, m, 14, 22);
        assert_eq!(
            hit.target,
            Some(EditTarget::RootClassName),
            "click on the class name (col {}) must hit RootClassName, got {:?}",
            hit.col,
            hit.target
        );
        assert_eq!(
            hit.col, name.start,
            "hit column must be the name span start"
        );

        // (b) The resolved edit span + its seed text = the class name (not 'struct').
        let span = geometry::resolved_span_for(&lm, text, EditTarget::RootClassName, 14, 22);
        assert!(span.valid);
        let seed = text
            .get(geometry::byte_for_col(text, span.start)..geometry::byte_for_col(text, span.end))
            .unwrap_or("")
            .trim();
        assert_eq!(seed, "PNG_Header", "inline edit must seed the class name");
        assert_ne!(seed, "struct", "must NOT seed the type keyword");

        // (c) The inline-edit overlay-left lands exactly on the name column. The
        // overlay lives inside the text-region wrapper (origin == painted text
        // origin), so its left is `ICON_CELLS*cell + col_start*cell`. Painted text
        // column `name.start` sits at the same pixel — by construction.
        let overlay_left = ICON_CELLS * m.cell_width + span.start as f32 * m.cell_width;
        let painted_text_col_x = ICON_CELLS * m.cell_width + name.start as f32 * m.cell_width;
        assert_eq!(
            overlay_left, painted_text_col_x,
            "overlay must land on the painted name column, not shifted onto 'PNG'"
        );
    }

    #[test]
    fn field_name_click_resolves_to_name_and_overlay_lands_on_the_name() {
        // BUG 2 regression: single-clicking a field row's NAME token (after the
        // node is selected) must resolve to `EditTarget::Name` through the FULL
        // pixel hit test — the canonical `begin_inline_edit(Name)` gesture — and the
        // inline-edit overlay must land on the painted name column (not Type/Comment
        // and not shifted by the address/icon-gutter lead-in). We drive the metrics
        // path with a text-local pixel X (exactly what `RowElement` passes), and
        // verify the overlay-left math `ICON_CELLS*cell + name.start*cell`.
        let m = metrics(); // cell_width = 8.0
        let lm = field(NodeKind::Int32, 0);
        let (type_w, name_w) = (14, 22);
        // First resolve the Name target column from the layout widths, then lay out
        // a composed row text so the literal name "field" sits exactly at that
        // column (fold(3) + type_w(14) + sep(1) = 18). This mirrors how the painter
        // and hit-test agree on the same column space.
        let name = geometry::resolved_span_for(&lm, "", EditTarget::Name, type_w, name_w);
        assert!(name.valid, "field name span must resolve");
        assert_eq!(name.start, 18, "name column begins at fold+type_w+sep = 18");
        let mut text = String::new();
        text.push_str("int32"); // type token
        while text.chars().count() < name.start as usize {
            text.push(' ');
        }
        text.push_str("field"); // name token at exactly name.start
        while text.chars().count() < name.end as usize {
            text.push(' ');
        }
        text.push_str("100"); // value filler
        let text = text.as_str();

        // A pixel mid-way through the name span's first cell → Name (via the exact
        // path `on_row_mouse_down` runs: `hit_test_row` with a text-local X).
        let name_x = (name.start as f32 + 0.5) * m.cell_width;
        let hit = hit_test_row(&lm, text, name_x, m, type_w, name_w);
        assert_eq!(
            hit.target,
            Some(EditTarget::Name),
            "click on the field name (col {}) must hit Name, got {:?}",
            hit.col,
            hit.target
        );

        // The seed text is the field name (so the inline edit opens over it).
        let seed = text
            .get(geometry::byte_for_col(text, name.start)..geometry::byte_for_col(text, name.end))
            .unwrap_or("")
            .trim();
        assert_eq!(seed, "field");

        // The overlay-left lands on the painted name column. Inside the text-region
        // wrapper, painted text column `name.start` and overlay-left
        // `ICON_CELLS*cell + name.start*cell` are the same pixel.
        let overlay_left = ICON_CELLS * m.cell_width + name.start as f32 * m.cell_width;
        let painted_text_col_x = ICON_CELLS * m.cell_width + name.start as f32 * m.cell_width;
        assert_eq!(overlay_left, painted_text_col_x);
    }

    #[test]
    fn type_token_resolves_to_an_edit_target_on_pointer_and_primitive_rows() {
        // BUG 4 regression: clicking the TYPE token on a pointer (`void*`) row or a
        // primitive (`uint32_t`) row must resolve to an editable `EditTarget` so
        // `begin_inline_edit` (or the type-selector popup) can fire — the QA could
        // not confirm a type-token edit opened on a `void*` row. The type column's
        // first glyph (just past the fold prefix) must always hit *some* target.
        let type_col = compose::K_FOLD_COL + 1;

        // Primitive: the whole type column is the generic Type target.
        let prim = field(NodeKind::UInt32, 0);
        let prim_text = "uint32_t      count                  0";
        assert_eq!(
            target_at_col(&prim, prim_text, type_col, 14, 22),
            Some(EditTarget::Type),
            "primitive type token must hit Type"
        );

        // Pointer: the type token resolves to a clickable target (Type or, where
        // the pointer-target sub-span overlaps, PointerTarget — both open an edit).
        let ptr = field(NodeKind::Pointer64, 0);
        let ptr_text = "void*         pData                  0x0";
        let hit = target_at_col(&ptr, ptr_text, type_col, 14, 22);
        assert!(
            matches!(
                hit,
                Some(EditTarget::Type) | Some(EditTarget::PointerTarget)
            ),
            "pointer type token must hit Type or PointerTarget, got {hit:?}"
        );
        assert!(hit.is_some(), "void* type token must be editable");
    }

    #[test]
    fn pointer_target_wins_over_type() {
        let lm = field(NodeKind::Pointer64, 0);
        // "Player*       enemy" — '*' after the type text; target span is ind..'*'.
        let text = "Player*       enemy                  0x0";
        let pts = compose::pointer_target_span_for(&lm, text);
        assert!(pts.valid);
        // A column inside the pointer-target span resolves to PointerTarget,
        // even though the generic Type span also overlaps it.
        assert_eq!(
            target_at_col(&lm, text, pts.start, 14, 22),
            Some(EditTarget::PointerTarget)
        );
    }

    #[test]
    fn fold_prefix_click_flagged_and_not_an_edit() {
        let mut lm = field(NodeKind::Struct, 0);
        lm.line_kind = LineKind::Header;
        lm.fold_head = true;
        let text = "\u{25BE} Player                              {";
        let hit = hit_test_row(&lm, text, 2.0, metrics(), 14, 22); // col 0
        assert!(hit.in_fold_col);
        assert_eq!(hit.target, None);
    }

    #[test]
    fn click_past_text_resolves_no_target() {
        let lm = field(NodeKind::Int32, 0);
        let text = "int32         field                  100";
        // Way past the end of the line.
        let hit = hit_test_row(&lm, text, 4000.0, metrics(), 14, 22);
        assert_eq!(hit.target, None);
    }

    #[test]
    fn full_hit_test_maps_address_and_type_columns() {
        // BUG 1 regression guard: a left-click on the command-row base ADDRESS and
        // on a node row's TYPE token must resolve through the *full pixel* hit test
        // (`hit_test_row`, the exact path `on_row_mouse_down` runs) to the right
        // `EditTarget`. The earlier address bug was a coordinate mismatch — the
        // pixel X handed to `hit_test_row` for the address overlay was in row space
        // (margin+icon offset) instead of text-local, so the resolved column missed
        // the `BaseAddress` span entirely. Here we drive the metrics path directly
        // with a text-local pixel X to lock the column→target mapping.
        let m = metrics(); // cell_width = 8.0
                           // ── Command-row base address ──
        let cmd = LineMeta {
            line_kind: LineKind::CommandRow,
            ..LineMeta::default()
        };
        let cmd_text = "[\u{25B8}] source\u{25BE}  0x400000  struct Foo {";
        let addr = compose::command_row_addr_span(cmd_text);
        assert!(addr.valid, "address span must resolve on the command row");
        // A text-local pixel X mid-way through the address span's first cell.
        let addr_x = (addr.start as f32 + 0.5) * m.cell_width;
        let addr_hit = hit_test_row(&cmd, cmd_text, addr_x, m, 14, 22);
        assert_eq!(
            addr_hit.target,
            Some(EditTarget::BaseAddress),
            "address column (col {}) must hit BaseAddress, got {:?}",
            addr_hit.col,
            addr_hit.target
        );
        // And the resolved column is the address span start (not shifted right by a
        // phantom margin/icon offset — the precise regression).
        assert_eq!(addr_hit.col, addr.start);

        // ── Node-row type token ──
        let node = field(NodeKind::Int32, 0);
        let node_text = "int32         field                  100";
        // Type column begins just past the fold prefix (K_FOLD_COL).
        let type_col = compose::K_FOLD_COL + 1;
        let type_x = (type_col as f32 + 0.5) * m.cell_width;
        let type_hit = hit_test_row(&node, node_text, type_x, m, 14, 22);
        assert_eq!(
            type_hit.target,
            Some(EditTarget::Type),
            "type column (col {}) must hit Type, got {:?}",
            type_hit.col,
            type_hit.target
        );
    }
}
