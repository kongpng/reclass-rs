//! Value/pattern search engine over byte regions from a `Provider`.
//!
//! Port of `src/scanner.{h,cpp}`. **SKELETON** — the scan engine is filled in by
//! the dedicated `scanner` workflow (ARCHITECTURE.md §9). The request/result
//! types + the pure helpers' signatures are in place. The C++ `QtConcurrent`
//! parallelism maps to `rayon` behind the optional `scanner-parallel` feature.

/// `enum class ValueType` (`scanner.h:16-23`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ValueType {
    Int8,
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

/// `enum class ScanCondition` (`scanner.h:27-39`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScanCondition {
    ExactValue,
    UnknownValue,
    Changed,
    Unchanged,
    Increased,
    Decreased,
    BiggerThan,
    SmallerThan,
    Between,
    IncreasedBy,
    DecreasedBy,
}

/// `struct AddressRange` (`scanner.h:43-46`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressRange {
    pub start: u64,
    /// exclusive.
    pub end: u64,
}

/// `struct ScanRequest` (`scanner.h:48-78`).
#[derive(Clone, Debug)]
pub struct ScanRequest {
    pub pattern: Vec<u8>,
    pub mask: Vec<u8>,
    pub filter_executable: bool,
    pub filter_writable: bool,
    pub private_only: bool,
    pub skip_system_modules: bool,
    pub alignment: i32,
    pub max_results: i32,
    pub condition: ScanCondition,
    pub value_size: i32,
    pub value_type: ValueType,
    pub pattern2: Vec<u8>,
    pub start_address: u64,
    pub end_address: u64,
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

/// `struct ScanResult` (`scanner.h:80-85`).
#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    pub address: u64,
    pub region_module: String,
    pub scan_value: Vec<u8>,
    pub previous_value: Vec<u8>,
}

/// `struct ScanStats` (`scanner.h:88-93`).
#[derive(Copy, Clone, Debug, Default)]
pub struct ScanStats {
    pub regions_scanned: i32,
    pub bytes_scanned: u64,
    pub bytes_failed: u64,
    pub ms_elapsed: i32,
}

/// `parseSignature(input, &pattern, &mask, &err)` (`scanner.h:99-100`) — parse
/// an IDA-style signature ("48 8B ?? 05") into `(pattern, mask)`. SKELETON.
pub fn parse_signature(_input: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    todo!("port scanner.cpp parseSignature (workflow: scanner)")
}

/// `serializeValue(type, input, &pattern, &mask, &err)` (`scanner.h:104-106`).
/// SKELETON.
pub fn serialize_value(_ty: ValueType, _input: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    todo!("port scanner.cpp serializeValue (workflow: scanner)")
}

/// `naturalAlignment(type)` (`scanner.h:109`). SKELETON.
pub fn natural_alignment(_ty: ValueType) -> i32 {
    todo!("port scanner.cpp naturalAlignment (workflow: scanner)")
}

/// `valueSizeForType(type)` (`scanner.h:112`). SKELETON.
pub fn value_size_for_type(_ty: ValueType) -> i32 {
    todo!("port scanner.cpp valueSizeForType (workflow: scanner)")
}
