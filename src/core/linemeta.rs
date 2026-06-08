//! Line/render metadata (the display model): `LineKind`, `ChipKind`,
//! `LineChip`, `LineMeta`, `LayoutInfo`, `ComposeResult`, the `Marker` bit
//! indices, the selection-id encoding helpers, and the column constants.
//!
//! Faithful port of `src/core.h:182-193` (`Marker`), `:925-1076` (line model),
//! `:1130-1148` (column constants). `compose` produces these; the structs and
//! the selection-id math live in core as part of the model contract.

use super::kind::NodeKind;

// ── Markers (`core.h:182-193`) — bit indices for `LineMeta::marker_mask`. ──
pub const M_CONT: u32 = 0;
pub const M_PTR0: u32 = 2;
pub const M_CYCLE: u32 = 3;
pub const M_ERR: u32 = 4;
pub const M_STRUCT_BG: u32 = 5;
pub const M_HOVER: u32 = 6;
pub const M_SELECTED: u32 = 7;
pub const M_CMD_ROW: u32 = 8;
pub const M_ACCENT: u32 = 9;
pub const M_FOCUS: u32 = 10;

// ── Column constants (`core.h:1130-1148`). ──
pub const K_FOLD_COL: i32 = 3;
// Columns of indent per nesting level. ReClass uses 2; widened to 3 so nested
// structs / pointer-to-class expansions read as clear code-like steps (the prior
// 2-col step was too subtle to see deep nesting).
pub const K_TREE_INDENT: i32 = 3;
pub const K_COL_TYPE: i32 = 14;
pub const K_COL_NAME: i32 = 22;
pub const K_COL_VALUE: i32 = 96;
pub const K_COL_COMMENT: i32 = 28;
pub const K_COL_BASE_ADDR: i32 = 12;
pub const K_SEP_WIDTH: i32 = 1;
pub const K_MIN_TYPE_W: i32 = 9;
pub const K_MAX_TYPE_W: i32 = 128;
pub const K_MIN_NAME_W: i32 = 10;
pub const K_MAX_NAME_W: i32 = 128;
pub const K_COMPACT_TYPE_W: i32 = 20;
pub const K_DEFAULT_REFRESH_MS: i32 = 200;

/// `enum class LineKind : uint8_t` (`core.h:927-931`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum LineKind {
    CommandRow,
    /// (unused — kept for enum stability).
    Blank,
    Header,
    #[default]
    Field,
    Continuation,
    Footer,
    ArrayElementSeparator,
}

// ── Selection-id encoding (`core.h:933-961`). ──
pub const K_COMMAND_ROW_ID: u64 = u64::MAX;
pub const K_COMMAND_ROW_LINE: i32 = 0;
pub const K_FIRST_DATA_LINE: i32 = 1;
pub const K_FOOTER_ID_BIT: u64 = 0x8000_0000_0000_0000;
pub const K_ARRAY_ELEM_BIT: u64 = 0x4000_0000_0000_0000;
pub const K_ARRAY_ELEM_SHIFT: u64 = 42;
pub const K_ARRAY_ELEM_MASK: u64 = 0x3FFF_FC00_0000_0000;
// Member selection encoding (enum/bitfield members) — mirrors the array
// element pattern, but the flag bit sits one position LOWER (bit 61 vs the
// array's bit 62), so the value field is 19 bits (42-60), NOT 20. The mask
// must therefore EXCLUDE bit 61 (= `K_MEMBER_BIT`): a 20-bit mask
// (`0x3FFF_FC..`, reaching bit 61) would read the flag bit back into the
// decoded sub-line and inflate every result by 2^19 (524288), so
// `member_sub_from_sel_id` never matched the real sub-line and member rows
// were never highlighted. (The strip mask used for node lookup is
// `K_MEMBER_BIT | K_MEMBER_SUB_MASK` = bits 42-61, unchanged by this
// narrowing.)
pub const K_MEMBER_BIT: u64 = 0x2000_0000_0000_0000; // bit 61
pub const K_MEMBER_SUB_SHIFT: u64 = 42;
pub const K_MEMBER_SUB_MASK: u64 = 0x1FFF_FC00_0000_0000; // bits 42-60 (19 bits)

/// `makeArrayElemSelId` (`core.h:945`).
#[inline]
pub fn make_array_elem_sel_id(node_id: u64, elem_idx: i32) -> u64 {
    debug_assert!(elem_idx >= 0);
    node_id | K_ARRAY_ELEM_BIT | (((elem_idx as u64) & 0xFFFFF) << K_ARRAY_ELEM_SHIFT)
}
/// `arrayElemIdxFromSelId` (`core.h:948`).
#[inline]
pub fn array_elem_idx_from_sel_id(sel_id: u64) -> i32 {
    ((sel_id & K_ARRAY_ELEM_MASK) >> K_ARRAY_ELEM_SHIFT) as i32
}
/// `makeMemberSelId` (`core.h:957`).
#[inline]
pub fn make_member_sel_id(node_id: u64, sub_line: i32) -> u64 {
    node_id | K_MEMBER_BIT | (((sub_line as u64) & 0x7FFFF) << K_MEMBER_SUB_SHIFT)
}
/// `memberSubFromSelId` (`core.h:960`).
#[inline]
pub fn member_sub_from_sel_id(sel_id: u64) -> i32 {
    ((sel_id & K_MEMBER_SUB_MASK) >> K_MEMBER_SUB_SHIFT) as i32
}

/// What kind of selection an encoded `sel_id` represents. The flag bits are
/// NOT independent: the 20-bit array index field (bits 42-61) reaches bit 61
/// (= `K_MEMBER_BIT`) for indices >= 2^19, so a high array element id also has
/// the member bit set. Disambiguate by PRIORITY (footer 63 > array 62 >
/// member 61) — this is the single source of truth; never test the flag bits
/// independently (a `sel_id & K_MEMBER_BIT` check would misclassify a
/// high-index array element as a member). Decode the index/sub-line only after
/// classifying: `array_elem_idx_from_sel_id` reads the full 20-bit field, so
/// the array index round-trips correctly even with bit 61 set.
///
/// `enum class SelKind` / `selKindOf` (`core.h`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelKind {
    Plain,
    Footer,
    ArrayElem,
    Member,
}

/// `selKindOf(selId)` (`core.h`).
#[inline]
pub fn sel_kind(sel_id: u64) -> SelKind {
    if sel_id & K_FOOTER_ID_BIT != 0 {
        return SelKind::Footer;
    }
    if sel_id & K_ARRAY_ELEM_BIT != 0 {
        return SelKind::ArrayElem;
    }
    if sel_id & K_MEMBER_BIT != 0 {
        return SelKind::Member;
    }
    SelKind::Plain
}

/// Encoded selection id for a composed line — the single source of truth for
/// the line→sel_id rule. Footer rows carry the footer bit, array elements the
/// array-elem encoding, members the member encoding; everything else is the
/// bare node id. Used by both the controller's click handler
/// (`handle_node_click`) and the editor's byte-selection→row sync so a byte
/// selection produces exactly the ids a click would.
///
/// `selIdForLine(lm)` (`core.h`).
#[inline]
pub fn sel_id_for_line(lm: &LineMeta) -> u64 {
    if lm.line_kind == LineKind::Footer {
        return lm.node_id | K_FOOTER_ID_BIT;
    }
    if lm.is_array_element && lm.array_element_idx >= 0 {
        return make_array_elem_sel_id(lm.node_id, lm.array_element_idx);
    }
    if lm.is_member_line && lm.sub_line >= 0 {
        return make_member_sel_id(lm.node_id, lm.sub_line);
    }
    lm.node_id
}

/// `enum class ChipKind : uint8_t` (`core.h:971-980`).
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChipKind {
    Enum = 0,
    TypeHint,
    Rtti,
    Symbol,
    Comment,
    AddComment,
}

/// `struct LineChip` (`core.h:982-992`).
#[derive(Clone, Debug, PartialEq)]
pub struct LineChip {
    pub kind: ChipKind,
    pub start_col: i32,
    pub end_col: i32,
    /// already includes the prefix glyph (`/`, `[`, `{`, `(`).
    pub text: String,
    pub rtti_vtable_addr: u64,
    pub type_hint_kinds: Vec<NodeKind>,
    pub enum_current_value: i64,
    pub enum_ref_node_id: u64,
}

impl Default for LineChip {
    fn default() -> Self {
        LineChip {
            kind: ChipKind::Comment,
            start_col: -1,
            end_col: -1,
            text: String::new(),
            rtti_vtable_addr: 0,
            type_hint_kinds: Vec::new(),
            enum_current_value: 0,
            enum_ref_node_id: 0,
        }
    }
}

/// `struct LineMeta` (`core.h:994-1037`).
#[derive(Clone, Debug, PartialEq)]
pub struct LineMeta {
    pub node_idx: i32,
    pub node_id: u64,
    pub sub_line: i32,
    pub depth: i32,
    pub fold_level: i32,
    pub fold_head: bool,
    pub fold_collapsed: bool,
    pub is_continuation: bool,
    pub is_root_header: bool,
    pub is_array_header: bool,
    pub line_kind: LineKind,
    pub node_kind: NodeKind,
    pub element_kind: NodeKind,
    pub array_view_idx: i32,
    pub array_count: i32,
    pub array_element_idx: i32,
    pub offset_text: String,
    pub offset_addr: u64,
    pub ptr_base: u64,
    /// Whether this row sits inside an expanded pointer's target (a pointer-deref
    /// child). Distinct from `ptr_base != 0`: a NULL/unreadable pointer target has
    /// `ptr_base == 0` yet its children are still pointer-relative, so the offset
    /// gutter must measure them from the pointer target base (`ptr_base`, even 0)
    /// rather than falling back to the struct base.
    pub under_ptr: bool,
    pub marker_mask: u32,
    pub data_changed: bool,
    pub heat_level: i32,
    pub changed_byte_indices: Vec<i32>,
    pub line_byte_count: i32,
    pub effective_type_w: i32,
    pub effective_name_w: i32,
    pub pointer_target_name: String,
    pub is_array_element: bool,
    pub is_member_line: bool,
    pub brace_col: i32,
    pub parent_addr: u64,
    pub chips: Vec<LineChip>,
}

impl Default for LineMeta {
    fn default() -> Self {
        LineMeta {
            node_idx: -1,
            node_id: 0,
            sub_line: 0,
            depth: 0,
            fold_level: 0,
            fold_head: false,
            fold_collapsed: false,
            is_continuation: false,
            is_root_header: false,
            is_array_header: false,
            line_kind: LineKind::Field,
            node_kind: NodeKind::Int32,
            element_kind: NodeKind::UInt8,
            array_view_idx: 0,
            array_count: 0,
            array_element_idx: -1,
            offset_text: String::new(),
            offset_addr: 0,
            ptr_base: 0,
            under_ptr: false,
            marker_mask: 0,
            data_changed: false,
            heat_level: 0,
            changed_byte_indices: Vec::new(),
            line_byte_count: 0,
            effective_type_w: 14,
            effective_name_w: 22,
            pointer_target_name: String::new(),
            is_array_element: false,
            is_member_line: false,
            brace_col: -1,
            parent_addr: 0,
            chips: Vec::new(),
        }
    }
}

/// `findChip` (`core.h:1041-1046`) — first chip of `kind`, or `None`.
pub fn find_chip(lm: &LineMeta, kind: ChipKind) -> Option<&LineChip> {
    lm.chips.iter().find(|c| c.kind == kind)
}

/// `isSyntheticLine` (`core.h:1048`).
pub fn is_synthetic_line(lm: &LineMeta) -> bool {
    lm.line_kind == LineKind::CommandRow
}

/// `struct LayoutInfo` (`core.h:1054-1060`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutInfo {
    pub type_w: i32,
    pub name_w: i32,
    pub offset_hex_digits: i32,
    pub base_address: u64,
    pub tree_lines: bool,
}

impl Default for LayoutInfo {
    fn default() -> Self {
        LayoutInfo {
            type_w: 14,
            name_w: 22,
            offset_hex_digits: 8,
            base_address: 0,
            tree_lines: false,
        }
    }
}

/// `struct ComposeResult` (`core.h:1064-1076`).
#[derive(Clone, Debug, Default)]
pub struct ComposeResult {
    pub text: String,
    pub meta: Vec<LineMeta>,
    pub layout: LayoutInfo,
    /// longest line length in chars (ignoring trailing spaces).
    pub max_line_len: i32,
    /// char offset of the start of each line in `text`.
    pub line_starts: Vec<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_elem_sel_id_round_trips() {
        let id = make_array_elem_sel_id(7, 123);
        assert!(id & K_ARRAY_ELEM_BIT != 0);
        assert_eq!(array_elem_idx_from_sel_id(id), 123);
        assert_eq!(id & !(K_ARRAY_ELEM_BIT | K_ARRAY_ELEM_MASK), 7);
    }

    #[test]
    fn member_sel_id_tags_and_encodes() {
        // The member flag bit (61) sits one position BELOW the array bit (62),
        // so the sub-line field is only 19 bits (42-60) and `K_MEMBER_SUB_MASK`
        // EXCLUDES bit 61. With the narrowed mask the sub-line round-trips
        // EXACTLY (the prior 20-bit mask folded the flag bit back in and
        // inflated every decode by 2^19 = 524288, silently killing member-row
        // highlight). Assert the tag bit, that the node id strips clean, and an
        // exact round-trip with no inflation.
        let id = make_member_sel_id(9, 4);
        assert!(id & K_MEMBER_BIT != 0);
        assert_eq!(id & !(K_MEMBER_BIT | K_MEMBER_SUB_MASK), 9);
        assert_eq!(member_sub_from_sel_id(id), 4);
        assert_eq!(sel_kind(id), SelKind::Member);

        // Regression: a high array index (>= 2^19) sets bit 61 (= K_MEMBER_BIT),
        // but `sel_kind` must classify it as ArrayElem (array bit 62 has higher
        // priority than the member bit), and the full 20-bit array index field
        // must still round-trip — it must NOT be misread as a member.
        let arr = make_array_elem_sel_id(11, 0x80000);
        assert_eq!(sel_kind(arr), SelKind::ArrayElem);
        assert_eq!(array_elem_idx_from_sel_id(arr), 0x80000);
    }

    #[test]
    fn sel_id_for_line_matches_click_encoding() {
        // The single source of truth for the line→sel_id rule: footer rows carry
        // the footer bit, array elements the array encoding, members the member
        // encoding, everything else the bare node id. (Mirrors `selIdForLine`.)
        let plain = LineMeta {
            node_id: 5,
            ..LineMeta::default()
        };
        assert_eq!(sel_id_for_line(&plain), 5);
        assert_eq!(sel_kind(sel_id_for_line(&plain)), SelKind::Plain);

        let footer = LineMeta {
            node_id: 5,
            line_kind: LineKind::Footer,
            ..LineMeta::default()
        };
        assert_eq!(sel_id_for_line(&footer), 5 | K_FOOTER_ID_BIT);
        assert_eq!(sel_kind(sel_id_for_line(&footer)), SelKind::Footer);

        let elem = LineMeta {
            node_id: 5,
            is_array_element: true,
            array_element_idx: 3,
            ..LineMeta::default()
        };
        assert_eq!(sel_id_for_line(&elem), make_array_elem_sel_id(5, 3));
        assert_eq!(array_elem_idx_from_sel_id(sel_id_for_line(&elem)), 3);

        let member = LineMeta {
            node_id: 5,
            is_member_line: true,
            sub_line: 2,
            ..LineMeta::default()
        };
        assert_eq!(sel_id_for_line(&member), make_member_sel_id(5, 2));
        assert_eq!(member_sub_from_sel_id(sel_id_for_line(&member)), 2);
    }
}
