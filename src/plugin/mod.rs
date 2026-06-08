//! # Plugin system — the light, dependency-free CORE contract (Phase 1).
//!
//! This module is the host-side **plugin contract** of
//! `_design/plugin_system_design.md` §2/§3, plus the in-tree registry that wires
//! the built-in providers through it (design §6 Phase 1). It is an **always-on
//! engine module** (declared `pub mod plugin;` in `lib.rs`) and deliberately
//! **light**: std + `serde_json` (already core) + the existing
//! [`Provider`](crate::provider::Provider) trait — *no heavy deps*. The
//! stable-ABI machinery (`abi_stable`/`libloading`) is **Phase 3** and gates
//! behind the `plugins` cargo feature, so lean builds are unaffected.
//!
//! ## Map to the design + the C++ reference
//!
//! | Item | design | C++ (`_design/plugin_system_cpp_reference.md`) |
//! |---|---|---|
//! | [`Plugin`] trait | §2 | `IPlugin`/`IProviderPlugin` (`iplugin.h`) |
//! | [`Contribution`] | §2 | `Type()==ProviderPlugin` + `populatePluginMenu` |
//! | [`ProviderSpec`] | §1 table, §2 | `canHandle`/`createProvider`/`enumerateProcesses` |
//! | [`PluginManifest`] + [`derive_identifier`] | §5, §7.A [fix] | `Name/Version/…`; `Name().toLower().replace(" ","")` (cpp §2,§5,§10.6) |
//! | [`PluginHost`] / [`MockPluginHost`] | §2, §G | the host callback surface |
//! | [`ViewTree`] / [`UiEvent`] | §3 | (none — the "add UI" goal C++ never built) |
//! | [`DialogResult`] | §3 | the C++ `selectTarget` return (target string / reject) |
//! | [`PluginManager`] | §2, §6 | `PluginManager` (`pluginmanager.cpp`) |
//! | built-ins | §6 Phase 1 | the "File" source + the provider plugins |
//! | [`DemoPlugin`] | §6 Phase 2 | (none — the in-tree "add UI" demo deliverable) |
//!
//! ## Phase 2 — the declarative-UI host (design §6 Phase 2)
//!
//! Phase 2 renders a plugin's [`ViewTree`] into Zed-styled widgets and routes
//! [`UiEvent`]s back Elm-style. The contract grows only additively here:
//! [`DialogResult`] + the two no-op [`PluginHost`] hooks
//! ([`close_dialog`](PluginHost::close_dialog) /
//! [`request_rerender`](PluginHost::request_rerender)) + panic-guarded manager
//! event routing ([`PluginManager::handle_command`] /
//! [`handle_ui_event`](PluginManager::handle_ui_event)). The in-tree
//! [`DemoPlugin`] is the deliverable (a `Command`, a `Panel`, a `Dialog` that
//! re-expresses `select_target`); the host-side renderer lives behind the `ui`
//! feature in [`crate::ui::plugins::pluginview`] (+ `pluginpanel` / `plugindialog`).
//!
//! ## Phase 1 scope
//!
//! - Plain Rust types throughout (no `abi_stable` — design §2/§3 defer it).
//! - The built-ins (File/Buffer/Snapshot/Null, plus Process Memory with
//!   `memflow-provider`) flow through the same contract a native plugin will,
//!   registering into the existing
//!   [`ProviderRegistry`](crate::provider::ProviderRegistry) — design §7.A [fix]
//!   gives both source-picker surfaces one shared list and one identifier helper.
//! - Observable behavior is unchanged: the controller still attaches a buffered
//!   "File" provider; this is the registration/listing layer.

pub mod builtins;
pub mod contract;
pub mod demo;
pub mod host;
pub mod manager;
pub mod manifest;
pub mod provider_spec;
pub mod view;

// ── Phase 3: the native stable-ABI dynamic loader (design §6 Phase 3) ──
#[cfg(feature = "plugins")]
pub mod discovery;
#[cfg(feature = "plugins")]
pub mod loader;

// ── Phase 4: the ReClass.NET native compat subsystem (design §6 Phase 4) ──
// A CORE host subsystem (not a wrapped plugin) that bridges ReClass.NET's native
// CoreFunctions ABI into our `Provider`; the folder scan sniffs + routes here.
#[cfg(feature = "plugins")]
pub mod reclassnet;

// ── Public contract re-exports (design §2/§3) ──
pub use contract::{
    CommandResult, CommandSlot, Contribution, DialogResult, DockSide, Plugin, ProcessInfo,
};
pub use demo::DemoPlugin;
pub use host::{MockPluginHost, PluginHost};
pub use manager::{
    DiskPluginPersistence, MemPluginPersistence, PluginManager, PluginPersistence, PluginRow,
    UiContribution,
};
pub use manifest::{
    derive_identifier, detected_label, LoadType, ManifestError, Permission, PluginKind,
    PluginManifest,
};
pub use provider_spec::{ProviderSpec, SharedProvider};
pub use view::{TreeNode, UiEvent, ViewTree};

/// Native dynamic-library extension for the current compilation target.
///
/// Use the standard target constants instead of hand-written `cfg!(target_os)`
/// branches so tests and plugin discovery use the same platform naming rules as
/// Rust itself (`dll` / `so` / `dylib`).
#[cfg(feature = "plugins")]
pub(crate) fn platform_lib_extension() -> &'static str {
    std::env::consts::DLL_EXTENSION
}

/// Native dynamic-library filename prefix for the current target (`lib` on Unix,
/// empty on Windows).
#[cfg(all(test, feature = "plugins"))]
pub(crate) fn platform_lib_prefix() -> &'static str {
    std::env::consts::DLL_PREFIX
}

/// Native dynamic-library filename for a Cargo crate name or raw stem.
#[cfg(all(test, feature = "plugins"))]
pub(crate) fn platform_lib_filename(stem: &str) -> String {
    format!(
        "{}{}.{}",
        platform_lib_prefix(),
        stem.replace('-', "_"),
        platform_lib_extension()
    )
}

/// Locate a built example-plugin cdylib under the cargo target dir(s). Returns
/// `None` if it hasn't been built (so a load test skips rather than fails in
/// an environment where the example wasn't compiled). Shared by the loader /
/// manager / discovery test modules.
#[cfg(all(test, feature = "plugins"))]
pub(crate) fn example_plugin_path(crate_name: &str) -> Option<std::path::PathBuf> {
    let file = platform_lib_filename(crate_name);

    // CARGO_MANIFEST_DIR is the repo root (the `reclass` package).
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        root.join("target/debug").join(&file),
        root.join("target/release").join(&file),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_provider_count() -> usize {
        4 + usize::from(cfg!(feature = "process-provider"))
            + usize::from(cfg!(feature = "remote-process-provider"))
            + usize::from(cfg!(all(windows, feature = "kernel-provider")))
            + usize::from(cfg!(all(windows, feature = "windbg-provider")))
            + usize::from(cfg!(feature = "memflow-provider"))
    }

    /// The end-to-end Phase-1 vertical slice: build the manager with built-ins,
    /// confirm the registry the source picker reads is populated through the
    /// contract, and that an identifier round-trips to a real provider.
    #[test]
    fn phase1_vertical_slice() {
        let mgr = PluginManager::with_builtins();

        // The registry the source surfaces consume is the real one.
        let reg = mgr.registry();
        assert_eq!(reg.enabled_providers().count(), builtin_provider_count());
        assert!(reg.find("file").is_some());
        #[cfg(feature = "process-provider")]
        assert!(reg.find("processmemory").is_some());
        #[cfg(feature = "remote-process-provider")]
        assert!(reg.find("remoteprocessmemory").is_some());
        #[cfg(all(windows, feature = "kernel-provider"))]
        assert!(reg.find("kernelmemory").is_some());
        #[cfg(all(windows, feature = "windbg-provider"))]
        assert!(reg.find("windbgmemory").is_some());
        #[cfg(feature = "memflow-provider")]
        assert!(reg.find("memflowprocessmemory").is_some());

        // The centralized identifier helper agrees with the manifest path.
        assert_eq!(derive_identifier("File"), "file");

        // A buffer provider can be created through the contract (no file I/O).
        let prov = mgr.create_provider("buffer", "").expect("buffer");
        assert_eq!(prov.size(), 0);
    }

    /// The Phase-2 slice: the in-tree demo plugin contributes exactly one
    /// `Command`-pair, one `Panel`, and one `Dialog`, and its views enumerate
    /// for the declarative renderer (design §6 Phase 2 deliverable). Adding it
    /// does NOT change the Phase-1 registry (parity).
    #[test]
    fn phase2_demo_plugin_contributes_command_panel_dialog() {
        let demo = DemoPlugin::new();
        let contribs = demo.contributions();

        let n_command = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Command { .. }))
            .count();
        let n_panel = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Panel { .. }))
            .count();
        let n_dialog = contribs
            .iter()
            .filter(|c| matches!(c, Contribution::Dialog { .. }))
            .count();
        assert!(n_command >= 1, "at least one Command");
        assert_eq!(n_panel, 1, "exactly one Panel");
        assert_eq!(n_dialog, 1, "exactly one Dialog");

        // The renderer enumerates the panel + dialog views.
        let views = Contribution::view_ids(&contribs);
        assert_eq!(views, [demo::PANEL_ID, demo::DIALOG_ID]);

        // Parity: with_builtins is still the built-in provider set; the demo plugin
        // is additive and contributes no provider.
        let mgr = PluginManager::with_builtins_and_demo();
        assert_eq!(
            mgr.registry().enabled_providers().count(),
            builtin_provider_count()
        );
        assert!(mgr.find_plugin("file").is_some());
    }
}
