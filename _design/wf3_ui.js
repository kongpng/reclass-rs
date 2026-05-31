export const meta = {
  name: 'reclass-rs-ui',
  description: 'Build the GPUI UI layer of the Reclass Rust port on top of the finished logic layer. Reset-resilient: a single sequential pipeline in the main tree where EVERY stage commits immediately (typeinfer gap → UI foundation → editor surface → chrome → dialogs → panels → wire app).',
  whenToUse: 'Phase 3 of the Reclass C++→Rust port: the GPUI UI + app shell.',
  phases: [
    { title: 'typeinfer', detail: 'finish core::typeinfer per-kind checkers (lone remaining logic gap)' },
    { title: 'UI foundation', detail: 'shared app state, theme application, window + gpui-component Root + DockArea skeleton' },
    { title: 'Editor surface', detail: 'the bespoke raw-gpui Element: styled grid, inline edit, selection, folds, hex/ASCII' },
    { title: 'Chrome', detail: 'titlebar, docks/tabs, workspace tree, start page' },
    { title: 'Dialogs & pickers', detail: 'options/goto dialogs, command palette, type/enum/source pickers, hex popup, tooltips, context menus' },
    { title: 'Panels', detail: 'scanner panel + process picker (DataTables)' },
    { title: 'Wire app', detail: 'main.rs window/CLI/lifecycle wiring; full build; smoke check; commit' },
  ],
}

const SRC = '/home/loke/reclass-cpp'
const OUT = '/home/loke/reclass-rs'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'

const MOD_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    stage: { type: 'string' }, green: { type: 'boolean' },
    passed: { type: 'integer' }, failed: { type: 'integer' },
    files: { type: 'array', items: { type: 'string' } },
    committed: { type: 'string' }, notes: { type: 'string' },
  },
  required: ['stage', 'green', 'notes'],
}

// Every stage runs in the MAIN tree (gpui is cached in target/ → fast incremental builds) and
// COMMITS on success, so a host reset loses at most the one in-progress stage.
const COMMIT = (msg) => `\`cargo fmt\` then commit your work: \`git add -A && git commit -m "${msg}"\` (run from ${OUT}). Report the commit hash in "committed".`

// ---- Stage 0: the lone remaining logic gap, run directly in the main tree (reset-resilient) ----
const typeinferTask =
  `You are finishing ONE logic module of the Rust port of "Reclass" (single package "reclass" at ${OUT}; C++ source at ${SRC}). Work DIRECTLY in the main tree (no isolated copy — you are the only agent running; keeping work on disk makes it survive a host reset).\n\n` +
  `TASK: the per-kind feature checkers in ${OUT}/src/core/typeinfer.rs are still todo!()/unimplemented!(). Port them 1:1 from ${SRC}/src/typeinfer.h (and how core.h / compose use type inference). Read ${OUT}/_design/understand/core-model.md and the golden oracle ${OUT}/_oracle/logs/test_typeinfer.txt + ${OUT}/_oracle/test_sources/test_typeinfer.cpp. Translate test_typeinfer.cpp into Rust #[test]s. Make \`cargo test --no-default-features\` green (run \`cargo test --no-default-features typeinfer\` to focus). Un-ignore any compose test that depended on typeinfer if it now passes. No remaining product-code todo!()/unimplemented!() in src/core/typeinfer.rs.\n\n` +
  `VERIFY: \`cargo test --no-default-features\` green and \`${LP} cargo build\` (full, ui/gpui) still links. ` + COMMIT('fill gap: core::typeinfer')

// ---- UI stages: sequential, in the MAIN tree (gpui cached → ~3.5s incremental builds) ----
const uiHeader =
  `You are building the GPUI UI layer of the faithful Rust port of "Reclass" (an open-source struct-layout editor). ` +
  `The Rust port is a SINGLE package "reclass" at ${OUT}. The ENTIRE logic layer is already implemented and tested (core, provider, compose, format, addr, generator, scanner, disasm, rtti, theme, controller, imports, mcp — 800+ tests). Build the UI ON TOP of it; do not change logic modules except to add a minimal \`pub\` accessor if genuinely required (and say so). ` +
  `The UI lives under ${OUT}/src/ui/ (cargo feature "ui", default-on) and uses gpui + gpui_platform + gpui-component. ` +
  `READ/USE: ${OUT}/_design/ARCHITECTURE.md (§5 UI strategy + the surface→component mapping), ${OUT}/_design/gpui_component_cookbook.md (gpui-component widgets, exact APIs, the verified dep/setup) and ${OUT}/_design/gpui_cookbook.md (raw-gpui: custom Element render/layout/paint/hit-test, uniform_list, text layout, focus, actions, key bindings) — grounded in real Zed source. Also the C++ at ${SRC} and the understand maps ${OUT}/_design/understand/{editor-surface,app-shell,widgets-dialogs,controller,themes}.md. ` +
  `You work in the MAIN tree at ${OUT} (gpui is already compiled in target/, so \`cargo build\` is fast/incremental). Build with: \`${LP} cargo build\`. Keep the FULL build GREEN. Where a view contains pure logic (state reducers, formatting, key handling), add #[cfg(test)] unit tests. Full visual/interactive verification needs a display and is deferred — your bar is: compiles green, structured per the cookbooks, faithful to the C++, unit-tested where logic permits. ` +
  `Build on the PRIOR committed stages (this is a sequential pipeline; earlier UI stages are already in the tree).\n\n`

const UI_STAGES = [
  { key: 'foundation', label: 'UI foundation', task:
    `STAGE: UI FOUNDATION. Establish the shared scaffolding every other UI piece plugs into:\n` +
    `- The application-state types (open documents/tabs, active document, active data source, selection, theme handle) and a top-level gpui Root view using gpui-component's application()/init/Root setup (extend the existing minimal src/ui/mod.rs window).\n` +
    `- THEME APPLICATION: map the already-implemented theme model (src/theme/) onto gpui-component's Theme/ThemeRegistry so themes load and switch at runtime (understand/themes.md + the component cookbook). 8 default themes ship in _oracle/fixtures/themes/.\n` +
    `- The main window layout shell: gpui-component TitleBar + a DockArea with placeholder panels (workspace dock, scanner dock) and an MDI document-tab area in the center — placeholders fine; real content comes later. Define clear traits/structs so the editor surface, docks, dialogs, panels can be filled in next.\n` +
    `- Establish src/ui/ module structure (mod.rs + submodules: state, theme_apply, window, editor (stub), docks (stub), dialogs (stub), panels (stub)).\n` +
    `Keep it compiling and the window constructible. Define the seams; do not implement the editor grid or real dialogs yet. ` + COMMIT('ui: foundation (app state, theme apply, window + dock skeleton)') },
  { key: 'editor', label: 'Editor surface', task:
    `STAGE: THE EDITOR SURFACE (the hardest piece). Implement the bespoke structured-editor view as a custom raw-gpui Element (gpui-component has no equivalent — confirmed). Faithfully reproduce the C++ editor (understand/editor-surface.md + src/editor.cpp): render the rows produced by the already-implemented compose module (text + LineMeta: per-span styling/colors, columns, fold markers, hex/ASCII columns, per-byte change highlighting) as a VIRTUALIZED list (uniform_list or a custom Element per the gpui cookbook); hit-testing so the inline-editable regions (type names, field names, values, base address, array meta, pointer targets, enum/bitfield members, expressions, comments) are clickable to edit; tab-cycling between editable fields within a line; multi-select (Ctrl/Shift click) and cross-row selection; fold/expand of structs/arrays/pointers; and wire edits + navigation through the already-implemented controller (refresh, write-back, undo/redo). Use the controller's API for state; the Element is the view. Add unit tests for the pure parts (row→span layout, hit-test math, selection model). Keep the build green. ` + COMMIT('ui: editor surface (bespoke gpui Element: styled grid, inline edit, selection, folds)') },
  { key: 'chrome', label: 'Chrome', task:
    `STAGE: APP CHROME (understand/app-shell.md) via gpui-component: the custom TitleBar (src/titlebar.*), the DockArea docks (workspace explorer Tree of structs/enums/unions sorted by field count with quick navigation; scanner dock placeholder), MDI document tabs (one document per tab, the "+" new-tab sentinel, per-tab source icon), the dual view-mode toggle (tree view vs rendered C/C++ output), and the start page (src/startpage.h). Wire the workspace tree + tabs to the app state and the editor surface. Keep green. ` + COMMIT('ui: chrome (titlebar, docks, workspace tree, MDI tabs, start page)') },
  { key: 'dialogs', label: 'Dialogs & pickers', task:
    `STAGE: DIALOGS, POPUPS & PICKERS (understand/widgets-dialogs.md) via gpui-component: options dialog (src/optionsdialog.*), goto-address dialog (src/gotoaddressdialog.h), command palette (src/commandpalette.h — List/Input with fuzzy filtering; use the already-ported fuzzy scorer so ranking/highlights match), type/enum/source picker popups (src/typeselectorpopup.*, enum_picker_popup.h, sourcechooserpopup.*), hex toolbar popup (src/hextoolbarpopup.*), tooltips/hover previews (rcxtooltip.h, hover_preview.h), context menus, the find bar (Ctrl+F), and themed message/input dialogs. Wire actions to the controller/logic. Keep green. ` + COMMIT('ui: dialogs, popups & pickers') },
  { key: 'panels', label: 'Panels', task:
    `STAGE: PANELS via gpui-component DataTable: scanner panel (src/scannerpanel.*) — search controls + a virtualized results table wired to the already-implemented scanner module over the active Provider; process picker (src/processpicker.*) as a DataTable backed by the provider registry's available sources (the live process data source itself is an out-of-scope stub — show available sources + clearly-labeled stub entries). Also the profiler dialog table if present. Keep green. ` + COMMIT('ui: panels (scanner panel + process picker)') },
  { key: 'wire', label: 'Wire app', task:
    `STAGE: WIRE THE APP (src/main.cpp → src/main.rs). Implement the binary: parse CLI args with clap (open a file/.rcx given on the command line), init tracing, create the gpui application + main window assembling ALL the pieces (titlebar, docks, tabs, editor surface, panels, dialogs, theme), run the event loop. Connect the document lifecycle to controller + imports (open .rcx / file data source) and the refresh loop. Ensure the MCP bridge bin still builds.\n` +
    `VERIFY: \`${LP} cargo build\` (full) green; \`cargo test --no-default-features --features disasm,symbols,imports,mcp\` still green; \`cargo build --no-default-features\` green; SMOKE: run \`./target/debug/reclass --help\` and \`--version\` to confirm the CLI works without a display; if xvfb-run is available, confirm \`./target/debug/reclass\` constructs without panicking headless (else note GUI run needs a display). Update README.md with run instructions. ` + COMMIT('ui: wire main.rs app (window, CLI, lifecycle) — end-to-end build') },
]

async function tryAgent(p, opts) {
  try { return await agent(p, opts) } catch (e) { log(`agent ${opts && opts.label} failed: ${String(e).slice(0, 160)}`); return null }
}

// ---------- run: fully sequential, main-tree, commit-per-stage ----------
log('Phase 3 (resumed) — sequential main-tree pipeline, each stage commits (reset-resilient).')

const results = []
const STAGES = [
  { key: 'typeinfer', label: 'typeinfer', prompt: typeinferTask, opts: { label: 'gap:typeinfer' } },
  ...UI_STAGES.map(s => ({ key: s.key, label: s.label, prompt: uiHeader + s.task, opts: { label: `ui:${s.key}` } })),
]

for (const s of STAGES) {
  phase(s.label)
  const r = await tryAgent(
    s.prompt + `\n\nReturn the structured result (stage="${s.key}", green, passed, failed, files, committed=<hash>, notes).`,
    { ...s.opts, schema: MOD_SCHEMA })
  results.push(r)
  log(`Stage ${s.key}: ${r ? (r.green ? 'green' : 'NOT green') : 'failed/null'}${r && r.committed ? ' @' + r.committed : ''}`)
  if (r && !r.green) log(`  ⚠ ${s.key} not green — later stages may be affected; continuing.`)
}

return {
  stages: results.filter(Boolean).map(r => ({ stage: r.stage, green: r.green, committed: r.committed, notes: (r.notes || '').slice(0, 160) })),
}
