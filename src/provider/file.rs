//! `FileProvider` — a memory-mapped binary-file data source.
//!
//! Maps the original Reclass "File" source (`buffer_provider.h::fromFile`) but
//! backs reads with an `mmap` (`memmap2`) instead of slurping the whole file
//! into RAM — the cross-platform equivalent of the C++ `MappedFile`
//! (`mmap`/`MapViewOfFile`) used in `import_pdb.cpp` (crate_selection.md). Reads
//! are bounds-checked slice copies; writes are unsupported (read-only mmap).

use std::fs::File;
use std::path::Path;

use memmap2::Mmap;

use super::{MemoryRegion, Provider, RegionType};

/// A read-only memory-mapped file source.
pub struct FileProvider {
    map: Mmap,
    name: String,
}

impl FileProvider {
    /// Open `path` and mmap it read-only.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        // SAFETY: the file is opened read-only; the map is not mutated. As with
        // any mmap, external truncation is UB — acceptable for a debugger tool
        // inspecting a static dump (matches the C++ MappedFile contract).
        let map = unsafe { Mmap::map(&file)? };
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        Ok(FileProvider { map, name })
    }
}

impl Provider for FileProvider {
    fn size(&self) -> i32 {
        self.map.len().min(i32::MAX as usize) as i32
    }

    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        if !self.is_readable(addr, buf.len() as i32) {
            return false;
        }
        let start = addr as usize;
        buf.copy_from_slice(&self.map[start..start + buf.len()]);
        true
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn kind(&self) -> String {
        "File".to_string()
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        if self.map.is_empty() {
            return Vec::new();
        }
        vec![MemoryRegion {
            base: 0,
            size: self.map.len() as u64,
            readable: true,
            writable: false,
            executable: false,
            module_name: if self.name.is_empty() {
                "[file]".to_string()
            } else {
                self.name.clone()
            },
            region_type: RegionType::Mapped,
        }]
    }
}
