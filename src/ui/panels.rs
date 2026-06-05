//! Dock panels — the generic placeholder [`Panel`] for not-yet-built docks.
//!
//! Port of the dockable side panels + MDI document panels (app-shell §8, §10).
//! The **workspace** ("Project") dock is now the real
//! [`WorkspacePanel`](crate::ui::workspace::WorkspacePanel) (a virtualized tree) and
//! the **center document area** is the real
//! [`DocumentArea`](crate::ui::tabs::DocumentArea) (the tab strip + editor host); the
//! remaining docks (memory scanner, symbols, bookmarks) are still stood up with
//! the generic [`PlaceholderPanel`] until their workflows land.
//!
//! What's defined here is the *structure*: a single generic [`PlaceholderPanel`]
//! that satisfies gpui-component's `Panel` trait (`EventEmitter<PanelEvent> +
//! Render + Focusable` + a `panel_name`) so a
//! [`DockArea`](gpui_component::dock::DockArea) can host it. [`DocumentPanel`]
//! (the older single-editor center panel) is kept for reference + reuse; the
//! center is now the multi-tab [`DocumentArea`](crate::ui::tabs::DocumentArea).
//!
//! Gated behind the `ui` feature (pulls gpui-component).

use gpui::*;
use gpui_component::{
    dock::{Panel, PanelEvent},
    ActiveTheme,
};

use crate::ui::editor::RcxEditor;

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
    /// The right-dock modules/symbols/types list (the C++ View ▸ Modules,
    /// `Ctrl+Shift+Y`). The real view is [`crate::ui::modulespanel::ModulesPanel`].
    Modules,
    /// The right-dock bookmarks list (the C++ View ▸ Bookmarks, `Ctrl+Shift+B`).
    /// The real view is [`crate::ui::bookmarkspanel::BookmarksPanel`].
    Bookmarks,
}

impl PanelKind {
    /// The panel's display title (the dock header / tab label).
    pub fn title(self) -> &'static str {
        match self {
            PanelKind::Workspace => "Project",
            PanelKind::Scanner => "Memory Scanner",
            PanelKind::Document => "Document",
            PanelKind::Modules => "Modules",
            PanelKind::Bookmarks => "Bookmarks",
        }
    }

    /// The stable `panel_name` used for layout (de)serialization
    /// (`DockArea::dump`/`load`). Must be stable across versions.
    pub fn panel_name(self) -> &'static str {
        match self {
            PanelKind::Workspace => "WorkspacePanel",
            PanelKind::Scanner => "ScannerPanel",
            PanelKind::Document => "DocumentPanel",
            PanelKind::Modules => crate::ui::modulespanel::PANEL_NAME,
            PanelKind::Bookmarks => crate::ui::bookmarkspanel::PANEL_NAME,
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

/// The center document panel — the real editor surface host.
///
/// A gpui-component [`Panel`] wrapping the bespoke [`RcxEditor`] view (the
/// structured-editor grid). This replaces the [`PanelKind::Document`]
/// placeholder: the MDI document-tab area holds one `DocumentPanel` per open
/// document, each owning its own editor + controller (app-shell §8). For this
/// stage one panel hosts a fresh editor; per-tab document wiring lands with the
/// tab/source workflow.
pub struct DocumentPanel {
    title: SharedString,
    editor: Entity<RcxEditor>,
    focus_handle: FocusHandle,
}

impl DocumentPanel {
    /// Build a document panel hosting a fresh editor.
    pub fn new(
        title: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = RcxEditor::view(window, cx);
        DocumentPanel {
            title: title.into(),
            editor,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`] (the form a dock holds).
    pub fn view(title: impl Into<SharedString>, window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| DocumentPanel::new(title, window, cx))
    }

    /// The hosted editor view (for the app to push documents/options into).
    pub fn editor(&self) -> &Entity<RcxEditor> {
        &self.editor
    }
}

impl Panel for DocumentPanel {
    fn panel_name(&self) -> &'static str {
        "DocumentPanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }
}

impl EventEmitter<PanelEvent> for DocumentPanel {}

impl Focusable for DocumentPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DocumentPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("rcx-document-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.editor.clone())
    }
}
