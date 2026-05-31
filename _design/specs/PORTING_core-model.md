# PORTING SPEC — Core node/type model (`core-model`)

**Drives:** the faithful Rust implementation of the `core` module (the foundation of
the entire port; milestone #1 in `ARCHITECTURE.md §9`).

**C++ sources ported (read in full):** `src/core.h` (1537 lines), `src/commontypes.h`
(406 lines), `src/typeinfer.h` (551 lines), plus the two out-of-line `NodeTree`
methods in `src/compose.cpp:1747-1784` (`normalizePreferAncestors/Descendants`).

**Behavioral oracle:** `tests/test_core.cpp` (93 asserts, all PASS — `_oracle/RESULTS.md`)
and `tests/test_typeinfer.cpp` (20 asserts, all PASS). Both link only the pure-logic
files (`test_typeinfer` is header-only). There are **no golden text-dump fixtures**
for this subsystem — every assertion is an in-process value comparison, so the Rust
`#[test]`s are direct transliterations of the `QCOMPARE`/`QVERIFY` lines, not
file diffs (unlike `generator`/`format`/`compose`).

> **Convention note.** Everything here lives in `rcx::` in C++. In Rust it lives under
> the crate root (the package is named `reclass`, but the C++ namespace `rcx` is not
> reproduced as a Rust module prefix — the crate *is* `rcx`). All items below are
> public API of the `core` module unless marked `pub(crate)`/private.

---

## 0. Target Rust crate / module layout

Single package `reclass` (per `ARCHITECTURE.md §2`). This subsystem becomes the
`core` module directory:

```
src/core/
├─ mod.rs            # re-exports; module wiring
├─ kind.rs           # NodeKind, KindFlags, KindMeta + kKindMeta table + all kind helpers (§2)
├─ node.rs           # BitfieldMember, Node (§3), Bookmark (§4)
├─ tree.rs           # NodeTree + ValidateReport + OverlapPair (§6), rootClassNames (§6.20)
├─ history.rs        # ValueHistory (§7)
├─ line.rs           # LineKind, ChipKind, LineChip, LineMeta, LayoutInfo, ComposeResult,
│                    #   Marker, LineGeometry, ColumnSpan, EditTarget, sel-id encoders,
│                    #   column-span helpers, ViewState, column constants (§8)
├─ command.rs        # cmd::* structs → enum Command + OffsetAdj (§9)
├─ commontypes.rs    # CommonField, CommonType, COMMON_TYPES table, find_common_type (§11)
├─ typeinfer.rs      # InferHints, TypeSuggestion, infer_types, format_hint + detail fns (§12)
├─ json.rs           # hand-written serde glue (string-id / hex-base rules) (§5 of error strategy)
└─ hooks.rs          # the three g_*Hook decoupling seams (§5)
```

Cargo deps used by `core` (from `crate_selection.md`): `serde` + `serde_json`
(JSON), `ahash` (fast `HashMap` hasher for the caches), `indexmap` (not strictly
needed here — order is preserved by `Vec` + manual dedup; keep optional),
`bytemuck` (only `typeinfer` byte loads — `from_le_bytes` is enough, see §12),
`thiserror` (the one error enum). `bitflags` is **optional**; plain `u32` consts
mirror the C++ `enum KindFlags` more literally and are recommended.

The `fmt::`/`compose` forward declarations in `core.h:1470-1534` are **NOT** part of
this module — they are the boundary to the `format`/`compose` modules. We port only
the *types* they share (`LineMeta`, `ComposeResult`, `LayoutInfo`, column helpers,
`SymbolLookupFn` alias). `SymbolLookupFn` becomes `type SymbolLookupFn = Box<dyn Fn(u64) -> String>` (or a generic closure param on `compose`); declared in `compose.rs`, not here.

---

## 1. Top-level design decisions (apply everywhere)

| Topic | Decision |
|---|---|
| Integer types | `uint64_t`→`u64`, `int64_t`→`i64`, `int`→`i32` (the C++ uses `int` for offsets/sizes deliberately — keep `i32` so overflow/clamp math matches; do NOT widen to `usize`/`i64` except where C++ already uses `int64_t`), `uint8_t`→`u8`, `quint64`→`u64`. |
| Strings | `QString`→`String`; literals compared with `==` against `&str`. |
| Containers | `QVector<T>`→`Vec<T>`; `QHash`→`HashMap` (use `ahash::AHashMap` alias for hot caches; std `HashMap` fine for `validate`'s local maps); `QSet<u64>`→`HashSet<u64>`. |
| `QPair<A,B>` | `(A, B)` tuple. |
| Node identity | `NodeTree.nodes` is `Vec<Node>`; **index ≠ id**. `indexOfId` returns `i32` `-1` sentinel in C++ — port as `fn index_of_id(&self, id: u64) -> i32` returning `-1`, OR `Option<usize>`. **Decision: keep the `-1`/`i32` contract internally** (many call sites do `if idx < 0`), expose an `Option` wrapper only if convenient. Tests assert `indexOfId(99) == -1`. |
| Interior mutability | `m_idCache`/`m_childCache` are `mutable` and filled inside `const` methods. Port as `RefCell<HashMap<...>>`. **Crucially**, methods that fill them are `&self` (e.g. `index_of_id`, `children_of`, `field_path`, `subtree_indices`). `add_node`/`validate`/`invalidate_id_cache` are `&self` in C++ but mutate via the `mutable` members + the `nodes` vector — in Rust `add_node`/`validate` take `&mut self` (they mutate `nodes`), while the lazy read methods take `&self` and use `RefCell`. |
| `static constexpr` tables | `const`/`static` arrays of plain structs with `&'static str` fields. |
| Enum discriminants | `NodeKind`, `LineKind`, `ChipKind` get explicit `#[repr(u8)]` and explicit discriminants where the value is load-bearing (`NodeKind` is — it indexes the meta table). |
| Float math in typeinfer | Use `f32`/`f64` std ops; replicate integer division `(passed*100)/checked`. See §12. |

---

## 2. `kind.rs` — `NodeKind`, flags, `KindMeta` table, kind helpers

### 2.1 `NodeKind` (core.h:24-34)

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum NodeKind {
    Hex8 = 0, Hex16, Hex32, Hex64, Hex128,        // 0-4
    Int8, Int16, Int32, Int64, Int128,            // 5-9
    UInt8, UInt16, UInt32, UInt64, UInt128,       // 10-14
    Float16, Float, Double, Bool,                 // 15-18
    Pointer32, Pointer64,                         // 19-20
    FuncPtr32, FuncPtr64,                         // 21-22
    Vec2, Vec3, Vec4, Mat4x4,                     // 23-26
    Utf8, Utf16,                                  // 27-28  (C++: UTF8/UTF16)
    Struct, Array,                                // 29-30
}
```
**The discriminant order is load-bearing** (invariant #1): `kind_meta(k)` indexes
`K_KIND_META[k as usize]`, and several predicates use `>=`/`<=` ordering
(`is_hex_node` = `Hex8 <= k <= Hex128`). Implement ordering via `(k as u8)` compares,
NOT `#[derive(PartialOrd)]` (the enum is not declared in C++-ordering-as-Ord; using
`as u8` is unambiguous and matches `k >= NodeKind::Hex8`). `Default` = `Hex8`.

Provide `impl NodeKind { pub fn from_index(i: u8) -> Option<NodeKind> }` for the
`testKindMetaCompleteness` loop `for i in 0..=Array`. The Qt<6 `qHash` overload
(core.h:37-39) is **dropped** (Rust `Hash` is derived).

### 2.2 `KindFlags` (core.h:44-50)

```rust
pub const KF_NONE: u32        = 0;
pub const KF_HEX_PREVIEW: u32 = 1 << 0;
pub const KF_CONTAINER: u32   = 1 << 1;
pub const KF_STRING: u32      = 1 << 2;
pub const KF_VECTOR: u32      = 1 << 3;
```
(Plain `u32` consts mirror the C++ `enum KindFlags : uint32_t`. `bitflags!` is fine
too but adds a type — not needed since `KindMeta::flags` is a raw `u32` bitmask.)

### 2.3 `KindMeta` + table (core.h:54-100)

```rust
pub struct KindMeta {
    pub kind: NodeKind,
    pub name: &'static str,      // UI/JSON name: "Hex64"
    pub type_name: &'static str, // display/C name: "hex64", "uint16_t"
    pub size: i32,               // 0 for Struct/Array
    pub lines: i32,
    pub align: i32,
    pub flags: u32,
}

pub const K_KIND_META: [KindMeta; 31] = [ /* 31 rows, EXACTLY as core.h:66-96 */ ];
```
**Transcribe all 31 rows verbatim** from the table in `core-model.md §2.3` (which
matches `core.h` exactly — re-verified). Critical non-obvious values: `Int128`/`UInt128`
size 16 **align 8** (NOT 16), but `Hex128` align **16**; `Mat4x4` size 64 **lines 4**
align 4; `Struct`/`Array` size **0**; `Utf8` type_name `"str"`, `Utf16` `"wstr"`.
Add a `const _: () = assert!(K_KIND_META.len() == NodeKind::Array as usize + 1);`
to mirror the C++ `static_assert` (compile-time check).

### 2.4 Kind free functions (core.h:102-178) — item-by-item

| C++ | Rust signature | Behavior / notes |
|---|---|---|
| `kindMeta(k)` | `pub fn kind_meta(k: NodeKind) -> Option<&'static KindMeta>` | `K_KIND_META.get(k as usize)`. The closed enum makes it always `Some`, but the defensive `None` path mirrors C++ and feeds the defaults below. |
| `sizeForKind(k)` | `pub fn size_for_kind(k) -> i32` | `kind_meta(k).map_or(0, \|m\| m.size)`. |
| `linesForKind(k)` | `pub fn lines_for_kind(k) -> i32` | default **1** on miss. |
| `alignmentFor(k)` | `pub fn alignment_for(k) -> i32` | default **1** on miss. |
| `kindToString(k)` | `pub fn kind_to_string(k) -> &'static str` | default `"Unknown"`. |
| `kindFromString(s)` | `pub fn kind_from_string(s: &str) -> NodeKind` | linear scan over table on `m.name == s`; **miss → `NodeKind::Hex8`** (invariant #14). |
| `kindFromTypeName(s, *ok)` | `pub fn kind_from_type_name(s: &str) -> (NodeKind, bool)` | scan on `m.type_name == s`; hit → `(kind, true)`; **miss → `(Hex8, false)`**. (C++ out-param `bool* ok` → return a tuple.) |
| `flagsFor(k)` | `pub fn flags_for(k) -> u32` | default 0. |
| `isHexNode(k)` | `pub fn is_hex_node(k) -> bool` | `(Hex8..=Hex128).contains` via `k as u8` range `0..=4`. |
| `isHexPreview(k)` | `pub fn is_hex_preview(k) -> bool` | == `is_hex_node`. |
| `isVectorKind(k)` | `pub fn is_vector_kind(k) -> bool` | Vec2/Vec3/Vec4. |
| `isMatrixKind(k)` | `pub fn is_matrix_kind(k) -> bool` | Mat4x4. |
| `isFuncPtr(k)` | `pub fn is_func_ptr(k) -> bool` | FuncPtr32/64. |
| `isPointerKind(k)` | `pub fn is_pointer_kind(k) -> bool` | Pointer32/64. |
| `isContainerKind(k)` | `pub fn is_container_kind(k) -> bool` | Struct/Array. |
| `isStringKind(k)` | `pub fn is_string_kind(k) -> bool` | Utf8/Utf16. |
| `isValidPrimitivePtrTarget(k)` | `pub fn is_valid_primitive_ptr_target(k) -> bool` | false for hex/pointer/funcptr/Struct/Array; true otherwise (core.h:164-170). |
| `allTypeNamesForUI(strip=false)` | `pub fn all_type_names_for_ui() -> Vec<&'static str>` | every `m.type_name` in table order. The `stripBrackets` param is unused in C++ (`/*stripBrackets*/`) — **drop it**. (Tested in test_format: size == 31, no dups.) |

These can all be `const fn` where the C++ is `constexpr` (the predicates and
`size_for_kind` etc.) — nice-to-have, not required.

---

## 3. `node.rs` — `BitfieldMember`, `Node`

### 3.1 `BitfieldMember` (core.h:202-206)
```rust
#[derive(Clone, Default, PartialEq, Debug)]
pub struct BitfieldMember {
    pub name: String,
    pub bit_offset: u8, // default 0; position from LSB
    pub bit_width: u8,  // default 1; 1..=64
}
```

### 3.2 `Node` (core.h:210-363)
```rust
#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    pub id: u64,                                  // 0 = unassigned
    pub kind: NodeKind,                           // default Hex8
    pub name: String,
    pub struct_type_name: String,
    pub class_keyword: String,                    // empty == "struct"
    pub parent_id: u64,                           // 0 = root
    pub offset: i32,
    pub is_static: bool,
    pub offset_expr: String,
    pub is_relative: bool,
    pub array_len: i32,                           // default 1
    pub str_len: i32,                             // default 64
    pub collapsed: bool,                          // default true
    pub ref_id: u64,
    pub element_kind: NodeKind,                   // default UInt8
    pub ptr_depth: i32,                           // 0..=2
    pub view_index: i32,                          // TRANSIENT — never serialized
    pub enum_members: Vec<(String, i64)>,
    pub bitfield_members: Vec<BitfieldMember>,
    pub comment: String,
    pub big_endian: bool,
}
```
`impl Default for Node` MUST set `kind: Hex8, array_len: 1, str_len: 64,
collapsed: true, element_kind: UInt8` and all else zero/empty (cannot `#[derive(Default)]`
because of the non-zero defaults). `kMaxArrayLen` (core.h:198):
`pub const K_MAX_ARRAY_LEN: i32 = 1_000_000;` with
`const _: () = assert!(K_MAX_ARRAY_LEN <= (1 << 20));`.

### 3.3 `Node::byte_size()` (core.h:238-255) — leaf size only
```rust
pub fn byte_size(&self) -> i32 {
    match self.kind {
        NodeKind::Utf8  => self.str_len,
        NodeKind::Utf16 => self.str_len.min(i32::MAX / 2) * 2,
        NodeKind::Array => {
            let elem = size_for_kind(self.element_kind);
            if elem <= 0 { 0 } else { self.array_len.min(i32::MAX / elem) * elem }
        }
        NodeKind::Struct => {
            if self.class_keyword == "bitfield" {
                let sz = size_for_kind(self.element_kind);
                if sz > 0 { sz } else { 4 }
            } else { 0 } // real container size needs the tree
        }
        k => size_for_kind(k),
    }
}
```
Use `i32::MAX` exactly where C++ uses `INT_MAX` (overflow guards). Invariant #9.

### 3.4 `Node::total_byte_size(tree)` (core.h:841-845)
```rust
pub fn total_byte_size(&self, tree: &NodeTree) -> i32 {
    if matches!(self.kind, NodeKind::Struct | NodeKind::Array) {
        tree.struct_span(self.id, None, None)
    } else { self.byte_size() }
}
```
(`struct_span`'s optional `childMap`/`visited` params → `Option` args, see §6.16.)

### 3.5 Class-keyword helpers (core.h:357-362)
```rust
pub fn resolved_class_keyword(&self) -> &str { if self.class_keyword.is_empty() { "struct" } else { &self.class_keyword } }
pub fn is_union(&self) -> bool    { self.resolved_class_keyword() == "union" }
pub fn is_bitfield(&self) -> bool { self.class_keyword == "bitfield" } // raw, not resolved
pub fn is_enum(&self) -> bool     { self.resolved_class_keyword() == "enum" }
```
Note `is_bitfield` checks the **raw** field (empty is NOT bitfield) — tests
`testNodeIsUnionBitfieldEnum` depend on this.

### 3.6 JSON — see §5 (Error/serialization strategy) for `to_json`/`from_json`.

---

## 4. `Bookmark` (core.h:367-382)
```rust
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Bookmark { pub name: String, pub address_formula: String }
```
`to_json`/`from_json`: trivial — both keys **always present** (unlike Node's
conditional fields). Can use plain `#[derive(Serialize, Deserialize)]` with
`#[serde(rename = "addressFormula")]`, since there are no string-id/hex quirks here.

---

## 5. Error-handling & serialization strategy

### 5.1 Errors
The C++ here **never throws and never returns error codes** — it uses defaulting,
clamping, and sentinel returns (`-1`, `0`, `nullptr`). The Rust port mirrors that:
the model API is **infallible** (returns values, `Option`, or sentinels — no
`Result`). The only place a `Result` appears is the *document load* boundary, where
`serde_json` parsing of a malformed `.rcx` can fail. Define one error enum used by
the `core::json` loader (and shared up to `imports`):

```rust
#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    // (room for future variants; the C++ model itself has none)
}
```
`NodeTree::from_json` takes a `&serde_json::Value` (already-parsed) and is
**infallible** like the C++ (`fromJson` reads with defaults, never fails) — the
fallible step is `serde_json::from_str::<Value>` upstream. Keep `from_json` returning
`NodeTree` (not `Result`) to match C++ semantics exactly (missing keys → defaults).

### 5.2 JSON representation — hand-written, NOT derive

The C++ `toJson`/`fromJson` have **exact representational rules that `#[derive(Serialize)]`
cannot reproduce**. Port them by hand over `serde_json::Value`/`Map` in `core/json.rs`.
The non-negotiable rules (invariants #2-#6):

**`Node::to_json` (core.h:263-313) → `serde_json::Value::Object`:**
- Always written: `id` (decimal **string** via `id.to_string()`), `kind` (string name via
  `kind_to_string`), `name`, `parentId` (decimal **string**), `offset` (JSON **number**
  `i32`), `arrayLen` (number), `strLen` (number), `collapsed` (bool), `refId`
  (decimal **string**), `elementKind` (string name).
- Conditional (omit when default/empty):
  - `structTypeName` — only if non-empty.
  - `classKeyword` — only if non-empty **and** `!= "struct"`.
  - `isStatic` — only if `true` (value `true`).
  - `offsetExpr` — only if non-empty.
  - `isRelative` — only if `true`.
  - `ptrDepth` — only if `> 0` (number).
  - `enumMembers` — only if non-empty: array of `{name, value: <decimal string>}`.
  - `bitfieldMembers` — only if non-empty: array of `{name, bitOffset: <number>, bitWidth: <number>}`.
  - `comment` — only if non-empty.
  - `bigEndian` — only if `true`.
- **`viewIndex` is NEVER written.**

**`Node::from_json` (core.h:314-354):** read with these defaults/clamps —
- `id` = `o["id"]` as string, default `"0"`, parse u64 (lenient: see helper below).
- `kind` = `kind_from_string(o["kind"] as str)` (miss → Hex8).
- `name`/`structTypeName`/`classKeyword`/`offsetExpr`/`comment` = string or "".
- `parentId`/`refId` = string default "0" → u64.
- `offset` = int default 0.
- `isStatic` = `o["isStatic"]` bool **falling back to `o["isHelper"]` bool false**
  (legacy key — invariant #4).
- `isRelative`/`bigEndian` = bool default false.
- `arrayLen` = `clamp(o["arrayLen"].toInt(1), 1, K_MAX_ARRAY_LEN)`.
- `strLen` = `clamp(o["strLen"].toInt(64), 1, 1_000_000)`.
- `collapsed` = **always `true`** (ignores stored value — invariant #3).
- `elementKind` = `kind_from_string(o["elementKind"] as str, default "UInt8")`.
- `ptrDepth` = `clamp(o["ptrDepth"].toInt(0), 0, 2)`.
- `enumMembers` (if key present): each `{name (str), value (string default "0" → i64)}`.
- `bitfieldMembers` (if key present): each `{name (str), bitOffset = clamp(int 0, 0, 255) as u8, bitWidth = clamp(int 1, 1, 64) as u8}`.

**`NodeTree::to_json` (core.h:793-812):**
- `baseAddress` = **hex string, no "0x"** (`format!("{:x}", base_address)`).
- `baseAddressFormula` — only if non-empty.
- `initialClass` — only if non-empty.
- `pointerSize` — only if `!= 8` (number).
- `nextId` = decimal **string** (`m_next_id.to_string()`).
- `nodes` = array of `Node::to_json`.
- `bookmarks` — only if non-empty.

**`NodeTree::from_json` (core.h:814-833):**
- `baseAddress` = parse `o["baseAddress"]` as **hex** (radix 16), default `"400000"`.
- `baseAddressFormula`/`initialClass` = string.
- `pointerSize` = int default 8.
- `m_next_id` = string default "1" → u64.
- nodes: for each, `Node::from_json`; **after pushing, `if n.id >= m_next_id { m_next_id = n.id + 1 }`** (invariant #6).
- bookmarks: each `Bookmark::from_json`.

**Lenient integer parsing helper.** The C++ `QJsonValue::toString().toULongLong()`
quietly returns 0 on garbage and accepts decimal. `crate_selection.md` notes the
MCP/`parseInteger` path also accepts `0x` hex. For the *core* model the values are
always emitted as plain decimal strings (ids) or plain hex (baseAddress), so write
two small helpers and use them consistently:
```rust
fn parse_u64_dec(v: &serde_json::Value, default: u64) -> u64; // string→u64 radix 10, fallback default
fn parse_u64_hex(v: &serde_json::Value, default: u64) -> u64; // string→u64 radix 16
fn parse_i64_dec(v: &serde_json::Value, default: i64) -> i64;
fn json_i32(v: &serde_json::Value, default: i32) -> i32;       // number→i32 (Qt toInt)
fn json_bool(v: &serde_json::Value, default: bool) -> bool;
fn json_str(v: &serde_json::Value) -> String;                  // missing/non-string → ""
```
On unparsable strings these return the default (matching `QString::toULongLong`
returning 0 on failure → use `default` only when key is *absent*; on present-but-bad
return 0 to match Qt). **Document this precisely in code** because the round-trip
tests don't probe malformed input, but parity callers (imports) rely on it.

> **Why not derive?** Derive can't do: 64-bit ints as strings, hex-without-0x base,
> "omit when == \"struct\"", "collapsed always loads true", the `isHelper` fallback,
> or skipping `viewIndex`. A `#[serde(...)]`-heavy derive would be more code and still
> wrong on `collapsed`/`isHelper`. Hand-rolling over `serde_json::Value` is the
> faithful choice and is explicitly recommended by `core-model.md §6.19/§16`.

### 5.3 Hooks (`hooks.rs`, core.h:391-404)
The three `extern` C function pointers are **GUI decoupling seams**, left `nullptr`
in the test/core targets. Port as optional injected callbacks held in a small struct
(NOT global mutable statics — Rust discourages that, and tests want them absent):
```rust
#[derive(Default)]
pub struct CoreHooks {
    pub rtti_discovery: Option<Box<dyn Fn(&str, u64, &str)>>,
    pub name_lookup: Option<Box<dyn Fn(u64, /*active*/ &dyn Provider) -> String>>,
    pub names_changed: Option<Box<dyn Fn()>>,
}
```
These are **not exercised by `test_core`/`test_typeinfer`** (always null in tests), so
for milestone #1 they can be defined as the struct above and wired later by
`controller`/`main`. No behavior to port beyond "default = no-op". The `Provider`
reference in `name_lookup` is the abstract trait from the `provider` module
(forward-declared; out of scope for core itself).

---

## 6. `tree.rs` — `NodeTree`

### 6.1 Struct
```rust
pub struct NodeTree {
    pub nodes: Vec<Node>,
    pub base_address: u64,                 // default 0x0040_0000
    pub base_address_formula: String,
    pub pointer_size: i32,                 // default 8
    pub initial_class: String,
    pub bookmarks: Vec<Bookmark>,
    next_id: u64,                          // default 1 (was m_nextId)
    id_cache: RefCell<HashMap<u64, i32>>,  // lazy; i32 value to keep -1 sentinel out (uses .get)
    child_cache: RefCell<HashMap<u64, Vec<usize>>>,
    generation: u64,                       // default 1
}
impl Default for NodeTree // base_address 0x00400000, pointer_size 8, next_id 1, generation 1
```
`next_id`/`generation`/caches are private; expose `next_id()` getter only if a test
needs it. **`testNodeIdJsonRoundTrip` asserts `t2.m_nextId >= 3`** — so expose
`pub fn next_id(&self) -> u64` (read-only). Caches use `RefCell` for interior
mutability in `&self` methods.

> **Cache-invalidation timing matters for parity** (invariant #8). Tests
> `testDepthOfCycle`/`testComputeOffsetCycle` mutate `nodes[i].parent_id` directly
> then call `invalidate_id_cache()`. A clean Rust port MAY choose to rebuild caches
> eagerly, but MUST expose `invalidate_id_cache()` and MUST NOT cache stale
> parent/child data across a documented mutation. Recommended: keep the lazy-fill
> approach 1:1.

### 6.2 Generation (core.h:427-452)
```rust
pub fn generation(&self) -> u64 { self.generation }
pub fn bump_generation(&mut self) { self.generation += 1; }
pub fn touch(&mut self) { self.generation += 1; }
```

### 6.3 `add_node` (core.h:431-443) → `pub fn add_node(&mut self, n: Node) -> usize`
1. `let mut copy = n;`
2. `if copy.id == 0 { copy.id = self.next_id; self.next_id += 1; } else if copy.id >= self.next_id { self.next_id = copy.id + 1; }`
3. `let idx = self.nodes.len(); self.nodes.push(copy);`
4. **Incremental cache update only if cache non-empty**: if `!id_cache.borrow().is_empty()` → insert `(copy_id, idx as i32)`; if `!child_cache.borrow().is_empty()` → append `idx` to `child_cache[copy_parent_id]`.
5. `self.generation += 1;`
6. return `idx`.

Tests: first id == 1, second == 2 (`testStableNodeIds`); first `add_node` returns
index `0` (`testNodeTree_addAndChildren`, `testAddNodeAutoId`).
Note step 4 borrows `copy` *after* `push` moves it — capture `copy.id`/`copy.parent_id`
into locals before the push.

### 6.4 `reserve_id` (core.h:446) → `pub fn reserve_id(&mut self) -> u64`
`let id = self.next_id; self.next_id += 1; id` (monotonic — `testReserveIdMonotonic`).

### 6.5 Cache mgmt (core.h:448-452)
`pub fn invalidate_id_cache(&self)` — clears **both** caches (takes `&self` because
C++ is `const`; uses `RefCell::borrow_mut().clear()` on both).

### 6.6 `validate` (core.h:458-516) → `pub fn validate(&mut self, repair: bool) -> ValidateReport`
```rust
pub struct ValidateReport { pub orphans: i32, pub cycles: i32, pub duplicates: i32 }
impl ValidateReport {
    pub fn summary(&self) -> String { format!("orphans={} cycles={} duplicates={}", self.orphans, self.cycles, self.duplicates) }
    pub fn clean(&self) -> bool { self.orphans == 0 && self.cycles == 0 && self.duplicates == 0 }
}
```
Algorithm (return empty report if `nodes.is_empty()`):
- **Pass 1 (dedup ids):** `seen: HashMap<u64,i32>`. For each i: if `n.id == 0 || seen.contains_key(&n.id)` → `duplicates += 1`; if repair → `n.id = next_id; next_id += 1`. Then `seen.insert(n.id, i)`; `if n.id >= next_id { next_id = n.id + 1 }`. Then `invalidate_id_cache()`.
- **Pass 2 (orphans):** for each node with `parent_id != 0 && index_of_id(parent_id) < 0` → `orphans += 1`; if repair → `parent_id = 0`. Then `invalidate_id_cache()`.
- **Pass 3 (cycles):** for each i: walk parent chain with a `HashSet<u64> visited`; if id revisited → `cycles += 1`; if repair → set **that** node's `parent_id = 0`; break. Stop at `parent_id == 0`. Then `invalidate_id_cache()`.

`default repair = true` in C++ — Rust has no default args; provide `pub fn validate(&mut self, repair: bool)` plus a thin `pub fn validate_and_repair(&mut self) -> ValidateReport { self.validate(true) }` if ergonomic. (No `test_core` test directly covers `validate`, but `controller`/`imports` use it; port faithfully. The three-pass + per-pass invalidation order is the contract.)

Borrow note: passes mutate `self.nodes` while calling `self.index_of_id` (which borrows `id_cache`). Since `index_of_id` takes `&self` and pass-2/3 need `&mut`, fetch the index *before* mutating, or split into index-collection then mutation phases per pass.

### 6.7 `find_overlaps` (core.h:530-592) → `pub fn find_overlaps(&self) -> Vec<OverlapPair>`
```rust
pub struct OverlapPair { pub a_id: u64, pub b_id: u64, pub parent_id: u64 }
```
Algorithm:
1. Empty → empty.
2. Build a **local** `child_map: HashMap<u64, Vec<usize>>` from `nodes` (NOT the cache — this is `&self`/const). Iterate `nodes` in order so each group's child list is in node order.
3. For each `(parent_id, child_indices)`:
   - **Skip `parent_id == 0`** (root structs are independent).
   - **Skip if parent `is_union()`** (`index_of_id(parent_id)` → look up node; if union, skip).
   - Build `ranges: Vec<Range{ idx: usize, start: i64, end: i64 }>` over non-static children: `sz = if Struct|Array { struct_span(id) } else { byte_size() }`; **skip `sz <= 0`**; push `{idx, start: offset as i64, end: offset as i64 + sz as i64}`.
   - Sort ascending by `start` (`sort_by_key(|r| r.start)` — stable; C++ uses `std::sort`, unstable, but ties only happen at equal start where the forward double-loop still emits both pairs deterministically; **use a stable sort to guarantee aId = first declared at equal offsets**, matching `testFindOverlaps_twoFieldsAtSameOffset` which expects `aId == ids[0]`).
   - Forward double loop `for i, for j>i`: if `ranges[j].start >= ranges[i].end` → **break**; else push `OverlapPair{ a_id: nodes[ranges[i].idx].id, b_id: nodes[ranges[j].idx].id, parent_id }`.

> **Iteration-order caveat.** C++ iterates `QHash` in unspecified order across
> parent groups, but each test only has overlaps within ONE group, and the result is
> compared by `.size()` and specific `aId`/`bId` (not by overall ordering across
> groups). A `HashMap` iteration order difference does not affect any assertion. The
> within-group sort is what matters; use a **stable** sort by start.

Tests `testFindOverlaps_*` (10 cases) listed in §"TEST PLAN".

### 6.8 `index_of_id` (core.h:594-600) → `pub fn index_of_id(&self, id: u64) -> i32`
Lazy-fill `id_cache` if empty & `!nodes.is_empty()` (build full map `id→i as i32`).
Return `*id_cache.borrow().get(&id).unwrap_or(&-1)`. **Miss → -1** (`testIndexOfIdNotFound`, `testStableNodeIds`).

### 6.9 `children_of` (core.h:602-608) → `pub fn children_of(&self, parent_id: u64) -> Vec<usize>`
Lazy-fill `child_cache` (parentId→child indices in node order). Return cloned vec or empty. `children_of(0)` = root indices. `testChildrenOfEmpty` → empty for unknown parent. (Returns a clone like the C++ `QVector` copy — acceptable; callers don't mutate.)

### 6.10 `node_id_for_path` (core.h:615-645) → `pub fn node_id_for_path(&self, path: &str, sep: char /* default '.' */) -> u64`
**Miss → 0** everywhere. Provide `pub fn node_id_for_path(&self, path, sep)` and a convenience `node_id_for_path_dot(path)` if desired (C++ default sep `'.'`).
1. Empty path or empty tree → 0.
2. `segs: Vec<&str> = path.split(sep).filter(|s| !s.is_empty()).collect()` (Qt `SkipEmptyParts`). If empty → 0.
3. First segment matches a **top-level** node (`parent_id == 0`) whose `struct_type_name == segs[0] || name == segs[0]` (first by node order). Else → 0.
4. Each subsequent segment: among children of current node (`parent_id == cur.id`), first whose `name == seg` (exact, case-sensitive). Miss → 0.
5. Return matched node's id.

### 6.11 `field_path` (core.h:653-668) → `pub fn field_path(&self, id: u64, sep: char) -> String`
Walk `parent_id` chain (cycle-safe `HashSet<u64> seen`):
- `label = if !n.name.is_empty() { &n.name } else { &n.struct_type_name }`; if `label.is_empty() && n.parent_id != 0` → `label = "?"`; if non-empty → **prepend** to a `Vec<String>` (`parts.insert(0, label)`).
- stop on `cur == 0`, revisited id, or `index_of_id < 0`.
- `parts.join(&sep.to_string())`.
Anonymous root contributes nothing (no leading sep). Unknown id → "". `testFieldPath_*`, custom sep `/`.

### 6.12 `subtree_indices` (core.h:671-699) → `pub fn subtree_indices(&self, node_id: u64) -> Vec<usize>`
1. `idx = index_of_id(node_id); if idx < 0 { return vec![] }`.
2. Lazy-fill `child_cache`.
3. DFS with explicit `Vec<u64> stack` + `HashSet<u64> visited`: seed `result = vec![idx as usize]`, `visited = {node_id}`, `stack = [node_id]`.
4. Pop pid; for each child index `ci` in `child_cache[pid]`: if child id not visited → mark, push `ci` to result, push child id to stack.
5. Return result (node index first, then descendants in DFS order). `testSubtreeIndices` (3), `testSubtreeCycleSafe` (2, terminates).

### 6.13 `depth_of` (core.h:701-714) → `pub fn depth_of(&self, idx: i32) -> i32`
Walk `parent_id` chain from idx; `d += 1` per hop; cycle-safe (`HashSet visited`); stop at `parent_id == 0`, cycle, or missing parent. Root → 0, orphan → 0. (Note: the C++ loop increments `d` *after* resolving the parent, so an orphan whose parent isn't found contributes 0 — replicate exactly: check `cur = index_of_id(parent); if cur < 0 { break } d += 1;`.) `testNodeTree_depth` (0/1/2), `testDepthOfCycle` (< 100), `testDepthOfOrphan` (0).

### 6.14 `compute_offset` (core.h:723-736) → `pub fn compute_offset(&self, idx: i32) -> i64`
Sum `offset` (as i64) up the parent chain, cycle-safe. **Returns i64, CAN BE NEGATIVE.** Walk: `total += nodes[cur].offset as i64`; stop on `parent_id == 0`, revisited, or missing. `testNodeTree_computeOffset` (16), `testComputeOffsetNested` (24), `testComputeOffsetLarge` (0x7FFFFFFF preserved — `i64` avoids overflow), `testComputeOffsetCycle` (terminates). Invariant #7.

### 6.15 `absolute_address` (core.h:742-750) → `pub fn absolute_address(&self, idx: i32) -> (u64, bool)`
`let off = self.compute_offset(idx); if off < 0 { (self.base_address, false) } else { (self.base_address + off as u64, true) }`. (C++ out-param `bool* ok` → tuple. Safety: never cast a negative `i64` to `u64` — invariant #7.)

### 6.16 `struct_span` (core.h:752-787) → recursive, cycle-safe
```rust
pub fn struct_span(&self, struct_id: u64,
    child_map: Option<&HashMap<u64, Vec<usize>>>,
    visited: Option<&mut HashSet<u64>>) -> i32
```
The C++ defaults `childMap=nullptr`, `visited=nullptr` (then makes a local set).
Rust idiom: a thin public wrapper + a private recursive worker that always owns a
`&mut HashSet`:
```rust
pub fn struct_span(&self, id: u64, child_map: Option<&HashMap<u64,Vec<usize>>>, visited: Option<&mut HashSet<u64>>) -> i32 {
    match visited {
        Some(v) => self.struct_span_impl(id, child_map, v),
        None => { let mut v = HashSet::new(); self.struct_span_impl(id, child_map, &mut v) }
    }
}
```
`struct_span_impl`:
1. `if visited.contains(&id) { return 0 }` (cycle); `visited.insert(id)`.
2. `idx = index_of_id(id); if idx < 0 { return 0 }`.
3. `declared = node.byte_size()`.
4. **Short-circuit:** `if !is_container_kind(kind) && ref_id == 0 { return declared }`.
5. `max_end: i64 = 0`. For each child (`child_map.map(|m| m.get(&id)).flatten()` or `children_of(id)`): **skip `is_static`**; `sz = if Struct|Array { struct_span_impl(c.id, child_map, visited) } else { c.byte_size() }`; `end = c.offset as i64 + sz as i64`; `if end > max_end { max_end = end.min(i32::MAX as i64) }`.
6. **Embedded ref:** `if kids.is_empty() && kind == Struct && ref_id != 0 { max_end = max_end.max(struct_span_impl(ref_id, child_map, visited) as i64) }`.
7. `return declared.max(max_end as i32)` (max_end already clamped to i32::MAX).

Tests `testStructSpan` (12/8/8/0/64/88), `testStructSpanLeafShortCircuit` (8),
`testStructSpanExcludesStaticFields`/`testStaticFieldExcludedFromSpan` (12 / 4),
`testStructSpanCycleDetection` (>= 0). Invariants #8, #10. The `visited`-set
diamond-undercount behavior is intentional — keep it.

Borrow note: `children_of` mutates `child_cache` via `RefCell` (`&self`), while
`struct_span_impl` only reads `nodes`/`index_of_id` — all `&self`, so recursion is
fine. The `child_map` param lets `find_overlaps` (which built a local map) avoid the
cache; pass `None` from `total_byte_size`/leaf callers.

### 6.17 `normalize_prefer_ancestors` / `normalize_prefer_descendants` (compose.cpp:1747-1784)
```rust
pub fn normalize_prefer_ancestors(&self, ids: &HashSet<u64>) -> HashSet<u64>
pub fn normalize_prefer_descendants(&self, ids: &HashSet<u64>) -> HashSet<u64>
```
**Ancestors:** for each id: `idx = index_of_id(id); if idx < 0 { continue }`. Walk `parent_id` chain (cycle-safe `visited`); if any ancestor `cur` is in `ids` → mark `ancestor_selected`, break. Insert id into result iff `!ancestor_selected`. Keeps topmost.
**Descendants:** for each id: `sub = subtree_indices(id)`; if any `sub` node's id `!= id && ids.contains(id)` → `has_selected_descendant`. Insert id iff `!has_selected_descendant`. Keeps deepest.
Tests: `testNormalizePreferAncestors`/`...Basic`, `testNormalizePreferDescendants`/`...Basic`.

### 6.18-6.19 JSON — see §5.2.

### 6.20 `root_class_names` (core.h:853-863) — **free function** → `pub fn root_class_names(tree: &NodeTree) -> Vec<String>`
For each node with `parent_id == 0 && kind == Struct`: `name = if !struct_type_name.is_empty() { struct_type_name } else { name }`; if still empty → `"Untitled"`; **dedup** (skip if already present — linear `contains`, preserving first-seen order). If result empty → `vec!["Untitled".into()]`. `testRootClassNames_*` (5 cases). Invariant #16.

---

## 7. `history.rs` — `ValueHistory` (core.h:867-923)
```rust
pub struct ValueHistory {
    values: [String; 10],
    timestamps: [i64; 10],   // ms since epoch
    count: i32,              // public read needed (tests read h.count)
    head: i32,
}
pub const VALUE_HISTORY_CAPACITY: i32 = 10; // kCapacity
```
Tests read `h.count` directly (`testValueHistory_duplicateIgnored`, `_oscillation`,
`_ringWrap`) — expose `pub fn count(&self) -> i32` (or `pub count`). Methods:
- `record(&mut self, v: &str)`: if `count > 0` and `values[(head + 9) % 10] == v` → **return (dedup consecutive)**. Else `values[head] = v.into(); timestamps[head] = now_ms(); head = (head + 1) % 10; if count < i32::MAX { count += 1; }`.
- `clear(&mut self)`: `count = 0; head = 0;` (does not wipe arrays).
- `unique_count(&self) -> i32`: `count.min(10)`.
- `heat_level(&self) -> i32`: `count <= 1 → 0; count == 2 → 1; count <= 4 → 2; else 3`. **Keyed on total `count`**, not unique (invariant #12 — `testValueHistory_oscillation`: A/B/A/B → count 4 → 2/warm).
- `last(&self) -> String`: `count == 0 → "".into()` else `values[(head + 9) % 10].clone()`.
- `for_each(&self, f)`: oldest→newest over `unique_count()`; `start = (head + 10 - n) % 10`; `for i in 0..n { f(&values[(start + i) % 10]) }`. (`testValueHistory_forEach`, `_ringWrap`.)
- `for_each_with_time(&self, f)`: newest→oldest; `idx = (head + 10 - 1 - i) % 10`.

**Clock injection.** C++ uses `QDateTime::currentMSecsSinceEpoch()`. Tests never
assert timestamp values (only ordering/values), so the default impl can call
`std::time::SystemTime::now()` → ms. **Recommended for testability:** make `record`
take the time implicitly via a private `now_ms()` that the test harness can leave as
wall-clock (no test depends on it). Do NOT add `chrono` just for this — `std::time`
suffices. `Default` for `ValueHistory` zeroes everything (`values: Default` =
array of empty Strings — use `std::array::from_fn(|_| String::new())`).

---

## 8. `line.rs` — display model + column geometry

These types are produced by `compose` but **declared in core.h** and are part of the
shared contract; the column-span helpers are **pure and heavily tested by
`test_core`**. Port all of them here (the editor + compose import them).

### 8.1 Enums / consts
- `LineKind` (core.h:927-931): `#[repr(u8)]` `CommandRow=0, Blank, Header, Field, Continuation, Footer, ArrayElementSeparator`. Keep `Blank` for stability.
- `ChipKind` (core.h:971-980): `#[repr(u8)]` `Enum=0, TypeHint, Rtti, Symbol, Comment, AddComment`.
- `Marker` (core.h:182-193): consts `M_CONT=0, M_PTR0=2, M_CYCLE=3, M_ERR=4, M_STRUCT_BG=5, M_HOVER=6, M_SELECTED=7, M_CMD_ROW=8, M_ACCENT=9, M_FOCUS=10` (1 unused). Plain `i32`/`u32` consts (used as bit indices in `markerMask`).
- Selection-id consts + encoders (core.h:933-961): `K_COMMAND_ROW_ID = u64::MAX`, `K_COMMAND_ROW_LINE=0`, `K_FIRST_DATA_LINE=1`, `K_FOOTER_ID_BIT=0x8000_0000_0000_0000`, `K_ARRAY_ELEM_BIT=0x4000_0000_0000_0000`, `K_ARRAY_ELEM_SHIFT=42`, `K_ARRAY_ELEM_MASK=0x3FFFFC0000000000`, `K_MEMBER_BIT=0x2000_0000_0000_0000`, `K_MEMBER_SUB_SHIFT=42`, `K_MEMBER_SUB_MASK=0x3FFFFC0000000000`.
  - `make_array_elem_sel_id(node_id, elem_idx: i32) -> u64` = `node_id | K_ARRAY_ELEM_BIT | (((elem_idx as u64) & 0xFFFFF) << 42)` (C++ `Q_ASSERT(elemIdx >= 0)` → `debug_assert!(elem_idx >= 0)`).
  - `array_elem_idx_from_sel_id(sel) -> i32` = `((sel & K_ARRAY_ELEM_MASK) >> 42) as i32`.
  - `make_member_sel_id`/`member_sub_from_sel_id` — same pattern with the member bit/mask.
- Column constants (core.h:1130-1148): `K_FOLD_COL=3, K_TREE_INDENT=2, K_COL_TYPE=14, K_COL_NAME=22, K_COL_VALUE=96, K_COL_COMMENT=28, K_COL_BASE_ADDR=12, K_SEP_WIDTH=1, K_MIN_TYPE_W=9, K_MAX_TYPE_W=128, K_MIN_NAME_W=10, K_MAX_NAME_W=128, K_COMPACT_TYPE_W=20, K_DEFAULT_REFRESH_MS=200`. **`K_TREE_INDENT = 2`** (NOT 3 — the tests-catalog has a typo; `test_core.cpp:326` computes depth-1 type span start as `kFoldCol(3) + 1*kTreeIndent = 5`, confirming 2).

### 8.2 Structs (core.h:982-1076)
- `LineChip { kind: ChipKind, start_col: i32 /*-1*/, end_col: i32 /*-1*/, text: String, rtti_vtable_addr: u64, type_hint_kinds: Vec<NodeKind>, enum_current_value: i64, enum_ref_node_id: u64 }`.
- `LineMeta` — the 35+ field record (core.h:994-1037). Transcribe all fields with their defaults (e.g. `node_idx: i32 = -1`, `node_kind: NodeKind = Int32`, `element_kind = UInt8`, `array_element_idx = -1`, `effective_type_w = 14`, `effective_name_w = 22`, `brace_col = -1`, `chips: Vec<LineChip>`). Use a hand-written `Default`.
- `find_chip(lm: &LineMeta, kind: ChipKind) -> Option<&LineChip>` (linear, first match).
- `is_synthetic_line(lm: &LineMeta) -> bool` = `lm.line_kind == LineKind::CommandRow`.
- `LayoutInfo { type_w:14, name_w:22, offset_hex_digits:8, base_address:0, tree_lines:false }`.
- `ComposeResult { text: String, meta: Vec<LineMeta>, layout: LayoutInfo, max_line_len: i32, line_starts: Vec<i32> }`.
- `ViewState` (core.h:1456-1468): `scroll_line, cursor_line, cursor_col, x_offset: i32; cursor_node_id: u64; cursor_sub_line: i32`.

### 8.3 `ColumnSpan` + `EditTarget` + `LineGeometry` + span helpers (core.h:1119-1452)
- `ColumnSpan { start: i32, end: i32, valid: bool }`. **Invalid default `{0,0,false}`** = the C++ `return {};`. Provide `ColumnSpan::invalid()` / `Default` for the `return {}` paths.
- `EditTarget` enum (15 variants, core.h:1125-1127): plain `#[derive]` enum.
- `LineGeometry { prefix_width: i32 /*=K_FOLD_COL*/, indent_width: i32, type_column_width: i32 /*=K_COL_TYPE*/, name_column_width: i32 /*=K_COL_NAME*/ }` with `type_start()`, `name_start()`, `value_start()`, `document_column(content_col)`, and `for_line(lm: &LineMeta) -> LineGeometry` (flush-left prefix 0 for CommandRow / root Footer).
- All `*SpanFor` helpers (core.h:1183-1452) → free `pub fn`s taking `&LineMeta` (+ `&str lineText` for the text-scanning ones). These are pure string/column math. The text-scanning ones (`memberNameSpanFor`, `commandRow*`, `arrayElem*`, `pointer*`, `array{Prev,Index,Count,Next}SpanFor`, `staticExprSpanFor`) use `String`/`char` indexing — **port using char-index semantics, not byte-index**, because the C++ operates on `QString` (UTF-16 code units / `QChar`). Reclass content lines are ASCII except the special glyphs (▾ U+25BE, ▸ U+25B8, → U+2192, × U+00D7). **Decision: operate on a `Vec<char>` (or `&[char]`) view of the line** so `indexOf`/`[i]`/`mid` map to char positions exactly like `QString`; the editor stores line text and can supply char positions. Document this clearly: `lineText.indexOf(QChar(0x2192))` → search the char slice for `'\u{2192}'`. The column constants the tests check are pure integer math (no text), so `typeSpanFor`/`nameSpanFor`/`valueSpanFor`/`commentSpanFor` are trivially exact.

**`test_core` only asserts** `typeSpanFor`/`nameSpanFor`/`valueSpanFor` (depth 0 & 1),
their invalidity for Continuation/Header/Footer, and `staticExprSpanFor`. The other
~20 span helpers are exercised by editor/compose tests later — port them now (cheap)
but they're verified downstream.

---

## 9. `command.rs` — undo/redo command model (core.h:1080-1115)

The C++ `namespace cmd` POD structs unified with `std::variant<...>`. Port the
variant as a Rust `enum Command`. Keep `OffsetAdj` as a standalone struct (it's
embedded inside other commands, not a `Command` variant).

```rust
#[derive(Clone, PartialEq, Debug)]
pub struct OffsetAdj { pub node_id: u64, pub old_offset: i32, pub new_offset: i32 }

#[derive(Clone, PartialEq, Debug)]
pub enum Command {
    ChangeKind { node_id: u64, old_kind: NodeKind, new_kind: NodeKind, off_adjs: Vec<OffsetAdj> },
    Rename { node_id: u64, old_name: String, new_name: String },
    Collapse { node_id: u64, old_state: bool, new_state: bool },
    Insert { node: Node, off_adjs: Vec<OffsetAdj> },
    Remove { node_id: u64, subtree: Vec<Node>, off_adjs: Vec<OffsetAdj> },
    ChangeBase { old_base: u64, new_base: u64, old_formula: String, new_formula: String },
    WriteBytes { addr: u64, old_bytes: Vec<u8>, new_bytes: Vec<u8> },   // QByteArray → Vec<u8>
    ChangeArrayMeta { node_id: u64, old_element_kind: NodeKind, new_element_kind: NodeKind, old_array_len: i32, new_array_len: i32 },
    ChangePointerRef { node_id: u64, old_ref_id: u64, new_ref_id: u64 },
    ChangeStructTypeName { node_id: u64, old_name: String, new_name: String },
    ChangeClassKeyword { node_id: u64, old_keyword: String, new_keyword: String },
    ChangeOffset { node_id: u64, old_offset: i32, new_offset: i32 },
    ChangeEnumMembers { node_id: u64, old_members: Vec<(String, i64)>, new_members: Vec<(String, i64)> },
    ChangeOffsetExpr { node_id: u64, old_expr: String, new_expr: String },
    ToggleStatic { node_id: u64, old_val: bool, new_val: bool },
    ToggleRelative { node_id: u64, old_val: bool, new_val: bool },
    ToggleBigEndian { node_id: u64, old_val: bool, new_val: bool },
    ChangeComment { node_id: u64, old_comment: String, new_comment: String },
}
```
Each carries old+new for reversibility. The **application** of commands lives in
`controller` (out of this spec) — define only the data shapes here. `test_core` does
not exercise `Command`. Order of variants matches the `std::variant` alternative order
(not load-bearing, but keep it for readability/diffing).

---

## 10. `commontypes.rs` — predefined struct templates (commontypes.h)
```rust
pub struct CommonField { pub offset: i32, pub kind: NodeKind, pub name: &'static str, pub ptr_target: &'static str /* "" = void* */ }
pub struct CommonType  { pub name: &'static str, pub category: &'static str, pub class_keyword: &'static str, pub total_size: i32, pub fields: &'static [CommonField] }
pub const COMMON_TYPES: &[CommonType] = &[ /* ~50 entries */ ];
pub fn find_common_type(name: &str) -> Option<&'static CommonType>; // linear, exact case-sensitive
```
- `CommonField::ptr_target` is `const char* = nullptr` in C++ → use `&'static str` with
  `""` meaning "no target / void*" (the only consumers check `is_empty()`; mapping
  `nullptr`→`""` is faithful since C++ checks emptiness, not null specifically).
- The `CT(...)` macro auto-counts fields with `std::size`. In Rust each `CommonType`
  references a `&'static [CommonField]` slice; `fields.len()` replaces `fieldCount` (drop
  the separate count field — it's derivable). Shared field arrays
  (`std::wstring`↔`std::string`, `TArray`↔`FString`) → both reference the same `const`
  slice (e.g. `K_FIELDS_STD_STRING`). `kCommonTypeCount` → `COMMON_TYPES.len()`.
- **Transcribe all ~50 types and their field arrays verbatim** from `commontypes.h:30-390`
  (the full inventory is in `core-model.md §11.3`; values re-verified against source).
  `_padding`/`_pad` are **real fields** (exact byte layout). Pointer targets:
  `UNICODE_STRING.Buffer → "UTF16"`, `OBJECT_ATTRIBUTES.ObjectName → "UNICODE_STRING"`.
- `find_common_type`: `COMMON_TYPES.iter().find(|t| t.name == name)`.
- Build as `const` arrays + linear scan (mirrors C++; ~50 entries — no `phf` needed).
- **Not covered by `test_core`/`test_typeinfer`** but used by the type chooser
  (controller/ui) and likely `test_generator`/`test_controller`. Port now for the
  foundation; verify downstream.

---

## 11. `typeinfer.rs` — heuristic type inference (typeinfer.h)

Header-only, pure. **All thresholds/constants must match bit-for-bit** (invariant #13).

### 11.1 Public types & API
```rust
pub struct InferHints<'a> {
    pub min_observed: Option<&'a [u8]>, // same len as data
    pub max_observed: Option<&'a [u8]>,
    pub monotonic: bool,
    pub never_changed: bool,
    pub sample_count: i32,  // 0 = no history
    pub ptr_size: i32,      // default 8
}
pub struct TypeSuggestion { pub kinds: Vec<NodeKind>, pub score: i32, pub strength: i32 }

pub fn infer_types(data: &[u8], hints: &InferHints, max_results: i32) -> Vec<TypeSuggestion>;
pub fn format_hint(s: &TypeSuggestion) -> String;
```
- C++ signature is `inferTypes(const uint8_t* data, int len, hints={}, maxResults=3)`.
  In Rust, `data: &[u8]` carries the length; provide `infer_types(data, &InferHints::default(), 3)`
  as the common call, plus the `len <= 0` / `data == nullptr` guards map to
  `data.is_empty()`. Default `InferHints { ptr_size: 8, .. }` (impl `Default`).
- `format_hint` (typeinfer.h:38-44): empty kinds → ""; size 1 → `kind_meta(kinds[0]).type_name`; size >1 → `format!("{}\u{00D7}{}", type_name, kinds.len())` (× is U+00D7).

### 11.2 byte loads (detail) — endianness
C++ uses `std::memcpy` (host endianness) and *assumes little-endian x86/x64* (intrinsic
to the heuristic). On all supported targets this == little-endian. **Use
`u16/u32/u64::from_le_bytes` and `f32/f64::from_le_bytes`** (NOT `from_ne_bytes` — be
explicit; the C++ comment says it assumes LE data, and the Windows/macOS/Linux targets
are all LE, so `from_le_bytes` is correct and deterministic across the CI box).
`popcount32` → `u32::count_ones()`. `is_printable(c)` = `(0x20..=0x7E).contains(&c)`.

### 11.3 `is_good_float(bits: u32) -> bool` (typeinfer.h:88-95)
`exp = (bits >> 23) & 0xFF`; reject `exp == 0xFF` (inf/nan); reject `exp == 0 && (bits & 0x7FFFFF) != 0` (denormal); `f = f32::from_bits(bits); af = (f as f64).abs(); f == 0.0 || (af >= 1e-6 && af <= 1e7)`.

### 11.4 Feature checkers → all return `FeatureResult { passed: i32, checked: i32 }`
Port each verbatim (typeinfer.h:97-291). Key bit-exact details:
- `count_float_features`: 4 base features (finite via `f.is_finite()`; non-denormal via `exp>0 || (cur & 0x7FFFFFFF)==0`; range; fractional part `(modf →) f.fract().abs() > 0.0001`). +4 with history. **`modf`**: C++ `std::modf(f, &ip)` returns fractional part; Rust `f.fract()` (note `frac` is `fabs(modf(...))` → `f.fract().abs()` as f64). Compute on the `f32` promoted to `f64` exactly as C++ does (`(double)f`).
- `count_int_features`: **hard reject `val==0 || val==0xFFFFFFFF` → `{0,3}`**. Else 3 base: feature1 always +1; feature2 `val <= 1_000_000 || (sv.wrapping_add(1_000_000) as u32) <= 2_000_000` (replicate the C++ `(uint32_t)(sv + 1000000) <= 2000000u` with wrapping); feature3 `(-32768..=32767).contains(&sv)`. +3 with history.
- `count_flag_features`: 2 base (`pc in 1..=3`; `val > 256 || (val & (val.wrapping_sub(1))) != 0`). +3 with history (`(minV ^ maxV).count_ones() <= 4`; varies; `(minV & maxV) == minV`).
- `count_ptr_features64(val: u64)`: hard rejects → `{0,5}`: `0`, `u64::MAX`, `0x00000000FFFFFFFF`; **non-canonical** (`val > 0x00007FFFFFFFFFFF && val < 0xFFFF800000000000`); packed sentinels (`low == 0xFFFFFFFF`; `high == 0xFFFFFFFF`; `low != 0 && (low & 0x000FFFFF) == 0 && high < 0x10000`). Then 5 features (`val & 7 == 0`; `>= 0x10000`; `(val >> 32) != 0`; `> 0x100000000`; `< 0xFFFF800000000000`).
- `count_ptr_features32(val: u32)`: 3 (`!=0 && !=0xFFFFFFFF`; `val & 3 == 0`; `>= 0x10000`).
- `count_string_features(data: &[u8])`: `len < 2 → {0,4}`. Compute printable count, letters (`A-Za-z`), `max_consec`. 4 features (`max_consec >= 4`; `ratio > 0.75`; `letters >= 1`; `ratio > 0.90`). `ratio = printable as f64 / len as f64`.
- `count_int16_features(val: u16)`: 2 base (`val != 0`; `(-16384..=16384).contains(&sv)` with `sv = val as i16 as i32`). +2 with history.

### 11.5 Score & strength (typeinfer.h:295-309)
- `feature_score(r) -> i32` = `if r.checked == 0 { 0 } else { (r.passed * 100) / r.checked }` (**integer division**).
- `strength_from_score(score) -> i32`: `>= 85 → 3; >= 50 → 2; >= 25 → 1; else 0`.

### 11.6 Candidate accumulation (typeinfer.h:313-327)
```rust
struct Candidate { kinds: Vec<NodeKind>, score: i32 }
fn add_candidate(out: &mut Vec<Candidate>, k: NodeKind, score: i32) { if score >= 25 { out.push(Candidate { kinds: vec![k], score }) } }
fn add_split_candidate(out, k, count: i32, score) { if score >= 25 { out.push(Candidate { kinds: vec![k; count as usize], score }) } }
```

### 11.7 Whole-width tries (typeinfer.h:331-412) — port verbatim
- `try_whole8(data, hints, out)`: Pointer64 (if `ptr_size == 8`); Double (hard rejects: out of `[1e-6,1e7]` range OR `(u64 & 0xFFFFFFFF) == 0 && mantissa != 0` split-field; then 4 features incl. `frac > 0.001 || ad <= 1.0` and `!(ad > 1000.0 && frac < 0.001)`); UTF8 (string features over the 8 bytes); UInt64 (only if `(u64 >> 32) != 0`; 3 features: always-1; `< 0x0000FFFFFFFFFFFF`; `monotonic || (u64 & 0xFFF) == 0`).
- `try_whole4(data, minP, maxP, hints, out)`: Float; **Int32 AND UInt32 both use `count_int_features` (identical scores)**; UInt32 again via `count_flag_features` (flags map to UInt32 kind); Pointer32 (if `ptr_size == 4`).
- `try_whole2`: Int16 and UInt16, same `count_int16_features` score.
- `try_whole1`: UInt8 with score 50 if `v ∈ {0,1}` else 25.

### 11.8 `try_split_uniform` (typeinfer.h:416-477) — port verbatim
- `len == 8`: Float×2 (both halves good-float-or-zero, at least one non-zero; score = `min(featureScore(rA), featureScore(rB))`; zero half → `FeatureResult{2,4}`); Int32×2 (zero half → `{1,3}`); UInt32×2 (same). Min-pairs computed over the two 4-byte halves, with min/max-observed sliced at +4.
- `half_len == 2` (len 4 or 8): Int16×count & UInt16×count (`count = len/2`); `min_score` over parts; only if `any_non_zero`. min/max sliced at +`i*2`.

### 11.9 `prune_and_rank` (typeinfer.h:481-518)
1. Sort **descending by score** (`cands.sort_by(|a,b| b.score.cmp(&a.score))`). C++ uses `std::sort` (unstable) with `a.score > b.score`. **Use a stable sort (`sort_by`) descending** — Rust's `sort_by` is stable; this is safe and avoids nondeterminism across equal scores (the dedup keeps first, so insertion order among equals is preserved — matches the natural reading of the C++ accumulation order for the tested cases; equal-score ties in tests do not affect the asserted top kind).
2. **Dedup** by identical `kinds` vec (keep first/highest). O(n²) — iterate, push to `deduped` if no existing entry has equal `kinds`.
3. **Dominance:** `if deduped.len() >= 2 && deduped[0].score >= deduped[1].score * 3 / 2 && (deduped[0].score - deduped[1].score) >= 10 { deduped.truncate(1) } else if deduped.len() > max_results as usize { deduped.truncate(max_results as usize) }`. (Note `* 3 / 2` is integer math: `second * 3 / 2`.)
4. Build result: for each, `str = strength_from_score(score)`; **push only if `str > 0`**. Return `Vec<TypeSuggestion>`.

### 11.10 `infer_types` entry (typeinfer.h:524-548)
1. `if data.is_empty() { return vec![] }` (covers `!data || len<=0`).
2. `if all_zero(data) { return vec![] }`.
3. `let mut cands = Vec::with_capacity(12);`
4. whole-width by length: `len >= 8 → try_whole8`; `len == 4 → try_whole4`; `len == 2 → try_whole2`; `len == 1 → try_whole1`. (**A len-8 buffer does NOT also try whole4** — exact `==` for 4/2/1.)
5. `if len >= 4 { try_split_uniform(...) }`.
6. `prune_and_rank(cands, max_results)`.

---

## 12. Cross-cutting: platform & concurrency
- **No OS APIs** in any of the three files. `popcount32`'s `#if __GNUC__` branch →
  `u32::count_ones()` (one impl, all targets). The x64 address-range constants in
  `typeinfer` are *data heuristics*, compile everywhere — no `#[cfg]` needed.
- **No threads/atomics.** The model is single-threaded (controller drives it from the
  UI thread). The `RefCell` caches make `NodeTree` `!Sync` — fine, it's never shared
  across threads in the port. Do **not** reach for `Mutex`/`RwLock`.

---

## TEST PLAN

All tests run with `--no-default-features` (headless, no gpui). Place in
`src/core/*.rs` `#[cfg(test)] mod tests` (unit) and/or `tests/core.rs` (integration).
Every assertion below is a 1:1 transliteration of a `QCOMPARE`/`QVERIFY` from the
oracle source; **no golden-file diffs** apply to this subsystem. Cite: oracle log
`_oracle/logs/test_core.txt` (93/0/0) and `_oracle/logs/test_typeinfer.txt` (20/0/0)
confirm all expected values; `_oracle/test_sources/test_core.cpp` /
`test_typeinfer.cpp` are the verbatim sources.

> **Provider-backed tests inside test_core are OUT OF SCOPE here.**
> `testBufferProvider`, `testNullProvider`, `testIsReadable`, `testIsReadableOverflow`,
> `testProviderWrite` exercise `BufferProvider`/`NullProvider` — they belong to the
> **`provider`** subsystem spec (`test_provider`). Skip them in the core-model port;
> the remaining ~88 test_core assertions are core-model.

### Group A — kind metadata (`kind.rs`)
| Rust `#[test]` | From C++ | Asserts |
|---|---|---|
| `size_for_kind` | testSizeForKind | Hex8=1,Hex16=2,Hex32=4,Hex64=8,Float=4,Double=8,Vec3=12,Mat4x4=64,Struct=0 |
| `hex128_size` | testHex128Size | Hex128 == 16 |
| `lines_for_kind` | testLinesForKind | Hex32/Vec2/Vec3/Vec4=1, Mat4x4=4 |
| `alignment_for` | testAlignmentFor | Hex8=1,Hex16=2,Hex32=4,Hex64=8,Float=4,Double=8,Vec3=4,Mat4x4=4,UTF8=1,UTF16=2,Struct=1 |
| `kind_string_round_trip` | testKindStringRoundTrip | for `0..=Array`: `kind_from_string(kind_to_string(k)) == k` |
| `kind_meta_completeness` | testKindMetaCompleteness | for `0..=Array`: `kind_meta(k)` Some, `m.kind==k`, names non-empty, `lines>=1`, `align>=1`; and for every table row `size_for_kind/lines_for_kind/alignment_for == m.*` |
| `kind_meta_o1` | testKindMetaO1 | for every row, `kind_meta(m.kind)` matches kind & size |
| `kind_from_type_name_round_trip` | testKindFromTypeNameRoundTrip | for every row: `kind_from_type_name(m.type_name) == (m.kind, true)` |
| `is_pointer_kind` | testIsPointerKind | Pointer32/64 true; FuncPtr64/Hex64 false |
| `is_container_kind` | testIsContainerKind | Struct/Array true; Hex64/Pointer64 false |
| `is_string_kind` | testIsStringKind | UTF8/UTF16 true; Hex8 false |
| `helper_functions` | testHelperFunctions | is_hex_node (Hex8/Hex128 true, Int32 false), is_vector_kind, is_matrix_kind, is_func_ptr per source |

### Group B — Node (`node.rs`, `json.rs`)
| Rust `#[test]` | From C++ | Asserts |
|---|---|---|
| `byte_size_dynamic` | testByteSizeDynamic | UTF8 strLen=128→128; UTF16 strLen=32→64; Float→4 |
| `node_byte_size_utf8/utf16/array/bitfield` | testNodeByteSize* | 32; 32; 40 (10×Int32); bitfield Hex32 container→4 |
| `node_is_union_bitfield_enum` | testNodeIsUnionBitfieldEnum | union/bitfield/enum from class_keyword; empty→not union/enum |
| `node_json_round_trip` | testNodeJsonRoundTrip | all optional fields set → equal after round-trip (id,kind,name,structTypeName,classKeyword,parentId,offset,isStatic,offsetExpr,isRelative,arrayLen,strLen,refId,elementKind,ptrDepth,comment, enumMembers[(A,0),(B,1)], bitfieldMembers[(bit0,0,1),(bits,1,3)]) |
| `comment_json_round_trip` | testCommentJsonRoundTrip | `json["comment"]=="player HP"`; loaded comment/name/kind preserved |
| `empty_comment_not_serialized` | testEmptyCommentNotSerialized | `!json.contains("comment")`; loaded comment empty |

### Group C — NodeTree topology / offsets / spans (`tree.rs`)
| Rust `#[test]` | From C++ | Asserts |
|---|---|---|
| `add_and_children` | testNodeTree_addAndChildren | add returns idx 0; childrenOf(rootId)=[1]; childrenOf(0)=[0] |
| `stable_node_ids` | testStableNodeIds | first id 1, second 2; indexOfId 1→0,2→1,99→-1 |
| `add_node_auto_id` | testAddNodeAutoId | two id=0 nodes get distinct non-zero ids |
| `reserve_id_monotonic` | testReserveIdMonotonic | b>a, c>b |
| `children_of_empty` | testChildrenOfEmpty | unknown parent → empty |
| `index_of_id_not_found` | testIndexOfIdNotFound | unknown → -1 |
| `depth` | testNodeTree_depth | 0/1/2 for 3-level |
| `depth_of_cycle` | testDepthOfCycle | manual A↔B cycle → `d < 100` (terminates) |
| `depth_of_orphan` | testDepthOfOrphan | parent 99999 → 0 |
| `compute_offset` | testNodeTree_computeOffset | f@16 → 16 |
| `compute_offset_nested` | testComputeOffsetNested | 0+16+8 → 24 |
| `compute_offset_large` | testComputeOffsetLarge | 0x7FFFFFFF preserved as i64 |
| `compute_offset_cycle` | testComputeOffsetCycle | A↔B cycle terminates |
| `subtree_indices` | testSubtreeIndices | root+2 children → 3 |
| `subtree_cycle_safe` | testSubtreeCycleSafe | root+child → 2, contains 0 & 1 |
| `struct_span` | testStructSpan | 12; nested inner 8 / outer 8; empty 0; prim array[16]UInt32 64; container+array[10]UInt64@8 → 88 |
| `struct_span_leaf_short_circuit` | testStructSpanLeafShortCircuit | leaf UInt64 → 8 |
| `struct_span_excludes_static` | testStructSpanExcludesStaticFields | 12 (static struct@0 excluded) |
| `static_field_excluded_from_span` | testStaticFieldExcludedFromSpan | 4 (static UInt64@1000 excluded) |
| `struct_span_cycle_detection` | testStructSpanCycleDetection | B refId→A → span ≥ 0 (terminates) |

### Group D — selection normalizers (`tree.rs`)
`normalize_prefer_ancestors` (testNormalizePreferAncestors + ...Basic): root+leaf→{root}; A+leaf→{A}; root+A→{root}; leaf alone→{leaf}; root+child→{root}.
`normalize_prefer_descendants` (testNormalizePreferDescendants + ...Basic): root+a+b→{a,b}; root+a→{a}; root alone→{root}; root+child→{child}.

### Group E — overlaps (`tree.rs`) — testFindOverlaps_* (10)
clean adjacent → none; same-offset pair → 1 `(ids[0],ids[1])`; partial straddle → 1; fully-contained → 1; touching `[0,1)`/`[1,2)` → none; union children → none; static fields → none; one `UInt128[0,16)` over 3 `UInt32` → 3 pairs all `aId==ids[0]`; root-level structs → none; independent parents isolated → 1 in group A `(a1,a2,parentId=aid)`.

### Group F — paths (`tree.rs`)
testFieldPath_simple (`Player.Health`, root→`Player`), _nested (`Player.Stats.Health`), _unknownId (`""`), _customSeparator (`Pkt/len`). testNodeIdForPath_roundtrip (fieldPath→nodeIdForPath identity; `Player.Stats`→sId; `Player`→pId), _misses (`""`/`"Nope"`/`"Player.Missing"` → 0).

### Group G — rootClassNames (`tree.rs`) — testRootClassNames_* (5)
multiClass → ["UnnamedClass0","UnnamedClass1"] (nested excluded); prefersStructTypeName → ["MyStruct"]; emptyTreeYieldsUntitled → ["Untitled"]; dedupesByName → size 1; skipsNonStructRoots → ["Real"].

### Group H — tree JSON (`json.rs`)
testNodeTree_jsonRoundTrip (baseAddress 0xDEAD, 2 nodes, name/kind/offset preserved); testNodeIdJsonRoundTrip (ids preserved, `next_id() >= 3`); testNodeTreeJsonRoundTrip (baseAddress 0x7FF600000000, formula, pointerSize 4, node[1] name/offset); testStaticFieldJsonRoundTrip (isStatic/offsetExpr/name preserved); testStaticFieldJsonBackwardCompat (missing isStatic/offsetExpr → false/""); testCommentTreeRoundTrip (score comment preserved, speed empty).

### Group I — ValueHistory (`history.rs`)
testValueHistory_empty (heat 0, uniqueCount 0, last ""); _singleValue (heat 0, uc 1, last "42"); _duplicateIgnored (count 1, heat 0); _heatLevels (1→0,2→1,3→2,4→2,5→3); _ringWrap (count 15, uc 10, heat 3, last "14", forEach 10 items first "5" last "14"); _forEach (x,y,z in order); _oscillation (A/B/A/B → count 4, heat 2); testValueHistoryDedup (uc 1, last "42"); testValueHistoryRingOverflow (uc 10, last "14").

### Group J — column spans (`line.rs`)
testColumnSpan_field (depth 1: type `[5,19)`, name `[20,42)`, value `[43,43+96)`); _continuation (type/name invalid; value `[ind+38, ind+38+96)` with `ind=5`); _headerFooter (all invalid for Header and Footer); _depth0 (type `[3,17)`, name `[18,40)`, value `[41,41+96)`); testStaticExprSpanFor (extracts `"base + e_lfanew"` from `"   return base + e_lfanew  → 0x1400000E8"` — note the `→` is U+2192; use char-slice indexing).

### Group K — typeinfer (`typeinfer.rs`) — test_typeinfer.cpp (skip the 2 `QBENCHMARK` slots)
nullPtr/zeroLen (`infer_types(&[], ..)` empty), allZeros8/4/2 (empty); hex64_floatPair ({21.0488,547.3} → top kinds.len()==2, kinds[0]==Float, strength>=3); hex64_intPair ({42,99} → top len 2, kinds[0] Int32|UInt32); hex64_utf8 ("IChooseY" → some suggestion UTF8 strength>=3); hex64_pointer (0x00007FF6A0B01000 LE bytes → some Pointer64); hex32_float (0x41A86600 LE → top Float strength>=2); hex32_int_monotonic (0x0000BFFC + monotonic history min 16/max 49148 → top Int32|UInt32 strength>=2); hex16_uint (0x005F → top Int16|UInt16); hex8_uint (1 → top UInt8); formatHint_single ("float"); formatHint_split ("float\u{00D7}2"); denormalRejected (0x00000001 → if top is single, NOT Float).

**Test count target:** ~88 core asserts (test_core minus 5 provider tests) + 18
typeinfer asserts (20 minus 2 benchmarks) ≈ **106 `#[test]` assertions**. Match the
oracle's all-green status.

---

## ORDERED, INDEPENDENTLY-VERIFIABLE IMPLEMENTATION STEPS

Each step compiles + passes its tests under `cargo test --no-default-features -p reclass core::`
before moving on.

1. **`kind.rs`** — `NodeKind`, `KindFlags` consts, `KindMeta`, `K_KIND_META` (31 rows, with `const _ assert`), all kind helpers (§2). Tests: Group A. *Foundation; everything depends on it.*
2. **`node.rs`** — `BitfieldMember`, `Node` (+ `Default`), `byte_size`, class-keyword helpers, `K_MAX_ARRAY_LEN` (§3). Tests: byte_size + class-keyword subset of Group B. (Defer `total_byte_size` until step 5.)
3. **`history.rs`** — `ValueHistory` + `now_ms` (§7). Tests: Group I. *Self-contained.*
4. **`line.rs`** — enums/consts/structs (`LineKind`,`ChipKind`,`Marker`,`LineChip`,`LineMeta`,`LayoutInfo`,`ComposeResult`,`ViewState`,`ColumnSpan`,`EditTarget`,`LineGeometry`), sel-id encoders, column constants, **column-span helpers** (§8). Tests: Group J + sel-id round-trip smoke tests. *Pure; no tree dep.*
5. **`tree.rs` (part 1)** — `NodeTree` struct + caches, `add_node`, `reserve_id`, `index_of_id`, `children_of`, `invalidate_id_cache`, generation; then `depth_of`, `compute_offset`, `absolute_address`, `subtree_indices`, `struct_span`, `total_byte_size` (wire into `node.rs`). Tests: Group C. *Core algorithms; cycle-safety is the risk — verify the cycle tests early.*
6. **`tree.rs` (part 2)** — `field_path`, `node_id_for_path`, `root_class_names`, `find_overlaps`, `validate` + `ValidateReport`, `normalize_prefer_ancestors/descendants`. Tests: Groups D, E, F, G.
7. **`json.rs`** — lenient parse helpers + `Node::to_json/from_json` + `NodeTree::to_json/from_json` + `Bookmark` serde (§5). Tests: Group B (json round-trip) + Group H. *Highest-risk for parity (string-ids, hex base, collapsed-always-true, isHelper fallback) — test exhaustively.*
8. **`command.rs`** — `OffsetAdj` + `enum Command` (§9). No tests in this subsystem (verified later by controller); just compile.
9. **`commontypes.rs`** — `CommonField`/`CommonType`, `COMMON_TYPES` (~50 verbatim), `find_common_type` (§10). Add a sanity `#[test]` that `COMMON_TYPES.len() == 50` and `find_common_type("GUID")` Some / `find_common_type("nope")` None, and that each type's last field offset+size ≤ total_size (cheap invariant check). Verified deeply by `test_generator`/`test_controller` later.
10. **`typeinfer.rs`** — all `detail` helpers + `infer_types` + `format_hint` (§11). Tests: Group K. *Bit-exact thresholds — the dominance rule and integer division are the traps.*
11. **`hooks.rs`** + **`mod.rs`** — `CoreHooks` (§5.3) and re-exports. No tests; wires the module.

---

## APPENDIX — parity pitfalls (one-line reminders)
- `kTreeIndent = 2` (not 3 — tests-catalog typo).
- `Int128`/`UInt128` align **8**, `Hex128` align **16**.
- ids/refId/parentId/nextId/enum-value = **decimal strings**; baseAddress = **hex string, no 0x**.
- `collapsed` always loads `true`; `isStatic` falls back to legacy `isHelper`.
- `compute_offset` is `i64`, can be negative; never cast neg→u64 (use `absolute_address`).
- `byte_size()` is 0 for non-bitfield Struct & Array-of-container; bitfield→container width (default 4).
- `struct_span`/`find_overlaps` skip `is_static`; `find_overlaps` also skips root (parent 0), unions, ≤0-span; breaks early on sorted scan; **stable sort by start**.
- `heat_level` keys on total `count`, not unique.
- typeinfer: `add_candidate` gate 25; strength bins 25/50/85; dominance needs `top >= second*3/2` **AND** `top-second >= 10`; integer division; `from_le_bytes`; len uses exact `==` for 4/2/1 but `>=8` for whole8 and `>=4` for split.
- `kind_from_string` miss → Hex8; `kind_from_type_name` miss → (Hex8, false).
- Column-span text helpers index by **char** (QChar/UTF-16 semantics), not byte — special glyphs ▾/▸/→/× are multi-byte in UTF-8.
