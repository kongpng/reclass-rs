//! Legacy native data-source stubs (kernel / remote / WinDbg / ReClass.NET).
//!
//! The original Reclass ships process / kernel / remote / WinDbg memory
//! providers (under the C++ `plugins/` tree) plus a ReClass.NET DLL/CLR compat
//! layer. Live **process** memory is implemented first-party in
//! [`crate::provider::memflow`]; the remaining ABI-shaped native providers
//! (kernel / remote / WinDbg) are represented only as documented stubs here,
//! gated by the off-by-default `native-plugins` feature.
//!
//! Every other subsystem talks to data sources purely through the abstract
//! [`Provider`](super::Provider) trait, so dropping another native source in
//! here changes nothing elsewhere.

use super::Provider;

/// Stub for a legacy native source (kernel / remote / WinDbg). Behaves like a
/// null source until a real implementation is dropped in. Constructing one is
/// the seam the `pluginmanager` / `ProviderRegistry` factory would use. (Live
/// process memory is provided first-party by [`crate::provider::memflow`].)
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
/// `libloading`. OUT OF SCOPE: the loader is only present behind
/// `native-plugins`; plugin *implementations* are never analyzed or built here.
#[cfg(feature = "native-plugins")]
pub fn load_provider_plugin(_path: &str) -> Result<(), String> {
    // The real loader would `libloading::Library::new(path)`, resolve the C
    // `CreatePlugin` symbol, and register the returned provider factory. Stub:
    // refuse, since plugin implementations are out of scope.
    Err("native provider plugins are out of scope in this port".to_string())
}
