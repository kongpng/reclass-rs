//! `RttiNameProvider` — global, Mutex-protected store of RTTI walker
//! discoveries.
//!
//! Port of `src/names/rtti_name_provider.{h,cpp}`. The ONLY mutexed provider in
//! this subsystem (RTTI hits are pushed from compose while the panel reads).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::provider::Provider;
use crate::rtti::names::{NameProvider, NameRegistry, NamedAddress};

/// Inner state behind the lock: hits + a `"name@hex(addr)"` dedupe index.
#[derive(Default)]
struct RttiHits {
    hits: Vec<NamedAddress>,
    by_key: HashMap<String, usize>,
}

/// `class RttiNameProvider` (`rtti_name_provider.h:14`).
#[derive(Default)]
pub struct RttiNameProvider {
    lock: Mutex<RttiHits>,
}

impl RttiNameProvider {
    /// `RttiNameProvider::instance()` (`rtti_name_provider.cpp:9`). Returns the
    /// global as an `Arc` so it can be cloned into the [`NameRegistry`]
    /// (reproduces the C++ no-op-deleter `shared_ptr`).
    pub fn global() -> &'static Arc<RttiNameProvider> {
        static G: OnceLock<Arc<RttiNameProvider>> = OnceLock::new();
        G.get_or_init(|| Arc::new(RttiNameProvider::default()))
    }

    /// `push(name, address, moduleName)` (`rtti_name_provider.cpp:23`).
    /// Idempotent dedupe by `name + "@" + hex(addr)`. Ignores empty name /
    /// address 0. `emit_changed()` is called AFTER releasing the lock.
    pub fn push(&self, name: &str, address: u64, module_name: &str) {
        if name.is_empty() || address == 0 {
            return;
        }
        let key = format!("{name}@{address:x}");
        {
            let mut g = self.lock.lock().unwrap();
            if g.by_key.contains_key(&key) {
                return;
            }
            let idx = g.hits.len();
            g.by_key.insert(key, idx);
            g.hits.push(NamedAddress {
                name: name.to_owned(),
                address,
                kind: "rtti".to_owned(),
                source: if module_name.is_empty() {
                    String::new()
                } else {
                    module_name.to_owned()
                },
                ..Default::default()
            });
        } // lock released here
        NameRegistry::global().lock().unwrap().emit_changed();
    }

    /// `clear()` (`rtti_name_provider.cpp:41`).
    pub fn clear(&self) {
        {
            let mut g = self.lock.lock().unwrap();
            g.hits.clear();
            g.by_key.clear();
        }
        NameRegistry::global().lock().unwrap().emit_changed();
    }

    /// `clearForModule(moduleName)` (`rtti_name_provider.cpp:51`). Keeps only
    /// hits whose `source` differs from `module_name`.
    pub fn clear_for_module(&self, module_name: &str) {
        if module_name.is_empty() {
            return;
        }
        {
            let mut g = self.lock.lock().unwrap();
            let mut kept = Vec::with_capacity(g.hits.len());
            let mut new_key = HashMap::new();
            for h in g.hits.drain(..) {
                if h.source == module_name {
                    continue;
                }
                let key = format!("{}@{:x}", h.name, h.address);
                new_key.insert(key, kept.len());
                kept.push(h);
            }
            g.hits = kept;
            g.by_key = new_key;
        }
        NameRegistry::global().lock().unwrap().emit_changed();
    }
}

impl NameProvider for RttiNameProvider {
    fn id(&self) -> String {
        "rtti".to_owned()
    }
    fn display_name(&self) -> String {
        "RTTI".to_owned()
    }
    /// Returns a COPY of the hits (`rtti_name_provider.cpp:18`).
    fn entries(&self, _active: Option<&dyn Provider>) -> Vec<NamedAddress> {
        self.lock.lock().unwrap().hits.clone()
    }
    // inherited linear-scan name_for / address_for over the hits.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupe_and_ignore_empties() {
        let p = RttiNameProvider::default();
        p.push("Foo", 0x1000, "mod.dll");
        p.push("Foo", 0x1000, "mod.dll"); // dup -> ignored
        p.push("Bar", 0x2000, "");
        // empties ignored.
        p.push("", 0x3000, "x");
        p.push("Baz", 0, "x");
        let ents = p.entries(None);
        assert_eq!(ents.len(), 2);
        assert_eq!(ents[0].name, "Foo");
        assert_eq!(ents[0].source, "mod.dll");
        assert_eq!(ents[0].kind, "rtti");
        assert_eq!(ents[1].name, "Bar");
        assert_eq!(ents[1].source, ""); // empty module -> empty source
                                        // reverse lookup via inherited default.
        assert_eq!(p.name_for(0x2000, None), "Bar");
    }

    #[test]
    fn clear_for_module_keeps_others() {
        let p = RttiNameProvider::default();
        p.push("A", 0x10, "a.dll");
        p.push("B", 0x20, "b.dll");
        p.push("C", 0x30, "a.dll");
        p.clear_for_module("a.dll");
        let ents = p.entries(None);
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].name, "B");
        // by_key rebuilt: pushing B again is a no-op, pushing A again works.
        p.push("B", 0x20, "b.dll");
        assert_eq!(p.entries(None).len(), 1);
        p.push("A", 0x10, "a.dll");
        assert_eq!(p.entries(None).len(), 2);
    }

    #[test]
    fn clear_empties() {
        let p = RttiNameProvider::default();
        p.push("A", 0x10, "a.dll");
        p.clear();
        assert_eq!(p.entries(None).len(), 0);
    }
}
