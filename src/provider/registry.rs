//! Provider registry — lists data sources for the Source picker.
//!
//! Port of `src/providerregistry.h`. Providers register here so they appear in
//! the Source menu. The C++ supports plugin-based and built-in providers; only
//! the built-in (benign) factories are in scope. The Qt menu-population helper
//! (`populateSourceMenu`) belongs to the UI layer and is omitted here.

/// `struct SavedSourceDisplay` (`providerregistry.h:13-16`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedSourceDisplay {
    pub text: String,
    pub active: bool,
}

/// `ProviderRegistry::ProviderInfo` (`providerregistry.h:29-44`), trimmed to the
/// in-scope fields (the Qt `IProviderPlugin*` / `BuiltinFactory` callbacks are
/// modeled as a simple identifier + display name; native plugins are stubs).
///
/// The actual create/can_handle factory lives on the plugin's
/// [`ProviderSpec`](crate::plugin::provider_spec::ProviderSpec); the registry is
/// the **descriptor + listing model** both source-picker surfaces consume
/// (design §7.A [fix] — one shared list, not two divergent tables). The owning
/// [`PluginManager`](crate::plugin::manager::PluginManager) maps an identifier
/// back to the spec when attaching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    /// display name (e.g. "Process Memory").
    pub name: String,
    /// unique id (e.g. "process") — derived via
    /// [`derive_identifier`](crate::plugin::manifest::derive_identifier).
    pub identifier: String,
    pub is_builtin: bool,
    /// original DLL/SO filename (plugin-based only).
    pub dll_file_name: String,
    /// Whether this provider is currently enabled (design §7.A [fix] — C++ had no
    /// enable/disable). Defaults to `true`. Disabled providers are hidden from the
    /// source surfaces but remain registered (the [fix] prefers disable over the
    /// unsafe runtime unload).
    pub enabled: bool,
}

impl Default for ProviderInfo {
    fn default() -> Self {
        ProviderInfo {
            name: String::new(),
            identifier: String::new(),
            is_builtin: false,
            dll_file_name: String::new(),
            enabled: true,
        }
    }
}

/// `class ProviderRegistry` (`providerregistry.h:24-76`). A process-global
/// singleton in C++; modeled as a plain owned registry here (the app holds one).
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: Vec<ProviderInfo>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        ProviderRegistry::default()
    }

    /// Register a provider descriptor (`registerProvider`,
    /// `providerregistry.h:50`). The general entry point the
    /// [`PluginManager`](crate::plugin::manager::PluginManager) calls for every
    /// provider contribution (built-in or plugin). Listing order = registration
    /// order (cpp_reference §3). A duplicate identifier is ignored (the C++
    /// load-from-path dedupe).
    pub fn register_provider(&mut self, info: ProviderInfo) {
        if self.find(&info.identifier).is_some() {
            return;
        }
        self.providers.push(info);
    }

    /// `registerBuiltinProvider(name, identifier, factory)`
    /// (`providerregistry.h:53`). Thin wrapper over
    /// [`register_provider`](Self::register_provider) preserving the existing
    /// callers' two-argument shape; marks the entry `is_builtin = true`.
    pub fn register_builtin(&mut self, name: impl Into<String>, identifier: impl Into<String>) {
        self.register_provider(ProviderInfo {
            name: name.into(),
            identifier: identifier.into(),
            is_builtin: true,
            ..ProviderInfo::default()
        });
    }

    /// `unregisterProvider(identifier)` (`providerregistry.h:56`).
    pub fn unregister(&mut self, identifier: &str) {
        self.providers.retain(|p| p.identifier != identifier);
    }

    /// `providers()` (`providerregistry.h:59`) — **all** registered providers,
    /// enabled or not (registration order).
    pub fn providers(&self) -> &[ProviderInfo] {
        &self.providers
    }

    /// The enabled providers only (design §7.A [fix]). The source-picker surfaces
    /// list these; disabled providers stay registered but hidden.
    pub fn enabled_providers(&self) -> impl Iterator<Item = &ProviderInfo> {
        self.providers.iter().filter(|p| p.enabled)
    }

    /// Enable/disable a provider by identifier (design §7.A [fix] — preferred over
    /// the unsafe runtime unload). Returns whether a provider matched.
    pub fn set_enabled(&mut self, identifier: &str, enabled: bool) -> bool {
        if let Some(p) = self
            .providers
            .iter_mut()
            .find(|p| p.identifier == identifier)
        {
            p.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// `findProvider(identifier)` (`providerregistry.h:62`).
    pub fn find(&self, identifier: &str) -> Option<&ProviderInfo> {
        self.providers.iter().find(|p| p.identifier == identifier)
    }

    /// `clear()` (`providerregistry.h:65`).
    pub fn clear(&mut self) {
        self.providers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_provider_keeps_order_and_dedupes() {
        let mut reg = ProviderRegistry::new();
        reg.register_builtin("File", "file");
        reg.register_builtin("Buffer", "buffer");
        // Duplicate identifier is ignored (load-from-path dedupe).
        reg.register_builtin("File again", "file");
        let ids: Vec<&str> = reg
            .providers()
            .iter()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(ids, ["file", "buffer"]);
    }

    #[test]
    fn enable_disable_filters_enabled_list() {
        let mut reg = ProviderRegistry::new();
        reg.register_builtin("File", "file");
        reg.register_builtin("Buffer", "buffer");
        assert_eq!(reg.enabled_providers().count(), 2);

        assert!(reg.set_enabled("buffer", false));
        assert!(!reg.set_enabled("nope", false));
        let enabled: Vec<&str> = reg
            .enabled_providers()
            .map(|p| p.identifier.as_str())
            .collect();
        assert_eq!(enabled, ["file"]);
        // Still registered, just hidden.
        assert_eq!(reg.providers().len(), 2);
        assert!(!reg.find("buffer").unwrap().enabled);
    }

    #[test]
    fn provider_info_defaults_to_enabled() {
        assert!(ProviderInfo::default().enabled);
    }
}
