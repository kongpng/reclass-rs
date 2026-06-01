//! Memory-scanner panel — search controls above a virtualized results table,
//! wired to the [`crate::scanner`] engine over the active [`Provider`].
//!
//! Port of `src/scannerpanel.{h,cpp}` (widgets-dialogs.md §17). In Qt this was a
//! `QWidget` of input rows (mode/condition/type combos, value line edits, filter
//! checkboxes, Scan/Re-scan/Reset buttons, a progress bar + status line) above a
//! 2-column `QTableWidget` (Address, Value) with an `AddressDelegate` that dims
//! the leading-zero high bytes. Here the table is a gpui-component virtualized
//! [`DataTable`](gpui_component::table::DataTable) and the chrome is gpui-component
//! `Select`/`Input`/`Checkbox`/`Button`; the search engine is the already-ported
//! [`ScanEngine`](crate::scanner::ScanEngine) driven synchronously over the tab's
//! provider (the C++ `runValueScanAndWait`/`runPatternScanAndWait` path).
//!
//! Split (mirroring the foundation philosophy: gpui-free logic + a thin view):
//! - [`ScanMode`] — Signature vs Value (the C++ `m_modeCombo`).
//! - [`ScannerForm`] — the input reducer: which fields are visible/enabled for the
//!   current mode + condition (`onModeChanged` / `onConditionChanged`), the
//!   build-`ScanRequest` mapping (`buildRequest`), and value formatting
//!   (`formatValue` / `valueSize`). Pure + unit-tested headlessly.
//! - [`split_address_dim`] — the `AddressDelegate` dim/bright split (leading-zero
//!   prefix), unit-tested.
//! - [`ScanRow`] — one result row's display strings (address + formatted value).
//! - [`ScannerPanel`] — the gpui-component dock `Panel`: the control cluster +
//!   the `DataTable`, raising [`ScannerNav`] on a row activation (the C++
//!   `goToAddress` signal).
//!
//! Gated behind the `ui` feature.

use crate::scanner::{
    natural_alignment, parse_signature, serialize_value, value_size_for_type, ScanCondition,
    ScanRequest, ScanResult, ValueType,
};

/// The scan **mode** — the C++ `m_modeCombo` (Signature vs Value). The condition
/// combo's "Exact Sig" sentinel flips this to [`ScanMode::Signature`]
/// (`onConditionChanged`); every other condition implies [`ScanMode::Value`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ScanMode {
    /// IDA-style byte-pattern search (`parseSignature`); index 0.
    #[default]
    Signature,
    /// Typed value search (`serializeValue`); index 1.
    Value,
}

/// The dropdown row a [`ScanCondition`] (or the Signature sentinel) occupies in
/// the C++ condition combo (`scannerpanel.cpp:434-447`), in order. The first
/// entry is "Exact Value", the second the Signature sentinel; the rest map 1:1
/// to [`ScanCondition`] variants used in the UI.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CondEntry {
    /// A real value-scan condition.
    Value(ScanCondition),
    /// The "Exact Sig" sentinel — flips the panel into [`ScanMode::Signature`]
    /// (the C++ `-1` data sentinel).
    Signature,
}

impl CondEntry {
    /// The condition dropdown entries in display order (`scannerpanel.cpp` combo
    /// population). Mirrors the C++ exactly: Exact Value, Exact Sig, then the
    /// compare/range/delta conditions.
    pub fn entries() -> &'static [(CondEntry, &'static str)] {
        &[
            (CondEntry::Value(ScanCondition::ExactValue), "Exact Value"),
            (CondEntry::Signature, "Exact Sig"),
            (CondEntry::Value(ScanCondition::UnknownValue), "Unknown"),
            (CondEntry::Value(ScanCondition::Changed), "Changed"),
            (CondEntry::Value(ScanCondition::Unchanged), "Unchanged"),
            (CondEntry::Value(ScanCondition::Increased), "Increased"),
            (CondEntry::Value(ScanCondition::Decreased), "Decreased"),
            (CondEntry::Value(ScanCondition::BiggerThan), "Bigger Than"),
            (CondEntry::Value(ScanCondition::SmallerThan), "Smaller Than"),
            (CondEntry::Value(ScanCondition::Between), "Between"),
            (CondEntry::Value(ScanCondition::IncreasedBy), "Increased By"),
            (CondEntry::Value(ScanCondition::DecreasedBy), "Decreased By"),
        ]
    }
}

/// The value-type dropdown entries in display order (`scannerpanel.cpp:494-503`).
/// Order matters: the UI index maps onto the [`ValueType`] discriminant.
pub fn value_type_entries() -> &'static [(ValueType, &'static str)] {
    &[
        (ValueType::Int8, "int8"),
        (ValueType::Int16, "int16"),
        (ValueType::Int32, "int32"),
        (ValueType::Int64, "int64"),
        (ValueType::UInt8, "uint8"),
        (ValueType::UInt16, "uint16"),
        (ValueType::UInt32, "uint32"),
        (ValueType::UInt64, "uint64"),
        (ValueType::Float, "float"),
        (ValueType::Double, "double"),
    ]
}

/// Fast-Scan (alignment) dropdown values (`scannerpanel.cpp:528`): 1/4/8/16/32/64.
pub const FAST_SCAN_ALIGNMENTS: &[i32] = &[1, 4, 8, 16, 32, 64];

/// Which input field(s) a condition needs, mirroring `onConditionChanged`
/// (`scannerpanel.cpp:1151-1203`): whether the pattern field, value field, and
/// upper-bound (value2) field are visible/enabled, plus whether the value-type
/// combo applies.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FieldVisibility {
    /// Pattern (signature) input shown — Signature mode only.
    pub pattern_visible: bool,
    /// Value input shown — every non-signature condition.
    pub value_visible: bool,
    /// Value input enabled (a value is actually required by the condition).
    pub value_enabled: bool,
    /// Upper-bound (value2) input shown — `Between` (value mode) only.
    pub value2_visible: bool,
    /// Value-type combo applies (value mode only).
    pub type_enabled: bool,
    /// The label for the (shared) value row: "Pattern:" vs "Value:".
    pub value_label: &'static str,
}

/// The scanner control reducer — the gpui-free heart of the panel.
///
/// Holds the form state the C++ kept across its widgets (`m_modeCombo`,
/// `m_condCombo`, `m_typeCombo`, the value strings, the filter checkboxes), and
/// the "last scan" snapshot used to format result values (`m_lastScanMode`,
/// `m_lastValueType`, `m_lastPattern`). Feed it the picked condition / typed
/// strings; read [`field_visibility`](ScannerForm::field_visibility) for the view
/// and call [`build_request`](ScannerForm::build_request) to produce a
/// [`ScanRequest`] for the engine.
#[derive(Clone, Debug)]
pub struct ScannerForm {
    /// Selected condition entry (drives mode + field visibility).
    pub condition: CondEntry,
    /// Selected value type (value mode).
    pub value_type: ValueType,
    /// The pattern / value input text (the C++ shared value row).
    pub value_text: String,
    /// The upper-bound text (`Between`).
    pub value2_text: String,
    /// Fast-Scan alignment stride (floored at the type's natural alignment).
    pub alignment: i32,
    // ── Filter checkboxes (`buildRequest`) ──
    pub filter_executable: bool,
    pub filter_writable: bool,
    pub private_only: bool,
    pub skip_system_modules: bool,
    pub user_mode_only: bool,
    pub struct_only: bool,
    // ── "last scan" snapshot (drives value formatting) ──
    last_mode: ScanMode,
    last_value_type: ValueType,
    last_pattern: Vec<u8>,
}

impl Default for ScannerForm {
    fn default() -> Self {
        ScannerForm {
            condition: CondEntry::Value(ScanCondition::ExactValue),
            value_type: ValueType::Int32,
            value_text: String::new(),
            value2_text: String::new(),
            alignment: 1,
            filter_executable: false,
            filter_writable: false,
            private_only: false,
            skip_system_modules: false,
            user_mode_only: false,
            struct_only: false,
            last_mode: ScanMode::Value,
            last_value_type: ValueType::Int32,
            last_pattern: Vec::new(),
        }
    }
}

impl ScannerForm {
    /// A fresh form (Exact Value, int32, no filters).
    pub fn new() -> Self {
        ScannerForm::default()
    }

    /// The effective [`ScanMode`] for the current condition (the C++
    /// `onConditionChanged` mode flip): Signature for the sentinel, else Value.
    pub fn mode(&self) -> ScanMode {
        match self.condition {
            CondEntry::Signature => ScanMode::Signature,
            CondEntry::Value(_) => ScanMode::Value,
        }
    }

    /// The real [`ScanCondition`] the condition entry maps to (Signature folds to
    /// `ExactValue`, as the C++ does before `buildRequest`).
    pub fn effective_condition(&self) -> ScanCondition {
        match self.condition {
            CondEntry::Signature => ScanCondition::ExactValue,
            CondEntry::Value(c) => c,
        }
    }

    /// `onConditionChanged` field-visibility logic (`scannerpanel.cpp:1151-1203`).
    pub fn field_visibility(&self) -> FieldVisibility {
        let is_sig = self.mode() == ScanMode::Signature;
        let cond = self.effective_condition();
        let needs_value = is_sig
            || matches!(
                cond,
                ScanCondition::ExactValue
                    | ScanCondition::BiggerThan
                    | ScanCondition::SmallerThan
                    | ScanCondition::Between
                    | ScanCondition::IncreasedBy
                    | ScanCondition::DecreasedBy
            );
        let needs_range = cond == ScanCondition::Between && !is_sig;
        FieldVisibility {
            pattern_visible: is_sig,
            value_visible: !is_sig,
            value_enabled: needs_value,
            value2_visible: needs_range,
            type_enabled: !is_sig,
            value_label: if is_sig { "Pattern:" } else { "Value:" },
        }
    }

    /// `buildRequest()` (`scannerpanel.cpp:1256-1345`).
    ///
    /// Maps the form into a [`ScanRequest`], or `Err(message)` with the exact
    /// upstream status string on a parse failure. `pointer_size` supplies the
    /// user-mode VA cap; `struct_bounds` (start,size) supplies the struct-only
    /// range. Also records the "last scan" snapshot used by [`format_value`].
    pub fn build_request(
        &mut self,
        pointer_size: i32,
        struct_bounds: Option<(u64, u64)>,
    ) -> Result<ScanRequest, String> {
        let mut req = ScanRequest::default();

        if self.mode() == ScanMode::Signature {
            // Signature mode.
            let (pat, mask) =
                parse_signature(&self.value_text).map_err(|e| format!("Pattern error: {e}"))?;
            req.pattern = pat;
            req.mask = mask;
            req.alignment = 1;
        } else {
            // Value mode.
            let vt = self.value_type;
            let mut cond = self.effective_condition();

            // Compare-against-previous conditions need a baseline; on first scan
            // capture every aligned address (UnknownValue), the real comparison
            // is applied on the next Re-scan.
            if matches!(
                cond,
                ScanCondition::Changed
                    | ScanCondition::Unchanged
                    | ScanCondition::Increased
                    | ScanCondition::Decreased
                    | ScanCondition::IncreasedBy
                    | ScanCondition::DecreasedBy
            ) {
                cond = ScanCondition::UnknownValue;
            }

            req.condition = cond;
            let nat_align = natural_alignment(vt);
            let chosen = self.alignment.max(1);
            req.alignment = chosen.max(nat_align);
            req.value_size = value_size_for_type(vt);
            req.value_type = vt;

            if cond == ScanCondition::UnknownValue {
                req.max_results = 10_000_000;
            } else if matches!(
                cond,
                ScanCondition::BiggerThan | ScanCondition::SmallerThan | ScanCondition::Between
            ) {
                let (pat, _dummy) = serialize_value(vt, &self.value_text)
                    .map_err(|e| format!("Value error: {e}"))?;
                req.pattern = pat;
                if cond == ScanCondition::Between {
                    let (pat2, _d2) = serialize_value(vt, &self.value2_text)
                        .map_err(|e| format!("Upper bound error: {e}"))?;
                    req.pattern2 = pat2;
                }
                req.mask = vec![0xFFu8; req.pattern.len()];
            } else {
                // Exact value mode.
                let (pat, mask) = serialize_value(vt, &self.value_text)
                    .map_err(|e| format!("Value error: {e}"))?;
                req.pattern = pat;
                req.mask = mask;
            }
        }

        req.filter_executable = self.filter_executable;
        req.filter_writable = self.filter_writable;
        req.private_only = self.private_only;
        req.skip_system_modules = self.skip_system_modules;

        // User-mode VA cap (pointer size from the live provider).
        if self.user_mode_only {
            let cap: u64 = if pointer_size >= 8 {
                0x0000_7FFF_FFFF_FFFF
            } else {
                0x7FFF_FFFF
            };
            if req.end_address == 0 || req.end_address > cap {
                req.end_address = cap;
            }
        }

        if self.struct_only {
            if let Some((start, size)) = struct_bounds {
                if size > 0 {
                    req.start_address = start;
                    req.end_address = start + size;
                }
            }
        }

        // Record the "last scan" snapshot for result-value formatting.
        self.last_mode = self.mode();
        if self.last_mode == ScanMode::Value {
            self.last_value_type = self.value_type;
        }
        self.last_pattern = req.pattern.clone();

        Ok(req)
    }

    /// `formatValue(bytes)` (`scannerpanel.cpp:2145-2178`) — render a result's
    /// cached bytes for the Value column, using the **last scan** mode + type.
    pub fn format_value(&self, bytes: &[u8]) -> String {
        format_value(
            self.last_mode,
            self.last_value_type,
            &self.last_pattern,
            bytes,
        )
    }
}

/// `formatValue` (`scannerpanel.cpp:2145-2178`), as a free function over the
/// last-scan snapshot so it is testable without a `ScannerForm`.
///
/// Signature mode shows the matched bytes as `"AB CD .."` (uppercase, capped at
/// the searched pattern length). Value mode decodes the LE bytes as the typed
/// value; too-few bytes yields `"??"`.
pub fn format_value(mode: ScanMode, vt: ValueType, last_pattern: &[u8], bytes: &[u8]) -> String {
    if mode == ScanMode::Signature {
        // Show only as many bytes as the user searched for (cap at pattern len).
        let show_len = if last_pattern.is_empty() {
            bytes.len()
        } else {
            bytes.len().min(last_pattern.len())
        };
        let mut s = String::new();
        for (j, b) in bytes.iter().take(show_len).enumerate() {
            if j > 0 {
                s.push(' ');
            }
            s.push_str(&format!("{b:02X}"));
        }
        return s;
    }

    let sz = bytes.len();
    macro_rules! decode {
        ($t:ty, $w:expr) => {{
            if sz >= $w {
                let v = <$t>::from_le_bytes(bytes[..$w].try_into().unwrap());
                return v.to_string();
            }
        }};
    }
    match vt {
        ValueType::Int8 => decode!(i8, 1),
        ValueType::UInt8 => decode!(u8, 1),
        ValueType::Int16 => decode!(i16, 2),
        ValueType::UInt16 => decode!(u16, 2),
        ValueType::Int32 => decode!(i32, 4),
        ValueType::UInt32 => decode!(u32, 4),
        ValueType::Int64 => decode!(i64, 8),
        ValueType::UInt64 => decode!(u64, 8),
        ValueType::Float if sz >= 4 => {
            let v = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            return format_float(v as f64, 9);
        }
        ValueType::Double if sz >= 8 => {
            let v = f64::from_le_bytes(bytes[..8].try_into().unwrap());
            return format_float(v, 17);
        }
        _ => {}
    }
    "??".to_string()
}

/// `QString::number(double, 'g', prec)` — shortest `%g`-style rendering at the
/// given significant-digit precision (Qt default `%g` semantics).
fn format_float(v: f64, _prec: usize) -> String {
    // Rust's default f64 Display already produces the shortest round-trippable
    // form; for the scanner's display purposes this matches Qt's 'g' output for
    // the common cases (whole numbers, simple decimals) closely enough.
    let s = format!("{v}");
    s
}

/// The C++ `AddressDelegate::paint` dim/bright split (`scannerpanel.cpp:204-236`):
/// split a formatted address string at the first **significant** hex digit so the
/// leading-zero prefix (and any backtick separator) can be painted dimmed and the
/// remainder bright. Returns `(dim_prefix, bright_rest)`.
///
/// Walks from the left: a backtick is absorbed into the dim prefix and skipped;
/// a `'0'` extends the dim prefix; any other character stops the scan. An
/// all-zero address keeps its final `'0'` bright (the loop breaks before the
/// last char only if a non-zero follows — for all-zeros the whole string except
/// nothing is dim, so the last `'0'` is the break point), matching the C++
/// `dimEnd` advancing past every leading zero.
pub fn split_address_dim(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut dim_end = 0usize;
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'`' {
            dim_end = i + 1;
            continue;
        }
        if c != b'0' {
            break;
        }
        dim_end = i + 1;
    }
    // `dim_end` is a byte index into an ASCII-only hex string, so slicing is safe.
    text.split_at(dim_end)
}

/// Format an address the way the result table shows it: 16 hex digits with a
/// backtick between the high and low dwords (`"00007FF6`12340000"`), uppercase —
/// matching the C++ column width hint `"00000000`00000000"`.
pub fn format_scan_address(addr: u64) -> String {
    let hi = (addr >> 32) as u32;
    let lo = (addr & 0xFFFF_FFFF) as u32;
    format!("{hi:08X}`{lo:08X}")
}

/// One result row's display strings — the Address column (with backtick), the
/// formatted Value column, and the Previous column (the value before the last
/// rescan, the C++ third column in PIC6). Built from a [`ScanResult`] + the
/// form's last-scan snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRow {
    /// Absolute address (the row's stable key + goto target).
    pub address: u64,
    /// The formatted address string (`format_scan_address`).
    pub address_text: String,
    /// The formatted value string (`ScannerForm::format_value`).
    pub value_text: String,
    /// The formatted previous-value string (empty before any rescan).
    pub previous_text: String,
}

impl ScanRow {
    /// Build a row from a result + the form (for the value formatting). The
    /// Previous column is the prior value snapshot a rescan records on the
    /// [`ScanResult`] (`previous_value`); empty on a first scan.
    pub fn from_result(form: &ScannerForm, r: &ScanResult) -> ScanRow {
        ScanRow {
            address: r.address,
            address_text: format_scan_address(r.address),
            value_text: form.format_value(&r.scan_value),
            previous_text: if r.previous_value.is_empty() {
                String::new()
            } else {
                form.format_value(&r.previous_value)
            },
        }
    }
}

/// Filter the displayed rows by a post-scan text query (the C++
/// `applyResultFilter` / `m_resultFilter`): case-insensitive substring match
/// against the address text OR the value text. An empty query keeps everything.
pub fn filter_rows<'a>(rows: &'a [ScanRow], query: &str) -> Vec<&'a ScanRow> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return rows.iter().collect();
    }
    rows.iter()
        .filter(|r| {
            r.address_text.to_lowercase().contains(&q) || r.value_text.to_lowercase().contains(&q)
        })
        .collect()
}

/// The displayed-row cap (the C++ `kMaxRows`): the results table only ever
/// shows the first 10,000 rows, with a banner above when the real result count
/// exceeds it.
pub const MAX_DISPLAY_ROWS: usize = 10_000;

/// The signed delta between two cached value byte-slices, decoded as `vt`
/// (the C++ `computeDelta`, `scannerpanel.cpp:2099-2137`). Returns the
/// rendered delta text (`"+N"` / `"-N"`), the direction (`1`/`0`/`-1` for
/// up/none/down), and whether the decode succeeded. A too-short / non-numeric
/// value yields `ok == false` so the caller can degrade to a plain "→" arrow.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DeltaInfo {
    /// The rendered delta (`"+3"`, `"-12"`, `"+0.5"`); empty when `!ok`.
    pub text: String,
    /// `1` = increase, `-1` = decrease, `0` = unchanged.
    pub direction: i32,
    /// Whether the value type decoded to a numeric delta.
    pub ok: bool,
}

/// `computeDelta(vt, prev, cur)` (`scannerpanel.cpp:2099-2137`).
pub fn compute_delta(vt: ValueType, prev: &[u8], cur: &[u8]) -> DeltaInfo {
    let mut d = DeltaInfo::default();
    if prev.is_empty() || cur.is_empty() {
        return d;
    }
    let sign = |v: f64| -> i32 {
        if v > 0.0 {
            1
        } else if v < 0.0 {
            -1
        } else {
            0
        }
    };
    let mut fmt_int = |delta: i64| {
        d.direction = sign(delta as f64);
        d.text = if delta >= 0 {
            format!("+{delta}")
        } else {
            // The negative sign is already in the formatted integer.
            format!("{delta}")
        };
        d.ok = true;
    };
    macro_rules! int_delta {
        ($t:ty, $w:expr) => {{
            if prev.len() >= $w && cur.len() >= $w {
                let a = <$t>::from_le_bytes(prev[..$w].try_into().unwrap()) as i64;
                let b = <$t>::from_le_bytes(cur[..$w].try_into().unwrap()) as i64;
                fmt_int(b - a);
                return d;
            }
        }};
    }
    match vt {
        ValueType::Int8 => int_delta!(i8, 1),
        ValueType::UInt8 => int_delta!(u8, 1),
        ValueType::Int16 => int_delta!(i16, 2),
        ValueType::UInt16 => int_delta!(u16, 2),
        ValueType::Int32 => int_delta!(i32, 4),
        ValueType::UInt32 => int_delta!(u32, 4),
        ValueType::Int64 => int_delta!(i64, 8),
        ValueType::UInt64 => {
            // u64 wraps via i64 to match the C++ `(long long)(int64_t)(b - a)`.
            if prev.len() >= 8 && cur.len() >= 8 {
                let a = u64::from_le_bytes(prev[..8].try_into().unwrap());
                let b = u64::from_le_bytes(cur[..8].try_into().unwrap());
                fmt_int(b.wrapping_sub(a) as i64);
                return d;
            }
        }
        ValueType::Float => {
            if prev.len() >= 4 && cur.len() >= 4 {
                let a = f32::from_le_bytes(prev[..4].try_into().unwrap()) as f64;
                let b = f32::from_le_bytes(cur[..4].try_into().unwrap()) as f64;
                let delta = b - a;
                d.direction = sign(delta);
                let num = format_float(delta, 6);
                d.text = if delta >= 0.0 { format!("+{num}") } else { num };
                d.ok = true;
                return d;
            }
        }
        ValueType::Double => {
            if prev.len() >= 8 && cur.len() >= 8 {
                let a = f64::from_le_bytes(prev[..8].try_into().unwrap());
                let b = f64::from_le_bytes(cur[..8].try_into().unwrap());
                let delta = b - a;
                d.direction = sign(delta);
                let num = format_float(delta, 6);
                d.text = if delta >= 0.0 { format!("+{num}") } else { num };
                d.ok = true;
                return d;
            }
        }
        _ => {}
    }
    d // ok == false: caller falls back to byte-equality "→" arrow
}

/// The Previous-column text for a row showing a delta (the C++ `"<prev>  →  <Δ>"`
/// composition, `scannerpanel.cpp:1581-1590`). Empty when there is no previous
/// value; a bare `"<prev>  →"` arrow when the value changed but is non-numeric.
pub fn previous_delta_text(prev_text: &str, delta: &DeltaInfo, changed: bool) -> String {
    if prev_text.is_empty() {
        return String::new();
    }
    if delta.ok {
        format!("{prev_text}  →  {}", delta.text)
    } else if changed {
        format!("{prev_text}  →")
    } else {
        prev_text.to_string()
    }
}

/// The status line after a Re-scan (`scannerpanel.cpp:1716-1735`): the
/// `Narrowed N → M (eliminated K)` / `All N still match` / `M results` variants.
/// `before` is the pre-rescan count (0 = no narrowing context).
pub fn rescan_status(before: usize, after: usize) -> String {
    if after == 0 {
        "0 results — the condition eliminated everything. Click Reset to start over, \
         or relax the condition and try again."
            .to_string()
    } else if before > 0 && after < before {
        format!(
            "Narrowed {before} → {after}  (eliminated {})",
            before - after
        )
    } else if before > 0 && after == before {
        format!("All {after} results still match")
    } else if after == 1 {
        "1 result".to_string()
    } else {
        format!("{after} results")
    }
}

/// The scan-generation breadcrumb (`scannerpanel.cpp:updateStageLabel`,
/// 2256-2328) reduced to plain text: the "Step N — phase: count" stage line
/// shown next to the action buttons. `generation` is 0 (idle), 1 (first scan),
/// or ≥2 (re-scan); `before`/`after` give the narrowed counts on a re-scan.
pub fn stage_breadcrumb(generation: u32, before: usize, after: usize) -> String {
    match generation {
        0 => String::new(),
        1 => {
            if after == 0 {
                "Step 1 — First scan: 0 results".to_string()
            } else if after == 1 {
                "Step 1 — First scan: 1 result".to_string()
            } else {
                format!("Step 1 — First scan: {after} results")
            }
        }
        g => {
            if after == 0 {
                format!("Step {g} — 0 results (condition eliminated everything)")
            } else if before > 0 && after < before {
                format!(
                    "Step {g} — Narrowed {before} → {after} (eliminated {})",
                    before - after
                )
            } else if before > 0 && after == before {
                format!("Step {g} — All {after} results still match")
            } else if after == 1 {
                format!("Step {g} — 1 result")
            } else {
                format!("Step {g} — {after} results")
            }
        }
    }
}

/// The truncation banner text when the result count exceeds [`MAX_DISPLAY_ROWS`]
/// (the C++ `m_truncBanner`, `scannerpanel.cpp:1485-1488`). Returns `None` when
/// no banner is needed.
pub fn truncation_banner(total: usize) -> Option<String> {
    if total > MAX_DISPLAY_ROWS {
        Some(format!(
            "Showing first {MAX_DISPLAY_ROWS} of {total} results — narrow the scan to see fewer."
        ))
    } else {
        None
    }
}

/// Serialize a result list to the C++ scanner JSON shape (`saveResultsTo`,
/// `scannerpanel.cpp:2386-2406`): `{version, scanMode, valueType, count,
/// results:[{address, value, module?}]}` with hex address + hex-encoded bytes.
pub fn serialize_results_json(
    last_mode: ScanMode,
    last_value_type: ValueType,
    rows: &[(u64, Vec<u8>, String)],
) -> String {
    fn hex_bytes(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
    let mut entries = String::new();
    for (i, (addr, value, module)) in rows.iter().enumerate() {
        if i > 0 {
            entries.push(',');
        }
        entries.push_str(&format!(
            "{{\"address\":\"{addr:x}\",\"value\":\"{}\"",
            hex_bytes(value)
        ));
        if !module.is_empty() {
            entries.push_str(&format!(",\"module\":\"{module}\""));
        }
        entries.push('}');
    }
    let mode = if last_mode == ScanMode::Signature {
        0
    } else {
        1
    };
    format!(
        "{{\"version\":1,\"scanMode\":{mode},\"valueType\":{},\"count\":{},\"results\":[{entries}]}}",
        last_value_type as i32,
        rows.len()
    )
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{ScannerEdit, ScannerNav, ScannerPanel};

#[cfg(feature = "ui")]
mod view {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::checkbox::Checkbox;
    use gpui_component::dock::{Panel, PanelEvent};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::popover::Popover;
    use gpui_component::table::{
        Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
    };
    use gpui_component::{Disableable as _, Sizable as _};

    use super::{
        compute_delta, previous_delta_text, rescan_status, serialize_results_json,
        split_address_dim, stage_breadcrumb, truncation_banner, value_type_entries, CondEntry,
        ScanMode, ScanRow, ScannerForm, FAST_SCAN_ALIGNMENTS, MAX_DISPLAY_ROWS,
    };
    use crate::provider::Provider;
    use crate::scanner::{
        run_rescan, run_scan, serialize_value, value_size_for_type, NullObserver, ScanCondition,
        ScanResult, ValueType,
    };
    use crate::ui::design::{color, tokens};

    /// A "go to this address" request raised when a result row is activated
    /// (double-click / Enter) — the C++ `goToAddress(uint64_t)` signal. The window
    /// resolves it onto the active document (set view root / scroll to address).
    #[derive(Clone, Copy, Debug)]
    pub struct ScannerNav {
        pub address: u64,
    }

    /// An inline-edit intent raised when a result cell is double-clicked (the
    /// C++ `onCellEdited`). The provider held here is read-only (`Arc`, no `&mut`
    /// for `write`), and address-expression evaluation lives in the controller's
    /// parser, so the panel raises the intent for the window to resolve against
    /// its mutable source — the read-only-surface pattern bookmarks use.
    #[derive(Clone, Debug)]
    pub enum ScannerEdit {
        /// Re-evaluate the Address cell as an address expression, then move the
        /// row to + navigate to the result (the C++ col-0 edit). `row` indexes
        /// into the panel's result list.
        EvalAddress { row: usize },
        /// Write the Value cell back to memory at the row's address (the C++
        /// col-1 edit). `row` indexes into the panel's result list.
        WriteValue { row: usize, address: u64 },
    }

    /// The result columns. The C++ scanner table is 2 columns (Address, Value);
    /// PIC6 additionally surfaces a Previous→Δ column (the pre-rescan value with
    /// its signed delta) and a Module column (when any result falls in a known
    /// module), so the port shows up to four: Address / Value / Previous / Module.
    const COL_ADDRESS: usize = 0;
    const COL_VALUE: usize = 1;
    const COL_PREVIOUS: usize = 2;

    /// One displayed row plus its render metadata — the formatted strings, the
    /// per-row delta direction (drives green/red coloring on the Value + Previous
    /// columns, the C++ `computeDelta` foreground), and the module name (the
    /// C++ `regionModule`, shown only when present).
    #[derive(Clone, Debug)]
    pub(super) struct DisplayRow {
        pub(super) row: ScanRow,
        /// `1` = increase (green), `-1` = decrease (red), `0` = none.
        pub(super) delta_dir: i32,
        /// The module the address falls inside (empty when unknown).
        pub(super) module: String,
    }

    /// The [`TableDelegate`] backing the results [`DataTable`]: owns the displayed
    /// rows and paints the Address cell with the dimmed leading-zero prefix (the
    /// C++ `AddressDelegate`). Sorting is wired via [`TableDelegate::perform_sort`]
    /// over the address (the C++ sortable header). `show_previous` mirrors the C++
    /// `populateTable(showPrevious)` — the Previous→Δ column only appears after a
    /// re-scan; `show_module` appears when any row has a module name.
    struct ScanResultsDelegate {
        rows: Vec<DisplayRow>,
        show_previous: bool,
        show_module: bool,
    }

    impl ScanResultsDelegate {
        fn new() -> Self {
            ScanResultsDelegate {
                rows: Vec::new(),
                show_previous: false,
                show_module: false,
            }
        }
    }

    impl TableDelegate for ScanResultsDelegate {
        fn columns_count(&self, _cx: &App) -> usize {
            let base = if self.show_previous { 3 } else { 2 };
            base + if self.show_module { 1 } else { 0 }
        }

        fn rows_count(&self, _cx: &App) -> usize {
            self.rows.len()
        }

        fn column(&self, col_ix: usize, _cx: &App) -> Column {
            // When the Previous column is hidden the Module column slides into
            // slot 2; resolve the logical column for the physical index.
            let module_ix = if self.show_previous { 3 } else { 2 };
            if col_ix == module_ix && self.show_module {
                return Column::new("module", "Module").width(px(140.)).sortable();
            }
            match col_ix {
                COL_VALUE => Column::new("value", "Value").width(px(160.)).sortable(),
                COL_PREVIOUS => Column::new("previous", "Previous → Δ")
                    .width(px(180.))
                    .sortable(),
                _ => Column::new("address", "Address").width(px(176.)).sortable(),
            }
        }

        fn perform_sort(
            &mut self,
            col_ix: usize,
            sort: ColumnSort,
            _window: &mut Window,
            _cx: &mut Context<TableState<Self>>,
        ) {
            let asc = !matches!(sort, ColumnSort::Descending);
            let module_ix = if self.show_previous { 3 } else { 2 };
            if col_ix == module_ix && self.show_module {
                self.rows.sort_by(|a, b| a.module.cmp(&b.module));
            } else {
                match col_ix {
                    COL_ADDRESS => self.rows.sort_by_key(|r| r.row.address),
                    COL_PREVIOUS => self
                        .rows
                        .sort_by(|a, b| a.row.previous_text.cmp(&b.row.previous_text)),
                    _ => self
                        .rows
                        .sort_by(|a, b| a.row.value_text.cmp(&b.row.value_text)),
                }
            }
            if !asc {
                self.rows.reverse();
            }
        }

        fn render_td(
            &mut self,
            row_ix: usize,
            col_ix: usize,
            _window: &mut Window,
            cx: &mut Context<TableState<Self>>,
        ) -> impl IntoElement {
            let Some(d) = self.rows.get(row_ix) else {
                return div();
            };
            let row = &d.row;
            let module_ix = if self.show_previous { 3 } else { 2 };
            // Green/red direction tint (the C++ `#7BC97B` / `#E07B7B`): the
            // theme greens an increase, reds a decrease.
            let delta_color = |cx: &App| -> Option<gpui::Hsla> {
                use gpui_component::ActiveTheme as _;
                match d.delta_dir {
                    n if n > 0 => Some(cx.theme().green),
                    n if n < 0 => Some(cx.theme().red),
                    _ => None,
                }
            };
            if col_ix == COL_ADDRESS {
                // Dim the leading-zero prefix, bright the rest (AddressDelegate):
                // the C++ `AddressDelegate` paints the leading-zero high bytes in
                // a faint color so the significant address digits read first.
                let (dim, bright) = split_address_dim(&row.address_text);
                div()
                    .font_family(tokens::font::MONO_FAMILY)
                    .text_size(px(tokens::font::EDITOR_SIZE))
                    .child(
                        div()
                            .flex()
                            .child(
                                div()
                                    .text_color(color::text_muted(cx))
                                    .child(dim.to_string()),
                            )
                            .child(div().text_color(color::text(cx)).child(bright.to_string())),
                    )
            } else if col_ix == module_ix && self.show_module {
                // Module column: muted truncating name.
                div()
                    .w_full()
                    .truncate()
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::text_muted(cx))
                    .child(d.module.clone())
            } else {
                // Numeric / hex value column: monospace, right-aligned so digits
                // line up the way the original scanner table presents them. The
                // Value + Previous columns carry the green/red direction tint.
                let muted = col_ix == COL_PREVIOUS;
                let text = if col_ix == COL_PREVIOUS {
                    row.previous_text.clone()
                } else {
                    row.value_text.clone()
                };
                let tint = delta_color(cx);
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .font_family(tokens::font::MONO_FAMILY)
                    .text_size(px(tokens::font::EDITOR_SIZE))
                    .when_some(tint, |dv, c| dv.text_color(c))
                    .when(tint.is_none() && muted, |dv| {
                        dv.text_color(color::text_muted(cx))
                    })
                    .when(tint.is_none() && !muted, |dv| {
                        dv.text_color(color::text(cx))
                    })
                    .child(text)
            }
        }

        fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
            let Some(d) = self.rows.get(row_ix) else {
                return String::new();
            };
            let module_ix = if self.show_previous { 3 } else { 2 };
            if col_ix == module_ix && self.show_module {
                return d.module.clone();
            }
            match col_ix {
                COL_ADDRESS => d.row.address_text.clone(),
                COL_PREVIOUS => d.row.previous_text.clone(),
                _ => d.row.value_text.clone(),
            }
        }
    }

    /// The memory-scanner dock panel — search controls above a virtualized
    /// results [`DataTable`].
    ///
    /// Owns the [`ScannerForm`] reducer, the value/filter [`InputState`]s, the
    /// [`TableState`], and the currently-attached [`Provider`] (the active tab's
    /// source — set via [`set_provider`](ScannerPanel::set_provider)). Scans run
    /// synchronously over that provider via [`run_scan`] (the C++
    /// `runValueScanAndWait` path); the full async [`ScanEngine`] worker is wired
    /// when background scanning lands.
    pub struct ScannerPanel {
        form: ScannerForm,
        value_input: Entity<InputState>,
        value2_input: Entity<InputState>,
        filter_input: Entity<InputState>,
        table: Entity<TableState<ScanResultsDelegate>>,
        /// The live result list (the C++ `m_results`) — the raw [`ScanResult`]s
        /// the Re-scan / inline-edit paths re-read + filter. The table delegate
        /// holds the formatted, filtered display view derived from these.
        results: Vec<ScanResult>,
        /// Whether a re-scan has run yet — drives the Previous→Δ column
        /// (the C++ `populateTable(showPrevious)`).
        show_previous: bool,
        /// The undo stack of pre-rescan result snapshots (the C++ `m_undoStack`),
        /// capped at [`MAX_UNDO`] so a long narrowing chain stays bounded.
        undo_stack: Vec<Vec<ScanResult>>,
        /// The scan generation (0 = idle, 1 = first scan, ≥2 = re-scan) — drives
        /// the stage breadcrumb (the C++ `m_scanGeneration`).
        generation: u32,
        /// The pre-rescan count, for the "Narrowed N → M" status (the C++
        /// `m_lastResultCount`).
        last_result_count: usize,
        provider: Option<Arc<dyn Provider + Send + Sync>>,
        /// The attached source's display name, for the floating dock title
        /// ("Memory Scanner — notepad.exe (Process)"); empty when none.
        source_title: Option<String>,
        status: String,
        /// The currently-selected result row (drives the footer goto/copy
        /// buttons), tracked from [`TableEvent::SelectRow`].
        selected_row: Option<usize>,
        /// `true` while a scan/rescan is running off-thread — shows the progress
        /// bar + Cancel control (the C++ `m_progressBar` / Esc cancel).
        scanning: bool,
        /// Scan progress 0..=100, pushed from the off-thread observer.
        progress: i32,
        /// The flippable abort flag a running scan polls (the C++ `m_abort`);
        /// the Cancel / Esc control sets it.
        abort: Arc<AtomicBool>,
        /// Controlled-open state for the toolbar dropdown popovers (scan-type /
        /// condition / value-type / fast-scan), so picking an item dismisses the
        /// popover.
        mode_open: bool,
        cond_open: bool,
        type_open: bool,
        align_open: bool,
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

    /// The undo-stack cap (the C++ `kMaxUndo`).
    const MAX_UNDO: usize = 16;

    impl ScannerPanel {
        /// Build an empty scanner panel.
        pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let value_input =
                cx.new(|cx| InputState::new(window, cx).placeholder("value / pattern"));
            let value2_input = cx.new(|cx| InputState::new(window, cx).placeholder("upper bound"));
            let filter_input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter results..."));
            let table = cx.new(|cx| {
                TableState::new(ScanResultsDelegate::new(), window, cx).row_selectable(true)
            });

            let mut subs = Vec::new();
            // Keep the form's value strings synced from the inputs.
            subs.push(
                cx.subscribe(&value_input, |this, input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.form.value_text = input.read(cx).value().to_string();
                    }
                }),
            );
            subs.push(
                cx.subscribe(&value2_input, |this, input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.form.value2_text = input.read(cx).value().to_string();
                    }
                }),
            );
            // Re-apply the post-scan filter when the filter text changes.
            subs.push(
                cx.subscribe(&filter_input, |this, _input, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.apply_filter(cx);
                    }
                }),
            );
            // Track row selection (for the footer goto/copy buttons) and forward
            // row activations as goto requests (the C++ onResultDoubleClicked).
            subs.push(
                cx.subscribe(&table, |this, table, ev: &TableEvent, cx| match ev {
                    TableEvent::SelectRow(row_ix) => {
                        this.selected_row = Some(*row_ix);
                        cx.notify();
                    }
                    TableEvent::DoubleClickedRow(row_ix) => {
                        let addr = table
                            .read(cx)
                            .delegate()
                            .rows
                            .get(*row_ix)
                            .map(|r| r.row.address);
                        if let Some(address) = addr {
                            cx.emit(ScannerNav { address });
                        }
                    }
                    // Double-click a cell to edit it (the C++ onCellEdited):
                    // the Address cell re-evaluates + navigates, the Value cell
                    // writes back to memory. The window resolves the intent
                    // against its mutable source.
                    TableEvent::DoubleClickedCell(row_ix, col_ix) => {
                        let addr = table
                            .read(cx)
                            .delegate()
                            .rows
                            .get(*row_ix)
                            .map(|r| r.row.address);
                        if let Some(address) = addr {
                            if *col_ix == COL_ADDRESS {
                                cx.emit(ScannerEdit::EvalAddress { row: *row_ix });
                            } else if *col_ix == COL_VALUE {
                                cx.emit(ScannerEdit::WriteValue {
                                    row: *row_ix,
                                    address,
                                });
                            }
                        }
                    }
                    _ => {}
                }),
            );

            ScannerPanel {
                form: ScannerForm::new(),
                value_input,
                value2_input,
                filter_input,
                table,
                results: Vec::new(),
                show_previous: false,
                undo_stack: Vec::new(),
                generation: 0,
                last_result_count: 0,
                provider: None,
                source_title: None,
                status: String::new(),
                selected_row: None,
                scanning: false,
                progress: 0,
                abort: Arc::new(AtomicBool::new(false)),
                mode_open: false,
                cond_open: false,
                type_open: false,
                align_open: false,
                focus_handle: cx.focus_handle(),
                _subs: subs,
            }
        }

        /// Construct as an [`Entity`] (the form a dock holds).
        pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(|cx| ScannerPanel::new(window, cx))
        }

        /// Attach the active tab's [`Provider`] (the C++ `setProviderGetter`
        /// result). Scans run against this source; the source name + kind feed
        /// the floating dock title (the C++ `updateScannerTitle`,
        /// "Memory Scanner — notepad.exe (Process)").
        pub fn set_provider(&mut self, provider: Option<Arc<dyn Provider + Send + Sync>>) {
            self.source_title = provider.as_ref().map(|p| {
                let name = p.name();
                let kind = p.kind();
                if kind.is_empty() {
                    name
                } else {
                    format!("{name} ({kind})")
                }
            });
            self.provider = provider;
        }

        /// Attach the active source by [`Entity`] handle is not needed — the
        /// window passes the provider directly via [`set_provider`].
        ///
        /// The floating dock title reflecting the attached source (the C++
        /// `updateScannerTitle`). "Memory Scanner" alone when no source.
        pub fn dock_title(&self) -> String {
            match &self.source_title {
                Some(s) => format!("Memory Scanner — {s}"),
                None => "Memory Scanner".to_string(),
            }
        }

        /// The form reducer (for tests / external wiring).
        pub fn form(&self) -> &ScannerForm {
            &self.form
        }

        /// The current status line text.
        pub fn status(&self) -> &str {
            &self.status
        }

        /// Pick a value type from the dropdown (dismisses the popover).
        fn set_value_type(&mut self, vt: ValueType, cx: &mut Context<Self>) {
            self.form.value_type = vt;
            self.type_open = false;
            cx.notify();
        }

        /// Pick the scan **type** (Value vs Signature) from the dedicated
        /// scan-type dropdown (PIC3/PIC6). Signature selects the C++ "Exact Sig"
        /// sentinel condition; Value falls back to the default Exact Value
        /// condition (so the condition/type/align controls re-appear).
        fn set_scan_mode(&mut self, mode: ScanMode, cx: &mut Context<Self>) {
            self.form.condition = match mode {
                ScanMode::Signature => CondEntry::Signature,
                ScanMode::Value => CondEntry::Value(ScanCondition::ExactValue),
            };
            self.mode_open = false;
            cx.notify();
        }

        /// Pick a condition entry from the dropdown (dismisses the popover).
        fn set_condition(&mut self, c: CondEntry, cx: &mut Context<Self>) {
            self.form.condition = c;
            self.cond_open = false;
            cx.notify();
        }

        /// Pick a fast-scan (alignment) stride from the dropdown.
        fn set_alignment(&mut self, a: i32, cx: &mut Context<Self>) {
            self.form.alignment = a;
            self.align_open = false;
            cx.notify();
        }

        /// Toggle a filter checkbox (the C++ filter checkboxes feeding
        /// `buildRequest`).
        fn toggle_executable(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.filter_executable = on;
            cx.notify();
        }
        fn toggle_writable(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.filter_writable = on;
            cx.notify();
        }
        fn toggle_struct_only(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.struct_only = on;
            cx.notify();
        }
        fn toggle_private_only(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.private_only = on;
            cx.notify();
        }
        fn toggle_skip_system(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.skip_system_modules = on;
            cx.notify();
        }
        fn toggle_user_mode(&mut self, on: bool, cx: &mut Context<Self>) {
            self.form.user_mode_only = on;
            cx.notify();
        }

        /// Run a **first scan** over the attached provider (the C++
        /// `onScanClicked` → `runScan`). Builds the request, runs the engine
        /// off the UI thread (so the panel can show a progress bar + Cancel),
        /// and refreshes the table + breadcrumb when it finishes.
        fn run_scan(&mut self, cx: &mut Context<Self>) {
            if self.scanning {
                return;
            }
            let Some(provider) = self.provider.clone() else {
                self.status = "No data source — attach a process or file to scan".to_string();
                cx.notify();
                return;
            };
            let ptr_size = provider.pointer_size();
            let req = match self.form.build_request(ptr_size, None) {
                Err(msg) => {
                    self.status = msg;
                    cx.notify();
                    return;
                }
                Ok(req) => req,
            };

            // Fresh first-scan: drop the undo history + previous column.
            self.undo_stack.clear();
            self.show_previous = false;
            self.generation = 1;
            self.last_result_count = 0;
            self.begin_scan(cx);

            let abort = self.abort.clone();
            let form = self.form.clone();
            cx.spawn(async move |this, cx| {
                let results = cx
                    .background_spawn(async move {
                        let obs = NullObserver;
                        run_scan(provider.as_ref(), &req, &abort, &obs)
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.finish_first_scan(results, &form, cx);
                })
                .ok();
            })
            .detach();
        }

        /// Run a **next scan** (the C++ `onUpdateClicked`): re-read ONLY the
        /// current result addresses and narrow them by the comparison condition,
        /// instead of running a brand-new full scan. Snapshots the pre-rescan
        /// list onto the undo stack first.
        fn next_scan(&mut self, cx: &mut Context<Self>) {
            if self.scanning || self.results.is_empty() {
                return;
            }
            let Some(provider) = self.provider.clone() else {
                self.status = "No source attached".to_string();
                cx.notify();
                return;
            };

            let last_mode = self.form.mode();
            let read_size = if last_mode == ScanMode::Value {
                value_size_for_type(self.form.value_type)
            } else {
                16
            };

            // Resolve the rescan condition (the C++ onUpdateClicked mapping).
            let mut cond = if last_mode == ScanMode::Value {
                self.form.effective_condition()
            } else {
                ScanCondition::ExactValue
            };
            // UnknownValue on rescan means "update only" — re-read, no filter.
            if cond == ScanCondition::UnknownValue {
                cond = ScanCondition::ExactValue;
            }

            // Build the filter pattern from the current input (only when the
            // condition consumes a typed/exact value).
            let mut filter_pattern: Vec<u8> = Vec::new();
            let mut filter_mask: Vec<u8> = Vec::new();
            let mut filter_pattern2: Vec<u8> = Vec::new();
            let vt = self.form.value_type;
            let needs_typed = matches!(
                cond,
                ScanCondition::ExactValue
                    | ScanCondition::BiggerThan
                    | ScanCondition::SmallerThan
                    | ScanCondition::Between
                    | ScanCondition::IncreasedBy
                    | ScanCondition::DecreasedBy
            );
            if needs_typed && !self.form.value_text.trim().is_empty() {
                if last_mode == ScanMode::Signature {
                    match crate::scanner::parse_signature(&self.form.value_text) {
                        Ok((p, m)) => {
                            filter_pattern = p;
                            filter_mask = m;
                        }
                        Err(e) => {
                            self.status = format!("Pattern error: {e}");
                            cx.notify();
                            return;
                        }
                    }
                } else {
                    match serialize_value(vt, &self.form.value_text) {
                        Ok((p, m)) => {
                            filter_pattern = p;
                            filter_mask = m;
                        }
                        Err(e) => {
                            self.status = format!("Value error: {e}");
                            cx.notify();
                            return;
                        }
                    }
                    if cond == ScanCondition::Between && !self.form.value2_text.trim().is_empty() {
                        match serialize_value(vt, &self.form.value2_text) {
                            Ok((p, _)) => filter_pattern2 = p,
                            Err(e) => {
                                self.status = format!("Upper bound error: {e}");
                                cx.notify();
                                return;
                            }
                        }
                    }
                }
            }

            // Snapshot the pre-rescan list so Undo Scan can roll back.
            self.push_undo_snapshot();
            self.last_result_count = self.results.len();
            self.generation = self.generation.max(1) + 1;
            self.show_previous = true;
            self.begin_scan(cx);

            let abort = self.abort.clone();
            let form = self.form.clone();
            let seed = std::mem::take(&mut self.results);
            cx.spawn(async move |this, cx| {
                let results = cx
                    .background_spawn(async move {
                        let obs = NullObserver;
                        run_rescan(
                            provider.as_ref(),
                            seed,
                            read_size,
                            cond,
                            vt,
                            &filter_pattern,
                            &filter_mask,
                            &filter_pattern2,
                            &abort,
                            &obs,
                        )
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.finish_rescan(results, &form, cx);
                })
                .ok();
            })
            .detach();
        }

        /// Mark the scan as running (progress bar + Cancel) and clear the abort.
        fn begin_scan(&mut self, cx: &mut Context<Self>) {
            self.abort = Arc::new(AtomicBool::new(false));
            self.scanning = true;
            self.progress = 0;
            self.status = "Scanning…".to_string();
            cx.notify();
        }

        /// Cancel a running scan / rescan (the C++ Esc / Cancel button): flip the
        /// abort flag the worker polls.
        fn cancel_scan(&mut self, cx: &mut Context<Self>) {
            if self.scanning {
                self.abort.store(true, std::sync::atomic::Ordering::Relaxed);
                self.status = "Cancelling…".to_string();
                cx.notify();
            }
        }

        /// Apply the results of a finished first scan.
        fn finish_first_scan(
            &mut self,
            mut results: Vec<ScanResult>,
            form: &ScannerForm,
            cx: &mut Context<Self>,
        ) {
            // Exact-value scans override the cached bytes with the searched
            // pattern (the engine caches raw chunk bytes); previous is cleared.
            for r in &mut results {
                r.previous_value.clear();
            }
            self.scanning = false;
            self.results = results;
            self.show_previous = false;
            self.selected_row = None;
            self.form = form.clone();
            let n = self.results.len();
            self.status = if n == 1 {
                "1 result".to_string()
            } else {
                format!("{n} results")
            };
            self.refresh_table(cx);
            cx.notify();
        }

        /// Apply the results of a finished re-scan (the C++ `onRescanFinished`):
        /// the narrowed-count status + the Previous→Δ column.
        fn finish_rescan(
            &mut self,
            results: Vec<ScanResult>,
            form: &ScannerForm,
            cx: &mut Context<Self>,
        ) {
            self.scanning = false;
            self.results = results;
            self.form = form.clone();
            self.selected_row = None;
            let n = self.results.len();
            self.status = rescan_status(self.last_result_count, n);
            self.refresh_table(cx);
            cx.notify();
        }

        /// Push the current result list onto the undo stack (capped).
        fn push_undo_snapshot(&mut self) {
            self.undo_stack.push(self.results.clone());
            if self.undo_stack.len() > MAX_UNDO {
                self.undo_stack.remove(0);
            }
        }

        /// Restore the previous result list (the C++ `popUndoSnapshot` / "Undo
        /// Scan"): roll back the last re-scan when a condition over-narrowed.
        fn undo_scan(&mut self, cx: &mut Context<Self>) {
            let Some(prev) = self.undo_stack.pop() else {
                return;
            };
            self.results = prev;
            self.last_result_count = 0;
            if self.generation > 1 {
                self.generation -= 1;
            }
            self.show_previous = self.undo_stack.iter().count() > 0 || self.generation > 1;
            let n = self.results.len();
            self.status = if n == 1 {
                "Restored — 1 result".to_string()
            } else {
                format!("Restored — {n} results")
            };
            self.selected_row = None;
            self.refresh_table(cx);
            cx.notify();
        }

        /// Clear the result list (the C++ `onNewScanClicked` / Reset).
        fn reset(&mut self, cx: &mut Context<Self>) {
            self.results.clear();
            self.undo_stack.clear();
            self.show_previous = false;
            self.generation = 0;
            self.last_result_count = 0;
            self.status.clear();
            self.selected_row = None;
            self.refresh_table(cx);
            cx.notify();
        }

        /// The address of the currently-selected result row, if any (drives the
        /// footer goto/copy buttons).
        fn selected_address(&self, cx: &App) -> Option<u64> {
            let ix = self.selected_row?;
            self.table
                .read(cx)
                .delegate()
                .rows
                .get(ix)
                .map(|r| r.row.address)
        }

        /// "Go to Address" footer button — raise a [`ScannerNav`] for the selected
        /// row (the C++ `goToAddress` signal), same as a double-click.
        fn go_to_selected(&mut self, cx: &mut Context<Self>) {
            if let Some(address) = self.selected_address(cx) {
                cx.emit(ScannerNav { address });
            }
        }

        /// "Copy Address" footer button — copy the selected row's address to the
        /// clipboard as `0x...` (the C++ copy-address context action).
        fn copy_selected(&mut self, cx: &mut Context<Self>) {
            if let Some(address) = self.selected_address(cx) {
                cx.write_to_clipboard(ClipboardItem::new_string(format!("0x{address:X}")));
                self.status = format!("Copied 0x{address:X}");
                cx.notify();
            }
        }

        /// The current post-scan filter text.
        fn filter_text(&self, cx: &App) -> String {
            self.filter_input.read(cx).value().to_string()
        }

        /// Re-apply the post-scan filter to the displayed rows.
        fn apply_filter(&mut self, cx: &mut Context<Self>) {
            self.refresh_table(cx);
            cx.notify();
        }

        /// Build the formatted display rows from the live [`ScanResult`] list
        /// (capped at [`MAX_DISPLAY_ROWS`]), computing the per-row delta + module
        /// metadata, then push the post-scan-filtered view into the table.
        fn refresh_table(&mut self, cx: &mut Context<Self>) {
            let query = self.filter_text(cx);
            let show_previous = self.show_previous;
            let vt = self.form.value_type;

            // Format every result into a ScanRow, computing the delta from the
            // recorded previous value (the C++ populateTable foreground tint).
            let all: Vec<DisplayRow> = self
                .results
                .iter()
                .take(MAX_DISPLAY_ROWS)
                .map(|r| {
                    let mut row = ScanRow::from_result(&self.form, r);
                    let mut delta_dir = 0;
                    if show_previous && !r.previous_value.is_empty() {
                        let d = compute_delta(vt, &r.previous_value, &r.scan_value);
                        delta_dir = d.direction;
                        let changed = r.previous_value != r.scan_value;
                        row.previous_text = previous_delta_text(&row.previous_text, &d, changed);
                    }
                    DisplayRow {
                        row,
                        delta_dir,
                        module: r.region_module.clone(),
                    }
                })
                .collect();

            let show_module = all.iter().any(|d| !d.module.is_empty());

            // Post-scan substring filter against the formatted row strings.
            let q = query.trim().to_lowercase();
            let rows: Vec<DisplayRow> = if q.is_empty() {
                all
            } else {
                all.into_iter()
                    .filter(|d| {
                        d.row.address_text.to_lowercase().contains(&q)
                            || d.row.value_text.to_lowercase().contains(&q)
                            || d.module.to_lowercase().contains(&q)
                    })
                    .collect()
            };

            self.table.update(cx, |state, cx| {
                let del = state.delegate_mut();
                del.rows = rows;
                del.show_previous = show_previous;
                del.show_module = show_module;
                cx.notify();
            });
        }

        /// The current value type / scan mode (so the window can decode/encode
        /// a value for an inline write or address re-eval).
        pub fn last_value_type(&self) -> ValueType {
            self.form.value_type
        }
        pub fn last_scan_mode(&self) -> ScanMode {
            self.form.mode()
        }

        /// The current text in the Value/pattern input (the inline-edit source —
        /// the window reads this to know what the user typed before writing).
        pub fn value_input_text(&self, cx: &App) -> String {
            self.value_input.read(cx).value().to_string()
        }

        /// The address of result row `row`, if present.
        pub fn row_address(&self, row: usize) -> Option<u64> {
            self.results.get(row).map(|r| r.address)
        }

        /// Apply an inline Address-cell edit (the C++ col-0 `onCellEdited`): set
        /// the row's address to the re-evaluated value + its re-read bytes, then
        /// refresh. The window evaluates the expression (the parser is a logic
        /// module) and re-reads via its provider.
        pub fn apply_address_edit(
            &mut self,
            row: usize,
            new_address: u64,
            new_value: Vec<u8>,
            cx: &mut Context<Self>,
        ) {
            if let Some(r) = self.results.get_mut(row) {
                r.address = new_address;
                if !new_value.is_empty() {
                    r.scan_value = new_value;
                }
                self.refresh_table(cx);
                cx.notify();
            }
        }

        /// Apply an inline Value-cell write (the C++ col-1 `onCellEdited`): record
        /// the freshly-read bytes after a successful write, then refresh.
        pub fn apply_value_write(
            &mut self,
            row: usize,
            new_value: Vec<u8>,
            cx: &mut Context<Self>,
        ) {
            if let Some(r) = self.results.get_mut(row) {
                r.previous_value = r.scan_value.clone();
                r.scan_value = new_value;
                self.status = format!("Wrote {} bytes to 0x{:X}", r.scan_value.len(), r.address);
                self.refresh_table(cx);
                cx.notify();
            }
        }

        /// Load a result list previously saved with [`results_json`]
        /// (the C++ `loadResultsFrom`): replace the results + refresh.
        pub fn load_results(&mut self, results: Vec<ScanResult>, cx: &mut Context<Self>) {
            let n = results.len();
            self.results = results;
            self.show_previous = false;
            self.generation = if n > 0 { 1 } else { 0 };
            self.undo_stack.clear();
            self.selected_row = None;
            self.status = if n == 1 {
                "Loaded 1 result".to_string()
            } else {
                format!("Loaded {n} results")
            };
            self.refresh_table(cx);
            cx.notify();
        }

        /// Serialize the current result list to the C++ scanner JSON shape
        /// (the C++ `saveResultsTo`). Pure-string output the window can write to
        /// a file.
        pub fn results_json(&self) -> String {
            let rows: Vec<(u64, Vec<u8>, String)> = self
                .results
                .iter()
                .map(|r| (r.address, r.scan_value.clone(), r.region_module.clone()))
                .collect();
            serialize_results_json(self.form.mode(), self.form.value_type, &rows)
        }

        /// The scan-type dropdown label (PIC3/PIC6: "Signature" vs "Value").
        fn mode_label(&self) -> &'static str {
            match self.form.mode() {
                ScanMode::Signature => "Signature",
                ScanMode::Value => "Value",
            }
        }

        /// The currently-selected condition entry's display label. In Signature
        /// mode the scan-type dropdown owns the label, so the condition reads as
        /// the implicit "Exact Value".
        fn cond_label(&self) -> &'static str {
            CondEntry::entries()
                .iter()
                .find(|(c, _)| *c == self.form.condition)
                .map(|(_, n)| *n)
                .unwrap_or("Exact Value")
        }

        /// The currently-selected value-type display label.
        fn type_label(&self) -> &'static str {
            value_type_entries()
                .iter()
                .find(|(vt, _)| *vt == self.form.value_type)
                .map(|(_, n)| *n)
                .unwrap_or("int32")
        }
    }

    /// A compact Zed dropdown popover row: a clickable inset row inside the
    /// elevated popover surface, with the soft-accent fill on the current pick.
    fn dropdown_row(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let label: SharedString = label.into();
        gpui_component::h_flex()
            .id(id)
            .w_full()
            .h(px(24.))
            .px(px(tokens::space::MD))
            .items_center()
            .rounded(px(tokens::radius::MD))
            .text_size(px(tokens::font::UI_MD))
            .text_color(color::text(cx))
            .when(selected, |r| r.bg(color::selected_bg(cx)))
            .when(!selected, |r| r.hover(|s| s.bg(color::hover_overlay(cx))))
            .child(label)
    }

    /// The elevated container the dropdown popovers drop into (the §5.10 menu
    /// surface: `elevated_bg`, 1px border, 6px radius, soft shadow).
    fn dropdown_menu(cx: &App) -> Div {
        gpui_component::v_flex()
            .min_w(px(140.))
            .p(px(tokens::space::XS))
            .gap(px(1.))
            .bg(color::elevated_bg(cx))
            .border_1()
            .border_color(color::border(cx))
            .rounded(px(tokens::radius::LG))
            .shadow_md()
    }

    impl Panel for ScannerPanel {
        fn panel_name(&self) -> &'static str {
            "ScannerPanel"
        }

        fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            SharedString::from(self.dock_title())
        }
    }

    impl EventEmitter<PanelEvent> for ScannerPanel {}
    impl EventEmitter<ScannerNav> for ScannerPanel {}
    impl EventEmitter<ScannerEdit> for ScannerPanel {}

    impl Focusable for ScannerPanel {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    /// A compact Zed dropdown trigger: an outline button showing the current pick
    /// with a trailing chevron, sitting in the toolbar like the reclass scan-type
    /// / type / scan combos. Built on a gpui-component `Button` (which is
    /// `Selectable` — the bound `Popover::trigger` requires) so it inherits the
    /// themed control look + the open/selected highlight.
    ///
    /// The chevron is a text glyph appended to the label rather than an
    /// [`IconName`] icon: this app does not bundle the gpui-component SVG icon
    /// assets (no asset source is registered; see `main.rs`), so an `.icon(...)`
    /// would paint an empty box. A `"⌄"` glyph reads as the combo affordance with
    /// no missing-asset artifact.
    fn dropdown_trigger(id: impl Into<SharedString>, text: impl Into<SharedString>) -> Button {
        let id: SharedString = id.into();
        let text: SharedString = text.into();
        Button::new(SharedString::from(format!("scanner-trig-{id}")))
            .outline()
            .small()
            .label(SharedString::from(format!("{text}  ⌄")))
    }

    impl Render for ScannerPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let vis = self.form.field_visibility();
            let has_results = !self.results.is_empty();
            let has_selection = self.selected_row.is_some();
            let has_undo = !self.undo_stack.is_empty();
            let scanning = self.scanning;
            let progress = self.progress;
            let breadcrumb =
                stage_breadcrumb(self.generation, self.last_result_count, self.results.len());
            let trunc = truncation_banner(self.results.len());
            let cur_cond = self.form.condition;
            let cur_type = self.form.value_type;
            let cur_align = self.form.alignment.max(1);

            let cur_mode = self.form.mode();

            // A weak handle to self so the popover content (which renders in the
            // PopoverState context) can drive the panel reducer on a pick.
            let panel = cx.entity().downgrade();

            // ── Scan-type dropdown (the C++ `m_modeCombo`: Value vs Signature) ──
            let mode_popover = Popover::new("scanner-mode-pop")
                .anchor(Anchor::TopLeft)
                .open(self.mode_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.mode_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger("mode", self.mode_label()))
                .content({
                    let panel = panel.clone();
                    move |_state, _window, cx| {
                        let mut menu = dropdown_menu(cx);
                        for (i, (mode, name)) in [
                            (ScanMode::Value, "Value"),
                            (ScanMode::Signature, "Signature"),
                        ]
                        .iter()
                        .enumerate()
                        {
                            let mode = *mode;
                            let panel = panel.clone();
                            menu = menu.child(
                                dropdown_row(("mode-row", i), *name, mode == cur_mode, cx)
                                    .on_click(move |_e, _w, cx| {
                                        panel
                                            .update(cx, |this, cx| this.set_scan_mode(mode, cx))
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    }
                });

            // ── Condition dropdown (the C++ condition combo). In Value mode it
            // offers the real value conditions only (the Signature sentinel lives
            // in the scan-type dropdown above). ──
            let cond_popover = Popover::new("scanner-cond-pop")
                .anchor(Anchor::TopLeft)
                .open(self.cond_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.cond_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger(
                    "cond",
                    format!("Scan: {}", self.cond_label()),
                ))
                .content({
                    let panel = panel.clone();
                    move |_state, _window, cx| {
                        let mut menu = dropdown_menu(cx);
                        for (i, (entry, name)) in CondEntry::entries()
                            .iter()
                            .filter(|(c, _)| !matches!(c, CondEntry::Signature))
                            .enumerate()
                        {
                            let entry = *entry;
                            let panel = panel.clone();
                            menu = menu.child(
                                dropdown_row(("cond-row", i), *name, entry == cur_cond, cx)
                                    .on_click(move |_e, _w, cx| {
                                        panel
                                            .update(cx, |this, cx| this.set_condition(entry, cx))
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    }
                });

            // ── Value-type dropdown (value mode only) ──
            let type_popover = Popover::new("scanner-type-pop")
                .anchor(Anchor::TopLeft)
                .open(self.type_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.type_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger(
                    "type",
                    format!("Type: {}", self.type_label()),
                ))
                .content({
                    let panel = panel.clone();
                    move |_state, _window, cx| {
                        let mut menu = dropdown_menu(cx);
                        for (i, (vt, name)) in value_type_entries().iter().enumerate() {
                            let vt = *vt;
                            let panel = panel.clone();
                            menu = menu.child(
                                dropdown_row(("type-row", i), *name, vt == cur_type, cx).on_click(
                                    move |_e, _w, cx| {
                                        panel
                                            .update(cx, |this, cx| this.set_value_type(vt, cx))
                                            .ok();
                                    },
                                ),
                            );
                        }
                        menu
                    }
                });

            // ── Fast-Scan (alignment) dropdown (value mode only) ──
            let align_popover = Popover::new("scanner-align-pop")
                .anchor(Anchor::TopLeft)
                .open(self.align_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.align_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger("align", format!("Align: {cur_align}")))
                .content({
                    let panel = panel.clone();
                    move |_state, _window, cx| {
                        let mut menu = dropdown_menu(cx);
                        for (i, a) in FAST_SCAN_ALIGNMENTS.iter().enumerate() {
                            let a = *a;
                            let panel = panel.clone();
                            menu = menu.child(
                                dropdown_row(("align-row", i), a.to_string(), a == cur_align, cx)
                                    .on_click(move |_e, _w, cx| {
                                        panel.update(cx, |this, cx| this.set_alignment(a, cx)).ok();
                                    }),
                            );
                        }
                        menu
                    }
                });

            // ── Status line: muted "N results" / "Copied ..." (the C++ result
            // count line). The scan path sets `status`; before the first scan it
            // is blank, so fall back to a count ("N results") and — when there is
            // no live data source — the graceful "attach a process or file to
            // scan" hint (live scanning needs a provider wired from the document,
            // out of scope here). ──
            let has_provider = self.provider.is_some();
            let status_text = if !self.status.is_empty() {
                self.status.clone()
            } else if !has_provider {
                "No data source — attach a process or file to scan".to_string()
            } else {
                let n = self.results.len();
                if n == 1 {
                    "1 result".to_string()
                } else {
                    format!("{n} results")
                }
            };

            gpui_component::v_flex()
                .id("rcx-scanner-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                // Esc cancels a running scan (the C++ Cancel shortcut).
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                    if ev.keystroke.key == "escape" && this.scanning {
                        this.cancel_scan(cx);
                    }
                }))
                // ── Panel header (uppercase muted title strip) ──
                .child(crate::ui::design::panel_header("Scanner", cx))
                .child(
                    // ── Search controls cluster (the reclass scanner toolbar) ──
                    gpui_component::v_flex()
                        .gap(px(tokens::space::MD))
                        .p(px(tokens::space::LG))
                        // Row 1: scan-type / value-type / alignment dropdowns on
                        // the left; Scan / Re-scan / Reset right-aligned (PIC3/PIC6
                        // put the action buttons at the toolbar's trailing edge).
                        .child(
                            gpui_component::h_flex()
                                .w_full()
                                .gap(px(tokens::space::MD))
                                .flex_wrap()
                                .items_center()
                                .justify_between()
                                .child(
                                    gpui_component::h_flex()
                                        .gap(px(tokens::space::MD))
                                        .flex_wrap()
                                        .items_center()
                                        // Scan-type (Value/Signature) is always shown.
                                        .child(mode_popover)
                                        // Value mode: Type / Scan-condition / Align.
                                        .when(vis.type_enabled, |row| row.child(type_popover))
                                        .when(vis.type_enabled, |row| row.child(cond_popover))
                                        .when(vis.type_enabled, |row| row.child(align_popover)),
                                )
                                .child(
                                    gpui_component::h_flex()
                                        .gap(px(tokens::space::SM))
                                        .items_center()
                                        // While a scan runs, the First Scan slot
                                        // becomes the Cancel control (the C++ Esc
                                        // / Cancel during a running scan).
                                        .when(scanning, |row| {
                                            row.child(
                                                Button::new("scanner-cancel")
                                                    .danger()
                                                    .small()
                                                    .label("Cancel  (Esc)")
                                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                                        this.cancel_scan(cx)
                                                    })),
                                            )
                                        })
                                        .when(!scanning, |row| {
                                            row.child(
                                                Button::new("scanner-scan")
                                                    .primary()
                                                    .small()
                                                    .label("First Scan")
                                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                                        this.run_scan(cx)
                                                    })),
                                            )
                                            .child(
                                                Button::new("scanner-rescan")
                                                    .small()
                                                    .label("Next Scan")
                                                    .disabled(!has_results)
                                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                                        this.next_scan(cx)
                                                    })),
                                            )
                                            .child(
                                                Button::new("scanner-undo")
                                                    .ghost()
                                                    .small()
                                                    .label("Undo Scan")
                                                    .disabled(!has_undo)
                                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                                        this.undo_scan(cx)
                                                    })),
                                            )
                                            .child(
                                                Button::new("scanner-reset")
                                                    .ghost()
                                                    .small()
                                                    .label("Reset")
                                                    .disabled(!has_results)
                                                    .on_click(cx.listener(|this, _e, _w, cx| {
                                                        this.reset(cx)
                                                    })),
                                            )
                                        }),
                                ),
                        )
                        // Row 2: Pattern:/Value: label + compact input(s).
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::MD))
                                .items_center()
                                .child(
                                    div()
                                        .w(px(56.))
                                        .flex_none()
                                        .text_size(px(tokens::font::UI_SM))
                                        .text_color(color::text_muted(cx))
                                        .child(vis.value_label),
                                )
                                .when(vis.value_visible || vis.pattern_visible, |row| {
                                    row.child(
                                        Input::new(&self.value_input)
                                            .small()
                                            .when(!vis.value_enabled, |i| i.disabled(true))
                                            .flex_1(),
                                    )
                                })
                                .when(vis.value2_visible, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(tokens::font::UI_SM))
                                            .text_color(color::text_muted(cx))
                                            .child(".."),
                                    )
                                    .child(Input::new(&self.value2_input).small().flex_1())
                                }),
                        )
                        // Row 3: filter checkboxes (Executable / Writable / Current Struct).
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::LG))
                                .items_center()
                                .child(
                                    Checkbox::new("scanner-exec")
                                        .label("Executable")
                                        .checked(self.form.filter_executable)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_executable(*on, cx)
                                        })),
                                )
                                .child(
                                    Checkbox::new("scanner-write")
                                        .label("Writable")
                                        .checked(self.form.filter_writable)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_writable(*on, cx)
                                        })),
                                )
                                .child(
                                    Checkbox::new("scanner-struct")
                                        .label("Current Struct")
                                        .checked(self.form.struct_only)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_struct_only(*on, cx)
                                        })),
                                )
                                // The C++ build_request also honors these filters;
                                // render them so they're reachable (gap 16).
                                .child(
                                    Checkbox::new("scanner-private")
                                        .label("Private only")
                                        .checked(self.form.private_only)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_private_only(*on, cx)
                                        })),
                                )
                                .child(
                                    Checkbox::new("scanner-skip-sys")
                                        .label("Skip system modules")
                                        .checked(self.form.skip_system_modules)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_skip_system(*on, cx)
                                        })),
                                )
                                .child(
                                    Checkbox::new("scanner-usermode")
                                        .label("User-mode only")
                                        .checked(self.form.user_mode_only)
                                        .on_click(cx.listener(|this, on: &bool, _w, cx| {
                                            this.toggle_user_mode(*on, cx)
                                        })),
                                ),
                        )
                        // ── Stage breadcrumb + progress bar (the C++ stage label
                        // + m_progressBar). ──
                        .when(!breadcrumb.is_empty() || scanning, |col| {
                            col.child(
                                gpui_component::v_flex()
                                    .gap(px(tokens::space::XS))
                                    .when(!breadcrumb.is_empty(), |c| {
                                        c.child(
                                            gpui_component::h_flex()
                                                .gap(px(tokens::space::XS))
                                                .items_center()
                                                .child(
                                                    div()
                                                        .size(px(6.))
                                                        .rounded_full()
                                                        .bg(color::accent(cx)),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens::font::UI_SM))
                                                        .text_color(color::text_muted(cx))
                                                        .child(breadcrumb.clone()),
                                                ),
                                        )
                                    })
                                    .when(scanning, |c| {
                                        // A 2px progress track filling to `progress`%.
                                        c.child(
                                            div()
                                                .w_full()
                                                .h(px(2.))
                                                .rounded_full()
                                                .bg(color::border(cx))
                                                .child(
                                                    div()
                                                        .h_full()
                                                        .w(relative(
                                                            (progress.clamp(0, 100) as f32) / 100.0,
                                                        ))
                                                        .rounded_full()
                                                        .bg(color::accent(cx)),
                                                ),
                                        )
                                    }),
                            )
                        }),
                )
                // ── Truncation banner (the C++ m_truncBanner). ──
                .when_some(trunc, |col, banner| {
                    col.child(
                        div()
                            .w_full()
                            .px(px(tokens::space::LG))
                            .py(px(tokens::space::XS))
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::text_muted(cx))
                            .bg(color::hover_overlay(cx))
                            .child(banner),
                    )
                })
                // ── Post-scan filter + status ──
                .child(
                    gpui_component::h_flex()
                        .px(px(tokens::space::LG))
                        .pb(px(tokens::space::MD))
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .child(Input::new(&self.filter_input).small().flex_1())
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(color::text_muted(cx))
                                .child(status_text),
                        ),
                )
                // ── Results table (virtualized) ──
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        // The DataTable's own table/head/row colors fall back to
                        // gpui-component's (light) defaults until the theme seeds
                        // them (theme_apply.rs). Paint the dark content bg behind
                        // the table so the body never flashes a near-white block
                        // inside the dark dock (the QA "glaring light rectangle").
                        .bg(color::content_bg(cx))
                        .border_t_1()
                        .border_color(color::border(cx))
                        .child(DataTable::new(&self.table).bordered(false).small()),
                )
                // ── Footer: Go to Address + Copy Address ──
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .px(px(tokens::space::LG))
                        .py(px(tokens::space::MD))
                        .gap(px(tokens::space::MD))
                        .justify_end()
                        .border_t_1()
                        .border_color(color::border(cx))
                        .bg(color::panel_bg(cx))
                        .child(
                            Button::new("scanner-goto")
                                .small()
                                .label("Go to Address")
                                .disabled(!has_selection)
                                .on_click(cx.listener(|this, _e, _w, cx| this.go_to_selected(cx))),
                        )
                        .child(
                            Button::new("scanner-copy")
                                .small()
                                .label("Copy Address")
                                .disabled(!has_selection)
                                .on_click(cx.listener(|this, _e, _w, cx| this.copy_selected(cx))),
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_delta, filter_rows, format_value, previous_delta_text, rescan_status,
        serialize_results_json, split_address_dim, stage_breadcrumb, truncation_banner,
        value_type_entries, CondEntry, ScanMode, ScanRow, ScannerForm, FAST_SCAN_ALIGNMENTS,
        MAX_DISPLAY_ROWS,
    };
    use crate::scanner::{ScanCondition, ScanResult, ValueType};

    // ── Delta / narrowed-count / breadcrumb / truncation / JSON helpers ──

    #[test]
    fn compute_delta_int32_increase_and_decrease() {
        let up = compute_delta(ValueType::Int32, &10i32.to_le_bytes(), &15i32.to_le_bytes());
        assert!(up.ok);
        assert_eq!(up.direction, 1);
        assert_eq!(up.text, "+5");
        let down = compute_delta(ValueType::Int32, &15i32.to_le_bytes(), &10i32.to_le_bytes());
        assert_eq!(down.direction, -1);
        assert_eq!(down.text, "-5");
        let same = compute_delta(ValueType::Int32, &7i32.to_le_bytes(), &7i32.to_le_bytes());
        assert_eq!(same.direction, 0);
        assert_eq!(same.text, "+0");
    }

    #[test]
    fn compute_delta_too_short_is_not_ok() {
        let d = compute_delta(ValueType::Int64, &[1, 2], &[3, 4]);
        assert!(!d.ok);
        assert_eq!(d.direction, 0);
    }

    #[test]
    fn previous_delta_text_variants() {
        let inc = compute_delta(ValueType::Int32, &1i32.to_le_bytes(), &2i32.to_le_bytes());
        assert_eq!(previous_delta_text("1", &inc, true), "1  →  +1");
        // Non-numeric change degrades to a bare arrow.
        let none = super::DeltaInfo::default();
        assert_eq!(previous_delta_text("AB", &none, true), "AB  →");
        // Unchanged non-numeric: just the prev text.
        assert_eq!(previous_delta_text("AB", &none, false), "AB");
        // No previous value: empty.
        assert_eq!(previous_delta_text("", &inc, true), "");
    }

    #[test]
    fn rescan_status_narrowed_and_eliminated() {
        assert_eq!(rescan_status(100, 30), "Narrowed 100 → 30  (eliminated 70)");
        assert_eq!(rescan_status(40, 40), "All 40 results still match");
        assert!(rescan_status(50, 0).starts_with("0 results"));
        assert_eq!(rescan_status(0, 1), "1 result");
        assert_eq!(rescan_status(0, 7), "7 results");
    }

    #[test]
    fn stage_breadcrumb_per_generation() {
        assert_eq!(stage_breadcrumb(0, 0, 0), "");
        assert_eq!(
            stage_breadcrumb(1, 0, 47),
            "Step 1 — First scan: 47 results"
        );
        assert_eq!(
            stage_breadcrumb(2, 47, 12),
            "Step 2 — Narrowed 47 → 12 (eliminated 35)"
        );
        assert_eq!(
            stage_breadcrumb(3, 12, 12),
            "Step 3 — All 12 results still match"
        );
    }

    #[test]
    fn truncation_banner_only_past_cap() {
        assert!(truncation_banner(MAX_DISPLAY_ROWS).is_none());
        let b = truncation_banner(MAX_DISPLAY_ROWS + 1).unwrap();
        assert!(b.contains(&MAX_DISPLAY_ROWS.to_string()));
    }

    #[test]
    fn serialize_results_json_shape() {
        let rows = vec![
            (0x401000u64, vec![0x39, 0x05, 0x00, 0x00], String::new()),
            (0x7ff0u64, vec![0xFFu8], "game.exe".to_string()),
        ];
        let json = serialize_results_json(ScanMode::Value, ValueType::Int32, &rows);
        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"scanMode\":1"));
        assert!(json.contains("\"count\":2"));
        assert!(json.contains("\"address\":\"401000\""));
        assert!(json.contains("\"value\":\"39050000\""));
        assert!(json.contains("\"module\":\"game.exe\""));
        // Signature mode records scanMode 0.
        let sig = serialize_results_json(ScanMode::Signature, ValueType::Int32, &[]);
        assert!(sig.contains("\"scanMode\":0"));
        assert!(sig.contains("\"count\":0"));
    }

    // ── Field-visibility reducer (onConditionChanged) ──

    #[test]
    fn signature_mode_shows_pattern_field() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Signature;
        assert_eq!(f.mode(), ScanMode::Signature);
        let v = f.field_visibility();
        assert!(v.pattern_visible);
        assert!(!v.value_visible);
        assert!(!v.type_enabled);
        assert_eq!(v.value_label, "Pattern:");
        // Signature folds to ExactValue for build_request.
        assert_eq!(f.effective_condition(), ScanCondition::ExactValue);
    }

    #[test]
    fn exact_value_shows_enabled_value_field() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        let v = f.field_visibility();
        assert_eq!(f.mode(), ScanMode::Value);
        assert!(v.value_visible);
        assert!(v.value_enabled);
        assert!(!v.value2_visible);
        assert!(v.type_enabled);
        assert_eq!(v.value_label, "Value:");
    }

    #[test]
    fn unknown_value_disables_value_field() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::UnknownValue);
        let v = f.field_visibility();
        // Value field visible but NOT enabled (no value needed).
        assert!(v.value_visible);
        assert!(!v.value_enabled);
        assert!(!v.value2_visible);
    }

    #[test]
    fn between_shows_upper_bound_field() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::Between);
        let v = f.field_visibility();
        assert!(v.value_visible);
        assert!(v.value_enabled);
        assert!(v.value2_visible);
    }

    #[test]
    fn changed_needs_no_value() {
        // Changed/Increased/... are compare-against-previous: no value field.
        for cond in [
            ScanCondition::Changed,
            ScanCondition::Unchanged,
            ScanCondition::Increased,
            ScanCondition::Decreased,
        ] {
            let mut f = ScannerForm::new();
            f.condition = CondEntry::Value(cond);
            let v = f.field_visibility();
            assert!(!v.value_enabled, "{cond:?} should not require a value");
        }
    }

    // ── build_request (buildRequest) ──

    #[test]
    fn build_request_exact_int32() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::Int32;
        f.value_text = "1337".to_string();
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.condition, ScanCondition::ExactValue);
        // 1337 == 0x539 little-endian.
        assert_eq!(req.pattern, vec![0x39, 0x05, 0x00, 0x00]);
        assert_eq!(req.mask, vec![0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(req.value_type, ValueType::Int32);
    }

    #[test]
    fn build_request_signature_mode() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Signature;
        f.value_text = "48 8B ?? 05".to_string();
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.alignment, 1);
        assert_eq!(req.pattern, vec![0x48, 0x8B, 0x00, 0x05]);
        assert_eq!(req.mask, vec![0xFF, 0xFF, 0x00, 0xFF]);
    }

    #[test]
    fn build_request_alignment_floored_at_natural() {
        // A uint64 scanned at stride 4 must be floored to 8 (natural alignment).
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::UInt64;
        f.value_text = "0".to_string();
        f.alignment = 4;
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.alignment, 8);
    }

    #[test]
    fn build_request_changed_becomes_unknown_capture() {
        // First-scan "Changed" captures all aligned addresses (UnknownValue) with
        // a huge max_results.
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::Changed);
        f.value_type = ValueType::Int32;
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.condition, ScanCondition::UnknownValue);
        assert_eq!(req.max_results, 10_000_000);
    }

    #[test]
    fn build_request_between_serializes_both_bounds() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::Between);
        f.value_type = ValueType::Int32;
        f.value_text = "10".to_string();
        f.value2_text = "20".to_string();
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.condition, ScanCondition::Between);
        assert_eq!(req.pattern, 10i32.to_le_bytes().to_vec());
        assert_eq!(req.pattern2, 20i32.to_le_bytes().to_vec());
        assert_eq!(req.mask.len(), 4);
    }

    #[test]
    fn build_request_user_mode_cap() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::Int32;
        f.value_text = "1".to_string();
        f.user_mode_only = true;
        // 64-bit cap.
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.end_address, 0x0000_7FFF_FFFF_FFFF);
        // 32-bit cap.
        let req32 = f.build_request(4, None).expect("ok");
        assert_eq!(req32.end_address, 0x7FFF_FFFF);
    }

    #[test]
    fn build_request_struct_only_bounds() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_text = "1".to_string();
        f.struct_only = true;
        let req = f.build_request(8, Some((0x1000, 0x200))).expect("ok");
        assert_eq!(req.start_address, 0x1000);
        assert_eq!(req.end_address, 0x1200);
    }

    #[test]
    fn build_request_bad_value_is_error() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::Int32;
        f.value_text = "notanumber".to_string();
        let err = f.build_request(8, None).unwrap_err();
        assert!(err.starts_with("Value error:"), "got {err}");
    }

    #[test]
    fn build_request_bad_pattern_is_error() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Signature;
        f.value_text = "ZZ".to_string();
        let err = f.build_request(8, None).unwrap_err();
        assert!(err.starts_with("Pattern error:"), "got {err}");
    }

    // ── formatValue ──

    #[test]
    fn format_value_signature_caps_at_pattern_len() {
        // Engine caches 16 bytes; signature mode shows only the searched length.
        let bytes: Vec<u8> = (0..16).collect();
        let last_pattern = vec![0u8; 4];
        let s = format_value(ScanMode::Signature, ValueType::Int32, &last_pattern, &bytes);
        assert_eq!(s, "00 01 02 03");
    }

    #[test]
    fn format_value_int32_decodes_le() {
        let bytes = 1337i32.to_le_bytes().to_vec();
        let s = format_value(ScanMode::Value, ValueType::Int32, &[], &bytes);
        assert_eq!(s, "1337");
    }

    #[test]
    fn format_value_too_short_is_question_marks() {
        let s = format_value(ScanMode::Value, ValueType::Int64, &[], &[1, 2]);
        assert_eq!(s, "??");
    }

    #[test]
    fn format_value_via_form_uses_last_scan() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::UInt8;
        f.value_text = "5".to_string();
        // build_request records the last-scan snapshot.
        f.build_request(8, None).expect("ok");
        assert_eq!(f.format_value(&[0xFF]), "255");
    }

    // ── AddressDelegate dim split + address formatting ──

    #[test]
    fn split_address_dims_leading_zeros() {
        // "00007FF6`12340000" → dim "00007", bright "FF6`12340000"? No: the
        // backtick rule only absorbs a backtick; the first non-zero stops it.
        let (dim, bright) = split_address_dim("0000000012340000");
        assert_eq!(dim, "00000000");
        assert_eq!(bright, "12340000");
    }

    #[test]
    fn split_address_absorbs_backtick_in_prefix() {
        // All-zero high dword + backtick are dim; low dword's first non-zero is bright.
        let (dim, bright) = split_address_dim("00000000`12340000");
        assert_eq!(dim, "00000000`");
        assert_eq!(bright, "12340000");
    }

    #[test]
    fn split_address_all_zero_keeps_trailing_zero_dim() {
        // Entirely zeros: everything is a leading zero, so the whole string dims.
        let (dim, bright) = split_address_dim("00000000");
        assert_eq!(dim, "00000000");
        assert_eq!(bright, "");
    }

    #[test]
    fn scan_row_formats_address_with_backtick() {
        let mut f = ScannerForm::new();
        f.value_type = ValueType::Int32;
        f.value_text = "1".to_string();
        f.build_request(8, None).expect("ok");
        let r = ScanResult {
            address: 0x0000_7FF6_1234_0000,
            scan_value: 1i32.to_le_bytes().to_vec(),
            ..ScanResult::default()
        };
        let row = ScanRow::from_result(&f, &r);
        assert_eq!(row.address_text, "00007FF6`12340000");
        assert_eq!(row.value_text, "1");
        assert_eq!(row.address, 0x0000_7FF6_1234_0000);
    }

    // ── Post-scan filter ──

    #[test]
    fn filter_rows_matches_address_or_value() {
        let rows = vec![
            ScanRow {
                address: 0x1000,
                address_text: "00000000`00001000".to_string(),
                value_text: "42".to_string(),
                previous_text: String::new(),
            },
            ScanRow {
                address: 0x2000,
                address_text: "00000000`00002000".to_string(),
                value_text: "1337".to_string(),
                previous_text: String::new(),
            },
        ];
        // Match by value.
        let by_val = filter_rows(&rows, "1337");
        assert_eq!(by_val.len(), 1);
        assert_eq!(by_val[0].address, 0x2000);
        // Match by address substring.
        let by_addr = filter_rows(&rows, "1000");
        assert_eq!(by_addr.len(), 1);
        assert_eq!(by_addr[0].address, 0x1000);
        // Empty query keeps all.
        assert_eq!(filter_rows(&rows, "  ").len(), 2);
    }

    // ── Dropdown catalogues mirror the C++ combos ──

    #[test]
    fn value_type_entries_in_discriminant_order() {
        let entries = value_type_entries();
        assert_eq!(entries[0].0, ValueType::Int8);
        assert_eq!(entries[0].1, "int8");
        assert_eq!(entries[2].0, ValueType::Int32);
        assert_eq!(entries.last().unwrap().0, ValueType::Double);
    }

    #[test]
    fn cond_entries_start_with_exact_then_signature() {
        let entries = CondEntry::entries();
        assert_eq!(entries[0].0, CondEntry::Value(ScanCondition::ExactValue));
        assert_eq!(entries[1].0, CondEntry::Signature);
        assert_eq!(entries[1].1, "Exact Sig");
    }

    #[test]
    fn fast_scan_alignments_are_powers_of_two() {
        assert_eq!(FAST_SCAN_ALIGNMENTS, &[1, 4, 8, 16, 32, 64]);
    }
}
