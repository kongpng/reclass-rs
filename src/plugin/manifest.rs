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

/// The auto-detected kind label shown in the Manage Plugins dialog for a plugin
/// **without** a `plugin.toml` (design §6 Phase 6 disclosure; design §4
/// detection). The host loader sniffs each library and bridges it; this maps the
/// resulting [`PluginKind`] + how it was loaded to a short human label:
///
/// - [`Builtin`](PluginKind::Builtin) → `"builtin"`
/// - [`Native`](PluginKind::Native) → `"native"`
/// - [`Process`](PluginKind::Process) → `"process"`
/// - [`ReclassNet`](PluginKind::ReclassNet) → `"reclassnet-managed"` when it came
///   through the managed CLR bridge (`managed = true`), else `"reclassnet-native"`
///   (the cross-platform native CoreFunctions path) — the two ReClass.NET
///   sub-paths of design §4 the user should be able to tell apart.
pub fn detected_label(kind: PluginKind, managed: bool) -> &'static str {
    match kind {
        PluginKind::Builtin => "builtin",
        PluginKind::Native => "native",
        PluginKind::Process => "process",
        PluginKind::ReclassNet => {
            if managed {
                "reclassnet-managed"
            } else {
                "reclassnet-native"
            }
        }
    }
}

// ── `plugin.toml` parsing (design §5 schema, §6 Phase 6) ─────────────────────

/// A `plugin.toml` parse failure (design §7.A [fix] — structured, surfaced
/// errors rather than a "check the console" box). Each unknown-token variant
/// names the offending value so the Manage Plugins dialog can show actionable
/// detail.
#[derive(Debug)]
pub enum ManifestError {
    /// The manifest file could not be read.
    Io(std::io::Error),
    /// The bytes were not valid TOML / were missing a required field.
    Toml(toml::de::Error),
    /// `kind = "…"` was not one of `builtin|native|reclassnet|process`.
    UnknownKind(String),
    /// `load = "…"` was not one of `auto|manual`.
    UnknownLoad(String),
    /// A `permissions = […]` entry was not a recognized capability token.
    UnknownPermission(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "could not read plugin.toml: {e}"),
            ManifestError::Toml(e) => write!(f, "invalid plugin.toml: {e}"),
            ManifestError::UnknownKind(k) => write!(
                f,
                "unknown plugin kind '{k}' (expected builtin|native|reclassnet|process)"
            ),
            ManifestError::UnknownLoad(l) => {
                write!(f, "unknown load type '{l}' (expected auto|manual)")
            }
            ManifestError::UnknownPermission(p) => write!(
                f,
                "unknown permission '{p}' (expected read_memory|write_memory|network|\
                 filesystem|add_ui|add_provider)"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<std::io::Error> for ManifestError {
    fn from(e: std::io::Error) -> Self {
        ManifestError::Io(e)
    }
}

impl From<toml::de::Error> for ManifestError {
    fn from(e: toml::de::Error) -> Self {
        ManifestError::Toml(e)
    }
}

/// The raw on-disk `plugin.toml` shape (design §5). Optional fields default to
/// empty / the documented default; the string tokens for `kind`/`load`/
/// `permissions` are mapped back through the existing
/// [`PluginKind::as_str`]/[`LoadType::as_str`]/[`Permission::as_str`] spellings
/// (so the schema and the round-trip tokens can never drift), rejecting any
/// unrecognized token with a structured [`ManifestError`].
#[derive(serde::Deserialize)]
struct RawManifest {
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    /// `kind = builtin|native|reclassnet|process`. Required by the schema, but a
    /// missing value defaults to `native` (the common third-party case) rather
    /// than failing — built-ins never ship a `plugin.toml`.
    #[serde(default)]
    kind: Option<String>,
    /// `entry = "…"` — the artifact filename (the C++ `dllFileName`). Recorded on
    /// the manifest as `dll_file_name` when present; otherwise the caller's
    /// discovered filename is used.
    #[serde(default)]
    entry: Option<String>,
    /// `load = auto|manual`; missing ⇒ [`LoadType::Auto`].
    #[serde(default)]
    load: Option<String>,
    /// `permissions = [..]`; missing ⇒ empty.
    #[serde(default)]
    permissions: Vec<String>,
}

/// Map a `kind` token back through [`PluginKind::as_str`] (rejecting unknowns).
fn parse_kind(tok: &str) -> Result<PluginKind, ManifestError> {
    for k in [
        PluginKind::Builtin,
        PluginKind::Native,
        PluginKind::ReclassNet,
        PluginKind::Process,
    ] {
        if k.as_str() == tok {
            return Ok(k);
        }
    }
    Err(ManifestError::UnknownKind(tok.to_string()))
}

/// Map a `load` token back through [`LoadType::as_str`] (rejecting unknowns).
fn parse_load(tok: &str) -> Result<LoadType, ManifestError> {
    for l in [LoadType::Auto, LoadType::Manual] {
        if l.as_str() == tok {
            return Ok(l);
        }
    }
    Err(ManifestError::UnknownLoad(tok.to_string()))
}

/// Map a permission token back through [`Permission::as_str`] (rejecting unknowns).
fn parse_permission(tok: &str) -> Result<Permission, ManifestError> {
    for p in [
        Permission::ReadMemory,
        Permission::WriteMemory,
        Permission::Network,
        Permission::Filesystem,
        Permission::AddUi,
        Permission::AddProvider,
    ] {
        if p.as_str() == tok {
            return Ok(p);
        }
    }
    Err(ManifestError::UnknownPermission(tok.to_string()))
}

impl PluginManifest {
    /// Parse a [`PluginManifest`] from `plugin.toml` text (design §5 schema, §6
    /// Phase 6). `dll_file_name` is the filename the discovery scan found beside
    /// the manifest; it is overridden by an explicit `entry = "…"` if present.
    ///
    /// Defaults: missing `load` ⇒ [`LoadType::Auto`], missing `permissions` ⇒
    /// empty, missing `kind` ⇒ [`PluginKind::Native`] (built-ins never carry a
    /// `plugin.toml`). Unknown `kind`/`load`/`permission` tokens surface as a
    /// structured [`ManifestError`] (design §7.A [fix]). The identifier stays
    /// **derived** from `name` (never read from the file) so it can never drift.
    pub fn from_toml_str(s: &str, dll_file_name: &str) -> Result<PluginManifest, ManifestError> {
        let raw: RawManifest = toml::from_str(s)?;

        let kind = match raw.kind.as_deref() {
            Some(tok) => parse_kind(tok)?,
            None => PluginKind::Native,
        };
        let load = match raw.load.as_deref() {
            Some(tok) => parse_load(tok)?,
            None => LoadType::Auto,
        };
        let permissions = raw
            .permissions
            .iter()
            .map(|p| parse_permission(p))
            .collect::<Result<Vec<_>, _>>()?;

        let dll_file_name = raw.entry.unwrap_or_else(|| dll_file_name.to_string());

        Ok(PluginManifest {
            name: raw.name,
            version: raw.version,
            author: raw.author,
            description: raw.description,
            kind,
            load,
            permissions,
            dll_file_name,
        })
    }

    /// Read + parse a `plugin.toml` file (design §6 Phase 6). The `dll_file_name`
    /// fallback (when the file has no `entry`) is the file's parent directory name
    /// — the `plugins/<name>/` bundle convention (design §5/§H) — so a manifest
    /// without an explicit `entry` still records a sensible artifact hint.
    pub fn from_toml_file(path: &std::path::Path) -> Result<PluginManifest, ManifestError> {
        let text = std::fs::read_to_string(path)?;
        let fallback = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        PluginManifest::from_toml_str(&text, fallback)
    }
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

    // ── `plugin.toml` parsing (Phase 6) ──

    #[test]
    fn from_toml_full_round_trip() {
        let toml = r#"
            name = "Remote Process Memory"
            version = "1.2.3"
            author = "Acme"
            description = "Reads a remote process."
            kind = "native"
            entry = "remote.so"
            load = "manual"
            permissions = ["read_memory", "write_memory", "add_provider"]
        "#;
        let m = PluginManifest::from_toml_str(toml, "ignored.so").unwrap();
        assert_eq!(m.name, "Remote Process Memory");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.author, "Acme");
        assert_eq!(m.description, "Reads a remote process.");
        assert_eq!(m.kind, PluginKind::Native);
        assert_eq!(m.load, LoadType::Manual);
        assert_eq!(
            m.permissions,
            vec![
                Permission::ReadMemory,
                Permission::WriteMemory,
                Permission::AddProvider
            ]
        );
        // `entry` overrides the discovered filename.
        assert_eq!(m.dll_file_name, "remote.so");
    }

    #[test]
    fn from_toml_identifier_still_derived_from_name() {
        // The identifier is NEVER read from the file — always derived (the [fix]
        // that keeps name + identifier from drifting).
        let m = PluginManifest::from_toml_str("name = \"My Cool Reader\"\n", "x.so").unwrap();
        assert_eq!(m.identifier(), "mycoolreader");
        assert_eq!(m.identifier(), derive_identifier(&m.name));
    }

    #[test]
    fn from_toml_applies_defaults() {
        // Only the required `name` present: load=Auto, permissions empty,
        // kind defaults to Native, dll_file_name falls back to the passed name.
        let m = PluginManifest::from_toml_str("name = \"Bare\"\n", "bare.so").unwrap();
        assert_eq!(m.load, LoadType::Auto);
        assert!(m.permissions.is_empty());
        assert_eq!(m.kind, PluginKind::Native);
        assert_eq!(m.dll_file_name, "bare.so");
        assert!(m.version.is_empty());
    }

    #[test]
    fn from_toml_reclassnet_kind_parses() {
        let toml = r#"
            name = "ReClass.NET Compat Layer"
            kind = "reclassnet"
            permissions = ["read_memory"]
        "#;
        let m = PluginManifest::from_toml_str(toml, "compat.dll").unwrap();
        assert_eq!(m.kind, PluginKind::ReclassNet);
        assert_eq!(m.identifier(), "reclass.netcompatlayer");
    }

    #[test]
    fn from_toml_unknown_tokens_surface_structured_errors() {
        let bad_kind = PluginManifest::from_toml_str("name=\"X\"\nkind=\"wasm\"\n", "x");
        assert!(matches!(
            bad_kind,
            Err(ManifestError::UnknownKind(k)) if k == "wasm"
        ));

        let bad_load = PluginManifest::from_toml_str("name=\"X\"\nload=\"lazy\"\n", "x");
        assert!(matches!(
            bad_load,
            Err(ManifestError::UnknownLoad(l)) if l == "lazy"
        ));

        let bad_perm = PluginManifest::from_toml_str("name=\"X\"\npermissions=[\"gpu\"]\n", "x");
        assert!(matches!(
            bad_perm,
            Err(ManifestError::UnknownPermission(p)) if p == "gpu"
        ));
    }

    #[test]
    fn from_toml_missing_name_is_a_toml_error() {
        // `name` is the one required field; its absence is a serde/TOML error,
        // not a panic.
        let err = PluginManifest::from_toml_str("version = \"1\"\n", "x");
        assert!(matches!(err, Err(ManifestError::Toml(_))));
    }

    #[test]
    fn from_toml_file_reads_and_uses_parent_dir_as_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "rcx-manifest-test-{}-{}",
            std::process::id(),
            "myplugin"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.toml");
        std::fs::write(&path, "name = \"My Plugin\"\nkind = \"native\"\n").unwrap();

        let m = PluginManifest::from_toml_file(&path).unwrap();
        assert_eq!(m.name, "My Plugin");
        // No `entry` ⇒ fall back to the bundle directory name.
        assert_eq!(m.dll_file_name, dir.file_name().unwrap().to_str().unwrap());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn detected_label_cases() {
        assert_eq!(detected_label(PluginKind::Builtin, false), "builtin");
        assert_eq!(detected_label(PluginKind::Native, false), "native");
        assert_eq!(detected_label(PluginKind::Process, false), "process");
        // The two ReClass.NET sub-paths are distinguished by `managed`.
        assert_eq!(
            detected_label(PluginKind::ReclassNet, false),
            "reclassnet-native"
        );
        assert_eq!(
            detected_label(PluginKind::ReclassNet, true),
            "reclassnet-managed"
        );
    }
}
