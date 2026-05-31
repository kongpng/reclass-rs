export const meta = {
  name: 'reclass-ui-zed-round3',
  description: 'Round 3: real SVG icons (register gpui-component asset source), C++ interaction parity (node right-click menu, change-type popup, address-format hover, status-bar key hints), and a POP-OUT/toggleable Memory Scanner (hidden until summoned, like the C++ version). Verify with screenshots.',
  whenToUse: 'After rounds 1-2: icon assets + behavioral parity with the C++ reference + toggleable scanner.',
  phases: [
    { title: 'Assets', detail: 'register gpui-component icon asset source (main.rs + Cargo.toml) + design::icon_* helpers — so SVG icons actually render' },
    { title: 'Surfaces', detail: '5 parallel surfaces (disjoint files): editor menu/hover/icons, change-type popup, chrome scanner pop-out + status bar, tabs, workspace icons' },
    { title: 'Integrate', detail: 'authoritative green build + fmt + commit' },
    { title: 'Verify', detail: 'screenshots: icons crisp, node right-click menu, change-type popup, address hover, scanner toggle -> punch-list' },
    { title: 'Polish', detail: 'fix punch-list per surface in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, assessment' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const CPP = '/home/loke/reclass-cpp'
const REFPNG = `${CPP}/docs`                                  // static reclass PIC1-6
const CREF = '/home/loke/Pictures/reclass_reference/cpp_only' // C++-only crops of the live interactions
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`
const GC = '/home/loke/.cargo/git/checkouts/gpui-component-95ce574d8a0da8b8/bd4174e'

const COMMON =
  `You are adding C++-PARITY INTERACTIONS + real icons to the GPUI UI of "Reclass" (Rust+GPUI port; package "reclass" at ${OUT}; C++ at ${CPP}/src). Rounds 1-2 shipped a Zed-aesthetic redesign + wired the code view & scanner panel (build green, committed). The logic layer is fully implemented — WIRE existing logic; do NOT change logic modules (a tiny pub accessor at most; say so).\n\n` +
  `AESTHETIC = ZED (One Dark, comfortable type, subtle borders, soft-accent selection, hover overlay, elevated rounded popovers). READ + USE the shared design system: ${OUT}/src/ui/design.rs (Zed tokens + NEW design::icon_* SVG helpers from the Assets stage) and ${OUT}/_design/zed_ui_spec.md. No ad-hoc hex — use tokens / cx.theme().\n\n` +
  `BEHAVIORAL REFERENCE = the C++ reclass, captured live in ${CREF}/*.png (these are the ACTUAL C++ app — read the ones listed for your surface; reproduce the interaction in Zed styling). Static layout refs: ${REFPNG}/README_PIC*.png.\n\n` +
  `ICONS: the Assets stage registered the gpui-component asset source, so \`gpui_component::Icon::new(gpui_component::IconName::X)\` and the \`crate::ui::design::icon_*\` helpers now render real SVGs (they showed as empty boxes before, which is why round-2 used text glyphs — those text-glyph workarounds should now be REPLACED with design::icon_* / IconName SVGs). Only use IconName variants that exist (the Assets stage listed the valid set in design.rs).\n\n` +
  `gpui know-how: ${OUT}/_design/gpui_component_cookbook.md (PopupMenu/ContextMenuExt, Icon, Dropdown, Modal, Tooltip) + ${OUT}/_design/gpui_cookbook.md (raw-gpui Element, mouse/hit-test, deferred overlays).\n\n` +
  `BUILD (gpui cached, fast): \`${BUILD}\`. Other agents edit OTHER files concurrently — if the only errors are in files you don't own, that's expected; make YOUR files compile clean. RULES: edit ONLY your owned files; do NOT touch src/ui/mod.rs, logic modules, or other surfaces' files; honor the cross-agent CONTRACTS below verbatim; do NOT run git/cargo fmt (a later stage does). Preserve existing wiring + #[cfg(test)] tests.\n\n`

const SURFACE_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: { surface: { type: 'string' }, compiles: { type: 'boolean' }, files: { type: 'array', items: { type: 'string' } }, delivered: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } },
  required: ['surface', 'compiles', 'notes'],
}

// ── CONTRACTS shared verbatim between the coupled agents ─────────────────────
const CONTRACT_TYPESEL =
  `CONTRACT (TypeSelector popup — menus agent PROVIDES, editor agent CONSUMES):\n` +
  `  • src/ui/typeselectorpopup.rs exposes a view + event:\n` +
  `      pub fn TypeSelectorPopup::view(current: crate::core::NodeKind, window: &mut Window, cx: &mut App) -> Entity<TypeSelectorPopup>\n` +
  `      pub enum TypeSelectorEvent { Chosen { kind: crate::core::NodeKind, modifier: Option<Modifier> }, Cancel }   // EventEmitter<TypeSelectorEvent>\n` +
  `    (Modifier is the existing typeselectorpopup::Modifier.) The popup builds its TypeModel over the existing type catalogue.\n` +
  `  • The editor opens it via window.open_dialog (same pattern as window.rs's command palette), subscribes to TypeSelectorEvent, and on Chosen calls controller_mut().change_node_kind(idx, kind) (+ apply the chosen Modifier: pointer/array/etc. via the existing controller ops) then apply_document(cx); on Cancel closes.\n`

// ── The 5 surfaces (disjoint files) ─────────────────────────────────────────
const SURFACES = [
  {
    key: 'editor',
    label: 'editor: node right-click menu + address hover + type cycler + icons',
    files: ['src/ui/editor/element.rs', 'src/ui/editor/mod.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/palette.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/inline_edit.rs', 'src/ui/editor/tab_cycle.rs'],
    crefs: ['reclass_right_click_on_address.png', 'reclass_address_hover.png', 'reclass_reclass_active.png', 'reclass_right_click_on_type.png'],
    task:
      `SURFACE: EDITOR INTERACTION PARITY (src/ui/editor/* ONLY). Read ${CREF}/reclass_right_click_on_address.png (the node context menu) and ${CREF}/reclass_address_hover.png (address tooltip).\n` +
      `1) NODE RIGHT-CLICK CONTEXT MENU: on right-mouse-down over a node row, open a Zed PopupMenu anchored at the cursor (gpui-component PopupMenu / the deferred-overlay pattern; the editor already scaffolds context-menu state in mod.rs). Reproduce the C++ menu items (see the screenshot): New Class · Ptr to New Class · (sep) · a "← <curType> ↔ <altType> →" quick type-cycler row · Rename (F2) · Change Type (T) · (sep) · Insert ▸ · Convert ▸ · Big endian · Static ▸ · (sep) · Duplicate (Ctrl+D) · Delete (Delete) · (sep) · Fold ▸ · Copy ▸ · Tracking ▸ · Copy as C Struct. Leading SVG icons (design::icon_*) + right-aligned accelerator hints + separators. WIRE the items to the existing controller ops on the editor's controller_mut(): change_node_kind, rename_node (open an inline rename or a small input), insert_node / insert_node_above, remove_node, duplicate_node, toggle_collapse (Fold), convert_to_typed_pointer (Ptr to New Class / Convert). After a mutating op call apply_document(cx). Items without a logic op (Big endian/Tracking/Copy variants) may be graceful stubs but must render + close cleanly. Bind the accelerators (F2/T/Ctrl+D/Delete) in the RcxEditor key context.\n` +
      `2) CHANGE TYPE: "Change Type" (and the T accelerator, and clicking the type token) opens the TypeSelector popup. ${CONTRACT_TYPESEL}\n` +
      `3) ADDRESS-FORMAT HOVER TOOLTIP: hovering the class-header base-address region shows a Zed hover popover listing the address formats (build it inline with design tokens; match ${CREF}/reclass_address_hover.png + PIC5 "Base Address"): "0x7FF61234ABCD — hex address / <app.exe> — module base / <app.exe> + 0x1A0 — module + offset / [<app.exe> + 0x58] — follow pointer / ntdll!Symbol — PDB symbol", then "Operators: + - * << >> & | ^" and "All numbers are hexadecimal".\n` +
      `4) ICONS: replace the round-2 text-glyph node-kind markers (geometry::kind_glyph ◆ ▦ → # • ƒ) and fold carets with crisp SVG icons via design::icon_* (struct/array/pointer/hex/value/function + chevron-right/down). Keep the gutter column math + hit-testing intact.\n` +
      `Keep ALL hit-test/inline-edit/fold/selection behaviour + tests. Controller op signatures (verified): change_node_kind(node_idx:usize,new_kind), rename_node(node_idx,&str), insert_node(parent_id,offset,kind,name), insert_node_above(before_idx,kind,name), remove_node(node_idx), duplicate_node(node_idx), toggle_collapse(node_idx), convert_to_typed_pointer(node_id).`,
  },
  {
    key: 'menus',
    label: 'change-type popup parity + source chooser + popover icons',
    files: ['src/ui/typeselectorpopup.rs', 'src/ui/sourcechooser.rs', 'src/ui/contextmenu.rs', 'src/ui/tooltip.rs', 'src/ui/enumpicker.rs', 'src/ui/hextoolbar.rs', 'src/ui/commandpalette.rs'],
    crefs: ['reclass_right_click_on_type.png', 'reclass_view_click_active.png', 'reclass_source_click_active.png', 'reclass_source_hover.png'],
    task:
      `SURFACE: POPOVERS PARITY (your files ONLY). Read ${CREF}/reclass_right_click_on_type.png + ${CREF}/reclass_view_click_active.png (the change-type popup) and ${CREF}/reclass_source_click_active.png (the source chooser).\n` +
      `1) CHANGE-TYPE POPUP (typeselectorpopup.rs) — THE headline. Build the popup VIEW to the contract below and match the C++: a search input (× close); CATEGORY FILTER TABS with live counts "● Hex (5)  ● Int (11)  ● Float (3)  ● Ptr (4)" + "all / none / N types"; column header "group · name · size" with sort + layout toggles; a grouped, scrollable type list (section labels "● Hex", "● Int / Bool", "● Float", "● Ptr") where each row = a colored kind chip + monospace type name + right-aligned SIZE with a small width bar (e.g. "4B", "16B"); the selected/current type highlighted; a footer "<curtype> · <size>" + MODIFIER buttons "*" (pointer) "**" (double-ptr) "[]" (array) "+ New" and a primary blue "OK". The model already exists (TypeModel/KindGroup/Modifier/TypeEntry/TypeRow) — build the gpui view over it. ${CONTRACT_TYPESEL}\n` +
      `2) SOURCE CHOOSER (sourcechooser.rs) — match ${CREF}/reclass_source_click_active.png + PIC4: plugin list (File / Kernel Memory / Process Memory / ReClass.NET Compat / Remote Process Memory / WinDbg) each with a leading SVG icon + plugin hint, a separator, recent sources, a CHECKMARK on the active source, a separator, "Clear All". Zed dropdown-menu styling.\n` +
      `3) ICON SWAP across contextmenu.rs / tooltip.rs / enumpicker.rs / hextoolbar.rs / commandpalette.rs: replace text glyphs with design::icon_* SVGs (leading menu icons, chevrons, search icon, checkmarks). Keep command-palette fuzzy + all event/selection wiring + tests.\n` +
      `KEEP public APIs stable except where the contract specifies new ones. Read the listed C++ crops.`,
  },
  {
    key: 'chrome',
    label: 'scanner pop-out toggle + menu wiring + status-bar key hints + icons',
    files: ['src/ui/window.rs', 'src/ui/menubar.rs', 'src/ui/docks.rs', 'src/ui/statusbar.rs'],
    crefs: ['reclass_memory_scanner.png', 'reclass_reclass_active.png', 'reclass_right_click_on_address.png'],
    task:
      `SURFACE: CHROME — make the scanner POP OUT (toggleable, hidden by default) + finish status-bar parity + menu wiring + icons (your files ONLY: window.rs, menubar.rs, docks.rs, statusbar.rs). Read ${CREF}/reclass_memory_scanner.png (the C++ scanner is a separate pop-out window, hidden until summoned) and ${CREF}/reclass_reclass_active.png (status bar).\n` +
      `1) SCANNER POP-OUT (the user's explicit ask — it should NOT always be present): in docks.rs revert the round-2 "open by default" → the bottom scanner dock is CLOSED by default (set_bottom_dock open flag back to false). In window.rs add \`toggle_scanner_dock(&mut self, window, cx)\` that flips the bottom dock open/closed (mirror the existing set_left_dock_open helper but for DockPlacement::Bottom), an action OpenScanner / ToggleScanner, and a key binding (e.g. ctrl-shift-m) registered in open_main_window_with. In menubar.rs add a "Memory Scanner" item to the View menu (checkable, reflecting open state) that calls the toggle. If gpui-component DockArea supports a floating/detached panel, prefer making the scanner a true floating pop-out window like the C++ (read the cookbook); otherwise a toggleable bottom dock satisfies "pops out when we want it, not always present". Result must: launch with NO scanner visible, and View→Memory Scanner (or ctrl-shift-m) shows/hides it.\n` +
      `2) STATUS BAR PARITY (statusbar.rs) — match ${CREF}/reclass_right_click_on_address.png bottom strip: "<Struct>.<field>  |  +0xNN  ↔  <type> (idx/total)  |  size" and the KEY HINTS "P=ptr  F=float  S=int  U=uint" on the right, plus the data-source + theme segments. Extend StatusInfo (pure, unit-tested) for the type-index + key-hints; keep render_status_bar(info, source, cx) signature stable so window.rs still compiles.\n` +
      `3) MENU WIRING (menubar.rs + window.rs run_menu_command): wire the menu items that have logic (New Class/Open/Save/Import/Export/Close — most already dispatch; View toggles for workspace + scanner + theme switch + Tree/Code view) to real handlers; keep stubs graceful. Ensure Edit menu (Undo/Redo/Cut/Copy/Paste/Delete) routes to the active editor/controller where ops exist.\n` +
      `4) ICONS: swap any chrome text glyphs (menu/window-control/status) for design::icon_* SVGs. Keep ALL existing window.rs wiring (events, palette, open_project, theme, key bindings) intact — you are ADDING the scanner toggle + status parity + menu wiring, not rewriting the shell.\n` +
      `NOTE: do NOT touch src/main.rs (the Assets stage owns it) or src/ui/tabs.rs / editor/* / workspace.rs.`,
  },
  {
    key: 'tabs',
    label: 'tab strip + view-mode toggle icons/affordances',
    files: ['src/ui/tabs.rs'],
    crefs: ['reclass_view_hover.png', 'reclass_view_click_active.png', 'reclass_code_view.png', 'reclass_reclass_active.png'],
    task:
      `SURFACE: TABS + VIEW TOGGLE (src/ui/tabs.rs ONLY). Read ${CREF}/reclass_view_hover.png + ${CREF}/reclass_view_click_active.png + ${CREF}/reclass_reclass_active.png.\n` +
      `1) Replace text-glyph tab affordances with crisp SVG icons via design::icon_*: per-tab source icon, hover-reveal close ×, modified dot, the "+" new-tab sentinel. Polish the Zed active-tab treatment (elevated bg + accent edge) + hover bg to match the round-1 spec, now with real icons.\n` +
      `2) The bottom view-mode segmented control ("Reclass | Code"): refine to Zed segmented-control quality matching ${CREF}/reclass_view_click_active.png (clear selected segment contrast, hover state). (A third "Debug" mode exists in the C++ but is DEFERRED this round — do not add it, it needs a ViewMode enum change outside your ownership.)\n` +
      `Keep ALL DocAreaEvent/ViewMode/codegen-view wiring + tests. Light, focused surface.`,
  },
  {
    key: 'workspace',
    label: 'workspace tree icons + click-open',
    files: ['src/ui/workspace.rs'],
    crefs: ['reclass_reclass_active.png'],
    task:
      `SURFACE: WORKSPACE TREE polish (src/ui/workspace.rs ONLY).\n` +
      `1) Replace the text "S" type badge + text chevrons with crisp SVG icons via design::icon_* (a struct/type icon per row; chevron-right/down for disclosure). Keep the member-count pill + hover/selected states from round-2.\n` +
      `2) Confirm single/double-click a type row opens it in the editor (WorkspaceNav already wired ~line 1045) — verify it still fires after the icon changes; the row's clickable area should include the icon + name.\n` +
      `3) Keep the round-2 right-click context menu (Open in Tab / Rename / Duplicate / Add Member / Delete); swap its glyphs for SVG icons. The mutating items remain graceful stubs (the editor's own right-click menu provides the real node-edit path this round).\n` +
      `Keep ALL WorkspaceModel/WorkspaceNav/filter/tooltip wiring + tests. Read ${CREF}/reclass_reclass_active.png.`,
  },
]

// ── PHASE 1: Assets (serial, FIRST) — register icon source + design::icon_* ──
phase('Assets')
const assets = await agent(
  `Register the gpui-component ICON ASSET SOURCE so SVG icons render (round-2 used text glyphs because no asset source was registered → IconName showed empty boxes). Then add curated design::icon_* helpers. Package "reclass" at ${OUT}.\n\n` +
  `FACTS (verified): the asset crate is \`gpui-component-assets\` (crate \`gpui_component_assets\`) in the SAME git repo as gpui-component (checkout ${GC}; the dep block is [dependencies.gpui-component] in Cargo.toml). The registration pattern (from ${GC}/crates/story/src/main.rs) is:\n` +
  `    gpui_platform::application().with_assets(gpui_component_assets::Assets).run(...)\n` +
  `The 107 icon SVGs live at ${GC}/crates/assets/assets/icons — the file stems map to gpui_component::IconName variants (verify exact variant names against the IconName enum / docs at ${GC}/docs/docs/components/icon.md).\n\n` +
  `DO:\n` +
  `1) Cargo.toml: add \`gpui-component-assets\` as an OPTIONAL dependency from the same git source+rev as gpui-component (mirror the existing [dependencies.gpui-component] git/branch), and add \`"dep:gpui-component-assets"\` to the existing \`ui = [...]\` feature list so \`--no-default-features\` stays clean. Read the existing gpui-component dep block first and mirror it exactly (do NOT pin a rev that differs).\n` +
  `2) src/main.rs: in the #[cfg(feature="ui")] run(), change \`gpui_platform::application()\` to \`gpui_platform::application().with_assets(gpui_component_assets::Assets)\`. Do not change the headless (non-ui) path.\n` +
  `3) src/ui/design.rs: add a documented \`pub mod icon { ... }\` (or pub fns) of curated helpers returning gpui_component::Icon for the icons every surface needs — using ONLY valid IconName variants you confirmed. Cover at least: chevron_right, chevron_down, struct_/object, pointer (arrow), array (grid), hex (hash), value (dot), function, enum_, search, scan (search), refresh, close, plus, source/database, check, settings, info. Each helper applies a sensible default size/color from tokens; document the IconName each maps to.\n` +
  `4) Smoke: the existing IconName usages (tabs.rs IconName::Close/Plus, messagebox IconName::Info/TriangleAlert/CircleX) must still compile and now have a backing asset. Build green.\n\n` +
  `VERIFY: \`${BUILD}\` green AND \`cargo build --no-default-features 2>&1 | tail -3\` green (the asset dep must be ui-gated). Run \`cargo fmt\`. Commit: \`cd ${OUT} && git add -A && git commit -m "ui: register gpui-component icon asset source + design::icon_* helpers (real SVG icons)"\`. Report committed=<hash> AND the exact list of design::icon_* helpers + their IconName mapping (surfaces depend on this).`,
  { label: 'r3:assets', phase: 'Assets', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, headless_green: { type: 'boolean' }, icon_helpers: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Assets: ${assets ? (assets.green ? 'green' : 'NOT green') : 'FAILED'}${assets && assets.committed ? ' @' + assets.committed : ''} — helpers: ${(assets && assets.icon_helpers || []).slice(0, 20).join(', ')}`)

// ── PHASE 2: Surfaces (parallel, disjoint files) ────────────────────────────
phase('Surfaces')
const helperNote = assets && assets.icon_helpers && assets.icon_helpers.length
  ? `Available design::icon_* helpers (from the Assets stage): ${assets.icon_helpers.join(', ')}. Use these; if you need another, use gpui_component::IconName directly with a variant you verify exists.\n\n`
  : `Use crate::ui::design icon helpers (see design.rs) and gpui_component::IconName SVGs (verify variants exist).\n\n`
log(`Implementing ${SURFACES.length} surfaces in parallel.`)
const impl = await parallel(SURFACES.map(s => () =>
  agent(
    COMMON + helperNote +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `C++ interaction crops to READ: ${s.crefs.map(c => `${CREF}/${c}`).join(', ')}.\n\n` +
    s.task +
    `\n\nEnsure your files compile (\`${BUILD}\`) and return the structured result.`,
    { label: `r3:${s.key}`, phase: 'Surfaces', schema: SURFACE_SCHEMA },
  )
))
log(`Surfaces done: ${impl.filter(Boolean).map(r => `${r.surface.slice(0, 12)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 3: Integrate (serial) ─────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `Round-3 surfaces just landed for the Reclass GPUI UI at ${OUT} (editor node menu + address hover + icons; change-type popup; chrome scanner POP-OUT toggle + status-bar parity; tabs; workspace icons). The Assets stage registered the icon source + design::icon_* helpers. Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(impl.filter(Boolean).map(r => ({ s: r.surface, ok: r.compiles, notes: (r.notes || '').slice(0, 220) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/meaningful warnings. Likely seams: the TypeSelector contract (editor consumes typeselectorpopup::TypeSelectorPopup::view + TypeSelectorEvent — make the two sides match), design::icon_* helper signatures, window.rs scanner-toggle + key binding, menubar View item. 2) Confirm both builds: ui (\`${LP} cargo build\`) AND headless (\`cargo build --no-default-features 2>&1|tail -3\`). 3) Logic tests green: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\`. 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: round 3 — real SVG icons, node context menu + change-type popup, address hover, pop-out scanner, status-bar parity"\`. Report green + committed=<hash>.`,
  { label: 'r3:integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 4: Verify (serial, owns display) ──────────────────────────────────
phase('Verify')
const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    shots: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { surface: { type: 'string', description: 'editor|menus|chrome|tabs|workspace|assets' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, issue: { type: 'string' }, fix_hint: { type: 'string' } }, required: ['surface', 'severity', 'issue', 'fix_hint'] } },
    overall: { type: 'string' },
  },
  required: ['shots', 'findings', 'overall'],
}
const verify = await agent(
  `VISUAL QA for Reclass GPUI round-3 (icons + C++ interaction parity + pop-out scanner). You OWN the headless display. Build is green. Compare against the C++ crops in ${CREF}/*.png and Zed quality.\n\n` +
  `HARNESS from ${OUT}: \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\` (builds+launches, "app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null || xdotool windowsize "$WID" 1680 1010; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/r3_X.png\`; CROP for legibility: \`convert IN -crop WxH+X+Y OUT\` then Read. Drive: \`./scripts/ui.sh key <keys>\`, \`./scripts/ui.sh click X Y\`, right-click \`export DISPLAY=:99; xdotool mousemove X Y click 3\`.\n\n` +
  `CAPTURE + JUDGE (the round-3 deliverables):\n` +
  `  1. /tmp/r3_loaded.png — confirm the SCANNER is NOT visible by default (pop-out, hidden); icons across navbar/tree/editor/tabs are crisp SVGs (NOT empty boxes, NOT text glyphs ◆▦⌄). Crop the tree + a few editor rows + tab strip.\n` +
  `  2. /tmp/r3_scanner_toggle.png — toggle the scanner: press \`ctrl+shift+m\` (or open the View menu and click "Memory Scanner") → confirm the scanner panel/window APPEARS; toggle again → confirm it HIDES. Shot the visible state.\n` +
  `  3. /tmp/r3_node_menu.png — right-click an editor NODE row (find a row's X,Y in r3_loaded; \`xdotool mousemove X Y click 3; sleep 1\`) → confirm the rich context menu (New Class / Ptr to New Class / type-cycler / Rename / Change Type / Insert / Convert / Duplicate / Delete / Fold / Copy / Copy as C Struct) with SVG icons + accelerators. Compare to ${CREF}/reclass_right_click_on_address.png. Escape after.\n` +
  `  4. /tmp/r3_typepopup.png — open Change Type: with the menu up click "Change Type" OR press \`t\` on a selected node → confirm the type popup with category filter tabs (Hex/Int/Float/Ptr + counts), grouped list with size bars, and * ** [] +New OK footer. Compare to ${CREF}/reclass_right_click_on_type.png. Escape after.\n` +
  `  5. /tmp/r3_addr_hover.png — hover the class-header base address (\`xdotool mousemove <addrX> <addrY>; sleep 1\`) → confirm the address-format help tooltip. Compare to ${CREF}/reclass_address_hover.png.\n` +
  `  6. /tmp/r3_status.png — crop the status bar → confirm name·offset·type(idx/total)·size + P/F/S/U key hints.\n` +
  `Be a harsh critic vs the C++ crops + Zed. Return: shots = paths; findings = ranked punch-list tagged surface∈{editor,menus,chrome,tabs,workspace,assets}, severity, issue, fix_hint; overall = 2-3 sentence verdict (esp: do icons render as real SVGs? does the scanner toggle work? does the node menu + type popup match the C++?). \`./scripts/ui.sh stop\` when done.`,
  { label: 'r3:verify', phase: 'Verify', schema: VERIFY_SCHEMA })
const findings = (verify && verify.findings) || []
log(`Verify: ${findings.length} findings (${findings.filter(f => f.severity !== 'minor').length} blocker/major). ${verify ? verify.overall : ''}`)

// ── PHASE 5: Polish (parallel by surface with findings) ─────────────────────
phase('Polish')
const bySurface = {}
for (const f of findings) (bySurface[f.surface] = bySurface[f.surface] || []).push(f)
const targets = SURFACES.filter(s => (bySurface[s.key] || []).length > 0)
log(`Polishing ${targets.length}: ${targets.map(s => `${s.key}(${bySurface[s.key].length})`).join(' ')}`)
const polish = await parallel(targets.map(s => () =>
  agent(
    COMMON + helperNote +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `C++ crops: ${s.crefs.map(c => `${CREF}/${c}`).join(', ')}.\n\n` +
    `POLISH pass. Visual QA filed these on your surface ("${s.label}") from real screenshots — FIX EACH:\n` +
    bySurface[s.key].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.issue}\n     hint: ${f.fix_hint}`).join('\n') +
    `\n\nRe-read your files, fix to Zed + C++-parity quality, ensure they compile (\`${BUILD}\`). Original brief:\n` + s.task +
    `\n\nReturn the structured result.`,
    { label: `r3polish:${s.key}`, phase: 'Polish', schema: SURFACE_SCHEMA },
  )
))
log(`Polish done: ${polish.filter(Boolean).map(r => `${r.surface.slice(0, 12)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 6: Final (serial) ─────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for Reclass GPUI round 3 at ${OUT}.\n` +
  `DO: 1) \`${BUILD}\` to FULL GREEN + \`cargo build --no-default-features 2>&1|tail -3\` green + \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\` passes. 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: round 3 polish — icon + interaction-parity refinements from QA"\`. 3) FINAL SCREENSHOTS (you own the display): \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\`, maximize, capture: /tmp/r3final_loaded.png (icons crisp, NO scanner), /tmp/r3final_scanner.png (after ctrl+shift+m — scanner shown), /tmp/r3final_nodemenu.png (editor right-click menu), /tmp/r3final_typepopup.png (change-type popup). Read each to confirm they render. \`./scripts/ui.sh stop\`.\n` +
  `4) Assess closeness to ZED-quality + C++ interaction parity; list remaining gaps. Return green, committed, shots, remaining, assessment.`,
  { label: 'r3:final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  assets: assets && { green: assets.green, committed: assets.committed, headless_green: assets.headless_green },
  surfaces: impl.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { findings: findings.length, overall: verify.overall, shots: verify.shots },
  polish: polish.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  final: final && { green: final.green, committed: final.committed, shots: final.shots, remaining: final.remaining, assessment: final.assessment },
}
