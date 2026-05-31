//! Theme application — map our [`crate::theme::Theme`] onto gpui-component's
//! `Theme` / `ThemeColor` so themes load and switch at runtime.
//!
//! Port of the C++ `MainWindow::applyGlobalTheme` (app-shell §4) + the
//! `ThemeManager::themeChanged` → re-style flow (themes.md §4.16). In Qt the
//! `Theme` was projected onto a `QPalette` (the role table in app-shell §4) and
//! pushed to every widget; in gpui-component the equivalent is the global
//! [`Theme`](gpui_component::Theme) whose `colors: ThemeColor` every widget
//! reads via `cx.theme()`. So "apply theme" = overwrite the global
//! `ThemeColor`'s fields from our resolved [`Theme`] and `window.refresh()`
//! (themes.md §4.16: store active theme + mark UI dirty).
//!
//! The C++→palette mapping (app-shell §4) is the binding contract; this module
//! reproduces it against gpui-component's (richer) `ThemeColor` field set.
//! The pure mapping — our [`Theme`] → a flat list of `(field, [`Rgb`])` pairs —
//! lives in [`resolve_palette`] and is unit-tested **headlessly** (no gpui).
//! The gpui write (`apply_theme`) is a thin feature-gated wrapper over it.
//!
//! Resolution note: our [`Theme`]'s color fields are `Option<Color>` (the
//! `QColor::isValid()` sentinel). After [`Theme::from_json`] the derived fields
//! (heat/focusGlow/markers) are always `Some`, but the *base* chrome fields can
//! still be `None` for a sparse user theme. [`resolve_palette`] substitutes a
//! conservative fallback for any `None` base color so the gpui theme is always
//! fully populated (gpui-component `ThemeColor` has no validity sentinel).

use crate::theme::color::Color;
use crate::theme::model::Theme;

/// A gpui-component `ThemeColor` field we drive from our theme.
///
/// We name the subset of `ThemeColor` fields that correspond to the Qt palette
/// roles the C++ chrome actually used (app-shell §4) plus the few extra
/// gpui-component roles needed so standard widgets (tabs, sidebar, list, popover,
/// scrollbar, title bar) render coherently. Each maps to a real
/// `gpui_component::ThemeColor` field of the same conceptual meaning; the
/// gpui write in [`apply_theme`] is the single place those field names appear,
/// so a gpui-component field rename is a one-spot fix.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum UiColor {
    // Core surfaces / text (Qt: Window/Base, WindowText/Text, …).
    Background,
    Foreground,
    Border,
    // Accent / links (Qt: Highlight / Link).
    Accent,
    AccentForeground,
    Selection,
    Link,
    LinkHover,
    Ring,
    // Muted / secondary text (Qt: Disabled.*, AlternateBase).
    MutedForeground,
    Secondary,
    SecondaryForeground,
    // Inputs / popovers / lists (chrome the editor docks/dialogs sit in).
    Input,
    Caret,
    Popover,
    PopoverForeground,
    List,
    ListActive,
    ListHover,
    // Tabs (the MDI document-tab strip — app-shell §8).
    Tab,
    TabActive,
    TabActiveForeground,
    TabForeground,
    TabBar,
    // Sidebar (the dockable workspace/scanner panels — app-shell §10).
    Sidebar,
    SidebarForeground,
    SidebarBorder,
    // Scrollbars (app-shell §4 global stylesheet).
    Scrollbar,
    ScrollbarThumb,
    ScrollbarThumbHover,
    // Title bar (the custom frameless titlebar — app-shell §5).
    TitleBar,
    TitleBarBorder,
    // Drag/drop affordances (the dock drag overlay — app-shell §9).
    DragBorder,
    DropTarget,
    // Status / severity (markers + heat → success/warning/danger roles).
    Danger,
    DangerForeground,
    Warning,
    WarningForeground,
    // Primary button / brand accent (the Zed-blue action color).
    Primary,
    PrimaryForeground,
    PrimaryHover,
    PrimaryActive,
    // Named accent palette (gpui-component's `blue`/`green`/… roles). The bespoke
    // editor surface reads these for SYNTAX coloring (see `ui/editor/palette.rs`),
    // so we drive them from our theme's syntax/marker fields rather than leaving
    // them at the gpui-component built-in defaults. Without this, theme switches
    // would not retint editor syntax.
    Blue,
    BlueLight,
    Green,
    GreenLight,
    Cyan,
    CyanLight,
    Yellow,
    YellowLight,
    Red,
    RedLight,
    Magenta,
    MagentaLight,
}

/// One resolved palette assignment: a gpui-component color field + the concrete
/// RGB to write into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaletteEntry {
    pub field: UiColor,
    pub color: Color,
}

/// Conservative fallbacks for the *base* chrome fields when a (sparse user)
/// theme leaves them `None`. The shipped 8 defaults populate all of these, so
/// these only bite for hand-rolled partial themes — but gpui-component's
/// `ThemeColor` has no "unset" state, so every field must resolve to a color.
///
/// Values are a neutral dark scheme (close to "Reclass Dark"), chosen so a
/// near-empty theme still renders legibly rather than all-black.
mod fallback {
    use super::Color;
    pub const BACKGROUND: Color = Color::rgb(0x1e, 0x1e, 0x1e);
    pub const FOREGROUND: Color = Color::rgb(0xd4, 0xd4, 0xd4);
    pub const BORDER: Color = Color::rgb(0x3c, 0x3c, 0x3c);
    pub const ACCENT: Color = Color::rgb(0x2a, 0x2d, 0x2e);
    pub const SELECTION: Color = Color::rgb(0x26, 0x4f, 0x78);
    pub const LINK: Color = Color::rgb(0x4f, 0xc3, 0xf7);
    pub const MUTED: Color = Color::rgb(0x85, 0x85, 0x85);
    pub const SURFACE: Color = Color::rgb(0x25, 0x25, 0x25);
    pub const BUTTON: Color = Color::rgb(0x33, 0x33, 0x33);
    pub const HOVER: Color = Color::rgb(0x2a, 0x2a, 0x2a);
    // Accent palette fallbacks (Zed One Dark-ish), for themes that omit the
    // corresponding syntax field. These only bite for sparse user themes; the
    // accent-blue and red roles always derive from always-present fields
    // (indHoverSpan / markerPtr), so they need no fallback const here.
    pub const GREEN: Color = Color::rgb(0x98, 0xc3, 0x79); // string/value green
    pub const CYAN: Color = Color::rgb(0x56, 0xb6, 0xc2); // preproc / class cyan
    pub const YELLOW: Color = Color::rgb(0xe5, 0xc0, 0x7b); // type / rtti yellow
    pub const MAGENTA: Color = Color::rgb(0xc6, 0x78, 0xdd); // keyword purple
}

/// Lighten a [`Color`] toward white by `f` (0..1) for the `*_light` accent roles.
/// `f = 0` keeps the color; `f = 1` is white.
fn lighten(c: Color, f: f32) -> Color {
    let mix = |ch: u8| -> u8 {
        (ch as f32 + (255.0 - ch as f32) * f)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgb(mix(c.r), mix(c.g), mix(c.b))
}

/// Resolve our [`Theme`] into the concrete gpui-component palette assignments —
/// the C++ `applyGlobalTheme` palette-role projection (app-shell §4), against
/// gpui-component's `ThemeColor`.
///
/// Pure + gpui-free: this is the testable core of theme application. Every
/// returned [`PaletteEntry`] names a `ThemeColor` field and the color to write;
/// [`apply_theme`] just performs the writes. Any base color the theme leaves
/// `None` is filled from [`fallback`] so the result is always fully populated.
///
/// The mapping follows the C++ palette table (app-shell §4):
/// `Window/Base→background`, `WindowText/Text→text`, `Highlight→selected`,
/// `Mid→hover`, `Dark→border`, `Link→indHoverSpan`, `AlternateBase→surface`,
/// `Disabled.*→textMuted`, `ToolTip*→backgroundAlt/text`, plus the markers →
/// danger/warning severity roles.
pub fn resolve_palette(theme: &Theme) -> Vec<PaletteEntry> {
    // Resolve base chrome colors once, substituting fallbacks for None.
    let background = theme.background.unwrap_or(fallback::BACKGROUND);
    let foreground = theme.text.unwrap_or(fallback::FOREGROUND);
    let border = theme.border.unwrap_or(fallback::BORDER);
    let surface = theme.surface.unwrap_or(fallback::SURFACE);
    let background_alt = theme.background_alt.unwrap_or(surface);
    let button = theme.button.unwrap_or(fallback::BUTTON);
    let hover = theme.hover.unwrap_or(fallback::HOVER);
    // Qt `Highlight` = `selected` (row selection). `selection` (text selection)
    // is the distinct field; keep them separate per themes.md §2.1 / app-shell §4.
    let selected = theme.selected.unwrap_or(fallback::ACCENT);
    let selection = theme.selection.unwrap_or(fallback::SELECTION);
    // Qt `Link` = `indHoverSpan` (app-shell §4).
    let link = theme.ind_hover_span.unwrap_or(fallback::LINK);
    let border_focused = theme.border_focused.unwrap_or(link);
    let muted = theme.text_muted.unwrap_or(fallback::MUTED);
    let text_dim = theme.text_dim.unwrap_or(muted);
    let text_faint = theme.text_faint.unwrap_or(border);

    // Markers → severity roles. After from_json these are always Some, but be
    // defensive (a directly-constructed Theme may leave them None).
    let marker_error = theme.marker_error.unwrap_or(Color::rgb(0x5a, 0x1d, 0x1d));
    let marker_ptr = theme.marker_ptr.unwrap_or(Color::rgb(0xf4, 0x47, 0x47));
    let marker_cycle = theme.marker_cycle.unwrap_or(Color::rgb(0xe8, 0xa3, 0x5c));

    // Accent palette → gpui-component's named accent roles, which the bespoke
    // editor surface reads for SYNTAX coloring. We bind each gpui role to the
    // theme's closest semantic syntax/marker field so a theme switch retints the
    // editor. `link` (= indHoverSpan) is the brand blue; primary/accent buttons
    // ride it too.
    let blue = link; // brand accent blue (indHoverSpan)
    let magenta = theme.syntax_keyword.unwrap_or(fallback::MAGENTA);
    let green = theme.syntax_string.unwrap_or(fallback::GREEN);
    let green_light = theme.syntax_comment.unwrap_or(fallback::GREEN);
    let cyan = theme.syntax_preproc.unwrap_or(fallback::CYAN);
    let yellow = theme.syntax_type.unwrap_or(fallback::YELLOW);
    let red = marker_ptr;
    // Primary action color = the focus/brand blue (Zed's action blue).
    let primary = border_focused;

    use UiColor::*;
    vec![
        // ── Core (Qt Window/Base, WindowText/Text, Dark) ──
        PaletteEntry {
            field: Background,
            color: background,
        },
        PaletteEntry {
            field: Foreground,
            color: foreground,
        },
        PaletteEntry {
            field: Border,
            color: border,
        },
        // ── Accent / selection / links (Qt Highlight / Link) ──
        PaletteEntry {
            field: Accent,
            color: selected,
        },
        PaletteEntry {
            field: AccentForeground,
            color: foreground,
        },
        PaletteEntry {
            field: Selection,
            color: selection,
        },
        PaletteEntry {
            field: Link,
            color: link,
        },
        PaletteEntry {
            field: LinkHover,
            color: link,
        },
        PaletteEntry {
            field: Ring,
            color: border_focused,
        },
        // ── Muted / secondary (Qt Disabled.*, AlternateBase, Button) ──
        PaletteEntry {
            field: MutedForeground,
            color: muted,
        },
        PaletteEntry {
            field: Secondary,
            color: button,
        },
        PaletteEntry {
            field: SecondaryForeground,
            color: foreground,
        },
        // ── Inputs / popovers / lists ──
        PaletteEntry {
            field: Input,
            color: border,
        },
        PaletteEntry {
            field: Caret,
            color: foreground,
        },
        PaletteEntry {
            field: Popover,
            color: background_alt,
        },
        PaletteEntry {
            field: PopoverForeground,
            color: foreground,
        },
        PaletteEntry {
            field: List,
            color: background,
        },
        PaletteEntry {
            field: ListActive,
            color: selected,
        },
        PaletteEntry {
            field: ListHover,
            color: hover,
        },
        // ── Tabs (MDI document-tab strip) ──
        PaletteEntry {
            field: Tab,
            color: background,
        },
        PaletteEntry {
            field: TabActive,
            color: background_alt,
        },
        PaletteEntry {
            field: TabActiveForeground,
            color: foreground,
        },
        PaletteEntry {
            field: TabForeground,
            color: text_dim,
        },
        PaletteEntry {
            field: TabBar,
            color: background,
        },
        // ── Sidebar (dockable panels) ──
        PaletteEntry {
            field: Sidebar,
            color: background,
        },
        PaletteEntry {
            field: SidebarForeground,
            color: foreground,
        },
        PaletteEntry {
            field: SidebarBorder,
            color: border,
        },
        // ── Scrollbars ──
        PaletteEntry {
            field: Scrollbar,
            color: background,
        },
        PaletteEntry {
            field: ScrollbarThumb,
            color: text_faint,
        },
        PaletteEntry {
            field: ScrollbarThumbHover,
            color: text_dim,
        },
        // ── Title bar (frameless custom chrome) ──
        PaletteEntry {
            field: TitleBar,
            color: background,
        },
        PaletteEntry {
            field: TitleBarBorder,
            color: border,
        },
        // ── Drag/drop (dock overlay) ──
        PaletteEntry {
            field: DragBorder,
            color: border_focused,
        },
        PaletteEntry {
            field: DropTarget,
            color: selection,
        },
        // ── Severity (markers + heat → danger/warning) ──
        PaletteEntry {
            field: Danger,
            color: marker_error,
        },
        PaletteEntry {
            field: DangerForeground,
            color: marker_ptr,
        },
        PaletteEntry {
            field: Warning,
            color: marker_cycle,
        },
        PaletteEntry {
            field: WarningForeground,
            color: foreground,
        },
        // ── Primary / brand accent (Zed action blue) ──
        PaletteEntry {
            field: Primary,
            color: primary,
        },
        PaletteEntry {
            field: PrimaryForeground,
            color: background,
        },
        PaletteEntry {
            field: PrimaryHover,
            color: lighten(primary, 0.12),
        },
        PaletteEntry {
            field: PrimaryActive,
            color: lighten(primary, 0.20),
        },
        // ── Named accent palette → editor syntax roles ──
        PaletteEntry {
            field: Blue,
            color: blue,
        },
        PaletteEntry {
            field: BlueLight,
            color: lighten(blue, 0.18),
        },
        PaletteEntry {
            field: Green,
            color: green,
        },
        PaletteEntry {
            field: GreenLight,
            color: green_light,
        },
        PaletteEntry {
            field: Cyan,
            color: cyan,
        },
        PaletteEntry {
            field: CyanLight,
            color: lighten(cyan, 0.18),
        },
        PaletteEntry {
            field: Yellow,
            color: yellow,
        },
        PaletteEntry {
            field: YellowLight,
            color: lighten(yellow, 0.18),
        },
        PaletteEntry {
            field: Red,
            color: red,
        },
        PaletteEntry {
            field: RedLight,
            color: lighten(red, 0.18),
        },
        PaletteEntry {
            field: Magenta,
            color: magenta,
        },
        PaletteEntry {
            field: MagentaLight,
            color: lighten(magenta, 0.18),
        },
    ]
}

/// Whether our theme should drive gpui-component into dark or light mode.
///
/// gpui-component's `Theme` keeps a `ThemeMode` (light/dark) that gates a few
/// derived behaviors (e.g. `input_background`). Reclass themes are RGB bundles
/// with no explicit mode flag, so we infer it from background luminance — the
/// same `0.2126R + 0.7152G + 0.0722B` luminance the macOS titlebar code uses to
/// pick Aqua vs DarkAqua (app-shell §5). Background luminance < 0.5 ⇒ dark.
pub fn is_dark(theme: &Theme) -> bool {
    let bg = theme.background.unwrap_or(fallback::BACKGROUND);
    relative_luminance(bg) < 0.5
}

/// `0.2126R + 0.7152G + 0.0722B` on 0..1 channels (app-shell §5).
fn relative_luminance(c: Color) -> f32 {
    0.2126 * (c.r as f32 / 255.0) + 0.7152 * (c.g as f32 / 255.0) + 0.0722 * (c.b as f32 / 255.0)
}

// ── gpui write (feature-gated) ─────────────────────────────────────────────

/// Convert our [`Color`] to gpui's `Hsla` (via its `Rgba` → `Hsla` path).
///
/// gpui `Rgba` channels are f32 in `0.0..=1.0`; `Hsla: From<Rgba>` does the
/// HSL conversion. This is the single bridge between our RGB model and gpui's
/// color type.
#[cfg(feature = "ui")]
pub fn to_hsla(c: Color) -> gpui::Hsla {
    gpui::Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Write one resolved [`PaletteEntry`] into a gpui-component [`ThemeColor`].
///
/// The single place gpui-component `ThemeColor` field names appear — a rename
/// upstream is fixed here only.
#[cfg(feature = "ui")]
fn apply_entry(colors: &mut gpui_component::ThemeColor, entry: &PaletteEntry) {
    let c = to_hsla(entry.color);
    use UiColor::*;
    match entry.field {
        Background => colors.background = c,
        Foreground => colors.foreground = c,
        Border => colors.border = c,
        Accent => colors.accent = c,
        AccentForeground => colors.accent_foreground = c,
        Selection => colors.selection = c,
        Link => colors.link = c,
        LinkHover => colors.link_hover = c,
        Ring => colors.ring = c,
        MutedForeground => colors.muted_foreground = c,
        Secondary => colors.secondary = c,
        SecondaryForeground => colors.secondary_foreground = c,
        Input => colors.input = c,
        Caret => colors.caret = c,
        Popover => colors.popover = c,
        PopoverForeground => colors.popover_foreground = c,
        List => colors.list = c,
        ListActive => colors.list_active = c,
        ListHover => colors.list_hover = c,
        Tab => colors.tab = c,
        TabActive => colors.tab_active = c,
        TabActiveForeground => colors.tab_active_foreground = c,
        TabForeground => colors.tab_foreground = c,
        TabBar => colors.tab_bar = c,
        Sidebar => colors.sidebar = c,
        SidebarForeground => colors.sidebar_foreground = c,
        SidebarBorder => colors.sidebar_border = c,
        Scrollbar => colors.scrollbar = c,
        ScrollbarThumb => colors.scrollbar_thumb = c,
        ScrollbarThumbHover => colors.scrollbar_thumb_hover = c,
        TitleBar => colors.title_bar = c,
        TitleBarBorder => colors.title_bar_border = c,
        DragBorder => colors.drag_border = c,
        DropTarget => colors.drop_target = c,
        Danger => colors.danger = c,
        DangerForeground => colors.danger_foreground = c,
        Warning => colors.warning = c,
        WarningForeground => colors.warning_foreground = c,
        Primary => colors.primary = c,
        PrimaryForeground => colors.primary_foreground = c,
        PrimaryHover => colors.primary_hover = c,
        PrimaryActive => colors.primary_active = c,
        Blue => colors.blue = c,
        BlueLight => colors.blue_light = c,
        Green => colors.green = c,
        GreenLight => colors.green_light = c,
        Cyan => colors.cyan = c,
        CyanLight => colors.cyan_light = c,
        Yellow => colors.yellow = c,
        YellowLight => colors.yellow_light = c,
        Red => colors.red = c,
        RedLight => colors.red_light = c,
        Magenta => colors.magenta = c,
        MagentaLight => colors.magenta_light = c,
    }
}

/// The app-shared [`ThemeManager`](crate::theme::ThemeManager) handle, held in a
/// gpui global so the window and `main.rs` share one manager (the C++
/// `ThemeManager` singleton; themes.md §2.3).
///
/// gpui's `Global` is `'static`-only (no `Send`/`Sync`) and the app runs on one
/// thread, so an `Rc<RefCell<…>>` is the right interior-mutability handle —
/// matching the single-threaded, main-thread-owned model the C++ subsystem uses
/// (themes.md §8). [`get`](ThemeRegistryGlobal::get) lazily builds the manager
/// from the on-disk `themes/` dir (self-healing from the embedded defaults).
#[cfg(feature = "ui")]
pub struct ThemeRegistryGlobal(pub std::rc::Rc<std::cell::RefCell<crate::theme::ThemeManager>>);

#[cfg(feature = "ui")]
impl gpui::Global for ThemeRegistryGlobal {}

#[cfg(feature = "ui")]
impl ThemeRegistryGlobal {
    /// Get the shared [`ThemeManager`](crate::theme::ThemeManager) handle,
    /// constructing + installing it on first use.
    ///
    /// The manager is built over the default builtin dir (`<exe>/themes`,
    /// self-healing from embedded defaults if absent) and the per-user theme dir
    /// (themes.md §4.11/§4.12). The `"theme"` selection is read from a fresh
    /// [`MemSettings`](crate::theme::MemSettings) for now; wiring it to the app's
    /// persistent settings store is a later step.
    pub fn get(cx: &mut gpui::App) -> std::rc::Rc<std::cell::RefCell<crate::theme::ThemeManager>> {
        if !cx.has_global::<ThemeRegistryGlobal>() {
            // Seed the launch selection to "Zed One Dark" (the port's native
            // Zed One Dark theme) so the app opens on it. The manager's generic
            // fallback (first "VS2022" built-in) is unchanged — this only sets
            // the persisted `"theme"` key the constructor reads. A real
            // persistent settings store will replace `MemSettings` later; until
            // then this is the app's default theme.
            let mut settings = crate::theme::MemSettings::new();
            crate::theme::SettingsStore::set(
                &mut settings,
                "theme",
                super::design::DEFAULT_THEME_NAME,
            );
            let manager = crate::theme::ThemeManager::new(
                Box::new(settings),
                crate::theme::ThemeManager::default_builtin_dir(),
                crate::theme::ThemeManager::default_user_dir(),
            );
            cx.set_global(ThemeRegistryGlobal(std::rc::Rc::new(
                std::cell::RefCell::new(manager),
            )));
        }
        cx.global::<ThemeRegistryGlobal>().0.clone()
    }

    /// The current resolved [`Theme`](crate::theme::Theme), read-only.
    ///
    /// For views that tint from our theme's *semantic* colors (e.g. the type
    /// selector's per-group accents, which gpui-component's `ThemeColor` does not
    /// model) during `render` (a `&App` context). Returns a clone of the active
    /// theme, or [`Theme::default`](crate::theme::Theme::default) if the global is
    /// not yet installed (it always is after `gpui_component::init`).
    pub fn current(cx: &gpui::App) -> crate::theme::Theme {
        if cx.has_global::<ThemeRegistryGlobal>() {
            cx.global::<ThemeRegistryGlobal>()
                .0
                .borrow()
                .current()
                .clone()
        } else {
            crate::theme::Theme::default()
        }
    }
}

/// Apply our [`Theme`] to the global gpui-component theme and refresh.
///
/// The runtime theme-switch entry point (themes.md §4.16): resolve the palette,
/// write every field into `Theme::global_mut(cx).colors`, set the inferred
/// light/dark mode, then `window.refresh()` so every `cx.theme()`-reading widget
/// re-renders. Call this from the `themeChanged` observer wired in the window.
///
/// Requires the gpui-component global to exist (it does after
/// `gpui_component::init(cx)`); `gpui_component::Theme` is a `Global`.
#[cfg(feature = "ui")]
pub fn apply_theme(theme: &Theme, window: &mut gpui::Window, cx: &mut gpui::App) {
    let palette = resolve_palette(theme);
    let dark = is_dark(theme);

    let gtheme = gpui_component::Theme::global_mut(cx);
    gtheme.mode = if dark {
        gpui_component::ThemeMode::Dark
    } else {
        gpui_component::ThemeMode::Light
    };
    for entry in &palette {
        apply_entry(&mut gtheme.colors, entry);
    }

    // Zed-like global typography + surface geometry (design tokens). Comfortable
    // ~14px UI text, a real monospace for the editor, small radii on elevated
    // surfaces, restrained shadows. See `ui/design.rs` for the token source.
    use super::design::tokens;
    gtheme.font_family = tokens::font::UI_FAMILY.into();
    gtheme.font_size = gpui::px(tokens::font::UI_MD);
    gtheme.mono_font_family = tokens::font::MONO_FAMILY.into();
    gtheme.mono_font_size = gpui::px(tokens::font::EDITOR_SIZE);
    gtheme.radius = gpui::px(tokens::radius::MD);
    gtheme.radius_lg = gpui::px(tokens::radius::LG);
    gtheme.shadow = true;

    window.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::Color;

    /// Find the resolved color for a field in a palette (test helper).
    fn lookup(palette: &[PaletteEntry], field: UiColor) -> Option<Color> {
        palette.iter().find(|e| e.field == field).map(|e| e.color)
    }

    fn vs_dark_like() -> Theme {
        // A realistic populated theme (VS2022-ish values).
        let v = serde_json::json!({
            "name": "VS2022 Dark",
            "background": "#1e1e1e",
            "text": "#d4d4d4",
            "border": "#3c3c3c",
            "surface": "#252526",
            "backgroundAlt": "#2d2d30",
            "button": "#333333",
            "hover": "#2a2d2e",
            "selected": "#094771",
            "selection": "#264f78",
            "textMuted": "#808080",
            "textDim": "#969696",
            "textFaint": "#5a5a5a",
            "borderFocused": "#007acc",
            "indHoverSpan": "#4fc3f7",
            "markerPtr": "#f44747",
            "markerError": "#5a1d1d",
            "markerCycle": "#e8a35c"
        });
        Theme::from_json(&v)
    }

    #[test]
    fn palette_maps_core_roles_from_cpp_table() {
        // app-shell §4: Window/Base→background, WindowText/Text→text,
        // Dark→border, Highlight→selected, Link→indHoverSpan, Selection text bg.
        let t = vs_dark_like();
        let p = resolve_palette(&t);

        assert_eq!(lookup(&p, UiColor::Background), Color::parse("#1e1e1e"));
        assert_eq!(lookup(&p, UiColor::Foreground), Color::parse("#d4d4d4"));
        assert_eq!(lookup(&p, UiColor::Border), Color::parse("#3c3c3c"));
        // Highlight (row selection) = selected, NOT selection.
        assert_eq!(lookup(&p, UiColor::Accent), Color::parse("#094771"));
        // Text selection background = selection.
        assert_eq!(lookup(&p, UiColor::Selection), Color::parse("#264f78"));
        // Link = indHoverSpan.
        assert_eq!(lookup(&p, UiColor::Link), Color::parse("#4fc3f7"));
        // Disabled.* = textMuted.
        assert_eq!(
            lookup(&p, UiColor::MutedForeground),
            Color::parse("#808080")
        );
        // Ring (focus) = borderFocused.
        assert_eq!(lookup(&p, UiColor::Ring), Color::parse("#007acc"));
    }

    #[test]
    fn selected_and_selection_stay_distinct() {
        // themes.md §2.1: `selected` (row highlight) != `selection` (text bg).
        let t = vs_dark_like();
        let p = resolve_palette(&t);
        let accent = lookup(&p, UiColor::Accent).unwrap();
        let selection = lookup(&p, UiColor::Selection).unwrap();
        assert_ne!(accent, selection);
        assert_eq!(accent, Color::rgb(0x09, 0x47, 0x71));
        assert_eq!(selection, Color::rgb(0x26, 0x4f, 0x78));
    }

    #[test]
    fn markers_map_to_severity_roles() {
        let t = vs_dark_like();
        let p = resolve_palette(&t);
        assert_eq!(lookup(&p, UiColor::Danger), Color::parse("#5a1d1d")); // markerError
        assert_eq!(
            lookup(&p, UiColor::DangerForeground),
            Color::parse("#f44747")
        ); // markerPtr
        assert_eq!(lookup(&p, UiColor::Warning), Color::parse("#e8a35c")); // markerCycle
    }

    #[test]
    fn palette_is_fully_populated_even_for_sparse_theme() {
        // A near-empty theme: only name + background. Every entry must still
        // resolve (gpui-component ThemeColor has no validity sentinel).
        let t = Theme::from_json(&serde_json::json!({ "name": "Sparse", "background": "#101010" }));
        let p = resolve_palette(&t);

        // Background uses the theme's value; others use fallbacks.
        assert_eq!(lookup(&p, UiColor::Background), Color::parse("#101010"));
        // No None colors leak through — every entry has a concrete color, and
        // all the named fields are present exactly once.
        let mut seen = std::collections::HashSet::new();
        for e in &p {
            assert!(seen.insert(e.field), "field {:?} resolved twice", e.field);
        }
        // Spot-check a few that have no theme value → fallback.
        assert_eq!(lookup(&p, UiColor::Foreground), Some(fallback::FOREGROUND));
        assert_eq!(lookup(&p, UiColor::Border), Some(fallback::BORDER));
        assert!(lookup(&p, UiColor::Link).is_some());
    }

    #[test]
    fn dark_light_inferred_from_background_luminance() {
        // Dark background → dark mode.
        let dark = Theme::from_json(&serde_json::json!({ "name": "D", "background": "#1e1e1e" }));
        assert!(is_dark(&dark));
        // Light background ("Light"/tw theme) → light mode.
        let light = Theme::from_json(&serde_json::json!({ "name": "L", "background": "#f3f3f3" }));
        assert!(!is_dark(&light));
    }

    #[test]
    fn shipped_defaults_all_resolve() {
        // Every shipped default must produce a complete, concrete palette.
        for (_name, json) in crate::theme::DEFAULT_THEMES.iter() {
            let v: serde_json::Value = serde_json::from_str(json).unwrap();
            let t = Theme::from_json(&v);
            let p = resolve_palette(&t);
            // Sanity: a stable count of entries, no duplicates.
            let mut seen = std::collections::HashSet::new();
            for e in &p {
                assert!(seen.insert(e.field), "{}: dup field {:?}", t.name, e.field);
            }
            // Core fields always present.
            assert!(lookup(&p, UiColor::Background).is_some(), "{}", t.name);
            assert!(lookup(&p, UiColor::Foreground).is_some(), "{}", t.name);
            // The named accent palette (editor SYNTAX roles) always resolves too.
            for f in [
                UiColor::Primary,
                UiColor::Blue,
                UiColor::Green,
                UiColor::Cyan,
                UiColor::Yellow,
                UiColor::Red,
                UiColor::Magenta,
            ] {
                assert!(lookup(&p, f).is_some(), "{}: {:?} unresolved", t.name, f);
            }
        }
    }

    #[test]
    fn accent_palette_binds_to_syntax_fields() {
        // The named accent roles the editor reads for syntax must follow the
        // theme's syntax/marker/accent fields (a theme switch retints the editor).
        // Use a theme that actually ships the syntax fields.
        let t = Theme::from_json(&serde_json::json!({
            "name": "Syntax",
            "syntaxKeyword": "#c678dd",
            "syntaxString": "#98c379",
            "syntaxPreproc": "#56b6c2",
            "syntaxType": "#e5c07b",
            "markerPtr": "#e06c75",
            "indHoverSpan": "#61afef",
            "borderFocused": "#61afef"
        }));
        let p = resolve_palette(&t);
        // Magenta = syntaxKeyword, Green = syntaxString, Cyan = syntaxPreproc,
        // Yellow = syntaxType, Red = markerPtr, Blue = indHoverSpan, Primary = borderFocused.
        assert_eq!(lookup(&p, UiColor::Magenta), t.syntax_keyword);
        assert_eq!(lookup(&p, UiColor::Green), t.syntax_string);
        assert_eq!(lookup(&p, UiColor::Cyan), t.syntax_preproc);
        assert_eq!(lookup(&p, UiColor::Yellow), t.syntax_type);
        assert_eq!(lookup(&p, UiColor::Red), t.marker_ptr);
        assert_eq!(lookup(&p, UiColor::Blue), t.ind_hover_span);
        assert_eq!(lookup(&p, UiColor::Primary), t.border_focused);
    }

    #[test]
    fn zed_one_dark_maps_to_one_dark_colors() {
        // The bundled Zed One Dark theme resolves onto the expected One Dark hues.
        let json = crate::theme::DEFAULT_THEMES
            .iter()
            .find(|(name, _)| *name == "zed_one_dark.json")
            .map(|(_, j)| *j)
            .expect("zed_one_dark default present");
        let t = Theme::from_json(&serde_json::from_str(json).unwrap());
        assert_eq!(t.name, "Zed One Dark");
        let p = resolve_palette(&t);
        // Core One Dark chrome.
        assert_eq!(lookup(&p, UiColor::Background), Color::parse("#282c34"));
        assert_eq!(lookup(&p, UiColor::Foreground), Color::parse("#c8ccd4"));
        assert_eq!(lookup(&p, UiColor::Border), Color::parse("#3b414d"));
        assert_eq!(
            lookup(&p, UiColor::MutedForeground),
            Color::parse("#828997")
        );
        // Accent blue + syntax purple/green/yellow.
        assert_eq!(lookup(&p, UiColor::Primary), Color::parse("#61afef"));
        assert_eq!(lookup(&p, UiColor::Magenta), Color::parse("#c678dd"));
        assert_eq!(lookup(&p, UiColor::Green), Color::parse("#98c379"));
        assert_eq!(lookup(&p, UiColor::Yellow), Color::parse("#e5c07b"));
        // Dark mode inferred from the dark background.
        assert!(is_dark(&t));
    }
}
