export const meta = {
  name: 'reclass-functional-parity-audit',
  description: 'Audit the Rust GPUI port for MISSING FUNCTIONALITY vs the C++ Qt Reclass and fix it: the editor shows all 0x0 because opening a .rcx never attaches its saved data source (so no real values/ASCII/hex/float-interp/enum-labels/comments), Open Recent is always empty (recent files not populated/persisted/shown), editor keyboard modifiers (Shift/Ctrl+arrows etc.) do nothing, plus whatever else the audit surfaces. Verify on-screen with a real data source attached.',
  whenToUse: 'When live data + persistence + keyboard behavior diverge from the C++ — audit functionally and close the gaps.',
  phases: [
    { title: 'Audit', detail: '4 parallel read-only agents diff C++ vs Rust functional behavior (data/value display, persistence/recent, editor keyboard, misc)' },
    { title: 'Synthesize', detail: 'merge + prioritize gaps; assign each to a disjoint file-owner' },
    { title: 'Fix', detail: 'parallel fix agents (disjoint files) implement assigned gaps + the seeded root causes' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + tests + commit' },
    { title: 'Verify', detail: 'launch with a real data source so values populate; check recent files + modifiers -> punch-list' },
    { title: 'Final', detail: 'green build, final screenshots, commit, report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// Seeded ROOT CAUSES (found by inline scouting) so the audit/fix never miss them:
const SEEDS =
  `KNOWN ROOT CAUSES (already scouted — confirm + fix, and find MORE like them):\n` +
  `  • DATA = all 0x0 (the headline): a .rcx stores its data source under "savedSources" (e.g. png.rcx → {kind:File, filePath:"sample.png"}). RcxDocument::load (src/controller.rs ~249) parses these into \`pending_saved_sources\` but NOTHING ATTACHES them — window.rs::open_project (~open_project) only attaches a source when an explicit --data CLI path is passed. So the provider stays NullProvider and every typed value / ASCII / hex / float-interp / enum-label reads 0x0 (Rust ${CREF}/png_reference_rust.png) where the C++ shows real values (${CREF}/png_reference_c++.png). FIX: on open, AUTO-ATTACH the active saved File source — resolve a relative filePath RELATIVE TO THE .rcx's directory (and for bundled File ▸ Examples, materialize the example's sample.png next to the temp .rcx, or resolve to assets/examples/), then load_data_file + refresh. C++ does this on project_open.\n` +
  `  • OPEN RECENT always empty: MainWindow has a \`recent_files\` Vec field but it is never populated on open, never persisted across runs, and the start page + File ▸ Recent Files submenu show nothing. C++ keeps a QSettings recent list (main.cpp ~8765). FIX: push the opened path on open_project (most-recent-first, dedup, cap 10), persist it (a small JSON/config file — the QSettings equivalent) + load it at startup, and feed it to startpage + the Recent Files submenu.\n` +
  `  • EDITOR KEYBOARD MODIFIERS: plain Up/Down + Ctrl+Shift+Up/Down (reorder) work, but other C++ keyPressEvent combos (Shift+arrows range-select, Ctrl+arrows, Home/End variants, etc.) may be missing. Diff editor.cpp keyPressEvent fully.\n`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' },
    summary: { type: 'string' },
    gaps: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        properties: {
          feature: { type: 'string' },
          cpp_ref: { type: 'string' },
          rust_status: { type: 'string', enum: ['works', 'partial', 'stub', 'missing', 'broken'] },
          fix_file: { type: 'string' },
          fix_hint: { type: 'string' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
        },
        required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
      },
    },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT (4 parallel, read-only) ──────────────────────────────────
const AUDIT_HEADER =
  `You are auditing FUNCTIONAL parity of the Rust+GPUI port of "Reclass" (${OUT}/src) against the AUTHORITATIVE C++ Qt implementation (${QT}). Focus on BEHAVIOR that's wired-but-wrong or missing — not styling. Read the C++ for your domain, read the Rust, and produce an EXHAUSTIVE, HONEST gap list (works/partial/stub/missing/broken with the C++ ref + the Rust fix_file + a concrete fix_hint + severity). Trace end-to-end; "the field exists" is not "it works". READ-ONLY.\n\n` +
  SEEDS + `\n` +
  `Reference screenshots: ${CREF}/png_reference_c++.png (C++, values populated) vs ${CREF}/png_reference_rust.png (Rust, all 0x0). Also ${CREF}/*.png.\n\n`

const AUDIT_DOMAINS = [
  {
    key: 'data',
    cpp: ['controller.cpp', 'compose.cpp', 'format.cpp', 'editor.cpp', 'providerregistry.cpp'],
    rust: ['src/ui/window.rs', 'src/controller.rs', 'src/compose.rs', 'src/format.rs', 'src/provider/'],
    focus: `DATA SOURCE & VALUE DISPLAY (the headline). Trace the full chain: open a .rcx → parse savedSources → ATTACH the active File source (resolve relative filePath vs the .rcx dir) → Provider reads bytes → compose/format produce, per node: the typed VALUE (0x20, 0x6, 0x737a7af4…), the ASCII preview ("x...[HSq"), the hex byte columns, the MULTI-TYPE / FLOAT interpretation ("-99999+f, -0.0000f [float×2]"), ENUM value labels ("0x6 (RGBA)", "0x0 (Deflate)"), and node COMMENTS ("// IHDR chunk data length (always 13)…"). For EACH: does the Rust produce it, and is the data source actually attached on open? Pin exactly where the chain breaks (the seed says open_project ignores pending_saved_sources — confirm + find anything else: does compose read the provider? are float/multi interpretations implemented? enum labels? comment composition + the View>Comments/Type-Hints defaults vs C++?).`,
  },
  {
    key: 'persistence',
    cpp: ['main.cpp', 'mainwindow.h', 'gotoaddressdialog.h', 'optionsdialog.cpp'],
    rust: ['src/ui/window.rs', 'src/ui/startpage.rs', 'src/ui/menubar.rs', 'src/ui/commandpalette.rs', 'src/ui/gotoaddress.rs'],
    focus: `PERSISTENCE & RECENT. Recent files (populate on open, dedup, cap, PERSIST across runs via a config file = the C++ QSettings list, load at startup, show in BOTH the start page "Open recent" and File ▸ Recent Files submenu — currently always empty). Also: recent go-to-address formulas, window/dock layout persistence, options/settings persistence, theme persistence — what the C++ saves/restores via QSettings that the Rust drops. Pin where each is stubbed.`,
  },
  {
    key: 'keyboard',
    cpp: ['editor.cpp', 'editor.h'],
    rust: ['src/ui/editor/mod.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/tab_cycle.rs'],
    focus: `EDITOR KEYBOARD/MOUSE MODIFIERS. Diff editor.cpp keyPressEvent + mouse handlers COMPLETELY: every key + modifier combo (plain arrows, Shift+arrows = range/multi select, Ctrl+arrows, Ctrl+Shift+arrows = reorder [done], Home/End/PageUp/Down, Enter, Insert, Delete, F2, T, P/F/S/U quick-type, Ctrl+C/V/X/D/A, Esc, Tab/Shift+Tab) and Shift/Ctrl click selection. For each, is the Rust binding present + correct? The user reports "Shift+Down etc don't work" — enumerate which combos are missing/broken.`,
  },
  {
    key: 'misc',
    cpp: ['mainwindow.h', 'main.cpp', 'editor.cpp'],
    rust: ['src/ui/window.rs', 'src/ui/editor/mod.rs', 'src/ui/tabs.rs', 'src/ui/scannerpanel.rs'],
    focus: `OTHER FUNCTIONAL GAPS. Anything wired-but-wrong / missing not covered above: view-option toggles' real EFFECT (compact columns, tree lines, relative offsets, type hints, comments, minimap — do they change rendering like the C++?), the value-edit write-back (typing a new value writes it to the provider), pointer following / address navigation, hover popups (value history, struct preview, disasm) actually populating, the refresh loop (live values updating), tab/document lifecycle. Be skeptical; trace each.`,
  },
]

phase('Audit')
log('Auditing 4 functional domains vs the C++ in parallel.')
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\n` +
    `READ the C++: ${d.cpp.map(f => `${QT}/${f}`).join(', ')}.\n` +
    `READ the Rust: ${d.rust.map(f => `${OUT}/${f}`).join(', ')}.\n\n` +
    `Produce the exhaustive gap list — completeness over brevity.`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit: ${allGaps.length} gaps (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major).`)

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
const OWNER_FILES = {
  lifecycle: 'src/ui/window.rs, src/controller.rs, src/ui/examples.rs (data-source auto-attach + recent-files populate/persist + path resolution)',
  display: 'src/compose.rs, src/format.rs (typed value / ASCII / hex / float-multi-interp / enum-label / comment formatting)',
  editor: 'src/ui/editor/* (keyboard modifiers, selection, value write-back, hover popups, view-option render effects)',
  chrome: 'src/ui/startpage.rs, src/ui/menubar.rs, src/ui/commandpalette.rs, src/ui/tabs.rs (recent display, submenus)',
}
const synth = await agent(
  `Synthesis step of a C++-vs-Rust FUNCTIONAL parity audit (${allGaps.length} raw gaps):\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feature: g.feature, status: g.rust_status, fix_file: g.fix_file, sev: g.severity, hint: (g.fix_hint || '').slice(0, 150) }))) +
  `\n\nDE-DUPLICATE, drop anything already truly working, PRIORITIZE (blocker first — the data-source attach + value display is the headline, then recent files, then keyboard). For each surviving item return: feature, owner (one of: lifecycle, display, editor, chrome), fix_file, fix_hint, severity. Assign owner by fix_file: window/controller/examples→lifecycle; compose/format→display; editor/*→editor; startpage/menubar/commandpalette/tabs→chrome.`,
  {
    label: 'synthesize', phase: 'Synthesize',
    schema: { type: 'object', additionalProperties: false, properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        feature: { type: 'string' }, owner: { type: 'string', enum: ['lifecycle', 'display', 'editor', 'chrome'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
      overview: { type: 'string' },
    }, required: ['items', 'overview'] },
  })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by owner) ────────────────────────────────────────
phase('Fix')
const FIX_CPP = {
  lifecycle: `${QT}/controller.cpp, ${QT}/main.cpp, ${QT}/providerregistry.cpp`,
  display: `${QT}/compose.cpp, ${QT}/format.cpp`,
  editor: `${QT}/editor.cpp, ${QT}/editor.h`,
  chrome: `${QT}/main.cpp, ${QT}/mainwindow.h`,
}
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Implement FUNCTIONAL parity fixes in the Reclass Rust port (${OUT}) vs the C++ Qt Reclass. Logic layer is implemented — wire/fix it; touch logic modules only as truly needed (say so). Aesthetic = Zed (design.rs tokens).\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch other owners' files or src/ui/mod.rs. Do NOT run git/cargo fmt (later stage). Keep #[cfg(test)] tests; add tests for pure helpers.\n` +
    `AUTHORITATIVE C++: ${FIX_CPP[owner]}. Reference: ${CREF}/png_reference_c++.png (target) vs png_reference_rust.png (current).\n\n` +
    SEEDS + `\n` +
    `IMPLEMENT these audited gaps (make each WORK end-to-end vs the C++):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}] ${it.feature}\n     cpp: ${it.cpp_ref || '(your C++ files)'} — fix: ${it.fix_hint}`).join('\n') +
    (owner === 'lifecycle'
      ? `\n\nHEADLINE for you: make opening a .rcx ATTACH its saved File source so values populate (resolve relative filePath vs the .rcx dir; for File ▸ Examples, ensure sample.png is materialized next to the temp .rcx — copy it from assets/examples/ or resolve there), and make Open Recent actually work (populate on open + persist to a config file + load at startup + expose to startpage/menubar).`
      : '') +
    `\n\nEnsure your files compile (\`${BUILD}\`) — other agents edit other files concurrently. Return {owner, compiles, fixed:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `Functional-parity fixes just landed across the Reclass GPUI port (${OUT}): data-source auto-attach + value display, recent-files persistence, editor keyboard modifiers, view-option effects. Make the FULL build authoritative-green.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, notes: (r.notes || '').slice(0, 180) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (seams: window↔controller data attach, recent-files persistence types, compose/format signatures, editor key bindings). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) Tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`. 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: functional parity — attach saved data source on open (real values), recent files persistence, editor keyboard modifiers (C++-audited)"\`. Report green + committed=<hash>.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display) ───────────────────────────────────────
phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the functional-parity pass. You OWN the headless display. ALWAYS rebuild the ui binary first (a prior phase may have left the headless binary): \`cd ${OUT} && ${LP} cargo build 2>&1 | tail -2\`.\n\n` +
  `SETUP a project WITH its data source resolvable: \`cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/\` (so sample.png sits NEXT TO the .rcx and the relative filePath resolves). Then \`./scripts/ui.sh start /tmp/parity/png.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/q_X.png\`; CROP with \`convert IN -crop WxH+X+Y OUT\` then Read.\n\n` +
  `VERIFY (compare to ${CREF}/png_reference_c++.png):\n` +
  `  1. /tmp/q_values.png — THE HEADLINE: the editor now shows REAL values from sample.png — uint32_t width = 0x20 (not 0x0), color_type = 0x6 with an enum label "(RGBA)" if implemented, the hex64 rows show real bytes (78 9C D5 95…) + ASCII preview + float interpretations, and node comments ("// IHDR chunk…") if Comments/Type-Hints are on. Crop the field rows and READ — confirm NON-ZERO values. This was all 0x0 before.\n` +
  `  2. /tmp/q_recent.png — restart with NO project (\`./scripts/ui.sh start\`), the start page "Open recent" should now LIST png.rcx (the just-opened project), and File ▸ Recent Files should list it too. (If persistence writes a config file, opening then restarting should show it.)\n` +
  `  3. /tmp/q_keys.png — focus the editor (click a row), test the modifier combos the audit fixed (e.g. Shift+Down to extend selection — multiple rows highlighted) and report which now work.\n` +
  `Be a harsh end-to-end critic. Return: shots, findings=[{item, severity, works(bool), detail}], overall (esp: do REAL VALUES show now? does Open Recent list the project? do modifiers work?). \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { item: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['item', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH if needed + FINAL ───────────────────────────────────────
if (fails.length > 0) {
  phase('Fix')
  const byOwner = {}
  for (const f of fails) {
    // best-effort: route to the owner whose files the failing item names, else lifecycle
    const o = /recent|persist|data source|value|attach|provider/i.test(f.item + f.detail) ? 'lifecycle'
      : /key|shift|arrow|select|modifier/i.test(f.item + f.detail) ? 'editor'
        : /float|ascii|hex|enum|comment|format/i.test(f.item + f.detail) ? 'display' : 'chrome'
    ;(byOwner[o] = byOwner[o] || []).push(f)
  }
  const polishOwners = Object.keys(byOwner)
  log(`Polishing ${polishOwners.length}: ${polishOwners.join(' ')}`)
  await parallel(polishOwners.map(owner => () =>
    agent(
      `Fix-up pass (${OUT}). YOUR OWNERSHIP (edit ONLY): ${OWNER_FILES[owner]}. C++: ${FIX_CPP[owner]}. Do NOT run git/cargo fmt.\n` +
      `On-screen QA found these NOT working — fix each end-to-end:\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.item}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

phase('Final')
const final = await agent(
  `Final gate for the functional-parity pass (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: functional parity polish — data values / recent / keyboard QA fixes"\`. 3) FINAL SCREENSHOT: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`, maximize, /tmp/qfinal_values.png — Read it to confirm REAL non-zero values show. \`./scripts/ui.sh stop\`.\n` +
  `4) Honest report: do values populate from the data source now? does Open Recent work? which keyboard modifiers now work? what remains. Return {green, committed, values_show(bool), recent_works(bool), shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, values_show: { type: 'boolean' }, recent_works: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''} values=${final && final.values_show} recent=${final && final.recent_works}`)

return {
  audit: { total: allGaps.length, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, values_show: final.values_show, recent_works: final.recent_works, remaining: final.remaining, assessment: final.assessment },
}
