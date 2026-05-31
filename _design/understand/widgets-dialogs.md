# Subsystem: Widgets, Dialogs, Popups, Panels (`widgets-dialogs`)

Faithful-port structural/behavioral map of Reclass's themed-UI subsystem. Covers every dialog,
popup, tooltip, themed widget, the workspace model, the scanner panel, the profiler, the clipboard
codec, and supporting helpers. **Target UI framework for the Rust port is GPUI** (the original is
Qt6 Widgets). `fuzzy_match` → `nucleo`. Everything here is `ui-heavy` and largely
platform-independent (only ProcessPicker enumeration has `#[cfg]` OS branches).

All citations are `path:line` relative to `/home/loke/reclass-cpp/`.

---

## 0. Cross-cutting Qt → Rust/GPUI mapping

| Qt type | Meaning here | Rust/GPUI equivalent |
|---|---|---|
| `QDialog` / `ThemedDialog` | modal/modeless dialog window | GPUI window + modal layer; a `ThemedDialog`-style base trait |
| `QFrame` w/ `Qt::Popup\|FramelessWindowHint` | borderless popup that auto-dismisses on outside click | GPUI overlay/popup anchored to a point, dismiss-on-click-outside |
| `QLineEdit` | text input | GPUI text input element |
| `QListView` + `QStringListModel` + `QStyledItemDelegate` | list with custom-painted rows | GPUI `uniform_list` / `list` with custom render closures |
| `QTreeWidget` | options nav tree | GPUI nested list |
| `QStackedWidget` | page switcher | GPUI conditional render keyed by index |
| `QStandardItemModel` / `QSortFilterProxyModel` | workspace tree model + filter | Rust struct model + filtered view |
| `QTableWidget` | process list / scan results / profiler table | GPUI table (rows of cells) |
| `QPainter` (custom paint) | chip / tooltip / size-bar / hex-toolbar painting | GPUI canvas / element paint |
| `QSettings("Reclass","Reclass")` | persisted prefs (registry on Win, ini/plist elsewhere) | a `Settings` store keyed by org/app; key paths preserved verbatim |
| `QPalette` roles | theme color injection | direct theme struct fields (no palette layer) |
| `QSvgRenderer` + tint via `CompositionMode_SourceIn` | recolor monochrome SVG icons | tinted SVG render |
| `QKeySequence`/`QAction` | menubar commands | command registry |
| `QMimeData` + clipboard | copy/paste payload | clipboard with custom MIME `application/x-reclass-nodes-v1` |
| `QElapsedTimer`/`QMutex`/`QAtomicInt` | profiler timing/locking | `std::time::Instant`, `Mutex`, atomics |
| `fuzzyScore` (3 separate impls!) | fuzzy filter scoring | `nucleo` — **but each impl has distinct tie-break semantics, see §13** |

Theme color tokens referenced throughout (from `themes/thememanager.h` `Theme`): `background`,
`backgroundAlt`, `surface`, `text`, `textDim`, `textMuted`, `textFaint`, `border`, `borderFocused`,
`hover`, `selected` (row-selection bg), `selection` (text-selection bg), `indHoverSpan` (accent
purple), `markerPtr` (red), `markerError`, `indHeatHot/Warm/Cold`, `markerCycle` (amber),
`syntaxKeyword/Type/String/Number/Comment/Preproc`, `indDataChanged`. **`selected` ≠ `selection`** —
a recurring footgun the tests guard against (`test_options_dialog.cpp:151`).

---

## 1. ThemedDialog — base dialog (`widgets/themed_dialog.h/.cpp`)

Base class for every custom dialog. Two jobs (`themed_dialog.h:11`):
1. On construction, apply the theme palette so subclasses get dark-on-dark content; re-apply on
   `ThemeManager::themeChanged` (`themed_dialog.cpp:5-9`).
2. `static QHBoxLayout* makeButtonRow(initializer_list<QPushButton*>)` — right-aligned row,
   `addStretch(1)` then buttons left→right so caller order = visual order. Convention `{Cancel, OK}`
   (Windows habit) (`themed_dialog.cpp:43-51`, margins `0,8,0,0`, spacing 8).

`applyTheme()` (virtual; subclasses call base first) maps theme → `QPalette` (`themed_dialog.cpp:11-41`):
`Window/Base=background`, `WindowText/Text/ButtonText/HighlightedText=text`,
`AlternateBase=surface`, `Button=button`, `Highlight=selected`, `ToolTipBase=backgroundAlt`,
`Mid=hover`, `Dark=border`, `Light=textFaint`, `Link=indHoverSpan`. Disabled group → `textMuted`
(except `Disabled,Light=background`). Sets `setAutoFillBackground(true)`. Keeps the native OS title
bar deliberately (`Qt::Dialog`) (`themed_dialog.h:24`).

Port: a `ThemedDialog` trait or shared wrapper providing the button-row helper and theme application.
GPUI does its own theming so the palette translation collapses to passing theme fields to render.

---

## 2. DialogButton (`widgets/dialog_button.h/.cpp`)

`QPushButton` subclass; 3 variants (`dialog_button.h:33`): `Primary`, `Secondary`, `Destructive`.
Fixed height **30px**, square corners, **outline-only at rest** (no filled accent). Icon size 14×14,
pointing-hand cursor (`dialog_button.cpp:17-24`). Subscribes to `themeChanged` to re-style.

`applyTheme()` color rules (`dialog_button.cpp:31-110`):
- **Primary**: fg=`text`, border=`borderFocused`. The `:default` selector flags the action target.
- **Secondary**: fg=`textDim`, border=`border` (lowest-key, "back out").
- **Destructive**: fg+border=`markerPtr` (red; falls back to `indHeatHot` if `markerPtr` invalid).
- All flip to `hover` fill on `:hover` (`hoverFg=text` for Primary/Secondary, `=warn` for Destructive).
- Pressed bg = `hover.darker(115)`. Focus/default border = `focusBorder`. Disabled: fg=`textMuted`,
  border=`border`. Padding `5px 16px`, `min-width:88px`, `font-weight:500`, `border-radius:0`.

Port: a `DialogButton { label, variant, icon? }` element with these exact color rules per state.

---

## 3. ThemedMessageBox (`widgets/themed_messagebox.h/.cpp`)

Drop-in for `QMessageBox`. Severity enum `Info/Warning/Critical/Question` (`themed_messagebox.h:42`),
plus `UnsavedChoice { Save, Discard, Cancel }`. **Severity icon was removed** — title bar + text
convey severity; `m_iconLbl` kept null (`themed_messagebox.cpp:30`).

Layout (`themed_messagebox.cpp:18-56`): outer `QVBoxLayout` margins `24,22,24,18` spacing 18; a text
row (`m_textLbl`, word-wrapped, selectable-by-mouse, font +0.5pt); a button row (margins `0,4,0,0`
spacing 12, leading `addStretch(1)`). Width clamped `[420,640]`.

Static helpers (all block via `exec()`):
- `info/warn/critical(parent,title,text)` — one Primary "OK" button, default-focused.
- `confirm(parent,title,text,acceptLabel,rejectLabel="",destructive=false) -> bool`
  (`themed_messagebox.cpp:181-203`): Cancel (Secondary) + accept button (Primary, or **Destructive**
  if `destructive`). **Destructive defaults focus to Cancel** so a stray Enter can't destroy work.
  Returns `exec()==Accepted`.
- `unsavedChanges(parent,title,text,detail) -> UnsavedChoice` (`themed_messagebox.cpp:205-240`):
  Cancel(Secondary)/Discard(Destructive)/Save changes(Primary, default). `detail` is one-name-per-line.
- Instance API: `setDetailText`, `appendButton`, `setDefault`, plus protected `applyTheme()` override.

`setDetailText(detail)` (`themed_messagebox.cpp:59-111`): split on `\n` (`SkipEmptyParts`).
**If >5 items → scrollable `QListWidget`** (read-only: `NoFocus`, `NoSelection`, `NoItemFlags`,
arrow cursor, no mouse-tracking, fixed height = `rowH*5+6`, vertical scrollbar as-needed, horizontal
off). **Else → a word-wrapped `QLabel`** in `textDim`. Either is inserted just above the button row
(`insertWidget(count()-1, …)`). Re-applies theme after.

`applyTheme()` override (`themed_messagebox.cpp:123-142`) styles text label (`text`), detail label
(`textDim`), and detail list (bg `backgroundAlt`, fg `textDim`, border `border`, no hover, disabled
items keep `textDim`).

Wording conventions (header doc `themed_messagebox.h:14-37`): titles are noun phrases not severity;
verb button labels; build with positional `%1` placeholders not `+`; `QFileDialog`/`QColorDialog`
intentionally NOT wrapped.

---

## 4. ThemedInputDialog (`widgets/themed_inputdialog.h/.cpp`)

Static-only themed replacements for `QInputDialog`. **Returns `std::optional<T>`; `nullopt` = user
dismissed** (`themed_inputdialog.h:20`).
- `getText(parent,title,label,defaultText="",placeholder="") -> Option<String>` — min width 380;
  line-edit pre-`selectAll()`, Enter accepts (`themed_inputdialog.cpp:85-118`).
- `getInt(parent,title,label,value,min,max) -> Option<int>` — min width 320; `QSpinBox` range/value
  (`themed_inputdialog.cpp:120-153`).
- `getItem(parent,title,label,items,currentIndex=0) -> Option<String>` — min width 360; combo
  (`themed_inputdialog.cpp:155-188`).

Each builds an ephemeral `ThemedDialog` (margins `20,18,20,14` spacing 10), a word-wrapped label
(color `text`), the input widget, and the OK/Cancel row (`makeOkCancelRow`, `themed_inputdialog.cpp:63-81`).
OK is Primary+default; input is focused. Per-widget QSS in anon helpers: line-edit/spinbox focus
border = `borderFocused`; combo selection bg = `selected`, popup view uses `backgroundAlt`/`text`.

Port: three functions returning `Option<T>` from a modal.

---

## 5. OptionsDialog (`optionsdialog.h/.cpp`)

Fixed-size **700×450** settings dialog. `ThemedDialog` subclass.

### `struct OptionsResult` (`optionsdialog.h:13`)
`themeIndex:int=0`, `fontName:QString`, `menuBarTitleCase:bool=true`, `showIcon:bool=false`,
`autoStartMcp:bool=true`, `refreshMs:int=660`, `generatorAsserts:bool=false`, `braceWrap:bool=false`.

### Public API
- `OptionsDialog(const OptionsResult& current, parent=nullptr)` — builds UI seeded from `current`.
- `OptionsResult result() const` (`optionsdialog.cpp:222`) — reads widget state back.
- `void selectPage(int index)` (`optionsdialog.cpp:212`) — selects tree item whose page index matches
  and sets stacked page.

### Layout
Left column: `QLineEdit m_search` (placeholder "Search Options (Ctrl+E)", clear button) + a
`QTreeWidget m_tree` (header hidden, root decorated, **fixed width 200**, icon size 16, mouse
tracking). Tree palette overridden: `Text=textDim`, `Highlight=hover`, `HighlightedText=text`.
Tree structure: top "Environment" (folder icon) → children **General** (gear, page 0),
**AI Features** (remote, page 1), **Generator** (code, page 2). `expandAll()`, current=General
(`optionsdialog.cpp:54-194`).

Right column: `QStackedWidget m_pages` with 3 pages:
- **General** (page 0): "Refresh Rate" group → `m_refreshSpin` (objectName `"refreshSpin"`, range
  **1..60000**, step 50, suffix " ms", value=`refreshMs`) + description label (default 660 ms);
  "Visual Experience" group → `m_themeCombo` (objectName `"themeCombo"`, filled from
  `ThemeManager::themes()`, current=`themeIndex`), `m_fontCombo` (objectName `"fontCombo"`, items
  **"IBM Plex Mono","JetBrains Mono","Consolas"**, currentText=`fontName`), checkboxes
  `m_titleCaseCheck` ("Uppercase menu items"), `m_showIconCheck` ("Show icon in title bar"),
  `m_braceWrapCheck` ("Opening brace on new line").
- **AI Features** (page 1): "MCP Server" group → `m_autoMcpCheck` ("Auto-start MCP server") + desc.
- **Generator** (page 2): "C++ Header" group → `m_assertCheck` ("Emit static_assert size checks").

Tree↔page: `m_itemPageIndex` maps item→index; `currentItemChanged` sets `m_pages` index. Bottom:
Cancel(Secondary, `reject`) + OK(Primary, default, `accept`).

### Search filter (`optionsdialog.cpp:252-280`)
Recursive `filter(item)`: an item is visible if `item.text(0)` contains the query (case-insensitive)
**OR** any of its collected page keywords contains it **OR** any child is visible. Hidden via
`setHidden(!visible)`; visible parents auto-expand. `collectPageKeywords(page)`
(`optionsdialog.cpp:235-250`) harvests text from every `QLabel`/`QCheckBox`/`QGroupBox` title and every
`QComboBox` item under the page widget.

### Test-locked behaviors (`test_options_dialog.cpp`)
- 3 stacked pages; ≥3 themes; exactly **2** font combo items expected by one test... but the source
  adds **3** font items. *(Discrepancy: `test_options_dialog.cpp:81` `QCOMPARE(fontCombo->count(),2)`
  vs `optionsdialog.cpp:111-113` adds 3. The test predates a font addition or runs against an older
  build — port must match the **source** (3 fonts: IBM Plex Mono, JetBrains Mono, Consolas).)* — see
  Open Questions.
- Palette-driven widgets must carry **no** stylesheet (`DialogButton` exempt) (`:113`).
- "MCP" search hides General, shows AI Features; clearing un-hides (`:201`).
- Spin clamps 0→1 (min) (`:263`); `result()` reflects live widget edits.

---

## 6. ProcessPicker (`processpicker.h/.cpp`, `processpicker.ui`)

Modal "Attach to Process" dialog. `ThemedDialog` subclass built from a `.ui` (700×500 min,
1400×1000 max). **The only widget with real platform-specific code.** OUT OF SCOPE note: the live
process *providers* are stubs, but ProcessPicker's enumeration itself is built-in UI logic — keep its
`#[cfg]` branches compiling but they can return empty on the verified Linux build path.

### `struct ProcessInfo` (`processpicker.h:12`)
`pid:uint32`, `name:QString`, `path:QString`, `icon:QIcon`, `is32Bit:bool=false`.

### Public API
- `ProcessPicker(parent)` — live enumeration ctor.
- `ProcessPicker(const QList<ProcessInfo>& custom, parent)` — uses a supplied list, hides Refresh
  (`processpicker.cpp:40-51`).
- `uint32_t selectedProcessId() const`; `QString selectedProcessName() const`.

### UI columns
3-col `QTableWidget` (PID 80px, Name 200px, Path stretch). Sorting enabled, no grid, elide-left,
row height `fontMetrics().height()+6`. Theme colors derived from the **global app palette** (set by
`applyGlobalTheme`), notably focus border = `QPalette::Link` (mapped to `indHoverSpan`) because
`Highlight`=`selected` is near-invisible dark navy (`processpicker.cpp:71-104`). Buttons styled via
QSS to mimic DialogButton (28px). Filter edit auto-focused.

### Behaviors
- `refreshProcessList()` clears + re-enumerates (`:179`).
- `enumerateProcesses()` (`:202-326`):
  - **Windows** (`#ifdef _WIN32`): `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` → walk
    `PROCESSENTRY32W`. For each, `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`,
    `QueryFullProcessImageNameW` (3 fallbacks) for path, `SHGetFileInfoW(...SHGFI_ICON|SMALLICON)`
    for icon (`QImage::fromHICON` on Qt6), `IsWow64Process` → `is32Bit`. Snapshot-failure shows a
    `ThemedMessageBox::warn`.
  - **Linux** (`#elif __linux__`): scan `/proc/<pid>`; name from `/proc/<pid>/comm`; path from
    `/proc/<pid>/exe` symlink; **skip if `/proc/<pid>/mem` not R_OK**; 32-bit detected by ELF header
    byte[4]==1 (`ELFCLASS32`); default icon = `SP_ComputerIcon`.
  - else: warn "not supported".
- `populateTable(list)` (`:328`): PID via `Qt::EditRole` int (so numeric sort works), name displays
  `name (32-bit)` if `is32Bit` but **stores original `name` in `Qt::UserRole`**, path column tooltips
  full path. **Default sort: PID descending** (newest first).
- `filterProcesses` → `applyFilter()` (`:367`): trimmed lowercase substring match against PID-string,
  name, or path. Empty filter shows all.
- `onProcessSelected()` (`:187`): reads PID (EditRole) + name (UserRole, fallback display text) →
  `accept()`. Triggered by double-click or Attach.
- `selectPreferredProcess()` (`:391`): reads `QSettings("Reclass","Reclass") "lastAttachedProcess"`,
  selects+scrolls to the matching row (case-insensitive).
- Right-click context menu (`:131-158`): Copy PID / Copy Name / Copy Path (Path entry only if path
  non-empty) → clipboard. The `.ui` wires `cancelButton.clicked → reject()`.

---

## 7. GotoAddressDialog (`gotoaddressdialog.h`, header-only)

Modal address-jump dialog, `ThemedDialog` subclass, 440×320. Thin wrapper over
`AddressParser::evaluate` (addressparser subsystem).

### Constants
`kSettingsKey = "gotoAddress/recent"`, `kMaxRecent = 12` (`gotoaddressdialog.h:29-30`).

### Public API
- `GotoAddressDialog(const AddressParserCallbacks& cbs, int pointerSize=8, parent)`.
- `QString formula() const` — trimmed input text (for rebases).
- `uint64_t resolvedAddress() const` — last resolved abs address (0 if none).
- Static recent-list helpers (shared key, also used elsewhere):
  - `loadRecent() -> QStringList` (`:123`).
  - `pushRecent(entry)` (`:128`): trim; ignore empty; `removeAll` then `prepend` (dedup→top); cap to
    `kMaxRecent` by `removeLast`. **Most-recent-first.**
  - `clearRecent()` (`:139`).

### UI / live validation
Rich-text hint label, monospace input (`QSettings "font"` default "IBM Plex Mono"), status label,
"Recent:" label, `QListWidget` populated from `loadRecent()`, Cancel/Go(Primary, initially disabled)
row. `onTextChanged` (`:164-193`): trimmed empty → status blank, OK disabled, `m_resolved=0`,
`m_lastOk=false`. Else `AddressParser::evaluate(trimmed, ptrSize, &cbs)`: ok → status
`→ 0x<hex>` in `indHeatCold`, OK enabled, store value; error → status = error (or "invalid
expression") in `markerError`, OK disabled.

### Subtle behaviors / tests (`test_goto_address.cpp`)
- `accept()` override (`:145`): refuses if `m_resolved==0 && !m_lastOk` ("can't go nowhere"); else
  `pushRecent(input)` then base accept.
- `keyPressEvent` (`:151`): Down in input with non-empty recent list → focus list, select row 0.
- Recent-list `itemActivated` sets input + accepts if OK enabled; `currentTextChanged` copies text to
  input.
- Tests verify: OK disabled initially; `"0x1234"`→enabled & resolves `0x1234`; `"xyz nonsense"`→
  disabled; `"<game.exe> + 0x40"`→`0x140000040`; recent dedup-to-top, cap at 12, most-recent-first,
  list shows existing entries (`test_goto_address.cpp:49-124`).

---

## 8. CommandPalette (`commandpalette.h`, header-only)

Frameless `QDialog` (`Popup|FramelessWindowHint|NoDropShadow`), 560×380. Walks the menu bar, lets the
user fuzzy-search "Menu > Sub > Item" paths, triggers the matched `QAction`. **Reimplements no
behavior — just surfaces wired commands.** `WA_DeleteOnClose=false`.

### `struct Entry` (`commandpalette.h:30`)
`action:QAction*`, `path:QString` ("File > Recent > foo.rcx"), `shortcut:QString` ("Ctrl+O"),
`enabled:bool`.

### Public API
- `CommandPalette(QMenuBar* menuBar, parent)` — builds + populates.
- `populateFromMenuBar(QMenuBar*)` (`:104`): clears, walks each top action's submenu via `walkMenu`,
  rebuilds model. (Visible to tests.)
- `const QVector<Entry>& entries() const`.
- `bool activateEntry(int idx)` (`:119`): bounds + `enabled` check → `action->trigger()` + `accept()`;
  returns false on invalid/disabled/null.
- `static int fuzzyScore(needle, haystack) -> int` — **this palette's OWN scorer** (see §13).

### `walkMenu(menu, path, seen)` (`:204-229`)
Recursive, **cycle-safe via `QSet<QMenu*> seen`**. Skips separators. Strips a `\t`-embedded shortcut
hint and `&` mnemonics from labels; skips blank labels. Submenu → recurse with `path + " > " + label`;
leaf → push `Entry{action, fullPath, shortcut(NativeText), isEnabled}`.

### `rebuildModel()` (`:231-258`)
Score every entry against `m_filter`; keep `>0`; **`std::stable_sort` by score desc**. Build list
items "path    [shortcut]"; disabled entries → `textMuted` fg + cleared `ItemIsSelectable`. Selects
row 0 if any.

### Key handling (`eventFilter`, `:160`)
On the input: Down/Up/PageDown/PageUp forwarded to the list; Escape → reject. `returnPressed` →
`activateCurrent()` (uses current index, else row 0). List `activated`/`clicked` → `activateIndex`
(maps proxy→source row, reads stored entry index from `Qt::UserRole`).

### Tests (`test_command_palette.cpp`)
Empty needle matches all (score>0); word-start "Open" outscores mid-word "Reopen"; miss→0;
acronym "fs"→"File > Save">0; case-insensitive equal; enumerates exactly 4 leaf actions from a fixture
bar; separators skipped (count unchanged); disabled actions included but `enabled=false`; activate
triggers exactly once; shortcuts captured non-empty.

---

## 9. TypeSelectorPopup (`typeselectorpopup.h/.cpp`) — the centerpiece

Frameless `QFrame` popup (`Popup|FramelessWindowHint`) for picking a field/array/pointer type. Largest
and most intricate widget. `WA_DeleteOnClose=false` (instance is cached & reused).

### Enums & structs
- `enum class TypePopupMode { Root, FieldType, ArrayElement, PointerTarget }` (`:26`).
- `struct TypeEntry` (`typeselectorpopup.h:30`):
  - `entryKind: {Primitive, Composite, Section}` = Primitive
  - `category: {CatPrimitive, CatType, CatEnum}` = CatPrimitive
  - `primitiveKind:NodeKind=Hex8` (when Primitive)
  - `structId:uint64=0` (when Composite)
  - `displayName:QString`, `classKeyword:QString` ("struct"/"class"/"enum")
  - `enabled:bool=true` (false → grayed, visible but unselectable)
  - `sizeBytes:int=0` (0 ⇒ "dyn"), `alignment:int=0`, `fieldCount:int=0`
  - `fieldSummary:QStringList` (first ~6 fields, e.g. "0x00: float x")
  - `kindGroup:QString` (Hex/Int/UInt/Float/Ptr/Vec/Str/Ctr; auto-assigned if empty)
- `struct TypeSpec` (`:59`): `baseName:QString`, `isPointer:bool`, `ptrDepth:int` (1=`*`,2=`**`),
  `arrayCount:int` (0=not array).

### Free functions
- `TypeSpec parseTypeSpec(const QString&)` (`typeselectorpopup.cpp:32-61`):
  trim. If ends `*`: pointer, chop, `ptrDepth=1`; if then ends `*`: chop, `ptrDepth=2`; baseName=rest
  trimmed. Else if `[` at idx>0 and ends `]`: baseName=left of `[`; parse count; **count>0 sets
  `arrayCount`, else stays 0** (so `[0]` → arrayCount 0). Else baseName=whole. Tests lock:
  `"int32_t"`, `"int32_t[10]"→10`, `"Ball*"→ptrDepth1`, `"Ball**"→2`, `""`→empty, `"  Ball *  "`→
  pointer baseName "Ball", `"int32_t[0]"`→arrayCount 0.
- `QString kindGroupFor(NodeKind)` (`:91`): maps kinds to groups via `isHexNode/isPointerKind/…`.
  Int8..UInt128 + Bool → "Int"; Float16/Float/Double → "Float"; ptr/funcptr → "Ptr"; vec/mat → "Vec";
  string → "Str"; container → "Ctr"; default → "Hex".
- `QColor kindGroupColor(group)` (`:69`): Hex→indHoverSpan, Int→syntaxKeyword, Float→markerCycle,
  Ptr→markerPtr, Vec→syntaxType, Str→syntaxString, Ctr→indDataChanged, Common→syntaxPreproc, else text.
- `QColor kindGroupDimColor(group)` (`:82`): 18% accent + 82% `0x1e` background blend.

### Public methods
`setFont`, `setTitle`(no-op — title is dynamic), `setMode`, `applyTheme(theme)`,
`setCurrentNodeSize(int)`, `setPointerSize(int)`, `setModifier(modId, arrayCount=0)`,
`setTypes(types, current=nullptr)`, `setRecentTypes(QStringList)` (most-recent-first → "Recent"
pseudo-section), `popup(globalPos)`, `popupLoading(globalPos)`, `warmUp()`, `static preload()`.

### Signals
`typeSelected(const TypeEntry&, const QString& fullText)`, `createNewTypeRequested(int modId, int arr)`,
`saveRequested()`, `dismissed()`.

### Layout (ctor `:398-853`)
A `QVBoxLayout` (margins `6,5,6,5`, spacing 3) with rows:
1. **Filter row**: `m_filterEdit` (placeholder "Filter types..  (Ctrl+F)", clear button) + `×` close
   `QToolButton` (`m_escLabel`).
2. **Chip row**: four `CategoryChip` toggles (Hex/Int/Float/Ptr, all checked, group-colored) +
   stretch + `all`/`none` quick toggles + `m_statusLabel`. `none` keeps the first chip on.
   Vec/Str/Ctr/Common have **no chip** and are always visible.
3. **Sort toolbar**: `group`/`name`/`size` buttons (group checked default). Clicking the active mode
   flips `m_sortDir`; switching mode resets dir to 1 and shows ↑/↓ arrow. Plus density toggle
   (normal ☰ / compact ≡ → delegate compact flag + relayout) and detail-pane toggle (◨).
4. **Body**: `QListView m_listView` (`QStringListModel m_model`, `TypeSelectorDelegate`, no frame,
   batched layout batch 50, mouse-tracking, hover attr) + hidden 220px **detail pane**
   (`QScrollArea`→rich-text `m_detailContent`; links `action:select`/`action:ptr`/`action:arr` set the
   modifier and accept).
5. **Action row**: dynamic `m_titleLabel` (rich preview) + stretch + modifier toggles `*`(id1)
   `**`(id2) `[]`(id3) in a **non-exclusive** `QButtonGroup` (manually made mutually exclusive in the
   `idClicked` handler) + `m_arrayCountEdit` (50px, `QIntValidator(1,99999)`, hidden unless array) +
   `+ New` button (emits `createNewTypeRequested`, hides) + `OK` button (`acceptCurrent`).
6. **Footer crumb** `m_footerLabel`.

### setMode / setModifier
- `setMode(mode)` (`:1086`): shows modifier row only for FieldType/ArrayElement; **always unchecks all
  modifiers + clears/hides array edit** (test `testSetModeResetsModifierInPointerTargetMode`).
- `setModifier(modId, arr)` (`:1106`): unchecks all, then checks `*`/`**`/`[]` by id; for `[]` sets
  count text + shows edit. (Tests verify `checkedId()` == 1/2/3.)

### setTypes (`:1119-1176`)
Stores `m_allTypes`, caches longest displayName length, sets current entry (for delegate "you are
here"). Builds a mode-specific placeholder with the live type count ("Filter %1 structs/element
types/targets/types"). Clears filter, `applyFilter("")`. If `current`: pre-selects the matching row
(Primitive→by `primitiveKind`, Composite→by `structId`).

### applyFilter (`:1485-1747`) — core algorithm
Compute enabled groups from chips. `catAllowed(t)`: empty group passes; group without a chip
(Vec/Str/Ctr/Common) always passes; else must be in enabled set. `makeLabel(e)` = `displayName` +
` - <n>B` if sized. Pre-compute total & per-group counts (independent of filter).

**Two paths:**
- **Filter non-empty (fuzzy)**: flat ranked list, no section headers. For each non-Section entry:
  auto-assign kindGroup if empty; `fuzzyScore(filterBase, displayName, &pos)` (from
  `widgets/fuzzy_match.h`); skip score≤0; bump groupCounts; if `catAllowed`, push `{idx,score,pos}`.
  **`std::sort` by score desc**, then append entries + match positions + labels.
- **Filter empty**: bucket by kindGroup (auto-assign if missing). In **SortGroup**: optional "Recent"
  section first (entries whose displayName ∈ `m_recentNames`, enabled), then per-group sections in
  fixed order `[Hex,Int,Float,Ptr,Vec,Str,Ctr,Common]` with labels `[Hex, "Int / Bool", Float,
  "Pointer / FuncPtr", "Vec / Mat", String, Type, "Common Types"]`. Hex always sorts size-desc; other
  groups (non-Root, currentNodeSize>0) put **same-size entries first (alphabetical) then rest
  (size-desc)**; otherwise size-desc. `appendSection` emits a Section pseudo-entry + items.
  In **SortName/SortSize/SortAlign**: flatten all buckets, sort by `m_sortDir`. Empty-state appends a
  Section entry "No types available" / "No types match '<q>'".

`m_model->setStringList(displayStrings)`. Update chip counts (filtered → `visible/total`, else `total`)
and **equalize chip widths to widest** so the strip reads uniform. Status label = "N of M" (filtered)
or "N types". Push filtered list + match positions to delegate. Select first selectable row.

### Selection / accept
- `nextSelectableRow(from, dir)` (`:1778`): skip Section & disabled.
- `acceptCurrent()`/`acceptIndex(row)` (`:1749-1776`): reject Section/disabled; build `fullText` =
  displayName + modifier suffix (`*`/`**`/`[count]`); `emit typeSelected(entry, fullText)`; hide.
- `updateModifierPreview()` (`:1214`): builds rich `m_titleLabel` "type+suffix → newSize (±diff vs
  currentNodeSize)"; pointer modifiers set newSize=`m_pointerSize`; array multiplies. Footer crumb
  "name · size · group". No selection → "Select a type" + nav hint.
- `updateDetailPane()` (`:1289-1483`): builds a rich-text card (layout/memory-grid for ≤16B/
  properties/C-declaration/fields/actions) — purely cosmetic.

### Keyboard (`eventFilter`, `:1789-1868`)
Escape→hide; Ctrl+F→focus+selectAll filter. In filter: Down→move into list (next selectable),
Enter→accept. In list: Up→prev selectable (or back to filter at top), Down→next selectable,
Enter→accept, Backspace→pop last filter char + focus filter, any printable→append to filter + focus
filter (type-to-filter).

### TypeSelectorDelegate (`:108-394`) — custom row paint
Group-colored rows with: 2px left accent stripe (selected), group-tinted icon
(class/enum/variable SVG), name with fuzzy-highlight spans (segment fill `selection`), dim composite
keyword suffix, **size bar** (32px ceiling 64B, group-color alpha 140/200, hatched stripes if dyn)
+ size text ("<n>B"/"dyn"). Loading skeleton draws randomized grey rounded bars
(`barW = 40 + (row*73+29)%100`). All pixel constants scale off `fm.height()` (`recomputeLayout`,
`:148`). `helpEvent` shows a `QToolTip` for composites with `fieldSummary` (`:357`).

### popup / popupLoading / warmUp / preload
- `popup(pos)` (`:1178`): width scales with font (`charW*54` cap, `charW*46` floor), height clamps
  list rows `[3,16]`, min 400; clamped to screen; `setFixedSize`, move, show, raise,
  `activateWindow`, focus filter.
- `popupLoading(pos)` (`:901`): set loading flag, fill model with 12 empty rows for skeleton, show
  instantly; later `setTypes()` fills.
- `warmUp()` / static `preload()` / `runPrimerOnce()` (`:855-899`): show+hide a throwaway popup to
  absorb ~300ms first-show DLL/style/font cost. **Port: not needed in GPUI — drop these.**

### paintEvent / hideEvent
`paintEvent` draws a manual 1px border (`palette Dark` = border) on 4 edges (`:1870`). `hideEvent`
emits `dismissed()` (`:1882`).

### Tests (`test_type_selector.cpp`)
`parseTypeSpec` cases (above); `setTypes` inserts section headers (rowCount>2);
`setMode` resets modifiers (PointerTarget shows both primitives, rowCount≥3); `setModifier`
preselects (checkedId 1/2/3); `typeSelected`/`createNewTypeRequested` signal spies; icon scales with
font; popup width scales with font; theme-change re-style auto-connect. Also exercises `commandRow*Span`
helpers (these live in controller/editor, not this widget).

---

## 10. EnumPickerPopup (`widgets/enum_picker_popup.h`, header-only)

Themed enum-member picker replacing a plain `QMenu`. `QFrame` popup. **No `Q_OBJECT`** (uses a
`std::function<void(int64_t)>` callback instead of a signal, to avoid AUTOMOC in tests)
(`enum_picker_popup.h:30`).

### `struct Member { QString name; int64_t value; }` (`:34`); `using ChosenFn = function<void(int64_t)>`.

### API
- `EnumPickerPopup(parent)` — builds title row ("enum Name" + "Esc"), filter (visible only when
  >10 members), custom `Model`/`Delegate` list, footer crumb.
- `show(enumName, members, currentValue, accent:QColor, globalPos)` (`:98`): sets accent/current,
  filter cleared, filter visibility = `members>10`, pre-selects + center-scrolls the row matching
  `currentValue`. Computes size: rows ≤14, `rowH=fm.height()+6`, width via `computePreferredWidth`
  (≥280). Clamps to screen. Focuses filter (if visible) else list.
- `setOnChosen(ChosenFn)`.

### Inner classes
- `Model : QAbstractListModel` (`:204`): rows = filtered members; `DisplayRole`=name; carries match
  positions per row; `setRows(rows, matchPositions)` with begin/endResetModel.
- `Delegate : QStyledItemDelegate` (`:229`): row height `fm.height()+6`; paints selection/hover bg,
  a 2px left **accent stripe** (full alpha for current value, alpha 160 otherwise), a 4×4 source pip,
  a current-value triangle marker, the member name with fuzzy highlight (fill `selection`), and a
  **right-aligned hex (`0x…`) + decimal value** column.

### Filter (`applyFilter`, `:334`)
`fuzzyScore(pat, name, &hits)` from `widgets/fuzzy_match.h`; keep score>0. Sort: search active →
score desc; else **by value ascending**. Footer "N of M · ↑↓ navigate · Enter set · Esc dismiss" or
"M members · …".

### Keyboard (`eventFilter`, `:159`)
Escape→hide. In filter: Down→focus list (select row0 if none); Return/Enter→accept current. In list:
Return/Enter→accept; Backspace (filter visible)→chop filter; printable (filter visible)→append.
`acceptRow` hides then invokes `m_onChosen(value)`.

`paintEvent` draws a `borderFocused` 1px rect. Mono font from `QSettings "font"`.

---

## 11. SourceChooserPopup (`sourcechooserpopup.h/.cpp`)

Frameless `QFrame` popup ("Data Source") to switch between saved sources and provider actions.
**(Providers themselves are stubs in this port — but the popup UI is in scope; entries reference
provider identifiers only.)**

### Helpers (`sourcechooserpopup.h:19-42`, `inline`)
- `iconForProvider(identifier)`: processmemory→server-process, remoteprocessmemory→remote,
  windbgmemory→debug, reclass.netcompatlayer→plug, kernelmemory→symbol-key, File→file-binary, else
  extensions.svg.
- `kindLabelFor(identifier)`: processmemory→"Process", remote→"Remote", windbg→"Debug",
  kernel→"Kernel", File→"File", else "Plugin".

### `struct SourceEntry` (`:46`)
`entryKind: {SavedSource, ProviderAction, SectionHeader, ClearAction}` = SavedSource;
`displayName`, `kindLabel`, `providerIdentifier`, `providerTarget` ("1234:notepad.exe"), `filePath`,
`baseAddress` (hex), `pid`, `arch` ("x64"/"x86"), `iconPath`, `dllFileName`,
`savedIndex:int=-1` (index into controller saved sources; -1 for providers),
`isActive:bool`, `isStale:bool` (process exited / file missing), `enabled:bool=true`.

### API
`SourceChooserPopup(parent)`, `setFont`, `applyTheme(theme)`, `setSources(entries)`,
`setLivenessResults(QVector<bool> alive)`, `popup(globalPos)`, `warmUp`.
**Signals:** `sourceSelected(int savedIndex)`, `providerSelected(QString identifier)`,
`clearRequested()`, `dismissed()`.

### Layout (`sourcechooserpopup.cpp:320`)
Title row (bold label + "Esc" tool button) / filter edit (frameless, 30px, "Filter sources...",
clear button) / 1px separator / `QListView` (`SourceChooserDelegate`, no frame, vertical scroll
as-needed) / centered footer label.

### setSources / setLivenessResults
`setSources` caches longest name, points delegate at the filtered list + match positions, clears
filter, `applyFilter("")` (`:453`). `setLivenessResults(alive)` updates `isStale` per `savedIndex`
(stale = `!alive[savedIndex]`); re-filters if anything changed (`:469`).

### applyFilter (`:480-545`)
Empty filter → show all (matchPositions sized to all). Else, for each non-SectionHeader entry build a
**searchable string** = `displayName [+ " " + kindLabel] [+ " " + pid] [+ " " + dllFileName]
[+ " " + filePath]`; `fuzzyScore` (this file's own scorer, §13) → keep>0; sort score desc. Footer:
empty→nav hint, no matches→"No matches for \"q\"", else "N of M sources". **Pre-select only when
filtering** (else first row would look permanently selected); empty filter clears current index.

### popup (`:547-588`)
Width = `clamp((maxNameLen+24)*charW, 360, 560)`. Height = sum of per-row heights
(section `fm.height()+8`, two-line card `fm.height()+sfm.height()+14`, single `fm.height()+12`) +
chrome `32+30+1+28+4`, clamped `[140,520]`. Screen-clamped; flips above if overflowing bottom.

### Selection / accept (`acceptIndex`, `:681`)
Reject disabled/SectionHeader. **A SavedSource that `isActive` just hides (no signal).** Else hide
then emit: ClearAction→`clearRequested`; ProviderAction→`providerSelected(identifier)`;
SavedSource (savedIndex≥0)→`sourceSelected(savedIndex)`. `nextSelectableRow` skips SectionHeader/disabled.

### SourceChooserDelegate (`:86-316`) — two-line cards
`hasSubline(e)` = SavedSource OR ProviderAction-with-dll. Section headers: uppercased label in
`textFaint`, top hairline. Cards: bg per selected/hover; **stale rows get a warm-tinted bg**; 3px
left `indHoverSpan` accent bar when `isActive`. Row 1 = display name with per-char fuzzy highlight
(`indHoverSpan`, bold), `(exited)` suffix in `markerPtr` if stale, **bold name if active**. Row 2 =
metadata: kind label, **PID badge** (rounded pill), **arch badge** (x64→syntaxKeyword else
indHoverSpan), "active" label, right-aligned elided file path / base address. Icons cached
(`cachedIcon`, `:74`) with opacity 0.3 (stale) / 0.8.

`paintEvent` 1px border; `hideEvent` emits `dismissed`. Keyboard mirrors TypeSelectorPopup
(Down from filter → list; type/Backspace forward to filter).

---

## 12. HexToolbarPopup (`hextoolbarpopup.h/.cpp`)

Custom-painted popup attached to a hex node: pick hex size, insert above/below, join, fill-to-offset.
**No child buttons except one `QLineEdit`** — all buttons are painted rects with a hit-test list.
Two window modes: **Popup** (auto-dismiss) vs **Tool+StaysOnTop** (pinned, persistent)
(`hextoolbarpopup.cpp:65-84`).

### `struct HexPopupContext` (`hextoolbarpopup.h:11`)
`nodeId:uint64`, `currentKind:NodeKind=Hex8`, `data:QByteArray` (raw bytes), `nexts:QVector<Adjacent>`
(`Adjacent{exists, kind, data}`, up to 15 adjacent same-parent hex nodes for join preview), smart
suggestions `hasPtr/ptrSymbol`, `hasFloat/floatVal`, `hasString/stringPreview`, multi-select
`multiSelectCount/Bytes/Contiguous/Kind`.

### API
`HexToolbarPopup(parent)`, `setFont`, `setContext(ctx)`, `popup(globalPos)`, `isPinned() const`.
**Signals:** `sizeSelected(nodeId, NodeKind)`, `insertAbove(nodeId)`, `insertBelow(nodeId)`,
`joinSelected()`, `fillToOffset(nodeId, targetOffset)`, `dismissed()`.

### Constants
`kHexSizes = [Hex8,Hex16,Hex32,Hex64,Hex128]`, labels `["8","16","32","64","128"]`
(`hextoolbarpopup.cpp:14-19`). Hit actions: `HA_Size=0, HA_Pin=1, HA_Suggest=2, HA_InsAbove=3,
HA_InsBelow=4, HA_JoinSel=5, HA_FillGo=6` (`:22`).

### Algorithms
- `maxJoinableBytes()` (`:86`): sum currentKind size + contiguous following hex nodes of **same kind**
  until a gap/mismatch or total ≥16.
- `hexLine(typeName, bytes)` (`:97`): `"<type left-just 7> <ascii>  <HEX HEX …>"` (ascii: printable or
  `.`; hex upper, space-sep).
- `previewForKind(target)` (`:112`): same size → one line; smaller → split into `cur/tgt` lines;
  larger → merge data + adjacent same-kind, pad with `\0` to target.
- `infoForKind(target)` (`:140`): "current size" / "splits 1 X → N Y" / "joins N X → 1 Y" /
  "need N adjacent X to join".
- `computeSize()` (`:158`): row of size buttons + pin; preview width; pinned adds suggestion/insert/
  join/fill rows; min width 200.

### Paint (`:201-441`)
Manual border. **Size buttons**: current = `selected` bg + `indHoverSpan` border + text;
`canDo` = target ≤ current size OR ≤ joinable; hover highlight; disabled text `textFaint`. Pin icon
(pinned.svg/pin.svg). Preview lines (max 8 + "... +N more"), info text. **Pinned extras**: smart
suggestion buttons (ptr*/float/utf8 → emit `sizeSelected` with Pointer64/Float/UTF8), "+ hex64 above"
/"+ hex64 below", multi-select join button (joinKind by total bytes 2/4/8/16; valid only if
contiguous & power-of-two), and **fill-to-offset** row (label + `m_offsetEdit` + "Go" button parsed
hex). Keyboard-focus ring drawn around `m_hits[m_hoveredBtn]`.

### Interaction
`mouseMoveEvent`→`update()`. `mousePressEvent` (`:448`): hit-test enabled rects → dispatch (Pin
toggles mode; Size emits if different + hides if not pinned; Suggest emits + hides; Ins/Join/FillGo
emit; FillGo parses `m_offsetEdit` as hex16). Click outside → hide+`dismissed` if not pinned.
`keyPressEvent` (`:494`): Tab/arrows cycle enabled hits (`m_hoveredBtn`), Enter/Space synthesizes a
click at the hit center (reuses `mousePressEvent`), Escape unpins or hides+`dismissed`.

Port note: the synthetic-mouse-event trick (`:533`) becomes a direct dispatch call.

---

## 13. fuzzy_match.h and the THREE distinct fuzzy scorers

There are **three independent fuzzy implementations** with different semantics. The port should map
all to `nucleo` **but preserve the relative ordering each produced** where tests depend on it.

1. **`rcx::fuzzyScore` (`widgets/fuzzy_match.h:34`)** — used by TypeSelectorPopup, EnumPickerPopup.
   Two-pass strict matcher (`kMaxFuzzyLen=64`): empty pattern→1; `pLen>tLen`→0; `tLen>4096`→0.
   - **Pass 1** contiguous case-insensitive substring (`indexOf`): base 1000; +500 prefix(idx==0),
     +200 after `_/space/:/.−`, +150 CamelCase boundary; + tightness `max(0,100-(tLen-pLen))`;
     +200 if exact length. Fills `outPositions` with consecutive indices.
   - **Pass 2** word-start initials: a position is "matchable" at index 0, after `_/space/:/.−`, at
     CamelCase upper-after-lower, letter→digit, digit→letter, or any digit. All pattern chars must hit
     matchable positions in order; miss→0. Score ~600 (first hit at 0) or ~400, + `max(0,50-(span-pLen))`.
     Rejects scattered subsequence noise but accepts "GPA"→"GetProcAddress", "u32"→"uint32_t".

2. **`SourceChooserPopup::fuzzyScore` (`sourcechooserpopup.cpp:25`)** — recursive backtracking
   (`kMaxFuzzyLen=64`, `tLen` cap 256, fallback to prefix check beyond). Per-char bonus: 10 at idx0,
   8 after `_`/space, 8 at CamelCase; +5 for contiguous (i == prevPos+1); branch cap 4; + tightness
   `max(0,20-(tLen-pLen))`, +20 exact length. Returns best-scoring match positions.

3. **`CommandPalette::fuzzyScore` (`commandpalette.h:132`)** — linear single-pass over haystack.
   Per match: base 1, +2 if contiguous run, +3 if previous char was a separator (` >/_-`),
   +1 leading-prefix (hi==ni). Returns 0 if not all needle consumed. Empty needle→1. Used for
   "Menu > Item" paths. Test-locked: word-start beats mid-word; acronym works; case-insensitive.

**Port guidance:** use `nucleo` for matching + highlight positions; replicate the three score
*priorities* (prefix > separator/word-start > CamelCase > internal; contiguity bonus) so the same
entries float to the top. The match-position arrays drive per-character highlight painting.

---

## 14. CategoryChip (`widgets/category_chip.h`, header-only)

Flat custom-painted toggle pill (`QAbstractButton`), checkable, checked by default, pointing-hand
cursor, hover-attr (`category_chip.h:14`). Originally inline in TypeSelectorPopup; extracted for reuse.

- `setCount(n)` / `setCount(visible,total)` / `setGroupColor(QColor)` / `setLabel(QString)`.
- `chipText()`: `label`, or `"label (n)"`, or `"label (visible/total)"` when `total!=count`.
- `sizeHint()` = `5 + 4 + textWidth + 16` × `fm.height()+4`.
- `paintEvent`: hover fills `hover`; draws a 5×5 pip (group color if checked, else `textFaint`) + text
  (group color if checked, else `textMuted`), centered block, baseline-aligned.

Tests: `test_chips.cpp` actually targets the **compose chip data path** (LineMeta::chips: Enum→
TypeHint→Rtti→Comment ordering, glyph/text, startCol/endCol spans) — that is an **editor/compose**
concern, NOT this `CategoryChip` widget. Note this in the port: `test_chips` belongs to the editor
subsystem despite the name overlap.

---

## 15. RcxTooltip + GlobalTooltipBridge (`rcxtooltip.h`, `tooltip_bridge.h`)

### RcxTooltip (`rcxtooltip.h:37`)
Custom arrow tooltip: rounded-rect body + triangular arrow whose tip touches the anchor. Pure
`QPainter` on a translucent layered window. `Qt::ToolTip|FramelessWindowHint`; attributes
`WA_TranslucentBackground`, `WA_ShowWithoutActivating`, **`WA_TransparentForMouseEvents`** (critical:
stops the hover/leave flicker loop), `DarkTitleBar` property=true (prevents DWM dark-mode call that
breaks layered alpha on Windows). Constants: `kArrowH=8, kArrowW=14, kRadius=6, kPad=10, kGap=4,
kMaxW=550`.

- `setTheme(bg,border,title,body,sep)`.
- `populate(title, body, font)`: skips if unchanged & visible; splits body on `\n`; font scaled to
  **0.9×**; bold variant; `recalc()` (`:80`).
- `populateRich(title, QVector<TipLine>, font)`: rich per-segment lines (`struct TipSpan{text,color,
  bold,keyCap}`; `TipLine = QVector<TipSpan>`). Keycap spans render as outlined keyboard keys at
  **0.70×** font, uniform width = widest keycap (`m_maxKeyW`).
- `showAt(anchor, preferAbove=false)` (`:118`): chooses above/below; `preferAbove` flips down only if
  no room above, else legacy (below if fits). Horizontally bounded to screen; arrow X (`m_ax`)
  recomputed so the tip stays over the anchor. `setFixedSize`, move, show, update.
- `dismiss()`; `onMouseMove` callback hook.
- `recalc()` (`:264`): measures width (capped `kMaxW`) + height from title + lines (keycap rows taller).

`sharedRcxTooltip()` (`:335`): lazy process-wide singleton. `showRcxTooltip(globalAnchor,text,font)`
sets theme (`backgroundAlt/border/text/textDim/border`) + populate + showAt. `dismissRcxTooltip()`.

Tests (`test_tooltip_event.cpp`): arrow-below → `y()==anchor.y`; arrow-above →
`y()+height()==anchor.y`; stays within left/right screen edges; **wider content → wider tooltip**.

### GlobalTooltipBridge (`tooltip_bridge.h:24`)
App-wide `QObject` event filter on `qApp` replacing Qt's default tooltip with RcxTooltip for every
`setToolTip(...)` call. **No `Q_OBJECT`** (header-only, eventFilter-only).
- On `QEvent::ToolTip`: read `widget->toolTip()`. Empty → clear target + dismiss, return false.
  **Idempotent guard**: same widget + same text + already visible → return true (no re-show) — fixes
  per-tick flicker on a stationary mouse. Else set `m_target`/`m_lastText`,
  `showRcxTooltip(globalPos, tip, font)`, return true (suppress Qt's tooltip).
- Dismissal is **narrow**: `WindowDeactivate`/`FocusOut` → clear + dismiss; `MouseButtonPress` →
  dismiss but keep target/text; `Leave` → dismiss only if `obj == m_target` (so a child widget's Leave
  doesn't kill the tip).
- `tooltipTarget()` test accessor; `m_target` is a `QPointer<QWidget>`.

Tests (`test_tooltip_flicker.cpp`): install a Show/Hide counter on `sharedRcxTooltip()`; drive a
mouse-move stream over an editor row carrying Rtti+Comment chips; **a healthy tooltip = 1 show / 0
hide** over the stream (no flicker cycling).

Port: GPUI tooltip surface + a global hover-state owner that tracks "which element owns the visible
tip" and only dismisses on that element's leave / focus loss / click / window deactivate.

---

## 16. RttiBrowserDialog (`rttibrowser.h`, header-only)

Modal viewer of a parsed `RttiInfo`. `ThemedDialog` subclass, 720×480, modal. Title
`"RTTI · <demangled|raw>"` (`rttibrowser.h:36`). Constructor takes `(const RttiInfo&, Provider*=nullptr
[unused], parent)`.

Layout: a rich-text header (`<b>name</b> (abi)`, raw name if different, then
`vtable 0x… · module X · imagebase 0x… · COL 0x… · offset N`); a `QTabWidget` with:
- **Hierarchy** tab: tree (Class | Raw) listing `info.bases` in order; tab label "Hierarchy (N)".
- **Vtable** tab: tree (Slot | Address | Symbol) listing `info.vtable` (symbol or "(no symbol)");
  tab label "Vtable (N)".
Buttons: "Copy as tree" (Secondary, left) copies `buildTextReport(m_info)` to clipboard; "Close"
(Primary, right).

`static QString buildTextReport(const RttiInfo&)` (`:139`): plaintext dump — Class/ABI/Raw/Vtable/
Module/COL/Offset lines, then "Hierarchy (N):" with base names, then "Vtable (N):" with
`  [slot 2-wide] 0x<addr>  <symbol|(no symbol)>`. Used by Copy + tests.

Port: render from an `RttiInfo` struct (from the rtti subsystem); the text-report builder is a pure
function worth porting verbatim for parity.

---

## 17. ScannerPanel (`scannerpanel.h`, `scannerpanel.cpp` — 117KB)

A `QWidget` (not a dialog) embedding a memory scanner (Cheat-Engine-like). Wraps the **`ScanEngine`**
(scanner subsystem — separate). Reads memory via a `Provider` supplied by a getter. UI-heavy; most of
the 117KB is widget orchestration. Key port surface below.

### Custom delegate
`AddressDelegate : QStyledItemDelegate` (`scannerpanel.h:19`) — paints addresses with a dimmed
high-byte prefix; `dimColor`/`brightColor` fields; `paint()` override.

### Public API (`scannerpanel.h:32-97`)
- `ScannerPanel(parent)`.
- `setProviderGetter(ProviderGetter = function<shared_ptr<Provider>()>)`.
- `struct StructBounds { uint64 start=0; uint64 size=0; }`; `setBoundsGetter(BoundsGetter)`.
- `setEditorFont(font)`, `applyTheme(theme)`.
- Many test accessors returning the inner widgets (modeCombo, patternEdit, typeCombo, valueEdit,
  value2Edit, exec/write/privateOnly/skipSystem/userModeOnly/fastScan checks, fastScanCombo, scan/
  update/newScan/undo/goto/copy buttons, progressBar, resultsTable, statusLabel, resultFilter,
  condCombo, condLabel, structOnlyCheck) + `engine()`, `results()`.
- `bool saveResultsTo(path) const` / `bool loadResultsFrom(path)`.
- `void saveSettings(key="scanner") const` / `void loadSettings(key="scanner")`.
- Blocking automation entrypoints (for MCP): `runValueScanAndWait(valueType, value, filterExec=false,
  filterWritable=false, constrainRegions={})`, `runPatternScanAndWait(pattern, …)` (×2 overloads, one
  taking an explicit provider).
- **Signal:** `goToAddress(uint64 address)`.

### Serialization formats (port verbatim)
- **Results JSON** (`scannerpanel.cpp:2392-2443`): root object `{ "version":1, "scanMode":int,
  "valueType":int, "count":int, "results":[ { "address": hexstring, "value": hexstring (toHex),
  "module"?: string } ] }`. Load requires `version==1`; address parsed base-16, value
  `QByteArray::fromHex`. After load: repopulate table, enable Update if non-empty, show New Scan if
  non-empty, status "Loaded N result(s) from <file>".
- **Settings** (`scannerpanel.cpp:2445-2471`): `QSettings("Reclass","Reclass")` group `<key>` with
  keys `mode, valueType, condition, filterExec, filterWrite, privateOnly, skipSystem, userMode` —
  each restored only `if contains`.

### Behavioral notes
- Mode: Signature vs Value (`m_modeCombo`); value types via `m_typeCombo`; scan conditions
  (Exact/Unknown/Changed/…) via `m_condCombo`; Between needs two value edits.
- Filters: executable / writable / structOnly / privateOnly (skip Image/Mapped) / skipSystem /
  userModeOnly / fastScan (hidden stub) + alignment combo (1/4/8/16/32/64).
- **Undo stack** (`m_undoStack`, capped **16**): snapshots `m_results` before each Next Scan;
  `pushUndoSnapshot`/`popUndoSnapshot` (Cheat-Engine "undo over-narrow").
- Workflow tracking: `m_scanGeneration` (0 none, 1 first, 2+ rescan), stage breadcrumb label,
  truncation banner "Displaying N of M".
- Async scans run on the `ScanEngine` and report via `onScanFinished`/`onRescanFinished` slots;
  progress via `m_progressBar`.

Port: this is a large stateful panel; treat `ScanEngine` as the existing scanner crate and re-create
the panel's state machine + the two serialization formats exactly.

---

## 18. Profiler + ProfilerDialog (`profiler.h/.cpp`, `profilerdialog.h/.cpp`)

### Profiler (`profiler.h`) — concurrency-bearing
Singleton aggregating per-name timing.
- `struct ProfileStats { qint64 totalNs=0; minNs=MAX; maxNs=0; lastNs=0; count=0; }` (`profiler.h:12`).
- `Profiler::instance()` (singleton). `setEnabled(bool)`/`isEnabled()` via **`QAtomicInt`**
  (relaxed). `record(name:const char*, nanos)` — early-returns if disabled, else **`QMutex`-locked**
  update of the `QHash<QString,ProfileStats>` (`profiler.cpp:11-20`). `snapshot()` returns a locked
  copy; `reset()` clears under lock.
- `ProfileScope` (RAII): captures enabled state at ctor (all-or-nothing), starts `QElapsedTimer`,
  records `nsecsElapsed()` on dtor. `name` must be a string literal (captured by pointer).
  Macro `PROFILE_SCOPE(literalName)` with `__LINE__` token-paste for unique locals.

Port: `Mutex<HashMap<&'static str, ProfileStats>>` + an `AtomicBool` + an RAII guard using
`Instant`. Disabled fast-path must stay branch-cheap.

### ProfilerDialog (`profilerdialog.h/.cpp`)
`ThemedDialog`, 820×640. Live perf view: top horizontal bar chart of 15 hottest functions by total
time; bottom 7-col table (Function/Count/Total ms/Mean µs/Min µs/Max µs/Last µs). Auto-refresh
**~2 Hz** via `QTimer(500ms)` started/stopped in `showEvent`/`hideEvent` (`profilerdialog.cpp:202-211`).

- Top row: enable checkbox (toggles `Profiler::setEnabled`), summary label, "Reset" (clears profiler),
  "Copy CSV" (dumps `name,count,total_ms,mean_us,min_us,max_us,last_us`) (`:142-160`).
- `BarChart` inner `QWidget` (`:29-101`): bars proportional to `totalNs`; top entry full width; bar
  color by rank (indHeatHot / Warm / Cold / indHoverSpan); label elided, value "X ms ×count"; empty →
  "(no samples — enable profiling above)".
- `refreshData()` (`:223`): snapshot → vector → `std::sort` by `totalNs` desc → rebuild chart (top 15)
  / table (resize cols only when row count changes) / summary ("N buckets · M samples · T ms total").

---

## 19. WorkspaceModel (`workspace_model.h`, header-only)

Model + delegate for the project-explorer tree (structs/enums per tab). Built on
`QStandardItemModel` + a filtering proxy + a rich custom delegate.

### Data roles (`workspace_model.h:16-25`)
`Qt::UserRole+0` = `QDockWidget*` (as void*), `+1` = node id (uint64), `+2` = isEnum (bool),
`+3` = isViewed, `+4` = isPinned, `+5` = `RoleSectionHeader` (QString; non-empty ⇒ section header),
`+6` = `RoleDirty`.

### `struct TabInfo { const NodeTree* tree; QString name; void* subPtr; }` (`:27`).

### Free helpers
- `isHexPad(NodeKind)` — Hex8/16/32/64 are padding (filtered from member lists).
- `buildStructChildren(item, tree, structId, subPtr)` (`:40`): sort members by offset, skip hex-pad,
  child display = `"<typeName> <name>"` where typeName = struct's `structTypeName`/keyword or
  `kindToString`.
- `typeDisplayString(node, tree)` (`:72`): enum → `"Name — <memberCount>"`; struct → `"Name —
  <visibleFieldCount>"` (em-dash U+2014).
- `makeTypeItem(node, tree, subPtr)` (`:90`): icon enum/struct SVG; sets roles; builds children for
  structs.
- `makeSectionItem(label)` (`:109`): non-interactive header (`ItemIsEnabled` only, `RoleSectionHeader`
  set).
- `buildProjectExplorer(model, tabs, pinnedIds={})` (`:117`): clears, header "Name". Gathers all
  top-level Struct nodes across tabs. **PINNED** section (only if any pinned) then **ALL TYPES**
  (structs then enums). `syncProjectExplorer` = same, "debounced at 50ms" by caller.

### WorkspaceProxyModel (`:170`)
`QSortFilterProxyModel` subclass with `setHasFilter(bool)`. `filterAcceptsRow`: when a filter is
active, **hide section headers** and apply the base filter; when inactive, headers pass and base
filter applies. `invalidateFilter()` on change.

### WorkspaceDelegate (`:197`)
`QStyledItemDelegate` with `setThemeColors(theme)` caching ~12 colors. `sizeHint`: section headers
`fm.height()+16`; children `+6`, top-level `+10`. `paint`:
- **Section header**: bg `background`, label at **0.67× font** with 1.2 letter-spacing in `textMuted`,
  trailing 0.5px hairline.
- **Normal item**: selection bg `selected` + 2px left accent bar (`borderFocused`, inset 4px);
  hover bg `hover`. A square **letter badge** (S/E top-level by isEnum, F for children) in `badgeBg`,
  rounded 3px; letter `badgeText` (alpha 100 if not viewed). Top-level: split "Name — count" into
  name (left, `text`, elided) + a right-edge **count pill** (`surface` bg, `textMuted` text) + optional
  pin icon. Child: split "TypeName fieldName" → type in `syntaxType`, field in `textDim`.

Port: a workspace tree model + render closures honoring these roles, the filter-hides-headers rule,
the em-dash split, and the per-item rich paint (badge / count pill / pin / accent bar).

---

## 20. HoverPreviewRegistry (`widgets/hover_preview.h`, header-only)

Pluggable hover-preview registry (the editor's HoverPopupHost shows one of N preview views of the
hovered row). Adding a view = one `HoverPreview` subclass + `HoverPreviewRegistry::add()`.

- `struct HoverContext` (`hover_preview.h:36`): `editorFont`, `theme*`, `dataProvider*` (snapshot or
  real), `codeProvider*` (always real — for arbitrary code/pointer-target reads outside snapshot
  pages), `tree*`, `history*` (per-nodeId `ValueHistory` map, nullptr-safe).
- `class HoverPreview` (abstract, `:52`): `id()` (stable QSettings key), `tabLabel()`,
  `subtitle(LineMeta)` (optional richer title, default empty), `eligible(lm, node, ctx) const` (cheap,
  every tick), `widget(lm, node, ctx, parent)` (build/refresh content; nullptr ⇒ "eligible but no
  content this tick" → host hides; may cache + reuse).
- `class HoverPreviewRegistry` (`:92`): `add(unique_ptr<HoverPreview>)` (takes ownership; registration
  order = default tie-break), `eligibleFor(lm, node, ctx) -> QVector<HoverPreview*>` (in reg order),
  `size()`.

Port: a trait `HoverPreview { id, tab_label, subtitle, eligible, widget }` + a registry `Vec<Box<dyn
HoverPreview>>`; host owns dwell timing, anchor, per-node-kind last-pick persistence, Tab/Shift+Tab
cycling. The concrete previews live in the editor subsystem.

---

## 21. ClipboardCodec (`clipboard.h`, header-only)

Serializes node selections to/from the clipboard. **No `QApplication`/`QClipboard` dependency** —
callers do the `QMimeData` roundtrip (keeps it usable in QtCore-only tests). MIME type
`"application/x-reclass-nodes-v1"` (`clipboard.h:33`).

### Methods (all static)
- `collectSubtrees(tree, roots) -> QVector<Node>` (`:37`): iterative, cycle-safe (`QSet seen`) DFS
  collecting each root + all descendants via `tree.childrenOf`.
- `serialize(tree, rootIds, clearParentFor={}) -> QMimeData*` (`:58`): JSON payload
  `{ "schema":"rcx-clipboard/v1", "roots":[idstrings], "nodes":[Node::toJson...] }`; for ids in
  `clearParentFor`, sets `parentId=0` (decouple from old parent). Also sets plain-text via `plainDump`.
  Caller owns the returned object.
- `deserialize(tree&, mime) -> PasteResult { nodes, rootIds }` (`:97`): require MIME format +
  `schema=="rcx-clipboard/v1"`. Parse nodes (`Node::fromJson`); **remap every old id → a fresh
  `tree.reserveId()`** (so pasted nodes never collide); rewire `id/parentId/refId` through the map
  (0 stays 0); remap root ids. Empty/invalid → empty result.
- `parseLenientHex(src, err=nullptr) -> QByteArray` (`:148`): tokenizes on any non-hex char; strips a
  leading `0x`/`0X` per token; **left-pads each odd token** so `"A"`→`0x0A` (not `0xA0`); concatenates
  left→right. Mid-token stray letter (incl. mid-token `x`) → malformed (empty + err). Empty/no hex →
  empty (err "No hex data"). Accepts `"DE AD BE EF"`, `"DEADBEEF"`, `"0xDEADBEEF"`,
  `"{0xDE, 0xAD, 0xBE}"`, `"DE,AD"`, `"1 2 3"`→`01 02 03`, `"0x100"`→`01 00`.
- `plainDump(tree, rootIds) -> QString` (`:212`): one node per line via `dumpNode`:
  `"<indent depth*2 spaces>+0x<offset 4-wide hex>  <kindName left-just 8>  <name>"`, recursive.

`test_clipboard.cpp` exercises the codec (roundtrip id-remap, lenient-hex cases).

---

## 22. drawTabSourceIcon (`tab_source_icon.h`, header-only)

`inline void drawTabSourceIcon(painter, iconRect, iconPath, live:bool, tint:QColor)`
(`tab_source_icon.h:21`). Renders an SVG tinted to `tint` via `CompositionMode_SourceIn`, crisp at any
DPI (**pixmap sized at logical × dpr and `setDevicePixelRatio` set BEFORE the painter attaches** —
otherwise blurry). `live=false` → opacity ×0.40 (muted/disconnected look). Shared by the live
doc-tab paint path and `test_tab_source_icon.cpp` (samples rendered pixels — guarantees test/live
parity).

Port: a tinted-SVG draw helper honoring the DPR-before-paint ordering and the 0.40 muted alpha.

---

## 23. Concurrency & platform summary

- **Threading:** only `Profiler` is genuinely concurrent (mutex + atomic, records from any thread).
  `ScannerPanel` runs scans async on `ScanEngine` (engine owns threading) and marshals results back
  via queued slots. Everything else is main/UI-thread only.
- **Platform-specific:** `ProcessPicker::enumerateProcesses` (`#ifdef _WIN32` toolhelp/psapi/shell vs
  `#elif __linux__` `/proc`). `RcxTooltip`'s `DarkTitleBar` property is a Windows-DWM workaround
  (no-op elsewhere). All popups note menu-bar/`setNativeMenuBar(false)` cross-platform parity.
  Keep these behind `#[cfg(...)]`; Linux build can compile-verify with empty/`/proc` paths.

## 24. Open questions / discrepancies to confirm

1. **OptionsDialog font count:** source adds 3 fonts (`optionsdialog.cpp:111-113`) but
   `test_options_dialog.cpp:81` asserts `count()==2`. Port to the **source** (3 fonts); the test
   likely lags the source. Confirm against the live build if possible.
2. **`test_chips.cpp` naming:** despite the `widgets-dialogs` hint listing `test_chips`, that test
   targets the **compose** chip-data pipeline (LineMeta::chips), not `CategoryChip`. The `CategoryChip`
   widget has no dedicated test; verify whether any GUI test paints it.
3. **HexToolbarPopup pinned mode** uses `Qt::Tool|WindowStaysOnTopHint` (a real top-level persistent
   window) vs `Qt::Popup`. GPUI's equivalent (a non-auto-dismiss floating panel) must be chosen.
4. **Three fuzzy scorers** have subtly different tie-breaks; tests only pin the CommandPalette one
   strongly. Decide whether to unify on a single `nucleo`-backed scorer (risking minor ordering
   drift in the type/source popups) or replicate all three priority tables.
