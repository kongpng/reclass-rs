//! `SnapshotProvider` — a page-table-backed provider the controller builds from
//! async reads, so compose never blocks on I/O.
//!
//! Faithful port of `src/providers/snapshot_provider.h`. Holds a page map
//! (page-aligned addr → 4096-byte page) plus an optional real provider for
//! write-through and read fall-through (required by the RTTI auto-hint reading
//! vtable/.rdata bytes that weren't pre-fetched). Pages never fetched read as
//! zeros.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock, RwLockReadGuard};

use super::{MemoryRegion, ModuleEntry, Provider, ThreadInfo};

/// `SnapshotProvider::kPageSize` (`snapshot_provider.h:29`).
pub const K_PAGE_SIZE: u64 = 4096;
const K_PAGE_MASK: u64 = !(K_PAGE_SIZE - 1);

/// `using PageMap = QHash<uint64_t, QByteArray>` (`snapshot_provider.h:33`).
pub type PageMap = HashMap<u64, Vec<u8>>;

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
        }
    }

    /// `updatePages(pages, mainExtent)` (`snapshot_provider.h:130-133`).
    pub fn update_pages(&self, pages: PageMap, main_extent: i32) {
        let mut inner = self.inner.write().unwrap();
        inner.pages = pages;
        inner.main_extent = main_extent;
    }

    /// `mergePages(fresh, mainExtent)` (`snapshot_provider.h:139-143`) — fresh
    /// pages overwrite, absent pages keep their old bytes.
    pub fn merge_pages(&self, fresh: &PageMap, main_extent: i32) {
        let mut inner = self.inner.write().unwrap();
        for (k, v) in fresh {
            inner.pages.insert(*k, v.clone());
        }
        inner.main_extent = main_extent;
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
}

impl Provider for SnapshotProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if buf.is_empty() {
            return false;
        }
        let inner = self.inner.read().unwrap();
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
                let mut tmp = vec![0u8; chunk];
                if !real.read(cur, &mut tmp) {
                    tmp.iter_mut().for_each(|b| *b = 0);
                }
                buf[off..off + chunk].copy_from_slice(&tmp);
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
        let inner = self.inner.read().unwrap();
        let mut p = addr & K_PAGE_MASK;
        while p < end {
            if !inner.pages.contains_key(&p) {
                if let Some(real) = &self.real {
                    if real.is_readable(addr, len) {
                        return true;
                    }
                }
                return false;
            }
            p += K_PAGE_SIZE;
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
        self.real
            .as_ref()
            .map_or_else(Vec::new, |r| r.enumerate_modules())
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

    #[test]
    fn reads_from_pages_and_zero_fills_holes() {
        let mut pages = PageMap::new();
        pages.insert(0, {
            let mut v = vec![0u8; 4096];
            v[0] = 0xAB;
            v[1] = 0xCD;
            v
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
        pages.insert(0, vec![0u8; 4096]);
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
    fn write_without_real_provider_fails() {
        let snap = SnapshotProvider::new(None, PageMap::new(), 0);
        assert!(!snap.is_writable());
        assert!(!snap.write(0, &[1, 2, 3]));
    }
}
