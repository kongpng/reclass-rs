//! Source-chooser popup — switch between saved sources + provider actions
//! (`sourcechooserpopup.{h,cpp}`, widgets-dialogs.md §11).
//!
//! Port of `SourceChooserPopup`: a popover of two-line cards (saved sources +
//! provider actions + section headers + a clear action) with a fuzzy filter over
//! a composed searchable string, liveness/stale tracking, and an accept rule that
//! ignores the already-active source. Providers themselves are out-of-scope stubs
//! in this port, but the popup UI is in scope (entries reference provider
//! identifiers only). Per the cookbook (ARCHITECTURE §5) it maps onto a `Popover`
//! + a `List` with a custom `render_item`. This ports the pure model + filter
//! (unit-tested) + a popover view.
//!
//! Filter uses the recursive [`source_score`](super::fuzzy::source_score) (the
//! C++ source chooser's own scorer); the searchable string is
//! `name [+ kind] [+ pid] [+ dll] [+ path]` (`applyFilter`).
//!
//! Gated behind the `ui` feature for the view; the model is always built/tested.

/// The kind of a source-chooser entry (`SourceEntry::entryKind`,
/// `sourcechooserpopup.h:46`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceEntryKind {
    /// A saved source (has a `saved_index`).
    SavedSource,
    /// A provider action (process/remote/file/… — references a provider id).
    ProviderAction,
    /// A non-interactive section header.
    SectionHeader,
    /// The "clear sources" action.
    ClearAction,
}

/// A source-chooser entry (`struct SourceEntry`, condensed to the fields the
/// model + filter use; the cosmetic icon/badge fields are omitted).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceEntry {
    pub entry_kind: SourceEntryKind,
    /// The display name (saved source name, provider action label, or header).
    pub display_name: String,
    /// "Process"/"Remote"/"File"/… (`kindLabelFor`).
    pub kind_label: String,
    /// The provider identifier (for `ProviderAction`); empty otherwise.
    pub provider_identifier: String,
    /// Process id, if applicable (0 = none).
    pub pid: u32,
    /// The DLL/module file name, if applicable.
    pub dll_file_name: String,
    /// The file path, if applicable.
    pub file_path: String,
    /// Index into the controller's saved sources (-1 for non-saved).
    pub saved_index: i32,
    /// Whether this saved source is the currently active one.
    pub is_active: bool,
    /// Whether the source is stale (process exited / file missing).
    pub is_stale: bool,
    /// false → grayed + unselectable.
    pub enabled: bool,
}

impl Default for SourceEntry {
    fn default() -> Self {
        SourceEntry {
            entry_kind: SourceEntryKind::SavedSource,
            display_name: String::new(),
            kind_label: String::new(),
            provider_identifier: String::new(),
            pid: 0,
            dll_file_name: String::new(),
            file_path: String::new(),
            saved_index: -1,
            is_active: false,
            is_stale: false,
            enabled: true,
        }
    }
}

impl SourceEntry {
    /// A section header.
    pub fn section(label: &str) -> Self {
        SourceEntry {
            entry_kind: SourceEntryKind::SectionHeader,
            display_name: label.to_string(),
            enabled: false,
            ..Default::default()
        }
    }

    /// A saved source.
    pub fn saved(index: i32, name: &str, kind_label: &str) -> Self {
        SourceEntry {
            entry_kind: SourceEntryKind::SavedSource,
            display_name: name.to_string(),
            kind_label: kind_label.to_string(),
            saved_index: index,
            ..Default::default()
        }
    }

    /// A provider action.
    pub fn provider(identifier: &str, name: &str, kind_label: &str) -> Self {
        SourceEntry {
            entry_kind: SourceEntryKind::ProviderAction,
            display_name: name.to_string(),
            kind_label: kind_label.to_string(),
            provider_identifier: identifier.to_string(),
            ..Default::default()
        }
    }

    /// The "clear sources" action.
    pub fn clear_action() -> Self {
        SourceEntry {
            entry_kind: SourceEntryKind::ClearAction,
            display_name: "Clear data source".to_string(),
            ..Default::default()
        }
    }

    /// Whether this row can be selected (not a section header, and enabled).
    pub fn selectable(&self) -> bool {
        self.entry_kind != SourceEntryKind::SectionHeader && self.enabled
    }

    /// The composed searchable string (`applyFilter`): `name [+ " " + kind] [+ " "
    /// + pid] [+ " " + dll] [+ " " + path]`. Section headers aren't searched.
    pub fn searchable(&self) -> String {
        let mut s = self.display_name.clone();
        if !self.kind_label.is_empty() {
            s.push(' ');
            s.push_str(&self.kind_label);
        }
        if self.pid != 0 {
            s.push(' ');
            s.push_str(&self.pid.to_string());
        }
        if !self.dll_file_name.is_empty() {
            s.push(' ');
            s.push_str(&self.dll_file_name);
        }
        if !self.file_path.is_empty() {
            s.push(' ');
            s.push_str(&self.file_path);
        }
        s
    }
}

/// What accepting a row produces (`acceptIndex`, `sourcechooserpopup.cpp:681`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SourceAccept {
    /// Switch to a saved source (`sourceSelected(savedIndex)`).
    SavedSource(i32),
    /// Activate a provider (`providerSelected(identifier)`).
    Provider(String),
    /// Clear the data source (`clearRequested`).
    Clear,
    /// No-op (an already-active saved source just hides; a header/disabled row).
    None,
}

/// A rendered source row: the entry + fuzzy match positions (for highlight).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceRow {
    pub entry: SourceEntry,
    pub match_positions: Vec<usize>,
}

/// The source-chooser list model + filter (`applyFilter`,
/// `sourcechooserpopup.cpp:480`).
#[derive(Clone, Debug, Default)]
pub struct SourceModel {
    entries: Vec<SourceEntry>,
    rows: Vec<SourceRow>,
    selected: Option<usize>,
    filtering: bool,
}

impl SourceModel {
    /// Build a model over the entries (unfiltered, all shown).
    pub fn new(entries: Vec<SourceEntry>) -> Self {
        let mut m = SourceModel {
            entries,
            rows: Vec::new(),
            selected: None,
            filtering: false,
        };
        m.apply_filter("");
        m
    }

    /// All entries.
    pub fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    /// The rendered rows after filtering.
    pub fn rows(&self) -> &[SourceRow] {
        &self.rows
    }

    /// The number of rendered rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// The selected row index (only set while filtering — the C++ pre-selects
    /// only when filtering, else the first row would look permanently selected).
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Update staleness for a saved source by its `saved_index` (the C++
    /// `setLivenessResults`): stale = `!alive`. Re-filters if anything changed.
    pub fn set_liveness(&mut self, alive: &[bool]) {
        let mut changed = false;
        for e in &mut self.entries {
            if e.entry_kind == SourceEntryKind::SavedSource && e.saved_index >= 0 {
                let idx = e.saved_index as usize;
                if idx < alive.len() {
                    let stale = !alive[idx];
                    if e.is_stale != stale {
                        e.is_stale = stale;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            // Re-filter to refresh the rendered rows' stale flags. Liveness does
            // not change the filter text, so re-apply the empty filter (the rows
            // re-clone the updated entries); when actively filtering the host
            // re-pushes the live query on the next keystroke.
            self.apply_filter("");
        }
    }

    /// Re-filter against `filter` (`applyFilter`). Empty → show all (no
    /// selection). Else, for each non-section entry build the searchable string,
    /// [`source_score`](super::fuzzy::source_score) it, keep `>0`, **sort by score
    /// desc**, and pre-select row 0.
    pub fn apply_filter(&mut self, filter: &str) {
        let trimmed = filter.trim();
        self.filtering = !trimmed.is_empty();
        self.rows.clear();

        if !self.filtering {
            // Show all entries (sections included).
            self.rows = self
                .entries
                .iter()
                .cloned()
                .map(|entry| SourceRow {
                    entry,
                    match_positions: Vec::new(),
                })
                .collect();
            // No selection when not filtering (the C++ clears the current index).
            self.selected = None;
            return;
        }

        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if e.entry_kind == SourceEntryKind::SectionHeader {
                continue;
            }
            let mut pos = Vec::new();
            let s = super::fuzzy::source_score(trimmed, &e.searchable(), Some(&mut pos));
            if s > 0 {
                scored.push((s, i, pos));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.rows = scored
            .into_iter()
            .map(|(_, i, pos)| SourceRow {
                entry: self.entries[i].clone(),
                match_positions: pos,
            })
            .collect();
        // Pre-select the first selectable row while filtering.
        self.selected = self.rows.iter().position(|r| r.entry.selectable());
    }

    /// `nextSelectableRow(from, dir)` — skip section headers + disabled.
    fn next_selectable(&self, from: usize, dir: i32) -> Option<usize> {
        let len = self.rows.len();
        if len == 0 {
            return None;
        }
        let mut i = from as i32 + dir;
        while i >= 0 && (i as usize) < len {
            if self.rows[i as usize].entry.selectable() {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    /// Move selection down to the next selectable row.
    pub fn move_down(&mut self) {
        let from = self.selected.unwrap_or(0);
        if let Some(next) = self.next_selectable(from, 1) {
            self.selected = Some(next);
        } else if self.selected.is_none() {
            self.selected = self.rows.iter().position(|r| r.entry.selectable());
        }
    }

    /// Move selection up to the previous selectable row.
    pub fn move_up(&mut self) {
        if let Some(from) = self.selected {
            if let Some(prev) = self.next_selectable(from, -1) {
                self.selected = Some(prev);
            }
        }
    }

    /// Accept a row (`acceptIndex`): reject disabled/section; an **active** saved
    /// source just hides (`None`); else map to the corresponding action.
    pub fn accept(&self, row: usize) -> SourceAccept {
        let Some(r) = self.rows.get(row) else {
            return SourceAccept::None;
        };
        let e = &r.entry;
        if !e.selectable() {
            return SourceAccept::None;
        }
        match e.entry_kind {
            SourceEntryKind::ClearAction => SourceAccept::Clear,
            SourceEntryKind::ProviderAction => {
                SourceAccept::Provider(e.provider_identifier.clone())
            }
            SourceEntryKind::SavedSource => {
                if e.is_active {
                    // Already active → just hide, no signal.
                    SourceAccept::None
                } else if e.saved_index >= 0 {
                    SourceAccept::SavedSource(e.saved_index)
                } else {
                    SourceAccept::None
                }
            }
            SourceEntryKind::SectionHeader => SourceAccept::None,
        }
    }

    /// Accept the currently selected row.
    pub fn accept_current(&self) -> SourceAccept {
        match self.selected {
            Some(row) => self.accept(row),
            None => SourceAccept::None,
        }
    }
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{SourceChooserEvent, SourceChooserPopup};

#[cfg(feature = "ui")]
mod view {
    use super::{SourceAccept, SourceEntryKind, SourceModel};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::{ActiveTheme, Selectable as _};

    /// The popup's outcome.
    #[derive(Clone, Debug)]
    pub enum SourceChooserEvent {
        /// Switch to a saved source (`sourceSelected`).
        SourceSelected(i32),
        /// Activate a provider (`providerSelected`).
        ProviderSelected(String),
        /// Clear the data source (`clearRequested`).
        ClearRequested,
        /// Dismissed.
        Dismissed,
    }

    /// The source-chooser popover view.
    pub struct SourceChooserPopup {
        model: SourceModel,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        _subscription: Subscription,
    }

    impl SourceChooserPopup {
        /// Build the popup over the given entries.
        pub fn new(
            entries: Vec<super::SourceEntry>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let model = SourceModel::new(entries);
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter sources..."));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        this.model.apply_filter(&q);
                        cx.notify();
                    }
                });
            SourceChooserPopup {
                model,
                input,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            }
        }

        /// Read-only access to the model.
        pub fn model(&self) -> &SourceModel {
            &self.model
        }

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            match self.model.accept(row) {
                SourceAccept::SavedSource(i) => cx.emit(SourceChooserEvent::SourceSelected(i)),
                SourceAccept::Provider(id) => cx.emit(SourceChooserEvent::ProviderSelected(id)),
                SourceAccept::Clear => cx.emit(SourceChooserEvent::ClearRequested),
                SourceAccept::None => cx.emit(SourceChooserEvent::Dismissed),
            }
        }
    }

    impl Focusable for SourceChooserPopup {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl EventEmitter<SourceChooserEvent> for SourceChooserPopup {}

    impl Render for SourceChooserPopup {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};
            let selected = self.model.selected();
            let rows: Vec<_> = self
                .model
                .rows()
                .iter()
                .enumerate()
                .map(|(row, r)| {
                    if r.entry.entry_kind == SourceEntryKind::SectionHeader {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(r.entry.display_name.to_uppercase())
                            .into_any_element()
                    } else {
                        let is_sel = selected == Some(row);
                        let mut subline = r.entry.kind_label.clone();
                        if r.entry.is_active {
                            subline.push_str("  · active");
                        }
                        if r.entry.is_stale {
                            subline.push_str("  · (exited)");
                        }
                        Button::new(("source-row", row))
                            .ghost()
                            .w_full()
                            .selected(is_sel)
                            .when(!r.entry.enabled, |b| {
                                b.text_color(cx.theme().muted_foreground)
                            })
                            .child(
                                gpui_component::v_flex()
                                    .w_full()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .when(r.entry.is_active, |d| {
                                                d.font_weight(FontWeight::BOLD)
                                            })
                                            .text_color(cx.theme().foreground)
                                            .child(r.entry.display_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(subline),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _e, _window, cx| {
                                this.accept_row(row, cx);
                            }))
                            .into_any_element()
                    }
                })
                .collect();

            gpui_component::v_flex()
                .id("rcx-source-chooser")
                .track_focus(&self.focus_handle)
                .key_context("RcxSourceChooser")
                .min_w(px(360.))
                .max_h(px(520.))
                .bg(cx.theme().popover)
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .text_color(cx.theme().foreground)
                        .child("Data Source"),
                )
                .child(Input::new(&self.input).w_full())
                .child(
                    gpui_component::v_flex()
                        .id("rcx-source-chooser-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_hidden()
                        .children(rows),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceAccept, SourceEntry, SourceModel};

    fn entries() -> Vec<SourceEntry> {
        vec![
            SourceEntry::section("Saved"),
            {
                let mut e = SourceEntry::saved(0, "notepad.exe", "Process");
                e.pid = 1234;
                e
            },
            {
                let mut e = SourceEntry::saved(1, "game.bin", "File");
                e.file_path = "/tmp/game.bin".to_string();
                e
            },
            SourceEntry::section("Providers"),
            SourceEntry::provider("processmemory", "Attach to Process…", "Process"),
            SourceEntry::clear_action(),
        ]
    }

    #[test]
    fn empty_filter_shows_all_no_selection() {
        let model = SourceModel::new(entries());
        // All 6 rows (2 sections + 2 saved + 1 provider + 1 clear).
        assert_eq!(model.row_count(), 6);
        // No selection when not filtering.
        assert_eq!(model.selected(), None);
    }

    #[test]
    fn searchable_string_includes_pid_and_path() {
        let model = SourceModel::new(entries());
        let saved = model
            .entries()
            .iter()
            .find(|e| e.display_name == "notepad.exe")
            .unwrap();
        let s = saved.searchable();
        assert!(s.contains("notepad.exe"));
        assert!(s.contains("Process"));
        assert!(s.contains("1234"));
    }

    #[test]
    fn filter_ranks_and_excludes_sections() {
        let mut model = SourceModel::new(entries());
        model.apply_filter("notepad");
        // Only the matching saved source; sections never appear when filtering.
        assert!(model
            .rows()
            .iter()
            .all(|r| r.entry.entry_kind != super::SourceEntryKind::SectionHeader));
        assert_eq!(model.rows()[0].entry.display_name, "notepad.exe");
        assert!(!model.rows()[0].match_positions.is_empty());
        // Pre-selected while filtering.
        assert_eq!(model.selected(), Some(0));
    }

    #[test]
    fn filter_can_match_via_pid() {
        let mut model = SourceModel::new(entries());
        model.apply_filter("1234");
        assert!(!model.rows().is_empty());
        assert_eq!(model.rows()[0].entry.display_name, "notepad.exe");
    }

    #[test]
    fn accept_saved_source_returns_index() {
        let mut model = SourceModel::new(entries());
        model.apply_filter("game");
        assert_eq!(model.accept_current(), SourceAccept::SavedSource(1));
    }

    #[test]
    fn accept_active_source_is_noop() {
        let mut es = entries();
        // Mark the first saved source active.
        for e in &mut es {
            if e.display_name == "notepad.exe" {
                e.is_active = true;
            }
        }
        let mut model = SourceModel::new(es);
        model.apply_filter("notepad");
        // An active saved source just hides (no signal).
        assert_eq!(model.accept_current(), SourceAccept::None);
    }

    #[test]
    fn accept_provider_and_clear() {
        let mut model = SourceModel::new(entries());
        model.apply_filter("Attach");
        assert_eq!(
            model.accept_current(),
            SourceAccept::Provider("processmemory".to_string())
        );

        let mut model = SourceModel::new(entries());
        model.apply_filter("Clear");
        assert_eq!(model.accept_current(), SourceAccept::Clear);
    }

    #[test]
    fn liveness_marks_stale() {
        let mut model = SourceModel::new(entries());
        // saved_index 0 alive, 1 dead.
        model.set_liveness(&[true, false]);
        let dead = model.entries().iter().find(|e| e.saved_index == 1).unwrap();
        assert!(dead.is_stale);
        let alive = model.entries().iter().find(|e| e.saved_index == 0).unwrap();
        assert!(!alive.is_stale);
    }

    #[test]
    fn navigation_skips_sections() {
        let mut model = SourceModel::new(entries());
        model.apply_filter("e"); // broad match
        let first = model.selected().unwrap();
        assert!(model.rows()[first].entry.selectable());
        for _ in 0..model.row_count() {
            model.move_down();
            if let Some(sel) = model.selected() {
                assert!(model.rows()[sel].entry.selectable());
            }
        }
    }
}
