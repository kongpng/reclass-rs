//! Process picker — choose a data source to attach to, as a virtualized table.
//!
//! Port of `src/processpicker.{h,cpp,ui}` (widgets-dialogs.md §6). In the original
//! this was a modal that enumerated the running OS processes (Toolhelp on Windows,
//! `/proc` on Linux) into a 3-column `QTableWidget` (PID, Process Name, Path),
//! filterable by name-or-PID, default-sorted highest-PID-first, with double-click
//! / Attach to accept.
//!
//! This picker now enumerates live OS processes through the selected provider's
//! `enumerateProcesses()` equivalent and returns the provider id plus PID/name.
//!
//! Split (gpui-free model + a thin view):
//! - [`SourceAvailability`] — whether a row is attachable or a labeled disabled
//!   provider row.
//! - [`ProcessRow`] — one table row (PID, name, path/detail, availability, 32-bit
//!   flag), with the C++ `(32-bit)` display-name suffix.
//! - [`ProcessPickerModel`] — builds the row list from a [`ProviderRegistry`] or
//!   provider process list, the name-or-PID filter (`filterProcesses`), and the
//!   default highest-PID-first sort. Pure + unit-tested headlessly.
//! - [`ProcessPicker`] / [`ProcessPickEvent`] — the gpui view: a filter input
//!   above a [`DataTable`](gpui_component::table::DataTable), raising
//!   `Attach(row)` / `Cancel`.
//!
//! Gated behind the `ui` feature.

use crate::plugin::contract::ProcessInfo;
use crate::provider::ProviderRegistry;

/// Whether a picker row is an attachable source or a labeled disabled row.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceAvailability {
    /// An implemented source row — the row can be selected/attached.
    Available,
    /// A source row shown for context but not attachable from this picker.
    Stub,
}

impl SourceAvailability {
    /// The trailing tag shown after the name for stub rows (empty for available).
    pub fn tag(self) -> &'static str {
        match self {
            SourceAvailability::Available => "",
            SourceAvailability::Stub => " (stub)",
        }
    }
}

/// One process-picker row — the C++ `ProcessInfo` generalized to a *source* row.
///
/// For registry rows the `pid` is a synthetic 0 (they are not processes) and the
/// `path`/detail describes the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRow {
    /// PID column (`Qt::EditRole` int). 0 for non-process sources.
    pub pid: u32,
    /// The source/process name (without the architecture suffix).
    pub name: String,
    /// The Path column (full image path / source detail).
    pub path: String,
    /// 32-bit (WoW64) flag — appends " (32-bit)" to the display name.
    pub is_32bit: bool,
    /// Whether this row is attachable or a labeled stub.
    pub availability: SourceAvailability,
    /// The registry identifier (for available built-ins) or stub key.
    pub identifier: String,
}

impl ProcessRow {
    /// The Name column display string (`populateTable`): name + `" (32-bit)"`
    /// when 32-bit, then the availability tag for stub rows.
    pub fn display_name(&self) -> String {
        let mut s = self.name.clone();
        if self.is_32bit {
            s.push_str(" (32-bit)");
        }
        s.push_str(self.availability.tag());
        s
    }

    /// The PID column text — empty for synthetic (non-process) rows, else the
    /// decimal PID.
    pub fn pid_text(&self) -> String {
        if self.pid == 0 {
            String::new()
        } else {
            self.pid.to_string()
        }
    }

    /// Whether this row can be attached (the Attach button / double-click guard).
    pub fn is_attachable(&self) -> bool {
        self.availability == SourceAvailability::Available
    }
}

/// The process-picker model — the row list + the filter, built from the provider
/// registry's available sources or a provider's process enumeration.
#[derive(Clone, Debug, Default)]
pub struct ProcessPickerModel {
    rows: Vec<ProcessRow>,
}

impl ProcessPickerModel {
    /// Build a fallback model from the registry's registered providers. The real
    /// process attach path uses [`from_processes`](Self::from_processes).
    pub fn from_registry(registry: &ProviderRegistry) -> ProcessPickerModel {
        let mut rows: Vec<ProcessRow> = registry
            .enabled_providers()
            .map(|p| ProcessRow {
                pid: 0,
                name: p.name.clone(),
                path: format!("Built-in source: {}", p.identifier),
                is_32bit: false,
                availability: SourceAvailability::Available,
                identifier: p.identifier.clone(),
            })
            .collect();
        Self::sort_default(&mut rows);
        ProcessPickerModel { rows }
    }

    /// Build a real C++-style process list for one provider id.
    pub fn from_processes(processes: Vec<ProcessInfo>, identifier: &str) -> ProcessPickerModel {
        let mut rows: Vec<ProcessRow> = processes
            .into_iter()
            .map(|p| ProcessRow {
                pid: p.pid,
                name: p.name,
                path: p.path,
                is_32bit: p.is_32bit,
                availability: SourceAvailability::Available,
                identifier: identifier.to_string(),
            })
            .collect();
        Self::sort_default(&mut rows);
        ProcessPickerModel { rows }
    }

    /// Build a model from an explicit row list (the C++ custom-list constructor
    /// `ProcessPicker(customProcesses)`, e.g. for MCP/automation). Sorted default.
    pub fn from_rows(mut rows: Vec<ProcessRow>) -> ProcessPickerModel {
        Self::sort_default(&mut rows);
        ProcessPickerModel { rows }
    }

    /// `sortItems(0, DescendingOrder)` (`populateTable`): highest PID first.
    /// Ties broken attachable-before-stub then by name for a stable display.
    fn sort_default(rows: &mut [ProcessRow]) {
        rows.sort_by(|a, b| {
            b.pid
                .cmp(&a.pid)
                .then_with(|| a.availability_rank().cmp(&b.availability_rank()))
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    /// All rows (unfiltered), in display order.
    pub fn rows(&self) -> &[ProcessRow] {
        &self.rows
    }

    /// `filterProcesses(text)` / `applyFilter` (`processpicker.cpp:362-384`): keep
    /// rows whose **name** OR **PID** OR **path** contains the (case-insensitive)
    /// query. An empty query keeps everything. Returns borrowed rows in display
    /// order. The path match mirrors the C++ `proc.path.toLower().contains(...)`
    /// (cpp:378), which was previously omitted here.
    pub fn filtered(&self, query: &str) -> Vec<&ProcessRow> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.rows.iter().collect();
        }
        self.rows
            .iter()
            .filter(|r| {
                r.name.to_lowercase().contains(&q)
                    || r.pid_text().contains(&q)
                    || r.path.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// The first attachable row (the C++ `selectPreferredProcess` default
    /// selection target), if any.
    pub fn preferred(&self) -> Option<&ProcessRow> {
        self.rows.iter().find(|r| r.is_attachable())
    }
}

/// The index of the row that should be pre-selected within a displayed
/// (already-filtered, in display order) row list — the first attachable row, or
/// the first row when none is attachable, or `None` for an empty list. Mirrors
/// the C++ `selectPreferredProcess` (which falls back to the table's default top
/// selection so Attach/Enter always act on *some* row).
pub fn preferred_row_index(rows: &[ProcessRow]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    Some(rows.iter().position(ProcessRow::is_attachable).unwrap_or(0))
}

/// The pre-selected row index honoring the remembered last-attached process name,
/// the full `selectPreferredProcess` (`processpicker.cpp:386-403`): if
/// `last_attached` is `Some(non-empty)`, return the index of the FIRST row whose
/// **original** `name` (not the `(32-bit)`-suffixed display name) matches it
/// case-insensitively — the C++ compares against the `Qt::UserRole` original name
/// with `compare(..., Qt::CaseInsensitive) == 0`. When there is no remembered name
/// (or it does not match any row), fall back to [`preferred_row_index`] (first
/// attachable row, else the first row), so Attach/Enter always act on some row.
pub fn preferred_row_index_for(rows: &[ProcessRow], last_attached: Option<&str>) -> Option<usize> {
    if let Some(last) = last_attached.filter(|s| !s.is_empty()) {
        if let Some(ix) = rows.iter().position(|r| r.name.eq_ignore_ascii_case(last)) {
            return Some(ix);
        }
    }
    preferred_row_index(rows)
}

impl ProcessRow {
    /// Sort rank: attachable rows (0) before stub rows (1).
    fn availability_rank(&self) -> u8 {
        match self.availability {
            SourceAvailability::Available => 0,
            SourceAvailability::Stub => 1,
        }
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub(crate) use view::ProcessDelegate as ProcessTableDelegate;
#[cfg(feature = "ui")]
pub use view::{ProcessPickEvent, ProcessPicker};

#[cfg(feature = "ui")]
mod view {
    use crate::ui::design::{color, tokens};
    use crate::ui::dialogs::modal;
    use crate::ui::overlays::contextmenu::process_row_menu;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::table::{
        Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
    };
    use gpui_component::tooltip::Tooltip;
    use gpui_component::Sizable as _;

    use super::{ProcessPickerModel, ProcessRow, SourceAvailability};

    /// An open row context menu: the right-clicked row index + the window-space
    /// anchor point (the C++ `customContextMenuRequested` position).
    #[derive(Clone, Copy)]
    struct RowMenu {
        row_ix: usize,
        pos: Point<Pixels>,
    }

    /// The picker's outcome (the C++ `accept`/`reject`).
    #[derive(Clone, Debug)]
    pub enum ProcessPickEvent {
        /// Attach to the chosen source (carries the row's identifier + pid + name).
        Attach {
            identifier: String,
            pid: u32,
            name: String,
        },
        /// Cancel / Esc.
        Cancel,
    }

    const COL_PID: usize = 0;
    const COL_NAME: usize = 1;
    const COL_PATH: usize = 2;

    /// The [`TableDelegate`] backing the picker table — PID / Process Name / Path
    /// (the C++ `.ui` column set). Stub rows are dimmed (non-attachable).
    pub(crate) struct ProcessDelegate {
        pub(crate) rows: Vec<ProcessRow>,
        /// The Path column width, sized at construction to fill the table box.
        /// gpui-component's table has no "stretch last section", so we compute the
        /// fill width ourselves from the live card width (the C++
        /// `setStretchLastSection(true)`, processpicker.cpp:58 — Path absorbs the
        /// remaining width after the fixed PID/Name columns). Deriving it from
        /// `clamp_width` keeps Path filled even when the dialog shrinks to a small
        /// window, instead of a hardcoded width that left an empty strip / overflow.
        path_width: Pixels,
    }

    impl ProcessDelegate {
        pub(crate) fn new(path_width: Pixels) -> Self {
            ProcessDelegate {
                rows: Vec::new(),
                path_width,
            }
        }

        pub(crate) fn set_rows(&mut self, rows: Vec<ProcessRow>) {
            self.rows = rows;
        }

        pub(crate) fn rows(&self) -> &[ProcessRow] {
            &self.rows
        }
    }

    impl TableDelegate for ProcessDelegate {
        fn columns_count(&self, _cx: &App) -> usize {
            3
        }

        fn rows_count(&self, _cx: &App) -> usize {
            self.rows.len()
        }

        fn column(&self, col_ix: usize, _cx: &App) -> Column {
            match col_ix {
                COL_PID => Column::new("pid", "PID").width(px(72.)).sortable(),
                COL_NAME => Column::new("name", "Process Name")
                    .width(px(220.))
                    .sortable(),
                // The Path column absorbs the remaining card width so the columns
                // fill the table box exactly (no empty strip / overflow on the
                // right). Width is computed from the live card width at
                // construction (see `path_width`) rather than hardcoded.
                COL_PATH => Column::new("path", "Path")
                    .width(self.path_width)
                    .sortable(),
                _ => Column::new("path", "Path")
                    .width(self.path_width)
                    .sortable(),
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
                COL_PID => self.rows.sort_by_key(|r| r.pid),
                COL_NAME => self.rows.sort_by(|a, b| a.name.cmp(&b.name)),
                _ => self.rows.sort_by(|a, b| a.path.cmp(&b.path)),
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
                return div().into_any_element();
            };
            // Stub rows render dimmed to signal they are not attachable.
            let fg = if row.availability == SourceAvailability::Stub {
                color::text_muted(cx)
            } else {
                color::text(cx)
            };
            let text = match col_ix {
                COL_PID => row.pid_text(),
                COL_NAME => row.display_name(),
                _ => row.path.clone(),
            };
            // PID + Path columns read as monospace addresses/paths; the name is UI.
            let mono = matches!(col_ix, COL_PID | COL_PATH);
            let cell = div()
                .text_color(fg)
                .text_size(px(tokens::font::UI_SM))
                .when(mono, |d| d.font_family(tokens::font::mono_family()));
            if col_ix == COL_PATH && !text.is_empty() {
                // The C++ Path column elides (Qt::ElideLeft, cpp:61) and carries a
                // tooltip with the full path (`pathItem->setToolTip(proc.path)`,
                // cpp:349). gpui truncates trailing rather than leading — an
                // accepted Zed substitution for ElideLeft — and the full path is
                // surfaced on hover via the managed Tooltip so nothing is lost.
                let full = SharedString::from(text.clone());
                cell.id(("process-path", row_ix))
                    .w_full()
                    .truncate()
                    .child(text)
                    .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                    .into_any_element()
            } else {
                cell.child(text).into_any_element()
            }
        }

        fn cell_text(&self, row_ix: usize, col_ix: usize, _cx: &App) -> String {
            let Some(row) = self.rows.get(row_ix) else {
                return String::new();
            };
            match col_ix {
                COL_PID => row.pid_text(),
                COL_NAME => row.display_name(),
                _ => row.path.clone(),
            }
        }
    }

    /// The process-picker view — a filter input above the source [`DataTable`].
    ///
    /// Owns the [`ProcessPickerModel`], the filter [`InputState`], and the
    /// [`TableState`]. Double-clicking an attachable row (or Attach) raises
    /// [`ProcessPickEvent::Attach`]; stub rows are inert.
    pub struct ProcessPicker {
        model: ProcessPickerModel,
        filter: Entity<InputState>,
        table: Entity<TableState<ProcessDelegate>>,
        /// The remembered last-attached process name (the C++ `lastAttachedProcess`
        /// QSettings key, read in `selectPreferredProcess`). `None`/empty → fall
        /// back to the first-attachable preference. Used by [`select_preferred`].
        last_attached: Option<String>,
        /// The open right-click menu (row + anchor), if any.
        row_menu: Option<RowMenu>,
        /// The last cursor position seen on a right mouse-down over the table, used
        /// to anchor the row menu (`TableEvent::RightClickedRow` carries only the
        /// row index).
        last_right_click: Point<Pixels>,
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

    impl ProcessPicker {
        /// Build the picker over the given model, pre-selecting the row whose name
        /// matches `last_attached` (the C++ `lastAttachedProcess`) when set; pass
        /// `None` for the plain first-attachable preference.
        pub fn new(
            model: ProcessPickerModel,
            last_attached: Option<String>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let filter =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter by name or PID..."));
            // Size the Path column to fill the table box after the fixed PID (72)
            // and Name (220) columns — the C++ stretch-last-section. The box's inner
            // width is the card minus 34px of chrome (card border 2 + modal body
            // padding 16×2); Path takes the rest: card_w − 72 − 220 − 34 = card_w −
            // 326 (= 394 at the 720 card, the empirically-correct fill where the
            // Path sort-arrow sits exactly at the box's right edge). Derived from the
            // live `clamp_width` so a small-window (clamped) dialog stays filled
            // rather than leaving a strip; floored so a tiny window degrades to
            // horizontal scroll instead of a negative width.
            let path_w = px((f32::from(modal::clamp_width(720., window)) - 326.).max(160.));
            let table = cx.new(|cx| {
                TableState::new(ProcessDelegate::new(path_w), window, cx).row_selectable(true)
            });

            let mut subs = Vec::new();
            subs.push(cx.subscribe(&filter, |this, _input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.refresh_table(cx);
                }
            }));
            subs.push(
                cx.subscribe(&table, |this, table, ev: &TableEvent, cx| match ev {
                    // Double-click attaches (the C++ `cellDoubleClicked` → `accept`).
                    TableEvent::DoubleClickedRow(row_ix) => {
                        let row = table.read(cx).delegate().rows.get(*row_ix).cloned();
                        if let Some(row) = row {
                            this.attach_row(&row, cx);
                        }
                    }
                    // Right-click opens the row copy menu (the C++
                    // `customContextMenuRequested`). `None` (empty area) closes it.
                    TableEvent::RightClickedRow(row_ix) => {
                        this.row_menu = row_ix.map(|row_ix| RowMenu {
                            row_ix,
                            pos: this.last_right_click,
                        });
                        cx.notify();
                    }
                    _ => {}
                }),
            );

            let mut this = ProcessPicker {
                model,
                filter,
                table,
                last_attached: last_attached.filter(|s| !s.is_empty()),
                row_menu: None,
                last_right_click: Point::default(),
                focus_handle: cx.focus_handle(),
                _subs: subs,
            };
            // refresh_table also pre-selects the preferred (first attachable) row
            // and scrolls it into view, so Attach / Enter work immediately (the
            // C++ `selectPreferredProcess` + `scrollToItem`).
            this.refresh_table(cx);
            // Focus the filter input so typing filters immediately on open (the C++
            // `ui->filterEdit->setFocus()` in the constructor). The host opens this
            // picker without a `window.focus(...)` call, so the focus must be set
            // here; without it the picker card holds no input focus and typing into
            // the filter does nothing until it is clicked (the dead-input failure
            // mode the cross-cutting note flags). The `RcxProcessPicker`
            // `key_context` + capture-phase Enter/Escape handler still fire because
            // the focused filter input is a descendant of the `track_focus` card.
            this.filter.update(cx, |input, cx| input.focus(window, cx));
            this
        }

        /// Construct over a model as an [`Entity`], honoring the remembered
        /// last-attached process name (the C++ `lastAttachedProcess`); pass `None`
        /// for the plain first-attachable preference.
        pub fn view(
            model: ProcessPickerModel,
            last_attached: Option<String>,
            window: &mut Window,
            cx: &mut App,
        ) -> Entity<Self> {
            cx.new(|cx| ProcessPicker::new(model, last_attached, window, cx))
        }

        /// The backing model (for tests / external wiring).
        pub fn model(&self) -> &ProcessPickerModel {
            &self.model
        }

        /// The current filter text.
        fn filter_text(&self, cx: &App) -> String {
            self.filter.read(cx).value().to_string()
        }

        /// Push the filtered rows into the table delegate, then re-select the
        /// preferred row (the row set changed, so any prior selection is stale).
        fn refresh_table(&mut self, cx: &mut Context<Self>) {
            let query = self.filter_text(cx);
            let rows: Vec<ProcessRow> = self.model.filtered(&query).into_iter().cloned().collect();
            self.table.update(cx, |state, cx| {
                state.delegate_mut().rows = rows;
                cx.notify();
            });
            self.row_menu = None;
            self.select_preferred(cx);
            cx.notify();
        }

        /// Select the preferred row in the current table and scroll it into view
        /// (the C++ `selectPreferredProcess` + `scrollToItem`): the remembered
        /// last-attached process by name if present, else the first attachable
        /// row, else the first row — so Attach / Enter act on a real row
        /// immediately.
        fn select_preferred(&mut self, cx: &mut Context<Self>) {
            let ix = super::preferred_row_index_for(
                &self.table.read(cx).delegate().rows,
                self.last_attached.as_deref(),
            );
            if let Some(ix) = ix {
                self.table.update(cx, |state, cx| {
                    state.set_selected_row(ix, cx);
                    state.scroll_to_row(ix, cx);
                });
            }
        }

        /// Attach to a row if it is attachable (the C++ `onProcessSelected`).
        fn attach_row(&mut self, row: &ProcessRow, cx: &mut Context<Self>) {
            if !row.is_attachable() {
                return;
            }
            cx.emit(ProcessPickEvent::Attach {
                identifier: row.identifier.clone(),
                pid: row.pid,
                name: row.name.clone(),
            });
        }

        /// Attach to the currently-selected row (the Attach button).
        fn attach_selected(&mut self, cx: &mut Context<Self>) {
            let sel = self.table.read(cx).selected_row();
            if let Some(ix) = sel {
                let row = self.table.read(cx).delegate().rows.get(ix).cloned();
                if let Some(row) = row {
                    self.attach_row(&row, cx);
                }
            }
        }

        /// Cancel the picker (Esc / Cancel button).
        fn cancel(&mut self, cx: &mut Context<Self>) {
            cx.emit(ProcessPickEvent::Cancel);
        }

        /// Run a row-menu command (`process.copy_pid` / `_name` / `_path`): write
        /// the corresponding field of the menu's row to the system clipboard, then
        /// close the menu. Mirrors the C++ `QApplication::clipboard()->setText(...)`
        /// dispatch in `customContextMenuRequested`.
        fn run_row_command(&mut self, command: &str, cx: &mut Context<Self>) {
            if let Some(menu) = self.row_menu {
                let row = self
                    .table
                    .read(cx)
                    .delegate()
                    .rows
                    .get(menu.row_ix)
                    .cloned();
                if let Some(row) = row {
                    let text = match command {
                        "process.copy_pid" => row.pid.to_string(),
                        "process.copy_name" => row.name.clone(),
                        "process.copy_path" => row.path.clone(),
                        _ => String::new(),
                    };
                    if !text.is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
            }
            self.row_menu = None;
            cx.notify();
        }

        /// Capture-phase key handling: Enter attaches the selected row, Escape
        /// cancels (or, if a row menu is open, closes it). Returns `true` when
        /// handled.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            match key {
                "enter" => {
                    self.attach_selected(cx);
                    true
                }
                // Up/Down drive the result list straight from the (focused) filter,
                // like the command palette. The C++ left Up/Down inert in the
                // QLineEdit, but a keyboard-navigable filter+list is the standard
                // affordance.
                "up" | "pageup" => {
                    self.move_selection(-1, cx);
                    true
                }
                "down" | "pagedown" => {
                    self.move_selection(1, cx);
                    true
                }
                "escape" => {
                    if self.row_menu.take().is_some() {
                        cx.notify();
                    } else {
                        self.cancel(cx);
                    }
                    true
                }
                _ => false,
            }
        }

        /// Move the table selection by `delta` rows (clamped) and scroll it into
        /// view, without moving focus off the filter input. With no current
        /// selection, Down selects the first row and Up the last.
        fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
            let len = self.table.read(cx).delegate().rows.len();
            if len == 0 {
                return;
            }
            let next = match self.table.read(cx).selected_row() {
                Some(cur) => (cur as i32 + delta).clamp(0, len as i32 - 1) as usize,
                None if delta > 0 => 0,
                None => len - 1,
            };
            self.table.update(cx, |state, cx| {
                state.set_selected_row(next, cx);
                state.scroll_to_row(next, cx);
            });
            cx.notify();
        }

        /// Render the open right-click row menu as a deferred, anchored elevated
        /// surface (the C++ `QMenu` at the cursor): Copy PID / Copy Name / Copy
        /// Path, with "Copy PID" disabled for synthetic (PID-less) rows.
        fn render_row_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
            let menu = self.row_menu?;
            let has_pid = self
                .table
                .read(cx)
                .delegate()
                .rows
                .get(menu.row_ix)
                .map(|r| r.pid != 0)
                .unwrap_or(false);
            let items = process_row_menu(has_pid);
            let mut surface = crate::ui::design::elevated_surface(cx)
                .min_w(px(180.))
                .p(px(tokens::space::XS))
                .text_size(px(tokens::font::UI_MD));
            for (i, item) in items.into_iter().enumerate() {
                if let crate::ui::overlays::contextmenu::MenuItem::Action {
                    label,
                    command,
                    enabled,
                } = item
                {
                    let fg = if enabled {
                        color::text(cx)
                    } else {
                        color::text_disabled(cx)
                    };
                    let hover = color::hover_overlay(cx);
                    let row = gpui_component::h_flex()
                        .id(("process-menu-row", i))
                        .w_full()
                        .h(px(24.))
                        .px(px(tokens::space::SM))
                        .items_center()
                        .rounded(px(tokens::radius::MD))
                        .text_color(fg)
                        .child(label)
                        .when(enabled, |r| {
                            r.cursor_pointer()
                                .hover(|s| s.bg(hover))
                                .on_click(cx.listener(move |this, _e, _w, cx| {
                                    this.run_row_command(&command, cx);
                                }))
                        });
                    surface = surface.child(row);
                }
            }
            Some(
                deferred(
                    anchored()
                        .position(menu.pos)
                        .snap_to_window_with_margin(px(8.0))
                        .child(surface),
                )
                .with_priority(2)
                .into_any_element(),
            )
        }
    }

    impl Focusable for ProcessPicker {
        /// Return the FILTER INPUT's focus handle so any host that opens the picker
        /// with `window.focus(picker.focus_handle)` lands keystrokes in the filter
        /// (the C++ `ui->filterEdit->setFocus()`); the constructor also focuses it
        /// directly for the current host which opens without a focus call. The
        /// `RcxProcessPicker` `key_context` + capture-phase key handler still fire
        /// because the focused input is a descendant of the `track_focus` card.
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.filter.read(cx).focus_handle(cx)
        }
    }

    impl EventEmitter<ProcessPickEvent> for ProcessPicker {}

    impl Render for ProcessPicker {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let count = self.table.read(cx).delegate().rows.len();
            // Clamp the design 720×520 card to the live window so it stays fully
            // visible — including the Attach/Cancel footer — on a small window
            // (QA #1; the dialog layer centers with no edge clamp).
            let card_w = modal::clamp_width(720., window);
            let card_h = modal::clamp_height(520., 80., window);

            let body = modal::body(cx)
                .child(modal::help_text(
                    "Select a live process for the chosen data source.",
                    cx,
                ))
                .child(Input::new(&self.filter).w_full())
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .rounded(px(tokens::radius::LG))
                        .border_1()
                        .border_color(color::border(cx))
                        .overflow_hidden()
                        // Record the cursor position on a right mouse-down so the
                        // row menu (opened by the table's `RightClickedRow` event,
                        // which carries only the row index) anchors at the click.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, e: &MouseDownEvent, _w, _cx| {
                                this.last_right_click = e.position;
                            }),
                        )
                        .child(DataTable::new(&self.table).bordered(false).small()),
                )
                .child(
                    div()
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(color::text_muted(cx))
                        .child(format!("{count} sources")),
                );

            let footer = modal::footer(cx)
                .child(
                    Button::new("process-cancel")
                        .label("Cancel")
                        .on_click(cx.listener(|this, _e, _w, cx| this.cancel(cx))),
                )
                .child(
                    Button::new("process-attach")
                        .primary()
                        .label("Attach")
                        .on_click(cx.listener(|this, _e, _w, cx| this.attach_selected(cx))),
                );

            let row_menu = self.render_row_menu(cx);

            modal::card(cx)
                .id("rcx-process-picker")
                .track_focus(&self.focus_handle)
                .key_context("RcxProcessPicker")
                // Capture-phase key handling so Enter attaches / Escape cancels even
                // while the filter input owns focus (the C++ dialog accept/reject).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .h(card_h)
                .child(modal::header_with_close(
                    "Attach to Process",
                    "process-close",
                    cx.listener(|this, _e, _w, cx| this.cancel(cx)),
                    cx,
                ))
                .child(body)
                .child(footer)
                .children(row_menu)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        preferred_row_index, preferred_row_index_for, ProcessPickerModel, ProcessRow,
        SourceAvailability,
    };
    use crate::plugin::contract::ProcessInfo;
    use crate::provider::ProviderRegistry;
    fn available(pid: u32, name: &str) -> ProcessRow {
        ProcessRow {
            pid,
            name: name.to_string(),
            path: String::new(),
            is_32bit: false,
            availability: SourceAvailability::Available,
            identifier: name.to_lowercase(),
        }
    }

    #[test]
    fn from_registry_includes_registered_builtins_only() {
        let mgr = crate::plugin::PluginManager::with_builtins();
        let m = ProcessPickerModel::from_registry(mgr.registry());

        // The registered built-ins are attachable rows.
        let buffer = m.rows().iter().find(|r| r.identifier == "buffer").unwrap();
        assert!(buffer.is_attachable());
        assert_eq!(buffer.name, "Buffer");

        #[cfg(feature = "process-provider")]
        assert!(m.rows().iter().any(|r| {
            r.identifier == "processmemory" && r.availability == SourceAvailability::Available
        }));
        #[cfg(not(feature = "process-provider"))]
        assert!(!m.rows().iter().any(|r| r.identifier == "processmemory"));

        assert!(!m
            .rows()
            .iter()
            .any(|r| r.availability == SourceAvailability::Stub));
    }

    #[test]
    fn from_processes_builds_real_pid_rows_for_provider() {
        let m = ProcessPickerModel::from_processes(
            vec![ProcessInfo {
                pid: 42,
                name: "target.exe".to_string(),
                path: "C:/target.exe".to_string(),
                is_32bit: true,
            }],
            "processmemory",
        );
        assert_eq!(m.rows()[0].pid, 42);
        assert_eq!(m.rows()[0].identifier, "processmemory");
        assert_eq!(m.rows()[0].display_name(), "target.exe (32-bit)");
    }

    #[test]
    fn display_name_appends_arch_and_stub_tag() {
        let mut row = available(1234, "game.exe");
        assert_eq!(row.display_name(), "game.exe");
        row.is_32bit = true;
        assert_eq!(row.display_name(), "game.exe (32-bit)");

        let stub = ProcessRow {
            availability: SourceAvailability::Stub,
            ..available(0, "Kernel Memory")
        };
        assert_eq!(stub.display_name(), "Kernel Memory (stub)");
    }

    #[test]
    fn pid_text_blank_for_synthetic_rows() {
        assert_eq!(available(0, "x").pid_text(), "");
        assert_eq!(available(42, "x").pid_text(), "42");
    }

    #[test]
    fn default_sort_highest_pid_first() {
        let rows = vec![available(10, "a"), available(300, "b"), available(50, "c")];
        let m = ProcessPickerModel::from_rows(rows);
        let pids: Vec<u32> = m.rows().iter().map(|r| r.pid).collect();
        assert_eq!(pids, vec![300, 50, 10]);
    }

    #[test]
    fn sort_ties_put_attachable_before_stub() {
        // Two pid-0 rows: the attachable one should come first.
        let rows = vec![
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "zzz")
            },
            available(0, "aaa"),
        ];
        let m = ProcessPickerModel::from_rows(rows);
        assert!(m.rows()[0].is_attachable());
        assert_eq!(m.rows()[0].name, "aaa");
    }

    fn available_with_path(pid: u32, name: &str, path: &str) -> ProcessRow {
        ProcessRow {
            path: path.to_string(),
            ..available(pid, name)
        }
    }

    #[test]
    fn filter_matches_name_pid_or_path() {
        let rows = vec![
            available_with_path(1234, "notepad.exe", "C:/Windows/notepad.exe"),
            available_with_path(5678, "game.exe", "/opt/games/game.exe"),
        ];
        let m = ProcessPickerModel::from_rows(rows);

        // By name substring.
        let by_name = m.filtered("note");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "notepad.exe");

        // By PID substring.
        let by_pid = m.filtered("5678");
        assert_eq!(by_pid.len(), 1);
        assert_eq!(by_pid[0].name, "game.exe");

        // By PATH substring (case-insensitive) — the C++ cpp:378 path match that
        // was previously omitted. "/opt" only appears in game.exe's path.
        let by_path = m.filtered("/OPT/games");
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].name, "game.exe");

        // A path-only token that matches both paths keeps both.
        assert_eq!(m.filtered(".exe").len(), 2);

        // Empty keeps all.
        assert_eq!(m.filtered("  ").len(), 2);
    }

    #[test]
    fn preferred_is_first_attachable() {
        let mut reg = ProviderRegistry::new();
        reg.register_builtin("File", "file");
        let m = ProcessPickerModel::from_registry(&reg);
        let pref = m.preferred().expect("an attachable row");
        assert!(pref.is_attachable());
        assert_eq!(pref.identifier, "file");
    }

    #[test]
    fn preferred_none_when_no_rows() {
        // Empty registry means no fake rows and no attachable preferred.
        let reg = ProviderRegistry::new();
        let m = ProcessPickerModel::from_registry(&reg);
        assert!(m.preferred().is_none());
    }

    #[test]
    fn preferred_row_index_picks_first_attachable() {
        let rows = vec![
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "stub")
            },
            available(10, "real"),
            available(20, "real2"),
        ];
        // The first attachable row is index 1 (index 0 is a stub).
        assert_eq!(preferred_row_index(&rows), Some(1));
    }

    #[test]
    fn preferred_row_index_falls_back_to_first_row() {
        // No attachable row → fall back to the first row so Attach/Enter still
        // act on something (the C++ default top selection).
        let rows = vec![
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "a")
            },
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "b")
            },
        ];
        assert_eq!(preferred_row_index(&rows), Some(0));
    }

    #[test]
    fn preferred_row_index_none_for_empty() {
        assert_eq!(preferred_row_index(&[]), None);
    }

    #[test]
    fn preferred_row_index_for_matches_remembered_name_case_insensitively() {
        // The C++ `selectPreferredProcess` compares the remembered name against
        // each row's ORIGINAL name (Qt::UserRole) with Qt::CaseInsensitive.
        let rows = vec![
            available(30, "Alpha.exe"),
            available(20, "Target.exe"),
            available(10, "Gamma.exe"),
        ];
        // Different case than the row's name → still matches (index 1).
        assert_eq!(preferred_row_index_for(&rows, Some("target.EXE")), Some(1));
    }

    #[test]
    fn preferred_row_index_for_matches_original_not_display_name() {
        // The remembered name compares against the original `name`, NOT the
        // `(32-bit)`-suffixed display name. A 32-bit row whose original name is
        // "game.exe" matches "game.exe" even though display_name is
        // "game.exe (32-bit)".
        let mut row = available(99, "game.exe");
        row.is_32bit = true;
        assert_eq!(row.display_name(), "game.exe (32-bit)");
        let rows = vec![available(10, "other.exe"), row];
        assert_eq!(preferred_row_index_for(&rows, Some("game.exe")), Some(1));
        // The display-name form must NOT match the original-name compare.
        assert_eq!(
            preferred_row_index_for(&rows, Some("game.exe (32-bit)")),
            // Falls back to first attachable (index 0) since no original name matches.
            Some(0)
        );
    }

    #[test]
    fn preferred_row_index_for_missing_name_falls_back_to_first_attachable() {
        let rows = vec![
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "stub")
            },
            available(10, "real"),
        ];
        // Remembered name not present → fall back to the first attachable row (1).
        assert_eq!(
            preferred_row_index_for(&rows, Some("nonexistent.exe")),
            Some(1)
        );
    }

    #[test]
    fn preferred_row_index_for_empty_or_none_falls_back() {
        let rows = vec![
            ProcessRow {
                availability: SourceAvailability::Stub,
                ..available(0, "stub")
            },
            available(10, "real"),
        ];
        // None → first attachable.
        assert_eq!(preferred_row_index_for(&rows, None), Some(1));
        // Empty string → treated as "no remembered name" → first attachable.
        assert_eq!(preferred_row_index_for(&rows, Some("")), Some(1));
        // Empty list → None regardless of remembered name.
        assert_eq!(preferred_row_index_for(&[], Some("anything")), None);
    }
}
