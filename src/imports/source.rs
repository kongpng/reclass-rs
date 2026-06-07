//! C/C++ source → `NodeTree` (`importFromSource`).
//!
//! Faithful 1:1 port of `src/imports/import_source.cpp` (1624 lines). PURE /
//! fully portable (no OS calls, no threading). Hand-rolled tokenizer +
//! recursive-descent parser for a *subset* of C/C++ struct declarations; robust
//! to garbage (skips what it can't parse). See BMAP §1.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use std::sync::LazyLock;

use crate::core::kind::{alignment_for, NodeKind};
use crate::core::node::{BitfieldMember, Node};
use crate::core::tree::NodeTree;

use super::{largest_hex_cell_for_run, resolve_pending_refs, ImportError, PendingRef};

// ── Built-in type alias table (cpp:10-124) ──

/// `struct TypeInfo {NodeKind kind; int size;}` (`import_source.cpp:12-15`).
#[derive(Clone, Copy)]
struct TypeInfo {
    kind: NodeKind,
    /// bytes (0 = dynamic/pointer).
    size: i32,
}

/// `buildTypeTable(int ptrSize=8)` (`import_source.cpp:17-124`) — VERBATIM.
fn build_type_table(ptr_size: i32) -> HashMap<String, TypeInfo> {
    use NodeKind::*;
    let mut t: HashMap<String, TypeInfo> = HashMap::new();

    // Pointer/size_t kinds depend on target architecture
    let ptr_kind = if ptr_size >= 8 { Pointer64 } else { Pointer32 };
    let uintp_kind = if ptr_size >= 8 { UInt64 } else { UInt32 };
    let intp_kind = if ptr_size >= 8 { Int64 } else { Int32 };

    macro_rules! ins {
        ($n:literal, $k:expr, $s:expr) => {
            t.insert($n.to_string(), TypeInfo { kind: $k, size: $s });
        };
    }

    // stdint.h
    ins!("uint8_t", UInt8, 1);
    ins!("int8_t", Int8, 1);
    ins!("uint16_t", UInt16, 2);
    ins!("int16_t", Int16, 2);
    ins!("uint32_t", UInt32, 4);
    ins!("int32_t", Int32, 4);
    ins!("uint64_t", UInt64, 8);
    ins!("int64_t", Int64, 8);
    ins!("__int8", Int8, 1);
    ins!("__int16", Int16, 2);
    ins!("__int32", Int32, 4);
    ins!("__int64", Int64, 8);
    ins!("unsigned __int8", UInt8, 1);
    ins!("unsigned __int16", UInt16, 2);
    ins!("unsigned __int32", UInt32, 4);
    ins!("unsigned __int64", UInt64, 8);

    // Standard C
    ins!("char", Int8, 1);
    ins!("short", Int16, 2);
    ins!("int", Int32, 4);
    ins!("long", Int32, 4);
    ins!("float", Float, 4);
    ins!("double", Double, 8);
    ins!("bool", Bool, 1);
    ins!("_Bool", Bool, 1);
    ins!("void", Hex8, 1);
    ins!("wchar_t", UInt16, 2);

    // Multi-word C types (pre-merged by parser)
    ins!("unsigned char", UInt8, 1);
    ins!("signed char", Int8, 1);
    ins!("unsigned short", UInt16, 2);
    ins!("signed short", Int16, 2);
    ins!("unsigned int", UInt32, 4);
    ins!("signed int", Int32, 4);
    ins!("unsigned", UInt32, 4);
    ins!("long long", Int64, 8);
    ins!("unsigned long", UInt32, 4);
    ins!("signed long", Int32, 4);
    ins!("unsigned long long", UInt64, 8);
    ins!("signed long long", Int64, 8);
    ins!("long int", Int32, 4);
    ins!("long long int", Int64, 8);
    ins!("unsigned long int", UInt32, 4);
    ins!("unsigned long long int", UInt64, 8);
    ins!("short int", Int16, 2);
    ins!("unsigned short int", UInt16, 2);

    // Windows types
    ins!("BYTE", UInt8, 1);
    ins!("UCHAR", UInt8, 1);
    ins!("BOOLEAN", UInt8, 1);
    ins!("CHAR", Int8, 1);
    ins!("WORD", UInt16, 2);
    ins!("USHORT", UInt16, 2);
    ins!("SHORT", Int16, 2);
    ins!("WCHAR", UInt16, 2);
    ins!("TCHAR", UInt16, 2);
    ins!("DWORD", UInt32, 4);
    ins!("ULONG", UInt32, 4);
    ins!("UINT", UInt32, 4);
    ins!("LONG", Int32, 4);
    ins!("LONG32", Int32, 4);
    ins!("INT", Int32, 4);
    ins!("BOOL", Int32, 4);
    ins!("FLOAT", Float, 4);
    ins!("QWORD", UInt64, 8);
    ins!("ULONGLONG", UInt64, 8);
    ins!("DWORD64", UInt64, 8);
    ins!("ULONG64", UInt64, 8);
    ins!("UINT64", UInt64, 8);
    ins!("LONGLONG", Int64, 8);
    ins!("LONG64", Int64, 8);
    ins!("INT64", Int64, 8);
    ins!("_BYTE", UInt8, 1);
    ins!("_WORD", UInt16, 2);
    ins!("_DWORD", UInt32, 4);
    ins!("_QWORD", UInt64, 8);

    // Platform pointer-size types (depend on target architecture)
    ins!("PVOID", ptr_kind, ptr_size);
    ins!("LPVOID", ptr_kind, ptr_size);
    ins!("HANDLE", ptr_kind, ptr_size);
    ins!("HMODULE", ptr_kind, ptr_size);
    ins!("HWND", ptr_kind, ptr_size);
    ins!("HINSTANCE", ptr_kind, ptr_size);
    ins!("SIZE_T", uintp_kind, ptr_size);
    ins!("ULONG_PTR", uintp_kind, ptr_size);
    ins!("UINT_PTR", uintp_kind, ptr_size);
    ins!("DWORD_PTR", uintp_kind, ptr_size);
    ins!("LONG_PTR", intp_kind, ptr_size);
    ins!("INT_PTR", intp_kind, ptr_size);
    ins!("SSIZE_T", intp_kind, ptr_size);
    ins!("uintptr_t", uintp_kind, ptr_size);
    ins!("intptr_t", intp_kind, ptr_size);
    ins!("size_t", uintp_kind, ptr_size);
    ins!("ptrdiff_t", intp_kind, ptr_size);
    ins!("ssize_t", intp_kind, ptr_size);

    // Pointer type aliases
    ins!("PCHAR", ptr_kind, ptr_size);
    ins!("LPSTR", ptr_kind, ptr_size);
    ins!("LPCSTR", ptr_kind, ptr_size);
    ins!("PCSTR", ptr_kind, ptr_size);
    ins!("PWSTR", ptr_kind, ptr_size);
    ins!("LPWSTR", ptr_kind, ptr_size);
    ins!("LPCWSTR", ptr_kind, ptr_size);
    ins!("PCWSTR", ptr_kind, ptr_size);

    t
}

// ── Tokenizer (cpp:126-281) ──

/// `enum class TokKind` (`import_source.cpp:128-132`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokKind {
    Ident,
    Number,
    Star,
    Semi,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Colon,
    Equals,
    Hash,
    Eof,
    Other,
}

/// `struct Token` (`import_source.cpp:134-138`).
#[derive(Clone, Debug)]
struct Token {
    kind: TokKind,
    text: String,
    line: i32,
}

/// `struct LineOffset` (`import_source.cpp:141-144`).
#[derive(Clone, Copy)]
struct LineOffset {
    line: i32,
    offset: i32,
}

// Offset comment regexes (cpp:230, 232, 237). Compiled once.
static OFFSET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:->\s*\S+\s+)?0x([0-9A-Fa-f]+)$").unwrap());
static END_HEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b0x([0-9A-Fa-f]+)\s*$").unwrap());

/// `Tokenizer::tokenize()` (`import_source.cpp:146-281`). Returns the tokens
/// and the captured `// 0xNN` comment offsets.
fn tokenize(src: &str) -> (Vec<Token>, Vec<LineOffset>) {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0usize;
    let mut line = 1i32;
    let mut tokens: Vec<Token> = Vec::new();
    let mut offsets: Vec<LineOffset> = Vec::new();
    let n = chars.len();

    while pos < n {
        // skipWhitespace
        while pos < n {
            if chars[pos] == '\n' {
                line += 1;
                pos += 1;
            } else if chars[pos].is_whitespace() {
                pos += 1;
            } else {
                break;
            }
        }
        if pos >= n {
            break;
        }

        let c = chars[pos];

        // Line comments
        if c == '/' && pos + 1 < n && chars[pos + 1] == '/' {
            let comment_line = line;
            pos += 2; // skip //
            let start = pos;
            while pos < n && chars[pos] != '\n' {
                pos += 1;
            }
            let comment: String = chars[start..pos]
                .iter()
                .collect::<String>()
                .trim()
                .to_string();
            // offsetRe first, then endHexRe fallback (cpp:233-239)
            let captured: Option<i32> = OFFSET_RE
                .captures(&comment)
                .or_else(|| END_HEX_RE.captures(&comment))
                .and_then(|cap| cap.get(1))
                .and_then(|m| i32::from_str_radix(m.as_str(), 16).ok());
            if let Some(val) = captured {
                offsets.push(LineOffset {
                    line: comment_line,
                    offset: val,
                });
            }
            continue;
        }
        // Block comments
        if c == '/' && pos + 1 < n && chars[pos + 1] == '*' {
            pos += 2; // skip /*
            let mut terminated = false;
            while pos + 1 < n {
                if chars[pos] == '\n' {
                    line += 1;
                }
                if chars[pos] == '*' && chars[pos + 1] == '/' {
                    pos += 2;
                    terminated = true;
                    break;
                }
                pos += 1;
            }
            if !terminated {
                pos = n; // unterminated
            }
            continue;
        }
        // Preprocessor lines - skip entirely
        if c == '#' {
            while pos < n && chars[pos] != '\n' {
                pos += 1;
            }
            continue;
        }
        // Identifiers / keywords
        if c.is_alphabetic() || c == '_' {
            let start = pos;
            while pos < n && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
                pos += 1;
            }
            tokens.push(Token {
                kind: TokKind::Ident,
                text: chars[start..pos].iter().collect(),
                line,
            });
            continue;
        }
        // Numbers
        if c.is_ascii_digit() {
            let start = pos;
            if chars[pos] == '0' && pos + 1 < n && (chars[pos + 1] == 'x' || chars[pos + 1] == 'X')
            {
                pos += 2;
                while pos < n
                    && (chars[pos].is_ascii_digit()
                        || ('a'..='f').contains(&chars[pos])
                        || ('A'..='F').contains(&chars[pos]))
                {
                    pos += 1;
                }
            } else {
                while pos < n && chars[pos].is_ascii_digit() {
                    pos += 1;
                }
            }
            // Skip integer suffixes (U, L, LL, ULL, etc.)
            while pos < n
                && (chars[pos] == 'u'
                    || chars[pos] == 'U'
                    || chars[pos] == 'l'
                    || chars[pos] == 'L')
            {
                pos += 1;
            }
            tokens.push(Token {
                kind: TokKind::Number,
                text: chars[start..pos].iter().collect(),
                line,
            });
            continue;
        }
        // Single-character tokens
        let tk = match c {
            '*' => TokKind::Star,
            ';' => TokKind::Semi,
            '{' => TokKind::LBrace,
            '}' => TokKind::RBrace,
            '[' => TokKind::LBracket,
            ']' => TokKind::RBracket,
            '(' => TokKind::LParen,
            ')' => TokKind::RParen,
            ',' => TokKind::Comma,
            ':' => TokKind::Colon,
            '=' => TokKind::Equals,
            _ => TokKind::Other,
        };
        tokens.push(Token {
            kind: tk,
            text: c.to_string(),
            line,
        });
        pos += 1;
    }

    tokens.push(Token {
        kind: TokKind::Eof,
        text: String::new(),
        line,
    });
    (tokens, offsets)
}

// ── Parser state structs (cpp:285-326) ──

/// `struct ParsedField` (`import_source.cpp:285-296`).
#[derive(Clone, Default)]
struct ParsedField {
    type_name: String,
    name: String,
    is_pointer: bool,
    #[allow(dead_code)]
    pointer_depth: i32,
    array_sizes: Vec<i32>,
    /// -1 = none.
    comment_offset: i32,
    /// -1 = not a bitfield.
    bitfield_width: i32,
    pointer_target: String,
    is_union: bool,
    union_members: Vec<ParsedField>,
}

impl ParsedField {
    fn new() -> Self {
        ParsedField {
            comment_offset: -1,
            bitfield_width: -1,
            ..Default::default()
        }
    }
}

/// `struct ParsedStruct` (`import_source.cpp:298-304`).
#[derive(Clone, Default)]
struct ParsedStruct {
    name: String,
    /// "struct"/"class"/"union"/"enum".
    keyword: String,
    fields: Vec<ParsedField>,
    enum_values: Vec<(String, i64)>,
}

// Multi-word type prefix keywords (cpp:312-317)
fn is_type_modifier(s: &str) -> bool {
    s == "unsigned" || s == "signed" || s == "long" || s == "short"
}

// (cpp:319-326)
fn is_qualifier(s: &str) -> bool {
    s == "const"
        || s == "volatile"
        || s == "mutable"
        || s == "struct"
        || s == "class"
        || s == "enum"
}

// ── Parser (cpp:328-1049) ──

struct Parser<'a> {
    tokens: &'a [Token],
    line_offsets: &'a [LineOffset],
    cur: usize,

    structs: Vec<ParsedStruct>,
    forward_decls: HashSet<String>,
    typedefs: HashMap<String, String>,
    pointer_typedefs: HashSet<String>,
    array_typedefs: HashMap<String, Vec<i32>>,
    size_asserts: HashMap<String, i32>,
    struct_alignments: HashMap<String, i32>,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], line_offsets: &'a [LineOffset]) -> Self {
        Parser {
            tokens,
            line_offsets,
            cur: 0,
            structs: Vec::new(),
            forward_decls: HashSet::new(),
            typedefs: HashMap::new(),
            pointer_typedefs: HashSet::new(),
            array_typedefs: HashMap::new(),
            size_asserts: HashMap::new(),
            struct_alignments: HashMap::new(),
        }
    }

    // peek(ahead=0): out-of-range → tokens.back() (which is Eof) (cpp:344-347)
    fn peek(&self, ahead: usize) -> &Token {
        let i = self.cur + ahead;
        if i < self.tokens.len() {
            &self.tokens[i]
        } else {
            self.tokens.last().unwrap()
        }
    }

    // advance() (cpp:349-352): returns current then increments; clamps at last
    fn advance(&mut self) -> Token {
        if self.cur < self.tokens.len() - 1 {
            let t = self.tokens[self.cur].clone();
            self.cur += 1;
            t
        } else {
            self.tokens.last().unwrap().clone()
        }
    }

    fn check(&self, k: TokKind) -> bool {
        self.peek(0).kind == k
    }

    fn check_ident(&self, s: &str) -> bool {
        let t = self.peek(0);
        t.kind == TokKind::Ident && t.text == s
    }

    fn match_kind(&mut self, k: TokKind) -> bool {
        if self.check(k) {
            self.advance();
            true
        } else {
            false
        }
    }

    // skipToSemiOrBrace (cpp:367-380)
    fn skip_to_semi_or_brace(&mut self) {
        let mut depth = 0;
        while self.peek(0).kind != TokKind::Eof {
            let k = self.peek(0).kind;
            if k == TokKind::LBrace {
                depth += 1;
            } else if k == TokKind::RBrace {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if k == TokKind::Semi && depth == 0 {
                self.advance();
                return;
            }
            self.advance();
        }
    }

    // skipAlignMacro (cpp:384-403)
    fn skip_align_macro(&mut self) -> i32 {
        if self.check_ident("ALIGN") || self.check_ident("__declspec") {
            self.advance();
            let mut align_val = 0;
            if self.match_kind(TokKind::LParen) {
                if self.peek(0).kind == TokKind::Number {
                    align_val = self.peek(0).text.parse::<i32>().unwrap_or(0);
                }
                let mut depth = 1;
                while depth > 0 && self.peek(0).kind != TokKind::Eof {
                    if self.peek(0).kind == TokKind::LParen {
                        depth += 1;
                    } else if self.peek(0).kind == TokKind::RParen {
                        depth -= 1;
                    }
                    self.advance();
                }
            }
            return align_val;
        }
        0
    }

    // peekPastAlign (cpp:406-423)
    fn peek_past_align(&self, offset: usize, expected: TokKind) -> bool {
        let mut i = self.cur + offset;
        if i < self.tokens.len()
            && self.tokens[i].kind == TokKind::Ident
            && (self.tokens[i].text == "ALIGN" || self.tokens[i].text == "__declspec")
        {
            i += 1; // skip ALIGN
            if i < self.tokens.len() && self.tokens[i].kind == TokKind::LParen {
                let mut depth = 1;
                i += 1;
                while i < self.tokens.len() && depth > 0 {
                    if self.tokens[i].kind == TokKind::LParen {
                        depth += 1;
                    } else if self.tokens[i].kind == TokKind::RParen {
                        depth -= 1;
                    }
                    i += 1;
                }
            }
            return i < self.tokens.len() && self.tokens[i].kind == expected;
        }
        false
    }

    // ── Top-level parse (cpp:427-447) ──
    fn parse(&mut self) {
        while self.peek(0).kind != TokKind::Eof {
            if self.check_ident("struct") || self.check_ident("class") {
                self.parse_struct_or_forward();
            } else if self.check_ident("union") {
                self.parse_top_level_union();
            } else if self.check_ident("static_assert") {
                self.parse_static_assert();
            } else if self.check_ident("typedef") {
                self.parse_typedef();
            } else if self.check_ident("enum") {
                self.parse_enum_def();
            } else if self.peek(0).kind == TokKind::Hash {
                self.advance();
                while self.peek(0).kind != TokKind::Eof && self.peek(0).kind != TokKind::Semi {
                    self.advance();
                }
            } else {
                self.advance(); // skip unknown
            }
        }
    }

    // parseStructOrForward (cpp:449-498)
    fn parse_struct_or_forward(&mut self) {
        let keyword = self.advance().text; // "struct" or "class"

        let align_val = self.skip_align_macro();

        // Anonymous struct: struct { ... }
        if self.check(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            if self.check(TokKind::RBrace) {
                self.advance();
                self.match_kind(TokKind::Semi);
            }
            return;
        }

        if !self.check(TokKind::Ident) {
            self.skip_to_semi_or_brace();
            return;
        }
        let name = self.advance().text;

        if align_val > 0 {
            self.struct_alignments.insert(name.clone(), align_val);
        }

        // Inheritance: struct Foo : public Bar {  — skip the clause
        if self.check(TokKind::Colon) {
            self.advance(); // ':'
            while self.peek(0).kind != TokKind::LBrace
                && self.peek(0).kind != TokKind::Semi
                && self.peek(0).kind != TokKind::Eof
            {
                self.advance();
            }
        }

        // Forward declaration: struct Foo;
        if self.check(TokKind::Semi) {
            self.advance();
            self.forward_decls.insert(name);
            return;
        }

        if !self.match_kind(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            return;
        }

        let mut ps = ParsedStruct {
            name,
            keyword,
            ..Default::default()
        };

        self.parse_struct_body(&mut ps);

        if !self.match_kind(TokKind::RBrace) {
            self.skip_to_semi_or_brace();
            return;
        }
        self.match_kind(TokKind::Semi);

        self.structs.push(ps);
    }

    // parseStructBody (cpp:500-559)
    fn parse_struct_body(&mut self, ps: &mut ParsedStruct) {
        while self.peek(0).kind != TokKind::RBrace && self.peek(0).kind != TokKind::Eof {
            // Nested struct definition
            if self.check_ident("struct") || self.check_ident("class") {
                // struct [ALIGN(N)] Name {
                if (self.peek(1).kind == TokKind::Ident && self.peek(2).kind == TokKind::LBrace)
                    || self.peek_past_align(1, TokKind::Ident)
                {
                    self.parse_struct_or_forward();
                    continue;
                }
                // struct [ALIGN(N)] {
                if self.peek(1).kind == TokKind::LBrace || self.peek_past_align(1, TokKind::LBrace)
                {
                    // Anonymous nested struct { ... } fieldName;
                    self.advance(); // skip "struct"
                    self.skip_align_macro();
                    self.advance(); // skip "{"
                    let mut depth = 1;
                    while self.peek(0).kind != TokKind::Eof && depth > 0 {
                        if self.peek(0).kind == TokKind::LBrace {
                            depth += 1;
                        } else if self.peek(0).kind == TokKind::RBrace {
                            depth -= 1;
                        }
                        if depth > 0 {
                            self.advance();
                        }
                    }
                    if self.check(TokKind::RBrace) {
                        self.advance();
                    }
                    if self.check(TokKind::Ident) {
                        self.advance();
                    }
                    self.match_kind(TokKind::Semi);
                    continue;
                }
                // Might be "struct TypeName fieldName;" - fall through to field parsing
            }

            if self.check_ident("union") {
                self.parse_union(ps);
                continue;
            }

            if self.check_ident("enum") {
                self.parse_enum_def();
                continue;
            }

            if self.check_ident("static_assert") {
                self.parse_static_assert();
                continue;
            }

            // Try to parse as a field
            if let Some(field) = self.parse_field() {
                ps.fields.push(field);
            } else {
                self.advance(); // skip unrecognized token
            }
        }
    }

    // parseTopLevelUnion (cpp:563-601)
    fn parse_top_level_union(&mut self) {
        self.advance(); // skip "union"
        let align_val = self.skip_align_macro();

        // Forward declaration: union Name;
        if self.check(TokKind::Ident) && self.peek(1).kind == TokKind::Semi {
            let name = self.advance().text;
            self.advance(); // skip ;
            self.forward_decls.insert(name);
            return;
        }

        // Anonymous union at top level (skip)
        if self.check(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            if self.check(TokKind::RBrace) {
                self.advance();
                self.match_kind(TokKind::Semi);
            }
            return;
        }

        if !self.check(TokKind::Ident) {
            self.skip_to_semi_or_brace();
            return;
        }
        let name = self.advance().text;

        if align_val > 0 {
            self.struct_alignments.insert(name.clone(), align_val);
        }

        if !self.match_kind(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            return;
        }

        let mut ps = ParsedStruct {
            name,
            keyword: "union".to_string(),
            ..Default::default()
        };

        self.parse_struct_body(&mut ps);

        if !self.match_kind(TokKind::RBrace) {
            self.skip_to_semi_or_brace();
            return;
        }
        self.match_kind(TokKind::Semi);

        self.structs.push(ps);
    }

    // parseUnion (cpp:603-682)
    fn parse_union(&mut self, ps: &mut ParsedStruct) {
        self.advance(); // skip "union"
        self.skip_align_macro();

        // Optional union tag name (before {)
        if self.check(TokKind::Ident) && self.peek(1).kind == TokKind::LBrace {
            self.advance(); // skip union tag name
        }

        if !self.match_kind(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            return;
        }

        let mut union_field = ParsedField::new();
        union_field.is_union = true;

        while self.peek(0).kind != TokKind::RBrace && self.peek(0).kind != TokKind::Eof {
            // Nested unions
            if self.check_ident("union") {
                let mut tmp = ParsedStruct::default();
                self.parse_union(&mut tmp);
                for f in tmp.fields {
                    union_field.union_members.push(f);
                }
                continue;
            }

            // Anonymous struct inside union: struct [ALIGN(N)] { ... };
            if (self.check_ident("struct") || self.check_ident("class"))
                && (self.peek(1).kind == TokKind::LBrace
                    || self.peek_past_align(1, TokKind::LBrace))
            {
                self.advance(); // skip "struct"
                self.skip_align_macro();
                self.advance(); // skip "{"
                let mut depth = 1;
                while self.peek(0).kind != TokKind::Eof && depth > 0 {
                    if self.peek(0).kind == TokKind::LBrace {
                        depth += 1;
                    } else if self.peek(0).kind == TokKind::RBrace {
                        depth -= 1;
                    }
                    if depth > 0 {
                        self.advance();
                    }
                }
                if self.check(TokKind::RBrace) {
                    self.advance();
                }
                if self.check(TokKind::Ident) {
                    self.advance();
                }
                self.match_kind(TokKind::Semi);
                continue;
            }

            // Nested named struct definition inside union
            if (self.check_ident("struct") || self.check_ident("class"))
                && ((self.peek(1).kind == TokKind::Ident && self.peek(2).kind == TokKind::LBrace)
                    || self.peek_past_align(1, TokKind::Ident))
            {
                self.parse_struct_or_forward();
                continue;
            }

            if let Some(field) = self.parse_field() {
                union_field.union_members.push(field);
            } else {
                self.advance();
            }
        }
        self.match_kind(TokKind::RBrace);

        // Optional field name after union close: union { ... } u3;
        if self.check(TokKind::Ident) {
            union_field.name = self.advance().text;
        }
        self.match_kind(TokKind::Semi);

        // Determine offset from first member with a known offset
        for m in &union_field.union_members {
            if m.comment_offset >= 0 {
                union_field.comment_offset = m.comment_offset;
                break;
            }
        }

        ps.fields.push(union_field);
    }

    // parseField (cpp:684-792) — rewind on fail
    fn parse_field(&mut self) -> Option<ParsedField> {
        let start_pos = self.cur;
        let mut field = ParsedField::new();

        // Skip qualifiers
        while is_qualifier(&self.peek(0).text) {
            self.advance();
        }

        // Parse type
        let mut type_name = self.parse_type_name();
        if type_name.is_empty() {
            self.cur = start_pos;
            return None;
        }

        // Resolve typedef — track pointer and array typedefs in the chain
        let mut typedef_pointer = false;
        let mut typedef_array_dims: Vec<i32> = Vec::new();
        {
            let mut resolved = type_name.clone();
            let mut seen: HashSet<String> = HashSet::new();
            while self.typedefs.contains_key(&resolved) && !seen.contains(&resolved) {
                if self.pointer_typedefs.contains(&resolved) {
                    typedef_pointer = true;
                }
                if typedef_array_dims.is_empty() {
                    if let Some(dims) = self.array_typedefs.get(&resolved) {
                        typedef_array_dims = dims.clone();
                    }
                }
                seen.insert(resolved.clone());
                resolved = self.typedefs[&resolved].clone();
            }
            type_name = resolved;
        }

        // Pointer stars
        let mut is_pointer = typedef_pointer;
        let mut ptr_depth = if typedef_pointer { 1 } else { 0 };
        while self.match_kind(TokKind::Star) {
            is_pointer = true;
            ptr_depth += 1;
        }

        // Skip const after pointer
        while self.check_ident("const") || self.check_ident("volatile") {
            self.advance();
        }

        // More pointer stars (const Type * const * name)
        while self.match_kind(TokKind::Star) {
            is_pointer = true;
            ptr_depth += 1;
        }

        // Field name
        if !self.check(TokKind::Ident) {
            self.cur = start_pos;
            return None;
        }
        field.name = self.advance().text;

        // Array sizes
        while self.check(TokKind::LBracket) {
            self.advance(); // [
            if self.check(TokKind::Number) {
                let num_text = self.peek(0).text.clone();
                let val = parse_int_token(&num_text);
                if let Some(v) = val {
                    field.array_sizes.push(v);
                }
                self.advance();
            } else if self.check(TokKind::RBracket) {
                field.array_sizes.push(0); // unsized array
            }
            self.match_kind(TokKind::RBracket);
        }

        // Apply array dimensions from typedef
        if !typedef_array_dims.is_empty() {
            if field.array_sizes.is_empty() {
                field.array_sizes = typedef_array_dims;
            } else {
                let mut combined = typedef_array_dims;
                combined.append(&mut field.array_sizes);
                field.array_sizes = combined;
            }
        }

        // Bitfield: Type name : width
        if self.check(TokKind::Colon) {
            self.advance();
            if self.check(TokKind::Number) {
                field.bitfield_width = self.peek(0).text.parse::<i32>().unwrap_or(0).clamp(0, 64);
                self.advance();
            }
        }

        // Expect semicolon
        if !self.match_kind(TokKind::Semi) {
            self.cur = start_pos;
            return None;
        }

        // Match a LineOffset whose .line == toks[start_pos].line
        let field_line = self.tokens[start_pos].line;
        for lo in self.line_offsets {
            if lo.line == field_line {
                field.comment_offset = lo.offset;
                break;
            }
        }

        field.type_name = type_name.clone();
        field.is_pointer = is_pointer;
        field.pointer_depth = ptr_depth;
        if is_pointer {
            field.pointer_target = type_name;
        }

        Some(field)
    }

    // parseTypeName (cpp:794-825)
    fn parse_type_name(&mut self) -> String {
        if self.peek(0).kind != TokKind::Ident {
            return String::new();
        }

        let first = self.peek(0).text.clone();

        // "struct/class/enum TypeName" as a type reference
        if first == "struct" || first == "class" || first == "enum" {
            self.advance();
            if self.check(TokKind::Ident) {
                return self.advance().text;
            }
            return String::new();
        }

        // Multi-word type building
        if is_type_modifier(&first) {
            self.advance();
            let mut parts: Vec<String> = vec![first];
            while self.check(TokKind::Ident)
                && (is_type_modifier(&self.peek(0).text)
                    || self.peek(0).text == "int"
                    || self.peek(0).text == "char"
                    || self.peek(0).text == "long")
            {
                parts.push(self.advance().text);
            }
            return parts.join(" ");
        }

        // Simple identifier type
        self.advance();
        first
    }

    // parseStaticAssert (cpp:827-871)
    fn parse_static_assert(&mut self) {
        self.advance(); // "static_assert"
        if !self.match_kind(TokKind::LParen) {
            self.skip_to_semi_or_brace();
            return;
        }

        let mut depth = 1;
        let mut struct_name = String::new();
        let mut size_val: i32 = -1;

        while depth > 0 && self.peek(0).kind != TokKind::Eof {
            if self.check_ident("sizeof") {
                self.advance();
                if self.match_kind(TokKind::LParen) {
                    if self.check(TokKind::Ident) {
                        struct_name = self.advance().text;
                    }
                    self.match_kind(TokKind::RParen);
                }
            } else if self.peek(0).kind == TokKind::Number && size_val < 0 {
                let num_text = self.peek(0).text.clone();
                size_val = parse_int_token(&num_text).unwrap_or(-1);
                self.advance();
            } else if self.peek(0).kind == TokKind::LParen {
                depth += 1;
                self.advance();
            } else if self.peek(0).kind == TokKind::RParen {
                depth -= 1;
                if depth > 0 {
                    self.advance();
                }
            } else {
                self.advance();
            }
        }
        if depth == 0 {
            self.advance(); // consume closing ')'
        }
        self.match_kind(TokKind::Semi);

        if !struct_name.is_empty() && size_val > 0 {
            self.size_asserts.insert(struct_name, size_val);
        }
    }

    // parseTypedef (cpp:873-964)
    fn parse_typedef(&mut self) {
        self.advance(); // "typedef"

        // typedef struct { ... } Name;
        if self.check_ident("struct") || self.check_ident("class") {
            if self.peek(1).kind == TokKind::LBrace
                || (self.peek(1).kind == TokKind::Ident && self.peek(2).kind == TokKind::LBrace)
            {
                // Full struct typedef - parse as struct, then register alias
                self.parse_struct_or_forward();
                return;
            }
            // typedef struct ExistingName * AliasName;
            self.advance(); // skip struct/class
            if self.check(TokKind::Ident) {
                let existing_name = self.advance().text;
                let mut has_ptr = false;
                while self.match_kind(TokKind::Star) {
                    has_ptr = true;
                }
                while self.check_ident("const") || self.check_ident("volatile") {
                    self.advance();
                }
                if self.check(TokKind::Ident) {
                    let alias_name = self.advance().text;
                    if alias_name != existing_name {
                        self.typedefs.insert(alias_name.clone(), existing_name);
                        if has_ptr {
                            self.pointer_typedefs.insert(alias_name);
                        }
                    }
                }
            }
            self.match_kind(TokKind::Semi);
            return;
        }

        // typedef BaseType [*] AliasName [N];
        while self.check_ident("const") || self.check_ident("volatile") {
            self.advance();
        }
        let base_type = self.parse_type_name();
        if base_type.is_empty() {
            self.skip_to_semi_or_brace();
            return;
        }
        let mut has_ptr = false;
        while self.match_kind(TokKind::Star) {
            has_ptr = true;
        }
        while self.check_ident("const") || self.check_ident("volatile") {
            self.advance();
        }
        while self.match_kind(TokKind::Star) {
            has_ptr = true;
        }

        // Function pointer typedef: typedef RetType ( *Name )( args... );
        if self.check(TokKind::LParen) {
            let save = self.cur;
            self.advance(); // skip (
            let mut is_fn_ptr = false;
            let mut fn_name = String::new();
            if self.match_kind(TokKind::Star) && self.check(TokKind::Ident) {
                fn_name = self.advance().text;
                if self.match_kind(TokKind::RParen) && self.check(TokKind::LParen) {
                    is_fn_ptr = true;
                }
            }
            if is_fn_ptr {
                self.skip_to_semi_or_brace();
                self.pointer_typedefs.insert(fn_name.clone());
                self.typedefs.insert(fn_name, "void".to_string());
            } else {
                self.cur = save;
                self.skip_to_semi_or_brace();
            }
            return;
        }

        if self.check(TokKind::Ident) {
            let alias = self.advance().text;
            // Array dimensions
            let mut dims: Vec<i32> = Vec::new();
            while self.check(TokKind::LBracket) {
                self.advance();
                if self.check(TokKind::Number) {
                    let num_text = self.peek(0).text.clone();
                    if let Some(v) = parse_int_token(&num_text) {
                        dims.push(v);
                    }
                    self.advance();
                }
                self.match_kind(TokKind::RBracket);
            }
            if alias != base_type {
                self.typedefs.insert(alias.clone(), base_type);
                if has_ptr {
                    self.pointer_typedefs.insert(alias.clone());
                }
                if !dims.is_empty() {
                    self.array_typedefs.insert(alias, dims);
                }
            }
        }
        self.match_kind(TokKind::Semi);
    }

    // parseEnumDef (cpp:966-1048)
    fn parse_enum_def(&mut self) {
        self.advance(); // skip "enum"

        // Optional "class" or "struct" (enum class)
        if self.check_ident("class") || self.check_ident("struct") {
            self.advance();
        }

        // Optional name
        let mut name = String::new();
        if self.check(TokKind::Ident) && self.peek(1).kind != TokKind::Semi {
            if self.peek(1).kind == TokKind::LBrace || self.peek(1).kind == TokKind::Colon {
                name = self.advance().text;
            } else {
                // Not an enum definition — revert (field usage like "enum Foo bar;")
                return;
            }
        }

        // Optional underlying type: enum Name : uint8_t { ... }
        if self.check(TokKind::Colon) {
            self.advance();
            self.parse_type_name(); // skip underlying type
        }

        // Forward declaration: enum Name;
        if self.check(TokKind::Semi) {
            self.advance();
            return;
        }

        if !self.match_kind(TokKind::LBrace) {
            self.skip_to_semi_or_brace();
            return;
        }

        let mut ps = ParsedStruct {
            name,
            keyword: "enum".to_string(),
            ..Default::default()
        };

        // Parse enum members
        let mut next_value: i64 = 0;
        while self.peek(0).kind != TokKind::RBrace && self.peek(0).kind != TokKind::Eof {
            if !self.check(TokKind::Ident) {
                self.advance();
                continue;
            }
            let member_name = self.advance().text;
            let mut member_value = next_value;

            if self.check(TokKind::Equals) {
                self.advance();
                let mut negative = false;
                if self.peek(0).kind == TokKind::Other && self.peek(0).text == "-" {
                    negative = true;
                    self.advance();
                }
                if self.check(TokKind::Number) {
                    let num_text = self.peek(0).text.clone();
                    // C++ assigns `memberValue = numText.toLongLong(&ok)`
                    // unconditionally (ignores `ok`); on a parse failure
                    // (e.g. a suffixed literal) Qt returns 0, so we mirror
                    // that with `unwrap_or(0)` rather than keeping the
                    // running auto-increment value.
                    member_value = parse_i64_token(&num_text).unwrap_or(0);
                    if negative {
                        member_value = -member_value;
                    }
                    self.advance();
                } else {
                    // Complex expression — skip to comma or brace
                    while self.peek(0).kind != TokKind::Comma
                        && self.peek(0).kind != TokKind::RBrace
                        && self.peek(0).kind != TokKind::Eof
                    {
                        self.advance();
                    }
                }
            }

            ps.enum_values.push((member_name, member_value));
            next_value = member_value + 1;

            self.match_kind(TokKind::Comma);
        }
        self.match_kind(TokKind::RBrace);
        self.match_kind(TokKind::Semi);

        if !ps.name.is_empty() {
            self.structs.push(ps);
        }
    }
}

/// Parse a TokKind::Number text as i32 (decimal or `0x...`), mirroring the C++
/// `numText.toInt(&ok)` / `numText.mid(2).toInt(&ok, 16)` *exactly*.
///
/// The tokenizer keeps integer suffixes (u/U/l/L) in the token text (the suffix
/// chars are consumed into the token, mirroring `import_source.cpp` lines
/// 288-290). Qt's `QString::toInt(&ok)` rejects any non-numeric trailing
/// characters (`ok = false`, returns 0) — so a suffixed dimension like
/// `field[16u]` must FAIL to parse here, matching `toInt(&ok)` → the caller's
/// `if (ok)` guard then skips the dimension. We therefore do NOT strip the
/// suffix: any trailing non-digit (or non-hex-digit after `0x`) makes the parse
/// fail, exactly like Qt.
fn parse_int_token(text: &str) -> Option<i32> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        i32::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<i32>().ok()
    }
}

/// Parse a TokKind::Number text as i64 (decimal or `0x...`), mirroring C++
/// `numText.toLongLong(&ok)` / `numText.mid(2).toLongLong(&ok, 16)` exactly. As
/// with `parse_int_token`, a trailing integer suffix (kept by the tokenizer)
/// makes the parse fail, matching Qt's reject-on-trailing-garbage behavior.
fn parse_i64_token(text: &str) -> Option<i64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<i64>().ok()
    }
}

// ── Padding field detection (cpp:1053-1062) ──

fn is_padding_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("_pad")
        || lower.starts_with("pad_")
        || lower.starts_with("__pad")
        || lower.starts_with("padding")
        || lower.starts_with("_padding")
        || lower.starts_with("__padding")
        || lower.starts_with("_reserved")
        || lower.starts_with("reserved")
}

// emitHexPadding (cpp:1065-1086)
fn emit_hex_padding(tree: &mut NodeTree, parent_id: u64, offset: i32, size: i32) {
    if size <= 0 {
        return;
    }
    let (hex_kind, hex_size) = largest_hex_cell_for_run(size);
    let count = size / hex_size;
    for i in 0..count {
        let n = Node {
            kind: hex_kind,
            parent_id,
            offset: offset + i * hex_size,
            ..Node::default()
        };
        tree.add_node(n);
    }
}

// emitBitfieldGroup (cpp:1090-1123)
fn emit_bitfield_group(
    tree: &mut NodeTree,
    parent_id: u64,
    offset: i32,
    fields: &[ParsedField],
    start_idx: usize,
    end_idx: usize,
) {
    let mut total_bits = 0i32;
    for f in &fields[start_idx..end_idx] {
        total_bits = total_bits.saturating_add(f.bitfield_width);
    }
    let bytes = total_bits.saturating_add(7) / 8;
    let container_kind = if bytes <= 1 {
        NodeKind::Hex8
    } else if bytes <= 2 {
        NodeKind::Hex16
    } else if bytes <= 4 {
        NodeKind::Hex32
    } else {
        NodeKind::Hex64
    };

    let mut n = Node {
        kind: NodeKind::Struct,
        class_keyword: "bitfield".to_string(),
        element_kind: container_kind,
        parent_id,
        offset,
        collapsed: false,
        ..Node::default()
    };

    let mut bit_offset: u8 = 0;
    for f in &fields[start_idx..end_idx] {
        let bw = f.bitfield_width as u8;
        n.bitfield_members.push(BitfieldMember {
            name: f.name.clone(),
            bit_offset,
            bit_width: bw,
        });
        bit_offset = bit_offset.wrapping_add(bw);
    }

    tree.add_node(n);
}

// ── NodeTree builder: recursive field emitter ──

/// `struct BuildContext` (`import_source.cpp:1127-1137`). Holds everything but
/// the `&mut NodeTree`, which we thread as a separate parameter.
struct BuildContext {
    type_table: HashMap<String, TypeInfo>,
    class_ids: HashMap<String, u64>,
    pending_refs: Vec<PendingRef>,
    use_comment_offsets: bool,
    enum_names: HashSet<String>,
    ptr_size: i32,
    size_asserts: HashMap<String, i32>,
    struct_alignments: HashMap<String, i32>,
}

// unionNaturalAlignment (cpp:1143-1150)
fn union_natural_alignment(tree: &NodeTree, ctx: &BuildContext, field: &ParsedField) -> i32 {
    let mut max_align = 1;
    for member in &field.union_members {
        let a = field_natural_alignment(tree, ctx, member);
        if a > max_align {
            max_align = a;
        }
    }
    max_align
}

// fieldNaturalAlignment (cpp:1153-1166)
fn field_natural_alignment(tree: &NodeTree, ctx: &BuildContext, field: &ParsedField) -> i32 {
    if field.is_pointer {
        return ctx.ptr_size;
    }
    if field.is_union {
        return union_natural_alignment(tree, ctx, field);
    }
    if field.bitfield_width >= 0 {
        if let Some(ti) = ctx.type_table.get(field.type_name.as_str()) {
            return alignment_for(ti.kind);
        }
        return 4; // default bitfield alignment
    }
    if let Some(ti) = ctx.type_table.get(field.type_name.as_str()) {
        return alignment_for(ti.kind);
    }
    // Unknown type (struct reference) — align to pointer size
    ctx.ptr_size
}

// alignUp (cpp:1168-1170)
fn align_up(offset: i32, align: i32) -> i32 {
    (offset + align - 1) & !(align - 1)
}

// structTypeSize (cpp:1173-1189)
fn struct_type_size(tree: &NodeTree, ctx: &BuildContext, type_name: &str) -> i32 {
    if let Some(&id) = ctx.class_ids.get(type_name) {
        let span = tree.struct_span(id);
        if span > 0 {
            let mut span = span;
            if let Some(&align) = ctx.struct_alignments.get(type_name) {
                if align > 1 {
                    span = align_up(span, align);
                }
            }
            return span;
        }
    }
    if let Some(&sz) = ctx.size_asserts.get(type_name) {
        return sz;
    }
    0
}

// clampedArrayElements (cpp:1192-1199)
fn clamped_array_elements(dims: &[i32], max_elements: i64) -> i32 {
    let mut total: i64 = 1;
    for &dim in dims {
        total *= if dim > 0 { dim as i64 } else { 1 };
        if total > max_elements {
            return max_elements as i32;
        }
    }
    total as i32
}

// buildFields (cpp:1201-1495)
fn build_fields(
    tree: &mut NodeTree,
    ctx: &mut BuildContext,
    parent_id: u64,
    base_offset: i32,
    fields: &[ParsedField],
) {
    let mut computed_offset = 0i32;

    let mut fi = 0usize;
    while fi < fields.len() {
        let field = &fields[fi];

        // Bitfield group
        if field.bitfield_width >= 0 {
            let group_offset = if ctx.use_comment_offsets && field.comment_offset >= 0 {
                field.comment_offset - base_offset
            } else {
                let bf_align = field_natural_alignment(tree, ctx, field);
                computed_offset = align_up(computed_offset, bf_align);
                computed_offset
            };
            let start_idx = fi;
            let mut total_bits = 0i32;
            while fi < fields.len() && fields[fi].bitfield_width >= 0 {
                total_bits = total_bits.saturating_add(fields[fi].bitfield_width);
                fi += 1;
            }
            // fi now points past the group; emit covers [start_idx, fi)
            if total_bits > 0 {
                emit_bitfield_group(tree, parent_id, group_offset, fields, start_idx, fi);
            }
            let bytes = total_bits.saturating_add(7) / 8;
            let node_size = if bytes <= 1 {
                1
            } else if bytes <= 2 {
                2
            } else if bytes <= 4 {
                4
            } else {
                8
            };
            computed_offset = group_offset + node_size;
            continue; // fi already advanced
        }

        // Union container field
        if field.is_union {
            let union_offset = if ctx.use_comment_offsets && field.comment_offset >= 0 {
                field.comment_offset - base_offset
            } else {
                let u_align = field_natural_alignment(tree, ctx, field);
                computed_offset = align_up(computed_offset, u_align);
                computed_offset
            };

            let union_node = Node {
                kind: NodeKind::Struct,
                class_keyword: "union".to_string(),
                name: field.name.clone(),
                parent_id,
                offset: union_offset,
                collapsed: true,
                ..Node::default()
            };

            let union_idx = tree.add_node(union_node);
            let union_id = tree.nodes[union_idx].id;

            // Build each union member independently so each starts at offset 0
            let abs_union_offset = base_offset + union_offset;
            for member in &field.union_members {
                let single = std::slice::from_ref(member);
                build_fields(tree, ctx, union_id, abs_union_offset, single);
            }

            let union_span = tree.struct_span(union_id);
            computed_offset = union_offset + if union_span > 0 { union_span } else { 0 };
            fi += 1;
            continue;
        }

        let field_offset = if ctx.use_comment_offsets && field.comment_offset >= 0 {
            field.comment_offset - base_offset
        } else {
            let f_align = field_natural_alignment(tree, ctx, field);
            computed_offset = align_up(computed_offset, f_align);
            computed_offset
        };

        // Resolve type
        let type_info = ctx.type_table.get(field.type_name.as_str()).copied();
        let known_type = type_info.is_some();

        // Pointer field
        if field.is_pointer {
            let ptr_kind = if ctx.ptr_size >= 8 {
                NodeKind::Pointer64
            } else {
                NodeKind::Pointer32
            };

            if !field.array_sizes.is_empty() {
                let total_elements = clamped_array_elements(&field.array_sizes, 1_000_000);
                let n = Node {
                    kind: NodeKind::Array,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    array_len: total_elements,
                    element_kind: ptr_kind,
                    ..Node::default()
                };
                tree.add_node(n);
                computed_offset = field_offset + total_elements * ctx.ptr_size;
                fi += 1;
                continue;
            }

            let n = Node {
                kind: ptr_kind,
                name: field.name.clone(),
                parent_id,
                offset: field_offset,
                collapsed: true,
                ..Node::default()
            };
            let node_idx = tree.add_node(n);
            let node_id = tree.nodes[node_idx].id;

            if !field.pointer_target.is_empty() && field.pointer_target != "void" {
                ctx.pending_refs.push(PendingRef {
                    node_id,
                    class_name: field.pointer_target.clone(),
                });
            }
            computed_offset = field_offset + ctx.ptr_size;
            fi += 1;
            continue;
        }

        // Enum-typed field: emit as UInt32 with refId to enum definition
        if !known_type && ctx.enum_names.contains(&field.type_name) {
            let elem_size = 4;
            let elem_kind = NodeKind::UInt32;
            if !field.array_sizes.is_empty() {
                let total_elements = clamped_array_elements(&field.array_sizes, 1_000_000);
                let n = Node {
                    kind: NodeKind::Array,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    array_len: total_elements,
                    element_kind: elem_kind,
                    ..Node::default()
                };
                tree.add_node(n);
                computed_offset = field_offset + total_elements * elem_size;
            } else {
                let n = Node {
                    kind: elem_kind,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    ..Node::default()
                };
                let node_idx = tree.add_node(n);
                let node_id = tree.nodes[node_idx].id;
                ctx.pending_refs.push(PendingRef {
                    node_id,
                    class_name: field.type_name.clone(),
                });
                computed_offset = field_offset + elem_size;
            }
            fi += 1;
            continue;
        }

        // Determine base type info
        let (base_kind, base_size, is_struct_type) = match type_info {
            Some(ti) => (ti.kind, ti.size, false),
            None => (NodeKind::Hex8, 1, true),
        };

        // Padding fields
        if is_padding_name(&field.name) && !field.array_sizes.is_empty() {
            let mut total_size = base_size;
            for &dim in &field.array_sizes {
                total_size *= if dim > 0 { dim } else { 1 };
            }
            emit_hex_padding(tree, parent_id, field_offset, total_size);
            computed_offset = field_offset + total_size;
            fi += 1;
            continue;
        }

        // Array fields (primitive)
        if !field.array_sizes.is_empty() && !is_struct_type {
            let mut first_dim = *field.array_sizes.first().unwrap_or(&1);
            if first_dim <= 0 {
                first_dim = 1;
            }

            if base_kind == NodeKind::Int8
                && field.array_sizes.len() == 1
                && (field.type_name == "char" || field.type_name == "CHAR")
            {
                let n = Node {
                    kind: NodeKind::UTF8,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    str_len: first_dim,
                    ..Node::default()
                };
                tree.add_node(n);
                computed_offset = field_offset + first_dim;
                fi += 1;
                continue;
            }

            if base_kind == NodeKind::UInt16
                && field.array_sizes.len() == 1
                && (field.type_name == "wchar_t"
                    || field.type_name == "WCHAR"
                    || field.type_name == "TCHAR")
            {
                let n = Node {
                    kind: NodeKind::UTF16,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    str_len: first_dim,
                    ..Node::default()
                };
                tree.add_node(n);
                computed_offset = field_offset + first_dim * 2;
                fi += 1;
                continue;
            }

            if base_kind == NodeKind::Float && field.array_sizes.len() == 1 {
                if first_dim == 2 {
                    add_simple_vec(tree, parent_id, field_offset, NodeKind::Vec2, &field.name);
                    computed_offset = field_offset + 8;
                    fi += 1;
                    continue;
                }
                if first_dim == 3 {
                    add_simple_vec(tree, parent_id, field_offset, NodeKind::Vec3, &field.name);
                    computed_offset = field_offset + 12;
                    fi += 1;
                    continue;
                }
                if first_dim == 4 {
                    add_simple_vec(tree, parent_id, field_offset, NodeKind::Vec4, &field.name);
                    computed_offset = field_offset + 16;
                    fi += 1;
                    continue;
                }
            }

            if base_kind == NodeKind::Float
                && field.array_sizes.len() == 2
                && field.array_sizes[0] == 4
                && field.array_sizes[1] == 4
            {
                add_simple_vec(tree, parent_id, field_offset, NodeKind::Mat4x4, &field.name);
                computed_offset = field_offset + 64;
                fi += 1;
                continue;
            }

            let total_elements = clamped_array_elements(&field.array_sizes, 1_000_000);
            let n = Node {
                kind: NodeKind::Array,
                name: field.name.clone(),
                parent_id,
                offset: field_offset,
                array_len: total_elements,
                element_kind: base_kind,
                ..Node::default()
            };
            tree.add_node(n);
            computed_offset = field_offset + total_elements * base_size;
            fi += 1;
            continue;
        }

        // Struct-type field
        if is_struct_type {
            let elem_size = struct_type_size(tree, ctx, &field.type_name);

            if !field.array_sizes.is_empty() {
                let total_elements = clamped_array_elements(&field.array_sizes, 1_000_000);
                let n = Node {
                    kind: NodeKind::Array,
                    name: field.name.clone(),
                    parent_id,
                    offset: field_offset,
                    array_len: total_elements,
                    element_kind: NodeKind::Struct,
                    struct_type_name: field.type_name.clone(),
                    collapsed: true,
                    ..Node::default()
                };
                let node_idx = tree.add_node(n);
                let node_id = tree.nodes[node_idx].id;
                ctx.pending_refs.push(PendingRef {
                    node_id,
                    class_name: field.type_name.clone(),
                });
                if elem_size > 0 {
                    computed_offset = field_offset + total_elements * elem_size;
                }
                fi += 1;
                continue;
            }

            let n = Node {
                kind: NodeKind::Struct,
                name: field.name.clone(),
                parent_id,
                offset: field_offset,
                struct_type_name: field.type_name.clone(),
                collapsed: true,
                ..Node::default()
            };
            let node_idx = tree.add_node(n);
            let node_id = tree.nodes[node_idx].id;
            ctx.pending_refs.push(PendingRef {
                node_id,
                class_name: field.type_name.clone(),
            });
            if elem_size > 0 {
                computed_offset = field_offset + elem_size;
            }
            fi += 1;
            continue;
        }

        // Simple primitive field
        let n = Node {
            kind: base_kind,
            name: field.name.clone(),
            parent_id,
            offset: field_offset,
            ..Node::default()
        };
        tree.add_node(n);
        computed_offset = field_offset + base_size;
        fi += 1;
    }
}

fn add_simple_vec(tree: &mut NodeTree, parent_id: u64, offset: i32, kind: NodeKind, name: &str) {
    let n = Node {
        kind,
        name: name.to_string(),
        parent_id,
        offset,
        ..Node::default()
    };
    tree.add_node(n);
}

// hasAnyCommentOffset (cpp:1499-1505)
fn has_any_comment_offset(fields: &[ParsedField]) -> bool {
    for f in fields {
        if f.comment_offset >= 0 {
            return true;
        }
        if f.is_union && has_any_comment_offset(&f.union_members) {
            return true;
        }
    }
    false
}

// ── Top-level importFromSource (cpp:1509-1622) ──

pub fn import_from_source(source: &str, pointer_size: i32) -> Result<NodeTree, ImportError> {
    if source.trim().is_empty() {
        return Err(ImportError::EmptySource);
    }

    // Tokenize
    let (tokens, line_offsets) = tokenize(source);

    // Parse
    let mut parser = Parser::new(&tokens, &line_offsets);
    parser.parse();

    if parser.structs.is_empty() {
        return Err(ImportError::NoDefinitions);
    }

    // Build type table
    let mut type_table = build_type_table(pointer_size);

    // Register typedefs into type table (cpp:1532-1536)
    for (alias, real) in &parser.typedefs {
        if let Some(&ti) = type_table.get(real.as_str()) {
            type_table.insert(alias.clone(), ti);
        }
    }

    let mut tree = NodeTree::default();
    tree.base_address = 0x0040_0000;
    tree.pointer_size = pointer_size;

    // Determine offset mode (cpp:1546-1549)
    let mut use_comment_offsets = false;
    for ps in &parser.structs {
        if has_any_comment_offset(&ps.fields) {
            use_comment_offsets = true;
            break;
        }
    }

    // Collect enum type names (cpp:1552-1556)
    let mut enum_names: HashSet<String> = HashSet::new();
    for ps in &parser.structs {
        if ps.keyword == "enum" && !ps.name.is_empty() {
            enum_names.insert(ps.name.clone());
        }
    }

    let mut ctx = BuildContext {
        type_table,
        class_ids: HashMap::new(),
        pending_refs: Vec::new(),
        use_comment_offsets,
        enum_names,
        ptr_size: pointer_size,
        size_asserts: parser.size_asserts.clone(),
        struct_alignments: parser.struct_alignments.clone(),
    };

    // Build nodes for each struct/enum (cpp:1561-1603)
    // We clone the parsed structs into a local Vec so we can borrow `tree`
    // mutably while iterating.
    let structs = std::mem::take(&mut parser.structs);
    for ps in &structs {
        let mut struct_node = Node {
            kind: NodeKind::Struct,
            name: ps.name.clone(),
            struct_type_name: ps.name.clone(),
            class_keyword: ps.keyword.clone(),
            parent_id: 0,
            offset: 0,
            collapsed: true,
            ..Node::default()
        };

        if ps.keyword == "enum" {
            struct_node.enum_members = ps.enum_values.clone();
            let idx = tree.add_node(struct_node);
            let node_id = tree.nodes[idx].id;
            if !ps.name.is_empty() {
                ctx.class_ids.insert(ps.name.clone(), node_id);
            }
            continue;
        }

        let struct_idx = tree.add_node(struct_node);
        let struct_id = tree.nodes[struct_idx].id;
        ctx.class_ids.insert(ps.name.clone(), struct_id);

        build_fields(&mut tree, &mut ctx, struct_id, 0, &ps.fields);

        // Union: all direct children overlap at offset 0 (cpp:1588-1592)
        if ps.keyword == "union" {
            let children = tree.children_of(struct_id);
            for ci in children {
                tree.nodes[ci].offset = 0;
            }
        }

        // static_assert tail padding (cpp:1595-1602)
        if let Some(&declared_size) = parser.size_asserts.get(&ps.name) {
            let current_span = tree.struct_span(struct_id);
            if declared_size > current_span {
                emit_hex_padding(
                    &mut tree,
                    struct_id,
                    current_span,
                    declared_size - current_span,
                );
            }
        }
    }

    if tree.nodes.is_empty() {
        return Err(ImportError::NoNodes);
    }

    // Resolve deferred pointer/struct references (cpp:1610-1619)
    let pending = std::mem::take(&mut ctx.pending_refs);
    resolve_pending_refs(&mut tree, &pending, &ctx.class_ids);

    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Import with 8-byte pointers and return the (single) root struct's
    /// direct children as `(kind, offset)` pairs, sorted by tree order.
    fn fields_of(src: &str) -> Vec<(NodeKind, i32)> {
        let tree = import_from_source(src, 8).expect("import should succeed");
        // The first node is the root struct.
        let root_id = tree.nodes[0].id;
        tree.children_of(root_id)
            .into_iter()
            .map(|i| (tree.nodes[i].kind, tree.nodes[i].offset))
            .collect()
    }

    // ── Item 1: MSVC __int8/__int16/__int32/__int64 (+ unsigned) ──

    #[test]
    fn msvc_fixed_int_types_resolve() {
        // Each MSVC fixed-width type must resolve to the matching primitive
        // *and* advance the computed-offset cursor by its true size so the
        // following field lands at the correct offset.
        let src = "struct S {\n  __int8 a;\n  __int16 b;\n  __int32 c;\n  __int64 d;\n};";
        let fields = fields_of(src);
        assert_eq!(
            fields,
            vec![
                (NodeKind::Int8, 0),
                (NodeKind::Int16, 2), // aligned to 2
                (NodeKind::Int32, 4), // aligned to 4
                (NodeKind::Int64, 8), // aligned to 8
            ],
            "MSVC __intN types must resolve and advance the offset cursor"
        );
    }

    #[test]
    fn unsigned_msvc_int_matches_cpp_recovery() {
        // The `unsigned __intN` table entries exist verbatim (parity with the
        // C++ table, lines 37-40), but `parseTypeName` only merges further
        // `int`/`char`/`long`/modifier words — NOT `__intN`. So `unsigned __int8`
        // does NOT resolve to UInt8: the field parse fails on the leftover token,
        // the struct-body loop skips one token (`unsigned`) and re-parses the
        // bare `__intN`. The net effect (identical in C++) is that the `unsigned`
        // is dropped and the field resolves to the SIGNED `__intN` type. This
        // test pins that C++-faithful behavior, not an idealized one.
        let src = "struct S {\n  unsigned __int8 a;\n  unsigned __int16 b;\n  unsigned __int32 c;\n  unsigned __int64 d;\n};";
        let fields = fields_of(src);
        assert_eq!(
            fields,
            vec![
                (NodeKind::Int8, 0),
                (NodeKind::Int16, 2),
                (NodeKind::Int32, 4),
                (NodeKind::Int64, 8),
            ],
            "unsigned __intN drops the modifier during recovery, matching C++"
        );

        // The merged table key itself IS present (verbatim with the C++ table),
        // even though the parser never produces the merged string.
        let table = build_type_table(8);
        assert_eq!(
            table.get("unsigned __int8").map(|t| t.kind),
            Some(NodeKind::UInt8)
        );
        assert_eq!(
            table.get("unsigned __int64").map(|t| t.kind),
            Some(NodeKind::UInt64)
        );
    }

    #[test]
    fn msvc_int64_does_not_stall_following_fields() {
        // Regression for the audited bug: __int64 used to resolve to an unknown
        // struct-type reference (size 0), stalling the cursor so the trailing
        // field landed at offset 0 instead of 8.
        let src = "struct S {\n  __int64 first;\n  int second;\n};";
        let fields = fields_of(src);
        assert_eq!(fields[0], (NodeKind::Int64, 0));
        assert_eq!(fields[1], (NodeKind::Int32, 8));
    }

    // ── Item 2: IDA/Hex-Rays _BYTE/_WORD/_DWORD/_QWORD ──

    #[test]
    fn ida_hexrays_types_resolve() {
        let src = "struct S {\n  _BYTE a;\n  _WORD b;\n  _DWORD c;\n  _QWORD d;\n};";
        let fields = fields_of(src);
        assert_eq!(
            fields,
            vec![
                (NodeKind::UInt8, 0),
                (NodeKind::UInt16, 2),
                (NodeKind::UInt32, 4),
                (NodeKind::UInt64, 8),
            ]
        );
    }

    #[test]
    fn ida_dword_does_not_stall_following_fields() {
        let src = "struct S {\n  _DWORD a;\n  int b;\n};";
        let fields = fields_of(src);
        assert_eq!(fields[0], (NodeKind::UInt32, 0));
        assert_eq!(fields[1], (NodeKind::Int32, 4));
    }

    // ── Item 7: integer-suffix in array dimensions / static_assert ──

    #[test]
    fn parse_int_token_rejects_integer_suffix() {
        // Matches Qt `toInt(&ok)` which returns ok=false on trailing garbage.
        assert_eq!(parse_int_token("16"), Some(16));
        assert_eq!(parse_int_token("0x10"), Some(16));
        assert_eq!(parse_int_token("16u"), None);
        assert_eq!(parse_int_token("16U"), None);
        assert_eq!(parse_int_token("16ull"), None);
        assert_eq!(parse_int_token("0x10u"), None);
        assert_eq!(parse_i64_token("16"), Some(16));
        assert_eq!(parse_i64_token("16u"), None);
    }

    #[test]
    fn suffixed_array_dimension_is_skipped() {
        // `int a[16u]` — Qt's toInt(&ok) rejects "16u" (ok=false), and the
        // `if (ok)` guard means the dimension is NOT appended. With `arraySizes`
        // left empty the field collapses to a plain scalar `int`, exactly as in
        // the C++. (Previously the Rust port stripped the suffix and produced a
        // 16-element array — the bug this pins.)
        let suffixed = import_from_source("struct S {\n  int a[16u];\n};", 8).unwrap();
        let kids = suffixed.children_of(suffixed.nodes[0].id);
        assert_eq!(kids.len(), 1);
        assert_eq!(
            suffixed.nodes[kids[0]].kind,
            NodeKind::Int32,
            "suffixed dimension 16u is rejected, so the field is a scalar int"
        );

        // A plain `int a[16]` must still produce a 16-element array.
        let sized = import_from_source("struct S {\n  int a[16];\n};", 8).unwrap();
        let sa = sized.children_of(sized.nodes[0].id);
        assert_eq!(sized.nodes[sa[0]].kind, NodeKind::Array);
        assert_eq!(sized.nodes[sa[0]].array_len, 16);
    }

    #[test]
    fn adversarial_bitfield_widths_do_not_overflow() {
        // Regression: summing attacker-controlled bitfield widths used to be a
        // plain `total_bits += ...` over an i32, so widths near i32::MAX would
        // overflow (debug panic) while accumulating the group and again in the
        // `total_bits + 7` byte computation. The parse-time clamp(0,64) plus
        // saturating accumulation make import total, never panicking. A bitfield
        // wider than 64 bits is invalid C, so clamping only touches pathological
        // input. This input must import cleanly rather than abort the process.
        let src = format!(
            "struct S {{\n  int a : {max};\n  int b : {max};\n  int c : {max};\n}};",
            max = i32::MAX
        );
        let tree = import_from_source(&src, 8).expect("adversarial widths must import, not panic");
        let kids = tree.children_of(tree.nodes[0].id);
        // The three over-wide fields collapse into one clamped bitfield group.
        assert_eq!(kids.len(), 1);
        assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Struct);
        assert_eq!(tree.nodes[kids[0]].class_keyword, "bitfield");
    }

    #[test]
    fn normal_bitfield_group_still_emits_one_bitfield_node() {
        // Pin happy-path parity: ordinary in-range widths are unaffected by the
        // clamp and still coalesce into a single "bitfield" struct container.
        let tree = import_from_source(
            "struct S {\n  unsigned int a : 1;\n  unsigned int b : 2;\n  unsigned int c : 3;\n};",
            8,
        )
        .unwrap();
        let kids = tree.children_of(tree.nodes[0].id);
        assert_eq!(kids.len(), 1);
        assert_eq!(tree.nodes[kids[0]].kind, NodeKind::Struct);
        assert_eq!(tree.nodes[kids[0]].class_keyword, "bitfield");
        // 1+2+3 = 6 bits -> 1 byte -> Hex8 container.
        assert_eq!(tree.nodes[kids[0]].element_kind, NodeKind::Hex8);
        assert_eq!(tree.nodes[kids[0]].bitfield_members.len(), 3);
    }
}
