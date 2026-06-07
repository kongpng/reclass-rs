//! Performance Profiler dialog — the live hot-path profiler surface
//! (`profilerdialog.{h,cpp}`, widgets-dialogs.md).
//!
//! Port of `ProfilerDialog`: a modal with a horizontal bar chart of the hottest
//! buckets over a 7-column sortable table (Function / Count / Total / Mean / Min
//! / Max / Last), a control row (enable toggle, summary, Reset, Copy-CSV), and a
//! 2 Hz auto-refresh loop. Per ARCHITECTURE §5 it renders through the shared
//! [`modal`](crate::ui::dialogs::modal) scaffolding the other owned dialogs use.
//!
//! Split (gpui-free model + thin view, like the sibling dialogs):
//! - [`ProfileSortColumn`] — the 7 sortable columns + their labels / default
//!   sort direction (`profilerdialog.cpp:177`).
//! - [`sort_profile_rows`] — the pure, headlessly-tested column sort (generalizes
//!   the C++ `refreshData()` `std::sort`).
//! - [`profile_rows_to_csv`] — the "Copy CSV" serialization (`:145-158`).
//! - [`profile_summary`] — the summary line (`:274-288`).
//! - [`PROFILE_ENABLE_LABEL`] — the verbatim enable-checkbox label (`:123`).
//! - [`ProfilerDialog`] / [`ProfilerEvent`] — the gpui view + its close event.
//!
//! Gated behind the `ui` feature for the view (the model items stay gpui-free).

// ── Performance Profiler dialog ──────────────────────────────────────────────

/// The 7 sortable columns of the profiler table (`profilerdialog.cpp:177`).
/// The variant order is the on-screen column order (Function … Last).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileSortColumn {
    /// Bucket name (the only string column; sorts ascending A→Z by default).
    Function,
    /// Sample count.
    Count,
    /// Total time (the C++ default sort key, descending).
    Total,
    /// Mean (total / count).
    Mean,
    /// Minimum sample.
    Min,
    /// Maximum sample.
    Max,
    /// Most recent sample.
    Last,
}

impl ProfileSortColumn {
    /// The 7 columns left→right (table header order).
    pub const ALL: [ProfileSortColumn; 7] = [
        ProfileSortColumn::Function,
        ProfileSortColumn::Count,
        ProfileSortColumn::Total,
        ProfileSortColumn::Mean,
        ProfileSortColumn::Min,
        ProfileSortColumn::Max,
        ProfileSortColumn::Last,
    ];

    /// The header label (`profilerdialog.cpp:177`).
    pub fn label(self) -> &'static str {
        match self {
            ProfileSortColumn::Function => "Function",
            ProfileSortColumn::Count => "Count",
            ProfileSortColumn::Total => "Total (ms)",
            ProfileSortColumn::Mean => "Mean (\u{00b5}s)",
            ProfileSortColumn::Min => "Min (\u{00b5}s)",
            ProfileSortColumn::Max => "Max (\u{00b5}s)",
            ProfileSortColumn::Last => "Last (\u{00b5}s)",
        }
    }

    /// Whether this column's natural/default sort direction is descending. The
    /// C++ default view is total-descending; the name column reads better
    /// ascending, every numeric column most-interesting-first (descending).
    pub fn default_descending(self) -> bool {
        !matches!(self, ProfileSortColumn::Function)
    }
}

/// Sort `(name, stats)` rows by `column` in the given direction. Pure (no UI),
/// so it's unit-tested headlessly. Mirrors the C++ `std::sort` in
/// `refreshData()` (which always sorts by `totalNs` desc) generalized to any
/// column / direction (the Qt table's clickable-header sort).
pub fn sort_profile_rows(
    rows: &mut [(String, crate::theme::ProfileStats)],
    column: ProfileSortColumn,
    descending: bool,
) {
    rows.sort_by(|a, b| {
        let ord = match column {
            ProfileSortColumn::Function => a.0.cmp(&b.0),
            ProfileSortColumn::Count => a.1.count.cmp(&b.1.count),
            ProfileSortColumn::Total => a.1.total_ns.cmp(&b.1.total_ns),
            ProfileSortColumn::Mean => {
                a.1.mean_ns()
                    .partial_cmp(&b.1.mean_ns())
                    .unwrap_or(std::cmp::Ordering::Equal)
            }
            ProfileSortColumn::Min => a.1.min_ns_display().cmp(&b.1.min_ns_display()),
            ProfileSortColumn::Max => a.1.max_ns.cmp(&b.1.max_ns),
            ProfileSortColumn::Last => a.1.last_ns.cmp(&b.1.last_ns),
        };
        // Apply the descending flag to the PRIMARY column only, then break ties
        // by name ascending so the order is deterministic (the C++ QHash
        // iteration order is unstable; we make it stable). The tiebreak stays
        // ascending in both directions — reversing it too would flip equal-key
        // rows into name-descending order.
        let ord = if descending { ord.reverse() } else { ord };
        ord.then_with(|| a.0.cmp(&b.0))
    });
}

/// The verbatim profiler enable-checkbox label (`profilerdialog.cpp:123`). The
/// C++ checkbox names the three instrumented hot paths exactly, rather than a
/// generic "Profile hot paths".
pub const PROFILE_ENABLE_LABEL: &str = "Profile compose / refresh / applyDocument";

/// One CSV line per bucket plus the header, as `profilerdialog.cpp:145-158`'s
/// "Copy CSV": `name,count,total_ms,mean_us,min_us,max_us,last_us`. Pure, so
/// it's unit-tested. The rows are emitted in the order given (the dialog passes
/// the currently-sorted rows).
pub fn profile_rows_to_csv(rows: &[(String, crate::theme::ProfileStats)]) -> String {
    let mut out = String::from("name,count,total_ms,mean_us,min_us,max_us,last_us");
    for (name, s) in rows {
        out.push('\n');
        out.push_str(&format!(
            "{},{},{:.4},{:.3},{:.3},{:.3},{:.3}",
            name,
            s.count,
            s.total_ns as f64 / 1.0e6,
            s.mean_ns() / 1.0e3,
            s.min_ns_display() as f64 / 1.0e3,
            s.max_ns as f64 / 1.0e3,
            s.last_ns as f64 / 1.0e3,
        ));
    }
    out
}

/// The summary line (`profilerdialog.cpp:274-288`):
/// `"<buckets> buckets · <samples> samples · <ms> ms total"`, or `"(no data)"`.
pub fn profile_summary(rows: &[(String, crate::theme::ProfileStats)]) -> String {
    if rows.is_empty() {
        return "(no data)".to_string();
    }
    let mut grand_total: u64 = 0;
    let mut grand_count: u64 = 0;
    for (_n, s) in rows {
        grand_total = grand_total.saturating_add(s.total_ns);
        grand_count = grand_count.saturating_add(s.count);
    }
    format!(
        "{} buckets \u{00b7} {} samples \u{00b7} {:.2} ms total",
        rows.len(),
        grand_count,
        grand_total as f64 / 1.0e6,
    )
}

#[cfg(feature = "ui")]
pub use profiler_view::{ProfilerDialog, ProfilerEvent};

#[cfg(feature = "ui")]
mod profiler_view {
    use super::{
        profile_rows_to_csv, profile_summary, sort_profile_rows, ProfileSortColumn,
        PROFILE_ENABLE_LABEL,
    };
    use crate::theme::{ProfileStats, Profiler};
    use crate::ui::design::{color, tokens};
    use crate::ui::dialogs::modal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::{checkbox::Checkbox, ActiveTheme, Sizable as _};
    use std::time::Duration;

    /// The dialog's outcome (the C++ `QDialog::accept`/`reject` — the profiler
    /// dialog only ever closes).
    #[derive(Clone, Debug)]
    pub enum ProfilerEvent {
        /// Close the dialog (Esc / `×` / clicking outside).
        Close,
    }

    /// Live performance-profiler view — port of `ProfilerDialog`
    /// (`profilerdialog.{h,cpp}`).
    ///
    /// Top: a horizontal bar chart of the 15 hottest buckets by total time.
    /// Bottom: the full 7-column sortable table (Function / Count / Total /
    /// Mean / Min / Max / Last). A control row carries the enable toggle, a
    /// summary, Reset, and Copy-CSV. Auto-refreshes at ~2 Hz while open via a
    /// spawned timer loop (the C++ `QTimer(500ms)` started in `showEvent`).
    ///
    /// **Auto-enables profiling on open** (the C++ checkbox reflects the global
    /// flag; opening the dialog is the user's intent to profile) and restores
    /// the prior flag state when the dialog closes, so opening the dialog never
    /// silently leaves profiling running.
    pub struct ProfilerDialog {
        /// The current snapshot, already sorted for display.
        rows: Vec<(String, ProfileStats)>,
        /// Active sort column + direction (clickable header; default Total desc).
        sort_column: ProfileSortColumn,
        sort_descending: bool,
        /// The global enabled flag captured on open, restored on close.
        prior_enabled: bool,
        focus_handle: FocusHandle,
        /// The 2 Hz refresh loop (dropped → cancelled when the view drops).
        _refresh: Task<()>,
    }

    impl ProfilerDialog {
        /// Build the dialog. Captures the prior enabled state, **auto-enables**
        /// profiling (the C++ checkbox starts checked iff already enabled; we go
        /// one better and turn it on so the dialog has data to show), takes an
        /// initial snapshot, and starts the 2 Hz refresh loop.
        pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let prior_enabled = Profiler::is_enabled();
            Profiler::set_enabled(true);

            // Start the ~2 Hz auto-refresh loop (profilerdialog.cpp:196-198).
            let refresh = cx.spawn_in(window, async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.refresh();
                        cx.notify();
                    })
                    .is_err()
                {
                    break; // view dropped — stop the loop
                }
            });

            let mut this = ProfilerDialog {
                rows: Vec::new(),
                sort_column: ProfileSortColumn::Total,
                sort_descending: true,
                prior_enabled,
                focus_handle: cx.focus_handle(),
                _refresh: refresh,
            };
            this.refresh();
            this
        }

        /// Re-read `Profiler::snapshot()` and re-sort by the active column
        /// (`refreshData()`, profilerdialog.cpp:223-235).
        fn refresh(&mut self) {
            let mut rows = Profiler::snapshot();
            sort_profile_rows(&mut rows, self.sort_column, self.sort_descending);
            self.rows = rows;
        }

        /// The rows shown / exported (currently-sorted snapshot).
        pub fn rows(&self) -> &[(String, ProfileStats)] {
            &self.rows
        }

        /// The active sort (column, descending) — exposed for tests / wiring.
        pub fn sort(&self) -> (ProfileSortColumn, bool) {
            (self.sort_column, self.sort_descending)
        }

        /// Toggle profiling (`onEnabledToggled`, profilerdialog.cpp:213-216).
        fn set_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
            Profiler::set_enabled(on);
            self.refresh();
            cx.notify();
        }

        /// Reset all aggregated data (`onReset`, profilerdialog.cpp:218-221).
        fn reset(&mut self, cx: &mut Context<Self>) {
            Profiler::reset();
            self.refresh();
            cx.notify();
        }

        /// Copy the CSV of the current snapshot to the clipboard (the C++ "Copy
        /// CSV" button; profilerdialog.cpp:141-161). Uses the currently-sorted
        /// rows so the export matches what's on screen.
        fn copy_csv(&self, cx: &mut Context<Self>) {
            let csv = profile_rows_to_csv(&self.rows);
            cx.write_to_clipboard(ClipboardItem::new_string(csv));
        }

        /// Click a column header: toggle direction if already sorted by it, else
        /// switch to it at its default direction (the Qt clickable-header sort).
        fn sort_by(&mut self, column: ProfileSortColumn, cx: &mut Context<Self>) {
            if self.sort_column == column {
                self.sort_descending = !self.sort_descending;
            } else {
                self.sort_column = column;
                self.sort_descending = column.default_descending();
            }
            self.refresh();
            cx.notify();
        }

        /// Close the dialog and restore the prior profiling-enabled state.
        fn close(&mut self, cx: &mut Context<Self>) {
            Profiler::set_enabled(self.prior_enabled);
            cx.emit(ProfilerEvent::Close);
        }

        // ── Rendering ──

        /// The top control row: enable checkbox, summary, Reset, Copy-CSV
        /// (profilerdialog.cpp:119-163).
        fn render_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::Button;
            let enabled = Profiler::is_enabled();
            let summary = profile_summary(&self.rows);
            let mono = SharedString::from(tokens::font::mono_family());

            gpui_component::h_flex()
                .w_full()
                .items_center()
                .gap(px(tokens::space::MD))
                .child(
                    Checkbox::new("prof-enable")
                        // Verbatim C++ label (profilerdialog.cpp:123).
                        .label(PROFILE_ENABLE_LABEL)
                        .checked(enabled)
                        .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                            this.set_enabled(*checked, cx);
                        })),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .font_family(mono)
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child(summary),
                )
                .child(
                    Button::new("prof-reset")
                        .small()
                        .label("Reset")
                        .on_click(cx.listener(|this, _e, _window, cx| this.reset(cx))),
                )
                .child(
                    Button::new("prof-copy")
                        .small()
                        .label("Copy CSV")
                        .on_click(cx.listener(|this, _e, _window, cx| this.copy_csv(cx))),
                )
        }

        /// The bar chart: the top 15 buckets by total time, each a horizontal
        /// bar proportional to the hottest bucket's total, colored by rank
        /// (hot → warm → cold → neutral). Port of `BarChart::paintEvent`
        /// (profilerdialog.cpp:38-100) onto flex divs.
        fn render_chart(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let mono = SharedString::from(tokens::font::mono_family());
            let chart = gpui_component::v_flex()
                .w_full()
                .h(px(220.))
                .p(px(tokens::space::SM))
                .gap(px(2.))
                .rounded(px(tokens::radius::MD))
                .border_1()
                .border_color(color::border(cx))
                .bg(color::content_bg(cx))
                .overflow_hidden();

            if self.rows.is_empty() {
                return chart.items_center().justify_center().child(
                    div()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text_muted(cx))
                        .child("(no samples \u{2014} enable profiling above)"),
                );
            }

            // Top entry's total maps to the full bar width.
            let max_total = self.rows.first().map(|r| r.1.total_ns).unwrap_or(1).max(1);
            let theme = cx.theme();
            // Rank → color (the C++ indHeat* gradient; we map onto the gpui
            // semantic palette: hot=red, warm=yellow, cold=blue, rest=muted).
            let rank_color = |i: usize| -> Hsla {
                if i == 0 {
                    theme.danger
                } else if i <= 2 {
                    theme.warning
                } else if i <= 5 {
                    theme.blue
                } else {
                    theme.muted_foreground
                }
            };

            let bars = self.rows.iter().take(15).enumerate().map(|(i, (name, s))| {
                let frac = (s.total_ns as f64 / max_total as f64).clamp(0.0, 1.0);
                let total_ms = s.total_ns as f64 / 1.0e6;
                let value_text = format!("{total_ms:.2} ms  \u{00d7}{}", s.count);
                gpui_component::h_flex()
                    .w_full()
                    .h(px(16.))
                    .items_center()
                    .gap(px(tokens::space::SM))
                    .child(
                        // Label (truncated) — fixed left column.
                        div()
                            .flex_none()
                            .w(px(150.))
                            .truncate()
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::text(cx))
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        // The bar track grows to fill; the fill is a fraction.
                        gpui_component::h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(tokens::space::SM))
                            .child(
                                div()
                                    .flex_none()
                                    .h(px(12.))
                                    .rounded(px(2.))
                                    .bg(rank_color(i))
                                    .w(relative(frac as f32)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(mono.clone())
                                    .text_size(px(tokens::font::UI_XS))
                                    .text_color(color::text_muted(cx))
                                    .child(value_text),
                            ),
                    )
            });

            chart.children(bars)
        }

        /// One header cell: clickable, shows the sort arrow on the active column.
        fn header_cell(&self, column: ProfileSortColumn, cx: &mut Context<Self>) -> Stateful<Div> {
            let active = self.sort_column == column;
            let arrow = if active {
                if self.sort_descending {
                    " \u{25be}" // ▾
                } else {
                    " \u{25b4}" // ▴
                }
            } else {
                ""
            };
            let label = format!("{}{arrow}", column.label());
            // The Function (name) column is the only left-aligned one and gets a
            // wider minimum so names aren't crushed (matches the body cell).
            let is_name = matches!(column, ProfileSortColumn::Function);
            div()
                .id(SharedString::from(format!("prof-hdr-{}", column.label())))
                .flex_grow()
                .flex_shrink()
                .flex_basis(relative(0.))
                .when(is_name, |d| d.min_w(px(120.)))
                .px(px(tokens::space::SM))
                .py(px(tokens::space::XS))
                .when(!is_name, |d| d.flex().justify_end())
                .text_size(px(tokens::font::UI_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if active {
                    color::text(cx)
                } else {
                    color::text_muted(cx)
                })
                .cursor_pointer()
                .hover(|s| s.text_color(color::text(cx)))
                .on_click(cx.listener(move |this, _e, _window, cx| this.sort_by(column, cx)))
                .child(label)
        }

        /// The 7-column sortable table (profilerdialog.cpp:174-272).
        fn render_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let mono = SharedString::from(tokens::font::mono_family());

            let header = gpui_component::h_flex()
                .w_full()
                .border_b_1()
                .border_color(color::border(cx))
                .child(self.header_cell(ProfileSortColumn::Function, cx))
                .child(self.header_cell(ProfileSortColumn::Count, cx))
                .child(self.header_cell(ProfileSortColumn::Total, cx))
                .child(self.header_cell(ProfileSortColumn::Mean, cx))
                .child(self.header_cell(ProfileSortColumn::Min, cx))
                .child(self.header_cell(ProfileSortColumn::Max, cx))
                .child(self.header_cell(ProfileSortColumn::Last, cx));

            let num_cell = |text: String, mono: &SharedString, cx: &App| -> Div {
                div()
                    .flex_grow()
                    .flex_shrink()
                    .flex_basis(relative(0.))
                    .flex()
                    .justify_end()
                    .px(px(tokens::space::SM))
                    .py(px(2.))
                    .font_family(mono.clone())
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(color::text(cx))
                    .child(text)
            };

            let rows = self.rows.iter().enumerate().map(|(i, (name, s))| {
                let alt = i % 2 == 1;
                gpui_component::h_flex()
                    .w_full()
                    .when(alt, |r| r.bg(color::panel_bg(cx)))
                    .child(
                        div()
                            .flex_grow()
                            .flex_shrink()
                            .flex_basis(relative(0.))
                            .min_w(px(120.))
                            .px(px(tokens::space::SM))
                            .py(px(2.))
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(color::text(cx))
                            .child(SharedString::from(name.clone())),
                    )
                    .child(num_cell(format!("{}", s.count), &mono, cx))
                    .child(num_cell(
                        format!("{:.3}", s.total_ns as f64 / 1.0e6),
                        &mono,
                        cx,
                    ))
                    .child(num_cell(format!("{:.2}", s.mean_ns() / 1.0e3), &mono, cx))
                    .child(num_cell(
                        format!("{:.2}", s.min_ns_display() as f64 / 1.0e3),
                        &mono,
                        cx,
                    ))
                    .child(num_cell(
                        format!("{:.2}", s.max_ns as f64 / 1.0e3),
                        &mono,
                        cx,
                    ))
                    .child(num_cell(
                        format!("{:.2}", s.last_ns as f64 / 1.0e3),
                        &mono,
                        cx,
                    ))
            });

            gpui_component::v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .rounded(px(tokens::radius::MD))
                .border_1()
                .border_color(color::border(cx))
                .bg(color::content_bg(cx))
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .id("prof-table-body")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .child(gpui_component::v_flex().w_full().children(rows)),
                )
        }
    }

    impl Focusable for ProfilerDialog {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<ProfilerEvent> for ProfilerDialog {}

    impl Render for ProfilerDialog {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // The C++ dialog is 820×640; clamp to the live window (QA #1).
            let card_w = modal::clamp_width(820., window);
            let card_h = modal::clamp_height(640., 48., window);

            let body = modal::body(cx)
                .child(self.render_controls(cx))
                .child(self.render_chart(cx))
                .child(self.render_table(cx));

            modal::card(cx)
                .id("rcx-profiler-dialog")
                .track_focus(&self.focus_handle)
                .key_context("RcxProfiler")
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if ev.keystroke.key.as_str() == "escape" {
                        this.close(cx);
                        cx.stop_propagation();
                    }
                }))
                .w(card_w)
                .h(card_h)
                .child(modal::header_with_close(
                    "Performance Profiler",
                    "prof-close",
                    cx.listener(|this, _e, _window, cx| this.close(cx)),
                    cx,
                ))
                .child(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ProfileStats;

    #[test]
    fn profile_enable_label_is_verbatim_cpp() {
        // The enable checkbox must name the three instrumented hot paths
        // verbatim (profilerdialog.cpp:123), not the generic "Profile hot paths".
        assert_eq!(
            PROFILE_ENABLE_LABEL,
            "Profile compose / refresh / applyDocument"
        );
        assert_ne!(PROFILE_ENABLE_LABEL, "Profile hot paths");
    }

    fn mk(total_ns: u64, count: u64, min_ns: u64, max_ns: u64, last_ns: u64) -> ProfileStats {
        ProfileStats {
            total_ns,
            count,
            min_ns,
            max_ns,
            last_ns,
        }
    }

    fn sample_rows() -> Vec<(String, ProfileStats)> {
        vec![
            ("compose".to_string(), mk(900, 3, 100, 500, 300)),
            ("refresh".to_string(), mk(1500, 5, 100, 600, 200)),
            ("apply".to_string(), mk(300, 1, 300, 300, 300)),
        ]
    }

    #[test]
    fn sort_by_total_descending_default_view() {
        let mut rows = sample_rows();
        sort_profile_rows(&mut rows, ProfileSortColumn::Total, true);
        let names: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
        // refresh(1500) > compose(900) > apply(300).
        assert_eq!(names, vec!["refresh", "compose", "apply"]);
    }

    #[test]
    fn sort_by_function_ascending() {
        let mut rows = sample_rows();
        sort_profile_rows(&mut rows, ProfileSortColumn::Function, false);
        let names: Vec<_> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(names, vec!["apply", "compose", "refresh"]);
    }

    #[test]
    fn sort_by_count_descending() {
        let mut rows = sample_rows();
        sort_profile_rows(&mut rows, ProfileSortColumn::Count, true);
        assert_eq!(rows[0].0, "refresh"); // count 5
        assert_eq!(rows[2].0, "apply"); // count 1
    }

    #[test]
    fn sort_is_stable_by_name_tiebreak() {
        // Two buckets with the same total → name ascending tiebreak.
        let mut rows = vec![
            ("zebra".to_string(), mk(100, 1, 100, 100, 100)),
            ("alpha".to_string(), mk(100, 1, 100, 100, 100)),
        ];
        sort_profile_rows(&mut rows, ProfileSortColumn::Total, true);
        assert_eq!(rows[0].0, "alpha");
        assert_eq!(rows[1].0, "zebra");
    }

    #[test]
    fn default_descending_only_for_numeric_columns() {
        assert!(!ProfileSortColumn::Function.default_descending());
        for c in ProfileSortColumn::ALL.iter().skip(1) {
            assert!(c.default_descending(), "{:?}", c);
        }
    }

    #[test]
    fn csv_header_and_rows() {
        let rows = vec![(
            "compose".to_string(),
            mk(2_000_000, 4, 100_000, 900_000, 500_000),
        )];
        let csv = profile_rows_to_csv(&rows);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "name,count,total_ms,mean_us,min_us,max_us,last_us"
        );
        // total_ms = 2.0, mean_us = (2e6/4)/1e3 = 500.0, min=100, max=900, last=500.
        assert_eq!(
            lines.next().unwrap(),
            "compose,4,2.0000,500.000,100.000,900.000,500.000"
        );
    }

    #[test]
    fn csv_empty_is_header_only() {
        assert_eq!(
            profile_rows_to_csv(&[]),
            "name,count,total_ms,mean_us,min_us,max_us,last_us"
        );
    }

    #[test]
    fn summary_no_data() {
        assert_eq!(profile_summary(&[]), "(no data)");
    }

    #[test]
    fn summary_aggregates() {
        let rows = sample_rows();
        // 3 buckets, samples = 3+5+1 = 9, total = 900+1500+300 = 2700 ns = 0.0027 ms.
        let s = profile_summary(&rows);
        assert!(s.starts_with("3 buckets"), "{s}");
        assert!(s.contains("9 samples"), "{s}");
        assert!(s.contains("0.00 ms total"), "{s}");
    }
}
