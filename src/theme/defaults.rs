//! The shipped default theme JSON files, embedded via `include_str!`.
//!
//! The original 8 are verbatim copies of `src/themes/defaults/*.json` (and
//! byte-identical to `_oracle/fixtures/themes/*.json`); a 9th, `zed_one_dark`,
//! is the Reclass-port-native Zed One Dark theme used as the app's launch
//! default (see `src/ui/theme_apply.rs`). The C++ app shipped these next to the
//! executable and `loadBuiltInThemes` reads them from `<exe>/themes`. We embed
//! the same bytes so the manager can self-heal a missing `themes/` dir and so
//! headless tests can build the built-in set without touching disk.
//!
//! The slice is ordered by filename ascending — exactly the `QDir::Name` sort
//! `loadBuiltInThemes` relies on (`thememanager.cpp:48`).

/// `(filename, json)` for each shipped default, filename-sorted.
pub const DEFAULT_THEMES: &[(&str, &str); 9] = &[
    ("long_night.json", include_str!("defaults/long_night.json")),
    ("mid.json", include_str!("defaults/mid.json")),
    ("modern.json", include_str!("defaults/modern.json")),
    ("phosphor.json", include_str!("defaults/phosphor.json")),
    (
        "reclass_dark.json",
        include_str!("defaults/reclass_dark.json"),
    ),
    ("tw.json", include_str!("defaults/tw.json")),
    ("vs.json", include_str!("defaults/vs.json")),
    ("warm.json", include_str!("defaults/warm.json")),
    // The Zed One Dark-styled default (the app's launch theme). Sorts last by
    // filename ('z' > 'w'), so it appends to the built-in list without shifting
    // any existing index.
    (
        "zed_one_dark.json",
        include_str!("defaults/zed_one_dark.json"),
    ),
];
