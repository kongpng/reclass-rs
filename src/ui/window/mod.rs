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
    ConfirmChoice, RcxConfirmDialog, RcxUnsavedDialog, TextPromptDialog, TextPromptEvent,
    TypeAliasesDialog, TypeAliasesEvent,
};

use crate::ui::panels::bookmarkspanel::BookmarksPanel;
use crate::ui::panels::docks::{self, LayoutHandles, MAIN_DOCK_AREA};
use crate::ui::chrome::menubar::{MenuBar, MenuCommand};
use crate::ui::panels::modulespanel::ModulesPanel;
use crate::ui::chrome::startpage::{RecentEntry, StartPage, StartPageEvent};
use crate::ui::state::{AppState, DocId, ViewMode};
use crate::ui::chrome::statusbar::{render_status_bar, StatusInfo};
use crate::ui::chrome::tabs::{DocAreaEvent, DocumentArea};
use crate::ui::theme_apply::ThemeRegistryGlobal;
use crate::ui::chrome::titlebar::{self, LayoutPreset};
use crate::ui::panels::workspace::{
    WorkspaceDoc, WorkspaceModel, WorkspaceNav, WorkspaceNewType, WorkspacePanel,
    WorkspaceTypeAction,
};
use crate::theme::{SettingsStore, ThemeManager};

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
mod startpage;
mod view;
mod workspace;

// ─────────────────────────────────────────────────────────────────────────────
// DiskSettings — the disk-backed app settings store (the QSettings replacement)
// ─────────────────────────────────────────────────────────────────────────────

/// A JSON-file-backed [`SettingsStore`] — the Rust port's equivalent of the C++
/// `QSettings("Reclass", "Reclass")` shared app settings.
///
/// The C++ app persists everything (recent files, theme, view toggles, editor
/// font, go-to-address recents, refresh interval, MCP autostart, …) through one
/// `QSettings` instance. The port previously had only the in-memory
/// [`MemSettings`](crate::theme::MemSettings), so NOTHING survived a relaunch.
/// This is the single disk-backed store the window owns and threads to every
/// persistence consumer.
///
/// Storage is a flat `key → string` JSON object at
/// `<config_dir>/Reclass/settings.json` (`<config_dir>` from the same
/// `directories::ProjectDirs` the theme manager uses, so the location matches
/// the rest of the app). Reads are served from an in-memory cache; every
/// [`set`](SettingsStore::set) writes the whole object back to disk
/// (small file, infrequent writes — the same write-through `QSettings` does).
/// List-valued keys (recent files, go-to recents) are stored as `\n`-joined
/// strings via [`get_list`](DiskSettings::get_list) / [`set_list`].
pub struct DiskSettings {
    path: std::path::PathBuf,
    map: std::collections::HashMap<String, String>,
}

impl DiskSettings {
    /// The on-disk settings file path — `<config>/Reclass/settings.json`.
    /// `<config>` is `ProjectDirs::config_dir()` for org/app "Reclass"/"Reclass"
    /// (matching [`ThemeManager::default_user_dir`]); falls back to a temp dir.
    pub fn default_path() -> std::path::PathBuf {
        let base = directories::ProjectDirs::from("", "Reclass", "Reclass")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("Reclass"));
        let _ = std::fs::create_dir_all(&base);
        base.join("settings.json")
    }

    /// Open (or create) the disk store at the default path, loading any existing
    /// keys. A missing/corrupt file yields an empty store (the C++ `QSettings`
    /// "fresh defaults" behaviour) rather than failing.
    pub fn open_default() -> Self {
        Self::open_at(Self::default_path())
    }

    /// Open the store at an explicit path (used by tests with a temp file).
    pub fn open_at(path: std::path::PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| {
                serde_json::from_str::<std::collections::HashMap<String, String>>(&s).ok()
            })
            .unwrap_or_default();
        DiskSettings { path, map }
    }

    /// Persist the in-memory map back to disk (whole-object write-through).
    /// Best-effort: a write failure is logged, not propagated (matching the C++
    /// `QSettings` which silently no-ops on an unwritable backing store).
    fn flush(&self) {
        match serde_json::to_string_pretty(&self.map) {
            Ok(text) => {
                if let Some(parent) = self.path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&self.path, text) {
                    tracing::warn!(path = %self.path.display(), error = %e, "settings flush failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "settings serialize failed"),
        }
    }

    /// Read a list-valued key (C++ `QStringList`), stored `\n`-joined. Empty
    /// segments are dropped; a missing key yields an empty list.
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.map
            .get(key)
            .map(|s| {
                s.split('\n')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Write a list-valued key (C++ `QStringList`), stored `\n`-joined, and
    /// flush. Empty entries are dropped so they round-trip with [`get_list`].
    pub fn set_list(&mut self, key: &str, values: &[String]) {
        let joined = values
            .iter()
            .filter(|v| !v.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        self.map.insert(key.to_string(), joined);
        self.flush();
    }

    /// Read a bool key with a default (C++ `value(key, default).toBool()`).
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.map
            .get(key)
            .map(|s| s == "true" || s == "1")
            .unwrap_or(default)
    }

    /// Write a bool key (`"true"`/`"false"`) and flush.
    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.map.insert(
            key.to_string(),
            if value { "true" } else { "false" }.to_string(),
        );
        self.flush();
    }
}

impl SettingsStore for DiskSettings {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
    fn set(&mut self, key: &str, value: &str) {
        self.map.insert(key.to_string(), value.to_string());
        self.flush();
    }
}

/// QSettings key names (the C++ `QSettings(...).value("<key>")` strings) so the
/// Rust store reads/writes the SAME logical keys the C++ app uses. Kept in one
/// place so every consumer agrees on the spelling.
mod settings_keys {
    pub const RECENT_FILES: &str = "recentFiles";
    pub const FONT: &str = "font";
    pub const COMPACT_COLUMNS: &str = "compactColumns";
    pub const TREE_LINES: &str = "treeLines";
    pub const RELATIVE_OFFSETS: &str = "relativeOffsets";
    pub const TYPE_HINTS: &str = "typeHints";
    pub const SHOW_COMMENTS: &str = "showComments";
    pub const HOVER_EFFECTS: &str = "hoverEffects";
    pub const MINIMAP: &str = "minimap";
    pub const THEME: &str = "theme";
    pub const REFRESH_MS: &str = "refreshMs";
    pub const AUTO_START_MCP: &str = "autoStartMcp";
    pub const BRACE_WRAP: &str = "braceWrap";
    pub const GENERATOR_ASSERTS: &str = "generatorAsserts";
    /// The code-view output format index (the C++ `codeFormat`; main.cpp:2415).
    /// Stored as the `CodeFormat` enum discriminant; default `0` (C++ header).
    pub const CODE_FORMAT: &str = "codeFormat";
    /// The code-view scope index (the C++ `codeScope`; main.cpp:2441). Stored as
    /// the `CodeScope` enum discriminant; default `0` (Current struct).
    pub const CODE_SCOPE: &str = "codeScope";
    /// Title-case (vs Title-Case) for the menu-bar top-level titles (the C++
    /// `menuBarTitleCase`; main.cpp:988). Default `false` → "Title Case".
    pub const MENU_BAR_TITLE_CASE: &str = "menuBarTitleCase";
    /// Whether the titlebar shows the app icon (the C++ `showIcon`;
    /// main.cpp:990). Default `false`.
    pub const SHOW_ICON: &str = "showIcon";
    /// The last process the user attached to in the process picker, by name (the
    /// C++ `lastAttachedProcess`; processpicker.cpp:390). Read on picker open to
    /// preselect the matching row; written on a successful attach.
    pub const LAST_ATTACHED_PROCESS: &str = "lastAttachedProcess";
}

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
    /// flag + drives the dynamic menu label. No live bridge on this platform.
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
    /// Which right-dock tab the user last raised (Modules vs Bookmarks). The
    /// right dock tabifies both panels (one `Dock` open flag), so the View ✓
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
    Modules,
    Bookmarks,
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
        // Match the C++ persisted QSettings defaults (main.cpp:1336-1411):
        //   compactColumns=true, treeLines=true, relativeOffsets=true,
        //   typeHints=FALSE, showComments=false, hoverEffects=true, minimap=FALSE.
        // The previous Rust defaults wrongly turned typeHints + minimap ON.
        ViewOptions {
            compact_columns: true,
            tree_lines: true,
            relative_offsets: true,
            type_hints: false,
            show_comments: false,
            hover_effects: true,
            minimap: false,
        }
    }
}

impl ViewOptions {
    /// Load the persisted view-option toggles from the disk store, falling back
    /// to the C++ defaults for any unset key (the C++ `settings.value(key,
    /// default).toBool()` pattern; main.cpp:1336-1411).
    fn load(store: &DiskSettings) -> Self {
        let d = ViewOptions::default();
        ViewOptions {
            compact_columns: store.get_bool(settings_keys::COMPACT_COLUMNS, d.compact_columns),
            tree_lines: store.get_bool(settings_keys::TREE_LINES, d.tree_lines),
            relative_offsets: store.get_bool(settings_keys::RELATIVE_OFFSETS, d.relative_offsets),
            type_hints: store.get_bool(settings_keys::TYPE_HINTS, d.type_hints),
            show_comments: store.get_bool(settings_keys::SHOW_COMMENTS, d.show_comments),
            hover_effects: store.get_bool(settings_keys::HOVER_EFFECTS, d.hover_effects),
            minimap: store.get_bool(settings_keys::MINIMAP, d.minimap),
        }
    }

    /// The QSettings key one option persists under (the C++ `setValue(key, …)`).
    fn key(opt: ViewOpt) -> &'static str {
        match opt {
            ViewOpt::CompactColumns => settings_keys::COMPACT_COLUMNS,
            ViewOpt::TreeLines => settings_keys::TREE_LINES,
            ViewOpt::RelativeOffsets => settings_keys::RELATIVE_OFFSETS,
            ViewOpt::TypeHints => settings_keys::TYPE_HINTS,
            ViewOpt::ShowComments => settings_keys::SHOW_COMMENTS,
            ViewOpt::HoverEffects => settings_keys::HOVER_EFFECTS,
            ViewOpt::Minimap => settings_keys::MINIMAP,
        }
    }

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

    /// The [`CodeFormat`](crate::generator::CodeFormat) this code export maps to,
    /// or `None` for the ReClass-XML export (which goes through the importer's
    /// file exporter, not the generator).
    fn code_format(self) -> Option<crate::generator::CodeFormat> {
        use crate::generator::CodeFormat;
        Some(match self {
            ExportKind::Cpp => CodeFormat::CppHeader,
            ExportKind::Rust => CodeFormat::RustStruct,
            ExportKind::Defines => CodeFormat::DefineOffsets,
            ExportKind::CSharp => CodeFormat::CSharpStruct,
            ExportKind::Python => CodeFormat::PythonCtypes,
            ExportKind::Xml => return None,
        })
    }

    /// The save-file dialog filter for this export (the C++
    /// `codeFormatFileFilter(fmt)`; main.cpp:5758). XML uses a fixed ReClass-XML
    /// filter; everything else routes through
    /// [`code_format_file_filter`](crate::generator::code_format_file_filter).
    fn file_filter(self) -> &'static str {
        match self.code_format() {
            Some(fmt) => crate::generator::code_format_file_filter(fmt),
            None => "ReClass XML (*.xml);;All Files (*)",
        }
    }

    /// Render the active document's tree to this format. Mirrors the C++
    /// `exportToFile` (main.cpp:5752-5774), which always calls `renderCodeAll`
    /// (the **full SDK** — every root struct, ignoring the current view root) with
    /// the document's `typeAliases` and the persisted `generatorAsserts` flag.
    /// Returns `None` when nothing was produced.
    fn render(
        self,
        tree: &crate::core::NodeTree,
        aliases: Option<&crate::generator::TypeAliases>,
        emit_asserts: bool,
    ) -> Option<String> {
        let Some(fmt) = self.code_format() else {
            return Self::render_xml(tree);
        };
        let text = crate::generator::render_code_all(fmt, tree, aliases, emit_asserts);
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

/// File ▸ New {Class / Struct / Enum} — the root kind a fresh document is seeded
/// with (the C++ `project_new(keyword)`; main.cpp:4047). Determines the seed
/// root's `class_keyword` and the tab title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    Class,
    Struct,
    Enum,
}

impl RootKind {
    /// The C/C++ keyword stored on the seed root node ("class" / "struct" /
    /// "enum"); the compose pipeline renders it verbatim.
    fn class_keyword(self) -> &'static str {
        match self {
            RootKind::Class => "class",
            RootKind::Struct => "struct",
            RootKind::Enum => "enum",
        }
    }

    /// The new tab's title — distinct per kind so the three commands don't all
    /// land on an indistinguishable "Untitled".
    fn title(self) -> &'static str {
        match self {
            RootKind::Class => "Untitled Class",
            RootKind::Struct => "Untitled Struct",
            RootKind::Enum => "Untitled Enum",
        }
    }

    /// The seed root's type name.
    fn type_name(self) -> &'static str {
        match self {
            RootKind::Class => "NewClass",
            RootKind::Struct => "NewStruct",
            RootKind::Enum => "NewEnum",
        }
    }
}

/// Build a fresh document seeded with a single root struct of the given kind
/// (the C++ `project_new` template; main.cpp:6097 seeds a base address + a root
/// struct with 16 hex fields). The root carries the kind's `class_keyword` so
/// the rendered code reads `class` / `struct` / `enum`. Uses only the public
/// [`NodeTree`] API — no logic-module change.
fn seed_root_doc(kind: RootKind) -> crate::controller::RcxDocument {
    use crate::core::{Node, NodeKind};
    let mut doc = crate::controller::RcxDocument::new();
    // The C++ template lands a sensible default base so addresses read naturally.
    doc.tree.base_address = 0x0040_0000;
    let is32 = doc.tree.pointer_size < 8;
    let (hex_kind, stride) = if is32 {
        (NodeKind::Hex32, 4)
    } else {
        (NodeKind::Hex64, 8)
    };
    let mut root = Node {
        kind: NodeKind::Struct,
        name: "instance".to_string(),
        struct_type_name: kind.type_name().to_string(),
        class_keyword: kind.class_keyword().to_string(),
        parent_id: 0,
        offset: 0,
        ..Node::default()
    };
    root.id = doc.tree.reserve_id();
    let root_id = root.id;
    // File > New Enum seeds an enum root with 5 named members and NO hex fields
    // (the C++ `buildEmptyStruct` enum branch, main.cpp:3981-4000). Without this the
    // empty-members render gate (`is_enum() && !enum_members.is_empty()`) fails and
    // the 16 hex children would render as ordinary fields on screen and in exports.
    if matches!(kind, RootKind::Enum) {
        root.enum_members = (0..5).map(|i| (format!("Member{i}"), i as i64)).collect();
        doc.tree.add_node(root);
        doc.tree.touch();
        return doc;
    }
    doc.tree.add_node(root);
    for i in 0..16 {
        let mut c = Node {
            kind: hex_kind,
            name: format!("field_{:02x}", i * stride),
            parent_id: root_id,
            offset: i * stride,
            ..Node::default()
        };
        c.id = doc.tree.reserve_id();
        doc.tree.add_node(c);
    }
    doc.tree.touch();
    doc
}

/// Relabel the first leaf with the given command id, in place (used for the
/// dynamic MCP Start/Stop label). Recurses into submenus.
fn relabel_command(nodes: &mut [crate::ui::pickers::commandpalette::MenuNode], command: &str, new_label: &str) {
    use crate::ui::pickers::commandpalette::MenuNode;
    for node in nodes {
        match node {
            MenuNode::Item {
                label, command: c, ..
            } if c.as_str() == command => {
                *label = new_label.to_string();
                return;
            }
            MenuNode::Submenu { children, .. } => {
                relabel_command(children, command, new_label);
            }
            _ => {}
        }
    }
}

/// Map a plugin [`DockSide`](crate::plugin::DockSide) to the gpui-component
/// [`DockPlacement`] a contributed panel mounts at (design §6 Phase 2). The demo's
/// panel is `Right`, so it tabs in beside the Modules/Bookmarks right dock.
fn dock_placement_for(side: crate::plugin::DockSide) -> DockPlacement {
    match side {
        crate::plugin::DockSide::Left => DockPlacement::Left,
        crate::plugin::DockSide::Right => DockPlacement::Right,
        crate::plugin::DockSide::Bottom => DockPlacement::Bottom,
    }
}

/// Inject one menu item per enabled plugin `Command` whose slot surfaces in the
/// menu bar (`Menu`/`SourceMenu`/`Palette`) into the `&Plugins` submenu's children
/// (design §6 Phase 2). Pure (no gpui) so the byte-identical-when-empty parity is
/// unit-testable: with an empty `commands` list the tree is returned untouched
/// (the `&Plugins` submenu keeps exactly its static `[Manage Plugins…]` row).
/// `EditorContext`/`Toolbar` slots are not menu-bar surfaces, so they are skipped
/// here (they'd be injected into the editor context menu / toolbar instead — those
/// surfaces are intended-deferred for plugin contributions).
fn inject_plugin_menu_items(
    tree: &mut [crate::ui::pickers::commandpalette::MenuNode],
    commands: &[crate::plugin::UiContribution],
) {
    use crate::ui::pickers::commandpalette::MenuNode;
    use crate::plugin::{CommandSlot, UiContribution};
    // Find the &Plugins submenu by its label (the static menu tree carries it).
    let Some(MenuNode::Submenu { children, .. }) = tree
        .iter_mut()
        .find(|n| matches!(n, MenuNode::Submenu { label, .. } if label == "&Plugins"))
    else {
        return;
    };
    for c in commands {
        if let UiContribution::Command { id, title, slot } = c {
            if matches!(
                slot,
                CommandSlot::Menu | CommandSlot::SourceMenu | CommandSlot::Palette
            ) {
                children.push(MenuNode::item(title, "", id));
            }
        }
    }
}

/// The struct name of the active **view root** for the window title (the C++
/// `rootName(tree, viewRootId())`; main.cpp:5180). Climbs from the view-root
/// node to its top-level parent and returns that node's `struct_type_name`
/// (falling back to its `name`). Mirrors the status-bar's `root_name_of` climb.
/// Returns an empty string when the index is out of range. Pure; unit-tested.
fn root_name_for_title(tree: &crate::core::NodeTree, view_root_id: u64) -> String {
    let mut cur = tree.index_of_id(view_root_id);
    // A `0`/unknown view root means "the whole document" — start at the first
    // top-level node so a fresh document still names its root struct.
    if cur < 0 {
        cur = tree
            .nodes
            .iter()
            .position(|n| n.parent_id == 0)
            .map_or(-1, |i| i as i32);
    }
    while cur >= 0 {
        let Some(n) = tree.nodes.get(cur as usize) else {
            break;
        };
        if n.parent_id == 0 {
            return if n.struct_type_name.is_empty() {
                n.name.clone()
            } else {
                n.struct_type_name.clone()
            };
        }
        cur = tree.index_of_id(n.parent_id);
    }
    String::new()
}

/// The unsaved-changes display name for one dirty document (the C++ `closeEvent`
/// per-doc name rule; main.cpp:8991-8993): the file name when the document has a
/// path, else the view-root struct name. Pure; unit-tested. Returns `None` for a
/// clean document so callers can filter in one pass (the C++ `if (modified ...)`
/// gate; main.cpp:8989) — a clean set yields no names ⇒ the close is accepted.
fn dirty_doc_name(
    modified: bool,
    file_path: Option<&std::path::Path>,
    tree: &crate::core::NodeTree,
    view_root_id: u64,
) -> Option<String> {
    if !modified {
        return None;
    }
    let name = match file_path {
        Some(p) => p
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| root_name_for_title(tree, view_root_id)),
        None => root_name_for_title(tree, view_root_id),
    };
    Some(name)
}

/// Detect a ReClass-XML file from its leading bytes (the C++ `project_open`
/// signature sniff; main.cpp:6129): after trimming leading ASCII whitespace, the
/// head starts with `<?xml` or `<ReClass`. The C++ used
/// `head.trimmed().startsWith(...)` — `QByteArray::trimmed()` strips both ends,
/// but only the leading run matters for a prefix test, so trimming the start is
/// equivalent. Chosen by content, not by file *name*: a `.rcx` carrying XML is
/// imported as XML and an `.xml` carrying JSON falls through to the native JSON
/// load (the headline of this gap). Pure; unit-tested.
fn sniff_is_reclass_xml(head: &[u8]) -> bool {
    let trimmed = head.trim_ascii_start();
    trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<ReClass")
}

/// Assemble the OS window-title string from a root name + dirty flag (the C++
/// `updateWindowTitle` formatting; main.cpp:5176-5186): `"<name>[ *] - Reclass"`,
/// or plain `"Reclass"` when the name is empty (no document / unnamed root).
/// Pure; unit-tested.
fn window_title_string(root_name: &str, modified: bool) -> String {
    if root_name.is_empty() {
        return "Reclass".to_string();
    }
    let mut name = root_name.to_string();
    if modified {
        name.push_str(" *");
    }
    format!("{name} - Reclass")
}

/// The unsaved-changes guard's header sentence, picked by the count of distinct
/// dirty documents (the C++ `closeEvent`: two complete sentences by count
/// instead of in-string pluralization; main.cpp:9003-9006). Pure; unit-tested.
fn unsaved_changes_text(dirty_count: usize) -> String {
    if dirty_count == 1 {
        "One project has unsaved changes:".to_string()
    } else {
        format!("{dirty_count} projects have unsaved changes:")
    }
}

/// The GitHub URL the Help ▸ About dialog advertises (the C++ About dialog's
/// "Open GitHub" button opens `https://github.com/IChooseYou/Reclass`;
/// main.cpp:4434). Was wrongly `github.com/reclassnet/reclass` (item 8).
const ABOUT_GITHUB_URL: &str = "https://github.com/IChooseYou/Reclass";

/// Dedup the dirty-document display names while preserving first-seen order (the
/// C++ `closeEvent` builds `dirtyNames` from unique dirty docs, skipping repeats
/// — multiple tabs can share a document; main.cpp:8988-8996). Pure; unit-tested.
fn unique_dirty_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for n in names {
        if seen.insert(n.clone()) {
            out.push(n);
        }
    }
    out
}

/// A command's own toast, unless the live host already surfaced it (dedup the
/// `CommandResult.toast` against the host's drained toast list).
fn surface_command_toast(res_toast: Option<String>, already: &[String]) -> Option<String> {
    res_toast.filter(|m| !already.iter().any(|t| t == m))
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
        // default shipping build) leaves the manager at EXACTLY the four built-in
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
        // parity. The default build (no `plugins`) does NOT compile this: no scan.
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

        // ── Wire the modules dock row activation (double-click → set the active
        // document's base address to the module base). ──
        cx.subscribe_in(
            &modules,
            window,
            |this, _md, ev: &crate::ui::panels::modulespanel::ModuleAction, window, cx| match ev {
                crate::ui::panels::modulespanel::ModuleAction::Activate { base, .. } => {
                    this.navigate_active_editor_to_address(*base, window, cx);
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
            // The right dock defaults to the Modules tab (the first tabified
            // panel; docks.rs builds [modules, bookmarks]).
            right_dock_panel: RightDockPanel::Modules,
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
            // mount can subscribe to `self`). Empty in the default build.
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
        // no-op in the default build (the four providers contribute no `Panel`), so
        // the dock layout is byte-identical; with a contributing plugin (the demo,
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
        let status_info = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| StatusInfo::for_controller(ed.read(cx).controller()))
            .unwrap_or_default();
        let source = self.state.active_source();
        let status_bar = render_status_bar(&status_info, &source, cx);

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
            // Tools accelerators (RTTI Browser / Performance Profiler) — advertised
            // in the menu but previously unbound.
            .on_action(cx.listener(Self::on_rtti))
            .on_action(cx.listener(Self::on_profiler))
            // Alt+letter menu-bar mnemonics → toggle the matching top-level menu.
            .on_action(cx.listener(|this, _: &OpenMenuFile, _w, cx| {
                this.toggle_menu_mnemonic('f', cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenuEdit, _w, cx| {
                this.toggle_menu_mnemonic('e', cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenuView, _w, cx| {
                this.toggle_menu_mnemonic('v', cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenuTools, _w, cx| {
                this.toggle_menu_mnemonic('t', cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenuPlugins, _w, cx| {
                this.toggle_menu_mnemonic('p', cx)
            }))
            .on_action(cx.listener(|this, _: &OpenMenuHelp, _w, cx| {
                this.toggle_menu_mnemonic('h', cx)
            }))
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
    let mut bindings = scope_editor_text_keys_to_non_field(crate::ui::editor::editor_key_bindings());
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
