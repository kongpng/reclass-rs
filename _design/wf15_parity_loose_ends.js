export const meta = {
  name: 'reclass-parity-loose-ends',
  description: 'FOURTH parity sweep — hunt the LAST remaining loose ends between the Rust port and the authoritative C++ Qt Reclass, then fix them. Prior passes implemented ~115 items (features, edge-cases, editor input). This pass goes wide (16 parallel audit domains incl. cross-cutting stub/TODO and dead-wiring scanners) looking for: behaviors that DIVERGE from the C++, half-wired/no-op paths, stubs where the C++ has real logic, missing menu items / accelerators / enable-states, file/clipboard round-trip losses, and untested public APIs. Dedupe against what is already done + the intentional live-OS platform stubs, then implement + verify on-screen (scale-aware).',
  whenToUse: 'A broad final-sweep parity pass after feature/edge/editor passes — find and fix whatever loose ends remain vs the C++.',
  phases: [
    { title: 'Audit', detail: '16 parallel read-only agents diff C++ areas + cross-cutting stub/TODO + dead-wiring scans for remaining loose ends' },
    { title: 'Synthesize', detail: 'merge + dedupe against ~115 already-done items + platform stubs; prioritize; assign each to a disjoint owner' },
    { title: 'Fix', detail: 'parallel owners implement/correct the loose ends against the C++' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + full test suite + commit' },
    { title: 'Verify', detail: 'launch data-populated; scale-aware on-screen drive of the headline fixes -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, loose-ends report + honest residual' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

const ALREADY = `ALREADY DONE across four prior passes — do NOT re-report these; only find what is STILL off WITHIN/BEYOND them:
- Scanner: 9 comparison modes, first/next/undo-scan, all data types, AOB, FILE-source scanning, results table + add-as-nodes.
- Node model: 30+ NodeKinds + ops add/insert/delete/duplicate/change-type/wrap/unwrap/reorder/copy-paste; New Class creates a populated 8-field class instance (not an empty struct); Ptr-to-New-Class creates a 16-field class.
- Compose/format: multi-type value interpretation, ASCII+hex, change-heat, comments/hints, enum labels; pointer-to-class fold footers carry add-bytes pills that grow the referenced class.
- Generators (C++/Rust/#define/C#/Python, byte-exact) + importers (ReClass.NET-XML / PDB / PE-debug / source).
- Address-expression parser (hex/dec/module-base+offset/deref/symbol/arithmetic) + Go-to-Address live eval.
- RTTI walking logic; dialogs: Options, Type Aliases, Profiler (Ctrl+Shift+F), Process Picker, Goto-Address, Bookmarks, Plugins, RTTI Browser.
- App shell: 59+ wired menu actions, recent files, data-source auto-attach on open, examples, Reclass/Code view toggle, settings persistence + .rcx round-trip fidelity.
- Editor: focus-on-click; arrow nav; Down-at-end grows the struct; Shift+arrow multi-select; Ctrl+D duplicate; Ctrl+C/X/V node clipboard; Ctrl+Shift+Up/Down reorder; Left/Right same-size type cycle (INTENTIONAL — do NOT propose all-types cycling); Shift+Left/Right collapse/expand a foldable node; Ctrl+mouse-wheel zoom; arrow descent into expanded nested classes; inline edit incl. hex byte + ASCII overwrite (Enter opens, returns focus on commit); footer add-bytes pills (+1/+10h/+100h/+1000h/Trim) on root, nested struct, and pointer-to-class folds.`

const STUBS = `INTENTIONAL platform-boundary stubs — NOT loose ends (no live OS process backend / network / native dialogs on this host): live process attach + live memory scanning, RTTI over a live vtable, MS symbol-store network download, disasm hover over live code bytes, dynamic native-plugin (.so/.dll) loading, always-on MCP bridge, XDG-portal native file dialogs, and value WRITE-BACK to the read-only file provider (edits don't persist to a file — that is expected, not a bug). Do NOT report these; the logic is ported + tested up to the read_process_memory seam.`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' }, summary: { type: 'string' },
    gaps: { type: 'array', items: {
      type: 'object', additionalProperties: false,
      properties: {
        feature: { type: 'string', description: 'a SPECIFIC loose end: a behavior that diverges, a half-wired/no-op path, a stub where C++ has real logic, a missing item/accelerator/enable-state, a round-trip loss, or an untested public API' },
        cpp_ref: { type: 'string', description: 'C++ file:symbol/line that defines the correct behavior' },
        rust_status: { type: 'string', enum: ['present', 'partial', 'wrong', 'stub', 'missing'] },
        fix_file: { type: 'string', description: 'the Rust file(s) that should implement/correct it' },
        fix_hint: { type: 'string' },
        severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      },
      required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
    } },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT — 16 parallel domains (incl. 2 cross-cutting scanners) ────
const AUDIT_HEADER =
  `FOURTH-PASS "loose ends" parity audit of the Rust+GPUI port of Reclass (${OUT}) vs the AUTHORITATIVE C++ Qt source (${QT}). Three prior passes ported ~115 items; the features EXIST. Your job is the LAST remaining gaps: behaviors that DIVERGE (rust_status:"wrong"), half-wired/no-op paths, stubs where the C++ has real logic, missing menu items / accelerators / enable-states, file/clipboard round-trip losses, and public APIs with no caller or no test. Read your C++ area THOROUGHLY (in chunks; enumerate every branch/slot/option), then diff the Rust. Be exhaustive and skeptical — a behavior that exists but is subtly wrong is exactly the target. READ-ONLY.\n\n` +
  ALREADY + `\n\n` + STUBS + `\n\n` +
  `For each gap: feature (specific), cpp_ref (file:symbol/line), rust_status, fix_file, fix_hint, severity. Skip the platform stubs + anything that genuinely matches the C++. Reference screenshots: ${CREF}/*.png.\n\n`

const AUDIT_DOMAINS = [
  { key: 'app_shell', cpp: 'main.cpp (9598L) + mainwindow.h', rust: 'src/ui/window.rs, src/ui/menubar.rs, src/ui/commandpalette.rs, src/main.rs',
    focus: `THE APP SHELL: every menu item, toolbar, accelerator, slot, and lifecycle hook in main.cpp — enabled/disabled (greyed) state logic, multi-document tabs, dock show/hide + layout save/restore, window title updates, auto-refresh loop, drag-drop open, recent files, examples. Find missing items, wrong/missing shortcuts, missing enable-state, lifecycle hooks that don't fire.` },
  { key: 'controller_ops', cpp: 'controller.cpp (5959L) + controller.h', rust: 'src/controller.rs, src/controller/',
    focus: `CONTROLLER OPERATIONS + exact post-conditions: every node op's selection/offset/expand result, undo/redo of EACH op (incl. paste/multi-delete/reorder/type-change/comment/offset), big-endian per kind, wrap/unwrap, array resize edges, refId redirects. Find ops that diverge from the C++ post-condition or aren't undoable.` },
  { key: 'core_model', cpp: 'core.h (1604L)', rust: 'src/core/',
    focus: `THE NODE MODEL: every NodeKind's size/align/lines/flags, node fields, struct_span, absolute_address, (de)serialization of every field (comment/offset/collapsed/color/metadata/enum members/bitfield members/savedSources/base-address-formula/type aliases/bookmarks), clipboard codec. Find model fields the C++ has that the Rust drops or computes differently.` },
  { key: 'compose_render', cpp: 'compose.cpp (1639L)', rust: 'src/compose.rs, src/compose/',
    focus: `RENDERING: every line/row the C++ composes — per-kind headers/footers/children, ref/pointer expansion, RTTI chips, comment/hint chips, change-heat ramp, offset/address margin modes, command row, static fields, vtable rows, the exact column layout. Find render cases the Rust misses or paints differently (rust_status:"wrong").` },
  { key: 'format_values', cpp: 'format.cpp (950L)', rust: 'src/format.rs',
    focus: `VALUE FORMATTING: every value interpretation + format string the C++ produces per kind (int/uint signed display, float/double precision/NaN/inf, hex grouping, pointer/null/bad display, bool, vectors/matrix, strings, big-endian), value PARSE + validate on commit. Find formatting/parse divergences.` },
  { key: 'scanner', cpp: 'scannerpanel.cpp (2467L) + scanner.cpp (1138L)', rust: 'src/ui/scannerpanel.rs, src/scanner.rs, src/scanner/',
    focus: `THE SCANNER: every scan type/mode/data-type, first->next->undo flow, region filters, alignment, fast-scan, value freeze, results table (columns/sort/paging/count), go-to/copy/add-as-nodes, save/load scan, and FILE-source scanning correctness. Find UI controls or engine modes that are missing/no-op/wrong (beyond the live-process boundary).` },
  { key: 'typeselector', cpp: 'typeselectorpopup.cpp (1979L)', rust: 'src/ui/typeselectorpopup.rs',
    focus: `THE TYPE PICKER: filtering (Hex/Int/Float/Ptr/all), modifiers (* ** []), composite selection carrying struct_id, "+ New", keyboard nav, recents, the exact type set + ordering, applying a chosen type incl. existing user structs/enums (does selecting an existing composite carry its ref correctly?). Find picker behaviors that drop data or diverge.` },
  { key: 'generator', cpp: 'generator.cpp (1747L) + generator.h', rust: 'src/generator.rs, src/generator/',
    focus: `CODE GENERATION byte-fidelity on HARD cases: nested anonymous structs/unions, bitfields, enums (underlying type + values), arrays-of-pointers + multi-dim, vtables, padding + static_assert(sizeof/offsetof), #pragma pack, forward decls + include ordering, name sanitization/collisions. Diff each format's output byte-for-byte vs C++.` },
  { key: 'imports', cpp: 'imports/import_source.cpp + ReClass.NET XML + PDB', rust: 'src/imports/',
    focus: `IMPORTERS: ReClass.NET XML round-trip (every attribute), PDB type import, C/C++ source parse — malformed input handling, unknown types, partial parse, type mapping (msvc/ida fixed-ints), nested types, enums. Find importer attributes/edge-cases dropped or mismapped.` },
  { key: 'addr_expr', cpp: 'addressparser.cpp (540L) + gotoaddressdialog.h', rust: 'src/addr.rs, src/ui/gotoaddress.rs',
    focus: `ADDRESS EXPRESSIONS: every form + operator (hex/dec, module base, module+offset, deref [expr], symbol mod!Sym, + - * << >> & | ^, nested), AND every USE SITE (base-address edit, goto-address, pointer targets, static expressions, bookmarks, scanner address). Find forms the parser misses or sites where it isn't wired.` },
  { key: 'rtti_symbols', cpp: 'rtti.cpp (525L) + symbolstore.cpp + symbol_downloader.cpp + disasm.cpp + rttibrowser.h', rust: 'src/rtti/, src/disasm.rs, src/ui/modulespanel.rs',
    focus: `RTTI / SYMBOLS / DISASM (logic side, up to the live boundary): MSVC + Itanium RTTI walking + class-name demangling, base-class chains, the RTTI browser UI, symbol resolution + the modules/symbols/types panel, PDB type enumeration, disasm decoding. Find logic gaps or UI that doesn't surface ported results.` },
  { key: 'dialogs_panels', cpp: 'optionsdialog.cpp + profilerdialog.cpp + processpicker.cpp + pluginmanager.cpp + themes/themeeditor.cpp', rust: 'src/ui/optionsdialog.rs, src/ui/processpicker.rs, src/ui/bookmarkspanel.rs, src/ui/dialogs.rs, src/theme/, src/ui/enumpicker.rs, src/ui/sourcechooser.rs',
    focus: `DIALOGS/PANELS completeness: Options (every setting + apply + persist), Profiler (auto-enable/refresh/sort/CSV), Process Picker (list/filter/attach UI), Plugin Manager (list/enable), Theme editor (edit/create/apply themes), enum picker, source chooser, bookmarks panel (add/remove/goto/persist). Find dialog fields/controls missing or not wired to an effect.` },
  { key: 'editor_loose', cpp: 'editor.cpp (5077L) + rcxtooltip.h + clipboard.h', rust: 'src/ui/editor/, src/ui/tooltip.rs, src/ui/statusbar.rs, src/ui/findbar.rs',
    focus: `EDITOR loose ends NOT already fixed: hover popups (value-history graph, struct/pointer preview, disasm popup) content + dismiss, the node clipboard interchange FORMAT (ReClass.NET XML clipboard), find-in-editor (next/prev/wrap/highlight-all/match field), drag-to-reorder, multi-select batch ops, split view, presentation mode, Go-To-Definition, status-bar fields + live updates, every editor context-menu item + submenu + enable-state. Find what's still missing/wrong.` },
  { key: 'persistence', cpp: 'main.cpp (QSettings) + controller.cpp (RcxDocument save/load) + themes', rust: 'src/rcx.rs, src/document.rs, src/settings.rs, src/config.rs, src/ui/window.rs',
    focus: `PERSISTENCE: every QSettings key (refresh rate, theme, fonts, colors, address mode, default sizes, hex columns, recent files, window geometry, dock layout, scanner defaults, type aliases, plugin flags) — saved AND applied across restart. And .rcx / ReClass.NET-XML disk round-trip field-by-field. Find settings/fields not persisted or not applied on load.` },
  { key: 'stub_todo_scan', cpp: 'ANY (cross-cutting)', rust: 'src/ (whole tree)',
    focus: `CROSS-CUTTING STUB/TODO SCAN. grep the WHOLE Rust tree for: todo!() / unimplemented!() / "not implemented" / "not available" / "TODO" / "FIXME" / "stub" / "skeleton" / panic!("...") in non-test code / functions that return a hardcoded default / "// C++ does X" comments where X isn't done. For EACH, check the C++: is there real logic that should be ported? Report only the ones where the C++ has a real impl that the Rust stubs (skip intentional platform stubs).` },
  { key: 'dead_wiring_scan', cpp: 'main.cpp + controller.cpp (signals/slots) + *.h', rust: 'src/ui/, src/controller.rs',
    focus: `CROSS-CUTTING DEAD-WIRING SCAN. Enumerate every C++ signal/slot connection + every menu/toolbar action + every emitted request (xxxRequested) and confirm each has a Rust equivalent that is REGISTERED and does real work (not a no-op handler / not unreachable). Also: Rust pub fns / actions / events with NO caller or subscriber (dead). Report unwired actions, no-op handlers, and orphaned events.` },
]

phase('Audit')
log(`Loose-ends audit: ${AUDIT_DOMAINS.length} domains in parallel (incl. stub/TODO + dead-wiring scanners).`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\nC++: ${d.cpp}. Rust: ${d.rust}.\n\nExhaustive loose-ends list. Prefer specific, reproducible divergences with a file:line C++ ref.`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit: ${allGaps.length} loose ends (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.rust_status === 'wrong').length} diverging, ${allGaps.filter(g => g.rust_status === 'missing' || g.rust_status === 'stub').length} missing/stub).`)

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
const OWNER_FILES = {
  app_menus: 'src/ui/window.rs, src/ui/menubar.rs, src/ui/commandpalette.rs, src/main.rs',
  core_ctrl: 'src/controller.rs, src/controller/, src/core/',
  compose_fmt: 'src/compose.rs, src/compose/, src/format.rs',
  gen_import: 'src/generator.rs, src/generator/, src/imports/',
  scanner: 'src/ui/scannerpanel.rs, src/scanner.rs, src/scanner/',
  rtti_sym: 'src/rtti/, src/disasm.rs, src/ui/modulespanel.rs',
  addr_persist: 'src/addr.rs, src/rcx.rs, src/document.rs, src/settings.rs, src/config.rs',
  editor: 'src/ui/editor/, src/ui/tooltip.rs, src/ui/statusbar.rs, src/ui/findbar.rs',
  dialogs: 'src/ui/optionsdialog.rs, src/ui/processpicker.rs, src/ui/gotoaddress.rs, src/ui/bookmarkspanel.rs, src/ui/dialogs.rs, src/ui/typeselectorpopup.rs, src/ui/enumpicker.rs, src/ui/sourcechooser.rs, src/theme/',
}
const synth = await agent(
  `Synthesis of a FOURTH-PASS "loose ends" parity audit (${allGaps.length} raw candidates) vs the C++ Reclass:\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feat: g.feature, st: g.rust_status, f: g.fix_file, sev: g.severity, hint: (g.fix_hint || '').slice(0, 110) }))) +
  `\n\nDE-DUPLICATE, drop anything already done (see the four-pass list) / a known platform stub / a false positive on closer reading, and PRIORITIZE: "wrong" divergences and round-trip/data losses first (they silently corrupt), then missing/stub real logic, then minor. For each surviving item return: feature, owner (app_menus|core_ctrl|compose_fmt|gen_import|scanner|rtti_sym|addr_persist|editor|dialogs — assign by fix_file; ALL src/ui/editor/mod.rs work → "editor"), fix_file, fix_hint, severity. Be COMPREHENSIVE (this is the final sweep — surface every real loose end) but ruthless about false positives. Group related gaps so one owner ships a coherent fix.`,
  { label: 'synthesize', phase: 'Synthesize', schema: {
    type: 'object', additionalProperties: false,
    properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        feature: { type: 'string' }, owner: { type: 'string', enum: ['app_menus', 'core_ctrl', 'compose_fmt', 'gen_import', 'scanner', 'rtti_sym', 'addr_persist', 'editor', 'dialogs'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
      overview: { type: 'string' },
    }, required: ['items', 'overview'],
  } })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} loose-end items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by disjoint owner) ───────────────────────────────
phase('Fix')
const FIX_CPP = {
  app_menus: `${QT}/main.cpp, ${QT}/mainwindow.h`,
  core_ctrl: `${QT}/controller.cpp, ${QT}/controller.h, ${QT}/core.h`,
  compose_fmt: `${QT}/compose.cpp, ${QT}/format.cpp`,
  gen_import: `${QT}/generator.cpp, ${QT}/imports/import_source.cpp`,
  scanner: `${QT}/scannerpanel.cpp, ${QT}/scanner.cpp`,
  rtti_sym: `${QT}/rtti.cpp, ${QT}/symbolstore.cpp, ${QT}/symbol_downloader.cpp, ${QT}/disasm.cpp`,
  addr_persist: `${QT}/addressparser.cpp, ${QT}/main.cpp (QSettings), ${QT}/controller.cpp (save/load)`,
  editor: `${QT}/editor.cpp, ${QT}/rcxtooltip.h, ${QT}/clipboard.h`,
  dialogs: `${QT}/optionsdialog.cpp, ${QT}/profilerdialog.cpp, ${QT}/processpicker.cpp, ${QT}/pluginmanager.cpp, ${QT}/typeselectorpopup.cpp`,
}
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Correct the remaining loose ends in the Reclass Rust port (${OUT}) to match the C++ Qt original EXACTLY — port precise semantics (post-conditions, serialized fields, byte-exact output, enable-state), not approximations. Add/extend #[cfg(test)] tests pinning each correction. Aesthetic = Zed (design.rs tokens) for any UI.\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch src/ui/mod.rs or other owners' files (edited concurrently). Do NOT run git/cargo fmt. Keep the build green + existing tests passing (update a test only if it pinned OLD wrong behavior; say so).\n` +
    `AUTHORITATIVE C++: ${FIX_CPP[owner]}. Reference: ${CREF}/*.png.\n${STUBS}\n\n` +
    `FIX these audited loose ends (match the C++ precisely):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}] ${it.feature}\n     fix: ${it.fix_hint}`).join('\n') +
    `\n\nWork each to completion. If an item already matches the C++ (false positive), skip it + say so rather than churn. Ensure your files compile (\`${BUILD}\`) — other owners edit other files concurrently. Return {owner, compiles, implemented:[...], false_positives:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, implemented: { type: 'array', items: { type: 'string' } }, false_positives: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}(${(r.implemented || []).length})`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `A FOURTH-PASS loose-ends parity correction just landed across the Reclass port (${OUT}). Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, impl: (r.implemented || []).length, notes: (r.notes || '').slice(0, 140) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (likely seams: new controller post-conditions used by editor/UI, new serialized fields, generator/import APIs, settings keys). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) FULL tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\` — fix newly-broken tests (or update ones pinning old wrong behavior). 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "fix: fourth-pass C++ parity — loose ends across shell/controller/compose/generator/scanner/rtti/persistence/editor/dialogs (audited)"\`. Report green + committed=<hash> + anything still off.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, owns display; scale-robust) ────────────────────
const VERIFY_HARNESS =
  `You OWN the headless display. ALWAYS rebuild the ui binary first: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\` (the headless build overwrites the ui binary).\n` +
  `SETUP: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/v_X.png\`; CROP with ImageMagick \`magick\` + Read.\n` +
  `*** COORDINATE SCALE — COMPUTE IT, DON'T ASSUME (a prior verifier hit a FALSE NEGATIVE here): screen = \`xdotool getdisplaygeometry\` (e.g. 1680x1050); screenshot width = \`magick identify -format '%w' /tmp/v_X.png\`. SCALE = screen_width / screenshot_width (has been 1.0 AND 2.0 in different runs). Click REAL coords = screenshot-pixel × SCALE. Also note the Read tool DISPLAYS the png shrunk, so a feature you eyeball at display-x may be 2× that in the file — verify your scale by clicking a known row and reading the status bar BEFORE trusting any negative. ***\n` +
  `CONFIRMED WORKING (don't re-flag): keyboard reaches the editor after a click; the project sidebar (left ~340px) right-click correctly shows the workspace menu while the editor area shows the per-node menu; value edits don't persist to the read-only file provider (expected).\n`

phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the FOURTH-PASS loose-ends corrections.\n\n` + VERIFY_HARNESS + `\n` +
  `Spot-check ~10 of the highest-value corrections from the fix notes (adapt to what landed): a menu item's enable/disable state, a corrected node-op post-condition, a render/format fix, a generator output on a hard case, a scanner control, a dialog field's effect, a persistence round-trip (save->reload via the real RcxDocument path if the GUI save dialog is unavailable headless), an editor hover/clipboard/find behavior. For each: does it now match the C++?\n` +
  `Return: shots, findings=[{feature, severity, works(bool), detail}], overall. \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { feature: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['feature', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner) ─────────────────────────────────────
phase('Polish')
const ownerOf = (f) => {
  const t = (f.feature + ' ' + f.detail).toLowerCase()
  if (/scan/.test(t)) return 'scanner'
  if (/save|reload|round-trip|\.rcx|serializ|persist|setting|theme|recent|geometry|dock layout|addr|expression/.test(t)) return 'addr_persist'
  if (/export|import|generat|header|bitfield gen|enum gen|xml|pdb/.test(t)) return 'gen_import'
  if (/rtti|symbol|disasm|module|demangl/.test(t)) return 'rtti_sym'
  if (/render|format|color|heat|ascii|value interp|preview|precision|compose/.test(t)) return 'compose_fmt'
  if (/menu|accelerator|shortcut|enabled|disabled|greyed|toolbar|view toggle|tab|window title|dock/.test(t)) return 'app_menus'
  if (/dialog|option|profiler|plugin|process|goto|bookmark|theme editor|picker|enum picker|source chooser/.test(t)) return 'dialogs'
  if (/select|delete|insert|duplicate|undo|redo|reorder|node op|post-condition|wrap|big.?endian/.test(t)) return 'core_ctrl'
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
      `On-screen QA found these loose-end corrections NOT matching the C++ — fix each precisely:\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.feature}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

// ── PHASE 7: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the FOURTH-PASS loose-ends sweep (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + FULL tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit any remainder: \`cd ${OUT} && git add -A && git commit -m "fix: fourth-pass parity polish — QA corrections" || echo nothing\`. 3) FINAL SCREENSHOTS (compute the scale factor per the verify harness): data-populated png + 3-4 corrected behaviors. \`./scripts/ui.sh stop\`.\n` +
  `4) LOOSE-ENDS REPORT: how many loose ends were real vs false-positives, which categories dominated (divergence? wiring? fidelity? enable-state?), and the HONEST residual after FOUR passes (what genuinely remains, beyond the intentional live-OS stubs). Return {green, committed, corrected_count, false_positive_count, shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, corrected_count: { type: 'integer' }, false_positive_count: { type: 'integer' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  audit: { total: allGaps.length, wrong: allGaps.filter(g => g.rust_status === 'wrong').length, missing_stub: allGaps.filter(g => g.rust_status === 'missing' || g.rust_status === 'stub').length, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length, summary: (a.summary || '').slice(0, 110) })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, implemented: (r.implemented || []).length, false_positives: (r.false_positives || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, corrected_count: final.corrected_count, false_positive_count: final.false_positive_count, remaining: final.remaining, assessment: final.assessment },
}
