# PORTING SPEC — Field formatting / line rendering (`format-render`)

Function-level porting spec to drive a faithful Rust implementation of `src/format.cpp`
(936 lines) + `src/commontypes.h` (405 lines).

- **Read-first inputs:** `_design/understand/format-render.md` (behavioral map),
  `_design/ARCHITECTURE.md` (module layout), `_design/crate_selection.md` (crates),
  `_oracle/RESULTS.md` (golden test results — `test_format` PASS 44/0/0).
- **C++ sources of truth:** `/home/loke/reclass-cpp/src/format.cpp`,
  `/home/loke/reclass-cpp/src/commontypes.h`,
  `/home/loke/reclass-cpp/src/core.h` (NodeKind/KindMeta/Node/Provider/column constants),
  `/home/loke/reclass-cpp/tests/test_format.cpp` (489 lines, 32 test functions, 44 asserts).
- **Portability:** PURE logic — no Qt widgets, no threading, no platform `#ifdef`. The only
  C++ guard (`#if defined(__SIZEOF_INT128__)`) is dropped: Rust has native `i128`/`u128`.

This subsystem is **output-exact**: the golden oracle (`test_format`) asserts byte-for-byte
strings, so every formatter must reproduce the C++ output character for character.

---

## 0. Target crate / module

- **Package:** `reclass` (single package; see ARCHITECTURE.md §2 — NOT a workspace).
- **Module:** `src/format.rs` (maps `compose`-sibling `format.cpp`). Feature: **always built**
  (no feature gate). Dep: `bytemuck` for the few float↔uint bit casts (or std
  `f32::from_bits`/`to_bits`, which is sufficient — bytemuck is optional here).
- **Public Rust path:** `crate::format::*` (mirrors C++ `rcx::fmt::*`). Keep the function
  names identical (drop the `fmt` prefix is **not** done — keep `fmt_int32` etc. as
  snake_case of the C++ names so the mapping is 1:1 and greppable).
- **`commontypes.h`** → `src/core/commontypes.rs` (a `core` submodule, NOT in `format.rs`):
  it is a declarative data table consumed by the type-chooser UI and depends only on
  `NodeKind`, not on `format`. It is documented in §12 of this spec but **lives in `core`**.
  (Rationale: ARCHITECTURE.md §3 maps `commontypes.h` → `core`. `format.rs` does not call it.)

### 0.1 Dependencies this module assumes from sibling modules (do not redefine here)

These come from `core` and `provider` modules (their own specs own the definitions):

- **`crate::core::NodeKind`** — `#[repr(u8)]` enum, exact discriminant order (§1.1).
- **`crate::core::KindMeta` + `kind_meta(k)`, `size_for_kind(k)`, `is_hex_node(k)`,
  `is_hex_preview(k)`, `is_pointer_kind(k)`, `is_func_ptr(k)`, `is_valid_primitive_ptr_target(k)`**
  — the metadata table & helpers (§1.2). `format.rs` only **reads** these.
- **`crate::core::Node`** — struct fields read by format: `kind`, `name: String`,
  `comment: String`, `struct_type_name: String`, `class_keyword: String`, `array_len: i32`,
  `element_kind: NodeKind`, `str_len: i32` (default 64), `ptr_depth: i32` (0..2),
  `big_endian: bool`; methods `resolved_class_keyword() -> &str` (keyword or `"struct"`),
  `is_enum() -> bool` (keyword == "enum"). (§1.3)
- **`crate::provider::Provider`** trait + its lenient read helpers (§1.4). format.rs takes
  `&dyn Provider` (or `&P: Provider`) and only **reads**.
- **`crate::core::column` constants:** `K_TREE_INDENT=2`, `K_COL_TYPE=14`, `K_COL_NAME=22`,
  `K_COL_VALUE=96`, `K_COL_COMMENT=28`, `K_SEP_WIDTH=1`.
- **`crate::addr::validate(&str) -> Option<String>`** (or `Result`) — `validate_base_address`
  delegates here (the `addr` module / `addressparser` port; out of scope, just called).

If `core`/`provider` are not yet implemented when `format.rs` is ported, define the minimal
shims behind the same public paths and let the `core` workflow replace them — but the SIGNATURES
below are the contract.

---

## 1. Shared vocabulary pulled from `core` (read-only here)

### 1.1 `NodeKind` (C++ `core.h:24-34`)
`#[repr(u8)]`, discriminant order is **load-bearing** (range checks `Int8..=UInt128`,
`Hex8..=Hex128`, `Hex16..=Hex128` depend on it):
```
Hex8, Hex16, Hex32, Hex64, Hex128,
Int8, Int16, Int32, Int64, Int128,
UInt8, UInt16, UInt32, UInt64, UInt128,
Float16, Float, Double, Bool,
Pointer32, Pointer64,
FuncPtr32, FuncPtr64,
Vec2, Vec3, Vec4, Mat4x4,
UTF8, UTF16,
Struct, Array
```

### 1.2 `KindMeta` columns used by format (`core.h:54-97`)
`{kind, name, type_name, size, lines, align, flags}`. format uses `type_name`, `size`, `lines`,
and the `is_*` helpers. Exact `type_name` strings (LOWERCASE; ints get `_t`):
`hex8 hex16 hex32 hex64 hex128 int8_t int16_t int32_t int64_t int128_t uint8_t uint16_t
uint32_t uint64_t uint128_t float16 float double bool ptr32 ptr64 fnptr32 fnptr64 vec2 vec3
vec4 mat4x4 str wstr struct array`. Sizes: Hex8=1…Hex128=16, Int128/UInt128=16, Float16=2,
Float=4, Double=8, Bool=1, Pointer32/FuncPtr32=4, Pointer64/FuncPtr64=8, Vec2=8, Vec3=12,
Vec4=16, Mat4x4=64, UTF8=1, UTF16=2, Struct/Array=0. `lines`: all 1 except Mat4x4=4.
`is_valid_primitive_ptr_target`: false for hex/pointer/funcptr/Struct/Array, true otherwise
(tested at `test_format.cpp:476-484`).

### 1.3 `Node` fields read by format
See §0.1. `name`/`comment`/`struct_type_name`/`class_keyword` are `String` (UTF-8 in Rust;
C++ `QString` is UTF-16). **Width caveat:** C++ `.size()`/`.left(n)` count UTF-16 code units;
Rust `String::len()` is bytes and `.chars().count()` is scalar values. For column fitting
(§3.2) we must count **char units**, not bytes — see §3.2 note. For ASCII type names this is
moot (all three counts coincide); it only matters for user `name`/`comment` with non-ASCII.

### 1.4 `Provider` trait — read contract used here (owned by `provider` module)
The C++ pure-virtual is `bool read(uint64_t addr, void* buf, int len) const` + `int size()`,
with **non-virtual lenient helpers**. The Rust trait (per ARCHITECTURE.md §7 / provider spec)
exposes the equivalents. **What `format.rs` requires** (only these):

| C++ helper | Semantics format relies on | Rust expectation |
|---|---|---|
| `read(addr, buf, len) -> bool` | core read; format never calls directly | `read(&self, addr, &mut [u8]) -> bool` (or `Result<usize>` mapped to bool) |
| `size() -> int` | total byte length (`i32` in C++; can be 0) | `size(&self) -> i64`/`usize` |
| `isReadable(addr, len)` | `len<0`→false; `len==0`→true; else `addr <= size && len <= size-addr` (unsigned) | provided default method, **must match exactly** |
| `readU8/U16/U32/U64(addr)` | `readAs<T>` — **ignores read's bool**; on failure returns `T::default()` (zeros) | lenient helper returning `0` on failure |
| `readF32/F64(addr)` | `readAs<f32/f64>` (native LE bit-reinterpret of zeroed buffer on failure) | lenient helper |
| `readBytes(addr, len) -> Vec<u8>` | `len<=0`→empty; else alloc `len`; if `read` fails, **fill with 0** | lenient helper returning `Vec<u8>` of length `len` |

> **CONTRACT (parity-critical):** the "lenient read returns zeros" behavior is required for
> byte-for-byte output parity (unreadable memory renders all-dots / `0x0` rows, never errors).
> If the provider trait's primary `read` returns `Result`, format must call the **lenient
> zero-filling helpers** (`read_u*`, `read_bytes`), never `?`-propagate.

`BufferProvider` (provider module, `buffer.rs`) is the concrete impl the tests use: wraps
`Vec<u8>`, bounds-checked `read`/`write`, inherits `isReadable`/`readBytes`. The Rust
constructor must accept a byte buffer (mirror `BufferProvider(QByteArray data, name={})`).

---

## 2. C++ → Rust ITEM-BY-ITEM mapping

Legend for "crate": **std** = standard library only; **core/provider/addr** = sibling module.
All functions are free functions in `crate::format` unless noted. `QString`→`String`,
`QByteArray`→`Vec<u8>`, `bool* ok`→ return `Option<Vec<u8>>` or `(Vec<u8>, bool)` (see §6).

### 2.1 File-local helpers (private `fn`, not `pub`)

| C++ (line) | Rust signature | Behavior / crate |
|---|---|---|
| `halfToFloat(uint16_t)` (13) | `fn half_to_float(h: u16) -> f32` | bit-exact binary16→32; std (`f32::from_bits`). §4.1 |
| `floatToHalf(float)` (34) | `fn float_to_half(f: f32) -> u16` | bit-exact binary32→16 RNE; std (`f32::to_bits`). §4.1 |
| `fit(QString,int)` (67) | `fn fit(s: &str, w: i32) -> String` | column fit+ellipsis+pad; std. §3.2 |
| `fitOverflow(const QString&,int)` (77) | `fn fit_overflow(s: &str, w: i32) -> String` | pad-or-overflow; std. §3.2 |
| `hexVal(uint64_t)` (125) | `fn hex_val(v: u64) -> String` | `format!("0x{:x}", v)`; std. §3.3 |
| `rawHex(uint64_t,int)` (129) | `fn raw_hex(v: u64, digits: usize) -> String` | `format!("{:0width$x}", v, width=digits)`; std. §3.3 |
| `fmtUInt128Impl(u128)` (160) | `fn fmt_u128(v: u128) -> String` | `v.to_string()` (matches LSB-build loop). std |
| `isAsciiPrintable(u8)` (308) | `fn is_ascii_printable(c: u8) -> bool` | `(0x20..=0x7E).contains(&c)`; std |
| `sanitizeString(QString)` (311) | `fn sanitize_string(s: &str) -> String` | escape control chars; std. §7.1 |
| `bytesToAscii(QByteArray,int)` (325) | `fn bytes_to_ascii(b: &[u8], slot: usize) -> String` | printable-or-`.`; std. §7.1 |
| `bytesToHex(QByteArray,int)` (337) | `fn bytes_to_hex(b: &[u8], slot: usize) -> String` | UPPERCASE space-joined; std. §7.1 |
| `fmtAsciiAndBytes(...)` (349) | `fn fmt_ascii_and_bytes(prov, addr, size_bytes, slot_bytes) -> String` | helper; not on hot path. §7.1 |
| `toBytes<T>(T)` (567) | use `v.to_le_bytes()` inline | std; host-LE memcpy |
| `stripHex(QString)` (574) | `fn strip_hex(s: &str) -> &str` | drop case-insensitive `0x` prefix; std |
| `parseHexBytes(QString,int,ok)` (597) | `fn parse_hex_bytes(s: &str, expected: usize) -> Option<Vec<u8>>` | spaced/unspaced hex parse; std. §6.1 |
| `parseIntChecked<T,P>(P,ok)` (625) | generic helper or per-kind inline range check | std. §6.2 |
| `readValueImpl(...,ValueMode)` (362) | `fn read_value_impl(node, prov, addr, sub_line, mode: ValueMode) -> String` | the value engine. §5 |

### 2.2 Public type-name functions (`pub fn`)

| C++ (line) | Rust signature | Behavior |
|---|---|---|
| `TypeNameFn` alias (`core.h:1473`) | `pub type TypeNameFn = fn(NodeKind) -> String;` | override seam fn-ptr |
| `setTypeNameProvider(fn)` (89) | `pub fn set_type_name_provider(f: Option<TypeNameFn>)` | install override; global. §3.1 |
| `typeNameRaw(NodeKind)` (92) | `pub fn type_name_raw(kind: NodeKind) -> String` | override else `kind_meta(k).type_name` else `"???"`; **unpadded** |
| `typeName(NodeKind,int=14)` (98) | `pub fn type_name(kind: NodeKind, col_type: i32) -> String` | `fit(type_name_raw_or_override, col_type)` — width-exact. Test: `type_name(Float)` trimmed=="float", len==14 |
| `arrayTypeName(NodeKind,int,&QString)` (105) | `pub fn array_type_name(elem: NodeKind, count: i32, struct_name: &str) -> String` | `"uint32_t[16]"` / `"Material[2]"`. §3.4 |
| `pointerTypeName(NodeKind,&QString)` (117) | `pub fn pointer_type_name(_kind: NodeKind, target: &str) -> String` | `(target or "void") + "*"`; kind ignored |

> **Default-arg note:** C++ has `int colType = kColType (14)` defaults. Rust has no default
> args — provide a base `type_name(kind, col_type)` plus, where the C++ relies on the default
> at call sites, a thin `type_name_default(kind)` = `type_name(kind, K_COL_TYPE)`. The test
> calls `fmt::typeName(NodeKind::Float)` (default 14) so expose a no-col convenience too.

### 2.3 Public scalar value formatters (`pub fn`)

| C++ (line) | Rust signature | Behavior |
|---|---|---|
| `fmtInt8/16/32/64` (133) | `pub fn fmt_int8(v: i8) -> String` … `fmt_int64(v: i64) -> String` | signed decimal `v.to_string()`. Test: `fmt_int32(-42)=="-42"`, `(0)=="0"` |
| `fmtUInt8/16/32/64` (137) | `pub fn fmt_uint8(v: u8) -> String` … `fmt_uint64(v: u64) -> String` | `hex_val(v as u64)` → `0x…` lowercase |
| `fmtFloat16(uint16_t)` (142) | `pub fn fmt_float16(bits: u16) -> String` | half→f32; NaN/`infh`/`-infh`/`%g.4 + "h"`. §3.5 |
| `fmtFloat(float)` (184) | `pub fn fmt_float(v: f32) -> String` | fixed 7-char body. §3.5 — **heavily tested** |
| `fmtDouble(double)` (209) | `pub fn fmt_double(v: f64) -> String` | NaN/`inf`/`-inf`/`%g.6` + force `.0`. §3.5 |
| `fmtBool(uint8_t)` (217) | `pub fn fmt_bool(v: u8) -> String` | `if v != 0 {"true"} else {"false"}` |
| `fmtPointer32/64` (219) | `pub fn fmt_pointer32(v: u32) -> String` / `fmt_pointer64(v: u64) -> String` | `0` → `"nullptr"`, else `hex_val` |
| `fmtInt128(const void*)` (170) | `pub fn fmt_int128(le_bytes: &[u8; 16]) -> String` | read `i128::from_le_bytes`; `.to_string()` |
| `fmtUInt128(const void*)` (179) | `pub fn fmt_uint128(le_bytes: &[u8; 16]) -> String` | `u128::from_le_bytes`; `.to_string()` |

> `fmtInt128`/`fmtUInt128` take a raw 16-byte pointer in C++ (caller already byte-ordered).
> In Rust take `&[u8; 16]` (or `&[u8]` with a debug_assert len>=16) and use
> `i128::from_le_bytes` / `u128::from_le_bytes`. `(rcx_uint128)(-(v+1))+1` two's-complement
> trick is unnecessary — `i128::to_string()` handles negatives correctly.

### 2.4 Public layout / line functions (`pub fn`)

| C++ (line) | Rust signature |
|---|---|
| `indent(int)` (224) | `pub fn indent(depth: i32) -> String` |
| `fmtOffsetMargin(u64,bool,int=8)` (230) | `pub fn fmt_offset_margin(off: u64, is_continuation: bool, hex_digits: usize) -> String` |
| `structTypeName(const Node&)` (238) | `pub fn struct_type_name(node: &Node) -> String` |
| `fmtStructHeader(node,depth,collapsed,colType=14,colName=22,compact=false)` (248) | `pub fn fmt_struct_header(node: &Node, depth: i32, collapsed: bool, col_type: i32, col_name: i32, compact: bool) -> String` |
| `fmtStructFooter(node,depth,totalSize=-1)` (261) | `pub fn fmt_struct_footer(node: &Node, depth: i32, total_size: i64) -> String` |
| `fmtArrayHeader(node,depth,viewIdx,collapsed,colType,colName,elemStructName,compact)` (276) | `pub fn fmt_array_header(node: &Node, depth: i32, _view_idx: i32, collapsed: bool, col_type: i32, col_name: i32, elem_struct_name: &str, compact: bool) -> String` |
| `fmtPointerHeader(node,depth,collapsed,prov,addr,ptrTypeName,colType,colName,compact)` (286) | `pub fn fmt_pointer_header(node: &Node, depth: i32, collapsed: bool, prov: &dyn Provider, addr: u64, ptr_type_name: &str, col_type: i32, col_name: i32, compact: bool) -> String` |
| `fmtNodeLine(node,prov,addr,depth,subLine=0,comment={},colType,colName,typeOverride={},compact=false)` (512) | `pub fn fmt_node_line(node: &Node, prov: &dyn Provider, addr: u64, depth: i32, sub_line: i32, comment: &str, col_type: i32, col_name: i32, type_override: &str, compact: bool) -> String` |
| `readValue(node,prov,addr,subLine)` (505) | `pub fn read_value(node: &Node, prov: &dyn Provider, addr: u64, sub_line: i32) -> String` |
| `editableValue(node,prov,addr,subLine)` (560) | `pub fn editable_value(node: &Node, prov: &dyn Provider, addr: u64, sub_line: i32) -> String` |
| `fmtEnumMember(name,value,depth,nameW)` (907) | `pub fn fmt_enum_member(name: &str, value: i64, depth: i32, name_w: i32) -> String` |
| `fmtBitfieldMember(name,bitWidth,value,depth,nameW)` (929) | `pub fn fmt_bitfield_member(name: &str, bit_width: u8, value: u64, depth: i32, name_w: i32) -> String` |
| `extractBits(prov,addr,containerKind,bitOffset,bitWidth)` (914) | `pub fn extract_bits(prov: &dyn Provider, addr: u64, container_kind: NodeKind, bit_offset: u8, bit_width: u8) -> u64` |

### 2.5 Public parse / validate functions (`pub fn`)

| C++ (line) | Rust signature |
|---|---|
| `parseValue(NodeKind,text,ok)` (638) | `pub fn parse_value_kind(kind: NodeKind, text: &str) -> Option<Vec<u8>>` |
| `parseValue(Node&,text,ok)` (825) | `pub fn parse_value(node: &Node, text: &str) -> Option<Vec<u8>>` |
| `parseAsciiValue(text,expectedSize,ok)` (581) | `pub fn parse_ascii_value(text: &str, expected_size: usize) -> Option<Vec<u8>>` |
| `validateValue(NodeKind,text)` (845) | `pub fn validate_value(kind: NodeKind, text: &str) -> String` (empty = valid) |
| `validateBaseAddress(text)` (900) | `pub fn validate_base_address(text: &str) -> String` (empty = valid) |

> **`ok` parameter mapping (decision):** C++ uses `bool* ok` out-params and returns an empty
> `QByteArray` on failure. In Rust the idiomatic, parity-preserving mapping is
> **`Option<Vec<u8>>`**: `None` ⇔ `ok=false`, `Some(bytes)` ⇔ `ok=true` (`Some(vec![])` for the
> "empty string into UTF8/UTF16" success case — note the empty-but-OK case at `:644`). This
> distinguishes "valid empty" from "invalid" cleanly, which the C++ `(empty bytes, ok=true)`
> case requires. Do NOT collapse to `Result<Vec<u8>, _>` unless a typed error is wanted; the
> C++ has no error payload (just the bool), so `Option` is the closest fit.

---

## 3. Exact data structures, constants, helper algorithms

### 3.1 The type-name override seam (global mutable)
C++: `static TypeNameFn g_typeNameFn = nullptr;` (a plain function pointer, set once).
Rust: a thread-safe global. **Decision:** `static TYPE_NAME_FN: AtomicPtr<()>` storing a
`fn(NodeKind) -> String` as a raw pointer, OR simpler `static TYPE_NAME_FN: OnceLock<TypeNameFn>`
if the app sets it exactly once at startup. The tests **never** set it, so the default path
(`kind_meta`) is what the oracle exercises.
- Recommended: `OnceLock<TypeNameFn>` + a `set_type_name_provider` that `set`s it (ignoring
  re-set, matching "install once" usage). `set_type_name_provider(None)` is a no-op clearing
  intent — but since C++ allows re-assigning `g_typeNameFn`, prefer an `RwLock<Option<TypeNameFn>>`
  for exact "can be re-installed" parity. Either is acceptable; document the choice.
- `serde`: not applicable (function pointer; not serialized).

### 3.2 `fit` / `fit_overflow` (column fitting) — `format.cpp:67-82`
```
fn fit(s, w):
    if w <= 0: return ""              # empty
    let n = char_count(s)             # UTF-16-unit count in C++; see note
    if n > w:
        if w >= 2: s = first (w-1) chars of s + '\u{2026}'   # ellipsis …
        else:      s = first w chars of s
    return left_justify(s, w, ' ')    # pad RIGHT with spaces to exactly w chars

fn fit_overflow(s, w):
    if w <= 0: return ""
    if char_count(s) <= w: return left_justify(s, w, ' ')
    return s.to_string()              # overflow: full string, no pad, no truncate
```
- **Result of `fit` is ALWAYS exactly `w` chars** when `w>0` (invariant the tests rely on:
  `type_name(Float).len()==14`).
- Ellipsis is `'\u{2026}'` (…), U+2026, only when `w >= 2`.
- **UTF-16 vs char-count note:** C++ `QString::size()`/`.left(n)`/`leftJustified(n)` operate on
  **UTF-16 code units**. Type names are pure ASCII so char-count == UTF-16-unit count == byte
  count; for arbitrary user `name`/`comment` containing non-BMP chars these diverge. **Decision:**
  count by **`char` (Unicode scalar)** for `left(n)`/justify, accepting that non-BMP (surrogate-
  pair) strings could differ from Qt by 1 unit. The oracle only uses ASCII, so this is parity-
  safe for tests; flag as a known edge for full fidelity. Provide a `char_left(s, n)` helper
  (take first `n` chars) and a `pad_right(s, w)` helper (append spaces until char-count == `w`).
- `left_justify(s, w, ' ')`: if `char_count(s) < w` append `w - char_count(s)` spaces; if `>= w`
  return `s` unchanged (Qt `leftJustified` never truncates).

### 3.3 Hex helpers — `format.cpp:125-131`
- `hex_val(v)` = `format!("0x{:x}", v)` → lowercase, no leading zeros (matches `asprintf("0x%llx")`).
- `raw_hex(v, digits)` = `format!("{:0width$x}", v, width = digits)` → lowercase, zero-padded to
  `digits`, **no `0x`**, **NOT truncated** if value needs more than `digits` (Qt `rightJustified`
  only pads). Match: when `v` exceeds `digits` hex chars, output is wider than `digits`.

### 3.4 `array_type_name` / `pointer_type_name`
```
fn array_type_name(elem, count, struct_name):
    let elem_s = if elem == Struct && !struct_name.is_empty() { struct_name }
                 else { kind_meta(elem).map(type_name).unwrap_or("???") }
    format!("{}[{}]", elem_s, count)     # count is i32 decimal

fn pointer_type_name(_kind, target):
    let t = if target.is_empty() { "void" } else { target };
    format!("{}*", t)
```

### 3.5 Float formatters (parity-critical)

**`half_to_float(h: u16) -> f32`** (`:13-32`) — port bit-for-bit:
```
s = (h & 0x8000) as u32 << 16
e = (h >> 10) & 0x1f          (u32)
m = h & 0x3ff                 (u32)
if e == 0:
    if m == 0: b = s
    else:                      # subnormal renormalize
        while m & 0x400 == 0 { m <<= 1; e = e.wrapping_sub(1); }
        e += 1; m &= 0x3ff
        b = s | ((e + 112) << 23) | (m << 13)
elif e == 0x1f: b = s | 0x7f800000 | (m << 13)    # inf/NaN
else:           b = s | ((e + 112) << 23) | (m << 13)
return f32::from_bits(b)
```
**`float_to_half(f: f32) -> u16`** (`:34-57`) — port bit-for-bit (note `e` is `i32`):
```
b = f.to_bits()
s = (b >> 16) & 0x8000
e = ((b >> 23) & 0xff) as i32 - 127
m = b & 0x7fffff
if e == 128:  return (s | 0x7c00 | (if m != 0 {0x200} else {0})) as u16   # inf/NaN
if e > 15:    return (s | 0x7c00) as u16                                   # overflow→inf
if e < -14:
    if e < -24: return s as u16                                           # underflow→0
    m |= 0x800000
    shift = (-e - 14 + 13) as u32
    hm = m >> shift
    if (m >> (shift - 1)) & 1 != 0 { hm += 1 }                            # round nearest
    return (s | hm) as u16
hm = m >> 13
if (m >> 12) & 1 != 0:                                                    # round nearest even
    hm += 1
    if hm == 0x400 { hm = 0; e += 1; if e > 15 { return (s | 0x7c00) as u16 } }
return (s | (((e + 15) as u32) << 10) | hm) as u16
```
> The `half` crate exists but **rounding must match these exact algorithms**. Decision: **direct
> port** (do NOT use `half`). All arithmetic in `u32`/`i32`. `wrapping_sub` for the subnormal
> `e--` to mirror C++ unsigned wrap (only matters in the subnormal loop which exits before
> underflowing in practice; use wrapping to be safe).

**`fmt_float(v: f32) -> String`** (`:184-208`) — the most-tested function:
```
if v.is_nan(): return "NaN"
if v.is_infinite(): return if v > 0 {"inff"} else {"-inff"}
if v == 0.0 && v.is_sign_negative(): return "-0.000f"      # negative zero special-case
let av = v.abs()
if av >= 100000.0: return if v < 0 {"-99999+f"} else {"99999+f"}
for dec in (0..=4).rev():                                   # 4,3,2,1,0
    let mut body = format!("{:.*}", dec as usize, av)       # fixed-point, `dec` decimals
    body.push_str(if dec == 0 {".f"} else {"f"})
    if char_count(&body) == 7 {
        if v < 0.0 { body.insert(0, '-') }
        return body
    }
return if v < 0 {"-99999+f"} else {"99999+f"}              # rounding pushed past 99999
```
**Golden vectors (must match byte-for-byte, `testFmtFloat` lines 21-64):**
`3.14159→"3.1416f"`, `-3.14159→"-3.1416f"`, `0→"0.0000f"`, `0.02→"0.0200f"`,
`-0.069→"-0.0690f"`, `15.6543→"15.654f"`, `-77.6624→"-77.662f"`, `500→"500.00f"`,
`5000→"5000.0f"`, `50000→"50000.f"`, `100000→"99999+f"`, `-100000→"-99999+f"`,
`inf→"inff"`, `-inf→"-inff"`, `NaN→"NaN"`, `1→"1.0000f"`, `-1→"-1.0000f"`,
`1e-7→"0.0000f"` (size≤9, contains `'f'`), `-0.0f` starts with `'-'`.
> **Rounding caveat:** C++ `QString::number(av,'f',dec)` and Rust `format!("{:.*}",dec,av)` both
> round half-to-even via the platform's shortest-correct formatting. They should agree for these
> vectors; **validate the test vectors during implementation**. The 7-char search loop is the
> load-bearing invariant — keep it intact.

**`fmt_double(v: f64) -> String`** (`:209-216`):
```
if v.is_nan(): return "NaN"
if v.is_infinite(): return if v > 0 {"inf"} else {"-inf"}
let s = fmt_g(v, 6)                       # %g with 6 significant digits, trailing-zero stripped
if !s.contains('.') && !s.contains('e') && !s.contains('E'): s += ".0"
return s
```
**`fmt_float16(bits) -> String`** (`:142-149`):
```
let f = half_to_float(bits)
if f.is_nan(): return "NaN"
if f.is_infinite(): return if f > 0 {"infh"} else {"-infh"}
format!("{}h", fmt_g(f as f64, 4))        # %g with 4 sig digits, then 'h'
```
> **`%g` emulation (`fmt_g`):** Rust has no `'g'`. C `printf %.<N>g` = use `%e` if the decimal
> exponent < -4 or >= N, else `%f`, with the given **significant-digit** precision, then strip
> trailing zeros (and a trailing `.`). Implement a small `fmt_g(v: f64, sig: u32) -> String`:
> 1. Handle `0.0` → `"0"`. 2. Compute decimal exponent. 3. If `exp < -4 || exp >= sig`: format
> `%.{sig-1}e` then strip trailing zeros in mantissa, lowercase `e`, keep sign of exponent
> (C `%g` uses at least 2 exponent digits, e.g. `1e+08`). 4. Else format `%.{prec}f` where
> `prec = sig - 1 - exp`, then strip trailing zeros and trailing `.`. The `test_format`
> assertions for `fmt_double`/`fmt_float16` are **loose** (contains `.`/`e`, non-empty), so exact
> digit parity is less critical here than for `fmt_float`, but reproduce `%g` faithfully — the
> compose oracle (`eprocess_*.txt`) and generator may exercise tighter cases.
> Alternative: vendor a tiny `%g` via the `ryu`/`dragon` shortest + recompose, but the
> straightforward `%e`/`%f` + strip is simplest and matches glibc/Qt for these cases.

---

## 4. `read_value_impl` — the value engine (`format.cpp:362-503`)

`enum ValueMode { Display, Editable }`. `let be = node.big_endian; let display = mode == Display;`
Local readers swap on big-endian (use `u16/u32/u64::swap_bytes()` to mirror `qbswap`):
```
rU16(a) = { let v = prov.read_u16(a); if be { v.swap_bytes() } else { v } }
rU32(a) = … u32 …;  rU64(a) = … u64 …
rF32(a) = { let mut v = prov.read_u32(a); if be { v = v.swap_bytes() } f32::from_bits(v) }
rF64(a) = { let mut v = prov.read_u64(a); if be { v = v.swap_bytes() } f64::from_bits(v) }
```
Per `node.kind` (exact behavior, `display` vs `editable`):
- **Hex8/16/32/64:** display → `hex_val(read)`; editable → `raw_hex(read, 2/4/8/16)`. Hex8 uses
  `prov.read_u8(addr)` (no swap); Hex16/32/64 use `rU16/rU32/rU64`.
- **Hex128** (`:376-401`): `let mut b = prov.read_bytes(addr, 16); if b.len() < 16 { b.resize(16,0) }`.
  - editable: `let mut show = b.clone(); if be { show.reverse() }`; emit 16 space-joined
    `raw_hex(byte, 2)` → `"HH HH … HH"` (lowercase? **NO** — `raw_hex` is lowercase, but the test
    only checks `.contains(' ')` and `len>=47`). **Match C++ exactly:** `raw_hex` lowercase pairs,
    space-separated. (`bytesToHex` uppercase is a *different* helper used for hex-preview rows.)
  - display: `if be { b.reverse() }`; `lo = u64::from_le_bytes(b[0..8])`, `hi = u64::from_le_bytes(b[8..16])`;
    if `hi == 0` → `hex_val(lo)`; else `format!("0x{:X}{:016X}", hi, lo)` (uppercase, lo zero-padded
    to 16). NB: this branch uses **uppercase** (`QString::number(...).toUpper()`).
- **Int8/16/32/64:** `fmt_int8((rU.. as iN))`. Int8 = `prov.read_u8(addr) as i8`.
- **Int128/UInt128** (`:406-412`): `let mut b = read_bytes(addr,16); resize16; if be {b.reverse()}`;
  `fmt_int128(&b[..16].try_into())` / `fmt_uint128(...)`.
- **UInt8/16/32/64:** `fmt_uintN(rU..)` (UInt8 = `prov.read_u8(addr)`).
- **Float16:** `let s = fmt_float16(rU16(addr)); if display {s} else {s.trim().to_string()}`.
- **Float:** `fmt_float(rF32(addr))`; editable trims.
- **Double:** `fmt_double(rF64(addr))`; editable trims.
- **Bool:** `fmt_bool(prov.read_u8(addr))`.
- **Pointer32 / FuncPtr32:** `let val = prov.read_u32(addr)`; editable → `raw_hex(val,8)`;
  display → `fmt_pointer32(val)`. (No endian swap — matches C++; pointers read native.)
- **FuncPtr64:** `let val = prov.read_u64(addr)`; editable → `raw_hex(val,16)`; display → `fmt_pointer64(val)`.
- **Pointer64** (`:431-452`) — primitive-pointer dereference:
  ```
  let val = prov.read_u64(addr)
  if node.ptr_depth > 0 && is_valid_primitive_ptr_target(node.element_kind) && val != 0:
      let mut target = val
      for d in 1..node.ptr_depth:                         # follow ptr_depth-1 extra hops
          if target == 0 { break }
          target = if prov.is_readable(target, 8) { prov.read_u64(target) } else { 0 }
      if target != 0 && prov.is_readable(target, size_for_kind(node.element_kind)):
          let tmp = Node { kind: node.element_kind, str_len: node.str_len, ..Default };
          let deref = read_value_impl(&tmp, prov, target, 0, mode)
          return if display { format!("-> {}", deref) } else { deref }
      # else fall through:
      return if display { fmt_pointer64(val) } else { raw_hex(val, 16) }
  return if display { fmt_pointer64(val) } else { raw_hex(val, 16) }
  ```
  **Recursion note:** `read_value_impl` recurses into a temp Node — keep it a free fn that can
  recurse. The temp Node has only `kind` + `str_len` set; all other fields default (so `big_endian`
  defaults to false for the deref — matches C++ `Node tmp;`). The loop `for d in 1..ptr_depth`
  mirrors C++ `for (d=1; d<ptrDepth && target!=0; …)`.
- **Vec2/Vec3/Vec4** (`:463-471`): `count = size_for_kind(kind)/4` (2/3/4); join
  `fmt_float(prov.read_f32(addr + i*4))` with `", "`. **No endian handling** — always native
  LE `read_f32`. Test: comma counts 1/2/3.
- **Mat4x4** (`:472-482`): editable → `""` (empty). Display: if `sub_line < 0 || sub_line >= 4`
  → `"?"`; else `format!("row{} [", sub_line)` + 4 floats joined `", "` + `"]"`, each
  `fmt_float(prov.read_f32(addr + (sub_line*4 + c)*4))`.
- **UTF8** (`:483-490`): `let mut bytes = prov.read_bytes(addr, node.str_len)`; truncate at first
  `0` byte (`if let Some(i)=bytes.iter().position(|&b| b==0) { bytes.truncate(i) }`);
  `let s = String::from_utf8_lossy(&bytes)`; display → `format!("\"{}\"", sanitize_string(&s))`;
  editable → `s.to_string()`.
  > **`QString::fromUtf8` vs `from_utf8_lossy`:** Qt replaces invalid UTF-8 with U+FFFD; Rust
  > `from_utf8_lossy` also yields U+FFFD. Use `from_utf8_lossy` for parity on malformed input.
- **UTF16** (`:491-499`): `let bytes = prov.read_bytes(addr, node.str_len*2)`; decode as UTF-16LE:
  build `u16` units from LE byte pairs, `String::from_utf16_lossy(&units)`; truncate at first
  U+0000 (`if let Some(i)=s.find('\0') { s.truncate(i) }`); display → `format!("L\"{}\"", sanitize_string(&s))`;
  editable → `s`. (C++ does `fromUtf16` over `size/2` units then truncates at first NUL char.)
- **default** (Struct/Array): `""`.

`read_value` = `read_value_impl(.., Display)`; `editable_value` = `read_value_impl(.., Editable)`.

---

## 5. `fmt_node_line` — full row text (`format.cpp:512-556`)

```
ind = indent(depth)
raw_type = if type_override.is_empty() { type_name_raw(node.kind) } else { type_override.to_string() }
overflow = compact && char_count(&raw_type) > col_type
type_s = if overflow { fit_overflow(&raw_type, col_type) }
         else if type_override.is_empty() { type_name(node.kind, col_type) }
         else { fit(type_override, col_type) }
name_s = fit(&node.name, col_name)
effective_col_type = if overflow { char_count(&raw_type) as i32 } else { col_type }
prefix_w = effective_col_type + col_name + 2 * K_SEP_WIDTH       # continuation indent width
cmt_suffix = if comment.is_empty() { String::new() } else { fit(comment, K_COL_COMMENT) }   # COL_COMMENT=28

if node.kind == Mat4x4:
    val = read_value(node, prov, addr, sub_line)
    if sub_line == 0: return ind + &type_s + SEP + &name_s + SEP + &val + &cmt_suffix
    else:             return ind + &" ".repeat(prefix_w) + &val + &cmt_suffix      # continuation row

if is_hex_preview(node.kind):
    sz = size_for_kind(node.kind)
    b = if prov.is_readable(addr, sz) { prov.read_bytes(addr, sz) } else { vec![0u8; sz] }
    ascii = pad_right(&bytes_to_ascii(&b, sz), col_name)              # leftJustified(colName)
    hex   = pad_right(&bytes_to_hex(&b, sz),   max(23, sz*3 - 1))     # leftJustified(max(23, sz*3-1))
    return ind + &type_s + SEP + &ascii + SEP + &hex + &cmt_suffix

# default
val = if overflow { read_value(node, prov, addr, sub_line) }
      else { fit(&read_value(node, prov, addr, sub_line), K_COL_VALUE) }   # COL_VALUE=96
return ind + &type_s + SEP + &name_s + SEP + &val + &cmt_suffix
```
- `SEP = " "` (single space). `indent(d)` = `" ".repeat((d * K_TREE_INDENT) as usize)` = `2*d` spaces.
- For hex-preview rows: **ASCII occupies the name column** (padded to `col_name`), **hex occupies
  the value column** (padded to at least 23 chars). `bytes_to_ascii`/`bytes_to_hex` slot = `sz`.
- **Default-arg note:** C++ defaults `sub_line=0, comment={}, colType=14, colName=22,
  typeOverride={}, compact=false`. Provide a convenience `fmt_node_line_basic(node, prov, addr, depth)`
  delegating with those defaults, plus the full-arg fn. compose.cpp (out of scope, sibling) is
  the real caller and passes all args.

### 5.1 Layout / struct / array / pointer headers (`format.cpp:248-304`)

**`struct_type_name(node)`** = `if !node.struct_type_name.is_empty() { clone } else { node.resolved_class_keyword().to_string() }`.

**`fmt_struct_header`** (`:248`):
```
ind = indent(depth); raw_type = struct_type_name(node); suffix = if collapsed {""} else {"{"}
if node.name.is_empty():                                  # anonymous: no column padding
    return ind + &raw_type + SEP + suffix                 # e.g. "union {"
type_s = if compact { fit_overflow(&raw_type, col_type) } else { fit(&raw_type, col_type) }
return ind + &type_s + SEP + &node.name + SEP + suffix     # NAME NOT fitted (raw)
```

**`fmt_struct_footer`** (`:261`):
```
footer = indent(depth) + "};"
footer += if node.is_enum() { "  +1 +10 Top" } else { "  +1 +10h +100h +1000h Trim Top" }
if total_size > 0:
    footer += &format!("  // 0x{:X} ({})", total_size, total_size)    # uppercase hex + decimal
return footer
```
> Test `testFmtStructFooterSimple` calls with `total_size=0x14` and asserts the result contains
> `"};"` and does **NOT** contain `"sizeof"`. The `// 0x14 (20)` comment IS appended (0x14>0) —
> the test only checks the literal `"sizeof"` is absent (it uses `// 0xNN (dec)` form instead).

**`fmt_array_header`** (`:276`): `raw_type = array_type_name(node.element_kind, node.array_len, elem_struct_name)`;
`type_s = compact ? fit_overflow : fit`; `suffix = collapsed?"":"{"`;
`ind + type_s + SEP + node.name + SEP + suffix` (name NOT fitted).

**`fmt_pointer_header`** (`:286`) — merged pointer+struct header:
```
ind = indent(depth)
overflow = compact && char_count(ptr_type_name) > col_type
type_s = if compact { fit_overflow(ptr_type_name, col_type) } else { fit(ptr_type_name, col_type) }
if collapsed:
    if overflow:
        return ind + &type_s + SEP + &node.name + SEP + &read_value(node, prov, addr, 0)
    name_s = fit(&node.name, col_name)
    val    = fit(&read_value(node, prov, addr, 0), K_COL_VALUE)
    return ind + &type_s + SEP + &name_s + SEP + &val          # show pointer value, not brace
return ind + &type_s + SEP + &node.name + SEP + "{"            # expanded
```

### 5.2 Offset margin / indent (`format.cpp:224-234`)
```
fn indent(depth) = " ".repeat((depth * K_TREE_INDENT) as usize)        # 2*depth spaces
fn fmt_offset_margin(off, is_continuation, hex_digits):
    if is_continuation: return "  \u{00B7} ".to_string()               # "  · " (U+00B7 middle dot)
    format!("{:0w$X} ", off, w = hex_digits)                           # UPPERCASE zero-padded + trailing space
```
Test vectors: `(0x10,false,8)→"00000010 "`, `(0,false,8)→"00000000 "`, `(0x10,true,_)→"  · "`,
`(0xFFFFF80012345678,false,16)→"FFFFF80012345678 "`, `(0x10,false,16)→"0000000000000010 "`,
`(0x10,false,4)→"0010 "`. Note C++ `fmtOffsetMargin(0x10, false)` uses default `hexDigits=8`;
provide a 2-arg convenience defaulting to 8.

### 5.3 Enum / bitfield member + extract_bits (`format.cpp:907-934`)
```
fn fmt_enum_member(name, value: i64, depth, name_w):
    indent(depth) + &pad_right(name, name_w) + " = " + &value.to_string()
fn fmt_bitfield_member(name, bit_width: u8, value: u64, depth, name_w):
    indent(depth) + &pad_right(name, name_w) + &format!(" : {} = {}", bit_width, value)
fn extract_bits(prov, addr, container_kind, bit_offset: u8, bit_width: u8) -> u64:
    let container = match container_kind {
        Hex8  => prov.read_u8(addr) as u64,
        Hex16 => prov.read_u16(addr) as u64,
        Hex32 => prov.read_u32(addr) as u64,
        _     => prov.read_u64(addr),
    };
    debug_assert!(bit_offset as u32 + bit_width as u32 <= 64);     # Q_ASSERT
    if bit_width >= 64 { container >> bit_offset }
    else { (container >> bit_offset) & ((1u64 << bit_width) - 1) }
```
> `pad_right` here = Qt `leftJustified(nameW)` (pad with spaces to width; no truncation). No
> ellipsis — `leftJustified` only pads, so a name longer than `name_w` is returned whole.
> `extract_bits` does **no** endian handling (reads container natively). `bit_offset + bit_width`
> sum done in `u32` to avoid `u8` overflow before the assert.

### 7.1 Byte preview helpers (`format.cpp:308-356`)
```
fn is_ascii_printable(c: u8) = (0x20..=0x7E).contains(&c)
fn sanitize_string(s):                                   # escapes for display
    for c in s.chars():
        '\n' => "\\n", '\r' => "\\r", '\t' => "\\t", '\\' => "\\\\",
        c if (c as u32) < 0x20 => format!("\\x{:x}", c as u32),   # lowercase hex of codepoint
        c => c
fn bytes_to_ascii(b, slot):                              # `slot` chars
    (0..slot).map(|i| { let c = *b.get(i).unwrap_or(&0); if is_ascii_printable(c) { c as char } else { '.' } }).collect()
fn bytes_to_hex(b, slot):                                # UPPERCASE, space-joined, no trailing space
    (0..slot).map(|i| format!("{:02X}", b.get(i).unwrap_or(&0))).collect::<Vec<_>>().join(" ")
    # output length = slot*3 - 1
fn fmt_ascii_and_bytes(prov, addr, size_bytes, slot_bytes):
    let slot = max(slot_bytes, size_bytes)
    let b = if prov.is_readable(addr, slot) { prov.read_bytes(addr, slot) } else { vec![0u8; slot] }
    bytes_to_ascii(&b, slot) + "  " + &bytes_to_hex(&b, slot)
```
- `kHexDigits` uppercase table → `format!("{:02X}", ..)` (uppercase, exactly 2 digits per byte).
- `sanitize_string` uses **lowercase** `\x` hex (`QString::number(c.unicode(), 16)`), and `\\`
  escapes a single backslash → `"\\\\"` in Rust source = two-char output `\\`.

---

## 6. Parse & validate (`format.cpp:567-905`)

### 6.1 Static parse helpers
- `strip_hex(s) -> &str`: if `s` starts with `"0x"`/`"0X"` (case-insensitive) return `&s[2..]` else `s`.
- `parse_ascii_value(text, expected_size) -> Option<Vec<u8>>` (**pub**, `:581`): require
  `text.chars().count() == expected_size`; each char's scalar value must be `<= 255` (else `None`);
  one byte per char. **Note:** C++ indexes `text[i]` as UTF-16 units and checks `.unicode() > 255`.
  For parity count by `char` and check `c as u32 > 255`; ASCII inputs identical.
- `parse_hex_bytes(s, expected: usize) -> Option<Vec<u8>>` (`:597`): `let t = s.trim()`.
  - if `t.contains(' ')`: `let parts: Vec<_> = t.split_whitespace().collect()` (`SkipEmptyParts`);
    require `parts.len() == expected`; each part must be exactly 2 chars and parse base-16
    (`u8::from_str_radix(part, 16)`); collect into `Vec<u8>`.
  - else: require `t.chars().count() == expected*2`; parse 2 chars at a time (`from_str_radix(&t[i*2..i*2+2],16)`).
  - any failure → `None`.
  > C++ uses `split(' ', SkipEmptyParts)`. `split_whitespace()` also splits on tabs — but the
  > C++ only ever feeds spaces; to match exactly use `t.split(' ').filter(|p| !p.is_empty())`.
- `parse_int_checked::<T>(val) -> Option<Vec<u8>>` (`:625`): range-check `val` against `T::MIN..=T::MAX`
  (signed) or `<= T::MAX` (unsigned); on fit return `Some((val as T).to_le_bytes().to_vec())` else `None`.
  Implement per-kind inline (Rust generics over int types need a helper trait; simplest is a macro
  or explicit per-kind code mirroring the C++ template instantiations).

### 6.2 `parse_value_kind(kind, text) -> Option<Vec<u8>>` (`:638`)
```
let s = text.trim()
if s.is_empty():
    return if kind == UTF8 || kind == UTF16 { Some(vec![]) } else { None }   # only strings ok-empty
```
`parse_hex_unified(cleaned, byte_count)` lambda (`:656`):
```
if cleaned.contains(' '): return parse_hex_bytes(cleaned, byte_count)        # spaced: exact groups
if cleaned.chars().count() > byte_count*2: return None                       # too many digits
let cleaned = left-zero-pad to byte_count*2 chars                            # "0" repeated
parse_hex_bytes(&cleaned, byte_count)
```
Per kind:
- **Hex8/16/32/64/128:** `parse_hex_unified(strip_hex(s), 1/2/4/8/16)`. Bytes in **memory/display
  order** (no endian swap here). `"DEADBEEF"`→`[DE,AD,BE,EF]`; `"0xDEADBEEF"` same; `"AB CD"`→`[AB,CD]`;
  16-group spaced for Hex128; `"1FF"` into Hex8 fails (3>2); 8-group into Hex128 fails.
- **Int8/Int16:** if `0x`-prefixed → parse `strip_hex(s)` as `u32` base16; fail if `> 0xFF`/`> 0xFFFF`;
  cast to `i8`/`i16` (two's complement: `0xFF`→-1, `0x80`→-128). Else decimal `i32::from_str_radix(s,10)`
  (use `s.parse::<i32>()`), then `parse_int_checked::<i8/i16>(val)`.
- **Int32:** hex → parse as `u64` base16, fail if `> 0xFFFFFFFF`, cast `i32`. Decimal → `s.parse::<i32>()`
  → `to_le_bytes`. (C++ `toInt` here returns directly; no extra range check beyond i32 parse.)
- **Int64:** hex → `u64` base16 cast `i64`; decimal → `s.parse::<i64>()`.
- **UInt8/16/32:** base 16 if `0x` else 10; parse to `u64`/`u32`; `parse_int_checked::<uN>`.
- **UInt64:** base 16/10; parse `u64`; `to_le_bytes`.
- **Int128/UInt128** (`:719`): decimal (optional leading `-` for signed) or `0x…` hex; **manual
  digit accumulation** into `u128` with overflow detect (`next < acc` → `None`). Signed range
  `[-(2^127), 2^127-1]` via `sign_bit = 1u128 << 127`: if `neg` require `acc <= sign_bit` then
  `acc = acc.wrapping_neg()`; else require `acc < sign_bit`. Unsigned: reject negative. Write 16
  LE bytes (`acc.to_le_bytes()`).
- **Float16:** strip trailing `'h'`/`'H'` then `'f'`/`'F'`; replace `','`→`'.'`; `parse::<f32>()`;
  on ok `float_to_half(val).to_le_bytes()`.
- **Float:** strip trailing `'f'`/`'F'`; `,`→`.`; `parse::<f32>()`; `to_le_bytes`.
- **Double:** `,`→`.`; `parse::<f64>()`; `to_le_bytes`.
- **Bool:** `"true"`/`"1"`→`Some(vec![1])`; `"false"`/`"0"`→`Some(vec![0])`; else `None`.
- **Pointer32/FuncPtr32:** `strip_hex(s)` parse `u32` base16 → `to_le_bytes`.
- **Pointer64/FuncPtr64:** `strip_hex(s)` parse `u64` base16 → `to_le_bytes`.
- **UTF8** (`:802`): always ok; if `s` starts and ends with `"` strip both; return `s.as_bytes().to_vec()`
  (UTF-8). Test `"\"hello\""`→`b"hello"`.
- **UTF16** (`:808`): always ok; strip leading `L"` or `"`, trailing `"`; encode remaining `s` as
  UTF-16 **LE** bytes: `s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()`.
- **default** (Struct/Array/Vec/Mat): `None` (empty).

> **`QString::toInt/toUInt` parsing nuance:** Qt's `toInt(&ok, base)` rejects trailing garbage and
> respects sign. Rust `str::parse` / `from_str_radix` likewise reject garbage. For the hex `toUInt`
> paths, `strip_hex` removes the `0x`, then `u32::from_str_radix(rest, 16)` — but note Qt `toUInt(base=16)`
> on the **un-stripped** decimal-mode UInt paths (`UInt8` etc.) is called on `strip_hex(s)` regardless,
> so always strip first then `from_str_radix`. Empty `strip_hex` (e.g. just `"0x"`) → parse error → `None`.

### 6.3 `parse_value(node, text) -> Option<Vec<u8>>` (`:825`)
```
let out = parse_value_kind(node.kind, text)?;          # None propagates
if !node.big_endian || out.is_empty() { return Some(out) }
match node.kind {
    Int16|UInt16|Hex16 | Int32|UInt32|Hex32 | Int64|UInt64|Hex64 |
    Int128|UInt128|Hex128 | Float16|Float|Double => { let mut o = out; o.reverse(); Some(o) }
    _ => Some(out),   # Int8/UInt8/Hex8/Bool/pointers/strings/vec/mat NOT swapped
}
```

### 6.4 `validate_value(kind, text) -> String` (`:845`) — empty string = valid
```
let s = text.trim(); if s.is_empty() { return String::new() }     # empty is valid
let is_hex_kind = is_hex_node(kind) || matches!(kind, Pointer32|Pointer64|FuncPtr32|FuncPtr64)
let is_int_kind = kind >= Int8 && kind <= UInt128                  # repr(u8) ordering
if is_hex_kind || is_int_kind:
    let has_hex_prefix = s starts_with "0x" (case-insensitive)
    let digits = if has_hex_prefix { &s[2..] } else { s }
    if has_hex_prefix || is_hex_kind:                             # hex mode
        let is_multibyte_hex = kind >= Hex16 && kind <= Hex128
        for c in digits.chars():
            if c == ' ' && is_multibyte_hex { continue }
            if !(c.is_ascii_digit() || ('a'..='f').contains(&c) || ('A'..='F').contains(&c)):
                return format!("invalid hex '{}'", c)
    else:                                                          # decimal mode
        let is_signed = kind >= Int8 && kind <= Int128
        let mut chars = digits.chars(); let mut started = false
        # leading '-' allowed only for signed
        (iterate; skip index 0 if is_signed && digits starts with '-')
        for c in remaining: if !c.is_ascii_digit() { return format!("invalid '{}'", c) }
# then real parse for range checking
match parse_value_kind(kind, text) {
    Some(_) => String::new(),                                      # valid
    None => {
        if kind == Float || kind == Double { return "invalid number".into() }
        if let Some(m) = kind_meta(kind) { if m.size > 0 && m.size <= 8 {
            let max_val = if m.size == 8 { u64::MAX } else { (1u64 << (m.size*8)) - 1 };
            return format!("too large! max=0x{:0w$x}", max_val, w = (m.size*2) as usize);
        }}
        "invalid".into()
    }
}
```
> Error strings: `"invalid hex '%1'"`, `"invalid '%1'"`, `"invalid number"`, `"too large! max=0x%1"`,
> `"invalid"`. `%1` for the hex/decimal char is a single char; `max` is zero-padded to `size*2`
> lowercase hex digits (Qt `arg(maxVal, size*2, 16, '0')` is lowercase). Tests assert only
> *non-empty* on failure, but reproduce exact text for parity. `is_int_kind`/`is_signed`/
> `is_multibyte_hex` comparisons rely on the `repr(u8)` discriminant order — compare via
> `(kind as u8)` ranges or derive `PartialOrd` on `NodeKind`.

### 6.5 `validate_base_address(text) -> String` (`:900`)
```
let s = text.trim(); if s.is_empty() { return "empty".into() }
crate::addr::validate(s)        # delegates to AddressParser port (addr module)
```
> Returns the address-parser's error string (empty = valid). The `addr` module owns
> `AddressParser::validate`; this fn just trims + empty-checks + delegates.

---

## 7. Error-handling strategy

- **No exceptions, no panics on bad input.** Parse failures → `None` (was `bool* ok = false`).
  Reads through `Provider` use the **lenient zero-filling** helpers (`read_u*`, `read_bytes`),
  so out-of-bounds reads yield zeroed buffers and render stable rows (`0x0`, all-dots), never error.
- `extract_bits` uses `debug_assert!` (mirrors `Q_ASSERT`) — release builds do not abort; the
  shift logic is defined for `bit_width >= 64` (returns `container >> bit_offset`).
- Float parsing of malformed text → `None`; range overflow on ints → `None`.
- `from_utf8_lossy` / `from_utf16_lossy` never panic (replace invalid sequences with U+FFFD),
  matching Qt's `fromUtf8`/`fromUtf16`.
- The global type-name override is the only mutable state; guard with `OnceLock`/`RwLock`
  (see §3.1). No threading inside the module.

---

## 8. Idiomatic-Rust deviations (documented for reviewers)

1. `bool* ok` out-param → `Option<Vec<u8>>` (`None`=fail, `Some(vec![])`=valid-empty).
2. `const void* data` for 128-bit → `&[u8; 16]` + `from_le_bytes`.
3. C++ default args → explicit full-arg fns + thin `*_basic`/`*_default` convenience wrappers
   where the tests / sibling callers use defaults (`type_name(Float)`, `fmt_offset_margin(off,false)`,
   `fmt_node_line(node,prov,addr,depth)`).
4. `qbswap(v)` → `v.swap_bytes()`. `memcpy` float↔uint → `f32::from_bits`/`to_bits`.
5. `QString` UTF-16 length → `char`-count (ASCII-safe; non-BMP edge documented in §3.2).
6. `#if __SIZEOF_INT128__` guard dropped (native `i128`/`u128`).

---

## 9. `commontypes.h` → `core::commontypes` (lives in core, documented here for completeness)

Declarative table; **no dependency on format**. Port as `&'static` data.
```rust
pub struct CommonField {
    pub offset: i32,
    pub kind: NodeKind,
    pub name: &'static str,
    pub ptr_target: Option<&'static str>,   // None / "" = void*
}
pub struct CommonType {
    pub name: &'static str,
    pub category: &'static str,
    pub class_keyword: &'static str,         // "struct"/"union"/"class"
    pub total_size: i32,
    pub fields: &'static [CommonField],
}
pub static COMMON_TYPES: &[CommonType] = &[ /* ~48 entries */ ];
pub fn find_common_type(name: &str) -> Option<&'static CommonType>  // linear scan by exact name
```
~48 entries across categories **Windows NT** (`_M128A`, `UNICODE_STRING`, `LIST_ENTRY`,
`LARGE_INTEGER`(union), `OBJECT_ATTRIBUTES`, `CLIENT_ID`, `IO_STATUS_BLOCK`, `GUID`, `FILETIME`,
`FILETIME_u64`(union), `RTL_BALANCED_NODE`, `SINGLE_LIST_ENTRY`, `STRING`, `DISPATCHER_HEADER`),
**Time** (`UnixTime32`, `UnixTime64`), **C++ STL** (`std::string`/`std::wstring`, `std::vector`,
`std::shared_ptr`, `std::unique_ptr`, `std::function`, `std::map_node`, `std::unordered_map`),
**Unreal** (`FString`, `FName`, `TArray`(=FString fields), `FVector`, `FRotator`, `FTransform`,
`FQuat`, `FLinearColor`), **Generic** (`VTable8`, `RefCounted`, `LinkedNode`, `TreeNode`,
`SlabEntry`, `Delegate`, `Variant`, `Slice`, `FatPointer`, `TimeStamp`), **Math** (`RGBA8`,
`AABB`, `Matrix4x4`(single Mat4x4 field), `Sphere`, `Ray`, `Plane`).
Preserve exactly: `UNICODE_STRING.Buffer` = `Pointer64` ptr_target `"UTF16"`;
`OBJECT_ATTRIBUTES.ObjectName` → ptr_target `"UNICODE_STRING"`; `std::map_node._Color` comment
"0=red,1=black"; `FTransform` = three `Vec4`s. Copy the full table verbatim from
`commontypes.h:28-403`. `findCommonType` is a linear scan by exact name → `Option`.
> **This is `core`'s work, not `format`'s** — listed so the format workflow does not duplicate it.
> `test_format.cpp` does not test commontypes (it is covered indirectly by core/type-chooser tests).

---

## 10. TEST PLAN

All `test_format.cpp` (32 functions, 44 asserts; oracle PASS 44/0/0) → Rust `#[test]`s in
`src/format.rs` (`#[cfg(test)] mod tests`) using `crate::provider::BufferProvider`. Run with
`cargo test --no-default-features` (logic-only, no gpui). Each Rust test asserts the SAME
literal strings/bytes the C++ asserts (the oracle is `test_format` — output-exact for the
asserted vectors; loose-contains where C++ is loose).

| # | C++ test (line) | Rust `#[test]` | Assertions to reproduce |
|---|---|---|---|
| 1 | `testTypeName` (9) | `type_name_float_padded` | `type_name(Float, 14).trim() == "float"`; `char_count == 14` |
| 2 | `testFmtInt32` (15) | `fmt_int32_decimal` | `fmt_int32(-42)=="-42"`, `fmt_int32(0)=="0"` |
| 3 | `testFmtFloat` (21) | `fmt_float_vectors` | all 17 vectors in §3.5 byte-for-byte |
| 4 | `testFmtBool` (66) | `fmt_bool_basic` | `fmt_bool(1)=="true"`, `fmt_bool(0)=="false"` |
| 5 | `testFmtPointer64_null` (71) | `fmt_pointer64_null` | `fmt_pointer64(0)=="nullptr"` |
| 6 | `testFmtPointer64_nonNull` (75) | `fmt_pointer64_nonnull` | `fmt_pointer64(0x400000)` starts_with "0x", contains "400000" |
| 7 | `testFmtOffsetMargin_primary` (81) | `offset_margin_primary` | `(0x10,false,8)=="00000010 "`, `(0,false,8)=="00000000 "` |
| 8 | `testFmtOffsetMargin_continuation` (86) | `offset_margin_continuation` | `(0x10,true,_)=="  \u{00B7} "` |
| 9 | `testFmtOffsetMargin_kernelAddr` (90) | `offset_margin_widths` | `(0xFFFFF80012345678,false,16)=="FFFFF80012345678 "`; `(0x10,false,16)=="0000000000000010 "`; `(0x10,false,4)=="0010 "` |
| 10 | `testFmtStructHeader` (99) | `struct_header_expand_collapse` | expanded contains struct/Test/`{`; collapsed contains struct/Test, NOT `{` |
| 11 | `testFmtStructFooter` (116) | `struct_footer_basic` | `fmt_struct_footer(n,0,-1)` contains "};" |
| 12 | `testIndent` (125) | `indent_spaces` | `indent(0)==""`, `indent(1)=="  "`, `indent(3)=="      "` |
| 13 | `testParseValueInt32` (131) | `parse_int32_neg` | `parse_value_kind(Int32,"-42")==Some([4 bytes])`; `i32::from_le_bytes==-42` |
| 14 | `testParseValueFloat` (141) | `parse_float_pi` | `parse_value_kind(Float,"3.14")` → f32 within 0.01 of 3.14 |
| 15 | `testParseValueHex32` (151) | `parse_hex32_memory_order` | `"DEADBEEF"`→`[DE,AD,BE,EF]` |
| 16 | `testParseValueBool` (165) | `parse_bool` | `"true"`→`[1]`, `"false"`→`[0]`, `"banana"`→None |
| 17 | `testParseValueHex0xPrefix` (181) | `parse_hex_0x_and_ptr` | `Hex32 "0xDEADBEEF"`→`[DE..EF]`; `Pointer64 "0x0000000000400000"`→u64 0x400000 |
| 18 | `testParseValueOverflow` (198) | `parse_overflow` | UInt8 "300"→None; "255"→Some(255); Int8 "200"→None; "-129"→None; "-128"→Some(-128); UInt16 "70000"→None; Hex8 "1FF"→None; Hex16 "1FFFF"→None |
| 19 | `testSignedHexRoundTrip` (237) | `parse_signed_hex` | Int8 "0xFF"→-1; "0x80"→-128; Int16 "0xFFFF"→-1; Int32 "0xFFFFFFFF"→-1; Int8 "0x1FF"→None; Int16 "0x1FFFF"→None |
| 20 | `testReadValueBoundsCheck` (275) | `read_value_vec_commas` | BufferProvider(16 zeros): Vec2 read_value contains ","; Vec3 has 2 commas; Vec4 has 3 commas |
| 21 | `testEditableValueBasic` (293) | `editable_value_float_vec` | write 3.14f at 0; `editable_value(Float)` contains "3.14"; Vec2 editable contains "," |
| 22 | `testParseValueEmptyString` (312) | `parse_empty` | UTF8 ""→Some(empty); Int32 ""→None |
| 23 | `testFmtStructFooterSimple` (324) | `struct_footer_no_sizeof` | `fmt_struct_footer(n,0,0x14)` contains "};", NOT "sizeof" |
| 24 | `testFmtFloatEdgeCases` (334) | `fmt_float_edges` | NaN→"NaN"; inf→"inff"; -inf→"-inff"; `fmt_float(3.14)` contains 'f'; `fmt_float(-0.0)` starts_with '-' |
| 25 | `testFmtDoubleIntegerValue` (344) | `fmt_double_integer_has_dot` | `fmt_double(42.0)` contains '.' |
| 26 | `testFmtBoolValues` (350) | (merge into #4) | same as #4 |
| 27 | `testValidateValueEmpty` (355) | `validate_empty_valid` | `validate_value(Int32,"")` is empty |
| 28 | `testValidateValueHexOverflow` (360) | `validate_int8_overflow` | `validate_value(Int8,"999")` non-empty |
| 29 | `testParseValueBoolStrings` (366) | (merge into #16) | same as #16 |
| 30 | `testParseValueHex128` (377) | `parse_hex128_16groups` | 16-group spaced → 16 bytes; `[0]==0x00`, `[15]==0xFF` |
| 31 | `testParseValueHex128TooShort` (388) | `parse_hex128_too_short` | 8-group spaced → None |
| 32 | `testReadValueHex128` (396) | `read_value_hex128` | data[0]=0x41,data[15]=0xFF: display non-empty; editable contains ' ' and `char_count>=47` |
| 33 | `testFmtFloatVerySmall` (413) | `fmt_float_tiny` | `fmt_float(1e-7)` contains 'f', `char_count<=9` (== "0.0000f") |
| 34 | `testFmtDoubleVeryLarge` (420) | `fmt_double_1e308` | non-empty; contains '.'|'e'|'E' |
| 35 | `testFmtDoubleNegativeZero` (426) | `fmt_double_neg_zero` | non-empty |
| 36 | `testFmtDoubleNanInf` (432) | `fmt_double_nan_inf` | `fmt_double(NaN)` non-empty; `fmt_double(inf)` non-empty |
| 37 | `testParseValueUtf8Emoji` (439) | `parse_utf8_quoted` | `UTF8 "\"hello\""` → `b"hello"` |
| 38 | `testParseValueHex16SpaceSeparated` (446) | `parse_hex16_spaced` | `"AB CD"`→`[AB,CD]` |
| 39 | `testValidateValueHex128` (455) | `validate_hex128_valid` | 16-group form → empty (valid) |
| 40 | `testKindFromTypeNameUnknown` (461) | `kind_from_type_name_unknown` | **core** test — `kind_from_type_name("nonsense")` → (Hex8, ok=false). Put in core's tests, NOT format. |
| 41 | `testAllTypeNamesForUI` (468) | `all_type_names_for_ui` | **core** test — `all_type_names_for_ui().len() == kind_meta count`, no dups. core's tests. |
| 42 | `testIsValidPrimitivePtrTarget` (476) | `is_valid_primitive_ptr_target` | **core** test — Hex8/Pointer64/Struct/FuncPtr64→false; Int32/Float/Bool→true. core's tests (helper lives in core). |

**Notes for the test author:**
- #3 (`fmt_float_vectors`) is the highest-value parity check; assert each of the 17 vectors
  with `assert_eq!`. If any diverges, the `{:.*}` rounding differs from Qt — fix the formatter,
  not the test.
- #32/#9/#7/#8 assert UPPERCASE hex and the U+00B7 middle dot / U+2026 ellipsis — use the exact
  Unicode escapes (`"  \u{00B7} "`).
- #40–#42 exercise helpers that live in **`core`** (`kind_from_type_name`, `all_type_names_for_ui`,
  `is_valid_primitive_ptr_target`), not `format`. They are in `test_format.cpp` because they share
  the `using namespace rcx` test file, but in the Rust split they belong to `core`'s test module.
  Document this so neither workflow drops them.
- Construct `Node` via the `core` builder/`Default`; set only the fields each test needs
  (`kind`, `name`, etc.). `BufferProvider::new(vec![0u8;16])` for the read tests.
- There is **no separate golden text file** for `test_format` (unlike `test_compose`'s
  `eprocess_*.txt`); the golden values are the literal `QCOMPARE`/`QVERIFY` expectations embedded
  in `_oracle/test_sources/test_format.cpp` (= `tests/test_format.cpp`). Cite those line numbers
  in test comments.

---

## 11. ORDERED, INDEPENDENTLY-VERIFIABLE IMPLEMENTATION STEPS

Each step ends compiling (`cargo build --no-default-features`) and, where tests exist, passing
the listed `#[test]`s. Steps assume `core::{NodeKind, KindMeta+helpers, Node, column consts}` and
`provider::{Provider, BufferProvider}` exist (shim them minimally if not — the contracts in §0.1/§1).

1. **Module skeleton + constants.** Create `src/format.rs`, declare `mod format` in `lib.rs`,
   alias column consts (`COL_TYPE/NAME/VALUE` from core, local `COL_COMMENT=28`, `SEP=" "`).
   Add `fit`, `fit_overflow`, `hex_val`, `raw_hex`, `indent`, `char_left`/`pad_right` helpers.
   *Verify:* unit tests for `fit("float",14).len()==14`, `indent(3)=="      "` (#12), and
   `fit_overflow` overflow path.
2. **Type-name functions + override seam.** `set_type_name_provider`, `type_name_raw`, `type_name`,
   `array_type_name`, `pointer_type_name`. *Verify:* #1 (`testTypeName`).
3. **Scalar int/bool/pointer formatters.** `fmt_int8..64`, `fmt_uint8..64`, `fmt_bool`,
   `fmt_pointer32/64`. *Verify:* #2 (`fmt_int32`), #4 (`fmt_bool`), #5/#6 (`fmt_pointer64`).
4. **Float formatters.** `half_to_float`, `float_to_half` (bit-exact), `fmt_float`, `fmt_g`,
   `fmt_double`, `fmt_float16`. *Verify:* #3 (all 17 `fmt_float` vectors — the big one), #24,
   #25, #33, #34, #35, #36.
5. **128-bit formatters.** `fmt_u128`, `fmt_int128`, `fmt_uint128`. *Verify:* covered indirectly
   by step 9's Hex128/Int128 read tests (#32) once `read_value_impl` lands.
6. **Offset margin / indent / struct-name / headers / footer / enum / bitfield / extract_bits.**
   `fmt_offset_margin`, `struct_type_name`, `fmt_struct_header`, `fmt_struct_footer`,
   `fmt_array_header`, `fmt_pointer_header` (needs `read_value` from step 7 — implement header
   shape now, wire value after step 7), `fmt_enum_member`, `fmt_bitfield_member`, `extract_bits`.
   *Verify:* #7, #8, #9, #10, #11, #12, #23.
7. **Byte-preview helpers + `read_value_impl` + `read_value`/`editable_value`.** `is_ascii_printable`,
   `sanitize_string`, `bytes_to_ascii`, `bytes_to_hex`, `fmt_ascii_and_bytes`; then the full
   `read_value_impl` switch (all kinds incl. Pointer64 deref recursion, Vec, Mat4x4, UTF8/16,
   Hex128). *Verify:* #20 (Vec commas), #21 (editable float/vec), #32 (Hex128 read/editable);
   wire `fmt_pointer_header`'s value from step 6.
8. **`fmt_node_line`.** Mat4x4 continuation rows, hex-preview rows, default rows. *Verify:* no
   direct `test_format` test asserts `fmt_node_line` output, but it is exercised by the
   `test_compose` oracle later; add a smoke `#[test]` that a hex8 row and an int32 row produce
   the expected `ind + type + SEP + name + SEP + value` shape and column widths.
9. **Parse helpers + `parse_value_kind`.** `strip_hex`, `parse_ascii_value`, `parse_hex_bytes`,
   `parse_int_checked`, then `parse_value_kind` (all kinds incl. 128-bit manual accumulation,
   float suffix stripping, UTF8/16 quote stripping, hex memory-order). *Verify:* #13, #14, #15,
   #16, #17, #18, #19, #22, #30, #31, #37, #38.
10. **`parse_value(node)` endian wrapper.** Reverse for scalar endian-bearing kinds. *Verify:*
    a new `#[test]` round-tripping a `big_endian` Int32 (display via `read_value` then
    `parse_value` reverse) — not in the C++ suite but guards the swap table; assert the swap set
    matches §6.3 exactly.
11. **`validate_value` + `validate_base_address`.** Char-set checks, range messages, addr
    delegation. *Verify:* #27, #28, #39. (`validate_base_address` needs `addr::validate`; if
    the `addr` module is absent, gate that one assertion or stub `addr::validate` to return
    empty for now and note the dependency.)
12. **Cross-check & cleanup.** Run the full `format` test module; confirm 39 format-owned tests
    pass (the 3 core-owned ones #40–#42 are filed under `core`). Grep for any `unwrap()` on
    provider reads (must use lenient helpers). Confirm `cargo build` green on Linux with and
    without default features.

---

## 12. PARITY CHECKLIST (must all hold — from understand-map §12, verified against source)

1. `fit` returns exactly `w` chars (w>0); ellipsis U+2026 only when `w>=2`. `type_name(Float).len()==14`.
2. `fmt_float` 7-char-body search + special cases (`-0.000f`, `99999+f`, `inff`/`-inff`/`NaN`) — all 17 vectors.
3. Unsigned ints → hex (`0x…` lowercase); signed → decimal.
4. `fmt_pointer*` null→`"nullptr"`, else lowercase `0x…`.
5. `fmt_offset_margin` UPPERCASE zero-padded hex + trailing space; continuation `"  · "` (U+00B7); `hex_digits` width.
6. `fmt_struct_footer` never contains literal `"sizeof"` (uses `// 0xNN (dec)` uppercase-hex form).
7. `indent(d)` = `2*d` spaces.
8. Parse hex is memory/display order (`Hex32 "DEADBEEF"`→`[DE,AD,BE,EF]`); unspaced `0x` left-zero-pads to width.
9. Signed hex parse two's-complement + range-checked (`Int8 0xFF`→-1, `0x80`→-128, `0x1FF`→fail).
10. Range checks: UInt8 300 / Int8 200 / Int8 -129 / UInt16 70000 / Hex8 1FF / Hex16 1FFFF fail; UInt8 255 / Int8 -128 ok.
11. Bool: `true`/`1`→1, `false`/`0`→0, else fail.
12. Empty string: UTF8/UTF16→ok+empty; all others→fail.
13. Hex128 editable = 16 space-separated byte pairs (len≥47); spaced parse requires exactly 16 groups.
14. Vec2/3/4 single-line comma counts 1/2/3; subLine ignored.
15. `editable_value(Float)` contains "3.14" (trimmed `fmt_float` body, e.g. "3.1400f").
16. `is_valid_primitive_ptr_target`: hex/ptr/funcptr/Struct/Array→false; ints/float/bool→true (gates `-> value` deref).
17. `validate_value` error strings reproduced (`invalid hex '%1'`, `invalid '%1'`, `invalid number`, `too large! max=0x%1`, `invalid`).
18. Big-endian: display swaps via `swap_bytes`/reverse; `parse_value(node)` reverses for scalar endian-bearing kinds; Int8/UInt8/Hex8/Bool/pointers/strings/vec/mat NOT swapped.
19. Hex128 display: `hi==0`→`hex_val(lo)`, else `0x` + UPPERCASE hi + UPPERCASE lo zero-padded to 16.
20. Mat4x4: editable empty; display rows `row{n} [f, f, f, f]`; out-of-range subLine→`"?"`; `fmt_node_line` continuation rows blank-prefixed (width = effectiveColType+colName+2).
21. Lenient reads: unreadable memory → zero buffer (never error/panic).
