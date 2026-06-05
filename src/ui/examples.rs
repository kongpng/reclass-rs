//! Bundled example projects — a dependency-free loader over the `.rcx` files
//! shipped in `assets/examples/`.
//!
//! The original Reclass ships a handful of demo projects (`src/examples/*.rcx`)
//! that the start page surfaces under its **Examples** bucket
//! ([`Bucket::Examples`](crate::ui::startpage::Bucket::Examples)) so a fresh install
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
//!   (with its sidecar data file(s)) and return its path (see **Opening an
//!   example** below).
//!
//! ## Opening an example
//! The loader [`crate::controller::RcxDocument::load`] is **path-based only**
//! (it reads bytes off disk; there is no `load_str`). To open a bundled example
//! through the existing window plumbing
//! ([`MainWindow::open_project`](crate::ui::window::MainWindow::open_project), which
//! calls `doc.load(path)`), the caller must first put the embedded JSON on disk.
//! [`write_example_to_temp`] does exactly that — it writes the JSON to a
//! uniquely-named file under [`std::env::temp_dir`] and returns the [`PathBuf`],
//! which can then be handed straight to `open_project(&path, None, window, cx)`.
//! The file name is `reclass-example-<stem>.rcx` so the resulting document's
//! tab title reads naturally.
//!
//! ## Sidecar data files (the all-0x0 blocker)
//! Some examples carry a `savedSources` entry whose `filePath` is a **relative**
//! file name (e.g. `png.rcx` → `sample.png`). [`RcxDocument::load`] resolves
//! that path against the `.rcx`'s own directory, and the controller's
//! `ingestPendingSavedSources` then auto-attaches it. But if only the `.rcx` is
//! written to the temp dir, `<tempdir>/sample.png` does not exist, the
//! auto-attach bails (the missing-source warn), and every typed value renders
//! as `0x0`. So [`write_example_to_temp`] also materializes each example's
//! embedded sidecar data file **next to** the temp `.rcx`, so the relative
//! `filePath` resolves and the provider attaches with real bytes.

use std::path::{Path, PathBuf};

/// A bundled example's sidecar data file: `(relative_file_name, bytes)`.
///
/// The bytes are embedded at compile time (via [`include_bytes!`]) and written
/// next to the temp `.rcx` under the SAME relative name the example's
/// `savedSources[*].filePath` references, so [`RcxDocument::load`]'s
/// resolve-relative-to-`.rcx`-dir step finds them.
type Sidecar = (&'static str, &'static [u8]);

/// The embedded sidecar data files keyed by example display name.
///
/// Only examples whose `savedSources` reference a relative on-disk file need an
/// entry; the rest are tree-only (no provider). Add a file to
/// `assets/examples/` + a line here to bundle a new sidecar.
const SIDECARS: &[(&str, &[Sidecar])] = &[(
    "png",
    &[(
        "sample.png",
        include_bytes!("../../assets/examples/sample.png"),
    )],
)];

/// The sidecar data files an example bundles (empty for tree-only examples).
fn sidecars_for(name: &str) -> &'static [Sidecar] {
    SIDECARS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .unwrap_or(&[])
}

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
/// [`MainWindow::open_project`](crate::ui::window::MainWindow::open_project) path.
///
/// Crucially it ALSO writes each of the example's embedded sidecar data files
/// ([`sidecars_for`]) **next to** the `.rcx` (under the relative name its
/// `savedSources[*].filePath` references) so the controller's auto-attach finds
/// the data and the document loads with real values instead of all-`0x0` (the
/// headline data-source blocker). The sidecars are written into a per-example
/// subdirectory so each example's relative filePath resolves cleanly and
/// distinct examples never collide.
///
/// Returns `None` if `name` is unknown or the write fails.
pub fn write_example_to_temp(name: &str) -> Option<PathBuf> {
    let json = example_json(name)?;
    let sidecars = sidecars_for(name);
    let dir = example_temp_dir(name);
    std::fs::create_dir_all(&dir).ok()?;
    // Write the sidecar data file(s) FIRST so the .rcx's relative filePath
    // resolves the instant the document loads + auto-attaches its saved source.
    for (rel_name, bytes) in sidecars {
        write_sidecar(&dir, rel_name, bytes)?;
    }
    let path = dir.join(format!("{name}.rcx"));
    std::fs::write(&path, json).ok()?;
    Some(path)
}

/// The per-call temp directory holding the materialized `.rcx` + sidecars.
///
/// `<temp_dir>/reclass-example-<name>-<unique>/` — a fresh, per-call folder so
/// each example's relative `filePath` (e.g. `sample.png`) resolves against its
/// own directory, two examples with same-named sidecars cannot clobber each
/// other, AND re-opening the same example (or a parallel open) never races on a
/// shared directory. The `<unique>` suffix is a process id + monotonic counter.
fn example_temp_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("reclass-example-{name}-{pid}-{nanos}-{n}"))
}

/// Write one sidecar data file `rel_name` (relative to the example dir) with
/// `bytes`. `rel_name` is taken to be a plain file name from the trusted
/// embedded table; any leading directory components are flattened to the file
/// name so a malformed entry cannot escape `dir`. Returns `Some(())` on success.
fn write_sidecar(dir: &Path, rel_name: &str, bytes: &[u8]) -> Option<()> {
    let file = Path::new(rel_name).file_name()?;
    std::fs::write(dir.join(file), bytes).ok()
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
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn png_example_materializes_its_sidecar_next_to_the_rcx() {
        // The headline data-source blocker: png.rcx's saved source filePath is
        // the RELATIVE "sample.png"; it must exist next to the materialized .rcx
        // or the auto-attach bails and every value renders 0x0.
        let path = write_example_to_temp("png").expect("png temp write should succeed");
        let dir = path.parent().expect("rcx has a parent dir");
        let sidecar = dir.join("sample.png");
        assert!(
            sidecar.exists(),
            "sample.png must be materialized next to png.rcx at {sidecar:?}"
        );
        // And it must be the real embedded bytes (non-empty), not a stub.
        let bytes = std::fs::read(&sidecar).expect("sidecar readable");
        assert!(
            !bytes.is_empty(),
            "sample.png sidecar must carry real bytes"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tree_only_example_writes_no_sidecars() {
        // An example with no savedSources should materialize just the .rcx.
        assert!(sidecars_for("AlignmentBugDemo").is_empty());
        let path = write_example_to_temp("AlignmentBugDemo").expect("temp write should succeed");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sidecar_lookup_is_exact() {
        assert_eq!(sidecars_for("png").len(), 1);
        assert_eq!(sidecars_for("png")[0].0, "sample.png");
        assert!(sidecars_for("not-an-example").is_empty());
    }
}
