export const meta = {
  name: 'reclass-widgets-parity',
  description: 'Fourth parity follow-up: close the fixable Widgets+dialogs gaps from _design/PARITY_REPORT.md (subsystem at 90%). Sequential parity-GATED commit-or-revert batches (design → implement → 7-step gate → review; UI batches verify on-screen with the SCALE rule). W1 OptionsDialog search keywords (match the C++ per-option keyword set) + MessageBox detail-list + width clamp. W2 ProcessPicker path column + lastAttachedProcess highlight/restore + SourceChooser pid==0 handling + attempt confirm-dialog destructive default-focus (gpui-limited; document if not achievable). Then re-audit widgets and UPDATE the report. ALL agents pinned to Opus 4.8. Existing suite MUST stay green; intentional Zed substitutions (DialogButton chrome) stay out of scope; stubs unchanged.',
  whenToUse: 'Land the widgets+dialogs parity batch from the wf18 report.',
  phases: [
    { title: 'W1 options + messagebox' },
    { title: 'W2 processpicker + sourcechooser' },
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
If ALL pass: \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` → green=true, committed=<hash>. If a piece can't go green, prefer trimming to a working+tested subset over reverting the whole batch; commit the green subset + report what was trimmed.
If the batch fundamentally can't integrate: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` → green=false, reverted=true + notes. NEVER touch target/ or .git config.`

const COMMON = `Rust port: ${OUT}. Authoritative C++ original: ${QT}. Reference: ${SPECS}/PORTING_widgets-dialogs.md + ${UND}/widgets-dialogs.md + the wf18 findings in ${REPORT}. Match the C++ behavior PRECISELY (read the C++ source for exact semantics). Aesthetic = Zed (src/ui/design.rs); DialogButton/window chrome is an INTENTIONAL Zed substitution — do NOT try to match Qt chrome pixel-for-pixel, only the behavior/content. Keep default-build deps unchanged; add #[cfg(test)] tests. Do NOT regress existing behavior or touch unrelated modules/stubs. Do NOT commit in design/impl/review steps.`

const HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/p21_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP: SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm scale. *** \`./scripts/ui.sh stop\` when done.`

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
    `DESIGN STEP for parity batch "${b.title}".\n${COMMON}\n\nC++ source to study: ${b.cpp}\nRust file(s): ${b.rust}\n\nGoal:\n${b.goal}\n\nRead the C++ source (exact semantics + file:line), then the current Rust, and produce an ordered plan: exact files/edits, key signatures, and ENUMERATE the specifics you discover (the exact OptionsDialog search keyword sets per option, the MessageBox width-clamp value + detail-list layout, the ProcessPicker column set + lastAttachedProcess persistence key, the SourceChooser pid==0 rule). Keep increments small + compiling.`,
    { label: `design:${b.key}`, phase: b.title, schema: PLAN_SCHEMA })

  const impl = await safe(
    `IMPLEMENT parity batch "${b.title}" on the real tree (${OUT}).\n${COMMON}\n\nPlan: ${JSON.stringify((plan && plan.steps) || []).slice(0, 3500)}\nC++ behavior: ${plan ? (plan.cpp_behavior || '').slice(0, 800) : ''}\nSpecifics: ${plan ? (plan.specifics || '').slice(0, 900) : ''}\n\n${b.impl}\n\nWrite real, compiling Rust + tests. Verify compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`). Do NOT commit. Return changed files + compiles + deferred + notes.`,
    { label: `impl:${b.key}`, phase: b.title, schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE parity batch "${b.title}". Make the WHOLE project green + commit, or trim/revert.\n${GATE}\n\nImpl notes: ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}. Commit message: "${b.commitType}(parity): wf21 ${b.title} — <what changed>".`,
    { label: `integrate:${b.key}`, phase: b.title, schema: INTEG_SCHEMA })

  const landed = integ && integ.green && !integ.reverted && integ.committed && integ.committed !== '(no-op)'
  let verify = null
  if (b.uiVerify && landed) {
    verify = await safe(
      `ON-SCREEN VERIFY of parity batch "${b.title}".\n${HARNESS}\n\nCheck: ${b.uiVerify}\nConfirm no parity regression elsewhere on screen. Return works + shots + detail.`,
      { label: `verify:${b.key}`, phase: b.title, schema: VERIFY_SCHEMA })
  }

  const review = await safe(
    `ADVERSARIAL REVIEW (READ-ONLY) of "${b.title}" at HEAD (\`cd ${OUT} && git show --stat HEAD\` + read the diff). C++ truth: ${b.cpp}. Check: (1) parity preserved (no regressions, default deps unchanged); (2) the goal GENUINELY matches the C++ behavior (not superficial); (3) REACHABLE from the live app (trace the path), not just tests; (4) correctness issues / silent no-ops. Return parity_ok, resolved, wired_live, issues, assessment.`,
    { label: `review:${b.key}`, phase: b.title, schema: REVIEW_SCHEMA })

  log(`${b.title}: ${integ && integ.reverted ? 'REVERTED' : (landed ? 'LANDED @' + integ.committed : (integ && integ.green ? 'no-op' : 'UNCLEAR'))}` +
    `${(integ && integ.trimmed && integ.trimmed.length) ? ` (trimmed ${integ.trimmed.length})` : ''}` +
    `${review ? ` | review parity=${review.parity_ok} resolved=${review.resolved} wired=${review.wired_live}` : ''}${verify ? ` | ui works=${verify.works}` : ''}`)
  return { key: b.key, title: b.title, landed, committed: integ && integ.committed, reverted: integ && integ.reverted, trimmed: integ && integ.trimmed, review: review && { parity_ok: review.parity_ok, resolved: review.resolved, wired_live: review.wired_live }, ui_works: verify && verify.works }
}

const BATCHES = [
  { key: 'w1', title: 'W1 options + messagebox', commitType: 'fix',
    cpp: 'src/optionsdialog.cpp src/widgets/themed_messagebox.cpp src/widgets/themed_messagebox.h', rust: 'src/ui/optionsdialog.rs src/ui/messagebox.rs', refs: `${SPECS}/PORTING_widgets-dialogs.md`,
    goal: `Two widget gaps: (1) OptionsDialog SEARCH omits some keywords — match the C++ per-option keyword set so searching finds every option C++ finds (read optionsdialog.cpp for the exact keyword/synonym strings attached to each option/row). (2) MessageBox differs: port the DETAIL-LIST (the expandable/secondary detail text or item list C++ shows) and the WIDTH CLAMP (the max-width C++ applies so long messages wrap instead of stretching).`,
    impl: `Match optionsdialog.cpp's search keyword sets exactly + themed_messagebox.cpp's detail-list + width clamp. Add tests for the search-match keyword coverage + the width clamp logic. Keep the Zed dialog chrome.`,
    uiVerify: `open Options and type a search term that C++ matches via a keyword (not the literal label) and confirm the option appears; trigger a message box with detail + a long message and confirm the detail list shows and the width is clamped (wraps).` },

  { key: 'w2', title: 'W2 processpicker + sourcechooser', commitType: 'fix',
    cpp: 'src/processpicker.cpp src/sourcechooserpopup.cpp src/widgets/themed_messagebox.cpp', rust: 'src/ui/processpicker.rs src/ui/sourcechooser.rs src/ui/messagebox.rs', refs: `${SPECS}/PORTING_widgets-dialogs.md`,
    goal: `Three gaps: (1) ProcessPicker — add the PATH column C++ shows + the lastAttachedProcess behavior (remember + pre-select/highlight the last attached process across opens, persisted as C++ does). (2) SourceChooser — handle pid==0 the way C++ does (read sourcechooserpopup.cpp for the exact pid==0 rule — likely hide/disable or treat as a non-process source). (3) Confirm-dialog destructive DEFAULT-FOCUS — C++ focuses the safe/cancel (or the specified) button by default on destructive prompts; ATTEMPT to apply the same default focus in the gpui dialog, and if gpui cannot set initial button focus, document it precisely as a platform limit rather than faking it.`,
    impl: `Match processpicker.cpp columns + lastAttachedProcess persistence (find the settings key), sourcechooserpopup.cpp pid==0 handling, and the destructive default-focus. Add tests where unit-testable (column model, pid==0 filter, default-focus selection). If gpui can't focus a dialog button initially, leave a precise note (do not fake it).`,
    uiVerify: `open the process picker and confirm the path column shows + the last-attached process is highlighted/preselected; open the source chooser and confirm pid==0 behaves as C++; trigger a destructive confirm and observe which button has default focus.` },
]

const results = []
for (const b of BATCHES) { results.push(await runBatch(b)) }

phase('Re-audit')
const anyLanded = results.some((r) => r.landed)
const reaudits = anyLanded ? (await parallel([{ key: 'widgets', title: 'Widgets + dialogs', cpp: 'src/optionsdialog.cpp src/processpicker.cpp src/sourcechooserpopup.cpp src/widgets/', rust: 'src/ui/optionsdialog.rs src/ui/processpicker.rs src/ui/sourcechooser.rs src/ui/messagebox.rs src/ui/dialogs.rs', old: 90 }].map((s) => () => safe(
  `POST-FIX RE-AUDIT (READ-ONLY) of "${s.title}" after the wf21 batches landed.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}. The wf18 report scored Widgets+dialogs ${s.old}%. The wf21 fixes: ${JSON.stringify(results.filter((r) => r.landed).map((r) => r.title))}. Confirm which prior gaps are RESOLVED, which remain still_open (incl. the intentional Zed-substitution items that should NOT be counted against parity), and whether anything REGRESSED. Give the UPDATED parity_estimate.`,
  { label: `reaudit:${s.key}`, phase: 'Re-audit', schema: REAUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean) : []
log(`Re-audit: ${reaudits.map((r) => `${r.subsystem}=${r.parity_estimate}%`).join(' ')}`)

phase('Report')
const report = await safe(
  `UPDATE the parity report after the wf21 widgets batches.\n${COMMON}\n\n` +
  `Batches: ${JSON.stringify(results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted, trimmed: (r.trimmed || []).length, review: r.review })))}\n` +
  `Re-audit: ${JSON.stringify(reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate, still_open: r.still_open })))}\n\n` +
  `CONTEXT: ${REPORT} currently shows overall ≈95% (95.47%, wf20) with documented LOC weights (total 41,962; Widgets+dialogs w=3632 @90). Only the widgets score moves; recompute Σ(parity×weight)/Σ(weight) with the SAME weights.\n` +
  `STEPS: (1) Confirm the full 7-step parity gate GREEN at HEAD + tree clean; capture new test counts. (2) Read ${REPORT}; UPDATE the Widgets+dialogs row to the new %, RECOMPUTE the overall (state old→new honestly), update the overall section + roadmap, and ADD a "## wf21 follow-up" section (commit hashes + fixes + test counts + old→new overall). Keep the rest intact. (3) \`cd ${OUT} && git add ${REPORT} && git commit -m "docs(parity): wf21 — widgets+dialogs, overall <old>%→<new>%"\`. (4) Return the structured summary. Honest + quantitative.`,
  { label: 'report', phase: 'Report', schema: { type: 'object', additionalProperties: false, properties: {
    gate_green: { type: 'boolean' }, test_counts: { type: 'string' },
    old_overall_pct: { type: 'integer' }, new_overall_pct: { type: 'integer' }, widgets_pct: { type: 'integer' },
    landed: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } },
    committed: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['gate_green', 'new_overall_pct', 'assessment'] } })

return {
  batches: results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted })),
  reaudit: reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate })),
  report,
}
