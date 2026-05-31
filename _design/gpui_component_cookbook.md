# gpui-component Evaluation Cookbook (for the Reclass Rust port)

Authoritative evaluation of **[longbridge/gpui-component](https://github.com/longbridge/gpui-component)**
as the standard-widget library for Reclass's GPUI port. Everything here is traceable to the actual
library source cloned at `/tmp/gpui_component_src` (read it, not web docs — API moves fast). Read the
raw-gpui reference `/home/loke/reclass-rs/_design/gpui_cookbook.md` first; this builds on it.

- **Library source of truth:** `/tmp/gpui_component_src/crates/ui/src` (+ `examples/`, `crates/story`)
- **gpui-component commit read/built here:** `bd4174e4991eef4c0900cb6dfb7143e0889f5dd9` (crate `gpui-component v0.5.2`, default branch HEAD, 2026-05-31)
- **gpui it depends on:** Zed crate `gpui 0.2.2` / `gpui_platform 0.1.0`, declared as an **unpinned** `git` dep (no `rev`). Its own `Cargo.lock` pins `4bee4121…`, but consumed as a git dep Cargo re-resolves it to Zed default-branch HEAD = **`09165c15dc5d1fea93604231eaf30ca4c25f1cd6`** — the SAME commit our raw-gpui cookbook pinned and built.
- **Probe build result:** **SUCCESS** on Linux (nightly `cargo 1.97 (2026-04)`), `gpui_component::init` + `Root` + a `Button` window. Binary `/tmp/gpui_component_probe/target/debug/gpui_component_probe` (~660 MB debug). Only failures were missing system dev libs (fixed; see §4), never code/version errors after deps were aligned.

> **TL;DR:** USABLE — **yes**. Adopt gpui-component for all standard chrome (titlebar, docks/tabs,
> dialogs, tables, tree, inputs, menus, theme). Keep the bespoke virtualized structured-editor surface
> as a custom raw-gpui `Element` (gpui-component's Table/List/code-editor do NOT model it). It is
> Apache-2.0, actively maintained, 60+ components, virtualized Table/List, full dock system, ships in a
> commercial product (Longbridge Pro). It rides the SAME gpui we already pinned, so there's no version
> split if you declare gpui the same way it does (see §3 — this is the one real footgun).

---

## 1. COMPONENT CATALOG

All components live under crate `gpui-component` (lib at `crates/ui/src/lib.rs`). Module list:
`lib.rs:24-77`. Init: call `gpui_component::init(cx)` once at startup (`lib.rs:105-127`) — it wires
keybindings + globals for theme, dock, input, list, table, tree, menu, popover, dialog, etc. Every
top-level window view must be wrapped in `Root::new(view, window, cx)` (`root.rs:79`; required for
overlays/modals/notifications/tooltips to render).

Theme access trait `ActiveTheme` gives `cx.theme()` returning `&Theme` (`theme/mod.rs:32-41`); colors
are fields like `cx.theme().background`, `.foreground`, `.border`, `.accent`, `.muted_foreground`
(`theme/theme_color.rs:11`).

### Buttons & toggles
- **Button** — `button/button.rs`. `Button::new(id).label("…").primary().on_click(|_,_,_| …)`
  (`button.rs:222,285,332`; variants via trait `ButtonVariants`: `.primary()/.danger()/.ghost()/.outline()` — **must `use gpui_component::button::ButtonVariants`**, `button.rs:46`). Also `.icon(IconName::…)`, sizes via `Sizable` (`.small()/.large()`). Extras: `ButtonGroup` (`button/button_group.rs`), `DropdownButton` (`button/dropdown_button.rs`), `ToggleButton` (`button/toggle.rs`).
- **Checkbox** — `checkbox.rs:32`. `Checkbox::new(id).checked(b).on_click(|checked: &bool,_,_| …)`.
- **Radio / RadioGroup** — `radio.rs:34,245`. `RadioGroup::…on_click(|ix: &usize,…|)`.
- **Switch** — `switch.rs:29`. `Switch::new(id).checked(b).on_click(…)`.

### Text & labels
- **Label** — `label.rs`. **Link** — `link.rs`. **Kbd** (keystroke chip) — `kbd.rs`. **Tag**/**Badge** — `tag.rs`/`badge.rs`. **Text** (rich/markdown/HTML) — `text/` module (markdown + simple HTML rendering).
- **Input (the big one)** — `input/` module. `InputState` is an `Entity` you build in your view; the widget is `TextInput::new(&state)`. `InputState::new(window, cx)` (`input/state.rs:444`). Builder/setters: `.placeholder()` (`:592`), `.multi_line()` (`:536`), `.masked()` for passwords (`:847`), `.pattern(regex)` validation (`:922`), `.mask_pattern()` formatted input (`:2132`), `.soft_wrap()` (`:878`), `.line_number()` (`:623`), `.value()`/`.set_value()` (`:973,754`), `.searchable()` (`:585`). This is a **rope-backed, IME-correct, full editor**: `input/display_map` (folding via `Tree`), `input/lsp` (diagnostics/completion/hover), `input/search.rs` (find bar), code-editor mode `.code_editor(lang)` (`:568`) with tree-sitter highlighting. Also `NumberInput` (`input/number_input.rs`) and `OtpInput` (`input/otp_input.rs`).

### Selection / pickers (filtered-list + input — Reclass-critical)
- **Select** (dropdown) — `select.rs`. `SelectState<D: SearchableListDelegate>` + `Select::new(&state)` (`select.rs:97,600`); `.placeholder()` (`:622`). The delegate (`searchable_list/delegate.rs:46`) supplies `items_count`, `item`, `perform_search(query) -> Task` (async filter), `render_item`, `is_item_checked` — i.e. a **searchable dropdown with built-in fuzzy filtering**.
- **Combobox** — `combobox.rs:95,715`. Same `SearchableListDelegate`, editable trigger.
- **List** — `list/` module. `ListState<D: ListDelegate>` + `List::new(&state)` (`list/list.rs:94,696`). Delegate (`list/delegate.rs:10`): `items_count`, `render_item` (note "every item should have same height" → virtualized), **`perform_search(query) -> Task`** built in, sections, `set_selected_index` (`:184`). This is the **command-palette / fuzzy-picker primitive**: query input + virtualized filtered rows + sections, all in one.
- **SearchableList** — `searchable_list/` (shared infra behind Select/Combobox).

### Tables (two distinct constructs — important)
- **`Table`** — `table/table.rs:34`. A **simple, stateless, composable, NON-virtualized** table (`TableHeader`/`TableRow`/`TableHead`/`TableCell`/`TableCaption`). For small static tables only. Doc says explicitly "without virtual scrolling or column management" (`table.rs:11-13`).
- **`DataTable` + `TableState<D: TableDelegate>`** — `table/data_table.rs:99`, `table/state.rs`, `table/delegate.rs:16`. The real one. **Virtualized** (built on gpui `uniform_list`, `state.rs:19,227`). Features (`data_table.rs:50-70`): **sortable** columns (`TableDelegate::perform_sort`, `delegate.rs:29`; `Column::sortable()`, `column.rs:109`), **resizable** columns (`column.rs:171`, `state.rs:310`), **movable** columns (`move_column`, `delegate.rs:121`), **fixed/pinned** columns, **row/column/cell selection** (`state.rs:322,350`), keyboard nav (arrows/home/end/pageup/down/tab, `data_table.rs:16-27`), **per-row context menus** (`context_menu`, `delegate.rs:101`), **infinite scroll / load-more** (`has_more`/`load_more`, `delegate.rs:164-180`), stripe/bordered styles, loading skeleton, empty state.
  - **Editable cells?** Not as a built-in "edit mode" — but `render_td(row, col, …) -> impl IntoElement` (`delegate.rs:112`) returns **any element**, so you can return a `TextInput` (or other widget) to make a cell inline-editable. So: editable cells = yes, by composing an Input into the cell, with you owning commit logic. No first-class spreadsheet-style cell editor.

### Tree
- **Tree** — `tree.rs`. `TreeState` (`tree.rs:209`) holds items: `.items(vec![TreeItem::new(id,label).child(...)])` (`tree.rs:222`, `TreeItem::new` `:125`); render with `tree(&state, |ix, entry, selected, window, cx| ListItem::new(ix).pl(px(16.)*entry.depth()).child(...))` (`tree.rs:51,523`). **Virtualized** (`uniform_list`, `tree.rs:6`), keyboard expand/collapse/select (`tree.rs:18-25`), context menus (`ContextMenuExt`). Good fit for the workspace nav tree.

### Tabs, Dock/Panel system, splits (Reclass MDI + dockable panels)
- **Tabs** — `tab/` module. `TabBar::new(id).children([Tab::new()…]).selected_index(i).on_click(|ix,…|)` (`tab/tab_bar.rs:58,132,144,158`; `Tab` `tab/tab.rs:465,530`). Stateless tab strip.
- **Dock system** — `dock/` module. This is a full Zed-style workspace:
  - **`DockArea`** (`dock/dock.rs:44,520`) — the workspace root: a **center** `DockItem` + optional **left/right/bottom** `Dock`s (`left_dock`/`right_dock`/`bottom_dock` `:594-604`, `set_*_dock` `:633-677`, `toggle_dock` `:793`, `is_dock_open` `:721`). Persists/restores layout: **`dump()` / `load()`** (`:957,928`) returning `DockAreaState` (serde) — maps directly to Reclass's `QSettings` layout persistence. Zoom a panel (`set_zoomed_in` `:1070`).
  - **`DockItem`** (`dock/dock.rs:78`) — recursive layout node: `DockItem::split/v_split/h_split` (`:181-216`) resizable splits, `DockItem::tabs(...)` (`:326`) a **tab group of panels**, `DockItem::tiles(...)` (`:272`) freeform tiling, `DockItem::panel(...)` (`:262`) a leaf.
  - **`Panel` trait** (`dock/panel.rs:54`) — what your panel views implement: `panel_name`, `title` (`:69`), `title_suffix`, `closable` (`:92`), `zoomable` (`:99`), **`toolbar_buttons`** (`:147` — per-panel toolbar), `dump` (`:156` serialize state). `PanelView` is the object-safe form (`:168`). Panels are `Entity<T: Panel>`.
  - **`StackPanel`/`TabPanel`/`Tiles`** (`dock/stack_panel.rs`, `tab_panel.rs`, `tiles.rs`) — the concrete layout containers.
- **Resizable splits (standalone)** — `resizable/` module: `h_resizable(id)`/`v_resizable(id)` + `resizable_panel()` (`resizable/mod.rs:15-25`), `ResizablePanelGroup`/`ResizablePanel`/`ResizableState`. Use when you want splits without the full dock machinery.

### Overlays: modal/dialog, popover, tooltip, menu, notification, sheet
- **Dialog / Modal** — `dialog/` module. `Dialog::new(cx)` (`dialog/dialog.rs:228`) with `DialogHeader`/`DialogTitle`/`DialogContent`/`DialogFooter`/`DialogClose`/`DialogAction` parts (`dialog/*.rs`). **`AlertDialog`** (`dialog/alert_dialog.rs:80`, `.confirm()` `:95`) for message-box/confirm. Modals render through `Root`. (Open via the window/root modal API initialized in `dialog::init`.)
- **Sheet** — `sheet.rs` — slide-in side panel (drawer).
- **Popover** — `popover.rs:46`. `Popover::new(id).trigger(button).content(|window,cx| …)` (`:81,141`), `.anchor(...)` (`:69`); `PopoverState` for controlled open (`:207`). The mechanism behind type/enum/source picker popups.
- **HoverCard** — `hover_card.rs` — rich hover popover.
- **Tooltip** — `tooltip.rs:41`. `Tooltip::new(text).build(window, cx)`; attach to any interactive element. Reclass's themed tooltip maps here.
- **Menu / ContextMenu** — `menu/` module. **`PopupMenu`** (`menu/popup_menu.rs:273`): `.menu(label, action)` (`:392`), `.menu_with_icon` (`:470`), `.menu_with_check` (`:492`), `.separator()` (`:607`), `.submenu(...)` (`:621`). **`ContextMenuExt`** trait (`menu/context_menu.rs:13`) adds `.context_menu(|menu, window, cx| menu.menu(...))` to ANY element for right-click menus. **`AppMenuBar`** (`menu/app_menu_bar.rs:26`) for an in-window menu bar; **`DropdownMenu`** (`menu/dropdown_menu.rs`).
- **Notification (toast)** — `notification.rs`. `NotificationList` (`:444`) with `.push(Notification…)` (`:460`); types Info/Success/Warning/Error (`:23`). Rendered globally via `Root`.

### Chrome, theme, misc
- **TitleBar** — `title_bar.rs:32`. `TitleBar::new()` custom window titlebar with window controls (`.on_close_window` `:51`), `WindowBorder`/`window_border()` (`window_border.rs`) for frameless windows. Directly serves Reclass's custom frameless titlebar.
- **Sidebar** — `sidebar/` module (`SidebarHeader`/`SidebarGroup`/`SidebarMenu`/`SidebarFooter`).
- **Theme / ThemeProvider** — `theme/` module. `Theme` is a `Global` (`theme/mod.rs:109`); `cx.theme()` via `ActiveTheme` (`:32`). **Runtime switchable: yes** — `Theme::change(ThemeMode::Dark, Some(window), cx)` (`:161`) swaps colors and calls `window.refresh()`; `Theme::sync_system_appearance` (`:140`) follows OS. **`ThemeRegistry`** (`theme/registry.rs`) loads built-in + user themes from JSON (`theme/default-colors.json`, `.theme-schema.json`); themes are flat `ThemeColor` structs (`theme/theme_color.rs:11`) plus `HighlightTheme` for syntax. Mirrors Reclass's `ThemeManager` model almost 1:1 (named color bundle + JSON + change broadcast → re-render).
- **Icon** — `icon.rs:46`. `Icon::new(IconName::…)` or `.path(svg)`. **No SVG assets bundled** — you ship your own SVGs named per `IconName` (README "Icons"). Reclass already has its own SVG icon set → tint via theme.
- **Scrollbar** — `scroll/scrollbar.rs:306`. `Scrollbar::vertical(&handle)` / `::horizontal` / `::new` (`:324-344`); show modes (always/hover/scrolling) in theme.
- **Also:** Accordion, Alert, Avatar, Breadcrumb, Calendar/DatePicker (`time/`), Chart/Plot (`chart/`, `plot/`), Clipboard, Collapsible, ColorPicker, DescriptionList, Form, GroupBox, Pagination, Progress, Rating, Separator, Skeleton, Slider, Spinner, Stepper, VirtualList (`virtual_list.rs` — variable-size virtualization, generalization of `uniform_list`), WebView (`crates/webview`).

---

## 2. RECLASS SURFACE → gpui-component MAPPING

Reclass docs read: `understand/app-shell.md`, `widgets-dialogs.md`, `editor-surface.md`,
`controller.md`, `themes.md`. Mapping:

| Reclass surface (source) | gpui-component component | Notes / fit |
|---|---|---|
| Custom frameless titlebar (`titlebar.cpp`, app-shell §5) | **`TitleBar`** + `WindowBorder` | Direct fit. macOS native titlebar branch stays `#[cfg]`. |
| In-window menu bar / menus (`createMenus`, app-shell §7) | **`AppMenuBar`** + `PopupMenu` | Or use gpui native `cx.set_menus` on macOS; AppMenuBar for Win/Linux frameless. |
| MDI document tabs (QDockWidget tabbed, app-shell §8) | **`DockArea` center = `DockItem::tabs(...)`** of doc panels | Each open struct doc = a `Panel`. Tab strip, close buttons, source icons via `title_suffix`/`toolbar_buttons`. The sentinel "+" tab = a custom tab/toolbar button. |
| Dockable workspace + scanner/symbols/bookmarks panels (app-shell §10, scanner) | **`DockArea` left/right/bottom `Dock`s**, each panel a `Panel` | Direct fit. `dump()/load()` replaces `QSettings` dock layout persistence. Dock drag-overlay (app-shell §9) is built in. |
| Split panes within a doc (Reclass/Code/Debug tabs per SplitPane, app-shell §1) | **`DockItem::split` / `TabPanel`** or **`resizable`** + **`TabBar`** | DockItem nesting models the splitter+tabwidget tree. |
| Workspace tree (`QStandardItemModel`+filter, widgets-dialogs §0) | **`Tree`** (+ a `TextInput` filter above) | Virtualized; keyboard nav + context menus built in. Filtering you own. |
| Scanner results table (`QTableWidget`, widgets-dialogs §17) | **`DataTable`** | Virtualized, sortable, resizable cols, context menus, custom-painted cells via `render_td`. Strong fit. |
| Process picker table (`processpicker`, widgets-dialogs §6) | **`DataTable`** (or `List`) | Columns + selection. OS enumeration stays `#[cfg]`. |
| Profiler table (widgets-dialogs §18) | **`DataTable`** | Fit. |
| OptionsDialog (nav tree + stacked pages + search, widgets-dialogs §5) | **`Dialog`** + **`Tree`** + conditional render + **`TextInput`** | Compose: tree on left, page on right keyed by selection. |
| GotoAddressDialog (live-validated input, widgets-dialogs §7) | **`Dialog`** + **`TextInput`** (`.pattern()` / `mask`) + **`AlertDialog`** | Live validation via `pattern` or on-change. |
| ThemedMessageBox / ThemedInputDialog (widgets-dialogs §3,4) | **`AlertDialog`** / **`Dialog`+`TextInput`** | Direct. |
| **Command palette** (filtered list+input, widgets-dialogs §8) | **`List<ListDelegate>`** (built-in `perform_search`) inside a centered modal via `Root` | Built-in query+virtualized filtered rows+sections. Plug `nucleo` into `perform_search`. Strong fit. |
| TypeSelectorPopup (the centerpiece, custom-painted rows, widgets-dialogs §9) | **`Popover`/modal** + **`List`/`Select` w/ `SearchableListDelegate`** + custom `render_item` | List gives filtering+virtualization+keyboard; custom row paint via `render_item` returning your element (the §9 `TypeSelectorDelegate` custom paint → a `RenderOnce` row). Feasible. |
| EnumPickerPopup (widgets-dialogs §10) | **`Popover`** + **`List`/`Select`** | Fit. |
| SourceChooserPopup (two-line cards, widgets-dialogs §11) | **`Popover`** + **`List`** custom `render_item` | Fit. |
| HexToolbarPopup (custom-painted, widgets-dialogs §12) | **`Popover`** + a **custom canvas/`Element`** for the painted toolbar | gpui-component gives the popover shell; the bespoke painting stays raw-gpui. |
| Tooltips (RcxTooltip, widgets-dialogs §15) | **`Tooltip`** / `HoverCard` | Themed automatically. |
| Context menus (node right-click) | **`ContextMenuExt` + `PopupMenu`** | Direct. |
| Find bar (editor-surface) | **`TextInput` `.searchable()`** + the editor's own search hooks, OR compose a `TextInput` + buttons | The Input module has a search subsystem; for the custom editor surface you wire find into your own state. |
| Theme system (themes.md, `ThemeManager`) | **`Theme` + `ThemeRegistry`** (JSON, runtime `Theme::change`) | Near-1:1. Port Reclass's ~31 named colors into a `ThemeColor`-shaped struct + JSON; map `selected`≠`selection` carefully. |
| Status bar (app-shell §11, ShimmerLabel) | plain `div`/flex + `Label` (+ a small custom shimmer `Element`) | No dedicated component; trivial to build. |
| Start page (VS2022-style, app-shell §13) | plain `div`/flex + `Button`/`Link`/`List` | Compose from primitives. |
| **The structured-editor surface** (`editor.cpp`, QScintilla grid, editor-surface.md) | **CUSTOM raw-gpui `Element`** (see §below) — NOT a gpui-component widget | See assessment. |

### Can gpui-component's Table/List back the bespoke editor surface? — NO. Keep it custom.

The Reclass editor (editor-surface.md) is a virtualized grid of **per-line styled monospace text** with
**multiple inline-editable column spans per row** (type/name/value/comment, hex byte cells, array nav),
**continuous byte/text selection across rows**, fold glyphs, per-byte heatmap row backgrounds, tail
"chips", hover previews, tab-cycling between editable targets, and exact column geometry
(`LineGeometry`). Assessment of each candidate:

- **`DataTable`** is row/column/cell-structured with one element per cell. The editor's lines are NOT a
  fixed column grid — they're a single shaped text line with computed sub-spans, mixed glyphs, and
  per-span coloring/decoration; selection is character/byte-continuous, not cell-discrete. Forcing it
  into DataTable cells loses the text-shaping, cross-cell selection, and column-arithmetic parity.
- **`List`/`uniform_list`/`VirtualList`** give you the virtualization you want, but each row's *content*
  still has to be the bespoke styled-text + inline-edit element. The library has no element that shapes
  per-span-colored text with multiple inline-editable regions and shared selection.
- **The Input/code-editor** (`input/`) is a real rope-backed editor, but it edits ONE text buffer with
  uniform syntax highlighting — it can't express "this line is a struct field with 4 independently
  editable, independently colored, click-target spans plus byte cells plus chips."

**Conclusion (matches `gpui_cookbook.md` §3):** the editor surface stays a **custom raw-gpui `Element`**
(or `uniform_list` of custom row elements) using `window.text_system().shape_line(...)` + per-span
`TextRun`s + `EntityInputHandler` for inline edit + `paint_quad` for selection/cursor/heat. You MAY use
gpui-component's `VirtualList`/`uniform_list` as the scroll container, but the row painting + hit-testing
is yours. Everything *around* the editor (tabs, docks, dialogs, tree, scanner table, pickers, theme) is
gpui-component.

### Gaps gpui-component does NOT cover (build these yourself on raw gpui)
1. **The structured-editor surface** — custom Element (above). The single biggest piece.
2. **Hex-toolbar custom painting / size-bar / chip painting** — popover shell exists; the painted
   internals are custom canvas/Element.
3. **Status-bar shimmer label** — trivial custom Element.
4. **Dock drag-overlay drop-zone visuals** — DockArea has drag/drop, but Reclass's specific overlay
   look (DropZone visuals, size readout, app-shell §9) may need tweaks/custom paint.
5. **Reclass's exact fuzzy tie-break semantics** (widgets-dialogs §13, three distinct scorers) — provide
   your own scorer (`nucleo` or port each) inside `perform_search`; the List/Select just call it.
6. **Source/tab source-icon tinting, sentinel "+" tab** — compose from Tab + Icon + custom click logic.
7. **SVG icon assets** — not bundled; ship Reclass's own SVGs.

No *hard* blocker: every gap is additive custom paint on top of, or alongside, gpui-component, using the
same gpui primitives documented in `gpui_cookbook.md`.

---

## 3. VERSION / COMPAT RECONCILIATION (critical)

**What gpui-component depends on.** Its workspace `Cargo.toml` (`/tmp/gpui_component_src/Cargo.toml`)
declares gpui as **unpinned git**:
```toml
gpui          = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }
```
Its checked-in `Cargo.lock` resolved that to Zed commit **`4bee412118dafea3bbd491cd044d354f16b3d665`**
(`gpui 0.2.2`, `gpui_platform 0.1.0`). **But a consumer's Cargo.lock does not inherit a dependency's
Cargo.lock.** When you depend on gpui-component as a git dep, Cargo re-resolves its unpinned
`gpui = { git = ".../zed" }` to the Zed **default-branch HEAD at lock time**, which currently is
**`09165c15dc5d1fea93604231eaf30ca4c25f1cd6`** — the EXACT commit our raw-gpui cookbook pinned and built.
Same `gpui 0.2.2` / `gpui_platform 0.1.0`, same split-platform layout (`application()` lives in
`gpui_platform`). So they reconcile cleanly.

**The footgun (and it WILL bite — it bit the probe):** Cargo treats `git+url?rev=X` and `git+url`
(branch) as **different sources**. If you pin your own `gpui` with `rev = "09165c1…"` while gpui-component
uses unpinned `git`, you get **TWO copies of `gpui` in the graph** → "multiple different versions of
crate `gpui`" type-mismatch errors (e.g. `Context<T>`, `Styled`, `Root::new` won't line up). Observed
exactly this on the first probe attempt.

**Fix / recommendation — standardize on ONE gpui declared the SAME way gpui-component declares it:**
declare your `gpui`/`gpui_platform` as **plain unpinned `git` (NO `rev`)**, matching gpui-component, and
let the workspace `Cargo.lock` pin the commit. That yields a single `gpui` source. After resolution the
lock pins **`09165c15dc5d1fea93604231eaf30ca4c25f1cd6`** — i.e. you ARE standardizing on the same gpui
revision the raw-gpui cookbook validated.

- If you want determinism, COMMIT the `Cargo.lock` (it pins `09165c1…`). Do **not** convert your gpui dep
  to a `rev =` form while gpui-component stays unpinned — that re-splits the graph. (If you ever must pin,
  pin via a `[patch."https://github.com/zed-industries/zed"]` or fork so both deps share one source.)
- Standardize on gpui **`09165c15dc5d1fea93604231eaf30ca4c25f1cd6`** (= `gpui 0.2.2`, `gpui_platform 0.1.0`).

**Exact, mutually-compatible Cargo.toml dependency lines (verified to build):**
```toml
[dependencies]
# Declare gpui the SAME (unpinned-git) way gpui-component does, so the graph has ONE gpui source.
gpui          = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "bd4174e4991eef4c0900cb6dfb7143e0889f5dd9" }
```
- Pinning **gpui-component** by `rev` is fine (it does not re-split gpui, since gpui-component re-declares
  gpui as the same unpinned source internally). Pin it for reproducibility.
- `gpui_platform` features: `font-kit` (text/fontconfig on Linux/macOS), `x11`+`wayland` (Linux display),
  `runtime_shaders`. These mirror gpui-component's workspace. On Windows you don't need x11/wayland;
  gate features per-target if desired, but the above builds on Linux.
- Commit the resulting `Cargo.lock` (pins gpui `09165c1…`).

---

## 4. PROBE BUILD

**Location:** `/tmp/gpui_component_probe`. **Result: SUCCESS.** A window using
`gpui_platform::application()` → `gpui_component::init(cx)` → `Root::new(view, …)` → a `Button`.

### Working `Cargo.toml`
```toml
[package]
name = "gpui_component_probe"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
gpui          = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "bd4174e4991eef4c0900cb6dfb7143e0889f5dd9" }
```

### Working `src/main.rs` (this exact file built + linked)
```rust
use gpui::*;
use gpui_component::{
    button::{Button, ButtonVariants},
    ActiveTheme, Root,
};

pub struct Demo;

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex().flex_col().gap_2().size_full().items_center().justify_center()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child("gpui-component probe")
            .child(
                Button::new("ok").primary().label("Let's Go!")
                    .on_click(|_, _, _| println!("Clicked!")),
            )
    }
}

fn main() {
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx); // REQUIRED before using any component
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| Demo);
                // Top-level view MUST be a Root (enables overlays/modals/toasts/tooltips).
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("failed to open window");
        })
        .detach();
    });
}
```

### Build notes / errors hit and their fixes (all environmental or trivial, none version-fatal)
1. **`gpui` version split** → `error: multiple different versions of crate gpui` / `Root::new` &
   `cx.theme()` mismatch. **Cause:** probe initially pinned `gpui` with `rev=4bee412` while gpui-component
   pulled unpinned `09165c1`. **Fix:** declare gpui as plain unpinned `git` (§3). After fix: ONE gpui
   (`09165c1`), all crates compiled.
2. **`.primary()` not found on `Button`.** **Fix:** `use gpui_component::button::ButtonVariants;`
   (variants are a trait, `button.rs:46`).
3. **Linker: `unable to find library -lfontconfig`** (the `font-kit` feature). **Fix (Fedora):**
   `dnf install fontconfig-devel` (pulls freetype/harfbuzz devel too).
4. **Linker: `-lxkbcommon-x11` not found.** **Fix:** `dnf install libxkbcommon-x11-devel`.
5. **Linker: `-lstdc++` not found** (gpui-component pulls C++ deps, e.g. tree-sitter). `libstdc++-devel`
   was installed but Fedora 44 ships the linker symlink `libstdc++.so` under
   `/usr/lib/gcc/x86_64-redhat-linux/16/`, not `/usr/lib64`. **Fix:** build with
   `LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16` (or add a `build.rs`/`.cargo/config.toml`
   `rustflags = ["-L", "/usr/lib/gcc/x86_64-redhat-linux/<ver>"]`, or symlink). After this:
   `Finished dev profile … cargo build` exits 0.

**Linux build prerequisites (superset of raw-gpui, because of `font-kit`/x11/tree-sitter):** Vulkan loader,
X11/XCB/XKB (`libxkbcommon-x11-devel`), Wayland, **`fontconfig-devel` + freetype/harfbuzz devel**, a C++
toolchain with a discoverable `libstdc++.so` (`gcc-c++`/`libstdc++-devel` + correct `-L`). Windows uses
native DirectX 11 (no Vulkan/wgpu/fontconfig) — these Linux libs are not needed there (see raw cookbook
§1.4/§1.5). The cold build pulls ~600+ crates (wgpu, naga, resvg, swash, cosmic-text, accesskit, tree-
sitter, markdown/html parsers, chrono, etc.); debug binary ~660 MB.

---

## 5. RECOMMENDATION

**Use gpui-component for the standard chrome + raw gpui for the custom editor surface — YES.**

- **Adopt gpui-component** for: TitleBar, AppMenuBar/menus, the entire dock/tab/split workspace
  (`DockArea`/`DockItem`/`Panel`, with `dump`/`load` layout persistence), workspace `Tree`, scanner/
  process/profiler `DataTable`s, all dialogs (`Dialog`/`AlertDialog`), command palette & pickers
  (`List`/`Select`/`Combobox` with `perform_search`), popovers/tooltips/context menus, notifications,
  `TextInput`/`NumberInput`, scrollbars, and the `Theme`/`ThemeRegistry` system (runtime-switchable,
  near-1:1 with Reclass's `ThemeManager`).
- **Keep raw gpui** (per `gpui_cookbook.md`) for: the structured-editor surface (custom `Element` with
  per-span styled text + inline edit + cross-row selection), hex-toolbar/chip/size-bar custom painting,
  status-bar shimmer, and any pixel-exact custom visuals. These compose cleanly alongside components.

**Blockers:** none fatal. The one real pitfall is the **gpui version-split footgun** (§3) — declare gpui
the same unpinned way gpui-component does and commit `Cargo.lock`. Secondary: heavier Linux build deps
(fontconfig/xkbcommon-x11/libstdc++ `-L`); one-time environment setup.

**License:** **Apache-2.0** (`crates/ui/Cargo.toml` `license = "Apache-2.0"`, `LICENSE-APACHE`,
copyright Longbridge). Permissive, compatible with the port. gpui itself is also Apache-2.0.

**Maturity / maintenance:** Strong. 60+ components, published on crates.io (`gpui-component v0.5.2`) with
docs.rs, active default-branch development (HEAD dated 2026-05-31), CI, a full story/gallery app, and it
powers a shipping commercial desktop app (Longbridge Pro). Virtualized Table/List, full dock layout, and
a 200K-line-capable LSP code editor are real, exercised features. It tracks Zed's gpui closely (uses the
same `gpui_platform` split we already validated), so it stays current with the framework we're building
on.

---

## STRUCTURED SUMMARY

- **Usable:** **YES.** Use for standard chrome; keep the bespoke editor surface as a custom raw-gpui Element.
- **Recommended dependency lines (mutually compatible, build-verified):**
  ```toml
  gpui          = { git = "https://github.com/zed-industries/zed" }
  gpui_platform = { git = "https://github.com/zed-industries/zed", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }
  gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "bd4174e4991eef4c0900cb6dfb7143e0889f5dd9" }
  ```
  (Declare gpui UNPINNED — same source form as gpui-component — and COMMIT `Cargo.lock`. Do not add a
  `rev=` to gpui while gpui-component stays unpinned, or you get two gpui copies.)
- **gpui revision to standardize on:** `09165c15dc5d1fea93604231eaf30ca4c25f1cd6` (`gpui 0.2.2`,
  `gpui_platform 0.1.0`) — the same commit the raw-gpui cookbook pinned/built. (gpui-component's own
  Cargo.lock says `4bee412…`, but as a git dep it re-resolves to default-branch HEAD `09165c1…`.)
- **Probe build:** **SUCCESS** (`/tmp/gpui_component_probe`, nightly cargo 1.97). Code/version issues: only
  the gpui-source-split (fixed via unpinned decl) + `ButtonVariants` import. The rest were missing Linux
  system dev libs: `fontconfig-devel`, `libxkbcommon-x11-devel`, and a discoverable `libstdc++.so`
  (`LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16`).
- **Mapping highlights:** TitleBar→titlebar; DockArea/DockItem/Panel→MDI doc tabs + dockable panels (with
  `dump`/`load` persistence); Tree→workspace tree; DataTable→scanner/process/profiler tables (virtualized,
  sortable, custom cells); List/Select/Combobox (built-in `perform_search`)→command palette + type/enum/
  source pickers; Dialog/AlertDialog→options/goto/messagebox; Popover/Tooltip/ContextMenu→popups/menus;
  Theme/ThemeRegistry (runtime switch, JSON)→Reclass ThemeManager.
- **Gaps gpui-component does NOT cover:** the structured-editor surface (custom Element — biggest), hex-
  toolbar/chip/size-bar custom painting, status-bar shimmer, Reclass's exact fuzzy tie-breaks (own scorer
  in `perform_search`), dock drop-zone overlay specifics, sentinel "+"/source-icon tab chrome, and SVG
  icon assets (ship your own). All additive on top of gpui-component — no hard blocker.
```
