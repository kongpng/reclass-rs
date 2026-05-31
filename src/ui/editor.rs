//! The bespoke structured-editor surface — a custom raw-gpui `Element`.
//!
//! Port of `src/editor.{h,cpp}` (the QScintilla-backed grid). **DOCUMENTED
//! STUB.** Per the UI cookbooks this surface stays raw-gpui (not a
//! gpui-component widget): a virtualized grid of per-line styled monospace text
//! with multiple inline-editable column spans per row, continuous cross-row
//! byte/text selection, fold glyphs, per-byte heatmap backgrounds, tail chips,
//! and exact column geometry (`LineGeometry`). It will paint via
//! `window.text_system().shape_line(...)` + per-span `TextRun`s +
//! `EntityInputHandler` for inline edit + `paint_quad` for selection/cursor/heat
//! (`gpui_component_cookbook.md` §"Keep it custom"). The dedicated `ui` workflow
//! implements it (ARCHITECTURE.md §9, §10 risk: prototype early).

use gpui::*;

use crate::core::ComposeResult;

/// The editor surface view. Holds the last [`ComposeResult`] the controller
/// pushed; renders nothing meaningful yet (stub placeholder text).
#[derive(Default)]
pub struct RcxEditor {
    last_result: ComposeResult,
}

impl RcxEditor {
    pub fn new() -> Self {
        RcxEditor::default()
    }

    /// `RcxEditor::applyDocument(result)` — receive a fresh compose result.
    pub fn apply_document(&mut self, result: ComposeResult) {
        self.last_result = result;
    }

    pub fn last_result(&self) -> &ComposeResult {
        &self.last_result
    }
}

impl Render for RcxEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // STUB: the bespoke per-span styled-text Element is not yet implemented.
        div().size_full().child("editor surface (stub)")
    }
}
