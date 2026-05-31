//! Bottom status bar — the thin chrome strip at the very bottom of the window
//! (app-shell §11).
//!
//! Port of the C++ `FlatStatusBar` + `setAppStatus` node readout (`main.cpp`):
//! a borderless bar showing the active node's context. On selecting a field the
//! C++ built "`StructName.fieldName`" + a dim "`  +0xNN`" offset suffix
//! (`MainWindow::nodeSelected`, main.cpp:3023/3062); PIC5 shows exactly that:
//! "`UnnamedClass0.field_20  +0x20`". This module reproduces that always-on
//! selection readout, plus a Zed-style right-hand cluster of muted segments
//! (offset/size detail · data source · view mode) separated by hairline dots.
//!
//! ## Default readout (nothing selected)
//! With no active selection the C++ bar went blank. PIC5 shows the *viewed*
//! struct is still meaningful context, so this port falls back to describing the
//! viewed root: "`UnnamedClass0  ·  19 fields  ·  0x80 bytes`" — the root type
//! name, its direct-member count, and its total footprint
//! ([`NodeTree::total_byte_size`]). This keeps the bar informative instead of
//! empty (the task's default-branch requirement).
//!
//! ## Selection readout (PIC5 / PIC3)
//! A single selection produces the compact C++ "`Root.field`" left primary +
//! a richer right detail. PIC5 shows the compact "`+0x20`"; PIC3's editor hover
//! line spells it "`offset: 0x0010  size: 8 bytes`". We carry BOTH: `detail`
//! keeps the compact "`+0xNN`" suffix (unchanged, for existing wiring/tests),
//! and `info` carries the spelled "`offset: 0x.. · size: N bytes`" right segment.
//!
//! ## Split: gpui-free model + thin render
//! [`StatusInfo`] is a pure, unit-tested builder over a [`NodeTree`] + the
//! controller's selected-id set + the active view-root id; it mirrors the C++
//! "walk to root → `Type`, `Root.field`, `N selected`, `+0xNN`" logic and adds
//! the default branch. The render path ([`render_status_bar`]) only lays the
//! resulting strings out in Zed status-bar chrome (thin bar, top 1px border,
//! `UI_XS` muted text, right-aligned segments with subtle dot separators).
//!
//! Follows the shared Zed design system ([`crate::ui::design`]; spec §5.12):
//! `chrome_bg`, top 1px `border`, ~22px tall, `UI_XS` `text_muted`, a flex row
//! with the node path on the left and the offset/size detail + source + view-mode
//! segments on the right.
//!
//! Gated behind the `ui` feature.

use std::collections::HashSet;

use crate::controller::RcxController;
use crate::core::{kind_meta, NodeKind, NodeTree};

/// The resolved status-bar readout — the pure product of
/// [`StatusInfo::for_controller`] / [`StatusInfo::from_tree`].
///
/// `path` is the left-aligned primary string (`Root.field`, a root name, or
/// "`N nodes selected`"). `detail` is the compact C++ dim offset suffix —
/// the "`  +0xNN`" form (`main.cpp:3062`) shown in PIC5
/// ("`UnnamedClass0.field_20  +0x20`"). `info` is the richer right-hand context
/// segment: for a single selection the spelled "`offset: 0x.. · size: N bytes`"
/// (PIC3); for the *default* (no-selection) readout the viewed-struct summary
/// "`N fields · 0x.. bytes`". `is_default` distinguishes the two so the render
/// path can dim the default branch like Zed's idle status text.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StatusInfo {
    /// Left primary: "StructName.fieldName", a root type name, or "N nodes selected".
    pub path: String,
    /// Compact dim offset suffix "+0x20" (empty for the default / multi-select).
    pub detail: String,
    /// Right context segment: "offset: 0x.. · size: N bytes" (selection) or
    /// "N fields · 0x.. bytes" (default viewed-struct summary). Empty ⇒ none.
    pub info: String,
    /// `true` when this is the no-selection *default* readout (the viewed-struct
    /// summary) rather than a live selection — drives the dimmed render.
    pub is_default: bool,
}

impl StatusInfo {
    /// `true` when there is nothing at all to show on the left (no selection and
    /// no viewed struct) — the bar then shows only the source/view segments.
    pub fn is_empty(&self) -> bool {
        self.path.is_empty() && self.detail.is_empty() && self.info.is_empty()
    }

    /// Build the readout from a live controller — resolves the type name through
    /// the document's alias table (so "FuncPtr64" etc. honour user aliases), the
    /// selection from `selected_ids`, the viewed root from `view_root_id`, and the
    /// tree for path/offset/size.
    pub fn for_controller(ctrl: &RcxController) -> StatusInfo {
        let tree = ctrl.tree();
        let selected = ctrl.selected_ids();
        let view_root = ctrl.view_root_id();
        let resolve = |kind: NodeKind| ctrl.document().resolve_type_name(kind);
        Self::from_tree(tree, selected, view_root, resolve)
    }

    /// Build the readout from a tree + the selected-node ids + the active
    /// view-root id + a type-name resolver (the alias-aware `resolve_type_name`).
    /// Pure → unit-tested.
    ///
    /// Mirrors `MainWindow::nodeSelected` (main.cpp:3023) for the selection case,
    /// and adds the default viewed-struct branch:
    /// - `> 1` selected → "`TypeName ×N`" (the primary's type), no detail.
    /// - one node → "`Root.field`" (or the root name), `detail = +0xNN`,
    ///   `info = "offset: 0x.. · size: N bytes"`.
    /// - nothing selected → the viewed struct's "`Name`" + `info =
    ///   "N fields · 0x.. bytes"` (`is_default`), or empty when there is none.
    pub fn from_tree(
        tree: &NodeTree,
        selected: &HashSet<u64>,
        view_root: u64,
        resolve: impl Fn(NodeKind) -> String,
    ) -> StatusInfo {
        let count = selected.len();
        // The "primary" node drives the rich line. With one selection it is that
        // node; with many we still describe the first (deterministic: lowest id,
        // matching the C++ which reads the just-selected `nodeIdx`).
        let Some(primary_id) = selected.iter().copied().min() else {
            // Nothing selected → the default viewed-struct summary.
            return Self::default_for_view(tree, view_root);
        };
        let idx = tree.index_of_id(primary_id);
        if idx < 0 {
            return Self::default_for_view(tree, view_root);
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
                info: format!("{count} nodes selected"),
                is_default: false,
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

        // Detail: the dim offset suffix. The C++ `nodeSelected` → `setAppStatus`
        // emits exactly "  +0xNN" (`main.cpp:3062`), which is what PIC5 shows
        // ("UnnamedClass0.field_20  +0x20"). We keep that compact form in `detail`
        // and the richer "offset: 0x.. · size: N bytes" spelling (PIC3's editor
        // hover line) in `info` so the bar can show both.
        let detail = format!("+0x{:02X}", node.offset);
        let size = tree.total_byte_size(node);
        let info = format!(
            "offset: 0x{:04X} \u{00B7} size: {} bytes",
            node.offset, size
        );

        StatusInfo {
            path,
            detail,
            info,
            is_default: false,
        }
    }

    /// The default (no-selection) readout: describe the *viewed* struct — its
    /// type name + direct-member count + total footprint. PIC5's "viewed struct
    /// is still context" idea, spelled "`Name  ·  N fields  ·  0x.. bytes`".
    ///
    /// Picks the viewed node by `view_root` id, falling back to the first root
    /// struct (`parent_id == 0`) when `view_root == 0` ("show whole tree", the
    /// C++ default view-root). Returns an empty [`StatusInfo`] when the tree has
    /// no struct to describe.
    fn default_for_view(tree: &NodeTree, view_root: u64) -> StatusInfo {
        let idx = if view_root != 0 {
            tree.index_of_id(view_root)
        } else {
            // No explicit view-root → the first top-level struct.
            first_root_idx(tree)
        };
        if idx < 0 {
            return StatusInfo::default();
        }
        let node = &tree.nodes[idx as usize];

        // Display name: struct type name if set, else the node name, else a
        // sensible placeholder (matches `root_class_names`' "Untitled" fallback).
        let name = if !node.struct_type_name.is_empty() {
            node.struct_type_name.clone()
        } else if !node.name.is_empty() {
            node.name.clone()
        } else {
            "Untitled".to_string()
        };

        // Direct members (the C++ "N fields" count) + total footprint.
        let fields = tree.children_of(node.id).len();
        let size = tree.total_byte_size(node);
        let plural = if fields == 1 { "field" } else { "fields" };
        let info = format!("{fields} {plural} \u{00B7} 0x{size:X} bytes");

        StatusInfo {
            path: name,
            detail: String::new(),
            info,
            is_default: true,
        }
    }
}

/// Index of the first top-level struct (`parent_id == 0`, `Struct` kind) — the
/// node the editor shows when there is no explicit view-root. `-1` if none.
fn first_root_idx(tree: &NodeTree) -> i32 {
    tree.nodes
        .iter()
        .position(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
        .map_or(-1, |p| p as i32)
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
    use crate::ui::theme_apply::ThemeRegistryGlobal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;

    /// A faint vertical hairline separating two right-hand status segments — the
    /// Zed status-bar divider (a 1px-wide muted rule with a little vertical
    /// inset). Distinct from a textual "·" so the segments read as Zed cells.
    fn segment_sep(cx: &App) -> impl IntoElement {
        div()
            .flex_none()
            .w(px(1.0))
            .h(px(12.0))
            .bg(color::border(cx))
    }

    /// One muted right-hand status segment (offset/size · source · theme).
    fn segment(text: String, fg: Hsla) -> impl IntoElement {
        div().flex_none().text_color(fg).child(text)
    }

    /// The pinned status-bar height (logical px). The bar is locked to exactly
    /// this height — `h` *and* `min_h`/`max_h` are all set to it (and the row is
    /// `flex_none` ⇒ grow=0, shrink=0) so the surrounding flex column can never
    /// stretch *or* compress it. That hard reservation is what keeps the pinned
    /// bar inside the client area: the content/dock row above is `flex_1 min_h_0`
    /// and absorbs all remaining height, so the status row never gets pushed past
    /// the bottom edge by overflowing content (the maximized-window clipping bug).
    pub const STATUS_BAR_HEIGHT: f32 = 22.0;

    /// Render the bottom status bar (app-shell §11): a thin chrome strip with the
    /// active node path on the left and a Zed-style cluster of muted segments on
    /// the right — the offset/size detail (PIC5 "+0x20" / PIC3 "offset: 0x0010
    /// size: 8 bytes"), the data-source readout, and the active theme name —
    /// separated by subtle hairline rules. Zed chrome: `chrome_bg`, a top 1px
    /// border, ~22px tall, `UI_XS` muted type.
    ///
    /// The bar is a hard-pinned, fixed-height row: `flex_none` (grow=0, shrink=0)
    /// plus `h`/`min_h`/`max_h` all clamped to [`STATUS_BAR_HEIGHT`], so the flex
    /// column it lives in reserves exactly its height and can neither shrink it to
    /// zero nor let the content/dock above push it off-screen. `overflow_hidden`
    /// keeps a long readout from forcing the row taller than its fixed height.
    ///
    /// `info` is the resolved node readout ([`StatusInfo::for_controller`]) — a
    /// live selection (full-contrast path) or the default viewed-struct summary
    /// (dimmed). `source` is the active document's data source (dimmed when
    /// disconnected). The theme segment reads the active theme name from the
    /// global registry (no signature change to keep window-wiring stable).
    pub fn render_status_bar(info: &StatusInfo, source: &DataSource, cx: &App) -> impl IntoElement {
        let muted = color::text_muted(cx);
        let text = color::text(cx);

        // Left: the node path. A live selection is full-contrast; the default
        // viewed-struct summary is dimmed like Zed's idle status text.
        let has_path = !info.path.is_empty();
        let path = info.path.clone();
        let path_color = if info.is_default { muted } else { text };

        // Left dim suffix: the compact "+0xNN" (PIC5). Only on a live selection.
        let detail = info.detail.clone();
        let has_detail = !detail.is_empty();

        // Right segment 1: the offset/size detail (PIC3 spelling) or, in the
        // default branch, the viewed-struct "N fields · 0x.. bytes" summary.
        let info_seg = info.info.clone();
        let has_info = !info_seg.is_empty();

        // Right segment 2: the source readout ("File: x.bin" / "No source"),
        // dimmed when the source is disconnected (the C++ ×0.40 live opacity).
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

        // Right segment 3: the active theme name (the C++ status theme/view-mode
        // readout). Read from the global registry so the bar's signature — and
        // thus the window wiring — stays unchanged.
        let theme_label = ThemeRegistryGlobal::current(cx).name;

        gpui_component::h_flex()
            .id("rcx-status-bar")
            // Hard-pin the row: `flex_none` zeroes grow+shrink and the matched
            // `h`/`min_h`/`max_h` lock the height so the flex column reserves
            // exactly `STATUS_BAR_HEIGHT` for it — it can neither be stretched by
            // the column nor compressed/pushed off-screen by overflowing content
            // (the maximized-window clipping bug). `overflow_hidden` stops a long
            // readout from forcing the row taller than its fixed height.
            .flex_none()
            .flex_shrink_0()
            .w_full()
            .h(px(STATUS_BAR_HEIGHT))
            .min_h(px(STATUS_BAR_HEIGHT))
            .max_h(px(STATUS_BAR_HEIGHT))
            .overflow_hidden()
            .px(px(tokens::space::LG))
            .gap(px(tokens::space::MD))
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(color::border(cx))
            .bg(color::chrome_bg(cx))
            .text_size(px(tokens::font::UI_XS))
            .text_color(muted)
            // Left cluster: node path · compact offset suffix.
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .when(has_path, |row| {
                        row.child(div().flex_none().text_color(path_color).child(path.clone()))
                    })
                    .when(has_detail, |row| {
                        row.child(div().flex_none().text_color(muted).child(detail.clone()))
                    }),
            )
            // Spacer.
            .child(div().flex_1())
            // Right cluster: offset/size · source · theme, hairline-separated.
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .when(has_info, |row| {
                        row.child(segment(info_seg.clone(), muted))
                            .child(segment_sep(cx))
                    })
                    .child(segment(source_label, source_color))
                    .child(segment_sep(cx))
                    .child(segment(theme_label, muted)),
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

    // The fixture root struct has a single Int32 leaf child (4 bytes) at +0x20,
    // so its viewed-struct summary footprint is 0x24 and its field count is 1.
    const VIEW_NONE: u64 = 0; // "no explicit view-root → first root struct".

    #[test]
    fn empty_selection_falls_back_to_default_viewed_struct() {
        // The task's default branch: with nothing selected the bar describes the
        // viewed struct ("Name · N fields · 0x.. bytes") instead of going blank.
        let (tree, _root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[]), VIEW_NONE, default_type_name);
        assert!(!info.is_empty(), "default readout must not be empty");
        assert!(info.is_default);
        assert_eq!(info.path, "UnnamedClass0");
        // One direct member, footprint = leaf end 0x20 + 4 = 0x24.
        assert_eq!(info.info, "1 field \u{00B7} 0x24 bytes");
        // The default branch carries no compact "+0xNN" suffix.
        assert_eq!(info.detail, "");
    }

    #[test]
    fn default_readout_pluralizes_field_count_and_uses_explicit_view_root() {
        // A root struct with two members → "2 fields"; addressing it by an
        // explicit view-root id exercises the non-zero branch.
        let mut tree = NodeTree::new();
        let root = Node {
            kind: NodeKind::Struct,
            name: "Foo".into(),
            struct_type_name: "Foo".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        let ri = tree.add_node(root);
        let root_id = tree.nodes[ri].id;
        for off in [0x0, 0x8] {
            tree.add_node(Node {
                kind: NodeKind::Int64,
                name: format!("m{off}"),
                parent_id: root_id,
                offset: off,
                ..Node::default()
            });
        }
        let info = StatusInfo::from_tree(&tree, &sel(&[]), root_id, default_type_name);
        assert!(info.is_default);
        assert_eq!(info.path, "Foo");
        // Two Int64 members; footprint = 0x8 + 8 = 0x10.
        assert_eq!(info.info, "2 fields \u{00B7} 0x10 bytes");
    }

    #[test]
    fn default_readout_empty_when_no_struct_to_describe() {
        // An empty tree has nothing to view → the bar stays empty (only the
        // source/theme segments show).
        let tree = NodeTree::new();
        let info = StatusInfo::from_tree(&tree, &sel(&[]), VIEW_NONE, default_type_name);
        assert!(info.is_empty());
        assert!(!info.is_default);
    }

    #[test]
    fn default_readout_falls_back_to_untitled_when_unnamed() {
        // A nameless root struct → the "Untitled" placeholder (root_class_names
        // parity).
        let mut tree = NodeTree::new();
        tree.add_node(Node {
            kind: NodeKind::Struct,
            name: String::new(),
            struct_type_name: String::new(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let info = StatusInfo::from_tree(&tree, &sel(&[]), VIEW_NONE, default_type_name);
        assert_eq!(info.path, "Untitled");
        assert_eq!(info.info, "0 fields \u{00B7} 0x0 bytes");
    }

    #[test]
    fn nested_field_shows_root_dot_field_and_offset() {
        // PIC5: "UnnamedClass0.field_20  +0x20" — the compact C++ `nodeSelected`
        // dim suffix (`main.cpp:3062`); PIC3's spelled form lives in `info`.
        let (tree, _root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[field]), VIEW_NONE, default_type_name);
        assert_eq!(info.path, "UnnamedClass0.field_20");
        assert_eq!(info.detail, "+0x20");
        // PIC3 right segment: "offset: 0x0020 · size: 4 bytes" (Int32 leaf).
        assert_eq!(info.info, "offset: 0x0020 \u{00B7} size: 4 bytes");
        assert!(!info.is_default);
    }

    #[test]
    fn root_node_shows_its_own_name() {
        let (tree, root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[root]), VIEW_NONE, default_type_name);
        assert_eq!(info.path, "UnnamedClass0");
        // A struct has no leaf size → just the offset suffix; the rich `info`
        // segment reports its total span (0x24).
        assert_eq!(info.detail, "+0x00");
        assert_eq!(info.info, "offset: 0x0000 \u{00B7} size: 36 bytes");
    }

    #[test]
    fn multi_select_summarizes_type_and_count() {
        let (tree, root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[root, field]), VIEW_NONE, default_type_name);
        // Primary is the lowest id (the root, allocated first) → its type "Struct".
        // The summary form is "Type ×N" with no compact suffix.
        assert!(info.path.ends_with("\u{00D7}2"));
        assert_eq!(info.detail, "");
        assert_eq!(info.info, "2 nodes selected");
        assert!(!info.is_default);
    }

    #[test]
    fn unknown_selected_id_falls_back_to_default() {
        // A stale selected id that no longer resolves → fall back to the default
        // viewed-struct readout (not a blank bar).
        let (tree, _root, _field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[999_999]), VIEW_NONE, default_type_name);
        assert!(info.is_default);
        assert_eq!(info.path, "UnnamedClass0");
    }

    #[test]
    fn nonzero_offset_renders_compact_hex_suffix() {
        let mut tree = NodeTree::new();
        let n = Node {
            kind: NodeKind::Int8,
            name: "b".into(),
            parent_id: 0,
            offset: 0xAB,
            ..Node::default()
        };
        let i = tree.add_node(n);
        let id = tree.nodes[i].id;
        let info = StatusInfo::from_tree(&tree, &sel(&[id]), VIEW_NONE, default_type_name);
        // The compact "+0xNN" form, upper-case hex, min 2 digits (the C++ suffix).
        assert_eq!(info.detail, "+0xAB");
    }

    #[test]
    fn alias_resolver_is_used_for_type_name() {
        let (tree, root, field) = fixture();
        // Multi-select uses the primary's type name via the resolver; supply an
        // alias to prove the resolver is honoured.
        let info = StatusInfo::from_tree(&tree, &sel(&[root, field]), VIEW_NONE, |_k| {
            "Aliased".into()
        });
        assert_eq!(info.path, "Aliased \u{00D7}2");
    }
}

/// Layout-invariant tests for the render path. The status bar must be a pinned,
/// fixed-height row so the surrounding flex column reserves exactly its height
/// and never pushes it off-screen (the maximized-window clipping bug).
#[cfg(all(test, feature = "ui"))]
mod view_tests {
    use super::view::STATUS_BAR_HEIGHT;

    #[test]
    fn status_bar_height_is_the_pinned_zed_chrome_height() {
        // ~22px Zed chrome strip; this exact value is reserved by `h`/`min_h`/
        // `max_h` so the flex column can neither stretch nor compress the bar.
        assert_eq!(STATUS_BAR_HEIGHT, 22.0);
    }
}
