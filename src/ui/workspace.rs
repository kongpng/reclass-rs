//! Workspace explorer — the "Project" dock: a tree of every struct / enum /
//! union across all open documents, with quick navigation (click → open in a
//! tab) and a text filter.
//!
//! Port of the C++ workspace model + dock (app-shell §10 `createWorkspaceDock`,
//! `workspace_model.h` `syncProjectExplorer` / `WorkspaceDelegate`). In Qt this
//! was a `QStandardItemModel` + `QSortFilterProxyModel` + a custom delegate; in
//! gpui-component it is a virtualized [`Tree`](gpui_component::tree) fed by a
//! pure, testable model the engine builds from the open document trees.
//!
//! Two parts.
//!
//! [`WorkspaceModel`] is the gpui-free model: section rows (PINNED / ALL TYPES)
//! plus a type row per top-level struct/enum/union, each carrying its owning
//! `DocId`, node id, kind badge, and member-field count, plus child rows for a
//! struct's fields. It is **sorted by field count** (the stage's stated
//! ordering; the C++ delegate showed the count as a right-aligned pill), built
//! by [`WorkspaceModel::build`] and unit-tested headlessly.
//!
//! [`WorkspacePanel`] is the gpui-component dock `Panel` hosting a filter input
//! above a virtualized tree; clicking a type row routes a [`WorkspaceNav`]
//! request up to the window (quick navigation).
//!
//! Gated behind the `ui` feature.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::input::{Input, InputState};
use gpui_component::list::ListItem;
use gpui_component::tree::{tree, TreeItem, TreeState};

use crate::core::{kind_to_string, NodeKind, NodeTree};
use crate::ui::design::{color, tokens};

use super::state::DocId;

/// The badge a workspace row shows — the C++ `S`/`E`/`F` letter badge
/// (`WorkspaceDelegate::paint`), generalized to distinguish unions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeBadge {
    /// A `struct`/`class` top-level type (badge `S`).
    Struct,
    /// An `enum` top-level type (badge `E`).
    Enum,
    /// A `union` top-level type (badge `U`).
    Union,
    /// A struct field child row (badge `F`).
    Field,
}

impl TypeBadge {
    /// The single-letter badge glyph (`WorkspaceDelegate` letter).
    pub fn letter(self) -> char {
        match self {
            TypeBadge::Struct => 'S',
            TypeBadge::Enum => 'E',
            TypeBadge::Union => 'U',
            TypeBadge::Field => 'F',
        }
    }
}

/// One workspace tree row (a section header, a top-level type, or a field child).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkspaceRow {
    /// A non-interactive section header (PINNED / ALL TYPES). The C++
    /// `makeSectionItem` (`RoleSectionHeader`).
    Section(String),
    /// A top-level type entry — a navigable struct/enum/union.
    Type(TypeEntry),
}

/// A navigable top-level type row: which document + node it is, its display
/// name, badge, and member-field count (the right-aligned pill).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeEntry {
    /// The owning open document (the C++ dock pointer; here a stable [`DocId`]).
    pub doc: DocId,
    /// The struct/enum node id (`Qt::UserRole + 1`).
    pub id: u64,
    /// The display name (`structTypeName` or `name`, "(unnamed)" fallback).
    pub name: String,
    /// `S`/`E`/`U` badge.
    pub badge: TypeBadge,
    /// Member-field count (`typeDisplayString` count: non-hex-pad members, or
    /// enum-member count for enums) — also the sort key.
    pub field_count: usize,
    /// Whether this type is currently viewed in some tab (`Qt::UserRole + 3`;
    /// drives the badge's dimmed state — un-viewed types are dimmer).
    pub viewed: bool,
    /// The struct's field child rows (empty for enums; the C++
    /// `buildStructChildren`). Rendered as the tree's expandable children.
    pub children: Vec<FieldChild>,
}

/// A struct field child row — `"<TypeName> <fieldName>"` (the C++
/// `buildStructChildren` child display).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldChild {
    /// The field node id.
    pub id: u64,
    /// The member's type name (`memberTypeName`: a struct's name/keyword, else
    /// the primitive kind string).
    pub type_name: String,
    /// The member's field name.
    pub field_name: String,
    /// The member's byte offset within its parent struct (`Node.offset`).
    /// Rendered as a muted `+0xNN` prefix so nested rows are distinguishable
    /// at a glance instead of reading as a flat list of identical gray rows
    /// (the editor's address column on a collapsed struct's members).
    pub offset: i32,
}

impl FieldChild {
    /// The combined display string (`"TypeName fieldName"`; the C++
    /// `childDisplay`). Trailing space is trimmed when the field is unnamed.
    ///
    /// NOTE: this is the **filter/label** string and must stay byte-identical to
    /// the C++ `childDisplay` (the headless tests + `model_to_tree_items` assert
    /// it). The offset is shown only in the rich-rendered row, never folded into
    /// this string.
    pub fn display(&self) -> String {
        if self.field_name.is_empty() {
            self.type_name.clone()
        } else {
            format!("{} {}", self.type_name, self.field_name)
        }
    }
}

/// Format a member offset as the editor-style `+0xNN` address chip (uppercase
/// hex, no leading zeros — matching the editor gutter's compact relative
/// offsets). Pure + unit-tested.
fn format_field_offset(offset: i32) -> String {
    // Negative offsets should not occur for laid-out members, but render them
    // safely (sign before the `0x`) rather than as a huge unsigned hex.
    if offset < 0 {
        format!("-0x{:X}", -(offset as i64))
    } else {
        format!("+0x{:X}", offset)
    }
}

/// One open document, as the workspace model sees it: its [`DocId`] + tree.
///
/// The window passes a slice of these (one per *distinct* document; the C++
/// dedups docks sharing a doc via `seenDocs`). The model only reads the tree.
pub struct WorkspaceDoc<'a> {
    pub doc: DocId,
    pub tree: &'a NodeTree,
}

/// `isHexPad` (`workspace_model.h:34`) — Hex8/16/32/64 are padding members
/// excluded from the field count + child rows. Note Hex128 is **not** a pad
/// (matches the C++ which only lists the four).
fn is_hex_pad(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Hex8 | NodeKind::Hex16 | NodeKind::Hex32 | NodeKind::Hex64
    )
}

/// The display name of a top-level type (`structTypeName` or `name`, else
/// "(unnamed)"; the C++ `nameOf` + the context-menu "(unnamed)" fallback).
fn type_name(n: &crate::core::Node) -> String {
    let s = if n.struct_type_name.is_empty() {
        n.name.as_str()
    } else {
        n.struct_type_name.as_str()
    };
    if s.is_empty() {
        "(unnamed)".to_string()
    } else {
        s.to_string()
    }
}

/// The member type name for a field child (`memberTypeName`,
/// `workspace_model.h:50`): a nested struct → its name or class keyword; any
/// other kind → the primitive kind string.
fn member_type_name(m: &crate::core::Node) -> String {
    if m.kind == NodeKind::Struct {
        if m.struct_type_name.is_empty() {
            m.resolved_class_keyword().to_string()
        } else {
            m.struct_type_name.clone()
        }
    } else {
        kind_to_string(m.kind).to_string()
    }
}

/// The complete workspace tree model — the data the tree renders.
///
/// Built by [`WorkspaceModel::build`] from the open documents; pure + testable.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct WorkspaceModel {
    /// The rows in display order (section headers interleaved with type rows).
    pub rows: Vec<WorkspaceRow>,
    /// Header counts for the dock title (`"Project — N structs · M enums"`;
    /// the C++ `m_dockTitleLabel` text). Unions count as structs (the C++
    /// counts everything non-enum as a struct).
    pub struct_count: usize,
    pub enum_count: usize,
}

impl WorkspaceModel {
    /// Build the model from the open documents (the C++ `buildProjectExplorer`
    /// and `syncProjectExplorer`), with the type rows **sorted by field count**
    /// (descending — the busiest types first; the stage's stated ordering).
    ///
    /// Faithful structure: an optional `PINNED` section, then an `ALL TYPES`
    /// section whose rows are every top-level struct/enum/union across all
    /// documents. `viewed_ids` marks rows currently shown in a tab (the badge
    /// dim state). `pinned_ids` selects the PINNED section.
    pub fn build(
        docs: &[WorkspaceDoc<'_>],
        pinned_ids: &[u64],
        viewed_ids: &[u64],
    ) -> WorkspaceModel {
        let mut entries: Vec<TypeEntry> = Vec::new();

        for d in docs {
            for &idx in d.tree.children_of(0).iter() {
                let n = &d.tree.nodes[idx];
                // Top-level types are Struct nodes; the class keyword
                // distinguishes struct/class/union/enum (the C++ only walks
                // NodeKind::Struct top-levels).
                if n.kind != NodeKind::Struct {
                    continue;
                }
                let keyword = n.resolved_class_keyword();
                let is_enum = keyword == "enum";
                let is_union = keyword == "union";
                let badge = if is_enum {
                    TypeBadge::Enum
                } else if is_union {
                    TypeBadge::Union
                } else {
                    TypeBadge::Struct
                };

                // Field count + children (enums use their member count and have
                // no field children; the C++ `typeDisplayString`).
                let (field_count, children) = if is_enum {
                    (n.enum_members.len(), Vec::new())
                } else {
                    Self::struct_children(d.tree, n.id, d.doc)
                };

                entries.push(TypeEntry {
                    doc: d.doc,
                    id: n.id,
                    name: type_name(n),
                    badge,
                    field_count,
                    viewed: viewed_ids.contains(&n.id),
                    children,
                });
            }
        }

        // ── Sort by field count (descending), then name for a stable order. ──
        entries.sort_by(|a, b| {
            b.field_count
                .cmp(&a.field_count)
                .then_with(|| a.name.cmp(&b.name))
        });

        let struct_count = entries
            .iter()
            .filter(|e| e.badge != TypeBadge::Enum)
            .count();
        let enum_count = entries
            .iter()
            .filter(|e| e.badge == TypeBadge::Enum)
            .count();

        let mut rows = Vec::new();

        // ── PINNED section (only if any pinned). ──
        let pinned: Vec<&TypeEntry> = entries
            .iter()
            .filter(|e| pinned_ids.contains(&e.id))
            .collect();
        if !pinned.is_empty() {
            rows.push(WorkspaceRow::Section("PINNED".to_string()));
            for e in &pinned {
                rows.push(WorkspaceRow::Type((*e).clone()));
            }
        }

        // ── ALL TYPES section. ──
        rows.push(WorkspaceRow::Section("ALL TYPES".to_string()));
        for e in entries {
            rows.push(WorkspaceRow::Type(e));
        }

        WorkspaceModel {
            rows,
            struct_count,
            enum_count,
        }
    }

    /// The field child rows of a struct (`buildStructChildren`): members sorted
    /// by offset, hex-padding members skipped.
    fn struct_children(tree: &NodeTree, struct_id: u64, _doc: DocId) -> (usize, Vec<FieldChild>) {
        let mut members = tree.children_of(struct_id);
        members.sort_by(|&a, &b| tree.nodes[a].offset.cmp(&tree.nodes[b].offset));

        let mut children = Vec::new();
        for mi in members {
            let m = &tree.nodes[mi];
            if is_hex_pad(m.kind) {
                continue;
            }
            children.push(FieldChild {
                id: m.id,
                type_name: member_type_name(m),
                field_name: m.name.clone(),
                offset: m.offset,
            });
        }
        let count = children.len();
        (count, children)
    }

    /// The dock title (`m_dockTitleLabel` text; app-shell §10): `"Project"` plus
    /// a `" — N structs · M enums"` suffix when any types exist. `dirty` adds the
    /// leading `•` modified marker.
    pub fn dock_title(&self, dirty: bool) -> String {
        let mut s = String::new();
        if dirty {
            s.push_str("\u{2022} ");
        }
        s.push_str("Project");
        if self.struct_count > 0 || self.enum_count > 0 {
            s.push_str(&format!(
                " \u{2014} {} struct{}",
                self.struct_count,
                if self.struct_count != 1 { "s" } else { "" }
            ));
            if self.enum_count > 0 {
                s.push_str(&format!(
                    " \u{b7} {} enum{}",
                    self.enum_count,
                    if self.enum_count != 1 { "s" } else { "" }
                ));
            }
        }
        s
    }

    /// All navigable type entries, in row order (test/iteration helper).
    pub fn type_entries(&self) -> impl Iterator<Item = &TypeEntry> {
        self.rows.iter().filter_map(|r| match r {
            WorkspaceRow::Type(t) => Some(t),
            WorkspaceRow::Section(_) => None,
        })
    }

    /// Filter the type rows to those whose name (or any field child) matches the
    /// lowercased `query` (the C++ `QSortFilterProxyModel` + the proxy that hides
    /// section headers while filtering, `WorkspaceProxyModel`). An empty query
    /// returns the model unchanged. Section headers are dropped when filtering.
    pub fn filtered(&self, query: &str) -> WorkspaceModel {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.clone();
        }
        let rows: Vec<WorkspaceRow> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                WorkspaceRow::Section(_) => None,
                WorkspaceRow::Type(t) => {
                    let name_hit = t.name.to_lowercase().contains(&q);
                    let child_hit = t
                        .children
                        .iter()
                        .any(|c| c.display().to_lowercase().contains(&q));
                    (name_hit || child_hit).then(|| WorkspaceRow::Type(t.clone()))
                }
            })
            .collect();
        WorkspaceModel {
            rows,
            struct_count: self.struct_count,
            enum_count: self.enum_count,
        }
    }
}

// ── The gpui dock panel ──────────────────────────────────────────────────────

/// A quick-navigation request raised when a workspace type row is clicked
/// (the C++ "Open in Current Tab" default; app-shell §10 context menu). The
/// window resolves it: set the owning document's view-root to `node_id` and
/// activate its tab.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceNav {
    pub doc: DocId,
    pub node_id: u64,
}

/// Encode a `(DocId, node_id)` into a stable [`TreeItem`] id string.
fn nav_item_id(doc: DocId, node_id: u64) -> SharedString {
    SharedString::from(format!("{}:{}", doc.get(), node_id))
}

/// Decode a [`TreeItem`] id string back into `(DocId, node_id)`; `None` for
/// section/field rows (their ids do not encode a nav target).
fn parse_nav_item_id(id: &str) -> Option<WorkspaceNav> {
    let (d, n) = id.split_once(':')?;
    let doc = DocId::from_raw(d.parse::<u64>().ok()?);
    let node_id = n.parse::<u64>().ok()?;
    Some(WorkspaceNav { doc, node_id })
}

/// Convert a built [`WorkspaceModel`] into gpui-component [`TreeItem`]s: one item
/// per type row (id = `"<doc>:<id>"`, label = `"Name — count"`), with its field
/// children nested (non-navigable ids). Section headers are flattened into
/// disabled marker items so the strip still shows the PINNED / ALL TYPES bands.
fn model_to_tree_items(model: &WorkspaceModel) -> Vec<TreeItem> {
    let mut items = Vec::new();
    for (ri, row) in model.rows.iter().enumerate() {
        match row {
            WorkspaceRow::Section(label) => {
                items.push(
                    TreeItem::new(SharedString::from(format!("section-{ri}")), label.clone())
                        .disabled(true),
                );
            }
            WorkspaceRow::Type(t) => {
                let label = format!("{} \u{2014} {}", t.name, t.field_count);
                let mut item = TreeItem::new(nav_item_id(t.doc, t.id), label);
                for child in &t.children {
                    item = item.child(TreeItem::new(
                        SharedString::from(format!("field-{}-{}", t.id, child.id)),
                        child.display(),
                    ));
                }
                items.push(item);
            }
        }
    }
    items
}

// ── Rich row metadata (Zed styling) ──────────────────────────────────────────
//
// `TreeItem` only carries an `id` + a flat `label` (kept for filtering + the
// headless tests). To paint Zed-style rows — a colored type badge, a truncated
// name, a muted trailing count pill, and the syntax-tinted "Type field" child
// rows (the C++ `WorkspaceDelegate::paint`) — the panel keeps a side table keyed
// by tree-item id. The render closure looks the metadata up by `item.id`, so the
// flattened-label `TreeItem`s stay byte-identical to what the model tests assert.

/// What kind of row a tree entry is, for styling.
#[derive(Clone, PartialEq, Eq, Debug)]
enum RowMetaKind {
    /// A PINNED / ALL TYPES band header (non-interactive caption + hairline).
    Section,
    /// A top-level type: badge + name + count pill. `viewed` dims un-opened
    /// types' badge (the C++ `Qt::UserRole + 3` dim state).
    Type {
        badge: TypeBadge,
        name: String,
        field_count: usize,
        viewed: bool,
    },
    /// A struct field child: a muted `+0xNN` offset chip (the editor's address
    /// column, brought onto collapsed nested rows so they are not a flat list
    /// of identical gray rows), then `type_name` in the syntax-type tint and
    /// `field_name` muted (the C++ child paint path). No live value preview —
    /// the workspace model carries only the `NodeTree`, not a memory source.
    Field {
        offset: i32,
        type_name: String,
        field_name: String,
    },
}

/// Build the per-tree-item metadata table the render closure reads, in lockstep
/// with [`model_to_tree_items`] (same ids). Cheap to rebuild on every filter
/// change; wrapped in an [`Rc`] so the render closure can clone it freely.
fn model_to_row_meta(model: &WorkspaceModel) -> HashMap<SharedString, RowMetaKind> {
    let mut meta = HashMap::new();
    for (ri, row) in model.rows.iter().enumerate() {
        match row {
            WorkspaceRow::Section(_label) => {
                meta.insert(
                    SharedString::from(format!("section-{ri}")),
                    RowMetaKind::Section,
                );
            }
            WorkspaceRow::Type(t) => {
                meta.insert(
                    nav_item_id(t.doc, t.id),
                    RowMetaKind::Type {
                        badge: t.badge,
                        name: t.name.clone(),
                        field_count: t.field_count,
                        viewed: t.viewed,
                    },
                );
                for child in &t.children {
                    meta.insert(
                        SharedString::from(format!("field-{}-{}", t.id, child.id)),
                        RowMetaKind::Field {
                            offset: child.offset,
                            type_name: child.type_name.clone(),
                            field_name: child.field_name.clone(),
                        },
                    );
                }
            }
        }
    }
    meta
}

/// The accent tint for a type badge (the C++ letter-badge color by kind, here
/// mapped to the One Dark syntax roles so it retints with the theme).
fn badge_color(badge: TypeBadge, cx: &App) -> Hsla {
    match badge {
        // struct/class → type yellow; union → keyword magenta; enum → number/
        // accent blue; field → muted (unused for the badge, kept exhaustive).
        TypeBadge::Struct => color::syntax_type(cx),
        TypeBadge::Union => color::syntax_keyword(cx),
        TypeBadge::Enum => color::accent(cx),
        TypeBadge::Field => color::text_muted(cx),
    }
}

/// The workspace ("Project") dock panel — a filter input above a virtualized
/// [`Tree`](gpui_component::tree) of the open documents' types.
///
/// Owns the search [`InputState`], the [`TreeState`], and the last-built
/// [`WorkspaceModel`]. The window rebuilds the model (via [`set_model`]) when the
/// project changes; clicking a type row emits [`WorkspaceNav`] for the window to
/// act on (quick navigation).
pub struct WorkspacePanel {
    model: WorkspaceModel,
    search: Entity<InputState>,
    tree_state: Entity<TreeState>,
    /// Per-tree-item styling metadata (badge / count / field tints), rebuilt
    /// alongside the tree items so the render closure can paint Zed rows. Shared
    /// via [`Rc`] into the (re-created each frame) render closure.
    row_meta: Rc<HashMap<SharedString, RowMetaKind>>,
    focus_handle: FocusHandle,
}

impl WorkspacePanel {
    /// Build an empty workspace panel.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search types…"));
        let tree_state = cx.new(|cx| TreeState::new(cx));
        // Rebuild the tree whenever the filter changes.
        cx.subscribe(
            &search,
            |this, _input, _ev: &gpui_component::input::InputEvent, cx| {
                this.refresh_tree(cx);
            },
        )
        .detach();
        WorkspacePanel {
            model: WorkspaceModel::default(),
            search,
            tree_state,
            row_meta: Rc::new(HashMap::new()),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Construct as an [`Entity`].
    pub fn view(window: &mut Window, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| WorkspacePanel::new(window, cx))
    }

    /// Replace the workspace model (the window calls this on project change;
    /// `rebuildWorkspaceModel`) and refresh the tree.
    pub fn set_model(&mut self, model: WorkspaceModel, cx: &mut Context<Self>) {
        self.model = model;
        self.refresh_tree(cx);
    }

    /// The current model (for the dock title / tests).
    pub fn model(&self) -> &WorkspaceModel {
        &self.model
    }

    /// The current filter text.
    fn filter(&self, cx: &App) -> String {
        self.search.read(cx).value().to_string()
    }

    /// Rebuild the [`TreeState`] items + the per-row styling metadata from the
    /// (filtered) model.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let filtered = self.model.filtered(&self.filter(cx));
        let items = model_to_tree_items(&filtered);
        self.row_meta = Rc::new(model_to_row_meta(&filtered));
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
        });
        cx.notify();
    }
}

impl Panel for WorkspacePanel {
    fn panel_name(&self) -> &'static str {
        "WorkspacePanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.model.dock_title(false))
    }
}

impl EventEmitter<PanelEvent> for WorkspacePanel {}
impl EventEmitter<WorkspaceNav> for WorkspacePanel {}

impl Focusable for WorkspacePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspacePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let meta = self.row_meta.clone();

        gpui_component::v_flex()
            .id("rcx-workspace-panel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(color::panel_bg(cx))
            .text_color(color::text(cx))
            .child(self.render_header(cx))
            .child(self.render_search(cx))
            .child(
                div().flex_1().min_h_0().child(
                    tree(&self.tree_state, move |ix, entry, selected, _window, cx| {
                        let item = entry.item();
                        let kind = meta.get(&item.id);
                        let nav = parse_nav_item_id(&item.id);
                        let depth = entry.depth();
                        let is_folder = entry.is_folder();
                        let is_expanded = entry.is_expanded();

                        let row = render_row(
                            ix,
                            item.label.clone(),
                            kind,
                            depth,
                            is_folder,
                            is_expanded,
                            selected,
                            cx,
                        );

                        // Type rows route a quick-navigation request up to the
                        // window on click; section + field rows are inert (the
                        // C++ RoleSectionHeader bands / non-navigable children).
                        if let Some(nav) = nav {
                            let view = view.clone();
                            row.on_click(move |_e, _window, cx| {
                                view.update(cx, |_this, cx| cx.emit(nav));
                            })
                        } else {
                            row
                        }
                    })
                    .px(px(tokens::space::SM))
                    .py(px(tokens::space::XS)),
                ),
            )
    }
}

impl WorkspacePanel {
    /// The Zed-style panel header: a small uppercase muted "PROJECT" title with
    /// the struct/enum count, plus a close affordance on the right (the C++
    /// 36px `workspaceHeader` with its `×` button).
    fn render_header(&self, cx: &App) -> impl IntoElement {
        let m = &self.model;
        // The count caption ("N structs · M enums"), or nothing when empty.
        let mut count = String::new();
        if m.struct_count > 0 || m.enum_count > 0 {
            count.push_str(&format!(
                "{} struct{}",
                m.struct_count,
                if m.struct_count != 1 { "s" } else { "" }
            ));
            if m.enum_count > 0 {
                count.push_str(&format!(
                    " \u{b7} {} enum{}",
                    m.enum_count,
                    if m.enum_count != 1 { "s" } else { "" }
                ));
            }
        }

        gpui_component::h_flex()
            .h(px(32.0))
            .w_full()
            .flex_none()
            .px(px(tokens::space::LG))
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(color::border(cx))
            .child(
                gpui_component::h_flex()
                    .min_w_0()
                    .gap(px(tokens::space::MD))
                    .items_baseline()
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(tokens::font::UI_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color::text_muted(cx))
                            .child("PROJECT"),
                    )
                    .when(!count.is_empty(), |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(tokens::font::UI_XS))
                                .text_color(color::text_disabled(cx))
                                .child(count),
                        )
                    }),
            )
            // Close affordance — a ghost "×" glyph (the C++ header close button).
            // Closing the dock is the window's layout toggle; here it is a
            // restrained chrome hint that lightens on hover.
            .child(
                div()
                    .id("rcx-workspace-close")
                    .flex_none()
                    .size(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(tokens::radius::SM))
                    .text_size(px(tokens::font::UI_MD))
                    .text_color(color::text_muted(cx))
                    .hover(|s| s.bg(color::hover_overlay(cx)).text_color(color::text(cx)))
                    .child("\u{00d7}"),
            )
    }

    /// The Zed-style filter input row: a subtle-bg input with a leading
    /// magnifier glyph (the C++ `m_workspaceSearch` with its `filter.svg`
    /// leading icon). The `Input` carries the focus ring + clear button.
    fn render_search(&self, cx: &App) -> impl IntoElement {
        gpui_component::h_flex()
            .w_full()
            .flex_none()
            .px(px(tokens::space::MD))
            .py(px(tokens::space::MD))
            .gap(px(tokens::space::MD))
            .items_center()
            // Leading magnifier glyph — asset-free (no SVG bundle), tinted muted.
            .child(
                div()
                    .flex_none()
                    .text_size(px(tokens::font::UI_SM))
                    .text_color(color::text_muted(cx))
                    .child("\u{1f50d}"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).w_full()),
            )
    }
}

/// Render one workspace tree row in Zed styling. Dispatches on the row's
/// [`RowMetaKind`]: a band header, a top-level type (badge + name + count
/// pill), or a struct field child (syntax-tinted "Type field"). Falls back to
/// the flat label when no metadata is found (should not happen in practice).
#[allow(clippy::too_many_arguments)]
fn render_row(
    ix: usize,
    label: SharedString,
    kind: Option<&RowMetaKind>,
    depth: usize,
    is_folder: bool,
    is_expanded: bool,
    selected: bool,
    cx: &App,
) -> ListItem {
    // Left indent: a per-depth step that leaves room for indent guides.
    let indent = px(tokens::space::LG) * depth as f32;

    match kind {
        // ── Section band (PINNED / ALL TYPES) ──────────────────────────────
        Some(RowMetaKind::Section) => ListItem::new(ix)
            .w_full()
            .child(section_row(label, cx))
            .map(strip_row_padding),

        // ── Top-level type: badge + name + trailing count pill ──────────────
        Some(RowMetaKind::Type {
            badge,
            name,
            field_count,
            viewed,
        }) => {
            let row = gpui_component::h_flex()
                .w_full()
                .h(px(24.0))
                .pl(indent)
                .pr(px(tokens::space::XS))
                .gap(px(tokens::space::SM))
                .items_center()
                // Disclosure chevron (only when the type has field children).
                .child(disclosure(is_folder, is_expanded, cx))
                // Type badge (colored chip with the S / E / U letter).
                .child(type_badge(*badge, *viewed, cx))
                // Name — truncates; brighter when selected, muted otherwise so
                // the active row reads as content-forward (Zed list behavior).
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(tokens::font::UI_MD))
                        .text_color(if selected {
                            color::text(cx)
                        } else {
                            color::text_muted(cx)
                        })
                        .child(SharedString::from(name.clone())),
                )
                // Trailing member count — a muted pill (the C++ count pill).
                .child(count_pill(*field_count, cx));

            ListItem::new(ix).w_full().child(row).map(strip_row_padding)
        }

        // ── Struct field child: "+0xNN  Type field" with syntax tints ───────
        // The leading offset chip is the editor's address column carried onto
        // the collapsed nested rows (QA fix): a fixed-width, right-aligned,
        // faint `+0xNN` so a long struct's members read as a real layout, not a
        // flat list of identical gray rows.
        Some(RowMetaKind::Field {
            offset,
            type_name,
            field_name,
        }) => {
            let row = gpui_component::h_flex()
                .w_full()
                .h(px(22.0))
                .pl(indent + px(tokens::space::LG))
                .pr(px(tokens::space::XS))
                .gap(px(tokens::space::SM))
                .items_center()
                .text_size(px(tokens::font::UI_SM))
                .min_w_0()
                // Offset chip — faint, right-aligned in a fixed gutter so the
                // type names line up regardless of offset magnitude (the
                // editor's `offset_hex_digits`-aligned address column).
                .child(field_offset_chip(*offset, cx))
                .child(
                    div()
                        .flex_none()
                        .text_color(color::syntax_type(cx))
                        .child(SharedString::from(type_name.clone())),
                )
                .when(!field_name.is_empty(), |this| {
                    this.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(color::text_muted(cx))
                            .child(SharedString::from(field_name.clone())),
                    )
                });

            ListItem::new(ix).w_full().child(row).map(strip_row_padding)
        }

        // ── Fallback (no metadata) — the flat label. ────────────────────────
        None => ListItem::new(ix)
            .w_full()
            .child(div().pl(indent).child(label))
            .map(strip_row_padding),
    }
}

/// Strip the [`ListItem`]'s default `py_1 px_3` so our row geometry (heights +
/// indent) is exact; keep its hover / selected fill + radius.
fn strip_row_padding(li: ListItem) -> ListItem {
    li.p_0().rounded(px(tokens::radius::MD))
}

/// A PINNED / ALL TYPES band header: a small uppercase muted caption, padded so
/// it reads as a divider above the rows it groups (the C++ `RoleSectionHeader`).
fn section_row(label: SharedString, cx: &App) -> impl IntoElement {
    div()
        .w_full()
        .pt(px(tokens::space::MD))
        .pb(px(tokens::space::XS))
        .px(px(tokens::space::SM))
        .text_size(px(tokens::font::UI_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color::text_muted(cx))
        .child(SharedString::from(label.to_uppercase()))
}

/// The disclosure chevron for a row: a ▸ / ▾ glyph for folders (types with
/// field children), or a same-width spacer for leaves so names align. Asset-free
/// (no SVG bundle) — a Unicode triangle tinted muted.
fn disclosure(is_folder: bool, is_expanded: bool, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(12.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(tokens::font::UI_XS))
        .text_color(color::text_muted(cx))
        .when(is_folder, |this| {
            this.child(if is_expanded { "\u{25be}" } else { "\u{25b8}" })
        })
}

/// The colored type badge — a rounded chip carrying the S / E / U letter, tinted
/// by kind (the C++ letter-badge `WorkspaceDelegate::paint`). Un-viewed types
/// render dimmer (the C++ `Qt::UserRole + 3` dim state).
fn type_badge(badge: TypeBadge, viewed: bool, cx: &App) -> impl IntoElement {
    let mut tint = badge_color(badge, cx);
    if !viewed {
        tint.a = 0.55;
    }
    let mut bg = tint;
    bg.a = 0.16;

    div()
        .flex_none()
        .size(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(tokens::radius::SM))
        .bg(bg)
        .text_size(px(tokens::font::UI_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tint)
        .child(badge.letter().to_string())
}

/// The leading offset chip on a struct field child row — a faint, fixed-width,
/// right-aligned `+0xNN` in the editor's address tint (`syntax_address`). The
/// fixed width keeps every nested member's type name aligned in a column (the
/// editor gutter behaviour) so a deep struct reads as a layout. Monospace so the
/// hex digits are uniform-width.
fn field_offset_chip(offset: i32, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(44.0))
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .text_size(px(tokens::font::UI_XS))
        .font_family(tokens::font::MONO_FAMILY)
        .text_color(color::syntax_address(cx))
        .child(SharedString::from(format_field_offset(offset)))
}

/// The trailing member-count pill — a muted, low-contrast right-aligned chip
/// (the C++ count pill on `surface` bg). Hidden for empty types.
fn count_pill(count: usize, cx: &App) -> impl IntoElement {
    div()
        .flex_none()
        .min_w(px(18.0))
        .px(px(tokens::space::SM))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(tokens::font::UI_XS))
        .text_color(color::text_muted(cx))
        .child(count.to_string())
}

#[cfg(test)]
mod tests {
    // Import only the gpui-free model items under test — NOT `super::*`, which
    // would pull the module's `gpui::*` glob into the `#[test]` hygiene
    // expansion and overflow the type-recursion budget (see lib.rs note).
    use super::DocId;
    use super::{
        format_field_offset, model_to_tree_items, nav_item_id, parse_nav_item_id, TypeBadge,
        WorkspaceDoc, WorkspaceModel, WorkspaceRow,
    };
    use crate::core::{Node, NodeKind, NodeTree};

    /// Build a tree with three top-level types of varying field counts:
    /// - `Player` struct: 3 visible fields (+ a hex pad that must be excluded)
    /// - `Small` struct: 1 visible field
    /// - `Color` enum: 4 members
    /// Returns (tree, ids) for assertions.
    fn sample_tree() -> NodeTree {
        let mut tree = NodeTree::default();

        // Player struct.
        let p = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Player".into(),
            struct_type_name: "Player".into(),
            class_keyword: "class".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let pid = tree.nodes[p].id;
        tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "health".into(),
            parent_id: pid,
            offset: 0,
            ..Node::default()
        });
        tree.add_node(Node {
            kind: NodeKind::Float,
            name: "stamina".into(),
            parent_id: pid,
            offset: 8,
            ..Node::default()
        });
        // A hex-pad member — must NOT count toward field_count or appear as a child.
        tree.add_node(Node {
            kind: NodeKind::Hex32,
            name: String::new(),
            parent_id: pid,
            offset: 4,
            ..Node::default()
        });
        tree.add_node(Node {
            kind: NodeKind::Int64,
            name: "xp".into(),
            parent_id: pid,
            offset: 12,
            ..Node::default()
        });

        // Small struct.
        let s = tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Small".into(),
            struct_type_name: "Small".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let sid = tree.nodes[s].id;
        tree.add_node(Node {
            kind: NodeKind::Int8,
            name: "flag".into(),
            parent_id: sid,
            offset: 0,
            ..Node::default()
        });

        // Color enum.
        tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Color".into(),
            struct_type_name: "Color".into(),
            class_keyword: "enum".into(),
            parent_id: 0,
            offset: 0,
            enum_members: vec![
                ("Red".into(), 0),
                ("Green".into(), 1),
                ("Blue".into(), 2),
                ("Alpha".into(), 3),
            ],
            ..Node::default()
        });

        tree
    }

    fn doc_id(n: u64) -> DocId {
        // Allocate ids through a throwaway AppState so DocId stays opaque.
        let mut s = super::super::state::AppState::new();
        let mut last = s.open_document("x");
        for _ in 1..n {
            last = s.open_document("x");
        }
        last
    }

    #[test]
    fn builds_sections_and_sorts_by_field_count_desc() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);

        // First row is the ALL TYPES section (no pins).
        assert_eq!(m.rows[0], WorkspaceRow::Section("ALL TYPES".to_string()));

        // Type rows sorted by field count desc: Color(4) > Player(3) > Small(1).
        let names: Vec<&str> = m.type_entries().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Color", "Player", "Small"]);

        let counts: Vec<usize> = m.type_entries().map(|t| t.field_count).collect();
        assert_eq!(counts, vec![4, 3, 1]);
    }

    #[test]
    fn hex_pad_members_excluded_from_count_and_children() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);

        let player = m.type_entries().find(|t| t.name == "Player").unwrap();
        // 3 visible fields (health/stamina/xp), the Hex32 pad excluded.
        assert_eq!(player.field_count, 3);
        assert_eq!(player.children.len(), 3);
        // Children are sorted by offset: health(0), stamina(8), xp(12) — the pad
        // at offset 4 is gone.
        let fields: Vec<&str> = player
            .children
            .iter()
            .map(|c| c.field_name.as_str())
            .collect();
        assert_eq!(fields, vec!["health", "stamina", "xp"]);
        // Field child display = "Type name".
        assert_eq!(player.children[0].display(), "Int32 health");
        // Each child carries its member offset (for the nested-row offset chip),
        // in offset-sorted order: health@0, stamina@8, xp@12.
        let offsets: Vec<i32> = player.children.iter().map(|c| c.offset).collect();
        assert_eq!(offsets, vec![0, 8, 12]);
    }

    #[test]
    fn format_field_offset_is_compact_hex() {
        // Editor-style `+0xNN`, uppercase, no leading zeros.
        assert_eq!(format_field_offset(0), "+0x0");
        assert_eq!(format_field_offset(8), "+0x8");
        assert_eq!(format_field_offset(16), "+0x10");
        assert_eq!(format_field_offset(0x1A0), "+0x1A0");
        // Negative offsets (should not occur, but render safely with a sign).
        assert_eq!(format_field_offset(-8), "-0x8");
    }

    #[test]
    fn badges_distinguish_struct_enum_union() {
        let mut tree = sample_tree();
        // Add a union.
        tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Variant".into(),
            struct_type_name: "Variant".into(),
            class_keyword: "union".into(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        });
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);

        let badge_of = |name: &str| m.type_entries().find(|t| t.name == name).unwrap().badge;
        assert_eq!(badge_of("Player"), TypeBadge::Struct);
        assert_eq!(badge_of("Color"), TypeBadge::Enum);
        assert_eq!(badge_of("Variant"), TypeBadge::Union);
        assert_eq!(TypeBadge::Struct.letter(), 'S');
        assert_eq!(TypeBadge::Enum.letter(), 'E');
        assert_eq!(TypeBadge::Union.letter(), 'U');

        // The dock counts unions as structs (non-enum).
        assert_eq!(m.enum_count, 1);
        assert_eq!(m.struct_count, 3); // Player, Small, Variant
    }

    #[test]
    fn pinned_section_appears_first_when_pinned() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        // Pin Player.
        let player_id = tree
            .children_of(0)
            .iter()
            .map(|&i| &tree.nodes[i])
            .find(|n| n.struct_type_name == "Player")
            .unwrap()
            .id;
        let m = WorkspaceModel::build(&docs, &[player_id], &[]);

        assert_eq!(m.rows[0], WorkspaceRow::Section("PINNED".to_string()));
        // The pinned row is Player.
        match &m.rows[1] {
            WorkspaceRow::Type(t) => assert_eq!(t.name, "Player"),
            _ => panic!("expected pinned Player row"),
        }
        // The ALL TYPES section header is still present further down.
        assert!(m
            .rows
            .iter()
            .any(|r| matches!(r, WorkspaceRow::Section(s) if s == "ALL TYPES")));
    }

    #[test]
    fn dock_title_pluralizes_and_marks_dirty() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);
        // 2 structs (Player, Small) + 1 enum (Color).
        assert_eq!(
            m.dock_title(false),
            "Project \u{2014} 2 structs \u{b7} 1 enum"
        );
        // Dirty marker prefix.
        assert!(m.dock_title(true).starts_with("\u{2022} "));

        // Empty project → bare "Project".
        let empty = WorkspaceModel::default();
        assert_eq!(empty.dock_title(false), "Project");
    }

    #[test]
    fn filter_matches_name_or_field_and_drops_sections() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);

        // Filter by type name.
        let by_name = m.filtered("play");
        let names: Vec<&str> = by_name.type_entries().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Player"]);
        // No section headers survive a filter.
        assert!(!by_name
            .rows
            .iter()
            .any(|r| matches!(r, WorkspaceRow::Section(_))));

        // Filter by a field name (only Player has "stamina").
        let by_field = m.filtered("stamina");
        let names: Vec<&str> = by_field.type_entries().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Player"]);

        // Empty filter returns the full model (sections intact).
        let all = m.filtered("   ");
        assert_eq!(all, m);
    }

    #[test]
    fn nav_item_id_roundtrips() {
        let d = doc_id(2);
        let id = nav_item_id(d, 12345);
        let nav = parse_nav_item_id(&id).expect("type row id parses");
        assert_eq!(nav.doc, d);
        assert_eq!(nav.node_id, 12345);

        // Section / field ids do not parse to a nav target.
        assert!(parse_nav_item_id("section-0").is_none());
        assert!(parse_nav_item_id("field-1-2").is_none());
    }

    #[test]
    fn tree_items_mirror_model_rows_and_children() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let m = WorkspaceModel::build(&docs, &[], &[]);
        let items = model_to_tree_items(&m);

        // First item is the ALL TYPES section (disabled / non-navigable).
        assert_eq!(items[0].label.as_ref(), "ALL TYPES");
        assert!(items[0].is_disabled());

        // The Player type item carries its nav id + its 3 field children.
        let player = items
            .iter()
            .find(|i| i.label.starts_with("Player"))
            .expect("Player row present");
        assert!(parse_nav_item_id(&player.id).is_some());
        assert_eq!(player.children.len(), 3);
        // The label includes the count (the "Name — N" display).
        assert_eq!(player.label.as_ref(), "Player \u{2014} 3");
    }

    #[test]
    fn viewed_flag_set_from_viewed_ids() {
        let tree = sample_tree();
        let d = doc_id(1);
        let docs = vec![WorkspaceDoc {
            doc: d,
            tree: &tree,
        }];
        let player_id = tree
            .children_of(0)
            .iter()
            .map(|&i| &tree.nodes[i])
            .find(|n| n.struct_type_name == "Player")
            .unwrap()
            .id;
        let m = WorkspaceModel::build(&docs, &[], &[player_id]);
        let player = m.type_entries().find(|t| t.name == "Player").unwrap();
        let small = m.type_entries().find(|t| t.name == "Small").unwrap();
        assert!(player.viewed);
        assert!(!small.viewed);
    }
}
