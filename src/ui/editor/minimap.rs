//! The right-side **minimap**: a narrow scaled overview of the composed structure
//! (the Zed editor minimap + the C++ purple overview block in
//! `data_options.png` / `reclass_reclass_active.png`).
//!
//! It paints one tiny density bar per composed display line, colored by node kind /
//! line role, into a compact column on the editor's right edge, with a translucent
//! **viewport indicator** rectangle marking the slice currently visible in the
//! editor. Short documents retain minimap-scale rows instead of stretching into
//! full-height color blocks. The same document geometry drives painting and
//! click/drag navigation, so a pointer position maps to the line shown there.
//!
//! Toggled by [`RcxEditor::set_minimap`](super::RcxEditor::set_minimap). The pure
//! geometry (line → bar Y, viewport slice → indicator rect) is unit-tested below.

use gpui::*;
use std::sync::Arc;

use super::palette::EditorPalette;
use super::RcxEditor;
use crate::core::{LineKind, LineMeta, NodeKind};
use crate::ui::design::color::with_alpha;

/// One minimap bar: a single composed line reduced to a fill color + an indent
/// fraction (so nested rows read as shorter, left-inset bars, like Zed's minimap
/// glyph density). Built by the host from the line metas.
#[derive(Copy, Clone, Debug)]
pub struct MinimapRow {
    /// The bar fill color (resolved from the node-kind / line-role palette).
    pub color: Hsla,
    /// `[0,1]` horizontal start fraction of the bar (indent): deeper rows start
    /// further right so the overview shows the tree shape.
    pub indent: f32,
    /// `[0,1]` width fraction of the bar (how much of the column the bar spans).
    pub width: f32,
}

/// Horizontal density for one row. The actual trimmed display length is the main
/// signal; line/node kind only modulates it so a short hex preview stays quieter
/// than a long container declaration. Depth remains a separate left inset.
fn density_width(
    lm: &LineMeta,
    display_chars: usize,
    max_display_chars: usize,
    indent: f32,
) -> f32 {
    use NodeKind::*;
    let density = if max_display_chars == 0 {
        0.0
    } else {
        display_chars as f32 / max_display_chars as f32
    }
    .clamp(0.0, 1.0);
    let kind_scale = match lm.line_kind {
        LineKind::CommandRow => 0.85,
        LineKind::Footer => 0.58,
        LineKind::Header => 1.0,
        _ => match lm.node_kind {
            Struct | Array => 0.95,
            Pointer32 | Pointer64 | FuncPtr32 | FuncPtr64 => 0.88,
            Hex8 | Hex16 | Hex32 | Hex64 | Hex128 => 0.72,
            _ => 0.82,
        },
    };
    ((0.12 + density * 0.82) * kind_scale)
        .max(0.10)
        .min((1.0 - indent).max(0.10))
}

/// Reduce a composed [`LineMeta`] and its real trimmed display length to a
/// minimap bar. Color comes from node kind / line role; width comes from actual
/// content density, while depth supplies the left inset.
pub(crate) fn minimap_row_for(
    lm: &LineMeta,
    palette: &EditorPalette,
    display_chars: usize,
    max_display_chars: usize,
) -> MinimapRow {
    use NodeKind::*;
    // Depth → left indent fraction (cap so very deep rows still show a mark).
    let indent = (lm.depth.max(0) as f32 * 0.06).min(0.42);
    let (color, alpha) = match lm.line_kind {
        LineKind::CommandRow => (palette.class_name, 0.34),
        LineKind::Footer => (palette.dim, 0.22),
        LineKind::Header => {
            // Container headers remain the strongest landmarks, but are still
            // quiet enough not to compete with the editor text.
            let c = match lm.node_kind {
                Array => palette.type_fg,
                _ => palette.class_name,
            };
            (c, 0.46)
        }
        _ => {
            let (c, alpha) = match lm.node_kind {
                Struct => (palette.class_name, 0.40),
                Array => (palette.type_fg, 0.40),
                Pointer32 | Pointer64 => (palette.keyword, 0.38),
                FuncPtr32 | FuncPtr64 => (palette.fnptr_fg, 0.40),
                Hex8 | Hex16 | Hex32 | Hex64 | Hex128 => (palette.dim, 0.28),
                _ => (palette.value_fg, 0.34),
            };
            (c, alpha)
        }
    };
    MinimapRow {
        color: with_alpha(color, alpha),
        indent,
        width: density_width(lm, display_chars, max_display_chars, indent),
    }
}

/// Resolved colors for the minimap chrome (column background + viewport box),
/// snapshotted from the theme by the host so a theme switch retints it.
#[derive(Copy, Clone, Debug)]
pub struct MinimapChrome {
    /// The minimap column background (the faint panel behind the bars).
    pub bg: Hsla,
    /// The left separator hairline between the editor and the minimap.
    pub border: Hsla,
    /// The viewport indicator fill (the translucent "you are here" box).
    pub viewport: Hsla,
    /// The viewport indicator border.
    pub viewport_border: Hsla,
}

/// The minimap element. Paints `rows` scaled to fill its bounds vertically, plus
/// the viewport indicator for `[visible_start, visible_end)` over `total` lines.
pub struct Minimap {
    pub rows: Arc<[MinimapRow]>,
    pub chrome: MinimapChrome,
    /// Total composed line count (== `rows.len()`, carried explicitly for the
    /// viewport math so an empty document degrades gracefully).
    pub total: usize,
    /// First visible composed line (the scroll-top index).
    pub visible_start: usize,
    /// One past the last visible composed line.
    pub visible_end: usize,
    /// Owning editor, used only to route minimap click/drag/wheel navigation.
    pub editor: WeakEntity<RcxEditor>,
    /// Editor row height, used to convert wheel-line deltas consistently with
    /// the adjacent uniform list.
    pub line_height: f32,
}

/// The fixed minimap column width in pixels. This is an overview rail, not a
/// second content pane; keeping it narrow preserves the editor's hierarchy.
pub const MINIMAP_WIDTH: f32 = 32.0;

/// Short documents must not stretch one composed row into a tall color tile.
/// At most three physical pixels are reserved per row; long documents still
/// compress proportionally to fit the available track.
const MAX_ROW_SLOT_HEIGHT: f32 = 3.0;

/// Per-bar vertical gap as a fraction of the per-line slot height (keeps the bars
/// from merging into a solid block at dense scales).
const BAR_GAP_FRAC: f32 = 0.30;

/// Height occupied by the document map within the full minimap column. Small
/// documents use a compact top-aligned map; large documents consume the column.
pub fn map_height(total: usize, height: f32) -> f32 {
    if total == 0 || !height.is_finite() || height <= 0.0 {
        return 0.0;
    }
    (total as f32 * MAX_ROW_SLOT_HEIGHT).min(height)
}

/// The `[top, bottom)` pixel Y of the viewport indicator within a column of
/// `height` px showing `total` lines, where `[start, end)` is visible. Pure; the
/// indicator is clamped to the column and is at least a few px tall so it stays
/// grabbable even on a huge structure. Returns `None` when there is nothing to
/// indicate (no lines, or the whole document fits).
pub fn viewport_rect(total: usize, start: usize, end: usize, height: f32) -> Option<(f32, f32)> {
    let map_h = map_height(total, height);
    if map_h <= 0.0 {
        return None;
    }
    let start = start.min(total);
    let end = end.min(total).max(start);
    // Whole document visible → no indicator needed.
    if start == 0 && end >= total {
        return None;
    }
    let per = map_h / total as f32;
    let top = start as f32 * per;
    let bottom = end as f32 * per;
    // Enforce a minimum height so the box is always visible/usable.
    let min_h = 7.0_f32.min(map_h);
    let (top, bottom) = if bottom - top < min_h {
        let mid = (top + bottom) * 0.5;
        let adjusted_top = (mid - min_h * 0.5).clamp(0.0, map_h - min_h);
        (adjusted_top, adjusted_top + min_h)
    } else {
        (top, bottom)
    };
    Some((top.clamp(0.0, map_h), bottom.clamp(0.0, map_h)))
}

/// The pixel Y span `[top, bottom)` of bar `idx` of `total`, within `height` px,
/// applying the inter-bar gap. Pure; unit-tested.
pub fn bar_y(idx: usize, total: usize, height: f32) -> (f32, f32) {
    let map_h = map_height(total, height);
    if map_h <= 0.0 {
        return (0.0, 0.0);
    }
    let per = map_h / total as f32;
    let top = idx as f32 * per;
    let gap = per * BAR_GAP_FRAC;
    let bottom = (top + per - gap).min(map_h);
    (top.min(map_h), bottom.max(top.min(map_h)))
}

/// Map a pointer Y in minimap-local pixels to the composed line represented at
/// that position. Positions outside a compact short-document map clamp to the
/// nearest endpoint, matching click/drag navigation at the track edges.
pub fn line_at_y(total: usize, height: f32, y: f32) -> Option<usize> {
    let map_h = map_height(total, height);
    if map_h <= 0.0 || !y.is_finite() {
        return None;
    }
    let y = y.clamp(0.0, map_h);
    Some(((y / map_h * total as f32).floor() as usize).min(total - 1))
}

/// Negative uniform-list scroll offset that centers `line` in the editor
/// viewport, clamped at the document's top and bottom.
pub fn centered_scroll_offset(
    total: usize,
    line: usize,
    line_height: f32,
    viewport_height: f32,
) -> f32 {
    if total == 0
        || !line_height.is_finite()
        || line_height <= 0.0
        || !viewport_height.is_finite()
        || viewport_height <= 0.0
    {
        return 0.0;
    }
    let content_height = total as f32 * line_height;
    let max_scroll_top = (content_height - viewport_height).max(0.0);
    let line_center = (line.min(total - 1) as f32 + 0.5) * line_height;
    -(line_center - viewport_height * 0.5).clamp(0.0, max_scroll_top)
}

/// Apply a wheel delta to a negative uniform-list offset and clamp it to the
/// same document bounds used by click/drag navigation.
pub fn scroll_offset_after_delta(
    current: f32,
    delta: f32,
    total: usize,
    line_height: f32,
    viewport_height: f32,
) -> f32 {
    if !current.is_finite() || !delta.is_finite() {
        return 0.0;
    }
    let max_scroll_top = (total as f32 * line_height.max(0.0) - viewport_height.max(0.0)).max(0.0);
    (current + delta).clamp(-max_scroll_top, 0.0)
}

impl IntoElement for Minimap {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Minimap {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = px(MINIMAP_WIDTH).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let h = f32::from(bounds.size.height);
        let w = f32::from(bounds.size.width);
        let left = f32::from(bounds.origin.x);
        let top = f32::from(bounds.origin.y);

        // Column background.
        window.paint_quad(fill(bounds, self.chrome.bg));
        // Left separator hairline.
        window.paint_quad(fill(
            Bounds::from_corners(
                point(bounds.origin.x, bounds.origin.y),
                point(
                    bounds.origin.x + px(1.0),
                    bounds.origin.y + bounds.size.height,
                ),
            ),
            self.chrome.border,
        ));

        // A small horizontal inset so bars do not touch the column edges.
        let pad = (w * 0.14).clamp(3.0, 5.0);
        let inner_w = (w - 2.0 * pad).max(1.0);
        let total = self.total.max(self.rows.len());

        for (idx, row) in self.rows.iter().enumerate() {
            let (by_top, by_bottom) = bar_y(idx, total, h);
            if by_bottom <= by_top {
                continue;
            }
            let x0 = left + pad + inner_w * row.indent.clamp(0.0, 1.0);
            let bar_w = (inner_w * row.width.clamp(0.0, 1.0)).max(1.0);
            let x1 = (x0 + bar_w).min(left + w - pad);
            if x1 <= x0 {
                continue;
            }
            window.paint_quad(fill(
                Bounds::from_corners(
                    point(px(x0), px(top + by_top)),
                    point(px(x1), px(top + by_bottom)),
                ),
                row.color,
            ));
        }

        // Viewport indicator box.
        if let Some((vt, vb)) = viewport_rect(total, self.visible_start, self.visible_end, h) {
            let vbounds = Bounds::from_corners(
                point(bounds.origin.x + px(1.0), px(top + vt)),
                point(bounds.origin.x + bounds.size.width - px(1.0), px(top + vb)),
            );
            window.paint_quad(fill(vbounds, self.chrome.viewport));
            window.paint_quad(quad(
                vbounds,
                px(0.0),
                gpui::transparent_black(),
                px(1.0),
                self.chrome.viewport_border,
                BorderStyle::Solid,
            ));
        }

        window.set_cursor_style(CursorStyle::PointingHand, hitbox);

        // Click and captured drag both use the exact map geometry used above.
        // `capture_pointer` keeps edge clamping working when the drag leaves the
        // narrow rail; GPUI releases the capture automatically on mouse-up.
        let editor = self.editor.clone();
        let down_hitbox = hitbox.clone();
        let total = self.total;
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !down_hitbox.is_hovered(window)
            {
                return;
            }
            window.capture_pointer(down_hitbox.id);
            let local_y = f32::from(event.position.y - bounds.origin.y);
            if let Some(line) = line_at_y(total, h, local_y) {
                let _ = editor.update(cx, |this, cx| {
                    this.scroll_to_minimap_line(line, cx);
                });
            }
            cx.stop_propagation();
        });

        let editor = self.editor.clone();
        let drag_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || !event.dragging()
                || window.captured_hitbox() != Some(drag_hitbox.id)
            {
                return;
            }
            let local_y = f32::from(event.position.y - bounds.origin.y);
            if let Some(line) = line_at_y(total, h, local_y) {
                let _ = editor.update(cx, |this, cx| {
                    this.scroll_to_minimap_line(line, cx);
                });
            }
            cx.stop_propagation();
        });

        // The minimap sits beside (not inside) the uniform list, so forward plain
        // wheel deltas to the same scroll handle. Modified wheel events continue
        // bubbling to the editor's existing Ctrl/Cmd+wheel zoom handler.
        let editor = self.editor.clone();
        let wheel_hitbox = hitbox.clone();
        let wheel_line_height = px(self.line_height.max(1.0));
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || !wheel_hitbox.should_handle_scroll(window)
                || event.modifiers.control
                || event.modifiers.platform
            {
                return;
            }
            let delta = event.delta.pixel_delta(wheel_line_height);
            if delta.y != Pixels::ZERO {
                let _ = editor.update(cx, |this, cx| {
                    this.scroll_minimap_by(f32::from(delta.y), cx);
                });
            }
            cx.stop_propagation();
        });
    }
}

#[cfg(test)]
mod tests {
    // Import only the pure functions under test — NOT `super::*`, which would pull
    // this module's `gpui::*` glob into the `#[test]` hygiene expansion and stress
    // the type-recursion budget on this nightly+gpui toolchain (the same defensive
    // pattern the sibling editor test modules use).
    use super::{
        bar_y, centered_scroll_offset, density_width, line_at_y, map_height,
        scroll_offset_after_delta, viewport_rect,
    };
    use crate::core::LineMeta;

    #[test]
    fn bar_y_partitions_the_column_in_order() {
        // Four rows keep a compact 3px pitch at the top of a 100px column instead
        // of stretching into four dominant 25px tiles.
        let total = 4;
        let h = 100.0;
        let mut prev_top = -1.0;
        for i in 0..total {
            let (t, b) = bar_y(i, total, h);
            assert!(t > prev_top, "bars must descend: {i} top={t}");
            assert!(b > t, "bar has positive height");
            assert!(b <= t + 3.0 + 0.01, "bar fits its compact slot");
            assert!(b <= 12.0 + 0.01, "short-document map stays compact");
            prev_top = t;
        }
        // First bar starts at the top.
        assert_eq!(bar_y(0, total, h).0, 0.0);
        assert_eq!(map_height(total, h), 12.0);
    }

    #[test]
    fn bar_y_degenerate_inputs() {
        assert_eq!(bar_y(0, 0, 100.0), (0.0, 0.0));
        assert_eq!(bar_y(3, 4, 0.0), (0.0, 0.0));
    }

    #[test]
    fn viewport_none_when_all_visible_or_empty() {
        // Whole document visible → no indicator.
        assert_eq!(viewport_rect(10, 0, 10, 200.0), None);
        // Empty document.
        assert_eq!(viewport_rect(0, 0, 0, 200.0), None);
        // Zero height.
        assert_eq!(viewport_rect(10, 2, 5, 0.0), None);
    }

    #[test]
    fn viewport_maps_visible_slice_to_proportional_box() {
        // 100 lines over 200px → 2px/line. Lines [25,75) → [50,150).
        let (t, b) = viewport_rect(100, 25, 75, 200.0).unwrap();
        assert!((t - 50.0).abs() < 0.001, "top={t}");
        assert!((b - 150.0).abs() < 0.001, "bottom={b}");
    }

    #[test]
    fn viewport_enforces_minimum_height() {
        // A single visible line out of a huge doc would be sub-pixel; the box is
        // bumped to a minimum height so it stays grabbable, still clamped.
        let (t, b) = viewport_rect(10_000, 5000, 5001, 300.0).unwrap();
        assert!(b - t >= 6.99, "min height enforced: {}", b - t);
        assert!(t >= 0.0 && b <= 300.0);
    }

    #[test]
    fn viewport_clamps_end_past_total() {
        // end past total is clamped; start..total partial slice still indicated.
        let (t, b) = viewport_rect(50, 40, 999, 100.0).unwrap();
        // start 40/50 → 80px; end clamps to 50 → 100px.
        assert!((t - 80.0).abs() < 0.001, "top={t}");
        assert!((b - 100.0).abs() < 0.001, "bottom={b}");
    }

    #[test]
    fn pointer_mapping_uses_the_same_compact_track_as_painting() {
        // Four rows occupy y=[0,12]. Slot boundaries map to successive rows;
        // empty column space below the compact track clamps to the final row.
        assert_eq!(line_at_y(4, 100.0, -5.0), Some(0));
        assert_eq!(line_at_y(4, 100.0, 2.99), Some(0));
        assert_eq!(line_at_y(4, 100.0, 3.0), Some(1));
        assert_eq!(line_at_y(4, 100.0, 7.0), Some(2));
        assert_eq!(line_at_y(4, 100.0, 12.0), Some(3));
        assert_eq!(line_at_y(4, 100.0, 90.0), Some(3));
        assert_eq!(line_at_y(0, 100.0, 0.0), None);
    }

    #[test]
    fn real_display_length_controls_bar_density() {
        let lm = LineMeta::default();
        let short = density_width(&lm, 12, 120, 0.0);
        let medium = density_width(&lm, 60, 120, 0.0);
        let long = density_width(&lm, 120, 120, 0.0);
        assert!(
            short < medium && medium < long,
            "{short} < {medium} < {long}"
        );

        let nested = density_width(&lm, 120, 120, 0.42);
        assert!(nested <= 0.5801, "depth leaves horizontal room: {nested}");
    }

    #[test]
    fn centered_scroll_and_wheel_offsets_clamp_to_document() {
        assert_eq!(centered_scroll_offset(100, 0, 10.0, 100.0), 0.0);
        assert_eq!(centered_scroll_offset(100, 50, 10.0, 100.0), -455.0);
        assert_eq!(centered_scroll_offset(100, 99, 10.0, 100.0), -900.0);
        assert_eq!(centered_scroll_offset(5, 4, 10.0, 100.0), 0.0);

        assert_eq!(
            scroll_offset_after_delta(-400.0, -80.0, 100, 10.0, 100.0),
            -480.0
        );
        assert_eq!(
            scroll_offset_after_delta(-880.0, -80.0, 100, 10.0, 100.0),
            -900.0
        );
        assert_eq!(
            scroll_offset_after_delta(-20.0, 80.0, 100, 10.0, 100.0),
            0.0
        );
    }
}
