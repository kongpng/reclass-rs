# Subsystem: C/C++ (multi-language) Source Generator

**Key:** `generator`
**C++ files:** `src/generator.h` (90 lines), `src/generator.cpp` (1748 lines)
**Tests:** `tests/test_generator.cpp` (1760 lines, ~70 test methods)
**Portability:** pure (no I/O, no threading, no platform `#ifdef`, no GUI). Only Qt string/container types and the core data model are used. 1:1 port is straightforward.

---

## 1. Purpose

`generator` turns a `rcx::NodeTree` (the in-memory struct-layout model) into **source code text** in five target languages, so a Reclass user can export the struct they reverse-engineered as compilable type declarations. The five backends are:

| `CodeFormat` | Backend | Output |
|---|---|---|
| `CppHeader` | C/C++ | Vergilius-style `struct`/`union`/`enum` with `uint8_t _padXXXX[]` padding and offset comments. |
| `RustStruct` | Rust | `#[repr(C)]` structs/unions/enums, `*mut T` pointers, `[u8; N]` padding. |
| `DefineOffsets` | C `#define` | `#define ClassName_FieldName 0xNN` offset macros (no field types). |
| `CSharpStruct` | C# | `[StructLayout(LayoutKind.Explicit)]` with per-field `[FieldOffset(N)]`. |
| `PythonCtypes` | Python | `ctypes.Structure`/`ctypes.Union` subclasses with `_fields_` lists. |

Each backend has three **scopes** (`CodeScope`):
- **Current** — only the one selected root struct (`render*`).
- **WithChildren / "Current + Deps"** — the struct plus every struct reachable through named-struct children, pointer targets, and array element types, in dependency order (`render*Tree`).
- **FullSdk** — all root-level (`parentId == 0`) structs, sorted by offset (`render*All`).

There is also a `renderNull` placeholder that always returns an empty string (used as the "no export" sentinel).

The generator is **read-only** over the tree — it never mutates `NodeTree` or `Node`.

---

## 2. Dependencies on `core.h` (the data model)

The generator does not own its data types; it reads `rcx::NodeTree`, `rcx::Node`, `rcx::NodeKind`. The Rust port must already have these. Key pieces the generator relies on (all from `src/core.h`):

### `NodeKind` enum (`core.h:24-34`)
`uint8_t`-backed, declared in this exact order (the order matters for `anon_%1` naming and is used by `isHexNode` range checks):
```
Hex8, Hex16, Hex32, Hex64, Hex128,
Int8, Int16, Int32, Int64, Int128,
UInt8, UInt16, UInt32, UInt64, UInt128,
Float16, Float, Double, Bool,
Pointer32, Pointer64,
FuncPtr32, FuncPtr64,
Vec2, Vec3, Vec4, Mat4x4,
UTF8, UTF16,
Struct, Array
```

### `Node` struct (`core.h:210-363`) — fields the generator reads
- `uint64_t id` — node identity.
- `NodeKind kind` — node type.
- `QString name` — field/identifier name (may be empty → synthesized).
- `QString structTypeName` — for Struct/Array: the C type name (e.g. `"IMAGE_DOS_HEADER"`). **Empty ⇒ anonymous inline struct/union.**
- `QString classKeyword` — `""`/`"struct"`/`"class"`/`"union"`/`"enum"`/`"bitfield"`. Empty resolves to `"struct"`.
- `uint64_t parentId` — `0` = root.
- `int offset` — byte offset within parent.
- `bool isStatic` — static field: excluded from layout, emitted only as a comment.
- `QString offsetExpr` — for static fields, the address expression (e.g. `"base + e_lfanew"`).
- `int arrayLen = 1` — Array element count.
- `int strLen = 64` — UTF8/UTF16 character count.
- `uint64_t refId` — Pointer32/64: id of the Struct node the pointer targets (`0` = untyped).
- `NodeKind elementKind = UInt8` — Array element kind; also the bitfield container kind.
- `QVector<QPair<QString,int64_t>> enumMembers` — name→value pairs (Enum).
- `QVector<BitfieldMember> bitfieldMembers` — `{QString name; uint8_t bitOffset; uint8_t bitWidth;}` (`core.h:202-206`).

Fields the generator does **not** read: `isRelative`, `collapsed`, `ptrDepth`, `viewIndex`, `comment`, `bigEndian`.

### `Node` methods used
- `int byteSize() const` (`core.h:238-255`) — leaf-only size:
  - `UTF8` → `strLen`; `UTF16` → `min(strLen, INT_MAX/2) * 2`.
  - `Array` → `min(arrayLen, INT_MAX/elemSz) * sizeForKind(elementKind)` (0 if elemSz ≤ 0).
  - `Struct` → if `classKeyword == "bitfield"`: `sizeForKind(elementKind)` (or 4 if ≤0); else `0` (real size needs tree walk).
  - default → `sizeForKind(kind)` from `kKindMeta`.
- `QString resolvedClassKeyword() const` (`core.h:357-359`) — `classKeyword.isEmpty() ? "struct" : classKeyword`.
- `bool isBitfield() const` (`core.h:361`) — `classKeyword == "bitfield"`.
- (`isUnion()`/`isEnum()` exist but generator computes the keyword inline instead.)

### `NodeTree` (`core.h:408+`) — members/methods used
- `QVector<Node> nodes;` — flat node list; index order = insertion order (`addNode` appends, `core.h:431-443`).
- `int pointerSize = 8;` — 4 for 32-bit target, 8 for 64-bit. Used to decide native-pointer rendering.
- `int indexOfId(uint64_t id) const` (`core.h:594-600`) — id→index via a lazily-built cache; returns `-1` if missing.
- `int structSpan(uint64_t id, const QHash<uint64_t,QVector<int>>* childMap=nullptr, QSet<uint64_t>* visited=nullptr) const` (`core.h:752-787`) — **container-aware byte size**, the core size algorithm (see §6). Cycle-guarded (returns 0 on revisit). Skips static children. The generator always passes its own pre-built `childMap`.
- `QVector<int> childrenOf(...)` exists but the generator builds its own `childMap` instead.
- `int addNode(const Node&)` — only used by tests to construct trees; assigns `id = m_nextId++` when `id==0` (`core.h:431-443`). IDs are therefore small sequential integers in test data.

### `core.h` free helpers used
- `isHexNode(NodeKind k)` (`core.h:138-140`) — `k >= Hex8 && k <= Hex128`. Drives padding-run collapsing.
- `isPointerKind(NodeKind k)` (`core.h:153-155`) — `Pointer32 || Pointer64`.
- `isContainerKind` indirectly (via structSpan).
- `sizeForKind(NodeKind)` (`core.h:108`) — from the `kKindMeta` table (`core.h:64-97`). Relevant sizes: Hex8/Int8/UInt8/Bool/UTF8 = 1; Hex16/Int16/UInt16/Float16/UTF16 = 2; Hex32/Int32/UInt32/Float/Pointer32/FuncPtr32 = 4; Hex64/Int64/UInt64/Double/Pointer64/FuncPtr64 = 8; Hex128/Int128/UInt128 = 16; Vec2 = 8; Vec3 = 12; Vec4 = 16; Mat4x4 = 64; Struct/Array = 0.

---

## 3. Public API (`generator.h`)

All functions are in namespace `rcx`, all return `QString` (UTF-16 text). All take a `const NodeTree&` by reference and never mutate it. The `typeAliases` parameter is an **optional** `const QHash<NodeKind,QString>*` (default `nullptr`) overriding primitive type names per-kind; `emitAsserts` (default `false`) toggles size-assert emission.

### Enums
```cpp
enum class CodeFormat : int { CppHeader=0, RustStruct, DefineOffsets, CSharpStruct, PythonCtypes, _Count };
enum class CodeScope  : int { Current=0,  WithChildren, FullSdk, _Count };
```

### Name/filter helpers
- `const char* codeFormatName(CodeFormat)` (`generator.cpp:1408-1417`): CppHeader→`"C/C++"`, RustStruct→`"Rust"`, DefineOffsets→`"#define"`, CSharpStruct→`"C#"`, PythonCtypes→`"Python"`, default→`"C/C++"`.
- `const char* codeFormatFileFilter(CodeFormat)` (`generator.cpp:1419-1428`): Qt file-dialog filter strings. CppHeader→`"C++ Header (*.h);;All Files (*)"`, RustStruct→`"Rust Source (*.rs);;All Files (*)"`, DefineOffsets→`"C Header (*.h);;All Files (*)"`, CSharpStruct→`"C# Source (*.cs);;All Files (*)"`, PythonCtypes→`"Python Source (*.py);;All Files (*)"`, default→`"All Files (*)"`.
- `const char* codeScopeName(CodeScope)` (`generator.cpp:1430-1437`): Current→`"Current"`, WithChildren→`"Current + Deps"`, FullSdk→`"Full SDK"`, default→`"Current"`.

### Dispatchers (`generator.cpp:1710-1741`)
- `renderCode(fmt, tree, rootStructId, aliases=nullptr, emitAsserts=false)` → switches on `fmt` to `renderRust/renderDefines/renderCSharp/renderPython/renderCpp` (default = Cpp). Note: `DefineOffsets` and `PythonCtypes` **ignore** `aliases`/`emitAsserts`.
- `renderCodeTree(...)` → `render*Tree` variants.
- `renderCodeAll(fmt, tree, aliases, emitAsserts)` → `render*All` variants.

### Per-backend public entry points
For each of Cpp / Rust / CSharp there are three: `renderX`, `renderXTree`, `renderXAll`, all with `(tree, rootStructId[, aliases, emitAsserts])`. Defines and Python omit `aliases`/`emitAsserts`:
- C/C++: `renderCpp`, `renderCppTree`, `renderCppAll` (`generator.cpp:1439-1498`).
- Rust: `renderRust`, `renderRustTree`, `renderRustAll` (`generator.cpp:1502-1553`).
- #define: `renderDefines`, `renderDefinesTree`, `renderDefinesAll` (`generator.cpp:1557-1602`).
- C#: `renderCSharp`, `renderCSharpTree`, `renderCSharpAll` (`generator.cpp:1606-1657`).
- Python: `renderPython`, `renderPythonTree`, `renderPythonAll` (`generator.cpp:1661-1706`).
- `renderNull(tree, rootStructId)` (`generator.cpp:1743-1745`) → always `{}` (empty QString).

**Common entry-point contract** (every non-`All` function):
1. `idx = tree.indexOfId(rootStructId)`; if `< 0` return empty `QString{}`.
2. If `tree.nodes[idx].kind != NodeKind::Struct` return empty. (Cpp's `renderCpp` checks `!= Struct`; `renderCppTree`/others same — Array root is not accepted as a top-level emit target.)
3. Build `GenContext`, call `ctx.prepare()` then `ctx.assignUniqueNames()`.
4. Append the per-format header preamble (see §5).
5. Emit struct(s).
6. Return `alignComments(ctx.output)` — **except** the `#define` backend, which returns `ctx.output` raw (no alignment, no comment markers used).

`All` variants skip the root validity checks (they iterate roots), still build context + assign names + preamble, then iterate `ctx.childMap.value(0)` (root nodes), **sorted ascending by `offset`**, emitting each `Struct`-kind root.

---

## 4. Internal types

### `GenContext` (`generator.cpp:65-155`)
The shared mutable state threaded through every emit function (passed by `&`). Constructed via **aggregate initialization** in each public function as `GenContext{tree, buildChildMap(tree), {}, {}, {}, {}, {}, 0, typeAliases, emitAsserts}`. Field order (critical for the aggregate init):
1. `const NodeTree& tree` — the source tree (reference).
2. `QHash<uint64_t, QVector<int>> childMap` — parentId → child node indices (built by `buildChildMap`).
3. `QSet<QString> emittedTypeNames` — struct type names already emitted (dedup guard).
4. `QSet<uint64_t> emittedIds` — struct node ids already emitted.
5. `QSet<uint64_t> visiting` — cycle guard (ids currently on the emit stack).
6. `QSet<uint64_t> forwardDeclared` — ids already forward-declared (C++ only).
7. `QString output` — the accumulating result buffer.
8. `int padCounter = 0` — counter for unique `_padNNNN` names.
9. `const QHash<NodeKind,QString>* typeAliases = nullptr`.
10. `bool emitAsserts = false`.
- `QHash<uint64_t,QString> nameById` — kept **last** (after the 10 aggregate positions) so it defaults to empty; populated by `assignUniqueNames()`. Maps struct id → disambiguated unique C identifier.

Methods:
- `void prepare()` — `output.reserve(tree.nodes.size() * 80)`. (Pure optimization; port may no-op.)
- `std::pair<QVector<int>,QVector<int>> prepareChildren(uint64_t structId) const` (`generator.cpp:87-97`) — splits children into `{regular, static}`. **Regular children are sorted ascending by `offset`** (stable `std::sort` over indices); static children keep `childMap` order. This is the canonical child ordering used by C/Rust/C#/Python bodies. (Note: `std::sort` is not stable — equal-offset siblings, e.g. union members, may reorder; tests don't assert ordering for equal offsets.)
- `QString uniquePadName()` (`generator.cpp:99-101`) — returns `"_pad" + padCounter` formatted as **4-digit lowercase hex, zero-padded** (`QStringLiteral("_pad%1").arg(padCounter++, 4, 16, QChar('0'))`), e.g. `_pad0000`, `_pad0001`. The counter is per-`GenContext`, shared across all structs in one render.
- `QString cType(NodeKind) const` (`generator.cpp:104-111`) — alias lookup first (non-empty alias wins), else `cTypeName(kind)`.
- `QString structName(const Node&) const` (`generator.cpp:114-118`) — canonical type name: `sanitizeIdent(structTypeName)` if non-empty, else `sanitizeIdent(name)` if non-empty, else `"anon_" + hex(id)` (`QStringLiteral("anon_%1").arg(n.id, 0, 16)` — lowercase hex, no padding).
- `QString nameFor(const Node&) const` (`generator.cpp:123-127`) — looks up `nameById[id]`; falls back to `structName(n)` if absent (anonymous inline structs aren't in the map).
- `void assignUniqueNames()` (`generator.cpp:135-154`) — the disambiguation pre-pass (see §7).

### `buildChildMap(const NodeTree&)` (`generator.cpp:461-466`)
Returns `QHash<uint64_t,QVector<int>>` mapping each node's `parentId` to the list of child **indices** into `tree.nodes`, in insertion order (`for i in 0..size: map[nodes[i].parentId].append(i)`). Root nodes are under key `0`.

### `collectReachableStructs(tree, childMap, rootId)` (`generator.cpp:1368-1402`)
Used by every `*Tree` scope. DFS from `rootId`, **post-order** (children appended before parent), returning `QVector<uint64_t>` of struct ids in **dependency order (leaves first, root last)**. `visited` set prevents infinite loops. Walk rules per node (only descends into `Struct` nodes):
- For each child: if child is `Struct` **with non-empty `structTypeName`** → recurse `child.id`.
- If child is `Pointer32`/`Pointer64` with `refId != 0` → recurse `refId`.
- If child is `Array` → for each of the array's children that is a `Struct` → recurse that struct's id.

Note: anonymous inline structs (empty `structTypeName`) are **not** collected as separate types (they're emitted inline by the parent).

---

## 5. Per-format header preambles

Appended to `output` before any struct emission:
- **C/C++** (`renderCpp*`): `"#pragma once\n#include <cstdint>\n\n"`.
- **#define** (`renderDefines*`): `"#pragma once\n#include <cstdint>\n\n"`.
- **Rust** (`renderRust*`): `"// Generated by Reclass 2027\n\n"`.
- **C#** (`renderCSharp*`): `"using System.Runtime.InteropServices;\n#nullable disable\n\n"`.
- **Python** (`renderPython*`): `"import ctypes\n\n"`.

---

## 6. Core size algorithm: `NodeTree::structSpan` (`core.h:752-787`)

Not in generator.cpp but the generator depends on it heavily; the Rust port must replicate exactly.

```
structSpan(id, childMap, visited):
  if visited contains id: return 0          # cycle → 0
  visited.insert(id)
  idx = indexOfId(id); if idx<0: return 0
  node = nodes[idx]
  declaredSize = node.byteSize()
  if not isContainerKind(node.kind) and node.refId==0: return declaredSize   # leaf short-circuit
  maxEnd = 0
  for ci in childMap[id]:
     c = nodes[ci]
     if c.isStatic: continue                 # static excluded
     sz = (c.kind==Struct or c.kind==Array) ? structSpan(c.id,...) : c.byteSize()
     end = c.offset + sz
     if end>maxEnd: maxEnd = min(end, INT_MAX)
  if children empty and node.kind==Struct and node.refId!=0:
     maxEnd = max(maxEnd, structSpan(node.refId,...))   # embedded-by-refId struct
  return max(declaredSize, maxEnd)
```
Consequences exploited by tests:
- Empty struct → size `0` (`testEmptyStruct`: `sizeof(Empty) == 0x0`).
- Static fields never widen the struct (`testStructSizeUnchangedByStaticField`: `sizeof(Small)==0x4` despite a static struct child).
- Tail padding is `structSize - cursor` (`testTailPadding`: field at 0, next at 16 of 1 byte → pad `[0xF]`, size `0x11`).

---

## 7. Unique-name disambiguation: `assignUniqueNames()` (`generator.cpp:135-154`)

Runs once per render before emission. Builds `nameById`. Algorithm:
- A local lambda `assign(id, base)`: starts `name = base`; while `used` contains `name`, set `name = base + "_v" + suffix` with `suffix` starting at 2 and incrementing (so collisions become `base_v2`, `base_v3`, …). Insert into `used`, set `nameById[id] = name`.
- **Pass 1:** every **root** struct (`parentId==0 && kind==Struct`), in `tree.nodes` insertion order → `assign(id, structName(n))`. Roots get priority/stable naming.
- **Pass 2:** every **nested** struct (`parentId!=0 && kind==Struct`) **with non-empty `structTypeName`** → `assign(id, structName(n))`.
- Anonymous inline structs (empty `structTypeName`, nested) are intentionally **not** assigned — they're emitted inline by keyword, referenced by id-fallback in `nameFor`.

Tested by `testDuplicateTypeNameDisambiguation`: two root structs both named `"Shared"` → output contains both `struct Shared\n{` and `struct Shared_v2\n{`, each with its own field type. This is the reason `nameFor()` exists instead of using `structTypeName` directly at reference sites.

---

## 8. C/C++ backend (the canonical/primary one)

### `cTypeName(NodeKind)` (`generator.cpp:28-61`) — primitive→C type
Hex8/UInt8 → `uint8_t`; Hex16/UInt16 → `uint16_t`; Hex32/UInt32 → `uint32_t`; Hex64/UInt64 → `uint64_t`; **Hex128 → `uint8_t`** (see note); Int8 → `int8_t` … Int64 → `int64_t`; **Int128 → `__int128`**, **UInt128 → `unsigned __int128`**; Float16 → `_Float16`; Float → `float`; Double → `double`; Bool → `bool`; Pointer32/FuncPtr32 → `uint32_t`; Pointer64/FuncPtr64 → `uint64_t`; Vec2/3/4/Mat4x4 → `float`; UTF8 → `char`; UTF16 → `wchar_t`; default → `uint8_t`.
> Hex128 maps to `uint8_t` here, but as a struct field it's handled as a hex node and collapsed into a `uint8_t[0x10]` padding array (it never reaches `emitField` because `isHexNode(Hex128)` is true — see §8.3).

### 8.1 `emitField` (`generator.cpp:176-221`) — single non-container, non-hex field
Builds indent, a sanitized name (`field_%1` with `offset` as 2-digit zero-padded lowercase hex when name empty), and an offset comment marker (`\x01// 0xNN`). Per kind:
- Vec2/Vec3/Vec4 → `float NAME[2|3|4];`
- Mat4x4 → `float NAME[4][4];`
- UTF8 → `char NAME[strLen];`; UTF16 → `wchar_t NAME[strLen];`
- Pointer32/Pointer64:
  - if `refId != 0` and target index valid → `struct TARGET* NAME;` (target = `ctx.nameFor(targetNode)`).
  - else if **native pointer** (`Pointer32 && pointerSize<=4` or `Pointer64 && pointerSize>=8`) → `void* NAME;`.
  - else (cross-size) → `cType(kind) NAME;` (i.e. `uint32_t`/`uint64_t`).
- FuncPtr32/FuncPtr64 → `void (*NAME)();`
- default → `cType(kind) NAME;`

### 8.2 `offsetComment` / comment alignment
- `offsetComment(int offset, bool isSizeof=false)` (`generator.cpp:166-170`): returns `\x01// 0xHEX` where HEX is `QString::number(offset,16).toUpper()` (uppercase, no `0x` zero-padding). If `isSizeof`, the text is `\x01// sizeof 0xHEX`. `\x01` (= `kCommentMarker`, `QChar(0x01)`) is a sentinel separating code from comment.
- `indent(int depth)` (`generator.cpp:172-174`): `depth*4` spaces.
- `alignComments(const QString& raw)` (`generator.cpp:472-501`): splits on `'\n'`. **Pass 1** finds `maxCode` = max index of `\x01` over all lines. **Pass 2** for each line: text before marker + `(maxCode - markerPos + 1)` spaces (min 1) + text after marker; lines without a marker pass through unchanged. Re-joins with `'\n'`. This right-aligns all `// 0x…` comments to one column. Used by every backend except `#define`. `testAlignCommentsNoMarkers` confirms marker-free input is handled.

### 8.3 `emitStructBody` (`generator.cpp:225-377`) — the layout engine
Parameters `(ctx, structId, isUnion, depth, baseOffset)`. `baseOffset` is the absolute offset of this struct within the top-level type (so nested/inline structs report absolute offsets in comments). Steps:
1. `idx = indexOfId(structId)`; bail if `<0`.
2. `structSize = structSpan(structId, &childMap)`.
3. `[children, staticIdxs] = prepareChildren(structId)` (regular sorted by offset).
4. Local `emitPadRun(relOffset, size)`: if `size>0`, emit `uint8_t _padNNNN[0xSIZE];` + offset comment `baseOffset+relOffset`. SIZE is uppercase hex.
5. Iterate `children` with `cursor=0, i=0`:
   - Compute `childSize`: `structSpan(child.id)` for Struct/Array, else `child.byteSize()`.
   - **Gap/overlap** (skip if `isUnion`): if `child.offset > cursor` → `emitPadRun(cursor, child.offset-cursor)`. If `child.offset < cursor` → emit `// WARNING: overlap at offset 0xA (previous field ends at 0xB)` comment line (absolute offsets). (`testOverlapWarning`; `testUnionNoOverlapWarning` asserts unions produce neither WARNING nor `_pad`.)
   - **Hex-run collapse** (`isHexNode(child.kind)`, includes Hex128): start a run at `child.offset`..`child.offset+childSize`; greedily extend over following children while they are also hex nodes AND `next.offset >= runEnd` (i.e. contiguous/non-overlapping; an overlapping next breaks the run). Emit one `emitPadRun(runStart, runEnd-runStart)`, advance `cursor=runEnd`, `i=j`, `continue`. This is why Hex128 → `uint8_t _padNNNN[0x10]` (`testHex128CppOutput`, `testCppHex128InUnion`).
   - **Struct child:**
     - **Bitfield container** (`child.isBitfield() && !bitfieldMembers.isEmpty()`): emit `struct\n{\n`, then for each member `bfType name : bitWidth;` + offset comment (all members share `baseOffset+child.offset`); close `} [name];` + offset comment. `bfType = cType(child.elementKind)` (fallback `uint32_t` if empty). Field name prefixed by a space only if non-empty.
     - **Anonymous** (`structTypeName.isEmpty()`): emit `KW\n{\n` (KW = `resolvedClassKeyword()`), recurse `emitStructBody(child.id, childIsUnion = (KW=="union"), depth+1, baseOffset+child.offset)`, then `} [name];` + comment. (`testInlineAnonymousStruct`: produces `union\n    {`, contains `struct _LIST_ENTRY ListEntry;`, no `anon_`.)
     - **Named:** KW = `resolvedClassKeyword()`; if KW=="enum" and `enumMembers` empty → KW="struct". Emit `KW TYPENAME FIELDNAME;` + comment, where `TYPENAME = nameFor(child)`. (`testNestedStruct`/`testDeeplyNested`/`testOpaqueTypeNoStub`: opaque named child becomes `struct _LIST_ENTRY entry;` with **no** generated body and **no** padding.)
   - **Array child** (`generator.cpp:334-356`): collect array's children; if any is a `Struct`, `hasStructChild=true`, `elemTypeName = nameFor(thatStruct)`. If struct-element array → `struct ELEM NAME[arrayLen];`; else → `cType(elementKind) NAME[arrayLen];`. (`testPrimitiveArray`: `uint32_t data[16];`.)
   - **Else (primitive):** `emitField(...)`.
   - After emit: `childEnd = child.offset + childSize`; `cursor = max(cursor, childEnd)`; `i++`.
6. **Tail padding** (skip union): if `cursor < structSize` → `emitPadRun(cursor, structSize - cursor)`.
7. **Static fields**: for each `staticIdx`, emit a comment line `// static: TYPE NAME @ offsetExpr`, where TYPE = `structTypeName` if set else `cType(kind)`. (`testStaticFieldNotInStructBody`, `testStaticFieldCommentFormat`, `testCppStaticFieldComment`.)

### 8.4 `emitStruct` (`generator.cpp:381-457`) — full top-level definition
1. If `emittedIds` contains id → return (dedup). If `visiting` contains id → return (cycle).
2. Insert into `visiting`. `idx=indexOfId`; if `<0` remove from visiting, return.
3. If node kind not Struct nor Array → unvisit, return. If kind == Array → unvisit, return (arrays aren't top-level definitions).
4. `typeName = nameFor(node)`. If `emittedTypeNames` already has it → mark `emittedIds`, unvisit, return.
5. Insert `emittedIds` + `emittedTypeNames`.
6. **Forward declarations** (`generator.cpp:411-422`): for each child that is a pointer with `refId != 0`, if the target index is valid AND target not yet emitted AND not yet forward-declared → emit `struct TARGETNAME;\n` and record in `forwardDeclared`. (`testForwardDeclarationForPointerTarget`: `struct TargetB;` appears, plus the field `struct TargetB* ptr_to_b`.)
7. `structSize = structSpan(id)`. `kw = resolvedClassKeyword()`.
8. **Enum with members** (`kw=="enum" && !enumMembers.isEmpty()`): emit `enum TYPENAME {\n`, then per member `    NAME = VALUE,\n` (`NAME` sanitized; VALUE = the int64), then `};\n\n`; unvisit; return. (`testEnumCppOutput`.) No `static_assert` for enums.
9. If `kw=="enum"` (but empty members) → `kw="struct"`.
10. Emit `KW TYPENAME\n{\n`. (Vergilius style: brace on its own line. `testSimpleStruct`: `struct Player\n{`.)
11. `emitStructBody(id, isUnion = (kw=="union"), depth=1, baseOffset=0)`.
12. Emit `};` + sizeof-comment (`\x01// sizeof 0xSIZE`) + `\n`.
13. If `emitAsserts`: emit `static_assert(sizeof(TYPENAME) == 0xSIZE, "Size mismatch for TYPENAME");\n`. (`testSimpleStruct`: `static_assert(sizeof(Player) == 0x10` present with asserts, absent without.)
14. Emit trailing `\n`. Unvisit.

### 8.5 C/C++ public functions (`generator.cpp:1439-1498`)
- `renderCpp`: validate root is Struct; preamble; `emitStruct(rootId)`; `alignComments`.
- `renderCppTree`: validate; preamble; for each id in `collectReachableStructs(rootId)` call `emitStruct`; `alignComments`. (Dedup ensures each emitted once; dependency order means targets defined before/independently.)
- `renderCppAll`: preamble; roots = `childMap[0]` sorted by offset; `emitStruct` each Struct-kind root; `alignComments`.

---

## 9. Rust backend (`generator.cpp:507-787`)

### `rustTypeName` (`generator.cpp:507-540`)
HexN/UIntN/IntN → `u8/u16/u32/u64/u128` and `i8…i128`; **Hex128 → `u128`** (in the table, but collapsed to `[u8; 0x10]` padding as a field like C); Float16 → `f16`; Float → `f32`; Double → `f64`; Bool → `bool`; Pointer/FuncPtr 32→`u32`,64→`u64`; Vec/Mat → `f32`; UTF8 → `u8`; UTF16 → `u16`; default → `u8`. `rustType(ctx,kind)` consults aliases first.

### `emitRustField` (`generator.cpp:554-596`)
- Vec2/3/4 → `pub NAME: [f32; 2|3|4],`; Mat4x4 → `pub NAME: [[f32; 4]; 4],`.
- UTF8 → `pub NAME: [u8; strLen],`; UTF16 → `pub NAME: [u16; strLen],`.
- Pointer with `refId` valid → `pub NAME: *mut TARGET,` (`testRustPointers`/`testRustPointerField`: `*mut Target`). Native ptr → `pub NAME: *mut core::ffi::c_void,` (`testRustPointers`: `*mut core::ffi::c_void`). Cross-size → `pub NAME: rustType(kind),`.
- FuncPtr32/64 → `pub NAME: Option<unsafe extern "C" fn()>,` (`testRustFuncPtr`, `testRustFuncPtrOption`).
- default → `pub NAME: rustType(kind),`.

### `emitRustStructBody` (`generator.cpp:598-729`)
Same skeleton as C++ but Rust syntax. Differences:
- Pad run → `pub _padNNNN: [u8; 0xSIZE],` + comment. (`testRustPadding`: `pub _pad` + `[u8; 0x4]`.)
- **No overlap WARNING** in Rust body (only the `child.offset > cursor` gap path is present; the `< cursor` overlap branch is omitted).
- **Bitfield**: Rust has no bitfields → emit `pub NAME: BFTYPE, // bits: a:3, b:5` + comment. Field name fallback `bitfield_HEX`. BFTYPE = `rustType(elementKind)` (fallback `u32`).
- **Anonymous inline struct**: Rust can't nest anonymously → flatten to `pub NAME: [u8; 0xSPAN],` (SPAN = structSpan). Name fallback `anon_HEX`.
- **Named struct child** → `pub FIELDNAME: TYPENAME,` (KW enum→struct fixup retained but unused for the rendered text).
- **Array** struct-element → `pub NAME: [ELEM; arrayLen],`; primitive → `pub NAME: [rustType(elementKind); arrayLen],`.
- Tail padding same. Static fields → `// static: TYPE NAME @ offsetExpr` (TYPE = structTypeName or rustType(kind)).

### `emitRustStruct` (`generator.cpp:731-787`)
Dedup/cycle like C++ (but **only** Struct kind accepted — Array silently returns; no Array branch). No forward declarations (Rust doesn't need them).
- **Enum with members**: `#[repr(i64)]\npub enum TYPENAME {\n` then `    NAME = VALUE,\n` then `}\n\n`.
- **Union** (`kw=="union"`): header `#[repr(C)]\n#[derive(Copy, Clone)]\n#[allow(dead_code)]\npub union TYPENAME {\n`.
- **Struct**: header `#[repr(C)]\n#[derive(Debug)]\n#[allow(dead_code)]\npub struct TYPENAME {\n` (`testRustSimpleStruct`, `testRustAllowDeadCode`).
- body via `emitRustStructBody(..., depth=1, baseOffset=0)`.
- Close `}` + sizeof-comment + `\n`. If `emitAsserts`: `const _: () = assert!(core::mem::size_of::<TYPENAME>() == 0xSIZE);\n` (`testRustSimpleStruct`: `core::mem::size_of::<Player>() == 0x10`). Trailing `\n`.
- Header comment string is literally `"// Generated by Reclass 2027"` (`testRustSimpleStruct`).

---

## 10. `#define` offsets backend (`generator.cpp:793-846`, `1557-1602`)

`emitDefinesForStruct(ctx, structId, prefix, baseOffset)` — recursive. `typeName = prefix.isEmpty() ? nameFor(node) : prefix`.
- **Enum with members**: emit `// TYPENAME (enum)\n` then `#define TYPENAME_MEMBER VALUE\n` for each, then `\n`. (`testDefinesEnumMembers`: `Status_OK 0`, `Status_ERR 1`.)
- Else: emit `// TYPENAME (0xSIZE bytes)\n` (SIZE from structSpan). Children = `childMap[structId]` **sorted by offset**. For each child: skip if `isStatic`; skip if `isHexNode` (hex/padding excluded — `testDefineSkipsHex`). Compute `fieldName` (sanitized, fallback `field_HEX`), `absOffset = baseOffset + child.offset`. Emit `#define TYPENAME_FIELDNAME 0xABS\n`. Then **recurse** into the child if it is a named non-bitfield Struct (`kind==Struct && !structTypeName.isEmpty() && classKeyword != "bitfield"`), with new prefix `TYPENAME_FIELDNAME` and `baseOffset = absOffset`. Trailing `\n`.

Public functions add the `#pragma once\n#include <cstdint>\n\n` preamble and **return `ctx.output` raw** (no `alignComments`, no `typeAliases`/`emitAsserts` parameters). `renderDefinesTree` iterates `collectReachableStructs`; `renderDefinesAll` iterates offset-sorted roots. (`testDefineSimpleStruct`: `#define Player_health 0x0` etc.)

---

## 11. C# backend (`generator.cpp:852-1085`)

`csTypeName` (`generator.cpp:852-885`): byte/ushort/uint/ulong for HexN/UIntN; **Hex128 → `byte`** (emitted `fixed byte[16]`); sbyte/short/int/long for IntN; Int128→`Int128`, UInt128→`UInt128`; Float16→`Half`; float/double/bool; Pointer32/FuncPtr32→`uint`, 64→`ulong`; Vec/Mat→`float`; UTF8→`byte`; UTF16→`char`; default→`byte`. `csType(ctx,kind)` consults aliases.

`emitCSharpStructBody` (`generator.cpp:899-1033`): **uses explicit `[FieldOffset(N)]` — no manual padding**, so it iterates `children` (offset-sorted) **without a cursor**, and **skips hex nodes** (`isHexNode → continue`). For each non-hex child at `absOffset = baseOffset+child.offset`:
- Bitfield → `[FieldOffset(0xABS)] public BFTYPE NAME; // bits: …` + comment.
- Anonymous struct → `[FieldOffset(0xABS)] public fixed byte NAME[0xSPAN];` + comment.
- Named struct → `[FieldOffset(0xABS)] public TYPENAME NAME;` + comment.
- Array of struct → `[FieldOffset(0xABS)] [MarshalAs(UnmanagedType.ByValArray, SizeConst = arrayLen)] public ELEM[] NAME;`.
- Array primitive → `[FieldOffset(0xABS)] public fixed ELEMTYPE NAME[arrayLen];`.
- Vec2/3/4 → `public fixed float NAME[2|3|4];`; Mat4x4 → `[16]` (`testCSharpVec3`: `fixed float` + `[3]`).
- UTF8 → `public fixed byte NAME[strLen];`; UTF16 → `public fixed char NAME[strLen];`.
- Pointer: native → `public IntPtr NAME;` (`testCSharpPointers`: `IntPtr ptr`); cross-size → `public csType(kind) NAME;`.
- FuncPtr → `public IntPtr NAME; // fn ptr`.
- default → `public csType(kind) NAME;`.
- Static fields → `// static: …` comments.

`emitCSharpStruct` (`generator.cpp:1035-1085`): dedup/cycle (Struct-only).
- Enum members → `public enum TYPENAME : long\n{\n    NAME = VALUE,\n}\n\n` (`testCSharpEnum`).
- Else header `[StructLayout(LayoutKind.Explicit, Size = 0xSIZE)]\npublic unsafe struct TYPENAME\n{\n` (note: **even unions** use this struct layout — `isUnion` is computed but unused in the C# header beyond body iteration). Body, then `}` + sizeof-comment + `\n\n`. **No static_assert** in C#.
Public funcs: preamble `using System.Runtime.InteropServices;\n#nullable disable\n\n`; return `alignComments`. (`testCSharpSimpleStruct`, `testCSharpStructLayoutSize`, `testCSharpNullableDisable`.)

---

## 12. Python ctypes backend (`generator.cpp:1091-1359`)

`pyTypeName` (`generator.cpp:1091-1124`): `ctypes.c_uint8/16/32/64`, **Hex128/UInt128 → `ctypes.c_uint8 * 16`**, **Int128 → `ctypes.c_int8 * 16`**, `ctypes.c_int8/16/32/64`, Float16→`ctypes.c_uint16` (no half), `ctypes.c_float`, `ctypes.c_double`, `ctypes.c_bool`, Pointer/FuncPtr 32→`ctypes.c_uint32`/64→`ctypes.c_uint64`, Vec/Mat→`ctypes.c_float`, UTF8→`ctypes.c_char`, UTF16→`ctypes.c_wchar`, default→`ctypes.c_uint8`. **Python does NOT consult typeAliases** (no `pyType(ctx,...)` wrapper — `renderPython*` pass `nullptr` aliases anyway).

`emitPythonStructBody` (`generator.cpp:1129-1300`): cursor-based like C++ (manual padding), indent is fixed `8 spaces` (2 levels inside `_fields_`). Pad field → `("_padNNNN", ctypes.c_uint8 * 0xSIZE),` + comment. Hex-run collapse identical to C++. Per child emits a `("name", TYPE),` tuple line:
- Bitfield → `("name", BFTYPE), # bits: …` + comment.
- Anonymous struct → `("name", ctypes.c_uint8 * 0xSPAN),`.
- Named struct → `("name", TYPENAME),`.
- Array of struct → `("name", ELEM * arrayLen),`; primitive → `("name", pyTypeName(elementKind) * arrayLen),`.
- Vec2/3/4 → `ctypes.c_float * 2|3|4`; Mat4x4 → `(ctypes.c_float * 4) * 4`.
- UTF8 → `ctypes.c_char * strLen`; UTF16 → `ctypes.c_wchar * strLen`.
- Pointer with `refId` valid → `("name", ctypes.POINTER(TARGET)),` (`testPythonTypedPointers`). Else native → `ctypes.c_void_p`; cross-size → `pyTypeName(kind)`.
- FuncPtr → `("name", ctypes.CFUNCTYPE(None)),` (`testPythonFuncPtrCFUNCTYPE`).
- Tail padding appended (no `# WARNING`, no overlap path). **Note:** no static-field comments inside the body (handled in the wrapper instead).

`emitPythonStruct` (`generator.cpp:1302-1359`): dedup/cycle (Struct-only).
- Enum members → `class TYPENAME:  # enum\n    __slots__ = ()\n` then `    NAME = VALUE\n` per member then `\n` (`testPythonEnum`, `testPythonEnumSlots`).
- Else: `baseClass = isUnion ? "ctypes.Union" : "ctypes.Structure"` (`testPythonUnionOutput`). Emit `class TYPENAME(BASECLASS):` + sizeof-comment + `\n`, then `    _fields_ = [\n`, body, `    ]\n`. Then static-field comments: `    # static: TYPE NAME @ offsetExpr` (TYPE = `pyTypeName(sf.kind)` — note: ignores structTypeName here, unlike C/Rust/C#). Trailing `\n`.
Public funcs: preamble `import ctypes\n\n`; return `alignComments`. (`testPythonSimpleStruct` → `class Player(ctypes.Structure)`.)

---

## 13. `sanitizeIdent` (`generator.cpp:13-24`)

Identifier sanitizer used everywhere:
- Empty input → `"unnamed"`.
- For each char: keep if `c.isLetterOrNumber() || c == '_'`, else replace with `'_'`.
- After building, if `out[0]` is not a letter and not `'_'` (e.g. starts with a digit) → prepend `'_'`.
(`testNameSanitization`: `"my struct-name"` → `my_struct_name`; `"field with spaces"` → `field_with_spaces`.)
> Rust note: `QChar::isLetterOrNumber()` is Unicode-aware (accepts letters/digits in any script). A faithful port should use a Unicode-aware predicate (e.g. `char::is_alphanumeric`) rather than ASCII-only, to match edge cases on non-ASCII names.

---

## 14. Qt usage → Rust equivalents

| Qt type/API | Use in generator | Rust equivalent |
|---|---|---|
| `QString` | All text building/return | `String` |
| `QStringLiteral("…")` | Compile-time string literals | `&str` / `format!` |
| `QString::arg(...)` (positional `%1 %2`, with width/base/fill) | Formatting (`.arg(n, width, 16, QChar('0'))` = hex, zero-padded) | `format!("{:04x}", n)` etc. **Careful: hex outputs are sometimes lowercase (`uniquePadName`, `field_%`, `anon_%`) and sometimes uppercase (`offsetComment` via `QString::number(off,16).toUpper()`, pad-array sizes). Preserve case exactly.** |
| `QString::number(n, 16)` + `.toUpper()` | Offset/size comments, pad sizes (uppercase, no `0x` from number) | `format!("{:X}", n)` |
| `QChar(0x01)` (`kCommentMarker`) | Internal sentinel | a control char `'\u{1}'` |
| `QChar::isLetter/isLetterOrNumber` | sanitizeIdent | `char::is_alphabetic` / `char::is_alphanumeric` (Unicode) |
| `QString::prepend`, `+=`, `+`, `.left/.mid/.indexOf` | Buffer assembly, alignComments | `String` push/format, `str` slicing, `find` |
| `QString::split('\n')` / `QStringList::join` | alignComments, bitfield bits join | `str::split('\n')`, `slice::join` |
| `QHash<K,V>` | `childMap`, `typeAliases`, `nameById` | `HashMap` (or `BTreeMap` for determinism — see §16) |
| `QSet<T>` | dedup/cycle/forward-decl guards, visited | `HashSet` (or `BTreeSet`) |
| `QVector<T>` | child index lists, results | `Vec<T>` |
| `QPair<QString,int64_t>` | enum members | `(String, i64)` |
| `std::sort` with lambda | sort children/roots by offset | `Vec::sort_by_key` / `sort_by` (note: not stable in C++) |
| `std::function<void(uint64_t)>` recursion | collectReachableStructs | closure / recursive fn |
| `std::pair` | prepareChildren return | tuple |

No file I/O is in the generator itself (tests do `QTemporaryFile` writes externally, `testExportToFile`). No threads, no async, no signals/slots, no `#ifdef`. Fully portable.

---

## 15. Edge cases & invariants the tests pin down

1. **Invalid root id** → empty string (`testInvalidRootId`, `renderCpp(tree, 9999)`).
2. **Non-Struct root** → empty string (`testNonStructRoot`, a `UInt32` root).
3. **Null generator** → always empty (`testNullGenerator`).
4. **Empty struct** → emits `struct Empty\n{`, `};`, `sizeof(Empty) == 0x0` (`testEmptyStruct`).
5. **FullSdk with no structs** → preamble only, no `struct ` text (`testFullSdkNoStructs`).
6. **Duplicate root type names** disambiguated `_v2` (`testDuplicateTypeNameDisambiguation`).
7. **Opaque named child** (no children, no `refId`) → referenced by name, **no body, no padding** generated for it in Current scope (`testOpaqueTypeNoStub`). It only gets a body if reached via Tree/All scope.
8. **Static fields**: excluded from layout & size, emitted as `// static: TYPE NAME @ EXPR` comment after the body (`testStaticField*`).
9. **Union**: no padding, no overlap warnings, deliberate offset overlap allowed (`testUnionNoOverlapWarning`, `testUnionCppOutput`).
10. **Overlap (non-union)** → `// WARNING: overlap …` comment (C++ only) (`testOverlapWarning`).
11. **Padding**: gaps and tail filled with `uint8_t _padNNNN[0xN]` (C/Python), `pub _padNNNN: [u8; 0xN]` (Rust); C#/#define use explicit offsets / skip.
12. **Hex nodes collapse** into a single padding array; Hex128 → `[0x10]` (`testHex128*`).
13. **Pointer rendering**: typed→`struct T*`/`*mut T`/`POINTER(T)`/`IntPtr`; untyped native→`void*`/`*mut c_void`/`c_void_p`/`IntPtr`; cross-pointer-size→raw integer (`testPointerFields`, `testPointerFieldCpp`).
14. **Forward declaration** for not-yet-emitted pointer targets in C++ (`testForwardDeclarationForPointerTarget`); only C++ emits these.
15. **`emitAsserts`** toggles `static_assert` (C++) / `const _: () = assert!` (Rust); C#/Python/#define never emit asserts (`testSimpleStruct`, `testRustSimpleStruct`).
16. **Enum members** emitted as language-native enums/`#define`s/Python class constants; member values are `int64_t` and printed verbatim.
17. **Tree scope** pulls in pointer targets & named children (`testTreeScopeIncludesReferencedTypes`, `testTreeScopeDispatch`).
18. **Type aliases** override primitive names (C/Rust/C# only) (`testCppTypeAliases`: `Int32`→`LONG`).
19. `testCppNullptrPointerValue` references `rcx::fmt::fmtPointer32/64` — that is the **formatter** subsystem (`fmt::`), **not** the generator. (Pointer *values* aren't part of code generation; the generator only emits type declarations.) Out of scope for the generator port itself but noted because the test file mixes it in.

---

## 16. Determinism / port notes

- **Output must be byte-stable** (tests use substring `contains`, but `alignComments` depends on the maximal code column, and `_padNNNN` numbering depends on emission order). The port must preserve: child sort by offset; root/reachable iteration order; `padCounter` shared across the whole render; `nameById` pass order (roots first by insertion, then nested-named).
- `QHash`/`QSet` iteration order is **unspecified** in Qt. The generator never iterates `childMap`/`emittedTypeNames`/etc. in a way that leaks order into output **except** through `QVector` child-index lists (which are insertion-ordered and then offset-sorted) — so a Rust `HashMap`/`HashSet` is fine for the guards, but child lists must stay `Vec` in insertion order before sorting. Using `BTreeMap`/`BTreeSet` is a safe conservative choice.
- `std::sort` is **not stable**; equal-offset siblings (notably union members at offset 0) may be reordered. Tests don't assert order among equal offsets, so either a stable or unstable sort passes — but to be conservative, replicate with `sort_by_key(offset)` and document the non-determinism for equal offsets.
- Integer formatting case must match exactly (lowercase for `_pad`/`field_`/`anon_` ids; uppercase for offset/size hex). See §14.
- The control-char marker `\u{1}` and the two-pass `alignComments` must be reproduced verbatim for column alignment to match.
- `structSpan` (in `core.h`) is a hard dependency — the port's `NodeTree::struct_span` must already exist with identical semantics (cycle→0, static excluded, refId-embedded fallback, `max(declared, maxEnd)`).
