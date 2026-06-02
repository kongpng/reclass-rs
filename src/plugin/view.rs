//! Declarative UI vocabulary (design §3) — the small widget tree plugins describe
//! and the **host renders** (Elm-style), so plugin UI is decoupled from our GPUI
//! version and the same model works for native-dynamic and subprocess plugins.
//!
//! ## Phase 1: plain Rust enums
//!
//! Design §3 specifies these as `abi_stable`-derived types (`RVec`/`RString`) so
//! they cross the native FFI boundary. That wrapping is **Phase 3** (gated behind
//! the `plugins` cargo feature). Phase 1 uses plain `Vec`/`String` and keeps the
//! variant *shape* identical, so the later swap (`Vec<T>` → `RVec<T>`, `String` →
//! `RString`) is mechanical and changes no call sites' structure. Nothing here
//! pulls a heavy dependency — std only.
//!
//! The host renderer (design §6 Phase 2) maps each variant onto a Zed-styled
//! `gpui-component` widget; Phase 1 only defines the data the renderer will read.

/// The widget tree a plugin contributes (design §3). Rendered by the host; only
/// plain data crosses the boundary. IDs are opaque strings the plugin chooses and
/// receives back in [`UiEvent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewTree {
    /// Vertical stack.
    Column(Vec<ViewTree>),
    /// Horizontal stack.
    Row(Vec<ViewTree>),
    /// A titled group box wrapping one child subtree.
    Group { title: String, child: Box<ViewTree> },
    /// Static text.
    Label(String),
    /// A push button; emits [`UiEvent::Clicked`] with `id`.
    Button { id: String, label: String },
    /// A single-line text field; submitting emits [`UiEvent::Submitted`].
    TextInput {
        id: String,
        value: String,
        placeholder: String,
    },
    /// A boolean toggle; emits [`UiEvent::Toggled`].
    Checkbox {
        id: String,
        label: String,
        checked: bool,
    },
    /// A single-select dropdown; emits [`UiEvent::Submitted`] with the option.
    Dropdown {
        id: String,
        options: Vec<String>,
        selected: usize,
    },
    /// A table (header row + body rows); a row click emits
    /// [`UiEvent::RowSelected`]. This is the surface the C++ `ProcessPicker`
    /// custom-list ctor becomes (design §1, §3 — the generalized process picker).
    Table {
        id: String,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// A tree of labeled nodes; a node click emits [`UiEvent::RowSelected`].
    Tree { id: String, nodes: Vec<TreeNode> },
    /// A read-only key/value list (e.g. plugin "about" / status detail).
    KeyValue(Vec<(String, String)>),
    /// A horizontal rule / spacer.
    Separator,
}

/// A node in a [`ViewTree::Tree`] (kept distinct from `ViewTree` so the tree's
/// node identity/label is explicit; children nest arbitrarily).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    pub fn leaf(id: impl Into<String>, label: impl Into<String>) -> Self {
        TreeNode {
            id: id.into(),
            label: label.into(),
            children: Vec::new(),
        }
    }
}

/// An event the host routes back to the plugin's
/// [`handle_ui_event`](crate::plugin::contract::Plugin::handle_ui_event) (design
/// §3). The plugin may return a fresh [`ViewTree`] to re-render (Elm update).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// A [`ViewTree::Button`] with this id was clicked.
    Clicked(String),
    /// A [`ViewTree::TextInput`]/[`ViewTree::Dropdown`] with this id submitted
    /// `text`.
    Submitted { id: String, text: String },
    /// A [`ViewTree::Checkbox`] with this id toggled to `on`.
    Toggled { id: String, on: bool },
    /// A row in the [`ViewTree::Table`]/[`ViewTree::Tree`] `table`/tree id was
    /// selected (0-based `row` for tables; node-id-derived index for trees).
    RowSelected { table: String, row: usize },
}

impl ViewTree {
    /// A convenience builder for a labeled group.
    pub fn group(title: impl Into<String>, child: ViewTree) -> ViewTree {
        ViewTree::Group {
            title: title.into(),
            child: Box::new(child),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_tree_nests_and_clones() {
        let tree = ViewTree::Column(vec![
            ViewTree::Label("Pick a target".to_string()),
            ViewTree::group(
                "Process",
                ViewTree::Table {
                    id: "procs".to_string(),
                    columns: vec!["PID".to_string(), "Name".to_string()],
                    rows: vec![vec!["1234".to_string(), "notepad.exe".to_string()]],
                },
            ),
            ViewTree::Separator,
            ViewTree::Button {
                id: "attach".to_string(),
                label: "Attach".to_string(),
            },
        ]);
        // Clone + structural equality round-trip (the shape the host renderer and
        // the future abi_stable swap rely on).
        assert_eq!(tree.clone(), tree);
        let ViewTree::Column(children) = &tree else {
            panic!("expected a column root");
        };
        assert_eq!(children.len(), 4);
        assert!(matches!(children[1], ViewTree::Group { .. }));
    }

    #[test]
    fn ui_event_variants_match_widgets() {
        let click = UiEvent::Clicked("attach".to_string());
        let submit = UiEvent::Submitted {
            id: "target".to_string(),
            text: "1234:notepad.exe".to_string(),
        };
        let toggle = UiEvent::Toggled {
            id: "live".to_string(),
            on: true,
        };
        let row = UiEvent::RowSelected {
            table: "procs".to_string(),
            row: 0,
        };
        // Distinct, comparable, cloneable.
        assert_ne!(click, submit);
        assert_eq!(toggle.clone(), toggle);
        assert_eq!(
            row,
            UiEvent::RowSelected {
                table: "procs".to_string(),
                row: 0
            }
        );
    }

    #[test]
    fn tree_node_leaf_has_no_children() {
        let n = TreeNode::leaf("n1", "Module A");
        assert!(n.children.is_empty());
        assert_eq!(n.label, "Module A");
    }
}
