//! Symbol store — the process-global PDB/RTTI symbol database.
//!
//! Port of `src/symbolstore.{h,cpp}`. `PdbSymbolSet` (`symbolstore.h:14`),
//! `SymbolStore` (`symbolstore.h:32` / `symbolstore.cpp`).
//!
//! C++ uses an unsynchronized Meyers singleton (single-threaded GUI). The port
//! makes it a `Mutex`-guarded global so concurrent compose/panel access is sound
//! — a safety upgrade with identical observable results. The same type is usable
//! as a local instance for parallel-safe unit tests.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::provider::Provider;

/// `struct PdbTypeInfo` (`imports/import_pdb.h:28`).
///
/// Plain data struct, used by both `imports` and `rtti`. Defined here (mirror)
/// so the `symbols` feature compiles without transitively requiring `imports`,
/// per PORTING spec §0 (the integrator may instead hoist it into `core`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdbTypeInfo {
    /// TPI type index.
    pub type_index: u32,
    /// struct/class/union/enum name.
    pub name: String,
    /// sizeof in bytes.
    pub size: u64,
    /// direct member count.
    pub child_count: i32,
    /// union vs struct/class.
    pub is_union: bool,
    /// enum type.
    pub is_enum: bool,
}

/// `struct PdbSymbolSet` (`symbolstore.h:14`).
#[derive(Clone, Debug, Default)]
pub struct PdbSymbolSet {
    /// empty marks an RTTI-only set.
    pub pdb_path: String,
    /// canonical lowercase (e.g. "ntoskrnl").
    pub module_name: String,
    pub name_to_rva: HashMap<String, u32>,
    pub name_to_type_index: HashMap<String, u32>,
    /// kept SORTED ascending by `.0` for binary search.
    pub rva_to_name: Vec<(u32, String)>,
    /// address-less TPI defs.
    pub types: Vec<PdbTypeInfo>,
}

impl PdbSymbolSet {
    /// `sortRvaIndex()` (`symbolstore.h:26`). Stable sort by RVA (C++ uses
    /// `std::sort`; equal RVAs are rare and reverse lookup only needs an entry
    /// <= target, so either ordering matches behavior).
    fn sort_rva_index(&mut self) {
        self.rva_to_name.sort_by_key(|e| e.0);
    }
}

/// `class SymbolStore` (`symbolstore.h:32`).
pub struct SymbolStore {
    /// canonical -> set.
    modules: HashMap<String, PdbSymbolSet>,
    /// alias -> canonical.
    aliases: HashMap<String, String>,
}

impl Default for SymbolStore {
    fn default() -> Self {
        SymbolStore::new()
    }
}

/// `getModuleBase(provider, canonical)` (`symbolstore.cpp:7`) — `symbol_to_address`
/// + `.exe/.dll/.sys` cascade. Free fn so name providers can resolve a module base
/// without a `SymbolStore` (the C++ static helper duplicated in
/// `pdb_name_provider.cpp:14`).
pub(crate) fn module_base_for(provider: Option<&dyn Provider>, canonical: &str) -> u64 {
    let p = match provider {
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

impl SymbolStore {
    /// Constructor seeds the common Windows kernel aliases (`symbolstore.h:110`).
    pub fn new() -> Self {
        let mut aliases = HashMap::new();
        for a in ["nt", "ntkrnlmp", "ntkrnlpa", "ntkrpamp"] {
            aliases.insert(a.to_owned(), "ntoskrnl".to_owned());
        }
        SymbolStore {
            modules: HashMap::new(),
            aliases,
        }
    }

    /// `SymbolStore::instance()` (`symbolstore.h:34`) — process-global, guarded.
    pub fn global() -> &'static Mutex<SymbolStore> {
        static G: OnceLock<Mutex<SymbolStore>> = OnceLock::new();
        G.get_or_init(|| Mutex::new(SymbolStore::new()))
    }

    /// `resolveAlias(name)` (`symbolstore.h:100`).
    pub fn resolve_alias(&self, name: &str) -> String {
        let mut lower = name.to_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".dll") || lower.ends_with(".sys") {
            if let Some(dot) = lower.rfind('.') {
                lower.truncate(dot);
            }
        }
        self.aliases.get(&lower).cloned().unwrap_or(lower)
    }

    /// `getModuleBase(provider, canonical)` (`symbolstore.cpp:7`).
    fn get_module_base(&self, provider: Option<&dyn Provider>, canonical: &str) -> u64 {
        module_base_for(provider, canonical)
    }

    /// `addModule(moduleName, pdbPath, symbols)` (`symbolstore.cpp:20`). Returns
    /// the number of unique symbols stored. First-wins symbol dedupe; auto-aliases
    /// the raw module name when it differs from the canonical form.
    pub fn add_module(
        &mut self,
        module_name: &str,
        pdb_path: &str,
        symbols: &[(String, u32)],
    ) -> i32 {
        let canonical = self.resolve_alias(module_name);

        let mut set = PdbSymbolSet {
            pdb_path: pdb_path.to_owned(),
            module_name: canonical.clone(),
            ..Default::default()
        };
        for (name, rva) in symbols {
            if set.name_to_rva.contains_key(name) {
                continue;
            }
            set.name_to_rva.insert(name.clone(), *rva);
            set.rva_to_name.push((*rva, name.clone()));
        }
        set.sort_rva_index();
        let count = set.name_to_rva.len() as i32;

        // Register the raw module name as an alias if it differs from canonical.
        let mut raw_lower = module_name.to_lowercase();
        if raw_lower.ends_with(".exe") || raw_lower.ends_with(".dll") || raw_lower.ends_with(".sys")
        {
            if let Some(dot) = raw_lower.rfind('.') {
                raw_lower.truncate(dot);
            }
        }
        if raw_lower != canonical {
            self.aliases.insert(raw_lower, canonical.clone());
        }

        self.modules.insert(canonical.clone(), set);
        tracing::debug!(
            module = %canonical,
            count,
            pdb = %pdb_path,
            "[SymbolStore] loaded symbols for module"
        );
        count
    }

    /// `addModuleTypeIndices(moduleName, map)` (`symbolstore.cpp:55`).
    pub fn add_module_type_indices(&mut self, module_name: &str, map: HashMap<String, u32>) {
        let canonical = self.resolve_alias(module_name);
        if let Some(set) = self.modules.get_mut(&canonical) {
            set.name_to_type_index = map;
        }
    }

    /// `addModuleTypes(moduleName, types)` (`symbolstore.cpp:63`).
    pub fn add_module_types(&mut self, module_name: &str, types: Vec<PdbTypeInfo>) {
        let canonical = self.resolve_alias(module_name);
        if let Some(set) = self.modules.get_mut(&canonical) {
            set.types = types;
        }
    }

    /// `addRttiHits(moduleName, hits)` (`symbolstore.cpp:71`). Creates an empty
    /// RTTI-only set (`pdb_path=""`) if the module is absent.
    pub fn add_rtti_hits(&mut self, module_name: &str, hits: &[(String, u32)]) {
        let canonical = self.resolve_alias(module_name);
        let set = self
            .modules
            .entry(canonical.clone())
            .or_insert_with(|| PdbSymbolSet {
                module_name: canonical.clone(),
                ..Default::default()
            });
        for (name, rva) in hits {
            if set.name_to_rva.contains_key(name) {
                continue;
            }
            set.name_to_rva.insert(name.clone(), *rva);
            set.rva_to_name.push((*rva, name.clone()));
        }
        set.sort_rva_index();
    }

    /// `typeIndexForSymbol(qualified)` (`symbolstore.cpp:92`). Requires
    /// `module!symbol`; returns 0 otherwise.
    pub fn type_index_for_symbol(&self, qualified: &str) -> u32 {
        let bang = match qualified.find('!') {
            Some(b) => b,
            None => return 0,
        };
        // C++ `bangIdx <= 0 || bangIdx >= size-1` -> reject. ASCII '!' so byte==char.
        if bang == 0 || bang == qualified.len() - 1 {
            return 0;
        }
        let mod_part = &qualified[..bang];
        let sym_part = &qualified[bang + 1..];
        let canonical = self.resolve_alias(mod_part);
        self.modules
            .get(&canonical)
            .and_then(|set| set.name_to_type_index.get(sym_part).copied())
            .unwrap_or(0)
    }

    /// `unloadModule(moduleName)` (`symbolstore.cpp:104`).
    pub fn unload_module(&mut self, module_name: &str) {
        let canonical = self.resolve_alias(module_name);
        self.modules.remove(&canonical);
    }

    /// `resolve(token, provider, ok)` (`symbolstore.cpp:109`). Returns
    /// `(value, ok)` instead of the C++ out-param.
    pub fn resolve(&self, token: &str, provider: Option<&dyn Provider>) -> (u64, bool) {
        // Qualified "module!symbol".
        if let Some(b) = token.find('!') {
            if b > 0 && b < token.len() - 1 {
                let mod_part = &token[..b];
                let sym_part = &token[b + 1..];
                let canonical = self.resolve_alias(mod_part);
                let set = match self.modules.get(&canonical) {
                    Some(s) => s,
                    None => return (0, false),
                };
                let rva = match set.name_to_rva.get(sym_part) {
                    Some(r) => *r,
                    None => return (0, false),
                };
                let mut base = self.get_module_base(provider, &canonical);
                if base == 0 {
                    base = self.get_module_base(provider, mod_part);
                }
                return (base.wrapping_add(rva as u64), true);
            }
        }

        // Bare symbol — scan all modules, ambiguity check.
        let mut found_rva = 0u32;
        let mut found_module = String::new();
        let mut matches = 0;
        for (key, set) in &self.modules {
            if let Some(r) = set.name_to_rva.get(token) {
                found_rva = *r;
                found_module = key.clone();
                matches += 1;
                if matches > 1 {
                    return (0, false); // ambiguous
                }
            }
        }
        if matches == 1 {
            let base = self.get_module_base(provider, &found_module);
            return (base.wrapping_add(found_rva as u64), true);
        }

        // matches == 0 -> fallback: treat bare token as a module name.
        let canonical = self.resolve_alias(token);
        let base = self.get_module_base(provider, &canonical);
        if base != 0 {
            return (base, true);
        }
        (0, false)
    }

    /// `getSymbolForAddress(addr, provider)` (`symbolstore.cpp:172`). Reverse
    /// lookup: returns "module!sym" / "module!sym+0xN", or "" if no match. The
    /// owning module must be live-attached (base resolves) to avoid false matches.
    pub fn get_symbol_for_address(&self, addr: u64, provider: Option<&dyn Provider>) -> String {
        if self.modules.is_empty() || provider.is_none() {
            return String::new();
        }
        const K_MAX_DISPLACEMENT: u32 = 0x1000;
        for set in self.modules.values() {
            let base = self.get_module_base(provider, &set.module_name);
            if base == 0 {
                continue;
            }
            if addr < base {
                continue;
            }
            let rva = (addr - base) as u32; // C++ static_cast<uint32_t> (truncating)
            if set.rva_to_name.is_empty() {
                continue;
            }
            // upper_bound(rva) then step back one (last entry with key <= rva).
            let idx = set.rva_to_name.partition_point(|e| e.0 <= rva);
            if idx == 0 {
                continue;
            }
            let (sym_rva, sym_name) = &set.rva_to_name[idx - 1];
            let disp = rva.wrapping_sub(*sym_rva);
            if disp > K_MAX_DISPLACEMENT {
                continue;
            }
            return if disp == 0 {
                format!("{}!{}", set.module_name, sym_name)
            } else {
                format!("{}!{}+0x{:x}", set.module_name, sym_name, disp)
            };
        }
        String::new()
    }

    /// `addAlias(alias, canonicalModule)` (`symbolstore.cpp:216`).
    pub fn add_alias(&mut self, alias: &str, canonical_module: &str) {
        self.aliases
            .insert(alias.to_lowercase(), canonical_module.to_lowercase());
    }

    /// `hasSymbols()` (`symbolstore.h:81`).
    pub fn has_symbols(&self) -> bool {
        !self.modules.is_empty()
    }

    /// `loadedModules()` (`symbolstore.h:84`).
    pub fn loaded_modules(&self) -> Vec<String> {
        self.modules.keys().cloned().collect()
    }

    /// `moduleCount()` (`symbolstore.h:87`).
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// `moduleData(moduleName)` (`symbolstore.h:90`).
    pub fn module_data(&self, name: &str) -> Option<&PdbSymbolSet> {
        let canonical = self.resolve_alias(name);
        self.modules.get(&canonical)
    }
}

/// The C++ Types-tab sort comparator
/// (`a.name.compare(b.name, Qt::CaseInsensitive) < 0`; `main.cpp:7982-7984`).
///
/// Qt's case-insensitive `compare` orders by case-folded name; ties (names that
/// differ only in case) fall back to the original case so the order is
/// deterministic (Qt's `compare` returns 0 for those and `std::sort` leaves the
/// order unspecified — pinning it here is a strict refinement, never a deviation
/// for distinct case-insensitive names). Pure + unit-tested.
pub fn pdb_type_name_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// `MainWindow::loadPdbAndCacheTypes(pdbPath)` (`main.cpp:7958`) — the one entry
/// point that pulls symbols + types out of a local/cached PDB and folds them into
/// the process-global [`SymbolStore`].
///
/// This is the production wiring the host (`window.rs`) calls from the
/// Download-All loop and from a module-row activation once a PDB has been located
/// on disk (network fetch is the only true platform stub). It is the missing
/// caller for [`extract_pdb_symbols`](crate::imports::extract_pdb_symbols),
/// [`enumerate_pdb_types`](crate::imports::enumerate_pdb_types),
/// [`add_module`](SymbolStore::add_module),
/// [`add_module_type_indices`](SymbolStore::add_module_type_indices), and
/// [`add_module_types`](SymbolStore::add_module_types).
///
/// Faithful to the C++ post-conditions:
///   1. `extractPdbSymbols(pdbPath)`; if it has **no symbols**, return `0` and
///      touch nothing (the C++ `if (result.symbols.isEmpty()) return 0;`).
///   2. Build the `(name, rva)` pairs in symbol order and the `name → typeIndex`
///      map for symbols whose `typeIndex != 0`.
///   3. `addModule(result.moduleName, pdbPath, pairs)` → the returned count.
///   4. `addModuleTypeIndices(result.moduleName, typeIndices)` **only when the map
///      is non-empty** (the C++ `if (!typeIndices.isEmpty())`).
///   5. `enumeratePdbTypes(pdbPath)`; when non-empty, sort case-insensitively by
///      name (the C++ `std::sort` comparator) and `addModuleTypes(...)` — this is
///      where the Rust port caches the C++ `m_cachedModuleTypes` entries so the
///      Modules ▸ Types tab can list them.
///
/// `module` is accepted for API symmetry with the C++ call sites (which already
/// know the module the PDB belongs to) but the canonical module name always comes
/// from the PDB itself (`result.moduleName`), exactly as the C++ does — the store
/// keys off the PDB's own name so reverse lookups stay consistent.
///
/// Returns the number of unique symbols stored (`0` when the PDB has none, or on
/// an extract error — the C++ has no symbols to add in that case either).
#[cfg(feature = "imports")]
pub fn load_pdb_and_cache_types(path: &std::path::Path, _module: &str) -> i32 {
    let result = match crate::imports::extract_pdb_symbols(path) {
        Ok(r) => r,
        // No symbols extractable (bad/locked/non-PDB) — nothing to add, like the
        // C++ early-out on an empty result.
        Err(_) => return 0,
    };
    if result.symbols.is_empty() {
        return 0;
    }

    let mut pairs: Vec<(String, u32)> = Vec::with_capacity(result.symbols.len());
    let mut type_indices: HashMap<String, u32> = HashMap::new();
    for s in &result.symbols {
        pairs.push((s.name.clone(), s.rva));
        if s.type_index != 0 {
            type_indices.insert(s.name.clone(), s.type_index);
        }
    }

    let pdb_path = path.to_string_lossy();
    let count = {
        let mut store = match SymbolStore::global().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let count = store.add_module(&result.module_name, &pdb_path, &pairs);
        if !type_indices.is_empty() {
            store.add_module_type_indices(&result.module_name, type_indices);
        }
        count
    };

    // Cache enumerated types for the Types tab (the C++ `m_cachedModuleTypes`
    // entry + `rebuildTypesModel()`; the Rust port stores them on the module set).
    if let Ok(mut types) = crate::imports::enumerate_pdb_types(path) {
        if !types.is_empty() {
            // C++ `std::sort(..., a.name.compare(b.name, Qt::CaseInsensitive) < 0)`.
            types.sort_by(|a, b| pdb_type_name_order(&a.name, &b.name));
            // `imports::PdbTypeInfo` and `symbol_store::PdbTypeInfo` are field-for-
            // field mirrors; convert across the seam.
            let mapped: Vec<PdbTypeInfo> = types
                .into_iter()
                .map(|t| PdbTypeInfo {
                    type_index: t.type_index,
                    name: t.name,
                    size: t.size,
                    child_count: t.child_count,
                    is_union: t.is_union,
                    is_enum: t.is_enum,
                })
                .collect();
            let mut store = match SymbolStore::global().lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            store.add_module_types(&result.module_name, mapped);
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test provider exposing `symbol_to_address` (module base) and a flat buffer.
    struct TestProvider {
        bases: HashMap<String, u64>,
    }
    impl Provider for TestProvider {
        fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
            false
        }
        fn size(&self) -> i32 {
            0
        }
        fn symbol_to_address(&self, name: &str) -> u64 {
            self.bases.get(name).copied().unwrap_or(0)
        }
    }

    // ── resolve_alias (§TEST 2) ──
    #[test]
    fn resolve_alias_kernel_and_ext_strip() {
        let s = SymbolStore::new();
        assert_eq!(s.resolve_alias("nt"), "ntoskrnl");
        assert_eq!(s.resolve_alias("NTOSKRNL.EXE"), "ntoskrnl");
        assert_eq!(s.resolve_alias("foo.dll"), "foo");
        assert_eq!(s.resolve_alias("Bar"), "bar");
    }

    // ── add_module first-wins dedupe + count + auto-alias ──
    #[test]
    fn add_module_dedupe_and_alias() {
        let mut s = SymbolStore::new();
        let syms = vec![
            ("bar".to_owned(), 0x10u32),
            ("baz".to_owned(), 0x20u32),
            ("bar".to_owned(), 0x99u32), // dup name — first wins
        ];
        let count = s.add_module("foo.dll", "/x/foo.pdb", &syms);
        assert_eq!(count, 2);
        // auto-alias "foo" -> "foo" canonical means no extra alias (raw==canonical).
        // Module is reachable as foo!bar; with no provider, base==0 but ok=true.
        let (v, ok) = s.resolve("foo!bar", None);
        assert!(ok);
        assert_eq!(v, 0x10); // base 0 + rva 0x10
                             // dup did not overwrite.
        let set = s.module_data("foo").unwrap();
        assert_eq!(set.name_to_rva.get("bar").copied(), Some(0x10));
    }

    // ── add_module auto-alias of a differing raw name ──
    #[test]
    fn add_module_auto_alias_differing_raw() {
        let mut s = SymbolStore::new();
        // canonical of "ntoskrnl.exe" is "ntoskrnl"; raw "ntkrnlmp" pre-aliases
        // to ntoskrnl already. Use a fresh differing form.
        s.add_alias("mymod64", "mymod");
        let count = s.add_module("mymod64.dll", "", &[("sym".to_owned(), 0x40)]);
        assert_eq!(count, 1);
        // resolve via the raw user form too.
        let (v, ok) = s.resolve("mymod!sym", None);
        assert!(ok);
        assert_eq!(v, 0x40);
        let (v2, ok2) = s.resolve("mymod64!sym", None);
        assert!(ok2);
        assert_eq!(v2, 0x40);
    }

    // ── resolve ambiguity / single / module fallback ──
    #[test]
    fn resolve_ambiguity_single_and_fallback() {
        let mut s = SymbolStore::new();
        s.add_module("a.dll", "", &[("dup".to_owned(), 0x10)]);
        s.add_module("b.dll", "", &[("dup".to_owned(), 0x20)]);
        let prov = TestProvider {
            bases: HashMap::new(), // base 0 for both
        };
        // ambiguous bare symbol -> (0, false).
        let (_, ok) = s.resolve("dup", Some(&prov));
        assert!(!ok);

        // single-module bare symbol -> resolved.
        let mut s2 = SymbolStore::new();
        s2.add_module("only.dll", "", &[("uniq".to_owned(), 0x33)]);
        let (v, ok) = s2.resolve("uniq", None);
        assert!(ok);
        assert_eq!(v, 0x33);

        // bare module-name fallback -> module base.
        let mut bases = HashMap::new();
        bases.insert("only".to_owned(), 0x4000u64);
        let prov2 = TestProvider { bases };
        let (v, ok) = s2.resolve("only", Some(&prov2));
        assert!(ok);
        assert_eq!(v, 0x4000);
    }

    // ── get_symbol_for_address binary search + displacement + live-attach ──
    #[test]
    fn get_symbol_for_address_full() {
        let mut s = SymbolStore::new();
        s.add_module(
            "mod.dll",
            "",
            &[
                ("a".to_owned(), 0x100),
                ("b".to_owned(), 0x200),
                ("c".to_owned(), 0x300),
            ],
        );
        let mut bases = HashMap::new();
        bases.insert("mod".to_owned(), 0x4000_0000u64);
        let prov = TestProvider { bases };

        // exact hit.
        assert_eq!(
            s.get_symbol_for_address(0x4000_0000 + 0x200, Some(&prov)),
            "mod!b"
        );
        // displaced within cap.
        assert_eq!(
            s.get_symbol_for_address(0x4000_0000 + 0x210, Some(&prov)),
            "mod!b+0x10"
        );
        // displacement > 0x1000 -> "".
        assert_eq!(
            s.get_symbol_for_address(0x4000_0000 + 0x300 + 0x2000, Some(&prov)),
            ""
        );
        // addr < base -> "".
        assert_eq!(s.get_symbol_for_address(0x10, Some(&prov)), "");
        // no provider -> "".
        assert_eq!(s.get_symbol_for_address(0x4000_0200, None), "");
        // unattached module (base 0) -> "".
        let prov0 = TestProvider {
            bases: HashMap::new(),
        };
        assert_eq!(s.get_symbol_for_address(0x4000_0200, Some(&prov0)), "");
    }

    // ── add_rtti_hits creates an RTTI-only set when module absent ──
    #[test]
    fn add_rtti_hits_creates_set() {
        let mut s = SymbolStore::new();
        s.add_rtti_hits("newmod.dll", &[("Class".to_owned(), 0x500)]);
        let set = s.module_data("newmod").expect("RTTI-only set created");
        assert_eq!(set.pdb_path, "");
        assert_eq!(set.name_to_rva.get("Class").copied(), Some(0x500));
        let (v, ok) = s.resolve("newmod!Class", None);
        assert!(ok);
        assert_eq!(v, 0x500);
    }

    // ── type_index_for_symbol ──
    #[test]
    fn type_index_for_symbol_cases() {
        let mut s = SymbolStore::new();
        s.add_module("m.dll", "", &[("sym".to_owned(), 0x10)]);
        let mut map = HashMap::new();
        map.insert("sym".to_owned(), 42u32);
        s.add_module_type_indices("m.dll", map);
        assert_eq!(s.type_index_for_symbol("m!sym"), 42);
        // missing '!' -> 0.
        assert_eq!(s.type_index_for_symbol("sym"), 0);
        // leading '!' -> 0.
        assert_eq!(s.type_index_for_symbol("!sym"), 0);
        // trailing '!' -> 0.
        assert_eq!(s.type_index_for_symbol("m!"), 0);
        // unknown symbol -> 0.
        assert_eq!(s.type_index_for_symbol("m!other"), 0);
    }

    // ── unload_module ──
    #[test]
    fn unload_module_removes() {
        let mut s = SymbolStore::new();
        s.add_module("m.dll", "", &[("sym".to_owned(), 0x10)]);
        assert!(s.has_symbols());
        s.unload_module("m");
        assert!(!s.has_symbols());
    }

    // ── pdb_type_name_order: the C++ Qt::CaseInsensitive sort comparator ──
    #[test]
    fn pdb_type_name_order_is_case_insensitive() {
        use std::cmp::Ordering;
        // Case-insensitive primary order: "apple" < "Banana" < "cherry".
        assert_eq!(pdb_type_name_order("apple", "Banana"), Ordering::Less);
        assert_eq!(pdb_type_name_order("Banana", "cherry"), Ordering::Less);
        assert_eq!(pdb_type_name_order("ZEBRA", "apple"), Ordering::Greater);
        // Names equal under case-fold are NOT Equal (deterministic tiebreak), but
        // they sort adjacent — the C++ post-condition (case-insensitive grouping)
        // holds.
        assert_eq!(pdb_type_name_order("Foo", "Foo"), Ordering::Equal);
        assert_ne!(pdb_type_name_order("Foo", "foo"), Ordering::Equal);
        // Sorting a mixed-case list groups case-insensitively.
        let mut v = vec!["delta", "Alpha", "charlie", "Bravo"];
        v.sort_by(|a, b| pdb_type_name_order(a, b));
        assert_eq!(v, vec!["Alpha", "Bravo", "charlie", "delta"]);
    }

    // ── load_pdb_and_cache_types: the C++ early-out (main.cpp:7960-7961) ──
    // A path that doesn't resolve to a readable PDB yields no symbols, so the
    // loader returns 0 and adds nothing to the store (the
    // `if (result.symbols.isEmpty()) return 0;` short-circuit). The success path
    // needs a real PDB fixture (the network/PDB seam) and is exercised by the
    // imports-layer tests; here we pin the no-op contract that gates it.
    #[cfg(feature = "imports")]
    #[test]
    fn load_pdb_missing_file_is_a_noop_returning_zero() {
        let before = SymbolStore::global()
            .lock()
            .map(|g| g.module_count())
            .unwrap_or(0);
        let n = super::load_pdb_and_cache_types(
            std::path::Path::new("nonexistent-load-pdb-xyzzy.pdb"),
            "ghost.dll",
        );
        assert_eq!(n, 0, "a missing PDB must extract no symbols");
        let after = SymbolStore::global()
            .lock()
            .map(|g| g.module_count())
            .unwrap_or(0);
        assert_eq!(after, before, "a failed load must not mutate the store");
        // And nothing was keyed under the requested module name.
        assert!(SymbolStore::global()
            .lock()
            .map(|g| g.module_data("ghost.dll").is_none())
            .unwrap_or(true));
    }
}
