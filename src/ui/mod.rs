//! GPUI views — the bespoke editor `Element` (stub) + standard chrome via
//! gpui-component, theme application, and the application shell.
//!
//! Port of `src/editor.*`, `widgets/*`, dialogs, popups, docks, `mainwindow.h`,
//! `startpage.h`, `titlebar.*`. **FOUNDATION** — this stage establishes the
//! shared scaffolding every other UI piece plugs into (ARCHITECTURE §5):
//!
//! - [`design`] — the shared **Zed design system**: spacing/radius/border/
//!   shadow/type tokens + semantic color accessors (reading `cx.theme()`) +
//!   small reusable surface builders. Every surface uses it for a consistent,
//!   themeable look (written spec: `_design/zed_ui_spec.md`).
//! - [`menubar`] / [`statusbar`] — pre-declared chrome stubs the chrome surface
//!   stage fills in (menu row, bottom status strip).
//! - [`state`] — the gpui-free application-state types (open documents/tabs,
//!   active document, active data source, selection, theme handle), with the
//!   window-level reducers unit-tested headlessly.
//! - [`theme_apply`] — maps the [`crate::theme`] model onto gpui-component's
//!   `Theme`/`ThemeColor` so themes load + switch at runtime (themes.md §4.16),
//!   plus the app-shared [`ThemeManager`](crate::theme::ThemeManager) global.
//! - [`window`] — the main-window shell: gpui-component `TitleBar` + a
//!   [`DockArea`](gpui_component::dock::DockArea) (workspace dock, scanner dock,
//!   center MDI document-tab area) wrapped in [`Root`](gpui_component::Root).
//! - [`docks`] — the dock-layout builder + persistence id/version (the seam).
//! - [`panels`] — placeholder [`Panel`](gpui_component::dock::Panel)s for the
//!   workspace / scanner / document surfaces (the seam real content replaces).
//! - [`editor`] — the bespoke raw-gpui structured-editor surface (stub).
//! - [`dialogs`] — the modal/overlay seam (stub catalogue).
//!
//! The seams (traits/structs) are defined so the editor surface, docks, dialogs,
//! and panels can be filled in by the next workflows; the editor grid and real
//! dialogs are deliberately NOT implemented here. Gated behind the `ui` feature.

pub(crate) mod cpp_highlight;
pub mod design;
pub mod dialogs;
pub mod editor;
pub mod overlays;
pub mod pickers;
pub mod examples;
mod navlist;
pub mod fuzzy;
pub mod menubar;
pub mod panels;
pub mod plugindialog;
pub mod pluginhost;
pub mod pluginpanel;
pub mod pluginview;
pub mod startpage;
pub mod state;
pub mod statusbar;
pub mod tabs;
pub mod theme_apply;
pub mod titlebar;
pub mod window;

pub use window::{open_main_window, open_main_window_with, MainWindow, StartupOptions};
