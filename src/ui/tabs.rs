//! MDI document tabs — the center area that hosts one editor per open document,
//! with the always-visible tab strip, the "+" new-tab sentinel, per-tab source
//! icons, and the dual tree/rendered view-mode toggle.
//!
//! Port of the C++ document-tab / MDI system (app-shell §8). In Qt each open
//! struct view was a tabified `QDockWidget` with a sentinel `​` dock keeping the
//! `QTabBar` visible and supplying the "+" affordance. The cookbook (app-shell §8
//! "Rust/GPUI sentinel note") says to **render the tab strip directly** instead:
//! a persistent "+" tab + the documents as a `Vec`. That is what [`DocumentArea`]
//! does — it is the [`DockArea`](gpui_component::dock::DockArea) center
//! [`Panel`](gpui_component::dock::Panel), rendering a [`TabBar`] of
//! [`DocEntry`]s above the active document's [`RcxEditor`].
//!
//! Behaviors reproduced (app-shell §8 "replicate the behaviors"):
//! - **always-visible strip** + a trailing **"+" tab** that opens a new document,
//! - **per-tab source icon** (full opacity = live, dimmed = disconnected),
//! - **active-tab follows selection / close** (the `m_activeDocDock` rules),
//! - **never leave a blank area** — closing the last tab opens a fresh document,
//! - the **dual view-mode toggle** (tree ⇄ rendered C/C++) in the strip suffix.
//!
//! The drag-to-reorder/redock overlay, middle-click close, and right-click tab
//! context menu (app-shell §8/§9) are layered on by later workflows; this stage
//! establishes the strip + sentinel + source-icon + view-toggle chrome and wires
//! it to the editor surface.
//!
//! Gated behind the `ui` feature.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::{ActiveTheme, Selectable as _, Sizable as _};

use super::editor::RcxEditor;
use super::state::{DataSource, DocId, SourceKind, ViewMode};
use super::titlebar::source_icon;

/// One open document tab in the center area — the per-tab UI state + its editor.
///
/// Mirrors the C++ `TabState` (the bits the center needs; app-shell §6): the
/// stable [`DocId`], the title, the active data source (the tab source-icon), the
/// per-pane view mode, and the owned [`RcxEditor`] view (one editor per tab — the
/// "one document per tab" model).
pub struct DocEntry {
    pub id: DocId,
    pub title: SharedString,
    pub source: DataSource,
    pub view_mode: ViewMode,
    pub editor: Entity<RcxEditor>,
}

impl DocEntry {
    fn new(id: DocId, title: impl Into<SharedString>, editor: Entity<RcxEditor>) -> Self {
        DocEntry {
            id,
            title: title.into(),
            source: DataSource::none(),
            view_mode: ViewMode::default(),
            editor,
        }
    }
}

/// An event the document area raises to the window (the C++ signal wiring,
/// app-shell §8 step 9). The window reflects these into [`AppState`](super::state)
/// and the workspace title.
#[derive(Clone, Debug)]
pub enum DocAreaEvent {
    /// A tab became active (`visibilityChanged` → `m_activeDocDock`).
    Activated(DocId),
    /// The "+" sentinel was clicked — open a fresh document (`project_new`).
    NewDocumentRequested,
    /// A tab was closed (`dock.destroyed`).
    Closed(DocId),
    /// The active tab's view mode changed (the dual toggle; `setViewMode`).
    ViewModeChanged(DocId, ViewMode),
}

/// The center MDI document area: a [`TabBar`] + the active editor.
///
/// A gpui-component [`Panel`] (the `DockArea` center). Owns the ordered tab list
/// and the active index, and emits [`DocAreaEvent`]s the window observes.
pub struct DocumentArea {
    tabs: Vec<DocEntry>,
    active: usize,
    focus_handle: FocusHandle,
    /// Monotonic id allocator — independent of the window's [`AppState`] so the
    /// area is self-contained, but kept in lockstep by the window's wiring.
    next_id: u64,
}

impl DocumentArea {
    /// Build the area with one initial document (the C++ "never leave a blank
    /// window": a document is always present; app-shell §8 step 9).
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut area = DocumentArea {
            tabs: Vec::new(),
            active: 0,
            focus_handle: cx.focus_handle(),
            next_id: 0,
        };
        area.push_document("Untitled", window, cx);
        area
    }

    /// Construct as an [`Entity`] (the form the dock holds).
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| DocumentArea::new(window, cx))
    }

    /// Allocate the next document id (monotonic, never reused).
    fn alloc_id(&mut self) -> DocId {
        self.next_id += 1;
        DocId::from_raw(self.next_id)
    }

    /// Append a new document tab hosting a fresh editor, and make it active
    /// (`createTab` + `m_activeDocDock = dock`). Returns its id.
    pub fn push_document(
        &mut self,
        title: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DocId {
        let id = self.alloc_id();
        let editor = RcxEditor::view(window, cx);
        self.tabs.push(DocEntry::new(id, title, editor));
        self.active = self.tabs.len() - 1;
        id
    }

    /// The active document entry, if any.
    pub fn active_entry(&self) -> Option<&DocEntry> {
        self.tabs.get(self.active)
    }

    /// The active document's editor (for the window to push documents/options into).
    pub fn active_editor(&self) -> Option<&Entity<RcxEditor>> {
        self.active_entry().map(|e| &e.editor)
    }

    /// All open tabs, in strip order.
    pub fn tabs(&self) -> &[DocEntry] {
        &self.tabs
    }

    /// The active tab index.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Index of a tab by id.
    fn index_of(&self, id: DocId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Set a tab's source (the tab source-icon + liveness; `refreshDocTabSourceIcon`).
    pub fn set_source(&mut self, id: DocId, source: DataSource, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            self.tabs[i].source = source;
            cx.notify();
        }
    }

    /// Rename a tab (the `rootName(tree)` change → tab title).
    pub fn set_title(&mut self, id: DocId, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            self.tabs[i].title = title.into();
            cx.notify();
        }
    }

    /// Activate a tab by index (a tab-strip click). Emits [`DocAreaEvent::Activated`].
    fn activate_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.tabs.len() && ix != self.active {
            self.active = ix;
            let id = self.tabs[ix].id;
            cx.emit(DocAreaEvent::Activated(id));
            cx.notify();
        }
    }

    /// The "+" sentinel was clicked: open a fresh document and signal the window
    /// (`project_new`; app-shell §8 sentinel "+" click).
    fn on_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Default new-document title mirrors the C++ "UnnamedClassN" scheme at the
        // shell level; the real root name follows once a document is attached.
        self.push_document("Untitled", window, cx);
        cx.emit(DocAreaEvent::NewDocumentRequested);
        cx.notify();
    }

    /// Close the tab at `ix`, fixing up the active index (`dock.destroyed`:
    /// reassign `m_activeDocDock` to the last remaining tab). When the final tab
    /// closes, a fresh one is opened so the area is never blank (the C++
    /// "never leave a blank window" reflex; app-shell §8/§20).
    pub fn close_index(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let was_active = ix == self.active;
        let closed = self.tabs.remove(ix);
        cx.emit(DocAreaEvent::Closed(closed.id));

        if self.tabs.is_empty() {
            // Never leave a blank area.
            self.push_document("Untitled", window, cx);
            cx.emit(DocAreaEvent::NewDocumentRequested);
        } else {
            // Fix up the active index so the *same* document stays active:
            // - the active tab itself was closed → reassign to the last tab
            //   (the C++ "reassign `m_activeDocDock` to last" rule);
            // - a tab *before* the active one was removed → the active element
            //   shifted left by one, so decrement to follow it;
            // - a tab *after* the active one was removed → the active index is
            //   unchanged.
            if was_active {
                self.active = self.tabs.len() - 1;
            } else if ix < self.active {
                self.active -= 1;
            }
            self.active = self.active.min(self.tabs.len() - 1);
            let id = self.tabs[self.active].id;
            cx.emit(DocAreaEvent::Activated(id));
        }
        cx.notify();
    }

    /// Toggle the active tab's view mode (the dual tree/rendered toggle). Emits
    /// [`DocAreaEvent::ViewModeChanged`].
    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.tabs.get_mut(self.active) {
            entry.view_mode = entry.view_mode.toggled();
            let (id, mode) = (entry.id, entry.view_mode);
            cx.emit(DocAreaEvent::ViewModeChanged(id, mode));
            cx.notify();
        }
    }

    /// Set the active tab's view mode explicitly (`setViewMode` from the window).
    pub fn set_active_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if let Some(entry) = self.tabs.get_mut(self.active) {
            if entry.view_mode != mode {
                entry.view_mode = mode;
                cx.notify();
            }
        }
    }

    /// Build the tab strip: one [`Tab`] per document (source icon + title + close
    /// ✕) followed by the trailing "+" sentinel and the view-mode toggle suffix.
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;
        let view_mode = self.active_entry().map(|e| e.view_mode).unwrap_or_default();
        let has_doc = !self.tabs.is_empty();

        let tab_bar = TabBar::new("rcx-doc-tabs")
            .selected_index(active)
            .children(self.tabs.iter().enumerate().map(|(ix, entry)| {
                let id = entry.id;
                // Per-tab source icon (full opacity = live, dimmed = disconnected).
                let icon = source_icon(entry.source.kind, entry.source.live, cx);
                // Close ✕ on the right (the C++ `DockTabButtons` close button).
                let close = Button::new(SharedString::from(format!("tab-close-{}", id.get())))
                    .ghost()
                    .xsmall()
                    .label("\u{2715}") // ✕
                    .tooltip("Close")
                    .on_click(cx.listener(move |this, _e, window, cx| {
                        if let Some(i) = this.index_of(id) {
                            this.close_index(i, window, cx);
                        }
                    }));

                Tab::new()
                    .selected(ix == active)
                    .prefix(icon)
                    .label(entry.title.clone())
                    .suffix(close)
                    .on_click(cx.listener(move |this, _e, _window, cx| {
                        this.activate_index(ix, cx);
                    }))
            }))
            // The trailing "+" new-tab sentinel (app-shell §8).
            .suffix(
                gpui_component::h_flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .child(
                        Button::new("rcx-new-tab")
                            .ghost()
                            .small()
                            .label("+")
                            .tooltip("New document")
                            .on_click(cx.listener(|this, _e, window, cx| {
                                this.on_new_tab(window, cx);
                            })),
                    )
                    // The dual view-mode toggle (tree ⇄ rendered C/C++).
                    .child(
                        Button::new("rcx-view-mode")
                            .ghost()
                            .small()
                            .selected(view_mode == ViewMode::Rendered)
                            .label(view_mode.label())
                            .tooltip(match view_mode {
                                ViewMode::Tree => "Switch to rendered C/C++",
                                ViewMode::Rendered => "Switch to tree view",
                            })
                            .when(has_doc, |b| {
                                b.on_click(cx.listener(|this, _e, _window, cx| {
                                    this.toggle_view_mode(cx);
                                }))
                            }),
                    ),
            );

        tab_bar
    }

    /// The body for the active tab: the editor (tree mode) or a rendered-output
    /// placeholder (rendered mode). The real rendered C/C++ Scintilla view is
    /// added with the editor/codegen workflow; here the toggle switches surfaces
    /// faithfully and the rendered side shows the generated text affordance.
    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("No document")
                .into_any_element();
        };

        match entry.view_mode {
            ViewMode::Tree => entry.editor.clone().into_any_element(),
            ViewMode::Rendered => div()
                .id("rcx-rendered-view")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .text_color(cx.theme().muted_foreground)
                .font_family("monospace")
                .child(format!(
                    "// Rendered C/C++ for {} (generated view)",
                    entry.title
                ))
                .into_any_element(),
        }
    }
}

impl Panel for DocumentArea {
    fn panel_name(&self) -> &'static str {
        "DocumentArea"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // The dock-tab title for the area itself; the per-document strip lives
        // inside the panel body.
        SharedString::from("Documents")
    }

    fn closable(&self, _cx: &App) -> bool {
        // The center document area is never closed (it always holds a document).
        false
    }
}

impl EventEmitter<PanelEvent> for DocumentArea {}
impl EventEmitter<DocAreaEvent> for DocumentArea {}

impl Focusable for DocumentArea {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DocumentArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = self.render_tab_strip(cx);
        let body = self.render_body(cx);

        div()
            .id("rcx-document-area")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(strip)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// A tiny extension so the area can map [`SourceKind`] → its tab source-icon
/// without exposing the titlebar glyph helper everywhere. (Kept for the tab/icon
/// workflow to swap glyphs for the real SVG assets.)
pub fn tab_source_icon(kind: SourceKind, live: bool, cx: &App) -> impl IntoElement {
    source_icon(kind, live, cx)
}

#[cfg(test)]
mod tests {
    // Pure state-transition tests for the tab model. The gpui-bound rendering is
    // covered by the build; here we test the active-index reducer logic directly
    // on a lightweight mirror of `DocumentArea`'s rules (the same invariants the
    // gpui methods enforce: active follows close, never blank, monotonic ids),
    // to keep them gpui-free and deterministic.
    use super::super::state::{DocId, ViewMode};

    /// A gpui-free mirror of `DocumentArea`'s tab list + active-index rules.
    struct TabModel {
        ids: Vec<DocId>,
        active: usize,
        next: u64,
    }
    impl TabModel {
        fn new() -> Self {
            let mut m = TabModel {
                ids: Vec::new(),
                active: 0,
                next: 0,
            };
            m.push();
            m
        }
        fn push(&mut self) -> DocId {
            self.next += 1;
            let id = DocId::from_raw(self.next);
            self.ids.push(id);
            self.active = self.ids.len() - 1;
            id
        }
        fn index_of(&self, id: DocId) -> Option<usize> {
            self.ids.iter().position(|&x| x == id)
        }
        fn activate(&mut self, ix: usize) {
            if ix < self.ids.len() {
                self.active = ix;
            }
        }
        // Mirror of close_index: emit-less, but the same active fix-up rules.
        fn close(&mut self, ix: usize) {
            if ix >= self.ids.len() {
                return;
            }
            let was_active = ix == self.active;
            self.ids.remove(ix);
            if self.ids.is_empty() {
                self.push();
            } else {
                if was_active {
                    self.active = self.ids.len() - 1;
                } else if ix < self.active {
                    self.active -= 1;
                }
                self.active = self.active.min(self.ids.len() - 1);
            }
        }
    }

    #[test]
    fn starts_with_one_active_document() {
        let m = TabModel::new();
        assert_eq!(m.ids.len(), 1);
        assert_eq!(m.active, 0);
    }

    #[test]
    fn ids_are_monotonic_and_unique() {
        let mut m = TabModel::new();
        let a = m.ids[0];
        let b = m.push();
        let c = m.push();
        assert!(a.get() < b.get() && b.get() < c.get());
    }

    #[test]
    fn push_makes_new_tab_active() {
        let mut m = TabModel::new();
        let _b = m.push();
        assert_eq!(m.active, 1);
        let _c = m.push();
        assert_eq!(m.active, 2);
    }

    #[test]
    fn close_last_tab_opens_a_fresh_one() {
        // Never leave a blank area (app-shell §8/§20).
        let mut m = TabModel::new();
        let only = m.ids[0];
        m.close(0);
        assert_eq!(m.ids.len(), 1);
        // The fresh tab has a new id (not the closed one).
        assert_ne!(m.ids[0], only);
        assert_eq!(m.active, 0);
    }

    #[test]
    fn closing_active_reassigns_active() {
        let mut m = TabModel::new(); // [1]
        m.push(); // [1,2]
        m.push(); // [1,2,3], active=2
                  // Close the active last tab → active clamps to new last (index 1).
        m.close(2);
        assert_eq!(m.ids.len(), 2);
        assert_eq!(m.active, 1);

        // Re-grow, then close a tab *before* the active one → active shifts left.
        m.push(); // [.. , active=2]
        let before = m.active;
        m.close(0);
        assert_eq!(m.active, before - 1);
    }

    #[test]
    fn closing_non_active_keeps_active_document() {
        let mut m = TabModel::new(); // [1]
        m.push(); // [1,2]
        m.push(); // [1,2,3] active=2
        m.activate(2);
        let active_id = m.ids[2];
        // Close the first (non-active) tab.
        m.close(0);
        // The same document remains active.
        assert_eq!(m.ids[m.active], active_id);
    }

    #[test]
    fn view_mode_default_is_tree() {
        assert_eq!(ViewMode::default(), ViewMode::Tree);
    }
}
