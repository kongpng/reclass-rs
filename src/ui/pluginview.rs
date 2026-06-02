//! The host-side **declarative-UI renderer** (design §3, §6 Phase 2) — the core
//! Phase-2 deliverable.
//!
//! A plugin contributes UI as a plain-data [`ViewTree`](crate::plugin::view::ViewTree);
//! the **host renders it** with our Zed-styled `gpui-component` widgets and routes
//! events back Elm-style (design §3 — only plain data crosses the ABI, so native
//! plugins stay decoupled from our GPUI version). This module is that renderer:
//! [`render_view_tree`] maps each `ViewTree` variant onto a widget built **only**
//! from [`crate::ui::design`] tokens/color (so plugin UI is auto-themed — design
//! §7.E [+]) plus `gpui-component` widgets. It is reused by both the dock
//! [`Panel`](crate::ui::pluginpanel) and the modal [`Dialog`](crate::ui::plugindialog).
//!
//! ## The mapping (design §3 widget vocabulary → Zed widget)
//!
//! | [`ViewTree`] | widget |
//! |---|---|
//! | `Column` | [`v_flex`](gpui_component::v_flex) |
//! | `Row` | [`h_flex`](gpui_component::h_flex) |
//! | `Group { title, child }` | [`section_label`](crate::ui::design::section_label) + bordered `v_flex` |
//! | `Label` | a themed `div` |
//! | `Button { id, label }` | [`Button`](gpui_component::button::Button) emitting [`UiEvent::Clicked`] |
//! | `TextInput { id, .. }` | [`Input`](gpui_component::input::Input) over a cached [`InputState`] |
//! | `Checkbox { id, .. }` | [`Checkbox`](gpui_component::checkbox::Checkbox) emitting [`UiEvent::Toggled`] |
//! | `Dropdown { id, .. }` | a Zed dropdown trigger emitting [`UiEvent::Submitted`] |
//! | `Table { id, .. }` | a header + clickable rows emitting [`UiEvent::RowSelected`] |
//! | `Tree { id, .. }` | a flattened indented list emitting [`UiEvent::RowSelected`] |
//! | `KeyValue` | a two-column read-only grid |
//! | `Separator` | a 1px rule |
//!
//! ## Elm loop + keyboard input
//!
//! Widgets emit a [`UiEvent`] through the `on_event` callback the host supplies;
//! the host forwards it to `Plugin::handle_ui_event`, whose returned `ViewTree`
//! replaces the panel/dialog tree. `TextInput`s cache a gpui
//! [`InputState`](gpui_component::input::InputState) **per id** in a per-view
//! [`PluginRenderState`] so the input box actually accepts keyboard input across
//! frames (the same deferred-focus / persistent-state shape the editor's
//! inline-edit fix uses) — a freshly built `InputState` every frame would drop
//! focus and keystrokes.
//!
//! The gpui-free parts (event derivation, tree flattening, input-id collection)
//! are unit-tested in this module's `#[cfg(test)]` block; the widget rendering
//! itself is gpui-bound and exercised through the panel/dialog views.
//!
//! Gated behind the `ui` feature.

use crate::plugin::view::{TreeNode, UiEvent, ViewTree};

// ── gpui-free helpers (unit-tested) ──────────────────────────────────────────

/// Collect the ids of every [`ViewTree::TextInput`] in `tree`, in pre-order. The
/// renderer uses this to pre-build a persistent [`InputState`] per input id (so
/// keyboard focus/value survive re-renders), and to gather the current input
/// values when a dialog is submitted (the [`DialogResult`] field set). Pure.
///
/// [`DialogResult`]: crate::plugin::contract::DialogResult
pub fn collect_input_ids(tree: &ViewTree) -> Vec<String> {
    let mut out = Vec::new();
    collect_input_ids_into(tree, &mut out);
    out
}

fn collect_input_ids_into(tree: &ViewTree, out: &mut Vec<String>) {
    match tree {
        ViewTree::TextInput { id, .. } => out.push(id.clone()),
        ViewTree::Column(children) | ViewTree::Row(children) => {
            for c in children {
                collect_input_ids_into(c, out);
            }
        }
        ViewTree::Group { child, .. } => collect_input_ids_into(child, out),
        _ => {}
    }
}

/// Flatten a [`ViewTree::Tree`] node list into `(depth, node)` rows in pre-order,
/// so the renderer can paint an indented flat list (gpui-component has a
/// virtualized tree, but a plugin tree is small and a flat indented list keeps
/// the renderer simple + uniform with the table). Pure.
pub fn flatten_tree(nodes: &[TreeNode]) -> Vec<(usize, &TreeNode)> {
    let mut out = Vec::new();
    fn walk<'a>(nodes: &'a [TreeNode], depth: usize, out: &mut Vec<(usize, &'a TreeNode)>) {
        for n in nodes {
            out.push((depth, n));
            walk(&n.children, depth + 1, out);
        }
    }
    walk(nodes, 0, &mut out);
    out
}

/// The [`UiEvent`] a [`ViewTree::Button`] click produces — [`UiEvent::Clicked`]
/// with the button's id. A tiny pure seam so the (gpui-bound) `on_click` closure
/// and the tests agree on the event shape.
pub fn button_event(id: &str) -> UiEvent {
    UiEvent::Clicked(id.to_string())
}

/// The [`UiEvent`] a [`ViewTree::Checkbox`] toggle produces.
pub fn checkbox_event(id: &str, on: bool) -> UiEvent {
    UiEvent::Toggled {
        id: id.to_string(),
        on,
    }
}

/// The [`UiEvent`] a [`ViewTree::Table`]/[`ViewTree::Tree`] row click produces.
pub fn row_event(view_id: &str, row: usize) -> UiEvent {
    UiEvent::RowSelected {
        table: view_id.to_string(),
        row,
    }
}

/// The [`UiEvent`] a [`ViewTree::Dropdown`] selection produces — [`UiEvent::Submitted`]
/// with the chosen option text.
pub fn dropdown_event(id: &str, option: &str) -> UiEvent {
    UiEvent::Submitted {
        id: id.to_string(),
        text: option.to_string(),
    }
}

// ── gpui rendering ───────────────────────────────────────────────────────────

#[cfg(feature = "ui")]
pub use render::{render_view_tree, PluginRenderState};

#[cfg(feature = "ui")]
mod render {
    use super::*;
    use crate::ui::design::{color, section_label, tokens};
    use gpui::{
        div, px, App, AppContext as _, Entity, FontWeight, InteractiveElement as _, IntoElement,
        ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, Window,
    };
    use gpui_component::input::InputState;
    use gpui_component::ActiveTheme as _;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// The host event callback as a cheap-to-clone trait object. Using
    /// `Rc<dyn Fn>` (not `impl Fn + Clone`) keeps the recursive [`render_node`]
    /// **non-generic**, so it instantiates once instead of monomorphizing a new
    /// (ever-growing) closure type at every recursion depth — which the deeply
    /// nested generic form did, crashing rustc's codegen on the full crate.
    pub type OnEvent = Rc<dyn Fn(UiEvent, &mut Window, &mut App)>;

    /// Per-view render state the host owns and threads into [`render_view_tree`]
    /// across frames. It caches one [`InputState`] entity per `TextInput` id so a
    /// text box keeps its focus + value between renders (a fresh `InputState`
    /// every frame would lose keystrokes — the deferred-focus lesson from the
    /// editor inline-edit fix). The panel/dialog view holds one of these.
    #[derive(Default)]
    pub struct PluginRenderState {
        inputs: HashMap<String, Entity<InputState>>,
    }

    impl PluginRenderState {
        /// A fresh, empty render state.
        pub fn new() -> Self {
            PluginRenderState::default()
        }

        /// Ensure an [`InputState`] exists for every `TextInput` id in `tree`,
        /// seeded with the tree's current value/placeholder, and drop states for
        /// inputs no longer present (so the cache tracks the live tree). Call once
        /// before rendering a (possibly re-rendered) tree.
        pub fn sync(&mut self, tree: &ViewTree, window: &mut Window, cx: &mut App) {
            let ids = collect_input_ids(tree);
            // Add any missing inputs, seeded from the tree.
            for id in &ids {
                if !self.inputs.contains_key(id) {
                    let (value, placeholder) = find_input_seed(tree, id);
                    let state = cx.new(|cx| {
                        let mut s = InputState::new(window, cx).placeholder(placeholder);
                        if !value.is_empty() {
                            s.set_value(value, window, cx);
                        }
                        s
                    });
                    self.inputs.insert(id.clone(), state);
                }
            }
            // Drop stale inputs.
            self.inputs.retain(|k, _| ids.iter().any(|id| id == k));
        }

        /// The cached [`InputState`] for `id`, if synced.
        pub fn input(&self, id: &str) -> Option<&Entity<InputState>> {
            self.inputs.get(id)
        }

        /// The current `(id, value)` pairs of every cached text input — the field
        /// set a submitted dialog reports as its
        /// [`DialogResult`](crate::plugin::contract::DialogResult).
        pub fn input_values(&self, cx: &App) -> Vec<(String, String)> {
            self.inputs
                .iter()
                .map(|(id, state)| (id.clone(), state.read(cx).value().to_string()))
                .collect()
        }
    }

    /// Find the `(value, placeholder)` to seed the input `id` with from `tree`.
    fn find_input_seed(tree: &ViewTree, id: &str) -> (String, String) {
        match tree {
            ViewTree::TextInput {
                id: tid,
                value,
                placeholder,
            } if tid == id => (value.clone(), placeholder.clone()),
            ViewTree::Column(children) | ViewTree::Row(children) => {
                for c in children {
                    let seed = find_input_seed(c, id);
                    if !seed.0.is_empty() || !seed.1.is_empty() {
                        return seed;
                    }
                }
                (String::new(), String::new())
            }
            ViewTree::Group { child, .. } => find_input_seed(child, id),
            _ => (String::new(), String::new()),
        }
    }

    /// Render a plugin [`ViewTree`] into a Zed-styled element (design §3, §6
    /// Phase 2). `view_id` is the contributing view's id (used to scope element
    /// ids + table/tree row events); `on_event` is the host callback every
    /// interactive widget emits a [`UiEvent`] through (the Elm loop); `state` is
    /// the per-view [`PluginRenderState`] (already [`sync`](PluginRenderState::sync)ed)
    /// providing persistent text inputs.
    ///
    /// `on_event` is an [`OnEvent`] (`Rc<dyn Fn>`) so it is cheaply cloned into
    /// each interactive child's closure; the host typically wraps a weak-entity
    /// update. Accepts any `Fn(UiEvent, &mut Window, &mut App) + 'static` and
    /// boxes it.
    pub fn render_view_tree(
        tree: &ViewTree,
        view_id: &str,
        on_event: impl Fn(UiEvent, &mut Window, &mut App) + 'static,
        state: &PluginRenderState,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let on_event: OnEvent = Rc::new(on_event);
        // A stable, unique element-id path so sibling widgets don't collide.
        render_node(tree, view_id, &mut 0, &on_event, state, cx)
    }

    /// Render one node; `seq` is a monotonically increasing counter making each
    /// element id unique within the view (gpui interactivity requirement).
    fn render_node(
        tree: &ViewTree,
        view_id: &str,
        seq: &mut usize,
        on_event: &OnEvent,
        state: &PluginRenderState,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let this_seq = *seq;
        *seq += 1;
        match tree {
            ViewTree::Column(children) => {
                let mut col = gpui_component::v_flex().gap(px(tokens::space::MD)).w_full();
                for c in children {
                    col = col.child(render_node(c, view_id, seq, on_event, state, cx));
                }
                col.into_any_element()
            }
            ViewTree::Row(children) => {
                let mut row = gpui_component::h_flex()
                    .gap(px(tokens::space::MD))
                    .items_center();
                for c in children {
                    row = row.child(render_node(c, view_id, seq, on_event, state, cx));
                }
                row.into_any_element()
            }
            ViewTree::Group { title, child } => {
                // section_label + a bordered v_flex (design §7.E [+]).
                gpui_component::v_flex()
                    .w_full()
                    .gap(px(tokens::space::SM))
                    .child(section_label(title.clone(), cx))
                    .child(
                        gpui_component::v_flex()
                            .w_full()
                            .gap(px(tokens::space::SM))
                            .p(px(tokens::space::MD))
                            .rounded(px(tokens::radius::MD))
                            .border_1()
                            .border_color(color::border(cx))
                            .child(render_node(child, view_id, seq, on_event, state, cx)),
                    )
                    .into_any_element()
            }
            ViewTree::Label(text) => div()
                .text_size(px(tokens::font::UI_MD))
                .text_color(color::text(cx))
                .child(SharedString::from(text.clone()))
                .into_any_element(),
            ViewTree::Button { id, label } => {
                use gpui_component::button::Button;
                use gpui_component::Sizable as _;
                let id = id.clone();
                let cb = on_event.clone();
                Button::new(SharedString::from(format!("{view_id}.btn.{this_seq}")))
                    .small()
                    .label(label.clone())
                    .on_click(move |_e, window, cx| {
                        cb(button_event(&id), window, cx);
                    })
                    .into_any_element()
            }
            ViewTree::TextInput {
                id,
                value: _,
                placeholder,
            } => {
                use gpui_component::input::Input;
                // Use the cached persistent InputState so the box accepts
                // keyboard input across frames; fall back to a placeholder-only
                // read-only label if the state wasn't synced (defensive).
                if let Some(input_state) = state.input(id) {
                    Input::new(input_state).w_full().into_any_element()
                } else {
                    div()
                        .w_full()
                        .px(px(tokens::space::MD))
                        .py(px(tokens::space::XS))
                        .rounded(px(tokens::radius::MD))
                        .border_1()
                        .border_color(color::border(cx))
                        .text_color(color::text_muted(cx))
                        .child(SharedString::from(placeholder.clone()))
                        .into_any_element()
                }
            }
            ViewTree::Checkbox { id, label, checked } => {
                use gpui_component::checkbox::Checkbox;
                let id_s = id.clone();
                let cb = on_event.clone();
                Checkbox::new(SharedString::from(format!("{view_id}.chk.{this_seq}")))
                    .label(label.clone())
                    .checked(*checked)
                    .on_click(move |checked: &bool, window, cx| {
                        cb(checkbox_event(&id_s, *checked), window, cx);
                    })
                    .into_any_element()
            }
            ViewTree::Dropdown {
                id,
                options,
                selected,
            } => render_dropdown(id, options, *selected, view_id, this_seq, on_event, cx),
            ViewTree::Table { id, columns, rows } => render_table(id, columns, rows, on_event, cx),
            ViewTree::Tree { id, nodes } => render_tree(id, nodes, on_event, cx),
            ViewTree::KeyValue(pairs) => {
                let mut col = gpui_component::v_flex().w_full().gap(px(tokens::space::XS));
                for (k, v) in pairs {
                    col = col.child(
                        gpui_component::h_flex()
                            .w_full()
                            .gap(px(tokens::space::MD))
                            .items_center()
                            .child(
                                div()
                                    .w(px(140.))
                                    .flex_none()
                                    .text_size(px(tokens::font::UI_SM))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(color::text_muted(cx))
                                    .child(SharedString::from(k.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(tokens::font::UI_SM))
                                    .text_color(color::text(cx))
                                    .child(SharedString::from(v.clone())),
                            ),
                    );
                }
                col.into_any_element()
            }
            ViewTree::Separator => div()
                .w_full()
                .h(px(tokens::border::THIN))
                .my(px(tokens::space::XS))
                .bg(color::border(cx))
                .into_any_element(),
        }
    }

    /// A simple Zed dropdown: a trigger button that, when clicked, cycles to the
    /// next option and emits [`UiEvent::Submitted`] (a full popover-list dropdown
    /// like the options dialog is overkill for the small plugin case; the plugin
    /// gets the chosen option text and re-renders). Shows the current selection.
    fn render_dropdown(
        id: &str,
        options: &[String],
        selected: usize,
        view_id: &str,
        seq: usize,
        on_event: &OnEvent,
        _cx: &mut App,
    ) -> gpui::AnyElement {
        use gpui_component::button::Button;
        use gpui_component::Sizable as _;
        let label = options
            .get(selected)
            .cloned()
            .unwrap_or_else(|| "\u{2014}".to_string());
        let next = if options.is_empty() {
            None
        } else {
            options.get((selected + 1) % options.len()).cloned()
        };
        let id_s = id.to_string();
        let cb = on_event.clone();
        Button::new(SharedString::from(format!("{view_id}.dd.{seq}")))
            .small()
            .outline()
            .label(format!("{label}  \u{25be}"))
            .on_click(move |_e, window, cx| {
                if let Some(opt) = &next {
                    cb(dropdown_event(&id_s, opt), window, cx);
                }
            })
            .into_any_element()
    }

    /// A header row + clickable body rows; a row click emits
    /// [`UiEvent::RowSelected`] keyed on the table id (design §3 — the generalized
    /// `selectTarget` process picker). Monospace cells, Zed list-row hover.
    fn render_table(
        id: &str,
        columns: &[String],
        rows: &[Vec<String>],
        on_event: &OnEvent,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let header = {
            let mut h = gpui_component::h_flex()
                .w_full()
                .px(px(tokens::space::SM))
                .py(px(tokens::space::XS))
                .border_b_1()
                .border_color(color::border(cx));
            for c in columns {
                h = h.child(
                    div()
                        .flex_1()
                        .text_size(px(tokens::font::UI_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color::text_muted(cx))
                        .child(SharedString::from(c.clone())),
                );
            }
            h
        };

        let table_id = id.to_string();
        let mut body = gpui_component::v_flex().w_full();
        for (ri, row) in rows.iter().enumerate() {
            let cb = on_event.clone();
            let tid = table_id.clone();
            let mut row_el = gpui_component::h_flex()
                .id(("plugin-table-row", ri))
                .w_full()
                .px(px(tokens::space::SM))
                .py(px(2.))
                .gap(px(tokens::space::SM))
                .items_center()
                .rounded(px(tokens::radius::SM))
                .cursor_pointer()
                .hover(|s| s.bg(color::hover_overlay(cx)))
                .on_click(move |_e, window, cx| {
                    cb(row_event(&tid, ri), window, cx);
                });
            for cell in row {
                row_el = row_el.child(
                    div()
                        .flex_1()
                        .text_size(px(tokens::font::UI_SM))
                        .text_color(color::text(cx))
                        .child(SharedString::from(cell.clone())),
                );
            }
            body = body.child(row_el);
        }

        gpui_component::v_flex()
            .w_full()
            .rounded(px(tokens::radius::MD))
            .border_1()
            .border_color(color::border(cx))
            .bg(cx.theme().background)
            .overflow_hidden()
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// A flattened, indented clickable node list (a small plugin tree). A node
    /// click emits [`UiEvent::RowSelected`] with the node's pre-order index.
    fn render_tree(
        id: &str,
        nodes: &[TreeNode],
        on_event: &OnEvent,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let flat = flatten_tree(nodes);
        let tree_id = id.to_string();
        let mut col = gpui_component::v_flex().w_full().gap(px(1.));
        for (ri, (depth, node)) in flat.into_iter().enumerate() {
            let cb = on_event.clone();
            let tid = tree_id.clone();
            col = col.child(
                gpui_component::h_flex()
                    .id(("plugin-tree-row", ri))
                    .w_full()
                    .pl(px(tokens::space::SM + depth as f32 * tokens::space::LG))
                    .pr(px(tokens::space::SM))
                    .py(px(2.))
                    .items_center()
                    .rounded(px(tokens::radius::SM))
                    .cursor_pointer()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)))
                    .on_click(move |_e, window, cx| {
                        cb(row_event(&tid, ri), window, cx);
                    })
                    .child(SharedString::from(node.label.clone())),
            );
        }
        col.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::view::{TreeNode, UiEvent, ViewTree};

    #[test]
    fn collect_input_ids_walks_nested_tree() {
        let tree = ViewTree::Column(vec![
            ViewTree::TextInput {
                id: "a".to_string(),
                value: String::new(),
                placeholder: String::new(),
            },
            ViewTree::group(
                "g",
                ViewTree::Row(vec![
                    ViewTree::TextInput {
                        id: "b".to_string(),
                        value: "v".to_string(),
                        placeholder: String::new(),
                    },
                    ViewTree::Label("x".to_string()),
                ]),
            ),
            ViewTree::Button {
                id: "btn".to_string(),
                label: "Go".to_string(),
            },
        ]);
        assert_eq!(collect_input_ids(&tree), ["a", "b"]);
    }

    #[test]
    fn collect_input_ids_empty_when_no_inputs() {
        let tree = ViewTree::Column(vec![
            ViewTree::Label("only text".to_string()),
            ViewTree::Separator,
        ]);
        assert!(collect_input_ids(&tree).is_empty());
    }

    #[test]
    fn flatten_tree_is_preorder_with_depth() {
        let nodes = vec![
            TreeNode {
                id: "root".to_string(),
                label: "Root".to_string(),
                children: vec![
                    TreeNode::leaf("c1", "Child 1"),
                    TreeNode {
                        id: "c2".to_string(),
                        label: "Child 2".to_string(),
                        children: vec![TreeNode::leaf("g1", "Grandchild")],
                    },
                ],
            },
            TreeNode::leaf("sib", "Sibling"),
        ];
        let flat = flatten_tree(&nodes);
        let shape: Vec<(usize, &str)> = flat.iter().map(|(d, n)| (*d, n.label.as_str())).collect();
        assert_eq!(
            shape,
            [
                (0, "Root"),
                (1, "Child 1"),
                (1, "Child 2"),
                (2, "Grandchild"),
                (0, "Sibling"),
            ]
        );
    }

    #[test]
    fn event_constructors_match_variants() {
        assert_eq!(button_event("go"), UiEvent::Clicked("go".to_string()));
        assert_eq!(
            checkbox_event("live", true),
            UiEvent::Toggled {
                id: "live".to_string(),
                on: true
            }
        );
        assert_eq!(
            row_event("procs", 3),
            UiEvent::RowSelected {
                table: "procs".to_string(),
                row: 3
            }
        );
        assert_eq!(
            dropdown_event("mode", "fast"),
            UiEvent::Submitted {
                id: "mode".to_string(),
                text: "fast".to_string()
            }
        );
    }
}
