//! Golden-oracle integration test for the `format` subsystem.
//!
//! Translated 1:1 from `tests/test_format.cpp` (the upstream QtTest `TestFormat`
//! suite, 44 cases). Where the C++ asserts output-exact strings (`QCOMPARE`),
//! these assert the same strings byte-for-byte against the public `format` API.
//! Captured golden run: `_oracle/logs/test_format.txt` (44 passed, 0 failed).
//!
//! Run with `cargo test --no-default-features --test format_oracle`.

use reclass::core::{is_valid_primitive_ptr_target, Node, NodeKind};
use reclass::format as fmt;
use reclass::provider::BufferProvider;

// testTypeName (test_format.cpp:9-13)
#[test]
fn type_name() {
    let s = fmt::type_name(NodeKind::Float);
    assert_eq!(s.trim(), "float");
    assert_eq!(s.encode_utf16().count(), 14); // kColType
}

// testFmtInt32 (test_format.cpp:15-19)
#[test]
fn fmt_int32() {
    assert_eq!(fmt::fmt_int32(-42), "-42");
    assert_eq!(fmt::fmt_int32(0), "0");
}

// testFmtFloat (test_format.cpp:21-64)
#[test]
fn fmt_float() {
    let c = |v: f32, e: &str| assert_eq!(fmt::fmt_float(v), e, "fmt_float({v})");
    c(3.14159, "3.1416f");
    c(-3.14159, "-3.1416f");
    c(0.0, "0.0000f");
    c(0.02, "0.0200f");
    c(-0.069, "-0.0690f");
    c(15.6543, "15.654f");
    c(-77.6624, "-77.662f");
    c(500.0, "500.00f");
    c(5000.0, "5000.0f");
    c(50000.0, "50000.f");
    c(100000.0, "99999+f");
    c(-100000.0, "-99999+f");
    c(f32::INFINITY, "inff");
    c(f32::NEG_INFINITY, "-inff");
    assert_eq!(fmt::fmt_float(f32::NAN), "NaN");
    c(1.0, "1.0000f");
    c(-1.0, "-1.0000f");
}

// testFmtBool / testFmtBoolValues (test_format.cpp:66-69, 350-353)
#[test]
fn fmt_bool() {
    assert_eq!(fmt::fmt_bool(1), "true");
    assert_eq!(fmt::fmt_bool(0), "false");
}

// testFmtPointer64_null / _nonNull (test_format.cpp:71-79)
#[test]
fn fmt_pointer64() {
    assert_eq!(fmt::fmt_pointer64(0), "nullptr");
    let s = fmt::fmt_pointer64(0x400000);
    assert!(s.starts_with("0x"));
    assert!(s.contains("400000"));
}

// testFmtOffsetMargin_* (test_format.cpp:81-97)
#[test]
fn fmt_offset_margin() {
    assert_eq!(fmt::fmt_offset_margin(0x10, false, 8), "00000010 ");
    assert_eq!(fmt::fmt_offset_margin(0, false, 8), "00000000 ");
    assert_eq!(fmt::fmt_offset_margin(0x10, true, 8), "  \u{00B7} ");
    assert_eq!(
        fmt::fmt_offset_margin(0xFFFF_F800_1234_5678, false, 16),
        "FFFFF80012345678 "
    );
    assert_eq!(fmt::fmt_offset_margin(0x10, false, 16), "0000000000000010 ");
    assert_eq!(fmt::fmt_offset_margin(0x10, false, 4), "0010 ");
}

// testFmtStructHeader (test_format.cpp:99-114)
#[test]
fn fmt_struct_header() {
    let n = Node {
        kind: NodeKind::Struct,
        name: "Test".into(),
        ..Node::default()
    };
    let s = fmt::fmt_struct_header(&n, 0, false, fmt::COL_TYPE, fmt::COL_NAME, false);
    assert!(s.contains("struct") && s.contains("Test") && s.contains('{'));
    let c = fmt::fmt_struct_header(&n, 0, true, fmt::COL_TYPE, fmt::COL_NAME, false);
    assert!(c.contains("struct") && c.contains("Test") && !c.contains('{'));
}

// testFmtStructFooter / testFmtStructFooterSimple (test_format.cpp:116-123, 324-333)
#[test]
fn fmt_struct_footer() {
    let n = Node {
        kind: NodeKind::Struct,
        name: "Test".into(),
        ..Node::default()
    };
    assert!(fmt::fmt_struct_footer(&n, 0, -1).contains("};"));
    let s = fmt::fmt_struct_footer(&n, 0, 0x14);
    assert!(s.contains("};"));
    assert!(!s.contains("sizeof"));
}

// testIndent (test_format.cpp:125-129) — INTENTIONALLY DIVERGES from the C++
// oracle (which used kTreeIndent=2). The tree indent was widened to 3 cols/level
// so nested structs / pointer-to-class expansions read as clear code-like steps;
// `indent(n)` is therefore `n * 3` spaces. Mirrors the unit test in `format.rs`.
#[test]
fn indent() {
    assert_eq!(fmt::indent(0), "");
    assert_eq!(fmt::indent(1), "   ");
    assert_eq!(fmt::indent(3), "         ");
}

// testParseValueInt32 (test_format.cpp:131-139)
#[test]
fn parse_value_int32() {
    let b = fmt::parse_value_kind(NodeKind::Int32, "-42").unwrap();
    assert_eq!(b.len(), 4);
    assert_eq!(i32::from_le_bytes(b.try_into().unwrap()), -42);
}

// testParseValueFloat (test_format.cpp:141-149)
#[test]
fn parse_value_float() {
    let b = fmt::parse_value_kind(NodeKind::Float, "3.14").unwrap();
    let v = f32::from_le_bytes(b.try_into().unwrap());
    assert!((v - 3.14f32).abs() < 0.01);
}

// testParseValueHex32 / testParseValueHex0xPrefix (test_format.cpp:151-196)
#[test]
fn parse_value_hex32() {
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Hex32, "DEADBEEF").unwrap(),
        vec![0xDE, 0xAD, 0xBE, 0xEF]
    );
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Hex32, "0xDEADBEEF").unwrap(),
        vec![0xDE, 0xAD, 0xBE, 0xEF]
    );
    let b = fmt::parse_value_kind(NodeKind::Pointer64, "0x0000000000400000").unwrap();
    assert_eq!(u64::from_le_bytes(b.try_into().unwrap()), 0x400000);
}

// testParseValueBool (test_format.cpp:165-179)
#[test]
fn parse_value_bool() {
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Bool, "true").unwrap(),
        vec![1]
    );
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Bool, "false").unwrap(),
        vec![0]
    );
    assert!(fmt::parse_value_kind(NodeKind::Bool, "banana").is_none());
}

// testParseValueOverflow (test_format.cpp:198-235)
#[test]
fn parse_value_overflow() {
    assert!(fmt::parse_value_kind(NodeKind::UInt8, "300").is_none());
    assert_eq!(
        fmt::parse_value_kind(NodeKind::UInt8, "255").unwrap()[0],
        255
    );
    assert!(fmt::parse_value_kind(NodeKind::Int8, "200").is_none());
    assert!(fmt::parse_value_kind(NodeKind::Int8, "-129").is_none());
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Int8, "-128").unwrap()[0] as i8,
        -128
    );
    assert!(fmt::parse_value_kind(NodeKind::UInt16, "70000").is_none());
    assert!(fmt::parse_value_kind(NodeKind::Hex8, "1FF").is_none());
    assert!(fmt::parse_value_kind(NodeKind::Hex16, "1FFFF").is_none());
}

// testSignedHexRoundTrip (test_format.cpp:237-273)
#[test]
fn signed_hex_round_trip() {
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Int8, "0xFF").unwrap()[0] as i8,
        -1
    );
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Int8, "0x80").unwrap()[0] as i8,
        -128
    );
    let b = fmt::parse_value_kind(NodeKind::Int16, "0xFFFF").unwrap();
    assert_eq!(i16::from_le_bytes(b.try_into().unwrap()), -1);
    let b = fmt::parse_value_kind(NodeKind::Int32, "0xFFFFFFFF").unwrap();
    assert_eq!(i32::from_le_bytes(b.try_into().unwrap()), -1);
    assert!(fmt::parse_value_kind(NodeKind::Int8, "0x1FF").is_none());
    assert!(fmt::parse_value_kind(NodeKind::Int16, "0x1FFFF").is_none());
}

// testReadValueBoundsCheck (test_format.cpp:275-291)
#[test]
fn read_value_bounds_check() {
    let prov = BufferProvider::new(vec![0u8; 16], "t");
    let mut n = Node {
        kind: NodeKind::Vec2,
        name: "v".into(),
        ..Node::default()
    };
    assert!(fmt::read_value(&n, &prov, 0, 0).contains(','));
    n.kind = NodeKind::Vec3;
    assert_eq!(fmt::read_value(&n, &prov, 0, 0).matches(',').count(), 2);
    n.kind = NodeKind::Vec4;
    assert_eq!(fmt::read_value(&n, &prov, 0, 0).matches(',').count(), 3);
}

// testEditableValueBasic (test_format.cpp:293-310)
#[test]
fn editable_value_basic() {
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&3.14f32.to_le_bytes());
    let prov = BufferProvider::new(data, "t");
    let mut n = Node {
        kind: NodeKind::Float,
        name: "f".into(),
        ..Node::default()
    };
    assert!(fmt::editable_value(&n, &prov, 0, 0).contains("3.14"));
    n.kind = NodeKind::Vec2;
    assert!(fmt::editable_value(&n, &prov, 0, 0).contains(','));
}

// testParseValueEmptyString (test_format.cpp:312-322)
#[test]
fn parse_value_empty_string() {
    assert!(fmt::parse_value_kind(NodeKind::UTF8, "")
        .unwrap()
        .is_empty());
    assert!(fmt::parse_value_kind(NodeKind::Int32, "").is_none());
}

// testFmtFloatEdgeCases (test_format.cpp:334-342)
#[test]
fn fmt_float_edge_cases() {
    assert_eq!(fmt::fmt_float(f32::NAN), "NaN");
    assert_eq!(fmt::fmt_float(f32::INFINITY), "inff");
    assert_eq!(fmt::fmt_float(f32::NEG_INFINITY), "-inff");
    assert!(fmt::fmt_float(3.14).contains('f'));
    assert!(fmt::fmt_float(-0.0).starts_with('-'));
}

// testFmtDoubleIntegerValue (test_format.cpp:344-348)
#[test]
fn fmt_double_integer_value() {
    assert!(fmt::fmt_double(42.0).contains('.'));
}

// testValidateValueEmpty (test_format.cpp:355-358)
#[test]
fn validate_value_empty() {
    assert!(fmt::validate_value(NodeKind::Int32, "").is_empty());
}

// testValidateValueHexOverflow (test_format.cpp:360-364)
#[test]
fn validate_value_hex_overflow() {
    assert!(!fmt::validate_value(NodeKind::Int8, "999").is_empty());
}

// testParseValueHex128 / TooShort (test_format.cpp:377-394)
#[test]
fn parse_value_hex128() {
    let b = fmt::parse_value_kind(
        NodeKind::Hex128,
        "00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF",
    )
    .unwrap();
    assert_eq!(b.len(), 16);
    assert_eq!(b[0], 0x00);
    assert_eq!(b[15], 0xFF);
    assert!(fmt::parse_value_kind(NodeKind::Hex128, "00 11 22 33 44 55 66 77").is_none());
}

// testReadValueHex128 (test_format.cpp:396-411)
#[test]
fn read_value_hex128() {
    let mut data = vec![0u8; 16];
    data[0] = 0x41;
    data[15] = 0xFF;
    let prov = BufferProvider::new(data, "t");
    let n = Node {
        kind: NodeKind::Hex128,
        ..Node::default()
    };
    assert!(!fmt::read_value(&n, &prov, 0, 0).is_empty());
    let edit = fmt::editable_value(&n, &prov, 0, 0);
    assert!(edit.contains(' '));
    assert!(edit.len() >= 47);
}

// testFmtFloatVerySmall (test_format.cpp:413-418)
#[test]
fn fmt_float_very_small() {
    let s = fmt::fmt_float(1e-7);
    assert!(s.contains('f'));
    assert!(s.len() <= 9);
}

// testFmtDoubleVeryLarge / NegativeZero / NanInf (test_format.cpp:420-437)
#[test]
fn fmt_double_special() {
    let s = fmt::fmt_double(1e308);
    assert!(s.contains('.') || s.contains('e') || s.contains('E'));
    assert!(!fmt::fmt_double(-0.0).is_empty());
    assert!(!fmt::fmt_double(f64::NAN).is_empty());
    assert!(!fmt::fmt_double(f64::INFINITY).is_empty());
}

// testParseValueUtf8Emoji (test_format.cpp:439-444)
#[test]
fn parse_value_utf8() {
    assert_eq!(
        fmt::parse_value_kind(NodeKind::UTF8, "\"hello\"").unwrap(),
        b"hello".to_vec()
    );
}

// testParseValueHex16SpaceSeparated (test_format.cpp:446-453)
#[test]
fn parse_value_hex16_space_separated() {
    assert_eq!(
        fmt::parse_value_kind(NodeKind::Hex16, "AB CD").unwrap(),
        vec![0xAB, 0xCD]
    );
}

// testValidateValueHex128 (test_format.cpp:455-459)
#[test]
fn validate_value_hex128() {
    assert!(fmt::validate_value(
        NodeKind::Hex128,
        "00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF"
    )
    .is_empty());
}

// testIsValidPrimitivePtrTarget (test_format.cpp:476-484)
#[test]
fn is_valid_primitive_ptr_target_cases() {
    assert!(!is_valid_primitive_ptr_target(NodeKind::Hex8));
    assert!(!is_valid_primitive_ptr_target(NodeKind::Pointer64));
    assert!(!is_valid_primitive_ptr_target(NodeKind::Struct));
    assert!(!is_valid_primitive_ptr_target(NodeKind::FuncPtr64));
    assert!(is_valid_primitive_ptr_target(NodeKind::Int32));
    assert!(is_valid_primitive_ptr_target(NodeKind::Float));
    assert!(is_valid_primitive_ptr_target(NodeKind::Bool));
}
