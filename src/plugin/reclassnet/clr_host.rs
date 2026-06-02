//! `clr_host` — the Windows-only **.NET Framework CLR host** (design §4 managed
//! path, §6 Phase 5, §8 decision; a Rust port of the C++ `ClrHost`, reference §8).
//!
//! This is the single new piece of machinery the managed ReClass.NET compat path
//! adds: it hosts the **.NET Framework v4.0.30319** runtime in-process via the
//! `mscoree` COM surface (the `windows` crate's `Win32_System_ClrHosting`) and
//! calls into the ported C# bridge ([`RcNetBridge.cs`](super::RCNET_BRIDGE_DLL))
//! through `ICLRRuntimeHost::ExecuteInDefaultAppDomain`. The bridge marshals the
//! managed plugin's `ICoreProcessFunctions` into the same 8 native function
//! pointers the Phase-4 native bridge resolves, written into an
//! [`RcNetFunctions`](super::RcNetFunctions) table; from there it is **byte-for-byte
//! the Phase-4 path** ([`RcNetProvider`](super::RcNetProvider)).
//!
//! ## The exact COM sequence (the C++ `ClrHost::startClr`, reference §8)
//!
//! ```text
//! CLRCreateInstance::<ICLRMetaHost>(&CLSID_CLRMetaHost)         // mscoree.dll
//!   -> ICLRMetaHost::GetRuntime::<ICLRRuntimeInfo>("v4.0.30319")
//!   -> ICLRRuntimeInfo::GetInterface::<ICLRRuntimeHost>(&CLSID_CLR_RUNTIME_HOST)
//!   -> ICLRRuntimeHost::Start()
//! ```
//!
//! then, per managed plugin:
//!
//! ```text
//! ICLRRuntimeHost::ExecuteInDefaultAppDomain(
//!     bridgeDll, "RcNetBridge.Bridge", "Initialize",
//!     "<hexptr-to-RcNetFunctions>|<pluginPath>") -> u32   // the bridge return code
//! ```
//!
//! ## [fix]: the GUID the `windows` crate omits
//!
//! The `windows` crate exports `CLSID_CLRMetaHost` but **not**
//! `CLSID_CLRRuntimeHost`. So — exactly like the C++ `ClrHost.cpp`, which
//! redeclares the same GUID to avoid the SDK `mscoree.h` — we define
//! [`CLSID_CLR_RUNTIME_HOST`] locally (`90F1A06E-7712-4762-86B5-7A5EBA6BDB02`). A
//! `#[cfg(test)]` test pins its bytes against the canonical value.
//!
//! ## Lifetime / leak (the C++ gotcha 9)
//!
//! The C# thunks write into and keep calling through the
//! [`RcNetFunctions`](super::RcNetFunctions) table, and there is **no AppDomain
//! unload** (reference §8 gotcha 9). So the table and the [`ClrHost`] must outlive
//! every provider built from them; [`super::managed`] leaks both for the process
//! lifetime, matching the C++. A future safe-unload is out of scope.
//!
//! Entirely `#[cfg(windows)]` (not compiled on Linux) and gated behind the
//! `plugins` cargo feature (via the parent module). Because it needs a live `.NET
//! FW4` CLR, the COM dance is exercised only on Windows CI / manually; the
//! Linux-buildable logic (the GUID constant, the arg formatting helper) is
//! unit-tested.

use std::path::Path;

use windows::core::{GUID, PCWSTR};
use windows::Win32::System::ClrHosting::{
    CLRCreateInstance, CLSID_CLRMetaHost, ICLRMetaHost, ICLRRuntimeHost, ICLRRuntimeInfo,
};

use super::CLR_VERSION;

/// `CLSID_CLRRuntimeHost` (`90F1A06E-7712-4762-86B5-7A5EBA6BDB02`) — the runtime
/// host class id. **[fix]:** the `windows` crate does **not** export this constant
/// (it exports only [`CLSID_CLRMetaHost`]), so we define it here, byte-for-byte
/// matching the C++ `ClrHost.cpp` which likewise redeclares it (reference §8). Used
/// in [`ICLRRuntimeInfo::GetInterface`] to obtain the [`ICLRRuntimeHost`].
const CLSID_CLR_RUNTIME_HOST: GUID = GUID::from_u128(0x90f1a06e_7712_4762_86b5_7a5eba6bdb02);

/// An in-process **.NET Framework 4.x CLR**, started once and reused for every
/// managed ReClass.NET plugin (the C++ `ClrHost`, reference §8). Holds the started
/// [`ICLRRuntimeHost`]; [`execute_in_default_app_domain`](ClrHost::execute_in_default_app_domain)
/// drives the C# bridge.
pub struct ClrHost {
    /// The started runtime host. The `windows` crate's COM interface wrappers are
    /// ref-counted `IUnknown` smart pointers, so `Drop` releases it; but in
    /// practice we leak the host (see the module doc) because the bridge's thunks
    /// keep calling through it for the process lifetime.
    runtime_host: ICLRRuntimeHost,
}

// The runtime host is a process-global COM object (one CLR per process), shared
// the same way the resolved `RcNetFunctions` table is (`unsafe impl Send + Sync`
// on `RcNetFunctions`). We never hand out `&mut` to it across threads.
unsafe impl Send for ClrHost {}
unsafe impl Sync for ClrHost {}

impl ClrHost {
    /// Start the .NET Framework [`CLR_VERSION`] CLR via the `mscoree` COM surface
    /// (the C++ `ClrHost::startClr`, reference §8 — exact call order). Returns the
    /// started host, or the COM error from the first failing step.
    ///
    /// Live-CLR path: exercised only on Windows (needs `.NET FW4` installed); the
    /// Linux build never compiles this.
    pub fn start() -> windows::core::Result<ClrHost> {
        // SAFETY: `CLRCreateInstance` loads `mscoree.dll` and instantiates the
        // metahost; the generic arg infers the IID from `ICLRMetaHost`. All four
        // COM calls below are FFI into the CLR host — each returns an `HRESULT`
        // surfaced as `Result`, and the smart-pointer wrappers own the ref-counts.
        unsafe {
            let meta_host: ICLRMetaHost = CLRCreateInstance(&CLSID_CLRMetaHost)?;

            // GetRuntime(L"v4.0.30319") — the .NET Framework 4 runtime. We build a
            // NUL-terminated UTF-16 buffer and pass a borrowed PCWSTR over it
            // (kept alive across the call by `version_w`).
            let version_w = to_wide(CLR_VERSION);
            let runtime_info: ICLRRuntimeInfo = meta_host.GetRuntime(PCWSTR(version_w.as_ptr()))?;

            // GetInterface(CLSID_CLRRuntimeHost) — the locally-defined GUID [fix].
            let runtime_host: ICLRRuntimeHost =
                runtime_info.GetInterface(&CLSID_CLR_RUNTIME_HOST)?;

            // Start the CLR (idempotent if already started by another host).
            runtime_host.Start()?;

            Ok(ClrHost { runtime_host })
        }
    }

    /// Call `type_name::method(arg)` in the CLR's default AppDomain (the C++
    /// `ICLRRuntimeHost::ExecuteInDefaultAppDomain`, reference §8) and return the
    /// managed method's `int` return value. For the bridge that is the
    /// [load return code](super::managed::ManagedSkip) (0 ok, 1 bad arg, 2 no
    /// `ICoreProcessFunctions`, 3 load/deps fail, 4 other).
    ///
    /// `assembly` is the bridge DLL path; `arg` is the
    /// `"<hexptr>|<pluginpath>"` string built by
    /// [`format_bridge_arg`](super::managed::format_bridge_arg).
    ///
    /// Live path: needs the started CLR + a real bridge DLL (Windows only).
    pub fn execute_in_default_app_domain(
        &self,
        assembly: &Path,
        type_name: &str,
        method: &str,
        arg: &str,
    ) -> windows::core::Result<u32> {
        // Build NUL-terminated UTF-16 buffers; the PCWSTRs borrow them, so they
        // must outlive the call (they do — locals dropped at end of fn).
        let assembly_w = path_to_wide(assembly);
        let type_w = to_wide(type_name);
        let method_w = to_wide(method);
        let arg_w = to_wide(arg);

        // SAFETY: FFI into the started CLR. Each PCWSTR points at a NUL-terminated
        // buffer kept alive above for the call's duration; the call marshals them
        // into the managed entry point and returns the managed `int` as `u32`.
        unsafe {
            self.runtime_host.ExecuteInDefaultAppDomain(
                PCWSTR(assembly_w.as_ptr()),
                PCWSTR(type_w.as_ptr()),
                PCWSTR(method_w.as_ptr()),
                PCWSTR(arg_w.as_ptr()),
            )
        }
    }
}

/// Encode `s` as a NUL-terminated UTF-16 buffer (a wide C string the CLR COM
/// surface takes as `LPCWSTR`). The trailing NUL is what makes a `PCWSTR` over the
/// returned `Vec` a valid wide string.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Encode a path as a NUL-terminated UTF-16 buffer. On Windows the CLR expects
/// native (`\\`-separated) paths; the caller passes a path already normalized to
/// native separators (the C++ `QDir::toNativeSeparators`, reference §8).
fn path_to_wide(path: &Path) -> Vec<u16> {
    to_wide(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **[fix] guard:** the locally-defined [`CLSID_CLR_RUNTIME_HOST`] byte-equals
    /// the canonical `90F1A06E-7712-4762-86B5-7A5EBA6BDB02` (the C++ `ClrHost.cpp`
    /// `sCLSID_CLRRuntimeHost`, reference §8). A typo here would fail
    /// `GetInterface` at runtime on Windows only, so we pin the bytes at compile
    /// time. The live COM dance itself is Windows-CI / manual.
    #[test]
    fn clsid_clr_runtime_host_matches_canonical_guid() {
        // The C++ GUID literal, field-by-field:
        // {0x90F1A06E, 0x7712, 0x4762, {0x86,0xB5,0x7A,0x5E,0xBA,0x6B,0xDB,0x02}}
        let expected = GUID {
            data1: 0x90f1a06e,
            data2: 0x7712,
            data3: 0x4762,
            data4: [0x86, 0xb5, 0x7a, 0x5e, 0xba, 0x6b, 0xdb, 0x02],
        };
        assert_eq!(CLSID_CLR_RUNTIME_HOST, expected);
        // And it is NOT the metahost CLSID (a copy-paste guard).
        assert_ne!(CLSID_CLR_RUNTIME_HOST, CLSID_CLRMetaHost);
    }

    /// `to_wide` produces a NUL-terminated UTF-16 buffer (the wide C string the CLR
    /// COM surface takes).
    #[test]
    fn to_wide_is_nul_terminated_utf16() {
        let w = to_wide("v4.0.30319");
        assert_eq!(*w.last().unwrap(), 0, "must be NUL-terminated");
        let expected: Vec<u16> = "v4.0.30319".encode_utf16().collect();
        assert_eq!(&w[..w.len() - 1], &expected[..]);

        // Empty string → just the NUL.
        assert_eq!(to_wide(""), vec![0u16]);
    }
}
