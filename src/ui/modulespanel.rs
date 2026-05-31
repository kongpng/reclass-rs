//! Modules dock — the loaded-modules / image list (STUB).
//!
//! Port of the Reclass "Modules" side dock: the list of modules/images backing
//! the active data source (a process's loaded DLLs, or the sections of a file),
//! used to resolve addresses to `module+offset` and to jump the editor base.
//!
//! This is a **pre-declared stub** so parallel UI workflows can reference
//! `crate::ui::modulespanel` without racing on `src/ui/mod.rs`. The real panel
//! — a gpui-component [`Panel`](gpui_component::dock::Panel) hosting the module
//! table — is filled in by the Docks stage. Gated behind the `ui` feature.

use gpui::SharedString;

/// The stable `panel_name` for layout (de)serialization
/// (`DockArea::dump`/`load`). Must stay stable once docks persist it.
pub const PANEL_NAME: &str = "ModulesPanel";

/// The dock's display title (header / tab label).
pub fn title() -> SharedString {
    SharedString::from("Modules")
}
