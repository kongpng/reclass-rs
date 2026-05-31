//! Dock panels — the placeholder gpui-component [`Panel`]s the window shell
//! docks, and the seam real content plugs into next.
//!
//! Port of the dockable side panels + MDI document panels (app-shell §8, §10):
//! the workspace ("Project") tree, the memory-scanner panel, the symbols/
//! bookmarks panels, and each open-document editor panel. **SKELETON** — every
//! panel is a placeholder that renders its name; the real content (workspace
//! `Tree`, scanner `DataTable`, the bespoke editor surface) is filled in by the
//! later UI workflows. What's defined here is the *structure*: a single generic
//! [`PlaceholderPanel`] that satisfies gpui-component's `Panel` trait so a
//! [`DockArea`](gpui_component::dock::DockArea) can be assembled and shown.
//!
//! gpui-component's `Panel` requires `EventEmitter<PanelEvent> + Render +
//! Focusable` plus a `panel_name`; this provides exactly that, parameterized by
//! a [`PanelKind`] so one type covers the workspace dock, scanner dock, and the
//! center document area. As real panels arrive they become their own views
//! implementing `Panel`, replacing the matching placeholder.
//!
//! Gated behind the `ui` feature (pulls gpui-component).

use gpui::*;
use gpui_component::{
    dock::{Panel, PanelEvent},
    ActiveTheme,
};

/// Which Reclass surface a placeholder stands in for — picks its title and the
/// real view that will eventually replace it (app-shell mapping).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelKind {
    /// The dockable workspace / project tree (app-shell §10 `createWorkspaceDock`).
    Workspace,
    /// The dockable memory-scanner panel (app-shell §10 `createScannerDock`).
    Scanner,
    /// The center MDI document area (app-shell §8 — the editor surface lives here).
    Document,
}

impl PanelKind {
    /// The panel's display title (the dock header / tab label).
    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Workspace => "Project",
            PanelKind::Scanner => "Memory Scanner",
            PanelKind::Document => "Document",
        }
    }

    /// The stable `panel_name` used for layout (de)serialization
    /// (`DockArea::dump`/`load`). Must be stable across versions.
    pub fn panel_name(self) -> &'static str {
        match self {
            PanelKind::Workspace => "WorkspacePanel",
            PanelKind::Scanner => "ScannerPanel",
            PanelKind::Document => "DocumentPanel",
        }
    }
}

/// A placeholder dock panel — a gpui-component [`Panel`] that renders its name.
///
/// The seam every real panel grows from: it owns a [`FocusHandle`] (required by
/// `Focusable`) and a [`PanelKind`]. Replace per-surface with a dedicated view
/// (workspace tree, scanner table, editor surface) implementing `Panel`.
pub struct PlaceholderPanel {
    kind: PanelKind,
    focus_handle: FocusHandle,
}

impl PlaceholderPanel {
    /// Build a placeholder for the given surface.
    pub fn new(kind: PanelKind, cx: &mut Context<Self>) -> Self {
        PlaceholderPanel {
            kind,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`] (the form a dock holds).
    pub fn view(kind: PanelKind, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| PlaceholderPanel::new(kind, cx))
    }

    /// Which surface this placeholder stands in for.
    pub fn kind(&self) -> PanelKind {
        self.kind
    }
}

impl Panel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        self.kind.panel_name()
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.kind.title())
    }
}

impl EventEmitter<PanelEvent> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Placeholder: centered label naming the surface. Real content arrives
        // in the per-surface UI workflows.
        div()
            .id(SharedString::from(self.kind.panel_name()))
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().muted_foreground)
            .child(format!("{} (placeholder)", self.kind.title()))
    }
}
