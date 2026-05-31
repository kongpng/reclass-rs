# Subsystem: Disassembler integration (`disasm`)

Source of truth (C++):
- `/home/loke/reclass-cpp/src/disasm.h` (16 lines — public interface)
- `/home/loke/reclass-cpp/src/disasm.cpp` (76 lines — implementation)
- `/home/loke/reclass-cpp/tests/test_disasm.cpp` (468 lines — behavioral spec)

Callers / wiring:
- `/home/loke/reclass-cpp/src/editor.cpp` (`HexDumpPreview`, `DisasmPreview` hover popups; lines ~1539-1591, `readPointerAtRow` lines 1511-1523, `capLines` lines 1525-1537)
- `/home/loke/reclass-cpp/src/controller.cpp` (provider wiring for popups; lines 2028-2044)
- `/home/loke/reclass-cpp/src/main.cpp` (about-box credit line ~4631)
- Build: `/home/loke/reclass-cpp/CMakeLists.txt` (fadec submodule build, lines 76-92, 196-211, 525-531)

---

## 1. Purpose

`disasm` is a tiny, self-contained module that turns raw memory bytes into human-readable text for two read-only hover popups in the editor:

1. **`disassemble()`** — x86/x64 instruction disassembly. Used by the "Disassembly" hover popup that appears when the user hovers a **function-pointer field** (`FuncPtr32`/`FuncPtr64`). It reads code bytes at the function address and renders one line per instruction, each prefixed by the absolute address.
2. **`hexDump()`** — classic 16-bytes-per-line hex+ASCII dump. Used by the "Hex Dump" hover popup that appears when the user hovers an **untyped raw pointer field** (`Pointer32`/`Pointer64` with no resolved struct target). It renders the memory the pointer points at.

The module is **pure formatting**: it takes a `QByteArray` (already-read bytes) plus a base address and produces a `QString`. It does **not** read process memory itself — callers read via a `Provider` and pass the bytes in. It has no Qt-widget dependency, no threading, no global state, no I/O. The only external dependency is the **fadec** x86 decoder (C library, `extern "C"`), which the Rust port replaces with **iced-x86**.

Both functions are stateless free functions in namespace `rcx`.

---

## 2. Public API (disasm.h)

Namespace `rcx`. Two free functions; no types/structs/enums are declared in the header.

### 2.1 `QString disassemble(const QByteArray& bytes, uint64_t baseAddr, int bitness, int maxBytes = 128)`

`disasm.h:10`. Disassembles up to `maxBytes` of x86 code.

Parameters:
- `bytes` — raw code bytes (already read from the target). Borrowed, not consumed.
- `baseAddr` — the absolute virtual address that `bytes[0]` corresponds to. Used both for the per-line address prefix AND passed into the decoder so RIP-relative / relative-branch targets resolve to absolute addresses.
- `bitness` — must be **32 or 64**. Any other value → returns empty string.
- `maxBytes` — cap on how many bytes to consider (default 128). The actual byte window is `min(bytes.size(), maxBytes)`.

Return: newline-joined (`\n`) string, **one line per decoded instruction**, each line:
```
<zero-padded-hex-address>  <formatted-instruction>
```
Note the **two-space separator** between address and mnemonic (load-bearing — see §5). Empty string on empty input / invalid bitness / immediate decode failure of the first byte.

Rust signature equivalent:
```rust
pub fn disassemble(bytes: &[u8], base_addr: u64, bitness: u32, max_bytes: usize) -> String
```
(In C++ `maxBytes` defaults to 128; provide an overload or `Option`/builder in Rust, or just always pass 128 as callers do.)

### 2.2 `QString hexDump(const QByteArray& bytes, uint64_t baseAddr, int maxBytes = 128)`

`disasm.h:13`. Formats bytes as a hex dump, **16 bytes per line**, with an ASCII sidebar.

Parameters:
- `bytes` — raw bytes to dump (borrowed).
- `baseAddr` — absolute address of `bytes[0]`; used for the per-line address column.
- `maxBytes` — cap (default 128). Window is `min(bytes.size(), maxBytes)`.

Return: newline-joined string, one line per 16-byte row. Empty on empty input.

Rust signature equivalent:
```rust
pub fn hex_dump(bytes: &[u8], base_addr: u64, max_bytes: usize) -> String
```

---

## 3. Implementation, line-by-line (disasm.cpp)

### 3.1 `disassemble` (disasm.cpp:9-36)

```cpp
QString disassemble(const QByteArray& bytes, uint64_t baseAddr, int bitness, int maxBytes) {
    if (bytes.isEmpty() || (bitness != 32 && bitness != 64))   // :10
        return {};                                              // :11

    int len = qMin((int)bytes.size(), maxBytes);                // :13
    const auto* buf = reinterpret_cast<const uint8_t*>(bytes.constData()); // :14

    QString result;                                             // :16
    int off = 0;                                                // :17
    while (off < len) {                                         // :18
        FdInstr instr;                                          // :19
        int ret = fd_decode(buf + off, len - off, bitness, baseAddr + off, &instr); // :20
        if (ret < 0)                                            // :21
            break;                                              // :22

        char fmtBuf[128];                                       // :24
        fd_format(&instr, fmtBuf, sizeof(fmtBuf));              // :25

        if (!result.isEmpty())                                  // :27
            result += QLatin1Char('\n');                        // :28
        result += QStringLiteral("%1  %2")                      // :29
            .arg(baseAddr + off, bitness == 64 ? 16 : 8, 16, QLatin1Char('0')) // :30
            .arg(QString::fromLatin1(fmtBuf));                  // :31

        off += ret;                                             // :33
    }
    return result;                                              // :35
}
```

**Step by step:**

1. **Guard** (`:10-11`): if `bytes` empty OR `bitness` not exactly 32 or 64 → return `{}` (default-constructed `QString`, i.e. empty). Edge: `bitness == 16` returns empty (tested: `testDisasm_invalidBitness`). `bitness == 0`, negative, 48, etc. all return empty.
2. **Window length** (`:13`): `len = min(bytes.size(), maxBytes)`. `bytes.size()` is cast to `int` first (Qt `QByteArray::size()` returns `qsizetype`/`int`). `maxBytes` default 128.
3. **Buffer pointer** (`:14`): reinterpret the `QByteArray` data as `const uint8_t*`. In Rust this is just the `&[u8]` slice; take `&bytes[..len]`.
4. **Decode loop** (`:18-34`): iterate `off` from 0 while `off < len`:
   - Call `fd_decode(buf + off, len - off, bitness, baseAddr + off, &instr)`.
     - Args: pointer into buffer, **remaining byte count** (`len - off`), bitness (32/64), **the instruction's own absolute address** (`baseAddr + off`), and out-param `FdInstr`.
     - Returns the **number of bytes consumed** on success (a positive int), or a **negative value on failure** (truncated/invalid instruction).
   - **On `ret < 0`** (`:21-22`): `break` out of the loop. Whatever was decoded so far is returned. The remaining bytes are silently dropped. **No error is propagated.** (If the very first byte fails, result is empty.)
   - **On success**: format into a fixed 128-byte stack buffer with `fd_format(&instr, fmtBuf, sizeof(fmtBuf))`. fadec writes a NUL-terminated string; if it would exceed 128 bytes it truncates (does not overflow). 128 is ample for any single x86 instruction text.
   - **Line assembly** (`:27-31`):
     - If `result` already has content, append `'\n'` first (so there is no trailing/leading newline — lines are *joined*, not *terminated*).
     - Append `"%1  %2"` where `%1` = address `baseAddr + off` formatted as hex, field width `16` if `bitness==64` else `8`, base 16, zero-padded with `'0'`; `%2` = the formatted instruction text decoded from Latin-1.
     - **Exactly two spaces** between the two fields.
   - **Advance** (`:33`): `off += ret`.
5. Return accumulated `result`.

**Critical invariants / edge cases:**
- **Address width is bitness-dependent**, not value-dependent: 64-bit → always 16 hex digits; 32-bit → always 8 hex digits. (`testDisasm64_addrWidth` expects the `"  "` separator at index 16; `testDisasm32_addrWidth` at index 8.) Contrast with `hexDump`, whose width is value-dependent.
- Each instruction's address prefix uses **its own offset** (`baseAddr + off`), so consecutive instructions show increasing addresses (`testDisasm64_pushMov` checks line 0 = `...401000`, line 1 = `...401001`).
- The `maxBytes` cap limits how many *bytes* are decoded; an instruction is only decoded if it starts at `off < len`. fadec is given `len - off` so it will not read past the window, but note: if a multi-byte instruction *starts* before `len` but would need bytes beyond `len`, fadec returns negative (insufficient bytes) and the loop breaks. So the effective output is "all complete instructions fully contained in the first `len` bytes."
- `testDisasm_maxBytes`: 200 `nop` bytes, `maxBytes=128` → exactly 128 lines (`count('\n')+1 == 128`). Each `nop` is 1 byte, so 128 bytes → 128 instructions.
- `testDisasm_empty`: empty input returns empty for both 64 and 32 bitness.

**Decoder semantics relied upon by tests (must match in iced-x86):**
- Mnemonics are **lowercase** (`push rbp`, `mov rbp, rsp`, `ret`, `nop`, `int3`).
- Operands separated by `", "` (comma + space): `xor eax, eax`, `mov rbp, rsp`.
- Immediates in **hex with `0x` prefix, lowercase**, no leading zeros: `sub rsp, 0x20`, `sub rsp, 0x28`.
- Memory operands: size keyword + `ptr` + bracketed expression: `qword ptr [rbx+0x10]`, `qword ptr [rsp+0x8]`. Base+disp uses `+` with `0x` displacement.
- **RIP-relative**: `lea rax, [rip+0x10]` — fadec renders the **raw displacement** form `[rip+0x10]` (NOT the resolved absolute address) for `lea`. (`testDisasm64_leaRipRel`, bytes `48 8d 05 10 00 00 00` at base `0x1000`.)
- **Relative branches resolve to absolute targets**: `call 0x1105` (base `0x1000`, 5-byte call, rel `0x100` → `0x1000+5+0x100 = 0x1105`); `jmp 0x1012` (base `0x1000`, 2-byte jmp, rel `0x10` → `0x1000+2+0x10 = 0x1012`). This is why `baseAddr + off` is passed to `fd_decode` as the instruction address. (`testDisasm64_callRel`, `testDisasm64_jmpRel`.)
- `int3` rendered as the single token `int3` (not `int 3`). (`testDisasm64_int3`.)
- 32-bit register names: `push ebp`, `mov ebp, esp`. (`testDisasm32_pushMov`.)

Full mnemonic table the tests pin (see §5 for the exact byte→text map).

### 3.2 `hexDump` (disasm.cpp:38-74)

```cpp
QString hexDump(const QByteArray& bytes, uint64_t baseAddr, int maxBytes) {
    if (bytes.isEmpty())                                        // :39
        return {};                                              // :40

    int len = qMin((int)bytes.size(), maxBytes);               // :42
    QString result;                                            // :43

    for (int off = 0; off < len; off += 16) {                  // :45
        int lineLen = qMin(16, len - off);                     // :46

        if (!result.isEmpty())                                 // :48
            result += QLatin1Char('\n');                       // :49

        // Address
        bool wide = (baseAddr + len > 0xFFFFFFFFULL);          // :52
        result += QStringLiteral("%1  ").arg(baseAddr + off, wide ? 16 : 8, 16, QLatin1Char('0')); // :53

        // Hex bytes
        for (int i = 0; i < 16; i++) {                         // :56
            if (i < lineLen) {                                 // :57
                uint8_t b = static_cast<uint8_t>(bytes[off + i]); // :58
                result += QStringLiteral("%1 ").arg(b, 2, 16, QLatin1Char('0')); // :59
            } else {                                           // :60
                result += QStringLiteral("   ");               // :61  (3 spaces)
            }
            if (i == 7) result += QLatin1Char(' ');            // :63  (extra middle gap)
        }

        // ASCII
        result += QLatin1Char(' ');                            // :67
        for (int i = 0; i < lineLen; i++) {                    // :68
            char c = bytes[off + i];                           // :69
            result += (c >= 0x20 && c < 0x7f) ? QLatin1Char(c) : QLatin1Char('.'); // :70
        }
    }
    return result;                                             // :73
}
```

**Step by step:**

1. **Guard** (`:39-40`): empty input → empty string. (Note: unlike `disassemble`, there is **no bitness param** and no validity check beyond emptiness.)
2. **Window** (`:42`): `len = min(bytes.size(), maxBytes)`, default 128.
3. **Row loop** (`:45`): step `off` by 16 while `off < len`. Number of rows = `ceil(len/16)`.
   - `lineLen = min(16, len - off)` — bytes available on this row (last row may be partial).
   - **Line separator** (`:48-49`): prepend `'\n'` if not the first row. Lines are joined, no trailing newline.
   - **Address column** (`:52-53`):
     - `wide = (baseAddr + len > 0xFFFFFFFF)`. **Crucial: width is decided once for the whole dump** from `baseAddr + len` (the end of the dumped range), NOT per-row and NOT from `baseAddr` alone. So a dump that *ends* above 4 GiB uses 16-digit addresses on every row.
     - Format: `"%1  "` = address `baseAddr + off`, width `16` if `wide` else `8`, base 16, zero-pad `'0'`, **followed by two spaces**.
   - **Hex byte columns** (`:56-64`): exactly 16 column slots, `i = 0..15`:
     - If `i < lineLen`: byte `bytes[off+i]` as **2 hex digits, zero-padded, lowercase**, then **one trailing space** → `"%1 "`.
     - Else (padding for a short final row): emit **3 spaces** `"   "` (placeholder so the ASCII column still aligns).
     - After column `i == 7` (the 8th byte), append **one extra space** — the classic "middle gap" between the two 8-byte halves. This applies in both the byte-present and padding branches, and is applied for **every** row regardless of `lineLen`.
   - **ASCII column** (`:67-71`):
     - Prepend **one space** before the ASCII section.
     - For `i = 0..lineLen-1` (only actual bytes, no padding): char `c = bytes[off+i]`; if printable (`0x20 <= c < 0x7f`) emit it as Latin-1, else emit `'.'`.
4. Return joined `result`.

**Exact column geometry of one full (16-byte) row:**
```
<addr><2sp> [hh' ' ×8] <1sp> [hh' ' ×8] <1sp> <16 ascii chars>
```
- Address: 8 or 16 hex + 2 spaces.
- 16 hex byte slots, each "hexhex" + 1 trailing space (3 chars each = 48 chars), PLUS one extra space inserted after slot 7.
- One space, then ASCII chars (no trailing padding on the ASCII side; short rows have fewer ASCII chars and fewer-but-padded hex slots).

**Edge cases / tests:**
- `testHexDump_empty`: empty → empty.
- `testHexDump_basic`: 32 bytes (values 0..31) at `0x1000`, maxBytes 128 → 2 lines (`32/16=2`); starts with `"00001000"` (8-digit because `0x1000+32` < 4 GiB).
- `testHexDump_maxBytes`: 200 bytes `0xAA`, maxBytes 64 → `count('\n')+1 == 4` rows (`64/16=4`).
- `testHexDump_wideAddr`: 16 bytes at `0x100000000` (= 4 GiB) → first line starts `"0000000100000000"` (16-digit, because `baseAddr+len = 0x100000010 > 0xFFFFFFFF`).
- `testHexDump_hexValues`: bytes `DE AD BE EF` then zeros → output contains `"de ad be ef"` (lowercase, space-separated; matched case-insensitively by the test but emitted lowercase by Qt's `%1` base-16).
- `testHexDump_ascii`: `"Hello, World!xx"` (15 bytes) → contains `"Hello"` in the ASCII sidebar.
- `testHexDump_nonPrintable`: 16 bytes, `[0]='A'`, `[15]='Z'`, rest `\0` → ASCII sidebar contains `"A..............Z"` (A, 14 dots for the NULs, Z). Note 14 dots = 16 - 2 printable.
- `testHexDump_secondLineAddr`: 32 bytes at `0x2000` → line[1] starts `"00002010"` (`0x2000 + 16 = 0x2010`, 8-digit).

---

## 4. Decoder integration: fadec → iced-x86

### 4.1 What the C++ uses

- Header `<fadec.h>` included `extern "C"` (`disasm.cpp:3-5`).
- `FdInstr` — opaque decoded-instruction struct (stack-allocated, `disasm.cpp:19`).
- `int fd_decode(const uint8_t* buf, size_t len, int mode /*32|64*/, uint64_t address, FdInstr* out)` — returns bytes consumed (>0) or negative on error.
- `void fd_format(const FdInstr* instr, char* buf, size_t len)` — writes NUL-terminated AT&T?-style... **no: Intel-syntax, lowercase** text into `buf`.
- Build: fadec is a git submodule at `third_party/fadec`. Its decode tables are generated at CMake configure time by running `parseinstrs.py decode instrs.txt ... --32 --64` (CMakeLists `:76-92`). Compiled sources: `third_party/fadec/decode.c` and `format.c` (`:196-197`, `:528`). The Rust port drops all of this.

### 4.2 fadec output style (must be reproduced by iced-x86 formatter config)

From the tests, fadec's `fd_format` emits **Intel syntax** with these specific conventions:

| Property | fadec output | iced-x86 setting to match |
|---|---|---|
| Mnemonic case | lowercase (`mov`, `push`, `nop`, `ret`, `int3`) | `Formatter::options_mut().set_uppercase_mnemonics(false)` (default lowercase) |
| Operand separator | `", "` | default in iced |
| Immediate radix | hex, `0x` prefix, lowercase, no leading zeros (`0x20`, `0x10`) | `set_hex_prefix("0x")`, `set_hex_suffix("")`, `set_uppercase_hex(false)`, `set_leading_zeroes(false)` |
| Small immediates | `0x8`, `0x10` (not `0x08`) | as above, leading zeros off |
| Memory size keyword | `qword ptr`, `dword ptr` (always shown) | `set_always_show_memory_size_keyword`/`MemorySizeOptions::Always` so `qword ptr` appears |
| Memory expr | `[rbx+0x10]`, `[rsp+0x8]` | default Intel; ensure `+` sign for positive disp |
| RIP-relative | `[rip+0x10]` (NOT resolved abs addr) | `set_rip_relative_addresses(false)` (keep symbolic rip+disp) |
| Branch targets | absolute resolved (`call 0x1105`, `jmp 0x1012`) | iced resolves rel branches to absolute by default when given the instruction IP; ensure branch immediates are shown as the absolute target |
| `int3` | single token `int3` | iced renders `int3` |
| Register names | bare (`rax`, `rbp`, `eax`) no `%` prefix | Intel syntax (not GAS) |

The **iced `IntelFormatter`** (the "masm"/"intel"/"nasm" family) is the right match. The **`NasmFormatter`** or **`IntelFormatter`** (a.k.a. the "intel" formatter) most closely matches. Concretely, the closest is iced's `IntelFormatter` with options:
- `uppercase_*` all false (defaults).
- `hex_prefix = "0x"`, `hex_suffix = ""` (NASM/Intel formatters default to `0x` prefix? — verify; the masm formatter uses `h` suffix, so prefer **NasmFormatter** or **IntelFormatter**, NOT MasmFormatter).
- `memory_size_options = MemorySizeOptions::Always` (so `qword ptr` is always present, matching fadec).
- `rip_relative_addresses = false` (so `[rip+0x10]` stays symbolic).
- `leading_zeroes = false`.
- Branch operand: keep default (absolute). iced shows e.g. `call 0000000000001105h` by default for MASM; with the right hex prefix/suffix config it must become `call 0x1105`. **Confirm leading-zero suppression** so it is `0x1105` not `0x0000000000001105`.

> IMPORTANT PARITY NOTE: The exact spacing/sign rules between fadec and iced may differ (e.g. `[rbx+0x10]` vs `[rbx+10h]`, or `0x8` vs `8h`). The Rust port MUST configure the iced formatter to produce byte-identical strings to the fadec expectations baked into `test_disasm.cpp`, and the ported tests should assert the same exact strings:
> - `push rbp`, `mov rbp, rsp`, `ret`, `nop`, `xor eax, eax`, `sub rsp, 0x20`, `int3`, `push rdi`, `pop rsi`, `test eax, eax`
> - `lea rax, [rip+0x10]`
> - `call 0x1105`, `jmp 0x1012`
> - `mov rax, qword ptr [rbx+0x10]`, `mov qword ptr [rsp+0x8], rcx`
> - `push ebp`, `mov ebp, esp` (32-bit)
> - `sub rsp, 0x28`
> Achieving these exact strings is the single most important fidelity requirement of this subsystem. Pin them in a Rust unit test mirroring `test_disasm.cpp`.

### 4.3 iced-x86 decode loop equivalent

```rust
use iced_x86::{Decoder, DecoderOptions, Formatter, IntelFormatter};

pub fn disassemble(bytes: &[u8], base_addr: u64, bitness: u32, max_bytes: usize) -> String {
    if bytes.is_empty() || (bitness != 32 && bitness != 64) {
        return String::new();
    }
    let len = bytes.len().min(max_bytes);
    let window = &bytes[..len];

    let mut decoder = Decoder::with_ip(bitness, window, base_addr, DecoderOptions::NONE);
    let mut formatter = IntelFormatter::new();
    // configure options here to match fadec exactly (see table above)

    let mut result = String::new();
    let mut text = String::new();
    while decoder.can_decode() {
        let instr = decoder.decode();
        if instr.is_invalid() {        // mirrors fadec ret < 0 -> break
            break;
        }
        text.clear();
        formatter.format(&instr, &mut text);
        if !result.is_empty() {
            result.push('\n');
        }
        let addr_width = if bitness == 64 { 16 } else { 8 };
        // instr.ip() == base_addr + off
        result.push_str(&format!("{:0width$x}  {}", instr.ip(), text, width = addr_width));
    }
    result
}
```

Key mapping notes:
- `Decoder::with_ip(bitness, data, ip, options)` sets the starting IP; `decoder.decode()` advances IP automatically, and `instr.ip()` gives `base_addr + off` — exactly what C++ formats. The remaining-byte handling (`len - off`) is implicit in the decoder's internal position.
- `decoder.can_decode()` replaces `off < len`.
- `instr.is_invalid()` (the decoded mnemonic is `Mnemonic::INVALID` / instruction code `Code::INVALID`) is the equivalent of fadec's `ret < 0` → `break`. **Important subtlety:** iced, unlike fadec, will return an "invalid" instruction that *still consumed bytes* and keep going if you let it. The C++ behavior is to **stop entirely** at the first decode failure, so the Rust port must `break` on `is_invalid()`, NOT `continue`. Also, at end-of-buffer iced may produce a truncated/invalid instruction; breaking on invalid matches fadec returning negative for insufficient bytes.
- The two-space separator and zero-padded width must be reproduced verbatim (`{:0width$x}  {}`).

### 4.4 hexDump has no decoder dependency

`hexDump` is pure byte/string formatting — port directly with `std::fmt`/`String`. No crate needed. See §6 for the Rust formatting recipe.

---

## 5. Output format details (load-bearing for tests)

These exact formats are asserted by the test suite and by the editor's `mnemonic()` helper (`test_disasm.cpp:9-12`), which splits each line on the **first occurrence of `"  "` (two spaces)** and takes everything after it:
```cpp
static QString mnemonic(const QString& line) {
    int sep = line.indexOf("  ");
    return sep >= 0 ? line.mid(sep + 2) : line;
}
```
Therefore:
- The address field MUST be followed by **exactly two spaces** and the instruction text MUST NOT contain a double-space before its first single-space-separated token, or `indexOf("  ")` would land in the wrong place. (fadec never emits a double space inside an instruction; iced config must avoid that too.)
- For 64-bit, `indexOf("  ") == 16` (16-hex address). For 32-bit, `== 8`. (`testDisasm64_addrWidth`, `testDisasm32_addrWidth`.)

### Disassembly line format
```
<addr:hex, width 16 if bitness==64 else 8, zero-padded, lowercase><2 spaces><instr-text>
```
Examples (from tests):
- `0000000000401000  push rbp`
- `0000000000401001  mov rbp, rsp`
- `0000000140001000  push rbp` (base `0x140001000`)
- `0000000000401000` / `00401000` etc.

### HexDump line format (full 16-byte row)
```
<addr:hex, width 8 or 16, zero-padded><2 spaces><b0 ><b1 >...<b7 ><1 extra space><b8 >...<b15 ><1 space><ascii...>
```
- Width 16 iff `baseAddr + len > 0xFFFFFFFF`, else 8 (decided once for whole dump).
- Each byte: 2 lowercase hex digits + 1 space.
- Extra single space after the 8th byte (index 7).
- One space before the ASCII column.
- ASCII: printable `[0x20, 0x7f)` as-is, else `.`; only for real bytes (no padding dots for missing bytes on a short final row).
- Missing-byte hex slots on a short final row are 3 spaces each (keeping alignment).

### Mnemonic reference table the tests pin
| Bytes (hex) | bitness | base | Expected `mnemonic()` |
|---|---|---|---|
| `55` | 64 | any | `push rbp` |
| `48 89 e5` | 64 | any | `mov rbp, rsp` |
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
| `89 e5` | 32 | 0x401000 | `mov ebp, esp` |

(Sequences: prologue `55 48 89 e5 48 83 ec 20 c3` → push rbp / mov rbp, rsp / sub rsp, 0x20 / ret. Five `90` → five `nop`. `31 c0 c3` → xor eax, eax / ret.)

No binary serialization, no persistence — the only "format" is this text output, which is ephemeral (rendered into a popup label).

---

## 6. Qt → Rust type/idiom mapping

| C++ / Qt | Meaning here | Rust equivalent |
|---|---|---|
| `QString` (return type) | UTF-16 string built up by concatenation | `String` (UTF-8) |
| `QByteArray` (input bytes) | raw byte buffer | `&[u8]` |
| `QByteArray::isEmpty()` | empty check | `slice.is_empty()` |
| `QByteArray::size()` cast to `int` | length | `slice.len()` (usize) |
| `QByteArray::constData()` → `reinterpret_cast<const uint8_t*>` | raw byte pointer | the slice itself / `slice.as_ptr()` (not needed) |
| `qMin(a, b)` | min | `a.min(b)` |
| `uint64_t` | 64-bit address | `u64` |
| `uint8_t` | byte | `u8` |
| `QLatin1Char('\n')`, `QLatin1Char('0')` | single Latin-1 char | `'\n'`, `'0'` |
| `QStringLiteral("%1  %2").arg(addr, width, 16, fillchar).arg(str)` | printf-style with hex, field width, zero fill | `format!("{:0width$x}  {}", addr, str, width = w)` |
| `.arg(b, 2, 16, QLatin1Char('0'))` (byte → 2 hex digits) | `format!("{:02x} ", b)` |
| `QString::fromLatin1(fmtBuf)` | decode C string as Latin-1 | from iced formatter output (already UTF-8 ASCII) |
| `QString::indexOf("  ")` (in test helper) | find substring | `str.find("  ")` |
| `QString::split('\n')` (tests) | split lines | `str.split('\n')` |
| `extern "C" { #include <fadec.h> }` | C decoder FFI | `iced_x86` crate (pure Rust, no FFI) |

Notes on `.arg(value, fieldWidth, base, fillChar)`:
- Positive `fieldWidth` = right-justify (pad on left). All uses here are positive → left-padding with `'0'` → `{:0N x}`/`{:0N$x}`.
- base 16, lowercase (Qt `%1` base-16 produces **lowercase** hex by default) → `{:x}`.
- For the byte case `.arg(b, 2, 16, '0')` → `{:02x}`.

All Qt usages here are trivial value formatting; **no Qt event loop, no widgets, no signals/slots, no Qt containers** beyond `QString`/`QByteArray`. The module is fully portable.

---

## 7. Caller integration (so the port wires the popups identically)

These functions are only consumed by two `HoverPreview` subclasses in `editor.cpp`. Reproduce their gating logic when porting the editor, but it does not affect `disasm` itself.

### 7.1 `DisasmPreview` (editor.cpp:1567-1591)
- `id() == "disasm"`, `tabLabel() == "Disassembly"`.
- **Eligible** iff `isFuncPtr(lm.nodeKind)` (FuncPtr32 or FuncPtr64) AND `readPointerAtRow(lm, ctx) != 0`.
- `widget()`:
  1. `target = readPointerAtRow(lm, ctx)`; bail if 0.
  2. `readProv = ctx.codeProvider ? ctx.codeProvider : ctx.dataProvider` — prefer the **real** process provider (can read arbitrary code addresses) over the snapshot.
  3. Read `kMax = 128` bytes at `target` via `readProv->read(target, buf, 128)`; bail if read fails.
  4. `is64 = (lm.nodeKind == FuncPtr64)`.
  5. `body = capLines(disassemble(bytes, target, is64 ? 64 : 32, 128))`. **`baseAddr` passed = `target`** (the function's absolute address).
  6. Bail if body empty; else build a monospace text widget colored with `theme.syntaxNumber`.

### 7.2 `HexDumpPreview` (editor.cpp:1539-1565)
- `id() == "hex_dump"`, `tabLabel() == "Hex Dump"`.
- **Eligible** iff node kind is Pointer32/Pointer64 AND `lm.pointerTargetName.isEmpty()` (i.e. NOT a typed pointer → typed ones get the StructTarget preview instead) AND `node.refId == 0` AND `readPointerAtRow != 0`.
- `widget()`: same read pattern; `body = capLines(hexDump(bytes, target, 128))`. Passes `target` as `baseAddr`.

### 7.3 `readPointerAtRow` (editor.cpp:1511-1523)
Reads the pointer **value** stored at the field's composed absolute address `lm.offsetAddr` from `ctx.dataProvider` (the snapshot/tree provider). `is64` for FuncPtr64/Pointer64 → `readU64`, else `readU32` (zero-extended). Returns 0 if value is `0`, `UINT64_MAX`, or (32-bit) `0xFFFFFFFF` — these are treated as null/sentinel → no popup.

### 7.4 `capLines` (editor.cpp:1525-1537)
Caps the body at `maxLines` (default 6) newlines: walks up to 6 `'\n'`, and if it hit exactly 6 with content remaining, truncates after the 6th newline and appends `"..."`. So both popups show at most ~6 lines plus an ellipsis. This is applied by the caller, **not** inside `disassemble`/`hexDump` (which always produce full output up to `maxBytes`).

### 7.5 Provider wiring (controller.cpp:2028-2044)
- `snapProv` (= `dataProvider`/`ctx.dataProvider`): snapshot provider if present, else the real provider — used for reading pointer *values* within the tree.
- `realProv` (= `codeProvider`/`ctx.codeProvider`): always the real process provider — used for reading *code* at arbitrary addresses (the snapshot only contains tree-data pages, not code pages). Passed to editors via `editor->setProviderRef(snapProv, realProv, &m_doc->tree)`.
- Tests `testHoverFlow_fullSimulation`, `testVTableDisasm_composedAddress`, `testVTableDisasm_wrongAddressGivesWrongCode` validate exactly this two-provider separation: read pointer values from the snapshot at `lm.offsetAddr` (the **composed** absolute address, not `node.offset`), then read code from the real provider at the pointer value, then `disassemble(code, ptrVal, 64, 128)`. These three tests exercise the `disasm` API only through `disassemble()`; their substantive assertions are about `compose()` producing the right `offsetAddr` (covered in the compose subsystem) plus the mnemonic strings already listed. The disasm-relevant assertions:
  - `disassemble(codeBytes, ptrVal, 64, 128)` is non-empty for valid code.
  - First lines decode to the expected mnemonics (`push rbp` / `ret`, `xor eax, eax` / `ret`, full prologue, `sub rsp, 0x28` / `nop` / `ret`).
  - The address prefix contains the function address substring (e.g. line 0 `.contains("200")`, `.contains("300")`) — i.e. the address column reflects `baseAddr` (= `ptrVal`).
  - Disassembling the *wrong* address (vtable data, not code) must NOT yield `sub rsp` — a sanity check that the data path matters, not a disasm-format requirement.

---

## 8. Platform-specificity, concurrency, error handling

- **Portability:** `disasm.cpp`/`.h` contain **zero** platform-specific code (no `#ifdef _WIN32`, no OS APIs). The decoder dependency (fadec/iced) is cross-platform. **Fully portable** — supports 32- and 64-bit x86 decoding regardless of host OS. (iced-x86 is pure Rust, builds everywhere including the Linux CI box.) Marked "mostly-portable" only because it depends on an external x86 decoder crate, but there is no `#[cfg(windows)]` needed.
- **Concurrency / threading:** none. Both functions are pure, reentrant, stateless. No locks, no shared mutable state, no `static` data. Safe to call from any thread. (The editor calls them on the UI thread inside hover handlers.)
- **Error handling:** by-value, silent. There are **no exceptions, no error codes, no `Result`** — failure modes all collapse to "return empty `QString`":
  - empty input → empty.
  - invalid bitness (disasm) → empty.
  - first instruction fails to decode → empty (loop breaks before appending).
  - mid-stream decode failure → returns the prefix decoded so far (partial result, no error).
  Callers treat an empty body as "no popup". The Rust port should return `String` (possibly empty) to match — do NOT return `Result`/`Option` unless callers are adapted; the contract is "empty string == nothing to show".
- **Memory safety note for the port:** the C++ uses a fixed `char fmtBuf[128]`; iced writes into a growable `String`, so no truncation concern. fadec truncates at 128 bytes silently, but no real x86 instruction's text approaches that, so behavior is equivalent.
- **Integer-width subtlety:** `len = qMin((int)bytes.size(), maxBytes)` casts size to `int`. With absurdly large inputs (>2 GiB) this could overflow in C++, but inputs are always ≤128 bytes in practice. In Rust use `usize` and `.min()`; no overflow.

---

## 9. Recommended Rust crates

- **`iced-x86`** — x86/x64 decoder + Intel-syntax formatter; replaces the fadec submodule. Use `Decoder::with_ip`, `IntelFormatter` (configured per §4.2), `instr.ip()`, `instr.is_invalid()`. Pure Rust, cross-platform, no codegen/submodule needed.
- Standard library only for `hex_dump` (`std::fmt` / `String`). No extra crate.

---

## 10. Implementer checklist (1:1 parity)

1. `disassemble(bytes, base_addr, bitness, max_bytes)`:
   - Guard empty / bitness∉{32,64} → `String::new()`.
   - Window `len = bytes.len().min(max_bytes)`.
   - `Decoder::with_ip(bitness, &bytes[..len], base_addr, DecoderOptions::NONE)`.
   - Loop `while decoder.can_decode()`: decode; `break` on `is_invalid()`; format; join with `\n`; line = `{:0w$x}  {text}` where `w = if bitness==64 {16} else {8}`, addr = `instr.ip()`.
   - Configure `IntelFormatter` to match fadec strings (lowercase, `0x` hex no leading zeros, `qword ptr` always, `[rip+0x..]` symbolic, absolute branch targets, `int3`).
2. `hex_dump(bytes, base_addr, max_bytes)`:
   - Guard empty → empty.
   - `len = bytes.len().min(max_bytes)`.
   - `wide = base_addr + (len as u64) > 0xFFFF_FFFF` (compute once).
   - For each 16-byte row: addr `{:0w$x}` (w=16 if wide else 8) + 2 spaces; 16 hex slots (`{:02x} ` present, `"   "` absent), extra space after slot 7; 1 space; ASCII for present bytes (`0x20..0x7f` literal else `.`); join rows with `\n`.
3. Port `test_disasm.cpp` 1:1, asserting the exact strings in §5. This is the acceptance gate.
4. Wire the two hover previews (DisasmPreview eligible on FuncPtr*, HexDumpPreview on untyped Pointer*) and the snapshot-vs-real provider split (§7) in the editor port — but those live in the editor subsystem, not `disasm`.

---

## 11. Open questions / verify during implementation
- **Exact iced formatter config** to byte-match fadec: confirm `IntelFormatter` vs `NasmFormatter` and the precise hex/leading-zero/memory-size/rip settings produce `lea rax, [rip+0x10]`, `mov rax, qword ptr [rbx+0x10]`, `call 0x1105`, `sub rsp, 0x20`, `int3` exactly. May require a couple of option tweaks; lock down via the ported tests.
- Whether iced renders the 2-byte `jmp eb 10` and 5-byte `call e8 ..` branch immediates as `0x1012`/`0x1105` (absolute, no leading zeros) out of the box — almost certainly yes with leading-zeros off, but verify.
- fadec's exact rendering of negative displacements / SIB scales is not exercised by tests; if the editor surfaces such code in practice, confirm iced matches (e.g. `[rax+rcx*4-0x8]`), but no test pins it, so parity there is best-effort against fadec's docs.
