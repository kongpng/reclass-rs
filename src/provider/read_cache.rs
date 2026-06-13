use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use ahash::{AHashMap, AHashSet};
use parking_lot::Mutex;

use super::{MemoryRegion, ModuleEntry, PageMap, Provider, ThreadInfo, VtopResult, K_PAGE_SIZE};

const K_PAGE_MASK: u64 = !(K_PAGE_SIZE - 1);

/// Short-lived page cache for a single live-provider refresh/compose pass.
///
/// Live process providers pay real syscall/transport cost for tiny adjacent
/// reads. This wrapper turns repeated reads within the same 4KB page into one
/// full-page read while preserving boundary behavior: if a complete page read
/// fails, the wrapper remembers that page for this pass, falls back to exact
/// requested reads, and does not cache that page.
pub struct CachedPageProvider {
    inner: Arc<dyn Provider + Send + Sync>,
    pages: Mutex<PageCache>,
    regions: OnceLock<ReadableRegionLookup>,
}

impl CachedPageProvider {
    pub fn new(inner: Arc<dyn Provider + Send + Sync>) -> Self {
        Self {
            inner,
            pages: Mutex::new(PageCache::default()),
            regions: OnceLock::new(),
        }
    }

    fn read_cached_chunk(&self, page_addr: u64, page_off: usize, out: &mut [u8]) -> bool {
        {
            let mut pages = self.pages.lock();
            if let Some(page) = pages.get(page_addr) {
                out.copy_from_slice(&page[page_off..page_off + out.len()]);
                return true;
            }
            if pages.is_uncacheable(page_addr) {
                drop(pages);
                return self.inner.read(page_addr + page_off as u64, out);
            }
        }

        let mut page = Box::new([0u8; K_PAGE_SIZE as usize]);
        if self.inner.read(page_addr, page.as_mut_slice()) {
            out.copy_from_slice(&page[page_off..page_off + out.len()]);
            self.pages.lock().insert(page_addr, page);
            true
        } else {
            self.pages.lock().mark_uncacheable(page_addr);
            self.inner.read(page_addr + page_off as u64, out)
        }
    }
}

impl Provider for CachedPageProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if buf.is_empty() {
            return false;
        }

        let mut cur = addr;
        let mut off = 0usize;
        let mut ok = true;
        while off < buf.len() {
            let page_addr = cur & K_PAGE_MASK;
            let page_off = (cur - page_addr) as usize;
            let chunk = (buf.len() - off).min((K_PAGE_SIZE as usize) - page_off);
            ok &= self.read_cached_chunk(page_addr, page_off, &mut buf[off..off + chunk]);
            off += chunk;
            cur += chunk as u64;
        }
        ok
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
        self.inner.read_pages(pages)
    }

    fn size(&self) -> i32 {
        self.inner.size()
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        let ok = self.inner.write(addr, data);
        if ok {
            self.pages.lock().clear();
        }
        ok
    }

    fn is_writable(&self) -> bool {
        self.inner.is_writable()
    }

    fn name(&self) -> String {
        self.inner.name()
    }

    fn is_live(&self) -> bool {
        self.inner.is_live()
    }

    fn prefers_coalesced_rescan_reads(&self) -> bool {
        self.inner.prefers_coalesced_rescan_reads()
    }

    fn kind(&self) -> String {
        self.inner.kind()
    }

    fn pointer_size(&self) -> i32 {
        self.inner.pointer_size()
    }

    fn base(&self) -> u64 {
        self.inner.base()
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.inner.get_symbol(addr)
    }

    fn symbol_to_address(&self, name: &str) -> u64 {
        self.inner.symbol_to_address(name)
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions()
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        self.inner.trusts_enumerated_region_readability()
    }

    fn peb(&self) -> u64 {
        self.inner.peb()
    }

    fn tebs(&self) -> Vec<ThreadInfo> {
        self.inner.tebs()
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }

    fn has_kernel_paging(&self) -> bool {
        self.inner.has_kernel_paging()
    }

    fn get_cr3(&self) -> u64 {
        self.inner.get_cr3()
    }

    fn translate_address(&self, va: u64) -> VtopResult {
        self.inner.translate_address(va)
    }

    fn read_page_table(&self, phys_addr: u64, start_idx: i32, count: i32) -> Vec<u64> {
        self.inner.read_page_table(phys_addr, start_idx, count)
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len <= 0 {
            return len == 0;
        }
        let regions = self
            .regions
            .get_or_init(|| ReadableRegionLookup::new(self.inner.enumerate_regions()));
        if regions.has_regions() {
            regions.contains(addr, len)
        } else {
            self.inner.is_readable(addr, len)
        }
    }
}

#[derive(Default)]
struct PageCache {
    pages: Vec<CachedPage>,
    page_to_index: AHashMap<u64, usize>,
    uncacheable_pages: AHashSet<u64>,
    last_index: Option<usize>,
}

struct CachedPage {
    addr: u64,
    bytes: Box<[u8; K_PAGE_SIZE as usize]>,
}

impl PageCache {
    fn get(&mut self, page_addr: u64) -> Option<&[u8; K_PAGE_SIZE as usize]> {
        if let Some(index) = self.last_index {
            if let Some(page) = self.pages.get(index) {
                if page.addr == page_addr {
                    return Some(&page.bytes);
                }
            }
        }

        let index = self.page_to_index.get(&page_addr).copied()?;
        self.last_index = Some(index);
        self.pages.get(index).map(|page| &*page.bytes)
    }

    fn insert(&mut self, page_addr: u64, page: Box<[u8; K_PAGE_SIZE as usize]>) {
        self.uncacheable_pages.remove(&page_addr);
        if let Some(index) = self.page_to_index.get(&page_addr).copied() {
            self.pages[index].bytes = page;
            self.last_index = Some(index);
            return;
        }

        let index = self.pages.len();
        self.pages.push(CachedPage {
            addr: page_addr,
            bytes: page,
        });
        self.page_to_index.insert(page_addr, index);
        self.last_index = Some(index);
    }

    fn is_uncacheable(&self, page_addr: u64) -> bool {
        self.uncacheable_pages.contains(&page_addr)
    }

    fn mark_uncacheable(&mut self, page_addr: u64) {
        self.uncacheable_pages.insert(page_addr);
    }

    fn clear(&mut self) {
        self.pages.clear();
        self.page_to_index.clear();
        self.uncacheable_pages.clear();
        self.last_index = None;
    }
}

struct ReadableRegionLookup {
    regions: Vec<MemoryRegion>,
    order: RegionLookupOrder,
    last_region_index: AtomicUsize,
}

enum RegionLookupOrder {
    Input,
    Sorted(Vec<usize>),
    Linear,
}

impl ReadableRegionLookup {
    fn new(regions: Vec<MemoryRegion>) -> Self {
        let order = region_lookup_order(&regions);
        Self {
            regions,
            order,
            last_region_index: AtomicUsize::new(usize::MAX),
        }
    }

    fn has_regions(&self) -> bool {
        !self.regions.is_empty()
    }

    fn contains(&self, addr: u64, len: i32) -> bool {
        if let Some(region) = self.last_region() {
            if region_contains_readable(region, addr, len) {
                return true;
            }
        }

        match &self.order {
            RegionLookupOrder::Input => {
                let idx = self.regions.partition_point(|region| region.base <= addr);
                idx.checked_sub(1)
                    .and_then(|i| self.region_contains_at(i, addr, len))
                    .unwrap_or(false)
            }
            RegionLookupOrder::Sorted(indices) => {
                let idx = indices.partition_point(|&i| self.regions[i].base <= addr);
                idx.checked_sub(1)
                    .and_then(|i| indices.get(i))
                    .and_then(|&i| self.region_contains_at(i, addr, len))
                    .unwrap_or(false)
            }
            RegionLookupOrder::Linear => self.regions.iter().enumerate().any(|(idx, region)| {
                let contains = region_contains_readable(region, addr, len);
                if contains {
                    self.last_region_index.store(idx, Ordering::Relaxed);
                }
                contains
            }),
        }
    }

    fn last_region(&self) -> Option<&MemoryRegion> {
        let idx = self.last_region_index.load(Ordering::Relaxed);
        self.regions.get(idx)
    }

    fn region_contains_at(&self, idx: usize, addr: u64, len: i32) -> Option<bool> {
        let region = self.regions.get(idx)?;
        let contains = region_contains_readable(region, addr, len);
        if contains {
            self.last_region_index.store(idx, Ordering::Relaxed);
        }
        Some(contains)
    }
}

fn region_lookup_order(regions: &[MemoryRegion]) -> RegionLookupOrder {
    if regions_non_overlapping_in_order(regions) {
        return RegionLookupOrder::Input;
    }
    let mut indices: Vec<usize> = (0..regions.len()).collect();
    indices.sort_unstable_by_key(|&i| regions[i].base);
    if regions_non_overlapping_by_index(regions, &indices) {
        RegionLookupOrder::Sorted(indices)
    } else {
        RegionLookupOrder::Linear
    }
}

fn regions_non_overlapping_in_order(regions: &[MemoryRegion]) -> bool {
    regions.windows(2).all(|pair| {
        let left_end = pair[0].base.saturating_add(pair[0].size);
        left_end <= pair[1].base
    })
}

fn regions_non_overlapping_by_index(regions: &[MemoryRegion], indices: &[usize]) -> bool {
    indices.windows(2).all(|pair| {
        let left = &regions[pair[0]];
        let right = &regions[pair[1]];
        left.base.saturating_add(left.size) <= right.base
    })
}

fn region_contains_readable(region: &MemoryRegion, addr: u64, len: i32) -> bool {
    region.readable
        && len >= 0
        && addr >= region.base
        && addr
            .checked_add(len as u64)
            .is_some_and(|end| end <= region.base.saturating_add(region.size))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingProvider {
        data: Vec<u8>,
        regions: Vec<MemoryRegion>,
        reads: AtomicUsize,
        region_enumerations: AtomicUsize,
        readability_checks: AtomicUsize,
    }

    impl CountingProvider {
        fn new(data: Vec<u8>) -> Self {
            let regions = vec![MemoryRegion {
                base: 0,
                size: data.len() as u64,
                readable: true,
                writable: false,
                executable: false,
                module_name: String::new(),
                region_type: crate::provider::RegionType::Private,
            }];
            Self {
                data,
                regions,
                reads: AtomicUsize::new(0),
                region_enumerations: AtomicUsize::new(0),
                readability_checks: AtomicUsize::new(0),
            }
        }

        fn without_regions(data: Vec<u8>) -> Self {
            Self {
                data,
                regions: Vec::new(),
                reads: AtomicUsize::new(0),
                region_enumerations: AtomicUsize::new(0),
                readability_checks: AtomicUsize::new(0),
            }
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::Relaxed)
        }

        fn region_enumerations(&self) -> usize {
            self.region_enumerations.load(Ordering::Relaxed)
        }

        fn readability_checks(&self) -> usize {
            self.readability_checks.load(Ordering::Relaxed)
        }
    }

    impl Provider for CountingProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let start = addr as usize;
            let Some(end) = start.checked_add(buf.len()) else {
                return false;
            };
            if end > self.data.len() {
                return false;
            }
            buf.copy_from_slice(&self.data[start..end]);
            true
        }

        fn size(&self) -> i32 {
            self.data.len() as i32
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.region_enumerations.fetch_add(1, Ordering::Relaxed);
            self.regions.clone()
        }

        fn is_readable(&self, addr: u64, len: i32) -> bool {
            self.readability_checks.fetch_add(1, Ordering::Relaxed);
            if len <= 0 {
                return len == 0;
            }
            let size = self.data.len() as u64;
            addr <= size && (len as u64) <= size - addr
        }
    }

    #[test]
    fn cached_page_provider_coalesces_same_page_reads() {
        let real = Arc::new(CountingProvider::new(
            (0..K_PAGE_SIZE as usize * 2).map(|i| i as u8).collect(),
        ));
        let cache = CachedPageProvider::new(real.clone());

        let mut a = [0u8; 8];
        let mut b = [0u8; 8];
        assert!(cache.read(16, &mut a));
        assert!(cache.read(128, &mut b));

        assert_eq!(a, [16, 17, 18, 19, 20, 21, 22, 23]);
        assert_eq!(b, [128, 129, 130, 131, 132, 133, 134, 135]);
        assert_eq!(real.reads(), 1);
        assert_eq!(real.region_enumerations(), 0);
        assert_eq!(real.readability_checks(), 0);
    }

    #[test]
    fn cached_page_provider_falls_back_when_full_page_read_fails() {
        let real = Arc::new(CountingProvider::without_regions(vec![0xA5; 64]));
        let cache = CachedPageProvider::new(real.clone());

        let mut a = [0u8; 8];
        let mut b = [0u8; 8];
        assert!(cache.read(16, &mut a));
        assert!(cache.read(24, &mut b));

        assert_eq!(a, [0xA5; 8]);
        assert_eq!(b, [0xA5; 8]);
        assert_eq!(real.reads(), 3);
        assert_eq!(real.region_enumerations(), 0);
        assert_eq!(real.readability_checks(), 0);
    }

    #[test]
    fn cached_page_provider_uses_unsorted_region_snapshot_for_page_reads() {
        let real = Arc::new(CountingProvider {
            data: vec![0x5A; K_PAGE_SIZE as usize * 3],
            regions: vec![
                MemoryRegion {
                    base: K_PAGE_SIZE * 2,
                    size: K_PAGE_SIZE,
                    readable: true,
                    writable: false,
                    executable: false,
                    module_name: String::new(),
                    region_type: crate::provider::RegionType::Private,
                },
                MemoryRegion {
                    base: 0,
                    size: K_PAGE_SIZE,
                    readable: true,
                    writable: false,
                    executable: false,
                    module_name: String::new(),
                    region_type: crate::provider::RegionType::Private,
                },
            ],
            reads: AtomicUsize::new(0),
            region_enumerations: AtomicUsize::new(0),
            readability_checks: AtomicUsize::new(0),
        });
        let cache = CachedPageProvider::new(real.clone());

        let mut first = [0u8; 8];
        let mut third = [0u8; 8];
        assert!(cache.read(16, &mut first));
        assert!(cache.read(K_PAGE_SIZE * 2 + 16, &mut third));

        assert_eq!(first, [0x5A; 8]);
        assert_eq!(third, [0x5A; 8]);
        assert_eq!(real.reads(), 2);
        assert_eq!(real.region_enumerations(), 0);
        assert_eq!(real.readability_checks(), 0);
    }

    #[test]
    fn cached_page_provider_answers_readability_from_region_snapshot() {
        let real = Arc::new(CountingProvider::new(vec![0xA5; K_PAGE_SIZE as usize * 2]));
        let cache = CachedPageProvider::new(real.clone());

        assert!(cache.is_readable(16, 8));
        assert!(cache.is_readable(K_PAGE_SIZE + 16, 8));
        assert!(!cache.is_readable(K_PAGE_SIZE * 2, 8));

        assert_eq!(real.region_enumerations(), 1);
        assert_eq!(real.readability_checks(), 0);
    }

    #[test]
    fn readable_region_lookup_keeps_linear_overlap_semantics() {
        let lookup = ReadableRegionLookup::new(vec![
            MemoryRegion {
                base: 0,
                size: K_PAGE_SIZE * 2,
                readable: false,
                writable: false,
                executable: false,
                module_name: String::new(),
                region_type: crate::provider::RegionType::Private,
            },
            MemoryRegion {
                base: K_PAGE_SIZE,
                size: K_PAGE_SIZE,
                readable: true,
                writable: false,
                executable: false,
                module_name: String::new(),
                region_type: crate::provider::RegionType::Private,
            },
        ]);

        assert!(lookup.contains(K_PAGE_SIZE + 16, 8));
        assert!(!lookup.contains(16, 8));
    }
}
