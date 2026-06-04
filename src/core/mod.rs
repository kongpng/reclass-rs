//! Core node/type model — the shared vocabulary the whole engine builds on.
//!
//! Faithful port of `src/core.h`, `src/commontypes.h`, `src/typeinfer.h`, and
//! `src/clipboard.h`. This is the foundation: the [`NodeKind`] type system, the
//! [`Node`] / [`NodeTree`] document model, the heatmap [`ValueHistory`] ring
//! buffer, the line/render metadata ([`LineMeta`] etc.), the undo/redo
//! [`Command`] model, predefined struct templates, the clipboard codec, and the
//! type-inference engine. **Genuinely ported** (not a stub) — per
//! ARCHITECTURE.md §3.

pub mod clipboard;
pub mod command;
pub mod commontypes;
pub mod debug_view;
pub mod kind;
pub mod linemeta;
pub mod node;
pub mod tree;
pub mod typeinfer;
pub mod value_history;

// ── Flat re-exports so callers can `use reclass::core::Node` etc. ──
pub use command::{Command, OffsetAdj, ViewState};
pub use commontypes::{find_common_type, CommonField, CommonType, K_COMMON_TYPES};
pub use debug_view::generate_debug_text;
pub use kind::{
    alignment_for, all_type_names_for_ui, flags_for, is_container_kind, is_func_ptr, is_hex_node,
    is_hex_preview, is_matrix_kind, is_pointer_kind, is_string_kind, is_valid_primitive_ptr_target,
    is_vector_kind, kind_from_string, kind_from_type_name, kind_meta, kind_to_string,
    lines_for_kind, size_for_kind, KindMeta, NodeKind, K_KIND_META,
};
pub use linemeta::{
    array_elem_idx_from_sel_id, find_chip, is_synthetic_line, make_array_elem_sel_id,
    make_member_sel_id, member_sub_from_sel_id, ChipKind, ComposeResult, LayoutInfo, LineChip,
    LineKind, LineMeta,
};
pub use node::{BitfieldMember, Bookmark, Node, K_MAX_ARRAY_LEN};
pub use tree::{root_class_names, NodeTree, OverlapPair, ValidateReport};
pub use typeinfer::{format_hint, infer_types, InferHints, TypeSuggestion};
pub use value_history::ValueHistory;
