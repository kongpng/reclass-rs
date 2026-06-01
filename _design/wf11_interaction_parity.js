export const meta = {
  name: 'reclass-interaction-parity-mega',
  description: 'HUGE interaction-parity pass: exhaustively diff EVERY editor + popup + dialog INTERACTION (click/double-click/right-click/hover/drag/wheel/keyboard, inline-edit input acceptance, selection tracking, rendering alignment) of the Rust GPUI port against the C++ Qt Reclass, then implement every missing/broken behavior. Seeded with 4 reported bugs: fold-chevron off-center, the T type-picker cursor not following up/down/hover/click, struct-NAME rename not working, and clicking the struct TYPE keyword opening a dead input box. Verify on-screen with a data-populated project.',
  whenToUse: 'A comprehensive sweep for missing/broken UI BEHAVIOR vs the C++ — interactions, inline editing, selection, rendering fidelity.',
  phases: [
    { title: 'Audit', detail: '8 parallel read-only agents exhaustively diff each interaction surface (command-row, node-rows, type-picker, popups/dialogs, rendering, keyboard/mouse, inline-edit, lifecycle/misc) vs the C++' },
    { title: 'Synthesize', detail: 'merge + dedupe + prioritize (the 4 seeded bugs are P0); assign each gap to a disjoint file-owner' },
    { title: 'Fix', detail: 'parallel fix agents (disjoint files) implement every assigned behavior against the C++' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + full tests + commit' },
    { title: 'Verify', detail: 'launch data-populated; exercise the 4 bugs + spot-check the new behaviors -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, parity report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// Seeded REPORTED BUGS (with my inline diagnoses) so the audit/fix never miss them:
const SEEDS =
  `REPORTED BUGS — fix these (P0) AND find every behavior like them:\n` +
  `  B1 FOLD CHEVRON OFF-CENTER (classes/structs/arrays): the expand chevron is rendered in a fixed 2-cell icon gutter (src/ui/editor/mod.rs ~2745: a div w(ICON_CELLS*cell), items_center + justify_center, Icon size = line_height*0.62). It reads off-center/misaligned vs the C++. Align it to the C++ (vertical baseline + horizontal position relative to the tree connector / type token); compare to the reference screenshots.\n` +
  `  B2 TYPE-PICKER (T) CURSOR DOESN'T TRACK: in src/ui/typeselectorpopup.rs the popup focuses the filter InputState, so Up/Down/Enter and mouse HOVER never reach move_up()/move_down() (which EXIST, ~684/694) — the selection highlight is static. Fix: route Up/Down/PageUp/Down/Home/End + Enter to move/confirm even while the filter input holds focus (bind on the popup's key_context AND/OR forward from the input); add HOVER-to-select on each row; make a CLICK select-then-confirm; ensure the highlighted row is always visible + scrolls into view. Match the C++ list behavior (typeselectorpopup.cpp).\n` +
  `  B3 STRUCT-NAME RENAME BROKEN: clicking the class/struct NAME in the header does not let you rename it. The code path exists (on_row_mouse_down → CommandRow → begin_inline_edit(RootClassName)) but the field either doesn't open or doesn't accept input. Make clicking the root name open an inline edit that ACCEPTS KEYSTROKES and commits the rename (C++ renames the root struct).\n` +
  `  B4 STRUCT-TYPE KEYWORD CLICK = DEAD INPUT BOX: clicking the "struct"/"class"/"enum" keyword (RootClassType) opens a highlighted input box that does NOT accept input. Mirror the C++ behavior exactly (read editor.cpp + controller.cpp for what clicking the root TYPE keyword does — likely CYCLES struct↔class or opens a small picker, NOT a free-text edit). Whatever it is, replicate it; and ensure NO context opens a dead/un-typeable input box.\n` +
  `  CROSS-CUTTING: B3+B4 imply COMMAND-ROW inline edits may not accept keyboard input at all (the field opens/highlights but keystrokes don't land). Verify the inline-edit field installs its input handler + receives focus + accepts text in EVERY context (data-row name/value/comment, array count, AND the command-row root name/address) — fix any context where the box opens but typing does nothing.\n`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' }, summary: { type: 'string' },
    gaps: { type: 'array', items: {
      type: 'object', additionalProperties: false,
      properties: {
        behavior: { type: 'string', description: 'the user-facing interaction, e.g. "double-click a pointer follows it"' },
        cpp_ref: { type: 'string' },
        rust_status: { type: 'string', enum: ['works', 'partial', 'broken', 'missing'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' },
        severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
        reported: { type: 'boolean', description: 'true if one of the 4 seeded B1-B4 bugs' },
      },
      required: ['behavior', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
    } },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT (8 parallel, read-only) ──────────────────────────────────
const AUDIT_HEADER =
  `You are auditing INTERACTION PARITY of the Rust+GPUI port of "Reclass" (${OUT}/src) against the AUTHORITATIVE C++ Qt implementation (${QT}). Find every interaction (click/double-click/right-click/hover/drag/wheel/keyboard, inline-edit input, selection, rendering alignment) that is missing/broken/partial vs the C++. Trace end-to-end and be skeptical: "the handler exists" ≠ "it works"; "a box opens" ≠ "you can type in it". READ-ONLY.\n\n` +
  SEEDS + `\n` +
  `Reference screenshots in ${CREF}/*.png (esp. png_reference_c++.png = data-populated C++; the right-click / source / type-picker / address-hover crops). Produce an EXHAUSTIVE gap list (behavior, cpp_ref, rust_status, fix_file, fix_hint, severity, reported). Completeness over brevity.\n\n`

const AUDIT_DOMAINS = [
  { key: 'command_row', cpp: ['editor.cpp', 'controller.cpp'], rust: ['src/ui/editor/mod.rs', 'src/ui/editor/hit_test.rs'],
    focus: `CLASS-HEADER (command row) interactions: the chevron [▸] (→ root type selector / collapse), the source chip ▾ (→ data-source picker), the base ADDRESS (→ edit + the address-format hover), the root struct NAME (→ rename — B3), the root TYPE keyword struct/class/enum (→ B4: what does the C++ do — cycle? picker?), and the trailing "{". For EACH: trigger + result + does an opened inline edit ACCEPT INPUT. Pin B3/B4 root causes.` },
  { key: 'node_rows', cpp: ['editor.cpp', 'controller.cpp', 'compose.cpp'], rust: ['src/ui/editor/mod.rs', 'src/ui/editor/selection.rs'],
    focus: `NODE-ROW interactions: click-select, click-already-selected→edit, double-click (pointer/struct follow / expand), the FULL right-click context menu (every item wired to a real controller op — New Class, Ptr to New Class, type-cycle, Rename, Change Type, Insert, Convert, Big endian, Static, Duplicate, Delete, Fold, Copy, Tracking, Copy as C Struct), fold/collapse, hex BYTE select + in-place hex edit (typing hex digits overwrites bytes), pointer-follow, array index nav, drag-reorder. Which are missing/broken?` },
  { key: 'type_picker', cpp: ['typeselectorpopup.cpp', 'typeselectorpopup.h'], rust: ['src/ui/typeselectorpopup.rs'],
    focus: `TYPE SELECTOR popup (T / Change Type / chevron / type token): B2 — selection cursor must follow Up/Down/PageUp/Down/Home/End + Enter (currently focus is on the filter input so they don't), HOVER must highlight the row under the pointer, CLICK must select+confirm, the highlighted row must scroll into view. Plus: filter typing narrows + re-ranks, category tabs filter, the * ** [] modifier buttons apply, +New creates a type, OK confirms, Esc cancels, and the popup opens in the right MODE (FieldType/ArrayElement/PointerTarget/Root). Diff every interaction vs the C++.` },
  { key: 'inline_edit', cpp: ['editor.cpp'], rust: ['src/ui/editor/inline_edit.rs', 'src/ui/editor/mod.rs', 'src/ui/editor/tab_cycle.rs'],
    focus: `INLINE-EDIT FIELD input acceptance in EVERY context (the B3/B4 cross-cutting bug): when a field opens (data-row name/value/comment, array count, command-row root name/address), does it (a) receive focus, (b) install the text input handler so KEYSTROKES land, (c) show a caret, (d) commit on Enter/Tab/focus-loss, (e) cancel on Esc, (f) Tab-cycle to the next field? Find any context where the box opens but typing does nothing (esp. the command row). Also hex-overwrite typing on hex rows.` },
  { key: 'popups_dialogs', cpp: ['sourcechooserpopup.cpp', 'hextoolbarpopup.cpp', 'gotoaddressdialog.h', 'optionsdialog.cpp', 'processpicker.cpp'], rust: ['src/ui/sourcechooser.rs', 'src/ui/enumpicker.rs', 'src/ui/hextoolbar.rs', 'src/ui/gotoaddress.rs', 'src/ui/findbar.rs', 'src/ui/optionsdialog.rs', 'src/ui/processpicker.rs', 'src/ui/contextmenu.rs'],
    focus: `Every OTHER popup/dialog's interactions (same scrutiny as the type picker): source chooser (keyboard nav + pick switches source), enum picker (member nav + pick sets value), hex toolbar (size 8/16/32/64/128 + join/split), goto-address (input + parse + navigate + recents), find bar (search + next/prev + match count), options (each setting applies + persists), process picker (list + attach). Selection tracking, input acceptance, confirm/cancel — what's missing/broken?` },
  { key: 'rendering', cpp: ['editor.cpp', 'compose.cpp', 'format.cpp'], rust: ['src/ui/editor/mod.rs', 'src/ui/editor/element.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/palette.rs'],
    focus: `RENDERING FIDELITY vs the C++ (and the reference screenshots): B1 — the fold CHEVRON / kind-icon alignment in the gutter (vertical baseline + horizontal position; it reads off-center). Also: tree connector lines, indent guides, column alignment, the change-HEAT byte coloring (orange→red), the value-changed highlight, comment/type-hint rendering, selection/hover bands, the footer pills, ASCII column. Pixel-level diffs that look wrong vs png_reference_c++.png.` },
  { key: 'keyboard_mouse', cpp: ['editor.cpp', 'editor.h'], rust: ['src/ui/editor/mod.rs'],
    focus: `COMPREHENSIVE keyPressEvent + mouse diff (beyond what's wired): every key + modifier (arrows + Shift/Ctrl/Alt combos, PageUp/Down, Home/End, Enter, Insert, Delete, F2/F12, T, P/F/S/U, digits 1-5, Ctrl+C/V/X/D/A/Z/Y/F, Esc, Tab, semicolon, brackets) and every mouse gesture (single/double/right click, Shift/Ctrl click multi-select, drag-select, drag-reorder, wheel, hover). For each: present + correct in the Rust? Enumerate the gaps.` },
  { key: 'lifecycle_misc', cpp: ['mainwindow.h', 'main.cpp', 'editor.cpp'], rust: ['src/ui/window.rs', 'src/ui/tabs.rs', 'src/ui/workspace.rs', 'src/ui/scannerpanel.rs', 'src/ui/statusbar.rs'],
    focus: `Everything else behavioral: tab open/close/reorder/switch + per-tab state, dock toggles + the right Modules/Bookmarks docks acting, scanner run/next/undo, the live REFRESH loop (values update), value WRITE-BACK (editing a value writes to the provider), hover popups (value-history / struct-preview / disasm) populating, status-bar live readout, view-option toggles' real render effect, workspace tree actions. What's stubbed/broken?` },
]

phase('Audit')
log(`HUGE audit: ${AUDIT_DOMAINS.length} interaction domains vs the C++ in parallel.`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\n` +
    `READ the C++: ${d.cpp.map(f => `${QT}/${f}`).join(', ')}.\nREAD the Rust: ${d.rust.map(f => `${OUT}/${f}`).join(', ')}.\n\n` +
    `Exhaustive gap list — every broken/missing/partial interaction.`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit: ${allGaps.length} gaps (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.reported).length} reported).`)

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
const OWNER_FILES = {
  editor: 'src/ui/editor/* (command-row + node-row interactions, chevron/render alignment, keyboard/mouse, inline-edit input)',
  popups: 'src/ui/typeselectorpopup.rs, sourcechooser.rs, enumpicker.rs, hextoolbar.rs, contextmenu.rs, tooltip.rs',
  dialogs: 'src/ui/gotoaddress.rs, findbar.rs, optionsdialog.rs, processpicker.rs, messagebox.rs, dialogs.rs',
  chrome: 'src/ui/window.rs, tabs.rs, workspace.rs, statusbar.rs, menubar.rs, scannerpanel.rs, docks.rs, panels.rs, startpage.rs',
  display: 'src/compose.rs, src/format.rs',
  controller: 'src/controller.rs',
}
const synth = await agent(
  `Synthesis of a HUGE C++-vs-Rust INTERACTION-parity audit (${allGaps.length} raw gaps):\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, b: g.behavior, st: g.rust_status, f: g.fix_file, sev: g.severity, rep: !!g.reported, hint: (g.fix_hint || '').slice(0, 130) }))) +
  `\n\nDE-DUPLICATE, drop truly-working items, PRIORITIZE (the 4 reported bugs B1-B4 are P0/blocker; then broken, then missing). For each surviving item return: behavior, owner (editor|popups|dialogs|chrome|display|controller — by fix_file: editor/*→editor; typeselectorpopup/sourcechooser/enumpicker/hextoolbar/contextmenu/tooltip→popups; gotoaddress/findbar/optionsdialog/processpicker/messagebox/dialogs→dialogs; window/tabs/workspace/statusbar/menubar/scannerpanel/docks/panels/startpage→chrome; compose/format→display; controller→controller), fix_file, fix_hint, severity, reported. Keep it comprehensive — this is meant to be a big pass.`,
  { label: 'synthesize', phase: 'Synthesize', schema: {
    type: 'object', additionalProperties: false,
    properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        behavior: { type: 'string' }, owner: { type: 'string', enum: ['editor', 'popups', 'dialogs', 'chrome', 'display', 'controller'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, reported: { type: 'boolean' },
      }, required: ['behavior', 'owner', 'fix_hint', 'severity'] } },
      overview: { type: 'string' },
    }, required: ['items', 'overview'],
  } })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by owner) ────────────────────────────────────────
phase('Fix')
const FIX_CPP = {
  editor: `${QT}/editor.cpp, ${QT}/editor.h, ${QT}/controller.cpp`,
  popups: `${QT}/typeselectorpopup.cpp, ${QT}/sourcechooserpopup.cpp, ${QT}/hextoolbarpopup.cpp`,
  dialogs: `${QT}/gotoaddressdialog.h, ${QT}/optionsdialog.cpp, ${QT}/processpicker.cpp`,
  chrome: `${QT}/mainwindow.h, ${QT}/main.cpp`,
  display: `${QT}/compose.cpp, ${QT}/format.cpp`,
  controller: `${QT}/controller.cpp, ${QT}/controller.h`,
}
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Implement INTERACTION-parity fixes in the Reclass Rust port (${OUT}) vs the C++ Qt Reclass. The logic layer is implemented — wire/fix behavior; touch logic modules only as truly needed (say so). Aesthetic = Zed (use ${OUT}/src/ui/design.rs tokens + design::icon_*; resolved mono font).\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch src/ui/mod.rs, other owners' files. Do NOT run git/cargo fmt (later stage). Keep #[cfg(test)] tests + add tests for pure helpers.\n` +
    `AUTHORITATIVE C++: ${FIX_CPP[owner]}. Reference screenshots: ${CREF}/*.png.\n\n` +
    SEEDS + `\n` +
    `IMPLEMENT these audited behaviors (make EACH work end-to-end exactly like the C++):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. ${it.reported ? '[REPORTED P0] ' : ''}[${it.severity}] ${it.behavior}\n     fix: ${it.fix_hint}`).join('\n') +
    `\n\nEnsure your files compile (\`${BUILD}\`) — other agents edit other files concurrently. Return {owner, compiles, fixed:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `A HUGE interaction-parity pass just landed across the Reclass GPUI port (${OUT}). Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, notes: (r.notes || '').slice(0, 160) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (likely seams: editor↔popups open/apply contracts, controller ops added, new key bindings). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) Tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`. 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: interaction parity — command-row rename/type, type-picker cursor, chevron alignment, inline-edit input, + audited behaviors (C++)"\`. Report green + committed=<hash>.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display) ───────────────────────────────────────
phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the HUGE interaction-parity pass. You OWN the headless display. ALWAYS rebuild the ui binary first: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`.\n\n` +
  `SETUP (data-populated so values + interactions are real): \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/m_X.png\`; CROP + Read. Drive: \`./scripts/ui.sh key/click/type\`, right-click \`xdotool mousemove X Y click 3\`.\n\n` +
  `VERIFY the 4 REPORTED bugs FIRST (find coords from a full shot):\n` +
  `  1. /tmp/m_chevron.png — crop the class header + a couple struct/array fold-head rows: is the expand chevron now properly aligned (vertically centered on the row, consistent horizontal position)? (B1)\n` +
  `  2. /tmp/m_typepicker.png — click a node to select, press \`t\` → type picker opens; press \`Down\`/\`Down\` and shot — the highlighted row must MOVE; \`xdotool mousemove\` over a different row and shot — it must highlight under the pointer. (B2)\n` +
  `  3. /tmp/m_rename.png — click the root struct NAME in the header, then \`./scripts/ui.sh type Renamed\` and shot — the name must CHANGE to the typed text (the box must accept input). (B3)\n` +
  `  4. /tmp/m_typeclick.png — click the "struct"/"class" KEYWORD in the header → confirm it does the C++ thing (cycle/picker) and is NOT a dead input box. (B4)\n` +
  `Then spot-check 4-6 other newly-implemented behaviors from the audit (e.g. a context-menu action mutates the tree, double-click follows a pointer, hex-byte typing overwrites, a dialog accepts input).\n` +
  `Return: shots, findings=[{item, severity, works(bool), detail}] (tag the 4 reported bugs), overall (do B1-B4 work now?). \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { item: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['item', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner with failing findings) ───────────────
phase('Polish')
const byOwner = {}
for (const f of fails) {
  const t = (f.item + ' ' + f.detail).toLowerCase()
  const o = /type ?picker|type ?selector|source chooser|enum|hex toolbar|popup/.test(t) ? 'popups'
    : /dialog|goto|find|options|process/.test(t) ? 'dialogs'
      : /tab|dock|scanner|status|workspace|menu|start/.test(t) ? 'chrome'
        : /value|float|ascii|hex byte|comment|enum label|format|render value/.test(t) ? 'display'
          : 'editor'
  ;(byOwner[o] = byOwner[o] || []).push(f)
}
const polishOwners = Object.keys(byOwner)
log(`Polishing ${polishOwners.length}: ${polishOwners.map(o => `${o}(${byOwner[o].length})`).join(' ')}`)
if (polishOwners.length) {
  await parallel(polishOwners.map(owner => () =>
    agent(
      `Fix-up pass (${OUT}). YOUR OWNERSHIP (edit ONLY): ${OWNER_FILES[owner]}. C++: ${FIX_CPP[owner]}. Aesthetic = Zed. Do NOT run git/cargo fmt.\n` +
      SEEDS + `\n` +
      `On-screen QA found these NOT working — fix each end-to-end vs the C++:\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.item}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

// ── PHASE 7: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the HUGE interaction-parity pass (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: interaction parity polish — QA fixes for command-row/type-picker/chevron/inline-edit + audited behaviors"\`. 3) FINAL SCREENSHOTS: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`, maximize, capture /tmp/mfinal_chevron.png, /tmp/mfinal_typepicker.png (T + Down), /tmp/mfinal_rename.png (click name + type). Read each. \`./scripts/ui.sh stop\`.\n` +
  `4) Honest PARITY REPORT: B1-B4 status (work now?), how many audited behaviors landed, what remains. Return {green, committed, b1_b4:[{bug,works}], shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, b1_b4: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { bug: { type: 'string' }, works: { type: 'boolean' } }, required: ['bug', 'works'] } }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  audit: { total: allGaps.length, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, fixed: (r.fixed || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, b1_b4: final.b1_b4, remaining: final.remaining, assessment: final.assessment },
}
