# Subsystem: App Shell (main window, start page, titlebar, docks)

**Key:** `app-shell` · **Portability:** ui-heavy
**Source files:** `src/main.cpp` (9627 lines), `src/mainwindow.h`, `src/startpage.h`, `src/titlebar.cpp`/`.h`, `src/macos_titlebar.h`/`.mm`, `src/dockoverlay.h`, `src/docksizereadout.h`, `src/dock_tab_buttons.h`, `src/tab_source_icon.h`.

This is the top-level Qt6 shell: the application entry point, the frameless main window with a custom titlebar, an MDI-style multi-document model built on `QDockWidget` tabs, dockable side panels (workspace/scanner/symbols/bookmarks), a custom dock-drag overlay, a VS2022-style start page, and a borderless status bar. The Rust port reimplements all window/docking/tab/start-page chrome in **GPUI**.

---

## 1. Purpose & High-Level Architecture

`MainWindow` (a `QMainWindow`) is the single top-level window. Documents are NOT shown as `QMdiSubWindow`s — each open struct view is a `QDockWidget` (objectName `DocDock_<hexptr>`) tabified together in the top dock area, which makes Qt's native `QTabBar` the document tab strip. Side panels (Project/workspace, Memory Scanner, Symbols, Bookmarks) are also `QDockWidget`s. The window is **frameless** (`Qt::FramelessWindowHint`) on Windows/Linux with a custom `TitleBarWidget` installed via `setMenuWidget`; macOS keeps the native title bar + native menu bar.

A document is `RcxDocument` (owns a `NodeTree` + provider + undo stack — out of this subsystem's scope except as referenced). Each dock maps to a `TabState` holding the doc, an `RcxController`, a `QSplitter`, and a vector of `SplitPane`s. Each `SplitPane` is a 3-tab `QTabWidget` (Reclass / Code / Debug).

---

## 2. Application Entry Point — `main()` (main.cpp:9475-9624)

Order of operations (faithful replication matters for first-paint timing & feature parity):

1. **Crash handlers:** Windows `SetUnhandledExceptionFilter(crashHandler)`; Linux/macOS `installPosixCrashHandler()`.
2. macOS only: `QCoreApplication::setAttribute(Qt::AA_DontUseNativeDialogs)`.
3. Construct `DarkApp app(argc, argv)` (a `QApplication` subclass). Set `applicationName="Reclass"`, `organizationName="Reclass"` (drives `QSettings("Reclass","Reclass")` everywhere). Set style `new MenuBarStyle("Fusion")`.
4. `qApp->installEventFilter(new rcx::GlobalTooltipBridge(&app))` — replaces Qt tooltips with `RcxTooltip` (out of scope, but the install order matters).
5. `--profile` (and not `--screenshot`) → enable `Profiler`.
6. Load embedded fonts `:/fonts/JetBrainsMono.ttf`, `:/fonts/IBMPlexMono.ttf` via `QFontDatabase::addApplicationFont`. Read saved font (`QSettings "font"`, default `"IBM Plex Mono"`) and call `RcxEditor::setGlobalFontName`.
7. Construct `rcx::MainWindow window;` set window icon `:/icons/class.png`.
8. `window.show()`; on **Linux** additionally `window.showMaximized()`.
9. **Deferred startup** via `QTimer::singleShot(0, …)` (run after first paint, in this order): `TypeSelectorPopup::preload()`, `window.loadPluginsDeferred()`, `window.ensureScannerPanel()`, `window.createSymbolsDock()`. If auto-profile, also queue `showProfilerDialog`.
10. **`--screenshot <path> [scanner]`** mode: queues `project_new()`, optionally shows scanner dock, then after 1500 ms `window.grab()` → save PNG → `qApp->quit()`. Returns `app.exec()` immediately (skips start page).
11. Normal path: `QMetaObject::invokeMethod(&window, "showStartPage", Qt::QueuedConnection)` then `return app.exec()`.

**Rust note:** GPUI `App::run`. Defaults `--profile`, `--screenshot <path> [scanner]` CLI flags should be parsed; the QTimer-singleShot deferral is a perf optimization, not behavioral — port the work, deferral optional.

### `DarkApp : QApplication` (main.cpp:359-375)
Overrides `notify`. On `QEvent::WindowActivate` for a top-level widget without the `DarkTitleBar` property, sets the property and (Windows) calls `setDarkTitleBar(w)` — one-shot DWM immersive-dark-mode hint per window. **Platform-specific (Win).** Rust: GPUI windows are dark by default; n/a except a Win DWM hint behind `#[cfg(windows)]`.

### Crash handlers (Win: main.cpp:114-251; POSIX: main.cpp:281-356)
- **Windows `crashHandler`:** re-entrancy guard, prints exception code/addr/RIP/RSP, writes a full minidump `reclass_crash_<YYYYMMDD_HHMMSS>.dmp` next to the exe (`MiniDumpWriteDump` with FullMemory|HandleData|ThreadInfo|UnloadedModules), then walks the stack (`StackWalk64`+`SymFromAddr`+`SymGetLineFromAddr64`, up to 64 frames). Returns `EXCEPTION_EXECUTE_HANDLER`.
- **POSIX `posixCrashHandler`:** `SA_SIGINFO|SA_RESETHAND` for SIGSEGV/SIGABRT/SIGFPE/SIGBUS/SIGILL. Prints signal+addr, writes `$HOME/.reclass/crash_<ts>.log`, backtrace via `backtrace`/`backtrace_symbols` (64 frames), re-raises with default handler.

Rust equivalent: a panic hook + optional `backtrace` crate; `minidump-writer` crate (Win) optional. Behind `#[cfg(...)]`. Not behavior-critical to parity.

---

## 3. `MenuBarStyle : QProxyStyle` (main.cpp:377-775)

A `Fusion` proxy style that owns ALL custom painting for menus, menu bar, dock separators, dock tab bars, item views, and status bar. **This is the single source of truth for the app's dark chrome.** Critical for visual parity. Key overrides:

- **`eventFilter`/`polish` (Win-only):** strips `CS_DROPSHADOW` and makes `QMenu` frameless+translucent so menus get the themed 1px border instead of Windows' native drop shadow.
- **`sizeFromContents`:** `CT_MenuBarItem` +50% height; `CT_MenuItem` separator = 7px tall, else +24w/+4h; `CT_ItemViewItem` +4h. **`CT_TabBarTab`** (only for tab bars whose parent is the `QMainWindow` — i.e. dock tabs): fixed height **37px**; sentinel `​` tab → fixed `32×37`; otherwise width += `24` (right close button) + `20` (left source icon).
- **`pixelMetric`:** `PM_MenuPanelWidth=1`, `PM_MenuHMargin=PM_MenuVMargin=3`, `PM_DockWidgetSeparatorExtent=4`, `PM_DockWidgetFrameWidth=0`, `PM_DockWidgetTitleMargin=0`, `PM_DockWidgetTitleBarButtonMargin=0`.
- **`drawPrimitive`:**
  - `PE_IndicatorDockWidgetResizeHandle`: 1px border line at rest (theme `Dark`/border), hover → fill `Mid`/hover + 2px centered accent (`Link`/indHoverSpan). Top/bottom horizontal separators in the top/bottom quarter of the window are painted invisible (just bg) to avoid double lines near menu/status bars.
  - `PE_FrameDockWidget`, `PE_PanelMenuBar`, `PE_FrameStatusBarItem`, `PE_PanelStatusBar`: suppressed (return).
  - `PE_FrameMenu`: bg fill + 1px border via four `fillRect` strips.
  - `PE_Frame`: suppressed for `QsciScintilla` and any widget inside a `QDockWidget`.
  - `PE_FrameTabBarBase` (dock tab bars): single 1px bottom border line in `Dark`/border.
  - `PE_PanelItemViewRow`: patches `Highlight`←`Mid` so row bg matches.
- **`drawControl`:**
  - `CE_MenuBarEmptyArea`: suppressed.
  - `CE_MenuBarItem`: fully owned. Non-hovered transparent (border shows through); hovered → fill `Highlight`; pressed → `Highlight.darker(130)`; text in `Link`/accent when selected, else `ButtonText`. `Qt::AlignCenter | TextShowMnemonic`.
  - `CE_MenuItem`: separator = 1px line in `AlternateBase`; selected → fill `Highlight` then delegate with `State_Selected` cleared and `Text`←`Link`.
  - `CE_ItemViewItem`: hover (not selected) → fill `Mid`; patch `Highlight`←`Mid`, `HighlightedText`←`Text`.
  - **`CE_TabBarTabShape`** (dock tabs): bg = `Window`/background; hovered or (sentinel & selected) → `Mid`/hover; fill is 1px short at the bottom so the base-line border survives; selected non-sentinel → 2px top accent strip in `Link`.
  - **`CE_TabBarTabLabel`** (dock tabs, main.cpp:645-770): the most intricate path.
    - Sentinel `​` tab: draw a `+` glyph (two fillRects) — dim (`Disabled,WindowText`) at rest, accent (`Link`) on hover; centered below 2px accent zone, above 1px bottom border.
    - Regular tab: read the editor font (`QSettings "font"`, 10pt, fixed pitch). `kIconSz = fm.height()`, `kIconPad=8`, `kIconGap=6`. Look up the source icon from the **dock object's properties** `rcxSourceIcon` (path) and `rcxSourceLive` (bool) by matching `tabText` against each `QDockWidget::windowTitle()`. Draw the icon via `rcx::drawTabSourceIcon` at content-area-centered rect (top inset 2 if selected, bottom inset 1). Unselected icons at 70% opacity, selected at 100%; disconnected further drops via `live=false` (×0.40 in helper). Then draw the label text middle/right-elided (`fm.elidedText` ElideRight) reserving the right close-button width + left icon inset.

**Rust note:** GPUI does no Qt-style delegate painting. All of this is reimplemented as direct `gpui` render code per widget (titlebar, tabstrip, menu, statusbar). The exact pixel constants (37px tab height, 2px accent, 8/6 icon paddings, 70% opacity, elide-right) must be reproduced for parity.

---

## 4. `applyGlobalTheme(const Theme&)` (main.cpp:781-827)

Builds a `QPalette` mapping `Theme` colors to Qt palette roles (the canonical mapping the whole style depends on):

| Palette role | Theme field |
|---|---|
| Window/Base | background | WindowText/Text | text | AlternateBase | surface |
| Button | button | ButtonText | text | Highlight | selected | HighlightedText | text |
| ToolTipBase | backgroundAlt | ToolTipText | text | Mid | hover | Dark | border |
| Light | textFaint | Link | indHoverSpan | | |
| Disabled{WindowText,Text,ButtonText,HighlightedText} | textMuted | Disabled.Light | background |

Then `qApp->setPalette(pal)` and a global stylesheet for scrollbars (8px, track=window, handle=textFaint, hover=textDim) and `QToolTip` (bg=backgroundAlt, fg=text, 1px border). **The Theme↔palette-role table is the binding contract the rest of the chrome reads.** Rust: a single `Theme` struct consumed directly by render code; no palette indirection.

---

## 5. `TitleBarWidget` (titlebar.h/.cpp) — Custom Window Chrome

`QWidget`, fixed height **32px** (34 when showing icon). Layout (L→R): app label "Reclass" (10/0/4/0 margins, transparent for mouse), the menu bar, a stretch, then chrome buttons. Subwidgets:

- **`m_appLabel`** `QLabel` (bold 12px). `setShowIcon(true)` swaps text for `:/icons/class.png` 24×24 pixmap and bumps height to 34.
- **`m_menuBar`** `QMenuBar` (`setNativeMenuBar(false)`).
  - **Linux:** `m_useToolButtons=true`; the menu bar is hidden, its top-level menus mirrored as `QToolButton`s (built in `finalizeMenuBar`) added into a separate `m_menuBtnLayout`. Rationale (titlebar.cpp:30-46): a `QMenuBar` inside a custom widget collapses to an extension popup on Linux.
  - **Windows/macOS-non:** the `QMenuBar` is added directly (`Minimum`/`Expanding` size policy).
- **Workspace toggle pair** `m_btnLayoutOff` / `m_btnLayoutOn` — two checkable 34×32 `QToolButton`s in an exclusive `QButtonGroup` (ids `Layout_NoWorkspace=0`, `Layout_Workspace=1`). Icons `:/vsicons/layout-sidebar-left-off.svg` / `layout-sidebar-left.svg`. Off is default-checked. `idClicked` → `emit layoutPresetSelected(id)`.
- **Chrome buttons** `m_btnMin` (`chrome-minimize.svg`), `m_btnMax` (`chrome-maximize.svg`), `m_btnClose` (`chrome-close.svg`), each 46×32, autoraise, no-focus. Min → `window()->showMinimized()`; Max → `toggleMaximize()`; Close → `window()->close()`.

### `enum LayoutPreset { Layout_NoWorkspace=0, Layout_Workspace=1 }` (titlebar.h:15-18)

### Public methods
- `menuBar()` → the `QMenuBar*`.
- `applyTheme(const Theme&)` (titlebar.cpp:117-205): sets autofill bg = background; styles app label; sets full menu-bar palette (Window/Button=background, ButtonText/Text=text, Highlight=selected, Link=indHoverSpan, AlternateBase=surface, Dark=border, Mid=hover) and propagates a similar palette to every child `QMenu`; styles min/max buttons (transparent, hover=hover); the layout toggle pair gets `:checked` → backgroundAlt bg + 2px bottom border in indHoverSpan; Linux menu tool buttons styled; close button hover = `markerPtr` (warning red, fallback indHeatHot).
- `setShowIcon(bool)`.
- `setMenuBarTitleCase(bool)` (titlebar.cpp:222-253): if true → uppercases each top-level menu title (prefixed `&`); else converts to Title Case (capitalize first letter of each word). Mirrors `MainWindow::applyMenuBarTitleCase`. Also syncs the Linux tool-button labels.
- `menuBarTitleCase()` getter.
- `finalizeMenuBar()` (titlebar.cpp:255-277): Linux-only. For each top-level menu action, create a `QToolButton` with `setMenu(action->menu())`, `InstantPopup` popup mode, install `this` as event filter on both the button and its menu, append to `m_menuBtnLayout` and `m_menuButtons`.
- `updateMaximizeIcon()`: swaps max icon for `chrome-restore.svg` when maximized.
- `setWorkspaceChecked(bool)` (titlebar.cpp:106-115): set the appropriate toggle checked **with signals blocked** (avoid re-emitting `layoutPresetSelected`). Wired from `MainWindow` to keep the toggle in sync with workspace dock visibility.

### Protected (event handlers)
- `mousePressEvent` (LMB) → `window()->windowHandle()->startSystemMove()` (native window drag). **Platform-bridged.**
- `mouseDoubleClickEvent` (LMB) → `toggleMaximize()`.
- `paintEvent`: base paint + 1px bottom border line in theme.border.
- `eventFilter` (Linux tool-button menus): on `MouseMove` over an open `QMenu`, if cursor is over a *sibling* menu button, close this menu and `QTimer::singleShot(0,…)` to open the sibling's menu (hover-to-switch top-level menus, like a real menu bar).
- `toggleMaximize`: `isMaximized() ? showNormal() : showMaximized()`, then `updateMaximizeIcon`.

**Rust/GPUI note:** Reimplement titlebar as a GPUI element row: app name, menu (or menu-buttons), stretch, layout-toggle pair, min/max/close. Window drag/resize/min/max/close use GPUI window controls or the platform via `#[cfg(...)]`. `startSystemMove`/`startSystemResize` map to GPUI's draggable-region or a manual move loop.

### macOS titlebar (macos_titlebar.h/.mm)
`void applyMacTitleBarTheme(QWidget* window, const Theme&)` — no-op on non-macOS. On macOS: forces native window creation (`winId()`), grabs the `NSWindow`, computes luminance (0.2126R+0.7152G+0.0722B), selects `NSAppearanceNameAqua`/`DarkAqua`, sets `titlebarAppearsTransparent=YES`, `titleVisibility=Visible`, `backgroundColor=theme.background`. **Platform-specific (mac)**, behind `#[cfg(target_os="macos")]`. Keeps native traffic lights.

---

## 6. `MainWindow` Class

### Construction order (`MainWindow::ctor`, main.cpp:907-1109)
1. `setWindowTitle("Reclass")`, `resize(1080,720)`.
2. **Non-Apple:** `setWindowFlags(FramelessWindowHint | WindowSystemMenuHint | WindowMinMaxButtonsHint)`; create `TitleBarWidget` (objectName `"TitleBarWidget"`), apply theme, connect `layoutPresetSelected→applyLayoutPreset`, `setMenuWidget(m_titleBar)`, `m_menuBar = m_titleBar->menuBar()`. A 0-timer later syncs `setWorkspaceChecked(m_workspaceDock->isVisible())` and connects `workspaceDock.visibilityChanged → setWorkspaceChecked`. Linux: `m_menuBar->setNativeMenuBar(false)`.
   **Apple:** `setUnifiedTitleAndToolBarOnMac(true)`, `m_menuBar=menuBar()` native, `applyMacTitleBarTheme`.
3. **Win:** `DwmExtendFrameIntoClientArea` margins {0,0,0,0} (disable DWM shadow on frameless).
4. `BorderOverlay` (`m_borderOverlay`) — 1px colored window border drawn on top of everything, color = `borderFocused`, geometry = `rect()`, raised+shown.
5. `m_centralPlaceholder` = a zero-size `QWidget` set as `centralWidget` (docks need a central widget anchor; the editor lives in docks, not central). `setDockNestingEnabled(true)`.
6. **Corners:** all four corners assigned so Left/Right dock areas span full window height (`TopLeft/BottomLeft→Left`, `TopRight/BottomRight→Right`). **Tab position:** all four areas → `QTabWidget::North` (tab strip always on top).
7. Build docks: `createWorkspaceDock()`, `createScannerDock()`, `createBookmarksDock()` (synchronous in ctor). `createSymbolsDock()` deferred. Then `createMenus()`, `m_titleBar->finalizeMenuBar()`, `createStatusBar()`.
8. **Autosave timer** `m_autosaveTimer` 60s → `autosaveAllModifiedDocs`.
9. Zero the main layout spacing/margins.
10. Restore `menuBarTitleCase` (default false) + `showIcon` (default false) from settings.
11. Connect `ThemeManager::themeChanged → applyTheme`; call `applyTheme(current())` once.
12. `m_mcp = new McpBridge(this,this)` (out of scope).
13. Register built-in `NameProvider`s (PDB symbols > PDB types > RTTI > Bookmarks) and wire `g_rttiDiscoveryHook`, `g_nameLookupHook`, `g_namesChangedHook` (out of scope).
14. `qApp->installEventFilter(this)` (global Ctrl+Shift+Click inspection, F12 screenshot, sentinel-tab clicks, dock-resize tooltip).
15. `setupDockOverlay()`.
16. 0-timer to re-raise the border overlay.
17. `QApplication::focusChanged` → updates `tab->activePaneIdx` + `syncViewButtons` for whichever pane gained focus.

### Destructor (main.cpp:4113-4134)
Disconnects each dock's `destroyed` + undo-stack signals, **resets the provider while plugin DLLs are still loaded** (`doc->provider.reset()`, `ctrl->resetProvider()`), clears `m_tabs`. Member destruction order matters: `~MainWindow` body runs while plugin DLLs are loaded; then members (incl. `m_pluginManager`) unload DLLs; then QObject child cleanup. **Rust:** providers are `Arc<dyn Provider>`; drop ordering is handled by Rust's `Drop`. No DLL-unload race in the in-scope (no-plugin) build.

### Key inner types (mainwindow.h)

**`enum ViewMode { VM_Reclass, VM_Rendered, VM_Debug }`** (mainwindow.h:149) — per-pane mode = which of the 3 tabs (Reclass/Code/Debug) is active.

**`struct SplitPane`** (mainwindow.h:189-216): one editor split. Fields: `tabWidget` (3-tab QTabWidget), `editor` (`RcxEditor*`), `rendered`/`debugView`/`minimap` (`QsciScintilla*`), `findBar`/`findContainer`/`renderedContainer`/`editorContainer` (`QWidget*`), `fmtCombo`/`scopeCombo` (`QComboBox*`), `fmtGear` (`QToolButton*`), `viewMode`, and a render cache: `lastRenderedRootId`, `lastRenderedText`, `lastRenderedTreeGen`, `lastRenderedFmt`, `lastRenderedScope`, `lastRenderedAsserts` (skip regen when the keyed inputs are unchanged).

**`struct TabState`** (mainwindow.h:218-224): `doc` (`RcxDocument*`), `ctrl` (`RcxController*`), `splitter` (`QSplitter*`), `panes` (`QVector<SplitPane>`), `activePaneIdx`.

**`struct ReferenceHit`** (mainwindow.h:138-144): `ownerDock`, `nodeId`, `ownerType`, `fieldName`, `fieldOffset` — find-references result.

**`struct InspectionResult`** (mainwindow.h:166-174): Ctrl+Shift+Click UI inspector output: `selected`, `widgetName`, `region`, `description`, `globalRect`, `themeColors` (JSON array), `properties` (JSON object).

**`struct SplitPane`-keyed maps:** `QMap<QDockWidget*, TabState> m_tabs`, `QVector<QDockWidget*> m_docDocks` (ordered), `QPointer<QDockWidget> m_activeDocDock`, `QVector<QDockWidget*> m_sentinelDocks`, `QVector<RcxDocument*> m_allDocs`.

**`struct ClosingGuard`** (mainwindow.h:234-238): RAII bool flag (`m_closingAll`) set true during a batch close to suppress spurious `project_new`.

**Rust modeling:** `m_tabs` keyed by dock pointer → in Rust use a `SlotMap`/`Vec<DocTab>` with a stable id; `m_docDocks` is the ordered tab strip; `m_activeDocDock` is the active index. Sentinels (see §8) are a Qt-specific hack and need NOT be ported literally in GPUI — replicate the *behavior* (always-visible tab strip, "+" tab) directly.

---

## 7. Menus — `createMenus()` (main.cpp:1166-1694)

All actions added through `Qt5Qt6AddAction(menu, text, shortcut, icon, recv, slot)` helper (main.cpp:1123-1131): adds icon+text action, sets shortcut if non-empty, connects `triggered`. Mnemonics use `&`. Full menu tree (the Rust port needs this exact command set + shortcuts; shortcuts drive parity and the Command Palette):

**File:** New Class (Ctrl+N), New Struct (Ctrl+T), New Enum (Ctrl+E), Open… (Ctrl+O), Recent Files (submenu, `m_recentFilesMenu`), Welcome Screen → `showStartPage`; sep; Save (Ctrl+S), Save As… (Ctrl+Shift+S→`SaveAs`); sep; Import submenu (From Source…, ReClass XML…, PDB…), Export submenu (C++ Header…, Rust Structs…, #define Offsets…, C# Structs…, Python ctypes…, ReClass XML…); Examples submenu (scans `<exeDir>/examples/*.rcx`, mac: `../Resources/examples`); sep; Close Project (Ctrl+W); sep; **Win-only** "Relaunch as Administrator" (Ctrl+Shift+A) shown only when NOT elevated (token check); `m_sourceMenu` "Data Source" (populated lazily on `aboutToShow`); sep; Exit.
- Data Source `triggered` handler (main.cpp:1244-1255): `#clear`→`clearSources()`, `#saved:<n>`→`switchSource(n)`, else `selectSource(data)`.

**Edit:** Undo (Ctrl+Z), Redo (Ctrl+Y); sep; Find Field… (Ctrl+F); sep; Add Bookmark… (Ctrl+B), Quick Bookmark Here (Ctrl+Alt+B — auto-named `bookmark_NN` from formula or hex base).

**View:** Reset Windows (rebuilds the whole dock layout — see below); sep; Font submenu (Consolas / JetBrains Mono / IBM Plex Mono, exclusive `QActionGroup`, default from settings); Theme submenu (one checkable action per `ThemeManager::themes()`, plus "Edit Theme…"); sep; checkable: Compact Columns (default on), Tree Lines (default on), Relative Offsets `m_actRelOfs` (default on); sep; "Tail Chips" disabled header + Show Comment chips (default off), Show RTTI chips (default on), Show Enum-value chips (default on); sep; Hover Effects (default on), Minimap (default off); Refresh (F5), Go to Address… (Ctrl+G), Command Palette… (Ctrl+K); sep; Split View Below (Ctrl+\), Unsplit View (Ctrl+Shift+\); sep; workspace `toggleViewAction()`, Memory Scanner (Ctrl+Shift+S checkable — builds panel lazily on toggle), Symbols (Ctrl+Shift+Y checkable — builds dock lazily), Bookmarks `toggleViewAction()` (Ctrl+Shift+B); sep; Presentation Mode (Ctrl+Shift+P checkable).

**Tools:** RTTI Browser (Ctrl+Shift+R — reads selected hex/pointer field, derefs vtable ptr, opens browser), Type Aliases…, Validate Project… (Ctrl+Shift+V), Performance Profiler… (Ctrl+Shift+F); sep; Start/Stop MCP Server `m_mcpAction` (label from `autoStartMcp` setting); sep; Options….

**Plugins:** Manage Plugins….

**Help:** Keyboard Shortcuts… (F1), About Reclass.

**Reset Windows** (main.cpp:1295-1374) — the canonical layout-reset algorithm (port for parity):
1. Reset all 4 corners (Left/Right own corners).
2. Delete every sentinel.
3. `addDockWidget` each dock to its canonical area (Workspace→Left shown, Bookmarks→Left hidden, Symbols→Right hidden, Scanner→Bottom hidden, each DocDock→Top shown), un-floating first.
4. Tabify all doc docks with the first; raise active.
5. Workspace width = `computeWorkspaceDockWidth()`.
6. `reconcileDockTabBars()`.

`applyMenuBarTitleCase(bool)` (main.cpp:1133-1164) — delegates to `m_titleBar->setMenuBarTitleCase` if present; otherwise applies the same upper/title-case transform to the menu bar's actions.

**Rust note:** Model the menu tree as data (a `Vec<MenuSpec>`); the Command Palette (Ctrl+K) walks all reachable actions and triggers them, so menus should be a queryable action registry.

---

## 8. The Document-Tab / MDI System (the heart of this subsystem)

Each open struct view is a `QDockWidget` named `DocDock_<hexptr>` tabified into the top area. Because Qt only renders a `QTabBar` when a dock area holds 2+ docks, a **sentinel dock** trick keeps the tab strip visible for a lone document and provides the "+" new-tab affordance.

### `createSentinelDock()` (main.cpp:2785-2796)
Invisible `QDockWidget` named `_sentinel_<hexptr>`, `NoDockWidgetFeatures`, empty widget, zero-height title bar, **windowTitle = `​`** (zero-width space) — the magic string the style/hit-testing uses to identify the sentinel "+" tab.

### `createTab(RcxDocument* doc)` → `QDockWidget*` (main.cpp:2798-3312)
The central factory. Steps:
1. `QSplitter(Qt::Horizontal, handleWidth=1)`; `RcxController(doc, splitter)`.
2. New `QDockWidget(title=rootName(tree))`, objectName `DocDock_<ptr>`, features Closable|Movable|Floatable, `WA_DeleteOnClose`.
3. **Two title-bar widgets:** `emptyTitleBar` (0-height, used while docked) and `floatTitleBar` (24px, used while floating) with a `DockGripWidget`, a title `QLabel` (`dockFloatTitle`), a stretch, and a close `QToolButton` (`dockFloatClose`, "✕"). Float titlebar has a custom context menu: Dock / Always Floating (toggles `DockWidgetMovable`) / Close.
4. `setTitleBarWidget(emptyTitleBar)`, `setWidget(splitter)`.
5. Per-dock `BorderOverlay` + `ResizeGrip` (hidden until floating). `topLevelChanged(floating)` swaps the title bar and shows/hides the border+grip; on re-dock calls `reconcileDockTabBars()`. `DockBorderFilter` event filter keeps the border/grip positioned + colored (focused=borderFocused, deactivated=border).
6. **Tabify target selection (main.cpp:2918-2937):** prefer `m_activeDocDock` (if it's a non-floating visible dock ≠ new); else `m_docDocks.last()`; else `addDockWidget(TopDockWidgetArea)` (new group). `tabifyDockWidget(target, dock)`.
7. Append to `m_docDocks`, register `m_tabs[dock] = {doc,ctrl,splitter,{},0}`, set `m_activeDocDock=dock`.
8. Create initial `SplitPane` (`createSplitPane`). Apply per-doc view settings from `QSettings` (compactColumns, treeLines, braceWrap, typeHints, showComments, showRttiChips, showEnumChips). `ctrl->setProjectDocuments(&m_allDocs)`, `rebuildAllDocs()`.
9. **Signal wiring** (the behavioral contract — port each):
   - `dock.visibilityChanged(visible)` → if visible: `m_activeDocDock=dock`, `updateWindowTitle()`, `syncViewButtons(active pane viewMode)`, `refreshBookmarksDock()`. Always re-raise border overlay.
   - `doc.documentChanged` → if active, refresh bookmarks; `refreshDocTabSourceIcon(dock)`.
   - `ctrl.sourceLivenessChanged` → `refreshDocTabSourceIcon` + (if active) `updateScannerTitle`.
   - `dock.destroyed` → erase from `m_tabs`; delete the doc only if no other tab references it (`deleteLater`); remove from `m_docDocks`; reassign `m_activeDocDock` to last; `rebuildAllDocs`+`rebuildWorkspaceModel`+`updateWindowTitle`; **if `m_tabs` empty and not `m_closingAll`: delete sentinels and `project_new()`** (never leave a blank window).
   - `ctrl.nodeSelected(idx)` → build a rich status string (`Struct.field`, `+0xNN`, ←→ variant hints, struct size). Also `updateAllRenderedPanes`/`updateAllDebugPanes`.
   - `ctrl.selectionChanged(count)` → status "N nodes selected".
   - `ctrl.statusHint(text)` → `setAppStatus`.
   - `ctrl.contextMenuAboutToShow(menu,line)` → adds "Copy as C Struct".
   - `ctrl.requestOpenStructInNewTab(structId)` → reuse existing tab if one already views it, else `createTab(doc)` (same doc) + `setViewRootId`.
   - `ctrl.requestOpenProviderTab(pluginId,target,title)` → new doc + `attachViaPlugin` (plugin — out of scope; the file/in-memory equivalent path still applies).
   - `doc.documentChanged` and `undoStack.indexChanged` → debounced (0-timer, `QPointer` guarded) updates of rendered/debug panes + window title + workspace + symbols. The undo path only regenerates panes that are *visible*.
   - `doc.documentChanged` → `m_mcp->notifyTreeChanged()` (out of scope).
10. **Auto-focus root:** prefer `tree.initialClass` (match `structTypeName` or `name`), else first root struct → `ctrl->setViewRootId`.
11. `ctrl->refresh()`, `rebuildWorkspaceModel()`, `dock->raise()/show()`, select the last tab in the bar, `reconcileDockTabBars()`.

### `reconcileDockTabBars()` (main.cpp:3483-3566) — idempotent, re-entry-guarded (`m_reconciling`)
The synchronous replacement for a previous family of racing 0-timers.
- **Phase A:** delete any sentinel not tabified with a visible non-floating doc dock.
- **Phase B:** for every visible non-floating doc dock with no partner (another doc OR a sentinel) in its tab group, create+tabify a fresh sentinel so the strip renders, raise the dock.
- **Phase B′:** for each main-window-owned `QTabBar`, if the current tab isn't a doc dock (e.g. the sentinel got auto-selected), switch to the first doc-dock tab.
- **Phase C:** `setupDockTabBars()` (restyle + install buttons).

### `setupDockTabBars()` (main.cpp:3613-3845)
For each `QTabBar` whose parent is the `QMainWindow`:
- Clear stylesheet (painting via style), `WA_Hover`, `ElideNone`, `Expanding(false)`, `UsesScrollButtons(true)`, editor font, palette (textDim/text/background/hover/border/indHoverSpan → WindowText/Text/Window/Mid/Dark/Link).
- Restyle overflow scroll arrows to `chevron-left/right.svg`.
- Ensure the sentinel `​` tab is always last (`moveTab`).
- Install `DockTabButtons` (close ✕) on the **RightSide** of each non-sentinel tab if missing (close-btn hover uses `theme.selected`, not hover, so it's visible over a hovered tab). Wire close→`dock->close()`. Remove any stale LeftSide widget (the source icon is now painted inline). Populate source-icon props on doc docks (`refreshDocTabSourceIcon`).
- Once per bar (guard: `contextMenuPolicy()==CustomContextMenu`): install `this` and `m_dockDragDetector` as event filters, `CustomContextMenu` policy, and a context menu (Close, Close All Tabs, Close All But This, Copy Full Path / Open Containing Folder (saved docs), Float/Dock, New Horizontal/Vertical Document Group).
- Re-raise border overlay.

### `refreshDocTabSourceIcon(QDockWidget*)` (main.cpp:3568-3607)
Resolves the active source's icon (`iconForProvider(kind)` from sourcechooserpopup.h) + liveness (`provider && provider->isValid()`); falls back to `:/vsicons/plug.svg` "No source". Stores `rcxSourceIcon`/`rcxSourceLive` **as properties on the dock object** (survive tab reorder/tabify) — read by `CE_TabBarTabLabel`. Sets per-tab tooltips.

### Sentinel "+" tab click (eventFilter, main.cpp:3927-3955)
On `MouseButtonPress` on a `QTabBar`: if the clicked tab text is `​` and LMB → `project_new()` + raise/show + reconcile (swallow other buttons). Middle-click on a real tab → close that dock (doc or sidebar).

### `DockTabButtons` & `DockTabSourceIcon` (dock_tab_buttons.h)
- **`DockTabButtons : QWidget`**: holds a single 16×16 `closeBtn` (`:/vsicons/close.svg` 12×12), placed via `setTabButton(RightSide)`. `applyTheme(hover)` styles hover bg.
- **`DockTabSourceIcon : QLabel`** (16×16, transparent for mouse): `setSourceIcon(path, live, tip)` renders the SVG at native DPR (target 14px logical), 0.35 opacity when `!live`. Caches on (path, live, dpr). *Now superseded by inline `drawTabSourceIcon` painting in the tab label; kept for non-tab uses.*

### `drawTabSourceIcon(p, iconRect, iconPath, live, tint)` (tab_source_icon.h:21-52)
Shared SVG-icon renderer used by both the live tab paint and tests. Renders SVG into a DPR-sized pixmap (**dpr set BEFORE attaching painter** — load-bearing comment), then tints via `CompositionMode_SourceIn` with `tint`; `live=false` multiplies opacity by 0.40. Rust: render SVG → tint → composite into the tab; `live` controls alpha.

**Rust/GPUI sentinel note:** In GPUI you render the tab strip directly, so the sentinel-dock workaround is unnecessary — render a persistent "+" tab element and the document tabs as a `Vec`. But replicate the *behaviors*: always-visible strip, "+" opens a new struct, middle-click closes, right-click context menu, drag-to-reorder/redock, left source icon (full opacity=live, 70% unselected, 0.40 disconnected), right close ✕, ElideRight labels.

---

## 9. Dock Drag Overlay (`dockoverlay.h`)

### `enum class DropZone` (dockoverlay.h:22-28)
`None, Left, Right, Top, Bottom` (retained for value stability but never produced/drawn), `Center` (tabify with hovered dock), `Float`, `EdgeLeft/Right/Top/Bottom` (outer window-frame zones). **Only Center + the four Edge zones + Float are live.**

### `DockOverlay : QWidget` (dockoverlay.h:34-430)
A transparent overlay covering the whole `QMainWindow` during a dock drag; grabs the mouse + keyboard. Public API:
- ctor: `WA_TransparentForMouseEvents=false`, `WA_NoSystemBackground`, mouse tracking, `ClosedHandCursor`, hidden.
- `setAccentColor`, `setTheme(Theme)` (accent defaults to `borderFocused` unless overridden), `setAccentColorOverride`.
- `beginDrag(dock, title)`: store dragged dock/title, geometry = main window rect, raise, show, `grabMouse()`, `setFocus()`.
- `endDrag()`: release mouse, hide, clear state.
- getters `activeZone`/`draggedDock`/`hoveredDock`.
- Signals: `dropRequested(source, target, zone)`, `dragCancelled(source)`.

Behavior:
- `mouseMoveEvent` → `updateDropTarget(pos)` + repaint.
- `mouseReleaseEvent` → snapshot zone/target/source, `endDrag()`, emit `dropRequested` (if zone≠None) else `dragCancelled`.
- `keyPressEvent(Escape)` → `endDrag()` + `dragCancelled`.
- `paintEvent`: draw active edge zone strip, draw the Center tabify diamond (only when hovering a *doc* dock), draw the preview rect, draw a floating cursor label with the drag title.

**`contentRect()`** (dockoverlay.h:177-190): the dockable region excluding chrome — top below the TitleBarWidget/menu bar, bottom above the status bar. Used for both drawing and hit testing so they stay in lockstep.

**`isTabbableTarget(d)`**: only `DocDock_*` docks are valid Center-tabify targets (never sidebars — tabifying a sidebar would hide it).

**`updateDropTarget(pos)`** (dockoverlay.h:239-275): if outside contentRect → None. Edge zones activate when cursor within `kEdgeW=36` of an edge AND `zoneAllowed` (the dragged dock's `allowedAreas()` includes that side). Otherwise `findDockAt(pos)` (a tabbable doc dock); none → Float; else Center (diamond hit radius `kHitR=20`, but the whole dock face falls back to Center). Constants: `kEdgeW=36, kTargetSz=28, kTargetDist=52, kHitR=20`.

Drawing helpers: `drawEdgeZones` (translucent accent fill α60 + 3px solid accent stripe, only when active), `drawDiamondTargets` (rounded-rect tabify icon = two overlapping squares, inverts when active), `drawPreviewRect` (`computePreviewRect`: edge = ¼ of window, Center = whole hovered dock; theme-text 2px outline + accent α60 fill + centered label "Tabify"/"Dock Left"…), `drawCursorLabel` (title near cursor, clamped on-screen, truncated >30 chars).

### `DockDragDetector : QObject` (dockoverlay.h:435-496)
Event filter installed on dock `QTabBar`s. On `MouseButtonPress` (LMB) records press pos/tab. On `MouseMove`, once dragged distance >14px (`manhattanLength`), if the pressed tab isn't the sentinel (`​`), find the dock by title and `emit dragStarted(dock, globalPos)`, return true (consume). Clears state on release.

### MainWindow drag glue
- **`setupDockOverlay()`** (main.cpp:3317-3343): create overlay+detector; wire `dragStarted→onDockDragStarted`, `dropRequested→onDockDropRequested`, `dragCancelled→` restore-to-original (re-tabify with `m_dragOrigPeer`, or re-add to `m_dragOrigArea`, or stay floating) + reconcile.
- **`onDockDragStarted(dock, globalPos)`** (main.cpp:3345-3374): set accent; remember `m_dragOrigArea = dockWidgetArea(dock)` and `m_dragOrigPeer` (first non-sentinel tabified peer); **detach the dock by `setFloating(true)` + `hide()`**; `beginDrag`; send a synthetic `MouseMove` to position the cursor.
- **`onDockDropRequested(source, target, zoneInt)`** (main.cpp:3376-3481):
  - `Float` → `setFloating(true)`, move to cursor−(50,10).
  - `Center` → tabify with target if it's a doc dock, else fallback to active/first doc dock, else `addDockWidget(RightDockWidgetArea)` (never silently stays floating).
  - `EdgeLeft/Right/Top/Bottom` → reassign corners so the chosen edge takes full extent, `addDockWidget(area)`, then resize to ¼ of the window (workspace uses `computeWorkspaceDockWidth()`, others `loadDockSize`).
  - Always `reconcileDockTabBars()` at the end.

**Rust/GPUI:** Reimplement drag as a GPUI drag-and-drop with a custom overlay element computing the same drop zones (edges within 36px, center tabify, float). The corner-reassignment is a Qt `QMainWindow` quirk; in GPUI model the dock layout directly (a tree of split regions) so edge drops simply insert into the chosen side.

### `DockSizeReadout : QWidget` (docksizereadout.h)
Inline size tooltip shown while dragging a dock separator. **Child of the main window, NOT a top-level `Qt::ToolTip`** (a layered top-level tooltip on Windows breaks the separator-drag mouse grab — load-bearing comment). `WA_TransparentForMouseEvents`, `WA_ShowWithoutActivating`, `WA_NoSystemBackground`, hidden initially.
- `setTheme(bg, border, title, body, sep)`.
- `updateText(title, body, font)` — no early-return dedup (body changes per pixel); recomputes size.
- `showAt(parentLocalPos)` — position +(12,12) clamped inside parent, raise + repaint every tick.
- `dismiss()`.
- `paintEvent`: rounded rect (radius 6, pad 10, gap 4), bold title + separator line + body. Constants `kRadius=6, kPad=10, kGap=4`.

Driven from `MainWindow::eventFilter` on dock `QEvent::Resize` while LMB is held (main.cpp:3970-3991): compares `oldSize`/`size`, picks the changed axis, calls `showDockSizeTipForAxis`. On `MouseButtonRelease`, `dismiss()`.
- **`showDockSizeTipForAxis(dock, sz, horizontalDrag)`** (main.cpp:3998-3043): builds the readout "selfSz px | otherSz px" where otherSz is probed from `m_docDocks.first()` (or central widget) **deferred via 0-timer** so both sides' geometry has settled. Title = "Workspace/Bookmarks/Symbols/Scanner/Dock size".
- **`showDockSizeTip(dock, sz)`** (main.cpp:4045-4063): legacy area-based axis detection (unreliable for tabified docks) → delegates.

---

## 10. Side-Panel Docks

All four side docks share a chrome pattern: a custom `DockTitleBar` (paint-only `QWidget` set via `setTitleBarWidget`, leaving Qt's native drag/dock intact) containing a `DockGripWidget` + title `QLabel` + close `QToolButton`, plus a per-dock `BorderOverlay`+`ResizeGrip` that show only while floating.

- **`createWorkspaceDock()`** (main.cpp:6974+): `QDockWidget("Project")` objectName `WorkspaceDock`, all areas, Closable|Movable|Floatable. 36px header (`workspaceHeader`). Content = `workspaceContainer` (vbox): top separator, `m_workspaceSearch` (`QLineEdit`, filter.svg leading icon + close.svg clear), bottom separator, `m_workspaceTree` (`QTreeView` + `QStandardItemModel m_workspaceModel` + `QSortFilterProxyModel m_workspaceProxy` + `WorkspaceDelegate`). (Tree model/delegate behavior is the `workspace` subsystem; only the dock chrome is in scope here.)
- **`createScannerDock()`** (main.cpp:7640-7770): `QDockWidget("Memory Scanner")` objectName `ScannerDock`, allowed Bottom|Left|Top. 24px header (`scannerHeader`) with grip/title (`m_scanDockTitle`, elide-on-overflow)/close. Placeholder widget until `ensureScannerPanel()`. minHeight 320; added to Bottom; resized to 360; hidden; installs `this` event filter (for size readout). **Defaults to floating** (720×700 centered) after wiring `topLevelChanged` so the border overlay appears.
- **`ensureScannerPanel()`** (main.cpp:7784-7839): idempotent lazy build of `ScannerPanel`, applies theme/font, `setWidget`, wires provider getter (active tab's provider), bounds getter (struct base+span), and `goToAddress` (rebase active tab).
- **`createSymbolsDock()`** (main.cpp:7841+): idempotent; `QDockWidget("Symbols")` objectName `SymbolsDock`, `UnifiedSymbolPanel` (out of scope content). Built lazily.
- **`createBookmarksDock()`** (main.cpp:8031+): `m_bookmarksDock` with `QListWidget m_bookmarksList` + `QLineEdit m_bookmarksFilter`.

### `placeSidebarDock(dock, area, preferredSize=-1)` (main.cpp:8639-8706)
Single entry for positioning a sidebar: if a visible docked peer already lives in `area`, **tabify** with it; else `addDockWidget(area)` + resize to the remembered size (`loadDockSize`, fallback `preferredSize`; workspace fallback = `computeWorkspaceDockWidth()`, capped to max(280, 35% width)). Resize is deferred via 0-timer (pre-show `resizeDocks` is overridden by Qt's size-hint pass). Guarded by `m_placingSidebar` against `visibilityChanged→placeSidebarDock` recursion. Ends with `reconcileDockTabBars()`.

### Dock size persistence
- `saveDockSize(dock)` (main.cpp:8708-8718): stores width (horizontal areas) or height under `ui/dock.<objectName>.size`.
- `loadDockSize(dock, fallback)` (main.cpp:8720-8725): reads it back.

### `applyLayoutPreset(int preset)` (main.cpp:6955-6969)
Two-mode toggle: `m_workspaceDock->setVisible(preset==Layout_Workspace)`, reconcile, persist `layoutPreset`, status. Other docks untouched.

---

## 11. Status Bar (`createStatusBar`, main.cpp:2054-2130)

Replaces the default with **`FlatStatusBar`** (main.cpp:1986-2052), a `QStatusBar` subclass that bypasses `QStatusBarLayout`'s hardcoded 2px margins by laying out children manually in `resizeEvent`/`showEvent`. `sizeGripEnabled(false)`. `sizeHint` ≈ `max(tabRowH, fontH+6) * 1.15`. `paintEvent`: window fill + 1px top hairline (device-pixel) + optional vertical divider. Children: `ShimmerLabel m_statusLabel`, plus `m_progressLabel`+`m_progressBar` (hidden until a long op), plus a `ResizeGrip` ("resizeGrip", a *direct child of the main window*, repositioned in `resizeEvent`).

### `ShimmerLabel : QWidget` (main.cpp:1869-1979)
Custom-painted status text. `setText(t)` / `setText(t, dimSuffix)` (main text + a dimmed suffix separated by a 1px vertical bar). `setShimmerActive(bool)` toggles a 30ms timer that sweeps a translucent glow band + bright text across the label (used for MCP activity). `onClicked` `std::function` (no Q_OBJECT — inline class) installed to open Goto Address. Colors: `colBase/colDim/colBright/colSep`.

### Status APIs (mainwindow.h:99-110, main.cpp:2132-2204)
- `setAppStatus(text)` / `setAppStatus(text, dimSuffix)`: store + show unless MCP is busy.
- `setMcpStatus(text)`: cancel pending clear, set shimmer on.
- `clearMcpStatus()`: delayed (750ms) restore of the app status + shimmer off.
- `beginProgress(label, total=0)` / `updateProgress(value, label)` / `endProgress()`: right-anchored label+bar (bar 160px, label 220px); total 0 = indeterminate (Qt convention). Bar styled with theme.

**Rust/GPUI:** a status bar element with a left text region (clickable → goto-address), a shimmer animation flag, and an optional right-aligned progress segment. The "dim suffix with vertical separator" layout is parity-relevant.

---

## 12. Small Helper Widgets (main.cpp)

- **`ResizeGrip`** (main.cpp:1699-1747): 16×16 bottom-right grip, 6 dots VS2022-style, `SizeFDiagCursor`; LMB → `startSystemResize(BottomEdge|RightEdge)`. `reposition()` pins to parent corner (pad 4). **Platform-bridged resize.**
- **`DockGripWidget`** (main.cpp:1750-1777): 12px-wide 2×4 dot grid, `SizeAllCursor`.
- **`DockTitleBar`** (main.cpp:1782-1805): paint-only fixed-height title bar; bg fill + optional 1px right border. Used by all sidebar docks.
- **`ViewTabButton`** (main.cpp:1808-1866): custom-painted checkable flat button with a top accent line when checked; used historically as status-bar view toggles (now per-pane tabs).
- **`BorderOverlay`** (main.cpp:829-845): 1px window-edge border, transparent for mouse. Color = borderFocused when active, border when inactive (driven by `changeEvent`/`DockBorderFilter`).
- **`InspectionOverlay`** (main.cpp:847-885): red highlight rect + label for Ctrl+Shift+Click UI inspection.
- **`MinimapScintilla`** + **`MinimapViewportIndicator`** (main.cpp:2218-2272): minimap mirror editor (click/drag → scroll main editor) and a translucent viewport indicator rectangle.
- **`DockBorderFilter`** (main.cpp:2726-2749): repositions/recolors a floating dock's border+grip on Resize/Activate/Deactivate.

---

## 13. Start Page (`startpage.h`)

### `StartPageWidget : QDialog` (startpage.h:19-431)
A single fully-custom-painted VS2022-style welcome dialog (`FramelessWindowHint | Dialog`, opaque paint, mouse tracking). Everything drawn in `paintEvent`; the only child widget is the search `QLineEdit` ("Search recent…", trailing search.svg, max width 330, fixed height 30).

**Signals:** `openProject`, `newClass`, `dismissed`, `importSource`, `importXml`, `importPdb`, `continueClicked`, `fileSelected(path)`.

**Layout constants (single source for paint + hit-test, startpage.h:185-196):** `kLeftMargin=48, kTopMargin=36, kRightMargin=32, kPanelGap=40, kCardPanelW=340, kCardH=84, kEntryH=28, kGroupHeaderH=28, kGroupSpacing=15, kBottomPad=24, kSearchBarH=30, kSearchGap=16`.

**Data:**
- `struct Entry { path, fileName, dirPath; QDateTime lastModified; bool isExample; }`
- `struct Group { name; bool expanded=true; QVector<int> entries; }`
- `enum HZ { HZ_None, HZ_Entry, HZ_Group, HZ_Card, HZ_Continue }` + `struct Hit { HZ zone; int idx; }`

**`loadEntries()`** (startpage.h:217-234): reads `QSettings("Reclass","Reclass").value("recentFiles")` (a `QStringList`), skips missing files, then appends `*.rcx` from `<exeDir>/examples` (mac: `../Resources/examples`) marked `isExample`.

**`buildGroups()`** (startpage.h:236-261): filter by search text (matches fileName or dirPath, lowercased); bucket non-examples by `lastModified.date().daysTo(today)`: Today(0), Yesterday(1), This week(<7), This month (same month+year), Older; examples → "Examples". Build `m_groups` only for non-empty buckets; reset scroll.

**Drawing:** `drawCards` (5 cards — New Class, Open project, Import from Source, Import ReClass XML, Import PDB — each 84px with icon+title+desc, hover fill + 3px accent left bar; a centered "Tutorial →" link below), `drawFileList` (collapsible groups with triangle markers; single-line entry = icon + filename + dim "·" separator + middle-elided dir path + right-anchored date `"M/d/yyyy h:mm AP"`; clipped + scrollable). Border 1px around the whole page.

**Hit testing (`hitTest`)** + interaction: hover updates cursor (PointingHand over a zone); LMB press dispatches: entry→`fileSelected(path)`, group→toggle expand, card 0..4→`newClass`/`openProject`/`importSource`/`importXml`/`importPdb`, continue→`continueClicked`. Wheel scrolls (clamped to `m_maxScroll`).

**Dismiss behavior:** `keyPressEvent(Escape)`→`dismissed`. While shown, installs a `qApp`-level event filter; any `MouseButtonPress` *outside* the page rect → `dismissed` (consume). (Shown non-modally so outside clicks reach the filter.)

### MainWindow integration
- **`showStartPage()`** (main.cpp:9383-9461): if already shown, no-op. **If no tabs exist, preloads `newClass()` behind the splash** so dismissing lands on something. Creates `StartPageWidget`, applies theme, sizes to ~90%×85% of the window (`qBound(900,…,w-20)` × `qBound(560,…,h-20)`), centers it, shows non-modally. Wires all signals: newClass→dismiss (+newClass if not preloaded); dismissed→dismiss (+newClass if no tabs); openProject→dismiss+`openFile`; importSource/Xml/Pdb→dismiss+import; continueClicked→dismiss+close-all+`selfTest`; fileSelected→dismiss+`project_open(path)`; rejected→dismiss.
- **`dismissStartPage()`** (main.cpp:9463-9469): null `m_startPage` first (close may re-enter via `rejected`), then `close()`+`deleteLater()`.

**Rust/GPUI:** a full-window overlay element (not a modal dialog). Recent files from settings + examples dir; bucketed groups; 5 cards; tutorial link; search filter; ESC/outside-click dismiss; the "preload a New Class behind the splash" behavior is parity-relevant. Use `gpui` painting + the same layout constants.

---

## 14. Project Lifecycle

- **`project_new(classKeyword="", forceFreshDoc=false)`** (main.cpp:6260-6368): if an active doc exists and `!forceFreshDoc`, add a new root struct to the *same* doc (`buildEmptyStruct`) and open it in a new tab sharing the doc (copies saved sources). Else create a fresh `RcxDocument`. **Win:** self-attach to a 64KB owned RW buffer at its own address via the `processmemory` plugin so every byte is writable (graceful when plugin missing). **Non-Win (in-scope path):** 256-byte zero buffer at base `0x00400000`. `buildEmptyStruct` then `createTab`.
- **`newClass`/`newStruct`/`newEnum`** = `project_new(class/""/enum, forceFreshDoc=true)`.
- **`buildEmptyStruct(tree, classKeyword)`** (main.cpp:4069+): `enum`→bare Struct node `UnnamedEnumN`; else a `UnnamedClass/StructN` with default hex children. (`s_classCounter` global increments per new class.)
- **`project_open(path="")`** (main.cpp:6370-6515): file dialog if no path (`*.rcx` / All). **Autosave recovery:** if `<path>.autosave` is newer, prompt to restore (`ThemedMessageBox::confirm`); if restored, load the shadow but reset `doc->filePath` to the real path + mark modified. Detect XML by first 64 bytes (`<?xml`/`<ReClass`) → `importReclassXml`. Otherwise `doc->load` (with `beginProgress` for large files; >5000 nodes shows "Composing…"). Closes all existing tabs under a `ClosingGuard` before creating the new tab. Reports class/node counts + sibling-overlap count in the status bar. `addRecentFile`.
- **`project_save(dock=nullptr, saveAs=false)`** (main.cpp:6517-6542): default to active dock. If saveAs or no path, file dialog (`*.rcx`/`*.json`); else save to existing path. Removes the `.autosave` shadow. `addRecentFile`, `updateWindowTitle`, `rebuildWorkspaceModel`.
- **`project_close(dock=nullptr)`** = `dock->close()`.
- **`closeAllDocDocks()`**: close every dock (copy first since the destroyed signal mutates `m_docDocks`).
- **`autosaveAllModifiedDocs()`**: 60s timer; writes `<path>.autosave` shadows for modified docs with a known path.
- **`addRecentFile`/`updateRecentFilesMenu`/`populateSourceMenu`**: recent-files list in `QSettings("recentFiles")`.

---

## 15. Window-Level Behaviors

- **`updateWindowTitle()`** (main.cpp:5366-5385): mac → always "Reclass"; else `<rootName>[ *] - Reclass` from the active tab's view root. Always calls `updateScannerTitle()`.
- **`updateScannerTitle()`** (main.cpp:5387-5429): the scanner dock title follows the active tab's provider: "Memory Scanner — <name> (<kind>)" or "— no source on active tab"; sets dock window title + a tooltip explaining the follow-active-tab behavior.
- **`changeEvent`** (main.cpp:8924-8953): `ActivationChange` → border color (focused/unfocused) + dismiss editor popups on deactivate; `WindowStateChange` → titlebar maximize icon; either → drive controllers' adaptive refresh via `setWindowState(focused, visible)`; re-raise border overlay.
- **`resizeEvent`** (main.cpp:8955-8966): resize+raise border overlay; reposition+raise resize grip.
- **`updateBorderColor(color)`**: set + update `BorderOverlay`.
- **`closeEvent`** (main.cpp:8973-9040): collect unique dirty docs + their root-struct labels (`collectDirtyDocLabels` = file name if saved, else all root struct names). Show `ThemedMessageBox::unsavedChanges` with one of three wordings (1 doc/1 class, 1 doc/N classes, N docs). Cancel→ignore; Save→save each dirty doc (abort if any save fails); Discard→accept.
- **`eventFilter`** (main.cpp:3847-3996): global filter handling **F12** screenshot (grab the screen rect the window occupies → `<exeDir>/issue.png`), **Ctrl+Shift+Click** UI inspection (`inspectAt` → overlay + status), sentinel "+" tab clicks, middle-click tab close, and dock-resize live size readout.

---

## 16. Active-Tab Accessors & View Mode

- `activeController()` / `activeTab()` / `tabByIndex(i)` / `tabCount()` (main.cpp:5346-5364) — look up via `m_activeDocDock` / `m_docDocks`.
- `findActiveSplitPane()` / `activePaneEditor()` / `findPaneByTabWidget(tw)` (main.cpp:2703-2723).
- `createSplitPane(tab)` (main.cpp:2274-2701): builds a `QTabWidget` (South tabs, documentMode) with 3 tabs — **"Reclass"** (editor+minimap container, index 0), **"Code"** (rendered Scintilla + Ctrl+F find bar, index 1), **"Debug"** (plain Scintilla, index 2) — plus a hidden bottom-right corner widget (`fmtCombo` of `CodeFormat`, `scopeCombo` of `CodeScope`, `fmtGear`→Options page 2) shown only on the Code tab. `currentChanged` sets the pane's `viewMode`, shows/hides the corner widget, and regenerates rendered/debug views. Wires minimap click-to-scroll + viewport indicator sync (`documentApplied`, `SCN_UPDATEUI`). Settings persisted: `codeFormat`, `codeScope`.
- `splitView()` (main.cpp:4453-4464): flip the splitter to `Vertical` and append a new pane (stacked below). `unsplitView()`: drop the last pane.
- `setViewMode`/`syncViewButtons`/`setupRenderedSci`/`setupDebugSci`/`updateRenderedView`/`updateDebugView` etc. — view rendering plumbing (mostly the editor/codegen subsystems; the shell just hosts them).

---

## 17. Qt → Rust/Crate Mapping

| Qt construct | Role here | Rust/GPUI equivalent |
|---|---|---|
| `QApplication`/`DarkApp` | app + event loop | `gpui::App` / `Application::new().run` |
| `QMainWindow` + dock areas | window + docking | a GPUI `Window` rendering a custom dock-layout tree |
| `QDockWidget` (doc/sidebar) | tabbed docs + panels | GPUI dock/panel elements; doc tabs = a `Vec` rendered as a tab strip |
| sentinel `​` dock | keep tab strip + "+" tab | render a persistent "+" tab element directly (no hack needed) |
| `QTabBar` + `MenuBarStyle` paint | doc tab strip | custom-rendered GPUI tab strip (icons, ✕, elide, accent) |
| `QMenuBar`/`QMenu`/`QAction` | menus + shortcuts | a menu/action registry; GPUI menus or a custom menu bar; `KeyBinding`s |
| `QProxyStyle` (MenuBarStyle) | all chrome painting | direct GPUI render code per widget |
| `QPalette`/`applyGlobalTheme` | theme→roles | a `Theme` struct read directly |
| `QSettings("Reclass","Reclass")` | persisted prefs/recent/dock sizes | `directories`/`serde` config file (e.g. `~/.config/reclass` or platform dir); keys preserved |
| `QSvgRenderer`/`QIcon` | vsicons SVGs | `gpui` SVG rendering of the same `:/vsicons/*.svg` assets |
| `QSplitter` | per-tab pane split | GPUI resizable split element |
| `QTimer::singleShot`/`QTimer` | deferral, debounce, autosave, shimmer | GPUI `cx.spawn`/timers / `Instant`-driven ticks |
| `QPointer` | safe dock pointers | weak handles / `Entity` ids |
| `startSystemMove`/`startSystemResize` | native window drag/resize | GPUI window drag region / platform `#[cfg]` |
| DWM dark titlebar / NSWindow appearance | OS chrome tint | `#[cfg(windows)]` DWM call / `#[cfg(macos)]` (or rely on GPUI dark default) |
| `MiniDumpWriteDump` / `backtrace` | crash dumps | `std::panic` hook + `backtrace` crate (optional) |
| `QStandardItemModel`/`QTreeView` | workspace tree | (workspace subsystem) GPUI list/tree |
| `QsciScintilla` | code/editor views | (editor subsystem) — out of this map |

---

## 18. Platform-Specific Code Summary

- **Windows:** frameless flags + `DwmExtendFrameIntoClientArea`; `setDarkTitleBar` DWM hint; `MenuBarStyle` `CS_DROPSHADOW` strip + frameless translucent menus; `crashHandler`/minidump; "Relaunch as Administrator" (token elevation check + `ShellExecuteExW "runas"`); `project_new` self-attach to owned buffer via `processmemory`.
- **Linux:** `showMaximized()` at startup; titlebar uses tool-button-mirrored menus (`m_useToolButtons`); `installPosixCrashHandler`.
- **macOS:** native titlebar + native menu bar; `AA_DontUseNativeDialogs`; `setUnifiedTitleAndToolBarOnMac`; `applyMacTitleBarTheme` (NSWindow appearance); examples path `../Resources/examples`; `installPosixCrashHandler`.

Only Linux compile-verifies here; keep all OS-specific code behind `#[cfg(...)]`.

---

## 19. Concurrency / Threading
The shell is single-threaded on the Qt GUI/event-loop thread. "Async" is via `QTimer::singleShot(0,…)` deferrals (post-paint init, debounced workspace rebuilds, dock-size readout settling, reconcile races) and timers (autosave 60s, MCP-status clear 750ms, shimmer 30ms). No worker threads in this subsystem (scanner/MCP own theirs, out of scope). Rust: GPUI runs on the main thread; deferrals → `cx.spawn`/`cx.on_next_frame`; timers → interval tasks.

---

## 20. Subtle Behaviors Tests Rely On
- **Sentinel tab text is exactly `​`** (zero-width space) — used by the style (32×37 size), hit-testing, drag detector, and "+" click. Tests sample the rendered tab pixmap.
- **`drawTabSourceIcon` must set devicePixelRatio BEFORE attaching the painter** (tab_source_icon.h) — tests grab the tab pixmap and sample pixels; live + test renders must match.
- **Dock-size readout fires on `QEvent::Resize` while LMB held** (`test_dock_size_tip` notes Qt 6.5 MinGW doesn't change `overrideCursor` during separator drags; 10 mouse moves → 10 resize events). The LMB gate excludes programmatic resizes.
- **`reconcileDockTabBars` is idempotent + re-entry-guarded** — repeated calls during cascading layout signals must converge to one pass.
- **Closing the last tab (not during a batch close) auto-creates a fresh class** via `project_new()` so the window is never blank.
- **Source-icon properties live on the dock object** (`rcxSourceIcon`/`rcxSourceLive`), not a tab index, so they survive reorder/tabify.
- **`showStartPage` preloads a New Class behind the splash** only when no tabs exist; dismiss paths recreate a class if the session ends up empty.
- Edge drop zones only activate when the dragged dock's `allowedAreas()` allows that side; Center only tabifies with `DocDock_*` targets (never sidebars).
