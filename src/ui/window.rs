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
        ShortcutsAction
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
    /// The bottom-dock memory [`ScannerPanel`] handle (the C++ summon-on-demand
    /// scanner). Held so the window can feed it the active document's provider —
    /// previously dropped (`..` in the `LayoutHandles` destructure), so the
    /// scanner could never scan.
    scanner: Entity<super::scannerpanel::ScannerPanel>,
    /// Live subscription to an open Tools ▸ Options dialog — kept so its
    /// Apply/Cancel events fire while shown (mirrors [`goto_sub`](Self::goto_sub)).
    options_sub: Option<Subscription>,
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

        // ── Wire the modules dock row activation (double-click → set the active
        // document's base address to the module base). ──
        cx.subscribe_in(
            &modules,
            window,
            |this, _md, ev: &super::modulespanel::ModuleAction, window, cx| {
                if let super::modulespanel::ModuleAction::Activate { base, .. } = ev {
                    this.navigate_active_editor_to_address(*base, window, cx);
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
        })
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
            scanner,
            options_sub: None,
            auto_start_mcp,
            brace_wrap,
            generator_asserts,
            refresh_ms,
        };

        // Observe the initial editor(s) so a row selection re-renders the window
        // (and thus refreshes the status bar; see [`Self::observe_editors`]).
        win.observe_editors(cx);
        // Push the active font family into the Font submenu ✓, mark the active
        // theme, and rebuild the dynamic menus (Recent Files / Data Source / MCP
        // label) on first paint.
        win.sync_font_menu_checked(cx);
        win.sync_theme_menu_checked(cx);
        win.rebuild_menus(cx);
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
            "source.kernel" | "source.remote" | "source.windbg" | "source.rcnet" => {
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

            // ── View: font family (the C++ exclusive Consolas / JetBrains Mono
            // picker persisted to settings("font")). ──
            "view.font.consolas" => self.set_editor_font("Consolas", window, cx),
            "view.font.jetbrains" => self.set_editor_font("JetBrains Mono", window, cx),

            // ── View: actions ──
            "view.refresh" => self.refresh_active_editor(cx),
            "view.goto_address" => self.open_goto_address(window, cx),
            "view.command_palette" => self.open_command_palette(&OpenCommandPalette, window, cx),
            "view.split" => self.notify(
                "Split Editor is a known stub in this port (single pane only).",
                window,
                cx,
            ),
            "view.unsplit" => self.notify(
                "Unsplit Editor is a known stub in this port (single pane only).",
                window,
                cx,
            ),
            "view.presentation" => self.toggle_presentation(cx),
            "view.theme_edit" => self.notify(
                "Theme editing is available in Tools ▸ Options (Appearance).",
                window,
                cx,
            ),

            // ── Tools ──
            "tools.rtti" => self.open_rtti_browser(window, cx),
            "tools.type_aliases" => self.notify(
                "Type Aliases: the alias editor dialog is not available in this port yet.",
                window,
                cx,
            ),
            "tools.mcp" => self.toggle_mcp(window, cx),
            "tools.options" => self.open_options_dialog(window, cx),
            "tools.profiler" => self.notify(
                "Performance Profiler is not available in this port yet.",
                window,
                cx,
            ),

            // ── Plugins ──
            "plugins.manage" => self.notify(
                "Plugin Manager: native plugins are not loaded in this port.",
                window,
                cx,
            ),

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
        self.observe_editors(cx);
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
                    ed.controller_mut().document_mut().load_data_file(&path);
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
        let model = ProcessPickerModel::from_registry(&crate::provider::ProviderRegistry::new());
        let picker = cx.new(|cx| ProcessPicker::new(model, window, cx));
        self.goto_sub = Some(cx.subscribe_in(
            &picker,
            window,
            |this, _p, ev: &ProcessPickEvent, window, cx| match ev {
                ProcessPickEvent::Attach { name, pid, .. } => {
                    window.close_dialog(cx);
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

    /// File ▸ Data Source ▸ {Kernel / Remote / WinDbg / ReClass.NET} — these live
    /// providers have no factory on this platform. The C++ shows a blocking
    /// warning when a source can't attach; mirror that with the themed modal
    /// message box (not a transient toast).
    fn report_unavailable_source(
        &mut self,
        cmd: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = match cmd {
            "source.kernel" => "Kernel Memory",
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
        let spec = super::messagebox::confirm(
            "Add Bookmark",
            &format!(
                "Bookmark the current address ({default_formula})? It will be added with an \
                 auto-generated name; rename it in the Bookmarks dock."
            ),
            "Add bookmark",
            false,
        );
        let editor2 = editor.clone();
        let this = cx.entity().downgrade();
        super::messagebox::open_confirm(
            spec,
            move |window, app| {
                let _ = this.update(app, |me, cx| {
                    let name = me.next_bookmark_name(&editor2, cx);
                    editor2.update(cx, |ed, _cx| {
                        ed.controller_mut().add_bookmark(&name, &default_formula);
                    });
                    me.after_bookmark_added(&name, &default_formula, window, cx);
                });
            },
            window,
            cx,
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
            let tree = &mut ed.controller_mut().document_mut().tree;
            tree.base_address = addr;
            ed.apply_document(cx);
        });
        self.rebuild_workspace(cx);
        self.notify(format!("Jumped to 0x{addr:X}"), window, cx);
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

    /// Tools ▸ RTTI Browser (Ctrl+Shift+R) — the C++ opens the vtable/RTTI browser
    /// for the selected pointer field. No RTTI walker is wired in this port, so
    /// report the requirement (a selected pointer + a live provider) clearly.
    fn open_rtti_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notify(
            "RTTI Browser: select a pointer/vtable field with a live provider attached.",
            window,
            cx,
        );
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
        let current = OptionsResult {
            theme_index,
            font_name: self.editor_font.clone(),
            menu_bar_title_case: true,
            show_icon: false,
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
                 GitHub: github.com/reclassnet/reclass",
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
        let recent: Vec<RecentMenuEntry> = self
            .existing_recent_files()
            .into_iter()
            .map(|(i, p)| RecentMenuEntry {
                label: p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(file)")
                    .to_string(),
                command: format!("file.recent.{i}"),
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

    /// File ▸ Exit — if any open document is modified, show the unsaved-changes
    /// guard before quitting (the C++ `closeEvent`); otherwise quit immediately.
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.any_document_modified(cx) {
            let spec = super::messagebox::confirm(
                "Unsaved changes",
                "One or more documents have unsaved changes. Quit without saving?",
                "Quit anyway",
                true,
            );
            super::messagebox::open_confirm(spec, |_window, app| app.quit(), window, cx);
        } else {
            cx.quit();
        }
    }

    /// Whether any open editor's document is modified (the C++ scans every tab's
    /// `doc.modified`).
    fn any_document_modified(&self, cx: &Context<Self>) -> bool {
        self.document_area
            .read(cx)
            .tabs()
            .iter()
            .any(|t| t.editor.read(cx).controller().document().modified)
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

    /// File ▸ Close Project (Ctrl+W) — close the active document tab. If the
    /// active document is modified, show the unsaved-changes guard first (the C++
    /// `closeFile` → unsaved prompt; act on the result). The document area never
    /// leaves a blank window (it re-seeds a fresh tab when the last closes).
    fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let modified = self
            .document_area
            .read(cx)
            .active_editor()
            .map(|ed| ed.read(cx).controller().document().modified)
            .unwrap_or(false);
        if modified {
            let spec = super::messagebox::confirm(
                "Unsaved changes",
                "This document has unsaved changes. Close it without saving?",
                "Close without saving",
                true,
            );
            let this = cx.entity().downgrade();
            super::messagebox::open_confirm(
                spec,
                move |window, app| {
                    let _ = this.update(app, |me, cx| me.do_close_active(window, cx));
                },
                window,
                cx,
            );
        } else {
            self.do_close_active(window, cx);
        }
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
        editor.update(cx, |ed, cx| {
            ed.set_compact_columns(o.compact_columns, cx);
            ed.set_tree_lines(o.tree_lines, cx);
            ed.set_relative_offsets(o.relative_offsets, cx);
            ed.set_type_hints(o.type_hints, cx);
            ed.set_show_comments(o.show_comments, cx);
            ed.set_hover_effects(o.hover_effects, cx);
            ed.set_minimap(o.minimap, cx);
            // Generator brace-wrap + the persisted refresh interval are
            // controller-level (the C++ pushes both into every controller).
            ed.controller_mut().set_brace_wrap(brace_wrap);
            ed.controller_mut().set_refresh_interval(refresh_ms);
        });
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

    /// The recent-files entries for the start page, built from the session
    /// recent-files list (the C++ `recentFiles` QSettings, surfaced by the start
    /// page). Most-recent-first; the age is left at 0 (no persisted timestamps in
    /// this port).
    fn recent_entries(&self) -> Vec<RecentEntry> {
        // Skip entries whose file no longer exists (the C++ start-page filters
        // the same way the Recent Files menu does; main.cpp:8789).
        self.existing_recent_files()
            .into_iter()
            .map(|(_, p)| RecentEntry {
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
                age_days: 0,
                is_example: false,
            })
            .collect()
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

        // Push the loaded document into the active editor (the C++ rebinds the
        // active tab's controller). The editor recomposes + picks a view root.
        let Some(active_id) = self.active_doc_id(cx) else {
            return false;
        };
        let mut source = super::state::DataSource::none();
        if let Some(editor) = self.document_area.read(cx).active_editor().cloned() {
            editor.update(cx, |ed, cx| ed.set_document(doc, cx));
            // Realign the fresh editor to the window's (persisted) view options —
            // a new doc otherwise inherits controller/view defaults that disagree.
            self.apply_view_opts_to_editor(&editor, cx);
            // Derive the source icon from the controller's NOW-attached provider
            // (post-ingest), so a .rcx's saved File source shows the File icon.
            source = Self::source_for_controller(editor.read(cx).controller());
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
        seed_root_doc, settings_keys, DiskSettings, ExportKind, ImportKind, RootKind, ViewOpt,
        ViewOptions,
    };
    use crate::theme::SettingsStore;

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
}
