//! The per-row painting `Element`: shapes one display line's styled text, paints
//! the dynamic overlays (heat byte-runs, byte-selection digits) over it, and
//! registers the row's hitbox + mouse-down handler in **row-local** coordinates.
//!
//! This is the rendering substrate that replaces a Scintilla line: instead of
//! indicators it builds a `Vec<TextRun>` (one run per colored span, from
//! [`geometry::style_runs`](super::geometry::style_runs) resolved through the
//! [`palette`](super::palette)) plus background `PaintQuad`s for the per-byte heat
//! / selection overlays (editor-surface.md §5 steps 11–17, §12). Crucially, the
//! mouse-down hit test runs against the **element's own painted bounds**
//! (`bounds.localize`), so the click→column math is correct regardless of how the
//! parent docks/scroll inset the surface — matching the C++ which hit-tests in
//! viewport-local coordinates (editor-surface.md §8 `hitTest`).

use gpui::*;

use super::geometry::{self, CellMetrics, SpanStyle};
use super::palette::EditorPalette;
use super::RcxEditor;

/// A fully-resolved row ready to paint: the display text, the colored runs (in
/// `SpanStyle` form, resolved to `TextRun`s at paint time via the palette), and
/// the inline overlay quads. Built by the view's row builder.
pub struct RowPaint {
    pub text: SharedString,
    /// Colored spans in char-column coordinates.
    pub runs: Vec<SpanStyle>,
    /// Inline background overlays as `(char_start, char_end, color)` — heat byte
    /// runs and byte-selection digit highlights (drawn behind the glyphs).
    pub overlays: Vec<(i32, i32, Hsla)>,
    /// Rounded chip/pill backgrounds (`char_start, char_end, fill, border`) — the
    /// subtle Zed buttons behind footer add-bytes/Trim controls and the
    /// command-row chevron/source chips (editor-surface.md §5 step 14, PIC4/PIC5).
    pub pills: Vec<PillPaint>,
    pub palette: EditorPalette,
    pub metrics: CellMetrics,
}

/// A rounded pill background drawn behind a tail/command-row chip, in char-column
/// coordinates. Painted as a soft Zed button (low-alpha fill + 1px border).
#[derive(Copy, Clone, Debug)]
pub struct PillPaint {
    pub start: i32,
    pub end: i32,
    pub fill: Hsla,
    pub border: Hsla,
}

/// The leaf element that paints a single row's text + overlays and routes clicks.
pub struct RowElement {
    pub row: RowPaint,
    /// The owning editor view (for the click callback). Weak to avoid a cycle.
    pub editor: WeakEntity<RcxEditor>,
    /// This row's display-line index.
    pub line: usize,
}

pub struct RowPrepaint {
    line: Option<ShapedLine>,
    /// Rounded pill backgrounds (drawn first, behind the inline overlays + text).
    pills: Vec<PaintQuad>,
    overlays: Vec<PaintQuad>,
    hitbox: Option<Hitbox>,
}

impl IntoElement for RowElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RowElement {
    type RequestLayoutState = ();
    type PrepaintState = RowPrepaint;

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
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
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
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let cell = self.row.metrics.cell_width;

        // Rounded pill backgrounds (footer / command-row chips). Built whether or
        // not the line has text so an empty pill list is cheap; inset vertically
        // a touch so the chip reads as a Zed button, not a full-height block.
        let mut pills = Vec::with_capacity(self.row.pills.len());
        let pill_inset = (f32::from(bounds.bottom() - bounds.top()) * 0.14).clamp(1.0, 4.0);
        for pill in &self.row.pills {
            if pill.end <= pill.start {
                continue;
            }
            let x0 = bounds.left() + px(pill.start.max(0) as f32 * cell);
            let x1 = bounds.left() + px(pill.end.max(0) as f32 * cell);
            let pb = Bounds::from_corners(
                point(x0, bounds.top() + px(pill_inset)),
                point(x1, bounds.bottom() - px(pill_inset)),
            );
            pills.push(quad(
                pb,
                px(crate::ui::design::tokens::radius::MD),
                pill.fill,
                px(crate::ui::design::tokens::border::THIN),
                pill.border,
                BorderStyle::Solid,
            ));
        }

        let text = self.row.text.clone();
        if text.is_empty() {
            return RowPrepaint {
                line: None,
                pills,
                overlays: Vec::new(),
                hitbox: Some(hitbox),
            };
        }
        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());

        let runs = build_text_runs(&text, &self.row.runs, &self.row.palette, &font);
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);

        let mut overlays = Vec::with_capacity(self.row.overlays.len());
        for &(start, end, color) in &self.row.overlays {
            if end <= start {
                continue;
            }
            let x0 = bounds.left() + px(start.max(0) as f32 * cell);
            let x1 = bounds.left() + px(end.max(0) as f32 * cell);
            overlays.push(fill(
                Bounds::from_corners(point(x0, bounds.top()), point(x1, bounds.bottom())),
                color,
            ));
        }

        RowPrepaint {
            line: Some(line),
            pills,
            overlays,
            hitbox: Some(hitbox),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Pills furthest back (the chip "button" surface), then inline overlays
        // (heat / byte-selection), then the glyphs.
        for pill in prepaint.pills.drain(..) {
            window.paint_quad(pill);
        }
        for quad in prepaint.overlays.drain(..) {
            window.paint_quad(quad);
        }
        if let Some(line) = prepaint.line.take() {
            let _ = line.paint(
                bounds.origin,
                window.line_height(),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        // Row-local click routing: on press over this row's hitbox, compute the
        // row-local X and dispatch into the view (editor-surface.md §9).
        if let Some(hitbox) = prepaint.hitbox.take() {
            let editor = self.editor.clone();
            let line = self.line;
            let left = bounds.left();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let rel_x = f32::from(event.position.x - left).max(0.0);
                let modifiers = event.modifiers;
                let _ = editor.update(cx, |this, cx| {
                    this.dispatch_row_click(line, rel_x, modifiers, window, cx);
                });
            });
        }
    }
}

/// Convert char-column colored spans into UTF-8-byte-length `TextRun`s. The spans
/// are disjoint and sorted (guaranteed by `style_runs`); any gap defaults to the
/// text color. `shape_line` merges identical adjacent runs implicitly.
fn build_text_runs(
    text: &str,
    spans: &[SpanStyle],
    palette: &EditorPalette,
    font: &Font,
) -> Vec<TextRun> {
    let mk = |len: usize, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    if spans.is_empty() {
        return vec![mk(text.len(), palette.text)];
    }
    let mut runs = Vec::with_capacity(spans.len());
    let mut last_byte = 0usize;
    for span in spans {
        let start_byte = geometry::byte_for_col(text, span.start);
        let end_byte = geometry::byte_for_col(text, span.end);
        if start_byte > last_byte {
            runs.push(mk(start_byte - last_byte, palette.text));
        }
        if end_byte > start_byte {
            runs.push(mk(end_byte - start_byte, palette.role_color(span.role)));
            last_byte = end_byte;
        }
    }
    if last_byte < text.len() {
        runs.push(mk(text.len() - last_byte, palette.text));
    }
    runs
}
