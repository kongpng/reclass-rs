//! Editor color palette: resolve a [`SpanRole`](super::geometry::SpanRole) and a
//! heat level to concrete `Hsla` colors from the active gpui-component theme.
//!
//! The C++ editor resolved every Scintilla indicator foreground from the
//! [`Theme`](crate::theme) at `applyTheme` time (editor-surface.md §4
//! `applyTheme`). Here we map the editor's semantic roles onto the gpui-component
//! `ThemeColor` palette so theme switches are instant (every frame re-reads
//! `cx.theme()`). Kept tiny and gpui-only; no logic that needs unit testing.

use gpui::Hsla;
use gpui_component::ActiveTheme;

use super::geometry::SpanRole;

/// Resolved editor colors for one frame, snapshotted from the active theme.
#[derive(Copy, Clone, Debug)]
pub struct EditorPalette {
    pub text: Hsla,
    pub type_fg: Hsla,
    pub name_fg: Hsla,
    pub value_fg: Hsla,
    pub dim: Hsla,
    pub class_name: Hsla,
    pub comment_green: Hsla,
    pub type_hint: Hsla,
    pub rtti_hint: Hsla,
    pub enum_chip: Hsla,
    pub tree_conn: Hsla,
    /// Selection row background (`M_SELECTED`).
    pub selection_bg: Hsla,
    /// Left accent bar for selected rows (`M_ACCENT`).
    pub accent: Hsla,
    /// Hover row background (`M_HOVER`).
    pub hover_bg: Hsla,
    /// Caret color during inline edit.
    pub caret: Hsla,
    /// Editor paper (slightly darker than chrome, like `background.darker(115)`).
    pub paper: Hsla,
    /// Heat colors (cold/warm/hot) for the change heatmap.
    pub heat_cold: Hsla,
    pub heat_warm: Hsla,
    pub heat_hot: Hsla,
    /// Byte-selection digit recolor (`IND_BYTE_SEL`).
    pub byte_sel: Hsla,
}

impl EditorPalette {
    /// Snapshot the palette from the active gpui-component theme.
    pub fn from_theme(cx: &gpui::App) -> Self {
        let t = cx.theme();
        EditorPalette {
            text: t.foreground,
            type_fg: t.blue,
            name_fg: t.foreground,
            value_fg: t.green,
            dim: t.muted_foreground,
            class_name: t.cyan,
            comment_green: t.green_light,
            type_hint: t.muted_foreground,
            rtti_hint: t.yellow,
            enum_chip: t.link,
            tree_conn: t.muted_foreground,
            selection_bg: t.selection,
            accent: t.primary,
            hover_bg: t.list_hover,
            caret: t.caret,
            paper: darker(t.background, 0.06),
            heat_cold: t.blue_light,
            heat_warm: t.yellow,
            heat_hot: t.red,
            byte_sel: t.link,
        }
    }

    /// The foreground color for a static [`SpanRole`].
    pub fn role_color(&self, role: SpanRole) -> Hsla {
        match role {
            SpanRole::Text => self.text,
            SpanRole::Type => self.type_fg,
            SpanRole::Name => self.name_fg,
            SpanRole::Value => self.value_fg,
            SpanRole::Dim => self.dim,
            SpanRole::ClassName => self.class_name,
            SpanRole::CommentGreen => self.comment_green,
            SpanRole::TypeHint => self.type_hint,
            SpanRole::RttiHint => self.rtti_hint,
            SpanRole::EnumChip => self.enum_chip,
            SpanRole::TreeConn => self.tree_conn,
        }
    }

    /// Heat color for a heat level (1=cold, 2=warm, 3=hot), or `None` for static.
    pub fn heat_color(&self, level: i32) -> Option<Hsla> {
        match level {
            1 => Some(self.heat_cold),
            2 => Some(self.heat_warm),
            3 => Some(self.heat_hot),
            _ => None,
        }
    }
}

/// Darken an `Hsla` by reducing lightness, approximating Qt's `color.darker(115)`
/// (≈ multiply lightness by 1/1.15). Clamped to `[0,1]`.
fn darker(c: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (c.l - amount).clamp(0.0, 1.0),
        ..c
    }
}
