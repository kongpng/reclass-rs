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

/// The built-in provider actions shown at the top of the chooser (PIC4): the
/// memory-source plugins, each labeled with its plugin filename hint. These map
/// 1:1 onto the C++ provider registry entries; the trailing recent/saved sources
/// + the "Clear All" action are appended by [`default_entries`].
pub fn provider_entries() -> Vec<SourceEntry> {
    let mk = |id: &str, name: &str, kind: &str, dll: &str| {
        let mut e = SourceEntry::provider(id, name, kind);
        e.dll_file_name = dll.to_string();
        e
    };
    vec![
        // "File" has no plugin dll hint in the screenshot.
        SourceEntry::provider("file", "File", "File"),
        mk(
            "kernelmemory",
            "Kernel Memory",
            "Kernel",
            "libKernelMemoryPlugin.dll",
        ),
        mk(
            "processmemory",
            "Process Memory",
            "Process",
            "libProcessMemoryPlugin.dll",
        ),
        mk(
            "rcnetcompat",
            "ReClass.NET Compat Layer",
            "Compat",
            "libRcNetCompatPlugin.dll",
        ),
        mk(
            "remoteprocessmemory",
            "Remote Process Memory",
            "Remote",
            "libRemoteProcessMemoryPlugin.dll",
        ),
        mk(
            "windbgmemory",
            "WinDbg Memory",
            "WinDbg",
            "libWinDbgMemoryPlugin.dll",
        ),
    ]
}

/// The full default chooser content (PIC4): the provider list, a separator, the
/// `recent` saved sources (with the active one flagged), a separator, and the
/// "Clear All" action. `recent` are `(name, kind_label, active)` tuples in
/// most-recent-first order.
pub fn default_entries(recent: &[(String, String, bool)]) -> Vec<SourceEntry> {
    let mut entries = provider_entries();
    if !recent.is_empty() {
        entries.push(SourceEntry::section("Recent"));
        for (i, (name, kind, active)) in recent.iter().enumerate() {
            let mut e = SourceEntry::saved(i as i32, name, kind);
            e.is_active = *active;
            entries.push(e);
        }
    }
    entries.push(SourceEntry::section("Actions"));
    let mut clear = SourceEntry::clear_action();
    clear.display_name = "Clear All".to_string();
    entries.push(clear);
    entries
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{SourceChooserEvent, SourceChooserPopup, SourcePick};

/// The leading SVG icon for a source row, chosen from the kind label / provider
/// identifier (the C++ paints a per-provider icon). Maps each provider family to a
/// verified [`IconName`](gpui_component::IconName) so the dropdown shows real icons
/// (the round-2 unicode glyphs are replaced). Falls back to a generic data-source
/// drive icon.
#[cfg(feature = "ui")]
pub fn source_icon(kind_label: &str, provider_identifier: &str) -> gpui_component::Icon {
    use gpui_component::{Icon, IconName};
    let key = if !provider_identifier.is_empty() {
        provider_identifier
    } else {
        kind_label
    };
    let lower = key.to_ascii_lowercase();
    let name = if lower.contains("kernel") {
        IconName::Cpu // kernel memory
    } else if lower.contains("remote") {
        IconName::Globe // remote process
    } else if lower.contains("windbg") || lower.contains("dbg") {
        IconName::SquareTerminal // debugger
    } else if lower.contains("net") || lower.contains("compat") || lower.contains("rcnet") {
        IconName::Network // .NET compat layer
    } else if lower.contains("process") || lower.contains("processmemory") {
        IconName::LayoutDashboard // process memory
    } else if lower.contains("file") {
        IconName::File // file
    } else {
        IconName::HardDrive // generic data source
    };
    Icon::new(name)
}

#[cfg(feature = "ui")]
mod view {
    use super::{default_entries, source_icon, SourceAccept, SourceEntryKind, SourceModel};
    use crate::ui::design::{color, icon, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::ActiveTheme as _;

    /// What the user picked in the chooser — the descriptor the editor applies
    /// through the controller/document data-source API (the C++
    /// `sourceSelected`/`providerSelected`/`clearRequested` signals collapsed to
    /// the menus↔editor CONTRACT shape).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum SourcePick {
        /// Switch to a saved source by its `saved_index` (`sourceSelected`).
        SavedSource(i32),
        /// Activate a provider by its identifier (`providerSelected`). The
        /// `process`/`processmemory` identifier is the editor's cue to open the
        /// ProcessPicker (defect 1); other identifiers attach that provider.
        Provider(String),
    }

    /// The popup's outcome (the menus↔editor CONTRACT: the editor opens the chooser
    /// under the source chip and applies the pick through the controller/document
    /// data-source API).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum SourceChooserEvent {
        /// A saved source or provider was chosen — apply it as the document's
        /// data source.
        Pick(SourcePick),
        /// The "File" provider was chosen — the editor opens the file-open flow
        /// (`providerSelected("file")` shortcut for the common case).
        OpenFile,
        /// Clear the data source (`clearRequested`).
        Clear,
        /// Dismissed (Esc / clicked outside / an already-active source).
        Cancel,
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
            let mut popup = SourceChooserPopup {
                model,
                input,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            };
            popup.spawn_liveness_probe(window, cx);
            popup
        }

        /// The CONTRACT entry point: build the chooser over the default content
        /// (provider actions + the given `recent` saved sources) as an [`Entity`]
        /// the editor opens anchored under the source chip and subscribes to for
        /// [`SourceChooserEvent`]. `recent` are `(name, kind_label, active)` tuples
        /// most-recent-first (see [`default_entries`](super::default_entries)).
        pub fn view(
            recent: Vec<(String, String, bool)>,
            window: &mut Window,
            cx: &mut App,
        ) -> Entity<Self> {
            cx.new(|cx| SourceChooserPopup::new(default_entries(&recent), window, cx))
        }

        /// Read-only access to the model.
        pub fn model(&self) -> &SourceModel {
            &self.model
        }

        /// Apply liveness results to the model (`setLivenessResults`), re-rendering
        /// so stale saved sources gain the "(exited)" styling. Public so a host
        /// that owns the real probe (process aliveness / file existence) can push
        /// fresh results; the default probe below marks file-backed sources by
        /// existence.
        pub fn set_liveness(&mut self, alive: Vec<bool>, cx: &mut Context<Self>) {
            self.model.set_liveness(&alive);
            cx.notify();
        }

        /// Defer a one-shot liveness probe (`SourceModel::set_liveness`): the C++
        /// runs an async liveness check after `popup()` so stale sources get the
        /// "(exited)" badge. We probe each saved source's `file_path` for existence
        /// (the in-scope analogue — process aliveness is the out-of-scope live data
        /// source). Sources without a path are treated as alive (no false stale).
        fn spawn_liveness_probe(&mut self, window: &Window, cx: &mut Context<Self>) {
            // Build (saved_index, file_path) pairs to probe off the entries.
            let probes: Vec<(usize, String)> = self
                .model
                .entries()
                .iter()
                .filter(|e| e.entry_kind == SourceEntryKind::SavedSource && e.saved_index >= 0)
                .map(|e| (e.saved_index as usize, e.file_path.clone()))
                .collect();
            if probes.is_empty() {
                return;
            }
            let max_idx = probes.iter().map(|(i, _)| *i).max().unwrap_or(0);
            cx.defer_in(window, move |this, _window, cx| {
                let mut alive = vec![true; max_idx + 1];
                for (idx, path) in &probes {
                    // A non-empty path that does not exist on disk → stale; an empty
                    // path (process/live source) stays alive (no file to probe).
                    if !path.is_empty() && !std::path::Path::new(path).exists() {
                        alive[*idx] = false;
                    }
                }
                this.model.set_liveness(&alive);
                cx.notify();
            });
        }

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            match self.model.accept(row) {
                SourceAccept::SavedSource(i) => {
                    cx.emit(SourceChooserEvent::Pick(SourcePick::SavedSource(i)))
                }
                SourceAccept::Provider(id) => {
                    // "File" is the common file-open flow; route it as OpenFile so
                    // the editor can open the file picker directly.
                    if id == "file" {
                        cx.emit(SourceChooserEvent::OpenFile);
                    } else {
                        cx.emit(SourceChooserEvent::Pick(SourcePick::Provider(id)));
                    }
                }
                SourceAccept::Clear => cx.emit(SourceChooserEvent::Clear),
                SourceAccept::None => cx.emit(SourceChooserEvent::Cancel),
            }
        }

        /// Keyboard navigation (`sourcechooserpopup.cpp:603` `eventFilter`):
        /// Up/Down move the selection (skipping section headers), Enter accepts the
        /// selected row, Esc cancels. Down from the (focused) filter traverses into
        /// the list; Up off the top returns focus to the filter. Returns `true`
        /// when handled.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            match key {
                "down" => {
                    // Down-from-filter-into-list: if nothing is selected yet, land
                    // on the first selectable row; else advance.
                    if self.model.selected().is_none() {
                        self.model.move_down();
                        // move_down seeds the first selectable row when none set.
                    } else {
                        self.model.move_down();
                    }
                    cx.notify();
                    true
                }
                "up" => {
                    self.model.move_up();
                    cx.notify();
                    true
                }
                "enter" => {
                    match self.model.accept_current() {
                        SourceAccept::SavedSource(i) => {
                            cx.emit(SourceChooserEvent::Pick(SourcePick::SavedSource(i)))
                        }
                        SourceAccept::Provider(id) => {
                            if id == "file" {
                                cx.emit(SourceChooserEvent::OpenFile);
                            } else {
                                cx.emit(SourceChooserEvent::Pick(SourcePick::Provider(id)));
                            }
                        }
                        SourceAccept::Clear => cx.emit(SourceChooserEvent::Clear),
                        SourceAccept::None => cx.emit(SourceChooserEvent::Cancel),
                    }
                    true
                }
                "escape" => {
                    cx.emit(SourceChooserEvent::Cancel);
                    true
                }
                _ => false,
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
            let selected = self.model.selected();
            let fg = color::text(cx);
            let muted = color::text_muted(cx);
            let disabled = color::text_disabled(cx);
            let accent = color::accent(cx);
            let danger = cx.theme().danger_foreground;
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);
            // Show the filter only when the list is long enough to warrant it
            // (PIC4 is a plain dropdown; a long saved-source list gets a filter).
            let show_filter = self.model.entries().len() > 8;

            let rows: Vec<AnyElement> = self
                .model
                .rows()
                .iter()
                .enumerate()
                .map(|(row, r)| {
                    let e = &r.entry;
                    match e.entry_kind {
                        SourceEntryKind::SectionHeader => div()
                            .h(px(tokens::border::THIN))
                            .my(px(tokens::space::XS))
                            .mx(px(tokens::space::SM))
                            .bg(border)
                            .into_any_element(),
                        _ => {
                            let is_sel = selected == Some(row);
                            let is_clear = e.entry_kind == SourceEntryKind::ClearAction;
                            let row_fg = if !e.enabled {
                                disabled
                            } else if is_clear {
                                danger
                            } else {
                                fg
                            };
                            // The plugin filename hint (PIC4: "(libKernelMemoryPlugin.dll)")
                            // or, for saved sources, the kind + stale note.
                            let hint = if !e.dll_file_name.is_empty() {
                                format!("({})", e.dll_file_name)
                            } else if e.is_stale {
                                format!("{}  · (exited)", e.kind_label)
                            } else if e.entry_kind == SourceEntryKind::SavedSource {
                                e.kind_label.clone()
                            } else {
                                String::new()
                            };

                            gpui_component::h_flex()
                                .id(("source-row", row))
                                .w_full()
                                .h(px(26.))
                                .px(px(tokens::space::SM))
                                .gap(px(tokens::space::MD))
                                .items_center()
                                .rounded(px(tokens::radius::MD))
                                .when(is_sel, |d| d.bg(sel_bg))
                                .when(!is_sel && e.enabled, |d| d.hover(|s| s.bg(hover_bg)))
                                .when(e.enabled, |d| d.cursor_pointer())
                                .when(e.enabled, |d| {
                                    d.on_click(cx.listener(move |this, _e, _w, cx| {
                                        this.accept_row(row, cx);
                                    }))
                                })
                                // Left checkmark slot (active saved source gets a ✓ SVG).
                                .child(
                                    div()
                                        .w(px(14.))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .text_color(accent)
                                        .when(e.is_active, |d| d.child(icon::check().size_3())),
                                )
                                // Leading kind icon (SVG): per-provider for sources,
                                // a ✕ for the clear action.
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .w(px(16.))
                                        .text_color(if is_clear { danger } else { muted })
                                        .child(if is_clear {
                                            icon::close().size_3()
                                        } else {
                                            source_icon(&e.kind_label, &e.provider_identifier)
                                                .size_3()
                                        }),
                                )
                                // Name (+ inline plugin hint).
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_color(row_fg)
                                        .when(e.is_active, |d| d.font_weight(FontWeight::SEMIBOLD))
                                        .child(e.display_name.clone()),
                                )
                                .when(!hint.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(tokens::font::UI_SM))
                                            .text_color(muted)
                                            .child(hint),
                                    )
                                })
                                .into_any_element()
                        }
                    }
                })
                .collect();

            gpui_component::v_flex()
                .id("rcx-source-chooser")
                .track_focus(&self.focus_handle)
                .key_context("RcxSourceChooser")
                // Capture-phase key handling so Up/Down/Enter/Esc drive the list
                // even when the filter input owns focus (the C++ `eventFilter`
                // forwarding from the line-edit to the list, incl. Down-into-list).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_nav_key(ev.keystroke.key.as_str(), cx) {
                        cx.stop_propagation();
                    }
                }))
                .min_w(px(360.))
                .max_h(px(520.))
                .p(px(tokens::space::XS))
                .bg(color::elevated_bg(cx))
                .border_1()
                .border_color(border)
                .rounded(px(tokens::radius::LG))
                .shadow_md()
                .text_size(px(tokens::font::UI_MD))
                .when(show_filter, |this| {
                    this.child(
                        gpui_component::h_flex()
                            .px(px(tokens::space::SM))
                            .pb(px(tokens::space::XS))
                            .gap(px(tokens::space::SM))
                            .items_center()
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
    use super::{default_entries, provider_entries, SourceEntryKind};

    #[test]
    fn provider_entries_match_pic4_list() {
        let ps = provider_entries();
        let names: Vec<&str> = ps.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "File",
                "Kernel Memory",
                "Process Memory",
                "ReClass.NET Compat Layer",
                "Remote Process Memory",
                "WinDbg Memory",
            ]
        );
        // The plugins carry their dll filename hint; "File" does not.
        let kernel = ps
            .iter()
            .find(|e| e.display_name == "Kernel Memory")
            .unwrap();
        assert_eq!(kernel.dll_file_name, "libKernelMemoryPlugin.dll");
        let file = ps.iter().find(|e| e.display_name == "File").unwrap();
        assert!(file.dll_file_name.is_empty());
        // All providers are provider actions.
        assert!(ps
            .iter()
            .all(|e| e.entry_kind == SourceEntryKind::ProviderAction));
    }

    #[test]
    fn default_entries_have_separators_recents_and_clear() {
        let recent = vec![
            ("Reclass.exe".to_string(), "Process".to_string(), false),
            ("Reclass.exe".to_string(), "File".to_string(), true),
        ];
        let es = default_entries(&recent);
        // Two section separators (Recent + Actions).
        let sections = es
            .iter()
            .filter(|e| e.entry_kind == SourceEntryKind::SectionHeader)
            .count();
        assert_eq!(sections, 2);
        // The active recent source is flagged.
        assert!(es
            .iter()
            .any(|e| e.is_active && e.display_name == "Reclass.exe"));
        // "Clear All" is the last entry.
        let last = es.last().unwrap();
        assert_eq!(last.entry_kind, SourceEntryKind::ClearAction);
        assert_eq!(last.display_name, "Clear All");
    }

    #[test]
    fn default_entries_without_recents_skip_recent_section() {
        let es = default_entries(&[]);
        // Only the "Actions" separator, no "Recent".
        assert_eq!(
            es.iter()
                .filter(|e| e.entry_kind == SourceEntryKind::SectionHeader)
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod model_tests {
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

    #[test]
    fn down_from_unfiltered_seeds_first_selectable_row() {
        // Defect 3: Down from the (focused) filter into the list. With no filter
        // there is no selection; Down must land on the first selectable row
        // (skipping the leading section header), not stay unselected.
        let mut model = SourceModel::new(entries());
        assert_eq!(model.selected(), None);
        model.move_down();
        let sel = model.selected().expect("Down seeds a selection");
        assert!(model.rows()[sel].entry.selectable());
        // The first row is a "Saved" section header → selection skips past it.
        assert_ne!(
            model.rows()[sel].entry.entry_kind,
            super::SourceEntryKind::SectionHeader
        );
    }
}
