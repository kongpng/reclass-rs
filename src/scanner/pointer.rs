//! Pointer-map and pointer-chain scanning over [`Provider`](crate::provider::Provider)
//! memory.
//!
//! This is deliberately a scanner submodule: it consumes provider bytes and
//! produces scan-like results, but it is not itself a provider. The generic path
//! works for any provider with readable regions. When `scanflow-provider` is
//! enabled, memflow providers can opt into scanflow's native pointer-map builder
//! through a narrow provider downhook.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::provider::{MemoryRegion, Provider, RegionType};
use crate::scanner::{is_system_module, AddressRange};

const POINTER_SCAN_CHUNK: u64 = 256 * 1024;
const DEFAULT_MAX_POINTERS: usize = 2_000_000;
const DEFAULT_MAX_CHAINS: usize = 10_000;

/// Which backend should build a pointer map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerMapBackend {
    /// Use a provider-native backend when one is genuinely available, otherwise
    /// fall back to the generic Provider reader.
    #[default]
    Auto,
    /// Force the generic Provider reader. Works for native process, remote,
    /// kernel, WinDbg, memflow, file, buffer, and snapshot providers.
    GenericProvider,
    /// Require a provider-native backend. Currently this means scanflow on
    /// MemflowProvider only.
    NativeProvider,
}

/// Backend that actually produced a pointer map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerMapSource {
    #[default]
    GenericProvider,
    MemflowScanflow,
}

/// Request for building a pointer map.
#[derive(Clone, Debug)]
pub struct PointerMapRequest {
    /// Pointer width in bytes. Values other than 4/8 are normalized from the
    /// active provider's pointer size by callers.
    pub pointer_size: usize,
    /// Address stride while scanning candidate pointer slots.
    pub alignment: usize,
    /// Stop after this many pointer records. `0` means unlimited.
    pub max_pointers: usize,
    pub filter_executable: bool,
    pub filter_writable: bool,
    pub private_only: bool,
    pub skip_system_modules: bool,
    pub start_address: u64,
    pub end_address: u64,
    pub constrain_regions: Vec<AddressRange>,
    pub backend: PointerMapBackend,
}

impl Default for PointerMapRequest {
    fn default() -> Self {
        Self {
            pointer_size: 8,
            alignment: 8,
            max_pointers: DEFAULT_MAX_POINTERS,
            filter_executable: false,
            filter_writable: false,
            private_only: false,
            skip_system_modules: false,
            start_address: 0,
            end_address: 0,
            constrain_regions: Vec::new(),
            backend: PointerMapBackend::Auto,
        }
    }
}

impl PointerMapRequest {
    pub fn normalize(mut self) -> Self {
        self.pointer_size = match self.pointer_size {
            4 => 4,
            _ => 8,
        };
        self.alignment = self.alignment.max(1);
        self
    }
}

/// Pointer-map build counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerMapStats {
    pub source: PointerMapSource,
    pub regions_scanned: usize,
    pub bytes_scanned: u64,
    pub bytes_failed: u64,
    pub pointers_found: usize,
    pub truncated: bool,
}

/// One memory slot that looked like a pointer into the accepted address ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerRecord {
    /// Address of the pointer-sized slot.
    pub address: u64,
    /// Little-endian value read from that slot.
    pub points_to: u64,
}

/// A map from pointer locations to the addresses they point at, plus indexes
/// for reverse chain searches.
#[derive(Clone, Debug)]
pub struct PointerMap {
    pointer_size: usize,
    records: Vec<PointerRecord>,
    by_value: Vec<usize>,
    target_ranges: Vec<AddressRange>,
    stats: PointerMapStats,
}

impl PointerMap {
    pub fn from_records(
        pointer_size: usize,
        target_ranges: Vec<AddressRange>,
        mut records: Vec<PointerRecord>,
        mut stats: PointerMapStats,
    ) -> Self {
        records.sort_unstable_by_key(|r| (r.address, r.points_to));
        records.dedup();
        stats.pointers_found = records.len();

        let mut by_value: Vec<usize> = (0..records.len()).collect();
        by_value.sort_unstable_by_key(|&idx| {
            let r = records[idx];
            (r.points_to, r.address)
        });

        Self {
            pointer_size,
            records,
            by_value,
            target_ranges,
            stats,
        }
    }

    pub fn pointer_size(&self) -> usize {
        self.pointer_size
    }

    pub fn records(&self) -> &[PointerRecord] {
        &self.records
    }

    pub fn target_ranges(&self) -> &[AddressRange] {
        &self.target_ranges
    }

    pub fn stats(&self) -> PointerMapStats {
        self.stats
    }

    fn record_indices_pointing_between(&self, start: u64, end: u64) -> &[usize] {
        let lo = self
            .by_value
            .partition_point(|&idx| self.records[idx].points_to < start);
        let hi =
            lo + self.by_value[lo..].partition_point(|&idx| self.records[idx].points_to <= end);
        &self.by_value[lo..hi]
    }
}

/// Request for finding pointer chains to one or more target addresses.
#[derive(Clone, Debug)]
pub struct PointerChainRequest {
    pub targets: Vec<u64>,
    /// Maximum pointer dereference depth. `1` means direct pointers only.
    pub max_depth: usize,
    /// Accepted absolute offset between each pointer value and the next address.
    pub max_offset: u64,
    /// Stop after this many chains. `0` means unlimited.
    pub max_results: usize,
}

impl Default for PointerChainRequest {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            max_depth: 3,
            max_offset: 0x1000,
            max_results: DEFAULT_MAX_CHAINS,
        }
    }
}

/// One dereference in a pointer chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerChainStep {
    /// Address that is read as a pointer.
    pub pointer_address: u64,
    /// Pointer value read from `pointer_address`.
    pub points_to: u64,
    /// Signed offset that turns `points_to` into the next address in the chain.
    pub offset: i64,
}

/// A pointer chain ending at `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerChain {
    pub target: u64,
    /// Ordered from base pointer slot toward `target`.
    pub steps: Vec<PointerChainStep>,
}

impl PointerChain {
    pub fn base_address(&self) -> u64 {
        self.steps
            .first()
            .map(|step| step.pointer_address)
            .unwrap_or(self.target)
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::with_capacity(self.steps.len());
        for (idx, step) in self.steps.iter().enumerate() {
            let next = self
                .steps
                .get(idx + 1)
                .map(|s| s.pointer_address)
                .unwrap_or(self.target);
            parts.push(format!(
                "[0x{:X}] {} => 0x{:X}",
                step.pointer_address,
                format_signed_offset(step.offset),
                next
            ));
        }
        parts.join("  |  ")
    }
}

/// Pointer-chain search result set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PointerChainResults {
    pub chains: Vec<PointerChain>,
    pub truncated: bool,
}

/// Build a pointer map using the requested backend.
pub fn build_pointer_map(
    provider: &dyn Provider,
    request: &PointerMapRequest,
    abort: &AtomicBool,
) -> Result<PointerMap, String> {
    let request = request.clone().normalize();
    match request.backend {
        PointerMapBackend::GenericProvider => build_generic_pointer_map(provider, &request, abort),
        PointerMapBackend::NativeProvider => build_native_pointer_map(provider, &request, abort),
        PointerMapBackend::Auto => match build_native_pointer_map(provider, &request, abort) {
            Ok(map) => Ok(map),
            Err(_) => build_generic_pointer_map(provider, &request, abort),
        },
    }
}

/// Build a pointer map through plain Provider reads.
pub fn build_generic_pointer_map(
    provider: &dyn Provider,
    request: &PointerMapRequest,
    abort: &AtomicBool,
) -> Result<PointerMap, String> {
    let request = request.clone().normalize();
    let regions = prepare_pointer_regions(provider, &request);
    let ranges = RangeSet::new(
        regions
            .iter()
            .map(|r| AddressRange {
                start: r.base,
                end: r.base.saturating_add(r.size),
            })
            .collect(),
    );
    let mut stats = PointerMapStats {
        source: PointerMapSource::GenericProvider,
        regions_scanned: regions.len(),
        ..PointerMapStats::default()
    };
    let mut records = Vec::new();

    'regions: for region in &regions {
        if abort.load(Ordering::Relaxed) {
            break;
        }
        let mut region_off = 0u64;
        while region_off < region.size {
            if abort.load(Ordering::Relaxed) {
                break 'regions;
            }
            let remaining = region.size - region_off;
            let scan_len = remaining.min(POINTER_SCAN_CHUNK);
            let read_len = remaining.min(scan_len + request.pointer_size.saturating_sub(1) as u64);
            let addr = region.base.saturating_add(region_off);
            let mut buf = vec![0u8; read_len as usize];
            if !provider.read(addr, &mut buf) {
                stats.bytes_failed = stats.bytes_failed.saturating_add(scan_len);
                region_off = region_off.saturating_add(scan_len);
                continue;
            }
            stats.bytes_scanned = stats.bytes_scanned.saturating_add(scan_len);
            scan_pointer_chunk(
                addr,
                scan_len as usize,
                &buf,
                request.pointer_size,
                request.alignment,
                &ranges,
                request.max_pointers,
                &mut records,
                &mut stats,
            );
            if stats.truncated {
                break 'regions;
            }
            region_off = region_off.saturating_add(scan_len);
        }
    }

    Ok(PointerMap::from_records(
        request.pointer_size,
        ranges.ranges,
        records,
        stats,
    ))
}

fn build_native_pointer_map(
    provider: &dyn Provider,
    request: &PointerMapRequest,
    abort: &AtomicBool,
) -> Result<PointerMap, String> {
    #[cfg(feature = "scanflow-provider")]
    {
        if let Some(memflow) = provider.as_memflow_provider() {
            return build_scanflow_pointer_map(memflow, request, abort);
        }
    }

    let _ = (provider, request, abort);
    Err("provider does not expose a native pointer-map backend".to_string())
}

/// Find pointer chains through a previously-built map.
pub fn find_pointer_chains(
    map: &PointerMap,
    request: &PointerChainRequest,
    abort: &AtomicBool,
) -> PointerChainResults {
    let max_depth = request.max_depth.max(1);
    let mut out = PointerChainResults::default();
    let mut reversed_path = Vec::with_capacity(max_depth);
    let mut seen = Vec::<u64>::with_capacity(max_depth);
    for &target in &request.targets {
        if abort.load(Ordering::Relaxed) || results_full(&out, request.max_results) {
            break;
        }
        walk_chains(
            map,
            target,
            target,
            max_depth,
            request.max_offset,
            request.max_results,
            abort,
            &mut reversed_path,
            &mut seen,
            &mut out,
        );
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn walk_chains(
    map: &PointerMap,
    final_target: u64,
    current: u64,
    remaining_depth: usize,
    max_offset: u64,
    max_results: usize,
    abort: &AtomicBool,
    reversed_path: &mut Vec<PointerChainStep>,
    seen: &mut Vec<u64>,
    out: &mut PointerChainResults,
) {
    if remaining_depth == 0 || abort.load(Ordering::Relaxed) || results_full(out, max_results) {
        return;
    }

    let start = current.saturating_sub(max_offset);
    let end = current.saturating_add(max_offset);
    for &record_idx in map.record_indices_pointing_between(start, end) {
        if abort.load(Ordering::Relaxed) || results_full(out, max_results) {
            break;
        }
        let record = map.records[record_idx];
        if seen.contains(&record.address) {
            continue;
        }
        let Some(offset) = signed_offset(current, record.points_to) else {
            continue;
        };
        let step = PointerChainStep {
            pointer_address: record.address,
            points_to: record.points_to,
            offset,
        };
        reversed_path.push(step);
        seen.push(record.address);

        let mut steps = reversed_path.clone();
        steps.reverse();
        out.chains.push(PointerChain {
            target: final_target,
            steps,
        });
        if results_full(out, max_results) {
            out.truncated = max_results > 0;
            seen.pop();
            reversed_path.pop();
            break;
        }

        walk_chains(
            map,
            final_target,
            record.address,
            remaining_depth - 1,
            max_offset,
            max_results,
            abort,
            reversed_path,
            seen,
            out,
        );
        seen.pop();
        reversed_path.pop();
    }
}

fn results_full(out: &PointerChainResults, max_results: usize) -> bool {
    max_results > 0 && out.chains.len() >= max_results
}

fn scan_pointer_chunk(
    chunk_addr: u64,
    scan_len: usize,
    buf: &[u8],
    pointer_size: usize,
    alignment: usize,
    ranges: &RangeSet,
    max_pointers: usize,
    records: &mut Vec<PointerRecord>,
    stats: &mut PointerMapStats,
) {
    if pointer_size == 0 || buf.len() < pointer_size || scan_len == 0 {
        return;
    }
    let align = alignment.max(1) as u64;
    let align_pad = (align - (chunk_addr % align)) % align;
    let mut off = align_pad as usize;
    while off < scan_len && off + pointer_size <= buf.len() {
        let address = chunk_addr.saturating_add(off as u64);
        let points_to = read_pointer(&buf[off..off + pointer_size], pointer_size);
        if points_to != 0 && ranges.contains(points_to) {
            records.push(PointerRecord { address, points_to });
            if max_pointers > 0 && records.len() >= max_pointers {
                stats.truncated = true;
                return;
            }
        }
        off = off.saturating_add(alignment.max(1));
    }
}

fn read_pointer(bytes: &[u8], pointer_size: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw[..pointer_size].copy_from_slice(&bytes[..pointer_size]);
    u64::from_le_bytes(raw)
}

fn prepare_pointer_regions(
    provider: &dyn Provider,
    request: &PointerMapRequest,
) -> Vec<MemoryRegion> {
    let mut regions = provider.enumerate_regions();
    if regions.is_empty() {
        let size = provider.size();
        if size > 0 {
            regions.push(MemoryRegion {
                base: 0,
                size: size as u64,
                readable: true,
                writable: provider.is_writable(),
                executable: false,
                module_name: provider.name(),
                region_type: RegionType::Mapped,
            });
        }
    }

    let constraints = RangeSet::new(request.constrain_regions.clone());
    let has_constraints = !constraints.ranges.is_empty();
    let mut out = Vec::new();
    for region in regions {
        if !region.readable || region.size == 0 {
            continue;
        }
        if request.filter_executable && !region.executable {
            continue;
        }
        if request.filter_writable && !region.writable {
            continue;
        }
        if request.private_only && region.region_type != RegionType::Private {
            continue;
        }
        if request.skip_system_modules && is_system_module(&region.module_name) {
            continue;
        }
        let Some(mut clipped) =
            clip_region_to_bounds(region, request.start_address, request.end_address)
        else {
            continue;
        };
        if !has_constraints {
            out.push(clipped);
            continue;
        }
        let base = clipped.base;
        let end = clipped.base.saturating_add(clipped.size);
        for range in &constraints.ranges {
            let start = base.max(range.start);
            let clipped_end = end.min(range.end);
            if clipped_end > start {
                clipped.base = start;
                clipped.size = clipped_end - start;
                out.push(clipped.clone());
            }
        }
    }
    out.sort_unstable_by_key(|r| r.base);
    out
}

fn clip_region_to_bounds(
    mut region: MemoryRegion,
    start_address: u64,
    end_address: u64,
) -> Option<MemoryRegion> {
    let mut start = region.base;
    let mut end = region.base.checked_add(region.size)?;
    if start_address != 0 {
        start = start.max(start_address);
    }
    if end_address != 0 {
        end = end.min(end_address);
    }
    if end <= start {
        return None;
    }
    region.base = start;
    region.size = end - start;
    Some(region)
}

#[derive(Clone, Debug, Default)]
struct RangeSet {
    ranges: Vec<AddressRange>,
}

impl RangeSet {
    fn new(mut ranges: Vec<AddressRange>) -> Self {
        ranges.retain(|r| r.end > r.start);
        ranges.sort_unstable_by_key(|r| r.start);
        let mut merged: Vec<AddressRange> = Vec::with_capacity(ranges.len());
        for range in ranges {
            if let Some(last) = merged.last_mut() {
                if range.start <= last.end {
                    last.end = last.end.max(range.end);
                    continue;
                }
            }
            merged.push(range);
        }
        Self { ranges: merged }
    }

    fn contains(&self, addr: u64) -> bool {
        let idx = self.ranges.partition_point(|r| r.start <= addr);
        idx > 0 && addr < self.ranges[idx - 1].end
    }
}

fn signed_offset(current: u64, points_to: u64) -> Option<i64> {
    let diff = current as i128 - points_to as i128;
    i64::try_from(diff).ok()
}

pub fn format_signed_offset(offset: i64) -> String {
    if offset < 0 {
        format!("- 0x{:X}", offset.unsigned_abs())
    } else {
        format!("+ 0x{:X}", offset as u64)
    }
}

#[cfg(feature = "scanflow-provider")]
fn build_scanflow_pointer_map(
    memflow_provider: &crate::provider::memflow::MemflowProvider,
    request: &PointerMapRequest,
    abort: &AtomicBool,
) -> Result<PointerMap, String> {
    if abort.load(Ordering::Relaxed) {
        return Err("pointer-map build cancelled".to_string());
    }

    let request = request.clone().normalize();
    let accepted_regions = prepare_pointer_regions(memflow_provider, &request);
    let ranges = RangeSet::new(
        accepted_regions
            .iter()
            .map(|r| AddressRange {
                start: r.base,
                end: r.base.saturating_add(r.size),
            })
            .collect(),
    );

    let mut stats = PointerMapStats {
        source: PointerMapSource::MemflowScanflow,
        regions_scanned: accepted_regions.len(),
        ..PointerMapStats::default()
    };

    let records = memflow_provider.with_process(
        Err("failed to lock memflow process".to_string()),
        |process| {
            let mut map = scanflow::pointer_map::PointerMap::default();
            map.create_map(process, request.pointer_size)
                .map_err(|err| format!("scanflow pointer map failed: {err:?}"))?;

            Ok(map
                .map()
                .iter()
                .filter_map(|(address, points_to)| {
                    let address = address.to_umem() as u64;
                    let points_to = points_to.to_umem() as u64;
                    (points_to != 0 && ranges.contains(address) && ranges.contains(points_to))
                        .then_some(PointerRecord { address, points_to })
                })
                .collect::<Vec<_>>())
        },
    )?;

    if abort.load(Ordering::Relaxed) {
        return Err("pointer-map build cancelled".to_string());
    }

    let mut records = records;

    records.sort_unstable_by_key(|r| (r.address, r.points_to));
    records.dedup();
    if request.max_pointers > 0 && records.len() > request.max_pointers {
        records.truncate(request.max_pointers);
        stats.truncated = true;
    }
    stats.pointers_found = records.len();

    Ok(PointerMap::from_records(
        request.pointer_size,
        ranges.ranges,
        records,
        stats,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::BufferProvider;

    fn write_u64(buf: &mut [u8], addr: usize, value: u64) {
        buf[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn generic_pointer_map_finds_pointer_sized_values_into_ranges() {
        let mut bytes = vec![0u8; 0x100];
        write_u64(&mut bytes, 0x20, 0x80);
        write_u64(&mut bytes, 0x28, 0xFFFF);
        let provider = BufferProvider::new(bytes, "ptr.bin");
        let request = PointerMapRequest {
            pointer_size: 8,
            alignment: 8,
            max_pointers: 0,
            ..PointerMapRequest::default()
        };

        let map = build_generic_pointer_map(&provider, &request, &AtomicBool::new(false)).unwrap();

        assert_eq!(
            map.records(),
            &[PointerRecord {
                address: 0x20,
                points_to: 0x80,
            }]
        );
        assert_eq!(map.stats().pointers_found, 1);
    }

    #[test]
    fn pointer_chain_scan_returns_direct_and_nested_chains() {
        let mut bytes = vec![0u8; 0x100];
        write_u64(&mut bytes, 0x08, 0x20);
        write_u64(&mut bytes, 0x20, 0x70);
        let provider = BufferProvider::new(bytes, "chains.bin");
        let request = PointerMapRequest {
            pointer_size: 8,
            alignment: 8,
            max_pointers: 0,
            ..PointerMapRequest::default()
        };
        let map = build_generic_pointer_map(&provider, &request, &AtomicBool::new(false)).unwrap();

        let chains = find_pointer_chains(
            &map,
            &PointerChainRequest {
                targets: vec![0x80],
                max_depth: 2,
                max_offset: 0x10,
                max_results: 10,
            },
            &AtomicBool::new(false),
        );

        assert!(chains.chains.iter().any(|chain| {
            chain.steps
                == vec![PointerChainStep {
                    pointer_address: 0x20,
                    points_to: 0x70,
                    offset: 0x10,
                }]
        }));
        assert!(chains.chains.iter().any(|chain| {
            chain.steps
                == vec![
                    PointerChainStep {
                        pointer_address: 0x08,
                        points_to: 0x20,
                        offset: 0,
                    },
                    PointerChainStep {
                        pointer_address: 0x20,
                        points_to: 0x70,
                        offset: 0x10,
                    },
                ]
        }));
    }

    #[test]
    fn pointer_chain_scan_honors_result_cap() {
        let records = vec![
            PointerRecord {
                address: 0x10,
                points_to: 0x80,
            },
            PointerRecord {
                address: 0x18,
                points_to: 0x80,
            },
        ];
        let map = PointerMap::from_records(
            8,
            vec![AddressRange {
                start: 0,
                end: 0x100,
            }],
            records,
            PointerMapStats::default(),
        );
        let chains = find_pointer_chains(
            &map,
            &PointerChainRequest {
                targets: vec![0x80],
                max_depth: 1,
                max_offset: 0,
                max_results: 1,
            },
            &AtomicBool::new(false),
        );
        assert_eq!(chains.chains.len(), 1);
        assert!(chains.truncated);
    }
}
