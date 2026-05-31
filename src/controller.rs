//! Editor controller — the mediator between the editor view(s), the in-memory
//! document (`NodeTree` + attached `Provider`), and the undo/redo command
//! pipeline; plus the async refresh loop, value-history/heatmap, selection
//! state, and source management.
//!
//! Port of `src/controller.{h,cpp}` (≈7k lines). **SKELETON** — the refresh
//! pipeline, `apply_command`, selection logic, and source management are filled
//! in by the dedicated `controller` workflow (ARCHITECTURE.md §9). The document
//! model + the key controller signatures are in place.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::{Command, NodeKind, NodeTree, ValueHistory};
use crate::provider::{NullProvider, Provider};

/// `struct SavedSourceEntry` (`controller.h:99`).
#[derive(Clone, Debug, Default)]
pub struct SavedSourceEntry {
    /// "File" or a provider identifier (e.g. "processmemory").
    pub kind: String,
    pub display_name: String,
    pub file_path: String,
    pub provider_target: String,
    pub base_address: u64,
    pub base_address_formula: String,
}

/// `class RcxDocument` (`controller.h:27`) — owns the data model.
pub struct RcxDocument {
    pub tree: NodeTree,
    /// active data source (defaults to [`NullProvider`]).
    pub provider: Arc<dyn Provider + Send + Sync>,
    /// the `.rcx` path.
    pub file_path: String,
    /// attached binary path (cleared when a non-file provider attaches).
    pub data_path: String,
    pub modified: bool,
    /// per-kind display-name overrides (saved/loaded as `typeAliases`).
    pub type_aliases: HashMap<NodeKind, String>,
    /// number of sibling overlaps detected on last `load()`.
    pub load_overlap_count: i32,
}

impl Default for RcxDocument {
    fn default() -> Self {
        RcxDocument {
            tree: NodeTree::new(),
            provider: Arc::new(NullProvider),
            file_path: String::new(),
            data_path: String::new(),
            modified: false,
            type_aliases: HashMap::new(),
            load_overlap_count: 0,
        }
    }
}

impl RcxDocument {
    pub fn new() -> Self {
        RcxDocument::default()
    }

    /// `RcxDocument::resolveTypeName(kind)` (`controller.h`, inline) — alias →
    /// kind type-name → "???".
    pub fn resolve_type_name(&self, kind: NodeKind) -> String {
        if let Some(alias) = self.type_aliases.get(&kind) {
            if !alias.is_empty() {
                return alias.clone();
            }
        }
        crate::core::kind_meta(kind).map_or_else(|| "???".to_string(), |m| m.type_name.to_string())
    }

    /// `RcxDocument::save(path)` (`controller.cpp:179`). SKELETON.
    pub fn save(&mut self, _path: &str) -> bool {
        todo!("port RcxDocument::save (workflow: controller)")
    }

    /// `RcxDocument::load(path)` (`controller.cpp:201`). SKELETON.
    pub fn load(&mut self, _path: &str) -> bool {
        todo!("port RcxDocument::load (workflow: controller)")
    }
}

/// `class RcxController` (`controller.h`) — the mediator. SKELETON: holds the
/// document; the refresh/command/selection machinery is ported by the
/// `controller` workflow.
pub struct RcxController {
    doc: RcxDocument,
    value_history: HashMap<u64, ValueHistory>,
    sel_ids: std::collections::HashSet<u64>,
    view_root_id: u64,
    saved_sources: Vec<SavedSourceEntry>,
    active_source_idx: i32,
}

impl RcxController {
    /// `RcxController(doc)` (`controller.cpp:357`).
    pub fn new(doc: RcxDocument) -> Self {
        RcxController {
            doc,
            value_history: HashMap::new(),
            sel_ids: std::collections::HashSet::new(),
            view_root_id: 0,
            saved_sources: Vec::new(),
            active_source_idx: -1,
        }
    }

    pub fn document(&self) -> &RcxDocument {
        &self.doc
    }
    pub fn document_mut(&mut self) -> &mut RcxDocument {
        &mut self.doc
    }
    pub fn value_history(&self) -> &HashMap<u64, ValueHistory> {
        &self.value_history
    }
    pub fn selected_ids(&self) -> &std::collections::HashSet<u64> {
        &self.sel_ids
    }
    pub fn view_root_id(&self) -> u64 {
        self.view_root_id
    }
    pub fn saved_sources(&self) -> &[SavedSourceEntry] {
        &self.saved_sources
    }
    pub fn active_source_index(&self) -> i32 {
        self.active_source_idx
    }

    /// `RcxController::refresh()` (`controller.cpp:1891`) — recompose + push to
    /// editors. SKELETON.
    pub fn refresh(&mut self) {
        todo!("port RcxController::refresh (workflow: controller)")
    }

    /// `RcxController::applyCommand(cmd, isUndo)` (`controller.cpp:2827`) —
    /// returns false only when the underlying op was rejected. SKELETON.
    pub fn apply_command(&mut self, _cmd: &Command, _is_undo: bool) -> bool {
        todo!("port RcxController::applyCommand (workflow: controller)")
    }

    /// `RcxController::setViewRootId(id)` (`controller.cpp:1859`).
    pub fn set_view_root_id(&mut self, id: u64) {
        if id == self.view_root_id {
            return;
        }
        self.view_root_id = id;
        // refresh() omitted in skeleton.
    }
}
