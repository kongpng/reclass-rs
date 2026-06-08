//! Editor color palette: resolve a [`SpanRole`](super::geometry::SpanRole) and a
//! heat level to concrete `Hsla` colors from the active gpui-component theme.
//!
//! The C++ editor resolved every Scintilla indicator foreground from the
//! [`Theme`](crate::theme) at `applyTheme` time (editor-surface.md §4
//! `applyTheme`). Here we map the editor's semantic roles onto the gpui-component
//! `ThemeColor` palette so theme switches are instant (every frame re-reads
//! `cx.theme()`). Kept tiny and gpui-only; no logic that needs unit testing.

use crate::ui::design::color::with_alpha;
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
    /// Item 37: a string-kind field value — the C++ `syntaxString` orange-tan
    /// (≈ #ce9178), distinct from the green numeric value color.
    pub string_val: Hsla,
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
    /// The crisp fold disclosure triangle (`▸`/`▾`) on expandable rows — painted
    /// as the TEXT glyph compose bakes into the fold prefix, on the row baseline
    /// (B1: C++ has no SVG icon gutter). A clear link/accent foreground so the fold
    /// affordance reads as a real disclosure control.
    pub fold_chevron: Hsla,
    /// The "active line" subtle highlight — the gentle full-row fill on the
    /// primary/cursor row (Zed's active-line background), softer than the
    /// accent-tinted selection fill so a selected row still reads as selected.
    pub active_line_bg: Hsla,
    /// Item 71: the `M_ERR` row band — a translucent red wash painted behind a row
    /// whose live inline edit is INVALID (the C++ `theme.markerError`). Reads as a
    /// clear error surface while leaving the row text legible.
    pub error_bg: Hsla,
    /// Item 71: the bright error foreground for the '! <error>' hint comment (the
    /// C++ `theme.markerError` text).
    pub error_fg: Hsla,
    /// Item 74: the presentation-mode focus-glow base color (the C++
    /// `theme.focusGlow`). The pulsing band lerps its alpha from this each tick.
    pub focus_glow: Hsla,
}

impl EditorPalette {
    /// Snapshot the palette from the active gpui-component theme.
    pub fn from_theme(cx: &gpui::App) -> Self {
        let t = cx.theme();
        // Light themes (e.g. the XP-Luna "Light" theme — background lightness
        // > 0.78) get pure-white editor paper instead of a darkened tint, matching
        // the C++ `applyTheme` rule (`lightnessF() > 0.78 ? #FFFFFF : darker(115)`)
        // so the body stays crisp instead of a dirty khaki that fights the chrome.
        let paper = if t.background.l > 0.78 {
            rgb_u8(0xff, 0xff, 0xff)
        } else {
            darker(t.background, 0.06)
        };
        EditorPalette {
            text: t.foreground,
            // Item 21: primitive type tokens (uint8_t/hex64/…) render BLUE (the C++
            // lexer keyword color, `syntaxKeyword`), not yellow/gold. Named struct
            // types are retinted teal at the span level (see `class_name`/teal).
            type_fg: t.blue,
            fnptr_fg: t.blue,
            name_fg: t.foreground,
            // Item 20: field VALUE column renders the C++ green/tan number color
            // (the lexer `Number` hue), not orange. Use the green-leaning syntax
            // number color so resolved values read green/tan like C++.
            value_fg: t.green,
            // Item 37: string values render in the `syntaxString` orange-tan
            // (≈ #ce9178); the theme orange (`warning`) is the closest theme-driven
            // hue, so retint it toward the softer tan.
            string_val: rgb_u8(0xce, 0x91, 0x78),
            // Item 29: the dim role (hex byte run + ASCII preview + braces/footer)
            // uses the deeper C++ `textFaint`, not the brighter muted foreground.
            dim: text_faint(t.muted_foreground),
            // Item 39: the C++ colors `struct`/`class`/`enum`/`union`/pointer C++
            // keyword tokens with `syntaxKeyword` (≈ #569cd6 blue), NOT magenta —
            // magenta is `syntaxPreproc`, used only for `#define` lines. Tie the
            // keyword role to the blue lexer-keyword hue.
            keyword: t.blue,
            // Item 23: the root class NAME on the command row is the C++
            // `IND_CLASS_NAME = syntaxType` (teal) — identical to the named-type
            // column color — not link-blue. Tie it to the teal type/struct color.
            class_name: t.cyan,
            // Base address / numeric literal — the true One Dark orange `#d19a66`
            // (`warning` ← `markerCycle`), matching the command-row address in PIC1.
            number: t.warning,
            // Item 29: the ASCII preview + dim hex byte run use the deeper C++
            // `textFaint` (≈ #505050), not the brighter `muted_foreground`, so the
            // hex/ASCII columns read as dim as C++. Build a faint tone by darkening
            // the muted foreground toward the paper.
            ascii: with_alpha(text_faint(t.muted_foreground), 0.9),
            comment_green: t.green_light,
            // Item 38: `IND_TYPE_HINT` is set to `theme.indHintGreen` (≈ #5a8248,
            // the muted comment-green family), NOT a dim gray. Tie the type-hint
            // role to the same green the comment annotations use so inference hints
            // read green.
            type_hint: t.green_light,
            rtti_hint: t.yellow,
            enum_chip: t.link,
            tree_conn: t.muted_foreground,
            // Item 30: selection band uses the near-neutral `theme.selected` fill
            // (the C++ `theme.selected` dark band), NOT a primary@0.20 accent wash.
            selection_bg: t.selection,
            // Item 30: the 2px left accent bar is the hover-span color (the C++
            // `indHoverSpan`), not the loud primary accent.
            accent: t.link,
            // Hover overlay: the theme `list_hover` is tuned for the dense UI
            // lists; lift it a touch so the per-row hover is visible over the
            // darker editor paper (the editor paper is `background.darker`).
            hover_bg: with_alpha(t.foreground, 0.05),
            caret: t.caret,
            paper,
            gutter_bg: paper,
            gutter_fg: with_alpha(t.muted_foreground, 0.70),
            // Item 31: footer pills are a flat translucent box (`indCmdPill`), no
            // border, dim pill fill. The 1px border + MD rounding are dropped at
            // the render site; the fill stays a faint translucent foreground wash.
            pill_bg: with_alpha(t.foreground, 0.06),
            // Item 40: the restrained C++ change-heat ramp (theme.cpp:69-74). The
            // defaults lerp in RGB space from `textDim` (≈ muted foreground):
            //   cold = lerp(textDim → gold  rgb(210,170,100) at 0.30)  (subtle)
            //   warm = lerp(textDim → orange rgb(235,145,50) at 0.60)
            //   hot  = markerPtr (the theme danger/pink ≈ t.red)
            // The prior Rust ramp lerped in HSL much hotter (cold at 0.6, warm =
            // full warning, hot = full red), reading noticeably brighter.
            heat_cold: lerp_rgb(t.muted_foreground, rgb_u8(210, 170, 100), 0.30),
            heat_warm: lerp_rgb(t.muted_foreground, rgb_u8(235, 145, 50), 0.60),
            heat_hot: t.red,
            byte_sel: t.link,
            border: t.border,
            // B1 / items 2-3: the fold disclosure triangle (`▸`/`▾`) is the TEXT
            // glyph compose bakes into the fold prefix, painted on the row baseline
            // (C++ has no SVG icon gutter). The C++ DIMS this arrow on every fold
            // head + the command-row chevron with the SAME `IND_HEX_DIM` treatment as
            // the hex byte run (editor.cpp:1522-1524, 2062-2064) — so it must read as
            // dim chrome, NOT a crisp accent-blue control. A crisp link-blue glyph
            // here is exactly the divergence the reference screenshot diff flags as
            // an "off-center/misaligned" chevron (the column placement matches C++
            // byte-for-byte; the EMPHASIS was wrong). Tie it to the same faint tone
            // the dim hex/ASCII glyphs use.
            fold_chevron: text_faint(t.muted_foreground),
            // Active-line band: a barely-there foreground wash (Zed's active-line
            // bg), softer than the accent selection fill so the two are distinct.
            active_line_bg: with_alpha(t.foreground, 0.03),
            // Item 71: the M_ERR red error band (translucent red wash) + bright red
            // hint text (the C++ `theme.markerError`). The wash is light enough to
            // keep the row text legible while flagging the invalid edit.
            error_bg: with_alpha(t.red, 0.22),
            error_fg: t.red,
            // Item 74: the presentation focus-glow base (the C++ `theme.focusGlow`);
            // tie it to the link/accent hue so the pulse reads as an AI/MCP marker.
            focus_glow: t.link,
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
            SpanRole::StringVal => self.string_val,
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

    /// An **opaque** fill for the inline-edit field band — the editor paper with
    /// the active-line wash composited on top, so it reads as a distinct editable
    /// surface while fully occluding the static row glyphs (the type token / the
    /// pre-edit name) beneath it. A translucent active-line bg alone would let the
    /// static text bleed through behind the seeded edit text, so the inline field
    /// composites it down to a solid color.
    pub fn active_line_fill(&self) -> Hsla {
        // `blend` composites `other` over `self`: the opaque paper stays opaque,
        // tinted by the translucent active-line wash.
        self.paper.blend(self.active_line_bg)
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

/// A deeper "text faint" tone (item 29): the C++ `textFaint` (≈ #505050) is a
/// good chunk dimmer than the muted foreground. Approximate it by pulling the
/// muted foreground's lightness down toward the floor while desaturating a hair,
/// so the hex byte run + ASCII preview read as faint as the C++ does. Clamped.
fn text_faint(muted: Hsla) -> Hsla {
    Hsla {
        s: (muted.s * 0.85).clamp(0.0, 1.0),
        l: (muted.l * 0.62).clamp(0.0, 1.0),
        ..muted
    }
}

/// An opaque `Hsla` from 8-bit RGB components (item 40 heat targets).
fn rgb_u8(r: u8, g: u8, b: u8) -> Hsla {
    gpui::Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Linear interpolation in **RGB** space (item 40): byte-faithful to the C++
/// `lerpRgb` (theme.cpp:63) used to derive the heat ramp from `textDim`. `t` is
/// clamped to `[0,1]`. Converts each endpoint to `Rgba`, lerps per channel, and
/// returns an opaque `Hsla`.
fn lerp_rgb(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_rgb();
    let b = b.to_rgb();
    gpui::Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
    .into()
}
