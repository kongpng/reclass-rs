//! Custom window titlebar — the frameless chrome (app-shell §5 `TitleBarWidget`).
//!
//! Port of `src/titlebar.{h,cpp}`. The C++ `TitleBarWidget` is a 32px frameless
//! bar: app label, the menu bar (or Linux tool-button-mirrored menus), a stretch,
//! the **workspace layout-toggle pair**, then the min/max/close window controls.
//! gpui-component's [`TitleBar`](gpui_component::TitleBar) already provides the
//! frameless bar + native min/max/close controls (cookbook §Chrome), so this
//! module contributes:
//!
//! - [`LayoutPreset`] — the two-mode workspace toggle enum (`titlebar.h:15-18`),
//! - [`title_case`] / [`upper_case`] — the menu-title transform
//!   (`setMenuBarTitleCase`, `titlebar.cpp:222-253`), unit-tested headlessly,
//! - [`render_titlebar`] — assembles the bar contents (app label, the in-window
//!   menu bar, stretch, the document title, and the workspace **sidebar toggle**)
//!   into a [`TitleBar`].
//!
//! The **view-mode** switch (Reclass ⇄ Code) is deliberately NOT here: it lives
//! as a "Reclass | Code" *segmented control* at the bottom of the document area
//! (`tabs::DocumentArea::render_view_toggle`; PIC5). A duplicate titlebar copy
//! was inconsistent dead UI and has been removed.
//!
//! The sidebar toggle emits its intent by calling back into the owning
//! [`MainWindow`](super::window::MainWindow) (the C++ `layoutPresetSelected`
//! signal → `applyLayoutPreset`); rendering takes a plain closure so this module
//! stays decoupled from the window type.
//!
//! Gated behind the `ui` feature.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme, Icon, IconName, Selectable as _, Sizable as _, TitleBar};

use super::menubar::MenuBar;

/// The two-mode workspace layout toggle (`enum LayoutPreset`, `titlebar.h:15-18`).
///
/// The titlebar shows an exclusive pair of checkable buttons; exactly one is
/// active. `Off` hides the workspace dock, `Workspace` shows it. `Off` is the
/// C++ default-checked state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LayoutPreset {
    /// `Layout_NoWorkspace = 0` — workspace dock hidden (default).
    #[default]
    Off,
    /// `Layout_Workspace = 1` — workspace dock visible.
    Workspace,
}

impl LayoutPreset {
    /// The C++ integer id (`idClicked`/`layoutPresetSelected` value).
    pub fn id(self) -> i32 {
        match self {
            LayoutPreset::Off => 0,
            LayoutPreset::Workspace => 1,
        }
    }

    /// Build from the integer id (the `applyLayoutPreset(int)` argument);
    /// anything other than `1` is `Off` (matches the C++ `preset==Layout_Workspace`).
    pub fn from_id(id: i32) -> Self {
        if id == 1 {
            LayoutPreset::Workspace
        } else {
            LayoutPreset::Off
        }
    }

    /// Whether this preset shows the workspace dock.
    pub fn workspace_visible(self) -> bool {
        matches!(self, LayoutPreset::Workspace)
    }

    /// The preset for a given workspace-visibility (`setWorkspaceChecked`).
    pub fn for_visible(visible: bool) -> Self {
        if visible {
            LayoutPreset::Workspace
        } else {
            LayoutPreset::Off
        }
    }
}

/// Upper-case a top-level menu title (`setMenuBarTitleCase(true)` branch,
/// `titlebar.cpp:222-253`). Qt prefixes mnemonics with `&`; the transform
/// upper-cases the visible text. Our titles carry no `&`, so this is a plain
/// ASCII upper-case.
pub fn upper_case(title: &str) -> String {
    title.to_uppercase()
}

/// Title-case a top-level menu title (`setMenuBarTitleCase(false)` branch):
/// capitalize the first letter of each whitespace-separated word, lower-casing
/// the rest (the C++ "capitalize first letter of each word").
pub fn title_case(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for (i, word) in title.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            for c in chars {
                out.extend(c.to_lowercase());
            }
        }
    }
    out
}

/// One small chrome glyph button used in the titlebar (the workspace/sidebar
/// toggle). The C++ painted these as custom flat checkable buttons with a 2px
/// bottom accent when checked; here a ghost [`Button`] with a `selected`/accent
/// state reproduces the look. The click closure carries the toggle's intent.
/// Kept as the canonical chrome-toggle recipe (the inline sidebar toggle mirrors
/// it); retained for the icon-button workflow.
#[allow(dead_code)]
fn chrome_toggle(
    id: &'static str,
    glyph: &'static str,
    checked: bool,
    tooltip: &'static str,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .selected(checked)
        .label(glyph)
        .tooltip(tooltip)
        .on_click(move |_e, w, cx| on_click(w, cx))
}

/// Assemble the titlebar contents into a [`TitleBar`] (app-shell §5 layout:
/// app label · menu bar · stretch · document title · sidebar toggle). The window
/// controls (min/max/close) are supplied by the gpui-component [`TitleBar`]
/// itself.
///
/// The **view-mode** switch is NOT in the titlebar: PIC5 shows it as a
/// "Reclass | Code" *segmented control* pinned to the bottom of the document
/// area (rendered by `tabs::DocumentArea::render_view_toggle`). Having a second
/// titlebar copy was inconsistent dead UI, so this bar carries only the sidebar
/// (workspace) toggle.
///
/// Callback (a plain `Fn` so the titlebar stays decoupled from `MainWindow`):
/// - `on_layout` — the workspace-toggle button was clicked (the C++
///   `layoutPresetSelected`); receives the chosen [`LayoutPreset`].
///
/// `preset` is the current workspace state (drives the toggle's checked styling);
/// `doc_title` is the active document's display title (the right-aligned label);
/// `menubar` is the in-window menu-bar entity (rendered as a child so its
/// dropdowns open from the bar).
pub fn render_titlebar(
    preset: LayoutPreset,
    doc_title: impl Into<SharedString>,
    has_doc: bool,
    menubar: Entity<MenuBar>,
    on_layout: impl Fn(LayoutPreset, &mut Window, &mut App) + 'static,
    cx: &App,
) -> TitleBar {
    let on_layout = std::rc::Rc::new(on_layout);

    // App label (the C++ bold "Reclass" `m_appLabel`).
    let app_label = div()
        .flex_none()
        .px_2()
        .font_weight(FontWeight::BOLD)
        .text_color(cx.theme().foreground)
        .child("Reclass");

    // Workspace (sidebar) toggle — a single clean ghost icon button (Zed's panel
    // toggle), replacing the crude exclusive glyph pair. Selected = sidebar shown.
    let sidebar_open = preset == LayoutPreset::Workspace;
    let sidebar_btn = {
        let cb = on_layout.clone();
        let next = if sidebar_open {
            LayoutPreset::Off
        } else {
            LayoutPreset::Workspace
        };
        let icon = if sidebar_open {
            IconName::PanelLeftClose
        } else {
            IconName::PanelLeftOpen
        };
        Button::new("toggle-sidebar")
            .ghost()
            .small()
            .selected(sidebar_open)
            .child(Icon::new(icon))
            .tooltip(if sidebar_open {
                "Hide workspace"
            } else {
                "Show workspace"
            })
            .on_click(move |_e, w, cx| cb(next, w, cx))
    };

    let title: SharedString = doc_title.into();

    TitleBar::new()
        // Left cluster: the app label + the in-window menu bar (PIC1/PIC5:
        // "Reclass  File  Edit  View  Tools  Plugins  Help").
        .child(
            gpui_component::h_flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(app_label)
                .child(menubar),
        )
        // Stretch pushes the right-side controls to the far edge.
        .child(div().flex_1())
        .child(
            gpui_component::h_flex()
                .flex_none()
                .gap_2()
                .items_center()
                .when(has_doc && !title.is_empty(), |row| {
                    row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(title.clone()),
                    )
                })
                .child(sidebar_btn),
        )
}

/// A small inline source-icon badge used by the document tabs + titlebar
/// (`drawTabSourceIcon`, app-shell §8). Renders a glyph at full/dimmed opacity to
/// signal liveness; the real SVG icon assets are wired with the icon workflow.
pub fn source_icon(kind: super::state::SourceKind, live: bool, cx: &App) -> impl IntoElement {
    let glyph = match kind {
        super::state::SourceKind::None => "\u{25CB}", // ○ no source (plug)
        super::state::SourceKind::File => "\u{1F5CE}", // 🗎 file
        super::state::SourceKind::Buffer => "\u{25A4}", // ▤ buffer
        super::state::SourceKind::Snapshot => "\u{25A3}", // ▣ snapshot
        super::state::SourceKind::Process => "\u{2699}", // ⚙ process
    };
    let color = if live {
        cx.theme().foreground
    } else {
        // Disconnected sources dim (the C++ ×0.40 live opacity).
        let mut c = cx.theme().muted_foreground;
        c.a = 0.55;
        c
    };
    div().flex_none().text_xs().text_color(color).child(glyph)
}

/// Build an [`Icon`] from a gpui-component [`IconName`] (kept for the icon
/// workflow; ensures the `Icon`/`IconName` imports are exercised).
#[allow(dead_code)]
pub(crate) fn named_icon(name: IconName) -> Icon {
    Icon::new(name)
}

#[cfg(test)]
mod tests {
    // Import only the gpui-free items under test — NOT `super::*`, which would
    // pull the module's `gpui::*` glob into the `#[test]` hygiene expansion and
    // overflow the type-recursion budget (see lib.rs note).
    use super::{title_case, upper_case, LayoutPreset};

    #[test]
    fn layout_preset_roundtrips_through_id() {
        assert_eq!(LayoutPreset::Off.id(), 0);
        assert_eq!(LayoutPreset::Workspace.id(), 1);
        assert_eq!(LayoutPreset::from_id(0), LayoutPreset::Off);
        assert_eq!(LayoutPreset::from_id(1), LayoutPreset::Workspace);
        // Any non-1 id is Off (matches `preset == Layout_Workspace`).
        assert_eq!(LayoutPreset::from_id(7), LayoutPreset::Off);
        assert_eq!(LayoutPreset::from_id(-1), LayoutPreset::Off);
    }

    #[test]
    fn layout_preset_default_is_off() {
        // Off is the C++ default-checked toggle.
        assert_eq!(LayoutPreset::default(), LayoutPreset::Off);
        assert!(!LayoutPreset::default().workspace_visible());
        assert!(LayoutPreset::Workspace.workspace_visible());
    }

    #[test]
    fn for_visible_maps_visibility() {
        assert_eq!(LayoutPreset::for_visible(true), LayoutPreset::Workspace);
        assert_eq!(LayoutPreset::for_visible(false), LayoutPreset::Off);
    }

    #[test]
    fn upper_case_matches_qt_uppercase() {
        assert_eq!(upper_case("File"), "FILE");
        assert_eq!(upper_case("View"), "VIEW");
        assert_eq!(upper_case("plugins"), "PLUGINS");
    }

    #[test]
    fn title_case_capitalizes_each_word() {
        // The C++ "Title Case": first letter of each word up, rest down.
        assert_eq!(title_case("file"), "File");
        assert_eq!(title_case("VIEW"), "View");
        assert_eq!(title_case("data source"), "Data Source");
        assert_eq!(title_case("reCLASS browser"), "Reclass Browser");
        // Idempotent on already-title-cased input.
        assert_eq!(title_case("Help"), "Help");
    }
}
