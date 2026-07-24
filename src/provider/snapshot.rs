//! `SnapshotProvider` — a page-table-backed provider the controller builds from
//! async reads, so compose never blocks on I/O.
//!
//! Faithful port of `src/providers/snapshot_provider.h`. Holds a page map
//! (page-aligned addr → 4096-byte page) plus an optional real provider for
//! write-through and read fall-through (required by the RTTI auto-hint reading
//! vtable/.rdata bytes that weren't pre-fetched). Pages never fetched read as
//! zeros.

use std::collections::HashSet;
use std::ops::Deref;
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard};

use ahash::AHashMap;
use bytes::Bytes;

use super::{MemoryRegion, ModuleEntry, Provider, ThreadInfo};

/// `SnapshotProvider::kPageSize` (`snapshot_provider.h:29`).
pub const K_PAGE_SIZE: u64 = 4096;
const K_PAGE_MASK: u64 = !(K_PAGE_SIZE - 1);

/// `using PageMap = QHash<uint64_t, QByteArray>` (`snapshot_provider.h:33`).
pub type PageMap = AHashMap<u64, PageBytes>;

/// Shared immutable page bytes.
///
/// Refresh keeps the same fresh page in both the snapshot provider and the
/// previous-read baseline. `Bytes` makes that split a cheap refcount bump;
/// snapshot write-through uses copy-on-write before patching cached bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageBytes {
    Shared(Bytes),
    Mutable(Vec<u8>),
}

impl Default for PageBytes {
    fn default() -> Self {
        Self::Shared(Bytes::new())
    }
}

impl PageBytes {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Shared(bytes) => bytes.as_ref(),
            Self::Mutable(bytes) => bytes.as_slice(),
        }
    }

    pub fn make_mut(&mut self) -> &mut [u8] {
        if let Self::Shared(bytes) = self {
            *self = Self::Mutable(bytes.to_vec());
        }
        match self {
            Self::Shared(_) => unreachable!("shared page was converted to mutable storage"),
            Self::Mutable(bytes) => bytes.as_mut_slice(),
        }
    }
}

impl From<Vec<u8>> for PageBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Shared(Bytes::from(bytes))
    }
}

impl From<Bytes> for PageBytes {
    fn from(bytes: Bytes) -> Self {
        Self::Shared(bytes)
    }
}

impl From<Box<[u8]>> for PageBytes {
    fn from(bytes: Box<[u8]>) -> Self {
        Self::Shared(Bytes::from(bytes))
    }
}

impl Deref for PageBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

/// Page table + logical extent — mutated by `update_pages`/`merge_pages`/
/// `patch_pages` (and `write`) through `&self`, so it lives behind a lock
/// (PORTING_providers §5).
#[derive(Default)]
struct SnapshotInner {
    pages: PageMap,
    main_extent: i32,
}

/// `class SnapshotProvider : public Provider` (`snapshot_provider.h:16-177`).
pub struct SnapshotProvider {
    real: Option<Arc<dyn Provider + Send + Sync>>,
    inner: RwLock<SnapshotInner>,
    permanent_pages: RwLock<HashSet<u64>>,
    real_readable_ranges: RwLock<Option<Vec<(u64, u64)>>>,
    // Module snapshots are immutable for one attach. Compose may ask for them
    // once per pass while resolving RTTI/type hints, so retain the provider's
    // expensive enumeration for the lifetime of this snapshot provider.
    real_modules: OnceLock<Vec<ModuleEntry>>,
}

impl SnapshotProvider {
    /// `SnapshotProvider(real, pages, mainExtent)` (`snapshot_provider.h:35-38`).
    pub fn new(
        real: Option<Arc<dyn Provider + Send + Sync>>,
        pages: PageMap,
        main_extent: i32,
    ) -> Self {
        SnapshotProvider {
            real,
            inner: RwLock::new(SnapshotInner { pages, main_extent }),
            permanent_pages: RwLock::new(HashSet::new()),
            real_readable_ranges: RwLock::new(None),
            real_modules: OnceLock::new(),
        }
    }

    /// `updatePages(pages, mainExtent)` (`snapshot_provider.h:130-133`).
    pub fn update_pages(&self, pages: PageMap, main_extent: i32) {
        let mut inner = self.inner.write().unwrap();
        inner.pages = pages;
        inner.main_extent = main_extent;
        self.invalidate_real_readable_ranges();
    }

    /// `mergePages(fresh, mainExtent)` (`snapshot_provider.h:139-143`) — fresh
    /// pages overwrite, absent pages keep their old bytes.
    pub fn merge_pages(&self, fresh: &PageMap, main_extent: i32) {
        let mut inner = self.inner.write().unwrap();
        for (k, v) in fresh {
            inner.pages.insert(*k, v.clone());
        }
        inner.main_extent = main_extent;
        self.invalidate_real_readable_ranges();
    }

    /// `markPermanent(pageAddr)` (`snapshot_provider.h:148-150`).
    pub fn mark_permanent(&self, page_addr: u64) {
        self.permanent_pages
            .write()
            .unwrap()
            .insert(page_addr & K_PAGE_MASK);
    }

    /// `isPermanent(pageAddr)` (`snapshot_provider.h:151-153`).
    pub fn is_permanent(&self, page_addr: u64) -> bool {
        self.permanent_pages
            .read()
            .unwrap()
            .contains(&(page_addr & K_PAGE_MASK))
    }
    /// `clearPermanent()` (`snapshot_provider.h:154`).
    pub fn clear_permanent(&self) {
        self.permanent_pages.write().unwrap().clear();
    }

    /// `patchPages(addr, buf, len)` (`snapshot_provider.h:157-173`) — overwrite
    /// bytes in *existing* pages only.
    pub fn patch_pages(&self, addr: u64, data: &[u8]) {
        let mut inner = self.inner.write().unwrap();
        let mut cur = addr;
        let mut off = 0usize;
        while off < data.len() {
            let page_addr = cur & K_PAGE_MASK;
            let page_off = (cur - page_addr) as usize;
            let chunk = (data.len() - off).min((K_PAGE_SIZE as usize) - page_off);
            if let Some(page) = inner.pages.get_mut(&page_addr) {
                if page_off + chunk <= page.len() {
                    let page = page.make_mut();
                    page[page_off..page_off + chunk].copy_from_slice(&data[off..off + chunk]);
                }
            }
            off += chunk;
            cur += chunk as u64;
        }
    }

    /// `const PageMap& pages() const` (`snapshot_provider.h:175`) — a cloned
    /// snapshot of the page table (the lock precludes returning a borrow).
    pub fn pages(&self) -> PageMap {
        self.inner.read().unwrap().pages.clone()
    }
    /// `const QSet& permanentPages() const` (`snapshot_provider.h:176`) — a
    /// read-lock guard over the permanent-page set (callers only iterate it).
    pub fn permanent_pages(&self) -> RwLockReadGuard<'_, HashSet<u64>> {
        self.permanent_pages.read().unwrap()
    }

    fn invalidate_real_readable_ranges(&self) {
        *self.real_readable_ranges.write().unwrap() = None;
    }

    fn real_readable_ranges_lookup(&self, addr: u64, len: i32) -> (bool, bool) {
        let cached = self.real_readable_ranges.read().unwrap();
        if let Some(ranges) = cached.as_ref() {
            return (
                readable_ranges_contain(ranges.as_slice(), addr, len),
                ranges.is_empty(),
            );
        }
        drop(cached);

        let ranges = readable_ranges_from_real(&self.real);
        let mut cached = self.real_readable_ranges.write().unwrap();
        if cached.is_none() {
            *cached = Some(ranges);
        }
        drop(cached);

        let cached = self.real_readable_ranges.read().unwrap();
        let ranges = cached.as_ref().map(Vec::as_slice).unwrap_or(&[]);
        (
            readable_ranges_contain(ranges, addr, len),
            ranges.is_empty(),
        )
    }
}

fn readable_ranges_from_real(real: &Option<Arc<dyn Provider + Send + Sync>>) -> Vec<(u64, u64)> {
    let Some(real) = real else {
        return Vec::new();
    };
    let mut ranges: Vec<(u64, u64)> = real
        .enumerate_regions()
        .into_iter()
        .filter_map(|region| {
            if !region.readable || region.size == 0 {
                return None;
            }
            let end = region.base.saturating_add(region.size);
            (end > region.base).then_some((region.base, end))
        })
        .collect();
    ranges.sort_unstable_by_key(|(start, _)| *start);
    let mut write = 0usize;
    for read in 0..ranges.len() {
        let (start, end) = ranges[read];
        if write > 0 && start <= ranges[write - 1].1 {
            ranges[write - 1].1 = ranges[write - 1].1.max(end);
        } else {
            ranges[write] = (start, end);
            write += 1;
        }
    }
    ranges.truncate(write);
    ranges
}

fn readable_ranges_contain(ranges: &[(u64, u64)], addr: u64, len: i32) -> bool {
    if len <= 0 {
        return len == 0;
    }
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    let idx = ranges.partition_point(|(_, range_end)| *range_end <= addr);
    ranges
        .get(idx)
        .is_some_and(|(start, range_end)| *start <= addr && end <= *range_end)
}

impl Provider for SnapshotProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if buf.is_empty() {
            return false;
        }
        let inner = self.inner.read().unwrap();
        let page_off = (addr & (K_PAGE_SIZE - 1)) as usize;
        if inner.pages.is_empty() && page_off + buf.len() <= K_PAGE_SIZE as usize {
            drop(inner);
            if let Some(real) = &self.real {
                if !real.read(addr, buf) {
                    buf.fill(0);
                }
            } else {
                buf.fill(0);
            }
            return true;
        }
        let mut cur = addr;
        let mut off = 0usize;
        while off < buf.len() {
            let page_addr = cur & K_PAGE_MASK;
            let page_off = (cur - page_addr) as usize;
            let chunk = (buf.len() - off).min((K_PAGE_SIZE as usize) - page_off);
            if let Some(page) = inner.pages.get(&page_addr) {
                let end = (page_off + chunk).min(page.len());
                let n = end.saturating_sub(page_off);
                buf[off..off + n].copy_from_slice(&page[page_off..page_off + n]);
                for b in &mut buf[off + n..off + chunk] {
                    *b = 0;
                }
            } else if let Some(real) = &self.real {
                let out = &mut buf[off..off + chunk];
                if !real.read(cur, out) {
                    out.fill(0);
                }
            } else {
                for b in &mut buf[off..off + chunk] {
                    *b = 0;
                }
            }
            off += chunk;
            cur += chunk as u64;
        }
        true
    }

    /// `isReadable` (`snapshot_provider.h:74-91`) — defers to the real provider
    /// for pages not in the snapshot.
    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len <= 0 {
            return len == 0;
        }
        let end = match addr.checked_add(len as u64) {
            Some(e) => e,
            None => return false,
        };
        {
            let inner = self.inner.read().unwrap();
            let mut p = addr & K_PAGE_MASK;
            while p < end {
                if !inner.pages.contains_key(&p) {
                    drop(inner);
                    let (readable, ranges_empty) = self.real_readable_ranges_lookup(addr, len);
                    if readable {
                        return true;
                    }
                    if ranges_empty {
                        if let Some(real) = &self.real {
                            return real.is_readable(addr, len);
                        }
                    }
                    return false;
                }
                p += K_PAGE_SIZE;
            }
        }
        true
    }

    fn size(&self) -> i32 {
        self.inner.read().unwrap().main_extent
    }
    fn is_writable(&self) -> bool {
        self.real.as_ref().map_or(false, |r| r.is_writable())
    }
    fn is_live(&self) -> bool {
        self.real.as_ref().map_or(false, |r| r.is_live())
    }
    fn name(&self) -> String {
        self.real.as_ref().map_or(String::new(), |r| r.name())
    }
    fn kind(&self) -> String {
        self.real
            .as_ref()
            .map_or_else(|| "File".to_string(), |r| r.kind())
    }
    fn pointer_size(&self) -> i32 {
        self.real.as_ref().map_or(8, |r| r.pointer_size())
    }
    fn base(&self) -> u64 {
        self.real.as_ref().map_or(0, |r| r.base())
    }
    fn get_symbol(&self, addr: u64) -> String {
        self.real
            .as_ref()
            .map_or(String::new(), |r| r.get_symbol(addr))
    }
    fn symbol_to_address(&self, name: &str) -> u64 {
        self.real.as_ref().map_or(0, |r| r.symbol_to_address(name))
    }
    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.real_modules
            .get_or_init(|| {
                self.real
                    .as_ref()
                    .map_or_else(Vec::new, |r| r.modules_cached())
            })
            .clone()
    }
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.real
            .as_ref()
            .map_or_else(Vec::new, |r| r.enumerate_regions())
    }
    fn peb(&self) -> u64 {
        self.real.as_ref().map_or(0, |r| r.peb())
    }
    fn tebs(&self) -> Vec<ThreadInfo> {
        self.real.as_ref().map_or_else(Vec::new, |r| r.tebs())
    }

    /// `write` (`snapshot_provider.h:122-127`) — write-through to the real
    /// provider, then patch the cached pages only on success.
    ///
    /// Now that the real provider is held as `Arc<dyn Provider>` and `write`
    /// takes `&self`, the write-through is a direct `real.write(addr, data)`
    /// even while the controller (or a refresh worker) holds another clone of
    /// the same `Arc` — the interior-mutable [`BufferProvider`] mutates through
    /// the shared handle.
    fn write(&self, addr: u64, data: &[u8]) -> bool {
        let Some(real) = &self.real else {
            return false;
        };
        let ok = real.write(addr, data);
        if ok {
            self.patch_pages(addr, data);
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn reads_from_pages_and_zero_fills_holes() {
        let mut pages = PageMap::new();
        pages.insert(0, {
            let mut v = vec![0u8; 4096];
            v[0] = 0xAB;
            v[1] = 0xCD;
            v.into()
        });
        let snap = SnapshotProvider::new(None, pages, 4096);
        let mut buf = [0u8; 2];
        assert!(snap.read(0, &mut buf));
        assert_eq!(buf, [0xAB, 0xCD]);
        // page 0x2000 absent, no real provider → zero-filled
        let mut z = [0xFFu8; 4];
        assert!(snap.read(0x2000, &mut z));
        assert_eq!(z, [0, 0, 0, 0]);
    }

    #[test]
    fn write_through_to_real_and_patches_pages() {
        use crate::provider::BufferProvider;
        // Real provider is a writable BufferProvider held behind a shared Arc.
        let real: Arc<dyn Provider + Send + Sync> =
            Arc::new(BufferProvider::new(vec![0u8; 8], "real"));
        // Snapshot caches page 0 as a copy of the real bytes (all zero).
        let mut pages = PageMap::new();
        pages.insert(0, vec![0u8; 4096].into());
        let snap = SnapshotProvider::new(Some(real.clone()), pages, 8);

        assert!(snap.is_writable());
        assert!(snap.write(2, &[0xAA, 0xBB]));

        // Landed in the REAL provider (write-through), even though `real` Arc is
        // still shared with `snap`.
        assert_eq!(real.read_u8(2), 0xAA);
        assert_eq!(real.read_u8(3), 0xBB);

        // And patched into the snapshot's cached page (compose reflects it).
        let mut buf = [0u8; 2];
        assert!(snap.read(2, &mut buf));
        assert_eq!(buf, [0xAA, 0xBB]);
    }

    #[test]
    fn patch_pages_copies_shared_page_before_mutating() {
        let page = PageBytes::from(vec![0u8; 4096]);
        let previous_read_baseline = page.clone();
        let mut pages = PageMap::new();
        pages.insert(0, page);
        let snap = SnapshotProvider::new(None, pages, 4096);

        snap.patch_pages(0, &[0xCC]);

        assert_eq!(previous_read_baseline[0], 0);
        let mut one = [0u8; 1];
        assert!(snap.read(0, &mut one));
        assert_eq!(one[0], 0xCC);
    }

    #[test]
    fn write_without_real_provider_fails() {
        let snap = SnapshotProvider::new(None, PageMap::new(), 0);
        assert!(!snap.is_writable());
        assert!(!snap.write(0, &[1, 2, 3]));
    }

    struct FailingReadProvider;

    impl Provider for FailingReadProvider {
        fn read(&self, _addr: u64, buf: &mut [u8]) -> bool {
            buf.fill(0xCC);
            false
        }

        fn size(&self) -> i32 {
            0x1000
        }

        fn is_readable(&self, _addr: u64, len: i32) -> bool {
            len >= 0
        }
    }

    #[test]
    fn missing_page_real_read_failure_zero_fills_output() {
        let real: Arc<dyn Provider + Send + Sync> = Arc::new(FailingReadProvider);
        let snap = SnapshotProvider::new(Some(real), PageMap::new(), 0);
        let mut buf = [0xAAu8; 8];

        assert!(snap.read(0x20, &mut buf));
        assert_eq!(buf, [0u8; 8]);
    }

    struct ModuleCountingProvider {
        module_cache: crate::provider::ProviderModuleCache,
        enumerate_calls: AtomicUsize,
    }

    impl Provider for ModuleCountingProvider {
        fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool {
            false
        }

        fn size(&self) -> i32 {
            0x1000
        }

        fn enumerate_modules(&self) -> Vec<ModuleEntry> {
            self.enumerate_calls.fetch_add(1, Ordering::Relaxed);
            vec![ModuleEntry {
                name: "cached.dll".into(),
                full_path: "cached.dll".into(),
                base: 0x1000,
                size: 0x1000,
            }]
        }

        fn module_cache(&self) -> Option<&crate::provider::ProviderModuleCache> {
            Some(&self.module_cache)
        }
    }

    #[test]
    fn module_enumeration_is_cached_for_snapshot_lifetime() {
        let real = Arc::new(ModuleCountingProvider {
            module_cache: crate::provider::ProviderModuleCache::default(),
            enumerate_calls: AtomicUsize::new(0),
        });
        let real_dyn: Arc<dyn Provider + Send + Sync> = real.clone();
        let snap = SnapshotProvider::new(Some(real_dyn), PageMap::new(), 0);

        assert_eq!(snap.enumerate_modules()[0].name, "cached.dll");
        assert_eq!(snap.enumerate_modules()[0].name, "cached.dll");

        let real_dyn: Arc<dyn Provider + Send + Sync> = real.clone();
        let second_snap = SnapshotProvider::new(Some(real_dyn), PageMap::new(), 0);
        assert_eq!(second_snap.enumerate_modules()[0].name, "cached.dll");
        assert_eq!(real.enumerate_calls.load(Ordering::Relaxed), 1);
    }

    struct RegionCountingProvider {
        regions: RwLock<Vec<MemoryRegion>>,
        enumerate_calls: AtomicUsize,
        readable_calls: AtomicUsize,
    }

    impl RegionCountingProvider {
        fn new(regions: Vec<MemoryRegion>) -> Self {
            Self {
                regions: RwLock::new(regions),
                enumerate_calls: AtomicUsize::new(0),
                readable_calls: AtomicUsize::new(0),
            }
        }

        fn set_regions(&self, regions: Vec<MemoryRegion>) {
            *self.regions.write().unwrap() = regions;
        }
    }

    impl Provider for RegionCountingProvider {
        fn read(&self, _addr: u64, buf: &mut [u8]) -> bool {
            buf.fill(0);
            true
        }

        fn size(&self) -> i32 {
            0
        }

        fn enumerate_regions(&self) -> Vec<MemoryRegion> {
            self.enumerate_calls.fetch_add(1, Ordering::Relaxed);
            self.regions.read().unwrap().clone()
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            self.readable_calls.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    fn region(base: u64, size: u64, readable: bool) -> MemoryRegion {
        MemoryRegion {
            base,
            size,
            readable,
            writable: false,
            executable: false,
            module_name: String::new(),
            region_type: super::super::RegionType::Private,
        }
    }

    #[test]
    fn snapshot_readability_uses_cached_real_ranges_and_refreshes_on_merge() {
        let real = Arc::new(RegionCountingProvider::new(vec![
            region(0x4000, 0x1000, true),
            region(0x1000, 0x1000, true),
            region(0x3000, 0x1000, false),
        ]));
        let real_dyn: Arc<dyn Provider + Send + Sync> = real.clone();
        let snap = SnapshotProvider::new(Some(real_dyn), PageMap::new(), 0);

        assert_eq!(real.enumerate_calls.load(Ordering::Relaxed), 0);
        assert!(snap.is_readable(0x1000, 8));
        assert!(snap.is_readable(0x4000, 8));
        assert!(!snap.is_readable(0x3000, 8));
        assert_eq!(real.enumerate_calls.load(Ordering::Relaxed), 1);
        assert_eq!(real.readable_calls.load(Ordering::Relaxed), 0);

        real.set_regions(vec![region(0x8000, 0x1000, true)]);
        snap.merge_pages(&PageMap::new(), 0);

        assert!(!snap.is_readable(0x1000, 8));
        assert!(snap.is_readable(0x8000, 8));
        assert_eq!(real.enumerate_calls.load(Ordering::Relaxed), 2);
        assert_eq!(real.readable_calls.load(Ordering::Relaxed), 0);
    }
}
