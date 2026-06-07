//! Data-source abstraction — the `Provider` trait every byte-reading module
//! talks to, the built-in benign sources, and the provider registry.
//!
//! Faithful port of `src/providers/provider.h`, `buffer_provider.h`,
//! `null_provider.h`, `snapshot_provider.h`, and `providerregistry.h`.
//!
//! **In scope (implemented):** [`BufferProvider`] (in-memory + file via
//! `from_file`), [`FileProvider`] (mmap'd binary file), [`NullProvider`],
//! [`SnapshotProvider`], and [`memflow`] for live process sources through
//! memflow's dynamic connector/OS plugins. The legacy process / kernel / remote
//! / WinDbg native-plugin seam remains documented in [`native`] as stubs.

mod buffer;
mod file;
pub mod memflow;
pub mod native;
mod null;
mod registry;
mod snapshot;

pub use buffer::BufferProvider;
pub use file::FileProvider;
pub use memflow::{MemflowAttachConfig, MemflowProvider};
pub use null::NullProvider;
pub use registry::{ProviderInfo, ProviderRegistry, SavedSourceDisplay};
pub use snapshot::{PageMap, SnapshotProvider, K_PAGE_SIZE};

/// `enum class RegionType : uint8_t` (`provider.h:13-17`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum RegionType {
    /// Loaded module (PE/ELF/Mach-O): code + .rdata + .data.
    Image = 0,
    /// Memory-mapped file or shared section.
    Mapped = 1,
    /// Heap, stack, VirtualAlloc — where mutable user data lives.
    #[default]
    Private = 2,
}

/// `struct MemoryRegion` (`provider.h:19-29`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,
    pub size: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub module_name: String,
    pub region_type: RegionType,
}

impl Default for MemoryRegion {
    fn default() -> Self {
        MemoryRegion {
            base: 0,
            size: 0,
            readable: true,
            writable: false,
            executable: false,
            module_name: String::new(),
            region_type: RegionType::Private,
        }
    }
}

/// `struct VtopResult` (`provider.h:31-37`) — virtual→physical translation
/// (kernel providers only; out of scope but the shape is part of the trait).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct VtopResult {
    pub physical: u64,
    pub pml4e: u64,
    pub pdpte: u64,
    pub pde: u64,
    pub pte: u64,
    /// 0=4KB, 1=2MB, 2=1GB.
    pub page_size: u8,
    pub valid: bool,
}

/// `Provider::ThreadInfo` (`provider.h:99`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadInfo {
    pub teb_address: u64,
    pub thread_id: u32,
}

/// `Provider::ModuleEntry` (`provider.h:102`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleEntry {
    pub name: String,
    pub full_path: String,
    pub base: u64,
    pub size: u64,
}

/// `class Provider` (`provider.h:38-153`) — the abstract data source.
///
/// Implementors MUST provide [`read`](Provider::read) and
/// [`size`](Provider::size); everything else has a default mirroring the C++
/// virtual defaults. The derived convenience readers (`read_u8` … `read_f64`,
/// `read_bytes`) are provided as default methods (the C++ non-virtual helpers).
pub trait Provider {
    // --- Must implement ---
    /// `read(addr, buf, len)` — read into `buf`; returns success.
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool;
    /// `size()` — logical size; 0 means "no data" / invalid.
    fn size(&self) -> i32;

    // --- Optional overrides (defaults mirror the C++ virtual defaults) ---
    /// `write(addr, buf, len)` (`provider.h:47`). C++ is a non-const virtual; in
    /// Rust it takes `&self` and the concrete writable sources use interior
    /// mutability (see [`BufferProvider`](super::BufferProvider)), so the write
    /// goes through a shared `Arc<dyn Provider>` even when a snapshot/worker
    /// holds a clone (PORTING_providers §2.1 / §5).
    fn write(&self, _addr: u64, _data: &[u8]) -> bool {
        false
    }
    fn is_writable(&self) -> bool {
        false
    }
    /// Human-readable label (e.g. "notepad.exe", "dump.bin").
    fn name(&self) -> String {
        String::new()
    }
    /// Whether data can change externally (live process / socket). Auto-refresh
    /// only ticks for live providers.
    fn is_live(&self) -> bool {
        false
    }
    /// Category tag for the command-row Source span ("File" / "Process" / …).
    fn kind(&self) -> String {
        "File".to_string()
    }
    /// Native pointer size of the target (4 or 8).
    fn pointer_size(&self) -> i32 {
        8
    }
    /// Initial base address discovered by the provider (main module base). 0 for file/buffer.
    fn base(&self) -> u64 {
        0
    }
    /// Resolve an absolute address to a symbol name, or empty.
    fn get_symbol(&self, _addr: u64) -> String {
        String::new()
    }
    /// Resolve a module/symbol name to its address (reverse of `get_symbol`). 0 if not found.
    fn symbol_to_address(&self, _name: &str) -> u64 {
        0
    }
    /// Enumerate committed/readable memory regions.
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        Vec::new()
    }
    /// Process Environment Block address (live process only). 0 if unavailable.
    fn peb(&self) -> u64 {
        0
    }
    fn tebs(&self) -> Vec<ThreadInfo> {
        Vec::new()
    }
    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        Vec::new()
    }

    // --- Kernel paging (override in kernel providers; out of scope) ---
    fn has_kernel_paging(&self) -> bool {
        false
    }
    fn get_cr3(&self) -> u64 {
        0
    }
    fn translate_address(&self, _va: u64) -> VtopResult {
        VtopResult::default()
    }
    fn read_page_table(&self, _phys_addr: u64, _start_idx: i32, _count: i32) -> Vec<u64> {
        Vec::new()
    }

    // --- Derived convenience (default methods; the C++ non-virtual helpers) ---
    fn is_valid(&self) -> bool {
        self.size() > 0
    }

    /// `isReadable(addr, len)` (`provider.h:122-126`). Overridable (snapshot
    /// defers to the real provider). Default bounds-checks against `size()`.
    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len <= 0 {
            return len == 0;
        }
        let size = self.size() as u64;
        addr <= size && (len as u64) <= size - addr
    }

    fn read_u8(&self, a: u64) -> u8 {
        let mut b = [0u8; 1];
        self.read(a, &mut b);
        b[0]
    }
    fn read_u16(&self, a: u64) -> u16 {
        let mut b = [0u8; 2];
        self.read(a, &mut b);
        u16::from_le_bytes(b)
    }
    fn read_u32(&self, a: u64) -> u32 {
        let mut b = [0u8; 4];
        self.read(a, &mut b);
        u32::from_le_bytes(b)
    }
    fn read_u64(&self, a: u64) -> u64 {
        let mut b = [0u8; 8];
        self.read(a, &mut b);
        u64::from_le_bytes(b)
    }
    fn read_f32(&self, a: u64) -> f32 {
        f32::from_bits(self.read_u32(a))
    }
    fn read_f64(&self, a: u64) -> f64 {
        f64::from_bits(self.read_u64(a))
    }

    /// `readBytes(addr, len)` (`provider.h:142-148`) — zero-filled on read
    /// failure.
    fn read_bytes(&self, addr: u64, len: i32) -> Vec<u8> {
        if len <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        if !self.read(addr, &mut buf) {
            buf.iter_mut().for_each(|b| *b = 0);
        }
        buf
    }

    /// `writeBytes(addr, d)` (`provider.h:150-152`). Non-const in C++; `&self`
    /// here (interior mutability — see [`write`](Provider::write)).
    fn write_bytes(&self, addr: u64, data: &[u8]) -> bool {
        self.write(addr, data)
    }
}
