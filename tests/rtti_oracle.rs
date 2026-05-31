//! Integration translation of the C++ `test_rtti.cpp` (oracle: 18 pass / 1 skip).
//!
//! Exercises the public `reclass::rtti` API exactly as the C++ `TestRtti` suite
//! does, asserting against the same golden values. The single
//! `smokeTestRealBinary` slot is Windows-only and skips upstream — ported as
//! `#[cfg(windows)] #[ignore]`.
//!
//! Run: `cargo test --no-default-features --features symbols --test rtti_oracle`.

#![cfg(feature = "symbols")]

use reclass::provider::{BufferProvider, ModuleEntry, Provider};
use reclass::rtti::{demangle_itanium_name, demangle_rtti_name, walk_rtti, walk_rtti_itanium};

const K_IMAGE_BASE: u64 = 0x10000;

// ── Synthetic MSVC RTTI buffer (test_rtti.cpp:325 layoutSyntheticRtti) ──
fn build_synthetic_msvc_rtti() -> Vec<u8> {
    let mut buf = vec![0u8; 0x10000];
    let wq = |b: &mut [u8], at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let wd = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let wc = |b: &mut [u8], at: usize, s: &str| {
        let by = s.as_bytes();
        b[at..at + by.len()].copy_from_slice(by);
        b[at + by.len()] = 0;
    };

    let vtable = 0x1000usize;
    let (td_foo, td_bar, td_baz) = (0x1100usize, 0x1200usize, 0x1300usize);
    let chd = 0x1400usize;
    let bca = 0x1500usize;
    let (bcd_foo, bcd_bar, bcd_baz) = (0x1600usize, 0x1700usize, 0x1800usize);
    let col = 0x1900usize;

    wq(&mut buf, vtable - 8, K_IMAGE_BASE + col as u64);
    for i in 0..5usize {
        wq(
            &mut buf,
            vtable + i * 8,
            K_IMAGE_BASE + 0x100 + i as u64 * 0x10,
        );
    }
    wq(&mut buf, vtable + 5 * 8, 0);

    let write_td = |b: &mut [u8], rva: usize, name: &str| {
        wq(b, rva, 0xDEAD_BEEF);
        wq(b, rva + 8, 0);
        wc(b, rva + 16, name);
    };
    write_td(&mut buf, td_foo, ".?AVFoo@@");
    write_td(&mut buf, td_bar, ".?AVBar@@");
    write_td(&mut buf, td_baz, ".?AVBaz@@");

    wd(&mut buf, chd, 0);
    wd(&mut buf, chd + 0x04, 0);
    wd(&mut buf, chd + 0x08, 3);
    wd(&mut buf, chd + 0x0C, bca as u32);

    wd(&mut buf, bca, bcd_foo as u32);
    wd(&mut buf, bca + 4, bcd_bar as u32);
    wd(&mut buf, bca + 8, bcd_baz as u32);

    wd(&mut buf, bcd_foo, td_foo as u32);
    wd(&mut buf, bcd_bar, td_bar as u32);
    wd(&mut buf, bcd_baz, td_baz as u32);

    wd(&mut buf, col + 0x00, 1);
    wd(&mut buf, col + 0x04, 0);
    wd(&mut buf, col + 0x08, 0);
    wd(&mut buf, col + 0x0C, td_foo as u32);
    wd(&mut buf, col + 0x10, chd as u32);
    wd(&mut buf, col + 0x14, K_IMAGE_BASE as u32);

    let mut data = vec![0u8; (K_IMAGE_BASE as usize) + buf.len()];
    data[K_IMAGE_BASE as usize..].copy_from_slice(&buf);
    data
}

// ── Itanium fixture (test_rtti.cpp:264 ItaniumFixture + ItProv) ──
struct ItProv {
    inner: BufferProvider,
}
impl Provider for ItProv {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        self.inner.read(addr, buf)
    }
    fn size(&self) -> i32 {
        self.inner.size()
    }
    fn enumerate_modules(&self) -> Vec<ModuleEntry> {
        vec![ModuleEntry {
            name: "synthetic-itanium".to_owned(),
            full_path: "synthetic-itanium".to_owned(),
            base: K_IMAGE_BASE,
            size: 0x10000,
        }]
    }
}

fn build_itanium(mangled: &str) -> Vec<u8> {
    const VT: u64 = 0x1000;
    const TI: u64 = 0x1100;
    const NAME: u64 = 0x1180;
    const TIVT: u64 = 0x1200;
    let mut d = vec![0u8; (K_IMAGE_BASE as usize) + 0x10000];
    let wq = |d: &mut [u8], off: u64, v: u64| {
        let off = off as usize;
        d[off..off + 8].copy_from_slice(&v.to_le_bytes());
    };
    wq(&mut d, K_IMAGE_BASE + VT - 16, 0);
    wq(&mut d, K_IMAGE_BASE + VT - 8, K_IMAGE_BASE + TI);
    for i in 0..5u64 {
        wq(
            &mut d,
            K_IMAGE_BASE + VT + i * 8,
            K_IMAGE_BASE + 0x100 + i * 0x10,
        );
    }
    wq(&mut d, K_IMAGE_BASE + VT + 5 * 8, 0);
    wq(&mut d, K_IMAGE_BASE + TI, K_IMAGE_BASE + TIVT);
    wq(&mut d, K_IMAGE_BASE + TI + 8, K_IMAGE_BASE + NAME);
    let no = (K_IMAGE_BASE + NAME) as usize;
    let nb = mangled.as_bytes();
    d[no..no + nb.len()].copy_from_slice(nb);
    d[no + nb.len()] = 0;
    wq(&mut d, K_IMAGE_BASE + TIVT, 0xFEED_FACE);
    d
}

fn it_prov(mangled: &str) -> ItProv {
    ItProv {
        inner: BufferProvider::new(build_itanium(mangled), "synthetic-itanium"),
    }
}

// ── demangleBasic / demangleNested / demangleMalformed ──
#[test]
fn demangle_basic() {
    assert_eq!(demangle_rtti_name(".?AVFoo@@"), "Foo");
    assert_eq!(demangle_rtti_name(".?AUStruct@@"), "Struct");
}
#[test]
fn demangle_nested() {
    assert_eq!(demangle_rtti_name(".?AVBar@Foo@@"), "Foo::Bar");
    assert_eq!(demangle_rtti_name(".?AVZ@Y@X@@"), "X::Y::Z");
}
#[test]
fn demangle_malformed() {
    assert_eq!(demangle_rtti_name("plain_name"), "plain_name");
    assert_eq!(demangle_rtti_name(""), "");
}

// ── walkSyntheticRtti ──
#[test]
fn walk_synthetic_rtti() {
    let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
    let vtable_va = K_IMAGE_BASE + 0x1000;
    let info = walk_rtti(&prov, vtable_va, 8, 16);

    assert!(info.ok, "{}", info.error);
    assert_eq!(info.vtable_address, vtable_va);
    assert_eq!(info.complete_locator, K_IMAGE_BASE + 0x1900);
    assert_eq!(info.image_base, K_IMAGE_BASE);
    assert_eq!(info.offset, 0);
    assert_eq!(info.raw_name, ".?AVFoo@@");
    assert_eq!(info.demangled_name, "Foo");
    assert_eq!(info.bases.len(), 3);
    assert_eq!(info.bases[0].demangled_name, "Foo");
    assert_eq!(info.bases[1].demangled_name, "Bar");
    assert_eq!(info.bases[2].demangled_name, "Baz");
    assert_eq!(info.vtable.len(), 5);
    for i in 0..5usize {
        assert_eq!(info.vtable[i].slot, i as i32);
        assert_eq!(
            info.vtable[i].address,
            K_IMAGE_BASE + 0x100 + i as u64 * 0x10
        );
    }
}

// ── walkRejectsBadSignature ──
#[test]
fn walk_rejects_bad_signature() {
    let mut data = build_synthetic_msvc_rtti();
    let col = (K_IMAGE_BASE + 0x1900) as usize;
    data[col..col + 4].copy_from_slice(&0xDEADu32.to_le_bytes());
    let prov = BufferProvider::new(data, "bad");
    let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(!info.ok);
    assert!(info.error.contains("signature"));
}

// ── walkRejectsHugeBaseCount ──
#[test]
fn walk_rejects_huge_base_count() {
    let mut data = build_synthetic_msvc_rtti();
    let at = (K_IMAGE_BASE + 0x1400 + 0x08) as usize;
    data[at..at + 4].copy_from_slice(&9999u32.to_le_bytes());
    let prov = BufferProvider::new(data, "bad");
    let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(!info.ok);
    assert!(info.error.contains("unreasonably"));
}

// ── textReportFormat ──
#[test]
fn text_report_format() {
    let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
    let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(info.ok);
    assert!(!info.demangled_name.is_empty());
    assert_ne!(info.vtable_address, 0);
    assert_ne!(info.complete_locator, 0);
}

// ── msvcAbiTagged ──
#[test]
fn msvc_abi_tagged() {
    let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
    let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(info.ok);
    assert_eq!(info.abi, "MSVC");
}

// ── demangleItaniumSimple / Nested / StdShorthand / Passthrough ──
#[test]
fn demangle_itanium_simple() {
    assert_eq!(demangle_itanium_name("3Foo"), "Foo");
}
#[test]
fn demangle_itanium_nested() {
    assert_eq!(demangle_itanium_name("N3Bar3FooE"), "Bar::Foo");
}
#[test]
fn demangle_itanium_std_shorthand() {
    assert!(demangle_itanium_name("St9type_info").ends_with("type_info"));
}
#[test]
fn demangle_itanium_passthrough() {
    assert_eq!(demangle_itanium_name("plain_text"), "plain_text");
}

// ── walkSyntheticItanium ──
#[test]
fn walk_synthetic_itanium() {
    let prov = it_prov("3Foo");
    let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(info.ok, "{}", info.error);
    assert_eq!(info.abi, "Itanium");
    assert_eq!(info.raw_name, "3Foo");
    assert_eq!(info.demangled_name, "Foo");
    assert_eq!(info.vtable.len(), 5);
}

// ── walkSyntheticItaniumNested ──
#[test]
fn walk_synthetic_itanium_nested() {
    let prov = it_prov("N3Bar3FooE");
    let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(info.ok);
    assert_eq!(info.demangled_name, "Bar::Foo");
}

// ── rejectsImplausibleOffsetToTop ──
#[test]
fn rejects_implausible_offset_to_top() {
    let mut data = build_itanium("3Foo");
    let at = (K_IMAGE_BASE + 0x1000 - 16) as usize;
    data[at..at + 8].copy_from_slice(&0x7FFF_FFFF_FFFF_FFFFi64.to_le_bytes());
    let prov = ItProv {
        inner: BufferProvider::new(data, "synthetic-itanium"),
    };
    let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(!info.ok);
    assert!(info.error.contains("offset_to_top"));
}

// ── rejectsNonItaniumNameString ──
#[test]
fn rejects_non_itanium_name() {
    let prov = it_prov("not_a_mangle");
    let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
    assert!(!info.ok);
    assert!(info.error.contains("mangle"));
}

// ── smokeTestRealBinary (test_rtti.cpp:215) — Windows-only, skips upstream. ──
#[cfg(windows)]
#[test]
#[ignore = "real-binary smoke is Windows-only (Itanium ABI elsewhere)"]
fn smoke_real_binary() {
    let path = "C:/Windows/System32/combase.dll";
    let bytes = match std::fs::read(path) {
        Ok(b) if b.len() >= 0x1000 => b,
        _ => return, // not present / truncated -> skip
    };
    let prov = BufferProvider::new(bytes, "combase.dll");
    let size = prov.size() as u64;
    for off in [0x1000u64, 0x10000, 0x100000] {
        if off + 0x100 >= size {
            continue;
        }
        let info = walk_rtti(&prov, off, 8, 4);
        // Either ok or a clean error — neither should panic.
        if !info.ok {
            assert!(!info.error.is_empty());
        }
    }
}
