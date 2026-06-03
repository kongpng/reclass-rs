export const meta = {
  name: 'reclass-full-parity-audit-fix',
  description: 'Exhaustive whole-app parity pass between the Rust port (/home/loke/reclass-rs) and the authoritative C++ Qt original (/home/loke/Documents/Reclass), EXCLUDING the deliberate live-memory stubs. Uses the existing reference corpus (_design/specs/PORTING_*.md, _design/understand/*.md incl. tests-catalog.md, plugin_system_cpp_reference.md). Flow: (1) coverage check — confirm every C++ real-app source file is assigned to a subsystem + nail the stub-exclusion boundary; (2) WIDE per-subsystem audit fan-out (~17 subsystems) cross-referencing spec + understand doc + C++ source + C++ tests vs the current Rust module, emitting classified parity findings + a per-subsystem parity estimate; (3) adversarial critic RE-AUDIT of low-confidence / high-gap subsystems to catch what round 1 missed; (4) triage/plan → prioritized, file-disjoint FIX BATCHES + preliminary scorecard; (5) sequential parity-GATED fix loop (each batch: implement → 7-step gate → commit-or-revert; UI batches also verify on-screen with the SCALE rule) that FIXES divergences + IMPLEMENTS missing features; (6) post-fix re-audit of touched subsystems; (7) FINAL quantified parity report (per-subsystem % + overall weighted % OUTSIDE STUBS, what landed, what remains + why) written to _design/PARITY_REPORT.md. The existing test suite MUST stay green throughout (commit-or-revert); the live-memory reader / Windows CLR / native runtime-load stay excluded stubs.',
  whenToUse: 'A final, exhaustive C++ parity audit + fix + scorecard for the whole app.',
  phases: [
    { title: 'Coverage' },
    { title: 'Audit' },
    { title: 'Critic re-audit' },
    { title: 'Plan' },
    { title: 'Fix' },
    { title: 'Re-audit' },
    { title: 'Report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const SPECS = `${OUT}/_design/specs`
const UND = `${OUT}/_design/understand`
const PLUGREF = `${OUT}/_design/plugin_system_cpp_reference.md`
const BASELINE = 'HEAD acb58f8 baseline tests: full(disasm,symbols,imports,mcp)=1059/0, ui=526/0, plugins(--lib)=1487/0, bare(no-default-features)=799/0.'

const STUBS = `EXCLUDED STUBS (deliberate, do NOT score as parity gaps and do NOT try to implement): the live-memory provider plugins (C++ plugins/{ProcessMemory,RemoteProcessMemory,WinDbgMemory,KernelMemory}) which read live OS process memory — the Rust port keeps a logical-source stub pending the security program; the RcNetPluginCompatLayer MANAGED .NET CLR host (Windows-only P5 scaffold); native plugin runtime DLL/SO loading (infra at parity, but no binary on disk to load here); the P7 subprocess loader; permission sandboxing (disclosure-only by design). EVERYTHING ELSE is in-scope for 1:1 parity. Note the plugin SYSTEM (contract, registry, manager, manage-plugins dialog, discovery wiring) was just brought to/over parity — verify it, don't re-stub it.`

// Universal parity gate (the proven 7-check; --features plugins builds clean on this host).
const GATE = `*** PARITY GATE — the EXISTING suite MUST stay green; never leave the tree red or commit red. ***
Run ALL from ${OUT} and require success:
  1) default/ui build:  \`${LP} cargo build 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  2) headless build:    \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  3) plugins build:     \`${LP} cargo build --features plugins 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  4) full tests:        \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  5) ui tests:          \`${LP} cargo test --lib "ui::" 2>&1 | grep 'test result' | tail -1\`  → 0 failed
  6) plugins tests:     \`${LP} cargo test --features plugins --lib 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  7) bare tests:        \`cargo test --no-default-features 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
If ALL pass (new tests included): \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` → green=true, committed=<hash>. If the batch legitimately changed nothing, green=true, committed="(no-op)".
If you CANNOT reach green after reasonable effort: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` (restores prior green HEAD) → green=false, reverted=true + notes. NEVER touch target/ or .git config.`

const COMMON = `Rust port: ${OUT}. Authoritative C++ original: ${QT}. Reference corpus (READ the relevant doc(s) first — they are detailed and accurate): per-subsystem porting spec ${SPECS}/PORTING_<area>.md, C++ behavior analysis ${UND}/<area>.md, the C++ test catalog ${UND}/tests-catalog.md, and ${PLUGREF}. The port has ALREADY been through many parity passes (wf8–wf17) so it is mostly close — focus on what REMAINS divergent/missing, verified against the ACTUAL current Rust code (get fresh line numbers; the docs may pre-date recent commits). Aesthetic = Zed (src/ui/design.rs). ${STUBS}`

const HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/p18_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP (the #1 false-negative cause): SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm scale. *** \`./scripts/ui.sh stop\` when done.`

const safe = async (prompt, opts) => {
  // Pin EVERY workflow agent to Opus 4.8 (user directive: opus-only). All agent
  // calls in this script route through safe(), so this is the single chokepoint.
  const o = Object.assign({ model: 'opus' }, opts || {})
  try { return await agent(prompt, o) } catch (e) {
    log(`[soft-fail] ${(o && o.label) || '?'}: ${String(e).slice(0, 140)}`); return null
  }
}

// ── The subsystem map (C++ ⇄ Rust ⇄ reference docs). Covers the real-app surface. ──
const SUBSYSTEMS = [
  { key: 'core', title: 'Core model', cpp: 'src/core.h src/commontypes.h src/typeinfer.h', rust: 'src/core/ (node.rs tree.rs kind.rs commontypes.rs typeinfer.rs linemeta.rs clipboard.rs)', spec: 'PORTING_core-model.md', und: 'core-model.md' },
  { key: 'addr', title: 'Address parser', cpp: 'src/addressparser.cpp src/addressparser.h', rust: 'src/addr.rs', spec: '(in PORTING_controller.md / core-model)', und: 'addressparser.md' },
  { key: 'compose', title: 'Compose + undo', cpp: 'src/compose.cpp', rust: 'src/compose.rs', spec: 'PORTING_compose-undo.md', und: 'compose-undo.md' },
  { key: 'controller', title: 'Controller', cpp: 'src/controller.cpp src/controller.h', rust: 'src/controller.rs', spec: 'PORTING_controller.md', und: 'controller.md' },
  { key: 'format', title: 'Format / render', cpp: 'src/format.cpp', rust: 'src/format.rs', spec: 'PORTING_format-render.md', und: 'format-render.md' },
  { key: 'editor', title: 'Editor surface', cpp: 'src/editor.cpp src/editor.h', rust: 'src/ui/editor/ (mod.rs geometry.rs inline_edit.rs hit_test.rs element.rs selection.rs minimap.rs palette.rs tab_cycle.rs)', spec: 'PORTING_editor-surface.md', und: 'editor-surface.md' },
  { key: 'typeselector', title: 'Type-selector popup', cpp: 'src/typeselectorpopup.cpp src/typeselectorpopup.h', rust: 'src/ui/typeselectorpopup.rs', spec: 'PORTING_widgets-dialogs.md (type selector section)', und: 'widgets-dialogs.md', cpptests: 'tests/test_type_selector.cpp' },
  { key: 'generator', title: 'Generator', cpp: 'src/generator.cpp src/generator.h', rust: 'src/generator.rs', spec: 'PORTING_generator.md', und: 'generator.md', cpptests: 'tests/test_generator.cpp' },
  { key: 'disasm', title: 'Disassembly', cpp: 'src/disasm.cpp src/disasm.h', rust: 'src/disasm.rs', spec: 'PORTING_disasm.md', und: 'disasm.md' },
  { key: 'rtti', title: 'RTTI + symbols', cpp: 'src/rtti.cpp src/rtti.h src/symbolstore.cpp src/symbol_downloader.cpp src/rttibrowser.h', rust: 'src/rtti/ (walk.rs browser.rs symbol_store.rs downloader.rs demangle.rs names/)', spec: 'PORTING_rtti-symbols.md', und: 'rtti-symbols.md' },
  { key: 'scanner', title: 'Scanner + panel', cpp: 'src/scanner.cpp src/scanner.h src/scannerpanel.cpp src/scannerpanel.h', rust: 'src/scanner.rs src/ui/scannerpanel.rs', spec: 'PORTING_scanner.md', und: 'scanner.md', cpptests: 'tests/test_scanner.cpp tests/test_scanner_ui.cpp' },
  { key: 'imports', title: 'Imports/exports (PDB, ReClass-XML, source, PE)', cpp: 'src/imports/ (import_pdb.cpp import_pdb_dialog.cpp import_reclass_xml.cpp export_reclass_xml.cpp import_source.cpp pe_debug_info.cpp)', rust: 'src/imports/ (pdb.rs reclass_xml.rs source.rs pe_debug_info.rs)', spec: 'PORTING_imports.md', und: 'imports.md', cpptests: 'tests/test_import_source.cpp' },
  { key: 'providers', title: 'Providers + registry + plugin system', cpp: 'src/providers/ src/providerregistry.cpp src/pluginmanager.cpp src/iplugin.h', rust: 'src/provider/ src/plugin/ (manager.rs contract.rs builtins.rs manifest.rs discovery.rs loader.rs host.rs view.rs reclassnet/)', spec: 'PORTING_providers.md', und: 'providers.md', note: 'plugin SYSTEM is at/over parity (just verified); live-memory backends are STUBS' },
  { key: 'themes', title: 'Themes + theme editor', cpp: 'src/themes/ (theme.cpp thememanager.cpp themeeditor.cpp)', rust: 'src/theme/ (manager.rs model.rs editor.rs profiler.rs) src/ui/theme_apply.rs', spec: 'PORTING_themes.md', und: 'themes.md' },
  { key: 'widgets', title: 'Widgets + dialogs', cpp: 'src/widgets/ src/optionsdialog.cpp src/processpicker.cpp src/sourcechooserpopup.cpp src/gotoaddressdialog.h src/hextoolbarpopup.cpp src/profiler.cpp src/profilerdialog.cpp src/rcxtooltip.h src/commandpalette.h', rust: 'src/ui/ (optionsdialog.rs processpicker.rs sourcechooser.rs gotoaddress.rs hextoolbar.rs dialogs.rs messagebox.rs tooltip.rs enumpicker.rs findbar.rs bookmarkspanel.rs modulespanel.rs contextmenu.rs commandpalette.rs)', spec: 'PORTING_widgets-dialogs.md', und: 'widgets-dialogs.md' },
  { key: 'appshell', title: 'App shell (main window, menus, docks, tabs, start page)', cpp: 'src/main.cpp src/mainwindow.h src/titlebar.cpp src/startpage.h src/workspace_model.h src/dockoverlay.h src/docksizereadout.h src/dock_tab_buttons.h src/tab_source_icon.h src/macos_titlebar.h', rust: 'src/ui/ (window.rs workspace.rs menubar.rs titlebar.rs startpage.rs tabs.rs docks.rs statusbar.rs state.rs) src/main.rs', spec: 'PORTING_app-shell.md', und: 'app-shell.md' },
  { key: 'mcp', title: 'MCP bridge', cpp: 'src/mcp/mcp_bridge.cpp src/mcp/mcp_bridge.h', rust: 'src/mcp/ (tools.rs schemas.rs host.rs dispatch.rs wire.rs mod.rs)', spec: 'PORTING_mcp.md', und: 'mcp.md' },
]

// ── Schemas ───────────────────────────────────────────────────────────────────
const FINDING = { type: 'object', additionalProperties: false, properties: {
  title: { type: 'string' },
  kind: { type: 'string', enum: ['missing-feature', 'behavioral-divergence', 'missing-test-coverage', 'cosmetic', 'stub-excluded'] },
  severity: { type: 'string', enum: ['high', 'medium', 'low'] },
  cpp_ref: { type: 'string' }, rust_ref: { type: 'string' },
  detail: { type: 'string' }, fix_sketch: { type: 'string' },
  fixable_now: { type: 'boolean' },
}, required: ['title', 'kind', 'severity', 'detail', 'fixable_now'] }
const AUDIT_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  subsystem: { type: 'string' },
  parity_estimate: { type: 'integer', description: '0-100, % of in-scope (non-stub) C++ behavior matched by the Rust port' },
  confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
  findings: { type: 'array', items: FINDING },
  strengths: { type: 'string' }, summary: { type: 'string' },
}, required: ['subsystem', 'parity_estimate', 'confidence', 'findings', 'summary'] }

const PLAN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  preliminary_scorecard: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { subsystem: { type: 'string' }, parity: { type: 'integer' } }, required: ['subsystem', 'parity'] } },
  batches: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
    id: { type: 'string' }, title: { type: 'string' }, subsystem: { type: 'string' },
    files: { type: 'array', items: { type: 'string' } },
    fixes: { type: 'array', items: { type: 'string' } },
    needs_ui_verify: { type: 'boolean' }, ui_check: { type: 'string' },
    risk: { type: 'string', enum: ['high', 'medium', 'low'] },
  }, required: ['id', 'title', 'subsystem', 'fixes', 'needs_ui_verify', 'risk'] } },
  deferred: { type: 'array', items: { type: 'string' } },
  summary: { type: 'string' },
}, required: ['batches', 'summary'] }

const IMPL_SCHEMA = { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, changed_files: { type: 'array', items: { type: 'string' } }, new_tests: { type: 'integer' }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] }
const INTEG_SCHEMA = { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, reverted: { type: 'boolean' }, committed: { type: 'string' }, fail_counts: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'reverted', 'notes'] }
const REVIEW_SCHEMA = { type: 'object', additionalProperties: false, properties: { parity_ok: { type: 'boolean' }, resolved: { type: 'boolean', description: 'did this batch genuinely resolve its findings' }, issues: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['parity_ok', 'assessment'] }
const VERIFY_SCHEMA = { type: 'object', additionalProperties: false, properties: { works: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' } }, required: ['works', 'detail'] }
const REAUDIT_SCHEMA = { type: 'object', additionalProperties: false, properties: { subsystem: { type: 'string' }, parity_estimate: { type: 'integer' }, resolved: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } }, regressions: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' } }, required: ['subsystem', 'parity_estimate', 'summary'] }

// ── Phase 1: Coverage check ───────────────────────────────────────────────────
phase('Coverage')
const coverage = await safe(
  `READ-ONLY coverage check for a whole-app C++→Rust parity audit.\n${COMMON}\n\nProposed subsystem map (key → C++ files / Rust modules):\n${JSON.stringify(SUBSYSTEMS.map((s) => ({ key: s.key, cpp: s.cpp, rust: s.rust })), null, 0).slice(0, 4000)}\n\nDo: (1) list every real-app C++ source file under ${QT}/src (NOT third_party qscintilla/raw_pdb, NOT tests/) and confirm each is covered by exactly one subsystem above; name any UNASSIGNED file + which subsystem it belongs to. (2) Confirm the STUB-exclusion boundary is correct + complete (the live-memory backends + CLR + native-runtime-load + subprocess + permission-sandbox). (3) Flag any C++ feature area with NO obvious Rust counterpart (a whole missing module). Return a concise structured note.`,
  { label: 'coverage', phase: 'Coverage', schema: { type: 'object', additionalProperties: false, properties: { unassigned_files: { type: 'array', items: { type: 'string' } }, missing_modules: { type: 'array', items: { type: 'string' } }, stub_boundary_ok: { type: 'boolean' }, notes: { type: 'string' } }, required: ['stub_boundary_ok', 'notes'] }, agentType: 'Explore' })
log(`Coverage: stub_boundary_ok=${coverage && coverage.stub_boundary_ok}; unassigned=${JSON.stringify((coverage && coverage.unassigned_files) || [])}; missing=${JSON.stringify((coverage && coverage.missing_modules) || [])}`)

// ── Phase 2: Audit fan-out ────────────────────────────────────────────────────
phase('Audit')
const auditOne = (s) => safe(
  `DEEP PARITY AUDIT (READ-ONLY) of the "${s.title}" subsystem.\n${COMMON}\n\nC++ source: ${s.cpp}\nRust module(s): ${s.rust}\nReference: ${SPECS}/${(s.spec || '').replace(/ .*/, '')} + ${UND}/${s.und}${s.cpptests ? ` + C++ tests ${s.cpptests} (these define expected behavior — check the Rust port has equivalent behavior/coverage)` : ''}.\n${s.note ? `NOTE: ${s.note}\n` : ''}\nMethod: read the reference doc(s), then the C++ source, then the CURRENT Rust code (fresh line numbers). Enumerate EVERY remaining parity gap: missing features, behavioral divergences (edge cases, ordering, formatting, key bindings, defaults, error handling), and missing test coverage vs the C++ tests. For each finding: kind, severity, cpp_ref (file:line), rust_ref (file:line), detail, a concrete fix_sketch, and fixable_now (false only if it depends on an excluded STUB). Mark stub-dependent items kind="stub-excluded". Give an honest parity_estimate (0-100, % of in-scope C++ behavior matched) + confidence + the port's strengths. Be precise + skeptical; do not invent gaps, do not paper over real ones.`,
  { label: `audit:${s.key}`, phase: 'Audit', schema: AUDIT_SCHEMA, agentType: 'Explore' })

const audits = (await parallel(SUBSYSTEMS.map((s) => () => auditOne(s)))).filter(Boolean)
log(`Audit r1: ${audits.length}/${SUBSYSTEMS.length} subsystems; ${audits.reduce((n, a) => n + (a.findings ? a.findings.filter((f) => f.fixable_now).length : 0), 0)} fixable findings; avg parity≈${Math.round(audits.reduce((n, a) => n + (a.parity_estimate || 0), 0) / Math.max(1, audits.length))}.`)

// ── Phase 3 (interleaved as 2b): Critic re-audit of weak/uncertain subsystems ──
phase('Critic re-audit')
const byKey = {}
for (const a of audits) { const s = SUBSYSTEMS.find((x) => x.title === a.subsystem || a.subsystem.startsWith(x.title)); if (s) byKey[s.key] = a }
const weak = SUBSYSTEMS.filter((s) => {
  const a = byKey[s.key]
  if (!a) return true
  const hasHigh = (a.findings || []).some((f) => f.severity === 'high' && f.fixable_now)
  return a.confidence !== 'high' || (a.parity_estimate || 0) < 92 || hasHigh
})
log(`Critic re-auditing ${weak.length} subsystem(s): ${weak.map((s) => s.key).join(', ')}`)
const critics = (await parallel(weak.map((s) => () => safe(
  `ADVERSARIAL CRITIC RE-AUDIT (READ-ONLY) of "${s.title}". A first auditor reported parity≈${byKey[s.key] ? byKey[s.key].parity_estimate : '?'} with these findings: ${JSON.stringify((byKey[s.key] && byKey[s.key].findings) || []).slice(0, 3000)}.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}; refs ${SPECS}/PORTING + ${UND}/${s.und}${s.cpptests ? ` + ${s.cpptests}` : ''}. Your job: find what the first auditor MISSED and VERIFY/correct its claims (some may be wrong or already-fixed). Look hard at edge cases, C++ test assertions, defaults, formatting/rounding, key bindings, menu/context items, undo grouping, error paths. Return the SAME audit schema with the ADDITIONAL/corrected findings + your own parity_estimate + confidence.`,
  { label: `critic:${s.key}`, phase: 'Critic re-audit', schema: AUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean)
const allAudits = audits.concat(critics)
log(`Critic round added ${critics.reduce((n, a) => n + (a.findings ? a.findings.length : 0), 0)} findings across ${critics.length} subsystems.`)

// ── Phase 4: Triage / plan ────────────────────────────────────────────────────
phase('Plan')
const plan = await safe(
  `TRIAGE + PLAN the parity fixes from the full audit (round 1 + critic). ${COMMON}\n\nAll audit findings (JSON, round1 then critic — DEDUP overlapping findings):\n${JSON.stringify(allAudits.map((a) => ({ subsystem: a.subsystem, parity: a.parity_estimate, conf: a.confidence, findings: (a.findings || []).map((f) => ({ t: f.title, k: f.kind, sev: f.severity, fix: f.fixable_now, cpp: f.cpp_ref, rust: f.rust_ref, sketch: (f.fix_sketch || '').slice(0, 180) })) }))).slice(0, 22000)}\n\nProduce: (a) preliminary_scorecard (one parity % per subsystem, reconciling round1+critic); (b) BATCHES — group the FIXABLE-NOW findings into ordered, FILE-DISJOINT batches (each batch = one coherent commit touching a small file set; order so dependencies/foundational fixes come first; put high-severity first; a batch should be achievable + independently parity-gated). For each batch: id, title, subsystem, files, fixes (concrete), needs_ui_verify (+ ui_check text if it changes visible UI/interaction), risk. EXCLUDE stub-dependent + cosmetic-only-trivial items (list them in deferred with a reason). Keep batches focused; prefer many small safe batches over few big risky ones. summary.`,
  { label: 'plan', phase: 'Plan', schema: PLAN_SCHEMA })
const batches = (plan && plan.batches) || []
log(`Plan: ${batches.length} fix batches. ${plan ? plan.summary.slice(0, 200) : '(plan soft-failed)'}`)

// ── Phase 5: Fix loop (sequential, parity-gated, commit-or-revert) ────────────
phase('Fix')
const fixResults = []
for (const b of batches) {
  const impl = await safe(
    `IMPLEMENT parity fix batch "${b.title}" (subsystem ${b.subsystem}) on the real tree (${OUT}).\n${COMMON}\n\nFiles: ${JSON.stringify(b.files || [])}\nFixes to make (match the C++ behavior precisely; consult the C++ source + ${SPECS}/PORTING + ${UND} for the exact semantics):\n${(b.fixes || []).map((f, i) => `${i + 1}. ${f}`).join('\n')}\n\nWrite real, compiling Rust + tests (port the relevant C++ test assertions where applicable). Do NOT regress existing behavior or touch unrelated modules/stubs. Verify your files compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`). Do NOT commit. Return changed files + compiles + deferred + notes.`,
    { label: `impl:${b.id}`, phase: 'Fix', schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE parity fix batch "${b.title}". Make the WHOLE project green + commit, or revert.\n${GATE}\n\nImpl notes: ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}. Commit message: "fix(parity): ${b.title} — <what changed>" (or "feat(parity): …" if it implements a missing feature).`,
    { label: `integrate:${b.id}`, phase: 'Fix', schema: INTEG_SCHEMA })

  let verify = null
  const landed = integ && integ.green && !integ.reverted && integ.committed && integ.committed !== '(no-op)'
  if (b.needs_ui_verify && landed) {
    verify = await safe(
      `ON-SCREEN VERIFY of parity fix "${b.title}".\n${HARNESS}\n\nCheck: ${b.ui_check || 'the changed UI/interaction now matches the C++ behavior described in the batch fixes'}. Confirm no parity regression elsewhere on screen. Return works + shots + detail.`,
      { label: `verify:${b.id}`, phase: 'Fix', schema: VERIFY_SCHEMA })
  }

  const review = await safe(
    `ADVERSARIAL REVIEW (READ-ONLY) of parity fix "${b.title}" at HEAD (\`cd ${OUT} && git show --stat HEAD\` + read the diff). Spec: ${SPECS}/PORTING + C++ ${QT}. Check: (1) parity preserved (no existing behavior regressed, default build deps unchanged); (2) the batch's findings are GENUINELY resolved (matches the C++ behavior, not a superficial patch); (3) any correctness issue / silent no-op. Return parity_ok, resolved, issues, assessment.`,
    { label: `review:${b.id}`, phase: 'Fix', schema: REVIEW_SCHEMA })

  log(`${b.id} "${b.title}": ${integ && integ.reverted ? 'REVERTED' : (landed ? 'LANDED @' + integ.committed : (integ && integ.green ? 'no-op' : 'UNCLEAR'))}` +
    `${review ? ` | review parity=${review.parity_ok} resolved=${review.resolved}` : ''}${verify ? ` | ui works=${verify.works}` : ''}`)
  fixResults.push({ id: b.id, title: b.title, subsystem: b.subsystem, landed, committed: integ && integ.committed, reverted: integ && integ.reverted, review: review && { parity_ok: review.parity_ok, resolved: review.resolved }, ui_works: verify && verify.works })
}
const touched = Array.from(new Set(fixResults.filter((r) => r.landed).map((r) => r.subsystem)))
log(`Fix loop done: ${fixResults.filter((r) => r.landed).length}/${batches.length} landed; ${fixResults.filter((r) => r.reverted).length} reverted. Touched subsystems: ${touched.join(', ') || '(none)'}`)

// ── Phase 6: Post-fix re-audit of touched subsystems ──────────────────────────
phase('Re-audit')
const touchedSubs = SUBSYSTEMS.filter((s) => touched.includes(s.title) || touched.some((t) => t.startsWith(s.title) || s.title.startsWith(t)))
const reaudits = touchedSubs.length ? (await parallel(touchedSubs.map((s) => () => safe(
  `POST-FIX RE-AUDIT (READ-ONLY) of "${s.title}" after parity fixes landed this run.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}; refs ${SPECS}/PORTING + ${UND}/${s.und}. The fixes applied: ${JSON.stringify(fixResults.filter((r) => r.subsystem === s.title || (r.subsystem || '').startsWith(s.title)).map((r) => r.title))}. Confirm which prior findings are RESOLVED, which remain still_open, and whether anything REGRESSED. Give the updated parity_estimate.`,
  { label: `reaudit:${s.key}`, phase: 'Re-audit', schema: REAUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean) : []
log(`Re-audit: ${reaudits.length} subsystem(s) re-checked.`)

// ── Phase 7: Final quantified parity report ───────────────────────────────────
phase('Report')
const finalParityByKey = {}
for (const s of SUBSYSTEMS) { const a = byKey[s.key]; finalParityByKey[s.key] = { subsystem: s.title, round1: a ? a.parity_estimate : null } }
for (const r of reaudits) { const s = SUBSYSTEMS.find((x) => x.title === r.subsystem || r.subsystem.startsWith(x.title)); if (s) finalParityByKey[s.key].post_fix = r.parity_estimate }

const report = await safe(
  `WRITE THE FINAL PARITY REPORT for the Rust port vs the C++ original, OUTSIDE THE STUBS. ${COMMON}\n\n${BASELINE}\n\n` +
  `Inputs:\n- Preliminary scorecard: ${JSON.stringify((plan && plan.preliminary_scorecard) || [])}\n- Per-subsystem audit (round1+critic) parity estimates + open findings: ${JSON.stringify(allAudits.map((a) => ({ sub: a.subsystem, parity: a.parity_estimate, open: (a.findings || []).filter((f) => f.fixable_now && f.kind !== 'stub-excluded').map((f) => `${f.severity}:${f.title}`) }))).slice(0, 14000)}\n- Fixes landed this run: ${JSON.stringify(fixResults.filter((r) => r.landed).map((r) => ({ id: r.id, title: r.title, sub: r.subsystem, commit: r.committed })))}\n- Reverted/failed batches: ${JSON.stringify(fixResults.filter((r) => r.reverted || (!r.landed && !r.reverted)).map((r) => r.title))}\n- Post-fix re-audit: ${JSON.stringify(reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate, still_open: r.still_open, regressions: r.regressions })))}\n- Deferred (plan): ${JSON.stringify((plan && plan.deferred) || [])}\n\n` +
  `STEPS: (1) Confirm the full 7-step parity gate is GREEN at HEAD (run it) + tree clean (\`git status --short\`); report the test counts. (2) Compute a FINAL parity % per subsystem (use the post-fix estimate where re-audited, else the reconciled audit estimate) and an OVERALL WEIGHTED parity % outside stubs — weight each subsystem by its C++ LOC/importance and STATE YOUR WEIGHTING METHOD. (3) WRITE the report to ${OUT}/_design/PARITY_REPORT.md (use a Markdown table: subsystem | parity % | key remaining gaps; then sections: Overall parity % (with method), What this run fixed (with commit hashes), Remaining in-scope gaps (prioritized, each: what + why-not-yet), Excluded stubs (the deliberate list), and How to reach 100%). Then \`cd ${OUT} && git add _design/PARITY_REPORT.md && git commit -m "docs(parity): wf18 full parity scorecard"\`. (4) Return the structured summary below. Be HONEST + quantitative — the user explicitly wants to know how close to 1:1 we are outside the stubs.`,
  { label: 'report', phase: 'Report', schema: { type: 'object', additionalProperties: false, properties: {
    gate_green: { type: 'boolean' }, test_counts: { type: 'string' },
    overall_parity_pct: { type: 'integer' }, weighting_method: { type: 'string' },
    per_subsystem: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { subsystem: { type: 'string' }, parity: { type: 'integer' }, gaps: { type: 'string' } }, required: ['subsystem', 'parity'] } },
    fixed_this_run: { type: 'array', items: { type: 'string' } },
    remaining_gaps: { type: 'array', items: { type: 'string' } },
    excluded_stubs: { type: 'array', items: { type: 'string' } },
    report_path: { type: 'string' }, committed: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['gate_green', 'overall_parity_pct', 'assessment'] } })

return {
  coverage: coverage && { unassigned: (coverage.unassigned_files || []).length, missing: coverage.missing_modules || [] },
  audit_subsystems: allAudits.length,
  batches_total: batches.length,
  landed: fixResults.filter((r) => r.landed).map((r) => ({ id: r.id, title: r.title, commit: r.committed })),
  reverted: fixResults.filter((r) => r.reverted).map((r) => r.title),
  report,
}
