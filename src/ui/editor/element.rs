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
use super::hit_test::{self, CursorKind};
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
    /// The mouse-cursor shape to request over this row's hitbox while it is hovered
    /// (the C++ `applyHoverCursor` per-column shape, items 1/3). `None` when the row
    /// is not hovered or hover-cursor styling is suppressed (editing / hover off).
    cursor: Option<CursorKind>,
}

/// The per-column hover decision for a row under the cursor: the cursor shape to
/// request and the `[start,end)` char-column span to recolor link-blue (the C++
/// `IND_HOVER_SPAN`). Resolved once in `prepaint` from the editor's live line model
/// + the current mouse position (items 1/2/3, mirroring `applyHoverCursor`).
struct HoverDecision {
    cursor: CursorKind,
    /// The token/arrow/pill span to recolor (link-blue), if any.
    recolor: Option<(i32, i32)>,
}

impl RowElement {
    /// Resolve the cursor shape + hover-span recolor for this row, given the
    /// row-local mouse X (the C++ `applyHoverCursor`). Reads the editor's live line
    /// model so hit-testing/coloring stay in lock-step with the painted columns.
    /// Returns `None` when hover styling is suppressed (hover-effects off, or an
    /// inline edit is active — the field owns its own cursor then).
    fn hover_decision(&self, rel_x: f32, cx: &App) -> Option<HoverDecision> {
        let editor = self.editor.upgrade()?;
        let editor = editor.read(cx);
        // Suppress hover visuals while editing or when hover-effects are off (the
        // C++ keeps Arrow + skips the hover span in both cases; editor.cpp:4190).
        if editor.editing.is_some() || !editor.hover_effects() {
            return None;
        }
        let lm = editor.line_meta(self.line)?.clone();
        let text = editor.line_text_owned(self.line);
        let metrics = self.row.metrics;
        let (type_w, name_w) = geometry::effective_widths(&lm);
        let hit = hit_test::hit_test_row(&lm, &text, rel_x, metrics, type_w, name_w);
        let cursor = hit_test::cursor_for_hit(&lm, &text, hit);
        let recolor = geometry::hover_span_for(
            &lm,
            &text,
            hit.col,
            hit.in_fold_col,
            hit.target,
            type_w,
            name_w,
        );
        Some(HoverDecision { cursor, recolor })
    }
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
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let cell = self.row.metrics.cell_width;

        // Per-column hover (items 1/2/3): when the cursor is over this row, resolve
        // the cursor shape (IBeam / PointingHand / Arrow) and the hovered token's
        // column span to recolor link-blue (the C++ `applyHoverCursor` /
        // `IND_HOVER_SPAN`). Computed here from the live line model + current mouse
        // position so the recolor is folded into the shaped text below, and the
        // cursor request is carried through to `paint`.
        let mut hover_cursor: Option<CursorKind> = None;
        let mut hover_recolor: Option<(i32, i32)> = None;
        if hitbox.is_hovered(window) {
            let rel_x = f32::from(window.mouse_position().x - bounds.left()).max(0.0);
            if let Some(d) = self.hover_decision(rel_x, cx) {
                hover_cursor = Some(d.cursor);
                hover_recolor = d.recolor;
            }
        }

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
                cursor: hover_cursor,
            };
        }
        let style = window.text_style();
        let font = style.font();
        let font_size = style.font_size.to_pixels(window.rem_size());

        let runs = build_text_runs(
            &text,
            &self.row.runs,
            &self.row.palette,
            &font,
            hover_recolor,
        );
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
            cursor: hover_cursor,
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
        // row-local X and dispatch into the view (editor-surface.md §9). The left
        // button routes the normal click/edit/select path; the right button opens
        // the node context menu anchored at the cursor (reclass
        // `customContextMenuRequested`), recording the row as the menu's target.
        if let Some(hitbox) = prepaint.hitbox.take() {
            // Per-column cursor shape over the row's hitbox (items 1/3, the C++
            // `applyHoverCursor`): IBeam over editable Name/Value/Comment text,
            // PointingHand over pick-tokens / fold arrows / footer pills, Arrow over
            // column padding. Resolved in `prepaint` from the live hit test; only
            // requested while this row is the hovered one (else the cursor is left
            // to the default / another row's request).
            if let Some(kind) = prepaint.cursor.take() {
                let style = match kind {
                    CursorKind::Arrow => CursorStyle::Arrow,
                    CursorKind::IBeam => CursorStyle::IBeam,
                    CursorKind::PointingHand => CursorStyle::PointingHand,
                };
                window.set_cursor_style(style, &hitbox);
            }
            let editor = self.editor.clone();
            let line = self.line;
            let left = bounds.left();
            {
                let hitbox = hitbox.clone();
                let editor = editor.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }
                    match event.button {
                        MouseButton::Left => {
                            let rel_x = f32::from(event.position.x - left).max(0.0);
                            let modifiers = event.modifiers;
                            // `click_count == 2` is the double-click-to-edit
                            // affordance (item 26): select the token, then begin its
                            // edit/picker. A single click routes the normal path.
                            let double = event.click_count >= 2;
                            let _ = editor.update(cx, |this, cx| {
                                if double {
                                    this.dispatch_row_double_click(
                                        line, rel_x, modifiers, window, cx,
                                    );
                                } else {
                                    this.dispatch_row_click(line, rel_x, modifiers, window, cx);
                                }
                            });
                        }
                        MouseButton::Right => {
                            let pos = event.position;
                            let rel_x = f32::from(event.position.x - left).max(0.0);
                            let _ = editor.update(cx, |this, cx| {
                                this.dispatch_row_context_menu(line, rel_x, pos, window, cx);
                            });
                        }
                        _ => {}
                    }
                });
            }
            // Byte-selection drag (item 11): while the left button is held and the
            // pointer moves over this row's hitbox, extend the armed byte selection
            // to the byte under the cursor. The editor decides whether a selection
            // is armed (a no-op otherwise), so this is cheap. When NOT dragging, the
            // same move drives the hover popup (item 13): the editor resolves the
            // hovered column → value-history / disasm / struct-preview card.
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                let rel_x = f32::from(event.position.x - left).max(0.0);
                if event.dragging() {
                    let _ = editor.update(cx, |this, cx| {
                        this.dispatch_row_drag(line, rel_x, window, cx);
                    });
                } else {
                    let pos = event.position;
                    let _ = editor.update(cx, |this, cx| {
                        this.dispatch_row_hover(line, rel_x, pos, window, cx);
                    });
                }
            });
        }
    }
}

/// Convert char-column colored spans into UTF-8-byte-length `TextRun`s. The spans
/// are disjoint and sorted (guaranteed by `style_runs`); any gap defaults to the
/// text color. `shape_line` merges identical adjacent runs implicitly.
///
/// `hover_recolor` is the optional `[start,end)` **char-column** span of the token
/// under the cursor (item 2 / the C++ `IND_HOVER_SPAN`): any glyph inside it is
/// forced to the link-blue `palette.accent`, overriding its static role color so
/// the hovered editable token / fold arrow / footer pill reads as a link.
fn build_text_runs(
    text: &str,
    spans: &[SpanStyle],
    palette: &EditorPalette,
    font: &Font,
    hover_recolor: Option<(i32, i32)>,
) -> Vec<TextRun> {
    // Hover-recolor byte range (link-blue). Empty/invalid → no override.
    let hover_bytes: Option<(usize, usize)> = hover_recolor.and_then(|(s, e)| {
        if e <= s {
            return None;
        }
        let sb = geometry::byte_for_col(text, s);
        let eb = geometry::byte_for_col(text, e);
        (eb > sb).then_some((sb, eb))
    });
    let mk = |len: usize, color: Hsla| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    // Emit a `[start,end)` byte run with `base` color, but split out any
    // sub-portion that overlaps the hover range and paint it link-blue instead.
    let push_colored = |runs: &mut Vec<TextRun>, start: usize, end: usize, base: Hsla| {
        if end <= start {
            return;
        }
        match hover_bytes {
            Some((hs, he)) if he > start && hs < end => {
                let mid_s = hs.max(start);
                let mid_e = he.min(end);
                if mid_s > start {
                    runs.push(mk(mid_s - start, base));
                }
                runs.push(mk(mid_e - mid_s, palette.accent));
                if end > mid_e {
                    runs.push(mk(end - mid_e, base));
                }
            }
            _ => runs.push(mk(end - start, base)),
        }
    };
    if spans.is_empty() {
        let mut runs = Vec::new();
        push_colored(&mut runs, 0, text.len(), palette.text);
        return runs;
    }
    let mut runs = Vec::with_capacity(spans.len());
    let mut last_byte = 0usize;
    for span in spans {
        let start_byte = geometry::byte_for_col(text, span.start);
        let end_byte = geometry::byte_for_col(text, span.end);
        if start_byte > last_byte {
            push_colored(&mut runs, last_byte, start_byte, palette.text);
        }
        if end_byte > start_byte {
            push_colored(
                &mut runs,
                start_byte,
                end_byte,
                palette.role_color(span.role),
            );
            last_byte = end_byte;
        }
    }
    if last_byte < text.len() {
        push_colored(&mut runs, last_byte, text.len(), palette.text);
    }
    runs
}
