//! RTTI walking, symbol store/resolve, demangling (Itanium + MSVC), and the
//! Microsoft symbol-server downloader.
//!
//! Port of `src/rtti.{h,cpp}`, `symbolstore.{h,cpp}`, `symbol_downloader.{h,cpp}`,
//! and `names/*`. **SKELETON** — the byte-level RTTI walker, the symbol store,
//! and the downloader are filled in by the dedicated `rtti-symbols` workflow
//! (ARCHITECTURE.md §9). Gated behind the `symbols` feature.

pub mod demangle;

use crate::provider::Provider;

/// `struct RttiBaseClass` (`rtti.h:30-34`).
#[derive(Clone, Debug, Default)]
pub struct RttiBaseClass {
    /// ".?AVFoo@@".
    pub raw_name: String,
    /// "Foo".
    pub demangled_name: String,
    /// 0 = self, 1 = direct base, 2+ = grandparent.
    pub depth: i32,
}

/// `struct RttiVirtualMethod` (`rtti.h:36-40`).
#[derive(Clone, Debug, Default)]
pub struct RttiVirtualMethod {
    /// index in vtable.
    pub slot: i32,
    pub address: u64,
    /// resolved via SymbolStore (empty when no PDB loaded).
    pub symbol: String,
}

/// `struct RttiInfo` (`rtti.h:42-58`).
#[derive(Clone, Debug, Default)]
pub struct RttiInfo {
    pub ok: bool,
    pub error: String,
    /// "MSVC" / "Itanium" — empty when `ok == false`.
    pub abi: String,
    pub vtable_address: u64,
    pub image_base: u64,
    pub module_name: String,
    pub complete_locator: u64,
    pub offset: i32,
    pub raw_name: String,
    pub demangled_name: String,
    pub bases: Vec<RttiBaseClass>,
    pub vtable: Vec<RttiVirtualMethod>,
}

/// Walk RTTI starting from a vtable address (`rtti.h`, defined in `rtti.cpp`).
/// Returns `RttiInfo { ok: false, .. }` with an error when the structure does
/// not match MSVC/Itanium layout. SKELETON.
pub fn walk_rtti(_prov: &dyn Provider, _vtable_addr: u64) -> RttiInfo {
    todo!("port rtti.cpp walkRtti (workflow: rtti-symbols)")
}
