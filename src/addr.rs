//! Address-expression evaluator (`<mod>+0x10`, deref chains, symbols).
//!
//! Port of `src/addressparser.{h,cpp}`. **SKELETON** — the recursive-descent
//! expression evaluator is filled in by the dedicated `addressparser` workflow
//! (ARCHITECTURE.md §9). The public API + callback shape are in place.

/// `struct AddressParseResult` (`addressparser.h:8-13`).
#[derive(Clone, Debug, Default)]
pub struct AddressParseResult {
    pub ok: bool,
    pub value: u64,
    pub error: String,
    pub error_pos: i32,
}

/// `struct AddressParserCallbacks` (`addressparser.h:15-24`) — pluggable hooks
/// the evaluator calls for module/symbol resolution and pointer deref. Each
/// returns `(value, ok)`; kernel-paging hooks are optional (wired only for the
/// out-of-scope kernel provider).
#[derive(Default)]
pub struct AddressParserCallbacks<'a> {
    pub resolve_module: Option<Box<dyn Fn(&str) -> (u64, bool) + 'a>>,
    pub read_pointer: Option<Box<dyn Fn(u64) -> (u64, bool) + 'a>>,
    pub resolve_identifier: Option<Box<dyn Fn(&str) -> (u64, bool) + 'a>>,
    pub vtop: Option<Box<dyn Fn(u32, u64) -> (u64, bool) + 'a>>,
    pub cr3: Option<Box<dyn Fn(u32) -> (u64, bool) + 'a>>,
    pub phys_read: Option<Box<dyn Fn(u64) -> (u64, bool) + 'a>>,
}

/// `class AddressParser` (`addressparser.h:26-31`).
pub struct AddressParser;

impl AddressParser {
    /// `AddressParser::evaluate(formula, ptrSize, cb)` (`addressparser.h:28`).
    /// SKELETON.
    pub fn evaluate(
        _formula: &str,
        _ptr_size: i32,
        _cb: Option<&AddressParserCallbacks<'_>>,
    ) -> AddressParseResult {
        todo!("port addressparser.cpp evaluate (workflow: addressparser)")
    }

    /// `AddressParser::validate(formula)` (`addressparser.h:29`) — empty string
    /// on success, else an error message. SKELETON.
    pub fn validate(_formula: &str) -> String {
        todo!("port addressparser.cpp validate (workflow: addressparser)")
    }
}
