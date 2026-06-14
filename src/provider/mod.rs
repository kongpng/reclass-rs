//! Data-source abstraction — the `Provider` trait every byte-reading module
//! talks to, the built-in benign sources, and the provider registry.
//!
//! Faithful port of `src/providers/provider.h`, `buffer_provider.h`,
//! `null_provider.h`, `snapshot_provider.h`, and `providerregistry.h`.
//!
//! **In scope (implemented):** [`BufferProvider`] (in-memory + file via
//! `from_file`), [`FileProvider`] (mmap'd binary file), [`NullProvider`],
//! [`SnapshotProvider`], first-party live providers behind `native-providers`,
//! and the connector-backed memflow provider.

use std::fmt::Write as _;

use ahash::AHashMap;
use bytes::Bytes;

mod buffer;
mod file;
#[cfg(feature = "kernel-provider")]
mod kernel;
#[cfg(feature = "memflow-provider")]
pub mod memflow;
pub mod native;
mod null;
#[cfg(feature = "process-provider")]
mod process;
mod read_cache;
mod registry;
#[cfg(feature = "remote-process-provider")]
mod remote;
mod snapshot;
#[cfg(feature = "windbg-provider")]
mod windbg;

pub use buffer::BufferProvider;
pub use file::FileProvider;
#[cfg(feature = "kernel-provider")]
pub use kernel::{KernelMemoryProvider, KernelTarget};
#[cfg(feature = "memflow-provider")]
pub use memflow::{MemflowAttachConfig, MemflowProvider};
pub use null::NullProvider;
#[cfg(feature = "process-provider")]
pub use process::{bench_readable_ranges_from_regions, LocalProcessProvider, ProcessTarget};
pub use read_cache::CachedPageProvider;
pub use registry::{ProviderInfo, ProviderRegistry, SavedSourceDisplay};
#[cfg(feature = "remote-process-provider")]
pub use remote::{
    bench_remote_read_batch_insert_adaptive, bench_remote_read_batch_insert_allocating,
    RemoteProcessProvider, RemoteProcessTarget,
};
pub use snapshot::{PageMap, SnapshotProvider, K_PAGE_SIZE};
#[cfg(feature = "windbg-provider")]
pub use windbg::WinDbgMemoryProvider;

pub(crate) const K_MAX_BULK_READ_PAGES: usize = 64;

pub(crate) enum NormalizedPages<'a> {
    Borrowed(&'a [u64]),
    Owned(Vec<u64>),
}

impl<'a> NormalizedPages<'a> {
    pub(crate) fn as_slice(&self) -> &[u64] {
        match self {
            Self::Borrowed(pages) => pages,
            Self::Owned(pages) => pages,
        }
    }
}

pub(crate) fn normalize_page_list(pages: &[u64]) -> NormalizedPages<'_> {
    let mut previous = None;
    let already_normalized = pages.iter().copied().all(|page| {
        if page & (K_PAGE_SIZE - 1) != 0 {
            return false;
        }
        if previous.is_some_and(|prev| page <= prev) {
            return false;
        }
        previous = Some(page);
        true
    });
    if already_normalized {
        return NormalizedPages::Borrowed(pages);
    }

    let mut sorted_pages: Vec<u64> = pages.iter().map(|page| page & !(K_PAGE_SIZE - 1)).collect();
    sorted_pages.sort_unstable();
    sorted_pages.dedup();
    NormalizedPages::Owned(sorted_pages)
}

#[allow(dead_code)]
pub(crate) fn read_pages_in_runs(
    pages: &[u64],
    mut read: impl FnMut(u64, &mut [u8]) -> bool,
) -> PageMap {
    let mut out = PageMap::new();
    if pages.is_empty() {
        return out;
    }

    let normalized_pages = normalize_page_list(pages);
    let pages = normalized_pages.as_slice();
    out.reserve(pages.len());

    let mut run_start = 0usize;
    while run_start < pages.len() {
        let mut run_end = run_start + 1;
        while run_end < pages.len()
            && run_end - run_start < K_MAX_BULK_READ_PAGES
            && pages[run_end - 1]
                .checked_add(K_PAGE_SIZE)
                .is_some_and(|next| next == pages[run_end])
        {
            run_end += 1;
        }
        read_page_run(&pages[run_start..run_end], &mut read, &mut out);
        run_start = run_end;
    }

    out
}

#[allow(dead_code)]
fn read_page_run(pages: &[u64], read: &mut impl FnMut(u64, &mut [u8]) -> bool, out: &mut PageMap) {
    if pages.is_empty() {
        return;
    }
    if pages.len() == 1 {
        read_single_page(pages[0], read, out);
        return;
    }

    let run_len = pages.len() * K_PAGE_SIZE as usize;
    let mut bytes = vec![0u8; run_len];
    if read(pages[0], &mut bytes) {
        let bytes = Bytes::from(bytes);
        for (idx, &page_addr) in pages.iter().enumerate() {
            let start = idx * K_PAGE_SIZE as usize;
            let end = start + K_PAGE_SIZE as usize;
            out.insert(page_addr, bytes.slice(start..end).into());
        }
        return;
    }

    for &page_addr in pages {
        read_single_page(page_addr, read, out);
    }
}

#[allow(dead_code)]
fn read_single_page(
    page_addr: u64,
    read: &mut impl FnMut(u64, &mut [u8]) -> bool,
    out: &mut PageMap,
) {
    let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
    let _ = read(page_addr, &mut bytes);
    out.insert(page_addr, bytes.into());
}

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

/// `struct VtopResult` (`provider.h:31-37`) — virtual→physical translation for
/// providers that expose kernel paging metadata.
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

/// Indexed module snapshot for live providers.
///
/// Process providers usually snapshot modules once at attach time, then resolve
/// many per-row addresses during compose/hover formatting. A sorted range index
/// turns those point lookups from linear scans into binary searches while a small
/// lowercase map handles reverse lookup by module name, path, or basename.
#[derive(Clone, Debug, Default)]
pub struct ModuleLookup {
    modules: Vec<ModuleEntry>,
    range_order: ModuleLookupOrder,
    name_to_base: AHashMap<String, u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ModuleRange {
    base: u64,
    end: u64,
    module_idx: usize,
}

#[derive(Clone, Debug, Default)]
enum ModuleLookupOrder {
    #[default]
    Input,
    Sorted(Vec<ModuleRange>),
    Linear,
}

pub(crate) struct ModuleRangeLookup<'a> {
    modules: &'a [ModuleEntry],
    range_order: ModuleLookupOrder,
}

impl<'a> ModuleRangeLookup<'a> {
    pub(crate) fn new(modules: &'a [ModuleEntry]) -> Self {
        Self {
            modules,
            range_order: module_lookup_order(modules),
        }
    }

    pub(crate) fn find_by_addr(&self, addr: u64) -> Option<&'a ModuleEntry> {
        find_module_by_addr(self.modules, &self.range_order, addr)
    }
}

impl ModuleLookup {
    pub fn new(modules: Vec<ModuleEntry>) -> Self {
        let range_order = module_lookup_order(&modules);

        let mut name_to_base = AHashMap::with_capacity(modules.len() * 3);
        for module in &modules {
            insert_module_lookup_key(&mut name_to_base, &module.name, module.base);
            insert_module_lookup_key(&mut name_to_base, &module.full_path, module.base);
            insert_module_lookup_key(
                &mut name_to_base,
                module_file_name(&module.full_path),
                module.base,
            );
        }

        Self {
            modules,
            range_order,
            name_to_base,
        }
    }

    pub fn modules(&self) -> &[ModuleEntry] {
        &self.modules
    }

    pub fn clone_modules(&self) -> Vec<ModuleEntry> {
        self.modules.clone()
    }

    pub fn find_by_addr(&self, addr: u64) -> Option<&ModuleEntry> {
        find_module_by_addr(&self.modules, &self.range_order, addr)
    }

    pub fn symbol_for_addr_lower(&self, addr: u64) -> String {
        self.symbol_for_addr(addr, false)
    }

    pub fn symbol_for_addr_upper(&self, addr: u64) -> String {
        self.symbol_for_addr(addr, true)
    }

    fn symbol_for_addr(&self, addr: u64, uppercase: bool) -> String {
        let Some(module) = self.find_by_addr(addr) else {
            return String::new();
        };
        let rel = addr.saturating_sub(module.base);
        let mut out = String::with_capacity(module.name.len() + 3 + 16);
        out.push_str(&module.name);
        out.push_str("+0x");
        if uppercase {
            let _ = write!(out, "{rel:X}");
        } else {
            let _ = write!(out, "{rel:x}");
        }
        out
    }

    pub fn symbol_to_address(&self, name: &str) -> u64 {
        let wanted = name.trim().trim_matches('<').trim_matches('>');
        if wanted.is_empty() {
            return 0;
        }
        self.name_to_base
            .get(&wanted.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
    }
}

fn module_lookup_order(modules: &[ModuleEntry]) -> ModuleLookupOrder {
    if modules_non_overlapping_in_order(modules) {
        return ModuleLookupOrder::Input;
    }

    let mut sorted_ranges: Vec<_> = modules
        .iter()
        .enumerate()
        .map(|(idx, module)| ModuleRange {
            base: module.base,
            end: module.base.saturating_add(module.size),
            module_idx: idx,
        })
        .collect();
    sorted_ranges.sort_unstable_by_key(|range| range.base);
    if sorted_ranges
        .windows(2)
        .all(|pair| pair[0].end <= pair[1].base)
    {
        ModuleLookupOrder::Sorted(sorted_ranges)
    } else {
        ModuleLookupOrder::Linear
    }
}

fn find_module_by_addr<'a>(
    modules: &'a [ModuleEntry],
    range_order: &ModuleLookupOrder,
    addr: u64,
) -> Option<&'a ModuleEntry> {
    match range_order {
        ModuleLookupOrder::Input => {
            let idx = modules.partition_point(|module| module.base <= addr);
            idx.checked_sub(1)
                .and_then(|i| modules.get(i))
                .filter(|module| module_contains_addr(module, addr))
        }
        ModuleLookupOrder::Sorted(sorted_ranges) => {
            let idx = sorted_ranges.partition_point(|range| range.base <= addr);
            idx.checked_sub(1)
                .and_then(|i| sorted_ranges.get(i))
                .filter(|range| addr < range.end)
                .and_then(|range| modules.get(range.module_idx))
        }
        ModuleLookupOrder::Linear => modules
            .iter()
            .find(|module| module_contains_addr(module, addr)),
    }
}

fn modules_non_overlapping_in_order(modules: &[ModuleEntry]) -> bool {
    modules.windows(2).all(|pair| {
        let left_end = pair[0].base.saturating_add(pair[0].size);
        left_end <= pair[1].base
    })
}

fn insert_module_lookup_key(name_to_base: &mut AHashMap<String, u64>, key: &str, base: u64) {
    let key = key.trim();
    if !key.is_empty() {
        name_to_base.entry(key.to_ascii_lowercase()).or_insert(base);
    }
}

fn module_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn module_contains_addr(module: &ModuleEntry, addr: u64) -> bool {
    addr >= module.base
        && addr
            .checked_sub(module.base)
            .is_some_and(|rel| rel < module.size)
}

#[doc(hidden)]
pub fn bench_region_module_label_len_linear(
    region_bases: &[u64],
    modules: &[ModuleEntry],
) -> usize {
    region_bases.iter().fold(0usize, |acc, &addr| {
        acc.wrapping_add(
            modules
                .iter()
                .find(|module| module_contains_addr(module, addr))
                .map_or(0, |module| module.name.len()),
        )
    })
}

#[doc(hidden)]
pub fn bench_region_module_label_len_indexed(
    region_bases: &[u64],
    modules: &[ModuleEntry],
) -> usize {
    let lookup = ModuleRangeLookup::new(modules);
    region_bases.iter().fold(0usize, |acc, &addr| {
        acc.wrapping_add(
            lookup
                .find_by_addr(addr)
                .map_or(0, |module| module.name.len()),
        )
    })
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
    /// Scanner rescans can either read page-spaced hits individually or coalesce
    /// them into larger spans. High-call-overhead live providers override this;
    /// cheap in-memory/file providers keep the sparse tiny-read path.
    fn prefers_coalesced_rescan_reads(&self) -> bool {
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
    /// True when a readable entry from `enumerate_regions()` is authoritative
    /// enough for UI preview/link decisions. Live process providers can avoid
    /// an extra one-byte target probe per visible pointer hint.
    fn trusts_enumerated_region_readability(&self) -> bool {
        false
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
    /// Enumerate modules and regions from one provider snapshot when the provider
    /// can do that cheaper or more consistently than two independent calls.
    fn enumerate_modules_and_regions(&self) -> (Vec<ModuleEntry>, Vec<MemoryRegion>) {
        (self.enumerate_modules(), self.enumerate_regions())
    }
    /// Enumerate regions using a caller-supplied module snapshot. Providers that
    /// label regions by module can avoid rebuilding the same module index.
    fn enumerate_regions_with_modules(&self, _modules: &[ModuleEntry]) -> Vec<MemoryRegion> {
        self.enumerate_regions()
    }

    // --- Kernel paging (override in kernel providers) ---
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
        let _ = self.read(addr, &mut buf);
        buf
    }

    /// Read page-aligned 4 KiB chunks for refresh snapshots. Providers with
    /// expensive per-call I/O can override this to coalesce contiguous pages.
    fn read_pages(&self, pages: &[u64]) -> PageMap {
        read_pages_in_runs(pages, |addr, buf| self.read(addr, buf))
    }

    /// `writeBytes(addr, d)` (`provider.h:150-152`). Non-const in C++; `&self`
    /// here (interior mutability — see [`write`](Provider::write)).
    fn write_bytes(&self, addr: u64, data: &[u8]) -> bool {
        self.write(addr, data)
    }

    /// Provider-specific memflow downhook for optional memflow-native analysis
    /// backends (for example scanflow). Kept as a narrow provider-layer escape
    /// hatch instead of making every provider pretend to support those APIs.
    #[cfg(feature = "memflow-provider")]
    fn as_memflow_provider(&self) -> Option<&memflow::MemflowProvider> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_page_list, read_pages_in_runs, ModuleEntry, ModuleLookup, ModuleRangeLookup,
        NormalizedPages, Provider, K_PAGE_SIZE,
    };
    use std::sync::Mutex;

    #[test]
    fn module_lookup_indexes_address_name_path_and_file_name() {
        let lookup = ModuleLookup::new(vec![
            ModuleEntry {
                name: "Second.dll".to_string(),
                full_path: r"C:\Game\Second.dll".to_string(),
                base: 0x3000,
                size: 0x1000,
            },
            ModuleEntry {
                name: "FirstModule".to_string(),
                full_path: r"C:\Game\Bin\First.dll".to_string(),
                base: 0x1000,
                size: 0x1000,
            },
        ]);

        assert_eq!(lookup.find_by_addr(0x1004).unwrap().name, "FirstModule");
        assert_eq!(lookup.find_by_addr(0x3004).unwrap().name, "Second.dll");
        assert!(lookup.find_by_addr(0x2000).is_none());
        assert_eq!(lookup.symbol_to_address("firstmodule"), 0x1000);
        assert_eq!(lookup.symbol_to_address("first.dll"), 0x1000);
        assert_eq!(lookup.symbol_to_address(r"c:\game\bin\first.dll"), 0x1000);
        assert_eq!(lookup.symbol_to_address("second.dll"), 0x3000);
        assert_eq!(lookup.symbol_to_address("<Second.dll>"), 0x3000);
    }

    #[test]
    fn module_lookup_preserves_original_order_for_overlapping_ranges() {
        let lookup = ModuleLookup::new(vec![
            ModuleEntry {
                name: "first".to_string(),
                base: 0x1000,
                size: 0x2000,
                ..ModuleEntry::default()
            },
            ModuleEntry {
                name: "second".to_string(),
                base: 0x1800,
                size: 0x1000,
                ..ModuleEntry::default()
            },
        ]);

        assert_eq!(lookup.find_by_addr(0x1900).unwrap().name, "first");
    }

    #[test]
    fn borrowed_module_range_lookup_preserves_original_order_for_overlaps() {
        let modules = vec![
            ModuleEntry {
                name: "first".to_string(),
                base: 0x1000,
                size: 0x2000,
                ..ModuleEntry::default()
            },
            ModuleEntry {
                name: "second".to_string(),
                base: 0x1800,
                size: 0x1000,
                ..ModuleEntry::default()
            },
        ];
        let lookup = ModuleRangeLookup::new(&modules);

        assert_eq!(lookup.find_by_addr(0x1900).unwrap().name, "first");
        assert!(lookup.find_by_addr(0x4000).is_none());
    }

    #[test]
    fn read_pages_in_runs_coalesces_sorted_contiguous_pages() {
        let pages = [
            K_PAGE_SIZE * 2,
            0,
            K_PAGE_SIZE,
            K_PAGE_SIZE,
            K_PAGE_SIZE * 3,
        ];
        let mut reads = Vec::new();

        let result = read_pages_in_runs(&pages, |addr, buf| {
            reads.push((addr, buf.len()));
            for page_idx in 0..(buf.len() / K_PAGE_SIZE as usize) {
                let start = page_idx * K_PAGE_SIZE as usize;
                buf[start] = ((addr / K_PAGE_SIZE) as usize + page_idx) as u8;
            }
            true
        });

        assert_eq!(reads, vec![(0, (K_PAGE_SIZE as usize) * 4)]);
        assert_eq!(result.len(), 4);
        assert_eq!(result.get(&0).unwrap()[0], 0);
        assert_eq!(result.get(&K_PAGE_SIZE).unwrap()[0], 1);
        assert_eq!(result.get(&(K_PAGE_SIZE * 2)).unwrap()[0], 2);
        assert_eq!(result.get(&(K_PAGE_SIZE * 3)).unwrap()[0], 3);
    }

    #[derive(Default)]
    struct DefaultReadPagesProvider {
        reads: Mutex<Vec<(u64, usize)>>,
    }

    impl Provider for DefaultReadPagesProvider {
        fn size(&self) -> i32 {
            (K_PAGE_SIZE as i32) * 4
        }

        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.reads.lock().unwrap().push((addr, buf.len()));
            for page_idx in 0..(buf.len() / K_PAGE_SIZE as usize) {
                let start = page_idx * K_PAGE_SIZE as usize;
                buf[start] = ((addr / K_PAGE_SIZE) as usize + page_idx) as u8;
            }
            true
        }
    }

    #[test]
    fn default_read_pages_coalesces_contiguous_pages() {
        let provider = DefaultReadPagesProvider::default();
        let pages = [0, K_PAGE_SIZE, K_PAGE_SIZE * 2, K_PAGE_SIZE * 3];

        let result = provider.read_pages(&pages);

        assert_eq!(
            provider.reads.lock().unwrap().as_slice(),
            &[(0, K_PAGE_SIZE as usize * 4)]
        );
        assert_eq!(result.len(), 4);
        assert_eq!(result.get(&(K_PAGE_SIZE * 3)).unwrap()[0], 3);
    }

    #[test]
    fn normalize_page_list_borrows_sorted_aligned_unique_pages() {
        let pages = [0, K_PAGE_SIZE, K_PAGE_SIZE * 2];

        let normalized = normalize_page_list(&pages);

        assert!(matches!(normalized, NormalizedPages::Borrowed(_)));
        assert_eq!(normalized.as_slice(), pages);
    }

    #[test]
    fn normalize_page_list_sorts_aligns_and_deduplicates_pages() {
        let pages = [K_PAGE_SIZE + 7, 0, K_PAGE_SIZE, K_PAGE_SIZE * 3];

        let normalized = normalize_page_list(&pages);

        assert!(matches!(normalized, NormalizedPages::Owned(_)));
        assert_eq!(normalized.as_slice(), [0, K_PAGE_SIZE, K_PAGE_SIZE * 3]);
    }

    #[test]
    fn read_pages_in_runs_falls_back_when_bulk_read_fails() {
        let pages = [0, K_PAGE_SIZE];
        let mut reads = Vec::new();

        let result = read_pages_in_runs(&pages, |addr, buf| {
            reads.push((addr, buf.len()));
            if buf.len() > K_PAGE_SIZE as usize {
                return false;
            }
            buf[0] = (addr / K_PAGE_SIZE) as u8;
            true
        });

        assert_eq!(
            reads,
            vec![
                (0, (K_PAGE_SIZE as usize) * 2),
                (0, K_PAGE_SIZE as usize),
                (K_PAGE_SIZE, K_PAGE_SIZE as usize),
            ]
        );
        assert_eq!(result.get(&0).unwrap()[0], 0);
        assert_eq!(result.get(&K_PAGE_SIZE).unwrap()[0], 1);
    }
}
