export const meta = {
  name: 'reclass-debug-view-parity',
  description: 'Port the C++ VM_Debug pane view-mode to 1:1 parity in the Rust port (the only "debugger"-class feature in the authoritative C++; there is NO process debugger). The C++ has enum ViewMode { VM_Reclass, VM_Rendered, VM_Debug } (mainwindow.h:144); the Rust port (src/ui/state.rs) has only Tree + Rendered — the third Debug mode is missing. Two sequential parity-GATED commit-or-revert batches: D1 model+text — add ViewMode::Debug (3-way switch/cycle + label) and implement generate_debug_text matching C++ generateDebugText (main.cpp:5534) byte-for-byte from the Rust LineMeta, with golden tests; D2 UI — render the read-only styled Debug pane (mirroring applyDebugStyles/styleDebugText intent), extend the view-mode switcher from 2→3 modes, and wire refresh-on-change (updateDebugView), verified on-screen. Then re-audit app-shell + editor-surface and UPDATE _design/PARITY_REPORT.md. ALL agents pinned to Opus 4.8. Existing suite MUST stay green; no live-process work (the Debug view reads the existing structure/LineMeta only); stubs unchanged.',
  whenToUse: 'Port the C++ VM_Debug view-mode to 1:1 parity.',
  phases: [
    { title: 'D1 debug-view model + text' },
    { title: 'D2 debug-view UI + switcher' },
    { title: 'Re-audit' },
    { title: 'Report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const SPECS = `${OUT}/_design/specs`
const UND = `${OUT}/_design/understand`
const REPORT = `${OUT}/_design/PARITY_REPORT.md`

const GATE = `*** PARITY GATE — the EXISTING suite MUST stay green; never leave the tree red or commit red. ***
Run ALL from ${OUT} and require success:
  1) default/ui build:  \`${LP} cargo build 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  2) headless build:    \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  3) plugins build:     \`${LP} cargo build --features plugins 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  4) full tests:        \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  5) ui tests:          \`${LP} cargo test --lib "ui::" 2>&1 | grep 'test result' | tail -1\`  → 0 failed
  6) plugins tests:     \`${LP} cargo test --features plugins --lib 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  7) bare tests:        \`cargo test --no-default-features 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
If ALL pass: \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` → green=true, committed=<hash>. If a piece can't go green, prefer trimming to a working+tested subset over reverting the whole batch.
If the batch fundamentally can't integrate: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` → green=false, reverted=true + notes. NEVER touch target/ or .git config.`

const COMMON = `Rust port: ${OUT}. Authoritative C++ original: ${QT}. Reference: ${SPECS}/PORTING_app-shell.md + ${SPECS}/PORTING_editor-surface.md + ${UND}/app-shell.md + ${UND}/editor-surface.md. Match the C++ behavior PRECISELY (read main.cpp for the exact debug-text format + styling). Aesthetic = Zed (src/ui/design.rs). The Debug view is a READ-ONLY developer view of the existing structure/line model — NO live process, NO new deps. Keep default-build deps unchanged; add #[cfg(test)] tests. Do NOT regress existing behavior (the existing Tree + Rendered modes must be byte-identical) or touch unrelated modules/stubs. Do NOT commit in design/impl/review steps.`

const HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/p22_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP: SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm scale. *** \`./scripts/ui.sh stop\` when done.`

const safe = async (prompt, opts) => {
  const o = Object.assign({ model: 'opus' }, opts || {})
  let lastErr
  for (let attempt = 1; attempt <= 3; attempt++) {
    try { return await agent(prompt, o) }
    catch (e) { lastErr = e; log(`[retry ${attempt}/3] ${o.label || '?'}: ${String(e).slice(0, 90)}`) }
  }
  log(`[soft-fail] ${o.label || '?'}: ${String(lastErr).slice(0, 140)} — gave up after 3 attempts`)
  return null
}

const PLAN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  steps: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { file: { type: 'string' }, action: { type: 'string', enum: ['create', 'edit', 'delete'] }, what: { type: 'string' } }, required: ['file', 'action', 'what'] } },
  cpp_behavior: { type: 'string' }, specifics: { type: 'string' }, risks: { type: 'string' }, summary: { type: 'string' },
}, required: ['summary'] }
const IMPL_SCHEMA = { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, changed_files: { type: 'array', items: { type: 'string' } }, new_tests: { type: 'integer' }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] }
const INTEG_SCHEMA = { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, reverted: { type: 'boolean' }, committed: { type: 'string' }, trimmed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'reverted', 'notes'] }
const REVIEW_SCHEMA = { type: 'object', additionalProperties: false, properties: { parity_ok: { type: 'boolean' }, resolved: { type: 'boolean' }, wired_live: { type: 'boolean' }, issues: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['parity_ok', 'assessment'] }
const VERIFY_SCHEMA = { type: 'object', additionalProperties: false, properties: { works: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' } }, required: ['works', 'detail'] }
const REAUDIT_SCHEMA = { type: 'object', additionalProperties: false, properties: { subsystem: { type: 'string' }, parity_estimate: { type: 'integer' }, resolved: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } }, regressions: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' } }, required: ['subsystem', 'parity_estimate', 'summary'] }

async function runBatch(b) {
  phase(b.title)
  const plan = await safe(
    `DESIGN STEP for "${b.title}".\n${COMMON}\n\nC++ source to study: ${b.cpp}\nRust file(s): ${b.rust}\n\nGoal:\n${b.goal}\n\nRead the C++ source (exact semantics + file:line), then the current Rust, and produce an ordered plan: exact files/edits, key signatures, and ENUMERATE the specifics you discover (the EXACT generate_debug_text line format + the special-glyph substitution table + the LineMeta field mapping Rust↔C++ + the LineKind names + how Rendered is rendered + where the view-mode switcher UI lives). Keep increments small + compiling.`,
    { label: `design:${b.key}`, phase: b.title, schema: PLAN_SCHEMA })

  const impl = await safe(
    `IMPLEMENT "${b.title}" on the real tree (${OUT}).\n${COMMON}\n\nPlan: ${JSON.stringify((plan && plan.steps) || []).slice(0, 3500)}\nC++ behavior: ${plan ? (plan.cpp_behavior || '').slice(0, 900) : ''}\nSpecifics: ${plan ? (plan.specifics || '').slice(0, 1000) : ''}\n\n${b.impl}\n\nWrite real, compiling Rust + tests. When you add the ViewMode::Debug variant, handle EVERY match site (the gate will catch non-exhaustive matches). Verify compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`). Do NOT commit. Return changed files + compiles + deferred + notes.`,
    { label: `impl:${b.key}`, phase: b.title, schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE "${b.title}". Make the WHOLE project green + commit, or trim/revert.\n${GATE}\n\nImpl notes: ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}. Commit message: "${b.commitType}(parity): wf22 ${b.title} — <what changed>".`,
    { label: `integrate:${b.key}`, phase: b.title, schema: INTEG_SCHEMA })

  const landed = integ && integ.green && !integ.reverted && integ.committed && integ.committed !== '(no-op)'
  let verify = null
  if (b.uiVerify && landed) {
    verify = await safe(
      `ON-SCREEN VERIFY of "${b.title}".\n${HARNESS}\n\nCheck: ${b.uiVerify}\nConfirm the existing Tree + Rendered modes still render correctly (parity). Return works + shots + detail.`,
      { label: `verify:${b.key}`, phase: b.title, schema: VERIFY_SCHEMA })
  }

  const review = await safe(
    `ADVERSARIAL REVIEW (READ-ONLY) of "${b.title}" at HEAD (\`cd ${OUT} && git show --stat HEAD\` + read the diff). C++ truth: ${b.cpp}. Check: (1) parity preserved — existing Tree/Rendered modes byte-identical, default deps unchanged; (2) the debug-text output matches C++ generateDebugText format (glyph substitutions, the "## L=.. nKind=.. depth=.. cmtStart=.. tW=.. nW=.." meta + flags) and is driven by the real LineMeta; (3) the Debug mode is REACHABLE from the live view-mode switcher (trace it), not just a function; (4) correctness issues / silent no-ops. Return parity_ok, resolved, wired_live, issues, assessment.`,
    { label: `review:${b.key}`, phase: b.title, schema: REVIEW_SCHEMA })

  log(`${b.title}: ${integ && integ.reverted ? 'REVERTED' : (landed ? 'LANDED @' + integ.committed : (integ && integ.green ? 'no-op' : 'UNCLEAR'))}` +
    `${(integ && integ.trimmed && integ.trimmed.length) ? ` (trimmed ${integ.trimmed.length})` : ''}` +
    `${review ? ` | review parity=${review.parity_ok} resolved=${review.resolved} wired=${review.wired_live}` : ''}${verify ? ` | ui works=${verify.works}` : ''}`)
  return { key: b.key, title: b.title, landed, committed: integ && integ.committed, reverted: integ && integ.reverted, trimmed: integ && integ.trimmed, review: review && { parity_ok: review.parity_ok, resolved: review.resolved, wired_live: review.wired_live }, ui_works: verify && verify.works }
}

const BATCHES = [
  { key: 'd1', title: 'D1 debug-view model + text', commitType: 'feat',
    cpp: 'src/mainwindow.h:144 (enum ViewMode) + src/main.cpp:5534-5615 (generateDebugText) + src/core.h (LineMeta fields + kindToString)', rust: 'src/ui/state.rs (ViewMode enum) + src/core/linemeta.rs + a new generate_debug_text (place in compose.rs or a new src/core/debug_view.rs — your call)', refs: `${SPECS}/PORTING_app-shell.md + ${SPECS}/PORTING_editor-surface.md`,
    goal: `Add the THIRD view mode and its text generator. (1) Extend src/ui/state.rs ViewMode from {Tree, Rendered} to {Tree, Rendered, Debug}: 3-way cycle in toggled() (Tree→Rendered→Debug→Tree, matching the C++ index 0/1/2 order: VM_Reclass→VM_Rendered→VM_Debug), a display name ("Debug"), and handle EVERY match site. (2) Implement generate_debug_text that reproduces C++ MainWindow::generateDebugText (main.cpp:5534) EXACTLY: for each editor line → \`offsetMargin + "|" + annotated + meta\`, where annotated applies the glyph substitutions (▸0x25B8→"[>]", ▾0x25BE→"[v]", │0x2502→"[|]", ├0x251C→"[+]", └0x2514→"[L]", …0x2026→"[..]", →0x2192→"[->]", ·0x00B7→"[.]", space→·0x00B7) and meta = \`  ## L=<i> <LineKindName> nKind=<kindToString(nodeKind)> depth=<d> cmtStart=<c> tW=<typeW> nW=<nameW>\` + flags (\` static\`,\` cont\`,\` member\`,\` arrElem\`,\` fold+\`/\` fold-\`,\` hint@<n>\`). LineKind names = ["CmdRow","Blank","Header","Field","Cont","Footer","ArrSep"]. Map the Rust LineMeta fields to the C++ ones precisely.`,
    impl: `Add the enum variant + 3-way cycle + name, fixing every match on ViewMode. Implement generate_debug_text against the Rust line model + LineMeta (find the equivalents of offsetText, lineKind, nodeKind, depth, commentStart, effectiveTypeW, effectiveNameW, isStaticLine/isContinuation/isMemberLine/isArrayElement, foldHead/foldCollapsed, typeHintStart). Add GOLDEN tests: build a representative structure (a class with a few fields, a nested struct, an array, a pointer, a comment) and assert the exact debug-text lines match the C++ format. This batch need NOT render anything yet (D2 does the UI) — just the model + text + tests, all green.` },

  { key: 'd2', title: 'D2 debug-view UI + switcher', commitType: 'feat',
    cpp: 'src/main.cpp:5301 setupDebugSci, :5341 applyDebugStyles, :5616 styleDebugText, :5597 updateDebugView, :5743 updateAllDebugPanes, :5387 the mode index mapping + the view-mode switcher widget', rust: 'the editor/pane render path (find where ViewMode::Rendered is rendered) + the view-mode switcher UI (find where Tree↔Rendered is toggled — statusbar.rs / a toolbar / window.rs) + src/ui/editor/', refs: `${SPECS}/PORTING_app-shell.md + ${SPECS}/PORTING_editor-surface.md`,
    goal: `Render the Debug view + expose it in the switcher. (1) When the active view mode is Debug, render a READ-ONLY monospace text pane showing generate_debug_text(), mirroring how the Rendered (C/C++) view is rendered. (2) Apply styling matching applyDebugStyles/styleDebugText INTENT (Zed-themed): dim the offset margin + the "## ..." meta annotation, normal weight for the line text — exact Qt scintilla colors are substituted by the theme, match the structure (margin dim / meta dim) not the literal hex. (3) Extend the view-mode switcher UI from 2 → 3 options (Tree / C/C++ / Debug). (4) Wire refresh: regenerate the debug text when the structure changes while in Debug mode (the C++ updateDebugView/updateAllDebugPanes). Existing Tree + Rendered behavior must stay byte-identical.`,
    impl: `Mirror the Rendered-view render path for Debug; add the 3rd switcher option; wire change-driven refresh. Add tests where unit-testable (switcher exposes 3 modes; debug pane shows the generated text). Keep it read-only.`,
    uiVerify: `open the sample, switch the view mode to Debug via the switcher, and confirm the annotated debug dump renders — offset margins on the left, the line text with [>] / [|] / · substitutions, and the "## L=.. nKind=.. depth=.." meta annotations per line, styled (dim margin/meta). Then switch back to Tree and to C/C++ and confirm both still render correctly (parity).` },
]

const results = []
for (const b of BATCHES) { results.push(await runBatch(b)) }

phase('Re-audit')
const anyLanded = results.some((r) => r.landed)
const SUBS = [
  { key: 'appshell', title: 'App shell', cpp: 'src/main.cpp src/mainwindow.h', rust: 'src/ui/window.rs src/ui/state.rs src/ui/statusbar.rs', old: 94 },
  { key: 'editor', title: 'Editor surface', cpp: 'src/editor.cpp src/main.cpp (view modes)', rust: 'src/ui/editor/ src/core/linemeta.rs', old: 96 },
]
const reaudits = anyLanded ? (await parallel(SUBS.map((s) => () => safe(
  `POST-FIX RE-AUDIT (READ-ONLY) of "${s.title}" after the wf22 VM_Debug view-mode port landed.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}. The wf18 report scored this ${s.old}%. NOTE: the missing 3rd view mode (VM_Debug) was a PREVIOUSLY-UNSCORED gap now closed by wf22 (${JSON.stringify(results.filter((r) => r.landed).map((r) => r.title))}). Give an honest UPDATED parity_estimate, listing what VM_Debug resolved + any still_open + regressions.`,
  { label: `reaudit:${s.key}`, phase: 'Re-audit', schema: REAUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean) : []
log(`Re-audit: ${reaudits.map((r) => `${r.subsystem}=${r.parity_estimate}%`).join(' ')}`)

phase('Report')
const report = await safe(
  `UPDATE the parity report after the wf22 VM_Debug view-mode port.\n${COMMON}\n\n` +
  `Batches: ${JSON.stringify(results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted, review: r.review })))}\n` +
  `Re-audit: ${JSON.stringify(reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate, still_open: r.still_open })))}\n\n` +
  `CONTEXT: ${REPORT} currently shows overall ≈96% (95.99%, wf21) with documented LOC weights (total 41,962; app-shell w=6000 @94, editor-surface w=5077 @96). VM_Debug was a previously-UNSCORED gap, so be careful + HONEST: only adjust a subsystem % if the re-audit genuinely re-derives it (closing a previously-uncounted gap may leave the % ~unchanged since it wasn't deducted before — say so plainly rather than inventing a bump).\n` +
  `STEPS: (1) Confirm the full 7-step parity gate GREEN at HEAD + tree clean; capture new test counts. (2) Read ${REPORT}; update any subsystem rows the re-audit genuinely moved, recompute the overall (state old→new honestly — likely ~unchanged at 96% since this closes an unscored dev-view gap), note VM_Debug as a completeness win regardless of the %, and ADD a "## wf22 follow-up" section (commit hashes + what landed + test counts + overall). Keep the rest intact. (3) \`cd ${OUT} && git add ${REPORT} && git commit -m "docs(parity): wf22 — VM_Debug view-mode 1:1 port"\`. (4) Return the structured summary. Honest + quantitative.`,
  { label: 'report', phase: 'Report', schema: { type: 'object', additionalProperties: false, properties: {
    gate_green: { type: 'boolean' }, test_counts: { type: 'string' },
    old_overall_pct: { type: 'integer' }, new_overall_pct: { type: 'integer' },
    landed: { type: 'array', items: { type: 'string' } }, vm_debug_done: { type: 'boolean' },
    still_open: { type: 'array', items: { type: 'string' } }, committed: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['gate_green', 'new_overall_pct', 'assessment'] } })

return {
  batches: results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted, ui_works: r.ui_works })),
  reaudit: reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate })),
  report,
}
