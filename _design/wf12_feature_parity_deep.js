export const meta = {
  name: 'reclass-feature-parity-deep',
  description: 'DEEP, whole-codebase feature-parity sweep: go module-by-module through the ENTIRE C++ Qt Reclass and find every CAPABILITY/feature/operation/mode the C++ has that the Rust port does NOT implement (logic + UI), then implement them. Prior passes covered UI interactions; this targets missing FEATURES — scanner modes, all node kinds + node operations, value/render formatting, generators/importers, RTTI/symbols/PDB, address-expression parsing, the full main.cpp menu/app feature set, and dialogs/panels (profiler, plugins, theme editor, modules/symbols). Verify on-screen.',
  whenToUse: 'When large feature areas of the C++ are suspected missing in the Rust port — exhaustive module-by-module capability audit + implementation.',
  phases: [
    { title: 'Audit', detail: '9 parallel read-only agents, one per C++ module cluster, exhaustively enumerate features the C++ has and whether the Rust implements them' },
    { title: 'Synthesize', detail: 'merge + dedupe + prioritize the missing features; assign each to a disjoint code-owner' },
    { title: 'Fix', detail: 'parallel owners implement the missing features (logic + UI) against the C++' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + full test suite + commit' },
    { title: 'Verify', detail: 'launch data-populated; exercise the headline new features -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, feature-parity report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' }, summary: { type: 'string' },
    gaps: { type: 'array', items: {
      type: 'object', additionalProperties: false,
      properties: {
        feature: { type: 'string', description: 'a concrete C++ capability/operation/mode, e.g. "Next-Scan narrows the previous result set"' },
        cpp_ref: { type: 'string', description: 'C++ file:symbol that implements it' },
        rust_status: { type: 'string', enum: ['present', 'partial', 'stub', 'missing'] },
        fix_file: { type: 'string', description: 'the Rust file(s) that should implement it' },
        fix_hint: { type: 'string' },
        severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
        logic_or_ui: { type: 'string', enum: ['logic', 'ui', 'both'] },
      },
      required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
    } },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT — one DEEP agent per C++ module cluster ──────────────────
const AUDIT_HEADER =
  `You are doing a DEEP feature-parity audit of the Rust+GPUI port of "Reclass" (${OUT}) against the AUTHORITATIVE C++ Qt source (${QT}). Prior passes already fixed UI INTERACTIONS (clicks/keys/inline-edit/rendering). THIS pass targets MISSING FEATURES/CAPABILITIES: whole operations, modes, node kinds, formatting cases, scan types, importers/exporters, and app actions the C++ has that the Rust does NOT (or only stubs). Read your C++ module(s) THOROUGHLY (they are large — read in chunks, enumerate every public method / slot / menu action / enum variant / mode), then check the Rust counterpart, and list every capability the Rust is MISSING or only PARTIAL/STUB. Be exhaustive — the user is certain a LOT is missing; missing a feature is the failure mode. READ-ONLY.\n\n` +
  `For each gap: feature (the concrete C++ capability), cpp_ref (file:symbol), rust_status (present/partial/stub/missing), fix_file (Rust file to implement it), fix_hint (how), severity, logic_or_ui. Skip things already fully present. Reference screenshots: ${CREF}/*.png (esp. png_reference_c++.png).\n\n`

const AUDIT_DOMAINS = [
  { key: 'scanner', cpp: ['scannerpanel.cpp', 'scanner.cpp', 'scanner.h', 'scannerpanel.h'], rust: ['src/ui/scannerpanel.rs', 'src/scanner.rs', 'src/scanner/'],
    focus: `THE MEMORY SCANNER (scannerpanel.cpp is 2467 lines — huge). Enumerate EVERY scan capability: value scans (exact / bigger / smaller / between / changed / unchanged / increased / decreased / unknown-initial), data types (int8..int64, uint*, float, double, string, array-of-bytes), signature/AOB pattern scans (with wildcards), the FIRST-scan vs NEXT-SCAN narrowing flow, UNDO-SCAN, region filters (executable/writable/copy-on-write/current-struct), alignment, fast-scan, value freeze, the results table (address/value/previous columns, sorting, paging), result count, Go-to-Address / Copy-Address / add-to-struct from a result, and scanning over a FILE data source (not just live process). Which of these does the Rust scanner (UI + engine) implement vs stub/miss?` },
  { key: 'core_nodes', cpp: ['core.h', 'controller.cpp', 'controller.h'], rust: ['src/core/', 'src/controller.rs', 'src/controller/'],
    focus: `NODE KINDS + NODE OPERATIONS (core.h 1604 lines, controller.cpp 5959). Enumerate EVERY NodeKind the C++ supports (hex8/16/32/64/128, int/uint 8-64, bool, float, double, vec2/3/4, matrix, pointer32/64, function-ptr, vtable, struct/class instance, nested struct, union, bitfield, enum, array, text/utf8/utf16, function, custom) and EVERY node OPERATION (add/insert above-below, delete, batch-delete, duplicate, change-type, wrap-in-struct/array/pointer, unwrap, convert, set name/comment/offset, big-endian toggle, array resize, pointer-target set, bitfield edit, enum-member edit, copy/paste nodes cross-struct, move/reorder, expand/collapse, set-as-root). For EACH: does the Rust core + controller implement it? Find missing node kinds + missing operations.` },
  { key: 'compose_format', cpp: ['compose.cpp', 'format.cpp'], rust: ['src/compose.rs', 'src/compose/', 'src/format.rs'],
    focus: `RENDERING + VALUE FORMATTING (compose.cpp 1639, format.cpp 950). For every node kind, what does the C++ RENDER (the text line: type token, name, value, ASCII, hex bytes, multi-type interpretation [int/float/pointer-target/string], enum label, vtable function rows, function signature, bitfield bits, pointer chains, comments, type-inference hints, change-heat coloring, address/offset column)? Enumerate every formatting case + value interpretation the C++ produces and check the Rust compose/format. Find missing value interpretations, missing node-kind rendering, missing comment/hint/heat handling.` },
  { key: 'gen_import', cpp: ['generator.cpp', 'generator.h', 'imports/import_source.cpp'], rust: ['src/generator.rs', 'src/generator/', 'src/imports/'],
    focus: `CODE GENERATION + IMPORT (generator.cpp 1747). Every export format (C++ header, Rust, C#, Python ctypes, #define offsets, ReClass.NET XML, JSON) — full fidelity (nested types, arrays, pointers, vtables, bitfields, enums, unions, padding, asserts, alignment). And every IMPORTER (ReClass.NET XML, PDB types, C/C++ source parse). Which formats/importers are missing or incomplete in the Rust generator/imports?` },
  { key: 'rtti_symbols', cpp: ['rtti.cpp', 'rtti.h', 'rttibrowser.h', 'symbolstore.cpp', 'symbol_downloader.cpp', 'disasm.cpp'], rust: ['src/rtti/', 'src/disasm.rs', 'src/ui/modulespanel.rs'],
    focus: `RTTI / SYMBOLS / DISASM. RTTI walking (vtable → class name, base classes, MSVC + Itanium), the RTTI browser UI, symbol resolution (PDB symbols, symbol store), symbol/PDB DOWNLOAD (symbol_downloader), the modules/symbols/types panel + "Download All", and the disassembler (disasm at an address, the disasm hover/view). Which of these does the Rust implement (logic + UI) vs miss?` },
  { key: 'app_menus', cpp: ['main.cpp', 'mainwindow.h'], rust: ['src/ui/window.rs', 'src/ui/menubar.rs', 'src/ui/commandpalette.rs', 'src/main.rs'],
    focus: `THE APP SHELL + ALL MENU/ACTION FEATURES (main.cpp is 9598 lines — read it thoroughly in chunks). Enumerate EVERY menu action, slot, toolbar, keyboard accelerator, and lifecycle feature: File (new/open/save/save-as/import/export/examples/recent/data-source/close), Edit (undo/redo/cut/copy/paste/delete/select-all/find/bookmarks), View (every toggle + its effect, split/unsplit editor, presentation mode, reset windows, font, theme), Tools, Plugins, Help (about/docs), the refresh/auto-refresh loop, multi-document tabs, dock layout save/restore, window title, settings persistence, drag-drop file open. For EACH: wired + working in Rust, or missing/stub?` },
  { key: 'dialogs_panels', cpp: ['optionsdialog.cpp', 'profilerdialog.cpp', 'profiler.cpp', 'processpicker.cpp', 'pluginmanager.cpp', 'gotoaddressdialog.h', 'themes/themeeditor.cpp'], rust: ['src/ui/optionsdialog.rs', 'src/ui/processpicker.rs', 'src/ui/gotoaddress.rs', 'src/ui/bookmarkspanel.rs', 'src/ui/dialogs.rs', 'src/theme/'],
    focus: `DIALOGS + PANELS as FEATURES: the Options/Settings dialog (every setting + persistence), the PROFILER (profiler.cpp + profilerdialog — what it measures/shows; likely MISSING in Rust), the process picker (live process list + attach), the PLUGIN MANAGER (load/list/enable plugins; likely missing), the THEME EDITOR (themes/themeeditor — edit/create themes; likely missing), goto-address (expression forms + recents), bookmarks panel (add/remove/goto/persist). Which dialogs/panels are missing entirely or stubbed?` },
  { key: 'addr_expr', cpp: ['addressparser.cpp', 'addressparser.h', 'gotoaddressdialog.h'], rust: ['src/addr.rs', 'src/ui/gotoaddress.rs'],
    focus: `ADDRESS EXPRESSION PARSING (addressparser.cpp 540). Every address form + operator the C++ parses: hex literals, module base (<app.exe>), module+offset, follow-pointer ([expr]), PDB symbol (mod!Symbol), arithmetic (+ - * << >> & | ^), nested derefs, decimal/hex. Plus where these are USED (base-address edit, goto-address, pointer targets, static expressions, bookmarks). Does the Rust addr.rs parse ALL forms, and is it wired everywhere the C++ uses it?` },
  { key: 'editor_features', cpp: ['editor.cpp', 'editor.h', 'rcxtooltip.h', 'clipboard.h'], rust: ['src/ui/editor/'],
    focus: `EDITOR FEATURES beyond basic interactions (editor.cpp 5077): the hover popups (value-history tracking, struct-preview, disasm popup, rcxtooltip), node CLIPBOARD (copy/cut/paste nodes incl. cross-struct + as-C-struct, clipboard.h), value change TRACKING/history, the live value REFRESH + change-heat decay, multi-select + batch operations, drag-to-reorder, the offset/address margin modes, the minimap, split-view, presentation mode, the Debug view (LineMeta dump), find-in-editor. Which editor FEATURES (not just clicks) are missing/stub?` },
]

phase('Audit')
log(`DEEP feature audit: ${AUDIT_DOMAINS.length} C++ module clusters in parallel.`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\n` +
    `READ the C++ (thoroughly, in chunks — these are large): ${d.cpp.map(f => `${QT}/${f}`).join(', ')}.\n` +
    `READ the Rust: ${d.rust.map(f => `${OUT}/${f}`).join(', ')}.\n\n` +
    `Exhaustive feature-gap list — enumerate every missing/partial/stub capability.`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit: ${allGaps.length} feature gaps (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.rust_status === 'missing').length} fully missing).`)

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
const OWNER_FILES = {
  scanner: 'src/ui/scannerpanel.rs, src/scanner.rs, src/scanner/',
  core_ctrl: 'src/controller.rs, src/controller/, src/core/',
  compose_fmt: 'src/compose.rs, src/compose/, src/format.rs',
  gen_import: 'src/generator.rs, src/generator/, src/imports/',
  rtti_sym: 'src/rtti/, src/disasm.rs, src/ui/modulespanel.rs',
  app_menus: 'src/ui/window.rs, src/ui/menubar.rs, src/ui/commandpalette.rs, src/main.rs',
  dialogs: 'src/ui/optionsdialog.rs, src/ui/processpicker.rs, src/ui/gotoaddress.rs, src/ui/bookmarkspanel.rs, src/ui/dialogs.rs, src/theme/, src/addr.rs',
  editor: 'src/ui/editor/, src/ui/tooltip.rs',
}
const synth = await agent(
  `Synthesis of a DEEP C++-vs-Rust FEATURE-parity audit (${allGaps.length} raw gaps). Each is a capability the C++ has and the Rust may lack:\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feat: g.feature, st: g.rust_status, f: g.fix_file, sev: g.severity, kind: g.logic_or_ui, hint: (g.fix_hint || '').slice(0, 120) }))) +
  `\n\nDE-DUPLICATE, drop anything already fully present, PRIORITIZE (fully-missing high-value features first, then partial/stub; blocker>major>minor). For each surviving item return: feature, owner (scanner|core_ctrl|compose_fmt|gen_import|rtti_sym|app_menus|dialogs|editor — assign by fix_file), fix_file, fix_hint, severity. Keep it COMPREHENSIVE — the goal is to surface and schedule a LOT of real missing functionality, not trim aggressively. Group related gaps so one owner implements a coherent feature.`,
  { label: 'synthesize', phase: 'Synthesize', schema: {
    type: 'object', additionalProperties: false,
    properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        feature: { type: 'string' }, owner: { type: 'string', enum: ['scanner', 'core_ctrl', 'compose_fmt', 'gen_import', 'rtti_sym', 'app_menus', 'dialogs', 'editor'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
      overview: { type: 'string' },
    }, required: ['items', 'overview'],
  } })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} feature items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by owner) ────────────────────────────────────────
phase('Fix')
const FIX_CPP = {
  scanner: `${QT}/scannerpanel.cpp, ${QT}/scanner.cpp`,
  core_ctrl: `${QT}/core.h, ${QT}/controller.cpp, ${QT}/controller.h`,
  compose_fmt: `${QT}/compose.cpp, ${QT}/format.cpp`,
  gen_import: `${QT}/generator.cpp, ${QT}/imports/import_source.cpp`,
  rtti_sym: `${QT}/rtti.cpp, ${QT}/symbolstore.cpp, ${QT}/symbol_downloader.cpp, ${QT}/disasm.cpp`,
  app_menus: `${QT}/main.cpp, ${QT}/mainwindow.h`,
  dialogs: `${QT}/optionsdialog.cpp, ${QT}/profilerdialog.cpp, ${QT}/processpicker.cpp, ${QT}/pluginmanager.cpp, ${QT}/addressparser.cpp`,
  editor: `${QT}/editor.cpp, ${QT}/rcxtooltip.h, ${QT}/clipboard.h`,
}
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Implement MISSING FEATURES in the Reclass Rust port (${OUT}) to reach parity with the C++ Qt Reclass — porting real LOGIC, not just wiring. This is a deep feature pass; implement the capability faithfully per the C++ (algorithms, modes, edge cases). Add #[cfg(test)] tests for new logic. Aesthetic = Zed (design.rs tokens) for any UI.\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch src/ui/mod.rs (it pre-declares modules), other owners' files. Do NOT run git/cargo fmt (later stage). Keep the build green and existing tests passing.\n` +
    `AUTHORITATIVE C++: ${FIX_CPP[owner]}. Reference: ${CREF}/*.png.\n\n` +
    `IMPLEMENT these audited MISSING features (port each faithfully from the C++; if a feature needs a logic op the Rust lacks, add it):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}] ${it.feature}\n     fix: ${it.fix_hint}`).join('\n') +
    `\n\nWork through as many as you can to completion (don't half-do them). If a feature genuinely depends on an out-of-scope backend (e.g. a LIVE OS-process provider that doesn't exist on this platform), implement everything UP TO that boundary + a clear stub, and say so. Ensure your files compile (\`${BUILD}\`) — other owners edit other files concurrently. Return {owner, compiles, implemented:[...], deferred:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, implemented: { type: 'array', items: { type: 'string' } }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}(${(r.implemented || []).length})`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `A DEEP feature-parity pass just landed across the Reclass port (${OUT}) — scanner, node kinds/ops, compose/format, generators/importers, rtti/symbols, app menus, dialogs/panels, editor features. Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, impl: (r.implemented || []).length, notes: (r.notes || '').slice(0, 150) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (likely seams: new controller/core ops used by UI, new scanner modes, compose/format signatures, generator/import APIs). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) FULL tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\` — fix any newly-broken tests (or update them if a deliberate behavior change). 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "feat: deep C++ feature parity — scanner modes, node kinds/ops, formatting, generators/imports, rtti/symbols, menu actions, dialogs (audited + ported)"\`. Report green + committed=<hash> + anything you had to stub.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display) ───────────────────────────────────────
phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the DEEP feature-parity pass. You OWN the headless display. ALWAYS rebuild the ui binary first: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`.\n\n` +
  `SETUP: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/f_X.png\`; CROP + Read. Drive: \`./scripts/ui.sh key/click/type\`, right-click \`xdotool mousemove X Y click 3\`. (NOTE: keyboard DOES reach the app — Down moves the selection; use it.)\n\n` +
  `Spot-check the HIGHEST-VALUE newly-implemented features from the fix notes (pick ~8). Examples (adapt to what landed): open the Memory Scanner (Ctrl+Shift+M) and run a Value scan over the file source (it should produce results, not 'no data source'); a Next-Scan narrows; create different node kinds via the right-click/type-picker (vtable/pointer/array/bitfield) and confirm they render with real values; toggle a View option and see the render change; open a dialog the audit said was newly added (profiler / plugins / options); export C++ Header and confirm output; check the modules/symbols panel; a new editor feature (clipboard copy/paste node, hover popup). For each: does it actually WORK end-to-end?\n` +
  `Return: shots, findings=[{feature, severity, works(bool), detail}], overall (how much new functionality is real). \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { feature: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['feature', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner) ─────────────────────────────────────
phase('Polish')
const ownerOf = (f) => {
  const t = (f.feature + ' ' + f.detail).toLowerCase()
  if (/scan/.test(t)) return 'scanner'
  if (/node|kind|vtable|pointer|array|bitfield|union|enum op|duplicate|convert|wrap/.test(t)) return 'core_ctrl'
  if (/render|format|value|ascii|hex byte|heat|comment|hint|interpret/.test(t)) return 'compose_fmt'
  if (/export|import|generat|c\+\+ header|rust struct|xml|pdb type/.test(t)) return 'gen_import'
  if (/rtti|symbol|disasm|module|download/.test(t)) return 'rtti_sym'
  if (/menu|action|recent|toolbar|split|presentation|refresh|dock|tab|theme switch/.test(t)) return 'app_menus'
  if (/dialog|option|profiler|plugin|process|goto|bookmark|theme editor|address expr/.test(t)) return 'dialogs'
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
      `On-screen QA found these newly-implemented features NOT working — fix each end-to-end vs the C++:\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.feature}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

// ── PHASE 7: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the DEEP feature-parity pass (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + FULL tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "feat: deep feature parity polish — QA fixes for ported C++ capabilities"\`. 3) FINAL SCREENSHOTS: data-populated png, capture 3-4 of the headline new features working (e.g. scanner results, a new node kind, an export). \`./scripts/ui.sh stop\`.\n` +
  `4) FEATURE-PARITY REPORT: how many of the audited missing features were implemented, which whole areas now work vs still stub (and why — e.g. live-process backend), and the honest remaining gap. Return {green, committed, implemented_count, shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, implemented_count: { type: 'integer' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  audit: { total: allGaps.length, missing: allGaps.filter(g => g.rust_status === 'missing').length, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length, summary: (a.summary || '').slice(0, 120) })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, implemented: (r.implemented || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, implemented_count: final.implemented_count, remaining: final.remaining, assessment: final.assessment },
}
