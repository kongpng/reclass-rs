//! Integration-level oracle test for the address parser.
//!
//! Translates the public-API-facing cases of `tests/test_addressparser.cpp`
//! (golden log `_oracle/logs/test_addressparser.txt`, 53 cases, all PASS).
//! These exercise `reclass::addr::AddressParser` exactly as the C++ test
//! harness does, asserting output-exact values and the error-substring
//! contract callers rely on.

use reclass::addr::{AddressParser, AddressParserCallbacks};

fn eval(s: &str) -> reclass::addr::AddressParseResult {
    AddressParser::evaluate(s, 8, None)
}

#[test]
fn hex_literals() {
    assert_eq!(eval("AB").value, 0xAB);
    assert_eq!(eval("0x1F4").value, 0x1F4);
    assert_eq!(eval("0").value, 0);
    assert_eq!(eval("7FF66CCE0000").value, 0x7FF66CCE0000);
    assert_eq!(eval("140000000").value, 0x140000000);
    assert_eq!(eval("DEAD").value, 0xDEAD);
}

#[test]
fn arithmetic_and_precedence() {
    assert_eq!(eval("0x100 + 0x200").value, 0x300);
    assert_eq!(eval("0x300 - 0x100").value, 0x200);
    assert_eq!(eval("0x10 * 4").value, 0x40);
    assert_eq!(eval("0x100 / 2").value, 0x80);
    assert_eq!(eval("0x10 + 2 * 3").value, 0x16);
    assert_eq!(eval("(0x10 + 2) * 3").value, 0x36);
    assert_eq!(eval("0x100 + 0x200 + 0x300").value, 0x600);
}

#[test]
fn unary_ops() {
    assert_eq!(eval("-0x10 + 0x20").value, 0x10);
    assert_eq!(eval("~0").value, 0xFFFFFFFFFFFFFFFF);
    assert_eq!(eval("~0xFFF").value, 0xFFFFFFFFFFFFF000);
}

#[test]
fn bitwise_and_shift() {
    assert_eq!(eval("0xFF & 0x0F").value, 0x0F);
    assert_eq!(eval("0xA0 | 0x0B").value, 0xAB);
    assert_eq!(eval("0xA ^ 0x5").value, 0xF);
    assert_eq!(eval("1 << 4").value, 0x10);
    assert_eq!(eval("0xFF00 >> 8").value, 0xFF);
    assert_eq!(eval("1 + 2 << 3").value, 0x18);
    assert_eq!(eval("0xFF | 0x100 & 0xF00").value, 0x1FF);
    assert_eq!(eval("0xF0 | 0x0F ^ 0xFF & 0x0F").value, 0xF0);
}

#[test]
fn module_resolution() {
    let cbs = AddressParserCallbacks {
        resolve_module: Some(Box::new(|name: &str| {
            let ok = name == "Program.exe";
            (if ok { 0x140000000 } else { 0 }, ok)
        })),
        ..Default::default()
    };
    let r = AddressParser::evaluate("<Program.exe> + 0x123", 8, Some(&cbs));
    assert!(r.ok);
    assert_eq!(r.value, 0x140000123);

    let cbs = AddressParserCallbacks {
        resolve_module: Some(Box::new(|_: &str| (0, false))),
        ..Default::default()
    };
    let r = AddressParser::evaluate("<NoSuch.dll>", 8, Some(&cbs));
    assert!(!r.ok);
    assert!(r.error.contains("not found"));
}

#[test]
fn dereference() {
    let cbs = AddressParserCallbacks {
        read_pointer: Some(Box::new(|addr: u64| {
            let ok = addr == 0x1000;
            (if ok { 0xDEADBEEF } else { 0 }, ok)
        })),
        ..Default::default()
    };
    let r = AddressParser::evaluate("[0x1000]", 8, Some(&cbs));
    assert!(r.ok);
    assert_eq!(r.value, 0xDEADBEEF);

    // Nested
    let cbs = AddressParserCallbacks {
        resolve_module: Some(Box::new(|name: &str| {
            let ok = name == "mod";
            (if ok { 0x400000 } else { 0 }, ok)
        })),
        read_pointer: Some(Box::new(|addr: u64| {
            let v = match addr {
                0x400100 => 0x500000,
                0x900000 => 0xABCDEF,
                _ => 0,
            };
            (v, true)
        })),
        ..Default::default()
    };
    let r = AddressParser::evaluate("[<mod> + [<mod> + 0x100]]", 8, Some(&cbs));
    assert!(r.ok);
    assert_eq!(r.value, 0xABCDEF);

    // Read failure
    let cbs = AddressParserCallbacks {
        read_pointer: Some(Box::new(|_: u64| (0, false))),
        ..Default::default()
    };
    let r = AddressParser::evaluate("[0x1000]", 8, Some(&cbs));
    assert!(!r.ok);
    assert!(r.error.contains("failed to read"));
}

#[test]
fn complex_expr() {
    let cbs = AddressParserCallbacks {
        resolve_module: Some(Box::new(|name: &str| {
            let ok = name == "Program.exe";
            (if ok { 0x140000000 } else { 0 }, ok)
        })),
        read_pointer: Some(Box::new(|addr: u64| {
            if addr == 0x1400000DE {
                (0x500000, true)
            } else {
                (0, true)
            }
        })),
        ..Default::default()
    };
    let r = AddressParser::evaluate("[<Program.exe> + 0xDE] - AB", 8, Some(&cbs));
    assert!(r.ok);
    assert_eq!(r.value, 0x4FFF55);
}

#[test]
fn identifiers() {
    let mk = || AddressParserCallbacks {
        resolve_identifier: Some(Box::new(|name: &str| match name {
            "base" => (0x140000000, true),
            "e_lfanew" => (0xE8, true),
            _ => (0, false),
        })),
        ..Default::default()
    };
    let cbs = mk();
    assert_eq!(
        AddressParser::evaluate("base + e_lfanew", 8, Some(&cbs)).value,
        0x1400000E8
    );
    let cbs = mk();
    assert_eq!(
        AddressParser::evaluate("(base + e_lfanew) & ~0xFFF", 8, Some(&cbs)).value,
        0x140000000
    );

    // Unknown identifier
    let cbs = AddressParserCallbacks {
        resolve_identifier: Some(Box::new(|_: &str| (0, false))),
        ..Default::default()
    };
    let r = AddressParser::evaluate("unknown_var", 8, Some(&cbs));
    assert!(!r.ok);
    assert!(r.error.contains("unknown identifier"));
}

#[test]
fn bare_module_identifiers() {
    let cbs = AddressParserCallbacks {
        resolve_identifier: Some(Box::new(|name: &str| {
            let ok = name == "client.dll";
            (if ok { 0x7FF600000000 } else { 0 }, ok)
        })),
        ..Default::default()
    };
    assert_eq!(
        AddressParser::evaluate("client.dll + 0xFF", 8, Some(&cbs)).value,
        0x7FF6000000FF
    );

    let cbs = AddressParserCallbacks {
        resolve_identifier: Some(Box::new(|name: &str| {
            let ok = name == "cs2.exe";
            (if ok { 0x140000000 } else { 0 }, ok)
        })),
        ..Default::default()
    };
    assert_eq!(
        AddressParser::evaluate("cs2.exe + 0xDE", 8, Some(&cbs)).value,
        0x1400000DE
    );
}

#[test]
fn errors() {
    assert!(!eval("").ok);
    assert!(eval("[0x1000").error.contains("']'"));
    assert!(eval("<Program.exe").error.contains("'>'"));
    assert!(eval("0x100 / 0").error.contains("division by zero"));
    assert!(eval("0x100 xyz").error.contains("unexpected"));
    assert!(!eval("0x100 +").ok);
}

#[test]
fn validation() {
    assert_eq!(AddressParser::validate("0x100 + 0x200"), "");
    assert_eq!(AddressParser::validate("<Prog.exe> + [0x100]"), "");
    assert_eq!(AddressParser::validate("base + e_lfanew"), "");
    assert_eq!(AddressParser::validate("0xFF & 0x0F"), "");
    assert_eq!(AddressParser::validate("1 << 4"), "");
    assert_eq!(AddressParser::validate("~0xFFF"), "");
    assert!(!AddressParser::validate("").is_empty());
    assert!(!AddressParser::validate("[0x100").is_empty());
    assert!(!AddressParser::validate("0x100 xyz").is_empty());
}

#[test]
fn cleaning_and_whitespace() {
    assert_eq!(eval("7ff6`6cce0000").value, 0x7FF66CCE0000);
    let r = eval("  0x100  +  0x200  ");
    assert!(r.ok);
    assert_eq!(r.value, 0x300);
}
