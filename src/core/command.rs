//! Undo/redo command model + `ViewState`.
//!
//! Faithful port of `src/core.h:1080-1115` (`namespace cmd` + the
//! `std::variant<...> Command`) and `core.h:1456-1468` (`ViewState`). Each
//! command carries both old and new values so the controller's `apply_command`
//! can run forward or backward. The *application* of these commands lives in
//! the `controller` module; only the data shapes are defined here.

use super::kind::NodeKind;
use super::node::Node;

/// `cmd::OffsetAdj` (`core.h:1081`) — a sibling offset shift embedded in
/// several commands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffsetAdj {
    pub node_id: u64,
    pub old_offset: i32,
    pub new_offset: i32,
}

/// `using Command = std::variant<...>` (`core.h:1080-1115`).
///
/// 19 variants (the `OffsetAdj` POD is a helper, not a command).
/// `ToggleRelative` toggles a node's RVA flag and is handled in
/// `applyCommand` (used by the type chooser's "Pointer32 (RVA)" entries).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `cmd::ChangeKind` (`core.h:1083-1084`).
    ChangeKind {
        node_id: u64,
        old_kind: NodeKind,
        new_kind: NodeKind,
        off_adjs: Vec<OffsetAdj>,
    },
    /// `cmd::Rename` (`core.h:1085`).
    Rename {
        node_id: u64,
        old_name: String,
        new_name: String,
    },
    /// `cmd::Collapse` (`core.h:1086`).
    Collapse {
        node_id: u64,
        old_state: bool,
        new_state: bool,
    },
    /// `cmd::Insert` (`core.h:1087`).
    Insert {
        node: Node,
        off_adjs: Vec<OffsetAdj>,
    },
    /// `cmd::Remove` (`core.h:1088-1089`).
    Remove {
        node_id: u64,
        subtree: Vec<Node>,
        off_adjs: Vec<OffsetAdj>,
    },
    /// `cmd::ChangeBase` (`core.h:1090`).
    ChangeBase {
        old_base: u64,
        new_base: u64,
        old_formula: String,
        new_formula: String,
    },
    /// `cmd::WriteBytes` (`core.h:1091`) — the only command that touches the provider.
    WriteBytes {
        addr: u64,
        old_bytes: Vec<u8>,
        new_bytes: Vec<u8>,
    },
    /// `cmd::ChangeArrayMeta` (`core.h:1092-1094`).
    ChangeArrayMeta {
        node_id: u64,
        old_element_kind: NodeKind,
        new_element_kind: NodeKind,
        old_array_len: i32,
        new_array_len: i32,
    },
    /// `cmd::ChangePointerRef` (`core.h:1095-1096`).
    ChangePointerRef {
        node_id: u64,
        old_ref_id: u64,
        new_ref_id: u64,
    },
    /// `cmd::ChangeStructTypeName` (`core.h:1097`).
    ChangeStructTypeName {
        node_id: u64,
        old_name: String,
        new_name: String,
    },
    /// `cmd::ChangeClassKeyword` (`core.h:1098`).
    ChangeClassKeyword {
        node_id: u64,
        old_keyword: String,
        new_keyword: String,
    },
    /// `cmd::ChangeOffset` (`core.h:1099`).
    ChangeOffset {
        node_id: u64,
        old_offset: i32,
        new_offset: i32,
    },
    /// `cmd::ChangeEnumMembers` (`core.h:1100-1101`).
    ChangeEnumMembers {
        node_id: u64,
        old_members: Vec<(String, i64)>,
        new_members: Vec<(String, i64)>,
    },
    /// `cmd::ToggleRelative` (`core.h`) — toggles a node's RVA flag.
    ToggleRelative {
        node_id: u64,
        old_val: bool,
        new_val: bool,
    },
    /// `cmd::ToggleBigEndian` (`core.h`).
    ToggleBigEndian {
        node_id: u64,
        old_val: bool,
        new_val: bool,
    },
    /// `cmd::ChangeComment` (`core.h`).
    ChangeComment {
        node_id: u64,
        old_comment: String,
        new_comment: String,
    },
}

/// `struct ViewState` (`core.h:1456-1468`) — caret anchored by node id so
/// refreshes that shift line counts re-land on the same node; falls back to
/// `(cursor_line, cursor_col)` when `cursor_node_id == 0`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewState {
    pub scroll_line: i32,
    pub cursor_line: i32,
    pub cursor_col: i32,
    pub x_offset: i32,
    pub cursor_node_id: u64,
    pub cursor_sub_line: i32,
}
