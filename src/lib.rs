//! # reclass
//!
//! A faithful, 1:1 Rust + GPUI port of **Reclass**
//! (github.com/IChooseYou/Reclass, MIT) — an open-source structured-data /
//! struct-layout editor (a modern successor to ReClass.NET).
//!
//! This is a **single package** whose `src/` mirrors the monolithic C++ app's
//! `src/` tree (see `_design/ARCHITECTURE.md` §2). Each C++ subsystem is a
//! module here; the headless/heavy-dep separation is handled by **cargo
//! features**, not crate walls:
//!
//! - `--no-default-features` → the headless engine (no gpui), for fast
//!   logic-only testing.
//! - `default = ["ui", "imports", "disasm", "symbols", "mcp"]` gates the heavy
//!   optional modules + their external deps.
//!
//! All data access goes through the [`provider::Provider`] trait. Only the
//! benign built-in sources (file / buffer / snapshot / null) are implemented;
//! the live OS process / kernel / remote / WinDbg sources are documented stubs
//! in [`provider::native`] (out of scope for this port).

// gpui's element/builder types are deeply nested generics; some `#[test]` macro
// expansions in the UI modules brush against the default 128 type-recursion
// budget. The editor test modules avoid the `gpui::*` glob to stay well under it;
// this modest bump gives headroom without masking real recursion. (No effect on
// non-test builds.)
#![recursion_limit = "256"]

// ── Always-on engine modules ──
pub mod addr;
pub mod compose;
pub mod controller;
pub mod core;
pub mod format;
pub mod generator;
pub mod provider;
pub mod scanner;
pub mod theme;

// ── Feature-gated heavy/optional modules ──
#[cfg(feature = "disasm")]
pub mod disasm;

#[cfg(feature = "symbols")]
pub mod rtti;

#[cfg(feature = "imports")]
pub mod imports;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "ui")]
pub mod ui;
