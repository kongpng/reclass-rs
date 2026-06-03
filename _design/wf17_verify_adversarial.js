export const meta = {
  name: 'reclass-plugin-wireup-adversarial-verify',
  description: 'Independently + adversarially verify the wf17 plugin wireup (HEAD 7c86e21, commits f2e33e7/750a99a/b3887b5/7c86e21). Do NOT trust the implementing workflow self-report. Three independent checks run, then synthesize a verdict: (1) re-run the FULL 7-step parity gate from scratch and report the real numbers; (2) a fan-out of REFUTERS that each try to DISPROVE one live-wiring claim by tracing a non-#[cfg(test)] call path from a real UI/startup entry to the manager method (default to refuted if the wire is test-only, no-ops, or breaks default-build parity); (3) re-capture on-screen evidence the final step skipped — default launch (Plugins menu + Manage-Plugins dialog + source lists, and that the default UI is byte-unchanged with the demo env UNSET) and RECLASS_DEMO_PLUGIN=1 launch (demo command/panel/dialog Elm loop). The synthesis flags any refuted claim, red gate step, or missing screenshot as a FAILURE — the goal is to catch over-optimistic reporting, not confirm it.',
  whenToUse: 'Adversarially verify a completed wireup before reporting it as done.',
  phases: [
    { title: 'Gate + Reachability' },
    { title: 'On-screen' },
    { title: 'Verdict' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const REF = `${OUT}/_design/plugin_system_cpp_reference.md`
const QT = '/home/loke/Documents/Reclass'

const GATE = `Run ALL 7 from ${OUT} and report each with its real number (do not stop early on a pass; if one fails, still run the rest):
  1) default/ui build:  \`${LP} cargo build 2>&1 | grep -E '^error|Finished' | tail -3\`
  2) headless build:    \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep -E '^error|Finished' | tail -3\`
  3) plugins build:     \`${LP} cargo build --features plugins 2>&1 | grep -E '^error|Finished' | tail -3\`
  4) full tests:        \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`
  5) ui tests:          \`${LP} cargo test --lib "ui::" 2>&1 | grep 'test result' | tail -1\`
  6) plugins tests:     \`${LP} cargo test --features plugins --lib 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`
  7) bare tests:        \`cargo test --no-default-features 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`
Also confirm \`git status --short\` is clean and \`git rev-parse --short HEAD\` == 7c86e21.`

const HARNESS = `Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/advX.png\`; crop with \`magick\` + Read. *** SCALE TRAP (the #1 cause of FALSE NEGATIVES): SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot\`); REAL click coords = screenshot-px × SCALE. Before trusting ANY negative, click a KNOWN element + re-shot to confirm the scale. *** For the demo path relaunch with \`RECLASS_DEMO_PLUGIN=1 ./scripts/ui.sh start /tmp/parity/png.rcx\`. \`./scripts/ui.sh stop\` when done. You MUST return real screenshot paths in shots[] — an empty shots[] is a verification FAILURE.`

const safe = async (prompt, opts) => {
  try { return await agent(prompt, opts) } catch (e) {
    log(`[soft-fail] ${(opts && opts.label) || '?'}: ${String(e).slice(0, 140)}`); return null
  }
}

const GATE_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  green: { type: 'boolean' }, head_ok: { type: 'boolean' }, tree_clean: { type: 'boolean' },
  steps: { type: 'array', items: { type: 'string' } }, failing: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' },
}, required: ['green', 'steps', 'detail'] }

const CLAIM_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  claim: { type: 'string' },
  verdict: { type: 'string', enum: ['confirmed', 'refuted', 'partial'] },
  test_only: { type: 'boolean', description: 'true if the wire is only reachable from #[cfg(test)]' },
  parity_ok: { type: 'boolean', description: 'true if the default build (env unset / plugins off) is unaffected' },
  call_path: { type: 'string', description: 'the concrete non-test path UI-entry → manager method, with file:line' },
  evidence: { type: 'string' },
}, required: ['claim', 'verdict', 'parity_ok', 'call_path', 'evidence'] }

const ONSCREEN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  works: { type: 'boolean' }, parity_default_unchanged: { type: 'boolean' },
  shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' },
}, required: ['works', 'parity_default_unchanged', 'shots', 'detail'] }

// ── Phase 1: Gate (1 agent) + adversarial reachability refuters (fan-out) ─────
phase('Gate + Reachability')

const CLAIMS = [
  { key: 'c1', t: 'F1a: MainWindow owns ONE session PluginManager (a field, built once in new()) and BOTH the process picker AND the Manage-Plugins dialog read THAT manager\'s registry/plugins_view — NOT a fresh PluginManager::with_builtins() throwaway.',
    hint: 'Hinted seams: plugin_manager field ~window.rs:340, built ~window.rs:884. REFUTE by finding any remaining live (non-test) PluginManager::with_builtins() that feeds a picker or the dialog, or by showing the two surfaces read different registries.' },
  { key: 'c2', t: 'F1b: the dialog Enable/Disable toggle is a LIVE control that calls set_enabled(id, !enabled, persist=true) writing through a DiskSettings-backed DiskPluginPersistence, so the flip SURVIVES closing+reopening the dialog and the app.',
    hint: 'REFUTE by showing the toggle is static text, no-ops, uses persist=false, or persistence is MemPluginPersistence/not wired to settings.json.' },
  { key: 'c3', t: 'F2a: the dialog Unload action (non-builtin rows only) calls PluginManager::safe_unload via the live LivePluginHost, and detach_documents_using runs BEFORE the plugin is dropped (the dangling-provider fix); built-ins are NOT unloadable.',
    hint: 'Seams: src/ui/pluginhost.rs LivePluginHost; DocumentArea::detach_sources_of_kind in src/ui/tabs.rs; dialog events ~window.rs:2476-2532. REFUTE by showing it is test-only, detach-after-drop, or built-ins unloadable.' },
  { key: 'c4', t: 'F2b: the "Load plugin…" picker is wired BEHIND #[cfg(feature="plugins")] to PluginManager::load_native_plugin on the session manager, and is ABSENT from the default build.',
    hint: 'Seam: load_plugin_from_path ~window.rs:2539. REFUTE by showing it is not feature-gated (parity break), not reachable, or not on the session manager.' },
  { key: 'c5', t: 'F3: a live Plugins menu enumerates contributed Commands and mounts Panel/Dialog via render_view_tree with the Elm loop (handle_ui_event → fresh ViewTree → set_tree); the DEMO plugin is loaded ONLY when RECLASS_DEMO_PLUGIN=1, and with the env UNSET the default UI shows NOTHING new (no Plugins-menu demo items, no demo dock).',
    hint: 'Seams: manager UiContribution enumerator (src/plugin/manager.rs); window mount/route + LivePluginHost request-collection (~window.rs, pluginhost.rs). REFUTE by showing the menu/panel/dialog is still never instantiated live, the Elm loop does not route, or the demo leaks into the default (env-unset) UI — a parity break.' },
  { key: 'c6', t: 'F4: startup folder-scan discovery runs in MainWindow::new under #[cfg(feature="plugins")] (load_native_plugins_from_default_dirs) and surfaces load_errors() in the dialog; the DEFAULT build (feature off) is a no-op.',
    hint: 'Seam: discovery ~window.rs:899-915. REFUTE by showing it runs in the default build (parity break) or is test-only / does not feed the session manager + dialog.' },
]

const gateP = safe(`Independently RE-RUN the full parity gate at HEAD (do not trust any prior report).\n${GATE}\nReturn green (all 7 pass + tree clean + HEAD==7c86e21), the per-step results, any failing steps, and detail.`,
  { label: 'gate', phase: 'Gate + Reachability', schema: GATE_SCHEMA })

const refutersP = parallel(CLAIMS.map((c) => () =>
  safe(`ADVERSARIAL REFUTER (READ-ONLY). Your job is to DISPROVE this claim about the wf17 plugin wireup in ${OUT}; default to verdict="refuted" unless you can trace a concrete, non-#[cfg(test)] call path that proves it. C++ truth: ${REF}, ${QT}/src/pluginmanager.cpp.\n\nCLAIM: ${c.t}\n\n${c.hint}\n\nRead the actual code (get the real current line numbers — the hints may drift). Produce: verdict (confirmed only if you found the live path and could not refute it; partial if reachable but incomplete/with a caveat; refuted if test-only/no-op/parity-breaking), test_only, parity_ok, the exact call_path (UI entry → … → manager method, file:line), and evidence. Be skeptical and specific.`,
    { label: `refute:${c.key}`, phase: 'Gate + Reachability', schema: CLAIM_SCHEMA, agentType: 'Explore' })))

const [gate, refuters0] = await Promise.all([gateP, refutersP])
const refuters = (refuters0 || []).filter(Boolean)
log(`Gate green=${gate && gate.green}. Refuters: ${refuters.map((r) => `${r.claim.slice(0, 4)}=${r.verdict}`).join(' ')}`)

// ── Phase 2: On-screen re-capture (the final step skipped this) ───────────────
phase('On-screen')
const onscreen = await safe(
  `RE-CAPTURE on-screen evidence (the implementing workflow's final step returned NO screenshots — that gap is what you are closing).\n${HARNESS}\n\nDo BOTH launches and capture cropped shots for each:\n` +
  `A) DEFAULT launch (env UNSET): (1) the main window looks normal; (2) open the Plugins menu / Manage Plugins dialog — built-ins (file/buffer/snapshot/null) enumerate with metadata + permission rows + Enable/Disable + (non-builtin) Unload controls; toggle one row's Enable/Disable, CLOSE + REOPEN the dialog, confirm the state STUCK; (3) confirm NO demo menu item and NO demo dock exist (default parity). \n` +
  `B) RECLASS_DEMO_PLUGIN=1 launch: the Plugins menu shows the demo Command (run it), the demo Panel renders in a dock (click a button → it updates: the Elm loop), the demo Dialog opens + submit routes back.\n` +
  `Return works (the live actions function), parity_default_unchanged (A.3 holds), shots[] (REAL paths — empty = failure), and detail. Use the SCALE rule; verify scale before trusting a negative. Stop the app when done.`,
  { label: 'onscreen', phase: 'On-screen', schema: ONSCREEN_SCHEMA })

// ── Phase 3: Verdict ──────────────────────────────────────────────────────────
phase('Verdict')
const verdict = await safe(
  `SYNTHESIZE the adversarial verification verdict. Be a skeptic: any refuted claim, any red/failing gate step, a dirty tree, wrong HEAD, a parity break (parity_ok=false or parity_default_unchanged=false), or an empty shots[] is a FAILURE that must be called out — do not paper over it.\n\n` +
  `Gate: ${JSON.stringify(gate)}\n\nReachability refuters: ${JSON.stringify(refuters.map((r) => ({ claim: r.claim.slice(0, 40), verdict: r.verdict, test_only: r.test_only, parity_ok: r.parity_ok, path: (r.call_path || '').slice(0, 200), ev: (r.evidence || '').slice(0, 200) })))}\n\nOn-screen: ${JSON.stringify(onscreen)}\n\n` +
  `Return overall_pass (true ONLY if gate green + every claim confirmed/partial-with-no-parity-break + parity intact + real screenshots captured), a per-claim summary, the concrete failures/caveats, and a recommendation (ship-as-is / fix-X-first). Be precise about what is genuinely live vs still test-only/stubbed.`,
  { label: 'verdict', phase: 'Verdict', schema: { type: 'object', additionalProperties: false, properties: {
    overall_pass: { type: 'boolean' }, gate_green: { type: 'boolean' },
    claims: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { claim: { type: 'string' }, verdict: { type: 'string' }, note: { type: 'string' } }, required: ['claim', 'verdict'] } },
    failures: { type: 'array', items: { type: 'string' } }, caveats: { type: 'array', items: { type: 'string' } },
    genuinely_live: { type: 'array', items: { type: 'string' } }, still_test_only_or_stub: { type: 'array', items: { type: 'string' } },
    recommendation: { type: 'string' }, assessment: { type: 'string' },
  }, required: ['overall_pass', 'gate_green', 'recommendation', 'assessment'] } })

return { gate: gate && { green: gate.green, head_ok: gate.head_ok, tree_clean: gate.tree_clean },
  refuters: refuters.map((r) => ({ claim: r.claim.slice(0, 50), verdict: r.verdict, parity_ok: r.parity_ok })),
  onscreen: onscreen && { works: onscreen.works, parity: onscreen.parity_default_unchanged, shots: (onscreen.shots || []).length },
  verdict }
