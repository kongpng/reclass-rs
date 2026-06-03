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
//! **Styling (Zed tab bar; `_design/zed_ui_spec.md` §5.6).** The strip is drawn
//! directly as a flat `chrome_bg` bar with a 1px bottom `border`. Each tab is a
//! bespoke row — a real-SVG **source icon** (`design::icon`, full opacity = live,
//! dimmed = disconnected), a middle-elided title, and a trailing slot that holds
//! the **modified dot** at rest and a reveal-on-hover **close ✕** (an
//! [`IconName::Close`] SVG). The **active** tab lifts to `tab_active`
//! (backgroundAlt) with full-contrast text and carries a 2px `accent` top edge;
//! inactive tabs are `text_muted` and lighten by a hover overlay. A trailing
//! SVG **"+"** affordance opens a new document. The dual tree/rendered view-mode
//! toggle is a Zed **segmented control** anchored at the bottom of the body
//! ("Reclass" | "Code", each with its own glyph) — the selected segment lifts out
//! of a recessed track, as in the C++ bottom view tabs.
//!
//! In **rendered** mode the body shows the real generated source (the
//! [`generator::render_code_scoped`](crate::generator::render_code_scoped)
//! codegen, honoring the persisted format/scope) as a scrollable, read-only Zed
//! code editor with a muted line-number gutter and One Dark syntax highlighting —
//! reclass PIC3's right pane (see [`DocumentArea::render_code_view`]).
//!
//! Gated behind the `ui` feature.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{Panel, PanelControl, PanelEvent, TitleStyle};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, Sizable as _};

use super::design::{color, icon, tokens};
use super::editor::RcxEditor;
use super::state::{DataSource, DocId, SourceKind, ViewMode};
use crate::generator::{self, code_format_name, code_scope_name, CodeFormat, CodeScope};

// ── Document-tab context-menu actions (the C++ doc-tab `QMenu`, `main.cpp:3652`)
//
// The Zed-style right-click menu (gpui-component `ContextMenuExt` + `PopupMenu`)
// dispatches a gpui `Action` per item; these are the document tab's commands —
// Close / Close All Tabs / Close All But This / Copy Full Path / Open Containing
// Folder (`main.cpp:3654-3689`). The area handles them on its tracked-focus root
// (`on_action`), reading the right-clicked tab from `context_target`. Every one
// is a live wire: Close / Close-others mutate the tab list, Copy/Reveal act on
// the document's `.rcx` path (read off the editor's controller).
actions!(
    rcx_doc_tabs,
    [
        TabClose,
        TabCloseAll,
        TabCloseOthers,
        TabCopyPath,
        TabRevealPath
    ]
);

/// The drag payload + preview for a document-tab reorder (mirrors the C++
/// movable `QTabBar`; `main.cpp` tab drag detection). Carries the dragged tab's
/// id so the drop target can resolve the *current* source index (indices shift
/// as the strip mutates, so resolving by stable [`DocId`] at drop time is
/// reorder-safe — the C++ `moveTab` semantics). It is its own [`Render`] preview
/// (a small floating tab chip), the way gpui-component's `DragPanel` is.
#[derive(Clone)]
struct TabDrag {
    id: DocId,
    title: SharedString,
}

impl Render for TabDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A compact floating chip that follows the cursor while dragging — the
        // elevated tab surface at reduced opacity (gpui-component `DragPanel`).
        div()
            .id("rcx-tab-drag")
            .cursor_grabbing()
            .flex()
            .items_center()
            .px(px(tokens::space::LG))
            .h(px(TAB_STRIP_H))
            .max_w(px(TAB_MAX_W))
            .overflow_hidden()
            .whitespace_nowrap()
            .border_1()
            .border_color(color::border(cx))
            .rounded(px(tokens::radius::MD))
            .bg(color::elevated_bg(cx))
            .text_color(color::text(cx))
            .text_size(px(tokens::font::UI_SM))
            .opacity(0.75)
            .child(self.title.clone())
    }
}

/// Tab-strip height (logical px). The C++ dock tab bar was a fixed 37px
/// (`MenuBarStyle::sizeFromContents` `CT_TabBarTab`, app-shell §3); Zed runs a
/// touch shorter for a tighter chrome.
const TAB_STRIP_H: f32 = 36.0;
/// Maximum tab width before the title middle-elides (Zed caps tab width so a long
/// struct name never crowds the strip).
const TAB_MAX_W: f32 = 220.0;
/// Bottom view-mode toggle bar height.
const VIEW_TOGGLE_H: f32 = 30.0;
/// One segment's height inside the view-mode toggle.
const SEGMENT_H: f32 = 22.0;

/// The code-format options in `enum class CodeFormat` order (generator.h:11-18) —
/// the items the C++ `fmtCombo` is filled with (main.cpp:2413-2414).
const CODE_FORMATS: [CodeFormat; 5] = [
    CodeFormat::CppHeader,
    CodeFormat::RustStruct,
    CodeFormat::DefineOffsets,
    CodeFormat::CSharpStruct,
    CodeFormat::PythonCtypes,
];
/// The code-scope options in `enum class CodeScope` order (generator.h:20-25) —
/// the `scopeCombo` items (main.cpp:2439-2440).
const CODE_SCOPES: [CodeScope; 3] = [
    CodeScope::Current,
    CodeScope::WithChildren,
    CodeScope::FullSdk,
];

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
    /// Unsaved-changes flag — drives the tab's **modified dot** (Zed shows a dot
    /// in the close slot until the tab is hovered). Pure tab-strip chrome state
    /// (the authoritative dirty bit lives on the document/undo stack); the window
    /// pushes it in via [`DocumentArea::set_modified`].
    pub modified: bool,
    pub editor: Entity<RcxEditor>,
}

impl DocEntry {
    fn new(id: DocId, title: impl Into<SharedString>, editor: Entity<RcxEditor>) -> Self {
        DocEntry {
            id,
            title: title.into(),
            source: DataSource::none(),
            view_mode: ViewMode::default(),
            modified: false,
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
    /// The code-view format/scope selector changed (the C++ `fmtCombo`/`scopeCombo`
    /// `currentIndexChanged` handlers; main.cpp:2458-2475). The window persists
    /// the new indices to the `codeFormat`/`codeScope` settings keys. Carries the
    /// raw enum indices (`CodeFormat as i32` / `CodeScope as i32`).
    CodeOptionsChanged { format_idx: i32, scope_idx: i32 },
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
    /// The tab a right-click context menu targets (the C++ menu's "tab under the
    /// cursor"; `tabBar->tabAt(pos)`). Set on right-mouse-down over a tab; read by
    /// the context-menu action handlers. `None` when no tab was right-clicked.
    context_target: Option<DocId>,
    /// The code-view output format (the C++ per-pane `fmtCombo`, synced to the
    /// `codeFormat` setting; main.cpp:2412-2415). Drives both the live rendered
    /// pane and the corner selector. App-wide (every pane shares it in the C++).
    code_format: CodeFormat,
    /// The code-view scope (the C++ `scopeCombo` / `codeScope`; main.cpp:2438-2441):
    /// just the selected struct, that struct + its deps, or the full SDK.
    code_scope: CodeScope,
    /// Whether the generator emits `static_assert` lines (the C++ `generatorAsserts`
    /// option; main.cpp:5453). Pushed in from the window's persisted setting so the
    /// live code view honors it like the export path does.
    generator_asserts: bool,
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
            context_target: None,
            // C++ QSettings defaults: codeFormat=0 (C++ header), codeScope=0
            // (Current), generatorAsserts=false. The window overrides these from
            // the persisted store right after construction.
            code_format: CodeFormat::CppHeader,
            code_scope: CodeScope::Current,
            generator_asserts: false,
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

    /// Set a tab's modified (unsaved-changes) flag — the Zed modified dot
    /// (`undoStack.indexChanged` → "document is dirty"; app-shell §8 step 9).
    pub fn set_modified(&mut self, id: DocId, modified: bool, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            if self.tabs[i].modified != modified {
                self.tabs[i].modified = modified;
                cx.notify();
            }
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

    /// Close a tab by id (the C++ doc-tab menu "Close"; `target->close()`).
    fn close_id(&mut self, id: DocId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.index_of(id) {
            self.close_index(i, window, cx);
        }
    }

    /// Replace **every** tab with a single fresh document tab and return its id +
    /// editor (the C++ `project_open` replace-all: `{ ClosingGuard guard; …
    /// closeAllDocDocks(); dock = createTab(doc); }`; main.cpp:6147-6150 /
    /// 6190-6194). Unlike [`close_all`](Self::close_all) — which leaves a generic
    /// "Untitled" tab — the caller drives a *loaded* document into the returned
    /// editor, so this never emits a `NewDocumentRequested` (the C++ `createTab`
    /// binds the just-loaded doc, it does not run `project_new`). Every prior tab
    /// emits a `Closed` so the window's per-tab bookkeeping (state map, dock
    /// refresh) unwinds exactly as it would for an explicit close.
    pub fn replace_all_with_fresh(
        &mut self,
        title: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (DocId, Entity<RcxEditor>) {
        // closeAllDocDocks(): drop the prior tabs, signalling each so the window
        // releases its per-tab state (the C++ `destroyed` cleanup).
        let ids: Vec<DocId> = self.tabs.iter().map(|t| t.id).collect();
        self.tabs.clear();
        for id in ids {
            cx.emit(DocAreaEvent::Closed(id));
        }
        // createTab(doc): one fresh tab, made active. The caller pushes the loaded
        // document into the editor (no NewDocumentRequested — this is a load, not
        // an empty `project_new`).
        self.active = 0;
        let id = self.alloc_id();
        let editor = RcxEditor::view(window, cx);
        self.tabs.push(DocEntry::new(id, title, editor.clone()));
        self.active = self.tabs.len() - 1;
        cx.notify();
        (id, editor)
    }

    /// Close **every** tab (the C++ "Close All Tabs"; `closeAllDocDocks`). The
    /// "never leave a blank area" reflex still applies — closing the final tab
    /// re-opens a fresh document — so this collapses to one fresh untitled tab.
    fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Emit a Closed for each existing doc, then reset to a single fresh tab.
        let ids: Vec<DocId> = self.tabs.iter().map(|t| t.id).collect();
        self.tabs.clear();
        for id in ids {
            cx.emit(DocAreaEvent::Closed(id));
        }
        self.active = 0;
        self.push_document("Untitled", window, cx);
        cx.emit(DocAreaEvent::NewDocumentRequested);
        cx.notify();
    }

    /// Close every open tab whose data source is of `kind`, returning how many
    /// were closed (the live-host **safe-unload** detach; design §7.A [fix]).
    ///
    /// The [`LivePluginHost`](super::pluginhost::LivePluginHost) drives this when
    /// the manager safe-unloads a provider plugin: every document still pointing
    /// at that provider's source kind is closed **before** the backing library is
    /// dropped, so none outlives the provider it reads (the C++ dangling-provider
    /// crash this fixes; cpp_reference §2/§10.3). Walks the strip in reverse so an
    /// index removal never disturbs a not-yet-visited tab, and reuses
    /// [`close_index`](Self::close_index) for the active-index fix-up + the
    /// never-leave-a-blank-area reflex.
    pub fn detach_sources_of_kind(
        &mut self,
        kind: SourceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let mut closed = 0;
        for i in (0..self.tabs.len()).rev() {
            if self.tabs[i].source.kind == kind {
                self.close_index(i, window, cx);
                closed += 1;
            }
        }
        closed
    }

    /// Close every tab **except** `keep` (the C++ "Close All But This"; the loop
    /// `for d in docks: if d != target d->close()`). Order-independent: it walks
    /// from the end so index removal never disturbs a not-yet-visited tab, and
    /// keeps `keep` active afterwards.
    fn close_others(&mut self, keep: DocId, window: &mut Window, cx: &mut Context<Self>) {
        for i in (0..self.tabs.len()).rev() {
            if self.tabs[i].id != keep {
                self.close_index(i, window, cx);
            }
        }
        // `keep` is now the sole tab — make sure it is the active one.
        if let Some(i) = self.index_of(keep) {
            self.activate_index(i, cx);
        }
    }

    /// The on-disk `.rcx` path of a tab's document, if it has been saved (the C++
    /// `tabIt->doc->filePath`). `None` for an unsaved/untitled document — the C++
    /// only shows Copy Full Path / Open Containing Folder when the path is set.
    /// Reads the editor → controller → document (a read-only borrow; no logic
    /// change).
    fn tab_file_path(&self, id: DocId, cx: &App) -> Option<std::path::PathBuf> {
        let i = self.index_of(id)?;
        self.tabs[i]
            .editor
            .read(cx)
            .controller()
            .document()
            .file_path
            .clone()
    }

    /// Reorder a tab: move the tab with id `from_id` to sit at the current index
    /// of `to_id` (the C++ movable `QTabBar` `moveTab`). Resolving both endpoints
    /// by stable [`DocId`] at drop time keeps the swap correct even though strip
    /// indices shift during a drag. The dragged tab stays active after the move.
    fn reorder(&mut self, from_id: DocId, to_id: DocId, cx: &mut Context<Self>) {
        if from_id == to_id {
            return;
        }
        let (Some(from), Some(to)) = (self.index_of(from_id), self.index_of(to_id)) else {
            return;
        };
        let entry = self.tabs.remove(from);
        // After removal, the target's index may have shifted left by one.
        let mut insert = self.index_of(to_id).map_or(to, |t| t);
        if from < insert {
            insert += 1;
        }
        let insert = insert.min(self.tabs.len());
        self.tabs.insert(insert, entry);
        // Keep the dragged tab active (it was the gesture's subject).
        if let Some(i) = self.index_of(from_id) {
            self.active = i;
            cx.emit(DocAreaEvent::Activated(from_id));
        }
        cx.notify();
    }

    // ── Context-menu action handlers ─────────────────────────────────────────
    //
    // Dispatched by the `PopupMenu` built in `tab_context_menu`; the targeted tab
    // was recorded in `context_target` on right-mouse-down. Every item is live:
    // Close / Close-others mutate the strip, Copy/Reveal act on the document path.

    /// "Close" — close the right-clicked tab (`target->close()`).
    fn action_close(&mut self, _: &TabClose, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.context_target.take() {
            self.close_id(id, window, cx);
        }
    }

    /// "Close All Tabs" — `closeAllDocDocks`.
    fn action_close_all(&mut self, _: &TabCloseAll, window: &mut Window, cx: &mut Context<Self>) {
        self.context_target = None;
        self.close_all(window, cx);
    }

    /// "Close All But This" — close every other tab (`d != target`).
    fn action_close_others(
        &mut self,
        _: &TabCloseOthers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.context_target.take() {
            self.close_others(id, window, cx);
        }
    }

    /// "Copy Full Path" — copy the document's `.rcx` path to the clipboard
    /// (`QGuiApplication::clipboard()->setText(path)`). No-op when unsaved.
    fn action_copy_path(&mut self, _: &TabCopyPath, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.context_target.take() {
            if let Some(path) = self.tab_file_path(id, cx) {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    path.to_string_lossy().to_string(),
                ));
            }
        }
    }

    /// "Open Containing Folder" — reveal the document's `.rcx` file in the OS file
    /// manager (`QDesktopServices::openUrl(absolutePath)`; gpui `reveal_path`
    /// opens the folder with the file selected). No-op when unsaved.
    fn action_reveal_path(
        &mut self,
        _: &TabRevealPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.context_target.take() {
            if let Some(path) = self.tab_file_path(id, cx) {
                cx.reveal_path(&path);
            }
        }
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

    /// Push the persisted code-generator options into the area (the window's
    /// equivalent of seeding `fmtCombo`/`scopeCombo` from QSettings + reading the
    /// `generatorAsserts` value; main.cpp:2415/2441/5453). Called by the window on
    /// startup and whenever the Generator options dialog changes the assert flag.
    /// Re-renders so the live code view reflects the new options immediately.
    pub fn set_generator_options(
        &mut self,
        format: CodeFormat,
        scope: CodeScope,
        emit_asserts: bool,
        cx: &mut Context<Self>,
    ) {
        self.code_format = format;
        self.code_scope = scope;
        self.generator_asserts = emit_asserts;
        cx.notify();
    }

    /// Update just the assert flag (the C++ Generator options-dialog round-trip
    /// only touches `generatorAsserts`; main.cpp:5061-5062). Re-renders the live
    /// code view so a toggled assert option is reflected without a tab switch.
    pub fn set_generator_asserts(&mut self, emit_asserts: bool, cx: &mut Context<Self>) {
        if self.generator_asserts != emit_asserts {
            self.generator_asserts = emit_asserts;
            cx.notify();
        }
    }

    /// Select the code-view format (the C++ `fmtCombo::currentIndexChanged`;
    /// main.cpp:2458-2466): update local state, re-render every rendered pane, and
    /// emit [`DocAreaEvent::CodeOptionsChanged`] so the window persists `codeFormat`.
    fn set_code_format(&mut self, format: CodeFormat, cx: &mut Context<Self>) {
        if self.code_format != format {
            self.code_format = format;
            self.emit_code_options(cx);
            cx.notify();
        }
    }

    /// Select the code-view scope (the C++ `scopeCombo::currentIndexChanged`;
    /// main.cpp:2467-2475).
    fn set_code_scope(&mut self, scope: CodeScope, cx: &mut Context<Self>) {
        if self.code_scope != scope {
            self.code_scope = scope;
            self.emit_code_options(cx);
            cx.notify();
        }
    }

    fn emit_code_options(&mut self, cx: &mut Context<Self>) {
        cx.emit(DocAreaEvent::CodeOptionsChanged {
            format_idx: self.code_format as i32,
            scope_idx: self.code_scope as i32,
        });
    }

    /// Build the Zed tab bar: a flat `chrome_bg` strip with a 1px bottom border,
    /// one bespoke tab per document, then a trailing "+" new-document affordance.
    ///
    /// Each tab carries a source icon, a middle-elided title, an active 2px accent
    /// top edge + `tab_active` lift, and a trailing slot that holds the modified
    /// dot at rest / a reveal-on-hover close ✕ (spec §5.6).
    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active;

        gpui_component::h_flex()
            .id("rcx-doc-tabs")
            .w_full()
            .flex_none()
            .h(px(TAB_STRIP_H))
            .items_stretch()
            .bg(color::chrome_bg(cx))
            .border_b_1()
            .border_color(color::border(cx))
            // Scroll the tabs horizontally when they overflow; the "+" stays put.
            .child(
                gpui_component::h_flex()
                    .id("rcx-doc-tabs-scroll")
                    .flex_1()
                    .min_w_0()
                    .items_stretch()
                    .overflow_x_scroll()
                    .children(
                        self.tabs
                            .iter()
                            .enumerate()
                            .map(|(ix, entry)| self.render_tab(ix, entry, ix == active, cx)),
                    ),
            )
            // The trailing "+" new-document affordance (app-shell §8 "+" sentinel).
            .child(self.render_new_tab_button(cx))
    }

    /// One bespoke Zed document tab.
    fn render_tab(
        &self,
        ix: usize,
        entry: &DocEntry,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = entry.id;
        let group_name = SharedString::from(format!("rcx-tab-{}", id.get()));
        // Per-tab source-status icon — a real SVG (full opacity = live, dimmed =
        // stale/none; the C++ `drawTabSourceIcon` ×0.35 live opacity). Wrapped in
        // a hoverable element carrying the source's tooltip ("File: x.bin" /
        // "No source"), the way the C++ tab provided a per-tab source tooltip.
        let source_tooltip = SharedString::from(source_status_label(&entry.source));
        let source_badge = div()
            .id(SharedString::from(format!("rcx-tab-src-{}", id.get())))
            .flex_none()
            .flex()
            .items_center()
            .child(source_icon(entry.source.kind, entry.source.live, cx))
            .tooltip(move |window, cx| Tooltip::new(source_tooltip.clone()).build(window, cx));
        // The drag payload carries the tab's stable id + title for the preview.
        let drag_title = entry.title.clone();
        // Context-menu gating: Copy/Reveal only when the doc is saved (a non-empty
        // `.rcx` path; the C++ `!doc->filePath.isEmpty()`), and "Close All But
        // This" only when more than one tab is open (`m_docDocks.size() > 1`).
        let has_path = self.tab_file_path(id, cx).is_some();
        let multi_tab = self.tabs.len() > 1;

        // Trailing slot: the modified dot at rest, the close ✕ on hover. Both
        // occupy the same fixed-width slot so the title never shifts.
        let dot = div()
            .absolute()
            .size(px(6.0))
            .rounded(px(tokens::radius::FULL))
            .bg(if selected {
                color::text(cx)
            } else {
                color::text_muted(cx)
            })
            .when(!entry.modified, |d| d.invisible())
            .group_hover(group_name.clone(), |d| d.invisible());

        let close = div()
            .id(SharedString::from(format!("rcx-tab-close-{}", id.get())))
            .absolute()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .rounded(px(tokens::radius::SM))
            .text_color(color::text_muted(cx))
            .invisible()
            .group_hover(group_name.clone(), |d| d.visible())
            .hover(|d| d.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .child(icon::close().xsmall())
            // Swallow the press so closing a tab doesn't also activate it (the
            // close click must not bubble to the tab's `on_click`).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _e, window, cx| {
                if let Some(i) = this.index_of(id) {
                    this.close_index(i, window, cx);
                }
            }));

        let trailing = div()
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .child(dot)
            .child(close);

        gpui_component::h_flex()
            .id(SharedString::from(format!("rcx-tab-{}", id.get())))
            .group(group_name)
            .relative()
            .flex_none()
            .h_full()
            .max_w(px(TAB_MAX_W))
            .items_center()
            .gap(px(tokens::space::SM))
            .px(px(tokens::space::LG))
            .border_r_1()
            .border_color(color::border(cx))
            .text_size(px(tokens::font::UI_SM))
            // Selected: lift to the elevated tab bg + full-contrast text so the
            // active tab reads as a connected surface (Zed active-tab); inactive:
            // muted text that lightens by a hover overlay only (no border change —
            // spec §5.6/§7), so the strip stays calm until pointed at.
            .map(|t| {
                if selected {
                    t.bg(color::elevated_bg(cx))
                        .text_color(color::text(cx))
                        .font_weight(FontWeight::MEDIUM)
                } else {
                    t.text_color(color::text_muted(cx))
                        .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                }
            })
            // The 2px accent top edge marks the active tab (spec §5.6; the C++
            // active-tab accent rail). Drawn as an overlaid bar so it sits flush
            // on the tab's top regardless of padding.
            .when(selected, |t| {
                t.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(tokens::border::THICK))
                        .bg(color::accent(cx)),
                )
            })
            .child(source_badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .whitespace_nowrap()
                    .child(entry.title.clone()),
            )
            .child(trailing)
            .on_click(cx.listener(move |this, _e, _window, cx| {
                this.activate_index(ix, cx);
            }))
            // Middle-click closes the tab (the C++ event filter
            // `me->button() == Qt::MiddleButton` → `d->close()`; main.cpp:3855).
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _e, window, cx| {
                    this.close_id(id, window, cx);
                }),
            )
            // Record the right-clicked tab so the context-menu action handlers
            // know which tab fired (before the menu opens; the C++ `tabAt(pos)`).
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _e, _window, cx| {
                    this.context_target = Some(id);
                    cx.notify();
                }),
            )
            // Drag-to-reorder within the strip (the C++ movable `QTabBar`). The
            // payload carries the dragged tab's id; the drop target resolves the
            // swap by stable id so the reorder is index-shift-safe.
            .on_drag(
                TabDrag {
                    id,
                    title: drag_title,
                },
                |drag, _pos, _window, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                },
            )
            // Drop-target feedback + the actual reorder when another tab is
            // dropped onto this one (`moveTab(from, to)`).
            .drag_over::<TabDrag>(|style, _drag, _window, cx| {
                style.border_l_2().border_color(color::accent(cx))
            })
            .on_drop(cx.listener(move |this, drag: &TabDrag, _window, cx| {
                this.reorder(drag.id, id, cx);
            }))
            // The Zed right-click context menu — Close / Close All Tabs / Close
            // All But This / Copy Full Path / Open Containing Folder (the C++
            // doc-tab `QMenu`; main.cpp:3652). The last two only appear when the
            // document has been saved (a non-empty `.rcx` path), mirroring the C++
            // `!doc->filePath.isEmpty()` guard.
            .context_menu(move |menu, _window, _cx| tab_context_menu(menu, has_path, multi_tab))
    }

    /// The trailing "+" affordance — a ghost icon button (real SVG) that opens a
    /// new document (the C++ sentinel "+" click; app-shell §8).
    fn render_new_tab_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("rcx-new-tab")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(TAB_STRIP_H))
            .h_full()
            .text_color(color::text_muted(cx))
            .hover(|d| d.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
            .child(icon::plus().small())
            .on_click(cx.listener(|this, _e, window, cx| {
                this.on_new_tab(window, cx);
            }))
    }

    /// The dual view-mode toggle, styled as a Zed **segmented control** —
    /// "Reclass" (tree) | "Code" (rendered C/C++), each with a real SVG glyph.
    /// Matches the C++ bottom view tabs (PIC5/PIC2; `reclass_view_click_active`):
    /// the track is a recessed pill and the **selected segment lifts** out of it
    /// to the elevated surface for a clear, high-contrast active state.
    fn render_view_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view_mode = self.active_entry().map(|e| e.view_mode).unwrap_or_default();
        let has_doc = !self.tabs.is_empty();

        // One segment of the control. Every segment ALWAYS renders its glyph +
        // label (a generous min width + `flex_none` so the text never collapses).
        // The selected segment LIFTS out of the recessed track to the elevated
        // surface (a 1px border + soft shadow + full-contrast text), the way a Zed
        // segmented control reads; inactive segments are `text_muted` over the
        // track and brighten with a hover overlay. The active segment ignores
        // clicks (it is already shown); only the inactive one toggles.
        let segment =
            |label: &'static str, glyph: Icon, this_mode: ViewMode, cx: &mut Context<Self>| {
                let selected = view_mode == this_mode;
                gpui_component::h_flex()
                    .id(label)
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .gap(px(tokens::space::XS))
                    .h(px(SEGMENT_H))
                    .min_w(px(72.0))
                    .px(px(tokens::space::LG))
                    .rounded(px(tokens::radius::SM))
                    .text_size(px(tokens::font::UI_SM))
                    .child(glyph.xsmall())
                    .child(label)
                    .map(|s| {
                        if selected {
                            // Lift to the elevated surface — a bordered, shadowed
                            // pill with full-contrast text (the active segment).
                            s.bg(color::elevated_bg(cx))
                                .border_1()
                                .border_color(color::border(cx))
                                .shadow_sm()
                                .text_color(color::text(cx))
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            // Transparent over the track; brighten on hover.
                            s.text_color(color::text_muted(cx)).hover(|h| {
                                h.bg(color::hover_overlay(cx)).text_color(color::text(cx))
                            })
                        }
                    })
                    .when(has_doc && !selected, |s| {
                        s.on_click(cx.listener(|this, _e, _window, cx| {
                            this.toggle_view_mode(cx);
                        }))
                    })
            };

        // The strip carries an explicit bg + top border so it never reads as
        // overlapped by the scanner dock above; the segmented control is a
        // recessed track (`chrome_bg`, 1px border) the active segment lifts out of.
        gpui_component::h_flex()
            .id("rcx-view-toggle")
            .flex_none()
            .h(px(VIEW_TOGGLE_H))
            .w_full()
            .items_center()
            .justify_between()
            .px(px(tokens::space::LG))
            .bg(color::chrome_bg(cx))
            .border_t_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(tokens::space::XXS))
                    .p(px(tokens::space::XXS))
                    .rounded(px(tokens::radius::MD))
                    .border_1()
                    .border_color(color::border(cx))
                    .bg(color::content_bg(cx))
                    .child(segment("Reclass", icon::struct_(), ViewMode::Tree, cx))
                    .child(segment("Code", icon::function(), ViewMode::Rendered, cx)),
            )
            // The C++ corner widget (`fmtCombo` + `scopeCombo`) is hidden until the
            // Code tab is selected (main.cpp:2449). Mirror that: the format/scope
            // selectors only appear in rendered mode.
            .when(has_doc && view_mode == ViewMode::Rendered, |s| {
                s.child(self.render_code_selectors(cx))
            })
    }

    /// The rendered code-view corner: the **format** + **scope** selectors (the
    /// C++ `fmtCombo` / `scopeCombo`; main.cpp:2412-2446). Two compact dropdown
    /// buttons styled like the toggle track; each lists every enum option and a
    /// pick re-renders the live view + persists the choice through the window
    /// (`DocAreaEvent::CodeOptionsChanged`).
    fn render_code_selectors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let fmt = self.code_format;
        let scope = self.code_scope;
        gpui_component::h_flex()
            .flex_none()
            .items_center()
            .gap(px(tokens::space::XS))
            .child(self.code_format_selector(fmt, cx))
            .child(self.code_scope_selector(scope, cx))
    }

    /// The `fmtCombo` dropdown — the current format name + a chevron, opening a
    /// menu of every [`CodeFormat`].
    fn code_format_selector(
        &self,
        current: CodeFormat,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use gpui_component::button::Button;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        use gpui_component::Sizable as _;

        let this = cx.entity();
        Button::new("rcx-code-fmt")
            .small()
            .outline()
            .label(format!("{}  \u{25be}", code_format_name(current)))
            .dropdown_menu(move |mut menu, _window, _cx| {
                for &fmt in CODE_FORMATS.iter() {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(code_format_name(fmt))
                            .checked(fmt == current)
                            .on_click(move |_e, _window, app| {
                                this.update(app, |area, cx| area.set_code_format(fmt, cx));
                            }),
                    );
                }
                menu
            })
    }

    /// The `scopeCombo` dropdown — the current scope name + a chevron, opening a
    /// menu of every [`CodeScope`].
    fn code_scope_selector(&self, current: CodeScope, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::button::Button;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        use gpui_component::Sizable as _;

        let this = cx.entity();
        Button::new("rcx-code-scope")
            .small()
            .outline()
            .label(format!("{}  \u{25be}", code_scope_name(current)))
            .dropdown_menu(move |mut menu, _window, _cx| {
                for &scope in CODE_SCOPES.iter() {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(code_scope_name(scope))
                            .checked(scope == current)
                            .on_click(move |_e, _window, app| {
                                this.update(app, |area, cx| area.set_code_scope(scope, cx));
                            }),
                    );
                }
                menu
            })
    }

    /// The body for the active tab: the editor (tree mode) or the rendered C/C++
    /// code view (rendered mode).
    ///
    /// The rendered side wires the fully-implemented codegen
    /// ([`generator::render_code_scoped`](crate::generator::render_code_scoped))
    /// and presents it like reclass PIC3's right pane — a scrollable, read-only Zed
    /// code editor with a muted line-number gutter and One Dark syntax
    /// highlighting (see [`Self::render_code_view`]).
    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let Some(entry) = self.active_entry() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color::text_muted(cx))
                .child("No document")
                .into_any_element();
        };

        match entry.view_mode {
            ViewMode::Tree => entry.editor.clone().into_any_element(),
            ViewMode::Rendered => self.render_code_view(entry, cx),
        }
    }

    /// The rendered C/C++ pane (reclass PIC3, right side) — a scrollable,
    /// read-only Zed code editor.
    ///
    /// Wires the real generator: it reads the active tab's editor → controller →
    /// tree + view root and calls
    /// [`render_code_scoped`](crate::generator::render_code_scoped) with the
    /// persisted [`CodeFormat`]/[`CodeScope`] (the C++ `fmtCombo`/`scopeCombo`),
    /// the document's [`TypeAliases`](crate::generator::TypeAliases) (the per-kind
    /// display-name overrides), and the persisted `generatorAsserts` flag — the
    /// same dispatch the C++ live view uses (main.cpp:5469-5477). The output is
    /// split into lines and rendered with a muted, right-aligned line-number gutter
    /// plus per-line One Dark syntax highlighting (keywords magenta, types yellow,
    /// numbers orange, strings green, trailing `// 0x..` comments dim green-gray).
    ///
    /// When there is no struct root (a fresh/empty document) the generator
    /// returns an empty string; we show a centered muted placeholder instead.
    fn render_code_view(&self, entry: &DocEntry, cx: &Context<Self>) -> AnyElement {
        let ed = entry.editor.read(cx);
        let tree = ed.controller().tree();
        let root = ed.controller().view_root_id();
        // The document's per-kind name overrides feed the renderer's type names.
        let aliases = &ed.controller().document().type_aliases;
        let aliases = if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        };
        let source = generator::render_code_scoped(
            self.code_format,
            self.code_scope,
            tree,
            root,
            aliases,
            self.generator_asserts,
        );

        // Empty (no struct root / non-struct view) → graceful placeholder.
        if source.trim().is_empty() {
            return div()
                .id("rcx-code-view-empty")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// nothing to render — open or build a struct")
                .into_any_element();
        }

        // Gutter width grows with the line count so the digits stay right-aligned
        // and the source column never shifts (Zed gutter behaviour).
        let line_count = source.lines().count().max(1);
        let digits = ((line_count as f32).log10().floor() as usize) + 1;
        let gutter_w = px(digits as f32 * 8.5 + 24.0);
        let line_h = px(tokens::font::EDITOR_SIZE * tokens::font::EDITOR_LINE_HEIGHT);

        let gutter_fg = color::syntax_address(cx);

        let rows: Vec<AnyElement> = source
            .lines()
            .enumerate()
            .map(|(i, line)| self.render_code_line(i + 1, line, gutter_w, line_h, gutter_fg, cx))
            .collect();

        gpui_component::v_flex()
            .id("rcx-code-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(tokens::font::EDITOR_SIZE))
            .py(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// One rendered source line: the muted right-aligned line-number gutter +
    /// the syntax-highlighted source spans (a per-line tokenizer pass).
    fn render_code_line(
        &self,
        number: usize,
        line: &str,
        gutter_w: Pixels,
        line_h: Pixels,
        gutter_fg: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let spans = highlight_cpp_line(line, cx);

        gpui_component::h_flex()
            .w_full()
            .flex_none()
            .h(line_h)
            .items_center()
            .child(
                // Line-number gutter: muted, right-aligned, fixed width.
                div()
                    .flex_none()
                    .w(gutter_w)
                    .pr(px(tokens::space::LG))
                    .text_color(gutter_fg)
                    .child(div().w_full().text_right().child(number.to_string())),
            )
            .child(
                // Source column. The highlight spans must sit INLINE on one row,
                // so the column is itself a horizontal flex (a bare `div()`
                // defaults to block/column layout and stacks each `.flex_none()`
                // span vertically). A trailing space keeps a blank line from
                // collapsing to zero height; the per-span runs preserve leading
                // indentation as plain whitespace spans.
                gpui_component::h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .pr(px(tokens::space::LG))
                    .whitespace_nowrap()
                    .children(if spans.is_empty() {
                        vec![div().child(" ").into_any_element()]
                    } else {
                        spans
                    }),
            )
            .into_any_element()
    }
}

/// Coarse C/C++ token kind for the per-line highlighter.
#[derive(Copy, Clone, PartialEq, Eq)]
enum CodeTok {
    /// `struct`/`class`/`void`/`const`/`unsigned`/`#pragma`/`#include`/… — purple.
    Keyword,
    /// `uint32_t`/`int64_t`/`float`/PascalCase user types — yellow.
    Type,
    /// numeric / hex literals — orange.
    Number,
    /// `// …` trailing offset comments — dim green-gray.
    Comment,
    /// `"…"` / `<…>` string-ish runs — green.
    String,
    /// everything else (identifiers, punctuation, whitespace) — content text.
    Plain,
}

/// C/C++ keywords (highlighted purple) the generator emits.
const CPP_KEYWORDS: &[&str] = &[
    "struct",
    "class",
    "enum",
    "union",
    "void",
    "const",
    "unsigned",
    "signed",
    "static",
    "inline",
    "namespace",
    "public",
    "private",
    "protected",
    "typedef",
    "using",
    "template",
    "char",
    "bool",
    "short",
    "int",
    "long",
    "double",
    "wchar_t",
    "sizeof",
];

/// Builtin scalar type names (highlighted yellow). User struct names are caught
/// by the PascalCase / `_t`-suffix heuristic in [`classify_word`].
const CPP_TYPES: &[&str] = &[
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "__int128",
    "_Float16",
    "float",
    "size_t",
    "intptr_t",
    "uintptr_t",
];

/// Classify a single identifier-ish word for the highlighter.
fn classify_word(word: &str) -> CodeTok {
    if CPP_KEYWORDS.contains(&word) {
        return CodeTok::Keyword;
    }
    if CPP_TYPES.contains(&word) {
        return CodeTok::Type;
    }
    // Numeric / hex literal (e.g. `0x70`, `16`, `4ull`).
    let bytes = word.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_digit) {
        return CodeTok::Number;
    }
    // Heuristic "looks like a user type": leading uppercase letter or a `_t`
    // suffix (struct/class names + `*_t` aliases the user emits) → yellow.
    let first = word.chars().next();
    if first.is_some_and(|c| c.is_ascii_uppercase()) || word.ends_with("_t") {
        return CodeTok::Type;
    }
    CodeTok::Plain
}

/// Map a token kind to its One Dark syntax color (all via design tokens — no
/// ad-hoc hex). Keyword purple, type yellow, number orange, string green,
/// comment dim green-gray.
fn tok_color(tok: CodeTok, cx: &gpui::App) -> Hsla {
    match tok {
        CodeTok::Keyword => color::syntax_keyword(cx),
        CodeTok::Type => color::syntax_type(cx),
        CodeTok::Number => color::syntax_number(cx),
        CodeTok::Comment => color::syntax_comment(cx),
        CodeTok::String => color::syntax_string(cx),
        CodeTok::Plain => color::text(cx),
    }
}

/// A simple per-line C/C++ tokenizer → colored spans (One Dark).
///
/// Splits a line into word / number / string / comment / punctuation runs and
/// classifies each (keyword/type/number/string/comment) so the rendered pane
/// reads like a Zed code editor. Whitespace is preserved as plain spans so
/// indentation and column alignment survive.
fn highlight_cpp_line(line: &str, cx: &gpui::App) -> Vec<AnyElement> {
    let mut spans: Vec<AnyElement> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;

    let push = |spans: &mut Vec<AnyElement>, text: String, tok: CodeTok| {
        if text.is_empty() {
            return;
        }
        spans.push(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(tok_color(tok, cx))
                .child(text)
                .into_any_element(),
        );
    };

    while i < n {
        let c = chars[i];

        // Trailing `// …` comment — everything to end of line (the offset notes).
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            let rest: String = chars[i..].iter().collect();
            push(&mut spans, rest, CodeTok::Comment);
            break;
        }

        // `#pragma` / `#include` preprocessor line → keyword purple to first ws.
        if c == '#' && (i == 0 || chars[..i].iter().all(|c| c.is_whitespace())) {
            let mut j = i;
            while j < n && !chars[j].is_whitespace() {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Keyword);
            i = j;
            continue;
        }

        // `"…"` string literal.
        if c == '"' {
            let mut j = i + 1;
            while j < n && chars[j] != '"' {
                j += 1;
            }
            if j < n {
                j += 1; // include closing quote
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::String);
            i = j;
            continue;
        }

        // `<…>` include path after a `#include` keyword → treat as a string.
        if c == '<' {
            let mut j = i + 1;
            while j < n && chars[j] != '>' {
                j += 1;
            }
            if j < n && chars[..i].iter().collect::<String>().contains('#') {
                j += 1;
                push(&mut spans, chars[i..j].iter().collect(), CodeTok::String);
                i = j;
                continue;
            }
        }

        // Whitespace run (preserved as plain).
        if c.is_whitespace() {
            let mut j = i;
            while j < n && chars[j].is_whitespace() {
                j += 1;
            }
            push(&mut spans, chars[i..j].iter().collect(), CodeTok::Plain);
            i = j;
            continue;
        }

        // Word / number run (identifier chars, plus a hex/number body).
        if c.is_alphanumeric() || c == '_' {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let tok = classify_word(&word);
            push(&mut spans, word, tok);
            i = j;
            continue;
        }

        // Punctuation / operator run (everything else) — plain content text.
        let mut j = i;
        while j < n
            && !chars[j].is_alphanumeric()
            && chars[j] != '_'
            && !chars[j].is_whitespace()
            && chars[j] != '"'
            && !(chars[j] == '/' && j + 1 < n && chars[j + 1] == '/')
        {
            j += 1;
        }
        if j == i {
            j += 1;
        }
        push(&mut spans, chars[i..j].iter().collect(), CodeTok::Plain);
        i = j;
    }

    spans
}

impl Panel for DocumentArea {
    fn panel_name(&self) -> &'static str {
        "DocumentArea"
    }

    /// The center document area must NOT show a dock-panel tab/title of its own —
    /// reclass (PIC1/PIC2/PIC5) and Zed both show a *single* tab row, and that row
    /// is the per-document strip rendered inside the panel body
    /// ([`render_tab_strip`](Self::render_tab_strip)), not the surrounding
    /// `DockArea` panel-tab. gpui-component's `TabPanel` always reserves a fixed
    /// title-bar strip for a single-panel center; we can't remove that strip from
    /// here, but we strip every scrap of chrome out of it so it reads as the same
    /// surface as the editor below (no "Documents" label, no active-tab underline,
    /// no zoom/menu button) — leaving the per-document strip as the only visible
    /// tab row. The bar's bg is blended to `content_bg` (see [`Self::title_style`]).
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // An empty, zero-content title — no "Documents" text, no tab underline.
        div()
    }

    /// Blend the (unavoidable, gpui-component-fixed) single-panel title strip into
    /// the document surface so it carries no visible chrome of its own — the only
    /// tab row the user sees is the per-document strip in the body.
    fn title_style(&self, cx: &App) -> Option<TitleStyle> {
        Some(TitleStyle {
            background: color::content_bg(cx),
            foreground: color::text(cx),
        })
    }

    /// No zoom/menu affordance on the center panel — it would re-introduce dock
    /// chrome on the row we are deliberately keeping chrome-less.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
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
        let view_toggle = self.render_view_toggle(cx);

        // Top → bottom: the document tab strip, the active editor/rendered body,
        // then the Zed segmented "Reclass | Code" view-mode toggle (PIC5/PIC2).
        div()
            .id("rcx-document-area")
            .track_focus(&self.focus_handle)
            .key_context("RcxDocumentArea")
            // Document-tab context-menu actions dispatched by the per-tab
            // right-click `PopupMenu` bubble to here (the menu is a child of this
            // tracked-focus subtree). Each acts on `context_target`.
            .on_action(cx.listener(Self::action_close))
            .on_action(cx.listener(Self::action_close_all))
            .on_action(cx.listener(Self::action_close_others))
            .on_action(cx.listener(Self::action_copy_path))
            .on_action(cx.listener(Self::action_reveal_path))
            .size_full()
            .flex()
            .flex_col()
            .bg(color::content_bg(cx))
            .child(strip)
            .child(div().flex_1().min_h_0().child(body))
            .child(view_toggle)
    }
}

/// Build the document-tab right-click [`PopupMenu`] (the C++ doc-tab `QMenu`,
/// `main.cpp:3652` — Close / Close All Tabs / Close All But This / Copy Full Path
/// / Open Containing Folder). Each item dispatches its tab action; the area
/// handles them. Rendered by gpui-component on the shared elevated-surface look
/// (6px radius, 1px border, soft shadow, hover overlay), so it matches the Zed
/// design system. Dismiss-on-select / escape / click-out is handled by the
/// `ContextMenuExt` machinery.
///
/// `has_path` gates the path actions (the C++ `!doc->filePath.isEmpty()`):
/// Copy Full Path / Open Containing Folder only appear for a saved document.
/// `multi_tab` gates "Close All But This" (the C++ `m_docDocks.size() > 1`).
fn tab_context_menu(menu: PopupMenu, has_path: bool, multi_tab: bool) -> PopupMenu {
    let menu = menu
        // Close — the right-clicked tab (trailing ⌘W hint, the editor accelerator).
        .menu_element_with_icon(IconName::Close, Box::new(TabClose), |_w, cx| {
            menu_row("Close", "\u{2318}W", cx)
        })
        .separator()
        // Close All Tabs.
        .menu_element_with_icon(IconName::Close, Box::new(TabCloseAll), |_w, cx| {
            menu_row("Close All Tabs", "", cx)
        });
    // Close All But This — only with more than one tab open.
    let menu = if multi_tab {
        menu.menu_element(Box::new(TabCloseOthers), |_w, cx| {
            menu_row("Close All But This", "", cx)
        })
    } else {
        menu
    };
    // Copy Full Path / Open Containing Folder — only for a saved document.
    if has_path {
        menu.separator()
            .menu_element_with_icon(IconName::Copy, Box::new(TabCopyPath), |_w, cx| {
                menu_row("Copy Full Path", "", cx)
            })
            .menu_element_with_icon(IconName::FolderOpen, Box::new(TabRevealPath), |_w, cx| {
                menu_row("Open Containing Folder", "", cx)
            })
    } else {
        menu
    }
}

/// One context-menu row body: the item `label` filling the row with a trailing
/// right-aligned dim `keys` shortcut hint (Zed's label↔accelerator layout). The
/// leading icon is supplied by `menu_element_with_icon`; this is the row's text.
fn menu_row(label: &'static str, keys: &'static str, cx: &App) -> impl IntoElement {
    gpui_component::h_flex()
        .w_full()
        .min_w(px(184.0))
        .gap(px(tokens::space::LG))
        .items_center()
        .justify_between()
        .child(div().flex_1().child(label))
        .when(!keys.is_empty(), |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::text_disabled(cx))
                    .child(keys),
            )
        })
}

/// The per-tab source-status tooltip text (the C++ per-tab source tooltip): the
/// source kind + target ("File: x.bin") or its plain label ("No source"). Pure;
/// unit-tested.
fn source_status_label(source: &DataSource) -> String {
    if source.target.is_empty() {
        source.kind.label().to_string()
    } else {
        format!("{}: {}", source.kind.label(), source.target)
    }
}

/// Map a [`SourceKind`] to its tab source-icon as a real SVG (the
/// `drawTabSourceIcon` per-tab provider badge; app-shell §8). Replaces the
/// round-2 text-glyph stand-in with a crisp [`design::icon`](icon) SVG.
///
/// Liveness mirrors `provider && provider->isValid()`: a live source paints at
/// the full tab foreground, a disconnected one dims to a muted, alpha-reduced
/// tint (the C++ ×0.40 live opacity) — the "No source" plug also reads dimmed.
fn source_icon(kind: SourceKind, live: bool, cx: &App) -> Icon {
    // Domain → SVG: a file is a document, a buffer/snapshot is memory/storage,
    // a process is the gear (matching the C++ ⚙), and "no source" reuses the
    // neutral data-store glyph rendered dimmed (the plug fallback).
    let svg = match kind {
        SourceKind::None => Icon::new(IconName::HardDrive),
        SourceKind::File => Icon::new(IconName::File),
        SourceKind::Buffer => icon::hex(), // MemoryStick — an in-memory buffer.
        SourceKind::Snapshot => icon::database(), // HardDrive — a captured store.
        SourceKind::Process => icon::settings(), // gear — a live process.
    };
    // None is never "live": always dim the plug fallback.
    let lit = live && kind != SourceKind::None;
    let tint = if lit {
        color::text(cx)
    } else {
        // Disconnected / no-source: muted, alpha-reduced (the C++ dim).
        color::text_disabled(cx)
    };
    svg.flex_none().text_color(tint).small()
}

/// A tiny extension so the area can map [`SourceKind`] → its tab source-icon
/// as a real SVG. (Kept as the public tab/icon hook for the titlebar + scanner.)
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
    use super::super::state::{DocId, SourceKind, ViewMode};

    /// A gpui-free mirror of `DocumentArea`'s tab list + active-index rules.
    struct TabModel {
        ids: Vec<DocId>,
        /// Per-tab source kind, index-parallel to `ids` — exercises the
        /// `detach_sources_of_kind` selection without a display.
        kinds: Vec<SourceKind>,
        active: usize,
        next: u64,
    }
    impl TabModel {
        fn new() -> Self {
            let mut m = TabModel {
                ids: Vec::new(),
                kinds: Vec::new(),
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
            self.kinds.push(SourceKind::None);
            self.active = self.ids.len() - 1;
            id
        }
        /// Set a tab's source kind (so a detach test can mark which tabs use a
        /// given provider kind).
        fn set_kind(&mut self, ix: usize, kind: SourceKind) {
            if ix < self.kinds.len() {
                self.kinds[ix] = kind;
            }
        }
        /// Mirror of `detach_sources_of_kind`: close every tab whose source kind
        /// matches, in reverse, reusing `close`; returns the count closed.
        fn detach_sources_of_kind(&mut self, kind: SourceKind) -> usize {
            let mut closed = 0;
            for i in (0..self.ids.len()).rev() {
                if self.kinds[i] == kind {
                    self.close(i);
                    closed += 1;
                }
            }
            closed
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
            self.kinds.remove(ix);
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
        fn index_of(&self, id: DocId) -> Option<usize> {
            self.ids.iter().position(|&x| x == id)
        }
        // Mirror of close_others: close every tab except `keep`.
        fn close_others(&mut self, keep: DocId) {
            for i in (0..self.ids.len()).rev() {
                if self.ids[i] != keep {
                    self.close(i);
                }
            }
            if let Some(i) = self.index_of(keep) {
                self.active = i;
            }
        }
        // Mirror of close_all: collapse to one fresh tab.
        fn close_all(&mut self) {
            self.ids.clear();
            self.kinds.clear();
            self.active = 0;
            self.push();
        }
        // Mirror of replace_all_with_fresh: drop every tab, allocate ONE fresh tab
        // and make it active — the same shape as `close_all` (the C++ replace-all
        // `closeAllDocDocks(); createTab(doc)`), returning the fresh id.
        fn replace_all_with_fresh(&mut self) -> DocId {
            self.ids.clear();
            self.kinds.clear();
            self.active = 0;
            self.push()
        }
        // Mirror of reorder: move `from_id` to sit at `to_id`'s position, keeping
        // the moved tab active (the same index math as `DocumentArea::reorder`).
        fn reorder(&mut self, from_id: DocId, to_id: DocId) {
            if from_id == to_id {
                return;
            }
            let (Some(from), Some(to)) = (self.index_of(from_id), self.index_of(to_id)) else {
                return;
            };
            let id = self.ids.remove(from);
            let kind = self.kinds.remove(from);
            let mut insert = self.index_of(to_id).unwrap_or(to);
            if from < insert {
                insert += 1;
            }
            let insert = insert.min(self.ids.len());
            self.ids.insert(insert, id);
            self.kinds.insert(insert, kind);
            if let Some(i) = self.index_of(from_id) {
                self.active = i;
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

    #[test]
    fn close_others_keeps_only_the_target() {
        // The C++ "Close All But This": every tab except `keep` closes.
        let mut m = TabModel::new(); // [1]
        m.push(); // [1,2]
        m.push(); // [1,2,3]
        let keep = m.ids[1];
        m.close_others(keep);
        assert_eq!(m.ids, vec![keep]);
        assert_eq!(m.active, 0);
    }

    #[test]
    fn close_all_collapses_to_one_fresh_tab() {
        // The C++ "Close All Tabs" + "never leave a blank area": all tabs close,
        // a single fresh (new-id) tab remains.
        let mut m = TabModel::new();
        m.push();
        m.push();
        let old: Vec<DocId> = m.ids.clone();
        m.close_all();
        assert_eq!(m.ids.len(), 1);
        assert_eq!(m.active, 0);
        // The surviving tab is brand-new (not any previously open id).
        assert!(!old.contains(&m.ids[0]));
    }

    #[test]
    fn replace_all_with_fresh_collapses_to_one_active_new_tab() {
        // The C++ `project_open` replace-all (`closeAllDocDocks(); createTab(doc)`,
        // main.cpp:6147-6150 / 6190-6194): every prior tab is dropped and ONE fresh
        // tab is left active to host the loaded document.
        let mut m = TabModel::new();
        m.push();
        m.push(); // [1,2,3]
        let old: Vec<DocId> = m.ids.clone();
        let fresh = m.replace_all_with_fresh();
        assert_eq!(m.ids.len(), 1, "exactly one tab remains after replace-all");
        assert_eq!(m.active, 0, "the fresh tab is active");
        assert_eq!(m.ids[0], fresh, "the returned id is the surviving tab");
        // The surviving tab is brand-new (id never reused) and source-clean.
        assert!(
            !old.contains(&fresh),
            "fresh id is not any previously open id"
        );
        assert_eq!(m.kinds[0], SourceKind::None);
    }

    #[test]
    fn reorder_moves_tab_to_target_position_and_keeps_it_active() {
        // Drag tab #1 (index 0) onto tab #3 (index 2): the strip becomes
        // [2, 3, 1] and the dragged tab stays active (the C++ movable QTabBar).
        let mut m = TabModel::new(); // [1]
        let a = m.ids[0];
        let b = m.push(); // [1,2]
        let c = m.push(); // [1,2,3]
        m.reorder(a, c);
        assert_eq!(m.ids, vec![b, c, a]);
        // The moved tab is the active one.
        assert_eq!(m.ids[m.active], a);

        // Drag it back to the front (onto b): [1, 2, 3].
        m.reorder(a, b);
        assert_eq!(m.ids, vec![a, b, c]);
        assert_eq!(m.ids[m.active], a);

        // Reordering onto itself is a no-op.
        let before = m.ids.clone();
        m.reorder(a, a);
        assert_eq!(m.ids, before);
    }

    #[test]
    fn detach_sources_of_kind_closes_only_matching_and_returns_count() {
        // The live-host safe-unload detach: close only the tabs whose source kind
        // matches, leave the rest, and report how many closed (design §7.A [fix]).
        let mut m = TabModel::new(); // [1] kind None
        m.push(); // [1,2]
        m.push(); // [1,2,3]
        m.push(); // [1,2,3,4]
                  // Mark tabs 1 and 3 as File, tab 2 as Buffer, tab 0 stays None.
        m.set_kind(1, SourceKind::File);
        m.set_kind(2, SourceKind::Buffer);
        m.set_kind(3, SourceKind::File);
        let none_id = m.ids[0];
        let buffer_id = m.ids[2];

        // Detaching the File kind closes exactly the two File tabs.
        let closed = m.detach_sources_of_kind(SourceKind::File);
        assert_eq!(closed, 2);
        assert_eq!(m.ids.len(), 2);
        // The None + Buffer tabs survive (and their kinds stay aligned).
        assert!(m.ids.contains(&none_id));
        assert!(m.ids.contains(&buffer_id));
        assert!(!m.kinds.contains(&SourceKind::File));

        // A kind no document uses closes nothing.
        let closed = m.detach_sources_of_kind(SourceKind::Snapshot);
        assert_eq!(closed, 0);
        assert_eq!(m.ids.len(), 2);
    }

    #[test]
    fn detach_sources_of_kind_never_leaves_blank_area() {
        // Closing the last remaining tab via a detach re-opens a fresh untitled
        // tab (the never-blank reflex still applies through `close_index`).
        let mut m = TabModel::new(); // [1] kind None
        m.set_kind(0, SourceKind::File);
        let only = m.ids[0];
        let closed = m.detach_sources_of_kind(SourceKind::File);
        assert_eq!(closed, 1);
        // One fresh (new-id, None-kind) tab remains.
        assert_eq!(m.ids.len(), 1);
        assert_ne!(m.ids[0], only);
        assert_eq!(m.kinds[0], SourceKind::None);
    }

    #[test]
    fn source_status_label_describes_kind_and_target() {
        use super::super::state::{DataSource, SourceKind};
        use super::source_status_label;
        // A live file source → "File: <target>".
        let s = DataSource::new(SourceKind::File, "game.bin");
        assert_eq!(
            source_status_label(&s),
            format!("{}: game.bin", SourceKind::File.label())
        );
        // No source → just the kind label (no trailing ": ").
        let none = DataSource::none();
        assert_eq!(source_status_label(&none), SourceKind::None.label());
    }

    /// The code-view selector option lists (`fmtCombo`/`scopeCombo` items) must be
    /// in `enum class` discriminant order so a clicked item's
    /// [`DocAreaEvent::CodeOptionsChanged`] index round-trips through
    /// `CodeFormat::from_index`/`CodeScope::from_index` to the same enum value
    /// (the persisted-index contract; main.cpp:2413-2415/2439-2441).
    #[test]
    fn code_selector_lists_match_enum_discriminant_order() {
        use crate::generator::{CodeFormat, CodeScope};
        for (i, &fmt) in super::CODE_FORMATS.iter().enumerate() {
            assert_eq!(fmt as i32, i as i32, "fmt list out of enum order at {i}");
            assert_eq!(CodeFormat::from_index(i as i32), fmt);
        }
        for (i, &scope) in super::CODE_SCOPES.iter().enumerate() {
            assert_eq!(
                scope as i32, i as i32,
                "scope list out of enum order at {i}"
            );
            assert_eq!(CodeScope::from_index(i as i32), scope);
        }
    }
}
