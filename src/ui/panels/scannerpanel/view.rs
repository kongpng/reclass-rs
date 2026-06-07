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
use gpui_component::table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState};
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
    /// The physical column index of the Module column — slides from 3 to 2
    /// when the Previous column is hidden.
    fn module_col_ix(&self) -> usize {
        if self.show_previous {
            3
        } else {
            2
        }
    }

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
        let module_ix = self.module_col_ix();
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
        let module_ix = self.module_col_ix();
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
        let module_ix = self.module_col_ix();
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
        let module_ix = self.module_col_ix();
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
        let value_input = cx.new(|cx| InputState::new(window, cx).placeholder("value / pattern"));
        let value2_input = cx.new(|cx| InputState::new(window, cx).placeholder("upper bound"));
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter results..."));
        let table = cx
            .new(|cx| TableState::new(ScanResultsDelegate::new(), window, cx).row_selectable(true));

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
    pub fn save_settings<S: crate::theme::manager::SettingsStore + ?Sized>(&self, store: &mut S) {
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
        let needs_typed = super::consumes_typed_value(cond);
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
        apply_scan_overrides(&mut results, override_exact.then(|| form.last_pattern()));
        self.scanning = false;
        self.results = results;
        self.show_previous = false;
        self.selected_row = None;
        self.form = form.clone();
        let n = self.results.len();
        // C++ onScanFinished (scannerpanel.cpp:1473-1479): a 0-result first
        // scan shows the "try widening the filters" guidance, not a bare
        // "0 results". Non-empty scans keep the plain result count.
        self.status = super::first_scan_status(n);
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
        apply_scan_overrides(&mut results, override_exact.then(|| req.pattern.as_slice()));

        self.results = results.clone();
        self.show_previous = false;
        self.undo_stack.clear();
        self.generation = 1;
        self.last_result_count = 0;
        self.selected_row = None;
        self.scanning = false;
        let n = results.len();
        self.status = super::count_status("", n);
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
        // C++ popUndoSnapshot always calls populateTable(false) — Undo Scan
        // unconditionally collapses back to Address/Value(+Module), hiding the
        // Previous→Δ column (scannerpanel.cpp:2243).
        self.show_previous = false;
        let n = self.results.len();
        self.status = super::count_status("Restored — ", n);
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
                        this.status = super::count_status("", n);
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
            self.status = format!("Copied: 0x{address:X}");
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
    fn act_scan_or_rescan(&mut self, _: &ScScanOrRescan, _w: &mut Window, cx: &mut Context<Self>) {
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
    fn act_focus_filter(&mut self, _: &ScFocusFilter, window: &mut Window, cx: &mut Context<Self>) {
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
                    // C++ Change All reassigns only scanValue (scannerpanel.cpp:984),
                    // keeping previousValue as the last-rescan snapshot — do not
                    // overwrite it here (would render a spurious Previous→Δ delta).
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
    /// The addresses of every currently-displayed result row (delegate order).
    fn displayed_addresses(&self, cx: &App) -> Vec<u64> {
        self.table
            .read(cx)
            .delegate()
            .rows
            .iter()
            .map(|r| r.row.address)
            .collect()
    }

    fn copy_all_addresses(&mut self, cx: &mut Context<Self>) {
        let addrs = self.displayed_addresses(cx);
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
        let addresses = self.displayed_addresses(cx);
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
    pub fn apply_value_write(&mut self, row: usize, new_value: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(r) = self.results.get_mut(row) {
            // C++ onCellEdited reassigns only scanValue (scannerpanel.cpp:1888),
            // leaving previousValue as the last-rescan snapshot — do not overwrite
            // it (would render a spurious Previous→Δ delta on the edited row).
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
        self.status = super::count_status("Loaded ", n);
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

/// Apply the post-scan value overrides to `results`: always clear the previous
/// snapshot, and — on an exact-value scan (`pattern` Some) — overwrite each
/// scan_value with the searched bytes (the C++ onScanFinished loop).
fn apply_scan_overrides(results: &mut [ScanResult], pattern: Option<&[u8]>) {
    for r in results {
        r.previous_value.clear();
        if let Some(pat) = pattern {
            r.scan_value = pat.to_vec();
        }
    }
}

/// The shared `.content` builder for the scanner toolbar dropdowns — a
/// `dropdown_menu` of `dropdown_row`s (the current pick highlighted) whose clicks
/// run `pick` on the panel. Factors the mode/cond/type/align popover menus.
#[allow(clippy::type_complexity)]
fn dropdown_content<T: Copy + PartialEq + 'static>(
    row_prefix: &'static str,
    items: Vec<(SharedString, T)>,
    current: T,
    panel: WeakEntity<ScannerPanel>,
    pick: impl Fn(&mut ScannerPanel, T, &mut Context<ScannerPanel>) + Copy + 'static,
) -> impl Fn(
    &mut gpui_component::popover::PopoverState,
    &mut Window,
    &mut Context<gpui_component::popover::PopoverState>,
) -> Div
       + 'static {
    move |_state, _window, cx| {
        let mut menu = dropdown_menu(cx);
        for (i, (label, item)) in items.iter().enumerate() {
            let item = *item;
            let label = label.clone();
            let panel = panel.clone();
            menu = menu.child(
                dropdown_row((row_prefix, i), label, item == current, cx).on_click(
                    move |_e, _w, cx| {
                        panel.update(cx, |this, cx| pick(this, item, cx)).ok();
                    },
                ),
            );
        }
        menu
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
        // C++ syncScanEnabled greys out the Scan button when a value-requiring
        // condition has an empty value field (scannerpanel.cpp:732-756). Mirror it so
        // First Scan is disabled (not just an error-on-click) when input is missing.
        let missing_input = vis.value_enabled && self.form.value_text.trim().is_empty();
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
            .content(dropdown_content(
                "mode-row",
                vec![
                    ("Value".into(), ScanMode::Value),
                    ("Signature".into(), ScanMode::Signature),
                ],
                cur_mode,
                panel.clone(),
                |this, m, cx| {
                    this.set_scan_mode(m, cx);
                },
            ));

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
            .content(dropdown_content(
                "cond-row",
                CondEntry::entries()
                    .iter()
                    .filter(|(c, _)| !matches!(c, CondEntry::Signature))
                    .map(|(e, n)| (SharedString::from(*n), *e))
                    .collect(),
                cur_cond,
                panel.clone(),
                |this, e, cx| {
                    this.set_condition(e, cx);
                },
            ));

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
            .content(dropdown_content(
                "type-row",
                value_type_entries()
                    .iter()
                    .map(|(vt, n)| (SharedString::from(*n), *vt))
                    .collect(),
                cur_type,
                panel.clone(),
                |this, vt, cx| {
                    this.set_value_type(vt, cx);
                },
            ));

        // ── Fast-Scan (alignment) dropdown (value mode only) ──
        let align_popover = Popover::new("scanner-align-pop")
            .anchor(Anchor::TopLeft)
            .open(self.align_open)
            .on_open_change(cx.listener(|this, open: &bool, _w, cx| {
                this.align_open = *open;
                cx.notify();
            }))
            .trigger(dropdown_trigger("align", format!("Align: {cur_align}")))
            .content(dropdown_content(
                "align-row",
                FAST_SCAN_ALIGNMENTS
                    .iter()
                    .map(|a| (SharedString::from(a.to_string()), *a))
                    .collect(),
                cur_align,
                panel.clone(),
                |this, a, cx| {
                    this.set_alignment(a, cx);
                },
            ));

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
            super::count_status("", n)
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
                                                .disabled(missing_input)
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
                                                .on_click(
                                                    cx.listener(|this, _e, _w, cx| this.reset(cx)),
                                                ),
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
                            .child(Button::new("scanner-load").small().label("Load…").on_click(
                                cx.listener(|this, _e, window, cx| {
                                    this.load_results_dialog(window, cx)
                                }),
                            )),
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
                                        cx.listener(|this, _e, _w, cx| this.delete_selected(cx)),
                                    ),
                            )
                            .child(
                                Button::new("scanner-copy-all")
                                    .small()
                                    .label("Copy All")
                                    .disabled(!has_results)
                                    .on_click(
                                        cx.listener(|this, _e, _w, cx| this.copy_all_addresses(cx)),
                                    ),
                            )
                            .child(
                                Button::new("scanner-add-nodes")
                                    .small()
                                    .label("Add as Nodes")
                                    .disabled(!has_results)
                                    .on_click(
                                        cx.listener(|this, _e, _w, cx| this.add_all_as_nodes(cx)),
                                    ),
                            ),
                    ),
            )
    }
}
