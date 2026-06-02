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
//! Scope (matches C++): **memory backends only.** This phase handles the **native**
//! path (cross-platform — design §I [+]); managed-C# plugins are Phase 5
//! (Windows-only CLR host), and a managed assembly seen by the scan is recorded as
//! a Phase-5 skip, not bridged here.
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

pub use bridge::{load_reclassnet_native, RcNetFunctions};
pub use provider::RcNetProvider;

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
