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
///
/// The roles map the C++ Scintilla indicator vocabulary (editor-surface.md §3)
/// onto **Zed One Dark** syntax hues (zed_ui_spec.md §1.3): types are yellow
/// (`#e5c07b`), function pointers blue (`#61afef`), keywords magenta (`#c678dd`),
/// numbers/addresses warm orange (`#d19a66`), comments dim green-gray (`#5c6370`),
/// the ASCII preview soft green (`#98c379`). Everything reads `cx.theme()` so a
/// theme switch instantly retints the editor.
#[derive(Copy, Clone, Debug)]
pub struct EditorPalette {
    pub text: Hsla,
    pub type_fg: Hsla,
    /// Function-pointer type token (`fnptr64`) — One Dark blue.
    pub fnptr_fg: Hsla,
    pub name_fg: Hsla,
    pub value_fg: Hsla,
    pub dim: Hsla,
    /// `struct`/`class`/`enum`/`void*` keyword — One Dark magenta/purple.
    pub keyword: Hsla,
    /// The root class name on the command row — One Dark blue (the screenshots
    /// render it in the link/accent blue, not teal).
    pub class_name: Hsla,
    /// Numeric literal / base address — One Dark warm orange.
    pub number: Hsla,
    /// The ASCII preview column on hex rows — soft One Dark green, dimmed.
    pub ascii: Hsla,
    pub comment_green: Hsla,
    pub type_hint: Hsla,
    pub rtti_hint: Hsla,
    pub enum_chip: Hsla,
    pub tree_conn: Hsla,
    /// Selection row background (`M_SELECTED`) — a soft accent-tinted fill, NOT
    /// the loud text-selection color (zed_ui_spec.md §5.4).
    pub selection_bg: Hsla,
    /// Left accent bar for selected rows (`M_ACCENT`).
    pub accent: Hsla,
    /// Hover row background (`M_HOVER`).
    pub hover_bg: Hsla,
    /// Caret color during inline edit.
    pub caret: Hsla,
    /// Editor paper (slightly darker than chrome, like `background.darker(115)`).
    pub paper: Hsla,
    /// The offset-margin / gutter background (a hair darker than the paper).
    pub gutter_bg: Hsla,
    /// The offset-margin digits — muted blue-gray Zed gutter text.
    pub gutter_fg: Hsla,
    /// Chip / pill background (the rounded fill behind footer + command-row pills).
    pub pill_bg: Hsla,
    /// Heat colors (cold/warm/hot) for the change heatmap — amber-leaning per the
    /// spec (§5.4 "heat backgrounds … amber, never red"), hottest reaches a warm
    /// red the screenshots use for freshly-changed bytes.
    pub heat_cold: Hsla,
    pub heat_warm: Hsla,
    pub heat_hot: Hsla,
    /// Byte-selection digit recolor (`IND_BYTE_SEL`).
    pub byte_sel: Hsla,
    /// The border color for chrome accents (chevron box, source chip outline).
    pub border: Hsla,
    /// The crisp fold disclosure triangle (`▸`/`▾`) on expandable rows — a clear
    /// foreground (not dim) so the fold affordance reads as a real control.
    pub fold_chevron: Hsla,
    /// The "active line" subtle highlight — the gentle full-row fill on the
    /// primary/cursor row (Zed's active-line background), softer than the
    /// accent-tinted selection fill so a selected row still reads as selected.
    pub active_line_bg: Hsla,
}

impl EditorPalette {
    /// Snapshot the palette from the active gpui-component theme.
    pub fn from_theme(cx: &gpui::App) -> Self {
        let t = cx.theme();
        EditorPalette {
            text: t.foreground,
            type_fg: t.yellow,
            fnptr_fg: t.blue,
            name_fg: t.foreground,
            // Resolved field values (the `0x7ff60baa84c0` pointer/address column)
            // render in the warm One Dark **number/orange** hue in the reclass
            // screenshots (PIC1 measures ≈ `#d19a66`, the `markerCycle`/`warning`
            // role), not green — green is reserved for comments/symbols.
            value_fg: t.warning,
            dim: t.muted_foreground,
            keyword: t.magenta,
            class_name: t.blue,
            // Base address / numeric literal — the true One Dark orange `#d19a66`
            // (`warning` ← `markerCycle`), matching the command-row address in PIC1.
            number: t.warning,
            ascii: with_alpha(t.green, 0.75),
            comment_green: t.green_light,
            type_hint: t.muted_foreground,
            rtti_hint: t.yellow,
            enum_chip: t.link,
            tree_conn: t.muted_foreground,
            // Soft accent-tinted selection fill (not the hard text-selection bg).
            // The reclass active row is a clear band; Zed uses a gentle accent
            // fill — 0.20 reads as a real "this row is selected" surface against
            // the dark editor paper while staying soft (zed_ui_spec.md §5.4).
            selection_bg: with_alpha(t.primary, 0.20),
            accent: t.primary,
            // Hover overlay: the theme `list_hover` is tuned for the dense UI
            // lists; lift it a touch so the per-row hover is visible over the
            // darker editor paper (the editor paper is `background.darker`).
            hover_bg: with_alpha(t.foreground, 0.05),
            caret: t.caret,
            paper: darker(t.background, 0.06),
            gutter_bg: darker(t.background, 0.06),
            gutter_fg: with_alpha(t.muted_foreground, 0.70),
            pill_bg: with_alpha(t.foreground, 0.06),
            heat_cold: t.blue_light,
            heat_warm: t.yellow_light,
            heat_hot: t.red,
            byte_sel: t.link,
            border: t.border,
            // The disclosure triangle reads in the link/accent blue — a crisp,
            // unmistakable fold control against the dim chrome around it.
            fold_chevron: t.link,
            // Active-line band: a barely-there foreground wash (Zed's active-line
            // bg), softer than the accent selection fill so the two are distinct.
            active_line_bg: with_alpha(t.foreground, 0.03),
        }
    }

    /// The foreground color for a static [`SpanRole`].
    pub fn role_color(&self, role: SpanRole) -> Hsla {
        match role {
            SpanRole::Text => self.text,
            SpanRole::Type => self.type_fg,
            SpanRole::FnPtr => self.fnptr_fg,
            SpanRole::Keyword => self.keyword,
            SpanRole::Address => self.number,
            SpanRole::Source => self.dim,
            SpanRole::Name => self.name_fg,
            SpanRole::Value => self.value_fg,
            SpanRole::Ascii => self.ascii,
            SpanRole::Dim => self.dim,
            SpanRole::ClassName => self.class_name,
            SpanRole::CommentGreen => self.comment_green,
            SpanRole::TypeHint => self.type_hint,
            SpanRole::RttiHint => self.rtti_hint,
            SpanRole::EnumChip => self.enum_chip,
            SpanRole::TreeConn => self.tree_conn,
            SpanRole::HeatCold => self.heat_cold,
            SpanRole::HeatWarm => self.heat_warm,
            SpanRole::HeatHot => self.heat_hot,
            SpanRole::FoldChevron => self.fold_chevron,
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

/// Apply an alpha to an `Hsla` (translucent tints: soft selection, dim ASCII).
fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}
