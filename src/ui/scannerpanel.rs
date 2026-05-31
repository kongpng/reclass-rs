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

/// One result row's display strings — the Address column (with backtick) and the
/// formatted Value column. Built from a [`ScanResult`] + the form's last-scan
/// snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRow {
    /// Absolute address (the row's stable key + goto target).
    pub address: u64,
    /// The formatted address string (`format_scan_address`).
    pub address_text: String,
    /// The formatted value string (`ScannerForm::format_value`).
    pub value_text: String,
}

impl ScanRow {
    /// Build a row from a result + the form (for the value formatting).
    pub fn from_result(form: &ScannerForm, r: &ScanResult) -> ScanRow {
        ScanRow {
            address: r.address,
            address_text: format_scan_address(r.address),
            value_text: form.format_value(&r.scan_value),
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

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{ScannerNav, ScannerPanel};

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
        filter_rows, split_address_dim, value_type_entries, CondEntry, ScanRow, ScannerForm,
        FAST_SCAN_ALIGNMENTS,
    };
    use crate::provider::Provider;
    use crate::scanner::{run_scan, NullObserver, ScanResult, ValueType};
    use crate::ui::design::{color, tokens};

    /// A "go to this address" request raised when a result row is activated
    /// (double-click / Enter) — the C++ `goToAddress(uint64_t)` signal. The window
    /// resolves it onto the active document (set view root / scroll to address).
    #[derive(Clone, Copy, Debug)]
    pub struct ScannerNav {
        pub address: u64,
    }

    /// The two result columns (`scannerpanel.cpp:633` — `setColumnCount(2)`).
    const COL_ADDRESS: usize = 0;
    const COL_VALUE: usize = 1;

    /// The [`TableDelegate`] backing the results [`DataTable`]: owns the displayed
    /// rows and paints the Address cell with the dimmed leading-zero prefix (the
    /// C++ `AddressDelegate`). Sorting is wired via [`TableDelegate::perform_sort`]
    /// over the address (the C++ sortable header).
    struct ScanResultsDelegate {
        rows: Vec<ScanRow>,
    }

    impl ScanResultsDelegate {
        fn new() -> Self {
            ScanResultsDelegate { rows: Vec::new() }
        }
    }

    impl TableDelegate for ScanResultsDelegate {
        fn columns_count(&self, _cx: &App) -> usize {
            2
        }

        fn rows_count(&self, _cx: &App) -> usize {
            self.rows.len()
        }

        fn column(&self, col_ix: usize, _cx: &App) -> Column {
            match col_ix {
                COL_VALUE => Column::new("value", "Value").width(px(220.)).sortable(),
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
            match col_ix {
                COL_ADDRESS => self.rows.sort_by_key(|r| r.address),
                _ => self.rows.sort_by(|a, b| a.value_text.cmp(&b.value_text)),
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
            let Some(row) = self.rows.get(row_ix) else {
                return div();
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
            } else {
                // Numeric / hex value column: monospace, right-aligned so digits
                // line up the way the original scanner table presents them.
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .font_family(tokens::font::MONO_FAMILY)
                    .text_size(px(tokens::font::EDITOR_SIZE))
                    .text_color(color::text(cx))
                    .child(row.value_text.clone())
            }
        }

        fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
            let Some(row) = self.rows.get(row_ix) else {
                return String::new();
            };
            if col_ix == COL_ADDRESS {
                row.address_text.clone()
            } else {
                row.value_text.clone()
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
        /// The full (unfiltered) result rows; the table holds the filtered view.
        all_rows: Vec<ScanRow>,
        provider: Option<Arc<dyn Provider + Send + Sync>>,
        status: String,
        /// The currently-selected result row (drives the footer goto/copy
        /// buttons), tracked from [`TableEvent::SelectRow`].
        selected_row: Option<usize>,
        /// Controlled-open state for the toolbar dropdown popovers (condition /
        /// value-type / fast-scan), so picking an item dismisses the popover.
        cond_open: bool,
        type_open: bool,
        align_open: bool,
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

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
                            .map(|r| r.address);
                        if let Some(address) = addr {
                            cx.emit(ScannerNav { address });
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
                all_rows: Vec::new(),
                provider: None,
                status: String::new(),
                selected_row: None,
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
        /// result). Scans run against this source.
        pub fn set_provider(&mut self, provider: Option<Arc<dyn Provider + Send + Sync>>) {
            self.provider = provider;
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

        /// Run the scan over the attached provider (the C++ `onScanClicked` →
        /// `runScan`, run synchronously here). Builds the request, runs the
        /// engine, formats the rows, and refreshes the table + status line.
        fn run_scan(&mut self, cx: &mut Context<Self>) {
            let Some(provider) = self.provider.clone() else {
                self.status = "No source attached".to_string();
                cx.notify();
                return;
            };
            let ptr_size = provider.pointer_size();
            match self.form.build_request(ptr_size, None) {
                Err(msg) => {
                    self.status = msg;
                }
                Ok(req) => {
                    let abort = AtomicBool::new(false);
                    let obs = NullObserver;
                    let results: Vec<ScanResult> = run_scan(provider.as_ref(), &req, &abort, &obs);
                    self.all_rows = results
                        .iter()
                        .map(|r| ScanRow::from_result(&self.form, r))
                        .collect();
                    self.selected_row = None;
                    let n = self.all_rows.len();
                    self.status = if n == 1 {
                        "1 result".to_string()
                    } else {
                        format!("{n} results")
                    };
                    self.refresh_table(cx);
                }
            }
            cx.notify();
        }

        /// Clear the result list (the C++ `onNewScanClicked` / Reset).
        fn reset(&mut self, cx: &mut Context<Self>) {
            self.all_rows.clear();
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
                .map(|r| r.address)
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

        /// Push the (filtered) rows into the table delegate.
        fn refresh_table(&mut self, cx: &mut Context<Self>) {
            let query = self.filter_text(cx);
            let rows: Vec<ScanRow> = filter_rows(&self.all_rows, &query)
                .into_iter()
                .cloned()
                .collect();
            self.table.update(cx, |state, cx| {
                state.delegate_mut().rows = rows;
                cx.notify();
            });
        }

        /// The currently-selected condition entry's display label.
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
            SharedString::from("Memory Scanner")
        }
    }

    impl EventEmitter<PanelEvent> for ScannerPanel {}
    impl EventEmitter<ScannerNav> for ScannerPanel {}

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
    fn dropdown_trigger(id: impl Into<SharedString>, text: impl Into<SharedString>) -> Button {
        let id: SharedString = id.into();
        let text: SharedString = text.into();
        Button::new(SharedString::from(format!("scanner-trig-{id}")))
            .outline()
            .small()
            .label(format!("{text}  ▾"))
    }

    impl Render for ScannerPanel {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let vis = self.form.field_visibility();
            let has_results = !self.all_rows.is_empty();
            let has_selection = self.selected_row.is_some();
            let cur_cond = self.form.condition;
            let cur_type = self.form.value_type;
            let cur_align = self.form.alignment.max(1);

            // A weak handle to self so the popover content (which renders in the
            // PopoverState context) can drive the panel reducer on a pick.
            let panel = cx.entity().downgrade();

            // ── Condition dropdown (the C++ scan-type / condition combo) ──
            let cond_popover = Popover::new("scanner-cond-pop")
                .anchor(Anchor::TopLeft)
                .open(self.cond_open)
                .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                    this.cond_open = *open;
                    cx.notify();
                }))
                .trigger(dropdown_trigger("cond", self.cond_label()))
                .content({
                    let panel = panel.clone();
                    move |_state, _window, cx| {
                        let mut menu = dropdown_menu(cx);
                        for (i, (entry, name)) in CondEntry::entries().iter().enumerate() {
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

            // ── Status line: muted "N results" / "Copied ..." ──
            let status_text = if self.status.is_empty() {
                if has_results {
                    String::new()
                } else {
                    "No results".to_string()
                }
            } else {
                self.status.clone()
            };

            gpui_component::v_flex()
                .id("rcx-scanner-panel")
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(color::panel_bg(cx))
                .text_color(color::text(cx))
                // ── Panel header (uppercase muted title strip) ──
                .child(crate::ui::design::panel_header("Scanner", cx))
                .child(
                    // ── Search controls cluster ──
                    gpui_component::v_flex()
                        .gap(px(tokens::space::MD))
                        .p(px(tokens::space::LG))
                        // Row 1: scan-type / value-type / alignment dropdowns.
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::MD))
                                .flex_wrap()
                                .items_center()
                                .child(cond_popover)
                                .when(vis.type_enabled, |row| row.child(type_popover))
                                .when(vis.type_enabled, |row| row.child(align_popover)),
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
                                ),
                        )
                        // Row 4: primary Scan + secondary Re-scan + Reset.
                        .child(
                            gpui_component::h_flex()
                                .gap(px(tokens::space::MD))
                                .items_center()
                                .child(
                                    Button::new("scanner-scan")
                                        .primary()
                                        .small()
                                        .label("⌕  Scan")
                                        .on_click(
                                            cx.listener(|this, _e, _w, cx| this.run_scan(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("scanner-rescan")
                                        .small()
                                        .label("↻  Re-scan")
                                        .disabled(!has_results)
                                        .on_click(
                                            cx.listener(|this, _e, _w, cx| this.run_scan(cx)),
                                        ),
                                )
                                .child(
                                    Button::new("scanner-reset")
                                        .ghost()
                                        .small()
                                        .label("Reset")
                                        .on_click(cx.listener(|this, _e, _w, cx| this.reset(cx))),
                                ),
                        ),
                )
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
                                .label("→  Go to Address")
                                .disabled(!has_selection)
                                .on_click(cx.listener(|this, _e, _w, cx| this.go_to_selected(cx))),
                        )
                        .child(
                            Button::new("scanner-copy")
                                .small()
                                .label("⧉  Copy Address")
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
        filter_rows, format_value, split_address_dim, value_type_entries, CondEntry, ScanMode,
        ScanRow, ScannerForm, FAST_SCAN_ALIGNMENTS,
    };
    use crate::scanner::{ScanCondition, ScanResult, ValueType};

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
            },
            ScanRow {
                address: 0x2000,
                address_text: "00000000`00002000".to_string(),
                value_text: "1337".to_string(),
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
