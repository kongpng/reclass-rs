//! `discovery` — scan plugin directories, **sniff** each candidate library's
//! exports, and **route** it to the right loader (design §6 Phase 3/4, §4
//! discovery "Each `.dll`/`.so` is sniffed … and routed to our loader, native
//! compat, or managed compat").
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
//! ## The sniff/route funnel (design §4)
//!
//! P3 loaded every candidate blindly through the `abi_stable`
//! [`load_native_plugin`]. P4 inserts a [`sniff`] classification step
//! ([`RcNetClass`]):
//!
//! 1. **Our `abi_stable` format wins first** — attempt our root-module load; if it
//!    succeeds the library is ours (an `abi_stable` plugin), routed to the
//!    unchanged P3 loader. *Our-format always wins to avoid a same-process
//!    collision* (a library could in principle export both surfaces).
//! 2. Else, open with `libloading` and probe the **8 ReClass.NET CoreFunctions**;
//!    if the 4 required resolve → route to the
//!    [`reclassnet`](crate::plugin::reclassnet) native bridge.
//! 3. Else, if the library is a **managed .NET assembly** (PE CLR data-directory
//!    index 14 non-zero) → route to the
//!    [`reclassnet`](crate::plugin::reclassnet) **managed** loader (P5): on Windows
//!    it hosts the .NET FW4 CLR + the C# bridge; a node-type / UI plugin is detected
//!    there and surfaced as the logged "node-type plugin unsupported" skip; on
//!    non-Windows it is the Windows-only stub. A managed outcome is a
//!    [`LoadError::RcNetManaged`] skip.
//! 4. Else → recorded as an unrecognized library.
//!
//! Gated behind the `plugins` cargo feature.

use std::path::{Path, PathBuf};

use crate::plugin::contract::Plugin;
use crate::plugin::loader::{load_native_plugin, LoadError};
use crate::plugin::reclassnet::{load_reclassnet_managed, load_reclassnet_native, RcNetFunctions};

/// The shared-library extension for the current platform (the C++ platform filter,
/// cpp_reference §2).
pub fn platform_lib_extension() -> &'static str {
    crate::plugin::platform_lib_extension()
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

/// The classification of a candidate library by [`sniff`] (design §4 discovery).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcNetClass {
    /// Our own `abi_stable` root-module plugin → the P3 [`load_native_plugin`].
    /// (Our format wins first to avoid a same-process collision — design §4.)
    OurFormat,
    /// Exports the 8 ReClass.NET CoreFunctions (the 4 required resolve) → the
    /// [`reclassnet`](crate::plugin::reclassnet) native bridge.
    ReclassNetNative,
    /// A managed .NET assembly (PE CLR data-directory non-zero) → the
    /// [`reclassnet`](crate::plugin::reclassnet) **managed** loader (Phase 5,
    /// Windows-bridged / Linux-stubbed).
    ManagedAssembly,
    /// Matched no known plugin format.
    Unrecognized,
}

/// Classify a candidate library (design §4 discovery). Order matters: our
/// `abi_stable` format is probed **first** (it wins to avoid a same-process
/// collision), then the ReClass.NET CoreFunctions, then the managed-assembly PE
/// check. This does **not** load the plugin for real (beyond the unavoidable
/// `dlopen` each probe performs); it only decides the route.
pub fn sniff(path: &Path) -> RcNetClass {
    // 1. Our format wins first. `load_native_plugin` performs the abi_stable
    //    root-module init (+ version/layout check); success ⇒ it's ours. We drop
    //    the loaded plugin here — `load_one` reloads via the same path — because
    //    the abi_stable library statics are leaked (stay mapped) regardless, so a
    //    second `load_native_plugin` on the same path is cheap + correct.
    if load_native_plugin(path).is_ok() {
        return RcNetClass::OurFormat;
    }

    // 2. The 8 ReClass.NET CoreFunctions (the 4 required resolvable).
    if exports_reclassnet_corefunctions(path) {
        return RcNetClass::ReclassNetNative;
    }

    // 3. A managed .NET assembly → Phase-5 skip.
    if is_dotnet_assembly(path) {
        return RcNetClass::ManagedAssembly;
    }

    RcNetClass::Unrecognized
}

/// Whether `path` opens as a library exporting the 4 **required** ReClass.NET
/// CoreFunctions (the sniffer's native-compat decision, design §4). Opening the
/// library runs its initializers — the inherent risk of any in-process loader
/// (design §2); this is the same `dlopen` the bridge would do.
fn exports_reclassnet_corefunctions(path: &Path) -> bool {
    // SAFETY: loading an arbitrary native library runs its initializers (design §2
    // full-trust). We only resolve symbols + drop the handle.
    let Ok(lib) = (unsafe { libloading::Library::new(path) }) else {
        return false;
    };
    match RcNetFunctions::resolve(&lib) {
        Ok(table) => table.has_required(),
        Err(_) => false,
    }
}

/// Whether `path` is a managed .NET assembly: a PE with a non-zero CLR
/// data-directory (`IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR`, index **14**) — the C++
/// `isDotNetAssembly` (cpp_reference §8 "Native/managed decided per-DLL by
/// `isDotNetAssembly`"). We parse the PE headers directly (no `dlopen`, so a
/// foreign-arch / managed DLL can't run init code), reading just enough of the
/// optional header to reach the data directories. Non-PE files (ELF `.so`,
/// Mach-O) are not .NET assemblies → `false`.
fn is_dotnet_assembly(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    parse_clr_directory_present(&bytes)
}

/// Pure parse of the PE optional header to test whether the CLR data directory
/// (index 14) has a non-zero RVA + size (the C++ `isDotNetAssembly` check). Split
/// out so it is unit-testable on hand-built header bytes (no real DLL needed).
fn parse_clr_directory_present(bytes: &[u8]) -> bool {
    // DOS header: "MZ" magic @0, e_lfanew (offset to PE header) @0x3C (u32 LE).
    if bytes.len() < 0x40 || &bytes[0..2] != b"MZ" {
        return false;
    }
    let e_lfanew =
        u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;

    // PE signature "PE\0\0" at e_lfanew, then the 20-byte COFF file header.
    let sig = e_lfanew;
    if bytes.len() < sig + 4 + 20 || &bytes[sig..sig + 4] != b"PE\0\0" {
        return false;
    }
    let opt_header = sig + 4 + 20;
    if bytes.len() < opt_header + 2 {
        return false;
    }

    // Optional-header magic: 0x10b = PE32, 0x20b = PE32+. The data directories
    // start at a magic-dependent offset; NumberOfRvaAndSizes precedes them.
    let magic = u16::from_le_bytes([bytes[opt_header], bytes[opt_header + 1]]);
    // Offset (within the optional header) of `NumberOfRvaAndSizes`, then the
    // directory array. (Per the PE spec: PE32 puts the 8-byte BaseOfData +
    // 32-bit ImageBase, so the dir array starts at 96; PE32+ at 112.)
    let (num_dirs_off, dirs_off) = match magic {
        0x10b => (92usize, 96usize),   // PE32
        0x20b => (108usize, 112usize), // PE32+
        _ => return false,
    };
    let num_dirs_at = opt_header + num_dirs_off;
    if bytes.len() < num_dirs_at + 4 {
        return false;
    }
    let num_dirs = u32::from_le_bytes([
        bytes[num_dirs_at],
        bytes[num_dirs_at + 1],
        bytes[num_dirs_at + 2],
        bytes[num_dirs_at + 3],
    ]);
    const CLR_INDEX: u32 = 14; // IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR
    if num_dirs <= CLR_INDEX {
        return false;
    }
    // Each data directory is 8 bytes: { VirtualAddress: u32, Size: u32 }.
    let clr_at = opt_header + dirs_off + (CLR_INDEX as usize) * 8;
    if bytes.len() < clr_at + 8 {
        return false;
    }
    let rva = u32::from_le_bytes([
        bytes[clr_at],
        bytes[clr_at + 1],
        bytes[clr_at + 2],
        bytes[clr_at + 3],
    ]);
    let size = u32::from_le_bytes([
        bytes[clr_at + 4],
        bytes[clr_at + 5],
        bytes[clr_at + 6],
        bytes[clr_at + 7],
    ]);
    rva != 0 && size != 0
}

/// Load a single candidate by sniffing + routing it (design §4). Returns the
/// loaded plugin, or a [`LoadError`] describing why it was not loaded. A managed
/// assembly is a Phase-5 [`LoadError::RcNetManaged`] skip with an explanatory
/// note. The C++ `LoadPlugin`, generalized to multiple loaders (reference §8).
pub fn load_one(path: &Path) -> Result<Box<dyn Plugin>, LoadError> {
    match sniff(path) {
        RcNetClass::OurFormat => load_native_plugin(path),
        RcNetClass::ReclassNetNative => load_reclassnet_native(path).map_err(LoadError::RcNet),
        // Phase 5: route to the managed (.NET) CLR-host loader. On Windows it hosts
        // the .NET FW4 CLR + the C# bridge; a node-type / UI plugin (no
        // ICoreProcessFunctions) is detected there and surfaced as the logged
        // "node-type plugin unsupported" skip (design §8). On non-Windows the
        // loader is a stub returning the Windows-only message. Either way a managed
        // outcome is a `RcNetManaged` skip, not a crash.
        RcNetClass::ManagedAssembly => {
            load_reclassnet_managed(path).map_err(LoadError::RcNetManaged)
        }
        RcNetClass::Unrecognized => Err(LoadError::Unrecognized(format!(
            "'{}' is neither an abi_stable Reclass plugin nor a ReClass.NET native \
             plugin (the 8 CoreFunctions)",
            path.display()
        ))),
    }
}

/// Scan + sniff + load every plugin across `dirs` (each scanned with [`scan_dir`],
/// each routed by [`load_one`]). Returns the successfully-loaded plugins plus the
/// `(path, error)` failures, so the caller can surface load / ABI-mismatch /
/// not-a-plugin errors with detail (design §7.A [fix] — structured, surfaced
/// errors, not a "check the console" box). The host then feeds the loaded plugins
/// to [`PluginManager::add_plugin`](crate::plugin::manager::PluginManager::add_plugin).
pub fn load_from_dirs(dirs: &[PathBuf]) -> (Vec<Box<dyn Plugin>>, Vec<(PathBuf, LoadError)>) {
    let mut loaded = Vec::new();
    let mut failures = Vec::new();
    for dir in dirs {
        for path in scan_dir(dir) {
            match load_one(&path) {
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
    use crate::plugin::example_plugin_path;

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

    // ── P4 sniff/route ──

    /// Build a minimal PE optional-header byte buffer with a chosen CLR
    /// data-directory (index 14) RVA/size, for testing `parse_clr_directory_present`
    /// without a real DLL. `pe32_plus` selects the PE32+ (0x20b) layout.
    fn synth_pe(pe32_plus: bool, clr_rva: u32, clr_size: u32) -> Vec<u8> {
        let (magic, num_dirs_off, dirs_off) = if pe32_plus {
            (0x20bu16, 108usize, 112usize)
        } else {
            (0x10bu16, 92usize, 96usize)
        };
        // Lay the PE header at a fixed e_lfanew.
        let e_lfanew = 0x80usize;
        let opt_header = e_lfanew + 4 + 20;
        let total = opt_header + dirs_off + 16 * 8;
        let mut b = vec![0u8; total];
        b[0] = b'M';
        b[1] = b'Z';
        b[0x3c..0x40].copy_from_slice(&(e_lfanew as u32).to_le_bytes());
        b[e_lfanew..e_lfanew + 4].copy_from_slice(b"PE\0\0");
        b[opt_header..opt_header + 2].copy_from_slice(&magic.to_le_bytes());
        // NumberOfRvaAndSizes = 16 (the standard count).
        b[opt_header + num_dirs_off..opt_header + num_dirs_off + 4]
            .copy_from_slice(&16u32.to_le_bytes());
        // CLR directory (index 14): { rva, size }.
        let clr_at = opt_header + dirs_off + 14 * 8;
        b[clr_at..clr_at + 4].copy_from_slice(&clr_rva.to_le_bytes());
        b[clr_at + 4..clr_at + 8].copy_from_slice(&clr_size.to_le_bytes());
        b
    }

    #[test]
    fn clr_directory_detection_matches_cpp_isdotnet() {
        // A managed assembly: non-zero CLR directory (both PE32 and PE32+).
        assert!(parse_clr_directory_present(&synth_pe(false, 0x2000, 0x48)));
        assert!(parse_clr_directory_present(&synth_pe(true, 0x2000, 0x48)));

        // A native PE: zero CLR directory → not managed.
        assert!(!parse_clr_directory_present(&synth_pe(true, 0, 0)));
        assert!(!parse_clr_directory_present(&synth_pe(false, 0x2000, 0))); // size 0
        assert!(!parse_clr_directory_present(&synth_pe(false, 0, 0x48))); // rva 0

        // Non-PE inputs (ELF magic, too short, garbage) are not assemblies.
        assert!(!parse_clr_directory_present(b"\x7fELF........"));
        assert!(!parse_clr_directory_present(b"MZ"));
        assert!(!parse_clr_directory_present(&[]));
    }

    #[test]
    fn sniff_unrecognized_for_a_garbage_file() {
        // A non-library file routes to Unrecognized (neither abi_stable, nor the
        // CoreFunctions, nor a .NET assembly).
        let dir = std::env::temp_dir().join(format!("rcx-sniff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let ext = platform_lib_extension();
        let junk = dir.join(format!("not_a_plugin.{ext}"));
        std::fs::write(&junk, b"this is not a shared library").unwrap();

        assert_eq!(sniff(&junk), RcNetClass::Unrecognized);
        // load_one surfaces a structured "not a recognized plugin" error.
        match load_one(&junk) {
            Ok(_) => panic!("expected an error for a junk file"),
            Err(LoadError::Unrecognized(msg)) => {
                assert!(msg.contains("neither"), "got: {msg}");
            }
            Err(other) => panic!("expected Unrecognized, got: {other}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_one_routes_a_synthetic_managed_assembly_to_the_managed_loader() {
        // A file that parses as a managed PE assembly → ManagedAssembly class →
        // the Phase-5 managed loader. We write a synthetic PE with a .dll extension;
        // `sniff` will fail the abi_stable + CoreFunctions probes (it isn't a real
        // loadable lib) and reach the PE parse, which sees the CLR directory.
        let dir = std::env::temp_dir().join(format!("rcx-managed-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let managed = dir.join("managed_plugin.dll");
        std::fs::write(&managed, synth_pe(true, 0x2000, 0x48)).unwrap();

        assert_eq!(sniff(&managed), RcNetClass::ManagedAssembly);
        // The managed route returns a `RcNetManaged` outcome (never `Ok` here, and
        // never `Unrecognized`): on non-Windows it is the Windows-only stub; on
        // Windows it fails fast (no RcNetBridge.dll beside this synthetic file, or
        // no .NET FW4). Either way it is a benign managed skip, not a crash.
        match load_one(&managed) {
            Ok(_) => panic!("this synthetic managed assembly must not load"),
            Err(LoadError::RcNetManaged(msg)) => {
                assert!(!msg.is_empty(), "managed skip must carry a reason");
                #[cfg(not(windows))]
                assert!(msg.contains("Windows-only"), "got: {msg}");
            }
            Err(other) => panic!("expected a RcNetManaged skip, got: {other}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── End-to-end: the reclassnet-fake-native cdylib loaded via the folder scan ──

    /// Locate the built `reclassnet-fake-native` cdylib (mirrors the loader test's
    /// `example_plugin_path`). Returns `None` if it hasn't been built, so the e2e
    /// test **skips** rather than fails where the example wasn't compiled.
    fn fake_native_path() -> Option<PathBuf> {
        let file = crate::plugin::platform_lib_filename("reclassnet-fake-native");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        [
            root.join("target/debug").join(&file),
            root.join("target/release").join(&file),
        ]
        .into_iter()
        .find(|p| p.is_file())
    }

    #[test]
    fn sniff_classifies_our_abi_stable_plugin_as_our_format_first() {
        let Some(path) = example_plugin_path("example-provider") else {
            eprintln!("skipping: example-provider cdylib not built");
            return;
        };
        // Our abi_stable plugin is classified OurFormat (not mistaken for a
        // ReClass.NET native plugin) — the funnel probes our format first.
        assert_eq!(sniff(&path), RcNetClass::OurFormat);
        // And `load_one` routes it through the P3 loader successfully.
        match load_one(&path) {
            Ok(p) => assert_eq!(p.manifest().identifier(), "exampleprovider"),
            Err(e) => panic!("our-format plugin must load via load_one: {e}"),
        }
    }

    #[test]
    fn reclassnet_native_plugin_loads_via_folder_scan_and_reads_end_to_end() {
        use crate::plugin::manager::PluginManager;
        use crate::plugin::reclassnet::RECLASSNET_IDENTIFIER;
        use crate::provider::RegionType;

        let Some(built) = fake_native_path() else {
            eprintln!("skipping: reclassnet-fake-native cdylib not built");
            return;
        };

        // Stage the plugin in a fresh plugins dir + scan it as the host would.
        let ext = platform_lib_extension();
        let dir = std::env::temp_dir().join(format!("rcx-rcnet-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let staged = dir.join(format!("reclassnet_fake_native.{ext}"));
        std::fs::copy(&built, &staged).expect("stage the cdylib");

        // Sniff classifies it as a ReClass.NET native plugin (not our format,
        // not managed).
        assert_eq!(sniff(&staged), RcNetClass::ReclassNetNative);

        // Load every plugin in the dir through the SAME manager path a built-in
        // uses; the bridged source registers under `reclass.netcompatlayer`.
        let mut mgr = PluginManager::new();
        let failures = mgr.load_native_plugins_from_dirs(&[dir.clone()]);
        assert!(
            failures.is_empty(),
            "expected no load failures, got: {failures:?}"
        );
        assert!(
            mgr.registry().find(RECLASSNET_IDENTIFIER).is_some(),
            "the bridged source must register under the reclassnet identifier"
        );

        // The plugin advertises a process list (the C++ enumerateProcesses).
        let spec = mgr.provider_spec(RECLASSNET_IDENTIFIER).expect("spec");
        assert!(spec.can_handle("dll|4321:fake-target.exe"));
        let procs = spec.enumerate_processes().expect("process list");
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, 4321);
        assert_eq!(procs[0].name, "fake-target.exe");

        // Attach: create the provider for "dllpath|pid:name" and read end-to-end
        // through the bridged ReadRemoteMemory (the ramp buffer).
        let target = format!("{}|4321:fake-target.exe", staged.display());
        let prov = match mgr.create_provider(RECLASSNET_IDENTIFIER, &target) {
            Ok(p) => p,
            Err(e) => panic!("create_provider: {e}"),
        };
        assert_eq!(prov.kind(), "RcNet");
        assert!(prov.is_live());
        assert_eq!(prov.name(), "fake-target.exe");

        // Read the ramp (byte i == (addr+i) as u8) — the e2e read assertion.
        let mut buf = [0u8; 4];
        assert!(prov.read(0x100, &mut buf));
        assert_eq!(buf, [0x00, 0x01, 0x02, 0x03]);
        let mut buf2 = [0u8; 3];
        assert!(prov.read(0xfd, &mut buf2));
        assert_eq!(buf2, [0xfd, 0xfe, 0xff]);

        // Section/module enumeration works end-to-end (design §7.A [fix] (2)):
        // one Image r-x region + one Private rw- region.
        let regions = prov.enumerate_regions();
        assert_eq!(regions.len(), 2, "got: {regions:?}");
        let image = regions
            .iter()
            .find(|r| r.region_type == RegionType::Image)
            .expect("image region");
        assert_eq!(image.base, 0x1000);
        assert!(image.readable && image.executable && !image.writable);
        assert_eq!(image.module_name, ".text");
        let private = regions
            .iter()
            .find(|r| r.region_type == RegionType::Private)
            .expect("private region");
        assert_eq!(private.base, 0x8000);
        assert!(private.readable && private.writable && !private.executable);

        // Base = the first enumerated module base (the C++ rule); symbol scan
        // resolves an address inside fake.exe.
        assert_eq!(prov.base(), 0x1000);
        assert_eq!(prov.get_symbol(0x1500), "fake.exe+0x500");
        assert_eq!(prov.symbol_to_address("fake.exe"), 0x1000);

        // [fix] (3): size = the region span (0x9000 − 0x1000), not the 0x10000
        // sentinel.
        assert_eq!(prov.size(), 0x8000);
        // Writable (the plugin exports WriteRemoteMemory).
        assert!(prov.is_writable());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
