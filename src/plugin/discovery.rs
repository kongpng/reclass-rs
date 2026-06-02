//! `discovery` — scan plugin directories for `kind = native` bundles and load
//! them (design §6 Phase 3 "Discover + load `kind = native` from `plugins/`").
//!
//! Mirrors the C++ `LoadPlugins()` folder scan (cpp_reference §2) with the design's
//! [+] improvements:
//! - **Multiple dirs** with precedence (design §7.C [+]): a bundled `plugins/`
//!   beside the exe **and** a user dir (`~/.config/reclass/plugins`) — C++ only
//!   checks `Plugins/` beside the exe.
//! - The C++ **`rcx_payload` skip** is honored exactly (cpp_reference §2 — skip
//!   files whose basename starts with `rcx_payload`, a remote-inject payload that
//!   would spawn a rogue thread).
//! - Platform shared-lib extension filter (`.so`/`.dll`/`.dylib`).
//!
//! Gated behind the `plugins` cargo feature.

use std::path::{Path, PathBuf};

use crate::plugin::contract::Plugin;
use crate::plugin::loader::{load_native_plugin, LoadError};

/// The shared-library extension for the current platform (the C++ platform filter,
/// cpp_reference §2).
pub fn platform_lib_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// Whether `path` is a loadable native plugin candidate: the right extension and
/// **not** a `rcx_payload*` file (the C++ skip, cpp_reference §2).
pub fn is_plugin_candidate(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if !ext.eq_ignore_ascii_case(platform_lib_extension()) {
        return false;
    }
    let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    // Skip the remote-inject payload (cpp_reference §2). The C++ checks the
    // basename prefix; `lib`-prefixed Unix names are also covered (a payload
    // `librcx_payload.so` would start with `lib`, so check both forms).
    let normalized = stem.strip_prefix("lib").unwrap_or(stem);
    if normalized.starts_with("rcx_payload") || stem.starts_with("rcx_payload") {
        return false;
    }
    true
}

/// The default plugin directories, in precedence order (design §7.C [+]):
/// 1. a `plugins/` directory beside the running executable (the C++ `Plugins/`),
/// 2. the user config dir `~/.config/reclass/plugins` (cross-platform via
///    `directories`).
///
/// Only existing directories are returned.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.join("plugins"));
        }
    }

    if let Some(proj) = directories::ProjectDirs::from("", "", "reclass") {
        dirs.push(proj.config_dir().join("plugins"));
    }

    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

/// Scan a single directory for native plugin candidates, returning the paths to
/// load (sorted for deterministic order — the C++ listing order is registration
/// order, cpp_reference §2/§3). Non-directories yield an empty list.
pub fn scan_dir(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_plugin_candidate(&path) {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Load every native plugin found across `dirs` (each scanned with [`scan_dir`]).
/// Returns the successfully-loaded plugins plus the `(path, error)` failures, so
/// the caller can surface load/ABI-mismatch errors with detail (design §7.A [fix]
/// — structured, surfaced errors, not a "check the console" box). The host then
/// feeds the loaded plugins to
/// [`PluginManager::add_plugin`](crate::plugin::manager::PluginManager::add_plugin).
pub fn load_from_dirs(dirs: &[PathBuf]) -> (Vec<Box<dyn Plugin>>, Vec<(PathBuf, LoadError)>) {
    let mut loaded = Vec::new();
    let mut failures = Vec::new();
    for dir in dirs {
        for path in scan_dir(dir) {
            match load_native_plugin(&path) {
                Ok(p) => loaded.push(p),
                Err(e) => failures.push((path, e)),
            }
        }
    }
    (loaded, failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_filter_honors_extension_and_rcx_payload_skip() {
        let ext = platform_lib_extension();
        // A normal plugin with the platform extension is a candidate.
        let good = PathBuf::from(format!("/p/example_provider.{ext}"));
        assert!(is_plugin_candidate(&good));
        let good_lib = PathBuf::from(format!("/p/libexample_provider.{ext}"));
        assert!(is_plugin_candidate(&good_lib));

        // The rcx_payload skip (the C++ remote-inject guard), both bare and
        // lib-prefixed.
        let payload = PathBuf::from(format!("/p/rcx_payload_x64.{ext}"));
        assert!(!is_plugin_candidate(&payload));
        let payload_lib = PathBuf::from(format!("/p/librcx_payload.{ext}"));
        assert!(!is_plugin_candidate(&payload_lib));

        // Wrong extension / no extension are not candidates.
        assert!(!is_plugin_candidate(Path::new("/p/example.txt")));
        assert!(!is_plugin_candidate(Path::new("/p/example")));
    }

    #[test]
    fn scan_dir_finds_candidates_and_skips_payloads() {
        let ext = platform_lib_extension();
        let dir = std::env::temp_dir().join(format!("rcx-disc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join(format!("a_plugin.{ext}")), b"x").unwrap();
        std::fs::write(dir.join(format!("rcx_payload.{ext}")), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();

        let found = scan_dir(&dir);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [format!("a_plugin.{ext}")]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_missing_dir_is_empty() {
        let missing = std::env::temp_dir().join("rcx-disc-nope-zzz-does-not-exist");
        assert!(scan_dir(&missing).is_empty());
    }

    #[test]
    fn load_from_empty_dirs_yields_nothing() {
        let (loaded, failures) = load_from_dirs(&[]);
        assert!(loaded.is_empty());
        assert!(failures.is_empty());
    }
}
