# Subsystem: Address Expression Parser (`addressparser`)

Source of truth: `/home/loke/reclass-cpp/src/addressparser.h`,
`/home/loke/reclass-cpp/src/addressparser.cpp`, tested by
`/home/loke/reclass-cpp/tests/test_addressparser.cpp`.

This document is a complete behavioral map for a 1:1 Rust port. It is written so
the implementer never needs to re-read the C++.

---

## 1. Purpose

The Address Expression Parser turns a human-typed *address formula string* into a
concrete 64-bit address (or value). It is the engine behind every "address field"
in Reclass: the document base-address formula, the "Go to address" dialog, static
field offset expressions, scanner address columns, and bookmark address formulas.

It supports a small C-like expression language over **hexadecimal** numbers, with:

- Hex literals (`AB`, `0x1F4`, `7FF66CCE0000`), all base-16 always.
- Full C operator precedence: `|`, `^`, `&`, `<< >>`, `+ -`, `* /`, unary `-`/`~`,
  parentheses.
- Module base resolution: `<Program.exe>` → base address via callback.
- Pointer dereference chains: `[ ... ]` reads a pointer-sized value at an address
  via callback (nestable).
- Identifier resolution: C/C++ names (`base`, `e_lfanew`) resolved via callback.
- Bare module identifiers with extensions: `client.dll`, `cs2.exe` parsed as a
  single identifier token (resolved via the identifier callback).
- WinDbg `module!symbol` syntax scanned as a single identifier token.
- WinDbg backtick separators (`7ff6\`6cce0000`) and single quotes stripped before
  parsing.
- Built-in function calls for kernel paging: `vtop(pid, va)`, `cr3(pid)`,
  `phys(addr)`, each wired to optional callbacks.

It has two entry points:
- `evaluate(...)` — fully evaluate using supplied callbacks (live memory/symbols).
- `validate(...)` — syntax-only check with **no** callbacks (returns empty string
  if valid, else an error message).

The subsystem is **pure** (no Qt UI, no I/O of its own — all environment access is
through caller-supplied `std::function` callbacks). It is trivially portable.

---

## 2. Public API (header: `addressparser.h`)

### 2.1 `struct AddressParseResult` (`addressparser.h:8-13`)

| Field | C++ type | Meaning |
|-------|----------|---------|
| `ok` | `bool` | `true` if the expression parsed and evaluated successfully. |
| `value` | `uint64_t` | The resulting 64-bit value (only meaningful when `ok==true`; it is `0` on error). |
| `error` | `QString` | Human-readable error message (empty when `ok==true`). |
| `errorPos` | `int` | Character index into the *cleaned* input where the error occurred, or `-1` on success. |

Success result is constructed as `{true, value, {}, -1}` (`addressparser.cpp:55`).
Error result is constructed via `error(msg)` as `{false, 0, msg, m_errorPos}`
(`addressparser.cpp:78-80`). NOTE: on error `value` is always `0` and `errorPos`
is `m_errorPos` (see error-position semantics in §6).

Rust equivalent:
```rust
pub struct AddressParseResult {
    pub ok: bool,
    pub value: u64,
    pub error: String,   // empty when ok
    pub error_pos: i32,  // -1 on success
}
```
A more idiomatic `Result<u64, AddressParseError { message, pos }>` is acceptable
*provided* the success path yields `error_pos == -1` semantics and the error
messages match exactly (callers test `error.contains(...)` — see §7).

### 2.2 `struct AddressParserCallbacks` (`addressparser.h:15-24`)

A bag of optional `std::function` callbacks. All fields default to empty/unset.
Any callback may be missing; the parser checks each before use and falls back to
returning `0` for the corresponding construct when absent (syntax-check mode).

| Field | Signature | Meaning |
|-------|-----------|---------|
| `resolveModule` | `uint64_t(const QString& name, bool* ok)` | Resolve `<name>` → module base address. `*ok` set by callee. |
| `readPointer` | `uint64_t(uint64_t addr, bool* ok)` | Read a pointer-sized value at `addr` (the caller wires this to read exactly `ptrSize` bytes — see §5). `*ok` set by callee. |
| `resolveIdentifier` | `uint64_t(const QString& name, bool* ok)` | Resolve a C/C++ identifier / bare module name → value. `*ok` set by callee. |
| `vtop` | `uint64_t(uint32_t pid, uint64_t va, bool* ok)` | Kernel: virtual→physical translate. Optional, only wired when a kernel provider is active. |
| `cr3` | `uint64_t(uint32_t pid, bool* ok)` | Kernel: read CR3 for a pid. Optional. |
| `physRead` | `uint64_t(uint64_t physAddr, bool* ok)` | Kernel: read 8 bytes from a physical address. Optional. |

The `bool* ok` out-parameter convention: the callback **must** set `*ok` to
indicate success. The parser reads `*ok` after each call; if `false`, it fails
with a specific error message.

Rust equivalent: a struct of `Option<Box<dyn Fn(...) -> ...>>` fields. Because the
C++ uses a `bool* ok` out-param, model each callback as returning a value that
also signals success. Idiomatic mapping:

```rust
type ResolveFn   = Box<dyn Fn(&str) -> Option<u64>>;     // resolveModule / resolveIdentifier
type ReadPtrFn   = Box<dyn Fn(u64) -> Option<u64>>;      // readPointer
type VtopFn      = Box<dyn Fn(u32, u64) -> Option<u64>>; // vtop
type Cr3Fn       = Box<dyn Fn(u32) -> Option<u64>>;      // cr3
type PhysReadFn  = Box<dyn Fn(u64) -> Option<u64>>;      // physRead

#[derive(Default)]
pub struct AddressParserCallbacks {
    pub resolve_module: Option<ResolveFn>,
    pub read_pointer: Option<ReadPtrFn>,
    pub resolve_identifier: Option<ResolveFn>,
    pub vtop: Option<VtopFn>,
    pub cr3: Option<Cr3Fn>,
    pub phys_read: Option<PhysReadFn>,
}
```
`Some(value)` ⇔ `*ok = true`; `None` ⇔ `*ok = false`. Where a C++ callback returns
a value *and* `*ok` (e.g. returns 0 with `*ok=true`), `Some(0)` is correct.

### 2.3 `class AddressParser` (`addressparser.h:26-31`)

Two static methods (no state, no instances).

#### `static AddressParseResult AddressParser::evaluate(const QString& formula, int ptrSize = 8, const AddressParserCallbacks* cb = nullptr)`
Defined `addressparser.cpp:506-522`.
- **Params:**
  - `formula` — the raw user string.
  - `ptrSize` — pointer size in bytes; **default 8**. *The parser itself never
    uses `ptrSize`* — it is `Q_UNUSED` (`addressparser.cpp:511`). It exists only so
    the signature documents the intended pointer width; the *caller* uses it to
    configure how `readPointer` reads memory (read N bytes). See §5.
  - `cb` — optional pointer to callbacks; may be `nullptr` (then all callback-backed
    constructs return 0 — pure syntax mode).
- **Behavior:**
  1. Copy `formula` into `cleaned`.
  2. `cleaned.remove('`')` — strip **all** backtick characters (WinDbg separator).
     (`addressparser.cpp:517`)
  3. `cleaned.remove('\'')` — strip **all** single-quote characters (digit grouping).
     (`addressparser.cpp:518`)
  4. Construct `ExpressionParser(cleaned, cb)` and `return parser.parse()`.
- **Returns** the `AddressParseResult`. Note: `evaluate` does **not** `trimmed()`
  the input (the parser handles leading/trailing whitespace itself via
  `skipSpaces`). Contrast with `validate` which *does* call `trimmed()`.

Rust signature suggestion (keep the default ptrSize=8 via a builder or explicit
arg; `ptr_size` is effectively unused by the parser):
```rust
pub fn evaluate(formula: &str, ptr_size: i32, cb: Option<&AddressParserCallbacks>) -> AddressParseResult
```

#### `static QString AddressParser::validate(const QString& formula)`
Defined `addressparser.cpp:524-538`.
- **Behavior:**
  1. Copy → `cleaned`; remove all `` ` `` and `'` (same as evaluate).
  2. `cleaned = cleaned.trimmed()` — strip leading/trailing whitespace
     (`addressparser.cpp:529`). (evaluate does NOT do this; validate does.)
  3. If `cleaned.isEmpty()` → return `QStringLiteral("empty")`
     (`addressparser.cpp:530-531`). This is a special fast-path: empty input yields
     the literal `"empty"` from `validate`, distinct from the parser's own
     `"empty expression"` message.
  4. Construct `ExpressionParser(cleaned, nullptr)` — **no callbacks**. All
     module/dereference/identifier/function constructs therefore succeed and
     produce `0`. This validates **syntax only**.
  5. `auto result = parser.parse();`
  6. Return `result.ok ? QString() : result.error`. I.e. **empty string ⇔ valid**;
     non-empty string is the error message.
- **Returns:** `QString` (empty if valid).

Rust: `pub fn validate(formula: &str) -> String` (empty == valid), or
`-> Option<String>`/`Result<(), String>` — but keep the `"empty"` literal and the
"valid → empty/None" contract; callers test `validate(...).isEmpty()`.

---

## 3. Internal `ExpressionParser` (the actual engine)

Defined entirely in `addressparser.cpp:37-502`. A single-pass recursive-descent
parser over the cleaned string. It is the only place real logic lives. In Rust,
implement it as a private struct with the same fields and methods.

### 3.1 State (`addressparser.cpp:59-63`)

| Field | Type | Init | Meaning |
|-------|------|------|---------|
| `m_input` | `const QString&` | ctor | The cleaned input (held by reference). |
| `m_callbacks` | `const AddressParserCallbacks*` | ctor | Callback bag (may be null). |
| `m_pos` | `int` | `0` | Current scan position (UTF-16 code-unit index into `m_input`). |
| `m_error` | `QString` | `{}` | Last error message set by `fail()`. |
| `m_errorPos` | `int` | `0` | Position recorded at the last `fail()` (or overridden in hex parsing). |

Constructor (`addressparser.cpp:39-40`): stores input ref + callbacks pointer.

Rust note: `m_pos` indexes UTF-16 code units in Qt. Since all meaningful tokens
are ASCII, indexing over a `Vec<char>` (or chars collected once) is the simplest
faithful model; positions in `error_pos` will then be char indices. For exact
parity with the C++ `errorPos` (UTF-16 units), if non-ASCII can appear the only
fully faithful approach is to operate on a `Vec<u16>` of UTF-16 units. In practice
inputs are ASCII; document this assumption. **Recommendation:** collect input into
`Vec<char>` and treat positions as char indices (matches behavior for all ASCII
inputs, which is everything the tests and real usage produce).

### 3.2 Helper methods

- `bool atEnd() const` (`addressparser.cpp:67`): `m_pos >= m_input.size()`.
- `QChar peek() const` (`addressparser.cpp:69`): returns `'\0'` if `atEnd()`, else
  `m_input[m_pos]`. **Important:** `peek()` is null-safe — it returns NUL at end,
  never out-of-bounds. Many comparisons rely on this (e.g. `peek() == '|'` is
  simply false at end).
- `void advance()` (`addressparser.cpp:71`): `m_pos++`. No bounds check (callers
  only advance after a successful `peek`/length check).
- `void skipSpaces()` (`addressparser.cpp:73-76`): advance while
  `!atEnd() && m_input[m_pos].isSpace()`. Qt's `QChar::isSpace()` is Unicode-aware
  (space, tab, newline, CR, vertical tab, form feed, plus Unicode separators).
  Rust equivalent: `char::is_whitespace()` (close enough; both treat ASCII spaces,
  `\t \n \r \x0B \x0C`). For strict parity use a helper matching `QChar::isSpace`:
  it returns true for `0x09..=0x0D`, `0x20`, `0x85` (NEL), `0xA0` (NBSP), and
  Unicode space separators. ASCII whitespace is all that matters in practice.
- `AddressParseResult error(const QString& msg) const` (`addressparser.cpp:78-80`):
  builds `{false, 0, msg, m_errorPos}`.
- `bool fail(const QString& msg)` (`addressparser.cpp:82-86`): sets
  `m_error = msg`, `m_errorPos = m_pos`, returns `false`. The convention
  throughout: any production returns `false` on error after calling `fail(...)`
  (which records the message+position). The caller chain propagates `false` up.
- `bool expect(QChar ch)` (`addressparser.cpp:88-94`): `skipSpaces()`; if
  `peek() != ch` → `fail("expected '<ch>'")` and return false; else `advance()`,
  return true. (Used for `]`, `)` and `,`-adjacent constructs via direct checks.)
- `static bool isHexDigit(QChar)` (`addressparser.cpp:96-100`): `0-9`, `a-f`,
  `A-F`. **ASCII only** — not locale/Unicode digits.
- `static bool isIdentStart(QChar)` (`addressparser.cpp:102-104`): `a-z`, `A-Z`,
  `_`.
- `static bool isIdentChar(QChar)` (`addressparser.cpp:106-108`): `isIdentStart`
  OR `0-9`.

### 3.3 `parse()` — top-level (`addressparser.cpp:42-56`)

1. `skipSpaces()`.
2. If `atEnd()` → `return error("empty expression")`. (Distinct from validate's
   `"empty"`.)
3. `uint64_t value = 0; if (!parseBitwiseOr(value)) return error(m_error);`
   — parse the whole expression at the lowest-precedence level. On failure,
   build the error result from `m_error`/`m_errorPos`.
4. `skipSpaces()`.
5. If `!atEnd()` → `return error("unexpected '<char>'")` where `<char>` is
   `m_input[m_pos]`. NOTE: the error message uses `m_input[m_pos]` directly, and
   `errorPos` here is whatever `m_errorPos` currently holds (it is **not** updated
   to `m_pos` on this trailing-garbage path — see §6 subtlety).
6. Return `{true, value, {}, -1}`.

---

## 4. Grammar & precedence (lowest → highest binding)

The grammar comment is at `addressparser.cpp:17-30`. Precedence ladder, loosest
first (each level calls the next-tighter level):

```
bitwiseOr  = bitwiseXor ('|' bitwiseXor)*
bitwiseXor = bitwiseAnd ('^' bitwiseAnd)*
bitwiseAnd = shift      ('&' shift)*
shift      = expr       (('<<' | '>>') expr)*
expr       = term       (('+' | '-') term)*
term       = unary      (('*' | '/') unary)*
unary      = '-' unary | '~' unary | atom
atom       = '[' bitwiseOr ']'    (dereference)
           | '<' moduleName '>'   (module base)
           | '(' bitwiseOr ')'    (grouping)
           | identifier           (callback resolution; or function call)
           | hexLiteral
```

This is standard C precedence ordering. All binary operators are **left
associative** (implemented with `for(;;)` loops). Unary `-`/`~` are right
associative (recursive). Note bracketed/parenthesized sub-expressions re-enter at
`parseBitwiseOr` (the top), so anything is allowed inside `[]` and `()`.

### 4.1 Binary-operator levels — exact implementations

All five binary levels share the same shape: parse one operand at the tighter
level, then loop: `skipSpaces()`, peek for the operator, if not present `break`,
else `advance()` past it, parse RHS at the tighter level (propagating failure),
and fold into `result` with the C operator. All arithmetic is on `uint64_t` with
**C wrapping semantics** (no overflow checks). Rust must use **wrapping
arithmetic** (`wrapping_add`, `wrapping_sub`, `wrapping_mul`, `wrapping_shl`,
`wrapping_shr`) to match.

- `parseBitwiseOr` (`:113-127`): operand=`parseBitwiseXor`; op `'|'`; `result |= rhs`.
- `parseBitwiseXor` (`:130-144`): operand=`parseBitwiseAnd`; op `'^'`; `result ^= rhs`.
- `parseBitwiseAnd` (`:147-161`): operand=`parseShift`; op `'&'`; `result &= rhs`.
- `parseShift` (`:164-183`): operand=`parseExpression`. Loop:
  - `skipSpaces()`, `c = peek()`; if `c != '<' && c != '>'` → break.
  - **Must be a doubled operator:** if `m_pos+1 >= size` OR `m_input[m_pos+1] != c`
    → **break** (do NOT consume). This is how `<` (module open) and a lone `>` are
    distinguished from `<<`/`>>`. (`addressparser.cpp:172-174`)
  - `isLeft = (c == '<')`; `advance(); advance();` (skip both chars).
  - parse RHS via `parseExpression`; `result = isLeft ? result << rhs : result >> rhs`.
  - **Edge case:** if `rhs >= 64`, C++ `<<`/`>>` on `uint64_t` is *undefined
    behavior*; in practice compilers mask the count to 6 bits (`rhs & 63`) on
    x86/x64 via the `shl/shr` instruction. **For 1:1 parity use Rust
    `wrapping_shl`/`wrapping_shr`**, which mask the shift amount to `rhs % 64` —
    matching the typical x86 codegen of the C++ original. (No test exercises
    `rhs >= 64`, but this is the safest faithful choice.)
- `parseExpression` (`:186-204`): operand=`parseTerm`; ops `'+'`/`'-'`;
  `result = (op=='+') ? result + rhs : result - rhs` (wrapping).
- `parseTerm` (`:207-231`): operand=`parseUnary`; ops `'*'`/`'/'`:
  - `'*'`: `result *= rhs` (wrapping).
  - `'/'`: **if `rhs == 0` → `fail("division by zero")`** (`:225-226`), else
    `result /= rhs` (integer division, truncating toward zero — both operands
    unsigned so it's floor division).

### 4.2 `parseUnary` (`addressparser.cpp:234-253`)

1. `skipSpaces()`.
2. If `peek() == '-'`: `advance()`; recursively `parseUnary(inner)`; on success
   `result = (uint64_t)(-(int64_t)inner)` — i.e. two's-complement negation. In
   Rust: `result = (inner as i64).wrapping_neg() as u64`, or equivalently
   `0u64.wrapping_sub(inner)` / `inner.wrapping_neg()`. Test `unaryMinus`
   (`-0x10 + 0x20`) expects `0x10`: `-0x10` = `0xFFFFFFFFFFFFFFF0`, `+ 0x20`
   wraps to `0x10`. (`addressparser.cpp:241`)
3. Else if `peek() == '~'`: `advance()`; `parseUnary(inner)`; `result = ~inner`
   (bitwise NOT). Test `unaryNot` (`~0`) → `0xFFFFFFFFFFFFFFFF`. (`:244-251`)
4. Else: `return parseAtom(result)`.

Note: there is **no unary `+`** and **no unary `!`/logical-not**. A leading `+`
falls through to `parseAtom`, which will `fail` (not a valid atom start).
Multiple unary operators stack via recursion (`--x`, `~~x`, `-~x` all valid).

### 4.3 `parseAtom` (`addressparser.cpp:256-272`)

1. `skipSpaces()`; if `atEnd()` → `fail("unexpected end of expression")`.
2. `ch = peek()`:
   - `'['` → `parseDereference`.
   - `'<'` → `parseModuleName`.
   - `'('` → `parseGrouping`.
   - `isIdentStart(ch)` → `parseIdentifierOrHex` (identifiers checked **before**
     hex, because `a-f`/`A-F` are both hex digits and identifier-start chars).
   - else → `parseHexNumber`.

---

## 5. Atom productions — exact behavior

### 5.1 `parseIdentifierOrHex` (`addressparser.cpp:279-342`)

This is the trickiest production. It disambiguates identifiers from hex literals
and handles module-extension / WinDbg-bang token extension and function calls.

Algorithm:
1. `start = m_pos`; `hasNonHex = false`.
2. **Scan ident chars:** while `!atEnd() && isIdentChar(peek())`: if not a hex
   digit, set `hasNonHex = true`; `advance()`. (`:284-288`)
3. **Handle `module.ext`** (`:291-302`): if `!atEnd() && peek() == '.' && m_pos > start`:
   - `dotPos = m_pos`; `advance()` (skip `.`); `extStart = m_pos`.
   - Scan ident chars: while `!atEnd() && isIdentChar(peek())` advance.
   - If `m_pos > extStart` (there *was* an extension): `hasNonHex = true` — the
     `.` makes it definitively an identifier (e.g. `client.dll`, `cs2.exe`).
   - Else (`.` at end, no ext): `m_pos = dotPos` (backtrack; the `.` is not part of
     the token). Only a **single** `.`+ext is consumed (no `a.b.c`).
4. **Handle `module!symbol`** (`:305-316`): if `!atEnd() && peek() == '!' && m_pos > start`:
   - `bangPos = m_pos`; `advance()` (skip `!`).
   - If `!atEnd() && isIdentStart(peek())`: `hasNonHex = true`; scan ident chars
     (while `isIdentChar`) — extends the token across `!`.
   - Else: `m_pos = bangPos` (backtrack; trailing `!` is not module!symbol).
   - Note order: `.ext` is handled *before* `!symbol`. A token like `a.b!c` would
     consume `.b` then `!c`. The bang scan only checks `isIdentStart` after `!`.
5. `token = m_input.mid(start, m_pos - start)` — the full scanned substring.
6. **If `!hasNonHex`** (pure hex digits, e.g. `DEAD`, `AB`, `140000000`):
   `m_pos = start` (backtrack to the beginning) and `return parseHexNumber(result)`.
   This is the hex/identifier disambiguation: a token made of only `0-9a-fA-F`
   characters is a hex literal, not an identifier. (`:320-324`)
7. **Function-call check** (`:327-329`): `skipSpaces()`; if `peek() == '('` →
   `return parseFunctionCall(token, result)`. (So `vtop(...)`, `cr3(...)`,
   `phys(...)` and any `name(` are routed here.)
8. **Identifier resolution** (`:332-341`):
   - If `!m_callbacks || !m_callbacks->resolveIdentifier`: `result = 0; return
     true`. (No callback → identifier resolves to 0; this is how `validate` lets
     identifiers pass syntax check.)
   - Else: `bool ok = false; result = resolveIdentifier(token, &ok)`; if `!ok` →
     `fail("unknown identifier '<token>'")`; else return true.

Subtleties for parity:
- Disambiguation is purely lexical on the scanned token; whether a callback exists
  is irrelevant to whether something is treated as identifier vs hex.
- `peek() == '('` for function-call detection occurs **after** `skipSpaces()`, so
  `vtop (1, 2)` (space before paren) is still a function call. But the `token`
  itself never contains the space.
- Because `isIdentStart` includes `a-f`/`A-F`, a leading hex-letter word like
  `base` reaches this function (starts with `b`); but a word like `FACE` (all hex)
  also reaches here (starts with `F`), scans, finds `hasNonHex==false`, backtracks
  and parses as hex `0xFACE`. Conversely `FACEs` has the non-hex `s` → identifier.

### 5.2 `parseFunctionCall` (`addressparser.cpp:345-407`)

Called with the already-scanned `name` and `m_pos` sitting on `(` (after the
`skipSpaces` in caller). Steps: `advance()` to skip `(` (`:346`). Then dispatch by
exact name:

- **`vtop`** (`:348-370`): `vtop(pid, va)` → physical address.
  - parse `pid` via `parseBitwiseOr`; `skipSpaces()`; if `peek() != ','` →
    `fail("vtop() requires 2 arguments: vtop(pid, va)")`; `advance()` past `,`.
  - parse `va` via `parseBitwiseOr`; `expect(')')`.
  - If no callbacks or no `vtop`: `result = 0; return true`.
  - Else `ok=false; result = vtop((uint32_t)pid, va, &ok)`; if `!ok` →
    `fail("vtop(0x<pid>, 0x<va>) failed")` (pid/va formatted as hex, no prefix,
    field width 0; `.arg(pid,0,16)`). Note `pid` is **truncated to 32 bits** via
    `(uint32_t)pid` before the callback. The error message uses the *original*
    `pid`/`va` (full 64-bit) hex-formatted.
- **`cr3`** (`:372-387`): `cr3(pid)` → CR3 value.
  - parse `pid` via `parseBitwiseOr`; `expect(')')`.
  - If no callback: `result = 0; return true`.
  - Else `result = cr3((uint32_t)pid, &ok)`; if `!ok` → `fail("cr3(<pid>) failed")`
    where `<pid>` is `.arg(pid)` = **decimal** formatting (no radix arg). (Note:
    vtop/phys error use hex; cr3 error uses decimal pid.)
- **`phys`** (`:389-404`): `phys(addr)` → reads 8 bytes from a physical address.
  - parse `addr` via `parseBitwiseOr`; `expect(')')`.
  - If no callback: `result = 0; return true`.
  - Else `result = physRead(addr, &ok)`; if `!ok` → `fail("phys(0x<addr>) failed")`
    (addr hex-formatted, `.arg(addr,0,16)`).
- **Unknown name** (`:406`): `fail("unknown function '<name>'")`.

Argument parsing uses `parseBitwiseOr` (the top of the grammar), so full
expressions are allowed as args. `expect(')')` does a `skipSpaces` first, so
trailing space before `)` is fine.

Important: function-call detection in `parseIdentifierOrHex` only triggers when the
scanned token `hasNonHex == true` (since pure-hex tokens backtrack to hex before
reaching the `(` check). So `vtop(` works (`v` is non-hex). A purely-hex name like
`abc(` would be parsed as hex `0xabc` then the trailing `(` becomes "unexpected".

### 5.3 `parseDereference` (`addressparser.cpp:410-430`)

`'[' bitwiseOr ']'` — read pointer at the computed address.
1. `advance()` (skip `[`).
2. parse address via `parseBitwiseOr`; `expect(']')` (else `fail("expected ']'")`).
3. If no callbacks or no `readPointer`: `result = 0; return true` (syntax mode).
4. Else `ok=false; result = readPointer(address, &ok)`; if `!ok` →
   `fail("failed to read memory at 0x<address>")` (address hex-formatted).
5. Nestable: inner re-enters `parseBitwiseOr`, so `[ [x] + y ]` works (test
   `derefNested`).

**Pointer width:** the parser does not read memory itself — it calls
`readPointer`. The *caller* wires `readPointer` to read exactly `ptrSize` bytes
(see `controller.cpp:1682-1686`: `prov->read(addr, &val, ptrSz)` where `ptrSz =
tree.pointerSize`). So a 32-bit document reads 4-byte pointers, a 64-bit reads 8.
The Rust port must preserve this: `ptrSize` flows from the document to the
`readPointer` closure, not into the parser.

### 5.4 `parseModuleName` (`addressparser.cpp:433-459`)

`'<' name '>'` — resolve module base.
1. `advance()` (skip `<`).
2. `nameStart = m_pos`; scan while `!atEnd() && peek() != '>'` advance. (The name
   may contain *anything* except `>` — spaces, dots, etc.)
3. If `atEnd()` (no closing `>`): `fail("expected '>'")`.
4. `name = m_input.mid(nameStart, m_pos - nameStart).trimmed()` — **trimmed** of
   leading/trailing whitespace. `advance()` (skip `>`).
5. If `name.isEmpty()` (i.e. `<>` or `<   >`): `fail("empty module name")`.
6. If no callbacks or no `resolveModule`: `result = 0; return true` (syntax mode).
7. Else `result = resolveModule(name, &ok)`; if `!ok` →
   `fail("module '<name>' not found")`; else true.

Note the `name` is `trimmed()` — `< Program.exe >` resolves `Program.exe`. But the
content is taken verbatim otherwise (case preserved). The trim uses `QString::trimmed`
(strips ASCII + Unicode whitespace on both ends). Rust `str::trim()`.

### 5.5 `parseGrouping` (`addressparser.cpp:462-467`)

`'(' bitwiseOr ')'`: `advance()` (skip `(`); `parseBitwiseOr(result)`; then
`return expect(')')` (`fail("expected ')'")` on mismatch). Grouping re-enters at
the top of the grammar.

### 5.6 `parseHexNumber` (`addressparser.cpp:470-501`)

All numeric literals are base-16.
1. `skipSpaces()`; if `atEnd()` → `fail("unexpected end of expression")`.
2. `start = m_pos`.
3. **Optional `0x`/`0X` prefix** (`:478-481`): if `m_pos+1 < size && m_input[m_pos]
   == '0' && (m_input[m_pos+1] == 'x' || 'X')` → `m_pos += 2`. (Requires at least
   one more char after `0`; a bare `0` does not consume a prefix.)
4. `digitsStart = m_pos`; consume hex digits: while `!atEnd() && isHexDigit(peek())`
   advance.
5. **If `m_pos == digitsStart`** (no digits): `m_errorPos = start` (point at the
   start, before the prefix); `fail("expected hex number")`. Covers `0x` with no
   digits, or a stray non-hex char reaching here.
6. `digits = m_input.mid(digitsStart, m_pos - digitsStart)`;
   `result = digits.toULongLong(&ok, 16)`.
7. If `!ok`: `m_errorPos = start`; `fail("invalid hex number")`. This triggers on
   **overflow** — `QString::toULongLong` fails (`ok=false`) if the value exceeds
   `0xFFFFFFFFFFFFFFFF` (more than 16 significant hex digits). For example
   `"10000000000000000"` (17 digits) → `invalid hex number`.
   - Rust equivalent: `u64::from_str_radix(digits, 16)` returns `Err` on overflow
     → map to `fail("invalid hex number")` with `error_pos = start`. Leading zeros
     are fine (`00000FF` → `0xFF`); a value with ≤16 sig. hex digits never
     overflows u64.
8. Else success (`result` holds the value).

Notes:
- `bare 0` (test `zeroLiteral`): no prefix consumed (only one char), digits scan
  consumes `0` → `result = 0`.
- Case-insensitive digits (`AB` == `ab` == `0xAb`).
- `parseHexNumber` is reached either directly from `parseAtom` (when first char is
  not ident-start, e.g. `0...`) or via backtrack from `parseIdentifierOrHex` (pure
  hex word starting with a-f/A-F). Both paths handle the `0x` prefix because the
  prefix check is inside `parseHexNumber`. (When backtracked from
  identifier-scan, `m_pos == start` again, so `0x...` is re-examined and the prefix
  is consumed correctly.)

---

## 6. Error-position (`errorPos`) semantics — precise

`m_errorPos` is set:
- by `fail()` to the **current `m_pos`** (`addressparser.cpp:84`) — this is the
  default for almost every error.
- explicitly overridden to `start` in `parseHexNumber` for the two hex errors
  (`:489, :498`).

It is reported as `result.errorPos` in `error()` (`:79`). Edge cases:
- On the **trailing-garbage** path in `parse()` (`:53`), `error("unexpected ...")`
  is built but `m_errorPos` is **not** updated to `m_pos` first — so `errorPos`
  reflects the *last `fail`* (or initial 0), not the garbage position. No test
  inspects `errorPos` here, but a faithful port should replicate (don't set pos on
  this path). (Only `error.contains("unexpected")` is tested — see
  `trailingGarbage`.)
- On the **empty-expression** path (`:45`), `errorPos` is the initial `0`.
- The position is into the **cleaned** string (after backtick/quote removal), not
  the original — so positions shift if the user typed backticks/quotes. (Tests do
  not assert exact positions; only `error.contains` substrings and `ok`.)

For the Rust port: keep an `error_pos` field updated by `fail()` to the current
position, overridden to `start` in the two hex-number errors, and **not** updated
on the trailing-garbage/empty paths. Initialize to `0`. Report `-1` on success.

---

## 7. Exact error messages (tests depend on substrings)

Tests use `QString::contains(substr)`. The full messages and the substrings the
tests check (`test_addressparser.cpp`):

| Where | Full message | Tested substring |
|-------|--------------|------------------|
| `parse` empty | `"empty expression"` | — (`emptyInput` only checks `!ok`) |
| `parse` trailing | `"unexpected '<ch>'"` | `"unexpected"` (`trailingGarbage`) |
| `parseTerm` div0 | `"division by zero"` | `"division by zero"` (`divisionByZero`) |
| `parseDereference` `expect(']')` | `"expected ']'"` | `"']'"` (`unmatchedBracket`) |
| `parseModuleName` no `>` | `"expected '>'"` | `"'>'"` (`unmatchedAngle`) |
| `parseModuleName` not found | `"module '<name>' not found"` | `"not found"` (`moduleNotFound`) |
| `parseDereference` read fail | `"failed to read memory at 0x<addr>"` | `"failed to read"` (`derefReadFailure`) |
| `parseIdentifierOrHex` unknown | `"unknown identifier '<token>'"` | `"unknown identifier"` (`identUnknown`) |
| `validate` empty | `"empty"` (from validate, not parser) | `!isEmpty()` (`validateInvalid`) |

The Rust port **must** reproduce these exact message strings (at least these
substrings) so any UI/tests relying on `contains(...)` keep working. Use the same
phrasing; safest is to copy the literals verbatim. The `"unexpected '%1'"` and
`"expected '%1'"` use the single offending character; hex `%1` formats with
`.arg(value, 0, 16)` = lowercase hex no prefix, field width 0 (so `0x1400000de`
etc. — note **lowercase**).

Hex formatting detail: `QString::arg(uint64, fieldWidth=0, base=16)` produces
**lowercase** hex with no `0x` and no padding. The error strings already prepend
`0x` literally (e.g. `"failed to read memory at 0x%1"`), so the rendered value is
`0x` + lowercase-hex. In Rust use `format!("0x{:x}", addr)`. The `cr3` error uses
decimal: `format!("cr3({}) failed", pid)`.

---

## 8. Qt usage → Rust equivalents

| Qt construct | Used for | Rust equivalent |
|--------------|----------|-----------------|
| `QString` | input, error msgs, module/ident names | `String` / `&str`; for char indexing collect to `Vec<char>` (or use `chars()`). |
| `QChar` | per-char scanning, `peek()` | `char`. NUL sentinel `'\0'` for end. |
| `QString::size()` | length | `Vec<char>::len()` (or `str::chars().count()`). |
| `QString::operator[]` | indexed access | `Vec<char>[i]`. |
| `QString::mid(pos, n)` | substring extraction | slice of the char buffer → `String`. |
| `QString::trimmed()` | trim module name & validate input | `str::trim()`. |
| `QString::isEmpty()` | empty checks | `str::is_empty()`. |
| `QString::remove(QChar)` | strip `` ` `` and `'` in evaluate/validate | `s.replace('`', "").replace('\'', "")` or `retain`. Removes **all** occurrences. |
| `QChar::isSpace()` | whitespace skip | `char::is_whitespace()` (ASCII subset is what matters). |
| `QString::toULongLong(&ok, 16)` | parse hex → u64; fails on overflow/invalid | `u64::from_str_radix(s, 16)` → `Ok`/`Err`. Err on overflow ⇒ "invalid hex number". |
| `QString::arg(...)` | error-message formatting | `format!`. Hex via `{:x}` (lowercase), decimal via `{}`. |
| `QStringLiteral(...)` | literal QStrings | string literals. |
| `std::function<...>` | callbacks | `Box<dyn Fn(...) -> Option<u64>>` (or trait objects). |
| `Q_UNUSED(ptrSize)` | mark unused param | `let _ = ptr_size;` (or `_ptr_size`). |
| `uint64_t` / `uint32_t` / `int64_t` | arithmetic | `u64` / `u32` / `i64`. |

No Qt signals/slots, no `QObject` in the parser itself (the test file uses
`QObject`/`QTest` only as a harness). The parser is plain logic.

---

## 9. Platform-specific code

**None in the parser.** It is fully portable. The *kernel paging* built-ins
(`vtop`/`cr3`/`phys`) are platform-neutral in the parser; they merely call
callbacks. Those callbacks are wired (in `controller.cpp`/`main.cpp`) only when a
provider reports `hasKernelPaging()` (Windows kernel-driver provider). The parser
treats absent callbacks uniformly (return 0 in syntax mode). So in the Rust port,
the parser file has zero `#[cfg(...)]`; the *callback wiring* in the controller is
where Windows-specific provider logic lives.

---

## 10. Concurrency / threading

None. `evaluate`/`validate` are static, stateless across calls, and operate on a
fresh `ExpressionParser` per call. No globals, no shared mutable state, no locks.
The callbacks may touch external resources (memory/symbols) but that is the
caller's concern. Fully reentrant. In Rust, free functions; callbacks via
`&AddressParserCallbacks` borrow.

---

## 11. Subtle behaviors the tests (and real callers) rely on

1. **All literals are hex** even without `0x` (`bareHex` "AB"→0xAB;
   `simpleHexAddress` "140000000"→0x140000000; `large64bit`). There is **no
   decimal** parsing anywhere. `4` in `0x10 * 4` is hex 4 (== decimal 4, so tests
   that use small numbers happen to coincide, but `10` means 0x10).
2. **Hex/identifier disambiguation by content**: a word with only hex chars is a
   hex literal (`DEAD`→0xDEAD, `hexDisambigDEAD`); any non-hex char makes it an
   identifier (`base`, `ABC_field`, `hexDisambigBase`/`hexDisambigABCwithUnderscore`).
3. **Module-with-extension as bare identifier**: `client.dll`, `cs2.exe` parse as a
   single identifier token resolved via `resolveIdentifier` (NOT `resolveModule`;
   `resolveModule` is only for `<...>` syntax). Tests `bareModuleDll`,
   `bareModuleExe`. Only one `.ext` segment is consumed.
4. **`<...>` module syntax** uses `resolveModule`; name is trimmed; `<>` is "empty
   module name". Test `moduleResolve` (`<Program.exe> + 0x123` → base+offset),
   `moduleNotFound`.
5. **Dereference uses caller-configured pointer width** via `readPointer`; nestable
   (`derefNested`); read failure → "failed to read". The complex test `complexExpr`
   (`[<Program.exe> + 0xDE] - AB`) exercises module+deref+subtraction together.
6. **C operator precedence exactly**: `precedence` (`0x10 + 2*3` = 0x16),
   `parentheses`, `shiftPrecedence` (`1 + 2 << 3` = `(1+2)<<3` = 0x18 — shift looser
   than +), `andOrPrecedence` (`&` tighter than `|`), `xorPrecedence` (`^` between
   `&` and `|`), `pageAlignedExpr` (`(base+e_lfanew) & ~0xFFF`).
7. **Unary `-` is two's-complement wrap**, `~` is bitwise NOT over 64 bits
   (`unaryMinus`, `unaryNot`→all-ones, `unaryNotMask` `~0xFFF`→0xFFFFFFFFFFFFF000).
8. **Wrapping arithmetic throughout** — no overflow errors (except hex-literal
   overflow → "invalid hex number"). Subtraction can wrap below 0
   (`-0x10` path). Use Rust `wrapping_*`.
9. **Division by zero** is the only arithmetic error: `divisionByZero`.
10. **Backtick & single-quote stripping** before parsing, in both `evaluate` and
    `validate`: `backtickStripping` (`7ff6\`6cce0000` → 0x7FF66CCE0000). All
    occurrences removed.
11. **Whitespace tolerance** everywhere via `skipSpaces` (`whitespace` test:
    `"  0x100  +  0x200  "`). `evaluate` does not trim but `skipSpaces` covers
    leading/trailing.
12. **`validate` is syntax-only with no callbacks**: module/deref/identifier all
    resolve to 0 and succeed; returns empty string ⇔ valid (`validateValid`,
    `validateIdentifier`, `validateBitwiseOps`). Invalid → non-empty
    (`validateInvalid`: empty input, unclosed `[`, trailing garbage). Empty input
    to validate returns the literal `"empty"`.
13. **No callbacks at all** (`cb == nullptr`, the default for bare-hex tests like
    `bareHex`, `addition`, etc.): arithmetic works; any `<...>`/`[...]`/identifier
    would resolve to 0 (but those tests don't use them without callbacks).
14. **Trailing operator** → error (`trailingOperator` `"0x100 +"`): after
    consuming `+`, `parseTerm`→`parseUnary`→`parseAtom` hits end →
    "unexpected end of expression" → `!ok`.
15. **Identifier with no `resolveIdentifier` callback resolves to 0** (not an
    error). This is exploited by `validate`. With a callback that sets `*ok=false`,
    it's "unknown identifier" (`identUnknown`).
16. **Function calls** only matter with kernel callbacks wired; without them
    `vtop/cr3/phys(...)` parse and return 0 (syntax-valid). Unknown function names
    error. No dedicated test in `test_addressparser.cpp` for the functions, but the
    behavior is wired in controller/main; preserve it for parity.

---

## 12. Implementation checklist for the Rust port (parity-critical)

- [ ] Two public functions: `evaluate(formula, ptr_size=8, cb)` and
      `validate(formula)`; `ptr_size` is accepted but unused by the parser.
- [ ] Strip all `` ` `` and `'` before parsing in both; `validate` additionally
      `trim()`s and returns `"empty"` for empty cleaned input.
- [ ] Recursive-descent with the exact precedence ladder of §4, all left-assoc,
      unary right-assoc, no unary `+`/`!`.
- [ ] `<<`/`>>` only when doubled; lone `<` starts module syntax; lone `>` ends
      `shift` loop without consuming.
- [ ] All arithmetic wrapping (`wrapping_*`), shifts mask to `% 64`, division by
      zero → `"division by zero"`.
- [ ] Hex literals base-16, optional `0x`/`0X` prefix (only if a char follows
      `0`), overflow → `"invalid hex number"` with `error_pos = start`; no digits →
      `"expected hex number"` with `error_pos = start`.
- [ ] Identifier vs hex disambiguation: scan ident token; pure-hex ⇒ backtrack to
      hex; non-hex char (incl. `.ext`, `!symbol`) ⇒ identifier. One `.ext` segment;
      `!symbol` extends only if followed by ident-start.
- [ ] `name(` ⇒ function call; dispatch `vtop`/`cr3`/`phys`; else
      `"unknown function '<name>'"`. vtop needs `,`-separated 2 args; pid truncated
      to u32 before callback; error strings match §7 (vtop/phys hex, cr3 decimal).
- [ ] `[...]` ⇒ readPointer (caller reads ptr_size bytes); nestable; fail
      `"failed to read memory at 0x<addr>"`.
- [ ] `<...>` ⇒ resolveModule, trimmed name, empty ⇒ `"empty module name"`, no
      close ⇒ `"expected '>'"`, not found ⇒ `"module '<name>' not found"`.
- [ ] `(...)` ⇒ grouping, `expect(')')` ⇒ `"expected ')'"`.
- [ ] Absent callbacks ⇒ the corresponding construct returns 0 and succeeds.
- [ ] `parse()`: empty ⇒ `"empty expression"`; success ⇒ value with `error_pos =
      -1`; trailing non-space ⇒ `"unexpected '<ch>'"`.
- [ ] Error messages copied verbatim (substring-tested by callers).
- [ ] Result shape: `{ok, value(0 on err), error, error_pos(-1 on ok)}`.

---

## 13. Caller integration (for context; not in this file's scope)

The parser is consumed by (all in `reclass-cpp/src`):
- `controller.cpp:1531-1549`, `1673-1711`, `6133-6169`, `6315-6351`, `6916-6934`
  — go-to-address, base-address formula evaluation, static fields, bookmarks.
  These wire `resolveModule = prov->symbolToAddress`, `readPointer = prov->read(addr,
  &val, ptrSz)` (ptrSz = `tree.pointerSize`), `resolveIdentifier =
  SymbolStore::resolve`, and (if `prov->hasKernelPaging()`) `vtop/cr3/physRead`
  to the kernel provider's `translateAddress`/`getCr3`/`readPageTable`.
- `compose.cpp:974-1030` — static field offset expressions
  (`sf.offsetExpr`), with `resolveIdentifier` over sibling fields.
- `scannerpanel.cpp:1789-1809` — scanner address column.
- `main.cpp:4495-4526` — another base-address evaluation path with kernel wiring.
- `mcp/mcp_bridge.cpp:568` — documents that bookmark `addressFormula` is an
  AddressParser expression.

The Rust port should expose the same callback-injection design so these call sites
can wire provider/symbol/kernel access without the parser knowing about them.
