# PORTING SPEC — `scanner` (Value / pattern search engine over byte regions)

Function-level port spec to drive a faithful Rust implementation of `src/scanner.{h,cpp}`.

- **Source of truth:** `/home/loke/reclass-cpp/src/scanner.h` (174 lines), `/home/loke/reclass-cpp/src/scanner.cpp` (1138 lines).
- **Behavioral map:** `_design/understand/scanner.md` (read it — this spec assumes its §1–§13 numbering).
- **Tests (oracle):** `tests/test_scanner.cpp` (163 asserts, PASS), `tests/test_scanner_combinations.cpp` (68 asserts, PASS). Both green in `_oracle/RESULTS.md`; there is **no golden text fixture** for the scanner — its oracle is the assertion set in those two `.cpp` files, which we translate 1:1.
- **Depends on:** `provider` module (`Provider` trait, `MemoryRegion`, `RegionType`). These are *consumed*, owned by `provider`.
- **Out of scope:** the Win32 `WinSelfProvider`/`selfAttach_findMutateRevalidate` test (live-process source, `#ifdef _WIN32`). It is a `provider::native` stub concern; the Rust scanner test for it stays `#[cfg(windows)]` and `#[ignore]`/stub.

---

## 0. Target crate / module

- **Crate:** the single `reclass` package (per `ARCHITECTURE.md` §2 — one package, modules under `src/`).
- **Module:** `src/scanner.rs` (declared `pub mod scanner;` in `lib.rs`). Feature: **`always`** (the algorithm core is `std`-only); the *parallel/async wrapper* is gated by the optional `scanner-parallel` feature which pulls **`rayon`** (`ARCHITECTURE.md` table: "scanner … always (rayon under `scanner-parallel`)"). Default features include neither gpui nor rayon for the logic core, so `cargo test --no-default-features` compiles and runs every scanner unit test.
- **Re-used from `provider`:** `use crate::provider::{Provider, MemoryRegion, RegionType};`. Do **not** redefine these here. Provider trait shape (from `_design/understand/providers.md`): `fn read(&self, addr: u64, buf: &mut [u8]) -> bool`, `fn size(&self) -> u64`, `fn enumerate_regions(&self) -> Vec<MemoryRegion>`, `fn write(&self, addr: u64, data: &[u8]) -> bool`. `MemoryRegion { base: u64, size: u64, readable: bool, writable: bool, executable: bool, module_name: String, type_: RegionType }` (field name `type` is a Rust keyword → `type_` or `region_type`; pick one and be consistent — this spec uses `region_type`). `RegionType { Image=0, Mapped=1, Private=2 }`, default `Private`.
- **Crate deps used by this module:** `std` only for the core. `rayon` (feature `scanner-parallel`) for the worker thread-pool path. Optional `memchr` for the 1-byte BMH fast path (the C++ uses `std::memchr`; `slice::iter().position` is a fine `std`-only substitute, so `memchr` is **not required** — note it as an optional micro-opt). `thiserror` is **not needed** — parse/serialize return `Result<(), String>`-shaped errors with the exact upstream message strings (the panel shows those strings verbatim), so we model errors as plain `String` payloads, not typed enums (see §6). `serde` is only touched by the panel's result-JSON (out of subsystem; documented in §5 / §8).

---

## 1. Module layout inside `src/scanner.rs`

```
// ---- value/condition vocabulary (serde-derive: see §3) ----
pub enum ValueType { ... }            // scanner.h:15-22
pub enum ScanCondition { ... }        // scanner.h:26-38
pub struct AddressRange { ... }       // scanner.h:42-45
pub struct ScanRequest { ... }        // scanner.h:47-75   (Default)
pub struct ScanResult { ... }         // scanner.h:77-82   (Default, Clone)
pub struct ScanStats { ... }          // scanner.h:85-90   (Default, Copy)

// ---- pure free functions (the parity-critical core) ----
pub fn parse_signature(input: &str) -> Result<(Vec<u8>, Vec<u8>), String>;      // scanner.cpp:128
pub fn serialize_value(t: ValueType, input: &str) -> Result<(Vec<u8>, Vec<u8>), String>; // :223
pub fn natural_alignment(t: ValueType) -> i32;                                  // :430
pub fn value_size_for_type(t: ValueType) -> i32;                                // :456
pub fn is_system_module(module_name: &str) -> bool;                             // :35
pub fn bmh_find(data: &[u8], pat: &[u8]) -> Option<usize>;                       // :86
fn format_region_context(region: &MemoryRegion, address: u64) -> String;        // :22 (internal)
fn hex_val(c: u8) -> i32;                                                        // :120 (internal)
fn compare_typed(a: &[u8], b: &[u8], vt: ValueType) -> i32;                      // :471 (internal)

// ---- progress/observer plumbing (replaces Qt signals) ----
pub trait ScanObserver { ... }        // §7
pub struct NullObserver;              // no-op default for tests

// ---- pure scan kernels (take provider + abort token + observer) ----
pub fn run_scan(prov: &dyn Provider, req: &ScanRequest,
                abort: &AtomicBool, obs: &dyn ScanObserver) -> Vec<ScanResult>;  // :562
pub fn run_rescan(prov: &dyn Provider, results: Vec<ScanResult>, read_size: i32,
                  condition: ScanCondition, value_type: ValueType,
                  filter_pattern: &[u8], filter_mask: &[u8], filter_pattern2: &[u8],
                  abort: &AtomicBool, obs: &dyn ScanObserver) -> Vec<ScanResult>; // :949

// ---- the async front-door (thin; replaces ScanEngine QObject) ----
pub struct ScanEngine { ... }         // §7  (owns region cache + abort flag + running flag)
```

`run_scan`/`run_rescan` are **pure free functions** with the region cache passed in (or `ScanEngine` calls them with `&self`-owned cache). Keeping them free + `pub` lets unit tests call them synchronously without any threading — which is exactly what the upstream `syncScan`/`syncRescan` test helpers do (they spin a `QEventLoop` purely to wait for the worker). The Rust tests call `run_scan(...)` directly and skip the event loop entirely (see §9).

---

## 2. ITEM-BY-ITEM C++ → Rust mapping

Format: `C++ symbol (scanner.cpp:line)` → **Rust signature** — behavior / crate.

### 2.1 Free functions (public API)

| C++ | Rust | Notes / crate |
|---|---|---|
| `bool parseSignature(const QString&, QByteArray& pat, QByteArray& mask, QString* err)` (`:128`) | `pub fn parse_signature(input: &str) -> Result<(Vec<u8>, Vec<u8>), String>` | Returns `(pattern, mask)` on `Ok`; the error message string on `Err` matches upstream verbatim. `std` only (`str::split`, `str::trim`). Algorithm in §4.1. |
| `bool serializeValue(ValueType, const QString&, QByteArray& pat, QByteArray& mask, QString* err)` (`:223`) | `pub fn serialize_value(t: ValueType, input: &str) -> Result<(Vec<u8>, Vec<u8>), String>` | Returns `(pattern, all-0xFF mask)`. `std` only (`i32::from_str`, `f32::from_str`, etc.). Algorithm in §4.2. |
| `int naturalAlignment(ValueType)` (`:430`) | `pub fn natural_alignment(t: ValueType) -> i32` | Pure `match`. Keep `i32` return (matches `ScanRequest.alignment` field type). |
| `int valueSizeForType(ValueType)` (`:456`) | `pub fn value_size_for_type(t: ValueType) -> i32` | Pure `match`; `default => 4` covers UTF8/UTF16/HexBytes. Keep `i32`. |
| `static bool ScanEngine::isSystemModule(const QString&)` (`:35`) | `pub fn is_system_module(module_name: &str) -> bool` | Static helper → free fn (no engine state). Hard-coded set + extension-strip loop (§4.3). |
| `static int ScanEngine::bmhFind(const char* data, int len, const char* pat, int patLen)` (`:86`) | `pub fn bmh_find(data: &[u8], pat: &[u8]) -> Option<usize>` | `len`/`patLen` become slice lengths. Return `-1` → `None`; offset → `Some(off)`. `[u8;256]` shift table. (§4.4) |

### 2.2 Internal helpers

| C++ | Rust | Notes |
|---|---|---|
| `static QString formatRegionContext(const MemoryRegion&, uint64_t addr)` (`:22`) | `fn format_region_context(region: &MemoryRegion, address: u64) -> String` | Empty `module_name` → `String::new()`. Else `off = address.saturating_sub(region.base)` and `format!("{}+0x{:x}", region.module_name, off)`. Lowercase hex, no leading zeros, no `0x` padding. **Exact strings the tests assert:** `"code+0x0"`, `"region0+0x4"`, `"Game.exe+0x0"`. |
| `static int hexVal(QChar)` (`:120`) | `fn hex_val(c: u8) -> i32` | `b'0'..=b'9' → c-b'0'`; `b'a'..=b'f' → c-b'a'+10`; `b'A'..=b'F' → +10`; else `-1`. Operate on bytes (ASCII only; inputs are hex/`?` text). For `parse_signature` operate over `input` *chars*: since all valid tokens are ASCII, iterate `s.bytes()` and treat non-ASCII as invalid hex (matches Qt `QChar::unicode()` falling out of the `0-9a-fA-F` ranges → `-1`). |
| `template<typename T> appendLE(QByteArray&, T)` (`:218`) | inline `out.extend_from_slice(&v.to_le_bytes())` | Host is LE on all targets; C++ used host-endian raw bytes ⇒ always LE. Use `to_le_bytes()` explicitly so it is correct even on a (hypothetical) BE host. |
| `static int compareTyped(const QByteArray& a, const QByteArray& b, ValueType)` (`:471`) | `fn compare_typed(a: &[u8], b: &[u8], vt: ValueType) -> i32` | Three-way `-1/0/1`. `sz = min(a.len(), b.len())`. For each numeric type with `sz >= width`, read LE via `T::from_le_bytes(a[..w].try_into().unwrap())` and return `(va>vb) as i32 - (va<vb) as i32`. **Float/Double:** use the same `(va>vb) as i32 - (va<vb) as i32` form so NaN ⇒ `0` (both comparisons false), matching C++ IEEE partial order. **Fallback** (default arm OR not-enough-bytes): byte compare → `match a[..sz].cmp(&b[..sz]) { Less=>-1, Equal=>0, Greater=>1 }` (lexicographic unsigned, == `memcmp` sign). (§4.5) |

### 2.3 `ScanEngine` methods → Rust

| C++ | Rust | Notes |
|---|---|---|
| `ScanEngine(QObject*)` (`:516`) | `ScanEngine::new() -> Self` | Drop `qRegisterMetaType`. Just default the cache/flags. |
| `bool isRunning() const` (`:522`) | `fn is_running(&self) -> bool` | `self.running.load(Acquire)` (an `Arc<AtomicBool>` set while a worker is in flight). The C++ keys on the watcher; we key on a running flag. |
| `void abort()` (`:526`) | `fn abort(&self)` | `self.abort.store(true, Relaxed)`. |
| `void invalidateRegionCache()` (`:113`) | `fn invalidate_region_cache(&mut self)` | `self.cached_regions.clear(); self.cached_provider = None;` (cache key = `*const dyn Provider` data-pointer, or a generation counter — see §7 caveat). |
| `void start(shared_ptr<Provider>, const ScanRequest&)` (`:530`) | `fn start(&mut self, prov: Arc<dyn Provider + Send + Sync>, req: ScanRequest)` | Synchronous validation (emit `error(...)` & return early per §4.6), then spawn worker calling `run_scan`, deliver via `finished`. |
| `void startRescan(...)` (`:919`) | `fn start_rescan(&mut self, prov, results, read_size, condition, value_type, filter_pattern, filter_mask, filter_pattern2)` | Spawn worker → `run_rescan`, deliver via `rescan_finished`. |
| `QVector<ScanResult> runScan(...)` (`:562`) | `pub fn run_scan(...) -> Vec<ScanResult>` (free fn, §4.7) | The big one. |
| `QVector<ScanResult> runRescan(...)` (`:949`) | `pub fn run_rescan(...) -> Vec<ScanResult>` (free fn, §4.8) | |

### 2.4 Qt → crate mapping (the rest are in scanner.md §8 — key ones)

`QByteArray`→`Vec<u8>`/`&[u8]`; `QByteArray(data+i, n)`→`data[i..i+n].to_vec()`; `chunk.mid(off, len)`→`chunk[off..(off+len).min(chunk.len())].to_vec()`. `QString`→`String`/`&str`. `QtConcurrent::run`+`QFutureWatcher`→`std::thread::spawn` (or `rayon::spawn` under feature). `std::atomic<bool>`→`Arc<AtomicBool>`. `QElapsedTimer`→`std::time::Instant`. `QSet<QString>`→a `match`/`HashSet<&'static str>` (see §4.3). Qt signals→`ScanObserver` callbacks + completion channel (§7).

---

## 3. Exact data structures + serde/representation decisions

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(i32)]                       // discriminant order matters for the UI dropdown index
pub enum ValueType {
    Int8 = 0, Int16, Int32, Int64,
    UInt8, UInt16, UInt32, UInt64,
    Float, Double,
    Vec2, Vec3, Vec4,
    UTF8, UTF16,
    HexBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(i32)]
pub enum ScanCondition {
    ExactValue = 0, UnknownValue,
    Changed, Unchanged, Increased, Decreased,
    BiggerThan, SmallerThan, Between,
    IncreasedBy, DecreasedBy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AddressRange { pub start: u64, pub end: u64 }   // end EXCLUSIVE

pub struct ScanRequest {
    pub pattern: Vec<u8>,
    pub mask: Vec<u8>,
    pub filter_executable: bool,        // default false
    pub filter_writable: bool,          // default false
    pub private_only: bool,             // default false
    pub skip_system_modules: bool,      // default false
    pub alignment: i32,                 // default 1
    pub max_results: i32,               // default 50000
    pub condition: ScanCondition,       // default ExactValue
    pub value_size: i32,                // default 4
    pub value_type: ValueType,          // default Int32
    pub pattern2: Vec<u8>,
    pub start_address: u64,             // default 0
    pub end_address: u64,               // default 0
    pub constrain_regions: Vec<AddressRange>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub address: u64,
    pub region_module: String,
    pub scan_value: Vec<u8>,
    pub previous_value: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    pub regions_scanned: i32,
    pub bytes_scanned: u64,
    pub bytes_failed: u64,
    pub ms_elapsed: i32,
}
```

**Serde decisions:**
- `ValueType`/`ScanCondition`: derive `Serialize`/`Deserialize` (used by the UI panel when it persists the last-used scan settings). Use `#[repr(i32)]` and rely on serde's default unit-variant naming (string) — but because the C++ persists the *enum integer* in some panel state, prefer `#[serde(into="i32"/try_from="i32")]` only if a panel JSON test requires the integer form. **Default to string variant names** (idiomatic, human-readable `.rcx`-adjacent settings); revisit only if a panel-state oracle test exists (none is in the scanner test set — `scanResult_jsonShape` only covers `ScanResult` fields, see below).
- `ScanRequest`/`ScanStats`/`AddressRange`: **no serde derive needed** (never serialized — they are transient engine inputs/outputs). Add `Default` impls. `ScanRequest::default()` must reproduce every C++ default exactly (table above). Implement `Default` by hand (not derive) because `alignment=1`, `max_results=50000`, `value_size=4`, `condition=ExactValue`, `value_type=Int32` are non-`Default` values.
- `ScanResult`: **no serde derive on the struct itself.** Its JSON shape is owned by the panel (`ScannerPanel::saveResultsTo`), and the shape is a *custom* projection, not a field-by-field derive. Per `scanResult_jsonShape` (test_scanner.cpp:2809) the panel emits:
  ```json
  { "address": "<lowercase hex of u64, no 0x>",   // QString::number(addr,16)
    "value":   "<lowercase hex of scan_value>",    // bytes → hex
    "module":  "<region_module verbatim>" }
  ```
  In Rust the panel does `format!("{:x}", r.address)`, `hex::encode(&r.scan_value)` (or a hand `iter().map(|b| format!("{:02x}",b))`), and `r.region_module.clone()`. **This is out of the scanner subsystem** but the spec records it so the panel port (UI workflow) preserves it; the scanner test `scanResult_jsonShape` can be ported as a unit test of that projection living next to `ScanResult` (see §9).

**`AddressRange.end` is exclusive** — preserve in all interval math; never use `<=`/`>=` where the C++ uses `<`/`>` on `end`.

---

## 4. Algorithm pseudocode (the tricky logic — port byte-for-byte)

### 4.1 `parse_signature` (scanner.cpp:128-214)
```
trimmed = input.trim()
if trimmed.is_empty(): return Err("Empty pattern")
pattern = []; mask = []
if trimmed.starts_with("\\x"):                 // C-STYLE
    for part in trimmed.split("\\x") where !part.is_empty():
        if part.chars().count() != 2: return Err(format!("Invalid C-style byte: \\x{part}"))
        hi = hex_val(part[0]); lo = hex_val(part[1])
        if hi<0 || lo<0: return Err(format!("Invalid hex char in: \\x{part}"))
        pattern.push((hi<<4 | lo) as u8); mask.push(0xFF)
    return if pattern.is_empty() { Err? } else Ok((pattern,mask))   // C++: returns !pattern.isEmpty()
else if trimmed.contains(' '):                 // SPACE-SEPARATED
    for tok in trimmed.split(' ') where !tok.is_empty():
        if tok=="??" || tok=="?": pattern.push(0); mask.push(0)
        else if tok.len()==2:
            hi=hex_val(tok[0]); lo=hex_val(tok[1])
            if hi<0||lo<0: return Err(format!("Invalid hex byte: {tok}"))
            pattern.push((hi<<4|lo) as u8); mask.push(0xFF)
        else: return Err(format!("Invalid token: {tok} (expected 2 hex chars or wildcards)"))
else:                                          // PACKED
    if trimmed.len() % 2 != 0: return Err("Odd number of characters in packed pattern")
    for i in (0..len).step_by(2):
        c0=trimmed[i]; c1=trimmed[i+1]
        if c0=='?' && c1=='?': pattern.push(0); mask.push(0)
        else:
            hi=hex_val(c0); lo=hex_val(c1)
            if hi<0||lo<0: return Err(format!("Invalid hex chars at position {i}: {c0}{c1}"))
            pattern.push((hi<<4|lo) as u8); mask.push(0xFF)
if pattern.is_empty(): return Err("Empty pattern after parsing")
Ok((pattern, mask))
```
**Edge fidelity:** in space mode `"?"` IS a wildcard; in packed mode only `"??"` is (a lone `'?'` paired with a hex digit fails `hex_val` → invalid). C-style splitting on the literal `"\\x"` with empty-parts skipped means a leading `\x` produces no empty token. `len()` here must mean **char count** (`.chars().count()`) for the `% 2` check and indexing — but since valid input is ASCII, byte length == char count for any input that parses; for non-ASCII input Qt counts UTF-16 code units. Practical port: index by `char` (collect to `Vec<char>`) to mirror `trimmed[i]` semantics and the `part.size()` checks; this keeps the `Invalid C-style byte` message reproducing the offending substring exactly.

### 4.2 `serialize_value` (scanner.cpp:223-428)
```
trimmed = input.trim()
if trimmed.is_empty(): return Err("Empty value")
pattern = []
match t:
  Int8:  v=i32::from_str(trimmed) or Err("Invalid int8 value"); if v<-128||v>127 Err; push (v as i8).to_le_bytes()
  Int16: i32::from_str; range -32768..=32767 else "Invalid int16 value"; push (v as i16) LE
  Int32: i32::from_str else "Invalid int32 value"; push v LE
  Int64: i64::from_str else "Invalid int64 value"; push v LE
  UInt8:  parse_uint_with_hex(trimmed, max=255)  else "Invalid uint8 value"; push (v as u8)
  UInt16: parse_uint_with_hex(trimmed, max=65535) else "Invalid uint16 value"; push (v as u16) LE
  UInt32: parse_u32_with_hex(trimmed) else "Invalid uint32 value"; push v LE
  UInt64: parse_u64_with_hex(trimmed) else "Invalid uint64 value"; push v LE
  Float:  f32::from_str(trimmed) else "Invalid float value"; push v.to_le_bytes()  // 4
  Double: f64::from_str(trimmed) else "Invalid double value"; push v.to_le_bytes() // 8
  Vec2/3/4: parts = trimmed.split_whitespace(); if parts.len()!=N return Err("VecN requires N space-separated floats")
            for p in parts: v=f32::from_str(p) else Err(format!("Invalid float in vecN: {p}")); push v LE
  UTF8:  bytes = trimmed.as_bytes().to_vec(); if empty Err("Empty UTF-8 string"); pattern=bytes  // no NUL
  UTF16: for ch in trimmed.chars(): push (ch as u16... see note) LE; if empty Err("Empty UTF-16 string")
  HexBytes: (pattern,_dummy) = parse_signature(trimmed)?   // delegate; ignore returned mask
// FINAL: mask = vec![0xFF; pattern.len()]   (overwrites HexBytes wildcards to 0xFF too)
Ok((pattern, mask))
```
**`parse_uint_with_hex` (mirrors UInt8/UInt16 `toUInt` path, scanner.cpp:275-301):** try `u32::from_str(trimmed)`; if that fails OR result `> max`, AND `trimmed` (case-insensitive) starts with `0x`, retry `u32::from_str_radix(&trimmed[2..], 16)`; succeed only if parsed and `<= max`. **`parse_u32_with_hex` (UInt32, :302):** try `u32::from_str`; on *failure only* (no `>max` because u32 can't overflow its own width) retry hex if `0x`-prefixed. **`parse_u64_with_hex` (UInt64, :315):** same with `u64`. *Subtle:* C++ `toULong` is 64-bit on Linux but the value is stored into `quint32`/`uint32_t`; the test only checks `"0xDEADBEEF"` which fits u32 — use `u32`/`u64` parsing directly. Decimal base 10 by default (matches Qt).
**UTF16 char note (scanner.cpp:403-413):** C++ iterates `trimmed[i].unicode()` — a UTF-16 *code unit* (`ushort`). For BMP text this equals the `char`. For astral chars Qt would emit surrogate pairs; the tests use only BMP ASCII. Port: encode via `trimmed.encode_utf16()` and push each `u16` LE (this reproduces surrogate behavior too, strictly better and matching for the tested inputs).
**Float fidelity:** `serialize_float`/`serialize_double` round-trip exact `3.14f`/`3.14159`; `f32::from_str("3.14")` gives the identically-rounded `f32` as Qt's `toFloat`, so the byte pattern matches. `serialize_invalidFloat` ("notafloat") → `f32::from_str` errors → `"Invalid float value"`. Note: Rust `f32::from_str` accepts `"inf"`,`"nan"` — Qt `toFloat` does NOT for those exact spellings in all cases; the tests do not exercise inf/nan, so the default `from_str` is acceptable. Trailing garbage ("3.14x") is rejected by both.

### 4.3 `is_system_module` (scanner.cpp:35-79) — **port the table verbatim**
```
if module_name.is_empty(): return false
name = module_name.trim().to_lowercase()
stem = name.clone()
loop:
    dot = stem.find('.')            // first '.' ; C++ uses indexOf, and requires dot>0
    match dot { Some(d) if d>0 => {
        suffix = &stem[d+1..]
        is_stripable = suffix=="dll"||suffix=="exe"||suffix=="dylib"||suffix=="so"
                    || (suffix.chars().count() <= 3 && parse_int(suffix) > 0)
        if is_stripable { stem = stem[..d].to_string(); continue }
        else break
    } _ => break }
K_SYSTEM.contains(stem.as_str()) || K_SYSTEM.contains(name.as_str())
```
- `K_SYSTEM`: a `static` `HashSet<&'static str>` (or a `phf::phf_set!` / a `matches!` macro) containing **all entries from scanner.cpp:37-61 verbatim** — Windows core, Qt6, Linux, macOS lists (copy the exact 60-ish strings from scanner.md §5 or the source; do not abbreviate). Build once with `std::sync::OnceLock<HashSet<&str>>`.
- `parse_int(suffix)`: mirror Qt `QString::toInt()` returning 0 on non-numeric: `suffix.parse::<i32>().unwrap_or(0)`. So `.6` (len 1, parses to 6>0) strips; `.so` (len 2, parses to 0) does not strip via the numeric arm but matches the explicit `"so"` arm; `.exe` strips. `game_x64.exe` → strip `.exe` → `game_x64` (not in set) → **false** (test `sysmod_userBinaryNotSystem`).
- Both `stem` and the full lowercased `name` are checked — this is how multi-dot entries (`libc.so.6`, `ld-linux-x86-64.so`, `libc++.1.dylib`) match via the `name` branch even when stripping reduces `stem`. *Port both checks.*

### 4.4 `bmh_find` (scanner.cpp:86-111)
```
fn bmh_find(data: &[u8], pat: &[u8]) -> Option<usize> {
    let (len, plen) = (data.len(), pat.len());
    if plen == 0 || plen > len { return None }            // C++: patLen<=0 || patLen>len
    if plen == 1 { return data.iter().position(|&b| b == pat[0]); }  // memchr equiv
    let mut shift = [plen; 256];                          // all init to patLen
    for i in 0..plen-1 { shift[pat[i] as usize] = plen-1-i; }  // last-occ wins, FINAL byte excluded
    let last = plen - 1;
    let mut i = 0usize;
    while i <= len - plen {
        let tail = data[i + last];
        if tail == pat[last] {
            let mut j = 0;
            while j < last && data[i+j] == pat[j] { j += 1; }
            if j == last { return Some(i); }
        }
        i += shift[tail as usize];
    }
    None
}
```
**Critical fidelity:** the bad-char table is built over `pat[0..plen-1]` (final byte excluded); on ties the *last* occurrence among those wins. The shift is keyed on the **text tail byte** `data[i+last]`. `i += shift[tail]` is `>=1` always (shift values are 1..=plen). The property test `bmh_equivalentToNaive` requires identical first-match offsets vs a naive scan over random data, patterns len 4..16 — keep this exact.

### 4.5 `compare_typed` (scanner.cpp:471-512) — see §2.2. Pseudocode:
```
sz = min(a.len(), b.len())
macro cmp_as(T, w): if sz>=w { let va=T::from_le_bytes(a[..w]); let vb=T::from_le_bytes(b[..w]);
                              return (va>vb) as i32 - (va<vb) as i32 }
match vt {
  Int8 => cmp_as(i8,1), UInt8 => cmp_as(u8,1),
  Int16 => cmp_as(i16,2), UInt16 => cmp_as(u16,2),
  Int32 => cmp_as(i32,4), UInt32 => cmp_as(u32,4),
  Int64 => cmp_as(i64,8), UInt64 => cmp_as(u64,8),
  Float => cmp_as(f32,4), Double => cmp_as(f64,8),
  _ => {}  // Vec*/UTF*/HexBytes fall through
}
// fallback (also when not enough bytes for the typed read):
match a[..sz].cmp(&b[..sz]) { Less => -1, Equal => 0, Greater => 1 }
```
Note the `cmp_as` arms `return` only when `sz>=w`; otherwise control falls to the byte fallback — this matches the C++ `if (sz>=N) {...; return ...;} break;` shape where the missing-bytes case drops to `memcmp(da,db,sz)`.

### 4.6 `start` validation (scanner.cpp:530-542)
```
if self.is_running(): return                  // silent no-op
if req.condition != UnknownValue {
    if req.pattern.is_empty(): obs.error("Empty pattern"); return
    if req.pattern.len() != req.mask.len(): obs.error("Pattern and mask size mismatch"); return
}
self.abort.store(false); self.running.store(true)
spawn worker -> run_scan(prov, req, abort, obs) -> obs.finished(results); self.running=false
```
**`UnknownValue` bypasses pattern/mask validation entirely.** Errors are delivered synchronously *before* spawning (tests `scan_emptyPattern`, `scan_maskSizeMismatch` assert an `error` signal and zero results / no `finished`).

### 4.7 `run_scan` (scanner.cpp:562-917) — first-scan kernel
Implement exactly per scanner.md §6 (it is precise). Key ordered steps, all parity-critical:

```
timer = Instant::now(); results = Vec::new()
cond = req.condition
is_capture = cond in {UnknownValue,Changed,Unchanged,Increased,Decreased,IncreasedBy,DecreasedBy}
is_typed_const = cond in {BiggerThan,SmallerThan,Between}
// early returns:
if !prov_valid: return []          (Rust: prov is &dyn, always present; instead mirror by checking nothing — the C++ `!prov` guard is for the shared_ptr; drop it)
if !is_capture && !is_typed_const && req.pattern.is_empty(): return []
if is_typed_const && req.pattern.is_empty(): return []

regions = region_cache_or_enumerate(prov)        // §7 cache
if regions.is_empty():
    regions = [ MemoryRegion{ base:0, size: prov.size(), readable:true, writable:true, executable:true, region_type: Private, module_name:"" } ]

pattern_len = if is_capture||is_typed_const { req.value_size } else { req.pattern.len() as i32 }
pat = if is_capture { &[] } else { &req.pattern[..] }
msk = if is_capture { &[] } else { &req.mask[..] }
alignment = max(1, req.alignment)
val_size = if is_capture||is_typed_const { req.value_size } else { pattern_len }
has_range = (req.start_address!=0 || req.end_address!=0) && req.end_address > req.start_address
bmh_eligible = !is_capture && pattern_len>=4 && alignment==1
               && msk.iter().take(pattern_len as usize).all(|&m| m==0xFF)

if !req.constrain_regions.is_empty(): regions = intersect_constraints(regions, &req.constrain_regions)  // §4.7a

region_accepted = |r| !(req.filter_executable && !r.executable)
                   && !(req.filter_writable && !r.writable)
                   && !(req.private_only && r.region_type != Private)
                   && !(req.skip_system_modules && is_system_module(&r.module_name))

// pre-pass
total_bytes=0; accepted_regions=0
for r in &regions where region_accepted(r):
    (rs, re) = clip(r.base, r.base+r.size, has_range, req)   // skip if entirely outside
    total_bytes += re-rs; accepted_regions += 1
obs.regions_resolved(accepted_regions, total_bytes)
if total_bytes == 0: return []     // also emits no progress

scanned_bytes=0; failed_bytes=0; last_pct=-1
K_CHUNK_BIG = 2*1024*1024; K_CHUNK_MIN = 64*1024; K_ABORT_STRIDE = 4096

'outer: for region in &regions:
    if abort.load(): break
    if !region_accepted(region): continue
    (reg_start, reg_end) = clip(...); if outside: continue
    reg_size = reg_end - reg_start; if reg_size==0: continue
    if pattern_len as u64 > reg_size: scanned_bytes += reg_size; continue
    overlap = pattern_len - 1
    target_chunk = min(K_CHUNK_BIG, reg_size); if reg_size < K_CHUNK_MIN { target_chunk = reg_size }
    chunk = vec![0u8; target_chunk as usize]
    off = 0u64
    while off < reg_size:
        if abort.load(): break
        remaining = reg_size - off
        read_len = min(chunk.len() as u64, remaining) as i32
        if !prov.read(reg_start+off, &mut chunk[..read_len as usize]):
            failed_bytes += read_len; scanned_bytes += read_len; off += read_len; continue
        scan_end = read_len - pattern_len           // last valid match start (can be <0)
        data = &chunk[..read_len as usize]
        // ----- four paths (mutually exclusive) -----
        if is_capture: §4.7b
        else if is_typed_const: §4.7c
        else if bmh_eligible: §4.7d
        else: §4.7e (naive masked)
        // ----- advance -----
        advance = if read_len as u64 >= remaining { remaining }
                  else if read_len > overlap {
                      let mut adv = (read_len - overlap) as u64;
                      if alignment > 1 {
                          let next_off = off + adv;
                          let aligned = ((next_off + alignment as u64 - 1)/alignment as u64)*alignment as u64;
                          adv = aligned - off;
                      }
                      adv
                  } else { 1 };
        scanned_bytes += advance; off += advance
        pct = min(100, (scanned_bytes*100/total_bytes) as i32)
        if pct != last_pct { last_pct = pct; obs.progress(pct) }
// 'done label target — Rust: use labeled break 'outer or a flag; emit stats then return
emit_stats: ScanStats{ regions_scanned: accepted_regions, bytes_scanned, bytes_failed, ms_elapsed: timer.elapsed().as_millis() as i32 }; obs.scan_stats(stats)
return results
```
The C++ uses `goto done` to break out of the doubly-nested loop on `max_results`/abort while still emitting stats. In Rust, model with a labeled loop and a small inner helper closure, OR return early from a closure that the outer code wraps to still emit stats. **Recommended:** put the scan body in a closure/inner-fn that returns `results` (and a `bool reached_cap`), then emit stats after — keeping the single stats-emit point. The cleanest is a labeled `'scan:` loop with `break 'scan` replacing every `goto done`.

**§4.7a `intersect_constraints` (scanner.cpp:642-674):**
```
constraints = req.constrain_regions.clone(); constraints.sort_by_key(|c| c.start)
merged: Vec<AddressRange> = []
for c in constraints:
    if c.end <= c.start: continue                 // degenerate
    if let Some(last) = merged.last_mut() && c.start <= last.end { last.end = max(last.end, c.end) }
    else { merged.push(c) }
clipped = []
for region in &regions:
    r_end = region.base + region.size
    for c in &merged:
        if c.end <= region.base || c.start >= r_end: continue
        i_start = max(region.base, c.start); i_end = min(r_end, c.end)
        if i_end <= i_start: continue
        let mut sub = region.clone(); sub.base = i_start; sub.size = i_end - i_start; clipped.push(sub)
regions = clipped
```

**§4.7b Capture path (scanner.cpp:777-792):**
```
let mut i = 0i32
while i <= scan_end {
    if (i & (K_ABORT_STRIDE-1))==0 && abort.load() { break 'scan }
    let addr = reg_start + off + i as u64
    results.push(ScanResult{ address: addr,
        region_module: format_region_context(region, addr),
        scan_value: data[i as usize .. i as usize + val_size as usize].to_vec(),
        previous_value: vec![] })
    if results.len() as i32 >= req.max_results { break 'scan }
    i += alignment
}
```
*`val_size` (=`req.value_size`) bytes cached* — this is what the rescan reads back. `scan_end` may be `< 0` (region too small handled earlier, but a partial last chunk can yield scan_end<0); the `while i<=scan_end` with `i=0` then naturally does nothing when `scan_end<0` — match by using signed `i32` for `i` and `scan_end`.

**§4.7c Typed-const path (scanner.cpp:793-820):**
```
lo = &req.pattern; hi = &req.pattern2
let mut i=0i32
while i <= scan_end {
    if (i & (K_ABORT_STRIDE-1))==0 && abort.load() { break 'scan }
    let val = &data[i as usize .. i as usize + val_size as usize]
    let cmp_lo = compare_typed(val, lo, req.value_type)
    let ok = match cond {
        BiggerThan => cmp_lo > 0,
        SmallerThan => cmp_lo < 0,
        Between if !hi.is_empty() => cmp_lo >= 0 && compare_typed(val, hi, req.value_type) <= 0,
        _ => false }
    if ok { results.push(... scan_value: val.to_vec() ...); if cap break 'scan }
    i += alignment
}
```
Between with empty `pattern2` never matches (the `!hi.is_empty()` guard); lo==hi gives exact equality (test `condition_between` "100..100"→1).

**§4.7d BMH path (scanner.cpp:821-839):**
```
let mut search_from = 0i32
while search_from <= scan_end {
    if abort.load() { break 'scan }
    match bmh_find(&data[search_from as usize..read_len as usize], &pat[..pattern_len as usize]) {
        None => break,
        Some(hit) => {
            let abs_i = search_from + hit as i32
            if abs_i > scan_end { break }
            let n = min(16, read_len - abs_i) as usize
            results.push(ScanResult{ address: reg_start+off+abs_i as u64,
                region_module: format_region_context(region, addr),
                scan_value: data[abs_i as usize .. abs_i as usize + n].to_vec(), previous_value: vec![] })
            if cap { break 'scan }
            search_from = abs_i + 1     // overlapping matches
        }
    }
}
```
**`scan_value` here is `min(16, read_len-abs_i)` bytes — NOT `val_size`.**

**§4.7e Naive masked path (scanner.cpp:840-862):**
```
let mut i=0i32
while i <= scan_end {
    if (i & (K_ABORT_STRIDE-1))==0 && abort.load() { break 'scan }
    let m = (0..pattern_len as usize).all(|j| (data[i as usize+j] & msk[j]) == (pat[j] & msk[j]))
    if m {
        let n = min(16, read_len - i) as usize
        results.push(... scan_value: data[i..i+n].to_vec() ...)
        if cap { break 'scan }
    }
    i += alignment
}
```
Handles wildcards (mask 0x00 bytes) and alignment>1. `scan_value` = `min(16, read_len-i)` bytes.

### 4.8 `run_rescan` (scanner.cpp:949-1136) — refine kernel
```
timer = Instant::now(); total = results.len()
if total==0 { return results }      // (no prov check needed; &dyn always present)
has_exact = !filter_pattern.is_empty() && condition==ExactValue
has_comparison = condition in {Changed,Unchanged,Increased,Decreased}
has_typed_const = condition in {BiggerThan,SmallerThan,Between}
has_delta = condition in {IncreasedBy,DecreasedBy}
needs_filter = has_exact||has_comparison||has_typed_const||has_delta

for r in &mut results { r.previous_value = r.scan_value.clone() }    // baseline snapshot

order: Vec<usize> = (0..total).collect()
order.sort_by_key(|&i| results[i].address)    // stable; C++ uses std::sort but addresses are unique-enough; use sort_by + stable
matched = vec![!needs_filter; total]          // unfiltered → all true

K_CHUNK = 256*1024; updated=0; last_pct=-1; i=0
while i < total && !abort.load():
    span_base = results[order[i]].address
    span_end = i
    while span_end+1 < total {
        end_addr = results[order[span_end+1]].address + read_size as u64
        if end_addr - span_base > K_CHUNK { break }
        span_end += 1
    }
    span_last = results[order[span_end]].address
    chunk_len = (span_last + read_size as u64 - span_base) as usize
    chunk = vec![0u8; chunk_len]
    prov.read(span_base, &mut chunk[..])      // RETURN VALUE IGNORED — failed reads leave zeros
    for j in i..=span_end {
        idx = order[j]; off = (results[idx].address - span_base) as usize
        results[idx].scan_value = chunk[off .. (off+read_size as usize).min(chunk_len)].to_vec()
        // (C++ chunk.mid(off,readSize): clamps at chunk end if truncated)
        if has_exact: apply_exact_filter(&mut matched[idx], &results[idx].scan_value, filter_pattern, filter_mask)
        if has_comparison && !results[idx].previous_value.is_empty():
            cmp = compare_typed(&results[idx].scan_value, &results[idx].previous_value, value_type)
            matched[idx] = match condition { Changed=>cmp!=0, Unchanged=>cmp==0, Increased=>cmp>0, Decreased=>cmp<0, _=>matched[idx] }
        if has_typed_const && !filter_pattern.is_empty():
            cmp_lo = compare_typed(&sv, filter_pattern, value_type)
            matched[idx] = match condition { BiggerThan=>cmp_lo>0, SmallerThan=>cmp_lo<0,
                Between if !filter_pattern2.is_empty() => cmp_lo>=0 && compare_typed(&sv,filter_pattern2,value_type)<=0,
                _ => matched[idx] }
        if has_delta && !prev.is_empty() && !filter_pattern.is_empty():
            sz = min(prev.len(), filter_pattern.len())
            if sv.len() >= sz: matched[idx] = delta_check(value_type, condition, prev, filter_pattern, sv, sz)
    updated += span_end - i + 1; i = span_end + 1
    pct = (updated*100/total) as i32; if pct!=last_pct { last_pct=pct; obs.progress(pct) }
if needs_filter:
    return results.into_iter().enumerate().filter(|(k,_)| matched[*k]).map(|(_,r)| r).collect()
else:
    return results
```
**`delta_check` (scanner.cpp:1069-1102) — wrapping/IEEE arithmetic, exact ==:**
```
macro check(T): if sz >= size_of::<T>() {
    prev = T::from_le_bytes(prev[..w]); delta = T::from_le_bytes(filter_pattern[..w]); cur = T::from_le_bytes(sv[..w])
    expected = if condition==IncreasedBy { prev.wrapping_add(delta) } else { prev.wrapping_sub(delta) }   // ints
    // floats: prev+delta / prev-delta (IEEE), compare with ==
    return cur == expected
}
match value_type { Int8=>check(i8), UInt8=>check(u8), Int16=>check(i16), UInt16=>check(u16),
  Int32=>check(i32), UInt32=>check(u32), Int64=>check(i64), UInt64=>check(u64),
  Float=>{f32 add/sub, ==}, Double=>{f64 add/sub, ==}, _=>false }
```
Integers: `wrapping_add`/`wrapping_sub` (C++ `T(prev+delta)` is two's-complement wrap). Floats: ordinary `+`/`-` then bitwise-exact `==` (NaN never equals, matching C++ `cur==expected`). The default arm leaves `matched[idx]` unchanged for unsupported value types (vec/string) — but note C++ sets `matched[idx]=ok` only inside the `if (sv.size()>=sz)` block AFTER the switch; if the switch hit `default` (no-op), `ok` stays `false` → `matched[idx]=false`. **Port:** initialize `ok=false`, run switch, then `matched[idx]=ok` (so an unsupported value_type with delta condition drops the row).

**`apply_exact_filter` (scanner.cpp:1022-1037):** if `sv.len() >= filter_pattern.len()`, `ok = (0..filter_pattern.len()).all(|k| (sv[k]&filter_mask[k]) == (filter_pattern[k]&filter_mask[k]))`; `matched[idx]=ok`. If `sv.len() < filter_pattern.len()`, leave `matched[idx]` at its default (`false` since needs_filter) — i.e. row dropped.

**Rescan subtleties to preserve:** (1) `read` return ignored → never aborts on unreadable address (compares against zeros). (2) `read_size` is caller-supplied, independent of the original pattern length. (3) `previous_value` is set to the prior `scan_value` *before* re-reading, so the comparison baseline is the prior cached bytes. (4) Empty seed → returns empty. (5) `needs_filter==false` (ExactValue with empty filter, or any non-filter condition) → all rows updated, none dropped, return as-is.

---

## 5. Data / serialization formats

The engine has **no on-disk format**. The only serialized artifact is the panel's per-result JSON (`scanResult_jsonShape`, test_scanner.cpp:2809), documented in §3 under `ScanResult` — owned by the UI panel, not this module. Pattern/value *input* formats (IDA `"48 8B ?? 05"`, packed `"488B??05"`, C-style `"\x48\x8B"`, per-type numeric/vector/string literals) are produced by `parse_signature`/`serialize_value` (§4.1/§4.2). No serde on engine structs (§3).

---

## 6. Error-handling strategy

- **`parse_signature`/`serialize_value`:** return `Result<(Vec<u8>, Vec<u8>), String>`. The `Err(String)` is the **exact upstream message** (the panel surfaces it to the user; tests assert specific strings). Do **not** use `thiserror`/typed enums — the strings are the contract. List of exact messages to reproduce (from §4.1/§4.2): `"Empty pattern"`, `"Invalid C-style byte: \\x{part}"`, `"Invalid hex char in: \\x{part}"`, `"Invalid hex byte: {tok}"`, `"Invalid token: {tok} (expected 2 hex chars or wildcards)"`, `"Odd number of characters in packed pattern"`, `"Invalid hex chars at position {i}: {c0}{c1}"`, `"Empty pattern after parsing"`, `"Empty value"`, `"Invalid int8 value"`..`"Invalid uint64 value"`, `"Invalid float value"`, `"Invalid double value"`, `"Vec{N} requires {N} space-separated floats"`, `"Invalid float in vec{N}: {p}"`, `"Empty UTF-8 string"`, `"Empty UTF-16 string"`.
- **`start`/`start_rescan` validation:** deliver via `obs.error(String)` synchronously before spawning (`"Empty pattern"`, `"Pattern and mask size mismatch"`), then return without starting a worker. `UnknownValue` skips validation.
- **Provider read failures:** never fatal. In `run_scan` an unreadable chunk is skipped and counted in `ScanStats.bytes_failed` (scan continues, `finished` still fires). In `run_rescan` the read return is ignored entirely (zero-filled bytes). No `Result` propagation out of the kernels — they always return a `Vec<ScanResult>`.
- **No panics:** all slice indexing is bounds-guarded by the precomputed `scan_end`/`read_len`/`val_size`; assert in debug via `debug_assert!` but never index out of range in release. Use `usize` for slice indices but keep the *iteration* counters as `i32`/`u64` to mirror C++ overflow/sign semantics exactly (notably `scan_end = read_len - pattern_len` can be negative; the `while i<=scan_end` over `i32` is the parity-safe form).

---

## 7. Concurrency / `ScanEngine` / observer

```rust
pub trait ScanObserver: Send + Sync {
    fn progress(&self, _percent: i32) {}
    fn regions_resolved(&self, _count: i32, _total_bytes: u64) {}
    fn scan_stats(&self, _stats: ScanStats) {}
    fn error(&self, _message: &str) {}
    fn finished(&self, _results: &[ScanResult]) {}        // delivered by ScanEngine after run_scan
    fn rescan_finished(&self, _results: &[ScanResult]) {}
}
pub struct NullObserver;          // all default no-ops — used by every unit test that calls run_scan directly
impl ScanObserver for NullObserver {}
```
- The six Qt signals (`progress`, `finished`, `rescanFinished`, `error`, `scanStats`, `regionsResolved`) map to `ScanObserver` methods. The kernels (`run_scan`/`run_rescan`) call `obs.progress/regions_resolved/scan_stats/error` *during* the scan; `ScanEngine` calls `obs.finished`/`rescan_finished` after the worker returns (mirroring `QFutureWatcher::finished`). The UI layer's observer marshals these onto the gpui main thread via a channel; the logic tests use `NullObserver` or a counting test observer (§9).
- **`ScanEngine` fields:** `abort: Arc<AtomicBool>`, `running: Arc<AtomicBool>`, `cached_regions: Vec<MemoryRegion>`, `cached_provider: Option<*const ()>` (the raw data-pointer of the last `Arc<dyn Provider>` used as the cache key — `Arc::as_ptr(&prov) as *const ()`). Because a raw pointer is not `Send`, prefer instead a **generation counter** or compare by `Arc::ptr_eq` against a stored `Weak<dyn Provider>`; the simplest parity-faithful choice: store `cached_provider_ptr: usize = Arc::as_ptr(&prov) as *const () as usize` (a plain `usize` is `Send`) and compare equality. Document that this matches the C++ `m_cachedProvider == prov.get()` keying. `invalidate_region_cache()` clears the vec and zeroes the key.
- **Single-scan-at-a-time:** `start`/`start_rescan` early-return if `running` is true. The region cache is touched only by the worker → safe under this invariant (no lock), exactly like the C++ `mutable` cache.
- **Abort cadence:** poll `abort.load(Relaxed)` at every region top, every chunk top, every `K_ABORT_STRIDE` (4096) inner iterations (capture/typed/naive), and every BMH iteration. `abort()` flips the flag; the in-flight scan returns a partial result set and `finished` still fires (test `scan_abort`).
- **Worker:** `std::thread::spawn` moving `Arc<dyn Provider+Send+Sync>`, `req`, `Arc<AtomicBool>` abort, and a cloned observer handle (`Arc<dyn ScanObserver>`); on completion call `obs.finished(&results)` and clear `running`. Under feature `scanner-parallel`, the *single scan* may parallelize across regions with `rayon` — but **NOT in the initial port**: the result ordering and `max_results` post-push cap are order-sensitive (results must come out region-by-region, position-ascending) so the first faithful port runs the kernel single-threaded on one worker thread. Defer rayon region-parallelism to a later optimization (and only if results are merged in deterministic region/offset order). For now `scanner-parallel` only gates the worker pool used to *not block* the caller; the kernel itself stays sequential.
- **Region cache passing into free `run_scan`:** since `run_scan` is a free fn, the cache lives in `ScanEngine`. Two options: (a) `run_scan` takes `regions: &[MemoryRegion]` already-enumerated (engine does the cache + enumerate, passes the slice in) — **preferred**, cleanest, and makes the cache test trivial; or (b) pass `&mut Option<(usize,Vec<MemoryRegion>)>` cache cell. Choose (a): `ScanEngine` resolves regions (cache hit or `prov.enumerate_regions()`), then calls a kernel `run_scan_in_regions(prov, regions, req, abort, obs)`. The public `run_scan(prov, req, abort, obs)` convenience enumerates fresh each call (used by tests that don't care about caching). The cache-reuse test (`regionCache_reusesAcrossScans`) drives `ScanEngine` directly.

---

## 8. Qt → crate notes recap (delta from scanner.md §8)

All in scanner.md §8; the load-bearing ones for *this* port: `QByteArray`→`Vec<u8>`; `QByteArray(p,n)`→`slice.to_vec()`; `chunk.mid(off,len)`→`chunk[off..(off+len).min(chunk.len())].to_vec()`; `QString::number(x,16)`→`format!("{:x}", x)`; `QSet`→`OnceLock<HashSet<&'static str>>`; `QtConcurrent::run`+`QFutureWatcher`→`thread::spawn`+observer/channel; `std::atomic<bool>`→`Arc<AtomicBool>`; `QElapsedTimer`→`Instant`; `std::memchr`→`slice::iter().position` (or optional `memchr` crate); `memcpy`→`copy_from_slice`/`from_le_bytes`; `memcmp` sign→`slice::cmp`.

---

## 9. TEST PLAN — C++ tests → Rust `#[test]`

All scanner tests live in two C++ files and are GREEN in the oracle (`test_scanner` 163/0/0, `test_scanner_combinations` 68/0/0). There is **no golden text fixture** — the assertions ARE the oracle. Translate each into `#[test]` (or `#[rstest]`/loop for the data-driven ones) under `src/scanner.rs` `#[cfg(test)] mod tests` (pure-fn + direct-kernel tests) and `tests/scanner_integration.rs` (engine async-path tests). Run with `cargo test --no-default-features` (no gpui/rayon). The two upstream helpers `syncScan`/`syncRescan` (event-loop wrappers) become **direct calls to `run_scan`/`run_rescan` with `NullObserver`** — no event loop. The `ScanEngine` async path is exercised by a handful of tests that need the `finished`/`error`/`progress` signals (`scan_exactMatch`-style + `scan_abort` + `scan_progressEmitted` + `scanStats_emitted` + `scan_isRunning` + the validation tests).

**Test provider:** port `RegionProvider`/`SyntheticProvider` (a `BufferProvider` subclass returning fixed `enumerate_regions()`) as a small `struct TestRegionProvider { data: Vec<u8>, regions: Vec<MemoryRegion> }` impl `Provider`. Port `MutableProvider` (writable buffer behind `Arc<Mutex<Vec<u8>>>` or `RefCell`-free `Arc<Mutex>`) for the IncreasedBy/DecreasedBy/e2e mutate tests.

### 9.1 Pure parsing/serialization/helper tests (call free fns directly)
Map 1:1, asserting returned bytes/mask and (for failures) the exact `Err` string:

| C++ test (test_scanner.cpp) | Rust `#[test]` assertion |
|---|---|
| `parse_emptyPattern`, `parse_spacesOnly` | `parse_signature("")`/`("   ")` → `Err("Empty pattern")` |
| `parse_singleByte` ("AB") | → `Ok(([0xAB],[0xFF]))` |
| `parse_spaceSeparated`, `parse_withWildcards`, `parse_singleQuestionMark`, `parse_allWildcards`, `parse_leadingTrailingSpaces`, `parse_lowercaseHex`, `parse_mixedCase` | pattern+mask bytes per §4.1; wildcards → `(0x00,0x00)` |
| `parse_packedNoSpaces`, `parse_oddCharsNoSpaces` | packed parse; odd → `Err("Odd number of characters in packed pattern")` |
| `parse_cStyle` | `"\x48\x8B"` → `Ok(([0x48,0x8B],[0xFF,0xFF]))` |
| `parse_invalidHex`, `parse_invalidTokenWidth` | exact `Err` strings |
| `serialize_int8`,`_int8_overflow`,`_int16`,`_int32`,`_int32_negative`,`_int64` | LE bytes; overflow → `Err("Invalid int8 value")`; `serialize_invalidInt` ("notanumber")→`Err("Invalid int32 value")` |
| `serialize_uint8`,`_uint8_hex`,`_uint16`,`_uint32`,`_uint64` | decimal + `0x` hex fallback; `"0xDEADBEEF"`→`[ef,be,ad,de]`; `"0xCAFEBABEDEADBEEF"`→8 LE bytes |
| `serialize_float`,`_double`,`serialize_invalidFloat` | `"3.14"`→`3.14f32.to_le_bytes()`; `"notafloat"`→`Err("Invalid float value")` |
| `serialize_vec2`,`_vec3`,`_vec3_wrongCount`,`_vec4` | N consecutive LE f32; wrong count → `Err("Vec3 requires 3 space-separated floats")` |
| `serialize_utf8`,`_utf16` | UTF-8 bytes / UTF-16LE u16s, no terminator |
| `serialize_hexBytes` ("DE AD BE EF") | → `Ok(([de,ad,be,ef],[ff,ff,ff,ff]))` (mask forced 0xFF) |
| `serialize_emptyValue` | `Err("Empty value")` |
| `alignment_int8/16/32/64/float/double/vec3/utf8/utf16` | `natural_alignment(t)` == 1/2/4/8/4/8/4/1/2 |
| `sysmod_emptyName`,`_kernel32WithExt`,`_kernel32NoExt`,`_caseInsensitive`,`_qtCore`,`_crt`,`_userBinaryNotSystem`,`_linuxSo` | `is_system_module(...)` == expected; **incl. `game_x64.exe`→false** |
| `bmh_singleByte`,`_longPattern`,`_patternEqualsLength`,`_patternLargerThanData`,`_atEnd` | `bmh_find(data,pat)` == `Some(off)`/`None` per case |
| `bmh_equivalentToNaive` | property test: random `data` (seeded RNG for determinism), patterns len 4..16, assert `bmh_find` first offset == a hand naive first-match over all `data` positions |

(`value_size_for_type` has no dedicated `value_*` test but is covered indirectly by the combinations matrix; add a direct unit test asserting 1/2/4/8/8/12/16/4 anyway.)

### 9.2 First-scan tests (call `run_scan(prov, &req, &abort, &NullObserver)` directly)

| C++ test | Rust assertion |
|---|---|
| `scan_exactMatch` | `"\x22\x33"` in 8-byte buf → 1 result, addr 2 |
| `scan_wildcardMatch` | `48 8B ?? 05` → 2 results, addr 0 & 8 |
| `scan_noMatch` | 0 results |
| `scan_alignment4`, `scan_alignment4_skipsUnaligned`, `scan_alignment8_skipsUnaligned`, `scan_alignment2_findsAligned_skipsUnaligned` | only aligned matches counted |
| `scan_maxResults` | 1000×0xAA, 1-byte pat, cap 10 → exactly 10 |
| `scan_emptyProvider`, `provider_nullProviderRegionsEmpty` | size 0 → 0 results |
| `scan_emptyPattern`, `scan_maskSizeMismatch` | via `ScanEngine.start` → `error` signal, no `finished` (engine test, §9.4) |
| `scan_chunkBoundaryOverlap` | 4-byte pat at `256KiB-2` → found (overlap carry) |
| `scan_multipleMatches`, `scan_singleBytePattern`, `scan_overlappingMatches` ("AA AA" in "AA AA AA"→0&1), `scan_allWildcardPattern` (mask all 0, 2-byte pat in 8-byte buf → 7) | counts/addresses |
| `scan_patternLargerThanData`, `scan_patternExactSize`, `scan_atEndOfBuffer`, `scan_oneByteBuffer` | edge sizes |
| `scan_filterExecutable`, `scan_filterWritable`, `scan_bothFilters` (AND) | region filters via `TestRegionProvider` |
| `scan_regionModuleName` | `region_module == "code+0x0"` etc. (exact string) |
| `scan_findFloatValue`, `scan_float_atRegionStart/End`, `scan_double_atRegionEnd`, `scan_pattern_atRegionStart/End`, `scan_pattern_withWildcard_atRegionEnd`, `scan_pattern_multiplePositions_inConstrainedRegion` | float/pattern at boundaries |
| `provider_defaultRegionsEmpty`, `provider_customRegions` | buffer synthetic region vs custom regions |
| `scan_multipleRegions` | matches across 2 regions |
| `scan_addressRangeNoLimit`, `_addressRangeClipsResults`, `_addressRangeOutsideData`, `_addressRangeWithRegions`, `scan_unknownWithAddressRange` (range[8,24) align4 valSize4 → 4) | range clipping; `hasRange` needs `end>start` |
| `scan_constrainRegions_*` (all ~30 variants: multipleRanges, intersects, noOverlap, gapBetween, partialOverlap, mixedModuleAndAnon, fallbackProvider, adjacent, writableFilterPreserved, extendsBeforeAndAfter, emptyConstraintScansAll, singleAddressRange, withStartEndAddress, unknownValueScan, nonZeroBase, zeroSizeConstraint→0, invertedRange→0, overlappingConstraints {4,20}+{12,28}→{4,28}, patternAtFirst/LastByte, patternOneByteAfterEnd→0, regionSmallerThanPattern→0, patternExactlyFitsRegion→1, matchAtRegionBoundaries, multibyteAtClipBoundary→0) | port each result-count assertion; these pin the §4.7a intersection/merge logic |
| `regionType_privateOnly_skipsImage/keepsPrivate/skipsMapped` | `private_only` keys on `RegionType` |
| `skipSystem_excludesNtdll`, `skipSystem_inactiveByDefault`, `skipSystem_combinesWithPrivateOnly` | skip-system compounding |
| `addressCap_clipsAboveLimit` | end_address cap |
| `adaptiveChunk_largeRegion` | 3 MiB region, hit near end → found (multi-chunk) |
| `bmh_pathParity` | same buf/pat scanned with mask all-FF (BMH) vs mask[0]=0x00 (naive) → equal count |
| `condition_biggerThan_firstScan`, `condition_smallerThan_firstScan`, `condition_between_firstScan` | typed-const inline filter result counts |

### 9.3 Rescan tests (call `run_rescan(...)` directly, with `MutableProvider`)

| C++ test | Rust assertion |
|---|---|
| `rescan_emptySeed` | empty seed → empty (no-op) |
| `condition_increasedBy_rescan` | seed 4 via UnknownValue(int32,align4,valSize4); mutate off0 +5; rescan IncreasedBy delta=5 → 1 result, addr 0 |
| `condition_decreasedBy_rescan` | seed 2; mutate off0 -7; rescan DecreasedBy delta=7 → 1, addr 0 |
| `e2e_findMutateRevalidate` | full flow: ExactValue scan → find → write → Changed rescan flags → ExactValue-new rescan → 1 match same addr; plus Increased/Decreased steps (port each sub-assert) |
| `condition_between` (data-driven "100..100"→1) | Between with lo==hi exact equality (combinations file, §9.5) |

### 9.4 Engine async-path tests (`ScanEngine` + a counting `TestObserver`)
A `TestObserver { progress: Mutex<Vec<i32>>, finished: Mutex<Option<Vec<ScanResult>>>, error: Mutex<Vec<String>>, stats: Mutex<Option<ScanStats>>, regions_resolved: Mutex<Option<(i32,u64)>> }`. The test blocks on a `std::sync::mpsc` recv (or a `Condvar`) the observer signals on `finished`/`error`.

| C++ test | Rust assertion |
|---|---|
| `scan_emptyPattern` | `engine.start` (ExactValue, empty pattern) → observer.error == ["Empty pattern"], no finished |
| `scan_maskSizeMismatch` | mask size != pattern → error == ["Pattern and mask size mismatch"] |
| `scan_isRunning` | `is_running()` true during, false after `finished` |
| `scan_abort` | start a large scan, call `abort()`, assert exactly one `finished` fires (partial allowed) |
| `scan_progressEmitted` | ≥1 progress value, last ≥50 (or ==100) |
| `scanStats_emitted` | `scan_stats` observed once; `bytes_failed==0` on happy path; `regions_scanned` matches accepted count |
| `regionCache_reusesAcrossScans` | scan1 enumerates (count 1), scan2 reuses (still 1), `invalidate_region_cache()`, scan3 re-enumerates (count 2) — use a `TestRegionProvider` that increments an `AtomicUsize` on each `enumerate_regions()` |
| `scanResult_jsonShape` | unit-test the panel projection: `format!("{:x}", 0xDEADBEEFCAFEBABE)=="deadbeefcafebabe"`, hex of `[de,ad,be,ef]=="deadbeef"`, module verbatim, and `u64::from_str_radix(...,16)` round-trip |
| `selfAttach_findMutateRevalidate` | `#[cfg(windows)] #[ignore]` stub — out of scope (live process provider) |

### 9.5 Combinations matrix (`test_scanner_combinations.cpp` — data-driven)
Port each `_data()`/slot as a Rust loop over a `&[(...)]` table (or `rstest` cases):

| C++ test | Rust translation |
|---|---|
| `fastScan_alignmentValues` (align 1→64, 4→64, 8→32, 16→16, 32→8, 64→4) | loop over `[(1,64),(4,64),(8,32),(16,16),(32,8),(64,4)]`; 256-byte buf with `0xCAFEBABE` every 4 bytes; UInt32 ExactValue scan; assert result count |
| `condition_valueType_matrix` (8 value types × 5 cases: Exact100→1, Exact50→2, Bigger25→3, Smaller25→0, Smaller75→2) | nested loop over value types and cases; build buffer of 3 typed slots [100,50,50]; assert counts |
| `condition_between` (data rows incl. "100..100"→1, plus other lo..hi rows) | loop; serialize lo→pattern, hi→pattern2; Between scan; assert count |
| `regionFilter_combos` | matrix of exec/write/private/skip flags vs accepted region set; assert counts |
| `addressRange_alignment` | combos of start/end address × alignment; assert counts |
| `signature_wildcards` | signature strings with `??` at various positions → naive masked path; assert match counts/addresses |

**Total target:** ~163 + 68 assertions reproduced (minus the 1 Windows-only `selfAttach` body, which becomes an ignored stub). Each `QCOMPARE(x, n)` → `assert_eq!(x, n)`; each `QVERIFY(b)` → `assert!(b)`; each `QVERIFY(!ok)` + message check → `assert_eq!(err, "exact string")`.

---

## 10. Work order (small, independently-verifiable steps)

Each step compiles + passes its own tests under `cargo test --no-default-features` before the next.

1. **Types + Defaults + serde.** Add `ValueType`, `ScanCondition` (with `#[repr(i32)]` + serde), `AddressRange`, `ScanRequest` (hand `Default`), `ScanResult`, `ScanStats`. Wire `use crate::provider::{Provider, MemoryRegion, RegionType}`. Compile-only. → unit test the `ScanRequest::default()` field values.
2. **`hex_val` + `parse_signature`.** Port §4.1 exactly. → run all `parse_*` tests (§9.1).
3. **`appendLE`/LE helpers + `serialize_value` + `natural_alignment` + `value_size_for_type`.** Port §4.2. → run all `serialize_*`, `alignment_*` tests.
4. **`is_system_module` + the `K_SYSTEM` table.** Port §4.3 verbatim. → run `sysmod_*`.
5. **`bmh_find`.** Port §4.4. → run `bmh_*` incl. the property test (seeded RNG).
6. **`compare_typed` + `format_region_context`.** Port §2.2/§4.5. → micro unit tests (3-way compare for each type incl. NaN→0; region-context exact strings).
7. **`ScanObserver` trait + `NullObserver` + `TestRegionProvider`/`MutableProvider` test fixtures.** Compile-only + a smoke test.
8. **`run_scan` capture + naive paths first** (skip BMH/typed-const initially by forcing the naive branch), get region acceptance, range clip, pre-pass, chunk loop, advance/overlap, alignment re-align correct. → run `scan_exactMatch`, `scan_wildcardMatch`, `scan_alignment*`, `scan_maxResults`, `scan_*Buffer`, `scan_multipleRegions`, `scan_chunkBoundaryOverlap`, `scan_overlappingMatches`, `scan_allWildcardPattern`, filters, `regionType_*`, `skipSystem_*`, `addressCap_*`, `adaptiveChunk_largeRegion`.
9. **Add BMH path + `bmh_pathParity`** to `run_scan`; verify it doesn't change any §8 result counts.
10. **Add typed-const path + capture for compare-conditions.** → `condition_biggerThan/smallerThan/between_firstScan`, `scan_unknownWithAddressRange`.
11. **`constrain_regions` intersection (§4.7a).** → all `scan_constrainRegions_*`.
12. **`run_rescan` (§4.8)** incl. exact/comparison/typed-const/delta filters + span chunking. → `rescan_emptySeed`, `condition_increasedBy/decreasedBy_rescan`, `e2e_findMutateRevalidate`.
13. **`ScanEngine` async wrapper** (spawn thread, region cache, abort, observer delivery). → `scan_isRunning`, `scan_abort`, `scan_progressEmitted`, `scanStats_emitted`, `regionCache_reusesAcrossScans`, validation `error` tests, `scanResult_jsonShape` projection.
14. **Combinations matrix (§9.5).** Port the 6 data-driven loops. → `fastScan_alignmentValues`, `condition_valueType_matrix`, `condition_between`, `regionFilter_combos`, `addressRange_alignment`, `signature_wildcards`.
15. **`#[cfg(windows)] #[ignore]` stub** for `selfAttach_findMutateRevalidate` (out of scope) so the test module is complete and compiles on all targets.

---

## 11. Parity checklist (must all hold — from scanner.md §12)
1. `scan_value` width: capture/typed-const cache `val_size` bytes; BMH/naive cache `min(16, read_len-i)` bytes.
2. `max_results` cap checked *after* push (exact, not off-by-one).
3. Overlapping matches found (BMH `search_from=abs_i+1`; naive `i+=alignment` w/ align 1).
4. Alignment measured from chunk start (re-aligned on chunk advance when align>1).
5. `has_range` requires `end_address > start_address`; `end_address==0` disables clipping.
6. `constrain_regions` merge avoids double-count; clips to intersection; partial-fit pattern not matched.
7. `private_only` keys on `RegionType`, not writability (buffer synthetic region is `Mapped`).
8. `skip_system_modules` + `private_only` compound.
9. Both region filters = AND.
10. `compare_typed` float/NaN → 0; byte fallback for vec/string/insufficient bytes.
11. Between lo==hi = exact equality.
12. First-scan compare-against-previous conditions capture every aligned slot (no filter).
13. IncreasedBy/DecreasedBy = wrapping int / IEEE float, exact `==`.
14. Rescan snapshots `previous_value=scan_value` before re-reading; returns seed unchanged when `needs_filter` false.
15. Empty seed rescan → empty.
16. Region cache keyed on provider identity; `invalidate_region_cache` forces re-enumeration.
17. Unreadable chunks skipped (counted in `bytes_failed`), not fatal; `finished` still fires.
18. `region_module` = `"name+0xoffset"` lowercase-hex no-leading-zeros, or empty.
19. BMH ⇔ naive equivalence (random property test).
20. Validation errors emitted synchronously before worker spawn; `UnknownValue` bypasses validation.
