//! Bottom status bar — the thin chrome strip at the very bottom of the window
//! (app-shell §11).
//!
//! Port of the C++ `FlatStatusBar` + `setAppStatus` node readout (`main.cpp`):
//! a borderless bar showing the active node's context. On selecting a field the
//! C++ built "`StructName.fieldName`" + a dim "`  +0xNN`" offset suffix
//! (`MainWindow::nodeSelected`, main.cpp:3023); the screenshots show
//! "`UnnamedClass0.field_20  +0x20`" (PIC5) and the richer hover form
//! "`FuncPtr64 qt_metacast  offset: 0x0010  size: 8 bytes`" (PIC3). This module
//! reproduces both from the editor's controller state.
//!
//! ## Split: gpui-free model + thin render
//! [`StatusInfo`] is a pure, unit-tested builder over a [`NodeTree`] + the
//! controller's selected-id set; it mirrors the C++ "walk to root → `Type`,
//! `Root.field`, `N selected`, offset, size" logic exactly. The render path
//! ([`render_status_bar`]) only lays the resulting `path` / `detail` strings out
//! in Zed status-bar chrome (thin bar, top 1px border, `UI_XS` muted text).
//!
//! Follows the shared Zed design system ([`crate::ui::design`]; spec §5.12):
//! `chrome_bg`, top 1px `border`, ~22px tall, `UI_XS` `text_muted`, a flex row
//! with the node path on the left and the offset/size detail on the right.
//!
//! Gated behind the `ui` feature.

use std::collections::HashSet;

use crate::controller::RcxController;
use crate::core::{kind_meta, size_for_kind, NodeKind, NodeTree};

/// The resolved status-bar readout for the active node selection — the pure
/// product of [`StatusInfo::for_controller`] / [`StatusInfo::from_tree`].
///
/// `path` is the left-aligned primary string (`Root.field`, a root name, or
/// "`N nodes selected`"); `detail` is the right-aligned dim string (the offset
/// and, for a single leaf, the byte size — the C++ "`  +0xNN`" plus the PIC3
/// "`offset: 0x.. size: N bytes`" form). Both empty ⇒ nothing selected (the bar
/// shows only the source readout).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StatusInfo {
    /// Left primary: "StructName.fieldName", a root type name, or "N nodes selected".
    pub path: String,
    /// Right detail: "+0x20" / "offset: 0x0010  size: 8 bytes" (empty if none).
    pub detail: String,
}

impl StatusInfo {
    /// `true` when there is no active selection (nothing to show but the source).
    pub fn is_empty(&self) -> bool {
        self.path.is_empty() && self.detail.is_empty()
    }

    /// Build the readout from a live controller — resolves the type name through
    /// the document's alias table (so "FuncPtr64" etc. honour user aliases), the
    /// selection from `selected_ids`, and the tree for path/offset/size.
    pub fn for_controller(ctrl: &RcxController) -> StatusInfo {
        let tree = ctrl.tree();
        let selected = ctrl.selected_ids();
        let resolve = |kind: NodeKind| ctrl.document().resolve_type_name(kind);
        Self::from_tree(tree, selected, resolve)
    }

    /// Build the readout from a tree + the selected-node ids + a type-name
    /// resolver (the alias-aware `resolve_type_name`). Pure → unit-tested.
    ///
    /// Mirrors `MainWindow::nodeSelected` (main.cpp:3023):
    /// - `> 1` selected → "`TypeName ×N`" (the primary's type), no detail.
    /// - root node → its own name/struct-type-name, detail = "`+0xNN`".
    /// - nested node → "`Root.field`", detail = "`+0xNN`" (+ size for a leaf).
    pub fn from_tree(
        tree: &NodeTree,
        selected: &HashSet<u64>,
        resolve: impl Fn(NodeKind) -> String,
    ) -> StatusInfo {
        let count = selected.len();
        // The "primary" node drives the rich line. With one selection it is that
        // node; with many we still describe the first (deterministic: lowest id,
        // matching the C++ which reads the just-selected `nodeIdx`).
        let Some(primary_id) = selected.iter().copied().min() else {
            return StatusInfo::default();
        };
        let idx = tree.index_of_id(primary_id);
        if idx < 0 {
            return StatusInfo::default();
        }
        let node = &tree.nodes[idx as usize];

        let type_name = resolve(node.kind);

        // Walk to the root struct for the "Root.field" prefix.
        let root_name = root_name_of(tree, idx);

        if count > 1 {
            // "TypeName ×N" — the multi-select summary (no offset detail).
            return StatusInfo {
                path: format!("{type_name} \u{00D7}{count}"),
                detail: String::new(),
            };
        }

        let path = if node.parent_id == 0 {
            // Root node: its own name.
            if root_name.is_empty() {
                node.name.clone()
            } else {
                root_name
            }
        } else if !root_name.is_empty() {
            format!("{root_name}.{}", node.name)
        } else {
            node.name.clone()
        };

        // Detail: the offset (always) + the byte size for a sized leaf. The C++
        // dim suffix is "  +0xNN"; the PIC3 hover form spells it "offset: 0x..
        // size: N bytes". We render the spelled-out, information-dense form.
        let mut detail = format!("+0x{:02X}", node.offset);
        let sz = size_for_kind(node.kind);
        if sz > 0 {
            detail = format!(
                "offset: 0x{:04X}  size: {} byte{}",
                node.offset,
                sz,
                if sz == 1 { "" } else { "s" }
            );
        }

        StatusInfo { path, detail }
    }
}

/// Walk up from `idx` to the root node and return its display name
/// (`structTypeName` if set, else `name`) — the C++ "walk to root struct" loop.
fn root_name_of(tree: &NodeTree, idx: i32) -> String {
    let mut cur = idx;
    // Climb while the current node has a parent.
    while cur >= 0 {
        let n = &tree.nodes[cur as usize];
        if n.parent_id == 0 {
            return if n.struct_type_name.is_empty() {
                n.name.clone()
            } else {
                n.struct_type_name.clone()
            };
        }
        cur = tree.index_of_id(n.parent_id);
    }
    String::new()
}

/// The type-name fallback used when no alias-aware resolver is supplied (tests /
/// previews): the built-in `kind_meta` type name, or "?" for an unknown kind.
#[allow(dead_code)]
pub fn default_type_name(kind: NodeKind) -> String {
    kind_meta(kind).map_or_else(|| "?".to_string(), |m| m.type_name.to_string())
}

// ── gpui view ────────────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use view::render_status_bar;

#[cfg(feature = "ui")]
mod view {
    use super::StatusInfo;
    use crate::ui::design::{color, tokens};
    use crate::ui::state::DataSource;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;

    /// Render the bottom status bar (app-shell §11): a thin chrome strip with the
    /// active node path on the left and the offset/size detail on the right, plus
    /// the active source readout. Zed chrome — `chrome_bg`, a top 1px border,
    /// ~22px tall, `UI_XS` muted type.
    ///
    /// `info` is the resolved node readout ([`StatusInfo::for_controller`]);
    /// `source` is the active document's data source (the source-kind label,
    /// dimmed when disconnected). When nothing is selected the path/detail are
    /// empty and only the source readout shows.
    pub fn render_status_bar(info: &StatusInfo, source: &DataSource, cx: &App) -> impl IntoElement {
        let muted = color::text_muted(cx);
        let text = color::text(cx);

        // Left: the node path (primary text) + its dim detail suffix.
        let has_path = !info.path.is_empty();
        let path = info.path.clone();
        let detail = info.detail.clone();
        let has_detail = !detail.is_empty();

        // Right: the source readout ("File: x.bin" / "No source"), dimmed when the
        // source is disconnected (the C++ ×0.40 live opacity).
        let source_label = if source.target.is_empty() {
            source.kind.label().to_string()
        } else {
            format!("{}: {}", source.kind.label(), source.target)
        };
        let source_color = if source.live {
            muted
        } else {
            color::text_disabled(cx)
        };

        gpui_component::h_flex()
            .id("rcx-status-bar")
            .flex_none()
            .w_full()
            .h(px(22.0))
            .px(px(tokens::space::LG))
            .gap(px(tokens::space::MD))
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(color::border(cx))
            .bg(color::chrome_bg(cx))
            .text_size(px(tokens::font::UI_XS))
            .text_color(muted)
            // Left cluster: node path · detail.
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .when(has_path, |row| {
                        row.child(div().flex_none().text_color(text).child(path.clone()))
                    })
                    .when(has_detail, |row| {
                        row.child(div().flex_none().text_color(muted).child(detail.clone()))
                    }),
            )
            // Spacer.
            .child(div().flex_1())
            // Right cluster: the active source readout.
            .child(
                div()
                    .flex_none()
                    .text_color(source_color)
                    .child(source_label),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{default_type_name, StatusInfo};
    use crate::core::kind::NodeKind;
    use crate::core::node::Node;
    use crate::core::tree::NodeTree;
    use std::collections::HashSet;

    /// A two-level tree: root struct "UnnamedClass0" with one Int32 field
    /// "field_20" at offset 0x20.
    fn fixture() -> (NodeTree, u64, u64) {
        let mut tree = NodeTree::new();
        let root = Node {
            kind: NodeKind::Struct,
            name: "UnnamedClass0".into(),
            struct_type_name: "UnnamedClass0".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        let ri = tree.add_node(root);
        let root_id = tree.nodes[ri].id;
        let field = Node {
            kind: NodeKind::Int32,
            name: "field_20".into(),
            parent_id: root_id,
            offset: 0x20,
            ..Node::default()
        };
        let fi = tree.add_node(field);
        let field_id = tree.nodes[fi].id;
        (tree, root_id, field_id)
    }

    fn sel(ids: &[u64]) -> HashSet<u64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn empty_selection_yields_empty_info() {
        let (tree, _root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[]), default_type_name);
        assert!(info.is_empty());
        assert_eq!(info.path, "");
        assert_eq!(info.detail, "");
    }

    #[test]
    fn nested_field_shows_root_dot_field_and_offset() {
        // PIC5: "UnnamedClass0.field_20  +0x20".
        let (tree, _root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[field]), default_type_name);
        assert_eq!(info.path, "UnnamedClass0.field_20");
        // Int32 is a sized leaf → spelled-out detail with the offset + size.
        assert_eq!(info.detail, "offset: 0x0020  size: 4 bytes");
    }

    #[test]
    fn root_node_shows_its_own_name() {
        let (tree, root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[root]), default_type_name);
        assert_eq!(info.path, "UnnamedClass0");
        // A struct has no leaf size → just the offset suffix.
        assert_eq!(info.detail, "+0x00");
    }

    #[test]
    fn multi_select_summarizes_type_and_count() {
        let (tree, root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[root, field]), default_type_name);
        // Primary is the lowest id (the root, allocated first) → its type "Struct".
        // The summary form is "Type ×N" with no detail.
        assert!(info.path.ends_with("\u{00D7}2"));
        assert_eq!(info.detail, "");
    }

    #[test]
    fn unknown_id_yields_empty() {
        let (tree, _root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[999_999]), default_type_name);
        assert!(info.is_empty());
    }

    #[test]
    fn single_byte_field_uses_singular_byte() {
        let mut tree = NodeTree::new();
        let n = Node {
            kind: NodeKind::Int8,
            name: "b".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        let i = tree.add_node(n);
        let id = tree.nodes[i].id;
        let info = StatusInfo::from_tree(&tree, &sel(&[id]), default_type_name);
        // Int8 is sized 1 → "1 byte" (singular).
        assert_eq!(info.detail, "offset: 0x0000  size: 1 byte");
    }

    #[test]
    fn alias_resolver_is_used_for_type_name() {
        let (tree, root, field) = fixture();
        // Multi-select uses the primary's type name via the resolver; supply an
        // alias to prove the resolver is honoured.
        let info = StatusInfo::from_tree(&tree, &sel(&[root, field]), |_k| "Aliased".into());
        assert_eq!(info.path, "Aliased \u{00D7}2");
    }
}
