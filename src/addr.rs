//! Address-expression evaluator (`<mod>+0x10`, deref chains, symbols).
//!
//! Port of `src/addressparser.{h,cpp}`. A single-pass recursive-descent parser
//! over a "cleaned" input string that turns a human-typed address formula into a
//! concrete 64-bit value, with C operator precedence and pluggable callbacks for
//! module/symbol resolution, pointer dereference, and kernel paging.
//!
//! The parser is pure (no UI, no I/O of its own — all environment access is
//! through caller-supplied callbacks), so it compiles under
//! `--no-default-features`.
//!
//! All numeric literals are hexadecimal (base 16). Arithmetic uses C wrapping
//! semantics throughout (Rust `wrapping_*`); the only arithmetic error is
//! division by zero. Hex-literal overflow is the only literal error.

/// `struct AddressParseResult` (`addressparser.h:8-13`).
///
/// On success: `{ ok: true, value, error: "", error_pos: -1 }`
/// (`addressparser.cpp:55`). On error: `{ ok: false, value: 0, error: msg,
/// error_pos: m_errorPos }` (`addressparser.cpp:78-80`).
#[derive(Clone, Debug, Default)]
pub struct AddressParseResult {
    pub ok: bool,
    pub value: u64,
    pub error: String,
    pub error_pos: i32,
}

/// `struct AddressParserCallbacks` (`addressparser.h:15-24`) — pluggable hooks
/// the evaluator calls for module/symbol resolution and pointer deref.
///
/// Each closure returns `(value, ok)`, modelling the C++ `bool* ok` out-param:
/// `ok == true` means the C++ callback set `*ok = true` (success). When a
/// callback is `None` (unset), the corresponding construct resolves to `0` and
/// the parse succeeds — this is the "syntax-check" mode used by [`validate`].
///
/// Kernel-paging hooks (`vtop`/`cr3`/`phys_read`) are optional; in the C++ they
/// are wired only when a kernel-capable provider is active.
#[derive(Default)]
pub struct AddressParserCallbacks<'a> {
    /// `resolveModule(name) -> (base, ok)` — resolve `<name>` to a module base.
    pub resolve_module: Option<Box<dyn Fn(&str) -> (u64, bool) + 'a>>,
    /// `readPointer(addr) -> (value, ok)` — read a pointer-sized value. The
    /// caller wires this to read exactly `ptr_size` bytes (the parser itself
    /// does not use `ptr_size`).
    pub read_pointer: Option<Box<dyn Fn(u64) -> (u64, bool) + 'a>>,
    /// `resolveIdentifier(name) -> (value, ok)` — resolve a C/C++ identifier or
    /// bare module name (e.g. `client.dll`) to a value.
    pub resolve_identifier: Option<Box<dyn Fn(&str) -> (u64, bool) + 'a>>,
    /// `vtop(pid, va) -> (phys, ok)` — kernel virtual→physical translate.
    pub vtop: Option<Box<dyn Fn(u32, u64) -> (u64, bool) + 'a>>,
    /// `cr3(pid) -> (cr3, ok)` — kernel CR3 read.
    pub cr3: Option<Box<dyn Fn(u32) -> (u64, bool) + 'a>>,
    /// `physRead(physAddr) -> (value, ok)` — kernel physical-address read.
    pub phys_read: Option<Box<dyn Fn(u64) -> (u64, bool) + 'a>>,
}

/// `class AddressParser` (`addressparser.h:26-31`). Two stateless static
/// methods; no instances.
pub struct AddressParser;

impl AddressParser {
    /// `AddressParser::evaluate(formula, ptrSize=8, cb)`
    /// (`addressparser.cpp:506-522`).
    ///
    /// Strips all backtick (`` ` ``) and single-quote (`'`) characters from the
    /// input (WinDbg separators / digit grouping), then parses the cleaned
    /// string. `ptr_size` is accepted for signature parity but unused by the
    /// parser itself (`Q_UNUSED(ptrSize)`, `addressparser.cpp:511`); the caller
    /// uses it to configure how `read_pointer` reads memory.
    ///
    /// `cb == None` ⇒ pure syntax mode: all callback-backed constructs resolve
    /// to `0`. Note: `evaluate` does **not** trim the input (the parser handles
    /// surrounding whitespace via `skip_spaces`).
    pub fn evaluate(
        formula: &str,
        ptr_size: i32,
        cb: Option<&AddressParserCallbacks<'_>>,
    ) -> AddressParseResult {
        // Profiler instrumentation (port of the C++ PROFILE_SCOPE peppered over
        // the hot paths): address evaluation runs on every editor render that
        // resolves a node's address and on every Goto keystroke, so it's a
        // meaningful bucket. No-op when profiling is disabled.
        crate::PROFILE_SCOPE!("AddressParser::evaluate");

        // ptrSize is used by the caller to configure the readPointer callback;
        // the parser itself doesn't need it directly. (addressparser.cpp:511)
        let _ = ptr_size;

        // Strip WinDbg backtick separators and any single-quote digit grouping
        // so users can paste addresses directly. (addressparser.cpp:516-518)
        let cleaned = clean(formula);

        let mut parser = ExpressionParser::new(&cleaned, cb);
        parser.parse()
    }

    /// `AddressParser::validate(formula)` (`addressparser.cpp:524-538`).
    ///
    /// Syntax-only check with **no** callbacks: module/dereference/identifier/
    /// function constructs all succeed and produce `0`. Returns the empty
    /// string when valid, else the parser's error message. Empty (after
    /// cleaning + trimming) input returns the literal `"empty"` — distinct from
    /// the parser's own `"empty expression"`.
    pub fn validate(formula: &str) -> String {
        let cleaned = clean(formula);
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            return "empty".to_string();
        }

        // Parse with no callbacks — modules, dereferences, identifiers succeed
        // but return 0. This checks syntax only. (addressparser.cpp:535-537)
        let mut parser = ExpressionParser::new(cleaned, None);
        let result = parser.parse();
        if result.ok {
            String::new()
        } else {
            result.error
        }
    }
}

/// Remove all backtick and single-quote characters, as
/// `cleaned.remove('`'); cleaned.remove('\'')` (`addressparser.cpp:517-518`).
fn clean(formula: &str) -> String {
    formula.chars().filter(|&c| c != '`' && c != '\'').collect()
}

// ── Internal ExpressionParser (the actual engine) ──────────────────────────
//
// Defined entirely in addressparser.cpp:37-502. A single-pass recursive-descent
// parser over the cleaned string.
//
// Grammar (C operator precedence), lowest → highest binding:
//   bitwiseOr  = bitwiseXor ('|' bitwiseXor)*
//   bitwiseXor = bitwiseAnd ('^' bitwiseAnd)*
//   bitwiseAnd = shift ('&' shift)*
//   shift      = expr (('<<' | '>>') expr)*
//   expr       = term (('+' | '-') term)*
//   term       = unary (('*' | '/') unary)*
//   unary      = '-' unary | '~' unary | atom
//   atom       = '[' bitwiseOr ']'   (dereference)
//              | '<' moduleName '>'  (module base)
//              | '(' bitwiseOr ')'   (grouping)
//              | identifier          (callback resolution / function call)
//              | hexLiteral          (hex number, optional 0x prefix)

/// NUL sentinel returned by `peek()` at end of input (mirrors the C++
/// `QChar('\0')`); never appears in cleaned ASCII input.
const NUL: char = '\0';

struct ExpressionParser<'a> {
    /// The cleaned input as a char buffer. Positions are char indices; for all
    /// ASCII inputs (everything the tests and real usage produce) these match
    /// the C++ UTF-16 code-unit indices. (`m_input`, `addressparser.cpp:59`)
    input: Vec<char>,
    /// Callback bag (may be `None`). (`m_callbacks`, `addressparser.cpp:60`)
    callbacks: Option<&'a AddressParserCallbacks<'a>>,
    /// Current scan position. (`m_pos`, `addressparser.cpp:61`)
    pos: usize,
    /// Last error message set by `fail()`. (`m_error`, `addressparser.cpp:62`)
    error: String,
    /// Position recorded at the last `fail()` (or overridden in hex parsing).
    /// (`m_errorPos`, `addressparser.cpp:63`)
    error_pos: i32,
}

impl<'a> ExpressionParser<'a> {
    /// `ExpressionParser(input, callbacks)` (`addressparser.cpp:39-40`).
    fn new(input: &str, callbacks: Option<&'a AddressParserCallbacks<'a>>) -> Self {
        ExpressionParser {
            input: input.chars().collect(),
            callbacks,
            pos: 0,
            error: String::new(),
            error_pos: 0,
        }
    }

    /// `parse()` — top-level entry (`addressparser.cpp:42-56`).
    fn parse(&mut self) -> AddressParseResult {
        self.skip_spaces();
        if self.at_end() {
            return self.error_result("empty expression".to_string());
        }

        let mut value: u64 = 0;
        if !self.parse_bitwise_or(&mut value) {
            let msg = self.error.clone();
            return self.error_result(msg);
        }

        self.skip_spaces();
        if !self.at_end() {
            // NOTE: errorPos is NOT updated to m_pos on this trailing-garbage
            // path — it reflects the last fail() (or the initial 0).
            // (addressparser.cpp:52-53)
            let msg = format!("unexpected '{}'", self.input[self.pos]);
            return self.error_result(msg);
        }

        AddressParseResult {
            ok: true,
            value,
            error: String::new(),
            error_pos: -1,
        }
    }

    // ── Helpers ──

    /// `atEnd()` (`addressparser.cpp:67`).
    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// `peek()` (`addressparser.cpp:69`) — null-safe; returns NUL at end.
    fn peek(&self) -> char {
        if self.at_end() {
            NUL
        } else {
            self.input[self.pos]
        }
    }

    /// `advance()` (`addressparser.cpp:71`) — no bounds check (callers only
    /// advance after a successful peek/length check).
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// `skipSpaces()` (`addressparser.cpp:73-76`). Qt's `QChar::isSpace()` is
    /// Unicode-aware; `char::is_whitespace()` matches it for the ASCII whitespace
    /// that is all that matters in practice.
    fn skip_spaces(&mut self) {
        while !self.at_end() && self.input[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    /// `error(msg)` (`addressparser.cpp:78-80`) — build an error result using
    /// the current `error_pos`.
    fn error_result(&self, msg: String) -> AddressParseResult {
        AddressParseResult {
            ok: false,
            value: 0,
            error: msg,
            error_pos: self.error_pos,
        }
    }

    /// `fail(msg)` (`addressparser.cpp:82-86`) — record message + position,
    /// return false.
    fn fail(&mut self, msg: impl Into<String>) -> bool {
        self.error = msg.into();
        self.error_pos = self.pos as i32;
        false
    }

    /// `expect(ch)` (`addressparser.cpp:88-94`).
    fn expect(&mut self, ch: char) -> bool {
        self.skip_spaces();
        if self.peek() != ch {
            return self.fail(format!("expected '{}'", ch));
        }
        self.advance();
        true
    }

    /// `isHexDigit(ch)` (`addressparser.cpp:96-100`) — ASCII only.
    fn is_hex_digit(ch: char) -> bool {
        ch.is_ascii_digit() || ('a'..='f').contains(&ch) || ('A'..='F').contains(&ch)
    }

    /// `isIdentStart(ch)` (`addressparser.cpp:102-104`).
    fn is_ident_start(ch: char) -> bool {
        ch.is_ascii_lowercase() || ch.is_ascii_uppercase() || ch == '_'
    }

    /// `isIdentChar(ch)` (`addressparser.cpp:106-108`).
    fn is_ident_char(ch: char) -> bool {
        Self::is_ident_start(ch) || ch.is_ascii_digit()
    }

    /// Extract `input[start..end]` as a `String` (mirrors `QString::mid`).
    fn slice(&self, start: usize, end: usize) -> String {
        self.input[start..end].iter().collect()
    }

    // ── Recursive descent parsing ──

    /// `bitwiseOr = bitwiseXor ('|' bitwiseXor)*` (`addressparser.cpp:113-127`).
    fn parse_bitwise_or(&mut self, result: &mut u64) -> bool {
        if !self.parse_bitwise_xor(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            if self.peek() != '|' {
                break;
            }
            self.advance();
            let mut rhs: u64 = 0;
            if !self.parse_bitwise_xor(&mut rhs) {
                return false;
            }
            *result |= rhs;
        }
        true
    }

    /// `bitwiseXor = bitwiseAnd ('^' bitwiseAnd)*` (`addressparser.cpp:130-144`).
    fn parse_bitwise_xor(&mut self, result: &mut u64) -> bool {
        if !self.parse_bitwise_and(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            if self.peek() != '^' {
                break;
            }
            self.advance();
            let mut rhs: u64 = 0;
            if !self.parse_bitwise_and(&mut rhs) {
                return false;
            }
            *result ^= rhs;
        }
        true
    }

    /// `bitwiseAnd = shift ('&' shift)*` (`addressparser.cpp:147-161`).
    fn parse_bitwise_and(&mut self, result: &mut u64) -> bool {
        if !self.parse_shift(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            if self.peek() != '&' {
                break;
            }
            self.advance();
            let mut rhs: u64 = 0;
            if !self.parse_shift(&mut rhs) {
                return false;
            }
            *result &= rhs;
        }
        true
    }

    /// `shift = expr (('<<' | '>>') expr)*` (`addressparser.cpp:164-183`).
    fn parse_shift(&mut self, result: &mut u64) -> bool {
        if !self.parse_expression(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            let c = self.peek();
            if c != '<' && c != '>' {
                break;
            }
            // Must be << or >> (not < or > alone). Lone '<' starts module
            // syntax; lone '>' just ends the loop. (addressparser.cpp:172-174)
            if self.pos + 1 >= self.input.len() || self.input[self.pos + 1] != c {
                break;
            }
            let is_left = c == '<';
            self.advance();
            self.advance(); // skip << or >>
            let mut rhs: u64 = 0;
            if !self.parse_expression(&mut rhs) {
                return false;
            }
            // C++ shift by >= 64 is UB but x86 codegen masks to 6 bits; Rust
            // wrapping_sh{l,r} mask the count to `rhs % 64`, matching it.
            *result = if is_left {
                result.wrapping_shl(rhs as u32)
            } else {
                result.wrapping_shr(rhs as u32)
            };
        }
        true
    }

    /// `expr = term (('+' | '-') term)*` (`addressparser.cpp:186-204`).
    fn parse_expression(&mut self, result: &mut u64) -> bool {
        if !self.parse_term(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            let op = self.peek();
            if op != '+' && op != '-' {
                break;
            }
            self.advance();
            let mut rhs: u64 = 0;
            if !self.parse_term(&mut rhs) {
                return false;
            }
            *result = if op == '+' {
                result.wrapping_add(rhs)
            } else {
                result.wrapping_sub(rhs)
            };
        }
        true
    }

    /// `term = unary (('*' | '/') unary)*` (`addressparser.cpp:207-231`).
    fn parse_term(&mut self, result: &mut u64) -> bool {
        if !self.parse_unary(result) {
            return false;
        }
        loop {
            self.skip_spaces();
            let op = self.peek();
            if op != '*' && op != '/' {
                break;
            }
            self.advance();
            let mut rhs: u64 = 0;
            if !self.parse_unary(&mut rhs) {
                return false;
            }
            if op == '*' {
                *result = result.wrapping_mul(rhs);
            } else {
                if rhs == 0 {
                    return self.fail("division by zero");
                }
                *result = result.wrapping_div(rhs);
            }
        }
        true
    }

    /// `unary = '-' unary | '~' unary | atom` (`addressparser.cpp:234-253`).
    fn parse_unary(&mut self, result: &mut u64) -> bool {
        self.skip_spaces();
        if self.peek() == '-' {
            self.advance();
            let mut inner: u64 = 0;
            if !self.parse_unary(&mut inner) {
                return false;
            }
            // static_cast<uint64_t>(-static_cast<int64_t>(inner)) — two's
            // complement negation. (addressparser.cpp:241)
            *result = inner.wrapping_neg();
            return true;
        }
        if self.peek() == '~' {
            self.advance();
            let mut inner: u64 = 0;
            if !self.parse_unary(&mut inner) {
                return false;
            }
            *result = !inner;
            return true;
        }
        self.parse_atom(result)
    }

    /// `atom` dispatch (`addressparser.cpp:256-272`). Identifiers are checked
    /// before hex because `a-f`/`A-F` are both hex digits and ident-start chars.
    fn parse_atom(&mut self, result: &mut u64) -> bool {
        self.skip_spaces();
        if self.at_end() {
            return self.fail("unexpected end of expression");
        }
        let ch = self.peek();
        if ch == '[' {
            return self.parse_dereference(result);
        }
        if ch == '<' {
            return self.parse_module_name(result);
        }
        if ch == '(' {
            return self.parse_grouping(result);
        }
        if Self::is_ident_start(ch) {
            return self.parse_identifier_or_hex(result);
        }
        self.parse_hex_number(result)
    }

    /// `parseIdentifierOrHex` (`addressparser.cpp:279-342`). Disambiguates
    /// identifiers from hex literals, handles `module.ext` and WinDbg
    /// `module!symbol` token extension, and routes function calls.
    fn parse_identifier_or_hex(&mut self, result: &mut u64) -> bool {
        let start = self.pos;
        let mut has_non_hex = false;

        // Scan the full token. (addressparser.cpp:284-288)
        while !self.at_end() && Self::is_ident_char(self.peek()) {
            if !Self::is_hex_digit(self.peek()) {
                has_non_hex = true;
            }
            self.advance();
        }

        // Handle module.dll / module.exe / module.sys extensions.
        // (addressparser.cpp:291-302)
        if !self.at_end() && self.peek() == '.' && self.pos > start {
            let dot_pos = self.pos;
            self.advance(); // skip '.'
            let ext_start = self.pos;
            while !self.at_end() && Self::is_ident_char(self.peek()) {
                self.advance();
            }
            if self.pos > ext_start {
                has_non_hex = true; // '.' makes it definitively an identifier
            } else {
                self.pos = dot_pos; // backtrack — '.' at end isn't an extension
            }
        }

        // WinDbg module!symbol: extend the token across '!' if followed by an
        // identifier start. (addressparser.cpp:305-316)
        if !self.at_end() && self.peek() == '!' && self.pos > start {
            let bang_pos = self.pos;
            self.advance(); // skip '!'
            if !self.at_end() && Self::is_ident_start(self.peek()) {
                has_non_hex = true;
                while !self.at_end() && Self::is_ident_char(self.peek()) {
                    self.advance();
                }
            } else {
                self.pos = bang_pos; // backtrack — '!' at end isn't module!symbol
            }
        }

        let token = self.slice(start, self.pos);

        if !has_non_hex {
            // Pure hex digits (e.g. "DEAD") — backtrack, parse as hex.
            // (addressparser.cpp:320-324)
            self.pos = start;
            return self.parse_hex_number(result);
        }

        // Function-call syntax: identifier '(' args ')'. (addressparser.cpp:327-329)
        self.skip_spaces();
        if self.peek() == '(' {
            return self.parse_function_call(&token, result);
        }

        // Identifier — resolve via callback. (addressparser.cpp:332-341)
        match self.callbacks.and_then(|c| c.resolve_identifier.as_ref()) {
            None => {
                *result = 0;
                true
            }
            Some(resolve) => {
                let (value, ok) = resolve(&token);
                if !ok {
                    return self.fail(format!("unknown identifier '{}'", token));
                }
                *result = value;
                true
            }
        }
    }

    /// `parseFunctionCall(name)` — built-ins `vtop`/`cr3`/`phys`
    /// (`addressparser.cpp:345-407`). Caller has positioned `pos` on `(`.
    fn parse_function_call(&mut self, name: &str, result: &mut u64) -> bool {
        self.advance(); // skip '('

        if name == "vtop" {
            // vtop(pid, virtualAddress) → physical address.
            let mut pid: u64 = 0;
            if !self.parse_bitwise_or(&mut pid) {
                return false;
            }
            self.skip_spaces();
            if self.peek() != ',' {
                return self.fail("vtop() requires 2 arguments: vtop(pid, va)");
            }
            self.advance(); // skip ','
            let mut va: u64 = 0;
            if !self.parse_bitwise_or(&mut va) {
                return false;
            }
            if !self.expect(')') {
                return false;
            }
            match self.callbacks.and_then(|c| c.vtop.as_ref()) {
                None => {
                    *result = 0;
                    true
                }
                Some(vtop) => {
                    // pid is truncated to 32 bits before the callback; the error
                    // message uses the original full-64-bit pid/va (hex).
                    let (value, ok) = vtop(pid as u32, va);
                    if !ok {
                        return self.fail(format!("vtop(0x{:x}, 0x{:x}) failed", pid, va));
                    }
                    *result = value;
                    true
                }
            }
        } else if name == "cr3" {
            // cr3(pid) → CR3 value.
            let mut pid: u64 = 0;
            if !self.parse_bitwise_or(&mut pid) {
                return false;
            }
            if !self.expect(')') {
                return false;
            }
            match self.callbacks.and_then(|c| c.cr3.as_ref()) {
                None => {
                    *result = 0;
                    true
                }
                Some(cr3) => {
                    let (value, ok) = cr3(pid as u32);
                    if !ok {
                        // cr3 error uses DECIMAL pid (vtop/phys use hex).
                        return self.fail(format!("cr3({}) failed", pid));
                    }
                    *result = value;
                    true
                }
            }
        } else if name == "phys" {
            // phys(addr) → read 8 bytes from physical address.
            let mut addr: u64 = 0;
            if !self.parse_bitwise_or(&mut addr) {
                return false;
            }
            if !self.expect(')') {
                return false;
            }
            match self.callbacks.and_then(|c| c.phys_read.as_ref()) {
                None => {
                    *result = 0;
                    true
                }
                Some(phys) => {
                    let (value, ok) = phys(addr);
                    if !ok {
                        return self.fail(format!("phys(0x{:x}) failed", addr));
                    }
                    *result = value;
                    true
                }
            }
        } else {
            self.fail(format!("unknown function '{}'", name))
        }
    }

    /// `parseDereference` — `'[' bitwiseOr ']'` (`addressparser.cpp:410-430`).
    fn parse_dereference(&mut self, result: &mut u64) -> bool {
        self.advance(); // skip '['
        let mut address: u64 = 0;
        if !self.parse_bitwise_or(&mut address) {
            return false;
        }
        if !self.expect(']') {
            return false;
        }
        // Without a callback, return 0 (syntax-check mode).
        match self.callbacks.and_then(|c| c.read_pointer.as_ref()) {
            None => {
                *result = 0;
                true
            }
            Some(read) => {
                let (value, ok) = read(address);
                if !ok {
                    return self.fail(format!("failed to read memory at 0x{:x}", address));
                }
                *result = value;
                true
            }
        }
    }

    /// `parseModuleName` — `'<' name '>'` (`addressparser.cpp:433-459`). The name
    /// may contain anything except `>`; it is trimmed of surrounding whitespace.
    fn parse_module_name(&mut self, result: &mut u64) -> bool {
        self.advance(); // skip '<'
        let name_start = self.pos;
        while !self.at_end() && self.peek() != '>' {
            self.advance();
        }
        if self.at_end() {
            return self.fail("expected '>'");
        }
        let name = self.slice(name_start, self.pos).trim().to_string();
        self.advance(); // skip '>'

        if name.is_empty() {
            return self.fail("empty module name");
        }
        // Without a callback, return 0 (syntax-check mode).
        match self.callbacks.and_then(|c| c.resolve_module.as_ref()) {
            None => {
                *result = 0;
                true
            }
            Some(resolve) => {
                let (value, ok) = resolve(&name);
                if !ok {
                    return self.fail(format!("module '{}' not found", name));
                }
                *result = value;
                true
            }
        }
    }

    /// `parseGrouping` — `'(' bitwiseOr ')'` (`addressparser.cpp:462-467`).
    fn parse_grouping(&mut self, result: &mut u64) -> bool {
        self.advance(); // skip '('
        if !self.parse_bitwise_or(result) {
            return false;
        }
        self.expect(')')
    }

    /// `parseHexNumber` — base-16 literal with optional `0x`/`0X` prefix
    /// (`addressparser.cpp:470-501`).
    fn parse_hex_number(&mut self, result: &mut u64) -> bool {
        self.skip_spaces();
        if self.at_end() {
            return self.fail("unexpected end of expression");
        }
        let start = self.pos;

        // Optional 0x/0X prefix (only if at least one char follows '0').
        // (addressparser.cpp:478-481)
        if self.pos + 1 < self.input.len()
            && self.input[self.pos] == '0'
            && (self.input[self.pos + 1] == 'x' || self.input[self.pos + 1] == 'X')
        {
            self.pos += 2;
        }

        // Consume hex digits.
        let digits_start = self.pos;
        while !self.at_end() && Self::is_hex_digit(self.peek()) {
            self.advance();
        }

        if self.pos == digits_start {
            // No digits — point the error at the start (before the prefix).
            self.error_pos = start as i32;
            return self.fail("expected hex number");
        }

        let digits = self.slice(digits_start, self.pos);
        match u64::from_str_radix(&digits, 16) {
            Ok(value) => {
                *result = value;
                true
            }
            Err(_) => {
                // Overflow (>16 significant hex digits) — QString::toULongLong
                // sets ok=false. (addressparser.cpp:496-499)
                self.error_pos = start as i32;
                self.fail("invalid hex number")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Faithful translation of `tests/test_addressparser.cpp` (53 cases in the
    //! golden oracle log `_oracle/logs/test_addressparser.txt`).
    use super::*;

    /// Build callbacks with a `resolve_module`.
    fn cb_module<'a>(f: impl Fn(&str) -> (u64, bool) + 'a) -> AddressParserCallbacks<'a> {
        AddressParserCallbacks {
            resolve_module: Some(Box::new(f)),
            ..Default::default()
        }
    }
    fn cb_read<'a>(f: impl Fn(u64) -> (u64, bool) + 'a) -> AddressParserCallbacks<'a> {
        AddressParserCallbacks {
            read_pointer: Some(Box::new(f)),
            ..Default::default()
        }
    }
    fn cb_ident<'a>(f: impl Fn(&str) -> (u64, bool) + 'a) -> AddressParserCallbacks<'a> {
        AddressParserCallbacks {
            resolve_identifier: Some(Box::new(f)),
            ..Default::default()
        }
    }
    fn eval(s: &str) -> AddressParseResult {
        AddressParser::evaluate(s, 8, None)
    }

    // -- Hex literals --

    #[test]
    fn bare_hex() {
        let r = eval("AB");
        assert!(r.ok);
        assert_eq!(r.value, 0xAB);
        assert_eq!(r.error_pos, -1);
    }
    #[test]
    fn prefixed_hex() {
        let r = eval("0x1F4");
        assert!(r.ok);
        assert_eq!(r.value, 0x1F4);
    }
    #[test]
    fn zero_literal() {
        let r = eval("0");
        assert!(r.ok);
        assert_eq!(r.value, 0);
    }
    #[test]
    fn large_64bit() {
        let r = eval("7FF66CCE0000");
        assert!(r.ok);
        assert_eq!(r.value, 0x7FF66CCE0000);
    }

    // -- Arithmetic --

    #[test]
    fn addition() {
        let r = eval("0x100 + 0x200");
        assert!(r.ok);
        assert_eq!(r.value, 0x300);
    }
    #[test]
    fn subtraction() {
        let r = eval("0x300 - 0x100");
        assert!(r.ok);
        assert_eq!(r.value, 0x200);
    }
    #[test]
    fn multiplication() {
        let r = eval("0x10 * 4");
        assert!(r.ok);
        assert_eq!(r.value, 0x40);
    }
    #[test]
    fn division() {
        let r = eval("0x100 / 2");
        assert!(r.ok);
        assert_eq!(r.value, 0x80);
    }
    #[test]
    fn precedence() {
        // 0x10 + 2*3 = 0x10 + 6 = 0x16
        let r = eval("0x10 + 2 * 3");
        assert!(r.ok);
        assert_eq!(r.value, 0x16);
    }
    #[test]
    fn parentheses() {
        // (0x10 + 2) * 3 = 0x12 * 3 = 0x36
        let r = eval("(0x10 + 2) * 3");
        assert!(r.ok);
        assert_eq!(r.value, 0x36);
    }

    // -- Unary minus --

    #[test]
    fn unary_minus() {
        let r = eval("-0x10 + 0x20");
        assert!(r.ok);
        assert_eq!(r.value, 0x10);
    }

    // -- Module resolution --

    #[test]
    fn module_resolve() {
        let cbs = cb_module(|name| {
            let ok = name == "Program.exe";
            (if ok { 0x140000000 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("<Program.exe> + 0x123", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x140000123);
    }
    #[test]
    fn module_not_found() {
        let cbs = cb_module(|_| (0, false));
        let r = AddressParser::evaluate("<NoSuch.dll>", 8, Some(&cbs));
        assert!(!r.ok);
        assert!(r.error.contains("not found"));
    }

    // -- Dereference --

    #[test]
    fn deref_simple() {
        let cbs = cb_read(|addr| {
            let ok = addr == 0x1000;
            (if ok { 0xDEADBEEF } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("[0x1000]", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0xDEADBEEF);
    }
    #[test]
    fn deref_nested() {
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
    }
    #[test]
    fn deref_read_failure() {
        let cbs = cb_read(|_| (0, false));
        let r = AddressParser::evaluate("[0x1000]", 8, Some(&cbs));
        assert!(!r.ok);
        assert!(r.error.contains("failed to read"));
    }

    // -- Complex expression --

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
        // [<Program.exe> + 0xDE] - AB = 0x500000 - 0xAB = 0x4FFF55
        let r = AddressParser::evaluate("[<Program.exe> + 0xDE] - AB", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x4FFF55);
    }

    // -- Errors --

    #[test]
    fn empty_input() {
        let r = eval("");
        assert!(!r.ok);
    }
    #[test]
    fn unmatched_bracket() {
        let r = eval("[0x1000");
        assert!(!r.ok);
        assert!(r.error.contains("']'"));
    }
    #[test]
    fn unmatched_angle() {
        let r = eval("<Program.exe");
        assert!(!r.ok);
        assert!(r.error.contains("'>'"));
    }
    #[test]
    fn division_by_zero() {
        let r = eval("0x100 / 0");
        assert!(!r.ok);
        assert!(r.error.contains("division by zero"));
    }
    #[test]
    fn trailing_garbage() {
        let r = eval("0x100 xyz");
        assert!(!r.ok);
        assert!(r.error.contains("unexpected"));
    }
    #[test]
    fn trailing_operator() {
        let r = eval("0x100 +");
        assert!(!r.ok);
    }

    // -- Validation --

    #[test]
    fn validate_valid() {
        assert_eq!(AddressParser::validate("0x100 + 0x200"), "");
        assert_eq!(AddressParser::validate("<Prog.exe> + [0x100]"), "");
    }
    #[test]
    fn validate_invalid() {
        assert!(!AddressParser::validate("").is_empty());
        assert!(!AddressParser::validate("[0x100").is_empty());
        assert!(!AddressParser::validate("0x100 xyz").is_empty());
    }

    // -- Backtick stripping --

    #[test]
    fn backtick_stripping() {
        let r = eval("7ff6`6cce0000");
        assert!(r.ok);
        assert_eq!(r.value, 0x7FF66CCE0000);
    }

    // -- Whitespace tolerance --

    #[test]
    fn whitespace() {
        let r = eval("  0x100  +  0x200  ");
        assert!(r.ok);
        assert_eq!(r.value, 0x300);
    }

    // -- Legacy compat: simple hex --

    #[test]
    fn simple_hex_address() {
        let r = eval("140000000");
        assert!(r.ok);
        assert_eq!(r.value, 0x140000000);
    }

    // -- Multiple additions --

    #[test]
    fn multiple_additions() {
        let r = eval("0x100 + 0x200 + 0x300");
        assert!(r.ok);
        assert_eq!(r.value, 0x600);
    }

    // -- Identifier resolution --

    #[test]
    fn ident_base() {
        let cbs = cb_ident(|name| {
            let ok = name == "base";
            (if ok { 0x140000000 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("base", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x140000000);
    }
    #[test]
    fn ident_field_name() {
        let cbs = cb_ident(|name| match name {
            "base" => (0x140000000, true),
            "e_lfanew" => (0xE8, true),
            _ => (0, false),
        });
        let r = AddressParser::evaluate("base + e_lfanew", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x1400000E8);
    }
    #[test]
    fn ident_unknown() {
        let cbs = cb_ident(|_| (0, false));
        let r = AddressParser::evaluate("unknown_var", 8, Some(&cbs));
        assert!(!r.ok);
        assert!(r.error.contains("unknown identifier"));
    }

    // -- Hex vs identifier disambiguation --

    #[test]
    fn hex_disambig_dead() {
        // "DEAD" is all hex digits → hex 0xDEAD
        let r = eval("DEAD");
        assert!(r.ok);
        assert_eq!(r.value, 0xDEAD);
    }
    #[test]
    fn hex_disambig_base() {
        let cbs = cb_ident(|name| {
            let ok = name == "base";
            (if ok { 42 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("base", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 42);
    }
    #[test]
    fn hex_disambig_abc_with_underscore() {
        let cbs = cb_ident(|name| {
            let ok = name == "ABC_field";
            (if ok { 99 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("ABC_field", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 99);
    }

    // -- Bitwise operators --

    #[test]
    fn bitwise_and() {
        let r = eval("0xFF & 0x0F");
        assert!(r.ok);
        assert_eq!(r.value, 0x0F);
    }
    #[test]
    fn bitwise_or_op() {
        let r = eval("0xA0 | 0x0B");
        assert!(r.ok);
        assert_eq!(r.value, 0xAB);
    }
    #[test]
    fn bitwise_xor() {
        let r = eval("0xA ^ 0x5");
        assert!(r.ok);
        assert_eq!(r.value, 0xF);
    }
    #[test]
    fn shift_left() {
        let r = eval("1 << 4");
        assert!(r.ok);
        assert_eq!(r.value, 0x10);
    }
    #[test]
    fn shift_right() {
        let r = eval("0xFF00 >> 8");
        assert!(r.ok);
        assert_eq!(r.value, 0xFF);
    }

    // -- Unary bitwise NOT --

    #[test]
    fn unary_not() {
        let r = eval("~0");
        assert!(r.ok);
        assert_eq!(r.value, 0xFFFFFFFFFFFFFFFF);
    }
    #[test]
    fn unary_not_mask() {
        let r = eval("~0xFFF");
        assert!(r.ok);
        assert_eq!(r.value, 0xFFFFFFFFFFFFF000);
    }

    // -- Operator precedence --

    #[test]
    fn shift_precedence() {
        // 1 + 2 << 3 = (1 + 2) << 3 = 0x18
        let r = eval("1 + 2 << 3");
        assert!(r.ok);
        assert_eq!(r.value, 0x18);
    }
    #[test]
    fn and_or_precedence() {
        // 0xFF | 0x100 & 0xF00 = 0xFF | 0x100 = 0x1FF
        let r = eval("0xFF | 0x100 & 0xF00");
        assert!(r.ok);
        assert_eq!(r.value, 0x1FF);
    }
    #[test]
    fn xor_precedence() {
        // 0xF0 | 0x0F ^ 0xFF & 0x0F = 0xF0
        let r = eval("0xF0 | 0x0F ^ 0xFF & 0x0F");
        assert!(r.ok);
        assert_eq!(r.value, 0xF0);
    }

    // -- E_lfanew end-to-end --

    #[test]
    fn elfanew_scenario() {
        let cbs = cb_ident(|name| match name {
            "base" => (0x140000000, true),
            "e_lfanew" => (0xE8, true),
            _ => (0, false),
        });
        let r = AddressParser::evaluate("base + e_lfanew", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x1400000E8);
    }
    #[test]
    fn page_aligned_expr() {
        let cbs = cb_ident(|name| match name {
            "base" => (0x140000000, true),
            "e_lfanew" => (0xE8, true),
            _ => (0, false),
        });
        // (base + e_lfanew) & ~0xFFF = 0x140000000
        let r = AddressParser::evaluate("(base + e_lfanew) & ~0xFFF", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x140000000);
    }

    // -- Bare module.dll identifier --

    #[test]
    fn bare_module_dll() {
        let cbs = cb_ident(|name| {
            let ok = name == "client.dll";
            (if ok { 0x7FF600000000 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("client.dll + 0xFF", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x7FF6000000FF);
    }
    #[test]
    fn bare_module_exe() {
        let cbs = cb_ident(|name| {
            let ok = name == "cs2.exe";
            (if ok { 0x140000000 } else { 0 }, ok)
        });
        let r = AddressParser::evaluate("cs2.exe + 0xDE", 8, Some(&cbs));
        assert!(r.ok);
        assert_eq!(r.value, 0x1400000DE);
    }

    // -- Validate with new syntax --

    #[test]
    fn validate_identifier() {
        assert_eq!(AddressParser::validate("base + e_lfanew"), "");
    }
    #[test]
    fn validate_bitwise_ops() {
        assert_eq!(AddressParser::validate("0xFF & 0x0F"), "");
        assert_eq!(AddressParser::validate("1 << 4"), "");
        assert_eq!(AddressParser::validate("~0xFFF"), "");
    }

    // -- Additional parity checks (not weakened; cover documented behavior) --

    #[test]
    fn validate_empty_literal() {
        // validate's empty fast-path returns the literal "empty".
        assert_eq!(AddressParser::validate(""), "empty");
        assert_eq!(AddressParser::validate("   "), "empty");
    }
    #[test]
    fn hex_overflow_invalid() {
        // 17 significant hex digits overflows u64 → "invalid hex number".
        // NOTE: parseHexNumber sets m_errorPos=start, but fail() then
        // overwrites it back to m_pos (addressparser.cpp:84) — so error_pos is
        // the position past the consumed digits, not `start`.
        let r = eval("10000000000000000");
        assert!(!r.ok);
        assert!(r.error.contains("invalid hex number"));
        assert_eq!(r.error_pos, 17);
    }
    #[test]
    fn prefix_no_digits() {
        // "0x" → m_pos advances to 2 over the prefix, no digits; fail() records
        // error_pos = m_pos = 2 (clobbering the earlier start assignment).
        let r = eval("0x");
        assert!(!r.ok);
        assert!(r.error.contains("expected hex number"));
        assert_eq!(r.error_pos, 2);
    }
    #[test]
    fn single_quote_stripped() {
        let r = eval("7ff6'6cce'0000");
        assert!(r.ok);
        assert_eq!(r.value, 0x7FF66CCE0000);
    }
    #[test]
    fn function_no_callback_returns_zero() {
        // Without kernel callbacks, function calls parse and yield 0.
        assert!(eval("vtop(1, 0x1000)").ok);
        assert!(eval("cr3(4)").ok);
        assert!(eval("phys(0x1000)").ok);
    }
    #[test]
    fn unknown_function() {
        let r = eval("frobnicate(1)");
        assert!(!r.ok);
        assert!(r.error.contains("unknown function"));
    }
    #[test]
    fn empty_module_name() {
        let r = eval("<>");
        assert!(!r.ok);
        assert!(r.error.contains("empty module name"));
    }
    #[test]
    fn deref_no_callback_zero() {
        // No readPointer callback → deref resolves to 0, syntax valid.
        let r = eval("[0x1000]");
        assert!(r.ok);
        assert_eq!(r.value, 0);
    }
    #[test]
    fn vtop_requires_two_args() {
        let r = eval("vtop(1)");
        assert!(!r.ok);
        assert!(r.error.contains("vtop() requires 2 arguments"));
    }
}
