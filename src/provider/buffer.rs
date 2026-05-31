//! `BufferProvider` — flat in-memory byte buffer source (also the "File"
//! source loaded into RAM).
//!
//! Faithful port of `src/providers/buffer_provider.h`. Trivial and benign:
//! bounds-checked `memcpy` over an owned `Vec<u8>`. `is_live()` is false, so
//! auto-refresh does NOT tick for file/buffer sources.

use std::fs;

use super::{MemoryRegion, Provider, RegionType};

/// `class BufferProvider : public Provider` (`buffer_provider.h:8-63`).
#[derive(Clone, Debug, Default)]
pub struct BufferProvider {
    data: Vec<u8>,
    name: String,
}

impl BufferProvider {
    /// `BufferProvider(data, name)` (`buffer_provider.h:13-15`).
    pub fn new(data: Vec<u8>, name: impl Into<String>) -> Self {
        BufferProvider {
            data,
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

    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }
}

impl Provider for BufferProvider {
    fn size(&self) -> i32 {
        self.data.len() as i32
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if !self.is_readable(addr, buf.len() as i32) {
            return false;
        }
        let start = addr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        true
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn write(&mut self, addr: u64, data: &[u8]) -> bool {
        if !self.is_readable(addr, data.len() as i32) {
            return false;
        }
        let start = addr as usize;
        self.data[start..start + data.len()].copy_from_slice(data);
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
        if self.data.is_empty() {
            return Vec::new();
        }
        vec![MemoryRegion {
            base: 0,
            size: self.data.len() as u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_bounds() {
        let mut p = BufferProvider::new(vec![1, 2, 3, 4], "x.bin");
        let mut buf = [0u8; 2];
        assert!(p.read(1, &mut buf));
        assert_eq!(buf, [2, 3]);
        assert!(!p.read(3, &mut [0u8; 2])); // out of range
        assert!(p.write(0, &[9, 8]));
        assert_eq!(p.data(), &[9, 8, 3, 4]);
        assert_eq!(p.enumerate_regions().len(), 1);
        assert_eq!(p.read_u16(0), 0x0809);
    }
}
