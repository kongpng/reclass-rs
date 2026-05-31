//! Runtime JSON theme model + manager — port of `src/themes/*`.
//!
//! Maps the C++/Qt6 theme subsystem (`theme.{h,cpp}`, `thememanager.{h,cpp}`,
//! `themeeditor.{h,cpp}`) onto Rust:
//!
//! - [`color`] — the `QColor` subset used here ([`Color`], parse/format,
//!   `lighter(130)`, lerp).
//! - [`model`] — the [`Theme`] struct, [`THEME_FIELDS`] table, and the
//!   `to_json` / `from_json` derivation pipeline.
//! - [`manager`] — [`ThemeManager`]: built-in + user themes, current selection,
//!   CRUD, persistence, preview, and the `themeChanged` observer registry.
//! - [`defaults`] — the 8 shipped default theme JSON files, embedded.
//! - `editor` — the GPUI `ThemeEditor` view (behind the `ui` feature).
//!
//! Logic (`color` / `model` / `manager`) builds with `--no-default-features`
//! so the fidelity tests run headless (ARCHITECTURE §8).

pub mod color;
pub mod defaults;
pub mod manager;
pub mod model;

#[cfg(feature = "ui")]
pub mod editor;

pub use color::{hex_or_black, lerp_rgb, Color};
pub use defaults::DEFAULT_THEMES;
pub use manager::{MemSettings, SettingsStore, SubId, ThemeManager};
pub use model::{FieldId, Theme, ThemeFieldMeta, THEME_FIELDS};
