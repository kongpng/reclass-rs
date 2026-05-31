//! The main window shell — the top-level gpui view every UI piece plugs into.
//!
//! Port of the C++ `MainWindow` chrome (app-shell §5 titlebar, §6 window, §8
//! document tabs, §10 docks, §13 start page). Composes:
//!
//! - the custom frameless [`TitleBar`](gpui_component::TitleBar) (app-shell §5)
//!   assembled by [`super::titlebar::render_titlebar`]: app label, the workspace
//!   layout-toggle pair, and the dual view-mode toggle,
//! - a [`DockArea`] holding the MDI document-tab center
//!   ([`DocumentArea`](super::tabs::DocumentArea)) + the workspace dock
//!   ([`WorkspacePanel`](super::workspace::WorkspacePanel)) + a scanner dock,
//!   built by [`super::docks::build_default_layout`],
//! - the [`StartPage`](super::startpage::StartPage) welcome overlay (shown over
//!   the workspace on launch; app-shell §13),
//! - the [`Root`] overlay layers (modals/dialogs/sheets/notifications), and
//! - the gpui-free [`AppState`] window-state (open docs, active doc, source,
//!   selection, view mode, theme handle).
//!
//! The chrome is **wired to the app state + editor surface** (the stage goal):
//! the workspace toggle shows/hides the left dock and syncs the preset; the tab
//! strip's events ([`DocAreaEvent`]) update [`AppState`] + rebuild the workspace
//! model; clicking a workspace type row ([`WorkspaceNav`]) drives quick
//! navigation; the start page's cards/files open documents or dismiss.
//!
//! Theme is owned by a [`ThemeManager`] global; on construction the window
//! resolves the current theme and applies it via [`super::theme_apply`].
//!
//! Gated behind the `ui` feature.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{DockArea, DockPlacement};
use gpui_component::{ActiveTheme, Root, TitleBar, WindowExt};

use super::docks::{self, LayoutHandles, MAIN_DOCK_AREA};
use super::startpage::{RecentEntry, StartPage, StartPageEvent};
use super::state::{AppState, DocId, ViewMode};
use super::tabs::{DocAreaEvent, DocumentArea};
use super::theme_apply::ThemeRegistryGlobal;
use super::titlebar::{self, LayoutPreset};
use super::workspace::{WorkspaceDoc, WorkspaceModel, WorkspaceNav, WorkspacePanel};
use crate::theme::ThemeManager;

// App-level actions. Mirrors Zed: the command palette opens on Ctrl+Shift+P / F1
// (`command_palette::Toggle` in Zed's default keymap).
actions!(rcx_app, [OpenCommandPalette]);

/// The application's root view — the C++ `MainWindow` (app-shell §6).
pub struct MainWindow {
    /// Window-level application state (open docs, active doc, source, selection,
    /// view mode, theme handle). gpui-free + unit-tested (see [`super::state`]).
    state: AppState,
    /// The docking workspace (center document tabs + side docks).
    dock_area: Entity<DockArea>,
    /// The center MDI document-tab area (the tab strip + editor host).
    document_area: Entity<DocumentArea>,
    /// The left workspace ("Project") dock panel.
    workspace: Entity<WorkspacePanel>,
    /// The start-page welcome overlay while shown (app-shell §13).
    start_page: Option<Entity<StartPage>>,
    /// The workspace layout-toggle state (mirrors the left dock's visibility).
    layout_preset: LayoutPreset,
    /// Shared theme manager (the C++ `ThemeManager` singleton; owned by the app).
    theme_manager: Rc<RefCell<ThemeManager>>,
    /// Live subscription to the currently-open command-palette modal — kept so its
    /// Trigger/Cancel events fire while shown (a dropped subscription stops them).
    palette_sub: Option<Subscription>,
}

impl MainWindow {
    /// Construct the main window view: build the [`DockArea`], assemble the
    /// default dock layout, seed [`AppState`], wire the dock/tab/workspace events,
    /// show the start page, and apply the current theme.
    pub fn new(
        theme_manager: Rc<RefCell<ThemeManager>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Build the docking workspace and its default layout (app-shell §7/§10).
        let dock_area =
            cx.new(|cx| DockArea::new(MAIN_DOCK_AREA.id, Some(MAIN_DOCK_AREA.version), window, cx));
        let LayoutHandles {
            document_area,
            workspace,
        } = docks::build_default_layout(&dock_area, window, cx);

        // Seed window state with the initial document tab (the C++ "never leave a
        // blank window"; app-shell §8 step 9). The center `DocumentArea` already
        // created its first editor tab; mirror it into `AppState`.
        let mut state = AppState::new();
        state.open_document("Untitled");

        // The left workspace dock is open by default (Layout_Workspace); reflect
        // that in the toggle preset.
        let layout_preset =
            LayoutPreset::for_visible(dock_area.read(cx).is_dock_open(DockPlacement::Left, cx));

        // Apply the current theme to gpui-component (themes.md §4.16). Record the
        // active theme name in the state (the theme handle).
        {
            let tm = theme_manager.borrow();
            let current = tm.current().clone();
            state.set_theme_name(&current.name);
            super::theme_apply::apply_theme(&current, window, cx);
        }

        // ── Wire the document-area events (app-shell §8 step 9 signal contract). ──
        cx.subscribe_in(
            &document_area,
            window,
            |this, _area, ev: &DocAreaEvent, window, cx| {
                this.on_doc_area_event(ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Wire workspace quick-navigation (app-shell §10 "Open in Current Tab"). ──
        cx.subscribe_in(
            &workspace,
            window,
            |this, _ws, nav: &WorkspaceNav, window, cx| {
                this.on_workspace_nav(*nav, window, cx);
            },
        )
        .detach();

        let mut win = MainWindow {
            state,
            dock_area,
            document_area,
            workspace,
            start_page: None,
            layout_preset,
            theme_manager,
            palette_sub: None,
        };

        // Rebuild the workspace model from the seeded document, then show the
        // start page over the workspace (the C++ deferred `showStartPage`).
        win.rebuild_workspace(cx);
        win.show_start_page(window, cx);
        win
    }

    /// Read-only access to the window state (for tests / wiring).
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Mutable access to the window state.
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// Open the command palette (Ctrl+Shift+P / F1 — Zed parity). Builds a fresh
    /// palette over the menu tree, shows it in the gpui-component dialog layer, and
    /// routes its Trigger/Cancel back here (close, then dispatch the command).
    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::commandpalette::{CommandPalette, PaletteEvent};
        let palette = cx.new(|cx| CommandPalette::new(window, cx));
        let focus = palette.read(cx).focus_handle(cx);
        self.palette_sub = Some(cx.subscribe_in(
            &palette,
            window,
            |this, _p, ev: &PaletteEvent, window, cx| match ev {
                PaletteEvent::Trigger(cmd) => {
                    let cmd = cmd.clone();
                    window.close_dialog(cx);
                    this.run_menu_command(&cmd, window, cx);
                }
                PaletteEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let palette_for_modal = palette.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog.child(palette_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Dispatch a chosen palette command. Full menu→action mapping is layered in
    /// incrementally; the palette already opens, filters, navigates, and closes.
    fn run_menu_command(
        &mut self,
        _cmd: &super::commandpalette::CommandId,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // TODO(menu-dispatch): map CommandId -> app actions (New Class, Open, …).
    }

    /// The docking workspace entity.
    pub fn dock_area(&self) -> &Entity<DockArea> {
        &self.dock_area
    }

    /// The center document-tab area entity.
    pub fn document_area(&self) -> &Entity<DocumentArea> {
        &self.document_area
    }

    /// The current workspace layout-toggle preset.
    pub fn layout_preset(&self) -> LayoutPreset {
        self.layout_preset
    }

    /// Whether the start page is currently shown.
    pub fn start_page_visible(&self) -> bool {
        self.start_page.is_some()
    }

    // ── Workspace layout toggle (the C++ `applyLayoutPreset`; app-shell §10) ──

    /// Apply a workspace layout preset: show/hide the left dock and persist the
    /// toggle state (`applyLayoutPreset` — only the workspace dock is touched).
    pub fn apply_layout_preset(
        &mut self,
        preset: LayoutPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout_preset = preset;
        let want_open = preset.workspace_visible();
        self.set_left_dock_open(want_open, window, cx);
        cx.notify();
    }

    /// Open/close the left workspace dock to a specific state (drives the
    /// `set_open` on the underlying `Dock`).
    fn set_left_dock_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Left, cx) != open {
                if let Some(dock) = area.left_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// Sync the toggle preset from the left dock's actual visibility (the C++
    /// `setWorkspaceChecked` wired from `visibilityChanged`).
    pub fn sync_workspace_toggle(&mut self, cx: &mut Context<Self>) {
        let visible = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let preset = LayoutPreset::for_visible(visible);
        if preset != self.layout_preset {
            self.layout_preset = preset;
            cx.notify();
        }
    }

    // ── View mode (the dual tree/rendered toggle; app-shell §6/§16) ──

    /// Request a view mode on the active tab (the titlebar/strip toggle). Routes
    /// to the document area (which owns the per-tab mode) and updates state.
    pub fn set_active_view_mode(
        &mut self,
        mode: ViewMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.document_area.update(cx, |area, cx| {
            area.set_active_view_mode(mode, cx);
        });
        if let Some(id) = self.state.active_id() {
            self.state.set_view_mode(id, mode);
        }
        cx.notify();
    }

    // ── Document-area event handling (app-shell §8 step 9) ──

    fn on_doc_area_event(
        &mut self,
        ev: DocAreaEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            DocAreaEvent::Activated(id) => {
                // Mirror the activation into AppState if the id is known; the area
                // and state allocate ids independently but in lockstep order.
                self.activate_doc(id);
            }
            DocAreaEvent::NewDocumentRequested => {
                // The center opened a fresh tab (`project_new`); mirror it.
                self.state.open_document("Untitled");
                self.rebuild_workspace(cx);
            }
            DocAreaEvent::Closed(id) => {
                self.state.close_document(id);
                self.rebuild_workspace(cx);
            }
            DocAreaEvent::ViewModeChanged(id, mode) => {
                self.state.set_view_mode(id, mode);
            }
        }
        cx.notify();
    }

    /// Activate a document in `AppState` by id (best-effort: the document area is
    /// the source of truth for ordering).
    fn activate_doc(&mut self, id: DocId) {
        // If AppState knows this id, activate it directly; else fall back to the
        // matching index so the two stay aligned.
        if self.state.tab(id).is_some() {
            self.state.activate(id);
        }
    }

    // ── Workspace quick navigation (app-shell §10) ──

    fn on_workspace_nav(
        &mut self,
        nav: WorkspaceNav,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Set the owning document's editor view-root to the chosen type and make
        // its tab active (the C++ "Open in Current Tab": `setViewRootId` + raise).
        self.document_area.update(cx, |area, cx| {
            if let Some(editor) = area.active_editor().cloned() {
                editor.update(cx, |ed, cx| {
                    ed.controller_mut().set_view_root_id(nav.node_id);
                    ed.apply_document(cx);
                });
            }
        });
        if let Some(t) = self.state.active_tab_mut() {
            t.view_root = Some(nav.node_id);
        }
        cx.notify();
    }

    // ── Workspace model rebuild (app-shell §10 `rebuildWorkspaceModel`) ──

    /// Rebuild the workspace tree model from the open documents' trees and push it
    /// to the [`WorkspacePanel`]. The editor controllers own the trees; here we
    /// borrow each one **by reference** (no clone — `NodeTree` is a logic type we
    /// don't extend) and build the model in one pass.
    fn rebuild_workspace(&mut self, cx: &mut Context<Self>) {
        // Snapshot the per-tab editor handles + ids first (a short borrow of the
        // area), then build against live read-borrows of each controller's tree.
        let entries: Vec<(DocId, Entity<super::editor::RcxEditor>)> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| (t.id, t.editor.clone()))
            .collect();

        let viewed: Vec<u64> = entries
            .iter()
            .map(|(_, ed)| ed.read(cx).controller().view_root_id())
            .filter(|&id| id != 0)
            .collect();

        // Hold the editor read-guards so their `&NodeTree`s stay valid for the
        // build call (all immutable borrows of `cx` coexist).
        let guards: Vec<(DocId, &super::editor::RcxEditor)> =
            entries.iter().map(|(id, ed)| (*id, ed.read(cx))).collect();
        let docs: Vec<WorkspaceDoc> = guards
            .iter()
            .map(|(id, ed)| WorkspaceDoc {
                doc: *id,
                tree: ed.controller().tree(),
            })
            .collect();
        let model = WorkspaceModel::build(&docs, &[], &viewed);

        self.workspace.update(cx, |ws, cx| {
            ws.set_model(model, cx);
        });
    }

    // ── Start page (app-shell §13) ──

    /// Show the start-page welcome overlay over the workspace (`showStartPage`).
    /// No-op if already shown. The "preload a New Class behind the splash" reflex
    /// is satisfied because the center always holds a document (created in
    /// [`DocumentArea::new`]); dismissing lands on it.
    pub fn show_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.start_page.is_some() {
            return;
        }
        let entries = self.recent_entries();
        let page = StartPage::view(entries, window, cx);
        cx.subscribe_in(
            &page,
            window,
            |this, _page, ev: &StartPageEvent, window, cx| {
                this.on_start_page_event(ev.clone(), window, cx);
            },
        )
        .detach();
        self.start_page = Some(page);
        cx.notify();
    }

    /// Dismiss the start page (`dismissStartPage`).
    pub fn dismiss_start_page(&mut self, cx: &mut Context<Self>) {
        if self.start_page.take().is_some() {
            cx.notify();
        }
    }

    /// The recent-files entries for the start page. The persistent recent-files
    /// list + examples dir are wired with the project-lifecycle workflow; for now
    /// this is empty (the start page shows "No recent files").
    fn recent_entries(&self) -> Vec<RecentEntry> {
        Vec::new()
    }

    fn on_start_page_event(
        &mut self,
        ev: StartPageEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::startpage::StartCard;
        match ev {
            StartPageEvent::Dismissed => {
                self.dismiss_start_page(cx);
            }
            StartPageEvent::Card(StartCard::NewClass) => {
                // New Class → dismiss + open a fresh document (the center already
                // holds one; open another to mirror "explicit New Class").
                self.dismiss_start_page(cx);
                self.document_area.update(cx, |area, cx| {
                    area.push_document("Untitled", window, cx);
                });
                self.state.open_document("Untitled");
                self.rebuild_workspace(cx);
            }
            StartPageEvent::Card(_) => {
                // Open project / import paths land with the project-lifecycle +
                // imports workflows; for now dismiss so the user reaches the editor.
                self.dismiss_start_page(cx);
            }
            StartPageEvent::FileSelected(path) => {
                // The C++ start-page recent-file click → `project_open(path)`
                // (app-shell §13). Dismiss the splash + load the `.rcx` into the
                // active document via the controller/imports lifecycle.
                self.dismiss_start_page(cx);
                self.open_project(std::path::Path::new(&path), None, window, cx);
            }
        }
    }

    // ── Document lifecycle (the C++ `project_open` / `loadData`; app-shell §8) ──

    /// Open a `.rcx` project into the **active** document tab (the C++
    /// `project_open(path)`; main.cpp:6370). Loads the tree via the controller's
    /// [`RcxDocument::load`](crate::controller::RcxDocument::load) (the forgiving
    /// 8-step JSON load), optionally attaches a binary data source
    /// (`loadData(dataPath)`), refreshes the editor, then syncs the window state:
    /// the tab title (from the file stem, the C++ `rootName`), the data-source
    /// icon, and the workspace tree. The start page is dismissed so the user
    /// lands on the loaded document.
    ///
    /// `.xml` paths are routed through the ReClass-XML importer
    /// ([`crate::imports::import_reclass_xml`]) when the `imports` feature is on;
    /// every other extension is treated as native `.rcx` JSON. Returns `true` on
    /// a successful load (the tab now shows the project), `false` otherwise (the
    /// existing document is left untouched — the C++ "refuse rather than show a
    /// placeholder", controller.cpp:201).
    pub fn open_project(
        &mut self,
        path: &std::path::Path,
        data_path: Option<&std::path::Path>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::controller::RcxDocument;

        // Build the document from disk. ReClass-XML imports go through the XML
        // importer (when compiled in); `.rcx`/`.json`/anything else is the native
        // forgiving JSON load. On any failure we leave the current doc untouched.
        let mut doc = RcxDocument::new();
        let is_xml = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("xml"))
            .unwrap_or(false);

        let loaded = if is_xml {
            self.load_reclass_xml(path, &mut doc)
        } else {
            doc.load(path)
        };
        if !loaded {
            tracing::warn!(path = %path.display(), "open_project: load failed — keeping current document");
            return false;
        }
        doc.file_path = Some(path.to_path_buf());

        // Optionally attach the binary data source (`--data` / saved source).
        if let Some(dp) = data_path {
            doc.load_data_file(dp);
        }
        let source = Self::source_for_doc(&doc);

        // Push the loaded document into the active editor (the C++ rebinds the
        // active tab's controller). The editor recomposes + picks a view root.
        let Some(active_id) = self.active_doc_id(cx) else {
            return false;
        };
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.set_document(doc, cx));
        }

        // Sync the tab title (file stem ≈ the C++ `rootName`) + source icon into
        // both the document area and the window state.
        let title = Self::title_for_path(path);
        self.document_area.update(cx, |area, cx| {
            area.set_title(active_id, title.clone(), cx);
            area.set_source(active_id, source.clone(), cx);
        });
        self.state.set_title(active_id, title);
        self.state.set_source(active_id, source);

        // Refresh the workspace tree from the freshly loaded document + dismiss
        // the start page so the user lands on the document.
        self.rebuild_workspace(cx);
        self.dismiss_start_page(cx);
        cx.notify();
        true
    }

    /// Load a ReClass-XML file into `doc.tree` (the C++ `import_reclass_xml`
    /// path). Gated on the `imports` feature; without it, XML opens fail
    /// gracefully (returns `false`).
    #[cfg(feature = "imports")]
    fn load_reclass_xml(
        &self,
        path: &std::path::Path,
        doc: &mut crate::controller::RcxDocument,
    ) -> bool {
        match crate::imports::import_reclass_xml(path, 8) {
            Ok(tree) => {
                doc.tree = tree;
                true
            }
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "import_reclass_xml failed");
                false
            }
        }
    }

    #[cfg(not(feature = "imports"))]
    fn load_reclass_xml(
        &self,
        _path: &std::path::Path,
        _doc: &mut crate::controller::RcxDocument,
    ) -> bool {
        tracing::warn!("ReClass-XML import requires the `imports` feature");
        false
    }

    /// The active document id, preferring the `DocumentArea` (the source of truth
    /// for tab ordering) and falling back to [`AppState`].
    fn active_doc_id(&self, cx: &Context<Self>) -> Option<DocId> {
        self.document_area
            .read(cx)
            .active_entry()
            .map(|e| e.id)
            .or_else(|| self.state.active_id())
    }

    /// Map a loaded document's provider/data path to the UI [`DataSource`]
    /// summary (the tab source-icon; app-shell §8 `refreshDocTabSourceIcon`).
    fn source_for_doc(doc: &crate::controller::RcxDocument) -> super::state::DataSource {
        use super::state::{DataSource, SourceKind};
        match &doc.data_path {
            Some(p) => DataSource::new(SourceKind::File, p.to_string_lossy().into_owned()),
            None => DataSource::none(),
        }
    }

    /// The tab title for an opened project file — its stem (the C++ titles a tab
    /// by the file/struct name; here the file stem is the stable, faithful
    /// choice and matches `updateWindowTitle`). Falls back to `"Untitled"`.
    fn title_for_path(path: &std::path::Path) -> String {
        path.file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    /// Switch the active theme by index and re-apply it (themes.md §4.4
    /// `setCurrent` → §4.16 `themeChanged` → re-style). Updates the state's
    /// theme handle. No-op if the index is out of range.
    pub fn switch_theme(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        let applied = {
            let mut tm = self.theme_manager.borrow_mut();
            tm.set_current(index);
            tm.current().clone()
        };
        self.state.set_theme_name(&applied.name);
        super::theme_apply::apply_theme(&applied, window, cx);
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Root overlay layers (modals/dialogs/sheets/notifications).
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);

        // The active document's title for the titlebar (app-shell §6
        // `updateWindowTitle`), and the current view mode for the toggle.
        let doc_title = self
            .state
            .active_tab()
            .map(|t| t.title.clone())
            .unwrap_or_else(|| "Reclass".to_string());
        let has_doc = !self.state.is_empty();
        let view_mode = self.state.active_view_mode();
        let preset = self.layout_preset;

        // The custom titlebar (app-shell §5) with the workspace + view-mode
        // toggles. The toggle callbacks re-enter this entity via a weak handle
        // (the idiomatic `App`-scoped closure ⇄ entity bridge).
        let this = cx.entity().downgrade();
        let layout_cb = {
            let this = this.clone();
            move |p: LayoutPreset, window: &mut Window, app: &mut App| {
                let _ = this.update(app, |me, cx| me.apply_layout_preset(p, window, cx));
            }
        };
        let view_cb = {
            let this = this.clone();
            move |m: ViewMode, window: &mut Window, app: &mut App| {
                let _ = this.update(app, |me, cx| me.set_active_view_mode(m, window, cx));
            }
        };
        let titlebar: TitleBar = titlebar::render_titlebar(
            preset, view_mode, doc_title, has_doc, layout_cb, view_cb, cx,
        );

        div()
            .id("reclass-main-window")
            .key_context("RcxWindow")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(Self::open_command_palette))
            .child(titlebar)
            // The docking workspace: center document tabs + side docks.
            .child(div().flex_1().min_h_0().child(self.dock_area.clone()))
            // The start-page overlay (rendered on top of the workspace while shown).
            .when_some(self.start_page.clone(), |this, page| this.child(page))
            // Overlay layers.
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
    }
}

/// Startup options for the application window — the CLI-derived launch state
/// (`src/main.rs`). Mirrors the C++ `main()` positional-argument + `--data`
/// handling: an optional `.rcx`/`.xml` project to open and an optional binary
/// data file to attach (app-shell §13; main.cpp:8774 `project_open(path)`).
///
/// Defaulting all fields gives the plain "launch to start page" behaviour
/// ([`StartupOptions::default`]), so [`open_main_window`] is `with` + defaults.
#[derive(Clone, Debug, Default)]
pub struct StartupOptions {
    /// A project file (`.rcx` native JSON or `.xml` ReClass-XML) to open on
    /// launch, loaded into the initial document tab.
    pub project: Option<std::path::PathBuf>,
    /// A binary file to attach as the document's data source (`loadData`).
    pub data: Option<std::path::PathBuf>,
}

/// Open the main application window with no startup project (launch to the start
/// page). Called from `main.rs` inside `gpui_platform::application().run(...)`,
/// after `gpui_component::init(cx)`.
pub fn open_main_window(cx: &mut App) {
    open_main_window_with(cx, StartupOptions::default());
}

/// Open the main application window, optionally opening a project / attaching a
/// data source on launch ([`StartupOptions`]). This is the entry point the
/// binary calls with the parsed CLI; the no-arg [`open_main_window`] delegates
/// here with defaults.
///
/// Registers the global key bindings (editor surface + inline fields + start
/// page + command palette + find bar), opens the `Root`-wrapped [`MainWindow`],
/// and — if a project path was given — drives the document-open lifecycle
/// ([`MainWindow::open_project`]) so the window comes up already showing the
/// loaded project (the C++ `QMetaObject::invokeMethod(... project_open ...)`
/// after `window.show()`; main.cpp:8774).
pub fn open_main_window_with(cx: &mut App, options: StartupOptions) {
    let theme_manager = ThemeRegistryGlobal::get(cx);

    // Register the editor-surface + inline-field + start-page key bindings, plus
    // the dialog/popup contexts (command palette + find bar; the dialogs/pickers
    // are opened through the `Root` overlay and own their own key contexts).
    let mut bindings = super::editor::editor_key_bindings();
    bindings.extend(super::editor::inline_edit::field_key_bindings());
    bindings.extend(super::startpage::start_page_key_bindings());
    bindings.extend(super::commandpalette::command_palette_key_bindings());
    bindings.extend(super::findbar::find_bar_key_bindings());
    // Global trigger to OPEN the palette (Zed: ctrl-shift-p / f1; cmd-shift-p on mac).
    bindings.push(KeyBinding::new(
        "ctrl-shift-p",
        OpenCommandPalette,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-p",
        OpenCommandPalette,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new("f1", OpenCommandPalette, Some("RcxWindow")));
    cx.bind_keys(bindings);

    cx.spawn(async move |cx| {
        let window_handle = cx.open_window(WindowOptions::default(), |window, cx| {
            window.set_window_title("Reclass");
            let view = cx.new(|cx| MainWindow::new(theme_manager.clone(), window, cx));
            cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
        });

        // After the window is up, open the CLI-requested project (if any) into
        // the initial tab — the C++ deferred `project_open(path)` after show.
        if let (Ok(handle), Some(project)) = (window_handle, options.project) {
            let data = options.data;
            let _ = handle.update(cx, move |root, window, cx| {
                let main = root.view().clone();
                if let Ok(main) = main.downcast::<MainWindow>() {
                    main.update(cx, |mw, cx| {
                        mw.open_project(&project, data.as_deref(), window, cx);
                    });
                }
            });
        }
    })
    .detach();
}
