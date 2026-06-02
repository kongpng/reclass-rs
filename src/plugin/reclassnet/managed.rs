//! `managed` — the Windows-only **managed (C#) ReClass.NET compat loader** (design
//! §4 managed path, §6 Phase 5, §8 decision; the C++ `loadManagedDll` +
//! `ClrHost::loadManagedPlugin`, reference §8).
//!
//! This is the orchestration layer over the `clr_host` (Windows-only) CLR host: it
//! locates the ported [`RcNetBridge.dll`](super::RCNET_BRIDGE_DLL), hosts the .NET
//! Framework CLR (once, process-wide), and drives the C# bridge to **populate** a
//! [`RcNetFunctions`](super::RcNetFunctions) table from the managed plugin's
//! `ICoreProcessFunctions`. From a populated table on, it is **byte-for-byte the
//! Phase-4 path**: the same `bridge::make_rcnet_plugin` builds the same
//! [`RcNetProvider`](super::RcNetProvider) under the same
//! [`RECLASSNET_IDENTIFIER`](super::RECLASSNET_IDENTIFIER).
//!
//! ## The bridge return code → outcome map (the C++ `loadManagedPlugin`, reference §8)
//!
//! The bridge's `Initialize` returns: **0** ok, **1** bad arg, **2** no
//! `ICoreProcessFunctions`, **3** load/deps fail, **4** other. The decided scope
//! (design §8) is **memory backends only**, so a managed assembly that loads but
//! exposes no `ICoreProcessFunctions` (a **node-type / UI plugin** like
//! `FrostbitePlugin`) returns **2**, which we map to a **logged "ReClass.NET
//! node-type plugin unsupported" skip** — [`ManagedSkip::NodeTypePlugin`]. We never
//! try to bridge node types / UI. This is the same outcome the C++ produces (its
//! compat only ever bridged the memory `CoreFunctions`).
//!
//! ## Table + host lifetime (the C++ gotcha 9)
//!
//! The C# thunks the bridge writes keep calling through the table, and there is **no
//! AppDomain unload**, so the table and the CLR host must outlive every provider.
//! We therefore [`Box::leak`] the table and keep the `clr_host::ClrHost` in a
//! process-static `OnceLock` — matching the C++, which likewise leaks. A future
//! safe-unload is out of scope.
//!
//! ## Build/run constraints
//!
//! The whole live path needs a real `.NET FW4` CLR **and** a compiled
//! `RcNetBridge.dll` (built from the vendored
//! [`bridge_cs/RcNetBridge.cs`](super::RCNET_BRIDGE_DLL) by `csc`/`dotnet`, shipped
//! beside the plugin — see `bridge_cs/README.md`). It therefore cannot build or run
//! on the Linux host, so the **live loader** (`load_reclassnet_managed`) and the
//! CLR plumbing live in a `#[cfg(windows)]` `imp` submodule, with a
//! `#[cfg(not(windows))]` Err stub in the parent module so discovery calls one
//! symbol unconditionally. The **pure logic is cross-platform** and lives at module
//! scope (the return-code → [`ManagedSkip`] map, the `"<hexptr>|<path>"` arg
//! formatting [`format_bridge_arg`], the manifest helper [`make_managed_manifest`],
//! [`bridge_dll_path`]); it is compiled + unit-tested on Linux — see the `tests`
//! module (which is **not** Windows-gated). The COM dance + a real C# plugin
//! round-trip are Windows-CI / manual only.
//!
//! Gated behind the `plugins` cargo feature (via the parent module).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::plugin::manifest::{LoadType, Permission, PluginKind, PluginManifest};
use crate::plugin::reclassnet::{RCNET_BRIDGE_DLL, RECLASSNET_NAME};

/// Why a managed-load attempt did not produce a bridged provider, derived from the
/// C# bridge's return code (the C++ `loadManagedPlugin` `switch (retVal)`, reference
/// §8). [`NodeTypePlugin`](ManagedSkip::NodeTypePlugin) is the **decided** scope
/// boundary (design §8): a node-type / UI plugin is detected (no
/// `ICoreProcessFunctions`) and skipped with a log, not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedSkip {
    /// Return code 1 — the bridge could not parse the `"<hexptr>|<path>"` argument.
    BadArgument,
    /// Return code 2 — the assembly loaded but exposes no `ICoreProcessFunctions`.
    /// **This is a ReClass.NET node-type / UI plugin** (e.g. `FrostbitePlugin`),
    /// which binds to ReClass.NET's managed rendering + node model our app can't
    /// run. Logged + skipped (design §8 decided scope: memory backends only).
    NodeTypePlugin,
    /// Return code 3 — the plugin assembly (or a dependency) failed to load.
    AssemblyLoadFailed,
    /// Return code 4 (or any other non-zero) — an unspecified bridge failure.
    Other(u32),
}

impl ManagedSkip {
    /// Map the bridge's `Initialize` return code to a skip reason. **Only** non-zero
    /// codes are skips; `0` is success and is handled by the caller (so this is
    /// `from_nonzero` — passing `0` is a programmer error, mapped to `Other(0)` for
    /// total-ness rather than panicking).
    pub fn from_return_code(code: u32) -> ManagedSkip {
        match code {
            1 => ManagedSkip::BadArgument,
            2 => ManagedSkip::NodeTypePlugin,
            3 => ManagedSkip::AssemblyLoadFailed,
            other => ManagedSkip::Other(other),
        }
    }

    /// Whether this skip is the **node-type / UI plugin** case (return code 2) — the
    /// one the loader logs as "unsupported" and treats as a benign skip rather than
    /// a hard error (design §8).
    pub fn is_node_type_plugin(&self) -> bool {
        matches!(self, ManagedSkip::NodeTypePlugin)
    }
}

impl fmt::Display for ManagedSkip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManagedSkip::BadArgument => {
                f.write_str("The .NET bridge rejected the argument format (internal error).")
            }
            // The required, decided-scope log line (design §4/§8).
            ManagedSkip::NodeTypePlugin => f.write_str(
                "ReClass.NET node-type plugin unsupported: the .NET assembly exposes no \
                 ICoreProcessFunctions (memory backend). Node-type / UI plugins bind to \
                 ReClass.NET's managed rendering and are skipped (memory backends only).",
            ),
            ManagedSkip::AssemblyLoadFailed => f.write_str(
                "Failed to load the .NET plugin assembly. Check that all its dependencies \
                 are present beside it.",
            ),
            ManagedSkip::Other(code) => {
                write!(f, "The .NET bridge returned error code {code}.")
            }
        }
    }
}

/// Format the bridge `Initialize` argument string: `"<hexptr>|<pluginpath>"` where
/// `<hexptr>` is the lowercase hex address of the native
/// [`RcNetFunctions`](super::RcNetFunctions) table (the C++
/// `swprintf(L"%llx|%ls", table, pluginPath)`, reference §8). Split out so it is
/// unit-testable without a CLR (pure string formatting).
///
/// `table_addr` is the table pointer cast to `usize` (the bridge `long.Parse(...,
/// HexNumber)`s it back into an `IntPtr`); `plugin_path` is the plugin DLL path in
/// native separators.
pub fn format_bridge_arg(table_addr: usize, plugin_path: &str) -> String {
    format!("{table_addr:x}|{plugin_path}")
}

/// The manifest for a bridged **managed** ReClass.NET plugin — identical in shape to
/// the native one (same [`RECLASSNET_NAME`] → [`RECLASSNET_IDENTIFIER`](super::RECLASSNET_IDENTIFIER)),
/// only the description differs. Split out so the (cross-platform) identifier
/// invariant is testable on Linux.
pub fn make_managed_manifest(dll_file_name: impl Into<String>) -> PluginManifest {
    let dll_file_name = dll_file_name.into();
    PluginManifest {
        name: RECLASSNET_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        author: "Reclass (Rust port) — ReClass.NET managed (C#) compat".to_string(),
        description: format!("Bridged ReClass.NET managed (.NET) plugin ({dll_file_name})."),
        kind: PluginKind::ReclassNet,
        load: LoadType::Auto,
        permissions: vec![
            Permission::ReadMemory,
            Permission::WriteMemory,
            Permission::AddProvider,
        ],
        dll_file_name,
    }
}

/// Locate the bridge DLL beside a plugin (the C++ `applicationDirPath()/Plugins/
/// RcNetBridge.dll`, reference §8 — adapted to our folder-scan model: the bridge
/// ships in the **same `plugins/` directory** as the managed plugin). Returns the
/// candidate path (existence is checked by the caller so it can surface a precise
/// "bridge missing" error). Split out for testability.
pub fn bridge_dll_path(plugin_path: &Path) -> PathBuf {
    match plugin_path.parent() {
        Some(dir) => dir.join(RCNET_BRIDGE_DLL),
        None => PathBuf::from(RCNET_BRIDGE_DLL),
    }
}

// ── The Windows-only live loader ──────────────────────────────────────────────
//
// Everything below needs the `windows` crate + a real CLR + the compiled bridge,
// so it is `#[cfg(windows)]`. The Linux build sees only the pure helpers above (and
// the parent module's `#[cfg(not(windows))]` stub).

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use super::*;
    use crate::plugin::contract::Plugin;
    use crate::plugin::reclassnet::bridge::{make_rcnet_plugin, KeepAlive, RcNetFunctions};
    use crate::plugin::reclassnet::clr_host::ClrHost;

    /// The process-wide CLR host (one per process — the C++ keeps one `ClrHost` for
    /// the plugin's lifetime; we make it process-static since the leaked tables keep
    /// calling through it forever, the gotcha-9 leak). Started lazily on first
    /// managed load.
    static CLR_HOST: OnceLock<Result<ClrHost, String>> = OnceLock::new();

    /// Get (starting on first use) the process-wide CLR host, or the surfaced error
    /// from starting it. The `Result` is cached so a machine without .NET FW4 fails
    /// fast on every managed plugin with the same message (the C++
    /// `isAvailable()` gate, reference §8).
    fn clr_host() -> Result<&'static ClrHost, String> {
        CLR_HOST
            .get_or_init(|| {
                ClrHost::start().map_err(|e| {
                    format!(
                        ".NET Framework 4.x is not available on this machine \
                         (CLR start failed: {e}). Install the .NET Framework 4.7.2+ \
                         runtime to load managed ReClass.NET plugins."
                    )
                })
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// Load a managed (.NET) ReClass.NET plugin (the C++ `loadManagedDll`, reference
    /// §8). On the decided node-type-plugin case (bridge return code 2) this logs
    /// the required "ReClass.NET node-type plugin unsupported" line and returns the
    /// [`ManagedSkip`] as an `Err` (a benign skip the discovery layer surfaces as a
    /// skip, not a crash).
    pub fn load_reclassnet_managed(path: &Path) -> Result<Box<dyn Plugin>, String> {
        // 1. Host (or reuse) the CLR.
        let host = clr_host()?;

        // 2. Locate the ported bridge DLL beside the plugin; a missing bridge is a
        //    clear error, not a crash (risk 6).
        let bridge = bridge_dll_path(path);
        if !bridge.is_file() {
            return Err(format!(
                "{RCNET_BRIDGE_DLL} not found beside the plugin (expected at: {}). \
                 Build it from bridge_cs/RcNetBridge.cs and ship it in the plugins folder.",
                bridge.display()
            ));
        }

        // 3. Allocate + LEAK a zeroed function table the bridge will populate (the
        //    C++ `memset(outFunctions, 0, …)`; we leak because the thunks keep
        //    calling through it and there is no AppDomain unload — gotcha 9).
        let table: &'static mut RcNetFunctions = Box::leak(Box::new(RcNetFunctions::empty()));
        let table_addr = table as *mut RcNetFunctions as usize;

        // 4. Build "<hexptr>|<pluginpath>" and run the bridge in the default
        //    AppDomain (native separators for the CLR — the C++
        //    `QDir::toNativeSeparators`).
        let plugin_path_native = native_separators(path);
        let arg = format_bridge_arg(table_addr, &plugin_path_native);
        let code = host
            .execute_in_default_app_domain(&bridge, "RcNetBridge.Bridge", "Initialize", &arg)
            .map_err(|e| {
                format!(
                    "Failed to execute the .NET bridge (HRESULT {e:?}). Bridge: {}, Plugin: {}",
                    bridge.display(),
                    path.display()
                )
            })?;

        // 5. Map the return code (the C++ `switch (retVal)`).
        if code != 0 {
            let skip = ManagedSkip::from_return_code(code);
            // The decided node-type / UI plugin skip: log the required line.
            if skip.is_node_type_plugin() {
                tracing::info!(target: "reclass::plugin", plugin = %path.display(), "{skip}");
            }
            return Err(skip.to_string());
        }

        // 6. Success: the bridge must have written the 4 required pointers (the C++
        //    post-check). `validate_required` reuses the Phase-4 validation.
        table.validate_required().map_err(|missing| {
            format!("The .NET bridge loaded but did not provide the required functions: {missing}")
        })?;

        // 7. From here it is the byte-for-byte Phase-4 path: the shared builder
        //    wraps the populated (leaked) table as the host plugin. Nothing
        //    per-plugin to keep alive (the table is leaked, the host is static).
        let dll_file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(make_rcnet_plugin(
            make_managed_manifest(dll_file_name),
            *table,
            KeepAlive::Leaked,
        ))
    }

    /// Convert a path to native (Windows `\\`) separators for the CLR (the C++
    /// `QDir::toNativeSeparators`, reference §8).
    fn native_separators(path: &Path) -> String {
        path.to_string_lossy().replace('/', "\\")
    }
}

#[cfg(windows)]
pub use imp::load_reclassnet_managed;

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge return-code → [`ManagedSkip`] map matches the C++
    /// `loadManagedPlugin` switch exactly (reference §8), and code 2 is the
    /// node-type-plugin case (the decided-scope skip, design §8).
    #[test]
    fn return_code_maps_to_skip_reasons() {
        assert_eq!(ManagedSkip::from_return_code(1), ManagedSkip::BadArgument);
        assert_eq!(
            ManagedSkip::from_return_code(2),
            ManagedSkip::NodeTypePlugin
        );
        assert_eq!(
            ManagedSkip::from_return_code(3),
            ManagedSkip::AssemblyLoadFailed
        );
        assert_eq!(ManagedSkip::from_return_code(4), ManagedSkip::Other(4));
        assert_eq!(ManagedSkip::from_return_code(99), ManagedSkip::Other(99));

        // Only code 2 is the node-type plugin skip.
        assert!(ManagedSkip::from_return_code(2).is_node_type_plugin());
        assert!(!ManagedSkip::from_return_code(1).is_node_type_plugin());
        assert!(!ManagedSkip::from_return_code(4).is_node_type_plugin());
    }

    /// The node-type-plugin skip carries the **required** "ReClass.NET node-type
    /// plugin unsupported" message (design §4/§8 — this exact phrasing is the
    /// logged skip line). The other reasons read as actionable errors.
    #[test]
    fn node_type_skip_message_is_the_required_unsupported_line() {
        let msg = ManagedSkip::NodeTypePlugin.to_string();
        assert!(
            msg.contains("ReClass.NET node-type plugin unsupported"),
            "got: {msg}"
        );
        assert!(msg.contains("ICoreProcessFunctions"), "got: {msg}");

        assert!(ManagedSkip::AssemblyLoadFailed
            .to_string()
            .contains("dependencies"));
        assert!(ManagedSkip::BadArgument.to_string().contains("argument"));
        assert_eq!(
            ManagedSkip::Other(7).to_string(),
            "The .NET bridge returned error code 7."
        );
    }

    /// `format_bridge_arg` produces `"<lowercase-hex-ptr>|<path>"` exactly as the
    /// C++ `swprintf(L"%llx|%ls", …)` (reference §8) and as the C# bridge parses
    /// (`long.Parse(hex, HexNumber)` of the part before `'|'`).
    #[test]
    fn bridge_arg_is_hexptr_pipe_path() {
        // A representative pointer value; %llx is lowercase, no "0x".
        let arg = format_bridge_arg(0xdead_beef, r"C:\plugins\memflow_managed.dll");
        assert_eq!(arg, r"deadbeef|C:\plugins\memflow_managed.dll");

        // Round-trips the way the C# bridge reads it: split on the FIRST '|',
        // hex-parse the left, take the right as the path (a path can't contain '|'
        // on Windows, matching the C++ assumption).
        let sep = arg.find('|').unwrap();
        let parsed_ptr = usize::from_str_radix(&arg[..sep], 16).unwrap();
        assert_eq!(parsed_ptr, 0xdead_beef);
        assert_eq!(&arg[sep + 1..], r"C:\plugins\memflow_managed.dll");

        // Zero pointer formats as a bare "0".
        assert_eq!(format_bridge_arg(0, "x"), "0|x");
    }

    /// The managed manifest derives to the SAME routing identifier the native path
    /// and the source picker use ([`RECLASSNET_IDENTIFIER`](super::super::RECLASSNET_IDENTIFIER)).
    /// This is the invariant that lets the managed + native paths share
    /// `make_rcnet_plugin` and register interchangeably.
    #[test]
    fn managed_manifest_uses_the_reclassnet_identifier() {
        use crate::plugin::reclassnet::RECLASSNET_IDENTIFIER;
        let m = make_managed_manifest("memflow_managed.dll");
        assert_eq!(m.identifier(), RECLASSNET_IDENTIFIER);
        assert_eq!(m.kind, PluginKind::ReclassNet);
        assert!(m.permissions.contains(&Permission::ReadMemory));
        assert!(m.description.contains("memflow_managed.dll"));
        assert!(m.description.contains("managed"));
    }

    /// `bridge_dll_path` puts the bridge DLL beside the plugin (our folder-scan
    /// model — the bridge ships in the same `plugins/` dir as the managed plugin).
    #[test]
    fn bridge_dll_sits_beside_the_plugin() {
        let p = bridge_dll_path(Path::new("/opt/app/plugins/memflow_managed.dll"));
        assert_eq!(p, Path::new("/opt/app/plugins").join(RCNET_BRIDGE_DLL));
        assert_eq!(p.file_name().unwrap(), RCNET_BRIDGE_DLL);

        // A bare filename (no parent) falls back to just the bridge name.
        let bare = bridge_dll_path(Path::new("memflow_managed.dll"));
        assert_eq!(bare, Path::new(RCNET_BRIDGE_DLL));
    }
}
