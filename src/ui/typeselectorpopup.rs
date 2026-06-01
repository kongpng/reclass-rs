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
//! - [`parse_type_spec`] / [`TypeSpec`] — the type-text parser (`parseTypeSpec`):
//!   `"int32_t[10]"` → array 10, `"Ball*"`/`"Ball**"` → pointer depth 1/2,
//!   `"int32_t[0]"` → array 0. Test-locked (`test_type_selector.cpp`).
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

/// `struct TypeSpec` (`typeselectorpopup.h:59`) — a parsed type text.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TypeSpec {
    /// The base type name (everything before the modifier suffix).
    pub base_name: String,
    /// Whether a pointer suffix (`*`/`**`) was present.
    pub is_pointer: bool,
    /// Pointer depth: 1 for `*`, 2 for `**`, 0 if not a pointer.
    pub ptr_depth: i32,
    /// Array element count (0 = not an array, also `[0]`).
    pub array_count: i32,
}

/// `parseTypeSpec(text)` (`typeselectorpopup.cpp:32-61`).
///
/// Trim. If it ends with `*`: pointer, chop, depth 1; if then ends with `*`:
/// chop, depth 2; base = remaining (trimmed). Else if `[` at index > 0 and ends
/// `]`: base = left of `[`, parse the count — **only `count > 0` sets
/// `array_count`** (so `[0]` → 0). Else base = whole string.
pub fn parse_type_spec(text: &str) -> TypeSpec {
    let mut spec = TypeSpec::default();
    let s = text.trim();
    if s.is_empty() {
        return spec;
    }

    // Pointer suffix.
    if let Some(stripped) = s.strip_suffix('*') {
        spec.is_pointer = true;
        spec.ptr_depth = 1;
        let stripped = if let Some(s2) = stripped.strip_suffix('*') {
            spec.ptr_depth = 2;
            s2
        } else {
            stripped
        };
        spec.base_name = stripped.trim().to_string();
        return spec;
    }

    // Array suffix: "base[count]".
    if let Some(bracket) = s.find('[') {
        if bracket > 0 && s.ends_with(']') {
            spec.base_name = s[..bracket].trim().to_string();
            let count_str = &s[bracket + 1..s.len() - 1];
            if let Ok(count) = count_str.trim().parse::<i32>() {
                if count > 0 {
                    spec.array_count = count;
                }
            }
            return spec;
        }
    }

    spec.base_name = s.to_string();
    spec
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
        };
        m.apply_filter("");
        m
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

    /// The current modifier.
    pub fn modifier(&self) -> Modifier {
        self.modifier
    }

    /// `setMode(mode)` (`typeselectorpopup.cpp:1086`): set the mode and **always
    /// clear the modifier** (the test `testSetModeResetsModifierInPointerTargetMode`).
    pub fn set_mode(&mut self, mode: TypePopupMode) {
        self.mode = mode;
        self.modifier = Modifier::None;
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
    /// flat list ranked by [`fuzzy_score`](super::fuzzy::fuzzy_score) desc (no
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
        if rows.is_empty() {
            rows.push(TypeRow {
                entry: TypeEntry::section("No types available"),
                match_positions: Vec::new(),
            });
        }
        self.rows = rows;
    }

    /// Build the empty-filter bucketed view: per-group sections in fixed order.
    fn build_bucketed(&mut self) {
        let mut rows: Vec<TypeRow> = Vec::new();
        for group in KindGroup::ALL {
            let mut group_entries: Vec<TypeEntry> = self
                .entries
                .iter()
                .filter(|e| e.group == group)
                .cloned()
                .collect();
            if group_entries.is_empty() {
                continue;
            }
            // Hex always sorts size-desc; others alphabetic for stability.
            if group == KindGroup::Hex {
                group_entries.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
            } else {
                group_entries.sort_by(|a, b| a.display_name.cmp(&b.display_name));
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
        if rows.is_empty() {
            rows.push(TypeRow {
                entry: TypeEntry::section("No types available"),
                match_positions: Vec::new(),
            });
        }
        self.rows = rows;
    }

    /// Build the filtered flat ranked view (fuzzy, no headers).
    fn build_filtered(&mut self, query: &str) {
        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let mut pos = Vec::new();
            let s = super::fuzzy::fuzzy_score(query, &e.display_name, Some(&mut pos));
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
        if rows.is_empty() {
            rows.push(TypeRow {
                entry: TypeEntry::section(&format!("No types match '{query}'")),
                match_positions: Vec::new(),
            });
        }
        self.rows = rows;
    }

    /// The first selectable row (`nextSelectableRow` from the top).
    fn first_selectable_row(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.entry.selectable())
    }

    /// `nextSelectableRow(from, dir)` (`typeselectorpopup.cpp:1778`): the next
    /// selectable row after `from` in direction `dir` (+1 down / -1 up), skipping
    /// sections + disabled. Returns `None` if none in that direction.
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

    /// Move the selection down to the next selectable row (Down).
    pub fn move_down(&mut self) {
        let from = self.selected.unwrap_or(0);
        if let Some(next) = self.next_selectable(from, 1) {
            self.selected = Some(next);
        } else if self.selected.is_none() {
            self.selected = self.first_selectable_row();
        }
    }

    /// Move the selection up to the previous selectable row (Up).
    pub fn move_up(&mut self) {
        if let Some(from) = self.selected {
            if let Some(prev) = self.next_selectable(from, -1) {
                self.selected = Some(prev);
            }
        }
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
    use crate::ui::design::{color, icon, tokens};
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

    /// Render a type name with fuzzy-matched chars emphasized (accent + semibold),
    /// the rest in `base`. `positions` are char indices into `name`.
    fn highlighted_name(
        name: &str,
        positions: &[usize],
        base: Hsla,
        accent: Hsla,
    ) -> Vec<AnyElement> {
        let pos: std::collections::BTreeSet<usize> = positions.iter().copied().collect();
        let mut spans: Vec<AnyElement> = Vec::new();
        let mut cur = String::new();
        let mut cur_hit: Option<bool> = None;
        let flush = |spans: &mut Vec<AnyElement>, text: &str, hit: bool| {
            if text.is_empty() {
                return;
            }
            let mut el = div().child(text.to_string());
            if hit {
                el = el.text_color(accent).font_weight(FontWeight::SEMIBOLD);
            } else {
                el = el.text_color(base);
            }
            spans.push(el.into_any_element());
        };
        for (i, ch) in name.chars().enumerate() {
            let hit = pos.contains(&i);
            if cur_hit != Some(hit) {
                if let Some(prev) = cur_hit {
                    flush(&mut spans, &cur, prev);
                }
                cur.clear();
                cur_hit = Some(hit);
            }
            cur.push(ch);
        }
        if let Some(prev) = cur_hit {
            flush(&mut spans, &cur, prev);
        }
        spans
    }

    /// The popup's outcome (the editor consumes this — see the menus↔editor
    /// CONTRACT). `Chosen` carries the picked base [`NodeKind`] plus the optional
    /// [`Modifier`] (`*` pointer / `**` double-pointer / `[]` array); the editor
    /// calls `controller_mut().change_node_kind(idx, kind)` then applies the
    /// modifier via its existing pointer/array ops.
    #[derive(Clone, Debug)]
    pub enum TypeSelectorEvent {
        /// A type was chosen: its base kind + the optional modifier.
        Chosen {
            kind: NodeKind,
            modifier: Option<Modifier>,
        },
        /// Dismissed (the `×`, Esc, or clicking outside).
        Cancel,
    }

    /// The type-selector popover view.
    pub struct TypeSelectorPopup {
        model: TypeModel,
        /// The kind the node currently has — highlighted as the active type.
        current: NodeKind,
        /// Which group chips are enabled (Hex/Int/Float/Ptr); `None` filter for the
        /// rest. Empty set = all shown (the "all" state); a non-empty set filters
        /// the list to those groups.
        active_groups: std::collections::BTreeSet<&'static str>,
        input: Entity<InputState>,
        focus_handle: FocusHandle,
        _subscription: Subscription,
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

        fn new_with_current(
            entries: Vec<TypeEntry>,
            current: NodeKind,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            let model = TypeModel::new(entries);
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Filter types..  (Ctrl+F)"));
            let subscription =
                cx.subscribe_in(&input, window, |this, _i, ev: &InputEvent, _window, cx| {
                    if matches!(ev, InputEvent::Change) {
                        let q = this.input.read(cx).value().to_string();
                        this.model.apply_filter(&q);
                        cx.notify();
                    }
                });
            TypeSelectorPopup {
                model,
                current,
                active_groups: std::collections::BTreeSet::new(),
                input,
                focus_handle: cx.focus_handle(),
                _subscription: subscription,
            }
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
        pub fn set_mode(&mut self, mode: TypePopupMode, cx: &mut Context<Self>) {
            self.model.set_mode(mode);
            cx.notify();
        }

        /// Set the array modifier count + select the `[]` modifier.
        pub fn set_array(&mut self, count: i32, cx: &mut Context<Self>) {
            self.model.set_modifier(Modifier::Array(count));
            cx.notify();
        }

        /// The explicit "none" sentinel key inserted by [`select_no_groups`] — its
        /// presence in `active_groups` hides *every* group (the C++ "none" chip).
        const NONE_SENTINEL: &'static str = "\u{0}none";

        /// Whether an entry's group passes the active category-chip filter.
        ///
        /// - empty set → all visible (the "all" state);
        /// - the `none` sentinel present → nothing visible (the "none" chip);
        /// - groups WITHOUT a category chip (Vec/Str/Ctr/Common) are always
        ///   visible (the C++ `catAllowed`: only chip-bearing groups can be
        ///   filtered out);
        /// - otherwise a chip-bearing group is visible iff its key is active.
        fn group_visible(&self, group: KindGroup) -> bool {
            // "none" sentinel → hide everything.
            if self.active_groups.contains(Self::NONE_SENTINEL) {
                return false;
            }
            // No active chips → show all.
            if self.active_groups.is_empty() {
                return true;
            }
            // Groups without a chip toggle are always visible.
            if !group.has_chip() {
                return true;
            }
            self.active_groups.contains(group.key())
        }

        /// Toggle a category chip (Hex/Int/Float/Ptr). Clears the "none" sentinel
        /// first so toggling a chip out of the "none" state actually re-shows it.
        fn toggle_group(&mut self, group: KindGroup, cx: &mut Context<Self>) {
            self.active_groups.remove(Self::NONE_SENTINEL);
            let key = group.key();
            if self.active_groups.contains(key) {
                self.active_groups.remove(key);
            } else {
                self.active_groups.insert(key);
            }
            cx.notify();
        }

        /// "all" — clear the category filter (every group shown).
        fn select_all_groups(&mut self, cx: &mut Context<Self>) {
            self.active_groups.clear();
            cx.notify();
        }

        /// "none" — restrict to a single empty bucket (nothing shown). We model
        /// this by enabling no chips but flagging the explicit-none state via a
        /// sentinel: an active set containing only an unused key hides every group.
        fn select_no_groups(&mut self, cx: &mut Context<Self>) {
            self.active_groups.clear();
            self.active_groups.insert(Self::NONE_SENTINEL);
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
            cx.emit(TypeSelectorEvent::Chosen {
                kind: entry.primitive_kind,
                modifier,
            });
        }

        fn accept_row(&mut self, row: usize, cx: &mut Context<Self>) {
            if self.model.select_row(row) {
                self.accept_selected(cx);
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
            cx.emit(TypeSelectorEvent::Chosen {
                kind: NodeKind::Struct,
                modifier,
            });
        }

        fn theme(&self, cx: &App) -> Theme {
            // The popover tints rows from our theme; pull the current one from the
            // app-shared manager so chip/group colors match the editor.
            super::super::theme_apply::ThemeRegistryGlobal::current(cx)
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
            // Ctrl+F focuses the filter from anywhere.
            if key == "f" && modifiers.control {
                let input = self.input.clone();
                window.focus(&input.read(cx).focus_handle(cx), cx);
                cx.notify();
                return true;
            }
            match key {
                "down" => {
                    self.model.move_down();
                    cx.notify();
                    true
                }
                "up" => {
                    self.model.move_up();
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
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.focus_handle.clone()
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
                .filter(|(_, r)| {
                    // Hide entries whose group is filtered out by the category chips
                    // (sections always show — they head their own group).
                    r.entry.entry_kind == EntryKind::Section || self.group_visible(r.entry.group)
                })
                .map(|(row, r)| {
                    if r.entry.entry_kind == EntryKind::Section {
                        // A Zed section caption: a colored group dot + uppercase
                        // micro label, muted.
                        let group = r.entry_group_for_label();
                        let dot = group.map(|g| {
                            super::super::theme_apply::to_hsla(super::kind_group_color(g, &theme))
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
                        let group_color = super::super::theme_apply::to_hsla(
                            super::kind_group_color(r.entry.group, &theme),
                        );
                        let is_sel = selected == Some(row);
                        let is_current = r.entry.primitive_kind == self.current
                            && r.entry.entry_kind != EntryKind::Composite;
                        let name_color = if r.entry.enabled {
                            group_color
                        } else {
                            color::text_disabled(cx)
                        };
                        let name_spans = if filtering {
                            highlighted_name(
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
                            .w_full()
                            .h(px(26.))
                            .px(px(tokens::space::MD))
                            .gap(px(tokens::space::MD))
                            .items_center()
                            .rounded(px(tokens::radius::MD))
                            .text_size(px(tokens::font::UI_MD))
                            .when(is_sel, |d| d.bg(sel_bg))
                            .when(!is_sel && is_current, |d| {
                                // The node's current type — a soft ring even when
                                // not the active row.
                                d.border_1().border_color(accent)
                            })
                            .when(!is_sel && r.entry.enabled, |d| d.hover(|s| s.bg(hover_bg)))
                            .when(r.entry.enabled, |d| d.cursor_pointer())
                            .when(r.entry.enabled, |d| {
                                d.on_click(cx.listener(move |this, _e, _w, cx| {
                                    this.accept_row(row, cx);
                                }))
                            })
                            // Leading colored kind chip (the SVG glyph for the group).
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(group_color)
                                    .child(group_icon(r.entry.group).size_3()),
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

            super::super::design::elevated_surface(cx)
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
                    gpui_component::v_flex()
                        .id("rcx-type-selector-list")
                        .p(px(tokens::space::XS))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_hidden()
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

            // Live per-group counts over the catalogue.
            let count_of = |g: KindGroup| -> usize {
                self.model
                    .entries()
                    .iter()
                    .filter(|e| e.group == g && e.selectable())
                    .count()
            };
            let total = self
                .model
                .entries()
                .iter()
                .filter(|e| e.selectable())
                .count();

            let chip = |group: KindGroup, count: usize, color: Hsla, on: bool| -> AnyElement {
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
                            .child(format!("{} ({count})", group.key())),
                    )
                    .into_any_element()
            };

            let group_color = |g: KindGroup| -> Hsla {
                super::super::theme_apply::to_hsla(super::kind_group_color(g, theme))
            };
            let on = |g: KindGroup| self.active_groups.contains(g.key());

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

            let all_active = self.active_groups.is_empty();
            let none_active = self.active_groups.contains(Self::NONE_SENTINEL);

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
                    count_of(KindGroup::Hex),
                    group_color(KindGroup::Hex),
                    on(KindGroup::Hex),
                ))
                .child(chip(
                    KindGroup::Int,
                    count_of(KindGroup::Int),
                    group_color(KindGroup::Int),
                    on(KindGroup::Int),
                ))
                .child(chip(
                    KindGroup::Float,
                    count_of(KindGroup::Float),
                    group_color(KindGroup::Float),
                    on(KindGroup::Float),
                ))
                .child(chip(
                    KindGroup::Ptr,
                    count_of(KindGroup::Ptr),
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
                        .child(format!("{total} types")),
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

            // The current selection summary "<curtype> · <size>".
            let summary = self
                .model
                .selected_entry()
                .map(|e| {
                    let size = if e.size_bytes > 0 {
                        format!("{}B", e.size_bytes)
                    } else {
                        "dyn".to_string()
                    };
                    let full = self
                        .model
                        .full_text()
                        .unwrap_or_else(|| e.display_name.clone());
                    format!("{full} · {size}")
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
            let array_modifier = match active {
                Modifier::Array(n) => Modifier::Array(n),
                _ => Modifier::Array(1),
            };
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
                            .child(chip(
                                "mod-array",
                                "[]",
                                array_on,
                                array_modifier,
                            ))
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
        kind_group_for, parse_type_spec, EntryKind, KindGroup, Modifier, SortMode, TypeEntry,
        TypeModel, TypePopupMode,
    };
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
}
