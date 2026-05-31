//! The 8 shipped default theme JSON files, embedded via `include_str!`.
//!
//! Verbatim copies of `src/themes/defaults/*.json` (and byte-identical to
//! `_oracle/fixtures/themes/*.json`). The C++ app ships these next to the
//! executable and `loadBuiltInThemes` reads them from `<exe>/themes`. We embed
//! the same bytes so the manager can self-heal a missing `themes/` dir and so
//! headless tests can build the built-in set without touching disk.
//!
//! The slice is ordered by filename ascending — exactly the `QDir::Name` sort
//! `loadBuiltInThemes` relies on (`thememanager.cpp:48`).

/// `(filename, json)` for each shipped default, filename-sorted.
pub const DEFAULT_THEMES: &[(&str, &str); 8] = &[
    ("long_night.json", include_str!("defaults/long_night.json")),
    ("mid.json", include_str!("defaults/mid.json")),
    ("modern.json", include_str!("defaults/modern.json")),
    ("phosphor.json", include_str!("defaults/phosphor.json")),
    ("reclass_dark.json", include_str!("defaults/reclass_dark.json")),
    ("tw.json", include_str!("defaults/tw.json")),
    ("vs.json", include_str!("defaults/vs.json")),
    ("warm.json", include_str!("defaults/warm.json")),
];
