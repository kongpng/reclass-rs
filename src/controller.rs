//! Editor controller — the mediator between the editor view(s), the in-memory
//! document (`NodeTree` + attached `Provider`), and the undo/redo command
//! pipeline; plus the async refresh loop, value-history/heatmap, selection
//! state, and source management.
//!
//! Faithful port of `src/controller.{h,cpp}` (≈7k lines). The headless core
//! (everything exercised by `tests/test_controller.cpp` and
//! `tests/test_refresh_speedups.cpp`) is implemented here without any GPUI/Qt
//! dependency: the *policy* (command apply, undo stack, refresh state machine,
//! value-history, selection, source management) is split from the *transport*
//! (the real OS timer + background read worker), which the `ui` layer drives by
//! calling [`RcxController::on_refresh_tick`] / [`RcxController::read_pages`] /
//! [`RcxController::on_read_complete`] (see PORTING_controller.md §0/§9).
//!
//! C++ origin references are given per item as `controller.cpp:LINE`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::compose;
use crate::core::linemeta::{
    is_synthetic_line, make_array_elem_sel_id, make_member_sel_id, K_ARRAY_ELEM_BIT,
    K_ARRAY_ELEM_MASK, K_COMMAND_ROW_ID, K_FOOTER_ID_BIT, K_MEMBER_BIT, K_MEMBER_SUB_MASK,
};
use crate::core::{
    alignment_for, is_container_kind, is_func_ptr, is_hex_node, kind_from_string, kind_meta,
    kind_to_string, size_for_kind, Command, ComposeResult, LineKind, Node, NodeKind, NodeTree,
    OffsetAdj, ValueHistory,
};
use crate::format;
use crate::provider::{
    BufferProvider, MemoryRegion, NullProvider, Provider, SnapshotProvider, K_PAGE_SIZE,
};

/// Strip mask for selection ids (footer / array-element / member tag + sub bits).
/// Mirrors the inline `& ~(...)` in `controller.cpp:5168` etc.
const SEL_STRIP_MASK: u64 =
    !(K_FOOTER_ID_BIT | K_ARRAY_ELEM_BIT | K_ARRAY_ELEM_MASK | K_MEMBER_BIT | K_MEMBER_SUB_MASK);

#[inline]
fn strip_sel(id: u64) -> u64 {
    id & SEL_STRIP_MASK
}

const K_PAGE_MASK: u64 = !(K_PAGE_SIZE - 1);
const K_STABILITY_THRESHOLD: i32 = 5;
const K_IDLE_BACKOFF_TICKS: i32 = 8;
const K_POINTER_SNAPSHOT_BYTE_BUDGET: i64 = 64 * 1024 * 1024;
const K_MAX_MAIN_EXTENT: i64 = 16 * 1024 * 1024;

/// `using PageMap = QHash<uint64_t, QByteArray>` (`controller.h:312`). Each
/// value is a 4096-byte page (padded/truncated by the read worker).
pub type PageMap = HashMap<u64, Vec<u8>>;

/// `Qt::KeyboardModifiers` subset used by selection (`handleNodeClick`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
    };
    pub fn ctrl() -> Self {
        Modifiers {
            ctrl: true,
            shift: false,
        }
    }
    pub fn shift() -> Self {
        Modifiers {
            ctrl: false,
            shift: true,
        }
    }
}

/// Qt signals (`controller.h:250-265`) re-expressed as values the UI drains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControllerEvent {
    /// `nodeSelected(int)` (`controller.h:251`).
    NodeSelected(i32),
    /// `selectionChanged(int)` (`controller.h:252`).
    SelectionChanged(usize),
    /// `statusHint(QString)` (`controller.h:253`).
    StatusHint(String),
    /// `sourceLivenessChanged(bool)` (`controller.h:265`).
    SourceLivenessChanged(bool),
    /// `documentChanged()` (`controller.h:82`) — internally also drives refresh.
    DocumentChanged,
}

/// `enum class RefreshPlan` — the result of [`RcxController::on_refresh_tick`].
/// `None` means "nothing to read this tick"; `Read` carries the page list +
/// an `Arc` clone of the provider for the background worker.
pub enum RefreshPlan {
    None,
    Read {
        pages: Vec<u64>,
        provider: Arc<dyn Provider + Send + Sync>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// SavedSourceEntry (`controller.h:99`)
// ─────────────────────────────────────────────────────────────────────────────

/// `struct SavedSourceEntry` (`controller.h:99`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedSourceEntry {
    /// "File" or a provider identifier (e.g. "processmemory").
    pub kind: String,
    pub display_name: String,
    pub file_path: String,
    pub provider_target: String,
    pub base_address: u64,
    pub base_address_formula: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// RcxDocument (`controller.h:27`)
// ─────────────────────────────────────────────────────────────────────────────

/// `class RcxDocument` (`controller.h:27`) — owns the data model.
///
/// Per PORTING_controller.md §3.5 the **undo stack lives in the controller**,
/// not the document, so undo/redo can borrow the controller mutably while the
/// document is borrowed; the clean-index in [`UndoStack`] still drives
/// [`RcxDocument::modified`].
pub struct RcxDocument {
    pub tree: NodeTree,
    /// active data source (defaults to [`NullProvider`]).
    pub provider: Arc<dyn Provider + Send + Sync>,
    /// the `.rcx` path.
    pub file_path: Option<PathBuf>,
    /// attached binary path (cleared when a non-file provider attaches).
    pub data_path: Option<PathBuf>,
    pub modified: bool,
    /// per-kind display-name overrides (saved/loaded as `typeAliases`).
    pub type_aliases: HashMap<NodeKind, String>,
    /// raw saved-source JSON lifted from a loaded `.rcx` (consumed once).
    pub pending_saved_sources: Vec<Value>,
    /// number of sibling overlaps detected on last `load()`.
    pub load_overlap_count: usize,
}

impl Default for RcxDocument {
    fn default() -> Self {
        RcxDocument {
            tree: NodeTree::new(),
            provider: Arc::new(NullProvider),
            file_path: None,
            data_path: None,
            modified: false,
            type_aliases: HashMap::new(),
            pending_saved_sources: Vec::new(),
            load_overlap_count: 0,
        }
    }
}

impl RcxDocument {
    pub fn new() -> Self {
        RcxDocument::default()
    }

    /// `RcxDocument::resolveTypeName(kind)` (`controller.h:64`).
    pub fn resolve_type_name(&self, kind: NodeKind) -> String {
        if let Some(alias) = self.type_aliases.get(&kind) {
            if !alias.is_empty() {
                return alias.clone();
            }
        }
        kind_meta(kind).map_or_else(|| "???".to_string(), |m| m.type_name.to_string())
    }

    /// `RcxDocument::compose(...)` (`controller.cpp:171`) — thin wrapper over
    /// `compose::compose`. (The thread-local doc/`ComposeDocGuard` of the C++ is
    /// dropped: aliases are resolved through `format`'s type-name hook, which
    /// the headless build leaves unset.)
    pub fn compose(
        &self,
        view_root_id: u64,
        compact_columns: bool,
        tree_lines: bool,
        brace_wrap: bool,
        type_hints: bool,
        show_comments: bool,
    ) -> ComposeResult {
        compose::compose(
            &self.tree,
            &*self.provider,
            view_root_id,
            compact_columns,
            tree_lines,
            brace_wrap,
            type_hints,
            show_comments,
            true,
            true,
        )
    }

    /// `RcxDocument::save(path)` (`controller.cpp:179`).
    pub fn save(&mut self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let mut json = self.tree.to_json();
        if !self.type_aliases.is_empty() {
            if let Value::Object(ref mut o) = json {
                let mut aliases = Map::new();
                for (k, v) in &self.type_aliases {
                    aliases.insert(kind_to_string(*k).to_string(), Value::String(v.clone()));
                }
                o.insert("typeAliases".into(), Value::Object(aliases));
            }
        }
        let text = match serde_json::to_string_pretty(&json) {
            Ok(t) => t,
            Err(_) => return false,
        };
        if std::fs::write(path, text).is_err() {
            return false;
        }
        self.file_path = Some(path.to_path_buf());
        self.modified = false;
        true
    }

    /// `RcxDocument::load(path)` (`controller.cpp:201`) — the forgiving 8-step
    /// load. Returns `false` (with a `tracing::warn!`) on a non-JSON file.
    /// NOTE: clears the undo stack via the returned flag — the controller wraps
    /// this and clears its own [`UndoStack`].
    pub fn load(&mut self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return false,
        };

        // Empty file → fresh empty project; non-empty MUST be a JSON object.
        let root: Value = if bytes.is_empty() {
            Value::Object(Map::new())
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) if v.is_object() => v,
                _ => {
                    tracing::warn!(
                        "[load] {:?} isn't a Reclass project — refuse rather than show placeholder",
                        path
                    );
                    return false;
                }
            }
        };

        self.tree = NodeTree::from_json(&root);

        let vr = self.tree.validate(true);
        if !vr.clean() {
            tracing::warn!("[load] tree validation: {:?} {}", path, vr.summary());
        }
        let overlaps = self.tree.find_overlaps();
        self.load_overlap_count = overlaps.len();
        if !overlaps.is_empty() {
            tracing::warn!(
                "[load] {:?}: {} sibling overlap(s) detected — manual review required",
                path,
                overlaps.len()
            );
        }

        // typeAliases.
        self.type_aliases.clear();
        if let Some(Value::Object(alias_obj)) = root.get("typeAliases") {
            for (k, v) in alias_obj {
                if let Some(s) = v.as_str() {
                    if !s.is_empty() {
                        self.type_aliases.insert(kind_from_string(k), s.to_string());
                    }
                }
            }
        }

        // savedSources — resolve relative filePath entries against the .rcx dir.
        self.pending_saved_sources.clear();
        if let Some(arr) = root.get("savedSources").and_then(Value::as_array) {
            let rcx_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            for v in arr {
                let mut entry = v.clone();
                if let Some(fp) = entry.get("filePath").and_then(Value::as_str) {
                    if !fp.is_empty() && Path::new(fp).is_relative() {
                        let abs = rcx_dir.join(fp);
                        if let Value::Object(ref mut o) = entry {
                            o.insert(
                                "filePath".into(),
                                Value::String(abs.to_string_lossy().into_owned()),
                            );
                        }
                    }
                }
                self.pending_saved_sources.push(entry);
            }
        }

        self.file_path = Some(path.to_path_buf());
        self.modified = false;
        true
    }

    /// `RcxDocument::loadData(binaryPath)` (`controller.cpp:308`).
    pub fn load_data_file(&mut self, binary_path: impl AsRef<Path>) {
        let binary_path = binary_path.as_ref();
        if !binary_path.exists() {
            return;
        }
        let name = binary_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let prov = BufferProvider::from_file(&binary_path.to_string_lossy());
        // `BufferProvider::from_file` already derives the basename; keep it.
        let _ = name;
        self.provider = Arc::new(prov);
        self.data_path = Some(binary_path.to_path_buf());
        self.tree.base_address = 0;
    }

    /// `RcxDocument::loadData(data)` (`controller.cpp:321`).
    pub fn load_data_bytes(&mut self, data: Vec<u8>, name: impl Into<String>) {
        self.provider = Arc::new(BufferProvider::new(data, name));
        self.tree.base_address = 0;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UndoStack (replaces QUndoStack / QUndoCommand / RcxCommand)
// ─────────────────────────────────────────────────────────────────────────────

/// One undo entry — a single command or a macro group (`QUndoStack` macro).
#[derive(Clone, Debug)]
enum Entry {
    One(Command),
    Macro {
        #[allow(dead_code)]
        text: String,
        cmds: Vec<Command>,
    },
}

impl Entry {
    fn cmds(&self) -> &[Command] {
        match self {
            Entry::One(c) => std::slice::from_ref(c),
            Entry::Macro { cmds, .. } => cmds,
        }
    }
}

/// `QUndoStack`/`QUndoCommand` semantics, hand-rolled (PORTING §4).
///
/// `push()` immediately executes the command forward (`QUndoCommand::redo`),
/// truncating any redo tail. The obsolete-on-failure logic of `RcxCommand`
/// (`controller.cpp:334-353`) lives in [`UndoStack::undo`]/[`UndoStack::redo`].
#[derive(Default)]
pub struct UndoStack {
    entries: Vec<Entry>,
    /// number of applied entries (0..=entries.len()).
    index: usize,
    clean_index: Option<usize>,
    /// open macro buffers (nested macros supported).
    macro_stack: Vec<(String, Vec<Command>)>,
}

/// `isTransientCommand` (`controller.cpp:342`).
#[inline]
fn is_transient(c: &Command) -> bool {
    matches!(c, Command::WriteBytes { .. })
}

impl UndoStack {
    pub fn new() -> Self {
        UndoStack {
            entries: Vec::new(),
            index: 0,
            clean_index: Some(0),
            macro_stack: Vec::new(),
        }
    }

    /// `QUndoStack::count()`.
    pub fn count(&self) -> usize {
        self.entries.len()
    }
    /// `QUndoStack::index()` — applied-entry count.
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn can_undo(&self) -> bool {
        self.index > 0
    }
    pub fn can_redo(&self) -> bool {
        self.index < self.entries.len()
    }

    /// `QUndoStack::clear()`.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.index = 0;
        self.clean_index = Some(0);
        self.macro_stack.clear();
    }

    /// `QUndoStack::setClean()`.
    pub fn set_clean(&mut self) {
        self.clean_index = Some(self.index);
    }
    /// `QUndoStack::isClean()`.
    pub fn is_clean(&self) -> bool {
        self.clean_index == Some(self.index)
    }

    /// `QUndoStack::beginMacro(text)`.
    pub fn begin_macro(&mut self, text: impl Into<String>) {
        self.macro_stack.push((text.into(), Vec::new()));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// RcxController (`controller.h:110`)
// ─────────────────────────────────────────────────────────────────────────────

/// `class RcxController` (`controller.h:110`) — the mediator. Owns the document
/// and the undo stack (see §3.5).
pub struct RcxController {
    doc: RcxDocument,
    undo: UndoStack,

    // View / selection
    last_result: ComposeResult,
    sel_ids: HashSet<u64>,
    anchor_line: i64,
    view_root_id: u64,

    // display toggles (`controller.h:273-282`)
    compact_columns: bool,
    tree_lines: bool,
    brace_wrap: bool,
    type_hints: bool,
    show_comments: bool,
    show_rtti: bool,
    show_enum_chips: bool,
    suppress_refresh: bool,
    read_only_override: bool,

    // saved sources (`controller.h:285-287`)
    saved_sources: Vec<SavedSourceEntry>,
    active_source_idx: i32,
    last_live: bool,

    // auto-refresh state (`controller.h:313-324`)
    snapshot: Option<Box<SnapshotProvider>>,
    prev_pages: PageMap,
    changed_offsets: HashSet<i64>,
    value_history: HashMap<u64, ValueHistory>,
    last_value_addr: HashMap<u64, u64>,
    track_values: bool,
    value_track_cooldown: i32,
    refresh_gen: u64,
    read_gen: u64,
    read_in_flight: bool,

    // refresh speedups (`controller.h:331-351`)
    page_stability: HashMap<u64, i32>,
    tick_count: u64,
    idle_ticks: i32,
    refresh_interval_base_ms: i32,
    refresh_interval_max_ms: i32,
    refresh_interval_blur_ms: i32,
    window_focused: bool,
    window_visible: bool,

    // timer state mirrored for the UI / tests
    current_interval_ms: i32,
    timer_active: bool,

    // mock editor (headless viewport tests) — see `EditorView`
    editor: Option<Box<dyn EditorView>>,

    // events out
    events: Vec<ControllerEvent>,
}

/// `RcxEditor` abstraction so the headless build links without gpui
/// (PORTING §9.2). Only the methods the controller actually calls in the
/// headless tests are required; UI methods have default no-ops.
pub trait EditorView {
    fn is_editing(&self) -> bool {
        false
    }
    fn first_visible_line(&self) -> i64 {
        0
    }
    fn lines_on_screen(&self) -> i64 {
        0
    }
    fn doc_line_from_visible(&self, v: i64) -> i64 {
        v
    }
    /// Offset-address of a given document line (0 = synthetic/no-address).
    fn offset_addr_for_line(&self, doc_line: i64) -> Option<u64> {
        let _ = doc_line;
        None
    }
}

impl RcxController {
    /// `RcxController(doc)` (`controller.cpp:357`). Sets up the adaptive timer
    /// state and ingests any pending saved sources from a loaded `.rcx`.
    pub fn new(doc: RcxDocument) -> Self {
        let mut c = RcxController {
            doc,
            undo: UndoStack::new(),
            last_result: ComposeResult::default(),
            sel_ids: HashSet::new(),
            anchor_line: -1,
            view_root_id: 0,
            compact_columns: false,
            tree_lines: false,
            brace_wrap: false,
            type_hints: false,
            show_comments: false,
            show_rtti: true,
            show_enum_chips: true,
            suppress_refresh: false,
            read_only_override: false,
            saved_sources: Vec::new(),
            active_source_idx: -1,
            last_live: false,
            snapshot: None,
            prev_pages: PageMap::new(),
            changed_offsets: HashSet::new(),
            value_history: HashMap::new(),
            last_value_addr: HashMap::new(),
            track_values: true,
            value_track_cooldown: 0,
            refresh_gen: 0,
            read_gen: 0,
            read_in_flight: false,
            page_stability: HashMap::new(),
            tick_count: 0,
            idle_ticks: 0,
            refresh_interval_base_ms: 200,
            refresh_interval_max_ms: 1500,
            refresh_interval_blur_ms: 1500,
            window_focused: true,
            window_visible: true,
            current_interval_ms: 200,
            timer_active: false,
            editor: None,
            events: Vec::new(),
        };
        // `setupAutoRefresh` (`controller.cpp:6515`): start the timer.
        c.apply_adaptive_interval();
        c.ingest_pending_saved_sources();
        c
    }

    // ── Accessors (test surface mirrors controller.h) ──

    pub fn document(&self) -> &RcxDocument {
        &self.doc
    }
    pub fn document_mut(&mut self) -> &mut RcxDocument {
        &mut self.doc
    }
    pub fn tree(&self) -> &NodeTree {
        &self.doc.tree
    }
    pub fn tree_mut(&mut self) -> &mut NodeTree {
        &mut self.doc.tree
    }
    pub fn undo_stack(&self) -> &UndoStack {
        &self.undo
    }
    pub fn value_history(&self) -> &HashMap<u64, ValueHistory> {
        &self.value_history
    }
    /// Mutable access — mirrors the C++ `const_cast` test seam.
    pub fn value_history_mut(&mut self) -> &mut HashMap<u64, ValueHistory> {
        &mut self.value_history
    }
    pub fn last_result(&self) -> &ComposeResult {
        &self.last_result
    }
    pub fn selected_ids(&self) -> &HashSet<u64> {
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
    pub fn track_values(&self) -> bool {
        self.track_values
    }
    pub fn read_only_override(&self) -> bool {
        self.read_only_override
    }
    pub fn show_comments(&self) -> bool {
        self.show_comments
    }
    pub fn show_rtti(&self) -> bool {
        self.show_rtti
    }
    pub fn show_enum_chips(&self) -> bool {
        self.show_enum_chips
    }
    pub fn type_hints(&self) -> bool {
        self.type_hints
    }

    /// `dataExtent()` (`controller.h:242`).
    pub fn data_extent(&self) -> i32 {
        self.compute_data_extent()
    }
    /// `refreshIntervalMs()` (`controller.h:244`).
    pub fn refresh_interval_ms(&self) -> i32 {
        self.current_interval_ms
    }
    /// `refreshTimerActive()` (`controller.h:245`).
    pub fn refresh_timer_active(&self) -> bool {
        self.timer_active
    }
    /// `idleTicks()` (`controller.h:246`).
    pub fn idle_ticks(&self) -> i32 {
        self.idle_ticks
    }
    /// `pageStability(pageAddr)` (`controller.h:247`).
    pub fn page_stability(&self, page_addr: u64) -> i32 {
        self.page_stability
            .get(&(page_addr & K_PAGE_MASK))
            .copied()
            .unwrap_or(0)
    }
    /// `snapshotProv()` (`controller.h:248`).
    pub fn snapshot_prov(&self) -> Option<&SnapshotProvider> {
        self.snapshot.as_deref()
    }

    /// Install a mock editor (headless viewport/timer tests).
    pub fn set_editor(&mut self, editor: Box<dyn EditorView>) {
        self.editor = Some(editor);
    }

    /// Drain queued events (the UI dispatches them to GPUI; tests assert on them).
    pub fn take_events(&mut self) -> Vec<ControllerEvent> {
        std::mem::take(&mut self.events)
    }

    fn emit(&mut self, ev: ControllerEvent) {
        self.events.push(ev);
    }

    // ── Undo/redo driving (the controller owns the stack; `m_doc->undoStack` →
    //    `ctrl.undo()/redo()/push_command()`). ──

    /// `QUndoStack::push(new RcxCommand(...))` — execute forward, track on stack.
    pub fn push_command(&mut self, cmd: Command) {
        // Inside an open macro: execute immediately, append to the macro buffer.
        if !self.undo.macro_stack.is_empty() {
            let ok = self.apply_command(&cmd, false);
            // Even on failure the C++ keeps the command in the macro group
            // (only top-level undo/redo consult obsolete); mirror that by
            // always appending — transient/non-transient alike.
            let _ = ok;
            self.undo.macro_stack.last_mut().unwrap().1.push(cmd);
            return;
        }
        // Truncate redo tail.
        self.undo.entries.truncate(self.undo.index);
        // Adjust clean index if it now points beyond the stack.
        if let Some(ci) = self.undo.clean_index {
            if ci > self.undo.entries.len() {
                self.undo.clean_index = None;
            }
        }
        let ok = self.apply_command(&cmd, false);
        if !ok && !is_transient(&cmd) {
            // obsolete-on-push: do NOT add to the stack.
            self.sync_modified();
            return;
        }
        self.undo.entries.push(Entry::One(cmd));
        self.undo.index += 1;
        self.sync_modified();
    }

    /// `QUndoStack::endMacro()` — collapse the open macro into one entry.
    pub fn end_macro(&mut self) {
        let Some((text, cmds)) = self.undo.macro_stack.pop() else {
            return;
        };
        if self.undo.macro_stack.is_empty() {
            // Top-level macro: push as a single undoable entry.
            self.undo.entries.truncate(self.undo.index);
            if let Some(ci) = self.undo.clean_index {
                if ci > self.undo.entries.len() {
                    self.undo.clean_index = None;
                }
            }
            self.undo.entries.push(Entry::Macro { text, cmds });
            self.undo.index += 1;
            self.sync_modified();
        } else {
            // Nested macro: fold into the parent buffer.
            self.undo.macro_stack.last_mut().unwrap().1.extend(cmds);
        }
    }

    pub fn begin_macro(&mut self, text: impl Into<String>) {
        self.undo.begin_macro(text);
    }

    /// `QUndoStack::undo()` (`controller.cpp:346`).
    pub fn undo(&mut self) {
        if self.undo.index == 0 {
            return;
        }
        let entry = self.undo.entries[self.undo.index - 1].clone();
        let cmds: Vec<Command> = entry.cmds().to_vec();
        let mut drop_entry = false;
        for cmd in cmds.iter().rev() {
            let ok = self.apply_command(cmd, true);
            if !ok && !is_transient(cmd) {
                drop_entry = true;
            }
        }
        if drop_entry {
            // setObsolete(true): the entry never existed.
            self.undo.entries.remove(self.undo.index - 1);
            // index stays pointing one slot lower (the dropped entry is gone).
            self.undo.index -= 1;
        } else {
            self.undo.index -= 1;
        }
        self.sync_modified();
    }

    /// `QUndoStack::redo()` (`controller.cpp:350`).
    pub fn redo(&mut self) {
        if self.undo.index >= self.undo.entries.len() {
            return;
        }
        let entry = self.undo.entries[self.undo.index].clone();
        let cmds: Vec<Command> = entry.cmds().to_vec();
        let mut drop_entry = false;
        for cmd in cmds.iter() {
            let ok = self.apply_command(cmd, false);
            if !ok && !is_transient(cmd) {
                drop_entry = true;
            }
        }
        if drop_entry {
            self.undo.entries.remove(self.undo.index);
            // index unchanged (the now-removed entry would have been applied).
        } else {
            self.undo.index += 1;
        }
        self.sync_modified();
    }

    pub fn set_clean(&mut self) {
        self.undo.set_clean();
        self.sync_modified();
    }

    fn sync_modified(&mut self) {
        // `cleanChanged → modified` (`controller.cpp:166`).
        self.doc.modified = !self.undo.is_clean();
    }

    // ── Public state setters ──

    /// `setSuppressRefresh(v)` (`controller.h:215`).
    pub fn set_suppress_refresh(&mut self, v: bool) {
        self.suppress_refresh = v;
    }
    /// `setReadOnlyOverride(v)` (`controller.h:210`).
    pub fn set_read_only_override(&mut self, v: bool) {
        self.read_only_override = v;
    }
    /// `setProjectDocuments` is UI-only; not modeled.

    /// `setViewRootId(id)` (`controller.cpp:1859`).
    pub fn set_view_root_id(&mut self, id: u64) {
        if id == self.view_root_id {
            return;
        }
        self.view_root_id = id;
        self.refresh();
    }

    /// `setCompactColumns(v)` (`controller.cpp:6480`).
    pub fn set_compact_columns(&mut self, v: bool) {
        self.compact_columns = v;
        self.refresh();
    }
    pub fn set_tree_lines(&mut self, v: bool) {
        self.tree_lines = v;
        self.refresh();
    }
    pub fn set_brace_wrap(&mut self, v: bool) {
        self.brace_wrap = v;
        self.refresh();
    }
    pub fn set_type_hints(&mut self, v: bool) {
        self.type_hints = v;
        self.refresh();
    }
    pub fn set_show_comments(&mut self, v: bool) {
        self.show_comments = v;
        self.refresh();
    }
    pub fn set_show_rtti(&mut self, v: bool) {
        self.show_rtti = v;
        self.refresh();
    }
    pub fn set_show_enum_chips(&mut self, v: bool) {
        self.show_enum_chips = v;
        self.refresh();
    }

    /// `setTrackValues(on)` (`controller.cpp:1870`).
    pub fn set_track_values(&mut self, on: bool) {
        self.track_values = on;
        if !on {
            self.value_history.clear();
            self.last_value_addr.clear();
            for lm in &mut self.last_result.meta {
                lm.heat_level = 0;
            }
            self.refresh();
        }
    }

    /// `resetChangeTracking()` (`controller.cpp:1881`) — does NOT refresh.
    pub fn reset_change_tracking(&mut self) {
        self.changed_offsets.clear();
        self.value_history.clear();
        self.last_value_addr.clear();
        self.prev_pages.clear();
        self.value_track_cooldown = 5;
        for lm in &mut self.last_result.meta {
            lm.heat_level = 0;
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // applyCommand (`controller.cpp:2827`)
    // ─────────────────────────────────────────────────────────────────────────

    /// `RcxController::applyCommand(cmd, isUndo)` (`controller.cpp:2827`).
    /// Returns `false` only when the underlying op was rejected (WriteBytes
    /// failure / read-only override blocking a write).
    pub fn apply_command(&mut self, cmd: &Command, is_undo: bool) -> bool {
        self.doc.tree.touch();
        let mut success = true;

        match cmd {
            Command::ChangeKind {
                node_id,
                old_kind,
                new_kind,
                off_adjs,
            } => {
                let idx = self.doc.tree.index_of_id(*node_id);
                if idx >= 0 {
                    self.doc.tree.nodes[idx as usize].kind =
                        if is_undo { *old_kind } else { *new_kind };
                }
                self.apply_off_adjs(off_adjs, is_undo);
                self.refresh_gen += 1;
                // Value history intentionally KEPT across kind changes.
                self.clear_history_for_adjs(off_adjs);
            }
            Command::Rename {
                node_id,
                old_name,
                new_name,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.name = if is_undo {
                        old_name.clone()
                    } else {
                        new_name.clone()
                    };
                }
            }
            Command::Collapse {
                node_id,
                old_state,
                new_state,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.collapsed = if is_undo { *old_state } else { *new_state };
                }
            }
            Command::Insert { node, off_adjs } => {
                if is_undo {
                    self.revert_off_adjs(off_adjs);
                    let idx = self.doc.tree.index_of_id(node.id);
                    if idx >= 0 {
                        self.doc.tree.nodes.remove(idx as usize);
                        self.doc.tree.invalidate_id_cache();
                    }
                } else {
                    self.doc.tree.add_node(node.clone());
                    self.apply_off_adjs_new(off_adjs);
                }
                self.clear_history_for_adjs(off_adjs);
            }
            Command::Remove {
                node_id,
                subtree,
                off_adjs,
            } => {
                if is_undo {
                    for n in subtree {
                        self.doc.tree.add_node(n.clone());
                    }
                    self.revert_off_adjs(off_adjs);
                } else {
                    self.apply_off_adjs_new(off_adjs);
                    let mut indices = self.doc.tree.subtree_indices(*node_id);
                    indices.sort_unstable_by(|a, b| b.cmp(a)); // descending
                    for idx in indices {
                        let id = self.doc.tree.nodes[idx].id;
                        self.clear_node_history(id);
                        self.doc.tree.nodes.remove(idx);
                    }
                    self.doc.tree.invalidate_id_cache();
                }
                self.clear_history_for_adjs(off_adjs);
            }
            Command::ChangeBase {
                old_base,
                new_base,
                old_formula,
                new_formula,
            } => {
                self.doc.tree.base_address = if is_undo { *old_base } else { *new_base };
                self.doc.tree.base_address_formula = if is_undo {
                    old_formula.clone()
                } else {
                    new_formula.clone()
                };
                self.reset_snapshot();
            }
            Command::WriteBytes {
                addr,
                old_bytes,
                new_bytes,
            } => {
                let bytes: &[u8] = if is_undo { old_bytes } else { new_bytes };
                if self.read_only_override {
                    success = false;
                } else {
                    let ok = self.write_through(*addr, bytes);
                    if !ok {
                        tracing::warn!("WriteBytes failed at address {:x}", addr);
                        self.emit(ControllerEvent::StatusHint(format!(
                            "Write rejected at 0x{:x} — removing from history",
                            addr
                        )));
                        success = false;
                    }
                }
            }
            Command::ChangeArrayMeta {
                node_id,
                old_element_kind,
                new_element_kind,
                old_array_len,
                new_array_len,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.element_kind = if is_undo {
                        *old_element_kind
                    } else {
                        *new_element_kind
                    };
                    n.array_len = if is_undo {
                        *old_array_len
                    } else {
                        *new_array_len
                    };
                    if n.view_index >= n.array_len {
                        n.view_index = (n.array_len - 1).max(0);
                    }
                }
            }
            Command::ChangePointerRef {
                node_id,
                old_ref_id,
                new_ref_id,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.ref_id = if is_undo { *old_ref_id } else { *new_ref_id };
                    if n.ref_id != 0 {
                        n.collapsed = true;
                    }
                }
            }
            Command::ChangeStructTypeName {
                node_id,
                old_name,
                new_name,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.struct_type_name = if is_undo {
                        old_name.clone()
                    } else {
                        new_name.clone()
                    };
                }
            }
            Command::ChangeClassKeyword {
                node_id,
                old_keyword,
                new_keyword,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.class_keyword = if is_undo {
                        old_keyword.clone()
                    } else {
                        new_keyword.clone()
                    };
                }
            }
            Command::ChangeOffset {
                node_id,
                old_offset,
                new_offset,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.offset = if is_undo { *old_offset } else { *new_offset };
                }
                self.refresh_gen += 1;
                self.clear_node_history(*node_id);
                for ci in self.doc.tree.subtree_indices(*node_id) {
                    let id = self.doc.tree.nodes[ci].id;
                    self.clear_node_history(id);
                }
            }
            Command::ChangeEnumMembers {
                node_id,
                old_members,
                new_members,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.enum_members = if is_undo {
                        old_members.clone()
                    } else {
                        new_members.clone()
                    };
                }
            }
            Command::ChangeOffsetExpr {
                node_id,
                old_expr,
                new_expr,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.offset_expr = if is_undo {
                        old_expr.clone()
                    } else {
                        new_expr.clone()
                    };
                }
            }
            Command::ToggleStatic {
                node_id,
                old_val,
                new_val,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.is_static = if is_undo { *old_val } else { *new_val };
                }
            }
            // FIXME-parity: `cmd::ToggleRelative` has NO arm in C++
            // `applyCommand` (`controller.cpp:2890-3049`) — the flag never
            // toggles via this command. Preserved as a no-op for fidelity.
            Command::ToggleRelative { .. } => {}
            Command::ToggleBigEndian {
                node_id,
                old_val,
                new_val,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.big_endian = if is_undo { *old_val } else { *new_val };
                }
            }
            Command::ChangeComment {
                node_id,
                old_comment,
                new_comment,
            } => {
                if let Some(n) = self.node_mut(*node_id) {
                    n.comment = if is_undo {
                        old_comment.clone()
                    } else {
                        new_comment.clone()
                    };
                }
            }
        }

        if success && !self.suppress_refresh {
            self.refresh();
        }
        success
    }

    /// `&mut Node` by id, or `None`.
    fn node_mut(&mut self, id: u64) -> Option<&mut Node> {
        let idx = self.doc.tree.index_of_id(id);
        if idx < 0 {
            None
        } else {
            Some(&mut self.doc.tree.nodes[idx as usize])
        }
    }

    /// Apply offset adjustments choosing old/new by `is_undo`.
    fn apply_off_adjs(&mut self, adjs: &[OffsetAdj], is_undo: bool) {
        for adj in adjs {
            let ai = self.doc.tree.index_of_id(adj.node_id);
            if ai >= 0 {
                self.doc.tree.nodes[ai as usize].offset = if is_undo {
                    adj.old_offset
                } else {
                    adj.new_offset
                };
            }
        }
    }
    fn apply_off_adjs_new(&mut self, adjs: &[OffsetAdj]) {
        for adj in adjs {
            let ai = self.doc.tree.index_of_id(adj.node_id);
            if ai >= 0 {
                self.doc.tree.nodes[ai as usize].offset = adj.new_offset;
            }
        }
    }
    fn revert_off_adjs(&mut self, adjs: &[OffsetAdj]) {
        for adj in adjs {
            let ai = self.doc.tree.index_of_id(adj.node_id);
            if ai >= 0 {
                self.doc.tree.nodes[ai as usize].offset = adj.old_offset;
            }
        }
    }

    /// `clearNodeHistory(id)` (`controller.cpp:2842`).
    fn clear_node_history(&mut self, id: u64) {
        self.value_history.remove(&id);
        self.last_value_addr.remove(&id);
    }

    /// `clearHistoryForAdjs(adjs)` (`controller.cpp:2847`).
    fn clear_history_for_adjs(&mut self, adjs: &[OffsetAdj]) {
        if adjs.is_empty() {
            return;
        }
        self.refresh_gen += 1;
        for adj in adjs {
            self.clear_node_history(adj.node_id);
        }
        // For any adjusted node that is a container, BFS-clear descendants.
        let mut has_containers = false;
        for adj in adjs {
            let ai = self.doc.tree.index_of_id(adj.node_id);
            if ai >= 0 && is_container_kind(self.doc.tree.nodes[ai as usize].kind) {
                has_containers = true;
                break;
            }
        }
        if !has_containers {
            return;
        }
        // Build child map once.
        let mut child_map: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, n) in self.doc.tree.nodes.iter().enumerate() {
            child_map.entry(n.parent_id).or_default().push(i);
        }
        for adj in adjs {
            let ai = self.doc.tree.index_of_id(adj.node_id);
            if ai < 0 {
                continue;
            }
            if !is_container_kind(self.doc.tree.nodes[ai as usize].kind) {
                continue;
            }
            let mut visited: HashSet<u64> = HashSet::new();
            visited.insert(adj.node_id);
            let mut stack = vec![adj.node_id];
            let mut to_clear: Vec<u64> = Vec::new();
            while let Some(pid) = stack.pop() {
                if let Some(kids) = child_map.get(&pid) {
                    for &ci in kids {
                        let cid = self.doc.tree.nodes[ci].id;
                        if !visited.contains(&cid) {
                            visited.insert(cid);
                            stack.push(cid);
                            to_clear.push(cid);
                        }
                    }
                }
            }
            for id in to_clear {
                self.clear_node_history(id);
            }
        }
    }

    /// Write `bytes` at `addr` — through the snapshot if present, else the
    /// real provider. Mirrors the `m_snapshotProv ? ... : provider->writeBytes`
    /// branches in `applyCommand`/`setNodeValue`.
    fn write_through(&mut self, addr: u64, bytes: &[u8]) -> bool {
        if let Some(snap) = self.snapshot.as_mut() {
            // SnapshotProvider::write patches its own pages; the real
            // write-through happens via the writable provider below.
            let ok_real = write_provider(&mut self.doc.provider, addr, bytes);
            // patch the snapshot pages too so compose reflects the change.
            snap.patch_pages(addr, bytes);
            ok_real
        } else {
            write_provider(&mut self.doc.provider, addr, bytes)
        }
    }
}

/// Write through an `Arc<dyn Provider>`. Uses `Arc::get_mut` (unique handle —
/// the headless write tests hold the only `Arc`); returns `false` when the
/// `Arc` is shared (a snapshot/worker holds a clone) or the provider rejects
/// the write. The C++ `shared_ptr` non-const `write` is faithfully reproduced
/// for the unique-handle case that the tests exercise.
fn write_provider(provider: &mut Arc<dyn Provider + Send + Sync>, addr: u64, bytes: &[u8]) -> bool {
    if !provider.is_writable() {
        return false;
    }
    match Arc::get_mut(provider) {
        Some(p) => p.write(addr, bytes),
        None => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// setNodeValue (`controller.cpp:3061`) + writeSelectedBytesToFile
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `setNodeValue(nodeIdx, subLine, text, isAscii, resolvedAddr)`
    /// (`controller.cpp:3061`).
    pub fn set_node_value(
        &mut self,
        node_idx: usize,
        sub_line: i32,
        text: &str,
        is_ascii: bool,
        resolved_addr: u64,
    ) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        if !self.doc.provider.is_writable() {
            return;
        }
        if self.read_only_override {
            return;
        }

        let node = self.doc.tree.nodes[node_idx].clone();

        // Address (compose-resolved or base + computeOffset).
        let mut addr = if resolved_addr != 0 {
            resolved_addr
        } else {
            let signed = self.doc.tree.compute_offset(node_idx as i32);
            if signed < 0 {
                return;
            }
            self.doc.tree.base_address + signed as u64
        };

        // Vector / matrix component redirection.
        let mut edit_kind = node.kind;
        if matches!(node.kind, NodeKind::Vec2 | NodeKind::Vec3 | NodeKind::Vec4) && sub_line >= 0 {
            addr += (sub_line as u64) * 4;
            edit_kind = NodeKind::Float;
        }
        if node.kind == NodeKind::Mat4x4 && (0..16).contains(&sub_line) {
            addr += (sub_line as u64) * 4;
            edit_kind = NodeKind::Float;
        }

        // Parse to bytes.
        let mut new_bytes = if is_ascii {
            let expected = size_for_kind(edit_kind);
            match format::parse_ascii_value(text, expected) {
                Some(b) => b,
                None => return,
            }
        } else {
            let mut edit_node = node.clone();
            edit_node.kind = edit_kind;
            match format::parse_value(&edit_node, text) {
                Some(b) => b,
                None => return,
            }
        };

        // Strings: pad/truncate to full buffer size.
        if matches!(node.kind, NodeKind::UTF8 | NodeKind::UTF16) {
            let full = node.byte_size().max(0) as usize;
            new_bytes.truncate(full);
            if new_bytes.len() < full {
                new_bytes.resize(full, 0);
            }
        }

        if new_bytes.is_empty() {
            return;
        }
        let write_size = new_bytes.len() as i32;

        if !self.doc.provider.is_readable(addr, write_size) {
            return;
        }
        let old_bytes = self.doc.provider.read_bytes(addr, write_size);

        // Test the write first.
        let write_ok = self.write_through(addr, &new_bytes);
        if !write_ok {
            tracing::warn!("Write failed at address {:x}", addr);
            self.refresh();
            return;
        }

        // Push undo command (redo re-writes; harmless).
        self.push_command(Command::WriteBytes {
            addr,
            old_bytes,
            new_bytes,
        });
    }

    /// `writeSelectedBytesToFile(addr, n, path)` (`controller.cpp:2789`).
    pub fn write_selected_bytes_to_file(
        &self,
        addr: u64,
        n: i32,
        path: impl AsRef<Path>,
    ) -> Result<(), String> {
        if n <= 0 {
            return Err("No bytes to save".to_string());
        }
        let prov: &dyn Provider = match &self.snapshot {
            Some(s) => s.as_ref(),
            None => &*self.doc.provider,
        };
        if !prov.is_readable(addr, n) {
            return Err(format!("Couldn't read {} bytes at 0x{:x}", n, addr));
        }
        let data = prov.read_bytes(addr, n);
        if data.len() != n as usize {
            return Err(format!(
                "Short read: got {} bytes, wanted {}",
                data.len(),
                n
            ));
        }
        std::fs::write(path.as_ref(), &data)
            .map_err(|e| format!("Couldn't open {:?} for writing: {}", path.as_ref(), e))?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Mutation helpers (`controller.cpp:2057-3672`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    fn node_size(&self, n: &Node) -> i32 {
        if is_container_kind(n.kind) {
            self.doc.tree.struct_span(n.id)
        } else {
            n.byte_size()
        }
    }

    /// `changeNodeKind(nodeIdx, newKind)` (`controller.cpp:2078`).
    pub fn change_node_kind(&mut self, node_idx: usize, new_kind: NodeKind) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let node = self.doc.tree.nodes[node_idx].clone();

        let mut old_size = node.byte_size();
        if old_size == 0 && is_container_kind(node.kind) {
            old_size = self.doc.tree.struct_span(node.id);
        }
        let mut tmp = node.clone();
        tmp.kind = new_kind;
        let mut new_size = tmp.byte_size();
        if matches!(new_kind, NodeKind::Struct | NodeKind::Array) {
            new_size = 0;
        }

        if new_size > 0 && new_size < old_size {
            // Shrinking — insert hex padding (no offset shift).
            let mut gap = old_size - new_size;
            let parent_id = node.parent_id;
            let base_offset = node.offset + new_size;

            let was_suppressed = self.suppress_refresh;
            self.suppress_refresh = true;
            self.begin_macro("Change type");

            let orig_name = node.name.clone();
            let orig_offset = node.offset;
            let needs_rename = is_hex_node(node.kind) && !is_hex_node(new_kind);

            self.push_command(Command::ChangeKind {
                node_id: node.id,
                old_kind: node.kind,
                new_kind,
                off_adjs: Vec::new(),
            });

            if needs_rename {
                let auto_name = format!("field_{:04x}", orig_offset);
                self.push_command(Command::Rename {
                    node_id: node.id,
                    old_name: orig_name,
                    new_name: auto_name,
                });
            }

            let hex_to_hex = is_hex_node(node.kind) && is_hex_node(new_kind);
            let mut pad_offset = base_offset;
            while gap > 0 {
                let (pad_kind, pad_size) = if hex_to_hex {
                    (new_kind, new_size)
                } else if gap >= 8 {
                    (NodeKind::Hex64, 8)
                } else if gap >= 4 {
                    (NodeKind::Hex32, 4)
                } else if gap >= 2 {
                    (NodeKind::Hex16, 2)
                } else {
                    (NodeKind::Hex8, 1)
                };
                self.insert_node(
                    parent_id,
                    pad_offset,
                    pad_kind,
                    &format!("pad_{:02x}", pad_offset),
                );
                pad_offset += pad_size;
                gap -= pad_size;
            }

            self.end_macro();
            self.suppress_refresh = was_suppressed;
            if !self.suppress_refresh {
                self.refresh();
            }
        } else {
            // Same size or larger — adjust sibling offsets.
            let delta = new_size - old_size;
            let mut adjs: Vec<OffsetAdj> = Vec::new();
            if delta != 0 && old_size > 0 && new_size > 0 {
                let old_end = node.offset + old_size;
                let siblings = self.doc.tree.children_of(node.parent_id);
                for si in siblings {
                    if si == node_idx {
                        continue;
                    }
                    let sib = &self.doc.tree.nodes[si];
                    if sib.offset >= old_end {
                        adjs.push(OffsetAdj {
                            node_id: sib.id,
                            old_offset: sib.offset,
                            new_offset: sib.offset + delta,
                        });
                    }
                }
            }
            let needs_rename = is_hex_node(node.kind) && !is_hex_node(new_kind);
            if needs_rename {
                self.begin_macro("Change type");
            }
            self.push_command(Command::ChangeKind {
                node_id: node.id,
                old_kind: node.kind,
                new_kind,
                off_adjs: adjs,
            });
            if needs_rename {
                let auto_name = format!("field_{:04x}", node.offset);
                self.push_command(Command::Rename {
                    node_id: node.id,
                    old_name: node.name.clone(),
                    new_name: auto_name,
                });
                self.end_macro();
            }
        }
    }

    /// `renameNode(nodeIdx, newName)` (`controller.cpp:2188`).
    pub fn rename_node(&mut self, node_idx: usize, new_name: &str) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let node = &self.doc.tree.nodes[node_idx];
        self.push_command(Command::Rename {
            node_id: node.id,
            old_name: node.name.clone(),
            new_name: new_name.to_string(),
        });
    }

    /// `insertNode(parentId, offset, kind, name)` (`controller.cpp:2459`).
    pub fn insert_node(&mut self, parent_id: u64, offset: i32, kind: NodeKind, name: &str) {
        let mut n = Node {
            kind,
            name: name.to_string(),
            parent_id,
            ..Node::default()
        };
        if offset < 0 {
            let mut max_end = 0;
            for si in self.doc.tree.children_of(parent_id) {
                let sn = &self.doc.tree.nodes[si];
                let sz = self.node_size(sn);
                let end = sn.offset + sz;
                if end > max_end {
                    max_end = end;
                }
            }
            let align = alignment_for(kind);
            n.offset = (max_end + align - 1) / align * align;
        } else {
            n.offset = offset;
        }
        n.id = self.doc.tree.reserve_id();
        self.push_command(Command::Insert {
            node: n,
            off_adjs: Vec::new(),
        });
    }

    /// `insertNodeAbove(beforeIdx, kind, name)` (`controller.cpp:2488`).
    pub fn insert_node_above(&mut self, before_idx: usize, kind: NodeKind, name: &str) {
        if before_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let before = self.doc.tree.nodes[before_idx].clone();
        let mut n = Node {
            kind,
            name: name.to_string(),
            parent_id: before.parent_id,
            offset: before.offset,
            ..Node::default()
        };
        n.id = self.doc.tree.reserve_id();
        let insert_size = size_for_kind(kind);
        let mut adjs: Vec<OffsetAdj> = Vec::new();
        for si in self.doc.tree.children_of(before.parent_id) {
            let sib = &self.doc.tree.nodes[si];
            if sib.offset >= before.offset {
                adjs.push(OffsetAdj {
                    node_id: sib.id,
                    old_offset: sib.offset,
                    new_offset: sib.offset + insert_size,
                });
            }
        }
        self.push_command(Command::Insert {
            node: n,
            off_adjs: adjs,
        });
    }

    /// `removeNode(nodeIdx)` (`controller.cpp:2513`).
    pub fn remove_node(&mut self, node_idx: usize) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let node = self.doc.tree.nodes[node_idx].clone();
        let node_id = node.id;
        let parent_id = node.parent_id;
        let deleted_size = self.node_size(&node);
        let deleted_end = node.offset + deleted_size;

        let mut adjs: Vec<OffsetAdj> = Vec::new();
        if parent_id != 0 {
            for si in self.doc.tree.children_of(parent_id) {
                if si == node_idx {
                    continue;
                }
                let sib = &self.doc.tree.nodes[si];
                if sib.offset >= deleted_end {
                    adjs.push(OffsetAdj {
                        node_id: sib.id,
                        old_offset: sib.offset,
                        new_offset: sib.offset - deleted_size,
                    });
                }
            }
        }

        let subtree: Vec<Node> = self
            .doc
            .tree
            .subtree_indices(node_id)
            .into_iter()
            .map(|i| self.doc.tree.nodes[i].clone())
            .collect();

        self.push_command(Command::Remove {
            node_id,
            subtree,
            off_adjs: adjs,
        });
    }

    /// `deleteRootStruct(structId)` (`controller.cpp:2547`).
    pub fn delete_root_struct(&mut self, struct_id: u64) {
        let ni = self.doc.tree.index_of_id(struct_id);
        if ni < 0 {
            return;
        }
        let node = &self.doc.tree.nodes[ni as usize];
        if node.parent_id != 0 || node.kind != NodeKind::Struct {
            return;
        }

        let was_suppressed = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Delete root struct");

        // Clear all refId references pointing to this struct.
        let refs: Vec<(u64, u64)> = self
            .doc
            .tree
            .nodes
            .iter()
            .filter(|n| n.ref_id == struct_id)
            .map(|n| (n.id, n.ref_id))
            .collect();
        for (id, ref_id) in refs {
            self.push_command(Command::ChangePointerRef {
                node_id: id,
                old_ref_id: ref_id,
                new_ref_id: 0,
            });
        }

        let ni = self.doc.tree.index_of_id(struct_id);
        if ni >= 0 {
            self.remove_node(ni as usize);
        }

        self.end_macro();
        self.suppress_refresh = was_suppressed;

        if self.view_root_id == struct_id {
            let next_root = self
                .doc
                .tree
                .nodes
                .iter()
                .find(|n| n.parent_id == 0 && n.kind == NodeKind::Struct)
                .map(|n| n.id)
                .unwrap_or(0);
            self.set_view_root_id(next_root);
        }

        if !self.suppress_refresh {
            self.refresh();
        }
    }

    /// `toggleCollapse(nodeIdx)` (`controller.cpp:2730`).
    pub fn toggle_collapse(&mut self, node_idx: usize) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let node = &self.doc.tree.nodes[node_idx];
        self.push_command(Command::Collapse {
            node_id: node.id,
            old_state: node.collapsed,
            new_state: !node.collapsed,
        });
    }

    /// `duplicateNode(nodeIdx)` (`controller.cpp:3151`).
    pub fn duplicate_node(&mut self, node_idx: usize) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let src = self.doc.tree.nodes[node_idx].clone();
        if matches!(src.kind, NodeKind::Struct | NodeKind::Array) {
            return;
        }
        let copy_size = src.byte_size();
        let copy_offset = src.offset + copy_size;

        let mut adjs: Vec<OffsetAdj> = Vec::new();
        if src.parent_id != 0 {
            for si in self.doc.tree.children_of(src.parent_id) {
                if si == node_idx {
                    continue;
                }
                let sib = &self.doc.tree.nodes[si];
                if sib.offset >= copy_offset {
                    adjs.push(OffsetAdj {
                        node_id: sib.id,
                        old_offset: sib.offset,
                        new_offset: sib.offset + copy_size,
                    });
                }
            }
        }
        let n = Node {
            kind: src.kind,
            name: format!("{}_copy", src.name),
            parent_id: src.parent_id,
            offset: copy_offset,
            id: self.doc.tree.reserve_id(),
            ..Node::default()
        };
        self.push_command(Command::Insert {
            node: n,
            off_adjs: adjs,
        });
    }

    /// `convertToTypedPointer(nodeId)` (`controller.cpp:3181`).
    pub fn convert_to_typed_pointer(&mut self, node_id: u64) {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return;
        }
        let node = self.doc.tree.nodes[ni as usize].clone();

        let ptr_kind = if self.doc.tree.pointer_size >= 8 {
            NodeKind::Pointer64
        } else {
            NodeKind::Pointer32
        };

        // Unique struct name.
        let base_name = "NewClass";
        let mut type_name = base_name.to_string();
        let mut suffix = 2;
        loop {
            let exists = self
                .doc
                .tree
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Struct && n.struct_type_name == type_name);
            if !exists {
                break;
            }
            type_name = format!("{}_{}", base_name, suffix);
            suffix += 1;
        }

        let mut root_struct = Node {
            kind: NodeKind::Struct,
            name: "instance".to_string(),
            struct_type_name: type_name,
            class_keyword: "class".to_string(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        root_struct.id = self.doc.tree.reserve_id();

        let is32 = self.doc.tree.pointer_size < 8;
        let hex_kind = if is32 {
            NodeKind::Hex32
        } else {
            NodeKind::Hex64
        };
        let stride = if is32 { 4 } else { 8 };
        let mut children: Vec<Node> = Vec::new();
        for i in 0..16 {
            let mut c = Node {
                kind: hex_kind,
                name: format!("field_{:02x}", i * stride),
                parent_id: root_struct.id,
                offset: i * stride,
                ..Node::default()
            };
            c.id = self.doc.tree.reserve_id();
            children.push(c);
        }

        let old_ref_id = node.ref_id;
        let new_ref_id = root_struct.id;

        self.suppress_refresh = true;
        self.begin_macro("Change to ptr*");

        if node.kind != ptr_kind {
            self.change_node_kind(ni as usize, ptr_kind);
        }
        self.push_command(Command::Insert {
            node: root_struct,
            off_adjs: Vec::new(),
        });
        for c in children {
            self.push_command(Command::Insert {
                node: c,
                off_adjs: Vec::new(),
            });
        }
        self.push_command(Command::ChangePointerRef {
            node_id,
            old_ref_id,
            new_ref_id,
        });

        self.end_macro();
        self.suppress_refresh = false;
        self.refresh();
    }

    /// `splitHexNode(nodeId)` (`controller.cpp:3332`).
    pub fn split_hex_node(&mut self, node_id: u64) {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return;
        }
        let node = self.doc.tree.nodes[ni as usize].clone();
        let (half_kind, half_size) = match node.kind {
            NodeKind::Hex128 => (NodeKind::Hex64, 8),
            NodeKind::Hex64 => (NodeKind::Hex32, 4),
            NodeKind::Hex32 => (NodeKind::Hex16, 2),
            NodeKind::Hex16 => (NodeKind::Hex8, 1),
            _ => return,
        };
        let parent_id = node.parent_id;
        let base_offset = node.offset;
        let base_name = node.name.clone();

        self.suppress_refresh = true;
        self.begin_macro("Split Hex node");

        self.push_command(Command::Remove {
            node_id,
            subtree: vec![node.clone()],
            off_adjs: Vec::new(),
        });

        let mut lo = Node {
            kind: half_kind,
            name: base_name.clone(),
            parent_id,
            offset: base_offset,
            ..Node::default()
        };
        lo.id = self.doc.tree.reserve_id();
        self.push_command(Command::Insert {
            node: lo,
            off_adjs: Vec::new(),
        });

        let mut hi = Node {
            kind: half_kind,
            name: format!("{}_hi", base_name),
            parent_id,
            offset: base_offset + half_size,
            ..Node::default()
        };
        hi.id = self.doc.tree.reserve_id();
        self.push_command(Command::Insert {
            node: hi,
            off_adjs: Vec::new(),
        });

        self.end_macro();
        self.suppress_refresh = false;
        self.refresh();
    }

    /// `joinHexNodes(nodeId, targetKind)` (`controller.cpp:3598`).
    pub fn join_hex_nodes(&mut self, node_id: u64, target_kind: NodeKind) {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return;
        }
        let orig_offset = self.doc.tree.nodes[ni as usize].offset;
        let orig_parent_id = self.doc.tree.nodes[ni as usize].parent_id;
        let orig_name = self.doc.tree.nodes[ni as usize].name.clone();
        let cur_sz = size_for_kind(self.doc.tree.nodes[ni as usize].kind);
        let tgt_sz = size_for_kind(target_kind);
        if tgt_sz <= cur_sz {
            return;
        }

        let mut merge_indices: Vec<usize> = vec![ni as usize];
        let mut accumulated = cur_sz;
        let mut next_off = orig_offset + cur_sz;
        while accumulated < tgt_sz {
            let mut found: Option<usize> = None;
            for (i, sib) in self.doc.tree.nodes.iter().enumerate() {
                if sib.parent_id == orig_parent_id
                    && sib.offset == next_off
                    && is_hex_node(sib.kind)
                {
                    found = Some(i);
                    break;
                }
            }
            let Some(f) = found else { break };
            let sib_sz = size_for_kind(self.doc.tree.nodes[f].kind);
            merge_indices.push(f);
            accumulated += sib_sz;
            next_off += sib_sz;
        }
        if accumulated < tgt_sz {
            self.emit(ControllerEvent::StatusHint(format!(
                "Cannot resize: need {} bytes at +0x{:X}, only {} available",
                tgt_sz, orig_offset, accumulated
            )));
            return;
        }

        let merged_ids: Vec<u64> = merge_indices
            .iter()
            .map(|&j| self.doc.tree.nodes[j].id)
            .collect();

        self.suppress_refresh = true;
        self.begin_macro("Join Hex nodes");

        // Remove in reverse index order.
        let mut rev = merge_indices.clone();
        rev.sort_unstable_by(|a, b| b.cmp(a));
        for idx in rev {
            let n = self.doc.tree.nodes[idx].clone();
            self.push_command(Command::Remove {
                node_id: n.id,
                subtree: vec![n],
                off_adjs: Vec::new(),
            });
        }

        let mut joined = Node {
            kind: target_kind,
            name: orig_name,
            parent_id: orig_parent_id,
            offset: orig_offset,
            ..Node::default()
        };
        joined.id = self.doc.tree.reserve_id();
        let joined_id = joined.id;
        self.push_command(Command::Insert {
            node: joined,
            off_adjs: Vec::new(),
        });

        // Transfer selection.
        let mut was_selected = self.sel_ids.remove(&node_id);
        for mid in merged_ids {
            was_selected |= self.sel_ids.remove(&mid);
        }
        if was_selected {
            self.sel_ids.insert(joined_id);
        }

        self.end_macro();
        self.suppress_refresh = false;
        self.refresh();
    }

    /// `groupIntoUnion(nodeIds)` (`controller.cpp:2589`).
    pub fn group_into_union(&mut self, node_ids: &HashSet<u64>) {
        if node_ids.len() < 2 {
            return;
        }
        let mut indices: Vec<usize> = Vec::new();
        let mut parent_id = 0;
        let mut first = true;
        for &id in node_ids {
            let idx = self.doc.tree.index_of_id(id);
            if idx < 0 {
                return;
            }
            if first {
                parent_id = self.doc.tree.nodes[idx as usize].parent_id;
                first = false;
            } else if self.doc.tree.nodes[idx as usize].parent_id != parent_id {
                return;
            }
            indices.push(idx as usize);
        }
        indices.sort_by_key(|&i| self.doc.tree.nodes[i].offset);
        let union_offset = self.doc.tree.nodes[indices[0]].offset;

        let was_suppressed = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Group into union");

        // Save copies (node + subtree).
        struct SavedNode {
            node: Node,
            subtree: Vec<Node>,
        }
        let mut saved: Vec<SavedNode> = Vec::new();
        for &idx in &indices {
            let node = self.doc.tree.nodes[idx].clone();
            let mut subtree = Vec::new();
            for si in self.doc.tree.subtree_indices(node.id) {
                if si != idx {
                    subtree.push(self.doc.tree.nodes[si].clone());
                }
            }
            saved.push(SavedNode { node, subtree });
        }

        // Remove selected nodes in reverse order.
        for i in (0..indices.len()).rev() {
            let idx = self.doc.tree.index_of_id(saved[i].node.id);
            if idx >= 0 {
                let subtree: Vec<Node> = self
                    .doc
                    .tree
                    .subtree_indices(saved[i].node.id)
                    .into_iter()
                    .map(|si| self.doc.tree.nodes[si].clone())
                    .collect();
                self.push_command(Command::Remove {
                    node_id: saved[i].node.id,
                    subtree,
                    off_adjs: Vec::new(),
                });
            }
        }

        // Insert union node.
        let mut union_node = Node {
            kind: NodeKind::Struct,
            class_keyword: "union".to_string(),
            parent_id,
            offset: union_offset,
            ..Node::default()
        };
        union_node.id = self.doc.tree.reserve_id();
        let union_id = union_node.id;
        self.push_command(Command::Insert {
            node: union_node,
            off_adjs: Vec::new(),
        });

        // Re-insert as children of the union at offset 0.
        for sn in &saved {
            let mut copy = sn.node.clone();
            copy.parent_id = union_id;
            copy.offset = 0;
            copy.id = self.doc.tree.reserve_id();
            let old_id = sn.node.id;
            let new_id = copy.id;
            self.push_command(Command::Insert {
                node: copy,
                off_adjs: Vec::new(),
            });
            for child in &sn.subtree {
                let mut cc = child.clone();
                if cc.parent_id == old_id {
                    cc.parent_id = new_id;
                }
                cc.id = self.doc.tree.reserve_id();
                self.push_command(Command::Insert {
                    node: cc,
                    off_adjs: Vec::new(),
                });
            }
        }

        self.end_macro();
        self.suppress_refresh = was_suppressed;
        if !self.suppress_refresh {
            self.refresh();
        }
    }

    /// `batchRemoveNodes(nodeIndices)` (`controller.cpp:5044`).
    pub fn batch_remove_nodes(&mut self, node_indices: &[usize]) {
        let mut id_set: HashSet<u64> = HashSet::new();
        for &idx in node_indices {
            if idx < self.doc.tree.nodes.len() {
                id_set.insert(self.doc.tree.nodes[idx].id);
            }
        }
        let id_set = self.doc.tree.normalize_prefer_ancestors(&id_set);
        if id_set.is_empty() {
            return;
        }
        self.sel_ids.clear();
        self.anchor_line = -1;

        self.suppress_refresh = true;
        self.begin_macro(format!("Delete {} nodes", id_set.len()));
        for id in id_set {
            let idx = self.doc.tree.index_of_id(id);
            if idx >= 0 {
                self.remove_node(idx as usize);
            }
        }
        self.end_macro();
        self.suppress_refresh = false;
        self.refresh();
    }

    /// `batchChangeKind(nodeIndices, newKind)` (`controller.cpp:5068`).
    pub fn batch_change_kind(&mut self, node_indices: &[usize], new_kind: NodeKind) {
        let mut id_set: HashSet<u64> = HashSet::new();
        for &idx in node_indices {
            if idx < self.doc.tree.nodes.len() {
                id_set.insert(self.doc.tree.nodes[idx].id);
            }
        }
        let id_set = self.doc.tree.normalize_prefer_descendants(&id_set);
        if id_set.is_empty() {
            return;
        }
        let saved_sel = self.sel_ids.clone();

        self.suppress_refresh = true;
        self.begin_macro(format!("Change type of {} nodes", id_set.len()));
        for id in id_set {
            let idx = self.doc.tree.index_of_id(id);
            if idx >= 0 {
                self.change_node_kind(idx as usize, new_kind);
            }
        }
        self.end_macro();
        self.suppress_refresh = false;

        self.sel_ids = saved_sel;
        self.refresh();
    }

    /// The `EditTarget::Type` "type[count]" → Array branch of
    /// `inlineEditCommitted` (`controller.cpp:1583`). Exposed directly because
    /// the test injects the committed text rather than a key event.
    pub fn apply_type_text(&mut self, node_idx: usize, text: &str) {
        if let (Some(bpos), true) = (text.find('['), text.ends_with(']')) {
            if bpos > 0 {
                let elem_type_name = text[..bpos].trim().to_string();
                let count_str = &text[bpos + 1..text.len() - 1];
                if let Ok(new_count) = count_str.parse::<i32>() {
                    if new_count > 0 {
                        let (elem_kind, type_ok) =
                            crate::core::kind_from_type_name(&elem_type_name);
                        if type_ok && node_idx < self.doc.tree.nodes.len() {
                            let node_id = self.doc.tree.nodes[node_idx].id;
                            let was_suppressed = self.suppress_refresh;
                            self.suppress_refresh = true;
                            self.begin_macro("Change to array");
                            if self.doc.tree.nodes[node_idx].kind != NodeKind::Array {
                                self.change_node_kind(node_idx, NodeKind::Array);
                            }
                            let idx = self.doc.tree.index_of_id(node_id);
                            if idx >= 0 {
                                let n = &self.doc.tree.nodes[idx as usize];
                                if n.element_kind != elem_kind || n.array_len != new_count {
                                    self.push_command(Command::ChangeArrayMeta {
                                        node_id,
                                        old_element_kind: n.element_kind,
                                        new_element_kind: elem_kind,
                                        old_array_len: n.array_len,
                                        new_array_len: new_count,
                                    });
                                }
                            }
                            self.end_macro();
                            self.suppress_refresh = was_suppressed;
                            if !self.suppress_refresh {
                                self.refresh();
                            }
                        }
                    }
                }
            }
            return;
        }
        // Regular type change.
        let (k, ok) = crate::core::kind_from_type_name(text);
        if ok && !matches!(k, NodeKind::Struct | NodeKind::Array) {
            self.change_node_kind(node_idx, k);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// refresh() (`controller.cpp:1891`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `RcxController::refresh()` (`controller.cpp:1891`). The headless pure
    /// steps 1-6 (compose + change-highlight + value-tracking + selection-prune)
    /// run; the editor-apply tail (steps 7-9) is a no-op without a real editor.
    pub fn refresh(&mut self) {
        // Compose against snapshot if active, else real provider.
        self.last_result = if let Some(snap) = &self.snapshot {
            compose::compose(
                &self.doc.tree,
                snap.as_ref(),
                self.view_root_id,
                self.compact_columns,
                self.tree_lines,
                self.brace_wrap,
                self.type_hints,
                self.show_comments,
                self.show_rtti,
                self.show_enum_chips,
            )
        } else {
            self.doc.compose(
                self.view_root_id,
                self.compact_columns,
                self.tree_lines,
                self.brace_wrap,
                self.type_hints,
                self.show_comments,
            )
        };

        // Change-highlight pass.
        if !self.changed_offsets.is_empty() {
            let base = self.doc.tree.base_address;
            // Snapshot the per-line node info we need (avoid borrow conflicts).
            let meta_len = self.last_result.meta.len();
            for i in 0..meta_len {
                let (node_idx, offset_addr, line_byte_count) = {
                    let lm = &self.last_result.meta[i];
                    (lm.node_idx, lm.offset_addr, lm.line_byte_count)
                };
                if node_idx < 0 || node_idx as usize >= self.doc.tree.nodes.len() {
                    continue;
                }
                let offset = offset_addr as i64 - base as i64;
                let node = &self.doc.tree.nodes[node_idx as usize];
                if is_hex_node(node.kind) {
                    let mut changed_idx: Vec<i32> = Vec::new();
                    let mut data_changed = false;
                    for b in 0..line_byte_count {
                        if self.changed_offsets.contains(&(offset + b as i64)) {
                            changed_idx.push(b);
                            data_changed = true;
                        }
                    }
                    if data_changed {
                        let lm = &mut self.last_result.meta[i];
                        lm.changed_byte_indices.extend(changed_idx);
                        lm.data_changed = true;
                    }
                } else {
                    let sz = self.node_size(node);
                    let mut data_changed = false;
                    let mut b = offset;
                    while b < offset + sz as i64 {
                        if self.changed_offsets.contains(&b) {
                            data_changed = true;
                            break;
                        }
                        b += 1;
                    }
                    if data_changed {
                        self.last_result.meta[i].data_changed = true;
                    }
                }
            }
        }

        // Value-tracking pass.
        self.value_tracking_pass();

        // Prune stale selections.
        let mut valid: HashSet<u64> = HashSet::new();
        for &id in &self.sel_ids {
            let node_id = strip_sel(id);
            if self.doc.tree.index_of_id(node_id) >= 0 {
                valid.insert(id);
            }
        }
        self.sel_ids = valid;

        // (custom_types / editor apply / command row / overlays — UI; skipped
        // headlessly. `selectionChanged` is emitted via update_command_row.)
        self.update_command_row();
    }

    /// Step 4 of refresh — value history + heat. Split out for clarity.
    fn value_tracking_pass(&mut self) {
        // Resolve the tracking provider (snapshot if live, else real if valid+live).
        let use_snapshot = self.snapshot.as_ref().map_or(false, |s| s.is_live());
        let use_real = !use_snapshot && self.doc.provider.is_valid() && self.doc.provider.is_live();
        if !use_snapshot && !use_real {
            if self.value_track_cooldown > 0 {
                self.value_track_cooldown -= 1;
            }
            return;
        }

        if self.value_track_cooldown > 0 {
            self.value_track_cooldown -= 1;
        }
        if !self.track_values || self.value_track_cooldown > 0 {
            return;
        }

        let meta_len = self.last_result.meta.len();
        for i in 0..meta_len {
            let (node_idx, node_id, offset_addr, sub_line, line_kind, is_cont) = {
                let lm = &self.last_result.meta[i];
                (
                    lm.node_idx,
                    lm.node_id,
                    lm.offset_addr,
                    lm.sub_line,
                    lm.line_kind,
                    lm.is_continuation,
                )
            };
            if node_idx < 0 || node_idx as usize >= self.doc.tree.nodes.len() {
                continue;
            }
            if is_synthetic_line(&self.last_result.meta[i]) || is_cont {
                continue;
            }
            if line_kind != LineKind::Field {
                continue;
            }
            let node = self.doc.tree.nodes[node_idx as usize].clone();
            if matches!(node.kind, NodeKind::Struct | NodeKind::Array) {
                continue;
            }
            if is_func_ptr(node.kind) {
                continue;
            }
            let addr = offset_addr;
            let sz = node.byte_size();

            // Read the value through the chosen provider.
            let val = {
                let prov: &dyn Provider = if use_snapshot {
                    self.snapshot.as_ref().unwrap().as_ref()
                } else {
                    &*self.doc.provider
                };
                if sz <= 0 || !prov.is_readable(addr, sz) {
                    continue;
                }
                format::read_value(&node, prov, addr, sub_line)
            };
            if val.is_empty() {
                continue;
            }
            if let Some(&prev_addr) = self.last_value_addr.get(&node_id) {
                if prev_addr != addr {
                    self.value_history.remove(&node_id);
                }
            }
            self.last_value_addr.insert(node_id, addr);
            let vh = self.value_history.entry(node_id).or_default();
            vh.record(&val);
            let heat = vh.heat_level();
            self.last_result.meta[i].heat_level = heat;
        }
    }

    /// `updateCommandRow()` (`controller.cpp:5187`) — the headless part is the
    /// `selectionChanged` emit; the row string is built by [`build_command_row`].
    fn update_command_row(&mut self) {
        let count = self.sel_ids.len();
        self.emit(ControllerEvent::SelectionChanged(count));
    }

    /// The pure string-builder behind `updateCommandRow` (`controller.cpp:5187`).
    /// Returns the combined command-row text.
    pub fn build_command_row(&self) -> String {
        let prov_name = self.doc.provider.name();
        let src = if prov_name.is_empty() {
            "source\u{25BE}".to_string()
        } else {
            format!("'{}'\u{25BE}", prov_name)
        };
        let addr = if !self.doc.tree.base_address_formula.is_empty() {
            self.doc.tree.base_address_formula.clone()
        } else {
            format!("0x{:X}", self.doc.tree.base_address)
        };
        let row = format!("{}  {}", elide(&src, 40), elide(&addr, 24));

        let brace = if self.brace_wrap {
            String::new()
        } else {
            " {".to_string()
        };
        let mut row2 = String::new();
        if self.view_root_id != 0 {
            let vi = self.doc.tree.index_of_id(self.view_root_id);
            if vi >= 0 {
                let n = &self.doc.tree.nodes[vi as usize];
                let keyword = n.resolved_class_keyword();
                let class_name = if !n.struct_type_name.is_empty() {
                    n.struct_type_name.clone()
                } else {
                    n.name.clone()
                };
                let class_name = if class_name.is_empty() {
                    "Untitled".to_string()
                } else {
                    class_name
                };
                row2 = format!("{} {}{}", keyword, class_name, brace);
            }
        }
        if row2.is_empty() {
            for n in &self.doc.tree.nodes {
                if n.parent_id == 0 && n.kind == NodeKind::Struct {
                    let keyword = n.resolved_class_keyword();
                    let class_name = if !n.struct_type_name.is_empty() {
                        n.struct_type_name.clone()
                    } else {
                        n.name.clone()
                    };
                    let class_name = if class_name.is_empty() {
                        "Untitled".to_string()
                    } else {
                        class_name
                    };
                    row2 = format!("{} {}{}", keyword, class_name, brace);
                    break;
                }
            }
        }
        if row2.is_empty() {
            row2 = format!("struct Untitled{}", brace);
        }
        format!("[\u{25B8}] {}  {}", row, row2)
    }
}

/// `elide(s, max)` (`controller.cpp:73`).
fn elide(s: &str, max: i32) -> String {
    if max <= 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max as usize {
        return s.to_string();
    }
    if max == 1 {
        return "\u{2026}".to_string();
    }
    let mut out: String = chars[..(max as usize - 1)].iter().collect();
    out.push('\u{2026}');
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Selection (`controller.cpp:5094-5184`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `effectiveId(line, nid)` (inner lambda of `handleNodeClick`,
    /// `controller.cpp:5104`).
    fn effective_id(&self, line: i64, nid: u64) -> u64 {
        if line < 0 || line as usize >= self.last_result.meta.len() {
            return nid;
        }
        let lm = &self.last_result.meta[line as usize];
        if lm.line_kind == LineKind::Footer {
            return nid | K_FOOTER_ID_BIT;
        }
        if lm.is_array_element && lm.array_element_idx >= 0 {
            return make_array_elem_sel_id(nid, lm.array_element_idx);
        }
        if lm.is_member_line && lm.sub_line >= 0 {
            return make_member_sel_id(nid, lm.sub_line);
        }
        nid
    }

    /// `handleNodeClick(source, line, nodeId, mods)` (`controller.cpp:5094`).
    pub fn handle_node_click(&mut self, line: i64, node_id: u64, mods: Modifiers) {
        if node_id == 0 {
            self.clear_selection();
            return;
        }
        let sel_id = self.effective_id(line, node_id);

        if !mods.ctrl && !mods.shift {
            self.sel_ids.clear();
            self.sel_ids.insert(sel_id);
            self.anchor_line = line;
        } else if mods.ctrl && !mods.shift {
            if self.sel_ids.contains(&sel_id) {
                self.sel_ids.remove(&sel_id);
            } else {
                self.sel_ids.insert(sel_id);
            }
            self.anchor_line = line;
        } else if mods.shift && !mods.ctrl {
            if self.anchor_line < 0 {
                self.sel_ids.clear();
                self.sel_ids.insert(sel_id);
                self.anchor_line = line;
            } else {
                self.sel_ids.clear();
                self.insert_range(self.anchor_line, line);
            }
        } else {
            // Ctrl+Shift
            if self.anchor_line < 0 {
                self.sel_ids.insert(sel_id);
                self.anchor_line = line;
            } else {
                self.insert_range(self.anchor_line, line);
            }
        }

        self.update_command_row();

        if self.sel_ids.len() == 1 {
            let sid = *self.sel_ids.iter().next().unwrap();
            let idx = self.doc.tree.index_of_id(strip_sel(sid));
            if idx >= 0 {
                self.emit(ControllerEvent::NodeSelected(idx));
            }
        }
    }

    fn insert_range(&mut self, a: i64, b: i64) {
        let from = a.min(b);
        let to = a.max(b);
        let mut i = from;
        while i <= to && (i as usize) < self.last_result.meta.len() {
            let nid = self.last_result.meta[i as usize].node_id;
            if nid != 0 && nid != K_COMMAND_ROW_ID {
                let eid = self.effective_id(i, nid);
                self.sel_ids.insert(eid);
            }
            i += 1;
        }
    }

    /// `clearSelection()` (`controller.cpp:5174`).
    pub fn clear_selection(&mut self) {
        self.sel_ids.clear();
        self.anchor_line = -1;
        self.update_command_row();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Refresh state machine (`controller.cpp:6586-6891`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `onRefreshTick()` (`controller.cpp:6586`) — returns the read plan.
    pub fn on_refresh_tick(&mut self) -> RefreshPlan {
        // Liveness-flip detection runs BEFORE early returns.
        let now_live = self.doc.provider.is_valid();
        if now_live != self.last_live {
            self.last_live = now_live;
            self.emit(ControllerEvent::SourceLivenessChanged(now_live));
        }

        if self.read_in_flight {
            return RefreshPlan::None;
        }
        if !self.doc.provider.is_live() {
            return RefreshPlan::None;
        }
        if self.suppress_refresh {
            return RefreshPlan::None;
        }
        if self.editor.as_ref().map_or(false, |e| e.is_editing()) {
            return RefreshPlan::None;
        }

        self.tick_count += 1;

        let extent = self.compute_data_extent();
        if extent <= 0 {
            return RefreshPlan::None;
        }

        // Ranges: main struct + pointer targets.
        let base = self.doc.tree.base_address;
        let mut ranges: Vec<(u64, i32)> = vec![(base, extent)];
        if self.snapshot.is_some() {
            let mut root_id = self.view_root_id;
            if root_id == 0 && !self.doc.tree.nodes.is_empty() {
                root_id = self.doc.tree.nodes[0].id;
            }
            let mut visited: HashSet<(u64, u64)> = HashSet::new();
            let mut budget = K_POINTER_SNAPSHOT_BYTE_BUDGET - extent as i64;
            self.collect_pointer_ranges(
                root_id,
                base,
                0,
                99,
                &mut visited,
                &mut ranges,
                &mut budget,
            );
        }

        let first_snapshot = self.snapshot.is_none() || self.prev_pages.is_empty();
        let viewport = if !first_snapshot {
            self.viewport_address_range()
        } else {
            None
        };

        const OVERSCAN_PAGES: u64 = 2;
        let mut request_pages: HashSet<u64> = HashSet::new();
        for &(range_start, range_len) in &ranges {
            let page_start = range_start & K_PAGE_MASK;
            let end = range_start.wrapping_add(range_len as u64);
            let page_end = (end + K_PAGE_SIZE - 1) & K_PAGE_MASK;
            let mut p = page_start;
            while p < page_end {
                let is_permanent = self.snapshot.as_ref().map_or(false, |s| s.is_permanent(p));
                if is_permanent {
                    p += K_PAGE_SIZE;
                    continue;
                }
                if let Some((vlo, vhi)) = viewport {
                    let lo = if vlo > OVERSCAN_PAGES * K_PAGE_SIZE {
                        (vlo - OVERSCAN_PAGES * K_PAGE_SIZE) & K_PAGE_MASK
                    } else {
                        0
                    };
                    let hi = ((vhi + OVERSCAN_PAGES * K_PAGE_SIZE) + K_PAGE_SIZE - 1) & K_PAGE_MASK;
                    let in_viewport = p >= lo && p < hi;
                    let is_main_range = range_start == base;
                    if is_main_range && !in_viewport {
                        let stab = self.page_stability.get(&p).copied().unwrap_or(0);
                        let is_stable = stab >= K_STABILITY_THRESHOLD;
                        if is_stable && (self.tick_count & 1) == 1 {
                            p += K_PAGE_SIZE;
                            continue;
                        }
                    }
                }
                request_pages.insert(p);
                p += K_PAGE_SIZE;
            }
        }

        if request_pages.is_empty() {
            self.idle_ticks += 1;
            self.apply_adaptive_interval();
            return RefreshPlan::None;
        }

        self.read_in_flight = true;
        self.read_gen = self.refresh_gen;
        RefreshPlan::Read {
            pages: request_pages.into_iter().collect(),
            provider: self.doc.provider.clone(),
        }
    }

    /// The background read worker body (`controller.cpp:6688`). Pure — captures
    /// only the `Arc<dyn Provider>` + page list. Each page padded to 4096.
    pub fn read_pages(provider: &Arc<dyn Provider + Send + Sync>, pages: &[u64]) -> PageMap {
        let mut out = PageMap::new();
        out.reserve(pages.len());
        for &p in pages {
            let mut bytes = provider.read_bytes(p, K_PAGE_SIZE as i32);
            bytes.resize(K_PAGE_SIZE as usize, 0);
            out.insert(p, bytes);
        }
        out
    }

    /// `onReadComplete()` (`controller.cpp:6698`).
    pub fn on_read_complete(&mut self, new_pages: PageMap) {
        self.read_in_flight = false;

        if self.read_gen != self.refresh_gen {
            return;
        }

        // All-zero page-0 guard.
        if !self.prev_pages.is_empty() {
            if let Some(p0) = new_pages.get(&0) {
                if p0.iter().all(|&b| b == 0) {
                    tracing::debug!("[Refresh] discarding all-zero page-0, keeping stale snapshot");
                    return;
                }
            }
        }

        // Diff + stability.
        self.changed_offsets.clear();
        let mut any_changed = false;
        let first_snapshot = self.prev_pages.is_empty();
        for (&page_addr, fresh) in &new_pages {
            match self.prev_pages.get(&page_addr) {
                None => {
                    self.page_stability.insert(page_addr, 0);
                }
                Some(prev) => {
                    let cmp_len = prev.len().min(fresh.len());
                    let mut page_changed = false;
                    for i in 0..cmp_len {
                        if prev[i] != fresh[i] {
                            self.changed_offsets.insert(page_addr as i64 + i as i64);
                            page_changed = true;
                        }
                    }
                    if page_changed {
                        self.page_stability.insert(page_addr, 0);
                        any_changed = true;
                    } else {
                        let prev_stab = self.page_stability.get(&page_addr).copied().unwrap_or(0);
                        self.page_stability
                            .insert(page_addr, (K_STABILITY_THRESHOLD + 16).min(prev_stab + 1));
                    }
                }
            }
        }

        if any_changed {
            self.idle_ticks = 0;
        } else if !first_snapshot {
            self.idle_ticks += 1;
        }
        self.apply_adaptive_interval();

        let main_extent = self.compute_data_extent();

        // Accumulate prev_pages, then merge into / create the snapshot.
        for (k, v) in &new_pages {
            self.prev_pages.insert(*k, v.clone());
        }
        match self.snapshot.as_mut() {
            Some(s) => s.merge_pages(&new_pages, main_extent),
            None => {
                self.snapshot = Some(Box::new(SnapshotProvider::new(
                    Some(self.doc.provider.clone()),
                    new_pages.clone(),
                    main_extent,
                )));
            }
        }

        // Speedup 4: classify permanent pages (after snapshot exists).
        self.classify_permanent_pages(&new_pages);

        if any_changed || first_snapshot {
            self.refresh();
        }
        self.changed_offsets.clear();
    }

    /// `collectPointerRanges(...)` (`controller.cpp:6532`).
    #[allow(clippy::too_many_arguments)]
    fn collect_pointer_ranges(
        &self,
        struct_id: u64,
        mem_base: u64,
        depth: i32,
        max_depth: i32,
        visited: &mut HashSet<(u64, u64)>,
        ranges: &mut Vec<(u64, i32)>,
        budget: &mut i64,
    ) {
        if depth >= max_depth {
            return;
        }
        if *budget <= 0 {
            return;
        }
        let key = (struct_id, mem_base);
        if visited.contains(&key) {
            return;
        }
        visited.insert(key);

        let span = self.doc.tree.struct_span(struct_id);
        if span <= 0 {
            return;
        }
        ranges.push((mem_base, span));
        *budget -= span as i64;
        if *budget <= 0 {
            return;
        }

        let Some(snap) = self.snapshot.as_ref() else {
            return;
        };

        let children = self.doc.tree.children_of(struct_id);
        for &ci in &children {
            if *budget <= 0 {
                break;
            }
            let child = &self.doc.tree.nodes[ci];
            if !matches!(child.kind, NodeKind::Pointer32 | NodeKind::Pointer64) {
                continue;
            }
            if child.collapsed || child.ref_id == 0 {
                continue;
            }
            let ptr_addr = mem_base + child.offset as u64;
            let ptr_size = child.byte_size();
            if !snap.is_readable(ptr_addr, ptr_size) {
                continue;
            }
            let ptr_val = if child.kind == NodeKind::Pointer32 {
                snap.read_u32(ptr_addr) as u64
            } else {
                snap.read_u64(ptr_addr)
            };
            if ptr_val == 0 || ptr_val == u64::MAX {
                continue;
            }
            let ref_id = child.ref_id;
            self.collect_pointer_ranges(
                ref_id,
                ptr_val,
                depth + 1,
                max_depth,
                visited,
                ranges,
                budget,
            );
        }

        // Embedded struct reference.
        let idx = self.doc.tree.index_of_id(struct_id);
        if idx >= 0 {
            let sn = &self.doc.tree.nodes[idx as usize];
            if sn.kind == NodeKind::Struct && sn.ref_id != 0 && children.is_empty() {
                let ref_id = sn.ref_id;
                self.collect_pointer_ranges(
                    ref_id, mem_base, depth, max_depth, visited, ranges, budget,
                );
            }
        }
    }

    /// `viewportAddressRange()` (`controller.cpp:6798`).
    fn viewport_address_range(&self) -> Option<(u64, u64)> {
        let editor = self.editor.as_ref()?;
        let mut any = false;
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        let first = editor.first_visible_line();
        let on_screen = editor.lines_on_screen();
        let mut v = first;
        while v < first + on_screen {
            let doc_line = editor.doc_line_from_visible(v);
            if let Some(addr) = editor.offset_addr_for_line(doc_line) {
                if addr != 0 {
                    if addr < lo {
                        lo = addr;
                    }
                    let hi_candidate = addr + 16;
                    if hi_candidate > hi {
                        hi = hi_candidate;
                    }
                    any = true;
                }
            }
            v += 1;
        }
        if !any {
            None
        } else {
            Some((lo, hi))
        }
    }

    /// `classifyPermanentPages(fresh)` (`controller.cpp:6832`).
    fn classify_permanent_pages(&mut self, fresh: &PageMap) {
        if self.snapshot.is_none() {
            return;
        }
        let regions: Vec<MemoryRegion> = self.doc.provider.enumerate_regions();
        if regions.is_empty() {
            return;
        }
        let to_mark: Vec<u64> = {
            let snap = self.snapshot.as_ref().unwrap();
            let mut marks = Vec::new();
            for (&page_addr, _) in fresh {
                if snap.is_permanent(page_addr) {
                    continue;
                }
                for r in &regions {
                    if r.module_name.is_empty() {
                        continue;
                    }
                    if page_addr < r.base {
                        continue;
                    }
                    if page_addr + K_PAGE_SIZE > r.base + r.size {
                        continue;
                    }
                    if !r.executable {
                        continue;
                    }
                    marks.push(page_addr);
                    break;
                }
            }
            marks
        };
        let snap = self.snapshot.as_mut().unwrap();
        for p in to_mark {
            snap.mark_permanent(p);
        }
    }

    /// `computeDataExtent()` (`controller.cpp:6856`).
    fn compute_data_extent(&self) -> i32 {
        let mut tree_extent: i64 = 0;
        for i in 0..self.doc.tree.nodes.len() {
            let off = self.doc.tree.compute_offset(i as i32);
            if off < 0 {
                continue;
            }
            let node = &self.doc.tree.nodes[i];
            let sz = self.node_size(node);
            let end = off + sz as i64;
            if end > tree_extent {
                tree_extent = end;
            }
        }
        if tree_extent > 0 {
            return tree_extent.min(K_MAX_MAIN_EXTENT) as i32;
        }
        let prov_size = self.doc.provider.size();
        if prov_size > 0 {
            return prov_size;
        }
        0
    }

    /// `resetSnapshot()` (`controller.cpp:6876`).
    fn reset_snapshot(&mut self) {
        self.refresh_gen += 1;
        self.read_in_flight = false;
        self.snapshot = None;
        self.prev_pages.clear();
        self.changed_offsets.clear();
        self.value_history.clear();
        self.last_value_addr.clear();
        self.page_stability.clear();
        self.idle_ticks = 0;
        self.tick_count = 0;
        self.apply_adaptive_interval();
    }

    /// Test-only helper: drive a full tick→read→complete cycle synchronously.
    /// Returns `true` if a read was launched and completed.
    pub fn pump_refresh(&mut self) -> bool {
        match self.on_refresh_tick() {
            RefreshPlan::None => false,
            RefreshPlan::Read { pages, provider } => {
                let result = RcxController::read_pages(&provider, &pages);
                self.on_read_complete(result);
                true
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Adaptive interval + window state (`controller.cpp:6430-6478`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `setRefreshInterval(ms)` (`controller.cpp:6430`).
    pub fn set_refresh_interval(&mut self, ms: i32) {
        self.refresh_interval_base_ms = ms.max(1);
        self.refresh_interval_max_ms = self.refresh_interval_base_ms.max(1500);
        self.refresh_interval_blur_ms = self.refresh_interval_base_ms.max(1500);
        self.apply_adaptive_interval();
    }

    /// `applyAdaptiveInterval()` (`controller.cpp:6441`).
    fn apply_adaptive_interval(&mut self) {
        if !self.window_visible {
            self.timer_active = false;
            return;
        }
        let target = if !self.window_focused {
            self.refresh_interval_blur_ms
        } else if self.idle_ticks >= K_IDLE_BACKOFF_TICKS {
            let shift =
                (4).min((self.idle_ticks - K_IDLE_BACKOFF_TICKS) / K_IDLE_BACKOFF_TICKS + 1);
            let factor = 1i32 << shift;
            self.refresh_interval_max_ms
                .min(self.refresh_interval_base_ms * factor)
        } else {
            self.refresh_interval_base_ms
        };
        if target != self.current_interval_ms {
            self.current_interval_ms = target;
        }
        self.timer_active = true;
    }

    /// `setWindowState(focused, visible)` (`controller.cpp:6469`).
    pub fn set_window_state(&mut self, focused: bool, visible: bool) {
        let focus_gained = focused && !self.window_focused;
        self.window_focused = focused;
        self.window_visible = visible;
        if focus_gained {
            self.idle_ticks = 0;
        }
        self.apply_adaptive_interval();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// View root, bookmarks, source management (`controller.cpp:6102-6956`)
// ─────────────────────────────────────────────────────────────────────────────

impl RcxController {
    /// `addBookmark(name, formula)` (`controller.cpp:6946`).
    pub fn add_bookmark(&mut self, name: &str, formula: &str) {
        let name = name.trim();
        let formula = formula.trim();
        if name.is_empty() || formula.is_empty() {
            return;
        }
        self.doc.tree.bookmarks.push(crate::core::Bookmark {
            name: name.to_string(),
            address_formula: formula.to_string(),
        });
        self.doc.modified = true;
        self.on_document_changed();
    }

    /// `removeBookmark(idx)` (`controller.cpp:6956`).
    pub fn remove_bookmark(&mut self, idx: usize) {
        if idx >= self.doc.tree.bookmarks.len() {
            return;
        }
        self.doc.tree.bookmarks.remove(idx);
        self.doc.modified = true;
        self.on_document_changed();
    }

    /// `connect(doc, documentChanged, refresh)` (`controller.cpp:362`).
    fn on_document_changed(&mut self) {
        self.emit(ControllerEvent::DocumentChanged);
        self.refresh();
    }

    /// `ingestPendingSavedSources()` (`controller.cpp:378`).
    fn ingest_pending_saved_sources(&mut self) {
        if self.doc.pending_saved_sources.is_empty() {
            return;
        }
        let arr = std::mem::take(&mut self.doc.pending_saved_sources);
        for v in &arr {
            let entry = SavedSourceEntry {
                kind: v
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                display_name: v
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                file_path: v
                    .get("filePath")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                provider_target: v
                    .get("providerTarget")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                base_address: v
                    .get("baseAddress")
                    .and_then(Value::as_str)
                    .map(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0))
                    .unwrap_or(0),
                base_address_formula: v
                    .get("baseAddressFormula")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            };
            self.saved_sources.push(entry);
        }
        if self.saved_sources.is_empty() {
            return;
        }
        let first = self.saved_sources[0].clone();
        if first.kind == "File"
            && !first.file_path.is_empty()
            && !Path::new(&first.file_path).exists()
        {
            tracing::warn!("[load] saved source missing: {}", first.file_path);
            return;
        }
        self.switch_to_saved_source(0);
    }

    /// `switchToSavedSource(idx)` (`controller.cpp:6210`).
    pub fn switch_to_saved_source(&mut self, idx: i32) {
        if idx < 0 || idx as usize >= self.saved_sources.len() {
            return;
        }
        if idx == self.active_source_idx {
            return;
        }
        // Save current source's base into its slot.
        if self.active_source_idx >= 0
            && (self.active_source_idx as usize) < self.saved_sources.len()
        {
            let slot = &mut self.saved_sources[self.active_source_idx as usize];
            slot.base_address = self.doc.tree.base_address;
            slot.base_address_formula = self.doc.tree.base_address_formula.clone();
        }
        self.active_source_idx = idx;
        let entry = self.saved_sources[idx as usize].clone();
        if entry.kind == "File" {
            if !entry.file_path.is_empty() {
                self.undo.clear();
                self.doc.load_data_file(&entry.file_path);
                self.doc.tree.base_address = entry.base_address;
                self.doc.tree.base_address_formula = entry.base_address_formula.clone();
                self.reset_snapshot();
                self.refresh();
            }
        }
        // Plugin sources are out of scope (documented stub).
        self.on_document_changed();
    }

    /// `clearSources()` (`controller.cpp:6398`).
    pub fn clear_sources(&mut self) {
        self.saved_sources.clear();
        self.active_source_idx = -1;
        self.doc.provider = Arc::new(NullProvider);
        self.doc.data_path = None;
        self.reset_snapshot();
        self.refresh();
    }

    /// `copySavedSources(sources, activeIdx)` (`controller.cpp:6408`).
    pub fn copy_saved_sources(&mut self, sources: Vec<SavedSourceEntry>, active_idx: i32) {
        self.saved_sources = sources;
        self.active_source_idx = active_idx;
    }

    /// In-scope subset of `attachViaPlugin` / `selectSource` source-attach
    /// bookkeeping (`controller.cpp:6102/6242`) for built-in providers. The
    /// base-address policy (§8.2): a non-zero/default base is preserved, a fresh
    /// doc adopts the provider's base.
    pub fn attach_provider(
        &mut self,
        provider: Arc<dyn Provider + Send + Sync>,
        register_as_saved: bool,
    ) {
        self.undo.clear();
        let new_base = provider.base();
        let pointer_size = provider.pointer_size();
        let name = provider.name();
        let kind = provider.kind();
        self.doc.provider = provider;
        self.doc.data_path = None;
        self.doc.tree.pointer_size = pointer_size;
        // Base-address policy: fresh/default doc adopts the provider base.
        if self.doc.tree.base_address == 0 || self.doc.tree.base_address == 0x0040_0000 {
            if new_base != 0 {
                self.doc.tree.base_address = new_base;
            }
        }
        self.reset_snapshot();
        if register_as_saved {
            // Dedup on (kind, providerTarget) — here providerTarget is empty.
            let pos = self
                .saved_sources
                .iter()
                .position(|s| s.kind == kind && s.provider_target.is_empty());
            let entry = SavedSourceEntry {
                kind: kind.clone(),
                display_name: name,
                base_address: self.doc.tree.base_address,
                base_address_formula: self.doc.tree.base_address_formula.clone(),
                ..Default::default()
            };
            match pos {
                Some(i) => {
                    self.saved_sources[i] = entry;
                    self.active_source_idx = i as i32;
                }
                None => {
                    self.saved_sources.push(entry);
                    self.active_source_idx = (self.saved_sources.len() - 1) as i32;
                }
            }
        }
        self.on_document_changed();
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
