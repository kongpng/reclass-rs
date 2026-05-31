//! Dock layout — the [`DockArea`] scaffolding for the main window.
//!
//! Port of the C++ dock/MDI machinery (app-shell §8 the document-tab area,
//! §10 the side-panel docks): an MDI document-tab area in the center, a
//! dockable workspace dock (left), and a dockable scanner dock (bottom). In Qt
//! these were `QDockWidget`s tabified into areas with `QSettings` layout
//! persistence; gpui-component models them directly with
//! [`DockArea`](gpui_component::dock::DockArea) / `DockItem` / `Panel` and
//! `dump`/`load` (ARCHITECTURE §5 surface map).
//!
//! **SKELETON** — [`build_default_layout`] assembles the canonical layout out
//! of [`PlaceholderPanel`]s so the window is constructible and shows the right
//! regions; the drag overlay, sentinel "+" tab, source-icon tab chrome, and
//! per-dock toolbars (app-shell §8/§9) come later. The seam is the layout
//! builder + the [`MAIN_DOCK_AREA`] id/version used by `dump`/`load`.
//!
//! Gated behind the `ui` feature.

use gpui::*;
use gpui_component::dock::{DockArea, DockItem};
use std::sync::Arc;

use super::panels::{DocumentPanel, PanelKind, PlaceholderPanel};

/// Identity + layout version for the main dock area, used as the `dump`/`load`
/// key (the C++ `QSettings` dock-layout slot; app-shell §10 dock persistence).
///
/// Bump [`version`](DockAreaId::version) whenever the default layout changes so
/// stale persisted layouts are discarded on load (mirrors gpui-component's
/// `DockArea` version gate).
pub struct DockAreaId {
    /// Stable id string.
    pub id: &'static str,
    /// Layout version — bump on incompatible default-layout changes.
    pub version: usize,
}

/// The main window's dock area identity.
pub const MAIN_DOCK_AREA: DockAreaId = DockAreaId {
    id: "reclass-main-dock",
    version: 1,
};

/// Assemble the canonical default dock layout into `dock_area`
/// (the C++ "Reset Windows" canonical placement; app-shell §7):
///
/// - **center** = MDI document-tab area (a placeholder [`PanelKind::Document`]),
/// - **left dock** = workspace / project tree ([`PanelKind::Workspace`]),
/// - **bottom dock** = memory scanner ([`PanelKind::Scanner`]), closed by
///   default (the C++ scanner dock is hidden until toggled; app-shell §10).
///
/// Mirrors the verified gpui-component `DockArea` construction pattern
/// (`examples/dock.rs`): build `DockItem::tabs(...)` of `Arc<dyn PanelView>`
/// panels against a `WeakEntity<DockArea>`, then `set_center` / `set_left_dock`
/// / `set_bottom_dock`. Sizes follow the C++ defaults (workspace ~280px wide,
/// scanner ~360px tall).
pub fn build_default_layout(dock_area: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
    let weak = dock_area.downgrade();

    // Center: the document-tab area hosting the real editor surface
    // (the bespoke `RcxEditor` grid). One document for now; per-tab wiring lands
    // with the tab/source workflow.
    let document = Arc::new(DocumentPanel::view("Untitled", window, cx));
    let center = DockItem::tabs(vec![document], &weak, window, cx);

    // Left: workspace / project tree.
    let workspace = Arc::new(PlaceholderPanel::view(PanelKind::Workspace, cx));
    let left = DockItem::tabs(vec![workspace], &weak, window, cx);

    // Bottom: memory scanner (closed by default, like the C++ hidden scanner dock).
    let scanner = Arc::new(PlaceholderPanel::view(PanelKind::Scanner, cx));
    let bottom = DockItem::tabs(vec![scanner], &weak, window, cx);

    dock_area.update(cx, |area, cx| {
        area.set_center(center, window, cx);
        area.set_left_dock(left, Some(px(280.)), true, window, cx);
        area.set_bottom_dock(bottom, Some(px(360.)), false, window, cx);
    });
}
