//! The main window shell — the top-level gpui view every UI piece plugs into.
//!
//! Port of the C++ `MainWindow` chrome (app-shell §5 titlebar, §6 window, §8
//! document tabs, §10 docks, §13 start page). Composes:
//!
//! - the custom frameless [`TitleBar`](gpui_component::TitleBar) (app-shell §5)
//!   assembled by [`super::titlebar::render_titlebar`]: app label, the in-window
//!   menu bar, the document title, and the workspace sidebar toggle (the
//!   view-mode switch is the document area's bottom segmented control),
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
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Root, TitleBar, WindowExt};

use super::bookmarkspanel::BookmarksPanel;
use super::docks::{self, LayoutHandles, MAIN_DOCK_AREA};
use super::menubar::{MenuBar, MenuCommand};
use super::modulespanel::ModulesPanel;
use super::startpage::{RecentEntry, StartPage, StartPageEvent};
use super::state::{AppState, DocId, ViewMode};
use super::statusbar::{render_status_bar, StatusInfo};
use super::tabs::{DocAreaEvent, DocumentArea};
use super::theme_apply::ThemeRegistryGlobal;
use super::titlebar::{self, LayoutPreset};
use super::workspace::{WorkspaceDoc, WorkspaceModel, WorkspaceNav, WorkspacePanel};
use crate::theme::ThemeManager;

// App-level actions. Mirrors Zed: the command palette opens on Ctrl+Shift+P / F1
// (`command_palette::Toggle` in Zed's default keymap). `ToggleScanner` shows/hides
// the bottom memory-scanner dock (the C++ pop-out summoned on demand; bound to
// Ctrl+Shift+M and View ▸ Memory Scanner). The rest are the View-menu shortcuts
// the contract requires global key bindings for (F5 Refresh, Ctrl+G Go to
// Address, Ctrl+Shift+Y/B Modules/Bookmarks docks, Ctrl+\ Split / Ctrl+Shift+\
// Unsplit). Each routes back through [`MainWindow::run_menu_command`] so the
// keyboard, the menu bar, and the command palette all share one dispatch.
actions!(
    rcx_app,
    [
        OpenCommandPalette,
        ToggleScanner,
        RefreshView,
        GotoAddressAction,
        ToggleModules,
        ToggleBookmarks,
        SplitEditor,
        UnsplitEditor
    ]
);

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
    /// The right-dock Modules panel (View ▸ Modules; `Ctrl+Shift+Y`). Held so the
    /// window can toggle/observe the right dock (the C++ summon-on-demand panel).
    #[allow(dead_code)]
    modules: Entity<ModulesPanel>,
    /// The right-dock Bookmarks panel (View ▸ Bookmarks; `Ctrl+Shift+B`). Shares
    /// the right dock's tab strip with [`modules`](Self::modules).
    #[allow(dead_code)]
    bookmarks: Entity<BookmarksPanel>,
    /// The in-window menu bar (the titlebar's File/Edit/View/… dropdown row).
    menubar: Entity<MenuBar>,
    /// The start-page welcome overlay while shown (app-shell §13).
    start_page: Option<Entity<StartPage>>,
    /// The workspace layout-toggle state (mirrors the left dock's visibility).
    layout_preset: LayoutPreset,
    /// Shared theme manager (the C++ `ThemeManager` singleton; owned by the app).
    theme_manager: Rc<RefCell<ThemeManager>>,
    /// Live subscription to the currently-open command-palette modal — kept so its
    /// Trigger/Cancel events fire while shown (a dropped subscription stops them).
    palette_sub: Option<Subscription>,
    /// Observations of the open editors — one per tab. The editor entities own
    /// the selection; they `cx.notify()` themselves on a row click but do NOT
    /// emit up to us, so without these the bottom status bar (computed in
    /// [`MainWindow::render`]) would stay stale after a selection. Each
    /// observation re-renders the window so [`StatusInfo::for_controller`] re-runs
    /// against the fresh selection. Rebuilt whenever the tab set changes.
    editor_observers: Vec<Subscription>,
    /// Live subscription to an open Go-to-Address dialog — kept so its Go/Cancel
    /// events fire while shown (a dropped subscription stops them; mirrors
    /// [`palette_sub`](Self::palette_sub)).
    goto_sub: Option<Subscription>,
    /// The current editor font-scale (rem-size multiplier). View ▸ Font ▸
    /// Increase/Decrease/Reset steps this and re-applies it via
    /// [`Window::set_rem_size`] (the C++ `setFontSize`; the whole editor grid
    /// scales because its glyph metrics derive from the active font size).
    font_scale: f32,
    /// Whether Presentation Mode is on (View ▸ Presentation Mode). A live toggle
    /// flag reflected in the View menu ✓; the chrome dimming lands with its own
    /// pass — here it owns the state + the checkmark so the menu reflects reality.
    presentation: bool,
    /// Live window-level mirror of the editor view-option toggles so the View
    /// menu's ✓ marks reflect reality even for the render-level flags
    /// (compact columns / hover effects / minimap / relative offsets). Compose
    /// flags (tree lines / type hints / comments) live on the controller; these
    /// are pushed into the active editor via the EDITOR SETTER CONTRACT.
    view_opts: ViewOptions,
}

/// The seven checkable View-menu options (the C++ View menu defaults; the
/// `[check]` items in the MENU CONTRACT). All default **on** except comments.
/// `compact_columns`/`relative_offsets`/`hover_effects`/`minimap` are
/// render-level (the editor view); `tree_lines`/`type_hints`/`show_comments` are
/// compose flags (threaded through the controller's recompose).
#[derive(Clone, Copy, Debug)]
struct ViewOptions {
    compact_columns: bool,
    tree_lines: bool,
    relative_offsets: bool,
    type_hints: bool,
    show_comments: bool,
    hover_effects: bool,
    minimap: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        // Match the C++ View menu defaults (view_options.png): everything checked
        // except Comments.
        ViewOptions {
            compact_columns: true,
            tree_lines: true,
            relative_offsets: true,
            type_hints: true,
            show_comments: false,
            hover_effects: true,
            minimap: true,
        }
    }
}

impl ViewOptions {
    fn get(&self, opt: ViewOpt) -> bool {
        match opt {
            ViewOpt::CompactColumns => self.compact_columns,
            ViewOpt::TreeLines => self.tree_lines,
            ViewOpt::RelativeOffsets => self.relative_offsets,
            ViewOpt::TypeHints => self.type_hints,
            ViewOpt::ShowComments => self.show_comments,
            ViewOpt::HoverEffects => self.hover_effects,
            ViewOpt::Minimap => self.minimap,
        }
    }

    fn set(&mut self, opt: ViewOpt, value: bool) {
        match opt {
            ViewOpt::CompactColumns => self.compact_columns = value,
            ViewOpt::TreeLines => self.tree_lines = value,
            ViewOpt::RelativeOffsets => self.relative_offsets = value,
            ViewOpt::TypeHints => self.type_hints = value,
            ViewOpt::ShowComments => self.show_comments = value,
            ViewOpt::HoverEffects => self.hover_effects = value,
            ViewOpt::Minimap => self.minimap = value,
        }
    }
}

/// The seven checkable editor view options, used to drive a single
/// [`MainWindow::toggle_view_option`] dispatch + the menu ✓ sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewOpt {
    CompactColumns,
    TreeLines,
    RelativeOffsets,
    TypeHints,
    ShowComments,
    HoverEffects,
    Minimap,
}

impl ViewOpt {
    const ALL: [ViewOpt; 7] = [
        ViewOpt::CompactColumns,
        ViewOpt::TreeLines,
        ViewOpt::RelativeOffsets,
        ViewOpt::TypeHints,
        ViewOpt::ShowComments,
        ViewOpt::HoverEffects,
        ViewOpt::Minimap,
    ];

    /// The MENU CONTRACT command id whose ✓ this option drives.
    fn command_id(self) -> &'static str {
        match self {
            ViewOpt::CompactColumns => "view.compact_columns",
            ViewOpt::TreeLines => "view.tree_lines",
            ViewOpt::RelativeOffsets => "view.relative_offsets",
            ViewOpt::TypeHints => "view.type_hints",
            ViewOpt::ShowComments => "view.comments",
            ViewOpt::HoverEffects => "view.hover",
            ViewOpt::Minimap => "view.minimap",
        }
    }
}

/// File ▸ Import ▸ … target — picks the importer + the file-picker prompt text.
#[derive(Clone, Copy, Debug)]
enum ImportKind {
    Source,
    Xml,
    Pdb,
}

impl ImportKind {
    fn prompt(self) -> &'static str {
        match self {
            ImportKind::Source => "Import C/C++ source",
            ImportKind::Xml => "Import ReClass XML",
            ImportKind::Pdb => "Import PDB",
        }
    }
}

/// File ▸ Export ▸ … target — picks the [`crate::generator`] renderer + the
/// suggested file extension.
#[derive(Clone, Copy, Debug)]
enum ExportKind {
    Cpp,
    Rust,
    Defines,
    CSharp,
    Python,
    Xml,
}

impl ExportKind {
    /// The suggested save-file extension (incl. the leading dot).
    fn extension(self) -> &'static str {
        match self {
            ExportKind::Cpp => ".h",
            ExportKind::Rust => ".rs",
            ExportKind::Defines => ".h",
            ExportKind::CSharp => ".cs",
            ExportKind::Python => ".py",
            ExportKind::Xml => ".xml",
        }
    }

    /// Render the active document's tree to this format. `view_root == 0` exports
    /// **all** top-level structs; a specific view root exports that struct + its
    /// reachable structs (`*_tree`). Returns `None` when nothing was produced.
    fn render(self, tree: &crate::core::NodeTree, view_root: u64) -> Option<String> {
        use crate::generator as g;
        let text = match (self, view_root) {
            (ExportKind::Cpp, 0) => g::render_cpp_all(tree, None, false),
            (ExportKind::Cpp, r) => g::render_cpp_tree(tree, r, None, false),
            (ExportKind::Rust, 0) => g::render_rust_all(tree, None, false),
            (ExportKind::Rust, r) => g::render_rust_tree(tree, r, None, false),
            (ExportKind::Defines, 0) => g::render_defines_all(tree),
            (ExportKind::Defines, r) => g::render_defines_tree(tree, r),
            (ExportKind::CSharp, 0) => g::render_csharp_all(tree, None, false),
            (ExportKind::CSharp, r) => g::render_csharp_tree(tree, r, None, false),
            (ExportKind::Python, 0) => g::render_python_all(tree),
            (ExportKind::Python, r) => g::render_python_tree(tree, r),
            (ExportKind::Xml, _) => return Self::render_xml(tree),
        };
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// ReClass XML export goes through the importer module's exporter, which is
    /// file-based — render to a temp file, read it back as the export text.
    /// Gated on the `imports` feature.
    #[cfg(feature = "imports")]
    fn render_xml(tree: &crate::core::NodeTree) -> Option<String> {
        let mut path = std::env::temp_dir();
        path.push("reclass-export.xml");
        crate::imports::export_reclass_xml(tree, &path).ok()?;
        let text = std::fs::read_to_string(&path).ok();
        let _ = std::fs::remove_file(&path);
        text.filter(|t| !t.trim().is_empty())
    }

    #[cfg(not(feature = "imports"))]
    fn render_xml(_tree: &crate::core::NodeTree) -> Option<String> {
        None
    }
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
            modules,
            bookmarks,
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

        // ── The in-window menu bar (the titlebar dropdown row; app-shell §7). ──
        // Its chosen-command events route to `run_menu_command` — the same
        // dispatch the command palette's `Trigger` uses.
        let menubar = MenuBar::view(cx);
        cx.subscribe_in(
            &menubar,
            window,
            |this, _mb, ev: &MenuCommand, window, cx| {
                this.run_menu_command(&ev.0, window, cx);
            },
        )
        .detach();

        let mut win = MainWindow {
            state,
            dock_area,
            document_area,
            workspace,
            modules,
            bookmarks,
            menubar,
            start_page: None,
            layout_preset,
            theme_manager,
            palette_sub: None,
            editor_observers: Vec::new(),
            goto_sub: None,
            font_scale: 1.0,
            presentation: false,
            view_opts: ViewOptions::default(),
        };

        // Observe the initial editor(s) so a row selection re-renders the window
        // (and thus refreshes the status bar; see [`Self::observe_editors`]).
        win.observe_editors(cx);
        // Reflect the initial scanner-dock state in the View menu (closed by
        // default ⇒ View ▸ Memory Scanner starts unchecked).
        win.sync_scanner_menu_checked(cx);
        // Push the initial checkable-View-menu states into the menu bar so the
        // View menu reflects reality on first open (the C++ defaults: every view
        // option checked except Comments; the Project dock open; the right-dock
        // panels + presentation closed). Mirrors `sync_scanner_menu_checked`.
        win.sync_view_menu_checked(cx);
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
            // Center a Zed-style picker: a ~600px card anchored in the top third
            // over a dimming scrim (the dialog's default `overlay`). The palette
            // renders its own elevated card + header, so suppress the dialog's
            // chrome (close button + inner padding) and let it size the card.
            dialog
                .w(px(600.))
                .margin_top(px(120.))
                .close_button(false)
                .child(palette_for_modal.clone())
        });
        // Focus the palette's query input (its `Focusable` handle delegates to the
        // input) so the first keystroke types into the palette instead of falling
        // through to the window's global shortcuts. `open_dialog` focuses its own
        // handle first, so this runs last and wins.
        window.focus(&focus, cx);
        cx.notify();
    }

    // ── Global-key-binding action handlers (route to `run_menu_command`) ──────
    // Each maps a registered key binding to its MENU CONTRACT command so the
    // keyboard, the menu bar, and the command palette share one dispatch.

    fn on_refresh(&mut self, _: &RefreshView, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"view.refresh".to_string(), window, cx);
    }
    fn on_goto_address(
        &mut self,
        _: &GotoAddressAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"view.goto_address".to_string(), window, cx);
    }
    fn on_toggle_modules(
        &mut self,
        _: &ToggleModules,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"view.modules".to_string(), window, cx);
    }
    fn on_toggle_bookmarks(
        &mut self,
        _: &ToggleBookmarks,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"view.bookmarks".to_string(), window, cx);
    }
    fn on_split_editor(&mut self, _: &SplitEditor, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"view.split".to_string(), window, cx);
    }
    fn on_unsplit_editor(
        &mut self,
        _: &UnsplitEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"view.unsplit".to_string(), window, cx);
    }

    /// Dispatch a chosen command (from the menu bar, the command palette, or a
    /// global key binding). Maps a
    /// [`CommandId`](super::commandpalette::CommandId) to the app operation that
    /// realizes it — handling **every** command id in the MENU CONTRACT (File /
    /// Edit / View / Help). Commands whose deeper workflow has no logic yet
    /// (some Edit clipboard ops, split editor) degrade gracefully — they notify
    /// the user rather than panic or dead-end — so a menu/palette pick always
    /// *does* something visible.
    fn run_menu_command(
        &mut self,
        cmd: &super::commandpalette::CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cmd.as_str() {
            // ── File: new documents ──
            "file.new_class" | "file.new_struct" | "file.new_enum" => {
                self.new_document(window, cx);
            }
            "file.welcome" => self.show_start_page(window, cx),

            // ── File: open / save ──
            "file.open" => self.prompt_open(window, cx),
            "file.save" => self.save_active(false, window, cx),
            "file.save_as" => self.save_active(true, window, cx),
            "file.close" => self.close_active_document(window, cx),
            "file.exit" => cx.quit(),

            // ── File: import ──
            "file.import.source" | "import.source" => {
                self.prompt_import(ImportKind::Source, window, cx)
            }
            "file.import.xml" | "import.xml" => self.prompt_import(ImportKind::Xml, window, cx),
            "file.import.pdb" | "import.pdb" => self.prompt_import(ImportKind::Pdb, window, cx),

            // ── File: export (render the active tree, prompt a save path) ──
            "file.export.cpp" | "export.cpp" => self.export_code(ExportKind::Cpp, window, cx),
            "file.export.rust" | "export.rust" => self.export_code(ExportKind::Rust, window, cx),
            "file.export.defines" | "export.defines" => {
                self.export_code(ExportKind::Defines, window, cx)
            }
            "file.export.csharp" | "export.csharp" => {
                self.export_code(ExportKind::CSharp, window, cx)
            }
            "file.export.python" | "export.python" => {
                self.export_code(ExportKind::Python, window, cx)
            }
            "file.export.xml" | "export.xml" => self.export_code(ExportKind::Xml, window, cx),

            // ── File: data source (the active-source picker; data_options.png) ──
            "source.clear" => self.set_active_source(super::state::DataSource::none(), window, cx),
            "source.file" => self.notify(
                "Pick a binary data file via Open… then attach it as the source.",
                window,
                cx,
            ),
            "source.process" | "source.kernel" | "source.remote" | "source.windbg"
            | "source.rcnet" => {
                self.notify("Live data-source providers are not available on this platform; open a `.rcx` with a saved source instead.", window, cx)
            }

            // ── Edit ──
            "edit.undo" => self.active_editor_undo(false, cx),
            "edit.redo" => self.active_editor_undo(true, cx),
            "edit.cut" | "edit.copy" | "edit.paste" | "edit.delete" | "edit.select_all" => {
                self.edit_clipboard(cmd.as_str(), window, cx)
            }

            // ── View: docks / windows ──
            "view.project" => self.toggle_left_dock(window, cx),
            "view.scanner" => self.toggle_scanner_dock(&ToggleScanner, window, cx),
            "view.modules" => self.toggle_right_dock(window, cx),
            "view.bookmarks" | "view.symbols" => self.toggle_right_dock(window, cx),
            "view.reset_windows" => self.reset_windows(window, cx),

            // ── View: editor view-option toggles (EDITOR SETTER CONTRACT) ──
            "view.compact_columns" => self.toggle_view_option(ViewOpt::CompactColumns, cx),
            "view.tree_lines" => self.toggle_view_option(ViewOpt::TreeLines, cx),
            "view.relative_offsets" => self.toggle_view_option(ViewOpt::RelativeOffsets, cx),
            "view.type_hints" => self.toggle_view_option(ViewOpt::TypeHints, cx),
            "view.comments" | "view.comment_chips" => {
                self.toggle_view_option(ViewOpt::ShowComments, cx)
            }
            "view.hover" | "view.hover_effects" => {
                self.toggle_view_option(ViewOpt::HoverEffects, cx)
            }
            "view.minimap" => self.toggle_view_option(ViewOpt::Minimap, cx),

            // ── View: theme / font ──
            "view.font.inc" => self.bump_font(0.1, window, cx),
            "view.font.dec" => self.bump_font(-0.1, window, cx),
            "view.font.reset" => self.reset_font(window, cx),

            // ── View: actions ──
            "view.refresh" => self.refresh_active_editor(cx),
            "view.goto_address" => self.open_goto_address(window, cx),
            "view.command_palette" => self.open_command_palette(&OpenCommandPalette, window, cx),
            "view.split" => self.notify("Split Editor is not available yet.", window, cx),
            "view.unsplit" => self.notify("Unsplit Editor is not available yet.", window, cx),
            "view.presentation" => self.toggle_presentation(cx),

            // ── Help ──
            "help.about" => self.notify(
                "Reclass — a Rust + GPUI port of ReClass. Memory structure editor.",
                window,
                cx,
            ),
            "help.docs" | "help.shortcuts" => self.notify(
                "Documentation: see the project README and the in-app Command Palette (Ctrl+K).",
                window,
                cx,
            ),

            // The view-mode dual toggle (Tree ⇄ rendered C/C++) is exposed through
            // the titlebar; the menu has no direct entries for it, but the toggle
            // is reachable from the command palette via these synthetic ids.
            "view.tree" => self.set_active_view_mode(ViewMode::Tree, window, cx),
            "view.rendered" => self.set_active_view_mode(ViewMode::Rendered, window, cx),

            // Theme by name (`view.theme.<NAME>`) — switch the active theme.
            other if other.starts_with("view.theme.") => {
                let name = &other["view.theme.".len()..];
                self.switch_theme_by_name(name, window, cx);
            }
            // A bundled example (`file.example.<NAME>`) — materialize + open it.
            other if other.starts_with("file.example.") => {
                let name = &other["file.example.".len()..];
                self.open_example(name, window, cx);
            }

            // ── Anything still unmapped: graceful, logged no-op. ──
            other => {
                tracing::debug!(command = %other, "menu command not yet wired");
            }
        }
    }

    /// Open a fresh document tab (the C++ `project_new` family). The per-kind
    /// seed (struct/enum) lands with the project-lifecycle workflow; for now
    /// every "New …" opens a blank document.
    fn new_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_start_page(cx);
        self.document_area.update(cx, |area, cx| {
            area.push_document("Untitled", window, cx);
        });
        self.state.open_document("Untitled");
        self.rebuild_workspace(cx);
        self.observe_editors(cx);
    }

    /// Show a transient notification (the gpui-component notification layer; the
    /// graceful fallback for commands whose deeper workflow has no logic yet, and
    /// the "copied / exported / done" confirmations).
    fn notify(
        &self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(Notification::info(message), cx);
    }

    // ── File: open / import (native file pickers) ───────────────────────────

    /// File ▸ Open… — the native path picker (`cx.prompt_for_paths`); on a chosen
    /// `.rcx`/`.xml`, route it through [`open_project`](Self::open_project) (the
    /// same plumbing the start page + CLI use). Cancelled / errored → no-op.
    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Reclass project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let Some(path) = path.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| {
                me.open_project(&path, None, window, cx);
            });
        })
        .detach();
    }

    /// File ▸ Import ▸ {From Source / ReClass XML / PDB} — pick a file, then build
    /// a document from it via the importers and load it into the active tab.
    /// XML routes through [`open_project`](Self::open_project) (its `.xml` branch);
    /// Source/PDB go through [`import_into_active`](Self::import_into_active).
    fn prompt_import(&mut self, kind: ImportKind, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(kind.prompt().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .and_then(|v| v.into_iter().next())
            else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| match kind {
                ImportKind::Xml => {
                    me.open_project(&path, None, window, cx);
                }
                ImportKind::Source | ImportKind::Pdb => {
                    me.import_into_active(kind, &path, window, cx);
                }
            });
        })
        .detach();
    }

    /// Build a document from a Source / PDB file and load it into the active tab
    /// (the C++ `import_from_source` / `import_pdb`). Gated on the `imports`
    /// feature; a parse failure notifies and leaves the current document intact.
    #[cfg(feature = "imports")]
    fn import_into_active(
        &mut self,
        kind: ImportKind,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tree = match kind {
            ImportKind::Source => match std::fs::read_to_string(path) {
                Ok(src) => crate::imports::import_from_source(&src, 8),
                Err(e) => {
                    self.notify(format!("Could not read source: {e}"), window, cx);
                    return;
                }
            },
            ImportKind::Pdb => crate::imports::import_pdb(path, ""),
            ImportKind::Xml => return,
        };
        match tree {
            Ok(tree) => {
                let mut doc = crate::controller::RcxDocument::new();
                doc.tree = tree;
                self.load_doc_into_active(doc, path, window, cx);
            }
            Err(e) => self.notify(format!("Import failed: {e}"), window, cx),
        }
    }

    #[cfg(not(feature = "imports"))]
    fn import_into_active(
        &mut self,
        _kind: ImportKind,
        _path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notify("Importing requires the `imports` feature.", window, cx);
    }

    // ── File: save / export ─────────────────────────────────────────────────

    /// File ▸ Save / Save As… — write the active document's tree as native `.rcx`
    /// JSON. "Save" reuses the known file path when present; "Save As…" (or a
    /// never-saved document) prompts for a new path via the native picker.
    fn save_active(&mut self, force_prompt: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        let known = editor.read(cx).controller().document().file_path.clone();
        if let (false, Some(path)) = (force_prompt, known) {
            self.write_document(&editor, &path, window, cx);
            return;
        }
        let suggested = editor
            .read(cx)
            .controller()
            .document()
            .file_path
            .as_ref()
            .and_then(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "Untitled.rcx".to_string());
        let dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| {
                me.write_document(&editor, &path, window, cx);
            });
        })
        .detach();
    }

    /// Persist the editor's document to `path` and reflect the new title/source.
    fn write_document(
        &mut self,
        editor: &Entity<super::editor::RcxEditor>,
        path: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ok = editor.update(cx, |ed, _cx| ed.controller_mut().document_mut().save(path));
        if ok {
            let title = Self::title_for_path(path);
            if let Some(id) = self.active_doc_id(cx) {
                self.document_area.update(cx, |area, cx| {
                    area.set_title(id, title.clone(), cx);
                });
                self.state.set_title(id, title);
            }
            self.notify(format!("Saved {}", path.display()), window, cx);
        } else {
            self.notify(format!("Failed to save {}", path.display()), window, cx);
        }
    }

    /// File ▸ Export ▸ … — render the active document's tree to the chosen code
    /// format ([`crate::generator`] / the XML exporter) and prompt a save path.
    /// Uses the editor's **view root** as the root struct (the C++ "export the
    /// open struct"); a `0` view root exports every top-level struct (`*_all`).
    fn export_code(&mut self, kind: ExportKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("No document to export.", window, cx);
            return;
        };
        let rendered = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            kind.render(ctrl.tree(), ctrl.view_root_id())
        };
        let Some(text) = rendered else {
            self.notify("Nothing to export from this document.", window, cx);
            return;
        };
        let suggested = format!("export{}", kind.extension());
        let dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
        let rx = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                return;
            };
            let _ = this.update_in(cx, |me, window, cx| match std::fs::write(&path, &text) {
                Ok(()) => me.notify(format!("Exported {}", path.display()), window, cx),
                Err(e) => me.notify(format!("Export failed: {e}"), window, cx),
            });
        })
        .detach();
    }

    // ── File: examples / close ──────────────────────────────────────────────

    /// File ▸ Examples ▸ <NAME> — materialize the bundled example to a temp file
    /// and open it through the normal window plumbing (the start-page Examples
    /// bucket). Opening a bundled example must WORK (the stage contract).
    fn open_example(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        match super::examples::write_example_to_temp(name) {
            Some(path) => {
                self.open_project(&path, None, window, cx);
            }
            None => self.notify(format!("Unknown example: {name}"), window, cx),
        }
    }

    /// File ▸ Close Project — close the active document tab. The document area
    /// never leaves a blank window (it re-seeds a fresh tab when the last closes).
    fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.document_area.read(cx).active_index();
        self.document_area.update(cx, |area, cx| {
            area.close_index(ix, window, cx);
        });
        // The area emits `Closed`/`NewDocumentRequested` which our doc-area event
        // handler mirrors into AppState + rebuilds the workspace.
        cx.notify();
    }

    // ── Edit clipboard (graceful where no logic exists) ─────────────────────

    /// Edit ▸ Cut/Copy/Paste/Delete/Select All. These node-clipboard ops have no
    /// controller logic yet, so they degrade to a notification — never a
    /// dead-end. (Undo/Redo are wired separately via [`active_editor_undo`].)
    fn edit_clipboard(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
        let label = match cmd {
            "edit.cut" => "Cut",
            "edit.copy" => "Copy",
            "edit.paste" => "Paste",
            "edit.delete" => "Delete",
            "edit.select_all" => "Select All",
            _ => "Edit",
        };
        self.notify(
            format!("{label} for nodes is not available yet."),
            window,
            cx,
        );
    }

    /// Load an already-built document into the active editor tab and sync the
    /// tab title + source (shared by import; mirrors the tail of
    /// [`open_project`](Self::open_project)).
    fn load_doc_into_active(
        &mut self,
        doc: crate::controller::RcxDocument,
        path: &std::path::Path,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = Self::source_for_doc(&doc);
        let Some(active_id) = self.active_doc_id(cx) else {
            return;
        };
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.set_document(doc, cx));
        }
        let title = Self::title_for_path(path);
        self.document_area.update(cx, |area, cx| {
            area.set_title(active_id, title.clone(), cx);
            area.set_source(active_id, source.clone(), cx);
        });
        self.state.set_title(active_id, title);
        self.state.set_source(active_id, source);
        self.rebuild_workspace(cx);
        self.dismiss_start_page(cx);
        cx.notify();
    }

    // ── View: editor view-option toggles (EDITOR SETTER CONTRACT) ────────────

    /// Flip a checkable View-menu option, push the new value into the active
    /// editor (render-level flags) or its controller (compose flags), and refresh
    /// the menu's ✓ to match. The window keeps a live mirror ([`ViewOptions`]) so
    /// the checkmark survives tab switches and reflects the C++ defaults.
    fn toggle_view_option(&mut self, opt: ViewOpt, cx: &mut Context<Self>) {
        let value = !self.view_opts.get(opt);
        self.view_opts.set(opt, value);
        // Push the new value into the active editor via the EDITOR SETTER
        // CONTRACT. The compose flags (tree_lines/type_hints/show_comments)
        // recompose; the render-level flags (compact/relative/hover/minimap)
        // repaint. The editor owns the actual effect; the window owns the ✓.
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| match opt {
                ViewOpt::CompactColumns => ed.set_compact_columns(value, cx),
                ViewOpt::TreeLines => ed.set_tree_lines(value, cx),
                ViewOpt::RelativeOffsets => ed.set_relative_offsets(value, cx),
                ViewOpt::TypeHints => ed.set_type_hints(value, cx),
                ViewOpt::ShowComments => ed.set_show_comments(value, cx),
                ViewOpt::HoverEffects => ed.set_hover_effects(value, cx),
                ViewOpt::Minimap => ed.set_minimap(value, cx),
            });
        }
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked(opt.command_id(), value, cx);
        });
        cx.notify();
    }

    /// Push every checkable View-menu option's current state into the menu bar so
    /// the View menu's ✓ marks reflect reality (called on construction + after a
    /// layout reset). Mirrors [`sync_scanner_menu_checked`](Self::sync_scanner_menu_checked)
    /// for the editor view options, the docks, and presentation mode.
    fn sync_view_menu_checked(&mut self, cx: &mut Context<Self>) {
        let opts = self.view_opts;
        let project_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let right_open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        let presentation = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            for opt in ViewOpt::ALL {
                mb.set_command_checked(opt.command_id(), opts.get(opt), cx);
            }
            mb.set_command_checked("view.project", project_open, cx);
            mb.set_command_checked("view.modules", right_open, cx);
            mb.set_command_checked("view.bookmarks", right_open, cx);
            mb.set_command_checked("view.presentation", presentation, cx);
        });
    }

    // ── View: docks ──────────────────────────────────────────────────────────

    /// Toggle the left workspace ("Project") dock (View ▸ Project). Mirrors the
    /// titlebar toggle but driven from the menu/palette, and re-syncs the ✓.
    fn toggle_left_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Left, cx);
        let preset = LayoutPreset::for_visible(!open);
        self.apply_layout_preset(preset, window, cx);
        self.sync_view_menu_checked(cx);
    }

    /// Toggle the **right** dock (Modules + Bookmarks, tabified together; View ▸
    /// Modules / Bookmarks). Mirrors [`toggle_scanner_dock`](Self::toggle_scanner_dock)
    /// for the bottom dock but for [`DockPlacement::Right`]. Both menu items
    /// reflect the dock's shared open state.
    fn toggle_right_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right, cx);
        self.set_right_dock_open(!open, window, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// Open/close the right dock to a specific state (drives `set_open` on the
    /// underlying `Dock`). Mirrors [`set_bottom_dock_open`](Self::set_bottom_dock_open).
    fn set_right_dock_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Right, cx) != open {
                if let Some(dock) = area.right_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// View ▸ Reset Windows — restore the canonical layout: the workspace dock
    /// open, the scanner + right docks closed (their default-closed state; the
    /// C++ canonical "Reset Windows" placement). Re-syncs every dock ✓.
    fn reset_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_layout_preset(LayoutPreset::Workspace, window, cx);
        self.set_bottom_dock_open(false, window, cx);
        self.set_right_dock_open(false, window, cx);
        self.sync_scanner_menu_checked(cx);
        self.sync_view_menu_checked(cx);
    }

    // ── View: font size (best-effort via the window rem size) ────────────────

    /// Step the editor font scale (View ▸ Font ▸ Increase/Decrease), clamped to a
    /// readable range, and re-apply it via [`Window::set_rem_size`] so the whole
    /// editor grid rescales (its glyph metrics derive from the active font size).
    fn bump_font(&mut self, delta: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.font_scale = (self.font_scale + delta).clamp(0.7, 2.0);
        self.apply_font_scale(window, cx);
    }

    /// View ▸ Font ▸ Reset — back to the default scale.
    fn reset_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_scale = 1.0;
        self.apply_font_scale(window, cx);
    }

    /// Apply the current font scale to the window's rem size (base 16px).
    fn apply_font_scale(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_rem_size(px(16.0 * self.font_scale));
        cx.notify();
    }

    // ── View: refresh / goto / theme / presentation ──────────────────────────

    /// View ▸ Refresh (F5) — force the active editor to recompose + repaint
    /// (the C++ `applyDocument`).
    fn refresh_active_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.apply_document(cx));
            self.rebuild_workspace(cx);
            cx.notify();
        }
    }

    /// View ▸ Go to Address… (Ctrl+G) — open the [`GotoAddressDialog`] in the
    /// dialog layer; on Go, jump the active editor to the resolved address (the
    /// status-bar readout follows). Mirrors [`open_command_palette`](Self::open_command_palette).
    fn open_goto_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::gotoaddress::{GotoAddressDialog, GotoEvent};
        let dialog = cx.new(|cx| GotoAddressDialog::new(Vec::new(), 8, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &GotoEvent, window, cx| match ev {
                GotoEvent::Go(formula, addr) => {
                    window.close_dialog(cx);
                    this.notify(format!("Go to {formula} → 0x{addr:x}"), window, cx);
                }
                GotoEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(460.))
                .margin_top(px(120.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Switch the active theme by display name (`view.theme.<NAME>`): find its
    /// index in the theme list and apply it. No-op (notified) if unknown.
    fn switch_theme_by_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let index = self
            .theme_manager
            .borrow()
            .themes()
            .iter()
            .position(|t| t.name == name);
        match index {
            Some(i) => self.switch_theme(i, window, cx),
            None => self.notify(format!("Unknown theme: {name}"), window, cx),
        }
    }

    /// View ▸ Presentation Mode — toggle the presentation flag + its ✓. The chrome
    /// dimming lands with its own pass; here the state + checkmark are live.
    fn toggle_presentation(&mut self, cx: &mut Context<Self>) {
        self.presentation = !self.presentation;
        let on = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.presentation", on, cx);
        });
        cx.notify();
    }

    /// Replace the active document's data source + reflect the tab icon
    /// (`source.clear` / source picks). Updates both the document area and state.
    fn set_active_source(
        &mut self,
        source: super::state::DataSource,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(id, source.clone(), cx);
            });
            self.state.set_source(id, source);
            cx.notify();
        }
    }

    /// Undo / redo on the active editor (menu Edit▸Undo/Redo). No-op if no
    /// editor is active.
    fn active_editor_undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                if redo {
                    ed.redo(cx);
                } else {
                    ed.undo(cx);
                }
            });
            self.rebuild_workspace(cx);
            cx.notify();
        }
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

    // ── Memory-scanner pop-out (the C++ summoned-on-demand scanner window) ──

    /// `true` when the bottom memory-scanner dock is currently open. Drives the
    /// View ▸ Memory Scanner checkmark.
    pub fn scanner_open(&self, cx: &App) -> bool {
        self.dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Bottom, cx)
    }

    /// Toggle the bottom memory-scanner dock open/closed (the C++ scanner pop-out,
    /// summoned via View ▸ Memory Scanner / `Ctrl+Shift+M`). The dock is closed by
    /// default (see [`docks::build_default_layout`]), so the first invocation shows
    /// it. Mirrors [`Self::set_left_dock_open`] but for [`DockPlacement::Bottom`],
    /// and refreshes the menu-bar checkmark so the menu reflects the new state.
    pub fn toggle_scanner_dock(
        &mut self,
        _: &ToggleScanner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.scanner_open(cx);
        self.set_bottom_dock_open(!open, window, cx);
        self.sync_scanner_menu_checked(cx);
        cx.notify();
    }

    /// Open/close the bottom scanner dock to a specific state (drives `set_open`
    /// on the underlying `Dock`). Mirrors [`Self::set_left_dock_open`].
    fn set_bottom_dock_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if area.is_dock_open(DockPlacement::Bottom, cx) != open {
                if let Some(dock) = area.bottom_dock().cloned() {
                    dock.update(cx, |d, cx| d.set_open(open, window, cx));
                }
            }
        });
    }

    /// Push the scanner-open state into the menu bar so View ▸ Memory Scanner
    /// renders a checkmark while the dock is visible (the C++ checkable action).
    fn sync_scanner_menu_checked(&mut self, cx: &mut Context<Self>) {
        let open = self.scanner_open(cx);
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.scanner", open, cx);
        });
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
                // The tab set grew — re-observe so the new editor's selections
                // refresh the status bar.
                self.observe_editors(cx);
            }
            DocAreaEvent::Closed(id) => {
                self.state.close_document(id);
                self.rebuild_workspace(cx);
                self.observe_editors(cx);
            }
            DocAreaEvent::ViewModeChanged(id, mode) => {
                self.state.set_view_mode(id, mode);
            }
        }
        cx.notify();
    }

    /// (Re)subscribe an observation to every open editor so a row selection
    /// re-renders the window — which re-runs [`StatusInfo::for_controller`] and
    /// repaints the bottom status bar with the live `Root.field  +0xNN` readout.
    ///
    /// The editors own the selection and `cx.notify()` themselves on a click but
    /// do not emit up to the window; an `observe` bridges that so a nested-entity
    /// change forces our (sibling) status bar to refresh. Called on construction
    /// and whenever the tab set changes (new/closed document); the previous
    /// observations are dropped (and thus unsubscribed) by reassigning the `Vec`.
    fn observe_editors(&mut self, cx: &mut Context<Self>) {
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        self.editor_observers = editors
            .into_iter()
            .map(|editor| cx.observe(&editor, |_this, _editor, cx| cx.notify()))
            .collect();
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
            // ROUTE EACH CARD DISTINCTLY (the bug: every card landed on a new
            // class). Each card now does its OWN thing — the same operation its
            // File-menu twin runs — after dismissing the splash.
            StartPageEvent::Card(card) => {
                self.dismiss_start_page(cx);
                match card {
                    StartCard::NewClass => self.new_document(window, cx),
                    StartCard::OpenProject => self.prompt_open(window, cx),
                    StartCard::ImportSource => self.prompt_import(ImportKind::Source, window, cx),
                    StartCard::ImportXml => self.prompt_import(ImportKind::Xml, window, cx),
                    StartCard::ImportPdb => self.prompt_import(ImportKind::Pdb, window, cx),
                }
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
        // `updateWindowTitle`).
        let doc_title = self
            .state
            .active_tab()
            .map(|t| t.title.clone())
            .unwrap_or_else(|| "Reclass".to_string());
        let has_doc = !self.state.is_empty();
        let preset = self.layout_preset;

        // The custom titlebar (app-shell §5) with the workspace (sidebar) toggle.
        // The view-mode switch is the "Reclass | Code" segmented control at the
        // bottom of the document area (`tabs::DocumentArea::render_view_toggle`),
        // not a titlebar control (PIC5). The toggle callback re-enters this entity
        // via a weak handle (the idiomatic `App`-scoped closure ⇄ entity bridge).
        let this = cx.entity().downgrade();
        let layout_cb = {
            let this = this.clone();
            move |p: LayoutPreset, window: &mut Window, app: &mut App| {
                let _ = this.update(app, |me, cx| me.apply_layout_preset(p, window, cx));
            }
        };
        let titlebar: TitleBar = titlebar::render_titlebar(
            preset,
            doc_title,
            has_doc,
            self.menubar.clone(),
            layout_cb,
            cx,
        );

        // The bottom status bar's active-node readout (app-shell §11): pull the
        // selection/offset/size from the active editor's controller (a read-only
        // borrow), and the active source from the window state.
        let status_info = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| StatusInfo::for_controller(ed.read(cx).controller()))
            .unwrap_or_default();
        let source = self.state.active_source();
        let status_bar = render_status_bar(&status_info, &source, cx);

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
            .on_action(cx.listener(Self::toggle_scanner_dock))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_goto_address))
            .on_action(cx.listener(Self::on_toggle_modules))
            .on_action(cx.listener(Self::on_toggle_bookmarks))
            .on_action(cx.listener(Self::on_split_editor))
            .on_action(cx.listener(Self::on_unsplit_editor))
            // ── Row 1: the frameless titlebar (app label · menu bar · controls). ──
            .child(titlebar)
            // ── Row 2: the content column — the docking workspace + the
            // start-page overlay. `flex_1 min_h_0` makes it take all the space
            // *between* the titlebar and the status bar; `overflow_hidden` clips
            // the dock (incl. the collapsed bottom-scanner header) strictly to
            // this box so it can NEVER bleed over / displace the status bar below.
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    // The docking workspace: center document tabs + side docks.
                    .child(div().size_full().child(self.dock_area.clone()))
                    // The start-page overlay (over the workspace while shown).
                    .when_some(self.start_page.clone(), |this, page| this.child(page)),
            )
            // ── Row 3: the bottom status bar (app-shell §11) — a thin chrome strip
            // pinned to the very bottom, never shrunk by the flex content above.
            .child(status_bar)
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
    // Toggle the memory-scanner pop-out (the C++ summoned-on-demand scanner;
    // closed by default). Ctrl+Shift+M shows/hides the bottom scanner dock.
    bindings.push(KeyBinding::new(
        "ctrl-shift-m",
        ToggleScanner,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-m",
        ToggleScanner,
        Some("RcxWindow"),
    ));
    // The View-menu shortcuts the contract requires global bindings for, all
    // routed through `run_menu_command` via their action handlers. Ctrl+K is the
    // C++ Command Palette shortcut (in addition to the Zed Ctrl+Shift+P / F1).
    bindings.push(KeyBinding::new(
        "ctrl-k",
        OpenCommandPalette,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-k",
        OpenCommandPalette,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new("f5", RefreshView, Some("RcxWindow")));
    bindings.push(KeyBinding::new(
        "ctrl-g",
        GotoAddressAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-g",
        GotoAddressAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "ctrl-shift-y",
        ToggleModules,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-y",
        ToggleModules,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "ctrl-shift-b",
        ToggleBookmarks,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-b",
        ToggleBookmarks,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new("ctrl-\\", SplitEditor, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-\\", SplitEditor, Some("RcxWindow")));
    bindings.push(KeyBinding::new(
        "ctrl-shift-\\",
        UnsplitEditor,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-\\",
        UnsplitEditor,
        Some("RcxWindow"),
    ));
    cx.bind_keys(bindings);

    // Run borderless: request CLIENT-side decorations so the OS/window-manager
    // (openbox under Xvfb, GNOME/KDE, …) suppresses its own server-side title
    // bar and the app's frameless `TitleBar` is the ONLY chrome — matching the
    // C++ `Qt::FramelessWindowHint` `TitleBarWidget` (titlebar.cpp; app-shell §5).
    // Without this, openbox draws a second (blue gradient) title bar above ours,
    // stealing ~22px and overlapping the top-right view-toggle hit area. On X11
    // gpui sets the `_MOTIF_WM_HINTS` no-decorations hint; gpui-component's
    // `TitleBar` also only renders its min/max/close controls when client-decorated.
    let window_options = WindowOptions {
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    cx.spawn(async move |cx| {
        let window_handle = cx.open_window(window_options, |window, cx| {
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

#[cfg(test)]
mod tests {
    // Headless tests for the pure dispatch helpers added with the action-wiring.
    // These import specific items (NOT `super::*`) so the module's `gpui::*` glob
    // is not pulled into the test-hygiene expansion (see the menubar.rs note).
    use super::{ExportKind, ImportKind, ViewOpt, ViewOptions};

    #[test]
    fn view_options_default_matches_cpp_view_menu() {
        // C++ View menu defaults (view_options.png): everything checked except
        // Comments.
        let d = ViewOptions::default();
        assert!(d.compact_columns);
        assert!(d.tree_lines);
        assert!(d.relative_offsets);
        assert!(d.type_hints);
        assert!(!d.show_comments);
        assert!(d.hover_effects);
        assert!(d.minimap);
    }

    #[test]
    fn view_option_get_set_round_trips_every_variant() {
        let mut o = ViewOptions::default();
        for opt in ViewOpt::ALL {
            let before = o.get(opt);
            o.set(opt, !before);
            assert_eq!(o.get(opt), !before, "{opt:?} did not flip");
            o.set(opt, before);
            assert_eq!(o.get(opt), before, "{opt:?} did not restore");
        }
    }

    #[test]
    fn view_option_command_ids_match_menu_contract() {
        // The ✓-driving ids must be exactly the MENU CONTRACT [check] ids so
        // MenuBar::set_command_checked targets the right rows.
        assert_eq!(ViewOpt::CompactColumns.command_id(), "view.compact_columns");
        assert_eq!(ViewOpt::TreeLines.command_id(), "view.tree_lines");
        assert_eq!(
            ViewOpt::RelativeOffsets.command_id(),
            "view.relative_offsets"
        );
        assert_eq!(ViewOpt::TypeHints.command_id(), "view.type_hints");
        assert_eq!(ViewOpt::ShowComments.command_id(), "view.comments");
        assert_eq!(ViewOpt::HoverEffects.command_id(), "view.hover");
        assert_eq!(ViewOpt::Minimap.command_id(), "view.minimap");
        // Every variant maps to a distinct id.
        let ids: Vec<&str> = ViewOpt::ALL.iter().map(|o| o.command_id()).collect();
        let mut uniq = ids.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(ids.len(), uniq.len(), "command ids must be unique");
    }

    #[test]
    fn export_kind_extensions_are_sensible() {
        assert_eq!(ExportKind::Cpp.extension(), ".h");
        assert_eq!(ExportKind::Rust.extension(), ".rs");
        assert_eq!(ExportKind::Defines.extension(), ".h");
        assert_eq!(ExportKind::CSharp.extension(), ".cs");
        assert_eq!(ExportKind::Python.extension(), ".py");
        assert_eq!(ExportKind::Xml.extension(), ".xml");
    }

    #[test]
    fn import_kind_prompts_are_distinct_and_nonempty() {
        let prompts = [
            ImportKind::Source.prompt(),
            ImportKind::Xml.prompt(),
            ImportKind::Pdb.prompt(),
        ];
        for p in prompts {
            assert!(!p.is_empty());
        }
        assert_ne!(prompts[0], prompts[1]);
        assert_ne!(prompts[1], prompts[2]);
    }
}
