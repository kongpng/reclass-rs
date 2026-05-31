# PORTING SPEC — Editor controller / interaction logic (`controller`)

Function-level porting spec for the C++ `src/controller.{h,cpp}` (and its two test files
`tests/test_controller.cpp`, `tests/test_refresh_speedups.cpp`) into idiomatic Rust.

Inputs consumed for this spec: `_design/understand/controller.md` (behavioral map — authoritative),
`_design/ARCHITECTURE.md` (module layout), `_design/crate_selection.md` (deps), `_oracle/RESULTS.md`
(oracle status), `_design/gpui_cookbook.md` + `_design/gpui_component_cookbook.md` (UI integration),
and the C++ source/tests/provider headers.

> **Refresh-interval correction (carried from controller.md):** the default base refresh interval is
> **200 ms** (`kDefaultRefreshMs`), NOT 660 ms. The blur/max caps are `max(base, 1500)`. (660 appears
> only in the *Options dialog spin box* default, a separate UI concern — see tests-catalog.md §
> test_options_dialog — and does not belong in the controller.)

---

## 0. Target crate / module(s)

Single package `reclass`, module **`src/controller.rs`** (see ARCHITECTURE.md §2/§3). Feature gating:
the controller core is **`always`** (no `ui`, no gpui). It depends only on `core`, `compose`, `format`,
`addr`, and the `provider` module — all `always`. This lets `cargo test --no-default-features` exercise
the entire command/undo/refresh/value-history/selection/source-management logic headlessly, which is
exactly how the C++ oracle logic is best mirrored.

Proposed sub-module split inside `controller.rs` (or a `controller/` dir if it grows — keep it one
file unless > ~2k lines):

```
src/controller.rs
  ├─ mod document      // RcxDocument: NodeTree + provider + undo stack + dirty/aliases + .rcx save/load
  ├─ mod command       // Command enum (18 variants) + apply_command + UndoStack + RcxCommand obsolete logic
  ├─ mod refresh       // refresh pipeline, async tick, snapshot diff, value tracking, adaptive interval
  ├─ mod selection     // m_sel_ids decoration/strip, handle_node_click, command-row text
  ├─ mod sources       // SavedSourceEntry, switch/select/clear/copy, base-address policy
  └─ mod mutations     // tree-mutation helpers (rename/insert/remove/changeKind/split/join/union/...)
```

**Threading separation (key design decision):** the headless controller does NOT own a real OS timer
or thread pool. Instead it exposes the refresh state machine as pure, synchronously-callable methods
(`on_refresh_tick`, `on_read_complete(pages)`, `read_pages(addrs) -> PageMap`) plus the adaptive-interval
calculator. The `ui`/`main` layer drives them: a GPUI `Timer`/`background_executor` fires `on_refresh_tick`,
which returns a *plan* (the list of page addresses to read); the UI spawns a background read of those pages
and feeds the result back via `on_read_complete`. The C++ entangles `QTimer`/`QFutureWatcher` into the
controller; the Rust port splits the *policy* (testable, headless) from the *transport* (UI). The
`test_refresh_speedups.cpp` tests drive the tick/complete cycle directly, so this split is also what makes
them portable as `#[test]`s without a window.

---

## 1. External-crate replacements (summary table)

| C++ / Qt construct | Rust replacement | Crate |
|---|---|---|
| `QHash<K,V>` (pages, value history, stability, lastValueAddr) | `HashMap<K,V>` with `ahash::AHasher` (`type FxMap<K,V> = HashMap<K,V,BuildHasherDefault<AHasher>>`) | std + ahash |
| `QSet<T>` (selection ids, changed offsets, visited, permanent pages) | `HashSet<T, ahash>` | std + ahash |
| `QVector<T>` / `QList<T>` (nodes, ranges, editors, saved sources) | `Vec<T>` | std |
| `QByteArray` (page bytes, value bytes) | `Vec<u8>` | std |
| `QString` (names, formulas, labels) | `String` | std |
| `std::variant<...>` (Command) | `enum Command { … }` | std |
| `std::shared_ptr<Provider>` | `Arc<dyn Provider>` (must be `Send + Sync`; reads `&self`) | std + provider module |
| `std::unique_ptr<SnapshotProvider>` | `Option<Box<SnapshotProvider>>` (or `Option<Arc<SnapshotProvider>>` when the read worker also needs it) | std |
| `QUndoStack` / `QUndoCommand` / `setObsolete` / macros | hand-rolled `UndoStack` (see §4) | — |
| `QTimer` | UI-layer GPUI timer; controller exposes `adaptive_interval_ms()` + state | gpui (ui only) |
| `QFutureWatcher<PageMap>` + `QtConcurrent::run` | `cx.background_executor().spawn(...)` + channel back to controller entity (ui); in headless tests, call `read_pages` synchronously then `on_read_complete` | gpui (ui) / direct (test) |
| `QSettings("Reclass","Reclass")` key `refreshMs` (default 200) | config file via `directories` + serde (read in `ui`/`main`); controller takes the value as a ctor/setter arg | directories, serde |
| `QJsonObject/Array/Document` (`.rcx` save/load) | `serde_json` (see `core` module's Node/NodeTree serde) | serde_json |
| `QFile`/`QDir`/`QFileInfo` (load/save, relative path resolution) | `std::fs`, `std::path::{Path,PathBuf}` | std |
| `Qt::KeyboardModifiers` (Ctrl/Shift) | `struct Modifiers { ctrl: bool, shift: bool }` | — |
| `thread_local s_composeDoc` + `ComposeDocGuard` | **pass the alias map / doc explicitly into `compose`** (no thread-local) | — |
| `qWarning`/`qDebug` | `tracing::{warn,debug}` | tracing |
| Qt signals (`nodeSelected`, `statusHint`, …) | an `enum ControllerEvent { … }` returned/pushed to an observer (see §9) | — |

No new heavy crates beyond what `crate_selection.md` already lists. `directories` is already selected.

---

## 2. Provider trait & built-in providers (in scope)

The controller talks to data sources **only** through the `Provider` trait defined in
`src/provider/mod.rs` (ARCHITECTURE.md §7). This spec assumes that trait exists; here we pin the exact
surface the controller relies on and the behaviors of the four built-in providers it constructs.

### 2.1 `Provider` trait (the subset the controller uses)

The controller calls: `read`, `size`/`is_valid`, `write`/`is_writable`, `is_readable`, `name`, `is_live`,
`kind`, `pointer_size`, `base`, `symbol_to_address`, `enumerate_regions`, plus the LE convenience readers
`read_u32`/`read_u64` and `read_bytes`. Because the snapshot read worker shares the `Arc<dyn Provider>`
across threads, the trait must be `Send + Sync`. The C++ `write` is non-`const`; in Rust model writes as
`fn write(&self, addr, &[u8]) -> Result<usize>` with **interior mutability** inside writable providers
(`BufferProvider` holds `Mutex<Vec<u8>>` or `RwLock`), so a single `Arc<dyn Provider>` can be both read on
the worker and written on the main thread. (Alternative — split `ReadProvider`/`WriteProvider` traits — is
discouraged for parity since the snapshot write-through path needs both on one object.)

Behavior pins (must match `provider.h` defaults):
- `is_valid()` ⇔ `size() > 0`.
- `is_readable(addr, len)`: `len < 0`→false; `len == 0`→true; else `addr <= size && (len as u64) <= size - addr`.
- `read_bytes(addr, len)`: `len <= 0`→empty Vec; allocate `len`; on read failure fill with `0`. (Never errors out — returns zeroed bytes. The controller relies on this.)
- Defaults: `is_writable=false`, `is_live=false`, `kind="File"`, `pointer_size=8`, `base=0`, `name=""`.
- `MemoryRegion { base, size, readable, writable, executable, module_name: String, type: RegionType }`; `RegionType { Image=0, Mapped=1, Private=2 }`.

### 2.2 `BufferProvider` (`src/provider/buffer.rs`) — file + in-memory buffer

```rust
pub struct BufferProvider { data: RwLock<Vec<u8>>, name: String }
```
- `size()` = data len; `read`/`write` bounds-checked `copy_from_slice` (return `false`/error out of range);
  `is_writable()` = **true**; `kind()` = `"File"`; `is_live()` = **false** (so auto-refresh never ticks for
  file/buffer sources); `name()` = ctor name.
- `enumerate_regions()`: empty data → `[]`; else one `Mapped` region `{ base:0, size:len, readable:true,
  writable:true, executable:false, module_name: name (or "[buffer]" if empty) }`.
- `from_file(path)`: open RO, read all → `BufferProvider(bytes, basename(path))`; on failure → empty buffer
  (invalid). (Mirrors `BufferProvider::fromFile`.)

### 2.3 `NullProvider` (`src/provider/null.rs`)
`size()=0`, `read`→false, `name()=""` (→ command row shows `<Select Source>`), `kind()="File"`. Default
provider a fresh `RcxDocument` holds (`Arc::new(NullProvider)`).

### 2.4 `SnapshotProvider` (`src/provider/snapshot.rs`) — page-table-backed

```rust
pub struct SnapshotProvider {
    real: Option<Arc<dyn Provider>>,
    pages: HashMap<u64, Box<[u8; 4096]>>,   // page-aligned addr → 4 KiB page
    main_extent: i32,
    permanent: HashSet<u64>,
}
const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = !(PAGE_SIZE - 1);
```
Port the EXACT semantics from `snapshot_provider.h`:
- `read(addr, buf)`: empty buf → false. Walk page by page; for each chunk: if page present, copy from it at
  `page_off`; else if `real` is `Some`, fall through to `real.read(cur, ..)` and **zero-fill on failure**;
  else zero-fill. Always returns `true`. (The real fall-through is required by RTTI hint reads of
  vtable/.rdata bytes not pre-fetched — keep it even though RTTI is feature-gated; the snapshot lives in
  `provider` which is always-on.)
- `is_readable(addr, len)`: `len <= 0`→`len == 0`; overflow guard (`end < addr`→false); for each page in
  range: if absent from `pages`, defer to `real.is_readable(addr, len)` (true if real says so), else continue;
  all present → true.
- `size()` = `main_extent`. `is_writable/is_live/name/kind/pointer_size/base/symbol_to_address/
  enumerate_modules/enumerate_regions/peb/tebs` forward to `real` (or default if `None`).
- `write(addr, buf)`: `real` `None`→false; else `real.write(..)`; on success `patch_pages(addr, buf)`.
- `update_pages(pages, extent)` (replace whole table — used only by tests/construction);
  `merge_pages(fresh, extent)`: insert each fresh page over existing; set extent. **Pages not in `fresh`
  keep old bytes** — KEY invariant for viewport-bounded reads (tested by `snapshotProviderMergeKeepsExisting`).
- `mark_permanent(p)` / `is_permanent(p)` (both page-align input via `& PAGE_MASK`) / `clear_permanent()`.
- `patch_pages(addr, buf)`: overwrite bytes in **existing** pages only (no allocation for missing).
- accessors `pages()`, `permanent_pages()`.

> Page storage choice: `Box<[u8;4096]>` (fixed-size, no per-page len bookkeeping) matches the C++ always-4096
> page model and avoids `min(len)` partial-page edge cases on read; the diff in §5.3 still tolerates short
> reads defensively. If a future provider returns a short page, the read worker must pad to 4096 before
> handing to the snapshot.

---

## 3. Data structures (with serde decisions)

### 3.1 `RcxDocument` (← `controller.h:27`)

```rust
pub struct RcxDocument {
    pub tree: NodeTree,                       // from core
    pub provider: Arc<dyn Provider>,          // default Arc::new(NullProvider)
    pub undo: UndoStack,                       // §4
    pub file_path: Option<PathBuf>,            // .rcx path
    pub data_path: Option<PathBuf>,            // attached binary path (cleared when non-file provider attaches)
    pub modified: bool,
    pub type_aliases: HashMap<NodeKind, String>,  // per-kind display-name overrides
    // owned RW buffer for self-attach tutorial — out of scope but field kept for fidelity:
    pub owned_buffer: Option<Vec<u8>>,         // nullptr-equivalent = None
    pub pending_saved_sources: Vec<serde_json::Value>, // raw saved-source JSON from load, consumed once
    pub load_overlap_count: usize,
}
```
- `resolve_type_name(kind)`: `type_aliases.get(kind)` if non-empty, else `kind_meta(kind).type_name`, else `"???"`.
- **`documentChanged` signal** → modeled as a `ControllerEvent::DocumentChanged` (or a direct call into
  `refresh()` — see §9). In the C++ this is `connect(doc, documentChanged, this, refresh)`.
- **`cleanChanged` → modified**: the undo stack's clean-index tracking drives `modified` (§4).

Serde: `RcxDocument` itself is NOT serialized; only `tree.to_json()` + `type_aliases` + (UI-written)
`savedSources` go into the `.rcx`. The serde is in `core` (NodeTree) and here for `typeAliases`/`savedSources`.

Methods (exact behavior — important):
- `compose(view_root_id, opts) -> ComposeResult`: thin wrapper over `compose::compose(&tree, &*provider, …)`.
  **Pass the `type_aliases` (or `&self`) explicitly** so `compose`/`format` resolve display names without a
  thread-local (replaces `fmt::setTypeNameProvider` + `ComposeDocGuard`).
- `save(path) -> io::Result<bool>`: `json = tree.to_json()`; if `!type_aliases.is_empty()` add a
  `"typeAliases"` object (keys = `kind_to_string(kind)`, values = alias); `serde_json::to_string_pretty`;
  write; on success set `file_path = Some(path)`, `undo.set_clean()`, `modified=false`, return true.
- `load(path) -> bool`: replicate the 8-step forgiving load:
  1. open RO; failure → false.
  2. read all bytes. **Empty file → fresh empty project** (treat as empty JSON object). Non-empty MUST parse
     as a JSON object; if `serde_json::from_slice::<Value>` is `Err` or not an object → `tracing::warn!` +
     **return false** (refuse to swallow raw binaries).
  3. `undo.clear()`; `tree = NodeTree::from_json(&root)`.
  4. `tree.validate(repair=true)` (re-root orphans, break cycles, renumber dup ids); log if dirty.
  5. `tree.find_overlaps()`; store count in `load_overlap_count`; log up to 5 pairs as warnings.
  6. load `typeAliases` (kind_from_string keys, non-empty string values).
  7. `pending_saved_sources = root["savedSources"].as_array()`; resolve **relative** `filePath` entries
     against the `.rcx` directory (`rcx_dir.join(rel)` → canonical/absolute).
  8. set `file_path`, `modified=false`, emit `DocumentChanged`, return true.
- `load_data_file(binary_path)`: open RO (silent return on failure); `undo.clear()`;
  `provider = Arc::new(BufferProvider::from_file(path))`; `data_path = Some(binary_path)`;
  `tree.base_address = 0`; emit `DocumentChanged`.
- `load_data_bytes(data, name)`: same but from in-memory bytes, no `data_path`.

### 3.2 `Command` enum (← `core.h:1080-1115`, `controller.h`)

One Rust enum, 18 variants (POD; each carries old+new for bidirectional apply). Lives in `core` if shared,
else here. Field-for-field:

```rust
pub struct OffsetAdj { pub node_id: u64, pub old_offset: i64, pub new_offset: i64 }

pub enum Command {
    ChangeKind        { node_id: u64, old_kind: NodeKind, new_kind: NodeKind, off_adjs: Vec<OffsetAdj> },
    Rename            { node_id: u64, old_name: String, new_name: String },
    Collapse          { node_id: u64, old_state: bool, new_state: bool },
    Insert            { node: Node, off_adjs: Vec<OffsetAdj> },
    Remove            { node_id: u64, subtree: Vec<Node>, off_adjs: Vec<OffsetAdj> },
    ChangeBase        { old_base: u64, new_base: u64, old_formula: String, new_formula: String },
    WriteBytes        { addr: u64, old_bytes: Vec<u8>, new_bytes: Vec<u8> }, // ONLY cmd touching provider
    ChangeArrayMeta   { node_id: u64, old_element_kind: NodeKind, new_element_kind: NodeKind,
                        old_array_len: i32, new_array_len: i32 },
    ChangePointerRef  { node_id: u64, old_ref_id: u64, new_ref_id: u64 },
    ChangeStructTypeName { node_id: u64, old_name: String, new_name: String },
    ChangeClassKeyword   { node_id: u64, old_keyword: String, new_keyword: String },
    ChangeOffset      { node_id: u64, old_offset: i64, new_offset: i64 },
    ChangeEnumMembers { node_id: u64, old_members: Vec<(String, i64)>, new_members: Vec<(String, i64)> },
    ChangeOffsetExpr  { node_id: u64, old_expr: String, new_expr: String },
    ToggleStatic      { node_id: u64, old_val: bool, new_val: bool },
    ToggleRelative    { node_id: u64, old_val: bool, new_val: bool }, // NOTE: no apply arm in C++ — preserve as no-op (§4.2)
    ToggleBigEndian   { node_id: u64, old_val: bool, new_val: bool },
    ChangeComment     { node_id: u64, old_comment: String, new_comment: String },
}
```
Helpers: `is_transient(&Command) -> bool` ⇔ `matches!(c, Command::WriteBytes{..})`.
`Insert{node, off_adjs}` and `Remove{node_id}` need ergonomic constructors so tests can write
`Command::insert(node)` / `Command::remove(id)` with empty `off_adjs`/`subtree` (the C++ tests use
`cmd::Insert{sf, {}}` and `cmd::Remove{sfId}` — the latter computes the subtree at apply time? NO: the C++
`cmd::Remove{sfId}` default-constructs empty `subtree`/`offAdjs`; on redo it walks `subtreeIndices` live and
on undo re-adds whatever was captured. **Pin:** the `Remove` test `testDeleteStaticFieldPreservesStructSize`
constructs `cmd::Remove{sfId}` with empty subtree and pushes it; redo removes the live subtree; this works
because redo recomputes indices from the tree, and the subtree vec is only consulted on *undo*. Replicate:
`Command::Remove::new(id)` → empty subtree/adjs.)

Serde: `Command` is **not serialized** (undo history is in-memory only). No serde derive needed.

### 3.3 `SavedSourceEntry` (← `controller.h:99`)

```rust
pub struct SavedSourceEntry {
    pub kind: String,                 // "File" or provider id (e.g. "processmemory")
    pub display_name: String,
    pub file_path: String,            // for File sources
    pub provider_target: String,      // for plugin providers
    pub base_address: u64,            // stored hex in JSON
    pub base_address_formula: String,
}
```
JSON read/write (the UI writes `savedSources`; controller reads/lifts): `base_address` is a **hex string**
(`format!("{:x}", base)` / parse with `u64::from_str_radix(s.trim_start_matches("0x"), 16)`).

### 3.4 `ValueHistory` (← `core.h:867`) — heatmap ring buffer

Belongs in `core` (shared with editor/compose) but heavily exercised by controller tests. Pin exactly:

```rust
pub struct ValueHistory {
    values: [String; 10],
    timestamps: [i64; 10],   // ms since epoch
    pub count: i32,          // total recorded; saturates at i32::MAX (NOT capped at 10)
    head: usize,             // next write index
}
const CAPACITY: usize = 10;
```
- `record(v)`: if `count > 0` and `last()` == `v` → **no-op** (dedup consecutive duplicates). Else store
  `v` + `now_ms()` at `head`; `head = (head+1) % 10`; `count = count.saturating_add(1)`.
- `clear()`: `count = 0; head = 0` (array contents not erased).
- `unique_count()` = `min(count, 10)`.
- `heat_level()`: `count <= 1 → 0` (static); `== 2 → 1` (cold); `<= 4 → 2` (warm); else `3` (hot).
- `last()`: value at `(head + 9) % 10`, or `""` if `count == 0`.
- `for_each(fn)`: oldest→newest over `unique_count` entries. `for_each_with_time(fn)`: newest→oldest with ts.

`now_ms()`: `SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64`. (Tests don't assert exact
timestamps; only ordering/heat.)

### 3.5 `RcxController` private state (← `controller.h:267-408`)

```rust
pub struct RcxController {
    // View / selection
    editors: Vec<EditorHandle>,          // see §9 — abstracted editor refs (Vec, primary = first)
    last_result: ComposeResult,
    sel_ids: HashSet<u64>,               // DECORATED ids (§7)
    anchor_line: i64,                    // -1 = none
    view_root_id: u64,                   // 0 = first root struct
    // display toggles
    compact_columns: bool, tree_lines: bool, brace_wrap: bool, type_hints: bool,
    show_comments: bool,                 // default false
    show_rtti: bool,                     // default true
    show_enum_chips: bool,               // default true
    suppress_refresh: bool,
    read_only_override: bool,
    // saved sources
    saved_sources: Vec<SavedSourceEntry>,
    active_source_idx: i32,              // -1 = none
    last_live: bool,
    // auto-refresh state
    snapshot: Option<Box<SnapshotProvider>>,
    prev_pages: PageMap,                 // accumulated previous-tick bytes for diffing
    changed_offsets: HashSet<i64>,       // changed byte offsets relative to baseAddress this tick
    value_history: HashMap<u64, ValueHistory>,
    last_value_addr: HashMap<u64, u64>,  // nodeId → last offsetAddr used for recording
    track_values: bool,                  // default true
    value_track_cooldown: i32,           // suppress recording for N cycles after clear
    refresh_gen: u64,                    // bumped on layout-invalidating edits
    read_gen: u64,                       // captured at read launch
    read_in_flight: bool,
    // refresh speedups
    page_stability: HashMap<u64, i32>,   // page-aligned addr → stability count
    tick_count: u64,
    idle_ticks: i32,
    refresh_interval_base_ms: i32,       // 200
    refresh_interval_max_ms: i32,        // 1500
    refresh_interval_blur_ms: i32,       // 1500
    window_focused: bool,                // true
    window_visible: bool,                // true
    // refresh timing exposed for UI to set the OS timer
    current_interval_ms: i32,            // mirrors the QTimer interval; 0 = stopped
    timer_active: bool,                  // mirrors QTimer::isActive
    // cross-tab type visibility
    project_docs: Option<...>,           // see core/project model; pointer to project doc list
    // events out
    events: Vec<ControllerEvent>,        // §9 (or a callback sink)
}
type PageMap = HashMap<u64, Vec<u8>>; // worker result; pages padded to 4096
const STABILITY_THRESHOLD: i32 = 5;
const IDLE_BACKOFF_TICKS: i32 = 8;
const POINTER_SNAPSHOT_BYTE_BUDGET: i64 = 64 * 1024 * 1024;
```
The C++ holds a raw `RcxDocument* m_doc`. In Rust the controller borrows/owns the document: the simplest
faithful model is `RcxController` owns `RcxDocument` (or holds `Rc<RefCell<RcxDocument>>` if the UI shares
it). For the headless tests the controller can own the doc directly. **Decision:** `RcxController` holds the
doc by value/`&mut` per call; tests build `(RcxDocument, RcxController)` and call methods that take
`&mut self` plus `&mut doc` — OR fold the doc into the controller. To match the C++ test ergonomics
(`m_doc->tree.nodes`, `m_ctrl->renameNode`, `m_doc->undoStack.undo`) the cleanest is:

> **Document ownership decision:** `RcxController` **owns** the `RcxDocument` (`doc: RcxDocument`), exposing
> `doc()` / `doc_mut()`. The undo stack lives in the doc but is driven by the controller (`apply_command`).
> Tests access `ctrl.doc().tree.nodes[i]` and call `ctrl.doc_mut().undo.undo(&mut ctrl)` — to avoid the
> aliasing problem (undo needs `&mut ctrl` while borrowing `ctrl.doc`), the undo stack is moved OUT of the
> doc into the controller as `undo: UndoStack` and the C++ `m_doc->undoStack.undo()` is mirrored by a thin
> `ctrl.undo()` / `ctrl.redo()`. The `.rcx` clean-index still reflects dirty state via `ctrl.undo`.

---

## 4. Undo stack (`UndoStack`) — replaces `QUndoStack`/`QUndoCommand`

### 4.1 Structure & semantics

`QUndoStack` semantics the port must reproduce:
- `push(cmd)` **immediately executes** `redo()` (i.e. applies the command forward). Truncates any redo tail.
- `undo()` / `redo()` move the current index and apply backward/forward.
- `begin_macro(text)` / `end_macro()`: group pushes into one undoable unit (nested-macro depth supported;
  pushes during a macro execute immediately but undo/redo treat the group atomically).
- `set_clean()` marks the current index clean; `is_clean()` ⇔ current index == clean index → drives
  `doc.modified`. `clear()` resets stack + clean index.
- `set_obsolete(true)` on a just-(un/re)done command **drops it from the stack** (used when a non-transient
  apply fails — see §4.3).

```rust
pub struct UndoStack {
    entries: Vec<Entry>,          // Entry = single Command OR a macro group of Commands
    index: usize,                 // # of applied entries (0..=entries.len())
    clean_index: Option<usize>,
    macro_stack: Vec<Vec<Command>>, // open macros (nested)
}
enum Entry { One(Command), Macro { text: String, cmds: Vec<Command> } }
```
Because the apply function needs `&mut RcxController`, `push`/`undo`/`redo` take `&mut RcxController` as an
argument (or are methods on the controller that delegate to an inner stack). Pseudocode:

```
fn push(ctrl, cmd):
    if macro open: execute cmd (apply_command forward, handle obsolete); append to top macro buffer; return
    truncate entries[index..]
    ok = ctrl.apply_command(&cmd, isUndo=false)
    if !ok && !is_transient(cmd): return        // obsolete-on-push: do NOT add to stack
    entries.push(One(cmd)); index += 1
    update_clean()

fn undo(ctrl):
    if index == 0: return
    entry = entries[index-1]
    for cmd in entry.cmds().rev():               // macro: undo in reverse order
        ok = ctrl.apply_command(cmd, isUndo=true)
        if !ok && !is_transient(cmd): mark_entry_obsolete   // drop on next compaction
    index -= 1; update_clean()

fn redo(ctrl): symmetric, forward order, index += 1
```

> **Obsolete simplification:** the C++ `setObsolete(true)` removes the command from the stack at the moment
> the failed (un/re)do runs. Faithful Rust: when a non-transient apply returns false during undo/redo, drop
> that entry from `entries` and re-derive `index` (the entry effectively never existed). Transient
> (`WriteBytes`) failures are KEPT (a later redo on a re-attached writable provider can succeed). This is the
> rationale documented at `controller.cpp:336-353`.

### 4.2 `apply_command(&mut self, cmd: &Command, is_undo: bool) -> bool`

Direct port of `controller.cpp:2827`. Returns `false` only when the underlying op was rejected (today only
`WriteBytes` failure, or `read_only_override` blocking a write). Algorithm (verified against source):

1. `self.doc.tree.touch()` (bump generation) — EVERY command, including WriteBytes/ChangeBase.
2. Two closures (or inline helpers):
   - `clear_node_history(id)`: remove from `value_history` and `last_value_addr`.
   - `clear_history_for_adjs(adjs)`: if empty → return. **`self.refresh_gen += 1`** (discard in-flight read
     whose layout is now stale). For each adj: `clear_node_history(adj.node_id)`; detect if any adjusted node
     is a container. If so build a childMap once and BFS-clear all descendants' histories (visited set,
     stack). (Matches the O(N²)-avoidance in the C++.)
3. `match cmd` (choose old/new by `is_undo`):
   - **ChangeKind**: `nodes[idx].kind = if is_undo {old} else {new}`; apply each `off_adj.offset`;
     `refresh_gen += 1`; `clear_history_for_adjs(off_adjs)`. **Value history is intentionally KEPT across kind
     changes** (do NOT wipe per-node history — the hover trend matters right when a TypeHint is accepted).
   - **Rename / Collapse / ChangeStructTypeName / ChangeClassKeyword / ChangeEnumMembers / ChangeOffsetExpr /
     ToggleStatic / ToggleBigEndian / ChangeComment**: set the single field.
   - **Insert**: undo → revert off_adjs then `remove(node.id)` (+ invalidate id cache); redo →
     `tree.add_node(node)` then apply off_adjs. Then `clear_history_for_adjs(off_adjs)`.
   - **Remove**: undo → re-add `subtree` nodes, then revert off_adjs; redo → apply off_adjs FIRST, then
     `subtree_indices(node_id)` sorted **descending** (so removals don't invalidate earlier indices),
     `clear_node_history` each, remove each; `invalidate_id_cache()`. Then `clear_history_for_adjs(off_adjs)`.
   - **ChangeBase**: set `base_address` + `base_address_formula`; `reset_snapshot()`.
   - **WriteBytes**: `bytes = if is_undo {old} else {new}`. **If `read_only_override` → `success=false`,
     return** (refuse even on undo/redo). Else write through `snapshot` if present else `doc.provider`. On
     failure: `tracing::warn!`, push `ControllerEvent::StatusHint("Write rejected at 0x{addr:x} — removing
     from history")`, `success=false`.
   - **ChangeArrayMeta**: set `element_kind` + `array_len`; clamp `view_index` to `< array_len`
     (`view_index = max(0, array_len-1)` if too large).
   - **ChangePointerRef**: set `ref_id`; if non-zero force `collapsed=true`.
   - **ChangeOffset**: set `offset`; `refresh_gen += 1`; clear this node's + all subtree nodes' history.
   - **ToggleRelative**: **NO ARM** — currently a no-op in C++. **Preserve as a no-op for fidelity, with a
     `// FIXME-parity:` comment** noting it is a latent gap (the value flag never actually toggles via this
     command). Document so a future fix is deliberate.
4. If `success && !suppress_refresh` → `self.refresh()`. Return `success`.

Invariant: a failed `WriteBytes` runs **no refresh** (UI keeps last good state).

### 4.3 `RcxCommand` analog
There is no separate `RcxCommand` type in Rust — the obsolete-on-failure logic lives inside `UndoStack::undo`/
`redo`/`push` (§4.1) consulting `apply_command`'s return + `is_transient`.

---

## 5. The refresh pipeline

### 5.1 `refresh(&mut self)` (← `controller.cpp:1891`) — synchronous recompose + push to editors

The central rebuild. Steps (port verbatim; the thread-local doc/`ComposeDocGuard` is replaced by passing the
doc/alias map into `compose`):
1. Build a `sym_lookup` closure: if a name-lookup hook is installed (GUI), call it; else fall back to the
   symbol store (`symbols` feature) — when `--no-default-features`, the hook/store are absent → `sym_lookup`
   returns empty (tests leave it null).
2. **Compose**: if `self.snapshot` is `Some`, `compose::compose(&tree, &**snapshot, view_root_id, …,
   show_rtti, show_enum_chips)`; else `doc.compose(view_root_id, …)` (the doc wrapper, which doesn't pass
   show_rtti/show_enum_chips — they default). Store in `self.last_result`.
3. **Change-highlight pass** (only if `changed_offsets` non-empty): build a childMap once. For each `LineMeta`
   with valid `node_idx`: `offset = lm.offset_addr - tree.base_address`.
   - Hex-preview nodes: per-byte — for `b in 0..lm.line_byte_count`, if `changed_offsets.contains(offset + b)`
     push `b` into `lm.changed_byte_indices` and set `lm.data_changed = true`.
   - Else: `sz = struct_span (containers) | byte_size (leaves)`; if any byte in `[offset, offset+sz)` is in
     `changed_offsets`, set `lm.data_changed = true` (break).
4. **Value-tracking pass**: pick `prov` = snapshot if `is_live()`, else real provider if `is_valid() &&
   is_live()`, else none. Decrement `value_track_cooldown` if > 0. If `track_values && prov.is_some() &&
   cooldown <= 0`: for each `LineMeta` that is a non-synthetic, non-continuation `Field` line of a
   non-container, non-FuncPtr node: `addr = lm.offset_addr`, `sz = node.byte_size()`. Skip if `sz <= 0` or
   `!prov.is_readable(addr, sz)`. `val = format::read_value(node, prov, addr, lm.sub_line)`. If non-empty:
   **if `last_value_addr[node_id] != addr`** remove its history (stale, address changed);
   `last_value_addr.insert(node_id, addr)`; `value_history.entry(node_id).record(val)`;
   `lm.heat_level = value_history[node_id].heat_level()`.
5. **Prune stale selections**: for each id in `sel_ids`, strip decoration bits (§7) and check
   `tree.index_of_id(node_id).is_some()`; keep the original (decorated) id if the base node still exists.
6. Collect `custom_types` = unique non-empty `struct_type_name`s of Struct nodes (for the type picker).
7. Resolve `snap_prov`/`real_prov` for the disasm popup (UI only).
8. **Apply to each editor** (UI): `set_custom_type_names`, `set_value_history_ref(&value_history)`,
   `set_provider_ref(snap, real, &tree)`, save view state, `apply_document(last_result)`, restore view state.
9. **Tail** (UI): `push_saved_sources_to_editors()`, `update_command_row()`, `apply_selection_overlays()`
   (text-modifying passes before overlays).

In the **headless** controller, steps 1–6 are pure and fully testable (they mutate `last_result`,
`value_history`, `sel_ids`). Steps 7–9 are UI and behind the editor abstraction (§9) — no-ops when no editor
is attached. `refresh()` is invoked from: `DocumentChanged`, `apply_command` (success && !suppress),
`on_read_complete` (only when bytes changed or first snapshot), `set_view_root_id`, all display toggles,
`set_track_values(false)`, source-switch paths.

### 5.2 `on_refresh_tick(&mut self) -> RefreshPlan` (← `controller.cpp:6586`)

Returns a plan describing which pages (if any) to read; the UI launches the background read, or in tests the
caller immediately calls `read_pages(plan.pages)` → `on_read_complete(result)`.

```
fn on_refresh_tick(&mut self) -> RefreshPlan:
  1. nowLive = provider.is_valid()                  // liveness flip detection runs BEFORE early returns
     if nowLive != self.last_live: self.last_live = nowLive; emit SourceLivenessChanged(nowLive)
  2. early-return RefreshPlan::None if: read_in_flight || !provider || !provider.is_live()
        || suppress_refresh || any editor.is_editing()
  3. tick_count += 1; extent = compute_data_extent(); if extent <= 0 return None
  4. ranges = [(base_address, extent)]
     if snapshot.is_some(): collectPointerRanges(rootId=view_root_id or first node id, base_address,
                                                  0, maxDepth=99, &mut visited, &mut ranges,
                                                  budget = POINTER_SNAPSHOT_BYTE_BUDGET - extent)
  5. firstSnapshot = snapshot.is_none() || prev_pages.is_empty()
  6. viewport = if !firstSnapshot { viewport_address_range() } else { None }
  7. request_pages: HashSet<u64> = {}
     for (rangeStart, rangeLen) in ranges:
        for p in pages_in(rangeStart & MASK ..= ceil(rangeStart+rangeLen) & MASK step 4096):
            if snapshot.is_permanent(p): continue                       // Speedup 4
            if let Some((vlo, vhi)) = viewport:
                lo = (vlo.saturating_sub(2*4096)) & MASK
                hi = ceil(vhi + 2*4096) & MASK                          // 2-page overscan each side
                in_viewport = lo <= p && p < hi
                is_main_range = rangeStart == base_address
                if is_main_range && !in_viewport:
                    stab = page_stability.get(p).copied().unwrap_or(0)
                    if stab >= STABILITY_THRESHOLD && (tick_count & 1 == 1):  // Speedup 2: half-rate odd ticks
                        continue
            request_pages.insert(p)
  8. if request_pages.is_empty():
        idle_ticks += 1; apply_adaptive_interval(); return RefreshPlan::None  // snapshot already on screen
  9. read_in_flight = true; read_gen = refresh_gen
     return RefreshPlan::Read { pages: request_pages.into_iter().collect(), provider: provider.clone() }
```
`RefreshPlan` is `enum { None, Read { pages: Vec<u64>, provider: Arc<dyn Provider> } }`. The provider clone
(`Arc`) is what the worker captures; reads are `&self` and `Send+Sync`.

`read_pages(provider, pages) -> PageMap` (the worker body): `pages.iter().map(|&p| (p,
provider.read_bytes(p, 4096))).collect()`. Pure; runs on background thread (or inline in tests). Each value
is padded/truncated to exactly 4096.

### 5.3 `on_read_complete(&mut self, new_pages: PageMap)` (← `controller.cpp:6698`)

```
fn on_read_complete(&mut self, new_pages):
  1. read_in_flight = false
  2. if read_gen != refresh_gen: return                  // generation guard — mutation invalidated this read
  3. (worker error already handled by caller; on Err log + return)
  4. ALL-ZERO GUARD: if !prev_pages.is_empty() and new_pages.get(&0) is all-zero:
        return                                            // process likely vanished mid-read — keep stale snapshot
  5. changed_offsets.clear(); firstSnapshot = prev_pages.is_empty(); any_changed = false
     for (page_addr, fresh) in &new_pages:
        match prev_pages.get(page_addr):
           None => { page_stability.insert(page_addr, 0); continue }   // first-sight is not a mutation
           Some(prev) =>
              changed_this_page = false
              for i in 0..min(prev.len(), fresh.len()):
                 if prev[i] != fresh[i]:
                    changed_offsets.insert((page_addr + i) as i64 - base_address? )  // NOTE: stored as absolute page_addr+i; see below
                    changed_this_page = true
              if changed_this_page: page_stability.insert(page_addr,0); any_changed = true
              else: stab = min(STABILITY_THRESHOLD+16, prev_stab+1)    // climbs, caps at 21
                    page_stability.insert(page_addr, stab)
  6. if any_changed: idle_ticks = 0 else if !firstSnapshot: idle_ticks += 1
     apply_adaptive_interval()
  7. main_extent = compute_data_extent()
  8. for (k,v) in &new_pages: prev_pages.insert(k, v.clone())          // accumulate
     match &mut self.snapshot:
        Some(s) => s.merge_pages(&new_pages, main_extent)
        None    => self.snapshot = Some(Box::new(SnapshotProvider::new(Some(provider.clone()),
                                                                        new_pages.clone(), main_extent)))
  9. classify_permanent_pages(&new_pages)                              // Speedup 4 — after snapshot exists
 10. if any_changed || firstSnapshot: self.refresh()
     changed_offsets.clear()                                          // always
```
> **changed_offsets storage detail:** the C++ stores `pageAddr + i` (absolute) into `m_changedOffsets` (a
> `QSet<int64_t>`), then in `refresh()` step 3 computes `offset = lm.offsetAddr - tree.baseAddress` and tests
> `changed_offsets.contains(offset + b)`. Since `lm.offsetAddr` is absolute, `offset + b` is RELATIVE to base.
> **Therefore the diff must store offsets RELATIVE to baseAddress too** (`changed_offsets.insert((page_addr +
> i) as i64 - base_address as i64)`). Verify against the C++: in `onReadComplete` it inserts the *absolute*
> `pageAddr+i`, but in `refresh` it adds `offset = lm.offsetAddr - baseAddress` to `b` (relative). That is a
> mismatch UNLESS baseAddress is 0 in the change-highlight tests. **PIN:** preserve the C++ exactly — store
> `pageAddr + i` (absolute) in `changed_offsets`, and in refresh test `changed_offsets.contains(offset + b)`
> where `offset = lm.offset_addr - base_address`. (For non-zero base this only highlights when base==0; that
> is the existing C++ behavior — do NOT "fix" it, just replicate. There is no oracle test asserting change
> highlight with non-zero base, so parity = byte-for-byte port of the arithmetic.)

### 5.4 `collect_pointer_ranges(...)` (← `controller.cpp:6532`) — pointer following

Recursive gather of `(absolute_addr, span)` ranges. Args: `struct_id, mem_base, depth, max_depth,
visited: &mut HashSet<(u64,u64)>, ranges: &mut Vec<(u64,i32)>, budget: &mut i64`.
- Stop if `depth >= max_depth` || `*budget <= 0` || `visited.contains(&(struct_id, mem_base))`.
- insert `(struct_id, mem_base)` into visited.
- `span = tree.struct_span(struct_id)`; if `span <= 0` return. push `(mem_base, span)`; `*budget -= span as
  i64`; if `*budget <= 0` return.
- if `snapshot.is_none()` return (bootstrap tick reads only main extent).
- for each child that is `Pointer32`/`Pointer64`, **non-collapsed, ref_id != 0** (Speedup 3: collapsed
  pointers skipped): `ptr_addr = mem_base + child.offset`; if `!snapshot.is_readable(ptr_addr, ptr_size)`
  skip; read `ptr_val` (U32/U64 from **the snapshot**, not the real provider); skip if `0` or `u64::MAX`;
  recurse `(child.ref_id, ptr_val, depth+1, …)`.
- Embedded struct ref (Struct node, `ref_id != 0`, no own children): recurse `(sn.ref_id, mem_base, depth,
  …)` (SAME depth — inline expansion, not a pointer hop).

> Reads pointer values from the PREVIOUS snapshot → targets lag one tick. Intentional. The
> `permanentPagesMarkedAfterModuleRead` test depends on the bootstrap-then-pointer-follow ordering (tick 1
> no snapshot → only main extent; tick 2 snapshot exists → pointer target page requested).

### 5.5 `viewport_address_range(&self) -> Option<(u64,u64)>` (← `controller.cpp:6798`)

Covers the visible lines across all editors. Per editor: `first = first_visible_line()`, `n =
lines_on_screen()`. For each visible visual line, `doc_line = doc_line_from_visible(v)`, `lm =
meta_for_line(doc_line)`. Skip if no meta or `lm.offset_addr == 0` (synthetic/command-row). Track `lo =
min(addr)`, `hi = max(addr + 16)` (16 = largest hex preview). `None` if nothing visible.
**Editor abstraction (§9):** `EditorHandle` must expose `first_visible_line`, `lines_on_screen`,
`doc_line_from_visible`, `meta_for_line`. In headless tests with no editor, returns `None` → first-snapshot
path always reads everything (matches the test setup, which DOES attach an editor for viewport tests).

### 5.6 `classify_permanent_pages(&mut self, fresh: &PageMap)` (← `controller.cpp:6832`)

If no snapshot or no provider → return. `regions = provider.enumerate_regions()`; empty → return. For each
fresh page not already permanent: find a region with non-empty `module_name` that **wholly contains** the
page (`page >= base && page+4096 <= base+size`) AND is `executable`; if found, `snapshot.mark_permanent(page)`.

### 5.7 `compute_data_extent(&self) -> i32` (← `controller.cpp:6856`)

For each node: `off = tree.compute_offset(i)`; skip if `< 0`. `sz = struct_span (containers) | byte_size`.
`tree_extent = max(tree_extent, off + sz)`. If `tree_extent > 0` return `min(tree_extent, 16 MiB)`. Else if
`provider.size() > 0` return that. Else 0. (This is `SnapshotProvider::size()` = `main_extent`.)
Exposed for tests as `data_extent()`.

### 5.8 `reset_snapshot(&mut self)` (← `controller.cpp:6876`)

`refresh_gen += 1` (cancels in-flight reads via gen guard); `read_in_flight = false`; `snapshot = None`;
clear `prev_pages`, `changed_offsets`, `value_history`, `last_value_addr`, `page_stability`; `idle_ticks =
0`; `tick_count = 0`; `apply_adaptive_interval()`. Called on base-address change and every source switch.

### 5.9 Adaptive interval

- `set_refresh_interval(ms)`: `base = max(1, ms)`; `max = max(base,1500)`; `blur = max(base,1500)`;
  `apply_adaptive_interval()`.
- `apply_adaptive_interval()` (← `controller.cpp:6441`): compute the target interval and the timer's
  active/inactive state, store into `current_interval_ms` / `timer_active`, and (UI) tell the real timer.
  ```
  if !window_visible: timer_active = false; return          // minimized → stop entirely
  target =
     if !window_focused: blur_ms
     else if idle_ticks >= IDLE_BACKOFF_TICKS (8):
        factor = 1 << min(4, (idle_ticks-8)/8 + 1)            // geometric backoff
        min(max_ms, base_ms * factor)                         // base 200 → 8:400 16:800 24:1500(cap)
     else: base_ms
  if target != current_interval_ms: current_interval_ms = target   // (UI: timer.set_interval)
  timer_active = true                                          // (UI: if !active, start)
  ```
- `set_window_state(focused, visible)` (← `controller.cpp:6469`): `focus_gained = focused && !window_focused`;
  store both; if `focus_gained` reset `idle_ticks = 0`; `apply_adaptive_interval()`.
- Test accessors: `refresh_interval_ms()` = `if timer_active {current_interval_ms} else {0}` (matches the
  C++ `m_refreshTimer ? interval() : 0` — note: C++ returns the *set* interval whether or not active; **PIN:**
  `refresh_interval_ms()` returns `current_interval_ms` regardless of active, and `refresh_timer_active()`
  returns `timer_active`. Re-check the tests: `focusOutWidensInterval` asserts `refreshIntervalMs()==1500`
  AND `refreshTimerActive()==true`; `minimizePausesTimer` asserts `!refreshTimerActive()` then after restore
  `refreshIntervalMs()==50`. So returning the stored interval irrespective of active is correct as long as
  `set_window_state(false,false)` does NOT change `current_interval_ms` — and it doesn't, because
  `apply_adaptive_interval` returns early on `!window_visible` before touching the target.) **PIN this
  ordering precisely.**

---

## 6. Editing — `set_node_value` (← `controller.cpp:3061`)

`set_node_value(&mut self, node_idx, sub_line, text: &str, is_ascii=false, resolved_addr=0)`:
1. Bounds-check `node_idx`. If `!provider.is_writable()` → return. If `read_only_override` → silent return.
2. `addr = if resolved_addr != 0 { resolved_addr } else { base_address + tree.compute_offset(node_idx)?
   (return if offset < 0) }`.
3. **Vec2/3/4**: if `sub_line >= 0`, `addr += sub_line*4`, `edit_kind = Float`. **Mat4x4**: if `0 <= sub_line
   < 16`, `addr += sub_line*4`, `edit_kind = Float`.
4. Parse to bytes: `is_ascii` → `format::parse_ascii_value(text, size_for_kind(edit_kind))`; else build a
   temp `Node { kind: edit_kind, big_endian: node.big_endian }` and `format::parse_value(&node, text)`.
   Return on parse failure.
5. **Strings (UTF8/UTF16)**: pad/truncate `new_bytes` to `node.byte_size()` (the full buffer).
6. If `new_bytes` empty → return. `write_size = new_bytes.len()`.
7. If `!provider.is_readable(addr, write_size)` → return.
8. `old_bytes = provider.read_bytes(addr, write_size)`.
9. **Test the write first**: write through snapshot (if present) else provider. On failure → log,
   `refresh()` (show real unchanged value), return (no command pushed).
10. On success: `undo.push(self, Command::WriteBytes { addr, old_bytes, new_bytes })`. (push immediately
    re-applies via redo → writes again, harmless.)

`write_selected_bytes_to_file(addr, n, path) -> Result<(), String>` (← `controller.cpp:2789`, public helper):
`n <= 0` → Err("No bytes to save"). `prov = snapshot if present else real (else Err("No active provider"))`.
If `!prov.is_readable(addr, n)` → Err. `data = prov.read_bytes(addr, n)`; (the C++ also guards a short read).
Open path WriteOnly|Truncate (Err on fail); write; Err on short write. Snapshot wins over real. Pure logic,
unit-testable.

---

## 7. Selection logic (← `controller.cpp:5094-5184`)

`sel_ids` holds **decorated** node ids. Decoration bits (from `core`):
```
const FOOTER_ID_BIT:  u64 = 0x8000_0000_0000_0000;
const ARRAY_ELEM_BIT: u64 = 0x4000_0000_0000_0000;
const MEMBER_BIT:     u64 = 0x2000_0000_0000_0000;
const ARRAY_ELEM_MASK / MEMBER_SUB_MASK = ((idx & 0xFFFFF) << 42)
make_array_elem_sel_id(id, elem) = id | ARRAY_ELEM_BIT | ((elem & 0xFFFFF) << 42)
make_member_sel_id(id, sub)      = id | MEMBER_BIT      | ((sub  & 0xFFFFF) << 42)
STRIP_MASK = !(FOOTER_ID_BIT | ARRAY_ELEM_BIT | ARRAY_ELEM_MASK | MEMBER_BIT | MEMBER_SUB_MASK)
```
These constants/helpers must match `core` exactly (the same masks decorate selections survive refresh
pruning, §5.1 step 5). `node_id & STRIP_MASK` → base id.

`handle_node_click(&mut self, source: EditorHandle, line, node_id, mods: Modifiers)`:
- `effective_id(line, nid)`: look at `last_result.meta[line]`: Footer → `nid|FOOTER_ID_BIT`; array element →
  `make_array_elem_sel_id`; member line → `make_member_sel_id`; else `nid`.
- `node_id == 0` → `clear_selection()`, return.
- `sel_id = effective_id(line, node_id)`. By modifiers:
  - **none**: clear; insert sel_id; `anchor_line = line`.
  - **ctrl (no shift)**: toggle sel_id in/out; `anchor_line = line`.
  - **shift (no ctrl)**: if no anchor → like none; else clear and insert `effective_id(i, meta[i].node_id)`
    for every line `i` in `[min(anchor,line), max(anchor,line)]` whose node_id != 0 and not the command row.
  - **ctrl+shift**: if no anchor → insert sel_id (additive) + set anchor; else add the whole range (additive,
    no clear).
- `update_command_row()`, `apply_selection_overlays()`.
- if exactly one selected → strip bits → `index_of_id` → emit `NodeSelected(idx)`.

`clear_selection()`: clear set; `anchor_line = -1`; update command row; apply overlays.
`selected_ids() -> &HashSet<u64>`.

`update_command_row()` (← `controller.cpp:5187`) builds line-0 text (UI; the *string-building* is pure and
should be a testable helper `build_command_row(provider, base_address, base_formula, view_root) -> String`):
- Source label: `name()` empty → `"source▾"`; else `"'<name>'▾"`.
- Address: `base_formula` if non-empty else `format!("0x{:X}", base_address)` (uppercase hex).
- Row = `"<src elided 40>  <addr elided 24>"`.
- Row2 (class line): if `view_root_id` resolves → `"<keyword> <typeName or 'Untitled'><brace>"`, brace = `""`
  if `brace_wrap` else `" {"`, typeName = `struct_type_name` or `name`. Fallback: first root Struct. Final
  fallback: `"struct Untitled<brace>"`.
- Combined = `"[▸] " + row + "  " + row2`. Emit `SelectionChanged(sel_ids.len())`.

(`▾` = U+25BE, `▸` = U+25B8 — preserve the exact glyphs; `test_command_row.cpp` pins the source-span logic.)

---

## 8. View root, bookmarks, source management, value tracking

### 8.1 View root & bookmarks
- `set_view_root_id(id)`: unchanged → return; set; `refresh()`.
- `scroll_to_node_id(id)` → primary editor (UI).
- `navigate_to_formula(formula) -> Result<(), String>` (← `controller.cpp:6913`): trim; empty → Err. Build
  `AddressParserCallbacks { resolve_module: provider.symbol_to_address, read_pointer: provider.read of
  pointer_size bytes, resolve_identifier: symbol-store resolve }`. `addr::evaluate(f, pointer_size, &cbs)`.
  On Err propagate. Else set `base_address` + `base_address_formula`, emit `DocumentChanged`, `refresh()`, Ok.
- `add_bookmark(name, formula)` (← `controller.cpp:6946`): trim; either empty → return; append to
  `tree.bookmarks`; `modified = true`; emit `DocumentChanged`.
- `remove_bookmark(idx)`: bounds-check; remove; `modified = true`; emit.

### 8.2 Source management (← `controller.cpp:6102-6414`)
- `attach_via_plugin(provider_id, target, register_as_saved=false)`: **plugin path is OUT OF SCOPE** — the
  provider creation goes through `ProviderRegistry::create_provider` which, for built-in sources, returns a
  `BufferProvider`/etc., and for live sources returns a documented stub error. The IN-SCOPE, portable parts:
  `undo.clear()`; replace `doc.provider`; clear `data_path`; **don't overwrite base_address**; adopt
  `provider.pointer_size()`; if `base_address_formula` non-empty re-evaluate against the new provider;
  `reset_snapshot()`; if `register_as_saved`: dedup on `(kind, provider_target)`, update-or-append a
  `SavedSourceEntry`, set active idx, push to editors; emit `DocumentChanged`, `refresh()`.
- `switch_to_saved_source(idx)` / `switch_source(idx)`: bounds-check; no-op if already active. Save current
  source's `base_address`+`base_address_formula` into its slot. Set active. If `kind=="File"` →
  `load_data_file(file_path)`, restore saved base+formula, `refresh()`. Else if `provider_target` non-empty →
  restore formula, `attach_via_plugin(kind, provider_target)`, restore saved base if formula empty. Emit
  `DocumentChanged`.
- `select_source(text)`: dispatch on a UI string:
  - `"#clear"` → `clear_sources()`.
  - `"#saved:N"` → `switch_to_saved_source(N)`.
  - `"File"` → open-file dialog (UI; in headless, the path is supplied) → save current base into active slot,
    `load_data_file(path)`, dedup/append File `SavedSourceEntry`, set active, emit `DocumentChanged`,
    `refresh()`.
  - else → look up provider by `text.to_lowercase().replace(" ", "")`; built-in → factory, plugin →
    `select_target` (stub); on success: save current base; capture `new_base = provider.base()` + display
    name; `undo.clear()`; swap provider; clear `data_path`; adopt pointer_size; **re-evaluate formula OR
    (only for a fresh/default project where `base_address == 0x0040_0000`) adopt `new_base`**;
    `reset_snapshot()`; dedup/append saved source; set active; emit `DocumentChanged` (twice — by design);
    `refresh()`.
- `clear_sources()`: clear list; active = -1; `provider = Arc::new(NullProvider)`; clear `data_path`;
  `reset_snapshot()`; push to editors; `refresh()`.
- `copy_saved_sources(sources, active_idx)`: replace list + active; push to editors.
- `push_saved_sources_to_editors()` (UI): build `{ text: "<kind> '<name>'", active }`; send to editors.
- accessors: `saved_sources()`, `active_source_index()`.

> **Base-address policy** (tested by `testSourceSwitchPreservesBase` / `...FreshDocUsesProviderBase`): a
> non-zero `base_address` is PRESERVED when attaching a provider with a different base; a fresh doc
> (`base_address == 0`, or the default `0x0040_0000`) ADOPTS the provider's base. `0x0040_0000` is
> `NodeTree`'s default base — the "fresh/default project" check keys off this magic value.

### 8.3 Value tracking control
- `set_track_values(on)`: set flag; if turning OFF → clear `value_history`, `last_value_addr`, zero all
  `last_result.meta[].heat_level`, `refresh()`.
- `reset_change_tracking()` (← `controller.cpp:1881`): clear `changed_offsets`, `value_history`,
  `last_value_addr`, `prev_pages`; `value_track_cooldown = 5`; zero all heat levels. (Does NOT refresh —
  caller does.)
- `set_read_only_override(v)` / `read_only_override()`: gate writes off even on writable providers.
- `set_suppress_refresh(v)`: MCP/batch bridge.

---

## 9. Signals → events; editor & timer abstraction; threading

### 9.1 Events (replaces Qt signals)
```rust
pub enum ControllerEvent {
    NodeSelected(i32),
    SelectionChanged(usize),
    StatusHint(String),
    ContextMenuAboutToShow { line: i32 },           // UI
    RequestOpenProviderTab { plugin_id: String, target: String, title: String }, // UI
    RequestOpenStructInNewTab(u64),                  // UI
    SourceLivenessChanged(bool),
    DocumentChanged,
}
```
Delivery: the controller pushes events into a `Vec<ControllerEvent>` drained by the UI each frame, OR holds an
`observer: Box<dyn FnMut(ControllerEvent)>`. **Decision:** for headless testability, accumulate into
`self.events: Vec<ControllerEvent>` and expose `take_events()`. The UI installs a drain that dispatches to
GPUI (`cx.emit` on the controller `Entity`). `DocumentChanged` internally also triggers `refresh()` (the C++
`connect(doc, documentChanged, refresh)`), so emitting it AND calling `refresh()` are coupled — model as a
private `self.on_document_changed()` that pushes the event and calls `refresh()`.

### 9.2 Editor abstraction (`EditorHandle`)
The controller talks to editors through a trait so the headless build links without gpui:
```rust
pub trait EditorView {
    fn is_editing(&self) -> bool;
    fn first_visible_line(&self) -> i64;
    fn lines_on_screen(&self) -> i64;
    fn doc_line_from_visible(&self, v: i64) -> i64;
    fn meta_for_line(&self, doc_line: i64) -> Option<&LineMeta>;
    fn apply_document(&mut self, result: &ComposeResult);
    fn apply_selection_overlay(&mut self, sel: &HashSet<u64>);
    fn set_command_row(&mut self, text: &str);
    // … set_custom_type_names, set_value_history_ref, set_provider_ref, view-state save/restore
}
```
`editors: Vec<Box<dyn EditorView>>`. `primary_editor()` = `editors.first()`. In tests, either attach a
mock editor implementing `EditorView` (for viewport tests) or attach none (most logic tests) — when none,
`is_editing()` is false, `viewport_address_range()` is `None`, and UI steps are skipped.

`add_split_editor`/`remove_split_editor`/`editors()` mirror the C++; the UI provides the real
gpui-backed editor.

### 9.3 Threading model (port of `QtConcurrent` + `QFutureWatcher`)
- One read at a time, guarded by `read_in_flight`. The worker captures `Arc<dyn Provider>` (clone) + page
  list; reads only (`read_bytes`), returns `PageMap`. **No controller-state access on the worker.**
- Completion (`on_read_complete`) runs on the main thread (UI: a channel/`cx.spawn` continuation; tests: a
  direct call). All state mutation happens there → no locking needed.
- **Generation guard** (`refresh_gen`/`read_gen`): any structural mutation that invalidates layout bumps
  `refresh_gen` (ChangeKind/ChangeOffset/ChangeBase via reset_snapshot, and `clear_history_for_adjs`).
  `on_read_complete` discards a result whose `read_gen != refresh_gen`.
- The `Provider` trait must be `Send + Sync`. `BufferProvider`/`SnapshotProvider`/`NullProvider` are
  read-shareable; writes go through interior mutability (`RwLock`) on the main thread only.
- Destructor analog: on `Drop` (or an explicit `shutdown()`), the UI must join/cancel the in-flight read
  before tearing down the snapshot. In Rust this is automatic if the worker holds its own `Arc` clones; the
  generation guard makes a late-completing read a harmless no-op.

UI timer wiring (from gpui cookbooks): a GPUI `Timer` (or `cx.background_executor().timer(Duration)` loop)
fires `on_refresh_tick`; on `RefreshPlan::Read`, `cx.background_executor().spawn(read_pages(...))` then
`.detach()` with a continuation that calls `entity.update(cx, |c,_| c.on_read_complete(map))`. Interval is
`controller.current_interval_ms` (re-read after each `apply_adaptive_interval`); when `timer_active` is
false the UI stops the timer.

---

## 10. Error-handling strategy

- **Library-internal fallible ops** (`load`, `save`, `navigate_to_formula`, `write_selected_bytes_to_file`):
  return `Result`/`bool` exactly as the C++ does. `load` returns `bool` (false = refused/parse error, with a
  `tracing::warn!`). `save` returns `io::Result<bool>` (or `bool` + log) matching `RcxDocument::save`.
  `navigate_to_formula` / `write_selected_bytes_to_file` return `Result<(), String>` (the C++ writes a reason
  string into an out-param).
- **Provider reads never error to the caller**: `read_bytes` returns zeroed bytes on failure (parity). The
  controller's `is_readable` gate is the real guard before a write/value-read.
- **Write failure** is the only non-WriteBytes-vs-tree-state distinction: `apply_command` returns `false`;
  the undo stack drops non-transient failures and keeps transient (`WriteBytes`) ones. Emit
  `StatusHint("Write rejected …")`.
- **Worker panics**: the C++ catches `QtConcurrent` exceptions in `onReadComplete` and returns. In Rust the
  background read body is pure and unlikely to panic; wrap the spawn so a panic is logged and treated as
  "no result" (read_in_flight reset, snapshot kept). Use `std::panic::catch_unwind` around `read_pages` in
  the UI worker, or simply let the join return an `Err` that the continuation logs + ignores.
- **No `unwrap()` on tree lookups**: `index_of_id` returns `Option`; `if let Some(idx)` guards mirror the C++
  `if (idx >= 0)` checks. A missing id is a silent no-op (matches C++).
- Use `thiserror` only if a typed error is wanted for `load`/`save`; otherwise `anyhow`/`String` at the
  controller boundary is fine (this is app glue, not a reusable library API).

---

## 11. Platform-specific code
The controller core is fully portable; no `#[cfg]` inside `controller.rs` itself. OS-specific concerns sit
in `provider` (the live process/kernel/WinDbg sources are stubs behind `ProviderRegistry`) and in `main`/`ui`
(config dir for `refreshMs`, GPUI timer). `owned_buffer` + `read_only_override` exist for the Windows
self-attach tutorial; keep the fields, but the path is inert behind the abstract provider on Linux.

---

## 12. TEST PLAN

Source tests: `tests/test_controller.cpp` (~65 slots) and `tests/test_refresh_speedups.cpp` (10 slots).
**Oracle note:** `_oracle/RESULTS.md` did NOT run `test_controller`/`test_refresh_speedups` (they are
QApplication/QScintilla-gated and were deferred as "UI-only/display-requiring", RESULTS.md §"Not built").
There is therefore **no captured golden stdout** for these two targets — the C++ assertions themselves are
the oracle (tests-catalog.md classifies them G/"second-most-important oracle after test_core"). Where a test
depends on `ValueHistory` thresholds or `SnapshotProvider` primitives, the *behavior* is also pinned by
`_oracle` indirectly via `test_core` (which IS green, 93/0/0, and covers `ValueHistory`).

Rust tests go in `tests/controller.rs` (integration) and/or `#[cfg(test)] mod tests` in `controller.rs`,
run with `cargo test --no-default-features` (no gpui). For the ~6 viewport/timer tests, attach a **mock
`EditorView`** (returns fixed `first_visible_line=0`, `lines_on_screen=N`, and a `meta_for_line` backed by
`last_result`). The C++ uses `QTest::qWait` + real timers; the Rust port **drives the tick/complete cycle
synchronously** (`ctrl.on_refresh_tick()` → `read_pages` → `ctrl.on_read_complete`) so no sleeping/timing
flakiness — this is the whole point of the policy/transport split (§0).

### 12.1 Shared test fixtures (port of `buildSmallTree` + `makeSmallBuffer`)
- `build_small_tree()`: root Struct "TestStruct"/"root" at 0 (uncollapsed) with children: u32@0
  "field_u32", Float@4 "field_float", u8@8 "field_u8", Hex16@9 "pad0", Hex8@11 "pad1", Hex32@12 "field_hex".
- `make_small_buffer()`: 64 bytes; u32@0 = `0xDEAD_BEEF`, f32@4 = 3.14, u8@8 = 0x42, u32@12 = `0xCAFE_BABE`.
- `BaseAwareProvider` test double: `BufferProvider`-like but `is_live()=true`, `kind()="Process"`,
  `name()="test"`, configurable `base()`. (For value-tracking tests that need a live provider.)
- `CountingProvider` test double (for speedups): tallies `reads_per_page`, `total_reads` (atomic);
  two regions — executable module image at base 0 (`8*4096`) + writable heap after it (`8*4096`);
  `is_live()=true`, `kind()="Process"`. Port `enumerate_regions()` exactly.

### 12.2 Test-by-test mapping (test_controller.cpp)

| C++ slot | Rust `#[test]` | What to assert (behavior pinned) |
|---|---|---|
| `testSetNodeValueWritesData` | `set_node_value_writes_data` | `set_node_value(u32_idx,0,"42")` → provider bytes at addr = 42 LE. |
| `testSetNodeValueUndoRedo` | `set_node_value_undo_redo` | write "99"; undo → 0xDEADBEEF; redo → 99. |
| `testSetNodeValueFloat` | `set_node_value_float` | "1.5" → f32 1.5; undo → ~3.14. |
| `testRenameNode` / `testRenameNodeUndoRedo` | `rename_node[_undo_redo]` | name set; undo restores; redo re-applies. |
| `testChangeNodeKind` | `change_node_kind` | UInt32→Float; undo → UInt32. |
| `testInsertAndRemoveNode` | `insert_and_remove_node` | insert Hex64@16 (size+1, kind/offset right); remove (size); undo → restored. |
| `testSetNodeValueHex` | `set_node_value_hex` | "AA BB CC DD" → those 4 bytes; undo → 0xCAFEBABE. |
| `testInlineEditRoundTrip` | `inline_edit_round_trip` | compose → find UInt8 Field line; `set_node_value(idx,0,"0xFF")` → byte = 0xFF. (Drop the QScintilla key-event part; keep the controller `set_node_value` half — the editor key-injection is UI, re-expressed as a direct `set_node_value` call.) |
| `testSourceSwitchPreservesBase` | `source_switch_preserves_base` | base=0x1000; attach provider base 0x400000; conditional `if base==0 { base=new }` leaves base 0x1000; provider.base()==0x400000. |
| `testSourceSwitchFreshDocUsesProviderBase` | `source_switch_fresh_doc_uses_provider_base` | base=0; adopt 0x7FFE0000. |
| `testToggleCollapse` / `testToggleCollapseRoundTrip` | `toggle_collapse[_round_trip]` | collapse/uncollapse + undo chain. |
| `testValueHistoryPopupOnlyDuringEdit` | (UI) → `value_history_ref_set_clears` (logic subset) | The popup-visibility half is UI (skip / move to ui tests). Keep the logic: history with 3 records → `unique_count()>1`; after refresh, meta carries heat. The "only during edit" gate is editor behavior. |
| `testDeleteClearsHeatForShiftedNodes` | `delete_clears_heat_for_shifted_nodes` | live provider; seed history for siblings after field_u32 + the node; `remove_node(field_u32)` → deleted id gone from `value_history`; each shifted sibling heat==0 (refresh re-records 1 value → count 1 → heat 0). |
| `testValueHistoryRingBuffer` | `value_history_ring_buffer` | the full ring spec: record dedup, heat thresholds 0/1/2/3, last(), uniqueCount caps at 10, count>10, for_each yields 10 oldest→newest, last==last(). |
| `testValueHistoryClear` | `value_history_clear` | record 2 → unique 2; clear → unique 0, heat 0. |
| `testInlineEditPrimitiveArray` | `inline_edit_primitive_array` | apply "int32_t[4]" type edit → Array, elementKind Int32, arrayLen 4; undo → UInt32. (Re-express the `inlineEditCommitted` signal as a direct call to the type-edit handler `apply_type_popup_result`/`change via array meta`.) |
| `testAddStaticField` / `testAddStaticFieldUndo` | `add_static_field[_undo]` | push `Command::Insert{static node}`; size+1, isStatic, offsetExpr "base"; undo/redo. |
| `testChangeStaticFieldExpression` | `change_static_field_expression` | `ChangeOffsetExpr` "base"→"base + 0x10"; undo restores. |
| `testDeleteStaticFieldPreservesStructSize` | `delete_static_field_preserves_struct_size` | static field doesn't change `struct_span`; remove keeps span. |
| `testStaticFieldRenamePreservesExpression` | `static_field_rename_preserves_expression` | Rename keeps offsetExpr + isStatic. |
| `testStaticFieldTypeChangePreservesFlags` | `static_field_type_change_preserves_flags` | ChangeKind Hex64→UInt32 keeps isStatic + offsetExpr. |
| `testClearValueHistoryResetsHeat` | `clear_value_history_resets_heat` | live provider, trackValues; seed heat≥2; refresh → meta shows heat>0; remove id + subtree from history; refresh → meta heat 0; history re-recorded with uniqueCount 1, heat 0. |
| `testQuickTypeChangeHexSameSize` | `quick_type_change_hex_same_size` | Hex32→Int32, no pad, same id. |
| `testQuickTypeChangeHexShrink` | `quick_type_change_hex_shrink` | Hex32→Hex16, same offset, a hex pad appears at offset+2. |
| `testQuickTypeChangeHexGrow` | `quick_type_change_hex_grow` | Hex32→Hex64, same offset, siblings shift by 4. |
| `testCycleSameSizeTypeVariants` | `cycle_same_size_type_variants` | build same-size variant list from `kind_meta`; cycle forward to next → kind matches. |
| `testDeleteKeyRemovesNode` | `delete_key_removes_node` | remove → id gone, size shrinks. |
| `testDuplicateNode` | `duplicate_node` | duplicate field_float → size+1, "field_float_copy" exists. |
| `testSplitHexNode` / `testSplitHexNodeUndo` | `split_hex_node[_undo]` | Hex32@12 split → two Hex16@12,14; undo restores Hex32. |
| `testGroupIntoUnion` | `group_into_union` | group {u32,float} → a union node, 2 children both offset 0. |
| `testInsertNodeAutoOffset` | `insert_node_auto_offset` | insert offset=-1 → appended after last sibling, offset>0. |
| `testBatchChangeKind` / `testMultiSelectBatchCycleType` | `batch_change_kind` / `multi_select_batch_cycle_type` | batch to Hex64/Hex32; undo restores both originals. |
| `testConvertToTypedPointer` | `convert_to_typed_pointer` | field_hex → Pointer32/64 with ref_id != 0. |
| `testInsertNodeAboveShiftsOffsets` | `insert_node_above_shifts_offsets` | insert Hex64 above field_float@4 → float shifts to 12. |
| `testDeleteRootStruct` | `delete_root_struct` | add 2nd root; delete it → gone, original survives. |
| `testMoveNodeSwapsOffsets` | `move_node_swaps_offsets` | macro of two `ChangeOffset`; offsets swap; undo restores. |
| `testChangeBaseAddress` | `change_base_address` | `ChangeBase` sets base; undo restores. |
| `testChangeArrayMeta` | `change_array_meta` | insert Array; `ChangeArrayMeta` element/len; undo restores. |
| `testChangeClassKeyword` | `change_class_keyword` | `ChangeClassKeyword` → "class"; undo restores resolved keyword. |
| `testChangeComment` | `change_comment` | `ChangeComment` sets; undo clears. |
| `testCollapseExpandAll` | `collapse_expand_all` | suppress_refresh + macro of `Collapse`; collapse all then expand all; undo re-collapses. |
| `testNullptrPointerDisplay` | (format) `nullptr_pointer_display` | `format::fmt_pointer64(0)=="nullptr"`, non-zero starts "0x". (Belongs to `format` module; include as a controller-adjacent sanity test.) |
| `testGeneratorPrepareChildren` | `static_field_excluded_from_span` | static field @9999 → `struct_span < 9999`. (The generator half is in `generator`; here assert tree/span only.) |
| `testBatchRemoveMultipleNodes` | `batch_remove_multiple_nodes` | batch remove 2 → both gone; undo restores both. |
| `testSetNodeValueBool` | `set_node_value_bool` | insert Bool@50; "true" → byte 1. |
| `testSetNodeValueNegativeInt` | `set_node_value_negative_int` | insert Int8@51; "-128" → byte -128. |
| `testNodeToJsonOmitsDefaults` / `testNodeToJsonIncludesIsRelative` | (core serde) `node_json_*` | omit isStatic/isRelative/ptrDepth defaults; include isRelative when set. (These exercise `core` Node serde — keep as cross-checks here or in `core` tests.) |
| `testCycleExcludesStringAndVectorTypes` | `cycle_excludes_string_and_vector_types` | variant-list filter rules: 1-byte from Hex8 excludes UTF8; 8-byte from Hex64 excludes Vec2; from Vec2, Vec2 included. |
| `testSpaceResizeWrapAndMultiSelect` | `space_resize_wrap_and_multi_select` | hex cycle wrap (Hex128→Hex8, Hex8→Hex128) + batch change two Hex32→Hex64. |
| `testSpaceCycleFullCircle` | `space_cycle_full_circle` | hex64→hex8 (shrink, pads, total 8 bytes, no overlap) → join hex8→16→32→64 back to 1 child of 8 bytes. |
| `testSpaceNoOverlapAfterGrow` | `space_no_overlap_after_grow` | join hex32@8+hex32@12→hex64@8; no overlapping offsets; 2 children. |
| `testSpaceSelectionSurvivesJoin` | `space_selection_survives_join` | select hex32@0; join → selection transfers to the new Hex64@0 (decoration/strip + re-resolve). |
| `testSpaceRapidCycleNoCorruption` | `space_rapid_cycle_no_corruption` | 20 join/shrink presses; after each: no overlapping offsets, total bytes==8. |

### 12.3 Test-by-test mapping (test_refresh_speedups.cpp)
Drive synchronously via a mock `EditorView` + direct tick/complete. Replace `setRefreshInterval(50)` +
`waitForOneTick` with explicit `on_refresh_tick()`/`read_pages`/`on_read_complete` calls; assert on
`ctrl.snapshot_prov()`, `ctrl.page_stability(p)`, `prov.reads_per_page`, `ctrl.refresh_interval_ms()`,
`ctrl.refresh_timer_active()`.

| C++ slot | Rust `#[test]` | Assert |
|---|---|---|
| `permanentPagesMarkedAfterModuleRead` | `permanent_pages_marked_after_module_read` | uncollapsed pointer + target in module page; tick 1 (main extent) → tick 2 (collect_pointer_ranges adds module page) → snapshot.is_permanent(target page); tick 3 → module page reads==0. (Drive 3 tick/complete cycles explicitly.) |
| `collapsedPointerSkipsTarget` | `collapsed_pointer_skips_target` | collapsed pointer → module target page never in any read plan (reads_per_page[target]==0). |
| `pageStabilityClimbsWhenIdle` | `page_stability_climbs_when_idle` | ≥6 tick/complete cycles with constant bytes → `page_stability(heap_page) >= 1`. |
| `viewportBoundsReReads` | `viewport_bounds_re_reads` | mock viewport covers only offset 0 lines; after first-snapshot tick, tick 2 plan excludes the last heap page (reads==0). |
| `adaptiveBackoffWidensInterval` | `adaptive_backoff_widens_interval` | base 50; ≥12 idle complete cycles → `refresh_interval_ms() > 50`. |
| `focusOutWidensInterval` | `focus_out_widens_interval` | base 50; `set_window_state(false,true)` → interval 1500, timer active. |
| `minimizePausesTimer` | `minimize_pauses_timer` | base 50; `set_window_state(false,false)` → !timer_active; restore (true,true) → timer active, interval 50. |
| `snapshotProviderPermanentSet` | `snapshot_provider_permanent_set` | `mark_permanent(0x1000+17)` page-aligns; `is_permanent(0x1000)`&`0x1FFF` true, `0x2000` false; `clear_permanent` → false. |
| `snapshotProviderMergeKeepsExisting` | `snapshot_provider_merge_keeps_existing` | init pages 0x0000=0xAA,0x1000=0xBB; merge fresh 0x1000=0xCC; read 0x0000→0xAA (kept), 0x1000→0xCC (merged). |

### 12.4 Coverage notes / parity checklist (from controller.md §15)
The Rust suite must additionally pin (assertions woven into the above):
- WriteBytes pushed only on success; `read_only_override` blocks writes silently; failed WriteBytes runs no
  refresh and is kept on the undo stack (transient), tree-state failures drop (obsolete).
- Generation guard: an `on_read_complete` with `read_gen != refresh_gen` (simulate by bumping refresh_gen via
  an edit between tick and complete) is discarded. Add a dedicated `#[test] generation_guard_discards_stale_read`.
- All-zero page-0 guard: with non-empty `prev_pages`, an `on_read_complete` whose page 0 is all-zero is
  discarded and snapshot kept. Add `#[test] all_zero_page0_discarded`.
- ChangeKind keeps value history; ChangeOffset/Remove/Insert clear via `clear_history_for_adjs`. Add
  `#[test] change_kind_keeps_history`.
- Selection bit decoration round-trips through refresh pruning (covered by `space_selection_survives_join`,
  add a direct footer/array/member decorate→strip unit test `selection_decoration_roundtrip`).

---

## 13. Ordered, independently-verifiable implementation steps

Each step compiles green (`cargo build --no-default-features`) and adds passing tests.

1. **Providers + ValueHistory + Command/OffsetAdj enum** (if not already in `core`/`provider`): pin
   `Provider` trait surface, `BufferProvider`, `NullProvider`, `SnapshotProvider` (with the exact read /
   is_readable / merge / permanent semantics), `ValueHistory`. Tests: `value_history_ring_buffer`,
   `value_history_clear`, `snapshot_provider_permanent_set`, `snapshot_provider_merge_keeps_existing`. (These
   have no controller dependency — land first.)
2. **`RcxDocument`** (struct + `compose` wrapper passing aliases; `save`/`load`/`load_data_*` with the
   forgiving-load rules). Tests: round-trip save/load of a small tree, empty-file → empty project,
   non-JSON → false, relative savedSources path resolution.
3. **`UndoStack`** (push=execute-redo, undo/redo, macros, clean index, obsolete-drop). Tests with a trivial
   command and a stub `apply_command` returning true/false.
4. **`apply_command`** (all 18 arms; clear_history helpers; refresh_gen bumps; success rules). Tests:
   `change_base_address`, `change_array_meta`, `change_class_keyword`, `change_comment`, static-field arms,
   `change_kind_keeps_history`, `move_node_swaps_offsets`.
5. **Mutation helpers** (`rename/insert/insert_above/remove/change_node_kind (shrink/grow/same)/duplicate/
   split_hex/join_hex/group_into_union/dissolve_union/batch_*/delete_root_struct/convert_to_typed_pointer/
   materialize_ref_children/insert_static_field`). Tests: the whole geometry block (quick-type-change,
   space-cycle, insert-above-shift, auto-offset, batch, union, split/join full-circle/no-overlap/rapid).
6. **`set_node_value` + `write_selected_bytes_to_file`** (write-first, undo/redo, ascii/float/hex/bool/neg).
   Tests: the `set_node_value_*` family + `write_selected_bytes_to_file` success/short-read/no-provider.
7. **`refresh()`** (compose + change-highlight + value-tracking + selection-prune + custom-types). Tests:
   `clear_value_history_resets_heat`, `delete_clears_heat_for_shifted_nodes`, heat propagation, set_track_values
   off zeroes heat.
8. **Selection** (`handle_node_click`, decoration/strip, `build_command_row`). Tests:
   `selection_decoration_roundtrip`, `space_selection_survives_join`, command-row label logic
   (`source▾`/`'name'▾`), `clear_selection`.
9. **Refresh state machine** (`on_refresh_tick` → `RefreshPlan`, `read_pages`, `on_read_complete`, diff +
   stability + generation/all-zero guards, `collect_pointer_ranges`, `viewport_address_range` via mock
   editor, `classify_permanent_pages`, `compute_data_extent`, `reset_snapshot`). Tests: all
   `test_refresh_speedups` slots + `generation_guard_discards_stale_read` + `all_zero_page0_discarded`.
10. **Adaptive interval + window state** (`apply_adaptive_interval`, `set_refresh_interval`,
    `set_window_state`, exposed `refresh_interval_ms`/`refresh_timer_active`/`idle_ticks`/`page_stability`).
    Tests: `adaptive_backoff_widens_interval`, `focus_out_widens_interval`, `minimize_pauses_timer`.
11. **Source management** (`SavedSourceEntry`, `select_source`/`switch_to_saved_source`/`clear_sources`/
    `copy_saved_sources`/`ingest_pending_saved_sources`, base-address policy, `attach_via_plugin` in-scope
    bookkeeping). Tests: `source_switch_preserves_base`, `source_switch_fresh_doc_uses_provider_base`,
    clear→Null + resets snapshot/history, switch-invalid no-op.
12. **View root, bookmarks, navigate_to_formula** + event plumbing (`ControllerEvent`, `take_events`). Tests:
    add/remove bookmark sets modified + emits; navigate_to_formula ok/err.
13. **UI integration (feature `ui`, later workflow):** implement `EditorView` over the bespoke gpui editor
    `Element`; wire the GPUI timer → `on_refresh_tick`, background read → `on_read_complete`, and drain
    `ControllerEvent` to `cx.emit`. Not part of the headless parity gate.

---

## 14. Notable parity hazards (do NOT "fix" silently)
1. `ToggleRelative` has no `apply_command` arm → keep as a no-op (`// FIXME-parity:`). (§4.2)
2. `changed_offsets` stores absolute `pageAddr+i` while `refresh()` tests it with a base-relative `offset+b`
   → only highlights when base==0; replicate the exact arithmetic. (§5.3)
3. `Command::Remove` constructed with empty subtree still works because redo recomputes the live subtree;
   only undo consults the stored subtree. (§3.2)
4. `select_source` emits `DocumentChanged` **twice** by design. (§8.2)
5. Value history is **kept** across `ChangeKind` (NOT cleared) — deliberate UX. (§4.2)
6. Page stability climbs and caps at `STABILITY_THRESHOLD + 16` (= 21), not at the threshold. (§5.3)
7. `refresh_interval_ms()` returns the stored interval regardless of active; `refresh_timer_active()` is the
   active flag — and `set_window_state(_,false)` must not alter the stored interval (early return in
   `apply_adaptive_interval`). (§5.9)
