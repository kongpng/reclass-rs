use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use gpui::SharedString;
use reclass::compose;
use reclass::controller::{PageMap, RcxController, RcxDocument, RefreshPlan};
use reclass::core::{
    infer_strong_types, infer_types, InferHints, LineKind, LineMeta, Node, NodeKind, NodeTree,
};
use reclass::format;
#[cfg(feature = "process-provider")]
use reclass::provider::bench_readable_ranges_from_regions;
#[cfg(all(target_os = "linux", feature = "process-provider"))]
use reclass::provider::LocalProcessProvider;
use reclass::provider::{
    BufferProvider, CachedPageProvider, MemoryRegion, ModuleEntry, ModuleLookup, Provider,
    RegionType, SnapshotProvider, K_PAGE_SIZE,
};
use reclass::scanner::pointer::{
    find_pointer_chains, PointerChainRequest, PointerMap, PointerMapStats, PointerRecord,
};
use reclass::scanner::{
    run_rescan, run_scan, AddressRange, NullObserver, ScanCondition, ScanRequest, ScanResult,
    ValueType,
};
use reclass::ui::chrome::statusbar::StatusInfo;
use reclass::ui::editor::palette::EditorPalette;
use reclass::ui::overlays::findbar::{FindMatch, FindState};
use reclass::ui::panels::modulespanel::build_module_rows;
use reclass::ui::panels::scannerpanel::{
    apply_change_all_results, bench_scanner_table_filter_cached, bench_scanner_table_refresh,
};
use reclass::ui::panels::workspace::{WorkspaceDoc, WorkspaceModel};
use reclass::ui::state::DocId;
use std::hint::black_box;

fn flat_tree(nodes: usize, kind: NodeKind) -> NodeTree {
    let mut tree = NodeTree::new();
    let root = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".to_string(),
        struct_type_name: "Root".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[root].id;
    let step = reclass::core::size_for_kind(kind).max(1);
    for i in 0..nodes {
        tree.add_node(Node {
            kind,
            name: format!("field_{i:05}"),
            parent_id: root_id,
            offset: (i as i32).saturating_mul(step),
            ..Node::default()
        });
    }
    tree
}

fn flat_tree_at_base(nodes: usize, kind: NodeKind, base_address: u64) -> NodeTree {
    let mut tree = flat_tree(nodes, kind);
    tree.base_address = base_address;
    tree
}

fn deep_tree(nodes: usize) -> NodeTree {
    let mut tree = NodeTree::new();
    let root = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".to_string(),
        struct_type_name: "Root".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let mut parent_id = tree.nodes[root].id;
    for i in 0..nodes {
        let idx = tree.add_node(Node {
            kind: if i + 1 == nodes {
                NodeKind::Hex64
            } else {
                NodeKind::Struct
            },
            name: format!("nested_{i:05}"),
            struct_type_name: if i + 1 == nodes {
                String::new()
            } else {
                format!("Nested{i}")
            },
            parent_id,
            offset: 8,
            collapsed: false,
            ..Node::default()
        });
        parent_id = tree.nodes[idx].id;
    }
    tree
}

fn enum_ref_tree(nodes: usize, members: usize) -> (NodeTree, u64) {
    let mut tree = NodeTree::new();
    let enum_idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        class_keyword: "enum".to_string(),
        name: "State".to_string(),
        struct_type_name: "State".to_string(),
        enum_members: (0..members)
            .map(|i| (format!("STATE_{i:04}"), i as i64))
            .collect(),
        ..Node::default()
    });
    let enum_id = tree.nodes[enum_idx].id;
    let root = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".to_string(),
        struct_type_name: "Root".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[root].id;
    for i in 0..nodes {
        tree.add_node(Node {
            kind: NodeKind::UInt32,
            name: format!("state_{i:05}"),
            parent_id: root_id,
            ref_id: enum_id,
            offset: (i as i32).saturating_mul(4),
            ..Node::default()
        });
    }
    (tree, root_id)
}

fn struct_array_ref_tree(elements: usize, fields_per_struct: usize) -> NodeTree {
    let mut tree = NodeTree::new();
    let struct_idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Element".to_string(),
        struct_type_name: "Element".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let struct_id = tree.nodes[struct_idx].id;
    for i in 0..fields_per_struct {
        tree.add_node(Node {
            kind: NodeKind::Hex64,
            name: format!("slot_{i:03}"),
            parent_id: struct_id,
            offset: (i as i32).saturating_mul(8),
            ..Node::default()
        });
    }

    let root_idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".to_string(),
        struct_type_name: "Root".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[root_idx].id;
    tree.add_node(Node {
        kind: NodeKind::Array,
        name: "items".to_string(),
        parent_id: root_id,
        offset: 0,
        array_len: elements as i32,
        element_kind: NodeKind::Struct,
        ref_id: struct_id,
        collapsed: false,
        ..Node::default()
    });
    tree
}

fn pointer_ref_tree(nodes: usize) -> NodeTree {
    let mut tree = NodeTree::new();
    let target_idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Target".to_string(),
        struct_type_name: "Target".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let target_id = tree.nodes[target_idx].id;
    tree.add_node(Node {
        kind: NodeKind::Hex64,
        name: "value".to_string(),
        parent_id: target_id,
        offset: 0,
        ..Node::default()
    });

    let root_idx = tree.add_node(Node {
        kind: NodeKind::Struct,
        name: "Root".to_string(),
        struct_type_name: "Root".to_string(),
        collapsed: false,
        ..Node::default()
    });
    let root_id = tree.nodes[root_idx].id;
    for i in 0..nodes {
        tree.add_node(Node {
            kind: NodeKind::Pointer64,
            name: format!("ptr_{i:05}"),
            parent_id: root_id,
            offset: (i as i32).saturating_mul(8),
            ref_id: target_id,
            ptr_depth: 0,
            collapsed: true,
            ..Node::default()
        });
    }
    tree
}

fn patterned_provider(bytes: usize) -> BufferProvider {
    let mut data = vec![0u8; bytes];
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i.wrapping_mul(37) ^ (i >> 3)) & 0xff) as u8;
    }
    BufferProvider::new(data, "bench.bin")
}

#[derive(Clone)]
struct LiveLikeProvider {
    base: u64,
    data: Vec<u8>,
    regions: Vec<MemoryRegion>,
    modules: Vec<ModuleEntry>,
}

impl LiveLikeProvider {
    fn with_dummy_modules(mut self, count: usize) -> Self {
        self.modules = (0..count)
            .map(|i| ModuleEntry {
                name: format!("module_{i}.dll"),
                full_path: format!(r"C:\dummy\module_{i}.dll"),
                base: 0xFFFF_0000_0000_0000u64.saturating_add((i as u64) << 20),
                size: 0x10000,
            })
            .collect();
        self
    }

    fn new_float_pairs(nodes: usize) -> Self {
        let mut data = vec![0u8; (nodes + 8) * 8];
        for i in 0..nodes {
            let a = 1.25f32 + (i as f32 % 97.0);
            let b = 2.5f32 + (i as f32 % 53.0);
            let off = i * 8;
            data[off..off + 4].copy_from_slice(&a.to_le_bytes());
            data[off + 4..off + 8].copy_from_slice(&b.to_le_bytes());
        }

        // Shape this like a live process: many readable regions, so a per-row
        // `is_readable` implementation has to enumerate and scan mappings.
        let mut regions = Vec::new();
        let mut base = 0u64;
        let chunk = 0x1000u64;
        while (base as usize) < data.len() {
            regions.push(MemoryRegion {
                base,
                size: chunk.min((data.len() as u64).saturating_sub(base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            base = base.saturating_add(chunk);
        }

        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_vec4_rows(nodes: usize) -> Self {
        let mut data = vec![0u8; (nodes + 8) * 16];
        for i in 0..nodes {
            let off = i * 16;
            for lane in 0..4 {
                let v = (lane as f32 + 1.0) * 0.5 + (i as f32 % 31.0);
                data[off + lane * 4..off + lane * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }

        let mut regions = Vec::new();
        let mut base = 0u64;
        let chunk = 0x1000u64;
        while (base as usize) < data.len() {
            regions.push(MemoryRegion {
                base,
                size: chunk.min((data.len() as u64).saturating_sub(base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            base = base.saturating_add(chunk);
        }

        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_repeated_pattern(bytes: usize, pattern: &[u8]) -> Self {
        let mut data = vec![0u8; bytes];
        for chunk in data.chunks_mut(pattern.len().max(1)) {
            let n = chunk.len().min(pattern.len());
            chunk[..n].copy_from_slice(&pattern[..n]);
        }

        let mut regions = Vec::new();
        let mut base = 0u64;
        let chunk = 0x1000u64;
        while (base as usize) < data.len() {
            regions.push(MemoryRegion {
                base,
                size: chunk.min((data.len() as u64).saturating_sub(base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            base = base.saturating_add(chunk);
        }

        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_masked_signature_hits(bytes: usize, stride: usize) -> Self {
        let mut data = vec![0u8; bytes];
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(37) ^ (i >> 3)) & 0xff) as u8;
        }
        let sig_len = 8usize;
        for pos in (0..bytes.saturating_sub(sig_len)).step_by(stride.max(sig_len)) {
            data[pos] = 0x48;
            data[pos + 1] = 0x8B;
            data[pos + 2] = (pos >> 4) as u8;
            data[pos + 3] = 0x05;
            data[pos + 4] = (pos >> 8) as u8;
            data[pos + 5] = (pos >> 16) as u8;
            data[pos + 6] = 0x89;
            data[pos + 7] = 0xD8;
        }

        let mut regions = Vec::new();
        let mut base = 0u64;
        let chunk = 0x1000u64;
        while (base as usize) < data.len() {
            regions.push(MemoryRegion {
                base,
                size: chunk.min((data.len() as u64).saturating_sub(base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            base = base.saturating_add(chunk);
        }

        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_pointer_chains(nodes: usize) -> Self {
        let second_base = 0x20_000usize;
        let value_base = 0x40_000usize;
        let len = value_base + nodes * 4 + 8;
        let mut data = vec![0u8; len];
        for i in 0..nodes {
            let first_off = i * 8;
            let second_addr = (second_base + i * 8) as u64;
            let value_addr = (value_base + i * 4) as u64;
            data[first_off..first_off + 8].copy_from_slice(&second_addr.to_le_bytes());
            data[second_base + i * 8..second_base + i * 8 + 8]
                .copy_from_slice(&value_addr.to_le_bytes());
            data[value_base + i * 4..value_base + i * 4 + 4]
                .copy_from_slice(&(i as i32).to_le_bytes());
        }

        let mut regions = Vec::new();
        let mut base = 0u64;
        let chunk = 0x1000u64;
        while (base as usize) < data.len() {
            regions.push(MemoryRegion {
                base,
                size: chunk.min((data.len() as u64).saturating_sub(base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            base = base.saturating_add(chunk);
        }

        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_pointer_targets(nodes: usize) -> Self {
        let provider_base = 0x0000_7FF6_A0B0_0000u64;
        let target_base = 0x40_000usize;
        let len = target_base + nodes + 8;
        let mut data = vec![0u8; len];
        for i in 0..nodes {
            let target = provider_base + (target_base + i) as u64;
            data[i * 8..i * 8 + 8].copy_from_slice(&target.to_le_bytes());
            data[target_base + i] = 0xAA;
        }

        let mut regions = Vec::new();
        let mut region_base = 0u64;
        let chunk = 0x1000u64;
        while (region_base as usize) < data.len() {
            regions.push(MemoryRegion {
                base: provider_base + region_base,
                size: chunk.min((data.len() as u64).saturating_sub(region_base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            region_base = region_base.saturating_add(chunk);
        }

        Self {
            base: provider_base,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_rtti_candidates(nodes: usize) -> Self {
        let provider_base = 0x0000_7FF6_A0B0_0000u64;
        let candidate_base = 0x20_000usize;
        let col_base = 0x40_000usize;
        let len = (candidate_base + nodes * 0x20 + 0x100).max(col_base + nodes * 0x10 + 0x100);
        let mut data = vec![0u8; len];
        for i in 0..nodes {
            let candidate_off = candidate_base + i * 0x20;
            let col_off = col_base + i * 0x10;
            let candidate = provider_base + candidate_off as u64;
            let col = provider_base + col_off as u64;
            data[i * 8..i * 8 + 8].copy_from_slice(&candidate.to_le_bytes());
            data[candidate_off - 8..candidate_off].copy_from_slice(&col.to_le_bytes());
            data[col_off..col_off + 4].copy_from_slice(&0xDEADu32.to_le_bytes());
        }

        let mut regions = Vec::new();
        let mut region_base = 0u64;
        let chunk = 0x1000u64;
        while (region_base as usize) < data.len() {
            regions.push(MemoryRegion {
                base: provider_base + region_base,
                size: chunk.min((data.len() as u64).saturating_sub(region_base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: "synthetic.dll".into(),
                region_type: RegionType::Image,
            });
            region_base = region_base.saturating_add(chunk);
        }
        let modules = vec![ModuleEntry {
            name: "synthetic.dll".into(),
            full_path: "synthetic.dll".into(),
            base: provider_base,
            size: data.len() as u64,
        }];

        Self {
            base: provider_base,
            data,
            regions,
            modules,
        }
    }

    fn new_module_pages(pages: usize) -> Self {
        let len = pages * 4096;
        let data = vec![0u8; len];
        let regions = (0..pages)
            .map(|i| MemoryRegion {
                base: (i * 4096) as u64,
                size: 4096,
                readable: true,
                writable: false,
                executable: true,
                module_name: format!("module_{i:05}.dll"),
                region_type: RegionType::Image,
            })
            .collect();
        Self {
            base: 0,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_memory_preview_targets(rows: usize) -> (Self, Vec<u8>) {
        let page_count = rows.max(1);
        let len = page_count * 4096;
        let mut data = vec![0u8; len];
        let mut preview = Vec::with_capacity(rows * 8);
        let mut regions = Vec::with_capacity(page_count);
        let mut modules = Vec::with_capacity(page_count);
        for i in 0..page_count {
            let base = (i * 4096) as u64;
            data[i * 4096] = (i as u8).wrapping_add(1);
            preview.extend_from_slice(&base.to_le_bytes());
            let name = format!("mod_{i:05}.dll");
            regions.push(MemoryRegion {
                base,
                size: 4096,
                readable: true,
                writable: false,
                executable: true,
                module_name: name.clone(),
                region_type: RegionType::Image,
            });
            modules.push(ModuleEntry {
                name: name.clone(),
                full_path: name,
                base,
                size: 4096,
            });
        }
        (
            Self {
                base: 0,
                data,
                regions,
                modules,
            },
            preview,
        )
    }

    fn new_compose_pointer_hint_regions(rows: usize) -> Self {
        let provider_base = 0x0000_7FF6_B000_0000u64;
        let pointer_table_len = rows.saturating_mul(8);
        let target_base = ((pointer_table_len + 4095) / 4096) * 4096;
        let len = target_base + rows.max(1) * 4096;
        let mut data = vec![0u8; len];
        let mut regions = Vec::with_capacity(rows.saturating_add(1));
        let mut modules = Vec::with_capacity(rows);

        regions.push(MemoryRegion {
            base: provider_base,
            size: target_base as u64,
            readable: true,
            writable: true,
            executable: false,
            module_name: String::new(),
            region_type: RegionType::Private,
        });

        for i in 0..rows {
            let target = target_base + i * 4096;
            let target_addr = provider_base + target as u64;
            data[i * 8..i * 8 + 8].copy_from_slice(&target_addr.to_le_bytes());
            data[target] = (i as u8).wrapping_add(1);
            let name = format!("hint_{i:05}.dll");
            regions.push(MemoryRegion {
                base: target_addr,
                size: 4096,
                readable: true,
                writable: false,
                executable: true,
                module_name: name.clone(),
                region_type: RegionType::Image,
            });
            modules.push(ModuleEntry {
                name: name.clone(),
                full_path: name,
                base: target_addr,
                size: 4096,
            });
        }

        Self {
            base: provider_base,
            data,
            regions,
            modules,
        }
    }

    fn new_rescan_i32_rows(rows: usize) -> Self {
        let provider_base = 0x0000_7FF6_C000_0000u64;
        let len = rows.saturating_mul(4).max(4);
        let mut data = vec![0u8; len];
        for i in 0..rows {
            let value = if i % 2 == 0 {
                0x1234_5678u32
            } else {
                0x9ABC_DEF0u32
            };
            data[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }

        let mut regions = Vec::new();
        let mut region_base = 0u64;
        let chunk = 0x1000u64;
        while (region_base as usize) < data.len() {
            regions.push(MemoryRegion {
                base: provider_base + region_base,
                size: chunk.min((data.len() as u64).saturating_sub(region_base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            region_base = region_base.saturating_add(chunk);
        }

        Self {
            base: provider_base,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn new_rescan_i32_pages(rows: usize) -> Self {
        let provider_base = 0x0000_7FF6_D000_0000u64;
        let page = 4096usize;
        let len = rows.saturating_mul(page).max(page);
        let mut data = vec![0u8; len];
        for i in 0..rows {
            let value = if i % 2 == 0 {
                0x1234_5678u32
            } else {
                0x9ABC_DEF0u32
            };
            data[i * page..i * page + 4].copy_from_slice(&value.to_le_bytes());
        }

        let mut regions = Vec::new();
        let mut region_base = 0u64;
        let chunk = 0x1000u64;
        while (region_base as usize) < data.len() {
            regions.push(MemoryRegion {
                base: provider_base + region_base,
                size: chunk.min((data.len() as u64).saturating_sub(region_base)),
                readable: true,
                writable: true,
                executable: false,
                module_name: String::new(),
                region_type: RegionType::Private,
            });
            region_base = region_base.saturating_add(chunk);
        }

        Self {
            base: provider_base,
            data,
            regions,
            modules: Vec::new(),
        }
    }

    fn module_for_addr(&self, addr: u64) -> Option<&ModuleEntry> {
        if self.modules.len() > 4 {
            let idx = self.modules.partition_point(|module| module.base <= addr);
            return idx
                .checked_sub(1)
                .and_then(|i| self.modules.get(i))
                .filter(|module| {
                    addr >= module.base
                        && addr
                            .checked_sub(module.base)
                            .is_some_and(|rel| rel < module.size)
                });
        }
        self.modules.iter().find(|module| {
            addr >= module.base
                && addr
                    .checked_sub(module.base)
                    .is_some_and(|rel| rel < module.size)
        })
    }
}

impl Provider for LiveLikeProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let Some(start) = addr.checked_sub(self.base).map(|off| off as usize) else {
            return false;
        };
        if start.saturating_add(buf.len()) > self.data.len() {
            return false;
        }
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        true
    }

    fn size(&self) -> i32 {
        self.data.len() as i32
    }

    fn base(&self) -> u64 {
        self.base
    }

    fn kind(&self) -> String {
        "LiveLikeProcess".to_string()
    }

    fn is_live(&self) -> bool {
        true
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.regions.clone()
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        true
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.modules.clone()
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        if len <= 0 {
            return len == 0;
        }
        self.enumerate_regions().into_iter().any(|region| {
            region.readable
                && addr >= region.base
                && addr
                    .checked_add(len as u64)
                    .is_some_and(|end| end <= region.base.saturating_add(region.size))
        })
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.module_for_addr(addr)
            .map(|module| format!("{}+0x{:x}", module.name, addr.saturating_sub(module.base)))
            .unwrap_or_default()
    }
}

struct CountingProvider {
    inner: LiveLikeProvider,
    reads: AtomicUsize,
    bytes: AtomicUsize,
}

impl CountingProvider {
    fn new(inner: LiveLikeProvider) -> Self {
        Self {
            inner,
            reads: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }

    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

impl Provider for CountingProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        self.inner.read(addr, buf)
    }

    fn size(&self) -> i32 {
        self.inner.size()
    }

    fn base(&self) -> u64 {
        self.inner.base()
    }

    fn kind(&self) -> String {
        self.inner.kind()
    }

    fn is_live(&self) -> bool {
        self.inner.is_live()
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions()
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        self.inner.is_readable(addr, len)
    }
}

struct SymbolWorkProvider {
    inner: LiveLikeProvider,
    symbol_calls: AtomicUsize,
    symbol_work: usize,
}

impl SymbolWorkProvider {
    fn new(inner: LiveLikeProvider, symbol_work: usize) -> Self {
        Self {
            inner,
            symbol_calls: AtomicUsize::new(0),
            symbol_work,
        }
    }

    fn symbol_calls(&self) -> usize {
        self.symbol_calls.load(Ordering::Relaxed)
    }
}

impl Provider for SymbolWorkProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }

    fn read_pages(&self, pages: &[u64]) -> PageMap {
        self.inner.read_pages(pages)
    }

    fn size(&self) -> i32 {
        self.inner.size()
    }

    fn kind(&self) -> String {
        self.inner.kind()
    }

    fn is_live(&self) -> bool {
        self.inner.is_live()
    }

    fn pointer_size(&self) -> i32 {
        self.inner.pointer_size()
    }

    fn base(&self) -> u64 {
        self.inner.base()
    }

    fn get_symbol(&self, addr: u64) -> String {
        self.symbol_calls.fetch_add(1, Ordering::Relaxed);
        let mut acc = addr;
        for i in 0..self.symbol_work {
            acc = acc
                .rotate_left(7)
                .wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        }
        black_box(acc);
        self.inner.get_symbol(addr)
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.inner.enumerate_regions()
    }

    fn trusts_enumerated_region_readability(&self) -> bool {
        self.inner.trusts_enumerated_region_readability()
    }

    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        self.inner.enumerate_modules()
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        self.inner.is_readable(addr, len)
    }
}

struct NoCoalescedRescanProvider<'a> {
    inner: &'a dyn Provider,
}

impl Provider for NoCoalescedRescanProvider<'_> {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }

    fn size(&self) -> i32 {
        self.inner.size()
    }
}

struct TailLimitedProvider {
    data: Vec<u8>,
}

impl TailLimitedProvider {
    fn new(pointer_addr: u64, target_addr: u64, target_len: usize) -> Self {
        let end = target_addr as usize + target_len;
        let mut data = vec![0u8; end.max(pointer_addr as usize + 8)];
        data[pointer_addr as usize..pointer_addr as usize + 8]
            .copy_from_slice(&target_addr.to_le_bytes());
        for (i, b) in data[target_addr as usize..end].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17).wrapping_add(3);
        }
        Self { data }
    }
}

impl Provider for TailLimitedProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
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

    fn kind(&self) -> String {
        "TailLimitedLiveLikeProcess".to_string()
    }

    fn is_live(&self) -> bool {
        true
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        Vec::new()
    }
}

fn rtti_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("rtti_autodetect");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_rtti_candidates(nodes);
        let mut tree = flat_tree(nodes, NodeKind::Hex64);
        tree.base_address = provider.base();
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("unique_invalid_candidates_live_like", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        false,
                        true,
                        true,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    group.finish();
}

fn compose_live_read_cache_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("compose_live_read_cache");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("direct_live_reads", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || CountingProvider::new(LiveLikeProvider::new_float_pairs(nodes + 8)),
                    |provider| {
                        let result = compose::compose(
                            black_box(&tree),
                            black_box(&provider),
                            0,
                            false,
                            false,
                            false,
                            false,
                            true,
                            true,
                            true,
                        );
                        black_box((result.meta.len(), provider.reads(), provider.bytes()));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("cached_page_reads", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let real = Arc::new(CountingProvider::new(
                            LiveLikeProvider::new_float_pairs(nodes + 8),
                        ));
                        let cache = CachedPageProvider::new(real.clone());
                        (real, cache)
                    },
                    |(real, cache)| {
                        let result = compose::compose(
                            black_box(&tree),
                            black_box(&cache),
                            0,
                            false,
                            false,
                            false,
                            false,
                            true,
                            true,
                            true,
                        );
                        black_box((result.meta.len(), real.reads(), real.bytes()));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let provider: Arc<dyn Provider + Send + Sync> = Arc::new(provider);
            let sample_addr = provider.base();
            group.bench_function("direct_current_process_map_snapshots", |b| {
                b.iter(|| {
                    let mut total = 0usize;
                    for _ in 0..16 {
                        total = total.wrapping_add(provider.enumerate_regions().len());
                        total = total.wrapping_add(provider.enumerate_modules().len());
                        total = total.wrapping_add(usize::from(
                            provider.is_readable(black_box(sample_addr), 8),
                        ));
                    }
                    black_box(total);
                });
            });
            group.bench_function("cached_current_process_map_snapshots", |b| {
                b.iter_batched(
                    || CachedPageProvider::new(Arc::clone(&provider)),
                    |cache| {
                        let mut total = 0usize;
                        for _ in 0..16 {
                            total = total.wrapping_add(cache.enumerate_regions().len());
                            total = total.wrapping_add(cache.enumerate_modules().len());
                            total = total.wrapping_add(usize::from(
                                cache.is_readable(black_box(sample_addr), 8),
                            ));
                        }
                        black_box(total);
                    },
                    BatchSize::SmallInput,
                );
            });
            for &nodes in &[10_000usize, 50_000] {
                let rows: Vec<u64> = (0..nodes)
                    .map(|i| 0x1111_0000_0000_0000u64.wrapping_add(i as u64))
                    .collect();
                let base = rows.as_ptr() as u64;
                let tree = flat_tree(nodes, NodeKind::Hex64);
                group.throughput(Throughput::Elements(nodes as u64));
                group.bench_with_input(
                    BenchmarkId::new("direct_current_process", nodes),
                    &nodes,
                    |b, _| {
                        b.iter(|| {
                            black_box(&rows);
                            let result = compose::compose_with_symbols_at_base(
                                black_box(&tree),
                                black_box(provider.as_ref()),
                                0,
                                black_box(base),
                                false,
                                false,
                                false,
                                true,
                                true,
                                None,
                                true,
                                true,
                            );
                            black_box(result.meta.len());
                        });
                    },
                );
                group.bench_with_input(
                    BenchmarkId::new("cached_current_process", nodes),
                    &nodes,
                    |b, _| {
                        b.iter_batched(
                            || CachedPageProvider::new(Arc::clone(&provider)),
                            |cache| {
                                black_box(&rows);
                                let result = compose::compose_with_symbols_at_base(
                                    black_box(&tree),
                                    black_box(&cache),
                                    0,
                                    black_box(base),
                                    false,
                                    false,
                                    false,
                                    true,
                                    true,
                                    None,
                                    true,
                                    true,
                                );
                                black_box(result.meta.len());
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
            }
        }
    }
    group.finish();
}

fn controller_for(tree: NodeTree, provider: Arc<dyn Provider + Send + Sync>) -> RcxController {
    let mut doc = RcxDocument::new();
    doc.tree = tree;
    doc.provider = provider;
    RcxController::new(doc)
}

fn apply_initial_live_snapshot(controller: &mut RcxController) {
    match controller.on_refresh_tick() {
        RefreshPlan::Read { pages, provider } => {
            assert!(
                !pages.is_empty(),
                "initial live refresh should request pages"
            );
            let initial = RcxController::read_pages(&provider, &pages);
            controller.on_read_complete(initial);
        }
        RefreshPlan::None => panic!("initial live refresh should request pages"),
    }
}

fn set_first_visible_field_window(controller: &mut RcxController, lines: usize) {
    let _ = set_visible_field_window_at(controller, 0, lines);
}

fn set_visible_field_window_at(
    controller: &mut RcxController,
    first_field_idx: usize,
    lines: usize,
) -> u64 {
    let mut field_idx = 0usize;
    let mut first_line = None;
    let mut first_addr = 0u64;
    let mut last_line = None;
    let target_last = first_field_idx.saturating_add(lines.saturating_sub(1));

    for (line, lm) in controller.last_result().meta.iter().enumerate() {
        if lm.line_kind != LineKind::Field || lm.is_continuation {
            continue;
        }
        if field_idx == first_field_idx {
            first_line = Some(line);
            first_addr = lm.offset_addr;
        }
        if field_idx == target_last {
            last_line = Some(line);
            break;
        }
        field_idx = field_idx.saturating_add(1);
    }

    let first = first_line.expect("composed tree should contain the requested first field line");
    let last = last_line.unwrap_or_else(|| controller.last_result().meta.len().saturating_sub(1));
    controller.set_visible_line_range(first, last);
    first_addr
}

fn field_addrs_at(controller: &RcxController, first_field_idx: usize, lines: usize) -> Vec<u64> {
    let mut field_idx = 0usize;
    let mut addrs = Vec::with_capacity(lines);
    let end_field_idx = first_field_idx.saturating_add(lines);

    for lm in &controller.last_result().meta {
        if lm.line_kind != LineKind::Field || lm.is_continuation {
            continue;
        }
        if field_idx >= first_field_idx && field_idx < end_field_idx {
            addrs.push(lm.offset_addr);
            if addrs.len() == lines {
                break;
            }
        }
        field_idx = field_idx.saturating_add(1);
    }
    assert!(
        !addrs.is_empty(),
        "composed tree should contain the requested visible field addresses"
    );
    addrs
}

fn first_visible_field_addr(controller: &RcxController) -> u64 {
    controller
        .last_result()
        .meta
        .iter()
        .find(|lm| lm.line_kind == LineKind::Field && !lm.is_continuation)
        .map(|lm| lm.offset_addr)
        .expect("composed tree should contain a field line")
}

fn changed_pages_from_next_live_tick(controller: &mut RcxController) -> PageMap {
    let addr = first_visible_field_addr(controller);
    changed_pages_from_next_live_tick_for_addr(controller, addr)
}

fn offscreen_changed_pages_from_next_live_tick(controller: &mut RcxController) -> PageMap {
    changed_pages_from_next_live_tick_at(controller, usize::MAX)
}

fn changed_pages_from_next_live_tick_for_addr(
    controller: &mut RcxController,
    addr: u64,
) -> PageMap {
    changed_pages_from_next_live_tick_for_addrs(controller, &[addr])
}

fn changed_pages_from_next_live_tick_for_addrs(
    controller: &mut RcxController,
    addrs: &[u64],
) -> PageMap {
    assert!(
        !addrs.is_empty(),
        "changed-page benchmark should mutate at least one address"
    );
    match controller.on_refresh_tick() {
        RefreshPlan::Read { pages, provider } => {
            let mut changed = RcxController::read_pages(&provider, &pages);
            for &addr in addrs {
                let page = addr & !(K_PAGE_SIZE - 1);
                assert!(
                    pages.contains(&page),
                    "visible changed page should be part of the next live refresh"
                );
                let bytes = changed
                    .get_mut(&page)
                    .expect("visible changed page should be present in read result");
                let bytes = bytes.make_mut();
                bytes[(addr - page) as usize] = bytes[(addr - page) as usize].wrapping_add(1);
            }
            changed
        }
        RefreshPlan::None => panic!("second live refresh should request pages"),
    }
}

fn changed_pages_from_next_live_tick_at(
    controller: &mut RcxController,
    page_index: usize,
) -> PageMap {
    match controller.on_refresh_tick() {
        RefreshPlan::Read { pages, provider } => {
            assert!(
                !pages.is_empty(),
                "second live refresh should request pages"
            );
            let mut changed = RcxController::read_pages(&provider, &pages);
            let page = pages[page_index.min(pages.len() - 1)];
            let bytes = changed
                .get_mut(&page)
                .expect("requested page should be present in read result");
            let bytes = bytes.make_mut();
            bytes[0] = bytes[0].wrapping_add(1);
            changed
        }
        RefreshPlan::None => panic!("second live refresh should request pages"),
    }
}

fn compose_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("compose_visible_rows");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::from_parameter(nodes), &nodes, |b, _| {
            b.iter(|| {
                let result = compose::compose(
                    black_box(&tree),
                    black_box(&provider),
                    0,
                    false,
                    false,
                    false,
                    false,
                    true,
                    true,
                    true,
                );
                black_box(result.meta.len());
            });
        });
    }
    group.finish();

    let mut tree_group = c.benchmark_group("compose_visible_rows_tree_lines");
    tree_group.sample_size(10);
    tree_group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        tree_group.throughput(Throughput::Elements(nodes as u64));
        tree_group.bench_with_input(BenchmarkId::from_parameter(nodes), &nodes, |b, _| {
            b.iter(|| {
                let result = compose::compose(
                    black_box(&tree),
                    black_box(&provider),
                    0,
                    false,
                    true,
                    false,
                    false,
                    true,
                    true,
                    true,
                );
                black_box(result.meta.len());
            });
        });
    }
    tree_group.finish();

    let mut deep_group = c.benchmark_group("compose_deep_struct_spans");
    deep_group.sample_size(10);
    deep_group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        let tree = deep_tree(nodes);
        let provider = patterned_provider((nodes + 8) * 8);
        deep_group.throughput(Throughput::Elements(nodes as u64));
        deep_group.bench_with_input(BenchmarkId::from_parameter(nodes), &nodes, |b, _| {
            b.iter(|| {
                let result = compose::compose(
                    black_box(&tree),
                    black_box(&provider),
                    0,
                    false,
                    false,
                    false,
                    false,
                    true,
                    true,
                    true,
                );
                black_box(result.meta.len());
            });
        });
    }
    deep_group.finish();

    let mut enum_group = c.benchmark_group("compose_enum_chips");
    enum_group.sample_size(10);
    enum_group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        let member_count = 1_024usize;
        let (tree, root_id) = enum_ref_tree(nodes, member_count);
        let mut data = vec![0u8; nodes * 4 + 4];
        for i in 0..nodes {
            let value = (i % member_count) as u32;
            data[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let provider = BufferProvider::new(data, "enum.bin");
        enum_group.throughput(Throughput::Elements(nodes as u64));
        enum_group.bench_with_input(
            BenchmarkId::new("uint32_enum_members_1024", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        black_box(root_id),
                        false,
                        false,
                        false,
                        false,
                        true,
                        false,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    enum_group.finish();

    let mut struct_array_group = c.benchmark_group("compose_struct_array_refs");
    struct_array_group.sample_size(10);
    struct_array_group.measurement_time(Duration::from_secs(2));
    for &elements in &[1_000usize, 10_000] {
        let fields_per_struct = 16usize;
        let tree = struct_array_ref_tree(elements, fields_per_struct);
        let provider = patterned_provider((elements + 1) * fields_per_struct * 8);
        struct_array_group.throughput(Throughput::Elements(elements as u64));
        struct_array_group.bench_with_input(
            BenchmarkId::new("element_struct_16_fields", elements),
            &elements,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        false,
                        true,
                        false,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    struct_array_group.finish();

    let mut pointer_ref_group = c.benchmark_group("compose_pointer_ref_names");
    pointer_ref_group.sample_size(10);
    pointer_ref_group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = pointer_ref_tree(nodes);
        let provider = patterned_provider((nodes + 8) * 8);
        pointer_ref_group.throughput(Throughput::Elements(nodes as u64));
        pointer_ref_group.bench_with_input(
            BenchmarkId::new("same_target_pointer64", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        false,
                        true,
                        false,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    pointer_ref_group.finish();
}

fn tree_child_access_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_child_access");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &children in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(children, NodeKind::Hex64);
        let root_id = tree.nodes[0].id;
        group.throughput(Throughput::Elements(children as u64));
        group.bench_with_input(
            BenchmarkId::new("children_of_len_clone", children),
            &children,
            |b, _| {
                b.iter(|| {
                    black_box(tree.children_of(black_box(root_id)).len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("child_count_cached", children),
            &children,
            |b, _| {
                b.iter(|| {
                    black_box(tree.child_count(black_box(root_id)));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("children_of_iter_clone", children),
            &children,
            |b, _| {
                b.iter(|| {
                    let mut sum = 0usize;
                    for idx in tree.children_of(black_box(root_id)) {
                        sum = sum.wrapping_add(idx);
                    }
                    black_box(sum);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("with_children_iter_borrowed", children),
            &children,
            |b, _| {
                b.iter(|| {
                    tree.with_children(black_box(root_id), |children| {
                        let mut sum = 0usize;
                        for &idx in children {
                            sum = sum.wrapping_add(idx);
                        }
                        black_box(sum);
                    });
                });
            },
        );
    }

    group.finish();
}

fn tree_id_lookup_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_id_lookup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let ids: Vec<u64> = tree.nodes.iter().map(|node| node.id).collect();
        let missing_base = ids
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(1_000_000);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("cached_hits", nodes),
            &nodes,
            |b, &nodes| {
                b.iter(|| {
                    let mut sum = 0i64;
                    for i in 0..nodes {
                        sum += tree.index_of_id(black_box(ids[i % ids.len()])) as i64;
                    }
                    black_box(sum);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("cached_misses", nodes),
            &nodes,
            |b, &nodes| {
                b.iter(|| {
                    let mut sum = 0i64;
                    for i in 0..nodes {
                        sum += tree.index_of_id(black_box(missing_base + i as u64)) as i64;
                    }
                    black_box(sum);
                });
            },
        );
    }

    group.finish();
}

fn tree_selection_normalization_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_selection_normalization");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let selected: HashSet<u64> = tree.nodes.iter().map(|node| node.id).collect();
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("flat_all_selected_prefer_ancestors", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let normalized = tree.normalize_prefer_ancestors(black_box(&selected));
                    black_box(normalized.len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("flat_all_selected_prefer_descendants", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let normalized = tree.normalize_prefer_descendants(black_box(&selected));
                    black_box(normalized.len());
                });
            },
        );
    }

    for &nodes in &[1_000usize, 2_000] {
        let tree = deep_tree(nodes);
        let selected: HashSet<u64> = tree.nodes.iter().map(|node| node.id).collect();
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("deep_all_selected_prefer_ancestors", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let normalized = tree.normalize_prefer_ancestors(black_box(&selected));
                    black_box(normalized.len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("deep_all_selected_prefer_descendants", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let normalized = tree.normalize_prefer_descendants(black_box(&selected));
                    black_box(normalized.len());
                });
            },
        );
    }

    group.finish();
}

fn statusbar_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("statusbar_model");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &children in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(children, NodeKind::Hex64);
        let root_id = tree.nodes[0].id;
        let selected = HashSet::new();
        group.throughput(Throughput::Elements(children as u64));
        group.bench_with_input(
            BenchmarkId::new("default_view_clone_count_baseline", children),
            &children,
            |b, _| {
                b.iter(|| {
                    let idx = tree.index_of_id(black_box(root_id));
                    let node = &tree.nodes[idx as usize];
                    let fields = tree.children_of(node.id).len();
                    let size = tree.total_byte_size(node);
                    let plural = if fields == 1 { "field" } else { "fields" };
                    let info = format!("{fields} {plural} \u{00B7} 0x{size:X} bytes");
                    black_box(info.len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("default_view_status_info", children),
            &children,
            |b, _| {
                b.iter(|| {
                    let status = StatusInfo::from_tree(
                        black_box(&tree),
                        black_box(&selected),
                        black_box(root_id),
                        |_| String::new(),
                    );
                    black_box(status.info.len());
                });
            },
        );
    }

    group.finish();
}

fn editor_line_text_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_line_text");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let result = compose::compose(
            black_box(&tree),
            black_box(&provider),
            0,
            false,
            false,
            false,
            false,
            true,
            true,
            true,
        );
        let start = result.meta.len().saturating_sub(256);
        let end = result.meta.len();
        group.throughput(Throughput::Elements((end - start) as u64));
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_utf16_scan", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for line in start..end {
                        let range = reclass::ui::editor::geometry::line_byte_range(
                            black_box(&result.text),
                            black_box(&result.line_starts),
                            black_box(line),
                        );
                        total = total.wrapping_add(
                            black_box(&result.text[range]).trim_end_matches('\n').len(),
                        );
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_byte_starts", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for line in start..end {
                        let range = reclass::ui::editor::geometry::line_byte_range_from_byte_starts(
                            black_box(&result.text),
                            black_box(&result.line_byte_starts),
                            black_box(line),
                        );
                        total = total.wrapping_add(
                            black_box(&result.text[range]).trim_end_matches('\n').len(),
                        );
                    }
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

fn linear_line_for_node(meta: &[LineMeta], node_id: u64) -> Option<usize> {
    meta.iter().position(|lm| {
        lm.node_id == node_id && lm.line_kind != LineKind::Footer && !lm.is_continuation
    })
}

fn build_node_line_index(meta: &[LineMeta]) -> HashMap<u64, usize> {
    let mut index = HashMap::with_capacity(meta.len());
    for (line, lm) in meta.iter().enumerate() {
        if lm.node_id == 0 || lm.node_id == reclass::core::linemeta::K_COMMAND_ROW_ID {
            continue;
        }
        if lm.line_kind == LineKind::Footer || lm.is_continuation {
            continue;
        }
        index.entry(lm.node_id).or_insert(line);
    }
    index
}

fn editor_node_line_lookup_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_node_line_lookup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let result = compose::compose(
            black_box(&tree),
            black_box(&provider),
            0,
            false,
            false,
            false,
            false,
            true,
            true,
            true,
        );
        let ids: Vec<u64> = (0..1024usize)
            .map(|i| {
                let pos = 1 + (i * nodes.saturating_sub(1) / 1023).min(nodes.saturating_sub(1));
                tree.nodes[pos].id
            })
            .collect();
        group.throughput(Throughput::Elements(ids.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("linear_1024_queries", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &id in &ids {
                        total = total.wrapping_add(
                            linear_line_for_node(black_box(&result.meta), black_box(id))
                                .unwrap_or(0),
                        );
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("compose_result_indexed_1024_queries", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &id in &ids {
                        total = total.wrapping_add(
                            black_box(&result).line_for_node(black_box(id)).unwrap_or(0),
                        );
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(BenchmarkId::new("build_index", nodes), &nodes, |b, _| {
            b.iter(|| {
                black_box(build_node_line_index(black_box(&result.meta)));
            });
        });
    }
    group.finish();
}

fn find_highlight_linear_count(
    matches: &[FindMatch],
    current: Option<FindMatch>,
    start: usize,
    end: usize,
) -> usize {
    let mut total = 0usize;
    for line in start..end {
        for m in matches {
            if m.line == line && m.end > m.start {
                total = total.wrapping_add(m.end - m.start);
                if current.is_some_and(|c| c.line == m.line && c.start == m.start && c.end == m.end)
                {
                    total = total.wrapping_add(1);
                }
            }
        }
    }
    total
}

fn editor_find_highlight_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_find_highlights");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &matches_len in &[10_000usize, 50_000] {
        let matches: Vec<FindMatch> = (0..matches_len)
            .map(|line| FindMatch {
                line,
                start: 12,
                end: 18,
            })
            .collect();
        let start = matches_len.saturating_sub(256);
        let end = matches_len;
        let current = matches.last().copied();
        let ranges = reclass::ui::editor::bench_find_match_line_ranges(matches_len, &matches);
        group.throughput(Throughput::Elements((end - start) as u64));
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_linear_scan", matches_len),
            &matches_len,
            |b, _| {
                b.iter(|| {
                    black_box(find_highlight_linear_count(
                        black_box(&matches),
                        black_box(current),
                        black_box(start),
                        black_box(end),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_indexed_prebuilt", matches_len),
            &matches_len,
            |b, _| {
                b.iter(|| {
                    black_box(
                        reclass::ui::editor::bench_find_highlight_indexed_count_with_ranges(
                            black_box(&matches),
                            black_box(&ranges),
                            black_box(current),
                            black_box(start),
                            black_box(end),
                        ),
                    );
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("build_line_range_index", matches_len),
            &matches_len,
            |b, _| {
                b.iter(|| {
                    black_box(reclass::ui::editor::bench_find_match_line_ranges(
                        black_box(matches_len),
                        black_box(&matches),
                    ));
                });
            },
        );
    }
    group.finish();
}

fn editor_find_search_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_find_search");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &lines_len in &[1_000usize, 10_000, 50_000] {
        let lines: Vec<String> = (0..lines_len)
            .map(|i| {
                format!(
                    "0x{:08X}    Hex64 field_{i:05} = 0x{:016X} // PlayerHealth",
                    i * 8,
                    (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                )
            })
            .collect();
        group.throughput(Throughput::Elements(lines_len as u64));
        group.bench_with_input(
            BenchmarkId::new("ascii_many_matches", lines_len),
            &lines_len,
            |b, _| {
                b.iter(|| {
                    let mut state = FindState::new();
                    state.set_query(black_box("FIELD"), black_box(&lines));
                    black_box(state.match_count());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("ascii_no_matches", lines_len),
            &lines_len,
            |b, _| {
                b.iter(|| {
                    let mut state = FindState::new();
                    state.set_query(black_box("missing_needle"), black_box(&lines));
                    black_box(state.match_count());
                });
            },
        );
    }

    group.finish();
}

fn selection_linear_matches_row(selected: &HashSet<u64>, lm: &LineMeta) -> bool {
    use reclass::core::linemeta::{
        array_elem_idx_from_sel_id, member_sub_from_sel_id, sel_kind, SelKind, K_COMMAND_ROW_ID,
    };
    if lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
        return false;
    }
    selected.iter().any(|&sel_id| {
        if reclass::controller::strip_sel_pub(sel_id) != lm.node_id {
            return false;
        }
        let sk = sel_kind(sel_id);
        let is_footer = lm.line_kind == LineKind::Footer;
        if (sk == SelKind::Footer) != is_footer {
            return false;
        }
        if sk == SelKind::ArrayElem {
            if !lm.is_array_element || lm.array_element_idx != array_elem_idx_from_sel_id(sel_id) {
                return false;
            }
        } else if lm.is_array_element {
            return false;
        }
        if sk == SelKind::Member {
            if !lm.is_member_line || lm.sub_line != member_sub_from_sel_id(sel_id) {
                return false;
            }
        } else if lm.is_member_line {
            return false;
        }
        true
    })
}

fn selection_direct_matches_row(selected: &HashSet<u64>, lm: &LineMeta) -> bool {
    use reclass::core::linemeta::{sel_id_for_line, K_COMMAND_ROW_ID};
    if lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
        return false;
    }
    selected.contains(&sel_id_for_line(lm))
}

fn selected_indices_ordered_linear(meta: &[LineMeta], selected: &HashSet<u64>) -> usize {
    use reclass::core::linemeta::K_COMMAND_ROW_ID;

    let mut total = 0usize;
    let mut seen = HashSet::new();
    for lm in meta {
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            continue;
        }
        if !seen.insert(lm.node_id) {
            continue;
        }
        if selected
            .iter()
            .any(|&id| reclass::controller::strip_sel_pub(id) == lm.node_id)
        {
            total = total.wrapping_add(lm.node_idx as usize);
        }
    }
    total
}

fn selected_indices_ordered_stripped_set(meta: &[LineMeta], selected: &HashSet<u64>) -> usize {
    use reclass::core::linemeta::K_COMMAND_ROW_ID;

    let selected_bare: HashSet<u64> = selected
        .iter()
        .map(|&id| reclass::controller::strip_sel_pub(id))
        .collect();
    let mut total = 0usize;
    let mut seen = HashSet::new();
    for lm in meta {
        if lm.node_idx < 0 || lm.node_id == 0 || lm.node_id == K_COMMAND_ROW_ID {
            continue;
        }
        if !seen.insert(lm.node_id) {
            continue;
        }
        if selected_bare.contains(&lm.node_id) {
            total = total.wrapping_add(lm.node_idx as usize);
        }
    }
    total
}

fn byte_selection_full_scan(meta: &[LineMeta], sel: (u64, u64)) -> usize {
    let mut total = 0usize;
    for lm in meta {
        if !reclass::core::is_hex_preview(lm.node_kind) || lm.line_kind != LineKind::Field {
            continue;
        }
        let count = if lm.line_byte_count > 0 {
            lm.line_byte_count
        } else {
            reclass::core::size_for_kind(lm.node_kind)
        };
        if reclass::ui::editor::selection::row_byte_overlap(lm.offset_addr, count, sel).is_some() {
            total = total.wrapping_add(reclass::core::sel_id_for_line(lm) as usize);
        }
    }
    total
}

fn build_byte_selection_intervals(meta: &[LineMeta]) -> (Vec<(u64, u64, u64, usize)>, bool) {
    let mut rows = Vec::new();
    let mut in_order = true;
    let mut non_overlapping = true;
    let mut previous: Option<(u64, u64)> = None;
    for (line, lm) in meta.iter().enumerate() {
        if !reclass::core::is_hex_preview(lm.node_kind) || lm.line_kind != LineKind::Field {
            continue;
        }
        let count = if lm.line_byte_count > 0 {
            lm.line_byte_count
        } else {
            reclass::core::size_for_kind(lm.node_kind)
        };
        if count <= 0 {
            continue;
        }
        let start = lm.offset_addr;
        let end = start.saturating_add(count as u64);
        if end > start {
            if let Some((prev_start, prev_end)) = previous {
                if prev_start > start || (prev_start == start && prev_end > end) {
                    in_order = false;
                }
                if prev_end > start {
                    non_overlapping = false;
                }
            }
            previous = Some((start, end));
            rows.push((start, end, reclass::core::sel_id_for_line(lm), line));
        }
    }
    if !in_order {
        rows.sort_unstable_by_key(|&(start, end, _, _)| (start, end));
        non_overlapping = rows.windows(2).all(|pair| pair[0].1 <= pair[1].0);
    }
    (rows, non_overlapping)
}

fn byte_selection_indexed(intervals: &[(u64, u64, u64, usize)], sel: (u64, u64)) -> usize {
    let (lo, hi) = sel;
    let start = intervals.partition_point(|&(_, end, _, _)| end <= lo);
    let mut total = 0usize;
    for &(row_lo, _, sel_id, _) in &intervals[start..] {
        if row_lo >= hi {
            break;
        }
        total = total.wrapping_add(sel_id as usize);
    }
    total
}

fn byte_row_line_full_scan(meta: &[LineMeta], addr: u64) -> Option<usize> {
    meta.iter().position(|lm| {
        let count = if lm.line_byte_count > 0 {
            lm.line_byte_count
        } else {
            reclass::core::size_for_kind(lm.node_kind)
        };
        reclass::core::is_hex_preview(lm.node_kind)
            && lm.line_kind == LineKind::Field
            && addr >= lm.offset_addr
            && addr < lm.offset_addr + count.max(0) as u64
    })
}

fn byte_row_line_indexed(intervals: &[(u64, u64, u64, usize)], addr: u64) -> Option<usize> {
    let idx = intervals.partition_point(|&(_, end, _, _)| end <= addr);
    let (start, end, _, line) = intervals.get(idx).copied()?;
    (addr >= start && addr < end).then_some(line)
}

fn editor_selection_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_selection_rows");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let result = compose::compose(
            black_box(&tree),
            black_box(&provider),
            0,
            false,
            false,
            false,
            false,
            true,
            true,
            true,
        );
        let selected: HashSet<u64> = result
            .meta
            .iter()
            .filter(|lm| lm.node_id != 0 && lm.node_id != reclass::core::linemeta::K_COMMAND_ROW_ID)
            .map(reclass::core::linemeta::sel_id_for_line)
            .collect();
        let start = result.meta.len().saturating_sub(256);
        let end = result.meta.len();
        group.throughput(Throughput::Elements((end - start) as u64));
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_linear_scan", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for lm in &result.meta[start..end] {
                        if selection_linear_matches_row(black_box(&selected), black_box(lm)) {
                            total += 1;
                        }
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_direct_lookup", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for lm in &result.meta[start..end] {
                        if selection_direct_matches_row(black_box(&selected), black_box(lm)) {
                            total += 1;
                        }
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("ordered_selected_indices_linear", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(selected_indices_ordered_linear(
                        black_box(&result.meta),
                        black_box(&selected),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("ordered_selected_indices_stripped_set", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(selected_indices_ordered_stripped_set(
                        black_box(&result.meta),
                        black_box(&selected),
                    ));
                });
            },
        );
        let sel_start = (nodes.saturating_sub(256) * 8) as u64;
        let sel = (sel_start, sel_start + 128);
        let (intervals, _) = build_byte_selection_intervals(&result.meta);
        let byte_line_addr = sel_start;
        group.bench_with_input(
            BenchmarkId::new("byte_rows_full_scan", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(byte_selection_full_scan(
                        black_box(&result.meta),
                        black_box(sel),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("byte_rows_indexed", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(byte_selection_indexed(
                        black_box(&intervals),
                        black_box(sel),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("byte_row_line_full_scan", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(byte_row_line_full_scan(
                        black_box(&result.meta),
                        black_box(byte_line_addr),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("byte_row_line_indexed", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(byte_row_line_indexed(
                        black_box(&intervals),
                        black_box(byte_line_addr),
                    ));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("build_byte_row_index", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(build_byte_selection_intervals(black_box(&result.meta)));
                });
            },
        );
    }
    group.finish();
}

fn editor_style_runs_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_style_runs");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let result = compose::compose(
            black_box(&tree),
            black_box(&provider),
            0,
            false,
            false,
            false,
            true,
            true,
            true,
            true,
        );
        let start = result.meta.len().saturating_sub(256);
        let end = result.meta.len();
        group.throughput(Throughput::Elements((end - start) as u64));
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_style_runs", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for line in start..end {
                        let range = reclass::ui::editor::geometry::line_byte_range_from_byte_starts(
                            black_box(&result.text),
                            black_box(&result.line_byte_starts),
                            black_box(line),
                        );
                        let text = black_box(&result.text[range]).trim_end_matches('\n');
                        let lm = black_box(&result.meta[line]);
                        let (type_w, name_w) = reclass::ui::editor::geometry::effective_widths(lm);
                        let runs = reclass::ui::editor::geometry::style_runs(
                            black_box(lm),
                            black_box(text),
                            black_box(type_w),
                            black_box(name_w),
                        );
                        total = total.wrapping_add(runs.len());
                    }
                    black_box(total);
                });
            },
        );
        let cached_static: Vec<_> = (start..end)
            .map(|line| {
                let range = reclass::ui::editor::geometry::line_byte_range_from_byte_starts(
                    &result.text,
                    &result.line_byte_starts,
                    line,
                );
                let text = result.text[range].trim_end_matches('\n').to_string();
                let lm = &result.meta[line];
                let (type_w, name_w) = reclass::ui::editor::geometry::effective_widths(lm);
                let runs: Arc<[reclass::ui::editor::geometry::SpanStyle]> =
                    reclass::ui::editor::geometry::style_runs(lm, &text, type_w, name_w).into();
                (SharedString::from(text), runs)
            })
            .collect();
        group.bench_with_input(
            BenchmarkId::new("tail_visible_rows_cached_static_reuse", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for (text, runs) in &cached_static {
                        let text = text.clone();
                        let runs = Arc::clone(runs);
                        total = total
                            .wrapping_add(black_box(text.len()))
                            .wrapping_add(black_box(runs.len()));
                    }
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

fn editor_static_row_cache_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_static_row_cache");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    let visible_rows = 256usize;
    for &total_lines in &[10_000usize, 50_000] {
        let start = total_lines.saturating_sub(visible_rows);
        let end = total_lines;
        group.throughput(Throughput::Elements(visible_rows as u64));
        group.bench_with_input(
            BenchmarkId::new("dense_vec_tail_rebuild", total_lines),
            &total_lines,
            |b, _| {
                b.iter(|| {
                    let mut rows: Vec<Option<usize>> = Vec::new();
                    rows.resize_with(black_box(total_lines), || None);
                    let mut total = 0usize;
                    for idx in start..end {
                        rows[idx] = Some(idx);
                        total = total.wrapping_add(rows[idx].unwrap_or(0));
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("sparse_hash_tail_rebuild", total_lines),
            &total_lines,
            |b, _| {
                b.iter(|| {
                    let mut rows = AHashMap::with_capacity(visible_rows);
                    let mut total = 0usize;
                    for idx in start..end {
                        rows.insert(idx, idx);
                        total = total.wrapping_add(*rows.get(&idx).unwrap_or(&0));
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("hybrid_tail_rebuild", total_lines),
            &total_lines,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    if total_lines >= 20_000 {
                        let mut rows = AHashMap::with_capacity(512);
                        for idx in start..end {
                            rows.insert(idx, idx);
                            total = total.wrapping_add(*rows.get(&idx).unwrap_or(&0));
                        }
                    } else {
                        let mut rows: Vec<Option<usize>> = Vec::new();
                        rows.resize_with(black_box(total_lines), || None);
                        for idx in start..end {
                            rows[idx] = Some(idx);
                            total = total.wrapping_add(rows[idx].unwrap_or(0));
                        }
                    }
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

fn bench_hsla(h: f32, s: f32, l: f32, a: f32) -> gpui::Hsla {
    gpui::hsla(h, s, l, a)
}

fn bench_editor_palette() -> EditorPalette {
    let text = bench_hsla(0.0, 0.0, 0.86, 1.0);
    let dim = bench_hsla(0.62, 0.10, 0.45, 1.0);
    let blue = bench_hsla(0.58, 0.82, 0.66, 1.0);
    let cyan = bench_hsla(0.50, 0.55, 0.62, 1.0);
    let green = bench_hsla(0.28, 0.45, 0.62, 1.0);
    let orange = bench_hsla(0.08, 0.55, 0.62, 1.0);
    let red = bench_hsla(0.0, 0.65, 0.60, 1.0);
    EditorPalette {
        text,
        type_fg: blue,
        fnptr_fg: blue,
        name_fg: text,
        value_fg: green,
        string_val: orange,
        dim,
        keyword: blue,
        class_name: cyan,
        number: orange,
        ascii: dim,
        comment_green: green,
        type_hint: green,
        type_hint_type: blue,
        type_hint_operator: dim,
        type_hint_address: orange,
        type_hint_number: orange,
        type_hint_string: green,
        type_hint_keyword: blue,
        rtti_hint: orange,
        enum_chip: blue,
        tree_conn: dim,
        selection_bg: bench_hsla(0.58, 0.30, 0.25, 0.45),
        accent: blue,
        hover_bg: bench_hsla(0.0, 0.0, 1.0, 0.05),
        caret: text,
        paper: bench_hsla(0.62, 0.08, 0.12, 1.0),
        gutter_bg: bench_hsla(0.62, 0.08, 0.10, 1.0),
        gutter_fg: dim,
        pill_bg: bench_hsla(0.0, 0.0, 1.0, 0.06),
        heat_cold: orange,
        heat_warm: orange,
        heat_hot: red,
        byte_sel: blue,
        border: bench_hsla(0.0, 0.0, 0.28, 1.0),
        fold_chevron: dim,
        active_line_bg: bench_hsla(0.0, 0.0, 1.0, 0.03),
        error_bg: bench_hsla(0.0, 0.65, 0.60, 0.22),
        error_fg: red,
        focus_glow: blue,
    }
}

fn editor_minimap_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("editor_minimap_rows");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    let palette = bench_editor_palette();

    for &nodes in &[10_000usize, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let result = compose::compose(
            black_box(&tree),
            black_box(&provider),
            0,
            false,
            false,
            false,
            false,
            true,
            true,
            true,
        );
        let cached_rows = reclass::ui::editor::bench_minimap_rows_for_meta(&result.meta, palette);
        group.throughput(Throughput::Elements(result.meta.len() as u64));
        group.bench_with_input(BenchmarkId::new("rebuild_rows", nodes), &nodes, |b, _| {
            b.iter(|| {
                black_box(reclass::ui::editor::bench_minimap_rows_for_meta(
                    black_box(&result.meta),
                    black_box(palette),
                ));
            });
        });
        group.bench_with_input(
            BenchmarkId::new("reuse_cached_rows_arc", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    black_box(Arc::clone(black_box(&cached_rows)));
                });
            },
        );
    }
    group.finish();
}

fn type_hint_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("type_inference_hex_rows");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("compose_type_hints", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        true,
                        true,
                        true,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    for &nodes in &[1_000usize, 10_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = LiveLikeProvider::new_float_pairs(nodes);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("compose_type_hints_live_like", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        true,
                        true,
                        true,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_pointer_targets(nodes);
        let mut tree = flat_tree(nodes, NodeKind::Hex64);
        tree.base_address = provider.base();
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("compose_pointer_type_hints_live_like", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        true,
                        true,
                        false,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_compose_pointer_hint_regions(nodes);
        let mut tree = flat_tree(nodes, NodeKind::Hex64);
        tree.base_address = provider.base();
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("compose_pointer_type_hints_many_mappings", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose(
                        black_box(&tree),
                        black_box(&provider),
                        0,
                        false,
                        false,
                        false,
                        true,
                        true,
                        false,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    group.bench_function("infer_10k_rows", |b| {
        b.iter(|| {
            for i in 0..10_000u64 {
                let bytes = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes();
                black_box(infer_types(black_box(&bytes), &InferHints::default(), 3));
            }
        });
    });
    group.bench_function("infer_strong_10k_rows", |b| {
        b.iter(|| {
            for i in 0..10_000u64 {
                let bytes = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes();
                black_box(infer_strong_types(
                    black_box(&bytes),
                    &InferHints::default(),
                    2,
                ));
            }
        });
    });
    group.finish();
}

fn refresh_extent_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_extent");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000, 50_000] {
        let provider = Arc::new(patterned_provider((nodes + 8) * 8));
        let controller = controller_for(flat_tree(nodes, NodeKind::Hex64), provider);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::new("flat", nodes), &nodes, |b, _| {
            b.iter(|| black_box(controller.data_extent()));
        });
    }
    for &nodes in &[1_000usize, 10_000] {
        let provider = Arc::new(patterned_provider((nodes + 8) * 8));
        let controller = controller_for(deep_tree(nodes), provider);
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::new("deep", nodes), &nodes, |b, _| {
            b.iter(|| black_box(controller.data_extent()));
        });
    }
    group.finish();
}

fn append_growth_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("append_grow_memory_nodes");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &fields in &[1_000usize, 10_000] {
        group.throughput(Throughput::Elements(fields as u64));
        group.bench_with_input(
            BenchmarkId::new("bulk_4096_live_refresh", fields),
            &fields,
            |b, &fields| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(fields + 4096));
                        controller_for(flat_tree(fields, NodeKind::Hex64), provider)
                    },
                    |mut controller| {
                        let root_id = controller.tree().nodes[0].id;
                        let ids = controller.append_hex_fields_to_struct(root_id, 4096);
                        black_box(ids.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &initial_fields in &[1_000usize, 10_000] {
        let append_count = 512usize;
        group.throughput(Throughput::Elements(append_count as u64));
        group.bench_with_input(
            BenchmarkId::new("single_tail_repeated_512_live_refresh", initial_fields),
            &initial_fields,
            |b, &initial_fields| {
                b.iter_batched(
                    || {
                        let provider =
                            Arc::new(LiveLikeProvider::new_float_pairs(initial_fields + 2048));
                        controller_for(flat_tree(initial_fields, NodeKind::Hex64), provider)
                    },
                    |mut controller| {
                        let root_id = controller.tree().nodes[0].id;
                        let mut last_id = *controller
                            .tree()
                            .children_of(root_id)
                            .last()
                            .map(|&idx| &controller.tree().nodes[idx].id)
                            .unwrap();
                        for _ in 0..append_count {
                            last_id = controller
                                .append_single_field(last_id)
                                .expect("append single field");
                        }
                        black_box(last_id);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("single_tail_batched_512_live_refresh", initial_fields),
            &initial_fields,
            |b, &initial_fields| {
                b.iter_batched(
                    || {
                        let provider =
                            Arc::new(LiveLikeProvider::new_float_pairs(initial_fields + 2048));
                        controller_for(flat_tree(initial_fields, NodeKind::Hex64), provider)
                    },
                    |mut controller| {
                        let root_id = controller.tree().nodes[0].id;
                        let last_id = *controller
                            .tree()
                            .children_of(root_id)
                            .last()
                            .map(|&idx| &controller.tree().nodes[idx].id)
                            .unwrap();
                        let ids = controller.append_single_fields(last_id, append_count);
                        black_box(ids.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        if initial_fields == 1_000 {
            group.bench_with_input(
                BenchmarkId::new(
                    "single_tail_repeated_512_live_extra_refresh",
                    initial_fields,
                ),
                &initial_fields,
                |b, &initial_fields| {
                    b.iter_batched(
                        || {
                            let provider =
                                Arc::new(LiveLikeProvider::new_float_pairs(initial_fields + 2048));
                            controller_for(flat_tree(initial_fields, NodeKind::Hex64), provider)
                        },
                        |mut controller| {
                            let root_id = controller.tree().nodes[0].id;
                            let mut last_id = *controller
                                .tree()
                                .children_of(root_id)
                                .last()
                                .map(|&idx| &controller.tree().nodes[idx].id)
                                .unwrap();
                            for _ in 0..append_count {
                                last_id = controller
                                    .append_single_field(last_id)
                                    .expect("append single field");
                                controller.refresh();
                            }
                            black_box(last_id);
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
        group.bench_with_input(
            BenchmarkId::new("single_tail_repeated_512_mutation_only", initial_fields),
            &initial_fields,
            |b, &initial_fields| {
                b.iter_batched(
                    || {
                        let provider =
                            Arc::new(LiveLikeProvider::new_float_pairs(initial_fields + 2048));
                        let mut controller =
                            controller_for(flat_tree(initial_fields, NodeKind::Hex64), provider);
                        controller.set_suppress_refresh(true);
                        controller
                    },
                    |mut controller| {
                        let root_id = controller.tree().nodes[0].id;
                        let mut last_id = *controller
                            .tree()
                            .children_of(root_id)
                            .last()
                            .map(|&idx| &controller.tree().nodes[idx].id)
                            .unwrap();
                        for _ in 0..append_count {
                            last_id = controller
                                .append_single_field(last_id)
                                .expect("append single field");
                        }
                        black_box(last_id);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &initial_fields in &[1_000usize, 10_000] {
        let batches = 8usize;
        let bytes_per_batch = 4096;
        group.throughput(Throughput::Elements((batches * bytes_per_batch / 8) as u64));
        group.bench_with_input(
            BenchmarkId::new("bulk_4096_repeated_8_mutation_only", initial_fields),
            &initial_fields,
            |b, &initial_fields| {
                b.iter_batched(
                    || {
                        let provider =
                            Arc::new(LiveLikeProvider::new_float_pairs(initial_fields + 8192));
                        let mut controller =
                            controller_for(flat_tree(initial_fields, NodeKind::Hex64), provider);
                        controller.set_suppress_refresh(true);
                        controller
                    },
                    |mut controller| {
                        let root_id = controller.tree().nodes[0].id;
                        let mut total = 0usize;
                        for _ in 0..batches {
                            total += controller
                                .append_hex_fields_to_struct(root_id, bytes_per_batch as i32)
                                .len();
                        }
                        black_box(total);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn changed_page_refresh_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_changed_pages");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("full_page_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        let mut initial = PageMap::new();
                        initial.insert(0, vec![0u8; 4096].into());
                        controller.on_read_complete(initial);

                        let mut changed = PageMap::new();
                        changed.insert(0, vec![0x5Au8; 4096].into());
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &nodes in &[10_000usize, 50_000] {
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("visible_80_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        set_first_visible_field_window(&mut controller, 80);
                        let changed = changed_pages_from_next_live_tick(&mut controller);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_middle_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        let addr = set_visible_field_window_at(&mut controller, nodes / 2, 80);
                        let changed =
                            changed_pages_from_next_live_tick_for_addr(&mut controller, addr);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_1_middle_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        let addr = set_visible_field_window_at(&mut controller, nodes / 2, 1);
                        let changed =
                            changed_pages_from_next_live_tick_for_addr(&mut controller, addr);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_middle_changed_editor_tick_extra_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        apply_initial_live_snapshot(&mut controller);
                        let addr = set_visible_field_window_at(&mut controller, nodes / 2, 80);
                        let changed =
                            changed_pages_from_next_live_tick_for_addr(&mut controller, addr);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        controller.refresh();
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_middle_changed_editor_tick_stale_guard", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        apply_initial_live_snapshot(&mut controller);
                        let addr = set_visible_field_window_at(&mut controller, nodes / 2, 80);
                        let changed =
                            changed_pages_from_next_live_tick_for_addr(&mut controller, addr);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        controller.refresh_if_stale();
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_middle_changed_modules_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(
                            LiveLikeProvider::new_float_pairs(nodes + 2048).with_dummy_modules(64),
                        );
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        let addr = set_visible_field_window_at(&mut controller, nodes / 2, 80);
                        let changed =
                            changed_pages_from_next_live_tick_for_addr(&mut controller, addr);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_middle_all_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        let first_field_idx = nodes / 2;
                        set_visible_field_window_at(&mut controller, first_field_idx, 80);
                        let addrs = field_addrs_at(&controller, first_field_idx, 80);
                        let changed =
                            changed_pages_from_next_live_tick_for_addrs(&mut controller, &addrs);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("visible_80_offscreen_changed_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree_at_base(nodes, NodeKind::Hex64, 0), provider);
                        controller.set_track_values(true);
                        apply_initial_live_snapshot(&mut controller);
                        set_first_visible_field_window(&mut controller, 80);
                        let changed = offscreen_changed_pages_from_next_live_tick(&mut controller);
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &pages in &[64usize, 256] {
        group.throughput(Throughput::Bytes((pages * K_PAGE_SIZE as usize) as u64));
        group.bench_with_input(
            BenchmarkId::new("merge_fresh_pages", pages),
            &pages,
            |b, &pages| {
                b.iter_batched(
                    || {
                        let fields = pages * (K_PAGE_SIZE as usize / 8);
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(fields + 2048));
                        let mut controller =
                            controller_for(flat_tree(fields, NodeKind::Hex64), provider);
                        let mut initial = PageMap::new();
                        for page in 0..pages {
                            initial.insert(
                                (page as u64) * K_PAGE_SIZE,
                                vec![0x11; K_PAGE_SIZE as usize].into(),
                            );
                        }
                        controller.on_read_complete(initial);

                        let mut changed = PageMap::new();
                        for page in 0..pages {
                            changed.insert(
                                (page as u64) * K_PAGE_SIZE,
                                vec![0x22; K_PAGE_SIZE as usize].into(),
                            );
                        }
                        (controller, changed)
                    },
                    |(mut controller, changed)| {
                        controller.on_read_complete(changed);
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("merge_unchanged_pages", pages),
            &pages,
            |b, &pages| {
                b.iter_batched(
                    || {
                        let fields = pages * (K_PAGE_SIZE as usize / 8);
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(fields + 2048));
                        let mut controller =
                            controller_for(flat_tree(fields, NodeKind::Hex64), provider);
                        let mut initial = PageMap::new();
                        for page in 0..pages {
                            initial.insert(
                                (page as u64) * K_PAGE_SIZE,
                                vec![0x33; K_PAGE_SIZE as usize].into(),
                            );
                        }
                        controller.on_read_complete(initial);

                        let mut unchanged = PageMap::new();
                        for page in 0..pages {
                            unchanged.insert(
                                (page as u64) * K_PAGE_SIZE,
                                vec![0x33; K_PAGE_SIZE as usize].into(),
                            );
                        }
                        (controller, unchanged)
                    },
                    |(mut controller, unchanged)| {
                        controller.on_read_complete(unchanged);
                        black_box(controller.page_stability(0));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn refresh_page_plan_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_page_plan");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[10_000usize, 50_000] {
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("flat_live_first_snapshot_tick", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        controller_for(flat_tree(nodes, NodeKind::Hex64), provider)
                    },
                    |mut controller| {
                        let pages = match controller.on_refresh_tick() {
                            reclass::controller::RefreshPlan::Read { pages, .. } => pages.len(),
                            reclass::controller::RefreshPlan::None => 0,
                        };
                        black_box(pages);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &nodes in &[10_000usize, 50_000] {
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("flat_live_second_snapshot_tick", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree(nodes, NodeKind::Hex64), provider);
                        assert!(controller.pump_refresh());
                        controller
                    },
                    |mut controller| {
                        let pages = match controller.on_refresh_tick() {
                            reclass::controller::RefreshPlan::Read { pages, .. } => pages.len(),
                            reclass::controller::RefreshPlan::None => 0,
                        };
                        black_box(pages);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn refresh_read_pages_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_read_pages");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &pages in &[512usize, 2_048] {
        group.throughput(Throughput::Bytes((pages * 4096) as u64));
        group.bench_with_input(
            BenchmarkId::new("live_like_sequential_pages", pages),
            &pages,
            |b, &pages| {
                let provider: Arc<dyn Provider + Send + Sync> = Arc::new(
                    LiveLikeProvider::new_repeated_pattern(pages * 4096, &[0xA5]),
                );
                let page_addrs: Vec<u64> = (0..pages).map(|i| (i * 4096) as u64).collect();
                b.iter(|| {
                    let page_map =
                        RcxController::read_pages(black_box(&provider), black_box(&page_addrs));
                    black_box(page_map.len());
                });
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let regions = provider.enumerate_regions();
            if let Some(region) = regions
                .iter()
                .filter(|region| region.readable && region.size >= K_PAGE_SIZE)
                .max_by_key(|region| region.size)
            {
                let max_pages = (region.size / K_PAGE_SIZE) as usize;
                for &pages in &[64usize, 512] {
                    if pages > max_pages {
                        continue;
                    }
                    let provider: Arc<dyn Provider + Send + Sync> = Arc::new(
                        LocalProcessProvider::attach(&std::process::id().to_string()).unwrap(),
                    );
                    let page_addrs: Vec<u64> = (0..pages)
                        .map(|i| region.base + (i as u64) * K_PAGE_SIZE)
                        .collect();
                    group.throughput(Throughput::Bytes((pages * K_PAGE_SIZE as usize) as u64));
                    group.bench_with_input(
                        BenchmarkId::new("current_process_per_page", pages),
                        &pages,
                        |b, _| {
                            b.iter(|| {
                                let page_map = read_pages_per_page(
                                    black_box(&provider),
                                    black_box(&page_addrs),
                                );
                                black_box(page_map.len());
                            });
                        },
                    );
                    group.bench_with_input(
                        BenchmarkId::new("current_process_provider_bulk", pages),
                        &pages,
                        |b, _| {
                            b.iter(|| {
                                let page_map = RcxController::read_pages(
                                    black_box(&provider),
                                    black_box(&page_addrs),
                                );
                                black_box(page_map.len());
                            });
                        },
                    );
                }
            }
        }
        if let Ok(discovery) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let regions = discovery.enumerate_regions();
            let first_readable = regions
                .iter()
                .filter(|region| region.readable && region.size >= K_PAGE_SIZE)
                .min_by_key(|region| region.base);
            let largest_readable = regions
                .iter()
                .filter(|region| region.readable && region.size >= K_PAGE_SIZE)
                .max_by_key(|region| region.size);
            if let (Some(first), Some(region)) = (first_readable, largest_readable) {
                let invalid_pages_before_first = (first.base / K_PAGE_SIZE) as usize;
                let max_valid_pages = (region.size / K_PAGE_SIZE) as usize;
                for &valid_pages in &[64usize, 256] {
                    if valid_pages > max_valid_pages || valid_pages > invalid_pages_before_first {
                        continue;
                    }
                    let mut page_addrs = Vec::with_capacity(valid_pages * 2);
                    for i in 0..valid_pages {
                        page_addrs.push((i as u64) * K_PAGE_SIZE);
                        page_addrs.push(region.base + (i as u64) * K_PAGE_SIZE);
                    }
                    group.throughput(Throughput::Bytes(
                        (page_addrs.len() * K_PAGE_SIZE as usize) as u64,
                    ));

                    group.bench_with_input(
                        BenchmarkId::new("current_process_mixed_uncached", page_addrs.len()),
                        &page_addrs,
                        |b, page_addrs| {
                            b.iter_batched(
                                || {
                                    Arc::new(
                                        LocalProcessProvider::attach(
                                            &std::process::id().to_string(),
                                        )
                                        .unwrap(),
                                    )
                                        as Arc<dyn Provider + Send + Sync>
                                },
                                |provider| {
                                    let page_map = RcxController::read_pages(
                                        black_box(&provider),
                                        black_box(page_addrs),
                                    );
                                    black_box(page_map.len());
                                },
                                BatchSize::SmallInput,
                            );
                        },
                    );
                    group.bench_with_input(
                        BenchmarkId::new("current_process_mixed_cached_ranges", page_addrs.len()),
                        &page_addrs,
                        |b, page_addrs| {
                            b.iter_batched(
                                || {
                                    let provider = LocalProcessProvider::attach(
                                        &std::process::id().to_string(),
                                    )
                                    .unwrap();
                                    let _ = provider.enumerate_regions();
                                    Arc::new(provider) as Arc<dyn Provider + Send + Sync>
                                },
                                |provider| {
                                    let page_map = RcxController::read_pages(
                                        black_box(&provider),
                                        black_box(page_addrs),
                                    );
                                    black_box(page_map.len());
                                },
                                BatchSize::SmallInput,
                            );
                        },
                    );
                }
            }
        }
    }
    group.finish();
}

fn read_pages_per_page(provider: &Arc<dyn Provider + Send + Sync>, pages: &[u64]) -> PageMap {
    let mut out = PageMap::new();
    out.reserve(pages.len());
    for &page_addr in pages {
        let mut bytes = vec![0u8; K_PAGE_SIZE as usize];
        let _ = provider.read(page_addr, &mut bytes);
        out.insert(page_addr, bytes.into());
    }
    out
}

fn value_tracking_refresh_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_value_tracking");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("flat_hex64_live_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree(nodes, NodeKind::Hex64), provider);
                        controller.set_track_values(true);
                        controller
                    },
                    |mut controller| {
                        controller.refresh();
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("flat_hex64_stable_second_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree(nodes, NodeKind::Hex64), provider);
                        controller.set_track_values(true);
                        controller.refresh();
                        controller
                    },
                    |mut controller| {
                        controller.refresh();
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(
            BenchmarkId::new("flat_hex64_visible_80_refresh", nodes),
            &nodes,
            |b, &nodes| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_float_pairs(nodes + 2048));
                        let mut controller =
                            controller_for(flat_tree(nodes, NodeKind::Hex64), provider);
                        controller.set_track_values(true);
                        controller.set_visible_line_range(2, 81);
                        controller
                    },
                    |mut controller| {
                        controller.refresh();
                        black_box(controller.last_result().meta.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let provider: Arc<dyn Provider + Send + Sync> = Arc::new(provider);
            for &nodes in &[1_000usize, 10_000] {
                group.throughput(Throughput::Elements(nodes as u64));
                group.bench_with_input(
                    BenchmarkId::new("flat_hex64_current_process_refresh", nodes),
                    &nodes,
                    |b, &nodes| {
                        b.iter_batched(
                            || {
                                let rows: Vec<u64> = (0..nodes)
                                    .map(|i| 0xCAFE_0000_0000_0000u64.wrapping_add(i as u64))
                                    .collect();
                                let mut tree = flat_tree(nodes, NodeKind::Hex64);
                                tree.base_address = rows.as_ptr() as u64;
                                let mut controller = controller_for(tree, Arc::clone(&provider));
                                controller.set_track_values(true);
                                (rows, controller)
                            },
                            |(rows, mut controller)| {
                                black_box(&rows);
                                controller.refresh();
                                black_box(controller.last_result().meta.len());
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
                group.bench_with_input(
                    BenchmarkId::new("flat_hex64_current_process_stable_second_refresh", nodes),
                    &nodes,
                    |b, &nodes| {
                        b.iter_batched(
                            || {
                                let rows: Vec<u64> = (0..nodes)
                                    .map(|i| 0xCAFE_0000_0000_0000u64.wrapping_add(i as u64))
                                    .collect();
                                let mut tree = flat_tree(nodes, NodeKind::Hex64);
                                tree.base_address = rows.as_ptr() as u64;
                                let mut controller = controller_for(tree, Arc::clone(&provider));
                                controller.set_track_values(true);
                                controller.refresh();
                                (rows, controller)
                            },
                            |(rows, mut controller)| {
                                black_box(&rows);
                                controller.refresh();
                                black_box(controller.last_result().meta.len());
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
                group.bench_with_input(
                    BenchmarkId::new("flat_hex64_current_process_visible_80_refresh", nodes),
                    &nodes,
                    |b, &nodes| {
                        b.iter_batched(
                            || {
                                let rows: Vec<u64> = (0..nodes)
                                    .map(|i| 0xCAFE_0000_0000_0000u64.wrapping_add(i as u64))
                                    .collect();
                                let mut tree = flat_tree(nodes, NodeKind::Hex64);
                                tree.base_address = rows.as_ptr() as u64;
                                let mut controller = controller_for(tree, Arc::clone(&provider));
                                controller.set_track_values(true);
                                controller.set_visible_line_range(2, 81);
                                (rows, controller)
                            },
                            |(rows, mut controller)| {
                                black_box(&rows);
                                controller.refresh();
                                black_box(controller.last_result().meta.len());
                            },
                            BatchSize::SmallInput,
                        );
                    },
                );
            }
        }
    }
    group.finish();
}

fn permanent_page_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("refresh_permanent_pages");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &pages in &[512usize, 2_048] {
        group.throughput(Throughput::Elements(pages as u64));
        group.bench_with_input(
            BenchmarkId::new("classify_module_pages", pages),
            &pages,
            |b, &pages| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(LiveLikeProvider::new_module_pages(pages));
                        let controller = controller_for(flat_tree(1, NodeKind::Hex64), provider);
                        let mut fresh = PageMap::new();
                        for page in 0..pages {
                            fresh.insert((page * 4096) as u64, vec![0u8; 4096].into());
                        }
                        (controller, fresh)
                    },
                    |(mut controller, fresh)| {
                        controller.on_read_complete(fresh);
                        black_box(controller.snapshot_prov().is_some());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    for &pages in &[512usize, 2_048] {
        group.throughput(Throughput::Elements(pages as u64));
        group.bench_with_input(
            BenchmarkId::new("classify_heap_pages_with_modules", pages),
            &pages,
            |b, &pages| {
                b.iter_batched(
                    || {
                        let provider = Arc::new(
                            LiveLikeProvider::new_float_pairs(pages * 512 + 2048)
                                .with_dummy_modules(64),
                        );
                        let controller = controller_for(flat_tree(1, NodeKind::Hex64), provider);
                        let mut fresh = PageMap::new();
                        for page in 0..pages {
                            fresh.insert((page * 4096) as u64, vec![0u8; 4096].into());
                        }
                        (controller, fresh)
                    },
                    |(mut controller, fresh)| {
                        controller.on_read_complete(fresh);
                        black_box(controller.snapshot_prov().is_some());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

fn snapshot_readability_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_readability");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &regions in &[1_000usize, 10_000] {
        let real: Arc<dyn Provider + Send + Sync> =
            Arc::new(LiveLikeProvider::new_float_pairs(regions * 512));
        let snapshot = SnapshotProvider::new(Some(real), PageMap::new(), 0);
        group.throughput(Throughput::Elements(regions as u64));
        group.bench_with_input(
            BenchmarkId::new("missing_page_region_fallback", regions),
            &regions,
            |b, &regions| {
                b.iter(|| {
                    let mut readable = 0usize;
                    for i in 0..regions {
                        if snapshot.is_readable(black_box((i * 4096) as u64), 8) {
                            readable += 1;
                        }
                    }
                    black_box(readable);
                });
            },
        );
    }
    group.finish();

    let mut merge_group = c.benchmark_group("snapshot_merge_pages");
    merge_group.sample_size(10);
    merge_group.measurement_time(Duration::from_secs(2));
    for &regions in &[1_000usize, 10_000] {
        let real: Arc<dyn Provider + Send + Sync> =
            Arc::new(LiveLikeProvider::new_compose_pointer_hint_regions(regions));
        let snapshot = SnapshotProvider::new(Some(real), PageMap::new(), 0);
        let mut fresh = PageMap::new();
        fresh.insert(0, vec![0x5Au8; 4096].into());
        merge_group.throughput(Throughput::Elements(regions as u64));
        merge_group.bench_with_input(
            BenchmarkId::new("single_page_many_live_mappings", regions),
            &regions,
            |b, _| {
                b.iter(|| {
                    snapshot.merge_pages(black_box(&fresh), black_box(4096));
                    black_box(snapshot.size());
                });
            },
        );
    }
    merge_group.finish();
}

fn snapshot_read_fallback_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_read_fallback");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &reads in &[1_000usize, 10_000] {
        let real: Arc<dyn Provider + Send + Sync> =
            Arc::new(LiveLikeProvider::new_float_pairs(reads + 8));
        let snapshot = SnapshotProvider::new(Some(real), PageMap::new(), 0);
        group.throughput(Throughput::Elements(reads as u64));
        group.bench_with_input(
            BenchmarkId::new("missing_page_small_reads", reads),
            &reads,
            |b, &reads| {
                b.iter(|| {
                    let mut acc = 0u64;
                    let mut buf = [0u8; 8];
                    for i in 0..reads {
                        snapshot.read(black_box((i * 8) as u64), &mut buf);
                        acc ^= u64::from_le_bytes(buf);
                    }
                    black_box(acc);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("missing_page_unreadable_small_reads", reads),
            &reads,
            |b, &reads| {
                b.iter(|| {
                    let mut acc = 0u64;
                    let mut buf = [0u8; 8];
                    for i in 0..reads {
                        snapshot.read(black_box(0x1_0000_0000u64 + (i * 8) as u64), &mut buf);
                        acc ^= u64::from_le_bytes(buf);
                    }
                    black_box(acc);
                });
            },
        );
    }
    group.finish();
}

fn live_provider_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("live_provider_readability");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) else {
            group.finish();
            return;
        };
        let regions = provider.enumerate_regions();
        let readable: Vec<_> = regions
            .iter()
            .filter(|region| region.readable && region.size >= 8)
            .collect();
        if readable.is_empty() {
            group.finish();
            return;
        }

        for &checks in &[1_000usize, 10_000] {
            let addresses: Vec<u64> = (0..checks)
                .map(|i| {
                    let region = readable[i % readable.len()];
                    let span = region.size.saturating_sub(8).max(1);
                    region.base + ((i as u64).wrapping_mul(64) % span)
                })
                .collect();
            group.throughput(Throughput::Elements(checks as u64));
            group.bench_with_input(
                BenchmarkId::new("current_process_is_readable_hits", checks),
                &checks,
                |b, _| {
                    b.iter(|| {
                        let mut readable_count = 0usize;
                        for &addr in &addresses {
                            if provider.is_readable(black_box(addr), 8) {
                                readable_count += 1;
                            }
                        }
                        black_box(readable_count);
                    });
                },
            );
        }

        let invalid_base = regions
            .iter()
            .map(|region| region.base.saturating_add(region.size))
            .max()
            .unwrap_or(0)
            .saturating_add(0x1000_0000);
        for &checks in &[1_000usize, 10_000] {
            let addresses: Vec<u64> = (0..checks)
                .map(|i| invalid_base.saturating_add((i as u64).wrapping_mul(0x1000)))
                .collect();
            group.throughput(Throughput::Elements(checks as u64));
            group.bench_with_input(
                BenchmarkId::new("current_process_is_readable_misses", checks),
                &checks,
                |b, _| {
                    b.iter(|| {
                        let mut readable_count = 0usize;
                        for &addr in &addresses {
                            if provider.is_readable(black_box(addr), 8) {
                                readable_count += 1;
                            }
                        }
                        black_box(readable_count);
                    });
                },
            );
        }

        let modules = provider.enumerate_modules();
        let module_hits: Vec<_> = modules.iter().filter(|module| module.size > 8).collect();
        if !module_hits.is_empty() {
            for &lookups in &[1_000usize, 10_000] {
                let addresses: Vec<u64> = (0..lookups)
                    .map(|i| {
                        let module = module_hits[i % module_hits.len()];
                        let span = module.size.saturating_sub(8).max(1);
                        module.base + ((i as u64).wrapping_mul(64) % span)
                    })
                    .collect();
                group.throughput(Throughput::Elements(lookups as u64));
                group.bench_with_input(
                    BenchmarkId::new("current_process_get_symbol_hits", lookups),
                    &lookups,
                    |b, _| {
                        b.iter(|| {
                            let mut total = 0usize;
                            for &addr in &addresses {
                                total =
                                    total.wrapping_add(provider.get_symbol(black_box(addr)).len());
                            }
                            black_box(total);
                        });
                    },
                );
            }
        }
    }

    group.finish();
}

fn linear_module_symbol(modules: &[ModuleEntry], addr: u64) -> String {
    modules
        .iter()
        .find(|module| addr >= module.base && addr < module.base.saturating_add(module.size))
        .map(|module| format!("{}+0x{:x}", module.name, addr - module.base))
        .unwrap_or_default()
}

#[cfg(feature = "process-provider")]
fn readable_region_cache_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("readable_region_cache");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &regions_len in &[1_000usize, 10_000] {
        let ordered: Vec<_> = (0..regions_len)
            .map(|i| MemoryRegion {
                base: 0x1_0000 + (i as u64) * 0x4000,
                size: 0x3000,
                readable: i % 7 != 0,
                writable: i % 3 == 0,
                executable: i % 5 == 0,
                module_name: String::new(),
                region_type: RegionType::Private,
            })
            .collect();
        let mut reversed = ordered.clone();
        reversed.reverse();

        group.throughput(Throughput::Elements(regions_len as u64));
        group.bench_with_input(
            BenchmarkId::new("build_ordered_maps_sort_baseline", regions_len),
            &regions_len,
            |b, _| {
                b.iter(|| {
                    black_box(readable_ranges_sort_baseline(black_box(&ordered)));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("build_ordered_maps", regions_len),
            &regions_len,
            |b, _| {
                b.iter(|| {
                    black_box(bench_readable_ranges_from_regions(black_box(&ordered)));
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("build_unsorted_maps", regions_len),
            &regions_len,
            |b, _| {
                b.iter(|| {
                    black_box(bench_readable_ranges_from_regions(black_box(&reversed)));
                });
            },
        );
    }

    group.finish();
}

#[cfg(feature = "process-provider")]
fn readable_ranges_sort_baseline(regions: &[MemoryRegion]) -> Vec<(u64, u64)> {
    let mut ranges: Vec<_> = regions
        .iter()
        .filter(|region| region.readable && region.size > 0)
        .filter_map(|region| {
            let end = region.base.saturating_add(region.size);
            (end > region.base).then_some((region.base, end))
        })
        .collect();
    ranges.sort_by_key(|(start, _)| *start);

    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut() {
            if start <= *last_end {
                *last_end = (*last_end).max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

#[cfg(not(feature = "process-provider"))]
fn readable_region_cache_workloads(_c: &mut Criterion) {}

fn indexed_module_symbol(lookup: &ModuleLookup, addr: u64) -> String {
    lookup.symbol_for_addr_lower(addr)
}

fn module_lookup_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("module_lookup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &modules_len in &[1_000usize, 10_000] {
        let modules: Vec<_> = (0..modules_len)
            .map(|i| ModuleEntry {
                name: format!("module_{i:05}.dll"),
                full_path: format!(r"C:\Game\Bin\module_{i:05}.dll"),
                base: 0x1_0000 + (i as u64) * 0x2000,
                size: 0x1000,
            })
            .collect();
        let lookup = ModuleLookup::new(modules.clone());
        let addresses: Vec<u64> = (0..modules_len)
            .map(|i| modules[i].base + ((i as u64).wrapping_mul(17) % 0x1000))
            .collect();

        group.throughput(Throughput::Elements(modules_len as u64));
        group.bench_with_input(
            BenchmarkId::new("build_index", modules_len),
            &modules_len,
            |b, _| {
                b.iter(|| {
                    let lookup = ModuleLookup::new(black_box(modules.clone()));
                    black_box(lookup);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("linear_get_symbol", modules_len),
            &modules_len,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &addr in &addresses {
                        total = total.wrapping_add(
                            linear_module_symbol(black_box(&modules), black_box(addr)).len(),
                        );
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("indexed_get_symbol", modules_len),
            &modules_len,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &addr in &addresses {
                        total = total.wrapping_add(
                            indexed_module_symbol(black_box(&lookup), black_box(addr)).len(),
                        );
                    }
                    black_box(total);
                });
            },
        );
    }

    group.finish();
}

fn modules_panel_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("modules_panel");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &modules_len in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_compose_pointer_hint_regions(modules_len);
        let cached_rows = build_module_rows(&provider.enumerate_modules());
        group.throughput(Throughput::Elements(modules_len as u64));
        group.bench_with_input(
            BenchmarkId::new("render_reenumerate_modules", modules_len),
            &modules_len,
            |b, _| {
                b.iter(|| {
                    let modules = provider.enumerate_modules();
                    let rows = build_module_rows(black_box(&modules));
                    let total = rows.iter().fold(0usize, |acc, row| {
                        acc.wrapping_add(row.name.len())
                            .wrapping_add(row.base_text.len())
                            .wrapping_add(row.size_text.len())
                    });
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("render_cached_rows", modules_len),
            &modules_len,
            |b, _| {
                b.iter(|| {
                    let total = black_box(&cached_rows).iter().fold(0usize, |acc, row| {
                        acc.wrapping_add(row.name.len())
                            .wrapping_add(row.base_text.len())
                            .wrapping_add(row.size_text.len())
                    });
                    black_box(total);
                });
            },
        );
    }

    group.finish();
}

#[derive(Clone)]
struct TargetPanelBenchDetails {
    modules: Vec<ModuleEntry>,
    region_count: usize,
    readable_regions: usize,
    writable_regions: usize,
    executable_regions: usize,
}

impl TargetPanelBenchDetails {
    fn from_provider(provider: &dyn Provider) -> Self {
        let mut modules = provider.enumerate_modules();
        modules.sort_by_key(|m| m.base);
        let regions = provider.enumerate_regions();
        let readable_regions = regions.iter().filter(|r| r.readable).count();
        let writable_regions = regions.iter().filter(|r| r.writable).count();
        let executable_regions = regions.iter().filter(|r| r.executable).count();
        Self {
            modules,
            region_count: regions.len(),
            readable_regions,
            writable_regions,
            executable_regions,
        }
    }

    fn metric(&self) -> usize {
        let module_text_len = self.modules.iter().take(6).fold(0usize, |acc, module| {
            acc.wrapping_add(module.name.len())
                .wrapping_add(format!("0x{:X}", module.base).len())
                .wrapping_add(format!("0x{:X}", module.size).len())
        });
        self.modules
            .len()
            .wrapping_add(self.region_count)
            .wrapping_add(self.readable_regions)
            .wrapping_add(self.writable_regions)
            .wrapping_add(self.executable_regions)
            .wrapping_add(module_text_len)
    }
}

#[derive(Default)]
struct TargetPanelBenchState {
    provider: Option<Arc<dyn Provider + Send + Sync>>,
    summary_label: String,
    details: Option<TargetPanelBenchDetails>,
}

impl TargetPanelBenchState {
    fn set_target(&mut self, provider: Arc<dyn Provider + Send + Sync>, summary_label: &str) {
        self.provider = Some(provider);
        self.summary_label.clear();
        self.summary_label.push_str(summary_label);
        self.details = None;
    }

    fn set_target_eager(&mut self, provider: Arc<dyn Provider + Send + Sync>, summary_label: &str) {
        self.details = Some(TargetPanelBenchDetails::from_provider(provider.as_ref()));
        self.provider = Some(provider);
        self.summary_label.clear();
        self.summary_label.push_str(summary_label);
    }

    fn metric(&self) -> usize {
        self.provider
            .as_ref()
            .map_or(0, Arc::strong_count)
            .wrapping_add(self.summary_label.len())
            .wrapping_add(
                self.details
                    .as_ref()
                    .map_or(0, TargetPanelBenchDetails::metric),
            )
    }
}

fn target_panel_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_panel");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &mappings in &[1_000usize, 10_000] {
        let provider: Arc<dyn Provider + Send + Sync> =
            Arc::new(LiveLikeProvider::new_compose_pointer_hint_regions(mappings));
        let cached_details = TargetPanelBenchDetails::from_provider(provider.as_ref());
        let summary_label = format!("LiveLikeProcess:{mappings}");
        group.throughput(Throughput::Elements(mappings as u64));
        group.bench_with_input(
            BenchmarkId::new("render_enumerate_details", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let details =
                        TargetPanelBenchDetails::from_provider(black_box(provider.as_ref()));
                    black_box(details.metric());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("render_cached_details", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    black_box(cached_details.metric());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("source_switch_set_target", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let mut state = TargetPanelBenchState::default();
                    state.set_target(Arc::clone(black_box(&provider)), black_box(&summary_label));
                    black_box(state.metric());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("source_switch_eager_details", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let mut state = TargetPanelBenchState::default();
                    state.set_target_eager(
                        Arc::clone(black_box(&provider)),
                        black_box(&summary_label),
                    );
                    black_box(state.metric());
                });
            },
        );
    }

    group.finish();
}

fn workspace_model_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_model");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &fields in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(fields, NodeKind::UInt32);
        let docs = [WorkspaceDoc {
            doc: DocId::from_raw(1),
            tree: &tree,
        }];
        group.throughput(Throughput::Elements(fields as u64));
        group.bench_with_input(
            BenchmarkId::new("single_large_struct", fields),
            &fields,
            |b, _| {
                b.iter(|| {
                    let model =
                        WorkspaceModel::build(black_box(&docs), black_box(&[]), black_box(&[]));
                    black_box(model.rows.len());
                });
            },
        );
        let model = WorkspaceModel::build(&docs, &[], &[]);
        let tail_query = format!("field_{:05}", fields.saturating_sub(1));
        group.bench_with_input(
            BenchmarkId::new("filter_tail_field", fields),
            &fields,
            |b, _| {
                b.iter(|| {
                    let filtered = model.filtered(black_box(&tail_query));
                    black_box(filtered.rows.len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("filter_type_name", fields),
            &fields,
            |b, _| {
                b.iter(|| {
                    let filtered = model.filtered(black_box("root"));
                    black_box(filtered.rows.len());
                });
            },
        );
    }

    group.finish();
}

fn hover_memory_preview_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("hover_memory_preview");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &rows in &[1_000usize, 10_000] {
        let (provider, preview) = LiveLikeProvider::new_memory_preview_targets(rows);
        let regions = provider.enumerate_regions();
        let modules = provider.enumerate_modules();
        let orders = reclass::ui::editor::bench_memory_preview_lookup_orders(&regions, &modules);
        let mut unsorted_regions = regions.clone();
        let mut unsorted_modules = modules.clone();
        unsorted_regions.reverse();
        unsorted_modules.reverse();
        let unsorted_orders = reclass::ui::editor::bench_memory_preview_lookup_orders(
            &unsorted_regions,
            &unsorted_modules,
        );
        group.throughput(Throughput::Elements(rows as u64));
        group.bench_with_input(
            BenchmarkId::new("pointer_rows_with_regions_modules", rows),
            &rows,
            |b, &rows| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_memory_preview_rows_with_cached_lookup(
                        black_box(&provider),
                        8,
                        rows,
                        black_box(&preview),
                        black_box(&regions),
                        black_box(&modules),
                        black_box(&orders),
                    );
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("pointer_rows_unsorted_regions_modules", rows),
            &rows,
            |b, &rows| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_memory_preview_rows_with_cached_lookup(
                        black_box(&provider),
                        8,
                        rows,
                        black_box(&preview),
                        black_box(&unsorted_regions),
                        black_box(&unsorted_modules),
                        black_box(&unsorted_orders),
                    );
                    black_box(total);
                });
            },
        );

        let repeated_target = 4096u64;
        let repeated_preview: Vec<u8> = (0..rows)
            .flat_map(|_| repeated_target.to_le_bytes())
            .collect();
        let symbol_provider = SymbolWorkProvider::new(provider.clone(), 128);
        group.bench_with_input(
            BenchmarkId::new("pointer_rows_duplicate_symbol_targets", rows),
            &rows,
            |b, &rows| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_memory_preview_rows_with_cached_lookup(
                        black_box(&symbol_provider),
                        8,
                        rows,
                        black_box(&repeated_preview),
                        black_box(&regions),
                        black_box(&modules),
                        black_box(&orders),
                    );
                    black_box((total, symbol_provider.symbol_calls()));
                });
            },
        );
    }
    for &mappings in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_compose_pointer_hint_regions(mappings);
        let regions = provider.enumerate_regions();
        let modules = provider.enumerate_modules();
        let orders = reclass::ui::editor::bench_memory_preview_lookup_orders(&regions, &modules);
        let pointer_addr = provider.base() + (mappings.saturating_sub(1) * 8) as u64;
        let rows = 64usize;
        group.throughput(Throughput::Elements(mappings as u64));
        group.bench_with_input(
            BenchmarkId::new("full_pointer_popup_many_mappings", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let total =
                        reclass::ui::editor::bench_pointer_memory_preview_with_cached_lookup(
                            black_box(&provider),
                            black_box(pointer_addr),
                            8,
                            rows,
                            black_box(&regions),
                            black_box(&modules),
                            black_box(&orders),
                        );
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("full_pointer_popup_reenumerate_many_mappings", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let regions = provider.enumerate_regions();
                    let modules = provider.enumerate_modules();
                    let total = reclass::ui::editor::bench_pointer_memory_preview_with_maps(
                        black_box(&provider),
                        black_box(pointer_addr),
                        8,
                        rows,
                        black_box(&regions),
                        black_box(&modules),
                    );
                    black_box(total);
                });
            },
        );
        let hover_rows = 64usize.min(mappings);
        let hover_start = mappings.saturating_sub(hover_rows);
        let pointer_addrs: Vec<u64> = (hover_start..mappings)
            .map(|i| provider.base() + (i * 8) as u64)
            .collect();
        group.throughput(Throughput::Elements(hover_rows as u64));
        group.bench_with_input(
            BenchmarkId::new("full_pointer_popup_hover_rows_cached_maps", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &addr in &pointer_addrs {
                        total +=
                            reclass::ui::editor::bench_pointer_memory_preview_with_cached_lookup(
                                black_box(&provider),
                                black_box(addr),
                                8,
                                rows,
                                black_box(&regions),
                                black_box(&modules),
                                black_box(&orders),
                            );
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("full_pointer_popup_hover_rows_reenumerate_maps", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for &addr in &pointer_addrs {
                        let regions = provider.enumerate_regions();
                        let modules = provider.enumerate_modules();
                        total += reclass::ui::editor::bench_pointer_memory_preview_with_maps(
                            black_box(&provider),
                            black_box(addr),
                            8,
                            rows,
                            black_box(&regions),
                            black_box(&modules),
                        );
                    }
                    black_box(total);
                });
            },
        );
    }
    for &(requested_rows, actual_rows) in &[(64usize, 8usize), (512, 8)] {
        let pointer_addr = 0u64;
        let target_addr = 8u64;
        let provider = TailLimitedProvider::new(pointer_addr, target_addr, actual_rows * 8);
        group.throughput(Throughput::Elements(requested_rows as u64));
        group.bench_with_input(
            BenchmarkId::new(
                format!("partial_tail_no_region_map_actual_{actual_rows}"),
                requested_rows,
            ),
            &requested_rows,
            |b, &requested_rows| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_pointer_memory_preview_with_maps(
                        black_box(&provider),
                        black_box(pointer_addr),
                        8,
                        requested_rows,
                        black_box(&[]),
                        black_box(&[]),
                    );
                    black_box(total);
                });
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            for &rows in &[64usize, 256] {
                let targets: Vec<[u8; 8]> = (0..rows)
                    .map(|i| {
                        [
                            i as u8,
                            i.wrapping_mul(3) as u8,
                            i.wrapping_mul(5) as u8,
                            i.wrapping_mul(7) as u8,
                            0,
                            0,
                            0,
                            0,
                        ]
                    })
                    .collect();
                let preview: Vec<u8> = targets
                    .iter()
                    .map(|row| row.as_ptr() as u64)
                    .flat_map(u64::to_le_bytes)
                    .collect();
                let regions = provider.enumerate_regions();
                let modules = provider.enumerate_modules();
                let orders =
                    reclass::ui::editor::bench_memory_preview_lookup_orders(&regions, &modules);
                group.throughput(Throughput::Elements(rows as u64));
                group.bench_with_input(
                    BenchmarkId::new("pointer_rows_current_process_provider", rows),
                    &rows,
                    |b, &rows| {
                        b.iter(|| {
                            black_box(&targets);
                            let total =
                                reclass::ui::editor::bench_memory_preview_rows_with_cached_lookup(
                                    black_box(&provider),
                                    8,
                                    rows,
                                    black_box(&preview),
                                    black_box(&regions),
                                    black_box(&modules),
                                    black_box(&orders),
                                );
                            black_box(total);
                        });
                    },
                );
            }
        }
    }
    group.finish();
}

fn hover_struct_preview_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("hover_struct_preview");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000, 50_000] {
        let tree = flat_tree(nodes, NodeKind::Hex64);
        let provider = patterned_provider((nodes + 8) * 8);
        let ref_id = tree.nodes[0].id;
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("compose_ref_at_base", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose_with_symbols_at_base(
                        black_box(&tree),
                        black_box(&provider),
                        black_box(ref_id),
                        black_box(0x5000_0000),
                        false,
                        false,
                        true,
                        true,
                        true,
                        None,
                        true,
                        true,
                    );
                    black_box(result.meta.len());
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("compose_ref_preview_limited", nodes),
            &nodes,
            |b, _| {
                b.iter(|| {
                    let result = compose::compose_preview_with_symbols_at_base(
                        black_box(&tree),
                        black_box(&provider),
                        black_box(ref_id),
                        black_box(0x5000_0000),
                        false,
                        false,
                        true,
                        true,
                        true,
                        None,
                        true,
                        true,
                        6,
                    );
                    black_box(result.meta.len());
                });
            },
        );
    }
    for &mappings in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_compose_pointer_hint_regions(mappings);
        let regions = provider.enumerate_regions();
        let pointer_addr = provider.base() + (mappings.saturating_sub(1) * 8) as u64;
        group.throughput(Throughput::Elements(mappings as u64));
        group.bench_with_input(
            BenchmarkId::new("preflight_reenumerate_many_mappings", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_struct_preview_preflight_reenumerate(
                        black_box(&provider),
                        black_box(pointer_addr),
                        8,
                    );
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("preflight_cached_regions_many_mappings", mappings),
            &mappings,
            |b, _| {
                b.iter(|| {
                    let total = reclass::ui::editor::bench_struct_preview_preflight_with_regions(
                        black_box(&provider),
                        black_box(pointer_addr),
                        8,
                        black_box(&regions),
                    );
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

fn change_all_seed(rows: usize) -> (Vec<ScanResult>, Vec<(u64, Option<Vec<u8>>)>) {
    let results: Vec<_> = (0..rows)
        .map(|i| ScanResult {
            address: 0x0000_7FF6_0000_0000 + (i as u64) * 8,
            scan_value: (i as u32).to_le_bytes().to_vec().into(),
            previous_value: ((i as u32) ^ 0xFFFF).to_le_bytes().to_vec().into(),
            region_module: String::new(),
        })
        .collect();
    let writebacks: Vec<_> = results
        .iter()
        .map(|result| (result.address, Some(0x1234_5678u32.to_le_bytes().to_vec())))
        .collect();
    (results, writebacks)
}

fn apply_change_all_linear_baseline(
    results: &mut [ScanResult],
    writebacks: Vec<(u64, Option<Vec<u8>>)>,
) -> usize {
    let mut wrote = 0usize;
    for (addr, new_bytes) in writebacks {
        if let Some(bytes) = new_bytes {
            if let Some(result) = results.iter_mut().find(|result| result.address == addr) {
                result.scan_value = bytes.into();
            }
            wrote += 1;
        }
    }
    wrote
}

fn scanner_change_all_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("scanner_change_all_apply");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &rows in &[1_000usize, 10_000, 50_000] {
        let (seed_results, ordered_writebacks) = change_all_seed(rows);
        let mut reversed_writebacks = ordered_writebacks.clone();
        reversed_writebacks.reverse();

        group.throughput(Throughput::Elements(rows as u64));
        if rows <= 10_000 {
            group.bench_with_input(
                BenchmarkId::new("linear_ordered_baseline", rows),
                &rows,
                |b, _| {
                    b.iter_batched(
                        || (seed_results.clone(), ordered_writebacks.clone()),
                        |(mut results, writebacks)| {
                            black_box(apply_change_all_linear_baseline(
                                black_box(&mut results),
                                black_box(writebacks),
                            ));
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
        group.bench_with_input(BenchmarkId::new("ordered_direct", rows), &rows, |b, _| {
            b.iter_batched(
                || (seed_results.clone(), ordered_writebacks.clone()),
                |(mut results, writebacks)| {
                    black_box(apply_change_all_results(
                        black_box(&mut results),
                        black_box(writebacks),
                    ));
                },
                BatchSize::LargeInput,
            );
        });
        group.bench_with_input(BenchmarkId::new("indexed_reversed", rows), &rows, |b, _| {
            b.iter_batched(
                || (seed_results.clone(), reversed_writebacks.clone()),
                |(mut results, writebacks)| {
                    black_box(apply_change_all_results(
                        black_box(&mut results),
                        black_box(writebacks),
                    ));
                },
                BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

fn scanner_table_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("scanner_table");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    let form = {
        let mut form = reclass::ui::panels::scannerpanel::ScannerForm::default();
        form.value_type = ValueType::Int32;
        form
    };
    for &rows in &[1_000usize, 10_000] {
        let results: Vec<_> = (0..rows)
            .map(|i| ScanResult {
                address: 0x0000_7FF6_0000_0000 + (i as u64) * 8,
                scan_value: (i as u32).to_le_bytes().to_vec().into(),
                previous_value: Default::default(),
                region_module: format!("module_{:03}.dll", i % 32),
            })
            .collect();
        group.throughput(Throughput::Elements(rows as u64));
        group.bench_with_input(
            BenchmarkId::new("filter_rebuild_each_keypress", rows),
            &rows,
            |b, _| {
                b.iter(|| {
                    let mut total = 0usize;
                    for query in [
                        "m",
                        "mo",
                        "mod",
                        "module",
                        "module_0",
                        "module_00",
                        "7ff6",
                        "42",
                    ] {
                        total = total.wrapping_add(bench_scanner_table_refresh(
                            black_box(&results),
                            black_box(&form),
                            black_box(query),
                        ));
                    }
                    black_box(total);
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("filter_cached_rows_keypresses", rows),
            &rows,
            |b, _| {
                b.iter(|| {
                    let total = bench_scanner_table_filter_cached(
                        black_box(&results),
                        black_box(&form),
                        black_box("module_00"),
                    );
                    black_box(total);
                });
            },
        );
    }
    group.finish();
}

fn scanner_workloads(c: &mut Criterion) {
    let mut scanner_group = c.benchmark_group("scanner_loops");
    scanner_group.sample_size(10);
    scanner_group.measurement_time(Duration::from_secs(2));
    let provider = LiveLikeProvider::new_float_pairs(1024 * 1024);
    let request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: vec![0x13, 0x37, 0x42, 0x99],
        mask: vec![0xff; 4],
        max_results: 50_000,
        ..ScanRequest::default()
    };
    let abort = AtomicBool::new(false);
    let observer = NullObserver;
    scanner_group.throughput(Throughput::Bytes(8 * 1024 * 1024));
    scanner_group.bench_function("exact_8mib", |b| {
        b.iter(|| {
            let results = run_scan(black_box(&provider), &request, &abort, &observer);
            black_box(results.len());
        });
    });
    let masked_provider = LiveLikeProvider::new_masked_signature_hits(8 * 1024 * 1024, 256);
    let masked_request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: vec![0x48, 0x8B, 0x00, 0x05, 0x00, 0x00, 0x89, 0xD8],
        mask: vec![0xff, 0xff, 0x00, 0xff, 0x00, 0x00, 0xff, 0xff],
        max_results: 50_000,
        ..ScanRequest::default()
    };
    scanner_group.throughput(Throughput::Bytes(8 * 1024 * 1024));
    scanner_group.bench_function("masked_signature_8mib", |b| {
        b.iter(|| {
            let results = run_scan(
                black_box(&masked_provider),
                black_box(&masked_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    let dense_pattern = [0x13, 0x37, 0x42, 0x99];
    let dense_provider = LiveLikeProvider::new_repeated_pattern(8 * 1024 * 1024, &dense_pattern);
    let dense_request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: dense_pattern.to_vec(),
        mask: vec![0xff; dense_pattern.len()],
        max_results: 50_000,
        ..ScanRequest::default()
    };
    scanner_group.throughput(Throughput::Elements(dense_request.max_results as u64));
    scanner_group.bench_function("exact_dense_50k_hits", |b| {
        b.iter(|| {
            let results = run_scan(
                black_box(&dense_provider),
                black_box(&dense_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    let mut dense_module_provider =
        LiveLikeProvider::new_repeated_pattern(8 * 1024 * 1024, &dense_pattern);
    for (i, region) in dense_module_provider.regions.iter_mut().enumerate() {
        region.module_name = format!("module_{i:05}.dll");
        region.region_type = RegionType::Image;
    }
    scanner_group.throughput(Throughput::Elements(dense_request.max_results as u64));
    scanner_group.bench_function("exact_dense_50k_hits_module_context", |b| {
        b.iter(|| {
            let results = run_scan(
                black_box(&dense_module_provider),
                black_box(&dense_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    let constrained_provider = LiveLikeProvider::new_float_pairs(1024 * 1024);
    let constrained_regions: Vec<_> = (0..512u64)
        .map(|i| MemoryRegion {
            base: i * 0x4000,
            size: 0x1000,
            readable: true,
            writable: true,
            executable: false,
            module_name: String::new(),
            region_type: RegionType::Private,
        })
        .collect();
    let constrained_request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: vec![0x13, 0x37],
        mask: vec![0xff; 2],
        max_results: 50_000,
        constrain_regions: (0..512u64)
            .step_by(32)
            .map(|i| AddressRange {
                start: i * 0x4000 + 0x100,
                end: i * 0x4000 + 0x900,
            })
            .collect(),
        ..ScanRequest::default()
    };
    scanner_group.throughput(Throughput::Elements(constrained_regions.len() as u64));
    scanner_group.bench_function("constrained_regions_512x16", |b| {
        b.iter(|| {
            let results = reclass::scanner::run_scan_in_regions(
                black_box(&constrained_provider),
                black_box(constrained_regions.clone()),
                black_box(&constrained_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    let large_constrained_regions: Vec<_> = (0..10_000u64)
        .map(|i| MemoryRegion {
            base: i * 0x4000,
            size: 0x1000,
            readable: true,
            writable: true,
            executable: false,
            module_name: String::new(),
            region_type: RegionType::Private,
        })
        .collect();
    let large_constrained_request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: vec![0x13, 0x37],
        mask: vec![0xff; 2],
        max_results: 50_000,
        constrain_regions: (0..10_000u64)
            .step_by(2)
            .map(|i| AddressRange {
                start: i * 0x4000 + 0x100,
                end: i * 0x4000 + 0x900,
            })
            .collect(),
        ..ScanRequest::default()
    };
    scanner_group.throughput(Throughput::Elements(large_constrained_regions.len() as u64));
    scanner_group.bench_function("constrained_regions_10000x5000", |b| {
        b.iter(|| {
            let results = reclass::scanner::run_scan_in_regions(
                black_box(&constrained_provider),
                black_box(large_constrained_regions.clone()),
                black_box(&large_constrained_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    let skip_system_provider = LiveLikeProvider::new_repeated_pattern(10_000 * 4096, &[0xAA]);
    let skip_system_regions: Vec<_> = (0..10_000u64)
        .map(|i| MemoryRegion {
            base: i * 4096,
            size: 4096,
            readable: true,
            writable: i % 3 != 0,
            executable: i % 2 == 0,
            module_name: if i % 2 == 0 {
                "kernel32.dll".to_string()
            } else {
                format!("game_{i:05}.dll")
            },
            region_type: if i % 2 == 0 {
                RegionType::Image
            } else {
                RegionType::Private
            },
        })
        .collect();
    let skip_system_request = ScanRequest {
        condition: ScanCondition::ExactValue,
        pattern: vec![0x13, 0x37],
        mask: vec![0xff; 2],
        max_results: 50_000,
        skip_system_modules: true,
        ..ScanRequest::default()
    };
    scanner_group.throughput(Throughput::Elements(skip_system_regions.len() as u64));
    scanner_group.bench_function("skip_system_modules_10000_regions", |b| {
        b.iter(|| {
            let results = reclass::scanner::run_scan_in_regions(
                black_box(&skip_system_provider),
                black_box(skip_system_regions.clone()),
                black_box(&skip_system_request),
                &abort,
                &observer,
            );
            black_box(results.len());
        });
    });
    for &hits in &[10_000usize, 50_000] {
        let provider = LiveLikeProvider::new_float_pairs(hits);
        let seed: Vec<_> = (0..hits)
            .map(|i| ScanResult {
                address: (i * 8) as u64,
                region_module: String::new(),
                scan_value: vec![0u8; 4].into(),
                previous_value: Default::default(),
            })
            .collect();
        scanner_group.throughput(Throughput::Elements(hits as u64));
        scanner_group.bench_with_input(
            BenchmarkId::new("rescan_changed_live_hits", hits),
            &hits,
            |b, _| {
                b.iter_batched(
                    || seed.clone(),
                    |seed| {
                        let results = run_rescan(
                            black_box(&provider),
                            seed,
                            4,
                            ScanCondition::Changed,
                            ValueType::Int32,
                            &[],
                            &[],
                            &[],
                            &abort,
                            &observer,
                        );
                        black_box(results.len());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    for &hits in &[10_000usize, 50_000] {
        let provider = LiveLikeProvider::new_rescan_i32_rows(hits);
        let seed: Vec<_> = (0..hits)
            .map(|i| ScanResult {
                address: provider.base() + (i * 4) as u64,
                region_module: String::new(),
                scan_value: vec![0u8; 4].into(),
                previous_value: Default::default(),
            })
            .collect();
        let pattern = 0x1234_5678u32.to_le_bytes();
        let mask = [0xff; 4];
        scanner_group.throughput(Throughput::Elements(hits as u64));
        scanner_group.bench_with_input(
            BenchmarkId::new("rescan_exact_half_matches_live_hits", hits),
            &hits,
            |b, _| {
                b.iter_batched(
                    || seed.clone(),
                    |seed| {
                        let results = run_rescan(
                            black_box(&provider),
                            seed,
                            4,
                            ScanCondition::ExactValue,
                            ValueType::UInt32,
                            black_box(&pattern),
                            black_box(&mask),
                            &[],
                            &abort,
                            &observer,
                        );
                        black_box(results.len());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    for &hits in &[10_000usize] {
        let provider = LiveLikeProvider::new_rescan_i32_pages(hits);
        let seed: Vec<_> = (0..hits)
            .map(|i| ScanResult {
                address: provider.base() + (i * 4096) as u64,
                region_module: String::new(),
                scan_value: vec![0u8; 4].into(),
                previous_value: Default::default(),
            })
            .collect();
        let pattern = 0x1234_5678u32.to_le_bytes();
        let mask = [0xff; 4];
        scanner_group.throughput(Throughput::Elements(hits as u64));
        scanner_group.bench_with_input(
            BenchmarkId::new("rescan_exact_sparse_pages_live_hits", hits),
            &hits,
            |b, _| {
                b.iter_batched(
                    || seed.clone(),
                    |seed| {
                        let results = run_rescan(
                            black_box(&provider),
                            seed,
                            4,
                            ScanCondition::ExactValue,
                            ValueType::UInt32,
                            black_box(&pattern),
                            black_box(&mask),
                            &[],
                            &abort,
                            &observer,
                        );
                        black_box(results.len());
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let hits = 10_000usize;
            let page = K_PAGE_SIZE as usize;
            let mut data = vec![0u8; hits.saturating_mul(page)];
            for i in 0..hits {
                let value = if i % 2 == 0 {
                    0x1234_5678u32
                } else {
                    0x9ABC_DEF0u32
                };
                data[i * page..i * page + 4].copy_from_slice(&value.to_le_bytes());
            }
            let base = data.as_ptr() as u64;
            let seed: Vec<_> = (0..hits)
                .map(|i| ScanResult {
                    address: base + (i * page) as u64,
                    region_module: String::new(),
                    scan_value: vec![0u8; 4].into(),
                    previous_value: Default::default(),
                })
                .collect();
            let pattern = 0x1234_5678u32.to_le_bytes();
            let mask = [0xff; 4];
            let no_coalesce = NoCoalescedRescanProvider { inner: &provider };
            scanner_group.throughput(Throughput::Elements(hits as u64));
            scanner_group.bench_function(
                "rescan_exact_sparse_pages_current_process_per_hit/10000",
                |b| {
                    b.iter_batched(
                        || seed.clone(),
                        |seed| {
                            black_box(&data);
                            let results = run_rescan(
                                black_box(&no_coalesce),
                                seed,
                                4,
                                ScanCondition::ExactValue,
                                ValueType::UInt32,
                                black_box(&pattern),
                                black_box(&mask),
                                &[],
                                &abort,
                                &observer,
                            );
                            black_box(results.len());
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
            scanner_group.bench_function(
                "rescan_exact_sparse_pages_current_process_coalesced/10000",
                |b| {
                    b.iter_batched(
                        || seed.clone(),
                        |seed| {
                            black_box(&data);
                            let results = run_rescan(
                                black_box(&provider),
                                seed,
                                4,
                                ScanCondition::ExactValue,
                                ValueType::UInt32,
                                black_box(&pattern),
                                black_box(&mask),
                                &[],
                                &abort,
                                &observer,
                            );
                            black_box(results.len());
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
    }
    scanner_group.finish();
}

fn pointer_chain_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("pointer_chain_lookup");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    for &records in &[10_000usize, 50_000] {
        let target = 0x8000_0000u64;
        let pointer_records: Vec<_> = (0..records)
            .map(|i| PointerRecord {
                address: 0x1000_0000 + (i as u64) * 8,
                points_to: target.saturating_sub((i % 16) as u64),
            })
            .collect();
        let map = PointerMap::from_records(
            8,
            vec![AddressRange {
                start: 0x1000_0000,
                end: target + 0x1000,
            }],
            pointer_records,
            PointerMapStats::default(),
        );
        let request = PointerChainRequest {
            targets: vec![target],
            max_depth: 1,
            max_offset: 0x10,
            max_results: 10,
        };
        group.throughput(Throughput::Elements(records as u64));
        group.bench_with_input(
            BenchmarkId::new("many_direct_candidates_capped", records),
            &records,
            |b, _| {
                b.iter(|| {
                    let result = find_pointer_chains(
                        black_box(&map),
                        black_box(&request),
                        &AtomicBool::new(false),
                    );
                    black_box((result.chains.len(), result.truncated));
                });
            },
        );
    }

    group.finish();
}

fn formatting_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_pointer_deref");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_pointer_chains(nodes);
        let node = Node {
            kind: NodeKind::Pointer64,
            ptr_depth: 2,
            element_kind: NodeKind::Int32,
            ..Node::default()
        };
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("primitive_pointer_deref_live_like", nodes),
            &nodes,
            |b, &nodes| {
                b.iter(|| {
                    let mut len = 0usize;
                    for i in 0..nodes {
                        let value =
                            format::read_value(&node, black_box(&provider), (i * 8) as u64, 0);
                        len = len.wrapping_add(value.len());
                    }
                    black_box(len);
                });
            },
        );
    }
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_vec4_rows(nodes);
        let node = Node {
            kind: NodeKind::Vec4,
            ..Node::default()
        };
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("vec4_live_like", nodes),
            &nodes,
            |b, &nodes| {
                b.iter(|| {
                    let mut len = 0usize;
                    for i in 0..nodes {
                        let value =
                            format::read_value(&node, black_box(&provider), (i * 16) as u64, 0);
                        len = len.wrapping_add(value.len());
                    }
                    black_box(len);
                });
            },
        );
    }
    for &nodes in &[1_000usize, 10_000] {
        let provider = LiveLikeProvider::new_vec4_rows(nodes * 4);
        let node = Node {
            kind: NodeKind::Mat4x4,
            ..Node::default()
        };
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(
            BenchmarkId::new("mat4x4_rows_live_like", nodes),
            &nodes,
            |b, &nodes| {
                b.iter(|| {
                    let mut len = 0usize;
                    for i in 0..nodes {
                        let base = (i * 64) as u64;
                        for row in 0..4 {
                            let value = format::read_value(&node, black_box(&provider), base, row);
                            len = len.wrapping_add(value.len());
                        }
                    }
                    black_box(len);
                });
            },
        );
    }
    #[cfg(all(target_os = "linux", feature = "process-provider"))]
    {
        if let Ok(provider) = LocalProcessProvider::attach(&std::process::id().to_string()) {
            let rows: Vec<[f32; 4]> = (0..1_000usize)
                .map(|i| {
                    [
                        1.0 + (i % 17) as f32,
                        2.0 + (i % 19) as f32,
                        3.0 + (i % 23) as f32,
                        4.0 + (i % 29) as f32,
                    ]
                })
                .collect();
            let base = rows.as_ptr() as u64;
            let node = Node {
                kind: NodeKind::Vec4,
                ..Node::default()
            };
            group.throughput(Throughput::Elements(rows.len() as u64));
            group.bench_function("vec4_current_process_provider/1000", |b| {
                b.iter(|| {
                    black_box(&rows);
                    let mut len = 0usize;
                    for i in 0..rows.len() {
                        let value = format::read_value(
                            &node,
                            black_box(&provider),
                            base + (i * 16) as u64,
                            0,
                        );
                        len = len.wrapping_add(value.len());
                    }
                    black_box(len);
                });
            });

            let matrices: Vec<[[f32; 4]; 4]> = (0..1_000usize)
                .map(|i| {
                    [
                        [
                            1.0 + (i % 17) as f32,
                            2.0 + (i % 19) as f32,
                            3.0 + (i % 23) as f32,
                            4.0 + (i % 29) as f32,
                        ],
                        [
                            5.0 + (i % 31) as f32,
                            6.0 + (i % 37) as f32,
                            7.0 + (i % 41) as f32,
                            8.0 + (i % 43) as f32,
                        ],
                        [
                            9.0 + (i % 47) as f32,
                            10.0 + (i % 53) as f32,
                            11.0 + (i % 59) as f32,
                            12.0 + (i % 61) as f32,
                        ],
                        [
                            13.0 + (i % 67) as f32,
                            14.0 + (i % 71) as f32,
                            15.0 + (i % 73) as f32,
                            16.0 + (i % 79) as f32,
                        ],
                    ]
                })
                .collect();
            let mat_base = matrices.as_ptr() as u64;
            let mat_node = Node {
                kind: NodeKind::Mat4x4,
                ..Node::default()
            };
            group.throughput(Throughput::Elements(matrices.len() as u64));
            group.bench_function("mat4x4_current_process_provider/1000", |b| {
                b.iter(|| {
                    black_box(&matrices);
                    let mut len = 0usize;
                    for i in 0..matrices.len() {
                        let base = mat_base + (i * 64) as u64;
                        for row in 0..4 {
                            let value =
                                format::read_value(&mat_node, black_box(&provider), base, row);
                            len = len.wrapping_add(value.len());
                        }
                    }
                    black_box(len);
                });
            });
            group.bench_function("mat4x4_current_process_provider_old_lane_reads/1000", |b| {
                b.iter(|| {
                    black_box(&matrices);
                    let mut len = 0usize;
                    for i in 0..matrices.len() {
                        let base = mat_base + (i * 64) as u64;
                        for row in 0..4 {
                            let mut line = format!("row{row} [");
                            for col in 0..4 {
                                if col > 0 {
                                    line.push_str(", ");
                                }
                                let addr = base + ((row * 4 + col) as u64) * 4;
                                line.push_str(&format::fmt_float(provider.read_f32(addr)));
                            }
                            line.push(']');
                            len = len.wrapping_add(line.len());
                        }
                    }
                    black_box(len);
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    compose_workloads,
    tree_child_access_workloads,
    tree_id_lookup_workloads,
    tree_selection_normalization_workloads,
    statusbar_workloads,
    editor_line_text_workloads,
    editor_node_line_lookup_workloads,
    editor_find_highlight_workloads,
    editor_find_search_workloads,
    editor_selection_workloads,
    editor_minimap_workloads,
    type_hint_workloads,
    refresh_extent_workloads,
    append_growth_workloads,
    changed_page_refresh_workloads,
    refresh_page_plan_workloads,
    refresh_read_pages_workloads,
    value_tracking_refresh_workloads,
    editor_style_runs_workloads,
    editor_static_row_cache_workloads,
    permanent_page_workloads,
    snapshot_readability_workloads,
    snapshot_read_fallback_workloads,
    live_provider_workloads,
    readable_region_cache_workloads,
    module_lookup_workloads,
    modules_panel_workloads,
    target_panel_workloads,
    workspace_model_workloads,
    hover_memory_preview_workloads,
    hover_struct_preview_workloads,
    scanner_change_all_workloads,
    scanner_table_workloads,
    scanner_workloads,
    pointer_chain_workloads,
    rtti_workloads,
    compose_live_read_cache_workloads,
    formatting_workloads
);
criterion_main!(benches);
