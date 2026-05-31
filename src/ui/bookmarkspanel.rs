//! Bookmarks dock — saved addresses for the active document (STUB).
//!
//! Port of the Reclass "Bookmarks" side dock: a list of user-saved addresses
//! (with optional labels) for the active document, each row re-navigating the
//! editor to that address when activated.
//!
//! This is a **pre-declared stub** so parallel UI workflows can reference
//! `crate::ui::bookmarkspanel` without racing on `src/ui/mod.rs`. The real panel
//! — a gpui-component [`Panel`](gpui_component::dock::Panel) hosting the
//! bookmark list — is filled in by the Docks stage. Gated behind the `ui`
//! feature.

use gpui::SharedString;

/// The stable `panel_name` for layout (de)serialization
/// (`DockArea::dump`/`load`). Must stay stable once docks persist it.
pub const PANEL_NAME: &str = "BookmarksPanel";

/// The dock's display title (header / tab label).
pub fn title() -> SharedString {
    SharedString::from("Bookmarks")
}
