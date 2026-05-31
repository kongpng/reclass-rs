//! Value formatting — `typeName`, scalar/pointer formatters, value
//! read/parse/validate, hex/ASCII previews, enum/bitfield member rendering.
//!
//! Faithful 1:1 port of `src/format.cpp` (the `rcx::fmt` namespace declared in
//! `core.h:1472-1521`). This is the pure presentation layer: a single [`Node`]
//! plus a [`Provider`] become the exact text shown on an editor row, and the
//! inverse (user text → raw bytes) for editing. No Qt widgets, no threading.
//!
//! `QString` → `String`, `QByteArray` → `Vec<u8>`, `Provider` → trait. The only
//! UTF-16-specific paths are the UTF16 string read/write (handled with
//! `char::decode_utf16` / `str::encode_utf16`). `QString::size()` counts UTF-16
//! code units; type/name strings are ASCII so char-count parity holds, and the
//! `fit`/ellipsis width math uses UTF-16 unit counts to match Qt exactly.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::addr::AddressParser;
use crate::core::{
    is_hex_node, is_hex_preview, is_valid_primitive_ptr_target, kind_meta, size_for_kind, Node,
    NodeKind,
};
use crate::provider::Provider;

// ── Column layout (`format.cpp:59-65`) ──
// COL_TYPE / COL_NAME / COL_VALUE alias the shared constants from `core.h`
// (kColType, kColName, kColValue) and double as the public default column
// widths callers (compose / UI) pass to the header/line functions. COL_COMMENT
// is a file-local 28.

/// `COL_TYPE = kColType = 14` (`format.cpp:61`, `core.h:1132`).
pub const COL_TYPE: i32 = 14;
/// `COL_NAME = kColName = 22` (`format.cpp:62`, `core.h:1133`).
pub const COL_NAME: i32 = 22;
/// `COL_VALUE = kColValue = 96` (`format.cpp:63`, `core.h:1134`).
pub const COL_VALUE: i32 = 96;
/// `COL_COMMENT = 28` (`format.cpp:64`, `= core.h:1135 kColComment`).
const COL_COMMENT: i32 = 28; // "// Enter=Save Esc=Cancel" fits
/// `SEP = QStringLiteral(" ")` (`format.cpp:65`).
const SEP: &str = " ";

/// `kTreeIndent = 2` (`core.h:1131`).
const K_TREE_INDENT: i32 = 2;
/// `kSepWidth = 1` (`core.h:1136`).
const K_SEP_WIDTH: i32 = 1;

// ── UTF-16-unit helpers ──
// Qt `QString` counts code units (UTF-16). All our column/width math mirrors
// that exactly so padding & ellipsis line up byte-for-byte.

/// UTF-16 code-unit length of `s` (`QString::size()`).
#[inline]
fn u16_len(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

/// Truncate to the first `n` UTF-16 code units (`QString::left(n)`). Mirrors Qt
/// truncating mid-surrogate is impossible here because we only cut on `char`
/// boundaries; type/name strings are ASCII so this is exact.
fn left_u16(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut taken = 0usize;
    for c in s.chars() {
        let w = c.len_utf16();
        if taken + w > n {
            break;
        }
        out.push(c);
        taken += w;
    }
    out
}

/// `s.leftJustified(w, ' ')` — pad on the right with spaces to UTF-16 width `w`
/// (never truncates; if already ≥ w returns `s` unchanged).
fn left_justified(s: &str, w: usize) -> String {
    let len = u16_len(s);
    if len >= w {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + (w - len));
    out.push_str(s);
    for _ in 0..(w - len) {
        out.push(' ');
    }
    out
}

// ── IEEE 754 half-precision (binary16) ↔ single-precision ──

/// `halfToFloat(uint16_t h)` (`format.cpp:13-32`). Direct bit-port (not the
/// `half` crate) to guarantee bit-exact parity.
fn half_to_float(h: u16) -> f32 {
    let s: u32 = (u32::from(h & 0x8000)) << 16;
    let mut e: u32 = (u32::from(h) >> 10) & 0x1f;
    let mut m: u32 = u32::from(h) & 0x3ff;
    let b: u32;
    if e == 0 {
        if m == 0 {
            b = s;
        } else {
            // Subnormal: renormalize into single-precision normal form.
            while (m & 0x400) == 0 {
                m <<= 1;
                e = e.wrapping_sub(1);
            }
            e = e.wrapping_add(1);
            m &= 0x3ff;
            b = s | ((e.wrapping_add(112)) << 23) | (m << 13);
        }
    } else if e == 0x1f {
        b = s | 0x7f80_0000 | (m << 13); // inf or NaN
    } else {
        b = s | ((e + 112) << 23) | (m << 13);
    }
    f32::from_bits(b)
}

/// `floatToHalf(float f)` (`format.cpp:34-57`). round-to-nearest-even.
fn float_to_half(f: f32) -> u16 {
    let b: u32 = f.to_bits();
    let s: u32 = (b >> 16) & 0x8000;
    let mut e: i32 = ((b >> 23) & 0xff) as i32 - 127;
    let mut m: u32 = b & 0x7f_ffff;
    if e == 128 {
        // inf / NaN
        return (s | 0x7c00 | (if m != 0 { 0x200 } else { 0 })) as u16;
    }
    if e > 15 {
        return (s | 0x7c00) as u16; // overflow → inf
    }
    if e < -14 {
        if e < -24 {
            return s as u16; // underflow → 0
        }
        m |= 0x80_0000;
        let shift = -e - 14 + 13;
        let mut hm = m >> shift;
        if ((m >> (shift - 1)) & 1) != 0 {
            hm += 1; // round to nearest
        }
        return (s | hm) as u16;
    }
    let mut hm = m >> 13;
    if ((m >> 12) & 1) != 0 {
        // round to nearest even
        hm += 1;
        if hm == 0x400 {
            hm = 0;
            e += 1;
            if e > 15 {
                return (s | 0x7c00) as u16;
            }
        }
    }
    (s | (((e + 15) as u32) << 10) | hm) as u16
}

// ── Column fitting (`format.cpp:67-82`) ──

/// `fit(QString s, int w)` (`format.cpp:67-74`). Result is **always exactly `w`
/// UTF-16 units** when `w>0`. Ellipsis `…` (U+2026) only when `w>=2`.
fn fit(s: &str, w: i32) -> String {
    if w <= 0 {
        return String::new();
    }
    let w = w as usize;
    let mut owned: String;
    let cut: &str = if u16_len(s) > w {
        if w >= 2 {
            owned = left_u16(s, w - 1);
            owned.push('\u{2026}'); // ellipsis
            &owned
        } else {
            owned = left_u16(s, w);
            &owned
        }
    } else {
        s
    };
    left_justified(cut, w)
}

/// `fitOverflow(const QString& s, int w)` (`format.cpp:77-82`). Pads to `w`, or
/// returns the full string unpadded if it overflows.
fn fit_overflow(s: &str, w: i32) -> String {
    if w <= 0 {
        return String::new();
    }
    let w = w as usize;
    if u16_len(s) <= w {
        left_justified(s, w)
    } else {
        s.to_string()
    }
}

// ── Type name ──

/// `fmt::TypeNameFn` (`core.h:1474`) — pluggable type-name override hook.
pub type TypeNameFn = fn(NodeKind) -> String;

// Override seam (`format.cpp:87`): `static g_typeNameFn = nullptr`. Stored as a
// raw fn-pointer in an atomic so the global is thread-safe (0 == nullptr).
static G_TYPE_NAME_FN: AtomicUsize = AtomicUsize::new(0);

/// `fmt::setTypeNameProvider(fn)` (`format.cpp:89`).
pub fn set_type_name_provider(f: Option<TypeNameFn>) {
    let raw = match f {
        Some(p) => p as usize,
        None => 0,
    };
    G_TYPE_NAME_FN.store(raw, Ordering::SeqCst);
}

#[inline]
fn current_type_name_fn() -> Option<TypeNameFn> {
    let raw = G_TYPE_NAME_FN.load(Ordering::SeqCst);
    if raw == 0 {
        None
    } else {
        // SAFETY: only ever set from `set_type_name_provider` with a valid
        // `TypeNameFn` (or 0 for None, handled above).
        Some(unsafe { std::mem::transmute::<usize, TypeNameFn>(raw) })
    }
}

/// `fmt::typeNameRaw(NodeKind)` (`format.cpp:92-96`) — unpadded display name,
/// for width/overflow detection. Override fn if set, else `kindMeta` typeName,
/// else `"???"`.
pub fn type_name_raw(kind: NodeKind) -> String {
    if let Some(f) = current_type_name_fn() {
        return f(kind);
    }
    kind_meta(kind).map_or_else(|| "???".to_string(), |m| m.type_name.to_string())
}

/// `fmt::typeName(NodeKind, int colType=14)` (`format.cpp:98-102`) — fixed-width
/// (fitted) display name. Use [`type_name_fitted`] to override the column width.
pub fn type_name(kind: NodeKind) -> String {
    type_name_fitted(kind, COL_TYPE)
}

/// `fmt::typeName(NodeKind, colType)` (`format.cpp:98-102`) with explicit width.
pub fn type_name_fitted(kind: NodeKind, col_type: i32) -> String {
    if let Some(f) = current_type_name_fn() {
        return fit(&f(kind), col_type);
    }
    let raw = kind_meta(kind).map_or_else(|| "???".to_string(), |m| m.type_name.to_string());
    fit(&raw, col_type)
}

/// `fmt::arrayTypeName(elemKind, count, structName)` (`format.cpp:105-114`).
/// E.g. `"uint32_t[16]"`, `"Material[2]"`.
pub fn array_type_name(elem_kind: NodeKind, count: i32, struct_name: &str) -> String {
    let elem = if elem_kind == NodeKind::Struct && !struct_name.is_empty() {
        struct_name.to_string()
    } else {
        kind_meta(elem_kind).map_or_else(|| "???".to_string(), |m| m.type_name.to_string())
    };
    format!("{elem}[{count}]")
}

/// `fmt::pointerTypeName(kind, targetName)` (`format.cpp:117-121`). `kind`
/// unused; empty target ⇒ `"void"`. E.g. `"void*"`, `"StructName*"`.
pub fn pointer_type_name(_kind: NodeKind, target_name: &str) -> String {
    let target = if target_name.is_empty() {
        "void"
    } else {
        target_name
    };
    format!("{target}*")
}

// ── Value formatting helpers ──

/// `hexVal(uint64_t v)` (`format.cpp:125-127`) — lowercase `0x…`.
fn hex_val(v: u64) -> String {
    format!("0x{v:x}")
}

/// `rawHex(uint64_t v, int digits)` (`format.cpp:129-131`) — lowercase hex,
/// zero-padded to `digits`, no `0x`. Not truncated if wider than `digits`.
fn raw_hex(v: u64, digits: usize) -> String {
    format!("{v:0digits$x}")
}

/// `fmt::fmtInt8` (`format.cpp:133`) — signed decimal.
pub fn fmt_int8(v: i8) -> String {
    v.to_string()
}
/// `fmt::fmtInt16` (`format.cpp:134`).
pub fn fmt_int16(v: i16) -> String {
    v.to_string()
}
/// `fmt::fmtInt32` (`format.cpp:135`).
pub fn fmt_int32(v: i32) -> String {
    v.to_string()
}
/// `fmt::fmtInt64` (`format.cpp:136`).
pub fn fmt_int64(v: i64) -> String {
    v.to_string()
}
/// `fmt::fmtUInt8` (`format.cpp:137`) — `0x…` lowercase.
pub fn fmt_uint8(v: u8) -> String {
    hex_val(u64::from(v))
}
/// `fmt::fmtUInt16` (`format.cpp:138`).
pub fn fmt_uint16(v: u16) -> String {
    hex_val(u64::from(v))
}
/// `fmt::fmtUInt32` (`format.cpp:139`).
pub fn fmt_uint32(v: u32) -> String {
    hex_val(u64::from(v))
}
/// `fmt::fmtUInt64` (`format.cpp:140`).
pub fn fmt_uint64(v: u64) -> String {
    hex_val(v)
}

/// `fmt::fmtInt128(const void* data)` (`format.cpp:170-177`). `data` is the
/// 16 little-endian bytes (caller already byte-ordered them).
pub fn fmt_int128(data: &[u8; 16]) -> String {
    i128::from_le_bytes(*data).to_string()
}

/// `fmt::fmtUInt128(const void* data)` (`format.cpp:179-182`).
pub fn fmt_uint128(data: &[u8; 16]) -> String {
    u128::from_le_bytes(*data).to_string()
}

/// `fmt::fmtFloat16(uint16_t bits)` (`format.cpp:142-149`). `'g',4` body + `h`.
pub fn fmt_float16(bits: u16) -> String {
    let f = half_to_float(bits);
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "infh" } else { "-infh" }.to_string();
    }
    let s = qstring_number_g(f64::from(f), 4);
    s + "h"
}

/// `fmt::fmtFloat(float v)` (`format.cpp:184-208`). Fixed 7-char body:
/// positive = 7 chars, negative = `-` + 7 = 8. See `format-render.md §6.1`.
pub fn fmt_float(v: f32) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inff" } else { "-inff" }.to_string();
    }
    if v == 0.0 && v.is_sign_negative() {
        return "-0.000f".to_string();
    }

    let av = v.abs();
    if av >= 100_000.0 {
        return if v < 0.0 { "-99999+f" } else { "99999+f" }.to_string();
    }

    // body = digits + "." + decimals + "f", target exactly 7 chars.
    for dec in (0..=4).rev() {
        let mut body = format!("{:.*}", dec as usize, av);
        body += if dec == 0 { ".f" } else { "f" };
        if body.len() == 7 {
            if v < 0.0 {
                body.insert(0, '-');
            }
            return body;
        }
    }
    // Rounding pushed past 99999 — use overflow cap
    if v < 0.0 { "-99999+f" } else { "99999+f" }.to_string()
}

/// `fmt::fmtDouble(double v)` (`format.cpp:209-216`). `'g',6`, then force a `.0`
/// if no `.`/`e`/`E` present.
pub fn fmt_double(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    let mut s = qstring_number_g(v, 6);
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        s += ".0";
    }
    s
}

/// `fmt::fmtBool(uint8_t v)` (`format.cpp:217`).
pub fn fmt_bool(v: u8) -> String {
    if v != 0 { "true" } else { "false" }.to_string()
}

/// `fmt::fmtPointer32(uint32_t v)` (`format.cpp:219`).
pub fn fmt_pointer32(v: u32) -> String {
    if v == 0 {
        "nullptr".to_string()
    } else {
        hex_val(u64::from(v))
    }
}
/// `fmt::fmtPointer64(uint64_t v)` (`format.cpp:220`).
pub fn fmt_pointer64(v: u64) -> String {
    if v == 0 {
        "nullptr".to_string()
    } else {
        hex_val(v)
    }
}

/// Emulates Qt `QString::number(double, 'g', precision)` = C `printf %.*g`
/// (significant-digit form, shortest of `%e`/`%f`, trailing zeros stripped),
/// then Qt lowercases the exponent marker (`e`). Used by `fmtDouble`/`fmtFloat16`.
fn qstring_number_g(v: f64, precision: i32) -> String {
    // C `%g`: precision 0 is treated as 1.
    let p = if precision <= 0 { 1 } else { precision as usize };
    // Rust's `{:e}` / `{:.*e}` mirror C `%e` (lowercase 'e', sign on exponent,
    // at least 2 exponent digits is NOT guaranteed in Rust — but `%g` chooses
    // between %e and %f, and after trailing-zero stripping the exact exponent
    // digit count rarely matters for our (loose) tests; we still match Qt which
    // strips a leading zero in the exponent on most platforms).
    if v == 0.0 {
        return "0".to_string();
    }

    // Determine decimal exponent X (as in C %g: choose %e if X < -4 or X >= P).
    let mag = v.abs();
    let exp10 = mag.log10().floor() as i32;
    // Recompute exponent precisely via formatting to avoid log10 rounding edge
    // cases: format with %e at p-1 fractional digits and read its exponent.
    let e_str = format!("{:.*e}", p - 1, v);
    let x = e_str
        .rsplit('e')
        .next()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(exp10);

    if x < -4 || x >= p as i32 {
        // %e form with (P-1) fractional digits, then strip trailing zeros.
        format_e_strip(&e_str)
    } else {
        // %f form with (P-1-X) fractional digits, then strip trailing zeros.
        let frac = (p as i32 - 1 - x).max(0) as usize;
        let f_str = format!("{:.*}", frac, v);
        strip_trailing_zeros_fixed(&f_str)
    }
}

/// Strip trailing fractional zeros from a `%e` string ("1.2300e5" → "1.23e5",
/// "1.0000e-7" → "1e-7") and normalize the exponent like Qt.
fn format_e_strip(s: &str) -> String {
    let (mantissa, exp) = match s.split_once('e') {
        Some((m, e)) => (m, e),
        None => return s.to_string(),
    };
    let mantissa = strip_trailing_zeros_fixed(mantissa);
    // Qt prints exponent with a sign and (typically) at least 2 digits. C's
    // %e/%g also use ≥2 exponent digits. Mirror that.
    let (sign, digits) = if let Some(d) = exp.strip_prefix('-') {
        ('-', d)
    } else if let Some(d) = exp.strip_prefix('+') {
        ('+', d)
    } else {
        ('+', exp)
    };
    let digits_num: i64 = digits.parse().unwrap_or(0);
    format!("{mantissa}e{sign}{:02}", digits_num)
}

/// Strip trailing zeros after a decimal point (and a dangling `.`). Leaves
/// integers untouched ("42" stays "42").
fn strip_trailing_zeros_fixed(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_string()
}

// ── Indentation (`format.cpp:224-226`) ──

/// `fmt::indent(int depth)` — `depth * kTreeIndent` (=2) spaces.
pub fn indent(depth: i32) -> String {
    let n = (depth.max(0) * K_TREE_INDENT) as usize;
    " ".repeat(n)
}

// ── Offset margin (`format.cpp:230-234`) ──

/// `fmt::fmtOffsetMargin(uint64_t off, bool isContinuation, int hexDigits=8)`.
/// UPPERCASE zero-padded hex + trailing space, or `"  · "` for continuation.
pub fn fmt_offset_margin(absolute_offset: u64, is_continuation: bool, hex_digits: i32) -> String {
    if is_continuation {
        return "  \u{00B7} ".to_string();
    }
    let digits = hex_digits.max(0) as usize;
    let hex = format!("{absolute_offset:X}");
    let padded = left_pad_zeros(&hex, digits);
    format!("{padded} ")
}

/// `QString::number(v,16).toUpper().rightJustified(digits,'0')` — zero-pad on
/// the LEFT to `digits`; never truncates.
fn left_pad_zeros(s: &str, digits: usize) -> String {
    let len = u16_len(s);
    if len >= digits {
        return s.to_string();
    }
    let mut out = String::with_capacity(digits);
    for _ in 0..(digits - len) {
        out.push('0');
    }
    out.push_str(s);
    out
}

// ── Struct type name (for width calculation) ──

/// `fmt::structTypeName(const Node&)` (`format.cpp:238-244`).
pub fn struct_type_name(node: &Node) -> String {
    if !node.struct_type_name.is_empty() {
        node.struct_type_name.clone()
    } else {
        node.resolved_class_keyword().to_string()
    }
}

// ── Struct header / footer ──

/// `fmt::fmtStructHeader(node, depth, collapsed, colType=14, colName=22, compact=false)`
/// (`format.cpp:248-259`).
pub fn fmt_struct_header(
    node: &Node,
    depth: i32,
    collapsed: bool,
    col_type: i32,
    _col_name: i32,
    compact: bool,
) -> String {
    let ind = indent(depth);
    let raw_type = struct_type_name(node);
    let suffix = if collapsed { "" } else { "{" };
    if node.name.is_empty() {
        // Anonymous struct/union: "union {" with no column padding.
        return format!("{ind}{raw_type}{SEP}{suffix}");
    }
    let ty = if compact {
        fit_overflow(&raw_type, col_type)
    } else {
        fit(&raw_type, col_type)
    };
    format!("{ind}{ty}{SEP}{}{SEP}{suffix}", node.name)
}

/// `fmt::fmtStructFooter(node, depth, totalSize=-1)` (`format.cpp:261-272`).
pub fn fmt_struct_footer(node: &Node, depth: i32, total_size: i32) -> String {
    let mut footer = format!("{}{}", indent(depth), "};");
    if node.is_enum() {
        footer += "  +1 +10 Top";
    } else {
        footer += "  +1 +10h +100h +1000h Trim Top";
    }
    if total_size > 0 {
        footer += &format!("  // 0x{:X} ({})", total_size, total_size);
    }
    footer
}

// ── Array header (`format.cpp:276-282`) ──

/// `fmt::fmtArrayHeader(node, depth, viewIdx, collapsed, colType=14, colName=22, elemStructName={}, compact=false)`.
#[allow(clippy::too_many_arguments)]
pub fn fmt_array_header(
    node: &Node,
    depth: i32,
    _view_idx: i32,
    collapsed: bool,
    col_type: i32,
    _col_name: i32,
    elem_struct_name: &str,
    compact: bool,
) -> String {
    let ind = indent(depth);
    let raw_type = array_type_name(node.element_kind, node.array_len, elem_struct_name);
    let ty = if compact {
        fit_overflow(&raw_type, col_type)
    } else {
        fit(&raw_type, col_type)
    };
    let suffix = if collapsed { "" } else { "{" };
    format!("{ind}{ty}{SEP}{}{SEP}{suffix}", node.name)
}

// ── Pointer header (merged pointer + struct header) (`format.cpp:286-304`) ──

/// `fmt::fmtPointerHeader(node, depth, collapsed, prov, addr, ptrTypeName, colType=14, colName=22, compact=false)`.
#[allow(clippy::too_many_arguments)]
pub fn fmt_pointer_header(
    node: &Node,
    depth: i32,
    collapsed: bool,
    prov: &dyn Provider,
    addr: u64,
    ptr_type_name: &str,
    col_type: i32,
    col_name: i32,
    compact: bool,
) -> String {
    let ind = indent(depth);
    let overflow = compact && (u16_len(ptr_type_name) as i32) > col_type;
    let ty = if compact {
        fit_overflow(ptr_type_name, col_type)
    } else {
        fit(ptr_type_name, col_type)
    };
    if collapsed {
        if overflow {
            // Overflow: no column padding.
            let val = read_value(node, prov, addr, 0);
            return format!("{ind}{ty}{SEP}{}{SEP}{val}", node.name);
        }
        // Collapsed: show pointer value instead of brace (name padded).
        let name = fit(&node.name, col_name);
        let val = fit(&read_value(node, prov, addr, 0), COL_VALUE);
        return format!("{ind}{ty}{SEP}{name}{SEP}{val}");
    }
    format!("{ind}{ty}{SEP}{}{SEP}{{", node.name)
}

// ── Hex / ASCII preview ──

/// `isAsciiPrintable(c)` (`format.cpp:308`).
#[inline]
fn is_ascii_printable(c: u8) -> bool {
    (0x20..=0x7E).contains(&c)
}

/// `sanitizeString(const QString&)` (`format.cpp:311-323`) — escape control
/// chars for display.
fn sanitize_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                out.push_str("\\x");
                out.push_str(&format!("{:x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// `bytesToAscii(const QByteArray&, int slot)` (`format.cpp:325-333`).
fn bytes_to_ascii(b: &[u8], slot: usize) -> String {
    let mut out = String::with_capacity(slot);
    for i in 0..slot {
        let c = if i < b.len() { b[i] } else { 0 };
        out.push(if is_ascii_printable(c) {
            c as char
        } else {
            '.'
        });
    }
    out
}

const K_HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// `bytesToHex(const QByteArray&, int slot)` (`format.cpp:337-347`) —
/// space-separated 2-digit UPPERCASE hex; no trailing space. Length `slot*3-1`.
fn bytes_to_hex(b: &[u8], slot: usize) -> String {
    let mut out = String::with_capacity(if slot > 0 { slot * 3 - 1 } else { 0 });
    for i in 0..slot {
        let c = if i < b.len() { b[i] } else { 0 };
        out.push(K_HEX_DIGITS[(c >> 4) as usize] as char);
        out.push(K_HEX_DIGITS[(c & 0xF) as usize] as char);
        if i + 1 < slot {
            out.push(' ');
        }
    }
    out
}

/// `fmtAsciiAndBytes(prov, addr, sizeBytes, slotBytes=8)` (`format.cpp:349-356`).
pub fn fmt_ascii_and_bytes(
    prov: &dyn Provider,
    addr: u64,
    size_bytes: i32,
    slot_bytes: i32,
) -> String {
    let slot = slot_bytes.max(size_bytes);
    let slot_usize = slot.max(0) as usize;
    let b = if prov.is_readable(addr, slot) {
        prov.read_bytes(addr, slot)
    } else {
        vec![0u8; slot_usize]
    };
    format!(
        "{}  {}",
        bytes_to_ascii(&b, slot_usize),
        bytes_to_hex(&b, slot_usize)
    )
}

// ── Single value from provider (unified) ──

#[derive(Copy, Clone, PartialEq, Eq)]
enum ValueMode {
    Display,
    Editable,
}

/// `readValueImpl(node, prov, addr, subLine, mode)` (`format.cpp:362-503`).
fn read_value_impl(
    node: &Node,
    prov: &dyn Provider,
    addr: u64,
    sub_line: i32,
    mode: ValueMode,
) -> String {
    let display = mode == ValueMode::Display;
    let be = node.big_endian;
    let r_u16 = |a: u64| -> u16 {
        let v = prov.read_u16(a);
        if be {
            v.swap_bytes()
        } else {
            v
        }
    };
    let r_u32 = |a: u64| -> u32 {
        let v = prov.read_u32(a);
        if be {
            v.swap_bytes()
        } else {
            v
        }
    };
    let r_u64 = |a: u64| -> u64 {
        let v = prov.read_u64(a);
        if be {
            v.swap_bytes()
        } else {
            v
        }
    };
    let r_f32 = |a: u64| -> f32 {
        let mut v = prov.read_u32(a);
        if be {
            v = v.swap_bytes();
        }
        f32::from_bits(v)
    };
    let r_f64 = |a: u64| -> f64 {
        let mut v = prov.read_u64(a);
        if be {
            v = v.swap_bytes();
        }
        f64::from_bits(v)
    };

    match node.kind {
        NodeKind::Hex8 => {
            if display {
                hex_val(u64::from(prov.read_u8(addr)))
            } else {
                raw_hex(u64::from(prov.read_u8(addr)), 2)
            }
        }
        NodeKind::Hex16 => {
            if display {
                hex_val(u64::from(r_u16(addr)))
            } else {
                raw_hex(u64::from(r_u16(addr)), 4)
            }
        }
        NodeKind::Hex32 => {
            if display {
                hex_val(u64::from(r_u32(addr)))
            } else {
                raw_hex(u64::from(r_u32(addr)), 8)
            }
        }
        NodeKind::Hex64 => {
            if display {
                hex_val(r_u64(addr))
            } else {
                raw_hex(r_u64(addr), 16)
            }
        }
        NodeKind::Hex128 => {
            // 16-byte hex: read as two uint64 (low + high).
            let mut b = prov.read_bytes(addr, 16);
            if b.len() < 16 {
                b.resize(16, 0);
            }
            if !display {
                // Editable: space-separated hex bytes (display order).
                let mut show = b.clone();
                if be {
                    show.reverse();
                }
                let mut hex = String::new();
                for (i, byte) in show.iter().enumerate().take(16) {
                    if i > 0 {
                        hex.push(' ');
                    }
                    hex += &raw_hex(u64::from(*byte), 2);
                }
                return hex;
            }
            if be {
                b.reverse();
            }
            // Display: "0x" + 32 hex digits (big-endian display).
            let lo = u64::from_le_bytes(b[0..8].try_into().unwrap());
            let hi = u64::from_le_bytes(b[8..16].try_into().unwrap());
            if hi == 0 {
                return hex_val(lo);
            }
            format!("0x{:X}{}", hi, left_pad_zeros(&format!("{lo:X}"), 16))
        }
        NodeKind::Int8 => fmt_int8(prov.read_u8(addr) as i8),
        NodeKind::Int16 => fmt_int16(r_u16(addr) as i16),
        NodeKind::Int32 => fmt_int32(r_u32(addr) as i32),
        NodeKind::Int64 => fmt_int64(r_u64(addr) as i64),
        NodeKind::Int128 | NodeKind::UInt128 => {
            let mut b = prov.read_bytes(addr, 16);
            if b.len() < 16 {
                b.resize(16, 0);
            }
            if be {
                b.reverse();
            }
            let arr: [u8; 16] = b[..16].try_into().unwrap();
            if node.kind == NodeKind::Int128 {
                fmt_int128(&arr)
            } else {
                fmt_uint128(&arr)
            }
        }
        NodeKind::UInt8 => fmt_uint8(prov.read_u8(addr)),
        NodeKind::UInt16 => fmt_uint16(r_u16(addr)),
        NodeKind::UInt32 => fmt_uint32(r_u32(addr)),
        NodeKind::UInt64 => fmt_uint64(r_u64(addr)),
        NodeKind::Float16 => {
            let s = fmt_float16(r_u16(addr));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Float => {
            let s = fmt_float(r_f32(addr));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Double => {
            let s = fmt_double(r_f64(addr));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Bool => fmt_bool(prov.read_u8(addr)),
        NodeKind::Pointer32 => {
            let val = prov.read_u32(addr);
            if !display {
                raw_hex(u64::from(val), 8)
            } else {
                fmt_pointer32(val)
            }
        }
        NodeKind::Pointer64 => {
            let val = prov.read_u64(addr);
            // Primitive pointer: dereference and show target value.
            if node.ptr_depth > 0 && is_valid_primitive_ptr_target(node.element_kind) && val != 0 {
                let mut target = val;
                let mut d = 1;
                while d < node.ptr_depth && target != 0 {
                    target = if prov.is_readable(target, 8) {
                        prov.read_u64(target)
                    } else {
                        0
                    };
                    d += 1;
                }
                if target != 0 && prov.is_readable(target, size_for_kind(node.element_kind)) {
                    let tmp = Node {
                        kind: node.element_kind,
                        str_len: node.str_len,
                        ..Node::default()
                    };
                    let deref_val = read_value_impl(&tmp, prov, target, 0, mode);
                    if display {
                        return format!("-> {deref_val}");
                    }
                    return deref_val;
                }
                if !display {
                    return raw_hex(val, 16);
                }
                return fmt_pointer64(val);
            }
            if !display {
                raw_hex(val, 16)
            } else {
                fmt_pointer64(val)
            }
        }
        NodeKind::FuncPtr32 => {
            let val = prov.read_u32(addr);
            if !display {
                raw_hex(u64::from(val), 8)
            } else {
                fmt_pointer32(val)
            }
        }
        NodeKind::FuncPtr64 => {
            let val = prov.read_u64(addr);
            if !display {
                raw_hex(val, 16)
            } else {
                fmt_pointer64(val)
            }
        }
        NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4 => {
            let count = size_for_kind(node.kind) / 4;
            let parts: Vec<String> = (0..count)
                .map(|i| fmt_float(prov.read_f32(addr + (i as u64) * 4)))
                .collect();
            parts.join(", ")
        }
        NodeKind::Mat4x4 => {
            if !display {
                return String::new(); // not editable as single value
            }
            if !(0..4).contains(&sub_line) {
                return "?".to_string();
            }
            let mut line = format!("row{sub_line} [");
            for c in 0..4 {
                if c > 0 {
                    line += ", ";
                }
                line += &fmt_float(prov.read_f32(addr + ((sub_line * 4 + c) as u64) * 4));
            }
            line += "]";
            line
        }
        NodeKind::UTF8 => {
            let mut bytes = prov.read_bytes(addr, node.str_len);
            if let Some(end) = bytes.iter().position(|&b| b == 0) {
                bytes.truncate(end);
            }
            let s = String::from_utf8_lossy(&bytes).into_owned();
            if display {
                format!("\"{}\"", sanitize_string(&s))
            } else {
                s
            }
        }
        NodeKind::UTF16 => {
            let bytes = prov.read_bytes(addr, node.str_len * 2);
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let mut s: String = char::decode_utf16(units.iter().copied())
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect();
            if let Some(end) = s.find('\u{0}') {
                s.truncate(end);
            }
            if display {
                format!("L\"{}\"", sanitize_string(&s))
            } else {
                s
            }
        }
        _ => String::new(),
    }
}

/// `fmt::readValue(node, prov, addr, subLine)` (`format.cpp:505-508`) — Display.
pub fn read_value(node: &Node, prov: &dyn Provider, addr: u64, sub_line: i32) -> String {
    read_value_impl(node, prov, addr, sub_line, ValueMode::Display)
}

/// `fmt::editableValue(node, prov, addr, subLine)` (`format.cpp:560-563`).
pub fn editable_value(node: &Node, prov: &dyn Provider, addr: u64, sub_line: i32) -> String {
    read_value_impl(node, prov, addr, sub_line, ValueMode::Editable)
}

// ── Full node line (`format.cpp:512-556`) ──

/// `fmt::fmtNodeLine(node, prov, addr, depth, subLine=0, comment={}, colType=14, colName=22, typeOverride={}, compact=false)`.
#[allow(clippy::too_many_arguments)]
pub fn fmt_node_line(
    node: &Node,
    prov: &dyn Provider,
    addr: u64,
    depth: i32,
    sub_line: i32,
    comment: &str,
    col_type: i32,
    col_name: i32,
    type_override: &str,
    compact: bool,
) -> String {
    let ind = indent(depth);

    let raw_type = if type_override.is_empty() {
        type_name_raw(node.kind)
    } else {
        type_override.to_string()
    };
    let overflow = compact && (u16_len(&raw_type) as i32) > col_type;

    let ty = if overflow {
        fit_overflow(&raw_type, col_type)
    } else if type_override.is_empty() {
        type_name_fitted(node.kind, col_type)
    } else {
        fit(type_override, col_type)
    };
    let name = fit(&node.name, col_name);

    let effective_col_type = if overflow {
        u16_len(&raw_type) as i32
    } else {
        col_type
    };
    let prefix_w = (effective_col_type + col_name + 2 * K_SEP_WIDTH).max(0) as usize;

    let cmt_suffix = if comment.is_empty() {
        String::new()
    } else {
        fit(comment, COL_COMMENT)
    };

    // Mat4x4: subLine 0..3 = rows — no truncation.
    if node.kind == NodeKind::Mat4x4 {
        let val = read_value(node, prov, addr, sub_line);
        if sub_line == 0 {
            return format!("{ind}{ty}{SEP}{name}{SEP}{val}{cmt_suffix}");
        }
        return format!("{ind}{}{val}{cmt_suffix}", " ".repeat(prefix_w));
    }

    // Hex nodes: hex byte preview (ASCII padded to colName).
    if is_hex_preview(node.kind) {
        let sz = size_for_kind(node.kind);
        let sz_usize = sz.max(0) as usize;
        let b = if prov.is_readable(addr, sz) {
            prov.read_bytes(addr, sz)
        } else {
            vec![0u8; sz_usize]
        };
        let ascii = left_justified(&bytes_to_ascii(&b, sz_usize), col_name.max(0) as usize);
        let hex_w = (23).max(sz * 3 - 1).max(0) as usize;
        let hex = left_justified(&bytes_to_hex(&b, sz_usize), hex_w);
        return format!("{ind}{ty}{SEP}{ascii}{SEP}{hex}{cmt_suffix}");
    }

    let val = if overflow {
        read_value(node, prov, addr, sub_line)
    } else {
        fit(&read_value(node, prov, addr, sub_line), COL_VALUE)
    };
    format!("{ind}{ty}{SEP}{name}{SEP}{val}{cmt_suffix}")
}

// ── Value parsing (text → bytes) ──

/// `stripHex(const QString&)` (`format.cpp:574-578`) — drop a leading `0x`/`0X`.
fn strip_hex(s: &str) -> &str {
    if s.len() >= 2 && s[..2].eq_ignore_ascii_case("0x") {
        &s[2..]
    } else {
        s
    }
}

/// `fmt::parseAsciiValue(text, expectedSize, &ok)` (`format.cpp:581-592`).
/// Each char becomes one byte; chars > 255 fail. `None` ⇒ ok=false.
pub fn parse_ascii_value(text: &str, expected_size: i32) -> Option<Vec<u8>> {
    let chars: Vec<char> = text.chars().collect();
    // QString::size() is UTF-16 units; for ASCII/Latin1 it equals char count.
    // Each unit > 255 fails anyway, so use UTF-16-unit length for the compare.
    if u16_len(text) != expected_size.max(0) as usize {
        return None;
    }
    let mut result = Vec::with_capacity(chars.len());
    for c in chars {
        let cp = c as u32;
        if cp > 255 {
            return None; // non-Latin1 character
        }
        result.push(cp as u8);
    }
    Some(result)
}

/// `parseHexBytes(s, expectedSize, &ok)` (`format.cpp:597-622`). Two forms:
/// space-separated exact-2-char groups, or contiguous `expectedSize*2` chars.
fn parse_hex_bytes(s: &str, expected_size: i32) -> Option<Vec<u8>> {
    let trimmed = s.trim();
    let expected = expected_size.max(0) as usize;
    if trimmed.contains(' ') {
        let parts: Vec<&str> = trimmed.split(' ').filter(|p| !p.is_empty()).collect();
        if parts.len() != expected {
            return None;
        }
        let mut result = Vec::with_capacity(expected);
        for p in parts {
            if p.chars().count() != 2 {
                return None;
            }
            let byte = u8::from_str_radix(p, 16).ok()?;
            result.push(byte);
        }
        Some(result)
    } else {
        if trimmed.chars().count() != expected * 2 {
            return None;
        }
        let chars: Vec<char> = trimmed.chars().collect();
        let mut result = Vec::with_capacity(expected);
        for i in 0..expected {
            let pair: String = chars[i * 2..i * 2 + 2].iter().collect();
            let byte = u8::from_str_radix(&pair, 16).ok()?;
            result.push(byte);
        }
        Some(result)
    }
}

/// `fmt::parseValue(NodeKind, text, &ok)` (`format.cpp:638-820`). `None` ⇒
/// ok=false. Empty UTF8/UTF16 ⇒ `Some(vec![])` (caller pads).
pub fn parse_value_kind(kind: NodeKind, text: &str) -> Option<Vec<u8>> {
    let s = text.trim();

    if s.is_empty() {
        // Allow empty for string types (zero-length content; caller pads).
        if kind == NodeKind::UTF8 || kind == NodeKind::UTF16 {
            return Some(Vec::new());
        }
        return None;
    }

    // Hex kinds parse as memory-order bytes (display order).
    let parse_hex_unified = |cleaned: &str, byte_count: i32| -> Option<Vec<u8>> {
        if cleaned.contains(' ') {
            return parse_hex_bytes(cleaned, byte_count);
        }
        let bc = byte_count.max(0) as usize;
        let clen = cleaned.chars().count();
        if clen > bc * 2 {
            return None; // too many digits
        }
        if clen < bc * 2 {
            let padded = format!("{}{}", "0".repeat(bc * 2 - clen), cleaned);
            return parse_hex_bytes(&padded, byte_count);
        }
        parse_hex_bytes(cleaned, byte_count)
    };

    match kind {
        NodeKind::Hex8 => parse_hex_unified(strip_hex(s), 1),
        NodeKind::Hex16 => parse_hex_unified(strip_hex(s), 2),
        NodeKind::Hex32 => parse_hex_unified(strip_hex(s), 4),
        NodeKind::Hex64 => parse_hex_unified(strip_hex(s), 8),
        NodeKind::Hex128 => parse_hex_unified(strip_hex(s), 16),
        NodeKind::Int8 => {
            if is_hex_prefixed(s) {
                let val = parse_radix_u64(strip_hex(s), 16)?;
                if val > 0xFF {
                    return None;
                }
                Some(((val as u8) as i8).to_le_bytes().to_vec())
            } else {
                let val: i64 = s.parse().ok()?;
                int_checked_i8(val)
            }
        }
        NodeKind::Int16 => {
            if is_hex_prefixed(s) {
                let val = parse_radix_u64(strip_hex(s), 16)?;
                if val > 0xFFFF {
                    return None;
                }
                Some(((val as u16) as i16).to_le_bytes().to_vec())
            } else {
                let val: i64 = s.parse().ok()?;
                int_checked_i16(val)
            }
        }
        NodeKind::Int32 => {
            if is_hex_prefixed(s) {
                let val = parse_radix_u64(strip_hex(s), 16)?;
                if val > 0xFFFF_FFFF {
                    return None;
                }
                Some(((val as u32) as i32).to_le_bytes().to_vec())
            } else {
                let val: i32 = parse_decimal_i32(s)?;
                Some(val.to_le_bytes().to_vec())
            }
        }
        NodeKind::Int64 => {
            if is_hex_prefixed(s) {
                let val = parse_radix_u64(strip_hex(s), 16)?;
                Some((val as i64).to_le_bytes().to_vec())
            } else {
                let val: i64 = s.parse().ok()?;
                Some(val.to_le_bytes().to_vec())
            }
        }
        NodeKind::UInt8 => {
            let base = if is_hex_prefixed(s) { 16 } else { 10 };
            let val = parse_radix_u64(strip_hex(s), base)?;
            int_checked_u8(val)
        }
        NodeKind::UInt16 => {
            let base = if is_hex_prefixed(s) { 16 } else { 10 };
            let val = parse_radix_u64(strip_hex(s), base)?;
            int_checked_u16(val)
        }
        NodeKind::UInt32 => {
            let base = if is_hex_prefixed(s) { 16 } else { 10 };
            let val = parse_radix_u64(strip_hex(s), base)?;
            int_checked_u32(val)
        }
        NodeKind::UInt64 => {
            let base = if is_hex_prefixed(s) { 16 } else { 10 };
            let val = parse_radix_u64(strip_hex(s), base)?;
            Some(val.to_le_bytes().to_vec())
        }
        NodeKind::Int128 | NodeKind::UInt128 => parse_value_128(kind, s),
        NodeKind::Float16 => {
            let mut n = s.trim().to_string();
            if n.to_lowercase().ends_with('h') {
                n.pop();
            }
            if n.to_lowercase().ends_with('f') {
                n.pop();
            }
            let n = n.replace(',', ".");
            let val: f32 = n.parse().ok()?;
            Some(float_to_half(val).to_le_bytes().to_vec())
        }
        NodeKind::Float => {
            let mut n = s.trim().to_string();
            if n.to_lowercase().ends_with('f') {
                n.pop();
            }
            let n = n.replace(',', ".");
            let val: f32 = n.parse().ok()?;
            Some(val.to_le_bytes().to_vec())
        }
        NodeKind::Double => {
            let n = s.replace(',', ".");
            let val: f64 = n.parse().ok()?;
            Some(val.to_le_bytes().to_vec())
        }
        NodeKind::Bool => {
            if s == "true" || s == "1" {
                Some(vec![1u8])
            } else if s == "false" || s == "0" {
                Some(vec![0u8])
            } else {
                None
            }
        }
        NodeKind::Pointer32 | NodeKind::FuncPtr32 => {
            let val = parse_radix_u64(strip_hex(s), 16)?;
            if val > 0xFFFF_FFFF {
                return None;
            }
            Some((val as u32).to_le_bytes().to_vec())
        }
        NodeKind::Pointer64 | NodeKind::FuncPtr64 => {
            let val = parse_radix_u64(strip_hex(s), 16)?;
            Some(val.to_le_bytes().to_vec())
        }
        NodeKind::UTF8 => {
            let mut t = s;
            if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
                t = &t[1..t.len() - 1];
            }
            Some(t.as_bytes().to_vec())
        }
        NodeKind::UTF16 => {
            let mut t = s.to_string();
            if let Some(rest) = t.strip_prefix("L\"") {
                t = rest.to_string();
            } else if let Some(rest) = t.strip_prefix('"') {
                t = rest.to_string();
            }
            if t.ends_with('"') {
                t.pop();
            }
            let mut bytes = Vec::with_capacity(t.len() * 2);
            for u in t.encode_utf16() {
                bytes.extend_from_slice(&u.to_le_bytes());
            }
            Some(bytes)
        }
        _ => None,
    }
}

#[inline]
fn is_hex_prefixed(s: &str) -> bool {
    s.len() >= 2 && s[..2].eq_ignore_ascii_case("0x")
}

/// Mirrors `QString::toUInt/toULongLong(&ok, base)` for our needs: parse the
/// whole string in the given radix (no `0x` prefix), returning None on any
/// failure. Qt rejects empty / non-digit input.
fn parse_radix_u64(s: &str, radix: u32) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    u64::from_str_radix(s, radix).ok()
}

/// Mirrors `QString::toInt(&ok, 10)` for Int32: rejects out-of-range i32.
fn parse_decimal_i32(s: &str) -> Option<i32> {
    s.parse::<i32>().ok()
}

// parseIntChecked<T> specializations (`format.cpp:625-636`).
fn int_checked_i8(val: i64) -> Option<Vec<u8>> {
    if val < i64::from(i8::MIN) || val > i64::from(i8::MAX) {
        return None;
    }
    Some((val as i8).to_le_bytes().to_vec())
}
fn int_checked_i16(val: i64) -> Option<Vec<u8>> {
    if val < i64::from(i16::MIN) || val > i64::from(i16::MAX) {
        return None;
    }
    Some((val as i16).to_le_bytes().to_vec())
}
fn int_checked_u8(val: u64) -> Option<Vec<u8>> {
    if val > u64::from(u8::MAX) {
        return None;
    }
    Some((val as u8).to_le_bytes().to_vec())
}
fn int_checked_u16(val: u64) -> Option<Vec<u8>> {
    if val > u64::from(u16::MAX) {
        return None;
    }
    Some((val as u16).to_le_bytes().to_vec())
}
fn int_checked_u32(val: u64) -> Option<Vec<u8>> {
    if val > u64::from(u32::MAX) {
        return None;
    }
    Some((val as u32).to_le_bytes().to_vec())
}

/// 128-bit parse (`format.cpp:719-755`).
fn parse_value_128(kind: NodeKind, s: &str) -> Option<Vec<u8>> {
    let is_hex = is_hex_prefixed(s);
    let mut digits = if is_hex { strip_hex(s) } else { s };
    let mut neg = false;
    if !is_hex {
        if let Some(rest) = digits.strip_prefix('-') {
            neg = true;
            digits = rest;
        }
    }
    if digits.is_empty() {
        return None;
    }
    let mut acc: u128 = 0;
    let base: u128 = if is_hex { 16 } else { 10 };
    for c in digits.chars() {
        let d: i32 = if c.is_ascii_digit() {
            (c as i32) - ('0' as i32)
        } else if is_hex && ('a'..='f').contains(&c) {
            10 + (c as i32) - ('a' as i32)
        } else if is_hex && ('A'..='F').contains(&c) {
            10 + (c as i32) - ('A' as i32)
        } else {
            -1
        };
        if d < 0 || d as u128 >= base {
            return None;
        }
        let next = acc.wrapping_mul(base).wrapping_add(d as u128);
        if next < acc {
            return None; // overflow
        }
        acc = next;
    }
    if kind == NodeKind::Int128 {
        // Signed range: [-(2^127), 2^127-1]
        let sign_bit: u128 = 1u128 << 127;
        if neg {
            if acc > sign_bit {
                return None;
            }
            acc = (acc as i128).wrapping_neg() as u128;
        } else if acc >= sign_bit {
            return None;
        }
    } else if neg {
        return None; // unsigned can't be negative
    }
    Some(acc.to_le_bytes().to_vec())
}

/// `fmt::parseValue(const Node&, text, &ok)` (`format.cpp:825-841`). Node-aware:
/// big-endian reverses the parsed bytes for scalar endian-bearing kinds.
pub fn parse_value(node: &Node, text: &str) -> Option<Vec<u8>> {
    let mut out = parse_value_kind(node.kind, text)?;
    if !node.big_endian || out.is_empty() {
        return Some(out);
    }
    match node.kind {
        NodeKind::Int16
        | NodeKind::UInt16
        | NodeKind::Hex16
        | NodeKind::Int32
        | NodeKind::UInt32
        | NodeKind::Hex32
        | NodeKind::Int64
        | NodeKind::UInt64
        | NodeKind::Hex64
        | NodeKind::Int128
        | NodeKind::UInt128
        | NodeKind::Hex128
        | NodeKind::Float16
        | NodeKind::Float
        | NodeKind::Double => out.reverse(),
        _ => {}
    }
    Some(out)
}

// ── Value validation (`format.cpp:845-896`) ──

/// `fmt::validateValue(NodeKind, text)` — error message, or empty if valid.
pub fn validate_value(kind: NodeKind, text: &str) -> String {
    let s = text.trim();
    if s.is_empty() {
        return String::new();
    }

    let is_hex_kind = is_hex_node(kind)
        || matches!(
            kind,
            NodeKind::Pointer32 | NodeKind::Pointer64 | NodeKind::FuncPtr32 | NodeKind::FuncPtr64
        );
    let is_int_kind = kind >= NodeKind::Int8 && kind <= NodeKind::UInt128;

    if is_hex_kind || is_int_kind {
        let has_hex_prefix = is_hex_prefixed(s);
        let digits: Vec<char> = if has_hex_prefix {
            s.chars().skip(2).collect()
        } else {
            s.chars().collect()
        };

        if has_hex_prefix || is_hex_kind {
            // Hex mode: 0-9, a-f, A-F (spaces allowed for multi-byte hex kinds).
            let is_multi_byte_hex = kind >= NodeKind::Hex16 && kind <= NodeKind::Hex128;
            for c in &digits {
                if *c == ' ' && is_multi_byte_hex {
                    continue;
                }
                if !c.is_ascii_digit()
                    && !('a'..='f').contains(c)
                    && !('A'..='F').contains(c)
                {
                    return format!("invalid hex '{c}'");
                }
            }
        } else {
            // Decimal mode: digits (and leading minus for signed).
            let is_signed = kind >= NodeKind::Int8 && kind <= NodeKind::Int128;
            let start = if is_signed && !digits.is_empty() && digits[0] == '-' {
                1
            } else {
                0
            };
            for c in digits.iter().skip(start) {
                if !c.is_ascii_digit() {
                    return format!("invalid '{c}'");
                }
            }
        }
    }

    // Then the actual parse for range checking.
    if parse_value_kind(kind, text).is_some() {
        return String::new();
    }

    let is_float_kind = matches!(kind, NodeKind::Float | NodeKind::Double);
    if is_float_kind {
        return "invalid number".to_string();
    }

    if let Some(m) = kind_meta(kind) {
        if m.size > 0 && m.size <= 8 {
            let max_val: u64 = if m.size == 8 {
                u64::MAX
            } else {
                (1u64 << (m.size * 8)) - 1
            };
            let width = (m.size * 2) as usize;
            return format!("too large! max=0x{:0width$x}", max_val);
        }
    }
    "invalid".to_string()
}

// ── Base address validation (delegates to AddressParser) (`format.cpp:900-905`) ──

/// `fmt::validateBaseAddress(text)` — error message, or empty if valid. Empty
/// input ⇒ `"empty"`; otherwise delegates to [`AddressParser::validate`].
pub fn validate_base_address(text: &str) -> String {
    let s = text.trim();
    if s.is_empty() {
        return "empty".to_string();
    }
    AddressParser::validate(s)
}

// ── Enum / bitfield member lines (`format.cpp:907-934`) ──

/// `fmt::fmtEnumMember(name, value, depth, nameW)` (`format.cpp:907-910`).
pub fn fmt_enum_member(name: &str, value: i64, depth: i32, name_w: i32) -> String {
    let ind = indent(depth);
    format!(
        "{ind}{} = {value}",
        left_justified(name, name_w.max(0) as usize)
    )
}

/// `fmt::extractBits(prov, addr, containerKind, bitOffset, bitWidth)`
/// (`format.cpp:914-927`).
pub fn extract_bits(
    prov: &dyn Provider,
    addr: u64,
    container_kind: NodeKind,
    bit_offset: u8,
    bit_width: u8,
) -> u64 {
    let container: u64 = match container_kind {
        NodeKind::Hex8 => u64::from(prov.read_u8(addr)),
        NodeKind::Hex16 => u64::from(prov.read_u16(addr)),
        NodeKind::Hex32 => u64::from(prov.read_u32(addr)),
        _ => prov.read_u64(addr),
    };
    debug_assert!(u32::from(bit_offset) + u32::from(bit_width) <= 64);
    if bit_width >= 64 {
        return container >> bit_offset;
    }
    (container >> bit_offset) & ((1u64 << bit_width) - 1)
}

/// `fmt::fmtBitfieldMember(name, bitWidth, value, depth, nameW)`
/// (`format.cpp:929-934`).
pub fn fmt_bitfield_member(name: &str, bit_width: u8, value: u64, depth: i32, name_w: i32) -> String {
    let ind = indent(depth);
    format!(
        "{ind}{} : {bit_width} = {value}",
        left_justified(name, name_w.max(0) as usize)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::BufferProvider;

    // ── testTypeName (test_format.cpp:9-13) ──
    #[test]
    fn test_type_name() {
        let s = type_name(NodeKind::Float);
        assert_eq!(s.trim(), "float");
        assert_eq!(u16_len(&s), 14); // kColType
    }

    // ── testFmtInt32 (test_format.cpp:15-19) ──
    #[test]
    fn test_fmt_int32() {
        assert_eq!(fmt_int32(-42), "-42");
        assert_eq!(fmt_int32(0), "0");
    }

    // ── testFmtFloat (test_format.cpp:21-64) ──
    #[test]
    fn test_fmt_float() {
        let check = |v: f32, expected: &str| {
            assert_eq!(fmt_float(v), expected, "fmt_float({v})");
        };
        check(3.14159, "3.1416f");
        check(-3.14159, "-3.1416f");
        check(0.0, "0.0000f");
        check(0.02, "0.0200f");
        check(-0.069, "-0.0690f");
        check(15.6543, "15.654f");
        check(-77.6624, "-77.662f");
        check(500.0, "500.00f");
        check(5000.0, "5000.0f");
        check(50000.0, "50000.f");
        check(100000.0, "99999+f");
        check(-100000.0, "-99999+f");
        check(1.0 / 0.0, "inff");
        check(-1.0 / 0.0, "-inff");
        assert_eq!(fmt_float(f32::NAN), "NaN");
        check(1.0, "1.0000f");
        check(-1.0, "-1.0000f");
    }

    // ── testFmtBool (test_format.cpp:66-69) ──
    #[test]
    fn test_fmt_bool() {
        assert_eq!(fmt_bool(1), "true");
        assert_eq!(fmt_bool(0), "false");
    }

    // ── testFmtPointer64_null / _nonNull (test_format.cpp:71-79) ──
    #[test]
    fn test_fmt_pointer64() {
        assert_eq!(fmt_pointer64(0), "nullptr");
        let s = fmt_pointer64(0x400000);
        assert!(s.starts_with("0x"));
        assert!(s.contains("400000"));
    }

    // ── testFmtOffsetMargin_* (test_format.cpp:81-97) ──
    #[test]
    fn test_fmt_offset_margin() {
        assert_eq!(fmt_offset_margin(0x10, false, 8), "00000010 ");
        assert_eq!(fmt_offset_margin(0, false, 8), "00000000 ");
        assert_eq!(fmt_offset_margin(0x10, true, 8), "  \u{00B7} ");
        assert_eq!(
            fmt_offset_margin(0xFFFF_F800_1234_5678, false, 16),
            "FFFFF80012345678 "
        );
        assert_eq!(fmt_offset_margin(0x10, false, 16), "0000000000000010 ");
        assert_eq!(fmt_offset_margin(0x10, false, 4), "0010 ");
    }

    // ── testFmtStructHeader (test_format.cpp:99-114) ──
    #[test]
    fn test_fmt_struct_header() {
        let n = Node {
            kind: NodeKind::Struct,
            name: "Test".into(),
            ..Node::default()
        };
        let s = fmt_struct_header(&n, 0, false, COL_TYPE, COL_NAME, false);
        assert!(s.contains("struct"));
        assert!(s.contains("Test"));
        assert!(s.contains('{'));

        let collapsed = fmt_struct_header(&n, 0, true, COL_TYPE, COL_NAME, false);
        assert!(collapsed.contains("struct"));
        assert!(collapsed.contains("Test"));
        assert!(!collapsed.contains('{'));
    }

    // ── testFmtStructFooter (test_format.cpp:116-123) ──
    #[test]
    fn test_fmt_struct_footer() {
        let n = Node {
            kind: NodeKind::Struct,
            name: "Test".into(),
            ..Node::default()
        };
        let s = fmt_struct_footer(&n, 0, -1);
        assert!(s.contains("};"));
    }

    // ── testFmtStructFooterSimple (test_format.cpp:324-333) ──
    #[test]
    fn test_fmt_struct_footer_simple() {
        let n = Node {
            kind: NodeKind::Struct,
            name: "Test".into(),
            ..Node::default()
        };
        let s = fmt_struct_footer(&n, 0, 0x14);
        assert!(s.contains("};"));
        assert!(!s.contains("sizeof"));
    }

    // ── testIndent (test_format.cpp:125-129) ──
    #[test]
    fn test_indent() {
        assert_eq!(indent(0), "");
        assert_eq!(indent(1), "  ");
        assert_eq!(indent(3), "      ");
    }

    // ── testParseValueInt32 (test_format.cpp:131-139) ──
    #[test]
    fn test_parse_value_int32() {
        let b = parse_value_kind(NodeKind::Int32, "-42").expect("ok");
        assert_eq!(b.len(), 4);
        assert_eq!(i32::from_le_bytes(b.try_into().unwrap()), -42);
    }

    // ── testParseValueFloat (test_format.cpp:141-149) ──
    #[test]
    fn test_parse_value_float() {
        let b = parse_value_kind(NodeKind::Float, "3.14").expect("ok");
        assert_eq!(b.len(), 4);
        let v = f32::from_le_bytes(b.try_into().unwrap());
        assert!((v - 3.14f32).abs() < 0.01);
    }

    // ── testParseValueHex32 (test_format.cpp:151-163) ──
    #[test]
    fn test_parse_value_hex32() {
        let b = parse_value_kind(NodeKind::Hex32, "DEADBEEF").expect("ok");
        assert_eq!(b, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    // ── testParseValueBool (test_format.cpp:165-179) ──
    #[test]
    fn test_parse_value_bool() {
        let b = parse_value_kind(NodeKind::Bool, "true").expect("ok");
        assert_eq!(b, vec![1]);
        let b = parse_value_kind(NodeKind::Bool, "false").expect("ok");
        assert_eq!(b, vec![0]);
        assert!(parse_value_kind(NodeKind::Bool, "banana").is_none());
    }

    // ── testParseValueHex0xPrefix (test_format.cpp:181-196) ──
    #[test]
    fn test_parse_value_hex_0x_prefix() {
        let b = parse_value_kind(NodeKind::Hex32, "0xDEADBEEF").expect("ok");
        assert_eq!(b.len(), 4);
        assert_eq!(b[0], 0xDE);
        assert_eq!(b[3], 0xEF);

        let b = parse_value_kind(NodeKind::Pointer64, "0x0000000000400000").expect("ok");
        let v = u64::from_le_bytes(b.try_into().unwrap());
        assert_eq!(v, 0x400000);
    }

    // ── testParseValueOverflow (test_format.cpp:198-235) ──
    #[test]
    fn test_parse_value_overflow() {
        assert!(parse_value_kind(NodeKind::UInt8, "300").is_none());
        let b = parse_value_kind(NodeKind::UInt8, "255").expect("ok");
        assert_eq!(b[0], 255);
        assert!(parse_value_kind(NodeKind::Int8, "200").is_none());
        assert!(parse_value_kind(NodeKind::Int8, "-129").is_none());
        let b = parse_value_kind(NodeKind::Int8, "-128").expect("ok");
        assert_eq!(b[0] as i8, -128);
        assert!(parse_value_kind(NodeKind::UInt16, "70000").is_none());
        assert!(parse_value_kind(NodeKind::Hex8, "1FF").is_none());
        assert!(parse_value_kind(NodeKind::Hex16, "1FFFF").is_none());
    }

    // ── testSignedHexRoundTrip (test_format.cpp:237-273) ──
    #[test]
    fn test_signed_hex_round_trip() {
        let b = parse_value_kind(NodeKind::Int8, "0xFF").expect("ok");
        assert_eq!(b[0] as i8, -1);
        let b = parse_value_kind(NodeKind::Int8, "0x80").expect("ok");
        assert_eq!(b[0] as i8, -128);
        let b = parse_value_kind(NodeKind::Int16, "0xFFFF").expect("ok");
        assert_eq!(i16::from_le_bytes(b.try_into().unwrap()), -1);
        let b = parse_value_kind(NodeKind::Int32, "0xFFFFFFFF").expect("ok");
        assert_eq!(i32::from_le_bytes(b.try_into().unwrap()), -1);
        assert!(parse_value_kind(NodeKind::Int8, "0x1FF").is_none());
        assert!(parse_value_kind(NodeKind::Int16, "0x1FFFF").is_none());
    }

    // ── testReadValueBoundsCheck (test_format.cpp:275-291) ──
    #[test]
    fn test_read_value_bounds_check() {
        let prov = BufferProvider::new(vec![0u8; 16], "t");
        let mut n = Node {
            kind: NodeKind::Vec2,
            name: "v".into(),
            ..Node::default()
        };
        assert!(read_value(&n, &prov, 0, 0).contains(','));
        n.kind = NodeKind::Vec3;
        assert_eq!(read_value(&n, &prov, 0, 0).matches(',').count(), 2);
        n.kind = NodeKind::Vec4;
        assert_eq!(read_value(&n, &prov, 0, 0).matches(',').count(), 3);
    }

    // ── testEditableValueBasic (test_format.cpp:293-310) ──
    #[test]
    fn test_editable_value_basic() {
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(&3.14f32.to_le_bytes());
        let prov = BufferProvider::new(data, "t");
        let mut n = Node {
            kind: NodeKind::Float,
            name: "f".into(),
            ..Node::default()
        };
        assert!(editable_value(&n, &prov, 0, 0).contains("3.14"));
        n.kind = NodeKind::Vec2;
        assert!(editable_value(&n, &prov, 0, 0).contains(','));
    }

    // ── testParseValueEmptyString (test_format.cpp:312-322) ──
    #[test]
    fn test_parse_value_empty_string() {
        let b = parse_value_kind(NodeKind::UTF8, "").expect("ok");
        assert!(b.is_empty());
        assert!(parse_value_kind(NodeKind::Int32, "").is_none());
    }

    // ── testFmtFloatEdgeCases (test_format.cpp:334-342) ──
    #[test]
    fn test_fmt_float_edge_cases() {
        assert_eq!(fmt_float(f32::NAN), "NaN");
        assert_eq!(fmt_float(f32::INFINITY), "inff");
        assert_eq!(fmt_float(f32::NEG_INFINITY), "-inff");
        assert!(fmt_float(3.14).contains('f'));
        assert!(fmt_float(-0.0).starts_with('-'));
    }

    // ── testFmtDoubleIntegerValue (test_format.cpp:344-348) ──
    #[test]
    fn test_fmt_double_integer_value() {
        assert!(fmt_double(42.0).contains('.'));
    }

    // ── testValidateValueEmpty (test_format.cpp:355-358) ──
    #[test]
    fn test_validate_value_empty() {
        assert!(validate_value(NodeKind::Int32, "").is_empty());
    }

    // ── testValidateValueHexOverflow (test_format.cpp:360-364) ──
    #[test]
    fn test_validate_value_hex_overflow() {
        let err = validate_value(NodeKind::Int8, "999");
        assert!(!err.is_empty());
    }

    // ── testParseValueHex128 (test_format.cpp:377-386) ──
    #[test]
    fn test_parse_value_hex128() {
        let b = parse_value_kind(
            NodeKind::Hex128,
            "00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF",
        )
        .expect("ok");
        assert_eq!(b.len(), 16);
        assert_eq!(b[0], 0x00);
        assert_eq!(b[15], 0xFF);
    }

    // ── testParseValueHex128TooShort (test_format.cpp:388-394) ──
    #[test]
    fn test_parse_value_hex128_too_short() {
        assert!(parse_value_kind(NodeKind::Hex128, "00 11 22 33 44 55 66 77").is_none());
    }

    // ── testReadValueHex128 (test_format.cpp:396-411) ──
    #[test]
    fn test_read_value_hex128() {
        let mut data = vec![0u8; 16];
        data[0] = 0x41; // 'A'
        data[15] = 0xFF;
        let prov = BufferProvider::new(data, "t");
        let n = Node {
            kind: NodeKind::Hex128,
            ..Node::default()
        };
        let val = read_value(&n, &prov, 0, 0);
        assert!(!val.is_empty());
        let edit = editable_value(&n, &prov, 0, 0);
        assert!(edit.contains(' '));
        assert!(edit.len() >= 47); // 16*3-1 = 47
    }

    // ── testFmtFloatVerySmall (test_format.cpp:413-418) ──
    #[test]
    fn test_fmt_float_very_small() {
        let s = fmt_float(1e-7);
        assert!(s.contains('f'));
        assert!(s.len() <= 9);
    }

    // ── testFmtDoubleVeryLarge (test_format.cpp:420-424) ──
    #[test]
    fn test_fmt_double_very_large() {
        let s = fmt_double(1e308);
        assert!(!s.is_empty());
        assert!(s.contains('.') || s.contains('e') || s.contains('E'));
    }

    // ── testFmtDoubleNegativeZero (test_format.cpp:426-430) ──
    #[test]
    fn test_fmt_double_negative_zero() {
        assert!(!fmt_double(-0.0).is_empty());
    }

    // ── testFmtDoubleNanInf (test_format.cpp:432-437) ──
    #[test]
    fn test_fmt_double_nan_inf() {
        assert!(!fmt_double(f64::NAN).is_empty());
        assert!(!fmt_double(f64::INFINITY).is_empty());
    }

    // ── testParseValueUtf8Emoji (test_format.cpp:439-444) ──
    #[test]
    fn test_parse_value_utf8() {
        let b = parse_value_kind(NodeKind::UTF8, "\"hello\"").expect("ok");
        assert_eq!(b, b"hello".to_vec());
    }

    // ── testParseValueHex16SpaceSeparated (test_format.cpp:446-453) ──
    #[test]
    fn test_parse_value_hex16_space_separated() {
        let b = parse_value_kind(NodeKind::Hex16, "AB CD").expect("ok");
        assert_eq!(b, vec![0xAB, 0xCD]);
    }

    // ── testValidateValueHex128 (test_format.cpp:455-459) ──
    #[test]
    fn test_validate_value_hex128() {
        let err = validate_value(
            NodeKind::Hex128,
            "00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF",
        );
        assert!(err.is_empty());
    }

    // ── testIsValidPrimitivePtrTarget (test_format.cpp:476-484) ──
    #[test]
    fn test_is_valid_primitive_ptr_target() {
        assert!(!is_valid_primitive_ptr_target(NodeKind::Hex8));
        assert!(!is_valid_primitive_ptr_target(NodeKind::Pointer64));
        assert!(!is_valid_primitive_ptr_target(NodeKind::Struct));
        assert!(!is_valid_primitive_ptr_target(NodeKind::FuncPtr64));
        assert!(is_valid_primitive_ptr_target(NodeKind::Int32));
        assert!(is_valid_primitive_ptr_target(NodeKind::Float));
        assert!(is_valid_primitive_ptr_target(NodeKind::Bool));
    }

    // ── Extra fidelity checks beyond the captured test (parity safeguards) ──
    #[test]
    fn test_fmt_uint_is_hex_lowercase() {
        assert_eq!(fmt_uint32(0xABCD), "0xabcd");
        assert_eq!(fmt_uint8(0), "0x0");
    }

    #[test]
    fn test_parse_value_node_big_endian_reverses() {
        let node = Node {
            kind: NodeKind::Int32,
            big_endian: true,
            ..Node::default()
        };
        // -42 LE = [0xD6, 0xFF, 0xFF, 0xFF]; reversed for BE.
        let le = parse_value_kind(NodeKind::Int32, "-42").unwrap();
        let be = parse_value(&node, "-42").unwrap();
        let mut expect = le.clone();
        expect.reverse();
        assert_eq!(be, expect);

        // Int8 is NOT swapped.
        let node8 = Node {
            kind: NodeKind::Int8,
            big_endian: true,
            ..Node::default()
        };
        assert_eq!(parse_value(&node8, "5").unwrap(), vec![5]);
    }

    #[test]
    fn test_extract_bits() {
        // container = 0xF0 ; bits [4..8) = 0xF
        let prov = BufferProvider::new(vec![0xF0, 0, 0, 0, 0, 0, 0, 0], "t");
        assert_eq!(extract_bits(&prov, 0, NodeKind::Hex8, 4, 4), 0xF);
        assert_eq!(extract_bits(&prov, 0, NodeKind::Hex8, 0, 4), 0x0);
    }

    #[test]
    fn test_fmt_enum_and_bitfield_member() {
        assert_eq!(fmt_enum_member("A", 5, 1, 4), "  A    = 5");
        assert_eq!(fmt_bitfield_member("flag", 3, 7, 0, 6), "flag   : 3 = 7");
    }

    #[test]
    fn test_array_and_pointer_type_name() {
        assert_eq!(array_type_name(NodeKind::UInt32, 16, ""), "uint32_t[16]");
        assert_eq!(array_type_name(NodeKind::Struct, 2, "Material"), "Material[2]");
        assert_eq!(pointer_type_name(NodeKind::Pointer64, ""), "void*");
        assert_eq!(
            pointer_type_name(NodeKind::Pointer64, "StructName"),
            "StructName*"
        );
    }

    #[test]
    fn test_fmt_float16_roundtrip() {
        // 1.0 in half = 0x3C00
        assert_eq!(fmt_float16(0x3C00), "1h");
        assert!(fmt_float16(0x7C00).contains("inf")); // +inf
    }

    #[test]
    fn test_type_name_override_seam() {
        fn over(_k: NodeKind) -> String {
            "X".to_string()
        }
        set_type_name_provider(Some(over));
        assert_eq!(type_name_raw(NodeKind::Float), "X");
        set_type_name_provider(None);
        assert_eq!(type_name_raw(NodeKind::Float), "float");
    }
}
