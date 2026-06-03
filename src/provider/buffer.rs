//! `BufferProvider` — flat in-memory byte buffer source (also the "File"
//! source loaded into RAM).
//!
//! Faithful port of `src/providers/buffer_provider.h`. Trivial and benign:
//! bounds-checked `memcpy` over an owned `Vec<u8>`. `is_live()` is false, so
//! auto-refresh does NOT tick for file/buffer sources.

use std::fs;
use std::sync::RwLock;

use super::{MemoryRegion, Provider, RegionType};

/// `class BufferProvider : public Provider` (`buffer_provider.h:8-63`).
///
/// `m_data` is held behind a [`RwLock`] so [`write`](Provider::write) can take
/// `&self` and mutate through a shared `Arc<dyn Provider>` (PORTING_providers
/// §5). Reads take the read lock; writes/`data_mut` take the write lock. The
/// fixed-size buffer is never resized through `write` (past-end writes fail,
/// mirroring the C++ `BufferProvider::write` bounds check).
#[derive(Debug, Default)]
pub struct BufferProvider {
    data: RwLock<Vec<u8>>,
    name: String,
}

impl Clone for BufferProvider {
    fn clone(&self) -> Self {
        BufferProvider {
            data: RwLock::new(self.data.read().unwrap().clone()),
            name: self.name.clone(),
        }
    }
}

impl BufferProvider {
    /// `BufferProvider(data, name)` (`buffer_provider.h:13-15`).
    pub fn new(data: Vec<u8>, name: impl Into<String>) -> Self {
        BufferProvider {
            data: RwLock::new(data),
            name: name.into(),
        }
    }

    /// `BufferProvider::fromFile(path)` (`buffer_provider.h:17-22`) — reads the
    /// whole file into RAM; an empty buffer on failure.
    pub fn from_file(path: &str) -> Self {
        match fs::read(path) {
            Ok(data) => {
                let file_name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                BufferProvider::new(data, file_name)
            }
            Err(_) => BufferProvider::default(),
        }
    }

    /// `const QByteArray& data() const` (`buffer_provider.h:61`) — a cloned
    /// snapshot of the buffer (the `RwLock` precludes returning a borrow).
    pub fn data(&self) -> Vec<u8> {
        self.data.read().unwrap().clone()
    }
    /// `QByteArray& data()` (`buffer_provider.h:62`) — the write-locked buffer
    /// guard for in-place mutation.
    pub fn data_mut(&self) -> std::sync::RwLockWriteGuard<'_, Vec<u8>> {
        self.data.write().unwrap()
    }
}

impl Provider for BufferProvider {
    fn size(&self) -> i32 {
        self.data.read().unwrap().len() as i32
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let data = self.data.read().unwrap();
        if !is_readable_len(data.len(), addr, buf.len() as i32) {
            return false;
        }
        let start = addr as usize;
        buf.copy_from_slice(&data[start..start + buf.len()]);
        true
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn write(&self, addr: u64, data: &[u8]) -> bool {
        let mut buf = self.data.write().unwrap();
        if !is_readable_len(buf.len(), addr, data.len() as i32) {
            return false;
        }
        let start = addr as usize;
        buf[start..start + data.len()].copy_from_slice(data);
        true
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> String {
        "File".to_string()
    }

    /// `enumerateRegions()` (`buffer_provider.h:48-59`) — one synthetic
    /// `Mapped` region named after the file (or "[buffer]").
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        let len = self.data.read().unwrap().len();
        if len == 0 {
            return Vec::new();
        }
        vec![MemoryRegion {
            base: 0,
            size: len as u64,
            readable: true,
            writable: true,
            executable: false,
            module_name: if self.name.is_empty() {
                "[buffer]".to_string()
            } else {
                self.name.clone()
            },
            region_type: RegionType::Mapped,
        }]
    }
}

/// Underflow-safe bounds check against a buffer of `size` bytes — the
/// `Provider::is_readable` default specialised to a known length (so `read`/
/// `write` can hold the lock and check in one shot).
fn is_readable_len(size: usize, addr: u64, len: i32) -> bool {
    if len <= 0 {
        return len == 0;
    }
    let size = size as u64;
    addr <= size && (len as u64) <= size - addr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_bounds() {
        // `write` takes `&self` now (interior mutability) — no `mut` needed.
        let p = BufferProvider::new(vec![1, 2, 3, 4], "x.bin");
        let mut buf = [0u8; 2];
        assert!(p.read(1, &mut buf));
        assert_eq!(buf, [2, 3]);
        assert!(!p.read(3, &mut [0u8; 2])); // out of range
        assert!(p.write(0, &[9, 8]));
        assert_eq!(p.data(), vec![9, 8, 3, 4]);
        assert_eq!(p.enumerate_regions().len(), 1);
        assert_eq!(p.read_u16(0), 0x0809);
    }

    /// `buffer_write_pastEndFails` — a write that runs past the fixed-size
    /// buffer fails and mutates nothing (C++ `BufferProvider::write` bounds
    /// check; PORTING_providers §3).
    #[test]
    fn write_past_end_fails() {
        let p = BufferProvider::new(vec![1, 2, 3, 4], "x.bin");
        assert!(!p.write(3, &[9, 8])); // would touch index 4 — out of range
        assert_eq!(p.data(), vec![1, 2, 3, 4]); // unchanged
    }

    /// `write(&self)` mutates through a shared `Arc` even when a clone is held
    /// elsewhere (the snapshot/worker-clone scenario the controller relies on).
    #[test]
    fn write_through_shared_arc() {
        use std::sync::Arc;
        let p: Arc<dyn Provider + Send + Sync> =
            Arc::new(BufferProvider::new(vec![0, 0, 0, 0], "x.bin"));
        let _clone = p.clone(); // a second owner keeps the Arc shared
        assert!(p.is_writable());
        assert!(p.write(1, &[0xAB, 0xCD]));
        assert_eq!(p.read_u8(1), 0xAB);
        assert_eq!(p.read_u8(2), 0xCD);
    }
}
