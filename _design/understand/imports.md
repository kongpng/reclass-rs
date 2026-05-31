# Subsystem: Importers / Exporters (`src/imports/`)

Scope of this report: the C++ files under `src/imports/`:
`import_source.{cpp,h}` (C/C++ source parser), `import_reclass_xml.{cpp,h}`,
`export_reclass_xml.{cpp,h}`, `import_pdb.{cpp,h}` (PDB type/symbol import),
`pe_debug_info.{cpp,h}` (PE CodeView debug-dir extraction). Tests:
`tests/test_import_source.cpp`, `test_import_xml.cpp`, `test_export_xml.cpp`,
`test_import_pdb.cpp`, `test_roundtrip_winsdk.cpp`.

**NOT in this subsystem** (clarifications, since the task title lists "RCX JSON" and
"C/C++"):
- **RCX native JSON** = `NodeTree::toJson`/`fromJson` + `Node::toJson`/`fromJson`,
  defined inline in `src/core.h` (lines 263–354, 793–833). It is the *core data
  model* serializer, not an `imports/` file. `test_roundtrip_winsdk.cpp::generateRcx`
  exercises it. Document it where the core model is documented; it is summarized
  briefly here because the importers produce the `NodeTree` it serializes.
- **C/C++ *export*** (`renderCpp`, `renderRust`, …) lives in `src/generator.{cpp,h}`
  — a *code-generation* subsystem, separate from `imports/`. The imports subsystem
  only does the *import* direction for C/C++ (`importFromSource`). The
  round-trip test (`test_roundtrip_winsdk.cpp::roundTrip30`) calls `renderCpp`
  (generator) → `importFromSource` (imports) → `renderCpp` again and checks the
  two generated strings are byte-identical. The Rust port of the *imports* side
  must reproduce `importFromSource` behavior precisely so that round-trip holds;
  the generator is ported elsewhere.

All importers/exporters live in `namespace rcx`. All produce/consume a `NodeTree`
(the universal in-memory struct-layout model).

---

## 0. The shared data model (just enough `core.h` to port the importers)

`NodeKind` (`core.h:24`): a flat enum of leaf and container kinds:
`Hex8/16/32/64/128, Int8/16/32/64/128, UInt8/16/32/64/128, Float16, Float, Double,
Bool, Pointer32, Pointer64, FuncPtr32, FuncPtr64, Vec2, Vec3, Vec4, Mat4x4, UTF8,
UTF16, Struct, Array`.

`kKindMeta[]` (`core.h:64`) is the single source of truth for: JSON `name`
("Hex64","UInt16"…), C-display `typeName` ("hex64","uint16_t"…), `size`
(0 for Struct/Array), `lines`, `align`, and flag bits. Importers use:
- `sizeForKind(NodeKind)` → byte size (0 for containers) (`core.h:108`).
- `alignmentFor(NodeKind)` → natural alignment (`core.h:110`).
- `kindToString(NodeKind)` → JSON name; `kindFromString` (`core.h:112`,117).
- `isHexNode(k)` true for Hex8..Hex128 (`core.h:138`).

`Node` (`core.h:210`) — fields the importers set:
- `uint64_t id` (assigned by `NodeTree::addNode` if 0), `NodeKind kind`,
  `QString name`, `QString structTypeName` (e.g. "_LIST_ENTRY"),
  `QString classKeyword` ("struct"/"class"/"union"/"enum"/"bitfield"; empty ⇒ "struct"),
  `uint64_t parentId` (0 = root), `int offset` (relative to parent),
  `bool isStatic`, `bool isRelative`, `int arrayLen` (≥1, ≤`kMaxArrayLen`=1,000,000),
  `int strLen` (default 64), `bool collapsed`, `uint64_t refId` (id of struct to
  expand at a pointer/embedded-struct), `NodeKind elementKind` (array element type;
  also reused as the *bitfield container* hex kind), `int ptrDepth`,
  `QVector<QPair<QString,int64_t>> enumMembers`,
  `QVector<BitfieldMember> bitfieldMembers`, `QString comment`, `bool bigEndian`.
- `BitfieldMember` (`core.h:202`): `{QString name; uint8_t bitOffset; uint8_t bitWidth(1..64);}`.
- Helpers: `resolvedClassKeyword()` (empty→"struct"), `isUnion()`, `isBitfield()`
  (`classKeyword=="bitfield"`), `isEnum()`.
- `byteSize()` (`core.h:238`): UTF8→`strLen`; UTF16→`min(strLen,INT_MAX/2)*2`;
  Array→`min(arrayLen,INT_MAX/elemSz)*elemSz` (0 if elemSz≤0); Struct→0 unless
  it is a `"bitfield"` (then `sizeForKind(elementKind)`, ≥4); else `sizeForKind(kind)`.

`NodeTree` (`core.h:408`) — fields/methods used by importers:
- `QVector<Node> nodes`, `uint64_t baseAddress` (default `0x00400000`),
  `int pointerSize` (4 or 8), `uint64_t m_nextId` (starts 1).
- `int addNode(const Node&)` (`core.h:431`): copies the node; if `id==0` assigns
  `m_nextId++`; else bumps `m_nextId` past it; appends; updates id/child caches if
  warm; bumps generation; returns the **index** (not id). Importers do
  `int idx = tree.addNode(n); uint64_t id = tree.nodes[idx].id;`.
- `int indexOfId(uint64_t)` (`core.h:594`), `QVector<int> childrenOf(uint64_t)`
  (`core.h:602`) — both lazily build caches.
- `int structSpan(uint64_t, …)` (`core.h:752`): cycle-safe max-extent of a struct:
  `max(declaredByteSize, max over non-static children of child.offset + childSpan)`.
  Used by `import_source` to size embedded structs and by `export` for hex sizing.

**Rust mapping**: `NodeKind` → a `#[repr(u8)]` enum or plain enum; `kKindMeta` →
a `const` table or `match` arms; `Node`/`NodeTree` → structs with `Vec<Node>`. Qt's
`QString` → `String`; `QVector` → `Vec`; `QHash`/`QSet` → `HashMap`/`HashSet`;
`QPair<A,B>` → `(A,B)`.

### RCX native JSON (core.h, for context)
`Node::toJson` (`core.h:263`) emits keys: `id`,`kind` (as name string),`name`,
`structTypeName`(omit if empty),`classKeyword`(omit if empty/"struct"),`parentId`,
`offset`,`isStatic`(only if true),`offsetExpr`,`isRelative`(if true),`arrayLen`,
`strLen`,`collapsed`,`refId`,`elementKind`,`ptrDepth`(if >0),`enumMembers`
(array of `{name,value(string)}`),`bitfieldMembers`(array of `{name,bitOffset,bitWidth}`),
`comment`(if non-empty),`bigEndian`(if true). IDs and `refId`/`parentId` are stored as
**decimal strings** (`QString::number`), because JS doubles can't hold 64-bit ints.
`fromJson` (`core.h:314`): clamps `arrayLen` to `[1,kMaxArrayLen]`, `strLen` to
`[1,1000000]`, `ptrDepth` to `[0,2]`, **always sets `collapsed=true`**, reads
`isStatic` falling back to legacy `isHelper`, enum `value` parsed as base-10 longlong.
`NodeTree::toJson` (`core.h:793`): `baseAddress` as **hex string**, optional
`baseAddressFormula`/`initialClass`, `pointerSize` only if ≠8, `nextId` as string,
`nodes` array, optional `bookmarks`. `fromJson` reads `baseAddress` from base-16
string (default "400000"). **Rust**: `serde_json`; keep IDs as decimal strings and
baseAddress as hex string for bit-exact files.

---

## 1. `importFromSource` — C/C++ source → NodeTree

`NodeTree importFromSource(const QString& sourceCode, QString* errorMsg=nullptr, int pointerSize=8)`
(`import_source.h:12`, impl `import_source.cpp:1509`).

A hand-rolled tokenizer + recursive-descent parser for a *subset* of C/C++ struct
declarations. Robust to garbage: anything it doesn't understand is skipped rather
than erroring. Returns an **empty tree** + sets `*errorMsg` on three failure paths
(empty input, no structs/enums found, no nodes generated).

### 1.1 Type alias table — `buildTypeTable(int ptrSize=8)` (`import_source.cpp:17`)
Builds `QHash<QString, TypeInfo{NodeKind kind; int size}>`. Three categories:
- **stdint / standard C / Windows fixed** (always same regardless of ptrSize):
  `uint8_t`→UInt8(1)…`int64_t`→Int64(8); `char`→Int8, `short`→Int16, `int`→Int32,
  **`long`→Int32(4)**, `float`→Float, `double`→Double, `bool`/`_Bool`→Bool,
  **`void`→Hex8(1)**, `wchar_t`→UInt16(2). Multi-word pre-merged keys (the parser
  joins modifier words with single spaces): `"unsigned char"`→UInt8,
  `"long long"`→Int64(8), `"unsigned long"`→UInt32(4), `"unsigned long long"`→UInt64,
  `"long int"`→Int32, `"short int"`→Int16, `"unsigned"`→UInt32, etc. (full list
  `import_source.cpp:47–64`). Windows: `BYTE/UCHAR/BOOLEAN`→UInt8, `WORD/USHORT/WCHAR/TCHAR`→UInt16,
  `DWORD/ULONG/UINT`→UInt32, `LONG/LONG32/INT`→Int32, **`BOOL`→Int32(4)**,
  `FLOAT`→Float, `QWORD/ULONGLONG/DWORD64/ULONG64/UINT64`→UInt64,
  `LONGLONG/LONG64/INT64`→Int64, `CHAR`→Int8.
- **Pointer-size-dependent** (`import_source.cpp:94–121`): `ptrKind` =
  Pointer64 if ptrSize≥8 else Pointer32; `uintpKind`/`intpKind` = UInt64/Int64 or
  UInt32/Int32. `PVOID/LPVOID/HANDLE/HMODULE/HWND/HINSTANCE`→ptrKind(ptrSize);
  `SIZE_T/ULONG_PTR/UINT_PTR/DWORD_PTR/uintptr_t/size_t`→uintpKind;
  `LONG_PTR/INT_PTR/SSIZE_T/intptr_t/ptrdiff_t/ssize_t`→intpKind;
  pointer aliases `PCHAR/LPSTR/LPCSTR/PCSTR/PWSTR/LPWSTR/LPCWSTR/PCWSTR`→ptrKind.
  Note: `SIZE_T`/`size_t` resolve to **UInt64/UInt32 (not a Pointer kind)** — see
  test `platformPointerTypes` expecting `Pointer64` for PVOID/HANDLE but `UInt64`
  for SIZE_T/ULONG_PTR/uintptr_t/size_t.

After building, user-declared typedefs are folded in: for each parsed typedef
`alias→real`, if the table contains `real`, copy `table[real]` to `table[alias]`
(`import_source.cpp:1532`).

### 1.2 Tokenizer (`import_source.cpp:146`)
`enum TokKind { Ident, Number, Star, Semi, LBrace, RBrace, LBracket, RBracket,
LParen, RParen, Comma, Colon, Equals, Hash, Eof, Other }`. `Token{kind, QString text, int line}`.
Plus `LineOffset{int line; int offset}` captured from comments.

`tokenize()` (`import_source.cpp:155`) walks the source:
- Skip whitespace, tracking `line` on `\n`.
- `//` → `parseLineComment` (captures offset comments, see below).
- `/* */` → `parseBlockComment` (advances `line`; unterminated → eats to EOF).
- `#` at start → `skipToEndOfLine` (preprocessor lines dropped entirely; `#` never
  becomes a token in practice).
- Letter/`_` → `parseIdent` (greedy `[A-Za-z0-9_]`).
- Digit → `parseNumber`: hex (`0x…` → hex digits) or decimal; then **eats integer
  suffixes** `u/U/l/L`. The full `0x…`+suffix text is the token text.
- Single chars → mapped to TokKind via `c.toLatin1()` switch; unknown char → `Other`
  with the char as text (this is how unary `-` in enum values surfaces — `Other`
  with text `"-"`, see `import_source.cpp:1015`).
- Always pushes a trailing `Eof`.

**`parseLineComment`** (`import_source.cpp:222`): the comment text after `//` is
trimmed; then offset extraction via two regexes:
1. `^(?:->\s*\S+\s+)?0x([0-9A-Fa-f]+)$` (whole-comment form like `"0x10"` or
   `"-> Material* 0x10"`).
2. fallback `\b0x([0-9A-Fa-f]+)\s*$` (trailing hex anywhere, e.g. `"// foo 0x1A"`).
On match the captured hex is parsed base-16 to `int` and stored as
`LineOffset{commentLine, val}`. **Edge case**: offset comments are associated with
the *comment's own line*, and the parser later matches a field's *start* line
(`tokens[startPos].line`) against these — so the `// 0xNN` must be on the **same
line** as the field declaration (the common ReClass-generated layout). See tests
`commentOffsets`, `mixedOffsetsAutoDetect`.

**Rust mapping**: hand-roll the same tokenizer (the task explicitly suggests
"tree-sitter-cpp or handroll" — but to match exact behavior incl. comment-offset
heuristics and skip rules, *hand-roll* mirroring this file). Use the `regex` crate
or manual scan for the two offset patterns. `QChar::isSpace/isDigit/isLetter` map to
`char::is_whitespace/is_ascii_digit/is_alphabetic` (Qt uses Unicode categories; for
ASCII source this is equivalent — be careful only with non-ASCII identifiers, rare).

### 1.3 Parser (`import_source.cpp:328`)
Recursive descent over the token vector. State it accumulates:
- `QVector<ParsedStruct> structs` — the output (struct/class/union/enum defs, in
  source order).
- `QSet<QString> forwardDecls`.
- `QHash<QString,QString> typedefs` (alias→real), `QSet<QString> pointerTypedefs`
  (aliases that are pointer-to-struct or fnptr), `QHash<QString,QVector<int>>
  arrayTypedefs` (alias→dims).
- `QHash<QString,int> sizeAsserts` (struct name → declared `sizeof` from
  `static_assert`).
- `QHash<QString,int> structAlignments` (struct name → `ALIGN(N)`/`__declspec(N)` value).

`ParsedField` (`import_source.cpp:285`): `typeName` (merged base type),`name`,
`isPointer`,`pointerDepth`,`QVector<int> arraySizes`,`commentOffset`(-1=none),
`bitfieldWidth`(-1=none),`pointerTarget`,`isUnion`,`QVector<ParsedField> unionMembers`.
`ParsedStruct` (`import_source.cpp:298`): `name`,`keyword`,`QVector<ParsedField> fields`,
`declaredSize`(unused -1),`QVector<QPair<QString,int64_t>> enumValues`.

Parser helpers: `peek(ahead)`, `advance()`, `check(kind)`, `checkIdent(s)`,
`match(kind)`, `matchIdent(s)`, `skipToSemiOrBrace()` (brace-depth-aware skip to a
top-level `;`), `skipAlignMacro()` (consumes `ALIGN(...)` or `__declspec(...)`,
returns the first numeric arg or 0), `peekPastAlign(offset, expectedKind)`
(lookahead past an ALIGN macro to test the following token kind).
`isTypeModifier` = {unsigned,signed,long,short}; `isQualifier` =
{const,volatile,mutable,struct,class,enum}.

**`parse()`** (`import_source.cpp:427`): top-level dispatch by leading keyword:
`struct`/`class`→`parseStructOrForward`; `union`→`parseTopLevelUnion`;
`static_assert`→`parseStaticAssert`; `typedef`→`parseTypedef`; `enum`→`parseEnumDef`;
stray `#`→skip to `;`/EOF; anything else→`advance()` (skip one token).

**`parseStructOrForward`** (`import_source.cpp:449`):
1. consume keyword; `skipAlignMacro()` → `alignVal`.
2. If `{` follows (anonymous top-level) → skip the body and trailing `;`, return.
3. Require an Ident name (else skip to `;`). Record `structAlignments[name]=alignVal`
   if >0.
4. If `:` → **skip inheritance clause** up to `{`/`;`/EOF (base classes ignored — see
   test `inheritanceSkipped`).
5. If `;` → forward declaration: insert into `forwardDecls`, return.
6. Require `{`. Build `ParsedStruct{name, keyword}`, call `parseStructBody`, require
   `}`, optional `;`, append to `structs`.

**`parseStructBody`** (`import_source.cpp:500`): loop until `}`/EOF:
- nested `struct`/`class` with `Name {` (or `ALIGN(N) Name {`) → recursively
  `parseStructOrForward` (becomes its **own** root struct; the member-by-name
  embedding is *not* created automatically — only a *named-type field* later
  creates an embedded reference). Continue.
- anonymous nested `struct {`/`ALIGN(N) {` → **skip body entirely** plus optional
  field name + `;` (anonymous nested structs are dropped). Continue.
- `union` → `parseUnion(ps)` (creates a union ParsedField). 
- `enum` → `parseEnumDef()`. `static_assert` → `parseStaticAssert()`.
- else `parseField(field)`; on success append, else `advance()`.

**`parseTopLevelUnion`** (`import_source.cpp:563`): like a struct but `keyword="union"`;
handles forward decl, anonymous (skip), and named union with body → appended to
`structs`. (At build time, a top-level union's direct children are all forced to
offset 0 — see §1.5.)

**`parseUnion`** (member union, `import_source.cpp:603`): consumes `union`,
`skipAlignMacro`, optional tag name, `{`. Builds a `ParsedField unionField{isUnion=true}`.
Inside, recurses on nested unions (steals their fields), skips anonymous/named
nested structs (named nested struct definitions are parsed as their own roots),
and otherwise `parseField` each member into `unionField.unionMembers`. After `}`,
captures optional field name (e.g. `} u3;` → test `namedUnion`), `;`. The union's
`commentOffset` is taken from the first member having one. Appended to `ps.fields`.

**`parseField`** (`import_source.cpp:684`) — the core field grammar:
1. Save `startPos`. Skip leading qualifiers (`isQualifier`).
2. `parseTypeName()` → base type (empty ⇒ rewind & fail).
3. **Typedef resolution loop**: follow `typedefs` chain (cycle-guarded by a `QSet`);
   if any link is in `pointerTypedefs`, set `typedefPointer=true`; capture first
   `arrayTypedefs` dims found. `typeName` becomes the fully-resolved real type.
4. Pointer stars: `isPointer = typedefPointer`, `ptrDepth = typedefPointer?1:0`;
   consume `*`s (each ++depth); skip `const`/`volatile`; consume more `*`s.
5. Field name: require Ident (else rewind & fail). 
6. Array dims: each `[N]` (N decimal or `0x…`; empty `[]`→0) appended to `arraySizes`.
7. Apply typedef array dims: if field had none, use typedef's; else prepend typedef
   dims (typedef-of-array combined with field array).
8. Bitfield: `: width` → `bitfieldWidth = width`.
9. Require `;` (else rewind & fail).
10. Match a `LineOffset` whose `.line == tokens[startPos].line` → `commentOffset`.
11. Fill `field.typeName/isPointer/pointerDepth`; if pointer, `pointerTarget=typeName`.

**`parseTypeName`** (`import_source.cpp:794`):
- `struct`/`class`/`enum` Ident → returns the Ident (consumes the keyword; test
  `structPrefixOnType`).
- Modifier word (`unsigned`/`signed`/`long`/`short`) → collect following
  modifier/`int`/`char`/`long` words, join with single space (yields the merged
  multi-word keys in the type table). 
- otherwise consume and return one Ident.

**`parseStaticAssert`** (`import_source.cpp:827`): scans the paren group for
`sizeof(Ident)` and the *first* numeric literal (decimal or `0x…`); on success and
size>0 records `sizeAsserts[structName]=sizeVal`. (test `staticAssertTailPadding`,
`basicRoundTrip`).

**`parseTypedef`** (`import_source.cpp:873`):
- `typedef struct {…} Name;` / `typedef struct Tag {…} Name;` → parse as a full
  struct (its body becomes a root), return.
- `typedef struct Existing * Alias;` → `typedefs[Alias]=Existing`; if `*`,
  `pointerTypedefs.insert(Alias)`. Skips self-referencing typedefs.
- `typedef BaseType [*] Alias [N]…;` → resolve base via `parseTypeName`, capture
  pointer flag, capture array dims; register `typedefs[Alias]=BaseType` (+pointer/array
  sets). Skips self-referencing.
- **Function-pointer typedef** `typedef Ret (*Name)(args);` → register `Name` in
  `pointerTypedefs` and `typedefs[Name]="void"` (treated as void*).

**`parseEnumDef`** (`import_source.cpp:966`): handles `enum`, `enum class`,
`enum struct`. Name only taken if followed by `{` or `:` (so `enum Foo bar;` field
usage and `enum Foo;` forward decls are *not* treated as definitions and the
function returns without consuming the body in the field-usage case — note for
`enum Name;` it consumes the `;` and returns). Skips `: underlying-type`. Parses
members `Name [= Value]` separated by optional commas. Value parsing: optional
unary `-` (an `Other` token with text "-"), then a Number (decimal or `0x…`,
parsed as `int64`); complex expressions are skipped to the next `,`/`}`. Auto-value
starts at 0 and is `prev+1` when omitted (tests `enumAutoValues`,`enumHexValues`).
`ps.keyword="enum"`, `enumValues` populated; appended only if `name` non-empty
(anonymous enums dropped — test `enumHexValues` comment).

### 1.4 Padding & bitfield emit helpers
**`isPaddingName`** (`import_source.cpp:1053`): case-insensitive prefix match on
`_pad`,`pad_`,`__pad`,`padding`,`_padding`,`__padding`,`_reserved`,`reserved`.
**`emitHexPadding(tree,parentId,offset,size)`** (`import_source.cpp:1065`): emits
best-fit hex run: if `size%8==0 && size≥8`→Hex64×(size/8); else `%4`→Hex32; `%2`→Hex16;
else Hex8. Each node named "" (empty). (test `paddingFieldExpansion`: 0x10 → 2×Hex64
at 0 and 8.)
**`emitBitfieldGroup(...startIdx,endIdx)`** (`import_source.cpp:1090`): computes
`totalBits = Σ bitfieldWidth`; `bytes=(totalBits+7)/8`; container hex kind by bytes
(≤1 Hex8, ≤2 Hex16, ≤4 Hex32, else Hex64). Emits one `Struct` node with
`classKeyword="bitfield"`, `elementKind=containerKind`, `collapsed=false`, and a
`BitfieldMember` per field with running `bitOffset` (cumulative widths).

### 1.5 Build phase — `BuildContext` + `buildFields` (`import_source.cpp:1127–1495`)
`BuildContext` holds `tree`, `typeTable`, `classIds` (name→nodeId),
`pendingRefs`, `useCommentOffsets`, `enumNames` (set of enum type names),
`ptrSize`, `sizeAsserts`, `structAlignments`.

`PendingRef{nodeId, className}` — deferred pointer/struct-ref resolution.

Alignment helpers: `fieldNaturalAlignment` (`import_source.cpp:1153`): pointer→ptrSize;
union→max member alignment; bitfield→alignment of its storage type (else 4);
known type→`alignmentFor(kind)`; unknown(struct ref)→ptrSize. `alignUp(off,align)`
= `(off+align-1)&~(align-1)`. `unionNaturalAlignment` = max over members.
`structTypeSize(name)` (`import_source.cpp:1173`): if the struct is already built
(`classIds`), use `structSpan`, then round **up to its `ALIGN(N)`** if declared;
else fall back to `sizeAsserts[name]`; else 0.
`clampedArrayElements(dims, max=1e6)` (`import_source.cpp:1192`): product of dims
(0/neg dims treated as 1), capped at `max`.

**`buildFields(ctx, parentId, baseOffset, fields)`** (`import_source.cpp:1201`):
walks fields, tracking `computedOffset` (running offset for non-comment mode).
Offset choice per field: if `useCommentOffsets && commentOffset>=0` use
`commentOffset - baseOffset` (so absolute comment offsets become parent-relative);
else `computedOffset = alignUp(computedOffset, fieldAlign); fieldOffset=computedOffset`.

Field categories (in order):
1. **Bitfield group**: consume consecutive `bitfieldWidth>=0` fields, emit one
   bitfield container via `emitBitfieldGroup`; advance `computedOffset` past the
   container's byte size (1/2/4/8). (tests `bitfieldSkipped`, `bitfieldWithOffsetsEmitsHex`.)
2. **Union field**: emit a `Struct{classKeyword="union", name=field.name}` node;
   build **each union member independently** by calling `buildFields` on a 1-element
   list with `baseOffset = baseOffset+unionOffset` so each member starts at relative
   offset 0; advance `computedOffset` by the union's `structSpan`. (tests
   `unionContainer`,`unionWithCommentOffsets`,`namedUnion`.)
3. **Pointer field**: `ptrKind` = Pointer64/32 by ptrSize. If `arraySizes` non-empty →
   `Array` of `elementKind=ptrKind`, `arrayLen=product`; advance by `count*ptrSize`.
   Else single pointer node (collapsed); if `pointerTarget` non-empty and ≠"void",
   push `PendingRef{id, pointerTarget}`; advance by `ptrSize`. (void* → refId stays 0,
   test `voidPointer`; self-ref test `selfReferencingPointer`; `doublePointer` `void**`
   still a single Pointer64.)
4. **Enum-typed field** (unknown type that is in `enumNames`): emit as `UInt32`
   (size 4) with `PendingRef` to the enum (resolved → `refId`); array form supported.
   (test `enumInStruct`.)
5. **Resolve base type**: `knownType = typeTable.contains(typeName)`. If known,
   `baseKind/baseSize` from table; else `isStructType=true` (unknown name = struct ref).
6. **Padding field** (`isPaddingName(name) && arraySizes non-empty`): expand to
   `emitHexPadding` over `baseSize*Πdims` bytes. (test `commentOffsets` `_pad000C[0x4]`.)
7. **Array of primitive** (`arraySizes && !isStructType`):
   - `char[N]`/`CHAR[N]` (single dim, Int8) → **UTF8** with `strLen=N`.
   - `wchar_t[N]`/`WCHAR[N]`/`TCHAR[N]` (single dim, UInt16) → **UTF16** `strLen=N`.
   - `float[2]`→Vec2(8), `float[3]`→Vec3(12), `float[4]`→Vec4(16).
   - `float[4][4]`→Mat4x4(64).
   - otherwise generic `Array{elementKind=baseKind, arrayLen=product}`; advance by
     `product*baseSize`. (tests `charArrayToUtf8`,`wcharArrayToUtf16`,`floatArrayToVecN`,
     `floatArray4x4ToMat4x4`,`genericFloatArray`,`primitiveArray`,`hexArraySizes`.)
8. **Struct-type field** (`isStructType`): `elemSize=structTypeSize(name)`. Array form
   → `Array{elementKind=Struct, structTypeName=name, arrayLen=product}` + `PendingRef`;
   advance by `product*elemSize` (only if elemSize>0). Single form → `Struct{name,
   structTypeName=name, collapsed}` + `PendingRef`; advance by `elemSize` (if >0).
   (tests `embeddedStruct`,`structArray`,`structPrefixOnType`; PEB layout depends on
   correct struct sizing — `test_roundtrip_winsdk::pebOffsets`.)
9. **Simple primitive**: `Node{kind=baseKind}`; advance by `baseSize`.

`hasAnyCommentOffset(fields)` (`import_source.cpp:1499`): recursive over union members —
if *any* field anywhere has `commentOffset>=0`, comment mode is enabled (whole-file
decision). (test `mixedOffsetsAutoDetect`: fields without comments get *computed*
offsets even in comment mode.)

### 1.6 Top-level orchestration (`import_source.cpp:1509`)
1. Trim-empty input check → error "Empty source code".
2. Tokenize, parse. If `structs.isEmpty()` → error "No struct or enum definitions found".
3. Build type table (ptrSize), fold typedefs.
4. `tree.baseAddress=0x00400000; tree.pointerSize=pointerSize`.
5. `useCommentOffsets` = any struct has any comment offset.
6. `enumNames` = names of enum ParsedStructs.
7. For each ParsedStruct (source order):
   - Make a root `Struct{name, structTypeName=name, classKeyword=ps.keyword}`.
   - **Enum**: set `enumMembers=ps.enumValues`, add node, register in `classIds`, continue.
   - Else add root, register `classIds[name]=id`, `buildFields(ctx,id,0,fields)`.
   - **Union root**: force every direct child `.offset=0` (`import_source.cpp:1588`).
   - **static_assert tail padding**: if `sizeAsserts[name] > structSpan` →
     `emitHexPadding(tree, id, currentSpan, declared-currentSpan)`. (test
     `staticAssertTailPadding`: 4-byte struct + `==0x10` → span becomes 0x10.)
8. If tree empty → error "No nodes generated from source".
9. **Resolve pendingRefs**: for each, if `classIds` has the class name, set the
   node's `refId` to that id (unresolved refs leave `refId=0`).

**Edge cases the tests pin down**:
- `noStructs` (`int x = 42;`) → empty tree + error.
- `singleEmptyStruct` → 1 root, name preserved, kind Struct.
- `classKeyword` → `class X{}` keeps `classKeyword="class"`.
- `computedOffsets`: u8,u16,u32,u64 → offsets 0,2,4,8 (natural alignment).
- `commentOffsets`: comment offsets honored verbatim; `_pad000C[0x4]` expands to hex.
- `forwardDeclaration`: a `struct Bar;` then pointer to Bar resolves once Bar is defined.

**Portability: PURE/mostly-portable.** No OS calls, no threading. Pure string/data
transform. Direct hand-rolled port. Watch: integer overflow guards
(`clampedArrayElements`, `byteSize` `qMin` caps), case-insensitive padding-name
checks, `toInt(&ok,16)` semantics (Rust `i64::from_str_radix` / `u32`), QString
trimming.

---

## 2. `import_reclass_xml` — ReClass .NET / ReClassEx XML → NodeTree

`NodeTree importReclassXml(const QString& filePath, QString* errorMsg=nullptr, int pointerSize=8)`
(`import_reclass_xml.h:10`, impl `import_reclass_xml.cpp:142`).

Reads a `.reclass`/`.MemeCls`/etc XML file with `QXmlStreamReader` (streaming SAX-ish).
Supports two format generations selected by `XmlVersion {V2013, V2016}`.

### 2.1 Type maps (`import_reclass_xml.cpp:17,55`)
Two `{int xmlType; NodeKind kind}` tables (the XML `Type` attribute is an integer
whose meaning differs per version):
- **`kTypeMap2016`** (ReClassEx/MemeClsEx, ~33 entries): 1→Struct(ClassInstance),
  4→Hex32, 5→Hex64, 6→Hex16, 7→Hex8, 8→Pointer64(ClassPointer), 9→Int64, 10→Int32,
  11→Int16, 12→Int8, 13→Float, 14→Double, 15→UInt32, 16→UInt16, 17→UInt8,
  18→UTF8, 19→UTF16, 20→Pointer64(FunctionPtr), 21→Hex8(Custom), 22→Vec2, 23→Vec3,
  24→Vec4, 25→Mat4x4, 26→Pointer64(VTable), 27→Array(ClassInstanceArray),
  29→Pointer64(UTF8TextPtr), 30→Pointer64(UTF16TextPtr), 31→UInt8(BitField fallback),
  32→UInt64, 33→Pointer64(Function).
- **`kTypeMap2013`** (ReClass 2011/2013): 1→Struct, 4→Hex32, 5→Hex16, 6→Hex8,
  7→Pointer64, 8→Int32, 9→Int16, 10→Int8, 11→Float, 12→UInt32, 13→UInt16, 14→UInt8,
  15→UTF8, 16→Pointer64(FunctionPtr), 17→Hex8(Custom), 18→Vec2, 19→Vec3, 20→Vec4,
  21→Mat4x4, 22→Pointer64(VTable), 23→Array, 27→Int64, 28→Double, 29→UTF16,
  30→Array(ClassPointerArray).

`lookupKind(xmlType, ver, ptrSize=8)` (`import_reclass_xml.cpp:83`): linear-search
the version table (default `Hex8` on miss); **if ptrSize<8 and result is Pointer64,
remap to Pointer32**.

Predicates: `isPointerType` (2016: 8,20,26,29,30,33; 2013: 7,16,22), 
`isClassInstanceType` (==1 both), `isClassInstanceArrayType` (2016:27; 2013:23 or 30),
`isTextType` (2016:18,19; 2013:15,29), `isUtf16TextType` (2016:19; 2013:29),
`isCustomType` (2016:21; 2013:17).

### 2.2 Parse loop (`import_reclass_xml.cpp:142`)
1. Open file read+text. Failure → error "Cannot open file: …", empty tree.
   (Emits several `qDebug()` traces — drop in Rust.)
2. `version = V2016` default; `tree.baseAddress=0x00400000; pointerSize` set.
3. `classIds` (name→structId), `pendingRefs`.
4. **Version detection from the first XML comment** (`isComment`, before
   `versionDetected`): if it contains (CI) "ReClassEx"/"MemeClsEx"/"2016"/"2015"→V2016;
   "2013"/"2011"→V2013; else keep default. Set `versionDetected=true`.
5. For each start-element named **`Class`**: read attrs `Name`, `strOffset`. Create
   root `Struct{name, structTypeName=name, parentId=0, offset=0, collapsed=true}`,
   register in `classIds`. Then `childOffset=0` and loop reading inner elements
   until the matching `Class` end-element:
   - Only `Node` start-elements are processed. Read attrs `Type`(int), `Name`,
     `Size`(int), `Pointer`(string), `Instance`(string).
   - **Custom** (`isCustomType && Size>0`): expand into best-fit hex run (same algorithm
     as `emitHexPadding`: ≥8&%8→Hex64; ≥4&%4→Hex32; ≥2&%2→Hex16; else Hex8). The
     *single*-node case keeps `nodeName`; multi-node case names are empty. Each child
     at `childOffset`, advancing. (test `exportHexCollapse` round-trip: a 4-byte Custom
     re-imports as one Hex32.)
   - `kind = lookupKind(Type,ver,ptrSize)`.
   - **ClassInstanceArray** (`isClassInstanceArrayType`): read `Total` (fallback
     `Count`, else 1). Then consume inner elements until the `Node` end-element,
     looking for an `<Array>` child whose `Name`/`Total`(or `Count`) override the
     element class/total. Create `Array{elementKind=Struct, arrayLen=total,
     structTypeName=arrayClassName}` + `PendingRef` if class named. Advance
     `childOffset` by `Size` (if >0). 
   - Else build `Node{kind, name, parentId=structId, offset=childOffset}`.
   - **Text** (`isTextType`): UTF16 → `strLen=max(1,Size/2)`; UTF8 → `strLen=max(1,Size)`.
     (test `importSmallXml`: Type 18 Size 32 → UTF8 strLen 32; offset 12.)
   - **Pointer** (`isPointerType && Pointer attr non-empty`): collapsed; add node;
     `PendingRef{id, ptrClass}`; advance by `Size` (>0) else `sizeForKind(kind)`.
     (test: Type 8 → Pointer64; refId resolves to same TestClass — self-ref.)
   - **ClassInstance** (`isClassInstanceType`): `resolvedClass = Instance` else
     `Pointer`; set `structTypeName=resolvedClass`, collapsed; if non-empty add +
     `PendingRef`, else just add. Advance by `Size` (>0) else 0.
   - **default**: add node; advance by `Size` (>0) else `sizeForKind(kind)`.
6. After EOF: if `xml.hasError()` and the error is **not**
   `PrematureEndOfDocumentError`, return empty + error "XML parse error at line N: …".
7. If tree empty → error "No classes found in file".
8. **Resolve pendingRefs**: same as import_source — `classIds` lookup → `refId`.

**Important offset behavior**: offsets are **purely sequential** — `childOffset` only
ever advances by each node's `Size` (or `sizeForKind`), never re-aligned. This matches
ReClass's flat layout (the XML stores explicit sizes). So `import_reclass_xml` does NOT
align like `import_source`. test `importSmallXml` asserts exact running offsets
0,8,12,44 from sizes 8,4,32,12.

**Portability: PURE.** File I/O + XML. Map `QFile` → `std::fs`, `QXmlStreamReader` →
**`quick-xml`** (Reader with `Event::Start`/`End`/`Comment`/`Empty`). Note: ReClass
`<Node …/>` are *empty* elements — quick-xml yields `Event::Empty` for self-closing
tags, so the handler must treat `Empty` like `Start`+immediate `End`. The
`PrematureEndOfDocumentError` tolerance: quick-xml returns an EOF/incomplete error
that should be treated as benign (return what was parsed). Attribute `.toInt()` on a
missing/empty attribute yields 0 in Qt — replicate (parse-or-0).

---

## 3. `export_reclass_xml` — NodeTree → ReClassEx XML

`bool exportReclassXml(const NodeTree& tree, const QString& filePath, QString* errorMsg=nullptr)`
(`export_reclass_xml.h:8`, impl `export_reclass_xml.cpp:63`). Always writes **V2016**
("ReClassEx") format.

`xmlTypeForKind(NodeKind)` (`export_reclass_xml.cpp:11`) — reverse of the 2016 map:
Struct→1, Hex32→4, Hex64→5, Hex16→6, Hex8→7, Pointer64/Pointer32→8, Int64→9,
Int32→10, Int16→11, Int8→12, Float→13, Double→14, UInt32→15, UInt16→16, UInt8→17,
UInt64→32, UTF8→18, UTF16→19, **Bool→17 (UInt8, no native bool)**, Vec2→22, Vec3→23,
Vec4→24, Mat4x4→25, Array→27; default fallback 7 (Hex8). (Hex128, Int128, UInt128,
Float16, FuncPtr32/64 fall through to the default 7 — lossy.)

`nodeSizeForExport(node)` (`export_reclass_xml.cpp:42`): UTF8→strLen; UTF16→strLen*2;
Array→`arrayLen * max(elemSize,0)`; else `sizeForKind(kind)`.

`resolveStructName(tree, refId)` (`export_reclass_xml.cpp:55`): the referenced node's
`structTypeName` (or `name` fallback), "" if not found.

**`exportReclassXml`** (`export_reclass_xml.cpp:63`):
1. Empty tree → false + "No nodes to export".
2. Open file write+text; failure → false + "Cannot open file for writing: …".
3. Build `childMap[parentId]→[indices]`.
4. `QXmlStreamWriter` with auto-formatting indent 4; `writeStartDocument()`;
   `<ReClass>` element; `writeComment("ReClassEx")` (this is what the importer's
   version detection keys on — round-trip stays V2016).
5. Root structs = `childMap[0]`, **sorted by offset**. For each `Struct` root:
   - `<Class Name=… Type="28" Comment="" Offset="0" strOffset="0" Code="">`. Name =
     `name` (or `structTypeName` if name empty). Type 28 is the ReClassEx class marker.
   - Children = `childMap[root.id]`, sorted by offset. Walk with index `i`:
     - **Bitfield container** (`Struct` + `resolvedClassKeyword()=="bitfield"`): export
       as a single hex `Node` — size = `byteSize()` (≥4 fallback), hex kind by size,
       `Comment="bitfield"`, `bHidden="false"`. (ReClassEx has no bitfield concept;
       lossy — re-import yields a hex node, not a bitfield.)
     - **Hex run collapse**: consecutive `isHexNode` children with no gap/overlap
       (`next.offset == runEnd`) collapse into one **Custom** node (`Type="21"`),
       `Size=totalSpan`. Name kept only if the run is a single node with a non-empty
       name; else empty. (test `exportHexCollapse`: 4×Hex8 → one Type=21 Size=4.)
     - **Generic node**: `<Node Name Type=xmlTypeForKind Size=nodeSizeForExport
       bHidden="false" Comment="">`. Then:
       - Pointer with `refId!=0` → `Pointer="<resolvedStructName>"` attribute.
       - Struct → `Instance="<structTypeName or name>"`.
       - Array → `Total=arrayLen`; element name resolved from
         `structTypeName` (if elementKind==Struct), else `resolveStructName(refId)`,
         else `kindToString(elementKind)`; writes a nested `<Array Name= Total=/>`.
   - Close `</Class>`; `classCount++`.
6. Close `</ReClass>`, end document, close file.
7. If `classCount==0` → false + "No struct classes found to export".

**Round-trip guarantees the tests rely on** (`test_export_xml.cpp`):
- Single struct of Int32/Float/UInt64 survives kind-for-kind.
- Pointer with refId → `Pointer="Target"` attribute, re-import resolves refId.
- Embedded struct → `Instance="Inner"`.
- Array → `Total` + nested `<Array>`.
- UTF8 strLen 32 / UTF16 strLen 16 preserved.
- Vec2/3/4/Mat4x4 preserved.
- Hex collapse → Type 21; re-import expands to best-fit hex.
- 5 multi-class round-trips preserve all class names.
- `roundTripImportExport`: full kind sweep + self-pointer; kinds and names match;
  self-pointer resolves to root id. **Note**: round-trip does *not* preserve offsets
  exactly for all kinds (it preserves *kind/name* and the sequential layout from
  sizes), so the test compares kind+name, not offset, for most fields.

**Portability: PURE.** `QXmlStreamWriter` → **`quick-xml`** Writer (or build strings).
Auto-formatting indent-4 matters only if you want byte-identical files; the tests only
check substrings (`xml.contains("Pointer=\"Target\"")`) so attribute *order* and
formatting must match Qt's: Qt writes attributes in call order, elements indented 4
spaces, self-closing empty elements. Replicate attribute call order to keep the
`contains` assertions valid (they look for exact `Type="21"`, `Size="4"`,
`Pointer="Target"`, `Instance="Inner"`, `Total="10"`, `<Array`, `ReClassEx`).

---

## 4. `pe_debug_info` — extract PDB GUID/age/name from a PE in memory

`PdbDebugInfo extractPdbDebugInfo(const Provider& prov, uint64_t moduleBase)`
(`pe_debug_info.h:18`, impl `pe_debug_info.cpp:85`).

`PdbDebugInfo` (`pe_debug_info.h:9`): `{QString pdbName; QString guidString (32 hex,
no dashes, uppercase); uint32_t age; bool valid}`.

Minimal `#pragma pack(1)` PE structs (no Windows SDK dependency):
`DosHeader{e_magic; pad[58]; int32 e_lfanew}`, `CoffHeader` (7 fields),
`DataDirectory{VA;Size}`, `OptionalHeader32/64` (only Magic + padding +
NumberOfRvaAndSizes), `DebugDirectory` (8 fields incl. `Type`, `SizeOfData`,
`AddressOfRawData`(RVA), `PointerToRawData`(file off, unused)),
`CvInfoPdb70{uint32 Signature; uint8 Guid[16]; uint32 Age; /*char name[] follows*/}`.
Constants: `kMZ=0x5A4D`, `kPE=0x00004550`, `kPE32=0x10b`, `kPE32P=0x20b`,
`kRSDS=0x53445352`, `kDebugType_CodeView=2`.

Algorithm (`pe_debug_info.cpp:85`): all reads via `prov.read(addr, buf, len)` →
returns `result` (valid=false) on any failed read:
1. Read DOS header at `moduleBase`; require `e_magic==MZ`.
2. `peOffset = moduleBase + e_lfanew`; read 4 bytes; require `==PE`.
3. Read COFF header at `peOffset+4`.
4. `optOffset = coffOffset + sizeof(CoffHeader)`; read 2-byte Magic.
5. PE32: `NumberOfRvaAndSizes` at `optOffset+92`, data dirs at `optOffset+96`.
   PE32+: at `optOffset+108`, dirs at `optOffset+112`. Other magic → bail.
6. Require `numRvaAndSizes>6` (need the Debug directory, index 6).
7. Read `DataDirectory` at `dataDirsOffset + 6*sizeof(DataDirectory)`; require
   VA≠0 and Size≠0.
8. `numEntries = debugDir.Size / sizeof(DebugDirectory)`; iterate each entry at
   `moduleBase + debugDir.VirtualAddress + i*sizeof(DebugDirectory)`:
   - Skip unless `Type == kDebugType_CodeView`.
   - Skip if `AddressOfRawData==0` or `SizeOfData < sizeof(CvInfoPdb70)+1`.
   - Read `CvInfoPdb70` at `moduleBase + AddressOfRawData`; require `Signature==RSDS`.
   - PDB filename = bytes after the struct (len capped at min(SizeOfData-sizeof, 260),
     null-terminated, Latin1). Strip path (last `\` then last `/`).
   - `guidString = guidToString(Guid)`; `age = Age`; `valid=true`; return.
9. No match → `result` (valid=false).

`guidToString(guid[16])` (`pe_debug_info.cpp:70`): Windows GUID mixed-endian: Data1
(4 bytes, native int via memcpy), Data2 (2 bytes), Data3 (2 bytes) formatted as
`%08x%04x%04x`, then Data4 (8 bytes) appended as sequential 2-hex bytes; whole string
`.toUpper()`. → 32 hex chars no dashes, e.g. matches MS symbol-server expectations.

**Concurrency/platform**: none — works on *any* Provider (file/buffer/snapshot), not
Windows-specific. The reads are byte-exact little-endian (it reads raw structs with
`#pragma pack`). On big-endian hosts this would be wrong, but targets are LE.

**Portability: mostly-portable.** Use **`object`** or **`goblin`** crate to parse the
PE in memory, OR hand-roll the same fixed-offset reads against the Provider trait
(simpler for parity since data comes from a Provider, not a file slice). The
`guidToString` mixed-endian formatting must be reproduced exactly:
`format!("{:08X}{:04X}{:04X}{:02X}{:02X}...", d1, d2, d3, guid[8], ...)` where d1/d2/d3
are read as native LE integers from the GUID bytes. `prov.read` → the Provider trait's
`read`. `QString::fromLatin1` → bytes interpreted as Latin1.

---

## 5. `import_pdb` — PDB type & symbol import (Windows-only)

`import_pdb.{h,cpp}`. The **entire implementation is `#ifdef _WIN32`**; the `#else`
branch (`import_pdb.cpp:1379`) provides stubs for all five public functions that set
`*errorMsg = "PDB import requires Windows"` and return empty. It uses the bundled
**RawPDB** library (`third_party/raw_pdb`, an MSF/PDB reader with no DIA dependency).

**Public API** (`import_pdb.h`):
- `struct PdbSymbol {QString name; uint32_t rva; uint32_t typeIndex=0;}`.
- `struct PdbSymbolResult {QString moduleName; QVector<PdbSymbol> symbols;}`.
- `PdbSymbolResult extractPdbSymbols(path, errorMsg)`.
- `struct PdbTypeInfo {uint32_t typeIndex; QString name; uint64_t size; int childCount;
  bool isUnion; bool isEnum=false;}`.
- `QVector<PdbTypeInfo> enumeratePdbTypes(path, errorMsg)` — fast scan, no recursion.
- `using ProgressCb = std::function<bool(int current,int total)>` (return false=cancel).
- `NodeTree importPdbSelected(path, QVector<uint32_t> typeIndices, errorMsg, ProgressCb)`.
- `NodeTree importPdb(path, structFilter={}, errorMsg)` — legacy: one struct by name
  (or all if filter empty).
- `NodeTree importTypeForSymbol(path, typeIndex, QString* typeName, errorMsg)`.

### 5.1 Infrastructure (Win32)
- **`MappedFile`** (`import_pdb.cpp:26`): `CreateFileW`/`CreateFileMappingW`/
  `MapViewOfFile`; size via `GetFileInformationByHandle`. RAII close. (Rust: `memmap2`
  crate `Mmap`, or read the file into a `Vec<u8>` — the `pdb` crate accepts any
  `Read+Seek`/`Source`.)
- **`TypeTable`** (`import_pdb.cpp:66`): builds an O(1) index→record array from the TPI
  stream via `ForEachTypeRecordHeaderAndOffset`. `firstIndex()`/`lastIndex()`/`count()`;
  `get(typeIndex)` returns the `Record*` or nullptr if `< firstIndex` or `>= lastIndex`.
  (Rust `pdb`: `TypeInformation` + `TypeFinder`, or iterate `type_table.iter()` and
  build a `HashMap<TypeIndex, Type>`.)
- **Leaf numeric helpers** (`import_pdb.cpp:110`): `leafSize(kind)`, `leafName(data,kind)`
  (= `data + leafSize`), `leafValue(data,kind)`. CodeView leaf encoding: if
  `kind < LF_NUMERIC`, the kind itself is the numeric value (small immediates);
  otherwise the value follows the 2-byte kind as LF_CHAR(u8)/LF_SHORT(i16)/LF_USHORT
  (u16)/LF_LONG(i32)/LF_ULONG(u32)/LF_QUADWORD(i64)/LF_UQUADWORD(u64). This variable-
  length integer encoding is used for member offsets, struct sizes, array byte-counts,
  enum values. (Rust `pdb` decodes these internally — you read `.size()`, member
  `.offset()`, enum `.value` as already-decoded numbers, so most of this disappears.)
- `unionLeafKind(data)` = first 2 bytes (LF_UNION has no `lfEasy` member, unlike LF_CLASS).

### 5.2 Primitive type mapping (`import_pdb.cpp:147`)
`mapPrimitiveType(typeIndex)`: for indices `< 0x1000`, mask `&0xFF` for the base type:
0x03 void→Hex8; 0x10/0x70/0x68 char/int8→Int8; 0x20/0x69 uchar/uint8→UInt8;
0x71/0x7a wchar/char16→UInt16; 0x7c char8→UInt8; 0x7b char32→UInt32;
0x11/0x72 short/int16→Int16; 0x21/0x73 ushort/uint16→UInt16;
0x12/0x74 long/int32→Int32; 0x22/0x75 ulong/uint32→UInt32;
0x13/0x76 int64→Int64; 0x23/0x77 uint64→UInt64; 0x40 float→Float; 0x41 double→Double;
0x30 bool→Bool; 0x31/0x32/0x33 bool16/32/64→UInt16/32/64; 0x08 HRESULT→UInt32;
0x60 bit→UInt8; 0x78/0x79 int128/uint128→Hex64 (best effort); default→Hex32.
`hexForSize(len)`: 1→Hex8,2→Hex16,4→Hex32,8→Hex64,else Hex32.

The *pointer mode* of a primitive index is `(typeIndex>>8)&0xF`: 0x04/0x05 → 32-bit
ptr; 0x06 → 64-bit ptr; nonzero other → 32-bit ptr; 0 → direct base type. Used in
`importMemberType` (`import_pdb.cpp:560`).

### 5.3 Import context `PdbCtx` (`import_pdb.cpp:232`)
Holds `NodeTree tree`, `const TypeTable* tt`, `typeCache (typeIndex→nodeId)` (prevents
re-import + handles recursion/self-reference), `structDefByName`/`unionDefByName`
(name→typeIndex for forward-ref resolution), `udtDefIndexBuilt` flag. Methods:
`importUDT`, `importEnum`, `importFieldList`, `importMemberType`,
`buildUdtDefinitionIndex`, `findUdtDefinitionIndex`, `unwrapModifier` (LF_MODIFIER →
underlying type index).

**`buildUdtDefinitionIndex`** (`import_pdb.cpp:258`): one-time scan of all TPI records;
for each non-forward-ref LF_UNION/LF_STRUCTURE/LF_CLASS with a real name, record the
*first* typeIndex per name in the appropriate map. **`findUdtDefinitionIndex(kind,name)`**
looks up the definition index for a forward reference's name.

**`importUDT(typeIndex)`** (`import_pdb.cpp:308`):
- Bail if `< firstIndex`; return cached id if present.
- For LF_STRUCTURE/LF_CLASS: skip if `fwdref` (return 0); extract field count,
  field-list index, name. For LF_UNION: same, `isUnion=true`.
- Create root `Struct{name, structTypeName=name, classKeyword=union?"union":"struct",
  parentId=0, collapsed}`; **cache the id before recursing** (so self/mutual refs
  resolve); then `importFieldList(fieldListIndex, nodeId)`. Return id.

**`importEnum(typeIndex)`** (`import_pdb.cpp:360`): require LF_ENUM, non-fwdref;
create root `Struct{classKeyword="enum"}`; walk the enum's LF_FIELDLIST collecting
`LF_ENUMERATE` members into `enumMembers` (name + `leafValue`); the manual walk
advances by `leafName`-end + strlen+1, 4-byte aligned (`(i+3)&~3`); stops at the
first non-ENUMERATE. Cache + return.

**`importFieldList(fieldListIndex, parentId)`** (`import_pdb.cpp:413`): the heart of
member iteration. Require LF_FIELDLIST. Walk raw bytes (`maximumSize = header.size -
sizeof(u16)`), 4-byte realigning after each record. Field kinds handled:
- **LF_MEMBER**: decode variable-length offset, member name, member type index.
  `unwrapModifier` the type; if it resolves to **LF_BITFIELD**, group bitfield members
  into a shared *bitfield container* keyed by `(offset, slotSize)`: first member
  creates a `Struct{classKeyword="bitfield", elementKind=hexForSize(slotSize),
  offset, collapsed=false}`; each member appends a `BitfieldMember{name, bitOffset=
  position, bitWidth=length}`. `slotSize` = `sizeForKind(mapPrimitiveType(underlying))`
  for primitive underlying, else 4. Otherwise call `importMemberType`.
- **LF_BCLASS** (base class): skipped (size computed from leaf). 
- **LF_VBCLASS/LF_IVBCLASS** (virtual base): skipped (two leaf sizes).
- **LF_INDEX** (field-list continuation): recurse `importFieldList` into the next record.
- **LF_VFUNCTAB**, **LF_NESTTYPE**, **LF_STMEMBER** (static member), **LF_METHOD**,
  **LF_ONEMETHOD** (handles the Intro/PureIntro vbaseoff variant), **LF_ENUMERATE**:
  all skipped (advance by their record size). Unknown kind → break.

**`importMemberType(typeIndex, offset, name, parentId)`** (`import_pdb.cpp:557`):
emits one `Node` for a member. Cases:
- **Primitive index** (`< firstIndex`): branch on pointer-mode bits — 0x04/0x05→
  Pointer32, 0x06→Pointer64, other nonzero→Pointer32 (collapsed), 0→base type via
  `mapPrimitiveType`.
- **LF_MODIFIER**: recurse on the underlying type.
- **LF_POINTER**: kind = Pointer32 if `attr.size<=4` else Pointer64 (collapsed). Unwrap
  modifier on pointee. If pointee is a UDT (struct/class/union): resolve forward refs
  via `findUdtDefinitionIndex`; **skip anonymous targets** (name null/empty/starts with
  `<`) to avoid creating root orphans; otherwise `refId = importUDT(defIndex)`. If
  pointee is LF_PROCEDURE/LF_MFUNCTION → kind becomes FuncPtr32/64. (test `verifyListEntry`:
  Flink/Blink are Pointer64 with refId == the _LIST_ENTRY root id, self-ref.)
- **LF_STRUCTURE/LF_CLASS/LF_UNION** (embedded): resolve fwdref to a definition;
  **anonymous types are inlined** — create a `Struct` container (no refId, parented to
  the current parent) and `importFieldList` directly into it (avoids root orphans).
  Named types → `refId = importUDT(defIndex)` and emit `Struct{structTypeName=typeName,
  classKeyword=union?"union":"struct", refId}`. (test `importKProcess`: `Header` →
  embedded `_DISPATCHER_HEADER` at offset 0; `ProfileListHead` → `_LIST_ENTRY` at 0x18.)
- **LF_ARRAY**: total byte size from leaf; element size computed from the (modifier-
  unwrapped) element type — primitive→`sizeForKind`; struct/class→leaf size; union→leaf
  size; pointer→`attr.size`; enum→underlying primitive size (else 4); nested array→leaf.
  `count = elemSize>0 ? totalSize/elemSize : 1`. `elementKind` set: primitive→
  `mapPrimitiveType`; struct/class/union→Struct + `refId=importUDT` + `structTypeName`;
  pointer→Pointer32/64; else `hexForSize(elemSize)`.
- **LF_ENUM**: map to underlying primitive kind (else UInt32) and `refId=importEnum`.
- **LF_PROCEDURE/LF_MFUNCTION**: emit Hex64.
- **LF_BITFIELD** (top-level, not in a member group): emit a single-member bitfield
  `Struct`.
- **default / unknown / `tt.get` miss**: emit Hex32.

### 5.4 Public entry points
- **`PdbFile::open(path, errorMsg)`** (`import_pdb.cpp:915`): exists check →
  `MappedFile.open` → `PDB::ValidateFile` → `CreateRawFile` → `HasValidTPIStream` →
  `CreateTPIStream` → `TypeTable`. Errors set distinct messages ("PDB file not found",
  "Failed to memory-map", "Invalid PDB file", "PDB has no valid TPI stream").
- **`extractPdbSymbols`** (`import_pdb.cpp:948`): validates DBI + its
  symbol-record/public-symbol/image-section sub-streams; `moduleName =
  QFileInfo(path).completeBaseName()` (e.g. "ntkrnlmp"). Reads **public symbols**
  (S_PUB32 only; RVA via `ConvertSectionOffsetToRVA`, skip rva==0) and **global
  symbols** (S_GDATA32/S_GTHREAD32/S_LDATA32/S_LTHREAD32 — each carries name+typeIndex+
  section/offset; skip rva==0 or empty name). Each `PdbSymbol{name, rva, typeIndex}`.
- **`enumeratePdbTypes`** (`import_pdb.cpp:1065`): scans all TPI records; for each
  non-fwdref UDT/enum with a real name (not null/empty/starting `<`), emits
  `PdbTypeInfo{typeIndex, name, size, childCount=fieldCount, isUnion, isEnum}`. Size for
  enum = underlying primitive size (else 4); for union/struct = leaf size. Tracks
  skip-diagnostics (non-UDT / fwdref / anon) and logs that empty TPI = stripped public
  PDB (expected for kernel32/advapi32; ntdll/ntoskrnl carry types). (test
  `enumerateTypes`: >100 types, finds `_KPROCESS` with childCount>0/size>0 and
  `_LIST_ENTRY`.)
- **`importPdbSelected`** (`import_pdb.cpp:1164`): for each requested typeIndex, dispatch
  to `importEnum` (if LF_ENUM) else `importUDT`; call `progressCb(i+1, total)` after
  each — **if it returns false, abort and return the partial tree** with errorMsg
  "Import cancelled". Empty result → "No types imported". (test `importSelected`:
  imports just `_LIST_ENTRY`, progress called >0 times, Flink/Blink self-ref, exactly
  1 root.)
- **`importPdb`** (legacy, `import_pdb.cpp:1211`): iterate TPI; for non-fwdref named
  UDTs, if `structFilter` empty import all, else import only the matching name and
  **break** after the first match. Empty result → "Type 'X' not found in PDB" or "No
  types found in PDB". (tests `importKProcess`/`verifyDispatcherHeader`/`verifyListEntry`/
  `importFilteredStruct` use this; transitive deps like `_DISPATCHER_HEADER`,
  `_LIST_ENTRY` come in as additional roots via `importUDT` recursion.)
- **`importTypeForSymbol`** (`import_pdb.cpp:1266`): typeIndex 0 → error "Symbol has no
  associated type". Walk LF_MODIFIER/LF_POINTER chains (≤16 deep) to the underlying type.
  If it resolves to a primitive → error "…resolves to a primitive". Must land on
  UDT/enum (else error). Extract `*typeName`. Resolve forward refs via
  `buildUdtDefinitionIndex`/`findUdtDefinitionIndex`. `importUDT`/`importEnum`. Empty →
  "Failed to import type at index N".

`missingFileReturnsError` test: `importPdb("C:/nonexistent.pdb")` → empty + non-empty
error (the "PDB file not found" path from `PdbFile::open`). This is the ONLY PDB test
that runs on non-Windows or without the symbol PDB — the others `QSKIP` if
`C:/Symbols/ntkrnlmp.pdb/…/ntkrnlmp.pdb` is absent.

**Concurrency**: none (single-threaded). `progressCb` is the only callback; cancellation
returns a partial tree.

**Platform**: Windows-only in C++ (RawPDB + Win32 file mapping). **Portability:
mostly-portable in Rust**: the **`pdb` crate** (pure Rust, cross-platform) replaces
RawPDB and removes the Win32 mapping + manual leaf decoding entirely. The Rust port can
build the type tree on Linux too, but to honor the "OS-specific stays behind cfg and
keeps compiling" rule, you may keep the public functions cross-platform (pdb crate is
portable) — the only Windows-specific aspect was the file mapping, which `pdb`'s
`Source`/`std::fs::File` handles portably. Map: `PDB::TPIStream`→`pdb::TypeInformation`;
`TRK::LF_*`→`pdb::TypeData` variants (`Class`, `Union`, `Enumeration`, `Member`,
`Pointer`, `Array`, `Modifier`, `Bitfield`, `Procedure`, `MemberFunction`,
`FieldList`); the variable-length leaf decoding (`leafValue`/`leafSize`/`leafName`) is
internal to the `pdb` crate; primitive type indices → `pdb::PrimitiveType` (still need a
mapping table equivalent to `mapPrimitiveType`); DBI public/global symbols →
`pdb::SymbolTable` (`PublicSymbol`/`Data`/`ThreadStorage`) + `AddressMap` for RVA
(`SymbolData::offset.to_rva(&address_map)`). Forward-ref resolution: `pdb` exposes
`Class.properties.forward_reference()` and you build the same name→index maps.

---

## 6. Cross-cutting porting notes

- **Error reporting**: every public fn takes `QString* errorMsg` (optional) and returns
  an empty `NodeTree`/`false`/`{}` on failure. Rust idiom: return
  `Result<NodeTree, String>` (or a typed error). Keep the *exact* English messages if
  any UI/test depends on them — tests only check `!err.isEmpty()`, so the strings are
  free to differ, but the *failure conditions* must match.
- **`refId` resolution pattern** (source + xml): nodes that reference a class by name
  push `PendingRef{nodeId, className}` during building; a post-pass sets `refId` from a
  `name→id` map. Self/forward references work because the root is added (and id known)
  before its fields. The PDB path uses `typeCache` keyed on typeIndex for the same
  effect during recursion.
- **`addNode` returns an index, not an id** — always re-read `tree.nodes[idx].id`.
- **Determinism**: no hashing of pointers; iteration is in source/file order. The XML
  exporter sorts roots and children **by offset** before writing. PDB enumeration is in
  TPI typeIndex order. Keep these orders for parity.
- **Integer parsing**: hex literals via base-16, decimal via base-10; Qt's `toInt`/
  `toLongLong(&ok)` return 0 on failure — replicate "parse or 0/skip" semantics.
- **No threading anywhere** in this subsystem. PDB `ProgressCb` is a synchronous
  callback (cancellation only).

## 7. Recommended Rust crates
- `serde`/`serde_json` — RCX native JSON (core model; not an imports/ file but the
  format these importers feed).
- `quick-xml` — ReClass XML import (Reader, handle `Event::Empty` for `<Node …/>`) and
  export (Writer; preserve attribute order + indent-4 if byte-parity desired).
- Hand-rolled tokenizer/parser for C/C++ source (mirror `import_source.cpp`); `regex`
  for the two comment-offset patterns.
- `pdb` — PDB type/symbol import (replaces RawPDB; pure Rust, portable). Add a
  `mapPrimitiveType` equivalent.
- `object` or `goblin` — PE parsing for `pe_debug_info` (or hand-roll fixed-offset reads
  against the Provider trait; the data comes from a `Provider::read`, not a file).
- `memmap2` — only if you choose to mmap PDBs (optional; the `pdb` crate works from any
  `Read+Seek`).

## 8. Open questions / risks for the port
- The C/C++ source tokenizer uses Qt's Unicode `QChar::isLetter/isDigit/isSpace`; the
  Rust port should decide whether to support non-ASCII identifiers (Qt does). For
  faithful parity use Unicode-aware `char` methods.
- XML round-trip does NOT round-trip offsets for every kind (the exporter writes sizes,
  the importer re-derives sequential offsets). The export/import tests compare kind+name
  (and sometimes strLen), not offsets — confirm the Rust port matches the same lossy
  behavior (e.g., bitfield→hex, Bool→UInt8, 128-bit/Float16/FuncPtr→Hex8 fallback).
- `import_reclass_xml` version detection keys only on the **first** comment; if a file
  has no version comment it defaults to V2016. The exporter always emits `<!--ReClassEx-->`
  so self-round-trips stay V2016.
- PDB tests (except `missingFileReturnsError`) require a specific `ntkrnlmp.pdb` and are
  Windows-path-hardcoded; they `QSKIP` otherwise. The Rust port's PDB tests will need a
  cross-platform fixture PDB (the `pdb` crate ships test PDBs; or generate one).
- `pe_debug_info` reads raw little-endian structs; ensure the Rust struct reads are LE
  regardless of host endianness (use `from_le_bytes` or `object`).
