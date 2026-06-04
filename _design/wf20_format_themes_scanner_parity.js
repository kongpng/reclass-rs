export const meta = {
  name: 'reclass-format-themes-scanner-parity',
  description: 'Third parity follow-up (after wf18 audit + wf19 MCP/app-shell). Lands the next three roadmap batches from _design/PARITY_REPORT.md, each a sequential parity-GATED commit-or-revert (design → implement → 7-step gate → review; UI batches verify on-screen with the SCALE rule). F1 Format de-duplication: delete the compose.rs render duplicate and route the live editor through the verified format.rs, fixing half-to-even→half-away rounding, %g carry, and dropped -0.0 sign in one move (golden tests vs C++ format.cpp). T1 Themes authoritative-default sync: re-derive heat-color derivation + from_json fallbacks + the 6/8 divergent default selection hexes from the SHIPPED C++ theme JSON, and REWRITE the fidelity tests that currently lock the wrong values. S1 Scanner UI-flow: apply the C++ smart-filter defaults on scan-mode switch + port the remaining keyboard shortcuts. Then re-audit format/themes/scanner and UPDATE _design/PARITY_REPORT.md (new commits, recomputed overall %, new test counts). ALL agents pinned to Opus 4.8. Existing suite MUST stay green throughout; live-memory/CLR/native-runtime/subprocess/sandbox stubs and core typeinfer (pending authoritative-revision confirmation) stay out of scope.',
  whenToUse: 'Land the format + themes + scanner parity batches from the wf18 report.',
  phases: [
    { title: 'F1 format de-dup' },
    { title: 'T1 themes default sync' },
    { title: 'S1 scanner UI-flow' },
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
If ALL pass (new/rewritten tests included): \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` → green=true, committed=<hash>. If a piece can't be made green, prefer trimming to a working+tested subset over reverting the whole batch; commit the green subset + report what was trimmed.
If the batch fundamentally can't integrate: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` → green=false, reverted=true + notes. NEVER touch target/ or .git config.`

const COMMON = `Rust port: ${OUT}. Authoritative C++ original: ${QT}. Reference: per-subsystem spec ${SPECS}/PORTING_<area>.md + behavior doc ${UND}/<area>.md + the wf18 findings in ${REPORT}. Match the C++ behavior PRECISELY (read the C++ source for exact semantics; docs are guides). Aesthetic = Zed (src/ui/design.rs). Keep the default build's deps unchanged; add #[cfg(test)] tests for new logic and port the relevant C++ test assertions. Do NOT regress existing behavior or touch unrelated modules/stubs. Do NOT commit in design/impl/review steps (integrate owns commits/reverts).`

const HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/p20_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP (the #1 false-negative cause): SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm scale. *** \`./scripts/ui.sh stop\` when done.`

// Pin every agent to Opus 4.8 + retry transient failures (chiefly server-side
// rate limiting). All calls route through safe().
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
  cpp_behavior: { type: 'string' }, specifics: { type: 'string', description: 'concrete enumerated items discovered (exact C++ values/shortcuts/JSON paths)' },
  risks: { type: 'string' }, summary: { type: 'string' },
}, required: ['summary'] }
const IMPL_SCHEMA = { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, changed_files: { type: 'array', items: { type: 'string' } }, new_tests: { type: 'integer' }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] }
const INTEG_SCHEMA = { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, reverted: { type: 'boolean' }, committed: { type: 'string' }, trimmed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'reverted', 'notes'] }
const REVIEW_SCHEMA = { type: 'object', additionalProperties: false, properties: { parity_ok: { type: 'boolean' }, resolved: { type: 'boolean' }, wired_live: { type: 'boolean' }, issues: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['parity_ok', 'assessment'] }
const VERIFY_SCHEMA = { type: 'object', additionalProperties: false, properties: { works: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' } }, required: ['works', 'detail'] }
const REAUDIT_SCHEMA = { type: 'object', additionalProperties: false, properties: { subsystem: { type: 'string' }, parity_estimate: { type: 'integer' }, resolved: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } }, regressions: { type: 'array', items: { type: 'string' } }, summary: { type: 'string' } }, required: ['subsystem', 'parity_estimate', 'summary'] }

async function runBatch(b) {
  phase(b.title)
  const plan = await safe(
    `DESIGN STEP for parity batch "${b.title}".\n${COMMON}\n\nC++ source to study: ${b.cpp}\nRust file(s) to change: ${b.rust}\nReference: ${b.refs}\n\nGoal:\n${b.goal}\n\nRead the C++ source (exact semantics + file:line), then the current Rust, and produce an ordered plan: exact files/edits (incl. any deletes), key signatures, and ENUMERATE the specifics you discover (exact C++ rounding/format rules, the exact shipped-JSON theme values + hexes, the exact scanner shortcuts/smart-filter defaults). Keep increments small + compiling.`,
    { label: `design:${b.key}`, phase: b.title, schema: PLAN_SCHEMA })

  const impl = await safe(
    `IMPLEMENT parity batch "${b.title}" on the real tree (${OUT}).\n${COMMON}\n\nPlan: ${JSON.stringify((plan && plan.steps) || []).slice(0, 3500)}\nC++ behavior to match: ${plan ? (plan.cpp_behavior || '').slice(0, 800) : ''}\nSpecifics: ${plan ? (plan.specifics || '').slice(0, 900) : ''}\n\n${b.impl}\n\nWrite real, compiling Rust + tests (port the relevant C++ test assertions; for themes, REWRITE the fidelity tests to assert the authoritative shipped-JSON values AND change the code to emit them — both, so the rewritten tests pass). Verify your files compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`). Do NOT commit. Return changed files + compiles + deferred + notes.`,
    { label: `impl:${b.key}`, phase: b.title, schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE parity batch "${b.title}". Make the WHOLE project green + commit, or trim/revert.\n${GATE}\n\nImpl notes: ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}. Commit message: "${b.commitType}(parity): wf20 ${b.title} — <what changed>".`,
    { label: `integrate:${b.key}`, phase: b.title, schema: INTEG_SCHEMA })

  const landed = integ && integ.green && !integ.reverted && integ.committed && integ.committed !== '(no-op)'
  let verify = null
  if (b.uiVerify && landed) {
    verify = await safe(
      `ON-SCREEN VERIFY of parity batch "${b.title}".\n${HARNESS}\n\nCheck: ${b.uiVerify}\nConfirm no parity regression elsewhere on screen. Return works + shots + detail.`,
      { label: `verify:${b.key}`, phase: b.title, schema: VERIFY_SCHEMA })
  }

  const review = await safe(
    `ADVERSARIAL REVIEW (READ-ONLY) of parity batch "${b.title}" at HEAD (\`cd ${OUT} && git show --stat HEAD\` + read the diff). C++ truth: ${b.cpp}; ref ${b.refs}. Check: (1) parity preserved (no existing behavior regressed, default deps unchanged); (2) the goal is GENUINELY resolved + matches the C++ behavior (for format: half-AWAY rounding, %g carry, -0.0 sign now correct AND the duplicate render path is actually gone; for themes: the emitted values match the shipped C++ JSON and the tests assert the AUTHORITATIVE values not the old wrong ones; for scanner: smart-filter defaults + shortcuts reachable from the live panel); (3) for UI/behavior, REACHABLE from the live app (trace the path), not just tests; (4) correctness issues / silent no-ops. Return parity_ok, resolved, wired_live, issues, assessment.`,
    { label: `review:${b.key}`, phase: b.title, schema: REVIEW_SCHEMA })

  log(`${b.title}: ${integ && integ.reverted ? 'REVERTED' : (landed ? 'LANDED @' + integ.committed : (integ && integ.green ? 'no-op' : 'UNCLEAR'))}` +
    `${(integ && integ.trimmed && integ.trimmed.length) ? ` (trimmed ${integ.trimmed.length})` : ''}` +
    `${review ? ` | review parity=${review.parity_ok} resolved=${review.resolved} wired=${review.wired_live}` : ''}${verify ? ` | ui works=${verify.works}` : ''}`)
  return { key: b.key, title: b.title, landed, committed: integ && integ.committed, reverted: integ && integ.reverted, trimmed: integ && integ.trimmed, review: review && { parity_ok: review.parity_ok, resolved: review.resolved, wired_live: review.wired_live }, ui_works: verify && verify.works }
}

const BATCHES = [
  { key: 'f1', title: 'F1 format de-dup', commitType: 'fix', sub: 'format', oldPct: 93,
    cpp: 'src/format.cpp src/compose.cpp (the value-render path)', rust: 'src/format.rs src/compose.rs', refs: `${SPECS}/PORTING_format-render.md + ${UND}/format-render.md`,
    goal: `The port has TWO divergent copies of the value-render layer: the verified src/format.rs and a DUPLICATE inside src/compose.rs. The compose.rs copy rounds half-to-even (banker's) where Qt rounds half-AWAY-from-zero, mishandles the %g exponent carry, and drops the -0.0 sign. FIX: delete the compose.rs render duplicate and route the live editor's value rendering through the single verified format.rs (which must itself match Qt half-away / %g / -0.0). Net effect: one render layer, the three rounding/format bugs gone, no maintenance hazard. Output for already-correct cases must stay byte-identical.`,
    impl: `Carefully delete the compose.rs duplicate render functions and route their callers through format.rs (confirm format.rs already does half-away rounding, correct %g carry, and preserves -0.0; if not, fix format.rs too). Add GOLDEN tests comparing rendered output to the C++ format.cpp expected strings for the previously-divergent cases (half-away rounding, %g carry boundary, -0.0) AND a spread of normal values to prove no regression. This is a refactor touching a widely-used path — keep it tight; if a caller can't be cleanly rerouted, trim that piece and report it rather than reverting the whole batch.`,
    uiVerify: `open the sample and confirm float/double and number values render correctly in the editor (e.g. a value ending in .5 rounds away-from-zero, negative-zero shows a sign as C++ does) — spot-check a couple of rendered rows look right; no value-column regressions.` },

  { key: 't1', title: 'T1 themes default sync', commitType: 'fix', sub: 'themes', oldPct: 88,
    cpp: 'src/themes/theme.cpp src/themes/thememanager.cpp + the SHIPPED default theme JSON in the C++ tree (search the C++ repo for the built-in theme .json / embedded defaults)', rust: 'src/theme/model.rs src/theme/manager.rs src/ui/theme_apply.rs', refs: `${SPECS}/PORTING_themes.md + ${UND}/themes.md`,
    goal: `The port's theme defaults DIVERGE from the authoritative shipped C++ values: the heat-color derivation + the from_json marker fallbacks use stale spec values, and 6 of the 8 default 'selection' hexes differ from the shipped C++ JSON — and the fidelity TESTS currently LOCK the wrong values. FIX: find the SHIPPED C++ default theme JSON (the authoritative source of truth), re-derive the heat-color derivation + from_json fallbacks + all 8 default selection hexes to match it EXACTLY, and REWRITE the fidelity tests to assert the authoritative values. (The 9th built-in theme the port added is an additive extra — NOTE it but do not remove unless clearly required; focus on color authenticity.)`,
    impl: `Locate the shipped C++ theme JSON (grep the C++ repo for the default theme definitions / hex values). Update the Rust default-color derivation + from_json fallbacks + the 8 selection hexes to match byte-for-byte. REWRITE the fidelity tests to assert the authoritative shipped values (both: fix the code to emit them AND fix the tests to expect them, so the gate passes on correct values). Keep pretty-print/indent differences out of scope unless trivial.`,
    uiVerify: `switch among a couple of built-in themes and confirm the selection highlight + heat colors render (sanity that the new default hexes load and apply without breaking theme rendering).` },

  { key: 's1', title: 'S1 scanner UI-flow', commitType: 'feat', sub: 'scanner', oldPct: 93,
    cpp: 'src/scannerpanel.cpp src/scanner.cpp', rust: 'src/ui/scannerpanel.rs src/scanner.rs', refs: `${SPECS}/PORTING_scanner.md + ${UND}/scanner.md`,
    goal: `The scanner ENGINE is at parity; the gaps are UI-flow: (1) switching scan MODE does not apply the C++ smart-filter DEFAULTS (C++ sets sensible default filters/options per scan-type on mode switch); (2) most keyboard SHORTCUTS in the scanner panel are unported. FIX: apply the C++ smart-filter defaults on mode switch + port the remaining scanner-panel keyboard shortcuts, matching scannerpanel.cpp.`,
    impl: `Enumerate from scannerpanel.cpp the exact smart-filter defaults per scan mode + the keyboard shortcuts (key → action), and implement them in scannerpanel.rs. Add tests for the mode-switch-default mapping where unit-testable; wire the shortcuts into the panel's key handling. Keep cosmetic status/label strings out of scope.`,
    uiVerify: `open the scanner panel, switch scan modes, and confirm the smart-filter defaults change accordingly; exercise one or two of the newly-ported keyboard shortcuts and confirm they act.` },
]

const results = []
for (const b of BATCHES) { results.push(await runBatch(b)) }

// ── Re-audit touched subsystems ───────────────────────────────────────────────
phase('Re-audit')
const SUBMAP = {
  format: { title: 'Format / render', cpp: 'src/format.cpp src/compose.cpp', rust: 'src/format.rs src/compose.rs', refs: `${SPECS}/PORTING_format-render.md`, old: 93 },
  themes: { title: 'Themes + theme editor', cpp: 'src/themes/', rust: 'src/theme/', refs: `${SPECS}/PORTING_themes.md`, old: 88 },
  scanner: { title: 'Scanner + panel', cpp: 'src/scannerpanel.cpp src/scanner.cpp', rust: 'src/scanner.rs src/ui/scannerpanel.rs', refs: `${SPECS}/PORTING_scanner.md`, old: 93 },
}
const landedSubs = Array.from(new Set(results.filter((r) => r.landed).map((r) => r.sub || (BATCHES.find((b) => b.key === r.key) || {}).sub)))
  .map((k) => Object.assign({ key: k }, SUBMAP[k])).filter((s) => s.title)
const reaudits = landedSubs.length ? (await parallel(landedSubs.map((s) => () => safe(
  `POST-FIX RE-AUDIT (READ-ONLY) of "${s.title}" after the wf20 parity batch landed.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}; ref ${s.refs}. The wf18 report scored this subsystem ${s.old}%. The wf20 fix: ${JSON.stringify(results.filter((r) => (BATCHES.find((b) => b.key === r.key) || {}).sub === s.key && r.landed).map((r) => r.title))}. Confirm which prior gaps are RESOLVED, which remain still_open, and whether anything REGRESSED. Give the UPDATED parity_estimate.`,
  { label: `reaudit:${s.key}`, phase: 'Re-audit', schema: REAUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean) : []
log(`Re-audit: ${reaudits.map((r) => `${r.subsystem}=${r.parity_estimate}%`).join(' ')}`)

// ── Update the parity report + final gate ─────────────────────────────────────
phase('Report')
const report = await safe(
  `UPDATE the parity report after the wf20 follow-up batches.\n${COMMON}\n\n` +
  `Batches: ${JSON.stringify(results.map((r) => ({ title: r.title, sub: (BATCHES.find((b) => b.key === r.key) || {}).sub, landed: r.landed, commit: r.committed, reverted: r.reverted, trimmed: (r.trimmed || []).length, review: r.review })))}\n` +
  `Re-audit estimates: ${JSON.stringify(reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate, still_open: r.still_open })))}\n\n` +
  `CONTEXT: ${REPORT} currently shows overall ≈95% (wf19) with documented LOC weights (total 41,962; format w=950 @93, themes w=672 @88, scanner w=3605 @93). Only these three scores move; recompute Σ(parity×weight)/Σ(weight) with the SAME weights.\n` +
  `STEPS: (1) Confirm the full 7-step parity gate is GREEN at HEAD (run it) + tree clean; capture the new test counts. (2) Read ${REPORT}; UPDATE the per-subsystem rows for Format/Themes/Scanner to the new re-audited %, RECOMPUTE the overall weighted % (state old→new — be HONEST; these are low-to-mid-weight subsystems so the overall move is modest, likely ~95→~96), update the overall section + roadmap (mark these batches DONE), and ADD a "## wf20 follow-up" section listing the new commit hashes + what each fixed + new test counts + old→new overall. Keep the rest intact. (3) \`cd ${OUT} && git add ${REPORT} && git commit -m "docs(parity): wf20 — format/themes/scanner, overall <old>%→<new>%"\`. (4) Return the structured summary. Honest + quantitative.`,
  { label: 'report', phase: 'Report', schema: { type: 'object', additionalProperties: false, properties: {
    gate_green: { type: 'boolean' }, test_counts: { type: 'string' },
    old_overall_pct: { type: 'integer' }, new_overall_pct: { type: 'integer' },
    format_pct: { type: 'integer' }, themes_pct: { type: 'integer' }, scanner_pct: { type: 'integer' },
    landed: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } },
    committed: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['gate_green', 'new_overall_pct', 'assessment'] } })

return {
  batches: results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted })),
  reaudit: reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate })),
  report,
}
