//! RTTI walkers — MSVC (`walkRtti`) and Itanium (`walkRttiItanium`) plus
//! `findOwningModule` and the result structs.
//!
//! Port of `src/rtti.cpp` (`findOwningModule` @ rtti.cpp:20, the walker helpers
//! `rttiResolve`/`readU32At`/`readCString` @ rtti.cpp:78-104, `walkRtti`
//! @ rtti.cpp:108, `walkRttiItanium` @ rtti.cpp:354) and the structs in
//! `src/rtti.h`.
//!
//! The walkers do NOT use `Result`: they mirror C++ exactly, returning an
//! [`RttiInfo`] with `ok=false` and a load-bearing `error` string on failure.

use crate::provider::Provider;
use crate::rtti::demangle::{demangle_itanium_name, demangle_rtti_name};
use crate::rtti::symbol_store::SymbolStore;

/// `struct RttiBaseClass` (`rtti.h:30-34`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RttiBaseClass {
    /// ".?AVFoo@@".
    pub raw_name: String,
    /// "Foo".
    pub demangled_name: String,
    /// Set to the loop index `i`, mirroring C++ (`b.depth = (int)i`).
    pub depth: i32,
}

/// `struct RttiVirtualMethod` (`rtti.h:36-40`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RttiVirtualMethod {
    /// index in vtable.
    pub slot: i32,
    pub address: u64,
    /// resolved via [`SymbolStore`] (empty when no PDB loaded).
    pub symbol: String,
}

/// `struct RttiInfo` (`rtti.h:42-58`).
#[derive(Clone, Debug, Default)]
pub struct RttiInfo {
    pub ok: bool,
    /// human-readable; empty when `ok`.
    pub error: String,
    /// "MSVC" / "Itanium"; empty when `!ok`.
    pub abi: String,
    /// echoed back even on failure.
    pub vtable_address: u64,
    pub image_base: u64,
    pub module_name: String,
    /// MSVC: VA of COL; Itanium: VA of type_info.
    pub complete_locator: u64,
    /// MSVC: COL.offset; Itanium: offset_to_top (truncated to i32).
    pub offset: i32,
    pub raw_name: String,
    pub demangled_name: String,
    pub bases: Vec<RttiBaseClass>,
    pub vtable: Vec<RttiVirtualMethod>,
}

/// `struct OwningModule` (`rtti.h`).
#[derive(Clone, Debug, Default)]
pub struct OwningModule {
    pub name: String,
    pub full_path: String,
    pub base: u64,
    pub size: u64,
    pub valid: bool,
}

// ── Provider read-adapter (PORTING spec §1.0) ──
// C++ `Provider::read(addr,buf,len) -> bool`. The Rust trait already returns a
// bool, so `pread` is a thin pass-through. All byte fields are little-endian
// (RTTI bytes come from x86/x64 images); explicit `from_le_bytes` keeps it
// correct on any host endianness.

/// Mirror of C++ `Provider::read(addr,buf,len) -> bool`: true iff the FULL slice
/// was read.
#[inline]
fn pread(p: &dyn Provider, addr: u64, buf: &mut [u8]) -> bool {
    p.read(addr, buf)
}

/// Mirror of `readU32At` (`rtti.cpp:87`): returns 0 on failure but sets ok=false.
#[inline]
fn read_u32(p: &dyn Provider, addr: u64) -> (u32, bool) {
    let mut b = [0u8; 4];
    let ok = pread(p, addr, &mut b);
    (u32::from_le_bytes(b), ok)
}

#[inline]
fn read_u64(p: &dyn Provider, addr: u64) -> (u64, bool) {
    let mut b = [0u8; 8];
    let ok = pread(p, addr, &mut b);
    (u64::from_le_bytes(b), ok)
}

#[inline]
fn read_i64(p: &dyn Provider, addr: u64) -> (i64, bool) {
    let mut b = [0u8; 8];
    let ok = pread(p, addr, &mut b);
    (i64::from_le_bytes(b), ok)
}

#[inline]
fn read_i32(p: &dyn Provider, addr: u64) -> (i32, bool) {
    let mut b = [0u8; 4];
    let ok = pread(p, addr, &mut b);
    (i32::from_le_bytes(b), ok)
}

/// `rttiResolve(field, imageBase, ptrSize)` (`rtti.cpp:81`) — 32-bit RVA on x64,
/// absolute on x86.
#[inline]
fn rtti_resolve(field: u32, image_base: u64, ptr_size: i32) -> u64 {
    if ptr_size == 8 {
        image_base.wrapping_add(field as u64)
    } else {
        field as u64
    }
}

/// `readCString(p, addr, maxLen=512)` (`rtti.cpp:94`) — NUL-terminated, UTF-8
/// (the MSVC walker path). Read failure or NUL stops.
fn read_cstring(p: &dyn Provider, addr: u64, max_len: usize) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(64);
    for i in 0..max_len {
        let mut b = [0u8; 1];
        if !pread(p, addr.wrapping_add(i as u64), &mut b) {
            break;
        }
        if b[0] == 0 {
            break;
        }
        out.push(b[0]);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `findOwningModule(prov, addr)` (`rtti.cpp:20`). Linear scan over
/// `enumerate_modules()`; returns the first module containing `addr`, else an
/// invalid [`OwningModule`].
pub fn find_owning_module(prov: &dyn Provider, addr: u64) -> OwningModule {
    let mods = prov.enumerate_modules();
    for m in &mods {
        // C++ wraps in uint64_t; use wrapping_add to match on a pathological size.
        if addr >= m.base && addr < m.base.wrapping_add(m.size) {
            return OwningModule {
                name: m.name.clone(),
                full_path: m.full_path.clone(),
                base: m.base,
                size: m.size,
                valid: true,
            };
        }
    }
    OwningModule::default()
}

/// `walkRtti(prov, vtableAddr, pointerSize, maxVtableSlots)` (`rtti.cpp:108`).
/// MSVC RTTI walker.
pub fn walk_rtti(
    prov: &dyn Provider,
    vtable_addr: u64,
    pointer_size: i32,
    max_vtable_slots: i32,
) -> RttiInfo {
    let mut info = RttiInfo {
        vtable_address: vtable_addr,
        ..Default::default()
    };

    if pointer_size != 4 && pointer_size != 8 {
        info.error = "invalid pointer size".to_owned();
        return info;
    }

    // 1. COL pointer at vtable[-ptrSize] (absolute VA on both x86 & x64).
    let meta_ptr_addr = vtable_addr.wrapping_sub(pointer_size as u64);
    let (col_addr, ok) = if pointer_size == 8 {
        read_u64(prov, meta_ptr_addr)
    } else {
        let (v, o) = read_u32(prov, meta_ptr_addr);
        (v as u64, o)
    };
    if !ok || col_addr == 0 {
        info.error = "could not read meta pointer at vtable[-1]".to_owned();
        return info;
    }
    info.complete_locator = col_addr;

    // 2. image base — owning module, else (x64) the COL.pSelf @ +0x14 fallback.
    let owner = find_owning_module(prov, col_addr);
    let mut image_base: u64 = 0;
    if owner.valid {
        info.module_name = owner.name.clone();
        image_base = owner.base;
    } else if pointer_size == 8 {
        let (ib, ib_ok) = read_u32(prov, col_addr.wrapping_add(0x14));
        if ib_ok && ib != 0 {
            image_base = ib as u64;
        }
    }
    info.image_base = image_base;

    // 3. COL signature @ +0x00 (must be 0 or 1).
    let (sig, sig_ok) = read_u32(prov, col_addr.wrapping_add(0x00));
    if !sig_ok {
        info.error = "could not read COL signature".to_owned();
        return info;
    }
    if sig != 0 && sig != 1 {
        info.error = format!("COL signature 0x{sig:x} not 0/1 — not MSVC RTTI");
        return info;
    }

    // 4. offset @ +0x04.
    let (off, _off_ok) = read_u32(prov, col_addr.wrapping_add(0x04));
    info.offset = off as i32;

    // 5. pTypeDescriptor @ +0x0C, pClassHierarchy @ +0x10.
    let (td_field, td_ok) = read_u32(prov, col_addr.wrapping_add(0x0C));
    let (chd_field, chd_ok) = read_u32(prov, col_addr.wrapping_add(0x10));
    if !td_ok || !chd_ok {
        info.error = "could not read COL TypeDescriptor / CHD fields".to_owned();
        return info;
    }
    let td_addr = rtti_resolve(td_field, image_base, pointer_size);
    let chd_addr = rtti_resolve(chd_field, image_base, pointer_size);

    // 6. TypeDescriptor name @ td + 2*ptrSize.
    let name_off = 2 * pointer_size as u64;
    info.raw_name = read_cstring(prov, td_addr.wrapping_add(name_off), 512);
    info.demangled_name = demangle_rtti_name(&info.raw_name);
    if info.raw_name.is_empty() {
        info.error = "type descriptor name empty".to_owned();
        return info;
    }

    // 7. CHD: numBaseClasses @ +0x08, pBaseClassArray @ +0x0C.
    let (num_bases, nb_ok) = read_u32(prov, chd_addr.wrapping_add(0x08));
    let (bca_field, bca_ok) = read_u32(prov, chd_addr.wrapping_add(0x0C));
    if !nb_ok || !bca_ok {
        info.error = "could not read CHD".to_owned();
        return info;
    }
    if num_bases > 256 {
        info.error = format!("CHD.numBaseClasses unreasonably large ({num_bases}) — not RTTI");
        return info;
    }
    let bca_addr = rtti_resolve(bca_field, image_base, pointer_size);
    // 32-bit RVA on x64; ptr on x86.
    let entry_size: u64 = if pointer_size == 8 {
        4
    } else {
        pointer_size as u64
    };

    // 8. base-class array.
    for i in 0..num_bases {
        let (bcd_field, e_ok) = read_u32(prov, bca_addr.wrapping_add(i as u64 * entry_size));
        if !e_ok {
            break;
        }
        let bcd_addr = rtti_resolve(bcd_field, image_base, pointer_size);
        let (bcd_td, tdf_ok) = read_u32(prov, bcd_addr.wrapping_add(0x00));
        if !tdf_ok {
            continue;
        }
        let bcd_td_addr = rtti_resolve(bcd_td, image_base, pointer_size);
        let raw_base = read_cstring(prov, bcd_td_addr.wrapping_add(name_off), 512);
        info.bases.push(RttiBaseClass {
            demangled_name: demangle_rtti_name(&raw_base),
            raw_name: raw_base,
            depth: i as i32,
        });
    }

    // 9. vtable enumeration.
    for slot in 0..max_vtable_slots {
        let entry_addr = vtable_addr.wrapping_add(slot as u64 * pointer_size as u64);
        let (target, tok) = if pointer_size == 8 {
            read_u64(prov, entry_addr)
        } else {
            let (v, o) = read_u32(prov, entry_addr);
            (v as u64, o)
        };
        if !tok {
            break;
        }
        if target == 0 {
            break;
        }
        let in_some_module = if !owner.valid {
            true // synthetic: trust the input
        } else {
            find_owning_module(prov, target).valid
        };
        if !in_some_module {
            break;
        }
        let symbol = SymbolStore::global()
            .lock()
            .unwrap()
            .get_symbol_for_address(target, Some(prov));
        info.vtable.push(RttiVirtualMethod {
            slot,
            address: target,
            symbol,
        });
    }

    info.ok = true;
    info.abi = "MSVC".to_owned();
    info
}

/// `walkRttiItanium(prov, vtableAddr, pointerSize, maxVtableSlots)`
/// (`rtti.cpp:354`). Unlike MSVC, requires real modules — every dereferenced
/// pointer must land inside an enumerated module (no synthetic image-base
/// fallback).
pub fn walk_rtti_itanium(
    prov: &dyn Provider,
    vtable_addr: u64,
    pointer_size: i32,
    max_vtable_slots: i32,
) -> RttiInfo {
    let mut info = RttiInfo {
        vtable_address: vtable_addr,
        ..Default::default()
    };

    if pointer_size != 4 && pointer_size != 8 {
        info.error = "invalid pointer size".to_owned();
        return info;
    }

    // 1. type_info* @ vtable[-ptrSize].
    let ti_ptr_addr = vtable_addr.wrapping_sub(pointer_size as u64);
    let (ti_addr, ok) = if pointer_size == 8 {
        read_u64(prov, ti_ptr_addr)
    } else {
        let (v, o) = read_u32(prov, ti_ptr_addr);
        (v as u64, o)
    };
    if !ok || ti_addr == 0 {
        info.error = "could not read type_info pointer at vtable[-1]".to_owned();
        return info;
    }

    // 2. type_info must be in a module.
    let ti_owner = find_owning_module(prov, ti_addr);
    if !ti_owner.valid {
        info.error = "type_info pointer outside any module".to_owned();
        return info;
    }
    info.image_base = ti_owner.base;
    info.module_name = ti_owner.name.clone();
    info.complete_locator = ti_addr;

    // 3. offset_to_top @ vtable[-2*ptrSize] (signed; default 0 on read fail).
    let addr = vtable_addr.wrapping_sub(pointer_size as u64 * 2);
    let offset_to_top: i64 = if pointer_size == 8 {
        let (v, o) = read_i64(prov, addr);
        if o {
            v
        } else {
            0
        }
    } else {
        let (v, o) = read_i32(prov, addr);
        if o {
            v as i64
        } else {
            0
        }
    };
    if offset_to_top > 0x1000000 || offset_to_top < -0x1000000 {
        info.error = "offset_to_top implausible — not Itanium RTTI".to_owned();
        return info;
    }
    info.offset = offset_to_top as i32;

    // 4. type_info[0] = abi type_info vtable ptr — non-zero & in a module.
    let (ti_vtable, ok) = if pointer_size == 8 {
        read_u64(prov, ti_addr)
    } else {
        let (v, o) = read_u32(prov, ti_addr);
        (v as u64, o)
    };
    if !ok {
        info.error = "could not read type_info vtable ptr".to_owned();
        return info;
    }
    if ti_vtable == 0 || !find_owning_module(prov, ti_vtable).valid {
        info.error = "type_info vtable not in any module".to_owned();
        return info;
    }

    // 5. type_info[ptrSize] = char* __name.
    let name_addr = ti_addr.wrapping_add(pointer_size as u64);
    let (name_ptr, ok) = if pointer_size == 8 {
        read_u64(prov, name_addr)
    } else {
        let (v, o) = read_u32(prov, name_addr);
        (v as u64, o)
    };
    if !ok {
        info.error = "could not read __name pointer".to_owned();
        return info;
    }
    if name_ptr == 0 || !find_owning_module(prov, name_ptr).valid {
        info.error = "__name pointer not in any module".to_owned();
        return info;
    }

    // 6. read mangled name (max 256). Latin-1 + printable filter — NOT
    //    read_cstring: a non-printable byte clears ALL collected bytes & stops.
    let mut name_bytes: Vec<u8> = Vec::with_capacity(64);
    for i in 0..256u64 {
        let mut b = [0u8; 1];
        if !pread(prov, name_ptr.wrapping_add(i), &mut b) {
            break;
        }
        if b[0] == 0 {
            break;
        }
        if b[0] < 0x20 || b[0] > 0x7E {
            name_bytes.clear();
            break;
        }
        name_bytes.push(b[0]);
    }
    if name_bytes.len() < 2 {
        info.error = "__name string empty or non-printable".to_owned();
        return info;
    }

    // 7. vague-linkage '*' prefix.
    let validate_off = if name_bytes[0] == b'*' { 1 } else { 0 };
    if validate_off >= name_bytes.len() {
        info.error = "__name is just a vague-linkage marker".to_owned();
        return info;
    }

    // 8. first real char must be a mangle marker.
    let c0 = name_bytes[validate_off];
    if !(c0.is_ascii_digit() || c0 == b'N' || c0 == b'S' || c0 == b'P' || c0 == b'K' || c0 == b'R')
    {
        info.error = "__name doesn't start with Itanium mangle marker".to_owned();
        return info;
    }

    // 9. store. raw_name includes the '*' prefix (shows literal memory); demangle
    //    the prefix-stripped form.
    info.raw_name = latin1_to_string(&name_bytes);
    info.demangled_name = demangle_itanium_name(&latin1_to_string(&name_bytes[validate_off..]));

    // 10. vtable enumeration — ALWAYS real-module check (no synthetic trust).
    for slot in 0..max_vtable_slots {
        let entry_addr = vtable_addr.wrapping_add(slot as u64 * pointer_size as u64);
        let (target, tok) = if pointer_size == 8 {
            read_u64(prov, entry_addr)
        } else {
            let (v, o) = read_u32(prov, entry_addr);
            (v as u64, o)
        };
        if !tok {
            break;
        }
        if target == 0 {
            break;
        }
        if !find_owning_module(prov, target).valid {
            break;
        }
        let symbol = SymbolStore::global()
            .lock()
            .unwrap()
            .get_symbol_for_address(target, Some(prov));
        info.vtable.push(RttiVirtualMethod {
            slot,
            address: target,
            symbol,
        });
    }

    info.ok = true;
    info.abi = "Itanium".to_owned();
    info
}

/// `QString::fromLatin1` — each byte -> U+00xx (`rtti.cpp:493`).
fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{BufferProvider, ModuleEntry};

    const K_IMAGE_BASE: u64 = 0x10000;

    // ── Synthetic MSVC RTTI buffer (test_rtti.cpp:325 layoutSyntheticRtti) ──
    fn build_synthetic_msvc_rtti() -> Vec<u8> {
        let mut buf = vec![0u8; 0x10000];
        let write_u64 = |b: &mut [u8], at: usize, v: u64| {
            b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        };
        let write_u32 = |b: &mut [u8], at: usize, v: u32| {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        };
        let write_cstr = |b: &mut [u8], at: usize, s: &str| {
            let bytes = s.as_bytes();
            b[at..at + bytes.len()].copy_from_slice(bytes);
            b[at + bytes.len()] = 0;
        };

        let vtable_rva = 0x1000usize;
        let td_foo = 0x1100usize;
        let td_bar = 0x1200usize;
        let td_baz = 0x1300usize;
        let chd = 0x1400usize;
        let bca = 0x1500usize;
        let bcd_foo = 0x1600usize;
        let bcd_bar = 0x1700usize;
        let bcd_baz = 0x1800usize;
        let col = 0x1900usize;

        // vtable[-1] = COL VA; 5 methods; null terminator.
        write_u64(&mut buf, vtable_rva - 8, K_IMAGE_BASE + col as u64);
        for i in 0..5usize {
            write_u64(
                &mut buf,
                vtable_rva + i * 8,
                K_IMAGE_BASE + 0x100 + i as u64 * 0x10,
            );
        }
        write_u64(&mut buf, vtable_rva + 5 * 8, 0);

        // TypeDescriptors: name at +0x10.
        let write_td = |buf: &mut [u8], rva: usize, name: &str| {
            write_u64(buf, rva, 0xDEAD_BEEF);
            write_u64(buf, rva + 8, 0);
            write_cstr(buf, rva + 16, name);
        };
        write_td(&mut buf, td_foo, ".?AVFoo@@");
        write_td(&mut buf, td_bar, ".?AVBar@@");
        write_td(&mut buf, td_baz, ".?AVBaz@@");

        // CHD: 3 bases, pBaseClassArray RVA.
        write_u32(&mut buf, chd, 0);
        write_u32(&mut buf, chd + 0x04, 0);
        write_u32(&mut buf, chd + 0x08, 3);
        write_u32(&mut buf, chd + 0x0C, bca as u32);

        // BCA: 3 BCD RVAs.
        write_u32(&mut buf, bca, bcd_foo as u32);
        write_u32(&mut buf, bca + 4, bcd_bar as u32);
        write_u32(&mut buf, bca + 8, bcd_baz as u32);

        // BCDs: only +0x00 (pTypeDescriptor RVA) matters.
        write_u32(&mut buf, bcd_foo, td_foo as u32);
        write_u32(&mut buf, bcd_bar, td_bar as u32);
        write_u32(&mut buf, bcd_baz, td_baz as u32);

        // COL.
        write_u32(&mut buf, col + 0x00, 1);
        write_u32(&mut buf, col + 0x04, 0);
        write_u32(&mut buf, col + 0x08, 0);
        write_u32(&mut buf, col + 0x0C, td_foo as u32);
        write_u32(&mut buf, col + 0x10, chd as u32);
        write_u32(&mut buf, col + 0x14, K_IMAGE_BASE as u32);

        // Full address space: zeros prefix + RTTI at K_IMAGE_BASE.
        let mut data = vec![0u8; (K_IMAGE_BASE as usize) + buf.len()];
        data[K_IMAGE_BASE as usize..].copy_from_slice(&buf);
        data
    }

    // ── Synthetic Itanium RTTI buffer (test_rtti.cpp:264 ItaniumFixture) ──
    fn build_synthetic_itanium_rtti(mangled: &str) -> Vec<u8> {
        const VT: u64 = 0x1000;
        const TI: u64 = 0x1100;
        const NAME: u64 = 0x1180;
        const TIVT: u64 = 0x1200;
        let mut data = vec![0u8; (K_IMAGE_BASE as usize) + 0x10000];
        let wq = |d: &mut [u8], off: u64, v: u64| {
            let off = off as usize;
            d[off..off + 8].copy_from_slice(&v.to_le_bytes());
        };
        // offset_to_top = 0 at vtable[-16].
        wq(&mut data, K_IMAGE_BASE + VT - 16, 0);
        // type_info VA at vtable[-8].
        wq(&mut data, K_IMAGE_BASE + VT - 8, K_IMAGE_BASE + TI);
        // 5 method ptrs + null terminator.
        for i in 0..5u64 {
            wq(
                &mut data,
                K_IMAGE_BASE + VT + i * 8,
                K_IMAGE_BASE + 0x100 + i * 0x10,
            );
        }
        wq(&mut data, K_IMAGE_BASE + VT + 5 * 8, 0);
        // type_info: vtable_ptr at +0, name_ptr at +8.
        wq(&mut data, K_IMAGE_BASE + TI, K_IMAGE_BASE + TIVT);
        wq(&mut data, K_IMAGE_BASE + TI + 8, K_IMAGE_BASE + NAME);
        // mangled name string.
        let name_off = (K_IMAGE_BASE + NAME) as usize;
        let nb = mangled.as_bytes();
        data[name_off..name_off + nb.len()].copy_from_slice(nb);
        data[name_off + nb.len()] = 0;
        // type_info's vtable head (readable).
        wq(&mut data, K_IMAGE_BASE + TIVT, 0xFEED_FACE);
        data
    }

    /// `ItProv` (test_rtti.cpp:252) — BufferProvider that reports one module.
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

    fn it_prov(mangled: &str) -> ItProv {
        ItProv {
            inner: BufferProvider::new(build_synthetic_itanium_rtti(mangled), "synthetic-itanium"),
        }
    }

    // ── walkSyntheticRtti (test_rtti.cpp:73) ──
    #[test]
    fn walk_synthetic_msvc_rtti() {
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

    // ── walkRejectsBadSignature (test_rtti.cpp:103) ──
    #[test]
    fn walk_rejects_bad_signature() {
        let mut data = build_synthetic_msvc_rtti();
        let col = (K_IMAGE_BASE + 0x1900) as usize;
        data[col..col + 4].copy_from_slice(&0xDEADu32.to_le_bytes());
        let prov = BufferProvider::new(data, "bad");
        let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(!info.ok);
        assert!(info.error.contains("signature"), "{}", info.error);
    }

    // ── walkRejectsHugeBaseCount (test_rtti.cpp:116) ──
    #[test]
    fn walk_rejects_huge_base_count() {
        let mut data = build_synthetic_msvc_rtti();
        let at = (K_IMAGE_BASE + 0x1400 + 0x08) as usize;
        data[at..at + 4].copy_from_slice(&9999u32.to_le_bytes());
        let prov = BufferProvider::new(data, "bad");
        let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(!info.ok);
        assert!(info.error.contains("unreasonably"), "{}", info.error);
    }

    // ── textReportFormat (test_rtti.cpp:129) ──
    #[test]
    fn walk_text_report_fields() {
        let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
        let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(info.ok);
        assert!(!info.demangled_name.is_empty());
        assert_ne!(info.vtable_address, 0);
        assert_ne!(info.complete_locator, 0);
    }

    // ── msvcAbiTagged (test_rtti.cpp:149) ──
    #[test]
    fn walk_msvc_abi_tagged() {
        let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
        let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(info.ok);
        assert_eq!(info.abi, "MSVC");
    }

    // ── walkSyntheticItanium (test_rtti.cpp:178) ──
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

    // ── walkSyntheticItaniumNested (test_rtti.cpp:188) ──
    #[test]
    fn walk_synthetic_itanium_nested() {
        let prov = it_prov("N3Bar3FooE");
        let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(info.ok);
        assert_eq!(info.demangled_name, "Bar::Foo");
    }

    // ── rejectsImplausibleOffsetToTop (test_rtti.cpp:195) ──
    #[test]
    fn reject_implausible_offset_to_top() {
        let mut data = build_synthetic_itanium_rtti("3Foo");
        let at = (K_IMAGE_BASE + 0x1000 - 16) as usize;
        data[at..at + 8].copy_from_slice(&0x7FFF_FFFF_FFFF_FFFFi64.to_le_bytes());
        let prov = ItProv {
            inner: BufferProvider::new(data, "synthetic-itanium"),
        };
        let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(!info.ok);
        assert!(info.error.contains("offset_to_top"), "{}", info.error);
    }

    // ── rejectsNonItaniumNameString (test_rtti.cpp:208) ──
    #[test]
    fn reject_non_itanium_name() {
        let prov = it_prov("not_a_mangle");
        let info = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 8, 64);
        assert!(!info.ok);
        assert!(info.error.contains("mangle"), "{}", info.error);
    }

    // ── find_owning_module (PORTING step 4) ──
    #[test]
    fn find_owning_module_hit_miss_empty() {
        // empty module list -> always invalid.
        let plain = BufferProvider::new(vec![0u8; 0x100], "x");
        assert!(!find_owning_module(&plain, 0x10).valid);

        let prov = it_prov("3Foo");
        let hit = find_owning_module(&prov, K_IMAGE_BASE + 0x500);
        assert!(hit.valid);
        assert_eq!(hit.name, "synthetic-itanium");
        assert_eq!(hit.base, K_IMAGE_BASE);
        // outside any module.
        assert!(!find_owning_module(&prov, K_IMAGE_BASE + 0x10000).valid);
        assert!(!find_owning_module(&prov, 0x10).valid);
    }

    // ── invalid pointer size guard ──
    #[test]
    fn invalid_pointer_size() {
        let prov = BufferProvider::new(build_synthetic_msvc_rtti(), "synthetic");
        let info = walk_rtti(&prov, K_IMAGE_BASE + 0x1000, 5, 64);
        assert!(!info.ok);
        assert!(info.error.contains("invalid pointer size"));
        let info2 = walk_rtti_itanium(&prov, K_IMAGE_BASE + 0x1000, 5, 64);
        assert!(!info2.ok);
        assert!(info2.error.contains("invalid pointer size"));
    }
}
