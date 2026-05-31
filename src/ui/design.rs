//! Zed design system — the shared design tokens + render helpers every GPUI
//! surface in the Reclass port uses.
//!
//! AESTHETIC TARGET = **Zed** (github.com/zed-industries/zed): the One Dark
//! palette, comfortable ~14px UI type + a real monospace editor font, a 4px
//! spacing grid, subtle 1px borders, 4–6px radii on elevated surfaces,
//! restrained shadows, low-contrast content-forward chrome, and
//! hover-overlay + soft-accent-selected interaction states.
//!
//! This module is the single source of truth for that language. It has two
//! halves:
//!
//! 1. [`tokens`] — plain `const`s (spacing / radius / border / shadow / type).
//!    These are theme-independent geometry + typography. They feed both the
//!    gpui-component global [`Theme`](gpui_component::Theme) (see
//!    [`crate::ui::theme_apply::apply_theme`], which copies the font/radius
//!    tokens into the global theme) AND the per-surface helpers below.
//! 2. Semantic color accessors ([`color`]) that READ from `cx.theme()`, so
//!    every surface stays themeable: a theme switch retints everything. They
//!    name the *semantic role* (chrome bg, panel bg, elevated surface, content
//!    bg, border, hover overlay, selected-accent bg, text/muted/disabled,
//!    accent, and the editor syntax kinds) rather than a raw gpui-component
//!    field, so a field rename upstream is a one-spot fix here.
//!
//! Plus a few tiny DRY builders ([`panel_header`], [`section_label`],
//! [`zed_list_row`], [`elevated_surface`]) that keep surfaces consistent.
//!
//! The written, prescriptive spec lives in `_design/zed_ui_spec.md`; this is
//! its executable counterpart. Keep the two in sync.
//!
//! Gated behind the `ui` feature (it pulls gpui / gpui-component).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, Div, FontWeight, Hsla, InteractiveElement as _, ParentElement as _, SharedString,
    Stateful, Styled,
};
use gpui_component::ActiveTheme;

/// The theme the app loads on launch — the Zed One Dark-styled bundled theme
/// (`src/theme/defaults/zed_one_dark.json`). Read by
/// [`crate::ui::theme_apply::ThemeRegistryGlobal::get`] to seed the selection.
pub const DEFAULT_THEME_NAME: &str = "Zed One Dark";

/// Theme-independent design tokens: geometry + typography on a 4px grid.
///
/// These are plain numbers (logical px / unitless) so they are usable both as
/// gpui [`Pixels`](gpui::Pixels) (wrap in `px(...)`) and when computing layout.
/// Color lives in [`color`] (it must read `cx.theme()` to stay themeable).
pub mod tokens {
    /// Spacing scale — a strict 4px grid (Zed's base unit). Use these instead of
    /// ad-hoc paddings so every gap/pad lands on the grid.
    pub mod space {
        /// 2px — hairline inset (icon nudge, key-cap padding).
        pub const XXS: f32 = 2.0;
        /// 4px — the base grid unit (tight inner padding, icon↔label gap).
        pub const XS: f32 = 4.0;
        /// 6px — compact row padding (list rows, menu items).
        pub const SM: f32 = 6.0;
        /// 8px — default control padding / small gap.
        pub const MD: f32 = 8.0;
        /// 12px — comfortable panel padding / section gap.
        pub const LG: f32 = 12.0;
        /// 16px — generous container padding (dialogs, cards).
        pub const XL: f32 = 16.0;
        /// 24px — large section separation (start page blocks).
        pub const XXL: f32 = 24.0;
    }

    /// Corner radii. Small on inline controls, 4–6px on elevated surfaces.
    pub mod radius {
        /// 3px — inline chips / key-caps / swatches.
        pub const SM: f32 = 3.0;
        /// 4px — buttons, inputs, list-row selection.
        pub const MD: f32 = 4.0;
        /// 6px — popovers / dropdowns / context menus.
        pub const LG: f32 = 6.0;
        /// 8px — modals / dialogs / elevated cards.
        pub const XL: f32 = 8.0;
        /// Fully round (pills).
        pub const FULL: f32 = 9999.0;
    }

    /// Border widths. Zed chrome is overwhelmingly 1px, low-contrast.
    pub mod border {
        /// 1px — the standard hairline border / separator.
        pub const THIN: f32 = 1.0;
        /// 2px — emphasized edge (focus ring, active-tab underline/accent bar).
        pub const THICK: f32 = 2.0;
    }

    /// Shadow elevation. Zed keeps shadows restrained; we mostly lean on the
    /// gpui-component `Theme::shadow` flag for elevated surfaces and expose a
    /// blur radius for any bespoke `box_shadow`.
    pub mod shadow {
        /// Popover/dropdown elevation blur (logical px).
        pub const POPOVER_BLUR: f32 = 12.0;
        /// Modal/dialog elevation blur (logical px).
        pub const MODAL_BLUR: f32 = 24.0;
        /// Shadow opacity (alpha) for the soft drop shadow.
        pub const ALPHA: f32 = 0.35;
    }

    /// Typography: a small UI type scale + the editor (monospace) sizing, with
    /// sensible cross-platform font families.
    pub mod font {
        /// 11px — micro labels (section captions, key-caps, status-bar fine print).
        pub const UI_XS: f32 = 11.0;
        /// 12px — secondary UI text (muted captions, tab labels).
        pub const UI_SM: f32 = 12.0;
        /// 14px — the default UI text size (Zed's comfortable body size).
        pub const UI_MD: f32 = 14.0;
        /// 16px — headings / dialog titles.
        pub const UI_LG: f32 = 16.0;

        /// 13px — the editor (monospace) glyph size.
        pub const EDITOR_SIZE: f32 = 13.0;
        /// Editor line-height multiple (× font size) — comfortable code leading.
        pub const EDITOR_LINE_HEIGHT: f32 = 1.4;
        /// UI line-height multiple (× font size).
        pub const UI_LINE_HEIGHT: f32 = 1.4;

        /// UI font family. A comma-separated preference list; gpui resolves the
        /// first available. Linux fallbacks first (Inter / Cantarell / DejaVu),
        /// then the OS UI font, then a generic.
        pub const UI_FAMILY: &str =
            "Inter, \"SF Pro Text\", \"Segoe UI\", Cantarell, \"Noto Sans\", \"DejaVu Sans\", sans-serif";

        /// Monospace (editor) font family. A real monospace with strong Linux
        /// fallbacks (JetBrains Mono / Fira Code / Cascadia / DejaVu Sans Mono),
        /// then the platform monospace, then generic.
        pub const MONO_FAMILY: &str =
            "\"JetBrains Mono\", \"Fira Code\", \"Cascadia Code\", \"Source Code Pro\", \"DejaVu Sans Mono\", Menlo, Consolas, monospace";
    }
}

/// Semantic color accessors — every one reads `cx.theme()` so surfaces stay
/// themeable (a theme switch retints them). Prefer these over reaching into raw
/// gpui-component `ThemeColor` fields: the role name documents intent and a
/// gpui-component field rename is fixed here once.
///
/// The roles cover the surfaces every chrome agent touches:
/// window/chrome bg, panel bg, elevated/popover surface, content/editor bg,
/// border, hover overlay, selected-accent bg, text/muted/disabled, accent, and
/// the editor SYNTAX kinds.
pub mod color {
    use super::*;

    // ── Surfaces ───────────────────────────────────────────────────────────

    /// Window / chrome background (titlebar, menubar, status bar) — the darkest
    /// shell. Maps to the theme `background`.
    pub fn chrome_bg(cx: &gpui::App) -> Hsla {
        cx.theme().background
    }

    /// Dockable panel background (workspace / scanner sidebars). Maps to
    /// `sidebar` (≈ chrome, intentionally near-identical for a flat look).
    pub fn panel_bg(cx: &gpui::App) -> Hsla {
        cx.theme().sidebar
    }

    /// Elevated surface (popover / dropdown / dialog / card) — one step lighter
    /// than chrome. Maps to `popover` (≙ our `backgroundAlt`).
    pub fn elevated_bg(cx: &gpui::App) -> Hsla {
        cx.theme().popover
    }

    /// Content / editor "paper" background. Maps to `background` (the editor
    /// surface darkens it slightly itself; see `ui/editor/palette.rs`).
    pub fn content_bg(cx: &gpui::App) -> Hsla {
        cx.theme().background
    }

    // ── Lines / overlays ─────────────────────────────────────────────────────

    /// The standard 1px border / separator color.
    pub fn border(cx: &gpui::App) -> Hsla {
        cx.theme().border
    }

    /// Focus ring color (focused input / active edit).
    pub fn focus_ring(cx: &gpui::App) -> Hsla {
        cx.theme().ring
    }

    /// Hover overlay background (row / tab / menu hover).
    pub fn hover_overlay(cx: &gpui::App) -> Hsla {
        cx.theme().list_hover
    }

    /// Selected-row background — the soft accent fill for the active list/tree
    /// row. Maps to `list_active` (our `selected`).
    pub fn selected_bg(cx: &gpui::App) -> Hsla {
        cx.theme().list_active
    }

    /// Text-selection background (the highlighted text span). Maps to
    /// `selection` (distinct from [`selected_bg`]).
    pub fn selection_bg(cx: &gpui::App) -> Hsla {
        cx.theme().selection
    }

    // ── Text ──────────────────────────────────────────────────────────────

    /// Primary content text.
    pub fn text(cx: &gpui::App) -> Hsla {
        cx.theme().foreground
    }

    /// Muted / secondary text (captions, inactive tabs, hints).
    pub fn text_muted(cx: &gpui::App) -> Hsla {
        cx.theme().muted_foreground
    }

    /// Disabled text — muted, further dimmed via alpha.
    pub fn text_disabled(cx: &gpui::App) -> Hsla {
        let mut c = cx.theme().muted_foreground;
        c.a = 0.55;
        c
    }

    // ── Accent ──────────────────────────────────────────────────────────────

    /// The brand accent (Zed action blue) — links, primary buttons, active
    /// underlines. Maps to `primary`.
    pub fn accent(cx: &gpui::App) -> Hsla {
        cx.theme().primary
    }

    /// Link / hover-span color.
    pub fn link(cx: &gpui::App) -> Hsla {
        cx.theme().link
    }

    // ── Editor syntax kinds ──────────────────────────────────────────────────
    //
    // These mirror `ui/editor/palette.rs`'s mapping so non-editor surfaces
    // (e.g. the type selector's per-kind tints, a syntax legend) can reuse the
    // exact same colors. The editor surface itself snapshots these per frame.

    /// Keyword (struct/class/enum keywords) — One Dark purple.
    pub fn syntax_keyword(cx: &gpui::App) -> Hsla {
        cx.theme().magenta
    }
    /// Type / class name — One Dark yellow.
    pub fn syntax_type(cx: &gpui::App) -> Hsla {
        cx.theme().yellow
    }
    /// Function / member name — content text.
    pub fn syntax_function(cx: &gpui::App) -> Hsla {
        cx.theme().blue
    }
    /// Numeric literal — One Dark orange (warm number).
    pub fn syntax_number(cx: &gpui::App) -> Hsla {
        cx.theme().yellow_light
    }
    /// String literal — One Dark green.
    pub fn syntax_string(cx: &gpui::App) -> Hsla {
        cx.theme().green
    }
    /// Comment — dim green/gray.
    pub fn syntax_comment(cx: &gpui::App) -> Hsla {
        cx.theme().green_light
    }
    /// Punctuation / operators — muted.
    pub fn syntax_punctuation(cx: &gpui::App) -> Hsla {
        cx.theme().muted_foreground
    }
    /// Address / offset column (the gray left margin digits).
    pub fn syntax_address(cx: &gpui::App) -> Hsla {
        cx.theme().muted_foreground
    }
}

// ── Reusable surface builders (keep chrome DRY + consistent) ─────────────────

/// A panel header strip: the small bold title row at the top of a dockable
/// panel (workspace / scanner). Content-forward — no heavy chrome, just a title
/// over the panel bg with a 1px bottom border.
///
/// Returns a styled [`Div`]; callers add trailing actions via `.child(...)`.
pub fn panel_header(title: impl Into<SharedString>, cx: &gpui::App) -> Div {
    let title: SharedString = title.into();
    gpui_component::h_flex()
        .h(px(28.0))
        .w_full()
        .px(px(tokens::space::LG))
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(color::border(cx))
        .bg(color::panel_bg(cx))
        .child(
            div()
                .flex_none()
                .text_size(px(tokens::font::UI_SM))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color::text_muted(cx))
                .child(title.to_uppercase()),
        )
}

/// A section caption label — the small uppercase muted divider used inside
/// dialogs / option pages / the theme editor (the C++ `makeSectionLabel`:
/// bold 11px, `textMuted`, bottom border). Group headers in a settings list.
pub fn section_label(text: impl Into<SharedString>, cx: &gpui::App) -> Div {
    let text: SharedString = text.into();
    div()
        .w_full()
        .pt(px(tokens::space::LG))
        .pb(px(tokens::space::XS))
        .mb(px(tokens::space::XS))
        .border_b_1()
        .border_color(color::border(cx))
        .text_size(px(tokens::font::UI_XS))
        .font_weight(FontWeight::BOLD)
        .text_color(color::text_muted(cx))
        .child(text.to_uppercase())
}

/// A list/tree/menu row shell with Zed interaction states: comfortable padding,
/// 4px radius, hover overlay, and a soft-accent selected fill. Callers add the
/// row content as children and wire `.on_click` themselves.
///
/// `id` must be unique within its parent (gpui interactivity requirement).
pub fn zed_list_row(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    cx: &gpui::App,
) -> Stateful<Div> {
    gpui_component::h_flex()
        .id(id)
        .w_full()
        .h(px(24.0))
        .px(px(tokens::space::MD))
        .gap(px(tokens::space::MD))
        .items_center()
        .rounded(px(tokens::radius::MD))
        .text_size(px(tokens::font::UI_MD))
        .text_color(color::text(cx))
        .when(selected, |r| r.bg(color::selected_bg(cx)))
        .when(!selected, |r| r.hover(|s| s.bg(color::hover_overlay(cx))))
}

/// An elevated surface container (popover / dropdown / dialog / card): the
/// `popover` bg, a 1px border, 6px radius, and the soft drop shadow. The base
/// shell every floating surface shares.
pub fn elevated_surface(cx: &gpui::App) -> Div {
    div()
        .bg(color::elevated_bg(cx))
        .border_1()
        .border_color(color::border(cx))
        .rounded(px(tokens::radius::LG))
        .shadow_md()
        .text_color(color::text(cx))
}

#[cfg(test)]
mod tests {
    // The token consts are gpui-free numbers; assert the grid + scale invariants
    // without pulling gpui into the test hygiene expansion.
    use super::tokens::{font, radius, space};
    use super::DEFAULT_THEME_NAME;

    #[test]
    fn spacing_is_on_a_grid_and_ascending() {
        let s = [
            space::XXS,
            space::XS,
            space::SM,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
        ];
        for w in s.windows(2) {
            assert!(w[0] < w[1], "spacing scale must strictly ascend");
        }
        // The grid unit is 4px; the larger steps are multiples of it.
        assert_eq!(space::XS, 4.0);
        assert_eq!(space::MD, 8.0);
        assert_eq!(space::XL, 16.0);
        assert_eq!(space::XXL, 24.0);
    }

    #[test]
    fn radii_ascend_and_match_zed_targets() {
        assert!(radius::SM < radius::MD);
        assert!(radius::MD < radius::LG);
        assert!(radius::LG < radius::XL);
        // Elevated surfaces sit in the 4–6px band.
        assert_eq!(radius::MD, 4.0);
        assert_eq!(radius::LG, 6.0);
    }

    #[test]
    fn type_scale_is_comfortable() {
        assert!(font::UI_XS < font::UI_SM);
        assert!(font::UI_SM < font::UI_MD);
        assert!(font::UI_MD < font::UI_LG);
        // The default UI size is the ~14px Zed body size.
        assert_eq!(font::UI_MD, 14.0);
        // The editor uses a real monospace family (contains a known mono).
        assert!(font::MONO_FAMILY.contains("Mono") || font::MONO_FAMILY.contains("Code"));
        assert!(font::MONO_FAMILY.contains("monospace"));
    }

    #[test]
    fn default_theme_name_is_zed_one_dark() {
        assert_eq!(DEFAULT_THEME_NAME, "Zed One Dark");
    }
}
