//! Faithful Rust translation of the C++ scanner oracle tests
//! (`tests/test_scanner.cpp`, 163 asserts; `tests/test_scanner_combinations.cpp`,
//! 68 asserts). The assertions ARE the oracle (there is no golden text fixture).
//!
//! The upstream `syncScan`/`syncRescan` event-loop helpers become direct calls
//! to [`run_scan`]/[`run_rescan`] with [`NullObserver`]. The async `ScanEngine`
//! path is exercised by a counting [`TestObserver`].

use super::*;
use crate::provider::{MemoryRegion, NullProvider, Provider, RegionType};
use std::cell::Cell;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

// ─────────────────────────────────────────────────────────────────────────────
// Test fixtures
// ─────────────────────────────────────────────────────────────────────────────

/// `RegionProvider` / `SyntheticProvider` — a fixed byte buffer that exposes a
/// caller-supplied region list (`test_scanner.cpp:26-34`).
struct TestRegionProvider {
    data: Vec<u8>,
    regions: Vec<MemoryRegion>,
    enum_count: std::sync::atomic::AtomicUsize,
}

impl TestRegionProvider {
    fn new(data: Vec<u8>, regions: Vec<MemoryRegion>) -> Self {
        TestRegionProvider {
            data,
            regions,
            enum_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn enum_count(&self) -> usize {
        self.enum_count.load(Ordering::SeqCst)
    }
}

impl Provider for TestRegionProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let len = buf.len() as u64;
        if addr + len > self.data.len() as u64 {
            return false;
        }
        let start = addr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        true
    }
    fn size(&self) -> i32 {
        self.data.len() as i32
    }
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.enum_count.fetch_add(1, Ordering::SeqCst);
        self.regions.clone()
    }
}

/// `MutableProvider` / `WritableRegionProvider` — interior-mutable buffer with a
/// fixed region list, so the test can mutate memory through an `Arc<dyn
/// Provider>` between scans (`test_scanner.cpp:2493-2511`, `:2608-2624`).
struct MutableProvider {
    data: Mutex<Vec<u8>>,
    regions: Vec<MemoryRegion>,
}

impl MutableProvider {
    fn new(data: Vec<u8>, regions: Vec<MemoryRegion>) -> Self {
        MutableProvider {
            data: Mutex::new(data),
            regions,
        }
    }
    /// `bool write(addr, src, len)` — through interior mutability (`&self`) so
    /// the engine's `Arc<dyn Provider>` can be mutated by the test.
    fn write_at(&self, addr: u64, src: &[u8]) -> bool {
        let mut d = self.data.lock().unwrap();
        if addr + src.len() as u64 > d.len() as u64 {
            return false;
        }
        let start = addr as usize;
        d[start..start + src.len()].copy_from_slice(src);
        true
    }
}

impl Provider for MutableProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let d = self.data.lock().unwrap();
        if addr + buf.len() as u64 > d.len() as u64 {
            return false;
        }
        let start = addr as usize;
        buf.copy_from_slice(&d[start..start + buf.len()]);
        true
    }
    fn size(&self) -> i32 {
        self.data.lock().unwrap().len() as i32
    }
    fn is_writable(&self) -> bool {
        true
    }
    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        self.regions.clone()
    }
}

struct SparseIslandProvider {
    read_calls: Cell<usize>,
}

impl SparseIslandProvider {
    fn island(addr: u64) -> Option<[u8; 4]> {
        match addr {
            0 => Some([0x13, 0x37, 0x42, 0x99]),
            0x1000 => Some([0x13, 0x37, 0x42, 0x99]),
            _ => None,
        }
    }
}

impl Provider for SparseIslandProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.read_calls.set(self.read_calls.get() + 1);
        let Some(bytes) = Self::island(addr) else {
            return false;
        };
        if buf.len() > bytes.len() {
            return false;
        }
        buf.copy_from_slice(&bytes[..buf.len()]);
        true
    }

    fn size(&self) -> i32 {
        i32::MAX
    }

    fn is_readable(&self, addr: u64, len: i32) -> bool {
        len >= 0 && Self::island(addr).is_some_and(|bytes| (len as usize) <= bytes.len())
    }

    fn enumerate_regions(&self) -> Vec<MemoryRegion> {
        vec![
            region(0, 4, true, true, false, "a"),
            region(0x1000, 4, true, true, false, "b"),
        ]
    }
}

struct LiveSparseSpanProvider {
    data: Vec<u8>,
    read_calls: Cell<usize>,
}

impl LiveSparseSpanProvider {
    fn new() -> Self {
        let mut data = vec![0u8; 0x1004];
        data[0..4].copy_from_slice(&[0x13, 0x37, 0x42, 0x99]);
        data[0x1000..0x1004].copy_from_slice(&[0x13, 0x37, 0x42, 0x99]);
        Self {
            data,
            read_calls: Cell::new(0),
        }
    }
}

impl Provider for LiveSparseSpanProvider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.read_calls.set(self.read_calls.get() + 1);
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

    fn is_live(&self) -> bool {
        true
    }

    fn prefers_coalesced_rescan_reads(&self) -> bool {
        true
    }
}

/// Counting observer mirroring the `QSignalSpy` accounting in the engine tests.
#[derive(Default)]
struct TestObserver {
    progress: Mutex<Vec<i32>>,
    finished: Mutex<Option<Vec<ScanResult>>>,
    rescan_finished: Mutex<Option<Vec<ScanResult>>>,
    error: Mutex<Vec<String>>,
    stats: Mutex<Option<ScanStats>>,
    regions_resolved: Mutex<Option<(i32, u64)>>,
}

impl ScanObserver for TestObserver {
    fn progress(&self, percent: i32) {
        self.progress.lock().unwrap().push(percent);
    }
    fn regions_resolved(&self, count: i32, total_bytes: u64) {
        *self.regions_resolved.lock().unwrap() = Some((count, total_bytes));
    }
    fn scan_stats(&self, stats: ScanStats) {
        *self.stats.lock().unwrap() = Some(stats);
    }
    fn error(&self, message: &str) {
        self.error.lock().unwrap().push(message.to_string());
    }
    fn finished(&self, results: &[ScanResult]) {
        *self.finished.lock().unwrap() = Some(results.to_vec());
    }
    fn rescan_finished(&self, results: &[ScanResult]) {
        *self.rescan_finished.lock().unwrap() = Some(results.to_vec());
    }
}

// ── Synchronous-scan helpers (replace the upstream QEventLoop wrappers). ──

fn sync_scan(prov: &dyn Provider, req: &ScanRequest) -> Vec<ScanResult> {
    let abort = AtomicBool::new(false);
    run_scan(prov, req, &abort, &NullObserver)
}

#[allow(clippy::too_many_arguments)]
fn sync_rescan(
    prov: &dyn Provider,
    seed: Vec<ScanResult>,
    read_size: i32,
    cond: ScanCondition,
    vt: ValueType,
    pat: &[u8],
    msk: &[u8],
    pat2: &[u8],
) -> Vec<ScanResult> {
    let abort = AtomicBool::new(false);
    run_rescan(
        prov,
        seed,
        read_size,
        cond,
        vt,
        pat,
        msk,
        pat2,
        &abort,
        &NullObserver,
    )
}

/// `MemoryRegion{base, size, r, w, x, name}` brace-init helper (region_type
/// defaults to Private to mirror the C++ 6-arg aggregate init).
fn region(base: u64, size: u64, r: bool, w: bool, x: bool, name: &str) -> MemoryRegion {
    MemoryRegion {
        base,
        size,
        readable: r,
        writable: w,
        executable: x,
        module_name: name.to_string(),
        region_type: RegionType::Private,
    }
}

fn region_ty(
    base: u64,
    size: u64,
    r: bool,
    w: bool,
    x: bool,
    name: &str,
    ty: RegionType,
) -> MemoryRegion {
    MemoryRegion {
        base,
        size,
        readable: r,
        writable: w,
        executable: x,
        module_name: name.to_string(),
        region_type: ty,
    }
}

fn buffer(data: Vec<u8>) -> crate::provider::BufferProvider {
    crate::provider::BufferProvider::new(data, "")
}

// ═════════════════════════════════════════════════════════════════════════════
// Pattern parsing — signature mode (test_scanner.cpp:45-167)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn parse_empty_pattern() {
    let e = parse_signature("").unwrap_err();
    assert!(e.contains("Empty"));
}

#[test]
fn parse_spaces_only() {
    let e = parse_signature("   ").unwrap_err();
    assert!(e.contains("Empty"));
}

#[test]
fn parse_single_byte() {
    let (pat, mask) = parse_signature("AB").unwrap();
    assert_eq!(pat, vec![0xAB]);
    assert_eq!(mask, vec![0xFF]);
}

#[test]
fn parse_space_separated() {
    let (pat, mask) = parse_signature("48 8B 05").unwrap();
    assert_eq!(pat, vec![0x48, 0x8B, 0x05]);
    assert_eq!(mask, vec![0xFF, 0xFF, 0xFF]);
}

#[test]
fn parse_with_wildcards() {
    let (pat, mask) = parse_signature("48 ?? 05 ?? ??").unwrap();
    assert_eq!(pat.len(), 5);
    assert_eq!(pat[0], 0x48);
    assert_eq!(pat[2], 0x05);
    assert_eq!(mask, vec![0xFF, 0x00, 0xFF, 0x00, 0x00]);
}

#[test]
fn parse_single_question_mark() {
    let (_pat, mask) = parse_signature("48 ? 05").unwrap();
    assert_eq!(mask[1], 0x00);
}

#[test]
fn parse_packed_no_spaces() {
    let (pat, mask) = parse_signature("488B??05CC").unwrap();
    assert_eq!(pat.len(), 5);
    assert_eq!(pat[0], 0x48);
    assert_eq!(pat[1], 0x8B);
    assert_eq!(mask[2], 0x00);
    assert_eq!(pat[3], 0x05);
    assert_eq!(pat[4], 0xCC);
}

#[test]
fn parse_c_style() {
    let (pat, _mask) = parse_signature("\\x48\\x8B\\x05").unwrap();
    assert_eq!(pat, vec![0x48, 0x8B, 0x05]);
}

#[test]
fn parse_lowercase_hex() {
    let (pat, _mask) = parse_signature("ab cd ef").unwrap();
    assert_eq!(pat, vec![0xAB, 0xCD, 0xEF]);
}

#[test]
fn parse_mixed_case() {
    let (pat, _mask) = parse_signature("aB Cd eF").unwrap();
    assert_eq!(pat, vec![0xAB, 0xCD, 0xEF]);
}

#[test]
fn parse_invalid_hex() {
    assert!(parse_signature("GG").is_err());
}

#[test]
fn parse_odd_chars_no_spaces() {
    let e = parse_signature("ABC").unwrap_err();
    assert!(e.contains("Odd"));
}

#[test]
fn parse_invalid_token_width() {
    assert!(parse_signature("48 ABC 05").is_err());
}

#[test]
fn parse_leading_trailing_spaces() {
    let (pat, _mask) = parse_signature("  48 8B  ").unwrap();
    assert_eq!(pat.len(), 2);
}

#[test]
fn parse_all_wildcards() {
    let (pat, mask) = parse_signature("?? ?? ??").unwrap();
    assert_eq!(pat.len(), 3);
    assert_eq!(mask, vec![0x00, 0x00, 0x00]);
}

// ═════════════════════════════════════════════════════════════════════════════
// Value serialization (test_scanner.cpp:173-363)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn serialize_int8() {
    let (pat, mask) = serialize_value(ValueType::Int8, "-42").unwrap();
    assert_eq!(pat, vec![(-42i8) as u8]);
    assert_eq!(mask, vec![0xFF]);
}

#[test]
fn serialize_int8_overflow() {
    assert!(serialize_value(ValueType::Int8, "200").is_err());
}

#[test]
fn serialize_int16() {
    let (pat, _) = serialize_value(ValueType::Int16, "-1000").unwrap();
    assert_eq!(pat, (-1000i16).to_le_bytes().to_vec());
}

#[test]
fn serialize_int32() {
    let (pat, _) = serialize_value(ValueType::Int32, "12345").unwrap();
    assert_eq!(pat, 12345i32.to_le_bytes().to_vec());
}

#[test]
fn serialize_int32_negative() {
    let (pat, _) = serialize_value(ValueType::Int32, "-1").unwrap();
    assert_eq!(pat, (-1i32).to_le_bytes().to_vec());
}

#[test]
fn serialize_int64() {
    let (pat, _) = serialize_value(ValueType::Int64, "9999999999").unwrap();
    assert_eq!(pat, 9999999999i64.to_le_bytes().to_vec());
}

#[test]
fn serialize_uint8() {
    let (pat, _) = serialize_value(ValueType::UInt8, "255").unwrap();
    assert_eq!(pat, vec![255]);
}

#[test]
fn serialize_uint8_hex() {
    let (pat, _) = serialize_value(ValueType::UInt8, "0xFF").unwrap();
    assert_eq!(pat, vec![0xFF]);
}

#[test]
fn serialize_uint16() {
    let (pat, _) = serialize_value(ValueType::UInt16, "1234").unwrap();
    assert_eq!(pat, 1234u16.to_le_bytes().to_vec());
}

#[test]
fn serialize_uint32() {
    let (pat, _) = serialize_value(ValueType::UInt32, "0xDEADBEEF").unwrap();
    assert_eq!(pat, 0xDEADBEEFu32.to_le_bytes().to_vec());
}

#[test]
fn serialize_uint64() {
    let (pat, _) = serialize_value(ValueType::UInt64, "0xCAFEBABEDEADBEEF").unwrap();
    assert_eq!(pat, 0xCAFEBABEDEADBEEFu64.to_le_bytes().to_vec());
}

#[test]
fn serialize_float() {
    let (pat, _) = serialize_value(ValueType::Float, "3.14").unwrap();
    assert_eq!(pat, 3.14f32.to_le_bytes().to_vec());
}

#[test]
fn serialize_double() {
    let (pat, _) = serialize_value(ValueType::Double, "2.71828").unwrap();
    assert_eq!(pat, 2.71828f64.to_le_bytes().to_vec());
}

#[test]
fn serialize_vec2() {
    let (pat, _) = serialize_value(ValueType::Vec2, "1.0 2.0").unwrap();
    assert_eq!(pat.len(), 8);
    assert_eq!(&pat[0..4], &1.0f32.to_le_bytes());
    assert_eq!(&pat[4..8], &2.0f32.to_le_bytes());
}

#[test]
fn serialize_vec3() {
    let (pat, _) = serialize_value(ValueType::Vec3, "1.0 0.0 0.0").unwrap();
    assert_eq!(pat.len(), 12);
    assert_eq!(&pat[0..4], &1.0f32.to_le_bytes());
    assert_eq!(&pat[4..8], &0.0f32.to_le_bytes());
    assert_eq!(&pat[8..12], &0.0f32.to_le_bytes());
}

#[test]
fn serialize_vec3_wrong_count() {
    let e = serialize_value(ValueType::Vec3, "1.0 2.0").unwrap_err();
    assert!(e.contains('3'));
}

#[test]
fn serialize_vec4() {
    let (pat, _) = serialize_value(ValueType::Vec4, "1.0 2.0 3.0 4.0").unwrap();
    assert_eq!(pat.len(), 16);
    for (i, want) in [1.0f32, 2.0, 3.0, 4.0].iter().enumerate() {
        assert_eq!(&pat[i * 4..i * 4 + 4], &want.to_le_bytes());
    }
}

#[test]
fn serialize_utf8() {
    let (pat, _) = serialize_value(ValueType::Utf8, "Hello").unwrap();
    assert_eq!(pat, b"Hello".to_vec());
}

#[test]
fn serialize_utf16() {
    let (pat, _) = serialize_value(ValueType::Utf16, "Hi").unwrap();
    assert_eq!(pat.len(), 4);
    assert_eq!(u16::from_le_bytes([pat[0], pat[1]]), b'H' as u16);
    assert_eq!(u16::from_le_bytes([pat[2], pat[3]]), b'i' as u16);
}

#[test]
fn serialize_hex_bytes() {
    let (pat, _) = serialize_value(ValueType::HexBytes, "DE AD BE EF").unwrap();
    assert_eq!(pat, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn serialize_empty_value() {
    let e = serialize_value(ValueType::Int32, "").unwrap_err();
    assert!(e.contains("Empty"));
}

#[test]
fn serialize_invalid_int() {
    assert!(serialize_value(ValueType::Int32, "notanumber").is_err());
}

#[test]
fn serialize_invalid_float() {
    assert!(serialize_value(ValueType::Float, "abc").is_err());
}

// ═════════════════════════════════════════════════════════════════════════════
// Natural alignment + value size (test_scanner.cpp:369-377)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn alignment_values() {
    assert_eq!(natural_alignment(ValueType::Int8), 1);
    assert_eq!(natural_alignment(ValueType::Int16), 2);
    assert_eq!(natural_alignment(ValueType::Int32), 4);
    assert_eq!(natural_alignment(ValueType::Int64), 8);
    assert_eq!(natural_alignment(ValueType::Float), 4);
    assert_eq!(natural_alignment(ValueType::Double), 8);
    assert_eq!(natural_alignment(ValueType::Vec3), 4);
    assert_eq!(natural_alignment(ValueType::Utf8), 1);
    assert_eq!(natural_alignment(ValueType::Utf16), 2);
}

#[test]
fn value_sizes() {
    assert_eq!(value_size_for_type(ValueType::Int8), 1);
    assert_eq!(value_size_for_type(ValueType::Int16), 2);
    assert_eq!(value_size_for_type(ValueType::Int32), 4);
    assert_eq!(value_size_for_type(ValueType::Int64), 8);
    assert_eq!(value_size_for_type(ValueType::Double), 8);
    assert_eq!(value_size_for_type(ValueType::Vec2), 8);
    assert_eq!(value_size_for_type(ValueType::Vec3), 12);
    assert_eq!(value_size_for_type(ValueType::Vec4), 16);
    assert_eq!(value_size_for_type(ValueType::Utf8), 4);
}

// ═════════════════════════════════════════════════════════════════════════════
// Default request field values
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_request_defaults() {
    let r = ScanRequest::default();
    assert_eq!(r.alignment, 1);
    assert_eq!(r.max_results, 50000);
    assert_eq!(r.value_size, 4);
    assert_eq!(r.condition, ScanCondition::ExactValue);
    assert_eq!(r.value_type, ValueType::Int32);
    assert!(!r.filter_executable);
    assert!(!r.private_only);
}

// ═════════════════════════════════════════════════════════════════════════════
// Scan engine — basic functionality (via run_scan directly)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_exact_match() {
    let mut data = vec![0u8; 8];
    data[2] = 0x22;
    data[3] = 0x33;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0x22, 0x33],
        mask: vec![0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 2);
}

#[test]
fn scan_wildcard_match() {
    let mut data = vec![0u8; 16];
    data[0] = 0x48;
    data[1] = 0x8B;
    data[2] = 0xAA;
    data[3] = 0x05;
    data[8] = 0x48;
    data[9] = 0x8B;
    data[10] = 0xBB;
    data[11] = 0x05;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0x48, 0x8B, 0x00, 0x05],
        mask: vec![0xFF, 0xFF, 0x00, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 0);
    assert_eq!(r[1].address, 8);
}

#[test]
fn scan_wildcard_overlapping_matches() {
    let prov = buffer(vec![0xAA, 0xAA, 0xAA, 0xAA]);
    let req = ScanRequest {
        pattern: vec![0xAA, 0x00, 0xAA],
        mask: vec![0xFF, 0x00, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 0);
    assert_eq!(r[1].address, 1);
}

#[test]
fn scan_no_match() {
    let prov = buffer(vec![0u8; 32]);
    let req = ScanRequest {
        pattern: vec![0xFF, 0xFF],
        mask: vec![0xFF, 0xFF],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_alignment4() {
    let mut data = vec![0u8; 16];
    data[2] = 0xAA;
    data[3] = 0xBB;
    data[4] = 0xAA;
    data[5] = 0xBB;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA, 0xBB],
        mask: vec![0xFF, 0xFF],
        alignment: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 4);
}

#[test]
fn scan_max_results() {
    let prov = buffer(vec![0xAAu8; 1000]);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        max_results: 10,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 10);
}

#[test]
fn scan_empty_provider() {
    let prov = NullProvider::default();
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_chunk_boundary_overlap() {
    // BufferProvider regions are not used (it returns one Mapped region);
    // the 256 KiB+16 buffer forces the multi-chunk advance path.
    let k_chunk = 256 * 1024;
    let mut data = vec![0u8; k_chunk + 16];
    let pos = k_chunk - 2;
    data[pos] = 0xDE;
    data[pos + 1] = 0xAD;
    data[pos + 2] = 0xBE;
    data[pos + 3] = 0xEF;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xDE, 0xAD, 0xBE, 0xEF],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, pos as u64);
}

#[test]
fn scan_multiple_matches() {
    let mut data = vec![0u8; 64];
    for i in 0..4 {
        data[i * 16] = 0xCA;
        data[i * 16 + 1] = 0xFE;
    }
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xCA, 0xFE],
        mask: vec![0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 4);
    for (i, res) in r.iter().enumerate() {
        assert_eq!(res.address, (i * 16) as u64);
    }
}

#[test]
fn scan_single_byte_pattern() {
    let mut data = vec![0u8; 8];
    data[5] = 0x42;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0x42],
        mask: vec![0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 5);
}

#[test]
fn scan_pattern_larger_than_data() {
    let prov = buffer(vec![0xAAu8; 4]);
    let req = ScanRequest {
        pattern: vec![0xAA; 8],
        mask: vec![0xFF; 8],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_pattern_exact_size() {
    let prov = buffer(vec![0xDE, 0xAD, 0xBE, 0xEF]);
    let req = ScanRequest {
        pattern: vec![0xDE, 0xAD, 0xBE, 0xEF],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 0);
}

#[test]
fn scan_at_end_of_buffer() {
    let mut data = vec![0u8; 32];
    data[30] = 0xAB;
    data[31] = 0xCD;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAB, 0xCD],
        mask: vec![0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 30);
}

#[test]
fn scan_one_byte_buffer() {
    let prov = buffer(vec![0xAB]);
    let req = ScanRequest {
        pattern: vec![0xAB],
        mask: vec![0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 0);
}

#[test]
fn scan_overlapping_matches() {
    let mut data = vec![0u8; 4];
    data[0] = 0xAA;
    data[1] = 0xAA;
    data[2] = 0xAA;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA, 0xAA],
        mask: vec![0xFF, 0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 0);
    assert_eq!(r[1].address, 1);
}

#[test]
fn scan_all_wildcard_pattern() {
    let prov = buffer(vec![0x42u8; 8]);
    let req = ScanRequest {
        pattern: vec![0x00, 0x00],
        mask: vec![0x00, 0x00],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 7);
}

// ═════════════════════════════════════════════════════════════════════════════
// Region filtering (test_scanner.cpp:653-759)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_filter_executable() {
    let mut data = vec![0u8; 32];
    data[0] = 0xAA;
    data[16] = 0xAA;
    let regions = vec![
        region(0, 16, true, true, false, "heap"),
        region(16, 16, true, false, true, "code"),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        filter_executable: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 16);
    assert_eq!(r[0].region_module, "code+0x0");
}

#[test]
fn scan_filter_writable() {
    let mut data = vec![0u8; 32];
    data[0] = 0xBB;
    data[16] = 0xBB;
    let regions = vec![
        region(0, 16, true, true, false, "data"),
        region(16, 16, true, false, true, "code"),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xBB],
        mask: vec![0xFF],
        filter_writable: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 0);
}

#[test]
fn scan_both_filters() {
    let mut data = vec![0u8; 48];
    data[0] = 0xCC;
    data[16] = 0xCC;
    data[32] = 0xCC;
    let regions = vec![
        region(0, 16, true, true, false, "data"),
        region(16, 16, true, false, true, "code"),
        region(32, 16, true, true, true, "rwx"),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xCC],
        mask: vec![0xFF],
        filter_executable: true,
        filter_writable: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 32);
}

#[test]
fn scan_region_module_name() {
    let mut data = vec![0u8; 16];
    data[0] = 0xDD;
    let regions = vec![region(0, 16, true, true, true, "Game.exe")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xDD],
        mask: vec![0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].region_module, "Game.exe+0x0");
}

// ═════════════════════════════════════════════════════════════════════════════
// Value scan integration (test_scanner.cpp:841-940)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_find_int32_value() {
    let mut data = vec![0u8; 64];
    data[20..24].copy_from_slice(&12345i32.to_le_bytes());
    let prov = buffer(data);
    let (pat, mask) = serialize_value(ValueType::Int32, "12345").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 20);
}

#[test]
fn scan_find_float_value() {
    let mut data = vec![0u8; 64];
    data[8..12].copy_from_slice(&3.14f32.to_le_bytes());
    let prov = buffer(data);
    let (pat, mask) = serialize_value(ValueType::Float, "3.14").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 8);
}

#[test]
fn scan_find_utf16_string() {
    let mut data = vec![0u8; 128];
    data[32..34].copy_from_slice(&(b'H' as u16).to_le_bytes());
    data[34..36].copy_from_slice(&(b'i' as u16).to_le_bytes());
    let prov = buffer(data);
    let (pat, mask) = serialize_value(ValueType::Utf16, "Hi").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 2,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 32);
}

#[test]
fn scan_find_vec3() {
    let mut data = vec![0u8; 64];
    data[12..16].copy_from_slice(&1.0f32.to_le_bytes());
    let prov = buffer(data);
    let (pat, mask) = serialize_value(ValueType::Vec3, "1.0 0.0 0.0").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 12);
}

// ═════════════════════════════════════════════════════════════════════════════
// Provider region defaults (test_scanner.cpp:946-984)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn provider_default_regions_empty() {
    let empty = crate::provider::BufferProvider::new(vec![], "");
    assert!(empty.enumerate_regions().is_empty());

    let anon = crate::provider::BufferProvider::new(vec![0u8; 16], "");
    let anon_regions = anon.enumerate_regions();
    assert_eq!(anon_regions.len(), 1);
    assert_eq!(anon_regions[0].base, 0);
    assert_eq!(anon_regions[0].size, 16);
    assert_eq!(anon_regions[0].module_name, "[buffer]");

    let named = crate::provider::BufferProvider::new(vec![0u8; 8], "test.dat");
    let named_regions = named.enumerate_regions();
    assert_eq!(named_regions.len(), 1);
    assert_eq!(named_regions[0].module_name, "test.dat");
}

#[test]
fn provider_null_provider_regions_empty() {
    let p = NullProvider::default();
    assert!(p.enumerate_regions().is_empty());
}

#[test]
fn provider_custom_regions() {
    let regs = vec![
        region(0x1000, 0x2000, true, true, false, "heap"),
        region(0x3000, 0x1000, true, false, true, "code"),
    ];
    let p = TestRegionProvider::new(vec![0u8; 0x4000], regs);
    let result = p.enumerate_regions();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].base, 0x1000);
    assert_eq!(result[0].module_name, "heap");
    assert!(result[1].executable);
}

#[test]
fn scan_multiple_regions() {
    let mut data = vec![0u8; 48];
    data[4] = 0xEE;
    data[20] = 0xEE;
    data[36] = 0xEE;
    let regions = vec![
        region(0, 16, true, true, false, "region0"),
        region(16, 16, true, true, false, "region1"),
        region(32, 16, true, true, false, "region2"),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xEE],
        mask: vec![0xFF],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].region_module, "region0+0x4");
    assert_eq!(r[1].region_module, "region1+0x4");
    assert_eq!(r[2].region_module, "region2+0x4");
}

// ═════════════════════════════════════════════════════════════════════════════
// Address range filtering (test_scanner.cpp:1111-1218)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_address_range_no_limit() {
    let mut data = vec![0u8; 32];
    data[8] = 0xAA;
    data[16] = 0xAA;
    data[24] = 0xAA;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 3);
}

#[test]
fn scan_address_range_clips_results() {
    let mut data = vec![0u8; 32];
    data[8] = 0xAA;
    data[16] = 0xAA;
    data[24] = 0xAA;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        start_address: 8,
        end_address: 20,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 8);
    assert_eq!(r[1].address, 16);
}

#[test]
fn scan_address_range_outside_data() {
    let prov = buffer(vec![0xAAu8; 16]);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        start_address: 100,
        end_address: 200,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_address_range_with_regions() {
    let mut data = vec![0u8; 4096];
    data[1000] = 0xBB;
    data[2000] = 0xBB;
    let regions = vec![
        region(1000, 16, true, true, false, ""),
        region(2000, 16, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xBB],
        mask: vec![0xFF],
        start_address: 1000,
        end_address: 1020,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 1000);
}

#[test]
fn scan_unknown_with_address_range() {
    let prov = buffer(vec![0x42u8; 32]);
    let req = ScanRequest {
        condition: ScanCondition::UnknownValue,
        value_size: 4,
        alignment: 4,
        start_address: 8,
        end_address: 24,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].address, 8);
    assert_eq!(r[3].address, 20);
}

// ═════════════════════════════════════════════════════════════════════════════
// constrainRegions (test_scanner.cpp:1223-1757)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_constrain_regions_multiple_ranges() {
    let mut data = vec![0u8; 32];
    data[4] = 0xBB;
    data[12] = 0xBB;
    data[20] = 0xBB;
    data[28] = 0xBB;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xBB],
        mask: vec![0xFF],
        constrain_regions: vec![
            AddressRange { start: 0, end: 8 },
            AddressRange { start: 16, end: 24 },
        ],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 4);
    assert_eq!(r[1].address, 20);
}

#[test]
fn scan_constrain_regions_intersects_provider_regions() {
    let mut data = vec![0u8; 256];
    data[160] = 0xCC;
    data[210] = 0xCC;
    let regions = vec![region(100, 100, true, false, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xCC],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange {
            start: 150,
            end: 250,
        }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 160);
}

#[test]
fn scan_constrain_regions_no_overlap() {
    let regions = vec![region(0, 16, true, false, false, "")];
    let prov = TestRegionProvider::new(vec![0xEEu8; 32], regions);
    let req = ScanRequest {
        pattern: vec![0xEE],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange {
            start: 100,
            end: 200,
        }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_constrain_regions_gap_between_regions() {
    let mut data = vec![0u8; 64];
    data[10] = 0xDD;
    data[35] = 0xDD;
    let regions = vec![
        region(0, 16, true, true, false, ""),
        region(32, 16, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xDD],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 8, end: 40 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 10);
    assert_eq!(r[1].address, 35);
}

#[test]
fn scan_constrain_regions_partial_region_overlap() {
    let mut data = vec![0u8; 256];
    data[120] = 0xAB;
    data[160] = 0xAB;
    let regions = vec![region(100, 100, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xAB],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange {
            start: 150,
            end: 250,
        }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 160);
}

#[test]
fn scan_constrain_regions_mixed_module_and_anonymous() {
    let mut data = vec![0u8; 0x10000];
    data[0x1500] = 0xCC;
    data[0x5500] = 0xCC;
    let regions = vec![
        region(0x1000, 0x1000, true, false, true, "game.exe"),
        region(0x5000, 0x1000, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xCC],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange {
            start: 0x0,
            end: 0x10000,
        }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 0x1500);
    assert_eq!(r[1].address, 0x5500);
}

#[test]
fn scan_constrain_regions_fallback_provider() {
    let mut data = vec![0u8; 64];
    data[10] = 0xAA;
    data[30] = 0xAA;
    data[50] = 0xAA;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 5, end: 35 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 10);
    assert_eq!(r[1].address, 30);
}

#[test]
fn scan_constrain_regions_adjacent_regions() {
    let mut data = vec![0u8; 32];
    data[12] = 0xEF;
    data[20] = 0xEF;
    let regions = vec![
        region(0, 16, true, true, false, ""),
        region(16, 16, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xEF],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 8, end: 24 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 12);
    assert_eq!(r[1].address, 20);
}

#[test]
fn scan_constrain_regions_writable_filter_preserved() {
    let mut data = vec![0u8; 0x4000];
    data[0x1100] = 0xBB;
    data[0x2100] = 0xBB;
    let regions = vec![
        region(0x1000, 0x1000, true, false, true, ""),
        region(0x2000, 0x1000, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xBB],
        mask: vec![0xFF],
        filter_writable: true,
        constrain_regions: vec![AddressRange {
            start: 0x1000,
            end: 0x3000,
        }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 0x2100);
}

#[test]
fn scan_constrain_regions_extends_before_and_after() {
    let mut data = vec![0u8; 32];
    data[5] = 0xAA;
    data[15] = 0xAA;
    data[25] = 0xAA;
    let regions = vec![region(10, 10, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 0, end: 30 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 15);
}

#[test]
fn scan_constrain_regions_empty_constraint_scans_all() {
    let mut data = vec![0u8; 32];
    data[5] = 0xBB;
    data[15] = 0xBB;
    let regions = vec![region(0, 32, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xBB],
        mask: vec![0xFF],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 2);
}

#[test]
fn scan_constrain_regions_single_address_range() {
    let mut data = vec![0u8; 32];
    data[8] = 0xAA;
    data[16] = 0xAA;
    data[24] = 0xAA;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 8, end: 20 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 8);
    assert_eq!(r[1].address, 16);
}

#[test]
fn scan_constrain_regions_with_start_end_address() {
    let mut data = vec![0u8; 32];
    for off in [4, 12, 20, 26, 30] {
        data[off] = 0xDD;
    }
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xDD],
        mask: vec![0xFF],
        constrain_regions: vec![
            AddressRange { start: 0, end: 16 },
            AddressRange { start: 24, end: 32 },
        ],
        start_address: 8,
        end_address: 28,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 12);
    assert_eq!(r[1].address, 26);
}

#[test]
fn scan_constrain_regions_unknown_value_scan() {
    let prov = buffer(vec![0x42u8; 32]);
    let req = ScanRequest {
        condition: ScanCondition::UnknownValue,
        value_size: 4,
        alignment: 4,
        constrain_regions: vec![AddressRange { start: 8, end: 24 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 4);
    assert_eq!(r[0].address, 8);
    assert_eq!(r[3].address, 20);
}

#[test]
fn scan_constrain_regions_non_zero_base() {
    let mut data = vec![0u8; 0x10000];
    data[0x8100] = 0xFF;
    let regions = vec![region(0x8000, 0x1000, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xFF],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange {
            start: 0x8000,
            end: 0x9000,
        }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 0x8100);
}

#[test]
fn scan_constrain_regions_zero_size_constraint() {
    let prov = buffer(vec![0xAAu8; 32]);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 10, end: 10 }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_constrain_regions_inverted_range() {
    let prov = buffer(vec![0xAAu8; 32]);
    let req = ScanRequest {
        pattern: vec![0xAA],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 20, end: 10 }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_constrain_regions_overlapping_constraints() {
    let mut data = vec![0u8; 32];
    data[8] = 0xCC;
    data[16] = 0xCC;
    data[24] = 0xCC;
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: vec![0xCC],
        mask: vec![0xFF],
        constrain_regions: vec![
            AddressRange { start: 4, end: 20 },
            AddressRange { start: 12, end: 28 },
        ],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 3);
}

#[test]
fn intersect_constraints_skips_non_overlapping_prefixes() {
    let regions: Vec<_> = (0..512u64)
        .map(|i| region(i * 0x1000, 0x100, true, false, false, ""))
        .collect();
    let mut unsorted_regions = regions.clone();
    unsorted_regions.swap(0, 511);
    let constraints: Vec<_> = (0..512u64)
        .step_by(64)
        .map(|i| AddressRange {
            start: i * 0x1000 + 0x20,
            end: i * 0x1000 + 0x40,
        })
        .collect();

    let clipped = intersect_constraints(regions, &constraints);
    let mut unsorted_clipped = intersect_constraints(unsorted_regions, &constraints);
    unsorted_clipped.sort_by_key(|r| r.base);

    assert_eq!(clipped.len(), constraints.len());
    assert_eq!(unsorted_clipped.len(), clipped.len());
    assert_eq!(
        unsorted_clipped
            .iter()
            .map(|r| (r.base, r.size))
            .collect::<Vec<_>>(),
        clipped.iter().map(|r| (r.base, r.size)).collect::<Vec<_>>()
    );
    for (sub, constraint) in clipped.iter().zip(constraints.iter()) {
        assert_eq!(sub.base, constraint.start);
        assert_eq!(sub.size, constraint.end - constraint.start);
    }
}

/// Regression: a region whose `base + size` overflows u64 (e.g. a corrupt or
/// hostile ReClass.NET plugin section reporting an absurd size) must not panic
/// the clip. `intersect_constraints` clamps the region end to `u64::MAX` via
/// `saturating_add`, so the intersection uses the true upper bound instead of
/// the wrapped (tiny) value. Before the fix this overflowed `region.base +
/// region.size`, panicking in debug / wrapping in release.
#[test]
fn intersect_constraints_saturates_overflowing_region_end() {
    // base + size = (u64::MAX - 9) + 100 overflows; saturates to u64::MAX.
    let regions = vec![region(u64::MAX - 9, 100, true, false, false, "")];
    let constraints = [AddressRange {
        start: u64::MAX - 5,
        end: u64::MAX,
    }];
    let clipped = intersect_constraints(regions, &constraints);
    // Intersection of [u64::MAX-9, u64::MAX) with [u64::MAX-5, u64::MAX)
    // is [u64::MAX-5, u64::MAX): a single 5-byte sub-region.
    assert_eq!(clipped.len(), 1);
    assert_eq!(clipped[0].base, u64::MAX - 5);
    assert_eq!(clipped[0].size, 5);
}

#[test]
fn scan_constrain_regions_pattern_at_first_byte() {
    let mut data = vec![0u8; 64];
    data[20] = 0xFE;
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xFE],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 20, end: 40 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 20);
}

#[test]
fn scan_constrain_regions_pattern_at_last_byte() {
    let mut data = vec![0u8; 64];
    data[36..40].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xDE, 0xAD, 0xBE, 0xEF],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        constrain_regions: vec![AddressRange { start: 20, end: 40 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 36);
}

#[test]
fn scan_constrain_regions_pattern_one_byte_after_end() {
    let mut data = vec![0u8; 64];
    data[36..40].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xDE, 0xAD, 0xBE, 0xEF],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        constrain_regions: vec![AddressRange { start: 20, end: 39 }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_constrain_regions_region_smaller_than_pattern() {
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(vec![0xAAu8; 64], regions);
    let req = ScanRequest {
        pattern: vec![0xAA, 0xAA, 0xAA, 0xAA],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        constrain_regions: vec![AddressRange { start: 30, end: 32 }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

#[test]
fn scan_constrain_regions_pattern_exactly_fits_region() {
    let mut data = vec![0u8; 64];
    data[30..34].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0x11, 0x22, 0x33, 0x44],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        constrain_regions: vec![AddressRange { start: 30, end: 34 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 30);
}

#[test]
fn scan_constrain_regions_match_at_region_boundaries() {
    let mut data = vec![0u8; 32];
    data[15] = 0x77;
    data[16] = 0x77;
    let regions = vec![
        region(0, 16, true, true, false, ""),
        region(16, 16, true, true, false, ""),
    ];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0x77],
        mask: vec![0xFF],
        constrain_regions: vec![AddressRange { start: 0, end: 32 }],
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].address, 15);
    assert_eq!(r[1].address, 16);
}

#[test]
fn scan_constrain_regions_multibyte_at_clip_boundary() {
    let mut data = vec![0u8; 64];
    data[10..14].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    let regions = vec![region(0, 64, true, true, false, "")];
    let prov = TestRegionProvider::new(data, regions);
    let req = ScanRequest {
        pattern: vec![0xAA, 0xBB, 0xCC, 0xDD],
        mask: vec![0xFF, 0xFF, 0xFF, 0xFF],
        constrain_regions: vec![AddressRange { start: 10, end: 13 }],
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 0);
}

// ── Value type + pattern scans at every position in a constrained region ──

fn scan_constrained(
    data: Vec<u8>,
    pat: Vec<u8>,
    mask: Vec<u8>,
    alignment: i32,
    c_start: u64,
    c_end: u64,
) -> Vec<u64> {
    let prov = buffer(data);
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment,
        constrain_regions: vec![AddressRange {
            start: c_start,
            end: c_end,
        }],
        ..Default::default()
    };
    sync_scan(&prov, &req)
        .into_iter()
        .map(|r| r.address)
        .collect()
}

#[test]
fn scan_int32_at_region_start() {
    let mut data = vec![0u8; 128];
    data[32..36].copy_from_slice(&0x12345678i32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Int32, "305419896").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 32, 96), vec![32]);
}

#[test]
fn scan_int32_at_region_end() {
    let mut data = vec![0u8; 128];
    data[92..96].copy_from_slice(&0x12345678i32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Int32, "305419896").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 32, 96), vec![92]);
}

#[test]
fn scan_float_at_region_start() {
    let mut data = vec![0u8; 128];
    data[16..20].copy_from_slice(&3.14f32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Float, "3.14").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 16, 80), vec![16]);
}

#[test]
fn scan_float_at_region_end() {
    let mut data = vec![0u8; 128];
    data[76..80].copy_from_slice(&3.14f32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Float, "3.14").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 16, 80), vec![76]);
}

#[test]
fn scan_double_at_region_end() {
    let mut data = vec![0u8; 128];
    data[120..128].copy_from_slice(&2.71828f64.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Double, "2.71828").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 8, 0, 128), vec![120]);
}

#[test]
fn scan_int64_at_region_end() {
    let mut data = vec![0u8; 128];
    data[64..72].copy_from_slice(&0x0BADC0DEDEADBEEFi64.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Int64, "841540768839352047").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 8, 8, 72), vec![64]);
}

#[test]
fn scan_utf16_at_region_end() {
    let mut data = vec![0u8; 128];
    data[124..126].copy_from_slice(&(b'A' as u16).to_le_bytes());
    data[126..128].copy_from_slice(&(b'B' as u16).to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Utf16, "AB").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 2, 0, 128), vec![124]);
}

#[test]
fn scan_vec3_at_region_end() {
    let mut data = vec![0u8; 128];
    data[116..120].copy_from_slice(&1.0f32.to_le_bytes());
    data[120..124].copy_from_slice(&2.0f32.to_le_bytes());
    data[124..128].copy_from_slice(&3.0f32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Vec3, "1.0 2.0 3.0").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 0, 128), vec![116]);
}

#[test]
fn scan_pattern_at_region_start() {
    let mut data = vec![0u8; 128];
    data[20..23].copy_from_slice(&[0x48, 0x8B, 0x05]);
    let (pat, mask) = parse_signature("48 8B 05").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 1, 20, 100), vec![20]);
}

#[test]
fn scan_pattern_at_region_end() {
    let mut data = vec![0u8; 128];
    data[97..100].copy_from_slice(&[0x48, 0x8B, 0x05]);
    let (pat, mask) = parse_signature("48 8B 05").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 1, 20, 100), vec![97]);
}

#[test]
fn scan_pattern_with_wildcard_at_region_end() {
    let mut data = vec![0u8; 128];
    data[97..100].copy_from_slice(&[0x48, 0xFF, 0x05]);
    let (pat, mask) = parse_signature("48 ?? 05").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 1, 20, 100), vec![97]);
}

#[test]
fn scan_int32_multiple_positions_in_constrained_region() {
    let mut data = vec![0u8; 128];
    let v = 0xCAFEBABEu32.to_le_bytes();
    for off in [32, 60, 92, 8, 100] {
        data[off..off + 4].copy_from_slice(&v);
    }
    let (pat, mask) = serialize_value(ValueType::UInt32, "0xCAFEBABE").unwrap();
    assert_eq!(
        scan_constrained(data, pat, mask, 4, 32, 96),
        vec![32, 60, 92]
    );
}

#[test]
fn scan_pattern_multiple_positions_in_constrained_region() {
    let mut data = vec![0u8; 128];
    for off in [16, 50, 78, 10, 90] {
        data[off] = 0xAA;
        data[off + 1] = 0xBB;
    }
    let (pat, mask) = parse_signature("AA BB").unwrap();
    assert_eq!(
        scan_constrained(data, pat, mask, 1, 16, 80),
        vec![16, 50, 78]
    );
}

#[test]
fn scan_int8_alignment1_at_region_end() {
    let mut data = vec![0u8; 64];
    data[49] = 0x7F;
    let (pat, mask) = serialize_value(ValueType::Int8, "127").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 1, 10, 50), vec![49]);
}

#[test]
fn scan_uint16_alignment2_at_region_end() {
    let mut data = vec![0u8; 64];
    data[48..50].copy_from_slice(&0xBEEFu16.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::UInt16, "0xBEEF").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 2, 10, 50), vec![48]);
}

#[test]
fn scan_alignment4_skips_unaligned() {
    let mut data = vec![0u8; 64];
    data[18..22].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::UInt32, "0xDEADBEEF").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 4, 16, 48).len(), 0);
}

#[test]
fn scan_alignment8_skips_unaligned() {
    let mut data = vec![0u8; 64];
    data[12..20].copy_from_slice(&99.99f64.to_le_bytes());
    let (pat, mask) = serialize_value(ValueType::Double, "99.99").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 8, 0, 64).len(), 0);
}

#[test]
fn scan_alignment2_finds_aligned_skips_unaligned() {
    let mut data = vec![0u8; 64];
    let h = (b'H' as u16).to_le_bytes();
    let i = (b'i' as u16).to_le_bytes();
    data[20..22].copy_from_slice(&h);
    data[22..24].copy_from_slice(&i);
    data[33..35].copy_from_slice(&h);
    data[35..37].copy_from_slice(&i);
    let (pat, mask) = serialize_value(ValueType::Utf16, "Hi").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 2, 16, 48), vec![20]);
}

#[test]
fn scan_alignment1_overlapping_writes() {
    let mut data = vec![0u8; 48];
    data[20] = 0xAA;
    data[21] = 0xBB;
    data[21] = 0xAA;
    data[22] = 0xBB;
    data[25] = 0xAA;
    data[26] = 0xBB;
    let (pat, mask) = parse_signature("AA BB").unwrap();
    assert_eq!(scan_constrained(data, pat, mask, 1, 16, 32), vec![21, 25]);
}

// ═════════════════════════════════════════════════════════════════════════════
// Smart region targeting — RegionType / system module / address cap / cache
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn sysmod_empty_name() {
    assert!(!is_system_module(""));
}
#[test]
fn sysmod_kernel32_with_ext() {
    assert!(is_system_module("kernel32.dll"));
}
#[test]
fn sysmod_kernel32_no_ext() {
    assert!(is_system_module("kernel32"));
}
#[test]
fn sysmod_case_insensitive() {
    assert!(is_system_module("KERNEL32.DLL"));
    assert!(is_system_module("Kernel32"));
    assert!(is_system_module("nTdLl.dll"));
}
#[test]
fn sysmod_qt_core() {
    assert!(is_system_module("Qt6Core.dll"));
    assert!(is_system_module("qt6gui"));
    assert!(is_system_module("qt6widgets.dll"));
}
#[test]
fn sysmod_crt() {
    assert!(is_system_module("ucrtbase.dll"));
    assert!(is_system_module("msvcrt"));
    assert!(is_system_module("vcruntime140"));
}
#[test]
fn sysmod_user_binary_not_system() {
    assert!(!is_system_module("Reclass.exe"));
    assert!(!is_system_module("MyGame.exe"));
    assert!(!is_system_module("custom.dll"));
    assert!(!is_system_module("game_x64.exe"));
}
#[test]
fn sysmod_linux_so() {
    assert!(is_system_module("libc.so.6"));
    assert!(is_system_module("ld-linux-x86-64.so"));
}

// ── BMH self-test against naive matcher ──

#[test]
fn bmh_single_byte() {
    let data = b"hello world";
    assert_eq!(bmh_find(data, b"w"), Some(6));
    assert_eq!(bmh_find(data, b"z"), None);
}

#[test]
fn bmh_long_pattern() {
    let mut data = vec![0xCCu8; 1024];
    data[500..508].copy_from_slice(b"needle12");
    assert_eq!(bmh_find(&data, b"needle12"), Some(500));
    assert_eq!(bmh_find(&data, b"missing!"), None);
}

#[test]
fn bmh_pattern_equals_length() {
    assert_eq!(bmh_find(b"ABCD", b"ABCD"), Some(0));
    assert_eq!(bmh_find(b"ABCD", b"BCDE"), None);
}

#[test]
fn bmh_pattern_larger_than_data() {
    assert_eq!(bmh_find(b"ABC", b"ABCD"), None);
}

#[test]
fn bmh_at_end() {
    assert_eq!(bmh_find(b"padding---FOUND", b"FOUND"), Some(10));
}

#[test]
fn bmh_equivalent_to_naive() {
    let mut data = vec![0u8; 4096];
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i * 31 + 7) & 0xFF) as u8;
    }
    for pat_len in 4..=16usize {
        let pat = data[2000..2000 + pat_len].to_vec();
        let bmh = bmh_find(&data, &pat).map(|x| x as i32).unwrap_or(-1);
        let mut naive = -1i32;
        let mut i = 0usize;
        while i + pat_len <= data.len() {
            if data[i..i + pat_len] == pat[..] {
                naive = i as i32;
                break;
            }
            i += 1;
        }
        assert_eq!(bmh, naive, "pat_len {pat_len}");
    }
}

// ── Region-type filter (Image/Mapped/Private) ──

#[test]
fn region_type_private_only_skips_image() {
    let mut data = vec![0u8; 64];
    let needle = 0x12345678i32.to_le_bytes();
    data[0..4].copy_from_slice(&needle);
    data[32..36].copy_from_slice(&needle);
    let regs = vec![
        region_ty(0, 16, true, false, true, "game.dll", RegionType::Image),
        region_ty(32, 16, true, true, false, "", RegionType::Private),
    ];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "305419896").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        private_only: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 32);
}

#[test]
fn region_type_private_only_keeps_private() {
    let mut data = vec![0u8; 32];
    data[8..12].copy_from_slice(&42i32.to_le_bytes());
    let regs = vec![region_ty(0, 32, true, true, false, "", RegionType::Private)];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "42").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        private_only: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 8);
}

#[test]
fn region_type_private_only_skips_mapped() {
    let mut data = vec![0u8; 48];
    data[4..8].copy_from_slice(&99i32.to_le_bytes());
    data[36..40].copy_from_slice(&99i32.to_le_bytes());
    let regs = vec![
        region_ty(0, 16, true, false, false, "asset.bin", RegionType::Mapped),
        region_ty(32, 16, true, true, false, "", RegionType::Private),
    ];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "99").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        private_only: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 36);
}

// ── System module exclusion ──

#[test]
fn skip_system_excludes_ntdll() {
    let mut data = vec![0u8; 48];
    data[0..4].copy_from_slice(&7i32.to_le_bytes());
    data[32..36].copy_from_slice(&7i32.to_le_bytes());
    let regs = vec![
        region_ty(0, 16, true, false, true, "ntdll.dll", RegionType::Image),
        region_ty(32, 16, true, true, false, "MyGame.exe", RegionType::Image),
    ];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "7").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        skip_system_modules: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 32);
}

#[test]
fn skip_system_inactive_by_default() {
    let mut data = vec![0u8; 48];
    data[0..4].copy_from_slice(&7i32.to_le_bytes());
    data[32..36].copy_from_slice(&7i32.to_le_bytes());
    let regs = vec![
        region_ty(0, 16, true, false, true, "kernel32.dll", RegionType::Image),
        region_ty(32, 16, true, true, false, "", RegionType::Private),
    ];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "7").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 2);
}

#[test]
fn skip_system_combines_with_private_only() {
    let mut data = vec![0u8; 64];
    let n = 1234i32.to_le_bytes();
    data[0..4].copy_from_slice(&n);
    data[16..20].copy_from_slice(&n);
    data[32..36].copy_from_slice(&n);
    data[48..52].copy_from_slice(&n);
    let regs = vec![
        region_ty(0, 16, true, false, true, "ntdll.dll", RegionType::Image),
        region_ty(16, 16, true, true, false, "qt6core", RegionType::Private),
        region_ty(32, 16, true, true, false, "", RegionType::Private),
        region_ty(48, 16, true, false, false, "", RegionType::Mapped),
    ];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "1234").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        private_only: true,
        skip_system_modules: true,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 32);
}

#[test]
fn filtered_scan_reports_prepared_region_totals() {
    let mut data = vec![0u8; 96];
    let needle = 0x12345678i32.to_le_bytes();
    data[0..4].copy_from_slice(&needle);
    data[44..48].copy_from_slice(&needle);
    data[68..72].copy_from_slice(&needle);
    let regs = vec![
        region_ty(0, 16, true, false, true, "kernel32.dll", RegionType::Image),
        region_ty(32, 16, true, true, false, "game.exe", RegionType::Private),
        region_ty(64, 16, true, true, false, "game.exe", RegionType::Private),
    ];
    let prov = TestRegionProvider::new(data, regs.clone());
    let obs = TestObserver::default();
    let (pat, mask) = serialize_value(ValueType::Int32, "305419896").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        skip_system_modules: true,
        start_address: 40,
        end_address: 72,
        ..Default::default()
    };
    let r = run_scan_in_regions(&prov, regs, &req, &AtomicBool::new(false), &obs);
    assert_eq!(
        r.iter().map(|r| r.address).collect::<Vec<_>>(),
        vec![44, 68]
    );
    assert_eq!(*obs.regions_resolved.lock().unwrap(), Some((2, 16)));
    let stats = obs.stats.lock().unwrap().expect("stats");
    assert_eq!(stats.regions_scanned, 2);
    assert_eq!(stats.bytes_scanned, 16);
    assert_eq!(stats.bytes_failed, 0);
}

// ── Address upper cap ──

#[test]
fn address_cap_clips_above_limit() {
    let mut data = vec![0u8; 64];
    data[8..12].copy_from_slice(&0xABCDi32.to_le_bytes());
    data[40..44].copy_from_slice(&0xABCDi32.to_le_bytes());
    let regs = vec![region_ty(0, 64, true, true, false, "", RegionType::Private)];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "43981").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        start_address: 0,
        end_address: 32,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 8);
}

// ── Adaptive chunk: huge region scan completes ──

#[test]
fn adaptive_chunk_large_region() {
    let sz = 3 * 1024 * 1024usize;
    let mut data = vec![0u8; sz];
    data[sz - 1024..sz - 1020].copy_from_slice(&0xCAFEBABEu32.to_le_bytes());
    let regs = vec![region_ty(
        0,
        sz as u64,
        true,
        true,
        false,
        "",
        RegionType::Private,
    )];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::UInt32, "0xCAFEBABE").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, (sz - 1024) as u64);
}

// ── BMH path parity ──

#[test]
fn bmh_path_parity() {
    let mut data = vec![0u8; 8192];
    let needle = b"MAGIC!!!";
    data[1234..1242].copy_from_slice(needle);
    data[5000..5008].copy_from_slice(needle);
    let regs = vec![region_ty(
        0,
        8192,
        true,
        true,
        false,
        "",
        RegionType::Private,
    )];
    let prov = TestRegionProvider::new(data, regs);

    let req_bmh = ScanRequest {
        pattern: needle.to_vec(),
        mask: vec![0xFF; 8],
        alignment: 1,
        ..Default::default()
    };
    let bmh_hits = sync_scan(&prov, &req_bmh);

    let mut req_naive = req_bmh.clone();
    req_naive.mask[0] = 0x00;
    let naive_hits = sync_scan(&prov, &req_naive);

    assert_eq!(bmh_hits.len(), 2);
    assert_eq!(bmh_hits.len(), naive_hits.len());
}

// ═════════════════════════════════════════════════════════════════════════════
// New scan conditions: BiggerThan / SmallerThan / Between (first scan)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn condition_bigger_than_first_scan() {
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&5i32.to_le_bytes());
    data[4..8].copy_from_slice(&50i32.to_le_bytes());
    data[8..12].copy_from_slice(&500i32.to_le_bytes());
    let regs = vec![region_ty(0, 16, true, true, false, "", RegionType::Private)];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "100").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        condition: ScanCondition::BiggerThan,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    let r = sync_scan(&prov, &req);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].address, 8);
}

#[test]
fn condition_smaller_than_first_scan() {
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&5i32.to_le_bytes());
    data[4..8].copy_from_slice(&50i32.to_le_bytes());
    data[8..12].copy_from_slice(&500i32.to_le_bytes());
    data[12..16].copy_from_slice(&999i32.to_le_bytes());
    let regs = vec![region_ty(0, 16, true, true, false, "", RegionType::Private)];
    let prov = TestRegionProvider::new(data, regs);
    let (pat, mask) = serialize_value(ValueType::Int32, "100").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        condition: ScanCondition::SmallerThan,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 2);
}

#[test]
fn condition_between_first_scan() {
    let mut data = vec![0u8; 20];
    for (i, v) in [10i32, 50, 100, 200, 300].iter().enumerate() {
        data[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    let regs = vec![region_ty(0, 20, true, true, false, "", RegionType::Private)];
    let prov = TestRegionProvider::new(data, regs);
    let (lo, _) = serialize_value(ValueType::Int32, "50").unwrap();
    let (hi, _) = serialize_value(ValueType::Int32, "200").unwrap();
    let req = ScanRequest {
        pattern: lo.clone(),
        mask: vec![0xFF; lo.len()],
        pattern2: hi,
        alignment: 4,
        condition: ScanCondition::Between,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &req).len(), 3);
}

// ═════════════════════════════════════════════════════════════════════════════
// IncreasedBy / DecreasedBy on rescan (test_scanner.cpp:2482-2585)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn condition_increased_by_rescan() {
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&100i32.to_le_bytes());
    data[8..12].copy_from_slice(&200i32.to_le_bytes());
    let regs = vec![region_ty(0, 16, true, true, false, "", RegionType::Private)];
    let prov = MutableProvider::new(data, regs);

    let req = ScanRequest {
        condition: ScanCondition::UnknownValue,
        value_type: ValueType::Int32,
        value_size: 4,
        alignment: 4,
        ..Default::default()
    };
    let seed = sync_scan(&prov, &req);
    assert_eq!(seed.len(), 4);

    prov.write_at(0, &(100i32 + 5).to_le_bytes());

    let (delta, dmask) = serialize_value(ValueType::Int32, "5").unwrap();
    let filtered = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::IncreasedBy,
        ValueType::Int32,
        &delta,
        &dmask,
        &[],
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].address, 0);
}

#[test]
fn condition_decreased_by_rescan() {
    let mut data = vec![0u8; 8];
    data[0..4].copy_from_slice(&1000i32.to_le_bytes());
    let regs = vec![region_ty(0, 8, true, true, false, "", RegionType::Private)];
    let prov = MutableProvider::new(data, regs);

    let req = ScanRequest {
        condition: ScanCondition::UnknownValue,
        value_type: ValueType::Int32,
        value_size: 4,
        alignment: 4,
        ..Default::default()
    };
    let seed = sync_scan(&prov, &req);
    assert_eq!(seed.len(), 2);

    prov.write_at(0, &(1000i32 - 7).to_le_bytes());

    let (delta, dmask) = serialize_value(ValueType::Int32, "7").unwrap();
    let filtered = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::DecreasedBy,
        ValueType::Int32,
        &delta,
        &dmask,
        &[],
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].address, 0);
}

// ═════════════════════════════════════════════════════════════════════════════
// End-to-end "tutorial" test (test_scanner.cpp:2596-2733)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn e2e_find_mutate_revalidate() {
    let mut data = vec![0u8; 64];
    data[0..4].copy_from_slice(&100i32.to_le_bytes()); // hp
    data[4..8].copy_from_slice(&50i32.to_le_bytes()); // mana
    data[8..12].copy_from_slice(&1234i32.to_le_bytes()); // gold
    data[12..16].copy_from_slice(&7i32.to_le_bytes()); // level
    data[16..20].copy_from_slice(&(-10i32).to_le_bytes()); // x
    data[20..24].copy_from_slice(&20i32.to_le_bytes()); // y
    data[40..44].copy_from_slice(&1234i32.to_le_bytes()); // decoy in ntdll image

    let regs = vec![
        region_ty(0, 32, true, true, false, "", RegionType::Private),
        region_ty(32, 32, true, false, true, "ntdll.dll", RegionType::Image),
    ];
    let prov = MutableProvider::new(data, regs);

    // Step 1: find "1234" with smart filters on.
    let (pat, mask) = serialize_value(ValueType::Int32, "1234").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        private_only: true,
        skip_system_modules: true,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    let found = sync_scan(&prov, &req);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].address, 8);

    // Step 2: mutate that address.
    prov.write_at(found[0].address, &9999i32.to_le_bytes());

    // Step 3: re-validate via ExactValue rescan against new bytes.
    let (new_pat, new_mask) = serialize_value(ValueType::Int32, "9999").unwrap();
    let revalidated = sync_rescan(
        &prov,
        found.clone(),
        4,
        ScanCondition::ExactValue,
        ValueType::Int32,
        &new_pat,
        &new_mask,
        &[],
    );
    assert_eq!(revalidated.len(), 1);
    assert_eq!(revalidated[0].address, 8);
    let reread = i32::from_le_bytes(revalidated[0].scan_value[..4].try_into().unwrap());
    assert_eq!(reread, 9999);

    // Step 4: Changed-condition rescan against fresh seed.
    prov.write_at(8, &1234i32.to_le_bytes());
    let seed2 = sync_scan(&prov, &req);
    assert_eq!(seed2.len(), 1);
    prov.write_at(8, &1500i32.to_le_bytes());
    let changed = sync_rescan(
        &prov,
        seed2,
        4,
        ScanCondition::Changed,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].address, 8);

    // Step 5: Increased rescan.
    prov.write_at(8, &1234i32.to_le_bytes());
    let seed3 = sync_scan(&prov, &req);
    prov.write_at(8, &1500i32.to_le_bytes());
    let inc = sync_rescan(
        &prov,
        seed3,
        4,
        ScanCondition::Increased,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );
    assert_eq!(inc.len(), 1);

    // Step 6: Decreased rescan → empty.
    prov.write_at(8, &1234i32.to_le_bytes());
    let seed4 = sync_scan(&prov, &req);
    prov.write_at(8, &1500i32.to_le_bytes());
    let dec = sync_rescan(
        &prov,
        seed4,
        4,
        ScanCondition::Decreased,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );
    assert_eq!(dec.len(), 0);

    // Step 7: BiggerThan 5000 first scan → only 9999/50000.
    prov.write_at(8, &50000i32.to_le_bytes());
    let (bp, bm) = serialize_value(ValueType::Int32, "5000").unwrap();
    let big = ScanRequest {
        pattern: bp,
        mask: bm,
        alignment: 4,
        private_only: true,
        condition: ScanCondition::BiggerThan,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    let big_hits = sync_scan(&prov, &big);
    assert_eq!(big_hits.len(), 1);
    assert_eq!(big_hits[0].address, 8);

    // Step 8: Between 0 and 100 → 6 values.
    prov.write_at(8, &1234i32.to_le_bytes());
    let (lo, _) = serialize_value(ValueType::Int32, "0").unwrap();
    let (hi, _) = serialize_value(ValueType::Int32, "100").unwrap();
    let bw = ScanRequest {
        pattern: lo.clone(),
        mask: vec![0xFF; lo.len()],
        pattern2: hi,
        alignment: 4,
        private_only: true,
        condition: ScanCondition::Between,
        value_type: ValueType::Int32,
        value_size: 4,
        ..Default::default()
    };
    assert_eq!(sync_scan(&prov, &bw).len(), 6);
}

#[test]
fn rescan_empty_seed() {
    let prov = crate::provider::BufferProvider::new(vec![0u8; 16], "x");
    let out = sync_rescan(
        &prov,
        vec![],
        4,
        ScanCondition::ExactValue,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );
    assert_eq!(out.len(), 0);
}

#[test]
fn rescan_exact_value_keeps_masked_wildcard_semantics() {
    let prov = crate::provider::BufferProvider::new(vec![0xAA, 0x11, 0xAB, 0x11], "x");
    let seed = vec![
        ScanResult {
            address: 0,
            region_module: String::new(),
            scan_value: vec![0; 2].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 2,
            region_module: String::new(),
            scan_value: vec![0; 2].into(),
            previous_value: Default::default(),
        },
    ];

    let out = sync_rescan(
        &prov,
        seed,
        2,
        ScanCondition::ExactValue,
        ValueType::HexBytes,
        &[0xAA, 0x00],
        &[0xFF, 0x00],
        &[],
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].address, 0);
    assert_eq!(&out[0].scan_value[..], &[0xAA, 0x11]);
}

#[test]
fn rescan_sparse_unreadable_gap_reads_hits_individually() {
    let prov = SparseIslandProvider {
        read_calls: Cell::new(0),
    };
    let seed = vec![
        ScanResult {
            address: 0,
            region_module: String::new(),
            scan_value: vec![0; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 0x1000,
            region_module: String::new(),
            scan_value: vec![0; 4].into(),
            previous_value: Default::default(),
        },
    ];

    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::ExactValue,
        ValueType::Int32,
        &[0x13, 0x37, 0x42, 0x99],
        &[0xFF; 4],
        &[],
    );

    assert_eq!(
        out.iter().map(|result| result.address).collect::<Vec<_>>(),
        vec![0, 0x1000]
    );
    assert_eq!(prov.read_calls.get(), 2);
}

#[test]
fn rescan_live_sparse_hits_keep_coalesced_span_read() {
    let prov = LiveSparseSpanProvider::new();
    let seed = vec![
        ScanResult {
            address: 0,
            region_module: String::new(),
            scan_value: vec![0; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 0x1000,
            region_module: String::new(),
            scan_value: vec![0; 4].into(),
            previous_value: Default::default(),
        },
    ];

    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::ExactValue,
        ValueType::Int32,
        &[0x13, 0x37, 0x42, 0x99],
        &[0xFF; 4],
        &[],
    );

    assert_eq!(
        out.iter().map(|result| result.address).collect::<Vec<_>>(),
        vec![0, 0x1000]
    );
    assert_eq!(prov.read_calls.get(), 1);
}

#[test]
fn rescan_moves_old_scan_value_to_previous_value() {
    let prov = crate::provider::BufferProvider::new(vec![5, 6, 7, 8], "x");
    let seed = vec![ScanResult {
        address: 0,
        region_module: String::new(),
        scan_value: vec![1, 2, 3, 4].into(),
        previous_value: vec![9, 9, 9, 9].into(),
    }];

    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::UnknownValue,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );

    assert_eq!(out.len(), 1);
    assert_eq!(&out[0].previous_value[..], &[1, 2, 3, 4]);
    assert_eq!(&out[0].scan_value[..], &[5, 6, 7, 8]);
}

#[test]
fn rescan_unsorted_seed_preserves_result_order_after_chunked_reads() {
    let prov = crate::provider::BufferProvider::new((0u8..12).collect(), "x");
    let seed = vec![
        ScanResult {
            address: 8,
            region_module: "third".into(),
            scan_value: vec![0xAA; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 0,
            region_module: "first".into(),
            scan_value: vec![0xBB; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 4,
            region_module: "second".into(),
            scan_value: vec![0xCC; 4].into(),
            previous_value: Default::default(),
        },
    ];

    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::UnknownValue,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );

    assert_eq!(
        out.iter()
            .map(|result| result.region_module.as_str())
            .collect::<Vec<_>>(),
        vec!["third", "first", "second"]
    );
    assert_eq!(&out[0].scan_value[..], &[8, 9, 10, 11]);
    assert_eq!(&out[1].scan_value[..], &[0, 1, 2, 3]);
    assert_eq!(&out[2].scan_value[..], &[4, 5, 6, 7]);
}

#[test]
fn rescan_filtered_unsorted_seed_preserves_original_result_order() {
    let prov = crate::provider::BufferProvider::new(
        [
            1u32.to_le_bytes(),
            0x1234_5678u32.to_le_bytes(),
            0x1234_5678u32.to_le_bytes(),
        ]
        .concat(),
        "x",
    );
    let seed = vec![
        ScanResult {
            address: 8,
            region_module: "third".into(),
            scan_value: vec![0xAA; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 0,
            region_module: "first".into(),
            scan_value: vec![0xBB; 4].into(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: 4,
            region_module: "second".into(),
            scan_value: vec![0xCC; 4].into(),
            previous_value: Default::default(),
        },
    ];
    let pattern = 0x1234_5678u32.to_le_bytes();

    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::ExactValue,
        ValueType::UInt32,
        &pattern,
        &[0xFF; 4],
        &[],
    );

    assert_eq!(
        out.iter()
            .map(|result| result.region_module.as_str())
            .collect::<Vec<_>>(),
        vec!["third", "second"]
    );
}

#[test]
fn rescan_abort_before_no_filter_preserves_scan_value() {
    let prov = crate::provider::BufferProvider::new(vec![5, 6, 7, 8], "x");
    let abort = AtomicBool::new(true);
    let out = run_rescan(
        &prov,
        vec![ScanResult {
            address: 0,
            region_module: String::new(),
            scan_value: vec![1, 2, 3, 4].into(),
            previous_value: Default::default(),
        }],
        4,
        ScanCondition::UnknownValue,
        ValueType::Int32,
        &[],
        &[],
        &[],
        &abort,
        &NullObserver,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(&out[0].previous_value[..], &[1, 2, 3, 4]);
    assert_eq!(&out[0].scan_value[..], &[1, 2, 3, 4]);
}

/// Regression: a saved-scan results file can carry an attacker-controlled
/// address parsed via `from_str_radix(..).unwrap_or(0)` (scannerpanel parsing),
/// so `address` may be `u64::MAX` ("ffffffffffffffff"). `run_rescan`'s span math
/// (`address + read_size` when extending a span, and `span_last + read_size -
/// span_base` when sizing the chunk) must not overflow-panic in debug or wrap.
/// Two adjacent addresses pinned to the top of the address space exercise both
/// the span-extension add (line ~1232) and the chunk-length add (line ~1240);
/// the read fails gracefully (out of bounds → zeros) and, with no filter, both
/// seeds are preserved.
#[test]
fn rescan_address_overflow_does_not_panic() {
    let prov = crate::provider::BufferProvider::new(vec![0u8; 16], "x");
    let seed = vec![
        ScanResult {
            address: u64::MAX - 8,
            scan_value: vec![0xAA, 0xBB, 0xCC, 0xDD].into(),
            region_module: String::new(),
            previous_value: Default::default(),
        },
        ScanResult {
            address: u64::MAX,
            scan_value: vec![0x11, 0x22, 0x33, 0x44].into(),
            region_module: String::new(),
            previous_value: Default::default(),
        },
    ];
    // UnknownValue applies no filter, so both seeds are carried through; the
    // point is that the span arithmetic for addresses near u64::MAX neither
    // panics in debug nor wraps.
    let out = sync_rescan(
        &prov,
        seed,
        4,
        ScanCondition::UnknownValue,
        ValueType::Int32,
        &[],
        &[],
        &[],
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].address, u64::MAX - 8);
    assert_eq!(out[1].address, u64::MAX);
}

// ═════════════════════════════════════════════════════════════════════════════
// ScanResult JSON shape projection (test_scanner.cpp:2809)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn scan_result_json_shape() {
    let r = ScanResult {
        address: 0xDEADBEEFCAFEBABE,
        scan_value: vec![0xDE, 0xAD, 0xBE, 0xEF].into(),
        region_module: "game.exe".to_string(),
        previous_value: Default::default(),
    };
    let addr_hex = format!("{:x}", r.address);
    let value_hex: String = r.scan_value.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(addr_hex, "deadbeefcafebabe");
    assert_eq!(value_hex, "deadbeef");
    assert_eq!(r.region_module, "game.exe");
    assert_eq!(u64::from_str_radix(&addr_hex, 16).unwrap(), r.address);
}

// ═════════════════════════════════════════════════════════════════════════════
// ScanEngine async-path tests (counting observer)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn engine_scan_empty_pattern() {
    let prov: Arc<dyn Provider + Send + Sync> =
        Arc::new(crate::provider::BufferProvider::new(vec![0u8; 16], ""));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    // ExactValue + empty pattern → error emitted synchronously, no finished.
    eng.start(prov, ScanRequest::default(), obs.clone());
    eng.wait();
    assert_eq!(
        obs.error.lock().unwrap().as_slice(),
        &["Empty pattern".to_string()]
    );
    assert!(obs.finished.lock().unwrap().is_none());
}

#[test]
fn engine_scan_mask_size_mismatch() {
    let prov: Arc<dyn Provider + Send + Sync> =
        Arc::new(crate::provider::BufferProvider::new(vec![0u8; 16], ""));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    let req = ScanRequest {
        pattern: vec![0u8; 4],
        mask: vec![0xFF; 2],
        ..Default::default()
    };
    eng.start(prov, req, obs.clone());
    eng.wait();
    assert_eq!(
        obs.error.lock().unwrap().as_slice(),
        &["Pattern and mask size mismatch".to_string()]
    );
    assert!(obs.finished.lock().unwrap().is_none());
}

#[test]
fn engine_scan_finished_and_is_running() {
    let prov: Arc<dyn Provider + Send + Sync> = Arc::new(crate::provider::BufferProvider::new(
        vec![0u8; 256 * 1024],
        "",
    ));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    assert!(!eng.is_running());
    let req = ScanRequest {
        pattern: vec![0xFF],
        mask: vec![0xFF],
        ..Default::default()
    };
    eng.start(prov, req, obs.clone());
    eng.wait();
    assert!(!eng.is_running());
    assert!(obs.finished.lock().unwrap().is_some());
}

#[test]
fn engine_scan_progress_emitted() {
    let prov: Arc<dyn Provider + Send + Sync> = Arc::new(crate::provider::BufferProvider::new(
        vec![0u8; 512 * 1024],
        "",
    ));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    let req = ScanRequest {
        pattern: vec![0xFF],
        mask: vec![0xFF],
        ..Default::default()
    };
    eng.start(prov, req, obs.clone());
    eng.wait();
    let prog = obs.progress.lock().unwrap();
    assert!(!prog.is_empty());
    assert!(*prog.last().unwrap() >= 50);
}

#[test]
fn engine_scan_abort() {
    let prov: Arc<dyn Provider + Send + Sync> = Arc::new(crate::provider::BufferProvider::new(
        vec![0u8; 1024 * 1024],
        "",
    ));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    let req = ScanRequest {
        pattern: vec![0xFF],
        mask: vec![0xFF],
        ..Default::default()
    };
    eng.start(prov, req, obs.clone());
    eng.abort();
    eng.wait();
    // Exactly one finished fires (partial allowed).
    assert!(obs.finished.lock().unwrap().is_some());
}

#[test]
fn engine_scan_stats_emitted() {
    let mut data = vec![0u8; 64];
    data[8..12].copy_from_slice(&5i32.to_le_bytes());
    let regs = vec![region_ty(0, 64, true, true, false, "", RegionType::Private)];
    let prov: Arc<dyn Provider + Send + Sync> = Arc::new(TestRegionProvider::new(data, regs));
    let obs = Arc::new(TestObserver::default());
    let mut eng = ScanEngine::new();
    let (pat, mask) = serialize_value(ValueType::Int32, "5").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };
    eng.start(prov, req, obs.clone());
    eng.wait();
    let stats = obs.stats.lock().unwrap().expect("stats");
    assert_eq!(stats.regions_scanned, 1);
    assert!(stats.bytes_scanned > 0);
    assert_eq!(stats.bytes_failed, 0);
}

#[test]
fn engine_region_cache_reuses_across_scans() {
    let mut data = vec![0u8; 32];
    data[0..4].copy_from_slice(&1i32.to_le_bytes());
    let regs = vec![region_ty(0, 32, true, true, false, "", RegionType::Private)];
    let prov_concrete = Arc::new(TestRegionProvider::new(data, regs));
    let prov: Arc<dyn Provider + Send + Sync> = prov_concrete.clone();
    let (pat, mask) = serialize_value(ValueType::Int32, "1").unwrap();
    let req = ScanRequest {
        pattern: pat,
        mask,
        alignment: 4,
        ..Default::default()
    };

    let mut eng = ScanEngine::new();
    let obs = Arc::new(TestObserver::default());

    eng.start(prov.clone(), req.clone(), obs.clone());
    eng.wait();
    assert_eq!(prov_concrete.enum_count(), 1);

    eng.start(prov.clone(), req.clone(), obs.clone());
    eng.wait();
    assert_eq!(prov_concrete.enum_count(), 1);

    eng.invalidate_region_cache();
    eng.start(prov.clone(), req.clone(), obs.clone());
    eng.wait();
    assert_eq!(prov_concrete.enum_count(), 2);
}

// ═════════════════════════════════════════════════════════════════════════════
// Combinations matrix (test_scanner_combinations.cpp)
// ═════════════════════════════════════════════════════════════════════════════

#[test]
fn fast_scan_alignment_values() {
    let mut data = vec![0u8; 256];
    let needle = 0xCAFEBABEu32.to_le_bytes();
    let mut off = 0;
    while off + 4 <= 256 {
        data[off..off + 4].copy_from_slice(&needle);
        off += 4;
    }
    let regs = vec![region_ty(
        0,
        256,
        true,
        true,
        false,
        "",
        RegionType::Private,
    )];
    for (alignment, expected) in [(1, 64), (4, 64), (8, 32), (16, 16), (32, 8), (64, 4)] {
        let prov = TestRegionProvider::new(data.clone(), regs.clone());
        let (pat, mask) = serialize_value(ValueType::UInt32, "0xCAFEBABE").unwrap();
        let req = ScanRequest {
            pattern: pat,
            mask,
            alignment,
            condition: ScanCondition::ExactValue,
            value_type: ValueType::UInt32,
            value_size: 4,
            ..Default::default()
        };
        assert_eq!(
            sync_scan(&prov, &req).len(),
            expected,
            "alignment {alignment}"
        );
    }
}

#[test]
fn condition_value_type_matrix() {
    fn write_val(data: &mut [u8], t: ValueType, off: usize, v: i64) {
        match t {
            ValueType::Int8 => data[off] = (v as i8) as u8,
            ValueType::UInt8 => data[off] = v as u8,
            ValueType::Int16 => data[off..off + 2].copy_from_slice(&(v as i16).to_le_bytes()),
            ValueType::UInt16 => data[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes()),
            ValueType::Int32 => data[off..off + 4].copy_from_slice(&(v as i32).to_le_bytes()),
            ValueType::UInt32 => data[off..off + 4].copy_from_slice(&(v as u32).to_le_bytes()),
            ValueType::Int64 => data[off..off + 8].copy_from_slice(&v.to_le_bytes()),
            ValueType::UInt64 => data[off..off + 8].copy_from_slice(&(v as u64).to_le_bytes()),
            _ => {}
        }
    }

    let vts = [
        ValueType::Int8,
        ValueType::Int16,
        ValueType::Int32,
        ValueType::Int64,
        ValueType::UInt8,
        ValueType::UInt16,
        ValueType::UInt32,
        ValueType::UInt64,
    ];
    let cases = [
        (ScanCondition::ExactValue, "100", 1),
        (ScanCondition::ExactValue, "50", 2),
        (ScanCondition::BiggerThan, "25", 3),
        (ScanCondition::SmallerThan, "25", 0),
        (ScanCondition::SmallerThan, "75", 2),
    ];

    for t in vts {
        let sz = value_size_for_type(t) as usize;
        for (cond, value, expected) in cases {
            let total = (3 * sz + 16).max(32);
            let mut data = vec![0u8; total];
            write_val(&mut data, t, 0, 100);
            write_val(&mut data, t, sz, 50);
            write_val(&mut data, t, 2 * sz, 50);
            let regs = vec![region_ty(
                0,
                data.len() as u64,
                true,
                true,
                false,
                "",
                RegionType::Private,
            )];
            let prov = TestRegionProvider::new(data, regs);
            let (pat, mask) = serialize_value(t, value).unwrap();
            let req = ScanRequest {
                pattern: pat,
                mask,
                alignment: sz as i32,
                condition: cond,
                value_type: t,
                value_size: sz as i32,
                end_address: (3 * sz) as u64,
                ..Default::default()
            };
            assert_eq!(
                sync_scan(&prov, &req).len(),
                expected,
                "vt {t:?} cond {cond:?} value {value}"
            );
        }
    }
}

#[test]
fn condition_between_matrix() {
    let mut data = vec![0u8; 20];
    for (i, v) in [10i32, 50, 100, 200, 300].iter().enumerate() {
        data[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    let regs = vec![region_ty(0, 20, true, true, false, "", RegionType::Private)];
    let cases = [
        ("0", "1000", 5),
        ("50", "200", 3),
        ("11", "99", 1),
        ("301", "999", 0),
        ("100", "100", 1),
    ];
    for (lo_s, hi_s, expected) in cases {
        let prov = TestRegionProvider::new(data.clone(), regs.clone());
        let (lo, _) = serialize_value(ValueType::Int32, lo_s).unwrap();
        let (hi, _) = serialize_value(ValueType::Int32, hi_s).unwrap();
        let req = ScanRequest {
            pattern: lo.clone(),
            mask: vec![0xFF; lo.len()],
            pattern2: hi,
            alignment: 4,
            condition: ScanCondition::Between,
            value_type: ValueType::Int32,
            value_size: 4,
            ..Default::default()
        };
        assert_eq!(sync_scan(&prov, &req).len(), expected, "{lo_s}..{hi_s}");
    }
}

#[test]
fn region_filter_combos() {
    let mut data = vec![0u8; 48];
    let needle = 0xC0DEi32.to_le_bytes();
    data[0..4].copy_from_slice(&needle);
    data[16..20].copy_from_slice(&needle);
    data[32..36].copy_from_slice(&needle);
    let regs = vec![
        region_ty(0, 16, true, true, true, "", RegionType::Private),
        region_ty(16, 16, true, true, false, "", RegionType::Private),
        region_ty(32, 16, true, false, true, "", RegionType::Private),
    ];
    let cases = [
        (false, false, 3),
        (true, false, 2),
        (false, true, 2),
        (true, true, 1),
    ];
    for (fe, fw, expected) in cases {
        let prov = TestRegionProvider::new(data.clone(), regs.clone());
        let (pat, mask) = serialize_value(ValueType::Int32, "49374").unwrap();
        let req = ScanRequest {
            pattern: pat,
            mask,
            alignment: 4,
            filter_executable: fe,
            filter_writable: fw,
            condition: ScanCondition::ExactValue,
            value_type: ValueType::Int32,
            value_size: 4,
            ..Default::default()
        };
        assert_eq!(sync_scan(&prov, &req).len(), expected, "fe {fe} fw {fw}");
    }
}

#[test]
fn address_range_alignment() {
    let mut data = vec![0u8; 1024];
    let needle = 0xDEADBEEFu32.to_le_bytes();
    let mut off = 0;
    while off + 4 <= 1024 {
        data[off..off + 4].copy_from_slice(&needle);
        off += 32;
    }
    let regs = vec![region_ty(
        0,
        1024,
        true,
        true,
        false,
        "",
        RegionType::Private,
    )];
    let cases: [(i32, u64, usize); 7] = [
        (4, 1024, 32),
        (4, 512, 16),
        (8, 1024, 32),
        (16, 1024, 32),
        (32, 1024, 32),
        (64, 1024, 16),
        (4, 256, 8),
    ];
    for (alignment, end_addr, expected) in cases {
        let prov = TestRegionProvider::new(data.clone(), regs.clone());
        let (pat, mask) = serialize_value(ValueType::UInt32, "0xDEADBEEF").unwrap();
        let req = ScanRequest {
            pattern: pat,
            mask,
            alignment,
            condition: ScanCondition::ExactValue,
            value_type: ValueType::UInt32,
            value_size: 4,
            end_address: end_addr,
            ..Default::default()
        };
        assert_eq!(
            sync_scan(&prov, &req).len(),
            expected,
            "align {alignment} end {end_addr}"
        );
    }
}

#[test]
fn signature_wildcards() {
    let mut data = vec![0u8; 48];
    data[0..4].copy_from_slice(&[0xAB, 0xCC, 0xDD, 0xEE]);
    data[16..20].copy_from_slice(&[0xAB, 0xAA, 0xDD, 0xEE]);
    data[32..36].copy_from_slice(&[0xAB, 0xCC, 0xDD, 0xAA]);
    let regs = vec![region_ty(0, 48, true, false, true, "", RegionType::Private)];
    let cases = [
        ("AB CC DD EE", 1),
        ("AB ?? DD EE", 2),
        ("AB ?? DD ??", 3),
        ("FF FF FF FF", 0),
    ];
    for (pattern, expected) in cases {
        let prov = TestRegionProvider::new(data.clone(), regs.clone());
        let (pat, mask) = parse_signature(pattern).unwrap();
        let req = ScanRequest {
            pattern: pat,
            mask,
            alignment: 1,
            condition: ScanCondition::ExactValue,
            ..Default::default()
        };
        assert_eq!(sync_scan(&prov, &req).len(), expected, "{pattern}");
    }
}

// ── Live-process self-attach scan test (placeholder). ──

#[test]
#[ignore = "requires a live memflow connector at runtime; not wired in CI"]
fn self_attach_find_mutate_revalidate() {
    // Intentionally unimplemented: a future cross-platform live-memory scan test
    // driven through `crate::provider::memflow`, gated at runtime on a discovered
    // connector (not a compile-time platform).
}
