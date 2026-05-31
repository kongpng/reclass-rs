//! PDB name + type providers.
//!
//! Port of `src/names/pdb_name_provider.{h,cpp}` and
//! `src/names/pdb_type_provider.{h,cpp}`. Both read through the global
//! [`SymbolStore`].

use crate::provider::Provider;
use crate::rtti::demangle::humanize_symbol_name;
use crate::rtti::names::{NameProvider, NamedAddress};
use crate::rtti::symbol_store::SymbolStore;

/// `moduleBaseFor(active, canonical)` (static, `pdb_name_provider.cpp:14`) —
/// `symbol_to_address` + `.exe/.dll/.sys` cascade.
fn module_base_for(active: Option<&dyn Provider>, canonical: &str) -> u64 {
    let p = match active {
        Some(p) => p,
        None => return 0,
    };
    let mut base = p.symbol_to_address(canonical);
    if base == 0 {
        base = p.symbol_to_address(&format!("{canonical}.exe"));
    }
    if base == 0 {
        base = p.symbol_to_address(&format!("{canonical}.dll"));
    }
    if base == 0 {
        base = p.symbol_to_address(&format!("{canonical}.sys"));
    }
    base
}

/// `class PdbNameProvider` (`pdb_name_provider.h:9`).
#[derive(Default)]
pub struct PdbNameProvider;

impl NameProvider for PdbNameProvider {
    fn id(&self) -> String {
        "pdb-symbols".to_owned()
    }
    fn display_name(&self) -> String {
        "PDB Symbols".to_owned()
    }

    /// `entries(active)` (`pdb_name_provider.cpp:23`). When the owning module is
    /// not live-attached (`base==0`), `address=0` so reverse lookup skips it.
    fn entries(&self, active: Option<&dyn Provider>) -> Vec<NamedAddress> {
        let store = SymbolStore::global().lock().unwrap();
        let mut out = Vec::new();
        for module in store.loaded_modules() {
            let set = match store.module_data(&module) {
                Some(s) => s,
                None => continue,
            };
            let base = module_base_for(active, &module);
            for (sym_name, rva) in &set.name_to_rva {
                out.push(NamedAddress {
                    name: sym_name.clone(),
                    display_name: humanize_symbol_name(sym_name),
                    address: if base != 0 {
                        base.wrapping_add(*rva as u64)
                    } else {
                        0
                    },
                    type_index: set.name_to_type_index.get(sym_name).copied().unwrap_or(0),
                    kind: "symbol".to_owned(),
                    meta: set.pdb_path.clone(),
                    ..Default::default()
                });
            }
        }
        out
    }

    /// `nameFor(addr, active)` (override, `pdb_name_provider.cpp:68`). Routes
    /// through `SymbolStore::get_symbol_for_address`, then humanizes the symbol
    /// portion.
    fn name_for(&self, addr: u64, active: Option<&dyn Provider>) -> String {
        if addr == 0 {
            return String::new();
        }
        let raw = SymbolStore::global()
            .lock()
            .unwrap()
            .get_symbol_for_address(addr, active);
        if raw.is_empty() {
            return String::new();
        }
        let bang = match raw.find('!') {
            Some(b) => b,
            None => return raw,
        };
        let prefix = &raw[..=bang]; // "module!"
        let rest = &raw[bang + 1..];
        let (sym, suffix) = match rest.find('+') {
            Some(plus) => (&rest[..plus], &rest[plus..]),
            None => (rest, ""),
        };
        let humanized = humanize_symbol_name(sym);
        let shown: &str = if humanized.is_empty() { sym } else { &humanized };
        format!("{prefix}{shown}{suffix}")
    }

    /// `addressFor(name, active)` (override, `pdb_name_provider.cpp:83`).
    fn address_for(&self, name: &str, active: Option<&dyn Provider>) -> u64 {
        let (a, ok) = SymbolStore::global().lock().unwrap().resolve(name, active);
        if ok {
            a
        } else {
            0
        }
    }
}

/// `class PdbTypeProvider` (`pdb_type_provider.h:9`).
#[derive(Default)]
pub struct PdbTypeProvider;

impl NameProvider for PdbTypeProvider {
    fn id(&self) -> String {
        "pdb-types".to_owned()
    }
    fn display_name(&self) -> String {
        "PDB Types".to_owned()
    }

    /// `entries(_active)` (`pdb_type_provider.cpp:16`). Always address-less.
    fn entries(&self, _active: Option<&dyn Provider>) -> Vec<NamedAddress> {
        let store = SymbolStore::global().lock().unwrap();
        let mut out = Vec::new();
        for module in store.loaded_modules() {
            let set = match store.module_data(&module) {
                Some(s) => s,
                None => continue,
            };
            for ti in &set.types {
                out.push(NamedAddress {
                    name: ti.name.clone(),
                    display_name: humanize_symbol_name(&ti.name),
                    address: 0,
                    size: ti.size as u32,
                    type_index: ti.type_index,
                    kind: if ti.is_enum {
                        "enum".to_owned()
                    } else if ti.is_union {
                        "union".to_owned()
                    } else {
                        "struct".to_owned()
                    },
                    meta: set.pdb_path.clone(),
                    ..Default::default()
                });
            }
        }
        out
    }
    // Inherited linear-scan name_for/address_for: all addresses 0 -> reverse
    // lookups are inert (the addr==0 guard / no nonzero match).
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtti::symbol_store::PdbTypeInfo;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // The SymbolStore global is process-shared; serialize the tests that mutate
    // it so parallel cargo threads don't interleave.
    static SERIAL: Mutex<()> = Mutex::new(());

    struct BaseProvider {
        bases: HashMap<String, u64>,
    }
    impl Provider for BaseProvider {
        fn read(&self, _a: u64, _b: &mut [u8]) -> bool {
            false
        }
        fn size(&self) -> i32 {
            0
        }
        fn symbol_to_address(&self, name: &str) -> u64 {
            self.bases.get(name).copied().unwrap_or(0)
        }
    }

    // ── entries base==0 -> address==0; base set -> base+rva ──
    #[test]
    fn pdb_entries_address_resolution() {
        let _g = SERIAL.lock().unwrap();
        {
            let mut store = SymbolStore::global().lock().unwrap();
            store.unload_module("pnp_test");
            store.add_module("pnp_test.dll", "/x/pnp_test.pdb", &[("sym".to_owned(), 0x40)]);
        }
        let p = PdbNameProvider;

        // base==0 -> address==0.
        let prov0 = BaseProvider {
            bases: HashMap::new(),
        };
        let ents = p.entries(Some(&prov0));
        let found = ents.iter().find(|e| e.name == "sym").expect("sym present");
        assert_eq!(found.address, 0);
        assert_eq!(found.kind, "symbol");
        assert_eq!(found.meta, "/x/pnp_test.pdb");

        // base set -> base + rva.
        let mut bases = HashMap::new();
        bases.insert("pnp_test".to_owned(), 0x5000_0000u64);
        let prov = BaseProvider { bases };
        let ents = p.entries(Some(&prov));
        let found = ents.iter().find(|e| e.name == "sym").expect("sym present");
        assert_eq!(found.address, 0x5000_0000 + 0x40);

        // cleanup.
        SymbolStore::global().lock().unwrap().unload_module("pnp_test");
    }

    // ── PdbTypeProvider address-less + kind mapping ──
    #[test]
    fn pdb_type_entries_kinds() {
        let _g = SERIAL.lock().unwrap();
        {
            let mut store = SymbolStore::global().lock().unwrap();
            store.unload_module("ptp_test");
            store.add_module("ptp_test.dll", "/x/ptp.pdb", &[]);
            store.add_module_types(
                "ptp_test.dll",
                vec![
                    PdbTypeInfo {
                        type_index: 1,
                        name: "MyStruct".to_owned(),
                        size: 8,
                        is_union: false,
                        is_enum: false,
                        ..Default::default()
                    },
                    PdbTypeInfo {
                        type_index: 2,
                        name: "MyUnion".to_owned(),
                        size: 4,
                        is_union: true,
                        is_enum: false,
                        ..Default::default()
                    },
                    PdbTypeInfo {
                        type_index: 3,
                        name: "MyEnum".to_owned(),
                        size: 4,
                        is_union: false,
                        is_enum: true,
                        ..Default::default()
                    },
                ],
            );
        }
        let p = PdbTypeProvider;
        let ents = p.entries(None);
        let by = |n: &str| ents.iter().find(|e| e.name == n).cloned().unwrap();
        assert_eq!(by("MyStruct").kind, "struct");
        assert_eq!(by("MyStruct").address, 0);
        assert_eq!(by("MyStruct").size, 8);
        assert_eq!(by("MyStruct").type_index, 1);
        assert_eq!(by("MyUnion").kind, "union");
        assert_eq!(by("MyEnum").kind, "enum");
        // reverse lookups inert.
        assert_eq!(p.name_for(0, None), "");

        SymbolStore::global().lock().unwrap().unload_module("ptp_test");
    }
}
