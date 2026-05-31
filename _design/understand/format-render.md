# Subsystem: Field formatting / line rendering (`format-render`)

**C++ sources:** `src/format.cpp` (936 lines), `src/commontypes.h` (405 lines)
**Declarations:** `src/core.h` lines 1470–1521 (the `rcx::fmt` namespace forward declarations), plus shared constants/types in `core.h`.
**Tests:** `tests/test_format.cpp` (489 lines).
**Provider interface:** `src/providers/provider.h`, `src/providers/buffer_provider.h`.
**Expected portability:** **pure** — no Qt widgets, no threading, no platform `#ifdef` except a static-assert on 128-bit-int availability. All logic is string/byte manipulation through an abstract `Provider`.

---

## 1. Purpose

`rcx::fmt` is the **pure presentation layer** that turns a single `Node` (a typed field in a struct-layout tree) plus a `Provider` (an abstract byte source) into the **exact text** shown on a row in the editor. It also handles the inverse: parsing user-typed text back into raw memory bytes for editing, plus validation of that text.

It is the single source of truth for:
- **Type-name strings** (`uint32_t`, `Material[2]`, `void*`, struct/union/enum keywords).
- **Value formatting**: integers (signed decimal, unsigned hex), floats (fixed 7-char body), doubles, bools, 128-bit ints, half-floats, pointers (with optional dereference), vectors, 4×4 matrices, UTF-8/UTF-16 strings, hex/ASCII byte previews.
- **Column layout**: fitting/justifying type, name, value, comment into fixed-width columns with ellipsis truncation or overflow.
- **Struct / array / pointer header & footer lines** (the `{` / `};` rows).
- **Offset-margin gutter** text (hex address column).
- **Enum-member and bitfield-member lines**.
- **Value parsing** (`parseValue`) with endianness handling and range checking.
- **Value validation** (`validateValue`) producing human-readable error strings.
- **Base-address validation** (delegates to `AddressParser`).
- **`extractBits`** — read a bitfield container and mask out a bit range.

`commontypes.h` is an unrelated-but-co-located data table: a static catalog of ~48 predefined struct templates (Windows NT, C++ STL MSVC x64, Unreal Engine, generic, math) the user can instantiate from a type chooser. It does **not** call into `format.cpp`; it just declares POD tables and a lookup function. It is documented in §11.

> NOTE: `compose.cpp` (a sibling subsystem, OUT OF SCOPE here) is the caller that walks the node tree, computes addresses/depths, and concatenates these per-line strings into the document. `format.cpp` is stateless aside from one global function pointer (`g_typeNameFn`).

---

## 2. Qt types used and Rust equivalents

| Qt type / API | Use here | Rust equivalent |
|---|---|---|
| `QString` | All returned text. UTF-16 internally. | `String` (UTF-8). The few UTF-16-specific paths (UTF16 string reads) need explicit `char16_t`/`u16` handling. |
| `QStringLiteral("…")` / `QLatin1Char` | Compile-time string/char literals | `"…"` / `char` literals |
| `QChar(0x2026)` | Ellipsis `…` | `'\u{2026}'` |
| `QChar(0x00B7)` (`·`) | Middle dot `·` for continuation margin | `'\u{00B7}'` |
| `QByteArray` | Raw byte buffers (reads, parse output) | `Vec<u8>` |
| `QStringList` / `.join(", ")` / `.split(' ', SkipEmptyParts)` | Vec component joins, hex-byte splits | `Vec<String>` + `.join`, `.split_whitespace()`/manual |
| `QString::number(v, base)` / `QString::number(v,'f'/'g',prec)` | Int→string, float→string | `format!`, or `ryu`/manual for `'g'`/`'f'` precision; see §6 caveats |
| `QString::asprintf("0x%llx", …)` | hex value formatting | `format!("{:#x}", v)` (note: `{:#x}` gives `0x…` lowercase, matches) |
| `QString::rightJustified(n,'0')`, `.leftJustified(n,' ')`, `.left(n)`, `.mid`, `.chop`, `.trimmed`, `.toUpper`, `.prepend`, `.contains` | column padding & string ops | `format!("{:>0width$}")`, manual padding, slicing; careful: Qt counts **UTF-16 code units**, see §10 |
| `QtEndian` `qbswap(v)` | byte-swap for big-endian | `v.swap_bytes()` |
| `QString::toInt/toUInt/toLongLong/toULongLong(&ok, base)` | parse ints | `i32::from_str_radix` etc., map errors to `ok=false` |
| `QString::toFloat/toDouble(&ok)` | parse floats | `f32::from_str` / `f64::from_str` |
| `QStringLiteral("L\"")` etc. | literal markers | literals |
| `Qt::CaseInsensitive` `startsWith/endsWith` | prefix/suffix checks | `.to_lowercase()` compare or `eq_ignore_ascii_case` |
| `Qt::Uninitialized` `QByteArray(n, ...)` | allocate without init | `vec![0u8; n]` (Rust zero-inits; behavior is equivalent because all bytes are overwritten before use) |
| `Q_ASSERT(...)` | debug assert in `extractBits` | `debug_assert!` |
| `Q_UNUSED(x)` | suppress unused warning | `let _ = x;` / `_param` |
| `std::numeric_limits<T>::min/max` | range checks | `T::MIN` / `T::MAX` |
| `std::isnan/isinf/signbit/fabs` | float classification | `f32::is_nan/is_infinite/is_sign_negative/abs` |
| `memcpy` bit-reinterpret of float↔uint | half/float conversions | `f32::from_bits` / `f32::to_bits` |
| `__int128` / `unsigned __int128` | 128-bit ints | `i128` / `u128` (native in Rust — no `#error` guard needed) |

**No threading, no signals/slots, no widgets.** This file is trivially portable; the only subtlety is QString UTF-16 semantics (§10).

---

## 3. Shared constants & types pulled from `core.h`

### 3.1 `NodeKind` enum (`core.h:24-34`)
`uint8_t`-backed, **order matters** (range checks like `k >= Int8 && k <= UInt128` depend on it):
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
Port as `#[repr(u8)]` with this exact discriminant order; range comparisons must be reproduced.

### 3.2 `KindMeta` table (`core.h:54-97`)
Single source of truth: `{kind, name, typeName, size, lines, align, flags}`. Relevant columns for this subsystem:
- `typeName` (display): `"hex8"`, `"int32_t"`, `"uint64_t"`, `"float"`, `"ptr64"`, `"fnptr64"`, `"vec3"`, `"mat4x4"`, `"str"`, `"wstr"`, `"struct"`, `"array"`, etc. **Lowercase** with `_t` suffix on int/uint.
- `size` (bytes): Hex8=1…Hex128=16; Int128/UInt128=16; Float16=2, Float=4, Double=8, Bool=1; Pointer32/FuncPtr32=4, Pointer64/FuncPtr64=8; Vec2=8, Vec3=12, Vec4=16, Mat4x4=64; UTF8=1, UTF16=2; Struct/Array=0 (dynamic).
- `lines`: all 1 except **Mat4x4=4**.
- `flags`: `KF_HexPreview` on Hex8..Hex128.

Helpers (`core.h:102-170`), all `constexpr`:
- `kindMeta(k)` → pointer into table or `nullptr` (out of range).
- `sizeForKind(k)` → `m->size` or 0.
- `linesForKind(k)` → `m->lines` or 1.
- `isHexNode(k)` / `isHexPreview(k)` → `Hex8 <= k <= Hex128`.
- `isPointerKind`, `isFuncPtr`, `isStringKind`, `isContainerKind`, `isVectorKind`, `isMatrixKind`.
- `isValidPrimitivePtrTarget(k)` (`core.h:164-170`): returns **false** for hex, pointer, funcptr, Struct, Array; **true** for everything else (ints, floats, bool, vecs, mats, strings). Tested at `test_format.cpp:476-484`.

### 3.3 `Node` struct (`core.h:210-363`)
Fields read by `format.cpp`:
- `kind` (NodeKind), `name` (QString), `comment` (QString).
- `structTypeName` (QString) — for struct/array headers; if empty falls back to keyword.
- `classKeyword` (QString) — `"struct"`/`"class"`/`"union"`/`"enum"`/`"bitfield"`; empty ⇒ `"struct"`.
- `arrayLen` (int), `elementKind` (NodeKind) — for arrays and primitive pointers.
- `strLen` (int, default 64) — string char/element count.
- `ptrDepth` (int 0..2) — pointer indirection levels for primitive-pointer deref.
- `bigEndian` (bool) — swap bytes on display & parse.
- Methods used: `resolvedClassKeyword()` (`core.h:357` — keyword or `"struct"`), `isEnum()` (`core.h:362` — keyword=="enum").

### 3.4 `Provider` (abstract, `providers/provider.h:38-153`)
Pure-virtual `read(addr, buf, len)→bool` and `size()→int`. Non-virtual convenience used by `format.cpp`:
- `readU8/U16/U32/U64(addr)` — little-endian-native `readAs<T>` via `read()`; **on read failure returns the zero-initialized `T{}`** (`readAs` ignores `read`'s bool return — `core.h:128-133`).
- `readF32(addr)` / `readF64(addr)`.
- `readBytes(addr, len)` — returns `QByteArray`; **on failure (`read` returns false) fills with `\0`** (`core.h:142-148`); `len<=0` → empty.
- `isReadable(addr, len)` (`core.h:122-126`): `len<0`→false; `len==0`→true; else `addr <= size() && len <= size()-addr` (unsigned). This is the bounds gate before optimistic reads.

Rust port: a `trait Provider` with `read(&self, addr: u64, buf: &mut [u8]) -> bool` and `size(&self) -> i64`, plus default-method helpers mirroring `readU*`/`readBytes`/`isReadable`. **`BufferProvider`** (`buffer_provider.h`) is the in-scope concrete impl: wraps a `Vec<u8>`, `read` does bounds-checked `memcpy`, `write` likewise; `isReadable` inherited. Tests use it.

### 3.5 Column-layout constants (`core.h:1130-1142`)
- `kFoldCol = 3` (fold-indicator prefix; NOT added by format.cpp — compose.cpp's concern).
- `kTreeIndent = 2` (chars per nesting level).
- `kColType = 14`, `kColName = 22`, `kColValue = 96`, `kColComment = 28`.
- `kSepWidth = 1`.
- `kCompactTypeW = 20` (compact-mode type cap; used by compose, not directly here).
- `format.cpp` aliases: `COL_TYPE=kColType`, `COL_NAME=kColName`, `COL_VALUE=kColValue` (`format.cpp:61-63`), and a **local** `COL_COMMENT = 28` (`format.cpp:64`, equals `kColComment`).
- `SEP = QStringLiteral(" ")` — single-space column separator (`format.cpp:65`).

---

## 4. File-local helpers (static, `format.cpp`)

### 4.1 Half-precision conversions
**`halfToFloat(uint16_t h) → float`** (`format.cpp:13-32`). IEEE-754 binary16 → binary32. Sign `s = (h&0x8000)<<16`; exp `e=(h>>10)&0x1f`; mant `m=h&0x3ff`.
- `e==0`: if `m==0` → ±0 (`b=s`); else **subnormal renormalization**: left-shift `m` until bit 0x400 set, decrementing `e`, then `e++`, `m&=0x3ff`, `b = s | ((e+112)<<23) | (m<<13)`.
- `e==0x1f`: inf/NaN → `b = s | 0x7f800000 | (m<<13)`.
- else (normal): `b = s | ((e+112)<<23) | (m<<13)`.
- `memcpy` `b`→`float`.

**`floatToHalf(float f) → uint16_t`** (`format.cpp:34-57`). binary32 → binary16, round-to-nearest-even.
- `e==128` (inf/NaN): `s | 0x7c00 | (m?0x200:0)`.
- `e>15`: overflow → `s | 0x7c00` (inf).
- `e<-14`: if `e<-24` underflow → `s` (±0); else subnormal: `m|=0x800000`, `shift=-e-14+13`, `hm=m>>shift`, round-up if bit `(shift-1)` set, return `s|hm`.
- normal: `hm=m>>13`; if bit 12 set round up; carry into exponent if `hm==0x400` (and re-check overflow); `s | ((e+15)<<10) | hm`.

Rust: implement bit-identically with `u16`/`u32`/`i32` and `f32::from_bits`/`to_bits`. (Could use the `half` crate's `f16`, but **bit-exact rounding must match** — verify against these algorithms; safest is a direct port.)

### 4.2 Column fitting
**`fit(QString s, int w) → QString`** (`format.cpp:67-74`):
- `w<=0` → empty.
- if `s.size() > w`: if `w>=2`, `s = s.left(w-1) + '…'` (ellipsis); else `s = s.left(w)`.
- return `s.leftJustified(w, ' ')` (pad right with spaces to width `w`).
Result is **always exactly `w` chars** (when `w>0`). `s.size()` is UTF-16 length (§10).

**`fitOverflow(const QString& s, int w) → QString`** (`format.cpp:77-82`):
- `w<=0` → empty.
- `s.size() <= w` → `s.leftJustified(w,' ')` (pad to `w`).
- else → return **full string unpadded** (overflow, no truncation). Used in compact mode.

### 4.3 Hex / value helpers
- **`hexVal(uint64_t v)`** (`:125`): `QString::asprintf("0x%llx", v)` → lowercase `0x…`. Rust: `format!("{:#x}", v)`.
- **`rawHex(uint64_t v, int digits)`** (`:129`): `QString::number(v,16).rightJustified(digits,'0')` — lowercase hex, zero-padded to `digits`, **no `0x`**. Rust: `format!("{:0width$x}", v)`. Note: if value needs more than `digits`, it is **not** truncated (rightJustified only pads).

---

## 5. Type-name functions (public)

A global override seam exists:
- **`TypeNameFn = QString(*)(NodeKind)`** (`core.h:1473`); `static g_typeNameFn = nullptr` (`format.cpp:87`).
- **`setTypeNameProvider(fn)`** (`:89`) — installs override. (Used so the app can substitute custom names.) Rust: a `static` (e.g. `OnceLock`/atomic fn-ptr) or thread-local; document as a global override hook. The tests do **not** set it, so default path = `kindMeta`.

- **`typeNameRaw(NodeKind) → QString`** (`:92-96`): override fn if set, else `kindMeta(kind)->typeName` (Latin1), else `"???"`. **Unpadded** — used for width/overflow detection.
- **`typeName(NodeKind, int colType=14) → QString`** (`:98-102`): same but wrapped in `fit(..., colType)` → fixed width. Test `testTypeName`: `typeName(Float)` trimmed == `"float"`, `.size()==14`.
- **`arrayTypeName(elemKind, count, structName) → QString`** (`:105-114`): elem name = `structName` if `elemKind==Struct && !structName.isEmpty()`, else `kindMeta(elemKind)->typeName` (or `"???"`); returns `elem + "[" + count + "]"`. E.g. `"uint32_t[16]"`, `"Material[2]"`.
- **`pointerTypeName(kind, targetName) → QString`** (`:117-121`): `kind` unused; `(targetName.isEmpty()?"void":targetName) + "*"`. E.g. `"void*"`, `"StructName*"`.

---

## 6. Scalar value formatters (public)

All return `QString`. Integer signed → decimal; unsigned → hex.

| Function | C++ line | Behavior |
|---|---|---|
| `fmtInt8/16/32/64(v)` | 133-136 | `QString::number(v)` — signed decimal. Test: `fmtInt32(-42)=="-42"`, `fmtInt32(0)=="0"`. |
| `fmtUInt8/16/32/64(v)` | 137-140 | `hexVal(v)` → `0x…` lowercase. |
| `fmtFloat16(uint16_t bits)` | 142-149 | `halfToFloat(bits)`; NaN→`"NaN"`; +inf→`"infh"`, -inf→`"-infh"`; else `QString::number(f,'g',4) + "h"`. |
| `fmtFloat(float v)` | 184-208 | Fixed 7-char body — see below. |
| `fmtDouble(double v)` | 209-216 | NaN→`"NaN"`; ±inf→`"inf"`/`"-inf"`; else `QString::number(v,'g',6)`; if result lacks `.`,`e`,`E` append `".0"`. Test: `fmtDouble(42.0)` contains `'.'`. |
| `fmtBool(uint8_t v)` | 217 | `v ? "true" : "false"`. |
| `fmtPointer32(uint32_t v)` | 219 | `v==0 ? "nullptr" : hexVal(v)`. |
| `fmtPointer64(uint64_t v)` | 220 | `v==0 ? "nullptr" : hexVal(v)`. Tests at 71-79. |

### 6.1 `fmtFloat` algorithm (`format.cpp:184-208`) — heavily tested
Produces a **fixed-width body**: positive = exactly 7 chars, negative = 8 (`-` + 7). Steps:
1. NaN → `"NaN"`. ±inf → `"inff"` / `"-inff"`.
2. `v==0 && signbit(v)` → `"-0.000f"` (negative zero special-case).
3. `av = fabs(v)`; if `av >= 100000` → overflow cap `"99999+f"` / `"-99999+f"`.
4. For `dec = 4` down to `0`: `body = QString::number(av,'f',dec)` then append `"f"` (or `".f"` when `dec==0`). If `body.size()==7`, prepend `-` if negative and return.
5. If no `dec` produced a 7-char body (rounding pushed past 99999) → overflow cap.

Expected outputs (from `testFmtFloat`, lines 21-64):
`3.14159→"3.1416f"`, `-3.14159→"-3.1416f"`, `0→"0.0000f"`, `0.02→"0.0200f"`, `-0.069→"-0.0690f"`, `15.6543→"15.654f"`, `-77.6624→"-77.662f"`, `500→"500.00f"`, `5000→"5000.0f"`, `50000→"50000.f"`, `100000→"99999+f"`, `-100000→"-99999+f"`, `inf→"inff"`, `-inf→"-inff"`, `NaN→"NaN"`, `1→"1.0000f"`, `-1→"-1.0000f"`. Also `1e-7→"0.0000f"` (size ≤9, contains `'f'`, `testFmtFloatVerySmall`), and `-0.0f` starts with `'-'`.

**Port caveat:** `QString::number(av,'f',dec)` = fixed-point with `dec` decimals, round-half-to-even per Qt/snprintf semantics. Rust `format!("{:.*}", dec, av)` uses round-half-to-even on most platforms and should match; **validate against the test vectors** during porting. The 7-char fixed-width search loop is the load-bearing invariant.

### 6.2 `fmtDouble` / `fmtFloat16` port caveats
`QString::number(v,'g',N)` = shortest of `%e`/`%f` with N significant digits, trailing-zero stripped. Rust has no direct `'g'`; emulate or use a small helper (the `'g'` format is C `printf %g`). `fmtDouble` then forces a decimal point if none present. `fmtFloat16` uses `'g',4`. These need a faithful `%g`-style formatter; the tests for `fmtDouble`/`fmtFloat16` are loose (contains `.`/`e`, non-empty) so exact digit parity is less critical than for `fmtFloat`, but aim for parity.

### 6.3 128-bit integers (`format.cpp:151-182`)
- `#if defined(__SIZEOF_INT128__)` typedef `rcx_int128`/`rcx_uint128`; else `#error`. **Rust has native `i128`/`u128` — drop the guard.**
- `fmtUInt128Impl(u128 v)`: `v==0`→`"0"`; else build decimal digits LSB-first into a buffer, reverse. Rust: `v.to_string()`.
- `fmtInt128(const void* data)`: `memcpy` 16 bytes → `i128`; if negative, two's-complement-negate without UB (`(u128)(-(v+1))+1`) and prefix `-`. Rust: read `i128::from_le_bytes` (caller already byte-ordered the bytes), `v.to_string()`.
- `fmtUInt128(const void* data)`: `memcpy`→`u128`, `to_string`.

---

## 7. Indentation, offset margin, struct/array/pointer headers

- **`indent(int depth) → QString`** (`:224-226`): `QString(depth * kTreeIndent, ' ')` = `depth*2` spaces. Test `testIndent`: 0→"", 1→"  ", 3→"      ".
- **`fmtOffsetMargin(uint64_t off, bool isContinuation, int hexDigits=8) → QString`** (`:230-234`):
  - continuation → `"  · "` (two spaces, middle-dot U+00B7, space).
  - else → `QString::number(off,16).toUpper().rightJustified(hexDigits,'0') + ' '` = **UPPERCASE** zero-padded hex + trailing space. Tests (81-97): `(0x10,false)→"00000010 "`, `(0,false)→"00000000 "`, `(0xFFFFF80012345678,false,16)→"FFFFF80012345678 "`, `(0x10,false,4)→"0010 "`.
- **`structTypeName(const Node&) → QString`** (`:238-244`): `node.structTypeName` if non-empty, else `node.resolvedClassKeyword()` (e.g. `"struct"`/`"union"`).

### 7.1 `fmtStructHeader(node, depth, collapsed, colType=14, colName=22, compact=false)` (`:248-259`)
`ind = indent(depth)`; `rawType = structTypeName(node)`; `suffix = collapsed ? "" : "{"`.
- If `node.name.isEmpty()` → **anonymous**: `ind + rawType + SEP + suffix` (no column padding). e.g. `"union {"`.
- Else: `type = compact ? fitOverflow(rawType,colType) : fit(rawType,colType)`; return `ind + type + SEP + name + SEP + suffix`. **Name is NOT fitted** here (raw `node.name`).
Test `testFmtStructHeader`: expanded contains `struct`,`Test`,`{`; collapsed contains `struct`,`Test`, no `{`.

### 7.2 `fmtStructFooter(node, depth, totalSize=-1)` (`:261-272`)
`footer = indent(depth) + "};"`. Then append a **toolbar hint string**:
- enum: `"  +1 +10 Top"`.
- else: `"  +1 +10h +100h +1000h Trim Top"`.
- If `totalSize > 0`: append `"  // 0x{HEX_UPPER} ({decimal})"`.
Tests: `fmtStructFooter(n,0)` contains `"};"`; with `totalSize=0x14`(20) `testFmtStructFooterSimple` checks it contains `"};"` and **NOT** `"sizeof"`. (Note: `totalSize=0x14>0` so the `// 0x14 (20)` comment IS appended — test only asserts absence of literal `"sizeof"`.)

### 7.3 `fmtArrayHeader(node, depth, viewIdx/*unused*/, collapsed, colType=14, colName=22, elemStructName={}, compact=false)` (`:276-282`)
`ind`; `rawType = arrayTypeName(node.elementKind, node.arrayLen, elemStructName)`; `type = compact?fitOverflow:fit`; `suffix = collapsed?"":"{"`; return `ind + type + SEP + node.name + SEP + suffix`. (Name not fitted.)

### 7.4 `fmtPointerHeader(node, depth, collapsed, prov, addr, ptrTypeName, colType=14, colName=22, compact=false)` (`:286-304`)
Merged pointer+struct header. `ind`; `overflow = compact && ptrTypeName.size() > colType`; `type = compact?fitOverflow(ptrTypeName,colType):fit(ptrTypeName,colType)`.
- `collapsed`:
  - if `overflow` → `ind + type + SEP + node.name + SEP + readValue(node,prov,addr,0)` (no padding).
  - else → `name = fit(node.name,colName)`; `val = fit(readValue(node,prov,addr,0), COL_VALUE)`; `ind + type + SEP + name + SEP + val`. (Shows pointer value instead of brace.)
- not collapsed → `ind + type + SEP + node.name + SEP + "{"`.

---

## 8. Hex/ASCII preview, value reading, full node line

### 8.1 Byte preview helpers (static)
- `isAsciiPrintable(c)` (`:308`): `0x20 <= c <= 0x7E`.
- `sanitizeString(QString) → QString` (`:311-323`): escapes `\n`→`"\\n"`, `\r`,`\t`,`\`→`"\\\\"`; `c < 0x20` → `"\\x"+hex(unicode)`; else passthrough. Used for displayed string values.
- `bytesToAscii(QByteArray b, int slot) → QString` (`:325-333`): `slot` chars; byte = `b[i]` or 0 if `i>=size`; printable→char, else `'.'`.
- `kHexDigits = "0123456789ABCDEF"` (uppercase).
- `bytesToHex(QByteArray b, int slot) → QString` (`:337-347`): space-separated 2-digit **uppercase** hex of `slot` bytes (0-fill beyond size); no trailing space. Output length `slot*3-1`.
- `fmtAsciiAndBytes(prov, addr, sizeBytes, slotBytes=8) → QString` (`:349-356`): `slot = max(slotBytes,sizeBytes)`; read `slot` bytes (zero buffer if not readable); `bytesToAscii(b,slot) + "  " + bytesToHex(b,slot)`. (Helper; not directly in the public per-line path, which inlines similar logic.)

### 8.2 `readValueImpl(node, prov, addr, subLine, mode)` (`:362-503`) — core
Two modes: `ValueMode::Display` (pretty) and `ValueMode::Editable` (parse-friendly). `be = node.bigEndian`. Local lambdas read & byte-swap on big-endian: `rU16/rU32/rU64` (`qbswap` when `be`), `rF32`/`rF64` (read uint, swap, `memcpy`→float).

Per `node.kind`:
- **Hex8/16/32/64**: display → `hexVal(read)`; editable → `rawHex(read, 2/4/8/16)` (zero-padded, no `0x`).
- **Hex128** (`:376-401`): read 16 bytes (resize to 16 if short).
  - editable: copy to `show`; if `be` reverse; emit space-separated 2-digit hex of all 16 bytes → `"HH HH … HH"`.
  - display: if `be` reverse `b`; `lo=bytes[0..8]`, `hi=bytes[8..16]` (LE memcpy); if `hi==0` → `hexVal(lo)`; else `"0x" + UPPER(hi) + UPPER(lo).rightJustified(16,'0')`.
- **Int8/16/32/64**: `fmtIntN((intN_t)read)` (with `be` swap on 16/32/64). Int8 uses `prov.readU8` directly (no swap).
- **Int128/UInt128** (`:406-412`): read 16 bytes; if `be` reverse; `fmtInt128`/`fmtUInt128(b.constData())`.
- **UInt8/16/32/64**: `fmtUIntN(read)`.
- **Float16** (`:417`): `s = fmtFloat16(rU16)`; editable → `s.trimmed()`.
- **Float** (`:418`): `s = fmtFloat(rF32)`; editable → `s.trimmed()`.
- **Double** (`:419`): `s = fmtDouble(rF64)`; editable → `s.trimmed()`.
- **Bool** (`:420`): `fmtBool(readU8)`.
- **Pointer32 / FuncPtr32 / FuncPtr64** (`:426-462`): display → `fmtPointer32/64`; editable → `rawHex(val, 8/16)`.
- **Pointer64** (`:431-452`): primitive-pointer dereference. If `ptrDepth>0 && isValidPrimitivePtrTarget(elementKind) && val!=0`:
  - follow `ptrDepth-1` extra hops (`target = readU64(target)` if readable, else 0).
  - if `target!=0 && isReadable(target, sizeForKind(elementKind))`: build temp `Node{kind=elementKind, strLen}`, recurse `readValueImpl(tmp, prov, target, 0, mode)`; display → `"-> " + derefVal`; editable → `derefVal`.
  - else fall through to plain `fmtPointer64`/`rawHex`.
  - non-deref path: display→`fmtPointer64`, editable→`rawHex(val,16)`.
- **Vec2/Vec3/Vec4** (`:463-471`): `count = sizeForKind/4`; join `fmtFloat(readF32(addr+i*4))` with `", "`. (No endian handling; always native LE F32.) Test: Vec2 contains `","`; Vec3 has 2 commas; Vec4 has 3 commas (subLine ignored).
- **Mat4x4** (`:472-482`): editable → empty (not single-value editable). Display: `subLine` 0..3 → `"row{n} [f, f, f, f]"` using `fmtFloat(readF32(addr+(subLine*4+c)*4))`; out-of-range subLine → `"?"`.
- **UTF8** (`:483-490`): read `strLen` bytes; truncate at first `\0`; `QString::fromUtf8`; display → `sanitizeString` then `"\"" + s + "\""`; editable → raw `s`.
- **UTF16** (`:491-499`): read `strLen*2` bytes; `QString::fromUtf16`(char16 ptr, size/2); truncate at first U+0000; display → sanitize then `"L\"" + s + "\""`; editable → raw `s`.
- **default** (Struct/Array): empty string.

**Public wrappers:**
- `readValue(node, prov, addr, subLine)` (`:505-508`) → Display mode.
- `editableValue(node, prov, addr, subLine)` (`:560-563`) → Editable mode.

### 8.3 `fmtNodeLine(node, prov, addr, depth, subLine=0, comment={}, colType=14, colName=22, typeOverride={}, compact=false)` (`:512-556`)
Builds the full row text. Steps:
1. `ind = indent(depth)`.
2. `rawType = typeOverride.isEmpty() ? typeNameRaw(node.kind) : typeOverride`.
3. `overflow = compact && rawType.size() > colType`.
4. `type = overflow ? fitOverflow(rawType,colType) : (typeOverride.isEmpty() ? typeName(node.kind,colType) : fit(typeOverride,colType))`.
5. `name = fit(node.name, colName)`.
6. `effectiveColType = overflow ? rawType.size() : colType`; `prefixW = effectiveColType + colName + 2*kSepWidth` (continuation indent width).
7. `cmtSuffix = comment.isEmpty() ? "" : fit(comment, COL_COMMENT)`.
8. **Mat4x4**: `val = readValue(...,subLine)`; `subLine==0` → `ind+type+SEP+name+SEP+val+cmtSuffix`; else → `ind + QString(prefixW,' ') + val + cmtSuffix` (continuation rows, blank prefix). No value truncation (large floats shown fully).
9. **isHexPreview(kind)** (`:544-551`): `sz = sizeForKind`; read `sz` bytes (zero if not readable); `ascii = bytesToAscii(b,sz).leftJustified(colName,' ')`; `hex = bytesToHex(b,sz).leftJustified(max(23, sz*3-1),' ')`; return `ind + type + SEP + ascii + SEP + hex + cmtSuffix`. (ASCII occupies the name column; hex occupies the value column. Min hex width 23.)
10. **default**: `val = overflow ? readValue(...) : fit(readValue(...), COL_VALUE)`; return `ind + type + SEP + name + SEP + val + cmtSuffix`.

---

## 9. Parsing & validation (text → bytes)

### 9.1 Static helpers
- `toBytes<T>(v) → QByteArray` (`:567-572`): `sizeof(T)` raw little-endian bytes (host-order memcpy; host is LE). Rust: `v.to_le_bytes()`.
- `stripHex(s)` (`:574-578`): drop leading `"0x"` (case-insensitive) if present.
- `parseAsciiValue(text, expectedSize, ok) → QByteArray` (**public**, `:581-592`): `*ok=false`; require `text.size()==expectedSize`; each char must be ≤255 (else fail) → one byte each; set `ok=true`.
- `parseHexBytes(s, expectedSize, ok) → QByteArray` (static, `:597-622`): trims; if contains space → split on spaces (`SkipEmptyParts`), require exactly `expectedSize` groups each of length 2, parse base-16; else require `size == expectedSize*2` and parse 2 chars at a time. Sets `ok` on success.
- `parseIntChecked<T,ParseT>(val, ok) → QByteArray` (`:625-636`): if `*ok` and `val` outside `T`'s `[min,max]` (signed) or `> max` (unsigned), set `*ok=false`; return `toBytes<T>(val)` or empty.

### 9.2 `parseValue(NodeKind kind, const QString& text, bool* ok) → QByteArray` (`:638-820`)
`*ok=false`; `s=text.trimmed()`.
- **Empty** `s`: if UTF8/UTF16 → `ok=true`, return empty (caller pads); else fail (return empty). Test `testParseValueEmptyString`: UTF8 empty ok+empty; Int32 empty fails.
- **`parseHexUnified(cleaned, byteCount)` lambda** (`:656-665`): if spaced → `parseHexBytes` (exact `byteCount` groups); else if `cleaned.size() > byteCount*2` → fail; else **left-zero-pad** to `byteCount*2` then `parseHexBytes`. So `0x42` into Hex32 → `00000042` → bytes `[00,00,00,42]`. Hex stored in **memory/display order** (no endian swap here).
- **Hex8/16/32/64/128** (`:667-671`): `parseHexUnified(stripHex(s), byteCount)`. Tests: `Hex32 "DEADBEEF"`→`[DE,AD,BE,EF]`; `0xDEADBEEF` same; `Hex16 "AB CD"`→`[AB,CD]`; `Hex128` 16-group space form; too-short fails; `Hex8 "1FF"` fails (3 digits > 2).
- **Int8/16** (`:672-693`): if `0x`-prefixed → parse `toUInt(base16)`, range-check (`>0xFF`/`>0xFFFF` fails), cast to signed (two's complement: `0xFF`→-1, `0x80`→-128). Else decimal `toInt` then `parseIntChecked<int8/16_t>`.
- **Int32** (`:694-704`): hex → `toULongLong(16)`, fail if `>0xFFFFFFFF`, cast int32. Decimal → `toInt`.
- **Int64** (`:705-714`): hex → `toULongLong(16)` cast int64; decimal → `toLongLong`.
- **UInt8/16/32** (`:715-717`): base 16 if `0x` else 10; `parseIntChecked<uintN_t>`.
- **UInt64** (`:718`): base 16/10; `toULongLong`; `toBytes<uint64_t>`.
- **Int128/UInt128** (`:719-755`): decimal (optional leading `-` for signed) or `0x…` hex. Manual digit accumulation into `u128` with overflow detection (`next < acc` → fail). Signed range `[-(2^127), 2^127-1]`; unsigned rejects negative. Writes 16 bytes (host LE).
- **Float16** (`:756-763`): strip trailing `'h'` then `'f'` (case-insensitive); replace `','`→`'.'`; `toFloat`; `toBytes<uint16_t>(floatToHalf(val))`.
- **Float** (`:764-770`): strip trailing `'f'`; `,`→`.`; `toFloat`; `toBytes<float>`. Test: `"3.14"`→f32≈3.14.
- **Double** (`:771-776`): `,`→`.`; `toDouble`; `toBytes<double>`.
- **Bool** (`:777-785`): `"true"`/`"1"`→1; `"false"`/`"0"`→0; else fail. Test: `"banana"` fails.
- **Pointer32/FuncPtr32** (`:786-789, 794-797`): `stripHex(s).toUInt(16)`; `toBytes<uint32_t>`.
- **Pointer64/FuncPtr64** (`:790-793, 798-801`): `stripHex(s).toULongLong(16)`; `toBytes<uint64_t>`. Test: `"0x0000000000400000"`→0x400000.
- **UTF8** (`:802-807`): `ok=true`; strip surrounding `"`; `s.toUtf8()`. Test: `"\"hello\""`→`"hello"`.
- **UTF16** (`:808-816`): `ok=true`; strip leading `L"` or `"`, trailing `"`; output `s.size()*2` bytes via `s.utf16()` (UTF-16LE code units). Rust: encode `String` as UTF-16 LE bytes.
- **default**: empty.

### 9.3 Node-aware `parseValue(const Node& node, text, ok) → QByteArray` (`:825-841`)
Calls `parseValue(node.kind, text, ok)`; if ok & `node.bigEndian` & non-empty, **reverse the byte array** for scalar endian-bearing kinds: Int/UInt/Hex 16/32/64/128, Float16, Float, Double. (Hex byte forms are stored in display order, so the same reverse applies uniformly.) Other kinds (Int8/UInt8/Hex8, Bool, pointers, strings, vec/mat) are left unswapped.

### 9.4 `validateValue(NodeKind kind, const QString& text) → QString` (`:845-896`)
Returns error message, or empty string if valid.
1. `s=trimmed`; empty → valid (`{}`). Test `testValidateValueEmpty`.
2. `isHexKind = isHexNode(kind) || pointer/funcptr kinds`; `isIntKind = Int8 <= kind <= UInt128`.
3. If hex/int: detect `0x` prefix; `digits` = remainder. Character-set check:
   - hex mode (`hasHexPrefix || isHexKind`): allow `0-9 a-f A-F`; allow spaces only for multi-byte hex (`Hex16..Hex128`); else error `"invalid hex '%1'"`.
   - decimal mode: allow leading `-` only for signed (`Int8..Int128`); each remaining char must be a digit, else `"invalid '%1'"`.
4. Then `parseValue(kind, text, &ok)`; if ok → valid.
5. Else build a message:
   - float kinds (`Float`/`Double`) → `"invalid number"`.
   - else if `kindMeta->size` in `1..8` → `"too large! max=0x%1"` where max = `~0ULL` (size 8) or `(1<<(size*8))-1`, formatted hex zero-padded to `size*2` digits.
   - else `"invalid"`.
   Tests: `Int8 "999"` → non-empty (`testValidateValueHexOverflow`); `Hex128` 16-group form valid (`testValidateValueHex128`).

### 9.5 `validateBaseAddress(text) → QString` (`:900-905`)
`trimmed`; empty → `"empty"`; else `AddressParser::validate(s)` (delegated; out-of-scope subsystem — `addressparser.h`). Rust: call into the address-parser port.

---

## 10. Enum/bitfield member lines & `extractBits`

- **`fmtEnumMember(name, value, depth, nameW) → QString`** (`:907-910`): `indent(depth) + name.leftJustified(nameW) + " = " + QString::number(value)`. `value` is `int64_t` decimal.
- **`extractBits(prov, addr, containerKind, bitOffset, bitWidth) → uint64_t`** (`:914-927`): reads container (Hex8→U8, Hex16→U16, Hex32→U32, else U64); `Q_ASSERT(bitOffset+bitWidth<=64)`; if `bitWidth>=64` → `container >> bitOffset`; else `(container >> bitOffset) & ((1<<bitWidth)-1)`. (No endian handling.)
- **`fmtBitfieldMember(name, bitWidth, value, depth, nameW) → QString`** (`:929-934`): `indent(depth) + name.leftJustified(nameW) + " : {bitWidth} = {value}"` (value decimal `uint64_t`).

---

## 11. `commontypes.h` — predefined struct catalog

A static, compile-time catalog. **No dependency on `format.cpp`**; purely declarative data used by the type-chooser UI (out of scope) to instantiate real fields instead of blank padding.

### Types
- **`CommonField`** (`commontypes.h:11-17`): `{ int offset; NodeKind kind; const char* name; const char* ptrTarget=nullptr; }`. `ptrTarget` = pointer target type name (empty/null = `void*`).
- **`CommonType`** (`:19-26`): `{ const char* name; const char* category; const char* classKeyword; int totalSize; const CommonField* fields; int fieldCount; }`.
- **`kCommonTypes[]`** (`:332-390`): array built via macro `CT(n,cat,kw,sz,arr)` = `{n,cat,kw,sz,arr,std::size(arr)}` (`:330`). ~48 entries across categories: **"Windows NT"** (`_M128A`, `UNICODE_STRING`, `LIST_ENTRY`, `LARGE_INTEGER`(union), `OBJECT_ATTRIBUTES`, `CLIENT_ID`, `IO_STATUS_BLOCK`, `GUID`, `FILETIME`, `FILETIME_u64`(union), `RTL_BALANCED_NODE`, `SINGLE_LIST_ENTRY`, `STRING`, `DISPATCHER_HEADER`), **"Time"** (`UnixTime32`, `UnixTime64`), **"C++ STL"** (`std::string`/`std::wstring` share fields, `std::vector`, `std::shared_ptr`, `std::unique_ptr`, `std::function`, `std::map_node`, `std::unordered_map`), **"Unreal"** (`FString`, `FName`, `TArray`(=FString), `FVector`, `FRotator`, `FTransform`, `FQuat`, `FLinearColor`), **"Generic"** (`VTable8`, `RefCounted`, `LinkedNode`, `TreeNode`, `SlabEntry`, `Delegate`, `Variant`, `Slice`, `FatPointer`, `TimeStamp`), **"Math"** (`RGBA8`, `AABB`, `Matrix4x4`, `Sphere`, `Ray`, `Plane`).
- **`kCommonTypeCount`** (`:394`): `std::size(kCommonTypes)`.
- **`findCommonType(const QString& name) → const CommonType*`** (`:397-403`): linear search by exact name; nullptr if not found.

Notable field details worth preserving exactly: `UNICODE_STRING.Buffer` is a `Pointer64` with `ptrTarget="UTF16"`; `OBJECT_ATTRIBUTES.ObjectName` → `ptrTarget="UNICODE_STRING"`; `std::map_node._Color` comment "0=red,1=black"; `FTransform` uses three `Vec4`s; `Matrix4x4` is a single `Mat4x4` field.

**Rust port:** a `const`/`static` array of structs, or `&'static [CommonType]`, with field slices. `&str` for the C strings, `Option<&str>` for `ptrTarget`. `findCommonType` = linear scan or a `match`/map. Fully portable, no Qt.

---

## 12. Subtle behaviors the tests rely on (checklist for the Rust port)

1. **`fit` always returns exactly `w` chars** (when `w>0`); ellipsis is U+2026 and only when `w>=2`. `typeName(Float).size()==14` and trimmed=="float" (test 9-13).
2. **`fmtFloat` fixed-width search**: must reproduce the exact 7-char-body algorithm and the special cases (`-0.000f`, `99999+f`, `inff`/`-inff`/`NaN`). All 17 `testFmtFloat` vectors must match byte-for-byte.
3. **Unsigned ints render hex, signed render decimal** (`fmtUInt32` = `0x…`, `fmtInt32` = decimal).
4. **`fmtPointer*` null → `"nullptr"`**, non-null → lowercase `0x…`.
5. **`fmtOffsetMargin`**: UPPERCASE zero-padded hex + trailing space; continuation = `"  · "` (U+00B7); `hexDigits` controls width (8 default, 16 kernel, 4 etc.).
6. **`fmtStructFooter`** never contains the literal `"sizeof"` (uses `// 0xNN (dec)` form instead).
7. **`indent(d) = 2*d` spaces** (kTreeIndent=2).
8. **Parse hex is memory/display order** (`Hex32 "DEADBEEF"` → bytes `[DE,AD,BE,EF]`), and `0x`-prefixed unspaced hex **left-zero-pads** to type width.
9. **Signed hex parse is two's complement & range-checked**: `Int8 "0xFF"`→-1, `"0x80"`→-128, `"0x1FF"`→fail; analogous for Int16/Int32.
10. **Range checks**: `UInt8 "300"`/`Int8 "200"`/`Int8 "-129"`/`UInt16 "70000"`/`Hex8 "1FF"`/`Hex16 "1FFFF"` all fail; boundary `UInt8 "255"`, `Int8 "-128"` succeed.
11. **Bool parse**: `"true"/"1"`→1, `"false"/"0"`→0, anything else fails (`"banana"`).
12. **Empty string**: UTF8/UTF16 → ok+empty; all other kinds → fail.
13. **Hex128 editable** = 16 space-separated uppercase byte pairs (length ≥47 = 16*3-1); spaced parse requires exactly 16 groups (8-group input fails).
14. **Vec2/3/4 single-line** comma counts: 1/2/3 commas; subLine ignored.
15. **`editableValue` for Float** strips padding/`f` suffix appropriately — `editableValue(Float)` contains `"3.14"` (trimmed; note: editable Float = `fmtFloat(...).trimmed()`, so it still contains the `f` body string, e.g. `"3.1400f"` — the test only asserts `.contains("3.14")`).
16. **`isValidPrimitivePtrTarget`**: hex/ptr/funcptr/Struct/Array → false; ints/float/bool → true (gates pointer dereference display `"-> value"`).
17. **`validateValue` error strings**: `"invalid hex '%1'"`, `"invalid '%1'"`, `"invalid number"`, `"too large! max=0x%1"`, `"invalid"` — exact text matters only loosely (tests assert non-empty), but reproduce for parity.
18. **Big-endian**: display reads swap via `qbswap`/manual reverse; `parseValue(node,...)` reverses the byte array for scalar endian-bearing kinds; Int8/UInt8/Hex8/Bool/pointers/strings/vec/mat are NOT swapped.

---

## 13. Concurrency, platform, error handling

- **Concurrency:** none. All functions are pure given their args, except the single mutable global `g_typeNameFn` (a function-pointer override seam). In Rust use a thread-safe global (e.g. `AtomicPtr`/`OnceLock<fn(...)>`) or document it as set-once at startup.
- **Platform-specific:** only `#if defined(__SIZEOF_INT128__)` / `#error` (`format.cpp:153-158`). Rust has native `i128`/`u128`, so this guard is dropped — no `#[cfg]` needed.
- **Error handling:** no exceptions. Failures surface as `bool* ok = false` (parse) or empty/`{}` returns. Reads through `Provider` never throw; out-of-bounds reads yield zeroed buffers (`readBytes` fills `\0`; `readAs` returns `T{}`). Rust port should keep this "lenient read returns zeros" contract for byte-for-byte output parity.
- **Bounds gate:** `fmtNodeLine`/`fmtAsciiAndBytes` call `prov.isReadable(addr, sz)` and substitute an all-zero buffer when not readable, so unreadable memory still renders a stable (all-dots / `0x0`) row rather than failing.

---

## 14. Public API surface (for the Rust module)

From `core.h:1472-1521`, namespace `rcx::fmt` (24 public functions + 1 type alias):

`TypeNameFn` (alias); `setTypeNameProvider`; `typeName`, `typeNameRaw`; `fmtInt8/16/32/64`, `fmtUInt8/16/32/64`, `fmtFloat`, `fmtDouble`, `fmtBool`, `fmtPointer32/64`; `fmtNodeLine`; `fmtOffsetMargin`; `fmtStructHeader`, `fmtStructFooter`, `fmtArrayHeader`; `structTypeName`, `arrayTypeName`, `pointerTypeName`; `fmtPointerHeader`; `validateBaseAddress`; `indent`; `readValue`, `editableValue`; `parseValue(NodeKind,…)`, `parseValue(Node&,…)`, `parseAsciiValue`; `validateValue`; `fmtEnumMember`, `fmtBitfieldMember`; `extractBits`.

Plus the additional public-but-not-forward-declared-in-this-region functions defined in `format.cpp`: `fmtFloat16`, `fmtInt128`, `fmtUInt128` (declared elsewhere or used internally; `fmtFloat16`/`fmtInt128`/`fmtUInt128` are used by `readValueImpl`). The half-conversion helpers (`halfToFloat`/`floatToHalf`) and column helpers (`fit`/`fitOverflow`) are file-static.

`commontypes.h` adds: `CommonField`, `CommonType` structs, `kCommonTypes[]`, `kCommonTypeCount`, `findCommonType`.

---

## 15. Recommended Rust crates

- **No mandatory external crate** — this is std-only string/byte work.
- `half` (optional) for `f16`, **but** verify bit-exact rounding against `halfToFloat`/`floatToHalf`; a direct port is the safe default.
- A small custom `%g`-style float formatter (or `ryu` + post-processing) for `fmtDouble`/`fmtFloat16` to mirror Qt `QString::number(v,'g',N)`.
- For UTF-16 string read/write: std `char::decode_utf16` / `str::encode_utf16` (no crate needed).
- Keep `QString`→`String`, `QByteArray`→`Vec<u8>`, `Provider`→a trait. Watch §10 (Qt counts UTF-16 code units in `.size()`/`.left()`; for the ASCII/hex preview and column fitting this matters only when non-BMP/multibyte chars appear in type/name strings — type names are ASCII so for those columns char-count parity holds; the `fit`/ellipsis logic on arbitrary user `name`/`comment` strings should ideally count UTF-16 units to match exactly, though ASCII content makes byte/char/u16 counts identical).
