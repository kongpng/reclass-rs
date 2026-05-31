//! Name providers — the pluggable `{name, address}` aggregation layer.
//!
//! Port of `src/names/*`: [`NamedAddress`] (`name_provider.h:21`),
//! [`NameProvider`] (`name_provider.h:42` + `name_provider.cpp`), and
//! [`NameRegistry`] (`name_registry.{h,cpp}`). The concrete providers live in
//! the [`pdb`], [`rtti`], and [`bookmark`] submodules.

pub mod bookmark;
pub mod pdb;
pub mod rtti;

use std::sync::{Arc, Mutex, OnceLock};

use crate::provider::Provider;

/// `struct NamedAddress` (`name_provider.h:21`). One resolvable `{name, address}`
/// pair contributed by some [`NameProvider`]. `address == 0` means "no live
/// address" (renders `—`, not navigable).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedAddress {
    /// canonical identifier — used for reverse lookups.
    pub name: String,
    /// humanised form; empty -> consumers fall back to `name`.
    pub display_name: String,
    /// ABSOLUTE; 0 == "no live address".
    pub address: u64,
    /// 0 = unknown.
    pub size: u32,
    /// non-zero -> "Import type" affordance.
    pub type_index: u32,
    /// filled on aggregation (= provider id / module).
    pub source: String,
    /// "symbol"/"type"/"bookmark"/"rtti"/"struct"/"union"/"enum".
    pub kind: String,
    /// provider-private follow-up data (e.g. PDB path for type-import).
    pub meta: String,
}

/// `class NameProvider` (`name_provider.h:42`). A pluggable source of
/// `{name, address}` information.
///
/// `add`/`remove` take `&self` (interior mutability) because the registry holds
/// `Arc<dyn NameProvider>`; this mirrors the C++ non-const virtuals on a
/// `shared_ptr`. `Send + Sync` is required because the registry and the
/// `RttiNameProvider` are process-global shared across threads.
pub trait NameProvider: Send + Sync {
    /// Stable identifier used for dedupe + UI filter chip key.
    fn id(&self) -> String;

    /// Display label (chip text).
    fn display_name(&self) -> String;

    /// List every entry this provider currently knows about.
    fn entries(&self, active: Option<&dyn Provider>) -> Vec<NamedAddress>;

    /// Accent color, packed `0xAARRGGBB`; 0 = "no opinion". GUI-only — headless
    /// builds return the default.
    fn accent(&self) -> u32 {
        0
    }

    /// `nameFor(addr, active)` default (`name_provider.cpp:5`) — linear scan over
    /// `entries()`. Override for O(1).
    fn name_for(&self, addr: u64, active: Option<&dyn Provider>) -> String {
        if addr == 0 {
            return String::new();
        }
        self.entries(active)
            .into_iter()
            .find(|e| e.address == addr)
            .map(|e| e.name)
            .unwrap_or_default()
    }

    /// `addressFor(name, active)` default (`name_provider.cpp:13`).
    fn address_for(&self, name: &str, active: Option<&dyn Provider>) -> u64 {
        if name.is_empty() {
            return 0;
        }
        self.entries(active)
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.address)
            .unwrap_or(0)
    }

    fn supports_add(&self) -> bool {
        false
    }
    fn add(&self, _name: &str, _address: u64) -> bool {
        false
    }
    fn supports_remove(&self) -> bool {
        false
    }
    fn remove(&self, _name: &str) -> bool {
        false
    }
}

/// `class NameRegistry` (`name_registry.h:17`). Global aggregator for
/// [`NameProvider`]s. The Qt `providersChanged()` signal is replaced by a list of
/// subscriber callbacks.
pub struct NameRegistry {
    providers: Vec<Arc<dyn NameProvider>>,
    on_changed: Vec<Box<dyn Fn() + Send + Sync>>,
}

impl Default for NameRegistry {
    fn default() -> Self {
        NameRegistry {
            providers: Vec::new(),
            on_changed: Vec::new(),
        }
    }
}

impl NameRegistry {
    /// `NameRegistry::instance()` (`name_registry.cpp:5`).
    pub fn global() -> &'static Mutex<NameRegistry> {
        static G: OnceLock<Mutex<NameRegistry>> = OnceLock::new();
        G.get_or_init(|| Mutex::new(NameRegistry::default()))
    }

    /// `registerProvider(p)` (`name_registry.cpp:10`). Idempotent by `id()` —
    /// replaces in place if the id already exists, else appends; then
    /// `emit_changed()`.
    pub fn register_provider(&mut self, p: Arc<dyn NameProvider>) {
        let id = p.id();
        for slot in self.providers.iter_mut() {
            if slot.id() == id {
                *slot = p;
                self.emit_changed();
                return;
            }
        }
        self.providers.push(p);
        self.emit_changed();
    }

    /// `unregisterProvider(id)` (`name_registry.cpp:26`). Only emits when a
    /// removal happened.
    pub fn unregister_provider(&mut self, id: &str) {
        if let Some(i) = self.providers.iter().position(|p| p.id() == id) {
            self.providers.remove(i);
            self.emit_changed();
        }
    }

    /// `providers()` (`name_registry.h:24`).
    pub fn providers(&self) -> Vec<Arc<dyn NameProvider>> {
        self.providers.clone()
    }

    /// `nameFor(addr, active)` (`name_registry.cpp:35`). First non-empty answer in
    /// registration order wins.
    pub fn name_for(&self, addr: u64, active: Option<&dyn Provider>) -> String {
        if addr == 0 {
            return String::new();
        }
        for p in &self.providers {
            let s = p.name_for(addr, active);
            if !s.is_empty() {
                return s;
            }
        }
        String::new()
    }

    /// `addressFor(name, active)` (`name_registry.cpp:44`).
    pub fn address_for(&self, name: &str, active: Option<&dyn Provider>) -> u64 {
        if name.is_empty() {
            return 0;
        }
        for p in &self.providers {
            let a = p.address_for(name, active);
            if a != 0 {
                return a;
            }
        }
        0
    }

    /// `emitChanged()` (`name_registry.cpp:53`) — run all subscriber callbacks.
    pub fn emit_changed(&self) {
        for cb in &self.on_changed {
            cb();
        }
    }

    /// Register a listener (replaces connecting to the Qt `providersChanged()`
    /// signal).
    pub fn subscribe(&mut self, cb: Box<dyn Fn() + Send + Sync>) {
        self.on_changed.push(cb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StubProvider {
        id: String,
        ents: Vec<NamedAddress>,
    }
    impl NameProvider for StubProvider {
        fn id(&self) -> String {
            self.id.clone()
        }
        fn display_name(&self) -> String {
            self.id.clone()
        }
        fn entries(&self, _active: Option<&dyn Provider>) -> Vec<NamedAddress> {
            self.ents.clone()
        }
    }

    fn named(name: &str, addr: u64) -> NamedAddress {
        NamedAddress {
            name: name.to_owned(),
            address: addr,
            ..Default::default()
        }
    }

    // ── default linear-scan name_for / address_for ──
    #[test]
    fn default_reverse_lookups() {
        let p = StubProvider {
            id: "a".to_owned(),
            ents: vec![named("foo", 0x10), named("bar", 0x20)],
        };
        assert_eq!(p.name_for(0x20, None), "bar");
        assert_eq!(p.name_for(0x99, None), "");
        assert_eq!(p.name_for(0, None), "");
        assert_eq!(p.address_for("foo", None), 0x10);
        assert_eq!(p.address_for("nope", None), 0);
        assert_eq!(p.address_for("", None), 0);
    }

    // ── registry idempotency by id + registration-order first-wins ──
    #[test]
    fn registry_idempotent_and_order() {
        let mut reg = NameRegistry::default();
        let a1 = Arc::new(StubProvider {
            id: "a".to_owned(),
            ents: vec![named("x", 0x100)],
        });
        let b = Arc::new(StubProvider {
            id: "b".to_owned(),
            ents: vec![named("y", 0x100)],
        });
        reg.register_provider(a1);
        reg.register_provider(b);
        assert_eq!(reg.providers().len(), 2);
        // first-wins on shared address 0x100 -> provider "a" name "x".
        assert_eq!(reg.name_for(0x100, None), "x");

        // re-register id "a" -> replace in place, len unchanged.
        let a2 = Arc::new(StubProvider {
            id: "a".to_owned(),
            ents: vec![named("x2", 0x100)],
        });
        reg.register_provider(a2);
        assert_eq!(reg.providers().len(), 2);
        assert_eq!(reg.name_for(0x100, None), "x2");

        reg.unregister_provider("a");
        assert_eq!(reg.providers().len(), 1);
        // now "b" answers.
        assert_eq!(reg.name_for(0x100, None), "y");
    }

    // ── emit_changed fires subscribed callbacks ──
    #[test]
    fn emit_changed_fires() {
        let mut reg = NameRegistry::default();
        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = counter.clone();
        reg.subscribe(Box::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
        }));
        // register fires once.
        reg.register_provider(Arc::new(StubProvider {
            id: "a".to_owned(),
            ents: vec![],
        }));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
