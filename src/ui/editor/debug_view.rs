//! The editor's read-only DEBUG surface (item 75, `VM_Debug`) — the
//! `generateDebugText`-style per-line dump — extracted from editor/mod.rs into a
//! second `impl RcxEditor`. A child module of `editor`, it keeps full access to
//! the editor's private fields and methods.

use super::EditorPalette;
use super::*;
use gpui::*;

impl super::RcxEditor {
    /// Build the DEBUG dump lines for the current compose result (the C++
    /// `generateDebugText`, main.cpp:5534) — one `margin|text  ## meta` string per
    /// composed line. The comment / type-hint columns are derived from the line's
    /// chips (Rust uses chips where the C++ kept `commentStart`/`typeHintStart`).
    fn build_debug_lines(&self) -> Vec<String> {
        let result = self.controller.last_result();
        let mut out = Vec::with_capacity(result.meta.len());
        for (i, lm) in result.meta.iter().enumerate() {
            let margin = lm.offset_text.clone();
            let text = self.line_text_owned(i);
            // Comment / type-hint chip start columns (or -1 if absent).
            let comment_col = lm
                .chips
                .iter()
                .find(|c| {
                    matches!(
                        c.kind,
                        crate::core::linemeta::ChipKind::Comment
                            | crate::core::linemeta::ChipKind::AddComment
                    )
                })
                .map(|c| c.start_col)
                .unwrap_or(-1);
            let hint_col = lm
                .chips
                .iter()
                .find(|c| c.kind == crate::core::linemeta::ChipKind::TypeHint)
                .map(|c| c.start_col)
                .unwrap_or(-1);
            out.push(geometry::debug_line(
                &margin,
                &text,
                lm,
                i,
                comment_col,
                hint_col,
            ));
        }
        out
    }

    /// Render the DEBUG surface (item 75): a virtualized monospace list of the
    /// debug dump lines, styled in the dim editor mono palette. Read-only.
    pub(super) fn render_debug_surface(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = EditorPalette::from_theme(cx);
        let lines = self.build_debug_lines();
        let line_h = self.metrics.line_height;
        let col_count = lines.len();
        let lines_rc = std::rc::Rc::new(lines);
        div()
            .id("rcx-debug-surface")
            .size_full()
            .bg(palette.paper)
            .text_color(palette.dim)
            .text_size(px(self.editor_font_size()))
            .font_family(self.editor_font_family())
            .child(
                uniform_list(
                    "rcx-debug-rows",
                    col_count,
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, _cx| {
                        let lines = lines_rc.clone();
                        range
                            .map(|ix| {
                                let text = lines.get(ix).cloned().unwrap_or_default();
                                div()
                                    .h(px(line_h))
                                    .px(px(4.0))
                                    .whitespace_nowrap()
                                    .child(SharedString::from(text))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }
}
