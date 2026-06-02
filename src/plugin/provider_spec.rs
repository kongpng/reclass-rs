//! `ProviderSpec` — the provider capability a [`Contribution::Provider`] carries
//! (design §1 parity table, §2).
//!
//! This is the Rust shape of the C++ `IProviderPlugin` provider-creation surface
//! (cpp_reference §1):
//!
//! | C++ `IProviderPlugin` | here |
//! |---|---|
//! | `canHandle(target) -> bool` | [`ProviderSpec::can_handle`] |
//! | `createProvider(target, &err) -> unique_ptr<Provider>` | [`ProviderSpec::create_provider`] |
//! | `enumerateProcesses()` | [`ProviderSpec::enumerate_processes`] |
//! | `getInitialBaseAddress(target)` | **dropped** — design §7.A [fix]: the host uses `Provider::base()` (C++ declared it, never called it) |
//!
//! The created object is the existing [`Provider`](crate::provider::Provider)
//! trait object — nothing sits between the plugin and the metal (design §1). It is
//! returned as `Arc<dyn Provider + Send + Sync>` to match the controller's
//! `doc.provider` ownership (`Arc<dyn Provider + Send + Sync>`).
//!
//! Phase 1 keeps this as plain boxed closures (no `abi_stable`); native dynamic
//! plugins wrap their own across the stable ABI in Phase 3.

use std::sync::Arc;

use crate::plugin::contract::ProcessInfo;
use crate::provider::Provider;

/// A boxed provider trait object as the controller holds it.
pub type SharedProvider = Arc<dyn Provider + Send + Sync>;

/// The signature a provider factory uses: given a target string, build a provider
/// or report why it failed (the C++ `createProvider(target, QString* errorMsg)` —
/// design §7.A [fix] surfaces the error string instead of "check the console").
type CreateFn = Box<dyn Fn(&str) -> Result<SharedProvider, String> + Send + Sync>;
type CanHandleFn = Box<dyn Fn(&str) -> bool + Send + Sync>;
type EnumerateFn = Box<dyn Fn() -> Option<Vec<ProcessInfo>> + Send + Sync>;

/// The provider contribution (design §2 `Contribution::Provider(ProviderSpec)`).
///
/// Holds the factory closures rather than a sub-trait so a built-in can be a tiny
/// inline construction (the Phase 1 built-ins) and a native plugin can forward
/// across the ABI (Phase 3) — both satisfy the same shape.
pub struct ProviderSpec {
    can_handle: CanHandleFn,
    create: CreateFn,
    enumerate: Option<EnumerateFn>,
}

impl ProviderSpec {
    /// Build a spec from its `can_handle` + `create_provider` closures. Process
    /// enumeration is opt-in via [`with_enumerate`](ProviderSpec::with_enumerate)
    /// (most built-ins do not enumerate processes).
    pub fn new(
        can_handle: impl Fn(&str) -> bool + Send + Sync + 'static,
        create: impl Fn(&str) -> Result<SharedProvider, String> + Send + Sync + 'static,
    ) -> Self {
        ProviderSpec {
            can_handle: Box::new(can_handle),
            create: Box::new(create),
            enumerate: None,
        }
    }

    /// Attach a process-enumeration closure (the C++ `enumerateProcesses()`;
    /// the host renders the picker — cpp_reference §5, design §1).
    pub fn with_enumerate(
        mut self,
        enumerate: impl Fn() -> Option<Vec<ProcessInfo>> + Send + Sync + 'static,
    ) -> Self {
        self.enumerate = Some(Box::new(enumerate));
        self
    }

    /// `canHandle(target)` (cpp_reference §1) — whether this provider accepts the
    /// target string.
    pub fn can_handle(&self, target: &str) -> bool {
        (self.can_handle)(target)
    }

    /// `createProvider(target, &err)` (cpp_reference §1) — build the provider or
    /// return a surfaced error string (design §7.A [fix]).
    pub fn create_provider(&self, target: &str) -> Result<SharedProvider, String> {
        (self.create)(target)
    }

    /// `enumerateProcesses()` (cpp_reference §1). `None` = this provider does not
    /// provide a process list (the host then opens a file/target dialog instead).
    pub fn enumerate_processes(&self) -> Option<Vec<ProcessInfo>> {
        self.enumerate.as_ref().and_then(|f| f())
    }

    /// Whether this provider advertises a process list (the C++
    /// `providesProcessList()` — here derived from whether an enumerate closure
    /// was supplied, so the two can't drift).
    pub fn provides_process_list(&self) -> bool {
        self.enumerate.is_some()
    }
}

impl std::fmt::Debug for ProviderSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSpec")
            .field("provides_process_list", &self.provides_process_list())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::BufferProvider;

    #[test]
    fn create_provider_builds_a_real_provider() {
        let spec = ProviderSpec::new(
            |t| !t.is_empty(),
            |t| Ok(Arc::new(BufferProvider::new(vec![1, 2, 3], t)) as SharedProvider),
        );
        assert!(spec.can_handle("x.bin"));
        assert!(!spec.can_handle(""));

        let p = match spec.create_provider("x.bin") {
            Ok(p) => p,
            Err(e) => panic!("create: {e}"),
        };
        assert_eq!(p.size(), 3);
        assert_eq!(p.name(), "x.bin");
        let mut buf = [0u8; 2];
        assert!(p.read(0, &mut buf));
        assert_eq!(buf, [1, 2]);

        // No enumerate closure → no process list.
        assert!(!spec.provides_process_list());
        assert_eq!(spec.enumerate_processes(), None);
    }

    #[test]
    fn create_provider_surfaces_errors() {
        let spec = ProviderSpec::new(|_| true, |_| Err("no such target".to_string()));
        let err = match spec.create_provider("bad") {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err, "no such target");
    }

    #[test]
    fn enumerate_processes_when_supplied() {
        let spec = ProviderSpec::new(
            |_| true,
            |_| Ok(Arc::new(BufferProvider::default()) as SharedProvider),
        )
        .with_enumerate(|| {
            Some(vec![ProcessInfo {
                pid: 7,
                name: "p.exe".to_string(),
                path: String::new(),
                is_32bit: false,
            }])
        });
        assert!(spec.provides_process_list());
        let procs = spec.enumerate_processes().unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, 7);
    }
}
