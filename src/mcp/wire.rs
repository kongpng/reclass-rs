//! JSON-RPC envelope construction + the small shared helpers (`parse_integer`,
//! `resolve_placeholder`, `make_text_result`, Qt-compatible JSON pretty-print
//! and `double` formatting).
//!
//! Maps `mcp_bridge.cpp:28-44` (`parseInteger`), `:245-294`
//! (`okReply`/`errReply`/`sendJson`/`makeTextResult`), and `:1157-1169`
//! (`resolvePlaceholder`). These are pure data helpers with no `Provider`/UI
//! dependency so they unit-test under `--no-default-features --features mcp`.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

/// Recursively rebuild `value` so every object's keys are in ascending order.
///
/// Qt's `QJsonObject` serializes keys sorted, so the C++ bridge emits
/// sorted-key JSON. serde_json's `Value` is normally a `BTreeMap` (already
/// sorted), BUT under the default (`ui`) feature set gpui transitively enables
/// `serde_json/preserve_order`, which switches `Value`'s backing map to an
/// insertion-ordered `IndexMap`. Cargo feature unification makes that global,
/// so our wire output would otherwise lose its sorted-key guarantee in the GUI
/// build. Normalizing explicitly makes the output deterministic regardless of
/// which map backend `serde_json` was compiled with. (For the `IndexMap`
/// backend, inserting keys in sorted order preserves that order on serialize;
/// for `BTreeMap` it is a no-op on ordering.)
fn sort_value_keys(value: Value) -> Value {
    match value {
        Value::Object(obj) => {
            let mut keys: Vec<String> = obj.keys().cloned().collect();
            keys.sort();
            let mut sorted = Map::new();
            let mut obj = obj;
            for k in keys {
                if let Some(v) = obj.remove(&k) {
                    sorted.insert(k, sort_value_keys(v));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(sort_value_keys).collect()),
        other => other,
    }
}

/// `okReply(id, result)` (`mcp_bridge.cpp:245-251`).
pub fn ok_reply(id: &Value, result: Value) -> Value {
    sort_value_keys(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `errReply(id, code, msg)` (`mcp_bridge.cpp:253-259`).
pub fn err_reply(id: &Value, code: i64, msg: &str) -> Value {
    sort_value_keys(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}))
}

/// `makeTextResult(text, isError)` (`mcp_bridge.cpp:284-294`). The `isError`
/// key is present ONLY when `is_error` is true (matches the C++).
pub fn make_text_result(text: &str, is_error: bool) -> Value {
    let mut r = json!({"content": [{"type": "text", "text": text}]});
    if is_error {
        r["isError"] = json!(true);
    }
    r
}

/// `parseInteger(v, defaultVal)` (`mcp_bridge.cpp:28-44`).
///
/// Tolerant integer parse: undefined/null → default; a string is trimmed and
/// parsed as hex if it starts with `0x`/`0X` (case-insensitive, dropping the
/// prefix) else base-10 (`QString::toLongLong`); a JSON number truncates toward
/// zero (`static_cast<int64_t>(double)`); anything else → default.
pub fn parse_integer(v: Option<&Value>, default: i64) -> i64 {
    match v {
        None | Some(Value::Null) => default,
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return default;
            }
            // QString::toLongLong with an explicit base: a leading 0x is only
            // honored on the hex branch. Mirror that by slicing the prefix.
            let lower = s.to_ascii_lowercase();
            if let Some(hex) = lower.strip_prefix("0x") {
                i64::from_str_radix(hex, 16).unwrap_or(default)
            } else {
                s.parse::<i64>().unwrap_or(default)
            }
        }
        // QJsonValue::isDouble() covers all JSON numbers; cast truncates.
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64).unwrap_or(default),
        _ => default,
    }
}

/// `resolvePlaceholder(ref, placeholderMap, ok)` (`mcp_bridge.cpp:1157-1169`).
///
/// Returns `(resolved, ok)`. A `$N` reference looks up the WHOLE `"$N"` key
/// (including the `$`) in the map → decimal id string + `ok=true`; an
/// unresolved placeholder returns the ref unchanged + `ok=false`. A non-`$`
/// string is returned as-is + `ok=true`.
pub fn resolve_placeholder(r: &str, map: &HashMap<String, u64>) -> (String, bool) {
    if r.starts_with('$') {
        match map.get(r) {
            Some(id) => (id.to_string(), true),
            None => (r.to_string(), false),
        }
    } else {
        (r.to_string(), true)
    }
}

/// Serialize `value` the way `QJsonDocument::toJson(QJsonDocument::Indented)`
/// does: 4-space indentation, keys sorted. Keys are sorted explicitly via
/// [`sort_value_keys`] so the output is deterministic even when `serde_json`
/// is compiled with `preserve_order` (which the default `ui`/gpui build pulls
/// in transitively). Used by the tool-text payloads that embed indented JSON
/// (`project.state`, `tree.search`, …).
pub fn qt_pretty(value: &Value) -> String {
    use serde::Serialize;
    use serde_json::ser::{PrettyFormatter, Serializer};
    let value = sort_value_keys(value.clone());
    let mut buf = Vec::new();
    let fmt = PrettyFormatter::with_indent(b"    ");
    let mut ser = Serializer::with_formatter(&mut buf, fmt);
    value.serialize(&mut ser).expect("serialize");
    String::from_utf8(buf).expect("utf8")
}

/// Mirror of `QString::number(double)` — the default `'g'`-format with 6
/// significant digits, used in `hex.read`'s f32/f64 interpretation lines.
///
/// Qt's default `QString::number(double)` is `QLocaleData::DFSignificantDigits`
/// with precision 6 (the same as C's `%g` with precision 6): the shorter of
/// `%e`/`%f`, trailing zeros stripped.
pub fn qt_number_double(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v < 0.0 {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    // C `%g` with precision 6.
    let s = format!("{:.6e}", v);
    // Decide between %e and %f the way printf %g does: use %e when the decimal
    // exponent is < -4 or >= precision (6).
    let exp = decimal_exponent(v);
    if exp < -4 || exp >= 6 {
        format_g_exp(v)
    } else {
        format_g_fixed(v, exp)
    }
    .unwrap_or(s)
}

fn decimal_exponent(v: f64) -> i32 {
    if v == 0.0 {
        return 0;
    }
    v.abs().log10().floor() as i32
}

fn format_g_fixed(v: f64, exp: i32) -> Option<String> {
    // precision after decimal point for %g fixed form = P - 1 - exp, P=6.
    let prec = (6 - 1 - exp).max(0) as usize;
    let s = format!("{:.*}", prec, v);
    Some(strip_trailing_zeros(&s))
}

fn format_g_exp(v: f64) -> Option<String> {
    // %e with precision P-1 = 5, then strip trailing zeros in the mantissa and
    // render the exponent like Qt (`e[+-]NN`, at least 2 digits).
    let s = format!("{:.5e}", v);
    let (mant, exp) = s.split_once('e')?;
    let mant = strip_trailing_zeros(mant);
    let exp_num: i32 = exp.parse().ok()?;
    let sign = if exp_num < 0 { '-' } else { '+' };
    Some(format!("{}e{}{:02}", mant, sign, exp_num.abs()))
}

fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_integer_cases() {
        assert_eq!(parse_integer(Some(&json!("0x10")), 0), 16);
        assert_eq!(parse_integer(Some(&json!("0X1F")), 0), 31);
        assert_eq!(parse_integer(Some(&json!("42")), 0), 42);
        assert_eq!(parse_integer(Some(&json!("  7 ")), 0), 7);
        assert_eq!(parse_integer(Some(&json!("")), 99), 99);
        assert_eq!(parse_integer(Some(&json!("zzz")), 99), 99);
        assert_eq!(parse_integer(Some(&json!(3.9)), 0), 3); // truncate toward 0
        assert_eq!(parse_integer(Some(&json!(-2.9)), 0), -2);
        assert_eq!(parse_integer(Some(&json!(5)), 0), 5);
        assert_eq!(parse_integer(Some(&Value::Null), 7), 7);
        assert_eq!(parse_integer(None, 7), 7);
        assert_eq!(parse_integer(Some(&json!(true)), 7), 7);
        assert_eq!(parse_integer(Some(&json!("-12")), 0), -12);
    }

    #[test]
    fn resolve_placeholder_cases() {
        let mut map = HashMap::new();
        map.insert("$0".to_string(), 100u64);
        assert_eq!(resolve_placeholder("$0", &map), ("100".to_string(), true));
        assert_eq!(resolve_placeholder("$9", &map), ("$9".to_string(), false));
        assert_eq!(resolve_placeholder("123", &map), ("123".to_string(), true));
    }

    #[test]
    fn make_text_result_shape() {
        let ok = make_text_result("hi", false);
        assert!(ok.get("isError").is_none());
        assert_eq!(ok["content"][0]["type"], "text");
        assert_eq!(ok["content"][0]["text"], "hi");
        let err = make_text_result("bad", true);
        assert_eq!(err["isError"], json!(true));
    }

    #[test]
    fn ok_err_reply_exact_bytes() {
        // sorted keys (serde default BTreeMap) == Qt's sorted-key output.
        let r = ok_reply(&json!(1), json!({"x": 2}));
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"id":1,"jsonrpc":"2.0","result":{"x":2}}"#
        );
        let e = err_reply(&json!("abc"), -32601, "Method not found: ");
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"error":{"code":-32601,"message":"Method not found: "},"id":"abc","jsonrpc":"2.0"}"#
        );
        // null id passthrough
        let n = err_reply(&Value::Null, -32700, "Parse error");
        assert_eq!(
            serde_json::to_string(&n).unwrap(),
            r#"{"error":{"code":-32700,"message":"Parse error"},"id":null,"jsonrpc":"2.0"}"#
        );
    }

    #[test]
    fn qt_number_double_format() {
        // Matches Qt's QString::number(double) defaults (%.6g semantics).
        assert_eq!(qt_number_double(1.0), "1");
        assert_eq!(qt_number_double(120.0), "120");
        assert_eq!(qt_number_double(1.5), "1.5");
        assert_eq!(qt_number_double(0.0), "0");
        assert_eq!(qt_number_double(3.14159), "3.14159");
        assert_eq!(qt_number_double(1234567.0), "1.23457e+06");
        assert_eq!(qt_number_double(0.0001), "0.0001");
        assert_eq!(qt_number_double(0.00001), "1e-05");
    }

    #[test]
    fn qt_pretty_4_space_indent() {
        let v = json!({"b": 1, "a": [1, 2]});
        let s = qt_pretty(&v);
        // sorted keys, 4-space indent.
        assert_eq!(
            s,
            "{\n    \"a\": [\n        1,\n        2\n    ],\n    \"b\": 1\n}"
        );
    }
}
