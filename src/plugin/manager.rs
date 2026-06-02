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

use crate::plugin::builtins;
use crate::plugin::contract::{Contribution, Plugin};
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
use crate::provider::{ProviderInfo, ProviderRegistry};

/// Owns the loaded plugins + the provider registry they populate (the C++
/// `MainWindow::m_pluginManager`, cpp_reference §2). One per application.
#[derive(Default)]
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
    registry: ProviderRegistry,
    /// identifier → index of the plugin in `plugins` (so we can fetch its
    /// `ProviderSpec` to create a provider at attach time). Built during
    /// registration; the C++ re-finds the plugin by recomputed identifier.
    provider_index: HashMap<String, usize>,
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

    /// Register a plugin and fan its contributions out (cpp_reference §2
    /// auto-register). Provider contributions are added to the registry under the
    /// plugin's derived identifier; the index records the slot so the spec can be
    /// recovered at attach time. Returns the derived identifier.
    pub fn add_plugin(&mut self, plugin: Box<dyn Plugin>) -> String {
        let manifest = plugin.manifest();
        let identifier = manifest.identifier();
        let name = manifest.name.clone();
        let dll = manifest.dll_file_name.clone();
        let is_builtin = matches!(manifest.kind, crate::plugin::manifest::PluginKind::Builtin);

        let idx = self.plugins.len();
        let mut has_provider = false;
        for contribution in plugin.contributions() {
            if let Contribution::Provider(_) = contribution {
                has_provider = true;
            }
            // UI-kind contributions are wired by the Phase-2 declarative host;
            // Phase 1 only fans out provider contributions.
        }

        if has_provider {
            self.registry.register_provider(ProviderInfo {
                name,
                identifier: identifier.clone(),
                is_builtin,
                dll_file_name: dll,
                enabled: true,
            });
            self.provider_index.insert(identifier.clone(), idx);
        }

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

    /// Find a loaded plugin by its derived identifier (the C++ `FindPlugin`, but
    /// keyed on the normalized identifier).
    pub fn find_plugin(&self, identifier: &str) -> Option<&dyn Plugin> {
        self.provider_index
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
}
