//! Plugin manifest — the static metadata + permissions of a plugin, and the ONE
//! centralized identifier-derivation helper.
//!
//! Maps the design §5 `plugin.toml` schema (`name`, `version`, `author`,
//! `description`, `kind`, `load`, `permissions`, plus the C++ `dllFileName`
//! tracked by the registry). **Parsing** of `plugin.toml` itself is Phase 6; this
//! module supplies only the in-memory struct + a [`PluginManifest::builtin`]
//! constructor for the in-tree built-ins.
//!
//! ## The `derive_identifier` [fix]
//!
//! C++ derives a provider's routing key as `Name().toLower().replace(" ","")` and
//! duplicates that expression across three call sites
//! (`pluginmanager.cpp` registration, `UnloadPlugin`, and `controller.cpp`
//! `selectSource` re-normalization — cpp_reference §2, §5, §10.6) that must stay
//! in sync. Design §7.A [fix] requires centralizing it; [`derive_identifier`] is
//! that single helper, and every consumer in this crate routes through it.

/// `IPlugin::Type()` (`iplugin.h:35-42`) — the only C++ plugin kind is
/// `ProviderPlugin`; we widen it to the design §5 `kind = builtin|native|
/// reclassnet|process` discriminant. The in-tree built-ins are [`Builtin`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PluginKind {
    /// Compiled-in (in-tree registry) plugin — the Phase 1 path. (cpp had no
    /// equivalent: its only built-in, "File", was wired by the menus directly.)
    #[default]
    Builtin,
    /// A natively-loaded `.so`/`.dll`/`.dylib` plugin (Phase 3, `abi_stable`).
    Native,
    /// A bridged ReClass.NET native/managed plugin (Phase 4/5).
    ReclassNet,
    /// A subprocess / RPC plugin (Phase 7).
    Process,
}

impl PluginKind {
    /// The `plugin.toml` token (`kind = …`), for round-tripping in Phase 6.
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Builtin => "builtin",
            PluginKind::Native => "native",
            PluginKind::ReclassNet => "reclassnet",
            PluginKind::Process => "process",
        }
    }
}

/// `k_ELoadType { Auto, Manual }` (`iplugin.h`). In C++ this is **dead** — every
/// DLL auto-loads regardless (cpp_reference §10.1). The design §7.A [fix] honors
/// it (Phase 6); Phase 1 records it on the manifest so the later wiring is purely
/// additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LoadType {
    /// Loaded eagerly when discovered.
    #[default]
    Auto,
    /// Loaded on demand / on enable.
    Manual,
}

impl LoadType {
    /// The `plugin.toml` token (`load = …`).
    pub fn as_str(self) -> &'static str {
        match self {
            LoadType::Auto => "auto",
            LoadType::Manual => "manual",
        }
    }
}

/// A declared capability (design §5 `permissions = […]`). For native plugins this
/// is **disclosure + consent**, not a hard sandbox (design §5); subprocess plugins
/// can be OS-confined. Phase 1 only records the set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    ReadMemory,
    WriteMemory,
    Network,
    Filesystem,
    AddUi,
    AddProvider,
}

impl Permission {
    /// The `plugin.toml` token.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::ReadMemory => "read_memory",
            Permission::WriteMemory => "write_memory",
            Permission::Network => "network",
            Permission::Filesystem => "filesystem",
            Permission::AddUi => "add_ui",
            Permission::AddProvider => "add_provider",
        }
    }
}

/// The plugin's static metadata (design §5; mirrors the C++ `IPlugin`
/// `Name/Version/Author/Description` + `Type/LoadType` and the registry's
/// `dllFileName`). No `QIcon` — icon selection is a UI concern keyed off the
/// derived identifier (see `sourcechooser::source_icon`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginManifest {
    /// Display name (e.g. "Remote Process Memory"). The identifier is *derived*
    /// from this via [`derive_identifier`] — never stored separately, so the two
    /// can never drift (the C++ wart §7.A [fix]).
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub kind: PluginKind,
    pub load: LoadType,
    pub permissions: Vec<Permission>,
    /// The backing library filename (plugin-based only; empty for built-ins). The
    /// C++ `ProviderInfo::dllFileName`, used as the dedupe key on load-from-path
    /// and the trailing hint in the source picker.
    pub dll_file_name: String,
}

impl Default for PluginManifest {
    fn default() -> Self {
        PluginManifest {
            name: String::new(),
            version: String::new(),
            author: String::new(),
            description: String::new(),
            kind: PluginKind::Builtin,
            load: LoadType::Auto,
            permissions: Vec::new(),
            dll_file_name: String::new(),
        }
    }
}

impl PluginManifest {
    /// Construct a built-in (in-tree) plugin manifest. The package version is the
    /// host version (the built-ins ship with the app, like the C++ "File" source).
    pub fn builtin(
        name: impl Into<String>,
        description: impl Into<String>,
        permissions: Vec<Permission>,
    ) -> Self {
        PluginManifest {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            author: "Reclass (Rust port)".to_string(),
            description: description.into(),
            kind: PluginKind::Builtin,
            load: LoadType::Auto,
            permissions,
            dll_file_name: String::new(),
        }
    }

    /// The routing key for this plugin (design §7.A [fix]) — derived, never stored.
    /// Equal to `derive_identifier(&self.name)`.
    pub fn identifier(&self) -> String {
        derive_identifier(&self.name)
    }
}

/// The ONE centralized identifier derivation (design §7.A [fix]).
///
/// Exact C++ parity with `Name().toLower().replace(" ","")` (cpp_reference §2 load
/// registration, §5 `selectSource` re-normalization, §10.6): lowercase, then drop
/// **spaces only** (other punctuation like `.` is preserved — that is why
/// `"ReClass.NET Compat Layer"` → `"reclass.netcompatlayer"`). Uses ASCII
/// lowercasing to match Qt's `QString::toLower()` behavior for the provider names
/// in scope (all ASCII).
pub fn derive_identifier(name: &str) -> String {
    name.chars()
        .filter(|c| *c != ' ')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_identifier_drops_spaces_and_lowercases() {
        // The C++ examples from cpp_reference §2/§3 icon table.
        assert_eq!(
            derive_identifier("Remote Process Memory"),
            "remoteprocessmemory"
        );
        assert_eq!(derive_identifier("Process Memory"), "processmemory");
        assert_eq!(derive_identifier("Kernel Memory"), "kernelmemory");
        assert_eq!(derive_identifier("WinDbg Memory"), "windbgmemory");
    }

    #[test]
    fn derive_identifier_preserves_dots() {
        // The dot is NOT a space, so it survives: this is the exact key the C++
        // icon table uses (`reclass.netcompatlayer`, cpp_reference §3).
        assert_eq!(
            derive_identifier("ReClass.NET Compat Layer"),
            "reclass.netcompatlayer"
        );
    }

    #[test]
    fn derive_identifier_single_word_is_just_lowercased() {
        assert_eq!(derive_identifier("File"), "file");
        assert_eq!(derive_identifier("Buffer"), "buffer");
    }

    #[test]
    fn manifest_identifier_matches_helper() {
        let m = PluginManifest::builtin("Process Memory", "desc", vec![Permission::ReadMemory]);
        assert_eq!(m.identifier(), derive_identifier(&m.name));
        assert_eq!(m.identifier(), "processmemory");
    }

    #[test]
    fn builtin_manifest_defaults() {
        let m = PluginManifest::builtin("File", "Reads a file.", vec![]);
        assert_eq!(m.kind, PluginKind::Builtin);
        assert_eq!(m.load, LoadType::Auto);
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
        assert!(m.dll_file_name.is_empty());
    }

    #[test]
    fn enum_tokens_round_trip() {
        assert_eq!(PluginKind::ReclassNet.as_str(), "reclassnet");
        assert_eq!(LoadType::Manual.as_str(), "manual");
        assert_eq!(Permission::AddProvider.as_str(), "add_provider");
    }
}
