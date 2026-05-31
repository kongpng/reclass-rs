# PORTING SPEC — Editor surface (QScintilla text grid)

**Subsystem key:** `editor-surface`
**C++ source:** `src/editor.cpp` (7969 lines) + `src/editor.h` (551 lines); consumes span helpers from `src/core.h:1119–1438`.
**Behavioral map:** `_design/understand/editor-surface.md` (read it first — this spec assumes its terminology).
**Covering C++ tests:** `tests/test_editor.cpp` (~70 slots), `tests/test_rendered_view.cpp` (13 slots). Chip-data tests (`tests/test_chips.cpp`) belong to the `compose` spec, not here.
**Target Rust:** package `reclass`, module `src/ui/editor/` (feature `ui`, default-on). Pure-logic span/edit-lifecycle code is feature-independent and lives in `src/ui/editor/geometry.rs` + `src/ui/editor/edit_state.rs`, compiled under the always-on path so logic `#[test]`s run with `--no-default-features`. GPUI rendering lives in `src/ui/editor/element.rs` + `view.rs` (gpui-gated).

> **The cardinal rule (from the map §1):** the editor is *stateless w.r.t. the data model*. It renders a `ComposeResult` and translates input into **events** (the C++ Qt signals). It NEVER mutates the `NodeTree`. Every node change round-trips through the controller, which recomposes and calls `apply_document` again. The Rust port keeps this contract: the editor emits `EditorEvent`s on an `mpsc`/callback channel; the controller owns mutation.

---

## 0. Module layout & dependency budget

```
src/ui/editor/
├─ mod.rs          # re-exports; EditorEvent enum; EditTarget; Marker; Indicator IDs
├─ geometry.rs     # LineGeometry + all *_span_for() pure functions (port of core.h:1150–1438)
│                  #   — ALWAYS compiled, no gpui. The bulk of the testable logic.
├─ edit_state.rs   # InlineEditState, edit lifecycle (begin/commit/cancel/end), clamp,
│                  #   editEndCol, hex-overwrite cursor math, byte-edit segments.
│                  #   ALWAYS compiled (operates on a Rope + LineMeta, no gpui).
├─ byte_sel.rs     # ByteSelection (half-open u64 range), byte_addr_at, overlay paint plan,
│                  #   status-line interpretation, extend/snap/select-all. ALWAYS compiled.
├─ hit_test.rs     # hit_test + hit_test_target (pixel/char → (line,col,EditTarget)). Char-space
│                  #   logic ALWAYS compiled; pixel→char done in element.rs.
├─ render_plan.rs  # apply_document pipeline → RowStyle plan (per-row Vec<StyledRange> + markers
│                  #   + margin text). Diff-and-patch / meta-diff dirty-range computation.
│                  #   ALWAYS compiled (produces a plan; gpui paints it).
├─ keymap.rs       # handle_normal_key / handle_edit_key / handle_hex_edit_key → EditorEvent or
│                  #   internal edit mutation. Key enum is gpui-independent.
├─ element.rs      # [cfg ui] the bespoke raw-gpui Element: shape_line, paint_quad, hitbox,
│                  #   EntityInputHandler bridge. Translates pixels→char then calls hit_test.
├─ view.rs         # [cfg ui] EditorView: Entity<T> holding state, uniform_list, find bar,
│                  #   popups, theme. Owns the channel to the controller.
└─ previews/       # [cfg ui] HoverPreview trait + 4 concrete previews + HoverPopupHost.
```

Crates (all already in `crate_selection.md` / `ARCHITECTURE.md`):
- **gpui** + **gpui_platform** (rendering, `uniform_list`, `shape_line`, `EntityInputHandler`, animations, focus/tab) — see `gpui_cookbook.md` §3.
- **gpui-component** for the *chrome around* the editor (find-bar `TextInput`, type-picker `List`/`Select`, context menus via `ContextMenuExt`, tooltips, popovers) — see `gpui_component_cookbook.md`. **The editor surface itself is custom raw-gpui** (cookbook §"Can gpui-component's Table/List back the bespoke editor surface? — NO").
- **ropey** (text buffer + char↔byte index conversion; the diff/patch path). Document sizes (45k-field benches) justify a rope.
- **unicode-width** / **unicode-segmentation** (multi-byte glyphs `▸ ▾ ├ │ └ · → ◀ ▶ ✕`; grapheme-aware cursor movement in edit mode).
- **smallvec** (per-row styled-range vectors), **bitflags** (marker mask `M_*`, `KindFlags`), **memchr** (find-bar substring search).

**No QScintilla equivalent exists.** Every `SCI_*` call is re-expressed: indicators → per-row `Vec<StyledRange{char_range, style, z}>` composited by ascending `z = indicator id`; markers → row background / left accent bar; margins → a fixed-width gutter element; `SCI_FINDCOLUMN`/`SCI_GETCOLUMN`/`SCI_POSITIONFROMPOINTCLOSE` → char↔byte + char↔pixel helpers on the shaped line.

---

## 1. Coordinate systems (MUST match the map §1 exactly)

- **Display column** = char index within a line's text (NOT byte). Multi-byte glyphs exist. C++ used `SCI_FINDCOLUMN` (col→utf8 byte) and `SCI_GETCOLUMN` (byte→col). **Rust decision: work in `char` indices throughout the span/edit logic** (matches the C++ `QString` which is UTF-16 but indexed per code-unit; all glyphs here are BMP so char-index == QString-index for these texts). Convert char↔byte only at the ropey/`shape_line` boundary:
  ```rust
  fn char_to_byte(line: &str, ch: usize) -> usize { line.char_indices().nth(ch).map(|(b,_)|b).unwrap_or(line.len()) }
  fn byte_to_char(line: &str, b: usize) -> usize { line[..b].chars().count() }
  ```
  Store `lineStarts: Vec<usize>` (char offset of each line start, from `ComposeResult`) for O(1) line slicing — exactly the C++ `ComposeResult::lineStarts`.
- **Line geometry** = `[fold prefix (kFoldCol=3, or 0 flush-left for CommandRow/root-footer)][indent depth*kTreeIndent(=2)][type col (effectiveTypeW)][sep(1)][name col (effectiveNameW)][sep(1)][value/comment…]`. The single source of truth is `LineGeometry::for_line()` (port of `core.h:1171`).

---

## 2. Key types consumed (defined in `core` per its own spec; restated for interface)

These belong to the `core` module (`PORTING_core-model.md`); the editor imports them. Listed here only so the editor signatures resolve.

| C++ (core.h) | Rust counterpart | Notes |
|---|---|---|
| `enum class NodeKind : uint8_t` (24) | `#[repr(u8)] enum NodeKind` + `const KIND_META: [KindMeta; N]` | helpers `size_for_kind`, `is_hex_node`/`is_hex_preview`, `is_func_ptr`, `is_vector_kind`, `is_matrix_kind`, `kind_meta`. |
| `struct LineMeta` (994) | `struct LineMeta` (`#[derive(Clone, PartialEq)]`) | one per display line. PartialEq is the basis for the meta-diff `same_line()` — see §5. `offset_text: String`, `chips: SmallVec<[LineChip;4]>`. |
| `struct LineChip` (982) | `struct LineChip { kind: ChipKind, start_col: i32, end_col: i32, text: String, rtti_vtable_addr: u64, type_hint_kinds: SmallVec<[NodeKind;4]>, enum_current_value: i64, enum_ref_node_id: u64 }` | `find_chip(lm, kind) -> Option<&LineChip>`. |
| `enum class ChipKind` (971) | `#[repr(u8)] enum ChipKind { Enum=0, TypeHint, Rtti, Symbol, Comment, AddComment }` | render order = enum order. |
| `struct ColumnSpan` (1119) | `#[derive(Clone,Copy,Default)] struct ColumnSpan { start: i32, end: i32, valid: bool }` | start inclusive, end exclusive. `Default` ⇒ `{0,0,false}` (the C++ `{}` invalid span). |
| `enum class EditTarget` (1125) | `#[derive(Clone,Copy,PartialEq)] enum EditTarget { Name, Type, Value, BaseAddress, Source, ArrayIndex, ArrayCount, ArrayElementType, ArrayElementCount, PointerTarget, RootClassType, RootClassName, TypeSelector, StaticExpr, Comment }` | discriminant order must match for any FFI/serde; we keep it identical. |
| `enum Marker:int` (182) | `bitflags! struct MarkerMask: u32` with `M_CONT, M_PTR0, M_CYCLE, M_ERR, M_STRUCT_BG, M_HOVER, M_SELECTED, M_CMD_ROW, M_ACCENT, M_FOCUS` | the C++ `markerMask:u32` ports as-is. |
| `struct ViewState` (1456) | `struct ViewState { scroll_line: usize, cursor_line: usize, cursor_col: usize, x_offset: f32, cursor_node_id: u64, cursor_sub_line: i32 }` | node-id-anchored save/restore. |
| `struct ComposeResult` (1064) | `struct ComposeResult { text: String, meta: Vec<LineMeta>, layout: LayoutInfo, max_line_len: usize, line_starts: Vec<usize> }` | from `compose`. |
| `struct LayoutInfo` (1054) | `struct LayoutInfo { type_w: i32 (14), name_w: i32 (22), offset_hex_digits: i32 (8), base_address: u64, tree_lines: bool }` | |
| `struct ValueHistory` (867) | `struct ValueHistory` 10-entry ring | editor holds `&HashMap<u64, ValueHistory>` (read-only). `heat_level()` 0/1/2/3 rules. |
| Selection-ID bit encoding (933) | free fns `make_array_elem_sel_id`, `array_elem_idx_from_sel_id`, `make_member_sel_id`, `member_sub_from_sel_id`; consts `K_COMMAND_ROW_ID=u64::MAX`, `K_FOOTER_ID_BIT`, `K_ARRAY_ELEM_BIT`, `K_MEMBER_BIT`, masks | editor masks these off to recover bare nodeId. |
| Column constants (1130) | `pub const K_FOLD_COL: i32 = 3; K_TREE_INDENT = 2; K_COL_TYPE = 14; K_COL_NAME = 22; K_COL_VALUE = 96; K_COL_COMMENT = 28; K_COL_BASE_ADDR = 12; K_SEP_WIDTH = 1; K_MIN_TYPE_W = 9; K_MAX_TYPE_W = 128; K_MIN_NAME_W = 10; K_MAX_NAME_W = 128; K_COMPACT_TYPE_W = 20;` | |

The `Provider` methods the editor calls (read-only) are the abstract trait from `provider` (per `PORTING_providers.md`): `read`, `read_u32`, `read_u64`, `read_bytes`, `is_readable`, `is_writable`, `is_valid`. Held as three references injected by `set_provider_ref(prov, real_prov, tree)`.

---

## 3. Indicator & marker vocabulary → Rust z-ordered styled ranges

The C++ uses 18 Scintilla indicator slots (map §3, `editor.cpp:1712–1761`) + 10 markers. Port:

```rust
/// One styled span on a row. z = the indicator slot ID; higher z wins on overlap
/// (matches Scintilla compositing by ascending index, map §3 "Higher slot wins").
struct StyledRange { char_range: Range<usize>, style: SpanStyle, z: u8 }
enum SpanStyle {
    Fore(Hsla),                 // TEXTFORE — recolor text
    BoxUnder { fill: Hsla, alpha: u8 },  // STRAIGHTBOX — pill/edit-bounds background behind text
    UnderlineThick(Hsla),       // COMPOSITIONTHICK — find match underline
    Hidden,                     // IND_EDITABLE — never drawn, queried for editable ranges
}
```

Indicator ID table (keep the numeric IDs — they ARE the z-order). All colors come from `Theme` at apply time:

| ID | const | z | role |
|----|-------|---|------|
| 8 | `IND_EDITABLE` | (not painted) | marks editable token ranges; queried by hit-test |
| 9 | `IND_HEX_DIM` | 9 | `theme.text_faint` Fore — dim hex bytes/fold arrows/braces/footer |
| 10 | `IND_BASE_ADDR` | 10 | `theme.text` Fore — command-row address |
| 11 | `IND_HOVER_SPAN` | 11 | `theme.ind_hover_span` Fore — link-blue on hovered editable token |
| 12 | `IND_CMD_PILL` | 12 | BoxUnder α100 — command-row + footer pill bg |
| 13 | `IND_HEAT_COLD` | 13 | `theme.ind_heat_cold` Fore |
| 14 | `IND_CLASS_NAME` | 14 | `theme.syntax_type` Fore — teal root class name |
| 15 | `IND_HINT_GREEN` | 15 | `theme.ind_hint_green` Fore — comment/symbol/AddComment + live hint |
| 16 | `IND_LOCAL_OFF` | 16 | `theme.text_faint` Fore — dim inline local offset |
| 17 | `IND_HEAT_WARM` | 17 | Fore |
| 18 | `IND_HEAT_HOT` | 18 | Fore |
| 19 | `IND_FIND` | 19 | UnderlineThick — search match |
| 20 | `IND_TYPE_HINT` | 20 | `theme.text_dim` Fore |
| 21 | `IND_RTTI_HINT` | 21 | `theme.ind_rtti_hint` Fore — amber |
| 22 | `IND_CHIP_BG` | 22 | BoxUnder α100 — pill bg under every chip |
| 23 | `IND_CHIP_HOVER` | 23 | BoxUnder α130 `theme.hover` |
| 24 | `IND_CHIP_PRESSED` | 24 | BoxUnder α200 `theme.selected` |
| 25 | `IND_TREE_CONN` | 25 | `theme.text_dim` Fore — innermost connector |
| 26 | `IND_BYTE_SEL` | 26 | `theme.ind_hover_span` Fore — selected hex digits |
| 27 | `IND_EDIT_BOUNDS` | 27 | BoxUnder α255 `theme.selected` — byte-range edit zone |

Markers (`M_*`, map §2): **whole-row decoration**, not text spans. Port as per-row `RowDecor { bg: Option<Hsla>, accent_bar: bool, focus_glow: Option<f32> }`. `M_SELECTED`/`M_STRUCT_BG`/`M_HOVER`/`M_CMD_ROW` → bg; `M_ACCENT` → 2px left accent bar (was margin-1 symbol); `M_FOCUS` → pulsing glow (presentation mode); `M_CONT`/`M_PTR0`/`M_CYCLE`/`M_ERR` → bg variants.

**GPUI paint:** in the custom `Element::paint`, for each visible row: (1) `window.paint_quad(fill(row_bounds, decor.bg))`; (2) build a `Vec<TextRun>` for the row by walking the composited styled ranges (sort by z, last-writer-wins for Fore on each char; BoxUnder ranges → `paint_quad` rects behind the shaped line via `line.x_for_index`); (3) `shape_line(text, font_size, &runs, None).paint(...)`; (4) accent bar / find underline as extra quads. See `gpui_cookbook.md` §3.2–3.5.

---

## 4. Public API surface → Rust (item-by-item)

`RcxEditor` (a `QWidget`) → **`EditorView`** = a gpui `Entity<EditorState>` implementing `Render` + `Focusable`. The state struct holds everything the C++ kept in members (`m_*`). Public methods map to inherent methods on `EditorState` (mutating via `&mut self`, taking `&mut Window, &mut Context<Self>` only where gpui needs it). Pure span statics become free fns in `geometry.rs`.

### 4.1 Lifecycle / document

| C++ | Rust signature | Behavior / crate |
|---|---|---|
| `RcxEditor(QWidget*)` ctor (1771) | `EditorState::new(window, cx) -> EditorState` | build focus handle, find-bar entity, dwell/glow timers (gpui `cx.spawn` deferrals), preview registry. No Scintilla setup; instead initialize `RenderPlan` empty. |
| `applyDocument(const ComposeResult&)` (2544) | `fn apply_document(&mut self, result: ComposeResult, cx: &mut Context<Self>)` | THE render pipeline — §5. Rebuilds `node_line_index`, computes dirty range, builds the per-row `RenderPlan`, restores hover/byte-sel/find, emits `EditorEvent::DocumentApplied(text)`, `cx.notify()`. |
| `saveViewState() const` (4011) | `fn save_view_state(&self) -> ViewState` | first-visible line, cursor line/col, x_offset, cursorNodeId/subLine from line's LineMeta. |
| `restoreViewState(vs)` (4029) | `fn restore_view_state(&mut self, vs: ViewState)` | node-id-anchored restore (find line owning same nodeId+subLine), else clamp; clamp x_offset to content width. |
| `scrollToNodeId(u64)` (4144) | `fn scroll_to_node_id(&mut self, id: u64)` | first non-footer line → set cursor + `scroll_handle.scroll_to_item(line, ScrollStrategy::Nearest)`. |
| `smoothScrollToNodeId(u64)` (4156) | `fn smooth_scroll_to_node_id(&mut self, id, cx)` | presentation mode: gpui animation (OutExpo, 400ms, center; snap-then-animate if >50 lines). Non-presentation → instant. |
| `setFocusNode`/`clearFocusNode` (4227/4272) | `fn set_focus_node(&mut self, id)` / `fn clear_focus_node(&mut self)` | `M_FOCUS` glow on all lines of node, 30ms sine-pulse timer. `is_focus_glow_active()` = `focus_node_id != 0`. |
| `metaForLine(int) const` | `fn meta_for_line(&self, line: usize) -> Option<&LineMeta>` | index into `self.meta`. **Public — tested directly.** |
| `currentNodeIndex() const` (4080) | `fn current_node_index(&self) -> i32` | cursor line → nodeIdx. |
| `textWithMargins() const` | `fn text_with_margins(&self) -> String` | margin offset text + line text per row. |

### 4.2 Span statics (pure — `geometry.rs`, ALWAYS compiled, the test bedrock)

These are the C++ `static` span helpers exposed on `RcxEditor`, delegating to `core.h` free functions. Port both layers as free fns:

```rust
// geometry.rs — direct 1:1 ports of core.h:1183–1438 (signatures keep the i32 col math).
pub fn type_span_for(lm: &LineMeta, type_w: i32) -> ColumnSpan;      // core.h:1183
pub fn name_span_for(lm: &LineMeta, type_w: i32, name_w: i32) -> ColumnSpan;  // 1189
pub fn value_span_for(lm: &LineMeta, _line_len: i32, type_w: i32, name_w: i32) -> ColumnSpan; // 1202 (hex→valWidth=23)
pub fn comment_span_for(lm: &LineMeta, line_len: i32, type_w: i32, name_w: i32) -> ColumnSpan; // 1263
pub fn member_name_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;   // 1225 (scan " = ")
pub fn member_value_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;  // 1235
pub fn static_expr_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;   // 1246 (scan "return " … "→")
pub fn command_row_src_span(line_text: &str) -> ColumnSpan;        // 1285 (label up to ▾)
pub fn command_row_addr_span(line_text: &str) -> ColumnSpan;       // 1312 (after ▾; 0x/<formula>/[ptr])
pub fn command_row_root_type_span(line_text: &str) -> ColumnSpan;  // 1338
pub fn command_row_root_name_span(line_text: &str) -> ColumnSpan;  // 1347
pub fn command_row_chevron_span(line_text: &str) -> ColumnSpan;    // 1365 ("[▸]" at col 0)
pub fn array_elem_type_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;        // 1376
pub fn array_elem_count_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;       // 1385
pub fn array_elem_count_click_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan; // 1395 (incl brackets)
pub fn pointer_kind_span_for(_lm: &LineMeta, _line_text: &str) -> ColumnSpan;  // 1408 ALWAYS invalid
pub fn pointer_target_span_for(lm: &LineMeta, line_text: &str) -> ColumnSpan;  // 1412 (ind→'*')
pub fn array_prev_span_for / array_index_span_for / array_count_span_for / array_next_span_for;  // 1424+

pub struct LineGeometry { prefix_width: i32, indent_width: i32, type_column_width: i32, name_column_width: i32 }
impl LineGeometry { fn for_line(lm: &LineMeta) -> Self; fn type_start/name_start/value_start/document_column(...); } // core.h:1155–1181
```

The three editor-public statics that tests call directly (`test_editor:740`):
```rust
// re-exported on EditorState as associated fns to keep test call-shape; delegate to geometry.
pub fn type_span(lm: &LineMeta) -> ColumnSpan { type_span_for(lm, K_COL_TYPE) }            // editor.cpp:4281
pub fn name_span(lm: &LineMeta) -> ColumnSpan { name_span_for(lm, K_COL_TYPE, K_COL_NAME) }// :4282
pub fn value_span(lm: &LineMeta, line_len: i32) -> ColumnSpan { value_span_for(lm, line_len, K_COL_TYPE, K_COL_NAME) } // :4283
```

**editor-local header/static helpers** (`editor.cpp:4638–4716`, NOT in core.h) → `geometry.rs` private fns: `header_name_span`, `header_type_name_span`, `array_header_type_span`. These reject anonymous `struct/union/class` keywords and `[N]` array-element names and handle the `static ` prefix. Port the exact text-scan logic.

### 4.3 Inline edit + byte selection + selection overlay

| C++ | Rust | Notes |
|---|---|---|
| `isEditing()` | `fn is_editing(&self) -> bool` | `self.edit.active`. |
| `editSpanStart()` | `fn edit_span_start(&self) -> i32` | `self.edit.span_start`. |
| `editEnd()` / `editEndCol()` (6841) | `fn edit_end(&self) -> i32` | `span_start + original.chars().count() + (cur_line_len - linelen_after_replace)` — §7. |
| `beginInlineEdit(target,line,col)` (6483) | `fn begin_inline_edit(&mut self, target: EditTarget, line: i32, col: i32, cx) -> bool` | §6 — dispatch + reject. |
| `cancelInlineEdit()` (6992) | `fn cancel_inline_edit(&mut self, cx)` | `end_inline_edit()` + emit `EditorEvent::InlineEditCancelled`. |
| `commitInlineEdit()` (6912) | `fn commit_inline_edit(&mut self, cx)` | §6 — byte-range vs normal commit. |
| `setHexEditPending(bool)` | `fn set_hex_edit_pending(&mut self, v: bool)` | context-menu hex flag. |
| `applySelectionOverlay(QSet<u64>)` (3282) | `fn apply_selection_overlay(&mut self, sel_ids: &HashSet<u64>, cx)` | skip if unchanged AND last apply was patch; decode bits, match line-type to sel-type, set `M_SELECTED`+`M_ACCENT`, paint editable spans on non-footer rows. |
| `selectedNodeIndices()` (4319) | `fn selected_node_indices(&self) -> HashSet<i32>` | from text-selection line range → nodeIdx set; cursor line if none. |
| `byteSelection()` | `fn byte_selection(&self) -> Option<(u64,u64)>` | `self.byte_sel`. |
| `clearByteSelection()` | `fn clear_byte_selection(&mut self, cx)` | reset + re-paint overlay. |
| `setByteSelection(lo,hi)` | `fn set_byte_selection(&mut self, lo: u64, hi: u64, cx) -> bool` | **enforces `hi > lo`**, returns false otherwise (editor.h:83). |

### 4.4 Setters / injectors

`set_value_history_ref(&HashMap<u64,ValueHistory>)`, `set_expr_evaluator(Box<dyn Fn(&str)->String>)`, `set_provider_ref(prov, real, tree)`, `set_relative_offsets(bool)` (triggers `reformat_margins`), `set_saved_sources(Vec<SavedSourceDisplay>)`, `set_custom_type_names(Vec<String>)`, `set_static_completions(Vec<String>)`, `set_hover_effects(bool)`, `set_presentation_mode(bool)`, `set_editor_font(String)` / `set_global_font_name` / `global_font_name`, `apply_theme(&Theme)`, `set_command_row_text(&str)` (7872).

Lifetime note: the C++ holds raw `const Provider*`/`const NodeTree*`/`const QHash*` borrowed from the controller. In Rust use `Option<Rc<dyn Provider>>` / `Rc<RefCell<NodeTree>>` / `Rc<RefCell<HashMap<u64,ValueHistory>>>` (the app is single-threaded on the UI; `Rc` matches the C++ shared-ownership-by-reference without `Send`). The expr evaluator is `Option<Rc<dyn Fn(&str)->String>>`.

### 4.5 Events (the C++ signals → one enum)

The 45+ Qt signals (editor.h:120–219) become one `EditorEvent` enum delivered via a channel the controller drains (or a `Box<dyn Fn(EditorEvent, &mut App)>` callback set by the controller). The editor NEVER mutates the model — it only emits.

```rust
pub enum EditorEvent {
    MarginClicked { margin: i32, line: usize, mods: Modifiers },
    ContextMenuRequested { line: usize, node_idx: i32, sub_line: i32, global_pos: Point<Pixels> },
    KeywordConvertRequested { new_keyword: String },
    NodeClicked { line: i32, node_id: u64, mods: Modifiers },
    InlineEditCommitted { node_idx: i32, sub_line: i32, target: EditTarget, text: String, resolved_addr: u64 },
    InlineEditCancelled,
    TypeSelectorRequested,
    TypePickerRequested { target: EditTarget, node_idx: i32, global_pos: Point<Pixels> },
    SourcePopupRequested { global_pos: Point<Pixels> },
    InsertAboveRequested { node_idx: i32, kind: NodeKind },
    CommentEditRequested,
    RelativeOffsetsChanged(bool),
    RttiChipClicked { vtable_addr: u64, class_name: String, host_node_id: u64 },
    RttiNullChipClicked,
    EnumChipClicked { node_idx: i32, enum_ref_node_id: u64, current_value: i64, global_pos: Point<Pixels> },
    TypeHintChipClicked { node_idx: i32, inferred_kinds: SmallVec<[NodeKind;4]> },
    AppendBytesRequested { struct_id: u64, byte_count: i32 },
    TrimHexRequested { struct_id: u64 },
    AppendEnumMembersRequested { enum_id: u64, count: i32 },
    AppendSingleFieldRequested { struct_id: u64 },
    DeleteSelectedRequested, DuplicateSelectedRequested,
    CopyNodesRequested, CutNodesRequested, PasteNodesRequested,
    DocumentApplied(String),
    QuickTypeChangeRequested { node_idx: i32, target_kind: NodeKind },
    CycleSameSizeTypeRequested { node_idx: i32, direction: i32 },
    MoveNodeRequested { node_idx: i32, direction: i32 },
    CollapseAllRequested, ExpandAllRequested,
    GoToDefinitionRequested { node_idx: i32 },
    OpenTypeInNewTabRequested { node_idx: i32 },
    ByteCopyHexRequested, BytePasteHexRequested, ByteZeroFillRequested,
    ByteCopyAsCArrayRequested, ByteCopyAsPythonRequested, ByteSaveAsFileRequested,
    ByteBreakIntoClassRequested { lo: u64, hi: u64 },
    ByteRangeCommitRequested { addr: u64, bytes: Vec<u8> },
    StatusHintRequested(String),
}
```

`QSignalSpy` in the C++ tests → in Rust, the test harness installs a `Vec<EditorEvent>` sink callback and asserts on its contents (count + payload).

---

## 5. `apply_document` pipeline (port of editor.cpp:2544; produces a `RenderPlan`)

The C++ steps in order (map §5). Rust mirrors them into building `self.plan: RenderPlan`:

```rust
struct RenderPlan {
    rows: Vec<RowPlan>,                  // one per display line
}
struct RowPlan {
    text_range: Range<usize>,            // into ComposeResult.text (via line_starts)
    spans: SmallVec<[StyledRange; 8]>,   // composited indicators (sorted by z at paint)
    decor: RowDecor,                     // markers → bg/accent/glow
    margin_text: String,                 // offset gutter
    fold_level: i32,
}
```

Steps (keep order — several are load-bearing for parity):
1. If editing → `end_inline_edit()` silently (no commit signal; refresh in progress).
2. Set `applying_document = true` (suppress popup dismiss). Save hover state.
3. `self.meta = result.meta; self.layout = result.layout`. Rebuild `node_line_index: HashMap<u64, SmallVec<[usize;4]>>` (ahash) for O(1) hover/selection lookup.
4. Margin width: relative mode uses `max(offset_hex_digits/2, 4)` digits else full.
5. **Diff-and-patch text update** (2582): compute longest common *line-aligned* prefix and suffix between `self.prev_text` and `result.text`; if changed middle ≤ 50% of doc → patch just the differing lines into the rope; else replace whole rope. **Edge case (2620):** the suffix-walk only extends forward past `\n` when mid-line ("footer ate everything" guard). If patch covers offset 0 → clear `last_command_row_text`. `prev_text = result.text.clone(); last_apply_was_patch = did_patch`. *(ropey makes the patch a `rope.remove(range)+insert`; the diff is over `&str` lines so memchr/`split('\n')` is fine.)*
6. Horizontal scroll width from `result.max_line_len` × char advance; reset x_offset (restore fixes later).
7. (No re-colourise step — coloring is the plan, rebuilt below.)
8. **Meta diff** (2734): if last apply was a patch, compute `[first_changed, last_changed]` by comparing `prev_meta[i]` vs `meta[i]` with `same_line()`. **`same_line` = field-by-field equality over EVERY field consumed by per-line passes** — implement as a hand-written comparison (NOT blanket `PartialEq`, because some fields like `data_changed`/`changed_byte_indices`/`offset_addr` must NOT trigger marker rebuild but heat fields must). Compare exactly: nodeId, subLine, lineKind, nodeKind, elementKind, foldLevel, markerMask, depth, foldHead, foldCollapsed, braceCol, isContinuation/RootHeader/ArrayHeader/ArrayElement/MemberLine/StaticLine, heatLevel, chips (kind/startCol/endCol/text each), lineByteCount, effectiveTypeW/NameW, pointerTargetName. This dirty range narrows the marker/indicator rebuild.
9. **Clear ALL Fore/Box indicators full-doc** (2800) — the C++ note: narrowing this broke in production, so always full-clear+rebuild (cheap). In Rust, rebuilding `RowPlan.spans` for every row each apply is fine (microseconds for the visible set; the plan is data, paint is virtualized). Reset chip hover/pressed bookkeeping.
10. `apply_line_attributes(first_changed, last_changed)` (3007) — markers/margins. Margin text is **forced full-pass** even on narrow updates. CommandRow→`M_CMD_ROW`; else apply each `M_*` bit in `markerMask`; set fold level from `lm.foldLevel`.
11. `apply_hex_dimming(-1,-1)` (3235) — full-pass: dim fold arrows on fold-head lines, entire hex-preview value row, entire footer, trailing `{` via braceCol; then `IND_TREE_CONN` tints innermost connector glyph per depth>0 row.
12. Build `line_texts` slices via `line_starts` (O(1)).
13. `apply_heatmap_highlight` (4370), `apply_symbol_coloring` (no-op now), `apply_command_row_pills` (4486) — full-pass.
14. **Footer pill backgrounds** (2846): search footer text for ` +1 `, `+1000h`/`+100h`/`+10h`, `+10`, `Trim`, `Top` (longest-first, collision guards) → paint `IND_CMD_PILL`.
15. **Per-chip indicators** (2893): for each chip paint `IND_CHIP_BG` over `[start_col,end_col)` exactly (no pad), plus kind-specific Fore: Comment/Symbol/AddComment→`IND_HINT_GREEN`, TypeHint→`IND_TYPE_HINT`, Rtti→`IND_RTTI_HINT`, Enum→`IND_HOVER_SPAN`.
16. Restore hover: if hovered node deleted, clear + dismiss popups. Clear `M_HOVER`, reset prev-hover, `apply_hover_highlight`.
17. Re-apply focus glow, re-apply find indicator (re-search full doc), `apply_byte_selection_overlay` (address-based → survives).
18. `prev_meta = result.meta`. Emit `EditorEvent::DocumentApplied(text)`. `cx.notify()`.

### `apply_heatmap_highlight` (4370)
Per line: clear 3 heat indicators; if `heat_level>0` pick cold/warm/hot. **Hex rows:** paint only `changed_byte_indices` (each byte's 2 digits at `vs.start + b*3 .. +2`); paint nothing if none changed this tick. Non-hex: full (narrowed) value span via `narrow_ptr_value_span` (clip at first chip start_col).

### `reformat_margins(first,last)` (3070)
Two passes. **Pass 1**: per-line margin text — Continuation/member → `"  · "`; Footer/sep/CommandRow in relative mode → blank; Relative → `"+<hexUpper>"` right-justified to `hex_digits` (base = `ptr_base` or layout base); Absolute → zero-padded uppercase hex. **Pass 2** (skip if `layout.tree_lines`): inline **local offsets** in the indent area for depth>1 field/header lines — derive parent address (ptr_base / parent_addr / backward-scan for enclosing Header/ArrayElementSeparator), write `"+<hex>"` into the indent slot (needs ≥3 chars), color `IND_LOCAL_OFF`. *(The C++ flips readOnly off and REPLACETARGETs into the buffer. In Rust there is no separate text buffer for margins — write into `RowPlan.margin_text` and inject local-offset spans into the row's content area as `IND_LOCAL_OFF` ranges over the indent slot. No readOnly dance needed.)*

---

## 6. Inline editing lifecycle (the heavily-tested contract)

### `InlineEditState` (editor.h:302) → `edit_state.rs`
```rust
#[derive(Default)]
struct InlineEditState {
    active: bool, line: i32, node_idx: i32, sub_line: i32,
    target: EditTarget, span_start: i32, linelen_after_replace: i32,
    original: String, pos_start: i64, pos_end: i64,  // byte positions in the line/doc
    edit_kind: NodeKind, comment_col: i32, last_validation_ok: bool, hex_overwrite: bool,
    pad_bytes: i64, pad_pos: i64, pad_restore_spaces: i32,   // trailing-pad bookkeeping
    byte_range: bool, byte_range_addr: u64, byte_range_len: i32,
    byte_segments: SmallVec<[ByteEditSegment; 2]>, byte_seg_idx: i32,
}
struct ByteEditSegment { line: i32, span_start: i32, span_end: i32, byte_count: i32 }
```
The edit operates on a **scratch copy of the active row's text** (the C++ edits the live Scintilla buffer with `readOnly` flipped off; Rust keeps the row text in the rope but tracks the edit as `(span_start, current_text, selection)` in the state and re-derives the displayed row each frame). Selection is `Range<usize>` char-cols on the edit line.

### `begin_inline_edit(target, line, col)` (port editor.cpp:6483 exactly — tests assert each branch)
Pseudocode (parity-critical order):
```
if target == TypeSelector: return false                         // popup-only (test:471)
if byte_sel.is_some(): byte_sel = None; apply_byte_selection_overlay()
// Picker targets → emit signal, NOT inline edit:
if target in {Type, ArrayElementType, PointerTarget}:
    if line < 0: line = cursor.line
    lm = meta_for_line(line)?;  if lm.node_idx < 0: return false
    if lm.line_kind == Footer: return false
    pos = type column start; emit TypePickerRequested{target, lm.node_idx, global_pos}; return true
if target == Source:
    pos = below src span; emit SourcePopupRequested{global_pos}; return true   // returns true, isEditing stays false (test:497)
if edit.active: return false
reset pad trackers; clear hover; dismiss popups; clear IND_EDITABLE hint
if line >= 0: set cursor (line, col>=0?col:0)
if col < 0: (line,col) = cursor
lm = meta_for_line(line)?
// CommandRow gate: nodeIdx<0 allowed ONLY for CommandRow + {BaseAddress,Source,RootClassType,RootClassName}
if lm.node_idx < 0 && !(lm.line_kind==CommandRow && target in {BaseAddress,Source,RootClassType,RootClassName}):
    return false                                                // test:471 Type/Name/Value rejected on cmd row
// Hex gate: hex nodes block Name/Value unless static, unless context-menu hex-edit pending
is_hex_edit = hex_edit_pending && is_hex_node(lm.node_kind) && !lm.is_static_line && target in {Name,Value}
hex_edit_pending = false
if target in {Name,Value} && is_hex_node(lm.node_kind) && !lm.is_static_line && !is_hex_edit:
    return false                                                // map §17.2
// resolve span: is_hex_edit→fixed hex/ASCII spans; Comment→chip span or EOL; else resolved_span_for
// vector/matrix Value → narrow to clicked component; strip comment marker prefix
// populate edit_state; for Value/BaseAddress/hex with comment_col: extend line w/ trailing pad (track pad_bytes/pad_pos)
// Comment: if none, trim trailing ws + append "  " placeholder (track pad_restore_spaces); else strip "/ " prefix
// set IBeam cursor, selection [pos_start,pos_end]; hex overwrite → caret at pos_start
// show initial hint comment; Value → defer apply_hover_cursor (history popup)
return true
```

### `resolved_span_for(line, target) -> Option<NormalizedSpan>` (editor.cpp:4758)
The authoritative editable-region resolver. Port the exact branch order (§ shown in source read): CommandRow → src/addr/root spans (reject targets not in the cmd-row set); `if lm.node_idx < 0: return None`; **hex nodes block Name/Value** (except static); per-target via the `*_span_for` helpers; header fallbacks (`array_header_type_span`/`header_type_name_span`/`pointer_kind_span_for` for Type; `header_name_span` for Name); member-line overrides; Comment → chip span or `[len-1,len)` EOL fallback for creation on Field/Header non-continuation/non-member rows. Finish with `normalize_span(s, line_text, target, skip_prefixes = (target==BaseAddress for cmd row else true))`.

### `normalize_span(raw, line_text, target, skip_prefixes)` (editor.cpp:4718)
Clamp to text length; optionally strip leading `->`/`=` value prefixes (Value); trim leading/trailing whitespace → `NormalizedSpan { start, end, valid }`.

### `commit_inline_edit()` (6912)
Extract edited text (trim unless hex overwrite). **Byte-range branch**: parse hex digit pairs (single-row from edited text; multi-row by reading each segment's slice), `end_inline_edit()`, emit `ByteRangeCommitRequested{addr, bytes}`. **Else**: Type edit empty → keep original; capture resolved address; `end_inline_edit()`; emit `InlineEditCommitted{node_idx, sub_line, target, text, resolved_addr}`.

### `end_inline_edit() -> EndEditInfo {node_idx, sub_line, target}` (4544)
Shared shutdown: cancel autocomplete; hide expr-result label; clear edit comment + `M_ERR`; **restore trailing padding** (Value/BaseAddress: strip last `pad_bytes` chars; Comment: restore from `pad_pos` with `pad_restore_spaces`) so the row is byte-identical to pre-edit; **clear `prev_text`** (forces full-replace next refresh — inline edits diverge the rope from prev_text; map §17.5); deactivate; clear byte-range state + `IND_EDIT_BOUNDS`; restore arrow cursor; reset selection rendering.

### `clamp_edit_selection()` (6849)
Re-entrancy guarded (`clamping_selection` flag). **Hex overwrite**: collapse any selection to cursor; multi-row byte edit preserves the cursor's actual line (advanceToByteSegment placed it). **Else**: if cursor (start==end) leave alone; if selection spans off the edit line → force to `[span_start, edit_end]` on edit line; else clamp both ends to `[span_start, edit_end]`. This is what guarantees the `testAddrEditSelectionCollapseLeft/Right` and `…NoLeakRight` invariants.

---

## 7. Key handling (`keymap.rs`)

Three handlers, ported as methods returning `bool` (handled) and mutating edit state / emitting events. Input is a toolkit-independent `KeyInput { key: Key, mods: Modifiers, text: Option<String> }` produced by the gpui `on_key_down`/action layer.

### `handle_normal_key` (5591) — large switch (map §10)
Popup-visible first (Tab/Backtab cycle previews, Esc dismiss). Then: F2→edit Name; F12→`GoToDefinitionRequested`; T→edit Type; Return→byteSel `begin_byte_edit` else edit Value; Backspace/Delete→byteSel `ByteZeroFillRequested`, Delete also `DeleteSelectedRequested`; Ctrl+D→`DuplicateSelectedRequested`; clipboard Ctrl+C/X/V (byte variants if byteSel); Insert/Shift+Insert→`InsertAboveRequested(Hex64/Hex32)`; `;`→`CommentEditRequested`; Up/Down nav (skip nodeId 0/CommandRow/continuation; Down past end → `AppendSingleFieldRequested`; byteSel+Shift→`snap_byte_selection_to_row`; Ctrl+Shift→`MoveNodeRequested`); PageUp/Down; **Tab** cycle edit targets `{Name,Value,Comment,ArrayElementType,ArrayElementCount,PointerTarget,Type}` after `last_tab_target`, skipping inapplicable; type shortcuts (Space cycles hex sizes; 1–5→Hex8/16/32/64/128; P→Pointer; F→Float/Double; S→signed; U→unsigned → `QuickTypeChangeRequested`); Left/Right byteSel-extend or `CycleSameSizeTypeRequested`; Esc two-stage (drop byteSel else clear selection via `NodeClicked(-1,0,NoMod)`); Home/End; Ctrl+A; Ctrl+Shift+[ / ] → `CollapseAllRequested`/`ExpandAllRequested`.

### `handle_edit_key` (6126) — non-hex inline editing
Return→commit; Tab→set `last_tab_target`+commit; Esc→cancel; Up/Down/PageUp/Down→block (consume); Delete→block at end else allow; Left/Backspace→clamp at `span_start` (Left collapses selection to left end); Right→clamp at `edit_end` (collapse selection right); Home/End→span start/end; Ctrl+V→sanitized paste (strip `\n\r`, also backtick for BaseAddress). **These exact clamps are the `testAddrEdit*` contract — port them verbatim.**

### `handle_hex_edit_key` (6218) — hex/ASCII overwrite (fixed-length)
`is_hex_mode` = Value target (hex digits) else Name = ASCII preview. Cursor confined to `[span_start, span_end)`. `replace_char_at` helper overwrites one char + re-fills `IND_HEX_DIM` (and `IND_EDIT_BOUNDS` during byte-range edits). Enter→commit; Esc→cancel; Tab/Up/Down/Page→block; Home/End→span ends; Left/Right→move skipping space separators, **hop to adjacent byte-edit segment** at boundaries (`advance_to_byte_segment`); Backspace→reset prev char to '0'/'.'; Delete→reset current; Ctrl+Z→block; Ctrl+V→paste only valid hex/printable skipping separators. Char input: hex mode accepts `[0-9a-fA-F]` (uppercased), overwrites current digit, advances skipping spaces; ASCII mode accepts printable 0x20–0x7E.

---

## 8. Byte selection (`byte_sel.rs`, ALWAYS compiled — map §12)

```rust
struct ByteSel { lo: u64, hi: u64 }  // half-open absolute address range
// state on EditorState: byte_sel: Option<ByteSel>, byte_sel_anchor: Option<u64>, byte_sel_dragging: bool
```
- `byte_addr_at(line, col) -> Option<u64>` (3447): only on hex-preview field rows in value column; `byte_idx = (col - vs.start)/3`; bounds-checked to `[0,size)`; returns `offset_addr + byte_idx`.
- `apply_byte_selection_overlay()` (3462): clear `IND_BYTE_SEL` full-doc; for each hex row overlapping `byte_sel` paint digits of intersected bytes (`vs.start + first*3 .. vs.start + (last-1)*3 + 2`); tail-call `update_byte_sel_status`.
- `update_byte_sel_status()` (3519): read selection bytes via provider; build a status line — address+size+multi-format interpretations by length (n==1: u8/i8/char; n==2: u16/i16 LE+BE; n==3: 24-bit LE/BE/RGB/ASCII; n==4: u32/i32/f32 LE+BE; n==8: u64/i64/f64 LE+BE; n∈[5,7]: packed ints + MAC for n==6; n≥9: first-12-byte hex preview); append `(read-only)` when provider not writable; emit `StatusHintRequested`. Use `bytemuck`/`from_le_bytes`/`from_be_bytes`.
- `begin_byte_edit()` (3660): one `ByteEditSegment` per overlapped hex row; reset `byte_sel`; `begin_inline_edit(Value, first_seg.line)` with `hex_edit_pending`; narrow to first segment; set `byte_range_*`; paint `IND_EDIT_BOUNDS` over all segments.
- `advance_to_byte_segment(delta)` (3739): hop active edit to prev/next segment; keep `edit.line` on the FIRST segment (padding bookkeeping lives there).
- `extend_byte_selection(d_byte)` (3772): grow/shrink `hi` (lo fixed); grow clamps to highest hex byte's end-address; shrink clamps at `lo+1`.
- `snap_byte_selection_to_row(dir)` (3803): Shift+Down/Up row-aware; snap `hi` to current row end then next/prev hex row's end; stop at first non-hex row; keep ≥1 byte.
- `select_all_hex_bytes()` (3880): union `[min lo, max hi]` across all hex-preview rows.

---

## 9. Hit testing (`hit_test.rs`)

```rust
struct HitInfo { line: i32, col: i32, node_id: u64, in_fold_col: bool }
fn hit_test(&self, vp: Point<Pixels>) -> HitInfo;     // 4858
fn hit_test_target(&self, vp) -> Option<(i32 /*line*/, i32 /*col*/, EditTarget)>;  // 4888 (static-ish)
```
- `hit_test` (4858): pixel→(line,col) via the shaped-line cache (`ShapedLine::closest_index_for_x` → char col; line = `y / line_height` clamped). Fallback computes line from Y when past text. `in_fold_col` = col in `[0,K_FOLD_COL]` on a fold-head line.
- `hit_test_target` (4888): maps a position to `(line,col,EditTarget)`. Order (verbatim): CommandRow (chevron→TypeSelector, src→Source, addr→BaseAddress, root-name→RootClassName); pointer sub-spans (target→PointerTarget); array header (count-click→ArrayElementCount, elem-type→ArrayElementType); generic type/name/value/comment with header/member fallbacks. **Redirects:** array-header Type→ArrayElementType; array-element type/name→ArrayElementType on parent header line; **hex nodes block Name/Value (only Type editable)**.

The pixel↔char conversion lives in `element.rs` (uses the per-row cached `ShapedLine` from the last paint); the **span/target logic above is pure char-col math** and is unit-tested without gpui by feeding synthetic `(line, col)`.

---

## 10. Event filter → gpui input wiring (`element.rs` + `view.rs`, map §9)

The C++ `eventFilter` (5012) intercepts mouse on `viewport()` and keys on `m_sci`. GPUI mapping (per `gpui_cookbook.md` §3.5, §4, §5):
- **Keys**: declare `actions!(reclass_editor, [EditNameF2, GotoDef, ...])` + bind in `run` with `key_context("ReclassEditor")`; OR a single `.on_key_down(cx.listener(...))` that builds `KeyInput` and routes to `handle_edit_key`/`handle_hex_edit_key`/`handle_normal_key`. Ctrl+F → show find bar.
- **Mouse**: in `Element::paint`, `window.on_mouse_event::<MouseDownEvent>(...)`, `MouseUpEvent`, `MouseMoveEvent`. Convert `ev.position` → `HitInfo`; reproduce the press branches: plain LMB clears byte_sel; Shift+Click on hex byte extends byte selection; fold-col click → `MarginClicked`; tail-chip router (Enum/TypeHint click → events; RTTI inert); footer-button click (` +1 `→`AppendSingleFieldRequested`, `+1000h/+100h/+10h`→`AppendBytesRequested`, `+10`→`AppendEnumMembersRequested`, `Trim`→`TrimHexRequested`, `Top`→scroll-top); CommandRow click → TypeSelector emits or `begin_inline_edit`; node click (Ctrl+Click navigable Header → `OpenTypeInNewTabRequested`; click already-selected → `begin_inline_edit`; else arm drag / byte-anchor / multi-select-defer → `NodeClicked`). **Consume all left-clicks** (no native caret). Double-click: offset margin → toggle relative/absolute; during edit → select all edit text; hex-byte dblclick → select whole-node bytes + `begin_byte_edit`; else `hit_test_target`→`NodeClicked`+`begin_inline_edit`.
- **Drag**: 8px Y threshold for row-drag; 8px move from byte anchor upgrades to byte-drag.
- **Hover**: `MouseMoveEvent` (or scrollbar `valueChanged` analog) re-derives hover nodeId/line, `update_chip_hover`, `apply_hover_cursor`. Cursor shape via `window.set_cursor_style(...)`: PointingHand for fold/footer-pill/type-pickers/clickable-chips/Ctrl-nav; IBeam for editable value text + hex bytes; Arrow over padding. Dwell timer 700ms (`cx.spawn` + check elapsed) gates hover-preview popups; arrow tooltips are NOT dwell-gated.
- **Focus**: FocusOut → commit active edit (deferred, unless picker/autocomplete active); FocusIn → `update_editable_indicators`. Use the field `FocusHandle`.

---

## 11. Find bar, popups, previews (`view.rs`, `previews/`)

- **Find bar** (map §4 `doFind`): a gpui-component `TextInput` (`.searchable()` not needed; we own search) + prev/next/close buttons. Two passes: paint `IND_FIND` at every match (full-doc `memchr`/`str::match_indices` over UTF-8); advance cursor to next/prev from `find_pos`, wrapping. Highlights + position persist across hide/reopen. Esc/close hides.
- **Pickers** (Type/ArrayElementType/PointerTarget/Source): the C++ `SCI_USERLISTSHOW` popups → gpui-component `List`/`Select` with `SearchableListDelegate` (`gpui_component_cookbook.md` §1). `show_type_list_filtered` merges `all_type_names_for_ui()` + `custom_type_names`, sorted, prefix-filtered. Pointer-target list = "void" + custom struct names. These are NOT inline edits — the editor only emits `TypePickerRequested`/`SourcePopupRequested`; the view/controller shows the picker and on selection emits `InlineEditCommitted` with the resolved type.
- **Hover preview system** (map §13): `HoverPreview` trait (`id`, `tab_label`, `subtitle`, `eligible`, `widget`), a `HoverPreviewRegistry: Vec<Box<dyn HoverPreview>>`, `HoverPopupHost` (gpui overlay via `deferred(anchored(...))`, `gpui_cookbook.md` §8), 4 concrete previews (StructTargetPreview, DisasmPreview, HexDumpPreview, ValueHistoryPreview). Dwell 700ms; Tab/Shift+Tab cycle eligible; per-kind persisted choice (`pick_last_used_preview_idx` reads app config — replaces `QSettings("hoverPreview/kind/<Kind>")`). `ValueHistoryPopup` with "Set" buttons is reused during inline value editing.
- **Persistence** (`QSettings` → app config store, per ARCHITECTURE): `compactRowSpacing`, `hoverPreview/kind/*`.

---

## 12. Theming, lexer, fonts

- `apply_theme(&Theme)` (2395): resolve editor paper = `theme.background.darker(115)`; set text/caret colors, all indicator Fore colors, lexer token colors, marker bg colors, find-bar style, cache `focus_glow_color` (pre-blend dim/bright opaque since alpha is ignored). Read `Theme` via the gpui `ActiveTheme` global pattern (`gpui_cookbook.md` §6) OR gpui-component's `cx.theme()` extended with Reclass's ~31 named colors (`gpui_component_cookbook.md` §6). The editor's per-row span colors are read fresh each frame from `cx.theme()`.
- **Lexer** (`setupLexer` 2285): C++ used `QsciLexerCPP` for syntax coloring of the displayed pseudo-code. **Rust:** a lightweight token classifier (keyword/number/string/type) — but per the map, *most coloring is indicator-driven, not lexer-driven*. Implement a small classifier in `render_plan.rs` that adds low-z (below indicator) Fore spans for keywords/numbers/strings/types; indicators (z 9–27) override. Built-in type names → keyword set; custom struct names → `GlobalClass` (teal) coloring.
- **Rendered C++ view** (`test_rendered_view.cpp`): this is the *separate* read-only generated-code view (`MainWindow::setupRenderedSci`), not the structured editor. Port as a simple gpui styled-text view (or a gpui-component code-editor `InputState::code_editor("cpp")`). The tests assert exact VS-Code colors (#569cd6 keyword, #6a9955 comment, #b5cea8 number, #ce9178 string, #c586c0 preproc, #d4d4d4 default, paper #1e1e1e, caret-line #2b2b2b, no brace match, dark paper on all 128 styles). These become a `RenderedViewStyle` constant table + `#[test]`s asserting the constants (color encoding is conceptual; in Rust assert against `Hsla`/`Rgba` equality).

---

## 13. Concurrency, platform

- **Single-threaded UI** (map §15). All deferrals via gpui `cx.spawn`/`cx.on_next_frame` / timers; no extra threads. Timers: hover dwell 700ms, focus glow 30ms, scroll anim 400ms.
- **Platform** (map §16): only one `#if QT_VERSION` guard (double-click X). No OS-specific code; nothing needs `#[cfg]` here beyond what gpui_platform already gates. The whole subsystem compiles on Linux (the only verifiable target).

---

## 14. Error-handling strategy

- The editor is infallible-by-design: it renders whatever `ComposeResult` it's given and emits events. There are **no `Result`-returning public methods** except `begin_inline_edit`/`set_byte_selection` which return `bool` (accept/reject) exactly like the C++. Keep `bool`, not `Result` — the boolean IS the contract the tests assert.
- Provider reads (byte-sel status, hover previews) return `Option`/empty on failure (the C++ silently shows nothing); never panic. `read_bytes` failure → status line omits interpretations / preview not shown.
- Internal invariants (span clamps, cursor bounds) use `debug_assert!` + saturating arithmetic; out-of-range indices clamp to `[0, line_len]` rather than panic (mirrors the C++ defensive `qMin`/`qMax`/`while` clamps). Char/byte conversions saturate at line length.
- No `thiserror` type needed in this subsystem; the app boundary (`main.rs`) uses `anyhow` for wiring, not the editor.

---

## 15. TEST PLAN (each C++ test → Rust `#[test]`)

**Strategy.** The map (§17) and tests-catalog mark `test_editor`/`test_rendered_view` as heavily UI/pixel-oriented; the *oracle* (`_oracle/RESULTS.md`) deliberately did NOT capture these (they require a display), so there is **no golden stdout** for them — they are *contract* tests, not *golden-output* tests. We translate the portable, deterministic assertions (span math, edit-lifecycle accept/reject, cursor clamp, event emission) into logic `#[test]`s that run with `--no-default-features` (no gpui). Pixel/cursor-shape assertions become assertions on the `RenderPlan`/`HitInfo`/cursor-shape enum the logic layer produces, NOT on rendered pixels.

**Shared test fixtures** (port once into `tests/editor_common.rs`):
- `make_test_tree()` — the ~PEB-like struct from `test_editor.cpp` (offsets 0x000–0x7C8, fields named InheritedAddressSpace … ExtendedFeatureDisableMask). Port the field list verbatim.
- `make_test_provider()` — `BufferProvider` of 0x7D0 bytes with the recognizable values (test_editor.cpp:65–355). Port the `w8/w16/w32/w64` writes verbatim.
- `make_ptr_demo(collapsed, null_ptr)` — Demo/ChildData pointer-expansion tree (test_editor.cpp:362–445).
- `kFirstDataLine` constant (root header suppressed; line 0 = CommandRow).
- An `EventSink` that records `Vec<EditorEvent>` (replaces `QSignalSpy`).
- Synthetic key/mouse helpers (replace `sendKey`/`sendMouseMove`/`sendLeftClick`).

### 15.1 `test_editor.cpp` → `tests/editor.rs`

| C++ slot | Rust `#[test]` | Translation / assertion |
|---|---|---|
| `testCommandRowLineRejectsEdits` (471) | `command_row_rejects_type_name_value` | `meta_for_line(0).line_kind==CommandRow`, `node_id==K_COMMAND_ROW_ID`, `node_idx==-1`; `!begin_inline_edit(Type/Name/Value,0)`; after `set_command_row_text("source▾  0xD87B5E5000")`, `begin_inline_edit(BaseAddress,0)`==true & editing; `begin_inline_edit(Source,0)`==true & NOT editing (popup). |
| `testInlineEditReEntry` (508) | `inline_edit_re_entry` | begin Name → editing; cancel → not editing; `apply_document` again; begin again ok. |
| `testCommitThenReEdit` (538) | `commit_then_re_edit` | begin Value; send Return → 1 `InlineEditCommitted`, not editing; re-apply; begin Value again ok. |
| `testMouseClickCommitsEdit` (568) | `mouse_click_commits_edit` | begin Name; synth viewport click → 1 commit, 0 cancel, not editing. |
| `testTypeEditCancel` (589) | `type_edit_emits_picker` | begin Type → 1 `TypePickerRequested`, not editing. |
| `testHeaderLineEdit` (603) | `header_line_edit` | find nested foldHead Header; Type→1 `TypePickerRequested`; Name→editing then cancel. |
| `testFooterLineEdit` (642) | `footer_line_rejects_all` | find Footer line; `!begin_inline_edit(Type/Name/Value)`. |
| `testParseValueHexWithSpaces` (662) | (belongs to `format` spec) | `fmt::parse_value` — assert in `tests/format.rs`; cross-link here. |
| `testTypeAutocompleteTypingAndCommit` (702) | `type_picker_carries_node_idx` | begin Type → `TypePickerRequested.node_idx >= 0`. |
| `testTypeEditClickAwayNoChange` (723) | `type_edit_click_away` | begin Type → picker emitted, not editing (popup owns click-away). |
| `testColumnSpanHitTest` (740) | `column_span_hit_test` | for `kFirstDataLine` (Field): `type_span`/`name_span`/`value_span(len)` all valid & start<end; for Footer: all three invalid. **Pure geometry — primary span test.** |
| `testSelectedNodeIndices` (787) | `selected_node_indices` | cursor on `kFirstDataLine` → set size 1 containing that nodeIdx. |
| `testBaseAddressDisplay` (802) | `no_base_comment_in_text` | compose with baseAddress=0x10; `!result.text.contains("// base:")`; `kFirstDataLine` is Field. |
| `testBaseAddressSpan` (823) | `command_row_addr_span_valid` | `set_command_row_text("source▾  0xD87B5E5000")`; `command_row_addr_span(line0)` valid, span text contains "0x". **Pure geometry.** |
| `testValueEditCommitUpdatesSignal` (862) | `value_edit_commit_signal` | begin Value; Home, Shift+End, type "42", Return → 1 commit with non-empty `text`. |
| `testBaseAddressEditBegins` (908) | `base_address_edit_begins` | set cmd text; `begin_inline_edit(BaseAddress,0)`==true & editing; cancel. |
| `testCursorAfterLeftClick` (926) | `cursor_arrow_after_left_click` | click at (line,col0) → cursor-shape == Arrow, not editing. Assert on the cursor-shape enum the logic returns for that `HitInfo`. |
| `testCursorShapeOverText` (940) | `cursor_ibeam_over_name_text` | over name text → IBeam; over name padding → Arrow. Assert via `cursor_shape_for(hit)` using cached row text (provide a synthetic shaped-line stub mapping col→x). |
| `testCursorShapeOverType` (967) | `cursor_pointing_hand_over_type` | over type text → PointingHand. |
| `testCursorShapeInFoldColumn` (985) | `cursor_pointing_hand_fold_col` | over col 1 of a foldHead Header → PointingHand; `in_fold_col` true. |
| `testNoIBeamAfterClickThenMove` (1020) | `no_ibeam_after_click_then_move` | click then move within padding → Arrow stays. |
| `testCommandRowRootClassEdits` (1045) | `command_row_root_type_edit` | cmd text with "class Foo {"; `begin_inline_edit(RootClassType,0)` accepted. |
| `testCommandRowRootClassName` (1060) | `command_row_root_name_edit` | `RootClassName` accepted; span covers "Foo". |
| `testRootFoldSuppressed` (1081) | `root_fold_suppressed` | root header line absent / `kFirstDataLine` is first field. |
| `testCommandRowHoverSurvivesRepeatedRefresh` (1103) | `command_row_hover_survives_refresh` | apply_document repeatedly; hover state on cmd row persists (assert hover marker present after N applies). |
| `testAccentMarkerOnSelectedRows` (1162) | `accent_marker_on_selected_rows` | `apply_selection_overlay({nodeId})` → those rows' `RowDecor.accent_bar==true` and `M_SELECTED` bg set. |
| `testDeleteClearsHeatOnShiftedNodes` (1279) | `delete_clears_heat_on_shifted_nodes` | heat-level meta diff: after a delete-shift, `RowPlan` heat spans follow the node (via `same_line` dirty-range). |
| `testHScrollResetAfterNameShrink` (2224) | `hscroll_reset_after_name_shrink` | apply wide then narrow doc; x_offset clamps to new content width. |
| `testStructTypeClickable` (2478) | `struct_type_clickable` | header type span valid & hit_test_target → Type/ArrayElementType. |
| `testStaticFieldNameEditable` (2517) | `static_field_name_editable` | static-line Name editable even though hex-ish. |
| `testStaticFieldTypeClickable` (2581) | `static_field_type_clickable` | static-line Type → picker. |
| `testStaticFieldExprEditable` (2647) | `static_field_expr_editable` | `StaticExpr` span via `static_expr_span_for`; begin editable. |
| `testStaticFieldNoSeparator` (2691) | `static_field_no_separator` | static line geometry. |
| `testAddrEditLeftArrowClampsAtStart` (2899) | `addr_edit_left_clamps_at_start` | begin BaseAddress edit; Home→col==span_start; Left ×11→stays span_start. |
| `testAddrEditRightArrowClampsAtEnd` (2925) | `addr_edit_right_clamps_at_end` | End→col==edit_end; Right ×11→stays edit_end. |
| `testAddrEditBackspaceStopsAtStart` (2951) | `addr_edit_backspace_stops_at_start` | Home; Backspace → line unchanged, col==span_start. |
| `testAddrEditDeleteStopsAtEnd` (2972) | `addr_edit_delete_stops_at_end` | End; Delete → line unchanged. |
| `testAddrEditHomeEnd` (2988) | `addr_edit_home_end` | End→edit_end; Home→span_start. |
| `testAddrEditTypingStaysInSpan` (3007) | `addr_edit_typing_stays_in_span` | select-all + type "0x8+0x8"; cursor in `[span_start, new_end]`; addr span text contains "0x8+0x8". |
| `testAddrEditClickOutsideCommits` (3036) | `addr_edit_click_outside_ends` | click a data line → not editing. |
| `testAddrEditEscapeCancels` (3053) | `addr_edit_escape_cancels` | type "FF"; Esc → not editing, 1 `InlineEditCancelled`. |
| `testAddrEditEnterCommits` (3071) | `addr_edit_enter_commits` | Return → not editing, 1 commit, non-empty text. |
| `testAddrEditFormulaSpan` (3089) | `addr_edit_formula_span` | cmd "source▾  <Reclass.exe>+0x8  class Foo {"; addr span valid, contains "<Reclass.exe>" and "+0x8". **Pure geometry.** |
| `testAddrEditVerticalKeysBlocked` (3110) | `addr_edit_vertical_keys_blocked` | Down → line stays 0. |
| `testAddrEditNoLeakRight` (3138) | `addr_edit_no_leak_right` | cmd "...0x10  class Foo {"; End, type "AB" → "class"/"Foo" intact, col ≤ edit_end. |
| `testAddrEditSelectionCollapseRight` (3162) | `addr_edit_collapse_right` | select-all; Right → col==span end. |
| `testAddrEditSelectionCollapseLeft` (3184) | `addr_edit_collapse_left` | End; Shift+Home; Left → col==span_start. |
| `testCommentEditOnNonHexField` (3211) | `comment_edit_non_hex` | `begin_inline_edit(Comment, firstData)`==true, editing; cancel. |
| `testCommentEditOnHexField` (3228) | `comment_edit_hex` | find hex Field; Comment edit ok; cursor at `edit_span_start`. |
| `testSemicolonKeyEmitsSignal` (3261) | `semicolon_emits_comment_request` | cursor on hex line; `;` key → 1 `CommentEditRequested`. |
| Popup/preview slots (`testStructPreviewPopup*`, `testPointer*`, `testDisasmPopupDismisses*`, `testMenuHoverRendersAmberText`, `testStatusBarViewToggleButtons`, `testMenuItemSizeIsAccessible`, `testResizeGripCornerSymmetry`, `testDockTabBarBorderAlignment`) | **defer to UI-integration tests** (feature `ui`) or controller/compose tests | These are gpui-overlay / pixel / dock-chrome assertions. The *eligibility logic* (which preview shows for which node kind) ports as a `#[test]` on `HoverPreview::eligible`; the *visual* parts are verified manually / via the `verify`/`run` skills, not unit tests. `testDockTabBarBorderAlignment` is already `#if 0` disabled in C++. |

### 15.2 `test_rendered_view.cpp` → `tests/rendered_view.rs`
This is the separate generated-C++ view, not the structured editor. Port as constant-table assertions:
- `rendered_view_colors` — assert `RenderedViewStyle` constants: keyword `#569cd6`, keywordset2 `#569cd6`, number `#b5cea8`, dquote/squote string `#ce9178`, comment/commentline/commentdoc `#6a9955`, default/identifier/operator `#d4d4d4`, preproc `#c586c0`, paper `#1e1e1e`, caret fg `#d4d4d4`, caret-line bg `rgb(43,43,43)`, selection bg `#264f78`, margin bg `#252526`, margin fg `#858585`.
- `rendered_view_all_styles_dark_paper` — all 128 (or our equivalent) style slots use paper `#1e1e1e`.
- `rendered_view_caret_line_enabled`, `rendered_view_brace_match_disabled` — boolean flags.
- `rendered_view_generated_code_loads` — `generator::render_cpp(tree, root)` output contains `#pragma once`, `struct TestStruct`, NOT `#pragma pack` (cross-links to `generator` spec); load into the view, style constants unchanged.

### 15.3 `bench_spam_append.cpp` behavioral assertions (NOT a strict bench)
The tests-catalog (§494) flags three real behavioral asserts inside this bench:
- `hex_dim_on_last_appended_line` — after each append, `IND_HEX_DIM` spans cover EVERY hex64 line (including the freshly appended one). Assert on `RowPlan.spans` for all hex rows.
- `controller_spam_down_preserves_all_dim` — same invariant through the editor→`AppendSingleFieldRequested`→controller→refresh loop.
- `type_cycle_colouring_correct` — per-line marker pass count matches type-change events (regression guard for the `same_line` dirty-range bug). Assert the meta-diff `[first_changed,last_changed]` range tightly tracks the changed lines.

---

## 16. Implementation order (small, independently verifiable steps)

1. **`geometry.rs`** — port `LineGeometry` + all `*_span_for` + the editor-local header/static span helpers. ✔ `column_span_hit_test`, `command_row_addr_span_valid`, `addr_edit_formula_span` (all pure). **No gpui.**
2. **`mod.rs` types** — `EditTarget`, `ColumnSpan`, `MarkerMask`, indicator-ID consts + `StyledRange`/`SpanStyle`/`RowDecor`, `EditorEvent`. Compiles standalone.
3. **`render_plan.rs` (data only)** — `RenderPlan`/`RowPlan`; `apply_document` minus gpui (build plan from `ComposeResult`, diff-and-patch the rope, `same_line` meta-diff, indicator/marker passes, footer pills, chip indicators, heat). ✔ `delete_clears_heat_on_shifted_nodes`, `hex_dim_on_last_appended_line`, `type_cycle_colouring_correct`, `accent_marker_on_selected_rows`.
4. **`edit_state.rs`** — `InlineEditState`, `begin_inline_edit` (dispatch+reject), `resolved_span_for`, `normalize_span`, `edit_end_col`, `clamp_edit_selection`, `commit_inline_edit`, `cancel_inline_edit`, `end_inline_edit`. ✔ all `command_row_*`, `header_line_edit`, `footer_line_rejects_all`, `comment_edit_*`, `type_edit_emits_picker`, `value_edit_commit_signal`, the full `addr_edit_*` clamp suite, re-entry/commit tests.
5. **`keymap.rs`** — `handle_normal_key`/`handle_edit_key`/`handle_hex_edit_key` against the synthetic `KeyInput`. ✔ `semicolon_emits_comment_request`, `addr_edit_vertical_keys_blocked`, `addr_edit_no_leak_right`, collapse selection tests.
6. **`byte_sel.rs`** — `ByteSel`, `byte_addr_at`, overlay-paint plan, `update_byte_sel_status` interpretation, extend/snap/select-all, `begin_byte_edit`/`advance_to_byte_segment`. ✔ `set_byte_selection` invariant, status interpretations, multi-row segments.
7. **`hit_test.rs`** — `HitInfo`, `hit_test_target` char-col logic + `cursor_shape_for(hit)`. ✔ cursor-shape tests (feed synthetic shaped-line col→x stub).
8. **`element.rs`** [cfg ui] — the raw-gpui `Element`: `request_layout`/`prepaint`/`paint`, `shape_line` per visible row from `RowPlan`, span→`TextRun` + BoxUnder quads, hitboxes, `EntityInputHandler` bridge for the active edit field, pixel→`HitInfo`. (Manual `verify`/`run` checks; no unit test.)
9. **`view.rs`** [cfg ui] — `EditorState` entity, `uniform_list` of rows + `track_scroll`, find bar, theme via `cx.theme()`, focus/Tab cycling, the event channel to the controller, dwell/glow timers, smooth scroll animation.
10. **`previews/`** [cfg ui] — `HoverPreview` trait + registry + 4 previews + `HoverPopupHost`. ✔ `HoverPreview::eligible` unit tests; visuals manual.
11. **Rendered-C++ view** — `RenderedViewStyle` constants + `tests/rendered_view.rs`.

Steps 1–7 are pure logic, run under `--no-default-features`, and cover the overwhelming majority of the `test_editor.cpp` contract. Steps 8–11 are the gpui surface, validated by compile-green + the `verify`/`run` skills (no golden oracle exists for them per `_oracle/RESULTS.md`).

---

## 17. Parity checklist (the 13 subtle behaviors from map §17 — each must hold)

1. CommandRow line 0 rejects Type/Name/Value; only BaseAddress/Source/RootClassName editable. (`command_row_rejects_type_name_value`)
2. Hex nodes block Name/Value (except static-field names); only Type editable; dbl-click/Enter → hex-overwrite. (`comment_edit_hex`, edit_state gate)
3. Footer lines reject all inline edits. (`footer_line_rejects_all`)
4. Header lines: Type opens picker, Name editable; anonymous keywords + `[N]` names not editable. (`header_line_edit`)
5. Inline edit forces full-replace next refresh (clears `prev_text`) and restores trailing padding byte-identically. (`end_inline_edit` + a `round_trip_padding` test)
6. Selection survives layout shifts via node-id anchoring. (`restore_view_state` test with shifted lines)
7. Byte selection is address-based, half-open, survives refresh; `set_byte_selection` enforces `lo<hi`. (`set_byte_selection` test)
8. Tab cycles edit targets in fixed order, skipping inapplicable; `last_tab_target` persists. (`tab_cycle_targets` test)
9. Live value validation toggles red error bg + comment hint only on state change. (`validate_edit_live` test)
10. Per-byte heat: only `changed_byte_indices` colored. (covered by step-3 heat asserts)
11. Hover preview dwell = 700ms; arrow tooltips NOT dwell-gated. (timing — manual/UI test)
12. Find highlights every match + steps cursor; persists across hide/reopen. (`find_highlights_all` logic test on the match-set)
13. Cursor shapes per region. (`cursor_*` tests)

---

## SUMMARY

The editor surface is the single most complex UI file; the port splits it into a **large always-compiled pure-logic core** (`geometry.rs` span math, `edit_state.rs` inline-edit lifecycle, `byte_sel.rs`, `hit_test.rs`, `render_plan.rs` with the diff-and-patch + `same_line` meta-diff) and a **thin gpui rendering shell** (`element.rs`/`view.rs`/`previews/`) built on the raw-gpui custom `Element` pattern (`shape_line` + per-span `TextRun` + `EntityInputHandler` + `paint_quad`, with `uniform_list` virtualization) — keeping gpui-component only for the chrome around it (find-bar input, type pickers, context menus, tooltips). All 18 Scintilla indicators become z-ordered `StyledRange`s (z = indicator ID), markers become per-row `RowDecor`, and margins become a gutter; the editor stays strictly stateless w.r.t. the model, emitting one `EditorEvent` enum (the 45 Qt signals) and never mutating the `NodeTree`. The TEST PLAN maps every portable `test_editor.cpp` slot (the span-validity, command-row/header/footer edit gating, the full `testAddrEdit*` caret-clamp suite, commit/cancel/re-entry, cursor shapes, semicolon/comment edits, accent markers, heat-on-shift) and `test_rendered_view.cpp` (constant-table color checks) into `--no-default-features` logic `#[test]`s using ported `make_test_tree`/`make_test_provider` fixtures and an `EventSink` replacing `QSignalSpy`; there is intentionally **no golden oracle** for these UI tests (`_oracle/RESULTS.md` captured only headless logic targets), so they are contract tests, and the gpui surface is validated by compile-green + manual `verify`/`run`.

**Spec path:** `/home/loke/reclass-rs/_design/specs/PORTING_editor-surface.md`
