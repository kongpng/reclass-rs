export const meta = {
  name: 'reclass-parity-second-pass',
  description: 'SECOND-PASS parity sweep: the prior deep pass audited the C++ Qt Reclass module-by-module and implemented ~52 feature-level capabilities. This pass hunts the LAST 20% a feature-level pass MISSES — exact behavioral semantics at boundaries, EVERY individual menu item/accelerator/enabled-state, settings persistence field-by-field, .rcx/ReClass.NET-XML round-trip FIDELITY, second-order rendering/format nuances, every View toggle effect, hover/tooltip/status-bar/context-menu completeness, clipboard/undo/find coverage, and generator/importer BYTE fidelity on hard cases — then implements the gaps. Verify on-screen (screenshots are 2x-downscaled; drive with 2x coords).',
  whenToUse: 'Follow-up parity sweep after a feature-level pass — find the missed edge cases, exact semantics, and fidelity gaps, and implement them.',
  phases: [
    { title: 'Audit', detail: '9 parallel read-only agents hunt second-order/edge-case/fidelity gaps the feature-level pass missed' },
    { title: 'Synthesize', detail: 'merge + dedupe against the ~52 already-done features + prioritize; assign each to a disjoint owner' },
    { title: 'Fix', detail: 'parallel owners implement the missed behaviors faithfully against the C++' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + full test suite + commit' },
    { title: 'Verify', detail: 'launch data-populated; drive with 2x coords; exercise the newly-closed gaps -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, missed-gap report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// What the prior deep pass already implemented — audit agents must NOT re-report these
// as missing; only find what they MISS (edge cases / exact semantics / fidelity).
const ALREADY = `ALREADY IMPLEMENTED by the prior deep pass (do NOT re-report these as missing — only find the MISSED details/edge-cases within them):
- Scanner: 9 comparison modes (exact/bigger/smaller/between/changed/unchanged/increased/decreased/unknown), first->next-scan narrowing, undo-scan, int8..64/uint/float/double/string/AOB types, FILE-source scanning, results table + add-as-nodes.
- Node model: 30+ NodeKinds (hex8..128, int/uint 8..128, float16/float/double, bool, ptr32/64, funcptr, vec2-4, mat4x4, utf8/16, struct/array/union/bitfield/enum) + ops add/insert/delete/duplicate/change-type/wrap/unwrap/reorder/copy-paste.
- Compose/format: multi-type value interpretation, ASCII+hex, change-heat, comments/hints, enum labels.
- Generators: C++ header / Rust / #define-offsets / C# / Python-ctypes; importers: PDB / PE-debug / ReClass.NET-XML / source.
- Addr parser: hex/decimal, module-base + offset, deref [expr], symbol mod!Sym, full arithmetic; Go-to-Address dialog with live eval.
- RTTI walking logic (MSVC + Itanium). Dialogs: Options, Type Aliases, Profiler (Ctrl+Shift+F), Process Picker, Goto-Address, Bookmarks, Plugins, RTTI Browser.
- App shell: 59 wired menu actions across File/Edit/View/Tools/Plugins/Help, recent files, data-source auto-attach on open, examples, Reclass/Code view toggle.
- Editor keyboard (VERIFIED WORKING ON-SCREEN): focus-on-click, arrow nav, Down-at-end grows the struct, Shift+arrow multi-select, Ctrl+D duplicate, Ctrl+C/X/V node clipboard, Ctrl+Shift+Up/Down reorder, inline-edit text input.`

const PLATFORM_STUBS = `INTENTIONAL platform-boundary stubs — do NOT report these (no live OS process-memory backend exists on this host): live process attach + live memory scanning, RTTI over a live vtable, Microsoft symbol-store network download, disasm hover over live code bytes, dynamic native-plugin (.so/.dll) loading, always-on MCP bridge. The logic up to the read_process_memory seam is ported + tested; the stubs are deliberate and clearly labeled.`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' }, summary: { type: 'string' },
    gaps: { type: 'array', items: {
      type: 'object', additionalProperties: false,
      properties: {
        feature: { type: 'string', description: 'a SPECIFIC missed behavior/edge-case/fidelity gap, e.g. "Delete on the last child of a struct re-selects the previous sibling (Rust leaves nothing selected)"' },
        cpp_ref: { type: 'string', description: 'C++ file:symbol/line that defines the exact behavior' },
        rust_status: { type: 'string', enum: ['present', 'partial', 'wrong', 'stub', 'missing'] },
        fix_file: { type: 'string', description: 'the Rust file(s) that should implement/correct it' },
        fix_hint: { type: 'string' },
        severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
        logic_or_ui: { type: 'string', enum: ['logic', 'ui', 'both'] },
      },
      required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
    } },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT — 9 "what gets missed on a first pass" angles ────────────
const AUDIT_HEADER =
  `You are doing a SECOND-PASS parity audit of the Rust+GPUI port of "Reclass" (${OUT}) against the AUTHORITATIVE C++ Qt source (${QT}). A prior deep pass already ported ~52 module/feature-level capabilities. THE FEATURES EXIST — your job is to find the LAST 20% a feature-level pass MISSES: exact behavioral semantics at boundaries, individual menu items / accelerators / enabled-disabled state, settings persistence field-by-field, file-format round-trip FIDELITY, second-order rendering/format nuances, and per-case correctness. Be EXHAUSTIVE and SKEPTICAL — assume "feature present" hides "behaves subtly differently / misses an edge case / drops a field on save". A capability that exists but DIVERGES from the C++ (use rust_status:"wrong") is exactly what this pass exists to catch. Read your C++ source THOROUGHLY (in chunks; enumerate every branch/option/menu-entry), then diff against the Rust. READ-ONLY.\n\n` +
  ALREADY + `\n\n` + PLATFORM_STUBS + `\n\n` +
  `For each gap: feature (the SPECIFIC missed behavior), cpp_ref (file:symbol/line), rust_status (present/partial/wrong/stub/missing — use "wrong" for diverging behavior), fix_file, fix_hint, severity, logic_or_ui. Skip the platform stubs above and anything that genuinely matches the C++. Reference screenshots: ${CREF}/*.png.\n\n`

const AUDIT_DOMAINS = [
  { key: 'editor_semantics', cpp: ['editor.cpp', 'editor.h', 'controller.cpp'], rust: ['src/ui/editor/', 'src/controller.rs', 'src/controller/'],
    focus: `EXACT EDITOR/CONTROLLER BEHAVIORAL SEMANTICS at boundaries. For EVERY mutating op, diff the precise post-conditions vs C++: what gets SELECTED after delete/insert/duplicate/paste (re-select sibling? parent? nothing?); array resize at 0 / shrink-below-used; deleting the last child / a nested struct; offset recomputation after reorder; big-endian toggle behavior per node-kind; converting a node with children; wrap/unwrap edge cases; Down-at-end vs Down-mid; caret/anchor after multi-select ops; collapse/expand state preservation across mutations; undo/redo of EACH op restoring exact prior state (selection included). Find where the Rust DIVERGES (rust_status:"wrong") or misses an edge case.` },
  { key: 'menus_accel', cpp: ['main.cpp', 'mainwindow.h', 'editor.cpp'], rust: ['src/ui/window.rs', 'src/ui/menubar.rs', 'src/ui/commandpalette.rs', 'src/ui/editor/mod.rs'],
    focus: `EVERY individual MENU ITEM, TOOLBAR button, CONTEXT-MENU entry, and KEYBOARD ACCELERATOR (read main.cpp menu construction + editor context menu in full). For EACH: does the Rust have the exact same item, the SAME accelerator/shortcut, the SAME enabled/disabled (greyed) state logic (e.g. Paste disabled when clipboard empty; Undo disabled at history start; ops disabled with no selection), and the SAME submenu structure + item order? Find missing items, wrong/missing shortcuts, and missing enable-state logic.` },
  { key: 'settings_persist', cpp: ['optionsdialog.cpp', 'optionsdialog.h', 'main.cpp', 'core.h'], rust: ['src/ui/optionsdialog.rs', 'src/ui/window.rs', 'src/settings.rs', 'src/config.rs'],
    focus: `SETTINGS COMPLETENESS + PERSISTENCE field-by-field. Enumerate EVERY QSettings key the C++ reads/writes (grep settings/QSettings/value(/setValue) and every control in optionsdialog) — refresh rate, theme, fonts, colors, address mode, default node sizes, hex column count, recent files, window geometry, dock layout, scanner defaults, type aliases, plugin enable-flags, etc. For EACH: does the Rust have the setting, PERSIST it across restart, and APPLY it? Find settings that exist in C++ but aren't persisted/applied, or aren't surfaced in the Rust Options dialog.` },
  { key: 'file_fidelity', cpp: ['serialize', 'core.h', 'controller.cpp', 'imports/import_source.cpp'], rust: ['src/rcx.rs', 'src/document.rs', 'src/core/', 'src/imports/'],
    focus: `.rcx + ReClass.NET-XML ROUND-TRIP FIDELITY. Enumerate EVERY field the C++ serializes per node/struct/project (name, comment, offset, hidden/collapsed flag, color, bytes-per-row, node metadata, custom type defs, enum members, savedSources, base-address formula, type aliases, bookmarks, view settings). For EACH: does the Rust SAVE and RELOAD it losslessly (save -> reload -> byte/semantically identical)? Find dropped/ignored fields on save or load, version-compat gaps, and ReClass.NET import/export attributes that aren't round-tripped. (Look at the actual C++ (de)serialization code.)` },
  { key: 'render_nuance', cpp: ['compose.cpp', 'format.cpp', 'editor.cpp'], rust: ['src/compose.rs', 'src/compose/', 'src/format.rs', 'src/ui/editor/'],
    focus: `SECOND-ORDER RENDERING/FORMAT nuances. Diff exact output vs C++: per-node-kind COLOR coding (type token vs name vs value vs comment vs hidden), change-heat color RAMP + decay timing, the precise multi-type value-interpretation set per kind (which extra interpretations show + their format/precision), non-printable ASCII rendering (dots vs hex), hidden-node display, nested-struct inline PREVIEW text, pointer-chain/NULL/bad-pointer display, the selected-row + multi-select visuals, address vs offset column formatting + leading zeros + the 0x prefix, big-endian value display, float precision/NaN/inf. Find any divergence (rust_status:"wrong").` },
  { key: 'view_display', cpp: ['main.cpp', 'editor.cpp', 'mainwindow.h'], rust: ['src/ui/window.rs', 'src/ui/editor/', 'src/ui/menubar.rs'],
    focus: `VIEW MENU + DISPLAY MODES, each toggle's EFFECT. Enumerate every View toggle in C++ (show/hide comments, addresses, offsets, type column, hex preview, plugins column, text/ASCII), split-editor / unsplit, presentation mode, font size +/- (Ctrl+= / Ctrl+-), theme switch live-apply, reset-window-layout, dock show/hide (project tree, scanner, modules, output). For EACH: does the Rust have it AND does toggling actually change the render/layout to match C++? Find toggles that are missing, no-ops, or wrong.` },
  { key: 'hover_status_ctx', cpp: ['rcxtooltip.h', 'editor.cpp', 'main.cpp'], rust: ['src/ui/editor/', 'src/ui/tooltip.rs', 'src/ui/statusbar.rs'],
    focus: `HOVER POPUPS, TOOLTIPS, STATUS BAR, CONTEXT MENUS — completeness + content. The rcxtooltip behaviors (what each hover shows: value history graph, struct preview, pointer target preview, disasm); the STATUS BAR fields (selected node path, offset, type, size, byte count, data source, theme — and their LIVE updates on selection/refresh); the editor right-click CONTEXT MENU (every item + submenu vs editor.cpp's menu builder). Find missing hover content, missing/wrong status-bar fields, missing context-menu items.` },
  { key: 'clip_undo_find', cpp: ['clipboard.h', 'editor.cpp', 'controller.cpp'], rust: ['src/ui/editor/', 'src/controller.rs', 'src/ui/findbar.rs', 'src/clipboard.rs'],
    focus: `CLIPBOARD + UNDO/REDO + FIND coverage. Node clipboard FIDELITY: copy/cut/paste fidelity incl. cross-struct, "copy as C struct", and the ReClass.NET XML clipboard interchange FORMAT (does Rust write/read the exact clipboard format C++ uses?). UNDO/REDO: is EVERY mutating op undoable + redoable with exact state restoration (incl. paste, multi-delete, reorder, type-change, comment edit, offset edit)? FIND: in-editor find (next/prev, wrap, highlight all, match name/value/offset). Find coverage gaps + format mismatches.` },
  { key: 'gen_import_fidelity', cpp: ['generator.cpp', 'generator.h', 'imports/import_source.cpp'], rust: ['src/generator.rs', 'src/generator/', 'src/imports/'],
    focus: `GENERATOR + IMPORTER BYTE FIDELITY on HARD cases. For each export format diff the EXACT output vs C++ for: nested anonymous structs/unions, bitfields (packing + width syntax), enums (underlying type + explicit values), arrays-of-pointers + multi-dim arrays, vtable generation (function pointer table + signatures), padding/alignment (#pragma pack, static_assert(sizeof/offsetof)), forward declarations + include ordering, name sanitization/collisions, base-class layout. And importer edge cases (malformed input, unknown types, partial parse). Find where output diverges from C++ byte-for-byte (rust_status:"wrong").` },
]

phase('Audit')
log(`Second-pass audit: ${AUDIT_DOMAINS.length} "what gets missed" angles in parallel.`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\n` +
    `READ the C++ thoroughly (in chunks): ${d.cpp.map(f => f.includes('/') || f.endsWith('.cpp') || f.endsWith('.h') ? `${QT}/${f}` : `(grep "${f}" under ${QT})`).join(', ')}.\n` +
    `READ the Rust: ${d.rust.map(f => `${OUT}/${f}`).join(', ')} (some paths may not exist — note if a whole file is absent).\n\n` +
    `Exhaustive list of MISSED behaviors / edge-cases / fidelity gaps. Prefer specific, reproducible divergences over vague "could be improved".`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit: ${allGaps.length} missed gaps (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.rust_status === 'wrong').length} diverging, ${allGaps.filter(g => g.rust_status === 'missing').length} missing).`)

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
const OWNER_FILES = {
  editor: 'src/ui/editor/, src/ui/tooltip.rs, src/ui/statusbar.rs, src/ui/findbar.rs',
  core_ctrl: 'src/controller.rs, src/controller/, src/core/, src/clipboard.rs',
  compose_fmt: 'src/compose.rs, src/compose/, src/format.rs',
  gen_import: 'src/generator.rs, src/generator/, src/imports/',
  fileio: 'src/rcx.rs, src/document.rs',
  app_menus: 'src/ui/window.rs, src/ui/menubar.rs, src/ui/commandpalette.rs',
  settings: 'src/ui/optionsdialog.rs, src/settings.rs, src/config.rs, src/theme/',
}
const synth = await agent(
  `Synthesis of a SECOND-PASS parity audit (${allGaps.length} raw MISSED-gap candidates). Each is a behavior/edge-case/fidelity gap a prior feature-level pass left behind:\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feat: g.feature, st: g.rust_status, f: g.fix_file, sev: g.severity, kind: g.logic_or_ui, hint: (g.fix_hint || '').slice(0, 120) }))) +
  `\n\nDE-DUPLICATE, drop anything that on reflection actually matches the C++ or is a known platform stub, and PRIORITIZE: "wrong" (diverging behavior) and fidelity/round-trip losses first (they silently corrupt user data/output), then missing edge-cases, blocker>major>minor. For each surviving item return: feature, owner (editor|core_ctrl|compose_fmt|gen_import|fileio|app_menus|settings — assign by fix_file), fix_file, fix_hint, severity. Keep it COMPREHENSIVE — the point of a second pass is to catch the subtle stuff, not trim it. Group related gaps so one owner ships a coherent fix.`,
  { label: 'synthesize', phase: 'Synthesize', schema: {
    type: 'object', additionalProperties: false,
    properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        feature: { type: 'string' }, owner: { type: 'string', enum: ['editor', 'core_ctrl', 'compose_fmt', 'gen_import', 'fileio', 'app_menus', 'settings'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
      overview: { type: 'string' },
    }, required: ['items', 'overview'],
  } })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} missed-gap items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by owner) ────────────────────────────────────────
phase('Fix')
const FIX_CPP = {
  editor: `${QT}/editor.cpp, ${QT}/editor.h, ${QT}/rcxtooltip.h`,
  core_ctrl: `${QT}/controller.cpp, ${QT}/controller.h, ${QT}/core.h, ${QT}/clipboard.h`,
  compose_fmt: `${QT}/compose.cpp, ${QT}/format.cpp`,
  gen_import: `${QT}/generator.cpp, ${QT}/imports/import_source.cpp`,
  fileio: `${QT}/controller.cpp (serialize), ${QT}/core.h, ${QT}/imports/import_source.cpp`,
  app_menus: `${QT}/main.cpp, ${QT}/mainwindow.h`,
  settings: `${QT}/optionsdialog.cpp, ${QT}/main.cpp (QSettings)`,
}
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Correct MISSED behaviors / edge-cases / fidelity gaps in the Reclass Rust port (${OUT}) to match the C++ Qt original EXACTLY. This is a precision second pass: port the precise semantics (post-conditions, selection behavior, serialized fields, byte-exact output), not approximations. Add/extend #[cfg(test)] tests pinning the corrected behavior. Aesthetic = Zed (design.rs tokens) for any UI.\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch src/ui/mod.rs or other owners' files. Do NOT run git/cargo fmt (later stage). Keep the build green + existing tests passing (if a test encoded the OLD wrong behavior, update it and say so).\n` +
    `AUTHORITATIVE C++: ${FIX_CPP[owner]}. Reference: ${CREF}/*.png.\n\n` +
    `FIX these audited MISSED items (match the C++ behavior precisely):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}] ${it.feature}\n     fix: ${it.fix_hint}`).join('\n') +
    `\n\nWork through as many as you can to completion. If an item turns out to already match the C++ (false positive), skip it and say so in notes rather than churn. Ensure your files compile (\`${BUILD}\`) — other owners edit other files concurrently. Return {owner, compiles, implemented:[...], false_positives:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, implemented: { type: 'array', items: { type: 'string' } }, false_positives: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}(${(r.implemented || []).length})`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `A SECOND-PASS parity correction just landed across the Reclass port (${OUT}) — editor/controller semantics, menus/accelerators/enable-state, settings persistence, file round-trip fidelity, render nuances, view toggles, hover/status/context, clipboard/undo/find, generator/importer fidelity. Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, impl: (r.implemented || []).length, notes: (r.notes || '').slice(0, 150) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (likely seams: new controller post-conditions used by editor, new serialized fields in rcx/document, settings keys, generator signatures). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) FULL tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\` — fix newly-broken tests (or update ones that pinned old wrong behavior). 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "fix: second-pass C++ parity — editor/controller semantics, menu enable-state, settings persistence, .rcx fidelity, render nuances, generator fidelity (audited)"\`. Report green + committed=<hash> + anything still off.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display) ───────────────────────────────────────
const VERIFY_HARNESS =
  `You OWN the headless display. ALWAYS rebuild the ui binary first: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\` (the headless build overwrites the ui binary).\n` +
  `SETUP: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/v_X.png\`; CROP with ImageMagick \`convert\` + Read.\n` +
  `*** CRITICAL COORDINATE RULE (this tripped the last verifier into a FALSE NEGATIVE): the Xvfb is 1680x1050 but \`./scripts/ui.sh shot\` outputs a 2x-DOWNSCALED png (~840px wide). xdotool uses REAL screen coords. So to click a row you see at screenshot pixel (sx,sy), click \`xdotool mousemove $((2*sx)) $((2*sy)) click 1\`. If you click raw screenshot coords you'll hit the sidebar/empty space, select nothing, and wrongly conclude "nothing works". ***\n` +
  `KEYBOARD IS CONFIRMED WORKING: after clicking a NAME/TYPE token (2x coords) to select a node, arrow keys navigate, Down-at-end grows the struct, Shift+arrow multi-selects, Ctrl+D duplicates, Ctrl+C/V copy-paste nodes. Do NOT re-report keyboard as broken — if a key seems dead, first re-check you selected a node at 2x coords.\n`

phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the SECOND-PASS parity corrections.\n\n` + VERIFY_HARNESS + `\n` +
  `Spot-check ~8-10 of the highest-value newly-corrected behaviors from the fix notes (adapt to what landed). Examples: select a node and Delete -> confirm the CORRECT next node is selected (not nothing); duplicate/paste -> confirm placement + selection match C++; toggle a View option -> confirm the render actually changes; check a menu item's enabled/disabled (greyed) state with vs without a selection / empty clipboard; save the doc, reload it, confirm a comment/offset/collapsed-state/color survived (.rcx fidelity); export C++ header and eyeball a hard case (bitfield/enum/nested); hover a value -> confirm the popup content; check the status bar updates on selection. For each: does it now match the C++?\n` +
  `Return: shots, findings=[{feature, severity, works(bool), detail}], overall. \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { feature: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['feature', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner) ─────────────────────────────────────
phase('Polish')
const ownerOf = (f) => {
  const t = (f.feature + ' ' + f.detail).toLowerCase()
  if (/save|reload|round-trip|\.rcx|serializ|fidelity|persist field|load/.test(t)) return 'fileio'
  if (/setting|option|persist|theme|font size|refresh rate|config/.test(t)) return 'settings'
  if (/export|import|generat|header|bitfield gen|enum gen|xml/.test(t)) return 'gen_import'
  if (/render|format|color|heat|ascii|value interp|preview|precision/.test(t)) return 'compose_fmt'
  if (/menu|accelerator|shortcut|enabled|disabled|greyed|toolbar|view toggle/.test(t)) return 'app_menus'
  if (/select|delete|insert|duplicate|undo|redo|reorder|clipboard|paste|post-condition|node op/.test(t)) return 'core_ctrl'
  return 'editor'
}
const byOwner = {}
for (const f of fails) (byOwner[ownerOf(f)] = byOwner[ownerOf(f)] || []).push(f)
const polishOwners = Object.keys(byOwner)
log(`Polishing ${polishOwners.length}: ${polishOwners.map(o => `${o}(${byOwner[o].length})`).join(' ')}`)
if (polishOwners.length) {
  await parallel(polishOwners.map(owner => () =>
    agent(
      `Fix-up pass (${OUT}). YOUR OWNERSHIP (edit ONLY): ${OWNER_FILES[owner]}. C++: ${FIX_CPP[owner]}. Aesthetic = Zed. Do NOT run git/cargo fmt.\n` +
      `On-screen QA found these second-pass corrections NOT matching the C++ — fix each precisely:\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.feature}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

// ── PHASE 7: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the SECOND-PASS parity sweep (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + FULL tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "fix: second-pass parity polish — QA corrections"\`. 3) FINAL SCREENSHOTS (remember the 2x coordinate rule from the verify harness): data-populated png, capture 3-4 corrected behaviors (e.g. correct post-delete selection, a View toggle effect, an .rcx field surviving reload). \`./scripts/ui.sh stop\`.\n` +
  `4) MISSED-GAP REPORT: how many second-pass gaps were real vs false-positives, which categories had the most divergences (semantics? fidelity? enable-state?), and the honest remaining gap after TWO passes. Return {green, committed, corrected_count, false_positive_count, shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, corrected_count: { type: 'integer' }, false_positive_count: { type: 'integer' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  audit: { total: allGaps.length, wrong: allGaps.filter(g => g.rust_status === 'wrong').length, missing: allGaps.filter(g => g.rust_status === 'missing').length, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length, summary: (a.summary || '').slice(0, 120) })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, implemented: (r.implemented || []).length, false_positives: (r.false_positives || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, corrected_count: final.corrected_count, false_positive_count: final.false_positive_count, remaining: final.remaining, assessment: final.assessment },
}
