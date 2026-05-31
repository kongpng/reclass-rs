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
//! [`build_default_layout`] assembles the canonical layout: the center is the
//! real MDI [`DocumentArea`](super::tabs::DocumentArea) (tab strip + "+" sentinel
//! + source icons + view-mode toggle + editor), the left dock is the real
//! [`WorkspacePanel`](super::workspace::WorkspacePanel), and the bottom dock is the
//! real [`ScannerPanel`](super::scannerpanel::ScannerPanel). It returns [`LayoutHandles`] the window
//! wires + observes. The dock drag overlay + per-dock toolbars (app-shell §9)
//! come later; the seam is the layout builder + the [`MAIN_DOCK_AREA`] id/version
//! used by `dump`/`load`.
//!
//! Gated behind the `ui` feature.

use gpui::*;
use gpui_component::dock::{DockArea, DockItem};
use std::sync::Arc;

use super::tabs::DocumentArea;
use super::workspace::WorkspacePanel;

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

/// Handles into the default layout the window keeps to drive + observe the docks.
pub struct LayoutHandles {
    /// The center MDI document-tab area (the `DocumentArea` panel).
    pub document_area: Entity<DocumentArea>,
    /// The left workspace ("Project") dock panel.
    pub workspace: Entity<WorkspacePanel>,
}

/// Assemble the canonical default dock layout into `dock_area`
/// (the C++ "Reset Windows" canonical placement; app-shell §7):
///
/// - **center** = the MDI document-tab area ([`DocumentArea`] — the tab strip,
///   "+" sentinel, source icons, view-mode toggle, and the editor),
/// - **left dock** = the workspace / project tree ([`WorkspacePanel`]),
/// - **bottom dock** = the real memory scanner
///   ([`ScannerPanel`](super::scannerpanel::ScannerPanel)), open by default so its
///   search controls + results table are visible (PIC3/PIC6).
///
/// Mirrors the verified gpui-component `DockArea` construction pattern
/// (`examples/dock.rs`): build `DockItem::tabs(...)` of `Arc<dyn PanelView>`
/// panels against a `WeakEntity<DockArea>`, then `set_center` / `set_left_dock`
/// / `set_bottom_dock`. Sizes follow the C++ defaults (workspace ~280px wide,
/// scanner ~320px tall). Returns the [`LayoutHandles`] the window wires.
pub fn build_default_layout(
    dock_area: &Entity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) -> LayoutHandles {
    let weak = dock_area.downgrade();

    // Center: the document-tab area hosting the bespoke `RcxEditor` grid + the
    // always-visible tab strip with the "+" sentinel and view-mode toggle.
    let document_area = DocumentArea::view(window, cx);
    let center = DockItem::tabs(vec![Arc::new(document_area.clone())], &weak, window, cx);

    // Left: workspace / project tree.
    let workspace = WorkspacePanel::view(window, cx);
    let left = DockItem::tabs(vec![Arc::new(workspace.clone())], &weak, window, cx);

    // Bottom: the real memory-scanner panel (`ScannerPanel`), open by default so
    // its search controls + results table are visible (PIC3/PIC6 show the scanner
    // docked at the bottom of the workspace).
    let scanner = Arc::new(super::scannerpanel::ScannerPanel::view(window, cx));
    let bottom = DockItem::tabs(vec![scanner], &weak, window, cx);

    dock_area.update(cx, |area, cx| {
        area.set_center(center, window, cx);
        area.set_left_dock(left, Some(px(280.)), true, window, cx);
        area.set_bottom_dock(bottom, Some(px(320.)), true, window, cx);
    });

    LayoutHandles {
        document_area,
        workspace,
    }
}
