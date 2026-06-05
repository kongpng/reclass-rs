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
    div, px, AnyElement, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Stateful, Styled,
};
use gpui_component::ActiveTheme;

/// Coalesce `text` + a sorted/raw list of matched character `positions` (from the
/// fuzzy matcher) into render spans: matched chars are tinted `accent` + semibold,
/// the rest `base`. Runs of same-state chars are merged into one element. Shared by
/// the command palette, the type selector, and the enum picker (was three
/// byte-identical private copies).
pub fn highlighted_spans(
    text: &str,
    positions: &[usize],
    base: Hsla,
    accent: Hsla,
) -> Vec<AnyElement> {
    let pos: std::collections::BTreeSet<usize> = positions.iter().copied().collect();
    let mut spans: Vec<AnyElement> = Vec::new();
    let mut cur = String::new();
    let mut cur_hit: Option<bool> = None;
    let flush = |spans: &mut Vec<AnyElement>, text: &str, hit: bool| {
        if text.is_empty() {
            return;
        }
        let mut el = div().child(text.to_string());
        if hit {
            el = el.text_color(accent).font_weight(FontWeight::SEMIBOLD);
        } else {
            el = el.text_color(base);
        }
        spans.push(el.into_any_element());
    };
    for (i, ch) in text.chars().enumerate() {
        let hit = pos.contains(&i);
        if cur_hit != Some(hit) {
            if let Some(prev) = cur_hit {
                flush(&mut spans, &cur, prev);
            }
            cur.clear();
            cur_hit = Some(hit);
        }
        cur.push(ch);
    }
    if let Some(prev) = cur_hit {
        flush(&mut spans, &cur, prev);
    }
    spans
}

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

        /// Candidate monospace families for the editor, best-first. gpui's
        /// `font_family` takes a SINGLE family name — it does NOT parse a CSS-style
        /// comma fallback list, so passing one silently falls back to the default
        /// PROPORTIONAL font, where the space glyph is narrower than a digit and the
        /// editor's fixed-cell column grid (offsets/types/names/values + the
        /// inline-edit overlay + mouse hit-test, all `col * cell_width`) drifts off
        /// the painted glyphs. We therefore resolve ONE real, installed monospace at
        /// startup ([`resolve_mono_family`]) and use it everywhere ([`mono_family`]).
        pub const MONO_CANDIDATES: &[&str] = &[
            "JetBrains Mono",
            "JetBrainsMono Nerd Font",
            "Fira Code",
            "FiraCode Nerd Font",
            "Cascadia Code",
            "Cascadia Mono",
            "Source Code Pro",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Roboto Mono",
            "Noto Sans Mono",
            "Ubuntu Mono",
            "Menlo",
            "SF Mono",
            "Consolas",
            "Courier New",
            "monospace",
        ];

        static RESOLVED_MONO: std::sync::OnceLock<String> = std::sync::OnceLock::new();

        /// Resolve the editor monospace family from the platform's installed fonts
        /// (call ONCE at startup with `cx.text_system().all_font_names()`). Picks the
        /// first [`MONO_CANDIDATES`] entry that is actually installed; failing that,
        /// the first family whose name signals monospace; else the generic
        /// `"monospace"`. Idempotent.
        pub fn resolve_mono_family(available: &[String]) {
            let _ = RESOLVED_MONO.get_or_init(|| {
                for cand in MONO_CANDIDATES {
                    if available.iter().any(|f| f == cand) {
                        return (*cand).to_string();
                    }
                }
                if let Some(m) = available.iter().find(|f| {
                    let l = f.to_lowercase();
                    l.contains("mono") || l.contains("code") || l.contains("consol")
                }) {
                    return m.clone();
                }
                "monospace".to_string()
            });
        }

        /// The resolved editor monospace family — a SINGLE real family gpui can load.
        /// Before [`resolve_mono_family`] runs it returns the first candidate.
        pub fn mono_family() -> &'static str {
            RESOLVED_MONO
                .get()
                .map(String::as_str)
                .unwrap_or(MONO_CANDIDATES[0])
        }
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

    /// Return `c` with its alpha replaced by `a` — the shared translucent-tint
    /// helper (heat / byte-selection overlays, soft selections, dim ASCII).
    /// `Hsla.a` is an unclamped `f32`, so this is identical to `let mut c=…; c.a=a`.
    pub fn with_alpha(c: Hsla, a: f32) -> Hsla {
        Hsla { a, ..c }
    }

    /// The menu-bar dropdown's hover / keyboard-highlight background — a soft
    /// accent wash that reads on the dropdown's lighter elevated surface (where
    /// the theme's `list_hover` / `list_active` overlays are nearly invisible).
    pub fn menu_highlight(cx: &gpui::App) -> Hsla {
        with_alpha(accent(cx), 0.30)
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

/// Curated icon helpers — the small, named set of [`gpui_component::Icon`]s the
/// Reclass surfaces actually need, each pinned to a verified
/// [`IconName`](gpui_component::IconName) variant and pre-styled with a sensible
/// default size/color from [`tokens`].
///
/// WHY a curated layer (not raw `IconName`): `IconName` exposes ~100 Lucide
/// variants whose names don't match our DOMAIN words (a struct field is a
/// `Frame`, a pointer is an `ArrowRight`, an array is a `LayoutDashboard`…).
/// Centralizing the mapping here means (a) surfaces speak in domain terms,
/// (b) the icon→variant choice is reviewed in ONE place, and (c) every icon
/// gets a consistent default tint/size. Each fn documents the exact `IconName`
/// it maps to so the choice is auditable.
///
/// Every variant referenced here is backed by a real SVG in the
/// `gpui-component-assets` bundle (registered in `main.rs` via
/// `application().with_assets(gpui_component_assets::Assets)`); without that
/// asset source these would render as empty boxes.
///
/// Default styling: icons render at the UI body size ([`tokens::font::UI_MD`],
/// ~16px → gpui-component's `medium`) unless a caller resizes via the
/// [`Sizable`](gpui_component::Sizable) trait, and are left UNtinted so they
/// inherit the parent's text color — except the few semantic-status helpers
/// ([`info`], [`success`]) which carry an intrinsic tint. Callers may always
/// override with `.text_color(..)` / `.with_size(..)`.
pub mod icon {
    use gpui_component::{Icon, IconName};

    /// Disclosure triangle pointing right (a collapsed tree node).
    /// Maps to [`IconName::ChevronRight`].
    pub fn chevron_right() -> Icon {
        Icon::new(IconName::ChevronRight)
    }

    /// Disclosure triangle pointing down (an expanded tree node).
    /// Maps to [`IconName::ChevronDown`].
    pub fn chevron_down() -> Icon {
        Icon::new(IconName::ChevronDown)
    }

    /// A struct / object / class node — a framed container.
    /// Maps to [`IconName::Frame`].
    pub fn struct_() -> Icon {
        Icon::new(IconName::Frame)
    }

    /// Alias of [`struct_`] for surfaces that speak in "object" terms.
    /// Maps to [`IconName::Frame`].
    pub fn object() -> Icon {
        Icon::new(IconName::Frame)
    }

    /// A pointer member (dereferenceable address → an arrow to the target).
    /// Maps to [`IconName::ArrowRight`].
    pub fn pointer() -> Icon {
        Icon::new(IconName::ArrowRight)
    }

    /// An array member (a grid of repeated cells).
    /// Maps to [`IconName::LayoutDashboard`].
    pub fn array() -> Icon {
        Icon::new(IconName::LayoutDashboard)
    }

    /// A raw hex / byte cell (the hex-bytes column).
    /// Maps to [`IconName::MemoryStick`].
    pub fn hex() -> Icon {
        Icon::new(IconName::MemoryStick)
    }

    /// A scalar value leaf (a single small mark).
    /// Maps to [`IconName::Dash`].
    pub fn value() -> Icon {
        Icon::new(IconName::Dash)
    }

    /// A function / method member (code).
    /// Maps to [`IconName::SquareTerminal`].
    pub fn function() -> Icon {
        Icon::new(IconName::SquareTerminal)
    }

    /// An enum member (a list of named cases).
    /// Maps to [`IconName::Menu`].
    pub fn enum_() -> Icon {
        Icon::new(IconName::Menu)
    }

    /// A search affordance (the find bar / quick-open).
    /// Maps to [`IconName::Search`].
    pub fn search() -> Icon {
        Icon::new(IconName::Search)
    }

    /// A scan affordance (the value/pattern scanner — a search variant).
    /// Maps to [`IconName::Search`].
    pub fn scan() -> Icon {
        Icon::new(IconName::Search)
    }

    /// A refresh / re-scan action (circular arrow).
    /// Maps to [`IconName::Redo`].
    pub fn refresh() -> Icon {
        Icon::new(IconName::Redo)
    }

    /// A close / dismiss action (the ✕ glyph).
    /// Maps to [`IconName::Close`].
    pub fn close() -> Icon {
        Icon::new(IconName::Close)
    }

    /// An add / new action (the ＋ glyph).
    /// Maps to [`IconName::Plus`].
    pub fn plus() -> Icon {
        Icon::new(IconName::Plus)
    }

    /// A data source / backing store (the document's data provider).
    /// Maps to [`IconName::HardDrive`].
    pub fn source() -> Icon {
        Icon::new(IconName::HardDrive)
    }

    /// Alias of [`source`] for surfaces that speak in "database" terms.
    /// Maps to [`IconName::HardDrive`].
    pub fn database() -> Icon {
        Icon::new(IconName::HardDrive)
    }

    /// A confirm / done / enabled mark (the ✓ glyph).
    /// Maps to [`IconName::Check`].
    pub fn check() -> Icon {
        Icon::new(IconName::Check)
    }

    /// A settings / preferences affordance (the gear).
    /// Maps to [`IconName::Settings`].
    pub fn settings() -> Icon {
        Icon::new(IconName::Settings)
    }

    /// An informational marker, tinted with the brand accent.
    /// Maps to [`IconName::Info`] (tinted via [`super::color::accent`]).
    pub fn info(cx: &gpui::App) -> Icon {
        use gpui::Styled as _;
        Icon::new(IconName::Info).text_color(super::color::accent(cx))
    }

    /// A success / OK status marker, tinted green.
    /// Maps to [`IconName::CircleCheck`] (tinted via the theme `success`).
    pub fn success(cx: &gpui::App) -> Icon {
        use gpui::Styled as _;
        use gpui_component::ActiveTheme as _;
        Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
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
/// A context-menu row: a left label and a right-aligned accelerator hint (shown
/// only when `keys` is non-empty). Shared by the tab-bar and workspace menus.
pub fn menu_accel_row(
    label: &'static str,
    keys: &'static str,
    min_w: f32,
    cx: &gpui::App,
) -> Div {
    gpui_component::h_flex()
        .w_full()
        .min_w(px(min_w))
        .gap(px(tokens::space::LG))
        .items_center()
        .justify_between()
        .child(div().flex_1().child(label))
        .when(!keys.is_empty(), |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::text_disabled(cx))
                    .child(keys),
            )
        })
}

/// A Zed key-cap chip (`zed_ui_spec.md` §6): a small `SM`-radius, `UI_XS`,
/// hairline-bordered muted pill for one keystroke token. Shared by the command
/// palette row end-slot and the welcome-page action rows.
pub fn key_cap(keys: impl Into<SharedString>, cx: &gpui::App) -> Div {
    div()
        .flex_none()
        .px(px(tokens::space::SM))
        .h(px(18.))
        .min_w(px(18.))
        .rounded(px(tokens::radius::SM))
        .bg(color::hover_overlay(cx))
        .border_1()
        .border_color(color::border(cx))
        .text_size(px(tokens::font::UI_XS))
        .text_color(color::text_muted(cx))
        .flex()
        .items_center()
        .justify_center()
        .child(keys.into())
}

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
        // The editor resolves to a single real monospace family (a CSS-style comma
        // list silently falls back to a proportional font in gpui). The candidate
        // list is best-first and ends in the generic fallback.
        assert!(font::MONO_CANDIDATES.len() > 3);
        assert_eq!(*font::MONO_CANDIDATES.last().unwrap(), "monospace");
        // Before resolution, `mono_family()` returns the first candidate.
        assert_eq!(font::mono_family(), font::MONO_CANDIDATES[0]);
    }

    #[test]
    fn default_theme_name_is_zed_one_dark() {
        assert_eq!(DEFAULT_THEME_NAME, "Zed One Dark");
    }
}
