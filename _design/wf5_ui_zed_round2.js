export const meta = {
  name: 'reclass-ui-zed-round2',
  description: 'Round 2: close the feature-parity blockers from screenshot QA — mount the real Scanner panel, render real generated C/C++ (code view), and add Zed interaction polish (row hover/selection, node icons, context menu, tab affordances). Verify with screenshots.',
  whenToUse: 'After the round-1 Zed redesign: wire the implemented scanner+generator into the UI and add interaction states.',
  phases: [
    { title: 'Implement', detail: '5 parallel surfaces (disjoint files): code view, scanner mount, editor interaction, workspace interaction, status bar' },
    { title: 'Integrate', detail: 'authoritative green build + fmt + commit' },
    { title: 'Verify', detail: 'headless screenshots: code view, scanner open, hover/selection, context menu -> punch-list' },
    { title: 'Polish', detail: 'fix punch-list per surface in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, assessment' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const CPP = '/home/loke/reclass-cpp'
const REF = `${CPP}/docs`
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

const COMMON =
  `You are closing feature-parity gaps in the GPUI UI of "Reclass" (Rust+GPUI port of the struct-layout editor). Package "reclass" at ${OUT}; C++ at ${CPP}/src. The logic layer (core/compose/controller/scanner/generator/provider/format/theme) is FULLY IMPLEMENTED + tested — your job is to WIRE existing logic into a real UI and add interaction polish. Do NOT change logic modules (at most a tiny pub accessor; say so).\n\n` +
  `Round 1 already shipped a Zed-aesthetic redesign (navbar/menus, project tree, editor grid, start page, command palette, modals) — build is green, committed. READ the shared design system FIRST: ${OUT}/src/ui/design.rs (Zed tokens + helpers) and ${OUT}/_design/zed_ui_spec.md, and USE them (no ad-hoc hex; pull from tokens/cx.theme()). AESTHETIC = ZED (One Dark, comfortable type, subtle borders, soft-accent selection, hover-overlay, elevated popovers). FEATURE reference = the reclass screenshots ${REF}/README_PIC1.png..PIC6.png — READ the ones listed and reproduce the features in Zed styling.\n\n` +
  `gpui know-how: ${OUT}/_design/gpui_component_cookbook.md + ${OUT}/_design/gpui_cookbook.md.\n\n` +
  `BUILD (gpui cached, fast): \`${BUILD}\`. Other agents edit OTHER files concurrently — if the only errors are in files you don't own, that's expected; make YOUR files compile clean. RULES: edit ONLY your owned files; do NOT touch src/ui/mod.rs, src/ui/window.rs, other surfaces' files, or logic modules; do NOT run git/cargo fmt (a later stage does). Preserve existing wiring + #[cfg(test)] tests.\n\n`

const SURFACE_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    surface: { type: 'string' }, compiles: { type: 'boolean' },
    files: { type: 'array', items: { type: 'string' } },
    delivered: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' },
  },
  required: ['surface', 'compiles', 'notes'],
}

const SURFACES = [
  {
    key: 'codeview',
    label: 'code view (generated C/C++) + tab affordances',
    files: ['src/ui/tabs.rs'],
    refs: ['README_PIC3.png', 'README_PIC1.png'],
    task:
      `BLOCKER: the C/C++ code view is PLACEHOLDER TEXT. In src/ui/tabs.rs, \`render_body\` (around line 519) currently renders \`"// Rendered C/C++ for {title} (generated view)"\` for \`ViewMode::Rendered\`. The generator is fully implemented — WIRE IT.\n` +
      `1) Render REAL generated source: get the active editor's tree + root via \`let ed = entry.editor.read(cx); let tree = ed.controller().tree(); let root = ed.controller().view_root_id();\` then call \`crate::generator::render_cpp_tree(tree, root, aliases, /*emit_asserts*/ false)\` (pass the document's TypeAliases if readily available via the controller/document, else None — read generator.rs for the exact signature: render_cpp_tree(&NodeTree, u64, Option<&TypeAliases>, bool) -> String). If you find a higher-level controller/document helper that already renders code, prefer it.\n` +
      `2) DISPLAY it like reclass PIC3's right pane: a scrollable code view with a LINE-NUMBER gutter (muted, right-aligned, monospace) and the source lines with ONE DARK syntax highlighting — color keywords (struct/class/void/const/unsigned/#pragma/#include) ~purple #c678dd, types (uint32_t/int64_t/float/User type names) ~yellow #e5c07b, numbers/hex ~orange #d19a66, strings ~green #98c379, and trailing "// 0x.." offset comments ~green-gray #5c6370. A simple per-line tokenizer is fine (split on words; classify by a small keyword/type set + regex-ish checks for 0x.. and //). Use design tokens for bg/gutter/colors. Make it look like a Zed read-only code editor. Handle empty (no root / non-struct) gracefully.\n` +
      `3) TAB STRIP affordances (the strip is also in tabs.rs): per-tab close × that appears on hover, a modified/dirty dot, hover background, active-tab Zed treatment, and the "+" new-tab sentinel — match reclass PIC1 tabs + Zed's tab bar. Keep all DocAreaEvent wiring.\n` +
      `Read PIC3 (code pane + line numbers + highlight) and PIC1 (tabs).`,
  },
  {
    key: 'scanner',
    label: 'mount + finish the real Scanner panel',
    files: ['src/ui/docks.rs', 'src/ui/scannerpanel.rs'],
    refs: ['README_PIC3.png', 'README_PIC6.png'],
    task:
      `BLOCKER: the real Scanner panel exists (src/ui/scannerpanel.rs — ScannerPanel::view(window,cx), full inputs/Table/run_scan, impls Panel+Render at line ~947) but is NOT MOUNTED. src/ui/docks.rs line ~88 mounts a PLACEHOLDER: \`PlaceholderPanel::view(PanelKind::Scanner, cx)\`. So the bottom dock shows an empty stub.\n` +
      `1) In docks.rs: mount the REAL panel — \`let scanner = Arc::new(super::scannerpanel::ScannerPanel::view(window, cx));\` (drop the PlaceholderPanel import if now unused) and keep it in the bottom DockItem::tabs. OPEN the bottom dock BY DEFAULT so the scanner is visible (change the \`false\` in \`set_bottom_dock(bottom, Some(px(360.)), false, ...)\` to \`true\`; ~300-340px is fine). Keep center/left mounts unchanged. Do NOT touch window.rs.\n` +
      `2) In scannerpanel.rs: make the panel render its FULL Zed UI even with NO provider attached (no live data source yet): the toolbar row (scan-type dropdown e.g. Value/Signature, Type: int32 dropdown, Scan/Condition: Exact Value dropdown, a Value/Pattern input), the Executable/Writable/Current-Struct checkboxes, a primary "Scan" + secondary "Re-scan" button, a RESULTS TABLE with proper column headers (Address / Value / Previous) shown empty, a muted count line ("0 results" / "No data source — attach a process or file to scan"), and the footer "Go to Address" + "Copy Address" buttons (disabled when no selection). Match reclass PIC3/PIC6 features in Zed styling (use design tokens + gpui-component Table/Input/Button/Checkbox/Dropdown). Live scanning needs a provider wired from the document (window.rs) — that's OUT OF SCOPE here; if no provider, Scan shows a graceful "attach a data source" state. NOTE this in your result.\n` +
      `Read PIC3, PIC6 and ${OUT}/_design/understand/scanner.md.`,
  },
  {
    key: 'editor',
    label: 'editor interaction states + icons + address margin',
    files: ['src/ui/editor/element.rs', 'src/ui/editor/mod.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/palette.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/inline_edit.rs', 'src/ui/editor/tab_cycle.rs'],
    refs: ['README_PIC1.png', 'README_PIC2.png', 'README_PIC5.png'],
    task:
      `INTERACTION POLISH for the core editor grid (src/ui/editor/* ONLY).\n` +
      `1) ROW HOVER + SELECTION: every node row should show a subtle hover overlay on mouse-over and a soft accent-tinted background when active/selected (reclass highlights the active row; Zed uses a gentle fill). The selection model + hit-testing already exist — ensure the Element actually PAINTS a visible hover + selected-row background (design tokens: hover_overlay, selected_accent bg). Active-line subtle highlight too.\n` +
      `2) NODE-TYPE ICONS: prefix each node row with a small kind glyph/icon distinguishing struct / pointer / array / hex / fnptr / value (like reclass + Zed outline icons). Keep the fold chevron clearly styled (a crisp disclosure triangle) for expandable nodes.\n` +
      `3) ADDRESS MARGIN + HEADER correctness: when there is NO live base address (file/empty source), the left margin should show clean +offset values (+0, +8, +10 …) — NOT a single repeated garbage absolute address like "0xFFFF800000000000" on every row. The class header should show the correct viewed struct name + a sensible base (offset mode when no live base). If the default view-root picks the wrong struct (e.g. shows _LIST_ENTRY instead of the project's main struct when a multi-struct .rcx loads), pick a better default root (the project's first/declared root struct) — investigate where view_root_id defaults in editor/mod.rs (~line 942) and the controller. Keep all hit-test/inline-edit/fold/selection behaviour + tests.\n` +
      `Read PIC1, PIC2, PIC5 and ${OUT}/_design/understand/editor-surface.md.`,
  },
  {
    key: 'workspace',
    label: 'workspace tree interaction + context menu',
    files: ['src/ui/workspace.rs'],
    refs: ['README_PIC2.png', 'README_PIC5.png'],
    task:
      `INTERACTION POLISH for the Project/Workspace left tree (src/ui/workspace.rs ONLY).\n` +
      `1) Row HOVER overlay + SELECTED-row soft-accent highlight (currently rows read as flat text). Brighter text on the selected row.\n` +
      `2) Crisp disclosure CHEVRONS for expandable type rows (struct members), with proper indent guides.\n` +
      `3) Right-click CONTEXT MENU on a type row (Zed context menu via the design system / gpui-component popup): items like Open in Tab / Rename / Duplicate / Delete / Add Member — wire to existing WorkspaceNav/actions where they exist, otherwise stub the item gracefully (menu opens, items show, closes on select/escape). \n` +
      `4) TOOLTIP on truncated long type names (show the full name on hover).\n` +
      `Keep all WorkspaceModel/WorkspaceNav/filter/nav wiring + tests. Read PIC2, PIC5.`,
  },
  {
    key: 'statusbar',
    label: 'status bar default + right segments',
    files: ['src/ui/statusbar.rs'],
    refs: ['README_PIC5.png', 'README_PIC3.png'],
    task:
      `POLISH the bottom status bar (src/ui/statusbar.rs ONLY). It already builds StatusInfo + renders, but reads EMPTY when nothing is selected.\n` +
      `1) DEFAULT readout (no selection): show the viewed struct/root type name + node/member count + total struct size (e.g. "_EPROCESS  ·  19 fields  ·  0x... bytes") instead of a blank bar — pull from the controller/tree (extend StatusInfo with a default branch; keep it a pure unit-tested helper).\n` +
      `2) RIGHT-ALIGNED segments like Zed's status bar + reclass: the offset/size detail (reclass PIC5 "+0x20"; PIC3 "offset: 0x0010  size: 8 bytes"), the data-source readout, and the current theme/view-mode — laid out as muted Zed status segments with subtle separators.\n` +
      `Keep it themed via design tokens (chrome_bg, top 1px border, UI_XS muted). Extend the existing #[cfg(test)] tests for the new default-readout logic. Read PIC5, PIC3.`,
  },
]

// ── Implement (parallel, disjoint files) ────────────────────────────────────
phase('Implement')
log(`Round 2: implementing ${SURFACES.length} surfaces in parallel (disjoint files).`)
const impl = await parallel(SURFACES.map(s => () =>
  agent(
    COMMON +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `Reclass reference screenshots (Read them): ${s.refs.map(r => `${REF}/${r}`).join(', ')}.\n\n` +
    s.task +
    `\n\nEnsure your files compile (\`${BUILD}\`) and return the structured result.`,
    { label: `r2:${s.key}`, phase: 'Implement', schema: SURFACE_SCHEMA },
  )
))
log(`Implement done: ${impl.filter(Boolean).map(r => `${r.surface.slice(0, 14)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── Integrate (serial: green build + commit) ────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `5 agents just wired round-2 features into the Reclass GPUI UI at ${OUT} (code view → real generated C/C++, scanner panel mounted+finished, editor hover/selection/icons, workspace context menu, status bar). Make the FULL build authoritative-green.\n` +
  `Their notes: ${JSON.stringify(impl.filter(Boolean).map(r => ({ s: r.surface, ok: r.compiles, notes: (r.notes || '').slice(0, 200) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/meaningful warnings (likely seams: docks.rs mounting ScannerPanel, tabs.rs calling generator, design.rs helper signatures). 2) Confirm logic tests green: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\`. 3) \`cargo fmt\`, then commit: \`cd ${OUT} && git add -A && git commit -m "ui: round 2 — real code view + mounted scanner panel + editor/tree interaction states + status bar"\`. Report green + committed=<hash>.`,
  { label: 'r2:integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── Verify (serial, owns display) ───────────────────────────────────────────
phase('Verify')
const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    shots: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { surface: { type: 'string', description: 'codeview|scanner|editor|workspace|statusbar' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, issue: { type: 'string' }, fix_hint: { type: 'string' } }, required: ['surface', 'severity', 'issue', 'fix_hint'] } },
    overall: { type: 'string' },
  },
  required: ['shots', 'findings', 'overall'],
}
const verify = await agent(
  `VISUAL QA for the Reclass GPUI round-2 wiring. You OWN the headless display (only you run the app). Build is green. Capture real screenshots and judge vs ZED (aesthetic) + the reclass feature screenshots ${REF}/README_PIC*.png.\n\n` +
  `From ${OUT}: \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\` (builds+launches with a 19-struct project, prints "app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Screenshot with \`./scripts/ui.sh shot /tmp/r2_X.png\`; for fine text CROP regions: \`convert IN -crop WxH+X+Y OUT\` then Read the crops. Drive with \`./scripts/ui.sh key <keys>\`, \`./scripts/ui.sh click X Y\`, and for right-click: \`export DISPLAY=:99; xdotool mousemove X Y click 3\`.\n\n` +
  `CAPTURE + JUDGE these (the round-2 deliverables):\n` +
  `  1. /tmp/r2_loaded.png — loaded project: confirm the SCANNER panel now shows a REAL UI docked at the bottom (toolbar, inputs, checkboxes, Scan/Re-scan, results table headers, count, Go to/Copy) — NOT a collapsed placeholder. Crop the bottom ~340px and Read it.\n` +
  `  2. /tmp/r2_code.png — toggle view mode to C/C++ (click the "Code"/"C/C++" toggle in the titlebar or the bottom segmented control) → confirm REAL generated C++ renders with line numbers + syntax highlighting (like reclass PIC3 right pane), not the old placeholder string. Crop + Read.\n` +
  `  3. /tmp/r2_hover.png — hover an editor node row (\`export DISPLAY=:99; xdotool mousemove 700 250; sleep 1\`) then shot → confirm a visible hover/row state; and confirm node-type ICONS + clean +offset address margin (no repeated garbage absolute address).\n` +
  `  4. /tmp/r2_ctx.png — right-click a type row in the left Project tree (find its X,Y in r2_loaded, then \`xdotool mousemove X Y click 3; sleep 1\`) → confirm a Zed context menu opens. Escape after.\n` +
  `  5. /tmp/r2_status.png — crop the very bottom status bar → confirm it shows a default readout (root type + counts/size) and right-aligned segments, not blank.\n` +
  `Be a harsh Zed-quality + reclass-parity critic. Return: shots = paths captured; findings = ranked punch-list tagged surface∈{codeview,scanner,editor,workspace,statusbar}, severity, issue, fix_hint; overall = 2-3 sentence verdict. \`./scripts/ui.sh stop\` when done.`,
  { label: 'r2:verify', phase: 'Verify', schema: VERIFY_SCHEMA })
const findings = (verify && verify.findings) || []
log(`Verify: ${findings.length} findings (${findings.filter(f => f.severity !== 'minor').length} blocker/major). ${verify ? verify.overall : ''}`)

// ── Polish (parallel by surface with findings) ──────────────────────────────
phase('Polish')
const bySurface = {}
for (const f of findings) (bySurface[f.surface] = bySurface[f.surface] || []).push(f)
const targets = SURFACES.filter(s => (bySurface[s.key] || []).length > 0)
log(`Polishing ${targets.length}: ${targets.map(s => `${s.key}(${bySurface[s.key].length})`).join(' ')}`)
const polish = await parallel(targets.map(s => () =>
  agent(
    COMMON +
    `YOUR OWNERSHIP (edit ONLY these): ${s.files.join(', ')}.\n` +
    `Reclass refs: ${s.refs.map(r => `${REF}/${r}`).join(', ')}.\n\n` +
    `POLISH pass. Visual QA filed these issues on your surface ("${s.label}") from real screenshots — FIX EACH:\n` +
    bySurface[s.key].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.issue}\n     hint: ${f.fix_hint}`).join('\n') +
    `\n\nRe-read your files, fix to Zed quality, ensure they compile (\`${BUILD}\`). Original brief for context:\n` + s.task +
    `\n\nReturn the structured result.`,
    { label: `r2polish:${s.key}`, phase: 'Polish', schema: SURFACE_SCHEMA },
  )
))
log(`Polish done: ${polish.filter(Boolean).map(r => `${r.surface.slice(0, 14)}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── Final (serial: green + final shots + commit) ────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the Reclass GPUI round-2 work at ${OUT}.\n` +
  `DO: 1) \`${BUILD}\` to FULL GREEN; confirm \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\` passes. 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: round 2 polish — Zed interaction + feature-parity refinements from QA"\`. 3) FINAL SCREENSHOTS (you own the display): \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/EPROCESS.rcx\`, maximize (export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1), capture: /tmp/r2final_loaded.png (project + scanner docked), /tmp/r2final_code.png (C/C++ view), /tmp/r2final_ctx.png (tree right-click menu). Read each to confirm they render the round-2 features (real scanner UI, real generated code). \`./scripts/ui.sh stop\` when done.\n` +
  `4) Assess closeness to ZED-quality + reclass FEATURE parity; list anything still missing (remaining[]). Return green, committed, shots, remaining, assessment.`,
  { label: 'r2:final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  implement: impl.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles, delivered: r.delivered })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { findings: findings.length, overall: verify.overall, shots: verify.shots },
  polish: polish.filter(Boolean).map(r => ({ surface: r.surface, compiles: r.compiles })),
  final: final && { green: final.green, committed: final.committed, shots: final.shots, remaining: final.remaining, assessment: final.assessment },
}
