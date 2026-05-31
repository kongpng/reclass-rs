# Editor Surface (QScintilla text grid) — `src/editor.cpp` / `src/editor.h`

**Subsystem key:** `editor-surface`  ·  **Portability:** ui-heavy
**Source size:** `editor.cpp` = 7969 lines, `editor.h` = 551 lines.
**Tests:** `tests/test_editor.cpp` (~3531 lines), `tests/test_rendered_view.cpp`. (Byte-selection-specific test files named in the task hint are not present in this checkout; byte behavior is still exercised through the public `setByteSelection`/`byteSelection`/`clearByteSelection` API.)

> This is the single most complex UI file in Reclass. It renders the structured-binary tree (produced by `compose()` in `compose.cpp`) as **formatted monospace plain text** in a read-only QScintilla widget, then layers on top: inline editing, tab-cycling between editable targets, node multi-select, per-byte hex selection + interpretation, find bar, fold markers (text-glyph based), hex/ASCII columns, per-byte heatmap highlighting, tail chips, hover previews, and a presentation-mode focus glow. The Rust port reimplements this in **GPUI** as a virtualized list of styled-text rows with interactive inline-editable regions + hit-testing. **There is no QScintilla equivalent — every `SCI_*` call must be re-expressed as explicit row/column geometry, indicator-as-styled-span, and marker-as-row-background.**

---

## 1. Purpose & Architectural Role

`RcxEditor` is a `QWidget` wrapping a single `QsciScintilla` instance. The data model lives elsewhere (`controller.cpp`, `core.h::NodeTree`); the editor:

1. Receives a fully-composed document via `applyDocument(ComposeResult)` — text + per-line metadata.
2. Renders that text into Scintilla and decorates it (markers, indicators, margins).
3. Translates **mouse/keyboard input into semantic signals** (Qt signals) the controller acts on. The editor itself **never mutates the NodeTree**; it emits `inlineEditCommitted`, `quickTypeChangeRequested`, `nodeClicked`, etc. and the controller recomposes, then calls `applyDocument` again.
4. Owns purely-visual state: selection overlays, hover, byte selection, inline edit buffer, find highlights, popups.

The editor is **stateless w.r.t. the data model** except for the cached `m_meta` (LineMeta vector) and `m_layout` snapshot from the last `applyDocument`. All node mutations round-trip through the controller.

### Coordinate systems (critical for the port)
- **Display column** = char index within a line's text (NOT byte offset). Multi-byte glyphs exist: fold arrows `▸`(U+25B8)/`▾`(U+25BE), tree connectors `├ │ └` , middle dot `·`(U+00B7), `→`(U+2192). Scintilla works in **UTF-8 bytes**; the code constantly converts via `SCI_FINDCOLUMN` (col→byte pos) and `SCI_GETCOLUMN` (byte pos→col). In Rust, store text as a Rust `String`/`Rope`; convert via char-index ↔ byte-index helpers, or work in char indices throughout and only convert at the GPUI text-shaping boundary.
- **Line geometry**: every line is `[fold prefix (kFoldCol=3, or 0 flush-left)][indent depth*kTreeIndent(=2)][type col (effectiveTypeW)][sep(1)][name col (effectiveNameW)][sep(1)][value/comment...]`. The header `LineGeometry::forLine()` (core.h:1171) is the single source of truth for column math.

---

## 2. Key Types & Constants (from `core.h`, consumed by editor)

### `enum class NodeKind : uint8_t` (core.h:24)
`Hex8,Hex16,Hex32,Hex64,Hex128, Int8..Int128, UInt8..UInt128, Float16,Float,Double,Bool, Pointer32,Pointer64, FuncPtr32,FuncPtr64, Vec2,Vec3,Vec4,Mat4x4, UTF8,UTF16, Struct,Array`. Backed by `kKindMeta[]` table (core.h:64) giving `name` (JSON "Hex64"), `typeName` (display "hex64"), `size` (bytes; 0 for Struct/Array), `lines`, `align`, `flags`. Helpers used by editor: `sizeForKind`, `isHexNode`/`isHexPreview` (Hex8..Hex128), `isFuncPtr`, `isVectorKind`, `isMatrixKind`, `kindMeta`. **Rust:** a `#[repr(u8)]` enum + a `const` metadata table indexed by discriminant.

### `struct LineMeta` (core.h:994) — per-display-line metadata. ONE entry per text line.
Fields the editor reads heavily:
- `nodeIdx:int` (-1 = synthetic), `nodeId:u64` (0=none, `UINT64_MAX`=`kCommandRowId`), `subLine:int`, `depth:int`.
- `foldLevel:int`, `foldHead:bool`, `foldCollapsed:bool`, `isContinuation:bool`, `isRootHeader:bool`, `isArrayHeader:bool`.
- `lineKind: LineKind {CommandRow, Blank(unused), Header, Field, Continuation, Footer, ArrayElementSeparator}`.
- `nodeKind:NodeKind`, `elementKind:NodeKind` (array elem type), `arrayViewIdx`, `arrayCount`, `arrayElementIdx` (-1 if not an element row).
- `offsetText:QString` (the left-margin text), `offsetAddr:u64` (absolute address — **drives byte selection + heat + relative offsets**), `ptrBase:u64` (pointer-expansion base for RVA), `parentAddr:u64` (precomputed enclosing-container address).
- `markerMask:u32` (bitmask of `M_*` markers to apply, except CommandRow handled separately).
- `dataChanged:bool`, `heatLevel:int` (0=static,1=cold,2=warm,3=hot), `changedByteIndices:QVector<int>` (which byte indices flipped this tick — for per-byte heat), `lineByteCount:int`.
- `effectiveTypeW:int` (=14 default), `effectiveNameW:int` (=22 default) — per-line column widths (compact mode / scope override).
- `pointerTargetName:QString` (resolved struct/class for a typed pointer; empty="void"/untyped).
- `isArrayElement:bool`, `isMemberLine:bool` (enum/bitfield member rows), `isStaticLine:bool`.
- `braceCol:int` (-1 none; column of trailing `{` for dimming, avoids char scan).
- `chips:QVector<LineChip>` — unified tail annotations (see below).

### `struct LineChip` (core.h:982)
`kind:ChipKind`, `startCol:int`, `endCol:int` (exclusive for hover, treated inclusive on click), `text:QString` (includes prefix glyph), payload: `rttiVtableAddr:u64`, `typeHintKinds:QVector<NodeKind>`, `enumCurrentValue:i64`, `enumRefNodeId:u64`. `findChip(lm, kind)` returns first matching chip.
`enum class ChipKind : u8 { Enum=0, TypeHint, Rtti, Symbol, Comment, AddComment }` (core.h:971). Render order in compose: Enum, TypeHint, Rtti, Comment, AddComment.

### `struct ColumnSpan` (core.h:1119): `{int start (inclusive); int end (exclusive); bool valid}`. The fundamental "where is this editable region" type. Many free functions in core.h compute spans (all are pure given a LineMeta + lineText) — the editor delegates to them:
- `typeSpanFor`, `nameSpanFor`, `valueSpanFor`, `commentSpanFor` (core.h:1183–1280) — column-arithmetic based, only depend on LineMeta widths. **Hex rows special-case the value width to 23 chars** (`isHex ? 23 : kColValue(96)`) — 8 bytes × "XX " minus trailing = covers byte 0..7. The name column for hex rows holds the ASCII preview (same width).
- `memberNameSpanFor`/`memberValueSpanFor`/`staticExprSpanFor` — text-scan based (locate `" = "`, `"return "`, `"→"`).
- CommandRow spans (core.h:1285+): `commandRowSrcSpan` (label up to `▾`), `commandRowAddrSpan` (after `▾`, handles `0x…`/`<formula>`/`[ptr]`), `commandRowRootTypeSpan`/`commandRowRootNameSpan` (locate `struct/class/enum ` keyword + name before ` {`), `commandRowChevronSpan` (`[▸]` at col 0).
- Array spans: `arrayElemTypeSpanFor` (ind→`[`), `arrayElemCountSpanFor`/`arrayElemCountClickSpanFor` (between `[ ]`), `arrayPrev/Index/Count/NextSpanFor` (the `<idx/count>` nav `< / >`).
- `pointerTargetSpanFor` (ind→`*`), `pointerKindSpanFor` (always invalid — "Type*" format has no separate kind span).
- `headerNameSpan`/`headerTypeNameSpan`/`arrayHeaderTypeSpan` — static helpers **local to editor.cpp** (lines 4638–4716) for header-line type/name; reject anonymous `struct/union/class` keywords and `[N]` array-element names; handle the `static ` prefix.

### `enum class EditTarget` (core.h:1125)
`Name, Type, Value, BaseAddress, Source, ArrayIndex, ArrayCount, ArrayElementType, ArrayElementCount, PointerTarget, RootClassType, RootClassName, TypeSelector, StaticExpr, Comment`. Drives which span is editable and whether editing is inline-text vs popup-picker.

### Column constants (core.h:1130): `kFoldCol=3, kTreeIndent=2, kColType=14, kColName=22, kColValue=96, kColComment=28, kColBaseAddr=12, kSepWidth=1, kMinTypeW=9, kMaxTypeW=128, kMinNameW=10, kMaxNameW=128, kCompactTypeW=20`.

### `enum Marker:int` (core.h:182): `M_CONT=0, M_PTR0=2, M_CYCLE=3, M_ERR=4, M_STRUCT_BG=5, M_HOVER=6, M_SELECTED=7, M_CMD_ROW=8, M_ACCENT=9, M_FOCUS=10`. These are **whole-line background/symbol markers**. In GPUI → per-row background color or left accent bar.

### `struct ViewState` (core.h:1456): `scrollLine, cursorLine, cursorCol, xOffset, cursorNodeId, cursorSubLine`. Saved/restored across refreshes; **node-id anchored** so layout shifts keep caret on same node.

### `struct ComposeResult` (core.h:1064): `text:QString`, `meta:QVector<LineMeta>`, `layout:LayoutInfo`, `maxLineLen:int` (longest line ignoring trailing spaces → scroll width), `lineStarts:QVector<int>` (char offset of each line start → O(1) line slicing).

### `struct LayoutInfo` (core.h:1054): `typeW(14), nameW(22), offsetHexDigits(8), baseAddress:u64, treeLines:bool`.

### `struct ValueHistory` (core.h:867): 10-entry ring of `(QString value, qint64 msec)`, `count`, `head`. `heatLevel()`: 0 if ≤1 unique, 1 if 2, 2 if 3–4, 3 if 5+. `uniqueCount()=min(count,10)`. `forEach`(oldest→newest), `forEachWithTime`(newest→oldest), `last()`. The controller owns the map `QHash<u64,ValueHistory>`; editor holds a const ref (`m_valueHistory`).

### Selection-ID bit encoding (core.h:933+) — used by `applySelectionOverlay`/hover:
- `kCommandRowId = UINT64_MAX`, `kFooterIdBit = 0x8000…0000` (footer row selection), `kArrayElemBit = 0x4000…`, `kArrayElemShift=42`, 20-bit element index; `makeArrayElemSelId(nodeId, idx)` / `arrayElemIdxFromSelId`. `kMemberBit = 0x2000…`, `makeMemberSelId(nodeId, subLine)` / `memberSubFromSelId`. The editor masks these off to recover the bare nodeId: `selId & ~(kFooterIdBit|kArrayElemBit|kArrayElemMask|kMemberBit|kMemberSubMask)`.

### `Provider` (providers/provider.h) methods the editor calls (read-only abstract trait — **in scope only via Provider abstraction; the live plugins are OUT of scope**): `read(addr, buf, len)->bool`, `readU32(addr)->u32`, `readU64(addr)->u64`, `readBytes(addr,len)->QByteArray`, `isReadable(addr,len)->bool`, `isWritable()->bool`, `isValid()->bool`. Held as `m_disasmProvider` (active/snapshot), `m_disasmRealProv` (real process — for arbitrary code/data reads), and `m_disasmTree:const NodeTree*` (set via `setProviderRef`).

---

## 3. Scintilla Indicator Vocabulary (`editor.cpp:1712–1761`)

Scintilla **indicators** = overlapping styled spans (foreground recolor, box fill, underline) on byte ranges; **markers** = whole-line decorations. The Rust port maps indicators → per-character style attributes accumulated per row, markers → row background/accent. Indicator IDs (style in parens):

| ID | Name | Scintilla style | Meaning |
|---|---|---|---|
| 8 | IND_EDITABLE | HIDDEN(5) | invisible; marks editable token ranges (queried, never drawn) |
| 9 | IND_HEX_DIM | TEXTFORE(17) `theme.textFaint` | dims hex bytes, fold arrows, braces, footer text |
| 10 | IND_BASE_ADDR | TEXTFORE `theme.text` | overrides lexer-green on command-row address |
| 11 | IND_HOVER_SPAN | TEXTFORE `theme.indHoverSpan` | link-like blue on hovered editable token |
| 12 | IND_CMD_PILL | STRAIGHTBOX(8) α100 under | rounded chip bg behind command-row + footer pills |
| 13 | IND_HEAT_COLD | TEXTFORE `theme.indHeatCold` | heat level 1 |
| 14 | IND_CLASS_NAME | TEXTFORE `theme.syntaxType` | teal root class name |
| 15 | IND_HINT_GREEN | TEXTFORE `theme.indHintGreen` | green comment/symbol/AddComment + live edit hint |
| 16 | IND_LOCAL_OFF | TEXTFORE `theme.textFaint` | dim inline local offset (relative mode) |
| 17 | IND_HEAT_WARM | TEXTFORE | heat 2 |
| 18 | IND_HEAT_HOT | TEXTFORE | heat 3 |
| 19 | IND_FIND | COMPOSITIONTHICK(14) under | search-match underline |
| 20 | IND_TYPE_HINT | TEXTFORE `theme.textDim` | type-inference chip text |
| 21 | IND_RTTI_HINT | TEXTFORE `theme.indRttiHint` | amber RTTI chip text |
| 22 | IND_CHIP_BG | STRAIGHTBOX α100 under | pill bg under every chip (exact span, no pad) |
| 23 | IND_CHIP_HOVER | STRAIGHTBOX α130 under `theme.hover` | brighten chip cursor is over |
| 24 | IND_CHIP_PRESSED | STRAIGHTBOX α200 under `theme.selected` | sink chip while mouse held |
| 25 | IND_TREE_CONN | TEXTFORE `theme.textDim` | tints innermost `├ │ └` connector |
| 26 | IND_BYTE_SEL | TEXTFORE `theme.indHoverSpan` | recolor selected hex byte digits |
| 27 | IND_EDIT_BOUNDS | STRAIGHTBOX α255 under `theme.selected` | bg fill over byte-range edit zone |

Higher slot wins on overlap (Scintilla composites by index); byte-sel (26) and edit-bounds (27) intentionally outrank heat/dim. **In the Rust port, model these as a per-row vector of styled ranges with explicit z-order = the ID number.**

---

## 4. Construction & Scintilla Setup

### `RcxEditor(QWidget* parent)` (1771)
- Builds: VBox layout with `m_sci` (QsciScintilla) + a hidden **find bar** container (`m_findBarContainer`): prev `◀`, next `▶`, close `✕` toolbuttons + `m_findBar` QLineEdit ("Find...").
- Calls `setupScintilla/Lexer/Margins/Folding/Markers/allocateMarginStyles`, then `applyTheme`.
- Installs `this` as event filter on `m_sci` **and** `m_sci->viewport()`; sets viewport mouse tracking.
- **Find logic** (`doFind` lambda, 1823): two passes. Pass 1 paints `IND_FIND` at **every** match (full-doc `SCI_SEARCHINTARGET` loop). Pass 2 advances cursor to next/prev match from `m_findPos`, wrapping. Selection is disabled in this Scintilla, so search visualization is indicator-only. `textChanged` resets `m_findPos=0` and re-searches; Return / next/prev buttons step; close button → `hideFindBar`; Escape (via `QAction` WidgetShortcut) hides.
- Connects vertical scrollbar `valueChanged` → recompute hover under cursor (`hitTest`, update `m_hoveredNodeId/Line`, `applyHoverHighlight` + `applyHoverCursor`) so hover tracks scroll.
- `marginClicked` → re-emits `marginClicked` signal.
- **Custom context menu** (`customContextMenuRequested`, 1910):
  1. If `m_byteSel` active AND click lands in a hex value column → byte-ops menu: "Break into new class", "Copy as hex"(Ctrl+C), "Copy as C array", "Copy as Python bytes", "Save as binary file…", "Paste hex"(Ctrl+V), "Edit hex…", "Zero-fill"(Del), "Clear selection"(Esc). Emits the matching `byte*Requested` signal or calls `beginByteEdit`/resets selection.
  2. Else if click X < margin-0 width → relative/absolute offset toggle menu.
  3. Else if line 0 command-row root-type keyword → struct↔class convert menu → `keywordConvertRequested`.
  4. Else → `contextMenuRequested(line, nodeIdx, subLine, globalPos)`.
- `userListActivated` (2018): when a type/array-elem/pointer-target picker list item is chosen (id==1), ends inline edit and emits `inlineEditCommitted` with the resolved address.
- `cursorPositionChanged` → `updateEditableIndicators`.
- `textChanged` (2034): during active edit, debounced (singleShot 0) `validateEditLive` (Value, non-hex), `updateExprResultPopup`, and StaticExpr autocomplete via `SCI_AUTOCSHOW`.
- `selectionChanged` → `clampEditSelection`.
- **Hover dwell timer** `m_hoverDwellTimer` (700 ms single-shot): on timeout sets `m_hoverDwellElapsed=true` and re-runs `applyHoverCursor` so preview popups appear only after rest.
- **Preview registry**: builds `HoverPreviewRegistry` and adds in order: `StructTargetPreview, DisasmPreview, HexDumpPreview, ValueHistoryPreview`. Creates `HoverPopupHost m_popupHost`; wires `setOnMouseMove` (keep popup alive while hovering it) and `setOnActiveChanged` (persist chosen preview per node-kind to `QSettings("Reclass","Reclass") "hoverPreview/kind/<KindName>"`).

### `setupScintilla` (2125)
ReadOnly, `WrapNone`, no caret line, caret width 0 (hidden), **arrow cursor by default (this is a viewer not editor)**, tab width 2, no tabs-as-indent. Line spacing: `SCI_SETEXTRAASCENT 4 / EXTRADESCENT 2` (or 1/0 in "compactRowSpacing" QSettings mode). **Native selection rendering disabled** (`SCI_SETSELFORE/BACK 0`) — selection uses markers. Scroll width set explicitly per `applyDocument`. `SCI_SETENDATLASTLINE 1`. `SC_CACHE_DOCUMENT` layout cache. Then configures all 18 indicator styles (table §3).

### `setupLexer` (2285)
`QsciLexerCPP` (C++ syntax coloring for the displayed pseudo-code). Built-in type names → keyword set 1 (`allTypeNamesForUI(stripBrackets=true)`). `setCustomTypeNames` (2301) puts struct type names into keyword set 3 → `GlobalClass` coloring. Brace matching off. **Rust:** there is no real lexer; the displayed text *resembles* C but coloring is mostly indicator-driven. Port can use a lightweight token classifier for keyword/number/string/type, but most color decisions come from indicators, not the lexer.

### `setupMargins` (2308)
Margin 0 = right-justified offset text (`TextMarginRightJustified`), sensitive (clickable), width sized to `"  00000000  "` (resized dynamically). Margin 1 = 2px symbol accent bar (selection indicator, mask = `1<<M_ACCENT`).

### `setupFolding` (2323)
Fold margin width 0 (fold UI is **text glyphs** `▸`/`▾` in the `kFoldCol` prefix, not Scintilla fold markers). Fold markers 25–31 set Invisible. Automatic fold off; lexer folding off (fold levels set manually from `LineMeta::foldLevel`). **Collapse is a model-level operation**: the editor emits `marginClicked`, controller toggles `Node` collapse + recomposes.

### `setupMarkers` (2343) / `allocateMarginStyles` (2375)
Defines each `M_*` marker's Scintilla type (Background/RightTriangle/ThreeRightArrows/FullRectangle/Invisible). Allocates 3 extended margin text styles (NORMAL/CONT dim, BRIGHT for innermost depth) via `SCI_ALLOCATEEXTENDEDSTYLES`.

### `applyTheme(const Theme&)` (2395)
Editor paper = `theme.background.darker(115)` (slightly darker than chrome). Sets text/caret colors, all indicator foreground colors, lexer token colors (keyword/number/string/comment/type/preproc), margin colors, all marker background colors, the find-bar stylesheet, and caches `m_focusGlowColor`. **Port:** a `Theme` struct → resolve all these colors at apply time.

---

## 5. `applyDocument(const ComposeResult&)` — THE render pipeline (2544)

This is the hot path; called on every refresh. Steps in order:

1. If editing active → silently `endInlineEdit()` (no commit signal — refresh already happening).
2. Set `m_applyingDocument=true` (suppresses popup dismiss from synthetic Leave events fired by setText). Save hover state.
3. `m_meta = result.meta; m_layout = result.layout`. Rebuild `m_nodeLineIndex: QHash<u64,QVector<int>>` (nodeId → display line indices) for O(1) hover/selection lookup.
4. Resize margin 0: relative mode uses `max(offsetHexDigits/2, 4)` digits, else full.
5. **Diff-and-patch text update** (2582): if `m_prevText` non-empty, compute longest common **line-aligned** prefix and suffix, then if the changed middle ≤ 50% of doc, do `SCI_SETTARGETSTART/END + SCI_REPLACETARGET` of only the differing bytes (char→UTF-8 byte counting done inline to avoid allocs). Else full `setText`. **Edge case:** the suffix-walk only extends forward past `\n` when mid-line (the "footer ate everything" bug guard, 2620). If patch covers byte 0 → clear `m_lastCommandRowText` cache. `m_prevText = newText; m_lastApplyWasPatch = didPatch`.
6. Set horizontal scroll width from `result.maxLineLen` (in pixels via font metrics); reset xOffset to 0 (restoreViewState fixes it after).
7. Re-colourise only the patched byte range (or full doc on full-replace).
8. **Meta diff** (2734): if last apply was a patch, compute `[firstChanged, lastChanged]` by comparing `m_prevMeta` vs new meta with `sameLine()` — a field-by-field equality over EVERY field consumed by per-line passes (nodeId, subLine, lineKind, nodeKind, elementKind, foldLevel, markerMask, depth, foldHead, foldCollapsed, braceCol, isContinuation/RootHeader/ArrayHeader/ArrayElement/MemberLine/StaticLine, heatLevel, chips (kind/startCol/endCol/text), lineByteCount, effectiveTypeW/NameW, pointerTargetName). **The Rust port must reproduce this dirty-range computation for incremental marker updates.**
9. **Clear ALL TEXTFORE/box indicators full-doc** (2800) — narrowing this broke in production (Scintilla edge case), so indicators are always cleared+rebuilt full-doc (cheap, microseconds). Reset chip hover/pressed bookkeeping.
10. `applyLineAttributes(meta, firstChanged, lastChanged)` — **markers/margins narrowed** (the expensive part).
11. `applyHexDimming(meta, -1, -1)` — full-pass.
12. Build `lineTexts` cache via `result.lineStarts` (O(1) slices).
13. `applyHeatmapHighlight`, `applySymbolColoring` (no-op now), `applyCommandRowPills` — full-pass.
14. **Footer pill backgrounds** (2846): search footer text for ` +1 `, `+1000h`/`+100h`/`+10h`, `+10`, `Trim`, `Top` (longest-first, with collision guards) and paint `IND_CMD_PILL`.
15. **Per-chip indicators** (2893): for each chip paint `IND_CHIP_BG` over `[startCol,endCol)` exactly (no pad — pad fused adjacent pills), plus a kind-specific TEXTFORE: Comment/Symbol/AddComment→IND_HINT_GREEN, TypeHint→IND_TYPE_HINT, Rtti→IND_RTTI_HINT, Enum→IND_HOVER_SPAN.
16. Restore hover state; if hovered node deleted, clear + `dismissAllPopups`. `markerDeleteAll(M_HOVER)` (correct for both patch and full-replace), reset prev-hover, `applyHoverHighlight`.
17. Re-apply focus glow markers (setText clears markers), re-apply find indicator (re-search full doc), `applyByteSelectionOverlay` (re-paint byte selection — address-based so survives).
18. `m_prevMeta = result.meta`. Emit `documentApplied(text)` (minimap mirror).

### `applyLineAttributes` (3007)
Margin text is **forced full-pass** even on narrow updates (REPLACETARGET line-renumbering desyncs margin text storage). In relative mode → `reformatMargins(-1,-1)`; else clear margin text and set per-line `offsetText` + style bytes. Markers + fold levels are narrowed: CommandRow → `M_CMD_ROW`; else apply each `M_*` bit in `markerMask`; set `SCI_SETFOLDLEVEL` from `lm.foldLevel`.

### `reformatMargins(first,last)` (3070)
Two passes. **Pass 1**: per-line margin text. Continuation/member → `"  · "`. Footer/sep/CommandRow in relative mode → blank. Relative: `"+<hexUpper>"` right-justified to `hexDigits`, base = `ptrBase` or layout base. Absolute: zero-padded uppercase hex. **Pass 2** (skipped if `m_layout.treeLines`): inline **local offsets** in the indent area for depth>1 field/header lines — derive parent address (ptrBase / parentAddr / backward-scan for enclosing Header/ArrayElementSeparator), write `"+<hex>"` into the indent slot (needs ≥3 chars), color `IND_LOCAL_OFF`. Toggling off restores spaces. **This temporarily flips readOnly off to REPLACETARGET into the text buffer.**

### `applyHexDimming` (3235)
Dims: fold arrows on fold-head lines (cols 0..kFoldCol), entire hex preview row value, entire footer line, trailing `{` (via `braceCol`). Then `IND_TREE_CONN` tints only the innermost connector glyph (trailing `kTreeIndent` chars of the indent region) per depth>0 row.

### `applyHeatmapHighlight` (4370)
Per line: clear all 3 heat indicators, then if `heatLevel>0` pick cold/warm/hot. For **hex rows**: paint only `changedByteIndices` (each byte's 2 digits at `vs.start + b*3 .. +2`); if no bytes changed this tick, paint nothing. For non-hex: paint full (narrowed) value span. Value span narrowed via `narrowPtrValueSpan` (clip at first chip startCol).

### `applyCommandRowPills` (4486) / `applyBaseAddressColoring` (4475)
Line 0 only. Dim chevron, source label + `▾`, address; root type dim + `▾`; root **name** teal (IND_CLASS_NAME); dim trailing `{`.

---

## 6. View State, Navigation, Presentation Mode

- `saveViewState()` (4011): firstVisibleLine, cursor line/col, xOffset, and `cursorNodeId`/`cursorSubLine` from the line's LineMeta.
- `restoreViewState(vs)` (4029): prefer node-id-anchored restore (find line owning same nodeId+subLine), else clamp saved coords. During active scroll animation keep the animation's position. Clamp xOffset to current content width.
- `scrollToNodeId` (4144): first non-footer line for nodeId → setCursor + ensureLineVisible.
- `smoothScrollToNodeId` (4156): presentation-mode smooth scroll (`QVariantAnimation`, OutExpo, 400 ms, center node; snap-then-animate if >50 lines away). Non-presentation falls back to instant.
- `setFocusNode`/`clearFocusNode` (4227/4272): `M_FOCUS` marker glow on all lines of a node, pulsing via 30 ms timer (sine blend between pre-blended dim/bright opaque colors — Scintilla ignores alpha). `isFocusGlowActive()` = `m_focusNodeId != 0`.
- `metaForLine`, `currentNodeIndex` (cursor line → nodeIdx).

---

## 7. Selection & Hover Overlays

### `applySelectionOverlay(QSet<u64> selIds)` (3282)
Skip if unchanged AND last apply was patch. Clear `M_SELECTED`/`M_ACCENT` + `IND_EDITABLE` full-doc. For each selId: decode footer/array-elem/member bits + bare nodeId, look up lines, **match line type to selection type** (footer-sel only on footer lines, array-elem-sel only on matching element index, member-sel only on matching subLine). Add `M_SELECTED`+`M_ACCENT`; non-footer rows also get `paintEditableSpans`. Then `applyHoverHighlight` + `applyHoverCursor`; dismiss arrow tooltip only when selection actually changed (avoids refresh-tick flicker).

### `selectedNodeIndices()` (4319)
From Scintilla's text selection range (lineFrom..lineTo) → set of `nodeIdx`. If no selection, the cursor line's node. (Used by controller for multi-select ops.)

### `applyHoverHighlight()` (3942)
No-op if `m_hoverEffects` off or hover unchanged. Removes old `M_HOVER` (single-line for footer/array-elem/member, else all lines of node via index). Skip if editing or not inside or no hover. Computes the **checkId** with the right bit (footer/array-elem/member) and skips if already selected. Adds `M_HOVER` single-line (footer/elem/member) or all non-footer/non-elem lines of the node.

### `applyHoverCursor()` (7221) — the big hover function
Clears `IND_HOVER_SPAN` lines. Branches:
- Drag in progress → Arrow, return.
- Hover effects off (non-edit) → dismiss popups, Arrow.
- **Edit mode**: IBeam inside edit span else Arrow; if list active → Arrow. **Value-history popup** (`m_historyPopup`, ValueHistoryPopup **with "Set" buttons**) shown when editing a Value on a heated node with >1 unique value; positioned at value column. Always dismiss the hover host during edit.
- Mouse outside viewport → Arrow, dismiss popups, reset dwell.
- List active → Arrow.
- Else: `hitTest` + `hitTestTarget`. Paint `IND_HOVER_SPAN` over the hovered editable token (with **vector/matrix component narrowing** and **pointer address-only narrowing**). Paint hover span on fold arrows, footer pills.
- **Dwell tracking**: restart 700 ms timer when (nodeId,line) target changes.
- **Unified hover preview host** (7487): if dwell elapsed + provider/tree set + cursor in (full∪narrow) value span → build `HoverContext`, query `eligibleFor`, pick last-used preview, `host->setEligible` + `showAt`.
- **Cursor shape**: fold col→PointingHand; footer pill→PointingHand; editable token→PointingHand (Type/Source/picker targets) or IBeam (else); **hex byte → IBeam** (byteAddrAt); clickable chip hovered → PointingHand.
- **Arrow tooltips** (RcxTooltip) on command-row spans (Data Source / Base Address help with formula examples / Class Name / Switch View) and Ctrl-hover "Open in new tab" on navigable Header lines.

---

## 8. Hit Testing & Spans

### `hitTest(viewportPos) -> HitInfo {line,col,nodeId,inFoldCol}` (4858)
`SCI_POSITIONFROMPOINTCLOSE` → line+col; fallback computes line from Y/lineHeight when past text. `inFoldCol` = col in `[0, kFoldCol]` on a fold-head line.

### `hitTestTarget(...) -> bool` static (4888)
Maps a pixel position to `(line, col, EditTarget)`. Order: CommandRow (chevron→TypeSelector, src→Source, addr→BaseAddress, root-name→RootClassName); pointer sub-spans (target→PointerTarget); array header (count-click→ArrayElementCount, elem-type→ArrayElementType); then generic type/name/value/comment spans (with header/member fallbacks). Redirects: array-header Type→ArrayElementType (popup); array-element type/name → ArrayElementType on the parent header line; **hex nodes block Name/Value editing** (only Type editable).

### `normalizeSpan(raw, lineText, target, skipPrefixes)` (4718)
Clamps to text length, optionally strips leading `->` or `=` value prefixes (for Value targets), trims leading/trailing whitespace → tight `NormalizedSpan {start,end,valid}`.

### `resolvedSpanFor(line, target, out, lineTextOut)` (4758)
The authoritative "where is this editable thing on this line" resolver, used by editable-indicator painting, hover, and edit-begin. CommandRow→source/addr/root spans; hex nodes block Name/Value (except static); per-target span via the core.h `*SpanFor` helpers; header/member fallbacks; comment chip span or end-of-line fallback for comment creation.

### `narrowPtrValueSpan(lm, vs, lineText)` (4294)
Clips a value span at the first chip's startCol (so the editable value excludes trailing RTTI/Symbol/Comment chips); legacy `"  // "` sniff fallback.

---

## 9. Event Filter — Mouse & Keyboard Dispatch (`eventFilter`, 5012)

The single most behavior-rich method. Operates on `m_sci` (key/focus) and `m_sci->viewport()` (mouse). Summary of branches:

**KeyPress on m_sci**: `Ctrl+F`→showFindBar; else dispatch to `handleEditKey` (editing) or `handleNormalKey`. On unhandled non-edit key → clear hover.

**MouseButtonPress on viewport while editing** (5028): click on edit line inside trimmed text → let Scintilla position caret; click in padding within raw span → move caret to edit end (consume); click elsewhere → `commitInlineEdit` + clear selection (consume; coords stale post-recompose).

**MouseButtonPress while NOT editing** (5075, LeftButton):
- Sync hover to click; `applyHoverHighlight`.
- **Plain LMB clears `m_byteSel`** (Shift/Ctrl preserved).
- **Shift+Click on hex byte** → extend byte selection (`byteAddrAt`, anchor=lo, half-open hi).
- Fold-col click → `marginClicked`.
- **Tail-chip click router** (5142): recompute hit col from pixel-x, walk chips (inclusive endCol on click); paint pressed overlay for clickable chips; Enum chip → `enumChipClicked(nodeIdx, enumRefNodeId, value, globalPos)`; TypeHint chip → `typeHintChipClicked(nodeIdx, kinds)`; RTTI chip intentionally inert.
- **Footer button click** (5210): ` +1 `→`appendSingleFieldRequested`; `+1000h/+100h/+10h`→`appendBytesRequested(nid, 0x1000/0x100/0x10)`; `+10`(enum)→`appendEnumMembersRequested(nid,10)`; `Trim`→`trimHexRequested`; `Top`→scroll to top.
- **CommandRow click** (5258): hitTestTarget → TypeSelector emits `typeSelectorRequested` else `beginInlineEdit`. Consumes all command-row clicks.
- **Node click** (5268): Ctrl+Click on navigable Header type/name/ptr-target → `openTypeInNewTabRequested`. Click on already-selected (plain) → `beginInlineEdit` of clicked token. Else arm drag (`m_dragging`, threshold, init mods); arm byte-select anchor if press on hex byte w/o modifiers; multi-select-with-already-selected defers click (`m_pendingClick*`) else `setCursorPosition` + emit `nodeClicked(line,nodeId,mods)`. **Consumes ALL left-clicks** (prevents Scintilla caret).

**MouseMove with drag/byte-arm active** (5347): byte-drag upgrade when moved ≥8px from a byte anchor → clears row-drag, enters `m_byteSelDragging`; byte-drag extends `m_byteSel` to hit byte (half-open). Row-drag: 8px Y threshold, flush deferred click, emit `nodeClicked` with Shift to extend selection as lines change.

**MouseButtonRelease** (5410): finalize byte drag (persist `m_byteSel`, reset anchor/flags); flush deferred click → `nodeClicked`; drop pressed-pill overlay.

**Double-click**: on offset margin → toggle relative/absolute (`reformatMargins` + `relativeOffsetsChanged`). During edit → select entire edit text. Else: **hex byte dblclick → select whole hex node bytes + `beginByteEdit`** (hex-overwrite mode); else `hitTestTarget` → `nodeClicked` + `beginInlineEdit`. Consumes even on miss.

**FocusOut on m_sci**: commit active edit on focus loss (deferred, unless PopupFocusReason or autocomplete active). Clear editable indicators.
**FocusIn**: `updateEditableIndicators`.

**Viewport tracking** (5524): ignore synthetic Leave during `m_applyingDocument`. MouseMove→track pos+inside; Leave→dismiss popups unless cursor moved onto our popup, clear hover/chip; Wheel→re-derive pos. On move/wheel (non-edit): re-derive hover nodeId/line, `updateChipHover`, `applyHoverCursor`. **Consumes MouseMove in non-edit mode** so Scintilla doesn't reset cursor to Arrow.

---

## 10. Keyboard Handling

### `handleNormalKey` (5591)
**Popup-visible first**: if `m_popupHost` visible, Tab/Backtab cycle eligible previews (>1), Esc → `dismissAllPopups`. Then a big switch:
- F2→edit Name; F12→`goToDefinitionRequested`; T→edit Type.
- Return/Enter→if byteSel `beginByteEdit` else edit Value.
- Backspace/Delete→if byteSel `byteZeroFillRequested`; Delete also `deleteSelectedRequested` if nodes selected.
- Ctrl+D→`duplicateSelectedRequested`. Ctrl+Shift+C→copy node address (or byte range "0xLO..0xHI (N bytes)"). Ctrl+C→`byteCopyHexRequested`/`copyNodesRequested`. Ctrl+X→`cutNodesRequested`. Ctrl+V→`bytePasteHexRequested`/`pasteNodesRequested`.
- Insert / Shift+Insert→`insertAboveRequested(currentNode, Hex64/Hex32)`. `;`→`commentEditRequested`.
- **Up/Down**: byteSel+Shift→`snapByteSelectionToRow`; Ctrl+Shift→`moveNodeRequested`; else navigate to next/prev real node (skip nodeId 0/CommandRow/continuation), emitting `nodeClicked`. Down past the end auto-appends a field (`appendSingleFieldRequested`).
- PageUp/Down→jump a screenful, land on nearest node.
- **Tab** (5804): cycle edit targets in order `{Name, Value, Comment, ArrayElementType, ArrayElementCount, PointerTarget, Type}`, starting after `m_lastTabTarget`, skipping inapplicable targets (array/pointer gating); first `beginInlineEdit` that succeeds sets `m_lastTabTarget`.
- **Type shortcuts**: Space/Shift+Space cycle hex sizes (Hex8/16/32/64); non-hex converts to same-size hex first. Keys 1–5→Hex8/16/32/64/128. P→Pointer (size≥4). F→Float(4)/Double(8). S→signed int of same size. U→unsigned int of same size. All emit `quickTypeChangeRequested(ni, kind)`.
- **Left/Right**: byteSel+Shift (or Ctrl+Shift)→`extendByteSelection(±1)`; byteSel+plain→absorbed; else→`cycleSameSizeTypeRequested(ni, dir)`.
- Esc→two-stage: drop byteSel first, else clear node selection (`nodeClicked(-1,0,NoModifier)`).
- Home/End→byteSel+Shift collapses-to-anchor / extends-to-doc-end; else jump first/last data node.
- Ctrl+A→byteSel `selectAllHexBytes`; on hex row creates whole-node byte sel; else select all siblings via first/shift-last `nodeClicked`.
- Ctrl+Shift+[ → `collapseAllRequested`; Ctrl+Shift+] → `expandAllRequested`.

### `handleEditKey` (6126) — non-hex inline editing
Hex/ASCII overwrite delegates to `handleHexEditKey`. Else: Return/Enter→commit; Tab→set lastTabTarget + commit; Esc→cancel; Up/Down/PageUp/Down→block (consume); Delete→block at end else allow; Left/Backspace→clamp at spanStart (Left collapses selection to left end); Right→clamp at editEnd (collapse selection right); Home/End→jump to span start/end; Ctrl+V→sanitized paste (strip `\n\r`, and backtick for BaseAddress).

### `handleHexEditKey` (6218) — hex/ASCII overwrite mode (fixed-length)
`isHexMode` = Value target (hex digits), else Name = ASCII preview. Cursor confined to `[spanStart, spanEnd)`. `replaceCharAt` helper overwrites one char + re-fills `IND_HEX_DIM` (and `IND_EDIT_BOUNDS` during byte-range edits, because REPLACETARGET strips edge indicators). Keys: Enter→commit; Esc→cancel; Tab/Up/Down/Page→block; Home/End→span ends; Left/Right→move skipping space separators, **hop to adjacent byte-edit segment** at boundaries (`advanceToByteSegment`); Backspace→reset prev char to '0'/'.' (hop segment at start); Delete→reset current; Ctrl+Z→block; Ctrl+V→paste only valid hex/printable, skipping separators. **Char input**: hex mode accepts `[0-9a-fA-F]` (uppercased), overwrites current digit, advances skipping spaces, hops to next segment at segment end; ASCII mode accepts printable 0x20–0x7E.

---

## 11. Inline Editing Lifecycle

### `InlineEditState` (editor.h:302) — the edit buffer
`active, line, nodeIdx, subLine, target, spanStart, linelenAfterReplace, original:QString, posStart/posEnd (byte positions), editKind, commentCol, lastValidationOk, hexOverwrite, padBytes/padPos/padRestoreSpaces (trailing-pad bookkeeping so endInlineEdit restores byte-identical line), byteRange/byteRangeAddr/byteRangeLen, byteSegments:QVector<ByteEditSegment>, byteSegIdx`. `ByteEditSegment {line, spanStart, spanEnd, byteCount}` — one per hex row a multi-row byte edit spans.

### `beginInlineEdit(target, line=-1, col=-1) -> bool` (6483)
- Reject `TypeSelector` (popup-only). Clear byteSel.
- **Picker targets** (Type/ArrayElementType/PointerTarget) → emit `typePickerRequested(target, nodeIdx, globalPos)` and return (popup, not inline). **Source** → emit `sourcePopupRequested(pos)`.
- Reject if already editing. Reset pad trackers, clear hover, dismiss popups, clear editable indicators.
- Resolve span: hex edit (via `m_hexEditPending` context-menu flag) computes raw hex/ASCII fixed spans; Comment finds chip span or end-of-line; else `resolvedSpanFor`. Vector/matrix Value → narrow to clicked component (`narrowToComponent`); strip comment marker prefix (`/ `, `// `, etc.).
- Populate `m_editState`. Enable undo collection, caret width 1, readOnly off.
- For Value/BaseAddress/hex-ASCII edits with a commentCol: **extend line with trailing spaces** to make room for the live comment area (kColComment=28 or 60 for BaseAddress); track padBytes/padPos for stripping.
- Comment editing: if no existing comment, trim trailing whitespace + append `"  "` placeholder (track padRestoreSpaces); else strip the `/ ` prefix.
- Set IBeam cursor (non-picker), re-enable selection rendering with a neutral tint, set selection `[posStart,posEnd]`. Hex overwrite → caret at posStart. Show initial edit hint comment ("Enter=Save Esc=Cancel" / "Hex edit: …"). Value → defer `applyHoverCursor` (show history popup).

### `editEndCol()` (6841) / `editEnd()`
`spanStart + original.size() + (currentLineLen - linelenAfterReplace)` — tracks the moving end as the user types.

### `clampEditSelection()` (6849)
Re-entrancy guarded. Hex overwrite → collapse any selection to cursor (multi-row byte edit preserves the cursor's actual line). Else clamp selection ends to `[spanStart, editEnd]` on the edit line.

### `commitInlineEdit()` (6912)
Extract edited text (trim unless hex overwrite). **Byte-range branch**: parse hex digit pairs (single-row from editedText, multi-row by reading each segment's line slice), `endInlineEdit`, emit `byteRangeCommitRequested(addr, rawBytes)`. Else: Type edit empty → keep original; capture address; `endInlineEdit`; emit `inlineEditCommitted(nodeIdx, subLine, target, text, addr)`.

### `cancelInlineEdit()` (6992): `endInlineEdit` + emit `inlineEditCancelled`.

### `endInlineEdit() -> EndEditInfo {nodeIdx,subLine,target}` (4544)
Shared shutdown: cancel autocomplete, hide expr-result label, clear edit comment + M_ERR, **restore trailing padding** (Value/BaseAddress: strip last padBytes chars; Comment: restore from padPos with padRestoreSpaces), **clear `m_prevText`** (forces full-replace next refresh — inline edits diverge Scintilla from prevText), deactivate, clear byte-range state + `IND_EDIT_BOUNDS`, readOnly on, caret width 0, arrow cursor, disable selection rendering, reset undo collection + empty undo buffer.

### `setEditComment(comment)` (7744)
Writes a `//<comment>` hint into the comment column area during edit (green IND_HINT_GREEN). Re-entrancy guarded (`m_updatingComment`).

### `validateEditLive()` (7782)
On Value/BaseAddress edits: validate via `fmt::validateValue`/`validateBaseAddress`; toggle M_ERR background + update hint comment ("Enter=Save Esc=Cancel" or "! <error>") only on state change.

### `updateExprResultPopup()` (7814)
For BaseAddress / Value edits containing an operator (`+ - * / << & | ^ ~`): evaluate via `m_exprEvaluator` callback, show a floating result label above the edit line. (Evaluator injected by controller.)

### Type / pointer-target pickers (7001–7138)
`showTypeAutocomplete`/`showTypeListFiltered`/`updateTypeListFilter` and pointer-target equivalents use Scintilla `SCI_USERLISTSHOW` (id=1). `showTypeListFiltered` merges `allTypeNamesForUI()` + `m_customTypeNames`, sorted, prefix-filtered. Pointer-target list = "void" + custom struct names. **These are popup pickers, not inline text — the Rust port replaces with a GPUI filtered list overlay.**

---

## 12. Byte Selection (hex preview rows)

`m_byteSel: Option<(u64 lo, u64 hi)>` — **half-open absolute-address range**, address-based so it survives refresh/tab-switch and naturally spans multiple hex rows. Public API: `byteSelection()`, `clearByteSelection()`, `setByteSelection(lo,hi)` (rejects `hi<=lo`). Drag state: `m_byteSelAnchor: Option<u64>`, `m_byteSelDragging:bool`.

- `byteAddrAt(line,col) -> Option<u64>` (3447): only valid on hex preview field rows in the value column; `byteIdx = (col - vs.start)/3` (each byte = "XX " = 3 chars), bounds-checked to `[0,size)`; returns `offsetAddr + byteIdx`.
- `applyByteSelectionOverlay()` (3462): clear `IND_BYTE_SEL` full-doc; for each hex row overlapping `m_byteSel`, paint digits of intersected bytes (`vs.start + firstByte*3 .. vs.start + (lastByte-1)*3 + 2`). Tail-calls `updateByteSelStatus`.
- `updateByteSelStatus()` (3519): read selection bytes from provider, build a status line: address + size + multi-format interpretations (n==1: u8/i8/char; n==2: u16/i16 LE+BE; n==3: 24-bit LE/BE/RGB/ASCII; n==4: u32/i32/f32 LE+BE; n==8: u64/i64/f64 LE+BE; n=5–7: packed ints + MAC for n=6; ≥9: first-12-bytes hex preview). Emits `statusHintRequested`. (read-only) appended when provider not writable.
- `beginByteEdit()` (3660): build one `ByteEditSegment` per overlapped hex row; reset `m_byteSel`; `beginInlineEdit(Value, firstSeg.line)` with `m_hexEditPending`; narrow to first segment, set `byteRange*`, paint `IND_EDIT_BOUNDS` over all segments. (Cross-row selections allowed via segments; commit walks segments.)
- `advanceToByteSegment(delta)` (3739): hop the active edit to the prev/next segment (updates spanStart/original/posStart/posEnd, places caret at segment edge). Keeps `m_editState.line` on the FIRST segment (padding bookkeeping lives there).
- `extendByteSelection(dByte)` (3772): grow/shrink `hi` (lo fixed); clamp grow to highest hex byte's end-address; shrink clamps at `lo+1`.
- `snapByteSelectionToRow(dir)` (3803): Shift+Down/Up row-aware — snap `hi` to current row end, then walk to next/prev hex row's end; stops at first non-hex row; keeps ≥1 byte.
- `selectAllHexBytes()` (3880): union [min lo, max hi] across all hex preview rows.

---

## 13. Hover Preview System (popup host + registry)

File-local classes in editor.cpp (forward-declared `HoverPreviewRegistry` in editor.h):
- `HoverPopup` (55): base QFrame tooltip; `showAt(globalPos, lineHeight)` with screen-edge constraint (shrink width, never shift left so popup stays anchored to value column); `dismiss`.
- `TitleBodyPopup`, `ValueHistoryPopup` (130/195): legacy popups; `ValueHistoryPopup` (with optional "Set" buttons) is still used during **inline value editing** (`m_historyPopup`). Rows = previous values + relative timestamps; "Set" button writes the value into the edit via `setOnSet`.
- `HoverPopupHost` (1218): the unified host. Fixed size via `computeHostStandardSize` (kHostCols=64, kHostRows=8 × font metrics). Title (kind · preview name), `PreviewDotsStrip` (●/○ click-to-switch), `ActiveSizeStack` (constant-size QStackedWidget), footer ("Tab cycle / Esc close all"). `setEligible(eligible, lm, node, ctx, activeIdx)` with a fingerprint (nodeId + history.count + eligible ids) to skip rebuilds; `setActiveIndex`/`cycleNext`/`cyclePrev`; emits `onActiveChanged` (persists choice).
- `HoverPreview` trait (hover_preview.h): `id()`, `tabLabel()`, `subtitle(lm)`, `eligible(lm,node,ctx)`, `widget(lm,node,ctx,parent)`. `HoverContext` carries fonts, theme, dataProvider/codeProvider, tree, history.
- Concrete previews (1489–1710): **ValueHistoryPreview** (Field, non-container/funcptr, heat>0, >1 unique value), **HexDumpPreview** (untyped pointer with non-null target → 128-byte hex dump capped 6 lines), **DisasmPreview** (funcptr → disassemble target), **StructTargetPreview** (typed collapsed pointer → mini composed struct excerpt, homogeneous-row dedup with `[N × type]` header). Helpers: `readPointerAtRow`, `capLines`, `buildTextBodyWidget`, `buildValueHistoryBody`.

`pickLastUsedPreviewIdx` (4120) reads the persisted choice from QSettings. **Port note:** the Rust port reimplements previews as GPUI overlays; the trait + registry + dwell + Tab-cycling + per-kind persistence are the portable contract.

---

## 14. Signals (public API surface, editor.h:120–219)

Mouse/edit signals the controller consumes (all are pure notifications; the editor never mutates the tree):
`marginClicked, contextMenuRequested, keywordConvertRequested, nodeClicked, inlineEditCommitted(nodeIdx,subLine,target,text,resolvedAddr), inlineEditCancelled, typeSelectorRequested, typePickerRequested, sourcePopupRequested, insertAboveRequested, commentEditRequested, relativeOffsetsChanged, rttiChipClicked, rttiNullChipClicked, enumChipClicked, typeHintChipClicked, appendBytesRequested, trimHexRequested, appendEnumMembersRequested, appendSingleFieldRequested, deleteSelectedRequested, duplicateSelectedRequested, copyNodesRequested, cutNodesRequested, pasteNodesRequested, documentApplied(text), quickTypeChangeRequested, cycleSameSizeTypeRequested(dir), moveNodeRequested(dir), collapseAllRequested, expandAllRequested, goToDefinitionRequested, openTypeInNewTabRequested, byteCopyHexRequested, bytePasteHexRequested, byteZeroFillRequested, byteCopyAsCArrayRequested, byteCopyAsPythonRequested, byteSaveAsFileRequested, byteBreakIntoClassRequested(lo,hi), byteRangeCommitRequested(addr,bytes), statusHintRequested(text)`.

Setters/injectors: `setValueHistoryRef`, `setExprEvaluator`, `setProviderRef`, `setRelativeOffsets`, `setSavedSources`, `setCustomTypeNames`, `setStaticCompletions`, `setHexEditPending`, `setHoverEffects`, `setPresentationMode`, `setEditorFont`/`setGlobalFontName`/`globalFontName`, `applyTheme`. Queries: `historyPopup`, `hoverPopup`, `hoverPopupActiveId`, `metaForLine`, `currentNodeIndex`, `isFocusGlowActive`, `selectedNodeIndices`, `isEditing`, `editSpanStart`, `editEnd`, `textWithMargins`. Static span helpers: `typeSpan`/`nameSpan`/`valueSpan`.

---

## 15. Concurrency / Threading
**None.** Everything runs on the Qt GUI thread. Async is limited to `QTimer::singleShot(0, ...)` deferrals (validation, expr popup, focus-out commit, hover-cursor refresh) and timers (hover dwell 700 ms, focus glow 30 ms, scroll animation 400 ms). The Rust/GPUI port stays single-threaded on the UI; defer with GPUI's `cx.spawn`/`defer` equivalents.

## 16. Platform-Specific Code
Only one `#if QT_VERSION >= 6.0.0` guard (5451) selecting `me->position()` vs `me->pos()` for double-click margin X. No OS-specific code in this file. **`ui-heavy` portability: the logic (spans, hit-testing, edit lifecycle, byte selection, key dispatch) is fully portable; only the QScintilla rendering substrate must be replaced.**

## 17. Subtle behaviors the tests / parity rely on
1. **CommandRow (line 0) rejects Type/Name/Value inline edits** (only BaseAddress/Source/RootClassName editable) — test_editor:471.
2. **Hex nodes block Name/Value editing** except static-field names; only Type editable. Double-click/Enter on a hex byte enters **hex-overwrite mode** instead.
3. **Footer lines reject all inline edits** (test_editor:642).
4. **Header lines**: Type opens picker, Name editable; anonymous struct/union/class keywords and `[N]` element names not editable.
5. **Inline edit forces full-replace next refresh** (clears m_prevText) and **restores trailing padding byte-identically** — required so the diff/patch path doesn't corrupt unrelated lines.
6. **Selection survives layout shifts** via node-id anchoring in save/restoreViewState.
7. **Byte selection is address-based**, half-open, survives refresh; `setByteSelection` enforces `lo<hi`.
8. **Tab cycles edit targets** in fixed order, skipping inapplicable; `m_lastTabTarget` persists across begins.
9. **Live value validation** toggles a red error background + comment hint only on state change.
10. **Per-byte heat**: only `changedByteIndices` get colored, not the whole hex run.
11. **Hover preview dwell** = 700 ms; arrow tooltips are NOT dwell-gated (instant discoverability).
12. **Find** highlights every match (indicator) and steps the cursor; highlights + position persist across hide/reopen.
13. **Cursor shapes**: PointingHand for fold toggle / footer pills / type-pickers / clickable chips / Ctrl-nav; IBeam for editable value text + hex bytes; Arrow over padding.

---

## 18. Rust / GPUI Mapping Cheatsheet
- `QsciScintilla` text buffer → a `Rope`/`String` of the composed text, sliced per-line via `lineStarts`. Display in a virtualized GPUI list of styled rows.
- **Indicators** → per-row `Vec<StyledRange { char_range, style, z=indicator_id }>`; composite by ascending z. `SCI_FINDCOLUMN`/`SCI_GETCOLUMN` → char↔byte helpers (or stay in char indices and convert only at shaping).
- **Markers** (`M_*`) → per-row background fill / left accent bar.
- **Margin text** → a fixed-width gutter element painting `LineMeta::offsetText`.
- `SCI_*` geometry calls (`POSITIONFROMPOINTCLOSE`, `POINTXFROMPOSITION`, `TEXTHEIGHT`, `LINESONSCREEN`) → GPUI line-layout queries (line height × index, char-x via shaped run).
- `QVariantAnimation` (smooth scroll), `QTimer` (dwell/glow) → GPUI animations / `cx.spawn` timers.
- `QSettings` persistence (compactRowSpacing, hoverPreview/kind/*) → app config store.
- Popups (`HoverPopupHost`, RcxTooltip, pickers, value-history) → GPUI overlay views; preview registry trait stays as a Rust trait object set.
- Editor emits **signals**; in Rust use an event/callback channel back to the controller. Editor must NEVER mutate the model directly.

### Recommended Rust crates
- **gpui** (rendering, layout, input, virtualized list, overlays, animations).
- **ropey** (efficient text buffer + char/byte index conversions) — optional; a `String` + `lineStarts` works for the typical doc sizes.
- **unicode-segmentation** / **unicode-width** (correct char/grapheme handling for the multi-byte glyphs ▸ ▾ ├ │ └ · →).
- **smallvec** (per-line styled-range vectors), **bitflags** (KindFlags / marker masks), **memchr** (find-bar substring search over UTF-8).
