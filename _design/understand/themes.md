# Subsystem: Theme System (`themes`)

Faithful-port structural/behavioral map of the Reclass C++/Qt6 theme subsystem.
Sources studied (all read in full):

- `src/themes/theme.h`, `src/themes/theme.cpp`
- `src/themes/thememanager.h`, `src/themes/thememanager.cpp`
- `src/themes/themeeditor.h`, `src/themes/themeeditor.cpp`
- `src/themes/defaults/*.json` (8 files)
- `tests/test_theme.cpp`
- Supporting: `src/widgets/themed_dialog.h`, `src/widgets/dialog_button.h`, `CMakeLists.txt` (theme-copy rules), `src/main.cpp` (signal consumer)

Expected portability: **mostly-portable**. The model (`Theme`) and manager (`ThemeManager`) are pure logic + filesystem + a settings store and a single change-notification signal — fully portable (serde + std::fs + a config crate + a callback/observer). The editor (`ThemeEditor`) is Qt-Widgets UI and maps to GPUI in the Rust port.

---

## 1. Purpose

A theme is a flat, named bundle of ~31 named colors plus a display name. The subsystem:

1. **Models** a theme as a struct of `QColor` fields (`Theme`).
2. **Serializes** to/from a flat JSON object of `"key": "#RRGGBB"` strings, with a metadata table (`kThemeFields`) driving both serialization and the editor UI from one source of truth.
3. **Manages** a list of built-in themes (loaded from JSON files shipped next to the executable) plus user themes (loaded from the OS app-data dir), tracks the current selection, persists the selection name to `QSettings`, supports CRUD on user themes + overriding built-ins, and broadcasts changes via a `themeChanged(Theme)` signal. A singleton.
4. **Live preview**: a modal editor can push transient themes to the whole app and revert them on cancel.
5. **Edits**: `ThemeEditor` is a modal dialog with a per-color swatch + hex label + color picker, generated from `kThemeFields`.

Every other UI widget in the app subscribes to `ThemeManager::themeChanged` and re-styles itself; `MainWindow::applyTheme` maps theme colors into a `QPalette` and stylesheets. In the Rust/GPUI port, "apply theme" becomes "store the active `Theme` in shared state and trigger a re-render".

---

## 2. Key types

### 2.1 `struct rcx::Theme` (`theme.h:8-58`)

A flat value type (copyable, no invariants enforced by ctor). Fields, grouped exactly as in source, with the C++ comment meaning:

| Field | Group | Meaning (from source comments) |
|---|---|---|
| `QString name` | — | Theme display name; also the persistence key |
| `QColor background` | Chrome | editor bg, margin bg, window |
| `QColor backgroundAlt` | Chrome | panels, selected tab, tooltips |
| `QColor surface` | Chrome | alternateBase |
| `QColor border` | Chrome | separators, menu borders |
| `QColor borderFocused` | Chrome | window border when focused |
| `QColor button` | Chrome | button bg |
| `QColor text` | Text | primary text, caret, identifiers |
| `QColor textDim` | Text | margin fg, status bar |
| `QColor textMuted` | Text | inactive tab, disabled menu |
| `QColor textFaint` | Text | margin dim, hex dim |
| `QColor hover` | Interactive | row/tab/menu hover |
| `QColor selected` | Interactive | row selection highlight |
| `QColor selection` | Interactive | text selection background |
| `QColor syntaxKeyword` | Syntax | |
| `QColor syntaxNumber` | Syntax | |
| `QColor syntaxString` | Syntax | |
| `QColor syntaxComment` | Syntax | |
| `QColor syntaxPreproc` | Syntax | |
| `QColor syntaxType` | Syntax | custom types / GlobalClass |
| `QColor indHoverSpan` | Indicators | hover link text |
| `QColor indCmdPill` | Indicators | command-row pill bg |
| `QColor indDataChanged` | Indicators | changed data values (legacy fallback for old themes) |
| `QColor indHeatCold` | Indicators | heatmap lvl 1 (changed once) |
| `QColor indHeatWarm` | Indicators | heatmap lvl 2 (moderate) |
| `QColor indHeatHot` | Indicators | heatmap lvl 3 (frequent) |
| `QColor indHintGreen` | Indicators | comment/hint text |
| `QColor indRttiHint` | Indicators | RTTI vtable name hint (distinct from indHintGreen) |
| `QColor markerPtr` | Markers | null pointer |
| `QColor markerCycle` | Markers | cycle detection |
| `QColor markerError` | Markers | error row bg |
| `QColor focusGlow` | Presentation | MCP focus pulse (warm amber) |

Total: `name` + **30 color fields**. Note `kThemeFields` (the metadata table) has **31 entries** because `indHeatCold/Warm/Hot` are listed there for the editor/serialization even though their *defaults* are derived (see §3.2). Wait — count carefully: the struct declares 30 `QColor` members; `kThemeFields` lists all 30 of them (background…focusGlow). `kThemeFieldCount == 31`? Verify in §2.2.

**Methods:**

- `QJsonObject toJson() const` — serialize (§3.1).
- `static Theme fromJson(const QJsonObject&)` — deserialize with defaulting (§3.2).

Default-constructed `Theme`: `name` empty, all `QColor` members default-constructed = **invalid** (`QColor().isValid() == false`). Important: an invalid `QColor` is the "missing/unset" sentinel throughout this code. The Rust port needs an equivalent — use `Option<Rgb>` or a custom `Color` with an `is_valid()` notion. The simplest faithful model is `Option<Color>` per field, because `fromJson` and the heat-derivation logic branch on `.isValid()`.

**Rust mapping for `QColor`:** `QColor` here is only ever used as: an 8-bit-per-channel RGB color (alpha is never set or read in this subsystem), `.name()` → `#rrggbb` lowercase hex string, `QColor("#rrggbb")` parse, `.isValid()`, `.red()/.green()/.blue()`, and `.lighter(130)`. Recommend a small `Color { r:u8, g:u8, b:u8 }` plus `Option<Color>` for the "invalid" state, or a wrapper that records validity. The `csscolorparser` or `palette` crate can help, but a hand-rolled `#rrggbb` parse/format is trivially exact and avoids edge-case divergence (see §6 on `QColor` parsing rules).

### 2.2 `struct rcx::ThemeFieldMeta` (`theme.h:62-70`)

Drives both serialization and editor UI from one table.

```cpp
struct ThemeFieldMeta {
    const char*    key;     // JSON key
    const char*    label;   // display label (editor)
    const char*    group;   // section group name (editor headers)
    QColor Theme::*ptr;     // pointer-to-member into Theme
};
extern const ThemeFieldMeta kThemeFields[];
extern const int kThemeFieldCount;
```

`kThemeFields` is defined in `theme.cpp:9-41` with **30 entries**, one per color field, in this exact order (the order matters for editor layout and section grouping):

```
background, backgroundAlt, surface, border, borderFocused, button     [Chrome]
text, textDim, textMuted, textFaint                                    [Text]
hover, selected, selection                                             [Interactive]
syntaxKeyword, syntaxNumber, syntaxString, syntaxComment,
  syntaxPreproc, syntaxType                                            [Syntax]
indHoverSpan, indCmdPill, indDataChanged, indHeatCold, indHeatWarm,
  indHeatHot, indHintGreen, indRttiHint                                [Indicators]
markerPtr, markerCycle, markerError                                    [Markers]
focusGlow                                                              [Presentation]
```

Count: 6 + 4 + 3 + 6 + 8 + 3 + 1 = **31**. `kThemeFieldCount = std::extent_v<...>` = 31 (`theme.cpp:42`). So the struct has 30 color members but the metadata table has 31 entries — recount the struct members: Chrome 6, Text 4, Interactive 3, Syntax 6, Indicators **8** (indHoverSpan, indCmdPill, indDataChanged, indHeatCold, indHeatWarm, indHeatHot, indHintGreen, indRttiHint), Markers 3, Presentation 1 = **31**. Correct count is **31 color fields + name**. (The §2.1 table omitted nothing — re-add: it lists all 8 indicators. 31 total.)

**Labels & groups** (for editor; group string is compared with `strcmp` to detect section changes, so adjacent same-group entries share one header — `themeeditor.cpp:96`):

| key | label | group |
|---|---|---|
| background | Background | Chrome |
| backgroundAlt | Background Alt | Chrome |
| surface | Surface | Chrome |
| border | Border | Chrome |
| borderFocused | Border Focused | Chrome |
| button | Button | Chrome |
| text | Text | Text |
| textDim | Text Dim | Text |
| textMuted | Text Muted | Text |
| textFaint | Text Faint | Text |
| hover | Hover | Interactive |
| selected | Selected | Interactive |
| selection | Selection | Interactive |
| syntaxKeyword | Keyword | Syntax |
| syntaxNumber | Number | Syntax |
| syntaxString | String | Syntax |
| syntaxComment | Comment | Syntax |
| syntaxPreproc | Preprocessor | Syntax |
| syntaxType | Type | Syntax |
| indHoverSpan | Hover Span | Indicators |
| indCmdPill | Cmd Pill | Indicators |
| indDataChanged | Data Changed | Indicators |
| indHeatCold | Heat Cold | Indicators |
| indHeatWarm | Heat Warm | Indicators |
| indHeatHot | Heat Hot | Indicators |
| indHintGreen | Hint Green | Indicators |
| indRttiHint | RTTI Hint | Indicators |
| markerPtr | Pointer | Markers |
| markerCycle | Cycle | Markers |
| markerError | Error | Markers |
| focusGlow | Focus Glow | Presentation |

**Rust mapping:** the pointer-to-member table is the C++ way to iterate fields generically. In Rust, replicate with either: (a) a `&[(&str /*key*/, &str /*label*/, &str /*group*/, FieldId)]` table + a `match`-based getter/setter on `Theme`, or (b) derive serde `Serialize/Deserialize` for the flat key->hex map and a separate static metadata table for editor labels/groups. Keep the **exact key strings and order** — they define on-disk JSON key order (Qt `QJsonObject` actually re-sorts keys alphabetically on output; see §3.1 note) and the editor row order.

### 2.3 `class rcx::ThemeManager : public QObject` (`thememanager.h:8-45`)

Singleton, the single source of truth for the active theme and theme list.

Private state:
- `QVector<Theme> m_builtIn` — built-in themes, possibly overridden by a same-named user file.
- `QVector<Theme> m_builtInDefaults` — pristine copies loaded from disk (used to detect "modified" built-ins for save/diff).
- `QVector<Theme> m_user` — user themes (not overriding any built-in).
- `int m_currentIdx = 0` — index into the **concatenated** list `themes() == m_builtIn ++ m_user`.
- `bool m_previewing = false` — preview-mode flag.
- `Theme m_savedTheme` — the theme to restore when reverting a preview.

Public API detailed in §4.

`Q_OBJECT` + `signals: void themeChanged(const rcx::Theme&)`. In Rust: an observer list / `gpui` global with subscribers, or a callback registry. There is exactly one signal.

### 2.4 `class rcx::ThemeEditor : public ThemedDialog` (`themeeditor.h:15-43`)

Modal dialog. State:
- `Theme m_theme` — the working copy being edited.
- `int m_themeIndex` — index into `ThemeManager::themes()` currently selected.
- `QVector<SwatchEntry> m_swatches` — one per `kThemeFields` entry. `SwatchEntry { const char* label; QColor Theme::* field; QPushButton* swatchBtn; QLabel* hexLabel; }`.
- UI: `QComboBox* m_themeCombo`, `QLineEdit* m_nameEdit`, `QLabel* m_fileInfoLabel`.
- `Theme result() const` returns `m_theme`; `int selectedIndex() const` returns `m_themeIndex`.

Behavior in §5. Base class `ThemedDialog` (`widgets/themed_dialog.h`): a `QDialog` that applies the theme palette on construct + on `themeChanged`, and provides `static QHBoxLayout* makeButtonRow(initializer_list<QPushButton*>)` (right-aligned, stretch then buttons left-to-right; convention `{Cancel, OK}`).

---

## 3. Serialization format & algorithms

### 3.1 `Theme::toJson()` (`theme.cpp:44-50`)

```cpp
QJsonObject o;
o["name"] = name;                                  // string
for (i in 0..kThemeFieldCount)
    o[key[i]] = (this->*ptr[i]).name();            // QColor::name() => "#rrggbb"
return o;
```

- Writes `"name"` then every color field as its `#rrggbb` lowercase string.
- **Edge case — invalid color:** `QColor().name()` returns `"#000000"` for a default/invalid color (Qt: invalid QColor's `name()` yields `#000000`). So a *missing* field serialized via `toJson` becomes black, NOT preserved-as-missing. This matters only for round-tripping a sparse theme through `toJson` (it won't stay sparse). Shipped themes are fully populated, so this is rarely hit, but the **diff-on-save** logic (§4.6) compares `toJson()` output, so an invalid-color built-in field would serialize to `#000000`.
- **`QColor::name()` format:** default `QColor::HexRgb` → `#rrggbb`, lowercase hex, 6 digits, no alpha. The Rust port must format lowercase 6-digit (`format!("#{:02x}{:02x}{:02x}", r, g, b)`).
- **Key ordering on disk:** `QJsonObject` stores keys **sorted alphabetically**; `QJsonDocument::toJson(Indented)` emits them sorted. So the actual file key order is alphabetical, NOT the `kThemeFields` order, and `name` sorts among the color keys (between `markerPtr`-ish... actually `name` < `selected`...). The shipped `defaults/*.json` are hand-authored with `name` first and a curated order; those are read-only inputs. Files *written* by `saveUserThemes` will have alphabetical key order. For faithful behavior the Rust writer should also emit deterministic keys; matching Qt's alphabetical-sorted, 4-space-indented output exactly is only needed if byte-identical files matter (they don't — load is order-independent). **Recommendation:** emit pretty JSON; key order is not load-bearing for correctness.

### 3.2 `Theme::fromJson()` (`theme.cpp:52-108`) — the important algorithm

```cpp
Theme t;
t.name = o["name"].toString("Untitled");           // default "Untitled" if absent
for (i in 0..kThemeFieldCount)
    if (o.contains(key[i]))
        t.*ptr[i] = QColor(o[key[i]].toString());   // parse; invalid string => invalid QColor
// (fields NOT present in JSON stay invalid/default)
```

Then a series of **defaulting / derivation** steps applied only when the corresponding field is invalid (i.e. absent from JSON, or present but unparseable):

1. **Heat-color amber gradient** (`theme.cpp:69-80`). Rationale in comment: heat indicators must be amber, never red.
   - Local lambda `lerpRgb(a, b, f)` = component-wise linear interpolation, each channel `qBound(0, a.c + int((b.c - a.c) * f), 255)`. Note `int(...)` truncates toward zero; `qBound(lo,v,hi)` clamps.
   - `dim = t.textDim.isValid() ? t.textDim : QColor(133,133,133)`.
   - if `!indHeatCold.isValid()`: `indHeatCold = lerpRgb(dim, QColor(220,180,120), 0.35)`.
   - if `!indHeatWarm.isValid()`: `indHeatWarm = QColor(225,170,90)`.
   - if `!indHeatHot.isValid()`: `indHeatHot = QColor(232,165,92)`.
   - **None of the shipped themes ship `indHeatCold/Warm/Hot`** (verified: not present in any `defaults/*.json`), so these derivations always fire for built-ins. `indHeatCold` therefore depends on each theme's `textDim`.
2. **focusGlow** (`theme.cpp:82-83`): if invalid → `borderFocused.isValid() ? borderFocused : QColor("#4fc3f7")`. Only `phosphor.json` ships `focusGlow` explicitly; all others derive it from `borderFocused`.
3. **indRttiHint** (`theme.cpp:88-89`): if invalid → `QColor("#d19a66")`. (All shipped themes do ship `indRttiHint`, so this only fires for user themes that omit it.)
4. **Marker fallbacks** (`theme.cpp:95-97`): if invalid →
   - `markerPtr = #f44747`
   - `markerCycle = #e8a35c`
   - `markerError = #5a1d1d`
   (All shipped themes ship markers; fallbacks for user themes only.)
5. **Hover distinctness guard** (`theme.cpp:99-106`): if both `hover` and `background` are valid, compute Manhattan distance `dist = |Δr| + |Δg| + |Δb|`; if `dist < 20`, set `hover = background.lighter(130)`.
   - `QColor::lighter(150_default; here 130)`: Qt lightens by converting to HSV and scaling V by `factor/100` (130 ⇒ ×1.30), clamped. **Important nuance:** if V==0 (pure black), `lighter()` instead returns a gray derived by adding to value (Qt special-cases black so it doesn't stay black). Faithful Rust impl must replicate `QColor::lighter`: convert RGB→HSV, `v = min(255, v*factor/100)`, convert back; with Qt's black special-case (`if (s==0 && v==0) return a gray`). See §6.

**Return** the fully-defaulted theme. Note: `name`, all six Chrome fields, all Text/Interactive/Syntax fields, `indHoverSpan`, `indCmdPill`, `indDataChanged`, `indHintGreen` have **no fallback** — if a user JSON omits them they remain invalid (and `toJson` would later emit `#000000`). The test `fromJsonMissingFields` (§7) explicitly asserts that omitted `text`, `syntaxKeyword`, `markerError` stay invalid — wait, `markerError` *does* have a fallback (step 4). See §7 for the test-vs-code conflict.

**Edge cases:**
- `o["name"].toString("Untitled")`: if `name` key absent OR present-but-not-a-string → `"Untitled"`.
- `QColor(QString)` with an unrecognized/empty string yields an **invalid** QColor (no throw). So a malformed hex like `"notacolor"` leaves the field invalid → triggers fallback if one exists.
- The `lerpRgb` truncation (`int(...)`) and `qBound` clamping must be matched bit-for-bit; with the constants given, no clamping/overflow actually occurs for in-range dims, but match the formula.

### 3.3 Default theme files (`src/themes/defaults/*.json`)

8 files, each a flat JSON object. Sorted by **filename** (QDir::Name): `long_night, mid, modern, phosphor, reclass_dark, tw, vs, warm`. Their `"name"` fields (the display names, which differ from filenames):

| File | `name` |
|---|---|
| long_night.json | Long Night |
| mid.json | Mid |
| modern.json | Modern |
| phosphor.json | Phosphor |
| reclass_dark.json | Reclass Dark |
| tw.json | **Light** (filename `tw` but name "Light") |
| vs.json | VS2022 Dark |
| warm.json | Warm |

Each file ships: name + 6 Chrome + 4 Text + 3 Interactive + 6 Syntax + indHoverSpan, indCmdPill, indDataChanged, indHintGreen, indRttiHint (5 of 8 indicators) + 3 Markers. They **omit** `indHeatCold/Warm/Hot` (always derived) and `focusGlow` (derived except phosphor which ships it). So a loaded built-in theme is `fromJson`-processed and gains derived heat/focusGlow values.

These 8 JSON files must be shipped with the Rust port and located the same way (next to the binary, or bundled). In Rust, the cleanest faithful approach is `include_str!`/`include_dir!` to embed the 8 defaults at compile time (guarantees they exist regardless of install layout) — but note the C++ loads them from `<exe-dir>/themes/`, and `themeFilePath` reports that path to the editor. If exact filesystem behavior matters (the editor shows the file path), keep the on-disk `themes/` dir model; otherwise embedding is simpler. **Recommendation:** ship the JSON next to the binary to preserve `themeFilePath` semantics, OR embed + synthesize paths. Either is acceptable for parity since the path is only an informational label.

---

## 4. `ThemeManager` public API — exact behavior

### 4.0 Construction / singleton (`thememanager.cpp:11-31`)
- `static ThemeManager& instance()` — Meyers singleton (`static ThemeManager s;`). Thread-safe init in C++11. In Rust: `OnceLock<Mutex<ThemeManager>>` or a `gpui` global; but note this is accessed from the UI thread only in practice.
- Constructor: `loadBuiltInThemes()` then `loadUserThemes()`. Then:
  - Open `QSettings("Reclass","Reclass")`.
  - Compute `fallback`: first built-in whose name contains `"VS2022"` case-insensitively; else `m_builtIn[0].name`; else empty.
  - `saved = settings.value("theme", fallback)` (string).
  - Loop the concatenated `themes()`; set `m_currentIdx` to first index whose `name == saved`. If none match, `m_currentIdx` stays 0.
- **`QSettings("Reclass","Reclass")`** = org "Reclass", app "Reclass". On Windows: registry `HKCU\Software\Reclass\Reclass`; macOS: `~/Library/Preferences/com.Reclass.Reclass.plist` (or similar); Linux: `~/.config/Reclass/Reclass.conf` (INI). Only the key `"theme"` (a string) is used by this subsystem. **Rust mapping:** a small persisted config (e.g. `directories` crate for the path + a tiny INI/JSON, or the `confy`/`config` crate). Faithful behavior only needs: read/write one string keyed `"theme"`, persisted across runs, in a per-user location.

### 4.1 `QVector<Theme> themes() const` (`thememanager.cpp:60-64`)
Returns `m_builtIn` concatenated with `m_user` (a copy). This concatenation defines the index space used everywhere. Rust: return `Vec<Theme>` or expose `&[Theme]` via a builtin/user split.

### 4.2 `int currentIndex() const` (inline) → `m_currentIdx`.

### 4.3 `const Theme& current() const` (`thememanager.cpp:66-76`)
- If `m_currentIdx < m_builtIn.size()` → `m_builtIn[m_currentIdx]`.
- Else `userIdx = m_currentIdx - m_builtIn.size()`; if in range → `m_user[userIdx]`.
- Else if `m_builtIn` nonempty → `m_builtIn[0]`.
- Else a `static const Theme empty;` (a default, all-invalid theme). Edge: only reached if zero themes exist at all (no JSON found).

### 4.4 `void setCurrent(int index)` (`thememanager.cpp:78-85`)
- Bounds-check against `themes().size()`; if `index<0 || index>=size`, **no-op** (returns, no signal).
- Set `m_currentIdx = index`.
- Persist `settings["theme"] = all[index].name`.
- `emit themeChanged(current())`.
- Edge: always emits when in-range, even if index unchanged.

### 4.5 `void addTheme(const Theme& theme)` (`thememanager.cpp:87-90`)
- Append to `m_user`. `saveUserThemes()`. Does **not** change `m_currentIdx` and does **not** emit.

### 4.6 `void updateTheme(int index, const Theme& theme)` (`thememanager.cpp:92-107`)
- `m_previewing = false` (commit any active preview — meaning a later `revertPreview` becomes a no-op).
- If `index < builtInCount()`: `m_builtIn[index] = theme; m_currentIdx = index;` (updating a built-in also selects it).
- Else `ui = index - builtInCount()`; if in range, `m_user[ui] = theme`. (Does **not** change `m_currentIdx` in the user branch.)
- `saveUserThemes()`.
- Persist `settings["theme"] = current().name`.
- `emit themeChanged(current())`.
- Edge: out-of-range user index → still saves + persists + emits, but the theme isn't stored (silent drop). Out-of-range overall (index ≥ total or negative): the `else` branch runs with a negative/oob `ui`, stored nowhere; current unchanged.

### 4.7 `void removeTheme(int index)` (`thememanager.cpp:109-121`)
- If `index < builtInCount()`: **no-op** (built-ins can't be removed).
- `ui = index - builtInCount()`; bounds-check `m_user`; if bad → no-op.
- `m_user.remove(ui)`.
- Current-index fixup:
  - if `m_currentIdx == index`: reset `m_currentIdx = 0` and `emit themeChanged(current())`.
  - else if `m_currentIdx > index`: `m_currentIdx--` (no emit).
  - (else `m_currentIdx < index`: unchanged, no emit.)
- `saveUserThemes()`.
- Note: does NOT update the persisted `settings["theme"]` here (so on next launch the saved name may not exist → falls to index 0 via ctor logic; benign).

### 4.8 `void loadBuiltInThemes()` (`thememanager.cpp:44-56`)
- `m_builtIn.clear()`. `QDir dir(builtInDir())`. If dir doesn't exist → return (empty builtins).
- `dir.entryList({"*.json"}, QDir::Files, QDir::Name)` — files matching `*.json`, **sorted by name ascending** (case-insensitive on Windows, locale/case-sensitive on Unix; Qt's QDir::Name uses a locale-aware comparison — see §6). For each: open read-only (skip on failure), `QJsonDocument::fromJson(readAll())`; if `isObject()`, append `Theme::fromJson(obj)`.
- After loop: `m_builtInDefaults = m_builtIn` (snapshot of pristine built-ins for diffing).
- **Order** is the filename sort: long_night, mid, modern, phosphor, reclass_dark, tw, vs, warm → display names: Long Night, Mid, Modern, Phosphor, Reclass Dark, Light, VS2022 Dark, Warm.

### 4.9 `void loadUserThemes()` (`thememanager.cpp:132-154`)
- `m_user.clear()`. `QDir dir(userDir())` (creates it, see §4.12). `entryList({"*.json"}, QDir::Files)` — **note: no sort flag here**, so default `QDir::SortFlags` (which is `QDir::Name | QDir::IgnoreCase`? Actually default for `entryList(nameFilters, filters)` with no sort arg is the dir's current `sorting()` which defaults to `QDir::Name | QDir::IgnoreCase | QDir::DirsFirst`). Practically name-sorted.
- For each file: open RO (skip fail), parse, skip if not object. `t = Theme::fromJson(obj)`.
- **Override logic:** scan `m_builtIn`; if any has `name == t.name`, replace `m_builtIn[i] = t` (override in place), set `isOverride = true`, break.
- If not an override, append to `m_user`.
- Effect: a user file named to match a built-in's *display name* (e.g. a saved "VS2022 Dark") overrides the built-in's colors in the built-in slot; `m_builtInDefaults` is untouched, so the diff in `saveUserThemes`/`themeFilePath` still sees it as "modified".

### 4.10 `void saveUserThemes() const` (`thememanager.cpp:156-179`)
- `dir = userDir()`. **Delete all `*.json` in the user dir first** (`d.remove(name)` for each entry). This is a full rewrite each save.
- **Save modified built-ins:** for `i in 0..min(m_builtIn.size, m_builtInDefaults.size)`: if `m_builtIn[i].toJson() != m_builtInDefaults[i].toJson()` (QJsonObject inequality = deep compare), write file. Filename = `name.toLower().replace(' ', '_') + ".json"`. Write `QJsonDocument(toJson()).toJson(Indented)` (4-space indented).
- **Save user themes:** for each `m_user[i]`, same filename scheme, same write.
- Filename derivation: `m_builtIn[i].name.toLower().replace(' ', '_') + ".json"`. E.g. "VS2022 Dark" → `vs2022 dark` → `vs2022_dark.json`; "Reclass Dark" → `reclass_dark.json`. **Note** this does NOT match all original built-in filenames: "Light" (file `tw.json`) → `light.json`; "VS2022 Dark" → `vs2022_dark.json` (orig file `vs.json`). So a modified built-in is saved under a *different* filename than its source, living in the user dir; on next load, `loadUserThemes` matches it back by *display name* (override path). Round-trips correctly even though filename differs.
- **Edge:** two themes whose names collapse to the same filename (e.g. "Foo Bar" and "foo bar") would clobber each other — last write wins. Names are not deduplicated.
- `QString::toLower()` is locale-independent Unicode lowercase in Qt; `replace(' ','_')` replaces only ASCII space (U+0020), not tabs/other whitespace.

### 4.11 `QString builtInDir() const` (`thememanager.cpp:35-42`) — **platform-specific**
```cpp
#ifdef Q_OS_MACOS
    return applicationDirPath() + "/../Resources/themes";   // inside .app bundle
#else
    return applicationDirPath() + "/themes";                 // next to exe
#endif
```
- `QCoreApplication::applicationDirPath()` = dir containing the running executable.
- **Rust mapping:** macOS bundle layout vs. exe-adjacent `themes/`. Use `std::env::current_exe()` + `parent()`; on macOS resolve `../Resources/themes` from the `MacOS` dir. Keep behind `#[cfg(target_os = "macos")]` / `#[cfg(not(...))]`. Must keep compiling on Linux.

### 4.12 `QString userDir() const` (`thememanager.cpp:125-130`)
```cpp
dir = QStandardPaths::writableLocation(QStandardPaths::AppDataLocation) + "/themes";
QDir().mkpath(dir);   // ensure exists
return dir;
```
- `AppDataLocation` per OS (with org/app "Reclass"/"Reclass" from the app name set at startup — actually `QStandardPaths` uses `QCoreApplication::organizationName()`/`applicationName()`):
  - Windows: `C:/Users/<u>/AppData/Roaming/Reclass/Reclass/themes` (roaming) — actually AppDataLocation is `%APPDATA%/<org>/<app>` plus Qt may also include the app subdir.
  - macOS: `~/Library/Application Support/Reclass/Reclass/themes` (or `~/Library/Application Support/<app>`).
  - Linux: `~/.local/share/Reclass/Reclass/themes` (XDG_DATA_HOME based).
- **Rust mapping:** `directories::ProjectDirs::from("", "Reclass", "Reclass").data_dir()` + `/themes`, then `fs::create_dir_all`. Match the org/app naming used by the project's settings subsystem (see the `project`/settings analysis for the canonical names).

### 4.13 `QString themeFilePath(int index) const` (`thememanager.cpp:181-197`)
Returns the file path shown in the editor's info label. Logic:
- If `index < builtInCount()`:
  - If `index < m_builtInDefaults.size()` AND `m_builtIn[index].toJson() != m_builtInDefaults[index].toJson()` (i.e. this built-in was modified by a user override): return `userDir()/<lowername_underscored>.json`.
  - Else: return `builtInDir()/<lowername_underscored>.json` (the source file — note filename derived from display name, which for "Light"/"VS2022 Dark" does NOT match the actual source filename `tw.json`/`vs.json`; this is a cosmetic path, possibly nonexistent, just shown as text).
- Else user index: bounds-check; return `userDir()/<lowername_underscored>.json` or empty `{}` if oob.
- **Used only** by `ThemeEditor` to label the file (empty/unmodified-builtin → "Built-in theme (edits save as user copy)").

### 4.14 `void previewTheme(const Theme& theme)` (`thememanager.cpp:199-205`)
- If not already previewing: snapshot `m_savedTheme = current()` and set `m_previewing = true`.
- `emit themeChanged(theme)`.
- **Does NOT** change `m_currentIdx` or the stored theme — purely a transient broadcast. The whole UI re-styles to `theme` without committing.
- Re-entrant: subsequent `previewTheme` calls (e.g. on each color pick) just re-emit; `m_savedTheme` is captured only on the first.

### 4.15 `void revertPreview()` (`thememanager.cpp:207-212`)
- If `m_previewing`: clear flag, `emit themeChanged(m_savedTheme)`.
- If not previewing (e.g. `updateTheme` already committed): no-op.

### 4.16 `signals: themeChanged(const rcx::Theme&)`
The single broadcast. Consumers (`themed_dialog`, `dialog_button`, `controller`, `editor`, `main`, etc., ~7 direct `connect`s) re-apply theme to their widgets. In the Rust/GPUI port: store active theme in a global and notify observers / mark UI dirty. The argument is the **full Theme value** (by const-ref → pass by clone/`Arc` in Rust). Preview emits a theme that is NOT the current — consumers must use the *emitted* theme, not call `current()`.

---

## 5. `ThemeEditor` — UI behavior (maps to GPUI)

Pure Qt-Widgets; in Rust this becomes a GPUI view. Behavior to replicate:

### 5.1 Construction (`themeeditor.cpp:31-156`)
- `m_theme = all[themeIndex]` if index valid else `tm.current()`.
- Window title "Theme Editor", min size 420×480, initial 440×640.
- Layout: a `QVBoxLayout`, spacing 6.
- **Theme selector row:** label "Theme:" + `QComboBox` listing all theme names; current index set to `themeIndex`; `currentIndexChanged(int)` → `loadTheme(idx)`.
- **Name row:** label "Name:" + `QLineEdit(m_theme.name)`; `textChanged` → `m_theme.name = t` (live).
- **File info label:** `m_fileInfoLabel`; text = `themeFilePath(themeIndex)` → "File: <path>" or, if empty, "Built-in theme (edits save as user copy)". Styled with `textDim`.
- **Scroll area** of swatch rows, generated from `kThemeFields`:
  - On group change (strcmp), insert a section header label (`makeSectionLabel`: bold 11px, color `textMuted`, bottom border `border`).
  - Each row: a fixed-width(120) label (the field's `label`), a 32×18 `QPushButton` swatch (PointingHand cursor), a fixed-width(60) hex `QLabel` (color `textMuted`), then stretch.
  - Swatch click → `pickColor(idx)`.
  - `SwatchEntry{label, field(ptr-to-member), swatchBtn, hexLabel}` stored in `m_swatches` (parallel to `kThemeFields`, same order/count).
- **Bottom bar:** `DialogButton "Cancel" (Secondary)` and `DialogButton "Save theme" (Primary)`.
  - Apply/Save → `QDialog::accept()` (closes with Accepted; caller reads `result()`/`selectedIndex()` and presumably calls `ThemeManager::updateTheme`/`addTheme`).
  - Cancel → `ThemeManager::revertPreview()` then `reject()`.
  - `applyBtn->setDefault(true)`.
- After building: `updateSwatch(i)` for all, then `tm.previewTheme(m_theme)` — **starts live preview immediately on open**.

### 5.2 `loadTheme(int index)` (`themeeditor.cpp:160-178`)
- Bounds-check vs `themes()`; oob → return.
- Set `m_themeIndex`, `m_theme = all[index]`, update name edit, update file-info label, refresh all swatches, then `tm.previewTheme(m_theme)`.
- Note: switching the combo discards unsaved edits to the previously selected theme (no confirm).

### 5.3 `updateSwatch(int idx)` (`themeeditor.cpp:182-193`)
- `c = m_theme.*field`. Style the swatch button bg=`c.name()`, border 1px `current().border`, radius 2px. Set hex label text = `c.name()`.
- Uses `ThemeManager::instance().current()` for the *editor's own* border color — so the editor chrome reflects the live (previewed) current theme.

### 5.4 `pickColor(int idx)` (`themeeditor.cpp:197-205`)
- `c = QColorDialog::getColor(currentFieldColor, this, label)`.
- If `c.isValid()` (user didn't cancel): `m_theme.*field = c`, `updateSwatch(idx)`, `tm.previewTheme(m_theme)` (live update).
- If invalid (cancelled): no change.
- **Rust/GPUI:** replace `QColorDialog` with a GPUI color picker; the swatch grid + section headers + name edit + combo are straightforward GPUI elements. Preview-on-edit and revert-on-cancel semantics must be preserved by calling the equivalent `preview_theme`/`revert_preview` on the manager.

`makeSectionLabel` (`themeeditor.cpp:15-27`): bold 11px label colored `textMuted` with bottom border `border`, both pulled live from `current()` so dividers re-tint on preview.

---

## 6. Qt → Rust type/behavior mapping (cheat sheet)

| Qt construct | Used for | Rust equivalent / note |
|---|---|---|
| `QColor` | RGB color, validity sentinel | `struct Color{r,g,b:u8}` + `Option<Color>` for invalid; hand-roll parse/format |
| `QColor("#rrggbb")` | parse hex | parse 6-digit hex; invalid string → `None`. Qt also accepts `#rgb`, `#rrggbbaa`, SVG color names — but inputs here are only 6-digit hex |
| `QColor::name()` | `#rrggbb` lowercase | `format!("#{:02x}{:02x}{:02x}",..)`. **Invalid QColor → `"#000000"`** |
| `QColor::isValid()` | sentinel | `Option::is_some()` |
| `QColor::lighter(130)` | hover guard | HSV V×1.30 clamped; Qt special-cases pure black. Replicate exactly (see below) |
| `qBound(lo,v,hi)` | clamp | `v.clamp(lo,hi)` |
| `qAbs` | abs | `.abs()` |
| `QString` | strings/names | `String` |
| `QString::toLower()` | filename | Unicode lowercase; `str::to_lowercase()` (close enough for ASCII names) |
| `QString::replace(' ','_')` | filename | `.replace(' ', "_")` |
| `QString::contains(x, CaseInsensitive)` | VS2022 fallback detect | `.to_lowercase().contains("vs2022")` |
| `QJsonObject`/`QJsonDocument` | (de)serialize | `serde_json::Map`/`Value`; pretty = `to_string_pretty` |
| `QJsonDocument::toJson(Indented)` | write | serde_json pretty (4-space differs from Qt; not load-bearing) |
| `QVector<Theme>` | lists | `Vec<Theme>` |
| `QDir::entryList({"*.json"}, Files, Name)` | sorted file list | read_dir, filter `.json`, **sort by name** — match QDir::Name ordering (see below) |
| `QStandardPaths::AppDataLocation` | user dir | `directories::ProjectDirs(...).data_dir()` |
| `QCoreApplication::applicationDirPath()` | builtin dir | `current_exe().parent()` |
| `QSettings("Reclass","Reclass")` | persist `"theme"` | small per-user config store; one string key |
| `QObject` signal `themeChanged` | broadcast | observer/callback list, or GPUI global + notify |
| `QColorDialog::getColor` | editor picker | GPUI color picker |
| `Q_OS_MACOS` cfg | bundle path | `#[cfg(target_os="macos")]` |

**`QColor::lighter(factor)` exact algorithm** (Qt source): convert to HSV; `v = min(255, (v * factor) / 100)`; if the color is achromatic black special path… precisely, Qt does: `if (factor <= 0) return *this; if (factor < 100) return darker(...)`. For `factor==130 (>100)`: `QColor hsv = toHsv(); int hue,sat,val,alpha; hsv.getHsv(...); val = qMin(255, (val*factor)/100); hsv.setHsv(hue, sat, val, alpha); return hsv.toRgb();`. For pure black (val==0) this stays black (no special add — Qt's `lighter` does NOT brighten pure black). **Caveat:** the hover-distinctness guard only fires when `dist < 20`; among shipped themes hover and background are always >20 apart, so this path is effectively never taken for built-ins. It can fire for user themes. Replicate the HSV-scale for correctness.

**`QDir::Name` sort:** for the built-in dir the entries are explicitly fixed (8 known filenames); just sort the embedded/known names ascending byte-wise (all lowercase ASCII → same as locale sort). Don't over-engineer locale collation; lowercase ASCII filenames make byte-sort == QDir::Name order here. Result order is the one in §3.3.

---

## 7. Tests (`tests/test_theme.cpp`) and subtle behaviors they rely on

The test is a `QObject` with `QTEST_MAIN`. **Important:** this test target is NOT registered in the current `CMakeLists.txt` (`grep test_theme` → none). It links against `theme.cpp` + `thememanager.cpp` and reads built-in themes from disk (so it depends on `themes/` being copied next to the test binary). The Rust port should re-create these as unit/integration tests, but **note two places where the shipped test is STALE relative to the shipped JSON** — the porter must decide truth = current JSON, and update the assertions:

1. **`builtInThemes()`** (`test_theme.cpp:13-40`):
   - `QVERIFY(all.size() >= 2)`.
   - Finds "Reclass Dark" and "Warm" by name; both must exist and have valid `background/text/...`.
   - `QCOMPARE(warm->background, QColor("#212121"))` — matches warm.json. OK.
   - **`QCOMPARE(warm->selection, QColor("#21213A"))`** — warm.json on disk has `"selection": "#3a2a3a"`. **MISMATCH.** The current JSON would fail this assertion. (Either the test or the JSON drifted; the JSON is the shipped artifact.) Porter: trust JSON; the Rust test should assert `#3a2a3a`.
   - `QCOMPARE(warm->syntaxKeyword, QColor("#AA9565"))` — matches. `QCOMPARE(warm->syntaxType, QColor("#6B959F"))` — matches.
2. **`themeManagerHasBuiltIns()`** (`test_theme.cpp:92-105`):
   - `QVERIFY(all.size() >= 3)`.
   - **`QCOMPARE(all[0].name, QString("Reclass Dark"))`** — but with the current 8-file set sorted by filename, `all[0]` is **"Long Night"** (long_night.json sorts first). **MISMATCH.** This test predates `long_night/mid/modern/phosphor/tw`. Porter: index-0 == filename-sort-first == "Long Night" with the current files.
   - Then verifies "VS2022 Dark" and "Warm" exist by scanning all — those still pass.
3. **`jsonRoundTrip()`** (`:42-60`): `orig = themes()[0]`; `toJson` → `fromJson`; compares `name, background, text, selection, syntaxKeyword, syntaxNumber, syntaxString, syntaxComment, syntaxType, markerPtr, markerError, indHoverSpan`. This relies on: a fully-populated theme round-trips losslessly through `#rrggbb` strings. Holds for any built-in. Robust to which theme is index 0.
4. **`jsonRoundTripWarm()`** (`:62-76`): same round-trip on the "Warm" theme. Robust.
5. **`fromJsonMissingFields()`** (`:78-90`): build sparse `{name:"Sparse", background:"#ff0000"}`; `fromJson`:
   - `name == "Sparse"`, `background == #ff0000`.
   - `QVERIFY(!t.text.isValid())` — text has no fallback → invalid. OK.
   - `QVERIFY(!t.syntaxKeyword.isValid())` — no fallback → invalid. OK.
   - **`QVERIFY(!t.markerError.isValid())`** — **but `fromJson` step 4 sets `markerError = #5a1d1d` when invalid** (`theme.cpp:97`). **MISMATCH:** current code makes `markerError` valid. This assertion would FAIL against current `theme.cpp`. The marker-fallback block (`theme.cpp:91-97`) was added after this test was written. Porter: in the Rust port, `from_json` of a sparse theme yields a VALID `markerError` (= `#5a1d1d`); the test must be updated. This is a load-bearing behavior of the current code — **markers always end valid after `fromJson`.**
6. **`themeManagerSwitch()`** (`:107-121`): `QSignalSpy` on `themeChanged`; `setCurrent(target)` where target = (cur==0?1:0); asserts exactly **1** signal emission, `currentIndex()==target`, `current().name == themes()[target].name`. Then restores. Relies on: `setCurrent` emits exactly once and updates index. (Restore `setCurrent(startIdx)` emits again — fine, spy not re-checked.)
7. **`themeManagerCRUD()`** (`:123-145`):
   - Add: `custom = themes()[0]; name="Test Custom"; bg=#ff0000; addTheme(custom)`. `themes().size()==initial+1`, `themes().last().name=="Test Custom"`. Relies on: addTheme appends to user list, themes() reflects it, no index change.
   - Update: `idx = size-1`; `updated.bg=#00ff00`; `updateTheme(idx, updated)`; `themes()[idx].background == #00ff00`. Relies on user-branch update storing in place.
   - Remove: `removeTheme(idx)`; `themes().size()==initial`. Relies on user removal.
   - **Side effect:** this test calls `addTheme`/`updateTheme`/`removeTheme`, all of which call `saveUserThemes()` which **deletes and rewrites the user themes dir on disk**. The test mutates real user files (no fixture isolation). Porter should isolate the data dir in tests (env override / temp dir).

**Net guidance for the port's tests:** Replicate tests 3,4,6,7 faithfully. For 1,2,5, port the *intent* but fix the constants to match current JSON + current `fromJson` (selection `#3a2a3a`; `all[0]=="Long Night"`; sparse `markerError` is VALID). Document these as known stale-test fixes.

---

## 8. Concurrency / threading

- `ThemeManager` is a singleton accessed on the GUI/main thread. No internal locking. `themeChanged` is a Qt signal delivered synchronously (direct connection within one thread). No `QMutex`, no atomics, no background threads in this subsystem.
- File I/O (`loadBuiltInThemes`, `loadUserThemes`, `saveUserThemes`) is synchronous, on the calling thread.
- **Rust mapping:** keep it single-threaded (UI thread). If a global is needed across threads, `OnceLock<Mutex<ThemeManager>>`; but match the synchronous, main-thread observer-notify model. In GPUI, a `Global` updated on the main thread + `cx.notify()` is the idiomatic fit. No data races to worry about given the original design.

---

## 9. Platform-specific code

- `builtInDir()` differs on macOS (`../Resources/themes` inside the `.app`) vs others (`<exe>/themes`). `#ifdef Q_OS_MACOS`. → `#[cfg(target_os="macos")]` in Rust; both arms must compile on Linux (only the non-macOS arm is active there, but keep the macOS arm gated so it still type-checks).
- `userDir()` uses `QStandardPaths::AppDataLocation` which resolves per-OS (Windows roaming AppData, macOS Application Support, Linux XDG data). → `directories` crate.
- `QSettings` storage backend is per-OS (registry / plist / INI) but the API surface used is a single string key; abstract it.
- No Windows-only or Linux-only code in this subsystem beyond the above. CMake copies `defaults/*.json` to `<exe>/themes/` (post-build) and, on macOS, sets `MACOSX_PACKAGE_LOCATION "Resources/themes"` (`CMakeLists.txt:261-286`). The Rust build must ship the 8 JSON files to the equivalent location (build script copy, or `include_dir!`).

---

## 10. Implementer checklist (Rust port)

1. `Color` type with validity (suggest `Option<Color>` per field) + exact `#rrggbb` parse/format (lowercase, invalid→`#000000` on format).
2. `Theme` struct: `name: String` + 31 `Option<Color>` fields in the documented order/groups.
3. `THEME_FIELDS: &[ThemeFieldMeta{key,label,group, accessor}]` table (31 entries, exact strings/order) driving both serde and editor.
4. `Theme::from_json` replicating §3.2 exactly incl. heat-lerp (truncating int + clamp), focusGlow/indRttiHint/marker fallbacks, and the hover-distinctness `lighter(130)` guard.
5. `Theme::to_json` writing name + all fields as hex.
6. Ship the 8 default JSON files; load sorted by filename.
7. `ThemeManager`: builtin/user/builtinDefaults vectors, currentIdx over concatenated space, the singleton + one `theme_changed` notification, all 11 public methods with the exact branch/emit/persist semantics in §4 (note `updateTheme` selects built-ins but not user themes; `addTheme` no-emit; `removeTheme` index fixup + emit-only-when-current-removed; preview/revert transient broadcast).
8. Persist current theme **name** (not index) under a `"theme"` key; ctor fallback = first name containing "VS2022" (case-insensitive) else first theme.
9. `userDir`/`builtInDir` with the macOS `#[cfg]` split; `mkpath` equivalent.
10. `saveUserThemes`: wipe-then-rewrite user dir; save modified built-ins (diff against pristine) under `name.to_lowercase().replace(' ',"_")+".json"`; save all user themes.
11. GPUI editor mirroring §5 (combo, name edit, file-info label, kThemeFields-driven swatch grid with section headers, color picker, Save→accept, Cancel→revert_preview, preview-on-open + preview-on-edit).
12. Tests: port test_theme.cpp intents, fixing the 3 stale constants (§7), and isolate the data dir.
