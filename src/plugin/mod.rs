//! # Plugin system — the light, dependency-free CORE contract (Phase 1).
//!
//! This module is the host-side **plugin contract** of
//! `_design/plugin_system_design.md` §2/§3, plus the in-tree registry that wires
//! the built-in providers through it (design §6 Phase 1). It is an **always-on
//! engine module** (declared `pub mod plugin;` in `lib.rs`) and deliberately
//! **light**: std + `serde_json` (already core) + the existing
//! [`Provider`](crate::provider::Provider) trait — *no heavy deps*. The
//! stable-ABI machinery (`abi_stable`/`libloading`) is **Phase 3** and gates
//! behind a future `plugins` cargo feature, so default builds are unaffected.
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
//! | [`PluginManager`] | §2, §6 | `PluginManager` (`pluginmanager.cpp`) |
//! | built-ins | §6 Phase 1 | the "File" source + the provider plugins |
//!
//! ## Phase 1 scope
//!
//! - Plain Rust types throughout (no `abi_stable` — design §2/§3 defer it).
//! - The built-ins (File/Buffer/Snapshot/Null) flow through the same contract a
//!   native plugin will, registering into the existing
//!   [`ProviderRegistry`](crate::provider::ProviderRegistry) — design §7.A [fix]
//!   gives both source-picker surfaces one shared list and one identifier helper.
//! - Observable behavior is unchanged: the controller still attaches a buffered
//!   "File" provider; this is the registration/listing layer.

pub mod builtins;
pub mod contract;
pub mod host;
pub mod manager;
pub mod manifest;
pub mod provider_spec;
pub mod view;

// ── Public contract re-exports (design §2/§3) ──
pub use contract::{CommandResult, CommandSlot, Contribution, DockSide, Plugin, ProcessInfo};
pub use host::{MockPluginHost, PluginHost};
pub use manager::PluginManager;
pub use manifest::{derive_identifier, LoadType, Permission, PluginKind, PluginManifest};
pub use provider_spec::{ProviderSpec, SharedProvider};
pub use view::{TreeNode, UiEvent, ViewTree};

#[cfg(test)]
mod tests {
    use super::*;

    /// The end-to-end Phase-1 vertical slice: build the manager with built-ins,
    /// confirm the registry the source picker reads is populated through the
    /// contract, and that an identifier round-trips to a real provider.
    #[test]
    fn phase1_vertical_slice() {
        let mgr = PluginManager::with_builtins();

        // The registry the source surfaces consume is the real one.
        let reg = mgr.registry();
        assert_eq!(reg.enabled_providers().count(), 4);
        assert!(reg.find("file").is_some());

        // The centralized identifier helper agrees with the manifest path.
        assert_eq!(derive_identifier("File"), "file");

        // A buffer provider can be created through the contract (no file I/O).
        let prov = mgr.create_provider("buffer", "").expect("buffer");
        assert_eq!(prov.size(), 0);
    }
}
