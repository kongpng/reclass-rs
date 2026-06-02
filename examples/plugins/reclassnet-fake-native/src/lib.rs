//! `reclassnet-fake-native` — a **test ReClass.NET native plugin** (design §6
//! Phase 4 deliverable). It exports the 8 ReClass.NET CoreFunctions as raw
//! `extern "C"` symbols over an in-memory buffer "process", so the host's folder
//! scan can sniff + bridge it end-to-end with no real connector (memflow needs
//! KVM/QEMU/PCILeech; this stands in for the `memflow-reclass-plugin` shape).
//!
//! This crate intentionally **does not** depend on the host or the abi_stable SDK:
//! it is a plain cdylib exporting ReClass.NET's ABI, exactly like a third-party
//! native ReClass.NET plugin (e.g. a GDB / DMA reader). The struct + enum
//! definitions below are transcribed from `ReClassNET_Plugin.hpp` (the same header
//! the host's `reclassnet::ffi` mirrors); the two must agree byte-for-byte.
//!
//! ### Calling convention
//!
//! ReClass.NET's `RC_CALLCONV` is `__stdcall` on **32-bit Windows** only, and
//! plain on every other target. This phase is cross-platform with no Windows-only
//! code, so the exports use `extern "C"` — correct on the Linux/macOS/x64-Windows
//! targets the test runs on (a 32-bit-Windows build of a real plugin would use
//! `extern "stdcall"`; the host's [`rc_extern!`] selects the matching convention).
//!
//! ### The simulated process
//!
//! A 64 KiB (`0x10000`) buffer whose byte `i` ramps as `i as u8` (so a read at
//! `addr` returns `addr, addr+1, …`). One module `fake.exe` @`0x1000` size
//! `0x2000`; two sections — an Image r-x region at the module and a Private rw-
//! region at `0x8000`. One process `fake-target.exe` (pid 4321).

#![allow(clippy::missing_safety_doc)]

use std::os::raw::c_void;

// ── The ReClass.NET ABI subset (verbatim from ReClassNET_Plugin.hpp) ──────────

type RcPointer = *mut c_void;

#[repr(i32)]
#[derive(Clone, Copy)]
pub enum ProcessAccess {
    Read = 0,
    Write = 1,
    Full = 2,
}

#[repr(C, packed)]
pub struct EnumerateProcessData {
    id: u64,
    name: [u16; 260],
    path: [u16; 260],
}

#[repr(C, packed)]
pub struct EnumerateRemoteSectionData {
    base_address: RcPointer,
    size: u64,
    ty: i32,
    category: i32,
    protection: i32,
    name: [u16; 16],
    module_path: [u16; 260],
}

#[repr(C, packed)]
pub struct EnumerateRemoteModuleData {
    base_address: RcPointer,
    size: u64,
    path: [u16; 260],
}

type EnumerateProcessCallback = extern "C" fn(*mut EnumerateProcessData);
type EnumerateRemoteSectionsCallback = extern "C" fn(*mut EnumerateRemoteSectionData);
type EnumerateRemoteModulesCallback = extern "C" fn(*mut EnumerateRemoteModuleData);

// ── The simulated process ─────────────────────────────────────────────────────

const MEM_SIZE: usize = 0x10000;
const MODULE_BASE: u64 = 0x1000;
const MODULE_SIZE: u64 = 0x2000;
const PRIVATE_BASE: u64 = 0x8000;
const PRIVATE_SIZE: u64 = 0x1000;
const PID: u64 = 4321;

// A stable non-null handle token (its address; never dereferenced by the host).
static mut HANDLE_TOKEN: u8 = 0;

fn handle() -> RcPointer {
    std::ptr::addr_of_mut!(HANDLE_TOKEN) as RcPointer
}

/// Build a fixed `[u16; N]` UTF-16 buffer from a `&str`, NUL-terminated (the
/// C-string form the ReClass.NET callback structs use). Returned by value so the
/// whole array can be stored into a `#[repr(C, packed)]` field (a `&mut` into a
/// packed field would be an unaligned reference — UB).
fn utf16_buf<const N: usize>(s: &str) -> [u16; N] {
    let mut dst = [0u16; N];
    // Copy at most N-1 code units, leaving the last slot as the trailing NUL.
    for (i, u) in s.encode_utf16().take(N.saturating_sub(1)).enumerate() {
        dst[i] = u;
    }
    dst
}

// ── The 8 exported CoreFunctions (the RcNetFunctions table order) ─────────────

/// `EnumerateProcesses(cb)` — one fake process.
#[no_mangle]
pub extern "C" fn EnumerateProcesses(callback: EnumerateProcessCallback) {
    let mut data = EnumerateProcessData {
        id: PID,
        name: utf16_buf("fake-target.exe"),
        path: utf16_buf("/proc/fake/fake-target.exe"),
    };
    callback(&mut data);
}

/// `OpenRemoteProcess(id, access) -> handle` — succeeds for our PID, else null.
#[no_mangle]
pub extern "C" fn OpenRemoteProcess(id: u64, _desired_access: ProcessAccess) -> RcPointer {
    if id == PID {
        handle()
    } else {
        std::ptr::null_mut()
    }
}

/// `IsProcessValid(handle) -> bool`.
#[no_mangle]
pub extern "C" fn IsProcessValid(h: RcPointer) -> bool {
    h == handle()
}

/// `CloseRemoteProcess(handle)`.
#[no_mangle]
pub extern "C" fn CloseRemoteProcess(_h: RcPointer) {}

/// `ReadRemoteMemory(handle, address, buffer, offset, size) -> bool` — serves the
/// ramp buffer (byte `i` == `i as u8`).
#[no_mangle]
pub unsafe extern "C" fn ReadRemoteMemory(
    h: RcPointer,
    address: RcPointer,
    buffer: RcPointer,
    offset: i32,
    size: i32,
) -> bool {
    if h != handle() || size <= 0 || offset < 0 {
        return false;
    }
    let start = address as usize + offset as usize;
    let end = match start.checked_add(size as usize) {
        Some(e) => e,
        None => return false,
    };
    if end > MEM_SIZE {
        return false;
    }
    let out = std::slice::from_raw_parts_mut(buffer as *mut u8, size as usize);
    for (i, b) in out.iter_mut().enumerate() {
        *b = (start + i) as u8;
    }
    true
}

/// `WriteRemoteMemory(...)` — accepts writes into range (no backing store kept;
/// the test only needs it to report writability + succeed in-range).
#[no_mangle]
pub unsafe extern "C" fn WriteRemoteMemory(
    h: RcPointer,
    address: RcPointer,
    _buffer: RcPointer,
    offset: i32,
    size: i32,
) -> bool {
    if h != handle() || size <= 0 || offset < 0 {
        return false;
    }
    let start = address as usize + offset as usize;
    matches!(start.checked_add(size as usize), Some(e) if e <= MEM_SIZE)
}

/// `EnumerateRemoteSectionsAndModules(handle, secCb, modCb)` — one module + two
/// sections.
#[no_mangle]
pub extern "C" fn EnumerateRemoteSectionsAndModules(
    h: RcPointer,
    section_callback: EnumerateRemoteSectionsCallback,
    module_callback: EnumerateRemoteModulesCallback,
) {
    if h != handle() {
        return;
    }

    // Module: fake.exe @0x1000, size 0x2000.
    let mut module = EnumerateRemoteModuleData {
        base_address: MODULE_BASE as RcPointer,
        size: MODULE_SIZE,
        path: utf16_buf("/proc/fake/fake.exe"),
    };
    module_callback(&mut module);

    // Section 1: Image (3), r-x (R|X = 1|4 = 5), at the module, name ".text".
    let mut text = EnumerateRemoteSectionData {
        base_address: MODULE_BASE as RcPointer,
        size: MODULE_SIZE,
        ty: 3,
        category: 1,
        protection: 1 | 4,
        name: utf16_buf(".text"),
        module_path: utf16_buf("/proc/fake/fake.exe"),
    };
    section_callback(&mut text);

    // Section 2: Private (1), rw- (R|W = 1|2 = 3), at 0x8000.
    let mut heap = EnumerateRemoteSectionData {
        base_address: PRIVATE_BASE as RcPointer,
        size: PRIVATE_SIZE,
        ty: 1,
        category: 3,
        protection: 1 | 2,
        name: utf16_buf("heap"),
        module_path: [0u16; 260],
    };
    section_callback(&mut heap);
}

/// `ControlRemoteProcess(handle, action)` — a no-op (the host resolves but never
/// calls it in this phase).
#[no_mangle]
pub extern "C" fn ControlRemoteProcess(_h: RcPointer, _action: i32) {}
