//! The model-access boundary the MCP bridge talks to.
//!
//! The C++ `McpBridge` lives on the Qt GUI thread and reaches into
//! `MainWindow`/`TabState`/`RcxDocument`/`Controller` synchronously
//! (`mcp.md §4.3`). In Rust the bridge runs on its own dispatch thread, so the
//! app model is reached through the [`McpHost`] trait, called while the dispatch
//! thread holds the (implicit) global request lock. This preserves the
//! "one request in flight, globally" invariant.
//!
//! [`TabState`] is the Rust counterpart of `MainWindow::TabState`: it owns the
//! `NodeTree`, the active `Provider`, an undo stack, value-history map, saved
//! sources, selection, view-root, file path and modified flag — i.e. the
//! `doc`+`ctrl` surface the in-scope tools consume. The tool-relevant
//! document/undo semantics live here (rather than reaching into the
//! `controller` subsystem), ported 1:1 from the command shapes in
//! `core::command`, keeping the MCP host self-contained at the model boundary.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::core::command::OffsetAdj;
use crate::core::{Command, NodeKind, NodeTree, ValueHistory};
use crate::provider::{BufferProvider, NullProvider, Provider};

/// A saved data source (`controller.h:99` `SavedSourceEntry`), trimmed to the
/// fields `project.state`/`source.switch` read.
#[derive(Clone, Debug, Default)]
pub struct SavedSource {
    pub kind: String,
    pub display_name: String,
    pub provider_target: String,
}

/// A single undo entry: a flat list of commands applied as one macro
/// (`QUndoStack::beginMacro`/`endMacro`). Undo reverts them in reverse order.
#[derive(Clone, Debug)]
struct UndoEntry {
    commands: Vec<Command>,
}

/// Minimal undo stack mirroring `QUndoStack`'s observable surface used by MCP
/// (`canUndo`/`canRedo`/`push`/`beginMacro`/`endMacro`/`undo`/`redo`).
#[derive(Default)]
pub struct UndoStack {
    entries: Vec<UndoEntry>,
    /// index of the next redo target (== count of currently-applied entries).
    cursor: usize,
    macro_depth: i32,
    macro_buf: Vec<Command>,
}

impl UndoStack {
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }
    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    fn begin_macro(&mut self) {
        if self.macro_depth == 0 {
            self.macro_buf.clear();
        }
        self.macro_depth += 1;
    }

    fn end_macro(&mut self, tab: &mut TabData) {
        self.macro_depth -= 1;
        if self.macro_depth == 0 {
            let cmds = std::mem::take(&mut self.macro_buf);
            // Drop any redo branch, then commit the macro (already applied live).
            self.entries.truncate(self.cursor);
            self.entries.push(UndoEntry { commands: cmds });
            self.cursor = self.entries.len();
            let _ = tab;
        }
    }

    /// `QUndoStack::push` — applies the command immediately and records it.
    fn push(&mut self, tab: &mut TabData, cmd: Command) {
        apply_command(tab, &cmd, false);
        if self.macro_depth > 0 {
            self.macro_buf.push(cmd);
        } else {
            self.entries.truncate(self.cursor);
            self.entries.push(UndoEntry {
                commands: vec![cmd],
            });
            self.cursor = self.entries.len();
        }
    }

    fn undo(&mut self, tab: &mut TabData) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let cmds = self.entries[self.cursor].commands.clone();
        for cmd in cmds.iter().rev() {
            apply_command(tab, cmd, true);
        }
    }

    fn redo(&mut self, tab: &mut TabData) {
        if self.cursor >= self.entries.len() {
            return;
        }
        let cmds = self.entries[self.cursor].commands.clone();
        for cmd in &cmds {
            apply_command(tab, cmd, false);
        }
        self.cursor += 1;
    }
}

/// The document + controller surface for one tab. Split into [`TabData`] (the
/// borrowable model) plus the undo stack so undo/redo can mutate the model
/// while iterating recorded commands.
pub struct TabState {
    pub data: TabData,
    pub undo: UndoStack,
}

/// Borrowable model fields (everything except the undo stack).
pub struct TabData {
    pub tree: NodeTree,
    pub provider: Arc<dyn Provider + Send + Sync>,
    pub value_history: HashMap<u64, ValueHistory>,
    pub sources: Vec<SavedSource>,
    pub active_source: i32,
    pub selected_ids: HashSet<u64>,
    pub view_root_id: u64,
    pub file_path: String,
    pub modified: bool,
    pub type_aliases: HashMap<NodeKind, String>,
}

impl Default for TabData {
    fn default() -> Self {
        TabData {
            tree: NodeTree::new(),
            provider: Arc::new(NullProvider),
            value_history: HashMap::new(),
            sources: Vec::new(),
            active_source: -1,
            selected_ids: HashSet::new(),
            view_root_id: 0,
            file_path: String::new(),
            modified: false,
            type_aliases: HashMap::new(),
        }
    }
}

impl Default for TabState {
    fn default() -> Self {
        TabState {
            data: TabData::default(),
            undo: UndoStack::default(),
        }
    }
}

impl TabState {
    pub fn new() -> Self {
        Self::default()
    }

    // ── undo-stack façade (so tools call `tab.begin_macro()` etc.) ──
    pub fn begin_macro(&mut self) {
        self.undo.begin_macro();
    }
    pub fn end_macro(&mut self) {
        let TabState { data, undo } = self;
        undo.end_macro(data);
    }
    pub fn push_command(&mut self, cmd: Command) {
        let TabState { data, undo } = self;
        undo.push(data, cmd);
    }
    pub fn undo(&mut self) {
        let TabState { data, undo } = self;
        undo.undo(data);
    }
    pub fn redo(&mut self) {
        let TabState { data, undo } = self;
        undo.redo(data);
    }
    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// `RcxDocument::loadData(path)` — load a binary file as a buffer provider.
    pub fn load_data(&mut self, path: &str) {
        self.data.provider = Arc::new(BufferProvider::from_file(path));
        self.data.sources.push(SavedSource {
            kind: "File".to_string(),
            display_name: path.to_string(),
            provider_target: String::new(),
        });
        self.data.active_source = (self.data.sources.len() - 1) as i32;
    }

    /// Attach a provider-backed source inside the MCP tab model. This mirrors the
    /// controller's source-switch subset without depending on the UI controller.
    pub fn attach_provider(
        &mut self,
        provider: Arc<dyn Provider + Send + Sync>,
        provider_target: String,
    ) {
        let identifier = provider.kind();
        self.attach_provider_with_identifier(provider, identifier, provider_target);
    }

    pub fn attach_provider_with_identifier(
        &mut self,
        provider: Arc<dyn Provider + Send + Sync>,
        provider_identifier: impl Into<String>,
        provider_target: String,
    ) {
        let base = provider.base();
        let pointer_size = provider.pointer_size();
        let display_name = provider.name();
        let kind = provider_identifier.into();
        self.data.provider = provider;
        self.data.tree.pointer_size = pointer_size;
        if (self.data.tree.base_address == 0 || self.data.tree.base_address == 0x0040_0000)
            && base != 0
        {
            self.data.tree.base_address = base;
        }
        let entry = SavedSource {
            kind: kind.clone(),
            display_name,
            provider_target: provider_target.clone(),
        };
        if let Some(pos) = self
            .data
            .sources
            .iter()
            .position(|s| s.kind == kind && s.provider_target == provider_target)
        {
            self.data.sources[pos] = entry;
            self.data.active_source = pos as i32;
        } else {
            self.data.sources.push(entry);
            self.data.active_source = (self.data.sources.len() - 1) as i32;
        }
    }

    /// `RcxController::switchSource(idx)` — minimal: set the active index.
    pub fn switch_source(&mut self, idx: i32) {
        self.data.active_source = idx;
    }
}

/// Apply (or revert) one command against a tab's model. Ported from the
/// per-command semantics in `core::command` + `controller::applyCommand`
/// (`controller.cpp:2827`). `is_undo` runs the command backwards.
fn apply_command(tab: &mut TabData, cmd: &Command, is_undo: bool) {
    let tree = &mut tab.tree;
    match cmd {
        Command::Insert { node, .. } => {
            if is_undo {
                remove_subtree(tree, node.id);
            } else if tree.index_of_id(node.id) < 0 {
                tree.add_node(node.clone());
                tree.invalidate_id_cache();
            }
        }
        Command::Remove {
            node_id, subtree, ..
        } => {
            if is_undo {
                for n in subtree {
                    if tree.index_of_id(n.id) < 0 {
                        tree.add_node(n.clone());
                    }
                }
                tree.invalidate_id_cache();
            } else {
                remove_subtree(tree, *node_id);
            }
        }
        Command::Rename {
            node_id,
            old_name,
            new_name,
        } => set_field(tree, *node_id, |n| {
            n.name = if is_undo {
                old_name.clone()
            } else {
                new_name.clone()
            }
        }),
        Command::ChangeKind {
            node_id,
            old_kind,
            new_kind,
            ..
        } => set_field(tree, *node_id, |n| {
            n.kind = if is_undo { *old_kind } else { *new_kind }
        }),
        Command::ChangeOffset {
            node_id,
            old_offset,
            new_offset,
        } => set_field(tree, *node_id, |n| {
            n.offset = if is_undo { *old_offset } else { *new_offset }
        }),
        Command::ChangeBase {
            old_base,
            new_base,
            old_formula,
            new_formula,
        } => {
            if is_undo {
                tree.base_address = *old_base;
                tree.base_address_formula = old_formula.clone();
            } else {
                tree.base_address = *new_base;
                tree.base_address_formula = new_formula.clone();
            }
        }
        Command::ChangeStructTypeName {
            node_id,
            old_name,
            new_name,
        } => set_field(tree, *node_id, |n| {
            n.struct_type_name = if is_undo {
                old_name.clone()
            } else {
                new_name.clone()
            }
        }),
        Command::ChangeClassKeyword {
            node_id,
            old_keyword,
            new_keyword,
        } => set_field(tree, *node_id, |n| {
            n.class_keyword = if is_undo {
                old_keyword.clone()
            } else {
                new_keyword.clone()
            }
        }),
        Command::ChangePointerRef {
            node_id,
            old_ref_id,
            new_ref_id,
        } => set_field(tree, *node_id, |n| {
            n.ref_id = if is_undo { *old_ref_id } else { *new_ref_id }
        }),
        Command::ChangeArrayMeta {
            node_id,
            old_element_kind,
            new_element_kind,
            old_array_len,
            new_array_len,
        } => set_field(tree, *node_id, |n| {
            if is_undo {
                n.element_kind = *old_element_kind;
                n.array_len = *old_array_len;
            } else {
                n.element_kind = *new_element_kind;
                n.array_len = *new_array_len;
            }
        }),
        Command::Collapse {
            node_id,
            old_state,
            new_state,
        } => set_field(tree, *node_id, |n| {
            n.collapsed = if is_undo { *old_state } else { *new_state }
        }),
        Command::ChangeEnumMembers {
            node_id,
            old_members,
            new_members,
        } => set_field(tree, *node_id, |n| {
            n.enum_members = if is_undo {
                old_members.clone()
            } else {
                new_members.clone()
            }
        }),
        // ToggleRelative is declared but UNHANDLED in the C++ applyCommand
        // (latent no-op) — preserve that.
        Command::ToggleRelative { .. } => {}
        Command::ToggleBigEndian {
            node_id,
            old_val,
            new_val,
        } => set_field(tree, *node_id, |n| {
            n.big_endian = if is_undo { *old_val } else { *new_val }
        }),
        Command::ChangeComment {
            node_id,
            old_comment,
            new_comment,
        } => set_field(tree, *node_id, |n| {
            n.comment = if is_undo {
                old_comment.clone()
            } else {
                new_comment.clone()
            }
        }),
        Command::WriteBytes {
            addr,
            old_bytes,
            new_bytes,
        } => {
            let bytes = if is_undo { old_bytes } else { new_bytes };
            // `Provider::write` takes `&self` (interior mutability), so the write
            // goes straight through the shared `Arc` even if it is cloned
            // elsewhere; read-only providers reject it as a no-op.
            tab.provider.write(*addr, bytes);
        }
    }
}

fn set_field<F: FnOnce(&mut crate::core::Node)>(tree: &mut NodeTree, id: u64, f: F) {
    let idx = tree.index_of_id(id);
    if idx >= 0 {
        f(&mut tree.nodes[idx as usize]);
        tree.bump_generation();
    }
}

fn remove_subtree(tree: &mut NodeTree, id: u64) {
    let indices = tree.subtree_indices(id);
    let ids: HashSet<u64> = indices.iter().map(|&i| tree.nodes[i].id).collect();
    tree.nodes.retain(|n| !ids.contains(&n.id));
    tree.invalidate_id_cache();
    tree.bump_generation();
}

/// What the MCP bridge needs from the host app. Implemented by the app shell;
/// the bridge never touches gpui/MainWindow directly. Mirrors the
/// `MainWindow`+`TabState` surface `mcp_bridge.cpp` consumes (`mcp.md §4.3`).
pub trait McpHost: Send {
    fn tab_count(&self) -> usize;
    /// `MainWindow::activeTab()` index, or `None`.
    fn active_tab_index(&self) -> Option<usize>;
    /// `MainWindow::project_new()` — auto-create on demand (`resolveTab` step 4).
    fn project_new(&mut self) -> usize;
    fn project_open(&mut self, path: &str);
    fn project_save(&mut self);
    /// Borrow a tab for one operation.
    fn with_tab(&mut self, idx: usize, f: &mut dyn FnMut(&mut TabState)) -> bool;
    /// Read-only borrow for a tab.
    fn with_tab_ref(&self, idx: usize, f: &mut dyn FnMut(&TabState)) -> bool;
    /// `m_appStatus` getter/setter (`status.set`, `project.state.statusText`).
    fn app_status(&self) -> String;
    fn set_app_status(&mut self, text: &str);
    /// UI-only; default no-op in headless tests (`status.set` command row).
    fn set_command_row_text(&mut self, _tab: usize, _text: &str) {}
    /// `ui.action reset_tracking` across all tabs — returns tab count.
    fn reset_change_tracking_all(&mut self) -> usize;
}

/// In-memory [`McpHost`] over a `Vec<TabState>` for logic/transport tests — no
/// GUI required (`PORTING_mcp.md §1`).
pub struct TestHost {
    pub tabs: Vec<TabState>,
    pub active: Option<usize>,
    pub status: String,
}

impl Default for TestHost {
    fn default() -> Self {
        TestHost {
            tabs: Vec::new(),
            active: None,
            status: String::new(),
        }
    }
}

impl TestHost {
    pub fn new() -> Self {
        Self::default()
    }
    /// Construct a host with a single pre-built tab.
    pub fn with_tab(tab: TabState) -> Self {
        TestHost {
            tabs: vec![tab],
            active: Some(0),
            status: String::new(),
        }
    }
}

impl McpHost for TestHost {
    fn tab_count(&self) -> usize {
        self.tabs.len()
    }
    fn active_tab_index(&self) -> Option<usize> {
        self.active.filter(|&i| i < self.tabs.len())
    }
    fn project_new(&mut self) -> usize {
        self.tabs.push(TabState::new());
        let idx = self.tabs.len() - 1;
        self.active = Some(idx);
        idx
    }
    fn project_open(&mut self, path: &str) {
        let idx = self.project_new();
        self.tabs[idx].data.file_path = path.to_string();
    }
    fn project_save(&mut self) {
        if let Some(i) = self.active_tab_index() {
            self.tabs[i].data.modified = false;
        }
    }
    fn with_tab(&mut self, idx: usize, f: &mut dyn FnMut(&mut TabState)) -> bool {
        match self.tabs.get_mut(idx) {
            Some(t) => {
                f(t);
                true
            }
            None => false,
        }
    }
    fn with_tab_ref(&self, idx: usize, f: &mut dyn FnMut(&TabState)) -> bool {
        match self.tabs.get(idx) {
            Some(t) => {
                f(t);
                true
            }
            None => false,
        }
    }
    fn app_status(&self) -> String {
        self.status.clone()
    }
    fn set_app_status(&mut self, text: &str) {
        self.status = text.to_string();
    }
    fn reset_change_tracking_all(&mut self) -> usize {
        for t in &mut self.tabs {
            t.data.value_history.clear();
        }
        self.tabs.len()
    }
}

// Silence unused import warning for OffsetAdj (kept for command parity docs).
const _: fn() = || {
    let _ = std::mem::size_of::<OffsetAdj>();
};
