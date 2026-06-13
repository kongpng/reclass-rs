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

use ahash::AHashMap;

use crate::scanner::{
    natural_alignment, parse_signature, serialize_value, value_size_for_type, ScanCondition,
    ScanRequest, ScanResult, ValueType,
};
use crate::theme::manager::SettingsStore;

/// QSettings group/key names for scanner-form persistence (the C++
/// `ScannerPanel::saveSettings`/`loadSettings`, scannerpanel.cpp:2439-2465). The
/// C++ uses `beginGroup(key)` + relative keys; here we flatten to
/// `"<group>/<field>"` so any flat [`SettingsStore`] (the disk INI / registry
/// replacement) round-trips them. Kept in one place so save + load agree.
pub mod settings_keys {
    /// Default group the panel persists under (the C++ caller passes "scanner").
    pub const GROUP: &str = "scanner";
    pub const MODE: &str = "mode";
    pub const VALUE_TYPE: &str = "valueType";
    pub const CONDITION: &str = "condition";
    pub const FILTER_EXEC: &str = "filterExec";
    pub const FILTER_WRITE: &str = "filterWrite";
    pub const PRIVATE_ONLY: &str = "privateOnly";
    pub const SKIP_SYSTEM: &str = "skipSystem";
    pub const USER_MODE: &str = "userMode";
}

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
///
/// The C++ combo exposes int8..double; the engine ([`serialize_value`] /
/// [`value_size_for_type`] / [`natural_alignment`]) additionally supports the
/// vector and string types, so the port surfaces them too (raw gap 5): Vec2/3/4
/// for game-coordinate scans and UTF8/UTF16/HexBytes for string / raw-byte
/// scans. The persistence layer keys on the [`ValueType`] discriminant (not the
/// dropdown index), so appending entries does not break saved settings.
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
        (ValueType::Vec2, "vec2"),
        (ValueType::Vec3, "vec3"),
        (ValueType::Vec4, "vec4"),
        (ValueType::Utf8, "utf8"),
        (ValueType::Utf16, "utf16"),
        (ValueType::HexBytes, "hex bytes"),
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
    /// The condition the last `build_request` actually emitted (the C++
    /// `m_lastCondition`). For value-mode ExactValue scans, `finish_first_scan`
    /// overrides each result's cached bytes with this searched `last_pattern`
    /// (the engine caches the raw chunk, not the searched value).
    last_condition: ScanCondition,
}

impl Default for ScannerForm {
    fn default() -> Self {
        ScannerForm {
            condition: CondEntry::Value(ScanCondition::ExactValue),
            value_type: ValueType::Int32,
            value_text: String::new(),
            value2_text: String::new(),
            // Fast-Scan default is index 1 = 4 (dword), matching the C++
            // `m_fastScanCombo->setCurrentIndex(1)` (scannerpanel.cpp:529).
            alignment: 4,
            filter_executable: false,
            // Value mode scans writable memory by default — the C++ constructor
            // calls `m_writeCheck->setChecked(true)` (scannerpanel.cpp:720), so a
            // fresh first scan is writable-only, not all readable memory.
            filter_writable: true,
            private_only: false,
            skip_system_modules: false,
            user_mode_only: false,
            struct_only: false,
            last_mode: ScanMode::Value,
            last_value_type: ValueType::Int32,
            last_pattern: Vec::new(),
            last_condition: ScanCondition::ExactValue,
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

    /// Smart filter defaults per scan **mode**, mirroring `onModeChanged(int)`
    /// (`scannerpanel.cpp:1135-1149`). In C++ the (hidden) mode-combo index
    /// drives this: index 0 = Signature → executable-only; index 1 = Value →
    /// writable-only; the private / skip-system / user-mode toggles always reset
    /// to off. In the Rust model the mode is *derived* from the condition entry
    /// (see [`mode`](ScannerForm::mode)), so there is no separate mode signal to
    /// hook — [`set_scan_mode`](self) applies these five defaults explicitly when
    /// the user picks a scan type from the dropdown. `struct_only` and
    /// `alignment` are deliberately left untouched (the C++ handler does not
    /// touch them either).
    pub fn apply_mode_defaults(&mut self, mode: ScanMode) {
        let is_sig = mode == ScanMode::Signature;
        self.filter_executable = is_sig;
        self.filter_writable = !is_sig;
        self.private_only = false;
        self.skip_system_modules = false;
        self.user_mode_only = false;
    }

    /// `onConditionChanged` field-visibility logic (`scannerpanel.cpp:1151-1203`).
    pub fn field_visibility(&self) -> FieldVisibility {
        let is_sig = self.mode() == ScanMode::Signature;
        let cond = self.effective_condition();
        let needs_value = is_sig || consumes_typed_value(cond);
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
            if is_compare_previous(cond) {
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
            } else if is_range_condition(cond) {
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

        // Record the "last scan" snapshot for result-value formatting (the C++
        // `m_lastScanMode`/`m_lastValueType`/`m_lastCondition`/`m_lastPattern`,
        // scannerpanel.cpp:1226-1231). `m_lastCondition` is only updated in value
        // mode, after the Changed→Unknown remap above (so ExactValue stays
        // ExactValue while Changed/Increased/etc. became UnknownValue).
        self.last_mode = self.mode();
        if self.last_mode == ScanMode::Value {
            self.last_value_type = self.value_type;
            self.last_condition = req.condition;
        }
        self.last_pattern = req.pattern.clone();

        Ok(req)
    }

    /// Overwrite the last-scan snapshot directly (used by the blocking
    /// automation entry points, which build their own [`ScanRequest`] outside
    /// [`build_request`](Self::build_request) and so must record the snapshot
    /// the value-column formatter reads).
    pub fn set_last_scan(
        &mut self,
        mode: ScanMode,
        value_type: ValueType,
        condition: ScanCondition,
        pattern: &[u8],
    ) {
        self.last_mode = mode;
        self.last_value_type = value_type;
        self.last_condition = condition;
        self.last_pattern = pattern.to_vec();
    }

    /// The mode the last `build_request` ran in (the C++ `m_lastScanMode`).
    pub fn last_mode(&self) -> ScanMode {
        self.last_mode
    }

    /// The value type the last value-mode `build_request` used (the C++
    /// `m_lastValueType`).
    pub fn last_value_type(&self) -> ValueType {
        self.last_value_type
    }

    /// The condition the last value-mode `build_request` emitted (the C++
    /// `m_lastCondition`, post Changed→Unknown remap).
    pub fn last_condition(&self) -> ScanCondition {
        self.last_condition
    }

    /// The serialized search bytes from the last `build_request` (the C++
    /// `m_lastPattern`). For a value-mode ExactValue scan this is the searched
    /// value, which `finish_first_scan` writes back over each result's cached
    /// bytes (the engine only caches the raw chunk).
    pub fn last_pattern(&self) -> &[u8] {
        &self.last_pattern
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

    /// Persist the form's mode / value-type / condition / filter checkboxes to a
    /// [`SettingsStore`] (the C++ `ScannerPanel::saveSettings`,
    /// scannerpanel.cpp:2439-2451). Unlike the C++ — which stores fragile combo
    /// *indices* — this persists semantic enum values (`ScanMode` 0/1, the
    /// `ValueType` discriminant, and the `ScanCondition` discriminant or `-1` for
    /// the Signature sentinel) so appending dropdown entries can't corrupt a
    /// saved session. Keyed under `group/<field>`.
    pub fn save_settings<S: SettingsStore + ?Sized>(&self, group: &str, store: &mut S) {
        let k = |field: &str| format!("{group}/{field}");
        let mode_i = match self.mode() {
            ScanMode::Signature => 0,
            ScanMode::Value => 1,
        };
        store.set(&k(settings_keys::MODE), &mode_i.to_string());
        store.set(
            &k(settings_keys::VALUE_TYPE),
            &(self.value_type as i32).to_string(),
        );
        let cond_i = match self.condition {
            CondEntry::Signature => -1,
            CondEntry::Value(c) => c as i32,
        };
        store.set(&k(settings_keys::CONDITION), &cond_i.to_string());
        store.set(
            &k(settings_keys::FILTER_EXEC),
            bool_str(self.filter_executable),
        );
        store.set(
            &k(settings_keys::FILTER_WRITE),
            bool_str(self.filter_writable),
        );
        store.set(&k(settings_keys::PRIVATE_ONLY), bool_str(self.private_only));
        store.set(
            &k(settings_keys::SKIP_SYSTEM),
            bool_str(self.skip_system_modules),
        );
        store.set(&k(settings_keys::USER_MODE), bool_str(self.user_mode_only));
    }

    /// Restore the form from a [`SettingsStore`] previously written by
    /// [`save_settings`](Self::save_settings) (the C++ `loadSettings`,
    /// scannerpanel.cpp:2453-2465). Missing / unparseable keys leave the current
    /// value untouched (the C++ `if (s.contains(...))` guard).
    pub fn load_settings<S: SettingsStore + ?Sized>(&mut self, group: &str, store: &S) {
        let k = |field: &str| format!("{group}/{field}");
        if let Some(v) = store.get(&k(settings_keys::CONDITION)) {
            if let Ok(c) = v.parse::<i32>() {
                self.condition = cond_entry_from_i32(c);
            }
        }
        // MODE is honored only when the condition didn't already pin Signature —
        // a stored mode of 0 (Signature) re-selects the Signature sentinel.
        if let Some(v) = store.get(&k(settings_keys::MODE)) {
            if v.parse::<i32>() == Ok(0) {
                self.condition = CondEntry::Signature;
            }
        }
        if let Some(v) = store.get(&k(settings_keys::VALUE_TYPE)) {
            if let Ok(d) = v.parse::<i32>() {
                if let Some(vt) = value_type_from_i32(d) {
                    self.value_type = vt;
                }
            }
        }
        if let Some(v) = store.get(&k(settings_keys::FILTER_EXEC)) {
            self.filter_executable = parse_bool(&v);
        }
        if let Some(v) = store.get(&k(settings_keys::FILTER_WRITE)) {
            self.filter_writable = parse_bool(&v);
        }
        if let Some(v) = store.get(&k(settings_keys::PRIVATE_ONLY)) {
            self.private_only = parse_bool(&v);
        }
        if let Some(v) = store.get(&k(settings_keys::SKIP_SYSTEM)) {
            self.skip_system_modules = parse_bool(&v);
        }
        if let Some(v) = store.get(&k(settings_keys::USER_MODE)) {
            self.user_mode_only = parse_bool(&v);
        }
    }
}

/// `"true"`/`"false"` (the `DiskSettings::set_bool` spelling).
fn bool_str(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// Parse a persisted bool (`"true"`/`"1"` ⇒ true), mirroring
/// `DiskSettings::get_bool`.
fn parse_bool(s: &str) -> bool {
    s == "true" || s == "1"
}

/// Map a `ValueType` discriminant back to the enum (the inverse of
/// `value_type as i32`). Returns `None` for an out-of-range / unknown value so
/// load leaves the current type untouched.
fn value_type_from_i32(d: i32) -> Option<ValueType> {
    value_type_entries()
        .iter()
        .map(|(vt, _)| *vt)
        .find(|vt| *vt as i32 == d)
}

/// Map a persisted condition discriminant (or `-1` for Signature) to the
/// dropdown entry it selects.
fn cond_entry_from_i32(c: i32) -> CondEntry {
    if c < 0 {
        return CondEntry::Signature;
    }
    CondEntry::entries()
        .iter()
        .map(|(e, _)| *e)
        .find(|e| match e {
            CondEntry::Value(cond) => *cond as i32 == c,
            CondEntry::Signature => false,
        })
        .unwrap_or(CondEntry::Value(ScanCondition::ExactValue))
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
        return hex_space_upper(&bytes[..show_len]);
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
        // Vector types: render as "(x, y[, z[, w]])" with the float-9 precision
        // each component would use as a scalar Float.
        ValueType::Vec2 if sz >= 8 => return format_vec(bytes, 2),
        ValueType::Vec3 if sz >= 12 => return format_vec(bytes, 3),
        ValueType::Vec4 if sz >= 16 => return format_vec(bytes, 4),
        // String types: render the decoded text (lossy for invalid sequences,
        // the way a value preview tolerates partial reads).
        ValueType::Utf8 if sz > 0 => {
            return String::from_utf8_lossy(bytes).to_string();
        }
        ValueType::Utf16 if sz >= 2 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            return String::from_utf16_lossy(&units);
        }
        // Hex bytes: space-separated uppercase, same shape as signature display.
        ValueType::HexBytes if sz > 0 => {
            return hex_space_upper(bytes);
        }
        _ => {}
    }
    "??".to_string()
}

/// Render `n` little-endian `f32` components as `"(x, y, …)"` using the same
/// per-component precision the scalar Float column uses (`%g,9`). Used for the
/// Vec2/Vec3/Vec4 value-column display.
fn format_vec(bytes: &[u8], n: usize) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        let v = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        parts.push(format_float(v as f64, 9));
    }
    format!("({})", parts.join(", "))
}

/// `QString::number(double, 'g', prec)` — `%g`-style rendering honoring the
/// significant-digit precision argument, matching C's `printf("%.*g", prec, v)`
/// (which is what Qt's `QString::number(double, 'g', prec)` ultimately calls).
///
/// The scanner uses three precisions: float values render with `%g,9`, double
/// values with `%g,17`, and float/double deltas with `%g,6` — so the precision
/// argument is load-bearing (a too-coarse default Display diverged from the C++
/// for e.g. `0.30000001192092896_f32` which `%g,9` prints as `0.300000012`).
///
/// `%g` semantics (per C99): with `P` significant digits (`P==0` treated as 1),
/// let `X` be the decimal exponent of `v`. If `X >= -4 && X < P`, use `%f`-style
/// with `P - 1 - X` fractional digits; otherwise use `%e`-style with `P - 1`
/// fractional digits. Trailing zeros (and a trailing `.`) are then stripped (no
/// `#` flag). NaN/inf degrade to Rust's Display (`NaN`/`inf`), which matches
/// what the column shows for a non-finite value.
fn format_float(v: f64, prec: usize) -> String {
    if !v.is_finite() {
        // C `%g` prints "nan"/"inf"; Qt mirrors libc. Keep Rust's spelling here
        // (the value column never compares these as strings).
        return format!("{v}");
    }
    let p = prec.max(1);

    if v == 0.0 {
        // Preserve sign of zero the way printf does (it does not print "-0").
        return "0".to_string();
    }

    // Decimal exponent X = floor(log10(|v|)). Compute via the %e rendering so
    // rounding at P significant digits picks the exponent libc would pick (a
    // value like 9.9999e0 at P=1 rounds up to 1e1, bumping X).
    let e_str = format!("{:.*e}", p - 1, v); // e.g. "1.23e2" / "9.9e0"
    let exp: i32 = e_str
        .rsplit_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);

    let pi = p as i32;
    let body = if exp >= -4 && exp < pi {
        // %f-style with (P - 1 - X) fractional digits.
        let frac = (pi - 1 - exp).max(0) as usize;
        let s = format!("{:.*}", frac, v);
        strip_g_trailing(&s)
    } else {
        // %e-style with (P - 1) fractional mantissa digits; normalize the
        // exponent to libc's `e±NN` (sign + at least two digits).
        let (mantissa, e) = e_str.split_once('e').unwrap_or((e_str.as_str(), "0"));
        let mantissa = strip_g_trailing(mantissa);
        let ev: i32 = e.parse().unwrap_or(0);
        let sign = if ev < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", ev.abs())
    };
    body
}

/// Strip the trailing zeros (and a now-dangling decimal point) from a fixed /
/// mantissa rendering — the `%g` no-`#`-flag behaviour. `"1.2300" -> "1.23"`,
/// `"5.000" -> "5"`, `"5" -> "5"`.
fn strip_g_trailing(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
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

/// Which scan the Ctrl+Return shortcut should dispatch, given the panel state.
///
/// Encodes the C++ `scanShortcut` preference (`scannerpanel.cpp:832-838`):
/// `if (m_updateBtn->isEnabled() && !isRunning()) onUpdateClicked(); else
/// onScanClicked();`. The Next-Scan ("Update") button is enabled exactly when a
/// scan has already produced results, so Ctrl+Return prefers a Next Scan once
/// there are results, falls back to a First Scan otherwise, and does nothing
/// while a scan is already running (the `onScanClicked` else-branch would abort
/// in C++, but the panel handles abort/cancel via Esc instead, so the keyboard
/// scan trigger is a no-op mid-scan).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScanShortcut {
    /// Start a First Scan (`onScanClicked` / `run_scan`).
    RunScan,
    /// Start a Next Scan (`onUpdateClicked` / `next_scan`).
    NextScan,
    /// Do nothing (a scan is already running).
    None,
}

/// Decide what Ctrl+Return does (the C++ `scanShortcut` lambda,
/// `scannerpanel.cpp:832-838`). Pure so the dispatch decision is unit-testable.
pub fn shortcut_scan_target(scanning: bool, has_results: bool) -> ScanShortcut {
    if scanning {
        ScanShortcut::None
    } else if has_results {
        ScanShortcut::NextScan
    } else {
        ScanShortcut::RunScan
    }
}

/// Format an address the way the result table shows it: 16 hex digits with a
/// backtick between the high and low dwords (`"00007FF6`12340000"`), uppercase —
/// matching the C++ column width hint `"00000000`00000000"`.
pub fn format_scan_address(addr: u64) -> String {
    let hi = (addr >> 32) as u32;
    let lo = (addr & 0xFFFF_FFFF) as u32;
    format!("{hi:08X}`{lo:08X}")
}

/// Format a batch of addresses for the clipboard (the C++ multi-row Copy):
/// newline-joined `0xUPPER` hex, in the order given. Used by "Copy All
/// Addresses" and the batch context action.
pub fn format_addresses_for_clipboard(addresses: &[u64]) -> String {
    addresses
        .iter()
        .map(|a| format!("0x{a:X}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop the results at the given displayed-row indices, returning the surviving
/// list (the C++ multi-row Delete). Indices are deduped + sorted descending so
/// removal can't shift an index out from under a later removal; out-of-range
/// indices are ignored.
pub fn delete_rows(mut results: Vec<ScanResult>, rows: &[usize]) -> Vec<ScanResult> {
    let mut idx: Vec<usize> = rows.to_vec();
    idx.sort_unstable();
    idx.dedup();
    for &r in idx.iter().rev() {
        if r < results.len() {
            results.remove(r);
        }
    }
    results
}

/// Apply the Change-All writeback payload to scanner results.
///
/// The window emits writebacks in result order in the normal live-provider path,
/// so this first takes a direct O(n) zip path. If an integration returns
/// out-of-order addresses, fall back to an address index instead of the old
/// per-update linear search. Only `scan_value` is changed; `previous_value`
/// remains the last rescan snapshot, matching the C++ panel behavior.
pub fn apply_change_all_results(
    results: &mut [ScanResult],
    writebacks: Vec<(u64, Option<Vec<u8>>)>,
) -> usize {
    let ordered = writebacks.len() == results.len()
        && writebacks
            .iter()
            .zip(results.iter())
            .all(|((addr, _), result)| *addr == result.address);

    let mut wrote = 0usize;
    if ordered {
        for (result, (_, new_bytes)) in results.iter_mut().zip(writebacks) {
            if let Some(bytes) = new_bytes {
                result.scan_value = bytes.into();
                wrote += 1;
            }
        }
        return wrote;
    }

    let mut by_address = AHashMap::with_capacity(results.len());
    for (idx, result) in results.iter().enumerate() {
        by_address.entry(result.address).or_insert(idx);
    }
    for (addr, new_bytes) in writebacks {
        if let Some(bytes) = new_bytes {
            if let Some(&idx) = by_address.get(&addr) {
                results[idx].scan_value = bytes.into();
            }
            wrote += 1;
        }
    }
    wrote
}

/// Parse a "Change All Values" replacement string into raw bytes, per the last
/// scan mode + value type (the C++ batch-write parse, scannerpanel.cpp:945-978).
///
/// Signature mode parses space-separated hex bytes (no wildcards — each token
/// must be a valid `00..FF` byte; a `??` is rejected, matching the C++ `toUInt`
/// path that fails on non-hex). Value mode serializes the typed value exactly
/// the way [`serialize_value`] does for a single cell. Returns the upstream-style
/// error string on a parse failure.
pub fn parse_change_all_bytes(
    mode: ScanMode,
    vt: ValueType,
    text: &str,
) -> Result<Vec<u8>, String> {
    if mode == ScanMode::Signature {
        // Space-separated hex bytes; reject anything that isn't 00..FF (the C++
        // `tok.toUInt(&ok, 16)` + `val > 0xFF` guard — wildcards not allowed in a
        // write).
        let mut bytes = Vec::new();
        for tok in text.split(' ').filter(|t| !t.is_empty()) {
            match u32::from_str_radix(tok, 16) {
                Ok(v) if v <= 0xFF => bytes.push(v as u8),
                _ => return Err(format!("Invalid hex byte: {tok}")),
            }
        }
        Ok(bytes)
    } else {
        // Typed value — serialize exactly like a single-cell edit.
        serialize_value(vt, text)
            .map(|(pat, _mask)| pat)
            .map_err(|_| "Invalid value".to_string())
    }
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
                // Wrapping matches the C++ `(long long)(int64_t)(b - a)` and the
                // UInt64 arm. For the narrower instantiations the i64 cast keeps
                // `b - a` well inside range, so this is bit-identical; for Int64 it
                // turns a debug-build overflow panic into the wrapped value release
                // already produces.
                fmt_int(b.wrapping_sub(a));
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

/// Whether a scan condition consumes a typed/exact value from the value field
/// (used to gate the value input + the rescan filter pattern).
fn consumes_typed_value(c: ScanCondition) -> bool {
    matches!(
        c,
        ScanCondition::ExactValue
            | ScanCondition::BiggerThan
            | ScanCondition::SmallerThan
            | ScanCondition::Between
            | ScanCondition::IncreasedBy
            | ScanCondition::DecreasedBy
    )
}

/// Whether a condition compares each value against its previous snapshot (so the
/// first scan must capture a baseline as UnknownValue).
fn is_compare_previous(c: ScanCondition) -> bool {
    matches!(
        c,
        ScanCondition::Changed
            | ScanCondition::Unchanged
            | ScanCondition::Increased
            | ScanCondition::Decreased
            | ScanCondition::IncreasedBy
            | ScanCondition::DecreasedBy
    )
}

/// Whether a condition compares against a typed bound / range.
fn is_range_condition(c: ScanCondition) -> bool {
    matches!(
        c,
        ScanCondition::BiggerThan | ScanCondition::SmallerThan | ScanCondition::Between
    )
}

/// Space-joined uppercase hex (`"DE AD BE EF"`) of `bytes` — the scanner's
/// signature / hex-bytes value rendering.
fn hex_space_upper(bytes: &[u8]) -> String {
    let mut s = String::new();
    for (j, b) in bytes.iter().enumerate() {
        if j > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// `"{prefix}1 result"` / `"{prefix}{n} results"` — the result-count status line
/// used across the scan-finish, undo, load, and render-fallback paths.
pub fn count_status(prefix: &str, n: usize) -> String {
    let noun = if n == 1 { "result" } else { "results" };
    format!("{prefix}{n} {noun}")
}

/// The status line after a first scan (`scannerpanel.cpp:1473-1479`): a 0-result
/// scan shows the "try widening the filters" guidance; otherwise the plain
/// result count. The empty result set is identical either way — this is the
/// UX hint the C++ `onScanFinished` surfaces so an empty scan doesn't read as a
/// dead end.
pub fn first_scan_status(n: usize) -> String {
    if n == 0 {
        "0 results — try widening the filters above or checking the value/pattern.".to_string()
    } else {
        count_status("", n)
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
    } else {
        count_status("", after)
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

/// A parsed scanner results document — the recovered [`ScanResult`] list plus
/// the saved scan mode + value type (the C++ `loadResultsFrom` reads
/// `root["scanMode"]` / `root["valueType"]`, scannerpanel.cpp:2417-2418). The
/// scan mode + value type drive how the Value column formats the loaded bytes,
/// so they must be restored — without them a saved uint64 scan would format
/// with the form's current type.
#[derive(Clone, Debug)]
pub struct ScannerResultsDoc {
    /// The recovered result rows.
    pub results: Vec<ScanResult>,
    /// The saved scan mode (the C++ `m_lastScanMode`, 0 = Signature, 1 = Value).
    /// Defaults to [`ScanMode::Signature`] (C++ `toInt(0)`) when absent.
    pub scan_mode: ScanMode,
    /// The saved value type (the C++ `m_lastValueType`). Defaults to
    /// [`ValueType::Int32`] (C++ `toInt((int)ValueType::Int32)`) when absent.
    pub value_type: ValueType,
}

/// Parse a scanner results JSON document (the inverse of
/// [`serialize_results_json`]; the C++ `loadResultsFrom`). Returns the recovered
/// [`ScanResult`] list together with the saved scan mode + value type. Tolerant
/// hand-rolled scan over the `results` array (the format is a flat list of
/// `{address, value, module?}` objects with hex fields), so loading a file the
/// port itself wrote round-trips. Unknown / extra keys are ignored; a malformed
/// entry is skipped. Pure + unit-tested.
///
/// The top-level `scanMode` / `valueType` ints are parsed the way the C++
/// `loadResultsFrom` reads them (default scanMode 0 = Signature, default
/// valueType = int32) so the Value column formats loaded bytes with the saved
/// type rather than the form's current type.
pub fn deserialize_results_json(json: &str) -> ScannerResultsDoc {
    // Extract a top-level integer field `"key": N` (numeric, not string-quoted).
    fn int_field(json: &str, key: &str) -> Option<i64> {
        let pat = format!("\"{key}\"");
        let kpos = json.find(&pat)?;
        let after = &json[kpos + pat.len()..];
        let colon = after.find(':')?;
        let rest = after[colon + 1..].trim_start();
        // Numeric run (optional leading '-'); stops at the first non-digit.
        let mut end = 0;
        let bytes = rest.as_bytes();
        while end < bytes.len() && (bytes[end] == b'-' || bytes[end].is_ascii_digit()) {
            end += 1;
        }
        rest[..end].parse::<i64>().ok()
    }

    let scan_mode = match int_field(json, "scanMode") {
        Some(1) => ScanMode::Value,
        // C++ `toInt(0)` defaults to 0 = Signature when absent / non-1.
        _ => ScanMode::Signature,
    };
    let value_type = int_field(json, "valueType")
        .and_then(|d| value_type_from_i32(d as i32))
        .unwrap_or(ValueType::Int32);

    ScannerResultsDoc {
        results: deserialize_result_rows(json),
        scan_mode,
        value_type,
    }
}

/// Parse just the `results:[...]` array of a scanner JSON document into the
/// recovered [`ScanResult`] list (the row-parsing half of
/// [`deserialize_results_json`]).
fn deserialize_result_rows(json: &str) -> Vec<ScanResult> {
    fn unhex(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len() / 2);
        let mut i = 0;
        while i + 1 < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16);
            let lo = (bytes[i + 1] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                _ => break,
            }
            i += 2;
        }
        out
    }
    // Extract the value of a `"key":"..."` string field within the slice `obj`.
    fn str_field(obj: &str, key: &str) -> Option<String> {
        let pat = format!("\"{key}\"");
        let kpos = obj.find(&pat)?;
        let after = &obj[kpos + pat.len()..];
        let colon = after.find(':')?;
        let rest = after[colon + 1..].trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    // Isolate the `results:[ ... ]` array body, then split on top-level `}`.
    let Some(arr_start) = json
        .find("\"results\"")
        .and_then(|p| json[p..].find('[').map(|b| p + b + 1))
    else {
        return Vec::new();
    };
    let arr = &json[arr_start..];
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut obj_start: Option<usize> = None;
    for (i, ch) in arr.char_indices() {
        match ch {
            ']' if depth == 0 => break,
            '{' => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = obj_start.take() {
                        let obj = &arr[start..=i];
                        if let Some(addr_hex) = str_field(obj, "address") {
                            let address =
                                u64::from_str_radix(addr_hex.trim_start_matches("0x"), 16)
                                    .unwrap_or(0);
                            let value = str_field(obj, "value")
                                .map(|v| unhex(&v))
                                .unwrap_or_default();
                            let module = str_field(obj, "module").unwrap_or_default();
                            out.push(ScanResult {
                                address,
                                region_module: module,
                                scan_value: value.into(),
                                previous_value: Default::default(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{
    bench_scanner_table_filter_cached, bench_scanner_table_refresh, scanner_panel_key_bindings,
    ScannerAddNodes, ScannerBatchEdit, ScannerDragAddress, ScannerEdit, ScannerNav, ScannerPanel,
};

#[cfg(feature = "ui")]
mod view;

#[cfg(test)]
mod tests {
    use super::{
        apply_change_all_results, compute_delta, delete_rows, deserialize_results_json,
        filter_rows, first_scan_status, format_addresses_for_clipboard, format_float, format_value,
        parse_change_all_bytes, previous_delta_text, rescan_status, serialize_results_json,
        shortcut_scan_target, split_address_dim, stage_breadcrumb, truncation_banner,
        value_type_entries, CondEntry, ScanMode, ScanRow, ScanShortcut, ScannerForm,
        FAST_SCAN_ALIGNMENTS, MAX_DISPLAY_ROWS,
    };
    use crate::scanner::{ScanCondition, ScanResult, ValueType};
    use crate::theme::manager::MemSettings;

    // ── Smart filter defaults per scan mode (onModeChanged) ──

    /// Entering Value mode applies the C++ `onModeChanged(1)` defaults
    /// (scannerpanel.cpp:1135-1149): exec off, write on, and the three
    /// scope toggles reset to off. `struct_only` and `alignment` are untouched.
    #[test]
    fn apply_mode_defaults_value() {
        let mut form = ScannerForm::new();
        // Dirty every filter + the untouched fields so we prove the reset.
        form.filter_executable = true;
        form.filter_writable = false;
        form.private_only = true;
        form.skip_system_modules = true;
        form.user_mode_only = true;
        form.struct_only = true;
        form.alignment = 32;

        form.apply_mode_defaults(ScanMode::Value);

        assert!(!form.filter_executable, "Value mode: exec off");
        assert!(form.filter_writable, "Value mode: write on");
        assert!(!form.private_only);
        assert!(!form.skip_system_modules);
        assert!(!form.user_mode_only);
        // Not touched by onModeChanged.
        assert!(form.struct_only, "struct_only untouched");
        assert_eq!(form.alignment, 32, "alignment untouched");
    }

    /// Entering Signature mode applies the C++ `onModeChanged(0)` defaults:
    /// exec on, write off, scope toggles off; struct_only/alignment untouched.
    #[test]
    fn apply_mode_defaults_signature() {
        let mut form = ScannerForm::new();
        form.filter_executable = false;
        form.filter_writable = true;
        form.private_only = true;
        form.skip_system_modules = true;
        form.user_mode_only = true;
        form.struct_only = true;
        form.alignment = 16;

        form.apply_mode_defaults(ScanMode::Signature);

        assert!(form.filter_executable, "Signature mode: exec on");
        assert!(!form.filter_writable, "Signature mode: write off");
        assert!(!form.private_only);
        assert!(!form.skip_system_modules);
        assert!(!form.user_mode_only);
        assert!(form.struct_only, "struct_only untouched");
        assert_eq!(form.alignment, 16, "alignment untouched");
    }

    // ── Ctrl+Return scan-target preference (scanShortcut) ──

    /// The C++ `scanShortcut` truth table (scannerpanel.cpp:832-838): mid-scan is
    /// a no-op; with results a Next Scan; otherwise a First Scan.
    #[test]
    fn shortcut_scan_target_truth_table() {
        // Running → no-op regardless of results.
        assert_eq!(shortcut_scan_target(true, false), ScanShortcut::None);
        assert_eq!(shortcut_scan_target(true, true), ScanShortcut::None);
        // Idle with results → Next Scan.
        assert_eq!(shortcut_scan_target(false, true), ScanShortcut::NextScan);
        // Idle without results → First Scan.
        assert_eq!(shortcut_scan_target(false, false), ScanShortcut::RunScan);
    }

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
    fn compute_delta_int64_extremes_wrap_without_panic() {
        // prev/cur differing by more than i64::MAX would overflow a checked `b - a`
        // and panic in debug builds; wrapping_sub yields the same value release
        // produces. i64::MAX - i64::MIN wraps to -1.
        let d = compute_delta(
            ValueType::Int64,
            &i64::MIN.to_le_bytes(),
            &i64::MAX.to_le_bytes(),
        );
        assert!(d.ok);
        assert_eq!(d.direction, -1);
        assert_eq!(d.text, "-1");
        // The mirrored direction (MIN - MAX) wraps to +1.
        let d = compute_delta(
            ValueType::Int64,
            &i64::MAX.to_le_bytes(),
            &i64::MIN.to_le_bytes(),
        );
        assert!(d.ok);
        assert_eq!(d.direction, 1);
        assert_eq!(d.text, "+1");
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
    fn first_scan_status_zero_gives_widen_hint() {
        // 0-result first scan surfaces the C++ "try widening the filters"
        // guidance (scannerpanel.cpp:1473-1475), not a bare "0 results".
        assert_eq!(
            first_scan_status(0),
            "0 results — try widening the filters above or checking the value/pattern."
        );
        // Non-empty scans keep the plain count.
        assert_eq!(first_scan_status(1), "1 result");
        assert_eq!(first_scan_status(42), "42 results");
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

    #[test]
    fn deserialize_results_json_round_trips() {
        let rows = vec![
            (0x401000u64, vec![0x39, 0x05, 0x00, 0x00], String::new()),
            (0x7ff0u64, vec![0xFFu8], "game.exe".to_string()),
        ];
        let json = serialize_results_json(ScanMode::Value, ValueType::Int32, &rows);
        let doc = deserialize_results_json(&json);
        let parsed = &doc.results;
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].address, 0x401000);
        assert_eq!(&parsed[0].scan_value[..], &[0x39, 0x05, 0x00, 0x00]);
        assert_eq!(parsed[0].region_module, "");
        assert_eq!(parsed[1].address, 0x7ff0);
        assert_eq!(&parsed[1].scan_value[..], &[0xFF]);
        assert_eq!(parsed[1].region_module, "game.exe");
        // The saved scan mode + value type round-trip (the C++ loadResultsFrom
        // restoring m_lastScanMode / m_lastValueType).
        assert_eq!(doc.scan_mode, ScanMode::Value);
        assert_eq!(doc.value_type, ValueType::Int32);
    }

    #[test]
    fn deserialize_results_json_restores_mode_and_type() {
        // A saved uint64 Value scan must round-trip its scanMode + valueType so
        // the Value column formats with the saved type, not the form default.
        let rows = vec![(
            0x1000u64,
            0xDEADBEEFu64.to_le_bytes().to_vec(),
            String::new(),
        )];
        let json = serialize_results_json(ScanMode::Value, ValueType::UInt64, &rows);
        let doc = deserialize_results_json(&json);
        assert_eq!(doc.scan_mode, ScanMode::Value);
        assert_eq!(doc.value_type, ValueType::UInt64);
        assert_eq!(doc.results.len(), 1);

        // Signature mode round-trips as scanMode 0.
        let sig = serialize_results_json(ScanMode::Signature, ValueType::Int32, &[]);
        let sig_doc = deserialize_results_json(&sig);
        assert_eq!(sig_doc.scan_mode, ScanMode::Signature);

        // A document with no scanMode/valueType keys defaults the C++ way
        // (scanMode 0 = Signature, valueType int32).
        let bare = "{\"results\":[]}";
        let bare_doc = deserialize_results_json(bare);
        assert_eq!(bare_doc.scan_mode, ScanMode::Signature);
        assert_eq!(bare_doc.value_type, ValueType::Int32);
    }

    #[test]
    fn deserialize_results_json_empty_and_malformed() {
        // No results array → empty.
        assert!(deserialize_results_json("{}").results.is_empty());
        // Empty results list.
        let empty = serialize_results_json(ScanMode::Value, ValueType::Int32, &[]);
        assert!(deserialize_results_json(&empty).results.is_empty());
        // Garbage → empty, no panic.
        assert!(deserialize_results_json("not json at all")
            .results
            .is_empty());
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
    fn default_form_matches_cpp_first_scan() {
        // The C++ Fast-Scan default is index 1 = 4 (dword), and the Value-mode
        // constructor checks the Writable chip before the first scan, while the
        // Executable chip stays unchecked (scannerpanel.cpp:529,720).
        let f = ScannerForm::new();
        assert_eq!(f.alignment, 4);
        assert!(f.filter_writable, "Value scans default to writable-only");
        assert!(!f.filter_executable);
        assert!(FAST_SCAN_ALIGNMENTS.contains(&f.alignment));
    }

    #[test]
    fn default_first_scan_uses_dword_stride_and_writable_filter() {
        // A fresh sub-dword (int8) Value scan still strides by 4 (the default
        // alignment, floored at the type's natural alignment of 1) and filters
        // to writable memory — not stride 1 over all readable memory.
        let mut f = ScannerForm::new();
        f.value_type = ValueType::Int8;
        f.value_text = "7".to_string();
        let req = f.build_request(8, None).expect("ok");
        assert_eq!(req.alignment, 4);
        assert!(req.filter_writable);
        assert!(!req.filter_executable);
    }

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

    #[test]
    fn apply_change_all_results_updates_ordered_rows_directly() {
        let mut results = vec![
            ScanResult {
                address: 0x1000,
                scan_value: vec![1].into(),
                previous_value: vec![9].into(),
                ..ScanResult::default()
            },
            ScanResult {
                address: 0x2000,
                scan_value: vec![2].into(),
                previous_value: vec![8].into(),
                ..ScanResult::default()
            },
        ];

        let wrote = apply_change_all_results(
            &mut results,
            vec![(0x1000, Some(vec![0xAA])), (0x2000, Some(vec![0xBB]))],
        );

        assert_eq!(wrote, 2);
        assert_eq!(&results[0].scan_value[..], &[0xAA]);
        assert_eq!(&results[1].scan_value[..], &[0xBB]);
        assert_eq!(&results[0].previous_value[..], &[9]);
    }

    #[test]
    fn apply_change_all_results_indexes_out_of_order_rows_and_counts_successes() {
        let mut results = vec![
            ScanResult {
                address: 0x1000,
                scan_value: vec![1].into(),
                ..ScanResult::default()
            },
            ScanResult {
                address: 0x2000,
                scan_value: vec![2].into(),
                ..ScanResult::default()
            },
        ];

        let wrote = apply_change_all_results(
            &mut results,
            vec![
                (0x2000, Some(vec![0xCC])),
                (0xDEAD, Some(vec![0xDD])),
                (0x1000, None),
            ],
        );

        assert_eq!(wrote, 2);
        assert_eq!(&results[0].scan_value[..], &[1]);
        assert_eq!(&results[1].scan_value[..], &[0xCC]);
    }

    #[test]
    fn apply_change_all_results_duplicate_addresses_update_first_result() {
        let mut results = vec![
            ScanResult {
                address: 0x1000,
                scan_value: vec![1].into(),
                ..ScanResult::default()
            },
            ScanResult {
                address: 0x1000,
                scan_value: vec![2].into(),
                ..ScanResult::default()
            },
        ];

        let wrote = apply_change_all_results(&mut results, vec![(0x1000, Some(vec![0xEE]))]);

        assert_eq!(wrote, 1);
        assert_eq!(&results[0].scan_value[..], &[0xEE]);
        assert_eq!(&results[1].scan_value[..], &[2]);
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
            scan_value: 1i32.to_le_bytes().to_vec().into(),
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
        assert_eq!(entries[9].0, ValueType::Double);
        // The port additionally surfaces the engine's vector + string types
        // (raw gap 5): Vec2/3/4 then UTF8/UTF16/HexBytes.
        assert_eq!(entries[10].0, ValueType::Vec2);
        assert_eq!(entries.last().unwrap().0, ValueType::HexBytes);
        assert_eq!(entries.last().unwrap().1, "hex bytes");
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

    // ── format_float honors the %g precision argument (raw gap 8) ──

    #[test]
    fn format_float_matches_libc_g_precision() {
        // float column uses %g,9 — the canonical 0.3f rounding case.
        assert_eq!(format_float(0.3f32 as f64, 9), "0.300000012");
        // whole numbers strip the trailing ".0".
        assert_eq!(format_float(1.0, 9), "1");
        // double column uses %g,17 — full round-trip precision.
        assert_eq!(format_float(0.1, 17), "0.10000000000000001");
        assert_eq!(format_float(1.5, 17), "1.5");
        // delta uses %g,6.
        assert_eq!(format_float(0.5, 6), "0.5");
        assert_eq!(format_float(-0.5, 6), "-0.5");
        // scientific-notation crossover (X >= P) → %e with libc exponent shape.
        assert_eq!(format_float(1_000_000.0, 6), "1e+06");
        assert_eq!(format_float(1234567.0, 6), "1.23457e+06");
        assert_eq!(format_float(123456789.0, 6), "1.23457e+08");
        // small magnitude stays %f down to X == -4.
        assert_eq!(format_float(0.0001234567, 6), "0.000123457");
        // zero is a bare "0".
        assert_eq!(format_float(0.0, 9), "0");
    }

    #[test]
    fn format_float_differs_from_default_display_for_float() {
        // The whole point of honoring the precision arg: default Display would
        // round-trip 0.3f as "0.3", but %g,9 surfaces the float's true value.
        let v = 0.3f32 as f64;
        assert_ne!(format_float(v, 9), format!("{v}"));
        assert_eq!(format_float(v, 9), "0.300000012");
    }

    // ── format_value for the newly-surfaced engine value types (raw gap 5) ──

    #[test]
    fn format_value_vector_and_string_types() {
        // vec2: two LE floats rendered "(x, y)".
        let mut v2 = Vec::new();
        v2.extend_from_slice(&1.0f32.to_le_bytes());
        v2.extend_from_slice(&2.5f32.to_le_bytes());
        assert_eq!(
            format_value(ScanMode::Value, ValueType::Vec2, &[], &v2),
            "(1, 2.5)"
        );
        // utf8 decodes the bytes as text.
        assert_eq!(
            format_value(ScanMode::Value, ValueType::Utf8, &[], b"Hi"),
            "Hi"
        );
        // utf16-LE.
        let u16b: Vec<u8> = "Hi".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(
            format_value(ScanMode::Value, ValueType::Utf16, &[], &u16b),
            "Hi"
        );
        // hex bytes: uppercase space-separated.
        assert_eq!(
            format_value(ScanMode::Value, ValueType::HexBytes, &[], &[0x90, 0xAB]),
            "90 AB"
        );
    }

    // ── Change All Values batch-write parse (feature 2) ──

    #[test]
    fn parse_change_all_value_mode_int32() {
        let b = parse_change_all_bytes(ScanMode::Value, ValueType::Int32, "999").unwrap();
        assert_eq!(b, 999i32.to_le_bytes().to_vec());
    }

    #[test]
    fn parse_change_all_signature_mode_hex_bytes() {
        let b = parse_change_all_bytes(ScanMode::Signature, ValueType::Int32, "90 90 90").unwrap();
        assert_eq!(b, vec![0x90, 0x90, 0x90]);
        // A non-hex / over-255 token is rejected (no wildcards in a write).
        assert!(parse_change_all_bytes(ScanMode::Signature, ValueType::Int32, "??").is_err());
        assert!(parse_change_all_bytes(ScanMode::Signature, ValueType::Int32, "1FF").is_err());
    }

    #[test]
    fn parse_change_all_value_mode_invalid() {
        assert_eq!(
            parse_change_all_bytes(ScanMode::Value, ValueType::Int32, "notanumber"),
            Err("Invalid value".to_string())
        );
    }

    // ── Batch multi-row helpers (feature 5) ──

    #[test]
    fn format_addresses_for_clipboard_newline_joined_upper() {
        let s = format_addresses_for_clipboard(&[0x1000, 0xDEAD_BEEF, 0xab]);
        assert_eq!(s, "0x1000\n0xDEADBEEF\n0xAB");
    }

    #[test]
    fn delete_rows_removes_by_index_descending() {
        let mk = |a: u64| ScanResult {
            address: a,
            ..ScanResult::default()
        };
        let results = vec![mk(1), mk(2), mk(3), mk(4)];
        // Remove rows 1 and 3 (addresses 2 and 4); 0 and 2 survive.
        let kept = delete_rows(results, &[3, 1, 3]); // dup + out-of-order tolerated
        let addrs: Vec<u64> = kept.iter().map(|r| r.address).collect();
        assert_eq!(addrs, vec![1, 3]);
    }

    // ── First-scan ExactValue override (raw gap 7) ──
    //
    // build_request records last_condition; for value-mode ExactValue the
    // finish-first-scan loop should replace each result's cached chunk with the
    // searched pattern. Exercised here at the snapshot level: a value-mode
    // ExactValue build records ExactValue + the searched bytes as last_pattern.

    #[test]
    fn build_request_records_exact_value_snapshot() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::ExactValue);
        f.value_type = ValueType::Int32;
        f.value_text = "1337".to_string();
        let _ = f.build_request(8, None).unwrap();
        assert_eq!(f.last_mode(), ScanMode::Value);
        assert_eq!(f.last_condition(), ScanCondition::ExactValue);
        assert_eq!(f.last_pattern(), 1337i32.to_le_bytes());
    }

    #[test]
    fn build_request_changed_records_unknown_condition() {
        // Changed/Increased/etc. remap to UnknownValue, so last_condition is NOT
        // ExactValue and the override must not fire.
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::Changed);
        f.value_type = ValueType::Int32;
        let _ = f.build_request(8, None).unwrap();
        assert_eq!(f.last_condition(), ScanCondition::UnknownValue);
    }

    // ── QSettings-equivalent form persistence (raw gap 6) ──

    #[test]
    fn settings_round_trip_preserves_form() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Value(ScanCondition::BiggerThan);
        f.value_type = ValueType::Float;
        f.filter_executable = true;
        f.filter_writable = false;
        f.private_only = true;
        f.skip_system_modules = true;
        f.user_mode_only = true;

        let mut store = MemSettings::new();
        f.save_settings("scanner", &mut store);

        let mut g = ScannerForm::new();
        g.load_settings("scanner", &store);
        assert_eq!(g.condition, CondEntry::Value(ScanCondition::BiggerThan));
        assert_eq!(g.value_type, ValueType::Float);
        assert!(g.filter_executable);
        assert!(!g.filter_writable);
        assert!(g.private_only);
        assert!(g.skip_system_modules);
        assert!(g.user_mode_only);
    }

    #[test]
    fn settings_round_trip_signature_mode() {
        let mut f = ScannerForm::new();
        f.condition = CondEntry::Signature;
        let mut store = MemSettings::new();
        f.save_settings("scanner", &mut store);

        let mut g = ScannerForm::new();
        g.load_settings("scanner", &store);
        assert_eq!(g.mode(), ScanMode::Signature);
        assert_eq!(g.condition, CondEntry::Signature);
    }

    #[test]
    fn settings_load_missing_keys_leaves_defaults() {
        let store = MemSettings::new();
        let mut g = ScannerForm::new();
        let before = (g.condition, g.value_type, g.filter_writable);
        g.load_settings("scanner", &store);
        assert_eq!((g.condition, g.value_type, g.filter_writable), before);
    }
}
