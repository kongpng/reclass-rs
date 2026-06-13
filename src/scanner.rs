//! Value/pattern search engine over byte regions from a [`Provider`].
//!
//! Faithful 1:1 port of `src/scanner.{h,cpp}` (rcx). A pure search engine over
//! the byte regions supplied by the [`crate::provider::Provider`] trait:
//! int/float/string/bytes matching, comparison operators, refine/next-scan, and
//! result sets.
//!
//! The C++ `QtConcurrent`/`QFutureWatcher` async plumbing maps onto a
//! [`ScanEngine`] driving a `std::thread` worker that delivers results through a
//! [`ScanObserver`]. The parity-critical algorithms ([`run_scan`],
//! [`run_rescan`] and the helpers) are pure free functions so unit tests can
//! call them synchronously (mirroring the upstream `syncScan`/`syncRescan`
//! helpers, which only spun a `QEventLoop` to wait for the worker).
//!
//! Optional region-parallelism is gated by the `scanner-parallel` feature
//! (rayon); the initial faithful port keeps the kernel sequential because the
//! result ordering and the post-push `max_results` cap are order-sensitive.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::provider::{MemoryRegion, Provider, RegionType};
use smallvec::SmallVec;

pub mod pointer;

// ─────────────────────────────────────────────────────────────────────────────
// Value / condition vocabulary
// ─────────────────────────────────────────────────────────────────────────────

/// `enum class ValueType` (`scanner.h:15-22`).
///
/// `#[repr(i32)]` and discriminant order match the C++ enum (the UI dropdown
/// index relies on this).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(i32)]
pub enum ValueType {
    Int8 = 0,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float,
    Double,
    Vec2,
    Vec3,
    Vec4,
    Utf8,
    Utf16,
    HexBytes,
}

/// `enum class ScanCondition` (`scanner.h:26-38`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(i32)]
pub enum ScanCondition {
    /// first scan + rescan: match specific bytes
    ExactValue = 0,
    /// first scan only: capture all aligned addresses
    UnknownValue,
    /// rescan: current != previous
    Changed,
    /// rescan: current == previous
    Unchanged,
    /// rescan: current > previous (numeric)
    Increased,
    /// rescan: current < previous (numeric)
    Decreased,
    /// first scan + rescan: current > constant (typed)
    BiggerThan,
    /// first scan + rescan: current < constant (typed)
    SmallerThan,
    /// first scan + rescan: lo <= current <= hi (typed)
    Between,
    /// rescan: current == previous + delta
    IncreasedBy,
    /// rescan: current == previous - delta
    DecreasedBy,
}

/// `struct AddressRange` (`scanner.h:42-45`). `end` is EXCLUSIVE.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressRange {
    pub start: u64,
    /// exclusive.
    pub end: u64,
}

/// `struct ScanRequest` (`scanner.h:47-75`).
#[derive(Clone, Debug)]
pub struct ScanRequest {
    /// literal bytes to match (empty for `UnknownValue`).
    pub pattern: Vec<u8>,
    /// `0xFF` = must match, `0x00` = wildcard.
    pub mask: Vec<u8>,
    /// only scan `+x` regions.
    pub filter_executable: bool,
    /// only scan `+w` regions.
    pub filter_writable: bool,
    /// skip Image (DLL) + Mapped (file) regions.
    pub private_only: bool,
    /// skip well-known system DLLs by `module_name`.
    pub skip_system_modules: bool,
    /// 1 = every byte, 4 = dword, 8 = qword.
    pub alignment: i32,
    pub max_results: i32,
    pub condition: ScanCondition,
    /// bytes per value (for unknown scans).
    pub value_size: i32,
    /// typed compares (`BiggerThan`/`Between`/etc.).
    pub value_type: ValueType,
    /// upper bound for `Between`, delta for `IncreasedBy`/`DecreasedBy`.
    pub pattern2: Vec<u8>,
    /// 0 = no limit (scan all regions).
    pub start_address: u64,
    /// 0 = no limit (scan all regions).
    pub end_address: u64,
    /// if non-empty, only scan within these address ranges (intersected with
    /// provider regions).
    pub constrain_regions: Vec<AddressRange>,
}

impl Default for ScanRequest {
    fn default() -> Self {
        ScanRequest {
            pattern: Vec::new(),
            mask: Vec::new(),
            filter_executable: false,
            filter_writable: false,
            private_only: false,
            skip_system_modules: false,
            alignment: 1,
            max_results: 50000,
            condition: ScanCondition::ExactValue,
            value_size: 4,
            value_type: ValueType::Int32,
            pattern2: Vec::new(),
            start_address: 0,
            end_address: 0,
            constrain_regions: Vec::new(),
        }
    }
}

/// `struct ScanResult` (`scanner.h:77-82`).
#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub address: u64,
    pub region_module: String,
    /// cached bytes at scan/update time.
    pub scan_value: ScanBytes,
    /// value before last update.
    pub previous_value: ScanBytes,
}

pub type ScanBytes = SmallVec<[u8; 16]>;

fn scan_bytes(bytes: &[u8]) -> ScanBytes {
    ScanBytes::from_slice(bytes)
}

/// `struct ScanStats` (`scanner.h:85-90`) — surfaced in the status line.
#[derive(Copy, Clone, Debug, Default)]
pub struct ScanStats {
    pub regions_scanned: i32,
    pub bytes_scanned: u64,
    /// sum of unreadable chunk sizes.
    pub bytes_failed: u64,
    pub ms_elapsed: i32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Pattern parsing — `parseSignature` / `serializeValue` (free functions)
// ─────────────────────────────────────────────────────────────────────────────

/// `static int hexVal(QChar)` (`scanner.cpp:120-126`).
///
/// `'0'..='9' → 0..=9`, `'a'..='f'/'A'..='F' → 10..=15`, else `-1`. Operates on
/// bytes (ASCII only); a non-ASCII / out-of-range byte falls through to `-1`,
/// matching `QChar::unicode()` falling outside the hex ranges.
fn hex_val(c: u8) -> i32 {
    match c {
        b'0'..=b'9' => (c - b'0') as i32,
        b'a'..=b'f' => (c - b'a' + 10) as i32,
        b'A'..=b'F' => (c - b'A' + 10) as i32,
        _ => -1,
    }
}

/// Hex digit value of a `char`, treating non-ASCII as invalid (`-1`).
fn hex_val_char(c: char) -> i32 {
    if c.is_ascii() {
        hex_val(c as u32 as u8)
    } else {
        -1
    }
}

/// `bool parseSignature(...)` (`scanner.cpp:128-214`).
///
/// Parses an IDA-style signature string (`"48 8B ?? 05"`), packed (`"488B??05"`)
/// or C-style (`"\x48\x8B"`) into `(pattern, mask)`. On failure returns the exact
/// upstream error message string (the panel surfaces it verbatim).
///
/// `trimmed[i]` semantics: we index by `char` to mirror Qt `QString` indexing /
/// `.size()` checks and reproduce the offending substring in error messages.
pub fn parse_signature(input: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Empty pattern".to_string());
    }

    let mut pattern: Vec<u8> = Vec::new();
    let mut mask: Vec<u8> = Vec::new();

    // C-style: \xAB\xCD
    if trimmed.starts_with("\\x") {
        // QString::split("\\x", SkipEmptyParts)
        for part in trimmed.split("\\x").filter(|p| !p.is_empty()) {
            let chars: Vec<char> = part.chars().collect();
            if chars.len() != 2 {
                return Err(format!("Invalid C-style byte: \\x{part}"));
            }
            let hi = hex_val_char(chars[0]);
            let lo = hex_val_char(chars[1]);
            if hi < 0 || lo < 0 {
                return Err(format!("Invalid hex char in: \\x{part}"));
            }
            pattern.push(((hi << 4) | lo) as u8);
            mask.push(0xFF);
        }
        // C++: `return !pattern.isEmpty();`
        if pattern.is_empty() {
            return Err("Empty pattern after parsing".to_string());
        }
        return Ok((pattern, mask));
    }

    let has_spaces = trimmed.contains(' ');

    if has_spaces {
        for tok in trimmed.split(' ').filter(|t| !t.is_empty()) {
            if tok == "??" || tok == "?" {
                pattern.push(0);
                mask.push(0);
            } else {
                let chars: Vec<char> = tok.chars().collect();
                if chars.len() == 2 {
                    let hi = hex_val_char(chars[0]);
                    let lo = hex_val_char(chars[1]);
                    if hi < 0 || lo < 0 {
                        return Err(format!("Invalid hex byte: {tok}"));
                    }
                    pattern.push(((hi << 4) | lo) as u8);
                    mask.push(0xFF);
                } else {
                    return Err(format!(
                        "Invalid token: {tok} (expected 2 hex chars or wildcards)"
                    ));
                }
            }
        }
    } else {
        // Packed: "488B??05"
        let chars: Vec<char> = trimmed.chars().collect();
        if chars.len() % 2 != 0 {
            return Err("Odd number of characters in packed pattern".to_string());
        }
        let mut i = 0usize;
        while i < chars.len() {
            let c0 = chars[i];
            let c1 = chars[i + 1];
            if c0 == '?' && c1 == '?' {
                pattern.push(0);
                mask.push(0);
            } else {
                let hi = hex_val_char(c0);
                let lo = hex_val_char(c1);
                if hi < 0 || lo < 0 {
                    return Err(format!("Invalid hex chars at position {i}: {c0}{c1}"));
                }
                pattern.push(((hi << 4) | lo) as u8);
                mask.push(0xFF);
            }
            i += 2;
        }
    }

    if pattern.is_empty() {
        return Err("Empty pattern after parsing".to_string());
    }

    Ok((pattern, mask))
}

/// `QString::startsWith("0x", CaseInsensitive)` → returns the part after the
/// prefix, or `None`.
fn strip_hex_prefix(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'0' && (b[1] == b'x' || b[1] == b'X') {
        Some(&s[2..])
    } else {
        None
    }
}

/// UInt8/UInt16 path (`scanner.cpp:275-301`): try decimal; if that fails OR
/// exceeds `max`, and the string is `0x`-prefixed, retry base-16; the result
/// must parse AND be `<= max`.
fn parse_uint_with_hex(trimmed: &str, max: u32) -> Option<u32> {
    let dec = trimmed.parse::<u32>().ok();
    let ok_dec = matches!(dec, Some(x) if x <= max);
    let v = if ok_dec {
        dec
    } else if let Some(rest) = strip_hex_prefix(trimmed) {
        u32::from_str_radix(rest, 16).ok()
    } else {
        None
    };
    match v {
        Some(x) if x <= max => Some(x),
        _ => None,
    }
}

/// UInt32 path (`scanner.cpp:302-313`): decimal `u32`; on *failure only* retry
/// hex if `0x`.
fn parse_u32_with_hex(trimmed: &str) -> Option<u32> {
    if let Ok(v) = trimmed.parse::<u32>() {
        return Some(v);
    }
    strip_hex_prefix(trimmed).and_then(|rest| u32::from_str_radix(rest, 16).ok())
}

/// UInt64 path (`scanner.cpp:315-326`): decimal `u64`; on failure retry hex.
fn parse_u64_with_hex(trimmed: &str) -> Option<u64> {
    if let Ok(v) = trimmed.parse::<u64>() {
        return Some(v);
    }
    strip_hex_prefix(trimmed).and_then(|rest| u64::from_str_radix(rest, 16).ok())
}

/// `bool serializeValue(...)` (`scanner.cpp:223-428`).
///
/// Serializes a typed value into raw little-endian bytes (`pattern`) and an
/// all-`0xFF` `mask`. On failure returns the exact upstream error message.
pub fn serialize_value(ty: ValueType, input: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() && !matches!(ty, ValueType::Utf8 | ValueType::Utf16) {
        return Err("Empty value".to_string());
    }

    let mut pattern: Vec<u8> = Vec::new();

    match ty {
        ValueType::Int8 => match trimmed.parse::<i32>() {
            Ok(v) if (-128..=127).contains(&v) => {
                pattern.extend_from_slice(&(v as i8).to_le_bytes())
            }
            _ => return Err("Invalid int8 value".to_string()),
        },
        ValueType::Int16 => match trimmed.parse::<i32>() {
            Ok(v) if (-32768..=32767).contains(&v) => {
                pattern.extend_from_slice(&(v as i16).to_le_bytes())
            }
            _ => return Err("Invalid int16 value".to_string()),
        },
        ValueType::Int32 => match trimmed.parse::<i32>() {
            Ok(v) => pattern.extend_from_slice(&v.to_le_bytes()),
            Err(_) => return Err("Invalid int32 value".to_string()),
        },
        ValueType::Int64 => match trimmed.parse::<i64>() {
            Ok(v) => pattern.extend_from_slice(&v.to_le_bytes()),
            Err(_) => return Err("Invalid int64 value".to_string()),
        },
        ValueType::UInt8 => match parse_uint_with_hex(trimmed, 255) {
            Some(v) => pattern.extend_from_slice(&(v as u8).to_le_bytes()),
            None => return Err("Invalid uint8 value".to_string()),
        },
        ValueType::UInt16 => match parse_uint_with_hex(trimmed, 65535) {
            Some(v) => pattern.extend_from_slice(&(v as u16).to_le_bytes()),
            None => return Err("Invalid uint16 value".to_string()),
        },
        ValueType::UInt32 => match parse_u32_with_hex(trimmed) {
            Some(v) => pattern.extend_from_slice(&v.to_le_bytes()),
            None => return Err("Invalid uint32 value".to_string()),
        },
        ValueType::UInt64 => match parse_u64_with_hex(trimmed) {
            Some(v) => pattern.extend_from_slice(&v.to_le_bytes()),
            None => return Err("Invalid uint64 value".to_string()),
        },
        // Reject non-finite results (overflow-to-±inf, the "inf"/"infinity"/"nan"
        // literals) — Rust's parse() returns Ok(inf) on overflow and accepts those
        // literals, but Qt's QString::toFloat/toDouble set ok=false for all of them
        // (scanner.cpp:328-345), so C++ shows a value error and runs no scan.
        ValueType::Float => match trimmed.parse::<f32>() {
            Ok(v) if v.is_finite() => pattern.extend_from_slice(&v.to_le_bytes()),
            _ => return Err("Invalid float value".to_string()),
        },
        ValueType::Double => match trimmed.parse::<f64>() {
            Ok(v) if v.is_finite() => pattern.extend_from_slice(&v.to_le_bytes()),
            _ => return Err("Invalid double value".to_string()),
        },
        ValueType::Vec2 => serialize_vec(trimmed, 2, &mut pattern)?,
        ValueType::Vec3 => serialize_vec(trimmed, 3, &mut pattern)?,
        ValueType::Vec4 => serialize_vec(trimmed, 4, &mut pattern)?,
        ValueType::Utf8 => {
            let encoded = input.as_bytes();
            if encoded.is_empty() {
                return Err("Empty UTF-8 string".to_string());
            }
            pattern.extend_from_slice(encoded);
        }
        ValueType::Utf16 => {
            // UTF-16LE encoding: iterate code units (matches QString unicode()).
            for u in input.encode_utf16() {
                pattern.extend_from_slice(&u.to_le_bytes());
            }
            if pattern.is_empty() {
                return Err("Empty UTF-16 string".to_string());
            }
        }
        ValueType::HexBytes => {
            // Parse hex bytes (like signature but no wildcards). The returned
            // mask is discarded; the final all-0xFF mask overwrites any
            // wildcards anyway (C++: "HexBytes = exact match").
            let (pat, _dummy_mask) = parse_signature(trimmed)?;
            pattern = pat;
        }
    }

    // Set mask to all 0xFF (exact match) for value scans.
    let mask = vec![0xFFu8; pattern.len()];
    Ok((pattern, mask))
}

/// `Vec2`/`Vec3`/`Vec4` path (`scanner.cpp:346-393`). Splits on whitespace
/// (`\s+`, skip empty), requires exactly `n` floats, pushes each LE.
fn serialize_vec(trimmed: &str, n: usize, pattern: &mut Vec<u8>) -> Result<(), String> {
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() != n {
        return Err(format!("Vec{n} requires {n} space-separated floats"));
    }
    for p in parts {
        // Reject non-finite (overflow/inf/nan) per-component, matching Qt toFloat.
        match p.parse::<f32>() {
            Ok(v) if v.is_finite() => pattern.extend_from_slice(&v.to_le_bytes()),
            _ => return Err(format!("Invalid float in vec{n}: {p}")),
        }
    }
    Ok(())
}

/// `int naturalAlignment(ValueType)` (`scanner.cpp:430-454`).
pub fn natural_alignment(ty: ValueType) -> i32 {
    match ty {
        ValueType::Int8 | ValueType::UInt8 | ValueType::Utf8 | ValueType::HexBytes => 1,
        ValueType::Int16 | ValueType::UInt16 | ValueType::Utf16 => 2,
        ValueType::Int32
        | ValueType::UInt32
        | ValueType::Float
        | ValueType::Vec2
        | ValueType::Vec3
        | ValueType::Vec4 => 4,
        ValueType::Int64 | ValueType::UInt64 | ValueType::Double => 8,
    }
}

/// `int valueSizeForType(ValueType)` (`scanner.cpp:456-467`).
pub fn value_size_for_type(ty: ValueType) -> i32 {
    match ty {
        ValueType::Int8 | ValueType::UInt8 => 1,
        ValueType::Int16 | ValueType::UInt16 => 2,
        ValueType::Int32 | ValueType::UInt32 | ValueType::Float => 4,
        ValueType::Int64 | ValueType::UInt64 | ValueType::Double => 8,
        ValueType::Vec2 => 8,
        ValueType::Vec3 => 12,
        ValueType::Vec4 => 16,
        // UTF8/UTF16/HexBytes
        _ => 4,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// System module skip list — `ScanEngine::isSystemModule`
// ─────────────────────────────────────────────────────────────────────────────

/// The hard-coded well-known system DLL/lib set (`scanner.cpp:37-61`), built
/// once. Copied verbatim from the upstream source.
fn k_system() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            // Windows core
            "kernel32",
            "kernelbase",
            "ntdll",
            "win32u",
            "user32",
            "gdi32",
            "gdi32full",
            "advapi32",
            "shell32",
            "shlwapi",
            "shcore",
            "combase",
            "ole32",
            "oleaut32",
            "rpcrt4",
            "sechost",
            "sspicli",
            "msvcrt",
            "ucrtbase",
            "msvcp140",
            "vcruntime140",
            "vcruntime140_1",
            "msvcp_win",
            "bcrypt",
            "bcryptprimitives",
            "cryptbase",
            "crypt32",
            "imm32",
            "dwmapi",
            "uxtheme",
            "comdlg32",
            "comctl32",
            "winmm",
            "ws2_32",
            "iphlpapi",
            "wininet",
            "winhttp",
            "psapi",
            "version",
            "wldap32",
            "secur32",
            "msasn1",
            "wintrust",
            "kernel.appcore",
            "twinapi",
            "twinapi.appcore",
            "windows.storage",
            "wintypes",
            "profapi",
            "dnsapi",
            "userenv",
            "setupapi",
            "cfgmgr32",
            "devobj",
            "powrprof",
            "atl",
            "atl120",
            "atl140",
            "msvcr120",
            "msvcp120",
            // Qt 6 core libs
            "qt6core",
            "qt6gui",
            "qt6widgets",
            "qt6concurrent",
            "qt6network",
            "qt6printsupport",
            "qt6svg",
            "qt6dbus",
            "qt6xml",
            // Linux
            "ld-linux-x86-64.so",
            "libc.so",
            "libc.so.6",
            "libdl.so",
            "libpthread.so",
            "librt.so",
            "libm.so",
            "libstdc++.so",
            "libgcc_s.so",
            // macOS
            "libsystem_kernel.dylib",
            "libsystem_c.dylib",
            "libsystem_pthread.dylib",
            "libsystem_malloc.dylib",
            "libsystem_platform.dylib",
            "libc++.1.dylib",
            "libobjc.A.dylib",
            "dyld",
        ]
        .into_iter()
        .collect()
    })
}

/// `static bool ScanEngine::isSystemModule(const QString&)` (`scanner.cpp:35-79`).
///
/// Lowercases + extension-strips the module name and checks both the stripped
/// stem and the full lowercased name against [`k_system`].
pub fn is_system_module(module_name: &str) -> bool {
    let name = module_name.trim();
    if name.is_empty() {
        return false;
    }

    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        let lower = name.to_ascii_lowercase();
        return is_system_module_lowercase(&lower);
    }

    is_system_module_lowercase(name)
}

fn is_system_module_lowercase(name: &str) -> bool {
    // Strip extension(s): "kernel32.dll" -> "kernel32". Keep the full name
    // check too, so exact entries such as "libc.so.6" continue to match.
    let mut stem_end = name.len();
    loop {
        let stem = &name[..stem_end];
        let dot = stem.find('.');
        match dot {
            Some(d) if d > 0 => {
                let suffix = &stem[d + 1..];
                // Qt toInt() returns 0 on non-numeric; numeric arm requires >0.
                let numeric = suffix.chars().count() <= 3 && suffix.parse::<i32>().unwrap_or(0) > 0;
                let strippable = suffix == "dll"
                    || suffix == "exe"
                    || suffix == "dylib"
                    || suffix == "so"
                    || numeric;
                if strippable {
                    stem_end = d;
                    continue;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    k_system().contains(&name[..stem_end]) || k_system().contains(name)
}

// ─────────────────────────────────────────────────────────────────────────────
// Boyer-Moore-Horspool — `ScanEngine::bmhFind`
// ─────────────────────────────────────────────────────────────────────────────

/// `static int ScanEngine::bmhFind(...)` (`scanner.cpp:86-111`).
///
/// Stable BMH match. Returns the offset of the first match in `data`, or `None`.
/// The bad-char table is built over `pat[0..plen-1]` (final byte excluded); on
/// ties the *last* occurrence among those wins. The shift is keyed on the text
/// tail byte `data[i+last]`.
pub fn bmh_find(data: &[u8], pat: &[u8]) -> Option<usize> {
    BmhNeedle::new(pat).and_then(|needle| needle.find(data))
}

struct BmhNeedle<'a> {
    pat: &'a [u8],
    shift: [usize; 256],
}

impl<'a> BmhNeedle<'a> {
    fn new(pat: &'a [u8]) -> Option<Self> {
        let plen = pat.len();
        if plen == 0 {
            return None;
        }
        let mut shift = [plen; 256];
        if plen > 1 {
            for i in 0..plen - 1 {
                shift[pat[i] as usize] = plen - 1 - i;
            }
        }
        Some(BmhNeedle { pat, shift })
    }

    fn find(&self, data: &[u8]) -> Option<usize> {
        bmh_find_with_shift(data, self.pat, &self.shift)
    }
}

fn bmh_find_with_shift(data: &[u8], pat: &[u8], shift: &[usize; 256]) -> Option<usize> {
    let len = data.len();
    let plen = pat.len();
    if plen == 0 || plen > len {
        return None;
    }
    if plen == 1 {
        return data.iter().position(|&b| b == pat[0]);
    }
    let last = plen - 1;
    let mut i = 0usize;
    // plen <= len guaranteed above, so len - plen >= 0.
    while i <= len - plen {
        let tail = data[i + last];
        if tail == pat[last] {
            let mut j = 0usize;
            while j < last && data[i + j] == pat[j] {
                j += 1;
            }
            if j == last {
                return Some(i);
            }
        }
        i += shift[tail as usize];
    }
    None
}

struct MaskedAnchor<'a> {
    offset: usize,
    finder: memchr::memmem::Finder<'a>,
}

fn masked_anchor<'a>(pat: &'a [u8], mask: &[u8]) -> Option<MaskedAnchor<'a>> {
    if pat.is_empty() || mask.len() < pat.len() {
        return None;
    }

    let mut best_start = 0usize;
    let mut best_len = 0usize;
    let mut run_start = 0usize;
    let mut run_len = 0usize;

    for (i, &m) in mask[..pat.len()].iter().enumerate() {
        if m == 0xFF {
            if run_len == 0 {
                run_start = i;
            }
            run_len += 1;
            if run_len > best_len {
                best_start = run_start;
                best_len = run_len;
            }
        } else {
            run_len = 0;
        }
    }

    if best_len == 0 {
        return None;
    }

    Some(MaskedAnchor {
        offset: best_start,
        finder: memchr::memmem::Finder::new(&pat[best_start..best_start + best_len]),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers — region context + typed comparison
// ─────────────────────────────────────────────────────────────────────────────

/// `static QString formatRegionContext(const MemoryRegion&, uint64_t)`
/// (`scanner.cpp:22-28`).
///
/// Empty module name -> empty string. Else `"name+0xOFFSET"` with lowercase hex,
/// no leading zeros, no `0x` padding (e.g. `"code+0x0"`, `"region0+0x4"`).
fn format_region_context(region: &MemoryRegion, address: u64) -> String {
    if region.module_name.is_empty() {
        return String::new();
    }
    let off = address.saturating_sub(region.base);
    let mut out = String::with_capacity(region.module_name.len() + 3 + 16);
    out.push_str(&region.module_name);
    out.push_str("+0x");
    push_hex_lower(&mut out, off);
    out
}

fn push_scan_result(results: &mut Vec<ScanResult>, max_results: i32, result: ScanResult) -> bool {
    if results.is_empty() {
        results.reserve(max_results.max(0) as usize);
    }
    results.push(result);
    results.len() as i32 >= max_results
}

fn push_hex_lower(out: &mut String, mut value: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    if value == 0 {
        out.push('0');
        return;
    }
    let mut buf = [0u8; 16];
    let mut idx = buf.len();
    while value != 0 {
        idx -= 1;
        buf[idx] = HEX[(value & 0xF) as usize];
        value >>= 4;
    }
    for &b in &buf[idx..] {
        out.push(char::from(b));
    }
}

/// `static int compareTyped(const QByteArray&, const QByteArray&, ValueType)`
/// (`scanner.cpp:471-512`). Three-way `-1/0/1`.
///
/// For each numeric type with `sz >= width`, reads LE and returns
/// `(va>vb) - (va<vb)`. Float/Double use the same form so NaN ⇒ 0 (both
/// comparisons false), matching the C++ IEEE partial order. Otherwise (default
/// arm OR not enough bytes) falls back to an unsigned lexicographic byte compare
/// over `sz` bytes (== `memcmp` sign).
fn compare_typed(a: &[u8], b: &[u8], vt: ValueType) -> i32 {
    let sz = a.len().min(b.len());

    macro_rules! cmp_as {
        ($t:ty, $w:expr) => {
            if sz >= $w {
                let va = <$t>::from_le_bytes(a[..$w].try_into().unwrap());
                let vb = <$t>::from_le_bytes(b[..$w].try_into().unwrap());
                return (va > vb) as i32 - (va < vb) as i32;
            }
        };
    }

    match vt {
        ValueType::Int8 => cmp_as!(i8, 1),
        ValueType::UInt8 => cmp_as!(u8, 1),
        ValueType::Int16 => cmp_as!(i16, 2),
        ValueType::UInt16 => cmp_as!(u16, 2),
        ValueType::Int32 => cmp_as!(i32, 4),
        ValueType::UInt32 => cmp_as!(u32, 4),
        ValueType::Int64 => cmp_as!(i64, 8),
        ValueType::UInt64 => cmp_as!(u64, 8),
        ValueType::Float => cmp_as!(f32, 4),
        ValueType::Double => cmp_as!(f64, 8),
        _ => {}
    }

    // Fallback: unsigned lexicographic byte comparison (== memcmp sign).
    match a[..sz].cmp(&b[..sz]) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Observer plumbing (replaces Qt signals)
// ─────────────────────────────────────────────────────────────────────────────

/// The six Qt signals (`progress`, `finished`, `rescanFinished`, `error`,
/// `scanStats`, `regionsResolved`) map onto these callbacks. The kernels call
/// `progress`/`regions_resolved`/`scan_stats`/`error` *during* the scan;
/// [`ScanEngine`] calls `finished`/`rescan_finished` after the worker returns.
pub trait ScanObserver: Send + Sync {
    fn progress(&self, _percent: i32) {}
    fn regions_resolved(&self, _count: i32, _total_bytes: u64) {}
    fn scan_stats(&self, _stats: ScanStats) {}
    fn error(&self, _message: &str) {}
    fn finished(&self, _results: &[ScanResult]) {}
    fn rescan_finished(&self, _results: &[ScanResult]) {}
}

/// No-op observer — used by every unit test that calls [`run_scan`] directly.
pub struct NullObserver;
impl ScanObserver for NullObserver {}

// ─────────────────────────────────────────────────────────────────────────────
// `run_scan` — the first-scan kernel (scanner.cpp:562-917)
// ─────────────────────────────────────────────────────────────────────────────

const K_CHUNK_BIG: u64 = 2 * 1024 * 1024;
const K_CHUNK_MIN: u64 = 64 * 1024;
const K_ABORT_STRIDE: i32 = 4096;
const K_EXACT_DENSE_PROBE_BYTES: i32 = 4096;
const K_EXACT_DENSE_PROBE_HITS: usize = 4;

/// Resolve the region list for a scan, applying the "no regions → one synthetic
/// region covering [0, size)" fallback (`scanner.cpp:612-620`).
fn resolve_regions(prov: &dyn Provider, regions: Vec<MemoryRegion>) -> Vec<MemoryRegion> {
    if regions.is_empty() {
        vec![MemoryRegion {
            base: 0,
            size: prov.size() as u64,
            readable: true,
            writable: true,
            executable: true, // unknown; include so filters don't exclude the only region
            module_name: String::new(),
            region_type: RegionType::Private,
        }]
    } else {
        regions
    }
}

struct PreparedScanRegion {
    region: MemoryRegion,
    start: u64,
    end: u64,
}

fn scan_region_accepted(req: &ScanRequest, r: &MemoryRegion) -> bool {
    if req.filter_executable && !r.executable {
        return false;
    }
    if req.filter_writable && !r.writable {
        return false;
    }
    if req.private_only && r.region_type != RegionType::Private {
        return false;
    }
    if req.skip_system_modules && is_system_module(&r.module_name) {
        return false;
    }
    true
}

fn prepare_scan_regions(
    regions: Vec<MemoryRegion>,
    req: &ScanRequest,
    has_range: bool,
) -> (Vec<PreparedScanRegion>, u64) {
    let mut prepared = Vec::with_capacity(regions.len());
    let mut total_bytes = 0u64;
    for region in regions {
        if !scan_region_accepted(req, &region) {
            continue;
        }
        let mut start = region.base;
        let mut end = region.base.saturating_add(region.size);
        if has_range {
            if end <= req.start_address || start >= req.end_address {
                continue;
            }
            start = start.max(req.start_address);
            end = end.min(req.end_address);
        }
        if end <= start {
            continue;
        }
        total_bytes = total_bytes.saturating_add(end - start);
        prepared.push(PreparedScanRegion { region, start, end });
    }
    (prepared, total_bytes)
}

/// `QVector<ScanResult> ScanEngine::runScan(...)` (`scanner.cpp:562-917`).
///
/// Convenience entry point that enumerates regions fresh on each call. The
/// region cache lives in [`ScanEngine`] (see [`run_scan_in_regions`]).
pub fn run_scan(
    prov: &dyn Provider,
    req: &ScanRequest,
    abort: &AtomicBool,
    obs: &dyn ScanObserver,
) -> Vec<ScanResult> {
    let regions = prov.enumerate_regions();
    run_scan_in_regions(prov, regions, req, abort, obs)
}

/// The first-scan kernel with the region list supplied by the caller (so the
/// engine's cache can be reused). Port of `scanner.cpp:562-917`.
pub fn run_scan_in_regions(
    prov: &dyn Provider,
    regions_in: Vec<MemoryRegion>,
    req: &ScanRequest,
    abort: &AtomicBool,
    obs: &dyn ScanObserver,
) -> Vec<ScanResult> {
    let timer = Instant::now();
    let mut results: Vec<ScanResult> = Vec::new();
    let cond = req.condition;

    // Compare-against-previous conditions on a first scan have no baseline, so
    // they capture every aligned address (rescan applies the real filter).
    let is_capture = matches!(
        cond,
        ScanCondition::UnknownValue
            | ScanCondition::Changed
            | ScanCondition::Unchanged
            | ScanCondition::Increased
            | ScanCondition::Decreased
            | ScanCondition::IncreasedBy
            | ScanCondition::DecreasedBy
    );
    // Typed-const compare on first scan: filter inline during capture.
    let is_typed_const = matches!(
        cond,
        ScanCondition::BiggerThan | ScanCondition::SmallerThan | ScanCondition::Between
    );

    // The C++ `!prov` guard is for the shared_ptr; `&dyn Provider` is always
    // present, so it is dropped.
    if !is_capture && !is_typed_const && req.pattern.is_empty() {
        return results;
    }
    if is_typed_const && req.pattern.is_empty() {
        return results;
    }

    let mut regions = resolve_regions(prov, regions_in);

    let pattern_len: i32 = if is_capture || is_typed_const {
        req.value_size
    } else {
        req.pattern.len() as i32
    };
    let empty: Vec<u8> = Vec::new();
    let pat: &[u8] = if is_capture { &empty } else { &req.pattern };
    let msk: &[u8] = if is_capture { &empty } else { &req.mask };
    let alignment = req.alignment.max(1);
    let val_size: i32 = if is_capture || is_typed_const {
        req.value_size
    } else {
        pattern_len
    };
    let has_range =
        (req.start_address != 0 || req.end_address != 0) && req.end_address > req.start_address;

    // BMH eligibility: pattern ≥4 bytes, no wildcards, alignment 1.
    let mut bmh_eligible = !is_capture && pattern_len >= 4 && alignment == 1;
    if bmh_eligible && !msk.is_empty() {
        for j in 0..pattern_len as usize {
            if msk[j] != 0xFF {
                bmh_eligible = false;
                break;
            }
        }
    }
    let memmem_finder = if bmh_eligible {
        Some(memchr::memmem::Finder::new(&pat[..pattern_len as usize]))
    } else {
        None
    };
    let bmh_needle = if bmh_eligible {
        BmhNeedle::new(&pat[..pattern_len as usize])
    } else {
        None
    };
    let masked_anchor = if !is_capture && !is_typed_const && !bmh_eligible {
        masked_anchor(&pat[..pattern_len as usize], msk)
    } else {
        None
    };

    // If constrainRegions specified, intersect with provider regions.
    if !req.constrain_regions.is_empty() {
        regions = intersect_constraints(regions, &req.constrain_regions);
    }

    let needs_prepared_regions = has_range
        || req.filter_executable
        || req.filter_writable
        || req.private_only
        || req.skip_system_modules;
    let mut prepared_regions = Vec::new();
    let (total_bytes, accepted_regions) = if needs_prepared_regions {
        let (prepared, total_bytes) =
            prepare_scan_regions(std::mem::take(&mut regions), req, has_range);
        let accepted_regions = prepared.len() as i32;
        prepared_regions = prepared;
        (total_bytes, accepted_regions)
    } else {
        let total_bytes = regions.iter().map(|region| region.size).sum();
        (total_bytes, regions.len() as i32)
    };
    obs.regions_resolved(accepted_regions, total_bytes);

    if total_bytes == 0 {
        return results;
    }

    let mut scanned_bytes: u64 = 0;
    let mut failed_bytes: u64 = 0;
    let mut last_pct: i32 = -1;
    let mut chunk: Vec<u8> = Vec::new();

    'scan: {
        macro_rules! scan_region {
            ($region:expr, $reg_start:expr, $reg_end:expr) => {{
        if abort.load(Ordering::Relaxed) {
            break 'scan;
        }
        let region = $region;
        let reg_start = $reg_start;
        let reg_end = $reg_end;
        let reg_size = reg_end - reg_start;
        if pattern_len as u64 > reg_size {
            scanned_bytes += reg_size;
            continue;
        }

        let overlap = pattern_len - 1;
        let mut target_chunk = K_CHUNK_BIG.min(reg_size);
        if reg_size < K_CHUNK_MIN {
            target_chunk = reg_size;
        }
        chunk.resize(target_chunk as usize, 0);

        let mut off: u64 = 0;
        while off < reg_size {
            if abort.load(Ordering::Relaxed) {
                break;
            }
            let remaining = reg_size - off;
            let read_len = (chunk.len() as u64).min(remaining) as i32;

            if !prov.read(reg_start + off, &mut chunk[..read_len as usize]) {
                // Skip unreadable chunk; track for status-line surfacing.
                failed_bytes += read_len as u64;
                scanned_bytes += read_len as u64;
                off += read_len as u64;
                continue;
            }

            let scan_end: i32 = read_len - pattern_len; // last valid match start (may be < 0)
            let data = &chunk[..read_len as usize];

            if is_capture {
                // Capture every aligned address.
                let mut i: i32 = 0;
                while i <= scan_end {
                    if (i & (K_ABORT_STRIDE - 1)) == 0 && abort.load(Ordering::Relaxed) {
                        break 'scan;
                    }
                    let addr = reg_start + off + i as u64;
                    let iu = i as usize;
                    if push_scan_result(
                        &mut results,
                        req.max_results,
                        ScanResult {
                            address: addr,
                            region_module: format_region_context(region, addr),
                            scan_value: scan_bytes(&data[iu..iu + val_size as usize]),
                            previous_value: ScanBytes::new(),
                        },
                    ) {
                        break 'scan;
                    }
                    i += alignment;
                }
            } else if is_typed_const {
                let lo = &req.pattern;
                let hi = &req.pattern2;
                let mut i: i32 = 0;
                while i <= scan_end {
                    if (i & (K_ABORT_STRIDE - 1)) == 0 && abort.load(Ordering::Relaxed) {
                        break 'scan;
                    }
                    let iu = i as usize;
                    let val = &data[iu..iu + val_size as usize];
                    let cmp_lo = compare_typed(val, lo, req.value_type);
                    let ok = match cond {
                        ScanCondition::BiggerThan => cmp_lo > 0,
                        ScanCondition::SmallerThan => cmp_lo < 0,
                        ScanCondition::Between if !hi.is_empty() => {
                            cmp_lo >= 0 && compare_typed(val, hi, req.value_type) <= 0
                        }
                        _ => false,
                    };
                    if ok {
                        let addr = reg_start + off + i as u64;
                        if push_scan_result(
                            &mut results,
                            req.max_results,
                            ScanResult {
                                address: addr,
                                region_module: format_region_context(region, addr),
                                scan_value: scan_bytes(val),
                                previous_value: ScanBytes::new(),
                            },
                        ) {
                            break 'scan;
                        }
                    }
                    i += alignment;
                }
            } else if bmh_eligible {
                let finder = memmem_finder
                    .as_ref()
                    .expect("BMH eligibility requires a non-empty pattern");
                let bmh_needle = bmh_needle
                    .as_ref()
                    .expect("BMH eligibility requires a non-empty pattern");
                let mut search_from: i32 = 0;
                let mut dense_probe_hits = 0usize;
                let mut use_bmh = false;
                while search_from <= scan_end {
                    if abort.load(Ordering::Relaxed) {
                        break 'scan;
                    }
                    let slice = &data[search_from as usize..read_len as usize];
                    let found = if use_bmh {
                        bmh_needle.find(slice)
                    } else {
                        finder.find(slice)
                    };
                    match found {
                        None => break,
                        Some(hit) => {
                            let abs_i = search_from + hit as i32;
                            if abs_i > scan_end {
                                break;
                            }
                            if !use_bmh && abs_i <= K_EXACT_DENSE_PROBE_BYTES {
                                dense_probe_hits += 1;
                                if dense_probe_hits >= K_EXACT_DENSE_PROBE_HITS {
                                    use_bmh = true;
                                }
                            }
                            let n = 16.min(read_len - abs_i) as usize;
                            let addr = reg_start + off + abs_i as u64;
                            if push_scan_result(
                                &mut results,
                                req.max_results,
                                ScanResult {
                                    address: addr,
                                    region_module: format_region_context(region, addr),
                                    scan_value: scan_bytes(
                                        &data[abs_i as usize..abs_i as usize + n],
                                    ),
                                    previous_value: ScanBytes::new(),
                                },
                            ) {
                                break 'scan;
                            }
                            search_from = abs_i + 1; // overlapping matches
                        }
                    }
                }
            } else if let Some(anchor) = masked_anchor.as_ref() {
                // Masked signatures can skip directly between fixed-byte anchors,
                // then verify the full mask only at candidate starts.
                let alignment = alignment as usize;
                let mut anchor_from = anchor.offset;
                while anchor_from < read_len as usize {
                    if abort.load(Ordering::Relaxed) {
                        break 'scan;
                    }
                    let Some(hit) = anchor.finder.find(&data[anchor_from..read_len as usize])
                    else {
                        break;
                    };
                    let anchor_i = anchor_from + hit;
                    let i = anchor_i - anchor.offset;
                    if i as i32 > scan_end {
                        break;
                    }
                    anchor_from = anchor_i + 1;
                    if i % alignment != 0 {
                        continue;
                    }

                    let mut matched = true;
                    for j in 0..pattern_len as usize {
                        if (data[i + j] & msk[j]) != (pat[j] & msk[j]) {
                            matched = false;
                            break;
                        }
                    }
                    if matched {
                        let n = 16.min(read_len - i as i32) as usize;
                        let addr = reg_start + off + i as u64;
                        if push_scan_result(
                            &mut results,
                            req.max_results,
                            ScanResult {
                                address: addr,
                                region_module: format_region_context(region, addr),
                                scan_value: scan_bytes(&data[i..i + n]),
                                previous_value: ScanBytes::new(),
                            },
                        ) {
                            break 'scan;
                        }
                    }
                }
            } else {
                // Naive aligned matcher (handles all-wildcard masks).
                let mut i: i32 = 0;
                while i <= scan_end {
                    if (i & (K_ABORT_STRIDE - 1)) == 0 && abort.load(Ordering::Relaxed) {
                        break 'scan;
                    }
                    let iu = i as usize;
                    let mut matched = true;
                    for j in 0..pattern_len as usize {
                        if (data[iu + j] & msk[j]) != (pat[j] & msk[j]) {
                            matched = false;
                            break;
                        }
                    }
                    if matched {
                        let n = 16.min(read_len - i) as usize;
                        let addr = reg_start + off + i as u64;
                        if push_scan_result(
                            &mut results,
                            req.max_results,
                            ScanResult {
                                address: addr,
                                region_module: format_region_context(region, addr),
                                scan_value: scan_bytes(&data[iu..iu + n]),
                                previous_value: ScanBytes::new(),
                            },
                        ) {
                            break 'scan;
                        }
                    }
                    i += alignment;
                }
            }

            // Advance with overlap to catch patterns that straddle chunks.
            let advance: u64 = if read_len as u64 >= remaining {
                remaining // last chunk, no overlap needed
            } else if read_len > overlap {
                let mut adv = (read_len - overlap) as u64;
                if alignment > 1 {
                    let next_off = off + adv;
                    let a = alignment as u64;
                    let aligned = ((next_off + a - 1) / a) * a;
                    adv = aligned - off;
                }
                adv
            } else {
                1 // prevent infinite loop on tiny regions
            };
            scanned_bytes += advance;
            off += advance;

            // Throttled progress.
            let mut pct = (scanned_bytes * 100 / total_bytes) as i32;
            if pct > 100 {
                pct = 100;
            }
            if pct != last_pct {
                last_pct = pct;
                obs.progress(pct);
            }
        }
            }};
        }

        if needs_prepared_regions {
            for prepared in &prepared_regions {
                scan_region!(&prepared.region, prepared.start, prepared.end);
            }
        } else {
            for region in &regions {
                scan_region!(region, region.base, region.base.saturating_add(region.size));
            }
        }
    }

    // done: emit stats
    let stats = ScanStats {
        regions_scanned: accepted_regions,
        bytes_scanned: scanned_bytes,
        bytes_failed: failed_bytes,
        ms_elapsed: timer.elapsed().as_millis() as i32,
    };
    obs.scan_stats(stats);

    results
}

/// `scanner.cpp:642-674` — sort + merge constraint ranges, then clip each
/// provider region to the intersection with the merged constraints.
fn intersect_constraints(
    regions: Vec<MemoryRegion>,
    constrain_regions: &[AddressRange],
) -> Vec<MemoryRegion> {
    if constraints_strictly_sorted_disjoint(constrain_regions) {
        if regions.windows(2).all(|pair| pair[0].base <= pair[1].base) {
            return intersect_sorted_regions_with_constraints(&regions, constrain_regions);
        }
        return intersect_unsorted_regions_with_constraints(&regions, constrain_regions);
    }

    let mut constraints = constrain_regions.to_vec();
    constraints.sort_by(|a, b| a.start.cmp(&b.start));

    let mut merged: Vec<AddressRange> = Vec::new();
    for c in constraints {
        if c.end <= c.start {
            continue; // skip degenerate ranges
        }
        if let Some(last) = merged.last_mut() {
            if c.start <= last.end {
                last.end = last.end.max(c.end);
                continue;
            }
        }
        merged.push(c);
    }

    if regions.windows(2).all(|pair| pair[0].base <= pair[1].base) {
        return intersect_sorted_regions_with_constraints(&regions, &merged);
    }

    intersect_unsorted_regions_with_constraints(&regions, &merged)
}

fn constraints_strictly_sorted_disjoint(constraints: &[AddressRange]) -> bool {
    let mut last_end = None;
    for c in constraints {
        if c.end <= c.start {
            return false;
        }
        if let Some(end) = last_end {
            if c.start <= end {
                return false;
            }
        }
        last_end = Some(c.end);
    }
    true
}

fn intersect_sorted_regions_with_constraints(
    regions: &[MemoryRegion],
    merged: &[AddressRange],
) -> Vec<MemoryRegion> {
    let mut clipped: Vec<MemoryRegion> = Vec::new();
    let mut first_overlap = 0usize;
    for region in regions {
        let r_end = region.base.saturating_add(region.size);
        while first_overlap < merged.len() && merged[first_overlap].end <= region.base {
            first_overlap += 1;
        }
        for c in &merged[first_overlap..] {
            if c.start >= r_end {
                break;
            }
            let i_start = region.base.max(c.start);
            let i_end = r_end.min(c.end);
            if i_end <= i_start {
                continue;
            }
            let mut sub = region.clone();
            sub.base = i_start;
            sub.size = i_end - i_start;
            clipped.push(sub);
        }
    }
    clipped
}

fn intersect_unsorted_regions_with_constraints(
    regions: &[MemoryRegion],
    merged: &[AddressRange],
) -> Vec<MemoryRegion> {
    let mut clipped: Vec<MemoryRegion> = Vec::new();
    for region in regions {
        let r_end = region.base.saturating_add(region.size);
        let first_overlap = merged.partition_point(|c| c.end <= region.base);
        for c in &merged[first_overlap..] {
            if c.start >= r_end {
                break;
            }
            let i_start = region.base.max(c.start);
            let i_end = r_end.min(c.end);
            if i_end <= i_start {
                continue;
            }
            let mut sub = region.clone();
            sub.base = i_start;
            sub.size = i_end - i_start;
            clipped.push(sub);
        }
    }
    clipped
}

// ─────────────────────────────────────────────────────────────────────────────
// `run_rescan` — the refine kernel (scanner.cpp:949-1136)
// ─────────────────────────────────────────────────────────────────────────────

const K_RESCAN_CHUNK: u64 = 256 * 1024;
const K_RESCAN_SPARSE_SPAN_FACTOR: u64 = 64;

/// `QVector<ScanResult> ScanEngine::runRescan(...)` (`scanner.cpp:949-1136`).
///
/// Refines a result set against current memory. `previous_value` is snapshotted
/// from `scan_value` before re-reading, so comparison baselines are the prior
/// cached bytes. The provider read return is ignored entirely (failed reads
/// leave zero-filled bytes). Returns the seed unchanged when no filter applies.
#[allow(clippy::too_many_arguments)]
pub fn run_rescan(
    prov: &dyn Provider,
    mut results: Vec<ScanResult>,
    read_size: i32,
    condition: ScanCondition,
    value_type: ValueType,
    filter_pattern: &[u8],
    filter_mask: &[u8],
    filter_pattern2: &[u8],
    abort: &AtomicBool,
    obs: &dyn ScanObserver,
) -> Vec<ScanResult> {
    let total = results.len();
    if total == 0 {
        return results;
    }

    let has_exact_filter = !filter_pattern.is_empty() && condition == ScanCondition::ExactValue;
    let has_comparison = matches!(
        condition,
        ScanCondition::Changed
            | ScanCondition::Unchanged
            | ScanCondition::Increased
            | ScanCondition::Decreased
    );
    let has_typed_const = matches!(
        condition,
        ScanCondition::BiggerThan | ScanCondition::SmallerThan | ScanCondition::Between
    );
    let has_delta = matches!(
        condition,
        ScanCondition::IncreasedBy | ScanCondition::DecreasedBy
    );
    let needs_filter = has_exact_filter || has_comparison || has_typed_const || has_delta;
    let exact_filter_unmasked = has_exact_filter
        && filter_mask.len() >= filter_pattern.len()
        && filter_mask[..filter_pattern.len()]
            .iter()
            .all(|&mask| mask == 0xFF);

    // Save previous values while keeping the spare buffer available for the
    // freshly read bytes.
    for r in &mut results {
        r.previous_value.clear();
        std::mem::swap(&mut r.previous_value, &mut r.scan_value);
    }

    // Most UI result sets are already produced in address order by first scan.
    // Avoid allocating and sorting an index vector on that common path; keep the
    // stable sorted-index fallback for imported/restored unsorted result sets.
    let results_sorted = results
        .windows(2)
        .all(|pair| pair[0].address <= pair[1].address);
    let mut order: Vec<usize> = Vec::new();
    if !results_sorted {
        order = (0..total).collect();
        order.sort_by(|&a, &b| results[a].address.cmp(&results[b].address));
    }

    // Positive-width filtered rescans can use the freshly populated scan_value as
    // match state: non-matches keep the empty buffer left by the previous-value
    // swap above. Keep the explicit bitmap only for unusual zero-width filters.
    let filter_by_scan_value = needs_filter && read_size > 0;
    let mut matched = if needs_filter && !filter_by_scan_value {
        vec![false; total]
    } else {
        Vec::new()
    };

    let mut updated: i32 = 0;
    let mut last_pct: i32 = -1;
    let mut i: usize = 0;
    let mut chunk: Vec<u8> = Vec::new();

    while i < total && !abort.load(Ordering::Relaxed) {
        let span_base_idx = if results_sorted { i } else { order[i] };
        let span_base = results[span_base_idx].address;
        let mut span_end = i;

        // Extend span while next result fits in the same chunk.
        while span_end + 1 < total {
            let next_idx = if results_sorted {
                span_end + 1
            } else {
                order[span_end + 1]
            };
            let end_addr = results[next_idx].address.saturating_add(read_size as u64);
            if end_addr - span_base > K_RESCAN_CHUNK {
                break;
            }
            span_end += 1;
        }

        let span_last_idx = if results_sorted {
            span_end
        } else {
            order[span_end]
        };
        let span_last = results[span_last_idx].address;
        let mut chunk_len = span_last
            .saturating_add(read_size as u64)
            .saturating_sub(span_base) as usize;
        let span_count = (span_end - i + 1) as u64;
        let useful_bytes = span_count.saturating_mul(read_size.max(0) as u64);
        let sparse_span = !prov.prefers_coalesced_rescan_reads()
            && useful_bytes > 0
            && (chunk_len as u64) > useful_bytes.saturating_mul(K_RESCAN_SPARSE_SPAN_FACTOR);
        if sparse_span {
            span_end = i;
            chunk_len = read_size.max(0) as usize;
        }
        chunk.resize(chunk_len, 0);
        if !prov.read(span_base, &mut chunk[..chunk_len]) {
            // Read return value ignored semantically: failed reads leave zeros.
            chunk[..chunk_len].fill(0);
        }

        for j in i..=span_end {
            let idx = if results_sorted { j } else { order[j] };
            let off = (results[idx].address - span_base) as usize;
            // chunk.mid(off, readSize): clamps at chunk end if truncated.
            let sv = if off <= chunk_len {
                let end = (off + read_size as usize).min(chunk_len);
                &chunk[off..end]
            } else {
                &[][..]
            };

            // Apply exact-value filter.
            let mut ok = !needs_filter;
            if has_exact_filter {
                let pat_len = filter_pattern.len();
                if sv.len() >= pat_len {
                    if exact_filter_unmasked {
                        ok = &sv[..pat_len] == filter_pattern;
                    } else {
                        ok = true;
                        for k in 0..pat_len {
                            if (sv[k] & filter_mask[k]) != (filter_pattern[k] & filter_mask[k]) {
                                ok = false;
                                break;
                            }
                        }
                    }
                }
            }

            // Apply comparison-based filter.
            if has_comparison && !results[idx].previous_value.is_empty() {
                let cmp = compare_typed(sv, &results[idx].previous_value, value_type);
                ok = match condition {
                    ScanCondition::Changed => cmp != 0,
                    ScanCondition::Unchanged => cmp == 0,
                    ScanCondition::Increased => cmp > 0,
                    ScanCondition::Decreased => cmp < 0,
                    _ => ok,
                };
            }

            // Typed const compare (BiggerThan / SmallerThan / Between).
            if has_typed_const && !filter_pattern.is_empty() {
                let cmp_lo = compare_typed(sv, filter_pattern, value_type);
                ok = match condition {
                    ScanCondition::BiggerThan => cmp_lo > 0,
                    ScanCondition::SmallerThan => cmp_lo < 0,
                    ScanCondition::Between if !filter_pattern2.is_empty() => {
                        cmp_lo >= 0 && compare_typed(sv, filter_pattern2, value_type) <= 0
                    }
                    _ => ok,
                };
            }

            // Delta compare (IncreasedBy / DecreasedBy).
            if has_delta && !results[idx].previous_value.is_empty() && !filter_pattern.is_empty() {
                let prev = &results[idx].previous_value;
                let sz = prev.len().min(filter_pattern.len());
                if sv.len() >= sz {
                    ok = delta_check(value_type, condition, prev, filter_pattern, sv, sz);
                }
            }

            if ok {
                if needs_filter && !filter_by_scan_value {
                    matched[idx] = true;
                }
                results[idx].scan_value.clear();
                results[idx].scan_value.extend_from_slice(sv);
            }
        }

        updated += (span_end - i + 1) as i32;
        i = span_end + 1;

        let pct = updated * 100 / total as i32;
        if pct != last_pct {
            last_pct = pct;
            obs.progress(pct);
        }
    }

    if !needs_filter && i < total {
        for j in i..total {
            let idx = if results_sorted { j } else { order[j] };
            let result = &mut results[idx];
            result.scan_value.clear();
            result.scan_value.extend_from_slice(&result.previous_value);
        }
    }

    if needs_filter {
        if filter_by_scan_value {
            results.retain(|r| !r.scan_value.is_empty());
            return results;
        }
        let mut filtered: Vec<ScanResult> = Vec::with_capacity(total);
        for (k, r) in results.into_iter().enumerate() {
            if matched[k] {
                filtered.push(r);
            }
        }
        return filtered;
    }

    results
}

/// `addAndCheck` lambda (`scanner.cpp:1069-1102`).
///
/// `ok` is `false` unless a typed arm matches; integers use two's-complement
/// wrapping; floats use IEEE `+`/`-` then a bitwise-exact `==` (NaN never
/// equals). An unsupported value type leaves `ok` false → the row is dropped.
fn delta_check(
    value_type: ValueType,
    condition: ScanCondition,
    prev: &[u8],
    delta: &[u8],
    cur: &[u8],
    sz: usize,
) -> bool {
    let inc = condition == ScanCondition::IncreasedBy;

    macro_rules! check_int {
        ($t:ty, $w:expr) => {{
            if sz < $w {
                return false;
            }
            let p = <$t>::from_le_bytes(prev[..$w].try_into().unwrap());
            let d = <$t>::from_le_bytes(delta[..$w].try_into().unwrap());
            let c = <$t>::from_le_bytes(cur[..$w].try_into().unwrap());
            let expected = if inc {
                p.wrapping_add(d)
            } else {
                p.wrapping_sub(d)
            };
            c == expected
        }};
    }

    macro_rules! check_float {
        ($t:ty, $w:expr) => {{
            if sz < $w {
                return false;
            }
            let p = <$t>::from_le_bytes(prev[..$w].try_into().unwrap());
            let d = <$t>::from_le_bytes(delta[..$w].try_into().unwrap());
            let c = <$t>::from_le_bytes(cur[..$w].try_into().unwrap());
            let expected = if inc { p + d } else { p - d };
            c == expected
        }};
    }

    match value_type {
        ValueType::Int8 => check_int!(i8, 1),
        ValueType::UInt8 => check_int!(u8, 1),
        ValueType::Int16 => check_int!(i16, 2),
        ValueType::UInt16 => check_int!(u16, 2),
        ValueType::Int32 => check_int!(i32, 4),
        ValueType::UInt32 => check_int!(u32, 4),
        ValueType::Int64 => check_int!(i64, 8),
        ValueType::UInt64 => check_int!(u64, 8),
        ValueType::Float => check_float!(f32, 4),
        ValueType::Double => check_float!(f64, 8),
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `ScanEngine` — async front-door (replaces the QObject)
// ─────────────────────────────────────────────────────────────────────────────

/// `class ScanEngine : public QObject` (`scanner.h:113-169`).
///
/// Owns the region cache + abort flag + running flag. `start`/`start_rescan`
/// spawn a worker thread (the C++ `QtConcurrent::run` + `QFutureWatcher`) and
/// deliver completion via the supplied [`ScanObserver`]. Only one scan runs at a
/// time, so the region cache needs no lock.
pub struct ScanEngine {
    abort: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    cached_regions: Vec<MemoryRegion>,
    /// Cache key = the `Arc<dyn Provider>` data-pointer address (a plain `usize`
    /// is `Send`). Matches the C++ `m_cachedProvider == prov.get()` keying.
    cached_provider_ptr: usize,
    /// Join handle for the in-flight worker (so tests can deterministically wait
    /// without an event loop).
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Default for ScanEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanEngine {
    /// `ScanEngine(QObject*)` (`scanner.cpp:516-520`).
    pub fn new() -> Self {
        ScanEngine {
            abort: Arc::new(AtomicBool::new(false)),
            running: Arc::new(AtomicBool::new(false)),
            cached_regions: Vec::new(),
            cached_provider_ptr: 0,
            worker: None,
        }
    }

    /// `bool isRunning() const` (`scanner.cpp:522-524`).
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// `void abort()` (`scanner.cpp:526-528`).
    pub fn abort(&self) {
        self.abort.store(true, Ordering::Relaxed);
    }

    /// `void invalidateRegionCache()` (`scanner.cpp:113-116`).
    pub fn invalidate_region_cache(&mut self) {
        self.cached_regions.clear();
        self.cached_provider_ptr = 0;
    }

    /// Block until any in-flight worker finishes (test convenience; replaces the
    /// upstream `QEventLoop` / `QSignalSpy::wait`).
    pub fn wait(&mut self) {
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }

    /// Resolve the region list, using the cache when the provider matches.
    fn resolve_cached(&mut self, prov: &Arc<dyn Provider + Send + Sync>) -> Vec<MemoryRegion> {
        let key = Arc::as_ptr(prov) as *const () as usize;
        if self.cached_provider_ptr == key && !self.cached_regions.is_empty() {
            self.cached_regions.clone()
        } else {
            let regions = prov.enumerate_regions();
            self.cached_regions = regions.clone();
            self.cached_provider_ptr = key;
            regions
        }
    }

    /// `void start(shared_ptr<Provider>, const ScanRequest&)`
    /// (`scanner.cpp:530-560`).
    ///
    /// Validation (delivered synchronously via `obs.error` before spawning),
    /// then a worker thread runs [`run_scan_in_regions`] and delivers via
    /// `obs.finished`. `UnknownValue` bypasses pattern/mask validation.
    pub fn start(
        &mut self,
        prov: Arc<dyn Provider + Send + Sync>,
        req: ScanRequest,
        obs: Arc<dyn ScanObserver>,
    ) {
        if self.is_running() {
            return;
        }

        if req.condition != ScanCondition::UnknownValue {
            if req.pattern.is_empty() {
                obs.error("Empty pattern");
                return;
            }
            if req.pattern.len() != req.mask.len() {
                obs.error("Pattern and mask size mismatch");
                return;
            }
        }

        self.abort.store(false, Ordering::Relaxed);
        self.running.store(true, Ordering::Release);

        // Resolve regions (cache hit or enumerate) on the calling thread, so the
        // cache is updated synchronously and matches the C++ `mutable` cache.
        let regions = self.resolve_cached(&prov);

        let abort = Arc::clone(&self.abort);
        let running = Arc::clone(&self.running);
        let handle = std::thread::spawn(move || {
            let results = run_scan_in_regions(prov.as_ref(), regions, &req, &abort, obs.as_ref());
            obs.finished(&results);
            running.store(false, Ordering::Release);
        });
        self.worker = Some(handle);
    }

    /// `void startRescan(...)` (`scanner.cpp:919-947`).
    #[allow(clippy::too_many_arguments)]
    pub fn start_rescan(
        &mut self,
        prov: Arc<dyn Provider + Send + Sync>,
        results: Vec<ScanResult>,
        read_size: i32,
        condition: ScanCondition,
        value_type: ValueType,
        filter_pattern: Vec<u8>,
        filter_mask: Vec<u8>,
        filter_pattern2: Vec<u8>,
        obs: Arc<dyn ScanObserver>,
    ) {
        if self.is_running() {
            return;
        }

        self.abort.store(false, Ordering::Relaxed);
        self.running.store(true, Ordering::Release);

        let abort = Arc::clone(&self.abort);
        let running = Arc::clone(&self.running);
        let handle = std::thread::spawn(move || {
            let out = run_rescan(
                prov.as_ref(),
                results,
                read_size,
                condition,
                value_type,
                &filter_pattern,
                &filter_mask,
                &filter_pattern2,
                &abort,
                obs.as_ref(),
            );
            obs.rescan_finished(&out);
            running.store(false, Ordering::Release);
        });
        self.worker = Some(handle);
    }
}

#[cfg(test)]
mod tests;
