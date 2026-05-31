# PORTING SPEC — App Shell (main window, start page, titlebar, docks)

**Subsystem key:** `app-shell`
**Drives:** the Rust implementation of the top-level GPUI window, custom titlebar,
VS2022-style start page, the document-tab / dockable-panel system, dock-drag overlay,
status bar, and the project lifecycle / window-level glue.

**Read alongside:** `_design/understand/app-shell.md` (behavioral map — the authoritative
behavior reference; this spec is the function-level translation of it),
`_design/ARCHITECTURE.md` (one-package layout, `ui` module under `src/ui/`),
`_design/crate_selection.md`, `_design/gpui_component_cookbook.md`,
`_design/gpui_cookbook.md`, `_oracle/RESULTS.md`.

**Source files ported here:** `src/main.cpp` (the `MainWindow`/`DarkApp`/`MenuBarStyle`
chrome + lifecycle portions only — logic subsystems live in their own specs),
`src/mainwindow.h`, `src/startpage.h`, `src/titlebar.cpp`/`.h`, `src/macos_titlebar.h`/`.mm`,
`src/dockoverlay.h`, `src/docksizereadout.h`, `src/dock_tab_buttons.h`, `src/tab_source_icon.h`.

> **SCOPE BOUNDARY.** This subsystem owns *chrome and lifecycle wiring*. The things it hosts —
> `RcxDocument`/`NodeTree` (`core`), `RcxController` (`controller`), the structured-editor
> `RcxEditor` surface (`editor-surface`), workspace tree model (`widgets-dialogs`/workspace),
> scanner/symbols panels, dialogs/popups, themes, MCP — are *referenced* but specified in their
> own `PORTING_*.md`. Native/live providers are OUT OF SCOPE (Provider trait + file/buffer/
> snapshot/null only). Crash handlers, DWM/NSWindow hints, "Relaunch as Admin", and the Win
> self-attach buffer are `#[cfg]`-gated and behavior-optional.

---

## 0. Target crate / module layout

One package `reclass`, `ui` module (feature `ui`, default-on). New files under `src/ui/`:

```
src/ui/
├─ mod.rs                 # re-exports; `ui` feature gate
├─ app.rs                 # ← main.cpp main()/DarkApp: App::run, CLI, lifecycle wiring, crash hook
├─ window.rs              # ← MainWindow: the root Render view (Workspace). Owns DocTabModel,
│                         #   docks, status bar, menus, theme application, window-level events.
├─ titlebar.rs            # ← titlebar.cpp/.h + macos_titlebar: TitleBar view + LayoutPreset
├─ start_page.rs          # ← startpage.h: StartPage overlay view (custom paint)
├─ docks/
│  ├─ mod.rs              # DockLayout model (replaces QMainWindow dock areas + corners)
│  ├─ doc_tabs.rs         # ← createTab/reconcileDockTabBars/setupDockTabBars/sentinel: DocTabStrip
│  ├─ tab_source_icon.rs  # ← tab_source_icon.h: draw_tab_source_icon() + DocTabChrome model
│  ├─ overlay.rs          # ← dockoverlay.h: DockOverlay element + DropZone + DockDragDetector
│  ├─ size_readout.rs     # ← docksizereadout.h: DockSizeReadout element
│  └─ sidebar.rs          # ← createWorkspaceDock/Scanner/Symbols/Bookmarks chrome + placeSidebarDock
├─ status_bar.rs          # ← FlatStatusBar + ShimmerLabel
├─ widgets.rs             # ← BorderOverlay, ResizeGrip, DockGripWidget, DockTitleBar, ViewTabButton
├─ menus.rs               # ← createMenus(): MenuSpec registry + Command Palette source
└─ theme_apply.rs         # ← applyGlobalTheme / MenuBarStyle constants → gpui-component ThemeRegistry
                          #   (specified in PORTING_themes.md; this subsystem only consumes it)
```

`src/main.rs` (`[[bin]] reclass`) is a thin `fn main()` that calls `reclass::ui::app::run()`.

**UI tech mapping (per cookbooks):**
- Root window + chrome → `gpui_platform::application().run` + `cx.open_window` + `Root::new`
  (`gpui_component`). Frameless via `gpui_component::window_border::window_border()` +
  `WindowOptions { window_decorations: Client, .. }`.
- Titlebar → `gpui_component::TitleBar` *as a base*, but Reclass paints its own row (app label,
  menu/menu-buttons, layout-toggle pair, min/max/close) — implement as a custom `Render` row
  inside the titlebar slot. macOS native branch behind `#[cfg(target_os="macos")]`.
- Doc tabs + dockable panels → a **bespoke `DockLayout`** modeled on Zed's
  `crates/workspace` (do NOT depend on the crate). gpui-component's `DockArea`/`DockItem`/
  `Panel` is the fallback, but Reclass's exact behaviors (sentinel "+" tab, source-icon tab
  chrome, 37px tabs, edge/center drop zones) need bespoke painting, so the cookbook's
  custom-pane recipe (`gpui_cookbook.md` §7) is the primary path; gpui-component panels host
  the *sidebar contents*.
- Custom-painted bits (start page, status-bar shimmer, drop-zone overlay, size readout, tab
  source icon, border overlay, grips) → raw-gpui custom `Element`s / `canvas` painting
  (`gpui_cookbook.md` §3.4–3.5, §2.3).
- Persisted prefs (`QSettings("Reclass","Reclass")`) → a `Settings` struct serialized to a
  JSON config file under `directories::ProjectDirs::from("", "Reclass", "Reclass")`
  (`config_dir()/reclass.json`). **Preserve the exact key names** so behavior matches (e.g.
  `recentFiles`, `font`, `layoutPreset`, `menuBarTitleCase`, `showIcon`, `codeFormat`,
  `codeScope`, `ui/dock.<name>.size`, plus the many per-doc view flags).

---

## 1. Application entry / lifecycle — `main.cpp` `main()` + `DarkApp` → `ui/app.rs`

### 1.1 CLI args → `clap`
```rust
#[derive(clap::Parser)]
struct Cli {
    #[arg(long)] profile: bool,
    /// --screenshot <path> [scanner]
    #[arg(long, num_args = 1..=2)] screenshot: Option<Vec<String>>,
    /// positional .rcx files to open
    files: Vec<PathBuf>,
}
```
The C++ parses `--profile`, `--screenshot <path> [scanner]` from `argv`. The `--profile` flag
gates the profiler; `--screenshot` is a headless capture path. Both are parsed; the screenshot
path may be stubbed (`todo!("--screenshot capture")`) initially since it needs `window.grab()`
which has no clean GPUI analogue — log + exit. **Not behavior-critical to oracle parity.**

### 1.2 `pub fn run() -> anyhow::Result<()>` — order of operations (faithful)
Mirror `main.cpp:9475-9624`:
1. Install panic hook (replaces crash handlers). `std::panic::set_hook` → log via `tracing`,
   optional `backtrace`; behind no special cfg (cross-platform). Win minidump and POSIX
   `$HOME/.reclass/crash_<ts>.log` are **optional**, `#[cfg]`-gated, non-parity.
2. `#[cfg(target_os="macos")]` set "don't use native dialogs" equivalent (no-op in GPUI).
3. `gpui_platform::application().run(|cx| { … })`. Inside:
   a. `gpui_component::init(cx)` (REQUIRED before any component — cookbook §1).
   b. Load app settings (`Settings::load()`); set theme registry from saved theme name
      (PORTING_themes). Apply saved `font` (default `"IBM Plex Mono"`) as the editor global
      font name (`editor` subsystem hook `RcxEditor::set_global_font_name`).
   c. Register embedded monospace fonts (`assets/fonts/JetBrainsMono.ttf`,
      `IBMPlexMono.ttf`) via gpui asset/font registration.
   d. `cx.open_window(window_options(), |window, cx| cx.new(|cx| Root::new(MainWindow::new(window, cx).into(), window, cx)))`.
   e. `cx.activate(true)`.
   f. **Deferred startup** (the C++ `QTimer::singleShot(0, …)` after first paint): schedule via
      `window.on_next_frame` / `cx.spawn` — `TypeSelectorPopup::preload`, `load_plugins_deferred`
      (no-op stub in-scope), `ensure_scanner_panel`, `create_symbols_dock`. **Deferral is a perf
      optimization, not behavior** — the work must run; the timing may be eager.
   g. Normal path: post `MainWindow::show_start_page` (queued). Screenshot path skips it.

### 1.3 `DarkApp::notify` (main.cpp:359-375)
Windows-only one-shot DWM immersive-dark-mode hint per top-level window. In GPUI windows are dark
by default. Port as a no-op except a `#[cfg(windows)]` `set_dark_titlebar(hwnd)` DWM call invoked
once per window creation. **Non-parity.**

### 1.4 `window_options()` helper
```rust
fn window_options(cx) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1080.), px(720.)), cx))),
        titlebar: None,                     // we paint our own (frameless)
        #[cfg(not(target_os="macos"))]
        window_decorations: Some(WindowDecorations::Client), // frameless + client-side chrome
        #[cfg(target_os="macos")]
        window_decorations: Some(WindowDecorations::Server), // keep native traffic lights
        focus: true, ..Default::default()
    }
}
```
- C++ initial size **1080×720**; on **Linux** also `showMaximized()` at startup → after open,
  `#[cfg(target_os="linux")] window.zoom_window()` (or maximize equivalent).
- `#[cfg(windows)]` `DwmExtendFrameIntoClientArea {0,0,0,0}` (disable DWM shadow on frameless) —
  best-effort behind cfg.

---

## 2. `TitleBarWidget` → `ui/titlebar.rs`

### 2.1 `enum LayoutPreset` (titlebar.h:15-18)
```rust
#[derive(Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum LayoutPreset { NoWorkspace = 0, Workspace = 1 }  // value-stable; persisted as int
```

### 2.2 `TitleBar` view
A GPUI view `struct TitleBar { theme, title_case: bool, show_icon: bool, maximized: bool, use_tool_buttons: bool, menu: MenuModel, … }`. Rendered as a fixed-height row inside the
window-decoration titlebar region (so OS chrome dragging works).

| C++ member / method | Rust counterpart | Behavior notes |
|---|---|---|
| ctor: `setFixedHeight(32)`, app label + menu bar + stretch + toggle pair + min/max/close | `render()` returns `h(px(32.))` flex row (34 when `show_icon`) | App label "Reclass" bold 12px on left, transparent-for-mouse. **Constants are load-bearing.** |
| `m_appLabel` `setShowIcon` swaps to 24×24 `:/icons/class.png`, height→34 | `show_icon` field toggles label vs `svg("icons/class.svg")` 24px, row height 32↔34 | `setShowIcon(bool)` setter; persisted under `showIcon` (default false). |
| `m_menuBar` (`setNativeMenuBar(false)`) | menu region | **Win/macOS-non:** render menu bar inline (gpui-component `AppMenuBar` or custom `PopupMenu` row). **macOS:** use native `cx.set_menus(...)` and render *nothing* in the row. |
| Linux `m_useToolButtons` + `finalizeMenuBar()` mirrors top menus as `QToolButton`s | `#[cfg(target_os="linux")] use_tool_buttons=true` → render each top-level menu as a `Button`/`DropdownMenu` (InstantPopup) in the row | Rationale (a `QMenuBar` collapses inside a custom widget on Linux) does not apply to GPUI, but **render the same set of top-level menu buttons** to match the visual. The `eventFilter` hover-to-switch between open menus (titlebar.cpp:279-299) maps to gpui-component menu hover behavior — acceptable to drop the exact 0-timer reopen if hover-switch works natively. |
| Workspace toggle pair `m_btnLayoutOff`/`m_btnLayoutOn` (34×32, exclusive group, ids 0/1, off default-checked) | two `ToggleButton`s in an exclusive pair; `selected = current_preset`; icons `vsicons/layout-sidebar-left-off.svg` / `layout-sidebar-left.svg` | `on_click` → emit `LayoutPresetSelected(preset)` to the window. Tooltips "Editor only" / "Editor + workspace". |
| Chrome buttons `m_btnMin`/`m_btnMax`/`m_btnClose` (46×32, autoraise, no-focus) | three icon `Button`s, 46×32 | Min → `window.minimize_window()`; Max → `toggle_maximize`; Close → `window.remove_window()` / quit. Icons `chrome-minimize/maximize/close.svg`. |
| `applyTheme(const Theme&)` (titlebar.cpp:117-205) | `set_theme(&Theme)` updates stored colors; render reads them | Background = `theme.background`; app label color `theme.text`; min/max hover = `theme.hover`; toggle `:checked` → `backgroundAlt` bg + 2px bottom border `indHoverSpan`; **close hover = `theme.markerPtr` (valid) else `indHeatHot`** (warning-red; the bleed regression — see TEST PLAN T5). Menu palette roles consumed directly by render. |
| `setMenuBarTitleCase(bool)` (titlebar.cpp:222-253) | `set_menu_title_case(bool)` transforms cached top-level menu titles | `true` → `to_uppercase()` of `&`-stripped title; `false` → Title Case (capitalize first letter of each whitespace-separated word). **Port the exact char loop** (letters → upper-if-`capitalize_next` else lower; whitespace sets `capitalize_next`). Persisted `menuBarTitleCase` (default false). Syncs Linux tool-button labels. |
| `menuBarTitleCase()` getter | `menu_title_case() -> bool` | |
| `updateMaximizeIcon()` | `set_maximized(bool)` → max icon swaps `chrome-restore.svg` ⇆ `chrome-maximize.svg` | Driven from window state-change. |
| `setWorkspaceChecked(bool)` (titlebar.cpp:106-115) **with signals blocked** | `set_workspace_checked(bool)` sets `current_preset` WITHOUT emitting `LayoutPresetSelected` | Critical: avoids the toggle→applyLayoutPreset→visibilityChanged→setWorkspaceChecked loop. In Rust, the setter just mutates state + `cx.notify()`; only `on_click` emits the event. |
| `mousePressEvent` LMB → `startSystemMove()` | the titlebar row is inside the OS-managed decoration region; GPUI handles drag. For the inner custom area, mark it a drag region (`window` client-decoration drag) | Platform-bridged; `#[cfg]` as needed. |
| `mouseDoubleClickEvent` LMB → `toggleMaximize()` | `.on_click` with `click_count==2` on the row background → `toggle_maximize` | |
| `paintEvent` 1px bottom border `theme.border` | render adds `.border_b_1().border_color(theme.border)` | |
| `toggleMaximize()` | `toggle_maximize(window)` → `window.zoom_window()` / restore; then `set_maximized` | |

**macOS titlebar** (`macos_titlebar.h/.mm`, `applyMacTitleBarTheme`): behind
`#[cfg(target_os="macos")]`. Compute luminance `0.2126R+0.7152G+0.0722B` of `theme.background`,
choose Aqua/DarkAqua appearance, set `titlebarAppearsTransparent`, `backgroundColor`. Implement
as a `#[cfg(target_os="macos")]` fn taking the NSWindow handle; **no-op + compiles** elsewhere.
Not Linux-verifiable.

---

## 3. Document-tab / dock system — `ui/docks/`

This is the heart of the subsystem. The C++ abuses `QDockWidget` tabification + a sentinel-dock
trick. **GPUI requires no sentinel** — render the tab strip directly. We model the layout as data
and replicate the *behaviors*, per app-shell.md §8.

### 3.1 Data model — `ui/docks/mod.rs`

```rust
use slotmap::{SlotMap, DefaultKey};
pub type DocId = DefaultKey;     // stable id replacing QDockWidget* keys

pub struct DocTab {
    pub doc: Entity<RcxDocument>,        // core (may be shared by >1 tab)
    pub ctrl: Entity<RcxController>,     // controller
    pub panes: Vec<SplitPane>,           // QSplitter + QVector<SplitPane>
    pub active_pane: usize,
    pub split_axis: Axis,                // Horizontal default; flips to Vertical on splitView
    pub title: SharedString,             // rootName(tree); recomputed on documentChanged
    pub source_icon: SharedString,       // rcxSourceIcon — provider icon path
    pub source_live: bool,               // rcxSourceLive — provider && provider.is_valid()
    pub source_tip: SharedString,        // per-tab tooltip
}

pub struct DocTabModel {
    tabs: SlotMap<DocId, DocTab>,
    order: Vec<DocId>,                   // m_docDocks — ordered tab strip
    active: Option<DocId>,               // m_activeDocDock
    all_docs: Vec<Entity<RcxDocument>>,  // m_allDocs (shared with controllers)
    closing_all: bool,                   // m_closingAll guard (ClosingGuard RAII)
}

#[derive(Copy,Clone)] pub enum ViewMode { Reclass, Rendered, Debug }  // VM_* (mainwindow.h:149)
```

`SplitPane` (mainwindow.h:189-216) — the 3-tab (Reclass/Code/Debug) editor split. The editor
widgets (`RcxEditor`, rendered/debug Scintilla→GPUI editors, minimap) and the render cache
(`last_rendered_*` keyed on tree-generation/rootId/fmt/scope/asserts) are **editor-surface**
concerns; here `SplitPane` is just a struct field of `DocTab`. Mirror the fields but defer their
content to that spec:
```rust
pub struct SplitPane {
    pub editor: Entity<RcxEditor>,
    pub view_mode: ViewMode,
    pub fmt: CodeFormat, pub scope: CodeScope,  // persisted codeFormat/codeScope
    // render cache (editor-surface): last_rendered_root_id, _text, _tree_gen, _fmt, _scope, _asserts
    // rendered/debug/minimap editors — editor-surface
}
```

`ReferenceHit` (mainwindow.h:138-144) and `InspectionResult` (mainwindow.h:166-174): port as plain
structs. `InspectionResult` (Ctrl+Shift+Click UI inspector) carries `theme_colors: serde_json::Value`
(array of `{key,value,label,group}`) and `properties: serde_json::Value` (object) — keep as
`serde_json::Value` for 1:1 JSON shape with the MCP tool that reuses it. UI inspection itself is a
debug affordance; port the data form, the overlay paint can be minimal.

### 3.2 `DockLayout` (replaces QMainWindow corners + dock areas)
The C++ uses Qt's dock-area machinery + corner assignment so Left/Right span full height and all
tab strips sit on top (North). GPUI has no dock widget; model a region tree directly:

```rust
pub enum DockSide { Left, Right, Bottom }   // (Top reserved for doc area)
pub struct DockLayout {
    // sidebar slots; each holds 0+ panels that tabify when sharing a side
    left: SidebarSlot, right: SidebarSlot, bottom: SidebarSlot,
    sizes: HashMap<String, f32>,  // ui/dock.<name>.size (px); workspace=width, scanner=height…
    center: DocArea,              // the document tab strip + active doc's pane group
}
```
Edge drops insert a panel into the chosen side; "Center" tabifies with a doc tab. The Qt corner
reassignment in `onDockDropRequested` is a `QMainWindow` quirk and is **dropped** — the explicit
region tree makes it unnecessary (app-shell.md §9 Rust note).

### 3.3 `createTab(RcxDocument*) -> DocId` (main.cpp:2798-3312)
The central factory. `fn create_tab(&mut self, doc: Entity<RcxDocument>, window, cx) -> DocId`.
Steps (faithful order):
1. New `RcxController(doc)` (controller subsystem). Split axis Horizontal.
2. Allocate `DocId`; build `DocTab { doc, ctrl, panes: vec![create_split_pane(...)], active_pane:0,
   title: root_name(tree), source_icon/_live/_tip from provider, .. }`.
3. **Float/dock title bars:** the C++ has an `emptyTitleBar` (docked) vs `floatTitleBar` (24px,
   grip + title + ✕, context menu Dock / Always-Floating / Close). In the GPUI region model docs
   are never literal floating windows during normal use, so the tab itself carries title+✕ chrome
   (see §3.5). A genuinely floated doc tab → render it in a separate `cx.open_window` *only if*
   we later support tear-off; **initial port keeps all docs in the center tab strip** (no float).
   Document this as a known simplification; the float context-menu actions map to layout ops.
4. Insert into `order`, set `active = Some(id)`, register doc in `all_docs` if new, call
   `ctrl.set_project_documents(&all_docs)` + `rebuild_all_docs()`.
5. Apply per-doc view settings from `Settings` (compactColumns, treeLines, braceWrap, typeHints,
   showComments, showRttiChips, showEnumChips) onto the controller/editor.
6. **Signal wiring** — in GPUI, "signals" become `cx.subscribe` on the controller/doc entities, or
   direct method calls. Port each (app-shell.md §8 step 9) as observer closures on the window:
   - doc becomes visible/active → `active = id`, `update_window_title()`, `sync_view_buttons`,
     `refresh_bookmarks_dock()`, re-raise border.
   - `doc.document_changed` → if active refresh bookmarks; `refresh_doc_tab_source_icon(id)`;
     debounced (next-frame) rebuild of rendered/debug panes + window title + workspace + symbols
     (undo path regenerates only *visible* panes); `mcp.notify_tree_changed()` (out of scope/stub).
   - `ctrl.source_liveness_changed` → `refresh_doc_tab_source_icon` + (if active) `update_scanner_title`.
   - `ctrl.node_selected(idx)` → build status string (`Struct.field`, `+0xNN`, ←→ variant hints,
     struct size) → `set_app_status`; update rendered/debug panes.
   - `ctrl.selection_changed(n)` → status "N nodes selected".
   - `ctrl.status_hint(text)` → `set_app_status(text)`.
   - `ctrl.context_menu_about_to_show` → add "Copy as C Struct".
   - `ctrl.request_open_struct_in_new_tab(struct_id)` → reuse a tab already viewing it, else
     `create_tab(same doc)` + `set_view_root_id`.
   - `ctrl.request_open_provider_tab(...)` → plugin path is out of scope; the file/in-memory
     equivalent attaches via the benign Provider.
   - **`doc.destroyed`/tab-close** → see `close` handling in §3.6.
7. **Auto-focus root:** prefer `tree.initial_class` (match `structTypeName` or `name`), else first
   root struct → `ctrl.set_view_root_id`.
8. `ctrl.refresh()`, `rebuild_workspace_model()`, raise/select the new tab,
   `reconcile_doc_tab_strip()`.

### 3.4 Sentinel "+" tab → direct render (app-shell.md §8 Rust note)
- C++ `createSentinelDock()` (main.cpp:2785-2796): invisible dock, windowTitle = **`​`**
  (zero-width space), to keep the tab strip alive for a lone doc + a "+" affordance.
- **Rust:** NO sentinel. `DocTabStrip` always renders a trailing **"+" tab element**. Click →
  `project_new()`. Middle-click a real tab → close it. The `​` magic string, the
  `reconcileDockTabBars`/`setupDockTabBars`/`m_sentinelDocks` machinery, and `m_reconciling`
  re-entry guard are **all elided** — replaced by deterministic stateless rendering of
  `order + ["+"]`.
- `reconcile_doc_tab_strip()` becomes a no-op-equivalent: just `cx.notify()`. Keep the function
  name as a thin shim so the call sites read 1:1 against the C++.

### 3.5 Tab chrome — `ui/docks/tab_source_icon.rs` + `DocTabStrip` render
Render each tab as a fixed **37px-tall** cell (the `MenuBarStyle::CT_TabBarTab` constant —
load-bearing for `test_tab_source_icon`'s baseline test), with:
- **2px top accent strip** (`theme.indHoverSpan`/Link) when selected; **1px bottom border**
  (`theme.border`/Dark) always.
- **Left source icon** at `kIconPad=8`, size `kIconSz = font_metrics.height()`, gap `kIconGap=6`,
  vertically centered in the content area (top inset 2 if selected, bottom inset 1).
  Opacity: selected = 1.0, unselected = 0.70; `live=false` multiplies by ×0.40 (in the helper).
- **Label** middle/right-elided (`ElideRight`) reserving right close-button width (`24`) + left
  icon inset.
- **Right close ✕** (`vsicons/close.svg` 12×12 in a 16×16 button); hover bg = `theme.selected`
  (NOT `hover`, so it's visible over a hovered tab — `DockTabButtons::applyTheme`).
- Hovered/selected fill `theme.hover`/Mid, else `theme.background`/Window.

**`draw_tab_source_icon` — port of `tab_source_icon.h` (the test oracle helper).** This is the
exact function `test_tab_source_icon` grabs pixels from, so it must be a standalone, testable fn:
```rust
/// Render an SVG into `icon_rect`, tinted to `tint` (SourceIn), dimmed ×0.40 when !live.
pub fn draw_tab_source_icon(
    window: &mut Window, icon_rect: Bounds<Pixels>, icon_path: &str, live: bool, tint: Hsla,
) { /* paint a tinted SVG quad; multiply paint opacity by 0.40 if !live */ }
```
Implementation: paint the SVG (gpui `svg().path(icon_path).text_color(tint)` masked, OR a
`canvas` that rasterizes + tints). The C++ `CompositionMode_SourceIn` tint == "fill the icon's
alpha mask with `tint`": in GPUI, `svg(...).text_color(tint)` already tints a single-color SVG;
for multicolor SVGs paint into an offscreen and composite. The "**DPR set before painter
attach**" comment is a Qt-specific footgun — N/A in GPUI (text system handles scale), but keep a
doc-comment noting the parity intent (crisp at any scale). The `live=false → ×0.40` opacity is
behaviorally observable (T3) — implement exactly.

`refreshDocTabSourceIcon(dock)` (main.cpp:3568-3607) → `fn refresh_doc_tab_source_icon(&mut self,
id: DocId)`: resolve `icon_for_provider(kind)` (from sourcechooserpopup mapping) + liveness
(`provider.is_some() && provider.is_valid()`); fall back to `vsicons/plug.svg` "No source". Store
on the `DocTab` struct (replaces "properties on the dock object" — naturally survives reorder).
Set the tooltip.

### 3.6 Tab close / "never blank window" (the parity-critical behavior)
Port `dock.destroyed` handler (app-shell.md §8 step 9) into `fn close_tab(&mut self, id, cx)`:
1. Remove tab from `order`; drop from `tabs`. Drop the doc from `all_docs` **only if no other tab
   references the same `Entity<RcxDocument>`** (Rust `Arc`/`Entity` refcount handles the
   deleteLater nuance — just check no other `DocTab.doc` == this doc).
2. `reassign active` = last in `order`. `rebuild_all_docs()` + `rebuild_workspace_model()` +
   `update_window_title()`.
3. **If `order` is empty AND `!closing_all`: `project_new()`** — never leave a blank window
   (T-behavior, app-shell.md §20). The `ClosingGuard` (RAII setting `closing_all`) → a Rust
   guard struct or a `closing_all = true; …; closing_all = false` scope.

`close_all_doc_docks()` → copy `order` first (closing mutates it), close each under `closing_all`.

### 3.7 Active-tab accessors (main.cpp:5346-5364)
`active_controller()`, `active_tab()`, `tab_by_index(i)`, `tab_count()`,
`find_active_split_pane()`, `active_pane_editor()`, `find_pane_by_tabwidget(tw)` → trivial
lookups over `active`/`order`/`tabs`. `split_view()` flips `split_axis` to Vertical + appends a
pane; `unsplit_view()` drops the last pane (main.cpp:4453-4464).

---

## 4. Dock-drag overlay — `ui/docks/overlay.rs` (`dockoverlay.h`)

### 4.1 `enum DropZone` (dockoverlay.h:22-28) — value-stable
```rust
#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum DropZone {
    None=0, Left, Right, Top, Bottom,   // retained for value stability; NEVER produced/drawn
    Center,                              // tabify with hovered doc
    Float,
    EdgeLeft, EdgeRight, EdgeTop, EdgeBottom,
}
```
Only `Center`, the four `Edge*`, and `Float` are live.

### 4.2 `DockOverlay` — a full-window overlay `Element`/view shown during a drag
Constants (load-bearing): `K_EDGE_W=36`, `K_TARGET_SZ=28`, `K_TARGET_DIST=52`, `K_HIT_R=20`.

| C++ | Rust | Notes |
|---|---|---|
| ctor (grab mouse/keyboard, ClosedHand cursor, hidden) | overlay view with `open: bool`; rendered as `deferred(div().absolute().inset_0())` over the window when dragging | gpui has no mouse-grab; the overlay covers the window and captures mouse while open. |
| `setTheme(Theme)` (accent defaults `borderFocused`) / `setAccentColor` / `setAccentColorOverride` | `set_theme`, `set_accent`, accent default `theme.border_focused` unless overridden | |
| `beginDrag(dock, title)` | `begin_drag(src: DocId/SidebarId, title)` → `open=true`, store source+title, `cx.notify()` | |
| `endDrag()` | `end_drag()` → `open=false` | |
| `activeZone`/`draggedDock`/`hoveredDock` getters | fields | |
| signals `dropRequested(source,target,zone)`, `dragCancelled(source)` | results returned to the window via a callback/`cx.emit` | |
| `mouseMoveEvent` → `updateDropTarget(pos)` + repaint | `.on_mouse_move` → `update_drop_target(pos)` + `cx.notify()` | |
| `mouseReleaseEvent` → snapshot zone/target, end, emit drop or cancel | `.on_mouse_up` | |
| `keyPressEvent(Escape)` → end + cancel | `.on_key_down` Escape | |
| `paintEvent` (edge strip, center diamond, preview rect, cursor label) | `render` paints these via `canvas`/quads | preserve visuals. |

**`content_rect()`** (dockoverlay.h:177-190): the dockable region excluding chrome — top below
the titlebar/menu bar, bottom above the status bar. In the GPUI region model this is the center
+ sidebars area bounds (exclude titlebar row + status bar). Used for BOTH drawing and hit-testing
(must stay in lockstep).

**`is_tabbable_target(id)`**: only document tabs are valid Center targets (never sidebars).

**`update_drop_target(pos)`** (dockoverlay.h:239-275) — pseudocode (port verbatim):
```
wr = content_rect()
if !wr.contains(pos): hovered=None; zone=None; return
if pos.x < wr.left + 36   && zone_allowed(EdgeLeft):   zone=EdgeLeft;   hovered=None; return
if pos.x > wr.right - 36   && zone_allowed(EdgeRight):  zone=EdgeRight;  hovered=None; return
if pos.y < wr.top + 36     && zone_allowed(EdgeTop):    zone=EdgeTop;    hovered=None; return
if pos.y > wr.bottom - 36   && zone_allowed(EdgeBottom): zone=EdgeBottom; hovered=None; return
hovered = find_dock_at(pos)            // a tabbable DOC tab under the cursor, else None
if hovered.is_none(): zone=Float; return
// inside a doc body → always Center (the K_HIT_R diamond just inverts the icon; whole face = Center)
zone = Center
```
**`zone_allowed(z)`**: edge zones require the dragged dock's `allowed_areas()` to include that
side; Center/Float always allowed. (Sidebars have restricted allowed areas, e.g. scanner =
Bottom|Left|Top.)

Drawing helpers (`drawEdgeZones`, `drawDiamondTargets`, `drawPreviewRect`/`computePreviewRect`,
`drawCursorLabel`) → port the geometry exactly: edge strips = translucent accent α60 + 3px solid
stripe (only when active); center diamond = rounded rect with two overlapping squares (inverts
when active, `K_TARGET_SZ`); preview rect = ¼ window for an edge / whole hovered dock for Center,
2px theme-text outline + accent α60 fill + centered label "Tabify"/"Dock Left"/…; cursor label =
drag title near cursor, clamped on-screen, truncated >30 chars (`left(28)+…`).

### 4.3 `DockDragDetector` (dockoverlay.h:435-496)
Drag-initiation detection. In GPUI this is `on_mouse_down` (record press pos+tab) + `on_mouse_move`
on the tab strip; once moved > **14px** (manhattan) and the pressed tab isn't the "+" affordance,
`begin_drag(dock, global_pos)`. The C++ `findDockByTitle` matching is replaced by carrying the
`DocId` directly (no title lookup needed).

### 4.4 MainWindow drag glue (main.cpp:3317-3481)
- `setup_dock_overlay()` — create overlay; wire callbacks.
- `on_dock_drag_started(id, global_pos)` — set accent; remember `drag_orig_side` and
  `drag_orig_peer` (first peer); `begin_drag`. (The C++ `setFloating(true)+hide()` detach is a Qt
  hack; in the region model just mark the source as "being dragged" and exclude it from layout.)
- `on_dock_drop_requested(source, target, zone)` — port the branch logic:
  - `Float` → tear-off (initial port: keep in center strip / no-op + log; full float later).
  - `Center` → tabify with target if it's a doc tab, else active/first doc, else add to center.
  - `EdgeLeft/Right/Top/Bottom` → insert into that `DockSide`; resize to ¼ window (workspace uses
    `compute_workspace_dock_width()`, others `load_dock_size`).
  - `dragCancelled` → restore to `drag_orig_peer`/`drag_orig_side` (or stay where it was).
  - Always `reconcile_doc_tab_strip()` (= notify) at the end.

---

## 5. Dock size readout — `ui/docks/size_readout.rs` (`docksizereadout.h`)

`DockSizeReadout` — inline tooltip shown while dragging a sidebar divider. A child element of the
window (NOT a top-level tooltip — the Qt rationale about WS_EX_LAYERED/mouse-grab is Qt-specific;
in GPUI it's a `deferred(anchored())` element, which has neither problem). Constants
`K_RADIUS=6`, `K_PAD=10`, `K_GAP=4`.

| C++ | Rust |
|---|---|
| `setTheme(bg,border,title,body,sep)` | `set_theme(...)` colors |
| `updateText(title, body, font)` (no dedup — body changes per pixel) | `set_text(title, body)`; recompute size each call |
| `showAt(parentLocalPos)` (+12,+12, clamped inside parent, raise+repaint each tick) | `show_at(pos)` → offset +(12,12), clamp inside window bounds, `cx.notify()` |
| `dismiss()` | `dismiss()` → hide |
| `paintEvent` rounded rect + bold title + sep line + body | `render` via `canvas` |

Driven (in the C++) from `MainWindow::eventFilter` on dock `QEvent::Resize` **while LMB held**.
In GPUI the divider is our own `div().id("divider").on_drag_move(...)` (gpui_cookbook §7), so the
readout is driven directly inside the drag-move handler — far simpler than the Qt event-filter
gymnastics. `showDockSizeTipForAxis(dock, sz, horizontal)` (main.cpp:3998+) builds the body
`"selfSz px | otherSz px"` where `otherSz` is the first doc dock / central widget extent on the
same axis; title = "Workspace/Bookmarks/Symbols/Scanner/Dock size". Port the body-string format
exactly (it's asserted in T-dock-size full-pipeline). The "deferred 0-timer to settle geometry"
is replaced by reading the post-layout bounds in the drag-move handler.

---

## 6. Sidebar docks — `ui/docks/sidebar.rs`

Each sidebar is a gpui-component `Panel` (or a plain view) hosted in a `DockSide`. Shared chrome:
a `DockTitleBar` (paint-only header: grip + title + close ✕) + per-dock `BorderOverlay`+`ResizeGrip`
shown only while floating. Contents are other subsystems.

| C++ fn | Rust | Behavior |
|---|---|---|
| `createWorkspaceDock()` (main.cpp:6974+) | `create_workspace_dock()` | "Project" panel, name `WorkspaceDock`, 36px header. Contents = search `TextInput` + `Tree` (workspace subsystem). All areas; default visible per `layoutPreset`. |
| `createScannerDock()` (main.cpp:7640-7770) | `create_scanner_dock()` | "Memory Scanner" `ScannerDock`, allowed Bottom|Left|Top, 24px header (`scannerHeader`, elide-on-overflow title). Placeholder until `ensure_scanner_panel`. Hidden at start; **defaults to floating** 720×700 centered. |
| `ensureScannerPanel()` (main.cpp:7784-7839) | `ensure_scanner_panel()` | Idempotent lazy build of `ScannerPanel` (scanner UI subsystem); wires provider getter (active tab's provider), bounds getter (struct base+span), `goToAddress` (rebase active tab). |
| `createSymbolsDock()` (main.cpp:7841+) | `create_symbols_dock()` | Idempotent; "Symbols" `SymbolsDock`, `UnifiedSymbolPanel` (rtti/symbols subsystem). Lazy. |
| `createBookmarksDock()` (main.cpp:8031+) | `create_bookmarks_dock()` | `BookmarksDock` = `List` + filter `TextInput`. Bookmarks logic = controller/core. |
| `placeSidebarDock(dock, side, preferred=-1)` (main.cpp:8639-8706) | `place_sidebar_dock(panel, side, preferred)` | If a visible peer already lives in `side` → **tabify** with it; else add + resize to remembered size (`load_dock_size`, fallback `preferred`; workspace fallback = `compute_workspace_dock_width()` capped to `max(280, 35% width)`). Guard `placing_sidebar` against recursion. End with `reconcile_doc_tab_strip()`. (The Qt 0-timer resize-defer is unnecessary in GPUI.) |
| `saveDockSize(dock)` (main.cpp:8708-8718) | `save_dock_size(name, axis_extent)` | Store width (horizontal sides) or height under `ui/dock.<name>.size`. |
| `loadDockSize(dock, fallback)` (main.cpp:8720-8725) | `load_dock_size(name, fallback) -> f32` | Read back. |
| `computeWorkspaceDockWidth()` (main.cpp:8610-8629) | `compute_workspace_dock_width() -> f32` | **Port the exact formula** (T-project-dock relies on width being 10–40% of window). Pseudocode below. |
| `applyLayoutPreset(int)` (main.cpp:6955-6969) | `apply_layout_preset(preset)` | `workspace.set_visible(preset==Workspace)`; reconcile; persist `layoutPreset`; status. Other docks untouched. |

`compute_workspace_dock_width` pseudocode (port verbatim):
```
max_chars = 12
for each open doc, for each root Struct node (parentId==0 && kind==Struct):
    name = structTypeName.is_empty() ? name : structTypeName
    max_chars = max(max_chars, name.len())
f = font(settings.font default "IBM Plex Mono", 10pt, fixed)
name_w = advance_width("W".repeat(max_chars))
total = font_height + 4 + name_w + 30 + 24
return clamp(total, 180, 420)
```

---

## 7. Status bar — `ui/status_bar.rs`

### 7.1 `FlatStatusBar` (main.cpp:1986-2052) → a status-bar row view
A borderless bottom row. `size_grip_enabled=false`. Height ≈ `round(max(tab_row_h, font_h+6) *
1.15)` (the `sizeHint` formula). Render: window-fill + 1px top hairline (device pixel) + optional
vertical divider. Children: `ShimmerLabel` (left), `progress_label`+`progress_bar` (hidden until a
long op), `ResizeGrip` (bottom-right, a direct child of the window).

### 7.2 `ShimmerLabel` (main.cpp:1869-1979) → a custom-painted label `Element`
| C++ | Rust |
|---|---|
| `setText(t)` / `setText(t, dimSuffix)` | `set_text(t)` / `set_text_dim(t, dim_suffix)` |
| `setShimmerActive(bool)` (30ms timer sweeps a glow band; phase += 0.012, wrap at 1.0) | `set_shimmer(bool)`; when active, a 30ms repeating task advances `phase` and `cx.notify()` |
| `onClicked` `std::function` (opens Goto Address) | `on_click` callback → `show_goto_address_dialog()` |
| colors `colBase/colDim/colBright/colSep` | fields, defaulted from theme |
| paint: normal text; if dimSuffix → main text + 1px vertical separator (gap = space advance) + dim suffix; shimmer → 20%-width glow band (`width*0.20`, `bandCenter = -bandW + (width+2*bandW)*phase`, linear gradient transparent→glow(α35)→transparent) + bright text | `render` via `canvas` + `shape_line` (cookbook §3.2). **The dim-suffix-with-vertical-separator layout is parity-relevant** (status string format). |

### 7.3 Status APIs (mainwindow.h:99-110, main.cpp:2132-2204)
- `set_app_status(text)` / `set_app_status_dim(text, dim_suffix)`: store + show unless MCP busy.
- `set_mcp_status(text)`: cancel pending clear, shimmer on.
- `clear_mcp_status()`: delayed **750ms** restore of app status + shimmer off (via `cx.spawn` +
  `Timer::after`).
- `begin_progress(label, total=0)` / `update_progress(value, label)` / `end_progress()`:
  right-anchored label (220px) + bar (160px); `total==0` ⇒ indeterminate. Use gpui-component
  `Progress` or a custom bar.

---

## 8. Small helper widgets — `ui/widgets.rs`

| C++ class (main.cpp) | Rust | Notes |
|---|---|---|
| `BorderOverlay` (829-845) | `border_overlay(color)` element: 4× 1px edge quads | Color = `border_focused` (active) / `border` (inactive). Transparent for mouse. |
| `ResizeGrip` (1699-1747) | `ResizeGrip` element: 16×16, 6-dot VS2022 triangle, `SizeFDiag` cursor | LMB → `window.start_window_resize(BottomRight)` (`#[cfg]` / GPUI resize edge). `kSize=16,kPad=4`; dot math: r=1.0, s=4.0, inset=4 (3+2+1 dots). |
| `DockGripWidget` (1750-1777) | `dock_grip()` element: 12px wide, 2×4 dot grid, `SizeAll` cursor | r=0.75, s=3.0; offsets `[-1.5,-0.5,0.5,1.5]`. |
| `DockTitleBar` (1782-1805) | `dock_title_bar(height, bg, border_right)` | paint-only header: bg fill + optional 1px right border. |
| `ViewTabButton` (1808-1866) | `ViewTabButton` (checkable flat button, top accent line when checked) | Historical status-bar view toggle; now per-pane tabs — implement only if referenced. |
| `InspectionOverlay` (847-885) | minimal red-rect+label overlay | Ctrl+Shift+Click UI inspector; debug affordance. |
| `DockBorderFilter` (2726-2749) | folded into the per-dock border element's state | Repositions/recolors a floating dock's border+grip; in GPUI the border element reads focus state directly. |
| `MinimapScintilla`/`MinimapViewportIndicator` (2218-2272) | editor-surface | OUT of this spec. |

---

## 9. Start page — `ui/start_page.rs` (`startpage.h`)

`StartPage` — a full-window custom-painted overlay view (NOT a modal dialog;
gpui_cookbook §8 modal-overlay pattern: `deferred(div().absolute().inset_0())` with high
priority). The only interactive child is the search `TextInput` ("Search recent…", max width 330,
height 30). Everything else painted in `render` via `canvas`.

**Layout constants** (single source for paint + hit-test, startpage.h:185-196) — port exactly:
`K_LEFT_MARGIN=48, K_TOP_MARGIN=36, K_RIGHT_MARGIN=32, K_PANEL_GAP=40, K_CARD_PANEL_W=340,
K_CARD_H=84, K_ENTRY_H=28, K_GROUP_HEADER_H=28, K_GROUP_SPACING=15, K_BOTTOM_PAD=24,
K_SEARCH_BAR_H=30, K_SEARCH_GAP=16`.

**Data:**
```rust
struct Entry { path: PathBuf, file_name: String, dir_path: String,
               last_modified: SystemTime, is_example: bool }
struct Group { name: String, expanded: bool, entries: Vec<usize> }   // indices into filtered
enum HitZone { None, Entry(usize), Group(usize), Card(usize), Continue }
```

**`load_entries()`** (startpage.h:217-234): read `settings.recent_files` (a `Vec<String>`); skip
missing files; append `*.rcx` from `<exe_dir>/examples` (mac: `../Resources/examples`) marked
`is_example`. Examples dir resolution behind `#[cfg(target_os="macos")]`.

**`build_groups()`** (startpage.h:236-261): filter by lowercased search substring (file_name OR
dir_path). Bucket non-examples by days-since-modified:
```
d = days_between(last_modified.date(), today)
d==0 → "Today"; d==1 → "Yesterday"; d<7 → "This week";
else if same month&year as today → "This month"; else → "Older"
examples → "Examples"
```
Build groups only for non-empty buckets, in fixed order `[Today, Yesterday, This week,
This month, Older, Examples]`. Reset scroll.

**Drawing:** `draw_cards` — 5 cards (New Class / Open project / Import from Source /
Import ReClass XML / Import PDB), each 84px (icon 32px + title 15px + desc 12px), hover fill
`theme.hover` + 3px accent left bar `theme.indHoverSpan`; centered "Tutorial →" link below (the
`HZ_Continue` zone — note the C++ signal is `continueClicked`). `draw_file_list` — collapsible
groups with triangle markers; each entry = icon + filename (full) + dim "  ·  " separator +
middle-elided dir path + right-anchored date `"M/d/yyyy h:mm AP"`; clipped + scrollable (wheel,
clamped to `max_scroll`). 1px border around the page.

**Hit testing** mirrors `hit_test`: cards first, then continue, then (within list y-range) groups
then entries. Hover → PointingHand cursor. Click dispatch:
- entry → `FileSelected(path)`; group → toggle expand; card 0..4 →
  `NewClass`/`OpenProject`/`ImportSource`/`ImportXml`/`ImportPdb`; continue → `ContinueClicked`.

**Dismiss:** Escape → `Dismissed`; click outside the page rect → `Dismissed` (the qApp event
filter becomes an `on_mouse_down_out` on the overlay panel). Emitted as `gpui` events the window
subscribes to.

### MainWindow integration
- **`show_start_page()`** (main.cpp:9383-9461): if already shown, no-op. **If no tabs exist,
  preload a `new_class()` behind the splash** (parity behavior, app-shell.md §20) so dismissing
  lands on something. Create the overlay, apply theme, size ~`clamp(900, 90%w, w-20) ×
  clamp(560, 85%h, h-20)`, center, show. Wire signals:
  - `NewClass` → dismiss (+`new_class()` if not preloaded).
  - `Dismissed` → dismiss (+`new_class()` if no tabs).
  - `OpenProject` → dismiss + `open_file()`.
  - `ImportSource`/`ImportXml`/`ImportPdb` → dismiss + import.
  - `ContinueClicked` → dismiss + close-all + `self_test()`.
  - `FileSelected(path)` → dismiss + `project_open(path)`.
- **`dismiss_start_page()`** (main.cpp:9463-9469): null `m_startPage` first (close may re-enter),
  then destroy. In Rust: set `start_page = None; cx.notify()`.

---

## 10. Menus — `ui/menus.rs` (`createMenus`, main.cpp:1166-1694)

Model the menu tree as **data** (a `Vec<MenuSpec>`), so the Command Palette (Ctrl+K) can walk all
reachable actions and trigger them, and so the menu bar / Linux tool-buttons / macOS native menus
all render from one source (app-shell.md §7 Rust note).

```rust
pub struct MenuItem {
    pub label: SharedString,          // "&Open…" — '&' = mnemonic
    pub shortcut: Option<SharedString>, // "Ctrl+O"
    pub icon: Option<SharedString>,   // svg path
    pub action: AppAction,            // enum dispatched to the window
    pub enabled: bool,
    pub checkable: bool, pub checked: bool,
    pub kind: ItemKind,               // Action | Separator | Submenu(Vec<MenuItem>) | Dynamic(DynId)
}
pub struct MenuSpec { pub title: SharedString, pub items: Vec<MenuItem> }
```

`AppAction` is an enum covering every command. **Port the full menu tree + shortcuts** from
app-shell.md §7 (File / Edit / View / Tools / Plugins / Help) — these drive parity AND the command
palette (T-command_palette in widgets-dialogs/controller specs, but the registry lives here).
Highlights to preserve exactly: New Class `Ctrl+N`, New Struct `Ctrl+T`, New Enum `Ctrl+E`,
Open `Ctrl+O`, Save `Ctrl+S`, Save As `Ctrl+Shift+S`, Close Project `Ctrl+W`, Undo `Ctrl+Z`,
Redo `Ctrl+Y`, Find Field `Ctrl+F`, Add Bookmark `Ctrl+B`, Quick Bookmark `Ctrl+Alt+B`,
Refresh `F5`, Go to Address `Ctrl+G`, Command Palette `Ctrl+K`, Split `Ctrl+\`, Unsplit
`Ctrl+Shift+\`, Memory Scanner `Ctrl+Shift+S`, Symbols `Ctrl+Shift+Y`, Bookmarks `Ctrl+Shift+B`,
Presentation `Ctrl+Shift+P`, RTTI Browser `Ctrl+Shift+R`, Validate `Ctrl+Shift+V`, Profiler
`Ctrl+Shift+F`, Keyboard Shortcuts `F1`. Checkable defaults: Compact Columns ON, Tree Lines ON,
Relative Offsets ON, Show Comment chips OFF, Show RTTI chips ON, Show Enum-value chips ON, Hover
Effects ON, Minimap OFF. `Dynamic` items: Recent Files, Examples (scan `<exe>/examples/*.rcx`),
Data Source (`#clear`/`#saved:<n>`/`<data>` handler, main.cpp:1244-1255), Theme submenu (one per
`ThemeManager::themes()` + "Edit Theme…"), Font submenu (Consolas/JetBrains Mono/IBM Plex Mono).
Win-only "Relaunch as Administrator" behind `#[cfg(windows)]` (token elevation check).

Shortcuts → GPUI `KeyBinding`s bound to `AppAction`s (cookbook §5). On macOS, also feed the same
tree to `cx.set_menus(...)`.

**Reset Windows** (main.cpp:1295-1374) → `reset_windows()`: in the region model, rebuild the
layout to canonical state: Workspace→Left shown, Bookmarks→Left hidden, Symbols→Right hidden,
Scanner→Bottom hidden, all doc tabs in center; raise active; workspace width =
`compute_workspace_dock_width()`. (No corners/sentinels to reset.)

`applyMenuBarTitleCase(bool)` (main.cpp:1133-1164) → `apply_menu_bar_title_case(bool)`: delegate
to `TitleBar::set_menu_title_case` (or apply the same transform to `MenuSpec` titles directly).

---

## 11. Project lifecycle — in `ui/window.rs` (app-shell.md §14)

| C++ fn | Rust signature | Behavior |
|---|---|---|
| `project_new(classKeyword="", forceFreshDoc=false)` (6260-6368) | `fn project_new(&mut self, class_keyword: &str, force_fresh: bool, cx) -> DocId` | If an active doc exists and `!force_fresh`: add a root struct (`build_empty_struct`) to the *same* doc + open a new tab sharing it (copy saved sources). Else fresh `RcxDocument`. **Win-only:** self-attach to a 64KB owned RW buffer via processmemory plugin (out of scope → graceful skip). **In-scope path (incl. non-Win):** a `BufferProvider` of **256 zero bytes at base `0x00400000`** so bytes are writable. Then `build_empty_struct` + `create_tab`. |
| `newClass`/`newStruct`/`newEnum` | thin wrappers → `project_new(class/""/enum, force_fresh=true)` | |
| `buildEmptyStruct(tree, classKeyword)` (4069+) | `build_empty_struct(tree, kw)` | `enum`→bare `UnnamedEnumN`; else `UnnamedClass/StructN` + default hex children. Global `class_counter` increments per new class. (core subsystem helper.) |
| `project_open(path="")` (6370-6515) | `fn project_open(&mut self, path: Option<PathBuf>, cx) -> Option<DocId>` | File dialog if no path (`*.rcx`/All). **Autosave recovery:** if `<path>.autosave` is newer → confirm-restore dialog; on restore, load shadow but reset `file_path` to real + mark modified. Detect XML by first 64 bytes (`<?xml`/`<ReClass`) → `import_reclass_xml`. Else `doc.load` (progress for large files; >5000 nodes "Composing…"). Close all existing tabs under `closing_all` guard before creating the new tab. Report class/node counts + sibling-overlap count to status. `add_recent_file`. |
| `project_save(dock=None, saveAs=false)` (6517-6542) | `fn project_save(&mut self, id: Option<DocId>, save_as: bool, cx) -> bool` | Default to active. If save_as or no path → dialog (`*.rcx`/`*.json`); else save existing path. Remove `.autosave` shadow. `add_recent_file`, `update_window_title`, `rebuild_workspace_model`. |
| `project_close(dock=None)` (=`dock->close()`) | `fn project_close(&mut self, id: Option<DocId>, cx)` | → `close_tab`. |
| `closeAllDocDocks()` | `close_all_doc_docks()` | Copy `order` first; close each. Wrap in `closing_all`. |
| `autosaveAllModifiedDocs()` (60s timer) | `autosave_all_modified_docs()` | `cx.spawn` 60s interval; write `<path>.autosave` for modified docs with a known path. |
| `addRecentFile`/`updateRecentFilesMenu`/`populateSourceMenu` | recent-files list in `settings.recent_files` | Most-recent-first, dedupe-to-top. |

---

## 12. Window-level behaviors — `ui/window.rs` (app-shell.md §15)

| C++ | Rust | Behavior |
|---|---|---|
| `updateWindowTitle()` (5366-5385) | `update_window_title(window)` | mac → always "Reclass"; else `<rootName>[ *] - Reclass` from active tab's view root. Always also `update_scanner_title()`. Set via `window.set_window_title`. |
| `updateScannerTitle()` (5387-5429) | `update_scanner_title()` | Scanner dock title follows active provider: `"Memory Scanner — <name> (<kind>)"` or `"— no source on active tab"`; set title + explanatory tooltip. |
| `changeEvent` (8924-8953) | window activation/state observers (`window.observe_window_activation`, `…_appearance`) | ActivationChange → border color (focused/unfocused) + dismiss editor popups on deactivate; state change → titlebar max icon; drive controllers' adaptive refresh `set_window_state(focused, visible)`; re-raise border. |
| `resizeEvent` (8955-8966) | resize handled by GPUI layout | reposition resize grip + border overlay (automatic in the region render). |
| `updateBorderColor(color)` | `set_border_color(color)` | update the border element. |
| `closeEvent` (8973-9040) | `window.on_close_requested` / handle the close action | Collect unique dirty docs + root-struct labels (`collect_dirty_doc_labels` = file name if saved, else all root struct names). Show unsaved-changes confirm (`AlertDialog`) with one of 3 wordings (1 doc/1 class, 1 doc/N classes, N docs). Cancel → veto close; Save → save each (abort on failure); Discard → close. |
| `eventFilter` (3847-3996) | scattered GPUI handlers | **F12** screenshot (grab window rect → `<exe>/issue.png` — best-effort/stub), **Ctrl+Shift+Click** UI inspection (`inspect_at` → overlay + status), "+" tab click + middle-click close (handled in `DocTabStrip`), dock-resize size readout (handled in divider drag). |
| `collectDirtyDocLabels(doc)` | `collect_dirty_doc_labels(doc) -> Vec<String>` | file name if on disk, else every root struct name. |

`createSplitPane(tab)` (main.cpp:2274-2701) — builds the 3-tab (Reclass/Code/Debug) pane. The
tab-widget chrome (South tabs, documentMode, the bottom-right `fmtCombo`/`scopeCombo`/`fmtGear`
corner shown only on Code) is shell chrome; the editor/rendered/debug contents are
**editor-surface**. Port the chrome shape here, defer contents. Persist `codeFormat`/`codeScope`.

---

## 13. Error-handling strategy

- App boundary (`ui/app.rs::run`) returns `anyhow::Result<()>`; window-open / settings-load
  failures bubble with context.
- File I/O for project/autosave/recent goes through `core`/`imports` which return
  `thiserror` types; the shell maps them to status-bar messages + `AlertDialog`s (mirrors the C++
  `ThemedMessageBox`). Never panic on user-facing failure (bad file, missing examples dir).
- Missing icon assets / SVG load failures: degrade gracefully (skip the icon), matching the C++
  `QSvgRenderer::isValid()` guards in `draw_tab_source_icon`.
- Provider/`is_valid` is queried, never unwrapped — a stale source just dims the tab icon.
- `#[cfg]`-gated platform code (DWM, NSWindow, admin relaunch, minidump) must **compile to a
  no-op** on other targets (only Linux is verifiable). Use `#[cfg(...)]` + `#[allow(unused)]`
  stubs returning `Ok(())`.
- Settings parse errors → fall back to defaults (don't block startup).

---

## 14. TEST PLAN

The pure-logic oracle (`_oracle/RESULTS.md`) has **no app-shell entry** — the shell's covering
tests are GUI/pixel tests (`G` grade) not in the headless capture set. They were not run for the
golden capture, so there is **no golden stdout** to diff against; instead they assert structural
invariants and pixel-sampled visuals. Translate each to a Rust `#[test]` that exercises the ported
*logic* headlessly where possible, and a `#[gpui::test]` (gated `feature="ui"`, run with a GPUI
test context) for the render/pixel assertions. Order them so logic-only tests land first.

> Run logic tests with `--no-default-features` (no gpui). The pixel tests require the `ui` feature
> and a `gpui::test` window; they are lower priority and may be `#[ignore]`d in CI if no display.

**Step ordering (small, independently verifiable):**

**T0 — Foundations (no UI feature).** Port the data structures + pure helpers:
`LayoutPreset` (value-stable 0/1), `DropZone` (value-stable enum), `compute_workspace_dock_width`
formula, `build_groups` bucketing, title-case transform, `update_drop_target` zone selection,
`menu_spec` tree. Each gets a `#[test]`.

**T1 — `test_project_dock.cpp` → `docks::sidebar` tests (logic).**
The C++ asserts: project dock starts left of center; width 10–40% of window; stays left after
hide/show; respects user drag to the right. In Rust the literal Qt geometry is replaced by the
`DockLayout` model:
- `#[test] workspace_starts_on_left` — `place_sidebar_dock(workspace, Left, …)` then assert it's
  in `layout.left` and ordered before `center`.
- `#[test] workspace_width_reasonable` — assert `compute_workspace_dock_width()` ∈ `[180,420]` and,
  for a window of width W, the resolved sidebar width / W ∈ `(0.10, 0.40)` (the C++ ratio bounds).
- `#[test] workspace_stays_left_after_hide_show` — hide then `place_sidebar_dock` again → still Left.
- `#[test] place_sidebar_respects_existing_side` — if user moved it to Right, re-show must NOT force
  it back to Left (mirrors `dockRespectsDragAfterShow`).

**T2 — `test_dock_size_tip.cpp` → `docks::size_readout` tests.**
Most of the C++ test probes Qt's separator-drag internals (which cursor mechanism fires, whether
`QEvent::Resize` is continuous) — **Qt-specific, not portable**. Port the portable behavioral core:
- `#[test] size_readout_body_updates_per_call` ⇐ `tooltipBodyTextChangesPerUpdate` /
  `fullPipelineReadoutUpdatesPerPixel`: call `set_text(title, body_i)` 10× with distinct bodies →
  assert the stored/last-rendered body equals the latest each time AND that 10 distinct bodies
  produce 10 distinct render states (no dedup/coalesce — the C++ "no early-return" guarantee).
- `#[test] size_readout_body_format` ⇐ `showDockSizeTipForAxis`: assert the body string format is
  exactly `"<self> px | <other> px"` for given inputs.
- `#[test] workspace_opens_at_minimum_width` ⇐ `workspaceOpensAtMinimumWidth`: tabify workspace
  (min 235) onto a 600px peer + force-resize → resolved width ≈ 235 (∈ [230,240]).
The cursor/resize-event probes become `// Qt-internal; N/A in GPUI (divider is our own on_drag_move)`
notes, not tests.

**T3 — `test_tab_source_icon.cpp` → `docks::tab_source_icon` tests (`#[gpui::test]`, ui feature).**
This is the strongest pixel oracle; `draw_tab_source_icon` is the shared helper both live and test
use. Render into an offscreen and sample pixels:
- `#[gpui::test] renders_at_correct_location_and_size` — icon at `iconRect(8,8,14,14)` has visible
  alpha inside, ~0 alpha in an outside rect `(30,8,14,14)`.
- `#[gpui::test] tint_matches_requested_color` — render with tint (240,240,240) vs (120,120,120);
  selected avg-red > dim avg-red + 50; each within ±20 of requested.
- `#[gpui::test] live_false_dims_opacity` — `live=false` avg alpha < `live=true` × 0.6 (the ×0.40
  multiplier; threshold matches the C++ assertion).
- `#[gpui::test] different_icon_paths_render_differently` — `file-binary.svg` vs
  `server-process.svg` differ in > 20% of icon-rect pixels.
- `#[gpui::test] icon_and_text_baselines_align` — reproduce the 37px tab cell (2px accent + 1px
  border), compute icon vertical center vs text vertical center; assert `|delta| <= 1px`. **The
  37px height + `kIconPad=8`/`kIconGap=6`/`kIconSz=font_height` geometry is load-bearing.**
(Pixel sampling: render via a `gpui::test` window's scene capture or a CPU rasterization helper;
if scene capture is unavailable, factor `draw_tab_source_icon` to also expose a `render_to_image`
test seam that paints the same quads to an `image::RgbaImage` so the alpha/tint assertions run
without a GPU.)

**T4 — `test_doc_tab_chrome.cpp` → `docks::doc_tabs` tests.**
- `#[test] close_button_closes_tab` ⇐ `closeButtonClosesDock`: build a `DocTabModel` with 2 tabs;
  invoke the close action for tab A → A removed from `order`, no longer in `tabs`.
- `#[test] source_props_persist_across_reorder` ⇐ `sourcePropertiesPersistAcrossTabify`: set
  `source_icon`/`source_live` on a `DocTab`; move it between sides/reorder → fields unchanged
  (trivially true since they're struct fields, not tab-index-keyed — this is the *point* of the
  port, and the test locks it in).
- `#[gpui::test] tab_paints_source_icon` ⇐ `tabPaintsSourceIconAtCorrectLocation`: render a tab
  with `source_icon` set + `source_live=true`; sample x≈8..22 vertical-center band → visible alpha
  (> threshold). (The C++ dead-tab dimness assertion is intentionally soft; keep it soft.)
- `#[test] last_tab_close_creates_new_class` (parity behavior, app-shell.md §20): close the only
  tab with `closing_all=false` → a fresh class tab exists afterward; with `closing_all=true` → no
  resurrection.

**T5 — `test_titlebar_border.cpp` → `titlebar` test (`#[gpui::test]`).**
- `#[gpui::test] close_hover_no_bleed_below_titlebar` ⇐ `testCloseButtonHoverNoBleed`: render the
  titlebar (32px) with the close button in hovered state (hover bg = warning red `markerPtr`/
  `indHeatHot`); scan the column under the close button at and just below `titlebar_bottom`;
  assert NO red pixels (R>150,G<100,B<100) at or below the titlebar bottom edge. Locks the
  32px-clipped hover region.

**T6 — Start page logic tests (`start_page`, no UI feature for the data parts).**
No dedicated C++ unit test, but the behaviors are parity-relevant (app-shell.md §13/§20):
- `#[test] start_page_buckets_recents` — `build_groups` puts entries in Today/Yesterday/This
  week/This month/Older/Examples per the day-delta rules; empty buckets omitted; order fixed.
- `#[test] start_page_filter` — search substring matches file_name OR dir_path (lowercased).
- `#[test] start_page_hit_zones` — `hit_test` returns Card before Continue before Group/Entry, and
  only inside the list y-range for groups/entries.
- `#[test] show_start_page_preloads_class_when_empty` — with no tabs, `show_start_page` preloads a
  New Class; dismiss lands on it. With tabs present, no preload.

**T7 — Menu registry test (`menus`).**
- `#[test] menu_tree_complete` — the `MenuSpec` tree contains every command + shortcut from §10
  (assert a representative checklist: counts per menu, the load-bearing shortcuts, checkable
  defaults). Feeds the command-palette enumeration test in the controller spec.

**Not ported as tests:** `grab_tabs.cpp` (screenshot tool, not a test), `test_pixels.py` (visual
QA tool). `test_scanner_ui.cpp` (DISABLED upstream — engine covered elsewhere). The Qt
separator-cursor probes from `test_dock_size_tip` (toolkit-internal).

---

## 15. Work order (small, independently-verifiable steps)

1. **`ui/docks/mod.rs` data model** — `DocId`, `DocTab`, `DocTabModel`, `ViewMode`, `SplitPane`
   skeleton, `DockLayout`, `DropZone`, `LayoutPreset`. + T0 tests. *(no UI feature)*
2. **Settings struct** — JSON config (`directories`), exact `QSettings` key names; recent-files
   list helpers. + a roundtrip `#[test]`.
3. **`ui/docks/sidebar.rs`** — `place_sidebar_dock`, `save/load_dock_size`,
   `compute_workspace_dock_width`, `apply_layout_preset`. + T1.
4. **`ui/docks/size_readout.rs`** — `DockSizeReadout` model + body-string formatter. + T2.
5. **`ui/docks/tab_source_icon.rs`** — `draw_tab_source_icon` (+ `render_to_image` test seam). + T3.
6. **`ui/docks/doc_tabs.rs`** — `DocTabStrip` render (37px tabs, "+" tab, close ✕, source icon),
   `create_tab`, `close_tab` (incl. never-blank), `refresh_doc_tab_source_icon`, active accessors,
   split/unsplit. + T4.
7. **`ui/titlebar.rs`** — `TitleBar` view, `LayoutPreset` wiring, title-case, theme apply, macOS
   `#[cfg]` stub. + T5.
8. **`ui/status_bar.rs`** + **`ui/widgets.rs`** — `FlatStatusBar`/`ShimmerLabel` (+ status APIs,
   progress), `BorderOverlay`/`ResizeGrip`/`DockGripWidget`/`DockTitleBar`.
9. **`ui/start_page.rs`** — overlay view, paint, hit-test, dismiss; window integration. + T6.
10. **`ui/menus.rs`** — `MenuSpec` registry + `AppAction` + keybindings + Reset Windows. + T7.
11. **`ui/docks/overlay.rs`** — `DockOverlay` element + `update_drop_target` + drag glue
    (`on_dock_drag_started`/`on_dock_drop_requested`). + (T0 covers `update_drop_target`).
12. **`ui/window.rs`** — assemble: root `MainWindow` view, dock layout, signal wiring, window-level
    behaviors (title, scanner title, activation, close-with-unsaved), project lifecycle.
13. **`ui/app.rs`** + **`main.rs`** — `run()`, CLI, deferred startup, panel/symbol pre-warm,
    show-start-page, platform `#[cfg]` stubs. End-to-end smoke.

Each step compiles green on Linux (`cargo build` / `cargo build --no-default-features` for 1–6,10);
`ui`-feature steps verified with `cargo build` (default features) which is GREEN on this machine
per ARCHITECTURE §6.
