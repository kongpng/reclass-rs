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
//! real MDI [`DocumentArea`](crate::ui::chrome::tabs::DocumentArea) (tab strip + "+" sentinel
//! + source icons + view-mode toggle + editor), the left dock is the real
//! [`WorkspacePanel`](crate::ui::panels::workspace::WorkspacePanel), the bottom dock is the
//! real [`ScannerPanel`](crate::ui::panels::scannerpanel::ScannerPanel) (closed by default),
//! and the **right** dock tabifies the
//! [`ModulesPanel`](crate::ui::panels::modulespanel::ModulesPanel) +
//! [`BookmarksPanel`](crate::ui::panels::bookmarkspanel::BookmarksPanel) (also closed by
//! default — the C++ View ▸ Modules / Bookmarks summon them on demand). It
//! returns [`LayoutHandles`] the window wires + observes. The dock drag overlay +
//! per-dock toolbars (app-shell §9) come later; the seam is the layout builder +
//! the [`MAIN_DOCK_AREA`] id/version used by `dump`/`load`.
//!
//! Gated behind the `ui` feature.

use gpui::*;
use gpui_component::dock::{DockArea, DockItem};
use std::sync::Arc;

use crate::ui::panels::bookmarkspanel::BookmarksPanel;
use crate::ui::panels::modulespanel::ModulesPanel;
use crate::ui::panels::scannerpanel::ScannerPanel;
use crate::ui::chrome::tabs::DocumentArea;
use crate::ui::panels::workspace::WorkspacePanel;

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
    /// The right-dock Modules / Symbols / Types panel (the C++ View ▸ Modules,
    /// `Ctrl+Shift+Y`). Tabified with [`bookmarks`](Self::bookmarks) in the
    /// **right** dock, **closed by default** — summoned from the View menu like
    /// the C++. The window keeps this so it can toggle/observe the dock.
    pub modules: Entity<ModulesPanel>,
    /// The right-dock Bookmarks panel (the C++ View ▸ Bookmarks,
    /// `Ctrl+Shift+B`). Shares the right dock's tab strip with
    /// [`modules`](Self::modules); the window pushes the document's bookmark list
    /// into it on change and observes its `BookmarkAction` intents.
    pub bookmarks: Entity<BookmarksPanel>,
    /// The bottom-dock memory [`ScannerPanel`] (the C++ View ▸ Memory Scanner,
    /// `Ctrl+Shift+M`), **closed by default**. The window MUST keep this handle:
    /// the scanner is built here but was previously dropped (only its `Arc` went
    /// into the dock), so `ScannerPanel::set_provider` could never be called and
    /// the scanner could never scan. Storing it lets the window wire the active
    /// document's provider in (and observe `ScannerNav` / `ScannerEdit`).
    pub scanner: Entity<ScannerPanel>,
}

/// Assemble the canonical default dock layout into `dock_area`
/// (the C++ "Reset Windows" canonical placement; app-shell §7):
///
/// - **center** = the MDI document-tab area ([`DocumentArea`] — the tab strip,
///   "+" sentinel, source icons, view-mode toggle, and the editor),
/// - **left dock** = the workspace / project tree ([`WorkspacePanel`]),
/// - **bottom dock** = the real memory scanner
///   ([`ScannerPanel`](crate::ui::panels::scannerpanel::ScannerPanel)), **closed by default** —
///   the C++ memory scanner is a separate pop-out summoned on demand
///   (`reclass_memory_scanner.png` shows it as its own detached window, hidden
///   until requested), not an always-present panel. It is revealed by
///   View ▸ Memory Scanner / `Ctrl+Shift+M`
///   ([`toggle_scanner_dock`](crate::ui::window::MainWindow::toggle_scanner_dock)).
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

    // Bottom: the real memory-scanner panel (`ScannerPanel`), **closed by
    // default**. The C++ scanner is a separate pop-out summoned on demand
    // (`reclass_memory_scanner.png`), so the window must launch with NO scanner
    // visible; View ▸ Memory Scanner / `Ctrl+Shift+M` toggles it
    // (`MainWindow::toggle_scanner_dock`). The panel is still built + registered
    // here so the toggle has it ready — only its `open` flag starts `false`.
    let scanner = ScannerPanel::view(window, cx);
    let bottom = DockItem::tabs(vec![Arc::new(scanner.clone())], &weak, window, cx);

    // Right: the Modules / Symbols / Types panel + the Bookmarks panel,
    // **tabified together**, **closed by default**. The C++ surfaces both from
    // the View menu (Modules `Ctrl+Shift+Y`, Bookmarks `Ctrl+Shift+B`) — they are
    // not always-present panels, so the dock launches closed; the View toggles
    // (`MainWindow::toggle_modules_dock` / `toggle_bookmarks_dock`) reveal it.
    // Both panels are still built + registered here so the toggle has them ready
    // — only the dock's `open` flag starts `false`.
    let modules = ModulesPanel::view(window, cx);
    let bookmarks = BookmarksPanel::view(window, cx);
    let right = DockItem::tabs(
        vec![Arc::new(modules.clone()), Arc::new(bookmarks.clone())],
        &weak,
        window,
        cx,
    );

    dock_area.update(cx, |area, cx| {
        area.set_center(center, window, cx);
        area.set_left_dock(left, Some(px(280.)), true, window, cx);
        area.set_bottom_dock(bottom, Some(px(320.)), false, window, cx);
        area.set_right_dock(right, Some(px(280.)), false, window, cx);
    });

    LayoutHandles {
        document_area,
        workspace,
        modules,
        bookmarks,
        scanner,
    }
}
