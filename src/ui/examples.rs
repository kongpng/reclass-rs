//! Bundled example projects — a dependency-free loader over the `.rcx` files
//! shipped in `assets/examples/`.
//!
//! The original Reclass ships a handful of demo projects (`src/examples/*.rcx`)
//! that the start page surfaces under its **Examples** bucket
//! ([`Bucket::Examples`](super::startpage::Bucket::Examples)) so a fresh install
//! has something to open. Rather than depend on a runtime examples *directory*
//! (which moves around per install), the port **embeds** the curated set into
//! the binary at compile time via [`include_str!`], so they are always present
//! and identical across platforms.
//!
//! ## Why this module is dependency-free
//! It pulls in nothing from gpui — it is plain `&'static str` data + two lookup
//! functions. It lives under `ui` only because that is where the start page /
//! window consume it; nothing here actually needs the UI feature.
//!
//! ## API
//! - [`examples()`] → the full `(display_name, rcx_json_text)` table.
//! - [`example_json`] → look up one example's JSON by display name.
//! - [`write_example_to_temp`] → materialize an example as a real file on disk
//!   and return its path (see **Opening an example** below).
//!
//! ## Opening an example
//! The loader [`crate::controller::RcxDocument::load`] is **path-based only**
//! (it reads bytes off disk; there is no `load_str`). To open a bundled example
//! through the existing window plumbing
//! ([`MainWindow::open_project`](super::window::MainWindow::open_project), which
//! calls `doc.load(path)`), the caller must first put the embedded JSON on disk.
//! [`write_example_to_temp`] does exactly that — it writes the JSON to a
//! uniquely-named file under [`std::env::temp_dir`] and returns the [`PathBuf`],
//! which can then be handed straight to `open_project(&path, None, window, cx)`.
//! The file name is `reclass-example-<stem>.rcx` so the resulting document's
//! tab title reads naturally.

use std::path::PathBuf;

/// The embedded example set: `(display_name, rcx_json_text)` per bundled file.
///
/// `display_name` is the file stem (no extension); `rcx_json_text` is the file's
/// JSON, embedded at compile time. Add a file to `assets/examples/` and a line
/// here to bundle it (each is listed explicitly so the embed is auditable).
const EXAMPLES: &[(&str, &str)] = &[
    (
        "AlignmentBugDemo",
        include_str!("../../assets/examples/AlignmentBugDemo.rcx"),
    ),
    (
        "EPROCESS",
        include_str!("../../assets/examples/EPROCESS.rcx"),
    ),
    (
        "KUSER_SHARED_DATA",
        include_str!("../../assets/examples/KUSER_SHARED_DATA.rcx"),
    ),
    ("MMPFN", include_str!("../../assets/examples/MMPFN.rcx")),
    (
        "PageTables",
        include_str!("../../assets/examples/PageTables.rcx"),
    ),
    ("png", include_str!("../../assets/examples/png.rcx")),
];

/// The bundled example projects as `(display_name, rcx_json_text)` pairs.
///
/// Display name = the source file's stem; the text is the embedded `.rcx` JSON.
/// Always non-empty (asserted by tests).
pub fn examples() -> &'static [(&'static str, &'static str)] {
    EXAMPLES
}

/// The embedded JSON for the example named `name` (its display name / file
/// stem), or `None` if no such example is bundled. The match is exact.
pub fn example_json(name: &str) -> Option<&'static str> {
    EXAMPLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, json)| *json)
}

/// Materialize the example named `name` as a file on disk and return its path.
///
/// Because [`crate::controller::RcxDocument::load`] is path-based, this writes
/// the embedded JSON to `reclass-example-<name>.rcx` under
/// [`std::env::temp_dir`] so callers can open it through the normal
/// [`MainWindow::open_project`](super::window::MainWindow::open_project) path.
/// Returns `None` if `name` is unknown or the write fails.
pub fn write_example_to_temp(name: &str) -> Option<PathBuf> {
    let json = example_json(name)?;
    let mut path = std::env::temp_dir();
    path.push(format!("reclass-example-{name}.rcx"));
    std::fs::write(&path, json).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_are_present_and_valid_json() {
        let all = examples();
        assert!(!all.is_empty(), "expected at least one bundled example");
        for (name, json) in all {
            assert!(!name.is_empty(), "example display name must not be empty");
            serde_json::from_str::<serde_json::Value>(json)
                .unwrap_or_else(|e| panic!("example {name:?} is not valid JSON: {e}"));
        }
    }

    #[test]
    fn example_json_lookup_round_trips() {
        let (name, json) = examples()[0];
        assert_eq!(example_json(name), Some(json));
        assert_eq!(example_json("definitely-not-an-example"), None);
    }

    #[test]
    fn write_example_to_temp_writes_loadable_file() {
        let name = examples()[0].0;
        let path = write_example_to_temp(name).expect("temp write should succeed");
        let on_disk = std::fs::read_to_string(&path).expect("temp file readable");
        serde_json::from_str::<serde_json::Value>(&on_disk).expect("written file is valid JSON");
        let _ = std::fs::remove_file(&path);
    }
}
