//! Symbol-name humanization — dispatch MSVC RTTI / Itanium / MSVC mangling to
//! the right demangler.
//!
//! Port of `src/names/symbol_demangle.{h,cpp}`. **SKELETON** — the dispatch
//! (`.?A`/`?A` → in-house RTTI parser; `_Z*` → Itanium via `cpp_demangle`;
//! `?`/`_?` → MSVC via `msvc-demangler`) is filled in by the `rtti-symbols`
//! workflow. Returns an empty string when the input is already human-readable
//! (the C++ `humanizeSymbolName` contract).

/// `humanizeSymbolName(mangled)` (`symbol_demangle.h:35`). SKELETON.
pub fn humanize_symbol_name(_mangled: &str) -> String {
    todo!("port symbol_demangle.cpp humanizeSymbolName (workflow: rtti-symbols)")
}
