//! `PluginManager` — owns the loaded plugins and auto-registers their `Provider`
//! contributions into the existing [`ProviderRegistry`] (design §6 Phase 1; the
//! C++ `PluginManager`, cpp_reference §2).
//!
//! C++ parity: on load, a provider plugin is auto-registered under
//! `identifier = Name().toLower().replace(" ","")` (cpp_reference §2). We do the
//! same, but route the identifier through the single
//! [`derive_identifier`](crate::plugin::manifest::derive_identifier) helper
//! (design §7.A [fix]) and keep the [`ProviderSpec`] so an identifier can be
//! mapped back to a real [`create_provider`](ProviderSpec::create_provider) at
//! attach time (the C++ `selectSource`/`attachViaPlugin` path, cpp_reference §5).
//!
//! Phase 1 loads only the in-tree built-ins ([`with_builtins`](PluginManager::with_builtins));
//! the native/`abi_stable` and ReClass.NET loaders (design §6 Phase 3/4) drop in
//! behind the `plugins` feature without changing this registration flow.

use std::collections::HashMap;

use serde_json::Value;

use crate::plugin::builtins;
use crate::plugin::contract::{CommandResult, Contribution, DialogResult, Plugin};
use crate::plugin::host::PluginHost;
use crate::plugin::manifest::{detected_label, LoadType, Permission, PluginKind};
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
use crate::plugin::view::{UiEvent, ViewTree};
use crate::provider::{ProviderInfo, ProviderRegistry};

/// The per-plugin persistence seam (design §7.A [fix] — C++ persists nothing;
/// the loaded set is just "DLLs in the folder", forgotten each run, cpp_reference
/// §2, §10.3). Mirrors the theme [`SettingsStore`](crate::theme::SettingsStore)
/// shape: a small key/value-ish surface the app backs with the shared
/// `QSettings("Reclass","Reclass")` replacement, injected so tests use an
/// in-memory store. The manager holds an **optional** boxed implementor — `None`
/// is the pre-Phase-6 behavior (nothing remembered).
///
/// Two pieces of state are remembered: a plugin's **enabled** flag (keyed on its
/// derived identifier — design §7.A [fix], §H "enable/disable persisted") and the
/// **load-from-path** set (design §7.A [fix] "persist the load-from-path set").
pub trait PluginPersistence {
    /// The persisted enabled flag for `identifier`, or `None` if never stored
    /// (the manager then falls back to the plugin's [`LoadType`] default).
    fn get_enabled(&self, identifier: &str) -> Option<bool>;
    /// Persist `identifier`'s enabled flag.
    fn set_enabled(&mut self, identifier: &str, enabled: bool);
    /// The persisted load-from-path set (absolute paths the user explicitly
    /// loaded, design §6 Phase 6 "keep C++'s load-from-path").
    fn get_paths(&self) -> Vec<String>;
    /// Remember a load-from-path entry (idempotent — no duplicates).
    fn add_path(&mut self, path: &str);
    /// Forget a load-from-path entry (e.g. after a safe-unload).
    fn remove_path(&mut self, path: &str);
}

/// An always-compiled in-memory [`PluginPersistence`] for tests + the conformance
/// suite (no real `QSettings`/INI). Mirrors the theme module's `MemSettings`.
#[derive(Default)]
pub struct MemPluginPersistence {
    enabled: HashMap<String, bool>,
    paths: Vec<String>,
}

impl MemPluginPersistence {
    pub fn new() -> Self {
        MemPluginPersistence::default()
    }
}

impl PluginPersistence for MemPluginPersistence {
    fn get_enabled(&self, identifier: &str) -> Option<bool> {
        self.enabled.get(identifier).copied()
    }
    fn set_enabled(&mut self, identifier: &str, enabled: bool) {
        self.enabled.insert(identifier.to_string(), enabled);
    }
    fn get_paths(&self) -> Vec<String> {
        self.paths.clone()
    }
    fn add_path(&mut self, path: &str) {
        if !self.paths.iter().any(|p| p == path) {
            self.paths.push(path.to_string());
        }
    }
    fn remove_path(&mut self, path: &str) {
        self.paths.retain(|p| p != path);
    }
}

/// One Manage-Plugins row — the single model the dialog renders for **every**
/// plugin: a built-in, a native `abi_stable` plugin, or an auto-detected
/// ReClass.NET bridge (design §6 Phase 6; the C++ `showPluginsDialog` list,
/// cpp_reference §6, plus the [fix] columns C++ lacks — enabled state + declared
/// permissions + a detected-kind label).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    /// The derived routing identifier (`Name().toLower().replace(" ","")`).
    pub identifier: String,
    pub kind: PluginKind,
    /// The short human label for the auto-detected kind (design §4 detection),
    /// distinguishing `reclassnet-native` from `reclassnet-managed` by how the
    /// plugin was loaded — see [`detected_label`].
    pub detected_label: &'static str,
    /// The declared permissions (design §5/§6 disclosure).
    pub permissions: Vec<Permission>,
    /// Whether this plugin is currently enabled (provider plugins reflect the
    /// registry; non-provider plugins reflect their own enabled flag).
    pub enabled: bool,
    pub is_builtin: bool,
    /// The backing artifact filename (empty for built-ins).
    pub dll_file_name: String,
    pub load: LoadType,
}

/// How a loaded plugin reached the manager — needed so a ReClass.NET plugin can
/// show `reclassnet-native` vs `reclassnet-managed` (design §4) and so a
/// non-provider plugin can carry its own enabled flag (the registry only tracks
/// provider plugins). One per entry in `plugins`, index-parallel.
struct PluginEntry {
    /// Whether this ReClass.NET bridge came through the managed CLR path (drives
    /// [`detected_label`]); always `false` for non-ReClass.NET kinds.
    managed: bool,
    /// Enabled state for **non-provider** plugins (provider plugins are tracked by
    /// the registry's `enabled` flag instead, the single source of truth there).
    non_provider_enabled: bool,
    /// True iff this plugin contributed a `Provider` (so we know which enabled
    /// flag is authoritative).
    has_provider: bool,
}

/// Owns the loaded plugins + the provider registry they populate (the C++
/// `MainWindow::m_pluginManager`, cpp_reference §2). One per application.
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
    /// Per-plugin bookkeeping, index-parallel to `plugins`.
    entries: Vec<PluginEntry>,
    registry: ProviderRegistry,
    /// identifier → index of the plugin in `plugins` (so we can fetch its
    /// `ProviderSpec` to create a provider at attach time). Built during
    /// registration; the C++ re-finds the plugin by recomputed identifier.
    provider_index: HashMap<String, usize>,
    /// EVERY plugin's derived identifier → index (provider or not), so the
    /// Manage-Plugins dialog can find any plugin by id (design §6 Phase 6 — the
    /// list is "all plugins", not just providers). `provider_index` is the subset
    /// that also registered a provider.
    plugin_index: HashMap<String, usize>,
    /// command id → owning plugin index (design §6 Phase 2 routing — so a
    /// dispatched `Command` reaches the plugin that contributed it).
    command_index: HashMap<String, usize>,
    /// view id (`Panel`/`Dialog`/`StatusItem`) → owning plugin index, so a
    /// routed [`UiEvent`]/[`DialogResult`] reaches the contributing plugin.
    view_index: HashMap<String, usize>,
    /// Optional per-plugin persistence (design §7.A [fix]). `None` = the
    /// pre-Phase-6 behavior (nothing remembered across restarts).
    persistence: Option<Box<dyn PluginPersistence>>,
    /// Retained load failures (design §7.A [fix] — C++ logs to the console and
    /// drops them; we keep `(path, detail)` so the Manage-Plugins dialog can
    /// surface ABI/load errors with detail). Populated by the discovery loaders.
    load_errors: Vec<(std::path::PathBuf, String)>,
}

impl PluginManager {
    /// An empty manager (no plugins, empty registry).
    pub fn new() -> Self {
        PluginManager::default()
    }

    /// Build a manager with the four in-tree built-ins loaded + their providers
    /// registered (design §6 Phase 1). This is the Phase-1 replacement for the C++
    /// deferred `LoadPlugins()` (cpp_reference §2) — the built-ins flow through the
    /// contract instead of being wired ad hoc by the menus.
    pub fn with_builtins() -> Self {
        let mut mgr = PluginManager::new();
        for plugin in builtins::builtin_plugins() {
            mgr.add_plugin(plugin);
        }
        mgr
    }

    /// As [`with_builtins`](PluginManager::with_builtins) but also loads the
    /// in-tree [`DemoPlugin`](crate::plugin::demo::DemoPlugin) — the Phase-2
    /// deliverable that contributes a `Command`, a `Panel`, and a `Dialog` for
    /// the declarative-UI host to render (design §6 Phase 2). Kept separate so
    /// [`with_builtins`](PluginManager::with_builtins) stays exactly the
    /// four-provider Phase-1 set (the parity tests rely on that).
    pub fn with_builtins_and_demo() -> Self {
        let mut mgr = PluginManager::with_builtins();
        mgr.add_plugin(crate::plugin::demo::DemoPlugin::boxed());
        mgr
    }

    /// Install (or replace) the per-plugin [`PluginPersistence`] backend (design
    /// §7.A [fix]). With a store set, [`add_plugin`](Self::add_plugin) restores a
    /// plugin's stored enabled flag and [`set_enabled`](Self::set_enabled) /
    /// [`safe_unload`](Self::safe_unload) write through it. The builder
    /// constructors leave it `None` (the pre-Phase-6 behavior).
    pub fn set_persistence(&mut self, persistence: Box<dyn PluginPersistence>) {
        self.persistence = Some(persistence);
    }

    /// Whether a persistence backend is installed.
    pub fn has_persistence(&self) -> bool {
        self.persistence.is_some()
    }

    /// Register a plugin and fan its contributions out (cpp_reference §2
    /// auto-register). Provider contributions are added to the registry under the
    /// plugin's derived identifier; the index records the slot so the spec can be
    /// recovered at attach time. Returns the derived identifier.
    ///
    /// **Load type + persistence** (design §7.A [fix], §7.C lazy): an
    /// [`Auto`](LoadType::Auto) plugin starts **enabled**; a
    /// [`Manual`](LoadType::Manual) plugin registers its provider but starts
    /// **disabled** (loaded-on-demand). Either way, if a persistence backend has a
    /// stored enabled flag for this identifier, that flag wins (the restore path).
    pub fn add_plugin(&mut self, plugin: Box<dyn Plugin>) -> String {
        self.add_plugin_detected(plugin, false)
    }

    /// As [`add_plugin`](Self::add_plugin) but records how a ReClass.NET plugin was
    /// loaded (`managed = true` ⇒ the CLR bridge path, design §4) so the
    /// Manage-Plugins dialog can show `reclassnet-managed` vs `reclassnet-native`.
    /// For non-ReClass.NET kinds `managed` is ignored.
    pub fn add_plugin_detected(&mut self, plugin: Box<dyn Plugin>, managed: bool) -> String {
        let manifest = plugin.manifest();
        let identifier = manifest.identifier();
        let name = manifest.name.clone();
        let dll = manifest.dll_file_name.clone();
        let kind = manifest.kind;
        let load = manifest.load;
        let is_builtin = matches!(kind, PluginKind::Builtin);

        // The default enabled state honors `load = auto|manual` (design §7.A
        // [fix]); a stored persistence flag overrides it (the restore path).
        let default_enabled = matches!(load, LoadType::Auto);
        let enabled = self
            .persistence
            .as_ref()
            .and_then(|p| p.get_enabled(&identifier))
            .unwrap_or(default_enabled);

        let idx = self.plugins.len();
        let mut has_provider = false;
        for contribution in plugin.contributions() {
            match &contribution {
                Contribution::Provider(_) => has_provider = true,
                // UI-kind contributions: index them so the Phase-2 declarative
                // host can route a dispatched command / UI event / dialog result
                // back to the contributing plugin (design §6 Phase 2).
                Contribution::Command { id, .. } => {
                    self.command_index.insert(id.clone(), idx);
                }
                Contribution::Panel { id, .. }
                | Contribution::Dialog { id, .. }
                | Contribution::StatusItem { id, .. } => {
                    self.view_index.insert(id.clone(), idx);
                }
            }
        }

        if has_provider {
            self.registry.register_provider(ProviderInfo {
                name,
                identifier: identifier.clone(),
                is_builtin,
                dll_file_name: dll,
                enabled,
            });
            self.provider_index.insert(identifier.clone(), idx);
        }

        self.plugin_index.insert(identifier.clone(), idx);
        self.entries.push(PluginEntry {
            managed,
            non_provider_enabled: enabled,
            has_provider,
        });
        self.plugins.push(plugin);
        identifier
    }

    /// The provider registry both source-picker surfaces read (design §7.A [fix]).
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// Mutable registry access (e.g. enable/disable from the Manage Plugins
    /// dialog, design §7.A [fix]).
    pub fn registry_mut(&mut self) -> &mut ProviderRegistry {
        &mut self.registry
    }

    /// All loaded plugins (the C++ `plugins()`, cpp_reference §2).
    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }

    /// Find a loaded **provider** plugin by its derived identifier (the C++
    /// `FindPlugin`, keyed on the normalized identifier). Kept as the
    /// provider-lookup path (the attach flow); use
    /// [`find_plugin_any`](Self::find_plugin_any) to find a plugin that may not
    /// contribute a provider (a UI-only plugin).
    pub fn find_plugin(&self, identifier: &str) -> Option<&dyn Plugin> {
        self.provider_index
            .get(identifier)
            .map(|&i| self.plugins[i].as_ref())
    }

    /// Find ANY loaded plugin by its derived identifier — provider or not (design
    /// §6 Phase 6: the Manage-Plugins dialog manages every plugin, including
    /// UI-only ones with no `ProviderSpec`).
    pub fn find_plugin_any(&self, identifier: &str) -> Option<&dyn Plugin> {
        self.plugin_index
            .get(identifier)
            .map(|&i| self.plugins[i].as_ref())
    }

    /// Fetch the [`ProviderSpec`] registered under `identifier` (the C++
    /// `selectSource` lookup before `createProvider`, cpp_reference §5). Pulls the
    /// (single) provider contribution out of the plugin's `contributions()`.
    pub fn provider_spec(&self, identifier: &str) -> Option<ProviderSpec> {
        let &idx = self.provider_index.get(identifier)?;
        self.plugins[idx]
            .contributions()
            .into_iter()
            .find_map(|c| match c {
                Contribution::Provider(spec) => Some(spec),
                _ => None,
            })
    }

    /// Create a provider for `identifier` + `target` (the C++ attach path:
    /// `findProvider` → `createProvider`, cpp_reference §5). Surfaces the
    /// provider's error string (design §7.A [fix]). Errors if no such provider or
    /// it can't handle the target.
    pub fn create_provider(
        &self,
        identifier: &str,
        target: &str,
    ) -> Result<SharedProvider, String> {
        let spec = self
            .provider_spec(identifier)
            .ok_or_else(|| format!("no provider registered as '{identifier}'"))?;
        spec.create_provider(target)
    }

    // ── Phase-6 management + permissions (design §6 Phase 6, §7.A [fix]) ──

    /// The single row model the Manage-Plugins dialog renders for every loaded
    /// plugin — built-in, native, or auto-detected ReClass.NET — in load order
    /// (design §6 Phase 6). [`PluginRow::detected_label`] distinguishes
    /// `reclassnet-native` vs `reclassnet-managed` by how the plugin was loaded;
    /// `enabled` reflects the registry for provider plugins and the plugin's own
    /// flag for non-provider (UI-only) plugins.
    pub fn plugins_view(&self) -> Vec<PluginRow> {
        self.plugins
            .iter()
            .zip(self.entries.iter())
            .map(|(plugin, entry)| {
                let m = plugin.manifest();
                let identifier = m.identifier();
                // Provider plugins: the registry's enabled flag is authoritative;
                // non-provider plugins carry their own.
                let enabled = if entry.has_provider {
                    self.registry
                        .find(&identifier)
                        .map(|p| p.enabled)
                        .unwrap_or(entry.non_provider_enabled)
                } else {
                    entry.non_provider_enabled
                };
                PluginRow {
                    name: m.name.clone(),
                    version: m.version.clone(),
                    author: m.author.clone(),
                    description: m.description.clone(),
                    identifier,
                    kind: m.kind,
                    detected_label: detected_label(m.kind, entry.managed),
                    permissions: m.permissions.clone(),
                    enabled,
                    is_builtin: matches!(m.kind, PluginKind::Builtin),
                    dll_file_name: m.dll_file_name.clone(),
                    load: m.load,
                }
            })
            .collect()
    }

    /// Enable or disable a plugin by identifier (design §7.A [fix] — preferred over
    /// the unsafe runtime unload). Flips the registry flag (for a provider plugin)
    /// and the plugin's own flag, and — when `persist` is set and a persistence
    /// backend is installed — writes the flag through so it survives a restart.
    /// Returns whether a plugin matched.
    pub fn set_enabled(&mut self, identifier: &str, enabled: bool, persist: bool) -> bool {
        let Some(&idx) = self.plugin_index.get(identifier) else {
            return false;
        };
        // Provider plugins: the registry is the source of truth the source
        // surfaces read; also mirror onto the entry so a non-provider plugin (no
        // registry row) still tracks its state.
        self.registry.set_enabled(identifier, enabled);
        self.entries[idx].non_provider_enabled = enabled;
        if persist {
            if let Some(p) = self.persistence.as_mut() {
                p.set_enabled(identifier, enabled);
            }
        }
        true
    }

    /// **Safe unload** a plugin by identifier (design §7.A [fix] — fixing the C++
    /// dangling-provider crash, cpp_reference §2/§10.3). In order:
    ///
    /// 1. ask the host to **detach every document** using this provider
    ///    ([`PluginHost::detach_documents_using`]) so none outlives it;
    /// 2. unregister the provider from the registry;
    /// 3. drop the plugin (and, for a native plugin, its backing library) by
    ///    removing it from `plugins` and rebuilding the indices;
    /// 4. forget the persisted load-from-path entry, if any.
    ///
    /// Returns whether a plugin matched. (The detach happens **before** the drop —
    /// the whole point of the [fix]; a conformance test asserts the order via the
    /// mock host's `detached()` log.)
    pub fn safe_unload(&mut self, identifier: &str, host: &mut dyn PluginHost) -> bool {
        let Some(&idx) = self.plugin_index.get(identifier) else {
            return false;
        };

        // (1) Detach affected documents FIRST (the dangling-provider [fix]).
        host.detach_documents_using(identifier);

        // (4-prep) Remember the artifact path to forget from persistence.
        let dll = self.plugins[idx].manifest().dll_file_name.clone();

        // (2) Unregister the provider (a no-op for a non-provider plugin).
        self.registry.unregister(identifier);

        // (3) Drop the plugin + its backing library, then rebuild every index
        // (the removal shifts subsequent indices).
        self.plugins.remove(idx);
        self.entries.remove(idx);
        self.rebuild_indices();

        // (4) Forget the persisted path (if it was a load-from-path entry).
        if !dll.is_empty() {
            if let Some(p) = self.persistence.as_mut() {
                p.remove_path(&dll);
            }
        }
        true
    }

    /// Rebuild the identifier/command/view indices from `plugins` after a removal
    /// (the slot indices shift when an entry is dropped). The registry is left
    /// untouched (safe_unload already unregistered the dropped provider).
    fn rebuild_indices(&mut self) {
        self.provider_index.clear();
        self.plugin_index.clear();
        self.command_index.clear();
        self.view_index.clear();
        for (idx, plugin) in self.plugins.iter().enumerate() {
            let identifier = plugin.manifest().identifier();
            self.plugin_index.insert(identifier.clone(), idx);
            for contribution in plugin.contributions() {
                match &contribution {
                    Contribution::Provider(_) => {
                        self.provider_index.insert(identifier.clone(), idx);
                    }
                    Contribution::Command { id, .. } => {
                        self.command_index.insert(id.clone(), idx);
                    }
                    Contribution::Panel { id, .. }
                    | Contribution::Dialog { id, .. }
                    | Contribution::StatusItem { id, .. } => {
                        self.view_index.insert(id.clone(), idx);
                    }
                }
            }
        }
    }

    /// The retained load failures, `(path, detail)` (design §7.A [fix]). The
    /// Manage-Plugins dialog surfaces these so ABI/load errors are visible with
    /// detail instead of only logged (C++ shows a generic "check the console"
    /// box). Populated by
    /// [`load_native_plugins_from_dirs`](Self::load_native_plugins_from_dirs) and
    /// the default-dirs variant.
    pub fn load_errors(&self) -> &[(std::path::PathBuf, String)] {
        &self.load_errors
    }

    // ── Phase-2 declarative-host routing (design §6 Phase 2, §7.B [+]) ──
    //
    // Each entry point is wrapped in `catch_unwind` so a panicking plugin
    // surfaces as "not handled" instead of taking down the host (design §7.B
    // "Wrap every plugin entry point in `catch_unwind`"). The `&mut` borrows are
    // not unwind-safe by default, but a plugin panic here only abandons that one
    // call — the manager keeps the plugin and stays usable — so `AssertUnwindSafe`
    // is sound for our use (we don't read poisoned plugin state afterwards).

    /// Dispatch a contributed `Command` to its owning plugin (design §3). Routes
    /// by the command id recorded at registration; returns the plugin's
    /// [`CommandResult`] (the default not-handled result if no plugin owns `id`
    /// or the handler panicked). Panic-guarded (design §7.B [+]).
    pub fn handle_command(
        &mut self,
        id: &str,
        args: Value,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let Some(&idx) = self.command_index.get(id) else {
            return CommandResult::default();
        };
        let plugin = &mut self.plugins[idx];
        guard(CommandResult::default(), || {
            plugin.handle_command(id, args, host)
        })
    }

    /// Route a [`UiEvent`] from a contributed `Panel`/`Dialog`/`StatusItem` to
    /// its owning plugin (design §3 Elm loop). Returns the fresh [`ViewTree`] to
    /// re-render that view, or `None` (unowned view, no re-render, or a panic).
    /// Panic-guarded (design §7.B [+]).
    pub fn handle_ui_event(
        &mut self,
        view: &str,
        ev: UiEvent,
        host: &mut dyn PluginHost,
    ) -> Option<ViewTree> {
        let &idx = self.view_index.get(view)?;
        let plugin = &mut self.plugins[idx];
        guard(None, || plugin.handle_ui_event(view, ev, host))
    }

    /// Report a contributed `Dialog`'s outcome to its owning plugin (the
    /// generalized C++ `selectTarget` return, design §3). Panic-guarded.
    pub fn handle_dialog_closed(
        &mut self,
        view: &str,
        result: DialogResult,
        host: &mut dyn PluginHost,
    ) -> CommandResult {
        let Some(&idx) = self.view_index.get(view) else {
            return CommandResult::default();
        };
        let plugin = &mut self.plugins[idx];
        guard(CommandResult::default(), || {
            plugin.handle_dialog_closed(view, result, host)
        })
    }

    // ── Phase-3 native dynamic loading (design §6 Phase 3; `plugins` feature) ──

    /// Load a single native plugin `.so`/`.dll`/`.dylib` from `path` and register
    /// it through the SAME [`add_plugin`](PluginManager::add_plugin) flow as an
    /// in-tree built-in (design §6 Phase 3 — the §1 parity table satisfied by a
    /// real loaded library). Returns the derived identifier, or a surfaced load /
    /// ABI-mismatch error (design §7.A [fix]). The C++ `LoadPluginFromPath`,
    /// cpp_reference §2.
    #[cfg(feature = "plugins")]
    pub fn load_native_plugin(
        &mut self,
        path: &std::path::Path,
    ) -> Result<String, crate::plugin::loader::LoadError> {
        let plugin = crate::plugin::loader::load_native_plugin(path)?;
        Ok(self.add_plugin(plugin))
    }

    /// Discover + load every native plugin in the default plugin directories
    /// (design §6 Phase 3, §7.C [+] multi-dir). Registers each loaded plugin and
    /// returns the `(path, error)` failures so the caller can surface them with
    /// detail (design §7.A [fix]). The C++ deferred `LoadPlugins()` folder scan,
    /// cpp_reference §2.
    #[cfg(feature = "plugins")]
    pub fn load_native_plugins_from_default_dirs(
        &mut self,
    ) -> Vec<(std::path::PathBuf, crate::plugin::loader::LoadError)> {
        let dirs = crate::plugin::discovery::default_plugin_dirs();
        self.load_native_plugins_from_dirs(&dirs)
    }

    /// Discover + load every native plugin across `dirs`, registering each loaded
    /// plugin; returns the load failures (design §6 Phase 3). The failures are
    /// ALSO **retained** in [`load_errors`](Self::load_errors) (as `(path,
    /// detail)`) so the Manage-Plugins dialog can surface them with detail later
    /// (design §7.A [fix] — C++ logs then drops them).
    #[cfg(feature = "plugins")]
    pub fn load_native_plugins_from_dirs(
        &mut self,
        dirs: &[std::path::PathBuf],
    ) -> Vec<(std::path::PathBuf, crate::plugin::loader::LoadError)> {
        let (loaded, failures) = crate::plugin::discovery::load_from_dirs(dirs);
        for plugin in loaded {
            self.add_plugin(plugin);
        }
        // Retain the failures with their rendered detail for the dialog (the
        // returned `LoadError`s are not `Clone`, so we record the `Display`
        // string the dialog actually shows).
        for (path, err) in &failures {
            self.load_errors.push((path.clone(), err.to_string()));
        }
        failures
    }

    /// Re-ask the plugin that owns `view` for that view's current `ViewTree`
    /// (the [`PluginHost::request_rerender`](crate::plugin::host::PluginHost::request_rerender)
    /// resolution: the host calls this to pull the fresh tree out of the plugin's
    /// `contributions()`). Panic-guarded; `None` if unowned / not a view / panic.
    pub fn view_tree(&self, view: &str) -> Option<ViewTree> {
        let &idx = self.view_index.get(view)?;
        let plugin = self.plugins[idx].as_ref();
        guard(None, || {
            plugin.contributions().into_iter().find_map(|c| match c {
                Contribution::Panel { id, initial, .. } if id == view => Some(initial),
                Contribution::Dialog { id, initial, .. } if id == view => Some(initial),
                Contribution::StatusItem { id, initial } if id == view => Some(initial),
                _ => None,
            })
        })
    }
}

/// Run a plugin entry point with a panic guard (design §7.B [+] — a plugin panic
/// must not crash the host). Returns `fallback` if the closure panics. The
/// closure borrows `&mut` plugin/host, which aren't `UnwindSafe`; we assert it
/// because a panic here abandons only this one call (we don't subsequently read
/// the plugin's now-possibly-inconsistent state).
fn guard<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_builtins_registers_four_providers_in_order() {
        let mgr = PluginManager::with_builtins();
        let ids: Vec<&str> = mgr
            .registry()
            .providers()
            .iter()
            .map(|p| p.identifier.as_str())
            .collect();
        // Registration order = built-in listing order (cpp_reference §2/§3).
        assert_eq!(ids, ["file", "buffer", "snapshot", "null"]);
        // All marked built-in + enabled.
        assert!(mgr
            .registry()
            .providers()
            .iter()
            .all(|p| p.is_builtin && p.enabled));
    }

    #[test]
    fn create_provider_routes_through_the_spec() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rcx-mgr-test-{}.bin", std::process::id()));
        std::fs::write(&path, [1u8, 2, 3, 4, 5]).unwrap();

        let mgr = PluginManager::with_builtins();
        // The "file" identifier maps back to the FilePlugin's spec.
        let prov = match mgr.create_provider("file", path.to_str().unwrap()) {
            Ok(p) => p,
            Err(e) => panic!("file provider: {e}"),
        };
        assert_eq!(prov.size(), 5);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_provider_unknown_identifier_errors() {
        let mgr = PluginManager::with_builtins();
        let err = match mgr.create_provider("doesnotexist", "x") {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(err.contains("doesnotexist"));
    }

    #[test]
    fn find_plugin_by_identifier() {
        let mgr = PluginManager::with_builtins();
        assert!(mgr.find_plugin("buffer").is_some());
        assert_eq!(mgr.find_plugin("buffer").unwrap().manifest().name, "Buffer");
        assert!(mgr.find_plugin("nope").is_none());
    }

    #[test]
    fn enable_disable_via_registry_mut() {
        let mut mgr = PluginManager::with_builtins();
        assert!(mgr.registry_mut().set_enabled("null", false));
        let enabled: Vec<&str> = mgr
            .registry()
            .enabled_providers()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(enabled, ["file", "buffer", "snapshot"]);
    }

    // ── Phase-6 management + permissions ──

    use crate::plugin::manifest::{LoadType, Permission, PluginKind, PluginManifest};
    use crate::plugin::provider_spec::ProviderSpec;

    #[test]
    fn mem_persistence_round_trips_enabled_and_paths() {
        let mut p = MemPluginPersistence::new();
        // enabled: unset → None, then stored.
        assert_eq!(p.get_enabled("x"), None);
        p.set_enabled("x", false);
        assert_eq!(p.get_enabled("x"), Some(false));

        // paths: idempotent add, remove.
        assert!(p.get_paths().is_empty());
        p.add_path("/a.so");
        p.add_path("/a.so"); // dedup
        p.add_path("/b.so");
        assert_eq!(
            p.get_paths(),
            vec!["/a.so".to_string(), "/b.so".to_string()]
        );
        p.remove_path("/a.so");
        assert_eq!(p.get_paths(), vec!["/b.so".to_string()]);
    }

    /// A bare provider plugin built from an arbitrary manifest, for exercising
    /// load-type / persistence / safe-unload without the built-ins' fixed set.
    struct TestProviderPlugin {
        manifest: PluginManifest,
    }
    impl crate::plugin::contract::Plugin for TestProviderPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn contributions(&self) -> Vec<Contribution> {
            // A provider that can_handle anything and yields an empty buffer.
            vec![Contribution::Provider(ProviderSpec::new(
                |_t| true,
                |t| {
                    Ok(
                        std::sync::Arc::new(crate::provider::BufferProvider::new(vec![], t))
                            as SharedProvider,
                    )
                },
            ))]
        }
    }

    fn provider_plugin(name: &str, load: LoadType) -> Box<dyn Plugin> {
        let mut m = PluginManifest::builtin(name, "desc", vec![Permission::ReadMemory]);
        m.kind = PluginKind::Native;
        m.load = load;
        m.dll_file_name = format!("{}.so", name.to_lowercase());
        Box::new(TestProviderPlugin { manifest: m })
    }

    #[test]
    fn manual_plugin_starts_disabled_without_persistence() {
        let mut mgr = PluginManager::new();
        let id = mgr.add_plugin(provider_plugin("Manual Reader", LoadType::Manual));
        // Registered but disabled (honors load = manual, design §7.A [fix]).
        assert!(mgr.registry().find(&id).is_some());
        assert!(!mgr.registry().find(&id).unwrap().enabled);
        assert_eq!(mgr.registry().enabled_providers().count(), 0);

        // An Auto plugin starts enabled.
        let id2 = mgr.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));
        assert!(mgr.registry().find(&id2).unwrap().enabled);
    }

    #[test]
    fn persistence_restores_enabled_flag_overriding_load_default() {
        // A Manual plugin would default disabled, but a stored `enabled = true`
        // restores it on load (the restore path, design §7.A [fix]).
        let mut persist = MemPluginPersistence::new();
        persist.set_enabled("manualreader", true);

        let mut mgr = PluginManager::new();
        mgr.set_persistence(Box::new(persist));
        let id = mgr.add_plugin(provider_plugin("Manual Reader", LoadType::Manual));
        assert!(mgr.registry().find(&id).unwrap().enabled);

        // Conversely, an Auto plugin with a stored `false` starts disabled.
        let mut persist2 = MemPluginPersistence::new();
        persist2.set_enabled("autoreader", false);
        let mut mgr2 = PluginManager::new();
        mgr2.set_persistence(Box::new(persist2));
        let id2 = mgr2.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));
        assert!(!mgr2.registry().find(&id2).unwrap().enabled);
    }

    #[test]
    fn set_enabled_flips_registry_and_persists() {
        let mut mgr = PluginManager::new();
        mgr.set_persistence(Box::new(MemPluginPersistence::new()));
        mgr.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));

        // Disable with persist=true: registry flips AND it's remembered.
        assert!(mgr.set_enabled("autoreader", false, true));
        assert!(!mgr.registry().find("autoreader").unwrap().enabled);

        // Re-adding the SAME plugin into a fresh manager that shares the stored
        // state restores the disabled flag.
        let mut persist = MemPluginPersistence::new();
        persist.set_enabled("autoreader", false);
        let mut mgr2 = PluginManager::new();
        mgr2.set_persistence(Box::new(persist));
        mgr2.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));
        assert!(!mgr2.registry().find("autoreader").unwrap().enabled);

        // An unknown identifier returns false.
        assert!(!mgr.set_enabled("nope", false, false));
    }

    #[test]
    fn set_enabled_without_persist_does_not_remember() {
        let mut mgr = PluginManager::new();
        mgr.set_persistence(Box::new(MemPluginPersistence::new()));
        mgr.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));
        // persist=false: registry flips but nothing is written through.
        assert!(mgr.set_enabled("autoreader", false, false));
        assert!(!mgr.registry().find("autoreader").unwrap().enabled);
        // The stored flag was never set, so a fresh manager defaults to Auto=on.
        let mut mgr2 = PluginManager::new();
        mgr2.add_plugin(provider_plugin("Auto Reader", LoadType::Auto));
        assert!(mgr2.registry().find("autoreader").unwrap().enabled);
    }

    #[test]
    fn plugins_view_models_every_plugin() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        // One more native manual plugin on top of the four built-ins + demo.
        mgr.add_plugin(provider_plugin("Remote Reader", LoadType::Manual));

        let rows = mgr.plugins_view();
        // Four built-ins + the demo (UI-only) + the native = 6 rows.
        assert_eq!(rows.len(), 6);

        // The built-ins are flagged builtin + enabled + labelled "builtin".
        let file = rows.iter().find(|r| r.identifier == "file").unwrap();
        assert!(file.is_builtin);
        assert!(file.enabled);
        assert_eq!(file.detected_label, "builtin");
        assert_eq!(file.kind, PluginKind::Builtin);

        // The native manual plugin: not builtin, disabled (manual), permission
        // disclosed, detected label "native", artifact recorded.
        let remote = rows
            .iter()
            .find(|r| r.identifier == "remotereader")
            .unwrap();
        assert!(!remote.is_builtin);
        assert!(!remote.enabled);
        assert_eq!(remote.detected_label, "native");
        assert_eq!(remote.load, LoadType::Manual);
        assert_eq!(remote.permissions, vec![Permission::ReadMemory]);
        assert_eq!(remote.dll_file_name, "remote reader.so");

        // The demo plugin is a non-provider row that still appears.
        assert!(rows.iter().any(|r| r.identifier == "plugindemo"));
    }

    #[test]
    fn plugins_view_reclassnet_label_distinguishes_native_vs_managed() {
        let mut mgr = PluginManager::new();
        let mut native = PluginManifest::builtin("RcNet Native", "n", vec![]);
        native.kind = PluginKind::ReclassNet;
        mgr.add_plugin_detected(Box::new(TestProviderPlugin { manifest: native }), false);

        let mut managed = PluginManifest::builtin("RcNet Managed", "m", vec![]);
        managed.kind = PluginKind::ReclassNet;
        mgr.add_plugin_detected(Box::new(TestProviderPlugin { manifest: managed }), true);

        let rows = mgr.plugins_view();
        let n = rows.iter().find(|r| r.identifier == "rcnetnative").unwrap();
        let m = rows
            .iter()
            .find(|r| r.identifier == "rcnetmanaged")
            .unwrap();
        assert_eq!(n.detected_label, "reclassnet-native");
        assert_eq!(m.detected_label, "reclassnet-managed");
    }

    #[test]
    fn find_plugin_any_finds_ui_only_plugins() {
        let mgr = PluginManager::with_builtins_and_demo();
        // The demo plugin contributes no provider → find_plugin (provider-only)
        // misses it, find_plugin_any finds it.
        assert!(mgr.find_plugin("plugindemo").is_none());
        assert!(mgr.find_plugin_any("plugindemo").is_some());
        // A provider plugin is found by both.
        assert!(mgr.find_plugin("file").is_some());
        assert!(mgr.find_plugin_any("file").is_some());
        assert!(mgr.find_plugin_any("nope").is_none());
    }

    #[test]
    fn safe_unload_detaches_first_then_drops() {
        let mut mgr = PluginManager::with_builtins();
        let id = mgr.add_plugin(provider_plugin("Doomed Reader", LoadType::Auto));
        assert!(mgr.find_plugin_any(&id).is_some());
        assert!(mgr.registry().find(&id).is_some());

        let mut host = MockPluginHost::new();
        assert!(mgr.safe_unload(&id, &mut host));

        // (1) The host was asked to detach the provider's documents FIRST.
        assert_eq!(host.detached(), [id.as_str()]);
        // (2) The provider is gone from the registry + (3) the plugin from the set.
        assert!(mgr.registry().find(&id).is_none());
        assert!(mgr.find_plugin_any(&id).is_none());
        // The surviving built-ins are still resolvable through the rebuilt index.
        assert!(mgr.find_plugin("file").is_some());
        assert_eq!(mgr.registry().providers().len(), 4);

        // Unloading an unknown id is a no-op false (host not re-asked).
        assert!(!mgr.safe_unload("nope", &mut host));
        assert_eq!(host.detached().len(), 1);
    }

    /// A shared-handle probe persistence so a test can read back what the manager
    /// passed to `remove_path` (the boxed copy and the test's handle share state).
    #[derive(Clone, Default)]
    struct SharedProbe {
        removed: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }
    impl PluginPersistence for SharedProbe {
        fn get_enabled(&self, _id: &str) -> Option<bool> {
            None
        }
        fn set_enabled(&mut self, _id: &str, _enabled: bool) {}
        fn get_paths(&self) -> Vec<String> {
            Vec::new()
        }
        fn add_path(&mut self, _path: &str) {}
        fn remove_path(&mut self, path: &str) {
            self.removed.borrow_mut().push(path.to_string());
        }
    }

    #[test]
    fn safe_unload_calls_remove_path_with_artifact() {
        let probe = SharedProbe::default();
        let mut mgr = PluginManager::new();
        mgr.set_persistence(Box::new(probe.clone()));
        // The provider_plugin's dll_file_name is "<lowercased name, spaces kept>.so".
        let id = mgr.add_plugin(provider_plugin("Doomed Reader", LoadType::Auto));

        let mut host = MockPluginHost::new();
        assert!(mgr.safe_unload(&id, &mut host));
        // The manager forgot the artifact filename from persistence.
        assert_eq!(
            *probe.removed.borrow(),
            vec!["doomed reader.so".to_string()]
        );
    }

    #[test]
    fn safe_unload_reindexes_commands_and_views() {
        // After dropping a plugin, the command/view indices must still route to
        // the SURVIVING plugins (the removal shifts indices — the rebuild [fix]).
        let mut mgr = PluginManager::with_builtins_and_demo();
        let doomed = mgr.add_plugin(provider_plugin("Doomed", LoadType::Auto));

        let mut host = MockPluginHost::new();
        assert!(mgr.safe_unload(&doomed, &mut host));

        // The demo plugin's command still routes correctly after re-indexing.
        let res = mgr.handle_command(
            crate::plugin::demo::CMD_PING,
            serde_json::Value::Null,
            &mut host,
        );
        assert!(res.handled);
        // And its panel view still resolves.
        assert!(mgr.view_tree(crate::plugin::demo::PANEL_ID).is_some());
    }

    #[test]
    fn load_errors_starts_empty() {
        // A fresh manager has no retained load errors.
        let mgr = PluginManager::with_builtins();
        assert!(mgr.load_errors().is_empty());
    }

    /// With the loader feature on, a failed discovery load is RETAINED in
    /// `load_errors` (the [fix]: C++ logs then drops the detail).
    #[cfg(feature = "plugins")]
    #[test]
    fn load_native_plugins_retains_failures_in_load_errors() {
        let dir = std::env::temp_dir().join(format!("rcx-mgr-loaderr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let ext = crate::plugin::discovery::platform_lib_extension();
        // A junk file with a plugin extension → discovery sniffs it, fails to
        // recognize it, and reports a structured Unrecognized error.
        let junk = dir.join(format!("not_a_plugin.{ext}"));
        std::fs::write(&junk, b"definitely not a shared library").unwrap();

        let mut mgr = PluginManager::with_builtins();
        let failures = mgr.load_native_plugins_from_dirs(&[dir.clone()]);
        assert!(!failures.is_empty(), "the junk file should fail to load");
        // The same failures are retained for the dialog, with a non-empty detail
        // string (the `Display` the dialog renders).
        assert_eq!(mgr.load_errors().len(), failures.len());
        let (path, detail) = &mgr.load_errors()[0];
        assert_eq!(path.file_name(), junk.file_name());
        assert!(!detail.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Phase-2 routing ──

    use crate::plugin::demo;
    use crate::plugin::host::MockPluginHost;

    #[test]
    fn with_builtins_unchanged_with_demo_added_separately() {
        // Parity: with_builtins is still exactly the four-provider set.
        let plain = PluginManager::with_builtins();
        assert_eq!(plain.registry().enabled_providers().count(), 4);
        assert!(plain.find_plugin("plugindemo").is_none());

        // The demo constructor adds the demo plugin (one more plugin, but it
        // contributes no provider, so the registry is still the four built-ins).
        let demo_mgr = PluginManager::with_builtins_and_demo();
        assert_eq!(demo_mgr.registry().enabled_providers().count(), 4);
        assert_eq!(demo_mgr.plugins().len(), 5);
    }

    #[test]
    fn routes_command_to_owning_plugin() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_command(demo::CMD_PING, serde_json::Value::Null, &mut host);
        assert!(res.handled);
        assert_eq!(host.toasts(), ["Plugin Demo: pong"]);
    }

    #[test]
    fn unknown_command_is_not_handled() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_command("nope.nothing", serde_json::Value::Null, &mut host);
        assert!(!res.handled);
    }

    #[test]
    fn routes_ui_event_and_returns_fresh_tree() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        // Attach button on the demo dialog: sets the data source + closes it.
        let tree = mgr.handle_ui_event(
            demo::DIALOG_ID,
            crate::plugin::view::UiEvent::Clicked(demo::BTN_ATTACH.to_string()),
            &mut host,
        );
        assert!(tree.is_some());
        assert_eq!(host.closed_dialogs(), [demo::DIALOG_ID]);
        assert!(host.data_source().is_some());
    }

    #[test]
    fn ui_event_for_unowned_view_is_none() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        assert_eq!(
            mgr.handle_ui_event(
                "no.such.view",
                crate::plugin::view::UiEvent::Clicked("x".to_string()),
                &mut host
            ),
            None
        );
    }

    #[test]
    fn view_tree_pulls_current_panel_tree() {
        let mgr = PluginManager::with_builtins_and_demo();
        let tree = mgr.view_tree(demo::PANEL_ID).expect("panel tree");
        assert!(matches!(tree, crate::plugin::view::ViewTree::Column(_)));
        assert!(mgr.view_tree("no.such.view").is_none());
    }

    #[test]
    fn routes_dialog_closed_to_plugin() {
        let mut mgr = PluginManager::with_builtins_and_demo();
        let mut host = MockPluginHost::new();
        let res = mgr.handle_dialog_closed(
            demo::DIALOG_ID,
            crate::plugin::contract::DialogResult::Submitted {
                values: vec![(demo::FIELD_TARGET.to_string(), "7:x.exe".to_string())],
            },
            &mut host,
        );
        assert!(res.handled);
        assert_eq!(
            host.data_source(),
            Some(&(demo::DEMO_IDENTIFIER.to_string(), "7:x.exe".to_string()))
        );
    }

    #[test]
    fn panicking_plugin_is_contained() {
        // A plugin whose command handler panics must not crash the host: the
        // guard returns the not-handled fallback (design §7.B [+]).
        struct PanicPlugin {
            manifest: crate::plugin::manifest::PluginManifest,
        }
        impl crate::plugin::contract::Plugin for PanicPlugin {
            fn manifest(&self) -> &crate::plugin::manifest::PluginManifest {
                &self.manifest
            }
            fn contributions(&self) -> Vec<Contribution> {
                vec![Contribution::Command {
                    id: "boom.go".to_string(),
                    title: "Boom".to_string(),
                    slot: crate::plugin::contract::CommandSlot::Menu,
                }]
            }
            fn handle_command(
                &mut self,
                _id: &str,
                _args: serde_json::Value,
                _host: &mut dyn PluginHost,
            ) -> CommandResult {
                panic!("plugin blew up");
            }
        }

        let mut mgr = PluginManager::new();
        mgr.add_plugin(Box::new(PanicPlugin {
            manifest: crate::plugin::manifest::PluginManifest::builtin("Boom", "panics", vec![]),
        }));
        let mut host = MockPluginHost::new();
        // Silence the default panic hook's backtrace noise during this test.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let res = mgr.handle_command("boom.go", serde_json::Value::Null, &mut host);
        std::panic::set_hook(prev);
        // Contained: not handled, host still alive.
        assert!(!res.handled);
    }
}
