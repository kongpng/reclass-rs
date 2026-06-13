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

/// `kTreeIndent` (ReClass uses 2; widened to 3 for clearer nested indentation)
/// and `kSepWidth = 1` (`core.h:1136`) — canonical definitions in `core::linemeta`.
use crate::core::linemeta::{K_SEP_WIDTH, K_TREE_INDENT};

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

/// Byte length of [`type_name_raw`] without allocating on the built-in type-name
/// path. The compose width pass uses byte counts for parity with its existing
/// `String::len()` calculation.
pub fn type_name_raw_len(kind: NodeKind) -> usize {
    if let Some(f) = current_type_name_fn() {
        return f(kind).len();
    }
    kind_meta(kind).map_or(3, |m| m.type_name.len())
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
    // Round half-AWAY-from-zero (Qt `QString::number(av,'f',dec)`), NOT Rust's
    // built-in round-half-to-even, so e.g. 37428.5 → "37429.f".
    for dec in (0..=4).rev() {
        let mut body = fmt_fixed_away_f32_body(av, dec as usize);
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

fn fmt_fixed_away_f32_body(v: f32, frac: usize) -> String {
    debug_assert!(v.is_finite() && v >= 0.0 && frac <= 4);
    const POW10: [u64; 5] = [1, 10, 100, 1_000, 10_000];
    let scale = POW10[frac];
    let rounded = (f64::from(v) * scale as f64 + 0.5).floor() as u64;
    let int_part = rounded / scale;
    if frac == 0 {
        return int_part.to_string();
    }
    let frac_part = rounded % scale;
    let mut out = int_part.to_string();
    out.push('.');
    push_zero_padded_u64(&mut out, frac_part, frac);
    out
}

fn push_zero_padded_u64(out: &mut String, value: u64, width: usize) {
    let text = value.to_string();
    for _ in text.len()..width {
        out.push('0');
    }
    out.push_str(&text);
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

/// Append the `  // <module>!<symbol>` suffix to `s` when the provider resolves a
/// symbol at `addr_val` (the shared C++ `format.cpp` pointer-value annotation).
fn with_symbol_suffix(mut s: String, prov: &dyn Provider, addr_val: u64) -> String {
    let sym = prov.get_symbol(addr_val);
    if !sym.is_empty() {
        s.push_str("  // ");
        s.push_str(&sym);
    }
    s
}

/// A 32-bit pointer / function-pointer value: raw 8-hex when `!display`, else the
/// formatted pointer + any symbol suffix. Shared by Pointer32 / FuncPtr32.
fn ptr32_value(prov: &dyn Provider, val: u32, display: bool) -> String {
    if !display {
        raw_hex(u64::from(val), 8)
    } else {
        with_symbol_suffix(fmt_pointer32(val), prov, u64::from(val))
    }
}

/// A 64-bit pointer / function-pointer value (the non-dereferencing case): raw
/// 16-hex when `!display`, else the formatted pointer + any symbol suffix. Shared
/// by FuncPtr64 and Pointer64's non-deref path.
fn ptr64_value(prov: &dyn Provider, val: u64, display: bool) -> String {
    if !display {
        raw_hex(val, 16)
    } else {
        with_symbol_suffix(fmt_pointer64(val), prov, val)
    }
}

/// Exact decimal expansion of a finite `f64`'s magnitude, as `(int, frac)`
/// digit strings (no sign). Rust's `{:.*}` formatting emits the *exact* decimal
/// value of a double (a dyadic rational, so it terminates) — formatting with a
/// precision past the longest possible expansion (`1074` for `2^-1074`) yields
/// the true digits with no rounding. We then do our own rounding so we can pick
/// round-half-AWAY-from-zero (Qt `QString::number`) instead of Rust's built-in
/// round-half-to-even.
fn exact_decimal_parts(mag: f64) -> (String, String) {
    debug_assert!(mag.is_finite() && mag >= 0.0);
    let s = format!("{mag:.1100}");
    match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s, String::new()),
    }
}

/// Round a (non-negative) decimal given as `int`/`frac` digit strings to exactly
/// `keep` fractional digits, **rounding half away from zero**. Returns the
/// rounded `(int, frac)` digit strings (frac has length `keep`). Propagates carry
/// into the integer part (e.g. `99.9` → `keep=0` → `("100","")`).
fn round_half_away(int_digits: &str, frac_digits: &str, keep: usize) -> (String, String) {
    // Combine into a single digit buffer; remember the decimal point position.
    let mut digits: Vec<u8> = Vec::with_capacity(int_digits.len() + frac_digits.len());
    for c in int_digits.bytes() {
        digits.push(c - b'0');
    }
    for c in frac_digits.bytes() {
        digits.push(c - b'0');
    }
    let int_len = int_digits.len();
    // Index of the first dropped fractional digit (the rounding digit).
    let cut = int_len + keep;
    let mut round_up = false;
    if cut < digits.len() {
        // Half away from zero: round up when the first dropped digit >= 5.
        if digits[cut] >= 5 {
            round_up = true;
        }
    }
    digits.truncate(cut);
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                // Carried a new leading digit into the integer part.
                let int_part: String = digits[..int_len + 1]
                    .iter()
                    .map(|d| (d + b'0') as char)
                    .collect();
                let frac_part: String = digits[int_len + 1..]
                    .iter()
                    .map(|d| (d + b'0') as char)
                    .collect();
                return (int_part, frac_part);
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    // Pad the fractional part back up to `keep` digits (truncation may have cut
    // it short if the source had fewer digits than requested).
    let mut int_part: String = digits[..int_len]
        .iter()
        .map(|d| (d + b'0') as char)
        .collect();
    let mut frac_part: String = digits[int_len..]
        .iter()
        .map(|d| (d + b'0') as char)
        .collect();
    while frac_part.len() < keep {
        frac_part.push('0');
    }
    if int_part.is_empty() {
        int_part.push('0');
    }
    (int_part, frac_part)
}

/// printf `%.*f`-faithful fixed-point formatting with round-half-AWAY-from-zero
/// (matching Qt `QString::number(v,'f',frac)`). Mirrors Rust's `{:.*}` output
/// *shape* (no decimal point when `frac == 0`, leading `-` for negatives,
/// `-0.000` preserved) so existing post-processing keeps working.
fn fmt_fixed_away(v: f64, frac: usize) -> String {
    let neg = v.is_sign_negative();
    let (int_d, frac_d) = exact_decimal_parts(v.abs());
    let (int_r, frac_r) = round_half_away(&int_d, &frac_d, frac);
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&int_r);
    if frac > 0 {
        out.push('.');
        out.push_str(&frac_r);
    }
    out
}

/// printf `%.*e`-faithful scientific formatting with round-half-AWAY-from-zero.
/// Produces the same *shape* as Rust's `{:.*e}` (one digit before the point,
/// `frac` digits after, lowercase `e`, exponent with no leading zeros and a sign
/// only when negative) so `format_e_strip` can post-process it unchanged.
fn fmt_sci_away(v: f64, frac: usize) -> String {
    let neg = v.is_sign_negative();
    let mag = v.abs();
    if mag == 0.0 {
        let mantissa = if frac > 0 {
            format!("0.{}", "0".repeat(frac))
        } else {
            "0".to_string()
        };
        return format!("{}{mantissa}e0", if neg { "-" } else { "" });
    }
    let (int_d, frac_d) = exact_decimal_parts(mag);
    // Full exact significant-digit sequence (no decimal point) and the decimal
    // exponent X such that value = d[0].d[1..] × 10^X.
    let mut all: String = int_d.clone();
    all.push_str(&frac_d);
    let trimmed_int = int_d.trim_start_matches('0');
    let exp10: i32;
    let sig: String;
    if !trimmed_int.is_empty() {
        // Magnitude >= 1: exponent is (#integer digits - 1).
        exp10 = trimmed_int.len() as i32 - 1;
        let lead = int_d.len() - trimmed_int.len();
        sig = all[lead..].to_string();
    } else {
        // Magnitude < 1: skip leading zeros in the fractional part.
        let lead_zeros = frac_d.len() - frac_d.trim_start_matches('0').len();
        exp10 = -(lead_zeros as i32) - 1;
        sig = frac_d.trim_start_matches('0').to_string();
    }
    // `sig` now has its first digit as the single pre-point digit. Round the
    // remaining `frac` significant digits (half away). Reuse round_half_away by
    // treating the first sig digit as the "integer" part.
    let first = &sig[..1];
    let rest = &sig[1..];
    let (int_r, frac_r) = round_half_away(first, rest, frac);
    // A carry can push e.g. 9.99→10.0, lengthening the integer part: re-normalize
    // so exactly one digit sits before the point and bump the exponent.
    let (digit, exp_final, frac_final) = if int_r.len() > 1 {
        // int_r is "10" (carry); becomes 1.0 × 10^(exp+1), dropping a frac digit.
        let mut combined = int_r.clone();
        combined.push_str(&frac_r);
        let d = combined[..1].to_string();
        let mut f = combined[1..].to_string();
        // Keep exactly `frac` fractional digits.
        f.truncate(frac);
        while f.len() < frac {
            f.push('0');
        }
        (d, exp10 + 1, f)
    } else {
        (int_r, exp10, frac_r)
    };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&digit);
    if frac > 0 {
        out.push('.');
        out.push_str(&frac_final);
    }
    out.push('e');
    out.push_str(&exp_final.to_string());
    out
}

/// Emulates Qt `QString::number(double, 'g', precision)` = C `printf %.*g`
/// (significant-digit form, shortest of `%e`/`%f`, trailing zeros stripped),
/// then Qt lowercases the exponent marker (`e`). Used by `fmtDouble`/`fmtFloat16`.
/// Rounding is round-half-AWAY-from-zero to match Qt (not Rust's half-to-even).
fn qstring_number_g(v: f64, precision: i32) -> String {
    // C `%g`: precision 0 is treated as 1.
    let p = if precision <= 0 {
        1
    } else {
        precision as usize
    };
    if v == 0.0 {
        // Qt's `QString::number(..,'g',..)` preserves the sign of negative zero
        // (snprintf `%g` emits "-0"); since `-0.0 == 0.0` in Rust, a plain
        // early-return would silently drop it. Mirror Qt so `fmt_double(-0.0)`
        // reads "-0.0" and `fmt_float16` keeps its sign (cf. C++ fmtDouble via
        // QString::number, and fmtFloat's explicit `-0.000f` handling).
        return if v.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    // Determine decimal exponent X (as in C `%g`: choose `%e` if X < -4 or X >= P).
    // printf rounds to P significant digits FIRST, then picks the exponent, so a
    // carry like 9.99999e5 → 1.00000e6 must bump X. Formatting the (half-away)
    // scientific form at (P-1) fractional digits already applies that rounding,
    // so we read the post-rounding exponent straight off the result.
    let e_str = fmt_sci_away(v, p - 1);
    let exp10 = v.abs().log10().floor() as i32;
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
        let f_str = fmt_fixed_away(v, frac);
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
    // Expanded: still show the raw stored pointer value, then open the
    // brace. For an absolute pointer the value matches the first child's
    // offset column (helpful confirmation). For an RVA pointer the raw
    // value (e.g. 0x78 for e_lfanew) differs from the resolved target —
    // surfacing it here is the only way a user can diagnose a shifted or
    // wrong RVA without diving into raw bytes.
    //
    // The value lines up with sibling rows' value columns via the
    // fixed-width name pad (fit(node.name, col_name)). DO NOT also pad the
    // value to COL_VALUE — that would push the trailing '{' all the way to
    // the right edge of the value column, visually disconnecting the brace
    // from its row (C++ keeps the brace attached).
    let name = fit(&node.name, col_name);
    let val = read_value(node, prov, addr, 0);
    format!("{ind}{ty}{SEP}{name}{SEP}{val} {{")
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
    let b = prov.read_bytes(addr, slot);
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

fn read_le<const N: usize>(bytes: &[u8]) -> Option<[u8; N]> {
    (bytes.len() >= N).then(|| bytes[..N].try_into().unwrap())
}

fn format_vec_f32_from_bytes(kind: NodeKind, bytes: &[u8]) -> Option<String> {
    let count = size_for_kind(kind) as usize / 4;
    if bytes.len() < count.saturating_mul(4) {
        return None;
    }
    let mut out = String::with_capacity(count.saturating_mul(12));
    for i in 0..count {
        if i > 0 {
            out.push_str(", ");
        }
        let start = i * 4;
        let lane = u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]);
        out.push_str(&fmt_float(f32::from_bits(lane)));
    }
    Some(out)
}

fn format_deref_primitive_from_bytes(
    kind: NodeKind,
    bytes: &[u8],
    mode: ValueMode,
) -> Option<String> {
    let display = mode == ValueMode::Display;
    Some(match kind {
        NodeKind::Int8 => fmt_int8(*bytes.first()? as i8),
        NodeKind::Int16 => fmt_int16(i16::from_le_bytes(read_le::<2>(bytes)?)),
        NodeKind::Int32 => fmt_int32(i32::from_le_bytes(read_le::<4>(bytes)?)),
        NodeKind::Int64 => fmt_int64(i64::from_le_bytes(read_le::<8>(bytes)?)),
        NodeKind::UInt8 => fmt_uint8(*bytes.first()?),
        NodeKind::UInt16 => fmt_uint16(u16::from_le_bytes(read_le::<2>(bytes)?)),
        NodeKind::UInt32 => fmt_uint32(u32::from_le_bytes(read_le::<4>(bytes)?)),
        NodeKind::UInt64 => fmt_uint64(u64::from_le_bytes(read_le::<8>(bytes)?)),
        NodeKind::Float16 => {
            let s = fmt_float16(u16::from_le_bytes(read_le::<2>(bytes)?));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Float => {
            let s = fmt_float(f32::from_bits(u32::from_le_bytes(read_le::<4>(bytes)?)));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Double => {
            let s = fmt_double(f64::from_bits(u64::from_le_bytes(read_le::<8>(bytes)?)));
            if display {
                s
            } else {
                s.trim().to_string()
            }
        }
        NodeKind::Bool => fmt_bool(*bytes.first()?),
        NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4 => format_vec_f32_from_bytes(kind, bytes)?,
        _ => return None,
    })
}

/// Display-format a node from bytes the caller already read. This intentionally
/// covers only cases where the display value is fully determined by `bytes`
/// (plus optional symbol lookup for pointer text). Dereference pointers can
/// depend on target memory and should keep using [`read_value`].
pub fn read_display_value_from_bytes(
    node: &Node,
    prov: &dyn Provider,
    bytes: &[u8],
    sub_line: i32,
) -> Option<String> {
    let be = node.big_endian;
    let load_u16 = |bytes: &[u8]| -> Option<u16> {
        let mut value = u16::from_le_bytes(read_le::<2>(bytes)?);
        if be {
            value = value.swap_bytes();
        }
        Some(value)
    };
    let load_u32 = |bytes: &[u8]| -> Option<u32> {
        let mut value = u32::from_le_bytes(read_le::<4>(bytes)?);
        if be {
            value = value.swap_bytes();
        }
        Some(value)
    };
    let load_u64 = |bytes: &[u8]| -> Option<u64> {
        let mut value = u64::from_le_bytes(read_le::<8>(bytes)?);
        if be {
            value = value.swap_bytes();
        }
        Some(value)
    };

    Some(match node.kind {
        NodeKind::Hex8 => hex_val(u64::from(*bytes.first()?)),
        NodeKind::Hex16 => hex_val(u64::from(load_u16(bytes)?)),
        NodeKind::Hex32 => hex_val(u64::from(load_u32(bytes)?)),
        NodeKind::Hex64 => hex_val(load_u64(bytes)?),
        NodeKind::Hex128 => {
            let mut b = read_le::<16>(bytes)?;
            if be {
                b.reverse();
            }
            let lo = u64::from_le_bytes(b[0..8].try_into().unwrap());
            let hi = u64::from_le_bytes(b[8..16].try_into().unwrap());
            if hi == 0 {
                hex_val(lo)
            } else {
                format!("0x{:X}{}", hi, left_pad_zeros(&format!("{lo:X}"), 16))
            }
        }
        NodeKind::Int8 => fmt_int8(*bytes.first()? as i8),
        NodeKind::Int16 => fmt_int16(load_u16(bytes)? as i16),
        NodeKind::Int32 => fmt_int32(load_u32(bytes)? as i32),
        NodeKind::Int64 => fmt_int64(load_u64(bytes)? as i64),
        NodeKind::Int128 | NodeKind::UInt128 => {
            let mut b = read_le::<16>(bytes)?;
            if be {
                b.reverse();
            }
            if node.kind == NodeKind::Int128 {
                fmt_int128(&b)
            } else {
                fmt_uint128(&b)
            }
        }
        NodeKind::UInt8 => fmt_uint8(*bytes.first()?),
        NodeKind::UInt16 => fmt_uint16(load_u16(bytes)?),
        NodeKind::UInt32 => fmt_uint32(load_u32(bytes)?),
        NodeKind::UInt64 => fmt_uint64(load_u64(bytes)?),
        NodeKind::Float16 => fmt_float16(load_u16(bytes)?),
        NodeKind::Float => fmt_float(f32::from_bits(load_u32(bytes)?)),
        NodeKind::Double => fmt_double(f64::from_bits(load_u64(bytes)?)),
        NodeKind::Bool => fmt_bool(*bytes.first()?),
        NodeKind::Pointer32 | NodeKind::FuncPtr32 => ptr32_value(prov, load_u32(bytes)?, true),
        NodeKind::Pointer64 | NodeKind::FuncPtr64 => {
            let value = load_u64(bytes)?;
            if node.kind == NodeKind::Pointer64
                && node.ptr_depth > 0
                && is_valid_primitive_ptr_target(node.element_kind)
                && value != 0
            {
                return None;
            }
            ptr64_value(prov, value, true)
        }
        NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4 => {
            format_vec_f32_from_bytes(node.kind, bytes)?
        }
        NodeKind::Mat4x4 => {
            if !(0..4).contains(&sub_line) {
                return Some("?".to_string());
            }
            let start = sub_line as usize * 16;
            let row = bytes.get(start..start + 16)?;
            let mut line = String::with_capacity(56);
            line.push_str("row");
            line.push(char::from(b'0' + sub_line as u8));
            line.push_str(" [");
            for c in 0..4 {
                if c > 0 {
                    line.push_str(", ");
                }
                let start = c * 4;
                let lane = u32::from_le_bytes(row[start..start + 4].try_into().unwrap());
                line.push_str(&fmt_float(f32::from_bits(lane)));
            }
            line.push(']');
            line
        }
        NodeKind::UTF8 => {
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            let s = String::from_utf8_lossy(&bytes[..end]).into_owned();
            format!("\"{}\"", sanitize_string(&s))
        }
        NodeKind::UTF16 => {
            let units = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]));
            let mut s: String = char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect();
            if let Some(end) = s.find('\u{0}') {
                s.truncate(end);
            }
            format!("L\"{}\"", sanitize_string(&s))
        }
        _ => return None,
    })
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
        NodeKind::Pointer32 | NodeKind::FuncPtr32 => {
            let val = prov.read_u32(addr);
            ptr32_value(prov, val, display)
        }
        NodeKind::Pointer64 => {
            let val = prov.read_u64(addr);
            // Primitive pointer: dereference and show target value.
            if node.ptr_depth > 0 && is_valid_primitive_ptr_target(node.element_kind) && val != 0 {
                let mut target = val;
                let mut d = 1;
                while d < node.ptr_depth && target != 0 {
                    let mut bytes = [0u8; 8];
                    target = if prov.read(target, &mut bytes) {
                        u64::from_le_bytes(bytes)
                    } else {
                        0
                    };
                    d += 1;
                }
                let target_size = size_for_kind(node.element_kind);
                let mut stack_probe = [0u8; 16];
                let mut heap_probe;
                let probe = if target_size > 0 && target_size as usize <= stack_probe.len() {
                    &mut stack_probe[..target_size as usize]
                } else {
                    heap_probe = vec![0u8; target_size.max(0) as usize];
                    heap_probe.as_mut_slice()
                };
                if target != 0 && target_size > 0 && prov.read(target, probe) {
                    let deref_val =
                        format_deref_primitive_from_bytes(node.element_kind, probe, mode)
                            .unwrap_or_else(|| {
                                let tmp = Node {
                                    kind: node.element_kind,
                                    str_len: node.str_len,
                                    ..Node::default()
                                };
                                read_value_impl(&tmp, prov, target, 0, mode)
                            });
                    if display {
                        // Arrow to deref target value + symbol on the pointer's
                        // own value (`format.cpp:443-449`).
                        let sym = prov.get_symbol(val);
                        if !sym.is_empty() {
                            return format!("-> {deref_val}  // {sym}");
                        }
                        return format!("-> {deref_val}");
                    }
                    return deref_val;
                }
                if !display {
                    return raw_hex(val, 16);
                }
                return fmt_pointer64(val);
            }
            ptr64_value(prov, val, display)
        }
        NodeKind::FuncPtr64 => {
            let val = prov.read_u64(addr);
            ptr64_value(prov, val, display)
        }
        NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4 => {
            let size = size_for_kind(node.kind).max(0) as usize;
            let mut bytes = [0u8; 16];
            let len = size.min(bytes.len());
            let data = &mut bytes[..len];
            let _ = prov.read(addr, data);
            format_vec_f32_from_bytes(node.kind, data).unwrap_or_default()
        }
        NodeKind::Mat4x4 => {
            if !display {
                return String::new(); // not editable as single value
            }
            if !(0..4).contains(&sub_line) {
                return "?".to_string();
            }
            let row_addr = addr + (sub_line as u64) * 16;
            let mut bytes = [0u8; 16];
            let _ = prov.read(row_addr, &mut bytes);
            let mut line = String::with_capacity(56);
            line.push_str("row");
            line.push(char::from(b'0' + sub_line as u8));
            line.push_str(" [");
            for c in 0..4 {
                if c > 0 {
                    line.push_str(", ");
                }
                let start = c as usize * 4;
                let lane = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
                line.push_str(&fmt_float(f32::from_bits(lane)));
            }
            line.push(']');
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
    fmt_node_line_impl(
        node,
        prov,
        addr,
        depth,
        sub_line,
        comment,
        col_type,
        col_name,
        type_override,
        compact,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn fmt_node_line_with_hex_preview_bytes(
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
    hex_preview_bytes: &[u8],
) -> String {
    fmt_node_line_impl(
        node,
        prov,
        addr,
        depth,
        sub_line,
        comment,
        col_type,
        col_name,
        type_override,
        compact,
        Some(hex_preview_bytes),
    )
}

#[allow(clippy::too_many_arguments)]
fn fmt_node_line_impl(
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
    hex_preview_bytes: Option<&[u8]>,
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
        let mut stack_bytes = [0u8; 16];
        let owned_bytes;
        let b = if let Some(bytes) = hex_preview_bytes {
            bytes
        } else if sz > 0 && sz_usize <= stack_bytes.len() {
            let _ = prov.read(addr, &mut stack_bytes[..sz_usize]);
            &stack_bytes[..sz_usize]
        } else {
            owned_bytes = prov.read_bytes(addr, sz);
            owned_bytes.as_slice()
        };
        let ascii = left_justified(&bytes_to_ascii(b, sz_usize), col_name.max(0) as usize);
        let hex_w = (23).max(sz * 3 - 1).max(0) as usize;
        let hex = left_justified(&bytes_to_hex(b, sz_usize), hex_w);
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
            if !qt_float_token_ok(&n) {
                return None;
            }
            let val: f32 = n.parse().ok()?;
            Some(float_to_half(val).to_le_bytes().to_vec())
        }
        NodeKind::Float => {
            let mut n = s.trim().to_string();
            if n.to_lowercase().ends_with('f') {
                n.pop();
            }
            let n = n.replace(',', ".");
            if !qt_float_token_ok(&n) {
                return None;
            }
            let val: f32 = n.parse().ok()?;
            Some(val.to_le_bytes().to_vec())
        }
        NodeKind::Double => {
            let n = s.replace(',', ".");
            if !qt_float_token_ok(&n) {
                return None;
            }
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
            // Qt: `if (s.startsWith('"') && s.endsWith('"')) s = s.mid(1, s.size()-2);`
            // A lone `"` both starts AND ends with `"`, and `mid(1, -1)` is empty,
            // so it strips to "" (writes no bytes) — NOT the literal 0x22 byte the
            // old `len >= 2` guard produced (`format.cpp:818-820`).
            let mut t = s;
            if t.starts_with('"') && t.ends_with('"') {
                // mirror QString::mid(1, size-2): start past the end → empty.
                t = if t.len() >= 2 { &t[1..t.len() - 1] } else { "" };
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

/// Gate float tokens to Qt `toFloat`/`toDouble` semantics. Rust's `str::parse`
/// for `f32`/`f64` accepts more non-finite spellings than Qt: `infinity`,
/// `INFINITY`, `+nan`/`-nan`, etc. Qt only accepts (case-insensitive) the exact
/// tokens `inf`, `+inf`, `-inf`, and `nan` (no sign) for non-finite values; any
/// other inf/nan spelling fails the parse. Finite numeric tokens are unaffected
/// (Rust and Qt agree there), so we only reject the divergent inf/nan spellings.
fn qt_float_token_ok(token: &str) -> bool {
    // Cheaply check whether this token even *looks* like an inf/nan literal.
    // (Plain numbers contain a digit; inf/nan literals never do.)
    if token.bytes().any(|b| b.is_ascii_digit()) {
        return true;
    }
    let lower = token.to_ascii_lowercase();
    let core = lower
        .strip_prefix('+')
        .or_else(|| lower.strip_prefix('-'))
        .unwrap_or(&lower);
    // `nan` accepts no sign in Qt (`+nan`/`-nan` are rejected).
    if core == "nan" {
        return lower == "nan";
    }
    if core == "inf" {
        return true; // inf / +inf / -inf all accepted
    }
    // Anything else non-numeric (e.g. `infinity`, empty, stray text) — let the
    // normal Rust parse decide (it will reject genuine garbage; `infinity` would
    // wrongly succeed in Rust, so reject it here explicitly).
    if core == "infinity" {
        return false;
    }
    true
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
                if !c.is_ascii_digit() && !('a'..='f').contains(c) && !('A'..='F').contains(c) {
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
    // A crafted document can push bit_offset past the container width (the
    // debug_assert documents the intended precondition but is a no-op in
    // release); guard the shift so an out-of-range offset yields 0 rather than a
    // debug-build panic / platform-dependent over-shift.
    if bit_offset >= 64 {
        return 0;
    }
    if bit_width >= 64 {
        return container >> bit_offset;
    }
    (container >> bit_offset) & ((1u64 << bit_width) - 1)
}

/// `fmt::fmtBitfieldMember(name, bitWidth, value, depth, nameW)`
/// (`format.cpp:929-934`).
pub fn fmt_bitfield_member(
    name: &str,
    bit_width: u8,
    value: u64,
    depth: i32,
    name_w: i32,
) -> String {
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
    use std::cell::Cell;
    use std::sync::Mutex;

    // Serializes tests that mutate the global type-name override seam
    // (`G_TYPE_NAME_FN`) so they can't interleave under the parallel test
    // runner and observe each other's provider state.
    static TYPE_NAME_SEAM_LOCK: Mutex<()> = Mutex::new(());

    struct CountingReadProvider {
        data: Vec<u8>,
        read_calls: Cell<usize>,
        readable_calls: Cell<usize>,
    }

    impl CountingReadProvider {
        fn new(data: Vec<u8>) -> Self {
            CountingReadProvider {
                data,
                read_calls: Cell::new(0),
                readable_calls: Cell::new(0),
            }
        }
    }

    impl Provider for CountingReadProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            self.read_calls.set(self.read_calls.get() + 1);
            let start = addr as usize;
            if start + buf.len() > self.data.len() {
                return false;
            }
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
            true
        }

        fn size(&self) -> i32 {
            self.data.len() as i32
        }

        fn is_readable(&self, _addr: u64, _len: i32) -> bool {
            self.readable_calls.set(self.readable_calls.get() + 1);
            true
        }
    }

    // ── testTypeName (test_format.cpp:9-13) ──
    #[test]
    fn test_type_name() {
        let _g = TYPE_NAME_SEAM_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn byte_preview_formatting_reads_without_readability_preflight() {
        let provider = CountingReadProvider::new(vec![0x41, 0x42, 0x00, 0x7F]);

        let preview = fmt_ascii_and_bytes(&provider, 0, 4, 4);
        assert!(preview.contains("AB.."));
        assert_eq!(provider.read_calls.get(), 1);
        assert_eq!(provider.readable_calls.get(), 0);

        let node = Node {
            kind: NodeKind::Hex32,
            name: "bytes".into(),
            ..Node::default()
        };
        let line = fmt_node_line(&node, &provider, 0, 0, 0, "", COL_TYPE, COL_NAME, "", false);
        assert!(line.contains("41 42 00 7F"));
        assert_eq!(provider.read_calls.get(), 2);
        assert_eq!(provider.readable_calls.get(), 0);
    }

    #[test]
    fn primitive_pointer_deref_formats_without_readability_preflight() {
        let mut data = vec![0u8; 32];
        data[0..8].copy_from_slice(&8u64.to_le_bytes());
        data[8..16].copy_from_slice(&16u64.to_le_bytes());
        data[16..20].copy_from_slice(&42i32.to_le_bytes());
        let provider = CountingReadProvider::new(data);
        let node = Node {
            kind: NodeKind::Pointer64,
            ptr_depth: 2,
            element_kind: NodeKind::Int32,
            ..Node::default()
        };

        let value = read_value(&node, &provider, 0, 0);

        assert_eq!(value, "-> 42");
        assert_eq!(
            provider.readable_calls.get(),
            0,
            "primitive pointer deref formatting should not preflight live provider readability"
        );
        assert_eq!(
            provider.read_calls.get(),
            3,
            "pointer, indirect target, and target value should each be read once"
        );
    }

    #[test]
    fn vector_value_formats_with_one_provider_read() {
        let mut data = Vec::new();
        for value in [1.0f32, 2.5, 3.0, 4.25] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let provider = CountingReadProvider::new(data);
        let node = Node {
            kind: NodeKind::Vec4,
            ..Node::default()
        };

        let value = read_value(&node, &provider, 0, 0);

        assert_eq!(value.matches(',').count(), 3);
        assert!(value.contains(&fmt_float(1.0)));
        assert!(value.contains(&fmt_float(4.25)));
        assert_eq!(provider.readable_calls.get(), 0);
        assert_eq!(
            provider.read_calls.get(),
            1,
            "Vec4 formatting should coalesce lane reads into one provider read"
        );
    }

    #[test]
    fn mat4x4_row_formats_with_one_provider_read() {
        let mut data = Vec::new();
        for value in [
            1.0f32, 2.0, 3.0, 4.0, 5.0, 6.5, 7.0, 8.25, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0,
            16.0,
        ] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let provider = CountingReadProvider::new(data);
        let node = Node {
            kind: NodeKind::Mat4x4,
            ..Node::default()
        };

        let value = read_value(&node, &provider, 0, 1);

        assert!(value.starts_with("row1 ["));
        assert_eq!(value.matches(',').count(), 3);
        assert!(value.contains(&fmt_float(5.0)));
        assert!(value.contains(&fmt_float(8.25)));
        assert_eq!(provider.readable_calls.get(), 0);
        assert_eq!(
            provider.read_calls.get(),
            1,
            "Mat4x4 row formatting should coalesce lane reads into one provider read"
        );
    }

    #[test]
    fn display_value_from_bytes_formats_vector_without_provider_read() {
        let mut data = Vec::new();
        for value in [1.0f32, 2.5, 3.0, 4.25] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let provider = CountingReadProvider::new(data.clone());
        let node = Node {
            kind: NodeKind::Vec4,
            ..Node::default()
        };

        let value = read_display_value_from_bytes(&node, &provider, &data, 0).unwrap();

        assert_eq!(value.matches(',').count(), 3);
        assert!(value.contains(&fmt_float(1.0)));
        assert!(value.contains(&fmt_float(4.25)));
        assert_eq!(provider.read_calls.get(), 0);
        assert_eq!(provider.readable_calls.get(), 0);
    }

    #[test]
    fn display_value_from_bytes_formats_hex64_without_provider_read() {
        let data = 0x1234_ABCD_0000_0042u64.to_le_bytes().to_vec();
        let provider = CountingReadProvider::new(data.clone());
        let node = Node {
            kind: NodeKind::Hex64,
            ..Node::default()
        };

        let value = read_display_value_from_bytes(&node, &provider, &data, 0).unwrap();

        assert_eq!(value, "0x1234abcd00000042");
        assert_eq!(provider.read_calls.get(), 0);
        assert_eq!(provider.readable_calls.get(), 0);
    }

    #[test]
    fn display_value_from_bytes_formats_mat4x4_row_without_provider_read() {
        let mut data = Vec::new();
        for value in [
            1.0f32, 2.0, 3.0, 4.0, 5.0, 6.5, 7.0, 8.25, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0,
            16.0,
        ] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let provider = CountingReadProvider::new(data.clone());
        let node = Node {
            kind: NodeKind::Mat4x4,
            ..Node::default()
        };

        let value = read_display_value_from_bytes(&node, &provider, &data, 1).unwrap();

        assert!(value.starts_with("row1 ["));
        assert_eq!(value.matches(',').count(), 3);
        assert!(value.contains(&fmt_float(5.0)));
        assert!(value.contains(&fmt_float(8.25)));
        assert_eq!(provider.read_calls.get(), 0);
        assert_eq!(provider.readable_calls.get(), 0);
    }

    #[test]
    fn display_value_from_bytes_defers_deref_pointer_to_provider_path() {
        let provider = CountingReadProvider::new(0x1000u64.to_le_bytes().to_vec());
        let node = Node {
            kind: NodeKind::Pointer64,
            ptr_depth: 1,
            element_kind: NodeKind::Int32,
            ..Node::default()
        };

        assert!(read_display_value_from_bytes(&node, &provider, &provider.data, 0).is_none());
        assert_eq!(provider.read_calls.get(), 0);
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
        assert_eq!(indent(1), "   ");
        assert_eq!(indent(3), "         ");
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

    // ── Negative-zero keeps its sign (Qt QString::number emits "-0"; cf. C++
    // fmtDouble via QString::number + fmtFloat's explicit -0 handling). fmt_double
    // and fmt_float16 both route through qstring_number_g; fmt_float already had it.
    #[test]
    fn test_fmt_negative_zero_keeps_sign() {
        assert_eq!(fmt_double(-0.0), "-0.0");
        assert_eq!(fmt_double(0.0), "0.0");
        // half-precision negative zero = 0x8000, positive zero = 0x0000.
        assert!(fmt_float16(0x8000).starts_with('-'));
        assert!(!fmt_float16(0x0000).starts_with('-'));
    }

    // ── Round-half-AWAY-from-zero parity with Qt QString::number ──
    // Rust's built-in formatters round half-to-EVEN; Qt rounds half-AWAY, so
    // these exact ties diverge. Pin the Qt-faithful outputs.
    #[test]
    fn test_fmt_float_rounds_half_away() {
        // 37428.5 → "37429.f" (half-even would give "37428.f").
        assert_eq!(fmt_float(37428.5), "37429.f");
        // Generic half ties round away from zero (not to even).
        assert_eq!(fmt_fixed_away(2.5, 0), "3");
        assert_eq!(fmt_fixed_away(-2.5, 0), "-3");
        assert_eq!(fmt_fixed_away(1.25, 1), "1.3");
    }

    #[test]
    fn test_fmt_double_rounds_half_away() {
        // 0.1015625 → "0.101563" (half-even would give "0.101562").
        assert_eq!(fmt_double(0.1015625), "0.101563");
        // Sanity: ordinary values unchanged.
        assert_eq!(fmt_double(0.1), "0.1");
        assert_eq!(fmt_double(42.0), "42.0");
        assert_eq!(fmt_double(1.5), "1.5");
        // Significant-digit carry bumps the exponent (999999.5 → 1e+06).
        assert_eq!(fmt_double(999999.5), "1e+06");
        assert_eq!(fmt_double(1234567.0), "1.23457e+06");
    }

    #[test]
    fn test_fmt_float16_rounds_half_away() {
        // half 0x2000 == 0.0078125 → "0.007813h" (half-even → "0.007812h").
        assert_eq!(fmt_float16(0x2000), "0.007813h");
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

    // ── UTF8 lone-quote strips to empty (Qt mid(1,size-2), format.cpp:818-820) ──
    // A single `"` both starts and ends with `"`; Qt's mid(1,-1) yields "" so no
    // bytes are written. The old `len >= 2` guard wrongly emitted the 0x22 byte.
    #[test]
    fn test_parse_value_utf8_lone_quote_is_empty() {
        let b = parse_value_kind(NodeKind::UTF8, "\"").expect("ok");
        assert!(b.is_empty(), "lone quote must strip to empty, got {b:?}");
        // A pair of quotes (empty string literal) also yields no bytes.
        assert!(parse_value_kind(NodeKind::UTF8, "\"\"").unwrap().is_empty());
        // Unquoted text is taken verbatim (no stripping).
        assert_eq!(
            parse_value_kind(NodeKind::UTF8, "ab").unwrap(),
            b"ab".to_vec()
        );
    }

    // ── Float parse rejects inf/nan spellings Qt toFloat/toDouble reject ──
    // Qt accepts only inf/-inf/+inf/nan (canonical tokens, case-insensitive);
    // Rust additionally accepts "infinity"/"INFINITY"/"+nan"/"-nan", which Qt
    // rejects (`format.cpp:775/782/788`). These divergent tokens contain no `f`/`h`
    // suffix, so the per-kind suffix-chop does not touch them — the gate is what
    // rejects them, for all three float kinds.
    #[test]
    fn test_parse_value_float_rejects_qt_invalid_nonfinite() {
        for kind in [NodeKind::Float, NodeKind::Double, NodeKind::Float16] {
            assert!(
                parse_value_kind(kind, "infinity").is_none(),
                "{kind:?} must reject 'infinity'"
            );
            assert!(
                parse_value_kind(kind, "INFINITY").is_none(),
                "{kind:?} must reject 'INFINITY'"
            );
            assert!(
                parse_value_kind(kind, "+nan").is_none(),
                "{kind:?} must reject '+nan'"
            );
            assert!(
                parse_value_kind(kind, "-nan").is_none(),
                "{kind:?} must reject '-nan'"
            );
            // `nan` (no sign) survives the (no-op) suffix chop and the gate.
            assert!(
                parse_value_kind(kind, "nan").is_some(),
                "{kind:?} accepts 'nan'"
            );
            // Ordinary finite numbers are unaffected.
            assert!(
                parse_value_kind(kind, "1.5").is_some(),
                "{kind:?} accepts '1.5'"
            );
        }
        // Double has NO suffix chop, so the canonical inf tokens round-trip.
        assert!(parse_value_kind(NodeKind::Double, "inf").is_some());
        assert!(parse_value_kind(NodeKind::Double, "-inf").is_some());
        assert!(parse_value_kind(NodeKind::Double, "+inf").is_some());
        // Float/Float16 chop a trailing `f`, so a bare "inf" → "in" → rejected —
        // matching the C++ (this is the existing suffix-chop behavior, unchanged).
        // The display form "inff" (one `f` suffix) DOES round-trip back to inf.
        assert!(parse_value_kind(NodeKind::Float, "inff").is_some());
    }

    #[test]
    fn test_qt_float_token_ok_unit() {
        // Divergent spellings rejected.
        assert!(!qt_float_token_ok("infinity"));
        assert!(!qt_float_token_ok("INFINITY"));
        assert!(!qt_float_token_ok("+nan"));
        assert!(!qt_float_token_ok("-nan"));
        // Canonical tokens accepted (case-insensitive).
        assert!(qt_float_token_ok("inf"));
        assert!(qt_float_token_ok("-inf"));
        assert!(qt_float_token_ok("+inf"));
        assert!(qt_float_token_ok("nan"));
        assert!(qt_float_token_ok("NaN"));
        // Numeric tokens always pass the gate (Rust parse decides validity).
        assert!(qt_float_token_ok("1.5"));
        assert!(qt_float_token_ok("-0.0"));
        assert!(qt_float_token_ok("garbage")); // gate lets Rust parse reject it
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
        assert_eq!(fmt_enum_member("A", 5, 1, 4), "   A    = 5");
        assert_eq!(fmt_bitfield_member("flag", 3, 7, 0, 6), "flag   : 3 = 7");
    }

    #[test]
    fn test_array_and_pointer_type_name() {
        assert_eq!(array_type_name(NodeKind::UInt32, 16, ""), "uint32_t[16]");
        assert_eq!(
            array_type_name(NodeKind::Struct, 2, "Material"),
            "Material[2]"
        );
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
        let _g = TYPE_NAME_SEAM_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        fn over(_k: NodeKind) -> String {
            "X".to_string()
        }
        set_type_name_provider(Some(over));
        assert_eq!(type_name_raw(NodeKind::Float), "X");
        assert_eq!(
            type_name_raw_len(NodeKind::Float),
            type_name_raw(NodeKind::Float).len()
        );
        set_type_name_provider(None);
        assert_eq!(type_name_raw(NodeKind::Float), "float");
        assert_eq!(
            type_name_raw_len(NodeKind::Float),
            type_name_raw(NodeKind::Float).len()
        );
    }

    // A provider whose 8 bytes at addr 0 hold a fixed pointer value, and which
    // resolves exactly that one value to a synthetic `module!Symbol` name.
    struct SymProvider {
        ptr_val: u64,
        sym_addr: u64,
        sym: String,
    }
    impl crate::provider::Provider for SymProvider {
        fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
            if addr == 0 && buf.len() <= 8 {
                let bytes = self.ptr_val.to_le_bytes();
                buf.copy_from_slice(&bytes[..buf.len()]);
                true
            } else {
                buf.iter_mut().for_each(|b| *b = 0);
                true
            }
        }
        fn size(&self) -> i32 {
            4096
        }
        fn get_symbol(&self, a: u64) -> String {
            if a == self.sym_addr {
                self.sym.clone()
            } else {
                String::new()
            }
        }
    }

    // ── Symbol annotation on rendered pointer values (format.cpp:421-475) ──
    #[test]
    fn test_pointer_symbol_suffix() {
        let prov = SymProvider {
            ptr_val: 0x7FF7_1857_0000,
            sym_addr: 0x7FF7_1857_0000,
            sym: "ntdll!RtlUserThreadStart".to_string(),
        };
        let p64 = Node {
            kind: NodeKind::Pointer64,
            ..Node::default()
        };
        let disp = read_value(&p64, &prov, 0, 0);
        assert!(
            disp.contains("// ntdll!RtlUserThreadStart"),
            "Pointer64 display should append `  // module!Symbol`, got: {disp}"
        );
        assert!(disp.contains("0x7ff718570000"));
        // Editable mode never appends the symbol.
        let edit = editable_value(&p64, &prov, 0, 0);
        assert!(
            !edit.contains("//"),
            "editable value must not carry symbol: {edit}"
        );

        // FuncPtr64 carries the same suffix.
        let fp64 = Node {
            kind: NodeKind::FuncPtr64,
            ..Node::default()
        };
        assert!(read_value(&fp64, &prov, 0, 0).contains("// ntdll!RtlUserThreadStart"));

        // No symbol → no suffix.
        let prov_nosym = SymProvider {
            ptr_val: 0x1234,
            sym_addr: 0xDEAD,
            sym: "x".to_string(),
        };
        let s = read_value(&p64, &prov_nosym, 0, 0);
        assert!(!s.contains("//"), "no matching symbol → no suffix: {s}");
    }

    // ── Symbol suffix on the primitive-deref `-> <derefVal>` branch ──
    #[test]
    fn test_pointer_deref_symbol_suffix() {
        // Pointer at addr 0 → 0x100; target holds an int32 = 0x2A.
        struct DerefProv;
        impl crate::provider::Provider for DerefProv {
            fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
                let v: u64 = match addr {
                    0 => 0x100,    // the pointer
                    0x100 => 0x2A, // the int32 target
                    _ => 0,
                };
                let bytes = v.to_le_bytes();
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = bytes.get(i).copied().unwrap_or(0);
                }
                true
            }
            fn size(&self) -> i32 {
                4096
            }
            fn get_symbol(&self, a: u64) -> String {
                if a == 0x100 {
                    "mod!gPtr".to_string()
                } else {
                    String::new()
                }
            }
        }
        let prov = DerefProv;
        let node = Node {
            kind: NodeKind::Pointer64,
            ptr_depth: 1,
            element_kind: NodeKind::Int32,
            ..Node::default()
        };
        let disp = read_value(&node, &prov, 0, 0);
        assert!(disp.starts_with("-> 42"), "deref arrow + value: {disp}");
        assert!(
            disp.ends_with("// mod!gPtr"),
            "deref branch should append symbol of the pointer value: {disp}"
        );
    }

    #[test]
    fn test_pointer_deref_vec4_stack_sized_target() {
        struct Vec4DerefProv;
        impl crate::provider::Provider for Vec4DerefProv {
            fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
                match addr {
                    0 => {
                        let bytes = 0x100u64.to_le_bytes();
                        buf.copy_from_slice(&bytes[..buf.len()]);
                    }
                    0x100 => {
                        let values = [1.0f32, 2.5, 3.0, 4.25];
                        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
                        buf.copy_from_slice(&bytes[..buf.len()]);
                    }
                    _ => buf.fill(0),
                }
                true
            }

            fn size(&self) -> i32 {
                4096
            }
        }
        let node = Node {
            kind: NodeKind::Pointer64,
            ptr_depth: 1,
            element_kind: NodeKind::Vec4,
            ..Node::default()
        };
        let disp = read_value(&node, &Vec4DerefProv, 0, 0);
        assert!(disp.starts_with("-> "), "deref arrow expected: {disp}");
        assert_eq!(disp.matches(',').count(), 3);
        assert!(disp.contains(&fmt_float(1.0)));
        assert!(disp.contains(&fmt_float(4.25)));
    }
}
