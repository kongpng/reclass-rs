# Test Suite Behavioral Catalog (Fidelity Oracle)

This catalogs **every** file under `/home/loke/reclass-cpp/tests`. It is the
fidelity contract for the Rust port: each behavior asserted here must hold for
the corresponding Rust code. For each file: which subsystem/public API it
exercises, the concrete behaviors/invariants/edge-cases it pins, notable
fixtures, headless-vs-GUI classification, the CMake link set (a strong hint for
which Rust crate the test maps to), and porting notes.

## How tests are wired (from `CMakeLists.txt` lines ~360–581)

- The test harness is **Qt Test** (`QtTest`). Headless tests use
  `QTEST_MAIN`/`QTEST_GUILESS_MAIN` over `QCoreApplication`; GUI tests use
  `QApplication` and need a display/offscreen platform.
- Two tiers in CMake:
  - **Headless tests** (link only `Qt::Core` (+ `Qt::Test`, sometimes
    `Qt::Concurrent`/`Qt::Gui`/`Qt::Network`)) — safe on CI without a display.
  - **UI tests** guarded by `option(BUILD_UI_TESTS ... ON)` — link
    `Qt::Widgets`, `Qt::PrintSupport`, `Qt::Svg`, `QScintilla`, etc.
- The CMake source-set each test links tells you exactly which C++ subsystem it
  covers (e.g. tests that link `src/scanner.cpp` are scanner tests).

### Rust crate-mapping convention used below

The Rust workspace is expected to be split roughly along the C++ source files.
Mapped crate names below are the natural targets:

| C++ source(s) the test links            | Suggested Rust crate / module       |
|------------------------------------------|-------------------------------------|
| `core.h`/`core.cpp` (NodeTree/Node/Provider/LineMeta/ValueHistory) | `reclass-core`   |
| `format.cpp`                             | `reclass-core::format`              |
| `addressparser.cpp`                      | `reclass-core::addressparser` (`reclass-addr`) |
| `compose.cpp`                            | `reclass-compose`                   |
| `rtti.cpp` / `symbolstore.cpp`           | `reclass-rtti`                      |
| `typeinfer.h`                            | `reclass-typeinfer`                 |
| `disasm.cpp` (+ fadec)                   | `reclass-disasm` (crate: `iced-x86`/`yaxpeax`) |
| `scanner.cpp`                            | `reclass-scanner`                   |
| `generator.cpp`                          | `reclass-generator`                 |
| `imports/*`                              | `reclass-imports` (xml/source/pdb)  |
| `providers/*` (buffer/null/snapshot)     | `reclass-core::providers`           |
| `controller.cpp`                         | `reclass-controller`                |
| `editor.cpp`, popups, overlays           | `reclass-ui` (egui/Qt-bridge port)  |
| `themes/*`                               | `reclass-theme`                     |
| MCP bridge                               | `reclass-mcp`                       |
| `clipboard.h`                            | `reclass-core::clipboard`           |

---

## SUMMARY TABLE

Legend — **H** = headless (Qt::Core only, pure logic), **G** = GUI/offscreen-Qt
(needs QApplication/QScintilla/pixmaps), **Win** = Windows-only or skipped
off-Windows, **OOS** = out-of-scope for this port (plugin/remote/manual), **REG?**
= referenced in CMake but `.cpp` **absent** from the tree.

| File | Kind | Subsystem / API | Registered in CMake |
|------|------|-----------------|---------------------|
| test_core.cpp | H | NodeTree, Node, providers, LineMeta column spans, ValueHistory, findOverlaps, fieldPath | yes |
| test_format.cpp | H | `fmt::` value formatting/parsing/validation | yes |
| test_addressparser.cpp | H | `AddressParser` expression evaluator | yes |
| test_typeinfer.cpp | H | `inferTypes` heuristic type inference | yes |
| test_compose.cpp | H | `compose()` text+LineMeta layout engine | yes |
| test_chips.cpp | H | unified tail-chip model (Enum/TypeHint/Rtti/Comment) | yes |
| test_overlay_null_rtti.cpp | H | null-vtable "(Name class…)" chip emission | yes |
| test_static_fields.cpp | H | static fields, offsetExpr, structSpan exclusion | yes |
| test_rtti.cpp | H | `walkRtti` (MSVC) + `walkRttiItanium` + demanglers | yes |
| test_rtti_hint.cpp | H | RTTI hint integration in compose + module cache | yes |
| test_tutorial.cpp | H (+Win paths) | typed-pointer RTTI flow, SnapshotProvider forwarding | yes |
| test_disasm.cpp | H | `disassemble`/`hexDump` (fadec) + composed-address read | yes |
| test_import_xml.cpp | H | `importReclassXml` | yes |
| test_export_xml.cpp | H | `exportReclassXml` + import round-trip | yes |
| test_import_source.cpp | H | `importFromSource` (C/C++ header parser) | yes |
| test_generator.cpp | H | `renderCpp`/Rust/C#/Python/define code generation | yes |
| test_scanner.cpp | H | `ScanEngine`, `parseSignature`, `serializeValue` | yes |
| test_scanner_combinations.cpp | H | scanner (mode×cond×type×align×region) matrix | yes |
| test_clipboard.cpp | H (Qt::Gui) | `ClipboardCodec` JSON mime round-trip | yes |
| test_command_row.cpp | H | command-row source label + span parsing | yes |
| test_provider.cpp | H | `NullProvider`, `BufferProvider` API | yes |
| test_mcp.cpp | H (Qt::Network) | MCP JSON-RPC multi-client protocol | yes |
| test_roundtrip_winsdk.cpp | H | import→generate round-trip of huge WinSDK header | yes |
| test_theme.cpp | G(Widgets) | `Theme`/`ThemeManager` JSON+CRUD | NOT registered |
| test_command_palette.cpp | G | `CommandPalette` fuzzy match + menu enumeration | yes |
| test_goto_address.cpp | G | `GotoAddressDialog` live validation + recents | yes |
| test_options_dialog.cpp | G | `OptionsDialog` widget tree + palette | yes |
| test_controller.cpp | G | `RcxController` edit/undo/redo operations | yes |
| test_context_menu.cpp | G | controller insert/duplicate/remove/byte-selection | yes |
| test_editor.cpp | G | `RcxEditor` inline-edit, hit-test, cursor, rendering | yes |
| test_type_selector.cpp | G | `TypeSelectorPopup` + controller type-change | yes |
| test_type_visibility.cpp | G | cross-tab type visibility / create-new naming | yes |
| test_source_provider.cpp | G/Win | provider attach via plugin + source menu icons | yes |
| test_source_management.cpp | G | controller source lifecycle (load/clear/switch) | yes |
| test_overlay_widget.cpp | G | inline RTTI/TypeHint chip rendering in editor | yes |
| test_overlay_classcreate.cpp | G | `attachRttiClassToPointer` controller flow | yes |
| test_default_class_footer.cpp | G | multi-tab default-class footer correctness | yes |
| test_tooltip_flicker.cpp | G | tooltip show/hide flicker counting | yes |
| test_refresh_speedups.cpp | G | memory-source refresh optimizations | yes |
| test_dock_size_tip.cpp | G | live dock-divider drag size readout | yes |
| test_doc_tab_chrome.cpp | G | doc-tab close button + source icon paint | yes |
| test_tab_source_icon.cpp | G | `drawTabSourceIcon` visual regression | yes |
| test_project_dock.cpp | G | project dock placement/sizing | yes |
| test_titlebar_border.cpp | G | titlebar close-button red-hover no-bleed | yes |
| test_rendered_view.cpp | G(QScintilla) | rendered C++ view Scintilla styling | yes |
| test_tooltip.cpp | G | `RcxTooltip` arrow callout geometry/render | NOT registered |
| test_tooltip_event.cpp | G | `RcxTooltip` positioning across screen edges | NOT registered |
| test_tooltip_ui.cpp | G | `RcxTooltip` translucent-bg pixel render | NOT registered |
| test_import_pdb.cpp | Win | `importPdb`/`enumeratePdbTypes` (raw_pdb) | yes (WIN32) |
| bench_import_pdb.cpp | Win | PDB import benchmark | yes (WIN32) |
| bench_large_class.cpp | G bench | compose/applyDocument perf on 45k-field class | yes |
| bench_project.cpp | G bench | .rcx load + workspace model perf | yes |
| bench_spam_append.cpp | G bench | rapid append/type-cycle refresh + hex-dim coloring | yes |
| test_provider_getSymbol.cpp | Win/OOS | ProcessProvider getSymbol (live process) | NOT registered |
| test_windbg_provider.cpp | Win/OOS | WinDbgMemory plugin provider | yes (WIN32) |
| test_kernel_provider.cpp | Win/OOS | KernelMemory plugin provider | yes (WIN32) |
| test_scanner_ui.cpp | G/OOS | `ScannerPanel` UI (DISABLED — hangs suite) | DISABLED |
| test_dbgconnect.cpp | Win/OOS | standalone dbgeng DebugConnect probe (`main()`) | no |
| test_dbgdump.cpp | Win/OOS | standalone dbgeng dump-open probe (`main()`) | no |
| grab_tabs.cpp | G/tool | screenshot helper for dock-tab chrome | no |
| test_pixels.py | tool | Python PIL Fusion-outline-leak pixel scan | no |
| REG? test_new_class_selfattach.cpp | (G) | New-Class self-attach (loopback) | in CMake, file ABSENT |
| REG? test_byte_selection_controller.cpp | (G) | byte-selection ↔ controller | in CMake, file ABSENT |
| REG? test_byte_selection.cpp | (G) | byte-selection editor | in CMake, file ABSENT |
| REG? test_lenient_hex.cpp | (H) | lenient hex paste parser | in CMake, file ABSENT |

> **Porting note on REG? files:** CMake declares targets `test_new_class_selfattach`,
> `test_byte_selection_controller`, `test_byte_selection`, and `test_lenient_hex`,
> but the `.cpp` files are not present in this snapshot. Don't author Rust tests
> for them from scratch; their behaviors are mostly covered by
> `test_context_menu` (byte-selection extraction) and `test_controller`. The
> "lenient hex paste" parser lives in `clipboard.h` (`Qt::Core` only) — port it
> if/when the clipboard hex-paste path is implemented.

---

# HEADLESS / PURE-LOGIC TESTS (highest porting priority — these are the oracle)

## test_core.cpp  (H — `reclass-core`)
Links `core.cpp format.cpp compose.cpp addressparser.cpp profiler.cpp rtti.cpp symbolstore.cpp`. ~50 slots. The single most important fidelity file for the data model.

Behaviors/invariants asserted:
- **`sizeForKind`**: Hex8=1,Hex16=2,Hex32=4,Hex64=8,Float=4,Double=8,Vec3=12,Mat4x4=64,Struct=0,Hex128=16.
- **`linesForKind`**: Hex32/Vec2/Vec3/Vec4=1 line; Mat4x4=4 lines.
- **`kindToString`/`kindFromString`** round-trip for every kind `0..Array`.
- **`KindMeta` table completeness**: every enum value `0..Array` has a `KindMeta` (non-null `name`/`typeName`, `lines>=1`, `align>=1`); `sizeForKind`/`linesForKind`/`alignmentFor` must agree with the table; O(1) `kindMeta(k)` matches every entry.
- **`alignmentFor`**: Hex8=1,Hex16=2,Hex32=4,Hex64=8,Float=4,Double=8,Vec3=4,Mat4x4=4,UTF8=1,UTF16=2,Struct=1.
- **NodeTree topology**: `addNode` returns insertion index; first id == 1 then monotonic (`reserveId` strictly increasing); `childrenOf(parentId)` returns child indices (parentId 0 = roots); `depthOf` (root=0); `indexOfId` (−1 if absent); `subtreeIndices` includes node+descendants and terminates even with a self-referential parent.
- **`computeOffset(idx)`** returns int64 sum of ancestor offsets (no overflow at 0x7FFFFFFF; nested 0+16+8=24). **Cycle safety**: `depthOf`/`computeOffset`/`structSpan` must terminate (no hang) when parent pointers form a cycle; `invalidateIdCache()` after mutating parentId.
- **JSON round-trip** (`Node::toJson`/`fromJson`, `NodeTree::toJson`/`fromJson`): preserves baseAddress, baseAddressFormula, pointerSize, initialClass, and ALL Node fields (id, kind, name, structTypeName, classKeyword, parentId, offset, isStatic, offsetExpr, isRelative, arrayLen, strLen, refId, elementKind, ptrDepth, comment, enumMembers `(name,value)`, bitfieldMembers `(name,bitOffset,bitWidth)`). `m_nextId` restored to > max id. **Backward compat**: missing `isStatic`/`offsetExpr`/`comment` default to false/empty; **empty comment is NOT serialized** (key absent).
- **`Node::byteSize()`**: UTF8=strLen, UTF16=strLen*2, Array=arrayLen*sizeForKind(elementKind), bitfield container=sizeForKind(elementKind), else falls back to sizeForKind.
- **Provider basics** (also in test_provider): BufferProvider valid iff non-empty; `readU8`/`readU16` little-endian; `isReadable(addr,len)` overflow-safe (rejects huge addr, negative len; zero-len always readable; rejects past-end); `writeBytes` writes/fails past end; NullProvider not valid/readable/writable, reads return 0.
- **`structSpan(id)`**: max(child.offset + child.span); nested struct propagates; empty struct = 0; primitive array (no children) = arrayLen*elemSize; container w/ array child = arrayOffset + arraySize; **static fields excluded** from span (a static field at offset 1000 doesn't inflate span); leaf node short-circuits to byteSize.
- **Column-span geometry** (`typeSpanFor`/`nameSpanFor`/`valueSpanFor`/`staticExprSpanFor`): constants `kFoldCol=3`, `kTreeIndent=3`, `kColType=14`, `kColName=22`, `kSepWidth=1`, plus `kColValue`. Field line at depth d: type [ind, ind+14], name [ind+15, ind+37], value starts at ind+14+22+2. Continuation lines: only value span valid. Header/Footer: no spans valid. Static-expr span extracts `base + e_lfanew` from a `return base + e_lfanew → 0x…` line.
- **`normalizePreferAncestors`** (selection): drops any node whose ancestor is also selected. **`normalizePreferDescendants`**: drops a node if any descendant is selected.
- **`ValueHistory`** (heat-map ring buffer, `kCapacity=10`): empty→heat 0; 1 unique→0 (static); 2→1 (cold); 3..4→2 (warm); 5+→3 (hot). Duplicate consecutive value not counted (dedup); `count` increments per recorded transition; ring wraps at capacity (uniqueCount caps at 10, oldest pushed out, `forEach` yields surviving newest 10 in order); `last()` is most recent.
- **`findOverlaps()`**: detects sibling field byte-range overlaps; clean adjacency (touching) is NOT overlap; same-offset and partial and fully-contained ARE; one long field overlapping N smaller → N pairs; **union children ignored** (intentional overlap); **static fields ignored**; **root-level structs excluded** (independent classes); independent parents isolated; reports `{aId,bId,parentId}`.
- **`rootClassNames(tree)`**: enumerates ALL root structs (multi-class), prefers `structTypeName` over `name`, dedupes by name, empty tree → `["Untitled"]`, skips non-Struct roots.
- **`fieldPath(id [,sep])`** / **`nodeIdForPath(path)`**: dotted path `Player.Stats.Health` round-trips; root returns its label; unknown id → empty; custom separator supported; misses return 0.
- **`isPointerKind`/`isContainerKind`/`isStringKind`/`isHexNode`/`isVectorKind`/`isMatrixKind`/`isFuncPtr`** classification; `Node::isUnion`/`isBitfield`/`isEnum` from `classKeyword` (default "struct"); `kindFromTypeName` round-trip for every typeName, unknown → `ok=false`+Hex8.

## test_format.cpp  (H — `reclass-core::format`)
Links `format.cpp addressparser.cpp`. `fmt::` namespace value display/parse/validate.
- `typeName(Float)` == `"float"` padded to 14 chars (kColType).
- **`fmtFloat`** exact decimal-place ladder: `<10`→4dp (`3.1416f`), `>=10`→3dp, `>=100`→2dp, `>=1000`→1dp, `>=10000`→`50000.f`, overflow→`99999+f`/`-99999+f`; `inff`/`-inff`/`NaN`; `0.0000f`; `-0.0f` keeps sign; always `f` suffix; very small nonzero ≤9 chars.
- `fmtInt32(-42)`=`"-42"`; `fmtBool(1/0)`=`true`/`false`; `fmtDouble` always contains `.` (or e/E); fmtDouble NaN/Inf non-empty.
- **`fmtPointer64`**: 0 → `nullptr`; non-zero → `0x…`.
- **`fmtOffsetMargin`**: primary `00000010 ` (8 hex + space); continuation `  · ` (middle-dot); kernel widths param (16→16 hex, 4→4 hex).
- **`fmtStructHeader`** contains `struct`/name/`{` when expanded, omits `{` when collapsed; **`fmtStructFooter`** is always `};` with no `sizeof` comment.
- `indent(n)` = 2n spaces.
- **`parseValue(kind, text, &ok)`**: Int32 dec; Float; **Hex kinds parse as memory-order bytes** (`"DEADBEEF"`→bytes DE AD BE EF; `0x`-prefix stripped); Pointer64 parses as integer; Bool true/false (unknown→fail); overflow rejection per width (UInt8>255, Int8>127/<−128, UInt16>65535, Hex8>0xFF, Hex16>0xFFFF); signed-hex two's-complement (`0xFF`→Int8 −1, `0x80`→−128, Int16 `0xFFFF`→−1, Int32 `0xFFFFFFFF`→−1; over-width fails); Hex128 expects 16 space-separated bytes (8 too short fails); empty UTF8 ok (caller pads), empty non-string fails; UTF8 quoted string `"hello"`→`hello`; Hex16 space-separated `AB CD`.
- **`readValue`/`editableValue`**: Vec2/3/4 single-line returns comma-separated component count (1/2/3 commas); Hex128 editable = space-separated bytes (>=47 chars).
- **`validateValue`** empty→ok-ish (empty error string); overflow→non-empty error.
- `allTypeNamesForUI()` size == `std::size(kKindMeta)`, no duplicates.
- **`isValidPrimitivePtrTarget`**: rejects Hex8/Pointer64/Struct/FuncPtr64; accepts Int32/Float/Bool.

## test_addressparser.cpp  (H — `reclass-core::addressparser`)
Links `addressparser.cpp`. `QTEST_GUILESS_MAIN`. The `AddressParser::evaluate(expr, ptrSize=8, callbacks)` / `validate(expr)` expression engine.
- Hex literals: bare `AB`=0xAB, `0x1F4`, `0`, `7FF66CCE0000`. Legacy bare hex `140000000`.
- Arithmetic `+ - * /`, precedence (`*` over `+`), parentheses, unary minus, multiple additions.
- Bitwise `& | ^ << >>`, unary `~` (`~0`=all-ones, `~0xFFF`=…F000). C precedence: shift looser than `+`; `&` tighter than `|`; `^` between them.
- Module resolution via `cbs.resolveModule` `<Program.exe>+0x123`; not-found → error contains "not found".
- Dereference `[addr]` via `cbs.readPointer`, nested `[<mod>+[<mod>+0x100]]`; read failure → "failed to read".
- Identifier resolution via `cbs.resolveIdentifier` (`base`, field names like `e_lfanew`); unknown → "unknown identifier".
- Hex-vs-identifier disambiguation: all-hex-digits→hex; presence of non-hex char or `_`→identifier; bare `module.dll`/`module.exe` → identifier.
- Errors: empty→fail; unmatched `[`→error mentions `']'`; unmatched `<`→`'>'`; division by zero; trailing garbage→"unexpected"; trailing operator→fail.
- Backtick stripping (`7ff6\`6cce0000`); whitespace tolerance.
- Real scenario `(base + e_lfanew) & ~0xFFF` page-align.
- `validate()` returns empty string on valid, non-empty on invalid.
- **Porting note:** mirror callback struct `AddressParserCallbacks { resolveModule, resolveIdentifier, readPointer }` as a Rust trait/closure set.

## test_typeinfer.cpp  (H — `reclass-typeinfer`)
Links `typeinfer.h` only (header-only). `inferTypes(const uint8_t*, len [,InferHints])` → `QVector<TypeSuggestion{kinds, strength}>`.
- null/zero-len/all-zeros → empty.
- Hex64 float pair `{21.0488,547.3}`→ top is `[Float,Float]` strength>=3.
- Hex64 int pair `{42,99}`→ top `[Int32 or UInt32]`×2.
- Hex64 UTF-8 `"IChooseY"`→ UTF8 suggestion strength>=3 present.
- Hex64 pointer-like `0x00007FF6A0B01000`→ Pointer64 suggestion present.
- Hex32 clear float `21.0488f`→ Float strength>=2.
- Hex32 small int + monotonic `InferHints{monotonic,sampleCount,minObserved,maxObserved}`→ Int32/UInt32 strength>=2.
- Hex16 small unsigned `95`→Int16/UInt16; Hex8 `1`→UInt8.
- `formatHint`: single → `"float"`, split pair → `"float×2"` (U+00D7).
- Denormal float `0x00000001` NOT top-pick Float.
- Includes `QBENCHMARK` perf checks (single call; 200-node batch).

## test_compose.cpp  (H — `reclass-compose`)
Links `compose.cpp format.cpp addressparser.cpp profiler.cpp rtti.cpp symbolstore.cpp`. ~60 slots. The renderer that turns a NodeTree + Provider into `ComposeResult{text, meta[], lineStarts[], layout}`. `compose(tree, prov, viewRootId=0, compactColumns=false, treeLines=false, braceWrap=false, typeHints=true, showComments=true, symbolLookup={}, showRtti=true, showEnumChips=true)`.

Key invariants (slot names indicate coverage):
- Basic struct header/field/footer text; Vec3 single line; Hex node compose; null-pointer marker (`nullptr`); unreadable pointer doesn't attempt read; fold levels per depth.
- Nested struct rendering; **pointer deref expansion** (typed pointer expands target struct at deref'd address), null-deref shows template, collapsed pointer no expansion, **deref cycle safe** (A→B→A), mutual cycle A↔B.
- Struct footer simple; `LineMeta.nodeId` carried; **array header format** (count, element-kind, char-types as string), array spans clickable, array with struct children, array collapsed → no children, **array count recompose**, primitive array elements (4 shown), primitive array collapsed, struct-array still uses children.
- Pointer default void; Pointer32 default void; displays target name (uses `name` when no `structTypeName`); pointer spans / void spans; pointer→pointer chain; refId→deleted struct handled; pointer width computation; **all structs resolvable as pointer targets**.
- ClassKeyword JSON round-trip + default "struct"; command-row root-name span; text non-empty.
- **Union** header shows keyword, collapsed, structSpan = max member; **Enum** displays members, collapsed.
- **`compactColumns`**: typeW capped at `kCompactTypeW=22`; normal typeW wider; long types print in full (no truncation). Fixture: `src/examples/EPROCESS.rcx`, `MMPFN.rcx` (skipped if not found).
- **Bitfield** members rendered, JSON round-trip, byteSize.
- **Static fields**: header line shows `static` keyword, doesn't affect struct size, `isStaticLine` flag set, collapsed hides them, expression shown in text.
- Tree-lines at depth 2; primitive-array element count 4; bitfield members three; compose with comments; compose with brace-wrap.

## test_chips.cpp  (H — `reclass-compose`)
Same link set as test_compose. The unified tail-chip model `LineMeta::chips` (`LineChip{kind, text, startCol, endCol, enumCurrentValue, enumRefNodeId, rttiVtableAddr, typeHintKinds, ...}`; helpers `findChip(lm, kind)`).
- **Enum chip** fires on int field whose `refId` resolves to an enum node; chip text contains current member name (`RUNNING`), `enumCurrentValue` = read value, `enumRefNodeId` = enum id; startCol≥0, endCol>startCol; suppressed when `showEnumChips=false`.
- **TypeHint chip** fires on hex node with strong inference; rendered inline (startCol/endCol set), text plain (no brackets), `typeHintKinds` non-empty; always-on overlay (the old typeHints flag retired for it).
- **Rtti chip** fires on Hex64 whose value is a known vtable; text = plain demangled name `"Foo"` (no `{RTTI:}` prefix; RTTI supersedes Symbol suffix); `rttiVtableAddr` set; suppressed when `showRtti=false`.
- **Comment chip** fires for `Node::comment`; text = raw comment; multi-line comments collapse newlines to middle-dot (no `\n`); suppressed when `showComments=false`.
- **Chip order on one line**: Enum → TypeHint → Rtti → Comment (monotonic by `ChipKind` enum order and by startCol for inline chips; overlay-only chips have startCol<0 and skip the strip). Comment chip is the last inline chip (rightmost, so comment-edit strips from chip.startCol→EOL).
- Chip startCol/endCol bracket the chip text exactly within the rendered line (`r.text` + `r.lineStarts`).
- **Fixture pattern (reused across RTTI tests):** synthetic MSVC RTTI address space built in a flat buffer — vtable[-1]=COL VA, TypeDescriptor with mangled `.?AVFoo@@`, CHD/BCA/BCD, COL with signature 0/1. A `FakeModuleProvider : BufferProvider` overrides `enumerateModules()` to claim a `synthetic.dll` module covering `kImageBase`.

## test_overlay_null_rtti.cpp  (H — `reclass-compose`)
Links `compose.cpp ...`. The null-vtable call-to-action chip.
- Typed Pointer64 with value 0x00 → overlay-only Rtti chip, text `"(Name class…)"`, `rttiVtableAddr=0`, startCol≥0.
- Untyped void Pointer64 (refId 0) value 0x00 → same chip (composeLeaf handles raw pointer fields).
- **Hex64 with value 0x00 does NOT get the chip** (restricted to pointer-kinded fields so a fresh struct's 60 zero rows don't sprout 60 chips).
- `showRtti=false` suppresses the null chip too.

## test_static_fields.cpp  (H — `reclass-core` + `reclass-compose` + `reclass-addr`)
Links `compose.cpp format.cpp addressparser.cpp ...`. Static-field model end-to-end.
- `isStatic`/`offsetExpr` flags; regular field is not static; static field is a child (`childrenOf` includes it).
- JSON round-trip preserves isStatic/offsetExpr/kind; backward-compat default false; multiple static fields preserved; empty expr preserved.
- structSpan excludes static fields (mixed order, only-static→0).
- AddressParser expression evaluation for offsetExpr: `base`, `base+0x3C`, `base+e_lfanew`, subtraction, `base+index*8`, unresolved id fails, pure hex, parentheses, empty fails. With BufferProvider resolver (mimics `compose.cpp` makeResolver): field-value resolution + pointer-chain `[base]`/`[[base]]`.
- Compose output: static-field header has `static {`, static line has `isStaticLine`, collapsed struct hides static lines, expression text appears, static lines come AFTER all regular fields.
- `byteSize` for static fields = kind size (Struct=0); static field can be Struct/Pointer64 with structTypeName.

## test_rtti.cpp  (H — `reclass-rtti`)
Links `rtti.cpp symbolstore.cpp`. `walkRtti`/`walkRttiItanium` + demanglers; cross-platform via synthetic buffers (no real binary needed). **`smokeTestRealBinary` is Windows-only (combase.dll), auto-skips.**
- **`demangleRttiName`** (MSVC): `.?AVFoo@@`→`Foo`, `.?AUStruct@@`→`Struct`, nested `.?AVBar@Foo@@`→`Foo::Bar`, `.?AVZ@Y@X@@`→`X::Y::Z`; non-RTTI passthrough; empty→empty.
- **`demangleItaniumName`**: `3Foo`→`Foo`, `N3Bar3FooE`→`Bar::Foo`, `St9type_info`→`…type_info`, passthrough plain.
- **`walkRtti`** on synthetic MSVC RTTI (layout fully documented at top of file — vtable at 0x1000, COL at 0x1900, 3 TypeDescriptors Foo/Bar/Baz, CHD 3 bases): returns `ok`, `vtableAddress`, `completeLocator`, `imageBase`, `offset=0`, `rawName=.?AVFoo@@`, `demangledName=Foo`, `bases=[Foo,Bar,Baz]`, `vtable` has 5 slots at predictable addresses (slot 6 null terminates), `abi="MSVC"`.
- Rejects bad COL signature (must be 0/1) → error "signature"; rejects huge `numBaseClasses` (>256) → error "unreasonably".
- **`walkRttiItanium`** on synthetic Itanium fixture (`ItProv : BufferProvider` overrides `enumerateModules`): `abi="Itanium"`, name/nested-name, 5 vtable slots; rejects implausible `offset_to_top` (magnitude filter, error "offset_to_top"); rejects non-Itanium name string (error "mangle").

## test_rtti_hint.cpp  (H — `reclass-compose` + `reclass-rtti`)
Links `compose.cpp ... rtti.cpp`. Integration of walkRtti into `compose()`.
- Hint attaches (Rtti chip w/ `text` containing `Foo`, `rttiVtableAddr`=vtable) when a Hex64's value points at a synthetic vtable inside a module.
- No hint when value outside any module; no hint for null value.
- **Per-pass module cache**: with 32 fields all pointing at the same vtable, `enumerateModules()` is called O(1) (≤4) — NOT per-field. This is a critical performance contract.
- **Itanium auto-detect fallback**: MSVC walker rejects (no COL sig), Itanium fallback in `rttiForVtable` picks it up → chip with `Foo`.
- Hint fires regardless of `typeHints` toggle (RTTI independent of inference noise); typeHints=false still suppresses green TypeHint chips.

## test_tutorial.cpp  (H synthetic + Win-only live paths — `reclass-compose` + `reclass-rtti` + `providers/snapshot_provider`)
Links `compose.cpp ... rtti.cpp symbolstore.cpp` (+ psapi on Win). Mirrors the `selfTest` "RcxEditor live demo" pipeline.
- **Pure-synthetic, always run:** typed Pointer64 (refId→vtable struct) with value = synthetic Itanium vtable VA → RTTI chip with demangled `RcxEditor`, `rttiVtableAddr`=vtable; Hex64 baseline lights up too; `tree.initialClass` round-trips through JSON (empty omitted).
- `abiTagSetOnItaniumWalk`: walkRttiItanium → `abi="Itanium"`, `demangledName="RcxEditor"` (mangled `9RcxEditor`).
- **Windows-only (QSKIP elsewhere):** `walkReal*VtableInOwnProcess` against ProbeClass / TutorialTest QObject / MIDerived multi-inheritance; `typedPointerComposeOnRealQtObject`; **`rttiHintFiresWhenComposingThroughSnapshotProvider`** — the regression: `SnapshotProvider` must forward `enumerateModules()` to its real provider AND fall through reads to the real provider for pages not in the snapshot (collapsed-pointer target/type_info/__name pages), else RTTI walker reads zeros and bails; `typedPointerComposeWithSymbolsEnabled`.
- **Porting note:** the Windows live-process probes are inherently platform-specific; under the SCOPE note, the live OS provider is a stub. Keep the synthetic-buffer assertions (these ARE portable) and gate the live-process variants behind `#[cfg(windows)]` + skip, or omit.

## test_disasm.cpp  (H — `reclass-disasm`; Rust crate `iced-x86` or `yaxpeax-x86`)
Links `disasm.cpp compose.cpp ... fadec/decode.c fadec/format.c`. `disassemble(bytes, addr, bitness, maxBytes=…)` and `hexDump(bytes, addr, maxBytes)`.
- **Exact x64 mnemonic strings** (AT&T-free Intel syntax): `push rbp`, `mov rbp, rsp`, `ret`, `nop`, `xor eax, eax`, `sub rsp, 0x20`, `int3`, `push rdi`, `pop rsi`, `test eax, eax`, `lea rax, [rip+0x10]`, `call 0x1105` (rel computed from addr+len+disp), `jmp 0x1012`, `mov rax, qword ptr [rbx+0x10]`, `mov qword ptr [rsp+0x8], rcx`. Multi-instruction prologue decode. Address column is 16 hex chars in 64-bit, 8 in 32-bit, followed by two-space sep.
- x86 32-bit: `push ebp`, `mov ebp, esp`.
- Empty input→empty; invalid bitness (16)→empty; `maxBytes` caps line count (128); multiple nops with incrementing addresses.
- **`hexDump`**: 16 bytes/line, address column width adapts (8 / 16 hex), ASCII gutter with `.` for non-printables, lowercase hex `de ad be ef`, second-line address += 0x10, maxBytes caps.
- **End-to-end "read from composed address, not node.offset"**: builds a vtable-with-FuncPtr64 layout; verifies pointer-expanded FuncPtr64 lines have `offsetAddr` at the vtable (0x100/0x108) NOT root; reading at node.offset gives the wrong (vptr) value; full hover-flow simulation reads pointer value from snapshot then code bytes from the *real* provider (snapshot lacks code pages) and disassembles. **This is a behavioral contract for the disasm-hover feature.**

## test_import_xml.cpp  (H — `reclass-imports::reclass_xml`)
Links `imports/import_reclass_xml.cpp ...`. `importReclassXml(path, &error)`.
- Parses ReClassEx XML; type-number mapping (Type 9→Int64 vtable, 13→Float, 18→UTF8 w/ Size→strLen, 23→Vec3, 8→Pointer64). Cumulative offsets computed from Size. Pointer `Pointer="TestClass"` resolves `refId` to the named root struct. 1 root + 5 children = 6 nodes.

## test_export_xml.cpp  (H — `reclass-imports::reclass_xml`)
Links `imports/export_reclass_xml.cpp imports/import_reclass_xml.cpp ...`. `exportReclassXml(tree, path, &err)` + round-trip via import.
- Empty tree → export fails with error. Single struct: XML contains names + `ReClassEx`; round-trip preserves 3 children kinds in order.
- Pointer ref emits `Pointer="Target"`; round-trip resolves refId. Embedded struct emits `Instance="Inner"`. Array emits `Total="10"`+`<Array`. Text nodes UTF8/UTF16 strLen round-trip. Vectors Vec2/3/4/Mat4x4 round-trip. 4 consecutive Hex8 collapse to `Type="21"` (Custom) `Size="4"`; import expands Custom back to best-fit hex. Multi-class (5) round-trip preserves all names. Comprehensive round-trip (13 primitive kinds + self-pointer + UTF8) preserves kind/name/offset and self-pointer refId.

## test_import_source.cpp  (H — `reclass-imports::source`)
Links `imports/import_source.cpp ...`. `importFromSource(text, &err, ptrSize=8)` — a C/C++ struct-header parser. ~45 slots, very thorough type-name mapping table.
- Empty / no-struct input → empty + error.
- **stdint** uint8_t..int64_t; **Windows types** BYTE→UInt8, WORD→UInt16, DWORD→UInt32, QWORD→UInt64, ULONG→UInt32, LONG→Int32, USHORT→UInt16, UCHAR→UInt8, BOOLEAN→UInt8, BOOL→Int32, CHAR→Int8, WCHAR→UInt16; **platform-pointer** PVOID/HANDLE→Pointer64, SIZE_T/ULONG_PTR/uintptr_t/size_t→UInt64; **standard C** char→Int8, short→Int16, int→Int32, long→Int32; **multi-word** unsigned char/short/int/long, long long→Int64, unsigned long long→UInt64; float/double; bool/_Bool→Bool.
- Pointers: `void*`→Pointer64 refId 0; typed `Target*`→Pointer64 refId→Target; self-referencing; `void**` double-pointer→Pointer64.
- Arrays: `int32_t[10]`→Array; `char[64]`→UTF8 strLen 64; `wchar_t[32]`→UTF16 strLen 32; `float[2/3/4]`→Vec2/3/4; `float[4][4]`→Mat4x4; `float[8]`→Array(Float); struct array→Array(elementKind Struct).
- Offsets: comment offsets `// 0x8` drive layout; if any field has a comment offset, ALL use comment mode (uncommented fields get computed offset); computed offsets respect natural alignment (uint8 at 0, uint16 at 2, uint32 at 4, uint64 at 8).
- Multi-struct, pointer cross-ref, forward declaration resolution.
- Unions: anonymous union container `classKeyword="union"` with members at offset 0, structSpan=max member, following field offset after union; comment-offset unions; named union (`} u3;`).
- Padding `uint8_t _pad[0x10]`→2× Hex64; `static_assert(sizeof(X)==0x10)` sets tail padding (structSpan==0x10). Embedded struct (Inner→refId), typedef resolution, const/volatile qualifiers stripped, `struct Inner` prefix on type. Bitfields → bitfield container struct (`resolvedClassKeyword()=="bitfield"`) with `bitfieldMembers[].name/bitWidth/bitOffset` and `elementKind` (Hex64 for ULONGLONG 64-bit). Windows PEB-style (no comments). `class` keyword preserved. Inheritance `: public Base` skipped (only own fields). Enums: `enum Color{Red=0,…}`→Struct classKeyword="enum" with enumMembers `(name,value)`, auto-increment values, hex values, enum-in-struct (field UInt32 + refId→enum), `enum class Scope : uint8_t`. `basicRoundTrip` import matches a hand-built tree.

## test_generator.cpp  (H — `reclass-generator`)
Links `generator.cpp compose.cpp ...`. ~70 slots. Code generation to multiple targets via `renderCpp(tree, rootId, externalTrees?, emitAsserts?)` + Rust/C#/Python/define dispatchers + `renderCode(format, …)`.
- **C++**: `#pragma once`, `// sizeof 0x10` on closing brace, `struct Player\n{` brace-on-newline, field decls `int32_t health;`, offset comments `// 0x0`, `static_assert(sizeof(Player)==0x10)` only when emitAsserts; padding gaps `uint8_t _pad…[0x4]`; tail padding; **overlap WARNING** comment (union members exempt); nested struct, primitive array, pointer fields, vectors, string types, full SDK export; duplicate-type-name disambiguation; null/invalid/non-struct/empty-struct roots; name sanitization; export to file; deeply nested; inline anonymous struct; opaque type no stub; **static field NOT in struct body** + comment format + size unchanged; forward declaration for pointer target; `#include <cstdint>`; nullptr pointer value; Hex128 in union; type aliases; struct-layout size.
- **Rust**: simple struct, padding, pointers (`Option`/raw), vectors, FuncPtr (`Option`), `#[allow(dead_code)]`, pointer field, all kinds.
- **C#**: simple struct, pointers, enum, vectors, `[StructLayout]` size, nullable-disable, Vec3.
- **Python (ctypes)**: simple struct, pointers, typed pointers, enum, vectors, union, FuncPtr `CFUNCTYPE`, `_fields_`/`__slots__`.
- **define** target: simple struct, skips hex, enum members, all kinds.
- Dispatch tests confirm `renderCode(format)` routes to the right backend (C++/Rust/C#/Python/define); tree-scope includes referenced types.

## test_scanner.cpp  (H — `reclass-scanner`)
Links `scanner.cpp` (+ `Qt::Concurrent`, psapi on Win). ~200 slots — the largest single behavioral file alongside test_editor. `ScanEngine` (async, emits `finished(QVector<ScanResult>)`, `progress`, `finished` via signals; driven synchronously in tests via a QEventLoop). Plus free functions `parseSignature`, `serializeValue`, `naturalAlignment`, BMH search, system-module detection.
- **`parseSignature`**: empty/spaces→fail "Empty"; single byte; space-separated; `??` wildcards (mask byte 0); single `?`; packed no-spaces; C-style `\x..`; lowercase/mixed-case hex; invalid hex→fail; odd char count→fail; invalid token width; leading/trailing spaces; all-wildcards.
- **`serializeValue(type, text, &pattern, &mask, &err)`** for every ValueType (Int8..UInt64, Float, Double, Vec2/3/4, UTF8, UTF16, hex bytes): overflow rejection, negative ints, hex uint, wrong vec component count→fail, empty→fail, invalid int/float→fail.
- **`naturalAlignment`**: Int8=1,Int16=2,Int32=4,Int64=8,Float=4,Double=8,Vec3=4,UTF8=1,UTF16=2.
- **Scan engine** against custom `RegionProvider : BufferProvider` (overrides `enumerateRegions`): exact match, wildcard match, no-match, alignment-4, maxResults cap, empty provider, empty pattern, **chunk-boundary overlap** (matches straddling internal read-chunk seams), multiple matches, single-byte pattern, pattern larger than data, pattern exact size, match at end-of-buffer.
- **Region filters**: `filterExecutable`, `filterWritable`, both; region module-name preserved; **RegionType** filter (privateOnly skips Image/Mapped, keeps Private); **skipSystem** (excludes ntdll, inactive by default, combines with privateOnly).
- Abort mid-scan; progress emitted; `isRunning` state.
- Typed value scans: int32, float, utf16 string, vec3.
- Provider region defaults: empty regions default, NullProvider empty regions, custom regions.
- Mask size mismatch handling; multiple regions; overlapping matches; one-byte buffer; all-wildcard pattern.
- **Address-range** (`startAddress`/`endAddress`): no-limit, clips results, outside-data, with-regions, unknown-value-with-range.
- **constrainRegions** (huge battery, ~30 slots): multiple ranges, intersect provider regions, no-overlap, gaps, partial overlap, mixed module+anonymous, fallback provider, adjacent regions, writable filter preserved, extends before/after, empty constraint scans all, single range, with start/end, unknown-value, non-zero base, zero-size, inverted range, overlapping constraints, pattern at first/last byte, one-byte-after-end, region smaller than pattern, exact-fit, match at region boundaries, multibyte at clip boundary.
- Boundary value scans (int32/float/double/int64/utf16/vec3 at region start/end); pattern at region start/end + with wildcard; multiple positions in constrained region; alignment skips unaligned (1/2/4/8); overlapping writes.
- **System-module name detection** (`sysmod_*`): kernel32 with/without ext, case-insensitive, qtCore, CRT, user-binary-not-system, linux `.so`.
- **Boyer–Moore–Horspool** search parity (`bmh_*`): single byte, long pattern, pattern==length, larger than data, at-end, equivalent to naive.
- Address cap clips above limit; **region cache reused across scans**; scan stats emitted.
- **Comparison conditions** (CE-style): BiggerThan/SmallerThan/Between first-scan; **IncreasedBy/DecreasedBy rescan** (seeded from previous results). Rescan empty seed.
- `e2e_findMutateRevalidate` full cycle; adaptive chunk for large region; BMH path parity; **`scanResult_jsonShape`** (ScanResult serialization shape). `selfAttach_findMutateRevalidate` (Windows self-process, declared separately).
- **Porting note:** map `ValueType`, `ScanCondition`, `ScanRequest{pattern,mask,pattern2,alignment,condition,valueType,valueSize,startAddress,endAddress,filterExecutable,filterWritable,filterRegionType,skipSystem,constrainRegions}`, `ScanResult`, `MemoryRegion{base,size,read,write,exec,moduleName,type}`, `RegionType{Private,Image,Mapped}` exactly. The async engine maps to a Rust thread/channel; tests drive it synchronously.

## test_scanner_combinations.cpp  (H — `reclass-scanner`)
Links `scanner.cpp`. Data-driven combinatorial sweep using `QTest::addColumn`/`newRow` (port as parameterized Rust tests).
- **Fast-Scan alignment** matrix (value planted every 4 bytes in 256-byte buffer): align 1→64, 4→64, 8→32, 16→16, 32→8, 64→4 hits.
- **condition × value-type** matrix (8 numeric types × {Exact100→1, Exact50→2, Bigger25→3, Smaller25→0, Smaller75→2}); `endAddress` restricts to populated region.
- **Between** matrix (values 10/50/100/200/300): `0..1000`→5, `50..200`→3, `11..99`→1, `301..999`→0, `100..100`→1.
- **Region-filter combos** (rwx/rw-/r-x): none→3, exec→2, write→2, both→1.
- **Address-range × alignment**: endAddress clips proportionally per alignment.
- **Signature wildcards × alignment**: `AB CC DD EE`→1, `AB ?? DD EE`→2, `AB ?? DD ??`→3, `FF FF FF FF`→0.
- Uses `SyntheticProvider : BufferProvider` with configurable `enumerateRegions`.

## test_clipboard.cpp  (H, links Qt::Gui for QMimeData — `reclass-core::clipboard`)
`ClipboardCodec::serialize(tree, ids)→QMimeData*` / `deserialize(target, mime)→{nodes, rootIds}` / `plainDump`.
- Leaf round-trip: serializes JSON mime (`kMimeType`) + plaintext; deserialize remaps to a **fresh id not colliding** with target; preserves kind/name; rootIds size 1.
- Subtree round-trip: copying a root struct carries 4 nodes, ids remapped, parent pointers re-wired (3 children point to new root). Pasting into the SAME tree gives all-fresh ids.
- Unknown mime / null mime → empty result. `plainDump` human-readable (`+0x0000`, names). Multiple roots preserve order.

## test_command_row.cpp  (H — `reclass-controller`/`reclass-ui` helper)
Links nothing but provider headers. Tests **replicated** label logic (not the real updateCommandRow): `buildSourceLabel(prov)` = `source▾` when name empty, `'name'▾` otherwise; full row `   'name'▾  0xADDR`; `commandRowSrcSpan` finds the clickable source span up to the `▾` (U+25BE). Provider switching null→file→"process". **Porting note:** these are spec for the command-row source-picker UI; the helper logic should live in the controller/UI layer.

## test_provider.cpp  (H — `reclass-core::providers`)
Links nothing extra (provider headers only). NullProvider and BufferProvider full API.
- NullProvider: not valid, size 0, `read` fails leaving buf unchanged, readU8/readBytes return zero(ed), not writable, name empty, getSymbol empty.
- BufferProvider: empty→invalid; non-empty→valid; name+kind ("File") from ctor; readU8/U16(LE)/U32/U64/F32/F64/`readAs<T>`; readBytes full/offset/past-end(zeroed)/zero-len; isReadable bounds; writable + writeBytes/`write` + past-end fail; **`fromFile`** (nonexistent→invalid; valid temp file→valid, size, readU8, name=basename); polymorphic `unique_ptr<Provider>` null→buffer; getSymbol always empty for buffer.

## test_mcp.cpp  (H, links Qt::Network — `reclass-mcp`)
`QTEST_GUILESS_MAIN`. Tests a `MockMcpServer` (QLocalServer/QLocalSocket) that mirrors `McpBridge`'s multi-client JSON-RPC architecture — so this is a **protocol** contract, not a test of real bridge code.
- Single client: `initialize` returns `protocolVersion`/`serverInfo`; `tools/list` returns tool array; unknown method → error `-32601`.
- Multi-client: both initialize independently; disconnect one keeps the other; **notification broadcast** only reaches initialized clients (uninitialized client gets nothing); serial requests carry their own ids; all-disconnect → server survives + accepts reconnect.
- Protocol errors: invalid JSON → `-32700`; missing `method` → `-32600`; notifications (`notifications/initialized`, `notifications/cancelled`) produce no response.
- `tools/call`: unknown tool → `-32601`; missing tool name → `-32602`; `mcp.reconnect` returns "Disconnected." then server drops only that client (other unaffected).
- **Porting note:** the Rust MCP server must implement this exact JSON-RPC 2.0 surface and multi-client broadcast semantics.

## test_roundtrip_winsdk.cpp  (H — `reclass-imports::source` + `reclass-generator`)
Links `imports/import_source.cpp generator.cpp ...`. Fixture: `WINSDK_HEADER_PATH` (a CMake define → `src/examples/windows-x86_64.h`, a large real Windows header).
- `importFromSource` of the full header yields ≥3000 root structs.
- **`_PEB` field offsets** match `WinDbg dt ntdll!_PEB` (sorted child offsets validated against known Windows ABI).
- `roundTrip30`: 30 random structs survive import→generate→re-import. `generateRcx`: produces a valid `.rcx`-shaped JSON.
- **Porting note:** ship `windows-x86_64.h` (and the example `.rcx` files) as test fixtures in the Rust workspace.

---

# GUI / OFFSCREEN-QT TESTS (port last; behaviors still binding for the UI layer)

These need `QApplication` (use `QT_QPA_PLATFORM=offscreen`). In Rust (egui or other), they map to UI-layer integration tests; many assert geometry/pixels and will need re-expression against the chosen toolkit, but the **logical** contracts (what edit produces what tree change, what fires what signal) are toolkit-independent and belong in `reclass-controller`/`reclass-compose` tests where possible.

## test_controller.cpp  (G — `reclass-controller`)  ~65 slots
Links the full editor stack (`controller.cpp editor.cpp compose.cpp` + popups + themes + widgets + disasm). `RcxController` is the command/undo layer over a `RcxDocument`. **Most of these are pure logic with a UI shell and are the second-most-important oracle after test_core.**
- Value edit writes data to provider; undo/redo restores; float; rename node (+undo/redo); change node kind; insert+remove node (+undo); hex value edit; inline-edit round-trip; **source switch preserves base address** (and a fresh doc uses provider base); toggle collapse (+round-trip).
- ValueHistory popup only during edit; **delete clears heat for shifted nodes**; ValueHistory ring buffer; clear-history resets heat.
- Inline-edit primitive array element.
- **Static fields**: add (+undo), change expression, delete preserves struct size, rename preserves expression, type-change preserves flags.
- **Quick type change** hex same-size / shrink / grow; cycle same-size type variants (excludes string & vector types).
- Delete-key removes node; duplicate node; split hex node (+undo); group into union; insert node auto-offset; batch change kind; convert to typed pointer; insert-above shifts offsets; delete root struct; move node swaps offsets; change base address; change array meta; change class keyword; change comment; collapse/expand all; nullptr pointer display; generator-prepare-children; batch remove; bool/negative-int value; multi-select batch cycle type.
- `Node::toJson` omits defaults; includes `isRelative` when set.
- **Space-key type-cycle** behaviors (resize-wrap + multi-select, full circle, no-overlap-after-grow, selection survives join, rapid cycle no corruption) — these guard the meta-diff narrowing/refresh path.

## test_context_menu.cpp  (G — `reclass-controller`)
Same link set. Context-menu controller operations + byte-selection extraction.
- Insert adds one node (+auto-offset); duplicate adds one (+preserves original, +undo, +struct no-op, +copy parent, +multiple); insert at root; **append 128 bytes** (+undo); insert child into struct; remove+undo; insert struct and children; batch remove; invalid parent/index guards.
- `changeToPtrStar` ("Foo*") creates a class and sets refId.
- **Byte-selection extraction** (`extractByteSelection`): mid-row across rows, inside single row, row-aligned no pads, undo restores original, preserves typed fields, ignores unrelated root structs, refuses partial non-hex selection.

## test_editor.cpp  (G — `reclass-ui`/editor)  ~70 slots
Links editor + QScintilla. Drives `RcxEditor` (a QScintilla subclass) with synthetic key/mouse/focus events. Heavily UI/pixel-oriented.
- **Inline editing**: command-row line rejects edits; re-entry; commit-then-re-edit; mouse-click commits; type-edit cancel; header/footer line edit; parse hex with spaces; type autocomplete typing+commit; type-edit click-away no change; **column-span hit-test** (which span a click lands in); selected-node indices; value-edit commit emits signal.
- **Base-address display/edit**: display, span, edit begins, full caret-movement clamp suite (`testAddrEdit*`: left/right arrow clamp at span edges, backspace/delete stop, Home/End, typing stays in span, click-outside commits, Escape cancels, Enter commits, formula span, vertical keys blocked, no-leak-right, selection collapse left/right).
- **Cursor**: after left click, shape over text/type/fold-column, no I-beam after click-then-move.
- Command-row root-class edits + name; root fold suppressed; **command-row hover survives repeated refresh**.
- **Rendering/pixels**: accent marker on selected rows; menu item size accessible; menu hover renders amber text; struct-preview popup on collapsed typed pointer (and NOT when expanded); pointer expansion renders children / collapsed hides / null shows template / chain expansion; status-bar view-toggle buttons; H-scroll reset after name shrink; resize-grip corner symmetry; struct-type clickable; static-field name/type/expr editable + no separator; disasm popup dismisses on mouse-move-through; comment edit on non-hex/hex field; semicolon key emits signal; dock tab-bar border alignment.

## test_type_selector.cpp  (G — `reclass-ui` + `reclass-controller`)  ~55 slots
Links editor stack + `typeselectorpopup`. `TypeSelectorPopup` + controller type-change.
- Chevron span detection/rejection, spans with prefix.
- `parseTypeSpec` ("`Foo`", "`Foo[10]`", "`Foo*`", "`Foo**`", empty, whitespace, "`Foo[0]`", "`prim*`", "`prim**`").
- Popup lists root structs; emits signals; view-switching + create-type; field-type composite changes node to Struct; composite with pointer modifier; primitive still works; section headers present; primitive array creation; delegate icon scales with font; popup width scales with font; updates on theme change + auto-connects.
- **Primitive-pointer fallback rules**: `Hex64*`/`Hex8*`/`Ptr64*` fall back to void pointer; `Int32*` creates primitive pointer; `double**` creates primitive pointer; `ptrDepth` JSON round-trip + default; compose shows void-ptr for hex ptrDepth; `isValidPrimitivePtrTarget`.
- Category enum on entry / default primitive / composites categorized in controller; three-group sections; struct-embed auto-selects current; create-new signal carries modifier; create-new with ptr/array modifier.
- Includes several `benchmark*` perf checks (popup open, first show, large SDK, cold-vs-warm).

## test_type_visibility.cpp  (G — `reclass-controller`)
- Create-new type gets default name; increments name on collision; cross-tab types visible; `findOrCreate` reuses existing; external types skip local duplicates.

## test_source_management.cpp  (G — `reclass-controller`)
- Initial provider is Null; loadData creates valid provider; clearSources resets to Null (+clears value history, +clears data path, +resets snapshot); select-source clear command; clear-then-refresh works; multiple clear idempotent; switch invalid index no-op; provider read fails after clear; NullProvider name empty.

## test_source_provider.cpp  (G/Win — `reclass-controller` + `providerregistry`)
Links full stack + windows.h; uses a mock `IProviderPlugin` reading current process. **Windows-specific in mock, but the registry/menu logic is portable.**
- Attach via plugin preserves base address; provider reads correct data (not PE header); ProviderRegistry register/find/unregister; source menu icons load; menu action icon visibility; select-source updates base address; dll filename in provider info.
- **Porting note:** under SCOPE, the live process plugin is a stub; keep `ProviderRegistry` register/find/unregister + base-address-on-attach contracts, mock with a buffer/snapshot provider.

## test_overlay_widget.cpp  (G — `reclass-ui`)
- After `applyDocument`, RTTI chip text appears inline at startCol..endCol; null-RTTI chip appears inline; **RTTI chip click is deliberately disabled** (no signal fired).

## test_overlay_classcreate.cpp  (G — `reclass-controller`)
`RcxController::attachRttiClassToPointer(nodeId, demangledName)`.
- Creates a fresh root struct named after the demangled type and wires the clicked node's refId to it; second click with same name suffixes `_N` (always-new); empty base name → "NewClass"; new struct has default fields; the whole operation is atomically undoable. **Mostly logic — port into controller tests.**

## test_default_class_footer.cpp  (G — `reclass-ui`/compose)
- Footer stays clean across multiple "New Class" tabs (regression: 2nd+ tab footer was malformed); editor text matches compose across sequential applies; footer survives setCommandRow-then-refresh. Replicates `MainWindow::buildEmptyStruct`.

## test_tooltip_flicker.cpp  (G — `reclass-ui`)
Counts `QEvent::Show`/`Hide` on `rcx::sharedRcxTooltip()` while streaming mouse-moves.
- Bridge doesn't flicker on stationary cursor; command-row arrow tooltip survives refresh; tutorial-mode live-refresh no flicker; button tooltip survives editor refresh ticks; chip tooltip survives live-refresh ticks; chip hover doesn't flicker at chip-column boundary. **Contract: a non-flickering tooltip shows once on entry / hides once on exit per oscillation.**

## test_refresh_speedups.cpp  (G — `reclass-controller` + `snapshot_provider`)
Memory-source refresh optimizations (counting reads via instrumented provider):
- Permanent pages marked after module read (.rdata read once per attach); collapsed pointer skips target; page-stability backoff climbs when idle (unchanged pages re-read at half rate); viewport-bounded re-reads (only pages in view); adaptive backoff widens interval on idle; focus-out widens interval; minimize pauses timer; `SnapshotProvider` permanent-set + merge keeps existing pages.

## test_dock_size_tip.cpp  (G — `reclass-ui`)
Live dock-divider drag size readout via a qApp event-filter; probes which Qt6 cursor mechanism the platform uses; synthesized drag changes dock width; readout fires every move; resize events fire continuously; full pipeline per-pixel; workspace opens at minimum width; tooltip body text changes per update. **Largely Qt-internal; in Rust port this is toolkit-specific UI behavior.**

## test_doc_tab_chrome.cpp  (G — `reclass-ui`)
- Close (X) button on a doc tab closes the dock; `rcxSourceIcon`/`rcxSourceLive` dock properties persist across tabify/reorder; tab paints the source icon at the expected location (avg-alpha pixel sampling; live tab has visible icon pixels).

## test_tab_source_icon.cpp  (G — `reclass-ui`)
`drawTabSourceIcon` visual regression: renders at correct location/size (x≈8..22, vertical-center band); tint matches requested color (selected=white, unselected=dim); `live=false` dims opacity to ~40%; different icon paths render visibly different pixmaps; icon and text baselines align.

## test_project_dock.cpp  (G — `reclass-ui`)
- Project dock starts on the left of the central tab widget; width is 10–40% of window; stays left after hide/show; respects user drag to the right (showProject must not force it back).

## test_titlebar_border.cpp  (G — `reclass-ui`)
- Custom frameless titlebar: close-button red hover (`indHeatHot`) does NOT bleed below the title bar (vertical pixel scan asserts no red at/below titleBarBottom). Saves diagnostic PNGs.

## test_rendered_view.cpp  (G(QScintilla) — `reclass-ui` rendered view)
Links `generator.cpp compose.cpp ...`. The read-only "Rendered C++" Scintilla view styling (replicates `MainWindow::setupRenderedSci`).
- Caret-line background not yellow; caret line enabled; paper color = theme bg; caret fg; selection colors; syntax colors (keyword/comment/number/string/preprocessor/default) from theme; all styles have dark paper; margin colors; generated code appears in rendered view; brace-match disabled. Color encoding 0x00BBGGRR.

## test_theme.cpp  (G(Widgets) — `reclass-theme`; NOT registered in CMake)
`Theme`/`ThemeManager`. **Note: not in CMake's `add_test` list — verify before relying on it as CI gate.**
- Built-in themes present (≥2, "Reclass Dark" + "Warm" with exact hex colors); JSON round-trip preserves all color fields; missing fields → invalid QColor; ThemeManager has built-ins ("Reclass Dark" first, "VS2022 Dark", "Warm"); `setCurrent` emits `themeChanged` + updates index; CRUD (add/update/remove). **Logic is portable** (color de/serialization, manager state) even though it links Widgets.

## test_command_palette.cpp  (G — `reclass-ui`/`reclass-controller`)
- **`CommandPalette::fuzzyScore`** (pure logic, portable): empty needle matches everything; exact word-start outscores middle-of-word; miss→0; acronym ("fs"→"File > Save"); case-insensitive.
- Menu enumeration: enumerates leaf actions with path "File > Open"; skips separators; disabled actions included but `enabled=false`; `activateEntry` triggers the QAction; shortcuts captured.

## test_goto_address.cpp  (G — `reclass-ui` + `reclass-addr`)
`GotoAddressDialog` over AddressParser.
- OK disabled on empty; enabled on valid input (`resolvedAddress`); rejects invalid; evaluates module expressions `<game.exe>+0x40`; **recents** persist via QSettings (most-recent-first, dedupe-moves-to-top, capped at `kMaxRecent`); recent list shows existing entries.

## test_options_dialog.cpp  (G — `reclass-ui` + `reclass-theme`)
`OptionsDialog` / `OptionsResult{themeIndex, fontName, menuBarTitleCase, autoStartMcp, refreshMs}`.
- Creates all widgets (tree, 3 stacked pages, themeCombo ≥3, fontCombo=2, show-icon check, OK/Cancel `DialogButton`s); result reflects input; no stylesheet on palette-driven widgets (DialogButton exempt); **palette Highlight == theme.selected** and differs from background; tree page switching; search filter hides items; refresh-rate spin box (660 default, min 1, max 60000, clamps min 1, result reflects input); dialog inherits app palette when shown.

---

# WINDOWS-ONLY / IMPORT-PDB TESTS

## test_import_pdb.cpp  (Win — `reclass-imports::pdb`; Rust crate `pdb`)
Links `imports/import_pdb.cpp ... raw_pdb`, only built `if(WIN32)`. Requires `ntkrnlmp.pdb` fixture at a hardcoded symbol-cache path (QSKIP if absent).
- `importPdb("nonexistent")` → empty + error.
- `importPdb(pdb, "_KPROCESS")` → `_KPROCESS` root with `Header` (embedded `_DISPATCHER_HEADER` at 0), `ProfileListHead` (embedded `_LIST_ENTRY` at 0x18); transitive dep structs imported.
- `_LIST_ENTRY`: Flink (Pointer64 @0) / Blink (@8) self-referencing (refId→`_LIST_ENTRY`).
- `enumeratePdbTypes` → ≥100 `PdbTypeInfo{name, childCount, size, typeIndex}`; `_KPROCESS`/`_LIST_ENTRY` present. `importPdbSelected(pdb, indices, &err, progressCb)` with progress callback (cur≤total).
- **Porting note:** PDB parsing is a Rust crate (`pdb`); behavior table (embedded struct resolution, self-ref pointer) ports directly. Fixture availability is platform-bound.

## bench_import_pdb.cpp  (Win bench — `reclass-imports::pdb`)
`benchEnumerateAll` / `benchImportAll` timing of the above; QSKIP without fixture.

---

# BENCHMARKS (perf guards, not strict oracles — but they pin data shapes)

## bench_large_class.cpp  (G bench — `reclass-compose` / `reclass-ui`)
Compose/applyDocument/remove/changeKind perf on a 45 000-field class; hover-highlight + selection-overlay perf. Fixture: built programmatically (`src/examples/LargeClass.rcx` is the human counterpart).

## bench_project.cpp  (G bench — `reclass-controller` + workspace model)
`benchNewClass`, `benchLoadVergilius` (`Vergilius_25H2.rcx`), `benchLoadWinSDK` (`WinSDK.rcx`), `benchJsonParse`, `benchNodeTreeFromJson`, `benchBuildWorkspaceModel`, `benchWorkspaceSearch`. Validates large `.rcx` load paths.

## bench_spam_append.cpp  (G bench — `reclass-controller`/compose)
Rapid append + type-cycle through `editor→appendSingleFieldRequested→insertNode→applyCommand→refresh`. Asserts (a) refresh stays cheap during rapid edits, (b) per-line pass counts match type-change events (regression guard for a `sameLine` bug), (c) `IND_HEX_DIM` indicator stays applied to ALL hex64 lines after each press (including controller-driven path). Has real test assertions (`testHexDimOnLastAppendedLine`, `testControllerSpamDownPreservesAllDim`, `benchTypeCycleColouringCorrect`) — these ARE behavioral, not just timing.

---

# OUT-OF-SCOPE (plugins / live-OS providers / manual probes / tooling)

Per the SCOPE NOTE, the live process/kernel/remote/WinDbg memory providers are **out of scope** — represented only as documented stubs behind the `Provider` trait. The following tests exercise those plugins or manual debug probes and are **not** to be ported as behavioral tests; the abstract `Provider` contract they share is already covered by `test_provider`/`test_core`/`test_scanner` (via Buffer/Null/Snapshot/Region providers).

- **test_windbg_provider.cpp** (Win, registered) — WinDbgMemory plugin: plugin name/version/`canHandle` (tcp/npipe/pid/dump/invalid), connect bad/valid, name, isLive, baseAddress, read MZ/4k/refreshes on main+background threads, readU16, PE signature, zero/negative length, getSymbol, enumerateRegions (module names, executable), scanner signature/value over the provider, and **kernel** read/regions/scan/vtop. **OOS.**
- **test_kernel_provider.cpp** (Win, registered) — KernelMemory driver plugin: plugin name/loadType/canHandle; no-driver invalid; KUSER_SHARED_DATA fields (NtMajor/Minor/BuildNumber, SystemTime, TickCount, cross-validate vs RPM); self read MZ/PE; sig-scan; regions; PEB; symbol; CR3; virtual-to-physical (vtop) for KUSD/self-module/unmapped; page-table read; driver ping/version. **OOS.**
- **test_provider_getSymbol.cpp** (Win, NOT registered) — `ProcessProvider::getSymbol` against the live self-process (DuplicateHandle/EnumProcessModulesEx; resolves own exe + ntdll). **OOS** (live process provider).
- **test_dbgconnect.cpp / test_dbgdump.cpp** (Win, NOT registered, `int main()`) — standalone dbgeng probes (DebugConnect over TCP; OpenDumpFile + ReadVirtual). Manual diagnostics, not Qt tests. **OOS.**
- **test_scanner_ui.cpp** (G, **DISABLED** in CMake — "hangs the suite") — `ScannerPanel` widget tests (initial state, mode switching, scan flow, go-to-address signal, double-click edit, copy address, result formatting/preview, filter toggles, theme apply, results-replaced, update/rescan columns). The **engine** behaviors here are fully covered by `test_scanner` + `test_scanner_combinations`; only the panel chrome is unique and it's disabled. Port the panel as UI-layer work without resurrecting this hanging test verbatim.
- **grab_tabs.cpp** (G, NOT registered, `GrabTabs::grab()`) — screenshot helper that renders the dock-tab chrome to a PNG. Tooling, not a test.
- **test_pixels.py** (Python + PIL, NOT registered) — scans an app screenshot for a Fusion-outline color leak `(23,23,23)` at the workspace→editor seam. Tooling/visual QA, not a Rust test target.

---

# CROSS-CUTTING NOTES FOR THE RUST PORT

1. **Priority order for porting tests:** (1) `test_core`, `test_format`, `test_addressparser`, `test_provider`, `test_clipboard` — the data-model bedrock; (2) `test_compose`, `test_chips`, `test_overlay_null_rtti`, `test_static_fields`, `test_rtti`, `test_rtti_hint`, `test_typeinfer` — the rendering/inference engine; (3) `test_import_*`, `test_export_xml`, `test_generator`, `test_roundtrip_winsdk` — file-format fidelity; (4) `test_scanner*` — the scan engine; (5) `test_controller`/`test_context_menu`/`test_overlay_classcreate`/`test_type_*` — controller logic (extract the non-UI assertions); (6) UI/pixel tests last, re-expressed against the chosen Rust toolkit.

2. **Synthetic-buffer RTTI fixture is shared** across test_chips, test_rtti, test_rtti_hint, test_tutorial — port it once as a reusable Rust test helper (`build_synthetic_msvc_rtti()` / `build_synthetic_itanium_rtti()` + a `FakeModuleProvider` that overrides `enumerate_modules`).

3. **Provider test doubles to port:** `BufferProvider`, `NullProvider`, `SnapshotProvider` (with PageMap + permanent set + read-fallthrough + `enumerateModules` forwarding), plus test-only subclasses `RegionProvider`/`SyntheticProvider` (override `enumerate_regions`) and `FakeModuleProvider` (override `enumerate_modules`). These are the only providers in scope.

4. **Exact-string contracts** must be reproduced byte-for-byte: `fmtFloat` decimal ladder, disasm mnemonics, `nullptr`/`source▾` labels, struct footer `};`, fold/column constants (`kFoldCol=3`, `kTreeIndent=3`, `kColType=14`, `kColName=22`, `kSepWidth=1`, `kCompactTypeW=22`), middle-dot continuation `  · `.

5. **Data fixtures to vendor into the Rust workspace** (`src/examples/`): `windows-x86_64.h` (WinSDK header for `test_roundtrip_winsdk`), `EPROCESS.rcx`, `MMPFN.rcx` (compose compact-columns), `Vergilius_25H2.rcx`, `WinSDK.rcx`, `LargeClass.rcx` (benches). `ntkrnlmp.pdb` is NOT vendored (Windows symbol cache, optional/skip).

6. **Headless vs GUI split is explicit in CMake** — the headless tier (`Qt::Core` only) is exactly the set of pure-logic tests; mirror that split in Rust (lib unit tests vs feature-gated `ui` integration tests).

7. **Data-driven tests** (`QTest::addColumn`/`newRow` in test_scanner_combinations and parts of test_scanner) map to Rust parameterized tests (e.g. `rstest` cases or table-driven loops).
