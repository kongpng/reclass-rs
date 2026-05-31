//! Live OS data sources — **OUT OF SCOPE** documented stubs.
//!
//! The original Reclass ships process / kernel / remote / WinDbg memory
//! providers (under the C++ `plugins/` tree) plus a ReClass.NET DLL/CLR compat
//! layer. Per the port's scope note these are **not** implemented here: they are
//! represented only as documented stubs behind the [`Provider`](super::Provider)
//! trait, gated by the off-by-default `native-plugins` feature.
//!
//! Reaching real implementations is blocked by Anthropic's automated cyber
//! safeguard; they are unlockable via the Cyber Verification Program or by
//! supplying your own code, without touching any other module. Every other
//! subsystem talks to data sources purely through the abstract `Provider`
//! trait, so dropping a real native source in here changes nothing elsewhere.

use super::Provider;

/// Stub for a live OS source (process / kernel / remote / WinDbg). Behaves like
/// a null source until a real implementation is dropped in. Constructing one is
/// the seam the `pluginmanager` / `ProviderRegistry` factory would use.
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
