//! `ThemeManager` — port of `src/themes/thememanager.{h,cpp}`.
//!
//! The C++ class is a `QObject` Meyers singleton with a
//! `themeChanged(const Theme&)` signal. The Rust port is a plain owned struct
//! plus an observer-callback registry (the single signal). It is NOT a
//! process-global `static`: the app owns it (a GPUI Global) so tests can build
//! isolated instances. Single-threaded / UI-thread-owned — no locking
//! (`thememanager` map §8).
//!
//! The persisted `"theme"` selection key is routed through a [`SettingsStore`]
//! abstraction (the shared app settings in the C++ `QSettings("Reclass",
//! "Reclass")`), injected so tests use an in-memory store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::defaults::DEFAULT_THEMES;
use super::model::Theme;

/// The single `"theme"` string key store — the shared-app-settings seam.
///
/// In the app build this is backed by the same store as `compactColumns`,
/// `treeLines`, etc. (the `QSettings("Reclass","Reclass")` replacement). The
/// theme module only ever reads/writes the one `"theme"` key.
pub trait SettingsStore {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
}

/// In-memory [`SettingsStore`] for tests (no real registry / INI).
#[derive(Default)]
pub struct MemSettings {
    map: HashMap<String, String>,
}

impl MemSettings {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SettingsStore for MemSettings {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
    fn set(&mut self, key: &str, value: &str) {
        self.map.insert(key.to_string(), value.to_string());
    }
}

/// Opaque subscription id for [`ThemeManager::unsubscribe`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct SubId(u64);

type Observer = Box<dyn FnMut(&Theme)>;

/// `class ThemeManager` (`thememanager.h:8-45`).
pub struct ThemeManager {
    builtin: Vec<Theme>,          // m_builtIn (possibly overridden in-place)
    builtin_defaults: Vec<Theme>, // m_builtInDefaults (pristine, for diffing)
    user: Vec<Theme>,             // m_user
    current_idx: usize,           // m_currentIdx
    previewing: bool,             // m_previewing
    saved_theme: Theme,           // m_savedTheme
    settings: Box<dyn SettingsStore>,
    builtin_dir: PathBuf,
    user_dir: PathBuf,
    observers: Vec<(SubId, Observer)>,
    next_sub_id: u64,
}

/// The static all-default fallback theme (≙ C++ `static const Theme empty`).
fn empty_theme() -> &'static Theme {
    use std::sync::OnceLock;
    static EMPTY: OnceLock<Theme> = OnceLock::new();
    EMPTY.get_or_init(Theme::default)
}

impl ThemeManager {
    /// Construct the manager (`thememanager.cpp:16-31`).
    ///
    /// `builtin_dir` is the directory holding the shipped `*.json` (tests point
    /// it at `_oracle/fixtures/themes/`; the app at `<exe>/themes`). `user_dir`
    /// holds user themes (tests use a `TempDir`). Loads built-ins then user
    /// themes, resolves the fallback selection (a name containing "VS2022",
    /// else the first built-in), reads the persisted `"theme"` key, and selects
    /// the matching theme (else index 0).
    pub fn new(settings: Box<dyn SettingsStore>, builtin_dir: PathBuf, user_dir: PathBuf) -> Self {
        let mut m = ThemeManager {
            builtin: Vec::new(),
            builtin_defaults: Vec::new(),
            user: Vec::new(),
            current_idx: 0,
            previewing: false,
            saved_theme: Theme::default(),
            settings,
            builtin_dir,
            user_dir,
            observers: Vec::new(),
            next_sub_id: 0,
        };

        m.load_builtin_themes();
        m.load_user_themes();

        // Fallback selection name: first built-in whose name contains "VS2022"
        // (case-insensitive), else the first built-in, else empty.
        let fallback = m
            .builtin
            .iter()
            .find(|t| t.name.to_lowercase().contains("vs2022"))
            .map(|t| t.name.clone())
            .or_else(|| m.builtin.first().map(|t| t.name.clone()))
            .unwrap_or_default();

        let saved = m.settings.get("theme").unwrap_or(fallback);

        m.current_idx = 0;
        for (i, t) in m.themes().iter().enumerate() {
            if t.name == saved {
                m.current_idx = i;
                break;
            }
        }

        m
    }

    /// Built-in followed by user themes (`thememanager.cpp:60-64`). Defines the
    /// index space used everywhere.
    pub fn themes(&self) -> Vec<Theme> {
        let mut all = self.builtin.clone();
        all.extend(self.user.iter().cloned());
        all
    }

    /// Number of built-in themes (`builtInCount()`).
    pub fn builtin_count(&self) -> usize {
        self.builtin.len()
    }

    /// `currentIndex()` (`thememanager.h:14`).
    pub fn current_index(&self) -> usize {
        self.current_idx
    }

    /// `current()` (`thememanager.cpp:66-76`). Falls back to `builtin[0]` then a
    /// static empty `Theme` rather than panicking on an empty list.
    pub fn current(&self) -> &Theme {
        if self.current_idx < self.builtin.len() {
            return &self.builtin[self.current_idx];
        }
        let user_idx = self.current_idx - self.builtin.len();
        if user_idx < self.user.len() {
            return &self.user[user_idx];
        }
        if !self.builtin.is_empty() {
            return &self.builtin[0];
        }
        empty_theme()
    }

    /// `setCurrent(index)` (`thememanager.cpp:78-85`). No-op if out of range;
    /// otherwise persists the name and emits (always, even if unchanged).
    pub fn set_current(&mut self, index: usize) {
        let all = self.themes();
        if index >= all.len() {
            return;
        }
        self.current_idx = index;
        self.settings.set("theme", &all[index].name);
        let t = self.current().clone();
        self.emit_theme_changed(t);
    }

    /// `addTheme(theme)` (`thememanager.cpp:87-90`). Appends a user theme and
    /// persists; does NOT change `current_idx` and does NOT emit.
    pub fn add_theme(&mut self, theme: Theme) {
        self.user.push(theme);
        self.save_user_themes();
    }

    /// `updateTheme(index, theme)` (`thememanager.cpp:92-107`). Commits any
    /// active preview, overwrites the slot, persists, emits.
    pub fn update_theme(&mut self, index: usize, theme: Theme) {
        self.previewing = false; // commit any active preview

        if index < self.builtin.len() {
            self.builtin[index] = theme;
            self.current_idx = index;
        } else {
            let ui = index - self.builtin.len();
            if ui < self.user.len() {
                self.user[ui] = theme;
            }
            // out-of-range user index: silent drop (still saves+persists+emits)
        }
        self.save_user_themes();
        let name = self.current().name.clone();
        self.settings.set("theme", &name);
        let t = self.current().clone();
        self.emit_theme_changed(t);
    }

    /// `removeTheme(index)` (`thememanager.cpp:109-121`). Built-ins / out-of-
    /// range user indices are no-ops. Does NOT update the persisted `"theme"`.
    pub fn remove_theme(&mut self, index: usize) {
        if index < self.builtin.len() {
            return; // built-ins can't be removed
        }
        let ui = index - self.builtin.len();
        if ui >= self.user.len() {
            return; // out-of-range user
        }
        self.user.remove(ui);
        if self.current_idx == index {
            self.current_idx = 0;
            let t = self.current().clone();
            self.emit_theme_changed(t);
        } else if self.current_idx > index {
            self.current_idx -= 1; // no emit
        }
        // else current_idx < index: unchanged, no emit
        self.save_user_themes();
    }

    // ── Loading ──

    /// `builtInDir()` (`thememanager.cpp:35-42`) — the directory holding the
    /// shipped JSON next to the executable (macOS: `../Resources/themes`).
    /// Both arms compile on every OS; only the active one is used.
    pub fn default_builtin_dir() -> PathBuf {
        let dir = std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        #[cfg(target_os = "macos")]
        {
            dir.join("../Resources/themes")
        }
        #[cfg(not(target_os = "macos"))]
        {
            dir.join("themes")
        }
    }

    /// `userDir()` (`thememanager.cpp:125-130`) — `QStandardPaths::
    /// AppDataLocation` for org/app "Reclass"/"Reclass", `+ "/themes"`, created
    /// if missing.
    pub fn default_user_dir() -> PathBuf {
        let base = directories::ProjectDirs::from("", "Reclass", "Reclass")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("Reclass"));
        let dir = base.join("themes");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// `loadBuiltInThemes()` (`thememanager.cpp:44-56`).
    ///
    /// Reads every `*.json` in `builtin_dir`, name-sorted ascending, parsing
    /// each object through [`Theme::from_json`]. If the dir is missing or empty
    /// it self-heals from the embedded [`DEFAULT_THEMES`] (the headless test
    /// path). Unreadable / unparseable / non-object files are skipped.
    fn load_builtin_themes(&mut self) {
        self.builtin.clear();

        let mut loaded_from_disk = false;
        if self.builtin_dir.exists() {
            let mut names: Vec<String> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&self.builtin_dir) {
                for entry in rd.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.to_lowercase().ends_with(".json") {
                        names.push(name);
                    }
                }
            }
            names.sort(); // QDir::Name (byte-wise ascending; all lowercase ASCII)
            for name in names {
                let path = self.builtin_dir.join(&name);
                let data = match std::fs::read(&path) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("theme: skip unreadable {}: {e}", path.display());
                        continue;
                    }
                };
                let v: serde_json::Value = match serde_json::from_slice(&data) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("theme: skip unparseable {}: {e}", path.display());
                        continue;
                    }
                };
                if v.is_object() {
                    self.builtin.push(Theme::from_json(&v));
                    loaded_from_disk = true;
                }
            }
        }

        if !loaded_from_disk {
            // Self-heal from the embedded defaults (already filename-sorted).
            for (_name, json) in DEFAULT_THEMES.iter() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                    if v.is_object() {
                        self.builtin.push(Theme::from_json(&v));
                    }
                }
            }
        }

        self.builtin_defaults = self.builtin.clone();
    }

    /// `loadUserThemes()` (`thememanager.cpp:132-154`).
    ///
    /// Parses every `*.json` in `user_dir`; a theme whose display `name`
    /// matches a built-in replaces that built-in slot in place (the
    /// override-by-name path); otherwise it's appended to `user`.
    pub fn load_user_themes(&mut self) {
        self.user.clear();
        let _ = std::fs::create_dir_all(&self.user_dir);

        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.user_dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.to_lowercase().ends_with(".json") {
                    names.push(name);
                }
            }
        }
        names.sort();

        for name in names {
            let path = self.user_dir.join(&name);
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let v: serde_json::Value = match serde_json::from_slice(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if !v.is_object() {
                continue;
            }
            let t = Theme::from_json(&v);

            if let Some(i) = self.builtin.iter().position(|b| b.name == t.name) {
                self.builtin[i] = t; // override; builtin_defaults untouched
            } else {
                self.user.push(t);
            }
        }
    }

    /// `saveUserThemes()` (`thememanager.cpp:156-179`).
    ///
    /// Wipes every `*.json` in `user_dir`, then writes any built-in whose JSON
    /// differs from its pristine default, then all user themes. Filenames are
    /// `name.to_lowercase().replace(' ', '_') + ".json"`. Per-file write
    /// failures are logged and skipped.
    pub fn save_user_themes(&self) {
        // 1. wipe existing *.json
        if let Ok(rd) = std::fs::read_dir(&self.user_dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.to_lowercase().ends_with(".json") {
                    let _ = std::fs::remove_file(self.user_dir.join(name));
                }
            }
        }

        // 2. save modified built-ins (diff against pristine defaults)
        let n = self.builtin.len().min(self.builtin_defaults.len());
        for i in 0..n {
            if self.builtin[i].to_json() != self.builtin_defaults[i].to_json() {
                self.write_theme_file(&self.builtin[i]);
            }
        }

        // 3. save all user themes
        for u in &self.user {
            self.write_theme_file(u);
        }
    }

    fn write_theme_file(&self, t: &Theme) {
        let fname = theme_filename(&t.name);
        let path = self.user_dir.join(&fname);
        let json = serde_json::to_string_pretty(&t.to_json()).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!("theme: failed to write {}: {e}", path.display());
        }
    }

    /// `themeFilePath(index)` (`thememanager.cpp:181-197`).
    ///
    /// Returns the on-disk path the editor reports for the theme: a user-dir
    /// override copy for modified built-ins, the built-in source path
    /// otherwise, the user-dir path for user themes. `None` (≙ empty `QString`)
    /// only for an out-of-range user index.
    pub fn theme_file_path(&self, index: usize) -> Option<PathBuf> {
        if index < self.builtin.len() {
            if index < self.builtin_defaults.len()
                && self.builtin[index].to_json() != self.builtin_defaults[index].to_json()
            {
                return Some(
                    self.user_dir
                        .join(theme_filename(&self.builtin[index].name)),
                );
            }
            return Some(
                self.builtin_dir
                    .join(theme_filename(&self.builtin[index].name)),
            );
        }
        let ui = index - self.builtin.len();
        if ui >= self.user.len() {
            return None;
        }
        Some(self.user_dir.join(theme_filename(&self.user[ui].name)))
    }

    // ── Preview ──

    /// `previewTheme(theme)` (`thememanager.cpp:199-205`). Broadcasts the
    /// transient theme without changing the current selection. Re-entrant:
    /// `saved_theme` is captured only on the first call.
    pub fn preview_theme(&mut self, theme: Theme) {
        if !self.previewing {
            self.saved_theme = self.current().clone();
            self.previewing = true;
        }
        self.emit_theme_changed(theme);
    }

    /// `revertPreview()` (`thememanager.cpp:207-212`). Re-emits the pre-preview
    /// theme; no-op if not previewing.
    pub fn revert_preview(&mut self) {
        if self.previewing {
            self.previewing = false;
            let t = self.saved_theme.clone();
            self.emit_theme_changed(t);
        }
    }

    // ── Observers (the themeChanged signal) ──

    /// Subscribe to `themeChanged`. Returns a [`SubId`] for [`Self::unsubscribe`].
    pub fn subscribe(&mut self, cb: Box<dyn FnMut(&Theme)>) -> SubId {
        let id = SubId(self.next_sub_id);
        self.next_sub_id += 1;
        self.observers.push((id, cb));
        id
    }

    /// Unsubscribe a previously-registered observer.
    pub fn unsubscribe(&mut self, id: SubId) {
        self.observers.retain(|(sid, _)| *sid != id);
    }

    /// Emit `themeChanged(theme)` to all observers synchronously (Qt direct
    /// connection).
    fn emit_theme_changed(&mut self, t: Theme) {
        for (_, cb) in self.observers.iter_mut() {
            cb(&t);
        }
    }
}

/// Filename derivation: `name.to_lowercase().replace(' ', '_') + ".json"`
/// (`thememanager.cpp:165` etc.). Replaces only ASCII space U+0020.
fn theme_filename(name: &str) -> String {
    format!("{}.json", name.to_lowercase().replace(' ', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::color::Color;
    use std::cell::Cell;
    use std::rc::Rc;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("_oracle/fixtures/themes")
    }

    /// Build a manager over the oracle fixtures + a fresh temp user dir.
    fn mk_manager() -> (ThemeManager, PathBuf) {
        let user_dir = std::env::temp_dir().join(format!(
            "rcx-theme-test-{}-{}",
            std::process::id(),
            next_unique()
        ));
        let _ = std::fs::create_dir_all(&user_dir);
        let m = ThemeManager::new(
            Box::new(MemSettings::new()),
            fixtures_dir(),
            user_dir.clone(),
        );
        (m, user_dir)
    }

    fn next_unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static C: AtomicU64 = AtomicU64::new(0);
        C.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn theme_filename_derivation() {
        assert_eq!(theme_filename("VS2022 Dark"), "vs2022_dark.json");
        assert_eq!(theme_filename("Light"), "light.json");
        assert_eq!(theme_filename("Reclass Dark"), "reclass_dark.json");
    }

    #[test]
    fn builtin_themes() {
        // C++ test_theme.cpp:13-40 (builtInThemes).
        let (m, _u) = mk_manager();
        let all = m.themes();
        assert!(all.len() >= 2);

        let dark = all.iter().find(|t| t.name == "Reclass Dark").unwrap();
        assert_eq!(dark.name, "Reclass Dark");
        assert!(dark.background.is_some());
        assert!(dark.text.is_some());
        assert!(dark.syntax_keyword.is_some());
        assert!(dark.marker_error.is_some());

        let warm = all.iter().find(|t| t.name == "Warm").unwrap();
        assert_eq!(warm.name, "Warm");
        assert!(warm.background.is_some());
        assert!(warm.text.is_some());
        assert_eq!(warm.background, Color::parse("#212121"));
        // ORACLE FIX: shipped JSON has selection "#3a2a3a" (NOT the stale test's
        // "#21213A"). The oracle records test_theme FAIL at line 37.
        assert_eq!(warm.selection, Color::parse("#3a2a3a"));
        assert_eq!(warm.syntax_keyword, Color::parse("#AA9565"));
        assert_eq!(warm.syntax_type, Color::parse("#6B959F"));
    }

    #[test]
    fn json_round_trip() {
        // C++ test_theme.cpp:42-60 (jsonRoundTrip).
        let (m, _u) = mk_manager();
        let orig = m.themes()[0].clone();
        let loaded = Theme::from_json(&orig.to_json());

        assert_eq!(loaded.name, orig.name);
        assert_eq!(loaded.background, orig.background);
        assert_eq!(loaded.text, orig.text);
        assert_eq!(loaded.selection, orig.selection);
        assert_eq!(loaded.syntax_keyword, orig.syntax_keyword);
        assert_eq!(loaded.syntax_number, orig.syntax_number);
        assert_eq!(loaded.syntax_string, orig.syntax_string);
        assert_eq!(loaded.syntax_comment, orig.syntax_comment);
        assert_eq!(loaded.syntax_type, orig.syntax_type);
        assert_eq!(loaded.marker_ptr, orig.marker_ptr);
        assert_eq!(loaded.marker_error, orig.marker_error);
        assert_eq!(loaded.ind_hover_span, orig.ind_hover_span);
    }

    #[test]
    fn json_round_trip_warm() {
        // C++ test_theme.cpp:62-76 (jsonRoundTripWarm).
        let (m, _u) = mk_manager();
        let all = m.themes();
        let orig = all.iter().find(|t| t.name == "Warm").unwrap().clone();
        let loaded = Theme::from_json(&orig.to_json());

        assert_eq!(loaded.name, orig.name);
        assert_eq!(loaded.background, orig.background);
        assert_eq!(loaded.selection, orig.selection);
        assert_eq!(loaded.syntax_keyword, orig.syntax_keyword);
    }

    #[test]
    fn manager_has_builtins() {
        // C++ test_theme.cpp:92-105 (themeManagerHasBuiltIns).
        let (m, _u) = mk_manager();
        let all = m.themes();
        assert!(all.len() >= 3);
        // ORACLE FIX: filename-sort-first is "Long Night" (NOT the stale test's
        // "Reclass Dark"). The oracle records test_theme FAIL at line 96.
        assert_eq!(all[0].name, "Long Night");

        let has_vs = all.iter().any(|t| t.name == "VS2022 Dark");
        let has_warm = all.iter().any(|t| t.name == "Warm");
        assert!(has_vs);
        assert!(has_warm);
    }

    #[test]
    fn builtin_display_order() {
        // Filename-sorted display-name order (thememanager.cpp:48 / spec §3.8).
        let (m, _u) = mk_manager();
        let names: Vec<String> = m.builtin.iter().map(|t| t.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "Long Night",
                "Mid",
                "Modern",
                "Phosphor",
                "Reclass Dark",
                "Light",
                "VS2022 Dark",
                "Warm",
            ]
        );
    }

    #[test]
    fn manager_switch_emits_once() {
        // C++ test_theme.cpp:107-121 (themeManagerSwitch).
        let (mut m, _u) = mk_manager();
        let count = Rc::new(Cell::new(0usize));
        let c2 = count.clone();
        m.subscribe(Box::new(move |_t| c2.set(c2.get() + 1)));

        let start = m.current_index();
        let target = if start == 0 { 1 } else { 0 };
        m.set_current(target);

        assert_eq!(count.get(), 1);
        assert_eq!(m.current_index(), target);
        assert_eq!(m.current().name, m.themes()[target].name);

        m.set_current(start);
    }

    #[test]
    fn set_current_out_of_range_is_noop() {
        let (mut m, _u) = mk_manager();
        let count = Rc::new(Cell::new(0usize));
        let c2 = count.clone();
        m.subscribe(Box::new(move |_t| c2.set(c2.get() + 1)));
        let before = m.current_index();
        m.set_current(9999);
        assert_eq!(count.get(), 0);
        assert_eq!(m.current_index(), before);
    }

    #[test]
    fn manager_crud() {
        // C++ test_theme.cpp:123-145 (themeManagerCRUD).
        let (mut m, _u) = mk_manager();
        let initial = m.themes().len();

        // Add
        let mut custom = m.themes()[0].clone();
        custom.name = "Test Custom".to_string();
        custom.background = Color::parse("#ff0000");
        m.add_theme(custom.clone());
        assert_eq!(m.themes().len(), initial + 1);
        assert_eq!(m.themes().last().unwrap().name, "Test Custom");

        // Update
        let idx = m.themes().len() - 1;
        let mut updated = custom;
        updated.background = Color::parse("#00ff00");
        m.update_theme(idx, updated);
        assert_eq!(m.themes()[idx].background, Color::parse("#00ff00"));

        // Remove
        m.remove_theme(idx);
        assert_eq!(m.themes().len(), initial);
    }

    #[test]
    fn fallback_selection_is_vs2022() {
        // No persisted "theme" → fallback to the VS2022-containing built-in.
        let (m, _u) = mk_manager();
        assert_eq!(m.current().name, "VS2022 Dark");
    }

    #[test]
    fn persisted_theme_selected() {
        let user_dir = std::env::temp_dir().join(format!("rcx-theme-persel-{}", next_unique()));
        let mut settings = MemSettings::new();
        settings.set("theme", "Warm");
        let m = ThemeManager::new(Box::new(settings), fixtures_dir(), user_dir);
        assert_eq!(m.current().name, "Warm");
    }

    #[test]
    fn preview_revert() {
        let (mut m, _u) = mk_manager();
        let emitted: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let e2 = emitted.clone();
        m.subscribe(Box::new(move |t| e2.borrow_mut().push(t.name.clone())));

        let original = m.current().name.clone();

        let mut ta = Theme::default();
        ta.name = "Preview A".to_string();
        let mut tb = Theme::default();
        tb.name = "Preview B".to_string();

        m.preview_theme(ta.clone()); // emit 1: A, saved = original
        m.preview_theme(tb.clone()); // emit 2: B, saved unchanged
        m.revert_preview(); // emit 3: original
        m.revert_preview(); // no-op

        let got = emitted.borrow().clone();
        assert_eq!(got, vec!["Preview A", "Preview B", &original]);
    }

    #[test]
    fn update_theme_commits_preview() {
        let (mut m, _u) = mk_manager();
        let count = Rc::new(Cell::new(0usize));
        let c2 = count.clone();
        m.subscribe(Box::new(move |_t| c2.set(c2.get() + 1)));

        let mut tx = Theme::default();
        tx.name = "X".to_string();
        m.preview_theme(tx); // emit 1

        let mut updated = m.themes()[0].clone();
        updated.background = Color::parse("#00ff00");
        m.update_theme(0, updated); // emit 2, previewing -> false

        let after = count.get();
        m.revert_preview(); // no-op (no extra emit)
        assert_eq!(count.get(), after);
    }

    #[test]
    fn user_override_by_name() {
        let (mut m, user_dir) = mk_manager();
        let warm_idx = m.themes().iter().position(|t| t.name == "Warm").unwrap();
        let len_before = m.themes().len();

        // Write a user JSON named "Warm" with a changed background.
        let json = r##"{ "name": "Warm", "background": "#010203" }"##;
        std::fs::write(user_dir.join("warm.json"), json).unwrap();

        m.load_user_themes();

        // Replaced the built-in slot, not appended.
        assert_eq!(m.themes().len(), len_before);
        assert_eq!(m.themes()[warm_idx].name, "Warm");
        assert_eq!(m.themes()[warm_idx].background, Color::parse("#010203"));

        // theme_file_path now points into user_dir (built-in differs from default).
        let p = m.theme_file_path(warm_idx).unwrap();
        assert_eq!(p, user_dir.join("warm.json"));
    }

    #[test]
    fn save_then_reload_user() {
        let (mut m, user_dir) = mk_manager();
        let mut custom = m.themes()[0].clone();
        custom.name = "Test Custom".to_string();
        custom.background = Color::parse("#ff0000");
        m.add_theme(custom); // triggers save_user_themes

        assert!(user_dir.join("test_custom.json").exists());

        // New manager over the same dirs reloads it as a user theme.
        let m2 = ThemeManager::new(
            Box::new(MemSettings::new()),
            fixtures_dir(),
            user_dir.clone(),
        );
        let reloaded = m2
            .themes()
            .iter()
            .find(|t| t.name == "Test Custom")
            .cloned()
            .unwrap();
        assert_eq!(reloaded.background, Color::parse("#ff0000"));
    }

    #[test]
    fn theme_file_path_builtin_unmodified_points_at_builtin_dir() {
        let (m, _u) = mk_manager();
        // builtin[0] == "Long Night", unmodified → builtin_dir path.
        let p = m.theme_file_path(0).unwrap();
        assert_eq!(p, fixtures_dir().join("long_night.json"));
    }

    #[test]
    fn theme_file_path_out_of_range_user_is_none() {
        let (m, _u) = mk_manager();
        let oob = m.themes().len() + 5;
        assert_eq!(m.theme_file_path(oob), None);
    }

    #[test]
    fn remove_theme_builtin_is_noop() {
        let (mut m, _u) = mk_manager();
        let before = m.themes().len();
        m.remove_theme(0); // built-in
        assert_eq!(m.themes().len(), before);
    }
}
