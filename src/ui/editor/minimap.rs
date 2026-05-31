//! The right-side **minimap**: a narrow scaled overview of the composed structure
//! (the Zed editor minimap + the C++ purple overview block in
//! `data_options.png` / `reclass_reclass_active.png`).
//!
//! It paints one tiny proportional bar per composed display line, colored by node
//! kind / line role, into a fixed-width column on the editor's right edge, with a
//! translucent **viewport indicator** rectangle marking the slice currently
//! visible in the editor. It is intentionally cheap: the bar list is derived once
//! per frame from the existing [`ComposeResult`](crate::core::ComposeResult) line
//! metas (no re-layout, no text shaping), and the whole thing is a single custom
//! [`Element`] that paints a stack of `fill` quads.
//!
//! Toggled by [`RcxEditor::set_minimap`](super::RcxEditor::set_minimap). The pure
//! geometry (line → bar Y, viewport slice → indicator rect) is unit-tested below.

use gpui::*;

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
    pub rows: Vec<MinimapRow>,
    pub chrome: MinimapChrome,
    /// Total composed line count (== `rows.len()`, carried explicitly for the
    /// viewport math so an empty document degrades gracefully).
    pub total: usize,
    /// First visible composed line (the scroll-top index).
    pub visible_start: usize,
    /// One past the last visible composed line.
    pub visible_end: usize,
}

/// The fixed minimap column width in pixels (a narrow Zed-style strip).
pub const MINIMAP_WIDTH: f32 = 64.0;

/// Per-bar vertical gap as a fraction of the per-line slot height (keeps the bars
/// from merging into a solid block at dense scales).
const BAR_GAP_FRAC: f32 = 0.25;

/// The `[top, bottom)` pixel Y of the viewport indicator within a column of
/// `height` px showing `total` lines, where `[start, end)` is visible. Pure; the
/// indicator is clamped to the column and is at least a few px tall so it stays
/// grabbable even on a huge structure. Returns `None` when there is nothing to
/// indicate (no lines, or the whole document fits).
pub fn viewport_rect(total: usize, start: usize, end: usize, height: f32) -> Option<(f32, f32)> {
    if total == 0 || height <= 0.0 {
        return None;
    }
    let end = end.min(total).max(start);
    // Whole document visible → no indicator needed.
    if start == 0 && end >= total {
        return None;
    }
    let per = height / total as f32;
    let top = start as f32 * per;
    let bottom = end as f32 * per;
    // Enforce a minimum height so the box is always visible/usable.
    let min_h = 6.0_f32.min(height);
    let (top, bottom) = if bottom - top < min_h {
        let mid = (top + bottom) * 0.5;
        let half = min_h * 0.5;
        ((mid - half).max(0.0), (mid - half).max(0.0) + min_h)
    } else {
        (top, bottom)
    };
    Some((top.clamp(0.0, height), bottom.clamp(0.0, height)))
}

/// The pixel Y span `[top, bottom)` of bar `idx` of `total`, within `height` px,
/// applying the inter-bar gap. Pure; unit-tested.
pub fn bar_y(idx: usize, total: usize, height: f32) -> (f32, f32) {
    if total == 0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let per = height / total as f32;
    let top = idx as f32 * per;
    let gap = per * BAR_GAP_FRAC;
    let bottom = (top + per - gap).min(height);
    (top.min(height), bottom.max(top.min(height)))
}

impl IntoElement for Minimap {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Minimap {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
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
        let pad = (w * 0.12).clamp(2.0, 8.0);
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
                point(bounds.origin.x, px(top + vt)),
                point(bounds.origin.x + bounds.size.width, px(top + vb)),
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
    }
}

#[cfg(test)]
mod tests {
    // Import only the pure functions under test — NOT `super::*`, which would pull
    // this module's `gpui::*` glob into the `#[test]` hygiene expansion and stress
    // the type-recursion budget on this nightly+gpui toolchain (the same defensive
    // pattern the sibling editor test modules use).
    use super::{bar_y, viewport_rect};

    #[test]
    fn bar_y_partitions_the_column_in_order() {
        // 4 bars over a 100px column: each slot is 25px tall, bars ordered top→down,
        // each shorter than its slot by the gap.
        let total = 4;
        let h = 100.0;
        let mut prev_top = -1.0;
        for i in 0..total {
            let (t, b) = bar_y(i, total, h);
            assert!(t > prev_top, "bars must descend: {i} top={t}");
            assert!(b > t, "bar has positive height");
            assert!(b <= t + 25.0 + 0.01, "bar fits its slot");
            assert!(b <= h + 0.01);
            prev_top = t;
        }
        // First bar starts at the top.
        assert_eq!(bar_y(0, total, h).0, 0.0);
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
        assert!(b - t >= 5.99, "min height enforced: {}", b - t);
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
}
