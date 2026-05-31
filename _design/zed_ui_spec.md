# Zed UI Design Language — Reclass Rust Port (authoritative spec)

This is the prescriptive design language every GPUI surface in the Reclass port
MUST follow. Its executable counterpart is [`src/ui/design.rs`](../src/ui/design.rs)
(tokens + semantic color accessors + reusable builders) and the theme mapping in
[`src/ui/theme_apply.rs`](../src/ui/theme_apply.rs). When this doc and those
modules disagree, fix both — they are one system.

**AESTHETIC TARGET = Zed** (github.com/zed-industries/zed): the One Dark palette,
comfortable ~14px UI type + a real monospace editor font, a strict 4px spacing
grid, subtle 1px low-contrast borders, 4–6px radii on elevated surfaces,
restrained shadows, content-forward chrome, and hover-overlay +
soft-accent-selected interaction states.

**Rule of thumb:** chrome recedes, content leads. No heavy fills, no loud
borders, no drop shadows except on truly-floating surfaces (popovers, modals).

---

## 0. How to consume this system

- Geometry / typography → `use crate::ui::design::tokens;` and wrap numbers in
  `gpui::px(...)` (e.g. `.px(px(tokens::space::LG))`).
- Colors → `use crate::ui::design::color;` and call the semantic accessor
  (e.g. `.bg(color::panel_bg(cx))`). NEVER hardcode hex in a surface. NEVER read
  a raw `cx.theme().<field>` when a `design::color::*` role exists — the role
  documents intent and is the single rename seam.
- Common surfaces → use the builders: `design::panel_header`, `design::section_label`,
  `design::zed_list_row`, `design::elevated_surface`.
- The global gpui-component `Theme` already carries the Zed font/radius tokens
  (set by `apply_theme`), so gpui-component widgets (Button, Input, Dialog, Tab,
  …) inherit the look automatically. Bespoke `div`s must apply tokens themselves.

---

## 1. Color tokens — Zed One Dark, mapped onto the theme

The launch theme is **"Zed One Dark"** (`src/theme/defaults/zed_one_dark.json`,
also in `_oracle/fixtures/themes/`). It is selected by default via
`ThemeRegistryGlobal::get` seeding the `"theme"` key to
`design::DEFAULT_THEME_NAME`. All values below are the resolved One Dark hues; a
theme switch re-derives every role, so surfaces stay correct under any theme.

### 1.1 Reclass theme field → One Dark hex (the bundled theme)

| Reclass field   | Hex       | Role                                   |
|-----------------|-----------|----------------------------------------|
| `background`    | `#282c34` | editor/content + window bg             |
| `backgroundAlt` | `#2f343e` | panels, popovers, tooltips, active tab |
| `surface`       | `#21252b` | darker inset surface / alt base        |
| `border`        | `#3b414d` | separators, 1px borders                |
| `borderFocused` | `#61afef` | focus ring, primary/action blue        |
| `button`        | `#3b414d` | secondary button bg                    |
| `text`          | `#c8ccd4` | primary text                           |
| `textDim`       | `#abb2bf` | bright-dim text                        |
| `textMuted`     | `#828997` | muted/secondary text                   |
| `textFaint`     | `#5c6370` | faint (margins, scrollbar thumb)       |
| `hover`         | `#2f343e` | row/tab/menu hover overlay             |
| `selected`      | `#2f343e` | selected-row soft fill                 |
| `selection`     | `#3d4350` | text-selection background (soft)       |
| `syntaxKeyword` | `#c678dd` | keyword (purple)                       |
| `syntaxNumber`  | `#d19a66` | number (orange)                        |
| `syntaxString`  | `#98c379` | string (green)                         |
| `syntaxComment` | `#5c6370` | comment (gray-green)                   |
| `syntaxPreproc` | `#56b6c2` | preproc / class (cyan)                 |
| `syntaxType`    | `#e5c07b` | type (yellow)                          |
| `indHoverSpan`  | `#61afef` | link / hover span (blue)               |
| `markerPtr`     | `#e06c75` | null pointer / error red               |
| `markerCycle`   | `#d19a66` | cycle / warning (orange)               |
| `markerError`   | `#3a1d1f` | error row bg                           |

### 1.2 Semantic color roles (use these in surfaces)

Each is a `design::color::*` accessor reading `cx.theme()`:

| Role accessor          | gpui-component field | One Dark | Use for |
|------------------------|----------------------|----------|---------|
| `chrome_bg`            | `background`         | `#282c34`| titlebar, menubar, status bar |
| `panel_bg`             | `sidebar`            | `#282c34`| workspace / scanner docks |
| `elevated_bg`          | `popover`            | `#2f343e`| popovers, dropdowns, dialogs, cards |
| `content_bg`           | `background`         | `#282c34`| editor paper (editor darkens it ~6%) |
| `border`               | `border`             | `#3b414d`| 1px separators / outlines |
| `focus_ring`           | `ring`               | `#61afef`| focused input / active edit |
| `hover_overlay`        | `list_hover`         | derived  | hovered rows/tabs/menu items |
| `selected_bg`          | `list_active`        | `#2f343e`| active list/tree row fill |
| `selection_bg`         | `selection`          | `#3d4350`| highlighted text span |
| `text`                 | `foreground`         | `#c8ccd4`| primary text |
| `text_muted`           | `muted_foreground`   | `#828997`| captions, inactive tabs, hints |
| `text_disabled`        | `muted_foreground` @55%α | —    | disabled labels |
| `accent`               | `primary`            | `#61afef`| links, primary buttons, active underline |
| `link`                 | `link`               | `#61afef`| hover-span links |

### 1.3 Editor syntax color map

The bespoke editor reads gpui-component's named accent roles, which
`apply_theme` binds to the theme's syntax fields (so a theme switch retints the
editor). Mirror via `design::color::syntax_*` for non-editor surfaces (e.g. the
type-selector tints):

| Syntax kind   | Accessor / theme bind        | gpui field     | One Dark |
|---------------|------------------------------|----------------|----------|
| keyword       | `syntax_keyword` ← syntaxKeyword | `magenta`  | `#c678dd`|
| type          | `syntax_type` ← syntaxType   | `yellow`       | `#e5c07b`|
| function/name | `syntax_function` ← indHoverSpan | `blue`     | `#61afef`|
| number        | `syntax_number`              | `yellow_light` | warm     |
| string        | `syntax_string` ← syntaxString | `green`      | `#98c379`|
| comment       | `syntax_comment` ← syntaxComment | `green_light` | `#5c6370`|
| punctuation   | `syntax_punctuation`         | `muted_foreground` | `#828997`|
| address/offset| `syntax_address`             | `muted_foreground` | `#828997`|

The editor's own role table (`ui/editor/palette.rs`) is the source of truth for
the editor surface; this map keeps everything else aligned.

### 1.4 Status / severity

`danger` ← `markerError`, `danger_foreground` ← `markerPtr`,
`warning` ← `markerCycle`. Use gpui-component's `.danger()` / `.warning()`
variants; do not hand-color.

---

## 2. Spacing — strict 4px grid (`tokens::space`)

| Token | px | Use |
|-------|----|-----|
| `XXS` | 2  | hairline inset, key-cap padding |
| `XS`  | 4  | base unit; icon↔label gap, tight inner pad |
| `SM`  | 6  | compact row padding (list rows, menu items) |
| `MD`  | 8  | default control padding / small gap |
| `LG`  | 12 | panel padding, section gap |
| `XL`  | 16 | dialog/card container padding |
| `XXL` | 24 | large block separation (start page) |

Never use off-grid paddings. Horizontal control padding defaults to `MD` (8px),
vertical to `SM`/`XS`.

---

## 3. Radius / border / shadow

- **Radius** (`tokens::radius`): `SM`=3 (chips, key-caps), `MD`=4 (buttons,
  inputs, list-row selection), `LG`=6 (popovers, dropdowns, context menus),
  `XL`=8 (modals, dialogs, cards), `FULL` (pills). The global theme `radius`=4,
  `radius_lg`=6.
- **Border** (`tokens::border`): `THIN`=1 everywhere; `THICK`=2 only for focus
  ring + active-tab accent. Border color = `color::border(cx)` always.
- **Shadow** (`tokens::shadow`): only floating surfaces cast shadows. Use
  `.shadow_md()` (popovers/dropdowns) or `.shadow_lg()` (modals). Flat chrome,
  panels, tabs, list rows cast NO shadow. `ALPHA`=0.35.

---

## 4. Typography (`tokens::font`)

| Token | px | Use |
|-------|----|-----|
| `UI_XS` | 11 | section captions, key-caps, status fine print |
| `UI_SM` | 12 | tab labels, muted captions |
| `UI_MD` | 14 | **default UI text** (global `font_size`) |
| `UI_LG` | 16 | dialog titles, headings |
| `EDITOR_SIZE` | 13 | editor monospace glyphs (global `mono_font_size`) |

- Line-height: 1.4× for both UI and editor.
- **UI family** (`UI_FAMILY`): `Inter, "SF Pro Text", "Segoe UI", Cantarell,
  "Noto Sans", "DejaVu Sans", sans-serif`.
- **Mono family** (`MONO_FAMILY`): `"JetBrains Mono", "Fira Code",
  "Cascadia Code", "Source Code Pro", "DejaVu Sans Mono", Menlo, Consolas,
  monospace`.
- Weights: body = normal (400); panel headers / section labels / active items =
  semibold/bold; never use heavier than bold.

---

## 5. Surface → recipe map

Every named surface, its background role, and its construction. Use the builders
where noted.

### 5.1 Chrome / titlebar (`ui/titlebar.rs`)
- bg `chrome_bg`, height 32–38px (gpui-component `TitleBar`), bottom 1px
  `border`. App label bold `UI_MD` in `text`. Right-aligned ghost controls.
- Window controls: gpui-component native min/max/close.

### 5.2 Menubar (`ui/menubar.rs`, stub)
- bg `chrome_bg`. Top-level menu titles `UI_SM` in `text_muted`, hover →
  `hover_overlay`, `MD` radius. Dropdowns are popovers (§5.10).

### 5.3 Workspace / scanner panels (docks)
- bg `panel_bg`. Each dock leads with a `design::panel_header(title, cx)`
  (28px, uppercase `UI_SM` semibold `text_muted`, bottom 1px border).
- Tree/list rows via `design::zed_list_row(id, selected, cx)` (24px, 8px pad,
  `MD` radius, hover overlay, selected soft-accent fill).

### 5.4 Editor grid (`ui/editor/*`)
- paper = `content_bg` darkened ~6%. Monospace `EDITOR_SIZE`, 1.4 line-height.
- Address/offset margin: `syntax_address` (muted) digits, no border, fixed-width
  from `offset_hex_digits`.
- Selected row: `selected_bg` fill + 2px left accent bar in `accent`. Hovered
  row: `hover_overlay`. Text selection span: `selection_bg`. Caret: `caret`.
- Syntax spans per §1.3. Heat backgrounds from `heat_cold/warm/hot` (amber, never
  red).

### 5.5 Scanner results table
- gpui-component `DataTable`. Header bg `table_head`, row hover `table_hover`,
  active `table_active`, 1px `table_row_border`. Right-align numeric columns;
  monospace addresses. Inherits theme.

### 5.6 Document tabs (`ui/tabs.rs`)
- gpui-component `TabBar` over `chrome_bg`. Active tab bg `tab_active`
  (`backgroundAlt`), inactive `tab` text `tab_foreground` (`text_muted`). Active
  tab carries a 2px bottom/side accent in `accent`. Close "×" appears on hover.
  Source icon badge tinted by liveness (full vs `text_muted`@dim).

### 5.7 Start page (`ui/startpage.rs`)
- bg `content_bg`. Centered. Block separation `XXL`. Section captions via
  `design::section_label`. Recent items via `zed_list_row`. Primary actions =
  gpui-component primary `Button` (accent blue); links = `link`.

### 5.8 Dialogs / modals
- gpui-component `Dialog`/`AlertDialog` → built on `design::elevated_surface`
  semantics: `elevated_bg`, 1px `border`, `XL` (8px) radius, `.shadow_lg()`.
  Title `UI_LG` semibold; body `UI_MD`; section dividers via `section_label`.
  Footer right-aligned, convention `{Cancel(secondary), OK/Primary}`.

### 5.9 Command palette (`ui/commandpalette.rs`)
- Centered modal on `elevated_surface` (rounded, `.shadow_lg()`). Query `Input`
  at top (no border, `elevated_bg`). Virtualized rows via `zed_list_row`;
  selected = soft-accent fill; key-cap chips right-aligned (`SM` radius,
  `UI_XS`, `border` outline). This is the reference for picker styling.

### 5.10 Menus / popovers / dropdowns / context menus
- gpui-component `PopupMenu`/`Popover` → `elevated_bg`, 1px `border`, `LG` (6px)
  radius, `.shadow_md()`. Items `UI_MD`, `SM` vertical pad, hover overlay,
  separators 1px `border`. Checkmarks/icons in `text_muted`.

### 5.11 Tooltips (`ui/tooltip.rs`)
- gpui-component `Tooltip`: `elevated_bg`, 1px `border`, `MD` radius, `UI_SM`
  text, `SM`/`XS` pad, `.shadow_md()`.

### 5.12 Status bar (`ui/statusbar.rs`, stub)
- bg `chrome_bg`, top 1px `border`, height ~22px. `UI_XS` `text_muted`. Flex row:
  source readout (left), spacer, selection/offset info (right). Shimmer label is
  a bespoke `Element`.

---

## 6. Component recipes (atoms)

| Component | Recipe |
|-----------|--------|
| **Button (primary)** | gpui-component `Button::new(id).primary()` → `primary` bg (accent blue), `primary_foreground` text, `MD` radius, hover `primary_hover`. |
| **Button (secondary)** | `.outline()`/default → `secondary`/`button` bg, 1px `border`, `text`. |
| **Button (ghost)** | `.ghost()` → transparent, hover `hover_overlay`. Chrome toggles. |
| **Button (danger)** | `.danger()` → `danger` role. |
| **Input** | gpui-component `TextInput` → `elevated_bg`/`input` bg, 1px `border`, focus 2px `focus_ring`, `MD` radius, `UI_MD`. Mono inputs use `MONO_FAMILY`. |
| **List row** | `design::zed_list_row(id, selected, cx)`. 24px, 8px pad, `MD` radius, hover overlay, selected soft-accent. |
| **Panel header** | `design::panel_header(title, cx)`. 28px, uppercase `UI_SM` semibold `text_muted`, bottom 1px border. |
| **Section label** | `design::section_label(text, cx)`. Uppercase `UI_XS` bold `text_muted`, bottom 1px border. |
| **Tab** | gpui-component `Tab` in `TabBar`. Active `tab_active` + 2px accent edge; inactive `tab_foreground`. |
| **Dropdown / Select** | gpui-component `Select`/`Combobox`. Trigger like Input; menu like §5.10. |
| **Tooltip** | gpui-component `Tooltip`, per §5.11. |
| **Modal** | `design::elevated_surface` + `.shadow_lg()`, `XL` radius, per §5.8. |
| **Table** | gpui-component `DataTable`, per §5.5. |
| **Status bar** | bespoke flex per §5.12. |
| **Key-cap chip** | `SM` radius, `UI_XS`, `XXS`/`XS` pad, 1px `border`, `text_muted`. |
| **Pill / tag** | `FULL` radius, `UI_XS`, tinted bg at low alpha of its accent. |

---

## 7. Interaction states (apply consistently)

- **Hover**: overlay `hover_overlay` (a subtle lift), never a border change.
- **Selected/active**: soft-accent fill `selected_bg` + (for editor rows/tabs) a
  2px `accent` edge. Text stays `text` (do NOT invert to a loud accent fill).
- **Focused (inputs)**: 2px `focus_ring` ring, `THICK` border.
- **Disabled**: `text_disabled` (muted @55% alpha), no hover.
- **Pressed**: one step darker/lighter than hover (gpui-component `*_active`).

Keep contrast low and motion minimal — content leads, chrome recedes.
