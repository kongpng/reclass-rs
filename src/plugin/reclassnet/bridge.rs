//! `bridge` — the `libloading` loader for a ReClass.NET **native** plugin: open
//! the `.so`/`.dll`, resolve the 8 CoreFunctions into [`RcNetFunctions`], validate
//! the 4 required exports, and wrap the result as a host [`Plugin`] whose single
//! [`Contribution::Provider`] builds a [`RcNetProvider`] (the C++ `loadNativeDll`
//! + `RcNetCompatPlugin`, reference §8 "Native path").
//!
//! ### Library lifetime (mirrors C++ never unloading)
//!
//! The resolved table holds raw `extern "C"` fn pointers into the loaded library;
//! they are valid only while the library stays mapped. The bridged [`Plugin`]
//! keeps the [`libloading::Library`] in an `Arc` for the process lifetime (C++
//! never `dlclose`s a compat plugin — reference §8), so the table stays valid for
//! every later `create_provider`/`enumerate_processes` call. The provider does
//! **not** hold the `Arc`; it relies on the plugin (registered in the manager)
//! outliving it — the same model as the C++, where the provider holds the table by
//! value and the plugin owns the `QLibrary`.
//!
//! Gated behind the `plugins` cargo feature (via the parent module).

use std::path::Path;
use std::sync::Arc;

use libloading::{Library, Symbol};

use crate::plugin::contract::{Contribution, Plugin, ProcessInfo};
use crate::plugin::manifest::{LoadType, Permission, PluginKind, PluginManifest};
use crate::plugin::provider_spec::{ProviderSpec, SharedProvider};
use crate::plugin::reclassnet::ffi::{
    decode_utf16_fixed, EnumerateProcessData, FnCloseRemoteProcess, FnControlRemoteProcess,
    FnEnumerateProcesses, FnEnumerateRemoteSectionsAndModules, FnIsProcessValid,
    FnOpenRemoteProcess, FnReadRemoteMemory, FnWriteRemoteMemory, CORE_FUNCTION_NAMES,
};
use crate::plugin::reclassnet::provider::RcNetProvider;
use crate::plugin::reclassnet::{RECLASSNET_IDENTIFIER, RECLASSNET_NAME};

/// The resolved ReClass.NET CoreFunctions table (the C++ `RcNetFunctions`,
/// reference §8). Each entry is `Option` because the **optional** 4 exports
/// (`IsProcessValid`, `WriteRemoteMemory`, `EnumerateRemoteSectionsAndModules`,
/// `ControlRemoteProcess`) are nullptr-tolerant; the **required** 4
/// (`EnumerateProcesses`, `OpenRemoteProcess`, `CloseRemoteProcess`,
/// `ReadRemoteMemory`) are validated `Some` by [`RcNetFunctions::resolve`].
///
/// The fn pointers are `Copy`, so the table is trivially clonable into the
/// provider + the plugin's factory closures.
#[derive(Clone, Copy)]
pub struct RcNetFunctions {
    pub enumerate_processes: Option<FnEnumerateProcesses>,
    pub open_remote_process: Option<FnOpenRemoteProcess>,
    pub is_process_valid: Option<FnIsProcessValid>,
    pub close_remote_process: Option<FnCloseRemoteProcess>,
    pub read_remote_memory: Option<FnReadRemoteMemory>,
    pub write_remote_memory: Option<FnWriteRemoteMemory>,
    pub enumerate_sections_and_modules: Option<FnEnumerateRemoteSectionsAndModules>,
    pub control_remote_process: Option<FnControlRemoteProcess>,
}

// The table is a bundle of plain fn pointers; sharing it across threads is sound
// (the providers it backs are `unsafe impl Send + Sync`).
unsafe impl Send for RcNetFunctions {}
unsafe impl Sync for RcNetFunctions {}

impl RcNetFunctions {
    /// Resolve all 8 CoreFunctions from `lib` by their [`CORE_FUNCTION_NAMES`] and
    /// validate the **4 required** exports (the C++ `loadNativeDll`, reference §8;
    /// design §7.A [fix] — a missing required export is a surfaced error string,
    /// not a "check the console").
    ///
    /// Missing optional exports stay `None` (nullptr-tolerant, C++ parity).
    pub fn resolve(lib: &Library) -> Result<RcNetFunctions, String> {
        // SAFETY: each symbol is a `extern "C"` fn of the typedef'd shape (the
        // ReClass.NET ABI). `libloading` returns `None`/err for a missing symbol;
        // we transmute the resolved address to the fn-pointer type. The plugin
        // author guarantees the signature (the same trust model as the C++
        // `resolve` + `reinterpret_cast`).
        unsafe fn get<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
            let sym: Result<Symbol<T>, _> = lib.get(name);
            sym.ok().map(|s| *s)
        }

        // SAFETY: see `get`; the index→type mapping follows `CORE_FUNCTION_NAMES`.
        let table = unsafe {
            RcNetFunctions {
                enumerate_processes: get::<FnEnumerateProcesses>(lib, CORE_FUNCTION_NAMES[0]),
                open_remote_process: get::<FnOpenRemoteProcess>(lib, CORE_FUNCTION_NAMES[1]),
                is_process_valid: get::<FnIsProcessValid>(lib, CORE_FUNCTION_NAMES[2]),
                close_remote_process: get::<FnCloseRemoteProcess>(lib, CORE_FUNCTION_NAMES[3]),
                read_remote_memory: get::<FnReadRemoteMemory>(lib, CORE_FUNCTION_NAMES[4]),
                write_remote_memory: get::<FnWriteRemoteMemory>(lib, CORE_FUNCTION_NAMES[5]),
                enumerate_sections_and_modules: get::<FnEnumerateRemoteSectionsAndModules>(
                    lib,
                    CORE_FUNCTION_NAMES[6],
                ),
                control_remote_process: get::<FnControlRemoteProcess>(lib, CORE_FUNCTION_NAMES[7]),
            }
        };

        table.validate_required()?;
        Ok(table)
    }

    /// The 4 required exports are present (the C++ minimum of read, open, close,
    /// and enumerate, reference §8). Returns the C++-style error string naming the
    /// missing set on failure.
    pub fn validate_required(&self) -> Result<(), String> {
        let mut missing = Vec::new();
        if self.read_remote_memory.is_none() {
            missing.push("ReadRemoteMemory");
        }
        if self.open_remote_process.is_none() {
            missing.push("OpenRemoteProcess");
        }
        if self.close_remote_process.is_none() {
            missing.push("CloseRemoteProcess");
        }
        if self.enumerate_processes.is_none() {
            missing.push("EnumerateProcesses");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "DLL is missing required ReClass.NET exports ({}). \
                 Is this a ReClass.NET native plugin?",
                missing.join(", ")
            ))
        }
    }

    /// Whether the table has the 4 required exports (the sniffer's "is this a
    /// ReClass.NET native plugin?" decision — design §4 discovery).
    pub fn has_required(&self) -> bool {
        self.validate_required().is_ok()
    }

    /// Run the process-enumeration callback and collect the processes (the C++
    /// `enumerateProcesses` + its `thread_local` collector, reference §8). Returns
    /// an empty list if the plugin exports no `EnumerateProcesses` (shouldn't
    /// happen post-`resolve`, but defensive).
    pub fn enumerate_processes(&self) -> Vec<ProcessInfo> {
        let Some(enumerate) = self.enumerate_processes else {
            return Vec::new();
        };
        let mut out: Vec<ProcessInfo> = Vec::new();
        PROCESS_SINK.with(|s| *s.borrow_mut() = &mut out as *mut _);
        // Calling the resolved export (an `extern "C" fn`); `process_callback`
        // only touches the thread-local sink set for this synchronous call.
        enumerate(process_callback);
        PROCESS_SINK.with(|s| *s.borrow_mut() = std::ptr::null_mut());
        out
    }
}

// ── thread_local process collector (no user-context ptr in the callback) ──────

thread_local! {
    static PROCESS_SINK: std::cell::RefCell<*mut Vec<ProcessInfo>> =
        const { std::cell::RefCell::new(std::ptr::null_mut()) };
}

/// The process-enumeration callback (the C++ `processCallback`, reference §8).
extern "C" fn process_callback(data: *mut EnumerateProcessData) {
    if data.is_null() {
        return;
    }
    // SAFETY: the plugin hands us a valid pointer to a packed struct for this
    // synchronous call; read it unaligned (the struct is `#[repr(C, packed)]`).
    let data = unsafe { std::ptr::read_unaligned(data) };
    // Copy the packed fields to aligned locals before borrowing the u16 arrays.
    let id = data.id;
    let name_buf = data.name;
    let path_buf = data.path;
    let info = ProcessInfo {
        pid: id as u32,
        name: decode_utf16_fixed(&name_buf),
        path: decode_utf16_fixed(&path_buf),
        // The native CoreFunctions struct carries no bitness; the host's picker
        // determines it (the C++ `is32Bit` is set by the host, not the callback).
        is_32bit: false,
    };
    PROCESS_SINK.with(|sink| {
        let ptr = *sink.borrow();
        if !ptr.is_null() {
            // SAFETY: set to a live `&mut Vec` for the call's duration.
            unsafe { (*ptr).push(info) };
        }
    });
}

// ── The bridged plugin ────────────────────────────────────────────────────────

/// A loaded ReClass.NET native plugin presented as a host [`Plugin`] (the C++
/// `RcNetCompatPlugin`, reference §8). Owns the `libloading::Library` (kept alive
/// for the process — C++ never unloads) and the resolved table; contributes one
/// provider under [`RECLASSNET_IDENTIFIER`].
struct RcNetPlugin {
    manifest: PluginManifest,
    lib: Arc<Library>,
    fns: RcNetFunctions,
}

impl Plugin for RcNetPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contributions(&self) -> Vec<Contribution> {
        // Clone the `Arc` + the (Copy) table into the factory closures so they
        // keep the library mapped for as long as the closures live (the closures
        // are stored in the manager alongside this plugin). The provider built by
        // `create_provider` relies on this plugin staying registered (C++ parity).
        let fns_create = self.fns;
        let _lib_create = Arc::clone(&self.lib);
        let fns_enum = self.fns;

        let spec = ProviderSpec::new(
            // `canHandle`: the C++ `target.contains('|')` (reference §8).
            |target: &str| target.contains('|'),
            move |target: &str| -> Result<SharedProvider, String> {
                // Capture the `Arc` so the library outlives the closure (and thus
                // every provider it makes); the table's fn pointers stay valid.
                let _keep_alive = &_lib_create;
                let parsed = ParsedTarget::parse(target)?;
                let provider = RcNetProvider::open(
                    fns_create,
                    parsed.pid,
                    parsed.name.clone(),
                    parsed.is_32bit,
                )
                .ok_or_else(|| {
                    format!(
                        "Failed to open process {} (PID: {}) via ReClass.NET plugin. \
                         Ensure the process is running and the plugin supports it.",
                        parsed.name, parsed.pid
                    )
                })?;
                Ok(Arc::new(provider) as SharedProvider)
            },
        )
        .with_enumerate(move || {
            // The plugin's process list (the C++ `enumerateProcesses` feeding the
            // host's picker, reference §5/§8). `None` if the plugin reports none.
            let procs = fns_enum.enumerate_processes();
            if procs.is_empty() {
                None
            } else {
                Some(procs)
            }
        });

        vec![Contribution::Provider(spec)]
    }
}

// `RcNetPlugin` holds an `Arc<Library>` (Send+Sync) and an `RcNetFunctions`
// (declared Send+Sync above); the contract requires `Plugin: Send + Sync`.

/// A parsed `"dllpath|pid:name"` target (the C++ `createProvider` parse, reference
/// §8). The dll path is informational here (the library is already loaded by the
/// time `create_provider` runs — the host loaded it during the folder scan), but
/// we preserve the C++ target-string convention so saved sources / the MCP bridge
/// round-trip. The optional trailing `@32`/`@64` is our extension carrying the
/// picker's bitness hint (design §7.A [fix] (1)); absent ⇒ inferred from modules.
struct ParsedTarget {
    pid: u32,
    name: String,
    is_32bit: bool,
}

impl ParsedTarget {
    fn parse(target: &str) -> Result<ParsedTarget, String> {
        let sep = target
            .find('|')
            .ok_or_else(|| "Invalid target format (expected \"dllpath|pid:name\")".to_string())?;
        let pid_part = &target[sep + 1..];

        // "pid:name" (name optional). An optional "@32"/"@64" bitness suffix on the
        // name is our extension; strip it for the name and record the hint.
        let mut parts = pid_part.splitn(2, ':');
        let pid_str = parts.next().unwrap_or("");
        let pid: u32 = pid_str
            .trim()
            .parse()
            .map_err(|_| format!("Invalid PID: {pid_str}"))?;
        if pid == 0 {
            return Err(format!("Invalid PID: {pid_str}"));
        }

        let raw_name = parts.next().unwrap_or("");
        let (name, is_32bit) = match raw_name.rsplit_once('@') {
            Some((n, "32")) => (n.to_string(), true),
            Some((n, "64")) => (n.to_string(), false),
            _ => (raw_name.to_string(), false),
        };
        let name = if name.is_empty() {
            format!("PID {pid}")
        } else {
            name
        };

        Ok(ParsedTarget {
            pid,
            name,
            is_32bit,
        })
    }
}

/// Load a ReClass.NET **native** plugin from `path` and adapt it into a
/// `Box<dyn Plugin>` ready for
/// [`PluginManager::add_plugin`](crate::plugin::manager::PluginManager::add_plugin)
/// (design §6 Phase 4; the router entry — the C++ `loadNativeDll`, reference §8).
///
/// Opens the library (kept alive for the process via the plugin's `Arc<Library>`),
/// resolves the table, validates the 4 required exports, and builds the plugin.
/// Returns a surfaced error string (design §7.A [fix]) on a load / missing-export
/// failure. `dll_file_name` is recorded on the manifest (the C++ `dllFileName`).
pub fn load_reclassnet_native(path: &Path) -> Result<Box<dyn Plugin>, String> {
    // SAFETY: loading an arbitrary native library runs its initializers — the
    // inherent risk of any in-process plugin loader (design §2 "full-trust"). This
    // is the C++ `QLibrary::load()` equivalent.
    let lib = unsafe { Library::new(path) }.map_err(|e| {
        format!(
            "Failed to load ReClass.NET plugin '{}': {e}",
            path.display()
        )
    })?;

    let fns = RcNetFunctions::resolve(&lib)?;

    let dll_file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    let manifest = PluginManifest {
        name: RECLASSNET_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        author: "Reclass (Rust port) — ReClass.NET native compat".to_string(),
        description: format!("Bridged ReClass.NET native plugin ({dll_file_name})."),
        kind: PluginKind::ReclassNet,
        load: LoadType::Auto,
        permissions: vec![
            Permission::ReadMemory,
            Permission::WriteMemory,
            Permission::AddProvider,
        ],
        dll_file_name,
    };
    debug_assert_eq!(manifest.identifier(), RECLASSNET_IDENTIFIER);

    Ok(Box::new(RcNetPlugin {
        manifest,
        lib: Arc::new(lib),
        fns,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::reclassnet::ffi::{ProcessAccess, RcPointer};

    // A minimal in-test table: only the 4 required exports, plus a process
    // enumerator. (The provider-mapping logic itself is tested in `provider.rs`;
    // here we test the table validation + target parsing + the plugin shape.)

    static mut TOK: u8 = 0;
    extern "C" fn open(_id: u64, _a: ProcessAccess) -> RcPointer {
        std::ptr::addr_of_mut!(TOK) as RcPointer
    }
    extern "C" fn close(_h: RcPointer) {}
    extern "C" fn read(
        _h: RcPointer,
        _addr: RcPointer,
        buf: RcPointer,
        _off: i32,
        size: i32,
    ) -> bool {
        if size <= 0 {
            return false;
        }
        // SAFETY (test): zero-fill the provided buffer.
        unsafe { std::ptr::write_bytes(buf as *mut u8, 0, size as usize) };
        true
    }
    extern "C" fn enum_procs(cb: crate::plugin::reclassnet::ffi::EnumerateProcessCallback) {
        let mut d = EnumerateProcessData {
            id: 1234,
            name: [0u16; 260],
            path: [0u16; 260],
        };
        for (i, u) in "notepad.exe".encode_utf16().enumerate() {
            d.name[i] = u;
        }
        cb(&mut d);
    }

    fn required_only_table() -> RcNetFunctions {
        RcNetFunctions {
            enumerate_processes: Some(enum_procs),
            open_remote_process: Some(open),
            is_process_valid: None,
            close_remote_process: Some(close),
            read_remote_memory: Some(read),
            write_remote_memory: None,
            enumerate_sections_and_modules: None,
            control_remote_process: None,
        }
    }

    #[test]
    fn validate_required_passes_with_four_and_tolerates_missing_optionals() {
        let t = required_only_table();
        assert!(t.validate_required().is_ok());
        assert!(t.has_required());
        // Optional ones absent is fine.
        assert!(t.write_remote_memory.is_none());
        assert!(t.control_remote_process.is_none());
    }

    #[test]
    fn validate_required_names_each_missing_export() {
        let mut t = required_only_table();
        t.read_remote_memory = None;
        t.open_remote_process = None;
        let err = t.validate_required().unwrap_err();
        assert!(err.contains("ReadRemoteMemory"), "got: {err}");
        assert!(err.contains("OpenRemoteProcess"), "got: {err}");
        // The two present required ones are not named.
        assert!(!err.contains("CloseRemoteProcess"), "got: {err}");
        assert!(!t.has_required());
    }

    #[test]
    fn enumerate_processes_collects_via_thread_local() {
        let t = required_only_table();
        let procs = t.enumerate_processes();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, 1234);
        assert_eq!(procs[0].name, "notepad.exe");
        // After the call the sink is cleared (a second call still works).
        let again = t.enumerate_processes();
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn parse_target_dllpath_pid_name() {
        let p = ParsedTarget::parse("/plugins/memflow.so|1234:notepad.exe").expect("parse");
        assert_eq!(p.pid, 1234);
        assert_eq!(p.name, "notepad.exe");
        assert!(!p.is_32bit);
    }

    #[test]
    fn parse_target_bitness_suffix_extension() {
        let p32 = ParsedTarget::parse("x|99:game.exe@32").expect("parse");
        assert_eq!(p32.pid, 99);
        assert_eq!(p32.name, "game.exe");
        assert!(p32.is_32bit);

        let p64 = ParsedTarget::parse("x|99:game.exe@64").expect("parse");
        assert!(!p64.is_32bit);
        assert_eq!(p64.name, "game.exe");
    }

    #[test]
    fn parse_target_pid_only_synthesizes_name() {
        let p = ParsedTarget::parse("dll|42").expect("parse");
        assert_eq!(p.pid, 42);
        assert_eq!(p.name, "PID 42");
    }

    #[test]
    fn parse_target_rejects_bad_input() {
        // No '|' separator.
        assert!(ParsedTarget::parse("1234:notepad").is_err());
        // Non-numeric PID.
        assert!(ParsedTarget::parse("dll|abc:x").is_err());
        // Zero PID (the C++ rejects pid == 0).
        assert!(ParsedTarget::parse("dll|0:x").is_err());
    }

    #[test]
    fn bridged_plugin_contributes_one_provider_under_the_identifier() {
        // Build the plugin shape directly (no real library) to test the manifest +
        // the provider contribution's can_handle/enumerate without a .so on disk.
        // We fabricate an Arc<Library> by loading our OWN test binary is not
        // portable; instead test the manifest identity + spec via a hand-built
        // plugin mirroring `load_reclassnet_native`'s output.
        let manifest = PluginManifest {
            name: RECLASSNET_NAME.to_string(),
            version: "0".to_string(),
            author: String::new(),
            description: String::new(),
            kind: PluginKind::ReclassNet,
            load: LoadType::Auto,
            permissions: vec![Permission::ReadMemory],
            dll_file_name: "memflow.so".to_string(),
        };
        // The identifier the source picker keys off.
        assert_eq!(manifest.identifier(), RECLASSNET_IDENTIFIER);

        // The spec's can_handle mirrors the C++ `target.contains('|')`.
        let fns = required_only_table();
        let spec = ProviderSpec::new(
            |target: &str| target.contains('|'),
            move |_t: &str| -> Result<SharedProvider, String> {
                let p = RcNetProvider::open(fns, 1, "p", false).ok_or("open failed")?;
                Ok(Arc::new(p) as SharedProvider)
            },
        )
        .with_enumerate(move || {
            let procs = fns.enumerate_processes();
            (!procs.is_empty()).then_some(procs)
        });
        assert!(spec.can_handle("x|1:y"));
        assert!(!spec.can_handle("nopipe"));
        assert!(spec.provides_process_list());
        let listed = spec.enumerate_processes().unwrap();
        assert_eq!(listed[0].name, "notepad.exe");
        // create_provider builds a working provider (reads zero-filled).
        let prov = spec.create_provider("x|1:y").expect("create");
        let mut b = [0xffu8; 4];
        assert!(prov.read(0, &mut b));
        assert_eq!(b, [0, 0, 0, 0]);
    }

    #[test]
    fn load_missing_file_surfaces_error() {
        // `Box<dyn Plugin>` is not `Debug`, so match instead of `unwrap_err`.
        match load_reclassnet_native(Path::new("/no/such/reclassnet-plugin.so")) {
            Ok(_) => panic!("expected a load error for a missing file"),
            Err(err) => assert!(err.contains("Failed to load"), "got: {err}"),
        }
    }
}
