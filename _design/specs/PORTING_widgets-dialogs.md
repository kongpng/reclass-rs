# PORTING SPEC — Widgets, Dialogs, Popups, Panels (`widgets-dialogs`)

Function-level porting spec to drive the faithful Rust implementation of Reclass's themed-UI
subsystem. Authoritative inputs: `_design/understand/widgets-dialogs.md` (behavioral map),
`_design/ARCHITECTURE.md` (one-package layout), `_design/crate_selection.md` (crates),
`_oracle/RESULTS.md` (golden outputs), `_design/gpui_component_cookbook.md` (which gpui-component
backs each surface), `_design/gpui_cookbook.md` (raw-gpui patterns). C++ source at
`/home/loke/reclass-cpp/`; citations are `path:line` relative to that root.

> **Read order for the implementer:** §0 (scope + crate/module map) → §1 (fuzzy scorers, the one
> piece every popup depends on) → §2 (pure-logic items: clipboard, recent-list, parseTypeSpec,
> profiler, text-report builders — these are oracle-testable headless) → §3 (themed-widget value
> structs) → §4 (the GPUI surfaces) → §5 (platform/`#[cfg]`) → §6 (error handling) → §7 (TEST PLAN)
> → §8 (step ordering) → §9 (open questions).

---

## 0. Target crate / modules, and the headless/UI split

**One package `reclass`, modules under `src/`** (ARCHITECTURE.md §2). This subsystem lands almost
entirely in **`src/ui/`** behind the default `ui` feature — EXCEPT a handful of pure-logic items
that have **no gpui dependency** and MUST be reachable in `--no-default-features` headless tests
(that is how the oracle ran `test_clipboard` GUILESS). Place those in non-`ui` modules:

| Item | Rust module | Feature gate | Why |
|---|---|---|---|
| `ClipboardCodec` (serialize/deserialize/collectSubtrees/parseLenientHex/plainDump) | `src/core/clipboard.rs` | **always** (no `ui`) | `test_clipboard` is headless `QTEST_GUILESS_MAIN`; depends only on `core::{Node,NodeTree}` + serde_json. Oracle PASS 9/9. |
| `rcx::fuzzy_score` (the two-pass scorer) | `src/ui/fuzzy.rs` … but the **function** is `pub` and gate-free | always | Used by type/enum popups AND testable headless. Keep the scoring fn out of any gpui type so `--no-default-features` can test it. |
| `CommandPalette::fuzzy_score` | `src/ui/command_palette.rs` (fn `pub`, gate-free) | always | `test_command_palette` fuzzy slots are pure logic. |
| `SourceChooserPopup::fuzzy_score` | `src/ui/source_chooser.rs` (fn `pub`, gate-free) | always | pure logic. |
| `parse_type_spec` + `TypeSpec` | `src/ui/type_selector.rs` (fn/struct `pub`, gate-free) OR `src/core/typespec.rs` | always | `test_type_selector` parseTypeSpec slots are pure logic; controller also parses inline edits. **Recommend `src/core/typespec.rs`** so controller can use it without `ui`. |
| `Profiler` + `ProfileStats` + `ProfileScope`/`profile_scope!` | `src/profiler.rs` | always | concurrency primitive used app-wide; not UI. |
| Recent-list helpers (`load_recent`/`push_recent`/`clear_recent`) | `src/ui/goto_address.rs` (fns `pub`, but back them with a `Settings` store, see §2.2) | `ui` for the dialog; the **functions** can be `ui`-gated since `test_goto_address` is a `G` (display) test | — |
| `RttiBrowserDialog::build_text_report` | `src/ui/rtti_browser.rs` (fn `pub`, gate-free) | always | pure string builder; cheap to test headless. |
| `WorkspaceModel` builders (`build_project_explorer`, `type_display_string`, `is_hex_pad`, `build_struct_children`) | `src/ui/workspace_model.rs` (logic `pub`, gate-free) | logic gate-free; the delegate paint is `ui` | model-building is pure; only the delegate touches gpui. |
| `ScannerPanel` results-JSON + settings codecs | `src/ui/scanner_panel.rs` (codec fns gate-free) | codec gate-free; panel `ui` | the two serialization formats are pure and worth headless tests. |

Everything else (`ThemedDialog`, `DialogButton`, `ThemedMessageBox`, `ThemedInputDialog`,
`OptionsDialog`, `ProcessPicker`, `GotoAddressDialog` UI, `CommandPalette` UI, `TypeSelectorPopup`,
`EnumPickerPopup`, `SourceChooserPopup`, `HexToolbarPopup`, `CategoryChip`, `RcxTooltip` +
`GlobalTooltipBridge`, `RttiBrowserDialog` UI, `ScannerPanel` UI, `ProfilerDialog`,
`WorkspaceDelegate`, `HoverPreviewRegistry`, `drawTabSourceIcon`) is `#[cfg(feature = "ui")]`.

**Proposed `src/ui/` file layout (mirrors C++ `src/widgets/*` + the loose dialog/popup files):**

```
src/ui/
├─ mod.rs                 # declares the below; re-exports public surfaces
├─ fuzzy.rs               # rcx::fuzzy_score (two-pass)            ← widgets/fuzzy_match.h
├─ theme_apply.rs         # ReclassTheme → gpui-component Theme    ← (themes subsystem boundary)
├─ widgets/
│  ├─ mod.rs
│  ├─ dialog_button.rs    # DialogButton element                  ← widgets/dialog_button.*
│  ├─ themed_dialog.rs    # button-row helper + modal shell trait  ← widgets/themed_dialog.*
│  ├─ themed_messagebox.rs# AlertDialog wrappers                   ← widgets/themed_messagebox.*
│  ├─ themed_input.rs     # getText/getInt/getItem                 ← widgets/themed_inputdialog.*
│  ├─ category_chip.rs    # toggle pill Element                    ← widgets/category_chip.h
│  ├─ hover_preview.rs    # HoverPreview trait + registry          ← widgets/hover_preview.h
│  └─ enum_picker_popup.rs# EnumPickerPopup                        ← widgets/enum_picker_popup.h
├─ options_dialog.rs      # OptionsDialog + OptionsResult          ← optionsdialog.*
├─ process_picker.rs      # ProcessPicker + ProcessInfo + enum     ← processpicker.*
├─ goto_address.rs        # GotoAddressDialog + recent helpers     ← gotoaddressdialog.h
├─ command_palette.rs     # CommandPalette + Entry + fuzzy         ← commandpalette.h
├─ type_selector.rs       # TypeSelectorPopup + TypeEntry + delegate← typeselectorpopup.*
├─ source_chooser.rs      # SourceChooserPopup + SourceEntry + fuzzy← sourcechooserpopup.*
├─ hex_toolbar.rs         # HexToolbarPopup (custom-painted)       ← hextoolbarpopup.*
├─ rcx_tooltip.rs         # RcxTooltip + GlobalTooltipBridge       ← rcxtooltip.h, tooltip_bridge.h
├─ rtti_browser.rs        # RttiBrowserDialog + build_text_report  ← rttibrowser.h
├─ scanner_panel.rs       # ScannerPanel + codecs                  ← scannerpanel.*
├─ profiler_dialog.rs     # ProfilerDialog + BarChart              ← profilerdialog.*
├─ workspace_model.rs     # WorkspaceModel/Proxy/Delegate          ← workspace_model.h
└─ tab_source_icon.rs     # draw_tab_source_icon                   ← tab_source_icon.h
```
(`Profiler` lives at `src/profiler.rs`; `parse_type_spec`/`TypeSpec` and `ClipboardCodec` at
`src/core/`.)

**Cross-cutting Qt→Rust/GPUI mapping** (full table in widgets-dialogs.md §0). Key decisions reused:
`QDialog`/`ThemedDialog` → gpui-component `Dialog`/`AlertDialog` rendered through `Root`; frameless
auto-dismiss `QFrame` popup → `Popover` shell (gpui-component) or raw `deferred(anchored())` overlay
(gpui_cookbook §8) with `.on_mouse_down_out(...)`; `QLineEdit` → `TextInput`+`InputState`;
`QListView`+delegate → `uniform_list` of custom `RenderOnce` rows; `QTableWidget` → `DataTable`;
`QPainter` custom paint → raw-gpui `Element` (`window.text_system().shape_line` + `paint_quad`);
`QSettings("Reclass","Reclass")` → a `Settings` store keyed by org/app with **key paths preserved
verbatim**; theme colors → direct `ReclassTheme` struct fields (no QPalette layer);
`QSvgRenderer`+`SourceIn` tint → tinted-SVG draw helper; `QKeySequence`/`QAction` → command registry;
`QMimeData` clipboard → custom MIME `application/x-reclass-nodes-v1`.

**Theme token names** (from the themes subsystem `ReclassTheme`, ≈31 fields): `background`,
`backgroundAlt`, `surface`, `text`, `textDim`, `textMuted`, `textFaint`, `border`, `borderFocused`,
`hover`, `selected` (row-select bg), `selection` (text-select bg), `indHoverSpan` (accent purple),
`markerPtr` (red), `markerError`, `indHeatHot/Warm/Cold`, `markerCycle` (amber),
`syntaxKeyword/Type/String/Number/Comment/Preproc`, `indDataChanged`, `button`.
**`selected` ≠ `selection`** — a recurring footgun the tests guard (`test_options_dialog.cpp:151`).

---

## 1. The three fuzzy scorers (§13 of the map) — port verbatim

`crate_selection.md` and ARCHITECTURE.md §10 are **authoritative over the map's `nucleo` aside**:
**HAND-ROLL the scorers.** Ranking and the emitted match-position arrays are observable behavior
(they drive per-character highlight painting and result ordering that tests pin). Do NOT substitute
`nucleo` — its ranking differs and would break `test_command_palette` and the type/source popup
ordering. (If a library is ever wanted later it would have to reproduce these exact priority tables;
not worth it.) All three are ASCII-fold-to-lowercase scorers.

### 1.1 `rcx::fuzzy_score` — `src/ui/fuzzy.rs` (← `widgets/fuzzy_match.h:34`)

```rust
pub const MAX_FUZZY_LEN: usize = 64; // kMaxFuzzyLen (advisory; not enforced as a hard cut in C++)

/// Two-pass strict matcher. Returns (score, positions) — score 0 = no match.
/// `out_positions` are char indices into `text` (NOT byte offsets) for highlight painting.
pub fn fuzzy_score(pattern: &str, text: &str) -> (i32, Vec<usize>);
```
Behavior (mirror `fuzzy_match.h:34-116` exactly; operate over `Vec<char>` to match Qt `QChar`
indexing):
- `p_len == 0` → `(1, [])`. `p_len > t_len` → `(0, [])`. `t_len > 4096` → `(0, [])`.
- **Pass 1 — contiguous case-insensitive substring** (`text.indexOf(pattern, CaseInsensitive)`):
  if found at `idx`, positions = `idx..idx+p_len`. Score = `1000`
  + `if idx==0 {500}` else `{ if prev ∈ {'_',' ',':','.','-'} {200} else if text[idx].is_upper() && prev.is_lower() {150} else {0} }`
  + `max(0, 100 - (t_len - p_len))` + `if p_len==t_len {200}`.
- **Pass 2 — word-start initials.** `matchable(i)`: `i==0` OR `prev ∈ {'_',' ',':','.','-'}` OR
  (`c.is_upper() && prev.is_lower()`) OR (`c.is_digit() && prev.is_alphabetic()`) OR
  (`c.is_alphabetic() && prev.is_digit()`) OR `c.is_digit()`. Walk pattern (lowercased) left→right;
  each pattern char must hit the next matchable position with equal lowercased char; any miss → `(0,[])`.
  Score = `if hits[0]==0 {600} else {400}` + `max(0, 50 - (span - p_len))` where `span = hits.last-hits.first+1`;
  final `max(1, score)`.
- **Casing/digit predicates:** use Unicode-aware `char::is_uppercase/is_lowercase/is_ascii_digit/is_alphabetic`
  to match QChar semantics; the test inputs are ASCII so ASCII predicates suffice but Unicode-safe is fine.

### 1.2 `CommandPalette::fuzzy_score` — `src/ui/command_palette.rs` (← `commandpalette.h:132`)

```rust
pub fn command_palette_fuzzy(needle: &str, haystack: &str) -> i32;
```
Linear single pass (no positions). `needle.is_empty()` → `1`. Walk both lowercased; on equal char
add bonus `1 + if run>0 {2} + if prev_sep {3} + if hi==ni {1}` (run = current contiguous run length,
`prev_sep` starts **true**, updated each haystack step to `h ∈ {' ','>','/','_','-'}`, `ni`/`hi` are
needle/haystack indices). On mismatch `run=0` (do NOT advance `ni`). After the loop, if `ni < needle.len()`
return `0`. **Test-locked:** empty needle → >0; `"open"` vs `"File > Open"` (word-start) outscores
`"open"` vs `"Reclass > Reopen Last"`; `"xyz"`/`"File > Open"` → 0; `"fs"`/`"File > Save"` > 0;
`OPEN`==`open` case-insensitive (`test_command_palette.cpp:18-51`).

### 1.3 `SourceChooserPopup::fuzzy_score` — `src/ui/source_chooser.rs` (← `sourcechooserpopup.cpp:25`)

```rust
pub fn source_chooser_fuzzy(pattern: &str, text: &str) -> (i32, Vec<usize>);
```
Recursive backtracking with branch cap 4 (`kMaxFuzzyLen=64`; `t_len` cap 256 → beyond that fall back
to a contiguous-prefix check). Per-char bonus: `10` at idx0, `8` after `_`/space, `8` at CamelCase
boundary; `+5` for contiguous (`i == prev_pos+1`); `+ max(0, 20 - (t_len - p_len))` tightness; `+20`
exact length. Returns the **best-scoring** match positions. Port the recursion faithfully (depth-limited
branching mirrors the C++); it is not on the hard oracle path but drives source-popup ordering.

---

## 2. Pure-logic items (headless-testable, oracle-relevant)

### 2.1 `ClipboardCodec` — `src/core/clipboard.rs` (← `clipboard.h`)

C++ is `struct ClipboardCodec` with static methods + a `PasteResult`. In Rust make it a module of free
functions (no `QApplication`/`QClipboard` dependency — callers do the OS clipboard roundtrip, exactly
as the C++ keeps it `QtCore`-only). MIME constant:
```rust
pub const MIME_TYPE: &str = "application/x-reclass-nodes-v1"; // kMimeType (clipboard.h:33)

pub struct PasteResult { pub nodes: Vec<Node>, pub root_ids: Vec<u64> }
```

| C++ | Rust signature | Behavior notes |
|---|---|---|
| `collectSubtrees(tree, roots)` | `pub fn collect_subtrees(tree: &NodeTree, roots: &[u64]) -> Vec<Node>` | iterative DFS, **cycle-safe** via `HashSet<u64> seen`; stack starts = `roots` (clone); pop, skip if seen, `tree.index_of_id`; push node clone + `tree.children_of(id)` ids. **Order = the C++ stack order** (`takeLast` = LIFO). Preserve it for byte-exact JSON node order. |
| `serialize(tree, rootIds, clearParentFor={})` | `pub fn serialize(tree: &NodeTree, root_ids: &[u64], clear_parent_for: &HashSet<u64>) -> ClipboardPayload` | returns a struct carrying both the JSON bytes and the plain-text. Build `serde_json::json!({ "schema":"rcx-clipboard/v1", "roots":[root id strings], "nodes":[Node::to_json...] })`; for ids in `clear_parent_for` set `parentId` to `"0"` **(string, matching `QString::number(0)`)**. Compact JSON (`QJsonDocument::Compact`). Plain text = `plain_dump(tree, root_ids)`. Return `ClipboardPayload { mime_json: Vec<u8>, text: String }`; the caller wraps into the OS clipboard (gpui: `cx.write_to_clipboard(ClipboardItem::new_string(text))` + a typed entry; see §4.note). |
| `deserialize(tree&, mime)` | `pub fn deserialize(tree: &mut NodeTree, mime_json: Option<&[u8]>) -> PasteResult` | None / not-our-format → empty result. Parse JSON; require object; require `schema=="rcx-clipboard/v1"`. Parse `nodes` via `Node::from_json`; **empty → empty result**. Build `HashMap<u64,u64> idMap` mapping each old `node.id → tree.reserve_id()` **in node array order**. Rewire each node: `id=map[id]`, `parentId=map.get(parentId).unwrap_or(parentId)`, `refId=map.get(refId).unwrap_or(refId)` (**0 stays 0** because 0 is never inserted into the map). `root_ids` = each `roots` string parsed as u64 → `map.get(...).unwrap_or(0)`. |
| `parseLenientHex(src, err)` | `pub fn parse_lenient_hex(src: &str) -> Result<Vec<u8>, String>` | see algorithm below. |
| `plainDump(tree, rootIds)` | `pub fn plain_dump(tree: &NodeTree, root_ids: &[u64]) -> String` | join `dump_node` lines with `'\n'`; skip roots with `index_of_id < 0`. |
| `dumpNode(tree, idx, depth, out)` | private `fn dump_node(tree, idx, depth, out: &mut Vec<String>)` | line = `format!("{indent}+0x{:04x}  {:<8}  {}", offset, kind_to_string(kind), name)` where `indent = " ".repeat(depth*2)`. **`{:<8}` = left-justify kind name to width 8** (matches `.arg(name, -8)`); hex offset is **4-wide zero-padded lowercase** (`offset, 4, 16, '0'`). Recurse over `children_of`. |

**`parse_lenient_hex` algorithm** (mirror `clipboard.h:148-209`; the contract is exact):
```
tok = String::new()           // current token
out = Vec<u8>::new()
is_hex(c) = ascii_digit | a-f | A-F
for c in src.chars():
    if is_hex(c):                          tok.push(c); continue
    if (c=='x'||c=='X') && tok == "0":     tok.push(c); continue   // keep "0x" to strip in flush
    if c.is_alphabetic():                  return Err("Invalid hex digit '{c}'")  // stray/mid-token letter
    // any other char (space/punct/control) → separator → flush
    flush(&mut tok, &mut out)?              // propagates Err
flush(&mut tok, &mut out)?                 // final
if out.is_empty(): return Err("No hex data")
Ok(out)

flush(tok, out):
    if tok.is_empty(): return Ok
    if tok.len() > 2 && tok starts_with "0x"/"0X": strip first 2 chars
    for c in tok: if !is_hex(c): return Err("Invalid hex digit '{c}'")  // a surviving 'x' fails here
    if tok.len() is odd: prepend '0'                                    // "A" → "0A" => 0x0A, not 0xA0
    for each 2-char window step 2: push u8::from_str_radix(pair,16)
    tok.clear(); Ok
```
Locked cases (from the header doc): `"DE AD BE EF"`, `"DEADBEEF"`, `"0xDEADBEEF"`,
`"{0xDE, 0xAD, 0xBE}"`, `"DE,AD"`, `"1 2 3"`→`01 02 03`, `"0x100"`→`01 00`. Note: the C++ writes
`"No hex data"` into `*err` only when err was empty; the Rust `Result` makes empty-input vs
malformed unambiguous.

**Serde note:** reuse `Node`'s existing serde from `core` (do NOT fork the field schema — same as C++
reusing `Node::toJson`/`fromJson`). IDs serialize as **decimal strings** (`QString::number`), matching
`core`'s `.rcx` convention. The clipboard schema string `"rcx-clipboard/v1"` is distinct from the MIME
type `application/x-reclass-nodes-v1` (the map's §0 note) — keep both literals exact.

### 2.2 GotoAddress recent-list helpers — `src/ui/goto_address.rs` (← `gotoaddressdialog.h:123-142`)

`QSettings("Reclass","Reclass")` key `"gotoAddress/recent"` (a `QStringList`), cap 12.
Back this with the app's `Settings` store (same store the OptionsDialog/ScannerPanel use); **preserve
the literal key path `"gotoAddress/recent"`**. Most-recent-first; dedup-to-top; cap by dropping the tail.
```rust
pub const SETTINGS_KEY: &str = "gotoAddress/recent";
pub const MAX_RECENT: usize = 12;
pub fn load_recent(settings: &Settings) -> Vec<String>;            // value or empty
pub fn push_recent(settings: &mut Settings, entry: &str);          // trim; ignore empty; remove_all+prepend; truncate to MAX_RECENT
pub fn clear_recent(settings: &mut Settings);
```
Tests (`test_goto_address.cpp:84-124`): 3 pushes → 3 entries most-recent-first; dup moves to top
(len shrinks); 30 pushes → capped at 12. For headless testing, make `Settings` injectable (a trait or
an in-memory impl) so these run without a real registry/ini. (The dialog UI itself is a `G`/display
test; only the recent-list logic is trivially headless.)

### 2.3 `parse_type_spec` + `TypeSpec` — `src/core/typespec.rs` (← `typeselectorpopup.cpp:32-61`)

```rust
#[derive(Default, Debug, Clone, PartialEq)]
pub struct TypeSpec { pub base_name: String, pub is_pointer: bool, pub ptr_depth: i32, pub array_count: i32 }
pub fn parse_type_spec(text: &str) -> TypeSpec;
```
Algorithm: `s = text.trim()`; empty → default. If `s.ends_with('*')`: `is_pointer=true`, chop one `*`,
`ptr_depth=1`; if still ends `*`, chop, `ptr_depth=2`; `base_name = remaining.trim()`; return. Else find
`'['`: if its index `> 0` **and** `s.ends_with(']')`: `base_name = s[..bracket].trim()`,
`count = s[bracket+1 .. len-1].parse::<i32>()`; **`array_count = count` only if parsed OK and `count > 0`**
(so `[0]` and non-numeric leave `array_count = 0`). Else `base_name = s`.
Locked cases (`test_type_selector.cpp:597-1103`): `"int32_t"`→{int32_t,0,0}; `"int32_t[10]"`→count 10;
`"Ball*"`→ptr1; `"Ball**"`→ptr2; `""`→empty; `"  Ball *  "`→pointer base "Ball"; `"int32_t[0]"`→count 0;
`"int32_t*"`→ptr1; `"f64**"`→ptr2. (Note `"  Ball *  "`: after trim it is `"Ball *"`, ends `*`, chop→`"Ball "`,
trim→`"Ball"`. Matches C++.)

### 2.4 `Profiler` — `src/profiler.rs` (← `profiler.h`, `profiler.cpp`)

```rust
#[derive(Clone, Copy)]
pub struct ProfileStats { pub total_ns: i64, pub min_ns: i64, pub max_ns: i64, pub last_ns: i64, pub count: i64 }
impl Default for ProfileStats { /* min_ns = i64::MAX, rest 0 */ }

pub struct Profiler { enabled: AtomicBool, stats: Mutex<HashMap<&'static str, ProfileStats>> }
impl Profiler {
    pub fn instance() -> &'static Profiler;            // OnceLock singleton
    pub fn set_enabled(&self, on: bool);               // Relaxed store
    pub fn is_enabled(&self) -> bool;                  // Relaxed load
    pub fn record(&self, name: &'static str, nanos: i64); // early-return if !enabled; else lock + update
    pub fn snapshot(&self) -> HashMap<&'static str, ProfileStats>; // locked clone
    pub fn reset(&self);                               // locked clear
}

pub struct ProfileScope { name: &'static str, active: bool, start: Option<Instant> }
// new(name): active = instance().is_enabled(); if active { start = Some(Instant::now()) }
// Drop: if active { instance().record(name, start.elapsed().as_nanos() as i64) }
macro_rules! profile_scope { ($name:literal) => { let _g = $crate::profiler::ProfileScope::new($name); } }
```
`record` update under lock: `e = stats.entry(name).or_default(); e.total_ns += n; e.min_ns = min(e.min_ns,n);
e.max_ns = max(e.max_ns,n); e.last_ns = n; e.count += 1;`. **Disabled fast-path must stay a single
relaxed atomic load + early return** (the C++ documents ~2ns when off). Key is `&'static str` (C++
captures the literal by pointer; using `&'static str` keeps that semantics and is `Eq` by content).

### 2.5 `RttiBrowserDialog::build_text_report` — `src/ui/rtti_browser.rs` (← `rttibrowser.h:139`)

```rust
pub fn build_text_report(info: &RttiInfo) -> String;
```
Plain string, port verbatim for parity (used by the "Copy as tree" button). Lines (no trailing space):
`Class: {demangled|raw}\n`; `ABI:   {abi}\n` (only if non-empty); `Raw:   {raw}\n` (only if non-empty);
`Vtable: 0x{vtable_address:x}\n`; `Module: {module}\n` (only if non-empty); `COL:    0x{col:x}\n`;
`Offset: {offset}\n\n`; `Hierarchy ({n}):\n` then each base `  {demangled|raw}\n`; `\nVtable ({n}):\n`
then each slot `  [{slot:2}] 0x{address:x}  {symbol|"(no symbol)"}\n`. **`{slot:2}` = right-aligned
width 2** (`.arg(m.slot, 2)`). `RttiInfo` comes from the `rtti` subsystem.

---

## 3. Themed-widget value structs & helpers

### 3.1 `ReclassTheme` injection (no QPalette)
The C++ `ThemedDialog::applyTheme` maps theme→`QPalette`. In GPUI there is no palette layer: every
render reads `cx.theme()` (a `ReclassTheme` Global, gpui_cookbook §6) and applies fields directly.
**Drop the entire palette-translation code path.** The themes subsystem owns `ReclassTheme` and the
`ThemeRegistry`/`Theme::change` runtime-switch (themes spec). This subsystem only *consumes* it.

### 3.2 `DialogButton` — `src/ui/widgets/dialog_button.rs` (← `dialog_button.*`)
A `RenderOnce` component (not a stateful view). gpui-component `Button` exists but the resting/hover/
pressed/disabled color rules are specific, so render a styled `div` with `.id` + `.on_click` to match
exactly (or use `Button` with `.ghost()` and override colors — but the explicit rules below are clearer).
```rust
pub enum DialogVariant { Primary, Secondary, Destructive }
pub struct DialogButton { id: ElementId, label: SharedString, variant: DialogVariant, icon: Option<IconName>, default: bool, enabled: bool }
```
Fixed height **30px**, square corners (`border_radius:0`), icon 14×14, IBeam→pointing-hand cursor.
Color rules (`dialog_button.cpp:31-110`), all resting state **outline-only, transparent fill**:
- **Primary**: fg=`text`, border=`borderFocused`; `default` flag draws the focus/default border = `borderFocused`.
- **Secondary**: fg=`textDim`, border=`border`.
- **Destructive**: `warn = if markerPtr valid { markerPtr } else { indHeatHot }`; fg=border=`warn`.
- Hover: fill=`hover`; hover fg = `text` (Primary/Secondary) or `warn` (Destructive).
- Pressed: fill = `hover` darkened to **85%** (`hover.darker(115)` → multiply RGB by 100/115 ≈ ×0.87;
  reproduce Qt's `QColor::darker(factor)` = divide HSV value by factor/100, i.e. `v' = v*100/115`).
- Focus/default border = `borderFocused` (Primary/Secondary) or `warn` (Destructive).
- Disabled: transparent fill, fg=`textMuted`, border=`border`.
- Padding `5px 16px`, `min-width:88px`, font-weight 500.
Subscribes to theme-change in C++; in GPUI re-render reads `cx.theme()` fresh each frame — no subscription.

### 3.3 `ThemedDialog` button-row helper — `src/ui/widgets/themed_dialog.rs` (← `themed_dialog.*`)
The palette job collapses (§3.1). Keep only the button-row helper:
```rust
/// Right-aligned row: a flex_row with leading flex_1 spacer then buttons in caller order.
/// Convention {Cancel, OK}. Margins (0,8,0,0), gap 8px.
pub fn button_row(buttons: impl IntoIterator<Item = AnyElement>) -> Div;
```
Modal shell: gpui-component `Dialog` rendered through `Root`; keep the native OS title bar
deliberately (C++ uses `Qt::Dialog`) — i.e. open a normal window/modal, do not draw a custom title bar
for dialogs.

### 3.4 `ThemedMessageBox` — `src/ui/widgets/themed_messagebox.rs` (← `themed_messagebox.*`)
Map to gpui-component `AlertDialog` (`dialog/alert_dialog.rs`). **Severity icon is removed** (C++ keeps
`m_iconLbl` null); title+text convey severity. Returns are blocking in Qt (`exec()`); in GPUI dialogs
are async — model as a future/callback that resolves to the choice. Provide:
```rust
pub enum UnsavedChoice { Save, Discard, Cancel }
pub async fn info(window, cx, title, text);        // one Primary "OK", default-focused
pub async fn warn(window, cx, title, text);
pub async fn critical(window, cx, title, text);
pub async fn confirm(window, cx, title, text, accept_label, reject_label/*default "Cancel"*/, destructive: bool) -> bool;
pub async fn unsaved_changes(window, cx, title, text, detail: &str) -> UnsavedChoice;
```
- `confirm`: Cancel(Secondary) + accept(Primary, or **Destructive** if `destructive`). **When destructive,
  default focus → Cancel** (a stray Enter is safe). Returns `accepted`.
- `unsaved_changes`: Cancel(Secondary) / Discard(Destructive) / Save changes(Primary, default).
- `set_detail_text(detail)`: split on `\n` (skip empty parts). **If > 5 items → scrollable read-only list**
  (no focus, no selection, arrow cursor, fixed height = `row_h*5 + 6`, vertical scrollbar as-needed).
  **Else → a single word-wrapped label** in `textDim`. Inserted just above the button row.
- Layout: outer column margins `24,22,24,18` gap 18; text row word-wrapped, selectable, font +0.5pt;
  button row margins `0,4,0,0` gap 12, leading spacer. **Width clamped `[420,640]`.**
- Wording conventions (header doc) are author guidance — preserve the existing message strings verbatim
  at call sites. `QFileDialog`/`QColorDialog` intentionally NOT wrapped → use the OS file/color pickers.

### 3.5 `ThemedInputDialog` — `src/ui/widgets/themed_input.rs` (← `themed_inputdialog.*`)
Three async functions returning `Option<T>` (None = dismissed):
```rust
pub async fn get_text(window, cx, title, label, default_text, placeholder) -> Option<String>; // min width 380; pre-select-all; Enter accepts
pub async fn get_int(window, cx, title, label, value, min, max) -> Option<i32>;                // min width 320; NumberInput range/value
pub async fn get_item(window, cx, title, label, items: &[String], current_index: usize) -> Option<String>; // min width 360; combo
```
Each = an ephemeral `Dialog` (margins `20,18,20,14` gap 10) + word-wrapped label (`text`) + input
(`TextInput`/`NumberInput`/`Combobox`) + OK(Primary,default)/Cancel row; input focused.
Focus border=`borderFocused`; combo selection bg=`selected`, popup view `backgroundAlt`/`text`.

### 3.6 `CategoryChip` — `src/ui/widgets/category_chip.rs` (← `category_chip.h`)
Custom-painted toggle pill (`RenderOnce` or small `Element`); checkable, checked by default,
pointing-hand cursor.
```rust
pub struct CategoryChip { label, count: Option<i32>, total: Option<i32>, group_color: Option<Hsla>, checked: bool }
```
- `chip_text()`: `label`; or `"label (count)"`; or `"label (visible/total)"` when `total != count` and total set.
- `size_hint`: width `5 + 4 + text_width + 16`, height `fm.height() + 4`.
- Paint: hover → fill `hover`; 5×5 pip (group color if checked else `textFaint`) + gap 4 + text
  (group color if checked else `textMuted`), centered block, baseline-aligned, **antialiasing OFF**
  (crisp pixel pip). `group_color` defaults to `textMuted` if unset.

---

## 4. The GPUI surfaces

> General GPUI patterns: stateful views implement `Render` (`Entity<T>`); popups use
> `Popover`/`deferred(anchored())` + `.on_mouse_down_out` dismiss (gpui_cookbook §8); filtered lists =
> `TextInput` query + `uniform_list` of custom rows recomputed on each keystroke (`cx.notify()`);
> tables = `DataTable<TableDelegate>`; custom paint = a raw `Element` with `shape_line`+`paint_quad`.
> Tooltips → gpui-component `Tooltip`/`HoverCard` plus a global owner (§4.11). **Drop `warmUp()`/
> `preload()`/`runPrimerOnce()` — GPUI has no cold-DLL/style first-show cost** (map §9).
>
> **Clipboard note:** gpui clipboard takes one string; carry the JSON in a typed/metadata entry plus the
> plain-text. On read, attempt JSON-parse via `ClipboardCodec::deserialize`; if absent/invalid, treat as
> plain text. This mirrors `QMimeData`'s dual payload without a `QMimeData` type.

### 4.1 `OptionsDialog` — `src/ui/options_dialog.rs` (← `optionsdialog.*`)
Fixed **700×450**. gpui-component `Dialog` + `Tree` (left) + conditional page render (right) +
`TextInput` search.
```rust
#[derive(Clone)]
pub struct OptionsResult {
    pub theme_index: i32,           // = 0
    pub font_name: String,
    pub menu_bar_title_case: bool,  // = true
    pub show_icon: bool,            // = false
    pub auto_start_mcp: bool,       // = true
    pub refresh_ms: i32,            // = 660
    pub generator_asserts: bool,    // = false
    pub brace_wrap: bool,           // = false
}
pub struct OptionsDialog { /* widget state seeded from OptionsResult */ }
impl OptionsDialog {
    pub fn new(current: &OptionsResult) -> Self;
    pub fn result(&self) -> OptionsResult;   // reads live widget state back
    pub fn select_page(&mut self, index: i32);
}
```
- **Left:** search `TextInput` (placeholder `"Search Options (Ctrl+E)"`, clear button) + `Tree`
  (header hidden, width **200**). Tree: top **Environment** (folder) → **General** (gear, page 0),
  **AI Features** (remote, page 1), **Generator** (code, page 2). Expand-all; current = General.
  Tree colors: text=`textDim`, selected bg=`hover`, selected fg=`text`.
- **Right page 0 (General):** "Refresh Rate" group → spin (objectName-equivalent id `"refreshSpin"`,
  range **1..=60000**, step 50, suffix `" ms"`, value=`refresh_ms`) + desc (default 660 ms). "Visual
  Experience" → theme combo (filled from `ThemeManager::themes()`, current=`theme_index`), font combo
  (`"fontCombo"`, items **["IBM Plex Mono","JetBrains Mono","Consolas"]** — **3 items, source-truth**,
  see §9.1, current_text=`font_name`), checkboxes: "Uppercase menu items" (`menu_bar_title_case`),
  "Show icon in title bar" (`show_icon`), "Opening brace on new line" (`brace_wrap`).
- **Page 1 (AI Features):** "MCP Server" → checkbox "Auto-start MCP server" (`auto_start_mcp`) + desc.
- **Page 2 (Generator):** "C++ Header" → checkbox "Emit static_assert size checks" (`generator_asserts`).
- Tree↔page: a map item→page-index; selecting sets the rendered page.
- Bottom: Cancel(Secondary,reject) + OK(Primary,default,accept).
- **Search filter** (`optionsdialog.cpp:235-280`): `collect_page_keywords(page)` harvests text from every
  label/checkbox/groupbox title and every combo item under that page. `filter(item)` recursive: visible if
  `item.text` contains query (case-insensitive) OR any collected page keyword contains it OR any child
  visible. Hidden items removed from the tree view; visible parents auto-expand.

### 4.2 `ProcessPicker` — `src/ui/process_picker.rs` (← `processpicker.*`, `processpicker.ui`)
Modal "Attach to Process" (700×500 min, 1400×1000 max). gpui-component `DataTable`. **The only widget
with real platform code** — keep enumeration behind `#[cfg]`; on the verified Linux path the `/proc`
branch is real, Windows is `#[cfg(windows)]` (compiles, not run here). (Reminder: live process *providers*
are stubs; ProcessPicker's enumeration is benign built-in UI logic and stays.)
```rust
pub struct ProcessInfo { pub pid: u32, pub name: String, pub path: String, pub icon: Option<Icon>, pub is_32bit: bool }
pub struct ProcessPicker { /* … */ }
impl ProcessPicker {
    pub fn new() -> Self;                          // live enumeration
    pub fn with_processes(custom: Vec<ProcessInfo>) -> Self;  // supplied list, hides Refresh
    pub fn selected_process_id(&self) -> u32;
    pub fn selected_process_name(&self) -> String;
}
```
- Columns: PID (80px), Name (200px), Path (stretch). Sorting enabled; elide-left path; row height
  `fm.height()+6`. Focus border = accent (`indHoverSpan`) because `selected` is near-invisible navy.
- `refresh_process_list()` → clear + `enumerate_processes()`:
  - `#[cfg(windows)]`: `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` walk `PROCESSENTRY32W`;
    `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` (3 fallbacks) path;
    `SHGetFileInfoW(...SMALLICON)` icon; `IsWow64Process` → `is_32bit`. Snapshot fail → `warn`. **Use the
    `windows` crate behind `#[cfg(windows)]`; must compile, not exercised on Linux.**
  - `#[cfg(target_os="linux")]`: scan `/proc/<pid>`; name from `/proc/<pid>/comm`; path from
    `/proc/<pid>/exe`; **skip if `/proc/<pid>/mem` not readable**; 32-bit if ELF `e_ident[4]==1`
    (`ELFCLASS32`); default computer icon.
  - else (`#[cfg(target_os="macos")]` etc.): return empty / "not supported" warn.
- `populate_table(list)`: PID stored as an int sort key (numeric sort); name display `"name (32-bit)"`
  if `is_32bit` but the **original name kept as the row's data** (used on accept); path cell tooltips the
  full path. **Default sort: PID descending.**
- `apply_filter()`: trimmed-lowercase substring match vs pid-string / name / path; empty → all.
- `on_process_selected()`: read pid + original name → accept (double-click or Attach).
- `select_preferred_process()`: read settings key `"lastAttachedProcess"`, select+scroll matching row
  (case-insensitive).
- Right-click context menu (gpui-component `ContextMenuExt`): Copy PID / Copy Name / Copy Path (Path only
  if non-empty) → clipboard. Cancel → reject.

### 4.3 `GotoAddressDialog` — `src/ui/goto_address.rs` (← `gotoaddressdialog.h`)
Modal 440×320; thin wrapper over `addr::AddressParser::evaluate`. Constants in §2.2.
```rust
pub struct GotoAddressDialog { /* cbs, ptr_size, input, status, recent_list, ok_enabled, resolved, last_ok */ }
impl GotoAddressDialog {
    pub fn new(cbs: AddressParserCallbacks, pointer_size: i32 /*=8*/) -> Self;
    pub fn formula(&self) -> String;          // trimmed input
    pub fn resolved_address(&self) -> u64;     // 0 if none
}
```
- UI: rich hint label; monospace input (font from settings, default "IBM Plex Mono"); status label;
  "Recent:" label; recent `uniform_list` from `load_recent()`; Cancel/Go(Primary, initially disabled).
- `on_text_changed`: trimmed empty → status blank, OK disabled, `resolved=0`, `last_ok=false`. Else
  `AddressParser::evaluate(trimmed, ptr_size, &cbs)`: ok → status `"→ 0x{value:x}"` in `indHeatCold`,
  OK enabled, store value, `last_ok=true`; err → status = error (or `"invalid expression"`) in
  `markerError`, OK disabled, `resolved=0`, `last_ok=false`.
- `accept()` override: refuse if `resolved==0 && !last_ok`; else `push_recent(input)` then accept.
- Keyboard: Down in input with non-empty recent → focus list select row 0; recent `item_activated` →
  set input + accept if OK enabled; `current_text_changed` copies selected text to input.
- Tests (`test_goto_address.cpp`): OK disabled initially; `"0x1234"`→enabled & resolves `0x1234`;
  `"xyz nonsense"`→disabled; `"<game.exe> + 0x40"`→`0x140000040`; recent dedup/cap/order; list shows
  existing entries (item 0 = most-recent). The `AddressParserCallbacks.resolve_module` hook resolves
  `"game.exe"→0x140000000`.

### 4.4 `CommandPalette` — `src/ui/command_palette.rs` (← `commandpalette.h`)
Frameless centered modal (560×380). Walks the command/menu registry, fuzzy-searches "Menu > Sub > Item"
paths, triggers the matched command. gpui-component `List<ListDelegate>` inside a `Root` modal, plugging
`command_palette_fuzzy` (§1.2) into the filter (NOT `perform_search`'s default).
```rust
pub struct Entry { pub command_id: CommandId, pub path: String, pub shortcut: String, pub enabled: bool }
pub struct CommandPalette { entries: Vec<Entry>, filter: String, /* … */ }
impl CommandPalette {
    pub fn new(menu: &MenuModel) -> Self;                 // walks + populates
    pub fn populate_from_menu(&mut self, menu: &MenuModel); // clears + re-walks (test-visible)
    pub fn entries(&self) -> &[Entry];
    pub fn activate_entry(&mut self, idx: usize) -> bool;  // bounds+enabled+non-null → trigger + accept; else false
}
```
- `walk_menu(menu, path, seen)`: recursive, **cycle-safe via `HashSet seen`**; skip separators; strip a
  `\t`-embedded shortcut hint and `&` mnemonics from labels; skip blank labels; submenu → recurse with
  `path + " > " + label`; leaf → push `Entry{command, full_path, shortcut(native text), enabled}`.
  **Reclass has no `QMenuBar`** in GPUI — model commands as a `MenuModel` tree (the app-shell subsystem
  owns the command registry; this walks it). `command->trigger()` becomes `cx.dispatch_action(...)` or a
  registry callback.
- `rebuild_model()`: score every entry vs `filter` with `command_palette_fuzzy`; keep `>0`;
  **stable-sort by score desc** (Rust `sort_by` is stable). Row label `"path    [shortcut]"`; disabled
  entries → `textMuted` fg + not selectable. Select row 0 if any.
- Keyboard (`event_filter`): Down/Up/PageDown/PageUp forwarded to list; Escape → reject;
  Enter → activate current (else row 0); list activate/click → `activate_entry`.
- Tests (`test_command_palette.cpp`): see §1.2 plus: enumerates exactly **4** leaf actions from the
  fixture menu (`File>Open`, `File>Save`, `File>Recent>foo.rcx`, `Edit>Undo`); separators don't change
  count; disabled actions included with `enabled=false`; activate triggers exactly once; shortcuts
  captured non-empty.

### 4.5 `TypeSelectorPopup` — `src/ui/type_selector.rs` (← `typeselectorpopup.*`, the centerpiece)
Frameless popup for picking a field/array/pointer type. gpui-component `Popover`/modal shell + a
`uniform_list` of bespoke rows (custom `render_item`); the row painting (accent stripe, tinted icon,
fuzzy-highlight spans, size bar) is **raw-gpui custom paint** (gpui_component_cookbook §2: "feasible").
Instance is cached & reused (C++ `WA_DeleteOnClose=false`) → keep as a persistent `Entity`.

**Enums/structs:**
```rust
pub enum TypePopupMode { Root, FieldType, ArrayElement, PointerTarget }   // (default state FieldType)
pub enum EntryKind { Primitive, Composite, Section }
pub enum EntryCategory { CatPrimitive, CatType, CatEnum }
pub struct TypeEntry {
    pub entry_kind: EntryKind,        // = Primitive
    pub category: EntryCategory,      // = CatPrimitive
    pub primitive_kind: NodeKind,     // = Hex8 (when Primitive)
    pub struct_id: u64,               // = 0 (when Composite)
    pub display_name: String,
    pub class_keyword: String,        // "struct"/"class"/"enum"
    pub enabled: bool,                // = true (false → grayed, unselectable)
    pub size_bytes: i32,              // = 0 (0 ⇒ "dyn")
    pub alignment: i32, pub field_count: i32,
    pub field_summary: Vec<String>,   // first ~6 fields "0x00: float x"
    pub kind_group: String,           // Hex/Int/Float/Ptr/Vec/Str/Ctr (auto-assigned if empty)
}
// TypeSpec/parse_type_spec → §2.3
```
**Free functions** (port verbatim — used by delegate + tests):
- `kind_group_for(NodeKind) -> &'static str` (`typeselectorpopup.cpp:91`): `is_hex_node`→"Hex";
  Int8..UInt128 + Bool→"Int"; Float16/Float/Double→"Float"; ptr/funcptr→"Ptr"; vec/mat→"Vec";
  string→"Str"; container→"Ctr"; default→"Hex".
- `kind_group_color(group, theme) -> Hsla` (`:69`): Hex→indHoverSpan, Int→syntaxKeyword,
  Float→markerCycle, Ptr→markerPtr, Vec→syntaxType, Str→syntaxString, Ctr→indDataChanged,
  Common→syntaxPreproc, else→text.
- `kind_group_dim_color(group, theme) -> Hsla` (`:82`): per-channel `(c*18 + 0x1e*82)/100` blend
  (18% accent + 82% background 0x1e).

**Public methods** (mirror `typeselectorpopup.h`): `set_font`, `set_title`(no-op), `set_mode`,
`apply_theme`(no-op in GPUI), `set_current_node_size`, `set_pointer_size`, `set_modifier(mod_id, array=0)`,
`set_types(types, current: Option<&TypeEntry>)`, `set_recent_types(Vec<String>)`, `popup(global_pos)`,
`popup_loading(global_pos)`. **Drop `warm_up`/`preload`.**
**Signals → callbacks/events:** `type_selected(TypeEntry, full_text: String)`,
`create_new_type_requested(mod_id, array)`, `save_requested()`, `dismissed()`.

**Layout** (ctor `:398-853`): a column with rows — filter row (`m_filterEdit` placeholder
`"Filter types..  (Ctrl+F)"` + `×` close); **chip row** (four `CategoryChip`: Hex/Int/Float/Ptr, all
checked, group-colored, + `all`/`none` quick toggles — `none` keeps the first chip on; Vec/Str/Ctr/Common
have NO chip and are always visible); **sort toolbar** (`group`/`name`/`size`, group default; clicking the
active mode flips `sort_dir`, switching mode resets dir to 1 and shows ↑/↓; density toggle normal/compact;
detail-pane toggle); **body** (`uniform_list` + a hidden 220px detail pane); **action row** (dynamic title
preview + modifier toggles `*`(id1)/`**`(id2)/`[]`(id3) made mutually exclusive in the click handler +
array-count edit `[1..=99999]` hidden unless array + `+ New` + `OK`); **footer crumb**.

**`set_mode(mode)`** (`:1086`): show modifier row only for FieldType/ArrayElement; **always uncheck all
modifiers + clear/hide array edit** (test `testSetModeResetsModifierInPointerTargetMode`).
**`set_modifier(mod_id, arr)`** (`:1106`): uncheck all, then check `*`/`**`/`[]` by id; for `[]` set count
text + show edit. (Tests verify checked id == 1/2/3.)

**`set_types`** (`:1119-1176`): store `all_types`, cache longest display-name length, set current entry
("you are here"), build mode-specific placeholder with live count
(`"Filter {n} structs/element types/targets/types"`), clear filter, `apply_filter("")`; if `current`,
pre-select matching row (Primitive→by primitive_kind, Composite→by struct_id).

**`apply_filter` core algorithm** (`:1485-1747`) — port faithfully:
```
enabled_groups = set of groups whose chip is checked
cat_allowed(e): if e.kind_group empty → true
                if group has no chip (Vec/Str/Ctr/Common) → true
                else group ∈ enabled_groups
make_label(e): display_name + (if size_bytes>0 then " - {n}B")
precompute total + per-group counts (filter-independent)

if filter non-empty (FUZZY, flat, no headers):
    for each non-Section entry: auto-assign kind_group if empty;
        (score, pos) = rcx::fuzzy_score(filter_base, display_name); skip score<=0;
        bump group_counts; if cat_allowed → push {idx, score, pos}
    sort by score DESC (stable);  emit entries + positions + labels
else (filter empty, BUCKETED):
    bucket by kind_group (auto-assign if missing)
    SortGroup: optional "Recent" section first (entries whose name ∈ recent_names, enabled),
               then groups in fixed order [Hex,Int,Float,Ptr,Vec,Str,Ctr,Common]
               with labels [Hex,"Int / Bool",Float,"Pointer / FuncPtr","Vec / Mat",String,Type,"Common Types"]
               Hex always size-DESC; other groups when (non-Root && current_node_size>0):
                   same-size entries first (alphabetical) then rest (size-DESC); else size-DESC
               append_section emits a Section pseudo-entry + items
    SortName/SortSize/SortAlign: flatten all buckets, sort by sort_dir
    empty-state → Section "No types available" / "No types match '{q}'"
update chip counts (filtered→ "visible/total", else "total"); equalize chip widths to widest
status label = "N of M" (filtered) or "N types"; push filtered list + positions to delegate; select first selectable row
```
**Selection/accept:** `next_selectable_row(from, dir)` skips Section & disabled.
`accept_current()`/`accept_index(row)`: reject Section/disabled; `full_text = display_name + modifier
suffix` (`*`/`**`/`[count]`); emit `type_selected(entry, full_text)`; hide.
`update_modifier_preview()`: rich title `"type+suffix → newSize (±diff vs current)"`; pointer modifiers
set newSize=`pointer_size`; array multiplies; footer crumb `"name · size · group"`; no selection →
"Select a type" + nav hint. `update_detail_pane()`: cosmetic rich card (layout/memory grid ≤16B /
properties / C-declaration / fields / actions) — render but parity is not test-gated.

**Keyboard** (`:1789-1868`): Escape→hide; Ctrl+F→focus+select-all filter. In filter: Down→first
selectable in list; Enter→accept. In list: Up→prev selectable (or back to filter at top), Down→next,
Enter→accept, Backspace→pop last filter char + focus filter, any printable→append + focus filter
(type-to-filter).

**Row paint (`TypeSelectorDelegate`, `:108-394`)** — raw-gpui custom row (`render_item` returns a custom
`Element`): group-colored rows; 2px left accent stripe (selected); group-tinted icon (class/enum/variable
SVG, via §4.13 tint helper); name with **fuzzy-highlight spans** (segment fill `selection`, using the
match-position arrays); dim composite keyword suffix; **size bar** (32px ceiling 64B, group-color alpha
140/200, hatched stripes if dyn) + size text (`"{n}B"`/"dyn"). Loading skeleton draws randomized grey
rounded bars (`bar_w = 40 + (row*73+29)%100`). All pixel constants scale off `fm.height()`. Composite
rows show a tooltip with `field_summary`.

**`popup(pos)`** (`:1178`): width scales with font (`char_w*54` cap, `char_w*46` floor), height clamps
list rows `[3,16]`, min 400; clamp to screen; show, raise, focus filter. **`popup_loading`** (`:901`):
loading flag + 12 empty skeleton rows shown instantly; later `set_types()` fills. `paint_event` draws a
manual 1px `border` rect on 4 edges; `hide_event` emits `dismissed()`.

**Tests** (`test_type_selector.cpp`): `parse_type_spec` cases (§2.3); `set_types` inserts section headers
(row_count>2); `set_mode` resets modifiers (PointerTarget shows both primitives, rows≥3); `set_modifier`
preselects (checked id 1/2/3); `type_selected`/`create_new_type_requested` callback spies; icon scales
with font; popup width scales with font; theme-change re-style. **Note:** the `commandRow*Span` helpers
exercised by this test live in the **controller/editor** subsystem, not here.

### 4.6 `EnumPickerPopup` — `src/ui/widgets/enum_picker_popup.rs` (← `enum_picker_popup.h`)
Themed enum-member picker. `Popover` shell + `uniform_list` custom rows. **Callback-based** (C++ uses a
`std::function` not a signal to avoid AUTOMOC) → store a `Box<dyn Fn(i64)>`.
```rust
pub struct Member { pub name: String, pub value: i64 }
pub struct EnumPickerPopup { /* … */ }
impl EnumPickerPopup {
    pub fn new() -> Self;
    pub fn show(&mut self, enum_name: &str, members: Vec<Member>, current_value: i64, accent: Hsla, global_pos: Point<Pixels>);
    pub fn set_on_chosen(&mut self, f: impl Fn(i64) + 'static);
}
```
- `show`: set accent/current; clear filter; **filter visible only when members > 10**; pre-select +
  center-scroll the row matching `current_value`. Rows ≤ 14; `row_h = fm.height()+6`;
  width via `compute_preferred_width` (≥ 280); clamp to screen; focus filter (if visible) else list.
- Filter (`apply_filter`): `rcx::fuzzy_score(pat, name)` keep score>0; sort search-active → score desc,
  else **by value ascending**. Footer `"N of M · ↑↓ navigate · Enter set · Esc dismiss"` or
  `"M members · …"`.
- Row paint: selection/hover bg; 2px left **accent stripe** (full alpha for current value, alpha 160
  otherwise); 4×4 source pip; current-value triangle marker; name with fuzzy highlight (fill `selection`);
  **right-aligned hex `0x…` + decimal** value column.
- Keyboard: Escape→hide; in filter Down→list (row 0 if none), Enter→accept current; in list Enter→accept,
  Backspace (filter visible)→chop, printable (filter visible)→append. `accept_row` hides then calls
  `on_chosen(value)`. 1px `borderFocused` border; mono font from settings.

### 4.7 `SourceChooserPopup` — `src/ui/source_chooser.rs` (← `sourcechooserpopup.*`)
Frameless popup ("Data Source") to switch between saved sources + provider actions. (Providers are stubs
in this port; entries reference **provider identifiers only** — no live enumeration here.) `Popover` shell
+ `uniform_list` two-line cards (custom paint).
```rust
pub enum SourceEntryKind { SavedSource, ProviderAction, SectionHeader, ClearAction }
pub struct SourceEntry {
    pub entry_kind: SourceEntryKind,  // = SavedSource
    pub display_name: String, pub kind_label: String,
    pub provider_identifier: String, pub provider_target: String, // "1234:notepad.exe"
    pub file_path: String, pub base_address: String /*hex*/, pub pid: u32, pub arch: String, // "x64"/"x86"
    pub icon_path: String, pub dll_file_name: String,
    pub saved_index: i32,             // = -1 (index into controller saved sources; -1 = provider)
    pub is_active: bool, pub is_stale: bool, pub enabled: bool, // enabled = true
}
```
Helpers (`sourcechooserpopup.h:19-42`, inline → free fns):
- `icon_for_provider(id)`: processmemory→server-process, remoteprocessmemory→remote, windbgmemory→debug,
  reclass.netcompatlayer→plug, kernelmemory→symbol-key, "File"→file-binary, else extensions.svg.
- `kind_label_for(id)`: processmemory→"Process", remote→"Remote", windbg→"Debug", kernel→"Kernel",
  "File"→"File", else "Plugin".
API: `new`, `set_font`, `apply_theme`(no-op), `set_sources(entries)`, `set_liveness_results(Vec<bool>)`,
`popup(global_pos)`. **Drop `warm_up`.** Callbacks: `source_selected(saved_index)`,
`provider_selected(identifier)`, `clear_requested()`, `dismissed()`.
- `set_sources`: cache longest name; clear filter; `apply_filter("")`. `set_liveness_results(alive)`:
  update `is_stale` per `saved_index` (`stale = !alive[saved_index]`); re-filter if changed.
- `apply_filter` (`:480-545`): empty → show all. Else build searchable string =
  `display_name [+ " "+kind_label] [+ " "+pid] [+ " "+dll_file_name] [+ " "+file_path]`; `source_chooser_fuzzy`
  (§1.3) keep>0; sort score desc. Footer: empty→nav hint, none→`"No matches for \"q\""`, else
  `"N of M sources"`. **Pre-select only when filtering** (empty filter clears current index).
- `popup` (`:547-588`): width `clamp((max_name_len+24)*char_w, 360, 560)`; height = Σ per-row heights
  (section `fm.height()+8`, two-line `fm.height()+sfm.height()+14`, single `fm.height()+12`) + chrome
  `32+30+1+28+4`, clamp `[140,520]`; screen-clamp; flip above if overflowing bottom.
- `accept_index` (`:681`): reject disabled/SectionHeader. **A SavedSource that `is_active` just hides
  (no signal).** Else hide + emit: ClearAction→clear_requested; ProviderAction→provider_selected(id);
  SavedSource(idx≥0)→source_selected(idx). `next_selectable_row` skips SectionHeader/disabled.
- Two-line card paint (`SourceChooserDelegate`): `has_subline = SavedSource OR (ProviderAction with dll)`;
  section header = uppercased `textFaint` + top hairline; cards bg per selected/hover; **stale rows get
  a warm-tinted bg**; 3px left `indHoverSpan` accent bar when active; row1 = name w/ fuzzy highlight
  (`indHoverSpan`, bold), `(exited)` in `markerPtr` if stale, bold name if active; row2 = kind label +
  **PID pill** + **arch badge** (x64→syntaxKeyword else indHoverSpan) + "active" + right-aligned elided
  path/base. Icons cached, opacity 0.3 (stale) / 0.8. Keyboard mirrors TypeSelector.

### 4.8 `HexToolbarPopup` — `src/ui/hex_toolbar.rs` (← `hextoolbarpopup.*`)
Custom-painted popup on a hex node: pick hex size, insert above/below, join, fill-to-offset. **No child
widgets except one text input** — all buttons are painted rects with a hit-test list → a raw-gpui
custom `Element` inside a `Popover` shell. Two modes: **Popup** (auto-dismiss) vs **pinned** (persistent
floating panel — see §9.3 for the GPUI equivalent of `Qt::Tool|StaysOnTop`: a non-auto-dismiss `deferred`
overlay that ignores outside clicks).
```rust
pub struct Adjacent { pub exists: bool, pub kind: NodeKind, pub data: Vec<u8> }
pub struct HexPopupContext {
    pub node_id: u64, pub current_kind: NodeKind /*=Hex8*/, pub data: Vec<u8>,
    pub nexts: Vec<Adjacent>,  // up to 15 adjacent same-parent hex nodes
    pub has_ptr: bool, pub ptr_symbol: String,
    pub has_float: bool, pub float_val: f64,
    pub has_string: bool, pub string_preview: String,
    pub multi_select_count: i32, pub multi_select_bytes: i32,
    pub multi_select_contiguous: bool, pub multi_select_kind: NodeKind,
}
pub struct HexToolbarPopup { /* … */ }
impl HexToolbarPopup {
    pub fn new() -> Self; pub fn set_font(&mut self, f); pub fn set_context(&mut self, ctx: HexPopupContext);
    pub fn popup(&mut self, global_pos); pub fn is_pinned(&self) -> bool;
}
```
Callbacks: `size_selected(node_id, NodeKind)`, `insert_above(node_id)`, `insert_below(node_id)`,
`join_selected()`, `fill_to_offset(node_id, target_offset)`, `dismissed()`.
Constants: `HEX_SIZES = [Hex8,Hex16,Hex32,Hex64,Hex128]`, labels `["8","16","32","64","128"]`. Hit actions
enum `HA_Size=0, HA_Pin=1, HA_Suggest=2, HA_InsAbove=3, HA_InsBelow=4, HA_JoinSel=5, HA_FillGo=6`.
Algorithms (port verbatim):
- `max_joinable_bytes()`: sum current size + contiguous following hex of **same kind** until gap/mismatch
  or total ≥16.
- `hex_line(type_name, bytes)`: `"{type:<7} {ascii}  {HEX HEX …}"` (ascii: printable else `.`; hex
  uppercase, space-separated).
- `preview_for_kind(target)`: same size → one line; smaller → split into `cur`/`tgt` lines; larger →
  merge data + adjacent same-kind, pad with `\0` to target.
- `info_for_kind(target)`: "current size" / "splits 1 X → N Y" / "joins N X → 1 Y" / "need N adjacent X".
- `compute_size()`: row of size buttons + pin; preview width; pinned adds suggestion/insert/join/fill
  rows; min width 200.
Paint: manual border; size buttons (current = `selected` bg + `indHoverSpan` border; `can_do` = target ≤
current OR ≤ joinable; hover highlight; disabled text `textFaint`); pin icon (pinned/pin svg); preview
lines (max 8 + "... +N more"); pinned extras: suggestion buttons (ptr*/float/utf8 → emit `size_selected`
with Pointer64/Float/UTF8), "+ hex64 above/below", multi-select join (join kind by total bytes 2/4/8/16,
valid only if contiguous & power-of-two), fill-to-offset row (label + offset input + "Go"). Keyboard-focus
ring around hovered button.
Interaction: mouse-move → repaint; mouse-press → hit-test enabled rects → dispatch (Pin toggles mode;
Size emits if different + hides if not pinned; Suggest emits + hides; Ins/Join/FillGo emit; FillGo parses
offset input as hex). Click outside → hide+dismiss if not pinned. Keyboard: Tab/arrows cycle enabled hits;
Enter/Space → **direct dispatch** of the hovered hit (the C++ synthesizes a mouse click at the hit center,
`:533`; in GPUI call the dispatch directly — map note); Escape unpins or hides+dismiss.

### 4.9 `RttiBrowserDialog` — `src/ui/rtti_browser.rs` (← `rttibrowser.h`)
Modal 720×480 viewer of a parsed `RttiInfo`. `Dialog` + tabbed view. Title
`"RTTI · {demangled|raw}"`. Constructor `(info: &RttiInfo)` (the `Provider*` arg is unused → drop).
- Rich header: `<b>name</b> (abi)`, raw name if different, then
  `vtable 0x… · module X · imagebase 0x… · COL 0x… · offset N`.
- Tabs (gpui-component `Tabs`/`TabBar`): **Hierarchy** tab — tree (Class | Raw) over `info.bases` in
  order; label `"Hierarchy (N)"`. **Vtable** tab — tree (Slot | Address | Symbol) over `info.vtable`
  (symbol or "(no symbol)"); label `"Vtable (N)"`.
- Buttons: "Copy as tree" (Secondary, left) → copies `build_text_report(info)` (§2.5) to clipboard;
  "Close" (Primary, right).

### 4.10 `ScannerPanel` — `src/ui/scanner_panel.rs` (← `scannerpanel.*`, 117KB)
A panel (not a dialog) embedding a memory scanner, wrapping the existing `scanner::ScanEngine`; reads via
a `Provider` supplied by a getter. gpui-component `DataTable` for results + many inputs/checkboxes. Most
of the bulk is widget orchestration; the parity-critical pieces are the two serialization formats and the
state machine.
```rust
pub struct StructBounds { pub start: u64, pub size: u64 }
pub type ProviderGetter = Box<dyn Fn() -> Option<Arc<dyn Provider>>>;
pub type BoundsGetter   = Box<dyn Fn() -> StructBounds>;
pub struct ScannerPanel { /* engine, results, undo_stack(cap 16), scan_generation, … */ }
impl ScannerPanel {
    pub fn new() -> Self;
    pub fn set_provider_getter(&mut self, g: ProviderGetter);
    pub fn set_bounds_getter(&mut self, g: BoundsGetter);
    pub fn set_editor_font(&mut self, f); pub fn apply_theme(&mut self, t);
    pub fn engine(&self) -> &ScanEngine; pub fn results(&self) -> &[ScanResult];
    pub fn save_results_to(&self, path: &Path) -> bool;
    pub fn load_results_from(&mut self, path: &Path) -> bool;
    pub fn save_settings(&self, key: &str /*="scanner"*/, settings: &mut Settings);
    pub fn load_settings(&mut self, key: &str, settings: &Settings);
    // Blocking automation entrypoints (for MCP):
    pub fn run_value_scan_and_wait(&mut self, value_type, value, filter_exec, filter_writable, constrain_regions) -> …;
    pub fn run_pattern_scan_and_wait(&mut self, pattern, …) -> …;  // 2 overloads (one taking explicit provider)
}
```
Callback: `go_to_address(u64)`.
**Serialization formats — port verbatim:**
- **Results JSON** (`scannerpanel.cpp:2392-2443`): root object
  `{ "version":1, "scanMode":int, "valueType":int, "count":int, "results":[ { "address": hexstring,
  "value": hexstring (toHex), "module"?: string } ] }`. Load requires `version==1`; address parsed
  base-16; value = hex-decode (`QByteArray::fromHex` → `hex::decode`/manual). After load: repopulate
  table, enable Update if non-empty, show New Scan if non-empty, status `"Loaded N result(s) from <file>"`.
- **Settings** (`scannerpanel.cpp:2445-2471`): `Settings` group `<key>` with keys
  `mode, valueType, condition, filterExec, filterWrite, privateOnly, skipSystem, userMode` — each restored
  only if present. **Preserve key names verbatim.**
**Behavior:** Mode Signature vs Value; value types; conditions (Exact/Unknown/Changed/…); Between needs two
value edits; filters (executable / writable / structOnly / privateOnly / skipSystem / userModeOnly /
fastScan stub + alignment combo 1/4/8/16/32/64); **undo stack capped 16** (snapshot `results` before each
Next Scan; `push_undo_snapshot`/`pop_undo_snapshot`); `scan_generation` (0 none, 1 first, 2+ rescan) +
stage breadcrumb + truncation banner `"Displaying N of M"`. Async scans run on `ScanEngine` (engine owns
threading via rayon under `scanner-parallel`) and report back via state updates + `cx.notify()` (the C++
`onScanFinished`/`onRescanFinished` slots).

### 4.11 `RcxTooltip` + `GlobalTooltipBridge` — `src/ui/rcx_tooltip.rs` (← `rcxtooltip.h`, `tooltip_bridge.h`)
Custom arrow tooltip (rounded-rect body + triangular arrow whose tip touches the anchor). In GPUI: a
deferred overlay element drawn with `paint_quad`/path painting; or gpui-component `Tooltip`/`HoverCard`
shell with custom paint for the arrow. The C++ window-attribute tricks (`WA_TranslucentBackground`,
`WA_ShowWithoutActivating`, **`WA_TransparentForMouseEvents`**, `DarkTitleBar`) are Qt-layered-window
specifics — in GPUI the overlay is naturally non-interactive; **keep the *intent*: the tip must not steal
hover/clicks** (gpui `deferred` overlay with no hitbox). Constants: `arrow_h=8, arrow_w=14, radius=6,
pad=10, gap=4, max_w=550`.
```rust
pub struct TipSpan { pub text: String, pub color: Hsla, pub bold: bool, pub key_cap: bool }
pub type TipLine = Vec<TipSpan>;
impl RcxTooltip {
    pub fn set_theme(&mut self, bg, border, title, body, sep);
    pub fn populate(&mut self, title: &str, body: &str, font);     // skip if unchanged & visible; split body on '\n'; font ×0.9 + bold variant; recalc
    pub fn populate_rich(&mut self, title: &str, lines: Vec<TipLine>, font); // keycap spans at ×0.70, uniform width = widest keycap
    pub fn show_at(&mut self, anchor: Point<Pixels>, prefer_above: bool /*=false*/); // chooses above/below; arrow X recomputed; bounded to screen
    pub fn dismiss(&mut self);
}
```
`recalc()`: measure width (cap `max_w`) + height from title + lines (keycap rows taller). **`show_at`
geometry is test-locked** (`test_tooltip_event.cpp`): arrow-below → `y == anchor.y`; arrow-above →
`y + height == anchor.y`; stays within left/right screen edges; **wider content → wider tooltip**.
Process-wide singleton equivalents: `shared_rcx_tooltip()` / `show_rcx_tooltip(anchor, text, font)`
(sets theme `backgroundAlt/border/text/textDim/border` + populate + show_at) / `dismiss_rcx_tooltip()`.

**`GlobalTooltipBridge`** → in GPUI there is no app-wide `setToolTip` event filter; instead a **global
hover-state owner** (gpui_component_cookbook §2 / map §15): track *which element owns the visible tip* and
dismiss only on that element's leave / focus loss / click / window deactivate. Preserve the **idempotent
guard**: same owner + same text + already visible → no re-show (this is what kills per-tick flicker;
`test_tooltip_flicker` asserts **1 show / 0 hide** over a mouse-move stream).

### 4.12 `ProfilerDialog` — `src/ui/profiler_dialog.rs` (← `profilerdialog.*`)
Modal 820×640 live perf view. Top horizontal bar chart of 15 hottest functions by total time; bottom
7-col table (Function/Count/Total ms/Mean µs/Min µs/Max µs/Last µs). **Auto-refresh ~2 Hz** via a 500ms
timer started/stopped on show/hide (`cx.spawn` + `Timer`).
- Top row: enable checkbox (toggles `Profiler::set_enabled`), summary label, "Reset" (`Profiler::reset`),
  "Copy CSV" (dumps `name,count,total_ms,mean_us,min_us,max_us,last_us`).
- `BarChart` (raw-gpui custom `Element`): bars proportional to `total_ns`; top entry full width; color by
  rank (indHeatHot / Warm / Cold / indHoverSpan); label elided; value `"X ms ×count"`; empty →
  `"(no samples — enable profiling above)"`.
- `refresh_data()`: `Profiler::snapshot()` → vec → **sort by total_ns desc** → rebuild chart (top 15) +
  table + summary `"N buckets · M samples · T ms total"`.

### 4.13 `draw_tab_source_icon` — `src/ui/tab_source_icon.rs` (← `tab_source_icon.h`)
```rust
pub fn draw_tab_source_icon(/* gpui paint cx */, icon_rect: Bounds<Pixels>, icon_path: &str, live: bool, tint: Hsla);
```
Renders an SVG tinted to `tint` (gpui paints SVG via `window.paint_svg` / an `Icon` colored by `tint`;
the Qt `CompositionMode_SourceIn` recolor maps to gpui's tinted-SVG paint, which is intrinsically
DPR-correct — the C++ "set DPR before painter attaches" caveat is a Qt-pixmap concern that does not
recur in gpui). `live=false` → opacity ×**0.40** (muted/disconnected look). **Visual-regression test
(`test_tab_source_icon`)** samples rendered pixels: pixels exist inside `icon_rect`, none bleed outside;
selected tint (240,240,240) renders brighter than dim (120,120,120) within ±20; `live=false` dims avg
alpha to < 60% of live; two different SVGs differ by > 20% of pixels. Port this test against gpui's
offscreen render (see §7).

### 4.14 `WorkspaceModel` — `src/ui/workspace_model.rs` (← `workspace_model.h`)
Model + delegate for the project-explorer tree (structs/enums per tab). gpui-component `Tree` +
custom row paint. Data roles → fields on a `WorkspaceItem` struct (no `Qt::UserRole+N` integers):
```rust
pub struct WorkspaceItem {
    pub node_id: u64, pub is_enum: bool, pub is_viewed: bool, pub is_pinned: bool,
    pub section_header: Option<String>,  // Some ⇒ section header (non-interactive)
    pub dirty: bool, pub label: String, pub children: Vec<WorkspaceItem>,
}
pub struct TabInfo<'a> { pub tree: &'a NodeTree, pub name: String /*, sub ptr → omit (Qt dock handle) */ }
```
Free helpers (logic, gate-free, testable):
- `is_hex_pad(NodeKind) -> bool`: Hex8/16/32/64 are padding (filtered from member lists).
- `build_struct_children(tree, struct_id) -> Vec<WorkspaceItem>`: sort members by offset, skip hex-pad,
  child label = `"{type_name} {name}"` where type_name = struct's `structTypeName`/keyword or
  `kind_to_string`.
- `type_display_string(node, tree) -> String`: enum → `"Name — {member_count}"`; struct →
  `"Name — {visible_field_count}"` (em-dash U+2014).
- `make_type_item(node, tree) -> WorkspaceItem`: icon enum/struct; build children for structs.
- `make_section_item(label) -> WorkspaceItem`.
- `build_project_explorer(tabs, pinned_ids) -> Vec<WorkspaceItem>`: gather all top-level Struct nodes
  across tabs; **PINNED** section (only if any pinned) then **ALL TYPES** (structs then enums).
  `sync_project_explorer` = same (caller debounces ~50ms).
- `WorkspaceProxyModel`: a filter with `set_has_filter(bool)` — **when filter active, hide section
  headers** and apply base filter; when inactive, headers pass.
- `WorkspaceDelegate` row paint: section header bg `background`, label at **0.67× font** with 1.2 letter-
  spacing in `textMuted` + trailing 0.5px hairline; normal item: selection bg `selected` + 2px left accent
  bar (`borderFocused`, inset 4px), hover bg `hover`; square **letter badge** (S/E top-level by is_enum,
  F for children) in `badgeBg` rounded 3px, letter `badgeText` (alpha 100 if not viewed); top-level: split
  `"Name — count"` into name (`text`, elided) + right-edge **count pill** (`surface` bg, `textMuted` text)
  + optional pin icon; child: split `"TypeName fieldName"` → type in `syntaxType`, field in `textDim`.

### 4.15 `HoverPreviewRegistry` — `src/ui/widgets/hover_preview.rs` (← `hover_preview.h`)
Pluggable hover-preview registry (editor's HoverPopupHost shows one of N preview views).
```rust
pub struct HoverContext<'a> {
    pub editor_font: Font, pub theme: &'a ReclassTheme,
    pub data_provider: &'a dyn Provider /*snapshot or real*/, pub code_provider: &'a dyn Provider /*always real*/,
    pub tree: &'a NodeTree, pub history: Option<&'a HashMap<u64, ValueHistory>>,
}
pub trait HoverPreview {
    fn id(&self) -> &str;                         // stable settings key
    fn tab_label(&self) -> &str;
    fn subtitle(&self, lm: &LineMeta) -> String { String::new() }  // optional
    fn eligible(&self, lm: &LineMeta, node: &Node, ctx: &HoverContext) -> bool;  // cheap, every tick
    fn widget(&self, lm: &LineMeta, node: &Node, ctx: &HoverContext, /*build cx*/) -> Option<AnyElement>; // None ⇒ host hides
}
pub struct HoverPreviewRegistry { previews: Vec<Box<dyn HoverPreview>> }
impl HoverPreviewRegistry {
    pub fn add(&mut self, p: Box<dyn HoverPreview>);   // registration order = default tie-break
    pub fn eligible_for(&self, lm, node, ctx) -> Vec<&dyn HoverPreview>;  // in reg order
    pub fn size(&self) -> usize;
}
```
The host (editor subsystem) owns dwell timing, anchor, per-node-kind last-pick persistence, Tab/Shift+Tab
cycling. The concrete previews live in the editor subsystem — out of scope here.

---

## 5. Platform-specific code (`#[cfg(...)]`)

- **`ProcessPicker::enumerate_processes`** — the only genuinely platform-branched widget:
  `#[cfg(windows)]` toolhelp/psapi/shell (uses the `windows` crate; compiles, not run on Linux);
  `#[cfg(target_os="linux")]` `/proc` (real on the verified build); else empty/"not supported".
  Each branch must compile on every target (cfg-gate the bodies, keep a common signature).
- **`RcxTooltip`** `DarkTitleBar`/layered-alpha is a Windows-DWM workaround → no-op in GPUI; drop.
- **Menu bar parity** (`setNativeMenuBar(false)`): in GPUI use gpui-component `AppMenuBar` cross-platform
  (or native `cx.set_menus` on macOS); `CommandPalette` walks the in-app `MenuModel` either way.
- GPUI itself confines OS gating to `gpui_platform::current_platform()` — Reclass app code is OS-agnostic
  (gpui_cookbook §1.4) — so this subsystem's only `#[cfg]` surface is the ProcessPicker enumeration.

---

## 6. Error-handling strategy

- **Library/codec functions** (`parse_lenient_hex`, results-JSON load, clipboard deserialize): return
  `Result<_, String>` or `Result<_, thiserror`-typed error. `parse_lenient_hex` returns `Err(reason)`
  exactly mirroring the C++ `*err` strings (`"Invalid hex digit '{c}'"`, `"No hex data"`, `"Parse failed"`).
  `ClipboardCodec::deserialize` returns an **empty `PasteResult`** (not an error) on missing/invalid MIME —
  matching the C++ "return empty" contract (`test_clipboard.cpp` `noMatchForUnknownMime`/`nullMime`).
- **Scanner load** (`load_results_from`): bool false on `version != 1` / parse failure (matches C++).
- **Settings reads**: missing keys leave defaults untouched (`if contains` in C++ → `Option`/`unwrap_or`).
- **Address evaluation**: `AddressParser::evaluate` already returns `{ok, value, error}` (addr subsystem);
  GotoAddress maps `error` (or `"invalid expression"`) to the status label; never panics.
- **UI fallibility**: render code must not panic — popups clamp to screen, empty lists show empty-state
  Section rows. Modal helpers return `Option`/`bool`/enum, never error.
- App-level glue uses `anyhow`; subsystem codecs use `thiserror`. `tracing` replaces `qDebug`/`qWarning`
  (e.g. ProcessPicker snapshot-failure logs + shows a `warn` box).

---

## 7. TEST PLAN — each covering C++ test → Rust `#[test]`

Headless logic tests run under `--no-default-features` (no `ui`/gpui). Citations: oracle logs in
`_oracle/logs/`, sources mirrored in `_oracle/test_sources/` and `/home/loke/reclass-cpp/tests/`.

### 7.1 Headless / oracle-backed (must pass in CI without a display)

| C++ test | Oracle | Rust translation |
|---|---|---|
| `test_clipboard.cpp` (9 slots) | **PASS 9/0/0**, `logs/test_clipboard.txt` | `tests/clipboard.rs` (or `#[cfg(test)]` in `core/clipboard.rs`). Mirror each slot: `round_trip_leaf_node` (serialize one leaf → deserialize into a fresh tree → 1 node, kind Hex32, name "f0", 1 root id, **new id not in target**); `round_trip_subtree` (serialize root → 4 nodes, 3 children re-parented to new root, target `next_id=100`); `no_match_for_unknown_mime` (foreign text → empty); `null_mime` (None → empty); `pasted_ids_are_fresh` (paste into same tree → no id collides with preexisting set); `plain_dump_readable` (contains "Root"/"root", `+0x0000`, "f0"); `multiple_roots_preserve_order` (two leaves → 2 non-zero root ids). Build the same `makeSimple()` tree helper. |
| `test_command_palette.cpp` fuzzy slots (5) | `G` (full test needs display) but **fuzzy fns are pure** | `tests/command_palette_fuzzy.rs`: `command_palette_fuzzy("", "File > Open") > 0`; word-start `"open"/"File > Open"` **>** mid-word `"open"/"Reclass > Reopen Last"`; `"xyz"/"File > Open" == 0`; `"fs"/"File > Save" > 0`; `command_palette_fuzzy("OPEN", …) == command_palette_fuzzy("open", …)`. |
| `test_type_selector.cpp` parseTypeSpec slots (9) | `G` overall, but parse slots are pure | `tests/typespec.rs`: the 9 cases in §2.3 verbatim (plain, array 10, ptr1, ptr2, empty, whitespace→"Ball" ptr, `[0]`→count 0, primitive `*`, `f64**`). |

(The fuzzy and parseTypeSpec assertions are *exact* — they are part of the behavioral contract even
though the surrounding `G`-class tests need a display.)

### 7.2 New headless tests worth adding (parity coverage the C++ exercises only indirectly)

- `parse_lenient_hex`: every documented case (`"DE AD BE EF"`, `"DEADBEEF"`, `"0xDEADBEEF"`,
  `"{0xDE, 0xAD, 0xBE}"`, `"DE,AD"`, `"1 2 3"`→`[1,2,3]`, `"0x100"`→`[1,0]`) + error cases (`""`→
  `Err("No hex data")`, `"0xZZ"`/mid-token letter → `Err`).
- `goto_address` recent helpers (§2.2) against an in-memory `Settings`: dedup-to-top, cap 12,
  most-recent-first (mirrors `persistsRecentEntries`/`recentDeduplicates`/`recentIsCapped`).
- `Profiler`: disabled `record` is a no-op (snapshot empty); enabled accumulates total/min/max/last/count;
  `reset` clears; `ProfileScope`/`profile_scope!` records once on drop; min/max correct over multiple samples.
- `build_text_report`: assert the exact multi-line layout for a synthetic `RttiInfo` (Class/ABI/Raw/
  Vtable/Module/COL/Offset, `Hierarchy (N)`, `Vtable (N)` with `[slot:2] 0x{addr:x} symbol|(no symbol)`).
- `rcx::fuzzy_score`: contiguous prefix outscores word-internal; acronym `"GPA"→"GetProcAddress"` and
  `"u32"→"uint32_t"` match (>0) and return correct positions; scattered `"Test"→"TerminateSomething"` → 0;
  empty pattern → `(1,[])`; `p_len>t_len` → 0.
- `kind_group_for` for representative NodeKinds; `parse_type_spec` already covered.
- ScannerPanel codecs: results-JSON round-trip (`version:1`, hex address, hex value, optional module) and
  `version!=1` → load fails; settings round-trip preserves the 8 keys.
- `source_chooser_fuzzy`: searchable-string composition + ordering on a small fixture.

### 7.3 UI / display-gated tests (re-express against gpui; lower priority, run with `ui` feature)

These were **not** in the headless oracle set (`RESULTS.md` "Not built" note); translate them as gpui
view/render tests or pixel-sample tests where the framework allows an offscreen surface:

- `test_command_palette.cpp` enumeration slots → drive `CommandPalette::new(&MenuModel)` over a fixture
  menu (File>Open/Save/Recent>foo.rcx, Edit>Undo): exactly 4 entries; separators don't change count;
  disabled marked `enabled=false`; `activate_entry(open_idx)` triggers exactly once; shortcuts non-empty.
- `test_goto_address.cpp` → construct the dialog with the module-callback fixture; assert OK disabled
  initially, enabled+resolved for `"0x1234"`, disabled for `"xyz nonsense"`, `"<game.exe> + 0x40"`→
  `0x140000040`; recent list shows existing entries (item 0 = most recent).
- `test_options_dialog.cpp` → 3 stacked pages; ≥3 themes; **3 font items** (source-truth, §9.1);
  palette-driven widgets carry no stylesheet (N/A in gpui — assert colors come from theme not inline);
  "MCP" search hides General shows AI Features and clearing un-hides; spin clamps 0→1 (min);
  `result()` reflects live edits; **assert `selected != selection` distinction is honored**.
- `test_type_selector.cpp` non-parse slots → section headers present (rowcount>2); `set_mode` resets
  modifiers (PointerTarget shows both primitives, rows≥3); `set_modifier` preselects (checked id 1/2/3);
  `type_selected`/`create_new_type_requested` callback fires with correct payload; icon scales with font;
  popup width scales with font; theme-change re-render. (The `commandRow*Span` assertions belong to the
  editor/controller test, not here.)
- `test_tab_source_icon.cpp` → render `draw_tab_source_icon` to an offscreen image and sample pixels:
  pixels inside `icon_rect`, none outside; selected brighter than dim within ±20; `live=false` dims avg
  alpha < 60% of live; two different SVGs differ > 20%. (The icon/text baseline-alignment slot is a live
  tab-geometry check — port if/when the tab chrome is built.)
- `test_tooltip_event.cpp` → `RcxTooltip::show_at`: arrow-below `y==anchor.y`; arrow-above
  `y+height==anchor.y`; stays within screen left/right; wider content → wider tooltip. (`test_tooltip`/
  `test_tooltip_ui` were "NOT registered" upstream — keep as optional render checks.)
- `test_tooltip_flicker.cpp` → with the global hover-state owner, a mouse-move stream over a row carrying
  Rtti+Comment chips yields **1 show / 0 hide** (idempotent guard). Re-express as a state-machine test on
  the hover owner (no real window needed).
- `test_chips.cpp` → **belongs to the compose/editor subsystem** (LineMeta::chips), NOT `CategoryChip`
  (map §14). Do **not** put it under widgets-dialogs; note it for the compose spec.

---

## 8. Implementation order (small, independently verifiable steps)

1. **`src/profiler.rs`** — `Profiler`/`ProfileStats`/`ProfileScope`/`profile_scope!`. Headless test
   (§7.2). No deps.
2. **`src/core/typespec.rs`** — `TypeSpec` + `parse_type_spec`. Headless test (§7.1 parse slots). No deps.
3. **`src/core/clipboard.rs`** — `ClipboardCodec` (depends on `core::Node`/`NodeTree`). Headless
   `test_clipboard` (§7.1) + `parse_lenient_hex` (§7.2). **First oracle-green milestone.**
4. **`src/ui/fuzzy.rs`** + `command_palette` fuzzy + `source_chooser` fuzzy — the three scorers; pure fns,
   gate-free. Headless tests (§7.1 command-palette + §7.2). No gpui.
5. **`src/ui/rtti_browser.rs::build_text_report`** + **goto recent helpers** (§2.2) + **scanner codecs**
   (§4.10) + **workspace_model logic** (§4.14 free fns) — all pure; headless tests (§7.2). No gpui.
   *(Steps 1–5 fully verifiable with `cargo test --no-default-features`.)*
6. **`theme_apply.rs`** — `ReclassTheme` → gpui-component `Theme` (consumes themes subsystem). Establishes
   `cx.theme()` access for everything below.
7. **`widgets/` primitives** — `DialogButton`, `themed_dialog::button_row`, `CategoryChip`,
   `tab_source_icon::draw_tab_source_icon`. Pixel test for the icon (§7.3). No higher-level deps.
8. **Themed modals** — `ThemedMessageBox`, `ThemedInputDialog` (gpui-component `AlertDialog`/`Dialog`).
9. **`GotoAddressDialog`** (depends on addr subsystem + recent helpers + DialogButton). UI test (§7.3).
10. **`CommandPalette` UI** (depends on the MenuModel from app-shell + scorer). UI test (§7.3).
11. **`OptionsDialog`** (Tree + pages + search; depends on themes for theme combo). UI test (§7.3).
12. **`EnumPickerPopup`** then **`SourceChooserPopup`** (popover shell + custom rows + scorers).
13. **`TypeSelectorPopup`** (the centerpiece: chips + sort toolbar + bucketed/fuzzy `apply_filter` +
    delegate paint + modifiers). UI test (§7.3) — largest single step; build incrementally
    (list+filter first, then chips, then sort modes, then detail pane, then modifier preview).
14. **`HexToolbarPopup`** (raw-gpui custom paint + hit-test + pinned mode, §9.3).
15. **`RcxTooltip` + global hover owner** (§4.11). Geometry test (§7.3) + flicker state-machine test.
16. **`ProcessPicker`** (`DataTable` + `#[cfg]` enumeration; Linux `/proc` real, Windows compiles).
17. **`RttiBrowserDialog`** UI (tabs over `RttiInfo`; Copy uses step-5 builder).
18. **`WorkspaceModel` delegate** (Tree + custom row paint over step-5 model logic).
19. **`ScannerPanel`** UI (DataTable + state machine over step-5 codecs + scanner engine).
20. **`ProfilerDialog`** (bar chart + table + 2Hz refresh over step-1 Profiler).
21. **`HoverPreviewRegistry`** trait + registry (concrete previews are editor-subsystem work).

---

## 9. Open questions / discrepancies (carry forward; default to source-truth)

1. **OptionsDialog font count.** Source adds **3** fonts (`optionsdialog.cpp:111-113`:
   IBM Plex Mono, JetBrains Mono, Consolas) but `test_options_dialog.cpp:81` asserts `count()==2`.
   **Port to the source (3 fonts)** — the test lags the source. The Rust UI test should assert 3.
2. **`test_chips.cpp` is NOT this `CategoryChip`.** It targets the compose tail-chip data pipeline
   (LineMeta::chips Enum/TypeHint/Rtti/Comment ordering). It is oracle PASS (9/0/0) but belongs to the
   **compose/editor** spec, not widgets-dialogs. `CategoryChip` has no dedicated test.
3. **HexToolbarPopup pinned mode.** C++ uses `Qt::Tool|WindowStaysOnTopHint` (a real top-level persistent
   window) vs `Qt::Popup`. GPUI equivalent = a non-auto-dismiss `deferred` overlay (no `on_mouse_down_out`
   dismiss; closes only via the ✕/Escape/explicit action) layered above the editor; the auto-dismiss
   variant uses `.on_mouse_down_out`. Decide per `is_pinned()` at render time.
4. **Fuzzy scorers — three, not one.** Despite the map's `nucleo` aside, **hand-roll all three**
   (crate_selection.md is authoritative): their tie-breaks differ and tests pin the CommandPalette one
   strongly; the type/source popup ordering + highlight positions are observable. Do not unify on `nucleo`.
5. **Blocking-modal semantics.** Qt `exec()` blocks; GPUI dialogs are async. Model `ThemedMessageBox`/
   `ThemedInputDialog`/`GotoAddressDialog` results as awaited futures/callbacks; ensure the call sites
   (other subsystems) consume the async shape. No behavioral change — just control-flow shape.
6. **Clipboard transport.** gpui's clipboard is string-centric; carry the JSON via a typed/metadata
   clipboard entry alongside the plain text, and on paste try `deserialize` first, falling back to text.
   This preserves the dual-payload `QMimeData` behavior without a `QMimeData` analogue.
