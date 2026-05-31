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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInfo {
    /// display name (e.g. "Process Memory").
    pub name: String,
    /// unique id (e.g. "process").
    pub identifier: String,
    pub is_builtin: bool,
    /// original DLL/SO filename (plugin-based only).
    pub dll_file_name: String,
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

    /// `registerBuiltinProvider(name, identifier, factory)`
    /// (`providerregistry.h:53`). The factory closure is the UI layer's job;
    /// here we record the descriptor.
    pub fn register_builtin(&mut self, name: impl Into<String>, identifier: impl Into<String>) {
        self.providers.push(ProviderInfo {
            name: name.into(),
            identifier: identifier.into(),
            is_builtin: true,
            dll_file_name: String::new(),
        });
    }

    /// `unregisterProvider(identifier)` (`providerregistry.h:56`).
    pub fn unregister(&mut self, identifier: &str) {
        self.providers.retain(|p| p.identifier != identifier);
    }

    /// `providers()` (`providerregistry.h:59`).
    pub fn providers(&self) -> &[ProviderInfo] {
        &self.providers
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
