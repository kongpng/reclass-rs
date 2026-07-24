//! Source-chooser popup — switch between saved sources + provider actions
//! (`sourcechooserpopup.{h,cpp}`, widgets-dialogs.md §11).
//!
//! Port of `SourceChooserPopup`: a popover of two-line cards (saved sources +
//! provider actions + section headers + a clear action) with a fuzzy filter over
//! a composed searchable string, liveness/stale tracking, and an accept rule that
//! ignores the already-active source. The file / buffer / snapshot / null providers
//! are always real; the feature/platform-enabled live providers register through
//! the same provider list.
//! The popup UI references provider identifiers only. Per the cookbook
//! (ARCHITECTURE §5) it maps onto a `Popover` + a `List` with a custom
//! `render_item`. This ports the pure model + filter (unit-tested) + a popover view.
//!
//! Filter uses the recursive [`source_score`](crate::ui::fuzzy::source_score) (the
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
            display_name: "Clear All".to_string(),
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
    /// [`source_score`](crate::ui::fuzzy::source_score) it, keep `>0`, **sort by score
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
            let s = crate::ui::fuzzy::source_score(trimmed, &e.searchable(), Some(&mut pos));
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

    /// The selectability mask over `rows`, for [`crate::ui::navlist`] navigation.
    fn selectable_mask(&self) -> Vec<bool> {
        self.rows.iter().map(|r| r.entry.selectable()).collect()
    }

    /// Move selection down to the next selectable row.
    pub fn move_down(&mut self) {
        self.selected = crate::ui::navlist::step(&self.selectable_mask(), self.selected, 1);
    }

    /// Move selection up to the previous selectable row.
    pub fn move_up(&mut self) {
        self.selected = crate::ui::navlist::step(&self.selectable_mask(), self.selected, -1);
    }

    /// Set the selection to `row` if it is selectable (a hover / click preview).
    /// Returns whether the selection changed.
    pub fn select_at(&mut self, row: usize) -> bool {
        if self
            .rows
            .get(row)
            .map(|r| r.entry.selectable())
            .unwrap_or(false)
        {
            self.selected = Some(row);
            true
        } else {
            false
        }
    }

    /// Move selection down by `page` selectable rows (PageDown).
    pub fn page_down(&mut self, page: usize) {
        self.selected = crate::ui::navlist::page(&self.selectable_mask(), self.selected, 1, page);
    }

    /// Move selection up by `page` selectable rows (PageUp).
    pub fn page_up(&mut self, page: usize) {
        self.selected = crate::ui::navlist::page(&self.selectable_mask(), self.selected, -1, page);
    }

    /// Select the first selectable row (Home).
    pub fn move_home(&mut self) {
        self.selected = crate::ui::navlist::first_selectable(&self.selectable_mask());
    }

    /// Select the last selectable row (End).
    pub fn move_end(&mut self) {
        self.selected = crate::ui::navlist::last_selectable(&self.selectable_mask());
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

    /// Remove one saved source without dismissing the chooser. Remaining saved
    /// indices are compacted to stay aligned with the controller's vector.
    pub fn remove_saved_source(&mut self, saved_index: i32, filter: &str) -> bool {
        if saved_index < 0 {
            return false;
        }
        let Some(entry_index) = self.entries.iter().position(|entry| {
            entry.entry_kind == SourceEntryKind::SavedSource && entry.saved_index == saved_index
        }) else {
            return false;
        };
        self.entries.remove(entry_index);
        for entry in &mut self.entries {
            if entry.entry_kind == SourceEntryKind::SavedSource && entry.saved_index > saved_index {
                entry.saved_index -= 1;
            }
        }
        let has_saved = self
            .entries
            .iter()
            .any(|entry| entry.entry_kind == SourceEntryKind::SavedSource);
        if !has_saved {
            // Upstream only shows the Connected section when at least one bound
            // source exists. Removing the last row must remove its now-orphaned
            // header in-place (the popup stays open after a per-row delete).
            self.entries.retain(|entry| {
                !(entry.entry_kind == SourceEntryKind::SectionHeader
                    && entry.display_name == "Connected")
            });
        }
        if let Some(clear) = self
            .entries
            .iter_mut()
            .find(|entry| entry.entry_kind == SourceEntryKind::ClearAction)
        {
            clear.enabled = has_saved;
        }
        self.apply_filter(filter);
        true
    }
}

/// The kind label for a provider, keyed off its identifier (the C++ `kindLabelFor`,
/// `sourcechooserpopup.h:19-42`). Centralized so both source surfaces agree
/// (design §7.A [fix] — the C++ had two divergent tables). Falls back to "Source".
pub fn kind_label_for(identifier: &str) -> &'static str {
    match identifier {
        "file" => "File",
        "buffer" => "Buffer",
        "snapshot" => "Snapshot",
        "null" => "Null",
        "kernelmemory" => "Kernel",
        "processmemory" => "Process",
        "memflowprocessmemory" => "Memflow",
        "remoteprocessmemory" => "Remote",
        "windbgmemory" => "WinDbg",
        "reclass.netcompatlayer" | "rcnetcompat" => "Compat",
        _ => "Source",
    }
}

/// Build the provider-action entries from a [`ProviderRegistry`](crate::provider::ProviderRegistry)
/// — the SINGLE shared model both source-picker surfaces consume (design §7.A
/// [fix]: the C++ had two icon/label tables that already diverged). Each enabled
/// registry provider becomes a [`SourceEntry::provider`] row, labeled via
/// [`kind_label_for`] and carrying its `dll_file_name` hint; the trailing
/// recent/saved sources + "Clear All" are appended by [`default_entries`].
pub fn provider_entries_from_registry(
    registry: &crate::provider::ProviderRegistry,
) -> Vec<SourceEntry> {
    registry
        .enabled_providers()
        .map(|p| {
            let mut e =
                SourceEntry::provider(&p.identifier, &p.name, kind_label_for(&p.identifier));
            e.dll_file_name = p.dll_file_name.clone();
            e
        })
        .collect()
}

/// The default source families the picker shows when no live registry is threaded
/// through. It is derived from registered built-ins only; platform-specific
/// providers appear only when they are actually registered.
pub fn provider_entries() -> Vec<SourceEntry> {
    let builtins = crate::plugin::PluginManager::with_builtins();
    let mut reg = crate::provider::ProviderRegistry::new();
    for info in builtins.registry().providers().iter().cloned() {
        reg.register_provider(info);
    }
    provider_entries_from_registry(&reg)
}

/// The full upstream chooser content: saved sources under **Connected** first,
/// providers under **Add Source** second, then the standalone **Clear All**
/// action. `recent` are `(name, kind_label, active)` tuples in most-recent-first
/// order. Connected is omitted and Clear All is disabled when there are no saved
/// sources, matching `RcxController::showSourcePopup`.
pub fn default_entries(recent: &[(String, String, bool)]) -> Vec<SourceEntry> {
    let mut entries = Vec::new();
    if !recent.is_empty() {
        entries.push(SourceEntry::section("Connected"));
        for (i, (name, kind, active)) in recent.iter().enumerate() {
            let mut e = SourceEntry::saved(i as i32, name, kind);
            e.is_active = *active;
            entries.push(e);
        }
    }
    entries.push(SourceEntry::section("Add Source"));
    entries.extend(provider_entries());
    let mut clear = SourceEntry::clear_action();
    clear.enabled = !recent.is_empty();
    entries.push(clear);
    entries
}

/// The right-aligned source-chooser footer status count (the C++ `m_footerLabel`
/// count text). While filtering: an empty result echoes the typed query as
/// `No matches for "<query>"` (matching the C++ `No matches for "%1"`,
/// `sourcechooserpopup.cpp:526-528`), else `N of M sources`. When idle (the
/// always-on navigate hint is a separate footer child, item 14): `M sources`.
pub fn footer_status_text(query: &str, shown_sources: usize, total_sources: usize) -> String {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        format!("{total_sources} sources")
    } else if shown_sources == 0 {
        format!("No matches for \"{trimmed}\"")
    } else {
        format!("{shown_sources} of {total_sources} sources")
    }
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
        /// Remove a saved source while leaving the chooser open.
        RemoveSaved(i32),
        /// Dismissed (Esc / clicked outside / an already-active source).
        Cancel,
    }

    /// The source-chooser popover view.
    pub struct SourceChooserPopup {
        model: SourceModel,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        /// Scrolls the list so the keyboard-selected row stays visible (item 9).
        list_scroll: ScrollHandle,
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
                list_scroll: ScrollHandle::new(),
                _subscription: subscription,
            };
            popup.spawn_liveness_probe(window, cx);
            popup
        }

        /// Scroll the selected row into view (item 9). Rows render 1:1 with model
        /// rows, so the model index is the rendered child index.
        fn scroll_selected_into_view(&self) {
            crate::ui::design::scroll_selected(&self.list_scroll, self.model.selected());
        }

        /// Hover over a row moves the selection highlight to it (item 2) so
        /// keyboard + mouse selection stay in sync.
        fn hover_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.selected() != Some(row)
                && self
                    .model
                    .rows()
                    .get(row)
                    .map(|r| r.entry.selectable())
                    .unwrap_or(false)
            {
                self.model.select_at(row);
                cx.notify();
            }
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
        /// as a lightweight liveness signal (full process aliveness is available
        /// through the memflow provider). Sources without a path are treated as
        /// alive (no false stale).
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

        /// Route an accepted source outcome to its event ("File" provider →
        /// OpenFile; saved / provider / clear / none → their picks). Shared by the
        /// click (`accept_row`) and Enter paths so the file-vs-provider special
        /// case can't drift between them.
        fn emit_accept(&mut self, outcome: SourceAccept, cx: &mut Context<Self>) {
            match outcome {
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

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            let outcome = self.model.accept(row);
            self.emit_accept(outcome, cx);
        }

        fn remove_saved(&mut self, saved_index: i32, cx: &mut Context<Self>) {
            let query = self.input.read(cx).value().to_string();
            if self.model.remove_saved_source(saved_index, &query) {
                cx.emit(SourceChooserEvent::RemoveSaved(saved_index));
                cx.notify();
            }
        }

        fn remove_selected_saved(&mut self, cx: &mut Context<Self>) -> bool {
            let Some(saved_index) = self.model.selected().and_then(|row| {
                self.model.rows().get(row).and_then(|row| {
                    (row.entry.entry_kind == SourceEntryKind::SavedSource)
                        .then_some(row.entry.saved_index)
                })
            }) else {
                return false;
            };
            self.remove_saved(saved_index, cx);
            true
        }

        /// Keyboard navigation (`sourcechooserpopup.cpp:603` `eventFilter`):
        /// Up/Down move the selection (skipping section headers), Enter accepts the
        /// selected row, Esc cancels. Down from the (focused) filter traverses into
        /// the list; Up off the top returns focus to the filter. Returns `true`
        /// when handled.
        fn handle_nav_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
            const PAGE: usize = 8;
            match key {
                "down" => {
                    // Down-from-filter-into-list: if nothing is selected yet, land
                    // on the first selectable row; else advance.
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
                    let outcome = self.model.accept_current();
                    self.emit_accept(outcome, cx);
                    true
                }
                "escape" => {
                    cx.emit(SourceChooserEvent::Cancel);
                    true
                }
                "delete" => self.remove_selected_saved(cx),
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
            let danger = color::danger_emphasis(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);
            let query = self.input.read(cx).value().to_string();
            // Footer counts (the C++ `m_statusLabel`, via `footer_status_text`):
            // "N of M sources" while filtering / `No matches for "<query>"` on an
            // empty result / "M sources" otherwise.
            let total_sources = self
                .model
                .entries()
                .iter()
                .filter(|e| e.entry_kind != SourceEntryKind::SectionHeader)
                .count();
            let shown_sources = self
                .model
                .rows()
                .iter()
                .filter(|r| r.entry.entry_kind != SourceEntryKind::SectionHeader)
                .count();

            let rows: Vec<AnyElement> = self
                .model
                .rows()
                .iter()
                .enumerate()
                .map(|(row, r)| {
                    let e = &r.entry;
                    match e.entry_kind {
                        SourceEntryKind::SectionHeader => gpui_component::h_flex()
                            .w_full()
                            .h(px(28.0))
                            .px(px(tokens::space::SM))
                            .items_center()
                            .when(row > 0, |d| d.border_t_1().border_color(border))
                            .text_size(px(tokens::font::UI_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(disabled)
                            .child(e.display_name.to_uppercase())
                            .into_any_element(),
                        _ => {
                            let is_sel = selected == Some(row);
                            let is_clear = e.entry_kind == SourceEntryKind::ClearAction;
                            let saved_index = e.saved_index;
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

                            let row_element = gpui_component::h_flex()
                                .id(("source-row", row))
                                .w_full()
                                // Stable min-height (not a tight fixed height): the
                                // single-line label + trailing dim dll never get
                                // clipped, and long provider names ("ReClass.NET
                                // Compat Layer", "Remote Process Memory") can't wrap
                                // into the next row (defect 1). Matches the C++
                                // data_options single-line provider layout.
                                .min_h(px(28.))
                                .py(px(tokens::space::XS))
                                .px(px(tokens::space::SM))
                                .gap(px(tokens::space::MD))
                                .items_center()
                                .rounded(px(tokens::radius::MD))
                                .when(is_sel, |d| d.bg(sel_bg))
                                .when(!is_sel && e.enabled, |d| d.hover(|s| s.bg(hover_bg)))
                                .when(e.enabled, |d| d.cursor_pointer())
                                // Hover-to-select (item 2): sync keyboard + mouse.
                                .when(e.enabled, |d| {
                                    d.on_mouse_move(cx.listener(move |this, _e, _w, cx| {
                                        this.hover_row(row, cx);
                                    }))
                                })
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
                                // Name — single-line, truncates with an ellipsis so a
                                // long provider label can never wrap onto a second
                                // line and collide with the neighbouring row's dll
                                // text (defect 1).
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(row_fg)
                                        .when(e.is_active, |d| d.font_weight(FontWeight::SEMIBOLD))
                                        .child(e.display_name.clone()),
                                )
                                // PID pill (the C++ row-2 "PID <n>" badge,
                                // `sourcechooserpopup.cpp:253-257`): drawn ONLY when the
                                // entry is a SavedSource AND its pid is non-zero. The C++
                                // guard is `!e.pid.isEmpty()` on a QString pid; the Rust
                                // analogue of "empty" is `pid == 0`, so a non-process /
                                // File source (pid 0) shows no pill (the pid==0 rule).
                                // Rendered as a small dim/muted pill before the existing
                                // dll/kind hint slot (Zed substitution for the Qt rounded
                                // badge); single-line + flex_none so it never wraps.
                                .when(
                                    e.entry_kind == SourceEntryKind::SavedSource && e.pid != 0,
                                    |d| {
                                        d.child(
                                            div()
                                                .flex_none()
                                                .whitespace_nowrap()
                                                .px(px(tokens::space::XS))
                                                .rounded(px(tokens::radius::SM))
                                                .bg(color::hover_overlay(cx))
                                                .text_size(px(tokens::font::UI_SM))
                                                .text_color(color::text_muted(cx))
                                                .child(format!("PID {}", e.pid)),
                                        )
                                    },
                                )
                                // Trailing dim slot: the plugin dll filename (or the
                                // saved-source kind/stale note). Single-line and
                                // non-shrinking so it stays on the same row as the
                                // label (the C++ data_options trailing dll text).
                                .when(!hint.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .whitespace_nowrap()
                                            .text_size(px(tokens::font::UI_SM))
                                            .text_color(muted)
                                            .child(hint),
                                    )
                                })
                                // Faint on row hover; bright only over the x itself.
                                // The child stops propagation so removing a source
                                // neither activates the row nor closes the chooser.
                                .when(e.entry_kind == SourceEntryKind::SavedSource, |d| {
                                    d.child(
                                        div()
                                            .id(("remove-saved-source", saved_index as u64))
                                            .flex_none()
                                            .p(px(tokens::space::XS))
                                            .rounded(px(tokens::radius::SM))
                                            .cursor_pointer()
                                            .text_color(color::with_alpha(muted, 0.42))
                                            .hover(|s| s.bg(hover_bg).text_color(danger))
                                            .on_click(cx.listener(
                                                move |this, _event, _window, cx| {
                                                    cx.stop_propagation();
                                                    this.remove_saved(saved_index, cx);
                                                },
                                            ))
                                            .child(icon::close().size_3()),
                                    )
                                })
                                .into_any_element();

                            if is_clear {
                                // Keep the destructive global action visually
                                // separate from the provider list, matching the
                                // inset rule and extra gap in the upstream row.
                                gpui_component::v_flex()
                                    .w_full()
                                    .pt(px(tokens::space::XS))
                                    .child(
                                        div()
                                            .h(px(tokens::border::THIN))
                                            .mx(px(tokens::space::SM))
                                            .bg(border),
                                    )
                                    .child(row_element)
                                    .into_any_element()
                            } else {
                                row_element
                            }
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
                // No horizontal outer inset: the list separators and footer
                // rule meet the popup's side borders, while their contents own
                // their normal horizontal padding.
                .py(px(tokens::space::XS))
                .bg(color::elevated_bg(cx))
                .border_1()
                .border_color(border)
                .rounded(px(tokens::radius::LG))
                .shadow_md()
                .text_size(px(tokens::font::UI_MD))
                // The fuzzy filter is ALWAYS shown now (item 14) — the C++ source
                // chooser always offers it, not just for long lists.
                .child(
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
                .child(
                    // Scrollable list (item 9): the keyboard-selected row scrolls
                    // into view via `list_scroll.scroll_to_item`.
                    gpui_component::v_flex()
                        .id("rcx-source-chooser-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.list_scroll)
                        .children(rows),
                )
                // Footer chrome (item 14): a "↑↓ navigate · ↵ select · Esc close"
                // hint, the "N of M sources" / "No matches" status, and an Esc
                // button — the C++ source-chooser footer status line.
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .mt(px(tokens::space::XS))
                        .px(px(tokens::space::SM))
                        .py(px(tokens::space::XS))
                        .gap(px(tokens::space::SM))
                        .items_center()
                        .border_t_1()
                        .border_color(border)
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(muted)
                        .child(div().flex_none().child(
                            "\u{2191}\u{2193} navigate \u{00B7} \u{21B5} select \u{00B7} Esc close",
                        ))
                        .child(div().flex_1())
                        .child(div().flex_none().child(super::footer_status_text(
                            &query,
                            shown_sources,
                            total_sources,
                        )))
                        .child(
                            div()
                                .id("source-esc")
                                .flex_none()
                                .px(px(tokens::space::SM))
                                .rounded(px(tokens::radius::SM))
                                .cursor_pointer()
                                .hover(|s| s.bg(hover_bg).text_color(fg))
                                .on_click(cx.listener(|_this, _e, _w, cx| {
                                    cx.emit(SourceChooserEvent::Cancel)
                                }))
                                .child("Esc"),
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        default_entries, footer_status_text, kind_label_for, provider_entries,
        provider_entries_from_registry, SourceEntryKind,
    };

    fn builtin_provider_names() -> Vec<&'static str> {
        let mut names = vec!["File"];
        #[cfg(feature = "process-provider")]
        names.push("Process Memory");
        #[cfg(feature = "remote-process-provider")]
        names.push("Remote Process Memory");
        #[cfg(all(windows, feature = "kernel-provider"))]
        names.push("Kernel Memory");
        #[cfg(all(windows, feature = "windbg-provider"))]
        names.push("WinDbg Memory");
        #[cfg(feature = "memflow-provider")]
        names.push("Memflow Process Memory");
        names.extend(["Buffer", "Snapshot", "Null"]);
        names
    }

    fn provider_entries_pic4_names() -> Vec<&'static str> {
        builtin_provider_names()
    }

    #[test]
    fn footer_status_echoes_query_on_no_matches() {
        // Empty filtered result echoes the typed query, matching the C++ footer
        // `No matches for "%1"` (sourcechooserpopup.cpp:526-528) — not a bare
        // "No matches".
        assert_eq!(footer_status_text("zzz", 0, 6), "No matches for \"zzz\"");
        // The query is trimmed in the echo (the branch is only reached for a
        // non-blank filter).
        assert_eq!(
            footer_status_text("  foo  ", 0, 6),
            "No matches for \"foo\""
        );
        // A non-empty result keeps the "N of M sources" count.
        assert_eq!(footer_status_text("fi", 2, 6), "2 of 6 sources");
        // An idle (blank) filter shows the plain total.
        assert_eq!(footer_status_text("   ", 6, 6), "6 sources");
    }

    #[test]
    fn provider_entries_from_registry_is_the_shared_model() {
        // The in-tree PluginManager registry → the same SourceEntry shape both
        // surfaces consume (design §7.A [fix] — one shared list).
        let mgr = crate::plugin::PluginManager::with_builtins();
        let entries = provider_entries_from_registry(mgr.registry());
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, builtin_provider_names());
        // All are provider actions with a kind label and the derived identifier.
        assert!(entries
            .iter()
            .all(|e| e.entry_kind == SourceEntryKind::ProviderAction));
        let file = entries.iter().find(|e| e.display_name == "File").unwrap();
        assert_eq!(file.provider_identifier, "file");
        assert_eq!(file.kind_label, "File");
    }

    #[test]
    fn provider_entries_from_registry_skips_disabled() {
        let mut mgr = crate::plugin::PluginManager::with_builtins();
        mgr.registry_mut().set_enabled("buffer", false);
        let entries = provider_entries_from_registry(mgr.registry());
        let names: Vec<&str> = entries.iter().map(|e| e.display_name.as_str()).collect();
        let mut expected = builtin_provider_names();
        expected.retain(|name| *name != "Buffer");
        assert_eq!(names, expected);
    }

    #[test]
    fn kind_label_for_is_centralized_and_consistent() {
        // The single label table both surfaces share (design §7.A [fix]).
        assert_eq!(kind_label_for("processmemory"), "Process");
        assert_eq!(kind_label_for("memflowprocessmemory"), "Memflow");
        assert_eq!(kind_label_for("reclass.netcompatlayer"), "Compat");
        assert_eq!(kind_label_for("kernelmemory"), "Kernel");
        assert_eq!(kind_label_for("unknownthing"), "Source");
    }

    #[test]
    fn provider_entries_match_pic4_list() {
        let ps = provider_entries();
        let names: Vec<&str> = ps.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, provider_entries_pic4_names());
        // First-party built-ins do not carry a legacy DLL filename hint.
        let file = ps.iter().find(|e| e.display_name == "File").unwrap();
        assert!(file.dll_file_name.is_empty());
        #[cfg(feature = "process-provider")]
        {
            let process = ps
                .iter()
                .find(|e| e.display_name == "Process Memory")
                .unwrap();
            assert!(process.dll_file_name.is_empty());
        }
        #[cfg(feature = "memflow-provider")]
        {
            let process = ps
                .iter()
                .find(|e| e.display_name == "Memflow Process Memory")
                .unwrap();
            assert!(process.dll_file_name.is_empty());
        }
        #[cfg(all(windows, feature = "kernel-provider"))]
        {
            let process = ps
                .iter()
                .find(|e| e.display_name == "Kernel Memory")
                .unwrap();
            assert!(process.dll_file_name.is_empty());
        }
        #[cfg(all(windows, feature = "windbg-provider"))]
        {
            let process = ps
                .iter()
                .find(|e| e.display_name == "WinDbg Memory")
                .unwrap();
            assert!(process.dll_file_name.is_empty());
        }
        // All providers are provider actions.
        assert!(ps
            .iter()
            .all(|e| e.entry_kind == SourceEntryKind::ProviderAction));
    }

    #[test]
    fn default_entries_match_connected_then_add_source_then_clear_order() {
        let recent = vec![
            ("Reclass.exe".to_string(), "Process".to_string(), false),
            ("Reclass.exe".to_string(), "File".to_string(), true),
        ];
        let es = default_entries(&recent);
        let sections: Vec<_> = es
            .iter()
            .filter(|e| e.entry_kind == SourceEntryKind::SectionHeader)
            .map(|e| e.display_name.as_str())
            .collect();
        assert_eq!(sections, ["Connected", "Add Source"]);
        assert_eq!(es.first().unwrap().display_name, "Connected");

        let add_source = es
            .iter()
            .position(|e| e.display_name == "Add Source")
            .unwrap();
        assert!(es[1..add_source]
            .iter()
            .all(|e| e.entry_kind == SourceEntryKind::SavedSource));
        assert!(es[add_source + 1..es.len() - 1]
            .iter()
            .all(|e| e.entry_kind == SourceEntryKind::ProviderAction));
        // The active recent source is flagged.
        assert!(es
            .iter()
            .any(|e| e.is_active && e.display_name == "Reclass.exe"));
        // "Clear All" is the last entry.
        let last = es.last().unwrap();
        assert_eq!(last.entry_kind, SourceEntryKind::ClearAction);
        assert_eq!(last.display_name, "Clear All");
        assert!(last.enabled);
    }

    #[test]
    fn default_entries_without_recents_skip_recent_section() {
        let es = default_entries(&[]);
        let sections: Vec<_> = es
            .iter()
            .filter(|e| e.entry_kind == SourceEntryKind::SectionHeader)
            .map(|e| e.display_name.as_str())
            .collect();
        assert_eq!(sections, ["Add Source"]);
        assert_eq!(es.first().unwrap().display_name, "Add Source");
        let clear = es.last().unwrap();
        assert_eq!(clear.entry_kind, SourceEntryKind::ClearAction);
        assert!(
            !clear.enabled,
            "Clear All is disabled with no connected source"
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
    fn searchable_omits_pid_when_zero() {
        // The pid==0 rule (`sourcechooserpopup.cpp` `!e.pid.isEmpty()` guard):
        // a saved source with pid 0 is a non-process/File source, so the pid is
        // NOT appended to the searchable string. The view PID pill is guarded by
        // the same `pid != 0` condition (see `render`), so a pid-0 row also paints
        // no pill. `game.bin` has pid 0 + a file path; its searchable must carry
        // the name/kind/path but no "0" pid token.
        let model = SourceModel::new(entries());
        let file_src = model
            .entries()
            .iter()
            .find(|e| e.display_name == "game.bin")
            .unwrap();
        assert_eq!(file_src.pid, 0);
        let s = file_src.searchable();
        assert!(s.contains("game.bin"));
        assert!(s.contains("File"));
        assert!(s.contains("/tmp/game.bin"));
        // No stray " 0" pid token appended (the pid==0 omission).
        assert!(!s.contains(" 0"));
    }

    #[test]
    fn searchable_includes_pid_when_nonzero() {
        // The complement of the pid==0 rule: a non-zero pid IS appended as a
        // space-separated token (and the view paints the "PID <n>" pill).
        let mut e = SourceEntry::saved(0, "svc.exe", "Process");
        e.pid = 4321;
        let s = e.searchable();
        assert!(s.contains("svc.exe"));
        assert!(s.contains(" 4321"));
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
    fn removing_saved_source_compacts_indices_and_keeps_filter() {
        let mut model = SourceModel::new(entries());
        assert!(model.remove_saved_source(0, "game"));
        let saved: Vec<_> = model
            .entries()
            .iter()
            .filter(|entry| entry.entry_kind == super::SourceEntryKind::SavedSource)
            .collect();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].display_name, "game.bin");
        assert_eq!(saved[0].saved_index, 0);
        assert_eq!(model.rows().len(), 1, "the active filter is reapplied");
        assert_eq!(model.rows()[0].entry.display_name, "game.bin");
    }

    #[test]
    fn removing_invalid_saved_source_is_a_noop() {
        let mut model = SourceModel::new(entries());
        let before = model.entries().to_vec();
        assert!(!model.remove_saved_source(-1, ""));
        assert!(!model.remove_saved_source(99, ""));
        assert_eq!(model.entries(), before);
    }

    #[test]
    fn removing_last_default_source_drops_connected_header_and_disables_clear() {
        let recent = vec![("game.exe".to_string(), "Process".to_string(), true)];
        let mut model = SourceModel::new(super::default_entries(&recent));
        assert!(model.remove_saved_source(0, ""));
        assert!(!model.entries().iter().any(|entry| {
            entry.entry_kind == super::SourceEntryKind::SectionHeader
                && entry.display_name == "Connected"
        }));
        let clear = model
            .entries()
            .iter()
            .find(|entry| entry.entry_kind == super::SourceEntryKind::ClearAction)
            .unwrap();
        assert!(!clear.enabled);
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
    fn select_at_skips_unselectable_rows() {
        let mut model = SourceModel::new(entries());
        // Row 0 is the "Saved" section header → not selectable.
        assert!(!model.select_at(0));
        // The notepad.exe saved row is selectable.
        let np = model
            .rows()
            .iter()
            .position(|r| r.entry.display_name == "notepad.exe")
            .unwrap();
        assert!(model.select_at(np));
        assert_eq!(model.selected(), Some(np));
    }

    #[test]
    fn page_home_end_skip_sections() {
        let mut model = SourceModel::new(entries());
        model.move_end();
        let last = model.selected().unwrap();
        assert!(model.rows()[last].entry.selectable());
        model.move_home();
        let first = model.selected().unwrap();
        assert!(model.rows()[first].entry.selectable());
        // Home lands on the first selectable row (skips the leading section).
        assert_ne!(
            model.rows()[first].entry.entry_kind,
            super::SourceEntryKind::SectionHeader
        );
        model.page_down(8);
        if let Some(sel) = model.selected() {
            assert!(model.rows()[sel].entry.selectable());
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
