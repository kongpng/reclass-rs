//! Application-state types — the shared scaffolding every UI piece plugs into.
//!
//! Models the C++ `MainWindow`'s document/tab/source bookkeeping (app-shell
//! §6, §8) as plain Rust data, independent of gpui so it can be unit-tested
//! headlessly:
//!
//! - [`AppState`] — open documents/tabs, the active document, the active data
//!   source, the current selection, and the theme handle.
//! - [`DocId`] — a stable id for an open document tab (replaces the C++
//!   `QDockWidget*` key into `m_tabs`; app-shell §6 "Rust modeling").
//! - [`DocTab`] — per-open-document UI state (title, source, view-root,
//!   selection); the editor surface / docks fill in the rest later.
//! - [`Selection`] — the active node selection mirrored from the controller's
//!   `nodeSelected` / `selectionChanged` signals (app-shell §8 step 9).
//!
//! The real `core::Document` / `controller::Controller` own the tree + provider
//! + undo stack; this layer holds only the *window-level* state that the C++
//! `MainWindow` kept alongside them (which dock is active, the tab order, the
//! status selection). Keeping it here — and gpui-free — lets the reducer logic
//! (open/close/activate/select) be tested without a display.

use std::num::NonZeroU64;

/// Stable identifier for an open document tab.
///
/// Replaces the C++ `QDockWidget*` used as the key into `m_tabs` and the
/// ordered `m_docDocks` vector (app-shell §6). A monotonic id is allocation-
/// independent (unlike a pointer), so the tab order and "active tab" survive
/// open/close churn deterministically.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct DocId(NonZeroU64);

impl DocId {
    /// The raw numeric id (for debugging / serialization keys).
    pub fn get(self) -> u64 {
        self.0.get()
    }

    /// Construct from a raw non-zero id. Used by the document-tab area, which
    /// owns its own monotonic allocator kept in lockstep with [`AppState`].
    ///
    /// # Panics
    /// Panics if `raw == 0` (a `DocId` is always a live, allocated id).
    pub fn from_raw(raw: u64) -> Self {
        DocId(NonZeroU64::new(raw).expect("DocId must be non-zero"))
    }
}

/// The active data source backing a document — the UI-facing summary of the
/// provider the controller is attached to (app-shell §8 `refreshDocTabSourceIcon`).
///
/// The real byte access goes through [`crate::provider::Provider`]; this enum
/// only records *which kind* of source is active, for the tab source-icon +
/// liveness chrome. Mirrors the built-in providers (file / buffer / snapshot /
/// null) plus the live `Process` source, which is read through the memflow
/// provider.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SourceKind {
    /// No source attached yet (the `:/vsicons/plug.svg` "No source" fallback).
    #[default]
    None,
    /// An open binary file (the C++ "File" source).
    File,
    /// An in-memory buffer.
    Buffer,
    /// A captured snapshot.
    Snapshot,
    /// A live OS process, read through the memflow provider.
    Process,
}

impl SourceKind {
    /// Short human label for tab tooltips / the source chooser.
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::None => "No source",
            SourceKind::File => "File",
            SourceKind::Buffer => "Buffer",
            SourceKind::Snapshot => "Snapshot",
            SourceKind::Process => "Process",
        }
    }
}

/// The active data source: kind + a display target (the file path, module name,
/// or process title shown in the tab tooltip) + a liveness flag.
///
/// Liveness mirrors `provider && provider->isValid()` (app-shell §8): a live
/// source paints the tab source-icon at full opacity; a disconnected one dims
/// it. Benign sources (file/buffer/snapshot) are live whenever attached.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DataSource {
    pub kind: SourceKind,
    /// The source target description (path / module / pid+title); empty when none.
    pub target: String,
    /// `provider->isValid()` — drives the dimmed/disconnected tab-icon state.
    pub live: bool,
}

impl DataSource {
    /// The "no source" default (kind `None`, not live).
    pub fn none() -> Self {
        DataSource::default()
    }

    /// A live source of the given kind/target.
    pub fn new(kind: SourceKind, target: impl Into<String>) -> Self {
        DataSource {
            kind,
            target: target.into(),
            live: true,
        }
    }
}

/// The per-pane view mode — which of the editor's rendering surfaces is shown
/// (the C++ `enum ViewMode`, app-shell §6; the per-`SplitPane` Reclass/Code/Debug
/// tab). The chrome stage wires the dual toggle the titlebar/tab strip exposes:
/// the structured **tree** view vs the generated **rendered** C/C++ output. The
/// third `Debug` surface exists in the C++ `SplitPane` but is not part of the
/// dual toggle and is added with the editor split work.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ViewMode {
    /// `VM_Reclass` — the bespoke structured-editor grid (default; app-shell §6).
    #[default]
    Tree,
    /// `VM_Rendered` — the generated C/C++ source for the viewed struct.
    Rendered,
    /// `VM_Debug` — the read-only developer dump of the composed line/`LineMeta`
    /// model (the C++ `SplitPane` debug surface; main.cpp `generateDebugText`).
    /// Reached via the 3-way [`ViewMode::toggled`] cycle, not the dual titlebar
    /// toggle.
    Debug,
}

impl ViewMode {
    /// Short label for the toggle button / tooltips.
    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Tree => "Tree",
            ViewMode::Rendered => "C/C++",
            ViewMode::Debug => "Debug",
        }
    }

    /// The next mode in the C++ 3-way cycle: `VM_Reclass`→`VM_Rendered`→`VM_Debug`
    /// →`VM_Reclass` (the SplitPane view index `0`→`1`→`2`→`0`).
    pub fn toggled(self) -> Self {
        match self {
            ViewMode::Tree => ViewMode::Rendered,
            ViewMode::Rendered => ViewMode::Debug,
            ViewMode::Debug => ViewMode::Tree,
        }
    }
}

/// The active node selection, mirrored from the controller's selection signals
/// (`nodeSelected(idx)` / `selectionChanged(count)`, app-shell §8 step 9).
///
/// Used to drive the status-bar string ("Struct.field +0xNN", "N nodes
/// selected") and any selection-dependent chrome. The authoritative selection
/// lives in the controller; this is the window-level cache the status bar reads.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Selection {
    /// Number of selected nodes (`selectionChanged` count). 0 = nothing selected.
    pub count: usize,
    /// The primary selected node id, if exactly one drives the rich status line.
    pub primary: Option<u64>,
}

impl Selection {
    /// Nothing selected.
    pub fn empty() -> Self {
        Selection::default()
    }

    /// `true` when at least one node is selected.
    pub fn is_active(&self) -> bool {
        self.count > 0
    }
}

/// Per-open-document window state — one entry per document tab.
///
/// Holds only the UI/window-level bookkeeping the C++ `TabState` kept beside
/// the `RcxDocument`/`RcxController` (app-shell §6): the tab title, the active
/// data source (for the tab icon), the view-root being shown, and the cached
/// selection. The tree / provider / undo stack live in the engine layer and are
/// referenced by id; this struct deliberately does NOT own them so the state
/// reducers stay gpui-free and testable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocTab {
    /// Stable id (tab-strip key).
    pub id: DocId,
    /// Tab title — the root struct name (`rootName(tree)`); updated on rename.
    pub title: String,
    /// The active data source backing this document (tab source-icon + liveness).
    pub source: DataSource,
    /// The view-root node id currently displayed (the struct shown in the grid),
    /// if any (`ctrl->setViewRootId`).
    pub view_root: Option<u64>,
    /// Cached selection for the status bar.
    pub selection: Selection,
    /// The active pane's view mode — structured tree vs rendered C/C++ output
    /// (the dual toggle; app-shell §6, §16 `syncViewButtons`). Per-tab so the
    /// strip + toggle reflect the active document.
    pub view_mode: ViewMode,
}

impl DocTab {
    fn new(id: DocId, title: impl Into<String>) -> Self {
        DocTab {
            id,
            title: title.into(),
            source: DataSource::none(),
            view_root: None,
            selection: Selection::empty(),
            view_mode: ViewMode::default(),
        }
    }
}

/// The window-level application state — the gpui-free heart of the UI.
///
/// Mirrors the document-management members of the C++ `MainWindow`
/// (`m_tabs` / `m_docDocks` / `m_activeDocDock`, app-shell §6) plus the active
/// data source, current selection, and the active theme name (the theme handle
/// the window re-renders from). It is held by the gpui root view (a `Global` /
/// the `MainWindow` entity) and mutated through the reducer methods below; the
/// reducers carry the C++ behavioral invariants (e.g. "never leave a blank
/// window", "active tab follows close") and are unit-tested headlessly.
///
/// The 8-default theme set + switching live in [`crate::theme::ThemeManager`];
/// `AppState` stores only the *current theme name* (the selection handle),
/// matching the C++ persistence of the `"theme"` key by name (themes.md §4.0).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AppState {
    /// Open document tabs, in tab-strip order (`m_docDocks`).
    tabs: Vec<DocTab>,
    /// The active document id (`m_activeDocDock`), `None` when no tabs are open.
    active: Option<DocId>,
    /// Monotonic id allocator (never reuses ids; pointer-independent).
    next_id: u64,
    /// The active theme's display name — the theme handle (themes.md §4.0).
    /// Empty until a theme is applied.
    theme_name: String,
}

impl AppState {
    /// A fresh, empty application state (no documents, no theme selected).
    pub fn new() -> Self {
        AppState::default()
    }

    // ── Documents / tabs ──

    /// All open tabs, in tab-strip order.
    pub fn tabs(&self) -> &[DocTab] {
        &self.tabs
    }

    /// The number of open documents.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// `true` when no documents are open (the C++ "blank window" condition that
    /// triggers `project_new()`; app-shell §8 step 9).
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The active document id, if any.
    pub fn active_id(&self) -> Option<DocId> {
        self.active
    }

    /// The active tab, if any.
    pub fn active_tab(&self) -> Option<&DocTab> {
        self.active.and_then(|id| self.tab(id))
    }

    /// Mutable access to the active tab.
    pub fn active_tab_mut(&mut self) -> Option<&mut DocTab> {
        let id = self.active?;
        self.tab_mut(id)
    }

    /// Look up a tab by id.
    pub fn tab(&self, id: DocId) -> Option<&DocTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    /// Mutable look-up of a tab by id.
    pub fn tab_mut(&mut self, id: DocId) -> Option<&mut DocTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// Index of a tab in the strip, if present.
    pub fn index_of(&self, id: DocId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Open a new document tab with the given title and make it active —
    /// the `createTab` registration + `m_activeDocDock = dock` step
    /// (app-shell §8 step 7). Returns the new id.
    pub fn open_document(&mut self, title: impl Into<String>) -> DocId {
        self.next_id += 1;
        // next_id starts at 0, so the first allocation is 1 (NonZero-safe).
        let id = DocId(NonZeroU64::new(self.next_id).expect("doc id counter overflow"));
        self.tabs.push(DocTab::new(id, title));
        self.active = Some(id);
        id
    }

    /// Activate an existing tab (the `visibilityChanged` → `m_activeDocDock`
    /// path, app-shell §8 step 9). No-op if the id is unknown.
    pub fn activate(&mut self, id: DocId) {
        if self.index_of(id).is_some() {
            self.active = Some(id);
        }
    }

    /// Close a document tab and fix up the active selection
    /// (`dock.destroyed`, app-shell §8 step 9): if the closed tab was active,
    /// the active id is reassigned to the **last** remaining tab (the C++
    /// "reassign `m_activeDocDock` to last" rule), else `None` when empty.
    ///
    /// Returns `true` if a tab was removed. Note the C++ "never leave a blank
    /// window" reflex (re-open a new doc when the last closes) is a window-level
    /// policy left to the caller — this reducer only maintains tab/active
    /// consistency.
    pub fn close_document(&mut self, id: DocId) -> bool {
        let Some(idx) = self.index_of(id) else {
            return false;
        };
        self.tabs.remove(idx);
        if self.active == Some(id) {
            self.active = self.tabs.last().map(|t| t.id);
        }
        true
    }

    /// Set a document's active data source (the tab source-icon + liveness;
    /// app-shell §8 `refreshDocTabSourceIcon`). No-op if the id is unknown.
    pub fn set_source(&mut self, id: DocId, source: DataSource) {
        if let Some(t) = self.tab_mut(id) {
            t.source = source;
        }
    }

    /// The active document's data source, or the "no source" default.
    pub fn active_source(&self) -> DataSource {
        self.active_tab()
            .map(|t| t.source.clone())
            .unwrap_or_default()
    }

    /// Update a document's cached selection (the controller's `nodeSelected` /
    /// `selectionChanged` signals; app-shell §8 step 9). No-op if unknown.
    pub fn set_selection(&mut self, id: DocId, selection: Selection) {
        if let Some(t) = self.tab_mut(id) {
            t.selection = selection;
        }
    }

    /// Rename a document tab (the `rootName(tree)` change → tab title;
    /// app-shell §8). No-op if unknown.
    pub fn set_title(&mut self, id: DocId, title: impl Into<String>) {
        if let Some(t) = self.tab_mut(id) {
            t.title = title.into();
        }
    }

    // ── View mode (the dual tree/rendered toggle) ──

    /// The active tab's view mode (defaults to `Tree` when no tab is open).
    pub fn active_view_mode(&self) -> ViewMode {
        self.active_tab().map(|t| t.view_mode).unwrap_or_default()
    }

    /// Set a document's view mode (the toggle button / `setViewMode`; app-shell
    /// §16). No-op if the id is unknown.
    pub fn set_view_mode(&mut self, id: DocId, mode: ViewMode) {
        if let Some(t) = self.tab_mut(id) {
            t.view_mode = mode;
        }
    }

    /// Flip the active tab's view mode (the dual toggle click). Returns the new
    /// mode, or `None` when no tab is open.
    pub fn toggle_active_view_mode(&mut self) -> Option<ViewMode> {
        let t = self.active_tab_mut()?;
        t.view_mode = t.view_mode.toggled();
        Some(t.view_mode)
    }

    // ── Theme handle ──

    /// The active theme's display name (the theme handle).
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Record the active theme's display name (called when a theme is applied /
    /// switched; themes.md §4.4 `setCurrent`). The actual colors are pushed to
    /// gpui-component by [`crate::ui::theme_apply`].
    pub fn set_theme_name(&mut self, name: impl Into<String>) {
        self.theme_name = name.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_documents_get_unique_increasing_ids() {
        let mut s = AppState::new();
        let a = s.open_document("A");
        let b = s.open_document("B");
        let c = s.open_document("C");
        assert_ne!(a, b);
        assert_ne!(b, c);
        // Monotonic / never reused.
        assert!(a.get() < b.get() && b.get() < c.get());
        assert_eq!(s.tab_count(), 3);
        // Tab-strip order is insertion order.
        let titles: Vec<&str> = s.tabs().iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["A", "B", "C"]);
    }

    #[test]
    fn opening_makes_active() {
        let mut s = AppState::new();
        assert!(s.is_empty());
        assert_eq!(s.active_id(), None);
        let a = s.open_document("A");
        assert_eq!(s.active_id(), Some(a));
        let b = s.open_document("B");
        // Newest open becomes active (createTab sets m_activeDocDock = dock).
        assert_eq!(s.active_id(), Some(b));
    }

    #[test]
    fn activate_unknown_is_noop() {
        let mut s = AppState::new();
        let a = s.open_document("A");
        // Fabricate an id that was never allocated.
        let bogus = DocId(NonZeroU64::new(9999).unwrap());
        s.activate(bogus);
        assert_eq!(s.active_id(), Some(a));
    }

    #[test]
    fn closing_active_reassigns_to_last() {
        // C++ dock.destroyed: reassign m_activeDocDock to last (app-shell §8).
        let mut s = AppState::new();
        let a = s.open_document("A");
        let b = s.open_document("B");
        let c = s.open_document("C");
        s.activate(b);
        assert_eq!(s.active_id(), Some(b));

        // Close the active middle tab → active jumps to the last remaining (C).
        assert!(s.close_document(b));
        assert_eq!(s.active_id(), Some(c));
        assert_eq!(s.tab_count(), 2);

        // Closing a non-active tab leaves active untouched.
        s.activate(c);
        assert!(s.close_document(a));
        assert_eq!(s.active_id(), Some(c));

        // Closing the final tab → empty, no active.
        assert!(s.close_document(c));
        assert!(s.is_empty());
        assert_eq!(s.active_id(), None);
    }

    #[test]
    fn close_unknown_returns_false() {
        let mut s = AppState::new();
        let a = s.open_document("A");
        let bogus = DocId(NonZeroU64::new(424242).unwrap());
        assert!(!s.close_document(bogus));
        assert_eq!(s.tab_count(), 1);
        assert_eq!(s.active_id(), Some(a));
    }

    #[test]
    fn source_tracks_per_document_and_active() {
        let mut s = AppState::new();
        let a = s.open_document("A");
        let b = s.open_document("B");
        // Default is "no source", not live.
        assert_eq!(s.active_source(), DataSource::none());
        assert_eq!(s.active_source().kind, SourceKind::None);
        assert!(!s.active_source().live);

        s.set_source(a, DataSource::new(SourceKind::File, "/tmp/x.bin"));
        s.set_source(b, DataSource::new(SourceKind::Buffer, "buf"));

        // active == b
        assert_eq!(s.active_source().kind, SourceKind::Buffer);
        assert!(s.active_source().live);
        assert_eq!(s.active_source().target, "buf");

        s.activate(a);
        assert_eq!(s.active_source().kind, SourceKind::File);
        assert_eq!(s.tab(a).unwrap().source.target, "/tmp/x.bin");
    }

    #[test]
    fn selection_and_title_update() {
        let mut s = AppState::new();
        let a = s.open_document("Old");
        assert!(!s.tab(a).unwrap().selection.is_active());

        s.set_selection(
            a,
            Selection {
                count: 3,
                primary: None,
            },
        );
        assert!(s.tab(a).unwrap().selection.is_active());
        assert_eq!(s.tab(a).unwrap().selection.count, 3);

        s.set_title(a, "New");
        assert_eq!(s.tab(a).unwrap().title, "New");

        // Mutating the active tab directly.
        s.active_tab_mut().unwrap().view_root = Some(42);
        assert_eq!(s.active_tab().unwrap().view_root, Some(42));
    }

    #[test]
    fn theme_handle_roundtrips() {
        let mut s = AppState::new();
        assert_eq!(s.theme_name(), "");
        s.set_theme_name("VS2022 Dark");
        assert_eq!(s.theme_name(), "VS2022 Dark");
    }

    #[test]
    fn source_kind_labels() {
        assert_eq!(SourceKind::None.label(), "No source");
        assert_eq!(SourceKind::File.label(), "File");
        assert_eq!(SourceKind::Process.label(), "Process");
    }

    #[test]
    fn view_mode_defaults_to_tree_and_toggles() {
        // The C++ default per-pane mode is VM_Reclass (the structured grid).
        assert_eq!(ViewMode::default(), ViewMode::Tree);
        // The 3-way cycle mirrors the C++ SplitPane view index 0→1→2→0
        // (VM_Reclass → VM_Rendered → VM_Debug → VM_Reclass).
        assert_eq!(ViewMode::Tree.toggled(), ViewMode::Rendered);
        assert_eq!(ViewMode::Rendered.toggled(), ViewMode::Debug);
        assert_eq!(ViewMode::Debug.toggled(), ViewMode::Tree);
        assert_eq!(ViewMode::Tree.label(), "Tree");
        assert_eq!(ViewMode::Rendered.label(), "C/C++");
        assert_eq!(ViewMode::Debug.label(), "Debug");
    }

    #[test]
    fn view_mode_is_per_tab_and_follows_active() {
        let mut s = AppState::new();
        // No tabs → the default mode.
        assert_eq!(s.active_view_mode(), ViewMode::Tree);
        assert_eq!(s.toggle_active_view_mode(), None);

        let a = s.open_document("A");
        let b = s.open_document("B");
        // Toggle the active tab (b) only.
        assert_eq!(s.toggle_active_view_mode(), Some(ViewMode::Rendered));
        assert_eq!(s.active_view_mode(), ViewMode::Rendered);
        assert_eq!(s.tab(b).unwrap().view_mode, ViewMode::Rendered);
        // a is untouched.
        assert_eq!(s.tab(a).unwrap().view_mode, ViewMode::Tree);

        // Switching back to a shows a's (still Tree) mode.
        s.activate(a);
        assert_eq!(s.active_view_mode(), ViewMode::Tree);

        // Explicit set on a specific tab.
        s.set_view_mode(a, ViewMode::Rendered);
        assert_eq!(s.tab(a).unwrap().view_mode, ViewMode::Rendered);
    }
}
