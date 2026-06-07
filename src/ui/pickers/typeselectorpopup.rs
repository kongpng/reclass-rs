//! Type-selector popup — the centerpiece type/array/pointer picker
//! (`typeselectorpopup.{h,cpp}`, widgets-dialogs.md §9).
//!
//! Port of `TypeSelectorPopup`: a filtered, group-colored list of selectable
//! types (primitives + composites + enums) with modifier toggles (`*`, `**`,
//! `[]`) and a fuzzy filter. The C++ is a custom `QFrame` popup with a hand-
//! painted delegate; per the cookbook (ARCHITECTURE §5) it maps onto a `Popover` +
//! a `List` with a custom `render_item`. This module ports the **load-bearing pure
//! logic** (unit-tested, the parts the C++ tests lock) + a popover view:
//!
//! - [`kind_group_for`] / [`KindGroup`] — `kindGroupFor(NodeKind)` → the colored
//!   group bucket (Hex/Int/Float/Ptr/Vec/Str/Ctr) used for sorting + tinting.
//! - [`kind_group_color`] — `kindGroupColor` against our [`Theme`] semantic colors.
//! - [`TypeEntry`] / [`TypeModel`] — the entry model + the filter algorithm
//!   (`applyFilter`): fuzzy-ranked flat list when filtering, else group-bucketed
//!   sections in the fixed group order with section headers.
//! - [`Modifier`] — the `*`/`**`/`[]` modifier state (`setModifier`/`setMode`).
//! - [`TypeSelectorPopup`] / [`TypeSelectorEvent`] — the popover view.
//!
//! Gated behind the `ui` feature for the view; the model is always built/tested.

use crate::core::kind::{
    is_container_kind, is_func_ptr, is_hex_node, is_matrix_kind, is_pointer_kind, is_string_kind,
    is_vector_kind, size_for_kind, NodeKind, K_KIND_META,
};
use crate::theme::color::Color;
use crate::theme::model::Theme;

/// The default built-in primitive type catalogue (`TypeSelectorPopup::setTypes`
/// over `kKindMeta`): one [`TypeEntry`] per primitive kind, in table order, using
/// the kind's display `type_name` ("hex64", "int32_t", "ptr64", …). The
/// dynamic-size container kinds (Struct/Array) are excluded — those are picked via
/// the modifier row / composite list, not as a base primitive. This is the
/// catalogue [`TypeSelectorPopup::view`] builds its model over.
pub fn default_type_entries() -> Vec<TypeEntry> {
    K_KIND_META
        .iter()
        .filter(|m| !matches!(m.kind, NodeKind::Struct | NodeKind::Array))
        .map(|m| TypeEntry::primitive(m.kind, m.type_name))
        .collect()
}

/// `enum class TypePopupMode` (`typeselectorpopup.h:26`) — what is being picked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TypePopupMode {
    /// Picking a top-level type at the root.
    #[default]
    Root,
    /// Picking a field type (modifiers allowed).
    FieldType,
    /// Picking an array element type (modifiers allowed).
    ArrayElement,
    /// Picking a pointer target (no modifiers).
    PointerTarget,
}

impl TypePopupMode {
    /// Whether the modifier row (`*`/`**`/`[]`) is shown for this mode
    /// (`setMode`: only FieldType / ArrayElement).
    pub fn allows_modifiers(self) -> bool {
        matches!(self, TypePopupMode::FieldType | TypePopupMode::ArrayElement)
    }
}

/// The colored type-group buckets (`kindGroupFor` outputs; `typeselectorpopup`).
/// The order of [`ALL`](KindGroup::ALL) is the fixed section order the empty-
/// filter view buckets into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KindGroup {
    Hex,
    Int,
    Float,
    Ptr,
    Vec,
    Str,
    Ctr,
    Common,
}

impl KindGroup {
    /// The fixed section order (`applyFilter` empty path:
    /// `[Hex,Int,Float,Ptr,Vec,Str,Ctr,Common]`).
    pub const ALL: [KindGroup; 8] = [
        KindGroup::Hex,
        KindGroup::Int,
        KindGroup::Float,
        KindGroup::Ptr,
        KindGroup::Vec,
        KindGroup::Str,
        KindGroup::Ctr,
        KindGroup::Common,
    ];

    /// The short key (`kindGroupFor` returns these literals).
    pub fn key(self) -> &'static str {
        match self {
            KindGroup::Hex => "Hex",
            KindGroup::Int => "Int",
            KindGroup::Float => "Float",
            KindGroup::Ptr => "Ptr",
            KindGroup::Vec => "Vec",
            KindGroup::Str => "Str",
            KindGroup::Ctr => "Ctr",
            KindGroup::Common => "Common",
        }
    }

    /// The section header label (`appendSection` labels:
    /// `[Hex, "Int / Bool", Float, "Pointer / FuncPtr", "Vec / Mat", String, Type,
    /// "Common Types"]`).
    pub fn section_label(self) -> &'static str {
        match self {
            KindGroup::Hex => "Hex",
            KindGroup::Int => "Int / Bool",
            KindGroup::Float => "Float",
            KindGroup::Ptr => "Pointer / FuncPtr",
            KindGroup::Vec => "Vec / Mat",
            KindGroup::Str => "String",
            KindGroup::Ctr => "Type",
            KindGroup::Common => "Common Types",
        }
    }

    /// Whether this group has a category chip (Hex/Int/Float/Ptr do; the rest are
    /// always visible — `catAllowed`).
    pub fn has_chip(self) -> bool {
        matches!(
            self,
            KindGroup::Hex | KindGroup::Int | KindGroup::Float | KindGroup::Ptr
        )
    }
}

/// The list sort mode for the empty-filter (group-bucketed) view's column-header
/// sort toolbar (`SortMode` in `typeselectorpopup.cpp:640`). `Group` is the
/// default bucketed layout; `Name`/`Size` flatten the list and sort by that key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SortMode {
    /// Group-bucketed sections in the fixed [`KindGroup::ALL`] order (default).
    #[default]
    Group,
    /// A flat list sorted by display name.
    Name,
    /// A flat list sorted by byte size.
    Size,
}

/// `kindGroupFor(NodeKind)` (`typeselectorpopup.cpp:91`).
pub fn kind_group_for(k: NodeKind) -> KindGroup {
    if is_hex_node(k) {
        return KindGroup::Hex;
    }
    if matches!(
        k,
        NodeKind::Int8
            | NodeKind::Int16
            | NodeKind::Int32
            | NodeKind::Int64
            | NodeKind::Int128
            | NodeKind::UInt8
            | NodeKind::UInt16
            | NodeKind::UInt32
            | NodeKind::UInt64
            | NodeKind::UInt128
            | NodeKind::Bool
    ) {
        return KindGroup::Int;
    }
    if matches!(k, NodeKind::Float16 | NodeKind::Float | NodeKind::Double) {
        return KindGroup::Float;
    }
    if is_pointer_kind(k) || is_func_ptr(k) {
        return KindGroup::Ptr;
    }
    if is_vector_kind(k) || is_matrix_kind(k) {
        return KindGroup::Vec;
    }
    if is_string_kind(k) {
        return KindGroup::Str;
    }
    if is_container_kind(k) {
        return KindGroup::Ctr;
    }
    KindGroup::Hex
}

/// `kindGroupColor(group)` (`typeselectorpopup.cpp:69`) — the accent color for a
/// group, resolved from our [`Theme`]'s semantic colors. Falls back to `text`
/// when a color is unset (the C++ reads the live palette which is always set).
pub fn kind_group_color(group: KindGroup, theme: &Theme) -> Color {
    let text = theme.text.unwrap_or(Color::rgb(0xd4, 0xd4, 0xd4));
    match group {
        KindGroup::Hex => theme.ind_hover_span,    // purple
        KindGroup::Int => theme.syntax_keyword,    // blue
        KindGroup::Float => theme.marker_cycle,    // amber
        KindGroup::Ptr => theme.marker_ptr,        // red
        KindGroup::Vec => theme.syntax_type,       // teal
        KindGroup::Str => theme.syntax_string,     // salmon
        KindGroup::Ctr => theme.ind_data_changed,  // green
        KindGroup::Common => theme.syntax_preproc, // grey
    }
    .unwrap_or(text)
}

/// The kind of a [`TypeEntry`] (`TypeEntry::entryKind`, `typeselectorpopup.h:30`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    /// A built-in primitive kind.
    Primitive,
    /// A user struct/class/enum (composite).
    Composite,
    /// A section header pseudo-entry (not selectable).
    Section,
}

/// A selectable type entry (`struct TypeEntry`, condensed to the fields the model
/// + view actually use; the cosmetic detail-pane fields are omitted).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeEntry {
    pub entry_kind: EntryKind,
    /// For [`EntryKind::Primitive`]: the kind it represents.
    pub primitive_kind: NodeKind,
    /// For [`EntryKind::Composite`]: the struct/enum id.
    pub struct_id: u64,
    /// The display name (e.g. "int32_t", "Player", "EFlags").
    pub display_name: String,
    /// "struct"/"class"/"enum" for composites; empty for primitives.
    pub class_keyword: String,
    /// false → grayed + unselectable (still shown).
    pub enabled: bool,
    /// Byte size (0 ⇒ "dyn").
    pub size_bytes: i32,
    /// The colored group (auto-assigned from the kind if a primitive).
    pub group: KindGroup,
}

impl TypeEntry {
    /// Build a primitive entry from a [`NodeKind`].
    pub fn primitive(kind: NodeKind, display_name: &str) -> Self {
        TypeEntry {
            entry_kind: EntryKind::Primitive,
            primitive_kind: kind,
            struct_id: 0,
            display_name: display_name.to_string(),
            class_keyword: String::new(),
            enabled: true,
            size_bytes: size_for_kind(kind),
            group: kind_group_for(kind),
        }
    }

    /// Build a composite (struct/enum) entry.
    pub fn composite(struct_id: u64, name: &str, keyword: &str, size: i32) -> Self {
        TypeEntry {
            entry_kind: EntryKind::Composite,
            primitive_kind: NodeKind::Struct,
            struct_id,
            display_name: name.to_string(),
            class_keyword: keyword.to_string(),
            enabled: true,
            size_bytes: size,
            // Composites bucket into the container group ("Type" section).
            group: KindGroup::Ctr,
        }
    }

    /// A section-header pseudo-entry.
    fn section(label: &str) -> Self {
        TypeEntry {
            entry_kind: EntryKind::Section,
            primitive_kind: NodeKind::Struct,
            struct_id: 0,
            display_name: label.to_string(),
            class_keyword: String::new(),
            enabled: false,
            size_bytes: 0,
            group: KindGroup::Common,
        }
    }

    /// Whether this row can be selected (not a section, and enabled).
    pub fn selectable(&self) -> bool {
        self.entry_kind != EntryKind::Section && self.enabled
    }
}

/// The modifier applied to a selected type (`*`/`**`/`[]`; `setModifier`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Modifier {
    /// No modifier.
    #[default]
    None,
    /// `*` — single pointer (id 1).
    Pointer,
    /// `**` — double pointer (id 2).
    PointerPointer,
    /// `[]` — array of `count` (id 3).
    Array(i32),
}

impl Modifier {
    /// The C++ `checkedId()` (`setModifier` checks `*`/`**`/`[]` by id 1/2/3).
    pub fn checked_id(self) -> i32 {
        match self {
            Modifier::None => 0,
            Modifier::Pointer => 1,
            Modifier::PointerPointer => 2,
            Modifier::Array(_) => 3,
        }
    }

    /// The suffix this modifier appends to the base name to form the full type
    /// text (`acceptCurrent` `fullText`): `*`, `**`, or `[count]`.
    pub fn suffix(self) -> String {
        match self {
            Modifier::None => String::new(),
            Modifier::Pointer => "*".to_string(),
            Modifier::PointerPointer => "**".to_string(),
            Modifier::Array(n) => format!("[{n}]"),
        }
    }
}

/// A row in the rendered (filtered) list — either a selectable entry or a section
/// header, plus the fuzzy match positions for highlight painting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeRow {
    /// Index into the model's `entries` (for selection), or the synthesized
    /// section/header (stored inline).
    pub entry: TypeEntry,
    /// Fuzzy match char positions in `display_name` (empty when not filtering).
    pub match_positions: Vec<usize>,
}

impl TypeRow {
    /// For a section-header row, the [`KindGroup`] it heads (resolved from its
    /// section label) so the view can paint a matching colored dot. `None` for a
    /// non-section row or an unrecognized label.
    pub fn entry_group_for_label(&self) -> Option<KindGroup> {
        if self.entry.entry_kind != EntryKind::Section {
            return None;
        }
        KindGroup::ALL
            .into_iter()
            .find(|g| g.section_label() == self.entry.display_name)
    }
}

/// The type-selector list model + filter (`applyFilter`,
/// `typeselectorpopup.cpp:1485`).
#[derive(Clone, Debug, Default)]
pub struct TypeModel {
    /// All candidate entries (primitives + composites), in insertion order.
    entries: Vec<TypeEntry>,
    /// The rendered rows after the last [`apply_filter`](TypeModel::apply_filter).
    rows: Vec<TypeRow>,
    /// The selected row index (into `rows`), `None` when empty.
    selected: Option<usize>,
    mode: TypePopupMode,
    modifier: Modifier,
    sort_mode: SortMode,
    /// Sort direction for the flat (Name/Size) sort modes: +1 ascending, -1
    /// descending. Toggled when the active sort header is re-clicked (`m_sortDir`).
    sort_dir: i32,
    /// The last filter applied — kept so a sort/category change can re-run the
    /// filter without the host re-pushing the query text.
    last_filter: String,
    /// Recently-picked type names (most-recent-first), surfaced in a "Recent"
    /// section at the top of the group view (`m_recentNames`,
    /// `typeselectorpopup.cpp:1716`).
    recent_names: Vec<String>,
    /// The byte size of the node's current type (`m_currentNodeSize`). When
    /// non-zero AND the mode is not [`TypePopupMode::Root`], the group-bucketed
    /// view lists the entries whose `size_bytes` equal this size FIRST within
    /// each group ("same-size-first", `typeselectorpopup.cpp:1735-1745`).
    current_node_size: i32,
    /// Which chip-bearing groups (Hex/Int/Float/Ptr) are active — the model-side
    /// mirror of the C++ category chips (`m_groupChips` / `catAllowed`,
    /// `typeselectorpopup.cpp:1602-1608`). An EMPTY set means "all visible" (the
    /// default all-checked state); a non-empty set restricts the scored/bucketed
    /// list to those chip-bearing groups (the always-on Vec/Str/Ctr/Common groups
    /// are never filtered out). Filtering happens in the model so hidden rows are
    /// excluded from the ranked/bucketed list rather than hidden at render.
    active_groups: std::collections::BTreeSet<&'static str>,
}

impl TypeModel {
    /// Build a model over the given entries (unfiltered, group-bucketed).
    pub fn new(entries: Vec<TypeEntry>) -> Self {
        let mut m = TypeModel {
            entries,
            rows: Vec::new(),
            selected: None,
            mode: TypePopupMode::default(),
            modifier: Modifier::None,
            sort_mode: SortMode::default(),
            sort_dir: 1,
            last_filter: String::new(),
            recent_names: Vec::new(),
            current_node_size: 0,
            // The C++ seeds all four category chips CHECKED (typeselectorpopup.cpp:588);
            // active_groups is the set of VISIBLE/checked chip-groups, so it starts
            // full. Unchecking a chip removes its group (hides it); it is NOT an
            // "only these" filter.
            active_groups: KindGroup::ALL
                .iter()
                .filter(|g| g.has_chip())
                .map(|g| g.key())
                .collect(),
        };
        m.apply_filter("");
        m
    }

    /// Set the recently-picked type names (`setRecentNames`/`m_recentNames`):
    /// these surface in a "Recent" section at the top of the group-bucketed view
    /// (the C++ lists them first so common picks are one chord away). Re-runs the
    /// last filter so the section appears immediately.
    pub fn set_recent_names(&mut self, names: Vec<String>) {
        self.recent_names = names;
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// The recently-picked type names.
    pub fn recent_names(&self) -> &[String] {
        &self.recent_names
    }

    /// The current list sort mode.
    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    /// The current sort direction (+1 ascending / -1 descending).
    pub fn sort_dir(&self) -> i32 {
        self.sort_dir
    }

    /// Re-click a sort header (`m_sortMode`/`m_sortDir`): re-clicking the active
    /// mode flips the direction, picking a new mode resets to ascending. Re-runs
    /// the last filter so the rows re-layout immediately.
    pub fn set_sort_mode(&mut self, mode: SortMode) {
        if self.sort_mode == mode {
            self.sort_dir = -self.sort_dir;
        } else {
            self.sort_mode = mode;
            self.sort_dir = 1;
        }
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// All candidate entries.
    pub fn entries(&self) -> &[TypeEntry] {
        &self.entries
    }

    /// The rendered rows (sections + entries) after filtering.
    pub fn rows(&self) -> &[TypeRow] {
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

    /// The selected entry (skips sections — selection always lands on a selectable
    /// row), if any.
    pub fn selected_entry(&self) -> Option<&TypeEntry> {
        self.selected
            .and_then(|r| self.rows.get(r))
            .map(|row| &row.entry)
            .filter(|e| e.selectable())
    }

    /// The current popup mode.
    pub fn mode(&self) -> TypePopupMode {
        self.mode
    }

    /// The mode-aware filter placeholder, carrying the total selectable-type
    /// count (C++ `setTypes` dynamic placeholder, typeselectorpopup.cpp:1256-1267):
    /// the noun tracks the mode and `count` is every non-section entry in the full
    /// catalogue (not the live-filtered subset). The `(Ctrl+F)` focus hint is a
    /// Rust addition — that shortcut is wired in the popup's key handler.
    pub fn filter_placeholder(&self) -> String {
        let count = self
            .entries
            .iter()
            .filter(|e| e.entry_kind != EntryKind::Section)
            .count();
        let noun = match self.mode {
            TypePopupMode::Root => "structs",
            TypePopupMode::FieldType => "types",
            TypePopupMode::ArrayElement => "element types",
            TypePopupMode::PointerTarget => "targets",
        };
        format!("Filter {count} {noun}..  (Ctrl+F)")
    }

    /// The current modifier.
    pub fn modifier(&self) -> Modifier {
        self.modifier
    }

    /// `setMode(mode)` (`typeselectorpopup.cpp:1086`): set the mode and **always
    /// clear the modifier** (the test `testSetModeResetsModifierInPointerTargetMode`).
    /// Re-runs the last filter so the group-bucketed same-size-first ordering
    /// (which is gated on `mode != Root`) re-layouts immediately.
    pub fn set_mode(&mut self, mode: TypePopupMode) {
        self.mode = mode;
        self.modifier = Modifier::None;
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// The byte size of the node's current type (`m_currentNodeSize`).
    pub fn current_node_size(&self) -> i32 {
        self.current_node_size
    }

    /// Set the node's current type size (`setCurrentNodeSize`/`m_currentNodeSize`).
    /// Drives the group-bucketed same-size-first ordering when the mode is not
    /// [`TypePopupMode::Root`]. Re-runs the last filter so the rows re-layout.
    pub fn set_current_node_size(&mut self, size: i32) {
        self.current_node_size = size;
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// The active chip-bearing groups (empty ⇒ all visible).
    pub fn active_groups(&self) -> &std::collections::BTreeSet<&'static str> {
        &self.active_groups
    }

    /// Whether a chip-bearing group is currently active (its key is in the set).
    pub fn group_active(&self, group: KindGroup) -> bool {
        self.active_groups.contains(group.key())
    }

    /// `catAllowed(entry)` (`typeselectorpopup.cpp:1602-1608`): whether an entry's
    /// group passes the active category-chip filter.
    ///
    /// - empty active set → all visible (the all-checked default);
    /// - groups WITHOUT a chip toggle (Vec/Str/Ctr/Common) are always visible
    ///   (only chip-bearing groups can be filtered out);
    /// - otherwise a chip-bearing group passes iff its key is active.
    fn group_allowed(&self, group: KindGroup) -> bool {
        // Groups without a chip toggle (Vec/Str/Ctr/Common) always pass; a
        // chip-bearing group passes iff its chip is checked/active — exactly the
        // C++ `catAllowed` (typeselectorpopup.cpp:1602-1608). active_groups is now
        // the literal set of checked chips, so an empty set hides ALL chip groups
        // (no special "empty = show all" case).
        if !group.has_chip() {
            return true;
        }
        self.active_groups.contains(group.key())
    }

    /// Toggle a chip-bearing category (Hex/Int/Float/Ptr) and re-run the last
    /// filter so the bucketed/ranked list re-excludes the hidden group's rows
    /// (`m_groupChips` toggle → `applyFilter`).
    pub fn toggle_group(&mut self, group: KindGroup) {
        let key = group.key();
        if self.active_groups.contains(key) {
            self.active_groups.remove(key);
        } else {
            self.active_groups.insert(key);
        }
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// "all" — check every chip (every chip-group visible) and re-filter.
    pub fn select_all_groups(&mut self) {
        self.active_groups = KindGroup::ALL
            .iter()
            .filter(|g| g.has_chip())
            .map(|g| g.key())
            .collect();
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// "none" — the C++ `noneBtn` keeps AT LEAST ONE group checked (the first
    /// chip stays on, the rest go off) so the list never goes fully empty. We
    /// restrict to the first chip-bearing group (Hex), then re-filter.
    pub fn select_no_groups(&mut self) {
        self.active_groups.clear();
        self.active_groups.insert(KindGroup::Hex.key());
        let q = self.last_filter.clone();
        self.apply_filter(&q);
    }

    /// `setModifier(modId, arr)` (`typeselectorpopup.cpp:1106`): set the modifier
    /// (unchecks all, then checks by id). For an array, `count` is the element
    /// count.
    pub fn set_modifier(&mut self, modifier: Modifier) {
        self.modifier = modifier;
    }

    /// The full type text for the current selection + modifier (`fullText`):
    /// `display_name` + the modifier suffix. `None` if nothing selectable is
    /// selected.
    pub fn full_text(&self) -> Option<String> {
        let e = self.selected_entry()?;
        Some(format!("{}{}", e.display_name, self.modifier.suffix()))
    }

    /// Re-filter against `filter` (`applyFilter`). Empty → group-bucketed sections
    /// in the fixed [`KindGroup::ALL`] order, each with a header; non-empty →
    /// flat list ranked by [`fuzzy_score`](crate::ui::fuzzy::fuzzy_score) desc (no
    /// headers). Selects the first selectable row.
    pub fn apply_filter(&mut self, filter: &str) {
        let trimmed = filter.trim();
        self.last_filter = filter.to_string();
        self.rows.clear();
        if trimmed.is_empty() {
            // Empty filter honors the sort mode: Group → bucketed sections; the
            // flat sort modes (Name/Size) produce a single sorted list, no
            // section headers (the C++ `m_sortMode != SortGroup` branch).
            match self.sort_mode {
                SortMode::Group => self.build_bucketed(),
                SortMode::Name | SortMode::Size => self.build_sorted_flat(),
            }
        } else {
            self.build_filtered(trimmed);
        }
        self.selected = self.first_selectable_row();
    }

    /// Build the empty-filter **flat** sorted view for the Name/Size sort modes:
    /// every selectable entry in one list, sorted by the active key + direction,
    /// with no section headers (the C++ `SortName`/`SortSize` branch).
    /// The single section row shown when a filter yields no rows. A no-op when
    /// `rows` is non-empty — the guard the three list builders share.
    fn push_empty_state(rows: &mut Vec<TypeRow>, label: &str) {
        if rows.is_empty() {
            rows.push(TypeRow {
                entry: TypeEntry::section(label),
                match_positions: Vec::new(),
            });
        }
    }

    fn build_sorted_flat(&mut self) {
        let dir = self.sort_dir;
        let mut entries: Vec<TypeEntry> = self.entries.clone();
        match self.sort_mode {
            SortMode::Name => {
                entries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
            }
            SortMode::Size => {
                entries.sort_by(|a, b| {
                    a.size_bytes
                        .cmp(&b.size_bytes)
                        .then_with(|| a.display_name.cmp(&b.display_name))
                });
            }
            SortMode::Group => {}
        }
        if dir < 0 {
            entries.reverse();
        }
        let mut rows: Vec<TypeRow> = entries
            .into_iter()
            .map(|e| TypeRow {
                entry: e,
                match_positions: Vec::new(),
            })
            .collect();
        Self::push_empty_state(&mut rows, "No types available");
        self.rows = rows;
    }

    /// Build the empty-filter bucketed view: per-group sections in fixed order.
    fn build_bucketed(&mut self) {
        let mut rows: Vec<TypeRow> = Vec::new();
        // Recent section (`m_recentNames`, `typeselectorpopup.cpp:1716`): entries
        // whose display name matches a recent pick, listed first. Items still
        // appear in their normal group section below.
        if !self.recent_names.is_empty() {
            let mut recents: Vec<TypeEntry> = Vec::new();
            for nm in &self.recent_names {
                if let Some(e) = self.entries.iter().find(|e| {
                    &e.display_name == nm && e.selectable() && self.group_allowed(e.group)
                }) {
                    recents.push(e.clone());
                }
            }
            if !recents.is_empty() {
                rows.push(TypeRow {
                    entry: TypeEntry::section("Recent"),
                    match_positions: Vec::new(),
                });
                for e in recents {
                    rows.push(TypeRow {
                        entry: e,
                        match_positions: Vec::new(),
                    });
                }
            }
        }
        // Case-insensitive alphabetical comparator — the single within-group sort
        // key for EVERY group (`typeselectorpopup.cpp:1690`'s `alphabetical`); the
        // Hex group is no longer special-cased to size-descending.
        let alphabetical = |a: &TypeEntry, b: &TypeEntry| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        };
        for group in KindGroup::ALL {
            // Category-chip gate (`catAllowed` → `buckets`,
            // `typeselectorpopup.cpp:1687`): chip-hidden groups are skipped so
            // their rows never enter the bucketed list.
            let mut group_entries: Vec<TypeEntry> = self
                .entries
                .iter()
                .filter(|e| e.group == group && self.group_allowed(e.group))
                .cloned()
                .collect();
            if group_entries.is_empty() {
                continue;
            }
            // Same-size-first within the group when retyping a sized node
            // (`m_mode != Root && m_currentNodeSize > 0`,
            // `typeselectorpopup.cpp:1735-1745`): entries whose `size_bytes`
            // equal the node's current size come first (each part sorted
            // case-insensitively alphabetical), then the rest.
            if self.mode != TypePopupMode::Root && self.current_node_size > 0 {
                let (mut same_size, mut other): (Vec<TypeEntry>, Vec<TypeEntry>) = group_entries
                    .into_iter()
                    .partition(|e| e.size_bytes == self.current_node_size);
                same_size.sort_by(alphabetical);
                other.sort_by(alphabetical);
                same_size.extend(other);
                group_entries = same_size;
            } else {
                group_entries.sort_by(alphabetical);
            }
            rows.push(TypeRow {
                entry: TypeEntry::section(group.section_label()),
                match_positions: Vec::new(),
            });
            for e in group_entries {
                rows.push(TypeRow {
                    entry: e,
                    match_positions: Vec::new(),
                });
            }
        }
        Self::push_empty_state(&mut rows, "No types available");
        self.rows = rows;
    }

    /// Build the filtered flat ranked view (fuzzy, no headers).
    fn build_filtered(&mut self, query: &str) {
        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            // Category-chip gate (`catAllowed`, `typeselectorpopup.cpp:1660`):
            // chip-hidden groups are excluded from the scored list entirely.
            if !self.group_allowed(e.group) {
                continue;
            }
            let mut pos = Vec::new();
            let s = crate::ui::fuzzy::source_score(query, &e.display_name, Some(&mut pos));
            if s > 0 {
                scored.push((s, i, pos));
            }
        }
        // Sort by score desc (stable for ties → insertion order).
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        let mut rows: Vec<TypeRow> = scored
            .into_iter()
            .map(|(_, i, pos)| TypeRow {
                entry: self.entries[i].clone(),
                match_positions: pos,
            })
            .collect();
        Self::push_empty_state(&mut rows, &format!("No types match '{query}'"));
        self.rows = rows;
    }

    /// The selectability mask over `rows`, for [`crate::ui::navlist`] navigation.
    fn selectable_mask(&self) -> Vec<bool> {
        self.rows.iter().map(|r| r.entry.selectable()).collect()
    }

    /// The first selectable row (`nextSelectableRow` from the top).
    fn first_selectable_row(&self) -> Option<usize> {
        crate::ui::navlist::first_selectable(&self.selectable_mask())
    }

    /// Move the selection down to the next selectable row (Down).
    pub fn move_down(&mut self) {
        self.selected = crate::ui::navlist::step(&self.selectable_mask(), self.selected, 1);
    }

    /// Move the selection up to the previous selectable row (Up).
    pub fn move_up(&mut self) {
        self.selected = crate::ui::navlist::step(&self.selectable_mask(), self.selected, -1);
    }

    /// Move the selection down by `page` selectable rows (PageDown), landing on
    /// the last selectable row if fewer remain (`Key_PageDown`).
    pub fn page_down(&mut self, page: usize) {
        self.selected = crate::ui::navlist::page(&self.selectable_mask(), self.selected, 1, page);
    }

    /// Move the selection up by `page` selectable rows (PageUp), landing on the
    /// first selectable row if fewer remain (`Key_PageUp`).
    pub fn page_up(&mut self, page: usize) {
        self.selected = crate::ui::navlist::page(&self.selectable_mask(), self.selected, -1, page);
    }

    /// Select the first selectable row (Home).
    pub fn move_home(&mut self) {
        self.selected = self.first_selectable_row();
    }

    /// Select the last selectable row (End).
    pub fn move_end(&mut self) {
        self.selected = crate::ui::navlist::last_selectable(&self.selectable_mask());
    }

    /// Pre-select the row matching a primitive `kind` (`setTypes` current-entry
    /// pre-select, `typeselectorpopup.cpp:1273`): scan for a primitive row of that
    /// kind and select it. Returns the selected row index, if found.
    pub fn select_kind(&mut self, kind: NodeKind) -> Option<usize> {
        let found = self.rows.iter().position(|r| {
            r.entry.selectable()
                && r.entry.entry_kind == EntryKind::Primitive
                && r.entry.primitive_kind == kind
        });
        if let Some(i) = found {
            self.selected = Some(i);
        }
        found
    }

    /// Pre-select the row matching a COMPOSITE `struct_id` (`setTypes`
    /// current-entry pre-select for composites, `typeselectorpopup.cpp:1280`:
    /// `entry.structId == m_currentEntry.structId`). Scan for a composite row
    /// with that id and select it. Returns the selected row index, if found.
    pub fn select_struct(&mut self, struct_id: u64) -> Option<usize> {
        if struct_id == 0 {
            return None;
        }
        let found = self.rows.iter().position(|r| {
            r.entry.selectable()
                && r.entry.entry_kind == EntryKind::Composite
                && r.entry.struct_id == struct_id
        });
        if let Some(i) = found {
            self.selected = Some(i);
        }
        found
    }

    /// Select a specific row if it is selectable (a click; `acceptIndex`).
    pub fn select_row(&mut self, row: usize) -> bool {
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
}

// ── gpui view ───────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::{TypeSelectorEvent, TypeSelectorPopup};

#[cfg(feature = "ui")]
mod view {
    use super::{
        default_type_entries, EntryKind, KindGroup, Modifier, SortMode, TypeEntry, TypeModel,
        TypePopupMode,
    };
    use crate::core::kind::NodeKind;
    use crate::theme::model::Theme;
    use crate::ui::design::{color, highlighted_spans, icon, tokens};
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::input::{Input, InputEvent, InputState};
    use gpui_component::{Icon, IconName, Sizable as _};

    /// The leading kind-chip icon for a type group (the colored square chip in the
    /// C++ delegate). Maps each [`KindGroup`] to a domain [`icon`] helper so the
    /// chip glyph reads the group (hex memory-stick, int dash, float dash, ptr
    /// arrow, …). Tinted by the caller with the group color.
    fn group_icon(group: KindGroup) -> Icon {
        match group {
            KindGroup::Hex => icon::hex(),
            KindGroup::Int => Icon::new(IconName::Dash),
            KindGroup::Float => Icon::new(IconName::Dash),
            KindGroup::Ptr => icon::pointer(),
            KindGroup::Vec => icon::array(),
            KindGroup::Str => Icon::new(IconName::Dash),
            KindGroup::Ctr => icon::struct_(),
            KindGroup::Common => Icon::new(IconName::Dash),
        }
    }

    /// The leading glyph for a row. Enum composites get the dedicated enum symbol
    /// (the C++ `symbol-enum.svg` chosen for `category == CatEnum`,
    /// typeselectorpopup.cpp:353-356); every other entry keeps its group glyph —
    /// struct/class composites the container icon, primitives their kind glyph.
    fn entry_icon(entry: &TypeEntry) -> Icon {
        if entry.entry_kind == EntryKind::Composite
            && entry.class_keyword.eq_ignore_ascii_case("enum")
        {
            icon::enum_()
        } else {
            group_icon(entry.group)
        }
    }

    /// Render a type name with fuzzy-matched chars emphasized (accent + semibold),
    /// the rest in `base`. `positions` are char indices into `name`.

    /// The popup's outcome (the editor consumes this — see the menus↔editor
    /// CONTRACT). `Chosen` carries the picked base [`NodeKind`] plus the optional
    /// [`Modifier`] (`*` pointer / `**` double-pointer / `[]` array); the editor
    /// calls `controller_mut().change_node_kind(idx, kind)` then applies the
    /// modifier via its existing pointer/array ops.
    #[derive(Clone, Debug)]
    pub enum TypeSelectorEvent {
        /// A type was chosen: its base kind + the optional modifier. The
        /// `create_new` flag marks the "+ New" case (item 15) — a brand-new
        /// struct/class type rather than the existing `Struct` primitive — so the
        /// editor's apply path can create a fresh composite. Carried on the
        /// existing `Chosen` variant (not a new variant) to keep the editor's
        /// exhaustive match stable; the editor reads `create_new` to branch.
        Chosen {
            kind: NodeKind,
            modifier: Option<Modifier>,
            /// `true` for the "+ New" footer button — the editor creates a fresh
            /// populated `NewClass[_N]` (8×Hex64) and embeds the node as an
            /// instance of it, rather than applying a bare empty `Struct`.
            create_new: bool,
            /// Whether the picked row is a primitive or a composite (struct/enum).
            /// The editor routes the apply through `apply_type_popup_result` and
            /// needs the entry kind to build the right `TypePopupChoice` (a
            /// composite carries its `struct_id`, a primitive its `kind`). Mirrors
            /// the C++ `TypeEntry::entryKind` passed to `applyTypePopupResult`.
            entry_kind: EntryKind,
            /// For a composite pick: the referenced struct/enum id (0 ⇒ a built-in
            /// or cross-document type imported by `display_name`). Ignored for
            /// primitives. The C++ `TypeEntry::structId`.
            struct_id: u64,
            /// The picked row's display name (`int32_t`, `Player`, …). Carried so
            /// the editor can record it in the recent-types list and import a
            /// built-in composite by name. The C++ `TypeEntry::displayName`.
            display_name: String,
        },
        /// Dismissed (the `×`, Esc, or clicking outside).
        Cancel,
    }

    /// The type-selector popover view.
    pub struct TypeSelectorPopup {
        model: TypeModel,
        /// The kind the node currently has — highlighted as the active type.
        current: NodeKind,
        /// When the node is a COMPOSITE, the referenced struct/enum id so the popup
        /// opens pre-highlighting that row by id (the C++ `m_currentEntry.structId`
        /// branch in `setTypes`). 0 ⇒ the node is a primitive (use `current`).
        current_struct_id: u64,
        input: Entity<InputState>,
        /// The array-element count input (the `[]` modifier's `n` box,
        /// `m_arrayCountEdit`); shown only when the array modifier is active.
        array_count_input: Entity<InputState>,
        focus_handle: FocusHandle,
        /// Scrolls the list so the keyboard-selected row stays visible (B2 / item
        /// 1 / item 9). A plain `ScrollHandle` on the scrollable list container;
        /// the rows are variable-height (section headers vs entries) so a uniform
        /// list does not fit — `scroll_to_item` over the rendered children does.
        list_scroll: ScrollHandle,
        /// The byte size of the node's current type — drives the size diff in the
        /// footer (`m_currentNodeSize`).
        current_node_size: i32,
        /// The pointer byte size — the resulting size when a pointer modifier is
        /// active (`m_pointerSize`).
        pointer_size: i32,
        _subscription: Subscription,
        _count_subscription: Subscription,
    }

    impl TypeSelectorPopup {
        /// Build the change-type popup over the built-in primitive catalogue,
        /// highlighting `current` as the active type (the contract entry point the
        /// editor opens via `window.open_dialog`). Returns the entity so the host
        /// can subscribe to [`TypeSelectorEvent`].
        ///
        /// Defaults the model to [`TypePopupMode::FieldType`] so the modifier row
        /// (`*` / `**` / `[]`) renders — change-type-on-a-field is the field-type
        /// flow in the C++ (`reclass_right_click_on_type.png` shows
        /// `*  **  []  + New  OK` in the footer). Hosts that pick a pointer target
        /// (no modifiers) can override via [`set_mode`](Self::set_mode).
        pub fn view(current: NodeKind, window: &mut Window, cx: &mut App) -> Entity<Self> {
            cx.new(|cx| {
                let mut popup = Self::new_with_current(default_type_entries(), current, window, cx);
                popup.model.set_mode(TypePopupMode::FieldType);
                popup
            })
        }

        /// Build the popup over the given entries (kept for tests / custom
        /// catalogues), with no preset current kind.
        pub fn new(entries: Vec<TypeEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
            Self::new_with_current(entries, NodeKind::Hex8, window, cx)
        }

        pub fn new_with_current(
            entries: Vec<TypeEntry>,
            current: NodeKind,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let mut model = TypeModel::new(entries);
            // Pre-select the node's current type so the picker opens highlighting
            // the existing kind (B7 / item 7) rather than the default first row.
            model.select_kind(current);
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter types..  (Ctrl+F)"));
            let array_count_input = cx.new(|cx| InputState::new(window, cx).placeholder("n"));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        this.model.apply_filter(&q);
                        // Re-pin the current type when the filter clears.
                        if q.trim().is_empty() {
                            this.re_pin_current();
                        }
                        this.scroll_selected_into_view();
                        cx.notify();
                    }
                });
            // The array count box feeds Modifier::Array(n) live so the footer
            // size-diff preview tracks the typed count (item 8).
            let count_subscription = cx.subscribe_in(
                &array_count_input,
                window,
                |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        this.sync_array_count(cx);
                        cx.notify();
                    }
                },
            );
            TypeSelectorPopup {
                model,
                current,
                current_struct_id: 0,
                input,
                array_count_input,
                focus_handle: cx.focus_handle(),
                list_scroll: ScrollHandle::new(),
                current_node_size: crate::core::kind::size_for_kind(current),
                pointer_size: 8,
                _subscription: subscription,
                _count_subscription: count_subscription,
            }
        }

        /// Read the array-count input and, if the array modifier is active, push
        /// the parsed count into the model (`Modifier::Array(n)`). Empty / invalid
        /// → count 1 (the C++ defaults the count box to "1").
        fn sync_array_count(&mut self, cx: &mut Context<Self>) {
            if !matches!(self.model.modifier(), Modifier::Array(_)) {
                return;
            }
            let n = self.array_count_value(cx);
            self.model.set_modifier(Modifier::Array(n));
        }

        /// The current array-count value parsed from the count box (≥1).
        fn array_count_value(&self, cx: &App) -> i32 {
            self.array_count_input
                .read(cx)
                .value()
                .trim()
                .parse::<i32>()
                .ok()
                .filter(|n| *n > 0)
                .unwrap_or(1)
        }

        /// Scroll the currently-selected model row into view (B2 / item 1 / item
        /// 9). The category-chip filtering now happens in the MODEL (chip-hidden
        /// rows are excluded from `model.rows()`), so the rendered children map
        /// 1:1 onto the model rows and the model row index IS the child index.
        fn scroll_selected_into_view(&self) {
            crate::ui::design::scroll_selected(&self.list_scroll, self.model.selected());
        }

        /// Read-only access to the model.
        pub fn model(&self) -> &TypeModel {
            &self.model
        }

        /// Set the popup mode (`setMode`) — drives whether the modifier row
        /// (`*` / `**` / `[]`) is shown. The change-type entry point
        /// ([`view`](Self::view)) defaults to [`TypePopupMode::FieldType`]; a host
        /// retyping an array element should pass [`TypePopupMode::ArrayElement`],
        /// and a pointer-target pick [`TypePopupMode::PointerTarget`] (which hides
        /// the modifiers). Clears any active modifier (per `setMode` semantics).
        pub fn set_mode(
            &mut self,
            mode: TypePopupMode,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.model.set_mode(mode);
            // Re-label the filter input for the new mode (noun + total type count).
            let placeholder = self.model.filter_placeholder();
            self.input
                .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
            cx.notify();
        }

        /// Set the array modifier count + select the `[]` modifier.
        pub fn set_array(&mut self, count: i32, cx: &mut Context<Self>) {
            self.model.set_modifier(Modifier::Array(count));
            cx.notify();
        }

        /// Pre-toggle an arbitrary modifier (`*`/`**`/`[]`) — the opener uses this
        /// to mirror the field's current shape when the selector launches (C++
        /// `setModifier(preModId, preArrayCount)`, controller.cpp:4511).
        pub fn set_modifier(&mut self, modifier: Modifier, cx: &mut Context<Self>) {
            self.model.set_modifier(modifier);
            cx.notify();
        }

        /// Set the node's current type size + pointer size for the footer size
        /// diff (`setCurrentNodeSize`/`setPointerSize`). The current node size is
        /// also pushed into the model so the group-bucketed same-size-first
        /// ordering applies (re-pinning the current selection after the rows
        /// rebuild).
        pub fn set_sizes(&mut self, current_node_size: i32, pointer_size: i32) {
            self.current_node_size = current_node_size;
            self.pointer_size = pointer_size;
            self.model.set_current_node_size(current_node_size);
            self.re_pin_current();
        }

        /// Set the recently-picked type names (item 16) — forwarded to the model.
        pub fn set_recent_names(&mut self, names: Vec<String>, cx: &mut Context<Self>) {
            self.model.set_recent_names(names);
            // Re-pin the current type after the rows rebuild.
            self.re_pin_current();
            cx.notify();
        }

        /// Set the node's CURRENT composite (struct/enum) id so the popup opens
        /// pre-highlighting that composite row (the C++ `setTypes` with a Composite
        /// `m_currentEntry`). Re-pins the selection immediately. Pass 0 to clear.
        pub fn set_current_struct(&mut self, struct_id: u64, cx: &mut Context<Self>) {
            self.current_struct_id = struct_id;
            self.re_pin_current();
            cx.notify();
        }

        /// Pre-select the row for the node's current type — a composite by
        /// `current_struct_id` when set (the C++ structId match), else the
        /// primitive `current` kind. Shared by every rows-rebuild path.
        fn re_pin_current(&mut self) {
            if self.current_struct_id != 0 {
                if self.model.select_struct(self.current_struct_id).is_some() {
                    return;
                }
            }
            self.model.select_kind(self.current);
        }

        /// Toggle a category chip (Hex/Int/Float/Ptr) — delegated to the model,
        /// which now does the filtering (chip-hidden rows are excluded from the
        /// scored/bucketed list rather than hidden at render). Re-pins the current
        /// selection after the rows rebuild.
        fn toggle_group(&mut self, group: KindGroup, cx: &mut Context<Self>) {
            self.model.toggle_group(group);
            self.re_pin_current();
            cx.notify();
        }

        /// "all" — clear the category filter (every group shown), via the model.
        fn select_all_groups(&mut self, cx: &mut Context<Self>) {
            self.model.select_all_groups();
            self.re_pin_current();
            cx.notify();
        }

        /// "none" — the C++ `noneBtn` keeps AT LEAST ONE group checked (the first
        /// chip stays on, the rest go off), so the list never goes fully empty.
        /// Delegated to the model.
        fn select_no_groups(&mut self, cx: &mut Context<Self>) {
            self.model.select_no_groups();
            self.re_pin_current();
            cx.notify();
        }

        /// Emit the chosen kind + the active modifier, then signal the host to
        /// close. The base kind is the selected row's primitive kind; the modifier
        /// is the active `*`/`**`/`[]` (or `None`).
        fn accept_selected(&mut self, cx: &mut Context<Self>) {
            let Some(entry) = self.model.selected_entry().cloned() else {
                return;
            };
            let modifier = match self.model.modifier() {
                Modifier::None => None,
                m => Some(m),
            };
            // Carry the full identity of the picked row (kind/entryKind/structId/
            // displayName) so the editor can build a faithful `TypePopupChoice` and
            // route through `apply_type_popup_result` — selecting an EXISTING
            // composite must reference it by `struct_id` (not materialize a bare
            // empty Struct), and a primitive must apply by kind. Mirrors the C++
            // `typeSelected(const TypeEntry&, ...)` payload (items 35/36/37).
            cx.emit(TypeSelectorEvent::Chosen {
                kind: entry.primitive_kind,
                modifier,
                create_new: false,
                entry_kind: entry.entry_kind,
                struct_id: entry.struct_id,
                display_name: entry.display_name.clone(),
            });
        }

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.select_row(row) {
                self.accept_selected(cx);
            }
        }

        /// A single click on a row only PREVIEWS the selection (item 6): it sets
        /// the selection so a modifier (`*` / `**` / `[]`) can be adjusted before
        /// confirming with OK / Enter / double-click. The C++ list selects on
        /// single click and accepts only on double-click / Enter / OK.
        fn select_row_preview(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.select_row(row) {
                self.scroll_selected_into_view();
                cx.notify();
            }
        }

        /// Hover over a row moves the selection highlight to it (item 2) so
        /// keyboard + mouse selection stay in sync (native-menu behavior).
        fn hover_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.selected() != Some(row) && self.model.select_row(row) {
                cx.notify();
            }
        }

        /// The "+ New" footer button (`createNewTypeRequested`,
        /// `typeselectorpopup.cpp:924`): create a brand-new struct/class and apply
        /// it to the node, carrying the active `*`/`**`/`[]` modifier. Emitted as a
        /// [`TypeSelectorEvent::Chosen`] with [`NodeKind::Struct`] — the editor's
        /// existing apply path (`change_node_kind` → struct) creates the new
        /// composite and applies it, so this works end-to-end without a new
        /// contract variant (keeping the editor's `Chosen`/`Cancel` match stable).
        fn create_new(&mut self, cx: &mut Context<Self>) {
            let modifier = match self.model.modifier() {
                Modifier::None => None,
                m => Some(m),
            };
            // "+ New" (item 15): emit a Struct-kinded Chosen with `create_new`
            // set, so the editor materializes a fresh populated `NewClass[_N]`
            // (8×Hex64) and embeds the node as an instance of it — rather than
            // applying a bare empty `Struct` primitive (which rendered an empty
            // body that the arrow keys could not descend into).
            cx.emit(TypeSelectorEvent::Chosen {
                kind: NodeKind::Struct,
                modifier,
                create_new: true,
                // "+ New" makes a fresh composite; the editor's create-new path
                // materializes it and fills in the struct_id, so the carried
                // identity here is an empty composite placeholder.
                entry_kind: EntryKind::Composite,
                struct_id: 0,
                display_name: String::new(),
            });
        }

        fn theme(&self, cx: &App) -> Theme {
            // The popover tints rows from our theme; pull the current one from the
            // app-shared manager so chip/group colors match the editor.
            crate::ui::theme_apply::ThemeRegistryGlobal::current(cx)
        }

        /// Keyboard navigation (`typeselectorpopup.cpp:1881` `eventFilter`):
        /// Up/Down move the selection (skipping section headers), Enter accepts the
        /// selected type, Esc cancels, Ctrl+F focuses the filter input. Returns
        /// `true` when handled so the caller stops propagation.
        fn handle_nav_key(
            &mut self,
            key: &str,
            modifiers: &Modifiers,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> bool {
            // Trap Tab / Shift+Tab inside the popup so focus can't leave it for a
            // widget behind the modal scrim. The popup is self-contained (filter +
            // category row + list), driven by the keys handled below — never Tab —
            // so swallowing it costs nothing. (Shift+Tab arrives as key "tab" with
            // shift in the modifiers.)
            if key == "tab" {
                return true;
            }
            // Ctrl+F focuses the filter from anywhere AND selects all its text so
            // typing replaces the query (item 12; the C++ `selectAll()` on focus).
            if key == "f" && modifiers.control {
                self.input.update(cx, |input, cx| input.focus(window, cx));
                // Route the input's own SelectAll (ctrl-a) so the existing text is
                // selected — `InputState::select_all` is not public, so dispatch
                // the keystroke to the now-focused input.
                if let Ok(ks) = Keystroke::parse("ctrl-a") {
                    window.dispatch_keystroke(ks, cx);
                }
                cx.notify();
                return true;
            }
            // One visible page ≈ the 520px max popup minus chrome over a 26px row.
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
                    self.accept_selected(cx);
                    true
                }
                "escape" => {
                    cx.emit(TypeSelectorEvent::Cancel);
                    true
                }
                _ => false,
            }
        }
    }

    impl Focusable for TypeSelectorPopup {
        /// Return the FILTER INPUT's focus handle so the editor's
        /// `window.focus(popup.focus_handle)` lands on the input — opening the
        /// picker and immediately typing filters (B2 fix 1). The capture-phase key
        /// handler on the popup surface still receives Up/Down/Enter/Esc because it
        /// is an ancestor of the focused input.
        fn focus_handle(&self, cx: &App) -> FocusHandle {
            self.input.read(cx).focus_handle(cx)
        }
    }

    impl EventEmitter<TypeSelectorEvent> for TypeSelectorPopup {}

    impl Render for TypeSelectorPopup {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = self.theme(cx);
            let selected = self.model.selected();
            let muted = color::text_muted(cx);
            let accent = color::accent(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);
            let query = self.input.read(cx).value().to_string();
            let filtering = !query.trim().is_empty();

            // The widest primitive (16B = Hex128/Int128) anchors the size width-bar
            // so each row's bar is proportional to its byte size (the C++ delegate's
            // little size bar). dyn (0B) shows no bar.
            const MAX_SIZE_B: f32 = 16.0;

            let rows: Vec<AnyElement> = self
                .model
                .rows()
                .iter()
                .enumerate()
                // Category-chip filtering now happens in the model (chip-hidden
                // rows are excluded from `model.rows()`), so the rendered children
                // map 1:1 onto the model rows — no render-time filter needed.
                .map(|(row, r)| {
                    if r.entry.entry_kind == EntryKind::Section {
                        // A Zed section caption: a colored group dot + uppercase
                        // micro label, muted.
                        let group = r.entry_group_for_label();
                        let dot = group.map(|g| {
                            crate::ui::theme_apply::to_hsla(super::kind_group_color(g, &theme))
                        });
                        gpui_component::h_flex()
                            .w_full()
                            .px(px(tokens::space::MD))
                            .pt(px(tokens::space::MD))
                            .pb(px(tokens::space::XS))
                            .gap(px(tokens::space::SM))
                            .items_center()
                            .when_some(dot, |d, c| d.child(div().size(px(6.)).rounded_full().bg(c)))
                            .child(
                                div()
                                    .text_size(px(tokens::font::UI_XS))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(muted)
                                    .child(r.entry.display_name.to_uppercase()),
                            )
                            .into_any_element()
                    } else {
                        let group_color = crate::ui::theme_apply::to_hsla(super::kind_group_color(
                            r.entry.group,
                            &theme,
                        ));
                        let is_sel = selected == Some(row);
                        // The node's CURRENT type — the C++ pre-selects it as the
                        // initial highlight rather than painting a separate, static
                        // ring. We mark it only when it is NOT the moving selection,
                        // and as a subtle dotted edge that can never be mistaken for
                        // the (now strongly-painted) active selection (B2: the static
                        // ring on `current` used to read as a "frozen" highlight).
                        let is_current = !is_sel
                            && r.entry.primitive_kind == self.current
                            && r.entry.entry_kind != EntryKind::Composite;
                        // Selected rows use the C++ readout: full `selected` fill +
                        // the group-color name turns to plain text. Keep the group
                        // color when not selected.
                        let name_color = if !r.entry.enabled {
                            color::text_disabled(cx)
                        } else if is_sel {
                            color::text(cx)
                        } else {
                            group_color
                        };
                        let name_spans = if filtering {
                            highlighted_spans(
                                &r.entry.display_name,
                                &r.match_positions,
                                name_color,
                                accent,
                            )
                        } else {
                            vec![div()
                                .text_color(name_color)
                                .child(r.entry.display_name.clone())
                                .into_any_element()]
                        };
                        let size_label = if r.entry.size_bytes > 0 {
                            format!("{}B", r.entry.size_bytes)
                        } else {
                            "dyn".to_string()
                        };
                        // Proportional width bar (max 40px) for the byte size.
                        let bar_w = if r.entry.size_bytes > 0 {
                            (r.entry.size_bytes as f32 / MAX_SIZE_B * 40.).clamp(4., 40.)
                        } else {
                            0.
                        };
                        // The composite keyword chip (struct/class/enum), shown muted.
                        let keyword = r.entry.class_keyword.clone();
                        gpui_component::h_flex()
                            .id(("type-row", row))
                            .relative()
                            .w_full()
                            .h(px(26.))
                            .px(px(tokens::space::MD))
                            .gap(px(tokens::space::MD))
                            .items_center()
                            .rounded(px(tokens::radius::MD))
                            .text_size(px(tokens::font::UI_MD))
                            // The MOVING selection (keyboard cursor / hover / click)
                            // — a full `selected` fill so the highlight is plainly
                            // visible as it moves (B2: the previous fill was too
                            // faint to register; the C++ delegate fills the whole row
                            // with `t.selected`). A bold group-color left bar mirrors
                            // the C++ `kAccent` accent stripe so the active row also
                            // reads its group at a glance.
                            .when(is_sel, |d| d.bg(sel_bg))
                            .when(is_sel, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top_0()
                                        .bottom_0()
                                        .w(px(2.5))
                                        .rounded_l(px(tokens::radius::MD))
                                        .bg(group_color),
                                )
                            })
                            // The node's current type, when it is NOT the active
                            // selection: a faint dotted outline that is clearly
                            // distinct from the solid selection fill so it can never
                            // be mistaken for a "frozen" highlight.
                            .when(is_current, |d| {
                                d.border_1().border_dashed().border_color(muted)
                            })
                            .when(!is_sel && r.entry.enabled, |d| d.hover(|s| s.bg(hover_bg)))
                            .when(r.entry.enabled, |d| d.cursor_pointer())
                            // Hover-to-select (item 2): keep keyboard + mouse
                            // selection in sync so Enter confirms the hovered row.
                            .when(r.entry.enabled, |d| {
                                d.on_mouse_move(cx.listener(move |this, _e, _w, cx| {
                                    this.hover_row(row, cx);
                                }))
                            })
                            // Single click = PREVIEW (select only); double click =
                            // confirm (item 6) so a modifier can be adjusted first.
                            .when(r.entry.enabled, |d| {
                                d.on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                                    if e.click_count() >= 2 {
                                        this.accept_row(row, cx);
                                    } else {
                                        this.select_row_preview(row, cx);
                                    }
                                }))
                            })
                            // Leading colored kind chip (the SVG glyph for the group).
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(group_color)
                                    .child(entry_icon(&r.entry).size_3()),
                            )
                            .child(
                                gpui_component::h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .font_family(tokens::font::mono_family())
                                    .text_size(px(tokens::font::EDITOR_SIZE))
                                    .children(name_spans),
                            )
                            .when(!keyword.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(tokens::font::UI_XS))
                                        .text_color(muted)
                                        .child(keyword),
                                )
                            })
                            // Right-aligned size: a proportional width bar + the
                            // "NB" label.
                            .child(
                                gpui_component::h_flex()
                                    .flex_none()
                                    .gap(px(tokens::space::SM))
                                    .items_center()
                                    .justify_end()
                                    .child(div().w(px(40.)).h(px(4.)).flex().justify_end().when(
                                        bar_w > 0.,
                                        |d| {
                                            d.child(
                                                div()
                                                    .w(px(bar_w))
                                                    .h(px(4.))
                                                    .rounded_full()
                                                    .bg(group_color),
                                            )
                                        },
                                    ))
                                    .child(
                                        div()
                                            .min_w(px(28.))
                                            .text_size(px(tokens::font::UI_XS))
                                            .text_color(muted)
                                            .child(size_label),
                                    ),
                            )
                            .into_any_element()
                    }
                })
                .collect();

            crate::ui::design::elevated_surface(cx)
                .id("rcx-type-selector")
                .track_focus(&self.focus_handle)
                .key_context("RcxTypeSelector")
                // Capture-phase key handling so Up/Down/Enter/Esc/Ctrl+F drive the
                // list even when the filter input owns focus (the C++ `eventFilter`
                // that forwarded these from the line-edit to the list).
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if this.handle_nav_key(
                        ev.keystroke.key.as_str(),
                        &ev.keystroke.modifiers,
                        window,
                        cx,
                    ) {
                        cx.stop_propagation();
                    }
                }))
                .flex()
                .flex_col()
                .w(px(380.))
                .max_h(px(520.))
                .text_size(px(tokens::font::UI_MD))
                // Search row with a trailing × close.
                .child(
                    gpui_component::h_flex()
                        .px(px(tokens::space::MD))
                        .py(px(tokens::space::MD))
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
                        .child(div().flex_1().child(Input::new(&self.input).w_full()))
                        .child(
                            div()
                                .id("type-close")
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .rounded(px(tokens::radius::MD))
                                .text_color(muted)
                                .cursor_pointer()
                                .hover(|s| s.bg(hover_bg).text_color(color::text(cx)))
                                .on_click(cx.listener(|_this, _e, _w, cx| {
                                    cx.emit(TypeSelectorEvent::Cancel)
                                }))
                                .child(icon::close().size_3()),
                        ),
                )
                .child(self.render_category_tabs(&theme, cx))
                .child(self.render_column_header(cx))
                .child(
                    // A SCROLLABLE list (B2 fix 2 / item 9): the keyboard-selected
                    // row scrolls into view via `list_scroll.scroll_to_item` over
                    // the rendered children. Rows are variable-height (section
                    // headers vs entries) so a `ScrollHandle` + `overflow_y_scroll`
                    // fits where a uniform list would not.
                    gpui_component::v_flex()
                        .id("rcx-type-selector-list")
                        .p(px(tokens::space::XS))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.list_scroll)
                        .children(rows),
                )
                .child(self.render_footer(cx))
        }
    }

    impl TypeSelectorPopup {
        /// The CATEGORY FILTER TABS row: "● Hex (5)  ● Int (11)  ● Float (3)  ● Ptr
        /// (4)" colored chips with live counts, plus the "all / none / N types"
        /// trailing controls (the C++ category filter bar).
        fn render_category_tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
            let muted = color::text_muted(cx);
            let fg = color::text(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);

            let filtering = !self.input.read(cx).value().trim().is_empty();
            // Per-group TOTAL count over the catalogue.
            let total_of = |g: KindGroup| -> usize {
                self.model
                    .entries()
                    .iter()
                    .filter(|e| e.group == g && e.selectable())
                    .count()
            };
            // Per-group VISIBLE count: when filtering, only the rows that survived
            // the fuzzy filter (the C++ shows "visible / total"); else the total.
            let visible_of = |g: KindGroup| -> usize {
                if filtering {
                    self.model
                        .rows()
                        .iter()
                        .filter(|r| r.entry.selectable() && r.entry.group == g)
                        .count()
                } else {
                    total_of(g)
                }
            };
            let total = self
                .model
                .entries()
                .iter()
                .filter(|e| e.selectable())
                .count();

            // The chip label shows "Group (visible/total)" while filtering, else
            // "Group (total)" — matching the C++ CategoryChip count semantics.
            let chip = |group: KindGroup, color: Hsla, on: bool| -> AnyElement {
                let vis = visible_of(group);
                let tot = total_of(group);
                let count_label = if filtering && vis != tot {
                    format!("{} ({vis}/{tot})", group.key())
                } else {
                    format!("{} ({tot})", group.key())
                };
                gpui_component::h_flex()
                    .id(SharedString::from(format!("cat-{}", group.key())))
                    .h(px(20.))
                    .px(px(tokens::space::SM))
                    .gap(px(tokens::space::XS))
                    .items_center()
                    .rounded(px(tokens::radius::SM))
                    .cursor_pointer()
                    .when(on, |d| d.bg(sel_bg))
                    .when(!on, |d| d.hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _e, _w, cx| this.toggle_group(group, cx)))
                    .child(div().size(px(6.)).rounded_full().bg(color))
                    .child(
                        div()
                            .text_size(px(tokens::font::UI_XS))
                            .text_color(if on { fg } else { muted })
                            .child(count_label),
                    )
                    .into_any_element()
            };

            let group_color = |g: KindGroup| -> Hsla {
                crate::ui::theme_apply::to_hsla(super::kind_group_color(g, theme))
            };
            let on = |g: KindGroup| self.model.group_active(g);

            // A small "all" / "none" text control.
            let text_btn = |id: &'static str,
                            label: &'static str,
                            active: bool,
                            f: fn(&mut Self, &mut Context<Self>)|
             -> AnyElement {
                div()
                    .id(id)
                    .px(px(tokens::space::SM))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .rounded(px(tokens::radius::SM))
                    .text_size(px(tokens::font::UI_XS))
                    .text_color(if active { fg } else { muted })
                    .cursor_pointer()
                    .when(active, |d| d.bg(sel_bg))
                    .when(!active, |d| d.hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _e, _w, cx| f(this, cx)))
                    .child(label.to_string())
                    .into_any_element()
            };

            let active_groups = self.model.active_groups();
            // "all" lit when every chip-bearing group is checked (the C++ default).
            let all_active = KindGroup::ALL
                .iter()
                .filter(|g| g.has_chip())
                .all(|g| active_groups.contains(g.key()));
            // "none" = exactly the first chip-bearing group (Hex) is active — the
            // C++ keeps one group on (it never goes fully empty).
            let none_active =
                active_groups.len() == 1 && active_groups.contains(KindGroup::Hex.key());

            gpui_component::h_flex()
                .w_full()
                .px(px(tokens::space::MD))
                .py(px(tokens::space::SM))
                .gap(px(tokens::space::SM))
                .items_center()
                .flex_wrap()
                .border_b_1()
                .border_color(color::border(cx))
                .child(chip(
                    KindGroup::Hex,
                    group_color(KindGroup::Hex),
                    on(KindGroup::Hex),
                ))
                .child(chip(
                    KindGroup::Int,
                    group_color(KindGroup::Int),
                    on(KindGroup::Int),
                ))
                .child(chip(
                    KindGroup::Float,
                    group_color(KindGroup::Float),
                    on(KindGroup::Float),
                ))
                .child(chip(
                    KindGroup::Ptr,
                    group_color(KindGroup::Ptr),
                    on(KindGroup::Ptr),
                ))
                .child(div().flex_1())
                .child(text_btn(
                    "cat-all",
                    "all",
                    all_active,
                    Self::select_all_groups,
                ))
                .child(text_btn(
                    "cat-none",
                    "none",
                    none_active,
                    Self::select_no_groups,
                ))
                .child(
                    div()
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(muted)
                        .child(if filtering {
                            let shown = self
                                .model
                                .rows()
                                .iter()
                                .filter(|r| r.entry.selectable())
                                .count();
                            format!("{shown} of {total}")
                        } else {
                            format!("{total} types")
                        }),
                )
        }

        /// The column header with the working `group` / `name` / `size` sort
        /// toggles (the C++ list-header sort toolbar, `typeselectorpopup.cpp:640`).
        /// Clicking a sort key re-sorts the list; re-clicking the active key flips
        /// the direction (shown with an ↑/↓ arrow). The trailing list/grid icons
        /// are the layout affordances mirroring the C++ density toggle.
        fn render_column_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
            let muted = color::text_muted(cx);
            let fg = color::text(cx);
            let accent = color::accent(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let active_mode = self.model.sort_mode();
            let dir_arrow = if self.model.sort_dir() >= 0 {
                " \u{2191}"
            } else {
                " \u{2193}"
            };

            // A clickable sort-key text button.
            let sort_btn = |id: &'static str, label: &'static str, mode: SortMode| -> AnyElement {
                let is_active = active_mode == mode;
                let text = if is_active {
                    format!("{label}{dir_arrow}")
                } else {
                    label.to_string()
                };
                div()
                    .id(id)
                    .px(px(tokens::space::SM))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(tokens::radius::SM))
                    .text_color(if is_active { accent } else { muted })
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(sel_bg))
                    .when(!is_active, |d| d.hover(|s| s.bg(hover_bg).text_color(fg)))
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.model.set_sort_mode(mode);
                        cx.notify();
                    }))
                    .child(text)
                    .into_any_element()
            };

            // A layout-toggle icon button: sets the bucketed (Group) layout vs a
            // flat name-sorted layout (the C++ density/layout toggle).
            let layout_btn = |id: &'static str, ic: Icon, mode: SortMode| -> AnyElement {
                let is_active = active_mode == mode;
                div()
                    .id(id)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .rounded(px(tokens::radius::SM))
                    .text_color(if is_active { accent } else { muted })
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(sel_bg))
                    .when(!is_active, |d| d.hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        this.model.set_sort_mode(mode);
                        cx.notify();
                    }))
                    .child(ic.size_3())
                    .into_any_element()
            };

            gpui_component::h_flex()
                .w_full()
                .h(px(20.))
                .px(px(tokens::space::MD))
                .gap(px(tokens::space::XS))
                .items_center()
                .text_size(px(tokens::font::UI_XS))
                .text_color(muted)
                .child(sort_btn("col-sort-group", "group", SortMode::Group))
                .child(sort_btn("col-sort-name", "name", SortMode::Name))
                .child(sort_btn("col-sort-size", "size", SortMode::Size))
                .child(div().flex_1())
                // Layout toggles: bucketed sections (Group) vs a flat list (Name).
                .child(layout_btn(
                    "col-layout-list",
                    Icon::new(IconName::Menu),
                    SortMode::Group,
                ))
                .child(layout_btn(
                    "col-layout-grid",
                    Icon::new(IconName::LayoutDashboard),
                    SortMode::Name,
                ))
        }

        /// The footer: "<curtype> · <size>" on the left + the MODIFIER buttons
        /// `*` `**` `[]`, a "+ New" button, and a primary blue "OK".
        fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
            use gpui_component::button::{Button, ButtonVariants as _};
            let muted = color::text_muted(cx);
            let fg = color::text(cx);
            let accent = color::accent(cx);
            let hover_bg = color::hover_overlay(cx);
            let sel_bg = color::selected_bg(cx);
            let border = color::border(cx);
            let active = self.model.modifier();

            // The current selection summary "<full> · <resulting-size> (±diff)"
            // — computes the size AFTER the active modifier (ptr → pointer size,
            // array → base*n) and a diff vs the node's current size (item 13).
            let summary = self
                .model
                .selected_entry()
                .map(|e| {
                    let base = e.size_bytes;
                    let result = match self.model.modifier() {
                        Modifier::Pointer | Modifier::PointerPointer => self.pointer_size,
                        Modifier::Array(n) if base > 0 => base * n.max(1),
                        _ => base,
                    };
                    let size = if result > 0 {
                        format!("{result}B")
                    } else {
                        "dyn".to_string()
                    };
                    let full = self
                        .model
                        .full_text()
                        .unwrap_or_else(|| e.display_name.clone());
                    let mut s = format!("{full} · {size}");
                    if result > 0 && self.current_node_size > 0 && result != self.current_node_size
                    {
                        let diff = result - self.current_node_size;
                        let sign = if diff > 0 { "+" } else { "" };
                        s.push_str(&format!(" ({sign}{diff})"));
                    }
                    s
                })
                .unwrap_or_else(|| "—".to_string());

            // A modifier toggle chip.
            let chip = |id: &'static str, label: &'static str, is_on: bool, modifier: Modifier| {
                gpui_component::h_flex()
                    .id(id)
                    .h(px(24.))
                    .min_w(px(30.))
                    .px(px(tokens::space::MD))
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::radius::MD))
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(if is_on { accent } else { fg })
                    .when(is_on, |d| d.bg(sel_bg).font_weight(FontWeight::SEMIBOLD))
                    .when(!is_on, |d| d.cursor_pointer().hover(|s| s.bg(hover_bg)))
                    .on_click(cx.listener(move |this, _e, _w, cx| {
                        let next = if this.model.modifier() == modifier {
                            Modifier::None
                        } else {
                            modifier
                        };
                        this.model.set_modifier(next);
                        cx.notify();
                    }))
                    .child(label.to_string())
                    .into_any_element()
            };
            let array_on = matches!(active, Modifier::Array(_));
            let modifiers_allowed = self.model.mode().allows_modifiers();

            gpui_component::v_flex()
                .w_full()
                .border_t_1()
                .border_color(border)
                .child(
                    // The summary line.
                    div()
                        .w_full()
                        .px(px(tokens::space::MD))
                        .pt(px(tokens::space::SM))
                        .font_family(tokens::font::mono_family())
                        .text_size(px(tokens::font::UI_XS))
                        .text_color(muted)
                        .child(summary),
                )
                .child(
                    gpui_component::h_flex()
                        .w_full()
                        .px(px(tokens::space::MD))
                        .py(px(tokens::space::SM))
                        .gap(px(tokens::space::XS))
                        .items_center()
                        .when(modifiers_allowed, |d| {
                            // The `[]` chip: toggling it on seeds the count box to
                            // "1" and focuses it; the box feeds Modifier::Array(n).
                            let array_chip = gpui_component::h_flex()
                                .id("mod-array")
                                .h(px(24.))
                                .min_w(px(30.))
                                .px(px(tokens::space::MD))
                                .items_center()
                                .justify_center()
                                .rounded(px(tokens::radius::MD))
                                .text_size(px(tokens::font::UI_SM))
                                .text_color(if array_on { accent } else { fg })
                                .when(array_on, |d| d.bg(sel_bg).font_weight(FontWeight::SEMIBOLD))
                                .when(!array_on, |d| d.cursor_pointer().hover(|s| s.bg(hover_bg)))
                                .on_click(cx.listener(move |this, _e, window, cx| {
                                    if matches!(this.model.modifier(), Modifier::Array(_)) {
                                        this.model.set_modifier(Modifier::None);
                                    } else {
                                        // Toggling `[]` on seeds the count box to "1"
                                        // (the C++ defaults the count to 1) + focuses
                                        // it so the user can type a new count.
                                        if this.array_count_input.read(cx).value().trim().is_empty()
                                        {
                                            this.array_count_input
                                                .update(cx, |i, cx| i.set_value("1", window, cx));
                                        }
                                        let n = this.array_count_value(cx);
                                        this.model.set_modifier(Modifier::Array(n));
                                        this.array_count_input
                                            .update(cx, |i, cx| i.focus(window, cx));
                                    }
                                    cx.notify();
                                }))
                                .child("[]");
                            d.child(chip(
                                "mod-ptr",
                                "*",
                                active == Modifier::Pointer,
                                Modifier::Pointer,
                            ))
                            .child(chip(
                                "mod-ptrptr",
                                "**",
                                active == Modifier::PointerPointer,
                                Modifier::PointerPointer,
                            ))
                            .child(array_chip)
                            // The array element COUNT box (item 8) — visible only
                            // when the array modifier is active; feeds Array(n).
                            .when(array_on, |d| {
                                d.child(
                                    div()
                                        .w(px(52.))
                                        .child(Input::new(&self.array_count_input).w_full()),
                                )
                            })
                        })
                        .child(
                            Button::new("type-new")
                                .ghost()
                                .small()
                                .icon(IconName::Plus)
                                .label("New")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.create_new(cx);
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("type-ok")
                                .primary()
                                .small()
                                .label("OK")
                                .on_click(cx.listener(|this, _e, _w, cx| {
                                    this.accept_selected(cx);
                                })),
                        ),
                )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        kind_group_for, EntryKind, KindGroup, Modifier, SortMode, TypeEntry, TypeModel,
        TypePopupMode,
    };
    use crate::controller::parse_type_spec;
    use crate::core::kind::NodeKind;

    // ── parse_type_spec (test_type_selector.cpp) ──

    #[test]
    fn parse_plain_type() {
        let s = parse_type_spec("int32_t");
        assert_eq!(s.base_name, "int32_t");
        assert!(!s.is_pointer);
        assert_eq!(s.array_count, 0);
    }

    #[test]
    fn parse_array() {
        let s = parse_type_spec("int32_t[10]");
        assert_eq!(s.base_name, "int32_t");
        assert_eq!(s.array_count, 10);
    }

    #[test]
    fn parse_array_zero_count_stays_zero() {
        // "[0]" → arrayCount 0 (the C++ only sets it for count > 0).
        let s = parse_type_spec("int32_t[0]");
        assert_eq!(s.base_name, "int32_t");
        assert_eq!(s.array_count, 0);
    }

    #[test]
    fn parse_single_pointer() {
        let s = parse_type_spec("Ball*");
        assert!(s.is_pointer);
        assert_eq!(s.ptr_depth, 1);
        assert_eq!(s.base_name, "Ball");
    }

    #[test]
    fn parse_double_pointer() {
        let s = parse_type_spec("Ball**");
        assert!(s.is_pointer);
        assert_eq!(s.ptr_depth, 2);
        assert_eq!(s.base_name, "Ball");
    }

    #[test]
    fn parse_empty_is_empty() {
        let s = parse_type_spec("");
        assert_eq!(s.base_name, "");
        assert!(!s.is_pointer);
        assert_eq!(s.array_count, 0);
    }

    #[test]
    fn parse_pointer_with_surrounding_space() {
        // "  Ball *  " → pointer, base "Ball".
        let s = parse_type_spec("  Ball *  ");
        assert!(s.is_pointer);
        assert_eq!(s.ptr_depth, 1);
        assert_eq!(s.base_name, "Ball");
    }

    // ── kind_group_for ──

    #[test]
    fn kind_groups_bucket_correctly() {
        assert_eq!(kind_group_for(NodeKind::Hex64), KindGroup::Hex);
        assert_eq!(kind_group_for(NodeKind::Int32), KindGroup::Int);
        assert_eq!(kind_group_for(NodeKind::UInt64), KindGroup::Int);
        assert_eq!(kind_group_for(NodeKind::Bool), KindGroup::Int);
        assert_eq!(kind_group_for(NodeKind::Float), KindGroup::Float);
        assert_eq!(kind_group_for(NodeKind::Double), KindGroup::Float);
        assert_eq!(kind_group_for(NodeKind::Pointer64), KindGroup::Ptr);
        assert_eq!(kind_group_for(NodeKind::FuncPtr32), KindGroup::Ptr);
        assert_eq!(kind_group_for(NodeKind::Vec3), KindGroup::Vec);
        assert_eq!(kind_group_for(NodeKind::Mat4x4), KindGroup::Vec);
        assert_eq!(kind_group_for(NodeKind::UTF8), KindGroup::Str);
        assert_eq!(kind_group_for(NodeKind::Struct), KindGroup::Ctr);
    }

    #[test]
    fn group_order_and_labels() {
        assert_eq!(KindGroup::ALL.len(), 8);
        assert_eq!(KindGroup::ALL[0], KindGroup::Hex);
        assert_eq!(KindGroup::ALL[7], KindGroup::Common);
        assert_eq!(KindGroup::Int.section_label(), "Int / Bool");
        assert_eq!(KindGroup::Ptr.section_label(), "Pointer / FuncPtr");
        assert_eq!(KindGroup::Ctr.section_label(), "Type");
        // Chips only on Hex/Int/Float/Ptr.
        assert!(KindGroup::Hex.has_chip());
        assert!(!KindGroup::Vec.has_chip());
    }

    // ── TypeModel filter ──

    fn sample_entries() -> Vec<TypeEntry> {
        vec![
            TypeEntry::primitive(NodeKind::Hex64, "hex64"),
            TypeEntry::primitive(NodeKind::Hex8, "hex8"),
            TypeEntry::primitive(NodeKind::Int32, "int32_t"),
            TypeEntry::primitive(NodeKind::UInt32, "uint32_t"),
            TypeEntry::primitive(NodeKind::Float, "float"),
            TypeEntry::primitive(NodeKind::Pointer64, "ptr64"),
            TypeEntry::composite(100, "Player", "struct", 64),
        ]
    }

    #[test]
    fn empty_filter_inserts_section_headers() {
        let model = TypeModel::new(sample_entries());
        // rowCount > entries because section headers are inserted (the C++
        // testSetTypesInsertsSectionHeaders: rowCount > 2).
        assert!(model.row_count() > model.entries().len());
        // At least one Section row exists.
        assert!(model
            .rows()
            .iter()
            .any(|r| r.entry.entry_kind == EntryKind::Section));
        // The first selectable row is selected.
        let sel = model.selected().unwrap();
        assert!(model.rows()[sel].entry.selectable());
    }

    #[test]
    fn empty_filter_groups_in_fixed_order() {
        let model = TypeModel::new(sample_entries());
        // The section headers appear in KindGroup::ALL order.
        let sections: Vec<&str> = model
            .rows()
            .iter()
            .filter(|r| r.entry.entry_kind == EntryKind::Section)
            .map(|r| r.entry.display_name.as_str())
            .collect();
        // Hex before Int before Float before Ptr before Type (Ctr).
        let hex = sections.iter().position(|s| *s == "Hex").unwrap();
        let int = sections.iter().position(|s| *s == "Int / Bool").unwrap();
        let float = sections.iter().position(|s| *s == "Float").unwrap();
        assert!(hex < int && int < float);
    }

    #[test]
    fn filter_produces_flat_ranked_list() {
        let mut model = TypeModel::new(sample_entries());
        model.apply_filter("int");
        // No section headers when filtering.
        assert!(model
            .rows()
            .iter()
            .all(|r| r.entry.entry_kind != EntryKind::Section));
        // "int32_t" and "uint32_t" both match; the prefix one ranks first.
        assert_eq!(model.rows()[0].entry.display_name, "int32_t");
        // Match positions are populated for highlight painting.
        assert!(!model.rows()[0].match_positions.is_empty());
    }

    #[test]
    fn filter_ranks_camelcase_boundary_above_mid_word() {
        // The filtered list uses the branch-cap-4 recursive scorer
        // (`fuzzy::source_score`, byte-exact to typeselectorpopup.cpp:149
        // `fuzzyScore`). A CamelCase-boundary hit earns bonus 8; a mid-word
        // hit earns bonus 1, so for equal-length names the boundary match
        // ranks first. Names are equal length here so the tightness/exact
        // bonuses cancel and only the per-char boundary bonus decides order.
        let entries = vec![
            // "a" hits the mid-word lowercase 'a' (index 3, prev 'o') → bonus 1.
            TypeEntry::composite(1, "Fooabc", "struct", 16),
            // "a" hits the upper 'A' at a CamelCase boundary (index 3, prev
            // lower 'o') → bonus 8.
            TypeEntry::composite(2, "FooAbc", "struct", 16),
        ];
        let mut model = TypeModel::new(entries);
        model.apply_filter("a");
        let names: Vec<&str> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["FooAbc", "Fooabc"],
            "CamelCase-boundary match should outrank the mid-word match"
        );
        // And cross-check the underlying scorer directly: boundary > mid-word.
        let boundary = crate::ui::fuzzy::source_score("a", "FooAbc", None);
        let mid_word = crate::ui::fuzzy::source_score("a", "Fooabc", None);
        assert!(
            boundary > mid_word,
            "boundary {boundary} should beat mid-word {mid_word}"
        );
    }

    #[test]
    fn filter_no_match_shows_empty_state() {
        let mut model = TypeModel::new(sample_entries());
        model.apply_filter("zzzzz");
        assert_eq!(model.row_count(), 1);
        assert_eq!(model.rows()[0].entry.entry_kind, EntryKind::Section);
        assert!(model.rows()[0]
            .entry
            .display_name
            .contains("No types match"));
        assert_eq!(model.selected(), None);
    }

    #[test]
    fn set_mode_resets_modifier() {
        // testSetModeResetsModifierInPointerTargetMode.
        let mut model = TypeModel::new(sample_entries());
        model.set_modifier(Modifier::Pointer);
        assert_eq!(model.modifier().checked_id(), 1);
        model.set_mode(TypePopupMode::PointerTarget);
        assert_eq!(model.modifier(), Modifier::None);
        assert!(!model.mode().allows_modifiers());
    }

    #[test]
    fn filter_placeholder_tracks_mode_and_count() {
        // P12 / C++ parity (typeselectorpopup.cpp:1256-1267): the placeholder noun
        // follows the mode and carries the total non-section entry count (7 here),
        // not the live-filtered subset.
        let mut model = TypeModel::new(sample_entries());
        model.set_mode(TypePopupMode::Root);
        assert_eq!(model.filter_placeholder(), "Filter 7 structs..  (Ctrl+F)");
        model.set_mode(TypePopupMode::FieldType);
        assert_eq!(model.filter_placeholder(), "Filter 7 types..  (Ctrl+F)");
        model.set_mode(TypePopupMode::ArrayElement);
        assert_eq!(
            model.filter_placeholder(),
            "Filter 7 element types..  (Ctrl+F)"
        );
        model.set_mode(TypePopupMode::PointerTarget);
        assert_eq!(model.filter_placeholder(), "Filter 7 targets..  (Ctrl+F)");
        // The count ignores the live filter — narrowing to one match keeps "7".
        model.apply_filter("int32");
        assert_eq!(model.filter_placeholder(), "Filter 7 targets..  (Ctrl+F)");
    }

    #[test]
    fn set_modifier_checked_ids() {
        let mut model = TypeModel::new(sample_entries());
        model.set_modifier(Modifier::Pointer);
        assert_eq!(model.modifier().checked_id(), 1);
        model.set_modifier(Modifier::PointerPointer);
        assert_eq!(model.modifier().checked_id(), 2);
        model.set_modifier(Modifier::Array(5));
        assert_eq!(model.modifier().checked_id(), 3);
    }

    #[test]
    fn full_text_appends_modifier_suffix() {
        let mut model = TypeModel::new(sample_entries());
        model.apply_filter("int32_t");
        // Select the matching row.
        assert!(model.select_row(0));
        assert_eq!(model.full_text().as_deref(), Some("int32_t"));
        model.set_modifier(Modifier::Pointer);
        assert_eq!(model.full_text().as_deref(), Some("int32_t*"));
        model.set_modifier(Modifier::Array(8));
        assert_eq!(model.full_text().as_deref(), Some("int32_t[8]"));
    }

    #[test]
    fn navigation_skips_section_headers() {
        let mut model = TypeModel::new(sample_entries());
        // Walk down through all rows; selection must never land on a section.
        for _ in 0..model.row_count() {
            model.move_down();
            let sel = model.selected().unwrap();
            assert!(model.rows()[sel].entry.selectable());
        }
    }

    #[test]
    fn cannot_select_a_section_row() {
        let model_entries = sample_entries();
        let mut model = TypeModel::new(model_entries);
        // Row 0 is the first section header → not selectable.
        let first_section = model
            .rows()
            .iter()
            .position(|r| r.entry.entry_kind == EntryKind::Section)
            .unwrap();
        assert!(!model.select_row(first_section));
    }

    // ── sort modes (defect 6: working sort toolbar) ──

    #[test]
    fn default_sort_mode_is_group_bucketed() {
        let model = TypeModel::new(sample_entries());
        assert_eq!(model.sort_mode(), SortMode::Group);
        // Group layout has section headers.
        assert!(model
            .rows()
            .iter()
            .any(|r| r.entry.entry_kind == EntryKind::Section));
    }

    #[test]
    fn name_sort_flattens_and_orders_by_name() {
        let mut model = TypeModel::new(sample_entries());
        model.set_sort_mode(SortMode::Name);
        assert_eq!(model.sort_mode(), SortMode::Name);
        // Flat list: no section headers.
        assert!(model
            .rows()
            .iter()
            .all(|r| r.entry.entry_kind != EntryKind::Section));
        // Ascending by display name.
        let names: Vec<&str> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[test]
    fn size_sort_orders_by_byte_size() {
        let mut model = TypeModel::new(sample_entries());
        model.set_sort_mode(SortMode::Size);
        let sizes: Vec<i32> = model.rows().iter().map(|r| r.entry.size_bytes).collect();
        let mut sorted = sizes.clone();
        sorted.sort();
        assert_eq!(sizes, sorted);
    }

    // ── page/home/end navigation (item 10) ──

    #[test]
    fn page_down_and_up_move_multiple_selectable_rows() {
        let mut model = TypeModel::new(sample_entries());
        let first = model.selected().unwrap();
        model.page_down(3);
        let after = model.selected().unwrap();
        assert!(after > first, "page_down advances past the first row");
        assert!(model.rows()[after].entry.selectable());
        // PageUp returns toward the top.
        model.page_up(3);
        assert!(model.rows()[model.selected().unwrap()].entry.selectable());
    }

    #[test]
    fn home_and_end_select_first_and_last_selectable() {
        let mut model = TypeModel::new(sample_entries());
        model.move_end();
        let last = model.selected().unwrap();
        assert!(model.rows()[last].entry.selectable());
        // End lands on the last selectable row (no later selectable row exists).
        assert!(model.rows()[last + 1..]
            .iter()
            .all(|r| !r.entry.selectable()));
        model.move_home();
        let first = model.selected().unwrap();
        assert!(model.rows()[first].entry.selectable());
        // Home lands on the first selectable row (no earlier selectable row).
        assert!(model.rows()[..first].iter().all(|r| !r.entry.selectable()));
    }

    // ── current-kind pre-select (B7 / item 7) ──

    #[test]
    fn select_kind_preselects_matching_primitive() {
        let mut model = TypeModel::new(sample_entries());
        let row = model.select_kind(NodeKind::Float).unwrap();
        assert_eq!(model.selected(), Some(row));
        let e = model.selected_entry().unwrap();
        assert_eq!(e.primitive_kind, NodeKind::Float);
        // A kind not present yields None and leaves the selection unchanged.
        let before = model.selected();
        assert_eq!(model.select_kind(NodeKind::Mat4x4), None);
        assert_eq!(model.selected(), before);
    }

    // ── composite current-entry pre-select by structId (items 38/39) ──

    #[test]
    fn select_struct_preselects_matching_composite_by_id() {
        let mut model = TypeModel::new(sample_entries());
        // The sample has a composite "Player" with struct_id 100.
        let row = model.select_struct(100).unwrap();
        assert_eq!(model.selected(), Some(row));
        let e = model.selected_entry().unwrap();
        assert_eq!(e.entry_kind, EntryKind::Composite);
        assert_eq!(e.struct_id, 100);
        assert_eq!(e.display_name, "Player");
        // A struct_id not present yields None and leaves the selection unchanged.
        let before = model.selected();
        assert_eq!(model.select_struct(999), None);
        assert_eq!(model.selected(), before);
        // struct_id 0 never matches (a primitive node uses select_kind instead).
        assert_eq!(model.select_struct(0), None);
    }

    // ── recent names (item 16) ──

    #[test]
    fn recent_names_surface_a_recent_section_first() {
        let mut model = TypeModel::new(sample_entries());
        model.set_recent_names(vec!["float".to_string(), "int32_t".to_string()]);
        // The first section header is "Recent".
        let first_section = model
            .rows()
            .iter()
            .find(|r| r.entry.entry_kind == EntryKind::Section)
            .unwrap();
        assert_eq!(first_section.entry.display_name, "Recent");
        // The recent entries follow it (before any other group section).
        let recent_pos = model
            .rows()
            .iter()
            .position(|r| r.entry.display_name == "Recent")
            .unwrap();
        assert_eq!(model.rows()[recent_pos + 1].entry.display_name, "float");
    }

    #[test]
    fn recent_names_empty_has_no_recent_section() {
        let model = TypeModel::new(sample_entries());
        assert!(model
            .rows()
            .iter()
            .all(|r| r.entry.display_name != "Recent"));
    }

    #[test]
    fn reclicking_active_sort_flips_direction() {
        let mut model = TypeModel::new(sample_entries());
        model.set_sort_mode(SortMode::Name);
        assert_eq!(model.sort_dir(), 1);
        let asc: Vec<String> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.clone())
            .collect();
        // Re-clicking the active mode flips to descending.
        model.set_sort_mode(SortMode::Name);
        assert_eq!(model.sort_dir(), -1);
        let mut desc: Vec<String> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.clone())
            .collect();
        desc.reverse();
        assert_eq!(asc, desc);
    }

    // ── within-group alphabetical-case-insensitive ordering (cpp:1690) ──

    /// Helper: the entry display names in the group section headed by `label`,
    /// in row order (stops at the next section header).
    fn group_section_names(model: &TypeModel, label: &str) -> Vec<String> {
        let rows = model.rows();
        let start = rows
            .iter()
            .position(|r| r.entry.entry_kind == EntryKind::Section && r.entry.display_name == label)
            .expect("section present");
        let mut out = Vec::new();
        for r in &rows[start + 1..] {
            if r.entry.entry_kind == EntryKind::Section {
                break;
            }
            out.push(r.entry.display_name.clone());
        }
        out
    }

    #[test]
    fn within_group_sorted_case_insensitive_alphabetical() {
        // The C++ uses a single case-insensitive alphabetical comparator for EVERY
        // group (`typeselectorpopup.cpp:1690`), including Hex — the old size-desc
        // Hex special case is gone. Build an Int group with mixed-case names whose
        // case-sensitive and case-insensitive orders differ.
        let entries = vec![
            TypeEntry::primitive(NodeKind::UInt32, "Zeta"),
            TypeEntry::primitive(NodeKind::Int32, "alpha"),
            TypeEntry::primitive(NodeKind::Int16, "Beta"),
            TypeEntry::primitive(NodeKind::UInt16, "gamma"),
        ];
        let model = TypeModel::new(entries);
        let names = group_section_names(&model, KindGroup::Int.section_label());
        assert_eq!(
            names,
            vec!["alpha", "Beta", "gamma", "Zeta"],
            "group must be case-insensitive alphabetical (not ASCII upper-before-lower)"
        );
    }

    #[test]
    fn hex_group_is_alphabetical_not_size_descending() {
        // Hex no longer sorts size-descending — it is alphabetical like the rest.
        let entries = vec![
            TypeEntry::primitive(NodeKind::Hex8, "hex8"),
            TypeEntry::primitive(NodeKind::Hex64, "hex64"),
            TypeEntry::primitive(NodeKind::Hex16, "hex16"),
            TypeEntry::primitive(NodeKind::Hex32, "hex32"),
        ];
        let model = TypeModel::new(entries);
        let names = group_section_names(&model, KindGroup::Hex.section_label());
        // Case-insensitive alphabetical over the strings (digit order):
        // "hex16" < "hex32" < "hex64" < "hex8".
        assert_eq!(names, vec!["hex16", "hex32", "hex64", "hex8"]);
    }

    // ── same-size-first (mode != Root && node_size > 0; cpp:1735-1745) ──

    #[test]
    fn same_size_first_when_mode_not_root_and_node_size_set() {
        // An Int group with two 4-byte and two 8-byte entries. With a node of size
        // 4 in a non-Root mode, the 4-byte entries lead (each part alphabetical).
        let entries = vec![
            TypeEntry::primitive(NodeKind::Int64, "bigB"),    // 8B
            TypeEntry::primitive(NodeKind::Int32, "smallA"),  // 4B
            TypeEntry::primitive(NodeKind::UInt64, "bigA"),   // 8B
            TypeEntry::primitive(NodeKind::UInt32, "smallB"), // 4B
        ];
        let mut model = TypeModel::new(entries);
        // Sanity: the chosen kinds really have the expected sizes.
        assert_eq!(crate::core::kind::size_for_kind(NodeKind::Int32), 4);
        assert_eq!(crate::core::kind::size_for_kind(NodeKind::Int64), 8);

        model.set_mode(TypePopupMode::FieldType);
        model.set_current_node_size(4);
        let names = group_section_names(&model, KindGroup::Int.section_label());
        assert_eq!(
            names,
            vec!["smallA", "smallB", "bigA", "bigB"],
            "size-4 entries lead (alphabetical), then the rest (alphabetical)"
        );

        // In Root mode the same-size-first gate is OFF → pure alphabetical.
        model.set_mode(TypePopupMode::Root);
        // set_mode clears node size? No — only the modifier. Re-assert ordering.
        let names_root = group_section_names(&model, KindGroup::Int.section_label());
        assert_eq!(names_root, vec!["bigA", "bigB", "smallA", "smallB"]);
    }

    #[test]
    fn same_size_first_off_when_node_size_zero() {
        let entries = vec![
            TypeEntry::primitive(NodeKind::Int64, "bigB"),
            TypeEntry::primitive(NodeKind::Int32, "smallA"),
        ];
        let mut model = TypeModel::new(entries);
        model.set_mode(TypePopupMode::FieldType);
        // node size 0 → no same-size partition, pure alphabetical.
        model.set_current_node_size(0);
        let names = group_section_names(&model, KindGroup::Int.section_label());
        assert_eq!(names, vec!["bigB", "smallA"]);
    }

    // ── model-side chip filtering (catAllowed; cpp:1684-1688) ──

    #[test]
    fn chip_off_excludes_group_from_bucketed_list() {
        let mut model = TypeModel::new(sample_entries());
        // All four chip groups are checked/visible by default (the C++ default —
        // all chips setChecked(true)).
        assert_eq!(model.active_groups().len(), 4);
        assert!(model
            .rows()
            .iter()
            .any(|r| r.entry.group == KindGroup::Int && r.entry.selectable()));
        // Turn the Int chip OFF: the C++ catAllowed excludes the unchecked group.
        model.toggle_group(KindGroup::Int); // active = {Hex, Float, Ptr}
                                            // Int is NOT in the active set → its rows are excluded.
        assert!(
            !model.rows().iter().any(|r| r.entry.group == KindGroup::Int),
            "chip-off Int group must be excluded from the bucketed list"
        );
        // A chip-less group (Ctr) is always visible (Player composite).
        assert!(model
            .rows()
            .iter()
            .any(|r| r.entry.group == KindGroup::Ctr && r.entry.selectable()));
    }

    #[test]
    fn chip_off_excludes_group_from_fuzzy_ranked_list() {
        // A fuzzy query with one chip OFF must drop that group's rows from the
        // ranked list (model-side catAllowed gate, cpp:1660).
        let entries = vec![
            // Both match the fuzzy query "t" but live in different groups.
            TypeEntry::primitive(NodeKind::Int32, "int32_t"), // Int group
            TypeEntry::primitive(NodeKind::Pointer64, "ptr64"), // Ptr group
            TypeEntry::composite(100, "Trophy", "struct", 8), // Ctr group (always on)
        ];
        let mut model = TypeModel::new(entries);
        // With all chips on, "t" matches int32_t, ptr64 and Trophy.
        model.apply_filter("t");
        let names: Vec<&str> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.as_str())
            .collect();
        assert!(names.contains(&"int32_t"));
        assert!(names.contains(&"ptr64"));
        // Turn the Int chip OFF: the Int group's "int32_t" must vanish from the
        // ranked list; Ptr's "ptr64" stays; the chip-less Ctr "Trophy" is always
        // allowed.
        model.toggle_group(KindGroup::Int); // active = {Hex, Float, Ptr}
        model.apply_filter("t");
        let names: Vec<&str> = model
            .rows()
            .iter()
            .map(|r| r.entry.display_name.as_str())
            .collect();
        assert!(
            !names.contains(&"int32_t"),
            "Int chip off → int32_t excluded from ranked list"
        );
        assert!(names.contains(&"ptr64"), "Ptr chip on → ptr64 still ranked");
        assert!(
            names.contains(&"Trophy"),
            "chip-less Ctr group always allowed"
        );
    }
}
