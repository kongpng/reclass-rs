# PORTING SPEC — Disassembler integration (`disasm`)

Function-level porting spec to drive the faithful Rust implementation of the
`disasm` subsystem. Authored from:
- `/home/loke/reclass-cpp/src/disasm.h` (16 lines) and `src/disasm.cpp` (76 lines) — source of truth.
- `/home/loke/reclass-cpp/tests/test_disasm.cpp` (468 lines, 35 assertions) — behavioral contract.
- `/home/loke/reclass-rs/_design/understand/disasm.md` — behavioral map (read fully).
- `/home/loke/reclass-rs/_design/ARCHITECTURE.md` (§2/§3 module layout, §8 testing).
- `/home/loke/reclass-rs/_design/crate_selection.md` (iced-x86 1.21.0).
- `/home/loke/reclass-rs/_oracle/RESULTS.md` + `_oracle/logs/test_disasm.txt`
  (`test_disasm`: **35 passed / 0 failed / 0 skipped**, exit 0 — fully green oracle).

> This subsystem is **not a UI subsystem**, so the gpui cookbooks are out of scope here.
> The hover-popup *callers* (DisasmPreview / HexDumpPreview) live in the `ui::editor`
> subsystem; this spec covers only the pure formatting module and notes the caller contract.

---

## 0. Summary of the subsystem

`disasm` is a tiny, **pure, stateless, dependency-light** formatting module: two free
functions that turn already-read raw bytes + a base address into a `String` for two
read-only hover popups in the editor.

1. `disassemble()` — x86/x64 instruction disassembly (one line per instruction, address-prefixed).
   Replaces the C `fadec` decoder/formatter with the pure-Rust **`iced-x86`** crate.
2. `hex_dump()` — classic 16-bytes-per-line hex + ASCII dump. **No external crate** — `std` only.

No process memory is read here (callers read via a `Provider` and pass bytes in), no Qt
widgets, no threading, no global/`static` state, no I/O, **no platform-specific code**
(`disasm.{h,cpp}` contain zero `#ifdef`). Error handling is by-value and silent: every
failure mode collapses to an **empty `String`** ("nothing to show"). The single hardest
fidelity requirement is making `iced-x86`'s formatter emit **byte-identical** instruction
text to fadec for the 18 pinned mnemonics in §5 / `test_disasm.cpp`.

---

## 1. Target crate / module(s)

- **Package:** `reclass` (the single package; see ARCHITECTURE §2 — NOT a workspace).
- **Module:** `src/disasm.rs` — already exists as a SKELETON to be filled in (this workflow).
  - Declared in `src/lib.rs` as:
    ```rust
    #[cfg(feature = "disasm")]
    pub mod disasm;
    ```
  - Gated by the `disasm` cargo feature (default-on): `disasm = ["dep:iced-x86"]`.
- **External crate:** `iced-x86 = { version = "1", optional = true }` (already in `Cargo.toml`).
  Pure-Rust x86/x64 decoder + formatter, replaces the `third_party/fadec` C submodule (and all
  its CMake `parseinstrs.py` codegen). Cross-platform; compiles on the Linux dev box.
- **Std only** for `hex_dump` (`core::fmt` / `String`); no extra crate.
- **Tests:** unit tests inline in `src/disasm.rs` under `#[cfg(test)] mod tests` (the module is
  small and the tests need only `super::*`, `core` and `provider::buffer` for the VTable
  end-to-end tests). Run with `--features disasm` (it is default-on, so `cargo test` includes it;
  for the headless logic build use `cargo test --no-default-features --features disasm`).

### 1.1 Existing skeleton (to replace)

`src/disasm.rs` currently has (note the `i32` parameter types, kept from C++ `int`):
```rust
pub fn disassemble(_bytes: &[u8], _base_addr: u64, _bitness: i32, _max_bytes: i32) -> String { todo!() }
pub fn hex_dump(_bytes: &[u8], _base_addr: u64, _max_bytes: i32) -> String { todo!() }
```
**Keep these exact public signatures** (`i32` for `bitness`/`max_bytes`). Rationale:
- `bitness: i32` is load-bearing — the C++ guard rejects any value ≠ 32 and ≠ 64, including
  16, 0, negatives, 48 (`testDisasm_invalidBitness` passes 16). A `u32` would still work for the
  guard, but keeping `i32` mirrors the C++ `int` and lets the guard read identically; convert to
  `u32` only at the `Decoder::new(bitness as u32, …)` call site after the guard.
- `max_bytes: i32` mirrors the C++ default `int maxBytes = 128`. Rust has no default args; the
  callers always pass `128` explicitly, so do not add a builder/overload — just require the arg.
  (The behavioral map suggested `usize`; we use `i32` to match the skeleton already committed and
  the C++ `int` semantics, clamping to 0 for any negative `max_bytes` via `.max(0)` before use.)

---

## 2. Item-by-item C++ → Rust mapping

### 2.1 Types / headers

| C++ item | Where | Rust counterpart | Crate |
|---|---|---|---|
| `namespace rcx` | `disasm.h:6` | module `crate::disasm` | — |
| `#include <QString>` (return type) | `disasm.h:2` | `String` (UTF-8; output is pure ASCII) | std |
| `#include <QByteArray>` (input) | `disasm.h:3` | `&[u8]` | std |
| `#include <cstdint>` (`uint64_t`,`uint8_t`) | `disasm.h:4` | `u64`, `u8` | std |
| `extern "C" { #include <fadec.h> }` | `disasm.cpp:3-5` | `use iced_x86::{...}` | iced-x86 |
| `FdInstr instr;` (opaque decoded insn) | `disasm.cpp:19` | `iced_x86::Instruction` (returned by `decoder.decode()`) | iced-x86 |
| `int fd_decode(buf,len,mode,addr,&instr)` → bytes consumed / neg on fail | `disasm.cpp:20` | `Decoder::decode()` + `decoder.can_decode()` + `instr.is_invalid()` (length/position tracked internally, IP via `instr.ip()`) | iced-x86 |
| `void fd_format(&instr,buf,len)` → NUL-term Intel text | `disasm.cpp:25` | `Formatter::format(&instr, &mut out: String)` on an `IntelFormatter` | iced-x86 |
| `char fmtBuf[128]` fixed stack buffer | `disasm.cpp:24` | a reusable `String` (growable; no truncation concern) | std |

### 2.2 Functions

#### `QString disassemble(const QByteArray& bytes, uint64_t baseAddr, int bitness, int maxBytes = 128)` (`disasm.h:10`, body `disasm.cpp:9-36`)

Rust:
```rust
pub fn disassemble(bytes: &[u8], base_addr: u64, bitness: i32, max_bytes: i32) -> String
```
- **Behavior:** guard → empty `String` if `bytes.is_empty()` OR `bitness` ∉ {32, 64}. Else decode a
  window of `min(bytes.len(), max_bytes)` bytes via `iced-x86`, formatting one address-prefixed
  line per fully-decoded instruction, joined with `\n` (no trailing newline). Stop (break) on the
  first decode failure / truncated instruction, returning what was decoded so far.
- **Replaces fadec** (`fd_decode`/`fd_format`) with `iced-x86`'s `Decoder` + `IntelFormatter`.
- See §3.1 for line-by-line + §4 for the formatter config + §6 for full pseudocode.

#### `QString hexDump(const QByteArray& bytes, uint64_t baseAddr, int maxBytes = 128)` (`disasm.h:13`, body `disasm.cpp:38-74`)

Rust:
```rust
pub fn hex_dump(bytes: &[u8], base_addr: u64, max_bytes: i32) -> String
```
- **Behavior:** guard → empty `String` if `bytes.is_empty()` (no bitness, no other validity check).
  Else format `min(bytes.len(), max_bytes)` bytes as 16-byte rows, address-prefixed, with an ASCII
  sidebar, joined with `\n`. **No external crate** (`std::fmt`/`String` only).
- See §3.2 + §6.2 for full pseudocode.

### 2.3 Qt idiom → Rust idiom (used inside both functions)

| C++ / Qt expression | Rust |
|---|---|
| `bytes.isEmpty()` | `bytes.is_empty()` |
| `qMin((int)bytes.size(), maxBytes)` | `bytes.len().min(max_bytes.max(0) as usize)` |
| `reinterpret_cast<const uint8_t*>(bytes.constData())` | the slice `&bytes[..len]` (no raw pointer) |
| `result.isEmpty()` (line-join guard) | `!result.is_empty()` (push `'\n'` before each non-first line) |
| `result += QLatin1Char('\n')` | `result.push('\n')` |
| `.arg(addr, width, 16, QLatin1Char('0'))` (zero-pad hex, dynamic width) | `format!("{:0width$x}", addr, width = w)` / `write!(s, "{addr:0w$x}")` |
| `.arg(b, 2, 16, QLatin1Char('0'))` (byte → 2 hex digits) | `format!("{b:02x}")` (lowercase) |
| `QStringLiteral("%1  %2").arg(addr,…).arg(text)` | `format!("{addr:0w$x}  {text}")` — **two literal spaces** |
| `QString::fromLatin1(fmtBuf)` | iced formatter output is already a UTF-8 ASCII `String` |
| `(c >= 0x20 && c < 0x7f)` printable test | `(0x20..0x7f).contains(&b)` on the `u8` |
| test helper `line.indexOf("  ")` | `line.find("  ")` |
| test helper `result.split('\n')` | `s.split('\n')` |
| test helper `result.count('\n') + 1` | `s.matches('\n').count() + 1` |

> Note: Qt's `%1` base-16 emits **lowercase** hex by default; Rust `{:x}` is also lowercase. ✅ match.
> Qt positive field width = right-justify / left-pad → Rust `{:0N$x}` left-pads with `'0'`. ✅ match.

---

## 3. Implementation, line-by-line

### 3.1 `disassemble` (`disasm.cpp:9-36`)

| C++ line | Action | Rust equivalent |
|---|---|---|
| `:10-11` | guard: empty OR bitness∉{32,64} → `{}` | `if bytes.is_empty() \|\| (bitness != 32 && bitness != 64) { return String::new(); }` |
| `:13` | `len = min(size, maxBytes)` | `let len = bytes.len().min(max_bytes.max(0) as usize);` |
| `:14` | raw buffer ptr | not needed; use `let window = &bytes[..len];` |
| `:16-17` | `result=""`, `off=0` | `let mut result = String::new();` (no explicit `off` — iced tracks position) |
| `:18` | `while off < len` | `while decoder.can_decode()` |
| `:19-20` | `fd_decode(buf+off, len-off, bitness, baseAddr+off, &instr)` | `let instr = decoder.decode();` (Decoder constructed with `with_ip(bitness, window, base_addr, …)` so `instr.ip() == base_addr + off`) |
| `:21-22` | `if ret < 0 break;` | `if instr.is_invalid() { break; }` (see §4.3 — **break**, never continue) |
| `:24-25` | `fd_format(&instr, fmtBuf, 128)` | `text.clear(); formatter.format(&instr, &mut text);` |
| `:27-28` | append `'\n'` if non-first | `if !result.is_empty() { result.push('\n'); }` |
| `:29-31` | `"%1  %2"` with addr width 16/8 and instr text | `let w = if bitness == 64 { 16 } else { 8 }; write!(result, "{:0w$x}  {}", instr.ip(), text).unwrap();` |
| `:33` | `off += ret` | implicit (decoder advanced IP internally) |
| `:35` | `return result` | `result` |

**Critical invariants (all pinned by tests):**
- **Address width is bitness-dependent, not value-dependent.** 64-bit → always 16 hex digits;
  32-bit → always 8. (`testDisasm64_addrWidth`: `find("  ") == 16`; `testDisasm32_addrWidth`: `== 8`.)
  This is the *opposite* of `hex_dump`, whose width is value-dependent.
- **Each line uses the instruction's own absolute address** (`base_addr + off` = `instr.ip()`), so
  consecutive instructions show increasing addresses (`testDisasm64_pushMov`: line 0 `…401000`,
  line 1 `…401001`; `testDisasm64_multipleNops`: line `i` starts with `format!("{:016x}", 0x1000+i)`).
- **Two-space separator** between address and mnemonic (load-bearing for the `mnemonic()` test
  helper which splits on the first `"  "`; see §5).
- **`max_bytes` caps decoded bytes, not instructions.** Window = first `len` bytes. A multi-byte
  instruction that *starts* inside the window but needs bytes beyond it makes iced produce an
  invalid/truncated decode → `break` (matches fadec returning negative for insufficient bytes).
  (`testDisasm_maxBytes`: 200 `nop` bytes, `max_bytes=128` → exactly 128 lines, since each `nop`
  is 1 byte.)
- **First-byte decode failure → empty result** (loop breaks before any append).

### 3.2 `hexDump` (`disasm.cpp:38-74`)

| C++ line | Action | Rust equivalent |
|---|---|---|
| `:39-40` | guard empty → `{}` | `if bytes.is_empty() { return String::new(); }` |
| `:42` | `len = min(size, maxBytes)` | `let len = bytes.len().min(max_bytes.max(0) as usize);` |
| `:43` | `result=""` | `let mut result = String::new();` |
| `:45` | `for off in (0..len).step_by(16)` | `let mut off = 0; while off < len { … off += 16; }` (or `for off in (0..len).step_by(16)`) |
| `:46` | `lineLen = min(16, len-off)` | `let line_len = 16.min(len - off);` |
| `:48-49` | `'\n'` if non-first | `if !result.is_empty() { result.push('\n'); }` |
| `:52` | `wide = (baseAddr + len > 0xFFFFFFFF)` — **once, from whole-dump end** | `let wide = base_addr + (len as u64) > 0xFFFF_FFFF;` (compute once **before** the loop; see note) |
| `:53` | `"%1  ".arg(baseAddr+off, wide?16:8, 16, '0')` | `let aw = if wide { 16 } else { 8 }; write!(result, "{:0aw$x}  ", base_addr + off as u64).unwrap();` |
| `:56-64` | 16 hex slots loop | see below |
| `:57-59` | `i < lineLen`: byte → `"%1 "` (2 hex + space) | `write!(result, "{:02x} ", bytes[off + i]).unwrap();` |
| `:60-61` | else: 3 spaces | `result.push_str("   ");` |
| `:63` | after `i==7`: one extra space | `if i == 7 { result.push(' '); }` |
| `:67` | one space before ASCII | `result.push(' ');` |
| `:68-70` | ASCII for real bytes | loop `0..line_len`: printable `[0x20,0x7f)` push as `char`, else `'.'` |
| `:73` | `return result` | `result` |

**Critical invariants:**
- **`wide` is computed once for the whole dump from `base_addr + len`** (the *end* of the dumped
  range), NOT per-row and NOT from `base_addr` alone. In C++ it is recomputed each row inside the
  loop, but the inputs (`baseAddr`, `len`) are loop-invariant, so the value is identical every row —
  in Rust compute it **once before the loop** (equivalent result, clearer). A dump that *ends* above
  4 GiB uses 16-digit addresses on every row. (`testHexDump_wideAddr`: 16 bytes at `0x100000000`
  → first line `0000000100000000` because `0x100000000 + 16 > 0xFFFFFFFF`. Contrast
  `testHexDump_basic`: 32 bytes at `0x1000` → `00001000`, 8-digit, because `0x1000+32 < 4 GiB`.)
- **Exactly 16 hex slots per row** regardless of `line_len`; absent slots on a short final row emit
  **3 spaces** so the ASCII column stays aligned. The extra middle gap after slot 7 is emitted in
  *both* the present and absent branches, **every** row.
- **ASCII side has no padding** — only `line_len` chars (a short row shows fewer ASCII chars).
- Printable test is `0x20 <= b < 0x7f` (space through `~`); else `.`.
  (`testHexDump_nonPrintable`: 16 bytes, `[0]='A'`, `[15]='Z'`, rest `\0` → sidebar contains
  `A..............Z` — A + 14 dots for the 14 NULs + Z.)
- Byte hex is **2 lowercase digits**. (`testHexDump_hexValues`: `de ad be ef`; the test matches
  case-insensitively but Qt and Rust both emit lowercase.)

#### Exact column geometry of one full (16-byte) row (asserted shape)
```
<addr:8|16 hex><2 spaces>[hh' ' ×8]<1 extra space>[hh' ' ×8]<1 space><up to 16 ASCII chars>
```
- Address: 8 or 16 hex digits + 2 spaces.
- 16 hex byte slots, each `hh` + 1 trailing space; PLUS one extra space inserted after slot 7.
- One space, then ASCII chars.

---

## 4. Decoder integration: fadec → iced-x86

### 4.1 Construction

```rust
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter, Instruction};

let mut decoder = Decoder::with_ip(bitness as u32, window, base_addr, DecoderOptions::NONE);
let mut formatter = IntelFormatter::new();
configure_formatter(formatter.options_mut());   // §4.2
let mut instr = Instruction::default();          // reuse to avoid per-iter alloc (optional)
let mut text = String::new();
```
- `Decoder::with_ip(bitness, data, ip, options)` sets the starting IP. `decoder.decode()` (or
  `decode_out(&mut instr)`) advances the IP automatically, and `instr.ip()` yields `base_addr + off`
  — exactly what the C++ formats. The "remaining bytes" (`len - off`) and "bytes consumed" (`ret`)
  bookkeeping is internal to the decoder.
- `bitness` is validated to be 32 or 64 *before* this line, so `bitness as u32` is always valid
  (iced requires 16/32/64; we never pass 16 because the guard rejected it).

### 4.2 Formatter configuration — **byte-match fadec** (the #1 fidelity requirement)

fadec's `fd_format` emits **Intel syntax, lowercase**. iced's `IntelFormatter` (the "intel"
family, distinct from `MasmFormatter`/`NasmFormatter`/`GasFormatter`) is the closest match. Set
these options (via `formatter.options_mut()`), each justified by a specific pinned test:

| iced option (method on `FormatterOptions`) | Value | Why (fadec behavior / test) |
|---|---|---|
| `set_uppercase_mnemonics(false)` (default) | false | lowercase `push`, `mov`, `nop`, `ret`, `int3` |
| `set_uppercase_registers(false)` (default) | false | `rax`, `rbp`, `eax`, `esp` lowercase |
| `set_uppercase_hex(false)` | false | `0x20`, `0x10`, `0x1105` lowercase hex |
| `set_hex_prefix("0x")` | `"0x"` | immediates like `0x20`, displacements `[rbx+0x10]` (IntelFormatter defaults to `0x`-prefix already; set explicitly to be safe) |
| `set_hex_suffix("")` | `""` | no `h` suffix (that's the MasmFormatter style — avoid) |
| `set_leading_zeroes(false)` | false | `0x8`/`0x10`/`0x1105`, **not** `0x08`/`0x0000…1105` — critical for `sub rsp, 0x20`, `call 0x1105`, `[rsp+0x8]` |
| `set_branch_leading_zeroes(false)` | false | `call 0x1105`, `jmp 0x1012` (absolute, no leading zeros) |
| `set_memory_size_options(MemorySizeOptions::Always)` | `Always` | always emit `qword ptr` / `dword ptr` — `mov rax, qword ptr [rbx+0x10]` |
| `set_rip_relative_addresses(false)` | false | keep `[rip+0x10]` symbolic, NOT the resolved absolute address — `lea rax, [rip+0x10]` |
| `set_space_after_operand_separator(true)` (default) | true | operands separated by `", "` — `xor eax, eax`, `mov rbp, rsp` |
| `set_show_zero_displacements(false)` (default) | false | no spurious `+0x0` displacements |

> `MemorySizeOptions` is `iced_x86::MemorySizeOptions`.

**Branch targets** resolve to absolute by default in iced when the decoder was given the
instruction IP (`with_ip`): a 5-byte `call rel32` at `0x1000` with rel `0x100` → target
`0x1000+5+0x100 = 0x1105`; a 2-byte `jmp rel8` at `0x1000` with rel `0x10` → `0x1000+2+0x10 = 0x1012`.
With `branch_leading_zeroes=false` and `hex_prefix="0x"` these render as `call 0x1105` / `jmp 0x1012`.

**`int3`** — iced renders the `0xcc` opcode as the single token `int3` (not `int 3`). ✅ matches fadec.

### 4.3 Decode loop semantics — the iced ↔ fadec subtleties

```rust
while decoder.can_decode() {
    decoder.decode_out(&mut instr);   // or: let instr = decoder.decode();
    if instr.is_invalid() {           // mirrors fadec ret < 0
        break;                        // STOP entirely — never `continue`
    }
    text.clear();
    formatter.format(&instr, &mut text);
    if !result.is_empty() { result.push('\n'); }
    let w = if bitness == 64 { 16 } else { 8 };
    let _ = write!(result, "{:0w$x}  {}", instr.ip(), text);
}
```
- `decoder.can_decode()` replaces `off < len`.
- `instr.is_invalid()` (i.e. `instr.code() == Code::INVALID`) is the equivalent of fadec's
  `ret < 0` → `break`. **Subtlety:** iced will, if you let it, return an "invalid" instruction that
  *still consumed bytes* and keep going. The C++ behavior is to **stop entirely** at the first
  decode failure, so the port **must `break`, NOT `continue`** on `is_invalid()`. At end-of-buffer
  iced may produce a truncated/invalid instruction; breaking on invalid matches fadec returning
  negative for insufficient bytes.
- The two-space separator and bitness-dependent zero-padded width are reproduced verbatim
  (`{:0w$x}  {}`). Use `core::fmt::Write` (`use std::fmt::Write;`) so `write!(result, …)` works on a
  `String`; the `Result` is infallible for `String`, so `let _ =` / `.unwrap()` is fine.

### 4.4 `hex_dump` has no decoder dependency

Pure byte/string formatting — implement with `std::fmt`/`String`. No `iced-x86` use. (It still
lives in the `disasm` module / behind the `disasm` feature for simplicity, since the module as a
whole is feature-gated; the function itself uses no optional dep.)

---

## 5. Output format details (load-bearing for tests)

The test helper (`test_disasm.cpp:9-12`) splits each disasm line on the **first `"  "`** and takes
everything after:
```cpp
static QString mnemonic(const QString& line) {
    int sep = line.indexOf("  ");
    return sep >= 0 ? line.mid(sep + 2) : line;
}
```
Rust port of the helper (test-local):
```rust
fn mnemonic(line: &str) -> &str {
    match line.find("  ") { Some(sep) => &line[sep + 2..], None => line }
}
```
Therefore the address field MUST be followed by **exactly two spaces** and the instruction text
MUST NOT contain a leading double space (fadec/iced never do). For 64-bit `find("  ") == 16`; for
32-bit `== 8`.

### Disassembly line format
```
<addr: hex, width 16 if bitness==64 else 8, zero-padded, lowercase><2 spaces><instr text>
```
Example: `0000000000401000  push rbp`, `0000000140001000  push rbp` (base `0x140001000`),
`00401000  push ebp` (32-bit).

### HexDump line format (full 16-byte row)
```
<addr: hex, width 8 or 16, zero-padded><2 spaces><b0 ><b1 >…<b7 ><1 extra space><b8 >…<b15 ><1 space><ascii…>
```
Width 16 iff `base_addr + len > 0xFFFFFFFF` (decided once for whole dump), else 8.

### Mnemonic reference table the tests pin (the acceptance gate)

| Bytes (hex) | bitness | base | Expected `mnemonic()` |
|---|---|---|---|
| `55` | 64 | 0x401000 | `push rbp` |
| `48 89 e5` | 64 | (line 1 of above) | `mov rbp, rsp` |
| `c3` | 64 | 0x7FF000 | `ret` |
| `90` | 64 | 0 | `nop` |
| `31 c0` | 64 | 0 | `xor eax, eax` |
| `48 83 ec 20` | 64 | 0 | `sub rsp, 0x20` |
| `cc` | 64 | 0 | `int3` |
| `57` | 64 | 0 | `push rdi` |
| `5e` | 64 | 0 | `pop rsi` |
| `85 c0` | 64 | 0 | `test eax, eax` |
| `48 8d 05 10 00 00 00` | 64 | 0x1000 | `lea rax, [rip+0x10]` |
| `e8 00 01 00 00` | 64 | 0x1000 | `call 0x1105` |
| `eb 10` | 64 | 0x1000 | `jmp 0x1012` |
| `48 8b 43 10` | 64 | 0 | `mov rax, qword ptr [rbx+0x10]` |
| `48 89 4c 24 08` | 64 | 0 | `mov qword ptr [rsp+0x8], rcx` |
| `48 83 ec 28` | 64 | 0x100 | `sub rsp, 0x28` |
| `55` | 32 | 0x401000 | `push ebp` |
| `89 e5` | 32 | (line 1 of above) | `mov ebp, esp` |

Sequences: prologue `55 48 89 e5 48 83 ec 20 c3` → `push rbp` / `mov rbp, rsp` / `sub rsp, 0x20` /
`ret`. Five `90` → five `nop`. `31 c0 c3` → `xor eax, eax` / `ret`. `48 83 ec 28 90 c3` →
`sub rsp, 0x28` / `nop` / `ret`.

> ACHIEVING THESE EXACT STRINGS IS THE SINGLE MOST IMPORTANT FIDELITY REQUIREMENT. If any string
> differs after the §4.2 config, tweak only the formatter options (never post-process the text) and
> re-run the ported tests until byte-identical. There is **no binary serialization / persistence**;
> the only "format" is this ephemeral text.

---

## 6. Algorithm pseudocode (the full functions)

### 6.1 `disassemble`
```rust
use std::fmt::Write;
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter, Instruction, MemorySizeOptions};

pub fn disassemble(bytes: &[u8], base_addr: u64, bitness: i32, max_bytes: i32) -> String {
    // :10-11 guard
    if bytes.is_empty() || (bitness != 32 && bitness != 64) {
        return String::new();
    }
    // :13 window
    let len = bytes.len().min(max_bytes.max(0) as usize);
    let window = &bytes[..len];

    let mut decoder = Decoder::with_ip(bitness as u32, window, base_addr, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    {
        let o = formatter.options_mut();
        o.set_uppercase_hex(false);
        o.set_hex_prefix("0x");
        o.set_hex_suffix("");
        o.set_leading_zeroes(false);
        o.set_branch_leading_zeroes(false);
        o.set_rip_relative_addresses(false);
        o.set_memory_size_options(MemorySizeOptions::Always);
        // (uppercase_mnemonics / uppercase_registers / space_after_operand_separator: defaults OK)
    }

    let mut result = String::new();
    let mut instr = Instruction::default();
    let mut text = String::new();
    while decoder.can_decode() {                 // :18
        decoder.decode_out(&mut instr);          // :20
        if instr.is_invalid() { break; }         // :21-22  (BREAK, not continue)
        text.clear();
        formatter.format(&instr, &mut text);     // :24-25
        if !result.is_empty() { result.push('\n'); }  // :27-28
        let w = if bitness == 64 { 16 } else { 8 };
        let _ = write!(result, "{:0w$x}  {}", instr.ip(), text); // :29-31
    }
    result                                       // :35
}
```

### 6.2 `hex_dump`
```rust
use std::fmt::Write;

pub fn hex_dump(bytes: &[u8], base_addr: u64, max_bytes: i32) -> String {
    if bytes.is_empty() { return String::new(); }          // :39-40
    let len = bytes.len().min(max_bytes.max(0) as usize);  // :42
    let wide = base_addr + (len as u64) > 0xFFFF_FFFF;     // :52 (compute once)
    let aw = if wide { 16 } else { 8 };

    let mut result = String::new();
    let mut off = 0;
    while off < len {                                      // :45
        let line_len = 16.min(len - off);                 // :46
        if !result.is_empty() { result.push('\n'); }      // :48-49
        let _ = write!(result, "{:0aw$x}  ", base_addr + off as u64);  // :53

        for i in 0..16 {                                  // :56
            if i < line_len {                             // :57
                let _ = write!(result, "{:02x} ", bytes[off + i]); // :58-59
            } else {
                result.push_str("   ");                   // :61 (3 spaces)
            }
            if i == 7 { result.push(' '); }               // :63 (middle gap)
        }

        result.push(' ');                                 // :67
        for i in 0..line_len {                            // :68
            let b = bytes[off + i];
            result.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' }); // :70
        }
        off += 16;
    }
    result                                                // :73
}
```

---

## 7. Error-handling strategy

**By-value, silent — never `Result`/`Option`.** Mirror the C++ contract exactly: the caller treats
an empty body as "no popup". All failure modes collapse to `String::new()`:
- empty input → empty (both functions).
- `disassemble` invalid bitness (∉ {32,64}) → empty.
- `disassemble` first instruction fails to decode → empty (loop breaks before any append).
- `disassemble` mid-stream decode failure → returns the prefix decoded so far (**partial, not an
  error**). The remaining bytes are silently dropped.
- `negative max_bytes` → `.max(0)` clamps to 0 → empty (C++ would also produce empty/UB-ish; clamp
  is the safe parity choice; tests never pass negatives).

No panics in the happy path: `write!(String, …)` is infallible (`fmt::Error` only on a failing
`Write` impl, which `String` never is) → `let _ =` it. Slicing `&bytes[..len]` is safe because
`len <= bytes.len()` by construction. `as char` from a `u8` in `0x20..0x7f` is always valid ASCII.

**Do NOT** introduce a `thiserror` enum here — there is nothing to report. (Contrast `imports`/`rtti`
which do use typed errors.) Keep the two functions infallible-returning-`String`.

---

## 8. Concurrency / portability

- **Concurrency:** none. Both functions are pure, reentrant, stateless — no locks, no `static`,
  no shared mutable state. Safe to call from any thread (callers invoke on the UI thread inside
  hover handlers). No `unsafe`.
- **Portability:** zero platform-specific code; no `#[cfg(...)]` needed in this module. `iced-x86`
  is pure Rust and builds on Linux/macOS/Windows. The whole module sits behind the `disasm` cargo
  feature (default-on), which is the only conditional compilation.
- **Memory safety:** the C++ `char fmtBuf[128]` is replaced by a growable `String`, so no truncation
  concern (fadec truncated at 128 bytes silently, but no real x86 instruction text approaches that —
  behavior equivalent). `len` uses `usize`/`.min()`, no integer-width overflow (C++ casts size to
  `int`, only a risk for >2 GiB inputs that never occur; inputs are ≤128 bytes in practice).

---

## 9. Caller contract (for the editor port — NOT implemented here)

These functions are consumed only by two `HoverPreview` subclasses in `editor.cpp`; reproduce this
gating when porting `ui::editor`, but it does not affect `disasm` itself. Documented so the
end-to-end disasm tests (§10.3) can be written and so the editor port wires them identically.

- **`DisasmPreview`** (`editor.cpp:1567-1591`): `id()=="disasm"`, label "Disassembly". Eligible iff
  the node is `FuncPtr32`/`FuncPtr64` AND the pointer value read at the row ≠ 0. On show: read the
  pointer value at `lm.offsetAddr` (the **composed** absolute address) from the **snapshot/data**
  provider; read up to 128 code bytes at that value from the **real/code** provider (preferred over
  snapshot, which lacks code pages); `body = capLines(disassemble(bytes, target, is64?64:32, 128))`;
  `baseAddr = target` (the function's own address). Empty body ⇒ no popup.
- **`HexDumpPreview`** (`editor.cpp:1539-1565`): `id()=="hex_dump"`, label "Hex Dump". Eligible iff
  node is `Pointer32`/`Pointer64`, `lm.pointerTargetName` empty (untyped pointer), `node.refId==0`,
  and pointer value ≠ 0. `body = capLines(hexDump(bytes, target, 128))`.
- **Two-provider split** (`controller.cpp:2028-2044`): pointer *values* are read from the snapshot
  (tree-data pages) at the composed `offsetAddr`; *code* is read from the real provider at the
  pointer value. `capLines` (default 6 lines + `"..."`) is applied by the **caller**, not inside
  `disasm`.

---

## 10. TEST PLAN

All 35 assertions in `test_disasm.cpp` pass in the oracle (`_oracle/logs/test_disasm.txt`,
exit 0). Port each `private slot` to a Rust `#[test]` in `src/disasm.rs`'s `#[cfg(test)] mod tests`.
The oracle output is the exact-string golden reference; assert the **exact** mnemonic / prefix
strings, not just "non-empty". Add the test-local helper `mnemonic(&str) -> &str` (§5).

> `QCOMPARE(a,b)` → `assert_eq!(a, b)`; `QVERIFY(x)` → `assert!(x)`; `QVERIFY2(x,msg)` →
> `assert!(x, "{}", msg)`; `result.split('\n')` → `result.split('\n').collect::<Vec<_>>()`;
> `lines.size()` → `lines.len()`; `result.count('\n')+1` → `result.matches('\n').count()+1`;
> `line.startsWith(s)` → `line.starts_with(s)`; `result.contains(s)` → `result.contains(s)`;
> `contains(s, Qt::CaseInsensitive)` → `result.to_ascii_lowercase().contains(&s.to_ascii_lowercase())`
> (or just `contains` since output is lowercase). Build byte literals with `b"\x55\x48\x89\xe5"`
> or `&[0x55u8, 0x48, 0x89, 0xe5]`.

### 10.1 `disassemble` exact-mnemonic tests (assert against §5 table — the acceptance gate)

| C++ test | Rust `#[test]` | Assertions |
|---|---|---|
| `testDisasm64_pushMov` | `disasm64_push_mov` | `&[0x55,0x48,0x89,0xe5]` @ `0x401000`/64 → 2 lines; `lines[0].starts_with("0000000000401000")`, `lines[1].starts_with("0000000000401001")`; `mnemonic(lines[0])=="push rbp"`, `mnemonic(lines[1])=="mov rbp, rsp"` |
| `testDisasm64_ret` | `disasm64_ret` | `&[0xc3]` @ `0x7FF000`/64 → `mnemonic == "ret"` |
| `testDisasm64_nop` | `disasm64_nop` | `&[0x90]` @ 0/64 → `"nop"` |
| `testDisasm64_xorEax` | `disasm64_xor_eax` | `&[0x31,0xc0]` → `"xor eax, eax"` |
| `testDisasm64_subRsp` | `disasm64_sub_rsp` | `&[0x48,0x83,0xec,0x20]` → `"sub rsp, 0x20"` |
| `testDisasm64_int3` | `disasm64_int3` | `&[0xcc]` → `"int3"` |
| `testDisasm64_pushRdi` | `disasm64_push_rdi` | `&[0x57]` → `"push rdi"` |
| `testDisasm64_popRsi` | `disasm64_pop_rsi` | `&[0x5e]` → `"pop rsi"` |
| `testDisasm64_testEax` | `disasm64_test_eax` | `&[0x85,0xc0]` → `"test eax, eax"` |
| `testDisasm64_leaRipRel` | `disasm64_lea_rip_rel` | `&[0x48,0x8d,0x05,0x10,0,0,0]` @ `0x1000` → `"lea rax, [rip+0x10]"` |
| `testDisasm64_callRel` | `disasm64_call_rel` | `&[0xe8,0x00,0x01,0,0]` @ `0x1000` → `"call 0x1105"` |
| `testDisasm64_jmpRel` | `disasm64_jmp_rel` | `&[0xeb,0x10]` @ `0x1000` → `"jmp 0x1012"` |
| `testDisasm64_movMemRead` | `disasm64_mov_mem_read` | `&[0x48,0x8b,0x43,0x10]` → `"mov rax, qword ptr [rbx+0x10]"` |
| `testDisasm64_movMemWrite` | `disasm64_mov_mem_write` | `&[0x48,0x89,0x4c,0x24,0x08]` → `"mov qword ptr [rsp+0x8], rcx"` |
| `testDisasm64_functionPrologue` | `disasm64_function_prologue` | `&[0x55,0x48,0x89,0xe5,0x48,0x83,0xec,0x20,0xc3]` @ `0x140001000`/64 → 4 lines; `lines[0].starts_with("0000000140001000")`; mnemonics `push rbp`/`mov rbp, rsp`/`sub rsp, 0x20`/`ret` |
| `testDisasm64_multipleNops` | `disasm64_multiple_nops` | `&[0x90;5]` @ `0x1000`/64 → 5 lines; each `mnemonic=="nop"` and `lines[i].starts_with(&format!("{:016x}", 0x1000u64 + i as u64))` |
| `testDisasm32_pushMov` | `disasm32_push_mov` | `&[0x55,0x89,0xe5]` @ `0x401000`/32 → 2 lines; `lines[0].starts_with("00401000")`; `"push ebp"`, `"mov ebp, esp"` |

### 10.2 `disassemble` / `hexDump` edge-case + format tests

| C++ test | Rust `#[test]` | Assertions |
|---|---|---|
| `testDisasm_empty` | `disasm_empty` | `disassemble(&[],0,64,128).is_empty()` AND `disassemble(&[],0,32,128).is_empty()` |
| `testDisasm_invalidBitness` | `disasm_invalid_bitness` | `disassemble(&[0x90],0,16,128).is_empty()` (also assert for 0, 48, -1 as extra parity coverage) |
| `testDisasm_maxBytes` | `disasm_max_bytes` | `disassemble(&[0x90;200],0,64,128)`: `matches('\n').count()+1 == 128` |
| `testDisasm64_addrWidth` | `disasm64_addr_width` | `disassemble(&[0x90],0,64,128).find("  ") == Some(16)` |
| `testDisasm32_addrWidth` | `disasm32_addr_width` | `disassemble(&[0x90],0,32,128).find("  ") == Some(8)` |
| `testHexDump_basic` | `hexdump_basic` | 32 bytes (values 0..31) @ `0x1000`/128 → `matches('\n').count()+1 == 2`; `starts_with("00001000")` |
| `testHexDump_ascii` | `hexdump_ascii` | `hex_dump(b"Hello, World!xx",0,128).contains("Hello")` |
| `testHexDump_nonPrintable` | `hexdump_non_printable` | 16 bytes, `[0]='A'`, `[15]='Z'`, rest 0 → `.contains("A..............Z")` |
| `testHexDump_empty` | `hexdump_empty` | `hex_dump(&[],0,128).is_empty()` (C++ uses default maxBytes; pass 128) |
| `testHexDump_maxBytes` | `hexdump_max_bytes` | `hex_dump(&[0xAA;200],0,64)`: `matches('\n').count()+1 == 4` |
| `testHexDump_wideAddr` | `hexdump_wide_addr` | `hex_dump(&[0;16],0x100000000,128).starts_with("0000000100000000")` |
| `testHexDump_hexValues` | `hexdump_hex_values` | `[0xDE,0xAD,0xBE,0xEF]` + zeros to 16 → `.contains("de ad be ef")` |
| `testHexDump_secondLineAddr` | `hexdump_second_line_addr` | 32 bytes `0x42` @ `0x2000`/128 → 2 lines; `lines[1].starts_with("00002010")` |

### 10.3 End-to-end VTable / hover-flow tests (exercise `disasm` through the compose pipeline)

These three tests primarily validate the `compose` subsystem (composed `offsetAddr` vs `node.offset`)
and the two-provider split, but they assert disasm mnemonics. They depend on `core::{NodeTree, Node,
NodeKind, LineKind}`, `compose::compose`, and `provider::buffer::BufferProvider` (with `readU64`,
`readBytes`/`read`, `isReadable`). **Port them as integration tests** once those modules exist; if
`compose`/`provider` are not yet ported when this workflow runs, **stub these as `#[ignore]` with a
`// TODO(compose): un-ignore once compose+BufferProvider land` note** so the disasm-only gate (§10.1/
§10.2) is independently verifiable now. The disasm-specific assertions to preserve verbatim:

| C++ test | Rust `#[test]` | Disasm assertions |
|---|---|---|
| `testVTableDisasm_composedAddress` | `vtable_disasm_composed_address` | Two FuncPtr64 lines at composed `offsetAddr` `0x100`/`0x108`; read ptr values `0x200`/`0x300`; `disassemble(code,0x200,64,128)` → `push rbp`/`ret` and `lines[0].contains("200")`; `disassemble(code,0x300,64,128)` → `xor eax, eax`/`ret` and `lines[0].contains("300")`. Plus the negative check: reading `node.offset` (0 / 8) gives the vptr value `0x100`, not `0x200`/`0x300`. |
| `testVTableDisasm_wrongAddressGivesWrongCode` | `vtable_disasm_wrong_address` | RIGHT target `0x100` code `48 83 ec 28 90 c3` → `sub rsp, 0x28`/`nop`/`ret` (≥3 lines). WRONG target `0x80` (vtable data) → `!wrong_asm.contains("sub rsp")`. |
| `testHoverFlow_fullSimulation` | `hover_flow_full_simulation` | snapshot provider (only first `0x200` bytes) has the vtable pages, NOT the code pages (`isReadable(codeAddr,1)==false`); real provider reads code; `disassemble(code,ptrVal,64,128)` → func0 `push rbp`/`mov rbp, rsp`/`sub rsp, 0x20`/`ret`, func1 `xor eax, eax`/`ret`. |

### 10.4 Acceptance gate
1. `cargo build --features disasm` (and `--no-default-features --features disasm`) compiles.
2. `cargo test --features disasm disasm::` — all §10.1/§10.2 tests pass with the **exact** strings.
   This is the parity gate; if any string differs, adjust only the §4.2 formatter options.
3. §10.3 tests pass (or are `#[ignore]`d pending compose/provider) — un-ignore in the editor/compose
   workflow.
4. Cross-check against the oracle: 35 C++ assertions ↔ the Rust test count (note: a few C++ slots
   bundle multiple `QCOMPARE`s, e.g. `testDisasm_empty` has 2 — keep them as one `#[test]` with 2
   `assert!`s).

---

## 11. Ordered, independently-verifiable work steps

1. **Replace the `hex_dump` skeleton** (`src/disasm.rs`) with §6.2 — no external dep. Add the
   §10.2 hexdump tests + the `mnemonic` helper. Run `cargo test --features disasm` → hexdump tests
   green. *(Independently verifiable: zero decoder dependency.)*
2. **Implement `disassemble`** per §6.1 with the §4.2 formatter config. Add the §10.1 exact-mnemonic
   tests + §10.2 disasm edge tests. Run; iterate **only on §4.2 options** until every pinned string
   in §5 is byte-identical. *(Acceptance gate §10.4.2.)*
3. **Confirm the iced formatter identifiers** compile (`IntelFormatter`, `FormatterOptions` setters,
   `MemorySizeOptions::Always`, `Decoder::with_ip`, `instr.ip()`, `instr.is_invalid()`,
   `decode_out`) against the resolved iced-x86 1.x in `Cargo.lock`; fix any setter-name drift
   (e.g. `set_branch_leading_zeroes` vs `set_branch_leading_zeros`) using the locked crate's docs.
   *(Verifiable: build green.)*
4. **Port the §10.3 end-to-end tests.** If `compose`/`provider::buffer` exist, wire them; otherwise
   add as `#[ignore]` stubs with the TODO note. *(Verifiable independently of the editor port.)*
5. **Final gate:** `cargo build` (default features) + `cargo build --no-default-features --features
   disasm` + `cargo test --features disasm disasm::` all green; mnemonic strings match §5 exactly.

---

## 12. Open questions / verify during implementation
- **Exact iced 1.x setter names.** The names in §4.2 follow iced-x86's `FormatterOptions` API
  (`set_hex_prefix`, `set_leading_zeroes`, `set_branch_leading_zeroes`, `set_rip_relative_addresses`,
  `set_memory_size_options`, `set_uppercase_hex`). Confirm against the locked version; the *British*
  spelling `leading_zeroes` is what iced uses — do not "correct" it to `zeros`.
- **`IntelFormatter` vs `NasmFormatter`.** Both can match; `IntelFormatter` is the documented closest.
  If a string differs (e.g. `int3` vs `int 3`, or `qword ptr` spacing), try `NasmFormatter` with the
  same options before hand-tuning. The oracle confirms fadec produces exactly the §5 strings, so the
  goal is matching fadec, not "correct disassembly".
- **Branch/RIP rendering out of the box.** Verify `call 0x1105`/`jmp 0x1012` (absolute, no leading
  zeros) and `[rip+0x10]` (symbolic) emerge with the §4.2 config; pin via §10.1 tests.
- **Negative displacement / SIB scale rendering** (e.g. `[rax+rcx*4-0x8]`) is **not** pinned by any
  test; parity there is best-effort against fadec docs — do not over-engineer.
```
