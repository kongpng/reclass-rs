# PORTING SPEC — Composition + Undo/Redo Command Stack (`compose-undo`)

> Function-level porting spec to drive the faithful Rust implementation of the two
> cooperating halves of this subsystem:
> **(A) the composition / render pipeline** (`compose.cpp` → text + `LineMeta`) and
> **(B) the undo/redo command stack** (`core.h` `namespace cmd` + the `applyCommand`
> dispatcher, `RcxCommand`, builders and batch macros in `controller.cpp`).
>
> Sources of truth, in order: the behavioral map `_design/understand/compose-undo.md`;
> the original C++ at `/home/loke/reclass-cpp` (`src/compose.cpp`, `src/core.h`,
> `src/controller.cpp`, `src/controller.h`); the test oracle (`tests/test_controller.cpp`,
> `tests/test_compose.cpp`, `_oracle/RESULTS.md`, `_oracle/fixtures/eprocess_*.txt`);
> and the target architecture (`_design/ARCHITECTURE.md`, `_design/crate_selection.md`).
>
> **Scope reminder (matches the understand map):** the command *definitions* are in
> `core.h`; the *stack machinery* lives in `controller.cpp`. The 18 command variants are
> heavily tested by **`tests/test_controller.cpp`** (NOT `test_compose.cpp`, which tests the
> renderer). `compose.cpp` is the rendering engine. Both are pure logic over a `NodeTree`
> plus an abstract `Provider` (built-in providers only; RTTI/module hooks are no-ops here).

---

## 0. Target crate / modules

Single package `reclass` (per ARCHITECTURE.md §2 — NOT a workspace). This subsystem lands
in two existing modules that depend on `core`:

| Half | Rust module | C++ origin | Cargo feature |
|---|---|---|---|
| **A. Composition** | `src/compose.rs` | `compose.cpp` (+ the `compose()` decl, `LineMeta`/`LineChip`/`LayoutInfo`/`LineGeometry`/span helpers in `core.h`) | always |
| **B. Undo/redo stack** | `src/controller.rs` | `controller.cpp` (`applyCommand`, `RcxCommand`, builders, macros) + `core.h` `namespace cmd` | always |
| Shared model the commands mutate | `src/core/` (`Node`, `NodeTree`, `NodeKind`, `ValueHistory`) | `core.h` | always |

The `Command` enum, `OffsetAdj`, `LineMeta`, `LineChip`, `LineKind`, `ChipKind`,
`LayoutInfo`, `LineGeometry`, the column-span helpers, and the column constants are
declared in `core.h`. To match the C++ layout, put:
- `Command`/`OffsetAdj` and the 18 command structs in `src/core/command.rs` (re-exported
  from `core`), so both `compose` and `controller` see them with no cycle.
- `LineMeta`/`LineChip`/`LineKind`/`ChipKind`/`LayoutInfo`/`LineGeometry`/spans/column
  constants in `src/core/line.rs` (consumed by `compose` and by the `ui::editor`).

`compose()` is a free function in `compose.rs`. The undo stack is a hand-rolled struct
(see B.2) living in `controller.rs` (or `core/undo.rs` if you want it testable without
the controller — recommended, since the controller carries UI glue).

**No `#[cfg]` platform code in this subsystem.** All branches compile on Linux. Logic tests
run `cargo test --no-default-features` (no `ui` ⇒ no gpui).

---

## A. COMPOSITION (`compose.rs`)

The composition engine is read-only over the tree. The behavioral map
(`_design/understand/compose-undo.md` §A) documents every field and walker in detail; this
spec records the *port decisions* and the test plan. Do not re-derive the algorithm here —
follow the map plus the C++.

### A.1 Public entry point

C++ (`compose.cpp:1510`, decl `core.h:1529`):
```cpp
ComposeResult compose(const NodeTree& tree, const Provider& prov, uint64_t viewRootId = 0,
                      bool compactColumns=false, bool treeLines=false, bool braceWrap=false,
                      bool typeHints=false, bool showComments=true,
                      SymbolLookupFn symbolLookup={}, bool showRtti=true, bool showEnumChips=true);
```

Rust:
```rust
pub struct ComposeOptions {            // defaults match the C++ default args
    pub view_root_id: u64,             // 0 = all roots
    pub compact_columns: bool,         // false
    pub tree_lines: bool,              // false
    pub brace_wrap: bool,              // false
    pub type_hints: bool,              // false
    pub show_comments: bool,           // true
    pub show_rtti: bool,               // true
    pub show_enum_chips: bool,         // true
}
impl Default for ComposeOptions { /* matches the C++ default arg list */ }

pub type SymbolLookupFn<'a> = Option<Box<dyn Fn(u64) -> String + 'a>>;

pub fn compose(tree: &NodeTree, prov: &dyn Provider,
               opts: &ComposeOptions, symbol_lookup: SymbolLookupFn<'_>) -> ComposeResult;
```
Rationale: 8 bool/u64 params + a closure is unergonomic positionally; bundle the flags into
an options struct whose `Default` reproduces the C++ default args exactly. Keep a thin
`compose_default(tree, prov)` helper for the many tests that call `compose(tree, prov)`.

The `RcxDocument::compose(...)` wrapper (`controller.h:72`) maps to a method on the Rust
document/controller that fills `ComposeOptions` from the doc's render flags and forwards.
(Note the wrapper passes `showRtti`/`showEnumChips` implicitly as the C++ defaults.)

### A.2 Item-by-item type mapping (composition)

| C++ (`compose.cpp` / `core.h`) | Rust (`compose.rs` / `core/line.rs`) | Notes / crate |
|---|---|---|
| file-local `struct ComposeState` (`compose.cpp:71`) | private `struct ComposeState<'a>` in `compose.rs` | mutable accumulator threaded through recursion; NOT public. Holds `text: Utf16Buf`, `meta: Vec<LineMeta>`, `line_starts: Vec<usize>`, etc. (full field table in understand map §A.1). |
| `QString text` (UTF-16 buffer) | **`Utf16Buf`** — newtype wrapping `Vec<u16>` with `push_str(&str)` (via `encode_utf16`), `len_units()`, and `to_string()` | **Fidelity-critical:** all column/`startCol`/`endCol`/`maxLineLen` math is in **UTF-16 code units**. The glyphs used (`▸ ▾ ├ └ │ ↻ → ·`) are all single UTF-16 units, so a UTF-16 count matches Qt exactly. Do NOT index by Rust `char`/byte. Convert to a `String` only in the final `ComposeResult`. |
| `QVector<LineMeta> meta` | `Vec<LineMeta>` | parallel to lines |
| `QVector<int> lineStarts` | `Vec<usize>` | char (UTF-16 unit) offset of each line start |
| `QSet<uint64_t> visiting` / `ptrVisiting` / `virtualPtrRefs` | `HashSet<u64>` (ahash) | cycle guards |
| `QHash<uint64_t, QVector<int>> childMap` | `HashMap<u64, Vec<i32>>` (ahash) | parent id → child indices |
| `mutable QSet<uint64_t> childMapSorted` | `RefCell<HashSet<u64>>` or just `HashSet<u64>` on the `&mut state` | tracks which parents had children lazily sorted; since `ComposeState` is `&mut`, a plain `HashSet` field works — no interior mutability needed. |
| `QVector<int64_t> absOffsets` | `Vec<i64>` | precomputed abs addr per node index |
| `QHash<uint64_t,int> scopeTypeW/scopeNameW` | `HashMap<u64,i32>` | per-scope column widths |
| `SymbolLookupFn = std::function<QString(uint64_t)>` | `Option<Box<dyn Fn(u64)->String + 'a>>` | optional PDB symbol callback; `None` in headless tests |
| RTTI fields (`rttiModulesCached`, `rttiCache`, `g_rttiDiscoveryHook`) | keep as struct fields but the discovery hook is `None`/no-op | **out of scope**: providers return empty module lists, hook absent → RTTI chips never appear in tests. Mirror the fields so the code shape matches; the hook is a `None` `Option<Box<dyn Fn...>>` global-equivalent (pass via options or a thread-local left unset). |
| `compose()` free fn | `pub fn compose(...)` | A.1 |
| `composeNode` (`compose.cpp:1234`) | `fn compose_node(state, tree, prov, idx, depth)` | dispatcher: pointer-with-refId → merged fold header + deref expansion; Struct/Array → `compose_parent`; else → `compose_leaf` |
| `composeParent` (`compose.cpp:607`) | `fn compose_parent(...)` | struct cycle guard, array separators, enum/bitfield/primitive-array/static-field rendering, header/footer |
| `composeLeaf` (`compose.cpp:305`) | `fn compose_leaf(...)` | single field → `lines_for_kind(kind)` lines; builds chips |
| `ComposeState::emitLine(text, LineMeta&&)` | `fn emit_line(&mut self, text: &str, lm: LineMeta)` | prepends 3-char fold indicator, rewrites indent into tree connectors when `tree_lines`, auto-detects trailing `{` col for `brace_col`, appends to `text`, records `line_starts`, tracks `max_line_len` (trailing spaces excluded) |
| `effectiveTypeW/NameW(scopeId)` | `fn effective_type_w/name_w(&self, scope_id: u64) -> i32` | scope width → global fallback |
| `setTreeSibling(depth, hasMore)` | `fn set_tree_sibling(&mut self, depth, has_more)` | grows `sibling_stack` |
| `QRegularExpression("  +")` run-squash (chip sanitize) | **`regex::Regex`** `"  +"` → `" "`, compiled once (`once_cell`/`LazyLock`) | the only regex in compose |
| Scintilla fold consts (`SC_FOLDLEVELBASE=0x400`, `SC_FOLDLEVELHEADERFLAG=0x2000`) | `const FOLD_LEVEL_BASE: i32 = 0x400; const FOLD_LEVEL_HEADER_FLAG: i32 = 0x2000;` | plain ints; the editor uses them later |
| `normalizePreferAncestors/Descendants` (`compose.cpp:1747/1768`) | **methods on `NodeTree`** (`core`): `fn normalize_prefer_ancestors(&self, ids: &HashSet<u64>) -> HashSet<u64>` etc. | These are `NodeTree` methods defined in `compose.cpp` for file-locality; in Rust put them on `NodeTree` in `core` (the controller's batch ops call them). Pure id-set filtering. |

### A.3 Output data structures (serde / representation)

These live in `core/line.rs`. They are **runtime-only** (not serialized to `.rcx`), so
**no serde** is needed on `LineMeta`/`LineChip`/`ComposeResult` — the `.rcx` format
serializes `Node`/`NodeTree`, not render output. (Add `#[derive(Debug, Clone)]` for test
ergonomics.)

```rust
pub struct ComposeResult {
    pub text: String,            // built from the Utf16Buf at the end
    pub meta: Vec<LineMeta>,
    pub layout: LayoutInfo,
    pub max_line_len: i32,
    pub line_starts: Vec<usize>, // UTF-16-unit offsets
}
```

`LineKind` (`core.h:927`) → `#[repr(u8)] enum LineKind { CommandRow, Blank, Header, Field,
Continuation, Footer, ArrayElementSeparator }` (Blank unused; keep slot for value parity).

`ChipKind` (`core.h:971`) → `#[repr(u8)] enum ChipKind { Enum, TypeHint, Rtti, Symbol,
Comment, AddComment }`. **Order is observable** (test_chips asserts Enum→TypeHint→Rtti→
Comment ordering); keep the discriminant order identical.

`LineChip` (`core.h:982`):
```rust
pub struct LineChip {
    pub kind: ChipKind,
    pub start_col: i32, pub end_col: i32,   // UTF-16 columns; <0 = overlay-only
    pub text: String,
    pub rtti_vtable_addr: u64,
    pub type_hint_kinds: Vec<NodeKind>,
    pub enum_current_value: i64,
    pub enum_ref_node_id: u64,
}
```
`findChip(lm, kind)` → `fn find_chip(lm: &LineMeta, kind: ChipKind) -> Option<&LineChip>`
(linear scan, n ≤ 4).

`LineMeta` (`core.h:994`, ~30 fields) → a `#[derive(Debug, Clone, Default)] pub struct
LineMeta { ... }` mapping each field 1:1 (full list in understand map §A; e.g. `node_idx:
i32`, `node_id: u64`, `sub_line: i32`, `depth: i32`, `fold_level: i32`, `fold_head: bool`,
`fold_collapsed: bool`, `is_continuation: bool`, `is_root_header: bool`, `is_array_header:
bool`, `line_kind: LineKind`, `node_kind: NodeKind`, `element_kind: NodeKind`,
`array_view_idx/array_count/array_element_idx: i32`, `offset_text: String`, `offset_addr:
u64`, `ptr_base: u64`, `marker_mask: u32`, `data_changed: bool`, `heat_level: i32`,
`changed_byte_indices: Vec<i32>`, `line_byte_count: i32`, `effective_type_w/name_w: i32`,
`pointer_target_name: String`, `is_array_element/is_member_line/is_static_line: bool`,
`brace_col: i32`, `parent_addr: u64`, `chips: Vec<LineChip>`).

`LayoutInfo` (`core.h:1054`), `LineGeometry` (`core.h:1155`), `ColumnSpan` (`core.h:1119`),
`EditTarget` enum (`core.h:1125`), and the `*SpanFor` inline helpers (`core.h:1183-1452`)
port as plain structs/`const fn`s. The column constants (`kFoldCol=3`, `kTreeIndent=2`,
`kColType=14`, `kColName=22`, `kColValue=96`, `kColComment=28`, `kColBaseAddr=12`,
`kSepWidth=1`, `kMinTypeW=9`, `kMaxTypeW=128`, `kMinNameW=10`, `kMaxNameW=128`,
`kCompactTypeW=20`, `kDefaultRefreshMs=200`) → module-level `pub const`s with the same
values. `LineGeometry::forLine` and the span fns are pure column arithmetic — port verbatim,
keeping `qBound` → `value.clamp(min, max)`.

### A.4 Algorithm pseudocode for the tricky compose bits

These are the parts where a naive port silently diverges. Follow the C++ exactly.

**Absolute-offset BFS (`compose.cpp:1539-1586`)** — O(N), guards orphans:
```
abs[i] = 0 for all i
for i where node[i].parentId == 0: abs[i] = node[i].offset
queue = childMap[0]; mark those visited
while queue nonempty (FIFO):
    idx = pop_front
    pi = tree.index_of_id(node[idx].parentId)        // -1 → treat parent abs as 0
    abs[idx] = (pi>=0 ? abs[pi] : 0) + node[idx].offset
    for ci in childMap[node[idx].id]: if !visited: mark, push_back
// re-root any still-unvisited node (broken parent chain) at its own offset,
// then DFS-fix its descendants:
for i not visited:
    abs[i] = node[i].offset; visit
    dfs stack=[i]; while stack: p=pop; for ci in childMap[node[p].id] not visited:
        abs[ci] = abs[p] + node[ci].offset; visit; push
finally: abs[i] += tree.baseAddress for all i
```
Edge case the port MUST keep: orphans land at their own offset + base, **never 0** (else
they'd overlay real roots). Use a FIFO queue for the main BFS (`Vec` + front index, like the
C++) and a LIFO stack for the descendant fix (matches the C++ `takeLast`).

**Hex-digit tier (`compose.cpp:1589`)**: `maxAddr<=0xFFFF→4`, `<=0xFFFF_FFFF→8`,
`<=0xFFFF_FFFF_FFFF→12`, else `16`. `maxAddr` seeded with `baseAddress`, raised by each
`abs[i]`.

**Column widths (`compose.cpp:1601-1688`)**: global
`typeW = clamp(maxTypeLen, kMinTypeW=9, typeCap)` where `typeCap = if compact {20} else {128}`;
`nameW = clamp(maxNameLen, kMinNameW=10, kMaxNameW=128)`. Per-scope widths computed per
container over **direct non-struct children only** (struct children excluded so a nested
pointer header doesn't inflate sibling widths); **name width includes ALL nodes** (stable
when cycling types). Synthesized primitive-array element type names (`"uint32_t[99]"`) count
toward the type width.

**Lazy child sort**: `childIndices(state, parentId)` sorts the parent's child-index vector by
`absOffsets` on first access, recording the parent id in `childMapSorted`. Port: in
`fn child_indices(&mut self, parent_id) -> &[i32]`, sort `child_map[&parent_id]` by
`abs_offsets[idx]` if not yet in `child_map_sorted`. **Stable sort by absolute offset.**

**Chip text sanitize**: replace `\r \n \t` with `·` (middle dot) and squash `"  +"`→`" "` so a
chip never spans rows. (test_chips asserts multi-line comments collapse to middle-dot.)

### A.5 Cycle guards (compose)
- struct recursion: `visiting: HashSet<u64>` of node ids (insert on enter `compose_parent`,
  remove on exit).
- pointer expansion: `ptrVisiting` keyed by `pBase ^ refId.wrapping_mul(GOLDEN)`
  (`GOLDEN = 0x9E3779B97F4A7C15`). Keep the exact mixing constant for parity of which cycles
  trip the guard.
- virtual ref expansion: `virtualPtrRefs: HashSet<u64>` of refIds currently expanding.

---

## B. UNDO/REDO COMMAND STACK (`controller.rs`)  ← pure, port-critical core

### B.1 Command value types — item-by-item (`core.h:1080-1115` → `core/command.rs`)

`OffsetAdj` (`core.h:1081`):
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct OffsetAdj { pub node_id: u64, pub old_offset: i32, pub new_offset: i32 }
```
A sibling-offset shift recorded when a node is inserted/removed/resized.

The **18** command structs (each fully self-inverse — stores both old and new), mapping
`core.h:1082-1105` exactly. Each is `#[derive(Debug, Clone)]`:

| # | C++ struct | Rust struct | apply-arm field set (forward = `new`, undo = `old`) |
|---|---|---|---|
| 1 | `ChangeKind{nodeId; oldKind,newKind; offAdjs}` | `ChangeKind { node_id: u64, old_kind: NodeKind, new_kind: NodeKind, off_adjs: Vec<OffsetAdj> }` | set `node.kind`; set each `offAdj` sibling offset; bump refresh_gen; **keep** value history; `clear_history_for_adjs(off_adjs)` |
| 2 | `Rename{nodeId; oldName,newName}` | `Rename { node_id, old_name: String, new_name: String }` | set `node.name` |
| 3 | `Collapse{nodeId; oldState,newState}` | `Collapse { node_id, old_state: bool, new_state: bool }` | set `node.collapsed` |
| 4 | `Insert{node; offAdjs}` | `Insert { node: Node, off_adjs: Vec<OffsetAdj> }` | redo: `tree.add_node(node)` then apply adjs; undo: revert adjs then remove by id + `invalidate_id_cache`; `clear_history_for_adjs` |
| 5 | `Remove{nodeId; subtree; offAdjs}` | `Remove { node_id, subtree: Vec<Node>, off_adjs: Vec<OffsetAdj> }` | redo: apply adjs FIRST, then remove subtree indices **descending**, clear each node's history, `invalidate_id_cache`; undo: re-add subtree (stored order) then revert adjs; `clear_history_for_adjs` |
| 6 | `ChangeBase{oldBase,newBase; oldFormula,newFormula}` | `ChangeBase { old_base: u64, new_base: u64, old_formula: String, new_formula: String }` | set `tree.base_address` + `base_address_formula`; `reset_snapshot()` |
| 7 | `WriteBytes{addr; oldBytes,newBytes}` | `WriteBytes { addr: u64, old_bytes: Vec<u8>, new_bytes: Vec<u8> }` | write `bytes` via snapshot-or-provider; **only fallible arm** |
| 8 | `ChangeArrayMeta{nodeId; oldElementKind,newElementKind; oldArrayLen,newArrayLen}` | `ChangeArrayMeta { node_id, old_element_kind: NodeKind, new_element_kind: NodeKind, old_array_len: i32, new_array_len: i32 }` | set `element_kind`+`array_len`; clamp `view_index` to `max(0, array_len-1)` if `>= array_len` |
| 9 | `ChangePointerRef{nodeId; oldRefId,newRefId}` | `ChangePointerRef { node_id, old_ref_id: u64, new_ref_id: u64 }` | set `ref_id`; if `ref_id != 0` force `collapsed = true` |
| 10 | `ChangeStructTypeName{nodeId; oldName,newName}` | `ChangeStructTypeName { node_id, old_name: String, new_name: String }` | set `struct_type_name` |
| 11 | `ChangeClassKeyword{nodeId; oldKeyword,newKeyword}` | `ChangeClassKeyword { node_id, old_keyword: String, new_keyword: String }` | set `class_keyword` |
| 12 | `ChangeOffset{nodeId; oldOffset,newOffset}` | `ChangeOffset { node_id, old_offset: i32, new_offset: i32 }` | set `node.offset`; bump refresh_gen; clear history for node + ALL descendants |
| 13 | `ChangeEnumMembers{nodeId; oldMembers,newMembers}` | `ChangeEnumMembers { node_id, old_members: Vec<(String, i64)>, new_members: Vec<(String, i64)> }` | set `enum_members` |
| 14 | `ChangeOffsetExpr{nodeId; oldExpr,newExpr}` | `ChangeOffsetExpr { node_id, old_expr: String, new_expr: String }` | set `offset_expr` |
| 15 | `ToggleStatic{nodeId; oldVal,newVal}` | `ToggleStatic { node_id, old_val: bool, new_val: bool }` | set `node.is_static` |
| 16 | `ToggleRelative{nodeId; oldVal,newVal}` | `ToggleRelative { node_id, old_val: bool, new_val: bool }` | **NO apply arm** — see note below |
| 17 | `ToggleBigEndian{nodeId; oldVal,newVal}` | `ToggleBigEndian { node_id, old_val: bool, new_val: bool }` | set `node.big_endian` |
| 18 | `ChangeComment{nodeId; oldComment,newComment}` | `ChangeComment { node_id, old_comment: String, new_comment: String }` | set `node.comment` |

The variant (`core.h:1108`):
```rust
#[derive(Debug, Clone)]
pub enum Command {
    ChangeKind(ChangeKind), Rename(Rename), Collapse(Collapse),
    Insert(Insert), Remove(Remove), ChangeBase(ChangeBase), WriteBytes(WriteBytes),
    ChangeArrayMeta(ChangeArrayMeta), ChangePointerRef(ChangePointerRef),
    ChangeStructTypeName(ChangeStructTypeName), ChangeClassKeyword(ChangeClassKeyword),
    ChangeOffset(ChangeOffset), ChangeEnumMembers(ChangeEnumMembers),
    ChangeOffsetExpr(ChangeOffsetExpr), ToggleStatic(ToggleStatic),
    ToggleRelative(ToggleRelative), ToggleBigEndian(ToggleBigEndian),
    ChangeComment(ChangeComment),
}
```
Keep the variant order identical (documentation parity). `std::variant` → tagged enum;
`std::visit` → `match`. **No serde** on `Command` (the undo stack is not persisted).

**Port note on #16 `ToggleRelative` (understand map §B.1, code-derived):** it is in the
variant but the C++ `applyCommand` switch has **no branch** for it — so applying it is a
no-op beyond the entry-level `tree.touch()` + trailing `refresh()`. Replicate as a
defined-but-inert match arm:
```rust
Command::ToggleRelative(_) => { /* no field set — parity with C++ missing branch */ }
```
Add a `// PARITY: C++ applyCommand has no ToggleRelative arm (controller.cpp:2890-3050)`
comment so a future reader doesn't "fix" it.

### B.2 The undo-stack wrapper (`controller.cpp:331-353`, `controller.h:34,87` → hand-rolled)

The C++ uses Qt's `QUndoStack` + a single `QUndoCommand` subclass `RcxCommand`. There is no
Rust crate matching the exact contract (push-executes, self-inverse-via-flag, obsolete
pruning, nested macros), so **hand-roll a small stack** (crate_selection.md + understand map
agree). Recommended location: `src/core/undo.rs` so it is unit-testable without the
controller; the controller owns one instance.

```rust
pub enum Entry { Single(Command), Macro(Vec<Command>) }

pub struct UndoStack {
    done: Vec<Entry>,            // executed, can be undone (index = done.len())
    undone: Vec<Entry>,         // undone, can be redone (top = last pushed back)
    macro_stack: Vec<Vec<Command>>, // open macros (Vec supports NESTED beginMacro)
    clean_index: i64,           // done.len() at last set_clean(); -1 if never / dirtied past it
}
```

**The single hardest invariant: `push` executes immediately.** This is the QUndoStack
contract and the tests depend on it (e.g. `insertNode(...)` then read the tree synchronously,
`test_controller.cpp:265-302`). Because of this, every builder reads old state *before*
pushing (the hex64→int32 empty-name bug fix, `controller.cpp:2107-2117`).

Public API (mirrors the QUndoStack calls used across `controller.cpp`):
```rust
impl UndoStack {
    // push: execute the command NOW, record it, clear redo tail (unless inside a macro).
    fn push(&mut self, ctrl: &mut Controller, cmd: Command);
    fn undo(&mut self, ctrl: &mut Controller);
    fn redo(&mut self, ctrl: &mut Controller);
    fn begin_macro(&mut self, _label: &str);  // label kept for parity/UX only
    fn end_macro(&mut self);
    fn clear(&mut self);                       // empties done + undone (+ open macros)
    fn set_clean(&mut self);                   // clean_index = done.len()
    fn is_clean(&self) -> bool;                // done.len() == clean_index
    fn can_undo(&self) -> bool; fn can_redo(&self) -> bool;
}
```

Because `apply_command` needs `&mut Controller` (it mutates the tree, history, snapshot,
emits status), `push/undo/redo` take `&mut Controller`. If borrow-checker friction arises
(stack lives on the controller), store the `UndoStack` in an `Option`/`take()` swap, or keep
the stack on the document (`RcxDocument`) as the C++ does (`undoStack` is a `RcxDocument`
member). **Recommended:** put `UndoStack` on the document (`Document`), so `controller`
borrows `&mut document.undo_stack` and passes `&mut self` (the controller) to apply — exactly
the C++ shape (`m_doc->undoStack.push(new RcxCommand(this, cmd))`).

Algorithms (cite `controller.cpp:346-353` for the obsolete logic):

```
push(ctrl, cmd):
    if macro open:
        macro_stack.last().push(cmd.clone())
        run cmd forward (apply_command(cmd, is_undo=false))   // execute immediately
        // do NOT clear redo tail until the macro closes
    else:
        ok = apply_command(cmd, is_undo=false)
        undone.clear()                       // push truncates the redo tail
        if !ok && !is_transient(cmd):         // failed non-transient → drop, mark obsolete
            return                            // do not record
        done.push(Single(cmd))
        // if clean_index > done.len() it is now unreachable → set clean_index = -1
        if clean_index as usize > done.len() { clean_index = -1 }

begin_macro(label): macro_stack.push(Vec::new())
end_macro():
    cmds = macro_stack.pop()
    if macro_stack still open: macro_stack.last().extend(cmds)   // NESTED: fold into parent
    else if !cmds.is_empty():
        undone.clear()
        done.push(Macro(cmds))
        if clean_index as usize > done.len() { clean_index = -1 }

undo(ctrl):
    e = done.pop(); if none: return
    match e:
        Single(c): if !apply_command(c, is_undo=true) && !is_transient(c) { /* obsolete: drop, don't re-push */ return }
        Macro(cs): for c in cs.iter().rev() { apply_command(c, is_undo=true); /* obsolete handling per-cmd */ }
    undone.push(e)

redo(ctrl):
    e = undone.pop(); if none: return
    match e:
        Single(c): apply_command(c, is_undo=false)
        Macro(cs): for c in cs.iter() { apply_command(c, is_undo=false) }   // forward order
    done.push(e)
```

**Obsolete / transient (`controller.cpp:342-353`)**: `is_transient(cmd) ==
matches!(cmd, Command::WriteBytes(_))`. When `apply_command` returns `false`:
- For a **transient** command (WriteBytes), KEEP it — a later redo can succeed on a
  re-attached writable provider.
- For a **non-transient** command, treat it as obsolete: drop the entry instead of
  re-recording it (QUndoStack would `setObsolete(true)` and delete on next walk). In
  practice only WriteBytes ever fails today, so the non-transient branch is defensive; model
  it but it won't trigger in tests.

**Macro nesting (understand map §B.2):** `beginMacro` nests (e.g. `deleteRootStruct` opens a
macro that calls `removeNode`; `groupIntoUnion`/`convertToTypedPointer` open macros calling
`insertNode`/`changeNodeKind`, which may themselves macro). A `Vec<Vec<Command>>` macro stack
folds inner macros into the parent so the whole group is one atomic undo entry — matching
QUndoStack's flattening of nested macros into a single composite command.

`RcxCommand::undo/redo` (`controller.cpp:346-353`) has no standalone Rust counterpart — its
behavior is absorbed into `UndoStack::{undo,redo}` (call `apply_command` with the flag; honor
transient/obsolete).

### B.3 The dispatcher `apply_command` (`controller.cpp:2827-3059`)

```rust
// on the Controller (it mutates tree, value history, snapshot; emits status)
pub fn apply_command(&mut self, cmd: &Command, is_undo: bool) -> bool;
```
Returns `false` only on rejection (today: WriteBytes write failure, or
`read_only_override`). On success → `true`.

Flow (verbatim from C++):
1. `self.doc.tree.touch()` at entry — **every** command, incl. WriteBytes/ChangeBase
   (value caches key off `(generation, base_address)`). (`controller.cpp:2835`)
2. Local helpers (port as `&mut self` methods or closures):
   - `clear_node_history(id)` → remove `value_history[id]` and `last_value_addr[id]`.
   - `clear_history_for_adjs(adjs)` → if non-empty: bump `refresh_gen`; build a one-shot
     `childMap` **only if** any adjusted node is a Struct/Array; clear each adjusted node's
     history; for container nodes, BFS-clear all descendants' history (cycle-safe with a
     `visited` set). (`controller.cpp:2847-2888`)
3. `match cmd` — one arm per variant; each looks up `tree.index_of_id(node_id)` and is
   **guarded `if idx >= 0`** so a command targeting a deleted node is a **silent no-op** (no
   crash, no failure, still returns `true`). The arm sets the field to
   `if is_undo { old } else { new }`. (`controller.cpp:2890-3050`)
4. After the match: `if success && !self.suppress_refresh { self.refresh(); }`
   (`controller.cpp:2056`).

Per-arm specifics beyond the simple field-set (already tabulated in B.1; key citations):
- **ChangeKind** (2892): set kind; apply offAdjs; bump `refresh_gen`; **deliberately KEEP**
  value-history for the node (a prior version wiped it; the comment explains keeping the hover
  trend visible after accepting a TypeHint); `clear_history_for_adjs(off_adjs)` still clears
  shifted siblings.
- **Insert** (2921): undo→revert adjs then `nodes.remove(index_of_id(node.id))` +
  `invalidate_id_cache`; redo→`tree.add_node(node)` then apply adjs. `add_node` assigns id if
  0, bumps `next_id`, appends, updates caches, bumps generation. `clear_history_for_adjs`.
- **Remove** (2942): redo applies adjs **first** (before removal changes indices), then
  removes `subtree_indices(node_id)` sorted **descending** (so `remove(idx)` doesn't
  invalidate not-yet-processed indices), clearing each removed node's history, then
  `invalidate_id_cache`. Undo re-adds every node in `subtree` (stored order) then reverts adjs.
- **ChangeBase** (2969): set base+formula, `reset_snapshot()`.
- **WriteBytes** (2973): see B.5.
- **ChangeArrayMeta** (2997): clamp `view_index`.
- **ChangePointerRef** (3005): force `collapsed=true` when resulting `ref_id != 0` (applies to
  undo too if the reverted ref is non-zero).
- **ChangeOffset** (3020): bump `refresh_gen`; clear history for node + all descendants
  (`subtree_indices`).
- Remaining arms are plain guarded field sets.

### B.4 Command builders (`controller.cpp`) — capture old/new before push

These build a `Command`, then `doc.undo_stack.push(ctrl, cmd)` (push executes it). The
builders hold the invariants. Port each as a method on the controller. Cite line numbers.

| C++ builder | Rust signature | Behavior notes |
|---|---|---|
| `insertNode(parentId, offset, kind, name)` (2459) | `fn insert_node(&mut self, parent_id: u64, offset: i32, kind: NodeKind, name: &str)` | if `offset < 0`: auto-place after last sibling — `maxEnd = max over siblings of (offset + size)` (size = `struct_span` for containers else `byte_size`), `n.offset = ceil_div(maxEnd, align) * align` with `align = alignment_for(kind)`. `n.id = tree.reserve_id()` (atomic, BEFORE push). Push `Insert{n}` (no adjs — appends at end). |
| `insertNodeAbove(beforeIdx, kind, name)` (2488) | `fn insert_node_above(&mut self, before_idx: i32, kind, name)` | new node at `before.offset`; `OffsetAdj` for every sibling with `offset >= before.offset`, shifting `+size_for_kind(kind)`. Push `Insert{n, adjs}`. |
| `removeNode(nodeIdx)` (2513) | `fn remove_node(&mut self, node_idx: i32)` | deleted size = `struct_span` for containers else `byte_size`; `deletedEnd = offset + size`. For each sibling (skip self) with `offset >= deletedEnd`, `OffsetAdj` shifting `-size` — **only when `parentId != 0`** (root-level never shifts). Collect full `subtree_indices` into `Vec<Node>`. Push `Remove{nodeId, subtree, adjs}`. |
| `changeNodeKind(nodeIdx, newKind)` (2078) | `fn change_node_kind(&mut self, node_idx: i32, new_kind: NodeKind)` | see pseudocode below — the trickiest builder. |
| `renameNode(nodeIdx, newName)` (2188) | `fn rename_node(&mut self, node_idx: i32, new_name: &str)` | push `Rename{id, node.name, new_name}` |
| `toggleCollapse(nodeIdx)` (2730) | `fn toggle_collapse(&mut self, node_idx: i32)` | push `Collapse{id, collapsed, !collapsed}` |
| `convertRootKeyword(newKeyword)` (2057) | `fn convert_root_keyword(&mut self, new_keyword: &str)` | resolve target (view root, else first root Struct); no-op if unchanged; **refuses if old or new is `"enum"`** (only class↔struct/union allowed). Push `ChangeClassKeyword`. |
| `setNodeValue(nodeIdx, subLine, text, isAscii=false, resolvedAddr=0)` (3061) | `fn set_node_value(&mut self, node_idx, sub_line, text: &str, is_ascii: bool, resolved_addr: u64)` | the WriteBytes builder — see B.5. |
| `duplicateNode(nodeIdx)` (3151) | `fn duplicate_node(&mut self, node_idx: i32)` | refuse Struct/Array; copy at `offset+size`, name `+"_copy"`; `OffsetAdj` for siblings `>= copyOffset` shifting `+size` (only when `parentId != 0`); push `Insert{n, adjs}`. |
| `splitHexNode(nodeId)` (3332) | `fn split_hex_node(&mut self, node_id: u64)` | macro "Split Hex node": Remove original (subtree=[node]), Insert `lo` (halfKind @offset, same name), Insert `hi` (halfKind @offset+half, name+`"_hi"`). half: Hex128→Hex64, Hex64→Hex32, Hex32→Hex16, Hex16→Hex8; else return. |
| `joinHexNodes(nodeId, targetKind)` (3598) | `fn join_hex_nodes(&mut self, node_id: u64, target: NodeKind)` | save offset/parent/name by value; require `tgtSz > curSz`. Scan adjacent hex siblings at exactly `nextOff` until `accumulated >= tgtSz` (status-hint + bail if not enough). macro "Join Hex nodes": Remove each merged (reverse order), Insert one joined node; transfer selection from merged ids → joined id. |
| `convertToTypedPointer(nodeId)` (3181) | `fn convert_to_typed_pointer(&mut self, node_id: u64)` | macro "Change to ptr*": ptrKind by `pointer_size>=8`; unique struct name `NewClass`/`NewClass_2`/…; new root struct (`classKeyword="class"`, name "instance") + 16 hex children sized to arch; `changeNodeKind`→ptr (if needed); Insert struct; Insert children; `ChangePointerRef{nodeId, oldRefId, struct.id}`. |
| `groupIntoUnion(nodeIds)` (2589) | `fn group_into_union(&mut self, node_ids: &HashSet<u64>)` | `< 2` → no-op; verify shared parent; sort by offset; macro "Group into union": Remove each (reverse, with subtree), Insert union Struct (`classKeyword="union"`) at min offset, re-insert each node + its subtree as children at offset 0 with fresh ids. |
| `deleteRootStruct(structId)` (2547) | `fn delete_root_struct(&mut self, struct_id: u64)` | only root Struct; macro "Delete root struct": first push `ChangePointerRef{n.id, refId, 0}` for every node whose `refId == structId`; re-lookup index; `removeNode`; switch view to next root if it was the viewed root. |
| `batchRemoveNodes(indices)` (5044) | `fn batch_remove_nodes(&mut self, indices: &[i32])` | id-set; `normalize_prefer_ancestors`; clear selection; `suppress_refresh=true`; macro "Delete N nodes"; `remove_node` each; `suppress_refresh=false`; one `refresh()`. |
| `batchChangeKind(indices, newKind)` (5068) | `fn batch_change_kind(&mut self, indices: &[i32], new_kind: NodeKind)` | id-set; `normalize_prefer_descendants`; save selection; `suppress_refresh=true`; macro "Change type of N nodes"; `change_node_kind` each; restore selection; one `refresh()`. |

**`change_node_kind` pseudocode (`controller.cpp:2078-2186`)** — the reversibility-critical builder:
```
node = tree.nodes[node_idx]
oldSize = node.byte_size(); if 0 && container: oldSize = tree.struct_span(node.id)
newSize = { tmp = node.clone(); tmp.kind = new_kind; tmp.byte_size() }
if new_kind is Struct or Array: newSize = 0     // size deferred to follow-up commands

if newSize > 0 && newSize < oldSize:            // SHRINK path
    gap = oldSize - newSize; parentId = node.parent_id; baseOffset = node.offset + newSize
    was = suppress_refresh; suppress_refresh = true
    begin_macro("Change type")
    // capture BEFORE push (push mutates node.kind in place):
    origName = node.name; origOffset = node.offset
    needsRename = is_hex_node(node.kind) && !is_hex_node(new_kind)
    push(ChangeKind{node.id, node.kind, new_kind, []})          // no adjs
    if needsRename: push(Rename{node.id, origName, format!("field_{:04x}", origOffset)})
    hexToHex = is_hex_node(node.kind) && is_hex_node(new_kind)  // NOTE: node.kind already mutated by push!
    // (the C++ reads node.kind here AFTER the push — so node.kind == new_kind now;
    //  is_hex_node(new_kind) on both sides ⇒ hexToHex effectively = is_hex_node(new_kind).
    //  Reproduce by reading the (now-mutated) kind, or equivalently compute from new_kind+the
    //  pre-push old kind captured above. MATCH THE C++: hexToHex uses node.kind (post-push) && new_kind.)
    padOffset = baseOffset
    while gap > 0:
        if hexToHex: padKind=new_kind; padSize=newSize
        elif gap>=8: Hex64,8  elif gap>=4: Hex32,4  elif gap>=2: Hex16,2  else: Hex8,1
        insert_node(parentId, padOffset, padKind, format!("pad_{:02x}", padOffset))
        padOffset += padSize; gap -= padSize
    end_macro(); suppress_refresh = was; if !suppress_refresh: refresh()
else:                                            // SAME or GROW path
    delta = newSize - oldSize
    adjs = []
    if delta != 0 && oldSize > 0 && newSize > 0:
        oldEnd = node.offset + oldSize
        for sib in siblings(parentId), sib != self, sib.offset >= oldEnd:
            adjs.push(OffsetAdj{sib.id, sib.offset, sib.offset + delta})
    needsRename = is_hex_node(node.kind) && !is_hex_node(new_kind)
    if needsRename: begin_macro("Change type")
    push(ChangeKind{node.id, node.kind, new_kind, adjs})
    if needsRename: push(Rename{node.id, node.name, format!("field_{:04x}", node.offset)}); end_macro()
```
**Subtlety to preserve:** in the shrink path the C++ reads `node.kind` for `hexToHex`
*after* the `ChangeKind` push has already mutated it (the same in-place-mutation hazard the
`origName` capture guards against). The net effect: `hexToHex = is_hex_node(new_kind) &&
is_hex_node(new_kind)` because both `node.kind` (now == new_kind) and `new_kind` are hex.
Replicate the C++ ordering precisely (capture pre-push state explicitly, then compute
`hexToHex` from the post-push kind == new_kind) so same-size hex padding stays reversible
(tested by `testSpaceCycleFullCircle` / `testSpaceRapidCycleNoCorruption`).

### B.5 Failure handling & error strategy

**Only `WriteBytes` is fallible.** The error strategy is *value-typed, not `Result`-typed* at
the dispatcher boundary: `apply_command` returns `bool` (matching the C++). Wrap the
underlying provider write (which returns `Result`/`bool`) and collapse to the `bool`.

`setNodeValue` (the WriteBytes builder, `controller.cpp:3061-3149`) — **test-the-write-then-push**:
```
if node_idx invalid: return
if !provider.is_writable(): return
if read_only_override: return            // tutorial/self-attach safety, silent
addr = resolved_addr if nonzero else base_address + compute_offset(node_idx)  // bail if signed offset < 0
// Vec/Mat components: editKind = Float, addr += subLine*4 (Mat4x4: subLine in 0..16)
newBytes = if is_ascii { parse_ascii_value(text, size_for_kind(editKind)) }
           else { parse_value(editNode{kind=editKind, big_endian}, text) }  // carries endian
if parse failed: return
if UTF8/UTF16: pad/truncate newBytes to node.byte_size() (left(full) then zero-fill)
if newBytes empty: return
if !provider.is_readable(addr, newBytes.len()): return
oldBytes = provider.read_bytes(addr, newBytes.len())          // for undo
writeOk = snapshot ? snapshot.write(addr, &newBytes) : provider.write_bytes(addr, &newBytes)
if !writeOk: warn; refresh(); return                          // NO push — never push a failing cmd
push(WriteBytes{addr, oldBytes, newBytes})                    // redo re-writes (harmless)
```
On a *later* `apply_command(WriteBytes, ...)` failure (re-attached read-only provider, page
gone), the arm: emits a `status_hint("Write rejected at 0x{addr:x} — removing from history")`,
sets `success=false`, and **no refresh** (keep last good visual); the snapshot/provider is NOT
patched on failure, so a subsequent undo can't push stale `oldBytes` over never-written memory.
The stack keeps the (transient) entry.

**Error types per crate_selection.md:** subsystem-level fallible glue uses `thiserror`; the
provider boundary returns `Result<usize>` (`Provider::write/read`). For `apply_command`, keep
the `bool` return (parity). Use `tracing::warn!` in place of `qWarning`. `status_hint` is a
controller signal → in Rust an event/callback (or a `Vec<String>` test sink) the UI observes;
the headless tests can capture it via a recorded-status vector on the controller.

### B.6 Controller bookkeeping fields (`controller.h` → `controller.rs`)

| C++ | Rust | role |
|---|---|---|
| `bool m_suppressRefresh` (273) | `suppress_refresh: bool` | batch builders set it so only one recompose runs; `apply_command` skips trailing `refresh()` when true |
| `uint64_t m_refreshGen` (322) | `refresh_gen: u64` | bumped on layout-changing commands to invalidate in-flight async reads (false-heat avoidance); pure bookkeeping |
| `bool m_readOnlyOverride` (281) | `read_only_override: bool` | tutorial/self-attach safety; blocks WriteBytes even on undo/redo |
| `QHash<uint64_t,ValueHistory> m_valueHistory` | `value_history: HashMap<u64, ValueHistory>` | live-value ring; commands only *clear* entries |
| `QHash<uint64_t,uint64_t> m_lastValueAddr` | `last_value_addr: HashMap<u64,u64>` | cleared alongside history |
| `QTimer* m_cycleMacroTimer` + `bool m_cycleMacroOpen` (356-357) | `cycle_macro_open: bool` (+ explicit idle close) | 800ms type-cycle grouping; see B.7 |
| `NodeTree::m_generation` (`core.h:427`) | `generation: u64` on `NodeTree` | structural-change counter; `touch()` bumps; cache keys read it |

### B.7 The 800ms type-cycle macro (`controller.cpp:990-1006`) — the one timing-dependent bit

Rapid ←/→ type-cycle keypresses fold into ONE undo macro via an 800ms single-shot `QTimer`:
first press opens "Cycle type", each press restarts the timer, timeout closes the macro.
Multi-select cycle opens an inner macro "Cycle type for N nodes".

**Rust port:** this is UI-glue. For the **headless core** model it as explicit open/close
without a real timer:
```rust
fn cycle_type(&mut self, direction: i32) {
    if !self.cycle_macro_open { self.doc.undo_stack.begin_macro("Cycle type"); self.cycle_macro_open = true; }
    // ... compute target kind(s), changeNodeKind/batchChangeKind ...
    // timer.restart()  -> in headless, expose end_cycle_macro_on_idle() the UI calls on timeout
}
fn end_cycle_macro_on_idle(&mut self) {
    if self.cycle_macro_open { self.doc.undo_stack.end_macro(); self.cycle_macro_open = false; }
}
```
In the gpui UI layer (`ui` feature), wire `end_cycle_macro_on_idle` to a 800ms debounce timer.
The headless tests don't exercise the timer; they call the explicit close. Same-size variant
list construction (`controller.cpp:976-987`): iterate `K_KIND_META`, keep kinds with matching
`size` that are not containers, excluding string kinds (unless origin is string) and vector
kinds (unless origin is vector); rotate `(i + direction + n) % n`.

---

## C. Model dependencies (shared with `core` spec — keep consistent)

The commands mutate `Node`/`NodeTree`; compose reads them. These belong to the `core` module
(its own porting spec owns the full mapping). This subsystem requires these `core` items to
exist with the documented behavior:

- `NodeKind` `#[repr(u8)]` enum (31 kinds) + `const K_KIND_META: [KindMeta; 31]` indexed by
  `kind as usize`; helpers `size_for_kind`, `lines_for_kind`, `alignment_for`,
  `kind_to_string`/`kind_from_string`, and predicates `is_hex_node`, `is_pointer_kind`,
  `is_container_kind`, `is_string_kind`, `is_vector_kind`, `is_matrix_kind`. (`core.h:24-97`)
- `Node` with fields `id,kind,name,struct_type_name,class_keyword,parent_id,offset,is_static,
  offset_expr,is_relative,array_len,str_len,collapsed,ref_id,element_kind,ptr_depth,view_index,
  enum_members: Vec<(String,i64)>, bitfield_members, comment, big_endian`; methods
  `byte_size()`, `total_byte_size(tree)`, `resolved_class_keyword()`, `is_union/is_bitfield/is_enum`. (`core.h:210-363`)
- `NodeTree` with `nodes: Vec<Node>`, `base_address` (default `0x0040_0000`),
  `base_address_formula`, `pointer_size=8`, caches, `generation=1`. Methods the stack needs:
  `add_node`, `reserve_id`, `invalidate_id_cache`, `touch`, `index_of_id` (returns -1 on miss
  → drives guarded no-ops), `children_of`, `subtree_indices` (cycle-safe), `compute_offset`,
  `struct_span`, `normalize_prefer_ancestors`, `normalize_prefer_descendants`. (`core.h:408-835`)
- `ValueHistory` (10-slot ring). The stack only clears entries.

**The `core` spec is authoritative for these.** If discrepancies arise, the `core` spec wins;
this spec consumes them.

---

## D. Subtle behaviors a faithful port MUST preserve (test- & code-derived)

(Condensed from understand map §D; each is asserted by a named test in §E.)

1. **Push executes immediately.** `insert_node` then synchronous tree read works
   (`testInsertAndRemoveNode`).
2. **Self-inverse via flag.** undo/redo are the *same* `apply_command` with `is_undo`
   flipped; old+new both stored; round-trips restore exact bytes/names/kinds.
3. **Guarded no-op on missing node.** Every arm checks `index_of_id >= 0`; a command for a
   since-deleted node silently does nothing and returns `true` (matters inside macros).
4. **Remove ordering.** redo: adjs first, remove descending; undo: re-add then revert adjs.
   Insert mirrors.
5. **OffsetAdj only for siblings at/after the affected end; never root-level on removal**
   (`controller.cpp:2526`). Insert-above shifts `>= before.offset`; remove shifts
   `>= deletedEnd` by `-size`; resize shifts `>= oldEnd` by `delta`.
6. **WriteBytes is test-the-write-then-push;** undo restores `oldBytes`; on apply failure no
   refresh, no snapshot patch, command kept (transient).
7. **ChangeKind keeps value history; ChangeOffset/Insert/Remove clear it** for
   affected nodes+descendants and bump `refresh_gen`.
8. **changeNodeKind shrink is reversible** because hex→hex pads with *same-size* hex nodes;
   non-hex pads largest-first.
9. **ChangePointerRef forces `collapsed=true`** whenever resulting `ref_id != 0`.
10. **ChangeArrayMeta clamps `view_index`** to new `array_len`.
11. **Macros are atomic**; `suppress_refresh` collapses a batch to one recompose; nested
    macros flatten.
12. **`touch()` fires for every command;** `ChangeBase` also `reset_snapshot()`.
13. **ToggleRelative has no apply arm** — inert for parity.
14. **Compose UTF-16 column math** — index by UTF-16 units (`Utf16Buf`), not bytes/chars.
15. **Compose orphan BFS** never lands an orphan at offset 0.

---

## E. TEST PLAN

The behavioral oracle for this subsystem is **`tests/test_controller.cpp`** (undo/redo,
~65 slots) and **`tests/test_compose.cpp`** (renderer, ~60 slots). Per `_oracle/RESULTS.md`,
`test_controller` is *not* in the captured headless oracle set (it links the editor/UI shell
and `QApplication`), but its assertions are **overwhelmingly pure logic** (tree state after a
command). **Port them as pure logic tests against the headless core** (no gpui): build a
`Document` (tree + `BufferProvider`) + a headless `Controller`, run the builder, assert tree
state. Drop the `QApplication::processEvents()` / editor-overlay lines — they are UI plumbing
the synchronous Rust apply makes unnecessary. `test_compose` IS in the headless oracle and has
golden fixtures (`_oracle/fixtures/eprocess_normal.txt`, `eprocess_compact.txt`).

Run all with `cargo test --no-default-features`.

### E.1 Undo/redo command tests (from `test_controller.cpp`)

Translate each slot to a `#[test]`. Shared fixtures port directly:
- `buildSmallTree` (`:32`) → `fn build_small_tree() -> NodeTree`: root Struct "TestStruct" +
  fields UInt32@0, Float@4, UInt8@8, Hex16@9, Hex8@11, Hex32@12. `base_address = 0`.
- `makeSmallBuffer` (`:63`) → `fn make_small_buffer() -> Vec<u8>`: 64 bytes; `u32@0=0xDEADBEEF`,
  `f32@4=3.14`, `u8@8=0x42`, `u32@12=0xCAFEBABE`. **Note exact f32 bytes** so round-trips
  match; assert with epsilon as the C++ does (`< 0.01`).
- A `BaseAwareProvider` (`:12`) → trivial Rust struct implementing `Provider` with configurable
  base.

| C++ test (line) | Rust `#[test]` | Asserts |
|---|---|---|
| `testSetNodeValueWritesData` (117) | `set_node_value_writes_data` | write "42" → provider bytes at addr = 42 LE |
| `testSetNodeValueUndoRedo` (144) | `set_node_value_undo_redo` | write→undo restores 0xDEADBEEF→redo restores 99 |
| `testSetNodeValueFloat` (187) | `set_node_value_float` | "1.5"→1.5f; undo→≈3.14 |
| `testRenameNode` (220) | `rename_node` | name set; undo restores; redo re-applies |
| `testChangeNodeKind` (245) | `change_node_kind_basic` | UInt32→Float; undo→UInt32 (same-size, no padding) |
| `testInsertAndRemoveNode` (265) | `insert_and_remove_node` | insert Hex64@16 (synchronous read works ⇒ **push-executes**); remove; undo-remove restores |
| `testSetNodeValueHex` (305) | `set_node_value_hex` | "AA BB CC DD" → 4 bytes; undo restores |
| `testToggleCollapse` (448) | `toggle_collapse` | collapse/expand + 2× undo round-trip |
| `testQuickTypeChangeHexSameSize` (914) | `quick_type_change_hex_same_size` | Hex32→Int32, id survives |
| `testQuickTypeChangeHexShrink` (928) | `quick_type_change_hex_shrink` | Hex32→Hex16 keeps offset; **padding node at offset+2** exists |
| `testQuickTypeChangeHexGrow` (956) | `quick_type_change_hex_grow` | Hex32→Hex64 keeps offset; siblings shift +4 |
| `testCycleSameSizeTypeVariants` (976) | `cycle_same_size_type_variants` | same-size variant list contains Hex32/Int32/Float; forward cycle applies |
| `testDeleteKeyRemovesNode` (1009) | `delete_key_removes_node` | removeNode drops id, shrinks tree |
| `testDuplicateNode` (1023) | `duplicate_node` | `field_float_copy` appears, count+1 |
| `testSplitHexNode` (1040) | `split_hex_node` | Hex32@12 → two Hex16 @12,@14; original gone |
| `testSplitHexNodeUndo` (1058) | `split_hex_node_undo` | undo restores original Hex32, count unchanged (**macro atomic**) |
| `testGroupIntoUnion` (1073) | `group_into_union` | union node with 2 children both at offset 0 |
| `testToggleCollapseRoundTrip` (1101) | `toggle_collapse_round_trip` | collapse true/false by re-lookup |
| `testInsertNodeAutoOffset` (1114) | `insert_node_auto_offset` | offset<0 auto-places after last sibling (>0) |
| `testBatchChangeKind` (1130) | `batch_change_kind` | both fields → Hex64 |
| `testConvertToTypedPointer` (1147) | `convert_to_typed_pointer` | becomes Pointer64/32 with non-zero refId; **atomic macro** |
| `testRenameNodeUndoRedo` (1163) | `rename_node_undo_redo` | by-id rename + undo + redo |
| `testInsertNodeAboveShiftsOffsets` (1180) | `insert_node_above_shifts_offsets` | Hex64 above Float@4 → Float now @12 |
| `testDeleteRootStruct` (1198) | `delete_root_struct` | 2nd root struct deleted; original survives |
| `testMoveNodeSwapsOffsets` (1217) | `move_node_swaps_offsets` | **macro of two ChangeOffset** swaps offsets; undo restores both |
| `testChangeBaseAddress` (1256) | `change_base_address` | push ChangeBase; base updates; undo restores |
| `testChangeArrayMeta` (1266) | `change_array_meta` | element/len change; undo restores |
| `testChangeClassKeyword` (1293) | `change_class_keyword` | struct→class; undo→struct |
| `testChangeComment` (1305) | `change_comment` | comment set; undo clears |
| `testCollapseExpandAll` (1320) | `collapse_expand_all` | macro of N Collapse cmds; undo re-collapses |
| `testBatchRemoveMultipleNodes` (1388) | `batch_remove_multiple_nodes` | both ids gone; undo restores both |
| `testSetNodeValueBool` (1412) | `set_node_value_bool` | "true" → byte 1 |
| `testSetNodeValueNegativeInt` (1428) | `set_node_value_negative_int` | "-128" → i8 -128 |
| `testMultiSelectBatchCycleType` (1451) | `multi_select_batch_cycle_type` | batch→Hex32; undo restores UInt32/Float |
| `testSpaceResizeWrapAndMultiSelect` (1540) | `space_resize_wrap_and_multi_select` | hex cycle wrap arithmetic; batch Hex32→Hex64 |
| `testSpaceCycleFullCircle` (1580) | `space_cycle_full_circle` | hex64→hex8 (shrink+pad, **total 8 bytes, no overlap**), then join hex8→16→32→64; back to 1 child of size 8 |
| `testSpaceNoOverlapAfterGrow` (1668) | `space_no_overlap_after_grow` | join hex32@8 + hex32@12 → hex64@8; no overlap; 2 nodes |
| `testSpaceSelectionSurvivesJoin` (1733) | `space_selection_survives_join` | join transfers selection to new node |
| `testSpaceRapidCycleNoCorruption` (1779) | `space_rapid_cycle_no_corruption` | 20 join/shrink presses; **never overlapping offsets; total always 8 bytes** (the reversibility stress test) |
| `testCycleExcludesStringAndVectorTypes` (1493) | `cycle_excludes_string_and_vector` | variant list excludes UTF8 / Vec2 for non-string/non-vector origin |

Tests that also touch other subsystems but are exercised here (port as logic tests, asserting
tree/JSON state only): `testGeneratorPrepareChildren` (1362 — `struct_span` excludes static
field), `testNodeToJsonOmitsDefaults`/`testNodeToJsonIncludesIsRelative` (1472/1484 — these
belong to the `core` serde spec; cross-list), `testValueHistoryClear` (1441 — `core`).

**UI-only slots to skip in the headless core** (re-express later under `ui`):
`testInlineEditRoundTrip` (339, drives QScintilla `SCI_REPLACESEL`/key events),
`testValueHistoryPopupOnlyDuringEdit` (471), `testSourceSwitch*` (410/432 — these assert pure
base-address logic and SHOULD be ported as logic tests: fresh doc adopts provider base, loaded
doc keeps its base), and the static-field/heat slots that overlap `test_static_fields`
(`testAddStaticField*`, `testChangeStaticFieldExpression`, etc. — covered by the
`static-fields`/`core` specs; cross-list rather than duplicate).

### E.2 Composition tests (from `test_compose.cpp`)

`test_compose` IS in the headless oracle (`_oracle/RESULTS.md`: 60/0/1, golden fixtures
written). Two assertion styles:
1. **Structural** — build a tree + `BufferProvider`, call `compose(...)`, assert on
   `result.text` substrings and `result.meta[i]` fields (the bulk; ~55 slots).
2. **Golden-file** — the compact/normal EPROCESS dumps. Port `testCompactColumns` (and the
   normal-column counterpart) to load `_design/_oracle/fixtures/eprocess_normal.txt` /
   `eprocess_compact.txt` and assert `compose(...).text == fixture` (byte-exact). These are
   the strongest parity checks for the renderer. (The `MMPFN.rcx` slot is skipped upstream —
   fixture absent — so skip it too with a `#[ignore]` + comment.)

| C++ test (line) | Rust `#[test]` | Asserts |
|---|---|---|
| `testBasicStruct` (11) | `basic_struct` | header/field/footer text present |
| `testVec3SingleLine` (60) | `vec3_single_line` | Vec3 renders on one line |
| `testHexNodeCompose` (94) | `hex_node_compose` | hex row format + ASCII preview |
| `testNullPointerMarker` (123) | `null_pointer_marker` | `nullptr` text on null ptr |
| `testUnreadablePointerNoRead` (186) | `unreadable_pointer_no_read` | no read attempted; null-template shown |
| `testFoldLevels` (221) | `fold_levels` | `fold_level` per depth (FOLD_LEVEL_BASE + depth, header flag on containers) |
| `testNestedStruct` (261) | `nested_struct` | child rows indented |
| `testPointerDerefExpansion` (333) | `pointer_deref_expansion` | typed ptr expands target struct at deref'd addr |
| `testPointerDerefNull` (424) | `pointer_deref_null` | null deref → template, no crash |
| `testPointerDerefCollapsed` (485) | `pointer_deref_collapsed` | collapsed ptr → no expansion |
| `testPointerDerefCycle` (540) | `pointer_deref_cycle` | A→B→A guarded (cycle constant) |
| `testPointerMutualCycleAtoB` (587) | `pointer_mutual_cycle` | A↔B guarded |
| `testStructFooterSimple` (611) | `struct_footer_simple` | footer `}` row |
| `testLineMetaHasNodeId` (657) | `line_meta_has_node_id` | `meta[i].node_id` carried |
| `testArrayHeaderFormat` (690) | `array_header_format` | count + element-kind in header |
| `testArrayHeaderCharTypes` (746) | `array_header_char_types` | char arrays render as strings |
| `testArraySpansClickable` (791) | `array_spans_clickable` | array nav spans `[start,end)` |
| `testArrayWithStructChildren` (841) | `array_with_struct_children` | struct-array uses children |
| `testArrayCollapsedNoChildren` (924) | `array_collapsed_no_children` | collapsed array → no element rows |
| `testArrayCountRecompose` (976) | `array_count_recompose` | change count → re-render reflects |
| `testPrimitiveArrayElements` (1033) | `primitive_array_elements` | 4 synthesized element rows |
| `testPrimitiveArrayCollapsed` (1105) | `primitive_array_collapsed` | collapsed → none |
| `testStructArrayStillUsesChildren` (1139) | `struct_array_still_uses_children` | struct array path |
| `testPointerDefaultVoid` (1195) / `testPointer32DefaultVoid` (1241) | `pointer_default_void[ _32]` | void* default display |
| `testPointerDisplaysTargetName` (1271) | `pointer_displays_target_name` | target struct name shown |
| `testPointerTargetUsesNameWhenNoTypeName` (1336) | `pointer_target_uses_name` | falls back to `name` |
| `testPointerSpans` (1377) / `testPointerVoidSpans` (1435) | `pointer[_void]_spans` | clickable spans |
| `testPointerToPointerChain` (1482) | `pointer_to_pointer_chain` | ptrDepth deref chain |
| `testAllStructsResolvedAsPointerTargets` (1682) | `all_structs_resolved_as_pointer_targets` | any struct usable as ptr target |
| `testPointerRefIdToDeletedStruct` (1743) | `pointer_refid_to_deleted_struct` | dangling refId handled |
| `testPointerCollapsedNoExpansion` (1775) | `pointer_collapsed_no_expansion` | collapsed ptr no expand |
| `testPointerWidthComputation` (1837) | `pointer_width_computation` | column width math |
| (compact/normal column slots) | `compose_eprocess_normal_golden`, `compose_eprocess_compact_golden` | **byte-exact** vs `_oracle/fixtures/eprocess_{normal,compact}.txt` |
| union/enum/bitfield/static/tree-line/comment/brace-wrap slots (per tests-catalog §test_compose) | one `#[test]` each | header keyword, member rendering, span sizes, `is_static_line` flag, tree connectors at depth, comment/brace-wrap output |

`test_chips.cpp` and `test_overlay_null_rtti.cpp` (both `reclass-compose`) extend the compose
test plan: port the chip-ordering (Enum→TypeHint→Rtti→Comment), `find_chip`, multi-line
comment middle-dot collapse, `show_*=false` suppression, and the null-vtable CTA chip tests.
**RTTI/Symbol chips never fire in headless tests** (no module hook), so those specific chip
slots are skipped/`#[ignore]` upstream-style; keep enum/typehint/comment chips covered.

### E.3 Step ordering for verification

Implement in small, independently-verifiable steps (each ends with green
`cargo test --no-default-features` for the listed tests):

1. **`core/command.rs`** — `OffsetAdj`, the 18 command structs, `Command` enum. No logic.
   *Verify:* compiles; a trivial `Debug`/`Clone` round-trip test.
2. **`core/undo.rs`** — `UndoStack`, `Entry`, `is_transient`, push-executes, macro
   nesting, clean index, obsolete pruning — driven by a **mock apply** closure (so it's
   testable without the controller). *Verify:* push-executes ordering, redo-tail truncation,
   nested-macro flatten, undo/redo of a macro, clean/dirty transitions.
3. **`controller.rs` `apply_command`** — the `match` with all 18 arms + `clear_node_history`
   / `clear_history_for_adjs` helpers; the trailing-refresh gate. *Verify:* the simple-field
   command tests: `rename_node`, `change_class_keyword`, `change_comment`,
   `change_array_meta`, `change_base_address`, `toggle_collapse` (direct-push variants from
   `test_controller`).
4. **Insert/Remove arms + builders** — `insert_node`, `insert_node_above`, `remove_node`,
   `duplicate_node`. *Verify:* `insert_and_remove_node`, `insert_node_auto_offset`,
   `insert_node_above_shifts_offsets`, `delete_key_removes_node`, `duplicate_node`,
   `move_node_swaps_offsets` (ChangeOffset arm), `batch_remove_multiple_nodes`.
5. **`change_node_kind`** (same/grow/shrink + reversibility) + `split_hex_node` +
   `join_hex_nodes`. *Verify:* `change_node_kind_basic`, `quick_type_change_hex_*`,
   `cycle_same_size_type_variants`, `cycle_excludes_string_and_vector`, `split_hex_node[_undo]`,
   `space_cycle_full_circle`, `space_no_overlap_after_grow`, `space_rapid_cycle_no_corruption`.
6. **WriteBytes + `set_node_value`** (test-the-write-then-push, failure handling, transient).
   *Verify:* `set_node_value_writes_data`, `set_node_value_undo_redo`, `set_node_value_float`,
   `set_node_value_hex`, `set_node_value_bool`, `set_node_value_negative_int`.
7. **Batch / union / pointer macros** — `batch_change_kind`, `group_into_union`,
   `convert_to_typed_pointer`, `delete_root_struct`, `convert_root_keyword`, the cycle-type
   macro (explicit open/close). *Verify:* `batch_change_kind`, `multi_select_batch_cycle_type`,
   `group_into_union`, `convert_to_typed_pointer`, `delete_root_struct`,
   `collapse_expand_all`, `space_selection_survives_join`, `space_resize_wrap_and_multi_select`.
8. **`core/line.rs`** — `LineMeta`, `LineChip`, `LineKind`, `ChipKind`, `LayoutInfo`,
   `LineGeometry`, `ColumnSpan`, `EditTarget`, column consts, span helpers, `Utf16Buf`.
   *Verify:* `LineGeometry::for_line` + span helper unit tests.
9. **`compose.rs` skeleton** — `compose()` + absOffset BFS + hex tier + column widths +
   CommandRow + roots walk; `compose_leaf` first. *Verify:* `basic_struct`, `hex_node_compose`,
   `line_meta_has_node_id`, `fold_levels`.
10. **`compose_parent`** — nested struct/array, footer, union/enum/bitfield, primitive-array,
    static fields. *Verify:* nested/array/union/enum/bitfield/static compose slots.
11. **`compose_node` pointer path** — deref expansion, cycle guards, null/unreadable,
    ptrDepth chains, target-name display, spans. *Verify:* all `pointer_*` compose slots.
12. **Chips** — enum/typehint/comment chips, ordering, sanitize, null-vtable CTA.
    *Verify:* `test_chips` + `test_overlay_null_rtti` ports (RTTI/Symbol `#[ignore]`).
13. **Golden parity** — `compose_eprocess_normal_golden`, `compose_eprocess_compact_golden`
    byte-exact vs `_oracle/fixtures/`. *Verify:* exact-match (the final renderer gate).

---

## F. Error-handling strategy (summary)

- **Dispatcher boundary:** `apply_command -> bool` (parity; `false` = rejected). Only
  `WriteBytes`/`read_only_override` produce `false`.
- **Provider boundary:** `Result<usize, ProviderError>` (thiserror) collapsed to `bool` at
  the WriteBytes arm; no panics on write failure.
- **Missing-node arms:** silent no-op (guarded `index_of_id >= 0`), return `true`.
- **Builders:** early-return on invalid index / non-writable / parse failure (no command
  pushed) — never push a command that would fail.
- **Compose:** total/pure; no `Result` — out-of-range reads fall back to `NullProvider`
  zeros; cycle guards prevent infinite recursion; no panics.
- **Logging:** `tracing::warn!` replaces `qWarning`; `status_hint` is an
  observable controller event (test sink in headless builds).
