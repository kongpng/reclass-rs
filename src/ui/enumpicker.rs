//! Enum-member picker popup — themed enum-value chooser
//! (`widgets/enum_picker_popup.h`, widgets-dialogs.md §10).
//!
//! Port of `EnumPickerPopup`: a popover listing an enum's members with a fuzzy
//! filter (visible only when >10 members), pre-selecting the member matching the
//! current value, and reporting the chosen value via a callback. Per the cookbook
//! (ARCHITECTURE §5) it maps onto a `Popover` + a `List`. This ports the pure
//! filter/sort model (unit-tested) + a popover view.
//!
//! Filter uses the two-pass [`fuzzy_score`](crate::ui::fuzzy::fuzzy_score) (the C++
//! enum picker calls `rcx::fuzzyScore` from `fuzzy_match.h`); sort is **score
//! desc** while searching, else **by value ascending** (`applyFilter`).
//!
//! Gated behind the `ui` feature for the view; the model is always built/tested.

/// `struct Member { QString name; int64_t value; }` (`enum_picker_popup.h:34`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub name: String,
    pub value: i64,
}

impl Member {
    pub fn new(name: &str, value: i64) -> Self {
        Member {
            name: name.to_string(),
            value,
        }
    }
}

/// The filter threshold: the filter box is shown only when there are **>10**
/// members (`show`: `filter visibility = members > 10`).
pub const FILTER_THRESHOLD: usize = 10;

/// A rendered member row: the member + its fuzzy match positions (for highlight).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MemberRow {
    pub member: Member,
    pub match_positions: Vec<usize>,
}

/// The enum-picker list model + filter (`applyFilter`, `enum_picker_popup.h:334`).
#[derive(Clone, Debug, Default)]
pub struct EnumPickerModel {
    members: Vec<Member>,
    rows: Vec<MemberRow>,
    /// The current enum value (pre-selected on open).
    current_value: i64,
    selected: Option<usize>,
}

impl EnumPickerModel {
    /// Build a model over the enum's members, pre-selecting the row matching
    /// `current_value` (center-scrolled in the C++).
    pub fn new(members: Vec<Member>, current_value: i64) -> Self {
        let mut m = EnumPickerModel {
            members,
            rows: Vec::new(),
            current_value,
            selected: None,
        };
        m.apply_filter("");
        m
    }

    /// All members.
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// The current enum value the picker opened on (used by the view to mark the
    /// active member with a checkmark).
    pub fn current_value(&self) -> i64 {
        self.current_value
    }

    /// Whether the filter box should be shown (>10 members).
    pub fn filter_visible(&self) -> bool {
        self.members.len() > FILTER_THRESHOLD
    }

    /// The rendered rows after filtering (in display order).
    pub fn rows(&self) -> &[MemberRow] {
        &self.rows
    }

    /// The number of rendered rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The selected row index.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The value of the selected member, if any.
    pub fn selected_value(&self) -> Option<i64> {
        self.selected
            .and_then(|r| self.rows.get(r))
            .map(|row| row.member.value)
    }

    /// Re-filter against `pat` (`applyFilter`): empty → all members **sorted by
    /// value ascending**; non-empty → fuzzy-scored, keep `>0`, **sorted by score
    /// desc**. Pre-selects the row matching the current value (empty filter) or
    /// the first row (filtering).
    pub fn apply_filter(&mut self, pat: &str) {
        let trimmed = pat.trim();
        let search_active = !trimmed.is_empty();
        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (i, m) in self.members.iter().enumerate() {
            let mut pos = Vec::new();
            let sc = if search_active {
                crate::ui::fuzzy::fuzzy_score(trimmed, &m.name, Some(&mut pos))
            } else {
                1
            };
            if sc == 0 {
                continue;
            }
            scored.push((sc, i, pos));
        }
        // Sort: search active → score desc; else by value ascending.
        if search_active {
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(self.members[a.1].value.cmp(&self.members[b.1].value))
            });
        } else {
            scored.sort_by(|a, b| self.members[a.1].value.cmp(&self.members[b.1].value));
        }
        self.rows = scored
            .into_iter()
            .map(|(_, i, pos)| MemberRow {
                member: self.members[i].clone(),
                match_positions: pos,
            })
            .collect();

        // Pre-select: the current value (empty filter) else row 0.
        self.selected = if self.rows.is_empty() {
            None
        } else if search_active {
            Some(0)
        } else {
            self.rows
                .iter()
                .position(|r| r.member.value == self.current_value)
                .or(Some(0))
        };
    }

    /// Move selection down (Down), clamped.
    pub fn move_down(&mut self) {
        match self.selected {
            Some(r) if r + 1 < self.rows.len() => self.selected = Some(r + 1),
            None if !self.rows.is_empty() => self.selected = Some(0),
            _ => {}
        }
    }

    /// Move selection up (Up), clamped.
    pub fn move_up(&mut self) {
        if let Some(r) = self.selected {
            if r > 0 {
                self.selected = Some(r - 1);
            }
        }
    }

    /// Move selection down by `page` rows (PageDown), clamped to the last row.
    pub fn page_down(&mut self, page: usize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let from = self.selected.unwrap_or(0);
        self.selected = Some((from + page.max(1)).min(last));
    }

    /// Move selection up by `page` rows (PageUp), clamped to the first row.
    pub fn page_up(&mut self, page: usize) {
        if let Some(r) = self.selected {
            self.selected = Some(r.saturating_sub(page.max(1)));
        } else if !self.rows.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Select the first row (Home).
    pub fn move_home(&mut self) {
        if !self.rows.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Select the last row (End).
    pub fn move_end(&mut self) {
        if !self.rows.is_empty() {
            self.selected = Some(self.rows.len() - 1);
        }
    }

    /// Select + return the value of a clicked row (`acceptRow`).
    pub fn select_row(&mut self, row: usize) -> Option<i64> {
        if row < self.rows.len() {
            self.selected = Some(row);
            Some(self.rows[row].member.value)
        } else {
            None
        }
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{EnumPickerEvent, EnumPickerPopup};

#[cfg(feature = "ui")]
mod view {
    use super::EnumPickerModel;
    use crate::ui::design::{color, highlighted_spans, icon, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};


    /// The picker's outcome.
    #[derive(Clone, Debug)]
    pub enum EnumPickerEvent {
        /// A member value was chosen (`m_onChosen(value)`).
        Chosen(i64),
        /// Dismissed.
        Dismissed,
    }

    /// The enum-picker popover view.
    pub struct EnumPickerPopup {
        enum_name: String,
        model: EnumPickerModel,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        /// Scrolls the list so the keyboard-selected member stays visible (item 9).
        list_scroll: ScrollHandle,
        _subscription: Subscription,
    }

    impl EnumPickerPopup {
        /// Build the picker for `enum_name` over its `members`, pre-selecting the
        /// member matching `current_value`.
        pub fn new(
            enum_name: &str,
            members: Vec<super::Member>,
            current_value: i64,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let model = EnumPickerModel::new(members, current_value);
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter members..."));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        this.model.apply_filter(&q);
                        cx.notify();
                    }
                });
            EnumPickerPopup {
                enum_name: enum_name.to_string(),
                model,
                input,
                focus_handle: cx.focus_handle(),
                list_scroll: ScrollHandle::new(),
                _subscription: subscription,
            }
        }

        /// Read-only access to the model.
        pub fn model(&self) -> &EnumPickerModel {
            &self.model
        }

        /// Scroll the selected member into view (item 9). Rows render 1:1 with
        /// model rows, so the model index is the rendered child index.
        fn scroll_selected_into_view(&self) {
            if let Some(sel) = self.model.selected() {
                self.list_scroll.scroll_to_item(sel);
            }
        }

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if let Some(value) = self.model.select_row(row) {
                cx.emit(EnumPickerEvent::Chosen(value));
            }
        }

        /// Hover over a member moves the selection highlight to it (item 2) so
        /// keyboard + mouse selection stay in sync.
        fn hover_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.selected() != Some(row) {
                self.model.select_row(row);
                cx.notify();
            }
        }

        /// Keyboard navigation (the C++ enum picker `eventFilter`): Up/Down move the
        /// selected member (clamped), Enter chooses it, Esc dismisses. Returns
        /// `true` when handled so the caller stops propagation.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            const PAGE: usize = 10;
            match key {
                "down" => {
                    self.model.move_down();
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "up" => {
                    self.model.move_up();
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "pagedown" => {
                    self.model.page_down(PAGE);
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "pageup" => {
                    self.model.page_up(PAGE);
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "home" => {
                    self.model.move_home();
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "end" => {
                    self.model.move_end();
                    self.scroll_selected_into_view();
                    cx.notify();
                    true
                }
                "enter" => {
                    if let Some(value) = self.model.selected_value() {
                        cx.emit(EnumPickerEvent::Chosen(value));
                    }
                    true
                }
                "escape" => {
                    cx.emit(EnumPickerEvent::Dismissed);
                    true
                }
                _ => false,
            }
        }
    }

    impl Focusable for EnumPickerPopup {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<EnumPickerEvent> for EnumPickerPopup {}

    impl Render for EnumPickerPopup {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let selected = self.model.selected();
            let fg = color::text(cx);
            let muted = color::text_muted(cx);
            let accent = color::accent(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);
            let current = self.model.current_value();
            let query = self.input.read(cx).value().to_string();
            let filtering = !query.trim().is_empty();

            let rows: Vec<AnyElement> = self
                .model
                .rows()
                .iter()
                .enumerate()
                .map(|(row, r)| {
                    let is_sel = selected == Some(row);
                    let is_current = r.member.value == current;
                    let name_spans = if filtering {
                        highlighted_spans(&r.member.name, &r.match_positions, fg, accent)
                    } else {
                        vec![div()
                            .text_color(fg)
                            .child(r.member.name.clone())
                            .into_any_element()]
                    };
                    gpui_component::h_flex()
                        .id(("enum-row", row))
                        .w_full()
                        .h(px(26.))
                        .px(px(tokens::space::MD))
                        .gap(px(tokens::space::MD))
                        .items_center()
                        .rounded(px(tokens::radius::MD))
                        .text_size(px(tokens::font::UI_MD))
                        .when(is_sel, |d| d.bg(sel_bg))
                        .when(!is_sel, |d| d.hover(|s| s.bg(hover_bg)))
                        .cursor_pointer()
                        // Hover-to-select (item 2): keep keyboard + mouse in sync.
                        .on_mouse_move(cx.listener(move |this, _e, _window, cx| {
                            this.hover_row(row, cx);
                        }))
                        .on_click(cx.listener(move |this, _e, _window, cx| {
                            this.accept_row(row, cx);
                        }))
                        // Left check slot: the current enum value gets a ✓ SVG.
                        .child(
                            div()
                                .w(px(14.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .text_color(accent)
                                .when(is_current, |d| d.child(icon::check().size_3())),
                        )
                        .child(
                            gpui_component::h_flex()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .children(name_spans),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(tokens::font::mono_family())
                                .text_size(px(tokens::font::EDITOR_SIZE))
                                .text_color(muted)
                                .child(format!("0x{:x}", r.member.value)),
                        )
                        .into_any_element()
                })
                .collect();

            crate::ui::design::elevated_surface(cx)
                .id("rcx-enum-picker")
                .track_focus(&self.focus_handle)
                .key_context("RcxEnumPicker")
                // Capture-phase key handling so Up/Down/Enter/Esc drive the member
                // list even while the filter input owns focus (the C++ eventFilter
                // forwarding from the line-edit to the list).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .flex()
                .flex_col()
                .min_w(px(280.))
                .max_h(px(360.))
                .text_size(px(tokens::font::UI_MD))
                .child(
                    div()
                        .w_full()
                        .px(px(tokens::space::MD))
                        .py(px(tokens::space::SM))
                        .border_b_1()
                        .border_color(border)
                        .text_size(px(tokens::font::UI_SM))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(muted)
                        .child(format!("enum {}", self.enum_name)),
                )
                .when(self.model.filter_visible(), |this| {
                    this.child(
                        gpui_component::h_flex()
                            .px(px(tokens::space::MD))
                            .py(px(tokens::space::SM))
                            .gap(px(tokens::space::SM))
                            .items_center()
                            .border_b_1()
                            .border_color(border)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(muted)
                                    .child(icon::search().size_3()),
                            )
                            .child(div().flex_1().child(Input::new(&self.input).w_full())),
                    )
                })
                .child(
                    // Scrollable member list (item 9): the keyboard-selected
                    // member scrolls into view via `list_scroll.scroll_to_item`.
                    gpui_component::v_flex()
                        .id("rcx-enum-picker-list")
                        .p(px(tokens::space::XS))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.list_scroll)
                        .children(rows),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EnumPickerModel, Member, FILTER_THRESHOLD};

    fn flags() -> Vec<Member> {
        vec![
            Member::new("None", 0),
            Member::new("Visible", 1),
            Member::new("Hidden", 2),
            Member::new("Selected", 4),
            Member::new("Disabled", 8),
        ]
    }

    #[test]
    fn empty_filter_sorts_by_value_ascending() {
        // Deliberately unsorted input.
        let members = vec![
            Member::new("D", 8),
            Member::new("A", 1),
            Member::new("C", 4),
            Member::new("B", 2),
        ];
        let model = EnumPickerModel::new(members, 0);
        let values: Vec<i64> = model.rows().iter().map(|r| r.member.value).collect();
        assert_eq!(values, vec![1, 2, 4, 8]);
    }

    #[test]
    fn preselects_current_value() {
        let model = EnumPickerModel::new(flags(), 4);
        let sel = model.selected().unwrap();
        assert_eq!(model.rows()[sel].member.value, 4);
        assert_eq!(model.selected_value(), Some(4));
    }

    #[test]
    fn filter_visible_only_above_threshold() {
        // 5 members → no filter box.
        assert!(!EnumPickerModel::new(flags(), 0).filter_visible());
        // 11 members → filter box.
        let many: Vec<Member> = (0..11).map(|i| Member::new(&format!("M{i}"), i)).collect();
        assert!(EnumPickerModel::new(many, 0).filter_visible());
        assert_eq!(FILTER_THRESHOLD, 10);
    }

    #[test]
    fn filter_ranks_by_score_and_populates_positions() {
        let mut model = EnumPickerModel::new(flags(), 0);
        model.apply_filter("Vis");
        assert!(!model.rows().is_empty());
        assert_eq!(model.rows()[0].member.name, "Visible");
        assert!(!model.rows()[0].match_positions.is_empty());
        // First row selected when filtering.
        assert_eq!(model.selected(), Some(0));
    }

    #[test]
    fn filter_no_match_empties() {
        let mut model = EnumPickerModel::new(flags(), 0);
        model.apply_filter("zzz");
        assert_eq!(model.row_count(), 0);
        assert_eq!(model.selected(), None);
        assert_eq!(model.selected_value(), None);
    }

    #[test]
    fn navigation_clamps() {
        let mut model = EnumPickerModel::new(flags(), 0);
        // Current value 0 → first row selected.
        assert_eq!(model.selected(), Some(0));
        model.move_up();
        assert_eq!(model.selected(), Some(0));
        for _ in 0..10 {
            model.move_down();
        }
        assert_eq!(model.selected(), Some(model.row_count() - 1));
    }

    #[test]
    fn page_home_end_navigation() {
        let many: Vec<Member> = (0..20).map(|i| Member::new(&format!("M{i}"), i)).collect();
        let mut model = EnumPickerModel::new(many, 0);
        // Current value 0 → first row.
        assert_eq!(model.selected(), Some(0));
        model.page_down(10);
        assert_eq!(model.selected(), Some(10));
        model.page_down(10);
        // Clamps to the last row.
        assert_eq!(model.selected(), Some(model.row_count() - 1));
        model.page_up(5);
        assert_eq!(model.selected(), Some(model.row_count() - 1 - 5));
        model.move_home();
        assert_eq!(model.selected(), Some(0));
        model.move_end();
        assert_eq!(model.selected(), Some(model.row_count() - 1));
    }

    #[test]
    fn select_row_returns_value() {
        let mut model = EnumPickerModel::new(flags(), 0);
        // Row order is value-ascending: [0,1,2,4,8]. Row 3 = value 4.
        assert_eq!(model.select_row(3), Some(4));
        assert_eq!(model.selected_value(), Some(4));
        // Out of range → None.
        assert_eq!(model.select_row(99), None);
    }
}
