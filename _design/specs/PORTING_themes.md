# PORTING SPEC — Theme System (`themes`)

Function-level porting spec to drive the faithful Rust implementation of the Reclass
theme subsystem. Source of truth: the C++/Qt6 source under `/home/loke/reclass-cpp/src/themes/`
plus the behavioral map `_design/understand/themes.md`. This spec is the contract; where the map
and the C++ disagree, the **C++ source + shipped JSON win** (see §9 stale-test fixes, confirmed
by the oracle `_oracle/RESULTS.md` `test_theme` failures).

Read alongside: `_design/ARCHITECTURE.md` (module = `src/theme/`), `_design/crate_selection.md`
(serde/serde_json/directories/thiserror), `_design/gpui_cookbook.md` §6 (Global-theme pattern),
`_design/gpui_component_cookbook.md` (Dialog / Select / ColorPicker / Scrollbar for the editor view).

---

## 0. Target crate / modules

One package `reclass`, module `src/theme/` (per ARCHITECTURE §2/§4, `src/themes/* → theme`).
Split:

```
src/theme/
├─ mod.rs            # re-exports; pub use color::*, model::*, manager::*; (ui::editor behind `ui`)
├─ color.rs         # Color (RGB8) + parse/format + lighter() — replaces QColor (this subsystem's subset)
├─ model.rs         # Theme struct, ThemeFieldMeta table, to_json/from_json   ← theme.h / theme.cpp
├─ manager.rs       # ThemeManager (builtin/user/defaults, current, CRUD, preview)  ← thememanager.*
├─ defaults/        # the 8 shipped JSON files (verbatim copies of src/themes/defaults/*.json)
│                   # embedded via include_str! AND shipped to <exe>/themes/ by build.rs (see §6)
└─ editor.rs        # ThemeEditor GPUI view  ← themeeditor.*    [#[cfg(feature = "ui")] only]
```

- `color`, `model`, `manager` are **`always`-feature** (no gpui) so logic tests run with
  `--no-default-features` (ARCHITECTURE §8). `editor.rs` is gated `#[cfg(feature = "ui")]`.
- Crates: `serde` + `serde_json` (model), `directories` 6.0 (user dir), `thiserror` (errors),
  `tracing` (warn on bad theme files). UI editor: `gpui` + `gpui-component`. **No color crate** —
  `Color` is hand-rolled for bit-exact `QColor` parity (map §6 / §2.1 recommendation).
- The persisted `"theme"` selection key does **NOT** get its own store. The C++ uses the
  app-wide `QSettings("Reclass","Reclass")` (verified: 30+ call sites in `main.cpp` share it).
  The Rust port routes the single `"theme"` string key through the shared app `Settings`
  abstraction (the `core`/app-settings subsystem). This spec defines a `SettingsStore` trait
  the manager depends on so theme tests can inject an in-memory/temp store (see §3.6, §7).

---

## 1. The `Color` type (`color.rs`) — replaces `QColor` (subset used here)

`QColor` is used in this subsystem ONLY as: 8-bit RGB (alpha never set/read), `.name()`
→ `#rrggbb` lowercase, `QColor("#rrggbb")` parse, `.isValid()`, `.red()/.green()/.blue()`,
and `.lighter(130)`. The validity-as-sentinel semantics are load-bearing (`from_json` branches
on `.isValid()`), so we model "invalid" as `Option<Color>` per field — NOT an `is_valid` flag
inside `Color`. `Color` itself is always a concrete valid RGB triple.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Color { pub r: u8, pub g: u8, pub b: u8 }
```

### 1.1 Parse — `Color::parse(s: &str) -> Option<Color>`  (replaces `QColor(QString)`)

Maps `QColor(const QString&)`. Inputs in this subsystem are *only* 6-digit `#rrggbb` (all
shipped JSON + editor + tests). Behavior:

- Trim nothing (Qt does not trim). Accept exactly `#` + 6 hex digits (case-insensitive).
- Parse each pair as `u8` from base 16. Any other length/format → `None` (invalid QColor).
- Return `Some(Color{r,g,b})`.

Pseudocode:
```
fn parse(s):
    if s.len() == 7 && s[0] == '#':
        r = hex2(s[1..3])?; g = hex2(s[3..5])?; b = hex2(s[5..7])?
        return Some(Color{r,g,b})
    return None      // matches QColor → invalid (no panic, no throw)
```
**Faithful-enough note:** Qt also accepts `#rgb`, `#rrggbbaa`, `#aarrggbb`, and SVG names; the
behavioral map (§6) confirms only 6-digit hex appears in any real input here, so we restrict to
that. A non-6-digit string therefore yields `None` (invalid) which then triggers `from_json`
fallbacks — same observable outcome as Qt for the inputs that occur. **Do NOT widen** without a
new test vector, to avoid divergence.

### 1.2 Format — `Color::to_hex(self) -> String`  (replaces `QColor::name()`)

`format!("#{:02x}{:02x}{:02x}", r, g, b)` — lowercase, 6 digits, no alpha (Qt `HexRgb` default).

For an absent field (`Option<Color> == None`), the serializer must emit `"#000000"` to match
`QColor().name()` (invalid QColor formats as black). Implement as a free helper:
```rust
fn hex_or_black(c: Option<Color>) -> String {
    c.map(Color::to_hex).unwrap_or_else(|| "#000000".into())
}
```
This is used by `to_json` (§2.3) and by the `to_json`-diff in save/path logic (§3.5/§3.7).

### 1.3 `Color::lighter_130(self) -> Color`  (replaces `QColor::lighter(130)`)

Used only by the hover-distinctness guard (`from_json` step, §2.4.5). Replicate Qt's
`QColor::lighter(factor)` for `factor = 130` (`>100`, so the brighten path):

Qt algorithm: convert RGB→HSV, `v = min(255, (v * factor) / 100)`, convert HSV→RGB, keep H,S.
Integer arithmetic, truncating. For factor 130: `v' = min(255, (v*130)/100)`. Pure black
(`v==0`) stays black (Qt's `lighter` does NOT special-case black to brighten it — confirmed in
map §6).

Pseudocode (must match Qt's `qrgb`→hsv→rgb integer conversion):
```
fn lighter_130(c):
    (h, s, v) = rgb_to_hsv_qt(c)         // Qt's getHsv: h in 0..359 or -1 (achromatic), s,v in 0..255
    v = min(255, (v as i32 * 130) / 100) as u8
    return hsv_to_rgb_qt(h, s, v)
```
`rgb_to_hsv_qt` / `hsv_to_rgb_qt` must mirror Qt's `QColor::toHsv()`/`fromHsv()` integer math
(see Qt `qcolor.cpp`). **Parity caveat (map §6):** this guard fires only when the Manhattan
distance between `hover` and `background` is `< 20`. No shipped built-in trips it (all are
≥ 20 apart), so it is effectively dead for built-ins and exercised only by hand-crafted user
themes / unit tests. Implement it correctly but a small integer-rounding deviation here is
extremely low-risk; add a targeted unit test (§7 test `lighter_130_matches_qt`) using a couple
of known Qt outputs to lock it.

> Implementation tip: lift the exact integer HSV round-trip from Qt's `qcolor.cpp`
> (`QColor::toHsv`, `QColor::convertTo`) — ~25 lines. Do not use the `palette`/`csscolorparser`
> crates here (they round differently → divergence).

---

## 2. `Theme` model (`model.rs`)  ← `theme.h` / `theme.cpp`

### 2.1 Struct

`name: String` + **31** color fields, each `Option<Color>` (the `Option` is the `isValid()`
sentinel). Field order/groups match `kThemeFields` exactly (the order is load-bearing for the
editor row layout and section grouping). Recount per map §2.2: Chrome 6, Text 4, Interactive 3,
Syntax 6, Indicators 8, Markers 3, Presentation 1 = **31**.

```rust
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    pub name: String,
    // Chrome
    pub background: Option<Color>,
    pub background_alt: Option<Color>,
    pub surface: Option<Color>,
    pub border: Option<Color>,
    pub border_focused: Option<Color>,
    pub button: Option<Color>,
    // Text
    pub text: Option<Color>,
    pub text_dim: Option<Color>,
    pub text_muted: Option<Color>,
    pub text_faint: Option<Color>,
    // Interactive
    pub hover: Option<Color>,
    pub selected: Option<Color>,
    pub selection: Option<Color>,
    // Syntax
    pub syntax_keyword: Option<Color>,
    pub syntax_number: Option<Color>,
    pub syntax_string: Option<Color>,
    pub syntax_comment: Option<Color>,
    pub syntax_preproc: Option<Color>,
    pub syntax_type: Option<Color>,
    // Indicators
    pub ind_hover_span: Option<Color>,
    pub ind_cmd_pill: Option<Color>,
    pub ind_data_changed: Option<Color>,
    pub ind_heat_cold: Option<Color>,
    pub ind_heat_warm: Option<Color>,
    pub ind_heat_hot: Option<Color>,
    pub ind_hint_green: Option<Color>,
    pub ind_rtti_hint: Option<Color>,
    // Markers
    pub marker_ptr: Option<Color>,
    pub marker_cycle: Option<Color>,
    pub marker_error: Option<Color>,
    // Presentation
    pub focus_glow: Option<Color>,
}
```
`Default` (all `None`, empty name) == the C++ default-constructed `Theme` (the `static const
Theme empty` fallback in `current()`, §3.3).

> **serde decision:** do NOT `#[derive(Serialize/Deserialize)]` directly. The JSON shape is a
> flat `{ "name": str, "<key>": "#rrggbb", ... }` where (a) keys are the camelCase JSON keys
> (`backgroundAlt`, `syntaxKeyword`, …) not Rust snake_case, (b) values are hex strings not RGB,
> and (c) `from_json` applies the §2.4 derivation pipeline that plain derive cannot express.
> Implement `to_json`/`from_json` manually over `serde_json::Map<String, Value>`, driven by
> `THEME_FIELDS` (§2.2). This keeps one source of truth identical to the C++ `kThemeFields`-loop.

### 2.2 `ThemeFieldMeta` table  ← `kThemeFields[]` (`theme.cpp:9-42`)

The C++ uses `QColor Theme::*ptr` (pointer-to-member) to iterate fields generically. Rust has
no pointer-to-member; replace with an enum `FieldId` + match-based getter/setter, plus a static
table carrying `(key, label, group, FieldId)` in the exact source order.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldId {
    Background, BackgroundAlt, Surface, Border, BorderFocused, Button,
    Text, TextDim, TextMuted, TextFaint,
    Hover, Selected, Selection,
    SyntaxKeyword, SyntaxNumber, SyntaxString, SyntaxComment, SyntaxPreproc, SyntaxType,
    IndHoverSpan, IndCmdPill, IndDataChanged, IndHeatCold, IndHeatWarm, IndHeatHot,
    IndHintGreen, IndRttiHint,
    MarkerPtr, MarkerCycle, MarkerError,
    FocusGlow,
}

pub struct ThemeFieldMeta { pub key: &'static str, pub label: &'static str, pub group: &'static str, pub id: FieldId }

pub const THEME_FIELDS: &[ThemeFieldMeta] = &[
    ThemeFieldMeta{key:"background",     label:"Background",     group:"Chrome",       id:FieldId::Background},
    ThemeFieldMeta{key:"backgroundAlt",  label:"Background Alt",  group:"Chrome",       id:FieldId::BackgroundAlt},
    ThemeFieldMeta{key:"surface",        label:"Surface",         group:"Chrome",       id:FieldId::Surface},
    ThemeFieldMeta{key:"border",         label:"Border",          group:"Chrome",       id:FieldId::Border},
    ThemeFieldMeta{key:"borderFocused",  label:"Border Focused",  group:"Chrome",       id:FieldId::BorderFocused},
    ThemeFieldMeta{key:"button",         label:"Button",          group:"Chrome",       id:FieldId::Button},
    ThemeFieldMeta{key:"text",           label:"Text",            group:"Text",         id:FieldId::Text},
    ThemeFieldMeta{key:"textDim",        label:"Text Dim",        group:"Text",         id:FieldId::TextDim},
    ThemeFieldMeta{key:"textMuted",      label:"Text Muted",      group:"Text",         id:FieldId::TextMuted},
    ThemeFieldMeta{key:"textFaint",      label:"Text Faint",      group:"Text",         id:FieldId::TextFaint},
    ThemeFieldMeta{key:"hover",          label:"Hover",           group:"Interactive",  id:FieldId::Hover},
    ThemeFieldMeta{key:"selected",       label:"Selected",        group:"Interactive",  id:FieldId::Selected},
    ThemeFieldMeta{key:"selection",      label:"Selection",       group:"Interactive",  id:FieldId::Selection},
    ThemeFieldMeta{key:"syntaxKeyword",  label:"Keyword",         group:"Syntax",       id:FieldId::SyntaxKeyword},
    ThemeFieldMeta{key:"syntaxNumber",   label:"Number",          group:"Syntax",       id:FieldId::SyntaxNumber},
    ThemeFieldMeta{key:"syntaxString",   label:"String",          group:"Syntax",       id:FieldId::SyntaxString},
    ThemeFieldMeta{key:"syntaxComment",  label:"Comment",         group:"Syntax",       id:FieldId::SyntaxComment},
    ThemeFieldMeta{key:"syntaxPreproc",  label:"Preprocessor",    group:"Syntax",       id:FieldId::SyntaxPreproc},
    ThemeFieldMeta{key:"syntaxType",     label:"Type",            group:"Syntax",       id:FieldId::SyntaxType},
    ThemeFieldMeta{key:"indHoverSpan",   label:"Hover Span",      group:"Indicators",   id:FieldId::IndHoverSpan},
    ThemeFieldMeta{key:"indCmdPill",     label:"Cmd Pill",        group:"Indicators",   id:FieldId::IndCmdPill},
    ThemeFieldMeta{key:"indDataChanged", label:"Data Changed",    group:"Indicators",   id:FieldId::IndDataChanged},
    ThemeFieldMeta{key:"indHeatCold",    label:"Heat Cold",       group:"Indicators",   id:FieldId::IndHeatCold},
    ThemeFieldMeta{key:"indHeatWarm",    label:"Heat Warm",       group:"Indicators",   id:FieldId::IndHeatWarm},
    ThemeFieldMeta{key:"indHeatHot",     label:"Heat Hot",        group:"Indicators",   id:FieldId::IndHeatHot},
    ThemeFieldMeta{key:"indHintGreen",   label:"Hint Green",      group:"Indicators",   id:FieldId::IndHintGreen},
    ThemeFieldMeta{key:"indRttiHint",    label:"RTTI Hint",       group:"Indicators",   id:FieldId::IndRttiHint},
    ThemeFieldMeta{key:"markerPtr",      label:"Pointer",         group:"Markers",      id:FieldId::MarkerPtr},
    ThemeFieldMeta{key:"markerCycle",    label:"Cycle",           group:"Markers",      id:FieldId::MarkerCycle},
    ThemeFieldMeta{key:"markerError",    label:"Error",           group:"Markers",      id:FieldId::MarkerError},
    ThemeFieldMeta{key:"focusGlow",      label:"Focus Glow",      group:"Presentation", id:FieldId::FocusGlow},
];
```

Field accessors on `Theme` (replace `this->*ptr`):
```rust
impl Theme {
    pub fn get(&self, id: FieldId) -> Option<Color> { /* match id => self.<field> */ }
    pub fn set(&mut self, id: FieldId, c: Option<Color>) { /* match id => self.<field> = c */ }
}
```
Generate the two big `match`es by hand (31 arms) — mechanical, no macro needed. A
`#[test] field_table_count_is_31` asserts `THEME_FIELDS.len() == 31` (mirrors
`kThemeFieldCount`).

### 2.3 `to_json` — `Theme::to_json(&self) -> serde_json::Value`  (`theme.cpp:44-50`)

```
o = Map::new()
o["name"] = Value::String(self.name.clone())
for f in THEME_FIELDS:
    o[f.key] = Value::String(hex_or_black(self.get(f.id)))   // None → "#000000"
return Value::Object(o)
```
- Writes `"name"` then every field's hex. `None` → `"#000000"` (matches `QColor().name()`,
  map §3.1 edge case — a missing field does NOT round-trip as missing; it becomes black).
- **Key order is NOT load-bearing** (map §3.1: Qt re-sorts QJsonObject keys alphabetically on
  write; load is order-independent). `serde_json::Map` (default `BTreeMap`-backed via the
  `preserve_order`-off build → actually serde_json default uses `BTreeMap` only without the
  `preserve_order` feature) emits sorted keys, which incidentally matches Qt's alphabetical
  output. **Decision:** do not enable serde_json `preserve_order` for theme writes; deterministic
  sorted output is fine and closest to Qt. Pretty-print with `serde_json::to_string_pretty`
  (2-space indent vs Qt's 4-space; byte-identical files are explicitly NOT required — map §3.1).

### 2.4 `from_json` — `Theme::from_json(o: &serde_json::Value) -> Theme`  (`theme.cpp:52-108`)

THE important algorithm. Steps in exact source order:

```
t = Theme::default()
t.name = o["name"].as_str().unwrap_or("Untitled").to_string()
        // Qt: o["name"].toString("Untitled") — absent OR non-string → "Untitled"
for f in THEME_FIELDS:
    if o has key f.key:
        s = o[f.key].as_str().unwrap_or("")      // non-string → "" → parse None
        t.set(f.id, Color::parse(s))             // unparseable → None (stays invalid)
    // keys NOT present stay None
```
Then the **derivation pipeline** — each step fires only when the target field is `None`:

1. **Heat amber gradient** (`theme.cpp:69-80`):
   - `lerp_rgb(a, b, f)` (component-wise, **truncating** `int(...)`, then clamp 0..=255):
     ```
     fn lerp_rgb(a: Color, b: Color, f: f64) -> Color {
         let ch = |ac: u8, bc: u8| {
             let v = ac as i32 + ((bc as i32 - ac as i32) as f64 * f) as i32; // (as i32) truncates toward 0
             v.clamp(0, 255) as u8
         };
         Color{ r: ch(a.r,b.r), g: ch(a.g,b.g), b: ch(a.b,b.b) }
     }
     ```
     Match Qt exactly: `a.c + int((b.c - a.c) * f)` then `qBound(0, …, 255)`. The cast
     `(… as f64 * f) as i32` truncates toward zero (Rust `as i32` on f64 == C++ `int(double)`).
   - `dim = t.text_dim.unwrap_or(Color{133,133,133})`
   - `if t.ind_heat_cold.is_none() { t.ind_heat_cold = Some(lerp_rgb(dim, Color{220,180,120}, 0.35)) }`
   - `if t.ind_heat_warm.is_none() { t.ind_heat_warm = Some(Color{225,170,90}) }`
   - `if t.ind_heat_hot.is_none()  { t.ind_heat_hot  = Some(Color{232,165,92}) }`
   - **All 8 shipped themes omit heat fields**, so these always fire for built-ins;
     `ind_heat_cold` thus depends on each theme's `text_dim`.
2. **focusGlow** (`theme.cpp:82-83`): `if t.focus_glow.is_none() { t.focus_glow =
   Some(t.border_focused.unwrap_or(Color::parse("#4fc3f7").unwrap())) }`. Only `phosphor.json`
   ships `focusGlow`; others derive from `border_focused`.
3. **indRttiHint** (`theme.cpp:88-89`): `if none → "#d19a66"`. All shipped themes ship it (so
   only user themes hit this).
4. **Marker fallbacks** (`theme.cpp:95-97`):
   - `marker_ptr` none → `"#f44747"`
   - `marker_cycle` none → `"#e8a35c"`
   - `marker_error` none → `"#5a1d1d"`
   **Load-bearing:** after `from_json`, all three markers are ALWAYS `Some` (valid). This is the
   behavior the oracle confirms and the stale C++ test gets wrong (§9 / RESULTS.md).
5. **Hover-distinctness guard** (`theme.cpp:99-106`): only if BOTH `hover` and `background` are
   `Some`:
   ```
   if let (Some(h), Some(bg)) = (t.hover, t.background) {
       let dist = (h.r as i32 - bg.r as i32).abs()
                + (h.g as i32 - bg.g as i32).abs()
                + (h.b as i32 - bg.b as i32).abs();
       if dist < 20 { t.hover = Some(bg.lighter_130()); }
   }
   ```
return t
```
**No fallback** for: `name`, all Chrome, all Text, all Interactive, all Syntax,
`ind_hover_span`, `ind_cmd_pill`, `ind_data_changed`, `ind_hint_green` — these stay `None` if
the JSON omits them (and `to_json` would later emit `#000000`).

Edge cases (map §3.2): unparseable hex → `None` → fallback if one exists; `name` absent or
non-string → `"Untitled"`; lerp truncation + clamp must match (no overflow for in-range dims).

---

## 3. `ThemeManager` (`manager.rs`)  ← `thememanager.*`

### 3.0 State, construction, singleton

C++ is a `QObject` Meyers singleton with a `themeChanged(const Theme&)` signal. Rust port: a
plain struct owning the state + an observer callback list. It is **NOT** a process-global
`static` — instead it is owned by the app (the GPUI app holds a `ThemeManager` in a Global, see
§3.10) so tests can construct isolated instances. (The C++ singleton is only ever touched on the
UI thread; map §8 confirms single-threaded, no locking.)

```rust
pub struct ThemeManager {
    builtin: Vec<Theme>,          // m_builtIn  (possibly overridden in-place)
    builtin_defaults: Vec<Theme>, // m_builtInDefaults (pristine, for diffing)
    user: Vec<Theme>,             // m_user
    current_idx: usize,           // m_currentIdx (index into builtin ++ user)
    previewing: bool,             // m_previewing
    saved_theme: Theme,           // m_savedTheme
    settings: Box<dyn SettingsStore>,  // the "theme" string key store (§3.6)
    builtin_dir: PathBuf,         // resolved once (cfg-split, §3.8)
    user_dir: PathBuf,            // resolved once (§3.9)
    observers: Vec<Box<dyn FnMut(&Theme)>>,  // themeChanged subscribers (§3.11)
}
```
Note: `current_idx` is logically a signed index in C++ (compared `< 0`), but it is only ever set
to in-bounds values or 0. Keep it `usize` and bounds-check via `>= len`. (No code path assigns
negative.)

**Constructor** `ThemeManager::new(settings, builtin_dir, user_dir) -> Self`
(`thememanager.cpp:16-31`):
```
load_builtin_themes()        // §3.7
load_user_themes()           // §3.8
// fallback selection name:
fallback = builtin.iter().find(|t| t.name.to_lowercase().contains("vs2022")).map(|t| t.name)
           .or_else(|| builtin.first().map(|t| t.name.clone()))
           .unwrap_or_default()                       // empty if no builtins
saved = settings.get("theme").unwrap_or(fallback)     // QSettings.value("theme", fallback)
current_idx = 0
for (i, t) in themes().enumerate():
    if t.name == saved { current_idx = i; break }     // else stays 0
```
`"VS2022"` match is **case-insensitive** (Qt `contains(.., Qt::CaseInsensitive)`); use
`to_lowercase().contains("vs2022")` (map §6).

### 3.1 `themes(&self) -> Vec<Theme>`  (`thememanager.cpp:60-64`)
Return `builtin` concatenated with `user` (a clone). This defines the index space used
everywhere. (Optionally also expose `builtin_count()` + slices to avoid the clone in hot paths,
but the canonical API returns the concatenated `Vec<Theme>` to mirror C++.)

### 3.2 `current_index(&self) -> usize`  → `current_idx`.

### 3.3 `current(&self) -> &Theme`  (`thememanager.cpp:66-76`)
```
if current_idx < builtin.len(): return &builtin[current_idx]
user_idx = current_idx - builtin.len()
if user_idx < user.len(): return &user[user_idx]
if !builtin.is_empty(): return &builtin[0]
return &EMPTY        // static all-default Theme; only if zero themes loaded
```
`EMPTY`: a `static` or `OnceLock<Theme>` default theme (== `Theme::default()`).

### 3.4 `set_current(&mut self, index: usize)`  (`thememanager.cpp:78-85`)
```
all = themes()
if index >= all.len(): return            // no-op, no signal (C++ also guards <0; usize covers it)
current_idx = index
settings.set("theme", &all[index].name)  // persist NAME
emit_theme_changed(self.current().clone())
```
Always emits when in-range, even if index unchanged (map §4.4).

### 3.5 `add_theme(&mut self, theme: Theme)`  (`thememanager.cpp:87-90`)
`user.push(theme); save_user_themes();` — does NOT change `current_idx`, does NOT emit.

### 3.6 `update_theme(&mut self, index: usize, theme: Theme)`  (`thememanager.cpp:92-107`)
```
previewing = false                       // commit any active preview (revert becomes no-op)
if index < builtin.len():
    builtin[index] = theme               // overwrite built-in
    current_idx = index                  // ...AND select it
else:
    ui = index - builtin.len()
    if ui < user.len(): user[ui] = theme // user branch does NOT change current_idx
save_user_themes()
settings.set("theme", &self.current().name)
emit_theme_changed(self.current().clone())
```
Edge (map §4.6): out-of-range user index still saves+persists+emits but stores nothing
(silent drop). With `usize`, the C++ `ui >= 0` is automatic; only the `ui < user.len()` check
matters.

### 3.7 `remove_theme(&mut self, index: usize)`  (`thememanager.cpp:109-121`)
```
if index < builtin.len(): return         // built-ins can't be removed (no-op)
ui = index - builtin.len()
if ui >= user.len(): return              // out-of-range user → no-op
user.remove(ui)
if current_idx == index:
    current_idx = 0
    emit_theme_changed(self.current().clone())
else if current_idx > index:
    current_idx -= 1                     // no emit
// else current_idx < index: unchanged, no emit
save_user_themes()
```
Note (map §4.7): does NOT update persisted `"theme"` here (benign; ctor re-resolves next launch).

### 3.8 `load_builtin_themes(&mut self)`  (`thememanager.cpp:44-56`)
```
builtin.clear()
if !builtin_dir.exists(): return
files = read_dir(builtin_dir), keep *.json (case per filesystem), sort ascending by file name
for name in files:
    data = read_file(builtin_dir/name) or continue   // skip unreadable
    v = serde_json::from_slice(&data) or continue
    if v.is_object(): builtin.push(Theme::from_json(&v))
builtin_defaults = builtin.clone()
```
**Sort:** `QDir::entryList({"*.json"}, Files, QDir::Name)` → name-ascending. The 8 filenames are
all lowercase ASCII (`long_night, mid, modern, phosphor, reclass_dark, tw, vs, warm`), so a plain
`sort()` byte-wise == QDir::Name order (map §6). Resulting display-name order:
**Long Night, Mid, Modern, Phosphor, Reclass Dark, Light, VS2022 Dark, Warm**. So `themes()[0]`
== "Long Night" (NOT "Reclass Dark" — the stale test, §9).

> Embedding vs disk (map §3.3, §6): see §6 of this spec — the builtin source is read from
> `<exe>/themes/*.json` to preserve `theme_file_path` semantics; the same 8 files are also
> `include_str!`-embedded so the app can self-heal a missing `themes/` dir. For TESTS, point
> `builtin_dir` at `_oracle/fixtures/themes/` (identical bytes to `src/themes/defaults/`).

### 3.9 `load_user_themes(&mut self)`  (`thememanager.cpp:132-154`)
```
user.clear()
ensure user_dir exists (mkpath — already created by user_dir resolution, §3.9.1)
files = read_dir(user_dir), keep *.json, name-sorted (Qt default sort)
for name in files:
    v = parse(read(user_dir/name)) or continue; if !v.is_object() continue
    t = Theme::from_json(&v)
    // override logic: same display NAME as a built-in → replace in place
    if let Some(i) = builtin.iter().position(|b| b.name == t.name):
        builtin[i] = t           // override; builtin_defaults untouched → still "modified"
    else:
        user.push(t)
```
Effect (map §4.9): a user file whose theme `name` matches a built-in display name overrides that
built-in's colors in the built-in slot; the diff vs `builtin_defaults` then reports it modified.

### 3.10 `save_user_themes(&self)`  (`thememanager.cpp:156-179`)
```
// 1. wipe: delete every *.json in user_dir
for name in read_dir(user_dir) keep *.json: remove_file(user_dir/name)
// 2. save MODIFIED built-ins (diff against pristine)
for i in 0..min(builtin.len(), builtin_defaults.len()):
    if builtin[i].to_json() != builtin_defaults[i].to_json():
        fname = builtin[i].name.to_lowercase().replace(' ', "_") + ".json"
        write(user_dir/fname, to_string_pretty(builtin[i].to_json()))
// 3. save ALL user themes
for u in &user:
    fname = u.name.to_lowercase().replace(' ', "_") + ".json"
    write(user_dir/fname, to_string_pretty(u.to_json()))
```
- Diff: `Theme::to_json()` deep equality (`Value == Value`; mirrors `QJsonObject !=`). Use
  `to_json()` on both sides and compare the `serde_json::Value`s. (`Value: PartialEq` is a deep
  structural compare — exact match for Qt's QJsonObject inequality.)
- **Filename derivation:** `name.to_lowercase().replace(' ', "_") + ".json"`. Qt `toLower()` is
  Unicode-lowercase; `str::to_lowercase()` matches for the ASCII names in use. `replace(' ', "_")`
  replaces only ASCII space U+0020 (not tabs). E.g. "VS2022 Dark" → `vs2022_dark.json`,
  "Light" → `light.json` (differs from the source filename `tw.json` — re-matched on load by
  display name via §3.9 override path; round-trips correctly — map §4.10).
- Write failures: silently skipped in C++ (`if (f.open(...))`). In Rust, ignore the `Err` from
  `fs::write` per file but log via `tracing::warn!` (does not change observable behavior).
- Edge: two names collapsing to the same filename clobber (last write wins) — not deduplicated
  (map §4.10). Match this (do not dedupe).

### 3.11 `theme_file_path(&self, index: usize) -> Option<PathBuf>`  (`thememanager.cpp:181-197`)
```
if index < builtin.len():
    if index < builtin_defaults.len() && builtin[index].to_json() != builtin_defaults[index].to_json():
        return Some(user_dir / fname(builtin[index].name))     // user override copy
    return Some(builtin_dir / fname(builtin[index].name))       // source file (cosmetic; may not exist)
ui = index - builtin.len()
if ui >= user.len(): return None                                // empty {} in C++
return Some(user_dir / fname(user[ui].name))
```
`fname(name) = name.to_lowercase().replace(' ', "_") + ".json"`. Returns `None` (≙ empty
`QString`) only for an out-of-range user index. Used solely by the editor's info label (map
§4.13). The editor maps `None` → "Built-in theme (edits save as user copy)" and `Some(p)` →
`"File: {p}"`.

### 3.12 `preview_theme(&mut self, theme: Theme)`  (`thememanager.cpp:199-205`)
```
if !previewing { saved_theme = self.current().clone(); previewing = true; }
emit_theme_changed(theme.clone())     // broadcast the TRANSIENT theme; does NOT change current_idx/stored
```
Re-entrant: subsequent calls just re-emit; `saved_theme` captured only on the first (map §4.14).

### 3.13 `revert_preview(&mut self)`  (`thememanager.cpp:207-212`)
```
if previewing { previewing = false; emit_theme_changed(self.saved_theme.clone()); }
// not previewing → no-op (e.g. after update_theme committed)
```

### 3.14 The `themeChanged` signal → observer callbacks
C++ `signals: void themeChanged(const rcx::Theme&)`. There is exactly ONE signal; ~7 consumers
(`themed_dialog`, `dialog_button`, `controller`, `editor`, `main`, …) re-apply the theme. Two
faithful Rust mappings, choose per build:

- **Headless/logic build** (`--no-default-features`): a callback registry on the manager:
  ```rust
  pub fn subscribe(&mut self, cb: Box<dyn FnMut(&Theme)>) -> SubId
  pub fn unsubscribe(&mut self, id: SubId)
  fn emit_theme_changed(&mut self, t: Theme) { for cb in &mut self.observers { cb(&t); } }
  ```
  Delivered synchronously, on the calling thread (matches Qt direct connection, map §8). Tests
  use a `Rc<Cell<usize>>` counter callback to count emissions (= `QSignalSpy`).
- **GPUI/`ui` build** (§3.15): emission updates a `GlobalActiveTheme` Global + `cx.refresh()`.

**Critical (map §4.16):** consumers must use the **emitted** theme value, not call `current()` —
because `preview_theme` emits a theme that is NOT the current. In GPUI this means
`emit_theme_changed` writes the emitted theme into the active-theme Global (see §3.15), and
views read that Global, so preview correctly paints the transient theme.

### 3.15 GPUI integration (the `themeChanged` → re-render bridge)  `[#[cfg(feature="ui")]]`
Per `gpui_cookbook.md` §6 (Global + `ActiveTheme`) and `gpui_component_cookbook.md` (gpui's
`Theme`/`ThemeRegistry`). The Reclass `Theme` is a richer 31-color bundle than gpui-component's
`ThemeColor`, so we keep our own:

```rust
pub struct GlobalActiveTheme(pub std::sync::Arc<Theme>);  // the currently-painted theme (fully-derived)
impl gpui::Global for GlobalActiveTheme {}

pub trait ActiveReclassTheme { fn rcx_theme(&self) -> &std::sync::Arc<Theme>; }
impl ActiveReclassTheme for gpui::App {
    fn rcx_theme(&self) -> &std::sync::Arc<Theme> { &self.global::<GlobalActiveTheme>().0 }
}
```
`ThemeManager` lives in its own Global; its `emit_theme_changed(t)` (in the ui build) does:
`cx.update_global::<GlobalActiveTheme,_>(|g,_| g.0 = Arc::new(t)); cx.refresh();` so every view
re-reads `cx.rcx_theme()` next frame (map §1 "apply theme" ≙ store active Theme + re-render).
Optionally also push a derived `gpui_component::Theme` so standard chrome (dialogs, Select,
Scrollbar) tints consistently — map our colors onto gpui-component's `ThemeColor`
(`theme_apply.rs`, ARCHITECTURE §2). This mapping table (Reclass→gpui-component field) is owned
by `ui::theme_apply` and out of scope for the model/manager tests; document the obvious
correspondences (`background→background`, `text→foreground`, `border→border`,
`borderFocused→ring/accent`, `hover→...`, `selection→selection`, `selected→list active`,
**keep `selected` ≠ `selection`** per component-cookbook line 114).

### 3.16 `SettingsStore` trait (the `"theme"` key)
```rust
pub trait SettingsStore {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
}
```
- App build: backed by the shared app settings (the `QSettings("Reclass","Reclass")` replacement
  in the app/`main` subsystem — same store used by `compactColumns`, `treeLines`, etc.). The
  theme module only ever reads/writes the single `"theme"` string key.
- Test build: `struct MemSettings(HashMap<String,String>)` for isolation (no real registry/INI).

---

## 4. `builtin_dir` / `user_dir` — platform paths

### 4.1 `builtin_dir`  (`thememanager.cpp:35-42`) — **platform-specific**
```rust
fn builtin_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let dir = exe.as_deref().and_then(|p| p.parent()).map(Path::to_path_buf).unwrap_or_default();
    #[cfg(target_os = "macos")]
    { dir.join("../Resources/themes") }     // inside .app bundle (Contents/MacOS → ../Resources)
    #[cfg(not(target_os = "macos"))]
    { dir.join("themes") }                   // next to the exe
}
```
Both arms must compile on Linux (only the non-macOS arm is active there; keep the macOS arm
gated so it still type-checks — ARCHITECTURE constraint). The app may inject this path so the
running app can point it at the embedded/extracted dir; tests inject the oracle fixtures dir.

### 4.2 `user_dir`  (`thememanager.cpp:125-130`)
```rust
fn user_dir() -> PathBuf {
    let base = directories::ProjectDirs::from("", "Reclass", "Reclass")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("Reclass"));
    let dir = base.join("themes");
    let _ = std::fs::create_dir_all(&dir);   // QDir().mkpath
    dir
}
```
`QStandardPaths::AppDataLocation` with org/app "Reclass"/"Reclass" → `directories` with
qualifier `""`, org `"Reclass"`, app `"Reclass"` (map §4.12). Per-OS: Win Roaming AppData,
macOS Application Support, Linux XDG data. Match whatever the app/settings subsystem uses for
its canonical org/app names (keep consistent). Tests inject a `tempfile::TempDir` instead of the
real data dir.

---

## 5. `ThemeEditor` GPUI view (`editor.rs`)  ← `themeeditor.*`  `[#[cfg(feature="ui")]]`

Qt-Widgets modal → GPUI view rendered through gpui-component's modal `Dialog`
(`gpui_component_cookbook.md` line 80, 105). Behavior to replicate (map §5):

State (`struct ThemeEditor`): `theme: Theme` (working copy), `theme_index: usize`,
`name_input: Entity<InputState>` (gpui-component `TextInput`, cookbook line 43),
`combo: SelectState<ThemeListDelegate>` (gpui-component `Select`, cookbook line 46),
`file_info: String`, plus a scroll handle for the swatch list. `result()` → `theme.clone()`;
`selected_index()` → `theme_index`.

### 5.1 Construction (`themeeditor.cpp:31-156`)
- `theme = themes()[index]` if `index` valid else `manager.current().clone()`.
- Title "Theme Editor", min 420×480, initial 440×640 (gpui-component Dialog sizing).
- **Theme selector row:** label "Theme:" + `Select` listing all theme names; current = index;
  on-change → `load_theme(idx)`.
- **Name row:** label "Name:" + `TextInput(name_input)` seeded with `theme.name`; on text change
  → `theme.name = t` (live).
- **File-info label:** text from `theme_file_path(index)` → `"File: {p}"` or, if `None`,
  "Built-in theme (edits save as user copy)". Styled with `text_dim`.
- **Swatch list** (scroll container; `gpui-component` `Scrollbar` / a `uniform_list` or simple
  scrollable `v_flex`), generated by iterating `THEME_FIELDS`:
  - On `group` change (compare `f.group` to previous, like the C++ `strcmp`), emit a section
    header (bold 11px, color `text_muted`, bottom border `border` — both pulled from
    `current()` so dividers re-tint on preview, per `makeSectionLabel`).
  - Each row: fixed-width(120) label = `f.label`; a 32×18 swatch button (bg = field color, 1px
    border = `current().border`, radius 2; pointing-hand cursor) → on click `pick_color(f.id)`;
    a fixed-width(60) hex label (color `text_muted`) = field color hex; then stretch.
- **Bottom bar:** `DialogButton "Cancel" (Secondary)` + `DialogButton "Save theme" (Primary,
  default)`. (DialogButton variants port from `widgets/dialog_button.h`: Primary = text `text` /
  border `border_focused`; Secondary = text `text_dim` / border `border`; Destructive = text+
  border `marker_ptr`. The bottom row is right-aligned, order `{Cancel, Save}` — the
  `ThemedDialog::makeButtonRow` convention.)
  - Save → close as "accepted"; caller reads `result()`/`selected_index()` and decides
    `update_theme`/`add_theme` (the editor itself does NOT call them — matches C++ `accept()`).
  - Cancel → `manager.revert_preview()` then close as "rejected".
- After building all rows: `manager.preview_theme(theme.clone())` — **live preview starts
  immediately on open**.

### 5.2 `load_theme(index)` (`themeeditor.cpp:160-178`)
Bounds-check vs `themes()` (oob → return). Set `theme_index`, `theme = themes()[index]`, update
name input + file-info label, refresh swatch rows, then `manager.preview_theme(theme.clone())`.
Switching discards unsaved edits to the previous selection (no confirm) — matches C++.

### 5.3 swatch render (`updateSwatch`, `themeeditor.cpp:182-193`)
Per row: bg = `theme.get(id)` color (for `None`, paint `#000000` — `QColor().name()`), 1px
border = `current().border`, radius 2; hex label = the field color hex (`#000000` if `None`).
Uses the manager's *current* (live/previewed) theme for the editor's own border tint.

### 5.4 `pick_color(id)` (`themeeditor.cpp:197-205`)
Replace `QColorDialog::getColor` with gpui-component **`ColorPicker`** (cookbook line 83) seeded
with the current field color and titled with `f.label`. On confirm (valid pick): `theme.set(id,
Some(c)); refresh that row; manager.preview_theme(theme.clone())`. On cancel: no change.
(`ColorPicker` yields an `Hsla`/`Rgba`; convert to our `Color{r,g,b}` by `(channel*255).round()`.)

> The swatch grid, section headers, name edit, combo, and file-info are straightforward GPUI;
> the only behavior that MUST be preserved exactly is **preview-on-open**, **preview-on-edit**,
> and **revert-on-cancel** (the transient broadcast semantics, §3.12/§3.13). This is the
> editor's contract with the rest of the app.

---

## 6. Shipping the 8 default JSON files (map §3.3, §9)

- Copy `src/themes/defaults/{long_night,mid,modern,phosphor,reclass_dark,tw,vs,warm}.json`
  **verbatim** into `src/theme/defaults/` (identical bytes; the oracle keeps the same 8 in
  `_oracle/fixtures/themes/`).
- **Embed + ship both:** `include_str!` each (a `const DEFAULT_THEMES: &[(&str /*filename*/,
  &str /*json*/); 8]`) so the engine can always reconstruct built-ins, AND a `build.rs` copies
  them next to the binary (`target/.../themes/` for dev; installer puts them at `<exe>/themes/`
  and macOS `Contents/Resources/themes/`). This preserves `theme_file_path` reporting a real
  `<exe>/themes/<name>.json` path while guaranteeing existence (map §3.3 recommendation).
- `load_builtin_themes` reads from `builtin_dir` (disk). If the dir is missing, the app falls
  back to parsing the embedded strings in filename-sorted order (self-heal) — this keeps
  built-ins available even with a broken install, and is the path tests can use directly without
  touching disk by constructing the manager from the embedded set.
- Build status note (ARCHITECTURE §6): the full `ui` build is GREEN on Linux; no blocker for the
  editor view.

---

## 7. Error-handling strategy

- **Model (`from_json`/`to_json`):** total functions, **never fail**. `from_json` mirrors Qt's
  lenient parsing — bad/missing values become `None` (→ fallback or stay invalid), bad `name`
  → "Untitled". No `Result`. (Matches C++: no throws, `QColor("garbage")` is just invalid.)
- **`Color::parse`** → `Option<Color>` (no error type; `None` == invalid QColor).
- **Manager file I/O:** mirror C++ "skip on failure" — `load_*` skips unreadable/unparseable
  files (continue), `save_*` ignores per-file write errors. Surface nothing to the caller (the
  public methods are infallible, matching `void` returns). Use `tracing::warn!` for diagnostics
  (replacing silent Qt behavior; non-observable). Define a small `thiserror` `ThemeError` only
  for internal helpers if useful; the public API stays infallible to match the C++ signatures.
- **No panics** on malformed input. `current()` falls back to `builtin[0]` then a static empty
  `Theme` (§3.3) rather than panicking on an empty list.
- **Bounds:** all index methods bounds-check and no-op out of range (§3.4–3.7, §3.11), exactly
  as C++.

---

## 8. Concurrency / threading (map §8)

Single-threaded, UI-thread-owned. No `Mutex` inside `ThemeManager`. The manager is held by the
GPUI app (a Global) and mutated via `cx.update_global` on the main thread; observers fire
synchronously. The headless build holds it in a plain owner and calls methods directly. No
atomics, no background threads in this subsystem.

---

## 9. TEST PLAN — port of `tests/test_theme.cpp`

Tests live in `tests/theme.rs` (integration) and inline `#[cfg(test)]` unit tests in
`color.rs`/`model.rs`. Run under `--no-default-features` (no gpui) for speed (ARCHITECTURE §8).
**Isolate the data dir**: every manager test constructs `ThemeManager::new(MemSettings::new(),
builtin_dir = _oracle fixtures (or embedded), user_dir = TempDir)` — never the real user data
dir (the C++ tests mutate real files; we must not — map §7.7).

The oracle (`_oracle/RESULTS.md`) records that the upstream `test_theme` itself FAILS 3
assertions because the shipped JSON drifted from the hardcoded constants. **The Rust tests
assert the CORRECT current behavior (JSON + current `theme.cpp` win), as the oracle documents.**

| # | C++ test (`test_theme.cpp`) | Rust `#[test]` | Notes / oracle |
|---|---|---|---|
| 1 | `builtInThemes()` (:13-40) | `builtin_themes` | Load manager from fixtures. `themes().len() >= 2`. Find "Reclass Dark" + "Warm" by name; assert both `background`/`text` are `Some`; `dark.syntax_keyword`/`dark.marker_error` `Some`. **`warm.background == #212121`** ✓. **FIX (oracle):** `warm.selection == #3a2a3a`** (NOT the stale `#21213A`/`#ff21213a`). `warm.syntax_keyword == #AA9565`, `warm.syntax_type == #6B959F` ✓. |
| 2 | `jsonRoundTrip()` (:42-60) | `json_round_trip` | `orig = themes()[0]`; `to_json` → `from_json`; compare `name, background, text, selection, syntax_keyword, syntax_number, syntax_string, syntax_comment, syntax_type, marker_ptr, marker_error, ind_hover_span`. Robust to which theme is index 0 (any fully-populated built-in round-trips losslessly). |
| 3 | `jsonRoundTripWarm()` (:62-76) | `json_round_trip_warm` | Same round-trip on "Warm"; compare `name, background, selection, syntax_keyword`. |
| 4 | `fromJsonMissingFields()` (:78-90) | `from_json_missing_fields` | Build sparse `{"name":"Sparse","background":"#ff0000"}`; `from_json`. Assert `name=="Sparse"`, `background==#ff0000`, `text.is_none()`, `syntax_keyword.is_none()`. **FIX (oracle):** **`marker_error.is_some()` and `== #5a1d1d`** — current `from_json` step 4 makes markers valid; the stale C++ asserts `!isValid()`. Document this inline. Also assert `marker_ptr==#f44747`, `marker_cycle==#e8a35c`. |
| 5 | `themeManagerHasBuiltIns()` (:92-105) | `manager_has_builtins` | `themes().len() >= 3`. **FIX (oracle):** **`themes()[0].name == "Long Night"`** (filename-sort-first), NOT the stale "Reclass Dark". Then scan: "VS2022 Dark" and "Warm" both present. |
| 6 | `themeManagerSwitch()` (:107-121) | `manager_switch_emits_once` | Subscribe a counting observer (= `QSignalSpy`). `start = current_index()`; `target = if start==0 {1} else {0}`; `set_current(target)`. Assert observer fired **exactly once**, `current_index()==target`, `current().name == themes()[target].name`. Restore `set_current(start)`. |
| 7 | `themeManagerCRUD()` (:123-145) | `manager_crud` | `initial = themes().len()`. **Add:** `custom = themes()[0]; custom.name="Test Custom"; custom.background=Some(#ff0000); add_theme(custom)`; assert `len()==initial+1`, `themes().last().name=="Test Custom"`. **Update:** `idx=len()-1; updated.background=Some(#00ff00); update_theme(idx, updated)`; assert `themes()[idx].background==Some(#00ff00)`. **Remove:** `remove_theme(idx)`; assert `len()==initial`. (TempDir user_dir absorbs the `save_user_themes` writes.) |

Additional Rust-only unit tests (lock subtle behavior the C++ relied on implicitly):

- `color_parse_format_roundtrip`: `Color::parse("#1e1e1e").unwrap().to_hex() == "#1e1e1e"`;
  uppercase input `"#1E1E1E"` parses, formats lowercase; `parse("notacolor")==None`;
  `parse("#abc")==None` (we restrict to 6-digit); `hex_or_black(None)=="#000000"`.
- `field_table_count_is_31`: `THEME_FIELDS.len() == 31` (≙ `kThemeFieldCount`).
- `to_json_emits_black_for_none`: a `Theme::default()` `to_json()` has every color key
  `"#000000"` and `name` `""`.
- `heat_derivation`: build a theme with only `text_dim = #7F7E74` (Long Night's), run
  `from_json`; assert `ind_heat_cold == lerp_rgb(#7F7E74, #DCB478 /*220,180,120*/, 0.35)`
  (compute the exact expected triple in the test), `ind_heat_warm == #E1AA5A` (225,170,90),
  `ind_heat_hot == #E8A55C` (232,165,92). Also assert that when `text_dim` is absent the lerp
  uses `#858585` (133,133,133).
- `focus_glow_derivation`: theme with `border_focused=#888888`, no `focusGlow` → `focus_glow ==
  Some(#888888)`; theme with neither → `#4fc3f7`; `phosphor` (ships `focusGlow=#28cdb2`) keeps it.
- `lighter_130_matches_qt`: lock `Color::lighter_130` against 2-3 known Qt outputs
  (e.g. `#1e1e1e`→ Qt `QColor("#1e1e1e").lighter(130).name()`; compute the golden once and
  hard-code). Pure black `#000000`.lighter_130 stays `#000000`.
- `hover_distinctness_guard`: theme with `background=#202020`, `hover=#212121` (dist=3 < 20) →
  `hover == background.lighter_130()`; with `hover=#303030` (dist≥20) → `hover` unchanged.
- `preview_revert`: subscribe counter; `preview_theme(tA)` (emits 1, saved=current);
  `preview_theme(tB)` (emits 2, saved unchanged); `revert_preview()` (emits 3 with the ORIGINAL
  current, previewing→false); second `revert_preview()` no-op (still 3). Assert each emitted
  theme value (capture via the observer) — esp. that preview emits tA/tB and revert emits the
  pre-preview current (proves consumers see the *emitted* theme, map §4.16).
- `update_theme_commits_preview`: `preview_theme(tX)`; `update_theme(...)` (sets previewing=false
  + emits current); then `revert_preview()` is a no-op (no extra emit).
- `user_override_by_name`: write a user JSON named to a built-in display name (e.g. "Warm" with
  a changed `background`) into `user_dir`; `load_user_themes()`; assert it replaced the built-in
  slot (not appended to `user`), `themes().len()` unchanged, and `theme_file_path(warm_idx)`
  points into `user_dir` (since builtin now differs from `builtin_defaults`).
- `save_then_reload_user`: `add_theme(custom)`; `save_user_themes()` writes
  `test_custom.json`; new manager over the same dirs reloads it as a user theme with the same
  colors (round-trip through disk). Confirms wipe-then-rewrite + filename derivation.

> Defaults parity test (optional but cheap): a `defaults_match_oracle_fixtures` test that asserts
> the embedded `DEFAULT_THEMES` JSON bytes equal `_oracle/fixtures/themes/*.json` byte-for-byte,
> guarding against accidental drift of the shipped defaults.

---

## 10. Ordered implementation steps (small, independently verifiable)

1. **`color.rs`** — `Color`, `parse`, `to_hex`, `hex_or_black`, `lerp_rgb` (free fn or assoc),
   `lighter_130` (+ Qt HSV round-trip). Unit tests: `color_parse_format_roundtrip`,
   `lighter_130_matches_qt`. ✅ compiles `--no-default-features`.
2. **`model.rs` types** — `Theme`, `FieldId`, `ThemeFieldMeta`, `THEME_FIELDS`,
   `Theme::{get,set,default}`. Test: `field_table_count_is_31`.
3. **`model.rs` serde** — `to_json`, `from_json` (full §2.4 pipeline). Tests:
   `to_json_emits_black_for_none`, `heat_derivation`, `focus_glow_derivation`,
   `hover_distinctness_guard`, `from_json_missing_fields` (with oracle fix), round-trip tests
   (#2, #3) using the embedded defaults.
4. **defaults** — copy the 8 JSON verbatim into `src/theme/defaults/`; `DEFAULT_THEMES`
   `include_str!` table; `build.rs` copy-to-`<exe>/themes/`; macOS `Resources/themes` arm
   `#[cfg]`. Test: `defaults_match_oracle_fixtures`.
5. **`SettingsStore` trait + `MemSettings`** (test impl) and the app-settings adapter stub.
6. **`manager.rs` core** — struct, `new` (fallback selection), `themes`, `current`,
   `current_index`, `builtin_dir`/`user_dir`, `load_builtin_themes`, `load_user_themes`
   (override-by-name). Tests: `builtin_themes` (oracle fix), `manager_has_builtins` (oracle fix),
   `user_override_by_name`.
7. **`manager.rs` mutation** — `set_current`, `add_theme`, `update_theme`, `remove_theme`,
   `save_user_themes`, `theme_file_path`. Tests: `manager_switch_emits_once`, `manager_crud`,
   `save_then_reload_user`.
8. **observers / `themeChanged`** — `subscribe`/`unsubscribe`/`emit_theme_changed`. Tests:
   `preview_theme`, `revert_preview`, `preview_revert`, `update_theme_commits_preview`.
9. **`editor.rs`** `[#[cfg(feature="ui")]]` — GPUI `ThemeEditor` view (Select + TextInput +
   file-info + `THEME_FIELDS`-driven swatch list with section headers + ColorPicker +
   Save→accept / Cancel→revert + preview-on-open/edit). `result()`/`selected_index()`. Compiles
   under the default (`ui`) feature on Linux (ARCHITECTURE §6 confirms green).
10. **GPUI bridge** `[#[cfg(feature="ui")]]` — `GlobalActiveTheme` + `ActiveReclassTheme`;
    manager-as-Global; `emit_theme_changed` → update Global + `cx.refresh()`; optional
    `theme_apply` mapping onto gpui-component `Theme` (keep `selected` ≠ `selection`).

Each step ends with `cargo test --no-default-features` (steps 1-8) or `cargo build` /
`cargo test` for the ui steps (9-10), checked against the oracle behavior in `_oracle/RESULTS.md`.
