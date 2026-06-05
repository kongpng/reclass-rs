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
use crate::core::{
    is_container_kind, is_string_kind, is_vector_kind, kind_meta, size_for_kind, NodeKind,
    NodeTree, K_KIND_META,
};

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
    /// The C++ "variant position" segment "↔ uint32_t (3/6)" — the selected
    /// node's kind position within the same-size, non-container variant list the
    /// `←→` type-cycle actually rotates through (`main.cpp:3063`). Empty when the
    /// node is a container or has no sibling variants (the C++ shows "(no variants
    /// for N bytes)" in that case, carried here too). PIC of `reclass_right_click`:
    /// "↔ uint32_t (3/6)".
    pub type_index: String,
    /// The C++ type-cycle key hints "P=ptr F=float S=int U=uint" (`main.cpp:3084`)
    /// — shown for any sized leaf (the keys that retype the selected field).
    /// Empty for containers / the default / multi-select.
    pub key_hints: String,
    /// The viewed root struct/enum summary the C++ appends to the dim part
    /// (`main.cpp:3104`): "Root: 0xNN (decimal)" for a struct, "Root: N members"
    /// for an enum. Empty when there is no struct/enum to summarize. This is the
    /// trailing "BATTERY_REPORTING_SCALE: 0x8 (8)" segment in the reference.
    pub struct_size: String,
}

impl StatusInfo {
    /// `true` when there is nothing at all to show on the left (no selection and
    /// no viewed struct) — the bar then shows only the source/view segments.
    pub fn is_empty(&self) -> bool {
        self.path.is_empty() && self.detail.is_empty() && self.info.is_empty()
    }

    /// Item 30: the Code (rendered) view variant of the readout. The C++ shows
    /// `"Rendered: {main}"` for a pane in `VM_Rendered` (main.cpp:2932), prefixing
    /// the primary line and dropping the offset/variant tail (the rendered pane has
    /// no per-node offset cursor). The host calls this when the active pane is in
    /// Rendered mode. A no-selection default readout is left as-is (nothing to
    /// render-prefix).
    pub fn into_rendered(mut self) -> StatusInfo {
        if self.is_default || self.path.is_empty() {
            return self;
        }
        self.path = format!("Rendered: {}", self.path);
        self.detail = String::new();
        self.type_index = String::new();
        self.key_hints = String::new();
        self
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

        // The viewed root struct/enum summary (the C++ appends it in *every*
        // selection branch; `main.cpp:3096`): "Root: 0xNN (dec)" / "Root: N members".
        let struct_size = struct_size_segment(tree, view_root);

        if count > 1 {
            // Item 30: the C++ `selectionChanged` slot fires AFTER `nodeSelected`
            // and OVERWRITES the whole status to exactly "N nodes selected"
            // (main.cpp:2945, `setAppStatus` with no dim suffix) — no offset, no
            // variant hints, no struct-size tail. Set `path` to that literal and
            // clear every other segment.
            let _ = type_name;
            let _ = root_name;
            let _ = struct_size;
            return StatusInfo {
                path: format!("{count} nodes selected"),
                detail: String::new(),
                info: String::new(),
                is_default: false,
                type_index: String::new(),
                key_hints: String::new(),
                struct_size: String::new(),
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

        // The "↔ uint32_t (pos/total)" variant indicator + the "P=ptr F=float
        // S=int U=uint" type-cycle key hints (`main.cpp:3063-3084`). Only shown for
        // a *sized* leaf (containers / 0-byte kinds get neither); when a sized leaf
        // has no sibling variants the C++ shows "(no variants for N bytes)".
        let (type_index, key_hints) = variant_segments(node.kind, &type_name);

        StatusInfo {
            path,
            detail,
            info,
            is_default: false,
            type_index,
            key_hints,
            struct_size,
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
            // The default branch has no live selection → no variant indicator /
            // key hints. The viewed-struct footprint is already in `info`, so the
            // separate "Root: 0xNN (dec)" segment (a *selection* extra) stays empty.
            type_index: String::new(),
            key_hints: String::new(),
            struct_size: String::new(),
        }
    }
}

/// The variant-cycle segments for a selected leaf's `kind`: the "↔ TypeName
/// (pos/total)" indicator and the "P=ptr F=float S=int U=uint" type-cycle key
/// hints (`main.cpp:3063-3088`). Returns `(type_index, key_hints)`.
///
/// `total` counts the kinds the `←→` cycle actually rotates through: same byte
/// size, non-container, and — unless the current kind is itself a string/vector —
/// excluding string/vector kinds (the exact C++ filter). `pos` is the current
/// kind's 1-based position in that list. With `> 1` variants the indicator reads
/// "↔ TypeName (pos/total)"; with a single variant the C++ shows "(no variants
/// for N bytes)". Either way a sized leaf gets the key hints; a container / 0-byte
/// kind gets neither (both empty).
fn variant_segments(kind: NodeKind, type_name: &str) -> (String, String) {
    let sz = size_for_kind(kind);
    if sz <= 0 {
        // Container / dynamic kind → no variant cycle, no hints (the C++ guards on
        // `sz > 0`).
        return (String::new(), String::new());
    }

    let cur_is_string = is_string_kind(kind);
    let cur_is_vector = is_vector_kind(kind);
    let mut pos = 0;
    let mut total = 0;
    for m in &K_KIND_META {
        if m.size != sz || is_container_kind(m.kind) {
            continue;
        }
        if !cur_is_string && is_string_kind(m.kind) {
            continue;
        }
        if !cur_is_vector && is_vector_kind(m.kind) {
            continue;
        }
        total += 1;
        if m.kind == kind {
            pos = total;
        }
    }

    let type_index = if total > 1 {
        format!("\u{2194} {type_name} ({pos}/{total})")
    } else {
        format!("(no variants for {sz} bytes)")
    };
    let key_hints = "P=ptr F=float S=int U=uint".to_string();
    (type_index, key_hints)
}

/// The viewed root struct/enum summary segment (`main.cpp:3096`): "Root: 0xNN
/// (decimal)" for a struct, "Root: N members" for an enum. Picks the view-root
/// (falling back to the first top-level struct when `view_root == 0`, exactly the
/// C++ `sizeRootId` fallback). Empty when there is no struct/enum to summarize.
fn struct_size_segment(tree: &NodeTree, view_root: u64) -> String {
    let id = if view_root != 0 {
        view_root
    } else {
        // No explicit view-root → the first top-level struct's id (C++ fallback).
        match first_root_idx(tree) {
            i if i >= 0 => tree.nodes[i as usize].id,
            _ => return String::new(),
        }
    };
    let ri = tree.index_of_id(id);
    if ri < 0 {
        return String::new();
    }
    let rn = &tree.nodes[ri as usize];
    let rname = if rn.struct_type_name.is_empty() {
        rn.name.clone()
    } else {
        rn.struct_type_name.clone()
    };
    if rn.is_enum() {
        format!("{rname}: {} members", rn.enum_members.len())
    } else {
        let span = tree.struct_span(id);
        if span > 0 {
            format!("{rname}: 0x{span:X} ({span})")
        } else {
            String::new()
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
    use crate::ui::design::{color, icon, tokens};
    use crate::ui::state::DataSource;
    use crate::ui::theme_apply::ThemeRegistryGlobal;
    use gpui::prelude::FluentBuilder as _;
    use gpui::*;
    use gpui_component::{Icon, Sizable as _};

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

    /// A small leading icon for a status segment, sized + tinted to sit on the
    /// `UI_XS` baseline (the status strip's chrome). Inherits its color from the
    /// caller's `text_color` unless overridden.
    fn segment_icon(ic: Icon) -> impl IntoElement {
        ic.xsmall()
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

        // The C++ selection extras (`reclass_right_click_on_address.png`):
        //   "<Struct>.<field> | +0xNN  ↔ <type> (pos/total)  P=ptr F=float S=int U=uint  <Root>: 0xNN (dec)".
        // The "|" splits the full-contrast path from the dim part; everything after
        // is muted. These only populate for a live selection (empty in the default
        // / multi-select branch), so the bar still reads cleanly when idle.
        let type_index = info.type_index.clone();
        let has_type_index = !type_index.is_empty();
        let key_hints = info.key_hints.clone();
        let has_key_hints = !key_hints.is_empty();
        let struct_size = info.struct_size.clone();
        let has_struct_size = !struct_size.is_empty();
        // The "offset/size" spelled segment is the default branch's right context;
        // for a live selection the compact "+0xNN" + variant cluster carry it, so
        // only surface `info` when there's no richer detail (i.e. the default).
        let show_info_seg = has_info && info.is_default;
        // The dimmed type-cycle key hints sit a touch fainter than the rest (they
        // are a hint, not a readout) — the C++ renders them in the dim part too.
        let hint_color = color::text_disabled(cx);

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
            // Left cluster (the C++ selection line): node path · "|" · +0xNN ·
            // "↔ <type> (pos/total)" · "P=ptr F=float S=int U=uint" · "Root: 0xNN".
            // `min_w_0` + `overflow_hidden` let the long line clip rather than push
            // the right cluster off-screen.
            .child(
                gpui_component::h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .when(has_path, |row| {
                        row.child(div().flex_none().text_color(path_color).child(path.clone()))
                    })
                    // The "|" divider between the full-contrast path and the dim
                    // part (a hairline, like the C++ separator glyph).
                    .when(has_path && (has_detail || has_type_index), |row| {
                        row.child(segment_sep(cx))
                    })
                    .when(has_detail, |row| {
                        row.child(div().flex_none().text_color(muted).child(detail.clone()))
                    })
                    .when(has_type_index, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(type_index.clone()),
                        )
                    })
                    .when(has_key_hints, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_color(hint_color)
                                .child(key_hints.clone()),
                        )
                    })
                    .when(has_struct_size, |row| {
                        row.child(segment_sep(cx)).child(
                            div()
                                .flex_none()
                                .text_color(muted)
                                .child(struct_size.clone()),
                        )
                    }),
            )
            // Right cluster: (default-only offset/size ·) source · theme,
            // hairline-separated. Each carries a small leading SVG icon (the
            // Assets-stage `design::icon_*` set) for Zed chrome polish.
            .child(
                gpui_component::h_flex()
                    .flex_none()
                    .gap(px(tokens::space::MD))
                    .items_center()
                    .when(show_info_seg, |row| {
                        row.child(segment(info_seg.clone(), muted))
                            .child(segment_sep(cx))
                    })
                    .child(
                        gpui_component::h_flex()
                            .flex_none()
                            .gap(px(tokens::space::XS))
                            .items_center()
                            .text_color(source_color)
                            .child(segment_icon(icon::source()))
                            .child(div().flex_none().child(source_label)),
                    )
                    .child(segment_sep(cx))
                    .child(
                        gpui_component::h_flex()
                            .flex_none()
                            .gap(px(tokens::space::XS))
                            .items_center()
                            .text_color(muted)
                            .child(segment_icon(icon::settings()))
                            .child(div().flex_none().child(theme_label)),
                    ),
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
        // The C++ variant indicator + key hints (reference: "↔ uint32_t (3/6)
        // P=ptr F=float S=int U=uint"). Int32 is the 2nd of the six 4-byte,
        // non-container, non-string/vector variants (Hex32, Int32, UInt32, Float,
        // Pointer32, FuncPtr32) → "↔ int32_t (2/6)".
        assert_eq!(info.type_index, "\u{2194} int32_t (2/6)");
        assert_eq!(info.key_hints, "P=ptr F=float S=int U=uint");
        // The trailing viewed-struct summary (the C++ "Root: 0xNN (dec)" tail).
        assert_eq!(info.struct_size, "UnnamedClass0: 0x24 (36)");
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
        // A container (Struct) has no variant cycle and no type-cycle key hints.
        assert_eq!(info.type_index, "");
        assert_eq!(info.key_hints, "");
        // The struct-size tail still describes the viewed root.
        assert_eq!(info.struct_size, "UnnamedClass0: 0x24 (36)");
    }

    #[test]
    fn multi_select_summarizes_type_and_count() {
        // Item 30 (corrected): the C++ `selectionChanged` slot OVERWRITES the whole
        // status to exactly "N nodes selected" — no "Type ×N", no offset detail, no
        // info, no variant hints, and NO struct-size tail. (This test previously
        // encoded the old "Type ×N" + struct-size behavior; updated to match C++.)
        let (tree, root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[root, field]), VIEW_NONE, default_type_name);
        assert_eq!(info.path, "2 nodes selected");
        assert_eq!(info.detail, "");
        assert_eq!(info.info, "");
        assert!(!info.is_default);
        assert_eq!(info.type_index, "");
        assert_eq!(info.key_hints, "");
        assert_eq!(info.struct_size, "");
    }

    #[test]
    fn rendered_view_prefixes_primary_line() {
        // Item 30: a pane in Rendered (Code) view shows "Rendered: {main}" and drops
        // the offset/variant tail.
        let (tree, _root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[field]), VIEW_NONE, default_type_name)
            .into_rendered();
        assert!(info.path.starts_with("Rendered: "), "got {:?}", info.path);
        assert_eq!(info.detail, "");
        assert_eq!(info.type_index, "");
        assert_eq!(info.key_hints, "");
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
        // Item 30: multi-select no longer surfaces the type name (it shows just
        // "N nodes selected"), so prove the resolver is honoured via the SINGLE
        // selection's variant segment instead, which formats the resolved name.
        let (tree, _root, field) = fixture();
        let info = StatusInfo::from_tree(&tree, &sel(&[field]), VIEW_NONE, |_k| "Aliased".into());
        assert!(
            info.type_index.contains("Aliased") || info.path.contains("Aliased"),
            "resolver not honoured: path={:?} type_index={:?}",
            info.path,
            info.type_index
        );
    }

    /// Build a single-root tree of one leaf `kind` at offset 0 and read its
    /// status. Returns the resolved [`StatusInfo`].
    fn leaf_status(kind: NodeKind) -> StatusInfo {
        let mut tree = NodeTree::new();
        let n = Node {
            kind,
            name: "f".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        let i = tree.add_node(n);
        let id = tree.nodes[i].id;
        StatusInfo::from_tree(&tree, &sel(&[id]), VIEW_NONE, default_type_name)
    }

    #[test]
    fn variant_index_matches_cpp_reference_uint32_3_of_6() {
        // The captured C++ reference (`reclass_right_click_on_address.png`) shows
        // "↔ uint32_t (3/6)" for a UInt32 field: of the six 4-byte non-container,
        // non-string/vector variants (Hex32, Int32, UInt32, Float, Pointer32,
        // FuncPtr32) UInt32 is the 3rd.
        let info = leaf_status(NodeKind::UInt32);
        assert_eq!(info.type_index, "\u{2194} uint32_t (3/6)");
        assert_eq!(info.key_hints, "P=ptr F=float S=int U=uint");
    }

    #[test]
    fn variant_index_for_8_byte_leaf_counts_all_eight() {
        // 8-byte non-container, non-string/vector kinds: Hex64, Int64, UInt64,
        // Double, Pointer64, FuncPtr64 (Vec2 is a vector → excluded). Pointer64 is
        // the 5th of those six.
        let info = leaf_status(NodeKind::Pointer64);
        assert_eq!(info.type_index, "\u{2194} ptr64 (5/6)");
    }

    #[test]
    fn sole_variant_size_shows_no_variants_hint_but_keeps_key_hints() {
        // A 64-byte leaf (Mat4x4) is the only kind of its size → the C++ shows
        // "(no variants for 64 bytes)" but still offers the type-cycle key hints.
        let info = leaf_status(NodeKind::Mat4x4);
        assert_eq!(info.type_index, "(no variants for 64 bytes)");
        assert_eq!(info.key_hints, "P=ptr F=float S=int U=uint");
    }

    #[test]
    fn enum_root_struct_size_segment_counts_members() {
        // An enum view-root summarizes as "Name: N members" (the C++ enum branch),
        // not a byte span.
        let mut tree = NodeTree::new();
        let mut e = Node {
            // An enum is a Struct-kind node with the "enum" class keyword (there is
            // no distinct `NodeKind::Enum`; `Node::is_enum` reads the keyword).
            kind: NodeKind::Struct,
            class_keyword: "enum".into(),
            name: "Color".into(),
            struct_type_name: "Color".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        e.enum_members = vec![("Red".into(), 0), ("Green".into(), 1), ("Blue".into(), 2)];
        let ri = tree.add_node(e);
        let id = tree.nodes[ri].id;
        let info = StatusInfo::from_tree(&tree, &sel(&[id]), id, default_type_name);
        assert_eq!(info.struct_size, "Color: 3 members");
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
