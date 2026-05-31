# Subsystem: Core node/type model (`core-model`)

**C++ source studied (all read in full):**
- `src/core.h` (1536 lines) — the central data model: `NodeKind`, `KindMeta`, `Node`, `NodeTree`, `ValueHistory`, `LineMeta`/`LineChip`/`ChipKind`, `Command` variant, column-span helpers, `ViewState`, and forward decls for `fmt::`/`compose`.
- `src/commontypes.h` (405 lines) — static table of predefined struct templates (`CommonType`/`CommonField`) and `findCommonType()`.
- `src/typeinfer.h` (550 lines) — heuristic type-inference engine (`inferTypes()`, `InferHints`, `TypeSuggestion`, feature checkers).

**Cross-referenced for accuracy:**
- `src/compose.cpp:1747-1784` — out-of-line definitions of `NodeTree::normalizePreferAncestors/Descendants`.
- `src/providers/provider.h` — the abstract `Provider` base (core.h includes it; details are OUT OF SCOPE except as a trait the model references).
- `tests/test_core.cpp` (1570 lines) and `tests/test_typeinfer.cpp` (187 lines) — exact behavioral contracts.

---

## 1. Purpose

This subsystem is the in-memory document model for the structured-data editor. It defines:

1. **What types of fields exist** (`NodeKind` enum + the `KindMeta` metadata table that is the *single source of truth* for size/alignment/display name/line count/flags).
2. **The node tree** (`NodeTree` holding a flat `QVector<Node>` with parent links by id), including geometry math (offsets, spans, depth), structural integrity (validate/repair, overlap detection, cycle-safe traversal), id↔path mapping, and JSON serialization.
3. **Predefined struct layouts** (`commontypes.h`) the user can instantiate from the type chooser (Windows NT, MSVC STL, Unreal, generic, math).
4. **Heuristic type inference** (`typeinfer.h`) that scores byte patterns into candidate `NodeKind`s for the "type hint chips".

Everything here is **pure data + algorithms** — no UI, no OS calls, no threading. The only Qt usage is container/string/JSON types and a few helpers (`qBound`, `qMin`, `QDateTime::currentMSecsSinceEpoch`). The model reaches into the live target only through the abstract `Provider` interface (passed by reference to `compose`/`fmt`, which live in other subsystems). Portability: **pure**.

---

## 2. `NodeKind` enum and the `KindMeta` table

### 2.1 `enum class NodeKind : uint8_t` (`core.h:24-34`)

Ordered (the integer value matters — it indexes `kKindMeta`):

```
Hex8, Hex16, Hex32, Hex64, Hex128,          // 0-4
Int8, Int16, Int32, Int64, Int128,          // 5-9
UInt8, UInt16, UInt32, UInt64, UInt128,     // 10-14
Float16, Float, Double, Bool,               // 15-18
Pointer32, Pointer64,                        // 19-20
FuncPtr32, FuncPtr64,                         // 21-22
Vec2, Vec3, Vec4, Mat4x4,                     // 23-26
UTF8, UTF16,                                  // 27-28
Struct, Array                                 // 29-30
```

`Array` (30) is the last value; `kKindMeta` has exactly 31 entries (`static_assert` at `core.h:99`). **Rust:** `#[repr(u8)] enum NodeKind`. Discriminant order MUST be preserved (JSON uses string names, but `kindMeta(k)` indexes by `as usize`). A pre-Qt6 `qHash` overload exists (`core.h:37-39`) only for Qt<6 — irrelevant for the port (target is Qt6 / Rust `HashMap`).

### 2.2 `enum KindFlags : uint32_t` (`core.h:44-50`)

Bitmask: `KF_None=0`, `KF_HexPreview=1<<0`, `KF_Container=1<<1`, `KF_String=1<<2`, `KF_Vector=1<<3`. **Rust:** `bitflags!` or plain `u32` consts.

### 2.3 `struct KindMeta` (`core.h:54-62`) and `kKindMeta[]` (`core.h:64-97`)

Fields: `NodeKind kind; const char* name; const char* typeName; int size; int lines; int align; uint32_t flags;`.

- `name` = UI/JSON name ("Hex64", "UInt16"). Used for JSON round-trip.
- `typeName` = display/C name ("hex64", "uint16_t", "ptr64", "str", "wstr", "struct", "array"). Used for type-name UI and `kindFromTypeName`.
- `size` = byte size; **0 for Struct & Array** (dynamic — need tree context).
- `lines` = display line count; **1 for everything except Mat4x4 = 4**.
- `align` = natural alignment.
- `flags` = `KindFlags` bitmask.

Full table (memorize exactly — tests assert these):

| kind | name | typeName | size | lines | align | flags |
|---|---|---|---|---|---|---|
| Hex8 | Hex8 | hex8 | 1 | 1 | 1 | HexPreview |
| Hex16 | Hex16 | hex16 | 2 | 1 | 2 | HexPreview |
| Hex32 | Hex32 | hex32 | 4 | 1 | 4 | HexPreview |
| Hex64 | Hex64 | hex64 | 8 | 1 | 8 | HexPreview |
| Hex128 | Hex128 | hex128 | 16 | 1 | 16 | HexPreview |
| Int8 | Int8 | int8_t | 1 | 1 | 1 | None |
| Int16 | Int16 | int16_t | 2 | 1 | 2 | None |
| Int32 | Int32 | int32_t | 4 | 1 | 4 | None |
| Int64 | Int64 | int64_t | 8 | 1 | 8 | None |
| Int128 | Int128 | int128_t | 16 | 1 | 8 | None |
| UInt8 | UInt8 | uint8_t | 1 | 1 | 1 | None |
| UInt16 | UInt16 | uint16_t | 2 | 1 | 2 | None |
| UInt32 | UInt32 | uint32_t | 4 | 1 | 4 | None |
| UInt64 | UInt64 | uint64_t | 8 | 1 | 8 | None |
| UInt128 | UInt128 | uint128_t | 16 | 1 | 8 | None |
| Float16 | Float16 | float16 | 2 | 1 | 2 | None |
| Float | Float | float | 4 | 1 | 4 | None |
| Double | Double | double | 8 | 1 | 8 | None |
| Bool | Bool | bool | 1 | 1 | 1 | None |
| Pointer32 | Pointer32 | ptr32 | 4 | 1 | 4 | None |
| Pointer64 | Pointer64 | ptr64 | 8 | 1 | 8 | None |
| FuncPtr32 | FuncPtr32 | fnptr32 | 4 | 1 | 4 | None |
| FuncPtr64 | FuncPtr64 | fnptr64 | 8 | 1 | 8 | None |
| Vec2 | Vec2 | vec2 | 8 | 1 | 4 | Vector |
| Vec3 | Vec3 | vec3 | 12 | 1 | 4 | Vector |
| Vec4 | Vec4 | vec4 | 16 | 1 | 4 | Vector |
| Mat4x4 | Mat4x4 | mat4x4 | 64 | **4** | 4 | None |
| UTF8 | UTF8 | str | 1 | 1 | 1 | String |
| UTF16 | UTF16 | wstr | 2 | 1 | 2 | String |
| Struct | Struct | struct | **0** | 1 | 1 | Container |
| Array | Array | array | **0** | 1 | 1 | Container |

Note `Int128`/`UInt128`/`UInt64`/`Int64`/`Double`/`Pointer64`/`FuncPtr64` all align 8; `Int128`/`UInt128` are size 16 but align **8** (not 16), whereas `Hex128` aligns **16**.

**Rust:** a `const [KindMeta; 31]` static array indexed by `kind as usize`, with `&'static str` for the two names.

### 2.4 Free functions over kinds (all `inline`/`constexpr`, `core.h:102-178`)

- `kindMeta(NodeKind k) -> const KindMeta*`: `i = (unsigned)k; return i < 31 ? &table[i] : nullptr`. **Rust:** `fn kind_meta(k) -> Option<&'static KindMeta>` (or always-Some since enum is closed; the `nullptr` path is dead for valid enum values but `sizeForKind` etc. defensively handle it).
- `sizeForKind(k) -> int`: `m ? m->size : 0`.
- `linesForKind(k) -> int`: `m ? m->lines : 1` (note default **1**, not 0).
- `alignmentFor(k) -> int`: `m ? m->align : 1`.
- `kindToString(k) -> const char*`: `m ? m->name : "Unknown"`.
- `kindFromString(const QString& s) -> NodeKind`: linear scan over `kKindMeta`, match `s == m.name`; **fallback `NodeKind::Hex8`** when not found. (Used by JSON load — unknown kinds become Hex8.)
- `kindFromTypeName(const QString& s, bool* ok=nullptr) -> NodeKind`: linear scan matching `s == m.typeName`; sets `*ok=true` on hit, else `*ok=false` and returns `Hex8`.
- `flagsFor(k) -> uint32_t`: `m ? m->flags : 0`.
- Predicates (all `constexpr bool`): `isHexNode(k)` = `Hex8 <= k <= Hex128` (by enum ordering); `isHexPreview(k)` = same as `isHexNode`; `isVectorKind(k)` = `Vec2||Vec3||Vec4`; `isMatrixKind(k)` = `Mat4x4`; `isFuncPtr(k)` = `FuncPtr32||FuncPtr64`; `isPointerKind(k)` = `Pointer32||Pointer64`; `isContainerKind(k)` = `Struct||Array`; `isStringKind(k)` = `UTF8||UTF16`.
- `isValidPrimitivePtrTarget(k) -> bool` (`core.h:164-170`): returns **false** for hex nodes, pointer kinds, func ptrs, Struct, Array; true otherwise. Semantics: "dereferencing these as a primitive-pointer target is meaningless / same as void*".
- `allTypeNamesForUI(bool stripBrackets=false) -> QStringList` (`core.h:172-178`): returns every `m.typeName` in table order as a `QStringList` (the `stripBrackets` param is ignored — `/*stripBrackets*/`). **Rust:** `Vec<String>` / `Vec<&'static str>`.

**Tests assert** (`test_core.cpp`): every enum value 0..=Array has a non-null `KindMeta` with matching `kind`, non-null names, `lines>=1`, `align>=1`; `kindToString`→`kindFromString` round-trips for all kinds; `kindFromTypeName(typeName)` round-trips with `ok=true`; specific sizes/lines/aligns (e.g. `Vec3`=12, `Mat4x4` size 64/lines 4/align 4, `Hex128`=16, `Struct` size 0/align 1).

---

## 3. `struct Node` (`core.h:210-363`) — the field/record

### 3.1 Fields (with defaults and meaning)

| field | type | default | meaning |
|---|---|---|---|
| `id` | `uint64_t` | 0 | unique node id; **0 = "unassigned"** (addNode auto-assigns) |
| `kind` | `NodeKind` | `Hex8` | node type |
| `name` | `QString` | "" | field name |
| `structTypeName` | `QString` | "" | Struct/Array: optional named type, e.g. "IMAGE_DOS_HEADER" |
| `classKeyword` | `QString` | "" | "struct"/"class"/"union"/"enum"/"bitfield"; **empty == "struct"** |
| `parentId` | `uint64_t` | 0 | parent node id; **0 = root** |
| `offset` | `int` | 0 | byte offset within parent (can be large; tests use 0x7FFFFFFF) |
| `isStatic` | `bool` | false | static field — excluded from struct layout/span |
| `offsetExpr` | `QString` | "" | C/C++ expression → absolute address (static fields only) |
| `isRelative` | `bool` | false | Pointer: target = base + value (RVA) vs absolute |
| `arrayLen` | `int` | 1 | Array element count |
| `strLen` | `int` | 64 | string length (UTF8/UTF16) |
| `collapsed` | `bool` | true | UI fold state |
| `refId` | `uint64_t` | 0 | Pointer32/64: id of Struct to expand at `*ptr`; also used by embedded-struct span |
| `elementKind` | `NodeKind` | `UInt8` | Array element type; for Pointer with ptrDepth>0: primitive target type |
| `ptrDepth` | `int` | 0 | Pointer: 0=struct/void*, 1=primitive*, 2=primitive** |
| `viewIndex` | `int` | 0 | Array: transient current view offset (NOT serialized) |
| `enumMembers` | `QVector<QPair<QString,int64_t>>` | {} | Enum: name→value pairs |
| `bitfieldMembers` | `QVector<BitfieldMember>` | {} | Bitfield: per-bit member defs |
| `comment` | `QString` | "" | user annotation (rendered "// text") |
| `bigEndian` | `bool` | false | scalar is big-endian (swap on display/parse) |

**`struct BitfieldMember`** (`core.h:202-206`): `QString name; uint8_t bitOffset=0; uint8_t bitWidth=1;` (bitOffset = position from LSB; bitWidth = 1..64).

`static constexpr int kMaxArrayLen = 1000000` (`core.h:198`) — array length clamp, also bounded by the 20-bit `kArrayElemMask` encoding (`static_assert kMaxArrayLen <= (1<<20)` at `core.h:940`).

**Rust mapping:** `QString`→`String`, `QVector<T>`→`Vec<T>`, `QPair<A,B>`→`(A,B)`. `viewIndex` is transient (not in JSON) — keep as a runtime field, default 0.

### 3.2 `int Node::byteSize() const` (`core.h:238-255`) — leaf size only

Switch on `kind`:
- `UTF8` → `strLen`.
- `UTF16` → `qMin(strLen, INT_MAX/2) * 2` (overflow-guarded doubling).
- `Array` → `elemSz = sizeForKind(elementKind); if (elemSz <= 0) return 0; else qMin(arrayLen, INT_MAX/elemSz) * elemSz`. (Array-of-Struct/Array returns 0 because `elementKind` is a primitive kind only; element 0-size containers yield 0.)
- `Struct` → if `classKeyword == "bitfield"`: `sz = sizeForKind(elementKind); return sz>0 ? sz : 4` (container width, default 4). Otherwise return **0** (real container size needs the tree).
- default → `sizeForKind(kind)`.

**Edge:** non-bitfield Struct and Array-of-container return 0; callers must use `totalByteSize(tree)`.

### 3.3 `int Node::totalByteSize(const NodeTree& tree) const` (`core.h:841-845`, out-of-line)

If `kind==Struct||kind==Array` → `tree.structSpan(id)`. Else → `byteSize()`. This is the container-aware footprint.

### 3.4 `Node::toJson() -> QJsonObject` (`core.h:263-313`) — serialization

Always written: `id` (as **decimal string** via `QString::number`), `kind` (string name via `kindToString`), `name`, `parentId` (decimal string), `offset` (int), `arrayLen` (int), `strLen` (int), `collapsed` (bool), `refId` (decimal string), `elementKind` (string name).

Conditionally written (omitted when default/empty):
- `structTypeName` — only if non-empty.
- `classKeyword` — only if non-empty AND != "struct".
- `isStatic` — only if true (value `true`). *(Backward-compat: old files used `isHelper`.)*
- `offsetExpr` — only if non-empty.
- `isRelative` — only if true.
- `ptrDepth` — only if `> 0`.
- `enumMembers` — only if non-empty; array of `{name, value(decimal string)}`.
- `bitfieldMembers` — only if non-empty; array of `{name, bitOffset(int), bitWidth(int)}`.
- `comment` — only if non-empty.
- `bigEndian` — only if true.

**`viewIndex` is NOT serialized.** **`id`/`parentId`/`refId` are stored as STRINGS** (because JSON numbers are doubles and would lose precision on 64-bit ids). `enum value` is also a string. `offset`/`arrayLen`/`strLen`/`ptrDepth`/`bitOffset`/`bitWidth` are JSON ints.

### 3.5 `static Node Node::fromJson(const QJsonObject& o) -> Node` (`core.h:314-354`)

Reads with defaults and clamps:
- `id` = `o["id"].toString("0").toULongLong()` (default "0").
- `kind` = `kindFromString(o["kind"].toString())` (unknown → Hex8).
- `name`, `structTypeName`, `classKeyword` = `.toString()`.
- `parentId` = string→u64 default "0".
- `offset` = `.toInt(0)`.
- `isStatic` = `o["isStatic"].toBool(o["isHelper"].toBool(false))` — **backward compat with legacy `isHelper` key**.
- `offsetExpr` = `.toString()`.
- `isRelative` = `.toBool(false)`.
- `arrayLen` = `qBound(1, o["arrayLen"].toInt(1), kMaxArrayLen)` → clamp [1, 1000000].
- `strLen` = `qBound(1, o["strLen"].toInt(64), 1000000)` → clamp [1, 1000000].
- `collapsed` = **always `true`** (ignores stored value: "Always load collapsed; user expands as needed").
- `refId` = string→u64 default "0".
- `elementKind` = `kindFromString(o["elementKind"].toString("UInt8"))`.
- `ptrDepth` = `qBound(0, o["ptrDepth"].toInt(0), 2)` → clamp [0,2].
- `enumMembers` — if key present: per obj `{name, value(string→i64 default "0")}`.
- `bitfieldMembers` — if key present: `name`; `bitOffset = (uint8_t)qBound(0, .toInt(0), 255)`; `bitWidth = (uint8_t)qBound(1, .toInt(1), 64)`.
- `comment` = `.toString()`.
- `bigEndian` = `.toBool(false)`.

**Round-trip caveat:** `collapsed` always loads `true` regardless of saved value — a saved expanded node loads collapsed. Tests rely on every other field round-tripping exactly (`testNodeJsonRoundTrip` sets all optional fields and checks each).

### 3.6 Class-keyword helpers (`core.h:357-362`)

- `resolvedClassKeyword()` → `classKeyword.isEmpty() ? "struct" : classKeyword`.
- `isUnion()` → `resolvedClassKeyword() == "union"`.
- `isBitfield()` → `classKeyword == "bitfield"` (note: checks raw, not resolved — empty is NOT bitfield).
- `isEnum()` → `resolvedClassKeyword() == "enum"`.

---

## 4. `struct Bookmark` (`core.h:367-382`)

`QString name; QString addressFormula;` (formula like `"<game.exe>+0x12340"` survives rebases). `toJson`/`fromJson` are trivial string pairs (both keys always present). **Rust:** struct with two `String`s, serde.

---

## 5. Global hook function pointers (`core.h:391-404`)

Three `extern` function pointers defined in `compose.cpp`, wired by `main.cpp` and left nullptr in tests:
- `void (*g_rttiDiscoveryHook)(const QString& name, uint64_t address, const QString& moduleName)` — RTTI vtable discovery callback.
- `QString (*g_nameLookupHook)(uint64_t address, const Provider* active)` — unified address→name lookup.
- `void (*g_namesChangedHook)()` — named-source-changed nudge.

These are **decoupling seams** so the core/test targets don't depend on the GUI's NameRegistry. **Rust:** model as `Option<Box<dyn Fn(...)>>` injected callbacks, or trait objects on a context. They are not part of the pure node model per se — they live in core.h only as forward seams. For the port, represent as optional callback hooks (default no-op).

---

## 6. `struct NodeTree` (`core.h:408-835`) — the document

### 6.1 Fields

| field | type | default | meaning |
|---|---|---|---|
| `nodes` | `QVector<Node>` | {} | flat node storage (index ≠ id) |
| `baseAddress` | `uint64_t` | `0x00400000` | base of the view |
| `baseAddressFormula` | `QString` | "" | e.g. "`<ReClass.exe> + 0x100`" |
| `pointerSize` | `int` | 8 | 4 (32-bit target) or 8 (64-bit) |
| `initialClass` | `QString` | "" | save-file "auto-open" class hint (non-binding) |
| `bookmarks` | `QVector<Bookmark>` | {} | user-named addresses |
| `m_nextId` | `uint64_t` | 1 | next id to assign |
| `m_idCache` | `mutable QHash<uint64_t,int>` | {} | id→index cache (lazy) |
| `m_childCache` | `mutable QHash<uint64_t,QVector<int>>` | {} | parentId→child-indices cache (lazy) |
| `m_generation` | `quint64` | 1 | bumped on every structural mutation |

**Rust:** `nodes: Vec<Node>`, `m_idCache: RefCell<HashMap<u64,i32>>` / `m_childCache: RefCell<HashMap<u64,Vec<usize>>>` (the caches are `mutable` and filled inside `const` methods — interior mutability). `m_generation: u64`. Caches are an optimization, not semantics — a clean Rust port may rebuild eagerly, but must preserve the **invalidation timing** (see below) because tests like `testDepthOfCycle`/`testComputeOffsetCycle` mutate `parentId` directly then call `invalidateIdCache()`.

### 6.2 Generation counter (`core.h:427-429,452`)

`generation()` returns `m_generation`; `bumpGeneration()` and `touch()` both `++m_generation`. Used by downstream caches (generator output, type popup) to skip rebuilds when shape is unchanged. `addNode` also bumps it.

### 6.3 `int addNode(const Node& n)` (`core.h:431-443`)

1. Copy `n`.
2. **Id assignment:** if `copy.id == 0` → `copy.id = m_nextId++`. Else if `copy.id >= m_nextId` → `m_nextId = copy.id + 1` (keeps m_nextId ahead of any explicit id).
3. `idx = nodes.size(); nodes.append(copy)`.
4. **Incremental cache update only if cache non-empty:** if `m_idCache` non-empty → `m_idCache[copy.id]=idx`; if `m_childCache` non-empty → `m_childCache[copy.parentId].append(idx)`. (If caches are empty, they stay empty and lazily rebuild later.)
5. `++m_generation`.
6. Return `idx` (the new index).

**Tests:** first added node gets id 1, second gets id 2 (`testStableNodeIds`); auto-id nodes get distinct non-zero ids (`testAddNodeAutoId`); `addNode` returns 0 for the first node (`testNodeTree_addAndChildren`).

### 6.4 `uint64_t reserveId()` (`core.h:446`)

`return m_nextId++;` — reserve an id before pushing an undo command. Monotonic (`testReserveIdMonotonic`).

### 6.5 Cache management

- `invalidateIdCache() const` (`core.h:448`): clears BOTH `m_idCache` and `m_childCache`.
- `touch()` (`core.h:452`): `++m_generation` (caller-driven for shape changes).

### 6.6 `ValidateReport validate(bool repair = true)` (`core.h:458-516`)

Inner struct `ValidateReport { int orphans=0, cycles=0, duplicates=0; QString summary(); bool clean(); }`. `summary()` → `"orphans=%1 cycles=%2 duplicates=%3"`; `clean()` → all three zero.

Algorithm (returns immediately with empty report if `nodes.isEmpty()`):

**Pass 1 — dedup ids:** iterate nodes; track `seen: QHash<uint64_t,int>`. If `n.id == 0 || seen.contains(n.id)` → `r.duplicates++`; if repair: `n.id = m_nextId++`. Then `seen.insert(n.id, i)` and bump `m_nextId` past any id ≥ it. (When repair=false the duplicate isn't renumbered but is still counted; later `seen.insert` overwrites.) Then `invalidateIdCache()`.

**Pass 2 — orphans:** for each node with `parentId != 0 && indexOfId(parentId) < 0` → `r.orphans++`; if repair: `n.parentId = 0`. Then `invalidateIdCache()`.

**Pass 3 — cycles:** for each node, walk the parent chain with a `QSet<uint64_t> visited`. If a node id is revisited → `r.cycles++`; if repair: set THAT node's `parentId = 0` (`nodes[cur].parentId = 0`), break. Stops at `parentId == 0`. Then `invalidateIdCache()`.

Returns `r`. **Subtle:** repairs mutate in place; the three passes each invalidate the cache so subsequent `indexOfId` calls see fresh data. **Rust:** keep the three-pass order; renumber duplicates with `m_nextId`; re-root orphans/cycles to parent 0.

### 6.7 `QVector<OverlapPair> findOverlaps() const` (`core.h:530-592`)

`struct OverlapPair { uint64_t aId; uint64_t bId; uint64_t parentId; }` — `aId` is the lower-offset sibling, `bId` the overlapping one, `parentId` the common parent.

Algorithm:
1. Empty tree → empty.
2. Build a **local** `childMap: QHash<uint64_t,QVector<int>>` (NOT the mutable cache, since this is `const`).
3. For each parent group:
   - **Skip `parentId == 0`** (root-level structs are independent classes, not siblings).
   - **Skip if parent `isUnion()`** (unions deliberately overlap at offset 0).
   - For each non-static child compute span: `(kind==Struct||Array) ? structSpan(id) : byteSize()`. **Skip children with span ≤ 0** (treated as points). Push `Range{idx, start=offset, end=offset+sz}` (int64).
   - Sort ranges ascending by `start`.
   - Forward double loop: for each `i`, for each `j>i`: if `ranges[j].start >= ranges[i].end` → **break** (sorted, no more overlaps for this i). Else emit `OverlapPair{aId=nodes[i].id, bId=nodes[j].id, parentId}`.

**Tests** (`test_core.cpp:1315-1469`): clean adjacent layout = none; same-offset pair = 1; partial straddle = 1; fully-contained = 1; touching-but-not-overlapping (`[0,1)`,`[1,2)`) = none; union children = none; static fields excluded; one long `UInt128 [0,16)` over three `UInt32`s = 3 pairs all with `aId==ids[0]`; root-level structs excluded; independent parents isolated. **Not auto-repaired** (no safe choice).

### 6.8 `int indexOfId(uint64_t id) const` (`core.h:594-600`)

If `m_idCache` empty and nodes non-empty → rebuild full id→index map. Return `m_idCache.value(id, -1)` (**-1 on miss**). **Rust:** lazy `HashMap`; missing → `-1` (or `Option<usize>`/sentinel; keep the -1 contract for ports that mirror it). `testIndexOfIdNotFound`/`testStableNodeIds` assert -1 for unknown.

### 6.9 `QVector<int> childrenOf(uint64_t parentId) const` (`core.h:602-608`)

Lazy-build `m_childCache` (parentId→child indices, in node order). Return `m_childCache.value(parentId)` (empty vector on miss). `childrenOf(0)` returns the root-level node indices. `testChildrenOfEmpty` → empty for unknown parent.

### 6.10 `uint64_t nodeIdForPath(const QString& path, QChar sep='.') const` (`core.h:615-645`)

Inverse of `fieldPath`. Returns **0 on any miss** (no partial matches):
1. Empty path or empty tree → 0.
2. `split(sep, SkipEmptyParts)` → if empty → 0.
3. First segment matches a **top-level** node (`parentId==0`) whose `structTypeName == seg[0] || name == seg[0]` (first match wins, by node order). Else → 0.
4. Each subsequent segment: among children of the current node (`parentId == cur.id`), find first whose `name == seg` (exact, **case-sensitive**). Miss → 0.
5. Return matched node's id.

**Tests:** `nodeIdForPath("Player.Stats.Health")` round-trips with `fieldPath`; `""`/`"Nope"`/`"Player.Missing"` → 0.

### 6.11 `QString fieldPath(uint64_t id, QChar sep='.') const` (`core.h:653-668`)

Dot path from root to node. Walk `parentId` chain (cycle-safe via `QSet visited`):
- For each node, `label = name.isEmpty() ? structTypeName : name`. If still empty AND `parentId != 0` → label = `"?"`. If label non-empty → `parts.prepend(label)`.
- Stops on `cur==0`, revisited id, or missing index.
- `return parts.join(sep)`.

**Subtle:** an anonymous root (no name, no structTypeName, parentId 0) contributes nothing → no leading separator. Non-root anonymous nodes become "?". **Tests:** `Player.Health`, root alone → `Player`, nested `Player.Stats.Health`, unknown id → `""`, custom separator `/` works.

### 6.12 `QVector<int> subtreeIndices(uint64_t nodeId) const` (`core.h:671-699`)

Collect node + all descendants, **iterative & cycle-safe** (DFS with explicit stack + `QSet visited`):
1. `idx = indexOfId(nodeId)`; if < 0 → empty.
2. Lazy-build `m_childCache`.
3. Seed result=[idx], visited={nodeId}, stack=[nodeId].
4. Pop pid; for each child index `ci` in `childMap[pid]`: if child id not visited → mark visited, append `ci` to result, push child id.
5. Return result (node index first, then descendants in DFS order).

**Tests:** root+child = 2 indices; root+2 children = 3; self-parent-ish setup terminates (`testSubtreeCycleSafe`).

### 6.13 `int depthOf(int idx) const` (`core.h:701-714`)

Count ancestors. Walk `parentId` chain from `idx`, `d++` per hop, until `parentId==0`, cycle (`QSet visited`), or missing parent. Returns 0 for root, 0 for orphan (parent not found). **Tests:** depth 0/1/2 for 3-level tree; cycle terminates with `d<100`; orphan depth 0.

### 6.14 `int64_t computeOffset(int idx) const` (`core.h:723-736`)

Sum of `offset` up the parent chain (cycle-safe). **Returns `int64_t` and CAN BE NEGATIVE** (a malformed negative offset propagates). Walk: `total += nodes[cur].offset`; stop on `parentId==0`, revisited id, or missing. **Tests:** single field offset 16 → 16; nested 0+16+8 → 24; `0x7FFFFFFF` preserved (no overflow); cycle terminates.

### 6.15 `uint64_t absoluteAddress(int idx, bool* ok=nullptr) const` (`core.h:742-750`)

Safe wrapper: `off = computeOffset(idx)`. If `off < 0` → `*ok=false`, return `baseAddress` alone. Else `*ok=true`, return `baseAddress + (uint64_t)off`. **Critical safety note** (`core.h:716-722`): casting a negative `computeOffset` to `uint64_t` wraps into high addresses → use `absoluteAddress`. **Rust:** return `(u64, bool)` or `Option<u64>`.

### 6.16 `int structSpan(uint64_t structId, const QHash<uint64_t,QVector<int>>* childMap=nullptr, QSet<uint64_t>* visited=nullptr) const` (`core.h:752-787`)

Container footprint, recursive, cycle-safe:
1. `visited` defaults to a local set. **If `visited` already contains `structId` → return 0** (cycle).
2. Insert `structId`.
3. `idx = indexOfId(structId)`; < 0 → 0.
4. `declaredSize = node.byteSize()`.
5. **Short-circuit:** if `!isContainerKind(kind) && refId == 0` → return `declaredSize` (leaf; no children to walk).
6. `maxEnd = 0`. For each child (`childMap ? childMap->value : childrenOf`): **skip `isStatic`**; `sz = (kind==Struct||Array) ? structSpan(c.id, childMap, visited) : c.byteSize()`; `end = c.offset + sz` (int64); `if (end > maxEnd) maxEnd = min(end, INT_MAX)`.
7. **Embedded struct ref:** if `kids.isEmpty() && kind==Struct && refId != 0` → `maxEnd = max(maxEnd, structSpan(refId, childMap, visited))`.
8. Return `max(declaredSize, maxEnd)`.

**Tests** (`test_core.cpp`): struct{UInt32@0, UInt64@4} → 12; nested inner(UInt64@0)=8, outer=8; empty struct=0; primitive array[16] UInt32 = 64; struct{array[10] UInt64 @8} = 88; leaf UInt64 short-circuits to 8; static field at offset 1000 excluded (span stays 4); cycle (B refId→A) terminates ≥0; static excluded again. **Subtle:** the `visited` set means a struct referenced twice in the same recursion yields 0 the second time — important for cycle safety, can slightly under-count diamond references (matches C++).

### 6.17 `normalizePreferAncestors` / `normalizePreferDescendants` (declared `core.h:790-791`, defined `compose.cpp:1747-1784`)

Batch-selection normalizers operating on `QSet<uint64_t>`:

**`normalizePreferAncestors(ids)`** — drop any node that has a *selected ancestor*. For each id: walk `parentId` chain (cycle-safe `visited`); if any ancestor is in `ids` → exclude this id. Keeps only the topmost selected nodes. **Tests:** select root+leaf → {root}; A+leaf → {A}; root+A → {root}; leaf alone → {leaf}.

**`normalizePreferDescendants(ids)`** — drop any node that has a *selected descendant*. For each id: `subtreeIndices(id)`; if any descendant (id != self) is in `ids` → exclude. Keeps only the deepest selected nodes. **Tests:** root+a+b → {a,b}; root+a → {a}; root alone → {root}.

### 6.18 `NodeTree::toJson() -> QJsonObject` (`core.h:793-812`)

- `baseAddress` = `QString::number(baseAddress, 16)` (**hex string, no "0x"**).
- `baseAddressFormula` — only if non-empty.
- `initialClass` — only if non-empty.
- `pointerSize` — only if **!= 8** (int).
- `nextId` = `QString::number(m_nextId)` (decimal string).
- `nodes` = array of `Node::toJson()`.
- `bookmarks` — only if non-empty; array of `Bookmark::toJson()`.

### 6.19 `static NodeTree::fromJson(const QJsonObject& o) -> NodeTree` (`core.h:814-833`)

- `baseAddress` = `o["baseAddress"].toString("400000").toULongLong(nullptr, 16)` (**hex parse, default "400000"**).
- `baseAddressFormula`, `initialClass` = `.toString()`.
- `pointerSize` = `.toInt(8)`.
- `m_nextId` = `o["nextId"].toString("1").toULongLong()` (default "1").
- `nodes`: for each, `Node::fromJson`; **after appending, `if (n.id >= m_nextId) m_nextId = n.id + 1`** (m_nextId always stays ahead of max id even if `nextId` field was stale).
- `bookmarks`: for each, `Bookmark::fromJson`.

**Tests:** baseAddress round-trips (0xDEAD, 0x7FF600000000); pointerSize 4 round-trips; `m_nextId >= 3` after two nodes; node ids preserved.

**Rust serialization note:** Use `serde_json`. Match exactly: 64-bit ids/refId/parentId/nextId/enum-values as **strings**; baseAddress as **hex string without 0x**; offset/arrayLen/strLen/ptrDepth/bitOffset/bitWidth as **numbers**; booleans omitted when false/default; `collapsed` always loads true; conditional-omit rules per §3.4. `viewIndex` never serialized.

### 6.20 Free function `rootClassNames(const NodeTree& tree) -> QStringList` (`core.h:853-863`)

Enumerate display names of every **root-level Struct** (`parentId==0 && kind==Struct`). Name = `structTypeName.isEmpty() ? name : structTypeName`; if still empty → "Untitled". **Dedups** by name (skips already-present). If result empty → `["Untitled"]`. **Tests:** multi-class lists both, nested struct excluded; prefers structTypeName over name; empty tree → ["Untitled"]; dedups identical names; non-Struct roots skipped.

---

## 7. `struct ValueHistory` (`core.h:867-923`) — heatmap ring buffer

`static constexpr int kCapacity = 10`. Fields: `std::array<QString,10> values; std::array<qint64,10> timestamps{}; int count=0; int head=0;` (`count`=total unique recorded, `head`=next write slot).

- `record(const QString& v)` (`core.h:874-883`): if `count>0` and last written value (`values[(head+9)%10]`) equals `v` → **no-op (dedup consecutive)**. Else write `values[head]=v`, `timestamps[head]=QDateTime::currentMSecsSinceEpoch()`, `head=(head+1)%10`, `if (count<INT_MAX) count++`.
- `clear()`: `count=0; head=0` (does not wipe array contents).
- `uniqueCount()` → `min(count, 10)`.
- `heatLevel()` (`core.h:893-898`): `count<=1 → 0` (static); `count==2 → 1` (cold); `count<=4 → 2` (warm); else `3` (hot). **Note this is keyed on total `count`, not uniqueCount** — oscillating A/B/A/B gives count=4 → warm.
- `last()` → `count==0 ? "" : values[(head+9)%10]`.
- `forEach(Fn)` (`core.h:906-912`): iterate **oldest→newest** over `uniqueCount()` entries; `start = (head+10-n)%10`.
- `forEachWithTime(Fn)` (`core.h:915-922`): iterate **newest→oldest** with timestamps.

**Tests:** empty → heat 0/last ""; single → heat 0; consecutive dup not counted (count stays 1); heat 1/2/3 at 2/3/5 uniques (4 → warm); ring wraps at 15 records (count 15, uniqueCount 10, last "14", oldest surviving "5"); oscillation A/B/A/B → count 4, warm.

**Rust:** `[String; 10]` + `[i64; 10]`; `QDateTime::currentMSecsSinceEpoch()` → `chrono`/`std::time` ms-since-epoch (or inject a clock for testability).

---

## 8. Line/render metadata types (display model)

These describe composed lines; the *compose* subsystem produces them, but the structs and the column-span helpers live in core.h and are part of the model contract. Listed for completeness; the geometry helpers are pure and testable.

### 8.1 `enum class LineKind : uint8_t` (`core.h:927-931`)
`CommandRow, Blank(unused), Header, Field, Continuation, Footer, ArrayElementSeparator`.

### 8.2 Selection-id encoding constants (`core.h:933-961`)
- `kCommandRowId = UINT64_MAX`, `kCommandRowLine = 0`, `kFirstDataLine = 1`.
- `kFooterIdBit = 0x8000000000000000`.
- **Array element selection:** `kArrayElemBit = 0x4000000000000000`, `kArrayElemShift = 42`, `kArrayElemMask = 0x3FFFFC0000000000` (20 bits, max 1048575 elements). `static_assert kMaxArrayLen <= (1<<20)`.
  - `makeArrayElemSelId(nodeId, elemIdx) = nodeId | kArrayElemBit | ((elemIdx & 0xFFFFF) << 42)` (asserts elemIdx≥0).
  - `arrayElemIdxFromSelId(selId) = (selId & kArrayElemMask) >> 42`.
- **Member selection (enum/bitfield):** `kMemberBit = 0x2000000000000000`, `kMemberSubShift = 42`, `kMemberSubMask = 0x3FFFFC0000000000`. Same make/extract pattern via `makeMemberSelId`/`memberSubFromSelId`.

**Rust:** plain `u64` bit-twiddling consts/functions. Note array-elem and member masks/shift are identical (42, 20-bit) but distinguished by their respective tag bits.

### 8.3 `enum class ChipKind : uint8_t` (`core.h:971-980`)
`Enum=0, TypeHint, Rtti, Symbol, Comment, AddComment`. Render order: Enum, TypeHint, Rtti, Comment, AddComment (Symbol slots in too). AddComment only when no Comment exists + focus row.

### 8.4 `struct LineChip` (`core.h:982-992`)
`ChipKind kind; int startCol=-1; int endCol=-1; QString text;` plus per-kind payloads: `uint64_t rttiVtableAddr=0` (Rtti); `QVector<NodeKind> typeHintKinds` (TypeHint); `int64_t enumCurrentValue=0` and `uint64_t enumRefNodeId=0` (Enum). `text` already includes the prefix glyph.

### 8.5 `struct LineMeta` (`core.h:994-1037`)
Large per-line record (35+ fields): `nodeIdx`, `nodeId`, `subLine`, `depth`, `foldLevel`, `foldHead`, `foldCollapsed`, `isContinuation`, `isRootHeader`, `isArrayHeader`, `lineKind`, `nodeKind`, `elementKind`, `arrayViewIdx`, `arrayCount`, `arrayElementIdx`, `offsetText`, `offsetAddr`, `ptrBase`, `markerMask`, `dataChanged`, `heatLevel`, `changedByteIndices(QVector<int>)`, `lineByteCount`, `effectiveTypeW(=14)`, `effectiveNameW(=22)`, `pointerTargetName`, `isArrayElement`, `isMemberLine`, `isStaticLine`, `braceCol(=-1)`, `parentAddr`, and `QVector<LineChip> chips`. Helpers: `findChip(lm, kind)` (linear, first match or nullptr), `isSyntheticLine(lm)` (`lineKind==CommandRow`).

### 8.6 `struct LayoutInfo` (`core.h:1054-1060`) and `struct ComposeResult` (`core.h:1064-1076`)
`LayoutInfo{ typeW=14, nameW=22, offsetHexDigits=8, baseAddress=0, treeLines=false }`. `ComposeResult{ QString text; QVector<LineMeta> meta; LayoutInfo layout; int maxLineLen=0; QVector<int> lineStarts; }`.

### 8.7 `enum Marker : int` (`core.h:182-193`)
`M_CONT=0, M_PTR0=2, M_CYCLE=3, M_ERR=4, M_STRUCT_BG=5, M_HOVER=6, M_SELECTED=7, M_CMD_ROW=8, M_ACCENT=9, M_FOCUS=10`. (Note 1 is unused.) Bit indices for `markerMask`.

### 8.8 Column constants (`core.h:1130-1148`)
`kFoldCol=3, kTreeIndent=2, kColType=14, kColName=22, kColValue=96, kColComment=28, kColBaseAddr=12, kSepWidth=1, kMinTypeW=9, kMaxTypeW=128, kMinNameW=10, kMaxNameW=128, kCompactTypeW=20, kDefaultRefreshMs=200`.

### 8.9 `struct LineGeometry` (`core.h:1155-1181`) and column-span helpers (`core.h:1183-1452`)
`LineGeometry{ prefixWidth=kFoldCol, indentWidth=0, typeColumnWidth=kColType, nameColumnWidth=kColName }` with `typeStart()`, `nameStart()`, `valueStart()`, `documentColumn(contentCol)`, and `static forLine(lm)` (flush-left for CommandRow / root Footer → prefixWidth 0).

The many `*SpanFor(...) -> ColumnSpan` functions (`typeSpanFor`, `nameSpanFor`, `valueSpanFor`, `commentSpanFor`, `memberNameSpanFor`, `memberValueSpanFor`, `staticExprSpanFor`, `commandRow*Span`, `arrayElem*Span`, `pointer*Span`, `array{Prev,Index,Count,Next}SpanFor`) compute `ColumnSpan{ int start; int end; bool valid; }` for inline editing/hit-testing. These are pure string/column math. **Tests** assert exact columns: depth-1 Field → type span `[5,19)`, name `[20,42)`, value `[43, 43+96)`; depth-0 Field → type `[3,17)`, name `[18,40)`, value `[41, 41+96)`; continuation only has a value span; Header/Footer have none; `staticExprSpanFor` extracts the expr between "return " and "→". `EditTarget` enum (`core.h:1125-1127`) lists editable regions.

These belong primarily to the *format/compose* subsystem but are declared here; the Rust port can keep them with the model or move them next to compose. They are **pure** and well-covered by `test_core.cpp`'s `testColumnSpan_*`.

---

## 9. Undo/redo command model (`core.h:1080-1115`)

`namespace cmd` defines POD command structs, combined into `using Command = std::variant<...>`:

| struct | fields |
|---|---|
| `OffsetAdj` | `uint64_t nodeId; int oldOffset, newOffset;` (used inside other commands) |
| `ChangeKind` | `nodeId; NodeKind oldKind, newKind; QVector<OffsetAdj> offAdjs;` |
| `Rename` | `nodeId; QString oldName, newName;` |
| `Collapse` | `nodeId; bool oldState, newState;` |
| `Insert` | `Node node; QVector<OffsetAdj> offAdjs;` |
| `Remove` | `nodeId; QVector<Node> subtree; QVector<OffsetAdj> offAdjs;` |
| `ChangeBase` | `uint64_t oldBase, newBase; QString oldFormula, newFormula;` |
| `WriteBytes` | `uint64_t addr; QByteArray oldBytes, newBytes;` |
| `ChangeArrayMeta` | `nodeId; NodeKind oldElementKind, newElementKind; int oldArrayLen, newArrayLen;` |
| `ChangePointerRef` | `nodeId; uint64_t oldRefId, newRefId;` |
| `ChangeStructTypeName` | `nodeId; QString oldName, newName;` |
| `ChangeClassKeyword` | `nodeId; QString oldKeyword, newKeyword;` |
| `ChangeOffset` | `nodeId; int oldOffset, newOffset;` |
| `ChangeEnumMembers` | `nodeId; QVector<QPair<QString,int64_t>> oldMembers, newMembers;` |
| `ChangeOffsetExpr` | `nodeId; QString oldExpr, newExpr;` |
| `ToggleStatic` | `nodeId; bool oldVal, newVal;` |
| `ToggleRelative` | `nodeId; bool oldVal, newVal;` |
| `ToggleBigEndian` | `nodeId; bool oldVal, newVal;` |
| `ChangeComment` | `nodeId; QString oldComment, newComment;` |

Each carries both old and new values for reversibility. `WriteBytes` uses `QByteArray` (→ `Vec<u8>`). **Rust:** `enum Command { ChangeKind{..}, Rename{..}, ... }` (tagged union = `std::variant`). The *application* of these commands lives in the controller subsystem (out of this file), but the data shapes are defined here.

### `struct ViewState` (`core.h:1456-1468`)
`int scrollLine=0, cursorLine=0, cursorCol=0, xOffset=0; uint64_t cursorNodeId=0; int cursorSubLine=0;` — caret anchored by node id so refreshes that shift line counts re-land on the same node; falls back to (cursorLine, cursorCol) when `cursorNodeId==0`.

---

## 10. `fmt::` / `compose` forward declarations (`core.h:1472-1534`)

Declared here, **defined in format.cpp/compose.cpp** (separate subsystems). Signatures the model exposes:
- `fmt::TypeNameFn = QString(*)(NodeKind)`, `setTypeNameProvider(fn)` — pluggable type-name override.
- Formatters: `typeName`, `typeNameRaw`, `fmtIntN/UIntN/Float/Double/Bool/Pointer32/64`, `fmtNodeLine`, `fmtOffsetMargin`, `fmtStructHeader/Footer`, `fmtArrayHeader`, `structTypeName`, `arrayTypeName`, `pointerTypeName`, `fmtPointerHeader`, `validateBaseAddress`, `indent`, `readValue`, `editableValue`, `parseValue` (two overloads + `parseAsciiValue`), `validateValue`, `fmtEnumMember`, `fmtBitfieldMember`, `extractBits`.
- `SymbolLookupFn = std::function<QString(uint64_t)>`.
- `compose(const NodeTree&, const Provider&, uint64_t viewRootId=0, bool compactColumns=false, treeLines=false, braceWrap=false, typeHints=false, showComments=true, SymbolLookupFn={}, showRtti=true, showEnumChips=true) -> ComposeResult`.

These read the live target via the abstract `Provider&` (out-of-scope plugins implement Provider; built-in providers are file/buffer/snapshot/null). **For the core-model port, treat these as the boundary** — the node model never calls a provider directly; only compose/format do.

---

## 11. `commontypes.h` — predefined struct templates

### 11.1 `struct CommonField` (`commontypes.h:11-17`)
`int offset; NodeKind kind; const char* name; const char* ptrTarget=nullptr;` (ptrTarget = pointer field's target type name; nullptr/empty = void*).

### 11.2 `struct CommonType` (`commontypes.h:19-26`)
`const char* name; const char* category; const char* classKeyword; int totalSize; const CommonField* fields; int fieldCount;`.

### 11.3 The table `kCommonTypes[]` (`commontypes.h:332-390`)
~50 predefined types built via the `CT(name, cat, kw, size, fieldsArray)` macro (`commontypes.h:330`), which auto-counts fields with `std::size`. Categories and members are *data*, not behavior. Full inventory (name | category | keyword | size bytes):

**Windows NT:** `_M128A`(struct,16), `UNICODE_STRING`(struct,16; Buffer is Pointer64→"UTF16"), `LIST_ENTRY`(struct,16), `LARGE_INTEGER`(union,8), `OBJECT_ATTRIBUTES`(struct,48; ObjectName→"UNICODE_STRING"), `CLIENT_ID`(struct,16), `IO_STATUS_BLOCK`(struct,16), `GUID`(struct,16), `FILETIME`(struct,8), `FILETIME_u64`(union,8), `RTL_BALANCED_NODE`(struct,24), `SINGLE_LIST_ENTRY`(struct,8), `STRING`(struct,16), `DISPATCHER_HEADER`(struct,24).

**Time:** `UnixTime32`(struct,4), `UnixTime64`(struct,8).

**C++ STL (MSVC x64):** `std::string`(class,32), `std::wstring`(class,32, shares `kFields_std_string`), `std::vector`(class,24), `std::shared_ptr`(class,16), `std::unique_ptr`(class,8), `std::function`(class,48), `std::map_node`(struct,48), `std::unordered_map`(class,56).

**Unreal:** `FString`(struct,16), `FName`(struct,8), `TArray`(struct,16, shares `kFields_FString`), `FVector`(struct,12), `FRotator`(struct,12), `FTransform`(struct,48; three Vec4), `FQuat`(struct,16), `FLinearColor`(struct,16).

**Generic:** `VTable8`(struct,64; 8× FuncPtr64), `RefCounted`(class,16), `LinkedNode`(struct,24), `TreeNode`(struct,32), `SlabEntry`(struct,24), `Delegate`(struct,16), `Variant`(struct,24), `Slice`(struct,16), `FatPointer`(struct,16), `TimeStamp`(struct,8).

**Math:** `RGBA8`(struct,4), `AABB`(struct,24; two Vec3), `Matrix4x4`(struct,64; one Mat4x4), `Sphere`(struct,16), `Ray`(struct,24), `Plane`(struct,16).

`static constexpr int kCommonTypeCount = std::size(kCommonTypes)` (`commontypes.h:394`).

### 11.4 `const CommonType* findCommonType(const QString& name)` (`commontypes.h:397-403`)
Linear scan; `name == QLatin1String(kCommonTypes[i].name)` (exact, case-sensitive) → pointer, else **nullptr**.

**Rust:** a `&'static [CommonType]` table (or `phf`/`OnceLock<HashMap>`); `CommonField` uses `&'static str` names/targets. `find_common_type(&str) -> Option<&'static CommonType>`. Several types intentionally share a field array (wstring↔string, TArray↔FString) — fine in Rust as shared slices. Note `_padding`/`_pad` fields are real fields in the templates (so instantiation produces exact byte layouts, not blank padding). When the user picks a CommonType, the editor creates a Struct with these exact `CommonField`s rather than generic hex64 fill.

---

## 12. `typeinfer.h` — heuristic type inference engine

Header-only, pure (no Qt beyond `QString`/`QVector`). Scores raw bytes into candidate `NodeKind`s. Used to render "type hint chips" (`ChipKind::TypeHint`).

### 12.1 Public types
- `struct InferHints` (`typeinfer.h:13-20`): `const uint8_t* minObserved=nullptr; const uint8_t* maxObserved=nullptr; bool monotonic=false; bool neverChanged=false; int sampleCount=0; int ptrSize=8;`. minObserved/maxObserved are raw byte buffers same length as `data` (value history min/max).
- `struct TypeSuggestion` (`typeinfer.h:24-28`): `QVector<NodeKind> kinds;` (size 1 = convert whole; >1 = uniform split, all same kind), `int score=0` (0-100), `int strength=0` (0=hidden,1=weak,2=moderate,3=strong).

### 12.2 Public API
- `QVector<TypeSuggestion> inferTypes(const uint8_t* data, int len, const InferHints& hints={}, int maxResults=3)` — entry point.
- `inline QString formatHint(const TypeSuggestion& s)` (`typeinfer.h:38-44`): empty kinds → ""; size 1 → `kindMeta(kinds[0])->typeName`; size >1 → `"<typeName>×<count>"` using `×` (multiplication sign). E.g. "float×2".

### 12.3 `inferTypes` algorithm (`typeinfer.h:524-548`)
1. `!data || len <= 0` → empty. `allZero(data,len)` → empty (NULL skipped entirely).
2. Reserve 12 candidates.
3. Whole-width candidates by length: `len>=8 → tryWhole8`; `len==4 → tryWhole4`; `len==2 → tryWhole2`; `len==1 → tryWhole1`. (Note: 8 uses tryWhole8 but 4/2/1 are exact-length only; a len-8 buffer does NOT also try whole4.)
4. `len>=4 → trySplitUniform(data,len,...)`.
5. Return `pruneAndRank(cands, maxResults)`.

### 12.4 Helpers (`detail` namespace)
- `loadU16/U32/U64`, `loadF32/F64`: `memcpy` (host endianness; inference assumes little-endian x86/x64 data — this is intrinsic, the model elsewhere has `bigEndian` flag but inference uses native loads).
- `allZero(p,n)`, `popcount32(v)` (`__builtin_popcount` or fallback loop), `isPrintable(c)` = `0x20..0x7E`.
- `isGoodFloat(bits)`: reject inf/nan (exp==0xFF), reject denormal (exp==0 && mantissa!=0); accept if `f==0` or `|f| in [1e-6, 1e7]`.

### 12.5 Feature checkers (each returns `FeatureResult{int passed; int checked}`)
- `countFloatFeatures` (`typeinfer.h:97-131`): 4 base features (finite; non-denormal; range [1e-6,1e7] or 0; has fractional part >0.0001). +4 with history (min/max good floats; field changes; range<1e6).
- `countIntFeatures` (`typeinfer.h:135-163`): **hard reject `val==0 || val==0xFFFFFFFF` → {0,3}**. Else 3 base (always-pass-1; small abs ≤1e6 or signed within ±1e6; fits int16 ±32767). +3 with history (min/max ≤1e6; monotonic; varies).
- `countFlagFeatures` (`typeinfer.h:167-189`): 2 base (1-3 bits set; not a small power-of-two/≤256). +3 with history (XOR popcount ≤4; varies; max superset of min).
- `countPtrFeatures64` (`typeinfer.h:193-236`): hard rejects → {0,5}: 0, all-FF, 0x00000000FFFFFFFF; **non-canonical x64** (`>0x00007FFFFFFFFFFF && <0xFFFF800000000000`); packed-two-int32 sentinels (low==0xFFFFFFFF; high==0xFFFFFFFF; low 1MB-aligned & high<0x10000). Else 5 features (8-aligned; ≥0x10000; upper32!=0; >4GB; user-mode <0xFFFF800000000000).
- `countPtrFeatures32` (`typeinfer.h:238-247`): 3 (non-zero & not 0xFFFFFFFF; 4-aligned; ≥0x10000).
- `countStringFeatures(data,len)` (`typeinfer.h:251-272`): `len<2 → {0,4}`. Computes printable ratio, letters, max consecutive printable run; 4 features (maxConsec≥4; ratio>0.75; letters≥1; ratio>0.90).
- `countInt16Features` (`typeinfer.h:276-291`): 2 base (non-zero; signed within ±16384). +2 with history (min/max ≤4096; varies).

### 12.6 Score & strength
- `featureScore(r)` = `r.checked==0 ? 0 : (r.passed*100)/r.checked` (integer division).
- `strengthFromScore(score)` (`typeinfer.h:300-309`): **≥85 → 3 (strong)**, ≥50 → 2, ≥25 → 1, else 0. (Comment notes 85 was raised from 75 to reduce chip flicker; compose only emits chips at strength≥3.)

### 12.7 Candidate accumulation
- `struct Candidate { QVector<NodeKind> kinds; int score; }`.
- `addCandidate(out, k, score)`: push `{{k},score}` only if **score≥25**.
- `addSplitCandidate(out, k, count, score)`: push `{count copies of k, score}` only if score≥25.

### 12.8 Whole-width tries
- `tryWhole8` (`typeinfer.h:331-377`): Pointer64 (if ptrSize==8); Double (with hard rejects: out of [1e-6,1e7] range, or lower-32-zero+nonzero-mantissa "split field"; then 4 features); UTF8 (string features on 8 bytes); UInt64 (only if upper 32 bits nonzero; 3 features: always-1; <0x0000FFFFFFFFFFFF; monotonic or page-aligned).
- `tryWhole4` (`typeinfer.h:379-398`): Float; Int32; UInt32 (int features); UInt32 again via flag features; Pointer32 (if ptrSize==4). **Note both Int32 and UInt32 use the same `countIntFeatures` → identical scores; dedup happens later. Flags also map to UInt32 kind.**
- `tryWhole2` (`typeinfer.h:400-406`): Int16 and UInt16 with same `countInt16Features` score.
- `tryWhole1` (`typeinfer.h:408-412`): UInt8 with score 50 if value∈{0,1} else 25.

### 12.9 `trySplitUniform` (`typeinfer.h:416-477`)
- `len==8`: Float×2, Int32×2, UInt32×2 — each only if at least one half nonzero; score = `min(featureScore(halfA), featureScore(halfB))`; zero halves get a fixed neutral `{2,4}` (float) / `{1,3}` (int) result. Float×2 also requires both halves be good floats (or zero).
- `halfLen==2` (len 4 or 8): Int16×count and UInt16×count (count=len/2); score = min over parts; only if any part nonzero.

### 12.10 `pruneAndRank(cands, maxResults)` (`typeinfer.h:481-518`)
1. Stable-ish sort **descending by score** (`std::sort` with `a.score > b.score`).
2. **Dedup** by identical `kinds` vector (keep first = highest score). O(n²) compare.
3. **Dominance:** if `deduped.size()>=2 && top.score >= second.score * 3/2 && (top - second) >= 10` → keep only top (resize 1). Else if `size > maxResults` → resize to maxResults.
4. Build result: for each candidate, `str = strengthFromScore(score)`; **only push if `str > 0`** (drops weak score<25, though addCandidate already gated at 25 → strength≥1). Returns `QVector<TypeSuggestion>`.

**Tests** (`test_typeinfer.cpp`): null/zero-len/all-zero → empty (for len 8/4/2). `{21.0488f,547.3f}` → top is Float×2 (size 2, kinds[0]==Float), strength≥3. `{42,99}` int pair → top size 2, Int32 or UInt32. `"IChooseY"` → UTF8 present with strength≥3. Windows addr `0x00007FF6A0B01000` → Pointer64 present. `0x41A86600` (21.0488f) → top Float strength≥2. monotonic int 49148 with history → Int32/UInt32 strength≥2. `0x005F` → Int16/UInt16. `1u8` → UInt8. `formatHint` single "float", split "float×2". denormal `0x00000001` → top (if single) NOT Float.

**Rust mapping:** `data: &[u8]`, `InferHints { min_observed: Option<&[u8]>, max_observed: Option<&[u8]>, monotonic, never_changed, sample_count, ptr_size }`. `f32::from_le_bytes`/`u32::from_ne_bytes` — match the C++ `memcpy` (native-endian; on the supported little-endian targets this equals from_le_bytes). `popcount` → `u32::count_ones`. `modf` → manual `f.fract()`/`f.trunc()`. Integer division for score. Keep all magic thresholds bit-exact. `×` is `\u{00D7}`.

---

## 13. Qt → Rust type mapping (this subsystem)

| Qt type | usage here | Rust equivalent |
|---|---|---|
| `QString` | names, comments, formulas, JSON string ids | `String` |
| `QStringList` | `rootClassNames`, `allTypeNamesForUI`, path split | `Vec<String>` |
| `QVector<T>` | `nodes`, `enumMembers`, `chips`, candidates | `Vec<T>` |
| `QHash<K,V>` | `m_idCache`, `m_childCache`, validate `seen`, childMap | `HashMap<K,V>` |
| `QSet<uint64_t>` | cycle-visited sets, selection normalizers | `HashSet<u64>` |
| `QPair<QString,int64_t>` | enum members | `(String, i64)` |
| `QJsonObject`/`QJsonArray` | serialization | `serde_json::Map`/`Value`, or derive `Serialize`/`Deserialize` (replicate string-id & hex-base rules) |
| `QByteArray` | `WriteBytes` cmd | `Vec<u8>` |
| `QDateTime::currentMSecsSinceEpoch()` | ValueHistory timestamps | `std::time`/`chrono` ms (or injected clock) |
| `std::array<T,N>` | ValueHistory ring | `[T; N]` |
| `std::variant<...>` | `Command` | `enum Command { ... }` |
| `qBound(lo,v,hi)` | clamps | `v.clamp(lo,hi)` |
| `qMin`/`qMax` | size math | `min`/`max` (watch i64↔i32 narrowing in structSpan `qMin(end, INT_MAX)`) |
| `QLatin1String`/`QStringLiteral` | literal compares | `&str` / `==` |
| `uint64_t`/`int64_t`/`uint8_t`/`quint64` | ids/offsets/bytes | `u64`/`i64`/`u8`/`u64` |
| `const char*` (KindMeta/CommonType) | static metadata | `&'static str` |
| function pointers `g_*Hook` | GUI decoupling seams | `Option<Box<dyn Fn>>` / injected trait |
| `Provider&` (abstract) | read/write target | `&dyn Provider` trait (built-in: file/buffer/snapshot/null) |

---

## 14. Platform-specific code & concurrency

- **Platform:** only `typeinfer.h:71-77` `popcount32` has a `#if defined(__GNUC__)||__clang__` branch (builtin vs. portable loop) — pure performance, no semantic difference. `core.h:37-39` has a `QT_VERSION < 6` `qHash` overload (dead on Qt6 / irrelevant for Rust). **No OS APIs, no `#ifdef _WIN32` in these three files.** All address-range constants in `typeinfer.h` (canonical x64 user/kernel split) are *data heuristics*, not platform calls — they encode x64 assumptions but compile/run everywhere.
- **Concurrency:** none. No threads, no mutexes, no atomics. The `mutable` caches make `const` methods non-thread-safe for concurrent reads-that-fill (a shared `&NodeTree` would need `&mut`/`RefCell`/recompute in Rust). The model is single-threaded by design; the controller drives it from the GUI thread. Hooks (`g_*Hook`) are set once at startup.

---

## 15. Subtle behaviors / invariants the port MUST preserve (tests depend on these)

1. **NodeKind enum order is load-bearing** — `kindMeta` indexes by `as usize`; the `static_assert` ties table length to `Array+1`=31.
2. **id/parentId/refId/nextId/enum-value serialize as decimal STRINGS; baseAddress as a HEX string without "0x"** — JSON-number precision avoidance. Offsets/lengths/depth/bit fields are JSON numbers.
3. **`collapsed` always loads `true`** regardless of saved value.
4. **`isStatic` falls back to legacy `isHelper` key** on load.
5. **fromJson clamps:** arrayLen [1,1e6], strLen [1,1e6], ptrDepth [0,2], bitOffset [0,255], bitWidth [1,64].
6. **`m_nextId` always stays strictly ahead of every id**, both in `addNode` and at end of `fromJson` (`id>=nextId → nextId=id+1`).
7. **`computeOffset` returns signed i64 and can be negative**; `absoluteAddress` guards the negative case (returns baseAddress, ok=false).
8. **All tree walks are cycle-safe** via visited sets (`depthOf`, `computeOffset`, `fieldPath`, `subtreeIndices`, `structSpan`, `validate`). structSpan returns 0 for an already-visited struct in the same recursion.
9. **`byteSize()` returns 0 for non-bitfield Struct and for Array-of-container**; bitfield Struct returns its container width (default 4). Use `totalByteSize(tree)`/`structSpan` for containers.
10. **structSpan & findOverlaps exclude `isStatic` children**; findOverlaps additionally skips root-level (parentId 0) and union parents and ≤0-span children, and breaks early on the sorted range scan.
11. **OverlapPair order:** lower-offset sibling is `aId`, the later one `bId`; deterministic via sort-by-start then forward scan.
12. **ValueHistory:** consecutive duplicates are ignored; `heatLevel` keys on total `count` (not unique), capacity 10, ring wrap keeps newest 10; `forEach` oldest→newest, `forEachWithTime` newest→oldest.
13. **Type inference thresholds are exact:** addCandidate gate 25; strength bins 25/50/85; dominance rule needs both `top >= 1.5*second` AND `top-second >= 10`; all hard-reject sentinel/range constants must match bit-for-bit; integer division for scores.
14. **`kindFromString` unknown → Hex8; `kindFromTypeName` unknown → (Hex8, ok=false).**
15. **`nodeIdForPath`/`fieldPath` matching is exact & case-sensitive**; first segment matches `structTypeName` OR `name` of a root; later segments match child `name` only.
16. **`rootClassNames`** prefers structTypeName, dedups, "Untitled" fallback, only root Structs.

---

## 16. Recommended Rust crates

- `serde` + `serde_json` — JSON serialization (custom (de)serialization to honor string-ids & hex-base-no-0x and conditional omission; likely hand-written `to_json`/`from_json` over `serde_json::Value` to match exactly rather than derive).
- `bitflags` — `KindFlags` and `Marker` bit constants (optional; plain consts also fine).
- `phf` or `std::sync::OnceLock` — static `kKindMeta` / `kCommonTypes` lookup tables (or just `const` arrays + linear scan to mirror C++).
- No external crate needed for inference math (std `f32/f64`, `count_ones`).
- `chrono` (optional) — only if you don't want raw `std::time` for ValueHistory timestamps; injecting a clock is cleaner for tests.

---

## 17. Public API surface count (this subsystem)

Roughly: kind helpers ~17 functions (`sizeForKind`, `linesForKind`, `alignmentFor`, `kindToString`, `kindFromString`, `kindFromTypeName`, `kindMeta`, `flagsFor`, 8 predicates, `isValidPrimitivePtrTarget`, `allTypeNamesForUI`); `Node` methods 8 (`byteSize`, `totalByteSize`, `toJson`, `fromJson`, `resolvedClassKeyword`, `isUnion`, `isBitfield`, `isEnum`); `NodeTree` methods ~22 (add/reserve/cache/validate/findOverlaps/indexOfId/childrenOf/nodeIdForPath/fieldPath/subtreeIndices/depthOf/computeOffset/absoluteAddress/structSpan/normalize×2/toJson/fromJson/generation/bumpGeneration/touch/invalidateIdCache); `ValueHistory` 7; free funcs `rootClassNames`, sel-id encoders (4), chip/line helpers (`findChip`,`isSyntheticLine`), ~25 column-span helpers; `commontypes` `findCommonType`; `typeinfer` `inferTypes`+`formatHint` (public) + ~20 detail helpers. **~110+ public/inline functions total**; ~20 key types/structs/enums.
