//! `provider` — [`RcNetProvider`], the host [`Provider`] backed by a resolved
//! ReClass.NET CoreFunctions table + an open process handle (the C++
//! `RcNetCompatProvider`, reference §8 / `RcNetCompatProvider.cpp`).
//!
//! Construction opens the process with [`ProcessAccess::Full`] (C++ parity) and
//! snapshots its sections + modules once (the C++ `cacheModules`, extended to also
//! keep sections for the [fix] (2) region enumeration). `read`/`write` are direct
//! calls into the plugin's `ReadRemoteMemory`/`WriteRemoteMemory`.
//!
//! ### The `thread_local` collector pattern (reference §8)
//!
//! ReClass.NET's enumeration callbacks take **no user-context pointer** — the only
//! argument is the per-item data struct. So, exactly like the C++
//! (`RcNetCompatPlugin.cpp:294-326`, `RcNetCompatProvider.cpp:88-132`), we smuggle
//! the destination `Vec` through a `thread_local` cell, set it for the synchronous
//! duration of the `Enumerate…` call, and clear it after. The calls are
//! synchronous (the plugin invokes the callback inline, then returns), so a
//! thread-local is sufficient and re-entrancy-safe across threads.
//!
//! Gated behind the `plugins` cargo feature (via the parent module).

use std::cell::RefCell;
use std::os::raw::c_void;

use crate::plugin::reclassnet::bridge::RcNetFunctions;
use crate::plugin::reclassnet::ffi::{
    decode_utf16_fixed, EnumerateRemoteModuleData, EnumerateRemoteSectionData, ProcessAccess,
    RcPointer, SectionProtection, SectionType,
};
use crate::provider::{MemoryRegion, Provider, RegionType};

// ── thread_local collectors for the no-context ReClass.NET callbacks ──────────
//
// `*mut Vec<…>` (a raw pointer, not a reference) so the cell is `'static` and we
// never hold a borrow across the FFI call; the pointer is valid only between the
// set-before / clear-after around each synchronous `Enumerate…` call.

thread_local! {
    static MODULE_SINK: RefCell<*mut Vec<ModuleInfo>> = const { RefCell::new(std::ptr::null_mut()) };
    static SECTION_SINK: RefCell<*mut Vec<MemoryRegion>> = const { RefCell::new(std::ptr::null_mut()) };
}

/// A cached module (the C++ `RcNetCompatProvider::ModuleInfo`) — name + base +
/// size, used for symbol resolution and the base address.
#[derive(Clone, Debug)]
struct ModuleInfo {
    name: String,
    base: u64,
    size: u64,
}

/// The module-enumeration callback (the C++ `moduleCallback`,
/// `RcNetCompatProvider.cpp:97-109`). Reads the packed UTF-16 path, takes the file
/// name as the module name, and appends to the thread-local sink.
extern "C" fn module_callback(data: *mut EnumerateRemoteModuleData) {
    if data.is_null() {
        return;
    }
    // SAFETY: the plugin hands us a valid pointer to a packed struct for the
    // duration of this synchronous call. We copy fields out (the struct is packed,
    // so we read through a `read_unaligned` of the whole value first).
    let data = unsafe { std::ptr::read_unaligned(data) };
    // Copy the packed fields to aligned locals before borrowing (a `&` into a
    // packed struct is UB even on an owned value — `[u16; N]` wants align 2).
    let path_buf = data.path;
    let base = data.base_address as u64;
    let size = data.size;
    let path = decode_utf16_fixed(&path_buf);
    let name = file_name_of(&path);
    let info = ModuleInfo { name, base, size };
    MODULE_SINK.with(|sink| {
        let ptr = *sink.borrow();
        if !ptr.is_null() {
            // SAFETY: set to a live `&mut Vec` for the duration of the enclosing
            // `EnumerateRemoteSectionsAndModules` call (see `snapshot`).
            unsafe { (*ptr).push(info) };
        }
    });
}

/// The section-enumeration callback (the C++ `sectionCallback` was intentionally
/// empty — reference §8; design §7.A [fix] (2) makes it real). Maps the section's
/// [`SectionType`] + [`SectionProtection`] to a host [`MemoryRegion`].
extern "C" fn section_callback(data: *mut EnumerateRemoteSectionData) {
    if data.is_null() {
        return;
    }
    // SAFETY: as `module_callback`.
    let data = unsafe { std::ptr::read_unaligned(data) };
    // Copy the packed fields to aligned locals before borrowing (see above).
    let base = data.base_address as u64;
    let size = data.size;
    let name_buf = data.name;
    let prot = SectionProtection(data.protection);
    let region_type = match SectionType::from_raw(data.ty) {
        SectionType::Image => RegionType::Image,
        SectionType::Mapped => RegionType::Mapped,
        // Unknown + Private → Private (design §7.A [fix] (2)).
        _ => RegionType::Private,
    };
    let region = MemoryRegion {
        base,
        size,
        readable: prot.readable(),
        writable: prot.writable(),
        executable: prot.executable(),
        module_name: decode_utf16_fixed(&name_buf),
        region_type,
    };
    SECTION_SINK.with(|sink| {
        let ptr = *sink.borrow();
        if !ptr.is_null() {
            // SAFETY: as `module_callback`.
            unsafe { (*ptr).push(region) };
        }
    });
}

/// The basename of a path (handling both `/` and `\\` separators, since a
/// ReClass.NET plugin may report Windows-style module paths on any host).
fn file_name_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// A host [`Provider`] backed by a ReClass.NET native plugin's CoreFunctions (the
/// C++ `RcNetCompatProvider`, reference §8). Holds the resolved table by value, an
/// open process handle (opened [`ProcessAccess::Full`] — C++ parity), and the
/// section + module snapshot captured at open.
pub struct RcNetProvider {
    fns: RcNetFunctions,
    handle: RcPointer,
    process_name: String,
    /// Cached modules for symbol resolution + base (the C++ `m_modules`).
    modules: Vec<ModuleInfo>,
    /// Cached regions for [`enumerate_regions`](Provider::enumerate_regions)
    /// (design §7.A [fix] (2); the C++ discarded these).
    regions: Vec<MemoryRegion>,
    /// First module base, or 0 (the C++ `m_base`).
    base: u64,
    /// Detected target pointer size, 4 or 8 (design §7.A [fix] (1); the C++
    /// hardcoded 8). Inferred from the module/region address span at open.
    pointer_size: i32,
}

// The handle is an opaque `*mut c_void` we never deref; the provider is logically
// shareable like the other native-backed providers (`loader::LoadedProvider`).
unsafe impl Send for RcNetProvider {}
unsafe impl Sync for RcNetProvider {}

impl RcNetProvider {
    /// Open `pid` through the table and build the provider (the C++
    /// `RcNetCompatProvider` ctor + `cacheModules`). Returns `None` if the process
    /// can't be opened (so the caller surfaces the C++ "Failed to open process"
    /// error). `is_32bit` is the picker-reported bitness hint (design §7.A [fix]
    /// (1)); the address span can still override it upward (a hint of 32-bit but a
    /// module above 4 GiB means 64-bit).
    pub fn open(
        fns: RcNetFunctions,
        pid: u32,
        process_name: impl Into<String>,
        is_32bit_hint: bool,
    ) -> Option<RcNetProvider> {
        let open = fns.open_remote_process?;
        // Calling the resolved plugin export (an `extern "C" fn`) with the
        // C++-parity args.
        let handle = open(pid as u64, ProcessAccess::Full);
        if handle.is_null() {
            return None;
        }

        let mut p = RcNetProvider {
            fns,
            handle,
            process_name: process_name.into(),
            modules: Vec::new(),
            regions: Vec::new(),
            base: 0,
            pointer_size: if is_32bit_hint { 4 } else { 8 },
        };
        p.snapshot();
        Some(p)
    }

    /// Run the section + module enumeration once, filling `modules`/`regions` and
    /// deriving `base` + `pointer_size` (the C++ `cacheModules`, extended).
    fn snapshot(&mut self) {
        let Some(enumerate) = self.fns.enumerate_sections_and_modules else {
            return;
        };
        if self.handle.is_null() {
            return;
        }

        let mut modules: Vec<ModuleInfo> = Vec::new();
        let mut regions: Vec<MemoryRegion> = Vec::new();

        // Point the thread-locals at our local Vecs for the synchronous call, then
        // clear them (the C++ `g_*Collector.dest = &…; …; = nullptr`).
        MODULE_SINK.with(|s| *s.borrow_mut() = &mut modules as *mut _);
        SECTION_SINK.with(|s| *s.borrow_mut() = &mut regions as *mut _);
        // Calling the resolved plugin export (an `extern "C" fn`); the callbacks
        // only touch the thread-local sinks set above for this call's duration.
        enumerate(self.handle, section_callback, module_callback);
        MODULE_SINK.with(|s| *s.borrow_mut() = std::ptr::null_mut());
        SECTION_SINK.with(|s| *s.borrow_mut() = std::ptr::null_mut());

        // Base = first module base (the C++ rule).
        if let Some(first) = modules.first() {
            self.base = first.base;
        }

        // [fix] (1): infer 64-bit if any module/region sits above the 32-bit
        // address ceiling, regardless of the picker hint. (We only widen, never
        // narrow — a real 64-bit target can have all-low modules early, but a
        // 32-bit target can never have a >4 GiB address.)
        let max_addr = modules
            .iter()
            .map(|m| m.base.saturating_add(m.size))
            .chain(regions.iter().map(|r| r.base.saturating_add(r.size)))
            .max()
            .unwrap_or(0);
        if max_addr > u32::MAX as u64 {
            self.pointer_size = 8;
        }

        self.modules = modules;
        self.regions = regions;
    }
}

impl Drop for RcNetProvider {
    fn drop(&mut self) {
        // The C++ dtor: close the handle if we have a close fn (reference §8).
        if !self.handle.is_null() {
            if let Some(close) = self.fns.close_remote_process {
                // Closing a handle we opened (an `extern "C" fn`); once at drop.
                close(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

impl Provider for RcNetProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        // The C++ `read`: ReadRemoteMemory(handle, addr as ptr, buf, offset=0, len)
        // (reference §8). Guard the same preconditions (handle + fn + len>0).
        if self.handle.is_null() || buf.is_empty() {
            return false;
        }
        let Some(read) = self.fns.read_remote_memory else {
            return false;
        };
        let len = match i32::try_from(buf.len()) {
            Ok(n) => n,
            Err(_) => return false,
        };
        // The plugin reads `len` bytes into `buf` (offset 0). `addr` is a target
        // address reinterpreted as the `RC_Pointer` the ABI expects. Calling the
        // resolved `extern "C" fn` is safe; the buffer it fills is `len` long.
        read(
            self.handle,
            addr as RcPointer,
            buf.as_mut_ptr() as *mut c_void,
            0,
            len,
        )
    }

    fn size(&self) -> i32 {
        // The handle must still be valid (the C++ `IsProcessValid` gate).
        if self.handle.is_null() {
            return 0;
        }
        if let Some(valid) = self.fns.is_process_valid {
            // Querying validity of our handle (an `extern "C" fn`).
            if !valid(self.handle) {
                return 0;
            }
        }
        // [fix] (3): the span of the regions/modules (max end − base) instead of
        // the C++ `0x10000` sentinel. Fall back to the sentinel only if nothing
        // was enumerated (so `is_valid()` still passes for a bare table).
        let max_end = self
            .regions
            .iter()
            .map(|r| r.base.saturating_add(r.size))
            .chain(self.modules.iter().map(|m| m.base.saturating_add(m.size)))
            .max();
        match max_end {
            Some(end) if end > self.base => (end - self.base).min(i32::MAX as u64) as i32,
            _ => 0x10000,
        }
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        if self.handle.is_null() || data.is_empty() {
            return false;
        }
        let Some(write) = self.fns.write_remote_memory else {
            return false;
        };
        let len = match i32::try_from(data.len()) {
            Ok(n) => n,
            Err(_) => return false,
        };
        // The plugin reads `len` bytes from `data` and writes them to the target
        // (offset 0). The buffer is const on our side; the ABI takes a non-const
        // `RC_Pointer`, so we cast away const (the plugin must not write to it —
        // matching the C++ `const_cast`). Calling the `extern "C" fn` is safe.
        write(
            self.handle,
            addr as RcPointer,
            data.as_ptr() as *mut c_void,
            0,
            len,
        )
    }

    fn is_writable(&self) -> bool {
        // The C++ `isWritable` = `WriteRemoteMemory != null` (reference §8).
        self.fns.write_remote_memory.is_some()
    }

    fn name(&self) -> String {
        self.process_name.clone()
    }

    fn kind(&self) -> String {
        // The C++ `kind` = "RcNet" (reference §8).
        "RcNet".to_string()
    }

    fn is_live(&self) -> bool {
        true
    }

    fn pointer_size(&self) -> i32 {
        self.pointer_size
    }

    fn base(&self) -> u64 {
        self.base
    }

    fn get_symbol(&self, addr: u64) -> String {
        // The C++ linear module scan → "mod+0xHEX" (reference §8).
        for m in &self.modules {
            if addr >= m.base && addr < m.base.saturating_add(m.size) {
                return format!("{}+0x{:x}", m.name, addr - m.base);
            }
        }
        String::new()
    }

    fn symbol_to_address(&self, name: &str) -> u64 {
        // The C++ case-insensitive module-name match → base (reference §8).
        for m in &self.modules {
            if m.name.eq_ignore_ascii_case(name) {
                return m.base;
            }
        }
        0
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        // [fix] (2): real regions from the section callback (C++ returned empty).
        self.regions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::reclassnet::bridge::RcNetFunctions;
    use crate::plugin::reclassnet::ffi::{
        EnumerateProcessCallback, EnumerateRemoteModulesCallback, EnumerateRemoteSectionsCallback,
    };

    // ── An in-test fake ReClass.NET plugin: a 64 KiB ramp "process" with one
    // module ("fake.exe" @0x1000, 0x2000 bytes) and two sections (an Image region
    // r-x at the module + a Private rw- region). Drives the real callbacks. ──

    static mut HANDLE_TOKEN: u8 = 0;

    fn fake_handle() -> RcPointer {
        // A stable non-null token address (we never deref it).
        std::ptr::addr_of_mut!(HANDLE_TOKEN) as RcPointer
    }

    extern "C" fn open(_id: u64, _access: ProcessAccess) -> RcPointer {
        fake_handle()
    }
    extern "C" fn close(_h: RcPointer) {}
    extern "C" fn is_valid(h: RcPointer) -> bool {
        !h.is_null()
    }

    extern "C" fn read(
        _h: RcPointer,
        address: RcPointer,
        buffer: RcPointer,
        offset: i32,
        size: i32,
    ) -> bool {
        if size <= 0 || offset != 0 {
            return false;
        }
        let start = address as usize;
        let end = match start.checked_add(size as usize) {
            Some(e) => e,
            None => return false,
        };
        if end > 0x10000 {
            return false;
        }
        // SAFETY (test): the bridge hands us a buffer of `size` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(buffer as *mut u8, size as usize) };
        for (i, b) in out.iter_mut().enumerate() {
            *b = (start + i) as u8;
        }
        true
    }

    extern "C" fn enumerate_sm(
        _h: RcPointer,
        section_cb: EnumerateRemoteSectionsCallback,
        module_cb: EnumerateRemoteModulesCallback,
    ) {
        // One module: "C:\\games\\fake.exe" @0x1000, size 0x2000.
        let mut mod_data = EnumerateRemoteModuleData {
            base_address: 0x1000 as RcPointer,
            size: 0x2000,
            path: [0u16; 260],
        };
        for (i, u) in "C:\\games\\fake.exe".encode_utf16().enumerate() {
            mod_data.path[i] = u;
        }
        module_cb(&mut mod_data);

        // Section 1: Image, r-x, @0x1000 size 0x1000, name ".text".
        let mut sec1 = EnumerateRemoteSectionData {
            base_address: 0x1000 as RcPointer,
            size: 0x1000,
            ty: SectionType::Image as i32,
            category: 0,
            protection: SectionProtection::READ | SectionProtection::EXECUTE,
            name: [0u16; 16],
            module_path: [0u16; 260],
        };
        for (i, u) in ".text".encode_utf16().enumerate() {
            sec1.name[i] = u;
        }
        section_cb(&mut sec1);

        // Section 2: Private, rw-, @0x8000 size 0x1000.
        let mut sec2 = EnumerateRemoteSectionData {
            base_address: 0x8000 as RcPointer,
            size: 0x1000,
            ty: SectionType::Private as i32,
            category: 0,
            protection: SectionProtection::READ | SectionProtection::WRITE,
            name: [0u16; 16],
            module_path: [0u16; 260],
        };
        section_cb(&mut sec2);
    }

    extern "C" fn enumerate_procs(cb: EnumerateProcessCallback) {
        use crate::plugin::reclassnet::ffi::EnumerateProcessData;
        let mut data = EnumerateProcessData {
            id: 4321,
            name: [0u16; 260],
            path: [0u16; 260],
        };
        for (i, u) in "fake.exe".encode_utf16().enumerate() {
            data.name[i] = u;
        }
        cb(&mut data);
    }

    fn fake_table() -> RcNetFunctions {
        RcNetFunctions {
            enumerate_processes: Some(enumerate_procs),
            open_remote_process: Some(open),
            is_process_valid: Some(is_valid),
            close_remote_process: Some(close),
            read_remote_memory: Some(read),
            write_remote_memory: None,
            enumerate_sections_and_modules: Some(enumerate_sm),
            control_remote_process: None,
        }
    }

    #[test]
    fn open_reads_and_snapshots_modules_and_regions() {
        let p = RcNetProvider::open(fake_table(), 4321, "fake.exe", false).expect("open");

        // Name + kind + live (reference §8 mapping).
        assert_eq!(p.name(), "fake.exe");
        assert_eq!(p.kind(), "RcNet");
        assert!(p.is_live());

        // read → ReadRemoteMemory ramp.
        let mut buf = [0u8; 4];
        assert!(p.read(0x10, &mut buf));
        assert_eq!(buf, [0x10, 0x11, 0x12, 0x13]);
        // Out-of-range read fails (no panic).
        assert!(!p.read(0xfffe, &mut [0u8; 8]));

        // base = first module base (the C++ rule).
        assert_eq!(p.base(), 0x1000);

        // [fix] (2): real regions from the section callback.
        let regions = p.enumerate_regions();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].region_type, RegionType::Image);
        assert!(regions[0].readable && regions[0].executable && !regions[0].writable);
        assert_eq!(regions[0].module_name, ".text");
        assert_eq!(regions[1].region_type, RegionType::Private);
        assert!(regions[1].readable && regions[1].writable && !regions[1].executable);

        // [fix] (3): size = span (max end 0x9000 − base 0x1000 = 0x8000), not the
        // 0x10000 sentinel.
        assert_eq!(p.size(), 0x8000);

        // Symbol resolution: an address inside the module → "fake.exe+0xHEX".
        assert_eq!(p.get_symbol(0x1234), "fake.exe+0x234");
        assert_eq!(p.get_symbol(0x9999), "");
        assert_eq!(p.symbol_to_address("FAKE.EXE"), 0x1000);
        assert_eq!(p.symbol_to_address("nope"), 0);
    }

    #[test]
    fn pointer_size_fix_detects_bitness() {
        // A 32-bit hint with only low modules stays 32-bit (the [fix] (1)
        // never narrows but here there's nothing to widen).
        let p = RcNetProvider::open(fake_table(), 1, "p", true).expect("open");
        assert_eq!(p.pointer_size(), 4);

        // A 64-bit hint stays 64-bit.
        let p2 = RcNetProvider::open(fake_table(), 1, "p", false).expect("open");
        assert_eq!(p2.pointer_size(), 8);
    }

    #[test]
    fn pointer_size_widens_on_high_address() {
        // A plugin reporting a module above 4 GiB forces 64-bit even with a
        // 32-bit hint (design §7.A [fix] (1) — we only widen).
        extern "C" fn enumerate_high(
            _h: RcPointer,
            _section_cb: EnumerateRemoteSectionsCallback,
            module_cb: EnumerateRemoteModulesCallback,
        ) {
            let mut m = EnumerateRemoteModuleData {
                base_address: 0x7fff_0000_0000 as RcPointer,
                size: 0x1000,
                path: [0u16; 260],
            };
            for (i, u) in "hi.dll".encode_utf16().enumerate() {
                m.path[i] = u;
            }
            module_cb(&mut m);
        }
        let mut t = fake_table();
        t.enumerate_sections_and_modules = Some(enumerate_high);
        let p = RcNetProvider::open(t, 1, "p", true).expect("open");
        assert_eq!(p.pointer_size(), 8);
        assert_eq!(p.base(), 0x7fff_0000_0000);
    }

    #[test]
    fn open_returns_none_when_handle_null() {
        // A plugin whose OpenRemoteProcess returns null → no provider (the C++
        // "Failed to open process" path).
        extern "C" fn open_null(_id: u64, _access: ProcessAccess) -> RcPointer {
            std::ptr::null_mut()
        }
        let mut t = fake_table();
        t.open_remote_process = Some(open_null);
        assert!(RcNetProvider::open(t, 1, "p", false).is_none());
    }

    #[test]
    fn write_unsupported_when_fn_absent() {
        // The fake table has no WriteRemoteMemory → not writable, write fails.
        let p = RcNetProvider::open(fake_table(), 1, "p", false).expect("open");
        assert!(!p.is_writable());
        assert!(!p.write(0x10, &[1, 2, 3]));
    }

    #[test]
    fn write_supported_round_trips_when_fn_present() {
        // A table WITH a write fn into a static scratch buffer.
        static mut SCRATCH: [u8; 16] = [0u8; 16];
        extern "C" fn open2(_id: u64, _a: ProcessAccess) -> RcPointer {
            std::ptr::addr_of_mut!(SCRATCH) as RcPointer
        }
        extern "C" fn write2(
            _h: RcPointer,
            address: RcPointer,
            buffer: RcPointer,
            offset: i32,
            size: i32,
        ) -> bool {
            if offset != 0 || size <= 0 {
                return false;
            }
            let at = address as usize;
            if at + size as usize > 16 {
                return false;
            }
            // SAFETY (test): copy `size` bytes from the bridge buffer into SCRATCH.
            unsafe {
                let src = std::slice::from_raw_parts(buffer as *const u8, size as usize);
                let dst = &mut *std::ptr::addr_of_mut!(SCRATCH);
                dst[at..at + size as usize].copy_from_slice(src);
            }
            true
        }
        extern "C" fn read2(
            _h: RcPointer,
            address: RcPointer,
            buffer: RcPointer,
            offset: i32,
            size: i32,
        ) -> bool {
            if offset != 0 || size <= 0 {
                return false;
            }
            let at = address as usize;
            if at + size as usize > 16 {
                return false;
            }
            // SAFETY (test): copy `size` bytes out of SCRATCH.
            unsafe {
                let src = &*std::ptr::addr_of!(SCRATCH);
                let dst = std::slice::from_raw_parts_mut(buffer as *mut u8, size as usize);
                dst.copy_from_slice(&src[at..at + size as usize]);
            }
            true
        }
        let mut t = fake_table();
        t.open_remote_process = Some(open2);
        t.write_remote_memory = Some(write2);
        t.read_remote_memory = Some(read2);
        // No section/module enumeration for this one.
        t.enumerate_sections_and_modules = None;

        let p = RcNetProvider::open(t, 1, "p", false).expect("open");
        assert!(p.is_writable());
        assert!(p.write(2, &[0xaa, 0xbb, 0xcc]));
        let mut buf = [0u8; 3];
        assert!(p.read(2, &mut buf));
        assert_eq!(buf, [0xaa, 0xbb, 0xcc]);

        // No enumeration → size falls back to the 0x10000 sentinel (the C++
        // fallback, [fix] (3) "fall back only if no regions").
        assert_eq!(p.size(), 0x10000);
    }
}
