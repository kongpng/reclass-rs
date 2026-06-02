//! RTTI walking, symbol store/resolve, demangling (Itanium + MSVC), name
//! providers, and the Microsoft symbol-server downloader.
//!
//! Faithful 1:1 port of `src/rtti.{h,cpp}`, `src/symbolstore.{h,cpp}`,
//! `src/symbol_downloader.{h,cpp}`, and `src/names/*`. Gated behind the
//! `symbols` feature (see `_design/specs/PORTING_rtti-symbols.md`).
//!
//! Submodules:
//! - [`walk`]    — `RttiInfo`, `walk_rtti` (MSVC), `walk_rtti_itanium`,
//!   `find_owning_module` (← `rtti.cpp`).
//! - [`demangle`] — `demangle_rtti_name`, `demangle_itanium_name`,
//!   `humanize_symbol_name` (← `rtti.cpp` + `names/symbol_demangle.cpp`).
//! - [`symbol_store`] — `SymbolStore` (global), `PdbSymbolSet`, `PdbTypeInfo`
//!   (← `symbolstore.*`).
//! - [`downloader`] — `SymbolDownloader` (← `symbol_downloader.*`).
//! - [`names`] — `NamedAddress`, `NameProvider`, `NameRegistry`, and the PDB /
//!   RTTI / bookmark providers (← `names/*`).

pub mod browser;
pub mod demangle;
pub mod downloader;
pub mod names;
pub mod symbol_store;
pub mod walk;

// ── Flat re-exports mirroring the C++ `rcx::` namespace surface ──
pub use browser::{
    build_text_report, display_class_name, header_lines, resolve_field_vtable, resolve_rtti,
    RttiFieldError,
};
#[cfg(feature = "ui")]
pub use browser::{RttiBrowserDialog, RttiBrowserEvent};
pub use demangle::{demangle_itanium_name, demangle_rtti_name, humanize_symbol_name};
pub use downloader::{cache_dir, DownloadEvent, DownloadRequest, SymbolDownloader};
pub use names::bookmark::{BookmarkHost, BookmarkNameProvider};
pub use names::pdb::{PdbNameProvider, PdbTypeProvider};
pub use names::rtti::RttiNameProvider;
pub use names::{NameProvider, NameRegistry, NamedAddress};
#[cfg(feature = "imports")]
pub use symbol_store::load_pdb_and_cache_types;
pub use symbol_store::{PdbSymbolSet, PdbTypeInfo, SymbolStore};
pub use walk::{
    find_owning_module, walk_rtti, walk_rtti_itanium, OwningModule, RttiBaseClass, RttiInfo,
    RttiVirtualMethod,
};
