# PORTING SPEC — `generator` (C/C++/Rust/#define/C#/Python source emission)

Function-level porting spec to drive the faithful Rust implementation of
`src/generator.{h,cpp}` (1748 lines + 90 header lines, ~70 tests).

**Read-with-this:** `_design/understand/generator.md` (behavioral map — the
authoritative narrative; this spec is the implementation contract that derives
from it), `_design/ARCHITECTURE.md` (§3 module table: `generator` → `src/generator.rs`,
always-on, no extra deps), `_design/crate_selection.md` (no crate needed — pure
port over `std`), `_oracle/RESULTS.md` (`test_generator → 76/0/0 PASS`).

---

## 0. Target crate / module

- **Crate:** the single `reclass` package.
- **Module:** `src/generator.rs` (already exists as a SKELETON with `CodeFormat`,
  `CodeScope`, `TypeAliases`, and `code_format_name`/`code_scope_name`/`render_code`/
  `render_code_tree`/`render_code_all` as `todo!()`). This spec fills it in and
  adds the per-backend functions + `code_format_file_filter`.
- **Feature gate:** none — the generator is always compiled (it has no heavy deps,
  no OS code, no UI). It depends only on `crate::core`.
- **Tests:** unit tests live in a `#[cfg(test)] mod tests` at the bottom of
  `src/generator.rs` (mirroring how `core::*` modules embed their tests), runnable
  with `cargo test --no-default-features generator`. (The C++ `tests/test_generator.cpp`
  is a standalone QtTest binary; in Rust the cheapest 1:1 mapping is in-module
  `#[test]`s because every test constructs a `NodeTree` in-process and calls a
  `render*` function — no integration-test harness needed. An optional
  `tests/generator.rs` integration file may mirror them for the public API.)

### Dependencies on `crate::core` (all already implemented — verified)

| C++ symbol | Rust symbol (path) | Status |
|---|---|---|
| `NodeKind` (`core.h:24`) | `core::NodeKind` (`core/kind.rs`) | ✅ present, same discriminant order |
| `Node`, fields | `core::Node` (`core/node.rs`) | ✅ snake_case fields: `struct_type_name`, `class_keyword`, `parent_id`, `is_static`, `offset_expr`, `array_len`, `str_len`, `ref_id`, `element_kind`, `enum_members: Vec<(String,i64)>`, `bitfield_members: Vec<BitfieldMember>` |
| `BitfieldMember{name,bitOffset,bitWidth}` | `core::BitfieldMember{ name, bit_offset:u8, bit_width:u8 }` | ✅ |
| `Node::byteSize()` | `Node::byte_size() -> i32` | ✅ |
| `Node::resolvedClassKeyword()` | `Node::resolved_class_keyword() -> &str` | ✅ |
| `Node::isBitfield()` | `Node::is_bitfield() -> bool` (raw keyword == "bitfield") | ✅ |
| `NodeTree::nodes` | `NodeTree::nodes: Vec<Node>` (pub) | ✅ |
| `NodeTree::pointerSize` | `NodeTree::pointer_size: i32` (pub) | ✅ |
| `NodeTree::indexOfId(id)` → `int` (-1 miss) | `NodeTree::index_of_id(id) -> i32` (-1 miss) | ✅ |
| `NodeTree::structSpan(id, &childMap)` | `NodeTree::struct_span(id) -> i32` | ✅ — **NOTE:** the Rust `struct_span` takes **no childMap arg**; it uses the tree's own `children_of` cache. This is behaviorally identical to the C++ passing `&ctx.childMap` because both maps are `parentId → child indices in insertion order`. Do **not** add a childMap parameter. |
| `isHexNode(k)` (`core.h:138`) | `core::is_hex_node(k) -> bool` | ✅ |
| `isPointerKind(k)` (`core.h:153`) | `core::is_pointer_kind(k) -> bool` | ✅ |
| `sizeForKind(k)` | `core::size_for_kind(k) -> i32` | ✅ |

No `core` changes are required for this port. Everything the generator reads is
already public.

---

## 1. Public API surface (what `generator.rs` must export)

All return `String` (was `QString`), all take `&NodeTree`, none mutate the tree.
`type_aliases: Option<&TypeAliases>` (= `Option<&HashMap<NodeKind, String>>`,
default `None`); `emit_asserts: bool`.

```rust
pub type TypeAliases = HashMap<NodeKind, String>;   // already in skeleton

// ── enums (already in skeleton; keep as-is) ──
#[repr(i32)] pub enum CodeFormat { CppHeader=0, RustStruct, DefineOffsets, CSharpStruct, PythonCtypes }
#[repr(i32)] pub enum CodeScope  { Current=0, WithChildren, FullSdk }
// NOTE: C++ has a trailing `_Count` variant in each enum. Rust does NOT need it
// (no array-sized-by-enum usage); omit `_Count`. If parity of the integer count
// is ever needed, add `Count` last — but no test references `_Count`.

// ── name/filter helpers (return &'static str) ──
pub fn code_format_name(fmt: CodeFormat) -> &'static str;          // generator.cpp:1408
pub fn code_format_file_filter(fmt: CodeFormat) -> &'static str;   // generator.cpp:1419  (NOT in skeleton — ADD)
pub fn code_scope_name(scope: CodeScope) -> &'static str;          // generator.cpp:1430

// ── dispatchers ──
pub fn render_code(fmt, &tree, root_struct_id: u64, aliases: Option<&TypeAliases>, emit_asserts: bool) -> String;
pub fn render_code_tree(fmt, &tree, root_struct_id: u64, aliases, emit_asserts) -> String;
pub fn render_code_all(fmt, &tree, aliases, emit_asserts) -> String;

// ── per-backend entry points ──
pub fn render_cpp(&tree, root: u64, aliases: Option<&TypeAliases>, emit_asserts: bool) -> String;
pub fn render_cpp_tree(&tree, root, aliases, emit_asserts) -> String;
pub fn render_cpp_all(&tree, aliases, emit_asserts) -> String;

pub fn render_rust(&tree, root, aliases, emit_asserts) -> String;
pub fn render_rust_tree(&tree, root, aliases, emit_asserts) -> String;
pub fn render_rust_all(&tree, aliases, emit_asserts) -> String;

pub fn render_defines(&tree, root) -> String;        // no aliases / no asserts
pub fn render_defines_tree(&tree, root) -> String;
pub fn render_defines_all(&tree) -> String;

pub fn render_csharp(&tree, root, aliases, emit_asserts) -> String;
pub fn render_csharp_tree(&tree, root, aliases, emit_asserts) -> String;
pub fn render_csharp_all(&tree, aliases, emit_asserts) -> String;

pub fn render_python(&tree, root) -> String;         // no aliases / no asserts
pub fn render_python_tree(&tree, root) -> String;
pub fn render_python_all(&tree) -> String;

pub fn render_null(&tree, root: u64) -> String;      // always String::new()
```

> **C++ default args:** the header gives `typeAliases=nullptr, emitAsserts=false`.
> Rust has no default args; callers pass `None, false`. The behavioral map and
> tests call e.g. `renderCpp(tree, id)` (2 args). Keep the full 4-arg signature;
> the dispatch helpers and tests pass `None, false` explicitly. (Do **not** add
> builder overloads — keep it 1:1.)

---

## 2. Item-by-item mapping (C++ → Rust)

### 2.1 Free helpers / file-level constants

| C++ (`generator.cpp`) | Rust | Notes |
|---|---|---|
| `static const QChar kCommentMarker = QChar(0x01)` (164) | `const COMMENT_MARKER: char = '\u{1}'` | internal column-alignment sentinel. |
| `sanitizeIdent(const QString&)` (13–24) | `fn sanitize_ident(name: &str) -> String` | §3.1 algorithm. **Unicode-aware** (`char::is_alphanumeric`), not ASCII-only. |
| `cTypeName(NodeKind)` (28–61) | `fn c_type_name(k: NodeKind) -> &'static str` | static table; see §4 type tables. |
| `offsetComment(int, bool=false)` (166–170) | `fn offset_comment(offset: i32, is_sizeof: bool) -> String` | returns `"\u{1}// 0xHEX"` or `"\u{1}// sizeof 0xHEX"`; HEX = **uppercase** `format!("{:X}", offset)`. |
| `indent(int)` (172–174) | `fn indent(depth: i32) -> String` | `" ".repeat((depth*4) as usize)`. |
| `buildChildMap(const NodeTree&)` (461–466) | `fn build_child_map(tree) -> HashMap<u64, Vec<usize>>` | parentId → indices, **insertion order** (no sort). Root under key `0`. Use `ahash`/std `HashMap`; iteration order of the map itself never leaks (see §6). |
| `alignComments(const QString&)` (472–501) | `fn align_comments(raw: &str) -> String` | §3.2 two-pass column alignment. |
| `collectReachableStructs(tree,childMap,rootId)` (1368–1402) | `fn collect_reachable_structs(tree, child_map, root: u64) -> Vec<u64>` | §3.3 post-order DFS. |
| `rustTypeName` (507–540) | `fn rust_type_name(k) -> &'static str` | §4. |
| `csTypeName` (852–885) | `fn cs_type_name(k) -> &'static str` | §4. |
| `pyTypeName` (1091–1124) | `fn py_type_name(k) -> &'static str` | §4. **Note:** Hex128/UInt128 → `"ctypes.c_uint8 * 16"`, Int128 → `"ctypes.c_int8 * 16"` (already a multiplied form). |

### 2.2 `GenContext` (generator.cpp:65–155) → `struct GenContext<'a>`

The shared mutable state. C++ uses aggregate init in field order; Rust uses a
constructor. Layout:

```rust
struct GenContext<'a> {
    tree: &'a NodeTree,
    child_map: HashMap<u64, Vec<usize>>,
    emitted_type_names: HashSet<String>,    // dedup by emitted name
    emitted_ids: HashSet<u64>,              // dedup by id
    visiting: HashSet<u64>,                 // cycle guard
    forward_declared: HashSet<u64>,         // C++-only forward-decl guard
    output: String,
    pad_counter: i32,                       // shared across the whole render
    type_aliases: Option<&'a TypeAliases>,
    emit_asserts: bool,
    name_by_id: HashMap<u64, String>,       // populated by assign_unique_names()
}
```
Construction helper (replaces the 10-positional aggregate init repeated in every
public fn):
```rust
impl<'a> GenContext<'a> {
    fn new(tree: &'a NodeTree, aliases: Option<&'a TypeAliases>, emit_asserts: bool) -> Self {
        GenContext { tree, child_map: build_child_map(tree),
            emitted_type_names: HashSet::new(), emitted_ids: HashSet::new(),
            visiting: HashSet::new(), forward_declared: HashSet::new(),
            output: String::new(), pad_counter: 0, type_aliases: aliases,
            emit_asserts, name_by_id: HashMap::new() }
    }
}
```

| C++ method | Rust method | Notes |
|---|---|---|
| `prepare()` (84) | omit, or `fn prepare(&mut self){}` no-op | pure `output.reserve(...)` optimization; not observable. **Do not** replicate the size — irrelevant. |
| `prepareChildren(structId)` (87–97) → `pair<QVector<int>,QVector<int>>` | `fn prepare_children(&self, struct_id: u64) -> (Vec<usize>, Vec<usize>)` | `(regular, static)`. Iterate `child_map[struct_id]` in insertion order; push static indices into the 2nd vec, others into the 1st; then `regular.sort_by_key(|&i| self.tree.nodes[i].offset)`. **`sort_by_key` is stable in Rust** — see §6 note on equal-offset siblings. |
| `uniquePadName()` (99–101) | `fn unique_pad_name(&mut self) -> String` | `let n = self.pad_counter; self.pad_counter += 1; format!("_pad{:04x}", n)` — **lowercase**, 4-digit, zero-padded. e.g. `_pad0000`. |
| `cType(NodeKind)` (104–111) | `fn c_type(&self, k) -> String` | alias lookup first (non-empty wins), else `c_type_name(k).to_string()`. Returns `String` because aliases are owned `String`. |
| `structName(const Node&)` (114–118) | `fn struct_name(&self, n: &Node) -> String` | `if !n.struct_type_name.is_empty() { sanitize_ident(&n.struct_type_name) } else if !n.name.is_empty() { sanitize_ident(&n.name) } else { format!("anon_{:x}", n.id) }` — **lowercase** hex, no padding. |
| `nameFor(const Node&)` (123–127) | `fn name_for(&self, n: &Node) -> String` | `self.name_by_id.get(&n.id).cloned().unwrap_or_else(|| self.struct_name(n))`. |
| `assignUniqueNames()` (135–154) | `fn assign_unique_names(&mut self)` | §3.4 disambiguation pre-pass. |

> **Aliasing/borrow note:** the C++ takes `GenContext&` everywhere and mutates
> `output`/`pad_counter`/the guard sets while also reading `tree`/`child_map`/
> `name_by_id`. In Rust this is fine because `tree` is a shared `&'a` ref held
> in the struct and the mutated fields are disjoint. Emit functions take
> `&mut GenContext`. Where you need both an immutable read of `self.tree`/
> `self.child_map` and a push to `self.output` in one statement, **compute the
> string into a local first**, then `self.output.push_str(&local)` — this avoids
> overlapping borrows that the C++ gets for free. (e.g. resolve `name_for(child)`
> into a `String` before formatting the field line.)

### 2.3 Per-backend emit functions

Each backend is a triplet `emit*Struct` / `emit*StructBody` (+ `emitField`/`emitRustField`)
plus the public `render*`. C# and Python `*Body` take no `depth` cursor for C#
(it's `[FieldOffset]`-based) but Python has fixed 8-space indent.

| C++ static fn | Rust fn | §ref |
|---|---|---|
| `emitField` (176–221) | `fn emit_field(ctx, node, depth, base_offset) -> String` | §5 C/C++ |
| `emitStructBody` (225–377) | `fn emit_struct_body(ctx: &mut GenContext, struct_id: u64, is_union: bool, depth: i32, base_offset: i32)` | §5 |
| `emitStruct` (381–457) | `fn emit_struct(ctx, struct_id: u64)` | §5 |
| `emitRustField` (554–596) | `fn emit_rust_field(ctx, node, depth, base_offset) -> String` | §6-rust |
| `emitRustStructBody` (598–729) | `fn emit_rust_struct_body(ctx, struct_id, is_union, depth, base_offset)` | |
| `emitRustStruct` (731–787) | `fn emit_rust_struct(ctx, struct_id)` | |
| `emitDefinesForStruct` (793–846) | `fn emit_defines_for_struct(ctx, struct_id, prefix: &str, base_offset: i32)` | §7 |
| `emitCSharpStructBody` (899–1033) | `fn emit_csharp_struct_body(ctx, struct_id, is_union, depth, base_offset)` | §8 |
| `emitCSharpStruct` (1035–1085) | `fn emit_csharp_struct(ctx, struct_id)` | |
| `emitPythonStructBody` (1129–1300) | `fn emit_python_struct_body(ctx, struct_id, is_union, base_offset)` | §9 |
| `emitPythonStruct` (1302–1359) | `fn emit_python_struct(ctx, struct_id)` | |

---

## 3. Tricky-logic pseudocode

### 3.1 `sanitize_ident` (generator.cpp:13–24)

```
fn sanitize_ident(name: &str) -> String:
    if name.is_empty(): return "unnamed"
    out = String
    for c in name.chars():
        if c.is_alphanumeric() || c == '_': out.push(c)
        else: out.push('_')
    // first-char fixup: prepend '_' unless first is a letter or '_'
    first = out.chars().next().unwrap()   // out is non-empty here (name non-empty)
    if !first.is_alphabetic() && first != '_':
        out.insert(0, '_')
    return out
```
- `QChar::isLetterOrNumber()` is Unicode-aware → `char::is_alphanumeric()`.
- `QChar::isLetter()` → `char::is_alphabetic()`.
- C++ indexes `out[0]` unconditionally; safe because `name` non-empty ⇒ `out`
  non-empty (every input char maps to exactly one output char). Replicate by
  reading `out.chars().next()`.
- Test `testNameSanitization`: `"my struct-name"` → `my_struct_name`;
  `"field with spaces"` → `field_with_spaces`.

### 3.2 `align_comments` (generator.cpp:472–501)

```
fn align_comments(raw: &str) -> String:
    lines: Vec<&str> = raw.split('\n').collect()
    // Pass 1: max byte-index of COMMENT_MARKER over all lines
    max_code = 0
    for line in &lines:
        if let Some(pos) = line.find(COMMENT_MARKER):    // byte index
            max_code = max(max_code, pos)
    // Pass 2
    result = String
    for (i, line) in lines.iter().enumerate():
        if i > 0: result.push('\n')
        match line.find(COMMENT_MARKER):
            Some(pos):
                result.push_str(&line[..pos])
                pad = max(max_code - pos + 1, 1)
                result.push_str(&" ".repeat(pad))
                result.push_str(&line[pos + marker_len..])   // skip the 1-byte marker
            None:
                result.push_str(line)
    return result
```
- `COMMENT_MARKER` (`'\u{1}'`) is 1 byte in UTF-8, so `pos + 1` skips it. Use
  `pos + COMMENT_MARKER.len_utf8()` for clarity (== 1).
- **Caution:** `str::find(char)` returns a **byte** offset; `QString::indexOf`
  returns a UTF-16 code-unit offset. Identifiers/types emitted before the marker
  are ASCII in all tested paths, so byte==char==UTF-16-unit. For pathological
  non-ASCII field names the column could differ from Qt by a few spaces, but no
  test exercises that and the column count is cosmetic. Document; do not special-case.
- `testAlignCommentsNoMarkers`: marker-free input passes through unchanged (the
  empty-struct header/footer have no markers on most lines).

### 3.3 `collect_reachable_structs` (generator.cpp:1368–1402)

Post-order DFS (dependencies first, root last). Recursion via a helper fn or an
explicit stack; recursion is simplest and matches the C++ `std::function` closure.

```
fn collect_reachable_structs(tree, child_map, root: u64) -> Vec<u64>:
    result = Vec::new(); visited = HashSet::new()
    walk(tree, child_map, root, &mut visited, &mut result)
    return result

fn walk(tree, child_map, id, visited, result):
    if !visited.insert(id): return            // already visited
    idx = tree.index_of_id(id); if idx < 0: return
    node = &tree.nodes[idx]
    if node.kind != Struct: return
    for &ci in child_map.get(&id).unwrap_or(&empty):
        child = &tree.nodes[ci]
        if child.kind == Struct && !child.struct_type_name.is_empty(): walk(child.id)
        if (child.kind == Pointer32 || Pointer64) && child.ref_id != 0:   walk(child.ref_id)
        if child.kind == Array:
            for &ak in child_map.get(&child.id).unwrap_or(&empty):
                if tree.nodes[ak].kind == Struct: walk(tree.nodes[ak].id)
    result.push(id)
```
- `visited.insert(id)` returns `false` if already present → use as the guard (one
  lookup). Anonymous inline structs (empty `struct_type_name`) are NOT recursed
  into as separate types (they're emitted inline by the parent).
- Borrow: `walk` needs `&tree`, `&child_map`, `&mut visited`, `&mut result`. Pass
  them as params (free fn), not closures-capturing-locals, to satisfy the borrow
  checker cleanly.

### 3.4 `assign_unique_names` (generator.cpp:135–154)

```
fn assign_unique_names(&mut self):
    used: HashSet<String> = {}
    // local closure logic, inlined as a helper that takes &mut used + &mut name_by_id:
    fn assign(used, name_by_id, id, base):
        name = base.clone(); suffix = 2
        while used.contains(&name):
            name = format!("{}_v{}", base, suffix); suffix += 1
        used.insert(name.clone())
        name_by_id.insert(id, name)
    // Pass 1: roots (parent_id==0 && kind==Struct), in nodes insertion order
    for n in &self.tree.nodes:
        if n.parent_id != 0 || n.kind != Struct: continue
        assign(.., n.id, self.struct_name(n))
    // Pass 2: nested named structs (parent_id!=0 && kind==Struct && !struct_type_name.is_empty())
    for n in &self.tree.nodes:
        if n.parent_id == 0 || n.kind != Struct: continue
        if n.struct_type_name.is_empty(): continue
        assign(.., n.id, self.struct_name(n))
```
- Borrow: `self.struct_name(n)` borrows `&self.tree` immutably while we want
  `&mut self.name_by_id`. Resolve by snapshotting: iterate over **indices**
  `0..self.tree.nodes.len()`, compute `base = self.struct_name(&self.tree.nodes[i])`
  into a `String`, then mutate `self.name_by_id`/`used` (which are not `tree`).
  `used` can be a local `HashSet` outside the struct.
- Roots get priority/stable naming; collisions become `Shared`, `Shared_v2`,
  `Shared_v3`, … Anonymous inline nested structs are intentionally skipped.
- Test `testDuplicateTypeNameDisambiguation`: two root structs both named
  `"Shared"` → output contains `struct Shared\n{` and `struct Shared_v2\n{`,
  with distinct field types (`uint32_t val;` and `uint64_t val;`).

---

## 4. Type-name tables (exact, copy verbatim — these are the parity surface)

Implement each as a `match k { … }` returning `&'static str`. **Hex128 mapping
differs per backend; preserve exactly.**

### `c_type_name` (generator.cpp:28–61)
```
Hex8→"uint8_t" Hex16→"uint16_t" Hex32→"uint32_t" Hex64→"uint64_t" Hex128→"uint8_t"
Int8→"int8_t" Int16→"int16_t" Int32→"int32_t" Int64→"int64_t" Int128→"__int128"
UInt8→"uint8_t" UInt16→"uint16_t" UInt32→"uint32_t" UInt64→"uint64_t" UInt128→"unsigned __int128"
Float16→"_Float16" Float→"float" Double→"double" Bool→"bool"
Pointer32→"uint32_t" Pointer64→"uint64_t" FuncPtr32→"uint32_t" FuncPtr64→"uint64_t"
Vec2/Vec3/Vec4/Mat4x4→"float" UTF8→"char" UTF16→"wchar_t" _→"uint8_t"
```

### `rust_type_name` (507–540)
```
Hex8/UInt8→"u8" Hex16/UInt16→"u16" Hex32/UInt32→"u32" Hex64/UInt64→"u64" Hex128/UInt128→"u128"
Int8→"i8" Int16→"i16" Int32→"i32" Int64→"i64" Int128→"i128"
Float16→"f16" Float→"f32" Double→"f64" Bool→"bool"
Pointer32/FuncPtr32→"u32" Pointer64/FuncPtr64→"u64"
Vec2/3/4/Mat4x4→"f32" UTF8→"u8" UTF16→"u16" _→"u8"
```

### `cs_type_name` (852–885)
```
Hex8→"byte" Hex16→"ushort" Hex32→"uint" Hex64→"ulong" Hex128→"byte"
Int8→"sbyte" Int16→"short" Int32→"int" Int64→"long" Int128→"Int128"
UInt8→"byte" UInt16→"ushort" UInt32→"uint" UInt64→"ulong" UInt128→"UInt128"
Float16→"Half" Float→"float" Double→"double" Bool→"bool"
Pointer32/FuncPtr32→"uint" Pointer64/FuncPtr64→"ulong"
Vec2/3/4/Mat4x4→"float" UTF8→"byte" UTF16→"char" _→"byte"
```

### `py_type_name` (1091–1124)
```
Hex8→"ctypes.c_uint8" Hex16→"ctypes.c_uint16" Hex32→"ctypes.c_uint32" Hex64→"ctypes.c_uint64"
Hex128→"ctypes.c_uint8 * 16"
Int8→"ctypes.c_int8" Int16→"ctypes.c_int16" Int32→"ctypes.c_int32" Int64→"ctypes.c_int64"
Int128→"ctypes.c_int8 * 16"
UInt8→"ctypes.c_uint8" UInt16→"ctypes.c_uint16" UInt32→"ctypes.c_uint32" UInt64→"ctypes.c_uint64"
UInt128→"ctypes.c_uint8 * 16"
Float16→"ctypes.c_uint16" Float→"ctypes.c_float" Double→"ctypes.c_double" Bool→"ctypes.c_bool"
Pointer32/FuncPtr32→"ctypes.c_uint32" Pointer64/FuncPtr64→"ctypes.c_uint64"
Vec2/3/4/Mat4x4→"ctypes.c_float" UTF8→"ctypes.c_char" UTF16→"ctypes.c_wchar" _→"ctypes.c_uint8"
```

### Alias resolution
- C/Rust/C# have a context wrapper (`c_type` / `rust_type` / `cs_type`) that
  checks `type_aliases` first: `if let Some(m)=aliases { if let Some(v)=m.get(&k) { if !v.is_empty() { return v.clone() } } }` then falls to the static table.
- **Python does NOT consult aliases** — `py_type_name` is called directly and
  `render_python*` pass no aliases. Keep `py_type_name` as a free fn taking only `k`.

---

## 5. C/C++ backend behavior (the canonical one)

### `offset_comment(offset, is_sizeof)`
- `is_sizeof=false` → `format!("\u{1}// 0x{:X}", offset)`.
- `is_sizeof=true`  → `format!("\u{1}// sizeof 0x{:X}", offset)`.
- Uppercase hex, **no** `0x`-zero-padding from the number (uses `{:X}`).

### `emit_field(ctx, node, depth, base_offset) -> String`
Build `ind = indent(depth)`,
`name = sanitize_ident(if node.name.is_empty() { &format!("field_{:02x}", node.offset) } else { &node.name })`
(field-fallback hex is **lowercase, 2-digit, zero-padded**),
`oc = offset_comment(base_offset + node.offset, false)`. Then `match node.kind`:
```
Vec2 → "{ind}float {name}[2];{oc}"      (type via ctx.c_type(Float))
Vec3 → "{ind}float {name}[3];{oc}"
Vec4 → "{ind}float {name}[4];{oc}"
Mat4x4 → "{ind}float {name}[4][4];{oc}"
UTF8  → "{ind}char {name}[{strLen}];{oc}"     (type via ctx.c_type(UTF8))
UTF16 → "{ind}wchar_t {name}[{strLen}];{oc}"  (type via ctx.c_type(UTF16))
Pointer32|Pointer64 →
    if ref_id != 0 && index_of_id(ref_id) >= 0:
        target = ctx.name_for(&tree.nodes[refIdx])
        "{ind}struct {target}* {name};{oc}"
    else if is_native_ptr(node.kind, tree.pointer_size):
        "{ind}void* {name};{oc}"
    else: "{ind}{ctx.c_type(kind)} {name};{oc}"
FuncPtr32|FuncPtr64 → "{ind}void (*{name})();{oc}"
_ → "{ind}{ctx.c_type(kind)} {name};{oc}"
```
where `is_native_ptr(kind, psize) = (kind==Pointer32 && psize<=4) || (kind==Pointer64 && psize>=8)`.
> The `c_type(Float)` etc. calls mean Vec/Mat element type is alias-overridable;
> keep using `ctx.c_type(NodeKind::Float)` not a literal `"float"`.

### `emit_struct_body(ctx, struct_id, is_union, depth, base_offset)`
1. `idx = index_of_id(struct_id); if idx < 0 { return }`.
2. `struct_size = tree.struct_span(struct_id)`. `ind = indent(depth)`.
3. `(children, static_idxs) = ctx.prepare_children(struct_id)`.
4. Closure `emit_pad_run(ctx, rel_offset, size)`: if `size > 0`:
   `output += "{ind}uint8_t {ctx.unique_pad_name()}[0x{SIZE:X}];{offset_comment(base_offset+rel_offset)}\n"`.
   (SIZE uppercase hex.) — In Rust, write this as a small helper method on ctx or
   a local fn taking `&mut ctx` (not a borrowing closure, since it mutates
   `pad_counter` + `output`).
5. Cursor loop `cursor=0, i=0; while i < children.len()`:
   - `child = &tree.nodes[children[i]]`.
   - `child_size = if matches!(child.kind, Struct|Array) { tree.struct_span(child.id) } else { child.byte_size() }`.
   - **Gap/overlap** (skip if `is_union`):
     - `child.offset > cursor` → `emit_pad_run(cursor, child.offset - cursor)`.
     - `child.offset < cursor` → append
       `"{ind}// WARNING: overlap at offset 0x{X} (previous field ends at 0x{X})\n"`
       with absolute offsets `base_offset+child.offset` and `base_offset+cursor` (uppercase hex).
   - **Hex-run collapse** (`is_hex_node(child.kind)`):
     ```
     run_start = child.offset; run_end = child.offset + child_size; j = i+1
     while j < children.len():
        next = &tree.nodes[children[j]]
        if !is_hex_node(next.kind): break
        next_size = next.byte_size()
        if next.offset < run_end: break          // overlap breaks the run
        run_end = next.offset + next_size; j += 1
     emit_pad_run(run_start, run_end - run_start)
     cursor = run_end; i = j; continue
     ```
   - **Struct child** (`child.kind == Struct`):
     - **Bitfield** (`child.is_bitfield() && !child.bitfield_members.is_empty()`):
       `bf_type = ctx.c_type(child.element_kind)` (if empty → `"uint32_t"`; with a
       valid closed enum it's never empty, but keep the guard for parity).
       `field_name = if child.name.is_empty() { "".into() } else { format!(" {}", sanitize_ident(&child.name)) }`.
       Emit `"{ind}struct\n"`, `"{ind}{{\n"`, then per member at `bf_ind = indent(depth+1)`:
       `"{bf_ind}{bf_type} {sanitize_ident(m.name)} : {m.bit_width};{offset_comment(base_offset+child.offset)}\n"`,
       then `"{ind}}}{field_name};{offset_comment(base_offset+child.offset)}\n"`.
     - **Anonymous** (`child.struct_type_name.is_empty()`):
       `kw = child.resolved_class_keyword()`; emit `"{ind}{kw}\n"`, `"{ind}{{\n"`;
       recurse `emit_struct_body(ctx, child.id, kw=="union", depth+1, base_offset+child.offset)`;
       `field_name` as above; emit `"{ind}}}{field_name};{offset_comment(...)}\n"`.
       (`testInlineAnonymousStruct`: contains `union\n    {`, `struct _LIST_ENTRY ListEntry;`, no `anon_`.)
     - **Named:** `kw = child.resolved_class_keyword(); if kw=="enum" && child.enum_members.is_empty() { kw="struct" }`.
       `type_name = ctx.name_for(child); field_name = sanitize_ident(&child.name)`.
       Emit `"{ind}{kw} {type_name} {field_name};{offset_comment(...)}\n"`.
       (Opaque named child → reference only, **no body, no padding** in Current scope.)
   - **Array child** (`child.kind == Array`):
     `array_kids = ctx.child_map.get(&child.id)`. Find first child that is `Struct`
     → `has_struct_child=true, elem = ctx.name_for(thatStruct)`.
     `field_name = sanitize_ident(&child.name)`.
     - struct element: `"{ind}struct {elem} {field_name}[{child.array_len}];{oc}\n"`.
     - primitive: `"{ind}{ctx.c_type(child.element_kind)} {field_name}[{child.array_len}];{oc}\n"`.
   - **Else (primitive):** `output += emit_field(ctx, child, depth, base_offset); output += "\n"`.
   - `child_end = child.offset + child_size; cursor = cursor.max(child_end); i += 1`.
6. **Tail padding** (skip union): if `cursor < struct_size` → `emit_pad_run(cursor, struct_size - cursor)`.
7. **Static fields:** for each `si` in `static_idxs`:
   `sf_type = if sf.struct_type_name.is_empty() { ctx.c_type(sf.kind) } else { sf.struct_type_name.clone() }`;
   `output += "{ind}// static: {sf_type} {sanitize_ident(sf.name)} @ {sf.offset_expr}\n"`.

### `emit_struct(ctx, struct_id)`
1. `if ctx.emitted_ids.contains(&id) { return }`.
2. `if ctx.visiting.contains(&id) { return }` (cycle). `ctx.visiting.insert(id)`.
3. `idx = index_of_id(id); if idx<0 { ctx.visiting.remove(&id); return }`.
4. `node = &tree.nodes[idx]`. If `node.kind != Struct && != Array` → unvisit, return.
   If `node.kind == Array` → unvisit, return. (Net: only `Struct` proceeds.)
5. `type_name = ctx.name_for(node)`. If `ctx.emitted_type_names.contains(&type_name)`
   → `ctx.emitted_ids.insert(id); unvisit; return`.
6. `emitted_ids.insert(id); emitted_type_names.insert(type_name.clone())`.
7. **Forward declarations:** for each `ci` in `ctx.child_map[id]`:
   `child = &tree.nodes[ci]`; if `is_pointer_kind(child.kind) && child.ref_id != 0`:
   `ri = index_of_id(child.ref_id)`; if `ri>=0 && !emitted_ids.contains(&ref_id) && !forward_declared.contains(&ref_id)`:
   `output += format!("struct {};\n", ctx.name_for(&tree.nodes[ri])); forward_declared.insert(ref_id)`.
   (`testForwardDeclarationForPointerTarget`: `struct TargetB;` + field `struct TargetB* ptr_to_b`.)
8. `struct_size = tree.struct_span(id)`. `kw = node.resolved_class_keyword()`.
9. **Enum with members** (`kw=="enum" && !node.enum_members.is_empty()`):
   `output += format!("enum {} {{\n", type_name)`; per member
   `output += format!("    {} = {},\n", sanitize_ident(&m.0), m.1)`; `output += "};\n\n"`;
   unvisit; return. (No `static_assert` for enums.)
10. `if kw == "enum" { kw = "struct" }`.
11. `output += format!("{} {}\n{{\n", kw, type_name)`.
12. `emit_struct_body(ctx, id, kw=="union", 1, 0)`.
13. `output += format!("}}{}\n", offset_comment(struct_size, true))`.
14. `if ctx.emit_asserts { output += format!("static_assert(sizeof({0}) == 0x{1:X}, \"Size mismatch for {0}\");\n", type_name, struct_size) }`.
15. `output += "\n"`. `ctx.visiting.remove(&id)`.

### C/C++ public fns (1439–1498)
- `render_cpp(tree, root, aliases, emit_asserts)`:
  `idx = tree.index_of_id(root); if idx<0 { return String::new() }`;
  `if tree.nodes[idx].kind != Struct { return String::new() }`;
  `ctx = GenContext::new(tree, aliases, emit_asserts); ctx.assign_unique_names();`
  `ctx.output += "#pragma once\n#include <cstdint>\n\n"; emit_struct(&mut ctx, root);`
  `align_comments(&ctx.output)`.
- `render_cpp_tree`: same validity + preamble; then
  `for sid in collect_reachable_structs(tree, &ctx.child_map, root) { emit_struct(&mut ctx, sid) }`;
  `align_comments`.
- `render_cpp_all`: NO root validity check; preamble; `roots = ctx.child_map.get(&0).cloned().unwrap_or_default()`;
  `roots.sort_by_key(|&i| tree.nodes[i].offset)`; for each `ri` with `nodes[ri].kind==Struct`
  → `emit_struct(&mut ctx, nodes[ri].id)`; `align_comments`.

---

## 6. Rust backend behavior (differences from C/C++)

`render_rust*` preamble: `"// Generated by Reclass 2027\n\n"`.

### `emit_rust_field`
Same name/oc derivation. `match`:
```
Vec2 → "{ind}pub {name}: [f32; 2],{oc}"  (literal f32 — NOT alias-resolved here)
Vec3 → "[f32; 3]"   Vec4 → "[f32; 4]"   Mat4x4 → "[[f32; 4]; 4]"
UTF8  → "{ind}pub {name}: [u8; {strLen}],{oc}"
UTF16 → "{ind}pub {name}: [u16; {strLen}],{oc}"
Pointer32|64 →
    if ref_id!=0 && refIdx>=0: "{ind}pub {name}: *mut {target},{oc}"
    else if native: "{ind}pub {name}: *mut core::ffi::c_void,{oc}"
    else: "{ind}pub {name}: {rust_type(ctx,kind)},{oc}"
FuncPtr32|64 → "{ind}pub {name}: Option<unsafe extern \"C\" fn()>,{oc}"
_ → "{ind}pub {name}: {rust_type(ctx,kind)},{oc}"
```
> Vec/Mat use literal `f32` (no alias path in the C++ Rust field emitter), unlike
> the C++ backend which routes Vec through `c_type(Float)`. Preserve this asymmetry.

### `emit_rust_struct_body` differences vs C/C++:
- Pad run → `"{ind}pub {pad}: [u8; 0x{SIZE:X}],{oc}\n"`.
- **No overlap WARNING branch** — only the `child.offset > cursor` gap path exists
  (the `< cursor` else-branch is omitted entirely).
- **Bitfield:** Rust has no bitfields → emit a single field + comment:
  `bf_type = rust_type(ctx, child.element_kind)` (empty→`"u32"`);
  `field_name = sanitize_ident(if child.name.is_empty() { &format!("bitfield_{:02x}", child.offset) } else { &child.name })`;
  `bits = members.map(|m| format!("{}:{}", sanitize_ident(&m.name), m.bit_width)).join(", ")`;
  `"{ind}pub {field_name}: {bf_type}, // bits: {bits}{oc}\n"`.
- **Anonymous inline struct:** flatten → `span = tree.struct_span(child.id)`;
  `field_name = sanitize_ident(if name empty { &format!("anon_{:02x}", child.offset) } else { &child.name })`;
  `"{ind}pub {field_name}: [u8; 0x{span:X}],{oc}\n"`.
- **Named struct child:** `kw` enum→struct fixup computed (unused in text);
  `"{ind}pub {sanitize_ident(child.name)}: {ctx.name_for(child)},{oc}\n"`.
- **Array:** struct elem → `"{ind}pub {field_name}: [{elem}; {len}],{oc}\n"`;
  primitive → `"{ind}pub {field_name}: [{rust_type(ctx,element_kind)}; {len}],{oc}\n"`.
- Tail padding same. Static fields → `"{ind}// static: {sf_type} {name} @ {expr}\n"`
  where `sf_type = if struct_type_name empty { rust_type(ctx, sf.kind) } else { struct_type_name }`.

### `emit_rust_struct` differences:
- **Only Struct accepted** (no Array branch; Array silently unvisits+returns).
- **No forward declarations.**
- **Enum with members:** `"#[repr(i64)]\npub enum {type_name} {{\n"` then
  `"    {} = {},\n"` per member then `"}}\n\n"`.
- **Union** (`kw=="union"`): header
  `"#[repr(C)]\n#[derive(Copy, Clone)]\n#[allow(dead_code)]\npub union {type_name} {{\n"`.
- **Struct:** header
  `"#[repr(C)]\n#[derive(Debug)]\n#[allow(dead_code)]\npub struct {type_name} {{\n"`.
- Close: `"}}{}\n"` with `offset_comment(struct_size, true)`.
- `if emit_asserts`: `"const _: () = assert!(core::mem::size_of::<{0}>() == 0x{1:X});\n"`.
- Trailing `"\n"`.

Public `render_rust` / `render_rust_tree` / `render_rust_all` mirror the C/C++
trio but with the Rust preamble and `emit_rust_struct`.

---

## 7. `#define` backend (generator.cpp:793–846, 1557–1602)

`emit_defines_for_struct(ctx, struct_id, prefix: &str, base_offset)` — recursive:
1. `idx = index_of_id; if <0 return`. `node = &nodes[idx]`.
   `type_name = if prefix.is_empty() { ctx.name_for(node) } else { prefix.to_string() }`.
   `kw = node.resolved_class_keyword()`.
2. **Enum with members:** `"// {type_name} (enum)\n"`, then per member
   `"#define {type_name}_{sanitize_ident(m.0)} {m.1}\n"`, then `"\n"`; return.
3. Else: `struct_size = tree.struct_span(id)`; `"// {type_name} (0x{SIZE:X} bytes)\n"`.
4. `children = ctx.child_map.get(&id).cloned().unwrap_or_default()`; `sort_by_key(offset)`.
5. For each child: `if child.is_static { continue }`; `if is_hex_node(child.kind) { continue }`.
   `field_name = sanitize_ident(if name empty { &format!("field_{:02x}", child.offset) } else { &child.name })`;
   `abs = base_offset + child.offset`;
   `"#define {type_name}_{field_name} 0x{abs:X}\n"`.
   Recurse if `child.kind==Struct && !child.struct_type_name.is_empty() && child.class_keyword != "bitfield"`:
   `emit_defines_for_struct(ctx, child.id, &format!("{}_{}", type_name, field_name), abs)`.
6. `"\n"`.

Public fns: preamble `"#pragma once\n#include <cstdint>\n\n"`; build ctx (aliases=None,
emit_asserts=false); `assign_unique_names`; emit; **return `ctx.output` raw — NO
`align_comments`** (the `#define` text contains no `\u{1}` markers; alignment is
skipped). `render_defines_tree` iterates `collect_reachable_structs`;
`render_defines_all` iterates offset-sorted roots. (Tests: `testDefineSimpleStruct`,
`testDefineSkipsHex`, `testDefineAll`, `testDefinesEnumMembers`, `testDefinesOutput`.)

---

## 8. C# backend (generator.cpp:852–1085)

`emit_csharp_struct_body(ctx, struct_id, is_union, depth, base_offset)` — **uses
`[FieldOffset(N)]`, no manual padding, no cursor.** Iterate offset-sorted `children`,
**skip hex nodes** (`if is_hex_node(child.kind) { continue }`). For each at
`abs = base_offset + child.offset`, `name = sanitize_ident(field-fallback)`,
`oc = offset_comment(abs)`:
```
Struct + bitfield → "{ind}[FieldOffset(0x{abs:X})] public {bf_type} {name}; // bits: {bits}{oc}\n"  (bf_type=cs_type(elementKind) | "uint")
Struct + anonymous → "{ind}[FieldOffset(0x{abs:X})] public fixed byte {name}[0x{span:X}];{oc}\n"  (span=struct_span(child.id))
Struct + named → "{ind}[FieldOffset(0x{abs:X})] public {name_for(child)} {name};{oc}\n"
Array struct-elem → "{ind}[FieldOffset(0x{abs:X})] [MarshalAs(UnmanagedType.ByValArray, SizeConst = {len})] public {elem}[] {name};{oc}\n"
Array primitive → "{ind}[FieldOffset(0x{abs:X})] public fixed {cs_type(elementKind)} {name}[{len}];{oc}\n"
Vec2/3/4 → "... public fixed float {name}[2|3|4];{oc}"
Mat4x4 → "... public fixed float {name}[16];{oc}"
UTF8 → "... public fixed byte {name}[{strLen}];{oc}"
UTF16 → "... public fixed char {name}[{strLen}];{oc}"
Pointer32/64 → native: "... public IntPtr {name};{oc}"   else: "... public {cs_type(kind)} {name};{oc}"
FuncPtr32/64 → "... public IntPtr {name}; // fn ptr{oc}"   (note: " // fn ptr" BEFORE the oc marker)
_ → "... public {cs_type(kind)} {name};{oc}"
```
Then static fields → `"{ind}// static: {sf_type} {name} @ {expr}\n"`
(`sf_type = struct_type_name | cs_type(sf.kind)`).

`emit_csharp_struct(ctx, struct_id)`: dedup/cycle (Struct-only, like Rust).
`is_union` computed but the header is the same `[StructLayout(LayoutKind.Explicit)]`
for both struct and union (union-ness only affects body iteration, which here is
identical — there's no cursor).
- **Enum members:** `"public enum {type_name} : long\n{{\n"`, per member
  `"    {} = {},\n"`, then `"}}\n\n"`; return.
- Else: `"[StructLayout(LayoutKind.Explicit, Size = 0x{SIZE:X})]\n"` +
  `"public unsafe struct {type_name}\n{{\n"` + body + `"}}{offset_comment(size,true)}\n\n"`.
  **No `static_assert`** in C# (`emit_asserts` ignored by this backend's body).

Public fns: preamble `"using System.Runtime.InteropServices;\n#nullable disable\n\n"`;
return `align_comments`. (Tests: `testCSharpSimpleStruct`, `testCSharpPointers`,
`testCSharpAll`, `testCSharpEnum`, `testCSharpVectors`, `testCSharpVec3`,
`testCSharpStructLayoutSize`, `testCSharpNullableDisable`, `testCSharpDispatch`.)

---

## 9. Python ctypes backend (generator.cpp:1091–1359)

`emit_python_struct_body(ctx, struct_id, is_union, base_offset)` — cursor-based
(manual padding) like C++, **fixed `ind = "        "` (8 spaces)**. `py_type_name`
takes only `k` (no aliases). Pad field →
`"{ind}(\"{pad}\", ctypes.c_uint8 * 0x{SIZE:X}),{oc}\n"`. Hex-run collapse identical
to C++. Per child a `("name", TYPE),` tuple:
```
Struct bitfield → "{ind}(\"{name}\", {bf_type}), # bits: {bits}{oc}\n"   (bf_type=py_type_name(elementKind) | "ctypes.c_uint32")
Struct anonymous → "{ind}(\"{name}\", ctypes.c_uint8 * 0x{span:X}),{oc}\n"
Struct named → "{ind}(\"{name}\", {name_for(child)}),{oc}\n"
Array struct-elem → "{ind}(\"{name}\", {elem} * {len}),{oc}\n"
Array primitive → "{ind}(\"{name}\", {py_type_name(elementKind)} * {len}),{oc}\n"
Vec2/3/4 → "(\"{name}\", ctypes.c_float * 2|3|4),"
Mat4x4 → "(\"{name}\", (ctypes.c_float * 4) * 4),"
UTF8 → "(\"{name}\", ctypes.c_char * {strLen}),"
UTF16 → "(\"{name}\", ctypes.c_wchar * {strLen}),"
Pointer32/64 → if ref_id!=0 && refIdx>=0: "(\"{name}\", ctypes.POINTER({target})),"
               else if native: "(\"{name}\", ctypes.c_void_p),"
               else: "(\"{name}\", {py_type_name(kind)}),"
FuncPtr32/64 → "(\"{name}\", ctypes.CFUNCTYPE(None)),"
_ → "(\"{name}\", {py_type_name(kind)}),"
```
Tail padding appended (no WARNING, no overlap path). **No static-field comments
inside the body** (the C++ Python body, unlike C/Rust/C#, does NOT emit static
fields in `*Body`; they're emitted in the wrapper).

`emit_python_struct(ctx, struct_id)`: dedup/cycle (Struct-only).
- **Enum members:** `"class {type_name}:  # enum\n    __slots__ = ()\n"`, then
  `"    {} = {}\n"` per member, then `"\n"`; return.
- Else: `base_class = if is_union { "ctypes.Union" } else { "ctypes.Structure" }`;
  `"class {type_name}({base_class}):{offset_comment(size,true)}\n"` then
  `"    _fields_ = [\n"`, body, `"    ]\n"`. **Then static fields** (from
  `ctx.prepare_children(struct_id).1`): `"    # static: {py_type_name(sf.kind)} {name} @ {expr}\n"`
  — **note this uses `py_type_name(sf.kind)` and ignores `struct_type_name`**,
  unlike C/Rust/C#. Trailing `"\n"`.

Public fns: preamble `"import ctypes\n\n"`; aliases=None; return `align_comments`.
(Tests: `testPythonSimpleStruct`, `testPythonPointers`, `testPythonTypedPointers`,
`testPythonAll`, `testPythonEnum`, `testPythonEnumSlots`, `testPythonVectors`,
`testPythonUnionOutput`, `testPythonFuncPtrCFUNCTYPE`, `testPythonDispatch`.)

---

## 10. Name/filter/dispatch helpers

```
code_format_name:    CppHeader→"C/C++" RustStruct→"Rust" DefineOffsets→"#define"
                     CSharpStruct→"C#" PythonCtypes→"Python"   (default "C/C++")
code_format_file_filter:
                     CppHeader→"C++ Header (*.h);;All Files (*)"
                     RustStruct→"Rust Source (*.rs);;All Files (*)"
                     DefineOffsets→"C Header (*.h);;All Files (*)"
                     CSharpStruct→"C# Source (*.cs);;All Files (*)"
                     PythonCtypes→"Python Source (*.py);;All Files (*)"   (default "All Files (*)")
code_scope_name:     Current→"Current" WithChildren→"Current + Deps" FullSdk→"Full SDK"  (default "Current")
```
Since `CodeFormat`/`CodeScope` are closed Rust enums, the `default:` arms are
unreachable; keep them only if you add a non-exhaustive value (you won't) — a plain
exhaustive `match` is correct. (`tests-catalog`/oracle don't test these names
directly in `test_generator`, but they're public API — port for completeness.)

Dispatchers:
```
render_code(fmt,...):  RustStruct→render_rust  DefineOffsets→render_defines(tree,root)
                       CSharpStruct→render_csharp  PythonCtypes→render_python(tree,root)  _→render_cpp
render_code_tree(...): →render_*_tree variants
render_code_all(fmt,tree,aliases,emit): RustStruct→render_rust_all DefineOffsets→render_defines_all(tree)
                       CSharpStruct→render_csharp_all PythonCtypes→render_python_all(tree) _→render_cpp_all
```
**Defines/Python ignore aliases+asserts** — dispatch drops those args for them.

`render_null(_tree, _root) -> String { String::new() }`.

---

## 11. Error-handling strategy

- **No `Result`s.** The C++ returns an empty `QString` on every failure
  (invalid root id, non-Struct root, missing node). Rust returns `String::new()`
  in the exact same places. No panics, no `unwrap()` on tree lookups — every
  `index_of_id` result is checked `< 0` before indexing, mirroring the C++.
- `index_of_id` returns `i32` (`-1` miss). Convert to `usize` only after the
  `>= 0` check: `let idx = tree.index_of_id(id); if idx < 0 { return ... } let node = &tree.nodes[idx as usize];`.
- Integer arithmetic on offsets/sizes is `i32` throughout (matching C++ `int`).
  Padding/size hex formatting uses `{:X}` on the `i32`; negative values cannot
  occur on the emitted paths (sizes come from `struct_span`/`byte_size`, both
  `>= 0`; gaps are `child.offset - cursor` only taken when `> 0`). Keep `i32`;
  do not switch to `u32`/`usize` (would diverge on the overlap-warning path which
  prints `base_offset + cursor`). For `format!("{:X}")` parity with
  `QString::number(n,16).toUpper()`: both print the value's magnitude without sign
  for the non-negative values used here.
- HashMap/HashSet: use `std::collections` (or `ahash`-backed) — iteration order
  never affects output (see §12).

---

## 12. Determinism / parity invariants (MUST hold)

1. **Output byte-stability.** Tests use substring `contains`, but `align_comments`
   depends on the global max marker column and `_pad{:04x}` numbering depends on
   emission order. Preserve: child sort by offset; root/reachable iteration order;
   `pad_counter` shared across the whole render (one `GenContext`); `name_by_id`
   pass order (roots first by insertion, then nested-named).
2. **HashMap/HashSet order does not leak.** `child_map`/`emitted_*`/`visiting`/
   `forward_declared` are only membership-tested or keyed; the only order that
   reaches output is via the `Vec<usize>` child lists, which are insertion-ordered
   then offset-sorted. So a std `HashMap` (random seed) is safe. (A `BTreeMap`
   would also work and is the conservative choice; not required.)
3. **`sort_by_key(offset)` is STABLE in Rust** (`std::sort` in C++ is NOT). For
   equal-offset siblings (union members at 0; overlapping fields) Rust will keep
   insertion order, C++ may reorder. Tests assert no ordering among equal offsets,
   so both pass. Document the divergence; do not emulate C++ instability.
4. **Hex case is load-bearing:** lowercase for `_pad{:04x}`, `field_{:02x}`,
   `anon_{:x}`/`anon_{:02x}`, `bitfield_{:02x}`; **uppercase** for offset/size
   comments (`{:X}`), pad-array sizes, `#define` offsets, `[FieldOffset(0x..)]`,
   `Size = 0x..`, `static_assert(... == 0x..)`. Match the table in §5–§9 exactly.
5. **`anon_` fallback padding widths differ by site:** `struct_name()` uses
   `anon_{:x}` (no padding, from `n.id`); the Rust anonymous-field-name fallback
   uses `anon_{:02x}` (2-digit, from `child.offset`). Keep both forms distinct.
6. The `\u{1}` marker + two-pass `align_comments` reproduce column alignment;
   the `#define` backend alone skips alignment and returns raw output.
7. `struct_span` is a hard dependency already implemented in `core::tree`
   (cycle→0, static excluded, refId-embedded fallback, `max(declared, max_end)`).
   The generator passes its own `child_map` in C++; the Rust `struct_span` uses
   the tree's own `children_of` — identical result. **Verified equivalent.**

---

## 13. TEST PLAN (each C++ test → Rust `#[test]`)

All from `tests/test_generator.cpp` (76 QtTest assertions, all PASS per
`_oracle/RESULTS.md` → `test_generator | PASS | 76/0/0`). The oracle has **no
captured golden text dump** for the generator (it's substring-asserted, not a
fixture file) — so the assertions ARE the contract; translate each `QVERIFY(...)`
into a `assert!(result.contains(...))` and each `QVERIFY(!...)` into `assert!(!...)`.
Build trees with a small Rust helper mirroring the C++ `makeSimpleStruct` and the
inline `Node{…}; tree.add_node(n)` pattern.

### Test helpers (Rust)
```rust
fn make_simple_struct() -> NodeTree {            // mirrors makeSimpleStruct()
    let mut t = NodeTree::new();
    let ri = t.add_node(Node{ kind:Struct, name:"Player".into(),
        struct_type_name:"Player".into(), parent_id:0, offset:0, ..Default::default() });
    let rid = t.nodes[ri].id;
    t.add_node(Node{ kind:Int32,  name:"health".into(), parent_id:rid, offset:0, ..d() });
    t.add_node(Node{ kind:Float,  name:"speed".into(),  parent_id:rid, offset:4, ..d() });
    t.add_node(Node{ kind:UInt64, name:"id".into(),     parent_id:rid, offset:8, ..d() });
    t
}
// `d()` = Node::default(); use struct-update syntax `..Node::default()`.
```
Root id in tests is `tree.nodes[0].id` (== 1 because `add_node` assigns sequential
ids from `next_id=1`).

### Mapping table (C++ test → Rust `#[test]` → key asserts / oracle note)

| C++ test | Rust `#[test]` | Asserts to translate |
|---|---|---|
| `testSimpleStruct` | `simple_struct` | contains `#pragma once`, `// sizeof 0x10`, `struct Player\n{`, `int32_t health;`, `float speed;`, `uint64_t id;`, `};`, `// 0x0/0x4/0x8`, `static_assert(sizeof(Player) == 0x10`; and no-asserts variant lacks `static_assert`. |
| `testPaddingGaps` | `padding_gaps` | `uint8_t _pad`, `[0x4]`, `uint32_t a;`, `uint32_t b;`. |
| `testTailPadding` | `tail_padding` | `[0xF]`, `static_assert(sizeof(TailPad) == 0x11`. |
| `testOverlapWarning` | `overlap_warning` | `WARNING: overlap`. |
| `testUnionNoOverlapWarning` | `union_no_overlap_warning` | `union TestUnion\n{`, `uint64_t wide;`, `uint32_t narrow;`, NOT `WARNING`, NOT `_pad`. |
| `testNestedStruct` | `nested_struct` | `struct Outer\n{`, `struct Vec2f pos;`, `int32_t score;`, `static_assert(sizeof(Outer) == 0xC`. |
| `testPrimitiveArray` | `primitive_array` | `uint32_t data[16];`. |
| `testPointerFields` | `pointer_fields` | `struct TargetData* pTarget;`, `void* pVoid;`, `struct TargetData* pTarget32;`. |
| `testVectorTypes` | `vector_types` | `float pos2d[2];`, `float pos3d[3];`, `float color[4];`, `float transform[4][4];`. |
| `testStringTypes` | `string_types` | `char name[64];`, `wchar_t wname[32];`. |
| `testFullSdkExport` | `full_sdk_export` | `struct StructA\n{`, `struct StructB\n{`, `uint32_t valueA;`, `uint64_t valueB;`, both `static_assert`s. |
| `testDuplicateTypeNameDisambiguation` | `duplicate_type_name_disambiguation` | `struct Shared\n{`, `struct Shared_v2\n{`, `uint32_t val;`, `uint64_t val;`. |
| `testNullGenerator` | `null_generator` | `render_null(...).is_empty()`. |
| `testInvalidRootId` | `invalid_root_id` | `render_cpp(&t, 9999, None, false).is_empty()`. |
| `testNonStructRoot` | `non_struct_root` | UInt32 root → empty. |
| `testEmptyStruct` | `empty_struct` | `struct Empty\n{`, `};`, `static_assert(sizeof(Empty) == 0x0`. |
| `testNameSanitization` | `name_sanitization` | `struct my_struct_name\n{`, `uint32_t field_with_spaces;`. |
| `testExportToFile` | `export_to_file` | write `result` to a `tempfile`, read back, assert `#pragma once`/`struct Player\n{`/`static_assert`. Use the `tempfile` crate **or** write to `std::env::temp_dir()`; or simpler — assert the in-memory string (the file round-trip adds nothing in Rust since `String` is already the content). Recommend asserting the string directly + a tiny `std::fs` round-trip in `target/`. |
| `testFullSdkNoStructs` | `full_sdk_no_structs` | `#pragma once` present, NOT `struct `. |
| `testDeeplyNested` | `deeply_nested` | `struct TypeA\n{`, `struct TypeB b;`. |
| `testInlineAnonymousStruct` | `inline_anonymous_struct` | NOT `anon_`, `union\n    {`, `struct _LIST_ENTRY ListEntry;`, `uint64_t Flags;`, `};`, `uint64_t PfnCount;`. |
| `testOpaqueTypeNoStub` | `opaque_type_no_stub` | `struct _LIST_ENTRY entry;`, NOT `struct _LIST_ENTRY\n{`, NOT `uint8_t _pad`. |
| `testStaticFieldNotInStructBody` | `static_field_not_in_struct_body` | NOT `IMAGE_NT_HEADERS nt_hdr;`, contains `// static:`, `nt_hdr`, `base + e_lfanew`. |
| `testStaticFieldCommentFormat` | `static_field_comment_format` | `uint64_t base_field;`, `// static:`, `@ base + 0xFF`. |
| `testStructSizeUnchangedByStaticField` | `struct_size_unchanged_by_static_field` | `sizeof(Small) == 0x4`. |
| `testRustSimpleStruct` | `rust_simple_struct` | `// Generated by Reclass 2027`, `#[repr(C)]`, `pub struct Player {`, `pub health: i32,`, `pub speed: f32,`, `pub id: u64,`, `// 0x0/4/8`, `core::mem::size_of::<Player>() == 0x10`; no-asserts variant lacks `size_of`. |
| `testRustPadding` | `rust_padding` | `pub _pad`, `[u8; 0x4]`. |
| `testRustPointers` | `rust_pointers` | `pub typed: *mut Target,`, `pub untyped: *mut core::ffi::c_void,`. |
| `testRustVectors` | `rust_vectors` | `pub pos: [f32; 2],`, `pub color: [f32; 4],`. |
| `testRustFuncPtr` | `rust_func_ptr` | `pub callback: Option<unsafe extern "C" fn()>,`. |
| `testRustAll` | `rust_all` | `#[repr(C)]`, `pub struct Player {`, `core::mem::size_of::<Player>()`. |
| `testDefineSimpleStruct` | `define_simple_struct` | `#pragma once`, `// Player`, `#define Player_health 0x0`, `_speed 0x4`, `_id 0x8`. |
| `testDefineSkipsHex` | `define_skips_hex` | NOT `padding`, `#define HexTest_real_field 0x4`. |
| `testDefineAll` | `define_all` | `#pragma once`, `#define Player_health 0x0`. |
| `testCodeFormatDispatch` | `code_format_dispatch` | cpp `struct Player`, rust `pub struct Player`, defs `#define Player_health`. |
| `testCodeFormatAllDispatch` | `code_format_all_dispatch` | same via `render_code_all`. |
| `testTreeScopeIncludesReferencedTypes` | `tree_scope_includes_referenced_types` | Current cpp: `struct Main\n{` & NOT `struct Target\n{`; cpp tree: both; rust tree: `pub struct Main {` & `pub struct Target {`; defines tree: `#define Main_pTarget` & `#define Target_val`. |
| `testTreeScopeDispatch` | `tree_scope_dispatch` | cpp `struct A`, rust `pub struct A`, defs `#define A_x`, cs `public unsafe struct A`, py `class A(ctypes.Structure)`. |
| `testCSharpSimpleStruct` | `csharp_simple_struct` | `using System.Runtime.InteropServices;`, `[StructLayout(LayoutKind.Explicit, Size = 0x10)]`, `public unsafe struct Player`, `[FieldOffset(0x0)] public int health;`, `[FieldOffset(0x4)] public float speed;`, `[FieldOffset(0x8)] public ulong id;`. |
| `testCSharpPointers` | `csharp_pointers` | `IntPtr ptr`. |
| `testCSharpAll` | `csharp_all` | `public unsafe struct Player`, `[StructLayout(`. |
| `testCSharpEnum` | `csharp_enum` | `public enum Color : long`, `Red = 0`, `Green = 1`, `Blue = 2`. |
| `testCSharpVectors` | `csharp_vectors` | `public fixed float position[3]`. |
| `testPythonSimpleStruct` | `python_simple_struct` | `import ctypes`, `class Player(ctypes.Structure)`, `_fields_ = [`, `("health", ctypes.c_int32)`, `("speed", ctypes.c_float)`, `("id", ctypes.c_uint64)`. |
| `testPythonPointers` | `python_pointers` | `("ptr", ctypes.c_void_p)`. |
| `testPythonTypedPointers` | `python_typed_pointers` | `ctypes.POINTER(Target)`. |
| `testPythonAll` | `python_all` | `class Player(ctypes.Structure)`. |
| `testPythonEnum` | `python_enum` | `class Status:`, `Active = 1`, `Inactive = 0`. |
| `testPythonVectors` | `python_vectors` | `("color", ctypes.c_float * 4)`. |
| `testCSharpDispatch` | `csharp_dispatch` | `[StructLayout(`. |
| `testPythonDispatch` | `python_dispatch` | `ctypes.Structure`. |
| `testHex128CppOutput` | `hex128_cpp_output` | `cstdint`, (`uint8_t` OR `0x10`). |
| `testHex128RustOutput` | `hex128_rust_output` | (`u8` OR `0x10`). |
| `testEnumCppOutput` | `enum_cpp_output` | `enum Colors`, `Red = 0`, `Green = 1`. |
| `testUnionCppOutput` | `union_cpp_output` | `union MyUnion`. |
| `testPythonUnionOutput` | `python_union_output` | `ctypes.Union`. |
| `testPointerFieldCpp` | `pointer_field_cpp` | `struct Target* target_ptr`. |
| `testCSharpStructLayoutSize` | `csharp_struct_layout_size` | `[StructLayout(`, `FieldOffset`. |
| `testAlignCommentsNoMarkers` | `align_comments_no_markers` | non-empty, contains `Empty`. |
| `testForwardDeclarationForPointerTarget` | `forward_declaration_for_pointer_target` | `struct TargetB;`, `struct TargetB* ptr_to_b`. |
| `testCppIncludesCstdint` | `cpp_includes_cstdint` | `#include <cstdint>`. |
| `testRustAllowDeadCode` | `rust_allow_dead_code` | `#[allow(dead_code)]`. |
| `testCSharpNullableDisable` | `csharp_nullable_disable` | `#nullable disable`. |
| `testDefinesOutput` | `defines_output` | `#pragma once`, `Player_health`, `Player_speed`, (`0x0` OR `0x00`). |
| `testPythonFuncPtrCFUNCTYPE` | `python_func_ptr_cfunctype` | `CFUNCTYPE`. |
| `testRustPointerField` | `rust_pointer_field` | `*mut Target`. |
| `testPythonEnumSlots` | `python_enum_slots` | `__slots__`. |
| `testCppHex128InUnion` | `cpp_hex128_in_union` | `union U`, (`0x10` OR `uint8_t`). |
| `testRustFuncPtrOption` | `rust_func_ptr_option` | `Option<unsafe extern`. |
| `testCSharpVec3` | `csharp_vec3` | `fixed float`, `[3]`. |
| `testDefinesEnumMembers` | `defines_enum_members` | `Status_OK 0`, `Status_ERR 1`. |
| `testCppStaticFieldComment` | `cpp_static_field_comment` | `// static:`, `vtable`. |
| `testCppTypeAliases` | `cpp_type_aliases` | build `TypeAliases` with `Int32 → "LONG"`; `render_cpp(&t, id, Some(&aliases), false)` contains `LONG`. |

### Out of scope (do NOT port into the generator tests)
- `testCppNullptrPointerValue` references `rcx::fmt::fmtPointer32/64` — that is the
  **`format` subsystem** (`src/format.rs`, currently a skeleton), not the
  generator. The generator emits type declarations, never pointer *values*. This
  assertion belongs to the `format` spec/tests. Skip it here. (It happens to live
  in `test_generator.cpp` only because the upstream file mixes it in.)

### Suggested extra (Rust-only) tests for determinism
- `pad_counter_shared_across_render`: a tree with two gaps in two roots
  (`render_cpp_all`) → expect `_pad0000` and `_pad0001` (sequential, shared counter).
- `align_columns_single_column`: assert that two fields of differing code width
  have their `// 0x..` comments starting at the same byte column after
  `align_comments` (mirrors the implicit behavior; not in C++ but cheap insurance).

---

## 14. Ordered, independently-verifiable implementation steps

Each step ends with `cargo test --no-default-features generator` (or `cargo build`)
green for the parts implemented so far.

1. **Scaffolding + enums + helpers (no rendering).** Add `code_format_name`,
   `code_format_file_filter`, `code_scope_name`; `sanitize_ident`; `offset_comment`;
   `indent`; `COMMENT_MARKER`; `align_comments`; the four type-name tables
   (`c_type_name`/`rust_type_name`/`cs_type_name`/`py_type_name`); `build_child_map`.
   Unit-test `sanitize_ident` (`testNameSanitization` cases) and `align_comments`
   (`testAlignCommentsNoMarkers`) directly. ✔ verifiable in isolation.
2. **`GenContext` + `assign_unique_names` + `prepare_children` + `name_for`/`struct_name`/`c_type`/`unique_pad_name`.**
   Unit-test `assign_unique_names` collision suffixing (`Shared`→`Shared_v2`) and
   `unique_pad_name` format (`_pad0000`, `_pad0001`).
3. **C/C++ backend:** `emit_field`, `emit_struct_body`, `emit_struct`,
   `render_cpp`/`render_cpp_tree`/`render_cpp_all`. Run all C/C++-tagged tests
   (rows: simple/padding/tail/overlap/union/nested/array/pointer/vector/string/
   fullsdk/dup/null/invalid/nonstruct/empty/sanitization/exportfile/fullsdknostructs/
   deeplynested/inlineanon/opaque/static*/hex128cpp/enumcpp/unioncpp/pointerfieldcpp/
   forwarddecl/cppincludes/typealiases). This is the bulk; once green, the layout
   engine is proven.
4. **`collect_reachable_structs`** (needed by every `*_tree`). Verified by the
   `*_tree` portions of `tree_scope_includes_referenced_types`.
5. **Rust backend:** `emit_rust_field`/`_struct_body`/`_struct` + `render_rust*`.
   Tests: rust_simple/padding/pointers/vectors/funcptr/all/hex128rust/
   rust_pointer_field/rust_func_ptr_option/rust_allow_dead_code.
6. **`#define` backend:** `emit_defines_for_struct` + `render_defines*`. Tests:
   define_simple/skips_hex/all/defines_output/defines_enum_members + the defines
   slice of `tree_scope_*`.
7. **C# backend:** `emit_csharp_struct_body`/`_struct` + `render_csharp*`. Tests:
   csharp_simple/pointers/all/enum/vectors/vec3/struct_layout_size/nullable_disable.
8. **Python backend:** `emit_python_struct_body`/`_struct` + `render_python*`.
   Tests: python_simple/pointers/typed_pointers/all/enum/enum_slots/vectors/
   union_output/func_ptr_cfunctype.
9. **Dispatchers:** `render_code`/`render_code_tree`/`render_code_all` + `render_null`.
   Tests: code_format_dispatch/code_format_all_dispatch/tree_scope_dispatch/
   csharp_dispatch/python_dispatch/null_generator.
10. **Wire into `lib.rs`/`core` re-exports if desired** (generator is already a
    declared module; export `render_code`, `CodeFormat`, etc. from the crate root
    if the UI needs them — match what `controller`/`ui` import). Final full-suite run.

> After step 3 the highest-value parity is locked (C/C++ is the canonical backend
> and most of the 76 assertions touch it directly). Steps 5–8 are mechanically
> similar; reuse the cursor-loop skeleton (C/Rust/Python) and the field-iteration
> skeleton (C#).
