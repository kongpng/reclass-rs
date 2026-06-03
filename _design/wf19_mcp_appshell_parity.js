export const meta = {
  name: 'reclass-mcp-appshell-parity',
  description: 'Follow-up to wf18: land the two biggest remaining parity levers identified in _design/PARITY_REPORT.md, moving the weighted overall from ~93% toward ~97%. Sequential, parity-GATED batches (each: design → implement → 7-step gate → commit-or-revert → review; UI batches also verify on-screen with the SCALE rule), STARTING with the data-loss-class window-close unsaved-changes guard. Batches: A1 app-shell window-close routes through the unsaved-changes guard (X/Alt+F4/titlebar); A2 app-shell behavior (open_project replace-all, XML byte-sniff, show_icon consumed, real recents age_days); M1a MCP tool surface (six evidence.* tools + tree.export_header + remove the 5 phantom tools/list entries + parity tests); M1b MCP behavior (initialize evidence paragraph, project.state evidence summary, tree.apply change_comment, McpBridge notify_evidence_changed + URI_EVIDENCE). Then re-audit MCP + app-shell and UPDATE _design/PARITY_REPORT.md with the new commits, recomputed parity %, and new test counts. ALL agents pinned to Opus 4.8. The existing suite MUST stay green throughout (commit-or-revert); the live-memory/CLR/native-runtime/subprocess/sandbox stubs stay excluded.',
  whenToUse: 'Land the MCP + app-shell parity batches from the wf18 report.',
  phases: [
    { title: 'A1 window-close guard' },
    { title: 'A2 app-shell behavior' },
    { title: 'M1a MCP tool surface' },
    { title: 'M1b MCP behavior' },
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
If ALL pass (new tests included): \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` → green=true, committed=<hash>. If a piece can't be made green, prefer trimming to a working+tested subset over reverting the whole batch; commit the green subset and report what was trimmed.
If the batch fundamentally can't integrate: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` → green=false, reverted=true + notes. NEVER touch target/ or .git config.`

const COMMON = `Rust port: ${OUT}. Authoritative C++ original: ${QT}. Reference: per-subsystem spec ${SPECS}/PORTING_<area>.md + behavior doc ${UND}/<area>.md + the wf18 findings in ${REPORT}. Match the C++ behavior PRECISELY (read the C++ source for exact semantics; the docs are guides). Aesthetic = Zed (src/ui/design.rs). Keep the default build's deps unchanged; add #[cfg(test)] tests for new logic and port the relevant C++ test assertions. Do NOT regress existing behavior or touch unrelated modules/stubs. Do NOT commit in design/impl/review steps (integrate owns commits/reverts).`

const HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/p19_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP (the #1 false-negative cause): SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm scale. *** \`./scripts/ui.sh stop\` when done.`

// Pin every agent to Opus 4.8 + retry transient failures (chiefly server-side
// rate limiting that truncates a turn before StructuredOutput). All calls route
// through safe(). This batch's fan-out is small, so throttle risk is low.
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
  steps: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { file: { type: 'string' }, action: { type: 'string', enum: ['create', 'edit'] }, what: { type: 'string' } }, required: ['file', 'action', 'what'] } },
  cpp_behavior: { type: 'string', description: 'the exact C++ behavior to match, with file:line' },
  specifics: { type: 'string', description: 'concrete enumerated items discovered (e.g. the exact evidence.* tool names, the 5 phantom tools)' },
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
    `DESIGN STEP for parity batch "${b.title}".\n${COMMON}\n\nC++ source to study: ${b.cpp}\nRust file(s) to change: ${b.rust}\nReference: ${b.refs}\n\nGoal:\n${b.goal}\n\nRead the C++ source (exact semantics + file:line), then the current Rust, and produce an ordered plan: exact files/edits, the key signatures/hooks, and ENUMERATE any specifics you discover (e.g. the exact tool names to add, the exact phantom tools to remove, the exact C++ close-event guard path). Keep increments small + compiling.`,
    { label: `design:${b.key}`, phase: b.title, schema: PLAN_SCHEMA })

  const impl = await safe(
    `IMPLEMENT parity batch "${b.title}" on the real tree (${OUT}).\n${COMMON}\n\nPlan: ${JSON.stringify((plan && plan.steps) || []).slice(0, 3500)}\nC++ behavior to match: ${plan ? (plan.cpp_behavior || '').slice(0, 800) : ''}\nSpecifics: ${plan ? (plan.specifics || '').slice(0, 800) : ''}\n\n${b.impl}\n\nWrite real, compiling Rust + tests (port the relevant C++ test assertions). Verify your files compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`; if you touched mcp/headless code also \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -3\`). Do NOT commit. Return changed files + compiles + deferred + notes.`,
    { label: `impl:${b.key}`, phase: b.title, schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE parity batch "${b.title}". Make the WHOLE project green + commit, or trim/revert.\n${GATE}\n\nImpl notes: ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}. Commit message: "${b.commitType}(parity): wf19 ${b.title} — <what changed>".`,
    { label: `integrate:${b.key}`, phase: b.title, schema: INTEG_SCHEMA })

  const landed = integ && integ.green && !integ.reverted && integ.committed && integ.committed !== '(no-op)'
  let verify = null
  if (b.uiVerify && landed) {
    verify = await safe(
      `ON-SCREEN VERIFY of parity batch "${b.title}".\n${HARNESS}\n\nCheck: ${b.uiVerify}\nConfirm no parity regression elsewhere on screen. Return works + shots + detail.`,
      { label: `verify:${b.key}`, phase: b.title, schema: VERIFY_SCHEMA })
  }

  const review = await safe(
    `ADVERSARIAL REVIEW (READ-ONLY) of parity batch "${b.title}" at HEAD (\`cd ${OUT} && git show --stat HEAD\` + read the diff). C++ truth: ${b.cpp}; ref ${b.refs}. Check: (1) parity preserved (no existing behavior regressed, default deps unchanged); (2) the goal is GENUINELY resolved + matches the C++ behavior (not a superficial patch); (3) for UI/behavior, that it is REACHABLE from the live app (trace the call path), not just tests; (4) correctness issues / silent no-ops. Return parity_ok, resolved, wired_live, issues, assessment.`,
    { label: `review:${b.key}`, phase: b.title, schema: REVIEW_SCHEMA })

  log(`${b.title}: ${integ && integ.reverted ? 'REVERTED' : (landed ? 'LANDED @' + integ.committed : (integ && integ.green ? 'no-op' : 'UNCLEAR'))}` +
    `${(integ && integ.trimmed && integ.trimmed.length) ? ` (trimmed ${integ.trimmed.length})` : ''}` +
    `${review ? ` | review parity=${review.parity_ok} resolved=${review.resolved} wired=${review.wired_live}` : ''}${verify ? ` | ui works=${verify.works}` : ''}`)
  return { key: b.key, title: b.title, landed, committed: integ && integ.committed, reverted: integ && integ.reverted, trimmed: integ && integ.trimmed, review: review && { parity_ok: review.parity_ok, resolved: review.resolved, wired_live: review.wired_live }, ui_works: verify && verify.works }
}

const BATCHES = [
  { key: 'a1', title: 'A1 window-close guard', commitType: 'fix',
    cpp: 'src/main.cpp (closeEvent / the unsaved-changes/maybeSave guard) + src/mainwindow.h', rust: 'src/ui/window.rs (+ titlebar.rs close control)', refs: `${SPECS}/PORTING_app-shell.md + ${UND}/app-shell.md`,
    goal: `DATA-LOSS FIX (top priority): in C++, closing the main window (the X / Alt+F4 / titlebar close) routes through the SAME unsaved-changes guard as File ▸ Exit (prompt to save/discard/cancel; cancel aborts the close). In the Rust port the menu/quit path is guarded but the WINDOW-CLOSE path bypasses it. Wire the gpui window-close interception (find the on_should_close / window-close hook gpui exposes, and the in-app custom titlebar close button in titlebar.rs) so EVERY close path runs the existing unsaved-changes guard, and cancelling KEEPS the window open. Match the C++ prompt semantics (save / discard / cancel) for the dirty-document set across all open tabs.`,
    impl: `Find the existing unsaved-changes guard used by File ▸ Exit in window.rs and the gpui window-close hook; route the OS/window close + the titlebar close button through it; abort the close on Cancel. Add a test for the guard decision logic (dirty → prompt, cancel → not closed, discard → closed) at whatever seam is unit-testable.`,
    uiVerify: `dirty a node (edit a value so the document is modified), then click the in-app titlebar close control (and/or trigger window close) — confirm the unsaved-changes guard PROMPT appears; click Cancel/Keep-editing and confirm the window stays open + the doc is intact. Do NOT confirm a discard that would close the app.` },

  { key: 'a2', title: 'A2 app-shell behavior', commitType: 'fix',
    cpp: 'src/main.cpp (openProject, recent-files, file-type sniffing, show_icon usage) + src/startpage.h + src/workspace_model.h', rust: 'src/ui/window.rs src/ui/startpage.rs src/ui/workspace.rs src/ui/tabs.rs', refs: `${SPECS}/PORTING_app-shell.md + ${UND}/app-shell.md`,
    goal: `Four app-shell behavior gaps from the wf18 report: (1) open_project should REPLACE-ALL into a fresh tab the way C++ does (the port loads into the active tab) — match the C++ openProject semantics exactly; (2) route a project file to the XML importer by BYTE-SNIFFING the first bytes (C++ sniffs for the XML/ReClass signature) instead of the file extension; (3) the persisted show_icon setting is currently DEAD — consume it where C++ applies it (tab/source icon visibility); (4) recent-files "age" collapses to "Today" (age_days=0) — compute the REAL age_days from the file timestamp like C++ so recents group correctly.`,
    impl: `Match each C++ behavior precisely (read main.cpp). Add tests: open_project replace-all bookkeeping; xml byte-sniff vs extension; show_icon honored; age_days computed from a known timestamp. Keep cosmetic strings/dock-overlay (intentional gpui substitutions) out of scope.`,
    uiVerify: `open a project from the start page / File menu and confirm it REPLACES rather than stacking onto the active tab; confirm the recents list shows real relative ages (not everything "Today"); if show_icon is toggled in options, confirm tab/source icons reflect it.` },

  { key: 'm1a', title: 'M1a MCP tool surface', commitType: 'feat',
    cpp: 'src/mcp/mcp_bridge.cpp src/mcp/mcp_bridge.h', rust: 'src/mcp/tools.rs src/mcp/schemas.rs src/mcp/dispatch.rs src/mcp/mod.rs', refs: `${SPECS}/PORTING_mcp.md + ${UND}/mcp.md`,
    goal: `MCP tool-surface parity (the single biggest lever, 78→~90): (1) implement the SIX evidence.* tools the C++ bridge exposes (enumerate their EXACT names + schemas + behavior from mcp_bridge.cpp — the evidence model is already ported to the Rust core, so these wrap existing core APIs); (2) implement tree.export_header (export the header/struct text — wire to the existing generator); (3) REMOVE the 5 phantom tools that tools/list currently advertises but does not implement (identify them by diffing advertised vs dispatched); (4) update the tools/list parity tests to assert the REAL, C++-matching tool set.`,
    impl: `Read mcp_bridge.cpp to get the exact evidence.* tool names/params/results + tree.export_header. Add them to tools.rs/schemas.rs/dispatch.rs wired to the ported core. Remove phantom advertisements. Update/extend the tools/list parity test to the authoritative set. Keep the JSON-RPC wire shape identical to C++.` },

  { key: 'm1b', title: 'M1b MCP behavior', commitType: 'feat',
    cpp: 'src/mcp/mcp_bridge.cpp src/mcp/mcp_bridge.h', rust: 'src/mcp/host.rs src/mcp/tools.rs src/mcp/dispatch.rs src/mcp/wire.rs src/mcp/mod.rs', refs: `${SPECS}/PORTING_mcp.md + ${UND}/mcp.md`,
    goal: `MCP behavioral parity (90→~95): (1) initialize instructions must include the EVIDENCE paragraph C++ emits; (2) project.state must include the evidence SUMMARY C++ includes; (3) tree.apply must accept the change_comment op/parameter C++ supports; (4) McpBridge must expose notify_evidence_changed + the URI_EVIDENCE resource URI (the evidence-changed notification + resource) as C++ does. Match the exact strings/shapes from mcp_bridge.cpp.`,
    impl: `Read mcp_bridge.cpp for the exact initialize instructions text, project.state evidence summary shape, tree.apply change_comment semantics, and the notify_evidence_changed + URI_EVIDENCE wiring. Implement to match byte-for-byte where the C++ emits fixed strings. Add tests for each (initialize contains the paragraph; project.state contains the summary; tree.apply change_comment round-trips; the evidence resource/notification is registered).` },
]

const results = []
for (const b of BATCHES) { results.push(await runBatch(b)) }
const landedSubs = []
if (results.some((r) => (r.key === 'a1' || r.key === 'a2') && r.landed)) landedSubs.push({ key: 'appshell', title: 'App shell', cpp: 'src/main.cpp src/mainwindow.h src/startpage.h', rust: 'src/ui/window.rs src/ui/startpage.rs', refs: `${SPECS}/PORTING_app-shell.md` })
if (results.some((r) => (r.key === 'm1a' || r.key === 'm1b') && r.landed)) landedSubs.push({ key: 'mcp', title: 'MCP bridge', cpp: 'src/mcp/mcp_bridge.cpp', rust: 'src/mcp/', refs: `${SPECS}/PORTING_mcp.md` })

// ── Re-audit the touched subsystems ───────────────────────────────────────────
phase('Re-audit')
const reaudits = landedSubs.length ? (await parallel(landedSubs.map((s) => () => safe(
  `POST-FIX RE-AUDIT (READ-ONLY) of "${s.title}" after wf19 parity batches landed.\n${COMMON}\n\nC++: ${s.cpp}; Rust: ${s.rust}; ref ${s.refs}. The wf18 report scored this subsystem ${s.key === 'mcp' ? '78' : '86'}%. The wf19 fixes: ${JSON.stringify(results.filter((r) => (s.key === 'appshell' ? (r.key === 'a1' || r.key === 'a2') : (r.key === 'm1a' || r.key === 'm1b')) && r.landed).map((r) => r.title))}. Confirm which prior wf18 gaps are RESOLVED, which remain still_open, and whether anything REGRESSED. Give the UPDATED parity_estimate.`,
  { label: `reaudit:${s.key}`, phase: 'Re-audit', schema: REAUDIT_SCHEMA, agentType: 'Explore' })))).filter(Boolean) : []
log(`Re-audit: ${reaudits.map((r) => `${r.subsystem}=${r.parity_estimate}%`).join(' ')}`)

// ── Update the parity report + final gate ─────────────────────────────────────
phase('Report')
const report = await safe(
  `UPDATE the parity report after the wf19 follow-up batches.\n${COMMON}\n\n` +
  `Batches: ${JSON.stringify(results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted, trimmed: (r.trimmed || []).length, review: r.review })))}\n` +
  `Re-audit estimates: ${JSON.stringify(reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate, still_open: r.still_open })))}\n\n` +
  `STEPS: (1) Confirm the full 7-step parity gate is GREEN at HEAD (run it) + tree clean; capture the new test counts. (2) Read the existing ${REPORT}; UPDATE the per-subsystem table rows for MCP + App shell to the new re-audited parity %, RECOMPUTE the overall weighted parity % with the SAME weighting method already documented in the report (state old→new), and ADD a "## wf19 follow-up" section listing the new commit hashes + what each fixed + the new test counts + the old→new overall %. Keep the rest of the report intact. (3) \`cd ${OUT} && git add ${REPORT} && git commit -m "docs(parity): wf19 — MCP + app-shell batches, overall <old>%→<new>%"\`. (4) Return the structured summary. Be honest + quantitative.`,
  { label: 'report', phase: 'Report', schema: { type: 'object', additionalProperties: false, properties: {
    gate_green: { type: 'boolean' }, test_counts: { type: 'string' },
    old_overall_pct: { type: 'integer' }, new_overall_pct: { type: 'integer' },
    mcp_pct: { type: 'integer' }, appshell_pct: { type: 'integer' },
    landed: { type: 'array', items: { type: 'string' } }, still_open: { type: 'array', items: { type: 'string' } },
    committed: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['gate_green', 'new_overall_pct', 'assessment'] } })

return {
  batches: results.map((r) => ({ title: r.title, landed: r.landed, commit: r.committed, reverted: r.reverted })),
  reaudit: reaudits.map((r) => ({ sub: r.subsystem, parity: r.parity_estimate })),
  report,
}
