export const meta = {
  name: 'reclass-ui-zed-redesign',
  description: 'Redesign the Reclass GPUI UI to Zed-level aesthetics with reclass FEATURE parity (incl. a working navbar/menu bar); verify with real headless screenshots.',
  whenToUse: 'Restyle every (mostly-stubbed) UI surface to match Zed visually + reclass functionally, with screenshot verification.',
  phases: [
    { title: 'Foundation', detail: 'Zed design system: shared tokens (src/ui/design.rs), theme tuned to Zed One Dark, helpers, pre-declare new module stubs' },
    { title: 'Surfaces', detail: '8 parallel surface redesigns (disjoint files): chrome/navbar, workspace, editor, scanner, tabs, startpage, menus/popovers, dialogs' },
    { title: 'Integrate', detail: 'authoritative green build + fmt + commit' },
    { title: 'Verify', detail: 'launch headless (Xvfb), screenshot every state, crop + vision-compare vs Zed & reclass refs -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per surface in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, assessment' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const CPP = '/home/loke/reclass-cpp'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`
const REF = `${CPP}/docs` // README_PIC1..6.png — the reclass feature reference

// Reference legend the agents should Read (vision):
//   PIC1 windows: top MENU BAR (Reclass File Edit View Tools Plugins Help) + min/max/close; editor tabs; vtable expansion w/ fnptr64 nodes, green // comments, orange/red changed hex; "Previous Values" hover.
//   PIC2 dark: left "Project — N structs" tree (struct icons + field counts, chevrons, filter); memory grid w/ nested expandable structs; bottom Reclass|C/C++ tabs.
//   PIC3 split: left memory view + right generated C/C++ (line numbers, syntax highlight); bottom SCANNER panel (Signature dropdown, Pattern input, Executable/Writable/Current-Struct checks, Scan/Re-scan, results table, "540 results", Go to Address / Copy Address); status bar.
//   PIC4 source picker dropdown: File / Kernel Memory / Process Memory / ReClass.NET / Remote / WinDbg (icons) + recent + checkmarked selection + separators + Clear All.
//   PIC5 main: top menu bar + window controls; left Project panel w/ filter + struct row + count; editor class header "[>] source v 0x400000 class Name {", offset margin (+0/+8...), hex64 rows, ASCII, hex bytes, footer "}; +10h +100h +1000h Trim"; "Base Address" help popup; bottom Reclass|Code tabs; status bar "UnnamedClass0.field_20  +0x20".
//   PIC6 counter: Project panel; editor; bottom Scanner (Value/int32/Exact Value/Value:40, checks, results, "1 of 2 results match", Go to/Copy Address).

const COMMON =
  `You are restyling ONE surface of the GPUI UI of "Reclass" — a faithful Rust+GPUI port of the struct-layout editor. Single cargo package "reclass" at ${OUT}; C++ source at ${CPP}/src. The whole logic layer (core/compose/controller/scanner/format/generator/theme/...) is DONE and tested — build the UI on top; do NOT change logic modules (at most add a tiny pub accessor, and say so).\n\n` +
  `AESTHETIC REFERENCE = ZED (github.com/zed-industries/zed). Match Zed's visual language precisely: One Dark palette, comfortable typography (UI ~14px, generous line-height; editor a real monospace), 4px spacing grid, subtle 1px borders, gentle rounded corners (4-6px) on elevated surfaces, restrained shadows, low-contrast chrome with content-forward color, hover states that lighten by a few % of an overlay, selected rows with a soft accent-tinted background (NOT a hard fill), muted secondary text. Buttons/inputs/menus/tabs/list-rows should look like Zed's. Use the shared design tokens (see below) and theme tokens (cx.theme().*) — NEVER hardcode ad-hoc hex; pull from the token module / theme so it stays themeable.\n\n` +
  `FEATURE REFERENCE = the reclass screenshots in ${REF}/README_PIC1.png .. README_PIC6.png — READ the ones listed for your surface (you have vision) and reproduce the FEATURES they show, in Zed styling. Also consult the C++ at ${CPP}/src and the understand maps under ${OUT}/_design/understand/.\n\n` +
  `SHARED DESIGN SYSTEM: the Foundation stage already committed ${OUT}/src/ui/design.rs (Zed tokens + small render helpers) and ${OUT}/_design/zed_ui_spec.md (the written spec). READ BOTH FIRST and build on them. All new ui submodules are already declared in src/ui/mod.rs — do NOT edit mod.rs.\n\n` +
  `gpui know-how: ${OUT}/_design/gpui_component_cookbook.md (gpui-component widgets/APIs) and ${OUT}/_design/gpui_cookbook.md (raw-gpui custom Element render/layout/paint/hit-test, uniform_list, text). gpui-component gives Button/Input/List/Menu/Dropdown/Tab/Table/Tooltip/Modal etc.\n\n` +
  `BUILD (gpui is cached -> incremental is fast): \`${BUILD}\`. OTHER agents are editing OTHER files concurrently; if the ONLY errors are in files you do not own, that is expected churn — make sure YOUR files compile clean and your code is correct. Keep the full build green by the end of your work.\n\n` +
  `RULES: (1) Edit ONLY the files in your ownership list. Do NOT touch src/ui/mod.rs, other surfaces' files, or logic modules. (2) Do NOT run \`git add\`/\`git commit\`/\`cargo fmt\` — a later serial stage does that. (3) Preserve existing behaviour/wiring (events, controller calls, key bindings, #[cfg(test)] tests) — you are RESTYLING + filling feature gaps, not rewriting logic. Keep/extend unit tests for any pure helpers you add. (4) Faithful to reclass features, beautiful like Zed.\n\n`

const SURFACE_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    surface: { type: 'string' },
    compiles: { type: 'boolean' },
    files: { type: 'array', items: { type: 'string' } },
    features_added: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' },
  },
  required: ['surface', 'compiles', 'files', 'notes'],
}

// ──────────────────────────────────────────────────────────────────────────
// The 8 surfaces — disjoint file ownership so they redesign in parallel.
// ──────────────────────────────────────────────────────────────────────────
const SURFACES = [
  {
    key: 'chrome',
    label: 'chrome/navbar',
    files: ['src/ui/window.rs', 'src/ui/titlebar.rs', 'src/ui/menubar.rs', 'src/ui/statusbar.rs'],
    refs: ['README_PIC1.png', 'README_PIC5.png', 'README_PIC3.png'],
    task:
      `SURFACE: APP CHROME — the top navbar/menu bar, the titlebar, the window shell layout, and the bottom status bar. THIS IS THE HEADLINE: a WORKING NAVBAR.\n` +
      `FILES YOU OWN: src/ui/window.rs (shell layout/render), src/ui/titlebar.rs, src/ui/menubar.rs (NEW — currently an empty stub), src/ui/statusbar.rs (NEW — empty stub).\n` +
      `DELIVER:\n` +
      `1) A REAL MENU BAR (the navbar) — reclass PIC1/PIC5 show: "Reclass  File  Edit  View  Tools  Plugins  Help". Implement it in menubar.rs as a Zed-style horizontal menu: each top-level title is a clickable button that opens a Zed-style dropdown menu (rounded elevated surface, inset rows, hover highlight, separators, optional left icon + right keybinding hint). The command/menu tree already exists for the command palette (read src/ui/commandpalette.rs to find the menu/command model) — REUSE it to populate the menus (File: New Class/Open/Save/Import…; Edit: Undo/Redo/Cut/Copy/Paste/Delete; View: toggle workspace/scanner, themes, Tree vs C/C++; Tools; Plugins; Help: About). Items may dispatch to existing actions / the command-palette command handler (window.rs run_menu_command) — wire what exists, stub the rest gracefully (menu must OPEN, show items, highlight, and CLOSE on select/escape). Use gpui-component's popup/menu or a custom anchored overlay per the cookbook.\n` +
      `2) Integrate the menu bar into the TitleBar row (PIC1/PIC5 layout: app label · menu bar · stretch · view-mode toggle · sidebar toggle · window min/max/close). Keep the existing LayoutPreset sidebar toggle + ViewMode toggle but restyle them to Zed (ghost icon buttons, subtle active state). gpui-component TitleBar already provides min/max/close — ensure they read as Zed controls.\n` +
      `3) A BOTTOM STATUS BAR (statusbar.rs) like Zed's: a thin bar at the very bottom of the window showing the active node context — reclass PIC5 shows "UnnamedClass0.field_20  +0x20"; PIC3 shows "FuncPtr64 qt_metacast  offset: 0x0010  size: 8 bytes". Pull the selection/active-node info from AppState/the active editor's controller (read what window.rs already has access to; add a read-only accessor on the editor/controller if truly needed and note it). Left = node path/name; center/right = offset + size + maybe theme/source. Muted Zed styling, 1px top border.\n` +
      `4) Overall shell: ensure window.rs render lays out titlebar(+menubar) / dock area / status bar as a clean Zed column with correct backgrounds (chrome darker, content area editor-bg), no overlap, and the start-page overlay still works.\n` +
      `Read PIC1, PIC5, PIC3. Match Zed's title bar + status bar from its source (crates/title_bar, crates/workspace status bar). Keep all existing window.rs wiring (events, palette, open_project, theme) intact.`,
  },
  {
    key: 'workspace',
    label: 'workspace panel',
    files: ['src/ui/workspace.rs'],
    refs: ['README_PIC2.png', 'README_PIC5.png', 'README_PIC6.png'],
    task:
      `SURFACE: THE PROJECT / WORKSPACE LEFT PANEL (src/ui/workspace.rs ONLY).\n` +
      `reclass PIC2/PIC5/PIC6 show a left dock titled "Project — N structs" with an × close, a "Filter types…" / "Search…" input with a magnifier icon, then a TREE of types: each row = a type icon (S for struct / box glyph) + the type name + a right-aligned field/member COUNT, with expand chevrons for nesting, indent guides, hover highlight, and a selected-row accent.\n` +
      `Restyle to Zed's PROJECT/OUTLINE PANEL: panel header (small, uppercase-ish muted title + count + actions on the right), a clean search input (Zed input: subtle bg, focus ring), virtualized/uniform list rows with comfortable height (~22-24px), left type icon, truncated name, muted trailing count, chevron disclosure, indent guides, hover = subtle overlay, selected = soft accent-tinted bg + slightly brighter text. Keep ALL existing model/build/nav wiring (WorkspaceModel, WorkspaceNav events, filter logic, quick-navigation). This panel must look like it belongs in Zed.\n` +
      `Read PIC2, PIC5, PIC6, and ${OUT}/_design/understand/app-shell.md (§10 workspace).`,
  },
  {
    key: 'editor',
    label: 'editor surface',
    files: ['src/ui/editor/element.rs', 'src/ui/editor/mod.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/palette.rs', 'src/ui/editor/inline_edit.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/tab_cycle.rs'],
    refs: ['README_PIC1.png', 'README_PIC2.png', 'README_PIC4.png', 'README_PIC5.png'],
    task:
      `SURFACE: THE CORE MEMORY EDITOR GRID — the heart of reclass (src/ui/editor/* ONLY). This is the most important surface; spend the most effort here.\n` +
      `It is a bespoke raw-gpui Element that paints rows produced by the compose module (text + per-span style/color, columns, fold markers, hex/ASCII, per-byte change highlighting). READ src/ui/editor/element.rs, mod.rs, geometry.rs, palette.rs first, and ${OUT}/_design/understand/editor-surface.md, and the C++ ${CPP}/src/editor.cpp.\n` +
      `reclass features to reproduce faithfully (PIC1/PIC2/PIC4/PIC5):\n` +
      `  • A CLASS HEADER row: "[>] source v   0x400000   class TypeName {" — a fold caret, a clickable SOURCE selector chip ("source"/'Reclass.exe' with a dropdown caret), the base ADDRESS (clickable), the "class"/"struct" keyword, the type name, and "{".\n` +
      `  • An OFFSET MARGIN (left gutter) on every row: "+0 +8 +10 +18 …" in a muted blue-gray, fixed-width, Zed-gutter styling (low contrast, right-aligned).\n` +
      `  • Node rows: tree connector lines + node-type token (hex64 / fnptr64 / uint32_t / void* …) colored by kind, field NAME, an ASCII preview column ("........"), then the HEX BYTES ("00 00 00 00 …"). PER-BYTE CHANGE HIGHLIGHTING: bytes that changed since last refresh render in orange/red (and recently-same fade) — keep this behaviour, just use Zed One Dark hues. Trailing "// comment" / "// Reclass.exe+0x284c0" in green.\n` +
      `  • Nested expandable structs/arrays/pointers with indentation + connector lines; fold/expand carets.\n` +
      `  • FOOTER row: "};   +10h  +100h  +1000h   Trim" — the add-bytes / trim controls as subtle Zed buttons.\n` +
      `  • Active-row highlight + multi-selection (soft accent bg), inline-edit overlays aligned past the offset margin.\n` +
      `ZED styling: comfortable monospace at a readable size (the current build is too cramped — increase to ~13-14px with proper line-height ~1.5), One Dark syntax colors mapped to node kinds (types ~ #e5c07b yellow, function-ptrs ~ #61afef blue, keywords ~ #c678dd purple, numbers/addresses ~ #d19a66 orange, comments ~ #5c6370 green-gray, strings/ASCII ~ #98c379), a Zed editor background, a muted gutter, soft active-line, gentle selection. Map compose's semantic span kinds -> these colors in palette.rs. Keep ALL hit-testing/inline-edit/selection/fold/tab-cycle behaviour and tests — RESTYLE + add the header/footer/source-chip/offset-margin chrome the screenshots show if missing.\n` +
      `Read PIC1, PIC2, PIC4, PIC5.`,
  },
  {
    key: 'scanner',
    label: 'scanner panel',
    files: ['src/ui/scannerpanel.rs'],
    refs: ['README_PIC3.png', 'README_PIC6.png'],
    task:
      `SURFACE: THE SCANNER BOTTOM PANEL (src/ui/scannerpanel.rs ONLY).\n` +
      `reclass PIC3/PIC6 show a docked "Scanner" panel: a header "Scanner" + × ; a SCAN-TYPE dropdown (e.g. ".* Signature ▾" or "[M] Value"); secondary controls ("Type: int32 ▾", "Scan: Exact Value ▾"); a "Pattern:"/"Value:" input; checkboxes "✓ Executable  Writable  Current Struct"; "Scan" + "Re-scan" buttons (with magnifier / refresh icons); a RESULTS TABLE (address column + hex/value columns); a result-count line ("540 results" / "1 of 2 results match"); and footer buttons "→ Go to Address" + "📋 Copy Address".\n` +
      `Restyle to ZED: a clean bottom panel with a Zed toolbar row (segmented/dropdown controls, compact inputs with subtle bg + focus ring, Zed checkboxes, primary "Scan" button + secondary "Re-scan"), a virtualized results table with Zed table styling (muted header, 1px row separators or zebra-free hover rows, monospace addresses, right-aligned numbers), the count as muted status text, and Zed footer buttons. Keep ALL existing wiring to the scanner module / provider / results model. Use gpui-component Table/Input/Button/Checkbox/Dropdown.\n` +
      `Read PIC3, PIC6, and ${OUT}/_design/understand/scanner.md.`,
  },
  {
    key: 'tabs',
    label: 'tabs + view toggle',
    files: ['src/ui/tabs.rs'],
    refs: ['README_PIC1.png', 'README_PIC5.png', 'README_PIC2.png'],
    task:
      `SURFACE: THE DOCUMENT TAB STRIP + VIEW-MODE TOGGLE (src/ui/tabs.rs ONLY).\n` +
      `reclass PIC1 shows editor tabs ("RcxEditor" active | "Unnamed") each with a close ×; PIC5/PIC2 show a bottom "Reclass | Code" / "Reclass | C/C++" view-mode toggle (tree view vs generated C/C++).\n` +
      `Restyle to ZED's TAB BAR: a tab strip with active-tab indicator (Zed uses a subtly elevated active tab against a darker tab-bar bg, a 1-2px top/bottom accent or brighter bg), per-tab source icon + title + modified dot, a close × that appears on hover, comfortable padding, and a "+" new-tab affordance. Style the view-mode toggle as a Zed segmented control / the bottom inline tabs ("Reclass" | "Code"). Keep ALL existing DocumentArea/DocAreaEvent/ViewMode wiring + tests.\n` +
      `Read PIC1, PIC5, PIC2, and ${OUT}/_design/understand/app-shell.md (§8 tabs).`,
  },
  {
    key: 'startpage',
    label: 'start page',
    files: ['src/ui/startpage.rs'],
    refs: ['README_PIC5.png'],
    task:
      `SURFACE: THE START / WELCOME PAGE (src/ui/startpage.rs ONLY).\n` +
      `The current start page (you can see it on launch) is plain: a big "Reclass" title, "Open recent" with a search box + "No recent files", and a "Get started" list (New Class / Open project / Import from Source / Import ReClass XML / Import PDB / Tutorial). Restyle it to look like ZED'S WELCOME TAB: centered, well-proportioned content, a clear hierarchy, the action list as Zed list-rows (icon + title + muted subtitle + right-aligned keybinding hint where applicable), a recent-files list with Zed styling, comfortable spacing, subtle dividers, and the same restrained palette. Keep ALL existing StartPageEvent/StartCard wiring + key bindings + tests. Make it feel like a polished Zed onboarding surface.\n` +
      `Read PIC5 for the overall app palette to stay consistent.`,
  },
  {
    key: 'menus',
    label: 'menus & popovers',
    files: ['src/ui/commandpalette.rs', 'src/ui/contextmenu.rs', 'src/ui/tooltip.rs', 'src/ui/sourcechooser.rs', 'src/ui/hextoolbar.rs', 'src/ui/typeselectorpopup.rs', 'src/ui/enumpicker.rs'],
    refs: ['README_PIC4.png', 'README_PIC5.png', 'README_PIC1.png'],
    task:
      `SURFACE: MENUS & POPOVERS — the floating/anchored surfaces (your files ONLY): commandpalette.rs, contextmenu.rs, tooltip.rs, sourcechooser.rs, hextoolbar.rs, typeselectorpopup.rs, enumpicker.rs.\n` +
      `Make every one a ZED-quality floating surface: rounded (6px) ELEVATED surface, 1px border, soft shadow, inset rows, hover highlight (subtle overlay), selected row soft-accent bg, separators between groups, optional left icon + right keybind/checkmark slot, muted secondary text.\n` +
      `Targets:\n` +
      `  • commandpalette.rs — already partly Zed-styled; make it pixel-clean (input header, scrollable list, fuzzy-match highlight on matched chars, selected row, key-cap end slots). Light touch but verify it matches Zed's command palette.\n` +
      `  • sourcechooser.rs — reclass PIC4 source dropdown: plugin list (File / Kernel Memory / Process Memory / ReClass.NET Compat / Remote Process Memory / WinDbg) each w/ a left icon + plugin filename hint, a separator, recent sources, a CHECKMARK on the selected source, a separator, "Clear All". Zed dropdown-menu styling.\n` +
      `  • contextmenu.rs — Zed right-click menu (icons, separators, keybind hints, danger items, submenus).\n` +
      `  • tooltip.rs — reclass PIC5 "Base Address" help popover + PIC1 "Previous Values" history popover: Zed hover-popover styling (elevated, padded, title + body, monospace where values).\n` +
      `  • typeselectorpopup.rs / enumpicker.rs — Zed picker popovers (search + list).\n` +
      `  • hextoolbar.rs — the hex-edit toolbar popover, Zed-styled.\n` +
      `Keep ALL existing wiring (fuzzy scorer, events, selection, callbacks) + tests. Read PIC4, PIC5, PIC1.`,
  },
  {
    key: 'dialogs',
    label: 'modal dialogs',
    files: ['src/ui/dialogs.rs', 'src/ui/optionsdialog.rs', 'src/ui/gotoaddress.rs', 'src/ui/processpicker.rs', 'src/ui/findbar.rs', 'src/ui/messagebox.rs'],
    refs: ['README_PIC5.png', 'README_PIC3.png'],
    task:
      `SURFACE: MODAL DIALOGS & BARS (your files ONLY): dialogs.rs, optionsdialog.rs, gotoaddress.rs, processpicker.rs, findbar.rs, messagebox.rs.\n` +
      `Make every modal a ZED MODAL: a centered elevated card on a dim backdrop, a header (title + optional close), a comfortable body (Zed inputs/labels/sections/checkboxes/dropdowns), and a footer button row (primary + secondary, right-aligned). Bars (findbar) = a Zed inline bar (compact input + match count + nav + close).\n` +
      `Targets:\n` +
      `  • optionsdialog.rs — the settings/options dialog: Zed settings styling (sections, rows of label + control, tabs/categories if present).\n` +
      `  • gotoaddress.rs — goto-address dialog: a single labeled input + OK/Cancel; reclass PIC5 shows the related "Base Address" help (a styled popover listing address formats: hex address / module base / module+offset / follow pointer / PDB symbol / operators). Make the input + help look Zed-clean.\n` +
      `  • processpicker.rs — process picker: a Zed modal with a search + a table/list of processes (available sources + clearly-labeled stub entries) + select/cancel.\n` +
      `  • findbar.rs — Ctrl+F find bar: compact Zed search bar.\n` +
      `  • messagebox.rs — themed message/confirm dialog (icon + text + buttons).\n` +
      `  • dialogs.rs — the shared modal scaffolding/seam: make it provide a reusable Zed modal frame the others use.\n` +
      `Keep ALL existing wiring (events, controller hooks, validation) + tests. Read PIC5, PIC3 and ${OUT}/_design/understand/widgets-dialogs.md.`,
  },
]

// ──────────────────────────────────────────────────────────────────────────
// PHASE 1 — FOUNDATION (single serial agent): the Zed design system + tokens +
// theme tune + module stubs + green build + commit. Everything else builds on it.
// ──────────────────────────────────────────────────────────────────────────
phase('Foundation')
const foundation = await agent(
  `You are establishing the shared ZED DESIGN SYSTEM for the GPUI UI of the Reclass Rust port (package "reclass" at ${OUT}). AESTHETIC TARGET = ZED (github.com/zed-industries/zed): One Dark palette, comfortable ~14px UI type + real monospace editor font, 4px spacing grid, subtle 1px borders, 4-6px radii on elevated surfaces, restrained shadows, low-contrast content-forward chrome, hover-overlay + soft-accent-selected states.\n\n` +
  `READ FIRST: ${OUT}/src/ui/theme_apply.rs, ${OUT}/src/theme/{model.rs,color.rs,manager.rs,mod.rs,editor.rs}, ${OUT}/_design/understand/themes.md, ${OUT}/_design/gpui_component_cookbook.md (how gpui-component Theme/ThemeColor works), ${OUT}/_oracle/fixtures/themes/*.json (the 8 bundled themes). You MAY WebFetch Zed's One Dark theme JSON (e.g. raw github zed-industries/zed assets/themes/one/one.json) for exact hex; if fetch fails use the well-known One Dark values.\n\n` +
  `DELIVER (commit when green):\n` +
  `1) ${OUT}/src/ui/design.rs (NEW — currently does not exist; you will create it AND add \`pub mod design;\` to src/ui/mod.rs): a small module of ZED DESIGN TOKENS + render helpers that every surface uses. Include: a spacing scale (px helpers), radius/border/shadow tokens, a type scale (ui_xs/sm/md, editor size + line-height, font families with sensible Linux fallbacks — a real monospace), semantic color accessors that READ from cx.theme() (so it stays themeable) for: window/chrome bg, panel bg, elevated/popover surface, content/editor bg, border, hover-overlay, selected-accent bg, text/muted/disabled text, accent, and the editor SYNTAX kinds (keyword/type/function/number/string/comment/punctuation/address). Plus tiny reusable builders, e.g. panel_header(title), zed_list_row(), elevated_surface(), section_label() — whatever keeps surfaces DRY and consistent. Keep it compiling and ergonomic; document each token briefly.\n` +
  `2) Tune the DEFAULT THEME to read like Zed One Dark: ensure the theme the app loads on launch maps onto gpui-component's Theme with Zed One Dark-ish colors (bg ~ #282c33 content / darker chrome, text ~ #c8ccd4, muted ~ #828997, border ~ #3b414d, accent/blue ~ #61afef, selection soft). Prefer doing this via theme_apply.rs and/or by adding a bundled "Zed One Dark"-style theme that is selected by default — WITHOUT breaking \`cargo test --no-default-features\` (do NOT change existing fixture JSON in ways that break theme tests; if you add a theme, add it cleanly). Verify: \`cargo test --no-default-features theme\` stays green if you touch theme code.\n` +
  `3) PRE-DECLARE the new ui submodules so the parallel surface stage never edits mod.rs: create EMPTY stub files ${OUT}/src/ui/menubar.rs and ${OUT}/src/ui/statusbar.rs (each: a doc comment + minimal compiling content, e.g. an empty pub fn or just the module doc), and add \`pub mod design;\`, \`pub mod menubar;\`, \`pub mod statusbar;\` to src/ui/mod.rs. (design.rs has real content; menubar/statusbar are stubs the chrome surface fills next.)\n` +
  `4) WRITE the spec ${OUT}/_design/zed_ui_spec.md — the authoritative written design language: the exact token values, the Zed→reclass mapping per surface (chrome/navbar, workspace, editor grid, scanner, tabs, startpage, menus/popovers, dialogs), typography, spacing, component recipes (button/input/list-row/panel-header/tab/dropdown/tooltip/modal/table/status-bar), and the editor syntax-color map. Every surface agent will READ this; make it concrete and prescriptive.\n\n` +
  `VERIFY: \`${BUILD}\` is green and the window still constructs (don't break window.rs wiring — you are adding design.rs + stubs + theme tune only; do NOT restyle surfaces yet). Run \`cargo fmt\`. Commit: \`cd ${OUT} && git add -A && git commit -m "ui: Zed design system — tokens (design.rs), One Dark theme tune, module stubs, spec"\`. Report committed=<hash>.`,
  {
    label: 'foundation', phase: 'Foundation',
    schema: {
      type: 'object', additionalProperties: false,
      properties: {
        green: { type: 'boolean' }, committed: { type: 'string' },
        tokens_summary: { type: 'string' }, new_modules: { type: 'array', items: { type: 'string' } },
        notes: { type: 'string' },
      },
      required: ['green', 'notes'],
    },
  })
log(`Foundation: ${foundation ? (foundation.green ? 'green' : 'NOT green') : 'FAILED'}${foundation && foundation.committed ? ' @' + foundation.committed : ''}`)

// ──────────────────────────────────────────────────────────────────────────
// PHASE 2 — SURFACES (parallel, disjoint files). Each redesigns+implements its
// surface on top of the committed design system. No commits here.
// ──────────────────────────────────────────────────────────────────────────
phase('Surfaces')
log(`Redesigning ${SURFACES.length} surfaces in parallel (disjoint file ownership).`)
const surfaceResults = await parallel(SURFACES.map(s => () =>
  agent(
    COMMON +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `Read these reclass reference screenshots (vision): ${s.refs.map(r => `${REF}/${r}`).join(', ')}.\n\n` +
    s.task +
    `\n\nWhen done, ensure your files compile (\`${BUILD}\`) and return the structured result.`,
    { label: `ui:${s.key}`, phase: 'Surfaces', schema: SURFACE_SCHEMA },
  )
))
const okSurfaces = surfaceResults.filter(Boolean)
log(`Surfaces done: ${okSurfaces.map(r => `${r.surface}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ──────────────────────────────────────────────────────────────────────────
// PHASE 3 — INTEGRATE (single serial agent): authoritative green build, fix any
// cross-surface glue, fmt, commit.
// ──────────────────────────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `8 surface agents just restyled the Reclass GPUI UI in parallel (chrome/navbar, workspace, editor grid, scanner, tabs, startpage, menus/popovers, dialogs) at ${OUT}. Now make the FULL build authoritative-green and consistent.\n\n` +
  `Their per-surface notes: ${JSON.stringify(okSurfaces.map(r => ({ s: r.surface, compiles: r.compiles, notes: (r.notes || '').slice(0, 240) })))}\n\n` +
  `DO:\n` +
  `1) \`${BUILD}\` — fix ALL compile errors and meaningful warnings. Errors are most likely at the seams: src/ui/window.rs integrating menubar/statusbar, shared design.rs helper signatures, mod.rs, or a surface using an API another surface changed. Fix minimally + correctly; keep each surface's restyle intent.\n` +
  `2) Sanity: the window must still construct and all existing wiring intact (events, palette, open_project, theme, key bindings). Skim window.rs render to confirm titlebar(+menubar)/dock/status-bar compose without overlap.\n` +
  `3) Keep logic tests green: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -15\` should still pass (UI restyle must not have touched logic). If a UI #[cfg(test)] test broke due to a deliberate restyle, fix the test to match.\n` +
  `4) \`cargo fmt\`. Then commit: \`cd ${OUT} && git add -A && git commit -m "ui: Zed-aesthetic redesign of all surfaces (navbar, workspace, editor, scanner, tabs, start page, menus, dialogs)"\`. Report committed=<hash> and green=true only if \`${LP} cargo build\` finishes with no errors.`,
  {
    label: 'integrate', phase: 'Integrate',
    schema: {
      type: 'object', additionalProperties: false,
      properties: { green: { type: 'boolean' }, committed: { type: 'string' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } },
      required: ['green', 'notes'],
    },
  })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ──────────────────────────────────────────────────────────────────────────
// PHASE 4 — VERIFY (single serial agent; OWNS the display). Launch the real app
// headless, screenshot every state, crop into readable regions, vision-compare
// vs Zed + reclass refs, emit a per-surface punch-list.
// ──────────────────────────────────────────────────────────────────────────
phase('Verify')
const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    shots: { type: 'array', items: { type: 'string' } },
    findings: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        properties: {
          surface: { type: 'string', description: 'one of: chrome, workspace, editor, scanner, tabs, startpage, menus, dialogs' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
          issue: { type: 'string' },
          fix_hint: { type: 'string' },
        },
        required: ['surface', 'severity', 'issue', 'fix_hint'],
      },
    },
    overall: { type: 'string' },
  },
  required: ['shots', 'findings', 'overall'],
}
const verifyPrompt =
  `You are the VISUAL QA gate for the Reclass GPUI UI redesign. You OWN the headless display — you are the ONLY agent running the app. Build is green. Capture real screenshots, compare against ZED (the aesthetic target) and the reclass feature screenshots, and produce a precise per-surface punch-list.\n\n` +
  `TOOLING (a working Xvfb harness exists): from ${OUT} use \`./scripts/ui.sh\`:\n` +
  `  • \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\`  — (re)builds, launches the app with a real project loaded (19 structs → populated editor + workspace tree), waits for first frame. It prints "app up" on success.\n` +
  `  • Maximize the window for full-res shots: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass | head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`\n` +
  `  • \`./scripts/ui.sh shot /tmp/v_main.png\` — screenshot the whole 1680x1050 virtual screen.\n` +
  `  • \`./scripts/ui.sh key <keys>\` (xdotool: e.g. ctrl+shift+p, Escape), \`./scripts/ui.sh click X Y\`, \`./scripts/ui.sh type <text>\`.\n` +
  `CAPTURE these states (shot each, and for fine text CROP regions with ImageMagick \`convert IN -crop WxH+X+Y OUT\` so each crop is readable, then Read the crops):\n` +
  `  1. /tmp/v_loaded.png — EPROCESS.rcx loaded: the NAVBAR/menu bar (top), workspace tree (left), editor grid (center), tabs, status bar (bottom). Crop: top strip (navbar+tabs ~ 1680x70+0+0), left panel (~360x900+0+70), editor body (~1200x900+360+70), bottom (~1680x80 at y≈970).\n` +
  `  2. /tmp/v_menu.png — open the navbar: click the "File" menu (top-left of the menu bar, roughly x≈70 y≈12 — find it in v_loaded first) and shot; confirm a Zed dropdown opens with items. Try one more (View). Press Escape after.\n` +
  `  3. /tmp/v_palette.png — \`./scripts/ui.sh key ctrl+shift+p\` then shot the command palette; Escape after.\n` +
  `  4. /tmp/v_code.png — toggle the view mode to C/C++ (the Reclass|Code toggle in the titlebar or bottom — find/click it) and shot the generated-code view; toggle back.\n` +
  `  5. /tmp/v_start.png — restart WITHOUT a project (\`./scripts/ui.sh start\`), maximize, shot the start/welcome page.\n` +
  `  (The scanner panel: if it's docked at the bottom in v_loaded capture it there; if it opens via a menu/toggle, open it and shot /tmp/v_scanner.png. Best-effort — note if you cannot reach a state.)\n\n` +
  `For EACH captured surface, JUDGE against BOTH: (a) ZED's look (One Dark palette, comfortable type, subtle borders, soft selection, elevated popovers, content-forward chrome) and (b) the reclass FEATURE screenshots ${REF}/README_PIC1.png..PIC6.png (does the feature exist and read correctly?). Be a harsh Zed-quality critic: cramped/tiny text, wrong/hardcoded colors, missing borders/radii, hard selection fills, missing features (especially: is the NAVBAR/menu bar present and does it OPEN? is the status bar present? offset margin? change-highlight colors? scanner controls? code view?), misalignment, overlap.\n\n` +
  `Return: shots = the screenshot file paths you captured (so the human can review them); findings = a ranked punch-list, each tagged with surface ∈ {chrome, workspace, editor, scanner, tabs, startpage, menus, dialogs}, a severity, the concrete issue, and a fix_hint (which file/what to change). overall = a 2-3 sentence verdict on how close to Zed-quality + reclass-parity it is. When done, \`./scripts/ui.sh stop\`.`
const verify = await agent(verifyPrompt, { label: 'verify:screenshots', phase: 'Verify', schema: VERIFY_SCHEMA })
const findings = (verify && verify.findings) || []
log(`Verify: ${findings.length} findings (${findings.filter(f => f.severity !== 'minor').length} blocker/major). Shots: ${(verify && verify.shots || []).join(', ')}`)
log(`Overall: ${verify ? verify.overall : '(verify failed)'}`)

// ──────────────────────────────────────────────────────────────────────────
// PHASE 5 — POLISH (parallel by surface; only surfaces with findings). Fix the
// punch-list. Same disjoint ownership. No commits.
// ──────────────────────────────────────────────────────────────────────────
phase('Polish')
const bySurface = {}
for (const f of findings) (bySurface[f.surface] = bySurface[f.surface] || []).push(f)
const polishTargets = SURFACES.filter(s => (bySurface[s.key] || []).length > 0)
log(`Polishing ${polishTargets.length} surfaces with findings: ${polishTargets.map(s => `${s.key}(${bySurface[s.key].length})`).join(' ')}`)
const polishResults = await parallel(polishTargets.map(s => () =>
  agent(
    COMMON +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `Reclass reference screenshots: ${s.refs.map(r => `${REF}/${r}`).join(', ')}.\n\n` +
    `This is a POLISH pass. Visual QA reviewed real screenshots of your surface ("${s.label}") and filed these issues — FIX EACH:\n` +
    bySurface[s.key].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.issue}\n     fix hint: ${f.fix_hint}`).join('\n') +
    `\n\nRe-read your current files (they were already restyled once), apply the fixes to Zed quality, and ensure your files compile (\`${BUILD}\`). Original surface brief for context:\n` + s.task +
    `\n\nReturn the structured result.`,
    { label: `polish:${s.key}`, phase: 'Polish', schema: SURFACE_SCHEMA },
  )
))
log(`Polish done: ${polishResults.filter(Boolean).map(r => `${r.surface}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ──────────────────────────────────────────────────────────────────────────
// PHASE 6 — FINAL (single serial agent): green build, fmt, commit, final
// screenshots for human review, assessment.
// ──────────────────────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the Reclass GPUI UI Zed-redesign at ${OUT}. A polish pass just edited surfaces in parallel.\n\n` +
  `DO:\n` +
  `1) \`${BUILD}\` — fix any remaining compile errors/warnings to FULL GREEN. Confirm logic tests still pass: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\`.\n` +
  `2) \`cargo fmt\`, then commit: \`cd ${OUT} && git add -A && git commit -m "ui: polish pass — Zed-quality refinements from screenshot QA"\`.\n` +
  `3) FINAL SCREENSHOTS (you own the display): \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\`, maximize (\`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`), then capture: /tmp/final_loaded.png (project loaded — navbar+tree+editor+status), /tmp/final_menu.png (a navbar menu open), /tmp/final_palette.png (Ctrl+Shift+P), /tmp/final_start.png (restart with no project → start page). Read each to confirm they actually render the redesign (not blank/panicked). \`./scripts/ui.sh stop\` when done.\n` +
  `4) Assess: how close is the result to ZED-quality aesthetics + reclass FEATURE parity? List anything still missing/weak (remaining[]).\n\n` +
  `Return: green, committed=<hash>, shots=[the final screenshot paths], remaining=[gaps], assessment=<2-4 sentences>.`,
  {
    label: 'final', phase: 'Final',
    schema: {
      type: 'object', additionalProperties: false,
      properties: {
        green: { type: 'boolean' }, committed: { type: 'string' },
        shots: { type: 'array', items: { type: 'string' } },
        remaining: { type: 'array', items: { type: 'string' } },
        assessment: { type: 'string' },
      },
      required: ['green', 'shots', 'assessment'],
    },
  })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  foundation: foundation && { green: foundation.green, committed: foundation.committed },
  surfaces: okSurfaces.map(r => ({ surface: r.surface, compiles: r.compiles })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { findings: findings.length, overall: verify.overall, shots: verify.shots },
  polish: polishResults.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  final: final && { green: final.green, committed: final.committed, shots: final.shots, remaining: final.remaining, assessment: final.assessment },
}
