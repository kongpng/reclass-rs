# PORTING SPEC — RTTI + Symbol Store + Name Providers (`rtti-symbols`)

Function-level porting spec to drive the faithful Rust implementation of the C++/Qt6
`rtti.*`, `symbolstore.*`, `symbol_downloader.*`, and `names/*` subsystem. Target = 1:1
behavioral parity with the published upstream source. Read alongside the behavioral map
`_design/understand/rtti-symbols.md`, `_design/ARCHITECTURE.md`, `_design/crate_selection.md`,
and the oracle `_oracle/RESULTS.md` (target `test_rtti` 18 pass / 1 skip; `test_rtti_hint` 8 pass).

> Scope reminder: the live OS process/kernel/remote/WinDbg providers are OUT OF SCOPE. This
> subsystem touches the data source ONLY through the abstract `provider::Provider` trait. The
> only providers exercised by these tests are `BufferProvider` and a test-local
> `FakeModuleProvider`/`ItProv` that overrides `enumerate_modules`.

---

## 0. Target crate / module(s)

One package (`reclass`), per `ARCHITECTURE.md §2`. This subsystem lives entirely under
**`src/rtti/`**, gated by the **`symbols`** cargo feature (default-on), mirroring the C++
`rtti.*`+`symbolstore.*`+`symbol_downloader.*`+`names/*` files.

```
src/rtti/
├─ mod.rs              # pub re-exports; module wiring; feature glue
├─ walk.rs             # ← rtti.cpp/.h : RttiInfo, walk_rtti (MSVC), walk_rtti_itanium,
│                      #   find_owning_module, OwningModule, RttiBaseClass, RttiVirtualMethod
├─ demangle.rs         # ← rtti.cpp demanglers + names/symbol_demangle.cpp :
│                      #   demangle_rtti_name, demangle_itanium_name, humanize_symbol_name
├─ symbol_store.rs     # ← symbolstore.* : SymbolStore (global), PdbSymbolSet
├─ downloader.rs       # ← symbol_downloader.* : SymbolDownloader, DownloadRequest, DownloadEvent
│                      #   [requires reqwest + directories — under `symbols` feature]
└─ names/
   ├─ mod.rs           # NamedAddress, trait NameProvider (+ default name_for/address_for),
   │                   #   NameRegistry (global aggregator)
   ├─ pdb.rs           # PdbNameProvider, PdbTypeProvider
   ├─ rtti.rs          # RttiNameProvider (global, Mutex)
   └─ bookmark.rs      # BookmarkNameProvider
```

**Crates pulled in (from `crate_selection.md`):** `cpp_demangle` 0.5.1 (Itanium),
`msvc-demangler` 0.11.0 (MSVC `?`-mangle), `reqwest` 0.13.4 (`rustls-tls`, blocking) for the
downloader, `directories` 6.0.0 for the symbol cache dir. Plus workspace-wide `thiserror`,
`tracing`, `bytemuck`. **Hand-rolled** (parity-critical, off-the-shelf demanglers reject the
bare RTTI forms): `demangle_rtti_name` and the Itanium bare-type fallback in `demangle_itanium_name`.

### Dependencies on other subsystems (must exist or be stubbed first)
- `provider::Provider` trait (`src/provider/mod.rs`) — see §1.0 for the read-adapter.
- `provider::ModuleEntry { name: String, full_path: String, base: u64, size: u64 }`.
- `imports::pdb::PdbTypeInfo { type_index: u32, name: String, size: u64, child_count: i32,
  is_union: bool, is_enum: bool }` — `SymbolStore` only *stores* these; under `--no-default-features`
  (no `imports`) the `symbols` feature must still compile, so `PdbTypeInfo` lives in a small
  always-available `core`/shared location OR `rtti` declares a feature-independent mirror.
  **Decision:** define `PdbTypeInfo` in `core` (it is a plain data struct with serde, used by both
  `imports` and `rtti`), so `symbols` does not transitively require `imports`.
- `controller::{Controller, Document}`, `core::NodeTree`/`Bookmark`, `addr::AddressParser` — used
  ONLY by `BookmarkNameProvider` (§4.7). Keep that dependency edge; the other three providers
  depend only on `SymbolStore` + `Provider`.
- `theme::ThemeManager` — `accent()` colors (GUI-only, `u32` packed `0xAARRGGBB`). In headless
  logic-test builds these return a default; not exercised by oracle tests.

### §1.0 — Provider read-adapter (load-bearing translation)

C++ `Provider::read(addr, buf, len) -> bool` returns a single bool: "did the full `len` bytes
read OK". The Rust trait (`ARCHITECTURE.md §7`) is `read(&self, addr: u64, buf: &mut [u8]) ->
Result<usize>`. RTTI/symbol code is written against the *boolean* contract. Provide an internal
helper used everywhere in this module:

```rust
/// Mirror of C++ `Provider::read(addr,buf,len) -> bool`: true iff the FULL slice was read.
#[inline]
fn pread(p: &dyn Provider, addr: u64, buf: &mut [u8]) -> bool {
    matches!(p.read(addr, buf), Ok(n) if n == buf.len())
}
/// Mirror of `readAs<T>` over little-endian POD; returns (value, ok).
#[inline]
fn read_u32(p: &dyn Provider, addr: u64) -> (u32, bool) {
    let mut b = [0u8; 4];
    let ok = pread(p, addr, &mut b);
    (u32::from_le_bytes(b), ok)            // value defined even on !ok (==0), matching readU32At
}
fn read_u64(p: &dyn Provider, addr: u64) -> (u64, bool) { /* [0u8;8], le */ }
fn read_i64(p: &dyn Provider, addr: u64) -> (i64, bool) { /* le */ }
fn read_i32(p: &dyn Provider, addr: u64) -> (i32, bool) { /* le */ }
```

All byte fields are **little-endian** (RTTI bytes come from x86/x64 images). The C++ relies on
host-LE `memcpy`; the port must use explicit `from_le_bytes` so it stays correct if ever built
big-endian. `readU32At` returns 0 on failure but sets `ok=false`; preserve that (value still
defined, callers branch on `ok`).

---

# PART 1 — RTTI walkers + demanglers (`walk.rs`, `demangle.rs`)

## 1.1 Data structures (`walk.rs`)

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RttiBaseClass {
    pub raw_name: String,       // ".?AVFoo@@"
    pub demangled_name: String, // "Foo"
    pub depth: i32,             // SEE NOTE: set to loop index i, not true hierarchy depth
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RttiVirtualMethod {
    pub slot: i32,
    pub address: u64,
    pub symbol: String,         // SymbolStore::get_symbol_for_address(); empty when no PDB
}

#[derive(Clone, Debug, Default)]
pub struct RttiInfo {
    pub ok: bool,
    pub error: String,          // human-readable; empty when ok
    pub abi: String,            // "MSVC" / "Itanium"; empty when !ok
    pub vtable_address: u64,    // echoed back even on failure
    pub image_base: u64,
    pub module_name: String,
    pub complete_locator: u64,  // MSVC: VA of COL; Itanium: VA of type_info
    pub offset: i32,            // MSVC: COL.offset; Itanium: offset_to_top (truncated to i32)
    pub raw_name: String,
    pub demangled_name: String,
    pub bases: Vec<RttiBaseClass>,
    pub vtable: Vec<RttiVirtualMethod>,
}

#[derive(Clone, Debug, Default)]
pub struct OwningModule {
    pub name: String,
    pub full_path: String,
    pub base: u64,
    pub size: u64,
    pub valid: bool,
}
```

Representation decisions: plain owned structs, `String`/`Vec` (no serde needed — these are
transient walk results, never persisted). `depth: i32` and `offset: i32` keep the C++ `int`
width and signedness exactly (matters for the `info.offset` cast from `int64_t offset_to_top`).

## 1.2 `find_owning_module`

```rust
pub fn find_owning_module(prov: &dyn Provider, addr: u64) -> OwningModule
```
C++ `rtti.cpp:20`. Linear scan over `prov.enumerate_modules()` (returns `Vec<ModuleEntry>`).
Return the **first** module `m` where `addr >= m.base && addr < m.base + m.size`, copying
name/full_path/base/size and `valid=true`. No match → `OwningModule::default()` (`valid=false`).
With an empty module list (plain `BufferProvider`) always invalid. Use `m.base.wrapping_add(m.size)`
or `checked_add` to avoid overflow UB on a pathological `size` (C++ wraps in `uint64_t`; use wrapping
to match). Trivial.

> **Provider naming:** the Rust `Provider` trait exposes `modules(&self) -> &[Module]`
> (ARCHITECTURE) — but the RTTI subsystem needs the *count-able, override-able* `enumerate_modules`
> semantics the test's `FakeModuleProvider` relies on (it increments a call counter and the
> compose cache asserts O(1) calls). **Decision:** the `Provider` trait must expose
> `fn enumerate_modules(&self) -> Vec<ModuleEntry>` (default `Vec::new()`), 1:1 with C++, so a test
> double can count invocations and so `find_owning_module` calls it exactly where C++ does. (The
> `modules()->&[Module]` accessor is a separate convenience; do not route RTTI through it.)

## 1.3 `demangle_rtti_name` — MSVC RTTI type-descriptor demangler (`demangle.rs`)

```rust
pub fn demangle_rtti_name(mangled: &str) -> String
```
C++ `rtti.cpp:38`. **Hand-roll** (do NOT use `msvc-demangler` here — it demangles full `?`-symbols
and emits garbage on bare `.?AV` descriptors, exactly as the C++ comment warns about
`UnDecorateSymbolName`). Algorithm (operate on Unicode scalar values / chars to mirror `QString`;
inputs are ASCII in practice):

```
fn demangle_rtti_name(mangled):
    if mangled.is_empty(): return ""
    if !(mangled.starts_with(".?A") || mangled.starts_with("?A")):
        return mangled.to_owned()                 # verbatim passthrough
    s = mangled
    if s.starts_with('.'): s = &s[1..]            # drop leading '.'
    if s.starts_with('?'): s = &s[1..]            # drop '?'
    if s.chars().count() < 3: return mangled.to_owned()
    body = s[2..]                                  # skip 'A' + class-kind char (V/U/W/X), unused
    term = body.find("@@")
    if term is None: return mangled.to_owned()
    segments = &body[..term]
    parts: Vec<&str> = segments.split('@').filter(|p| !p.is_empty()).collect()  # Qt::SkipEmptyParts
    if parts.is_empty(): return mangled.to_owned()
    parts.reverse()                                # inner-most first → outer::inner
    return parts.join("::")
```

Byte-slicing caveat: because we strip ASCII prefixes (`.`/`?`/2-char), use char-boundary-safe
slicing; inputs are ASCII so `[1..]`/`[2..]` are safe, but compute `body` as
`s.char_indices().nth(2)` start to be robust. `split('@')` + filter-empty reproduces
`Qt::SkipEmptyParts`. Verbatim-on-malformed is **load-bearing** for `demangleMalformed`.

Golden examples (asserted): `.?AVFoo@@`→`Foo`, `.?AUStruct@@`→`Struct`, `.?AVBar@Foo@@`→`Foo::Bar`,
`.?AVZ@Y@X@@`→`X::Y::Z`, `plain_name`→`plain_name`, `""`→`""`.

## 1.4 `demangle_itanium_name` — Itanium type-name demangler (`demangle.rs`)

```rust
pub fn demangle_itanium_name(mangled: &str) -> String
```
C++ `rtti.cpp:284`. Two-tier:

1. Empty → empty.
2. **Preferred path (cross-platform, replaces `__cxa_demangle`):** try `cpp_demangle`. RTTI
   `__name` stores a *bare* mangle (`3Foo`, `N3Bar3FooE`, `St9type_info`) WITHOUT the `_Z`
   prefix — `cpp_demangle::Symbol::new` expects a full `_Z…` symbol and will reject these. So:
   `cpp_demangle::Symbol::new(format!("_Z{mangled}"))`. If it parses, `to_string()` and `trim()`.
   **BUT** prefixing `_Z` changes the grammar position (a top-level `_Z` expects an
   *encoding*, i.e. a function name + bare-function-type, not a bare `<type>`), so for the
   plain type forms the test feeds this will usually still fail → fall through. Treat
   `cpp_demangle` as best-effort for the rich forms (templates, real `_Z…` typeinfo names that
   arrive via `humanize_symbol_name`), and **rely on the hand-rolled fallback for the bare type
   names** the RTTI walker produces. Document this clearly; do not gate test parity on the crate.

   > Implementation note: only attempt the crate path when `mangled` looks like a full symbol
   > (already `_Z…`) OR a typeinfo/vtable name; for the bare `<digit>`/`N…E`/`St…` type forms go
   > straight to the fallback. The fallback is what the synthetic tests exercise.

3. **Hand-rolled fallback** (also the MSVC-build path in C++; here the always-correct path for
   bare types). Reproduce exactly:

```
parse_length(s, &mut pos) -> i32:
    n = 0; any = false
    while pos < len && s[pos].is_ascii_digit():
        n = n*10 + (digit); pos += 1; any = true
    return if any { n } else { -1 }

consume_segment(s, &mut pos, parts) -> bool:
    if pos >= len: return false
    if s[pos]=='S' && pos+1<len && s[pos+1]=='t':         # "St" shorthand → "std"
        parts.push("std"); pos += 2; return true
    len_seg = parse_length(s, &mut pos)
    if len_seg <= 0 || pos + len_seg > len: return false
    parts.push(s[pos .. pos+len_seg]); pos += len_seg; return true

demangle_itanium_name(mangled):
    if empty: return ""
    # (crate attempt elided per step 2)
    s = mangled; p = 0; parts = []
    if s[0] == 'N':
        p = 1
        while p < len && s[p] != 'E':
            if !consume_segment(s, &mut p, parts): return mangled.to_owned()
        if p >= len || s[p] != 'E': return mangled.to_owned()
    else:
        if !consume_segment(s, &mut p, parts): return mangled.to_owned()
    if parts.is_empty(): return mangled.to_owned()
    return parts.join("::")
```

Indexing must be over **bytes** (ASCII mangle) to match `QString` index/length semantics on the
ASCII inputs; guard `s[p]` accesses with bounds (use `s.as_bytes()`). `n = n*10 + digit` uses
`i32` (overflow not a concern for these lengths; match C++ `int`).

Golden: `3Foo`→`Foo`, `N3Bar3FooE`→`Bar::Foo`, `St9type_info` ends-with `type_info`,
`plain_text`→`plain_text`.

> The C++ `St9type_info` path: with the fallback, `consume_segment` sees `S`+`t` → pushes
> `"std"`, then parse_length reads `9`, pushes `type_info` → `std::type_info` (ends-with
> `type_info` ✓). If the `cpp_demangle` path is taken for a real `_ZTISt9type_info`-style
> input it would also produce `std::type_info`; either way the `ends_with` assertion holds.

## 1.5 `walk_rtti` — MSVC walker (`walk.rs`)

```rust
pub fn walk_rtti(prov: &dyn Provider, vtable_addr: u64,
                 pointer_size: i32 /*=8*/, max_vtable_slots: i32 /*=64*/) -> RttiInfo
```
C++ `rtti.cpp:108`. No default-args in Rust → callers pass explicitly; provide a thin
`walk_rtti_default(prov, addr)` = `walk_rtti(prov, addr, 8, 64)` for ergonomics if wanted, but
compose calls with `(8, 0)`. Step-by-step (each step cites C++ behavior the tests pin):

```
info = RttiInfo::default(); info.vtable_address = vtable_addr
if pointer_size != 4 && pointer_size != 8:
    info.error = "invalid pointer size"; return info

# 1. COL pointer at vtable[-ptrSize]  (absolute VA on both x86 & x64)
meta = vtable_addr - ptrSize
(col_addr, ok) = if ptrSize==8 { read_u64(prov, meta) }
                 else { let (v,o)=read_u32(prov,meta); (v as u64, o) }
if !ok || col_addr == 0:
    info.error = "could not read meta pointer at vtable[-1]"; return info
info.complete_locator = col_addr

# 2. image base
owner = find_owning_module(prov, col_addr)
image_base = 0
if owner.valid:
    info.module_name = owner.name; image_base = owner.base
else if ptrSize == 8:
    # fallback: COL.pSelf @ +0x14 is the module image base (lets synthetic tests work)
    let (ib, ib_ok) = read_u32(prov, col_addr + 0x14)
    if ib_ok && ib != 0: image_base = ib as u64
info.image_base = image_base

# 3. COL signature @ +0x00
let (sig, sig_ok) = read_u32(prov, col_addr + 0x00)
if !sig_ok: info.error = "could not read COL signature"; return info
if sig != 0 && sig != 1:
    info.error = format!("COL signature 0x{sig:x} not 0/1 — not MSVC RTTI"); return info

# 4. offset @ +0x04
let (off, _) = read_u32(prov, col_addr + 0x04); info.offset = off as i32

# 5. pTypeDescriptor @ +0x0C, pClassHierarchy @ +0x10
let (td_field, td_ok)  = read_u32(prov, col_addr + 0x0C)
let (chd_field, chd_ok)= read_u32(prov, col_addr + 0x10)
if !td_ok || !chd_ok: info.error="could not read COL TypeDescriptor / CHD fields"; return info
td_addr  = rtti_resolve(td_field,  image_base, ptrSize)
chd_addr = rtti_resolve(chd_field, image_base, ptrSize)

# 6. TypeDescriptor name @ td + 2*ptrSize  (NUL-terminated, UTF-8, max 512)
name_off = 2 * ptrSize
info.raw_name = read_cstring(prov, td_addr + name_off, 512)
info.demangled_name = demangle_rtti_name(&info.raw_name)   # computed BEFORE empty-check
if info.raw_name.is_empty(): info.error="type descriptor name empty"; return info

# 7. CHD: numBaseClasses @ +0x08, pBaseClassArray @ +0x0C
let (num_bases, nb_ok)  = read_u32(prov, chd_addr + 0x08)
let (bca_field, bca_ok) = read_u32(prov, chd_addr + 0x0C)
if !nb_ok || !bca_ok: info.error="could not read CHD"; return info
if num_bases > 256:
    info.error = format!("CHD.numBaseClasses unreasonably large ({num_bases}) — not RTTI")
    return info
bca_addr = rtti_resolve(bca_field, image_base, ptrSize)
entry_size = if ptrSize==8 { 4 } else { ptrSize }      # 32-bit RVA on x64; ptr on x86

# 8. base-class array
for i in 0..num_bases:
    let (bcd_field, e_ok) = read_u32(prov, bca_addr + i*entry_size)
    if !e_ok: break                                    # stop, keep what we have
    bcd_addr = rtti_resolve(bcd_field, image_base, ptrSize)
    let (bcd_td, tdf_ok) = read_u32(prov, bcd_addr + 0x00)
    if !tdf_ok: continue                               # skip this base
    bcd_td_addr = rtti_resolve(bcd_td, image_base, ptrSize)
    raw_base = read_cstring(prov, bcd_td_addr + name_off, 512)
    info.bases.push(RttiBaseClass{ raw_name: raw_base.clone(),
                                   demangled_name: demangle_rtti_name(&raw_base),
                                   depth: i as i32 })   # NOTE: depth == loop index i

# 9. vtable enumeration
for slot in 0..max_vtable_slots:
    entry = vtable_addr + slot*ptrSize
    let (target, tok) = if ptrSize==8 { read_u64(prov,entry) }
                        else { let (v,o)=read_u32(prov,entry); (v as u64,o) }
    if !tok: break
    if target == 0: break                              # null terminator stops enumeration
    in_some_module = if !owner.valid { true }          # synthetic: trust the input
                     else { find_owning_module(prov, target).valid }
    if !in_some_module: break
    info.vtable.push(RttiVirtualMethod{
        slot, address: target,
        symbol: SymbolStore::global().get_symbol_for_address(target, Some(prov)) })

info.ok = true; info.abi = "MSVC".into(); return info
```

Helpers (`walk.rs`, private):
```rust
#[inline] fn rtti_resolve(field: u32, image_base: u64, ptr_size: i32) -> u64 {
    if ptr_size == 8 { image_base.wrapping_add(field as u64) } else { field as u64 }
}
fn read_cstring(p: &dyn Provider, addr: u64, max_len: usize /*=512*/) -> String {
    let mut out = Vec::with_capacity(64);
    for i in 0..max_len {
        let mut b = [0u8; 1];
        if !pread(p, addr + i as u64, &mut b) { break }     // read fail stops
        if b[0] == 0 { break }                              // NUL stops
        out.push(b[0]);
    }
    String::from_utf8_lossy(&out).into_owned()              // MSVC walker = UTF-8 (lossy ≈ fromUtf8)
}
```

Edge-case parity (all test-load-bearing): the redundant `prov.readAs<uint64_t>` at C++ `rtti.cpp:124`
is harmless and is simply dropped (the `read_u64` covers it). `arg(sig,0,16)` → `format!("{:x}")`
(lowercase, no `0x` already prefixed literally as `0x{:x}`). Synthetic-image-base fallback
(`+0x14`) is MSVC-only and is what makes `walkSyntheticRtti` pass without an enumerable module
(`info.image_base == 0x10000`, from `col+0x14 = kImageBase`). `info.offset == 0` for the synthetic.
First base `bases[0].demangled_name == "Foo"` (the class itself). Vtable size 5 (slot 5 is null).

## 1.6 `walk_rtti_itanium` — Itanium walker (`walk.rs`)

```rust
pub fn walk_rtti_itanium(prov: &dyn Provider, vtable_addr: u64,
                         pointer_size: i32 /*=8*/, max_vtable_slots: i32 /*=64*/) -> RttiInfo
```
C++ `rtti.cpp:354`. **Unlike MSVC, requires real modules** — every dereferenced pointer must land
inside an enumerated module (no synthetic image-base fallback). Steps:

```
info.vtable_address = vtable_addr
validate ptrSize (4/8) else "invalid pointer size"

# 1. type_info* @ vtable[-ptrSize]
ti_ptr = vtable_addr - ptrSize
(ti_addr, ok) = read ptr (u64 or u32→u64)
if !ok || ti_addr==0: error "could not read type_info pointer at vtable[-1]"; return

# 2. type_info must be in a module
ti_owner = find_owning_module(prov, ti_addr)
if !ti_owner.valid: error "type_info pointer outside any module"; return
info.image_base = ti_owner.base; info.module_name = ti_owner.name; info.complete_locator = ti_addr

# 3. offset_to_top @ vtable[-2*ptrSize]  (SIGNED; i64 on x64, i32 on x86; default 0 on read fail)
addr = vtable_addr - ptrSize*2
offset_to_top: i64 = if ptrSize==8 { let (v,o)=read_i64(prov,addr); if o {v} else {0} }
                     else          { let (v,o)=read_i32(prov,addr); if o {v as i64} else {0} }
if offset_to_top > 0x1000000 || offset_to_top < -0x1000000:
    error "offset_to_top implausible — not Itanium RTTI"; return
info.offset = offset_to_top as i32

# 4. type_info[0] = abi type_info vtable ptr — must be non-zero & in a module
(ti_vtable, ok) = read ptr at ti_addr
if !ok: error "could not read type_info vtable ptr"; return
if ti_vtable==0 || !find_owning_module(prov, ti_vtable).valid:
    error "type_info vtable not in any module"; return

# 5. type_info[ptrSize] = char* __name
(name_ptr, ok) = read ptr at ti_addr + ptrSize
if !ok: error "could not read __name pointer"; return
if name_ptr==0 || !find_owning_module(prov, name_ptr).valid:
    error "__name pointer not in any module"; return

# 6. read mangled name (max 256). Inlined loop (Latin-1 + printable filter) — NOT read_cstring.
name_bytes: Vec<u8> = []
for i in 0..256:
    let mut b=[0u8;1]
    if !pread(prov, name_ptr + i, &mut b): break
    if b[0]==0: break
    if b[0] < 0x20 || b[0] > 0x7E: name_bytes.clear(); break   # non-printable → reject ALL & stop
    name_bytes.push(b[0])
if name_bytes.len() < 2: error "__name string empty or non-printable"; return

# 7. vague-linkage '*' prefix
validate_off = if name_bytes[0]==b'*' { 1 } else { 0 }
if validate_off >= name_bytes.len(): error "__name is just a vague-linkage marker"; return

# 8. first real char must be a mangle marker
c0 = name_bytes[validate_off]
if !(c0.is_ascii_digit() || c0 in {b'N',b'S',b'P',b'K',b'R'}):
    error "__name doesn't start with Itanium mangle marker"; return

# 9. store
info.raw_name = latin1_to_string(&name_bytes)                  # INCLUDES '*' prefix (shows literal memory)
info.demangled_name = demangle_itanium_name(&latin1_to_string(&name_bytes[validate_off..]))

# 10. vtable enumeration — ALWAYS real-module check (no synthetic trust)
for slot in 0..max_vtable_slots:
    entry = vtable_addr + slot*ptrSize
    (target, tok) = read ptr at entry
    if !tok: break
    if target==0: break
    if !find_owning_module(prov, target).valid: break
    info.vtable.push(RttiVirtualMethod{ slot, address: target,
        symbol: SymbolStore::global().get_symbol_for_address(target, Some(prov)) })

info.ok = true; info.abi = "Itanium".into(); return info
```

`latin1_to_string(bytes)` = `bytes.iter().map(|&b| b as char).collect::<String>()` (1:1 with
`QString::fromLatin1`, each byte → U+00xx). The name loop is **distinct** from `read_cstring`
(Latin-1 + printable filter + clear-on-nonprintable) — keep both. `offset_to_top` magnitude
filter (`±0x1000000`) and the first-char marker set (`digit | N S P K R`) are pinned by
`rejectsImplausibleOffsetToTop` and `rejectsNonItaniumNameString`.

## 1.7 `humanize_symbol_name` — demangler façade (`demangle.rs`)

```rust
pub fn humanize_symbol_name(mangled: &str) -> String     // empty == "keep raw"
```
C++ `symbol_demangle.cpp:11`. **Contract: returns "" when input is already human-readable / unchanged;
a non-empty return means "use this instead of raw".** Preserve exactly:

```
if mangled.is_empty(): return ""
# 1. MSVC RTTI type descriptor
if mangled.starts_with(".?A") || mangled.starts_with("?A"):
    d = demangle_rtti_name(mangled)
    return if d != mangled { d } else { "".into() }
# 2. Itanium (_Z…)
if mangled.starts_with("_Z"):
    d = demangle_itanium_name(mangled)
    return if d != mangled && !d.is_empty() { d } else { "".into() }
# 3. MSVC function/method mangle ('?...', '_?...')
#    C++ gated on Q_OS_WIN via UnDecorateSymbolName. PORT: use msvc-demangler CROSS-PLATFORM
#    (an upgrade), but match the empty-on-unchanged / not-a-'?'-mangle semantics.
let trimmed = mangled.strip_prefix('_').unwrap_or(mangled);
if !trimmed.starts_with('?'): return ""
let flags = DemangleFlags::NAME_ONLY | NO_ACCESS_SPECIFIERS | NO_THISTYPE | NO_RETURN_UDT_MODEL
match msvc_demangler::demangle(trimmed, flags) {
    Ok(out) if !out.is_empty() && out != trimmed && out != mangled => out,
    _ => "".into(),
}
```

`msvc-demangler` flag mapping (conceptually equal to the dbghelp `UNDNAME_*` set; pick the
crate's closest flags — `NAME_ONLY`, `NO_ACCESS_SPECIFIERS`, `NO_MS_THISTYPE`/`NO_THISTYPE`,
`NO_RETURN_TYPE`/UDT-model equivalent). If `msvc-demangler` errors, return "" (raw kept). This
file's only behavioral oracle is indirect (via `PdbNameProvider`); no direct `test_*` asserts
`humanize_symbol_name` outputs, but the empty-on-unchanged contract is required by
`PdbNameProvider::name_for` (§4.4) — add a Rust unit test (§TEST 1.7).

---

# PART 2 — Symbol Store (`symbol_store.rs`)

## 2.1 `PdbSymbolSet`

```rust
#[derive(Clone, Debug, Default)]
pub struct PdbSymbolSet {
    pub pdb_path: String,                       // empty marks an RTTI-only set
    pub module_name: String,                    // canonical lowercase (e.g. "ntoskrnl")
    pub name_to_rva: HashMap<String, u32>,      // forward lookup
    pub name_to_type_index: HashMap<String, u32>,
    pub rva_to_name: Vec<(u32, String)>,        // kept SORTED ascending by .0 for binary search
    pub types: Vec<PdbTypeInfo>,                // address-less TPI defs
}
impl PdbSymbolSet {
    fn sort_rva_index(&mut self) { self.rva_to_name.sort_by_key(|e| e.0); }
}
```
Use `ahash`-backed `HashMap` for the hot name maps (interior, not exposed). `rva_to_name` is a
plain `Vec<(u32,String)>`; **stable-sort by RVA** — C++ uses `std::sort` (unstable); equal RVAs are
rare and reverse lookup only needs *an* entry ≤ target, so either sort matches behavior. Use
`sort_by_key` (stable) for determinism. `name_to_rva` insertion is **first-wins** (dedupe).

## 2.2 `SymbolStore` — process-global

C++ Meyers singleton, NOT internally synchronized (single-threaded GUI assumption). The port
makes it a guarded global so concurrent compose/panel access is sound:

```rust
pub struct SymbolStore {
    modules: HashMap<String, PdbSymbolSet>,     // canonical → set
    aliases: HashMap<String, String>,           // alias → canonical
}
impl SymbolStore {
    fn new() -> Self {                           // constructor seeds kernel aliases
        let mut aliases = HashMap::new();
        for a in ["nt","ntkrnlmp","ntkrnlpa","ntkrpamp"] { aliases.insert(a.into(),"ntoskrnl".into()); }
        SymbolStore { modules: HashMap::new(), aliases }
    }
    pub fn global() -> &'static Mutex<SymbolStore> {
        static G: OnceLock<Mutex<SymbolStore>> = OnceLock::new();
        G.get_or_init(|| Mutex::new(SymbolStore::new()))
    }
}
```

> Lock discipline: `get_symbol_for_address` / `resolve` are called from compose (per vtable
> method, potentially in a worker) AND from the UI. Take the `Mutex` for the duration of each
> public method. The methods do not re-enter the store, so no deadlock. (Contrast: C++ has no
> lock at all; the Rust lock is a safety upgrade with identical observable results.) Public API
> may be free functions on a `MutexGuard` or methods on the guard's `&mut`/`&` — present them as
> `SymbolStore` inherent methods called via `global().lock()`.

### Methods (all C++-cited)

`resolve_alias(&self, name) -> String` (C++ `symbolstore.h:100`):
```
lower = name.to_lowercase()
if lower ends_with .exe/.dll/.sys: lower = lower[..lower.rfind('.').unwrap()]   # strip ext
self.aliases.get(&lower).cloned().unwrap_or(lower)
```
`to_lowercase()` matches `QString::toLower()` for ASCII module names. Strip uses `rfind('.')`
(== `lastIndexOf('.')`). Central to every module lookup.

`add_module(&mut self, module_name, pdb_path, symbols: &[(String,u32)]) -> i32` (C++ `:20`):
- `canonical = self.resolve_alias(module_name)`.
- Fresh `PdbSymbolSet { pdb_path, module_name: canonical.clone(), .. }`; reserve.
- For each `(name, rva)`: **skip if `name_to_rva` contains name** (first-wins); else
  `name_to_rva.insert(name, rva)` and `rva_to_name.push((rva, name))`.
- `sort_rva_index(); count = name_to_rva.len()`.
- Auto-alias: `raw_lower = module_name.to_lowercase()`, strip ext; if `raw_lower != canonical`
  → `aliases.insert(raw_lower, canonical.clone())`.
- `modules.insert(canonical, set)` (**replaces** any existing set).
- `tracing::debug!` log (replaces `qDebug`). Return `count as i32`.

`add_module_type_indices(&mut self, module_name, map: HashMap<String,u32>)` (C++ `:55`):
resolve alias; if module absent → **no-op**; else overwrite `set.name_to_type_index = map`.

`add_module_types(&mut self, module_name, types: Vec<PdbTypeInfo>)` (C++ `:63`):
resolve alias; if absent → no-op; else overwrite `set.types = types`.

`add_rtti_hits(&mut self, module_name, hits: &[(String,u32)])` (C++ `:71`):
resolve alias; **if module absent, create an empty `PdbSymbolSet` (pdb_path="" = RTTI-only)** and
insert it. Then for each `(name,rva)`: skip if name present; else insert + push to `rva_to_name`.
`sort_rva_index()`.

`type_index_for_symbol(&self, qualified: &str) -> u32` (C++ `:92`):
```
bang = qualified.find('!')
if bang is None || bang == 0 || bang == qualified.len()-1: return 0   # require module!symbol
mod_part = &qualified[..bang]; sym_part = &qualified[bang+1..]
canonical = self.resolve_alias(mod_part)
self.modules.get(&canonical)?.name_to_type_index.get(sym_part).copied().unwrap_or(0)
```
> C++ `bangIdx <= 0` ⇒ reject when `!` absent (`indexOf` returns -1) or at pos 0; in Rust
> `find` returns byte index — `0` means leading `!`. Use the byte index; ASCII `!` so byte==char.

`unload_module(&mut self, module_name)` (C++ `:104`): `self.modules.remove(&self.resolve_alias(module_name))`.

`resolve(&self, token: &str, provider: Option<&dyn Provider>) -> (u64, bool)` (C++ `:109`) —
returns `(value, ok)` instead of out-param `bool* ok`:
```
ok = false
bang = token.find('!')
if let Some(b) = bang && b > 0 && b < token.len()-1:               # qualified module!symbol
    mod_part=&token[..b]; sym_part=&token[b+1..]; canonical=resolve_alias(mod_part)
    set = match modules.get(&canonical) { Some(s)=>s, None=>return (0,false) }
    rva = match set.name_to_rva.get(sym_part) { Some(r)=>*r, None=>return (0,false) }
    base = get_module_base(provider, &canonical)
    if base == 0 { base = get_module_base(provider, mod_part) }    # retry user-supplied form
    return (base.wrapping_add(rva as u64), true)                   # ok even if base==0 (bare RVA)
# bare symbol — scan all modules, ambiguity check
found_rva=0; found_module=""; matches=0
for (key, set) in &modules:
    if let Some(r) = set.name_to_rva.get(token):
        found_rva=*r; found_module=key.clone(); matches+=1
        if matches > 1: return (0, false)                          # ambiguous → 0, ok stays false
if matches == 1:
    base = get_module_base(provider, &found_module)
    return (base.wrapping_add(found_rva as u64), true)
# matches == 0 → fallback: treat bare token as a module name
canonical = resolve_alias(token); base = get_module_base(provider, &canonical)
if base != 0 { return (base, true) }
return (0, false)
```
> Iteration order over `modules` is `HashMap` (non-deterministic, like `QHash`). For the
> ambiguity check the order does not matter (any >1 → 0). For the matches==1 path the single
> match is order-independent. Acceptable to use `HashMap`; if a future test needs deterministic
> single-module behavior it already holds (one match).

`get_symbol_for_address(&self, addr: u64, provider: Option<&dyn Provider>) -> String` (C++ `:172`):
```
if modules.is_empty() || provider.is_none(): return ""
for (_, set) in &modules:
    base = get_module_base(provider, &set.module_name)
    if base == 0: continue                            # require live-attached (no false positives)
    if addr < base: continue
    rva = (addr - base) as u32                         # C++ static_cast<uint32_t> (truncating)
    if set.rva_to_name.is_empty(): continue
    # binary search: first entry with .0 > rva, then step back one
    idx = set.rva_to_name.partition_point(|e| e.0 <= rva)   # == std::upper_bound on key
    if idx == 0: continue                              # target before all symbols
    (sym_rva, sym_name) = &set.rva_to_name[idx-1]
    disp = rva - sym_rva
    if disp > 0x1000: continue                         # kMaxDisplacement cap
    return if disp==0 { format!("{}!{}", set.module_name, sym_name) }
           else       { format!("{}!{}+0x{:x}", set.module_name, sym_name, disp) }
return ""                                              # returns FIRST module's hit (QHash order)
```
> `partition_point(|e| e.0 <= rva)` gives the count of entries with key ≤ rva, i.e. the index
> just past the last `<= rva` — equivalent to `std::upper_bound(rva)` then the C++ `--upper`
> yields index `idx-1` (last entry with key ≤ rva). The `rva - sym_rva` is `u32` (matches C++
> `uint32_t displacement`). `+0x{:x}` is lowercase hex, no zero-pad (== `QString::number(d,16)`).

`get_module_base(&self, provider, canonical) -> u64` (C++ `:7`, private):
```
let Some(p) = provider else { return 0 };
let mut base = p.symbol_to_address(canonical);
if base==0 { base = p.symbol_to_address(&format!("{canonical}.exe")) }
if base==0 { base = p.symbol_to_address(&format!("{canonical}.dll")) }
if base==0 { base = p.symbol_to_address(&format!("{canonical}.sys")) }
base
```

`add_alias(&mut self, alias, canonical)` (C++ `:216`):
`self.aliases.insert(alias.to_lowercase(), canonical.to_lowercase())`.

Inline accessors: `has_symbols()` = `!modules.is_empty()`; `loaded_modules() -> Vec<String>`
(== `m_modules.keys()`); `module_count() -> usize`; `module_data(&self, name) -> Option<&PdbSymbolSet>`
(resolve alias → `modules.get(&canonical)`).

---

# PART 3 — Symbol Downloader (`downloader.rs`)  [feature `symbols`]

C++ `SymbolDownloader : QObject` (async, Qt signals). Port to a struct with a callback/event
channel; `reqwest` blocking on a worker thread. **The cache path format and the MS server URL
are exact behavioral contracts — replicate verbatim.**

```rust
#[derive(Clone, Debug, Default)]
pub struct DownloadRequest {
    pub module_name: String,  // display, e.g. "ntoskrnl.exe"
    pub pdb_name: String,     // e.g. "ntoskrnl.pdb"
    pub guid_string: String,  // 32 hex chars, no dashes
    pub age: u32,
}
pub enum DownloadEvent {                       // replaces Qt signals
    Progress { module_name: String, received: i64, total: i64 },
    Finished { module_name: String, local_path: String, success: bool, error: String },
}
pub struct SymbolDownloader { /* handle to the single active task + cancel flag */ }
```

`cache_dir() -> PathBuf` (C++ `:19`): `QStandardPaths::AppLocalDataLocation + "/SymbolCache"`.
Rust: `directories::ProjectDirs::from(qualifier, org, app)` → `.data_local_dir()` + `"SymbolCache"`.
Use the same org/app identity the rest of the app uses (define once in a shared `app_dirs()`).
On Windows `data_local_dir()` = `%LOCALAPPDATA%\<org>\<app>\data` (matches `AppLocalDataLocation`);
keep cross-platform.

`find_cached(&self, req) -> Option<PathBuf>` (C++ `:24`): build
`cache_dir()/{pdb_name}/{guid}{age:x}/{pdb_name}` (age = **lowercase hex, no `0x`**, via
`format!("{:x}", req.age)` == `QString::number(age,16)`). Return if the file exists.

`find_local(module_full_path, pdb_name) -> Option<PathBuf>` (static, C++ `:33`): if either empty
→ None. Candidate = `<dir-of-module>/<pdb_name>`; return if exists. (`Path::parent()` == Qt
`absolutePath()`; do NOT canonicalize — Qt's `absolutePath` only normalizes, but for parity a
plain parent join is sufficient since the test surface doesn't assert canonicalization.)

`download(&self, req, on_event: impl Fn(DownloadEvent) + Send)` (C++ `:44`): URL =
`https://msdl.microsoft.com/download/symbols/{pdb_name}/{guid}{age:x}/{pdb_name}`. Headers:
`User-Agent: Microsoft-Symbol-Server/10.0.0.0`. Redirects: reqwest default policy (limited safe
redirects ≈ `NoLessSafeRedirectPolicy`). Single active download → `cancel()` first. On finish:
- transport error → `Finished{success:false, error: format!("Download failed: {e}")}`.
- HTTP status != 200 → `error: format!("HTTP {status}")`.
- empty body → `error: "Empty response"`.
- else `create_dir_all(cache_dir()/{pdb}/{guid}{age:x})`, write `{dir}/{pdb}`; on write error
  → `error: format!("Cannot write: {e}")`; on success → `Finished{local_path, success:true, error:""}`.
Progress: map reqwest's downloaded/total to `Progress{received as i64, total as i64}` (C++
truncates to `int`; the event carries i64, the consumer may truncate — match the truncation at
the UI boundary, not here, since the i64 carries strictly more info; if exact parity is wanted
emit `received as i32 as i64`). Run blocking on a spawned thread; `cancel()` sets an atomic flag /
drops the handle (reqwest blocking has no abort — use a cancellation token checked between chunks
via a streaming `Response` + `chunk()` loop to honor `cancel()` like Qt's `abort()`).

> **No oracle test** covers the downloader (network + Windows symbol server; not in the headless
> set). Port for behavioral fidelity; cover with a unit test that asserts URL + cache-path
> construction only (§TEST 3).

---

# PART 4 — Name Providers (`names/`)

## 4.1 `NamedAddress` (`names/mod.rs`)

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedAddress {
    pub name: String,          // canonical identifier (reverse lookups)
    pub display_name: String,  // humanised; empty → consumers use `name`
    pub address: u64,          // ABSOLUTE; 0 == "no live address" (renders —, not navigable)
    pub size: u32,             // 0 = unknown
    pub type_index: u32,       // non-zero ⇒ "Import type" affordance
    pub source: String,        // filled on aggregation (= provider id / module)
    pub kind: String,          // "symbol"/"type"/"bookmark"/"rtti"/"struct"/"union"/"enum"
    pub meta: String,          // provider-private (e.g. PDB path for type-import)
}
```
Plain owned struct; no serde (transient UI/lookup data).

## 4.2 `NameProvider` trait (`names/mod.rs`)

```rust
pub trait NameProvider: Send + Sync {
    fn id(&self) -> String;                                   // pure-virtual
    fn display_name(&self) -> String;                         // pure-virtual
    fn entries(&self, active: Option<&dyn Provider>) -> Vec<NamedAddress>;  // pure-virtual
    fn accent(&self) -> u32 { 0 }                             // 0 = "no opinion", 0xAARRGGBB
    // Default reverse lookups: linear scan over entries(); override for O(1).
    fn name_for(&self, addr: u64, active: Option<&dyn Provider>) -> String {
        if addr == 0 { return String::new() }
        self.entries(active).into_iter().find(|e| e.address == addr).map(|e| e.name).unwrap_or_default()
    }
    fn address_for(&self, name: &str, active: Option<&dyn Provider>) -> u64 {
        if name.is_empty() { return 0 }
        self.entries(active).iter().find(|e| e.name == name).map(|e| e.address).unwrap_or(0)
    }
    fn supports_add(&self) -> bool { false }
    fn add(&self, _name: &str, _address: u64) -> bool { false }
    fn supports_remove(&self) -> bool { false }
    fn remove(&self, _name: &str) -> bool { false }
}
```
> `add`/`remove`/`push`/`clear` mutate provider-internal state. C++ uses non-const virtuals on a
> `shared_ptr<NameProvider>`. In Rust the registry holds `Arc<dyn NameProvider>`, so mutating
> providers use **interior mutability** (BookmarkNameProvider delegates to a `Controller`;
> RttiNameProvider has its own `Mutex`). Hence `add`/`remove` take `&self`.

`Send + Sync` is required because `RttiNameProvider` is a global shared across threads and the
registry is global.

## 4.3 `NameRegistry` — global aggregator (`names/mod.rs`)

```rust
pub struct NameRegistry {
    providers: Vec<Arc<dyn NameProvider>>,
    on_changed: Vec<Box<dyn Fn() + Send + Sync>>,   // replaces Qt providersChanged() signal
}
impl NameRegistry {
    pub fn global() -> &'static Mutex<NameRegistry> { static G: OnceLock<…>; … }
    pub fn register_provider(&mut self, p: Arc<dyn NameProvider>);   // idempotent by id()
    pub fn unregister_provider(&mut self, id: &str);
    pub fn providers(&self) -> Vec<Arc<dyn NameProvider>>;           // clone of the Vec
    pub fn name_for(&self, addr: u64, active: Option<&dyn Provider>) -> String;
    pub fn address_for(&self, name: &str, active: Option<&dyn Provider>) -> u64;
    pub fn emit_changed(&self);                                      // run all on_changed callbacks
    pub fn subscribe(&mut self, cb: Box<dyn Fn()+Send+Sync>);        // listener registration
}
```
- `register_provider` (C++ `name_registry.cpp:10`): **idempotent by `id()`** — if a provider with
  the same id exists, **replace in place** (same index); else append. Then `emit_changed()`.
- `unregister_provider`: remove first match by id; `emit_changed()`. (C++ only emits when found;
  match that — only emit if a removal happened.)
- `name_for` (C++ `:35`): `addr==0` → ""; iterate providers **in registration order**; first
  non-empty `p.name_for()` wins.
- `address_for`: empty name → 0; first nonzero `p.address_for()` wins.
- `emit_changed`: invoke all subscriber callbacks (Qt `emit providersChanged()`).

> **Registration order** (set in `main.rs`, mirroring `main.cpp:1054`): `PdbNameProvider`,
> `PdbTypeProvider`, `RttiNameProvider`, `BookmarkNameProvider`. So PDB symbols win reverse
> lookups over RTTI/bookmarks. `RttiNameProvider` is the singleton — register an `Arc` that
> aliases the global (see §4.6); C++ used a no-op-deleter `shared_ptr`.

## 4.4 `PdbNameProvider` (`names/pdb.rs`)

`id()="pdb-symbols"`, `display_name()="PDB Symbols"`, `accent()` = theme `syntaxKeyword` packed
(GUI-only; headless default 0).

`entries(active)` (C++ `pdb_name_provider.cpp:23`): take `SymbolStore::global().lock()`; for each
`loaded_modules()` → `module_data(mod)`; `base = module_base_for(active, mod)`. For each
`(sym_name, rva)` in `name_to_rva`:
```
NamedAddress {
    name: sym_name,
    display_name: humanize_symbol_name(sym_name),
    address: if base != 0 { base + rva } else { 0 },   # CRITICAL: base==0 → 0, NOT 0+rva
    type_index: set.name_to_type_index.get(sym_name).copied().unwrap_or(0),
    kind: "symbol", meta: set.pdb_path, ..
}
```
**Critical correctness rule** (`pdb_name_provider.cpp:34-48`): when the owning module is not
live-attached (`base==0`), `address=0` so reverse lookup skips it (no false-match against a fresh
user document at the symbol's RVA). The row still appears for browsing.

`module_base_for(active, canonical) -> u64` (static, C++ `:14`): identical `symbol_to_address` +
`.exe/.dll/.sys` cascade as `SymbolStore::get_module_base`. (Share one helper.)

`name_for(addr, active)` (override, C++ `:68`): `addr==0`→""; `raw =
SymbolStore::global().lock().get_symbol_for_address(addr, active)`; empty→""; split on first `!`:
if none → return `raw`; `prefix = raw[..=bang]` ("module!"); `rest = raw[bang+1..]`; split `rest`
on first `+`: `sym` = before, `suffix` = `+…` or ""; `humanized = humanize_symbol_name(sym)`;
return `prefix + (if humanized.is_empty() { sym } else { &humanized }) + suffix`. Routes through
SymbolStore's live-attached binary-search reverse lookup — never false-matches an unloaded PDB.

`address_for(name, active)` (override, C++ `:83`): `(a, ok) =
SymbolStore::global().lock().resolve(name, active)`; return `if ok { a } else { 0 }`. Accepts
both `module!symbol` and bare.

## 4.5 `PdbTypeProvider` (`names/pdb.rs`)

`id()="pdb-types"`, `display_name()="PDB Types"`, `accent()` = theme `syntaxType` packed.

`entries(_active)` (C++ `pdb_type_provider.cpp:16`): for each loaded module's `set.types`:
```
NamedAddress {
    name: ti.name, display_name: humanize_symbol_name(&ti.name),
    address: 0,                                  # always address-less
    size: ti.size as u32, type_index: ti.type_index,
    kind: if ti.is_enum {"enum"} else if ti.is_union {"union"} else {"struct"},
    meta: set.pdb_path, ..
}
```
Inherited (linear-scan) `name_for`/`address_for`: since all addresses are 0, `name_for` always
returns "" (the `addr==0` guard) and `address_for` returns 0 — effectively reverse-lookup-inert.
Exists for the Symbols-panel listing + type import.

## 4.6 `RttiNameProvider` — global, Mutex-protected (`names/rtti.rs`)

The ONLY mutexed component in this subsystem (RTTI hits pushed from compose while the panel reads).

```rust
struct RttiHits { hits: Vec<NamedAddress>, by_key: HashMap<String, usize> }  // "name@hex(addr)" → idx
pub struct RttiNameProvider { lock: Mutex<RttiHits> }
impl RttiNameProvider {
    pub fn global() -> &'static RttiNameProvider { static G: OnceLock<…>; … }
    // accent() = theme markerCycle packed
}
impl NameProvider for RttiNameProvider {
    fn id(&self)->String{"rtti".into()} fn display_name(&self)->String{"RTTI".into()}
    fn entries(&self,_active)->Vec<NamedAddress>{ self.lock.lock().unwrap().hits.clone() } // return a COPY
    // inherited linear-scan name_for/address_for over the hits
}
impl RttiNameProvider {
    pub fn push(&self, name: &str, address: u64, module_name: &str) {
        if name.is_empty() || address == 0 { return }            // ignore empties
        let key = format!("{name}@{address:x}");
        {
            let mut g = self.lock.lock().unwrap();
            if g.by_key.contains_key(&key) { return }            // idempotent dedupe
            let idx = g.hits.len();
            g.by_key.insert(key, idx);
            g.hits.push(NamedAddress{ name: name.into(), address, kind:"rtti".into(),
                                      source: if module_name.is_empty(){String::new()}else{module_name.into()}, ..Default::default() });
        }                                                        // <-- lock released HERE
        NameRegistry::global().lock().unwrap().emit_changed();   // AFTER unlock (avoid re-entrancy)
    }
    pub fn clear(&self) { { let mut g=self.lock.lock().unwrap(); g.hits.clear(); g.by_key.clear(); } NameRegistry::…emit_changed(); }
    pub fn clear_for_module(&self, module_name: &str) {
        if module_name.is_empty() { return }
        { let mut g=self.lock.lock().unwrap();
          let mut kept=Vec::with_capacity(g.hits.len()); let mut new_key=HashMap::new();
          for h in g.hits.drain(..) {
              if h.source == module_name { continue }
              let key = format!("{}@{:x}", h.name, h.address);
              new_key.insert(key, kept.len()); kept.push(h);
          }
          g.hits = kept; g.by_key = new_key; }
        NameRegistry::…emit_changed();
    }
}
```
**Ordering invariant (preserve):** `emit_changed()` is always called AFTER releasing `self.lock`
(C++ `rtti_name_provider.cpp:38/47/66`) to avoid deadlock/re-entrancy if a listener calls back
into the provider. The dedupe key is `name + "@" + hex(addr)` (lowercase hex, no `0x`).

> Registry registration: wrap the global in `Arc<RttiNameProviderShim>` where the shim
> forwards to `RttiNameProvider::global()`, OR make `RttiNameProvider::global()` itself an
> `Arc<RttiNameProvider>` so it can be cloned into the registry. Either reproduces the C++
> no-op-deleter shared_ptr. Prefer `static G: OnceLock<Arc<RttiNameProvider>>` so `global()`
> returns `&Arc<…>` and `register_provider(rtti.clone())` works.

## 4.7 `BookmarkNameProvider` (`names/bookmark.rs`)

Constructed with a callback returning the active controller (bookmarks are per-document):
```rust
pub type ActiveCtrlFn = Box<dyn Fn() -> Option<Rc<RefCell<Controller>>> + Send + Sync>;
pub struct BookmarkNameProvider { f: ActiveCtrlFn }
```
> The exact controller handle type depends on the `controller` port (likely a gpui `Entity` or
> `Rc<RefCell<…>>`); the spec uses a placeholder. The contract: a closure that fetches the active
> tab's controller, may return None.

`id()="bookmark"`, `display_name()="Bookmarks"`, `accent()` = theme `indDataChanged` packed.

`entries(active)` (C++ `bookmark_name_provider.cpp:38`): get controller+document via `f()`; None →
empty. For each bookmark `b` in `document.tree.bookmarks`: `NamedAddress{ name: b.name,
address: evaluate_formula(&b.address_formula, active, ptr_size), kind:"bookmark", .. }`. `ptr_size
= document.tree.pointer_size`.

`supports_add()=true`; `add(name, address)` (C++ `:54`): no controller → false; formula =
`format!("0x{address:x}")`; `ctrl.add_bookmark(name, formula)`; true.

`supports_remove()=true`; `remove(name)` (C++ `:62`): find bookmark by name → `ctrl.remove_bookmark(i)`;
true; else false.

`evaluate_formula(formula, prov, ptr_size) -> u64` (static, C++ `:16`): wire `AddressParserCallbacks`:
- `resolve_module(name) -> (u64, bool)` → `prov.symbol_to_address(name)`, ok = base != 0.
- `read_pointer(addr) -> (u64, bool)` → read `ptr_size` bytes via `pread` (LE).
- `resolve_identifier(name) -> (u64, bool)` → `SymbolStore::global().lock().resolve(name, prov)`.
Call `AddressParser::evaluate(formula, if ptr_size!=0 {ptr_size} else {8}, &cbs)`; return
`result.value` on ok else 0. Bookmark addresses are **formulas re-evaluated each `entries()` call**.

This provider is the only one depending on `Controller`/`NodeTree`/`AddressParser` — keep that
dependency edge; gate or stub if `controller`/`addr` ports lag (it has no oracle test in this set).

---

# PART 5 — Compose integration (CONTEXT ONLY — compose is a separate port unit)

The RTTI hint tests (`test_rtti_hint`) live in the **compose** port unit, but they exercise this
subsystem's walkers. Document the contract the compose port must honor so `rtti-symbols` is
implemented compatibly:

- `rtti_for_vtable(state, prov, candidate)` — per-compose-pass cache: `state.rtti_cache:
  HashMap<u64, RttiInfo>` memoizes **both successes and failures**; module list
  `state.rtti_modules` fetched **once per pass** (`rtti_modules_cached` flag). Candidate must land
  inside a cached module before any walk; else cached-negative.
- Calls `walk_rtti(prov, addr, 8, 0)` first; on `!ok` → `walk_rtti_itanium(prov, addr, 8, 0)`.
  **`max_vtable_slots=0`** ⇒ no vtable enumeration (just the class name; fast — the inline chip
  only needs `demangled_name`).
- On success with non-empty `demangled_name`, fire the discovery hook → `RttiNameProvider::global()
  .push(demangled_name, vtable_address, module_name)`. Tests leave the hook unset (silent push or
  no push — the hint chip itself does not require the push).
- `modulesEnumeratedFewTimesNotPerLine`: with 32 fields → same vtable, `enumerate_modules()` must
  be O(1) (≤4 and < field_count) — driven by the per-pass module cache + the `RttiInfo` memo.
  **This pins `enumerate_modules` as a counted, override-able trait method (§1.2 note).**
- `itaniumAutoDetectFallsBack`: MSVC walker rejects an Itanium buffer (no COL sig), Itanium
  fallback produces the chip.
- `hintIndependentOfTypeHintsToggle`: RTTI chip appears even with `type_hints=false`.

---

# PART 6 — Error-handling strategy

- **Walkers** do NOT use `Result`. They mirror C++ exactly: return an `RttiInfo` with `ok=false`
  and a populated `error: String` (and `vtable_address` always echoed). The error strings are
  **load-bearing** (tests assert `.contains("signature")`, `"unreasonably"`, `"offset_to_top"`,
  `"mangle"`) — reproduce them verbatim (see §1.5/§1.6). Provider read failures become
  `ok=false`/`break`/`continue` per the step pseudocode, never a panic.
- **Demanglers** return `String` (never fail; verbatim passthrough on unrecognized input).
- **SymbolStore** methods return values/`(u64,bool)`/`Option`/counts as in C++; no `Result`.
- **Downloader** uses the `DownloadEvent::Finished{success, error}` enum, not `Result` at the API
  boundary, to mirror the Qt signal contract. Internally it may use `reqwest`/`std::io` `Result`
  and `thiserror`-typed errors, formatted into the `error` string exactly (`"Download failed: …"`,
  `"HTTP <n>"`, `"Empty response"`, `"Cannot write: …"`).
- **Locking:** `Mutex::lock().unwrap()` is acceptable (poisoning only on panic-while-held, which
  these short critical sections do not do). No lock is held across `emit_changed()` (RttiNameProvider)
  or across a Provider read (SymbolStore takes the store lock, Provider reads happen inside but do
  not re-lock the store).
- All integer arithmetic on addresses uses `wrapping_add`/`wrapping_sub` where C++ relies on
  `uint64_t`/`uint32_t` wraparound (`rtti_resolve`, displacement, `find_owning_module` bound).

---

# PART 7 — TEST PLAN

Translate the two covering C++ tests into Rust `#[test]`s. Run logic tests with
`--no-default-features` plus `--features symbols` (no `ui`/gpui). Oracle: `test_rtti` (18 pass / 1
skip) and `test_rtti_hint` (8 pass) in `_oracle/RESULTS.md`. The `smokeTestRealBinary` is
Windows/Itanium-only and SKIPS upstream — port it as `#[ignore]`/`#[cfg(windows)]` (no assertion
beyond "doesn't panic, clean error").

### Shared test helpers (port once — `tests/common/rtti_fixtures.rs` or `#[cfg(test)] mod`)
Per `tests-catalog.md §516`, the synthetic buffer is shared by test_chips/test_rtti/test_rtti_hint/
test_tutorial. Provide:
- `build_synthetic_msvc_rtti() -> Vec<u8>` — the `m_data` layout from `test_rtti.cpp:325`
  (image base `0x10000`; vtable @ +0x1000 with COL VA at vtable[-8]; TDs Foo/Bar/Baz @
  +0x1100/0x1200/0x1300 with name at TD+16; CHD @ +0x1400 (3 bases); BCA @ +0x1500; BCDs @
  +0x1600/0x1700/0x1800; COL @ +0x1900 sig=1, pSelf=+0x14=image_base). Total buffer
  `0x10000 + 0x10000`.
- `build_synthetic_itanium_rtti(mangled: &str) -> Vec<u8>` — the `ItaniumFixture` layout
  (`test_rtti.cpp:264`): image base `0x10000`; vtable @ +0x1000, offset_to_top=0 at vt-16,
  type_info VA at vt-8 = +0x1100; 5 method ptrs + null; type_info[0]=tiVt VA (+0x1200),
  type_info[8]=name VA (+0x1180); name string @ +0x1180; tiVt head = 0xFEEDFACE. Buffer
  `0x10000 + 0x10000`.
- `FakeModuleProvider` — wraps `BufferProvider`, overrides `enumerate_modules()` to return one
  module `{name, name, base, size}` AND counts invocations (`enum_calls()` / `reset_enum_count()`)
  for the compose-cache test. Itanium variant returns module `("synthetic-itanium", …, 0x10000, 0x10000)`;
  MSVC compose variant returns `("synthetic.dll", …, 0x10000, 0x10000)`.

### From `test_rtti.cpp` → `walk.rs`/`demangle.rs` tests

| C++ slot | Rust `#[test]` | Assertions (golden) |
|---|---|---|
| `demangleBasic` | `demangle_rtti_basic` | `demangle_rtti_name(".?AVFoo@@")=="Foo"`; `".?AUStruct@@"=="Struct"` |
| `demangleNested` | `demangle_rtti_nested` | `".?AVBar@Foo@@"=="Foo::Bar"`; `".?AVZ@Y@X@@"=="X::Y::Z"` |
| `demangleMalformed` | `demangle_rtti_malformed` | `"plain_name"=="plain_name"`; `""==""` (passthrough/verbatim) |
| `walkSyntheticRtti` | `walk_synthetic_msvc_rtti` | over `BufferProvider(build_synthetic_msvc_rtti(),"synthetic")`, `walk_rtti(p, 0x11000, 8, 16)`: `ok`; `vtable_address==0x11000`; `complete_locator==0x10000+0x1900`; `image_base==0x10000`; `offset==0`; `raw_name==".?AVFoo@@"`; `demangled_name=="Foo"`; `bases.len()==3` with demangled `Foo`/`Bar`/`Baz`; `vtable.len()==5`; each `vtable[i].slot==i`, `vtable[i].address==0x10000+0x100+i*0x10` |
| `walkRejectsBadSignature` | `walk_rejects_bad_signature` | corrupt COL sig (@ `0x10000+0x1900`) to `0xDEAD`; `!ok`; `error.contains("signature")` |
| `walkRejectsHugeBaseCount` | `walk_rejects_huge_base_count` | set CHD numBases (@ `0x10000+0x1400+0x08`) = 9999; `!ok`; `error.contains("unreasonably")` |
| `textReportFormat` | `walk_text_report_fields` | `ok`; `!demangled_name.is_empty()`; `vtable_address!=0`; `complete_locator!=0` |
| `msvcAbiTagged` | `walk_msvc_abi_tagged` | `ok`; `abi=="MSVC"` |
| `demangleItaniumSimple` | `demangle_itanium_simple` | `demangle_itanium_name("3Foo")=="Foo"` |
| `demangleItaniumNested` | `demangle_itanium_nested` | `"N3Bar3FooE"=="Bar::Foo"` |
| `demangleItaniumStdShorthand` | `demangle_itanium_std_shorthand` | `demangle_itanium_name("St9type_info").ends_with("type_info")` |
| `demangleItaniumPassthrough` | `demangle_itanium_passthrough` | `"plain_text"=="plain_text"` |
| `walkSyntheticItanium` | `walk_synthetic_itanium` | `ItProv(build_synthetic_itanium_rtti("3Foo"))`, `walk_rtti_itanium(p, 0x11000, 8, 64)`: `ok`; `abi=="Itanium"`; `raw_name=="3Foo"`; `demangled_name=="Foo"`; `vtable.len()==5` |
| `walkSyntheticItaniumNested` | `walk_synthetic_itanium_nested` | mangled `"N3Bar3FooE"` → `ok`; `demangled_name=="Bar::Foo"` |
| `rejectsImplausibleOffsetToTop` | `reject_implausible_offset_to_top` | overwrite offset_to_top (@ `0x10000+0x1000-16`) = `0x7FFFFFFFFFFFFFFF`; `!ok`; `error.contains("offset_to_top")` |
| `rejectsNonItaniumNameString` | `reject_non_itanium_name` | mangled `"not_a_mangle"` (starts `n`) → `!ok`; `error.contains("mangle")` |
| `smokeTestRealBinary` | `smoke_real_binary` `#[ignore]` `#[cfg(windows)]` | mirror: map `combase.dll`, probe a few offsets, assert no panic + clean error if `!ok`. Skips on non-Windows (test does nothing). |

> Note the synthetic VA `0x11000` = `kImageBase(0x10000) + 0x1000(vtableRva)`. C++ calls
> `walkRtti(prov, kImageBase + 0x1000, ...)`.

### From `test_rtti_hint.cpp` → compose port (cross-reference)

These belong to the **compose** spec but depend on `walk_rtti`/`walk_rtti_itanium` and
`enumerate_modules`. List them here so the rtti walkers are validated through compose:

| C++ slot | Rust `#[test]` (in compose tests) | Depends on |
|---|---|---|
| `hintAttachesWhenValuePointsAtVtable` | `rtti_hint_attaches` | `walk_rtti` + `FakeModuleProvider` |
| `noHintWhenValueOutsideAnyModule` | `rtti_hint_outside_module` | candidate not in module → no chip |
| `noHintForNullValue` | `rtti_hint_null_value` | value 0 → no chip |
| `modulesEnumeratedFewTimesNotPerLine` | `rtti_modules_enumerated_o1` | counted `enumerate_modules` ≤4, < 32 |
| `itaniumAutoDetectFallsBack` | `rtti_itanium_autodetect_fallback` | `walk_rtti` reject → `walk_rtti_itanium` |
| `hintIndependentOfTypeHintsToggle` | `rtti_hint_independent_of_type_hints` | compose flags |

### New Rust unit tests (no direct C++ slot, cover ported logic)

- **§TEST 1.7 `humanize_symbol_name`** (`demangle.rs`): `humanize_symbol_name("")==""`;
  `".?AVFoo@@"=="Foo"`; `".?AVUnchanged"` (no `@@`) → demangle_rtti returns verbatim, so
  `humanize`=="" (unchanged); `"_Z3Foov"` → non-empty Itanium; a plain C name `"GetProcAddress"`
  (no marker) → "" (keep raw); an MSVC `"?foo@@YAXXZ"` → non-empty via `msvc-demangler` (note:
  cross-platform improvement over C++ Windows-only; assert non-empty rather than an exact string
  to stay crate-version-robust).
- **§TEST 2 SymbolStore** (`symbol_store.rs`), no C++ test file exists for it directly (it is
  covered indirectly via compose/tutorial), so add unit tests pinning the documented behavior:
  - `resolve_alias`: `"nt"→"ntoskrnl"`, `"NTOSKRNL.EXE"→"ntoskrnl"`, unknown `"foo.dll"→"foo"`.
  - `add_module` first-wins dedupe + count + auto-alias registration (`"foo.dll"` raw → alias
    `foo`→canonical), `resolve("foo!bar")` then works.
  - `resolve` ambiguity: same symbol in two modules with a provider returning base 0 for both →
    `(_, ok=false)` (returns 0). Single-module bare symbol → resolved. Bare module-name fallback.
  - `get_symbol_for_address`: build a module with sorted RVAs, a `TestProvider` whose
    `symbol_to_address("mod")` returns a base; assert exact-hit `"mod!sym"`, displaced
    `"mod!sym+0x10"`, displacement > 0x1000 → "" (skip), addr < base → "" , no-provider → "",
    unattached module (base 0) → "".
  - `add_rtti_hits` creates an RTTI-only set when module absent.
  - `type_index_for_symbol`: `module!symbol` → typeIndex; bare/missing `!` → 0.
- **§TEST 3 downloader** (`downloader.rs`): assert URL string ==
  `https://msdl.microsoft.com/download/symbols/ntoskrnl.pdb/<GUID><age:x>/ntoskrnl.pdb` and
  `find_cached` path layout for a known `DownloadRequest`; no network call (factor URL/path
  construction into pure functions so they are unit-testable without `reqwest`).
- **§TEST 4 NameRegistry / providers** (`names/`):
  - `register_provider` idempotency by id (register twice → `providers().len()` unchanged; the
    second replaces); `name_for` registration-order first-wins (register two stub providers, one
    returning a name, assert order decides).
  - `RttiNameProvider::push` dedupe by `name@hex(addr)`; ignore empty name / address 0; `entries()`
    returns a clone; `clear_for_module` keeps only other-module hits; `emit_changed` fires a
    subscribed callback (use an `Arc<AtomicUsize>` counter to assert it ran AFTER push, exactly once).
  - `PdbNameProvider::entries` base==0 → address==0 (no false-match): load a module via
    `SymbolStore`, use a provider whose `symbol_to_address` returns 0 → all entries have
    `address==0`; with a provider returning a base → `address==base+rva`.
  - `NameProvider` default `name_for`/`address_for` linear scan (stub provider with a couple of
    entries).

> Because `SymbolStore` and `RttiNameProvider`/`NameRegistry` are **process globals**, the
> unit tests must not assume a clean global between tests (cargo runs tests in parallel threads
> sharing the global). **Decision:** make `SymbolStore`/`NameRegistry`/`RttiNameProvider` testable
> by either (a) exposing `#[cfg(test)] fn reset()` and running these tests with
> `#[serial_test::serial]` (add `serial_test` dev-dependency), or (b) constructing **local
> instances** (`SymbolStore::new()`, `NameRegistry::default()`, a fresh `RttiNameProvider`) in
> tests instead of the global, and only the integration/compose tests touch the globals. Prefer
> (b) — keep the public methods callable on a local instance (the global is just
> `OnceLock<Mutex<SymbolStore>>` wrapping the same type), so unit tests are isolated and parallel-safe.
> The walkers call `SymbolStore::global()` only for the vtable `symbol` field, which is empty in
> the synthetic tests anyway (no module attached / no PDB), so `walk_*` tests are unaffected.

---

# PART 8 — Ordered, independently-verifiable implementation steps

Each step compiles + tests green under `cargo test --no-default-features --features symbols` before
moving on.

1. **Shared prerequisites.** Confirm/add to the `Provider` trait: `enumerate_modules(&self) ->
   Vec<ModuleEntry>` (default empty) and `symbol_to_address(&self, &str) -> u64` (default 0); add
   `ModuleEntry` if missing. Add `PdbTypeInfo` to `core` (serde-derive, feature-independent). Add
   the `pread`/`read_u32`/`read_u64`/`read_i64`/`read_i32` helpers (§1.0). *Verify:* unit test
   `pread` short-read returns false; `read_u32` LE decode.
2. **`demangle.rs` — `demangle_rtti_name`.** Hand-roll per §1.3. *Verify:* `demangle_rtti_basic`,
   `_nested`, `_malformed`.
3. **`demangle.rs` — `demangle_itanium_name`.** Hand-rolled fallback first (the test-critical
   path), then the `cpp_demangle` best-effort wrapper. *Verify:* `demangle_itanium_simple`,
   `_nested`, `_std_shorthand`, `_passthrough`.
4. **`walk.rs` — `find_owning_module` + `RttiInfo`/structs + `rtti_resolve`/`read_cstring`.**
   *Verify:* `find_owning_module` over a `FakeModuleProvider` (hit / miss / empty list).
5. **`walk.rs` — `walk_rtti` (MSVC).** Implement the 14-step pseudocode (§1.5); the vtable
   `symbol` field calls `SymbolStore::global()` but that yields "" with no module attached.
   *Verify:* `walk_synthetic_msvc_rtti`, `_rejects_bad_signature`, `_rejects_huge_base_count`,
   `_text_report_fields`, `_msvc_abi_tagged`. (Needs `build_synthetic_msvc_rtti` + `FakeModuleProvider`
   from the shared helpers — build those here.)
6. **`walk.rs` — `walk_rtti_itanium`.** §1.6. *Verify:* `walk_synthetic_itanium`, `_nested`,
   `reject_implausible_offset_to_top`, `reject_non_itanium_name`. (Needs `build_synthetic_itanium_rtti`
   + `ItProv`.)
7. **`symbol_store.rs` — `PdbSymbolSet` + `SymbolStore` (local-instance API + global).** All
   methods §2. *Verify:* §TEST 2 (resolve_alias, add_module dedupe/alias, resolve qualified/bare/
   ambiguous/fallback, get_symbol_for_address binary search + displacement cap + live-attach,
   add_rtti_hits, type_index_for_symbol). Use a local `TestProvider` implementing
   `symbol_to_address`/`read`.
8. **`demangle.rs` — `humanize_symbol_name`.** §1.7 (uses `demangle_rtti_name`,
   `demangle_itanium_name`, `msvc-demangler`). *Verify:* §TEST 1.7.
9. **`names/mod.rs` — `NamedAddress`, `NameProvider` trait (defaults), `NameRegistry`.** §4.1-4.3.
   *Verify:* registry idempotency + order; default linear-scan name_for/address_for via a stub.
10. **`names/rtti.rs` — `RttiNameProvider`.** §4.6 (Mutex, dedupe, emit-after-unlock ordering).
    *Verify:* push/dedupe/clear_for_module/entries-clone/emit_changed-fires-once-after-unlock.
11. **`names/pdb.rs` — `PdbNameProvider` + `PdbTypeProvider`.** §4.4-4.5. *Verify:* entries
    base==0→address==0; base set→base+rva; name_for routes through SymbolStore + humanize;
    address_for via resolve; type provider address-less + kind mapping.
12. **`names/bookmark.rs` — `BookmarkNameProvider`.** §4.7. Depends on `controller`/`core`/`addr`;
    if those lag, stub behind the same trait and a placeholder controller handle, implement
    `evaluate_formula` wiring. *Verify:* once `addr`/`controller` exist — entries formula eval,
    add/remove round-trip (may defer to the controller port's integration tests).
13. **`downloader.rs` — `SymbolDownloader`.** §3. Factor URL/cache-path into pure functions first.
    *Verify:* §TEST 3 (URL + path strings; no network). The streaming/cancel/event plumbing is
    integration-tested manually (no oracle).
14. **Wire registration order in `main.rs`** (PdbNameProvider, PdbTypeProvider, RttiNameProvider,
    BookmarkNameProvider) and the compose discovery hook → `RttiNameProvider::global().push(...)`.
    *Verify:* the compose-side `test_rtti_hint` translations (PART 5 / §TEST table) pass in the
    compose port unit.

---

# PART 9 — Edge-case parity checklist (must all hold)

1. `demangle_rtti_name` verbatim passthrough on non-`.?A`/`?A` and on malformed (no `@@`) input.
2. RTTI segment reversal: `.?AVZ@Y@X@@` → `X::Y::Z`.
3. Itanium `St`→`std` shorthand; `N…E` nesting; length-prefix; verbatim passthrough.
4. COL signature guard (0 or 1 only) → error contains `"signature"`.
5. CHD numBaseClasses > 256 guard → error contains `"unreasonably"`.
6. Vtable terminator: stop at first null slot or non-module pointer; synthetic → `vtable.len()==5`.
7. Synthetic image-base fallback (COL+0x14) is **MSVC-only**; Itanium requires real modules.
8. Itanium offset_to_top magnitude filter (`±0x1000000`) → error contains `"offset_to_top"`.
9. Itanium name printable-ASCII filter (clear-on-nonprintable) + first-char marker
   (`digit | N S P K R`) → error contains `"mangle"` on bad input.
10. Itanium `*` vague-linkage prefix stored in `raw_name`, stripped before demangle.
11. `resolve` ambiguity: bare symbol in >1 module → 0, `ok=false`.
12. `get_symbol_for_address` 0x1000 displacement cap + live-attach (base must resolve).
13. `PdbNameProvider::entries` base==0 → address=0.
14. `humanize_symbol_name` returns "" when unchanged (callers keep raw).
15. `RttiNameProvider` dedupe by `name@hex(addr)`; ignore empty name / address 0; `emit_changed`
    AFTER unlock.
16. `NameRegistry::register_provider` idempotent by `id()` (replace in place).
17. Compose per-pass module-enumeration caching O(1) in field count (compose port; pins
    counted `enumerate_modules`).
18. `add_module` first-wins symbol dedupe + auto-alias of the raw module name.
19. MSVC walker name read = UTF-8 (`from_utf8_lossy`); Itanium name read = Latin-1 + printable filter.
20. All address math wraps (`uint64_t`/`uint32_t` semantics) via `wrapping_*`.
