//! Symbol-name demanglers — MSVC RTTI type descriptors, Itanium type names, and
//! the `humanize_symbol_name` dispatch façade.
//!
//! Port of the demangler functions in `src/rtti.cpp`
//! (`demangleRttiName` @ rtti.cpp:38, `demangleItaniumName` @ rtti.cpp:284) and
//! `src/names/symbol_demangle.cpp` (`humanizeSymbolName` @ symbol_demangle.cpp:11).
//!
//! The MSVC RTTI demangler and the Itanium bare-type fallback are **hand-rolled**
//! (the off-the-shelf demanglers reject the bare `.?AV` / `3Foo` forms produced by
//! the RTTI walkers, exactly as the C++ comments warn). `cpp_demangle` is used
//! best-effort for full `_Z…` symbols; `msvc-demangler` replaces the Windows-only
//! `UnDecorateSymbolName` path cross-platform.

/// `demangleRttiName(mangled)` (`rtti.cpp:38`).
///
/// MSVC RTTI type-descriptor demangler. Hand-rolled: `UnDecorateSymbolName` /
/// `__cxa_demangle` emit garbage on bare `.?AV` descriptors. Anything that
/// doesn't fit the form is returned verbatim (load-bearing for `demangleMalformed`).
///
/// Format: `.?AV<segments>@@` (V=class, U=struct, W=enum, X=void). Segments are
/// listed inner-most first, so reversed for `outer::inner` display.
///   - `.?AVFoo@@`     -> `Foo`
///   - `.?AVBar@Foo@@` -> `Foo::Bar`
///   - `.?AVZ@Y@X@@`   -> `X::Y::Z`
pub fn demangle_rtti_name(mangled: &str) -> String {
    if mangled.is_empty() {
        return String::new();
    }

    // rtti.cpp:55-58 — only `.?A` / `?A` forms are parsed; everything else
    // passes through verbatim.
    if !mangled.starts_with(".?A") && !mangled.starts_with("?A") {
        return mangled.to_owned();
    }

    // rtti.cpp:59-62 — drop leading '.' then '?'. These are ASCII so byte
    // slicing is char-boundary-safe. After dropping, require >= 3 chars.
    let mut s = mangled;
    if let Some(rest) = s.strip_prefix('.') {
        s = rest;
    }
    if let Some(rest) = s.strip_prefix('?') {
        s = rest;
    }
    // QString::size() counts UTF-16 code units; inputs are ASCII so chars().count()
    // matches for the < 3 guard.
    if s.chars().count() < 3 {
        return mangled.to_owned();
    }

    // rtti.cpp:65 — skip the 2-char prefix ('A' kind char + class-kind V/U/W/X).
    // Robust char-boundary computation of the body start.
    let body_start = s
        .char_indices()
        .nth(2)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let body = &s[body_start..];

    // rtti.cpp:66-67 — terminator "@@".
    let term = match body.find("@@") {
        Some(t) => t,
        None => return mangled.to_owned(),
    };
    let segments = &body[..term];

    // rtti.cpp:69 — split('@', Qt::SkipEmptyParts).
    let mut parts: Vec<&str> = segments.split('@').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return mangled.to_owned();
    }

    // rtti.cpp:72 — reverse for outer::inner display.
    parts.reverse();
    parts.join("::")
}

/// `demangleItaniumName(mangled)` (`rtti.cpp:284`).
///
/// Itanium type-name demangler. The C++ prefers `__cxa_demangle` (GCC/Clang) and
/// falls back to a hand-rolled parser; on MSVC only the fallback exists. The port
/// keeps the same two-tier shape: a best-effort `cpp_demangle` attempt for full
/// `_Z…` symbols, then the always-correct hand-rolled parser for the bare type
/// forms (`3Foo`, `N3Bar3FooE`, `St9type_info`) that the RTTI walker produces.
/// Anything not understood passes through verbatim.
pub fn demangle_itanium_name(mangled: &str) -> String {
    if mangled.is_empty() {
        return String::new();
    }

    // Preferred path (cross-platform replacement for __cxa_demangle, rtti.cpp:287-302).
    // C++ tries `__cxa_demangle` FIRST on every input and only falls through to the
    // hand-rolled parser when it rejects. `cpp_demangle::Symbol::new` matches that
    // exactly: it parses full `_Z…` symbols AND the bare RTTI `__name` type forms
    // (`3Foo`, `N3Bar3FooE`, `St9type_info`, `Pi`, …) — and rejects non-mangle text
    // (`plain_text`, `not_a_mangle`), so verbatim passthrough is preserved. A
    // typeinfo-name demangles to just the type text, so we trim and return it.
    if let Ok(sym) = cpp_demangle::Symbol::new(mangled) {
        if let Ok(out) = sym.demangle() {
            let trimmed = out.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }

    // Hand-rolled fallback (rtti.cpp:304-349). Also the MSVC-build path in C++ (no
    // `__cxa_demangle` there). Index over bytes (ASCII mangle),
    // matching QString index/length semantics on these inputs.
    let s = mangled.as_bytes();
    let len = s.len();
    let mut parts: Vec<String> = Vec::new();
    let mut p: usize = 0;

    if s[0] == b'N' {
        // rtti.cpp:339-344 — nested name: N <segments...> E
        p = 1;
        while p < len && s[p] != b'E' {
            if !consume_segment(s, &mut p, &mut parts) {
                return mangled.to_owned();
            }
        }
        if p >= len || s[p] != b'E' {
            return mangled.to_owned();
        }
    } else {
        // rtti.cpp:345-347 — single segment.
        if !consume_segment(s, &mut p, &mut parts) {
            return mangled.to_owned();
        }
    }

    if parts.is_empty() {
        return mangled.to_owned();
    }
    parts.join("::")
}

/// `parseLength` lambda inside `demangleItaniumName` (`rtti.cpp:310-318`).
/// Reads a decimal length prefix; returns -1 when no digit was consumed.
fn parse_length(s: &[u8], pos: &mut usize) -> i32 {
    let mut n: i32 = 0;
    let mut any = false;
    while *pos < s.len() && s[*pos].is_ascii_digit() {
        n = n.wrapping_mul(10).wrapping_add((s[*pos] - b'0') as i32);
        *pos += 1;
        any = true;
    }
    if any {
        n
    } else {
        -1
    }
}

/// `consumeSegment` lambda inside `demangleItaniumName` (`rtti.cpp:323-336`).
fn consume_segment(s: &[u8], pos: &mut usize, parts: &mut Vec<String>) -> bool {
    if *pos >= s.len() {
        return false;
    }
    // "St" -> "std" shorthand segment.
    if s[*pos] == b'S' && *pos + 1 < s.len() && s[*pos + 1] == b't' {
        parts.push("std".to_owned());
        *pos += 2;
        return true;
    }
    let len_seg = parse_length(s, pos);
    if len_seg <= 0 || *pos + len_seg as usize > s.len() {
        return false;
    }
    let seg = &s[*pos..*pos + len_seg as usize];
    // ASCII mangle; from_utf8_lossy preserves the bytes as a String.
    parts.push(String::from_utf8_lossy(seg).into_owned());
    *pos += len_seg as usize;
    true
}

/// `humanizeSymbolName(mangled)` (`symbol_demangle.cpp:11`).
///
/// Dispatch façade. **Contract: returns "" when the input is already
/// human-readable / unchanged; a non-empty return means "use this instead of the
/// raw name".** The MSVC function/method path (C++ `UnDecorateSymbolName`, gated on
/// `Q_OS_WIN`) is replaced cross-platform by `msvc-demangler` — an upgrade that
/// keeps the empty-on-unchanged semantics.
pub fn humanize_symbol_name(mangled: &str) -> String {
    if mangled.is_empty() {
        return String::new();
    }

    // 1. MSVC RTTI type descriptor (symbol_demangle.cpp:16-21).
    if mangled.starts_with(".?A") || mangled.starts_with("?A") {
        let d = demangle_rtti_name(mangled);
        return if d != mangled { d } else { String::new() };
    }

    // 2. Itanium ABI (symbol_demangle.cpp:25-29).
    if mangled.starts_with("_Z") {
        let d = demangle_itanium_name(mangled);
        return if d != mangled && !d.is_empty() {
            d
        } else {
            String::new()
        };
    }

    // 3. MSVC function/method mangle ('?...', '_?...'). C++ Q_OS_WIN-only
    //    (symbol_demangle.cpp:33-48); ported cross-platform via msvc-demangler.
    let trimmed = mangled.strip_prefix('_').unwrap_or(mangled);
    if !trimmed.starts_with('?') {
        return String::new();
    }
    // Conceptually equal to the dbghelp UNDNAME_NAME_ONLY |
    // NO_ACCESS_SPECIFIERS | NO_THISTYPE | NO_RETURN_UDT_MODEL set; NAME_ONLY
    // already strips the signature so the *_THISTYPE / return flags are largely
    // subsumed, but we set the available closest matches for parity intent.
    use msvc_demangler::DemangleFlags;
    let flags = DemangleFlags::NAME_ONLY
        | DemangleFlags::NO_ACCESS_SPECIFIERS
        | DemangleFlags::NO_THISTYPE
        | DemangleFlags::NO_FUNCTION_RETURNS;
    match msvc_demangler::demangle(trimmed, flags) {
        Ok(out) if !out.is_empty() && out != trimmed && out != mangled => out,
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── demangleBasic (test_rtti.cpp:52) ──
    #[test]
    fn demangle_rtti_basic() {
        assert_eq!(demangle_rtti_name(".?AVFoo@@"), "Foo");
        assert_eq!(demangle_rtti_name(".?AUStruct@@"), "Struct");
    }

    // ── demangleNested (test_rtti.cpp:59) ──
    #[test]
    fn demangle_rtti_nested() {
        assert_eq!(demangle_rtti_name(".?AVBar@Foo@@"), "Foo::Bar");
        assert_eq!(demangle_rtti_name(".?AVZ@Y@X@@"), "X::Y::Z");
    }

    // ── demangleMalformed (test_rtti.cpp:66) ──
    #[test]
    fn demangle_rtti_malformed() {
        assert_eq!(demangle_rtti_name("plain_name"), "plain_name");
        assert_eq!(demangle_rtti_name(""), "");
    }

    // ── demangleItaniumSimple (test_rtti.cpp:161) ──
    #[test]
    fn demangle_itanium_simple() {
        assert_eq!(demangle_itanium_name("3Foo"), "Foo");
    }

    // ── demangleItaniumNested (test_rtti.cpp:165) ──
    #[test]
    fn demangle_itanium_nested() {
        assert_eq!(demangle_itanium_name("N3Bar3FooE"), "Bar::Foo");
    }

    // ── demangleItaniumStdShorthand (test_rtti.cpp:169) ──
    #[test]
    fn demangle_itanium_std_shorthand() {
        let out = demangle_itanium_name("St9type_info");
        assert!(out.ends_with("type_info"), "{out}");
    }

    // ── demangleItaniumPassthrough (test_rtti.cpp:173) ──
    #[test]
    fn demangle_itanium_passthrough() {
        assert_eq!(demangle_itanium_name("plain_text"), "plain_text");
    }

    // ── §TEST 1.7 humanize_symbol_name ──
    #[test]
    fn humanize_symbol_name_contract() {
        // empty -> empty
        assert_eq!(humanize_symbol_name(""), "");
        // MSVC RTTI descriptor -> demangled
        assert_eq!(humanize_symbol_name(".?AVFoo@@"), "Foo");
        // starts with ".?A" but no "@@" -> demangle_rtti returns verbatim ->
        // humanize returns "" (unchanged).
        assert_eq!(humanize_symbol_name(".?AVUnchanged"), "");
        // Itanium full symbol -> non-empty.
        assert!(!humanize_symbol_name("_Z3Foov").is_empty());
        // plain C name (no marker) -> "" (keep raw).
        assert_eq!(humanize_symbol_name("GetProcAddress"), "");
        // MSVC function mangle -> non-empty via msvc-demangler.
        assert!(!humanize_symbol_name("?foo@@YAXXZ").is_empty());
    }
}
