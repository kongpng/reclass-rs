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
use super::workspace::{
    WorkspaceDoc, WorkspaceModel, WorkspaceNav, WorkspaceNewType, WorkspacePanel,
    WorkspaceTypeAction,
};
use crate::theme::{SettingsStore, ThemeManager};

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
        ProfilerAction
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
    scanner: Entity<super::scannerpanel::ScannerPanel>,
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
    /// The mounted plugin [`PluginPanel`](super::pluginpanel::PluginPanel) views,
    /// keyed by the contributed panel id (design §6 Phase 2). One per enabled
    /// `Panel` contribution, built in [`new`](Self::new) after the demo gating and
    /// docked into the existing dock area. Empty in the default shipping build (no
    /// plugin contributes a panel ⇒ no extra dock view ⇒ byte-identical UI). Held so
    /// [`rerender_plugin_panel`](Self::rerender_plugin_panel) can push a fresh tree.
    plugin_panels: Vec<(String, Entity<super::pluginpanel::PluginPanel>)>,
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
    doc.tree.add_node(root);
    for i in 0..16 {
        let mut c = Node {
            kind: hex_kind,
            name: format!("field_{:04x}", i * stride),
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
fn relabel_command(nodes: &mut [super::commandpalette::MenuNode], command: &str, new_label: &str) {
    use super::commandpalette::MenuNode;
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
    tree: &mut [super::commandpalette::MenuNode],
    commands: &[crate::plugin::UiContribution],
) {
    use super::commandpalette::MenuNode;
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
        let goto_recent = crate::ui::gotoaddress::load_recent(&*settings.borrow());
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
                    .unwrap_or(super::optionsdialog::REFRESH_DEFAULT),
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
            |this, _ws, ev: &super::workspace::WorkspaceTypeAction, window, cx| {
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
            |this, _bm, ev: &super::bookmarkspanel::BookmarkAction, window, cx| {
                this.on_bookmark_action(ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Wire the scanner dock result-row navigation (the C++ result-row
        // double-click → jump the editor to the address). ──
        cx.subscribe_in(
            &scanner,
            window,
            |this, _sc, ev: &super::scannerpanel::ScannerNav, window, cx| {
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
            |this, sc, ev: &super::scannerpanel::ScannerEdit, window, cx| {
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
            |this, _sc, ev: &super::scannerpanel::ScannerAddNodes, window, cx| {
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
            |this, sc, ev: &super::scannerpanel::ScannerBatchEdit, window, cx| {
                this.on_scanner_batch_edit(sc.clone(), ev.clone(), window, cx);
            },
        )
        .detach();

        // ── Wire the modules dock row activation (double-click → set the active
        // document's base address to the module base). ──
        cx.subscribe_in(
            &modules,
            window,
            |this, _md, ev: &super::modulespanel::ModuleAction, window, cx| match ev {
                super::modulespanel::ModuleAction::Activate { base, .. } => {
                    this.navigate_active_editor_to_address(*base, window, cx);
                }
                super::modulespanel::ModuleAction::DownloadAll => {
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
        let initial_editors: Vec<Entity<super::editor::RcxEditor>> = win
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

    // The File/Edit accelerator handlers — each routes its bound key to the same
    // MENU CONTRACT command `run_menu_command` dispatches (the blocker fix: these
    // accelerators were advertised in the menu but had no global key binding).
    fn on_new_class(&mut self, _: &NewClassAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.new_class".to_string(), window, cx);
    }
    fn on_new_struct(&mut self, _: &NewStructAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.new_struct".to_string(), window, cx);
    }
    fn on_new_enum(&mut self, _: &NewEnumAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.new_enum".to_string(), window, cx);
    }
    fn on_open_file(&mut self, _: &OpenFileAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.open".to_string(), window, cx);
    }
    fn on_save(&mut self, _: &SaveAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.save".to_string(), window, cx);
    }
    fn on_save_as(&mut self, _: &SaveAsAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.save_as".to_string(), window, cx);
    }
    fn on_close_doc(&mut self, _: &CloseDocAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"file.close".to_string(), window, cx);
    }
    fn on_undo(&mut self, _: &UndoAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"edit.undo".to_string(), window, cx);
    }
    fn on_redo(&mut self, _: &RedoAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"edit.redo".to_string(), window, cx);
    }
    fn on_add_bookmark(
        &mut self,
        _: &AddBookmarkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"edit.add_bookmark".to_string(), window, cx);
    }
    fn on_quick_bookmark(
        &mut self,
        _: &QuickBookmarkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_menu_command(&"edit.quick_bookmark".to_string(), window, cx);
    }
    fn on_shortcuts(&mut self, _: &ShortcutsAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"help.shortcuts".to_string(), window, cx);
    }
    // The Tools accelerator handlers — the RTTI Browser (Ctrl+Shift+R;
    // main.cpp:1524) and Performance Profiler (Ctrl+Shift+F; main.cpp:1565) were
    // advertised in the Tools menu but had no global key binding, so both
    // shortcuts were dead. Each routes its bound key to the same MENU CONTRACT
    // command `run_menu_command` already dispatches.
    fn on_rtti(&mut self, _: &RttiAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"tools.rtti".to_string(), window, cx);
    }
    fn on_profiler(&mut self, _: &ProfilerAction, window: &mut Window, cx: &mut Context<Self>) {
        self.run_menu_command(&"tools.profiler".to_string(), window, cx);
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
            // ── File: new documents (seed the per-kind root; the C++ newClass /
            // newStruct / newEnum each pass a distinct class keyword). ──
            "file.new_class" => self.new_document(RootKind::Class, window, cx),
            "file.new_struct" => self.new_document(RootKind::Struct, window, cx),
            "file.new_enum" => self.new_document(RootKind::Enum, window, cx),
            "file.welcome" => self.show_start_page(window, cx),

            // ── File: open / save ──
            "file.open" => self.prompt_open(window, cx),
            "file.save" => self.save_active(false, window, cx),
            "file.save_as" => self.save_active(true, window, cx),
            "file.close" => self.close_active_document(window, cx),
            "file.exit" => self.request_quit(window, cx),

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
            // The C++ `m_sourceMenu` triggers route to controller->selectSource /
            // clearSources. File attaches a binary; Process opens the picker; the
            // remaining live providers have no factory on this platform.
            "source.clear" => self.clear_active_source(window, cx),
            "source.file" => self.prompt_data_file(window, cx),
            "source.process" => self.open_process_picker(window, cx),
            // The C++ Data Source set is File + the registered providers only — no
            // Kernel Memory row (kernelmemory is a Browse-Page-Tables provider id,
            // never a Data Source entry; see commandpalette.rs `menu_tree_with`).
            "source.remote" | "source.windbg" | "source.rcnet" => {
                self.report_unavailable_source(cmd.as_str(), window, cx)
            }

            // ── Edit (Undo / Redo / Add Bookmark… / Quick Bookmark Here) ──
            "edit.undo" => self.active_editor_undo(false, cx),
            "edit.redo" => self.active_editor_undo(true, cx),
            "edit.add_bookmark" => self.prompt_add_bookmark(window, cx),
            "edit.quick_bookmark" => self.quick_bookmark_here(window, cx),

            // ── View: docks / windows ──
            "view.project" => self.toggle_left_dock(window, cx),
            "view.scanner" => self.toggle_scanner_dock(&ToggleScanner, window, cx),
            "view.modules" => self.raise_modules(window, cx),
            "view.bookmarks" | "view.symbols" => self.raise_bookmarks(window, cx),
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

            // ── View: font family (the C++ exclusive Consolas / JetBrains Mono
            // picker persisted to settings("font")). ──
            "view.font.consolas" => self.set_editor_font("Consolas", window, cx),
            "view.font.jetbrains" => self.set_editor_font("JetBrains Mono", window, cx),

            // ── View: actions ──
            "view.refresh" => self.refresh_active_editor(cx),
            "view.goto_address" => self.open_goto_address(window, cx),
            "view.command_palette" => self.open_command_palette(&OpenCommandPalette, window, cx),
            "view.split" => self.split_view(window, cx),
            "view.unsplit" => self.unsplit_view(window, cx),
            "view.presentation" => self.toggle_presentation(cx),
            // View ▸ Edit Theme — open the dedicated ThemeEditor (swatch grid /
            // live preview / save-as-user-copy), the C++ `editTheme`. Previously
            // this folded into the Options dialog and the ThemeEditor view had no
            // caller (item 4).
            "view.theme_edit" => self.open_theme_editor(window, cx),

            // ── Tools ──
            "tools.rtti" => self.open_rtti_browser(window, cx),
            "tools.type_aliases" => self.open_type_aliases_dialog(window, cx),
            "tools.mcp" => self.toggle_mcp(window, cx),
            "tools.options" => self.open_options_dialog(window, cx),
            "tools.profiler" => self.open_profiler_dialog(window, cx),

            // ── Plugins ──
            "plugins.manage" => self.open_plugins_dialog(window, cx),

            // ── Help ──
            "help.about" => self.show_about(window, cx),
            "help.shortcuts" | "help.docs" => self.show_shortcuts(window, cx),

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
            // A recent file (`file.recent.<INDEX>`) — reopen the recorded path.
            other if other.starts_with("file.recent.") => {
                self.open_recent_by_command(other, window, cx);
            }
            // Switch the active saved data source (`source.saved.<INDEX>`) — the
            // C++ `m_sourceMenu` saved-source rows route to
            // `controller->switchToSavedSource(idx)`. Previously these rows fell
            // into the catch-all and did nothing.
            other if other.starts_with("source.saved.") => {
                self.switch_saved_source_by_command(other, window, cx);
            }

            // A plugin-contributed command (design §6 Phase 2): route it through the
            // session-owned manager + a scoped live host. Checked AFTER the built-in
            // ids so a plugin can't shadow a host command, and BEFORE the catch-all
            // so it doesn't fall to the log-noop. In the default build no plugin
            // contributes a command, so this never matches.
            other if self.plugin_manager.is_plugin_command(other) => {
                self.dispatch_plugin_command(other, window, cx);
            }

            // ── Anything still unmapped: graceful, logged no-op. ──
            other => {
                tracing::debug!(command = %other, "menu command not yet wired");
            }
        }
    }

    /// Open a fresh document tab seeded with the chosen root kind (the C++
    /// `project_new(keyword)` family: `newClass` passes "class", `newStruct`
    /// none, `newEnum` "enum"; main.cpp:4047). The root node carries the matching
    /// `class_keyword` so the rendered C/C++ reads `class` / `struct` / `enum`,
    /// and the tab title reflects the kind so the three commands are visibly
    /// distinct (the bug: all three collapsed to one blank "Untitled").
    fn new_document(&mut self, kind: RootKind, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_start_page(cx);
        let title = kind.title();
        let doc = seed_root_doc(kind);
        let mut new_editor: Option<Entity<super::editor::RcxEditor>> = None;
        self.document_area.update(cx, |area, cx| {
            area.push_document(title, window, cx);
            // Push the seeded tree into the just-created tab's editor.
            if let Some(editor) = area.active_editor().cloned() {
                editor.update(cx, |ed, cx| ed.set_document(doc, cx));
                new_editor = Some(editor);
            }
        });
        // Realign the fresh editor to the window's (persisted) view options so a
        // new tab honours the current compact-columns/tree-lines/etc. state.
        if let Some(editor) = new_editor {
            self.apply_view_opts_to_editor(&editor, cx);
        }
        self.state.open_document(title);
        self.rebuild_workspace(cx);
        // A fresh doc has the NullProvider — clear the docks accordingly.
        self.refresh_docks_for_active(cx);
        self.observe_editors(window, cx);
    }

    // ── File: data source providers (the C++ m_sourceMenu → selectSource) ──

    /// File ▸ Data Source ▸ File — attach a binary file as the active document's
    /// data source via the native file picker (the C++ `loadData(path)` /
    /// File-provider attach). Updates the tab source icon + window state.
    fn prompt_data_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach a binary data file".into()),
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
            let _ = this.update_in(cx, |me, window, cx| {
                editor.update(cx, |ed, _cx| {
                    // Route through the controller so the attach clears the undo
                    // stack and resets the live snapshot, matching C++
                    // `RcxDocument::loadData(path)` (controller.cpp:292). Calling
                    // `document_mut().load_data_file` directly would bypass both.
                    ed.controller_mut().attach_data_file(&path);
                });
                // Recompose against the freshly-attached provider + reflect the
                // File source icon in the tab and window state.
                editor.update(cx, |ed, cx| ed.apply_document(cx));
                let source = super::state::DataSource::new(
                    super::state::SourceKind::File,
                    path.to_string_lossy().into_owned(),
                );
                me.set_active_source(source, window, cx);
                me.notify(format!("Attached {}", path.display()), window, cx);
            });
        })
        .detach();
    }

    /// File ▸ Data Source ▸ Process Memory — open the live process picker (the
    /// C++ `ProcessPicker` reached from `selectSource("process")`). On this
    /// platform the registry exposes no live factories, so the picker surfaces
    /// the (possibly stub) provider rows; a chosen row reports the selection.
    fn open_process_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::processpicker::{ProcessPickEvent, ProcessPicker, ProcessPickerModel};
        // Build the picker's available-source rows from the SESSION-OWNED plugin
        // manager's registry (the in-tree File/Buffer/Snapshot/Null providers
        // registered through the contract) — the single source the rest of the app
        // reads, so an enable/disable in Manage-Plugins is reflected here within the
        // session (design §6 Phase 1 / §7.A [fix] / §H). Byte-identical output to the
        // old throwaway `with_builtins()` when no plugin has been toggled.
        let model = ProcessPickerModel::from_registry(self.plugin_manager.registry());
        // Remember which process the user last attached to (the C++
        // `lastAttachedProcess` QSettings key; processpicker.cpp:386). The picker
        // *reads* this to pre-select the matching row in `selectPreferredProcess` —
        // that read lives inside the picker (it owns the row table + selection), so
        // here we only own the WRITE side (on a successful attach, below). The read
        // is loaded so a future picker that consumes it sees a populated value.
        let _last_attached = self
            .settings
            .borrow()
            .get(settings_keys::LAST_ATTACHED_PROCESS)
            .filter(|s| !s.is_empty());
        let picker = cx.new(|cx| ProcessPicker::new(model, window, cx));
        self.goto_sub = Some(cx.subscribe_in(
            &picker,
            window,
            |this, _p, ev: &ProcessPickEvent, window, cx| match ev {
                ProcessPickEvent::Attach { name, pid, .. } => {
                    window.close_dialog(cx);
                    // Remember this process for next time (the C++ persists
                    // `lastAttachedProcess` on a successful attach; read back by
                    // `selectPreferredProcess`). Persisted via the disk store.
                    this.settings
                        .borrow_mut()
                        .set(settings_keys::LAST_ATTACHED_PROCESS, name);
                    // No live provider factory on this platform — record the pick
                    // as the document's logical source so the tab reflects it.
                    let source = super::state::DataSource::new(
                        super::state::SourceKind::Process,
                        format!("{name} (pid {pid})"),
                    );
                    this.set_active_source(source, window, cx);
                    this.notify(format!("Selected process {name} (pid {pid})"), window, cx);
                }
                ProcessPickEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let picker_for_modal = picker.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(720.))
                .margin_top(px(80.))
                .close_button(false)
                .child(picker_for_modal.clone())
        });
        cx.notify();
    }

    /// File ▸ Data Source ▸ {Remote / WinDbg / ReClass.NET} — these live providers
    /// have no factory on this platform. The C++ shows a blocking warning when a
    /// source can't attach; mirror that with the themed modal message box (not a
    /// transient toast). (No Kernel Memory case: the C++ Data Source menu has no
    /// such row — see `report_unavailable_source`'s callers / `menu_tree_with`.)
    fn report_unavailable_source(
        &mut self,
        cmd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = match cmd {
            "source.remote" => "Remote Process Memory",
            "source.windbg" => "WinDbg Memory",
            "source.rcnet" => "ReClass.NET Compat",
            _ => "This data source",
        };
        let spec = super::messagebox::warn(
            "Source Unavailable",
            &format!(
                "{label} is not available on this platform. Open a project with a saved \
                 source, or attach a binary File instead."
            ),
        );
        super::messagebox::open_message(spec, window, cx);
    }

    /// File ▸ Data Source ▸ Clear All — detach the active document's source (the
    /// C++ `clearSources`).
    fn clear_active_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                ed.controller_mut().clear_sources();
                ed.apply_document(cx);
            });
        }
        self.set_active_source(super::state::DataSource::none(), window, cx);
    }

    // ── Edit: bookmarks (the C++ promptAddBookmark / Quick Bookmark Here) ──

    /// Edit ▸ Add Bookmark… (Ctrl+B) — prompt for a name (defaulting the formula
    /// to the active doc's base) and add the bookmark (the C++
    /// `promptAddBookmark`; main.cpp:8090). The themed prompt collects the name;
    /// the formula defaults to the current base.
    fn prompt_add_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };
        let default_formula = {
            let ed = editor.read(cx);
            let tree = ed.controller().tree();
            if tree.base_address_formula.is_empty() {
                format!("0x{:X}", tree.base_address)
            } else {
                tree.base_address_formula.clone()
            }
        };
        // Free-text name entry (the C++ Edit ▸ Add Bookmark `QInputDialog::getText`
        // collects the bookmark NAME; the formula defaults to the current base).
        // Seed the field with the next free auto-name so a blind Enter still works,
        // but let the user type any name — replacing the old auto-name-only confirm.
        let default_name = self.next_bookmark_name(&editor, cx);
        let editor2 = editor.clone();
        let formula = default_formula.clone();
        let this = cx.entity().downgrade();
        self.open_text_prompt(
            "Add Bookmark",
            &format!("Name (address {default_formula})"),
            &default_name,
            window,
            cx,
            move |name, window, app| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let _ = this.update(app, |me, cx| {
                    editor2.update(cx, |ed, _cx| {
                        ed.controller_mut().add_bookmark(&name, &formula);
                    });
                    me.after_bookmark_added(&name, &formula, window, cx);
                });
            },
        );
    }

    /// Edit ▸ Quick Bookmark Here (Ctrl+Alt+B) — capture the current address as an
    /// auto-named `bookmark_NN` (no dialog; the C++ lambda at main.cpp:1197).
    fn quick_bookmark_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };
        let formula = {
            let ed = editor.read(cx);
            let tree = ed.controller().tree();
            if tree.base_address_formula.is_empty() {
                format!("0x{:X}", tree.base_address)
            } else {
                tree.base_address_formula.clone()
            }
        };
        let name = self.next_bookmark_name(&editor, cx);
        editor.update(cx, |ed, _cx| {
            ed.controller_mut().add_bookmark(&name, &formula);
        });
        self.after_bookmark_added(&name, &formula, window, cx);
    }

    /// Find a free `bookmark_NN` slot name in the active document (the C++
    /// taken-set loop; main.cpp:1204).
    fn next_bookmark_name(
        &self,
        editor: &Entity<super::editor::RcxEditor>,
        cx: &Context<Self>,
    ) -> String {
        let taken: std::collections::HashSet<String> = editor
            .read(cx)
            .controller()
            .tree()
            .bookmarks
            .iter()
            .map(|b| b.name.clone())
            .collect();
        let mut n = 1;
        loop {
            let name = format!("bookmark_{n:02}");
            if !taken.contains(&name) || n >= 1000 {
                return name;
            }
            n += 1;
        }
    }

    /// Shared tail after a bookmark is added: refresh the bookmarks dock, open the
    /// right dock, sync dirty state, and confirm (the C++ `refreshBookmarksDock` +
    /// `setAppStatus`).
    fn after_bookmark_added(
        &mut self,
        name: &str,
        formula: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_right_dock_open(true, window, cx);
        self.sync_view_menu_checked(cx);
        // Populate the bookmarks dock with the controller's current list (the C++
        // `refreshBookmarksDock`); previously the panel stayed empty after Add.
        self.refresh_docks_for_active(cx);
        self.sync_dirty_state(cx);
        self.notify(format!("Bookmarked: {name} → {formula}"), window, cx);
        cx.notify();
    }

    /// Handle a bookmarks-dock row action (the C++ bookmark-row click → navigate,
    /// right-click → Navigate / Remove): navigate the active editor to the
    /// bookmark's address formula, or remove the bookmark at its index + refresh
    /// the dock.
    fn on_bookmark_action(
        &mut self,
        ev: super::bookmarkspanel::BookmarkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::bookmarkspanel::BookmarkAction;
        match ev {
            BookmarkAction::Navigate { formula } => {
                // Reuse the go-to-address resolve/navigate path so `<mod>+0x..` /
                // `[ptr]` forms resolve against the live provider.
                self.commit_goto(&formula, 0, window, cx);
            }
            BookmarkAction::Remove { index } => {
                if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
                    editor.update(cx, |ed, cx| {
                        ed.controller_mut().remove_bookmark(index);
                        ed.apply_document(cx);
                    });
                }
                self.refresh_docks_for_active(cx);
                self.sync_dirty_state(cx);
                cx.notify();
            }
            // Header "+" → open the same Add-Bookmark prompt the Edit menu uses.
            BookmarkAction::Add => self.prompt_add_bookmark(window, cx),
        }
    }

    /// Navigate the active editor to an absolute address (the scanner result-row
    /// / module-row jump target): rebase the active tree to `addr` + recompose.
    fn navigate_active_editor_to_address(
        &mut self,
        addr: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };
        editor.update(cx, |ed, cx| {
            let ctrl = ed.controller_mut();
            ctrl.document_mut().tree.base_address = addr;
            // Reset value-history / heat on a jump (the C++ `resetChangeTracking`
            // on navigate): the old base's per-node change heat + history no longer
            // describes the new region, so clear it before recomposing — otherwise
            // every field flashes "changed" against the previous address's values.
            ctrl.reset_change_tracking();
            ed.apply_document(cx);
        });
        self.rebuild_workspace(cx);
        self.notify(format!("Jumped to 0x{addr:X}"), window, cx);
        cx.notify();
    }

    /// Resolve a scanner result-cell inline edit against the active document (the
    /// C++ `onCellEdited`):
    /// - **Address cell** (`EvalAddress`): re-evaluate the (re-typed) address
    ///   expression with the live provider callbacks, re-read the value at the new
    ///   address, and push it back into the row (`apply_address_edit`) so the row
    ///   tracks the new location.
    /// - **Value cell** (`WriteValue`): serialize the value-input text with the
    ///   scanner's value type and write it back to the row's address through the
    ///   active controller's writable provider, then refresh the row's recorded
    ///   bytes (`apply_value_write`).
    ///
    /// The panel raises the intent because it holds only a read-only `Arc` provider
    /// clone; the mutable controller lives here on the window.
    fn on_scanner_edit(
        &mut self,
        scanner: Entity<super::scannerpanel::ScannerPanel>,
        ev: super::scannerpanel::ScannerEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::scannerpanel::ScannerEdit;
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        match ev {
            ScannerEdit::EvalAddress { row } => {
                // The re-typed address expression lives in the scanner's value
                // input (the inline-edit source the panel exposes); evaluate it,
                // re-read the value at the resolved address, and rebind the row.
                let formula = scanner.read(cx).value_input_text(cx);
                if formula.trim().is_empty() {
                    return;
                }
                let value_type = scanner.read(cx).last_value_type();
                let ptr_size = editor.read(cx).controller().tree().pointer_size.max(1);
                let provider = editor.read(cx).controller().document().provider.clone();
                let cbs = crate::addr::AddressParserCallbacks {
                    resolve_module: Some(Box::new({
                        let p = provider.clone();
                        move |name: &str| {
                            let base = p.symbol_to_address(name);
                            (base, base != 0)
                        }
                    })),
                    read_pointer: Some(Box::new({
                        let p = provider.clone();
                        move |addr: u64| {
                            if ptr_size >= 8 {
                                (p.read_u64(addr), true)
                            } else {
                                (p.read_u32(addr) as u64, true)
                            }
                        }
                    })),
                    resolve_identifier: Some(Box::new({
                        let p = provider.clone();
                        move |name: &str| {
                            let base = p.symbol_to_address(name);
                            (base, base != 0)
                        }
                    })),
                    ..Default::default()
                };
                let r = crate::addr::AddressParser::evaluate(&formula, ptr_size, Some(&cbs));
                if !r.ok {
                    self.notify(format!("Couldn't evaluate \"{formula}\"."), window, cx);
                    return;
                }
                let size = crate::scanner::value_size_for_type(value_type).max(0);
                let new_value = if size > 0 {
                    provider.read_bytes(r.value, size)
                } else {
                    Vec::new()
                };
                scanner.update(cx, |sp, cx| {
                    sp.apply_address_edit(row, r.value, new_value, cx);
                });
            }
            ScannerEdit::WriteValue { row, address } => {
                let text = scanner.read(cx).value_input_text(cx);
                if text.trim().is_empty() {
                    self.notify(
                        "Type a value in the scanner's Value field, then double-click \
                         a result's Value cell to write it.",
                        window,
                        cx,
                    );
                    return;
                }
                let value_type = scanner.read(cx).last_value_type();
                // Serialize the typed value to bytes via the scanner value type
                // (the C++ writes the typed value, not the displayed bytes).
                let bytes = match crate::scanner::serialize_value(value_type, &text) {
                    Ok((b, _mask)) => b,
                    Err(e) => {
                        self.notify(format!("Invalid value: {e}"), window, cx);
                        return;
                    }
                };
                if bytes.is_empty() {
                    return;
                }
                let wrote = editor.update(cx, |ed, _cx| {
                    ed.controller_mut().write_memory(address, &bytes)
                });
                if wrote {
                    let new_value = bytes.clone();
                    scanner.update(cx, |sp, cx| {
                        sp.apply_value_write(row, new_value, cx);
                    });
                    // The write changed live memory — re-read so the editor reflects
                    // the new value.
                    editor.update(cx, |ed, cx| ed.apply_document(cx));
                } else {
                    self.notify(
                        "Write failed — the data source isn't writable (open a File source).",
                        window,
                        cx,
                    );
                }
            }
        }
    }

    /// Scanner "Add as Nodes" — append one node per result address into the active
    /// editor's view-root container (the C++ drag-result-into-editor / add-as-nodes
    /// path; scannerpanel.cpp:655). Each append is a tail `Hex64` field via the
    /// controller (the same path the editor's own add-member uses). Reports the
    /// count added; rebuilds the workspace + dirty dot like any other mutation.
    fn on_scanner_add_nodes(
        &mut self,
        addresses: &[u64],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first to add scanner nodes.", window, cx);
            return;
        };
        if addresses.is_empty() {
            return;
        }
        let added = editor.update(cx, |ed, cx| {
            let view_root = ed.controller().view_root_id();
            let mut added = 0usize;
            for _ in addresses {
                // Append into the view-root container (or its first root struct
                // when the view root is the whole document). `append_single_field`
                // walks up to the owning Struct/Array/Enum and adds a tail Hex64.
                if ed.controller_mut().append_single_field(view_root).is_some() {
                    added += 1;
                }
            }
            ed.apply_document(cx);
            added
        });
        self.rebuild_workspace(cx);
        self.sync_dirty_state(cx);
        self.notify(format!("Added {added} node(s) from scanner"), window, cx);
        cx.notify();
    }

    /// Scanner "Change All Values" — write `bytes` to every result address through
    /// the active editor's mutable controller, re-read each, and hand the results
    /// back to [`ScannerPanel::apply_change_all`] so it produces the "Wrote to X/Y"
    /// tail (the C++ batch write + re-read; scannerpanel.cpp:981). A failed write
    /// (read-only provider) yields `None` for that address so the count reflects
    /// only the writes that landed.
    fn on_scanner_batch_edit(
        &mut self,
        scanner: Entity<super::scannerpanel::ScannerPanel>,
        ev: super::scannerpanel::ScannerBatchEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        if ev.bytes.is_empty() || ev.addresses.is_empty() {
            return;
        }
        let read_size = ev.bytes.len() as i32;
        let provider = editor.read(cx).controller().document().provider.clone();
        let mut results: Vec<(u64, Option<Vec<u8>>)> = Vec::with_capacity(ev.addresses.len());
        for &addr in &ev.addresses {
            let wrote = editor.update(cx, |ed, _cx| {
                ed.controller_mut().write_memory(addr, &ev.bytes)
            });
            if wrote {
                // Re-read the just-written bytes so the scanner row reflects the
                // new value (the C++ `prov->readBytes(r.address, readSize)`).
                let nb = provider.read_bytes(addr, read_size);
                results.push((addr, Some(nb)));
            } else {
                results.push((addr, None));
            }
        }
        // A live write changed memory under the editor — recompose so it reflects.
        editor.update(cx, |ed, cx| ed.apply_document(cx));
        scanner.update(cx, |sp, cx| sp.apply_change_all(results, cx));
        cx.notify();
    }

    // ── View: font family (the C++ exclusive Consolas / JetBrains Mono picker) ──

    /// View ▸ Font ▸ {Consolas / JetBrains Mono} — set + persist the editor font
    /// family (the C++ `setEditorFont` + settings("font"); main.cpp:5071). The
    /// editor surface owns no live family setter in this port, so the window owns
    /// the selection + persisted setting + the Font submenu ✓.
    fn set_editor_font(&mut self, family: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_font = family.to_string();
        // Persist the selection to the disk store so it survives a relaunch (the
        // C++ `settings.setValue("font", family)`; main.cpp:1311). Loaded back in
        // the ctor.
        self.settings.borrow_mut().set(settings_keys::FONT, family);
        self.sync_font_menu_checked(cx);
        // Push the chosen family into every open editor surface (the C++
        // `setEditorFont` applies the font to all editors; main.cpp:5071).
        // Previously this only persisted the key + synced the menu ✓, so the
        // Font submenu was cosmetic — no editor ever changed font. Split panes
        // view the SAME active editor entity, so iterating the tab editors
        // covers every visible pane.
        let family_ss: SharedString = family.to_string().into();
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, cx| {
                ed.set_font_family(Some(family_ss.clone()), cx);
            });
        }
        self.notify(format!("Editor font: {family}"), window, cx);
    }

    /// Push the active font family into the Font submenu ✓ (exclusive — only the
    /// active family is checked).
    fn sync_font_menu_checked(&mut self, cx: &mut Context<Self>) {
        let consolas = self.editor_font == "Consolas";
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.font.consolas", consolas, cx);
            mb.set_command_checked("view.font.jetbrains", !consolas, cx);
        });
    }

    // ── Tools / Help ──

    /// Tools ▸ RTTI Browser (Ctrl+Shift+R) — open the vtable/RTTI hierarchy browser
    /// for the user's single selected hex/pointer field. Faithful port of the
    /// Tools-menu gate (`main.cpp:1523-1561`) + `MainWindow::showRttiBrowser`
    /// (`main.cpp:4395-4413`):
    ///
    /// 1. Resolve the active editor → its controller → tree + provider.
    /// 2. The Tools-menu gate ([`resolve_field_vtable`]) masks the single selected
    ///    id, requires a Hex32/64 or Pointer32/64 word, computes its absolute
    ///    address, reads the stored word and rejects null — surfacing each
    ///    rejection as the C++ `setAppStatus(...)` string.
    /// 3. Walk RTTI at the candidate vtable ([`resolve_rtti`], MSVC first then the
    ///    additive Itanium fallback); on failure show the walker error / the empty
    ///    placeholder (the C++ `ThemedMessageBox::info("No RTTI Here", …)`).
    /// 4. Otherwise open the [`RttiBrowserDialog`] modal (the C++ `dlg.exec()`).
    ///
    /// The C++ `showRttiBrowser` always walks with `ptrSize = 8` (the `walkRtti`
    /// default arg); mirror that with `max(tree.pointer_size, 8)`.
    fn open_rtti_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::rtti::browser::{
            resolve_field_vtable, resolve_rtti, RttiBrowserDialog, RttiBrowserEvent,
        };

        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };

        // Gate the selection against the live tree + provider, then walk RTTI.
        // Both steps are pure logic on borrows of the editor's controller, so do
        // them inside a single read borrow and surface the outcome afterwards.
        let outcome = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            let tree = ctrl.tree();
            let ptr_size = tree.pointer_size.max(8);
            let sel: Vec<u64> = ctrl.selected_ids().iter().copied().collect();
            let prov = ctrl.provider().clone();
            match resolve_field_vtable(tree, &sel, prov.as_ref()) {
                Err(e) => Err(e.message().to_string()),
                Ok(vtable) => {
                    let info = resolve_rtti(prov.as_ref(), vtable, ptr_size, 64);
                    if !info.ok {
                        Err(if info.error.is_empty() {
                            format!("No RTTI structures found at 0x{vtable:x}.")
                        } else {
                            info.error.clone()
                        })
                    } else {
                        Ok(info)
                    }
                }
            }
        };

        let info = match outcome {
            Ok(info) => info,
            Err(msg) => {
                self.notify(msg, window, cx);
                return;
            }
        };

        let dlg = cx.new(|cx| RttiBrowserDialog::new(info, window, cx));
        let focus = dlg.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dlg,
            window,
            |_w, _dlg, ev: &RttiBrowserEvent, window, cx| match ev {
                RttiBrowserEvent::Close => window.close_dialog(cx),
            },
        ));
        let dlg_for_modal = dlg.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(720.))
                .margin_top(px(80.))
                .close_button(false)
                .child(dlg_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    // ── F3 live declarative-UI host (design §6 Phase 2) ──
    //
    // The session-owned `plugin_manager` already routes a command / UI event /
    // dialog result back to its owning plugin (`handle_command` /
    // `handle_ui_event` / `handle_dialog_closed`); these methods are the gpui side
    // that (1) enumerates the manager's `ui_contributions` to mount panels + inject
    // menu items, (2) builds a scoped `LivePluginHost` per call sequence, and (3)
    // drains the host's collected toasts / open-dialog / re-render requests into
    // `notify` / `open_plugin_dialog` / `rerender_plugin_panel`. With NO contributing
    // plugin loaded (the default build) every loop here is a no-op over an empty
    // list — nothing is mounted, injected, or routed (HARD PARITY).

    /// Mount each enabled plugin-contributed `Panel` into the existing dock area
    /// (design §6 Phase 2). Adds one [`PluginPanel`](super::pluginpanel::PluginPanel)
    /// tab per `UiContribution::Panel`, docked on the contribution's
    /// [`DockSide`](crate::plugin::DockSide) (the demo's is `Right`, so it tabs in
    /// beside Modules/Bookmarks), and subscribes to its
    /// [`PluginPanelEvent`](super::pluginpanel::PluginPanelEvent) so a widget event
    /// routes through the manager. The target dock is **not** forced open, so a
    /// closed dock stays closed (no launch-time behavior change). Empty list ⇒
    /// no-op (parity).
    fn mount_plugin_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::pluginpanel::PluginPanel;
        // Collect the panel contributions first (ends the borrow on the manager
        // before we mount gpui views / subscribe to `self`).
        let panels: Vec<(
            String,
            String,
            crate::plugin::DockSide,
            crate::plugin::ViewTree,
        )> = self
            .plugin_manager
            .ui_contributions()
            .into_iter()
            .filter_map(|c| match c {
                crate::plugin::UiContribution::Panel {
                    id,
                    title,
                    dock,
                    initial,
                } => Some((id, title, dock, initial)),
                _ => None,
            })
            .collect();
        for (id, title, dock, initial) in panels {
            let panel = PluginPanel::view(id.clone(), title, initial, window, cx);
            // Route the panel's events through the manager + a scoped live host.
            let sub = cx.subscribe_in(
                &panel,
                window,
                |this, panel, ev: &super::pluginpanel::PluginPanelEvent, window, cx| {
                    this.route_plugin_panel_event(panel, ev, window, cx);
                },
            );
            self.plugin_panel_subs.push(sub);
            // Tab into the existing dock at the contributed side (does not open it).
            let placement = dock_placement_for(dock);
            let panel_view: std::sync::Arc<dyn gpui_component::dock::PanelView> =
                std::sync::Arc::new(panel.clone());
            self.dock_area.update(cx, |area, cx| {
                area.add_panel(panel_view, placement, None, window, cx);
            });
            self.plugin_panels.push((id, panel));
        }
    }

    /// Route one [`PluginPanelEvent`](super::pluginpanel::PluginPanelEvent) through
    /// the manager (design §6 Phase 2 Elm loop): build a scoped `LivePluginHost`,
    /// call `handle_ui_event`, push any fresh tree back into the panel, then drain
    /// the host's collected requests (toasts → `notify`, open-dialogs →
    /// `open_plugin_dialog`, re-renders → `rerender_plugin_panel`).
    fn route_plugin_panel_event(
        &mut self,
        panel: &Entity<super::pluginpanel::PluginPanel>,
        ev: &super::pluginpanel::PluginPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Scope the host so its `cx` borrow ends before we re-borrow `cx`.
        let (tree, toasts, open_dialogs, rerenders) = {
            let mut host = super::pluginhost::LivePluginHost::new(
                self.document_area.clone(),
                self.settings.clone(),
                window,
                cx,
            );
            let tree =
                self.plugin_manager
                    .handle_ui_event(&ev.view_id, ev.event.clone(), &mut host);
            let r = host.requests();
            (
                tree,
                r.take_toasts(),
                r.take_open_dialogs(),
                r.take_rerenders(),
            )
        };
        if let Some(tree) = tree {
            panel.update(cx, |p, cx| p.set_tree(tree, window, cx));
        }
        self.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
    }

    /// Re-pull a mounted panel's current `ViewTree` from its owning plugin and push
    /// it into the panel (the [`PluginHost::request_rerender`] resolution, design §3
    /// Elm loop). No-op if `view` isn't a mounted panel or the plugin yields no tree.
    fn rerender_plugin_panel(&mut self, view: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tree) = self.plugin_manager.view_tree(view) else {
            return;
        };
        if let Some((_, panel)) = self.plugin_panels.iter().find(|(id, _)| id == view) {
            let panel = panel.clone();
            panel.update(cx, |p, cx| p.set_tree(tree, window, cx));
        }
    }

    /// Dispatch a plugin-owned command id through the manager + a scoped live host
    /// (design §6 Phase 2), draining the host's collected requests. Reached from
    /// [`run_menu_command`](Self::run_menu_command) for an id
    /// [`is_plugin_command`](crate::plugin::PluginManager::is_plugin_command)
    /// recognizes (a contributed menu/palette item).
    fn dispatch_plugin_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (cmd_toast, toasts, open_dialogs, rerenders) = {
            let mut host = super::pluginhost::LivePluginHost::new(
                self.document_area.clone(),
                self.settings.clone(),
                window,
                cx,
            );
            let res = self
                .plugin_manager
                .handle_command(id, serde_json::Value::Null, &mut host);
            let r = host.requests();
            let toasts = r.take_toasts();
            // Surface a command's own `CommandResult::toast` ONLY if the handler
            // didn't already push the same message through `host.show_toast` (the
            // demo's ping does both — collecting both here would double-toast).
            let cmd_toast = res.toast.filter(|m| !toasts.iter().any(|t| t == m));
            (cmd_toast, toasts, r.take_open_dialogs(), r.take_rerenders())
        };
        if let Some(msg) = cmd_toast {
            self.notify(msg, window, cx);
        }
        self.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
    }

    /// Open a plugin-contributed `Dialog` modally (design §6 Phase 2 — the
    /// generalized C++ `selectTarget`), modeled on
    /// [`open_process_picker`](Self::open_process_picker). Pulls the initial tree +
    /// title from the manager, mounts a [`PluginDialog`](super::plugindialog::PluginDialog)
    /// via the proven `window.open_dialog` pattern, and subscribes (on the shared
    /// close-only [`goto_sub`](Self::goto_sub)) to route the dialog's Ui / Closed
    /// events through the manager. No-op if `id` isn't a contributed dialog.
    fn open_plugin_dialog(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(initial) = self.plugin_manager.view_tree(id) else {
            return;
        };
        // The contributed title (fall back to the id if somehow absent).
        let title = self
            .plugin_manager
            .ui_contributions()
            .into_iter()
            .find_map(|c| match c {
                crate::plugin::UiContribution::Dialog { id: did, title, .. } if did == id => {
                    Some(title)
                }
                _ => None,
            })
            .unwrap_or_else(|| id.to_string());

        let dialog = super::plugindialog::PluginDialog::view(id, title, initial, window, cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, ev: &super::plugindialog::PluginDialogEvent, window, cx| match ev {
                super::plugindialog::PluginDialogEvent::Ui { view_id, event } => {
                    let dialog_id = view_id.clone();
                    let (tree, toasts, open_dialogs, close_dialogs, rerenders) = {
                        let mut host = super::pluginhost::LivePluginHost::new(
                            this.document_area.clone(),
                            this.settings.clone(),
                            window,
                            cx,
                        );
                        let tree =
                            this.plugin_manager
                                .handle_ui_event(view_id, event.clone(), &mut host);
                        let r = host.requests();
                        (
                            tree,
                            r.take_toasts(),
                            r.take_open_dialogs(),
                            r.take_close_dialogs(),
                            r.take_rerenders(),
                        )
                    };
                    if let Some(tree) = tree {
                        dialog.update(cx, |d, cx| d.set_tree(tree, window, cx));
                    }
                    this.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
                    // If the plugin asked to close THIS dialog (the demo's Attach),
                    // dismiss the modal.
                    if close_dialogs.iter().any(|id| *id == dialog_id) {
                        window.close_dialog(cx);
                    }
                }
                super::plugindialog::PluginDialogEvent::Closed { view_id, result } => {
                    let (cmd_toast, toasts, open_dialogs, rerenders) = {
                        let mut host = super::pluginhost::LivePluginHost::new(
                            this.document_area.clone(),
                            this.settings.clone(),
                            window,
                            cx,
                        );
                        let res = this.plugin_manager.handle_dialog_closed(
                            view_id,
                            result.clone(),
                            &mut host,
                        );
                        let r = host.requests();
                        let toasts = r.take_toasts();
                        // Surface the plugin's `CommandResult::toast` RETURN (the
                        // footer-Submit path — the demo's submit returns "Attached
                        // to …" rather than calling `host.show_toast`), unless the
                        // handler already pushed the same message through the host
                        // (mirror of `dispatch_plugin_command`'s dedup → no
                        // double-toast).
                        let cmd_toast = res.toast.filter(|m| !toasts.iter().any(|t| t == m));
                        (cmd_toast, toasts, r.take_open_dialogs(), r.take_rerenders())
                    };
                    if let Some(msg) = cmd_toast {
                        this.notify(msg, window, cx);
                    }
                    this.drain_plugin_requests(toasts, open_dialogs, rerenders, window, cx);
                    window.close_dialog(cx);
                }
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(560.))
                .margin_top(px(80.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        cx.notify();
    }

    /// Drain a scoped live host's collected requests into the window: toasts →
    /// [`notify`](Self::notify), open-dialog ids → [`open_plugin_dialog`](Self::open_plugin_dialog),
    /// re-render view ids → [`rerender_plugin_panel`](Self::rerender_plugin_panel).
    /// Shared by every F3 routing path so the drain order is uniform.
    fn drain_plugin_requests(
        &mut self,
        toasts: Vec<String>,
        open_dialogs: Vec<String>,
        rerenders: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for msg in toasts {
            self.notify(msg, window, cx);
        }
        for id in open_dialogs {
            self.open_plugin_dialog(&id, window, cx);
        }
        for view in rerenders {
            self.rerender_plugin_panel(&view, window, cx);
        }
    }

    /// Plugins ▸ Manage Plugins… — open the read-only [`PluginManagerDialog`] (the
    /// C++ `showPluginsDialog`; main.cpp:8821). The C++ lists each loaded
    /// `IPlugin` (name, version, description, type, author) with Load/Unload
    /// buttons backed by native `dlopen`. This platform has no native plugin
    /// loader, so the dialog lists the **built-in provider plugins** the port ships
    /// — the in-scope analogue of "loaded provider plugins" — read-only, with a
    /// note that runtime DLL/SO loading is out of scope. This replaces the bare
    /// notify with the actual (read-only) manager surface.
    fn open_plugins_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Render the dialog from the SESSION-OWNED manager's live plugin set (design
        // §6 Phase 6 / §7.A [fix]) — not a throwaway `with_builtins()`. An
        // enable/disable here flips THIS manager (the same registry the source
        // pickers read) and persists via its DiskSettings-backed store.
        let dialog = cx.new(|cx| {
            PluginManagerDialog::new(
                plugin_infos_from_rows(self.plugin_manager.plugins_view()),
                cx,
            )
        });
        // Surface the session manager's retained native-load failures (design §7.A
        // [fix]) so ABI/load errors are visible with detail in the dialog instead of
        // only logged at startup. Feature-gated: the default build has no loader, so
        // there are no errors to push (and `set_load_errors` doesn't exist there).
        #[cfg(feature = "plugins")]
        {
            let errs = self.plugin_manager.load_errors().to_vec();
            if !errs.is_empty() {
                dialog.update(cx, |d, cx| d.set_load_errors(errs, cx));
            }
        }
        let focus = dialog.read(cx).focus_handle(cx);
        // The dialog reports Close + Toggle; Toggle drives the owned manager and
        // pushes the refreshed rows back so the chip reflects the real state.
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, ev: &PluginManagerEvent, window, cx| match ev {
                PluginManagerEvent::Close => window.close_dialog(cx),
                PluginManagerEvent::Toggle {
                    identifier,
                    enabled,
                } => {
                    // Flip + persist on the session-owned manager (the single source
                    // the pickers read), then re-render the dialog from the refreshed
                    // view so the enabled chip + button label track reality.
                    this.plugin_manager.set_enabled(identifier, *enabled, true);
                    let rows = plugin_infos_from_rows(this.plugin_manager.plugins_view());
                    dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                }
                PluginManagerEvent::Unload { identifier } => {
                    // Safe-unload through the LIVE host (design §7.A [fix]): build a
                    // window-backed host that detaches affected documents FIRST
                    // (manager.rs safe_unload step 1), drop the plugin + its backing
                    // library, then refresh the rows. The host borrows `cx`, so it is
                    // scoped + dropped before we re-borrow `cx` for `notify`/`update`.
                    let toasts = {
                        let mut host = super::pluginhost::LivePluginHost::new(
                            this.document_area.clone(),
                            this.settings.clone(),
                            window,
                            cx,
                        );
                        this.plugin_manager.safe_unload(identifier, &mut host);
                        host.take_toasts()
                    };
                    // Drain any toasts the unload path surfaced (the host can't call
                    // `notify` itself — it lacks `&mut MainWindow`).
                    for msg in toasts {
                        this.notify(msg, window, cx);
                    }
                    let rows = plugin_infos_from_rows(this.plugin_manager.plugins_view());
                    dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                }
                #[cfg(feature = "plugins")]
                PluginManagerEvent::Load => {
                    this.load_plugin_from_path(dialog.clone(), window, cx);
                }
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(620.))
                .margin_top(px(80.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Load a native plugin from a user-chosen path (the C++ load-from-path;
    /// design §6 Phase 3/6) — only compiled behind the `plugins` feature. Pops the
    /// native path picker; on a chosen library, loads it through the session-owned
    /// [`PluginManager`](crate::plugin::PluginManager) (the same registry the
    /// source pickers read), surfaces a load / ABI-mismatch error via a toast
    /// (design §7.A [fix]), and refreshes the open dialog's rows so the new plugin
    /// appears. The chosen-path filter keeps only a real library extension
    /// (`.so`/`.dll`/`.dylib`) so a stray pick is rejected cleanly.
    #[cfg(feature = "plugins")]
    fn load_plugin_from_path(
        &mut self,
        dialog: Entity<PluginManagerDialog>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Load a native plugin (.so/.dll/.dylib)".into()),
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
            // Only accept a real shared-library extension (reject a stray pick).
            let ok_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "so" | "dll" | "dylib"))
                .unwrap_or(false);
            let _ = this.update_in(cx, |me, window, cx| {
                if !ok_ext {
                    me.notify(
                        format!("Not a plugin library: {}", path.display()),
                        window,
                        cx,
                    );
                    return;
                }
                match me.plugin_manager.load_native_plugin(&path) {
                    Ok(id) => {
                        me.notify(format!("Loaded plugin '{id}'"), window, cx);
                        // Refresh the open dialog's rows so the new plugin appears.
                        let rows = plugin_infos_from_rows(me.plugin_manager.plugins_view());
                        dialog.update(cx, |d, cx| d.set_plugins(rows, cx));
                    }
                    Err(e) => {
                        // Surface the load / ABI-mismatch error with detail (design
                        // §7.A [fix] — C++ shows a generic "check the console" box).
                        me.notify(format!("Plugin load failed: {e}"), window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Tools ▸ Performance Profiler (Ctrl+Shift+F) — open the live
    /// [`ProfilerDialog`] (the C++ `ProfilerDialog`; profilerdialog.cpp:106).
    /// The dialog auto-enables profiling on open, takes a live snapshot, and
    /// auto-refreshes at ~2 Hz while shown; it restores the prior profiling
    /// flag on close. Close-only, so it reuses the generic [`goto_sub`](Self::goto_sub)
    /// subscription slot like the other single-button dialogs.
    fn open_profiler_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::dialogs::{ProfilerDialog, ProfilerEvent};
        let dialog = cx.new(|cx| ProfilerDialog::new(window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |_this, _d, _ev: &ProfilerEvent, window, cx| {
                window.close_dialog(cx);
            },
        ));
        // The ProfilerDialog renders its own self-clamping card (820×640); the
        // outer overlay just hosts it (no extra card / close button).
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(820.))
                .margin_top(px(48.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Tools ▸ Start/Stop MCP Server — toggle the MCP bridge flag and flip the
    /// menu label (the C++ `toggleMcp` + dynamic action text; main.cpp:1568). No
    /// live bridge on this platform; the toggle + label are real.
    fn toggle_mcp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mcp_running = !self.mcp_running;
        self.rebuild_menus(cx);
        let msg = if self.mcp_running {
            "MCP Server started."
        } else {
            "MCP Server stopped."
        };
        self.notify(msg, window, cx);
    }

    /// Tools ▸ Type Aliases… — open the [`TypeAliasesDialog`] seeded from the
    /// active document's per-kind alias map (the C++ `showTypeAliasesDialog`). On
    /// accept, apply the edited aliases to the document and recompose so the
    /// editor + generated code reflect the new type names.
    fn open_type_aliases_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Open a document first.", window, cx);
            return;
        };
        let current: std::collections::HashMap<crate::core::NodeKind, String> =
            editor.read(cx).controller().document().type_aliases.clone();
        let dlg = cx.new(|cx| TypeAliasesDialog::new(current, window, cx));
        let editor2 = editor.clone();
        let this = cx.entity().downgrade();
        self.goto_sub = Some(cx.subscribe_in(
            &dlg,
            window,
            move |_w, dlg, ev: &TypeAliasesEvent, window, cx| match ev {
                TypeAliasesEvent::Accept => {
                    let map = dlg.read(cx).collect(cx);
                    window.close_dialog(cx);
                    let _ = this.update(cx, |me, cx| {
                        editor2.update(cx, |ed, cx| {
                            ed.controller_mut().document_mut().type_aliases = map;
                            ed.apply_document(cx);
                        });
                        me.sync_dirty_state(cx);
                        me.notify("Type aliases updated", window, cx);
                        cx.notify();
                    });
                }
                TypeAliasesEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let focus = dlg.read(cx).focus_handle(cx);
        let dlg_for_modal = dlg.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(460.))
                .margin_top(px(80.))
                .close_button(false)
                .child(dlg_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Tools ▸ Options — open the [`OptionsDialog`] seeded from the live window
    /// state, subscribe to its Apply event, and on accept apply each field to
    /// the live window/editors/controllers + persist via the disk store (the
    /// C++ `showOptionsDialog` → apply + `QSettings::setValue`).
    fn open_options_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::optionsdialog::{OptionsDialog, OptionsEvent, OptionsResult};
        let themes: Vec<String> = self
            .theme_manager
            .borrow()
            .themes()
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let theme_index = self.theme_manager.borrow().current_index();
        // Seed the dialog with the LIVE persisted state (the C++ builds `current`
        // from `m_menuBarTitleCase` / `showIcon` etc; main.cpp:5019). Previously
        // these two were hardcoded, so the dialog never reflected — and Apply could
        // never preserve — the user's real preference.
        let current = OptionsResult {
            theme_index,
            font_name: self.editor_font.clone(),
            menu_bar_title_case: self.menu_bar_title_case,
            show_icon: self.show_icon,
            auto_start_mcp: self.auto_start_mcp,
            refresh_ms: self.refresh_ms,
            generator_asserts: self.generator_asserts,
            brace_wrap: self.brace_wrap,
        };
        let dialog = cx.new(|cx| OptionsDialog::new(current, themes, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.options_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &OptionsEvent, window, cx| match ev {
                OptionsEvent::Apply(result) => {
                    let result = result.clone();
                    window.close_dialog(cx);
                    this.apply_options(result, window, cx);
                }
                OptionsEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(640.))
                .margin_top(px(80.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// View ▸ Edit Theme… — open the dedicated [`ThemeEditor`] (the swatch grid /
    /// live preview / theme combo / name edit / save-as-user-copy) for the
    /// active theme (the C++ `editTheme`; main.cpp). On accept the editor has
    /// already committed via the manager (`updateTheme` — persists + commits the
    /// preview + re-styles), so the host only re-syncs its theme state + the
    /// Theme submenu ✓; on reject the editor reverts the live preview
    /// (`revertPreview`). Previously `view.theme_edit` routed to the Options
    /// dialog and this fully-built view had no caller (item 4).
    fn open_theme_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::theme::editor::{ThemeEditor, ThemeEditorEvent};
        let index = self.theme_manager.borrow().current_index();
        let dialog = cx.new(|cx| ThemeEditor::new(index, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.theme_editor_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &ThemeEditorEvent, window, cx| match ev {
                // Saved: the manager already committed + re-styled in
                // `ThemeEditor::save` (the C++ `updateTheme`); mirror the commit
                // into the host's theme state + Theme submenu ✓.
                ThemeEditorEvent::Saved(_index) => {
                    window.close_dialog(cx);
                    let name = this.theme_manager.borrow().current().name.clone();
                    this.state.set_theme_name(&name);
                    this.settings.borrow_mut().set(settings_keys::THEME, &name);
                    this.sync_theme_menu_checked(cx);
                    cx.notify();
                }
                // Cancelled: the editor already reverted the live preview (the
                // C++ `revertPreview`); just dismiss.
                ThemeEditorEvent::Cancelled => {
                    window.close_dialog(cx);
                    cx.notify();
                }
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(480.))
                .margin_top(px(60.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Apply an accepted [`OptionsResult`] to the live app + persist every field
    /// (the C++ Options "OK" path). Theme + font reuse the existing switch/set
    /// helpers (which persist on their own); the remaining fields persist here.
    fn apply_options(
        &mut self,
        result: super::optionsdialog::OptionsResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Theme (the C++ `m_themeCombo` → `setCurrent`). `switch_theme` re-styles
        // + records the active name; persist it via the disk store under "theme".
        let theme_name = self
            .theme_manager
            .borrow()
            .themes()
            .get(result.theme_index)
            .map(|t| t.name.clone());
        if let Some(name) = theme_name {
            self.switch_theme(result.theme_index, window, cx);
            self.sync_theme_menu_checked(cx);
            self.settings.borrow_mut().set(settings_keys::THEME, &name);
        }
        // Font (reuses `set_editor_font`, which persists "font" + syncs the menu).
        if result.font_name != self.editor_font && !result.font_name.is_empty() {
            self.set_editor_font(&result.font_name.clone(), window, cx);
        }
        // Refresh interval — push into every controller + persist "refreshMs".
        self.refresh_ms = result.refresh_ms.clamp(
            super::optionsdialog::REFRESH_MIN,
            super::optionsdialog::REFRESH_MAX,
        );
        self.settings
            .borrow_mut()
            .set(settings_keys::REFRESH_MS, &self.refresh_ms.to_string());
        // Brace-wrap (generator) — push into every controller + persist.
        self.brace_wrap = result.brace_wrap;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::BRACE_WRAP, self.brace_wrap);
        self.generator_asserts = result.generator_asserts;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::GENERATOR_ASSERTS, self.generator_asserts);
        // Reflect the new assert flag into the code-view so the live rendered pane
        // updates without a tab switch (the C++ `refreshAllRendered`; main.cpp:2451).
        let emit_asserts = self.generator_asserts;
        self.document_area.update(cx, |area, cx| {
            area.set_generator_asserts(emit_asserts, cx);
        });
        // Push refresh + brace-wrap into every open controller.
        let refresh_ms = self.refresh_ms;
        let brace_wrap = self.brace_wrap;
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, _cx| {
                ed.controller_mut().set_refresh_interval(refresh_ms);
                ed.controller_mut().set_brace_wrap(brace_wrap);
            });
        }
        // MCP autostart — persist; reflect the label via the running flag.
        self.auto_start_mcp = result.auto_start_mcp;
        self.settings
            .borrow_mut()
            .set_bool(settings_keys::AUTO_START_MCP, self.auto_start_mcp);
        // Menu-bar title-case (the C++ `r.menuBarTitleCase` → `applyMenuBarTitleCase`
        // + persist "menuBarTitleCase"; main.cpp:5041). Push into the live menu bar
        // so the titles re-case immediately, then persist across launches.
        if result.menu_bar_title_case != self.menu_bar_title_case {
            self.menu_bar_title_case = result.menu_bar_title_case;
            let title_case = self.menu_bar_title_case;
            self.menubar
                .update(cx, |mb, cx| mb.set_title_case(title_case, cx));
            self.settings
                .borrow_mut()
                .set_bool(settings_keys::MENU_BAR_TITLE_CASE, self.menu_bar_title_case);
        }
        // Titlebar show-icon (the C++ `r.showIcon` → `m_titleBar->setShowIcon` +
        // persist "showIcon"; main.cpp:5046). When on, the titlebar swaps its bold
        // "Reclass" text for a class-icon badge (the C++ `setShowIcon(true)` clears
        // `m_appLabel`'s text and sets it to the class.png pixmap; titlebar.cpp:202-
        // 214). The flag re-renders the bar on the next paint (see the
        // `render_titlebar` call site, which passes `self.show_icon`).
        if result.show_icon != self.show_icon {
            self.show_icon = result.show_icon;
            self.settings
                .borrow_mut()
                .set_bool(settings_keys::SHOW_ICON, self.show_icon);
        }
        self.notify("Options applied.", window, cx);
        cx.notify();
    }

    /// Help ▸ About Reclass — a themed message box with build info + a note on the
    /// project (the C++ `about()` themed dialog; main.cpp:4415). The GitHub button
    /// is folded into the body text (the modal message box is single-button).
    fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = super::messagebox::info(
            "About Reclass",
            &format!(
                "Reclass {} — a Rust + GPUI port of ReClass.\n\nA memory structure editor.\n\
                 GitHub: {ABOUT_GITHUB_URL}",
                env!("CARGO_PKG_VERSION")
            ),
        );
        super::messagebox::open_message(spec, window, cx);
    }

    /// Help ▸ Keyboard Shortcuts… (F1) — a themed reference of the bound
    /// accelerators (the C++ `showShortcutsDialog`; main.cpp:4440).
    fn show_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detail = vec![
            "Ctrl+N / Ctrl+T / Ctrl+E — New Class / Struct / Enum".to_string(),
            "Ctrl+O — Open    Ctrl+S — Save    Ctrl+Shift+S — Save As".to_string(),
            "Ctrl+W — Close    Ctrl+Z / Ctrl+Y — Undo / Redo".to_string(),
            "Ctrl+B — Add Bookmark    Ctrl+Alt+B — Quick Bookmark".to_string(),
            "F5 — Refresh    Ctrl+G — Go to Address".to_string(),
            "Ctrl+K / Ctrl+Shift+P / F1 — Command Palette / Shortcuts".to_string(),
            "Ctrl+Shift+S — Memory Scanner    Ctrl+Shift+Y — Modules".to_string(),
            "Ctrl+Shift+B — Bookmarks    Ctrl+\\ — Split Editor".to_string(),
        ];
        let mut spec = super::messagebox::info("Keyboard Shortcuts", "Bound accelerators:");
        spec.detail = detail;
        super::messagebox::open_message(spec, window, cx);
    }

    // ── Recent files (the C++ recentFiles QSettings list) ──

    /// Record an opened project path as the most-recent (the C++ `addRecentFile`;
    /// main.cpp:8765): dedup, most-recent-first, capped at 10. Rebuilds the menus
    /// + the start-page list.
    fn record_recent_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.recent_files.retain(|p| p != &abs);
        self.recent_files.insert(0, abs);
        self.recent_files.truncate(10);
        // Persist the list to the disk store (the C++ `addRecentFile` →
        // `settings.setValue("recentFiles", recent)`; main.cpp:8765) so Open
        // Recent survives a relaunch. Loaded back in the ctor.
        self.persist_recent_files();
        self.rebuild_menus(cx);
    }

    /// Write the in-memory recent-files list to the disk store as a
    /// `\n`-joined `QStringList` (the C++ `recentFiles` key).
    fn persist_recent_files(&self) {
        let values: Vec<String> = self
            .recent_files
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.settings
            .borrow_mut()
            .set_list(settings_keys::RECENT_FILES, &values);
    }

    /// The recent-files paths that still exist on disk, most-recent-first. Both
    /// the Recent Files submenu and the start page skip entries whose file no
    /// longer exists (the C++ `updateRecentFilesMenu` `if (!QFile::exists(path))
    /// continue;`; main.cpp:8789). Indices map back into the stored vec so a
    /// reopen targets the right path.
    fn existing_recent_files(&self) -> Vec<(usize, &std::path::PathBuf)> {
        self.recent_files
            .iter()
            .enumerate()
            .filter(|(_, p)| p.exists())
            .collect()
    }

    /// Reopen a recent file from its `file.recent.<index>` command id.
    fn open_recent_by_command(&mut self, cmd: &str, window: &mut Window, cx: &mut Context<Self>) {
        if cmd == "file.recent.empty" {
            return;
        }
        let Some(idx) = cmd
            .strip_prefix("file.recent.")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        let Some(path) = self.recent_files.get(idx).cloned() else {
            return;
        };
        self.open_project(&path, None, window, cx);
    }

    /// Switch the active saved data source from a `source.saved.<index>` command
    /// id (the C++ `m_sourceMenu` saved-source row → `switchToSavedSource(idx)`).
    /// Recomposes the editor, re-derives the tab source icon, re-feeds the docks,
    /// and rebuilds the menus so the new active row is checked.
    fn switch_saved_source_by_command(
        &mut self,
        cmd: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = cmd
            .strip_prefix("source.saved.")
            .and_then(|s| s.parse::<i32>().ok())
        else {
            return;
        };
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        editor.update(cx, |ed, cx| {
            ed.controller_mut().switch_to_saved_source(idx);
            ed.apply_document(cx);
        });
        // Re-derive the tab source icon + re-feed the docks from the new source.
        let source = Self::source_for_controller(editor.read(cx).controller());
        if let Some(active_id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(active_id, source.clone(), cx);
            });
            self.state.set_source(active_id, source);
        }
        self.refresh_docks_for_active(cx);
        self.rebuild_menus(cx);
        cx.notify();
    }

    // ── Dynamic menu rebuild (the C++ aboutToShow rebuilders) ──

    /// Rebuild the menu tree with the live Recent-Files + Data-Source rows and the
    /// dynamic MCP Start/Stop label, then push it into the menu bar (the C++
    /// `updateRecentFilesMenu` / `populateSourceMenu` / MCP label flip). Preserves
    /// the checkmark state (held separately on the menu bar).
    fn rebuild_menus(&mut self, cx: &mut Context<Self>) {
        use super::commandpalette::{menu_tree_with, RecentMenuEntry, SourceMenuEntry};
        // Skip entries whose file no longer exists (the C++
        // `updateRecentFilesMenu` exists-filter); the command carries the
        // ORIGINAL stored index so a reopen targets the right path.
        let existing = self.existing_recent_files();
        // Two recent entries can be different files that share a base name
        // (e.g. /tmp/parity/png.rcx vs /tmp/example/png.rcx). The C++ leans on a
        // per-action tooltip to disambiguate (main.cpp:8793), but this port's
        // menu rows have no hover tooltip, so a bare "png.rcx" twice is visually
        // identical. When a file name repeats among the visible entries, append a
        // parent-directory hint so each row is distinguishable, e.g.
        // "png.rcx — parity" / "png.rcx — example".
        let mut name_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut hint_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (_, p) in &existing {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                *name_counts.entry(name).or_insert(0) += 1;
            }
            if let Some(dir) = p
                .parent()
                .and_then(|d| d.file_name())
                .and_then(|s| s.to_str())
            {
                *hint_counts.entry(dir).or_insert(0) += 1;
            }
        }
        let recent: Vec<RecentMenuEntry> = existing
            .into_iter()
            .map(|(i, p)| {
                let file_name = p.file_name().and_then(|s| s.to_str()).unwrap_or("(file)");
                let label = if name_counts.get(file_name).copied().unwrap_or(0) > 1 {
                    // Prefer the short parent-dir name as the hint, but if that
                    // name itself is shared by another visible entry it would not
                    // disambiguate — fall back to the full parent path.
                    let short = p
                        .parent()
                        .and_then(|d| d.file_name())
                        .and_then(|s| s.to_str());
                    let hint = match short {
                        Some(dir) if hint_counts.get(dir).copied().unwrap_or(0) <= 1 => {
                            Some(dir.to_string())
                        }
                        _ => p.parent().map(|d| d.to_string_lossy().into_owned()),
                    };
                    match hint {
                        Some(h) => format!("{file_name} \u{2014} {h}"),
                        None => file_name.to_string(),
                    }
                } else {
                    file_name.to_string()
                };
                RecentMenuEntry {
                    label,
                    command: format!("file.recent.{i}"),
                }
            })
            .collect();
        // Saved sources from the active document's controller (the active one is
        // rendered checked via the host's checked-set).
        let sources: Vec<SourceMenuEntry> = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| {
                let ctrl = ed.read(cx).controller();
                let active = ctrl.active_source_index();
                ctrl.saved_sources()
                    .iter()
                    .enumerate()
                    .map(|(i, s)| SourceMenuEntry {
                        label: format!("{} '{}'", s.kind, s.display_name),
                        command: format!("source.saved.{i}"),
                        active: i as i32 == active,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut tree = menu_tree_with(&recent, &sources);
        // Flip the MCP label (the dynamic Start/Stop text; main.cpp:1568).
        let mcp_label = if self.mcp_running {
            "Stop MCP Server"
        } else {
            "Start MCP Server"
        };
        relabel_command(&mut tree, "tools.mcp", mcp_label);
        // Inject any enabled plugin-contributed menu commands into the &Plugins
        // submenu (design §6 Phase 2). With no contributing plugin loaded (the
        // default build) `ui_contributions()` is empty, so the tree is byte-identical
        // to before — the &Plugins submenu keeps only [Manage Plugins…].
        let plugin_commands = self.plugin_manager.ui_contributions();
        inject_plugin_menu_items(&mut tree, &plugin_commands);
        self.menubar.update(cx, |mb, cx| mb.set_menus(tree, cx));
        // After rebuilding the tree, re-push the active-source checkmark so the
        // saved-source row stays checked across the rebuild.
        let active_cmd = sources
            .iter()
            .position(|s| s.active)
            .map(|i| format!("source.saved.{i}"));
        if let Some(cmd) = active_cmd {
            self.menubar
                .update(cx, |mb, cx| mb.set_command_checked(&cmd, true, cx));
        }
    }

    // ── Unsaved-changes guard + quit (the C++ closeEvent + project_close) ──

    /// File ▸ Exit — if any open document is modified, show the 3-way
    /// unsaved-changes guard (Save changes / Discard / Cancel) before quitting
    /// (the C++ `closeEvent`; main.cpp:8984): **Save** persists each dirty
    /// document and aborts the quit on the first save failure, **Discard** quits
    /// without saving, **Cancel** aborts. With nothing dirty, quit immediately.
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dirty = self.collect_dirty_docs(cx);
        if dirty.is_empty() {
            cx.quit();
            return;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| {
                // Save succeeded for every dirty doc → quit.
                let _ = (me, window);
                cx.quit();
            },
            |_me, _window, cx| cx.quit(), // Discard → quit without saving.
            window,
            cx,
        );
    }

    /// The single window-close guard (the C++ `MainWindow::closeEvent`;
    /// main.cpp:8984-9031). Intercepts the OS/WM close (Alt+F4, `WM_DELETE_WINDOW`)
    /// and the in-app titlebar X, mirroring Qt's accept/ignore protocol:
    ///
    /// - **return `true`** ⇒ allow the close (the C++ `event->accept()`),
    /// - **return `false`** ⇒ abort *this* close (the C++ `event->ignore()`).
    ///
    /// With nothing dirty it returns `true` immediately (main.cpp:8998). Otherwise
    /// it opens the **async** 3-way unsaved-changes guard and returns `false`
    /// (abort) — the dialog is asynchronous, so the chosen Save/Discard branch
    /// performs the *actual* close itself ([`Window::remove_window`], the gpui
    /// analog of `event->accept()` for this single-window app). **Cancel** keeps the
    /// window open (no re-close), matching the C++ `event->ignore()` on Cancel
    /// (main.cpp:9013-9015).
    ///
    /// The programmatic re-close from the Save/Discard branch fires this hook
    /// again; the [`closing`](Self::closing) re-entrancy guard (the C++
    /// `ClosingGuard m_closingAll`; app-shell.md:167) makes that second pass return
    /// `true` without re-prompting.
    fn guarded_window_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Re-entrant close from the Save/Discard branch below → allow it through
        // without re-prompting (the C++ closing-all guard).
        if self.closing {
            return true;
        }
        let dirty = self.collect_dirty_docs(cx);
        if dirty.is_empty() {
            // Clean document set → accept the close (main.cpp:8998).
            return true;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| {
                // Save succeeded for every dirty doc → perform the close.
                me.closing = true;
                window.remove_window();
                let _ = cx;
            },
            |me, window, cx| {
                // Discard → close without saving.
                me.closing = true;
                window.remove_window();
                let _ = cx;
            },
            window,
            cx,
        );
        // Abort *this* close — the async dialog's Save/Discard branch re-closes
        // (the C++ `event->ignore()` while the modal decides).
        false
    }

    /// Collect the open editors whose document is modified, deduped by document
    /// (the C++ `closeEvent` walks `m_tabs`, skipping repeat docs; here each tab
    /// owns its editor so we dedup by [`DocId`]). Each entry is
    /// `(doc id, editor, display name)` where the name is the file name when the
    /// document has a path, else the view-root struct name (the C++ name rule;
    /// main.cpp:8991-8993).
    fn collect_dirty_docs(
        &self,
        cx: &Context<Self>,
    ) -> Vec<(Entity<super::editor::RcxEditor>, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for t in self.document_area.read(cx).tabs() {
            if seen.contains(&t.id) {
                continue;
            }
            let ed = t.editor.read(cx);
            let ctrl = ed.controller();
            let doc = ctrl.document();
            let Some(name) = dirty_doc_name(
                doc.modified,
                doc.file_path.as_deref(),
                ctrl.tree(),
                ctrl.view_root_id(),
            ) else {
                continue;
            };
            seen.insert(t.id);
            out.push((t.editor.clone(), name));
        }
        out
    }

    /// Open the 3-way unsaved-changes guard for `dirty` docs (the C++
    /// `ThemedMessageBox::unsavedChanges`). On **Save changes** it persists every
    /// dirty doc through its editor (the C++ `project_save(dock,false)` per doc),
    /// and only runs `on_saved` when ALL saved; on the first failure it reports +
    /// aborts (the C++ `event->ignore()` on a failed save). On **Discard** it runs
    /// `on_discard`. On **Cancel** (or Esc) it dismisses with no action.
    fn open_unsaved_guard<S, D>(
        &mut self,
        dirty: Vec<(Entity<super::editor::RcxEditor>, String)>,
        on_saved: S,
        on_discard: D,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        S: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        D: Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    {
        let names = unique_dirty_names(dirty.iter().map(|(_, n)| n.clone()));
        let text = unsaved_changes_text(names.len());
        let editors: Vec<Entity<super::editor::RcxEditor>> =
            dirty.iter().map(|(e, _)| e.clone()).collect();
        let dialog = cx.new(|cx| RcxUnsavedDialog::new("Unsaved Changes", &text, names, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        let on_saved = Rc::new(on_saved);
        let on_discard = Rc::new(on_discard);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            move |this, _d, choice: &super::messagebox::UnsavedChoice, window, cx| {
                use super::messagebox::UnsavedChoice;
                match choice {
                    UnsavedChoice::Cancel => window.close_dialog(cx),
                    UnsavedChoice::Discard => {
                        window.close_dialog(cx);
                        on_discard(this, window, cx);
                    }
                    UnsavedChoice::Save => {
                        // Persist each dirty doc; abort on the first failure (the
                        // C++ `if (!project_save(...)) { event->ignore(); return; }`).
                        let all_saved = this.save_dirty_docs(&editors, window, cx);
                        if all_saved {
                            window.close_dialog(cx);
                            on_saved(this, window, cx);
                        }
                        // On failure the dialog stays open + a notification was
                        // raised, mirroring the C++ aborted close.
                    }
                }
            },
        ));
        let dialog_for_modal = dialog.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(super::messagebox::MSG_MAX_WIDTH))
                .margin_top(px(80.))
                .close_button(false)
                .child(dialog_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Persist each editor's document to its known path (the C++
    /// `project_save(dock,false)` per dirty doc). Returns `true` only if every doc
    /// was written; a doc with NO file path can't be saved synchronously here, so
    /// it counts as a failure (a notification points the user at Save As). On a
    /// write failure it notifies + returns `false` (the C++ aborts the close).
    fn save_dirty_docs(
        &mut self,
        editors: &[Entity<super::editor::RcxEditor>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        for editor in editors {
            let path = editor.read(cx).controller().document().file_path.clone();
            let Some(path) = path else {
                self.notify(
                    "An unsaved document has no file yet — use File ▸ Save As… first.",
                    window,
                    cx,
                );
                return false;
            };
            let ok = editor.update(cx, |ed, _cx| ed.controller_mut().document_mut().save(&path));
            if !ok {
                self.notify(format!("Failed to save {}", path.display()), window, cx);
                return false;
            }
            self.record_recent_file(&path, cx);
        }
        // Reflect cleared dirty bits in the tab dots.
        self.sync_dirty_state(cx);
        true
    }

    /// Recompute the OS window title from the active document and push it to the
    /// platform window (the C++ `updateWindowTitle`; main.cpp:5172). The title is
    /// `"<rootName>[ *] - Reclass"` where `<rootName>` is the struct name of the
    /// active tab's **view root** and the trailing `" *"` marks an unsaved
    /// document; with no active document it is plain `"Reclass"`. Call on every
    /// active-tab change, view-root change, and dirty-state change.
    fn update_window_title(&self, window: &mut Window, cx: &Context<Self>) {
        let title = self.compute_window_title(cx);
        window.set_window_title(&title);
    }

    /// The window-title string for the active document (the pure half of
    /// [`update_window_title`], so it can be unit-tested via
    /// [`window_title_string`]). Reads the active editor's controller (tree +
    /// view root + dirty bit); falls back to `"Reclass"` when no document is open.
    fn compute_window_title(&self, cx: &Context<Self>) -> String {
        let Some(editor) = self.document_area.read(cx).active_editor() else {
            return "Reclass".to_string();
        };
        let ed = editor.read(cx);
        let ctrl = ed.controller();
        let name = root_name_for_title(ctrl.tree(), ctrl.view_root_id());
        let modified = ctrl.document().modified;
        window_title_string(&name, modified)
    }

    /// Push the active document's modified state into its tab's dirty dot (the
    /// bug: the controller tracks `doc.modified` but the window never propagated
    /// it). Mirrors the C++ tab-title dirty marker.
    fn sync_dirty_state(&mut self, cx: &mut Context<Self>) {
        let updates: Vec<(DocId, bool)> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| (t.id, t.editor.read(cx).controller().document().modified))
            .collect();
        self.document_area.update(cx, |area, cx| {
            for (id, modified) in updates {
                area.set_modified(id, modified, cx);
            }
        });
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
            // Record the just-saved path in the recent-files list (the C++ `saveFile`
            // / `saveFileAs` call `addRecentFile(path)` after a successful write;
            // main.cpp). Without this, Save As to a new path never surfaced it in
            // Recent Files or the start page until the next Open.
            self.record_recent_file(path, cx);
            self.notify(format!("Saved {}", path.display()), window, cx);
        } else {
            self.notify(format!("Failed to save {}", path.display()), window, cx);
        }
    }

    /// File ▸ Export ▸ … — render the active document's tree to the chosen code
    /// format ([`crate::generator`] / the XML exporter) and prompt a save path.
    /// Mirrors the C++ `exportToFile` (main.cpp:5752-5774): it exports the **full
    /// SDK** (`renderCodeAll` — every root struct, ignoring the current view root)
    /// with the document's `typeAliases` and the persisted `generatorAsserts`
    /// flag, and offers the format's [`code_format_file_filter`] in the save
    /// dialog (GPUI's `prompt_for_new_path` has no filter slot, so the filter
    /// drives only the suggested extension).
    fn export_code(&mut self, kind: ExportKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("No document to export.", window, cx);
            return;
        };
        let emit_asserts = self.generator_asserts;
        let rendered = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            // The document's per-kind name overrides (the C++ `tab->doc->typeAliases`,
            // passed as `nullptr` when empty; main.cpp:5761-5762).
            let aliases = &ctrl.document().type_aliases;
            let aliases = if aliases.is_empty() {
                None
            } else {
                Some(aliases)
            };
            kind.render(ctrl.tree(), aliases, emit_asserts)
        };
        let Some(text) = rendered else {
            self.notify("Nothing to export from this document.", window, cx);
            return;
        };
        // The C++ passes `codeFormatFileFilter(fmt)` to the save dialog; GPUI has
        // no filter parameter, so we surface it through the suggested extension.
        let _filter = kind.file_filter();
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

    /// File ▸ Close Project (Ctrl+W) — close the active document tab. If the
    /// active document is modified, show the 3-way unsaved-changes guard first
    /// (the C++ `closeFile` → unsaved prompt): **Save** persists the active doc
    /// then closes (aborting on a save failure), **Discard** closes without
    /// saving, **Cancel** aborts. The document area never leaves a blank window
    /// (it re-seeds a fresh tab when the last closes).
    fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.document_area.read(cx).active_editor().cloned();
        let dirty: Vec<(Entity<super::editor::RcxEditor>, String)> = match active {
            Some(editor) if editor.read(cx).controller().document().modified => {
                let ed = editor.read(cx);
                let ctrl = ed.controller();
                let doc = ctrl.document();
                let name = match &doc.file_path {
                    Some(p) => p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| root_name_for_title(ctrl.tree(), ctrl.view_root_id())),
                    None => root_name_for_title(ctrl.tree(), ctrl.view_root_id()),
                };
                vec![(editor.clone(), name)]
            }
            _ => Vec::new(),
        };
        if dirty.is_empty() {
            self.do_close_active(window, cx);
            return;
        }
        self.open_unsaved_guard(
            dirty,
            |me, window, cx| me.do_close_active(window, cx), // Saved → close.
            |me, window, cx| me.do_close_active(window, cx), // Discard → close.
            window,
            cx,
        );
    }

    /// Actually close the active tab (after any unsaved-changes guard).
    fn do_close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.document_area.read(cx).active_index();
        self.document_area.update(cx, |area, cx| {
            area.close_index(ix, window, cx);
        });
        // The area emits `Closed`/`NewDocumentRequested` which our doc-area event
        // handler mirrors into AppState + rebuilds the workspace.
        cx.notify();
    }

    /// Modules dock ▸ Download All (the C++ `download_all`): load/download PDB
    /// symbols for every module of the active source. Enumerate the active
    /// provider's modules; for each, prefer an already-cached or module-adjacent
    /// PDB (the C++ `findCached`/`findLocal`, which need no network). A real
    /// network fetch needs each module's PE debug GUID/age — only obtainable from
    /// a live process target (out of scope on this platform; see
    /// `provider::native`), so report what was resolvable and how many modules
    /// were seen. With no modules (a File source enumerates none) this guides the
    /// user to attach a live source.
    fn download_all_module_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            self.notify("Attach a data source first.", window, cx);
            return;
        };
        let modules = editor
            .read(cx)
            .controller()
            .document()
            .provider
            .enumerate_modules();
        if modules.is_empty() {
            self.notify(
                "No modules to download symbols for — attach a live process source \
                 (file sources expose no loaded modules).",
                window,
                cx,
            );
            return;
        }
        let downloader = crate::rtti::downloader::SymbolDownloader::new();
        let mut resolved = 0usize;
        for m in &modules {
            // Without PE debug info we can't form a download request; but if a PDB
            // is already cached or sits next to the module we can count it as
            // resolvable (the C++ short-circuits on findCached/findLocal first).
            let pdb_name = if m.name.to_ascii_lowercase().ends_with(".pdb") {
                m.name.clone()
            } else {
                // Best-effort: <module-stem>.pdb beside the module image.
                let stem = std::path::Path::new(&m.name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&m.name);
                format!("{stem}.pdb")
            };
            if crate::rtti::downloader::SymbolDownloader::find_local(&m.full_path, &pdb_name)
                .is_some()
            {
                resolved += 1;
                continue;
            }
            let _ = &downloader; // (network fetch needs live PE debug GUID/age)
        }
        self.notify(
            format!(
                "Download All: {resolved}/{} module symbols resolved from cache/local. \
                 Network PDB fetch needs a live process target.",
                modules.len()
            ),
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
        self.set_view_option_value(opt, value, cx);
    }

    /// Set a checkable View option to an explicit `value` (shared by
    /// [`toggle_view_option`] and the in-editor `ViewOptionToggled` event): push
    /// the value into EVERY open editor, mirror it into the window's `view_opts`,
    /// refresh the menu ✓, and persist it. Pushing to all panes is idempotent for
    /// the editor that originated an in-editor toggle.
    fn set_view_option_value(&mut self, opt: ViewOpt, value: bool, cx: &mut Context<Self>) {
        self.view_opts.set(opt, value);
        // Push the new value into EVERY open editor via the EDITOR SETTER
        // CONTRACT (the C++ applies each view option to all open tabs, not just
        // the active one; main.cpp:1339-1411). The compose flags
        // (tree_lines/type_hints/show_comments) recompose; the render-level flags
        // (compact/relative/hover/minimap) repaint. The editor owns the actual
        // effect; the window owns the ✓.
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
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
        // Persist the toggle to the disk store so it survives a relaunch (the
        // C++ `QSettings(...).setValue(key, checked)`; main.cpp:1336-1411).
        self.settings
            .borrow_mut()
            .set_bool(ViewOptions::key(opt), value);
        cx.notify();
    }

    /// Push the window's current [`view_opts`](Self::view_opts) into a single
    /// editor via the EDITOR SETTER CONTRACT. Fresh editors (the initial tab, a
    /// `new_document`, an `open_project`) get controller/view defaults that
    /// DISAGREE with the window's persisted view_opts, so without this a new tab
    /// ignores the current compact-columns/tree-lines/type-hints/comments/hover/
    /// minimap/relative-offsets state (the C++ applies every view option to all
    /// tabs). Calling this after `set_document` realigns the editor.
    fn apply_view_opts_to_editor(
        &self,
        editor: &Entity<super::editor::RcxEditor>,
        cx: &mut Context<Self>,
    ) {
        let o = self.view_opts;
        let brace_wrap = self.brace_wrap;
        let refresh_ms = self.refresh_ms;
        // The persisted editor font family (the C++ applies the chosen font to
        // every editor; main.cpp). New editors start `font_family: None` (the
        // mono default), so without this push a fresh tab ignores the saved
        // View ▸ Font selection. An empty string means "no override" → mono.
        let font_family: Option<SharedString> = if self.editor_font.trim().is_empty() {
            None
        } else {
            Some(self.editor_font.clone().into())
        };
        editor.update(cx, |ed, cx| {
            ed.set_compact_columns(o.compact_columns, cx);
            ed.set_tree_lines(o.tree_lines, cx);
            ed.set_relative_offsets(o.relative_offsets, cx);
            ed.set_type_hints(o.type_hints, cx);
            ed.set_show_comments(o.show_comments, cx);
            ed.set_hover_effects(o.hover_effects, cx);
            ed.set_minimap(o.minimap, cx);
            ed.set_font_family(font_family.clone(), cx);
            // Generator brace-wrap + the persisted refresh interval are
            // controller-level (the C++ pushes both into every controller).
            ed.controller_mut().set_brace_wrap(brace_wrap);
            ed.controller_mut().set_refresh_interval(refresh_ms);
        });
    }

    /// Build the editor split panes (View ▸ Split Editor) — one element per extra
    /// pane in [`split_panes`](Self::split_panes). Each pane views the SAME active
    /// document as the primary editor (the C++ `SplitPane` binds to the tab's
    /// controller) with its OWN view mode: a Tree pane embeds the live editor
    /// entity; a Rendered pane shows the generated C/C++ for the view root. Each
    /// pane carries a header with a per-pane Tree/Code segmented toggle (the C++
    /// per-`SplitPane` view-mode combo) and an "✕" that removes it. Returns an
    /// empty vec when unsplit (the primary pane is the dock area itself).
    fn render_split_panes(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        use super::design::color;
        if self.split_panes.is_empty() {
            return Vec::new();
        }
        let editor = self.document_area.read(cx).active_editor().cloned();
        self.split_panes
            .iter()
            .copied()
            .enumerate()
            .map(|(pane_ix, mode)| {
                let inner = self.render_one_split_pane(pane_ix, mode, editor.as_ref(), cx);
                // Each pane is an equal-flex column with a left divider separating it
                // from the dock area / its sibling panes (Zed split gutter).
                gpui_component::v_flex()
                    .id(SharedString::from(format!("rcx-split-pane-{pane_ix}")))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(color::border(cx))
                    .bg(color::content_bg(cx))
                    .child(inner)
                    .into_any_element()
            })
            .collect()
    }

    /// Render one split pane's header (a Tree/Code segmented toggle + remove "✕")
    /// stacked over its body (the live editor for Tree, the generated source for
    /// Rendered). Shared by [`render_split_panes`](Self::render_split_panes).
    fn render_one_split_pane(
        &self,
        pane_ix: usize,
        mode: ViewMode,
        editor: Option<&Entity<super::editor::RcxEditor>>,
        cx: &Context<Self>,
    ) -> AnyElement {
        use super::design::{color, tokens};

        // ── Header: per-pane view-mode segmented toggle + close button. ──
        let segment = |label: &'static str, this_mode: ViewMode, cx: &Context<Self>| {
            let selected = mode == this_mode;
            div()
                .id(SharedString::from(format!("split-seg-{pane_ix}-{label}")))
                .px(px(tokens::space::SM))
                .py(px(2.))
                .text_size(px(11.))
                .text_color(if selected {
                    color::text(cx)
                } else {
                    color::text_muted(cx)
                })
                .when(selected, |d| {
                    d.bg(color::selected_bg(cx)).rounded(px(tokens::radius::SM))
                })
                .hover(|d| d.bg(color::hover_overlay(cx)))
                .cursor_pointer()
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        // Only toggle when switching modes (clicking the active
                        // segment is a no-op, like the C++ exclusive combo).
                        if this.split_panes.get(pane_ix).copied() != Some(this_mode) {
                            this.toggle_split_pane_mode(pane_ix, cx);
                        }
                    }),
                )
        };

        let header = gpui_component::h_flex()
            .flex_none()
            .h(px(28.))
            .w_full()
            .items_center()
            .justify_between()
            .px(px(tokens::space::SM))
            .bg(color::chrome_bg(cx))
            .border_b_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .gap(px(tokens::space::XXS))
                    .child(segment("Tree", ViewMode::Tree, cx))
                    .child(segment("Code", ViewMode::Rendered, cx)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("split-close-{pane_ix}")))
                    .px(px(tokens::space::XS))
                    .text_size(px(13.))
                    .text_color(color::text_muted(cx))
                    .hover(|d| d.text_color(color::text(cx)))
                    .cursor_pointer()
                    .child("✕")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            // Remove THIS pane (the C++ closes the pane's tab widget).
                            if pane_ix < this.split_panes.len() {
                                this.split_panes.remove(pane_ix);
                                cx.notify();
                            }
                        }),
                    ),
            );

        // ── Body: the same document in the pane's view mode. ──
        //
        // NOTE: a split pane must NOT re-render the live `RcxEditor` entity — gpui
        // renders an entity once per frame, and the primary pane (inside the dock
        // area) already owns that render. So a split's *Tree* view shows a
        // read-only projection of the editor's composed tree text (the C++
        // `SplitPane` views the same document; here it mirrors the composed
        // output), and the *Code* view shows the generated C/C++. Both are pure
        // read-only projections built from the controller — never the live entity.
        let body = match (editor, mode) {
            (Some(ed), ViewMode::Tree) => self.render_split_tree(ed, cx),
            (Some(ed), ViewMode::Rendered) => self.render_split_code(ed, cx),
            (None, _) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(color::text_muted(cx))
                .child("No document")
                .into_any_element(),
        };

        gpui_component::v_flex()
            .size_full()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }

    /// The read-only Tree projection for a split pane: the editor's last composed
    /// tree text (the C++ `SplitPane` tree view shows the same document). Rendered
    /// as monospaced lines so the split mirrors the primary editor's structure
    /// without re-rendering the live `RcxEditor` entity (which gpui only permits
    /// once per frame). The primary pane stays fully interactive; this is a faithful
    /// read-only mirror.
    fn render_split_tree(
        &self,
        editor: &Entity<super::editor::RcxEditor>,
        cx: &Context<Self>,
    ) -> AnyElement {
        use super::design::{color, tokens};
        let text = editor.read(cx).last_result().text.clone();
        if text.trim().is_empty() {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(color::content_bg(cx))
                .text_color(color::text_muted(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .child("// empty document")
                .into_any_element();
        }
        let line_h = px(tokens::font::EDITOR_SIZE * tokens::font::EDITOR_LINE_HEIGHT);
        let rows: Vec<AnyElement> = text
            .lines()
            .map(|line| {
                div()
                    .h(line_h)
                    .px(px(tokens::space::SM))
                    .text_color(color::text(cx))
                    .child(line.to_string())
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .id("rcx-split-tree-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(tokens::font::EDITOR_SIZE))
            .py(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// The rendered C/C++ projection for a split pane (the C++ `updateRenderedView`
    /// for a `SplitPane`): generate the source for the active editor's view root
    /// and show it as a read-only, line-numbered, monospaced text block. A
    /// self-contained mirror of the document area's code view (which lives in the
    /// out-of-ownership `tabs.rs`); kept deliberately simple (no per-token syntax
    /// colouring) since it is a secondary pane.
    fn render_split_code(
        &self,
        editor: &Entity<super::editor::RcxEditor>,
        cx: &Context<Self>,
    ) -> AnyElement {
        use super::design::{color, tokens};
        let ed = editor.read(cx);
        let ctrl = ed.controller();
        let aliases = &ctrl.document().type_aliases;
        let aliases = if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        };
        let source = crate::generator::render_cpp_tree(
            ctrl.tree(),
            ctrl.view_root_id(),
            aliases,
            /* emit_asserts */ self.generator_asserts,
        );
        if source.trim().is_empty() {
            return div()
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
        let line_h = px(tokens::font::EDITOR_SIZE * tokens::font::EDITOR_LINE_HEIGHT);
        let rows: Vec<AnyElement> = source
            .lines()
            .enumerate()
            .map(|(i, line)| {
                gpui_component::h_flex()
                    .h(line_h)
                    .items_center()
                    .child(
                        div()
                            .w(px(44.))
                            .pr(px(tokens::space::SM))
                            .text_align(gpui::TextAlign::Right)
                            .text_color(color::syntax_address(cx))
                            .child(format!("{}", i + 1)),
                    )
                    .child(div().flex_1().min_w_0().child(line.to_string()))
                    .into_any_element()
            })
            .collect();
        gpui_component::v_flex()
            .id("rcx-split-code-view")
            .size_full()
            .bg(color::content_bg(cx))
            .overflow_scroll()
            .font_family(tokens::font::mono_family())
            .text_size(px(tokens::font::EDITOR_SIZE))
            .py(px(tokens::space::SM))
            .px(px(tokens::space::SM))
            .children(rows)
            .into_any_element()
    }

    /// Feed the active document's provider into the scanner + modules docks and
    /// the active document's bookmark list into the bookmarks dock (the C++
    /// `ScannerPanel::set_provider` / `refreshModulesDock` / `refreshBookmarksDock`).
    /// Called whenever the active document or its source changes so the docks
    /// reflect reality instead of staying empty.
    fn refresh_docks_for_active(&mut self, cx: &mut Context<Self>) {
        let editor = self.document_area.read(cx).active_editor().cloned();
        let Some(editor) = editor else {
            self.scanner.update(cx, |p, _| p.set_provider(None));
            self.modules.update(cx, |p, _| p.set_provider(None));
            self.bookmarks.update(cx, |p, cx| p.set_bookmarks(&[], cx));
            return;
        };
        let (provider, bookmarks) = {
            let ed = editor.read(cx);
            let ctrl = ed.controller();
            (
                ctrl.document().provider.clone(),
                ctrl.document().tree.bookmarks.clone(),
            )
        };
        self.scanner
            .update(cx, |p, _| p.set_provider(Some(provider.clone())));
        self.modules
            .update(cx, |p, _| p.set_provider(Some(provider)));
        self.bookmarks
            .update(cx, |p, cx| p.set_bookmarks(&bookmarks, cx));
    }

    /// Push the window's focus/visibility into every open editor's controller so
    /// the adaptive refresh interval throttles on blur / pauses on minimize (the
    /// C++ `RcxController::setWindowState`; controller.cpp:5474). Called from the
    /// gpui window-activation observer.
    fn set_controllers_window_state(
        &mut self,
        focused: bool,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, _cx| {
                ed.controller_mut().set_window_state(focused, visible);
            });
        }
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
        let right_panel = self.right_dock_panel;
        let presentation = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            for opt in ViewOpt::ALL {
                mb.set_command_checked(opt.command_id(), opts.get(opt), cx);
            }
            mb.set_command_checked("view.project", project_open, cx);
            // Modules / Bookmarks share the right dock, but each ✓ is tied to its
            // OWN panel's visibility (the C++ binds each ✓ to its dock; item 9).
            // A panel is "visible" only when the right dock is open AND that panel
            // is the raised tab — so opening Modules no longer also checks
            // Bookmarks (and vice versa).
            mb.set_command_checked(
                "view.modules",
                right_open && right_panel == RightDockPanel::Modules,
                cx,
            );
            mb.set_command_checked(
                "view.bookmarks",
                right_open && right_panel == RightDockPanel::Bookmarks,
                cx,
            );
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

    /// Toggle the **right** dock (Modules + Bookmarks, tabified together). Kept as
    /// the plain open/close primitive behind the more specific
    /// [`raise_modules`](Self::raise_modules) / [`raise_bookmarks`](Self::raise_bookmarks)
    /// (which open the dock AND focus their panel); a future "View ▸ Toggle Right
    /// Dock" command can route straight here.
    #[allow(dead_code)]
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

    /// View ▸ Modules (`Ctrl+Shift+Y`) — raise the **Modules** tab of the right
    /// dock (the C++ `m_modulesDock->raise()`; main.cpp). Opens the right dock if
    /// closed and gives the Modules panel keyboard focus so it is the one the user
    /// lands on (gpui-component's `TabPanel` has no public "select tab N" API, so
    /// raising == open + focus the panel). Re-syncs the View ✓.
    fn raise_modules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_right_dock_open(true, window, cx);
        // Record which right-dock tab is now active so the View ✓ for Modules
        // (and NOT Bookmarks) lights up (item 9).
        self.right_dock_panel = RightDockPanel::Modules;
        let focus = self.modules.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// View ▸ Bookmarks (`Ctrl+Shift+B`) — raise the **Bookmarks** tab of the right
    /// dock (the C++ `m_bookmarksDock->raise()`). Same open-+-focus behaviour as
    /// [`raise_modules`](Self::raise_modules) but targeting the Bookmarks panel.
    fn raise_bookmarks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_right_dock_open(true, window, cx);
        // Record which right-dock tab is now active so the View ✓ for Bookmarks
        // (and NOT Modules) lights up (item 9).
        self.right_dock_panel = RightDockPanel::Bookmarks;
        let focus = self.bookmarks.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        self.sync_view_menu_checked(cx);
        cx.notify();
    }

    /// View ▸ Split Editor (`Ctrl+\`) — append a split pane to the active document
    /// (the C++ `splitView` → `tab->panes.append(createSplitPane(*tab))`;
    /// main.cpp:4299). The new pane views the SAME active document and starts in
    /// the **rendered C/C++** mode (so the split is immediately useful: tree on the
    /// left, generated source on the right — the canonical reclass split). Capped
    /// to keep the layout legible. No-op (notified) with no active document.
    fn split_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_area.read(cx).active_editor().is_none() {
            self.notify("Open a document before splitting the editor.", window, cx);
            return;
        }
        // The C++ has no hard cap, but more than two extra panes is unreadable in
        // a single row; cap at 2 extra (3 total incl. the primary) so the layout
        // stays usable. A capped split notifies rather than silently no-opping.
        const MAX_EXTRA_PANES: usize = 2;
        if self.split_panes.len() >= MAX_EXTRA_PANES {
            self.notify("Editor is already split to the maximum.", window, cx);
            return;
        }
        // New panes default to the rendered (code) view — the primary keeps the
        // tree, so the user gets the side-by-side tree⇄code split out of the box.
        self.split_panes.push(ViewMode::Rendered);
        cx.notify();
    }

    /// View ▸ Unsplit Editor (`Ctrl+Shift+\`) — remove the last split pane (the C++
    /// `unsplitView` → `tab->panes.takeLast()` guarded by `panes.size() > 1`;
    /// main.cpp:4305). With no extra panes the editor is already unsplit; notify
    /// rather than silently no-op.
    fn unsplit_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split_panes.pop().is_none() {
            self.notify("Editor is not split.", window, cx);
            return;
        }
        cx.notify();
    }

    /// Toggle the view mode of a split pane (its own segmented Tree/Code control;
    /// the C++ per-`SplitPane` view-mode combo). No-op if `pane_ix` is stale.
    fn toggle_split_pane_mode(&mut self, pane_ix: usize, cx: &mut Context<Self>) {
        if let Some(mode) = self.split_panes.get_mut(pane_ix) {
            *mode = mode.toggled();
            cx.notify();
        }
    }

    /// View ▸ Reset Windows — restore the canonical dock layout (the C++ "Reset
    /// Windows": discard the current placement and return every dock to its
    /// default size + open state). Restores the workspace dock open at its default
    /// width, the scanner + right docks closed at their default sizes, collapses
    /// any editor split, and re-syncs every menu ✓.
    ///
    /// This resets **placement only** — open documents, the active tab, and each
    /// panel's content are preserved (the C++ Reset Windows likewise re-docks the
    /// existing widgets, it does not reload the project). Default sizes mirror
    /// [`docks::build_default_layout`] (workspace ~280px, scanner ~320px).
    fn reset_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Collapse any editor split back to the single primary pane.
        self.split_panes.clear();

        // Restore each dock to its canonical open state + default size. The
        // `apply_layout_preset` re-opens the workspace dock; the explicit size +
        // open resets below return the scanner/right docks to their default
        // geometry even after a drag-resize or close.
        self.apply_layout_preset(LayoutPreset::Workspace, window, cx);
        let dock_area = self.dock_area.clone();
        dock_area.update(cx, |area, cx| {
            if let Some(dock) = area.left_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(280.), window, cx);
                    d.set_open(true, window, cx);
                });
            }
            if let Some(dock) = area.bottom_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(320.), window, cx);
                    d.set_open(false, window, cx);
                });
            }
            if let Some(dock) = area.right_dock().cloned() {
                dock.update(cx, |d, cx| {
                    d.set_size(px(280.), window, cx);
                    d.set_open(false, window, cx);
                });
            }
        });
        self.layout_preset = LayoutPreset::Workspace;

        self.sync_scanner_menu_checked(cx);
        self.sync_view_menu_checked(cx);
        self.notify("Windows reset to the default layout.", window, cx);
        cx.notify();
    }

    // ── View: refresh / goto / theme / presentation ──────────────────────────

    /// View ▸ Refresh (F5) — reset the changed-byte heat tracking, then force the
    /// active editor to recompose + repaint (the C++ `resetChangeTracking()` then
    /// `refresh()`; main.cpp:1418). The previous version only recomposed, so the
    /// changed-byte highlight never cleared on refresh.
    fn refresh_active_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| {
                ed.controller_mut().reset_change_tracking();
                ed.apply_document(cx);
            });
            self.rebuild_workspace(cx);
            cx.notify();
        }
    }

    /// View ▸ Go to Address… (Ctrl+G) — open the [`GotoAddressDialog`] in the
    /// dialog layer; on Go, re-resolve the formula against the active provider's
    /// callbacks and **navigate** the active editor to the resolved address (the
    /// C++ `showGotoAddressDialog` → `navigateToFormula`; main.cpp:4324). The
    /// dialog is seeded with the recent list + the active doc's pointer size, the
    /// accepted formula is pushed onto the recent list, and a failed resolve shows
    /// a themed modal warning (not a transient toast).
    fn open_goto_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use super::gotoaddress::{GotoAddressDialog, GotoEvent};
        // Pointer size from the active document (32-bit projects deref correctly).
        let ptr_size = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| ed.read(cx).controller().tree().pointer_size)
            .filter(|&p| p > 0)
            .unwrap_or(8);
        let recent = self.goto_recent.clone();
        let dialog = cx.new(|cx| GotoAddressDialog::new(recent, ptr_size, window, cx));
        let focus = dialog.read(cx).focus_handle(cx);
        self.goto_sub = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _d, ev: &GotoEvent, window, cx| match ev {
                GotoEvent::Go(formula, addr) => {
                    let formula = formula.clone();
                    let dialog_addr = *addr;
                    window.close_dialog(cx);
                    this.commit_goto(&formula, dialog_addr, window, cx);
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

    /// Resolve `formula` against the active provider's module/symbol/pointer
    /// callbacks and navigate the active editor to it (rebase its base address;
    /// the C++ `navigateToFormula`). On a clean resolve: push the recent list,
    /// rebase + refresh, confirm. On failure: themed warning. `dialog_addr` is
    /// the dialog's literal-only evaluation (used when no provider is attached).
    fn commit_goto(
        &mut self,
        formula: &str,
        dialog_addr: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        // Re-evaluate WITH the live provider callbacks so `<mod>+0x..` / `[ptr]` /
        // `ntdll!Sym` forms resolve (the dialog evaluated literals-only).
        let ptr_size = editor.read(cx).controller().tree().pointer_size.max(1);
        let resolved = {
            let ed = editor.read(cx);
            let provider = ed.controller().document().provider.clone();
            let cbs = crate::addr::AddressParserCallbacks {
                resolve_module: Some(Box::new({
                    let p = provider.clone();
                    move |name: &str| {
                        let base = p.symbol_to_address(name);
                        (base, base != 0)
                    }
                })),
                read_pointer: Some(Box::new({
                    let p = provider.clone();
                    move |addr: u64| {
                        if ptr_size >= 8 {
                            let v = p.read_u64(addr);
                            (v, true)
                        } else {
                            let v = p.read_u32(addr) as u64;
                            (v, true)
                        }
                    }
                })),
                resolve_identifier: Some(Box::new({
                    let p = provider.clone();
                    move |name: &str| {
                        let base = p.symbol_to_address(name);
                        (base, base != 0)
                    }
                })),
                ..Default::default()
            };
            let r = crate::addr::AddressParser::evaluate(formula, ptr_size, Some(&cbs));
            if r.ok {
                Some(r.value)
            } else if !formula.trim().is_empty() && dialog_addr != 0 {
                // Fall back to the dialog's literal evaluation (no provider).
                Some(dialog_addr)
            } else {
                None
            }
        };
        match resolved {
            Some(addr) => {
                // Navigate: rebase the active editor's tree to the resolved
                // address (the C++ `navigateToFormula` sets baseAddress and
                // preserves the formula for re-rebase), then recompose + repaint.
                editor.update(cx, |ed, cx| {
                    let tree = &mut ed.controller_mut().document_mut().tree;
                    tree.base_address = addr;
                    tree.base_address_formula = formula.to_string();
                    ed.apply_document(cx);
                });
                self.goto_recent = super::gotoaddress::push_recent_list(&self.goto_recent, formula);
                // Persist the recent formulas across launches (the C++
                // `gotoAddress/recent` key) via the disk store.
                super::gotoaddress::store_recent(
                    &mut *self.settings.borrow_mut(),
                    &self.goto_recent,
                );
                self.rebuild_workspace(cx);
                self.refresh_docks_for_active(cx);
                self.notify(format!("Jumped to 0x{addr:X}"), window, cx);
                cx.notify();
            }
            None => {
                let spec = super::messagebox::warn(
                    "Address Not Resolved",
                    &format!(
                        "Couldn't evaluate \"{formula}\". The expression isn't valid or its \
                         module/symbol can't be resolved without a live data source."
                    ),
                );
                super::messagebox::open_message(spec, window, cx);
            }
        }
    }

    /// Switch the active theme by display name (`view.theme.<NAME>`): find its
    /// index in the theme list and apply it, then check its menu row (clearing
    /// the others — the C++ exclusive `themeGroup`; main.cpp:1318). No-op
    /// (notified) if unknown.
    fn switch_theme_by_name(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let index = self
            .theme_manager
            .borrow()
            .themes()
            .iter()
            .position(|t| t.name == name);
        match index {
            Some(i) => {
                self.switch_theme(i, window, cx);
                self.sync_theme_menu_checked(cx);
                // Persist the selection to the disk store ("theme" key) so it
                // survives a relaunch (the manager's own store is wiped each
                // launch); restored in the ctor.
                self.settings.borrow_mut().set(settings_keys::THEME, name);
            }
            None => self.notify(format!("Unknown theme: {name}"), window, cx),
        }
    }

    /// Mark the active theme's menu row checked and clear every other theme row
    /// (the C++ exclusive theme action group). Called after a theme switch and on
    /// first paint so the Theme submenu reflects reality.
    fn sync_theme_menu_checked(&mut self, cx: &mut Context<Self>) {
        let active = self.state.theme_name().to_string();
        let names = super::commandpalette::theme_display_names();
        self.menubar.update(cx, |mb, cx| {
            for n in &names {
                mb.set_command_checked(&format!("view.theme.{n}"), *n == active, cx);
            }
        });
    }

    /// View ▸ Presentation Mode — toggle the presentation flag + its ✓ (the C++
    /// `setPresentationMode` on every editor + MCP slow-mode; main.cpp:1511). The
    /// editor surface exposes no presentation setter in this port, so the window
    /// owns the live flag + checkmark; editor-side presentation rendering + MCP
    /// slow-mode land with the editor's presentation pass.
    fn toggle_presentation(&mut self, cx: &mut Context<Self>) {
        self.presentation = !self.presentation;
        let on = self.presentation;
        self.menubar.update(cx, |mb, cx| {
            mb.set_command_checked("view.presentation", on, cx);
        });
        // Slow the read cadence while presenting (the C++ presentation mode backs
        // off the refresh/MCP tick so the demo doesn't churn): in presentation the
        // controllers use a calm refresh interval; restoring leaves them at the
        // persisted interval. Mirror the blur-throttle path the window already owns.
        self.set_controllers_window_state(!on, true, cx);
        // Engage the editor spotlight on every open pane (the C++
        // `setPresentationMode(on)` on each editor; main.cpp:1511). This is the
        // central effect — the focus-glow + non-focused-row dimming — which was
        // dead because no pane's `set_presentation_mode` was ever called. Split
        // panes view the SAME active editor entity, so iterating the tab editors
        // covers every visible pane.
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |ed, cx| ed.set_presentation_mode(on, cx));
        }
        // The render reads `self.presentation` to fade the chrome (titlebar +
        // status bar); request a repaint so the fade applies immediately.
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
            // The provider changed — re-feed the docks + rebuild the Data Source
            // menu rows so the new (or cleared) source is reflected.
            self.refresh_docks_for_active(cx);
            self.rebuild_menus(cx);
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
            // Undo/redo changes the document's clean state — propagate the dirty
            // dot into the tab.
            self.sync_dirty_state(cx);
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

    fn on_doc_area_event(&mut self, ev: DocAreaEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            DocAreaEvent::Activated(id) => {
                // Mirror the activation into AppState if the id is known; the area
                // and state allocate ids independently but in lockstep order.
                self.activate_doc(id);
                // Re-feed the docks from the newly-active document's provider +
                // bookmarks and rebuild the dynamic menus (Data Source rows).
                self.refresh_docks_for_active(cx);
                self.rebuild_menus(cx);
            }
            DocAreaEvent::NewDocumentRequested => {
                // The center opened a fresh tab (`project_new`); mirror it.
                self.state.open_document("Untitled");
                self.rebuild_workspace(cx);
                // The tab set grew — re-observe so the new editor's selections
                // refresh the status bar.
                self.observe_editors(window, cx);
            }
            DocAreaEvent::Closed(id) => {
                self.state.close_document(id);
                self.rebuild_workspace(cx);
                self.observe_editors(window, cx);
            }
            DocAreaEvent::ViewModeChanged(id, mode) => {
                self.state.set_view_mode(id, mode);
            }
            DocAreaEvent::CodeOptionsChanged {
                format_idx,
                scope_idx,
            } => {
                // Persist the selector indices (the C++ `fmtCombo`/`scopeCombo`
                // `currentIndexChanged` → `setValue("codeFormat"/"codeScope")`;
                // main.cpp:2460/2469).
                let mut s = self.settings.borrow_mut();
                s.set(settings_keys::CODE_FORMAT, &format_idx.to_string());
                s.set(settings_keys::CODE_SCOPE, &scope_idx.to_string());
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
    fn observe_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editors: Vec<Entity<super::editor::RcxEditor>> = self
            .document_area
            .read(cx)
            .tabs()
            .iter()
            .map(|t| t.editor.clone())
            .collect();
        let mut subs = Vec::with_capacity(editors.len() * 2);
        for editor in editors {
            // A row-selection change re-renders the window (refreshes the status
            // bar). The editors `cx.notify()` themselves but don't emit up.
            subs.push(cx.observe(&editor, |_this, _editor, cx| cx.notify()));
            // Editor → host events. Ctrl+Click on a navigable header token emits
            // `OpenTypeInNewTab { ref_id }`; route it to the createTab+setViewRootId
            // flow (item 5). `Status` messages are harmless if unconsumed.
            subs.push(cx.subscribe_in(
                &editor,
                window,
                |this, _editor, ev: &super::editor::RcxEditorEvent, window, cx| match ev {
                    super::editor::RcxEditorEvent::OpenTypeInNewTab { ref_id } => {
                        this.open_type_in_new_tab(*ref_id, window, cx);
                    }
                    super::editor::RcxEditorEvent::Status { message } => {
                        this.notify(message.clone(), window, cx);
                    }
                    // An in-editor View-option toggle (offset-margin double-click /
                    // right-click Relative/Absolute): the editor already applied it
                    // locally; mirror it to the window (persist + ✓ + push to every
                    // pane) so it behaves like the menu toggle (the C++
                    // `relativeOffsetsChanged`).
                    super::editor::RcxEditorEvent::ViewOptionToggled { option, value } => {
                        let opt = match option {
                            super::editor::EditorViewOption::RelativeOffsets => {
                                ViewOpt::RelativeOffsets
                            }
                        };
                        this.set_view_option_value(opt, *value, cx);
                    }
                },
            ));
        }
        self.editor_observers = subs;
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

    /// Ctrl+Click on a navigable header token → open the referenced struct
    /// (`ref_id`) in a NEW tab (the C++ Ctrl+Click → `openTypeInNewTabRequested`
    /// → `createTab(doc)` + `setViewRootId`; main.cpp:3092). The Rust port's tabs
    /// each own their document, so "the same document in a new tab" is a deep copy
    /// of the active document's tree (node ids — and thus `ref_id` — preserved by
    /// [`NodeTree::clone`]) sharing the same provider `Arc`; the new tab's view
    /// root is set to `ref_id`. Without this the editor's
    /// [`RcxEditorEvent::OpenTypeInNewTab`] had no subscriber and the new tab was
    /// never created (item 5).
    fn open_type_in_new_tab(&mut self, ref_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.document_area.read(cx).active_editor().cloned() else {
            return;
        };
        // Deep-copy the active document (tree clone keeps ids/ref_ids; the provider
        // Arc is shared so the new tab reads the same source).
        let (tree, provider, file_path) = {
            let ed = active.read(cx);
            let doc = ed.controller().document();
            (
                doc.tree.clone(),
                doc.provider.clone(),
                doc.file_path.clone(),
            )
        };
        // Only open if the referenced struct actually exists in the tree (the C++
        // guards on a resolvable target before emitting).
        if tree.index_of_id(ref_id) < 0 {
            self.notify("No type to open in a new tab.", window, cx);
            return;
        }
        let mut doc = crate::controller::RcxDocument::new();
        doc.tree = tree;
        doc.provider = provider;
        doc.file_path = file_path;
        let title = root_name_for_title(&doc.tree, ref_id);
        let title = if title.is_empty() {
            "Untitled".to_string()
        } else {
            title
        };
        let source = Self::source_for_doc(&doc);
        // Append the new tab + push the cloned document into its editor, then set
        // the view root to the referenced struct (the C++ createTab + setViewRootId).
        let mut new_editor: Option<Entity<super::editor::RcxEditor>> = None;
        self.document_area.update(cx, |area, cx| {
            area.push_document(title.clone(), window, cx);
            if let Some(editor) = area.active_editor().cloned() {
                editor.update(cx, |ed, cx| {
                    ed.set_document(doc, cx);
                    ed.controller_mut().set_view_root_id(ref_id);
                    ed.apply_document(cx);
                });
                new_editor = Some(editor);
            }
        });
        if let Some(id) = self.active_doc_id(cx) {
            self.document_area.update(cx, |area, cx| {
                area.set_source(id, source.clone(), cx);
            });
        }
        // Mirror into AppState (title + source + view root) so the new tab is a
        // first-class document.
        let state_id = self.state.open_document(title);
        if let Some(t) = self.state.tab_mut(state_id) {
            t.view_root = Some(ref_id);
            t.source = source;
        }
        // Realign the fresh editor to the persisted view options + font, refresh
        // the docks + workspace, and re-observe so its events route.
        if let Some(editor) = new_editor {
            self.apply_view_opts_to_editor(&editor, cx);
        }
        self.rebuild_workspace(cx);
        self.refresh_docks_for_active(cx);
        self.observe_editors(window, cx);
        cx.notify();
    }

    /// The editor entity owning document `doc`, if it is open in a tab.
    fn editor_for_doc(&self, doc: DocId, cx: &App) -> Option<Entity<super::editor::RcxEditor>> {
        self.document_area
            .read(cx)
            .tabs()
            .iter()
            .find(|t| t.id == doc)
            .map(|t| t.editor.clone())
    }

    /// Resolve a workspace type-row mutation (the C++ workspace `QMenu`:
    /// Rename / Duplicate / Delete / Add Member) against the owning document's
    /// live controller. The targeted node is addressed by `(doc, node_id)`; map
    /// the id to its tree index and drive the matching controller command, then
    /// recompose the editor, rebuild the workspace tree, and sync dirty state.
    fn on_workspace_type_action(
        &mut self,
        action: WorkspaceTypeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            WorkspaceTypeAction::Rename {
                doc,
                node_id,
                current,
            } => {
                // The C++ `renameType` opens `QInputDialog::getText`; collect the
                // new name in a free-text prompt, then apply on accept.
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                let this = cx.entity().downgrade();
                self.open_text_prompt(
                    "Rename Type",
                    "New name",
                    &current,
                    window,
                    cx,
                    move |name, window, app| {
                        let name = name.trim().to_string();
                        if name.is_empty() {
                            return;
                        }
                        let _ = this.update(app, |me, cx| {
                            editor.update(cx, |ed, cx| {
                                let idx = ed.controller().tree().index_of_id(node_id);
                                if idx >= 0 {
                                    ed.controller_mut().rename_node(idx as usize, &name);
                                    ed.apply_document(cx);
                                }
                            });
                            me.rebuild_workspace(cx);
                            me.sync_dirty_state(cx);
                            me.notify(format!("Renamed to {name}"), window, cx);
                            cx.notify();
                        });
                    },
                );
            }
            WorkspaceTypeAction::Duplicate { doc, node_id } => {
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                editor.update(cx, |ed, cx| {
                    let idx = ed.controller().tree().index_of_id(node_id);
                    if idx >= 0 {
                        ed.controller_mut().duplicate_node(idx as usize);
                        ed.apply_document(cx);
                    }
                });
                self.rebuild_workspace(cx);
                self.sync_dirty_state(cx);
                self.notify("Duplicated type", window, cx);
                cx.notify();
            }
            WorkspaceTypeAction::Delete { doc, node_id } => {
                // Confirm before a destructive delete (the C++ asks before
                // deleting a top-level type).
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                let name = editor.read(cx).controller().tree().index_of_id(node_id);
                if name < 0 {
                    return;
                }
                let spec = super::messagebox::confirm(
                    "Delete Type",
                    "Delete this type and its members? This can be undone.",
                    "Delete",
                    true,
                );
                let this = cx.entity().downgrade();
                super::messagebox::open_confirm(
                    spec,
                    move |window, app| {
                        let _ = this.update(app, |me, cx| {
                            editor.update(cx, |ed, cx| {
                                let idx = ed.controller().tree().index_of_id(node_id);
                                if idx < 0 {
                                    return;
                                }
                                let node = ed.controller().tree().nodes[idx as usize].clone();
                                // A top-level struct uses `deleteRootStruct` (it
                                // also rebinds refs + the view root); a member field
                                // uses `removeNode`.
                                if node.parent_id == 0 && node.kind == crate::core::NodeKind::Struct
                                {
                                    ed.controller_mut().delete_root_struct(node_id);
                                } else {
                                    ed.controller_mut().remove_node(idx as usize);
                                }
                                ed.apply_document(cx);
                            });
                            me.rebuild_workspace(cx);
                            me.sync_dirty_state(cx);
                            me.notify("Deleted type", window, cx);
                            cx.notify();
                        });
                    },
                    window,
                    cx,
                );
            }
            WorkspaceTypeAction::AddMember { doc, node_id } => {
                let Some(editor) = self.editor_for_doc(doc, cx) else {
                    return;
                };
                editor.update(cx, |ed, cx| {
                    let idx = ed.controller().tree().index_of_id(node_id);
                    if idx < 0 {
                        return;
                    }
                    let node = ed.controller().tree().nodes[idx as usize].clone();
                    // Append a new member at the end of the struct. For a struct row
                    // the members are its children; for a field row, add a sibling
                    // into the field's parent.
                    let parent_id = if node.kind == crate::core::NodeKind::Struct {
                        node.id
                    } else {
                        node.parent_id
                    };
                    ed.controller_mut().insert_node(
                        parent_id,
                        -1,
                        crate::core::NodeKind::Hex64,
                        "new_member",
                    );
                    ed.apply_document(cx);
                });
                self.rebuild_workspace(cx);
                self.sync_dirty_state(cx);
                self.notify("Added member", window, cx);
                cx.notify();
            }
        }
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

    /// The recent-files entries for the start page, built from the session
    /// recent-files list (the C++ `recentFiles` QSettings, surfaced by the start
    /// page). Most-recent-first; each entry's age is the whole-day delta between
    /// now and the file's last-modified time (the C++ `buildGroups` compared
    /// `QFileInfo::lastModified()` against `now`; startpage.h:236), so the start
    /// page buckets recent files into Today / Yesterday / This Week / This Month /
    /// Older. On a metadata/time error the age falls back to 0 (today's bucket).
    fn recent_entries(&self) -> Vec<RecentEntry> {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Sample the clock once so every entry buckets against the same "now".
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Skip entries whose file no longer exists (the C++ start-page filters
        // the same way the Recent Files menu does; main.cpp:8789).
        let mut entries: Vec<RecentEntry> = self
            .existing_recent_files()
            .into_iter()
            .map(|(_, p)| {
                // Whole-day age from the file's mtime (UNIX seconds). Any
                // metadata/time failure → age 0 (today), matching the C++ "no
                // timestamp ⇒ treat as recent" fallthrough.
                let age_days = std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| super::startpage::age_days_from_secs(now_secs, d.as_secs()))
                    .unwrap_or(0);
                RecentEntry {
                    path: p.to_string_lossy().into_owned(),
                    file_name: p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("(file)")
                        .to_string(),
                    dir_path: p
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    age_days,
                    is_example: false,
                }
            })
            .collect();
        // Append the bundled examples (the C++ `loadEntries` always lists the
        // examples regardless of the recent list; startpage.h). These give the
        // start page a one-click "Continue" demo to open even on a fresh install
        // with no recent files — the audited "start-page Continue demo absent"
        // gap. Each example's `path` is its `file.example.<name>` key, routed
        // through `open_example` (not `open_project`) by `on_start_page_event`.
        entries.extend(super::startpage::example_entries());
        entries
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
            // File-menu twin runs.
            //
            // CRITICAL: do NOT dismiss the splash here for the async file-picker
            // cards. `prompt_open`/`prompt_import` spawn a native dialog that
            // resolves *later*; dismissing eagerly would tear the welcome page
            // down to the blank "Untitled" doc the instant the card is clicked,
            // so a cancelled (or, headless, un-openable) dialog would strand the
            // user on an empty document instead of returning to the splash. Only
            // the SUCCESS paths dismiss: `open_project` (window.rs ~1597) and
            // `load_doc_into_active` (~953) both call `dismiss_start_page` once a
            // project actually loads. `NewClass` is synchronous and unconditional,
            // so it still dismisses eagerly (inside `new_document`).
            StartPageEvent::Card(card) => match card {
                StartCard::NewClass => self.new_document(RootKind::Class, window, cx),
                StartCard::OpenProject => self.prompt_open(window, cx),
                StartCard::ImportSource => self.prompt_import(ImportKind::Source, window, cx),
                StartCard::ImportXml => self.prompt_import(ImportKind::Xml, window, cx),
                StartCard::ImportPdb => self.prompt_import(ImportKind::Pdb, window, cx),
            },
            StartPageEvent::FileSelected(path) => {
                // A bundled-example row carries a `file.example.<name>` key (not a
                // real on-disk path); route it through `open_example`
                // (materialize-then-open) — the C++ Examples bucket opens the
                // example, not a non-existent path. A real recent file → the C++
                // `project_open(path)`. Either way the splash dismisses.
                self.dismiss_start_page(cx);
                if let Some(name) = path.strip_prefix("file.example.") {
                    self.open_example(name, window, cx);
                } else {
                    self.open_project(std::path::Path::new(&path), None, window, cx);
                }
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::controller::RcxDocument;

        // Build the document from disk. ReClass-XML imports go through the XML
        // importer (when compiled in); `.rcx`/`.json`/anything else is the native
        // forgiving JSON load. On any failure we leave the current doc untouched.
        //
        // The importer is chosen by the file's leading BYTES (the C++ 64-byte
        // signature probe; main.cpp:6123-6131), not by extension: an `.rcx`
        // holding XML imports as XML, and an `.xml` holding JSON loads natively.
        let mut doc = RcxDocument::new();
        let is_xml = Self::path_is_reclass_xml(path);

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
        // When no explicit --data path is given the controller AUTO-ATTACHES the
        // active saved File source (resolved relative to the .rcx dir in `load`)
        // inside `set_document → RcxController::new → ingestPendingSavedSources`,
        // so the headline "all-0x0" case (a .rcx with a savedSources File entry)
        // now populates with real values. We therefore read the resulting source
        // BACK from the controller AFTER `set_document`, not from `doc` before it
        // (the bug: `doc.data_path` is still None until the controller ingests).
        if let Some(dp) = data_path {
            doc.load_data_file(dp);
        }

        // REPLACE-ALL into a fresh tab (the C++ `project_open` does NOT rebind the
        // active tab — it closes every existing doc dock then creates a brand-new
        // tab bound to the loaded document: `{ ClosingGuard guard(m_closingAll);
        // closeAllDocDocks(); dock = createTab(doc); }`; main.cpp:6147-6150 /
        // 6190-6194). `replace_all_with_fresh` drops the prior tabs (signalling each
        // `Closed` so the per-tab state unwinds) and returns the fresh id + editor;
        // the loaded document is then driven into THAT editor.
        let title = Self::title_for_path(path);
        let (fresh_id, editor) = self.document_area.update(cx, |area, cx| {
            area.replace_all_with_fresh(title.clone(), window, cx)
        });
        // Mirror the replace-all into AppState SYNCHRONOUSLY so its tab list +
        // monotonic id counter stay in lockstep with the area. The area's `Closed`
        // events are delivered later (deferred), and — unlike a `+`-tab open —
        // `replace_all_with_fresh` emits NO `NewDocumentRequested`, so the fresh
        // doc must be registered in state here, not via the event. Both id
        // allocators were equal before this call, so closing every state doc then
        // opening one fresh yields the SAME id the area just allocated; the later
        // `Closed(old_id)` events then no-op on the already-removed docs.
        let old_ids: Vec<DocId> = self.state.tabs().iter().map(|t| t.id).collect();
        for id in old_ids {
            self.state.close_document(id);
        }
        let state_fresh_id = self.state.open_document(title.clone());
        debug_assert_eq!(
            state_fresh_id, fresh_id,
            "AppState + DocumentArea id allocators must stay in lockstep"
        );
        editor.update(cx, |ed, cx| ed.set_document(doc, cx));
        // Realign the fresh editor to the window's (persisted) view options — a new
        // doc otherwise inherits controller/view defaults that disagree.
        self.apply_view_opts_to_editor(&editor, cx);
        // Derive the source icon from the controller's NOW-attached provider
        // (post-ingest), so a .rcx's saved File source shows the File icon.
        let source = Self::source_for_controller(editor.read(cx).controller());

        // Sync the tab title (file stem ≈ the C++ `rootName`) + source icon into
        // both the document area and the window state, keyed on the FRESH id.
        self.document_area.update(cx, |area, cx| {
            area.set_title(fresh_id, title.clone(), cx);
            area.set_source(fresh_id, source.clone(), cx);
        });
        self.state.set_title(fresh_id, title);
        self.state.set_source(fresh_id, source);

        // Refresh the workspace tree from the freshly loaded document + dismiss
        // the start page so the user lands on the document.
        self.rebuild_workspace(cx);
        // Force the workspace dock open (the C++ `placeSidebarDock(m_workspaceDock,
        // LeftDockWidgetArea); m_workspaceDock->show()` runs unconditionally after a
        // successful open; main.cpp:6152-6155 / 6196-6199). Done here — on the
        // open_project path only, not on every `set_document` — so an open always
        // reveals the project tree while plain in-editor reloads leave the layout
        // alone.
        self.apply_layout_preset(LayoutPreset::Workspace, window, cx);
        self.dismiss_start_page(cx);
        // Feed the freshly-attached provider into the scanner/modules docks and
        // the document's bookmarks into the bookmarks dock.
        self.refresh_docks_for_active(cx);
        // Record this project as a recent file (the C++ `addRecentFile`) so the
        // File ▸ Recent Files submenu + start page surface it on next open, and
        // propagate the (clean) dirty state into the tab.
        self.record_recent_file(path, cx);
        self.sync_dirty_state(cx);
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

    /// The UI [`DataSource`] for a controller AFTER it has ingested its saved
    /// sources — preferring the active saved-source entry (its display name +
    /// kind), falling back to the document's attached `data_path`. The C++ tab
    /// source icon reads the active source post-attach (`refreshDocTabSourceIcon`
    /// runs after `ingestPendingSavedSources`); reading the doc BEFORE the
    /// controller ingested left the icon showing "no source" even when values
    /// loaded (the bug). For a File source we keep the on-disk path so the icon +
    /// tooltip match the attached binary.
    fn source_for_controller(ctrl: &crate::controller::RcxController) -> super::state::DataSource {
        use super::state::{DataSource, SourceKind};
        let idx = ctrl.active_source_index();
        if idx >= 0 {
            if let Some(entry) = ctrl.saved_sources().get(idx as usize) {
                if entry.kind == "File" {
                    let path = if entry.file_path.is_empty() {
                        entry.display_name.clone()
                    } else {
                        entry.file_path.clone()
                    };
                    return DataSource::new(SourceKind::File, path);
                }
            }
        }
        Self::source_for_doc(ctrl.document())
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

    /// Probe a file for the ReClass-XML byte signature (the C++ `project_open`
    /// sniff; main.cpp:6123-6131). Opens the file, reads the first 64 bytes
    /// (matching `probe.read(64)`), and delegates to [`sniff_is_reclass_xml`]. On
    /// any open/read failure returns `false` — the C++ leaves `isXml=false` when
    /// the probe `QFile` fails to open, falling through to the native JSON load.
    ///
    /// An associated fn (no `&self`) so the byte-sniff is testable without a window.
    fn path_is_reclass_xml(path: &std::path::Path) -> bool {
        use std::io::Read as _;
        let Ok(mut f) = std::fs::File::open(path) else {
            return false;
        };
        let mut head = [0u8; 64];
        match f.read(&mut head) {
            Ok(n) => sniff_is_reclass_xml(&head[..n]),
            Err(_) => false,
        }
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

    /// Open a modal free-text input prompt (the C++ `QInputDialog::getText`):
    /// title + field label + a seeded value. On accept (Enter / OK) the trimmed
    /// text is delivered to `on_accept`; Cancel / Esc dismisses with no callback.
    /// Used by the bookmark-name and type-rename flows (replacing the previous
    /// auto-name confirm boxes).
    fn open_text_prompt<F>(
        &mut self,
        title: &str,
        label: &str,
        default: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_accept: F,
    ) where
        F: Fn(String, &mut Window, &mut App) + 'static,
    {
        let title = title.to_string();
        let label = label.to_string();
        let default = default.to_string();
        let prompt = cx.new(|cx| {
            TextPromptDialog::new(title.clone(), label.clone(), default.clone(), window, cx)
        });
        let on_accept = Rc::new(on_accept);
        self.goto_sub = Some(cx.subscribe_in(
            &prompt,
            window,
            move |_this, _p, ev: &TextPromptEvent, window, cx| match ev {
                TextPromptEvent::Accept(text) => {
                    let text = text.clone();
                    window.close_dialog(cx);
                    (on_accept)(text, window, cx);
                }
                TextPromptEvent::Cancel => window.close_dialog(cx),
            },
        ));
        let focus = prompt.read(cx).focus_handle(cx);
        let prompt_for_modal = prompt.clone();
        window.open_dialog(cx, move |d, _window, _cx| {
            d.w(px(420.))
                .margin_top(px(140.))
                .close_button(false)
                .child(prompt_for_modal.clone())
        });
        window.focus(&focus, cx);
        cx.notify();
    }
}

/// The three-way unsaved-changes guard dialog (the C++ `closeEvent`'s
/// `ThemedMessageBox::unsavedChanges`): Cancel / Discard / Save changes, with the
/// dirty document names listed and **Save changes** as the default (Enter)
/// button. Built from [`messagebox::unsaved_changes`] (button order + labels +
/// variants + default) so the layout matches the rest of the themed message
/// boxes; emits a [`messagebox::UnsavedChoice`] mapped via
/// [`messagebox::unsaved_choice_for`]. Replaces the old 2-button confirm that
/// quit/closed WITHOUT ever offering Save (item 1).
struct RcxUnsavedDialog {
    spec: super::messagebox::MessageSpec,
    focus_handle: FocusHandle,
}

impl RcxUnsavedDialog {
    fn new(title: &str, text: &str, dirty_names: Vec<String>, cx: &mut Context<Self>) -> Self {
        Self {
            spec: super::messagebox::unsaved_changes(title, text, dirty_names),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Emit the choice for `button_index` (0=Cancel, 1=Discard, 2=Save), per the
    /// `[Cancel, Discard, Save changes]` order [`messagebox::unsaved_changes`]
    /// builds.
    fn choose(&mut self, button_index: usize, cx: &mut Context<Self>) {
        cx.emit(super::messagebox::unsaved_choice_for(button_index));
    }
}

impl Focusable for RcxUnsavedDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<super::messagebox::UnsavedChoice> for RcxUnsavedDialog {}

impl Render for RcxUnsavedDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::dialogs::modal;
        use super::messagebox::{ButtonVariant, DetailLayout};
        use gpui_component::button::{Button, ButtonVariants as _};

        let card_w = modal::clamp_width(super::messagebox::MSG_MAX_WIDTH, window);
        let detail_layout = super::messagebox::format_detail(&self.spec.detail);

        // Body: the count sentence, then the dirty-name list (label for ≤5,
        // scrollable list for >5 — the C++ detail threshold).
        let mut body = modal::body(cx).child(
            div()
                .text_size(px(super::design::tokens::font::UI_MD))
                .text_color(super::design::color::text(cx))
                .child(self.spec.text.clone()),
        );
        body = match detail_layout {
            DetailLayout::None => body,
            DetailLayout::Label(s) => body.child(
                div()
                    .text_size(px(super::design::tokens::font::UI_SM))
                    .text_color(super::design::color::text_muted(cx))
                    .child(s),
            ),
            DetailLayout::List(items) => body.child(
                gpui_component::v_flex()
                    .id("rcx-unsaved-detail")
                    .max_h(px(140.))
                    .overflow_y_scroll()
                    .gap(px(super::design::tokens::space::XXS))
                    .children(items.into_iter().map(|item| {
                        div()
                            .text_size(px(super::design::tokens::font::UI_SM))
                            .text_color(super::design::color::text_muted(cx))
                            .child(item)
                    })),
            ),
        };

        // Footer: the buttons in the spec's left→right order [Cancel, Discard,
        // Save changes], each carrying its variant.
        let mut footer = modal::footer(cx);
        for (i, b) in self.spec.buttons.iter().enumerate() {
            let label = b.label.clone();
            let btn = Button::new(("unsaved-btn", i))
                .label(label)
                .map(|btn| match b.variant {
                    ButtonVariant::Primary => btn.primary(),
                    ButtonVariant::Secondary => btn,
                    ButtonVariant::Destructive => btn.danger(),
                })
                .on_click(cx.listener(move |this, _e, _w, cx| this.choose(i, cx)));
            footer = footer.child(btn);
        }

        modal::card(cx)
            .id("rcx-unsaved-dialog")
            .track_focus(&self.focus_handle)
            .key_context("RcxUnsaved")
            // Enter → the default (Save changes = last button); Esc → Cancel
            // (index 0) — the C++ default-button / reject wiring.
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        let last = this.spec.buttons.len().saturating_sub(1);
                        this.choose(last, cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.choose(0, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .child(modal::header(self.spec.title.clone(), cx))
            .child(body)
            .child(footer)
    }
}

/// The outcome of the [`TextPromptDialog`] (the C++ `QInputDialog` accept/reject).
#[derive(Clone, Debug)]
enum TextPromptEvent {
    /// OK / Enter on a non-empty value — the trimmed text.
    Accept(String),
    /// Cancel / Esc.
    Cancel,
}

/// A minimal modal free-text input dialog — the port's `QInputDialog::getText`.
/// A single input seeded with a default (all-selected), an OK button (disabled
/// while the trimmed text is empty), and Cancel. Enter confirms; Esc cancels.
struct TextPromptDialog {
    title: String,
    label: String,
    input: Entity<gpui_component::input::InputState>,
    focus_handle: FocusHandle,
    _sub: Subscription,
}

impl TextPromptDialog {
    fn new(
        title: String,
        label: String,
        default: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gpui_component::input::{InputEvent, InputState};
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(default.clone(), window, cx);
            s
        });
        // Re-render on change so the OK enabled-state tracks the field.
        let sub = cx.subscribe_in(&input, window, |_this, _i, ev: &InputEvent, _w, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        TextPromptDialog {
            title,
            label,
            input,
            focus_handle: cx.focus_handle(),
            _sub: sub,
        }
    }

    fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let text = self.value(cx).trim().to_string();
        if !text.is_empty() {
            cx.emit(TextPromptEvent::Accept(text));
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextPromptEvent::Cancel);
    }
}

impl Focusable for TextPromptDialog {
    /// Delegate the dialog focus to the INPUT so `window.focus(focus_handle)`
    /// lands keystrokes in the field (the command-palette pattern) — without this
    /// the dialog card holds focus and typing does nothing (the same dead-input
    /// failure mode the cross-cutting note flags for inline edits).
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl EventEmitter<TextPromptEvent> for TextPromptDialog {}

impl Render for TextPromptDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::input::Input;
        use gpui_component::Disableable as _;

        let can_ok = !self.value(cx).trim().is_empty();
        let card_w = modal::clamp_width(420., window);

        let body = modal::body(cx)
            .child(modal::field_label(self.label.clone(), cx))
            .child(Input::new(&self.input).w_full());

        let footer = modal::footer(cx)
            .child(
                Button::new("prompt-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("prompt-ok")
                    .primary()
                    .label("OK")
                    .when(!can_ok, |b| b.disabled(true))
                    .on_click(cx.listener(|this, _e, _w, cx| this.confirm(cx))),
            );

        modal::card(cx)
            .id("rcx-text-prompt")
            .track_focus(&self.focus_handle)
            .key_context("RcxTextPrompt")
            // Capture-phase Enter/Esc so the dialog confirms/cancels even while the
            // input owns focus (the C++ dialog's default-button / reject wiring).
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                match ev.keystroke.key.as_str() {
                    "enter" => {
                        this.confirm(cx);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        this.cancel(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .w(card_w)
            .child(
                modal::header(self.title.clone(), cx).child(modal::close_button(
                    "prompt-close",
                    cx.listener(|this, _e, _w, cx| this.cancel(cx)),
                    cx,
                )),
            )
            .child(body)
            .child(footer)
    }
}

/// The outcome of the [`TypeAliasesDialog`].
#[derive(Clone, Debug)]
enum TypeAliasesEvent {
    /// OK — commit the edited alias map.
    Accept,
    /// Cancel / Esc.
    Cancel,
}

/// The Tools ▸ Type Aliases editor (the C++ `showTypeAliasesDialog`): a row per
/// aliasable [`NodeKind`] (the primitives + pointers + strings; the C++ skips
/// Vec/Mat/Struct/Array), each with the canonical type name on the left and an
/// editable alias field on the right. Two preset buttons fill the column with the
/// stdint (C99) or Windows (basetsd.h) names; Clear empties them.
struct TypeAliasesDialog {
    /// (kind, canonical-name, alias-input) per aliasable kind, in `K_KIND_META`
    /// order.
    rows: Vec<(
        crate::core::NodeKind,
        &'static str,
        Entity<gpui_component::input::InputState>,
    )>,
    focus_handle: FocusHandle,
}

impl TypeAliasesDialog {
    fn aliasable(kind: crate::core::NodeKind) -> bool {
        use crate::core::NodeKind::*;
        !matches!(kind, Vec2 | Vec3 | Vec4 | Mat4x4 | Struct | Array)
    }

    /// The Windows (basetsd.h) preset alias for a kind, if any (the C++
    /// `kWindowsPreset`).
    fn windows_alias(kind: crate::core::NodeKind) -> Option<&'static str> {
        use crate::core::NodeKind::*;
        Some(match kind {
            Int8 => "CHAR",
            Int16 => "SHORT",
            Int32 => "LONG",
            Int64 => "LONGLONG",
            UInt8 => "UCHAR",
            UInt16 => "USHORT",
            UInt32 => "ULONG",
            UInt64 => "ULONGLONG",
            Float => "FLOAT",
            Double => "DOUBLE",
            Bool => "BOOLEAN",
            Pointer32 => "ULONG",
            Pointer64 => "ULONG_PTR",
            _ => return None,
        })
    }

    fn new(
        current: std::collections::HashMap<crate::core::NodeKind, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        use gpui_component::input::InputState;
        let mut rows = Vec::new();
        for meta in crate::core::K_KIND_META.iter() {
            if !Self::aliasable(meta.kind) {
                continue;
            }
            let seed = current.get(&meta.kind).cloned().unwrap_or_default();
            let input = cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(meta.type_name);
                if !seed.is_empty() {
                    s.set_value(seed, window, cx);
                }
                s
            });
            rows.push((meta.kind, meta.type_name, input));
        }
        TypeAliasesDialog {
            rows,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Collect the edited alias map (empty fields are dropped — no alias).
    fn collect(&self, cx: &App) -> std::collections::HashMap<crate::core::NodeKind, String> {
        let mut map = std::collections::HashMap::new();
        for (kind, _name, input) in &self.rows {
            let v = input.read(cx).value().to_string();
            let v = v.trim().to_string();
            if !v.is_empty() {
                map.insert(*kind, v);
            }
        }
        map
    }

    /// Fill every field with the stdint (C99) preset — the canonical type name
    /// per kind (the C++ `kStdintPreset`).
    fn apply_stdint(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (_kind, name, input) in &self.rows {
            let name = name.to_string();
            input.update(cx, |s, cx| s.set_value(name, window, cx));
        }
        cx.notify();
    }

    /// Fill the Windows-mapped fields with their basetsd.h names; clear the rest.
    fn apply_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (kind, _name, input) in &self.rows {
            let v = Self::windows_alias(*kind).unwrap_or("").to_string();
            input.update(cx, |s, cx| s.set_value(v, window, cx));
        }
        cx.notify();
    }

    /// Empty every field (no aliases).
    fn apply_clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (_kind, _name, input) in &self.rows {
            input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(TypeAliasesEvent::Accept);
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(TypeAliasesEvent::Cancel);
    }
}

impl Focusable for TypeAliasesDialog {
    /// Focus the first alias input so the dialog opens ready to type (the
    /// command-palette delegate pattern); fall back to the card handle if there
    /// are no rows.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.rows
            .first()
            .map(|(_, _, input)| input.read(cx).focus_handle(cx))
            .unwrap_or_else(|| self.focus_handle.clone())
    }
}

impl EventEmitter<TypeAliasesEvent> for TypeAliasesDialog {}

impl Render for TypeAliasesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::design::tokens;
        use super::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};
        use gpui_component::input::Input;
        use gpui_component::Sizable as _;

        let card_w = modal::clamp_width(460., window);
        let card_max_h = modal::clamp_height(560., 100., window);
        let mono = SharedString::from(tokens::font::mono_family());

        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|(_kind, name, input)| {
                gpui_component::h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(tokens::space::MD))
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .font_family(mono.clone())
                            .text_size(px(tokens::font::UI_SM))
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(*name)),
                    )
                    .child(Input::new(input).w_full().font_family(mono.clone()))
                    .into_any_element()
            })
            .collect();

        let presets = gpui_component::h_flex()
            .gap(px(tokens::space::SM))
            .child(
                Button::new("alias-stdint")
                    .small()
                    .label("stdint (C99)")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_stdint(window, cx))),
            )
            .child(
                Button::new("alias-windows")
                    .small()
                    .label("Windows (basetsd.h)")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_windows(window, cx))),
            )
            .child(
                Button::new("alias-clear")
                    .small()
                    .label("Clear")
                    .on_click(cx.listener(|this, _e, window, cx| this.apply_clear(window, cx))),
            );

        let body = modal::body(cx).child(presets).child(
            gpui_component::v_flex()
                .id("rcx-alias-rows")
                .w_full()
                .max_h(px(360.))
                .overflow_y_scroll()
                .gap(px(tokens::space::XS))
                .children(rows),
        );

        let footer = modal::footer(cx)
            .child(
                Button::new("alias-cancel")
                    .label("Cancel")
                    .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
            )
            .child(
                Button::new("alias-ok")
                    .primary()
                    .label("OK")
                    .on_click(cx.listener(|this, _e, _w, cx| this.confirm(cx))),
            );

        modal::card(cx)
            .id("rcx-type-aliases")
            .track_focus(&self.focus_handle)
            .key_context("RcxTypeAliases")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    this.cancel(cx);
                    cx.stop_propagation();
                }
            }))
            .w(card_w)
            .max_h(card_max_h)
            .child(modal::header("Type Aliases", cx).child(modal::close_button(
                "alias-close",
                cx.listener(|this, _e, _w, cx| this.cancel(cx)),
                cx,
            )))
            .child(body)
            .child(footer)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PluginManagerDialog — the read-only Plugins manager (the C++ showPluginsDialog)
// ─────────────────────────────────────────────────────────────────────────────

/// One row in the Plugins manager (the C++ `IPlugin` descriptor; main.cpp:8845),
/// upgraded to the Phase-6 disclosure model (design §6 Phase 6): beyond the C++
/// name·version·type·author·description it carries the auto-detected **kind
/// label** (design §4), the **enabled** state (design §7.A [fix] — C++ had none),
/// and the declared **permissions** (design §5/§6 disclosure).
#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginInfo {
    name: String,
    version: String,
    /// The detected-kind label (`builtin` / `native` / `reclassnet-native` /
    /// `reclassnet-managed` / `process`).
    kind: String,
    author: String,
    description: String,
    enabled: bool,
    /// Whether this is a compiled-in built-in provider. Built-ins are **not**
    /// runtime-unloadable (the C++ never `dlopen`'d them; parity), so the dialog
    /// shows no Unload control on a built-in row — only on a loaded native plugin.
    is_builtin: bool,
    /// The derived routing identifier (`Name().toLower().replace(" ","")`) — the
    /// key the dialog hands back to [`PluginManager::set_enabled`] when the user
    /// flips this row's enabled state (design §7.A [fix] / §H).
    identifier: String,
    /// The human-readable permission tokens (e.g. `read_memory`), for disclosure.
    permissions: Vec<String>,
}

/// The plugin rows the Manage Plugins dialog shows, **derived from the real
/// [`PluginManager`](crate::plugin::PluginManager)** via
/// [`plugins_view`](crate::plugin::PluginManager::plugins_view) (design §6 Phase 6:
/// the dialog renders the live plugin set — built-in, native, or auto-detected
/// ReClass.NET — not a second hand-kept table). The C++ `"<name> Provider"`
/// display text is preserved for the built-in providers. Native DLL/SO loading
/// stays out of the default build (the dialog notes it).
/// The four-built-in row list mapped to [`PluginInfo`] — the parity baseline (the
/// live opener reads the session-owned manager instead, but tests assert the shipped
/// built-in set's display fields through this). `#[cfg(test)]` because the only
/// non-test caller now reads the owned manager.
#[cfg(test)]
fn builtin_plugins() -> Vec<PluginInfo> {
    plugin_infos_from_rows(crate::plugin::PluginManager::with_builtins().plugins_view())
}

/// Map a [`PluginManager::plugins_view`](crate::plugin::PluginManager::plugins_view)
/// row list to the dialog's [`PluginInfo`] view-model (design §6 Phase 6). The C++
/// `"<name> Provider"` display text is preserved for built-in providers. Shared by
/// the live opener (reading the session-owned manager) and the parity helper
/// [`builtin_plugins`].
fn plugin_infos_from_rows(rows: Vec<crate::plugin::PluginRow>) -> Vec<PluginInfo> {
    rows.into_iter()
        .map(|r| PluginInfo {
            // Read `is_builtin` before `r.name` is moved by the display branch.
            is_builtin: r.is_builtin,
            name: if r.is_builtin {
                format!("{} Provider", r.name)
            } else {
                r.name
            },
            version: r.version,
            kind: r.detected_label.to_string(),
            author: r.author,
            description: r.description,
            enabled: r.enabled,
            // The derived routing identifier, so the dialog can ask the manager to
            // flip this exact plugin's enabled flag (design §7.A [fix] / §H).
            identifier: r.identifier,
            permissions: r
                .permissions
                .iter()
                .map(|p| p.as_str().to_string())
                .collect(),
        })
        .collect()
}

/// The Plugins manager's outcome. `Close` ends the dialog; `Toggle` asks the host
/// to flip a plugin's enabled flag (design §7.A [fix] / §H — the dialog is no longer
/// read-only: enable/disable drives the session-owned manager + persists). The
/// dialog does not own the manager, so it reports the intent and the window applies
/// it (then pushes the refreshed rows back via [`PluginManagerDialog::set_plugins`]).
#[derive(Clone, Debug)]
enum PluginManagerEvent {
    Close,
    /// Flip `identifier` to `enabled` (the new state the user clicked toward).
    Toggle {
        identifier: String,
        enabled: bool,
    },
    /// **Safe-unload** the plugin `identifier` (design §7.A [fix]). Emitted only by
    /// a NON-builtin row's Unload button (built-ins are never `dlopen`'d, so they
    /// have no Unload control — parity). The host detaches affected documents
    /// FIRST, then drops the plugin + its backing library, then refreshes the rows.
    Unload {
        identifier: String,
    },
    /// Load a native plugin from a user-chosen path (the C++ load-from-path;
    /// design §6 Phase 3/6). Only present + handled behind the `plugins` feature —
    /// the default build ships no runtime loader, so this variant doesn't exist
    /// there (and the dialog shows no "Load plugin…" button).
    #[cfg(feature = "plugins")]
    Load,
}

/// The read-only Plugins manager view (the C++ `showPluginsDialog`; main.cpp:8821).
/// Lists the built-in provider plugins with the same fields the C++ shows
/// (name·version·type·author·description) and a single Close button. The C++
/// Load/Unload buttons drove native `dlopen`/`dlclose`, which has no analogue on
/// this platform, so they are replaced by a one-line "loading runtime plugins is
/// not supported in this build" note (the honest boundary).
struct PluginManagerDialog {
    plugins: Vec<PluginInfo>,
    /// Retained native-load failures, `(path, detail)` (design §7.A [fix]). Always
    /// present so `new()` stays uniform across builds; populated only under the
    /// `plugins` feature (the default build has no runtime loader, so it stays
    /// empty and the render emits nothing — parity). The opener pushes the session
    /// manager's `load_errors()` in via [`set_load_errors`](Self::set_load_errors).
    load_errors: Vec<(std::path::PathBuf, String)>,
    focus_handle: FocusHandle,
}

impl PluginManagerDialog {
    fn new(plugins: Vec<PluginInfo>, cx: &mut Context<Self>) -> Self {
        PluginManagerDialog {
            plugins,
            load_errors: Vec::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Install the session manager's retained native-load failures so the dialog
    /// can surface them with detail (design §7.A [fix] — C++ logs then drops the
    /// detail). Feature-gated: the default build has no loader, so neither the
    /// caller nor this setter exists there and `load_errors` stays empty.
    #[cfg(feature = "plugins")]
    fn set_load_errors(&mut self, errs: Vec<(std::path::PathBuf, String)>, cx: &mut Context<Self>) {
        self.load_errors = errs;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Close);
    }

    /// Emit the intent to flip `identifier` to `enabled` (the host owns the manager
    /// and its persistence). The host applies it and pushes the refreshed rows back
    /// via [`set_plugins`](Self::set_plugins), so the chip re-renders from the real
    /// manager state rather than the dialog guessing.
    fn toggle(&mut self, identifier: String, enabled: bool, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Toggle {
            identifier,
            enabled,
        });
    }

    /// Emit the intent to **safe-unload** `identifier` (design §7.A [fix]). Mirrors
    /// [`toggle`](Self::toggle): the host owns the manager, applies the unload
    /// (detach-first), and pushes the refreshed rows back via
    /// [`set_plugins`](Self::set_plugins). Only a non-builtin row wires this.
    fn unload(&mut self, identifier: String, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Unload { identifier });
    }

    /// Emit the intent to load a native plugin from a path (the C++ load-from-path;
    /// design §6 Phase 3/6). Feature-gated — the default build has no loader, so
    /// neither the button nor this method exists there.
    #[cfg(feature = "plugins")]
    fn load(&mut self, cx: &mut Context<Self>) {
        cx.emit(PluginManagerEvent::Load);
    }

    /// Replace the rendered rows (the host calls this after applying a toggle so the
    /// enabled chip reflects the live [`PluginManager`](crate::plugin::PluginManager)
    /// state).
    fn set_plugins(&mut self, plugins: Vec<PluginInfo>, cx: &mut Context<Self>) {
        self.plugins = plugins;
        cx.notify();
    }
}

impl EventEmitter<PluginManagerEvent> for PluginManagerDialog {}

impl Focusable for PluginManagerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PluginManagerDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::design::{color, tokens};
        use super::dialogs::modal;
        use gpui_component::button::{Button, ButtonVariants as _};

        let card_w = modal::clamp_width(620., window);
        let card_max_h = modal::clamp_height(440., 100., window);

        // One card per plugin (the C++ `QListWidgetItem` rows).
        let rows: Vec<AnyElement> = self
            .plugins
            .iter()
            .map(|p| {
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(2.))
                    .p(px(tokens::space::SM))
                    .rounded(px(tokens::radius::SM))
                    .bg(color::panel_bg(cx))
                    .border_1()
                    .border_color(color::border(cx))
                    .child(
                        gpui_component::h_flex()
                            .items_center()
                            .gap(px(tokens::space::SM))
                            .child(
                                div()
                                    .text_color(color::text(cx))
                                    .text_size(px(tokens::font::UI_MD))
                                    .child(format!("{} v{}", p.name, p.version)),
                            )
                            .child(
                                div()
                                    .px(px(tokens::space::XS))
                                    .rounded(px(tokens::radius::SM))
                                    .bg(color::selected_bg(cx))
                                    .text_color(color::text_muted(cx))
                                    .text_size(px(tokens::font::UI_SM))
                                    .child(p.kind.clone()),
                            )
                            // Enabled/disabled chip (design §7.A [fix] — C++ had
                            // no enable state to show).
                            .child(
                                div()
                                    .px(px(tokens::space::XS))
                                    .rounded(px(tokens::radius::SM))
                                    .bg(color::selected_bg(cx))
                                    .text_color(if p.enabled {
                                        color::accent(cx)
                                    } else {
                                        color::text_disabled(cx)
                                    })
                                    .text_size(px(tokens::font::UI_SM))
                                    .child(if p.enabled { "enabled" } else { "disabled" }),
                            )
                            // Push the Enable/Disable control to the right edge.
                            .child(div().flex_grow())
                            // The Enable/Disable toggle (design §7.A [fix] / §H —
                            // the dialog is no longer read-only; the click flips the
                            // session-owned manager + persists). The button reports
                            // the *target* state (`!p.enabled`); the host applies it
                            // and pushes refreshed rows back.
                            .child(
                                Button::new(SharedString::from(format!(
                                    "plugin-toggle-{}",
                                    p.identifier
                                )))
                                .ghost()
                                .label(if p.enabled { "Disable" } else { "Enable" })
                                .on_click(cx.listener({
                                    let id = p.identifier.clone();
                                    let target = !p.enabled;
                                    move |this, _e, _w, cx| {
                                        this.toggle(id.clone(), target, cx);
                                    }
                                })),
                            )
                            // Unload — only a loaded (non-builtin) plugin shows it
                            // (built-ins are never `dlopen`'d, so there is nothing
                            // to unload; parity). A Destructive button: the click
                            // safe-unloads (detach-first) on the session manager.
                            .when(!p.is_builtin, |row| {
                                row.child(
                                    Button::new(SharedString::from(format!(
                                        "plugin-unload-{}",
                                        p.identifier
                                    )))
                                    .danger()
                                    .label("Unload")
                                    .on_click(cx.listener({
                                        let id = p.identifier.clone();
                                        move |this, _e, _w, cx| {
                                            this.unload(id.clone(), cx);
                                        }
                                    })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(p.description.to_string()),
                    )
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(format!("Author: {}", p.author)),
                    )
                    // Declared-permission disclosure (design §5/§6 — native plugins
                    // disclose capabilities; built-ins typically have none).
                    .child(
                        div()
                            .text_color(color::text_muted(cx))
                            .text_size(px(tokens::font::UI_SM))
                            .child(if p.permissions.is_empty() {
                                "Permissions: none".to_string()
                            } else {
                                format!("Permissions: {}", p.permissions.join(", "))
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        // The footer note. With the `plugins` feature the runtime loader is
        // present (so the wording invites Load plugin…); without it, the honest
        // boundary note stays verbatim (the C++ load-from-path has no analogue in
        // the default build).
        #[cfg(feature = "plugins")]
        let note_text = "Enable/Disable is applied to the session and persisted \
             across launches. Use “Load plugin…” to load a native plugin (.so/.dll) \
             from disk; built-in providers cannot be unloaded.";
        #[cfg(not(feature = "plugins"))]
        let note_text = "Enable/Disable is applied to the session and persisted \
             across launches. Loading runtime plugins (DLL/SO) is not supported in \
             this build; the provider plugins above are compiled in.";
        let note = div()
            .text_color(color::text_muted(cx))
            .text_size(px(tokens::font::UI_SM))
            .child(note_text);

        // Native-load failure section (design §7.A [fix] — surface ABI/load errors
        // with detail rather than only logging them, the way the C++ generic "check
        // the console" box did NOT). Gated on the `plugins` feature so the default
        // build (no loader, no errors) emits nothing — byte parity. Rendered ABOVE
        // the footer note, inside the same scroll area.
        #[cfg(feature = "plugins")]
        let error_section: Option<AnyElement> = if self.load_errors.is_empty() {
            None
        } else {
            let header = div()
                .text_color(cx.theme().danger)
                .text_size(px(tokens::font::UI_SM))
                .child(format!(
                    "Failed to load {} plugin(s):",
                    self.load_errors.len()
                ));
            let lines: Vec<AnyElement> = self
                .load_errors
                .iter()
                .map(|(path, detail)| {
                    div()
                        .text_color(color::text_muted(cx))
                        .text_size(px(tokens::font::UI_SM))
                        .child(format!("{}: {}", path.display(), detail))
                        .into_any_element()
                })
                .collect();
            Some(
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(tokens::space::XS))
                    .child(header)
                    .children(lines)
                    .into_any_element(),
            )
        };

        let body_col = gpui_component::v_flex()
            .id("rcx-plugin-rows")
            .w_full()
            .max_h(px(300.))
            .overflow_y_scroll()
            .gap(px(tokens::space::XS))
            .children(rows);
        #[cfg(feature = "plugins")]
        let body_col = body_col.children(error_section);
        let body = modal::body(cx).child(body_col.child(note));

        let footer = modal::footer(cx);
        // "Load plugin…" — the C++ load-from-path, only behind the `plugins`
        // feature (the default build has no runtime loader). Placed BEFORE Close.
        #[cfg(feature = "plugins")]
        let footer = footer.child(
            Button::new("plugins-load")
                .label("Load plugin…")
                .on_click(cx.listener(|this, _e, _w, cx| this.load(cx))),
        );
        let footer = footer.child(
            Button::new("plugins-close")
                .primary()
                .label("Close")
                .on_click(cx.listener(|this, _e, _w, cx| this.close(cx))),
        );

        modal::card(cx)
            .id("rcx-plugins")
            .track_focus(&self.focus_handle)
            .key_context("RcxPlugins")
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key.as_str() == "escape" {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            .w(card_w)
            .max_h(card_max_h)
            .child(modal::header("Plugins", cx).child(modal::close_button(
                "plugins-x",
                cx.listener(|this, _e, _w, cx| this.close(cx)),
                cx,
            )))
            .child(body)
            .child(footer)
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
    super::design::tokens::font::resolve_mono_family(&cx.text_system().all_font_names());

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
    let mut bindings = scope_editor_text_keys_to_non_field(super::editor::editor_key_bindings());
    bindings.extend(super::editor::inline_edit::field_key_bindings());
    bindings.extend(super::startpage::start_page_key_bindings());
    bindings.extend(super::commandpalette::command_palette_key_bindings());
    bindings.extend(super::findbar::find_bar_key_bindings());
    bindings.extend(super::scannerpanel::scanner_panel_key_bindings());
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
mod tests {
    // Headless tests for the pure dispatch helpers added with the action-wiring.
    // These import specific items (NOT `super::*`) so the module's `gpui::*` glob
    // is not pulled into the test-hygiene expansion (see the menubar.rs note).
    use super::{
        builtin_plugins, dirty_doc_name, root_name_for_title, seed_root_doc, settings_keys,
        sniff_is_reclass_xml, unique_dirty_names, unsaved_changes_text, window_title_string,
        DiskSettings, ExportKind, ImportKind, RootKind, ViewOpt, ViewOptions, ABOUT_GITHUB_URL,
    };
    use crate::theme::SettingsStore;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn temp_settings_path() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("reclass-settings-test-{n}.json"));
        p
    }

    #[test]
    fn disk_settings_round_trips_scalars_lists_and_bools_across_reopen() {
        let path = temp_settings_path();
        {
            let mut s = DiskSettings::open_at(path.clone());
            s.set("font", "Consolas");
            s.set_bool("minimap", true);
            s.set_list(
                "recentFiles",
                &["/a/one.rcx".to_string(), "/b/two.rcx".to_string()],
            );
        }
        // Reopen from disk — every value must survive (the QSettings semantics).
        let s2 = DiskSettings::open_at(path.clone());
        assert_eq!(s2.get("font").as_deref(), Some("Consolas"));
        assert!(s2.get_bool("minimap", false));
        assert_eq!(
            s2.get_list("recentFiles"),
            vec!["/a/one.rcx".to_string(), "/b/two.rcx".to_string()]
        );
        // A missing key falls back to the supplied default.
        assert!(s2.get_bool("definitely-missing", true));
        assert!(s2.get_list("definitely-missing").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn disk_settings_empty_list_round_trips_as_empty() {
        let path = temp_settings_path();
        let mut s = DiskSettings::open_at(path.clone());
        // Empty + whitespace-only entries are dropped so they round-trip empty.
        s.set_list("recentFiles", &["".to_string()]);
        assert!(s.get_list("recentFiles").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn view_options_load_reads_persisted_keys_and_defaults_the_rest() {
        let path = temp_settings_path();
        let mut s = DiskSettings::open_at(path.clone());
        // Persist a NON-default subset; the rest must fall back to C++ defaults.
        s.set_bool(settings_keys::TYPE_HINTS, true); // default false
        s.set_bool(settings_keys::COMPACT_COLUMNS, false); // default true
        let o = ViewOptions::load(&s);
        assert!(o.type_hints, "persisted typeHints=true must load");
        assert!(
            !o.compact_columns,
            "persisted compactColumns=false must load"
        );
        // Unset keys keep the defaults.
        assert!(o.tree_lines);
        assert!(o.relative_offsets);
        assert!(o.hover_effects);
        assert!(!o.show_comments);
        assert!(!o.minimap);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn view_option_persist_key_matches_cpp_qsettings_key() {
        // The keys MUST match the C++ QSettings spellings (main.cpp:1336-1411)
        // so a project edited in either build reads the other's saved toggles.
        assert_eq!(ViewOptions::key(ViewOpt::CompactColumns), "compactColumns");
        assert_eq!(ViewOptions::key(ViewOpt::TreeLines), "treeLines");
        assert_eq!(
            ViewOptions::key(ViewOpt::RelativeOffsets),
            "relativeOffsets"
        );
        assert_eq!(ViewOptions::key(ViewOpt::TypeHints), "typeHints");
        assert_eq!(ViewOptions::key(ViewOpt::ShowComments), "showComments");
        assert_eq!(ViewOptions::key(ViewOpt::HoverEffects), "hoverEffects");
        assert_eq!(ViewOptions::key(ViewOpt::Minimap), "minimap");
    }

    #[test]
    fn source_for_controller_reads_active_saved_file_source() {
        // The #3 fix: after a controller ingests a saved File source, the tab
        // icon must read the ACTIVE source (its file path), not the doc's
        // pre-ingest data_path. Build a controller with a savedSources File
        // entry pointing at a real on-disk file and assert the derived source.
        use crate::controller::{RcxController, RcxDocument};
        use serde_json::json;

        // A real sidecar file so the auto-attach does not bail.
        let data = temp_settings_path().with_extension("bin");
        std::fs::write(&data, b"hello").unwrap();

        let mut doc = RcxDocument::new();
        doc.pending_saved_sources.push(json!({
            "kind": "File",
            "displayName": "sample.bin",
            "filePath": data.to_string_lossy(),
        }));
        let ctrl = RcxController::new(doc);
        let src = super::MainWindow::source_for_controller(&ctrl);
        assert_eq!(src.kind, crate::ui::state::SourceKind::File);
        assert_eq!(src.target, data.to_string_lossy());
        let _ = std::fs::remove_file(&data);
    }

    // ── Dynamic window title (the C++ updateWindowTitle) ──

    #[test]
    fn window_title_formats_name_dirty_and_empty() {
        // `"<name> - Reclass"` when clean; a trailing `" *"` before " - Reclass"
        // when modified; plain "Reclass" when the name is empty (no document).
        assert_eq!(window_title_string("Player", false), "Player - Reclass");
        assert_eq!(window_title_string("Player", true), "Player * - Reclass");
        assert_eq!(window_title_string("", false), "Reclass");
        assert_eq!(window_title_string("", true), "Reclass");
    }

    // ── Unsaved-changes guard (the C++ closeEvent; item 1) ──

    #[test]
    fn unsaved_changes_text_picks_sentence_by_count() {
        // The C++ uses two complete sentences keyed on the distinct-dirty-doc
        // count (main.cpp:9003-9006): singular for one, "%1 projects …" otherwise.
        assert_eq!(unsaved_changes_text(1), "One project has unsaved changes:");
        assert_eq!(unsaved_changes_text(2), "2 projects have unsaved changes:");
        assert_eq!(unsaved_changes_text(7), "7 projects have unsaved changes:");
    }

    #[test]
    fn unique_dirty_names_dedups_preserving_first_seen_order() {
        // The C++ `dirtyNames` skips repeats (a doc shared across tabs is listed
        // once; main.cpp:8994-8995) while keeping discovery order.
        let names = vec![
            "Player".to_string(),
            "World".to_string(),
            "Player".to_string(), // duplicate (shared doc) → dropped
            "Enemy".to_string(),
        ];
        assert_eq!(
            unique_dirty_names(names),
            vec![
                "Player".to_string(),
                "World".to_string(),
                "Enemy".to_string()
            ]
        );
        // Empty in → empty out (nothing dirty).
        assert!(unique_dirty_names(Vec::<String>::new()).is_empty());
    }

    // ── A1: window-close guard decision logic (the C++ closeEvent; main.cpp:8989) ──

    #[test]
    fn clean_document_yields_no_dirty_name_so_close_is_accepted() {
        // Port of the C++ `closeEvent` early-out (main.cpp:8989, 8998): an
        // unmodified document contributes NO dirty name. `collect_dirty_docs`
        // filters each tab through `dirty_doc_name`; a set of only-clean docs
        // therefore collects empty ⇒ `guarded_window_close` returns true (the Qt
        // `event->accept()` — close immediately, no prompt). `seed_root_doc` builds
        // a fresh doc which starts `modified == false`.
        let doc = seed_root_doc(RootKind::Class);
        assert!(!doc.modified, "a freshly seeded doc starts clean");
        let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
        assert_eq!(
            dirty_doc_name(doc.modified, doc.file_path.as_deref(), &doc.tree, root_id),
            None,
            "a clean doc must not enter the dirty set (allow-close path)"
        );
    }

    #[test]
    fn dirty_unsaved_document_names_by_view_root_struct() {
        // The C++ name rule for a dirty, never-saved doc: the view-root struct name
        // (main.cpp:8991, the `filePath.isEmpty()` branch). A dirty doc DOES enter
        // the set (the prompt path).
        let doc = seed_root_doc(RootKind::Class);
        let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
        assert_eq!(
            dirty_doc_name(true, None, &doc.tree, root_id),
            Some(root_name_for_title(&doc.tree, root_id)),
            "an unsaved dirty doc names by its view-root struct"
        );
    }

    #[test]
    fn dirty_saved_document_names_by_file_basename() {
        // The C++ name rule for a dirty, saved doc: the file BASENAME (main.cpp:8993,
        // `QFileInfo(filePath).fileName()`), not the struct name and not the full path.
        let doc = seed_root_doc(RootKind::Class);
        let root_id = doc.tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
        let path = std::path::PathBuf::from("/home/u/projects/Player.rcx");
        assert_eq!(
            dirty_doc_name(true, Some(path.as_path()), &doc.tree, root_id),
            Some("Player.rcx".to_string()),
            "a saved dirty doc names by its file basename"
        );
    }

    // ── Help ▸ About GitHub URL (the C++ about() "Open GitHub"; item 8) ──

    #[test]
    fn about_github_url_points_at_ichooseyou_repo() {
        // The C++ About dialog opens https://github.com/IChooseYou/Reclass
        // (main.cpp:4434), NOT the old reclassnet/reclass URL.
        assert_eq!(ABOUT_GITHUB_URL, "https://github.com/IChooseYou/Reclass");
        assert!(!ABOUT_GITHUB_URL.contains("reclassnet"));
    }

    #[test]
    fn root_name_for_title_uses_view_root_struct_name() {
        // The window title names the active VIEW ROOT's top-level struct: build a
        // class document and assert its struct_type_name is returned for the root,
        // and that climbing from a child reaches the same root name.
        let doc = seed_root_doc(RootKind::Class);
        let tree = &doc.tree;
        // The root is the first top-level node; its view-root id names the title.
        let root_id = tree.nodes.iter().find(|n| n.parent_id == 0).unwrap().id;
        let name = root_name_for_title(tree, root_id);
        assert_eq!(name, RootKind::Class.type_name());
        // A `0`/unknown view root still resolves to the first top-level struct.
        assert_eq!(root_name_for_title(tree, 0), RootKind::Class.type_name());
        // Climbing from a child field reaches the same root name.
        if let Some(child) = tree.nodes.iter().find(|n| n.parent_id == root_id) {
            assert_eq!(
                root_name_for_title(tree, child.id),
                RootKind::Class.type_name()
            );
        }
    }

    // ── Plugins manager (the read-only port of showPluginsDialog) ──

    #[test]
    fn builtin_plugins_lists_the_provider_backends() {
        let plugins = builtin_plugins();
        // Every shipped provider backend is an in-tree built-in (the Phase-6
        // detected-kind label, design §4/§6).
        assert!(!plugins.is_empty());
        assert!(plugins.iter().all(|p| p.kind == "builtin"));
        // The built-ins auto-load (LoadType::Auto) → all shown enabled (design
        // §7.A [fix] enabled-state disclosure).
        assert!(plugins.iter().all(|p| p.enabled));
        let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"File Provider"));
        assert!(names.contains(&"Null Provider"));
        // Each row carries the fields the C++ dialog shows.
        assert!(plugins.iter().all(|p| {
            !p.version.is_empty() && !p.author.is_empty() && !p.description.is_empty()
        }));
        // Every row carries its routing identifier (so the dialog can toggle it).
        assert!(plugins.iter().all(|p| !p.identifier.is_empty()));
    }

    // ── F1: session-owned PluginManager + DiskSettings-backed persistence ──
    //
    // These exercise the EXACT production wiring `MainWindow::new` builds — a
    // `PluginManager::with_persistence_and_builtins` over a `DiskPluginPersistence`
    // backed by the real `DiskSettings` (settings.json) — but at a temp path, so no
    // GPUI window is needed to assert the enable/disable + persistence behaviour.

    use crate::plugin::{DiskPluginPersistence, PluginManager};

    /// Build the session manager exactly as the window does, over a DiskSettings at
    /// `path` (coerced to the shared `SettingsStore` trait object).
    fn session_manager_at(path: &std::path::Path) -> PluginManager {
        let settings: Rc<RefCell<dyn SettingsStore>> =
            Rc::new(RefCell::new(DiskSettings::open_at(path.to_path_buf())));
        PluginManager::with_persistence_and_builtins(Box::new(DiskPluginPersistence::new(settings)))
    }

    #[test]
    fn session_manager_parity_with_empty_settings() {
        // PARITY: a fresh config dir → the registry is exactly the four
        // Auto-enabled built-ins, identical order, all enabled — byte-identical to
        // the old throwaway `with_builtins()` the live sites used.
        let path = temp_settings_path();
        let mgr = session_manager_at(&path);
        let ids: Vec<&str> = mgr
            .registry()
            .providers()
            .iter()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(ids, ["file", "buffer", "snapshot", "null"]);
        assert!(mgr
            .registry()
            .providers()
            .iter()
            .all(|p| p.is_builtin && p.enabled));
        assert_eq!(mgr.registry().enabled_providers().count(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn disk_backed_enable_disable_reflected_in_view_registry_and_persisted() {
        let path = temp_settings_path();
        {
            // Disable a built-in via the manager with persist=true (the dialog's
            // Toggle path).
            let mut mgr = session_manager_at(&path);
            assert!(mgr.set_enabled("null", false, true));

            // Reflected in plugins_view (what the dialog renders)…
            let rows = mgr.plugins_view();
            let null = rows.iter().find(|r| r.identifier == "null").unwrap();
            assert!(!null.enabled);
            // …and in the registry's enabled_providers (what the pickers read).
            let enabled: Vec<&str> = mgr
                .registry()
                .enabled_providers()
                .map(|p| p.identifier.as_str())
                .collect();
            assert_eq!(enabled, ["file", "buffer", "snapshot"]);
        }
        // The flag landed in settings.json under the namespaced key.
        let s = DiskSettings::open_at(path.clone());
        assert_eq!(s.get("plugin.enabled.null").as_deref(), Some("false"));

        // RESTART: a brand-new session manager over the SAME file restores the
        // disabled built-in (the persistence round-trip end-to-end).
        let mgr2 = session_manager_at(&path);
        assert!(!mgr2.registry().find("null").unwrap().enabled);
        assert_eq!(mgr2.registry().enabled_providers().count(), 3);

        // Re-enabling persists too, so a third session sees it back on.
        {
            let mut mgr3 = session_manager_at(&path);
            assert!(mgr3.set_enabled("null", true, true));
        }
        let mgr4 = session_manager_at(&path);
        assert!(mgr4.registry().find("null").unwrap().enabled);
        assert_eq!(mgr4.registry().enabled_providers().count(), 4);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn plugin_infos_from_rows_preserves_builtin_display_and_identifier() {
        // The dialog's view-model mapper: built-ins get the C++ "<name> Provider"
        // text, the routing identifier is carried, and the enabled flag tracks the
        // manager (here a disabled built-in shows disabled).
        let path = temp_settings_path();
        let mut mgr = session_manager_at(&path);
        assert!(mgr.set_enabled("null", false, true));
        let infos = super::plugin_infos_from_rows(mgr.plugins_view());

        let file = infos.iter().find(|i| i.identifier == "file").unwrap();
        assert_eq!(file.name, "File Provider");
        assert!(file.enabled);
        assert_eq!(file.kind, "builtin");

        let null = infos.iter().find(|i| i.identifier == "null").unwrap();
        assert_eq!(null.name, "Null Provider");
        assert!(
            !null.enabled,
            "disabled built-in shows disabled in the dialog"
        );
        // Built-ins carry is_builtin = true (so the dialog shows no Unload control).
        assert!(infos.iter().all(|i| i.is_builtin));
        let _ = std::fs::remove_file(&path);
    }

    // ── F2: live-host safe-unload + the dialog's row refresh contract ──
    //
    // The window-backed `LivePluginHost` is gpui-bound (it needs a `Window`/`App`),
    // so following the repo convention (no gpui-bound tests — see the tabs.rs note)
    // these assert the NON-gpui logic the `Unload` arm runs: the manager's
    // safe_unload + the `plugin_infos_from_rows` row refresh. The host's
    // identifier→SourceKind detach mapping is tested in `pluginhost.rs`, and the
    // concrete tab close in `tabs.rs::detach_sources_of_kind_*`.

    /// A throwaway native (non-builtin) provider plugin, so a test can add a
    /// runtime-loaded-style plugin to the session manager and then unload it.
    struct UnloadableProvider {
        manifest: crate::plugin::PluginManifest,
    }
    impl crate::plugin::Plugin for UnloadableProvider {
        fn manifest(&self) -> &crate::plugin::PluginManifest {
            &self.manifest
        }
        fn contributions(&self) -> Vec<crate::plugin::Contribution> {
            vec![crate::plugin::Contribution::Provider(
                crate::plugin::ProviderSpec::new(
                    |_t| true,
                    |t| {
                        Ok(
                            std::sync::Arc::new(crate::provider::BufferProvider::new(vec![], t))
                                as crate::plugin::SharedProvider,
                        )
                    },
                ),
            )]
        }
    }

    fn unloadable_native(name: &str) -> Box<dyn crate::plugin::Plugin> {
        let mut m = crate::plugin::PluginManifest::builtin(
            name,
            "a runtime-loaded native provider",
            vec![crate::plugin::Permission::ReadMemory],
        );
        m.kind = crate::plugin::PluginKind::Native;
        m.load = crate::plugin::LoadType::Auto;
        m.dll_file_name = format!("{}.so", name.to_lowercase());
        Box::new(UnloadableProvider { manifest: m })
    }

    #[test]
    fn plugin_info_marks_loaded_native_as_not_builtin() {
        // A loaded native plugin is is_builtin = false → the dialog renders its
        // Unload control (the per-row guard), while built-ins do not.
        let path = temp_settings_path();
        let mut mgr = session_manager_at(&path);
        let id = mgr.add_plugin(unloadable_native("Remote Reader"));
        let infos = super::plugin_infos_from_rows(mgr.plugins_view());

        let native = infos.iter().find(|i| i.identifier == id).unwrap();
        assert!(!native.is_builtin, "a loaded native plugin is not built-in");
        // Its name is NOT given the built-in "<name> Provider" suffix.
        assert_eq!(native.name, "Remote Reader");
        // The built-ins are still flagged built-in.
        assert!(infos
            .iter()
            .filter(|i| i.identifier != id)
            .all(|i| i.is_builtin));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn safe_unload_through_host_removes_row_and_keeps_builtins() {
        // The exact non-gpui work the dialog's Unload arm does: safe_unload on the
        // session manager (with a host) drops the plugin's row + provider, and the
        // refreshed `plugin_infos_from_rows` no longer lists it while every
        // built-in survives (PARITY: built-ins are never unloaded).
        let path = temp_settings_path();
        let mut mgr = session_manager_at(&path);
        let id = mgr.add_plugin(unloadable_native("Doomed Reader"));

        // Pre-unload: the row + the registered provider are present.
        let before = super::plugin_infos_from_rows(mgr.plugins_view());
        assert!(before.iter().any(|i| i.identifier == id));
        assert!(mgr.registry().find(&id).is_some());

        // Safe-unload through a host (MockPluginHost stands in for the gpui-bound
        // LivePluginHost; both call detach_documents_using FIRST inside safe_unload).
        let mut host = crate::plugin::MockPluginHost::new();
        assert!(mgr.safe_unload(&id, &mut host));
        // The host WAS asked to detach the unloaded provider's documents first.
        assert_eq!(host.detached(), [id.as_str()]);

        // Post-unload: the row + provider are gone…
        let after = super::plugin_infos_from_rows(mgr.plugins_view());
        assert!(!after.iter().any(|i| i.identifier == id));
        assert!(mgr.registry().find(&id).is_none());
        // …and every built-in is still listed + still registered (parity).
        for builtin in ["file", "buffer", "snapshot", "null"] {
            assert!(
                after.iter().any(|i| i.identifier == builtin),
                "built-in {builtin} survives the unload"
            );
            assert!(mgr.registry().find(builtin).is_some());
        }
        assert_eq!(mgr.registry().providers().len(), 4);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn view_options_default_matches_cpp_view_menu() {
        // C++ persisted QSettings defaults (main.cpp:1336-1411): compactColumns,
        // treeLines, relativeOffsets, hoverEffects ON; typeHints, showComments,
        // minimap OFF.
        let d = ViewOptions::default();
        assert!(d.compact_columns);
        assert!(d.tree_lines);
        assert!(d.relative_offsets);
        assert!(!d.type_hints);
        assert!(!d.show_comments);
        assert!(d.hover_effects);
        assert!(!d.minimap);
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

    /// Build a tree with TWO independent top-level structs (no reference between
    /// them) so a `*_tree`/view-root export would only emit ONE while the
    /// full-SDK export emits BOTH — the property the C++ `exportToFile`
    /// (`renderCodeAll`; main.cpp:5764) guarantees.
    fn two_independent_structs() -> (crate::core::NodeTree, u64) {
        use crate::core::{Node, NodeKind, NodeTree};
        let mut tree = NodeTree::new();
        let ai = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "StructA".into(),
            struct_type_name: "StructA".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let a_id = tree.nodes[ai].id;
        tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "valueA".into(),
            parent_id: a_id,
            offset: 0,
            ..Node::default()
        });
        let bi = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "StructB".into(),
            struct_type_name: "StructB".into(),
            parent_id: 0,
            offset: 0x100,
            ..Node::default()
        });
        let b_id = tree.nodes[bi].id;
        tree.add_node(Node {
            kind: NodeKind::UInt64,
            name: "valueB".into(),
            parent_id: b_id,
            offset: 0,
            ..Node::default()
        });
        (tree, a_id)
    }

    #[test]
    fn export_code_format_maps_kind_to_generator_format() {
        use crate::generator::CodeFormat;
        assert_eq!(ExportKind::Cpp.code_format(), Some(CodeFormat::CppHeader));
        assert_eq!(ExportKind::Rust.code_format(), Some(CodeFormat::RustStruct));
        assert_eq!(
            ExportKind::Defines.code_format(),
            Some(CodeFormat::DefineOffsets)
        );
        assert_eq!(
            ExportKind::CSharp.code_format(),
            Some(CodeFormat::CSharpStruct)
        );
        assert_eq!(
            ExportKind::Python.code_format(),
            Some(CodeFormat::PythonCtypes)
        );
        // XML is not a generator format (routes through the importer's exporter).
        assert_eq!(ExportKind::Xml.code_format(), None);
    }

    #[test]
    fn export_file_filter_matches_code_format_file_filter() {
        // Each code export's filter mirrors the generator's per-format filter
        // (the C++ `codeFormatFileFilter`; main.cpp:5758). XML uses its own.
        for (kind, fmt) in [
            (ExportKind::Cpp, crate::generator::CodeFormat::CppHeader),
            (ExportKind::Rust, crate::generator::CodeFormat::RustStruct),
            (
                ExportKind::Defines,
                crate::generator::CodeFormat::DefineOffsets,
            ),
            (
                ExportKind::CSharp,
                crate::generator::CodeFormat::CSharpStruct,
            ),
            (
                ExportKind::Python,
                crate::generator::CodeFormat::PythonCtypes,
            ),
        ] {
            assert_eq!(
                kind.file_filter(),
                crate::generator::code_format_file_filter(fmt)
            );
        }
        assert!(ExportKind::Xml.file_filter().to_lowercase().contains("xml"));
    }

    #[test]
    fn export_renders_full_sdk_regardless_of_view_root() {
        // The C++ export always calls renderCodeAll — every root struct, ignoring
        // the open view root. So even with a non-zero `a_id` selected, BOTH
        // structs must appear in the output.
        let (tree, _a_id) = two_independent_structs();
        let out = ExportKind::Cpp
            .render(&tree, None, false)
            .expect("non-empty C++ export");
        assert!(out.contains("struct StructA"), "missing StructA:\n{out}");
        assert!(out.contains("struct StructB"), "missing StructB:\n{out}");
    }

    #[test]
    fn export_threads_aliases_and_asserts() {
        use crate::core::NodeKind;
        use crate::generator::TypeAliases;
        let (tree, _a_id) = two_independent_structs();

        // emit_asserts=false → no static_assert; true → present (the persisted
        // generatorAsserts flag, threaded through the export; main.cpp:5763).
        let no_assert = ExportKind::Cpp.render(&tree, None, false).unwrap();
        assert!(!no_assert.contains("static_assert"));
        let with_assert = ExportKind::Cpp.render(&tree, None, true).unwrap();
        assert!(with_assert.contains("static_assert"));

        // type_aliases override the rendered type name (the C++ tab->doc->typeAliases
        // passed to renderCodeAll; main.cpp:5761-5764).
        let mut aliases: TypeAliases = TypeAliases::new();
        aliases.insert(NodeKind::Int32, "LONG".into());
        let aliased = ExportKind::Cpp
            .render(&tree, Some(&aliases), false)
            .unwrap();
        assert!(aliased.contains("LONG"), "alias not applied:\n{aliased}");
    }

    #[test]
    fn export_no_struct_tree_emits_header_but_no_structs() {
        // The C++ `renderCodeAll` (and thus the export) emits the `#pragma once`
        // header even when there are no structs (generator: full_sdk_no_structs),
        // so the C++-header export is non-empty but struct-free — NOT `None`.
        use crate::core::NodeTree;
        let tree = NodeTree::new();
        let out = ExportKind::Cpp
            .render(&tree, None, false)
            .expect("C++ header export is the bare header, not None");
        assert!(out.contains("#pragma once"));
        assert!(!out.contains("struct "));
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

    #[test]
    fn root_kind_class_keyword_and_title_are_distinct() {
        // The three New commands must seed distinct root kinds + titles (the bug:
        // all three collapsed into one blank "Untitled").
        assert_eq!(RootKind::Class.class_keyword(), "class");
        assert_eq!(RootKind::Struct.class_keyword(), "struct");
        assert_eq!(RootKind::Enum.class_keyword(), "enum");
        let titles = [
            RootKind::Class.title(),
            RootKind::Struct.title(),
            RootKind::Enum.title(),
        ];
        let mut uniq = titles.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3, "each kind must have a distinct tab title");
    }

    #[test]
    fn seed_root_doc_builds_a_root_struct_with_the_kind_keyword() {
        use crate::core::NodeKind;
        for kind in [RootKind::Class, RootKind::Struct, RootKind::Enum] {
            let doc = seed_root_doc(kind);
            // Exactly one top-level struct, carrying the kind's class_keyword.
            let roots: Vec<&crate::core::Node> = doc
                .tree
                .nodes
                .iter()
                .filter(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
                .collect();
            assert_eq!(roots.len(), 1, "{kind:?} should seed one root struct");
            assert_eq!(roots[0].class_keyword, kind.class_keyword());
            // The 16-field hex body landed under the root.
            let children = doc
                .tree
                .nodes
                .iter()
                .filter(|n| n.parent_id == roots[0].id)
                .count();
            assert_eq!(children, 16, "{kind:?} should seed 16 hex fields");
            // A sensible default base (the C++ template).
            assert_eq!(doc.tree.base_address, 0x0040_0000);
        }
    }

    #[test]
    fn relabel_command_flips_the_mcp_label() {
        use crate::ui::commandpalette::{menu_tree_with, MenuNode};
        let mut tree = menu_tree_with(&[], &[]);
        super::relabel_command(&mut tree, "tools.mcp", "Stop MCP Server");
        // Find the relabelled leaf.
        fn find<'a>(nodes: &'a [MenuNode], cmd: &str) -> Option<&'a str> {
            for n in nodes {
                match n {
                    MenuNode::Item { label, command, .. } if command == cmd => {
                        return Some(label.as_str())
                    }
                    MenuNode::Submenu { children, .. } => {
                        if let Some(l) = find(children, cmd) {
                            return Some(l);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        assert_eq!(find(&tree, "tools.mcp"), Some("Stop MCP Server"));
    }

    #[test]
    fn editor_text_keys_are_scoped_off_the_inline_field() {
        // The cross-cutting inline-edit fix: bare printable editor accelerators must
        // be re-scoped to `RcxEditor && !RcxFieldInput` so they fall through to the
        // focused field's text input, while modified / navigation accelerators keep
        // their plain `RcxEditor` scope. Import the gpui binding types by name (NOT
        // a `gpui::*` glob — see the module note) to keep test hygiene lean.
        use gpui::{actions, KeyBinding};

        actions!(rcx_test, [PrintableA, NamedSpace, ModifiedD, NavUp]);

        let input = vec![
            KeyBinding::new("t", PrintableA, Some("RcxEditor")), // bare printable → rescoped
            KeyBinding::new("space", NamedSpace, Some("RcxEditor")), // named printable → rescoped
            KeyBinding::new("ctrl-d", ModifiedD, Some("RcxEditor")), // modified → unchanged
            KeyBinding::new("up", NavUp, Some("RcxEditor")),     // navigation → unchanged
        ];
        let out = super::scope_editor_text_keys_to_non_field(input);
        let pred = |b: &KeyBinding| b.predicate().map(|p| p.to_string()).unwrap_or_default();

        assert_eq!(
            pred(&out[0]),
            "RcxEditor && !RcxFieldInput && !RcxFindBar",
            "bare `t` must dodge the field + find bar"
        );
        assert_eq!(
            pred(&out[1]),
            "RcxEditor && !RcxFieldInput && !RcxFindBar",
            "named printable `space` must dodge the field + find bar"
        );
        assert_eq!(
            pred(&out[2]),
            "RcxEditor",
            "`ctrl-d` is not text — left scoped to the editor"
        );
        assert_eq!(
            pred(&out[3]),
            "RcxEditor",
            "`up` navigation is not text — left scoped"
        );

        // The keystroke + action survive the rewrite (only the predicate changes).
        assert_eq!(out[0].keystrokes()[0].inner().key, "t");
    }

    // ── F3 live declarative-UI host: parity guard + menu injection + Elm loop ──
    //
    // The gpui mount (PluginPanel/PluginDialog into the dock/modal) follows the
    // repo convention of NOT being unit-tested (it needs a live Window); every
    // routing DECISION is covered here + in the manager/host modules. These tests
    // import the pure helpers + the gpui-free manager/MockPluginHost seam the live
    // path mirrors (so they stay headless — no `gpui::*` glob is pulled in).

    /// The direct children-commands of the &Plugins submenu, in order (the menu-bar
    /// surface the F3 injection targets).
    fn plugins_submenu_commands(tree: &[crate::ui::commandpalette::MenuNode]) -> Vec<String> {
        use crate::ui::commandpalette::MenuNode;
        for n in tree {
            if let MenuNode::Submenu { label, children } = n {
                if label == "&Plugins" {
                    return children
                        .iter()
                        .filter_map(|c| match c {
                            MenuNode::Item { command, .. } => Some(command.clone()),
                            _ => None,
                        })
                        .collect();
                }
            }
        }
        Vec::new()
    }

    #[test]
    fn plugins_menu_is_byte_identical_without_contributions() {
        // PARITY: with no plugin UI contributions, injecting leaves the &Plugins
        // submenu EXACTLY as the static tree built it — only [plugins.manage].
        use crate::ui::commandpalette::menu_tree_with;
        let mut tree = menu_tree_with(&[], &[]);
        let before = plugins_submenu_commands(&tree);
        assert_eq!(before, ["plugins.manage"]);
        // An empty contributions list is the default-build state.
        super::inject_plugin_menu_items(&mut tree, &[]);
        assert_eq!(plugins_submenu_commands(&tree), ["plugins.manage"]);
    }

    #[test]
    fn plugins_menu_injects_demo_commands_after_manage() {
        // With the demo loaded, its two Menu-slot commands are appended AFTER the
        // static Manage Plugins… row (so the existing row is untouched, and the
        // dialog dialog/panel — not Menu-slot — are NOT injected as menu items).
        use crate::plugin::PluginManager;
        use crate::ui::commandpalette::menu_tree_with;
        let mgr = PluginManager::with_builtins_and_demo();
        let mut tree = menu_tree_with(&[], &[]);
        super::inject_plugin_menu_items(&mut tree, &mgr.ui_contributions());
        assert_eq!(
            plugins_submenu_commands(&tree),
            [
                "plugins.manage",
                crate::plugin::demo::CMD_PING,
                crate::plugin::demo::CMD_OPEN_TARGET,
            ]
        );
    }

    #[test]
    fn dock_placement_maps_every_side() {
        use crate::plugin::DockSide;
        use gpui_component::dock::DockPlacement;
        assert!(matches!(
            super::dock_placement_for(DockSide::Left),
            DockPlacement::Left
        ));
        assert!(matches!(
            super::dock_placement_for(DockSide::Right),
            DockPlacement::Right
        ));
        assert!(matches!(
            super::dock_placement_for(DockSide::Bottom),
            DockPlacement::Bottom
        ));
    }

    #[test]
    fn demo_command_elm_round_trips_through_manager_and_host() {
        // The exact decisions `dispatch_plugin_command` / `route_plugin_panel_event`
        // make, on the gpui-free seam the live path mirrors: ping toasts;
        // open_target requests a dialog open (these two are the menu-injected
        // commands run_menu_command routes); the panel's Refresh button bumps the
        // counter and returns a fresh tree the panel re-renders.
        use crate::plugin::demo::{CMD_OPEN_TARGET, CMD_PING, CMD_REFRESH, DIALOG_ID, PANEL_ID};
        use crate::plugin::{MockPluginHost, PluginManager, UiEvent, ViewTree};

        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();

        // Only the two Menu-slot commands are plugin commands run_menu_command
        // routes (refresh is a panel button id, not a contributed command).
        assert!(mgr.is_plugin_command(CMD_PING));
        assert!(mgr.is_plugin_command(CMD_OPEN_TARGET));
        assert!(!mgr.is_plugin_command(CMD_REFRESH));

        // ping → a toast surfaces (the live dispatch drains host toasts → notify).
        let res = mgr.handle_command(CMD_PING, serde_json::Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(host.toasts(), ["Plugin Demo: pong"]);

        // open_target → the host records an open-dialog request (the live dispatch
        // drains open_dialogs → open_plugin_dialog).
        mgr.handle_command(CMD_OPEN_TARGET, serde_json::Value::Null, &mut host);
        assert_eq!(host.opened_dialogs(), [DIALOG_ID]);

        // Panel Refresh button → handle_ui_event returns Some(fresh tree) the live
        // `route_plugin_panel_event` pushes into the mounted PluginPanel; the tree
        // reflects the bumped counter.
        let tree = mgr
            .handle_ui_event(
                PANEL_ID,
                UiEvent::Clicked(CMD_REFRESH.to_string()),
                &mut host,
            )
            .expect("refresh re-renders the panel");
        let ViewTree::Column(children) = &tree else {
            panic!("panel root is a Column");
        };
        assert!(children.iter().any(|c| matches!(c, ViewTree::KeyValue(p)
            if p.iter().any(|(k, v)| k == "Refreshes" && v == "1"))));
        // And view_tree resolves the panel for a request_rerender drain.
        assert!(mgr.view_tree(PANEL_ID).is_some());
    }

    #[test]
    fn demo_dialog_attach_sets_source_and_closes_through_host() {
        // The dialog Ui round-trip: Attach sets the data source, asks to close the
        // dialog (the live `open_plugin_dialog` Ui handler closes the modal on that
        // request), and returns a fresh tree.
        use crate::plugin::demo::{BTN_ATTACH, DEMO_IDENTIFIER, DIALOG_ID};
        use crate::plugin::UiEvent;
        use crate::plugin::{MockPluginHost, PluginManager};

        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let tree = mgr.handle_ui_event(
            DIALOG_ID,
            UiEvent::Clicked(BTN_ATTACH.to_string()),
            &mut host,
        );
        assert!(tree.is_some(), "Attach re-renders the dialog");
        assert_eq!(host.closed_dialogs(), [DIALOG_ID]);
        assert_eq!(
            host.data_source(),
            Some(&(DEMO_IDENTIFIER.to_string(), "1234:notepad.exe".to_string()))
        );
    }

    // ── A2 GAP 2: ReClass-XML byte-sniff (the C++ project_open probe) ──

    #[test]
    fn sniff_detects_reclass_xml_signatures() {
        // Port of the C++ `head.trimmed().startsWith("<?xml") ||
        // startsWith("<ReClass")` (main.cpp:6129).
        // A `<?xml …` prolog → XML.
        assert!(sniff_is_reclass_xml(b"<?xml version=\"1.0\"?>"));
        // Leading whitespace is trimmed before the prefix test → still XML.
        assert!(sniff_is_reclass_xml(b"  <ReClass>"));
        assert!(sniff_is_reclass_xml(b"\n\t <?xml"));
        // A JSON document is NOT XML (the native `.rcx` load path).
        assert!(!sniff_is_reclass_xml(b"{\"json\": true}"));
        // Empty / non-matching bytes → not XML.
        assert!(!sniff_is_reclass_xml(b""));
        assert!(!sniff_is_reclass_xml(b"GIF89a"));
    }

    #[test]
    fn path_sniff_chooses_importer_by_content_not_extension() {
        // The headline of the gap: the importer is chosen by the file's BYTES, not
        // its name. An `.xml` holding JSON is NOT XML; a `.rcx` holding XML IS XML.
        let base = temp_settings_path();

        // `<?xml …` true.
        let xml = base.with_extension("rcx_xmlprolog");
        std::fs::write(&xml, b"<?xml version=\"1.0\"?>\n<ReClass>").unwrap();
        assert!(super::MainWindow::path_is_reclass_xml(&xml));

        // Leading-whitespace `<ReClass>` true.
        let ws = base.with_extension("rcx_wsreclass");
        std::fs::write(&ws, b"   <ReClass>\n").unwrap();
        assert!(super::MainWindow::path_is_reclass_xml(&ws));

        // An `.xml`-EXTENSION file carrying JSON bytes → NOT XML (falls through to
        // the native JSON load).
        let xml_ext_json = base.with_extension("xml");
        std::fs::write(&xml_ext_json, b"{\"json\": 1}").unwrap();
        assert!(!super::MainWindow::path_is_reclass_xml(&xml_ext_json));

        // A `.rcx`-EXTENSION file carrying XML bytes → XML (the importer chosen by
        // signature, not by name).
        let rcx_ext_xml = base.with_extension("rcx");
        std::fs::write(&rcx_ext_xml, b"<?xml version=\"1.0\"?>").unwrap();
        assert!(super::MainWindow::path_is_reclass_xml(&rcx_ext_xml));

        // A missing file → false (the C++ leaves isXml=false when the probe fails
        // to open).
        let missing = base.with_extension("does_not_exist");
        assert!(!super::MainWindow::path_is_reclass_xml(&missing));

        for p in [&xml, &ws, &xml_ext_json, &rcx_ext_xml] {
            let _ = std::fs::remove_file(p);
        }
    }

    // ── A2 GAP 4: recent-file age from mtime (the C++ buildGroups bucketing) ──

    #[test]
    fn age_days_computed_from_known_timestamp() {
        // `age_days_from_secs` is the per-recent-file age `recent_entries` now feeds
        // into the start-page buckets. Assert the day-delta and the resulting
        // bucket for known timestamps.
        use super::super::startpage::{age_days_from_secs, bucket_for, Bucket, RecentEntry};
        const DAY: u64 = 24 * 60 * 60;
        let now = 1_000 * DAY; // an arbitrary fixed "now" in whole days.

        // Same day → 0 (Today).
        assert_eq!(age_days_from_secs(now, now), 0);
        // 1 day ago → Yesterday.
        assert_eq!(age_days_from_secs(now, now - DAY), 1);
        // 3 days ago → This Week.
        assert_eq!(age_days_from_secs(now, now - 3 * DAY), 3);
        // 40 days ago → Older.
        assert_eq!(age_days_from_secs(now, now - 40 * DAY), 40);
        // A future mtime (clock skew) floors at 0.
        assert_eq!(age_days_from_secs(now, now + 5 * DAY), 0);

        // The computed age drives the bucket the start page files the row under.
        let entry_for = |age: i64| RecentEntry {
            path: "/p/x.rcx".into(),
            file_name: "x.rcx".into(),
            dir_path: "/p".into(),
            age_days: age,
            is_example: false,
        };
        assert_eq!(bucket_for(&entry_for(0)), Bucket::Today);
        assert_eq!(bucket_for(&entry_for(1)), Bucket::Yesterday);
        assert_eq!(bucket_for(&entry_for(3)), Bucket::ThisWeek);
        assert_eq!(bucket_for(&entry_for(15)), Bucket::ThisMonth);
        assert_eq!(bucket_for(&entry_for(40)), Bucket::Older);
    }
}
