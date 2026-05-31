# PORTING SPEC — Importers / Exporters (key: `imports`)

Function-level porting spec to drive a faithful Rust implementation of the
`src/imports/` subsystem of Reclass. Target: 1:1 behavioral parity with the
published C++ source. Read alongside `_design/understand/imports.md` (the
behavioral map; cited as **BMAP §N**), `_design/ARCHITECTURE.md`,
`_design/crate_selection.md`, and `_oracle/RESULTS.md`.

This subsystem ports five C++ files:
`import_source.{cpp,h}` (C/C++ source → NodeTree), `import_reclass_xml.{cpp,h}`
(ReClass XML → NodeTree), `export_reclass_xml.{cpp,h}` (NodeTree → ReClassEx XML),
`pe_debug_info.{cpp,h}` (PE CodeView debug-dir → PDB GUID/age/name over a
`Provider`), and `import_pdb.{cpp,h}` (PDB type/symbol import).

> **Out of scope here:** RCX native JSON (`NodeTree::to_json`/`from_json`) lives in
> `core` (it is `core.h`, not `imports/`). C/C++ *export* (`renderCpp`, `renderRust`)
> lives in `generator`. This spec only does the *import* direction for C/C++ source,
> plus the XML in/out and PDB/PE importers. The round-trip test
> (`test_roundtrip_winsdk::roundTrip30`) calls `generator::render_cpp` →
> `importFromSource` → `render_cpp` and requires byte-identical output; this spec must
> reproduce `importFromSource` precisely so that round-trip holds, but the generator is
> ported by its own workflow.

---

## 0. Target crate / module layout

One package `reclass` (per ARCHITECTURE.md §2); this subsystem is the module tree
**`src/imports/`**, gated behind the `imports` cargo feature
(`default = ["ui","imports","disasm","symbols","mcp"]`). Sub-modules:

```
src/imports/
├─ mod.rs              # pub re-exports + shared PendingRef + error type ImportError
├─ source.rs           # ← import_source.cpp : import_from_source(...)
├─ reclass_xml.rs      # ← import_reclass_xml.cpp + export_reclass_xml.cpp
│                      #   (one module: import + export share the V2016 type map)
├─ pe_debug_info.rs    # ← pe_debug_info.cpp : extract_pdb_debug_info(prov, base)
└─ pdb.rs              # ← import_pdb.cpp : enumerate_pdb_types / import_pdb_selected /
                       #   import_pdb / import_type_for_symbol / extract_pdb_symbols
```

Depends on: `core` (the `Node`/`NodeTree`/`NodeKind` model — already ported by the
`core` workflow), and `provider` (the `Provider` trait — for `pe_debug_info`).

**External crates** (all already in `crate_selection.md`; add under
`[features] imports = [...]` deps):

| Concern | Crate | Version | Used by |
|---|---|---|---|
| ReClass XML read | `quick-xml` (Reader) | 0.40.1 | `reclass_xml.rs` |
| ReClass XML write | `quick-xml` (Writer) | 0.40.1 | `reclass_xml.rs` |
| offset-comment regexes | `regex` | 1.x | `source.rs` |
| PE header struct casts | `bytemuck` (`Pod`+`Zeroable`, `#[repr(C, packed)]`) | 1.x | `pe_debug_info.rs` |
| PDB parsing | `pdb2` | 0.10.1 | `pdb.rs` |
| PE-from-file (unused here; over-`Provider` path is hand-rolled) | `object` | 0.39.1 | (not needed for parity) |
| typed errors | `thiserror` | 2.x | `mod.rs` |
| log traces (`qDebug`) | `tracing` (`debug!`) | 0.1.x | all (drop most) |

> **`pdb2` not `pdb`:** the unmaintained `pdb` 0.8.0 is replaced by the maintained fork
> `pdb2` 0.10.1 (same API surface). Where this spec writes `pdb::` it means the `pdb2`
> crate's modules. The C++ gated PDB behind `#ifdef _WIN32` (RawPDB + Win32 mmap); `pdb2`
> is pure Rust and portable, so PDB import can build & run on Linux too. See §5.0 for the
> cfg-gating decision.

---

## 1. Shared types & error strategy (`src/imports/mod.rs`)

### 1.1 Error handling (BMAP §6)

Every C++ public fn takes `QString* errorMsg` (optional out-param) and returns an
**empty `NodeTree` / `false` / `{}` on failure**. Idiomatic Rust port:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("Empty source code")]
    EmptySource,
    #[error("No struct or enum definitions found")]
    NoDefinitions,
    #[error("No nodes generated from source")]
    NoNodes,
    #[error("Cannot open file: {0}")]
    CannotOpen(String),
    #[error("Cannot open file for writing: {0}")]
    CannotOpenWrite(String),
    #[error("XML parse error at line {line}: {msg}")]
    XmlParse { line: u64, msg: String },
    #[error("No classes found in file")]
    NoClasses,
    #[error("No nodes to export")]
    NoNodesToExport,
    #[error("No struct classes found to export")]
    NoClassesToExport,
    // PDB
    #[error("PDB file not found")] PdbNotFound,
    #[error("Failed to memory-map PDB")] PdbMapFailed,
    #[error("Invalid PDB file")] PdbInvalid,
    #[error("PDB has no valid TPI stream")] PdbNoTpi,
    #[error("Import cancelled")] PdbCancelled,
    #[error("No types imported")] PdbNoTypesImported,
    #[error("Type '{0}' not found in PDB")] PdbTypeNotFound(String),
    #[error("No types found in PDB")] PdbNoTypes,
    #[error("Symbol has no associated type")] SymbolNoType,
    #[error("Type at index {0} resolves to a primitive")] ResolvesToPrimitive(u32),
    #[error("Failed to import type at index {0}")] FailedImportType(u32),
    #[error("PDB error: {0}")] Pdb(String),  // wraps pdb2's error
    #[error("IO error: {0}")] Io(String),
}
```

Public fns return `Result<NodeTree, ImportError>` (or `Result<(), ImportError>` for
export). **Parity note (BMAP §6):** the C++ tests only check `!err.isEmpty()`, so the
exact message string is *not* asserted — but keep the **failure conditions** identical
(empty input, no defs, no nodes, file-open failure, etc.). Messages above mirror the
originals for fidelity, but any non-empty error satisfies the oracle.

### 1.2 PendingRef (BMAP §6, shared by `source.rs` and `reclass_xml.rs`)

Deferred name→id reference resolution. Nodes that reference a class/struct/enum by
**name** during building push a `PendingRef`; a post-pass sets `refId`:

```rust
struct PendingRef { node_id: u64, class_name: String }
```

Resolution post-pass (identical in both importers):
```
for r in pending_refs:
    idx = tree.index_of_id(r.node_id); if idx < 0 { continue }
    if let Some(&id) = class_ids.get(&r.class_name) { tree.nodes[idx].ref_id = id }
    // unresolved refs leave ref_id = 0 (do NOT error)
```

### 1.3 The `addNode` index/id contract (BMAP §0, §6) — CRITICAL

`NodeTree::add_node(&mut self, n: Node) -> usize` returns the **index**, not the id.
Importers must always do:
```rust
let idx = tree.add_node(n);
let id = tree.nodes[idx].id;   // re-read the assigned id
```
`add_node` assigns `id = m_next_id++` if `n.id == 0`. Self/forward references work
because the **root struct is added (and its id known) before its fields are built**.

### 1.4 Determinism (BMAP §6)

No pointer hashing. Iteration order = source/file order. The XML exporter sorts roots
and children **by offset** before writing. PDB enumeration is in TPI type-index order.
Preserve all of these. Use `IndexMap`/`Vec` where insertion order matters; the
`class_ids` name→id maps can be plain `HashMap` because lookups are by exact name.

---

## 2. `source.rs` — C/C++ source → NodeTree (`import_from_source`)

Ports `import_source.cpp` (1624 lines). **PURE / fully portable** (no OS calls, no
threading). Hand-rolled tokenizer + recursive-descent parser for a *subset* of C/C++
struct declarations; robust to garbage (skips what it can't parse). See BMAP §1.

### 2.0 Public signature

```rust
// C++: NodeTree importFromSource(const QString&, QString*, int pointerSize=8)
pub fn import_from_source(source: &str, pointer_size: i32) -> Result<NodeTree, ImportError>;
```
`pointer_size` defaults to 8 at call sites (no Rust default args; provide a thin
`import_from_source8(src)` helper or just pass 8). Returns `Err` on the three failure
paths (empty input, no structs/enums, no nodes generated).

### 2.1 Item-by-item mapping

| C++ (import_source.cpp) | Rust counterpart | Notes |
|---|---|---|
| `struct TypeInfo {NodeKind kind; int size;}` | `struct TypeInfo { kind: NodeKind, size: i32 }` | |
| `buildTypeTable(int ptrSize=8)` → `QHash<QString,TypeInfo>` (cpp:17) | `fn build_type_table(ptr_size: i32) -> HashMap<&'static str, TypeInfo>` | **Verbatim** table; full entries below (§2.2). |
| `enum class TokKind {…}` (cpp:128) | `#[derive(Clone,Copy,PartialEq)] enum TokKind {Ident,Number,Star,Semi,LBrace,RBrace,LBracket,RBracket,LParen,RParen,Comma,Colon,Equals,Hash,Eof,Other}` | |
| `struct Token {TokKind; QString text; int line;}` (cpp:134) | `struct Token { kind: TokKind, text: String, line: i32 }` | |
| `struct LineOffset {int line; int offset;}` (cpp:141) | `struct LineOffset { line: i32, offset: i32 }` | offset = parsed hex |
| `tokenize()` (cpp:155) | `fn tokenize(src: &str) -> (Vec<Token>, Vec<LineOffset>)` | returns tokens + captured comment offsets |
| `parseLineComment` (cpp:222) | inline in `tokenize` | two regexes, see §2.3 |
| `parseBlockComment`, `skipToEndOfLine`, `parseIdent`, `parseNumber` | inline helpers | |
| `struct ParsedField` (cpp:285) | `struct ParsedField {...}` (§2.4) | |
| `struct ParsedStruct` (cpp:298) | `struct ParsedStruct {...}` (§2.4) | |
| `class Parser` (cpp:328) — recursive descent | `struct Parser<'a> { toks: &'a [Token], pos: usize, structs, forward_decls, typedefs, pointer_typedefs, array_typedefs, size_asserts, struct_alignments }` | methods below |
| `peek/advance/check/checkIdent/match/matchIdent` | `fn peek(&self, ahead) -> &Token`, `fn advance(&mut self)`, `fn check(&self, TokKind) -> bool`, … | |
| `skipToSemiOrBrace()` | `fn skip_to_semi_or_brace(&mut self)` | brace-depth-aware skip to top-level `;` |
| `skipAlignMacro()` → int | `fn skip_align_macro(&mut self) -> i32` | consumes `ALIGN(...)`/`__declspec(...)`, returns first numeric arg or 0 |
| `peekPastAlign(offset, expectedKind)` | `fn peek_past_align(&self, offset, TokKind) -> bool` | |
| `isTypeModifier`/`isQualifier` | `fn is_type_modifier(&str)`, `fn is_qualifier(&str)` | sets `{unsigned,signed,long,short}` / `{const,volatile,mutable,struct,class,enum}` |
| `parse()` (cpp:427) | `fn parse(&mut self)` | top-level dispatch |
| `parseStructOrForward` (cpp:449) | `fn parse_struct_or_forward(&mut self)` | |
| `parseStructBody` (cpp:500) | `fn parse_struct_body(&mut self, ps: &mut ParsedStruct)` | |
| `parseTopLevelUnion` (cpp:563) | `fn parse_top_level_union(&mut self)` | |
| `parseUnion` (cpp:603) | `fn parse_union(&mut self, ps: &mut ParsedStruct)` | member union |
| `parseField` (cpp:684) | `fn parse_field(&mut self, line_offsets: &[LineOffset]) -> Option<ParsedField>` | rewind-on-fail |
| `parseTypeName` (cpp:794) | `fn parse_type_name(&mut self) -> String` | |
| `parseStaticAssert` (cpp:827) | `fn parse_static_assert(&mut self)` | |
| `parseTypedef` (cpp:873) | `fn parse_typedef(&mut self)` | |
| `parseEnumDef` (cpp:966) | `fn parse_enum_def(&mut self)` | |
| `isPaddingName` (cpp:1053) | `fn is_padding_name(&str) -> bool` | case-insensitive prefix match |
| `emitHexPadding(tree,parentId,offset,size)` (cpp:1065) | `fn emit_hex_padding(tree: &mut NodeTree, parent_id: u64, offset: i32, size: i32)` | best-fit hex run |
| `emitBitfieldGroup(...)` (cpp:1090) | `fn emit_bitfield_group(...)` | one bitfield container Struct node |
| `struct BuildContext` (cpp:1127) | `struct BuildContext { type_table, class_ids, pending_refs, use_comment_offsets, enum_names, ptr_size, size_asserts, struct_alignments }` (+ `&mut NodeTree`) | |
| `struct PendingRef` | shared from `mod.rs` (§1.2) | |
| `fieldNaturalAlignment` (cpp:1153) | `fn field_natural_alignment(...) -> i32` | |
| `unionNaturalAlignment` | `fn union_natural_alignment(...) -> i32` | |
| `alignUp(off,align)` | `fn align_up(off: i32, align: i32) -> i32` = `(off+align-1) & !(align-1)` | |
| `structTypeSize(name)` (cpp:1173) | `fn struct_type_size(&self, name: &str) -> i32` | |
| `clampedArrayElements(dims, max=1e6)` (cpp:1192) | `fn clamped_array_elements(dims: &[i32], max: i64) -> i32` | product, 0/neg→1, cap |
| `buildFields(ctx, parentId, baseOffset, fields)` (cpp:1201) | `fn build_fields(&mut self, parent_id: u64, base_offset: i32, fields: &[ParsedField])` | the core builder (§2.6) |
| `hasAnyCommentOffset(fields)` (cpp:1499) | `fn has_any_comment_offset(fields: &[ParsedField]) -> bool` | recursive incl. union members |
| `importFromSource` (cpp:1509) | `import_from_source` top-level (§2.7) | |

### 2.2 Type table (BMAP §1.1; verified against cpp:17–124) — port VERBATIM

Build a `HashMap<&'static str, TypeInfo>`. **Architecture-independent entries** (same
regardless of `ptr_size`):

- stdint: `uint8_t`→UInt8(1), `int8_t`→Int8(1), `uint16_t`→UInt16(2), `int16_t`→Int16(2),
  `uint32_t`→UInt32(4), `int32_t`→Int32(4), `uint64_t`→UInt64(8), `int64_t`→Int64(8).
- standard C: `char`→Int8(1), `short`→Int16(2), `int`→Int32(4), **`long`→Int32(4)**,
  `float`→Float(4), `double`→Double(8), `bool`/`_Bool`→Bool(1), **`void`→Hex8(1)**,
  `wchar_t`→UInt16(2).
- multi-word (parser pre-merges modifier words with single spaces):
  `"unsigned char"`→UInt8(1), `"signed char"`→Int8(1), `"unsigned short"`→UInt16(2),
  `"signed short"`→Int16(2), `"unsigned int"`→UInt32(4), `"signed int"`→Int32(4),
  `"unsigned"`→UInt32(4), `"long long"`→Int64(8), `"unsigned long"`→UInt32(4),
  `"signed long"`→Int32(4), `"unsigned long long"`→UInt64(8), `"signed long long"`→Int64(8),
  `"long int"`→Int32(4), `"long long int"`→Int64(8), `"unsigned long int"`→UInt32(4),
  `"unsigned long long int"`→UInt64(8), `"short int"`→Int16(2), `"unsigned short int"`→UInt16(2).
- Windows fixed: `BYTE`/`UCHAR`/`BOOLEAN`→UInt8(1), `CHAR`→Int8(1),
  `WORD`/`USHORT`/`WCHAR`/`TCHAR`→UInt16(2) (`SHORT`→Int16(2)),
  `DWORD`/`ULONG`/`UINT`→UInt32(4), `LONG`/`LONG32`/`INT`→Int32(4), **`BOOL`→Int32(4)**,
  `FLOAT`→Float(4), `QWORD`/`ULONGLONG`/`DWORD64`/`ULONG64`/`UINT64`→UInt64(8),
  `LONGLONG`/`LONG64`/`INT64`→Int64(8).

**Architecture-dependent** (`ptr_kind` = Pointer64 if `ptr_size>=8` else Pointer32;
`uintp_kind` = UInt64/UInt32; `intp_kind` = Int64/Int32; each entry's size = `ptr_size`):
- ptr_kind: `PVOID`,`LPVOID`,`HANDLE`,`HMODULE`,`HWND`,`HINSTANCE`, and pointer aliases
  `PCHAR`,`LPSTR`,`LPCSTR`,`PCSTR`,`PWSTR`,`LPWSTR`,`LPCWSTR`,`PCWSTR`.
- uintp_kind: `SIZE_T`,`ULONG_PTR`,`UINT_PTR`,`DWORD_PTR`,`uintptr_t`,`size_t`.
- intp_kind: `LONG_PTR`,`INT_PTR`,`SSIZE_T`,`intptr_t`,`ptrdiff_t`,`ssize_t`.

> **Pinned by test `platformPointerTypes`:** `PVOID`/`HANDLE` → **Pointer64**, but
> `SIZE_T`/`ULONG_PTR`/`uintptr_t`/`size_t` → **UInt64** (NOT a pointer kind).

After building, fold in user typedefs (cpp:1532): for each parsed `alias→real`, if
`table` contains `real`, set `table[alias] = table[real].clone()`.

### 2.3 Tokenizer (BMAP §1.2; cpp:155–280)

Hand-roll, mirroring the C++. Walk the source char-by-char:
- skip whitespace (`char::is_whitespace`), tracking `line += 1` on `\n`.
- `//` → line comment: take text after `//`, **trim**, run the two offset regexes
  (below); push `LineOffset { line: comment_line, offset }` on match; skip to EOL.
- `/* */` → block comment: advance `line` on each `\n`; unterminated → consume to EOF.
- `#` at the start of scanning (the C++ treats `#` specially) → `skip_to_end_of_line`
  (preprocessor lines dropped entirely; `#` essentially never becomes a token).
- letter/`_` → `parse_ident`: greedy `[A-Za-z0-9_]` (`char::is_alphanumeric || '_'`).
- digit → `parse_number`: `0x…` hex digits OR decimal; then **eat integer suffixes**
  `u/U/l/L`. The token text is the full literal incl. `0x` and suffix.
- single chars mapped to TokKind (`*`→Star, `;`→Semi, `{`→LBrace, `}`→RBrace, `[`→LBracket,
  `]`→RBracket, `(`→LParen, `)`→RParen, `,`→Comma, `:`→Colon, `=`→Equals, `#`→Hash);
  unknown char → `Other` with the char as text (this is how unary `-` in enum values
  surfaces — `Other` text `"-"`, cpp:1015).
- always push trailing `Eof`.

**Offset comment regexes** (cpp:222; `regex` crate, compile once via `once_cell`/`LazyLock`):
1. whole-comment: `^(?:->\s*\S+\s+)?0x([0-9A-Fa-f]+)$`  (matches `"0x10"` or `"-> Material* 0x10"`).
2. fallback trailing hex: `\b0x([0-9A-Fa-f]+)\s*$`  (matches `"// foo 0x1A"`).
Try (1) first; on miss try (2). Captured hex → `i32::from_str_radix(cap, 16)`.

**Critical edge (BMAP §1.2):** an offset comment is associated with the **comment's own
line**. The parser later matches a field's *start* token line (`toks[start].line`)
against `LineOffset.line`, so `// 0xNN` must be on the **same line** as the field. Pinned
by `commentOffsets`, `mixedOffsetsAutoDetect`.

> Qt uses Unicode `QChar::isLetter/isDigit/isSpace`; for ASCII source `char::is_*` is
> equivalent. For faithful parity with non-ASCII identifiers use Unicode-aware methods
> (`is_alphabetic`/`is_alphanumeric`). Rare in practice; document as a known edge (BMAP §8).

### 2.4 Parser state structs (cpp:285–326)

```rust
struct ParsedField {
    type_name: String,           // merged base type (e.g. "unsigned long")
    name: String,
    is_pointer: bool,
    pointer_depth: i32,
    array_sizes: Vec<i32>,
    comment_offset: i32,         // -1 = none
    bitfield_width: i32,         // -1 = none
    pointer_target: String,
    is_union: bool,
    union_members: Vec<ParsedField>,
}
struct ParsedStruct {
    name: String,
    keyword: String,             // "struct"/"class"/"union"/"enum"
    fields: Vec<ParsedField>,
    // declared_size: i32 (-1, unused — omit)
    enum_values: Vec<(String, i64)>,
}
```
Parser maps: `typedefs: HashMap<String,String>` (alias→real),
`pointer_typedefs: HashSet<String>`, `array_typedefs: HashMap<String,Vec<i32>>`,
`forward_decls: HashSet<String>`, `size_asserts: HashMap<String,i32>` (struct→sizeof),
`struct_alignments: HashMap<String,i32>` (struct→ALIGN(N)).

### 2.5 Parser methods — behavior notes (BMAP §1.3; cpp:427–1051)

Port the recursive descent **verbatim**. Key behaviors the oracle pins:

- **`parse()`** (cpp:427): dispatch by leading keyword — `struct`/`class`→
  `parse_struct_or_forward`; `union`→`parse_top_level_union`; `static_assert`→
  `parse_static_assert`; `typedef`→`parse_typedef`; `enum`→`parse_enum_def`; stray `#`→
  skip to `;`/EOF; else `advance()` (skip one token, e.g. `int x = 42;` → no struct → error).
- **`parse_struct_or_forward`** (cpp:449): consume keyword; `align_val = skip_align_macro()`.
  If `{` follows (anonymous top-level) → skip body + trailing `;`, return. Require Ident
  name (else skip to `;`); record `struct_alignments[name]=align_val` if >0. If `:` →
  skip inheritance to `{`/`;`/EOF (bases ignored — test `inheritanceSkipped`). If `;` →
  forward decl (`forward_decls.insert`), return. Require `{`; build `ParsedStruct{name,
  keyword}`; `parse_struct_body`; require `}`; optional `;`; append to `structs`.
- **`parse_struct_body`** (cpp:500): loop until `}`/EOF: nested named `struct`/`class`
  `Name {` (or `ALIGN(N) Name {`) → recurse `parse_struct_or_forward` (becomes its **own
  root**; no automatic embedding); anonymous nested `struct {` → **skip body** + optional
  field name + `;`; `union`→`parse_union`; `enum`→`parse_enum_def`;
  `static_assert`→`parse_static_assert`; else `parse_field` (append on success, else
  `advance()`).
- **`parse_top_level_union`** (cpp:563): like struct but `keyword="union"`; handles fwd
  decl, anonymous (skip), named-with-body → append. (Direct children later forced to
  offset 0; see §2.7.)
- **`parse_union`** (cpp:603): consume `union`, `skip_align_macro`, optional tag, `{`;
  build `ParsedField{is_union:true}`; recurse nested unions (steal fields), skip nested
  structs, else `parse_field` each into `union_members`; after `}`, optional field name
  (`} u3;` → test `namedUnion`), `;`. `comment_offset` = first member's offset; append to
  `ps.fields`.
- **`parse_field`** (cpp:684) — the core field grammar (port exactly):
  1. save `start_pos`; skip leading qualifiers (`is_qualifier`).
  2. `parse_type_name()` → base; empty ⇒ rewind to `start_pos` and **fail** (`None`).
  3. typedef-resolution loop: follow `typedefs` chain (cycle-guard with a visited
     `HashSet`); if any link ∈ `pointer_typedefs` set `typedef_pointer=true`; capture
     first `array_typedefs` dims found; `type_name` becomes fully-resolved real type.
  4. pointer stars: `is_pointer = typedef_pointer`, `depth = typedef_pointer?1:0`;
     consume `*` (each ++depth); skip `const`/`volatile`; consume more `*`.
  5. field name: require Ident (else rewind & fail).
  6. array dims: each `[N]` (N decimal or `0x…`; empty `[]`→0) pushed to `array_sizes`.
  7. apply typedef array dims: if field had none, use typedef's; else **prepend** typedef
     dims.
  8. bitfield: `: width` → `bitfield_width = width`.
  9. require `;` (else rewind & fail).
  10. match a `LineOffset` whose `.line == toks[start_pos].line` → `comment_offset`.
  11. fill fields; if pointer, `pointer_target = type_name`.
- **`parse_type_name`** (cpp:794): `struct`/`class`/`enum` Ident → returns the Ident
  (consumes the keyword; test `structPrefixOnType`). Modifier word
  (`unsigned`/`signed`/`long`/`short`) → collect following modifier/`int`/`char`/`long`
  words, **join with single space** (yields the multi-word table keys). Else consume +
  return one Ident.
- **`parse_static_assert`** (cpp:827): scan paren group for `sizeof(Ident)` and the
  *first* numeric literal (decimal or `0x…`); if found and `size>0`, record
  `size_asserts[struct_name]=size`. Tests `staticAssertTailPadding`, `basicRoundTrip`.
- **`parse_typedef`** (cpp:873): `typedef struct {…} Name;` / `typedef struct Tag {…}
  Name;` → parse full struct (body becomes a root), return. `typedef struct Existing *
  Alias;` → `typedefs[Alias]=Existing`; if `*` then `pointer_typedefs.insert(Alias)`;
  skip self-ref. `typedef Base [*] Alias [N]…;` → resolve base via `parse_type_name`,
  capture pointer + array dims, register (+ pointer/array sets); skip self-ref.
  **Fn-ptr typedef** `typedef Ret (*Name)(args);` → `pointer_typedefs.insert(Name)` +
  `typedefs[Name]="void"` (treated as void*).
- **`parse_enum_def`** (cpp:966): handles `enum`, `enum class`, `enum struct`. Name only
  taken if followed by `{` or `:` (so `enum Foo bar;` field-usage and `enum Foo;` fwd
  decls aren't definitions; for `enum Name;` it consumes the `;` and returns). Skip
  `: underlying`. Parse members `Name [= Value]`, optional commas. Value: optional unary
  `-` (an `Other` token text `"-"`), then a Number (decimal or `0x…`, parse as i64);
  complex exprs skipped to next `,`/`}`. Auto-value starts 0, `prev+1` when omitted.
  `ps.keyword="enum"`, `enum_values` populated; **append only if name non-empty**
  (anonymous enums dropped). Tests `enumAutoValues`, `enumHexValues`, `enumClass`.

### 2.6 `build_fields` — the field builder (BMAP §1.4–§1.5; cpp:1201–1495)

Walk fields, tracking `computed_offset` (running offset for non-comment mode). For each
field choose its offset:
```
if use_comment_offsets && field.comment_offset >= 0:
    field_offset = field.comment_offset - base_offset   # absolute → parent-relative
else:
    computed_offset = align_up(computed_offset, field_align)
    field_offset = computed_offset
```
`field_align = field_natural_alignment(field)` (cpp:1153): pointer→`ptr_size`;
union→`union_natural_alignment` (max member align); bitfield→alignment of its storage
type (else 4); known type→`alignment_for(kind)`; unknown (struct ref)→`ptr_size`.

Categories, **in this exact order** (cpp:1201; first match wins):

1. **Bitfield group:** consume consecutive `bitfield_width >= 0` fields; emit one
   bitfield container via `emit_bitfield_group(tree, parent_id, field_offset, group)`;
   advance `computed_offset` past the container's byte size (1/2/4/8). Tests
   `bitfieldSkipped`, `bitfieldWithOffsetsEmitsHex`.
   - `emit_bitfield_group` (cpp:1090): `total_bits = Σ width`; `bytes = (total_bits+7)/8`;
     container hex kind: ≤1 Hex8, ≤2 Hex16, ≤4 Hex32, else Hex64. Emit one `Struct` node
     with `class_keyword="bitfield"`, `element_kind=container_kind`, `collapsed=false`,
     and a `BitfieldMember{name, bit_offset (cumulative), bit_width}` per field.
2. **Union field** (`field.is_union`): emit `Struct{class_keyword="union", name=field.name,
   offset=field_offset}`; build **each union member independently** by calling
   `build_fields` on a 1-element slice with `base_offset = base_offset + union_offset` so
   each member starts at relative offset 0; advance `computed_offset` by the union's
   `tree.struct_span(union_id)`. Tests `unionContainer`, `unionWithCommentOffsets`,
   `namedUnion`.
3. **Pointer field** (`field.is_pointer`): `ptr_kind` = Pointer64/32 by `ptr_size`. If
   `array_sizes` non-empty → `Array{element_kind=ptr_kind, array_len=product}`; advance by
   `count*ptr_size`. Else single pointer node (`collapsed=true`); if `pointer_target`
   non-empty and ≠`"void"`, push `PendingRef{id, pointer_target}`; advance by `ptr_size`.
   Tests `voidPointer` (void*→refId stays 0), `selfReferencingPointer`, `doublePointer`
   (`void**` still single Pointer64), `typedPointer`, `pointerCrossRef`, `forwardDeclaration`.
4. **Enum-typed field** (unknown type that ∈ `enum_names`): emit `UInt32` (size 4) +
   `PendingRef` to the enum; array form supported. Test `enumInStruct`.
5. **Resolve base type:** `known = type_table.contains(type_name)`; if known,
   `base_kind/base_size` from table; else `is_struct_type = true` (unknown name = struct ref).
6. **Padding field** (`is_padding_name(name) && !array_sizes.is_empty()`): expand via
   `emit_hex_padding(tree, parent_id, field_offset, base_size * Πdims)`. Test
   `commentOffsets` (`_pad000C[0x4]`).
   - `emit_hex_padding` (cpp:1065): if `size%8==0 && size>=8`→Hex64×(size/8); else `%4`→
     Hex32; `%2`→Hex16; else Hex8. Each node `name=""`. Test `paddingFieldExpansion`
     (0x10 → 2×Hex64 at 0 and 8).
7. **Array of primitive** (`!array_sizes.is_empty() && !is_struct_type`):
   - `char[N]`/`CHAR[N]` (single dim, Int8) → **UTF8** `str_len=N`.
   - `wchar_t[N]`/`WCHAR[N]`/`TCHAR[N]` (single dim, UInt16) → **UTF16** `str_len=N`.
   - `float[2]`→Vec2, `float[3]`→Vec3, `float[4]`→Vec4.
   - `float[4][4]`→Mat4x4.
   - else generic `Array{element_kind=base_kind, array_len=product}`; advance by
     `product*base_size`.
   Tests `charArrayToUtf8`, `wcharArrayToUtf16`, `floatArrayToVec2/3/4`,
   `floatArray4x4ToMat4x4`, `genericFloatArray`, `primitiveArray`, `hexArraySizes`.
8. **Struct-type field** (`is_struct_type`): `elem_size = struct_type_size(name)`. Array →
   `Array{element_kind=Struct, struct_type_name=name, array_len=product}` + `PendingRef`;
   advance by `product*elem_size` (only if `elem_size>0`). Single → `Struct{name,
   struct_type_name=name, collapsed}` + `PendingRef`; advance by `elem_size` (if >0).
   Tests `embeddedStruct`, `structArray`, `structPrefixOnType`; PEB layout (`pebOffsets`).
9. **Simple primitive:** `Node{kind=base_kind}`; advance by `base_size`.

`struct_type_size(name)` (cpp:1173): if struct already built (`class_ids`), use
`tree.struct_span(id)`, then round **up to its `ALIGN(N)`** if declared; else
`size_asserts[name]`; else 0.

`clamped_array_elements(dims, max=1_000_000)` (cpp:1192): product of dims (0/neg → 1),
capped at `max`. Watch i32/i64 overflow — compute the product in i64, then clamp/cast.

`has_any_comment_offset(fields)` (cpp:1499): recursive over union members — if **any**
field anywhere has `comment_offset >= 0`, comment mode is enabled (whole-file decision).
Test `mixedOffsetsAutoDetect` (fields without comments still get *computed* offsets in
comment mode).

### 2.7 `import_from_source` top-level (BMAP §1.6; cpp:1509)

```
1. if source.trim().is_empty() -> Err(EmptySource)
2. (toks, line_offsets) = tokenize(source)
3. parser = Parser::new(&toks); parser.parse()
   if parser.structs.is_empty() -> Err(NoDefinitions)
4. type_table = build_type_table(ptr_size); fold parser.typedefs into it
5. tree.base_address = 0x0040_0000; tree.pointer_size = ptr_size
6. use_comment_offsets = any struct has any comment offset (has_any_comment_offset)
7. enum_names = { ps.name for ps in structs if ps.keyword == "enum" }
8. ctx = BuildContext { tree:&mut, type_table, class_ids:{}, pending_refs:{},
                        use_comment_offsets, enum_names, ptr_size,
                        size_asserts, struct_alignments }
9. for ps in parser.structs (source order):
     root = Node{kind:Struct, name:ps.name, struct_type_name:ps.name,
                 class_keyword:ps.keyword}
     if ps.keyword == "enum":
         root.enum_members = ps.enum_values; idx=add_node(root)
         class_ids[ps.name] = tree.nodes[idx].id; continue
     idx = add_node(root); id = tree.nodes[idx].id
     class_ids[ps.name] = id
     build_fields(id, 0, &ps.fields)
     if ps.keyword == "union":              # cpp:1588 — force direct children to offset 0
         for child in children_of(id): child.offset = 0
     if size_asserts[ps.name] > struct_span(id):   # tail padding
         emit_hex_padding(tree, id, current_span, declared - current_span)
10. if tree.nodes.is_empty() -> Err(NoNodes)
11. resolve pending_refs (§1.2)
12. Ok(tree)
```

> **`enum_members` typing:** core's `Node.enum_members` is `Vec<(String, i64)>` (was
> `QVector<QPair<QString,int64_t>>`). Test `enumBasic` checks `.first`/`.second`.

---

## 3. `reclass_xml.rs` — ReClass XML import + ReClassEx XML export

Ports `import_reclass_xml.cpp` (392 lines) and `export_reclass_xml.cpp` (222 lines).
Both **PURE**. Use `quick-xml` for read and write. See BMAP §2–§3.

### 3.0 Public signatures

```rust
// C++: NodeTree importReclassXml(const QString& path, QString*, int ptrSize=8)
pub fn import_reclass_xml(path: &Path, pointer_size: i32) -> Result<NodeTree, ImportError>;
// C++: bool exportReclassXml(const NodeTree&, const QString& path, QString*)
pub fn export_reclass_xml(tree: &NodeTree, path: &Path) -> Result<(), ImportError>;
```

### 3.1 Version + type maps (BMAP §2.1; cpp:17,55,83)

```rust
#[derive(Clone,Copy,PartialEq)] enum XmlVersion { V2013, V2016 }
```
Two `&[(i32, NodeKind)]` const tables (the XML `Type` integer's meaning differs per
version):

- **`K_TYPE_MAP_2016`** (~33 entries): 1→Struct, 4→Hex32, 5→Hex64, 6→Hex16, 7→Hex8,
  8→Pointer64, 9→Int64, 10→Int32, 11→Int16, 12→Int8, 13→Float, 14→Double, 15→UInt32,
  16→UInt16, 17→UInt8, 18→UTF8, 19→UTF16, 20→Pointer64, 21→Hex8, 22→Vec2, 23→Vec3,
  24→Vec4, 25→Mat4x4, 26→Pointer64, 27→Array, 29→Pointer64, 30→Pointer64, 31→UInt8,
  32→UInt64, 33→Pointer64.
- **`K_TYPE_MAP_2013`**: 1→Struct, 4→Hex32, 5→Hex16, 6→Hex8, 7→Pointer64, 8→Int32,
  9→Int16, 10→Int8, 11→Float, 12→UInt32, 13→UInt16, 14→UInt8, 15→UTF8, 16→Pointer64,
  17→Hex8, 18→Vec2, 19→Vec3, 20→Vec4, 21→Mat4x4, 22→Pointer64, 23→Array, 27→Int64,
  28→Double, 29→UTF16, 30→Array.

`fn lookup_kind(xml_type, ver, ptr_size) -> NodeKind` (cpp:83): linear-search the version
table (default `Hex8` on miss); **if `ptr_size < 8` and result is Pointer64, remap to
Pointer32**.

Predicates (small `match` per version):
- `is_pointer_type`: 2016 ∈ {8,20,26,29,30,33}; 2013 ∈ {7,16,22}.
- `is_class_instance_type`: == 1 (both).
- `is_class_instance_array_type`: 2016 == 27; 2013 ∈ {23,30}.
- `is_text_type`: 2016 ∈ {18,19}; 2013 ∈ {15,29}.
- `is_utf16_text_type`: 2016 == 19; 2013 == 29.
- `is_custom_type`: 2016 == 21; 2013 == 17.

### 3.2 Import parse loop (BMAP §2.2; cpp:142–390) — verified exact

`quick-xml` `Reader<BufReader<File>>` with `.trim_text(false)`. Iterate `read_event_into`.

> **`Event::Empty` handling (BMAP §2):** ReClass `<Node …/>` are self-closing → quick-xml
> yields `Event::Empty`. Treat `Empty` exactly like `Start` immediately followed by `End`.
> For `<Node>` this means: process attributes, then (for the array/custom paths that read
> nested children) there are no children to read.

Algorithm:
```
1. open file; on failure -> Err(CannotOpen(path))
2. version = V2016; tree.base_address = 0x0040_0000; tree.pointer_size = ptr_size
3. class_ids: HashMap<String,u64>; pending_refs: Vec<PendingRef>
4. version detection: on the FIRST Comment event (before version_detected):
     c = comment.trim() (case-insensitive contains):
       "ReClassEx"|"MemeClsEx"|"2016"|"2015" -> V2016
       "2013"|"2011" -> V2013
       else keep default V2016
     version_detected = true
5. on Start(name=="Class"):
     class_name = attr "Name"; (strOffset read but unused for layout)
     root = Node{kind:Struct, name:class_name, struct_type_name:class_name,
                 parent_id:0, offset:0, collapsed:true}
     struct_id = id of add_node(root); class_ids[class_name] = struct_id
     child_offset = 0
     loop reading events until End(name=="Class"):
       only handle Start/Empty(name=="Node"):
         xml_type = attr "Type".parse().unwrap_or(0)   # Qt toInt() → 0 on miss
         node_name = attr "Name"; node_size = attr "Size".parse().unwrap_or(0)
         ptr_class = attr "Pointer"; inst_class = attr "Instance"
         # (a) Custom (is_custom_type && node_size > 0): expand best-fit hex run
         #     (>=8 & %8 -> Hex64/8; >=4 & %4 -> Hex32/4; >=2 & %2 -> Hex16/2; else Hex8/1)
         #     count = node_size/hex_size; name = (count==1 ? node_name : ""); advance each
         # (b) kind = lookup_kind(xml_type, version, ptr_size)
         # (c) ClassInstanceArray (is_class_instance_array_type):
         #     total = attr "Total" (else "Count" else 1)
         #     read inner events until End(name=="Node"); on Start(name=="Array"):
         #        array_class_name = attr "Name"; array_total = "Total" (else "Count");
         #        if array_total > 0 { total = array_total }
         #     arr = Array{element_kind:Struct, name:node_name, offset:child_offset,
         #                 array_len:total, struct_type_name:array_class_name if set}
         #     add; if array_class_name set push PendingRef; child_offset += node_size if >0
         # (d) build Node{kind, name:node_name, parent_id:struct_id, offset:child_offset}
         #     Text (is_text_type): UTF16 -> str_len=max(1,node_size/2); else max(1,node_size)
         #     Pointer (is_pointer_type && !ptr_class.is_empty()): collapsed; add;
         #        PendingRef{id, ptr_class}; child_offset += node_size>0 ? node_size
         #                                                     : size_for_kind(kind)
         #     ClassInstance (is_class_instance_type): resolved = inst_class else ptr_class;
         #        struct_type_name = resolved; collapsed; if !resolved.is_empty add + PendingRef
         #        else add; child_offset += node_size if >0 else 0
         #     default: add; child_offset += node_size>0 ? node_size : size_for_kind(kind)
6. on reader EOF / Err:
     if it is a genuine parse error AND not a benign premature-EOF -> Err(XmlParse{line,msg})
     else fall through (return what was parsed)   # see note below
7. if tree.nodes.is_empty() -> Err(NoClasses)
8. resolve pending_refs (§1.2)
9. Ok(tree)
```

> **`PrematureEndOfDocumentError` tolerance (BMAP §2):** Qt treats a premature end as
> benign and returns what was parsed. `quick-xml` instead returns `Ok(Event::Eof)` at the
> clean end and only `Err` on malformed XML mid-stream. So: on `Event::Eof` → break the
> loop normally; on `Err(quick_xml::Error)` → map to `ImportError::XmlParse { line:
> reader.buffer_position() (or 0), msg }`. The line number is only used for the message
> (tests don't assert it). This reproduces "tolerate truncation, error on genuine
> malformed XML."

> **Offset behavior (BMAP §2, pinned by `importSmallXml`):** offsets are **purely
> sequential** — `child_offset` only ever advances by each node's `Size` (or
> `size_for_kind`), NEVER re-aligned (unlike `source.rs`). Test asserts running offsets
> 0, 8, 12, 44 from sizes 8, 4, 32, 12.

> **Qt `toInt()` semantics:** a missing/empty attribute → 0. Replicate with
> `attr.and_then(|s| s.parse().ok()).unwrap_or(0)` (parse-or-0). Use a helper
> `fn attr_int(e: &BytesStart, name: &str) -> i32` and `fn attr_str(...) -> String`.

### 3.3 Export (BMAP §3; cpp:11–220) — verified exact, always V2016

`fn xml_type_for_kind(NodeKind) -> i32` (cpp:11) — reverse of the 2016 map:
Struct→1, Hex32→4, Hex64→5, Hex16→6, Hex8→7, Pointer64→8, Pointer32→8, Int64→9, Int32→10,
Int16→11, Int8→12, Float→13, Double→14, UInt32→15, UInt16→16, UInt8→17, UInt64→32,
UTF8→18, UTF16→19, **Bool→17** (UInt8 — no native bool), Vec2→22, Vec3→23, Vec4→24,
Mat4x4→25, Array→27; **default fallback → 7 (Hex8)**. (Hex128, Int128, UInt128, Float16,
FuncPtr32/64 fall through to 7 — lossy.)

`fn node_size_for_export(node) -> i32` (cpp:42): UTF8→`str_len`; UTF16→`str_len*2`;
Array→`array_len * max(size_for_kind(element_kind), 0)`; else `size_for_kind(kind)`.

`fn resolve_struct_name(tree, ref_id) -> String` (cpp:55): referenced node's
`struct_type_name` (or `name` fallback); "" if `index_of_id` < 0.

**`export_reclass_xml`** algorithm (cpp:63):
```
1. if tree.nodes.is_empty() -> Err(NoNodesToExport)
2. open file for writing; on failure -> Err(CannotOpenWrite(path))
3. child_map: HashMap<u64, Vec<usize>> = for each node, push index under parent_id
4. writer = quick_xml Writer with indent(' ', 4); write_decl (<?xml version="1.0" ...?>)
   write Start("ReClass"); write Comment("ReClassEx")   # importer version-detection key
5. roots = child_map[0] sorted by node.offset
   for ri in roots where node.kind == Struct:
     write Start("Class") with attrs IN THIS ORDER:
       Name = (name.is_empty() ? struct_type_name : name)
       Type = "28"; Comment = ""; Offset = "0"; strOffset = "0"; Code = ""
     children = child_map[root.id] sorted by offset; i = 0
     while i < children.len():
       c = children[i]
       # (A) Bitfield container (c.kind==Struct && resolved_class_keyword()=="bitfield"):
       #   sz = c.byte_size(); if sz <= 0 { sz = 4 }
       #   hex_kind = sz<=1?Hex8 : sz<=2?Hex16 : sz<=4?Hex32 : Hex64
       #   write Empty("Node") attrs: Name=c.name, Type=xml_type_for_kind(hex_kind),
       #        Size=sz, bHidden="false", Comment="bitfield"; i+=1; continue
       # (B) Hex-run collapse (is_hex_node(c.kind)):
       #   run_start = c.offset; run_end = c.offset + c.byte_size(); j = i+1
       #   while j<len: next=children[j]; if !is_hex_node(next.kind) break;
       #        if next.offset < run_end break;   # overlap/gap stops the run
       #        run_end = next.offset + next.byte_size(); j+=1
       #   total = run_end - run_start
       #   name = (j-i==1 && !c.name.is_empty()) ? c.name : ""
       #   write Empty("Node"): Name=name, Type="21", Size=total, bHidden="false", Comment=""
       #   i = j; continue
       # (C) Generic node: write Start("Node") attrs IN ORDER:
       #     Name=c.name, Type=xml_type_for_kind(c.kind), Size=node_size_for_export(c),
       #     bHidden="false", Comment=""
       #   if (Pointer64|Pointer32) && c.ref_id != 0:
       #        t = resolve_struct_name(tree, c.ref_id); if !t.is_empty() attr Pointer=t
       #   if c.kind==Struct: attr Instance = (struct_type_name.is_empty()?name:struct_type_name)
       #   if c.kind==Array:
       #        attr Total = array_len
       #        elem_name = (element_kind==Struct && !struct_type_name.is_empty())
       #                       ? struct_type_name
       #                       : (ref_id!=0 ? resolve_struct_name(ref_id) : "")
       #        if elem_name.is_empty() { elem_name = kind_to_string(element_kind) }
       #        write Empty("Array"): Name=elem_name, Total=array_len
       #   write End("Node")
       #   i += 1
     write End("Class"); class_count += 1
6. write End("ReClass"); finish document
7. if class_count == 0 -> Err(NoClassesToExport)
8. Ok(())
```

> **Attribute order & formatting parity (BMAP §3):** the export tests only check
> *substrings* (`xml.contains("Type=\"21\"")`, `"Pointer=\"Target\""`, `"Instance=\"Inner\""`,
> `"Total=\"10\""`, `"<Array"`, `"ReClassEx"`). So byte-exact formatting is **not**
> required, but to keep these `contains` checks valid, emit attributes in the **call order
> above** and use double-quoted values. quick-xml `Writer::create_element(...).with_attributes([...])`
> preserves the given order. Generic nodes use `Start`+`End` (the C++ writes them
> non-self-closing because Pointer/Instance/Array attrs may be added; the bitfield and
> hex-collapse nodes can be `Empty`). The `<Array>` child forces the generic `<Node>` to
> be non-empty — match that.

> **Round-trip lossy behaviors the tests rely on** (`exportHexCollapse`,
> `roundTripImportExport`): 4×Hex8 → one Type=21 Size=4 → re-import expands to best-fit
> hex (Hex32). Bool→UInt8. 128-bit/Float16/FuncPtr→Hex8. Self-pointer `ref_id` survives
> via `Pointer="<name>"` and re-resolves on import. Round-trip compares **kind+name**
> (and sometimes `str_len`), NOT offsets for every kind.

---

## 4. `pe_debug_info.rs` — PE CodeView debug-dir over a `Provider`

Ports `pe_debug_info.cpp` (193 lines). **mostly-portable.** Reads raw little-endian PE
structs via discrete `Provider::read` calls (data may be live/in-memory, NOT a contiguous
file), so `object`/`goblin` cannot drive it — **hand-roll** the fixed-offset reads with
`bytemuck`. See BMAP §4. Verified exact against the C++ source.

### 4.0 Public signature + result struct

```rust
#[derive(Default, Clone)]
pub struct PdbDebugInfo {
    pub pdb_name: String,     // e.g. "ntoskrnl.pdb"
    pub guid_string: String,  // 32 hex chars, no dashes, uppercase
    pub age: u32,
    pub valid: bool,
}
// C++: PdbDebugInfo extractPdbDebugInfo(const Provider&, uint64_t moduleBase)
pub fn extract_pdb_debug_info(prov: &dyn Provider, module_base: u64) -> PdbDebugInfo;
```
Returns `PdbDebugInfo::default()` (`valid=false`) on any failed read / mismatch — it
**never errors**, it just returns invalid. The `Provider::read` signature (per
ARCHITECTURE.md §7) returns `Result<usize, _>`; treat any `Err` or short read as the C++
`!prov.read(...)` failure path → return the (invalid) result.

### 4.1 Packed structs (cpp:8–61) — `#[repr(C, packed)]`, `bytemuck::Pod`

Mirror the C++ `#pragma pack(1)` layout exactly. Read each via a fixed-size buffer +
`bytemuck::from_bytes` (or `pod_read_unaligned`). All multi-byte fields are interpreted
**little-endian** (targets are LE; use `from_le_bytes` if portability to BE hosts is
wanted, BMAP §8). Sizes that the algorithm depends on:
`DosHeader`=64 (`e_magic` u16, `pad[58]`, `e_lfanew` i32), `CoffHeader`=20 (7 fields),
`DataDirectory`=8 (VA u32, Size u32), `DebugDirectory`=28 (8 fields; need `Type`,
`SizeOfData`, `AddressOfRawData` (RVA), `PointerToRawData` unused),
`CvInfoPdb70`=24 (Signature u32, Guid[16], Age u32; name follows).
Constants: `K_MZ=0x5A4D`, `K_PE=0x0000_4550`, `K_PE32=0x10b`, `K_PE32P=0x20b`,
`K_RSDS=0x5344_5352`, `K_DEBUG_CODEVIEW=2`.

> The optional-header structs in the C++ (`OptionalHeader32/64`) are only used for their
> field offsets; this port reads `NumberOfRvaAndSizes` and the data-directory base at the
> hard-coded offsets below rather than casting the whole struct.

### 4.2 Algorithm (cpp:85–191) — port exactly

```
result = PdbDebugInfo::default()
read DosHeader @ module_base (fail -> return result); if e_magic != MZ -> return result
pe_offset = module_base + e_lfanew (e_lfanew is i32; sign-extend then wrapping_add)
read 4 bytes @ pe_offset -> pe_sig (LE u32); if != PE -> return
coff_offset = pe_offset + 4; read CoffHeader @ coff_offset
opt_offset = coff_offset + 20 (sizeof CoffHeader)
read 2 bytes @ opt_offset -> opt_magic
if opt_magic == PE32:   num_rva = read u32 @ opt_offset+92; data_dirs = opt_offset+96
elif opt_magic == PE32P: num_rva = read u32 @ opt_offset+108; data_dirs = opt_offset+112
else: return result
if num_rva <= 6 -> return result          # need Debug dir (index 6)
read DataDirectory @ data_dirs + 6*8 -> debug_dir
if debug_dir.VirtualAddress == 0 || debug_dir.Size == 0 -> return
num_entries = debug_dir.Size / 28
for i in 0..num_entries:
    entry_addr = module_base + debug_dir.VirtualAddress + i*28
    read DebugDirectory @ entry_addr (fail -> continue)
    if entry.Type != CodeView (2) -> continue
    if entry.AddressOfRawData == 0 || entry.SizeOfData < 24+1 -> continue
    cv_addr = module_base + entry.AddressOfRawData
    read CvInfoPdb70 @ cv_addr (fail -> continue)
    if cv.Signature != RSDS -> continue
    name_max = min(entry.SizeOfData - 24, 260)
    read name_max bytes @ cv_addr+24 into buf[261] (fail -> continue); buf[name_max]=0
    pdb_name = Latin1(buf up to NUL)
    strip path: after last '\\', then after last '/'
    guid_string = guid_to_string(cv.Guid)
    age = cv.Age; valid = true; return result
return result   # no CodeView match
```

`guid_to_string(guid: [u8;16]) -> String` (cpp:70) — **Windows mixed-endian GUID**:
```
d1 = u32::from_le_bytes(guid[0..4])
d2 = u16::from_le_bytes(guid[4..6])
d3 = u16::from_le_bytes(guid[6..8])
s = format!("{:08x}{:04x}{:04x}", d1, d2, d3)
for b in guid[8..16] { s += &format!("{:02x}", b) }     // sequential
s.to_uppercase()                                          // 32 hex chars, no dashes
```
Matches MS symbol-server expectations. **No test in `_oracle` exercises this directly**
(no provider fixture); add unit tests from a synthesized in-memory PE (§6.4).

---

## 5. `pdb.rs` — PDB type & symbol import

Ports `import_pdb.cpp` (1411 lines) using the `pdb2` crate, which **replaces RawPDB and
removes** the Win32 file-mapping (`MappedFile`) and the manual CodeView leaf decoding
(`leafSize`/`leafName`/`leafValue` — `pdb2` decodes these internally). See BMAP §5.

### 5.0 cfg-gating decision

The C++ implementation is entirely `#ifdef _WIN32` (with `#else` stubs returning
`"PDB import requires Windows"`). `pdb2` is pure Rust and portable, so we **keep the
public API cross-platform** (an improvement; ARCHITECTURE.md §3 and crate_selection.md
endorse this). All five public fns compile and run on Linux. No `#[cfg(windows)]`
needed here — the only Windows-specific thing was file mapping, which
`std::fs::File` + `pdb2::PDB::open` handle portably. (The C++'s `#else` stub branch is
*not* ported, since there is no platform where PDB is unavailable in Rust.)

### 5.1 Public API (cpp / import_pdb.h)

```rust
#[derive(Clone)] pub struct PdbSymbol { pub name: String, pub rva: u32, pub type_index: u32 } // type_index default 0
#[derive(Clone, Default)] pub struct PdbSymbolResult { pub module_name: String, pub symbols: Vec<PdbSymbol> }
#[derive(Clone)] pub struct PdbTypeInfo {
    pub type_index: u32, pub name: String, pub size: u64,
    pub child_count: i32, pub is_union: bool, pub is_enum: bool, // is_enum default false
}
pub type ProgressCb<'a> = dyn FnMut(i32, i32) -> bool + 'a; // false = cancel

pub fn extract_pdb_symbols(path: &Path) -> Result<PdbSymbolResult, ImportError>;
pub fn enumerate_pdb_types(path: &Path) -> Result<Vec<PdbTypeInfo>, ImportError>;
pub fn import_pdb_selected(path: &Path, type_indices: &[u32],
                           progress: Option<&mut ProgressCb>) -> Result<NodeTree, ImportError>;
pub fn import_pdb(path: &Path, struct_filter: &str) -> Result<NodeTree, ImportError>; // filter "" = all
pub fn import_type_for_symbol(path: &Path, type_index: u32,
                              type_name_out: &mut String) -> Result<NodeTree, ImportError>;
```

### 5.2 Item-by-item mapping to `pdb2`

| C++ (import_pdb.cpp) | Rust / `pdb2` counterpart | Notes |
|---|---|---|
| `MappedFile` (cpp:26, Win32 mmap) | `pdb2::PDB::open(File::open(path)?)` | crate reads from any `Source` (File). `memmap2` optional, not needed. |
| `PdbFile::open` (cpp:915) | `fn open_pdb(path) -> Result<PDB,_>` | exists-check → `PdbNotFound`; open fail → `PdbMapFailed`; `PDB::open` Err → `PdbInvalid`; `pdb.type_information()` Err → `PdbNoTpi`. |
| `TypeTable` (cpp:66, O(1) index→record) | `pdb2::TypeInformation` + `ItemFinder`/`TypeFinder` **or** build `HashMap<TypeIndex, TypeData>` by iterating `type_information.iter()` | `firstIndex()`/`lastIndex()`/`count()`/`get(idx)` → `TypeFinder` (`find(TypeIndex)`), or our own map. A primitive check `idx < first_index` → `type_index.0 < 0x1000` (primitives are < 0x1000). |
| `leafSize`/`leafName`/`leafValue` (cpp:110) | **gone** — `pdb2` exposes decoded `.size()`, member `.offset`, enum `.value`, array byte-len | the variable-length CodeView numeric leaf decoding is internal to the crate. |
| `unionLeafKind` | (internal) | not needed |
| `mapPrimitiveType(typeIndex)` (cpp:147) | `fn map_primitive(idx: TypeIndex) -> NodeKind` (§5.4) | still hand-written; map `pdb2::PrimitiveKind` or the raw `&0xFF`/`>>8&0xF` bits. |
| `hexForSize(len)` (cpp) | `fn hex_for_size(len: u64) -> NodeKind` | 1→Hex8,2→Hex16,4→Hex32,8→Hex64, else Hex32 |
| `PdbCtx` (cpp:232) | `struct PdbCtx<'t> { tree, finder, type_cache: HashMap<u32,u64>, struct_def_by_name, union_def_by_name, udt_def_index_built }` | `type_cache` keyed on typeIndex prevents re-import + handles recursion. |
| `buildUdtDefinitionIndex` (cpp:258) | `fn build_udt_definition_index(&mut self)` | one-time scan: record FIRST typeIndex per name for non-fwdref LF_UNION/STRUCTURE/CLASS |
| `findUdtDefinitionIndex(kind,name)` | `fn find_udt_definition_index(&self, is_union, name) -> Option<u32>` | fwd-ref → definition |
| `unwrapModifier` (LF_MODIFIER) | `fn unwrap_modifier(&self, idx) -> u32` | follow `TypeData::Modifier{underlying_type}` |
| `importUDT(typeIndex)` (cpp:308) | `fn import_udt(&mut self, idx: u32) -> u64` (returns node id, 0 = fail) | §5.5 |
| `importEnum(typeIndex)` (cpp:360) | `fn import_enum(&mut self, idx: u32) -> u64` | §5.5 |
| `importFieldList(idx, parentId)` (cpp:413) | `fn import_field_list(&mut self, idx: u32, parent_id: u64)` | §5.6 |
| `importMemberType(idx, offset, name, parentId)` (cpp:557) | `fn import_member_type(&mut self, idx: u32, offset: i32, name, parent_id: u64)` | §5.7 |
| `extractPdbSymbols` (cpp:948) | `extract_pdb_symbols` (§5.8) | DBI public + global symbols |
| `enumeratePdbTypes` (cpp:1065) | `enumerate_pdb_types` (§5.8) | fast scan, no recursion |
| `importPdbSelected` (cpp:1164) | `import_pdb_selected` (§5.8) | per-type dispatch + progress cb |
| `importPdb` (cpp:1211) | `import_pdb` (§5.8) | legacy; filter by name |
| `importTypeForSymbol` (cpp:1266) | `import_type_for_symbol` (§5.8) | |

### 5.3 `pdb2` type mapping cheat-sheet (BMAP §5)

- TPI stream → `pdb.type_information()?`; iterate `tpi.iter()` yielding `Type<'_>`;
  `type.parse()? -> TypeData`. Variants used: `TypeData::Class`, `Union`, `Enumeration`,
  `Member`, `Pointer`, `Array`, `Modifier`, `Bitfield`, `Procedure`, `MemberFunction`,
  `FieldList`, `BaseClass`, `VirtualBaseClass`, `Nested`, `StaticMember`, `Method`,
  `OverloadedMethod`/`Method`, `VirtualFunctionTablePointer`.
- forward-ref test: `Class.properties.forward_reference()` /
  `Union.properties.forward_reference()` / `Enumeration.properties.forward_reference()`.
- size: `Class.size` / `Union.size` (u16/u32) / enum underlying. Member offset:
  `Member.offset`. Bitfield: `Bitfield { underlying_type, length, position }`.
- DBI: `pdb.debug_information()?`; symbols via `pdb.global_symbols()?` and module symbols;
  RVA via `address_map = pdb.address_map()?` and `SymbolData::offset.to_rva(&address_map)`.
  Public symbols → `SymbolData::Public`; global data → `SymbolData::Data` /
  `ThreadStorage`.

### 5.4 `map_primitive` (cpp:147) — port verbatim (BMAP §5.2)

For type indices `< 0x1000`, mask `& 0xFF` for the base type:
0x03 void→Hex8; 0x10/0x70/0x68→Int8; 0x20/0x69→UInt8; 0x71/0x7a→UInt16; 0x7c→UInt8;
0x7b→UInt32; 0x11/0x72→Int16; 0x21/0x73→UInt16; 0x12/0x74→Int32; 0x22/0x75→UInt32;
0x13/0x76→Int64; 0x23/0x77→UInt64; 0x40→Float; 0x41→Double; 0x30→Bool;
0x31/0x32/0x33→UInt16/UInt32/UInt64; 0x08→UInt32 (HRESULT); 0x60→UInt8 (bit);
0x78/0x79→Hex64 (int128/uint128 best-effort); default→Hex32.

**Pointer mode** of a primitive index = `(idx >> 8) & 0xF`: 0x04/0x05→32-bit ptr;
0x06→64-bit ptr; other nonzero→32-bit ptr; 0→direct base type. Used in `import_member_type`.

> With `pdb2` you may instead match `tpi.find(idx)?.parse()` returning a primitive — but
> the raw-bit approach mirrors the C++ exactly and avoids crate-version surprises. If you
> read primitives via `pdb2::PrimitiveType { kind, indirection }`, map `PrimitiveKind` →
> `NodeKind` per the table and `Indirection` → pointer kind; either path must produce the
> same `NodeKind`.

### 5.5 `import_udt` / `import_enum` (cpp:308, 360)

`import_udt(idx)`: bail if `idx < first_index` (primitive); return cached id if present.
For Class/Structure: skip if `forward_reference()` (return 0); else read field count,
field-list index, name. For Union: same with `is_union=true`. Create root `Struct{name,
struct_type_name=name, class_keyword = if union {"union"} else {"struct"}, parent_id:0,
collapsed}`; **cache the id BEFORE recursing** (`type_cache[idx]=id`) so self/mutual refs
resolve; then `import_field_list(field_list_idx, id)`. Return id.

`import_enum(idx)`: require Enumeration, non-fwdref; create root `Struct{class_keyword=
"enum"}`; walk its field list collecting `Enumerate` members into `enum_members`
(name + decoded value); stop at first non-Enumerate. With `pdb2` iterate the
`FieldList` and match `TypeData::Enumerate { name, value }` (value already decoded — the
C++'s 4-byte-aligned manual walk is unnecessary). Cache + return id.

### 5.6 `import_field_list` (cpp:413) — the member iterator (BMAP §5.3)

With `pdb2`, get the `FieldList` (`TypeData::FieldList { fields, continuation }`) and
iterate `fields`. Handle each variant; **preserve C++ skip/handle semantics**:
- `Member { name, field_type, offset }`: `unwrap_modifier(field_type)`; if it resolves to
  `Bitfield` → group bitfield members into a shared container keyed by `(offset,
  slot_size)`: first member creates `Struct{class_keyword="bitfield",
  element_kind=hex_for_size(slot_size), offset, collapsed=false}`; each member appends a
  `BitfieldMember{name, bit_offset=position, bit_width=length}`. `slot_size =
  size_for_kind(map_primitive(underlying))` for primitive underlying, else 4. Otherwise
  call `import_member_type(field_type, offset, name, parent_id)`.
- `BaseClass` → skip. `VirtualBaseClass`/`IndirectVirtualBaseClass` → skip.
- continuation (`LF_INDEX`) → recurse into the next field-list record (`pdb2` exposes this
  as `continuation: Option<TypeIndex>`; recurse `import_field_list(cont, parent_id)`).
- `VirtualFunctionTablePointer`, `Nested`, `StaticMember`, `Method`, `OverloadedMethod`,
  `Enumerate` → skip. Unknown → stop.

> The bitfield grouping requires stable ordering and the `(offset, slot_size)` key — use
> an `IndexMap<(i32,i32), usize>` (key→bitfield-container node index) so members append in
> order. Pinned by `verifyListEntry` (Flink/Blink are Pointer64; self-ref) and PDB
> bitfield handling.

### 5.7 `import_member_type` (cpp:557) — one node per member (BMAP §5.3)

Emit exactly one `Node`. Cases (port branch-for-branch):
- **Primitive** (`idx < first_index`): pointer-mode bits → 0x04/0x05 Pointer32, 0x06
  Pointer64, other-nonzero Pointer32 (collapsed); 0 → `map_primitive` base type.
- **Modifier** → recurse on underlying.
- **Pointer** `{ underlying_type, attributes }`: kind = Pointer32 if `attr.size()<=4`
  else Pointer64 (collapsed). `unwrap_modifier` the pointee. If pointee is a UDT
  (Class/Union): resolve fwdref via `find_udt_definition_index`; **skip anonymous targets**
  (name empty / starts with `<`) to avoid root orphans; else `ref_id = import_udt(def)`.
  If pointee is Procedure/MemberFunction → kind becomes FuncPtr32/64.
- **Class/Structure/Union embedded**: resolve fwdref to definition; **anonymous types are
  inlined** — create a `Struct` container (no ref_id, parented to current parent) and
  `import_field_list` directly into it. Named → `ref_id = import_udt(def)` and emit
  `Struct{struct_type_name=type_name, class_keyword = union?"union":"struct", ref_id}`.
  Pinned by `importKProcess` (Header→`_DISPATCHER_HEADER` @0; ProfileListHead→`_LIST_ENTRY`
  @0x18).
- **Array** `{ element_type, indexing_type, stride, dimensions }`: total byte size from
  the array's leaf; element size from the (modifier-unwrapped) element type: primitive→
  `size_for_kind`; class/union→leaf size; pointer→attr.size; enum→underlying primitive
  size (else 4); nested array→leaf. `count = if elem_size>0 { total/elem_size } else {1}`.
  `element_kind`: primitive→`map_primitive`; class/union→Struct + `ref_id=import_udt` +
  `struct_type_name`; pointer→Pointer32/64; else `hex_for_size(elem_size)`.
- **Enumeration** → map to underlying primitive kind (else UInt32) + `ref_id=import_enum`.
- **Procedure/MemberFunction** → Hex64.
- **Bitfield** (top-level, not in member group) → single-member bitfield `Struct`.
- **default / unknown / finder miss** → Hex32.

### 5.8 Public entry points (cpp:915–1378)

- **`open_pdb`** (cpp:915): `Path::exists` → `PdbNotFound`; open/parse failures per §5.2.
- **`extract_pdb_symbols`** (cpp:948): validate DBI + symbol/public/section streams;
  `module_name = path.file_stem()` (e.g. "ntkrnlmp"). Read **public symbols** (Public
  only; RVA via address_map; skip rva==0) and **global symbols** (`Data`/`ThreadStorage`
  with name+typeIndex+section/offset; skip rva==0 or empty name). Build
  `PdbSymbol{name, rva, type_index}`.
- **`enumerate_pdb_types`** (cpp:1065): scan all TPI records; for each non-fwdref UDT/enum
  with a real name (not empty / not starting `<`), emit `PdbTypeInfo{type_index, name,
  size, child_count=field_count, is_union, is_enum}`. Enum size = underlying primitive
  size (else 4); union/struct size = leaf size. (No need to port the qDebug
  skip-diagnostics.) Pinned by `enumerateTypes`.
- **`import_pdb_selected`** (cpp:1164): for each requested typeIndex, dispatch to
  `import_enum` (if Enumeration) else `import_udt`; call `progress(i+1, total)` after each
  — **if it returns false, abort and return the partial tree** (`Err(PdbCancelled)`? — no:
  the C++ returns the *partial tree* with errorMsg "Import cancelled". For Rust, return
  `Ok(partial_tree)` and surface cancellation via the bool; OR keep `Err(PdbCancelled)` if
  the caller prefers — the test `importSelected` always returns true so this path is
  untested. **Match the C++: return the partial tree, do NOT discard it.** Use an out
  channel or return `Ok(tree)` even when cancelled). Empty result → `Err(PdbNoTypesImported)`.
- **`import_pdb`** (legacy, cpp:1211): iterate TPI; for non-fwdref named UDTs, if filter
  empty import all, else import only the matching name and **break** after the first match.
  Empty → `Err(PdbTypeNotFound(filter))` if filter non-empty else `Err(PdbNoTypes)`.
  Transitive deps (`_DISPATCHER_HEADER`, `_LIST_ENTRY`) come in as extra roots via
  `import_udt` recursion. Pinned by `importKProcess`/`verifyDispatcherHeader`/
  `verifyListEntry`/`importFilteredStruct`.
- **`import_type_for_symbol`** (cpp:1266): typeIndex 0 → `Err(SymbolNoType)`. Walk
  Modifier/Pointer chains (≤16 deep) to the underlying. If primitive →
  `Err(ResolvesToPrimitive(idx))`. Must land on UDT/enum (else err). Extract `*type_name`.
  Resolve fwdrefs; `import_udt`/`import_enum`. Empty → `Err(FailedImportType(idx))`.

> **No threading.** `progress` is a synchronous callback; cancellation returns the partial
> tree (do not spawn threads).

---

## 6. TEST PLAN

All oracle targets for this subsystem **PASS** upstream (RESULTS.md): `test_import_source`
(52/0/0), `test_import_xml` (3/0/0), `test_export_xml` (12/0/0). `test_import_pdb` is
`if(WIN32)`-gated and was **not built** on Linux; `test_roundtrip_winsdk` is not in the
captured oracle set but its fixture exists. Logic tests run with
`--no-default-features` + `--features imports` (no gpui).

Translate each C++ `QtTest` slot → a Rust `#[test]`. Vendor the source fixtures into
`tests/fixtures/imports/` (and `src/examples/`). The golden source files are copied at
`_oracle/test_sources/test_import_source.cpp`, `test_import_xml.cpp`, `test_export_xml.cpp`
— port their assertions literally.

### 6.1 `tests/import_source.rs` ← `test_import_source.cpp` (52 asserts; 50 slots)

Port **every** slot. Helpers `count_roots(&tree)` (parent_id==0 && kind==Struct) and
`children_of(&tree, parent_id) -> Vec<usize>` (mirror the C++ test helpers). Slot list
and what each pins (all map 1:1 to §2 behavior):

`empty_input` (Err), `no_structs` (`"int x = 42;"` → Err), `single_empty_struct`,
`stdint_types`, `windows_types`, `platform_pointer_types` (PVOID/HANDLE→Pointer64 vs
SIZE_T/ULONG_PTR/uintptr_t/size_t→UInt64), `standard_c_types` (long→Int32),
`multi_word_types`, `float_double`, `bool_type`, `void_pointer` (refId==0),
`typed_pointer` (refId→Target), `self_referencing_pointer` (refId==root id),
`double_pointer` (`void**`→Pointer64), `primitive_array` (Array len 10, elem Int32),
`char_array_to_utf8` (str_len 64), `wchar_array_to_utf16` (str_len 32),
`float_array_to_vec2/3/4`, `float_array_4x4_to_mat4x4`, `generic_float_array` (Array len
8, elem Float), `struct_array` (Array len 5, elem Struct), `comment_offsets`
(0,8 + double @0x10), `computed_offsets` (0,2,4,8 — natural alignment),
`mixed_offsets_auto_detect` (0,4,0x10), `multi_struct` (3 roots), `pointer_cross_ref`,
`forward_declaration`, `union_container` (struct_span==4; `after` @4),
`union_with_comment_offsets` (union @0x8, members @0, `b` @0xC), `named_union`
(name "u3", struct_span==8), `padding_field_expansion` (2×Hex64 @0/@8),
`static_assert_tail_padding` (struct_span==0x10), `embedded_struct`, `typedef_basic`,
`const_volatile_qualifiers`, `struct_prefix_on_type`, `bitfield_skipped` (container @4,
2 members, bitA width 4 off 0 / bitB width 12 off 4; `after` @8),
`bitfield_with_offsets_emits_hex` (container @4, element_kind Hex64, 4 members; `after`
@0xC), `hex_array_sizes` (Array len 0x20), `windows_style_peb` (4×UInt8 + 2×Pointer64),
`class_keyword` ("class"), `inheritance_skipped`, `enum_basic`, `enum_auto_values`,
`enum_hex_values`, `enum_in_struct` (UInt32 + refId), `enum_class`, `basic_round_trip`.

`basic_round_trip` and `enum_basic` assert on `enum_members` as `Vec<(String, i64)>`.
`QCOMPARE(...second, 0LL)` → assert the i64 value. Use the §2 builder directly; these are
**input-exact, deterministic** (no golden file needed — the source string is in the test).

### 6.2 `tests/import_xml.rs` ← `test_import_xml.cpp` (`import_small_xml`)

Embed the same XML string (test uses a temp file; in Rust write to a `tempfile::NamedTempFile`
or `tests/fixtures/imports/small.reclass` and call `import_reclass_xml(path, 8)`).
Assert exactly (from §3.2): 6 nodes total; node[0] Struct "TestClass"; node[1] Int64
"vtable" @0; node[2] Float "health" @8; node[3] UTF8 str_len 32 @12; node[4] Vec3 @44;
node[5] Pointer64 "pNext" with `ref_id == nodes[0].id` (self-ref resolved). The sequential
offsets 0/8/12/44 are the key parity check (no alignment).

### 6.3 `tests/export_xml.rs` ← `test_export_xml.cpp` (10 slots)

Port helpers `export_to_string(&tree) -> String` (write to temp file, read back) and
`round_trip(&tree) -> NodeTree` (export then import). Slots:
- `export_empty_tree` → `Err` (NoNodesToExport).
- `export_single_struct` → string contains "Player","health","speed","ReClassEx";
  round-trip: 1 root "Player", 3 kids Int32/Float/UInt64.
- `export_pointer_ref` → contains `Pointer="Target"`; round-trip pointer resolves
  (`ref_id != 0`).
- `export_embedded_struct` → contains `Instance="Inner"`.
- `export_array` → contains `Total="10"` and `<Array`.
- `export_text_nodes` → round-trip: UTF8 str_len 32, UTF16 str_len 16.
- `export_vectors` → round-trip: Vec2/Vec3/Vec4/Mat4x4.
- `export_hex_collapse` → contains `Type="21"` and `Size="4"`; round-trip: ≥2 kids, last
  is Int32 (Custom(4) → Hex32 + Int32).
- `export_multi_class` → round-trip: 5 roots, all class names "Class0".."Class4" present.
- `round_trip_import_export` → full kind sweep (Int8..Vec4 + self-pointer + UTF8);
  round-trip preserves **kind+name** per field (NOT offset for all kinds), and the
  self-pointer's `ref_id == root id`.

These are **substring / structural** assertions (not byte-exact), so quick-xml output is
fine as long as attribute order matches §3.3.

### 6.4 `tests/pe_debug_info.rs` (NEW — no C++ test, but `extract_pdb_debug_info` is in scope)

No upstream test exercises this (BMAP §4; needs a Provider fixture). Add Rust unit tests:
- Build a minimal synthetic PE byte-buffer in memory: DOS header (MZ + e_lfanew),
  PE sig, COFF header, a PE32+ optional header with `NumberOfRvaAndSizes > 6`, a debug
  data directory (index 6) pointing at a `DebugDirectory{Type=CodeView}` whose
  `AddressOfRawData` points at a `CvInfoPdb70{Signature=RSDS, Guid, Age}` + a
  null-terminated `"ntoskrnl.pdb"`. Wrap it in a `BufferProvider` (treat the buffer base
  as `module_base`, RVAs == file offsets for the test). Assert `valid==true`,
  `pdb_name=="ntoskrnl.pdb"`, `age` matches, and `guid_string` matches the mixed-endian
  formatting of a known GUID (compute the expected 32-char uppercase string by hand to
  pin `guid_to_string`).
- Negative: non-MZ buffer → `valid==false`; truncated buffer → `valid==false`.

### 6.5 `tests/import_pdb.rs` ← `test_import_pdb.cpp` (Windows-gated upstream)

Upstream slots (`missingFileReturnsError`, `importKProcess`, `verifyDispatcherHeader`,
`verifyListEntry`, `importFilteredStruct`, `enumerateTypes`, `importSelected`) all `QSKIP`
without `C:/Symbols/.../ntkrnlmp.pdb` and the target wasn't built on Linux (RESULTS.md).
Port plan:
- **`missing_file_returns_error`** — the ONLY one runnable anywhere: `import_pdb("nonexistent.pdb", "")`
  → `Err` (PdbNotFound). Port directly; runs cross-platform.
- The ntkrnlmp-dependent slots: keep them but gate behind an env var
  (`#[ignore]` or `if let Ok(p) = env::var("RECLASS_TEST_PDB")`). For CI parity, ALSO add a
  **cross-platform fixture PDB** — `pdb2` ships test PDBs in its repo, or generate a small
  one with MSVC/`llvm`/`cvdump`; vendor it under `tests/fixtures/imports/`. Then port:
  - `enumerate_types`: >0 types; find a known struct with `child_count>0`/`size>0`.
  - `import_filtered`: importing one struct → exactly 1 root if it self-references only.
  - `import_selected`: `import_pdb_selected` with one index; progress called >0 times;
    self-referencing pointers resolve; exactly 1 root.
  - `verify_*` (Flink/Blink Pointer64 self-ref; embedded structs at expected offsets) —
    use whatever fixture struct exposes a self-referencing list-entry pattern.
  The exact `_KPROCESS`/`_LIST_ENTRY` offsets are ntkrnlmp-specific; for a generic fixture,
  assert the *structural invariants* (self-ref pointers, embedded named structs, root
  counts), not the literal offsets.

### 6.6 `tests/roundtrip_winsdk.rs` ← `test_roundtrip_winsdk.cpp` (needs generator)

Fixture `src/examples/windows-x86_64.h` (1.3 MB; **present** in `reclass-cpp/src/examples/`)
— vendor it into the Rust `src/examples/`. This test couples `imports::source` with
`generator`, so it can only fully run once `generator` is ported; stage it accordingly.
Slots:
- `init` + `import_count`: `import_from_source(header, 8)` yields **≥ 3000** root structs.
- `peb_offsets`: find `_PEB` root; verify the ~42 named field offsets in the C++ list
  (InheritedAddressSpace@0x000 … NtGlobalFlag2@0x7C4, several `must_be_pointer`). This is a
  **pure `imports::source` test** (no generator) and the strongest parity check for the
  alignment/struct-sizing logic (§2.6) — port it even before generator lands.
- `round_trip_30`: deterministic shuffle (seed 42) of roots; for each, `render_cpp` →
  `import_from_source` → `render_cpp`; require **≥30** byte-identical pass pairs. Needs
  `generator`; gate until then.
- `generate_rcx`: set roots collapsed, `base_address=0xFFFFF80000000000`, `tree.to_json()`
  → ≥1 MB. Needs `core::NodeTree::to_json` (the RCX serializer) — a `core` dependency, not
  `imports`; can assert size only.

> When `generator` isn't yet available, `#[cfg_attr(not(feature="generator-ready"), ignore)]`
> the generator-coupled slots and keep `import_count`/`peb_offsets` active.

---

## 7. Ordered, independently-verifiable work steps

Each step compiles with `cargo build --no-default-features --features imports` and runs
its tests with the same. Steps are ordered so each is verifiable against the oracle alone.

1. **`mod.rs` scaffolding** — `ImportError`, `PendingRef`, the `add_node` index/id pattern
   helper, the name→id resolution post-pass. No tests yet; just compiles. (§1)
2. **`source.rs` type table + tokenizer** — `build_type_table`, `tokenize` (+ the two
   offset regexes). Unit-test the tokenizer and the type table against `stdint_types`,
   `windows_types`, `platform_pointer_types`, `multi_word_types` expectations (kind lookup
   only). (§2.2–§2.3)
3. **`source.rs` parser** — `Parser` + all `parse_*` methods producing `ParsedStruct`s.
   Unit-test parse output (struct/field/enum/union/typedef shapes) on small inputs. (§2.4–§2.5)
4. **`source.rs` builder + top-level** — `build_fields`, `emit_hex_padding`,
   `emit_bitfield_group`, alignment helpers, `import_from_source`. Run the FULL
   `tests/import_source.rs` (50 slots) → must match the 52-assert oracle. (§2.6–§2.7, §6.1)
5. **`reclass_xml.rs` import** — type maps, predicates, `lookup_kind`, the quick-xml parse
   loop (incl. `Event::Empty` handling + premature-EOF tolerance). Run
   `tests/import_xml.rs::import_small_xml`. (§3.1–§3.2, §6.2)
6. **`reclass_xml.rs` export** — `xml_type_for_kind`, `node_size_for_export`,
   `resolve_struct_name`, `export_reclass_xml` (attribute order, hex-collapse, bitfield,
   array). Run `tests/export_xml.rs` (10 slots, incl. round-trips through step 5). (§3.3, §6.3)
7. **`pe_debug_info.rs`** — packed structs (bytemuck), `guid_to_string`,
   `extract_pdb_debug_info` over the `Provider` trait. Run the synthetic-PE unit tests
   (§6.4). Depends on `provider` (file/buffer providers).
8. **`pdb.rs` infrastructure** — `open_pdb`, the type finder/cache, `map_primitive`,
   `hex_for_size`, `unwrap_modifier`, `build_udt_definition_index`. Run
   `import_pdb::missing_file_returns_error` (cross-platform). (§5.1–§5.4)
9. **`pdb.rs` type import** — `import_udt`/`import_enum`/`import_field_list`/
   `import_member_type` (bitfield grouping, embedded/anonymous inlining, arrays, pointers,
   enums). Run against the vendored fixture PDB: structural invariants (self-ref pointers,
   root counts, embedded named structs). (§5.5–§5.7, §6.5)
10. **`pdb.rs` public entry points** — `enumerate_pdb_types`, `import_pdb_selected`
    (+progress), `import_pdb`, `import_type_for_symbol`, `extract_pdb_symbols`. Finish
    `tests/import_pdb.rs`. (§5.8)
11. **`roundtrip_winsdk.rs` (partial)** — vendor `windows-x86_64.h`; run `import_count`
    (≥3000 roots) and `peb_offsets` (the ~42-field check). The strongest `source.rs`
    parity gate. (§6.6)
12. **`roundtrip_winsdk.rs` (full)** — once `generator` is ported, enable `round_trip_30`
    and `generate_rcx`. (§6.6)
```
