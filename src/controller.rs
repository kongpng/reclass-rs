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
    alignment_for, find_common_type, is_container_kind, is_func_ptr, is_hex_node,
    is_valid_primitive_ptr_target, kind_from_string, kind_meta, kind_to_string, size_for_kind,
    BitfieldMember, Command, ComposeResult, LineKind, Node, NodeKind, NodeTree, OffsetAdj,
    ValueHistory, K_COMMON_TYPES,
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

/// Public wrapper over [`strip_sel`] for the UI: recover the bare node id from a
/// selection id (masking off the footer / array-element / member tag + sub bits).
/// The editor surface needs this to map a `selected_ids()` entry back to the node
/// id of a rendered line (editor-surface.md §7 `applySelectionOverlay`); the strip
/// math itself stays private to the controller.
#[inline]
pub fn strip_sel_pub(id: u64) -> u64 {
    strip_sel(id)
}

const K_PAGE_MASK: u64 = !(K_PAGE_SIZE - 1);
const K_STABILITY_THRESHOLD: i32 = 5;
const K_IDLE_BACKOFF_TICKS: i32 = 8;
const K_POINTER_SNAPSHOT_BYTE_BUDGET: i64 = 64 * 1024 * 1024;
const K_MAX_MAIN_EXTENT: i64 = 16 * 1024 * 1024;

/// `using PageMap = QHash<uint64_t, QByteArray>` (`controller.h:312`). Each
/// value is a 4096-byte page (padded/truncated by the read worker).
pub use crate::provider::PageMap;

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
// Type-selector popup application (`controller.cpp:4826` applyTypePopupResult)
// ─────────────────────────────────────────────────────────────────────────────

/// `enum class TypePopupMode` (`typeselectorpopup.h:26`) — what the popup is
/// picking. Re-declared here (headless) so the controller's
/// [`apply_type_popup_result`](RcxController::apply_type_popup_result) does not
/// depend on the GPUI `ui` layer; the editor maps its own `TypePopupMode` onto
/// this 1:1 before calling.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TypePopupMode {
    /// Picking a top-level type at the root (sets the view-root id).
    #[default]
    Root,
    /// Picking a field type (modifiers `*`/`**`/`[]` allowed).
    FieldType,
    /// Picking an array element type (modifiers allowed).
    ArrayElement,
    /// Picking a pointer target (no modifiers).
    PointerTarget,
}

/// The kind of a [`TypePopupChoice`] (`TypeEntry::EntryKind`,
/// `typeselectorpopup.h:30`). Section headers are never delivered as a choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeEntryKind {
    /// A built-in primitive kind.
    Primitive,
    /// A user struct/class/enum, or a built-in/cross-document composite.
    Composite,
}

/// The chosen entry from the type-selector popup (`struct TypeEntry`, condensed
/// to the fields `applyTypePopupResult` reads, plus the resolved modifier text).
///
/// `struct_id == 0` on a [`TypeEntryKind::Composite`] means the type comes from
/// the built-in [`K_COMMON_TYPES`] library or another document — it is imported
/// on demand via [`find_or_create_struct_by_name`](RcxController::find_or_create_struct_by_name).
#[derive(Clone, Debug, PartialEq)]
pub struct TypePopupChoice {
    pub entry_kind: TypeEntryKind,
    /// For [`TypeEntryKind::Primitive`]: the kind it represents.
    pub primitive_kind: NodeKind,
    /// For [`TypeEntryKind::Composite`]: the struct/enum id (0 ⇒ import by name).
    pub struct_id: u64,
    /// The display name (e.g. "int32_t", "Player", "UNICODE_STRING").
    pub display_name: String,
    /// The full type text including any modifier suffix (`Ball*`, `int32_t[10]`).
    /// Empty ⇒ derived from `display_name` (no modifier).
    pub full_text: String,
    /// "+ New" marker (popup item 15): materialize a fresh named composite rather
    /// than the existing generic `Struct` primitive.
    pub create_new: bool,
}

impl TypePopupChoice {
    /// A plain primitive choice with no modifier.
    pub fn primitive(kind: NodeKind, display_name: impl Into<String>) -> Self {
        TypePopupChoice {
            entry_kind: TypeEntryKind::Primitive,
            primitive_kind: kind,
            struct_id: 0,
            display_name: display_name.into(),
            full_text: String::new(),
            create_new: false,
        }
    }
    /// A composite choice referencing an existing struct id.
    pub fn composite(struct_id: u64, display_name: impl Into<String>) -> Self {
        TypePopupChoice {
            entry_kind: TypeEntryKind::Composite,
            primitive_kind: NodeKind::Struct,
            struct_id,
            display_name: display_name.into(),
            full_text: String::new(),
            create_new: false,
        }
    }
}

/// `struct TypeSpec` (`typeselectorpopup.h:59`) — a parsed type text.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TypeSpec {
    pub base_name: String,
    pub is_pointer: bool,
    /// 1 for `*`, 2 for `**`, 0 if not a pointer.
    pub ptr_depth: i32,
    /// Array element count (0 = not an array).
    pub array_count: i32,
}

/// `parseTypeSpec(text)` (`typeselectorpopup.cpp:113-142`) — split a type text
/// into its base name + optional `*`/`**`/`[N]` modifier.
pub fn parse_type_spec(text: &str) -> TypeSpec {
    let mut spec = TypeSpec::default();
    let s = text.trim();
    if s.is_empty() {
        return spec;
    }
    // Pointer suffix: "Ball*" / "Ball**".
    if let Some(stripped) = s.strip_suffix('*') {
        spec.is_pointer = true;
        spec.ptr_depth = 1;
        let stripped = if let Some(s2) = stripped.strip_suffix('*') {
            spec.ptr_depth = 2;
            s2
        } else {
            stripped
        };
        spec.base_name = stripped.trim().to_string();
        return spec;
    }
    // Array suffix: "int32_t[10]".
    if let Some(bracket) = s.find('[') {
        if bracket > 0 && s.ends_with(']') {
            spec.base_name = s[..bracket].trim().to_string();
            let count_str = &s[bracket + 1..s.len() - 1];
            if let Ok(count) = count_str.trim().parse::<i32>() {
                if count > 0 {
                    spec.array_count = count;
                }
            }
            return spec;
        }
    }
    spec.base_name = s.to_string();
    spec
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
    ///
    /// `symbol_lookup` is the trailing optional PDB symbol-lookup callback
    /// (`SymbolLookupFn symbolLookup = {}`, `controller.h:75`); when `Some`,
    /// hex/pointer rows with no user comment are annotated with `// module!symbol`.
    /// `RcxController::refresh` wires it from the global `SymbolStore`; the other
    /// callers (hover popup, tests) pass `None`, matching the C++ default arg.
    pub fn compose(
        &self,
        view_root_id: u64,
        compact_columns: bool,
        tree_lines: bool,
        brace_wrap: bool,
        type_hints: bool,
        show_comments: bool,
        symbol_lookup: compose::SymbolLookupFn<'_>,
    ) -> ComposeResult {
        compose::compose_with_symbols(
            &self.tree,
            &*self.provider,
            view_root_id,
            compact_columns,
            tree_lines,
            brace_wrap,
            type_hints,
            show_comments,
            symbol_lookup,
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
        // Byte-exact `QJsonDocument::toJson(QJsonDocument::Indented)`
        // (`controller.cpp:201`): 4-space indent + a trailing newline. serde's
        // `to_string_pretty` emits a 2-space indent and no trailing '\n', which
        // would defeat byte-level round-trip checks and produce noisy diffs.
        let mut buf = Vec::new();
        {
            use serde::Serialize;
            let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
            let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
            if json.serialize(&mut ser).is_err() {
                return false;
            }
        }
        buf.push(b'\n');
        if std::fs::write(path, &buf).is_err() {
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

    /// Adjust `clean_index` after an obsolete entry was removed from
    /// `entries` at array position `removed`.
    ///
    /// Mirrors `QUndoStack`'s `setObsolete(true)` handling: when a command is
    /// dropped, the saved-state baseline is re-indexed against the shortened
    /// list. `clean_index` is a *count* of applied entries (0..=entries.len),
    /// so after the removal (with `entries` already shortened):
    ///   * if it now points past the new length → unreachable → `None`;
    ///   * else if it referenced the removed slot or anything above it
    ///     (`ci > removed`) → it shifts down by one (`ci - 1`);
    ///   * otherwise (`ci <= removed`) it is unaffected.
    /// This matches the `clean_index > entries.len()` clamp on the push path
    /// (the truncate branch of `push_command`/`end_macro`).
    fn adjust_clean_index_after_drop(&mut self, removed: usize) {
        if let Some(ci) = self.clean_index {
            if ci > self.entries.len() {
                self.clean_index = None;
            } else if ci > removed {
                self.clean_index = Some(ci - 1);
            }
        }
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
    /// The active data source (`ctrl->document()->provider`,
    /// `main.cpp:1530`/`4397`). Always valid — defaults to [`NullProvider`], never
    /// null — so the Tools ▸ RTTI Browser gate can hand the provider straight to
    /// [`resolve_field_vtable`](crate::rtti::browser::resolve_field_vtable) /
    /// [`resolve_rtti`](crate::rtti::browser::resolve_rtti).
    pub fn provider(&self) -> &Arc<dyn Provider + Send + Sync> {
        &self.doc.provider
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
    /// The current multi-selection anchor line (the origin a Shift-range extends
    /// from), or `-1` when there is no anchor. Read by the editor's Shift+Down
    /// grow so it can re-extend the range to the freshly-appended tail row.
    pub fn anchor_line(&self) -> i64 {
        self.anchor_line
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
            let removed = self.undo.index - 1;
            self.undo.entries.remove(removed);
            // index stays pointing one slot lower (the dropped entry is gone).
            self.undo.index -= 1;
            // Re-index the clean baseline against the shortened list, just like
            // QUndoStack does when it deletes an obsolete command.
            self.undo.adjust_clean_index_after_drop(removed);
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
            let removed = self.undo.index;
            self.undo.entries.remove(removed);
            // index unchanged (the now-removed entry would have been applied).
            // Re-index the clean baseline against the shortened list, just like
            // QUndoStack does when it deletes an obsolete command.
            self.undo.adjust_clean_index_after_drop(removed);
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

        // Trivial undo/redo field assignments: `n.field = if undo { old } else { new }`.
        macro_rules! apply_field {
            ($node_id:expr, $field:ident, $old:expr, $new:expr) => {
                if let Some(n) = self.node_mut($node_id) {
                    n.$field = if is_undo { $old } else { $new };
                }
            };
        }

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
                // The changed node's value format changed; clear its history
                // (`controller.cpp:2150-2155`). If `off_adjs` is empty (same-size
                // change) still bump the refresh gen to discard any in-flight
                // async read that would otherwise re-record the OLD format and
                // flash false change-heat; when `off_adjs` is non-empty,
                // `clear_history_for_adjs` performs the bump instead.
                if off_adjs.is_empty() {
                    self.refresh_gen += 1;
                }
                self.clear_node_history(*node_id);
                self.clear_history_for_adjs(off_adjs);
            }
            Command::Rename {
                node_id,
                old_name,
                new_name,
            } => apply_field!(*node_id, name, old_name.clone(), new_name.clone()),
            Command::Collapse {
                node_id,
                old_state,
                new_state,
            } => apply_field!(*node_id, collapsed, *old_state, *new_state),
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
            } => apply_field!(
                *node_id,
                struct_type_name,
                old_name.clone(),
                new_name.clone()
            ),
            Command::ChangeClassKeyword {
                node_id,
                old_keyword,
                new_keyword,
            } => apply_field!(
                *node_id,
                class_keyword,
                old_keyword.clone(),
                new_keyword.clone()
            ),
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
        if let Some(snap) = self.snapshot.as_ref() {
            // `SnapshotProvider::write` (now `&self`) does the real write-through
            // to its `real` provider AND patches the cached pages on success, so
            // a single call updates both — even though the snapshot holds a clone
            // of the same `Arc<dyn Provider>` as `self.doc.provider` (interior
            // mutability on the writable `BufferProvider`).
            snap.write(addr, bytes)
        } else {
            write_provider(&self.doc.provider, addr, bytes)
        }
    }

    /// Write `bytes` at the absolute `addr` through the active provider (the
    /// scanner result-table col-1 `onCellEdited` write path). Returns whether the
    /// write landed. Public so the window can resolve a scanner Value-cell edit
    /// against the live document's writable provider; mirrors the C++
    /// `provider->writeBytes(addr, data)` the scanner uses for a result write.
    pub fn write_memory(&mut self, addr: u64, bytes: &[u8]) -> bool {
        if self.read_only_override || bytes.is_empty() {
            return false;
        }
        self.write_through(addr, bytes)
    }
}

/// Write through an `Arc<dyn Provider>`. `Provider::write` now takes `&self`
/// (interior mutability — PORTING_providers §5), so the write goes straight
/// through the shared handle and succeeds even when a snapshot or refresh worker
/// holds another clone of the same `Arc` — faithfully reproducing the C++
/// `shared_ptr` non-const `write`. Gated by `is_writable()` (read-only providers
/// reject the write as a no-op).
fn write_provider(provider: &Arc<dyn Provider + Send + Sync>, addr: u64, bytes: &[u8]) -> bool {
    if !provider.is_writable() {
        return false;
    }
    provider.write(addr, bytes)
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

    /// Offset adjustments for siblings at or past `from_offset` when the field at
    /// `skip_idx` grows/shrinks by `delta` — the shared insert/remove/duplicate/
    /// resize sibling-shift walk. Empty when `delta == 0`.
    fn sibling_offset_adjs(
        &self,
        parent_id: u64,
        skip_idx: Option<usize>,
        from_offset: i32,
        delta: i32,
    ) -> Vec<OffsetAdj> {
        let mut adjs: Vec<OffsetAdj> = Vec::new();
        if delta == 0 {
            return adjs;
        }
        for si in self.doc.tree.children_of(parent_id) {
            if Some(si) == skip_idx {
                continue;
            }
            let sib = &self.doc.tree.nodes[si];
            if sib.offset >= from_offset {
                adjs.push(OffsetAdj {
                    node_id: sib.id,
                    old_offset: sib.offset,
                    new_offset: sib.offset + delta,
                });
            }
        }
        adjs
    }

    /// Fill `[start_offset, start_offset+total)` with padding fields. `uniform`
    /// forces one (kind,size) for every pad (the hex→hex resize case); otherwise a
    /// largest-first Hex64/32/16/8 ladder is used.
    fn fill_hex_pads(
        &mut self,
        parent_id: u64,
        start_offset: i32,
        total: i32,
        uniform: Option<(NodeKind, i32)>,
    ) {
        let mut pad_offset = start_offset;
        let mut gap = total;
        while gap > 0 {
            let (pad_kind, pad_size) = match uniform {
                Some(u) => u,
                None if gap >= 8 => (NodeKind::Hex64, 8),
                None if gap >= 4 => (NodeKind::Hex32, 4),
                None if gap >= 2 => (NodeKind::Hex16, 2),
                None => (NodeKind::Hex8, 1),
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
    }

    /// The viewed root class's `(target_id, node_index)`, or `None` when there is no
    /// root class target — the shared preamble of the root keyword/name edits.
    fn resolve_root_class(&self) -> Option<(u64, i32)> {
        let target_id = self.root_class_target_id();
        if target_id == 0 {
            return None;
        }
        let idx = self.doc.tree.index_of_id(target_id);
        if idx < 0 {
            return None;
        }
        Some((target_id, idx))
    }

    /// Clone-edit-push envelope for the enum-member operations: clones the member
    /// list, runs `edit`, and pushes `ChangeEnumMembers` only when it returns `true`
    /// (a failed bounds check is a no-op). Returns whether a command was queued.
    fn edit_enum_members(
        &mut self,
        node_id: u64,
        edit: impl FnOnce(&mut Vec<(String, i64)>) -> bool,
    ) -> bool {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return false;
        }
        if !self.doc.tree.nodes[ni as usize].is_enum() {
            return false;
        }
        let old_members = self.doc.tree.nodes[ni as usize].enum_members.clone();
        let mut members = old_members.clone();
        if !edit(&mut members) {
            return false;
        }
        self.push_command(Command::ChangeEnumMembers {
            node_id,
            old_members,
            new_members: members,
        });
        true
    }

    /// The `(member, element_kind, addr)` preamble shared by the three bitfield
    /// member operations, or `None` when the node isn't a valid bitfield member.
    fn resolve_bitfield_member(
        &self,
        node_id: u64,
        member_idx: usize,
    ) -> Option<(BitfieldMember, NodeKind, u64)> {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return None;
        }
        let (member, element_kind) = {
            let n = &self.doc.tree.nodes[ni as usize];
            if !n.is_bitfield() || member_idx >= n.bitfield_members.len() {
                return None;
            }
            (n.bitfield_members[member_idx].clone(), n.element_kind)
        };
        let signed_off = self.doc.tree.compute_offset(ni);
        if signed_off < 0 {
            return None;
        }
        let addr = self.doc.tree.base_address + signed_off as u64;
        Some((member, element_kind, addr))
    }

    /// `(1 << width) - 1`, saturating to `u64::MAX` at width ≥ 64 — the bitfield
    /// member value mask.
    fn bitfield_max(width: u8) -> u64 {
        if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
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
            let gap = old_size - new_size;
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
            let uniform = if hex_to_hex {
                Some((new_kind, new_size))
            } else {
                None
            };
            self.fill_hex_pads(parent_id, base_offset, gap, uniform);

            self.end_macro();
            self.suppress_refresh = was_suppressed;
            if !self.suppress_refresh {
                self.refresh();
            }
        } else {
            // Same size or larger — adjust sibling offsets.
            let delta = new_size - old_size;
            let adjs = if old_size > 0 && new_size > 0 {
                self.sibling_offset_adjs(
                    node.parent_id,
                    Some(node_idx),
                    node.offset + old_size,
                    delta,
                )
            } else {
                Vec::new()
            };
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
        let adjs = self.sibling_offset_adjs(before.parent_id, None, before.offset, insert_size);
        self.push_command(Command::Insert {
            node: n,
            off_adjs: adjs,
        });
    }

    /// `moveNode(nodeIdx, dir)` — reorder a field among its siblings (the C++
    /// `moveNodeRequested` handler, `controller.cpp:792-815`, bound to
    /// Ctrl+Shift+Up/Down in the editor).
    ///
    /// Faithful neighbor-swap: the field exchanges its `offset` with the
    /// immediate neighbor in offset order (`dir` = -1 up / +1 down). The
    /// underlying `nodes[]` array order is NOT touched and no other sibling
    /// offsets shift — compose's sort-by-offset makes the two appear swapped on
    /// the next refresh. Clamp at the first/last sibling (silent no-op, no
    /// wrap). Pushed as one undoable macro ("Move node") of two `ChangeOffset`
    /// commands so a single undo restores both.
    pub fn move_node(&mut self, node_idx: usize, dir: i32) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }
        let node = self.doc.tree.nodes[node_idx].clone();
        // Offset-sorted sibling indices (match compose's visual order;
        // `children_of` returns child-cache order which is not necessarily
        // offset-sorted).
        let mut sibs = self.doc.tree.children_of(node.parent_id);
        sibs.sort_by_key(|&i| self.doc.tree.nodes[i].offset);
        let Some(pos) = sibs.iter().position(|&i| i == node_idx) else {
            return;
        };
        let target = pos as i32 + dir;
        if target < 0 || target as usize >= sibs.len() {
            return; // clamp at the ends — no swap, no wrap.
        }
        let other_idx = sibs[target as usize];
        let other = self.doc.tree.nodes[other_idx].clone();
        if node.offset == other.offset {
            return; // nothing to exchange.
        }
        self.begin_macro("Move node");
        self.push_command(Command::ChangeOffset {
            node_id: node.id,
            old_offset: node.offset,
            new_offset: other.offset,
        });
        self.push_command(Command::ChangeOffset {
            node_id: other.id,
            old_offset: other.offset,
            new_offset: node.offset,
        });
        self.end_macro();
    }

    /// `appendSingleFieldRequested` handler (`controller.cpp:962-1020`), bound to
    /// plain Down-walking-off-the-end in the editor. `node_id` is the LAST VISIBLE
    /// data row's leaf id (a field id, not the struct).
    ///
    /// 1. PARENT RESOLUTION / walk-up: from the leaf, walk UP until the node is a
    ///    Struct/Array/Enum container (`structId`). Returns silently if the id is
    ///    unknown or the walk reaches a root non-container.
    /// 2. ENUM: append one auto-numbered `Member{nextVal}` (nextVal = last value
    ///    + 1, or 0 when empty) via `ChangeEnumMembers`.
    /// 3. EMBEDDED-STRUCT redirect: if `childrenOf(structId)` is empty AND the
    ///    container's `refId != 0`, append into the referenced root class instead.
    /// 4. STRUCT/ARRAY append — HARDCODED `Hex64` at the container TAIL:
    ///    slotOffset = max over children of (sib.offset + node_size(sib)); the new
    ///    offset is that rounded up to `alignment_for(Hex64)` (= 8). Auto-named
    ///    `field_%04x`, pushed as `Insert`.
    /// 5. SELECTION MOVE: clear `sel_ids`, set it to the new node id, reset the
    ///    anchor, `updateCommandRow` — a subsequent Down appends AFTER this field.
    ///    Returns the new node id so the UI can re-find/scroll to its line.
    pub fn append_single_field(&mut self, node_id: u64) -> Option<u64> {
        let mut si = self.doc.tree.index_of_id(node_id);
        if si < 0 {
            return None;
        }
        // Walk up from the leaf to the enclosing Struct/Array/Enum container.
        loop {
            let n = &self.doc.tree.nodes[si as usize];
            if matches!(n.kind, NodeKind::Struct | NodeKind::Array) || n.is_enum() {
                break;
            }
            if n.parent_id == 0 {
                return None;
            }
            si = self.doc.tree.index_of_id(n.parent_id);
            if si < 0 {
                return None;
            }
        }
        let container = self.doc.tree.nodes[si as usize].clone();
        let struct_id = container.id;

        // ENUM: append one auto-numbered member (not the struct-field path).
        if container.is_enum() {
            let mut new_members = container.enum_members.clone();
            let next_val = new_members.last().map(|(_, v)| v + 1).unwrap_or(0);
            new_members.push((format!("Member{}", next_val), next_val));
            self.push_command(Command::ChangeEnumMembers {
                node_id: struct_id,
                old_members: container.enum_members.clone(),
                new_members,
            });
            // Keep the selection on the enum container itself.
            self.sel_ids.clear();
            self.sel_ids.insert(struct_id);
            self.anchor_line = -1;
            self.update_command_row();
            return Some(struct_id);
        }

        // EMBEDDED-STRUCT redirect: an embedded placeholder with no children but a
        // refId → append into the referenced root class instead.
        let mut target_id = struct_id;
        if self.doc.tree.children_of(struct_id).is_empty() && container.ref_id != 0 {
            target_id = container.ref_id;
        }

        // STRUCT/ARRAY field append — HARDCODED Hex64 at the container tail.
        let mut slot_offset = 0i32;
        for ci in self.doc.tree.children_of(target_id) {
            let sib = &self.doc.tree.nodes[ci];
            let sz = self.node_size(sib);
            let end = sib.offset + sz;
            if end > slot_offset {
                slot_offset = end;
            }
        }
        let align = alignment_for(NodeKind::Hex64);
        let offset = (slot_offset + align - 1) / align * align;

        let mut n = Node {
            kind: NodeKind::Hex64,
            name: format!("field_{:04x}", offset),
            parent_id: target_id,
            offset,
            ..Node::default()
        };
        n.id = self.doc.tree.reserve_id();
        let new_id = n.id;
        self.push_command(Command::Insert {
            node: n,
            off_adjs: Vec::new(),
        });

        // SELECTION MOVE: clear then select the new field so the next Down appends
        // after it and the user can immediately retype its kind.
        self.sel_ids.clear();
        self.sel_ids.insert(new_id);
        self.anchor_line = -1;
        self.update_command_row();
        Some(new_id)
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

        let adjs = if parent_id != 0 {
            self.sibling_offset_adjs(parent_id, Some(node_idx), deleted_end, -deleted_size)
        } else {
            Vec::new()
        };

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

    /// `materializeRefChildren(nodeIdx)` (`controller.cpp:2024`) — pointer-follow /
    /// inline materialize.
    ///
    /// A typed pointer / embedded-struct-ref fold head has no children of its own;
    /// expanding it must clone the referenced struct's children inline so they
    /// appear under this node. The editor decides between this and
    /// [`RcxController::toggle_collapse`] from the fold head's `M_CYCLE` marker bit
    /// (`controller.cpp:5893-5897` `handleMarginClick`): a cycle/ref head
    /// materializes, a plain container head toggles.
    ///
    /// All inserts are wrapped in one undo macro ("Materialize ref children") so a
    /// single undo removes the whole materialized subtree, and the self-referential
    /// clone (same kind/name/refId as the parent — the one that *was* the cycle) is
    /// auto-expanded so a single click expands it.
    pub fn materialize_ref_children(&mut self, node_idx: usize) {
        if node_idx >= self.doc.tree.nodes.len() {
            return;
        }

        // Snapshot values before any mutation invalidates references.
        let parent_id = self.doc.tree.nodes[node_idx].id;
        let ref_id = self.doc.tree.nodes[node_idx].ref_id;
        let parent_kind = self.doc.tree.nodes[node_idx].kind;
        let parent_name = self.doc.tree.nodes[node_idx].name.clone();

        if ref_id == 0 {
            return;
        }
        if !self.doc.tree.children_of(parent_id).is_empty() {
            return; // already materialized
        }

        let ref_children = self.doc.tree.children_of(ref_id);
        if ref_children.is_empty() {
            return;
        }

        // Clone children by value, reparent under this node, collapsed.
        let mut clones: Vec<Node> = Vec::with_capacity(ref_children.len());
        for ci in ref_children {
            let mut copy = self.doc.tree.nodes[ci].clone();
            copy.id = self.doc.tree.reserve_id();
            copy.parent_id = parent_id;
            copy.collapsed = true;
            clones.push(copy);
        }

        let was_suppressed = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Materialize ref children");

        for clone in &clones {
            self.push_command(Command::Insert {
                node: clone.clone(),
                off_adjs: Vec::new(),
            });
        }

        // Auto-expand the self-referential child (the one that was the cycle) so
        // the user gets expand in a single click.
        if let Some(clone) = clones
            .iter()
            .find(|c| c.kind == parent_kind && c.name == parent_name && c.ref_id == ref_id)
        {
            self.push_command(Command::Collapse {
                node_id: clone.id,
                old_state: true,
                new_state: false,
            });
        }

        self.end_macro();
        self.suppress_refresh = was_suppressed;
        if !self.suppress_refresh {
            self.refresh();
        }
    }

    /// Resolve the *root class* a command-row edit targets: the explicit
    /// `m_viewRootId` if set, otherwise the first top-level (`parentId == 0`)
    /// `Struct`. Mirrors the `targetId = m_viewRootId; if (targetId == 0) { ... }`
    /// preamble shared by `convertRootKeyword` (`controller.cpp:1620`) and the
    /// `RootClassType`/`RootClassName` inline-commit arms (`controller.cpp:1353`,
    /// `:1377`). Returns `0` when no such struct exists.
    pub fn root_class_target_id(&self) -> u64 {
        if self.view_root_id != 0 {
            return self.view_root_id;
        }
        for n in &self.doc.tree.nodes {
            if n.parent_id == 0 && n.kind == NodeKind::Struct {
                return n.id;
            }
        }
        0
    }

    /// `convertRootKeyword(newKeyword)` (`controller.cpp:1620`) — the menu / keyword
    /// double-click path that *cycles* the root class keyword (struct↔class). Only
    /// allows class↔struct (never enum), pushes an undoable `ChangeClassKeyword`.
    pub fn convert_root_keyword(&mut self, new_keyword: &str) {
        let Some((target_id, idx)) = self.resolve_root_class() else {
            return;
        };
        let old_kw = self.doc.tree.nodes[idx as usize]
            .resolved_class_keyword()
            .to_string();
        if old_kw == new_keyword {
            return;
        }
        // Only allow class↔struct conversion (never enum).
        if old_kw == "enum" || new_keyword == "enum" {
            return;
        }
        self.push_command(Command::ChangeClassKeyword {
            node_id: target_id,
            old_keyword: old_kw,
            new_keyword: new_keyword.to_string(),
        });
    }

    /// `EditTarget::RootClassType` inline-commit (`controller.cpp:1353-1376`):
    /// clicking the `struct`/`class`/`enum` keyword opens a small edit whose only
    /// valid commits are exactly those three keywords (case-insensitive). Anything
    /// else is rejected (no-op). On a real change pushes `ChangeClassKeyword`.
    ///
    /// Unlike [`RcxController::convert_root_keyword`] this DOES permit setting/leaving
    /// `enum` — matching the C++ which guards enum only in the *cycle* path, not the
    /// explicit-keyword commit.
    pub fn set_root_class_keyword(&mut self, text: &str) {
        let kw = text.trim().to_lowercase();
        if kw != "struct" && kw != "class" && kw != "enum" {
            return;
        }
        let Some((target_id, idx)) = self.resolve_root_class() else {
            return;
        };
        let old_kw = self.doc.tree.nodes[idx as usize]
            .resolved_class_keyword()
            .to_string();
        if old_kw == kw {
            return;
        }
        self.push_command(Command::ChangeClassKeyword {
            node_id: target_id,
            old_keyword: old_kw,
            new_keyword: kw,
        });
    }

    /// `EditTarget::RootClassName` inline-commit (`controller.cpp:1377-1400`):
    /// clicking the class/struct NAME in the header renames the viewed root struct's
    /// `structTypeName` (NOT its `name`) via an undoable `ChangeStructTypeName`.
    /// Empty text is rejected (no-op).
    pub fn rename_root_class(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some((target_id, idx)) = self.resolve_root_class() else {
            return;
        };
        let old_name = self.doc.tree.nodes[idx as usize].struct_type_name.clone();
        if old_name == text {
            return;
        }
        self.push_command(Command::ChangeStructTypeName {
            node_id: target_id,
            old_name,
            new_name: text.to_string(),
        });
    }

    /// Rename a SPECIFIC composite's `struct_type_name` (the workspace "Rename" for a
    /// type row — enum / class / struct — whose displayed name IS its type name, not
    /// its instance `name`). `rename_root_class` only targets the viewed root; this
    /// targets any node by id. Pushes the same undoable `ChangeStructTypeName`.
    pub fn rename_struct_type(&mut self, node_id: u64, new_name: &str) {
        if new_name.is_empty() {
            return;
        }
        let idx = self.doc.tree.index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let old_name = self.doc.tree.nodes[idx as usize].struct_type_name.clone();
        if old_name == new_name {
            return;
        }
        self.push_command(Command::ChangeStructTypeName {
            node_id,
            old_name,
            new_name: new_name.to_string(),
        });
    }

    /// Set a node's comment by id (the workspace "Comment…" path). Pushes the same
    /// undoable `ChangeComment` the editor's inline comment edit commits; a no-op
    /// when the comment is unchanged.
    pub fn set_comment(&mut self, node_id: u64, comment: &str) {
        let idx = self.doc.tree.index_of_id(node_id);
        if idx < 0 {
            return;
        }
        let old_comment = self.doc.tree.nodes[idx as usize].comment.clone();
        if old_comment == comment {
            return;
        }
        self.push_command(Command::ChangeComment {
            node_id,
            old_comment,
            new_comment: comment.to_string(),
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

        let adjs = if src.parent_id != 0 {
            self.sibling_offset_adjs(src.parent_id, Some(node_idx), copy_offset, copy_size)
        } else {
            Vec::new()
        };
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

    /// `applyTypePopupResult(mode, nodeIdx, entry, fullText)`
    /// (`controller.cpp:4826`).
    ///
    /// Single entry point the editor calls once the type-selector popup resolves.
    /// It branches on the popup `mode` (root / field / array-element / pointer
    /// target) and the chosen entry kind (primitive vs composite), parsing the
    /// `full_text` for a `*`/`**`/`[N]` modifier, and emits all mutations inside
    /// one undo macro so a single undo reverts the whole type change.
    ///
    /// `struct_id == 0` on a composite means an external/built-in type — it is
    /// imported via [`find_or_create_struct_by_name`](Self::find_or_create_struct_by_name)
    /// before any field mutation (so compose can expand the referenced layout).
    pub fn apply_type_popup_result(
        &mut self,
        mode: TypePopupMode,
        node_id: u64,
        choice: TypePopupChoice,
    ) {
        // Whole popup-apply is one undo macro (`fix #1`): create-new / import /
        // kind-change / refId / sibling-shift all collapse into a single entry.
        // Inner `begin/end_macro` calls fold into this parent buffer.
        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Change type");
        self.apply_type_popup_inner(mode, node_id, choice);
        self.end_macro();
        self.suppress_refresh = was;
        if !self.suppress_refresh {
            self.refresh();
        }
    }

    /// Apply a TypeSelector pick to MANY nodes at once — the editor's
    /// multi-selection Change-Type (`t` over a highlighted range): the SAME
    /// [`TypePopupChoice`] is applied to every id in `node_ids`, collapsed into one
    /// undo macro with a single recompose at the end (mirroring
    /// [`batch_change_kind`](Self::batch_change_kind)). The selection is preserved
    /// (a type change keeps the same rows selected). Single-id lists fall through to
    /// [`apply_type_popup_result`]. Only `FieldType` batches — `Root` re-roots a
    /// single view and `ArrayElement`/`PointerTarget` are single-node contextual
    /// edits, so the caller passes those straight to the single-node path.
    pub fn apply_type_popup_result_batch(
        &mut self,
        mode: TypePopupMode,
        node_ids: &[u64],
        choice: TypePopupChoice,
    ) {
        if node_ids.len() <= 1 {
            if let Some(&id) = node_ids.first() {
                self.apply_type_popup_result(mode, id, choice);
            }
            return;
        }
        let saved_sel = self.sel_ids.clone();
        let saved_anchor = self.anchor_line;
        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro(format!("Change type of {} nodes", node_ids.len()));
        // Resolve each node by id every iteration: applying a kind change can shift
        // sibling offsets / insert padding, but ids are stable, so the next target
        // still resolves. `apply_type_popup_inner` is the raw (already-tree-based)
        // work; its own macro folds into this one.
        for &id in node_ids {
            if self.doc.tree.index_of_id(id) >= 0 {
                self.apply_type_popup_inner(mode, id, choice.clone());
            }
        }
        self.end_macro();
        self.suppress_refresh = was;
        self.sel_ids = saved_sel;
        self.anchor_line = saved_anchor;
        if !self.suppress_refresh {
            self.refresh();
        }
    }

    fn apply_type_popup_inner(
        &mut self,
        mode: TypePopupMode,
        node_id: u64,
        mut choice: TypePopupChoice,
    ) {
        // "+ New" (popup item 15): materialize a brand-new `NewClass[_N]` struct
        // (8×Hex64) and use it as the composite target, rather than importing by
        // name or reusing the generic `Struct` primitive (`controller.cpp:4520`).
        if choice.create_new {
            let (id, name) = self.create_new_class_struct();
            choice.entry_kind = TypeEntryKind::Composite;
            choice.struct_id = id;
            if choice.display_name.is_empty() {
                choice.display_name = name;
            }
        }

        // Resolve external / built-in composites: structId==0 → import by name.
        if choice.entry_kind == TypeEntryKind::Composite
            && choice.struct_id == 0
            && !choice.display_name.is_empty()
        {
            choice.struct_id = self.find_or_create_struct_by_name(&choice.display_name, 0);
        }

        if mode == TypePopupMode::Root {
            if choice.entry_kind == TypeEntryKind::Composite && choice.struct_id != 0 {
                self.set_view_root_id(choice.struct_id);
            }
            return;
        }

        let node_idx = self.doc.tree.index_of_id(node_id);
        if node_idx < 0 {
            return;
        }
        let node_idx = node_idx as usize;

        // Snapshot fields before any mutation (changeNodeKind may reallocate).
        let node_kind = self.doc.tree.nodes[node_idx].kind;
        let elem_kind = self.doc.tree.nodes[node_idx].element_kind;
        let node_ref_id = self.doc.tree.nodes[node_idx].ref_id;
        let arr_len = self.doc.tree.nodes[node_idx].array_len;

        let full = if choice.full_text.is_empty() {
            choice.display_name.clone()
        } else {
            choice.full_text.clone()
        };
        let spec = parse_type_spec(&full);

        match mode {
            TypePopupMode::FieldType => {
                self.apply_field_type(node_id, node_idx, &choice, &spec, node_kind);
            }
            TypePopupMode::ArrayElement => {
                if choice.entry_kind == TypeEntryKind::Primitive {
                    if choice.primitive_kind != elem_kind {
                        self.push_command(Command::ChangeArrayMeta {
                            node_id,
                            old_element_kind: elem_kind,
                            new_element_kind: choice.primitive_kind,
                            old_array_len: arr_len,
                            new_array_len: arr_len,
                        });
                    }
                } else if elem_kind != NodeKind::Struct || node_ref_id != choice.struct_id {
                    self.push_command(Command::ChangeArrayMeta {
                        node_id,
                        old_element_kind: elem_kind,
                        new_element_kind: NodeKind::Struct,
                        old_array_len: arr_len,
                        new_array_len: arr_len,
                    });
                    if node_ref_id != choice.struct_id {
                        self.push_command(Command::ChangePointerRef {
                            node_id,
                            old_ref_id: node_ref_id,
                            new_ref_id: choice.struct_id,
                        });
                    }
                }
            }
            TypePopupMode::PointerTarget => {
                // "void" / primitive entry → refId 0; composite → real structId.
                let real_ref = if choice.entry_kind == TypeEntryKind::Composite {
                    choice.struct_id
                } else {
                    0
                };
                if real_ref != node_ref_id {
                    self.push_command(Command::ChangePointerRef {
                        node_id,
                        old_ref_id: node_ref_id,
                        new_ref_id: real_ref,
                    });
                }
            }
            TypePopupMode::Root => unreachable!(),
        }
    }

    /// The `mode == FieldType` body of [`apply_type_popup_result`], factored out
    /// to keep the match arms readable. Mirrors `controller.cpp:4859-5043`,
    /// including the post-mutation sibling-offset adjustment for Struct/Array
    /// targets (`changeNodeKind` forces newSize=0 for those, so this block owns
    /// the shift).
    fn apply_field_type(
        &mut self,
        node_id: u64,
        node_idx: usize,
        choice: &TypePopupChoice,
        spec: &TypeSpec,
        node_kind: NodeKind,
    ) {
        // Old effective size for the post-mutation sibling adjustment.
        let parent_id = self.doc.tree.nodes[node_idx].parent_id;
        let node_offset = self.doc.tree.nodes[node_idx].offset;
        let mut old_effective = self.doc.tree.nodes[node_idx].byte_size();
        if old_effective == 0 && matches!(node_kind, NodeKind::Struct | NodeKind::Array) {
            old_effective = self.doc.tree.struct_span(node_id);
        }

        if choice.entry_kind == TypeEntryKind::Primitive {
            let pk = choice.primitive_kind;
            if spec.array_count > 0 {
                // Primitive array, e.g. "int32_t[10]".
                let was = self.suppress_refresh;
                self.suppress_refresh = true;
                self.begin_macro("Change to primitive array");
                if node_kind != NodeKind::Array {
                    self.change_node_kind(node_idx, NodeKind::Array);
                }
                let idx = self.doc.tree.index_of_id(node_id);
                if idx >= 0 {
                    let n = &self.doc.tree.nodes[idx as usize];
                    if n.element_kind != pk || n.array_len != spec.array_count {
                        let (oek, oal) = (n.element_kind, n.array_len);
                        self.push_command(Command::ChangeArrayMeta {
                            node_id,
                            old_element_kind: oek,
                            new_element_kind: pk,
                            old_array_len: oal,
                            new_array_len: spec.array_count,
                        });
                    }
                }
                self.end_macro();
                self.suppress_refresh = was;
                if !self.suppress_refresh {
                    self.refresh();
                }
            } else if spec.is_pointer {
                if !is_valid_primitive_ptr_target(pk) {
                    // Hex / pointer / fnptr with `*` → a plain void pointer.
                    if node_kind != NodeKind::Pointer64 {
                        self.change_node_kind(node_idx, NodeKind::Pointer64);
                    }
                    let idx = self.doc.tree.index_of_id(node_id);
                    if idx >= 0 {
                        let old_ref = {
                            let n = &mut self.doc.tree.nodes[idx as usize];
                            n.ptr_depth = 0;
                            n.ref_id
                        };
                        if old_ref != 0 {
                            self.push_command(Command::ChangePointerRef {
                                node_id,
                                old_ref_id: old_ref,
                                new_ref_id: 0,
                            });
                        }
                    }
                } else {
                    // Primitive pointer, e.g. "int32*" / "f64**".
                    let was = self.suppress_refresh;
                    self.suppress_refresh = true;
                    self.begin_macro("Change to primitive pointer");
                    if node_kind != NodeKind::Pointer64 {
                        self.change_node_kind(node_idx, NodeKind::Pointer64);
                    }
                    let idx = self.doc.tree.index_of_id(node_id);
                    if idx >= 0 {
                        let old_ref = {
                            let n = &mut self.doc.tree.nodes[idx as usize];
                            if n.element_kind != pk || n.ptr_depth != spec.ptr_depth {
                                n.element_kind = pk;
                                n.ptr_depth = spec.ptr_depth;
                            }
                            n.ref_id
                        };
                        if old_ref != 0 {
                            self.push_command(Command::ChangePointerRef {
                                node_id,
                                old_ref_id: old_ref,
                                new_ref_id: 0,
                            });
                        }
                    }
                    self.end_macro();
                    self.suppress_refresh = was;
                    if !self.suppress_refresh {
                        self.refresh();
                    }
                }
            } else if pk != node_kind {
                self.change_node_kind(node_idx, pk);
            }
        } else {
            // Composite target.
            let struct_id = choice.struct_id;
            let was = self.suppress_refresh;
            self.suppress_refresh = true;
            self.begin_macro("Change to composite type");

            if spec.is_pointer {
                // Pointer modifier → Pointer64 + refId + ptrDepth.
                if node_kind != NodeKind::Pointer64 {
                    self.change_node_kind(node_idx, NodeKind::Pointer64);
                }
                let idx = self.doc.tree.index_of_id(node_id);
                if idx >= 0 {
                    let new_depth = (spec.ptr_depth - 1).max(0);
                    let old_ref = {
                        let n = &mut self.doc.tree.nodes[idx as usize];
                        if n.ptr_depth != new_depth {
                            n.ptr_depth = new_depth;
                        }
                        n.ref_id
                    };
                    if old_ref != struct_id {
                        self.push_command(Command::ChangePointerRef {
                            node_id,
                            old_ref_id: old_ref,
                            new_ref_id: struct_id,
                        });
                    }
                }
            } else if spec.array_count > 0 {
                // Array modifier → Array + Struct element.
                if node_kind != NodeKind::Array {
                    self.change_node_kind(node_idx, NodeKind::Array);
                }
                let idx = self.doc.tree.index_of_id(node_id);
                if idx >= 0 {
                    let n = &self.doc.tree.nodes[idx as usize];
                    let (oek, oal, oref) = (n.element_kind, n.array_len, n.ref_id);
                    if oek != NodeKind::Struct || oal != spec.array_count {
                        self.push_command(Command::ChangeArrayMeta {
                            node_id,
                            old_element_kind: oek,
                            new_element_kind: NodeKind::Struct,
                            old_array_len: oal,
                            new_array_len: spec.array_count,
                        });
                    }
                    if oref != struct_id {
                        self.push_command(Command::ChangePointerRef {
                            node_id,
                            old_ref_id: oref,
                            new_ref_id: struct_id,
                        });
                    }
                }
            } else {
                // Plain struct → Struct + structTypeName + refId.
                if node_kind != NodeKind::Struct {
                    self.change_node_kind(node_idx, NodeKind::Struct);
                }
                let idx = self.doc.tree.index_of_id(node_id);
                if idx >= 0 {
                    // Derive the type name from the referenced root struct.
                    let target_name = {
                        let ri = self.doc.tree.index_of_id(struct_id);
                        if ri >= 0 {
                            let r = &self.doc.tree.nodes[ri as usize];
                            if r.struct_type_name.is_empty() {
                                r.name.clone()
                            } else {
                                r.struct_type_name.clone()
                            }
                        } else {
                            String::new()
                        }
                    };
                    let (old_type_name, old_ref) = {
                        let n = &self.doc.tree.nodes[idx as usize];
                        (n.struct_type_name.clone(), n.ref_id)
                    };
                    if old_type_name != target_name {
                        self.push_command(Command::ChangeStructTypeName {
                            node_id,
                            old_name: old_type_name,
                            new_name: target_name,
                        });
                    }
                    if old_ref != struct_id {
                        self.push_command(Command::ChangePointerRef {
                            node_id,
                            old_ref_id: old_ref,
                            new_ref_id: struct_id,
                        });
                    }
                }
            }

            self.end_macro();
            self.suppress_refresh = was;
            if !self.suppress_refresh {
                self.refresh();
            }
        }

        // ── Post-mutation sibling offset adjustment (Struct/Array only) ──
        let ni = self.doc.tree.index_of_id(node_id);
        if ni >= 0 {
            let kind = self.doc.tree.nodes[ni as usize].kind;
            if matches!(kind, NodeKind::Struct | NodeKind::Array) {
                let mut new_effective = self.doc.tree.nodes[ni as usize].byte_size();
                if new_effective == 0 && kind == NodeKind::Struct {
                    new_effective = self.doc.tree.struct_span(node_id);
                }
                if new_effective == 0 && kind == NodeKind::Array {
                    let (ek, rid, al) = {
                        let n = &self.doc.tree.nodes[ni as usize];
                        (n.element_kind, n.ref_id, n.array_len)
                    };
                    if ek == NodeKind::Struct && rid != 0 {
                        let elem_span = self.doc.tree.struct_span(rid) as i64;
                        new_effective = (elem_span * al as i64).min(i32::MAX as i64) as i32;
                    } else if ek != NodeKind::Struct {
                        let p = size_for_kind(ek) as i64 * al as i64;
                        new_effective = p.min(i32::MAX as i64) as i32;
                    }
                }
                let size_delta = new_effective - old_effective;
                if size_delta != 0 && old_effective > 0 {
                    let old_end = node_offset + old_effective;
                    let siblings = self.doc.tree.children_of(parent_id);
                    let was = self.suppress_refresh;
                    self.suppress_refresh = true;
                    self.begin_macro("Adjust sibling offsets");
                    for si in siblings {
                        let (sib_id, sib_off, sib_static) = {
                            let s = &self.doc.tree.nodes[si];
                            (s.id, s.offset, s.is_static)
                        };
                        if sib_id == node_id || sib_static {
                            continue;
                        }
                        if sib_off >= old_end {
                            self.push_command(Command::ChangeOffset {
                                node_id: sib_id,
                                old_offset: sib_off,
                                new_offset: sib_off + size_delta,
                            });
                        }
                    }
                    self.end_macro();
                    self.suppress_refresh = was;
                    if !self.suppress_refresh {
                        self.refresh();
                    }
                }
            }
        }
    }

    /// `findOrCreateStructByName(typeName, depth)` (`controller.cpp:5074`).
    ///
    /// Resolve a type by name to a root-struct id: reuse an existing local root
    /// struct, else materialize a built-in [`K_COMMON_TYPES`] layout (recursively
    /// wiring pointer targets), else a default 8×Hex64 struct. All inserts are
    /// wrapped in one "Import type" macro. Returns the resolved root id (0 only on
    /// recursion-depth overflow).
    pub fn find_or_create_struct_by_name(&mut self, type_name: &str, depth: i32) -> u64 {
        if depth > 8 {
            return 0; // guard against cyclic type graphs.
        }
        // Already present locally?
        for n in &self.doc.tree.nodes {
            if n.parent_id == 0
                && n.kind == NodeKind::Struct
                && (n.struct_type_name == type_name
                    || (n.struct_type_name.is_empty() && n.name == type_name))
            {
                return n.id;
            }
        }

        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Import type");

        let mut root = Node {
            kind: NodeKind::Struct,
            struct_type_name: type_name.to_string(),
            name: "instance".to_string(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        root.id = self.doc.tree.reserve_id();
        let root_id = root.id;

        if let Some(ct) = find_common_type(type_name) {
            root.class_keyword = ct.class_keyword.to_string();
            self.push_command(Command::Insert {
                node: root,
                off_adjs: Vec::new(),
            });
            for f in ct.fields {
                let mut child = Node {
                    kind: f.kind,
                    name: f.name.to_string(),
                    parent_id: root_id,
                    offset: f.offset,
                    ..Node::default()
                };
                child.id = self.doc.tree.reserve_id();
                let child_id = child.id;
                if !f.ptr_target.is_empty()
                    && matches!(f.kind, NodeKind::Pointer64 | NodeKind::Pointer32)
                    && f.ptr_target != type_name
                {
                    self.push_command(Command::Insert {
                        node: child,
                        off_adjs: Vec::new(),
                    });
                    let target_id = self.find_or_create_struct_by_name(f.ptr_target, depth + 1);
                    if target_id != 0 {
                        self.push_command(Command::ChangePointerRef {
                            node_id: child_id,
                            old_ref_id: 0,
                            new_ref_id: target_id,
                        });
                    }
                } else {
                    self.push_command(Command::Insert {
                        node: child,
                        off_adjs: Vec::new(),
                    });
                }
            }
        } else {
            // Unknown type → default 8×Hex64.
            self.push_command(Command::Insert {
                node: root,
                off_adjs: Vec::new(),
            });
            for i in 0..8 {
                let mut child = Node {
                    kind: NodeKind::Hex64,
                    name: format!("field_{:02x}", i * 8),
                    parent_id: root_id,
                    offset: i * 8,
                    ..Node::default()
                };
                child.id = self.doc.tree.reserve_id();
                self.push_command(Command::Insert {
                    node: child,
                    off_adjs: Vec::new(),
                });
            }
        }

        self.end_macro();
        self.suppress_refresh = was;
        root_id
    }

    /// Editor "New Class" materialization (`controller.cpp:3390-3423`): create a
    /// fresh `NewClass` / `NewClass_2` / `NewClass_3` … root **class** definition
    /// with 8×Hex64 fields in one macro. Returns `(root_id, type_name)`.
    ///
    /// Matches the C++ editor New-Class lambda exactly: `classKeyword = "class"`
    /// (so it renders + round-trips as `class NewClass { … }`, not `struct`) and
    /// the collision suffix is `"%1_%2"` starting the counter at **2** —
    /// identical to `convert_to_typed_pointer`. (The C++ type-picker
    /// `createNewTypeRequested` path used a different `NewClass1`/empty-keyword
    /// naming; in this port both the editor New-Class action and the type-picker
    /// "+ New" share `new_class_on_node` → this materialization, so we follow the
    /// editor New-Class semantics that the user actually sees.)
    pub fn create_new_class_struct(&mut self) -> (u64, String) {
        let base = "NewClass";
        let mut type_name = base.to_string();
        let mut counter = 2;
        let existing: std::collections::HashSet<String> = self
            .doc
            .tree
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Struct && !n.struct_type_name.is_empty())
            .map(|n| n.struct_type_name.clone())
            .collect();
        while existing.contains(&type_name) {
            type_name = format!("{base}_{counter}");
            counter += 1;
        }

        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("New Class");

        let mut root = Node {
            kind: NodeKind::Struct,
            struct_type_name: type_name.clone(),
            class_keyword: "class".to_string(),
            name: "instance".to_string(),
            parent_id: 0,
            offset: 0,
            ..Node::default()
        };
        root.id = self.doc.tree.reserve_id();
        let root_id = root.id;
        self.push_command(Command::Insert {
            node: root,
            off_adjs: Vec::new(),
        });
        for i in 0..8 {
            let mut child = Node {
                kind: NodeKind::Hex64,
                name: format!("field_{:02x}", i * 8),
                parent_id: root_id,
                offset: i * 8,
                ..Node::default()
            };
            child.id = self.doc.tree.reserve_id();
            self.push_command(Command::Insert {
                node: child,
                off_adjs: Vec::new(),
            });
        }

        self.end_macro();
        self.suppress_refresh = was;
        (root_id, type_name)
    }

    /// Editor "New Class" (`controller.cpp:3390`): create a fresh `NewClass[_N]`
    /// class definition (8×Hex64 = 64 bytes) and convert `node_id` into an
    /// embedded struct instance referencing it, shifting siblings to make room.
    /// This mirrors the type-picker "+ New" composite flow — the previous editor
    /// path inserted a bare childless `Struct`, so expanding the new class showed
    /// an EMPTY body and the arrow keys had no child rows to descend into.
    ///
    /// With `node_id == 0` (no selected node / caret) the populated class is
    /// created and made the view root, so the user still lands inside a
    /// non-empty class rather than on an empty inline struct.
    pub fn new_class_on_node(&mut self, node_id: u64) {
        // `create_new: true` makes `apply_type_popup_inner` materialize the
        // 8×Hex64 `NewClass[_N]` definition and use it as the composite target;
        // `FieldType` then converts the node into an instance of it (+ sibling
        // shift), exactly as the C++ "New Class" lambda does in three steps.
        let choice = TypePopupChoice {
            entry_kind: TypeEntryKind::Composite,
            primitive_kind: NodeKind::Struct,
            struct_id: 0,
            display_name: String::new(),
            full_text: String::new(),
            create_new: true,
        };
        let mode = if node_id != 0 && self.doc.tree.index_of_id(node_id) >= 0 {
            TypePopupMode::FieldType
        } else {
            TypePopupMode::Root
        };
        self.apply_type_popup_result(mode, node_id, choice);
    }

    /// The composite [`TypePopupChoice`] entries surfaced by the type popup beyond
    /// the local named structs: every built-in [`K_COMMON_TYPES`] entry, as an
    /// importable composite (`struct_id == 0` ⇒ imported on choose). Mirrors the
    /// C++ `fullTypeEntries` appending `kCommonTypes` after the local structs
    /// (`controller.cpp`, raw gaps 13/14). The editor concatenates these after its
    /// local-struct entries.
    pub fn common_type_entries(&self) -> Vec<TypePopupChoice> {
        K_COMMON_TYPES
            .iter()
            .map(|ct| TypePopupChoice {
                entry_kind: TypeEntryKind::Composite,
                primitive_kind: NodeKind::Struct,
                struct_id: 0,
                display_name: ct.name.to_string(),
                full_text: String::new(),
                create_new: false,
            })
            .collect()
    }

    /// `dissolveUnion(unionId)` (`controller.cpp:1959`).
    ///
    /// Flatten a union back into its parent scope: each member (and its subtree)
    /// is re-parented under the union's parent at `unionOffset + memberOffset`,
    /// then the union node itself is removed — all in one "Dissolve union" macro.
    pub fn dissolve_union(&mut self, union_id: u64) {
        let ui = self.doc.tree.index_of_id(union_id);
        if ui < 0 {
            return;
        }
        let (kind, is_union, parent_id, union_offset) = {
            let u = &self.doc.tree.nodes[ui as usize];
            (u.kind, u.is_union(), u.parent_id, u.offset)
        };
        if kind != NodeKind::Struct || !is_union {
            return;
        }

        // Snapshot each direct member + its (non-self) subtree, by value.
        struct SavedMember {
            node: Node,
            subtree: Vec<Node>,
        }
        let mut saved: Vec<SavedMember> = Vec::new();
        for ci in self.doc.tree.children_of(union_id) {
            let member = self.doc.tree.nodes[ci].clone();
            let mut subtree: Vec<Node> = Vec::new();
            for si in self.doc.tree.subtree_indices(member.id) {
                if self.doc.tree.nodes[si].id != member.id {
                    subtree.push(self.doc.tree.nodes[si].clone());
                }
            }
            saved.push(SavedMember {
                node: member,
                subtree,
            });
        }

        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Dissolve union");

        // Remove the union (and all its children).
        {
            let subtree: Vec<Node> = self
                .doc
                .tree
                .subtree_indices(union_id)
                .into_iter()
                .map(|si| self.doc.tree.nodes[si].clone())
                .collect();
            self.push_command(Command::Remove {
                node_id: union_id,
                subtree,
                off_adjs: Vec::new(),
            });
        }

        // Re-insert each member under the union's parent at the union's offset.
        for sm in &saved {
            let mut copy = sm.node.clone();
            copy.parent_id = parent_id;
            copy.offset = union_offset + sm.node.offset;
            copy.id = self.doc.tree.reserve_id();
            let new_id = copy.id;
            let old_id = sm.node.id;
            self.push_command(Command::Insert {
                node: copy,
                off_adjs: Vec::new(),
            });
            for child in &sm.subtree {
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
        self.suppress_refresh = was;
        if !self.suppress_refresh {
            self.refresh();
        }
    }

    /// The current value of a bitfield member + its max, for seeding the
    /// "Edit Bitfield Value" prompt (`controller.cpp:2871-2873`). Returns
    /// `(value, max_val)`, or `None` if the node is not a valid bitfield member.
    /// Reads through [`format::extract_bits`] (the same path the formatter uses).
    pub fn bitfield_member_value(&self, node_id: u64, member_idx: usize) -> Option<(u64, u64)> {
        let (member, element_kind, addr) = self.resolve_bitfield_member(node_id, member_idx)?;
        let val = format::extract_bits(
            &*self.doc.provider,
            addr,
            element_kind,
            member.bit_offset,
            member.bit_width,
        );
        Some((val, Self::bitfield_max(member.bit_width)))
    }

    /// `toggleBitfieldBit(nodeId, memberIdx)` (`controller.cpp:2825`).
    ///
    /// XOR-flip one bit of a bitfield member through an undoable `WriteBytes`.
    /// No-op unless the node is a bitfield, the member index is in range, and the
    /// provider is writable.
    pub fn toggle_bitfield_bit(&mut self, node_id: u64, member_idx: usize) {
        let Some((member, element_kind, addr)) = self.resolve_bitfield_member(node_id, member_idx)
        else {
            return;
        };
        if !self.doc.provider.is_writable() {
            return;
        }
        let container_size = {
            let s = size_for_kind(element_kind);
            if s <= 0 {
                4
            } else {
                s
            }
        };

        let mut old_bytes = vec![0u8; container_size as usize];
        self.doc.provider.read(addr, &mut old_bytes);
        let mut new_bytes = old_bytes.clone();
        let byte_idx = (member.bit_offset / 8) as usize;
        let bit_in_byte = member.bit_offset % 8;
        if byte_idx < new_bytes.len() {
            new_bytes[byte_idx] ^= 1u8 << bit_in_byte;
        }
        self.push_command(Command::WriteBytes {
            addr,
            old_bytes,
            new_bytes,
        });
    }

    /// `editBitfieldValue(nodeId, memberIdx, newValueText)` (`controller.cpp:2855`).
    ///
    /// Read-modify-write the member's bit span to `new_value_text` (decimal, or
    /// `0x`-prefixed hex), clamped to `(1<<width)-1`. The UI supplies the typed
    /// string (the C++ prompts a dialog); returns whether a write was queued.
    pub fn edit_bitfield_value(
        &mut self,
        node_id: u64,
        member_idx: usize,
        new_value_text: &str,
    ) -> bool {
        let Some((member, element_kind, addr)) = self.resolve_bitfield_member(node_id, member_idx)
        else {
            return false;
        };
        if !self.doc.provider.is_writable() {
            return false;
        }
        let container_size = {
            let s = size_for_kind(element_kind);
            if s <= 0 {
                4
            } else {
                s
            }
        };

        let max_val: u64 = Self::bitfield_max(member.bit_width);

        // Parse the typed value (hex with 0x prefix, else decimal).
        let s = new_value_text.trim();
        if s.is_empty() {
            return false;
        }
        let parsed = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u64::from_str_radix(hex, 16)
        } else {
            s.parse::<u64>()
        };
        let Ok(mut new_val) = parsed else {
            return false;
        };
        new_val &= max_val;

        let mut old_bytes = vec![0u8; container_size as usize];
        self.doc.provider.read(addr, &mut old_bytes);
        let mut new_bytes = old_bytes.clone();
        // Read-modify-write the container's masked bit span.
        let mut container: u64 = 0;
        let n = (container_size as usize).min(8);
        for (i, b) in old_bytes.iter().take(n).enumerate() {
            container |= (*b as u64) << (8 * i);
        }
        let mask = max_val << member.bit_offset;
        container = (container & !mask) | ((new_val & max_val) << member.bit_offset);
        for (i, b) in new_bytes.iter_mut().take(n).enumerate() {
            *b = (container >> (8 * i)) as u8;
        }

        self.push_command(Command::WriteBytes {
            addr,
            old_bytes,
            new_bytes,
        });
        true
    }

    /// `insertStaticField(parentId)` (`controller.cpp:2909`).
    ///
    /// Add a static/global field (`isStatic`, `offsetExpr = "base"`, Hex64) as a
    /// child of `parent_id`, undoable.
    pub fn insert_static_field(&mut self, parent_id: u64) {
        let mut sf = Node {
            kind: NodeKind::Hex64,
            name: "static_field".to_string(),
            parent_id,
            offset: 0,
            is_static: true,
            offset_expr: "base".to_string(),
            ..Node::default()
        };
        sf.id = self.doc.tree.reserve_id();
        self.push_command(Command::Insert {
            node: sf,
            off_adjs: Vec::new(),
        });
    }

    /// Add a member to an enum (`Add Member`, `controller.cpp:3364` / `3315`):
    /// append `("NewMember", lastVal+1)`. `at == None` appends; `at == Some(i)`
    /// inserts before index `i` (Add Member Above/Below map to `i` / `i+1`).
    /// Returns whether a member was added.
    ///
    /// Bitfield members have NO add operation in the C++ — the bitfield-member
    /// context menu only offers Toggle Bit / Edit Value (`controller.cpp:3348`),
    /// so this is a no-op (returns `false`) for bitfields.
    pub fn add_member(&mut self, node_id: u64, at: Option<usize>) -> bool {
        self.edit_enum_members(node_id, |members| {
            let pos = at.unwrap_or(members.len()).min(members.len());
            // Value policy (`controller.cpp:3321`/`3368`): a strict append uses
            // `last + 1`; an insert before `pos` uses `predecessor + 1` (or 0 at
            // the head).
            let val = if at.is_none() {
                members.last().map(|(_, v)| v + 1).unwrap_or(0)
            } else if pos > 0 {
                members[pos - 1].1 + 1
            } else {
                0
            };
            members.insert(pos, ("NewMember".to_string(), val));
            true
        })
    }

    /// Rename enum member `member_idx` to `name` (`controller.cpp:1142-1149`).
    ///
    /// Bitfield members have NO rename operation in the C++ (the bitfield-member
    /// context menu only offers Toggle Bit / Edit Value, `controller.cpp:3348`),
    /// so this is a no-op (returns `false`) for bitfields.
    pub fn rename_member(&mut self, node_id: u64, member_idx: usize, name: &str) -> bool {
        self.edit_enum_members(node_id, |members| {
            if member_idx >= members.len() {
                return false;
            }
            members[member_idx].0 = name.to_string();
            true
        })
    }

    /// Set enum member `member_idx`'s value (`controller.cpp:1222-1239`). Accepts
    /// decimal or `0x`-prefixed hex; no-op on parse failure.
    pub fn set_member_value(&mut self, node_id: u64, member_idx: usize, value_text: &str) -> bool {
        self.edit_enum_members(node_id, |members| {
            if member_idx >= members.len() {
                return false;
            }
            let s = value_text.trim();
            // toLongLong(10) then toLongLong(16) fallback, mirroring the C++.
            let parsed = s
                .parse::<i64>()
                .ok()
                .or_else(|| {
                    s.strip_prefix("0x")
                        .or_else(|| s.strip_prefix("0X"))
                        .and_then(|h| i64::from_str_radix(h, 16).ok())
                })
                .or_else(|| i64::from_str_radix(s, 16).ok());
            let Some(val) = parsed else {
                return false;
            };
            members[member_idx].1 = val;
            true
        })
    }

    /// Delete enum member `member_idx` (`Remove Member`, `controller.cpp:3337`),
    /// which is only offered for enum members.
    ///
    /// Bitfield members have NO remove operation in the C++ (the bitfield-member
    /// context menu only offers Toggle Bit / Edit Value, `controller.cpp:3348`),
    /// so this is a no-op (returns `false`) for bitfields.
    pub fn delete_member(&mut self, node_id: u64, member_idx: usize) -> bool {
        self.edit_enum_members(node_id, |members| {
            if member_idx >= members.len() {
                return false;
            }
            members.remove(member_idx);
            true
        })
    }

    /// Resolve an address expression through [`AddressParser`] against the active
    /// provider, wiring `resolveModule` / `readPointer` / `resolveIdentifier`
    /// (via [`SymbolStore`]) and — when the provider reports kernel paging —
    /// `vtop` / `cr3` / `physRead`. Mirrors the callback bag built in
    /// `controller.cpp:1107` / `1249` / `5168`. Returns `(value, ok)`.
    ///
    /// This is the single resolution path used by base-address inline edits,
    /// provider attach / source switch re-evaluation, and goto/scanner address
    /// resolution (raw gaps 63/64/65/66).
    pub fn resolve_address_expr(&self, expr: &str) -> (u64, bool) {
        let result = self.resolve_address_expr_full(expr);
        (result.value, result.ok)
    }

    /// Like [`resolve_address_expr`](Self::resolve_address_expr) but returns the
    /// full [`AddressParseResult`] so callers can surface the parser's error
    /// string (mirrors C++ `AddressParser::evaluate` feeding `result.error` back
    /// to `navigateToFormula`, `controller.cpp:5929-5933`).
    pub fn resolve_address_expr_full(&self, expr: &str) -> crate::addr::AddressParseResult {
        use crate::addr::{AddressParseResult, AddressParser, AddressParserCallbacks};
        #[cfg(feature = "symbols")]
        use crate::rtti::symbol_store::SymbolStore;

        let cleaned: String = expr.chars().filter(|&c| c != '`' && c != '\'').collect();
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            return AddressParseResult {
                ok: false,
                value: 0,
                error: "empty expression".to_string(),
                error_pos: 0,
            };
        }

        let prov = &*self.doc.provider;
        let ptr_sz = self.doc.tree.pointer_size;
        let mut cbs = AddressParserCallbacks {
            resolve_module: Some(Box::new(move |name: &str| {
                let base = prov.symbol_to_address(name);
                (base, base != 0)
            })),
            read_pointer: Some(Box::new(move |addr: u64| {
                let mut buf = [0u8; 8];
                let n = ptr_sz.clamp(1, 8) as usize;
                let ok = prov.read(addr, &mut buf[..n]);
                let mut val = 0u64;
                for (i, b) in buf[..n].iter().enumerate() {
                    val |= (*b as u64) << (8 * i);
                }
                (val, ok)
            })),
            ..Default::default()
        };

        // `resolveIdentifier` is backed by the global `SymbolStore`, which lives
        // in the `symbols`-gated `rtti` module. Without that feature there are no
        // user-imported symbols to resolve, so the callback is simply absent
        // (the parser then fails identifier lookups, matching the headless build).
        #[cfg(feature = "symbols")]
        {
            cbs.resolve_identifier = Some(Box::new(move |name: &str| {
                match SymbolStore::global().lock() {
                    Ok(store) => store.resolve(name, Some(prov)),
                    Err(_) => (0, false),
                }
            }));
        }

        if prov.has_kernel_paging() {
            cbs.vtop = Some(Box::new(move |_pid: u32, va: u64| {
                let r = prov.translate_address(va);
                (r.physical, r.valid)
            }));
            cbs.cr3 = Some(Box::new(move |_pid: u32| {
                let cr3 = prov.get_cr3();
                (cr3, cr3 != 0)
            }));
            cbs.phys_read = Some(Box::new(move |phys_addr: u64| {
                let entries = prov.read_page_table(phys_addr, 0, 1);
                (entries.first().copied().unwrap_or(0), !entries.is_empty())
            }));
        }

        AddressParser::evaluate(cleaned, ptr_sz, Some(&cbs))
    }

    /// Commit a base-address inline edit (`EditTarget::BaseAddress`,
    /// `controller.cpp:1243-1302`): evaluate the typed expression through the
    /// live provider callbacks ([`resolve_address_expr`](Self::resolve_address_expr)),
    /// and on success push a [`Command::ChangeBase`] preserving the user-typed
    /// expression as the formula — unless it is a bare hex/decimal literal that
    /// round-trips through the canonical `0xHEX` display.
    pub fn commit_base_address(&mut self, text: &str) {
        let mut s = text.trim().to_string();
        s.retain(|c| c != '`' && c != '\n' && c != '\r');
        let (value, ok) = self.resolve_address_expr(&s);
        if !ok {
            return;
        }
        // A bare literal (0xHEX or decimal) round-trips through the display, so
        // store an empty formula; anything richer is kept verbatim.
        let trimmed = s.trim();
        let is_literal = !trimmed.is_empty()
            && (trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
                .map(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
                .unwrap_or(false)
                || trimmed.chars().all(|c| c.is_ascii_digit()));
        let new_formula = if is_literal {
            String::new()
        } else {
            trimmed.to_string()
        };
        let old_base = self.doc.tree.base_address;
        let old_formula = self.doc.tree.base_address_formula.clone();
        if value != old_base || new_formula != old_formula {
            self.push_command(Command::ChangeBase {
                old_base,
                new_base: value,
                old_formula,
                new_formula,
            });
        }
    }

    /// `navigateToFormula(formula)` (`controller.cpp:5908`).
    ///
    /// Non-undoable "go to" used by the bookmark list and the goto-address
    /// dialog: trims the formula, evaluates it through the live provider
    /// callbacks, and on success sets `base_address` + `base_address_formula`
    /// **directly** (no [`Command::ChangeBase`] / undo entry — distinct from the
    /// user-driven [`commit_base_address`](Self::commit_base_address)). On
    /// failure the parser's error string is returned verbatim and the document is
    /// left untouched. Mirrors C++ which returns `false` + `*errOut = error` and
    /// only emits `documentChanged()` + `refresh()` on success.
    pub fn navigate_to_formula(&mut self, formula: &str) -> Result<(), String> {
        let f = formula.trim();
        if f.is_empty() {
            return Err("empty formula".to_string());
        }
        let result = self.resolve_address_expr_full(f);
        if !result.ok {
            return Err(result.error);
        }
        self.doc.tree.base_address = result.value;
        self.doc.tree.base_address_formula = f.to_string();
        self.on_document_changed();
        Ok(())
    }

    /// Re-evaluate the stored `base_address_formula` against the current provider
    /// and update `tree.base_address` in place (no undo entry — relocation, not a
    /// user edit). No-op when the formula is empty. Mirrors the post-attach /
    /// post-source-switch re-evaluation block (`controller.cpp:5167-5208`).
    pub fn reevaluate_base_address_formula(&mut self) {
        if self.doc.tree.base_address_formula.is_empty() {
            return;
        }
        let formula = self.doc.tree.base_address_formula.clone();
        let (value, ok) = self.resolve_address_expr(&formula);
        if ok {
            self.doc.tree.base_address = value;
        }
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

    /// "Convert to Hex" — decompose a non-hex primitive into raw hex pads
    /// (`controller.cpp:3713-3753`).
    ///
    /// Unlike the multi-select "Convert to Hex" (which just `changeNodeKind`s
    /// each node to a single same-size hex equivalent, keeping identity/name),
    /// the single-node convert REMOVES the node entirely and re-fills its byte
    /// range with the largest-first hex pads (Hex64 → Hex32 → Hex16 → Hex8) that
    /// cover the full `byteSize()`, each named `pad_<offset>` (2-wide zero-padded
    /// lowercase hex of the absolute offset). Example: a 12-byte `Vec3` at +0
    /// becomes `pad_00` (Hex64) + `pad_08` (Hex32). Containers (Struct/Array) and
    /// nodes that are already hex are not converted (the menu only offers this for
    /// non-hex non-container primitives), and a `byteSize() <= 0` node is a no-op.
    pub fn convert_to_hex(&mut self, node_id: u64) {
        let ni = self.doc.tree.index_of_id(node_id);
        if ni < 0 {
            return;
        }
        let node = self.doc.tree.nodes[ni as usize].clone();
        // Mirror the C++ menu guard: only non-hex, non-container primitives.
        if is_hex_node(node.kind) || node.kind == NodeKind::Struct || node.kind == NodeKind::Array {
            return;
        }
        let total_size = node.byte_size();
        if total_size <= 0 {
            return;
        }
        let parent_id = node.parent_id;
        let base_offset = node.offset;

        let was = self.suppress_refresh;
        self.suppress_refresh = true;
        self.begin_macro("Convert to Hex");

        // Remove the original node (and its — for a primitive, empty — subtree).
        self.push_command(Command::Remove {
            node_id,
            subtree: vec![node.clone()],
            off_adjs: Vec::new(),
        });

        // Largest-first hex pads covering the whole byte range.
        self.fill_hex_pads(parent_id, base_offset, total_size, None);

        self.end_macro();
        self.suppress_refresh = was;
        if !self.suppress_refresh {
            self.refresh();
        }
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
        } else if node_idx < self.doc.tree.nodes.len() {
            // `controller.cpp:1196-1217`: when the text isn't a primitive/array kind,
            // check whether it names a defined Struct type (some node's
            // `structTypeName`). If so, convert this node to a Struct and push a
            // `ChangeStructTypeName` so its `structTypeName` matches `text`.
            let is_struct_type = self
                .doc
                .tree
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Struct && n.struct_type_name == text);
            if is_struct_type {
                let node_id = self.doc.tree.nodes[node_idx].id;
                if self.doc.tree.nodes[node_idx].kind != NodeKind::Struct {
                    self.change_node_kind(node_idx, NodeKind::Struct);
                }
                let idx = self.doc.tree.index_of_id(node_id);
                if idx >= 0 {
                    let old_type_name = self.doc.tree.nodes[idx as usize].struct_type_name.clone();
                    if old_type_name != text {
                        self.push_command(Command::ChangeStructTypeName {
                            node_id,
                            old_name: old_type_name,
                            new_name: text.to_string(),
                        });
                    }
                }
            }
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
        // Build the PDB symbol-lookup callback (`controller.cpp:1911-1918`). The
        // C++ builds `symLookup` whenever a provider is attached and passes it
        // into compose in BOTH branches, so each hex/pointer row with no user
        // comment gets a `// module!symbol` annotation. The closure always
        // captures the REAL provider (`m_doc->provider`) — never the snapshot —
        // because module-base resolution must run against the live source.
        //
        // The headless build has no `g_nameLookupHook`, so this mirrors the C++
        // test-build fallback (`SymbolStore::getSymbolForAddress` directly). It
        // is gated on the `symbols` feature (where `SymbolStore` lives) and on a
        // real, non-`NullProvider` source (`provider.size() > 0`, the Rust
        // analogue of C++'s non-null `m_doc->provider`); without symbols loaded
        // `get_symbol_for_address` returns empty anyway, so output is unchanged.
        #[cfg(feature = "symbols")]
        let sym: compose::SymbolLookupFn<'_> = {
            use crate::rtti::symbol_store::SymbolStore;
            let has_syms = SymbolStore::global()
                .lock()
                .map(|s| s.has_symbols())
                .unwrap_or(false);
            if has_syms && self.doc.provider.size() > 0 {
                let prov = Arc::clone(&self.doc.provider);
                Some(Box::new(move |addr: u64| {
                    SymbolStore::global()
                        .lock()
                        .map(|s| s.get_symbol_for_address(addr, Some(&*prov)))
                        .unwrap_or_default()
                }) as Box<dyn Fn(u64) -> String>)
            } else {
                None
            }
        };
        #[cfg(not(feature = "symbols"))]
        let sym: compose::SymbolLookupFn<'_> = None;

        // Compose against snapshot if active, else real provider.
        self.last_result = if let Some(snap) = &self.snapshot {
            compose::compose_with_symbols(
                &self.doc.tree,
                snap.as_ref(),
                self.view_root_id,
                self.compact_columns,
                self.tree_lines,
                self.brace_wrap,
                self.type_hints,
                self.show_comments,
                sym,
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
                sym,
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

    /// Re-extend the multi-selection to cover `[anchor, to_line]`, restoring
    /// `anchor` as the range origin. Used by the editor's Shift+Down grow: after
    /// [`append_single_field`](Self::append_single_field) appends a tail row it
    /// collapses the selection to that one new field (the plain-Down behavior), so
    /// the extend path calls this to re-highlight every row from the original
    /// anchor down to the grown row (keeping the Shift-range consistent as the
    /// class expands).
    pub fn extend_selection_from(&mut self, anchor: i64, to_line: i64) {
        self.anchor_line = anchor;
        self.sel_ids.clear();
        self.insert_range(anchor, to_line);
        self.update_command_row();
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
                // `loadData(path)` clears the undo stack; the stack lives on the
                // controller in the Rust port so we clear it here.
                self.undo.clear();
                self.doc.load_data_file(&entry.file_path);
                // Restore the slot's *literal* saved base + formula. C++
                // (`controller.cpp:5228-5232`) deliberately does NOT reevaluate
                // the formula here — it keeps exactly what was saved — and does
                // NOT resetSnapshot in this branch; only `refresh()` follows
                // `loadData`.
                self.doc.tree.base_address = entry.base_address;
                self.doc.tree.base_address_formula = entry.base_address_formula.clone();
                self.refresh();
            }
        }
        // Non-File kinds (buffer / snapshot / live memflow process) are not
        // re-materialized by a saved-source switch — their original attach would
        // have to be re-invoked.
        self.on_document_changed();
    }

    /// Attach a binary data file as the active source, mirroring C++
    /// `RcxDocument::loadData(path)` (`controller.cpp:292`) which clears the
    /// undo stack and emits `documentChanged`. The undo stack lives on the
    /// controller in the Rust port, so the clear happens here; we then load the
    /// provider, reset the snapshot (the C++ `documentChanged`→`refresh`
    /// pipeline drops stale snapshot state), and notify listeners. This is the
    /// path used by the toolbar/menu "Attach Data File" action so it no longer
    /// bypasses `undo.clear()` / `reset_snapshot()`.
    pub fn attach_data_file(&mut self, path: impl AsRef<Path>) {
        self.undo.clear();
        self.doc.load_data_file(path);
        self.reset_snapshot();
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
        self.attach_provider_with_target(provider, register_as_saved, String::new());
    }

    /// Same attach bookkeeping as [`attach_provider`](Self::attach_provider), but
    /// preserves a provider-specific target string for live/plugin sources.
    pub fn attach_provider_with_target(
        &mut self,
        provider: Arc<dyn Provider + Send + Sync>,
        register_as_saved: bool,
        provider_target: String,
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
        // Re-evaluate a stored module-relative / `[ptr]` formula against the new
        // provider so a project saved with a module-relative base relocates to the
        // new module load address (`controller.cpp:5167-5208`).
        self.reevaluate_base_address_formula();
        self.reset_snapshot();
        if register_as_saved {
            // Dedup on (kind, providerTarget).
            let pos = self
                .saved_sources
                .iter()
                .position(|s| s.kind == kind && s.provider_target == provider_target);
            let entry = SavedSourceEntry {
                kind: kind.clone(),
                display_name: name,
                provider_target,
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

mod refresh;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod save_format_tests {
    use super::*;

    /// `RcxDocument::save` must mirror `QJsonDocument::toJson(Indented)`:
    /// 4-space indentation and a trailing newline (`controller.cpp:201`).
    #[test]
    fn save_uses_four_space_indent_and_trailing_newline() {
        let mut doc = RcxDocument::new();
        doc.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Foo".into(),
            ..Default::default()
        });
        let dir = std::env::temp_dir().join(format!("rcx_savefmt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fmt.rcx");
        assert!(doc.save(&path));
        let text = std::fs::read_to_string(&path).unwrap();
        // Trailing newline.
        assert!(text.ends_with('\n'), "missing trailing newline");
        // Top-level keys are indented by exactly 4 spaces (not serde's 2).
        assert!(
            text.contains("\n    \"baseAddress\""),
            "expected 4-space indent for top-level keys, got:\n{text}"
        );
        // Never a 2-space-only indent for a top-level key.
        assert!(
            !text.contains("\n  \"baseAddress\""),
            "found 2-space indent — should be 4"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
