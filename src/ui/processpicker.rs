//! Process picker — choose a data source to attach to, as a virtualized table.
//!
//! Port of `src/processpicker.{h,cpp,ui}` (widgets-dialogs.md §6). In the original
//! this was a modal that enumerated the running OS processes (Toolhelp on Windows,
//! `/proc` on Linux) into a 3-column `QTableWidget` (PID, Process Name, Path),
//! filterable by name-or-PID, default-sorted highest-PID-first, with double-click
//! / Attach to accept.
//!
//! **Live process enumeration is out of scope** (it is the live OS data source —
//! ARCHITECTURE §7, a documented stub). So this picker is backed by the **provider
//! registry's available sources** (the benign built-ins that *are* implemented)
//! plus **clearly-labeled stub rows** for the live process/kernel/remote sources,
//! so the surface is faithful and useful without enabling the blocked capability.
//!
//! Split (gpui-free model + a thin view):
//! - [`SourceAvailability`] — whether a row is an attachable built-in or a labeled
//!   stub.
//! - [`ProcessRow`] — one table row (PID, name, path/detail, availability, 32-bit
//!   flag), with the C++ `(32-bit)` display-name suffix.
//! - [`ProcessPickerModel`] — builds the row list from a [`ProviderRegistry`] +
//!   the standard stub set, the name-or-PID filter (`filterProcesses`), and the
//!   default highest-PID-first sort. Pure + unit-tested headlessly.
//! - [`ProcessPicker`] / [`ProcessPickEvent`] — the gpui view: a filter input
//!   above a [`DataTable`](gpui_component::table::DataTable), raising
//!   `Attach(row)` / `Cancel`.
//!
//! Gated behind the `ui` feature.

use crate::provider::ProviderRegistry;

/// Whether a picker row is an attachable source or a labeled out-of-scope stub.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceAvailability {
    /// A benign, implemented built-in source (file/buffer/snapshot/null) — the
    /// row can be selected/attached.
    Available,
    /// A live OS source (process/kernel/remote/WinDbg) — present for fidelity but
    /// **out of scope**: shown disabled with a "(stub)" tag.
    Stub,
}

impl SourceAvailability {
    /// The trailing tag shown after the name for stub rows (empty for available).
    pub fn tag(self) -> &'static str {
        match self {
            SourceAvailability::Available => "",
            SourceAvailability::Stub => " (stub — out of scope)",
        }
    }
}

/// One process-picker row — the C++ `ProcessInfo` generalized to a *source* row.
///
/// For real built-ins the `pid` is a synthetic 0 (they are not processes) and the
/// `path`/detail describes the source; for the live-source stubs the fields carry
/// representative placeholder text so the column layout matches the original.
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

/// The standard live-source stub rows (out of scope — ARCHITECTURE §7). Shown for
/// fidelity with the C++ picker, clearly labeled and non-attachable.
fn stub_rows() -> Vec<ProcessRow> {
    [
        (
            "process",
            "Process Memory",
            "Attach to a running process (live)",
        ),
        (
            "kernel",
            "Kernel Memory",
            "Kernel-mode physical memory (live)",
        ),
        (
            "remote",
            "Remote Target",
            "Remote agent over a socket (live)",
        ),
        (
            "windbg",
            "WinDbg Session",
            "WinDbg kernel/user session (live)",
        ),
    ]
    .into_iter()
    .map(|(id, name, detail)| ProcessRow {
        pid: 0,
        name: name.to_string(),
        path: detail.to_string(),
        is_32bit: false,
        availability: SourceAvailability::Stub,
        identifier: id.to_string(),
    })
    .collect()
}

/// The process-picker model — the row list + the filter, built from the provider
/// registry's available sources plus the standard stub set.
#[derive(Clone, Debug, Default)]
pub struct ProcessPickerModel {
    rows: Vec<ProcessRow>,
}

impl ProcessPickerModel {
    /// Build the model from the registry's registered providers (turned into
    /// attachable rows) plus the live-source stub rows, sorted the C++ way
    /// (highest PID first, then attachable-before-stub, then name).
    ///
    /// The C++ enumerated OS processes; here the available rows are the benign
    /// built-in providers the registry knows about (file/buffer/snapshot/null),
    /// which is the in-scope analogue of "things you can attach to".
    pub fn from_registry(registry: &ProviderRegistry) -> ProcessPickerModel {
        let mut rows: Vec<ProcessRow> = registry
            .providers()
            .iter()
            .map(|p| ProcessRow {
                pid: 0,
                name: p.name.clone(),
                path: format!("Built-in source: {}", p.identifier),
                is_32bit: false,
                availability: SourceAvailability::Available,
                identifier: p.identifier.clone(),
            })
            .collect();
        rows.extend(stub_rows());
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

    /// `filterProcesses(text)` / `applyFilter` (`processpicker.cpp`): keep rows
    /// whose **name** OR **PID** contains the (case-insensitive) query. An empty
    /// query keeps everything. Returns borrowed rows in display order.
    pub fn filtered(&self, query: &str) -> Vec<&ProcessRow> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.rows.iter().collect();
        }
        self.rows
            .iter()
            .filter(|r| r.name.to_lowercase().contains(&q) || r.pid_text().contains(&q))
            .collect()
    }

    /// The first attachable row (the C++ `selectPreferredProcess` default
    /// selection target), if any.
    pub fn preferred(&self) -> Option<&ProcessRow> {
        self.rows.iter().find(|r| r.is_attachable())
    }
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
pub use view::{ProcessPickEvent, ProcessPicker};

#[cfg(feature = "ui")]
mod view {
    use crate::ui::design::{color, tokens};
    use crate::ui::dialogs::modal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::button::{Button, ButtonVariants as _};
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::table::{
        Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState,
    };
    use gpui_component::Sizable as _;

    use super::{ProcessPickerModel, ProcessRow, SourceAvailability};

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
    struct ProcessDelegate {
        rows: Vec<ProcessRow>,
    }

    impl ProcessDelegate {
        fn new() -> Self {
            ProcessDelegate { rows: Vec::new() }
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
                COL_PATH => Column::new("path", "Path").width(px(360.)).sortable(),
                _ => Column::new("path", "Path").width(px(360.)).sortable(),
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
                return div();
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
            div()
                .text_color(fg)
                .text_size(px(tokens::font::UI_SM))
                .when(mono, |d| d.font_family(tokens::font::MONO_FAMILY))
                .child(text)
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
        focus_handle: FocusHandle,
        _subs: Vec<Subscription>,
    }

    impl ProcessPicker {
        /// Build the picker over the given model.
        pub fn new(model: ProcessPickerModel, window: &mut Window, cx: &mut Context<Self>) -> Self {
            let filter =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter by name or PID..."));
            let table = cx
                .new(|cx| TableState::new(ProcessDelegate::new(), window, cx).row_selectable(true));

            let mut subs = Vec::new();
            subs.push(cx.subscribe(&filter, |this, _input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.refresh_table(cx);
                }
            }));
            subs.push(cx.subscribe(&table, |this, table, ev: &TableEvent, cx| {
                if let TableEvent::DoubleClickedRow(row_ix) = ev {
                    let row = table.read(cx).delegate().rows.get(*row_ix).cloned();
                    if let Some(row) = row {
                        this.attach_row(&row, cx);
                    }
                }
            }));

            let mut this = ProcessPicker {
                model,
                filter,
                table,
                focus_handle: cx.focus_handle(),
                _subs: subs,
            };
            this.refresh_table(cx);
            this
        }

        /// Construct over a model as an [`Entity`].
        pub fn view(model: ProcessPickerModel, window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(|cx| ProcessPicker::new(model, window, cx))
        }

        /// The backing model (for tests / external wiring).
        pub fn model(&self) -> &ProcessPickerModel {
            &self.model
        }

        /// The current filter text.
        fn filter_text(&self, cx: &App) -> String {
            self.filter.read(cx).value().to_string()
        }

        /// Push the filtered rows into the table delegate.
        fn refresh_table(&mut self, cx: &mut Context<Self>) {
            let query = self.filter_text(cx);
            let rows: Vec<ProcessRow> = self.model.filtered(&query).into_iter().cloned().collect();
            self.table.update(cx, |state, cx| {
                state.delegate_mut().rows = rows;
                cx.notify();
            });
            cx.notify();
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
    }

    impl Focusable for ProcessPicker {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
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
                    "Select a data source to attach. Live process / kernel / remote \
                     sources are out of scope in this build and shown as stubs.",
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

            modal::card(cx)
                .id("rcx-process-picker")
                .track_focus(&self.focus_handle)
                .key_context("RcxProcessPicker")
                .w(card_w)
                .h(card_h)
                .child(
                    modal::header("Attach to Process", cx).child(modal::close_button(
                        "process-close",
                        cx.listener(|this, _e, _w, cx| this.cancel(cx)),
                        cx,
                    )),
                )
                .child(body)
                .child(footer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessPickerModel, ProcessRow, SourceAvailability};
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
    fn from_registry_includes_builtins_and_stubs() {
        let mut reg = ProviderRegistry::new();
        reg.register_builtin("Buffer", "buffer");
        reg.register_builtin("File", "file");
        let m = ProcessPickerModel::from_registry(&reg);

        // The two registered built-ins are attachable rows.
        let buffer = m.rows().iter().find(|r| r.identifier == "buffer").unwrap();
        assert!(buffer.is_attachable());
        assert_eq!(buffer.name, "Buffer");

        // The standard live-source stubs are present and NOT attachable.
        let process = m.rows().iter().find(|r| r.identifier == "process").unwrap();
        assert_eq!(process.availability, SourceAvailability::Stub);
        assert!(!process.is_attachable());
        assert!(m.rows().iter().any(|r| r.identifier == "kernel"));
        assert!(m.rows().iter().any(|r| r.identifier == "remote"));
        assert!(m.rows().iter().any(|r| r.identifier == "windbg"));
    }

    #[test]
    fn display_name_appends_arch_and_stub_tag() {
        let mut row = available(1234, "game.exe");
        assert_eq!(row.display_name(), "game.exe");
        row.is_32bit = true;
        assert_eq!(row.display_name(), "game.exe (32-bit)");

        let stub = ProcessRow {
            availability: SourceAvailability::Stub,
            ..available(0, "Process Memory")
        };
        assert_eq!(stub.display_name(), "Process Memory (stub — out of scope)");
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

    #[test]
    fn filter_matches_name_or_pid() {
        let rows = vec![available(1234, "notepad.exe"), available(5678, "game.exe")];
        let m = ProcessPickerModel::from_rows(rows);

        // By name substring.
        let by_name = m.filtered("note");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "notepad.exe");

        // By PID substring.
        let by_pid = m.filtered("5678");
        assert_eq!(by_pid.len(), 1);
        assert_eq!(by_pid[0].name, "game.exe");

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
    fn preferred_none_when_only_stubs() {
        // Empty registry → only stub rows → no attachable preferred.
        let reg = ProviderRegistry::new();
        let m = ProcessPickerModel::from_registry(&reg);
        assert!(m.preferred().is_none());
    }
}
