//! `reclassnet` — the ReClass.NET **native** compatibility subsystem (design §4,
//! §6 Phase 4; the C++ `RcNetPluginCompatLayer`, reference §8).
//!
//! ## What this is (and is not)
//!
//! The C++ ships ReClass.NET compat **as a plugin** the user points at a `.dll`
//! via a file dialog (reference §8). **We do it better: a CORE host subsystem.**
//! The folder scan ([`crate::plugin::discovery`]) sniffs each candidate `.so`/
//! `.dll` and, for one exporting the 8 ReClass.NET CoreFunctions, routes it here —
//! no compat *plugin*, no file dialog. This module bridges that native
//! **CoreFunctions** ABI (the memory-backend subset; no debug/breakpoint) into our
//! existing [`Provider`](crate::provider::Provider), registered under the
//! identifier the UI already keys its icon/label off (`reclass.netcompatlayer`).
//!
//! Scope (matches C++): **memory backends only.** The **native** path is
//! cross-platform (design §I [+]); the **managed-C#** path (Phase 5,
//! [`managed`] + [`clr_host`]) is **Windows-only** — it hosts the .NET Framework
//! CLR and bridges a managed plugin's `ICoreProcessFunctions` into the **same**
//! [`RcNetFunctions`] table → the **same** [`RcNetProvider`]. A managed assembly
//! that loads but exposes no `ICoreProcessFunctions` (a **node-type / UI plugin**)
//! is **detected + skipped** with a logged "ReClass.NET node-type plugin
//! unsupported" ([`ManagedSkip::NodeTypePlugin`]). On non-Windows the managed entry
//! is a `#[cfg(not(windows))]` Err stub.
//!
//! ## Map to the C++ reference §8 (file:line into `/home/loke/Documents/Reclass`)
//!
//! | Item | C++ (`plugins/RcNetPluginCompatLayer/…`) |
//! |---|---|
//! | [`ffi`] (the 8-function ABI + packed structs) | `ReClassNET_Plugin.hpp:1-141` |
//! | [`RcNetFunctions`] + `resolve` (4 required exports) | `RcNetCompatPlugin.cpp:86-132` (`loadNativeDll`) |
//! | [`load_reclassnet_native`] (the router entry) | `RcNetCompatPlugin.cpp:64-132` (`loadPlugin`/`loadNativeDll`) |
//! | [`RcNetProvider`] (`Provider` ← CoreFunctions) | `RcNetCompatProvider.cpp:1-132` |
//! | target string `"dllpath\|pid:name"` | `RcNetCompatPlugin.cpp:193-237` (`canHandle`/`createProvider`) |
//! | `thread_local` enumeration collectors | `RcNetCompatPlugin.cpp:294-326`, `RcNetCompatProvider.cpp:88-132` |
//! | [`RECLASSNET_IDENTIFIER`] | `RcNetCompatPlugin` Name → `reclass.netcompatlayer` (reference §8) |
//! | [`clr_host`] (the CLR-host COM sequence, Windows) | `ClrHost.cpp` (`startClr`) |
//! | [`managed`] (`load_reclassnet_managed`, return-code map) | `ClrHost.cpp` (`loadManagedPlugin`) + `RcNetCompatPlugin.cpp:147-189` (`loadManagedDll`) |
//! | [`RCNET_BRIDGE_DLL`] / `bridge/RcNetBridge.cs` (the C# bridge) | `bridge/RcNetBridge.cs` (ported verbatim) |
//!
//! ## The [fix]es over C++ (design §7.A)
//!
//! 1. [`RcNetProvider::pointer_size`](provider::RcNetProvider) returns 4 or 8 from
//!    the target's detected bitness (C++ hardcodes 8).
//! 2. [`RcNetProvider::enumerate_regions`](provider::RcNetProvider) is real, built
//!    from the section callback (C++ discarded sections).
//! 3. [`RcNetProvider::size`](provider::RcNetProvider) returns the main module's
//!    region span (C++ returns a `0x10000` sentinel).
//!
//! Gated behind the `plugins` cargo feature (declared from
//! [`crate::plugin`](crate::plugin)).

pub mod bridge;
pub mod ffi;
pub mod provider;

// ── Phase 5: the managed (C#) compat path (design §6 Phase 5, Windows-only) ──
// The cross-platform pieces (the return-code → skip map, the arg formatter, the
// manifest helper) live in `managed`; the live CLR host (`clr_host`) + the live
// `load_reclassnet_managed` are additionally `#[cfg(windows)]`. On Linux the parent
// exposes a `#[cfg(not(windows))]` Err stub so discovery calls one symbol.
#[cfg(windows)]
pub mod clr_host;
pub mod managed;

pub use bridge::{load_reclassnet_native, RcNetFunctions};
pub use managed::ManagedSkip;
pub use provider::RcNetProvider;

#[cfg(windows)]
pub use managed::load_reclassnet_managed;

/// Load a managed (.NET) ReClass.NET plugin — **the Linux stub** (design §6 Phase 5
/// is Windows-only). On non-Windows there is no .NET Framework CLR to host, so this
/// returns a surfaced error and discovery records it as a skip. The real
/// implementation ([`managed::load_reclassnet_managed`]) is `#[cfg(windows)]`; this
/// stub lets [`discovery`](crate::plugin::discovery) call one symbol unconditionally
/// (no `cfg` at the call site).
#[cfg(not(windows))]
pub fn load_reclassnet_managed(
    _path: &std::path::Path,
) -> Result<Box<dyn crate::plugin::contract::Plugin>, String> {
    Err("managed ReClass.NET compat is Windows-only (needs the .NET Framework CLR)".to_string())
}

/// The ported C# bridge assembly's file name (design §4 managed path, §8; the C++
/// `RcNetBridge.dll`, reference §8). Located beside the managed plugin in the
/// `plugins/` folder; built from the vendored `bridge_cs/RcNetBridge.cs` (see
/// `bridge_cs/README.md` for the build + shipping step).
pub const RCNET_BRIDGE_DLL: &str = "RcNetBridge.dll";

/// The .NET Framework runtime version the CLR host requests (design §4 managed
/// path, §8; the C++ `ClrHost::startClr` `GetRuntime(L"v4.0.30319")`, reference §8).
pub const CLR_VERSION: &str = "v4.0.30319";

/// The display name of the bridged source (the C++ `RcNetCompatPlugin` `Name()`,
/// reference §8). [`derive_identifier`](crate::plugin::manifest::derive_identifier)
/// turns this into [`RECLASSNET_IDENTIFIER`] — the routing key the source picker
/// already keys its icon/label off (`src/ui/sourcechooser.rs`).
pub const RECLASSNET_NAME: &str = "ReClass.NET Compat Layer";

/// The routing identifier a bridged ReClass.NET source registers under (reference
/// §8, design §4). Exactly `derive_identifier(RECLASSNET_NAME)` — the dot survives
/// because the helper drops spaces only (see the round-trip test). The UI's
/// `source_icon`/`kind_label_for` already map this to `plug.svg` / "Compat".
pub const RECLASSNET_IDENTIFIER: &str = "reclass.netcompatlayer";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::derive_identifier;

    /// The identity round-trips: `derive_identifier(NAME) == IDENTIFIER`, so a
    /// bridged source registers under exactly the key the UI icon/label table
    /// (and the C++ reference §8) uses. This is the one invariant the whole
    /// subsystem hangs off.
    #[test]
    fn name_derives_to_the_identifier() {
        assert_eq!(derive_identifier(RECLASSNET_NAME), RECLASSNET_IDENTIFIER);
    }
}
