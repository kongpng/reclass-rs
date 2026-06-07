//! The disk-backed app settings store (the C++ `QSettings` replacement) plus the
//! shared `QSettings` key-name constants. Extracted from window.rs; `use super::*`
//! inherits the parent's imports — notably the private
//! `use crate::theme::{SettingsStore, ThemeManager};` alias, so the
//! `impl SettingsStore for DiskSettings` trait is in scope.

use super::*;

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
pub(crate) mod settings_keys {
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
