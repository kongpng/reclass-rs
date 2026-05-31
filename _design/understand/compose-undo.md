# Subsystem: Composition + Undo/Redo Command Stack (`compose-undo`)

> Faithful-port structural/behavioral map. Source of truth: the original public C++/Qt6 Reclass
> source under `/home/loke/reclass-cpp`. Read this instead of the C++.
>
> **Scope correction (important):** The task brief lists files `src/compose.cpp` + `src/core.h`
> and 15 command types "heavily tested in test_compose.cpp". After reading the code: the command
> *definitions* live in `core.h` (`namespace cmd` + the `Command` variant), but the *undo/redo
> stack machinery* (the `applyCommand` dispatcher, `RcxCommand`, batch macros, command *builders*)
> lives in `src/controller.cpp` / `src/controller.h`, and the command behaviors are tested in
> **`tests/test_controller.cpp`** (NOT test_compose.cpp — that one tests the `compose()` renderer).
> `compose.cpp` is the *composition / rendering* engine (NodeTree → text+LineMeta). This report
> covers both halves of the `compose-undo` key: (A) the composition pipeline, and (B) the
> undo/redo command stack. The command stack is the porting-critical, pure-logic part.
>
> There are actually **18** command variants in the `Command` `std::variant` (the brief's "15"
> omits `ToggleRelative`, `ToggleBigEndian`, `ChangeComment`). All 18 are documented below.

---

## 0. Purpose

Two cooperating concerns:

1. **Composition (`compose.cpp`)** — pure transform from a `NodeTree` document model + a `Provider`
   (memory data source) into a flat, line-oriented render: a single text blob plus a parallel
   `QVector<LineMeta>` describing each line (offsets, fold levels, chips, column geometry, markers).
   Output is consumed by the Scintilla-backed editor. Recursive, with cycle guards for both struct
   recursion and pointer-dereference recursion. This is read-only over the tree.

2. **Undo/redo command stack (`core.h` + `controller.cpp`)** — every mutation to the document model
   is expressed as one of 18 small value-type command structs (each carrying *both* old and new
   state, i.e. fully self-inverse). Commands are pushed onto a Qt `QUndoStack`; a single dispatcher
   `RcxController::applyCommand(const Command&, bool isUndo)` applies a command either forward
   (`isUndo=false`) or in reverse (`isUndo=true`). Batches of commands are grouped into atomic
   "macros". This is the pure, behavior-critical part for the port.

---

## A. COMPOSITION (`src/compose.cpp`)

### A.1 Key types

#### `ComposeState` (file-local struct, `compose.cpp:71-194`)
Mutable accumulator threaded through the recursion. Fields:

| field | Qt type | Rust equivalent | meaning |
|---|---|---|---|
| `text` | `QString` | `String` | accumulated output buffer (UTF-16 in Qt; UTF-8 `String` in Rust — see A.5) |
| `meta` | `QVector<LineMeta>` | `Vec<LineMeta>` | per-line metadata, parallel to lines in `text` |
| `lineStarts` | `QVector<int>` | `Vec<usize>` | char offset of each line's start in `text` |
| `maxLineLen` | `int` | `i32` | longest line in chars, trailing spaces excluded (drives scroll width) |
| `visiting` | `QSet<uint64_t>` | `HashSet<u64>` | struct-recursion cycle detection (node ids) |
| `ptrVisiting` | `QSet<qulonglong>` | `HashSet<u64>` | pointer-expansion cycle guard (keyed by `pBase ^ refId*golden`) |
| `virtualPtrRefs` | `QSet<uint64_t>` | `HashSet<u64>` | refIds currently being virtually expanded |
| `currentLine` | `int` | `i32` | running line index |
| `typeW`/`nameW` | `int` | `i32` | global type/name column widths (fallback) |
| `offsetHexDigits` | `int` | `i32` | hex-digit tier for offset margin (4/8/12/16) |
| `baseEmitted` | `bool` | `bool` | only first root struct shows base address (suppresses root header) |
| `compactColumns`/`treeLines`/`braceWrap`/`typeHints`/`showComments`/`showRtti`/`showEnumChips` | `bool` | `bool` | render-option flags |
| `symbolLookup` | `SymbolLookupFn` (`std::function<QString(uint64_t)>`) | `Box<dyn Fn(u64)->String>` / `Option<...>` | optional PDB symbol callback |
| `siblingStack` | `QVector<bool>` | `Vec<bool>` | per-depth: more siblings follow (for tree connectors) |
| `currentPtrBase` | `uint64_t` | `u64` | abs addr of current pointer-expansion target |
| `rttiModulesCached`/`rttiModules`/`rttiCache` | bool / `QVector<Provider::ModuleEntry>` / `QHash<uint64_t,RttiInfo>` | bool / `Vec<...>` / `HashMap<u64,RttiInfo>` | per-pass RTTI memoization (out-of-scope for built-in providers — RTTI hooks no-op in tests) |
| `childMap` | `QHash<uint64_t,QVector<int>>` | `HashMap<u64,Vec<i32>>` | parent id → child node indices |
| `childMapSorted` | `mutable QSet<uint64_t>` | `RefCell<HashSet<u64>>` | which parent ids have had children lazily sorted |
| `absOffsets` | `QVector<int64_t>` | `Vec<i64>` | precomputed absolute address per node index |
| `scopeTypeW`/`scopeNameW` | `QHash<uint64_t,int>` | `HashMap<u64,i32>` | per-container column widths |

Helper methods: `effectiveTypeW/effectiveNameW(scopeId)` (fall back to global), `setTreeSibling(childDepth, hasMore)`, and `emitLine(lineText, LineMeta&&)` (the line-append routine that prepends the 3-char fold indicator, rewrites indent into Unicode tree connectors when `treeLines`, auto-detects trailing `{` column for `braceCol`, appends to `text`, records `lineStarts`, and tracks `maxLineLen`).

#### Result type `ComposeResult` (`core.h:1064-1076`)
`{ QString text; QVector<LineMeta> meta; LayoutInfo layout; int maxLineLen; QVector<int> lineStarts; }`.

#### `LineMeta` (`core.h:994-1037`)
~30 fields describing one rendered line: `nodeIdx`, `nodeId`, `subLine`, `depth`, `foldLevel`,
`foldHead`, `foldCollapsed`, `isContinuation`, `isRootHeader`, `isArrayHeader`, `lineKind`
(`LineKind` enum), `nodeKind`/`elementKind`, array nav (`arrayViewIdx`/`arrayCount`/`arrayElementIdx`),
`offsetText`/`offsetAddr`, `ptrBase`, `markerMask`, `dataChanged`/`heatLevel`/`changedByteIndices`/`lineByteCount`
(live-value highlighting), `effectiveTypeW`/`effectiveNameW`, `pointerTargetName`, `isArrayElement`/`isMemberLine`/`isStaticLine`,
`braceCol`, `parentAddr`, and `QVector<LineChip> chips`.

#### `LineChip` (`core.h:982-992`) + `ChipKind` (`core.h:971-980`)
A "tail chip" hanging off the right of a row: `{ ChipKind kind; int startCol, endCol; QString text; uint64_t rttiVtableAddr; QVector<NodeKind> typeHintKinds; int64_t enumCurrentValue; uint64_t enumRefNodeId; }`.
`ChipKind ∈ {Enum, TypeHint, Rtti, Symbol, Comment, AddComment}`. `findChip(lm,kind)` returns first matching chip (O(n), n≤4).

#### `LineKind` (`core.h:927-931`)
`enum class LineKind : uint8_t { CommandRow, Blank(unused), Header, Field, Continuation, Footer, ArrayElementSeparator }`.

#### `LayoutInfo` (`core.h:1054-1060`), `LineGeometry` (`core.h:1155-1181`), span helpers (`core.h:1183-1452`)
`LineGeometry::forLine(lm)` computes prefix width (0 for CommandRow / root footer, else `kFoldCol=3`),
indent (`depth*kTreeIndent`, `kTreeIndent=2`), and column widths. `documentColumn(contentCol)=prefixWidth+contentCol`.
The many `*SpanFor` inline functions compute clickable `[start,end)` column ranges for inline editing —
needed by the editor port, but pure string/column arithmetic.

### A.2 Public entry point

```cpp
ComposeResult compose(const NodeTree& tree, const Provider& prov, uint64_t viewRootId = 0,
                      bool compactColumns=false, bool treeLines=false, bool braceWrap=false,
                      bool typeHints=false, bool showComments=true,
                      SymbolLookupFn symbolLookup={}, bool showRtti=true, bool showEnumChips=true);
```
(`compose.cpp:1510-1745`, declared `core.h:1529-1534`.)

Algorithm:
1. Build `childMap` (parentId → child indices) in one pass (`compose.cpp:1527-1528`). Children sorted *lazily* on first access in `childIndices()` by `absOffsets`.
2. Reserve buffers (`~3 lines/node`, `~80 chars/line`).
3. **Absolute offset BFS** (`compose.cpp:1539-1586`): O(N). Seed roots (`parentId==0`) with their own `offset`; BFS from `childMap[0]`; each node `absOffsets = parent.absOffsets + node.offset`. Any node still unvisited (orphan / broken parent chain) is re-rooted as top-level with its own offset, then its descendants fixed via DFS. Finally add `tree.baseAddress` to every entry. **Edge case the BFS guards:** orphans must NOT silently land at offset 0 (that would overlay real roots).
4. **Hex-digit tier** (`compose.cpp:1589-1599`): `maxAddr ≤ 0xFFFF→4`, `≤0xFFFFFFFF→8`, `≤0xFFFFFFFFFFFF→12`, else `16`.
5. **Column widths** (`compose.cpp:1601-1688`): global `typeW=qBound(kMinTypeW=9, maxTypeLen, typeCap)`, `nameW=qBound(kMinNameW=10, maxNameLen, kMaxNameW=128)` where `typeCap = compact?kCompactTypeW(20):kMaxTypeW(128)`. Per-scope widths computed per container (struct/array) over *direct non-struct children only* (struct children excluded so a nested pointer header doesn't inflate sibling widths). Name width includes ALL nodes (stable when cycling types). Synthesized primitive-array element type names (`uint32_t[99]`) are accounted for.
6. Emit **CommandRow** as line 0 (placeholder `"[▸] source▾  0x0  struct Untitled {"`, `nodeId=kCommandRowId=UINT64_MAX`). If `braceWrap`, emit standalone `"{"` footer line (flush-left).
7. Walk roots (`childIndices(state,0)`), filtered by `viewRootId` if non-zero, via `composeNode(...,depth=0)`.

### A.3 The recursive walkers (file-local)
- `composeNode` (`compose.cpp:1234-1506`): dispatcher. Pointer w/ `refId` → merged fold header + dereference expansion (reads `*ptr`, follows `ptrDepth` extra indirections, RVA add when `isRelative`, renders materialized children OR virtually expands the ref struct with `ptrVisiting`/`virtualPtrRefs` cycle guards; falls back to `NullProvider` showing zeros for unreadable targets). Struct/Array → `composeParent`; else → `composeLeaf`.
- `composeParent` (`compose.cpp:607-1232`): struct-level cycle guard (`visiting`), array `[N] +0xRel` separators, header/footer lines, enum-member rendering (sorted by value), bitfield-member rendering, primitive-array element synthesis, struct-array expansion via refId, embedded-struct expansion, static-field rendering (offset-expression evaluation via `AddressParser`, body/footer lines, static pointer deref). Static fields render *after* regular children, before the footer.
- `composeLeaf` (`compose.cpp:305-593`): single field; emits `numLines = linesForKind(kind)` lines (Mat4x4=4). Builds chips: Enum (int field whose `refId`→enum, reads value, finds matching member), RTTI/Symbol/null-pointer CTA, TypeHint (inference strength≥3), Comment (`Node::comment` else `symbolLookup`). All chip text is sanitized so `\r\n\t` never split a chip across rows (collapsed to `·` separator).

### A.4 `NodeTree` selection normalizers (defined in compose.cpp)
- `normalizePreferAncestors(ids)` (`compose.cpp:1747-1766`): drops any id that has a *selected ancestor* (keeps only top-most selected). Used by `batchRemoveNodes`.
- `normalizePreferDescendants(ids)` (`compose.cpp:1768-1784`): drops any id that has a *selected descendant* (keeps only leaf-most selected). Used by `batchChangeKind`.

### A.5 Qt-type → Rust crate mapping (composition)
| Qt | Rust |
|---|---|
| `QString` | `String` (note: Qt is UTF-16; column indices are UTF-16 code units. For 1:1 column math the port must index by UTF-16 units, or keep all chips/spans ASCII-only — the code uses Unicode glyphs `▸ ▾ ├ └ │ ↻ → ·` which are single UTF-16 units, so a `Vec<u16>`/`encode_utf16` count, or `widestring`, matches exactly) |
| `QVector<T>` / `QSet` / `QHash` | `Vec<T>` / `HashSet` / `HashMap` |
| `QRegularExpression` | `regex` crate (only the `"  +"`→`" "` run-squash) |
| `QChar` / `QLatin1Char` | `char` |
| `std::function` | `Box<dyn Fn>` / closure |
| Scintilla fold constants (`SC_FOLDLEVELBASE=0x400`, `SC_FOLDLEVELHEADERFLAG=0x2000`) | plain `const i32` |

### A.6 Concurrency / platform
`compose()` is single-threaded and pure (no globals mutated except the RTTI hook fired through a function pointer). RTTI auto-detect (`rttiForVtable`) and `Provider::enumerateModules` are OUT OF SCOPE for built-in providers — in headless/test builds `g_rttiDiscoveryHook` is `nullptr` and the providers return empty module lists, so RTTI chips never appear. No platform `#cfg` in compose itself.

---

## B. UNDO/REDO COMMAND STACK  ← the pure, port-critical core

### B.1 Command value types (`core.h:1080-1115`, `namespace rcx::cmd`)

Helper carried by structural commands:
```cpp
struct OffsetAdj { uint64_t nodeId; int oldOffset, newOffset; };
```
A sibling-offset shift: when a node is inserted/removed/resized, later siblings move; each affected
sibling gets one `OffsetAdj` recording (id, before, after). Rust: `struct OffsetAdj { node_id: u64, old_offset: i32, new_offset: i32 }`.

The **18** command structs (each is fully self-inverse — stores old+new):

| # | Struct (fields) | Forward effect (`isUndo=false`); reverse swaps old↔new |
|---|---|---|
| 1 | `ChangeKind { nodeId; NodeKind oldKind,newKind; QVector<OffsetAdj> offAdjs }` | set `node.kind`; apply each `offAdj.newOffset` to siblings |
| 2 | `Rename { nodeId; QString oldName,newName }` | set `node.name` |
| 3 | `Collapse { nodeId; bool oldState,newState }` | set `node.collapsed` |
| 4 | `Insert { Node node; QVector<OffsetAdj> offAdjs }` | add `node` to tree; apply sibling shifts. (Undo removes it + reverts shifts.) |
| 5 | `Remove { nodeId; QVector<Node> subtree; QVector<OffsetAdj> offAdjs }` | apply shifts then remove whole subtree. (Undo re-adds subtree + reverts shifts.) |
| 6 | `ChangeBase { uint64_t oldBase,newBase; QString oldFormula,newFormula }` | set `tree.baseAddress` + `baseAddressFormula`; `resetSnapshot()` |
| 7 | `WriteBytes { uint64_t addr; QByteArray oldBytes,newBytes }` | write `bytes` through provider/snapshot — the **only** command that touches the data source, the **only** one that can *fail* |
| 8 | `ChangeArrayMeta { nodeId; NodeKind oldElementKind,newElementKind; int oldArrayLen,newArrayLen }` | set `elementKind`+`arrayLen`; clamp `viewIndex` |
| 9 | `ChangePointerRef { nodeId; uint64_t oldRefId,newRefId }` | set `refId`; if non-zero, force `collapsed=true` |
| 10 | `ChangeStructTypeName { nodeId; QString oldName,newName }` | set `structTypeName` |
| 11 | `ChangeClassKeyword { nodeId; QString oldKeyword,newKeyword }` | set `classKeyword` |
| 12 | `ChangeOffset { nodeId; int oldOffset,newOffset }` | set `node.offset`; clears value-history for node+descendants |
| 13 | `ChangeEnumMembers { nodeId; QVector<QPair<QString,int64_t>> oldMembers,newMembers }` | set `enumMembers` |
| 14 | `ChangeOffsetExpr { nodeId; QString oldExpr,newExpr }` | set `offsetExpr` (static fields) |
| 15 | `ToggleStatic { nodeId; bool oldVal,newVal }` | set `node.isStatic` |
| 16 | `ToggleRelative { nodeId; bool oldVal,newVal }` | set `node.isRelative` (RVA flag) — *declared but no apply branch wired in `applyCommand` as of this source* |
| 17 | `ToggleBigEndian { nodeId; bool oldVal,newVal }` | set `node.bigEndian` |
| 18 | `ChangeComment { nodeId; QString oldComment,newComment }` | set `node.comment` |

> **Port note on #16 `ToggleRelative`:** it is in the `Command` variant (`core.h:1108-1115`) but the
> `std::visit` switch in `applyCommand` (`controller.cpp:2890-3050`) has **no branch** for it — so an
> instance is a no-op beyond `tree.touch()`+`refresh()`. Replicate as a defined-but-inert variant
> arm for parity (or note the gap).

The variant alias (`core.h:1108-1115`):
```cpp
using Command = std::variant< ChangeKind, Rename, Collapse, Insert, Remove, ChangeBase,
   WriteBytes, ChangeArrayMeta, ChangePointerRef, ChangeStructTypeName, ChangeClassKeyword,
   ChangeOffset, ChangeEnumMembers, ChangeOffsetExpr, ToggleStatic, ToggleRelative,
   ToggleBigEndian, ChangeComment >;
```
**Rust:** `enum Command { ChangeKind(ChangeKind), Rename(Rename), ... }` — a tagged enum is the direct, idiomatic equivalent of `std::variant`. The dispatcher becomes a `match`.

### B.2 The undo-stack wrapper (`controller.h:85-95`, `controller.cpp:331-353`)
Reclass uses Qt's `QUndoStack` + `QUndoCommand`. The single concrete command:
```cpp
class RcxCommand : public QUndoCommand {
    RcxController* m_ctrl; Command m_cmd;
    void undo() override { if (!m_ctrl->applyCommand(m_cmd,true ) && !isTransientCommand(m_cmd)) setObsolete(true); }
    void redo() override { if (!m_ctrl->applyCommand(m_cmd,false) && !isTransientCommand(m_cmd)) setObsolete(true); }
};
```
**Crucial QUndoStack behavior the port must reproduce (it is not custom):**
- `undoStack.push(cmd)` **immediately calls `cmd->redo()`** — pushing a command *executes it*. (This is why every builder reads `node.kind`/`node.name` *before* pushing — see `controller.cpp:2107-2117`.)
- `push` also truncates any redo-tail (commands after the current index are deleted).
- `undo()`/`redo()` move the stack index and call the command's `undo`/`redo`.
- `beginMacro(text)` / `endMacro()` group all intervening `push`es into ONE composite command:
  the whole group undoes/redoes atomically. **Nested `beginMacro` is allowed** (Reclass nests, e.g.
  `changeNodeKind`'s shrink path begins "Change type" then internally calls `insertNode` which… does
  not itself macro, but `groupIntoUnion`/`dissolveUnion`/`deleteRootStruct` open macros that call
  `removeNode`). Inside an open macro, `push` does NOT clear the redo stack until the macro closes.
- `clear()` empties both stacks; `setClean()` marks the current state as the saved baseline (drives
  the modified/dirty indicator — `controller.cpp:196,233,313,323`).
- `setObsolete(true)` on a command (during its own undo/redo) tells QUndoStack to **delete it on the
  next stack walk** — used for failed `WriteBytes` (see B.5).

**Rust equivalent:** there is no `QUndoStack` crate that matches exactly; implement a small stack:
```
struct UndoStack { done: Vec<Entry>, undone: Vec<Entry>, macro_open: Option<Vec<Command>>, clean_index: usize }
enum Entry { Single(Command), Macro(Vec<Command>) }
```
with `push` = execute-then-record-and-clear-redo (respecting an open macro), `undo`/`redo` walking
entries (a macro undoes its commands in **reverse** order, redoes in forward order), `begin/end_macro`,
`clear`, `set_clean`/`is_clean`, and an obsolete/drop mechanism for failed writes. Crates `undo` or
`redo` exist but their command-trait shape differs; a hand-rolled stack is the most faithful and is
trivial. The key invariant: **push executes immediately**.

### B.3 The dispatcher `applyCommand` (`controller.cpp:2827-3059`)

Signature: `bool RcxController::applyCommand(const Command& command, bool isUndo)`. Returns `false`
only when the underlying op was *rejected* (today: only `WriteBytes` write failure, or `m_readOnlyOverride`
guard). On success → `true`.

Flow:
1. `tree.touch()` (bump `m_generation`) at entry — *every* command, including `WriteBytes`/`ChangeBase`,
   because value caches key off `(generation, baseAddress)` (`controller.cpp:2835`).
2. Define local helpers (lambdas):
   - `clearNodeHistory(id)` — removes `m_valueHistory[id]` and `m_lastValueAddr[id]`.
   - `clearHistoryForAdjs(adjs)` — if `adjs` non-empty: bump `m_refreshGen` (discard any in-flight async
     read whose layout is now stale), then for every adjusted node clear its history, and if it is a
     Struct/Array, clear all descendants' history too (builds a one-shot childMap only if containers are present).
3. `std::visit` over the variant — one `if constexpr (is_same_v<T, cmd::X>)` arm per command.
   Each arm looks up `tree.indexOfId(c.nodeId)`; **all arms are guarded `if (idx>=0)` so a command
   targeting a deleted node is a silent no-op** (does not crash, does not fail). The arm sets the
   field to `isUndo ? c.oldX : c.newX`.
4. After visit: `if (success && !m_suppressRefresh) refresh();` — recompose+repaint, *unless* a batch
   suppressed it or the write failed.

Per-arm specifics beyond the simple field-set (cite `controller.cpp`):
- **ChangeKind** (2892-2912): set kind; apply each `offAdj` (set sibling offset to old/new); bump
  `m_refreshGen`; **deliberately KEEPS value history** across kind changes (a prior version wiped it;
  comment explains why — keep the hover trend visible right after accepting a TypeHint). `clearHistoryForAdjs(c.offAdjs)` still clears shifted siblings.
- **Insert** (2921-2941): undo→revert adjs then `nodes.remove(indexOfId(node.id))` + `invalidateIdCache()`;
  redo→`tree.addNode(c.node)` then apply adjs. `addNode` (`core.h:431-443`) assigns id if 0, bumps `m_nextId`,
  appends, updates caches, bumps generation. `clearHistoryForAdjs`.
- **Remove** (2942-2968): undo→re-add every node in `subtree` (in stored order) then revert adjs;
  redo→apply adjs FIRST (before removal changes indices), then `subtreeIndices(nodeId)` sorted **descending**
  so `nodes.remove(idx)` doesn't invalidate not-yet-processed indices, clearing each node's history, then `invalidateIdCache()`.
- **ChangeBase** (2969-2972): set base+formula, `resetSnapshot()`.
- **WriteBytes** (2973-2996): see B.5 — the only fallible arm.
- **ChangeArrayMeta** (2997-3004): set `elementKind`+`arrayLen`; clamp `viewIndex` to `[0, arrayLen-1]`.
- **ChangePointerRef** (3005-3011): set `refId`; if `refId!=0` force `collapsed=true`.
- **ChangeOffset** (3020-3028): set offset; bump `m_refreshGen`; clear history for node and ALL descendants.
- Remaining arms (Rename, Collapse, ChangeStructTypeName, ChangeClassKeyword, ChangeEnumMembers,
  ChangeOffsetExpr, ToggleStatic, ToggleBigEndian, ChangeComment) are plain guarded field sets.

### B.4 Command builders (in `controller.cpp`) — how old/new are captured
These build a command, then `m_doc->undoStack.push(new RcxCommand(this, cmd))` (push executes it).
The builders are where invariants live. Key ones:

- `insertNode(parentId, offset, kind, name)` (`2459-2486`): if `offset<0`, auto-place after last
  sibling with alignment `n.offset = ceil(maxEnd/align)*align` (align from `alignmentFor(kind)`).
  `n.id = tree.reserveId()` (atomic id BEFORE push). Push `Insert{n}` (no offset adjs — appends at end).
- `insertNodeAbove(beforeIdx, kind, name)` (`2488-2511`): new node at `before.offset`; build `OffsetAdj`
  for every sibling with `offset >= before.offset`, shifting by `sizeForKind(kind)`. Push `Insert{n, adjs}`.
- `removeNode(nodeIdx)` (`2513-2545`): compute deleted size (`structSpan` for containers else `byteSize`);
  `deletedEnd = offset+size`. For each sibling (skip self) with `offset >= deletedEnd`, build `OffsetAdj`
  shifting `-deletedSize` (only when `parentId != 0` — root-level nodes never shift). Collect the full
  subtree (`subtreeIndices`) into `QVector<Node> subtree`. Push `Remove{nodeId, subtree, adjs}`.
- `changeNodeKind(nodeIdx, newKind)` (`2078-2186`): compute old/new `byteSize`. Converting *to*
  Struct/Array → `newSize=0` (size deferred to follow-up commands). **Shrinking path** (`newSize>0 && <oldSize`):
  open "Change type" macro; capture `origName/origOffset/needsRename` BEFORE pushing ChangeKind (push
  mutates `node.kind` in place — reading after would see the new kind); push `ChangeKind{node.id, oldKind, newKind, {}}`;
  if hex→non-hex push a `Rename` to `field_<offset:04x>`; fill the freed gap with hex pad nodes
  (`hexToHex`→same-size pads so the cycle is reversible; else largest-first Hex64/32/16/8) via `insertNode`;
  end macro. **Same/grow path**: build sibling `OffsetAdj`s for siblings `>= oldEnd` shifting by `delta=newSize-oldSize`;
  push `ChangeKind{..., adjs}`, optionally wrapped in a "Change type" macro with a `Rename`.
- `renameNode` (`2188-2193`), `toggleCollapse` (`2730-2735`), `convertRootKeyword` (`2057-2076`,
  only class↔struct, refuses enum, no-op if unchanged): single-command pushes.
- `setNodeValue(...)` (`3061-3149`): the WriteBytes builder. Guards: index valid, provider writable,
  `m_readOnlyOverride` → silent return. Resolve address (use `resolvedAddr` if given, else
  `baseAddress + computeOffset`, bail on negative). Vec/Mat components redirect to `Float` at sub-offset
  `subLine*4`. Parse via `fmt::parseAsciiValue` or `fmt::parseValue(editNode,...)` (carries `bigEndian`).
  Pad/truncate strings to full buffer. Validate range readable. **Read old bytes for undo. Test the
  write FIRST** (snapshot wins over provider) — if it fails, refresh and return WITHOUT pushing
  (no optimistic visual). On success push `WriteBytes{addr, oldBytes, newBytes}` (the redo re-writes, harmless).
- `duplicateNode` (`3151-3179`): copy primitive sibling at `offset+size`, name `+"_copy"`, shift later
  siblings down, push `Insert{n, adjs}`.

### B.5 Failure handling & the "obsolete/transient" mechanism (`controller.cpp:334-353, 2973-2996`)
Only `WriteBytes` can fail at apply time (provider rejected, page protection, process gone, or the
`m_readOnlyOverride` self-attach safety). When `applyCommand` returns `false`:
- `RcxCommand::undo/redo` calls `setObsolete(true)` **only if NOT transient**. `isTransientCommand`
  returns `true` exactly for `WriteBytes`. So a failed `WriteBytes` is **kept** (transient) — the same
  redo can succeed later on a re-attached writable provider. A failed *tree-state* command (which "can't
  recover") would be marked obsolete and dropped. (In practice only WriteBytes ever fails today, so the
  obsolete branch is defensive.)
- On WriteBytes failure: `qWarning`, emit `statusHint("Write rejected at 0x… — removing from history")`,
  set `success=false`, and **no refresh** (keep last good visual). Critically, because the snapshot/provider
  was NOT patched on failure, a later undo can't push stale `oldBytes` over never-written memory.

**Rust mapping:** model `apply_command(&mut self, cmd, is_undo) -> bool`; the stack entry tracks an
`obsolete` flag; `is_transient(cmd) == matches!(cmd, Command::WriteBytes(_))`. On `false` from a
non-transient command, mark the entry obsolete and prune it.

### B.6 Batch macros (the "batch macros" porting hint)
Patterns (all `beginMacro(label)` … push many … `endMacro()`), usually with `m_suppressRefresh=true`
around the body and one `refresh()` after:
- `batchRemoveNodes` (`5044-5066`): normalize via `normalizePreferAncestors` (drop descendants of
  selected ancestors), clear selection, macro "Delete N nodes", `removeNode` each.
- `batchChangeKind` (`5068-5092`): normalize via `normalizePreferDescendants`, save/restore selection,
  macro "Change type of N nodes", `changeNodeKind` each.
- `deleteRootStruct` (`2547-2587`): macro "Delete root struct"; first push `ChangePointerRef{n.id, refId, 0}`
  to null every pointer referencing the struct, then `removeNode`. Switches view to next root after.
- `groupIntoUnion`/`dissolveUnion` (`2589`, `2696`): macros wrapping multiple Insert/ChangeOffset etc.
- Paste (`641-671`): macro "Paste nodes", multiple `Insert`.
- "Collapse all"/"Expand all" (`1101-1107`, `1155-1161`): macro of many `Collapse` commands.
- "Reorder field" (`1090-1095`): macro of two `ChangeOffset`.
- **Cycle-type time-grouped macro** (`990-1006`): rapid ←/→ type-cycle keypresses are folded into ONE
  undo macro using an 800ms single-shot `QTimer` — `m_cycleMacroOpen` opens "Cycle type" on first
  press, the timer is restarted on each press, and `timeout` closes the macro. (Multi-select cycle
  opens its own inner macro "Cycle type for N nodes".) **Rust note:** this is the one *timing-dependent*
  bit; behind a UI-driven timer. For the headless core port it can be modeled as an explicit
  open/close-on-idle without a real timer.

### B.7 `m_suppressRefresh` / `m_refreshGen` / `m_generation`
- `m_suppressRefresh` (controller bool): when true, `applyCommand` skips its trailing `refresh()`.
  Batch builders set it so only one recompose happens at the end. Port: a `suppress_refresh: bool` on the controller.
- `m_refreshGen` (controller counter): bumped on layout-changing commands to invalidate in-flight async
  reads (so stale snapshot data doesn't record false heat). Pure bookkeeping; relevant only with the
  async live-refresh loop.
- `NodeTree::m_generation` (`core.h:427-452`): structural-change counter; `touch()` bumps it; cache keys read it.

---

## C. The document model the commands mutate: `NodeTree` + `Node` (`core.h`)

Although the brief frames these as "data structures the commands touch," they are central and worth
mapping (likely shared with the model/serialization subsystem report — keep consistent).

### C.1 `NodeKind` (`core.h:24-34`) + `KindMeta` table (`core.h:54-97`)
31 kinds (Hex8..Hex128, Int/UInt 8..128, Float16/Float/Double/Bool, Pointer32/64, FuncPtr32/64,
Vec2/3/4, Mat4x4, UTF8/UTF16, Struct, Array). The `kKindMeta[]` table is the single source of truth:
`{kind, name(JSON/UI e.g. "Hex64"), typeName(display e.g. "uint16_t"), size, lines, align, flags}`.
`static_assert` ties table length to the enum. Helpers: `sizeForKind`, `linesForKind`, `alignmentFor`,
`kindToString`/`kindFromString` (JSON name; unknown→`Hex8`), `kindFromTypeName`, `flagsFor`, and the
`isHexNode/isPointerKind/isContainerKind/isStringKind/isVectorKind/isMatrixKind/isFuncPtr/isValidPrimitivePtrTarget`
predicates. **Rust:** `#[repr(u8)] enum NodeKind` + a `const [KindMeta; 31]` table indexed by `kind as usize`.

### C.2 `Node` (`core.h:210-363`)
Fields: `id:u64`, `kind`, `name`, `structTypeName`, `classKeyword` (`""`→"struct"; values "struct"/"class"/"union"/"enum"/"bitfield"), `parentId:u64` (0=root), `offset:i32`, `isStatic`, `offsetExpr`, `isRelative`, `arrayLen:i32` (clamped 1..`kMaxArrayLen=1_000_000`), `strLen:i32`, `collapsed`, `refId:u64`, `elementKind`, `ptrDepth:i32` (0..2), `viewIndex:i32`, `enumMembers: Vec<(String,i64)>`, `bitfieldMembers: Vec<BitfieldMember{name,bitOffset:u8,bitWidth:u8}>`, `comment`, `bigEndian`.
- `byteSize()` (`238-255`): leaf size. UTF8=`strLen`; UTF16=`min(strLen,INT_MAX/2)*2`; Array=`min(arrayLen,INT_MAX/elemSz)*elemSz` (0 if elem size 0); Struct=0 *unless* bitfield (then `sizeForKind(elementKind)` or 4); else `sizeForKind(kind)`.
- `totalByteSize(tree)` (`841-845`): containers→`tree.structSpan(id)`, else `byteSize()`.
- `toJson/fromJson` (`263-354`): JSON serialization. ids serialized as decimal **strings** (u64 precision); collapsed forced `true` on load; `arrayLen`/`strLen`/`ptrDepth` re-clamped; `isStatic` falls back to legacy `isHelper`. **Rust:** `serde_json` with custom string-encoded u64s for `id`/`parentId`/`refId`/enum values, matching the exact key set/omission rules (fields omitted when default).
- `resolvedClassKeyword`/`isUnion`/`isBitfield`/`isEnum` (`356-362`).

### C.3 `NodeTree` (`core.h:408-835`)
`nodes: Vec<Node>`, `baseAddress=0x00400000`, `baseAddressFormula`, `pointerSize=8`, `initialClass`,
`bookmarks: Vec<Bookmark>`, `m_nextId=1`, mutable caches (`m_idCache`, `m_childCache`), `m_generation=1`.
Methods used by the command stack:
- `addNode`/`reserveId`/`invalidateIdCache`/`touch`/`bumpGeneration`.
- `indexOfId(id)` (lazy-built id→index cache, returns -1 on miss → drives the "guarded no-op" behavior).
- `childrenOf(parentId)`, `subtreeIndices(nodeId)` (BFS, cycle-safe), `depthOf`, `computeOffset`
  (parent-chain sum, can be negative), `absoluteAddress` (safe wrapper), `structSpan` (recursive
  footprint, cycle-guarded, skips static fields, honors embedded-ref structs).
- `validate(repair)` (`468-516`): 3 passes — dedupe ids, re-root orphans, break cycles by re-rooting.
  Returns `ValidateReport{orphans,cycles,duplicates}`.
- `findOverlaps()` (`535-592`): sibling overlap detection (skips root-level, unions, static, zero-size);
  returns `Vec<OverlapPair{aId,bId,parentId}>`. Not auto-repaired.
- `fieldPath`/`nodeIdForPath` (dot-path ↔ id), `normalizePrefer{Ancestors,Descendants}` (in compose.cpp).
- `toJson/fromJson` (`793-833`): baseAddress as hex string; nodes array; bookmarks.

### C.4 Other model types
- `Bookmark` (`core.h:367-382`): `{name, addressFormula}` + JSON.
- `ValueHistory` (`core.h:867-923`): fixed 10-slot ring of value strings + timestamps; `record` (dedupe
  consecutive equal), `heatLevel` (0 static / 1 cold / 2 warm / 3 hot). Used for live-value heatmap; the
  command stack only *clears* entries (`m_valueHistory` lives on the controller, keyed by node id).
- `ViewState` (`core.h:1456-1468`): scroll/cursor anchored by `cursorNodeId` so refresh after a
  structural edit lands the caret on the same node.

---

## D. Subtle behaviors a faithful port must preserve (test-derived & code-derived)

1. **Push executes immediately** (QUndoStack contract). Tests rely on it: `insertNode` then read the
   tree *synchronously* (`test_controller.cpp:270-282`). Builders capture old state *before* push
   (`controller.cpp:2107-2117`, the hex64→int32 empty-name bug fix).
2. **Self-inverse commands**: undo/redo are the *same* `applyCommand` call with the `isUndo` flag flipped;
   no separate inverse logic. Old+new both stored. Round-trip undo→redo restores exact bytes/names/kinds
   (`test_controller.cpp:144-183, 220-244, 245-264, 290-301`).
3. **Guarded no-op on missing node**: every arm checks `indexOfId>=0`; a command for a since-deleted node
   silently does nothing and still returns `true`. (Matters inside macros where earlier commands shift/remove.)
4. **Remove ordering**: redo applies offset adjs *before* removal; removes subtree indices in
   *descending* order; undo re-adds subtree *then* reverts adjs. Insert is the mirror.
5. **OffsetAdj only for siblings strictly at/after the affected end**, and **never for root-level
   (`parentId==0`) nodes** on removal (`controller.cpp:2526`). Insert-above shifts siblings `>= before.offset`;
   remove shifts siblings `>= deletedEnd` by `-deletedSize`; resize shifts `>= oldEnd` by `delta`.
6. **WriteBytes is test-the-write-then-push**: never push a command that would fail (`setNodeValue`
   3137-3144). On apply failure: no refresh, no snapshot patch, command kept (transient) so it can
   redo later; emits a statusHint. Undo restores `oldBytes` (`test_controller.cpp:168-183`).
7. **ChangeKind keeps value history; ChangeOffset/Insert/Remove clear it** for affected nodes+descendants
   and bump `m_refreshGen` (false-heat avoidance).
8. **changeNodeKind shrink path is reversible** specifically because hex→hex shrink pads with *same-size*
   hex nodes (so a later join consumes exact pairs); non-hex shrink pads largest-first.
9. **ChangePointerRef forces `collapsed=true`** whenever a non-zero refId is set (both forward and undo
   if the resulting refId is non-zero).
10. **ChangeArrayMeta clamps `viewIndex`** to the new `arrayLen`.
11. **Macros are atomic**: undo/redo of a macro processes all sub-commands. Time-grouped type-cycle
    (800ms) is one macro. `m_suppressRefresh` collapses a batch to a single recompose.
12. **`tree.touch()` fires for every command** (even WriteBytes/ChangeBase) so generation-keyed caches
    invalidate. `ChangeBase` also calls `resetSnapshot()`.
13. **ToggleRelative has no apply arm** (variant member without a dispatcher branch) — preserve as inert
    for parity, or document the deviation.

---

## E. Portability assessment

- **Composition (`compose.cpp`)**: pure logic over the tree + an abstract `Provider` (built-in providers
  only, per scope). The *only* non-pure touchpoints are the OUT-OF-SCOPE RTTI hooks/`enumerateModules`
  (no-ops in headless builds) and Scintilla fold constants (plain ints). UTF-16 column indexing is the
  single fidelity gotcha (Unicode tree/fold glyphs are all single UTF-16 units, so a UTF-16 count or
  `widestring` keeps span math byte-exact).
- **Undo/redo stack**: 100% pure logic except for (a) the `QUndoStack` semantics, which must be
  re-implemented as a small hand-rolled stack (push-executes, macro grouping, clean index, obsolete
  pruning), and (b) the `WriteBytes` arm which calls the provider — abstract behind the `Provider`
  trait. The 800ms type-cycle macro timer is UI-glue; the headless core can model it as explicit
  open/close-on-idle. No `#[cfg]` platform code in this subsystem.

**Recommended crates:** `serde`/`serde_json` (JSON with string-encoded u64 ids), `regex` (chip
whitespace squash), `widestring` *or* manual `encode_utf16().count()` (column math), optional `bitflags`
(KindFlags). A hand-rolled undo stack (the `undo`/`redo` crates don't match the push-executes +
self-inverse-via-flag shape closely enough for a 1:1 port).
