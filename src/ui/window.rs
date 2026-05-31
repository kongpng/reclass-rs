//! The main window shell — the top-level gpui view every UI piece plugs into.
//!
//! Port of the C++ `MainWindow` chrome (app-shell §5 titlebar, §6 window, §8
//! document tabs, §10 docks). Composes:
//!
//! - a gpui-component [`TitleBar`] (the custom frameless titlebar; app-shell §5),
//! - a [`DockArea`] holding the MDI document-tab center + workspace/scanner
//!   docks (built by [`super::docks::build_default_layout`]; app-shell §8/§10),
//! - the [`Root`] overlay layers (modals/dialogs/sheets/notifications), and
//! - the gpui-free [`AppState`] window-state (open docs, active doc, source,
//!   selection, theme handle).
//!
//! Theme is owned by a [`ThemeManager`] global; on construction the window
//! resolves the current theme and applies it via [`super::theme_apply`], and an
//! observer re-applies on `themeChanged` (themes.md §4.16). The structure mirrors
//! the verified gpui-component `StoryWorkspace` pattern (`examples/dock.rs`):
//! `TitleBar` + `DockArea` in a `flex_col`, wrapped at the window level in
//! `Root::new`.
//!
//! **SKELETON** — the editor grid, real docks, dialogs, menus, status bar, and
//! start page are filled in by later workflows. This establishes the seams.
//!
//! Gated behind the `ui` feature.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::*;
use gpui_component::{dock::DockArea, ActiveTheme, Root, TitleBar};

use super::docks::{self, MAIN_DOCK_AREA};
use super::state::AppState;
use super::theme_apply::ThemeRegistryGlobal;
use crate::theme::ThemeManager;

/// The application's root view — the C++ `MainWindow` (app-shell §6).
///
/// Holds the gpui-free [`AppState`], the [`DockArea`] entity (the docking
/// workspace), and the shared [`ThemeManager`] handle. Rendered inside a
/// [`Root`] (set up in [`open_main_window`]) so overlays/modals/tooltips work.
pub struct MainWindow {
    /// Window-level application state (open docs, active doc, source, selection,
    /// theme handle). gpui-free + unit-tested (see [`super::state`]).
    state: AppState,
    /// The docking workspace (center document tabs + side docks).
    dock_area: Entity<DockArea>,
    /// Shared theme manager (the C++ `ThemeManager` singleton; owned by the app).
    theme_manager: Rc<RefCell<ThemeManager>>,
}

impl MainWindow {
    /// Construct the main window view: build the [`DockArea`], assemble the
    /// default dock layout, seed [`AppState`] with the initial document, and
    /// apply the current theme (themes.md §4.16 initial `applyTheme(current())`).
    pub fn new(
        theme_manager: Rc<RefCell<ThemeManager>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Build the docking workspace and its default layout (app-shell §7/§10).
        let dock_area =
            cx.new(|cx| DockArea::new(MAIN_DOCK_AREA.id, Some(MAIN_DOCK_AREA.version), window, cx));
        docks::build_default_layout(&dock_area, window, cx);

        // Seed window state with the initial document tab (the C++ "never leave a
        // blank window" — a doc is always present; app-shell §8 step 9). The
        // placeholder document panel in the center stands in for its editor.
        let mut state = AppState::new();
        state.open_document("Untitled");

        // Apply the current theme to gpui-component (themes.md §4.16). Record the
        // active theme name in the state (the theme handle).
        {
            let tm = theme_manager.borrow();
            let current = tm.current().clone();
            state.set_theme_name(&current.name);
            super::theme_apply::apply_theme(&current, window, cx);
        }

        MainWindow {
            state,
            dock_area,
            theme_manager,
        }
    }

    /// Read-only access to the window state (for tests / wiring).
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Mutable access to the window state.
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// The docking workspace entity.
    pub fn dock_area(&self) -> &Entity<DockArea> {
        &self.dock_area
    }

    /// Switch the active theme by index and re-apply it (themes.md §4.4
    /// `setCurrent` → §4.16 `themeChanged` → re-style). Updates the state's
    /// theme handle. No-op if the index is out of range.
    pub fn switch_theme(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        let applied = {
            let mut tm = self.theme_manager.borrow_mut();
            tm.set_current(index);
            // set_current is a no-op when out of range; current() reflects it.
            tm.current().clone()
        };
        self.state.set_theme_name(&applied.name);
        super::theme_apply::apply_theme(&applied, window, cx);
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Root overlay layers (modals/dialogs/sheets/notifications) — mounted at
        // the top of the view tree so dialogs/popovers/toasts render over the
        // workspace (gpui-component `Root`; mirrors StoryWorkspace::render).
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // The title text shows the active document (app-shell §6 `updateWindowTitle`).
        let title = self
            .state
            .active_tab()
            .map(|t| format!("{} — Reclass", t.title))
            .unwrap_or_else(|| "Reclass".to_string());

        div()
            .id("reclass-main-window")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // Custom frameless titlebar (app-shell §5).
            .child(TitleBar::new().child(div().px_2().child(title)))
            // The docking workspace: center document tabs + side docks (app-shell §8/§10).
            .child(self.dock_area.clone())
            // Overlay layers.
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

/// Open the main application window. Called from `main.rs` inside
/// `gpui_platform::application().run(...)`, after `gpui_component::init(cx)`.
///
/// Builds (or reuses) the [`ThemeManager`] from the [`ThemeRegistryGlobal`],
/// constructs [`MainWindow`], and wraps it in a [`Root`] (required for overlays;
/// gpui-component cookbook §4 verified flow). Mirrors the verified
/// `open_window` → `Root::new` pattern.
pub fn open_main_window(cx: &mut App) {
    // The theme manager is a process-shared handle living in a gpui global.
    let theme_manager = ThemeRegistryGlobal::get(cx);

    // Register the editor-surface + inline-field key bindings (Tab-cycle, Esc,
    // undo/redo, and the inline text-edit keys). Bound here once, in their key
    // contexts (`RcxEditor` / `RcxFieldInput`); editor-surface.md §10/§11.
    let mut bindings = super::editor::editor_key_bindings();
    bindings.extend(super::editor::inline_edit::field_key_bindings());
    cx.bind_keys(bindings);

    cx.spawn(async move |cx| {
        let _ = cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| MainWindow::new(theme_manager.clone(), window, cx));
            cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
        });
    })
    .detach();
}
