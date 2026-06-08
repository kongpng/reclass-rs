//! Legacy native-plugin loader seam.
//!
//! The original Reclass ships process / kernel / remote / WinDbg memory
//! providers under the C++ `plugins/` tree, plus a ReClass.NET DLL/CLR compat
//! layer. The first-party Rust providers live in [`crate::provider::process`],
//! [`crate::provider::remote`], [`crate::provider::kernel`],
//! [`crate::provider::windbg`], and [`crate::provider::memflow`]. This module is
//! only the optional dynamic-loader hook for external native provider plugins.
//!
//! Every other subsystem talks to data sources purely through the abstract
//! [`Provider`](super::Provider) trait, so dropping another native source in
//! here changes nothing elsewhere.

use super::Provider;

/// Placeholder provider used by tests and dynamic-loader failure paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProviderStub;

impl Provider for NativeProviderStub {
    fn size(&self) -> i32 {
        0
    }
    fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
        false
    }
    fn kind(&self) -> String {
        "Process".to_string()
    }
}

/// `pluginmanager.cpp` entrypoint resolution — `extern "C" CreatePlugin()` via
/// `libloading`. The first-party provider parity work is implemented in Rust
/// modules; this hook is reserved for loading third-party native providers.
#[cfg(feature = "native-plugins")]
pub fn load_provider_plugin(_path: &str) -> Result<(), String> {
    // The real loader would `libloading::Library::new(path)`, resolve the C
    // `CreatePlugin` symbol, and register the returned provider factory.
    Err("native provider plugin loading is not implemented yet".to_string())
}
