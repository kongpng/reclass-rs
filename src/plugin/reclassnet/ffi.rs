//! `ffi` — the exact ReClass.NET native **CoreFunctions** ABI (design §4, the
//! reference §8), transcribed verbatim from the C++ header
//! `/home/loke/Documents/Reclass/plugins/RcNetPluginCompatLayer/ReClassNET_Plugin.hpp`.
//!
//! This is the **only** place the ReClass.NET wire layout lives. Everything here
//! is `#[repr(C)]` / `#[repr(C, packed)]` to match the C++ `#pragma pack(1)`
//! structs byte-for-byte, and the calling convention is selected per platform by
//! [`rc_extern!`]: ReClass.NET's `RC_CALLCONV` is `__stdcall` on Win32 and empty
//! elsewhere (header lines 8-12). `__stdcall` name-decoration only matters on
//! **32-bit Windows**; everywhere else (incl. x64 Windows) the two are identical,
//! so we select `extern "stdcall"` solely on `all(windows, target_arch = "x86")`
//! and plain `extern "C"` for every other target.
//!
//! | C++ (`ReClassNET_Plugin.hpp`) | here |
//! |---|---|
//! | `RC_Pointer = void*` | [`RcPointer`] (`*mut c_void`) |
//! | `RC_Size = uint64_t` | `u64` |
//! | `RC_UnicodeChar = char16_t` | `u16` |
//! | `enum class ProcessAccess` (:22) | [`ProcessAccess`] |
//! | `enum class SectionProtection` (:29) | [`SectionProtection`] (bitflags) |
//! | `enum class SectionType` (:38) | [`SectionType`] |
//! | `enum class SectionCategory` (:46) | [`SectionCategory`] |
//! | `enum class ControlRemoteProcessAction` (:54) | [`ControlRemoteProcessAction`] |
//! | `struct EnumerateProcessData` (:65) | [`EnumerateProcessData`] |
//! | `struct EnumerateRemoteSectionData` (:72) | [`EnumerateRemoteSectionData`] |
//! | `struct EnumerateRemoteModuleData` (:83) | [`EnumerateRemoteModuleData`] |
//! | the 8 `Fn*` typedefs (:100-126) | the `Fn*` aliases below |
//! | `RcNetFunctions` field order (:130) | [`CORE_FUNCTION_NAMES`] |
//!
//! Gated behind the `plugins` cargo feature (via the parent module).

use std::os::raw::c_void;

/// `RC_Pointer = void*` (header :16). An opaque process handle or address. Carried
/// as a raw pointer across the ABI; never dereferenced on our side.
pub type RcPointer = *mut c_void;

/// The ReClass.NET calling-convention selector (the header `RC_CALLCONV`, :8-12):
/// `extern "stdcall"` on **32-bit Windows** only, plain `extern "C"` everywhere
/// else. Used for every CoreFunction pointer type + the callback pointer types so
/// the ABI matches a real ReClass.NET native plugin on each platform.
///
/// `__stdcall` differs from `cdecl` only on 32-bit x86; on x64 / ARM / non-Windows
/// the platform has a single C convention, so selecting it only for
/// `all(windows, target_arch = "x86")` is exactly correct (and lets the rest of
/// the crate — incl. the cross-platform tests + the example plugin — use the plain
/// C convention).
#[macro_export]
macro_rules! rc_extern {
    (fn($($arg:ty),* $(,)?) $(-> $ret:ty)?) => {
        $crate::__rc_extern_inner!(fn($($arg),*) $(-> $ret)?)
    };
}

#[doc(hidden)]
#[cfg(all(windows, target_arch = "x86"))]
#[macro_export]
macro_rules! __rc_extern_inner {
    (fn($($arg:ty),* $(,)?) $(-> $ret:ty)?) => {
        extern "stdcall" fn($($arg),*) $(-> $ret)?
    };
}

#[doc(hidden)]
#[cfg(not(all(windows, target_arch = "x86")))]
#[macro_export]
macro_rules! __rc_extern_inner {
    (fn($($arg:ty),* $(,)?) $(-> $ret:ty)?) => {
        extern "C" fn($($arg),*) $(-> $ret)?
    };
}

// -- Enums (header :20-59) ----------------------------------------------------

/// `enum class ProcessAccess` (header :22-27). The compat provider always opens
/// with [`ProcessAccess::Full`] (the C++ `RcNetCompatProvider` ctor — reference
/// §8 "always opened `ProcessAccess::Full`").
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessAccess {
    Read = 0,
    Write = 1,
    Full = 2,
}

/// `enum class SectionType` (header :38-44). Maps to the host
/// [`RegionType`](crate::provider::RegionType): `Image → Image`, `Mapped →
/// Mapped`, else `Private` (design §7.A [fix] (2) — real region enumeration).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionType {
    Unknown = 0,
    Private = 1,
    Mapped = 2,
    Image = 3,
}

impl SectionType {
    /// Decode a raw `i32` (as it arrives in the packed callback struct) to the
    /// enum, defaulting unknown discriminants to [`SectionType::Unknown`].
    pub fn from_raw(v: i32) -> SectionType {
        match v {
            1 => SectionType::Private,
            2 => SectionType::Mapped,
            3 => SectionType::Image,
            _ => SectionType::Unknown,
        }
    }
}

/// `enum class SectionCategory` (header :46-52). Carried for completeness; the
/// compat provider does not currently surface the category.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionCategory {
    Unknown = 0,
    Code = 1,
    Data = 2,
    Heap = 3,
}

/// `enum class ControlRemoteProcessAction` (header :54-59). Resolved by the bridge
/// but, like C++, never invoked in this phase (reference §8 "resolved but never
/// called").
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlRemoteProcessAction {
    Suspend = 0,
    Resume = 1,
    Terminate = 2,
}

/// `enum class SectionProtection` (header :29-36) — a **bitflags** value
/// (`No=0, R=1, W=2, X=4, Guard=8`), carried as an `i32` in the packed section
/// struct. We expose the bit constants + accessors rather than a Rust enum
/// (combinations like `R|W|X` are valid), mapping the bits to the host region's
/// `readable`/`writable`/`executable` (design §7.A [fix] (2)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionProtection(pub i32);

impl SectionProtection {
    pub const NO_ACCESS: i32 = 0;
    pub const READ: i32 = 1;
    pub const WRITE: i32 = 2;
    pub const EXECUTE: i32 = 4;
    pub const GUARD: i32 = 8;

    pub fn readable(self) -> bool {
        self.0 & Self::READ != 0
    }
    pub fn writable(self) -> bool {
        self.0 & Self::WRITE != 0
    }
    pub fn executable(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }
    pub fn guarded(self) -> bool {
        self.0 & Self::GUARD != 0
    }
}

// -- Callback data structures (header :63-90, `#pragma pack(push, 1)`) ---------

/// `struct EnumerateProcessData` (header :65-70), `#pragma pack(1)`.
///
/// Exactly **1048 bytes**: `Id` (u64) @0, `Name[260]` (u16) @8, `Path[260]` (u16)
/// @528 (reference §8). `#[repr(C, packed)]` reproduces the C++ packing (no
/// padding after `Id`, since `u16` arrays need no extra alignment under pack(1)).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EnumerateProcessData {
    /// The process id (`RC_Size Id`).
    pub id: u64,
    /// UTF-16, NUL-terminated process name (`RC_UnicodeChar Name[260]`).
    pub name: [u16; 260],
    /// UTF-16, NUL-terminated full path (`RC_UnicodeChar Path[260]`).
    pub path: [u16; 260],
}

/// `struct EnumerateRemoteSectionData` (header :72-81), `#pragma pack(1)`.
///
/// Layout under pack(1): `BaseAddress` (ptr) @0, `Size` (u64) @8, `Type` (i32)
/// @16, `Category` (i32) @20, `Protection` (i32) @24, `Name[16]` (u16) @28,
/// `ModulePath[260]` (u16) @60. The three `enum class` fields are 32-bit (the C++
/// default underlying type), carried as `i32` so we can decode unknown values.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EnumerateRemoteSectionData {
    pub base_address: RcPointer,
    pub size: u64,
    /// `SectionType` discriminant (decode via [`SectionType::from_raw`]).
    pub ty: i32,
    /// `SectionCategory` discriminant.
    pub category: i32,
    /// `SectionProtection` bitflags (decode via [`SectionProtection`]).
    pub protection: i32,
    pub name: [u16; 16],
    pub module_path: [u16; 260],
}

/// `struct EnumerateRemoteModuleData` (header :83-88), `#pragma pack(1)`.
///
/// Layout: `BaseAddress` (ptr) @0, `Size` (u64) @8, `Path[260]` (u16) @16.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EnumerateRemoteModuleData {
    pub base_address: RcPointer,
    pub size: u64,
    pub path: [u16; 260],
}

// -- Callback typedefs (header :94-96) ----------------------------------------

/// `EnumerateProcessCallback` (header :94) — called once per process.
pub type EnumerateProcessCallback = rc_extern!(fn(*mut EnumerateProcessData));
/// `EnumerateRemoteSectionsCallback` (header :95) — called once per section.
pub type EnumerateRemoteSectionsCallback = rc_extern!(fn(*mut EnumerateRemoteSectionData));
/// `EnumerateRemoteModulesCallback` (header :96) — called once per module.
pub type EnumerateRemoteModulesCallback = rc_extern!(fn(*mut EnumerateRemoteModuleData));

// -- Function-pointer typedefs for the 8 resolved exports (header :100-126) ---

/// `FnEnumerateProcesses` (header :100).
pub type FnEnumerateProcesses = rc_extern!(fn(EnumerateProcessCallback));
/// `FnOpenRemoteProcess` (header :102): `(id, access) -> handle`.
pub type FnOpenRemoteProcess = rc_extern!(fn(u64, ProcessAccess) -> RcPointer);
/// `FnIsProcessValid` (header :104).
pub type FnIsProcessValid = rc_extern!(fn(RcPointer) -> bool);
/// `FnCloseRemoteProcess` (header :106).
pub type FnCloseRemoteProcess = rc_extern!(fn(RcPointer));
/// `FnReadRemoteMemory` (header :108): `(handle, address, buffer, offset, size) -> bool`.
pub type FnReadRemoteMemory = rc_extern!(fn(RcPointer, RcPointer, RcPointer, i32, i32) -> bool);
/// `FnWriteRemoteMemory` (header :114): same shape as read.
pub type FnWriteRemoteMemory = rc_extern!(fn(RcPointer, RcPointer, RcPointer, i32, i32) -> bool);
/// `FnEnumerateRemoteSectionsAndModules` (header :120): `(handle, secCb, modCb)`.
pub type FnEnumerateRemoteSectionsAndModules =
    rc_extern!(fn(RcPointer, EnumerateRemoteSectionsCallback, EnumerateRemoteModulesCallback));
/// `FnControlRemoteProcess` (header :125): `(handle, action)`.
pub type FnControlRemoteProcess = rc_extern!(fn(RcPointer, ControlRemoteProcessAction));

/// The 8 export symbol names the sniffer probes / the bridge resolves, in the
/// **`RcNetFunctions` field order** (header :130-140, reference §8). The first
/// four (read-only of this slice) plus open/close are the **required** subset; see
/// [`RcNetFunctions::resolve`](crate::plugin::reclassnet::bridge::RcNetFunctions::resolve).
///
/// As `&[u8]` (the form `libloading::Library::get` takes — a NUL-terminated symbol
/// name) with the trailing NUL included.
pub const CORE_FUNCTION_NAMES: [&[u8]; 8] = [
    b"EnumerateProcesses\0",
    b"OpenRemoteProcess\0",
    b"IsProcessValid\0",
    b"CloseRemoteProcess\0",
    b"ReadRemoteMemory\0",
    b"WriteRemoteMemory\0",
    b"EnumerateRemoteSectionsAndModules\0",
    b"ControlRemoteProcess\0",
];

/// Decode a fixed-size UTF-16 buffer (a `RC_UnicodeChar Name[N]` / `Path[N]`
/// field) to a `String`, stopping at the first NUL (the C-string convention these
/// fields use) and lossily replacing any unpaired surrogate (the C++
/// `QString::fromUtf16`, reference §8 — but lossy so a malformed plugin buffer
/// can't error us out).
///
/// If there is no NUL the whole buffer is decoded (a plugin that fills all `N`
/// code units).
pub fn decode_utf16_fixed(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    /// `EnumerateProcessData` is exactly 1048 bytes with `Name` @8 and `Path` @528
    /// (reference §8 — the verified C++ `#pragma pack(1)` layout). Packing to 1
    /// means no alignment padding, so the struct's align is 1.
    #[test]
    fn enumerate_process_data_layout_matches_cpp() {
        assert_eq!(size_of::<EnumerateProcessData>(), 1048);
        assert_eq!(offset_of!(EnumerateProcessData, id), 0);
        assert_eq!(offset_of!(EnumerateProcessData, name), 8);
        assert_eq!(offset_of!(EnumerateProcessData, path), 528);
        assert_eq!(align_of::<EnumerateProcessData>(), 1);
    }

    /// `EnumerateRemoteModuleData`: ptr @0, Size @ size_of::<ptr>, Path right after
    /// (pack(1), so Path @ ptr+8). 260 u16 = 520 bytes of path.
    #[test]
    fn enumerate_remote_module_data_layout() {
        let ptr = size_of::<RcPointer>();
        assert_eq!(offset_of!(EnumerateRemoteModuleData, base_address), 0);
        assert_eq!(offset_of!(EnumerateRemoteModuleData, size), ptr);
        assert_eq!(offset_of!(EnumerateRemoteModuleData, path), ptr + 8);
        // pack(1): no trailing padding → size is exactly the field span.
        assert_eq!(size_of::<EnumerateRemoteModuleData>(), ptr + 8 + 520);
        assert_eq!(align_of::<EnumerateRemoteModuleData>(), 1);
    }

    /// `EnumerateRemoteSectionData`: the three i32 enum fields sit packed between
    /// `Size` and the two UTF-16 arrays (no padding under pack(1)).
    #[test]
    fn enumerate_remote_section_data_layout() {
        let ptr = size_of::<RcPointer>();
        assert_eq!(offset_of!(EnumerateRemoteSectionData, base_address), 0);
        assert_eq!(offset_of!(EnumerateRemoteSectionData, size), ptr);
        assert_eq!(offset_of!(EnumerateRemoteSectionData, ty), ptr + 8);
        assert_eq!(offset_of!(EnumerateRemoteSectionData, category), ptr + 12);
        assert_eq!(offset_of!(EnumerateRemoteSectionData, protection), ptr + 16);
        assert_eq!(offset_of!(EnumerateRemoteSectionData, name), ptr + 20);
        assert_eq!(
            offset_of!(EnumerateRemoteSectionData, module_path),
            ptr + 20 + 32
        );
        assert_eq!(align_of::<EnumerateRemoteSectionData>(), 1);
    }

    /// The `SectionType` discriminants match the C++ header values exactly.
    #[test]
    fn section_type_discriminants_match_cpp() {
        assert_eq!(SectionType::Unknown as i32, 0);
        assert_eq!(SectionType::Private as i32, 1);
        assert_eq!(SectionType::Mapped as i32, 2);
        assert_eq!(SectionType::Image as i32, 3);
        // from_raw round-trips the known values and clamps the rest.
        assert_eq!(SectionType::from_raw(3), SectionType::Image);
        assert_eq!(SectionType::from_raw(99), SectionType::Unknown);
    }

    /// `ProcessAccess::Full == 2` (the value the provider opens with) and the
    /// other enums match the header.
    #[test]
    fn other_enum_discriminants_match_cpp() {
        assert_eq!(ProcessAccess::Read as i32, 0);
        assert_eq!(ProcessAccess::Write as i32, 1);
        assert_eq!(ProcessAccess::Full as i32, 2);
        assert_eq!(ControlRemoteProcessAction::Suspend as i32, 0);
        assert_eq!(ControlRemoteProcessAction::Terminate as i32, 2);
        assert_eq!(SectionCategory::Heap as i32, 3);
    }

    /// `SectionProtection` bit decoding maps to r/w/x (design §7.A [fix] (2)).
    #[test]
    fn section_protection_bits() {
        let rwx = SectionProtection(
            SectionProtection::READ | SectionProtection::WRITE | SectionProtection::EXECUTE,
        );
        assert!(rwx.readable() && rwx.writable() && rwx.executable());
        assert!(!rwx.guarded());

        let ro = SectionProtection(SectionProtection::READ);
        assert!(ro.readable() && !ro.writable() && !ro.executable());

        let guard = SectionProtection(SectionProtection::GUARD);
        assert!(guard.guarded() && !guard.readable());
    }

    /// `decode_utf16_fixed` stops at the first NUL and round-trips an ASCII /
    /// non-ASCII name written into a fixed 260-u16 buffer (the C++
    /// `QString::fromUtf16` behavior).
    #[test]
    fn decode_utf16_fixed_round_trips_nul_terminated() {
        let mut buf = [0u16; 260];
        let s = "notepad.exe";
        for (i, u) in s.encode_utf16().enumerate() {
            buf[i] = u;
        }
        assert_eq!(decode_utf16_fixed(&buf), "notepad.exe");

        // Non-ASCII round-trips too (a path with a BMP character).
        let mut buf2 = [0u16; 16];
        let s2 = "café";
        for (i, u) in s2.encode_utf16().enumerate() {
            buf2[i] = u;
        }
        assert_eq!(decode_utf16_fixed(&buf2), "café");

        // A fully-filled buffer (no NUL) decodes the whole thing.
        let full = [b'A' as u16; 4];
        assert_eq!(decode_utf16_fixed(&full), "AAAA");

        // Empty (leading NUL) → empty string.
        assert_eq!(decode_utf16_fixed(&[0u16; 8]), "");
    }

    /// The 8 export names are in `RcNetFunctions` field order and NUL-terminated
    /// (the `libloading` symbol form).
    #[test]
    fn core_function_names_order_and_nul() {
        assert_eq!(CORE_FUNCTION_NAMES.len(), 8);
        assert_eq!(&CORE_FUNCTION_NAMES[0], b"EnumerateProcesses\0");
        assert_eq!(&CORE_FUNCTION_NAMES[4], b"ReadRemoteMemory\0");
        assert_eq!(&CORE_FUNCTION_NAMES[7], b"ControlRemoteProcess\0");
        for name in CORE_FUNCTION_NAMES {
            assert_eq!(*name.last().unwrap(), 0, "names must be NUL-terminated");
        }
    }
}
