export const meta = {
  name: 'reclass-ui-functionality',
  description: 'Implement the missing functionality: wire ALL menu actions (File/View/Edit), C++-parity File menu with cascading Import/Export/Examples/Data-Source submenus, a bundled Examples picker, editor View-option toggles (Compact Columns/Tree Lines/Type Hints/Comments/Hover/Minimap), an editor MINIMAP, and fix two bugs (clicking address/type does not start inline edit; the text cursor disappears while typing). Verify with screenshots.',
  whenToUse: 'After the Zed redesign: make the menus/buttons actually DO things + fix inline-edit/cursor bugs + add minimap + examples.',
  phases: [
    { title: 'Prep', detail: 'pre-declare new modules in mod.rs; bundle the example .rcx files + examples() loader; commit green' },
    { title: 'Surfaces', detail: '4 parallel surfaces (disjoint files): menu tree+fly-outs, action wiring, editor view-options+minimap+bugfixes, modules/bookmarks docks' },
    { title: 'Integrate', detail: 'authoritative green build + fmt + commit' },
    { title: 'Verify', detail: 'screenshots: File submenus, examples open, View toggles (minimap/comments), click->inline-edit, cursor-while-typing -> punch-list' },
    { title: 'Polish', detail: 'fix punch-list per surface in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, assessment' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const CPP = '/home/loke/reclass-cpp'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// ── Shared CONTRACTS (verbatim in the coupled agents) ───────────────────────
const MENU_CONTRACT =
  `MENU CONTRACT (the menu tree built in commandpalette.rs::default_menu_tree, consumed by menubar.rs fly-outs AND window.rs::run_menu_command). Command-id namespace (id | label | shortcut), matching the C++ menus in ${CREF}/{import_options,export_options,data_options,view_options,reclass_side_by_side}.png:\n` +
  `  FILE: file.new_class "New Class" Ctrl+N · file.new_struct "New Struct" Ctrl+T · file.new_enum "New Enum" Ctrl+E · file.open "Open…" Ctrl+O · (Recent Files ▸ submenu, dynamic) · — · file.save "Save" Ctrl+S · file.save_as "Save As…" Ctrl+Shift+S · — · Import ▸ {file.import.source "From Source…", file.import.xml "ReClass XML…", file.import.pdb "PDB…"} · Export ▸ {file.export.cpp "C++ Header…", file.export.rust "Rust Structs…", file.export.defines "#define Offsets…", file.export.csharp "C# Structs…", file.export.python "Python ctypes…", file.export.xml "ReClass XML…"} · Examples ▸ {one item per bundled example -> id "file.example.<NAME>", label "<NAME>"} · — · file.close "Close Project" Ctrl+W · — · Data Source ▸ {source.file "File", source.process "Process Memory", source.kernel "Kernel Memory", source.remote "Remote Process Memory", source.windbg "WinDbg Memory", source.rcnet "ReClass.NET Compat", —, (active source shown checked), source.clear "Clear All"} · — · file.exit "Exit"\n` +
  `  EDIT: edit.undo "Undo" Ctrl+Z · edit.redo "Redo" Ctrl+Y · — · edit.cut "Cut" Ctrl+X · edit.copy "Copy" Ctrl+C · edit.paste "Paste" Ctrl+V · edit.delete "Delete" Del · — · edit.select_all "Select All" Ctrl+A\n` +
  `  VIEW: view.reset_windows "Reset Windows" · — · Font ▸ {view.font.inc "Increase", view.font.dec "Decrease", view.font.reset "Reset"} · Theme ▸ {one per theme -> "view.theme.<NAME>"} · — · view.compact_columns "Compact Columns"[check] · view.tree_lines "Tree Lines"[check] · view.relative_offsets "Relative Offsets"[check] · view.type_hints "Type Hints"[check] · view.comments "Comments"[check] · view.hover "Hover Effects"[check] · view.minimap "Minimap"[check] · — · view.refresh "Refresh" F5 · view.goto_address "Go to Address…" Ctrl+G · view.command_palette "Command Palette…" Ctrl+K · — · view.split "Split Editor" Ctrl+\\ · view.unsplit "Unsplit Editor" Ctrl+Shift+\\ · — · view.project "Project"[check] · view.scanner "Memory Scanner"[check] Ctrl+Shift+M · view.modules "Modules"[check] Ctrl+Shift+Y · view.bookmarks "Bookmarks"[check] Ctrl+Shift+B · — · view.presentation "Presentation Mode"[check] Ctrl+Shift+P\n` +
  `  HELP: help.about "About Reclass" · help.docs "Documentation"\n` +
  `  [check] = checkable item whose ✓ reflects live state via MenuBar::set_command_checked(id, bool). Keep the existing MenuNode::{Item{label,shortcut,command},Separator,Submenu{label,children}} enum.\n`

const EDITOR_SETTERS =
  `EDITOR VIEW-OPTION SETTER CONTRACT (RcxEditor exposes these for window.rs to call on the active editor; mirror the existing pub fn set_relative_offsets(bool,&mut Context) + getter):\n` +
  `  set_compact_columns(bool,cx)/compact_columns() · set_tree_lines(bool,cx)/tree_lines() · set_type_hints(bool,cx)/type_hints() · set_show_comments(bool,cx)/show_comments() · set_hover_effects(bool,cx)/hover_effects() · set_minimap(bool,cx)/minimap(). tree_lines/type_hints/show_comments are COMPOSE flags — thread them into the recompose (compose() already takes tree_lines:bool, type_hints:bool, show_comments:bool — see src/compose.rs:159). relative_offsets/hover_effects/compact_columns/minimap are render-level. Defaults: all true except comments=false (match the C++ View menu defaults).\n`

const COMMON =
  `You are implementing MISSING FUNCTIONALITY in the GPUI UI of "Reclass" (Rust+GPUI port; package "reclass" at ${OUT}; C++ at ${CPP}/src). Prior rounds shipped the Zed-aesthetic redesign + icons + node menu + pop-out scanner (build green, committed). The logic layer is fully implemented — WIRE it; do NOT change logic modules (tiny pub accessor at most; say so).\n\n` +
  `AESTHETIC = ZED; use the shared design system ${OUT}/src/ui/design.rs (tokens + design::icon_* SVG helpers) + ${OUT}/_design/zed_ui_spec.md. C++ BEHAVIOR reference = ${CREF}/*.png (read the ones listed; reproduce the behavior in Zed styling). gpui know-how: ${OUT}/_design/gpui_component_cookbook.md + ${OUT}/_design/gpui_cookbook.md.\n\n` +
  `BUILD (gpui cached, fast): \`${BUILD}\`. Other agents edit OTHER files concurrently — if the only errors are in files you don't own, that's expected; make YOUR files compile clean. RULES: edit ONLY your owned files; do NOT touch src/ui/mod.rs (the Prep stage pre-declared all new modules), logic modules, or other surfaces' files; honor the CONTRACTS verbatim; do NOT run git/cargo fmt (a later stage does). Preserve existing wiring + #[cfg(test)] tests; add tests for pure helpers you add.\n\n`

const SURFACE_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: { surface: { type: 'string' }, compiles: { type: 'boolean' }, files: { type: 'array', items: { type: 'string' } }, delivered: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } },
  required: ['surface', 'compiles', 'notes'],
}

const SURFACES = [
  {
    key: 'menus',
    label: 'File-menu parity + cascading fly-out submenus',
    files: ['src/ui/commandpalette.rs', 'src/ui/menubar.rs'],
    crefs: ['reclass_side_by_side.png', 'import_options.png', 'export_options.png', 'data_options.png', 'view_options.png'],
    task:
      `SURFACE: MENU TREE + FLY-OUT SUBMENUS (commandpalette.rs::default_menu_tree + menubar.rs ONLY). The user: "our file options do not look close to the same" + "most buttons do nothing" — fix the STRUCTURE here (wiring is a sibling agent).\n` +
      `1) default_menu_tree(): rebuild to the MENU CONTRACT below — the C++ File menu uses CASCADING SUBMENUS (Recent Files ▸, Import ▸, Export ▸, Examples ▸, Data Source ▸), NOT the current flat inlined sections. Add the missing Examples ▸ (build its children from \`crate::ui::examples::examples()\` → one item per (name,_), id \`file.example.<name>\`) and Data Source ▸. Match ${CREF}/reclass_side_by_side.png (left=C++ target, right=our current — make ours look like the left) + import_options/export_options/data_options/view_options.png. Build the full View menu with all the checkable toggles + Font/Theme submenus (Theme children = one per available theme from the ThemeManager).\n` +
      `2) menubar.rs: render REAL cascading fly-out submenus — currently nested submenus collapse to inlined section headers (see menu_dropdown ~line 290). Implement hover/click fly-out child menus anchored to the right of the parent item (Zed/gpui-component submenu pattern), so Import/Export/Examples/Data Source/Recent Files/Font/Theme open as proper cascading menus with the ▸ affordance. Keep the existing MenuCommand dispatch + set_command_checked + the AppMenu stop-propagation fix from round 3. Checkable items show a ✓ from the checked set.\n` +
      `${MENU_CONTRACT}\n` +
      `Keep the command palette's flat list working (it reads the same tree). Read the listed C++ crops.`,
  },
  {
    key: 'wiring',
    label: 'wire all menu actions + examples open + dialogs',
    files: ['src/ui/window.rs'],
    crefs: ['reclass_side_by_side.png', 'view_options.png', 'data_options.png'],
    task:
      `SURFACE: ACTION WIRING (src/ui/window.rs ONLY). The user: "most of the buttons in view, file, etc. do nothing." Make run_menu_command handle EVERY command id from the MENU CONTRACT below (it currently handles only a handful and falls through to a no-op default).\n` +
      `1) FILE: file.open / file.save / file.save_as → use \`cx.prompt_for_paths(PathPromptOptions{..})\` (gpui native dialog; see ${CPP}-side and the gpui-component story editor.rs example pattern) then open_project / RcxDocument save. file.import.{source,xml,pdb} → prompt + the existing import paths (XML via crate::imports). file.export.{cpp,rust,defines,csharp,python,xml} → call crate::generator::render_* on the active editor's tree+root and write/prompt a save path (or copy to clipboard + notify). file.example.<NAME> → look up the bundled json via \`crate::ui::examples::examples()\`, load it into the active tab (RcxDocument::load_str or write-temp+open) — opening a bundled example must WORK. file.close → close active doc. file.exit → quit. file.new_* already partly wired — verify.\n` +
      `2) VIEW: route the editor toggles to the active editor via the EDITOR SETTER CONTRACT — view.compact_columns→set_compact_columns, view.tree_lines→set_tree_lines, view.relative_offsets→set_relative_offsets, view.type_hints→set_type_hints, view.comments→set_show_comments, view.hover→set_hover_effects, view.minimap→set_minimap — and toggle+reflect the ✓ via menu_bar.set_command_checked. view.project→left dock toggle (exists), view.scanner→toggle_scanner_dock (exists), view.modules/view.bookmarks→toggle the right dock (DOCK CONTRACT), view.theme.<NAME>→switch_theme, view.font.*→editor font size, view.refresh→active editor apply_document, view.goto_address→open the goto dialog, view.command_palette→open palette, view.split/unsplit→best-effort or graceful, view.reset_windows→reset dock layout, view.presentation→toggle a presentation flag.\n` +
      `3) EDIT: edit.undo/redo already wired; add edit.cut/copy/paste/delete/select_all routed to the active editor/controller where ops exist (graceful otherwise).\n` +
      `4) On window construction, push the INITIAL checkmark states (compact/tree_lines/relative_offsets/type_hints/hover/minimap = on, comments = off, project = on) into the MenuBar so the View menu reflects reality.\n` +
      `5) BUG — "regardless of what you click on the welcome screen, it just moves you to a new class": on_start_page_event (window.rs ~622) handles Card(StartCard::NewClass) then \`Card(_) => just dismiss\` (so OpenProject/ImportSource/ImportXml/ImportPdb all fall through to a blank new class). ROUTE EACH StartCard distinctly: NewClass→new doc; OpenProject→the same Open dialog as file.open; ImportSource→file.import.source; ImportXml→file.import.xml; ImportPdb→file.import.pdb; (Tutorial→help.docs). The StartCard variants already exist (startpage.rs:34) — only this window.rs routing is wrong. Each card must do its OWN thing, not land on a new class.\n` +
      `${MENU_CONTRACT}\n${EDITOR_SETTERS}\n` +
      `DOCK CONTRACT: the Docks stage mounts a RIGHT dock holding Modules + Bookmarks panels (closed by default) and returns their handles via LayoutHandles; add toggle_right_dock / toggle_modules / toggle_bookmarks here mirroring the existing toggle_scanner_dock (bottom) + set_left_dock_open patterns, using DockPlacement::Right.\n` +
      `Keep ALL existing window wiring intact (events, palette, open_project, theme, key bindings, scanner toggle). Add key bindings for any new shortcuts (F5, Ctrl+G, Ctrl+K, Ctrl+Shift+Y/B, Ctrl+\\). Read ${CREF}/reclass_side_by_side.png + view_options.png + data_options.png.`,
  },
  {
    key: 'editor',
    label: 'view-options + minimap + fix inline-edit-on-click + fix cursor',
    files: ['src/ui/editor/element.rs', 'src/ui/editor/mod.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/palette.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/inline_edit.rs', 'src/ui/editor/tab_cycle.rs'],
    crefs: ['reclass_reclass_active.png', 'reclass_address_hover.png', 'data_options.png'],
    task:
      `SURFACE: EDITOR functionality + TWO BUGS (src/ui/editor/* ONLY). Highest-value: the bugs.\n` +
      `BUG 1 — "clicking on the address, type does nothing in the active class": the inline-edit machinery EXISTS (begin_inline_edit at mod.rs:458, hit_test_row at mod.rs:378 → mod.rs:421, handles EditTarget::{Name,RootClassName,Type,ArrayElementType,Value}). But clicking the base ADDRESS in the class-header row and the TYPE token on node rows does not enter edit. DEBUG the click→hit_test→begin_inline_edit path (mod.rs ~378-421, the Left mouse-down at ~981, and the class-header row which may have its own handler) and FIX it so a left-click on the base address, the type, the name, and the value each starts the correct inline edit in the active/root class. Add the missing EditTarget hit regions (esp. the header base address) if hit_test_row doesn't map them. Add a #[cfg(test)] hit-test test for address+type columns.\n` +
      `BUG 2 — "giving keyboard input makes the cursor disappear; it comes back when we move it": the field caret is a custom PaintQuad (inline_edit.rs:488 cursor: Option<PaintQuad>). It is painted only when the element re-lays-out / on mouse events, so a keystroke (which updates text + cursor_offset + cx.notify()) does not keep the caret visible — and a blink phase or animation-frame is likely not being driven. FIX: on every edit/key/cursor-move, keep the caret SOLID-visible (reset blink phase to visible) and request the next paint/animation-frame so it stays drawn while typing; ensure the caret quad is recomputed at the new cursor_offset on each notify. The caret must remain visible continuously while typing, blinking only when idle.\n` +
      `3) VIEW-OPTION TOGGLES + setters per the EDITOR SETTER CONTRACT: add fields + pub setters/getters for compact_columns, tree_lines, type_hints, show_comments, hover_effects, minimap (mirror the existing relative_offsets at mod.rs:147/292/299). Thread tree_lines/type_hints/show_comments into the recompose (compose() takes them — src/compose.rs:159; the editor must call compose with these flags instead of compose_default). compact_columns tightens column spacing; hover_effects gates the hovered-line bg; relative_offsets already works.\n` +
      `4) MINIMAP (new — editor/minimap.rs, declared via \`mod minimap;\` in editor/mod.rs which YOU own): a right-side scaled overview of the structure (Zed minimap + the C++ purple overview block in ${CREF}/data_options.png) — a narrow column painting tiny proportional bars/rows of the composed lines (color by node kind), with a viewport indicator, toggled by set_minimap(true/false). Keep it cheap (paint from the existing ComposeResult line metas).\n` +
      `Keep ALL hit-test/fold/selection/tab-cycle/inline-edit behaviour + tests. Read ${CREF}/reclass_reclass_active.png + reclass_address_hover.png.`,
  },
  {
    key: 'docks',
    label: 'Modules + Bookmarks toggleable docks',
    files: ['src/ui/docks.rs', 'src/ui/panels.rs', 'src/ui/modulespanel.rs', 'src/ui/bookmarkspanel.rs'],
    crefs: ['data_options.png', 'view_options.png'],
    task:
      `SURFACE: MODULES + BOOKMARKS DOCKS (docks.rs, panels.rs, modulespanel.rs, bookmarkspanel.rs — the last two are NEW empty stubs the Prep stage created; YOU fill them). The C++ View menu has Modules (Ctrl+Shift+Y) + Bookmarks (Ctrl+Shift+B) toggles and a right-side Modules/Symbols/Types panel with "Download All" (see ${CREF}/data_options.png top-right + view_options.png).\n` +
      `1) modulespanel.rs: a Zed-styled Panel (mirror workspace.rs's WorkspacePanel structure: pub fn view(window,cx)->Entity<Self>, impl Panel + Render) with a header (Modules / Symbols / Types tabs + a "Download All" button) and a list area. Real module data isn't exposed by the provider here — show the loaded module(s) if any, else a clean empty-state ("No modules — attach a data source"). Tabs switch the list (Modules/Symbols/Types), each empty-state for now.\n` +
      `2) bookmarkspanel.rs: a Zed Panel listing bookmarks. The controller has add_bookmark/remove_bookmark (controller.rs:3073/3088) — add a tiny pub accessor on the controller/document to READ the bookmark list if none exists (note it in your result), and render the list with add/remove affordances + empty-state.\n` +
      `3) docks.rs: mount Modules + Bookmarks as TABS in a RIGHT dock (DockPlacement::Right, ~280px, CLOSED by default — like the C++ where they're summoned from View). Return their entities in LayoutHandles (extend the struct) so window.rs can toggle/observe them. Keep the existing center/left/bottom mounts + the scanner pop-out (bottom, closed) from round 3 unchanged.\n` +
      `4) panels.rs: if PanelKind needs Modules/Bookmarks kinds for titles/icons, add them. Keep the existing placeholder machinery.\n` +
      `Keep existing dock wiring + tests. Read ${CREF}/data_options.png + view_options.png.`,
  },
]

// ── PHASE 1: Prep (serial) — mod stubs + bundle examples ────────────────────
phase('Prep')
const prep = await agent(
  `PREP for a UI-functionality round on the Reclass GPUI port (${OUT}). Set up shared scaffolding so 4 parallel agents never touch src/ui/mod.rs.\n\n` +
  `DO (commit when green):\n` +
  `1) src/ui/mod.rs: add \`pub mod examples;\`, \`pub mod modulespanel;\`, \`pub mod bookmarkspanel;\`. Create modulespanel.rs + bookmarkspanel.rs as minimal-but-compiling STUBS (module doc + a placeholder; the Docks stage fills them).\n` +
  `2) BUNDLE EXAMPLES: copy the example projects \`cp ${CPP}/src/examples/*.rcx ${OUT}/assets/examples/\` (mkdir -p ${OUT}/assets/examples first). Do NOT use a top-level examples/ dir (Cargo treats examples/*.rs as targets) — use assets/examples/.\n` +
  `3) src/ui/examples.rs (FULL, you own it): a dep-free loader exposing \`pub fn examples() -> &'static [(&'static str, &'static str)]\` returning (display_name, rcx_json_text) for each bundled file, using \`include_str!(\"../../assets/examples/<file>.rcx\")\` for each copied file (one entry per file you copied — list them explicitly; display_name = file stem). Also \`pub fn example_json(name: &str) -> Option<&'static str>\`. Add a #[cfg(test)] test asserting examples() is non-empty and each json parses as JSON (serde_json::from_str::<serde_json::Value>).\n` +
  `4) Verify the bundled examples are loadable by the existing loader: confirm \`crate::controller::RcxDocument\` can load one (read controller.rs for load/load_str; if only a path-based load(&Path) exists, examples.rs may instead expose the embedded bytes + a helper that writes a temp file and returns the path, OR add the json string to a load_str — pick the simplest that lets window.rs open an example. Document the chosen open path in your result so the wiring agent uses it.).\n\n` +
  `VERIFY: \`${BUILD}\` green + \`cargo build --no-default-features 2>&1|tail -3\` green (examples/panels are ui-gated). \`cargo fmt\`. Commit: \`cd ${OUT} && git add -A && git commit -m "ui(prep): bundle example .rcx projects + examples() loader; stub modules/bookmarks panels"\`. Report committed=<hash>, the example names bundled, and the exact API + how to OPEN an example (so the wiring agent matches).`,
  { label: 'wf7:prep', phase: 'Prep', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, examples: { type: 'array', items: { type: 'string' } }, open_api: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Prep: ${prep ? (prep.green ? 'green' : 'NOT green') : 'FAILED'}${prep && prep.committed ? ' @' + prep.committed : ''} — examples: ${(prep && prep.examples || []).join(', ')}`)

// ── PHASE 2: Surfaces (parallel) ────────────────────────────────────────────
phase('Surfaces')
const openNote = prep && prep.open_api ? `EXAMPLES OPEN API (from Prep — use this to open a bundled example): ${prep.open_api}\n\n` : ''
log(`Implementing ${SURFACES.length} surfaces in parallel.`)
const impl = await parallel(SURFACES.map(s => () =>
  agent(
    COMMON + (s.key === 'wiring' || s.key === 'menus' ? openNote : '') +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `C++ behavior crops to READ: ${s.crefs.map(c => `${CREF}/${c}`).join(', ')}.\n\n` +
    s.task +
    `\n\nEnsure your files compile (\`${BUILD}\`) and return the structured result.`,
    { label: `wf7:${s.key}`, phase: 'Surfaces', schema: SURFACE_SCHEMA },
  )
))
log(`Surfaces done: ${impl.filter(Boolean).map(r => `${r.surface.slice(0, 12)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 3: Integrate (serial) ─────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `Round-4 surfaces just landed for the Reclass GPUI UI at ${OUT}: menu tree+fly-out submenus (commandpalette/menubar), action wiring (window.rs run_menu_command for ALL ids + examples open + dialogs + view toggles), editor view-options+minimap+inline-edit/cursor bug fixes, and Modules/Bookmarks right-dock. Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(impl.filter(Boolean).map(r => ({ s: r.surface, ok: r.compiles, notes: (r.notes || '').slice(0, 220) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings. Likely seams: the MENU CONTRACT command-ids (menus emits ⇄ window handles — must match exactly), the EDITOR SETTER names (window calls ⇄ editor defines), the DOCK CONTRACT (window toggles ⇄ docks LayoutHandles), examples API (window ⇄ examples.rs). 2) Both builds green: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) Logic tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\`. 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: wire all menu actions + File-menu parity (Import/Export/Examples/Data Source) + examples picker + editor view-options/minimap + inline-edit & cursor fixes + modules/bookmarks docks"\`. Report green + committed=<hash> + any contract mismatches you fixed.`,
  { label: 'wf7:integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 4: Verify (serial, owns display) ──────────────────────────────────
phase('Verify')
const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    shots: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { surface: { type: 'string', description: 'menus|wiring|editor|docks|prep' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, issue: { type: 'string' }, fix_hint: { type: 'string' } }, required: ['surface', 'severity', 'issue', 'fix_hint'] } },
    overall: { type: 'string' },
  },
  required: ['shots', 'findings', 'overall'],
}
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for Reclass GPUI round-4 (menu wiring + File-menu parity + examples + view-options + minimap + inline-edit/cursor bug fixes). You OWN the headless display. Build is green. Compare vs the C++ crops in ${CREF}/*.png.\n\n` +
  `HARNESS from ${OUT}: \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null || xdotool windowsize "$WID" 1680 1010; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/w7_X.png\`; CROP with \`convert IN -crop WxH+X+Y OUT\` then Read. Drive: \`./scripts/ui.sh key <keys>\`, \`./scripts/ui.sh click X Y\`, \`./scripts/ui.sh type <text>\`, right-click \`export DISPLAY=:99; xdotool mousemove X Y click 3\`.\n\n` +
  `CAPTURE + JUDGE (round-4 deliverables):\n` +
  `  1. /tmp/w7_filemenu.png — click "File" → confirm it matches the C++ (${CREF}/reclass_side_by_side.png left): cascading Import ▸ / Export ▸ / Examples ▸ / Data Source ▸ submenus (hover one to confirm it FLIES OUT, not inlined). Shot the open menu + a flown-out submenu.\n` +
  `  2. /tmp/w7_example.png — open File ▸ Examples ▸ <pick one e.g. KUSER_SHARED_DATA> → confirm the example project LOADS into the editor (tree + struct change). \n` +
  `  3. /tmp/w7_viewmenu.png + /tmp/w7_minimap_on.png/_off.png — click "View" → confirm all toggles present with ✓ states. Toggle Minimap → confirm a minimap appears on the right; toggle again → gone. Toggle Comments → confirm the comments column appears/disappears. Toggle Tree Lines → connectors change.\n` +
  `  4. /tmp/w7_inline_edit.png — CLICK directly on a TYPE token (e.g. "uint32_t") on a node row, and separately on the base ADDRESS in the class header → confirm an inline EDIT field appears (the bug was: clicking did nothing). Crop tight.\n` +
  `  5. /tmp/w7_cursor.png — with an inline field open, \`./scripts/ui.sh type abc\` then IMMEDIATELY \`./scripts/ui.sh shot /tmp/w7_cursor.png\` (no mouse move) → confirm the text CURSOR/caret is VISIBLE after typing (the bug was: cursor disappears until mouse move). Judge carefully.\n` +
  `  6. /tmp/w7_modules.png — toggle View ▸ Modules (or ctrl+shift+y) → confirm a Modules panel appears (right dock).\n` +
  `  7. /tmp/w7_welcome.png — restart with NO project (\`./scripts/ui.sh start\`), then click the "Open project" card (and separately an Import card) on the welcome screen → confirm it does its OWN action (opens a file dialog / import), NOT silently landing on a blank new class (the reported bug). Note what each card does.\n` +
  `Be a harsh critic. Return: shots = paths; findings = ranked punch-list tagged surface∈{menus,wiring,editor,docks,prep}, severity, issue, fix_hint; overall = 2-3 sentence verdict (esp: do menus fly out + actions work? do examples open? does minimap toggle? does click start inline edit? is the cursor visible while typing?). \`./scripts/ui.sh stop\` when done.`,
  { label: 'wf7:verify', phase: 'Verify', schema: VERIFY_SCHEMA })
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
    COMMON + (s.key === 'wiring' || s.key === 'menus' ? openNote : '') +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `C++ crops: ${s.crefs.map(c => `${CREF}/${c}`).join(', ')}.\n\n` +
    `POLISH pass. Functional QA filed these on your surface ("${s.label}") from real screenshots — FIX EACH:\n` +
    bySurface[s.key].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.issue}\n     hint: ${f.fix_hint}`).join('\n') +
    `\n\nRe-read your files, fix to working + Zed/C++-parity quality, ensure they compile (\`${BUILD}\`). Original brief:\n` + s.task +
    `\n\nReturn the structured result.`,
    { label: `wf7polish:${s.key}`, phase: 'Polish', schema: SURFACE_SCHEMA },
  )
))
log(`Polish done: ${polish.filter(Boolean).map(r => `${r.surface.slice(0, 12)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 6: Final (serial) ─────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for Reclass GPUI round 4 at ${OUT}.\n` +
  `DO: 1) \`${BUILD}\` to FULL GREEN + \`cargo build --no-default-features 2>&1|tail -3\` green + \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\` passes. 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: round 4 polish — menu/examples/view-option/minimap/inline-edit refinements from QA"\`. 3) FINAL SCREENSHOTS (you own the display): \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\`, maximize, capture: /tmp/w7final_filemenu.png (File menu w/ Examples▸+Data Source▸ submenus), /tmp/w7final_example.png (an opened example), /tmp/w7final_minimap.png (minimap on), /tmp/w7final_inline.png (click→inline edit + visible cursor after typing). Read each. \`./scripts/ui.sh stop\`.\n` +
  `4) Assess: which menu actions now work vs still stub; do examples open; does minimap toggle; are the inline-edit + cursor bugs fixed; closeness to C++ parity. List remaining gaps. Return green, committed, shots, remaining, assessment.`,
  { label: 'wf7:final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  prep: prep && { green: prep.green, committed: prep.committed, examples: prep.examples },
  surfaces: impl.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { findings: findings.length, overall: verify.overall, shots: verify.shots },
  polish: polish.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  final: final && { green: final.green, committed: final.committed, shots: final.shots, remaining: final.remaining, assessment: final.assessment },
}
