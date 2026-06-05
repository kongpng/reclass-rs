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
            let mut s = String::new();
            for (j, b) in bytes.iter().enumerate() {
                if j > 0 {
                    s.push(' ');
                }
                s.push_str(&format!("{b:02X}"));
            }
            return s;
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
                                scan_value: value,
                                previous_value: Vec::new(),
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
    scanner_panel_key_bindings, ScannerAddNodes, ScannerBatchEdit, ScannerDragAddress, ScannerEdit,
    ScannerNav, ScannerPanel,
};

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
    use gpui_component::menu::PopupMenu;
    use gpui_component::popover::Popover;
    use gpui_component::table::{
        Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
    };
    use gpui_component::{Disableable as _, Sizable as _};

    // ── Result-row context-menu actions (the C++ `customContextMenuRequested`
    // menu, scannerpanel.cpp:899-993). Dispatched by the `PopupMenu` the table
    // delegate builds; the panel records the right-clicked row in
    // `context_target` (from `TableEvent::RightClickedRow`) so each handler knows
    // which result it targets, then runs the action against `self.results`. ──
    gpui::actions!(
        rcx_scanner,
        [
            ScCopyAddress,
            ScCopyValue,
            ScSetBaseAddress,
            ScChangeAllValues,
            // ── Panel-scoped keyboard shortcuts (scannerpanel.cpp:830-870) ──
            // Ctrl+Return: First Scan, or Next Scan when results exist.
            ScScanOrRescan,
            // F5: Next Scan (re-scan), enabled only when results exist.
            ScRescan,
            // Ctrl+Z: Undo Scan, enabled only when the undo stack is non-empty.
            ScUndo,
            // Ctrl+L: focus + select-all the result filter.
            ScFocusFilter
        ]
    );

    use super::{
        compute_delta, format_addresses_for_clipboard, parse_change_all_bytes, previous_delta_text,
        rescan_status, serialize_results_json, shortcut_scan_target, split_address_dim,
        stage_breadcrumb, truncation_banner, value_type_entries, CondEntry, ScanMode, ScanRow,
        ScanShortcut, ScannerForm, FAST_SCAN_ALIGNMENTS, MAX_DISPLAY_ROWS,
    };

    /// The scanner panel's keyboard shortcuts, bound in the `RcxScanner` context
    /// (the C++ panel-scoped `QShortcut`s, scannerpanel.cpp:830-868). Registered
    /// once at startup via `window::collect_key_bindings`. Both the Ctrl and Cmd
    /// variants are bound so the muscle memory works on macOS too.
    pub fn scanner_panel_key_bindings() -> Vec<KeyBinding> {
        vec![
            // Ctrl+Return — First Scan / Next Scan (the C++ `scanShortcut`).
            KeyBinding::new("ctrl-enter", ScScanOrRescan, Some("RcxScanner")),
            KeyBinding::new("cmd-enter", ScScanOrRescan, Some("RcxScanner")),
            // F5 — Next Scan (the C++ `rescanShortcut`).
            KeyBinding::new("f5", ScRescan, Some("RcxScanner")),
            // Ctrl+Z — Undo Scan (the C++ `undoShortcut`).
            KeyBinding::new("ctrl-z", ScUndo, Some("RcxScanner")),
            KeyBinding::new("cmd-z", ScUndo, Some("RcxScanner")),
            // Ctrl+L — focus the result filter (the C++ `focusFilter`).
            KeyBinding::new("ctrl-l", ScFocusFilter, Some("RcxScanner")),
            KeyBinding::new("cmd-l", ScFocusFilter, Some("RcxScanner")),
        ]
    }
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

    /// A batch Change-All-Values write intent (the C++ "Change All Values"
    /// context action). Raised as a SEPARATE event type from [`ScannerEdit`] so
    /// the panel can grow this capability without forcing every existing
    /// `match ScannerEdit` site to add an arm. The window writes `bytes` to every
    /// `addresses` entry through its mutable provider, re-reads each, and hands
    /// the new bytes back via [`ScannerPanel::apply_change_all`] (the panel held
    /// provider handle is read-only, so it cannot write directly).
    #[derive(Clone, Debug)]
    pub struct ScannerBatchEdit {
        /// Every current result address (the C++ `for (auto& r : m_results)`).
        pub addresses: Vec<u64>,
        /// The bytes to write, already parsed per the last scan mode / type.
        pub bytes: Vec<u8>,
    }

    /// An "add these result addresses as nodes" intent (the C++ drag-row-into-
    /// editor / add-as-nodes path). The window appends each address as a node in
    /// the active editor. A separate event type (like [`ScannerBatchEdit`]) so it
    /// doesn't disturb existing `ScannerEdit` match sites.
    #[derive(Clone, Debug)]
    pub struct ScannerAddNodes {
        /// The addresses to add (in displayed order).
        pub addresses: Vec<u64>,
    }

    /// The result columns. The C++ scanner table is 2 columns (Address, Value);
    /// PIC6 additionally surfaces a Previous→Δ column (the pre-rescan value with
    /// its signed delta) and a Module column (when any result falls in a known
    /// module), so the port shows up to four: Address / Value / Previous / Module.
    const COL_ADDRESS: usize = 0;
    const COL_VALUE: usize = 1;
    const COL_PREVIOUS: usize = 2;

    /// The drag payload a result row carries when dragged out of the scanner
    /// (the C++ `setDragEnabled(true)` + the QMimeData address). An editor drop
    /// target reads `address` to add it as a node (the C++ "drag a row into the
    /// editor to add a node" affordance). The editor drop handler lives in the
    /// editor surface (another module); this is the source side + the contract.
    #[derive(Clone, Debug)]
    pub struct ScannerDragAddress {
        pub address: u64,
    }

    /// The drag preview shown under the cursor while dragging a result row out —
    /// the formatted address in a small elevated pill (the visual stand-in for
    /// the dragged node).
    struct ScannerDragPreview {
        address_text: SharedString,
    }

    impl Render for ScannerDragPreview {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .px(px(tokens::space::MD))
                .py(px(tokens::space::XS))
                .rounded(px(tokens::radius::MD))
                .bg(color::elevated_bg(cx))
                .border_1()
                .border_color(color::border(cx))
                .font_family(tokens::font::mono_family())
                .text_size(px(tokens::font::EDITOR_SIZE))
                .text_color(color::text(cx))
                .child(self.address_text.clone())
        }
    }

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
        /// The total live result count (the C++ `m_results.size()`), shown in the
        /// "Change All Values (N)" context-menu label. The delegate only holds
        /// the displayed (capped/filtered) subset, so the full count is fed in.
        result_count: usize,
    }

    impl ScanResultsDelegate {
        fn new() -> Self {
            ScanResultsDelegate {
                rows: Vec::new(),
                show_previous: false,
                show_module: false,
                result_count: 0,
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

        /// The result-row right-click menu (the C++ `customContextMenuRequested`
        /// handler, scannerpanel.cpp:904-916): Copy Address (0xUPPER), Copy Value,
        /// Set as Base Address, a separator, then Change All Values (N). The menu
        /// dispatches the `rcx_scanner` actions; the panel root's `.on_action`
        /// handlers resolve them against the recorded `context_target` row.
        fn context_menu(
            &mut self,
            _row_ix: usize,
            menu: PopupMenu,
            _window: &mut Window,
            _cx: &mut Context<TableState<Self>>,
        ) -> PopupMenu {
            menu.menu("Copy Address", Box::new(ScCopyAddress))
                .menu("Copy Value", Box::new(ScCopyValue))
                .menu("Set as Base Address", Box::new(ScSetBaseAddress))
                .separator()
                .menu(
                    format!("Change All Values ({})", self.result_count),
                    Box::new(ScChangeAllValues),
                )
        }

        /// The row container — carries the drag-out source (the C++
        /// `setDragEnabled(true)`). Dragging a row hands a [`ScannerDragAddress`]
        /// to whatever drop target accepts it (the editor adds it as a node).
        fn render_tr(
            &mut self,
            row_ix: usize,
            _window: &mut Window,
            _cx: &mut Context<TableState<Self>>,
        ) -> Stateful<Div> {
            let mut row = div().id(("scanner-result-row", row_ix));
            if let Some(d) = self.rows.get(row_ix) {
                let address = d.row.address;
                let address_text: SharedString = d.row.address_text.clone().into();
                row = row.on_drag(
                    ScannerDragAddress { address },
                    move |_payload, _offset, _window, cx| {
                        let address_text = address_text.clone();
                        cx.new(|_| ScannerDragPreview { address_text })
                    },
                );
            }
            row
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
                    .font_family(tokens::font::mono_family())
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
                    .font_family(tokens::font::mono_family())
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
        /// The Reset two-click confirm flag (the C++ `resetArmed` dynamic
        /// property): when a large result list (≥ [`RESET_CONFIRM_THRESHOLD`]) is
        /// about to be discarded, the first Reset click only arms this + relabels
        /// the button; the second click within the cooldown commits.
        reset_armed: bool,
        /// Monotonic generation counter for the reset-cooldown timer so a stale
        /// 4 s disarm callback can't clobber a freshly re-armed state.
        reset_arm_gen: u64,
        /// The right-clicked result row recorded on `RightClickedRow` so the
        /// context-menu actions know which result they target (the C++
        /// `customContextMenuRequested` → `rowToResultIdx`). Indexes the
        /// displayed (filtered/sorted) row list.
        context_target: Option<usize>,
        /// The active editor's view-root span `(start, size)` for the
        /// "Current struct" scan-filter chip (the C++ `m_boundsGetter`,
        /// scannerpanel.cpp:1336-1342). The window sets this from the active
        /// document; `run_scan` passes it to `build_request` so the `struct_only`
        /// chip can actually clamp the scan range. `None` (no editor / no
        /// boundsGetter) leaves the chip a no-op, like the C++ null-getter path.
        struct_bounds: Option<(u64, u64)>,
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

    /// The undo-stack cap (the C++ `kMaxUndo`).
    const MAX_UNDO: usize = 16;

    /// Reset two-click confirm threshold (the C++ `kConfirmThreshold`): at or
    /// above this result count the first Reset click only arms the confirm.
    const RESET_CONFIRM_THRESHOLD: usize = 1000;

    /// Reset confirm cooldown in milliseconds (the C++ 4 s `QTimer::singleShot`).
    const RESET_CONFIRM_COOLDOWN_MS: u64 = 4000;

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
                    // Record the right-clicked row before the context-menu action
                    // fires so Copy/Set-base know which result they target (the
                    // C++ `rowAt(pos.y())` → `rowToResultIdx`). `None` (empty area)
                    // falls back to the current selection.
                    TableEvent::RightClickedRow(row_ix) => {
                        this.context_target = row_ix.or(this.selected_row);
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
                reset_armed: false,
                reset_arm_gen: 0,
                context_target: None,
                struct_bounds: None,
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

        /// Set the active editor's view-root span `(start, size)` for the
        /// "Current struct" scan-filter chip (the C++ `setBoundsGetter`,
        /// scannerpanel.cpp:1065-1074). The window calls this from the active
        /// document's struct bounds; `run_scan` passes the value to
        /// `build_request` so the `struct_only` chip clamps the scan range.
        ///
        /// Mirrors the C++ null-getter handling: when `bounds` is `None` (no
        /// editor / empty view), the chip is disabled and force-unchecked so a
        /// checked chip can't silently degrade to a full scan.
        pub fn set_struct_bounds(&mut self, bounds: Option<(u64, u64)>, cx: &mut Context<Self>) {
            self.struct_bounds = bounds.filter(|(_, size)| *size > 0);
            if self.struct_bounds.is_none() {
                self.form.struct_only = false;
            }
            cx.notify();
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

        /// Persist the scanner form (mode / value-type / condition / filters) to
        /// the app settings store under the scanner group (the C++
        /// `saveSettings`). The window calls this on close / settings flush.
        pub fn save_settings<S: crate::theme::manager::SettingsStore + ?Sized>(
            &self,
            store: &mut S,
        ) {
            self.form.save_settings(super::settings_keys::GROUP, store);
        }

        /// Restore the scanner form from the app settings store (the C++
        /// `loadSettings`). The window calls this once after constructing the
        /// panel so the controls reflect the user's last session, then syncs the
        /// value input + notifies.
        pub fn load_settings<S: crate::theme::manager::SettingsStore + ?Sized>(
            &mut self,
            store: &S,
            cx: &mut Context<Self>,
        ) {
            self.form.load_settings(super::settings_keys::GROUP, store);
            cx.notify();
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
            // Smart filter defaults per mode (the C++ `onModeChanged`,
            // scannerpanel.cpp:1135-1149). This is the only mode-change call site
            // (the scan-type dropdown); picking a *condition* (`set_condition`)
            // must NOT re-apply filter defaults — it only re-derives field
            // visibility, which `field_visibility()` recomputes each render.
            self.form.apply_mode_defaults(mode);
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
            // Supply the active editor's struct bounds so the "Current struct"
            // chip can clamp the scan range (the C++ `m_boundsGetter`,
            // scannerpanel.cpp:1336-1342); `None` leaves it a no-op.
            let req = match self.form.build_request(ptr_size, self.struct_bounds) {
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
            // Bytes are cached by the engine during the scan. Value-mode
            // ExactValue scans override the cached raw chunk with the exact
            // search pattern so the Value column shows the searched value rather
            // than the leading bytes of the chunk (the C++ onScanFinished loop,
            // scannerpanel.cpp:1451-1455). Unknown / signature / compare modes
            // keep the engine-captured bytes as the baseline. Previous is always
            // cleared on a first scan.
            let override_exact = form.last_mode() == ScanMode::Value
                && form.last_condition() == ScanCondition::ExactValue
                && !form.last_pattern().is_empty();
            for r in &mut results {
                r.previous_value.clear();
                if override_exact {
                    r.scan_value = form.last_pattern().to_vec();
                }
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

        /// Run a typed value scan SYNCHRONOUSLY and return its results (the C++
        /// `ScannerPanel::runValueScanAndWait`, scannerpanel.cpp:1347-1388). Built
        /// for MCP / automation callers that need the result list inline rather
        /// than via the async UI path: it serializes `value` per `value_type`,
        /// runs [`run_scan`] on the attached provider, populates the panel table
        /// (so the UI reflects the automated scan), and hands the results back.
        ///
        /// Returns an empty list (and sets the status) when there is no provider,
        /// a scan is already running, or the value fails to parse — matching the
        /// C++ early-returns. `constrain_regions` limits the scan to those address
        /// ranges (intersected with the provider's regions).
        pub fn run_value_scan_and_wait(
            &mut self,
            value_type: ValueType,
            value: &str,
            filter_executable: bool,
            filter_writable: bool,
            constrain_regions: Vec<crate::scanner::AddressRange>,
            cx: &mut Context<Self>,
        ) -> Vec<ScanResult> {
            let (pattern, mask) = match serialize_value(value_type, value) {
                Ok(pm) => pm,
                Err(e) => {
                    self.status = format!("Value error: {e}");
                    cx.notify();
                    return Vec::new();
                }
            };
            let mut req = crate::scanner::ScanRequest {
                pattern,
                mask,
                alignment: crate::scanner::natural_alignment(value_type),
                filter_executable,
                filter_writable,
                constrain_regions,
                value_size: value_size_for_type(value_type),
                value_type,
                ..Default::default()
            };
            req.condition = ScanCondition::ExactValue;
            self.run_blocking_scan(req, ScanMode::Value, value_type, cx)
        }

        /// Run a byte-pattern (signature) scan SYNCHRONOUSLY and return its
        /// results (the C++ `ScannerPanel::runPatternScanAndWait`,
        /// scannerpanel.cpp:1390-1437). Parses `pattern` via [`parse_signature`]
        /// (`??` wildcards allowed), runs [`run_scan`] at byte alignment, and
        /// populates the panel table. Same early-return contract as
        /// [`run_value_scan_and_wait`](Self::run_value_scan_and_wait).
        pub fn run_pattern_scan_and_wait(
            &mut self,
            pattern: &str,
            filter_executable: bool,
            filter_writable: bool,
            constrain_regions: Vec<crate::scanner::AddressRange>,
            cx: &mut Context<Self>,
        ) -> Vec<ScanResult> {
            let (pat, mask) = match crate::scanner::parse_signature(pattern) {
                Ok(pm) => pm,
                Err(e) => {
                    self.status = format!("Pattern error: {e}");
                    cx.notify();
                    return Vec::new();
                }
            };
            let req = crate::scanner::ScanRequest {
                pattern: pat,
                mask,
                alignment: 1,
                filter_executable,
                filter_writable,
                constrain_regions,
                condition: ScanCondition::ExactValue,
                ..Default::default()
            };
            self.run_blocking_scan(req, ScanMode::Signature, ValueType::Int32, cx)
        }

        /// The shared blocking-scan kernel for the automation entry points: guards
        /// the provider + running state, runs [`run_scan`] inline (no worker
        /// thread — the C++ `QEventLoop` blocked, so this blocks the caller too),
        /// records the last-scan snapshot for value formatting, populates the
        /// table, and returns the results.
        fn run_blocking_scan(
            &mut self,
            req: crate::scanner::ScanRequest,
            mode: ScanMode,
            value_type: ValueType,
            cx: &mut Context<Self>,
        ) -> Vec<ScanResult> {
            let Some(provider) = self.provider.clone() else {
                self.status = "No provider (attach to a process or open a file first)".to_string();
                cx.notify();
                return Vec::new();
            };
            if self.scanning {
                self.status = "Scan already in progress".to_string();
                cx.notify();
                return Vec::new();
            }

            let abort = AtomicBool::new(false);
            let obs = NullObserver;
            let mut results = run_scan(provider.as_ref(), &req, &abort, &obs);

            // Record the last-scan snapshot so the table value column formats
            // correctly + the ExactValue override applies (the C++ sets
            // m_lastScanMode / m_lastValueType / m_lastPattern before the loop).
            self.form
                .set_last_scan(mode, value_type, ScanCondition::ExactValue, &req.pattern);
            let override_exact = mode == ScanMode::Value && !req.pattern.is_empty();
            for r in &mut results {
                r.previous_value.clear();
                if override_exact {
                    r.scan_value = req.pattern.clone();
                }
            }

            self.results = results.clone();
            self.show_previous = false;
            self.undo_stack.clear();
            self.generation = 1;
            self.last_result_count = 0;
            self.selected_row = None;
            self.scanning = false;
            let n = results.len();
            self.status = if n == 1 {
                "1 result".to_string()
            } else {
                format!("{n} results")
            };
            self.refresh_table(cx);
            cx.notify();
            results
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
        ///
        /// Two-click guard (the C++ `kConfirmThreshold` flow,
        /// scannerpanel.cpp:2176-2220): when the live result list is large
        /// (≥ [`RESET_CONFIRM_THRESHOLD`]) the FIRST click only arms the confirm —
        /// it relabels the button to "Click again to reset" and starts a 4 s
        /// cooldown after which the arm auto-clears. A SECOND click while armed
        /// commits the reset. Lists under the threshold reset immediately (the
        /// CE-like pace for typical narrowing chains).
        fn reset(&mut self, cx: &mut Context<Self>) {
            if self.results.len() >= RESET_CONFIRM_THRESHOLD && !self.reset_armed {
                self.reset_armed = true;
                self.reset_arm_gen = self.reset_arm_gen.wrapping_add(1);
                let gen = self.reset_arm_gen;
                self.status = format!(
                    "About to discard {} results — click Reset again to confirm.",
                    self.results.len()
                );
                cx.notify();
                // Disarm after the cooldown unless the button was clicked again
                // (a fresh arm bumps `reset_arm_gen`, so a stale callback no-ops).
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(RESET_CONFIRM_COOLDOWN_MS))
                        .await;
                    this.update(cx, |this, cx| {
                        if this.reset_armed && this.reset_arm_gen == gen {
                            this.reset_armed = false;
                            // Restore the prior result-count status (the confirm
                            // prompt above overwrote it).
                            let n = this.results.len();
                            this.status = if n == 1 {
                                "1 result".to_string()
                            } else {
                                format!("{n} results")
                            };
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
                return;
            }

            self.reset_armed = false;
            self.results.clear();
            self.undo_stack.clear();
            self.show_previous = false;
            self.generation = 0;
            self.last_result_count = 0;
            self.status.clear();
            self.selected_row = None;
            self.context_target = None;
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

        /// The displayed-row payload (address + formatted value) the context-menu
        /// actions target — resolved through the table delegate so it honors the
        /// current sort/filter (the C++ `rowToResultIdx`). Falls back to the
        /// selected row when no explicit right-click target was recorded.
        fn context_row(&self, cx: &App) -> Option<(u64, String)> {
            let ix = self.context_target.or(self.selected_row)?;
            self.table
                .read(cx)
                .delegate()
                .rows
                .get(ix)
                .map(|r| (r.row.address, r.row.value_text.clone()))
        }

        /// Context menu ▸ Copy Address — copy the right-clicked row's address as
        /// `0xUPPER` (the C++ `copyAddr` action, scannerpanel.cpp:917-921).
        /// Ctrl+Return — First Scan, or Next Scan once results exist (the C++
        /// `scanShortcut`, scannerpanel.cpp:832-838). A no-op while a scan runs.
        fn act_scan_or_rescan(
            &mut self,
            _: &ScScanOrRescan,
            _w: &mut Window,
            cx: &mut Context<Self>,
        ) {
            match shortcut_scan_target(self.scanning, !self.results.is_empty()) {
                ScanShortcut::NextScan => self.next_scan(cx),
                ScanShortcut::RunScan => self.run_scan(cx),
                ScanShortcut::None => {}
            }
        }

        /// F5 — Next Scan (re-scan). Enabled only when not scanning and results
        /// exist (the C++ `rescanShortcut` guarded by `m_updateBtn->isEnabled()`,
        /// scannerpanel.cpp:840-843).
        fn act_rescan(&mut self, _: &ScRescan, _w: &mut Window, cx: &mut Context<Self>) {
            if !self.scanning && !self.results.is_empty() {
                self.next_scan(cx);
            }
        }

        /// Ctrl+Z — Undo Scan. Enabled only when the undo stack is non-empty (the
        /// C++ `undoShortcut` guarded by `m_undoBtn->isEnabled()`,
        /// scannerpanel.cpp:864-868).
        fn act_undo(&mut self, _: &ScUndo, _w: &mut Window, cx: &mut Context<Self>) {
            if !self.undo_stack.is_empty() {
                self.undo_scan(cx);
            }
        }

        /// Ctrl+L — focus + select-all the result filter (the C++ `focusFilter`
        /// shortcut, scannerpanel.cpp:850-855).
        fn act_focus_filter(
            &mut self,
            _: &ScFocusFilter,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.filter_input
                .update(cx, |input, cx| input.focus(window, cx));
            // `InputState::select_all` is not public, so dispatch the input's own
            // SelectAll (ctrl-a) keystroke to the now-focused field (mirrors the
            // C++ `m_resultFilter->selectAll()`).
            if let Ok(ks) = Keystroke::parse("ctrl-a") {
                window.dispatch_keystroke(ks, cx);
            }
            cx.notify();
        }

        fn ctx_copy_address(&mut self, _: &ScCopyAddress, _w: &mut Window, cx: &mut Context<Self>) {
            if let Some((address, _)) = self.context_row(cx) {
                cx.write_to_clipboard(ClipboardItem::new_string(format!("0x{address:X}")));
                self.status = format!("Copied: 0x{address:X}");
                cx.notify();
            }
        }

        /// Context menu ▸ Copy Value — copy the right-clicked row's formatted
        /// value (the C++ `copyVal` action, scannerpanel.cpp:922-924).
        fn ctx_copy_value(&mut self, _: &ScCopyValue, _w: &mut Window, cx: &mut Context<Self>) {
            if let Some((_, value)) = self.context_row(cx) {
                cx.write_to_clipboard(ClipboardItem::new_string(value));
                self.status = "Copied value".to_string();
                cx.notify();
            }
        }

        /// Context menu ▸ Set as Base Address — rebase the active editor tab onto
        /// the right-clicked row's address (the C++ `goTo` action emitting
        /// `goToAddress`, scannerpanel.cpp:925-926). Raises [`ScannerNav`].
        fn ctx_set_base_address(
            &mut self,
            _: &ScSetBaseAddress,
            _w: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if let Some((address, _)) = self.context_row(cx) {
                cx.emit(ScannerNav { address });
            }
        }

        /// Context menu ▸ Change All Values (N) — prompt for a value, parse it per
        /// the last scan mode / value type, and emit a [`ScannerEdit::ChangeAll`]
        /// intent the window resolves against its mutable source (the
        /// read-only-surface pattern the inline value-edit already uses). The C++
        /// (scannerpanel.cpp:927-992) parses + writes to every `m_results` address
        /// inline through the panel's writable provider; here the provider handle
        /// is read-only (`Arc`), so the bytes + addresses are handed to the window
        /// which owns the mutable controller + the re-read / "Wrote to X/Y" report.
        fn ctx_change_all_values(
            &mut self,
            _: &ScChangeAllValues,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            if self.results.is_empty() {
                return;
            }
            let last_mode = self.form.last_mode();
            let last_vt = self.form.last_value_type();
            let prompt = if last_mode == ScanMode::Signature {
                "Change All Values — new hex bytes (e.g. 90 90 90):"
            } else {
                "Change All Values — new value (e.g. 999):"
            };
            // Prompt for the new value. gpui's `prompt` is a simple message box
            // (no text entry), so route the value through the panel's own Value
            // input: the user types the replacement there, then picks the action.
            let text = self.value_input.read(cx).value().to_string();
            if text.trim().is_empty() {
                self.status = format!(
                    "{prompt}  Type the value in the Value field above, then re-run Change All Values."
                );
                cx.notify();
                let _ = window;
                return;
            }
            // Parse the typed value into raw bytes the way a single-cell edit
            // would (the C++ per-mode parse, scannerpanel.cpp:945-978).
            let bytes = match parse_change_all_bytes(last_mode, last_vt, &text) {
                Ok(b) if !b.is_empty() => b,
                Ok(_) => return,
                Err(e) => {
                    self.status = e;
                    cx.notify();
                    return;
                }
            };
            let addresses: Vec<u64> = self.results.iter().map(|r| r.address).collect();
            self.status = format!(
                "Writing {} bytes to {} addresses…",
                bytes.len(),
                addresses.len()
            );
            cx.emit(ScannerBatchEdit { addresses, bytes });
            cx.notify();
        }

        /// Apply the result of a batch Change-All write resolved by the window
        /// (the C++ re-read + "Wrote to X/Y" tail, scannerpanel.cpp:981-991). The
        /// window writes the bytes to every address through its mutable provider,
        /// re-reads each, and hands back the new bytes (or `None` where the write
        /// failed); the panel updates `scan_value`, refreshes, and reports the
        /// success count.
        pub fn apply_change_all(
            &mut self,
            results: Vec<(u64, Option<Vec<u8>>)>,
            cx: &mut Context<Self>,
        ) {
            let total = results.len();
            let mut wrote = 0usize;
            for (addr, new_bytes) in results {
                if let Some(nb) = new_bytes {
                    if let Some(r) = self.results.iter_mut().find(|r| r.address == addr) {
                        r.previous_value = r.scan_value.clone();
                        r.scan_value = nb;
                    }
                    wrote += 1;
                }
            }
            self.status = format!("Wrote to {wrote}/{total} addresses");
            self.refresh_table(cx);
            cx.notify();
        }

        /// "Copy All" footer button — copy every displayed result address to the
        /// clipboard, newline-joined (the C++ multi-row Copy). Honors the current
        /// sort/filter (it reads the displayed-row list).
        fn copy_all_addresses(&mut self, cx: &mut Context<Self>) {
            let addrs: Vec<u64> = self
                .table
                .read(cx)
                .delegate()
                .rows
                .iter()
                .map(|r| r.row.address)
                .collect();
            if addrs.is_empty() {
                return;
            }
            let n = addrs.len();
            cx.write_to_clipboard(ClipboardItem::new_string(format_addresses_for_clipboard(
                &addrs,
            )));
            self.status = format!("Copied {n} addresses");
            cx.notify();
        }

        /// "Delete" footer button — drop the selected result row from the list
        /// (the C++ multi-row Delete; the single-select DataTable limits this to
        /// the one selected row, so it removes that). Resolves the displayed-row
        /// index to its address, removes the matching result, and refreshes.
        fn delete_selected(&mut self, cx: &mut Context<Self>) {
            let Some((address, _)) = self.context_row(cx) else {
                return;
            };
            let before = self.results.len();
            self.results.retain(|r| r.address != address);
            if self.results.len() != before {
                self.status = format!("Deleted 0x{address:X}");
            }
            self.selected_row = None;
            self.context_target = None;
            self.refresh_table(cx);
            cx.notify();
        }

        /// "Add as Nodes" footer button — hand every displayed result address to
        /// the window so it appends each as a node in the active editor (the C++
        /// drag-into-editor / add-as-nodes path). Raised as a [`ScannerAddNodes`]
        /// event the window resolves against its editor controller.
        fn add_all_as_nodes(&mut self, cx: &mut Context<Self>) {
            let addresses: Vec<u64> = self
                .table
                .read(cx)
                .delegate()
                .rows
                .iter()
                .map(|r| r.row.address)
                .collect();
            if addresses.is_empty() {
                return;
            }
            let n = addresses.len();
            cx.emit(ScannerAddNodes { addresses });
            self.status = format!("Adding {n} addresses as nodes…");
            cx.notify();
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

            let result_count = self.results.len();
            self.table.update(cx, |state, cx| {
                let del = state.delegate_mut();
                del.rows = rows;
                del.show_previous = show_previous;
                del.show_module = show_module;
                del.result_count = result_count;
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

        /// Load a result document previously saved with [`results_json`]
        /// (the C++ `loadResultsFrom`): replace the results, restore the saved
        /// scan mode + value type so the Value column formats with the saved type
        /// (the C++ `m_lastScanMode = root["scanMode"]` / `m_lastValueType =
        /// root["valueType"]`, scannerpanel.cpp:2417-2418), then refresh.
        pub fn load_results(&mut self, doc: super::ScannerResultsDoc, cx: &mut Context<Self>) {
            let super::ScannerResultsDoc {
                results,
                scan_mode,
                value_type,
            } = doc;
            let n = results.len();
            self.results = results;
            // Restore the last-scan snapshot the Value column formats against.
            // The file stores no searched pattern, so the pattern is empty (a
            // signature-mode load then shows the full cached bytes, like the C++
            // which has no `m_lastPattern` persisted either).
            self.form
                .set_last_scan(scan_mode, value_type, ScanCondition::ExactValue, &[]);
            // Mirror the saved value type onto the form so a subsequent Next Scan
            // / inline edit decodes with the same width.
            if scan_mode == ScanMode::Value {
                self.form.value_type = value_type;
            }
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
            // The C++ `saveResultsTo` writes `m_lastScanMode` / `m_lastValueType`
            // (scannerpanel.cpp:2397-2398) — the mode + type of the *last* scan,
            // not the current form selection — so a loaded file restores the type
            // the saved bytes were captured with.
            serialize_results_json(self.form.last_mode(), self.form.last_value_type(), &rows)
        }

        /// "Save…" footer button — write the current results to a file the user
        /// picks (the C++ `saveResultsTo`). Serializes to the scanner JSON shape
        /// and writes it asynchronously after the native save dialog returns.
        fn save_results_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            if self.results.is_empty() {
                return;
            }
            let json = self.results_json();
            let dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
            let rx = cx.prompt_for_new_path(&dir, Some("scan_results.json"));
            cx.spawn_in(window, async move |this, cx| {
                let Some(path) = rx.await.ok().and_then(|r| r.ok()).flatten() else {
                    return;
                };
                let wrote = std::fs::write(&path, json).is_ok();
                let _ = this.update(cx, |this, cx| {
                    this.status = if wrote {
                        format!("Saved results to {}", path.display())
                    } else {
                        "Failed to write results file".to_string()
                    };
                    cx.notify();
                });
            })
            .detach();
        }

        /// "Load…" footer button — read a previously-saved results file the user
        /// picks (the C++ `loadResultsFrom`): parse the scanner JSON and replace
        /// the result list via [`load_results`](Self::load_results).
        fn load_results_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let rx = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Load scan results".into()),
            });
            cx.spawn_in(window, async move |this, cx| {
                let Some(path) = rx
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .flatten()
                    .and_then(|v| v.into_iter().next())
                else {
                    return;
                };
                let json = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(_) => {
                        let _ = this.update(cx, |this, cx| {
                            this.status = "Failed to read results file".to_string();
                            cx.notify();
                        });
                        return;
                    }
                };
                let doc = super::deserialize_results_json(&json);
                let _ = this.update(cx, |this, cx| {
                    this.load_results(doc, cx);
                });
            })
            .detach();
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
    impl EventEmitter<ScannerBatchEdit> for ScannerPanel {}
    impl EventEmitter<ScannerAddNodes> for ScannerPanel {}

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
            let reset_armed = self.reset_armed;
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
                // Key context so the panel-scoped `RcxScanner` KeyBindings
                // (Ctrl+Return / F5 / Ctrl+Z / Ctrl+L) dispatch here.
                .key_context("RcxScanner")
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                // Esc resolution (the C++ filter-Esc + panel-Esc shortcuts,
                // scannerpanel.cpp:845-863): a running scan cancels first; else a
                // non-empty result filter is cleared. Done in `on_key_down` (not a
                // bound action) so it never shadows the input's own Esc handling.
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if ev.keystroke.key != "escape" {
                        return;
                    }
                    if this.scanning {
                        this.cancel_scan(cx);
                    } else if !this.filter_text(cx).is_empty() {
                        // `set_value` suppresses the input's Change event, so the
                        // filter-recompute subscription won't fire — call
                        // `apply_filter` explicitly (the C++ `clear()` triggers
                        // `textChanged` which re-runs `applyFilter`).
                        this.filter_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.apply_filter(cx);
                    }
                }))
                // Result-row context-menu actions (the C++ row right-click menu).
                // The table delegate's `context_menu` dispatches these; they
                // bubble to these root handlers and resolve against the recorded
                // right-clicked row.
                .on_action(cx.listener(Self::ctx_copy_address))
                .on_action(cx.listener(Self::ctx_copy_value))
                .on_action(cx.listener(Self::ctx_set_base_address))
                .on_action(cx.listener(Self::ctx_change_all_values))
                // Panel-scoped keyboard shortcuts (scannerpanel.cpp:830-868).
                .on_action(cx.listener(Self::act_scan_or_rescan))
                .on_action(cx.listener(Self::act_rescan))
                .on_action(cx.listener(Self::act_undo))
                .on_action(cx.listener(Self::act_focus_filter))
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
                                                    .label(if reset_armed {
                                                        "Click again to reset"
                                                    } else {
                                                        "Reset"
                                                    })
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
                                    // Disabled when no view-root span is wired
                                    // (the C++ `setBoundsGetter` disables + unchecks
                                    // the chip when the bounds getter is null,
                                    // scannerpanel.cpp:1070-1073) — checking it
                                    // would otherwise silently fall back to a full
                                    // scan.
                                    Checkbox::new("scanner-struct")
                                        .label("Current Struct")
                                        .checked(self.form.struct_only)
                                        .disabled(self.struct_bounds.is_none())
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
                // ── Footer: Save/Load (left) + Go to Address + Copy Address (right) ──
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .px(px(tokens::space::LG))
                        .py(px(tokens::space::MD))
                        .gap(px(tokens::space::MD))
                        .justify_between()
                        .items_center()
                        .border_t_1()
                        .border_color(color::border(cx))
                        .bg(color::panel_bg(cx))
                        // Save / Load results to a file (the C++ saveResultsTo /
                        // loadResultsFrom). Save is enabled only with results.
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::SM))
                                .child(
                                    Button::new("scanner-save")
                                        .small()
                                        .label("Save…")
                                        .disabled(!has_results)
                                        .on_click(cx.listener(|this, _e, window, cx| {
                                            this.save_results_dialog(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("scanner-load").small().label("Load…").on_click(
                                        cx.listener(|this, _e, window, cx| {
                                            this.load_results_dialog(window, cx)
                                        }),
                                    ),
                                ),
                        )
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::MD))
                                .child(
                                    Button::new("scanner-goto")
                                        .small()
                                        .label("Go to Address")
                                        .disabled(!has_selection)
                                        .on_click(
                                            cx.listener(|this, _e, _w, cx| this.go_to_selected(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("scanner-copy")
                                        .small()
                                        .label("Copy Address")
                                        .disabled(!has_selection)
                                        .on_click(
                                            cx.listener(|this, _e, _w, cx| this.copy_selected(cx)),
                                        ),
                                )
                                // Batch actions over the result set (the C++
                                // multi-row Copy / Delete / add-as-nodes). The
                                // single-select DataTable limits Delete to the
                                // selected row; Copy All / Add as Nodes operate on
                                // every displayed row.
                                .child(
                                    Button::new("scanner-delete")
                                        .small()
                                        .label("Delete")
                                        .disabled(!has_selection)
                                        .on_click(
                                            cx.listener(|this, _e, _w, cx| {
                                                this.delete_selected(cx)
                                            }),
                                        ),
                                )
                                .child(
                                    Button::new("scanner-copy-all")
                                        .small()
                                        .label("Copy All")
                                        .disabled(!has_results)
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.copy_all_addresses(cx)
                                        })),
                                )
                                .child(
                                    Button::new("scanner-add-nodes")
                                        .small()
                                        .label("Add as Nodes")
                                        .disabled(!has_results)
                                        .on_click(cx.listener(|this, _e, _w, cx| {
                                            this.add_all_as_nodes(cx)
                                        })),
                                ),
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_delta, delete_rows, deserialize_results_json, filter_rows,
        format_addresses_for_clipboard, format_float, format_value, parse_change_all_bytes,
        previous_delta_text, rescan_status, serialize_results_json, shortcut_scan_target,
        split_address_dim, stage_breadcrumb, truncation_banner, value_type_entries, CondEntry,
        ScanMode, ScanRow, ScanShortcut, ScannerForm, FAST_SCAN_ALIGNMENTS, MAX_DISPLAY_ROWS,
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
        assert_eq!(parsed[0].scan_value, vec![0x39, 0x05, 0x00, 0x00]);
        assert_eq!(parsed[0].region_module, "");
        assert_eq!(parsed[1].address, 0x7ff0);
        assert_eq!(parsed[1].scan_value, vec![0xFF]);
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
