//! The main window shell — the top-level gpui view every UI piece plugs into.
//!
//! Port of the C++ `MainWindow` chrome (app-shell §5 titlebar, §6 window, §8
//! document tabs, §10 docks, §13 start page). Composes:
//!
//! - the custom frameless [`TitleBar`](gpui_component::TitleBar) (app-shell §5)
//!   assembled by [`crate::ui::chrome::titlebar::render_titlebar`]: app label, the in-window
//!   menu bar, the document title, and the workspace sidebar toggle (the
//!   view-mode switch is the document area's bottom segmented control),
//! - a [`DockArea`] holding the MDI document-tab center
//!   ([`DocumentArea`](crate::ui::chrome::tabs::DocumentArea)) + the workspace dock
//!   ([`WorkspacePanel`](crate::ui::panels::workspace::WorkspacePanel)) + a scanner dock,
//!   built by [`crate::ui::panels::docks::build_default_layout`],
//! - the [`StartPage`](crate::ui::chrome::startpage::StartPage) welcome overlay (shown over
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
//! resolves the current theme and applies it via [`crate::ui::theme_apply`].
//!
//! Gated behind the `ui` feature.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{DockArea, DockPlacement};
use gpui_component::notification::Notification;
use gpui_component::{ActiveTheme, Root, TitleBar, WindowExt};

use crate::ui::dialogs::plugin_manager::{
    plugin_infos_from_rows, PluginManagerDialog, PluginManagerEvent,
};

use crate::ui::dialogs::window_dialogs::{
    ConfirmChoice, RcxConfirmDialog, RcxUnsavedDialog, RemoteConnectChoice, RemoteConnectDialog,
    TextPromptDialog, TextPromptEvent, TypeAliasesDialog, TypeAliasesEvent,
};

use crate::theme::{SettingsStore, ThemeManager};
use crate::ui::chrome::menubar::{MenuBar, MenuCommand};
use crate::ui::chrome::startpage::{RecentEntry, StartPage, StartPageEvent};
use crate::ui::chrome::statusbar::{render_status_bar, StatusInfo};
use crate::ui::chrome::tabs::{DocAreaEvent, DocumentArea};
use crate::ui::chrome::titlebar::{self, LayoutPreset};
use crate::ui::panels::bookmarkspanel::BookmarksPanel;
use crate::ui::panels::docks::{self, LayoutHandles, MAIN_DOCK_AREA};
use crate::ui::panels::modulespanel::ModulesPanel;
use crate::ui::panels::targetpanel::TargetPanel;
use crate::ui::panels::workspace::{
    WorkspaceDoc, WorkspaceModel, WorkspaceNav, WorkspaceNewType, WorkspacePanel,
    WorkspaceTypeAction,
};
use crate::ui::state::{AppState, DocId, ViewMode};
use crate::ui::target_status::TargetStatusSummary;
use crate::ui::theme_apply::ThemeRegistryGlobal;

// Cohesive method clusters extracted from the original single `impl MainWindow`
// (mirrors src/ui/editor/): each sibling holds an `impl super::MainWindow` block.
mod dialogs;
mod documents;
mod files;
mod helpers;
mod layout;
mod lifecycle;
mod menus;
mod plugins;
mod settings;
mod startpage;
mod view;
mod workspace;

// The About-dialog GitHub URL lives in `dialogs.rs` (beside `show_about`, the
// sole non-test consumer, which uses it bare via `use super::*`); re-export so
// the tests module's `super::ABOUT_GITHUB_URL` import keeps resolving.
#[cfg(test)]
pub(crate) use dialogs::ABOUT_GITHUB_URL;

// The editor view-option types live in `view.rs` (beside the View-menu logic);
// re-export so `super::ViewOpt`/`super::ViewOptions` keep resolving for the
// `MainWindow::view_opts` field, the sibling menus/layout/workspace globs, and
// the tests module.
pub(crate) use view::{ViewOpt, ViewOptions};

// The File ▸ Import / Export kind enums live in `files.rs` (beside the file-I/O
// logic that consumes them); re-export so `super::ImportKind`/`super::ExportKind`
// keep resolving for the sibling menus/startpage globs and the tests module.
pub(crate) use files::{ExportKind, ImportKind};

// The document-seeding root kind + builder live in `documents.rs` (beside the
// `new_document` op that consumes them); re-export so `super::RootKind`/
// `super::seed_root_doc` keep resolving for the sibling menus/startpage/workspace
// globs and the tests module.
pub(crate) use documents::{seed_root_doc, RootKind};

// The pure tree/file-sniff helpers live in `helpers.rs` (beside `path_is_reclass_xml`,
// which calls `sniff_is_reclass_xml`); re-export so `root_name_for_title` keeps
// resolving for `dirty_doc_name` here + the sibling files/workspace/lifecycle globs,
// and both keep resolving for the tests module's `super::` imports.
pub(crate) use helpers::root_name_for_title;
#[cfg(test)]
pub(crate) use helpers::sniff_is_reclass_xml;

// The close-flow / window-title helpers live in `lifecycle.rs` (beside the
// unsaved-changes guard + window-title computation that consume them); re-export
// so the tests module's `super::` imports keep resolving (they are used bare from
// lifecycle.rs via `use super::*`).
#[cfg(test)]
pub(crate) use lifecycle::{
    dirty_doc_name, unique_dirty_names, unsaved_changes_text, window_title_string,
};

// The menu-tree mutation helpers live in `menus.rs` (beside `rebuild_menus`, the
// sole non-test caller, which uses them bare via `use super::*`); re-export so the
// tests module's `super::relabel_command` / `super::inject_plugin_menu_items`
// imports keep resolving.
#[cfg(test)]
pub(crate) use menus::{inject_plugin_menu_items, relabel_command};

// The plugin-host dock-side mapper lives in `plugins.rs` (beside `mount_plugin_panels`,
// the sole non-test caller, which uses it bare via `use super::*`); re-export so the
// tests module's `super::dock_placement_for` imports keep resolving.
// (`surface_command_toast` stays private to plugins.rs — no test/external refs.)
#[cfg(test)]
pub(crate) use plugins::dock_placement_for;

// The disk-backed settings store + the QSettings key-name constants live in
// `settings.rs`. `DiskSettings` is public API consumed across the crate (the
// window owns it; `ui/plugins/pluginhost.rs` imports `crate::ui::window::DiskSettings`),
// so it is re-exported **publicly**. `settings_keys` is re-exported `pub(crate)`
// so the siblings' `settings_keys::FOO` (via `use super::*` glob) and the tests
// module's `super::settings_keys` import keep resolving.
pub(crate) use settings::settings_keys;
pub use settings::DiskSettings;

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
        ToggleTarget,
        ToggleBookmarks,
        SplitEditor,
        UnsplitEditor,
        // File/Edit accelerators advertised in the menu but previously unbound
        // (the blocker fix). Each routes back through `run_menu_command` so the
        // keyboard, the menu bar, and the command palette share one dispatch.
        NewClassAction,
        NewStructAction,
        NewEnumAction,
        OpenFileAction,
        SaveAction,
        SaveAsAction,
        CloseDocAction,
        UndoAction,
        RedoAction,
        AddBookmarkAction,
        QuickBookmarkAction,
        ShortcutsAction,
        // Tools accelerators advertised in the menu but previously unbound — the
        // C++ binds QKeySequence(Ctrl|Shift|R) for the RTTI Browser (main.cpp:1524)
        // and QKeySequence(Ctrl|Shift|F) for the Performance Profiler
        // (main.cpp:1565). Each routes back through `run_menu_command`.
        RttiAction,
        ProfilerAction,
        // Alt+letter menu-bar mnemonics (the C++ `QMenuBar` Alt accelerators):
        // Alt+F File, Alt+E Edit, Alt+V View, Alt+T Tools, Alt+P Plugins, Alt+H
        // Help. Each toggles the matching top-level menu open/closed.
        OpenMenuFile,
        OpenMenuEdit,
        OpenMenuView,
        OpenMenuTools,
        OpenMenuPlugins,
        OpenMenuHelp
    ]
);

/// One entry in the window's custom centered-modal stack — a focused popup
/// rendered OVER the whole window by [`MainWindow::render_modal_layer`] with NO
/// competing `focus_trap`. This is the in-house replacement for gpui-component's
/// `window.open_dialog` for our keyboard-driven popups (command palette, type /
/// enum / source pickers): `open_dialog`'s `Dialog` installs a `focus_trap` on
/// its OWN focus handle (dialog.rs:526) that swallows the inner popup's
/// `capture_key_down`, so Enter hit the dialog's `ConfirmDialog`→close and the
/// arrow keys did nothing. Here the popup's own input handle is focused and
/// nothing competes, so the popup's key handling (list nav + Enter-apply) fires.
struct ModalEntry {
    /// The popup view to render centered (already a self-styled elevated card).
    view: AnyView,
    /// The popup's own focus handle (delegates to its query input) — focused on
    /// open and whenever it becomes the top entry, so keystrokes reach the popup.
    focus: FocusHandle,
    /// Optional fixed card width. `Some` for a popup that sizes via `w_full` + a
    /// `max_w` cap (the command palette → 600); `None` for a self-sizing popup
    /// (the type selector sets its own `w(380)`).
    width: Option<Pixels>,
    /// Whoever held focus when this modal opened — restored when it closes (and the
    /// stack empties) so focus returns to the true opener, not unconditionally to
    /// the active editor. Matters when a modal is opened from a side panel / sidebar
    /// control rather than the editor.
    restore: Option<WeakFocusHandle>,
}

/// The application's root view — the C++ `MainWindow` (app-shell §6).
pub struct MainWindow {
    /// Window-level application state (open docs, active doc, source, selection,
    /// view mode, theme handle). gpui-free + unit-tested (see [`crate::ui::state`]).
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
    /// The right-dock Target/session inspector.
    #[allow(dead_code)]
    target: Entity<TargetPanel>,
    /// The right-dock Bookmarks panel (View ▸ Bookmarks; `Ctrl+Shift+B`). Shares
    /// the right dock's tab strip with [`modules`](Self::modules) and
    /// [`target`](Self::target).
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
    /// The custom centered-modal overlay stack (command palette / type selector /
    /// enum + source pickers), rendered by [`render_modal_layer`](Self::render_modal_layer)
    /// OVER the window. Replaces `window.open_dialog` for keyboard-driven popups
    /// whose inner key handling the dialog's `focus_trap` used to swallow (the
    /// #1/#2 fix). Topmost is last; the top entry owns click-outside-to-dismiss and
    /// holds focus.
    modal_stack: Vec<ModalEntry>,
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
    /// Re-entrancy guard for the window-close path (the C++ `ClosingGuard
    /// m_closingAll`; app-shell.md:167). [`guarded_window_close`](Self::guarded_window_close)
    /// shows the async unsaved-changes dialog and aborts the in-flight close;
    /// the chosen Save/Discard branch then performs the *programmatic* close —
    /// which fires the same OS/titlebar hook again. This flag is set before that
    /// re-close so the second pass bypasses the prompt (mirrors the C++ guard that
    /// suppresses the re-prompt while closing-all is in progress).
    closing: bool,
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
    /// Most-recently-opened project paths (the C++ `recentFiles` QSettings list;
    /// main.cpp:8765). Most-recent-first, deduped, capped at 10. Drives both the
    /// File ▸ Recent Files submenu and the start-page recent list.
    recent_files: Vec<std::path::PathBuf>,
    /// The active editor font family (View ▸ Font; the C++ `setEditorFont` /
    /// settings("font"), main.cpp:1300). Persisted; drives the Font submenu ✓.
    editor_font: String,
    /// Whether the MCP bridge is "running" (Tools ▸ Start/Stop MCP Server). The
    /// C++ toggles `m_mcp` start/stop + flips the action label; here it owns the
    /// flag + drives the dynamic menu label. The UI does not embed a live bridge
    /// (the MCP server runs via the `mcp` feature, not a platform-specific path).
    mcp_running: bool,
    /// Recent Go-to-Address formulas (the C++ `GotoAddressDialog` recent list).
    /// Most-recent-first, deduped, capped. Loaded into the dialog on open and
    /// pushed on accept. Persisted via [`settings`](Self::settings).
    goto_recent: Vec<String>,
    /// The disk-backed app settings store (the C++ `QSettings("Reclass",
    /// "Reclass")` replacement). Owns recent-files / font / view-toggle
    /// persistence across launches. Shared, interior-mutable so async closures
    /// (file pickers) can persist after the borrow of `self` ends.
    settings: Rc<RefCell<DiskSettings>>,
    /// The session-owned [`PluginManager`](crate::plugin::PluginManager) (design
    /// §7.A [fix] / §H): the single source the source pickers + the Manage-Plugins
    /// dialog read, replacing the throwaway `with_builtins()` managers each live
    /// site used to build. Owned by value (not `Rc<RefCell<_>>`): every live read —
    /// the process picker, the Manage-Plugins dialog, the dialog's enable/disable
    /// toggle — runs inside a `&mut self` window method (synchronous), not an async
    /// closure, so a plain field suffices. Its persistence is the
    /// [`DiskPluginPersistence`](crate::plugin::DiskPluginPersistence) over the same
    /// `settings.json`-backed store as [`settings`](Self::settings), so enable/disable
    /// survives a restart; with no stored flags it is byte-identical to the four
    /// Auto-enabled built-ins (the parity guarantee).
    plugin_manager: crate::plugin::PluginManager,
    /// The bottom-dock memory [`ScannerPanel`] handle (the C++ summon-on-demand
    /// scanner). Held so the window can feed it the active document's provider —
    /// previously dropped (`..` in the `LayoutHandles` destructure), so the
    /// scanner could never scan.
    scanner: Entity<crate::ui::panels::scannerpanel::ScannerPanel>,
    /// Live subscription to an open Tools ▸ Options dialog — kept so its
    /// Apply/Cancel events fire while shown (mirrors [`goto_sub`](Self::goto_sub)).
    options_sub: Option<Subscription>,
    /// Live subscription to an open View ▸ Edit Theme… [`ThemeEditor`] dialog —
    /// kept so its Saved/Cancelled events fire while shown (the dedicated theme
    /// editor; the C++ `editTheme`).
    theme_editor_sub: Option<Subscription>,
    /// Which right-dock tab the user last raised (Target vs Modules vs Bookmarks).
    /// The right dock tabifies all three panels (one `Dock` open flag), so the View ✓
    /// for each must be driven from THIS (which tab is active) AND the dock's
    /// visibility — not from the shared open flag alone (the C++ ties each ✓ to
    /// its own dock's visibility; item 9).
    right_dock_panel: RightDockPanel,
    /// Pinned type node-ids (the C++ `m_pinnedIds`) — drives the workspace PINNED
    /// section; toggled via the project-explorer Pin/Unpin context action.
    pinned_ids: std::collections::HashSet<u64>,
    /// Persisted MCP autostart preference (the C++ `autoStartMcp` setting). Owned
    /// here so Options can toggle + persist it; the MCP label reflects
    /// [`mcp_running`](Self::mcp_running) which this seeds on startup.
    auto_start_mcp: bool,
    /// Persisted generated-code brace-wrap + size-assert prefs (the C++
    /// `braceWrap` / `generatorAsserts`). Applied to code export; persisted via
    /// the disk store.
    brace_wrap: bool,
    generator_asserts: bool,
    /// Persisted refresh interval (ms) (the C++ `refreshMs`). Pushed into the
    /// active controllers when Options applies; persisted via the disk store.
    refresh_ms: i32,
    /// Whether the menu-bar top-level titles are upper-cased (the C++
    /// `m_menuBarTitleCase`; main.cpp:988). Pushed into [`menubar`](Self::menubar)
    /// via [`MenuBar::set_title_case`] and persisted on Options apply.
    menu_bar_title_case: bool,
    /// Whether the titlebar shows the app icon (the C++ `showIcon`; main.cpp:990).
    /// Owned + persisted here so Options can toggle it; the titlebar has no icon
    /// slot in this port yet, so this is faithfully persisted but visually inert.
    show_icon: bool,
    /// The **extra** editor split panes beyond the primary document pane (the C++
    /// `TabState::panes` minus the primary `panes[0]`). Each carries its own
    /// [`ViewMode`] (Tree / rendered C/C++) and views the SAME active document as
    /// the primary pane (the C++ `createSplitPane` binds the pane to the tab's
    /// controller). Empty ⇒ the editor is unsplit (the default single-pane view).
    /// `view.split` appends a pane; `view.unsplit` removes the last.
    split_panes: Vec<ViewMode>,
    /// The mounted plugin [`PluginPanel`](crate::ui::plugins::pluginpanel::PluginPanel) views,
    /// keyed by the contributed panel id (design §6 Phase 2). One per enabled
    /// `Panel` contribution, built in [`new`](Self::new) after the demo gating and
    /// docked into the existing dock area. Empty in the default shipping build (no
    /// plugin contributes a panel ⇒ no extra dock view ⇒ byte-identical UI). Held so
    /// [`rerender_plugin_panel`](Self::rerender_plugin_panel) can push a fresh tree.
    plugin_panels: Vec<(String, Entity<crate::ui::plugins::pluginpanel::PluginPanel>)>,
    /// Live subscriptions to the mounted plugin panels' events — kept for the
    /// window's lifetime so a panel's [`PluginPanelEvent`] keeps routing through the
    /// manager (a dropped `Subscription` stops firing). Index-independent of
    /// `plugin_panels` (just a retention bag).
    plugin_panel_subs: Vec<Subscription>,
}

/// Which panel is active in the shared right dock (Modules vs Bookmarks). Used
/// to drive each panel's independent View-menu ✓ (item 9): the two panels share
/// one `Dock`, so the checkmark must reflect which tab is raised, not just the
/// dock's open state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RightDockPanel {
    Target,
    Modules,
    Bookmarks,
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
            target,
            bookmarks,
            // KEEP the scanner handle (previously dropped via `..`): the window
            // must feed it the active document's provider + wire result rows, or
            // it can never scan. Any future handles still tolerate `..`.
            scanner,
            ..
        } = docks::build_default_layout(&dock_area, window, cx);

        // Open the disk-backed app settings store (the QSettings replacement)
        // and load the persisted across-launch state from it.
        let settings = Rc::new(RefCell::new(DiskSettings::open_default()));
        // The session-owned plugin manager (design §7.A [fix] / §H). Built via
        // `with_persistence_and_builtins` so the DiskSettings-backed persistence is
        // installed BEFORE the built-ins are added — any previously-disabled
        // built-in is restored disabled at add time (set_persistence *after*
        // with_builtins would NOT retro-apply stored flags). The persistence shares
        // the SAME `settings.json` store as the rest of the window (coerced to the
        // shared `SettingsStore` trait object). With no stored flags the registry is
        // exactly the four Auto-enabled built-ins — byte-identical to before.
        let mut plugin_manager = crate::plugin::PluginManager::with_persistence_and_builtins(
            Box::new(crate::plugin::DiskPluginPersistence::new(
                settings.clone() as Rc<RefCell<dyn SettingsStore>>
            )),
        );
        // Developer affordance for the F3 declarative-UI host (design §6 Phase 2):
        // gate the in-tree demo plugin behind `RECLASS_DEMO_PLUGIN`. UNSET (the
        // default shipping build) leaves the manager at EXACTLY the built-in
        // providers — no plugin contributes UI, so the Plugins menu, docks, and
        // modals are byte-for-byte identical to before (the HARD PARITY guarantee).
        // SET (=anything) adds the demo, which contributes the two menu commands, a
        // right-dock panel, and a target dialog the wiring below mounts + routes.
        if std::env::var_os("RECLASS_DEMO_PLUGIN").is_some() {
            plugin_manager.add_plugin(crate::plugin::DemoPlugin::boxed());
        }
        // F4: startup folder-scan discovery (design §6 Phase 3, §7.C [+]; the C++
        // deferred `LoadPlugins()` folder scan). ONLY under `plugins`: scan
        // discovery::default_plugin_dirs() (<exe>/plugins + ~/.config/reclass/plugins),
        // sniff+route each .so/.dll/.dylib, and register every loaded plugin through
        // the SAME add_plugin flow (so its providers join the one shared registry the
        // pickers read, and persisted enabled flags + load=manual are honored by
        // add_plugin). Failures are retained in load_errors() for the dialog (§7.A
        // [fix]). default_plugin_dirs() filters to EXISTING dirs, so an absent/empty
        // plugins dir = no attempts, load_errors empty, registry unchanged — byte
        // parity. Builds without `plugins` do NOT compile this: no scan.
        #[cfg(feature = "plugins")]
        {
            let failures = plugin_manager.load_native_plugins_from_default_dirs();
            if !failures.is_empty() {
                tracing::warn!(
                    count = failures.len(),
                    "plugin discovery: load failure(s); see Manage Plugins for detail"
                );
            }
        }
        let recent_files: Vec<std::path::PathBuf> = settings
            .borrow()
            .get_list(settings_keys::RECENT_FILES)
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        let editor_font = settings
            .borrow()
            .get(settings_keys::FONT)
            .filter(|s| !s.is_empty())
            // The C++ default font is JetBrains Mono (main.cpp:1311).
            .unwrap_or_else(|| "JetBrains Mono".to_string());
        let view_opts = ViewOptions::load(&settings.borrow());
        let goto_recent = crate::ui::dialogs::gotoaddress::load_recent(&*settings.borrow());
        // Tools ▸ Options persisted prefs (the C++ refreshMs/autoStartMcp/
        // braceWrap/generatorAsserts QSettings keys).
        let (auto_start_mcp, brace_wrap, generator_asserts, refresh_ms) = {
            let s = settings.borrow();
            (
                s.get_bool(settings_keys::AUTO_START_MCP, true),
                s.get_bool(settings_keys::BRACE_WRAP, false),
                s.get_bool(settings_keys::GENERATOR_ASSERTS, false),
                s.get(settings_keys::REFRESH_MS)
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(crate::ui::dialogs::optionsdialog::REFRESH_DEFAULT),
            )
        };
        // Appearance prefs (the C++ `menuBarTitleCase` / `showIcon`; main.cpp:988).
        // Both default `false`, matching the C++ `value(key, false)` reads.
        let (menu_bar_title_case, show_icon) = {
            let s = settings.borrow();
            (
                s.get_bool(settings_keys::MENU_BAR_TITLE_CASE, false),
                s.get_bool(settings_keys::SHOW_ICON, false),
            )
        };
        // Code-view generator selectors (the C++ `codeFormat`/`codeScope`; both
        // default index 0; main.cpp:2415/2441). Decoded into the generator enums.
        let (code_format, code_scope) = {
            let s = settings.borrow();
            let fmt_idx = s
                .get(settings_keys::CODE_FORMAT)
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);
            let scope_idx = s
                .get(settings_keys::CODE_SCOPE)
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);
            (
                crate::generator::CodeFormat::from_index(fmt_idx),
                crate::generator::CodeScope::from_index(scope_idx),
            )
        };

        // Seed window state with the initial document tab (the C++ "never leave a
        // blank window"; app-shell §8 step 9). The center `DocumentArea` already
        // created its first editor tab; mirror it into `AppState`.
        let mut state = AppState::new();
        state.open_document("Untitled");

        // The left workspace dock is open by default (Layout_Workspace); reflect
        // that in the toggle preset.
        let layout_preset =
            LayoutPreset::for_visible(dock_area.read(cx).is_dock_open(DockPlacement::Left, cx));

        // Apply the active theme to gpui-component (themes.md §4.16). The
        // ThemeManager global is seeded with a fresh in-memory store on every
        // launch (so its persisted "theme" key is lost across restarts); restore
        // the selection from OUR disk store here so the saved theme survives a
        // relaunch. Record the resulting active theme name in the state.
        {
            let saved_theme = settings.borrow().get(settings_keys::THEME);
            let mut tm = theme_manager.borrow_mut();
            if let Some(name) = saved_theme {
                if let Some(idx) = tm.themes().iter().position(|t| t.name == name) {
                    tm.set_current(idx);
                }
            }
            let current = tm.current().clone();
            state.set_theme_name(&current.name);
            crate::ui::theme_apply::apply_theme(&current, window, cx);
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

        // Seed the code-view generator selectors from the persisted settings (the
        // C++ `fmtCombo`/`scopeCombo` `setCurrentIndex(value(...))`; main.cpp:2415/
        // 2441) plus the `generatorAsserts` flag (main.cpp:5453) so the live code
        // view honors all three from the first paint.
        document_area.update(cx, |area, cx| {
            area.set_generator_options(code_format, code_scope, generator_asserts, cx);
        });

        // ── Wire workspace quick-navigation (app-shell §10 "Open in Current Tab"). ──
        cx.subscribe_in(
            &workspace,
            window,
            |this, _ws, nav: &WorkspaceNav, window, cx| {
                this.on_workspace_nav(*nav, window, cx);
            },
        )
        .detach();

        // ── Wire the workspace empty-area "New …" context menu (right-click the
        // project background / an empty project; the C++ newClass()/newStruct()/
        // newEnum()) → the same path as File ▸ New …. ──
        cx.subscribe_in(
            &workspace,
            window,
            |this, _ws, req: &WorkspaceNewType, window, cx| {
                let cmd = match req {
                    WorkspaceNewType::Class => "file.new_class",
                    WorkspaceNewType::Struct => "file.new_struct",
                    WorkspaceNewType::Enum => "file.new_enum",
                };
                this.run_menu_command(&cmd.to_string(), window, cx);
            },
        )
        .detach();

        // ── Wire the workspace type-row right-click mutations (the C++ workspace
        // `QMenu`: Rename / Duplicate / Delete / Add Member). The panel is a
        // read-only surface, so it raises the intent for the window to resolve
        // against the owning document's live controller. ──
        cx.subscribe_in(
            &workspace,
            window,
            |this, _ws, ev: &crate::ui::panels::workspace::WorkspaceTypeAction, window, cx| {
                this.on_workspace_type_action(ev.clone(), window, cx);
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

        // ── Wire the bookmarks dock row actions (the C++ bookmark-row click →
        // navigate, right-click → Navigate/Remove). ──
        cx.subscribe_in(
            &bookmarks,
            window,
            |this, _bm, ev: &crate::ui::panels::bookmarkspanel::BookmarkAction, window, cx| {
                this.on_bookmark_action(ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Wire the scanner dock result-row navigation (the C++ result-row
        // double-click → jump the editor to the address). ──
        cx.subscribe_in(
            &scanner,
            window,
            |this, _sc, ev: &crate::ui::panels::scannerpanel::ScannerNav, window, cx| {
                this.navigate_active_editor_to_address(ev.address, window, cx);
            },
        )
        .detach();

        // ── Wire the scanner result-cell inline edits (the C++ `onCellEdited`):
        // the Address cell re-evaluates the expression + re-reads, the Value cell
        // writes the typed value back through the active provider. The panel is a
        // read-only surface (it holds only an `Arc` provider clone), so it raises
        // the intent for the window to resolve against its mutable controller. ──
        cx.subscribe_in(
            &scanner,
            window,
            |this, sc, ev: &crate::ui::panels::scannerpanel::ScannerEdit, window, cx| {
                this.on_scanner_edit(sc.clone(), ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Scanner "Add as Nodes" (the C++ drag-result-into-editor / add-as-nodes
        // path): append a node per address into the active editor's container.
        // Previously emitted but unsubscribed, so the button only set a status and
        // appended nothing (item 6). ──
        cx.subscribe_in(
            &scanner,
            window,
            |this, _sc, ev: &crate::ui::panels::scannerpanel::ScannerAddNodes, window, cx| {
                this.on_scanner_add_nodes(&ev.addresses, window, cx);
            },
        )
        .detach();

        // ── Scanner "Change All Values" (the C++ batch write): write `bytes` to
        // every result address through the mutable provider, re-read each, then
        // hand the results back so the panel produces the "Wrote to X/Y" tail.
        // Previously emitted but unsubscribed, so it never wrote (item 6). ──
        cx.subscribe_in(
            &scanner,
            window,
            |this, sc, ev: &crate::ui::panels::scannerpanel::ScannerBatchEdit, window, cx| {
                this.on_scanner_batch_edit(sc.clone(), ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Wire the modules dock row activation (double-click → open/reuse a
        // module-root class at that module's image base). ──
        cx.subscribe_in(
            &modules,
            window,
            |this, _md, ev: &crate::ui::panels::modulespanel::ModuleAction, window, cx| match ev {
                crate::ui::panels::modulespanel::ModuleAction::Activate { base, name } => {
                    this.jump_active_editor_to_module_base(*base, name, window, cx);
                }
                crate::ui::panels::modulespanel::ModuleAction::DownloadAll => {
                    this.download_all_module_symbols(window, cx);
                }
            },
        )
        .detach();

        // ── Window focus drives the controller's adaptive refresh throttle
        // (slow on blur; the C++ `setWindowState(focused, visible)`;
        // controller.cpp:5474). gpui has no separate "minimized" signal here, so
        // we map activation → focused and treat the window as visible while it
        // exists; a blurred window backs off the read cadence. ──
        cx.observe_window_activation(window, |this, window, cx| {
            let focused = window.is_window_active();
            this.set_controllers_window_state(focused, true, cx);
            // On window blur, dismiss the editor's open fly-out popups (the C++
            // `changeEvent` → `editor.dismissAllPopups()` per pane when the window
            // becomes inactive; item 10). The editor's type-selector / root-type /
            // format fly-outs are opened as window (`Root`) dialogs, so closing
            // the dialog layer collapses them — the user-visible "popups close on
            // blur" behaviour.
            if !focused {
                window.close_all_dialogs(cx);
            }
        })
        .detach();

        // ── OS/WM window-close interception (the C++ `closeEvent`; main.cpp:8984).
        // gpui's `on_window_should_close` is the Alt+F4 / `WM_DELETE_WINDOW` hook:
        // returning `false` aborts the close (Qt `event->ignore()`), `true` allows
        // it (Qt `event->accept()`). Route it through the shared unsaved-changes
        // guard. The hook is fire-and-forget on the platform window (no
        // Subscription to retain); it re-enters this entity via a weak handle and
        // defaults to allowing the close if the entity is already gone. ──
        let close_entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            close_entity
                .update(cx, |this, cx| this.guarded_window_close(window, cx))
                .unwrap_or(true)
        });

        let mut win = MainWindow {
            state,
            dock_area,
            document_area,
            workspace,
            modules,
            target,
            bookmarks,
            menubar,
            start_page: None,
            layout_preset,
            theme_manager,
            palette_sub: None,
            modal_stack: Vec::new(),
            editor_observers: Vec::new(),
            goto_sub: None,
            // Not closing yet (the C++ `m_closingAll` starts false).
            closing: false,
            presentation: false,
            // View toggles, recent files, the editor font, and go-to recents are
            // loaded from the disk store above (persisted across launches).
            view_opts,
            recent_files,
            editor_font,
            // The MCP "running" flag is seeded from the persisted autostart pref
            // (the C++ `autoStartMcp` drives the initial Start/Stop label).
            mcp_running: auto_start_mcp,
            goto_recent,
            settings,
            plugin_manager,
            scanner,
            options_sub: None,
            theme_editor_sub: None,
            // The right dock defaults to the Target tab (the first tabified
            // panel; docks.rs builds [target, modules, bookmarks]).
            right_dock_panel: RightDockPanel::Target,
            pinned_ids: std::collections::HashSet::new(),
            auto_start_mcp,
            brace_wrap,
            generator_asserts,
            refresh_ms,
            menu_bar_title_case,
            show_icon,
            // The editor starts unsplit (single pane); `view.split` appends panes.
            split_panes: Vec::new(),
            // Plugin panels are mounted just below (after the window is built so the
            // mount can subscribe to `self`). Empty until a plugin contributes UI.
            plugin_panels: Vec::new(),
            plugin_panel_subs: Vec::new(),
        };

        // Observe the initial editor(s) so a row selection re-renders the window
        // (and thus refreshes the status bar; see [`Self::observe_editors`]).
        win.observe_editors(window, cx);
        // Push the active font family into the Font submenu ✓, mark the active
        // theme, and rebuild the dynamic menus (Recent Files / Data Source / MCP
        // label) on first paint.
        win.sync_font_menu_checked(cx);
        win.sync_theme_menu_checked(cx);
        win.rebuild_menus(cx);
        // Push the persisted menu-bar title-case preference (the C++
        // `applyMenuBarTitleCase(m_menuBarTitleCase)` on startup; main.cpp:989).
        let title_case = win.menu_bar_title_case;
        win.menubar
            .update(cx, |mb, cx| mb.set_title_case(title_case, cx));
        // Reflect the initial scanner-dock state in the View menu (closed by
        // default ⇒ View ▸ Memory Scanner starts unchecked).
        win.sync_scanner_menu_checked(cx);
        // Push the initial checkable-View-menu states into the menu bar so the
        // View menu reflects reality on first open (the C++ defaults: every view
        // option checked except Comments; the Project dock open; the right-dock
        // panels + presentation closed). Mirrors `sync_scanner_menu_checked`.
        win.sync_view_menu_checked(cx);
        // Realign the initial editor tab(s) to the persisted view options (the
        // C++ applies each saved view setting to the first editor on startup).
        let initial_editors: Vec<Entity<crate::ui::editor::RcxEditor>> = win
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in &initial_editors {
            win.apply_view_opts_to_editor(editor, cx);
        }
        // Rebuild the workspace model from the seeded document, then show the
        // start page over the workspace (the C++ deferred `showStartPage`).
        win.rebuild_workspace(cx);
        // Seed the docks for the initial (empty) document.
        win.refresh_docks_for_active(cx);
        // Mount any enabled plugin-contributed dock panels (design §6 Phase 2). A
        // no-op when only provider plugins are loaded, so the dock layout is
        // byte-identical; with a contributing plugin (the demo,
        // gated above) it adds a tab into the existing right dock, which stays
        // CLOSED until summoned — no behavior change on launch.
        win.mount_plugin_panels(window, cx);
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

    /// Open the command palette (Ctrl+Shift+P / Ctrl+K — Zed + C++ parity; F1 is
    /// the Help ▸ Keyboard Shortcuts accelerator). Builds a fresh palette over the
    /// menu tree, shows it in the gpui-component dialog layer, and routes its
    /// Trigger/Cancel back here (close, then dispatch the command).
    /// Push `view` onto the centered-modal stack and focus its `focus` handle, then
    /// re-render so [`render_modal_layer`](Self::render_modal_layer) paints it OVER
    /// the window. The in-house replacement for `window.open_dialog` for our
    /// keyboard-driven popups: no `focus_trap` competes, so the popup's own
    /// `capture_key_down` (list nav + Enter-apply) actually fires (the #1/#2 fix).
    /// `width` is `Some` for popups that size via `w_full` + a `max_w` cap (the
    /// palette), `None` for self-sizing popups (the type selector).
    pub fn open_centered_modal(
        &mut self,
        view: AnyView,
        focus: FocusHandle,
        width: Option<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Remember who held focus so closing the modal hands it back to the opener
        // (only meaningful for the bottom entry; nested modals restore to the entry
        // beneath them). Capture BEFORE focusing the popup.
        let restore = window.focused(cx).map(|h| h.downgrade());
        self.modal_stack.push(ModalEntry {
            view,
            focus: focus.clone(),
            width,
            restore,
        });
        // Focus the popup's OWN input handle so the first keystroke reaches it (no
        // dialog focus_trap pulls focus back, unlike `open_dialog`).
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Pop the topmost centered modal, restoring focus to the next modal (if the
    /// stack is still non-empty) or the active editor surface. Mirrors
    /// `window.close_dialog` for the in-house stack.
    pub fn close_top_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(closed) = self.modal_stack.pop() else {
            return;
        };
        if let Some(top) = self.modal_stack.last() {
            // A modal is still open beneath — hand focus back to it.
            window.focus(&top.focus, cx);
        } else if let Some(handle) = closed.restore.and_then(|w| w.upgrade()) {
            // Return focus to whoever opened the modal (the editor in the common
            // case, but a side-panel control if that is what was focused).
            window.focus(&handle, cx);
        } else if let Some(editor) = self.document_area.read(cx).active_editor() {
            // Fall back to the active editor if the opener is gone.
            let fh = editor.read(cx).focus_handle(cx);
            window.focus(&fh, cx);
        }
        cx.notify();
    }

    /// Render the centered-modal stack as a full-window overlay: a dimming scrim
    /// (the top entry owns click-outside-to-dismiss) with each popup card centered
    /// horizontally near the top third (the Zed-picker placement). NO `focus_trap`
    /// — the focused popup owns key dispatch. `None` when no modal is open.
    fn render_modal_layer(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.modal_stack.is_empty() {
            return None;
        }
        let top_ix = self.modal_stack.len() - 1;
        let layers: Vec<AnyElement> = self
            .modal_stack
            .iter()
            .enumerate()
            .map(|(ix, entry)| {
                let is_top = ix == top_ix;
                let card = div()
                    .occlude()
                    .mt(px(120.))
                    // A fixed-width popup (palette / source / enum / hex sets only a
                    // `min_w`) is stretched to fill the card, exactly as the dialog's
                    // `v_flex` used to. A `None`-width popup self-sizes (type selector
                    // sets its own `w(380)`), so leave the card content-sized.
                    .when_some(entry.width, |this, w| {
                        this.flex().flex_col().items_stretch().w(w)
                    })
                    // Clicks INSIDE the card must not bubble to the scrim's
                    // close-on-press (selecting a row would otherwise dismiss it).
                    .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                        cx.stop_propagation()
                    })
                    .child(entry.view.clone());
                div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .when(is_top, |this| {
                        this.occlude()
                            .bg(gpui::hsla(0., 0., 0., 0.45))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e: &MouseDownEvent, window, cx| {
                                    this.close_top_modal(window, cx)
                                }),
                            )
                    })
                    .child(card)
                    .into_any_element()
            })
            .collect();
        Some(div().absolute().inset_0().children(layers))
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the OS window title in sync with the active document (the C++
        // `updateWindowTitle`; main.cpp:5172). Every active-tab change, view-root
        // change, and dirty-state change forces a re-render, so recomputing the
        // title here covers all three transitions without threading `window`
        // through each dirty-sync path; `set_window_title` is a cheap no-op when
        // the string is unchanged.
        self.update_window_title(window, cx);

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
        // The in-app titlebar X (gpui-component's Linux close control) must route
        // through the same close guard as the OS/WM close — without this it falls
        // to the default `window.remove_window()` and bypasses the unsaved-changes
        // prompt (title_bar.rs:189-194). `guarded_window_close` returns a bool we
        // ignore here: on Cancel it has already aborted (kept the window open); on
        // Save/Discard the async branch re-closes.
        let close_cb = {
            let this = this.clone();
            move |window: &mut Window, app: &mut App| {
                let _ = this.update(app, |me, cx| me.guarded_window_close(window, cx));
            }
        };
        let titlebar: TitleBar = titlebar::render_titlebar(
            preset,
            doc_title,
            has_doc,
            self.show_icon,
            self.menubar.clone(),
            layout_cb,
            close_cb,
            cx,
        );

        // The bottom status bar's active-node readout (app-shell §11): pull the
        // selection/offset/size from the active editor's controller (a read-only
        // borrow), and the active source from the window state.
        let (status_info, target_summary) = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| {
                let ed = ed.read(cx);
                let ctrl = ed.controller();
                (
                    StatusInfo::for_controller(ctrl),
                    TargetStatusSummary::for_controller(ctrl),
                )
            })
            .unwrap_or_else(|| (StatusInfo::default(), TargetStatusSummary::no_source()));
        let source = self.state.active_source();
        let open_target = {
            let this = cx.entity().downgrade();
            move |window: &mut Window, app: &mut App| {
                let _ = this.update(app, |me, cx| me.raise_target(window, cx));
            }
        };
        let status_bar = render_status_bar(&status_info, &source, &target_summary, open_target, cx);

        // Presentation Mode (View ▸ Presentation Mode): fade the surrounding chrome
        // (titlebar + status bar) so the editor surface reads as the focus, like the
        // C++ `setPresentationMode`. The editor content itself is untouched (full
        // opacity); only the chrome strips dim.
        let presentation = self.presentation;
        let chrome_opacity = if presentation { 0.45 } else { 1.0 };

        // Editor split panes (View ▸ Split Editor). Each extra pane views the same
        // active document as the primary editor (the C++ `SplitPane`s bound to the
        // tab's controller); lay them side-by-side to the right of the dock area.
        let split_panes = self.render_split_panes(cx);

        // Our in-house centered-modal overlay (command palette / type+enum+source
        // pickers), painted OVER the content with no `focus_trap` so the focused
        // popup owns key dispatch. `None` when no modal is open.
        let modal_layer = self.render_modal_layer(cx);

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
            .on_action(cx.listener(Self::on_toggle_target))
            .on_action(cx.listener(Self::on_toggle_bookmarks))
            .on_action(cx.listener(Self::on_split_editor))
            .on_action(cx.listener(Self::on_unsplit_editor))
            // File/Edit accelerators (previously advertised but unbound).
            .on_action(cx.listener(Self::on_new_class))
            .on_action(cx.listener(Self::on_new_struct))
            .on_action(cx.listener(Self::on_new_enum))
            .on_action(cx.listener(Self::on_open_file))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_save_as))
            .on_action(cx.listener(Self::on_close_doc))
            .on_action(cx.listener(Self::on_undo))
            .on_action(cx.listener(Self::on_redo))
            .on_action(cx.listener(Self::on_add_bookmark))
            .on_action(cx.listener(Self::on_quick_bookmark))
            .on_action(cx.listener(Self::on_shortcuts))
            // Tools accelerators: RTTI exists only with `symbols`; Profiler is
            // always present in UI builds.
            .when(cfg!(feature = "symbols"), |el| {
                el.on_action(cx.listener(Self::on_rtti))
            })
            .on_action(cx.listener(Self::on_profiler))
            // Alt+letter menu-bar mnemonics → toggle the matching top-level menu.
            .on_action(
                cx.listener(|this, _: &OpenMenuFile, _w, cx| this.toggle_menu_mnemonic('f', cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenMenuEdit, _w, cx| this.toggle_menu_mnemonic('e', cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenMenuView, _w, cx| this.toggle_menu_mnemonic('v', cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenMenuTools, _w, cx| this.toggle_menu_mnemonic('t', cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenMenuPlugins, _w, cx| this.toggle_menu_mnemonic('p', cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenMenuHelp, _w, cx| this.toggle_menu_mnemonic('h', cx)),
            )
            // ── Row 1: the frameless titlebar (app label · menu bar · controls). ──
            // Dimmed in Presentation Mode (chrome fade).
            .child(div().opacity(chrome_opacity).child(titlebar))
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
                    // The docking workspace (center document tabs + side docks) and,
                    // when the editor is split, the extra panes laid out beside it in
                    // a horizontal row. With no split this is just the dock area at
                    // full width (unchanged single-pane layout).
                    .child(
                        gpui_component::h_flex()
                            .size_full()
                            .items_stretch()
                            // The dock area (incl. the primary editor pane) takes the
                            // remaining width; each split pane shares the rest evenly.
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(self.dock_area.clone()),
                            )
                            .children(split_panes),
                    )
                    // The start-page overlay (over the workspace while shown).
                    .when_some(self.start_page.clone(), |this, page| this.child(page)),
            )
            // ── Row 3: the bottom status bar (app-shell §11) — a thin chrome strip
            // pinned to the very bottom, never shrunk by the flex content above.
            // Dimmed in Presentation Mode (chrome fade).
            .child(div().flex_none().opacity(chrome_opacity).child(status_bar))
            // Overlay layers.
            .children(sheet_layer)
            .children(dialog_layer)
            // Our centered-modal stack sits above Root sheets/dialogs but below
            // notifications (a toast must stay visible over an open modal).
            .children(modal_layer)
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

/// Re-scope the editor surface's bare printable-key accelerators so they do NOT
/// fire while an inline-edit field is focused (see the call site for the full
/// rationale — the systemic inline-edit input bug). A binding is rewritten when it
/// is a single keystroke, carries no ctrl/alt/cmd/fn modifier, and its key is a
/// printable character (a 1-char key like `t`/`f`/`1`, or a named printable such as
/// `space`/`semicolon`) — i.e. exactly the keys a user would type into a field. Its
/// `RcxEditor` predicate becomes `RcxEditor && !RcxFieldInput`; everything else
/// (modified accelerators, navigation/function keys) is passed through unchanged.
fn scope_editor_text_keys_to_non_field(bindings: Vec<KeyBinding>) -> Vec<KeyBinding> {
    // Named keys that produce a printable character (and so must reach the field's
    // text input during an edit) but are spelled as words in the keymap.
    fn is_named_printable(key: &str) -> bool {
        matches!(key, "space" | "semicolon")
    }
    fn is_typeable_key(ks: &Keystroke) -> bool {
        let m = &ks.modifiers;
        // A bare key only — any "command" modifier means it can never be plain text.
        if m.control || m.alt || m.platform || m.function {
            return false;
        }
        // A single Unicode scalar (`t`, `f`, `1`, `;`, …) or a named printable. Shift
        // alone (e.g. `shift-space`) still yields text, so it is allowed.
        ks.key.chars().count() == 1 || is_named_printable(&ks.key)
    }

    bindings
        .into_iter()
        .map(|binding| {
            // Only touch bindings scoped to exactly `RcxEditor` (the editor surface).
            let is_editor_ctx = binding
                .predicate()
                .map(|p| p.to_string() == "RcxEditor")
                .unwrap_or(false);
            let keystrokes = binding.keystrokes();
            let single_typeable_str = (keystrokes.len() == 1
                && is_typeable_key(keystrokes[0].inner()))
            .then(|| keystrokes[0].inner().unparse());
            let Some(keystroke_str) = single_typeable_str.filter(|_| is_editor_ctx) else {
                return binding;
            };
            // Reconstruct with the field-aware predicate, preserving the action and
            // keystroke. `unparse()` round-trips a bare key to a parseable string.
            // Both the inline-edit field (`RcxFieldInput`) and the Ctrl+F search bar
            // (`RcxFindBar`) are text inputs mounted as descendants of the editor div,
            // so a bare accelerator must dodge BOTH — otherwise typing e.g. `f` into
            // the Find box would fire the editor's "quick float" instead of inserting
            // the letter (the same ancestor-context theft as the inline-edit bug).
            let predicate =
                KeyBindingContextPredicate::parse("RcxEditor && !RcxFieldInput && !RcxFindBar")
                    .expect("static predicate parses");
            KeyBinding::load(
                &keystroke_str,
                binding.action().boxed_clone(),
                Some(std::rc::Rc::new(predicate)),
                false,
                binding.action_input(),
                &DummyKeyboardMapper,
            )
            .unwrap_or(binding)
        })
        .collect()
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
    // Resolve the editor monospace family from the platform's installed fonts
    // BEFORE any view shapes text. gpui's `font_family` takes a single family (not a
    // CSS fallback list), so we must pick one real installed mono — otherwise the
    // editor falls back to a proportional font and its fixed-cell column grid
    // (offsets/types/names/values, the inline-edit overlay, and mouse hit-testing)
    // drifts off the painted glyphs.
    crate::ui::design::tokens::font::resolve_mono_family(&cx.text_system().all_font_names());

    let theme_manager = ThemeRegistryGlobal::get(cx);

    // Register the editor-surface + inline-field + start-page key bindings, plus
    // the dialog/popup contexts (command palette + find bar; the dialogs/pickers
    // are opened through the `Root` overlay and own their own key contexts).
    //
    // CROSS-CUTTING INLINE-EDIT FIX (B3/B4 + the systemic "typing does nothing" QA
    // report): the editor surface (`RcxEditor`) binds bare printable keys as quick
    // accelerators — `t` (change type), `f`/`s`/`u`/`p` (quick float/signed/unsigned/
    // pointer), `1`-`5` (hex sizes), `space` (hex cycle), `semicolon` (comment).
    // The inline-edit field (`RcxFieldInput`) is a *descendant* of the editor in the
    // focus/dispatch tree, so when the field is focused gpui's context stack is
    // `[…, RcxEditor, RcxFieldInput]` and those bare-key bindings — registered on the
    // ANCESTOR `RcxEditor` — still MATCH (key_dispatch builds the predicate stack from
    // every node on the dispatch path). A matched binding fires its action and
    // consumes the keystroke, so the character never reaches the field's IME
    // `replace_text_in_range`: typing `f` into a hex byte ran "quick float", typing a
    // name with an `s`/`t`/`u`/`p` silently dropped those letters, etc. That is the
    // root cause behind B3 (root-name rename) and the failing hex `FF` overwrite.
    //
    // Fix: re-scope every bare (no ctrl/alt/cmd) single printable editor accelerator
    // to `RcxEditor && !RcxFieldInput && !RcxFindBar` (the inline field AND the
    // Ctrl+F search box are both text inputs mounted under the editor). gpui
    // evaluates the negations against the FULL context stack, so the binding matches
    // when only the editor is focused but is suppressed the moment a field/find input
    // is open — letting the keystroke fall through to that input. Modified
    // accelerators (Ctrl+D, F2, …) and navigation keys (already shadowed by the
    // field's deeper `RcxFieldInput` bindings) are left untouched.
    let mut bindings =
        scope_editor_text_keys_to_non_field(crate::ui::editor::editor_key_bindings());
    bindings.extend(crate::ui::editor::inline_edit::field_key_bindings());
    bindings.extend(crate::ui::chrome::startpage::start_page_key_bindings());
    bindings.extend(crate::ui::pickers::commandpalette::command_palette_key_bindings());
    bindings.extend(crate::ui::overlays::findbar::find_bar_key_bindings());
    bindings.extend(crate::ui::panels::scannerpanel::scanner_panel_key_bindings());
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
    // F1 opens Help ▸ Keyboard Shortcuts (the C++ QKeySequence(Qt::Key_F1);
    // main.cpp:1580). The command palette keeps Ctrl+Shift+P / Ctrl+K.
    bindings.push(KeyBinding::new("f1", ShortcutsAction, Some("RcxWindow")));
    // Alt+letter menu-bar mnemonics (the C++ `QMenuBar` Alt accelerators): open the
    // matching top-level menu from anywhere in the window.
    bindings.push(KeyBinding::new("alt-f", OpenMenuFile, Some("RcxWindow")));
    bindings.push(KeyBinding::new("alt-e", OpenMenuEdit, Some("RcxWindow")));
    bindings.push(KeyBinding::new("alt-v", OpenMenuView, Some("RcxWindow")));
    bindings.push(KeyBinding::new("alt-t", OpenMenuTools, Some("RcxWindow")));
    bindings.push(KeyBinding::new("alt-p", OpenMenuPlugins, Some("RcxWindow")));
    bindings.push(KeyBinding::new("alt-h", OpenMenuHelp, Some("RcxWindow")));
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

    // ── File/Edit accelerators advertised in the menu but previously unbound
    // (the blocker fix). Each routes to the same command id `run_menu_command`
    // dispatches, so keyboard + menu + palette stay in lockstep. The shortcuts
    // mirror the C++ QKeySequence set (main.cpp:1099-1215). Both the Ctrl (Win/
    // Linux) and Cmd (mac) forms are bound. ──
    bindings.push(KeyBinding::new("ctrl-n", NewClassAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-n", NewClassAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new(
        "ctrl-t",
        NewStructAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new("cmd-t", NewStructAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("ctrl-e", NewEnumAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-e", NewEnumAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("ctrl-o", OpenFileAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-o", OpenFileAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("ctrl-s", SaveAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-s", SaveAction, Some("RcxWindow")));
    // Save As — QKeySequence::SaveAs (Ctrl+Shift+S). Reserved over the C++
    // Memory Scanner accelerator (which the port keeps on Ctrl+Shift+M).
    bindings.push(KeyBinding::new(
        "ctrl-shift-s",
        SaveAsAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-s",
        SaveAsAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new("ctrl-w", CloseDocAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-w", CloseDocAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("ctrl-z", UndoAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-z", UndoAction, Some("RcxWindow")));
    // Redo — QKeySequence::Redo is Ctrl+Y or Ctrl+Shift+Z; bind both (and the
    // mac Cmd-Shift-Z form).
    bindings.push(KeyBinding::new("ctrl-y", RedoAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new("cmd-y", RedoAction, Some("RcxWindow")));
    bindings.push(KeyBinding::new(
        "ctrl-shift-z",
        RedoAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-z",
        RedoAction,
        Some("RcxWindow"),
    ));
    // Bookmarks — Add Bookmark… (Ctrl+B) / Quick Bookmark Here (Ctrl+Alt+B).
    bindings.push(KeyBinding::new(
        "ctrl-b",
        AddBookmarkAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-b",
        AddBookmarkAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "ctrl-alt-b",
        QuickBookmarkAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-alt-b",
        QuickBookmarkAction,
        Some("RcxWindow"),
    ));

    // ── Tools accelerators advertised in the Tools menu but previously unbound:
    // RTTI Browser (the C++ QKeySequence(Ctrl|Shift|R); main.cpp:1524) and
    // Performance Profiler (the C++ QKeySequence(Ctrl|Shift|F); main.cpp:1565).
    // Both shortcuts were dead labels until now. Each routes to its MENU CONTRACT
    // command via the action handler, so keyboard + menu + palette stay in
    // lockstep. The Ctrl (Win/Linux) and Cmd (mac) forms are both bound. ──
    #[cfg(feature = "symbols")]
    {
        bindings.push(KeyBinding::new(
            "ctrl-shift-r",
            RttiAction,
            Some("RcxWindow"),
        ));
        bindings.push(KeyBinding::new(
            "cmd-shift-r",
            RttiAction,
            Some("RcxWindow"),
        ));
    }
    bindings.push(KeyBinding::new(
        "ctrl-shift-f",
        ProfilerAction,
        Some("RcxWindow"),
    ));
    bindings.push(KeyBinding::new(
        "cmd-shift-f",
        ProfilerAction,
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
mod tests;
