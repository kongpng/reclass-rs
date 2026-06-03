export const meta = {
  name: 'reclass-plugin-verify-wireup',
  description: 'Verify the landed plugin system (P1–P6) against its design + the verified C++ behavior, then wire up the integration that exists only in tests so it works in the LIVE app — without breaking parity. Today the live app builds a THROWAWAY PluginManager::with_builtins() in three places (process picker, Manage-Plugins dialog, source chooser), so enable/disable cannot stick and the dialog drives nothing; the demo Panel/Dialog/Command + the ViewTree renderer are never instantiated live; enabled-state persistence + safe-unload + load-from-path + startup discovery have no live caller. This workflow: (1) parallel read-only AUDIT of every plugin seam vs design + C++; (2) synthesize a classified fix-list; (3) sequential, parity-GATED fix phases that each commit-or-revert — F1 session-owned manager + DiskSettings persistence + live enable/disable, F2 live PluginHost + safe-unload + (feature-gated) load-from-path, F3 live declarative UI (Command/Panel/Dialog Elm loop, demo behind an env affordance so default UI is byte-for-byte unchanged), F4 (feature-gated) startup folder-scan discovery; (4) on-screen verify; (5) full-gate + honest report. The DEFAULT app must stay byte-for-byte identical when no plugin UI is active; managed-C# (P5) stays a Windows-only scaffold; the live-memory reader stays a deliberate stub.',
  whenToUse: 'Verify + finish wiring the plugin subsystem into the live app, parity always preserved.',
  phases: [
    { title: 'Audit' },
    { title: 'Plan' },
    { title: 'F1 session manager + persistence + enable/disable' },
    { title: 'F2 live host + safe-unload + load-from-path' },
    { title: 'F3 live declarative UI (command/panel/dialog)' },
    { title: 'F4 startup discovery (feature-gated)' },
    { title: 'Verify' },
    { title: 'Final' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const DESIGN = `${OUT}/_design/plugin_system_design.md`
const REF = `${OUT}/_design/plugin_system_cpp_reference.md`
const QT = '/home/loke/Documents/Reclass'

// Extended parity gate. Adds a `--features plugins` build + test (confirmed to
// compile on this Linux host) so the feature-gated wireups F2/F4 add are actually
// compiled and checked — the prior gate never built that feature.
const PARITY = `*** PARITY GATE — the EXISTING app + its FULL test suite MUST stay green; this is verification+wireup, NOT a rewrite. ***
Run ALL of these from ${OUT} and require success:
  1) default/ui build:  \`${LP} cargo build 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished, 0 errors
  2) headless build:    \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  3) plugins build:     \`${LP} cargo build --features plugins 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished (compiles the feature-gated load-from-path / discovery / reclassnet code)
  4) full tests:        \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  5) ui tests:          \`${LP} cargo test --lib "ui::" 2>&1 | grep 'test result' | tail -1\`  → 0 failed
  6) plugins tests:     \`${LP} cargo test --features plugins --lib 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  7) bare tests:        \`cargo test --no-default-features 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
If ALL pass (with your new tests included): \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` and report green=true, committed=<short-hash>. If this phase legitimately changed nothing (already correct), report green=true, committed="(no-op)", reverted=false.
If you CANNOT reach green after reasonable fixing effort: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` to REVERT THIS PHASE ENTIRELY (restores the prior green HEAD), and report green=false, reverted=true with notes on what blocked it. NEVER leave the tree red or commit a red tree. NEVER touch target/ or .git config.`

const COMMON = `Authoritative spec: ${DESIGN} (READ the relevant section first). Verified C++ behavior: ${REF}. C++ source: ${QT} (esp. src/pluginmanager.cpp, src/iplugin.h). Aesthetic for any UI = Zed (src/ui/design.rs tokens). HARD PARITY RULE: the existing app must look + behave byte-for-byte identically when the user is NOT actively using a plugin; plugin-contributed UI appears ONLY when a contributing plugin is loaded; the demo plugin must NOT alter the default shipping UI. Keep abi_stable/libloading/windows behind the \`plugins\` feature; do not pull them into the default build. Add #[cfg(test)] tests for new logic. Do NOT modify unrelated modules. Do NOT run git commit in design/impl/review steps (the integrate step owns commits/reverts).`

// ── Ground truth handed to the agents so they don't re-derive it ──────────────
const FACTS = `VERIFIED CURRENT STATE (cite/confirm, don't blindly trust):
- NO session-owned PluginManager. Three LIVE sites each build a THROWAWAY \`PluginManager::with_builtins()\`:
  (a) src/ui/window.rs ~1520: process picker — \`PluginManager::with_builtins(); ProcessPickerModel::from_registry(mgr.registry())\`.
  (b) src/ui/window.rs ~2056 + ~5479: Manage-Plugins dialog renders \`with_builtins().plugins_view()\` (fn builtin_plugins()).
  (c) src/ui/sourcechooser.rs: \`provider_entries_from_registry(registry)\`; a comment at ~460 literally says "even before the host wires its real PluginManager registry in".
  ⇒ enable/disable can't persist within a session and the dialog drives nothing. F1 unifies these onto ONE owned manager whose registry IS what the pickers read.
- \`ProviderRegistry\` (src/provider/registry.rs) is a PLAIN OWNED struct (NOT a global singleton); each PluginManager owns one. It already has set_enabled/enabled_providers/unregister.
- \`PluginManager\` (src/plugin/manager.rs) ALREADY HAS the logic, called only by tests: set_enabled(id,enabled,persist), safe_unload(id,&mut host), plugins_view()->Vec<PluginRow>, set_persistence(Box<dyn PluginPersistence>), handle_command/handle_ui_event/handle_dialog_closed/view_tree, and (feature=plugins) load_native_plugin/load_native_plugins_from_default_dirs/load_native_plugins_from_dirs + load_errors().
- \`PluginPersistence\` trait + in-memory \`MemPluginPersistence\` exist (manager.rs ~40-87). The real settings store is \`DiskSettings\` (src/ui/window.rs ~78-172, ~/.config/Reclass/settings.json) used for recentFiles/font/etc. No DiskSettings-backed PluginPersistence exists yet.
- The Manage-Plugins dialog (src/ui/window.rs ~5514 PluginManagerDialog) is READ-ONLY: the only interactive control is Close (~5645) + Esc; it even prints "Loading runtime plugins (DLL/SO) is not supported in this build" (~5629).
- The declarative UI host is built but NEVER instantiated live: src/ui/pluginview.rs render_view_tree(), src/ui/pluginpanel.rs PluginPanel, src/ui/plugindialog.rs PluginDialog are only used inside their own modules' tests. The DemoPlugin (src/plugin/demo.rs: Commands demo.ping/demo.open_target, Panel demo.panel@Right, Dialog demo.target) is only constructed in tests; with_builtins_and_demo() has no live caller.
- Discovery (src/plugin/discovery.rs default_plugin_dirs/load_from_dirs) has no live caller; native loading is feature-gated (plugins) OFF by default. \`--features plugins\` BUILDS CLEAN on this Linux host (windows dep is optional + target-gated).
- Only MockPluginHost exists (src/plugin/host.rs); no window-backed live PluginHost.
- INTENDED-DEFERRED (do NOT "fix"): P5 managed-C# CLR is a Windows-only #[cfg(windows)] scaffold; the live-memory OS reader is a deliberate stub; the P7 subprocess loader; permissions are disclosure-only.`

// ── Schemas ───────────────────────────────────────────────────────────────────
const AUDIT_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  area: { type: 'string' },
  reality: { type: 'string', description: 'what is actually wired-live vs test-only vs absent' },
  gaps: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
    gap: { type: 'string' }, target_file: { type: 'string' },
    classify: { type: 'string', enum: ['fix-now', 'fix-under-feature', 'intended-deferred', 'already-ok'] },
    parity_risk: { type: 'string' },
  }, required: ['gap', 'target_file', 'classify'] } },
  cpp_parity_notes: { type: 'string' },
  recommendation: { type: 'string' },
}, required: ['area', 'reality', 'gaps', 'recommendation'] }

const FIXPLAN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  confirmed_gaps: { type: 'array', items: { type: 'string' } },
  phase_guidance: { type: 'object', additionalProperties: false, properties: {
    f1: { type: 'string' }, f2: { type: 'string' }, f3: { type: 'string' }, f4: { type: 'string' },
  }, required: ['f1', 'f2', 'f3', 'f4'] },
  extra_gaps: { type: 'array', items: { type: 'string' } },
  intended_deferred: { type: 'array', items: { type: 'string' } },
  parity_watchouts: { type: 'array', items: { type: 'string' } },
  summary: { type: 'string' },
}, required: ['phase_guidance', 'summary'] }

const PLAN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  steps: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { file: { type: 'string' }, action: { type: 'string', enum: ['create', 'edit'] }, what: { type: 'string' } }, required: ['file', 'action', 'what'] } },
  needed: { type: 'boolean', description: 'false if this phase is already wired / unnecessary' },
  risks: { type: 'string' }, summary: { type: 'string' },
}, required: ['needed', 'summary'] }
const IMPL_SCHEMA = { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, changed_files: { type: 'array', items: { type: 'string' } }, new_tests: { type: 'integer' }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] }
const INTEG_SCHEMA = { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, reverted: { type: 'boolean' }, committed: { type: 'string' }, fail_counts: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'reverted', 'notes'] }
const REVIEW_SCHEMA = { type: 'object', additionalProperties: false, properties: { parity_ok: { type: 'boolean' }, matches_design: { type: 'boolean' }, wired_live: { type: 'boolean', description: 'is the new path reachable from the live app, not just tests' }, issues: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['parity_ok', 'matches_design', 'assessment'] }
const VERIFY_SCHEMA = { type: 'object', additionalProperties: false, properties: { works: { type: 'boolean' }, parity_default_unchanged: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' } }, required: ['works', 'detail'] }

const VERIFY_HARNESS = `On-screen verify. Rebuild ui: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/ 2>/dev/null; ./scripts/ui.sh start /tmp/parity/png.rcx\`; maximize (\`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`); shot \`./scripts/ui.sh shot /tmp/wf17_X.png\`; crop with \`magick\` + Read. *** SCALE TRAP: compute SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w' shot.png\`); REAL click coords = screenshot-px × SCALE. The Read image may be downscaled — before trusting any NEGATIVE result, click a KNOWN element (e.g. a menu) and confirm via a follow-up shot that the scale is right. *** To exercise the demo plugin, relaunch with the env affordance F3 added (RECLASS_DEMO_PLUGIN=1): \`RECLASS_DEMO_PLUGIN=1 ./scripts/ui.sh start /tmp/parity/png.rcx\`. \`./scripts/ui.sh stop\` when done.`

const safe = async (prompt, opts) => {
  try { return await agent(prompt, opts) } catch (e) {
    log(`[soft-fail] ${(opts && opts.label) || '?'}: ${String(e).slice(0, 140)} — continuing.`)
    return null
  }
}

let halted = false

async function runFix(p, guidance) {
  phase(p.title)
  if (halted) { log(`SKIP ${p.title} — a foundational fix reverted; cannot proceed.`); return { skipped: true } }

  const plan = await safe(
    `DESIGN STEP for plugin verify+wireup ${p.title}.\n${COMMON}\n\n${FACTS}\n\nPlanner guidance from the audit synthesis:\n${(guidance || '').slice(0, 1800)}\n\nGoal of this phase:\n${p.goal}\n\nRead the design doc section + the C++ reference + the EXACT current Rust you'll touch (get fresh line numbers), and produce a concrete, ordered plan: exact files+edits, key signatures, how it wires into the live app WITHOUT changing default behavior, feature-gating, and risks. If the phase is ALREADY wired correctly, set needed=false with a one-line justification. Keep increments small + compiling.`,
    { label: `design:${p.key}`, phase: p.title, schema: PLAN_SCHEMA })

  if (plan && plan.needed === false) {
    log(`${p.title}: design says ALREADY WIRED — ${plan.summary.slice(0, 120)}`)
    return { plan: plan.summary, skipped_noop: true }
  }

  const impl = await safe(
    `IMPLEMENT plugin verify+wireup ${p.title} on the real working tree (${OUT}).\n${COMMON}\n\n${FACTS}\n\nPlan to execute:\n${JSON.stringify((plan && plan.steps) || []).slice(0, 4000)}\nSummary: ${plan ? plan.summary : ''}\n\n${p.impl}\n\nWrite real, compiling Rust + tests. Prefer a working, tested vertical slice over breadth. Verify your own files compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\`; if you touched feature-gated code also \`${LP} cargo build --features plugins 2>&1|tail -3\`). Do NOT commit. Return what changed + whether it compiles + deferred items.`,
    { label: `impl:${p.key}`, phase: p.title, schema: IMPL_SCHEMA })

  const integ = await safe(
    `INTEGRATE plugin verify+wireup ${p.title}. Make the WHOLE project green and commit, or revert.\n${PARITY}\n\nContext (impl notes): ${impl ? (impl.notes || '').slice(0, 700) : ''}; compiles=${impl && impl.compiles}; deferred=${impl ? JSON.stringify(impl.deferred || []) : '[]'}.\nFix any build/test breakage (seams with existing code, feature gating, fmt). The wireup is additive — if a small part can't be made to work, prefer trimming to a compiling+tested subset over reverting the whole phase; only full-revert if the phase fundamentally can't integrate. Commit message: "feat(plugin): wf17 ${p.title} — <what landed>".`,
    { label: `integrate:${p.key}`, phase: p.title, schema: INTEG_SCHEMA })

  const review = await safe(
    `ADVERSARIAL SELF-REVIEW of plugin verify+wireup ${p.title} (READ-ONLY). Spec: ${DESIGN}; C++ truth: ${REF} + ${QT}/src/pluginmanager.cpp. Inspect the phase's commit (\`cd ${OUT} && git show --stat HEAD\` + read the changed files) and check: (1) PARITY — the DEFAULT app behaves/looks identical when no plugin UI is active; default build pulls no new deps; existing tests untouched; (2) the wireup is actually REACHABLE FROM THE LIVE APP (not just new tests) — trace the call path from a real UI event/startup to the manager method; flag anything still test-only; (3) matches the design + C++ behavior (load/unload/from-path/registry registration/identifier derivation); (4) real correctness issues / silent no-ops. Be skeptical. Return parity_ok, matches_design, wired_live, issues, assessment.`,
    { label: `review:${p.key}`, phase: p.title, schema: REVIEW_SCHEMA })

  let verify = null
  if (p.uiVerify) {
    verify = await safe(
      `VISUAL VERIFY of plugin verify+wireup ${p.title}.\n${VERIFY_HARNESS}\n\nCheck: ${p.uiVerify}\nCRITICAL parity check: also launch WITHOUT RECLASS_DEMO_PLUGIN and confirm the default UI is visually identical to before (no new menu items, no demo dock) — set parity_default_unchanged accordingly. Return works + parity_default_unchanged + shots + detail.`,
      { label: `verify:${p.key}`, phase: p.title, schema: VERIFY_SCHEMA })
  }

  const reverted = integ && integ.reverted
  if (reverted && p.foundational) { halted = true }
  log(`${p.title}: ${reverted ? 'REVERTED' : (integ && integ.green ? 'GREEN @' + (integ.committed || '?') : 'UNCLEAR')}` +
    `${review ? ` | review parity=${review.parity_ok} design=${review.matches_design} wired_live=${review.wired_live} issues=${(review.issues || []).length}` : ''}` +
    `${verify ? ` | ui works=${verify.works} default-unchanged=${verify.parity_default_unchanged}` : ''}`)
  return { plan: plan && plan.summary, impl: impl && { compiles: impl.compiles, deferred: impl.deferred }, integ, review, verify }
}

// ── Phase 1: AUDIT (parallel, read-only) ──────────────────────────────────────
phase('Audit')
const AUDITS = [
  { key: 'dialog', label: 'audit:dialog-actions',
    p: `The Manage-Plugins dialog (src/ui/window.rs PluginManagerDialog ~5449-5665) + its trigger (open_plugins_dialog ~2047). Determine exactly which actions are wired to live controls vs static text. C++ truth: ${QT}/src/pluginmanager.cpp gives LoadPluginFromPath + UnloadPlugin + the showPluginsDialog list (${REF} §6); our [fix] adds enable/disable + persistence + permission disclosure + safe-unload. List the gap between what the dialog SHOWS and what it can DO.` },
  { key: 'session', label: 'audit:session-manager',
    p: `Whether MainWindow owns a single PluginManager or rebuilds throwaways. Trace EVERY live consumer of provider lists: the process picker (src/ui/window.rs ~1520), the source chooser (src/ui/sourcechooser.rs — where is it invoked live, and from which registry?), and the dialog (~2056/~5479). Confirm they each build their OWN \`with_builtins()\` and would NOT see an enable/disable done elsewhere. Identify the minimal unification: one owned manager whose registry() is the single source all pickers read.` },
  { key: 'persist', label: 'audit:persistence',
    p: `How DiskSettings (src/ui/window.rs ~78-172, settings.json) loads/saves, the key naming convention used by existing settings (recentFiles/font/etc), and how to back the PluginPersistence trait (src/plugin/manager.rs ~40-87) with it (enabled flags + load-from-path set). Confirm enabled state is NOT persisted today and find the startup point where restore should happen.` },
  { key: 'host', label: 'audit:live-host',
    p: `What a live PluginHost needs vs the only existing impl MockPluginHost (src/plugin/host.rs). Enumerate the trait methods (toast/detach_documents_using/set_data_source/close_dialog/request_rerender or whatever exists) and, for detach_documents_using, find how MainWindow tracks open documents + which provider each uses (so safe-unload can detach before drop — the C++ dangling-provider [fix], ${REF}). Identify what a window-backed host adapter must bridge.` },
  { key: 'ui', label: 'audit:declarative-ui',
    p: `The declarative UI host: src/ui/pluginview.rs render_view_tree(), src/ui/pluginpanel.rs PluginPanel, src/ui/plugindialog.rs PluginDialog, and src/plugin/demo.rs (Commands/Panel/Dialog). Confirm none are instantiated live. Then find the codebase's real mechanisms to (a) add a menu/command entry, (b) mount a dock panel, (c) open a modal dialog — name the concrete types/functions (e.g. docks.rs, the menu builder, the modal layer) a wireup would call. Note the PARITY constraint: surfacing must not change the default UI when no UI-plugin is loaded.` },
  { key: 'cpp', label: 'audit:cpp-parity',
    p: `Diff our PluginManager (src/plugin/manager.rs) + the dialog behavior against C++ ${QT}/src/pluginmanager.cpp + ${QT}/src/iplugin.h + ${REF}: LoadPlugins folder scan, LoadPlugin (resolve CreatePlugin → register under identifier=Name().toLower().replace(" ","") with dllFileName), LoadPluginFromPath dedupe, UnloadPlugin (unregister → delete → unload), providerPlugins/FindPlugin, IProviderPlugin selectTarget/enumerateProcesses/providesProcessList/populatePluginMenu, and the intentionally-dead items (LoadType, getInitialBaseAddress). Flag any BEHAVIORAL divergence we should match (or deliberately improve).` },
  { key: 'loaders', label: 'audit:feature-gated-loaders',
    p: `Under \`--features plugins\`: confirm the P3 abi_stable loader (src/plugin/loader.rs), P4 reclassnet native bridge (src/plugin/reclassnet/), discovery (src/plugin/discovery.rs), and P5 managed scaffold still build + match design §3/§4 and ${REF} §8. Confirm the ONLY missing LIVE wire for the plugins feature is a startup discovery call + the load-from-path button. Flag any real correctness issue in the loaders themselves (sniffing, CoreFunctions signatures, bitness/region [fix]es).` },
]
const audits = (await parallel(AUDITS.map((a) => () =>
  safe(`READ-ONLY AUDIT — area "${a.key}". ${COMMON}\n\n${FACTS}\n\nInvestigate: ${a.p}\n\nReport the current reality with file:line evidence, a gaps list (each classified fix-now / fix-under-feature / intended-deferred / already-ok, with target_file + parity_risk), C++ parity notes, and a recommendation. Do not modify files.`,
    { label: a.label, phase: 'Audit', schema: AUDIT_SCHEMA, agentType: 'Explore' })
))).filter(Boolean)
log(`Audit complete: ${audits.length}/${AUDITS.length} areas; ${audits.reduce((n, a) => n + (a.gaps ? a.gaps.length : 0), 0)} raw gaps.`)

// ── Phase 2: PLAN (synthesize) ────────────────────────────────────────────────
phase('Plan')
const plan = await safe(
  `SYNTHESIZE the verify+wireup plan from the audit findings.\n${COMMON}\n\n${FACTS}\n\nAudit findings (JSON):\n${JSON.stringify(audits.map((a) => ({ area: a.area, reality: (a.reality || '').slice(0, 400), gaps: a.gaps, cpp: (a.cpp_parity_notes || '').slice(0, 300), rec: (a.recommendation || '').slice(0, 300) }))).slice(0, 12000)}\n\nProduce: confirmed_gaps; phase_guidance for the FOUR fix phases (f1 session-owned manager + DiskSettings persistence + live enable/disable; f2 live PluginHost + safe-unload + feature-gated load-from-path; f3 live declarative UI command/panel/dialog Elm loop with the demo behind RECLASS_DEMO_PLUGIN=1 so default UI is unchanged; f4 feature-gated startup folder-scan discovery) — each guidance = the concrete call paths + files + the parity guard for that phase; extra_gaps not covered by f1-f4 (if any, say whether they're in-scope); intended_deferred (P5 Windows, live reader, subprocess, permissions); parity_watchouts; summary. Keep each phase achievable as one parity-gated commit.`,
  { label: 'plan', phase: 'Plan', schema: FIXPLAN_SCHEMA })
const G = (plan && plan.phase_guidance) || {}
log(`Plan: ${plan ? plan.summary.slice(0, 200) : '(synthesis soft-failed; proceeding on FACTS)'}`)

// ── Phases 3-6: FIX (sequential — they build on each other + touch window.rs) ──
const FIXES = [
  { key: 'f1', title: 'F1 session manager + persistence + enable/disable', foundational: true,
    goal: `MainWindow must OWN one PluginManager for the session (today three live sites build throwaways — see FACTS). Make that owned manager's registry() the SINGLE source the process picker, source chooser, AND Manage-Plugins dialog read. Back enable/disable with REAL persistence: implement a DiskSettings-backed PluginPersistence (enabled flags keyed by identifier + the load-from-path set) and set_persistence() it at construction; restore stored enabled flags on startup (design §7.A [fix], §H). Make the dialog's rows carry a live enable/disable toggle → set_enabled(id, !enabled, persist=true) → the pickers + dialog reflect it immediately. C++ has no disable; our [fix] adds it (default = all enabled, so the default app is unchanged).`,
    impl: `Add a session PluginManager to MainWindow (or the shared app state) built once via with_builtins() with set_persistence(DiskSettings-backed). Re-point window.rs ~1520 (process picker), the live source-chooser path, and window.rs ~2056/~5479 (dialog) to read THIS manager instead of \`with_builtins()\` throwaways. Implement the DiskSettings-backed PluginPersistence (reuse the existing settings.json load/flush; namespaced keys e.g. "plugins.enabled.<id>"). Make PluginManagerDialog able to flip enabled (hold a handle/callback to the owned manager; re-render plugins_view after). Tests: DiskSettings persistence round-trip in a temp config dir; enable/disable reflected in plugins_view + the registry's enabled_providers. PARITY: with no user action the registry content/order + picker output is identical to today (file, buffer, snapshot, null — all enabled).` },
  { key: 'f2', title: 'F2 live host + safe-unload + load-from-path', foundational: false,
    goal: `Add the window-backed live PluginHost (the production counterpart to MockPluginHost) and wire the destructive/loading dialog actions C++ has. detach_documents_using(id) must close/detach any open document whose provider is this plugin BEFORE the plugin drops (the C++ dangling-provider [fix], ${REF}). Add a per-row Unload action for NON-builtin rows → safe_unload(id, &mut host). Behind #[cfg(feature="plugins")]: a "Load plugin…" action → native file picker → load_native_plugin(path) → on Ok persist the path + refresh; on Err surface it; also surface retained load_errors().`,
    impl: `Implement PluginHost for a window adapter bridging toast→status/notify, detach_documents_using→the real document/provider bookkeeping found in the audit, set_data_source, close_dialog, request_rerender→view_tree re-pull. Wire the Unload button (guard is_builtin) and the feature-gated Load-from-path button (+ error surfacing) into PluginManagerDialog. Keep load-from-path strictly behind \`plugins\`. Tests: the host adapter detach bookkeeping; safe_unload through the live host removes the row + leaves built-ins intact. PARITY: built-ins are not unloadable; the load button is absent in the default build (already disclosed).` },
  { key: 'f3', title: 'F3 live declarative UI (command/panel/dialog)', foundational: false,
    uiVerify: `With RECLASS_DEMO_PLUGIN=1: a Plugins menu shows the demo's Command and running it works; the demo Panel renders in a dock from its ViewTree with Zed styling, and clicking a panel button updates the panel (the Elm loop: handle_ui_event → fresh ViewTree → set_tree); the demo Dialog opens modally and submitting routes back (handle_dialog_closed) e.g. setting a data source. Confirm Zed styling + no panics.`,
    goal: `Prove the declarative host renders LIVE (P2 is unit-tested only; render_view_tree/PluginPanel/PluginDialog are never instantiated live). Wire: (a) contributed Commands populate a live "Plugins" menu (empty/hidden when none are contributed) and dispatch via handle_command; (b) a contributed Panel mounts in a dock (PluginPanel + render_view_tree), routing UiEvents via handle_ui_event and pushing the returned ViewTree back via set_tree; (c) a contributed Dialog opens modally (PluginDialog), routing DialogResult via handle_dialog_closed. Use the live PluginHost from F2 for routing side-effects.`,
    impl: `Instantiate PluginPanel/PluginDialog from the window and populate the Plugins menu from the owned manager's contributions; route events through the manager + live host. Surface the DEMO behind a documented dev affordance: env RECLASS_DEMO_PLUGIN=1 ⇒ build the session manager with with_builtins_and_demo() AND mount the demo Panel + add its Commands to the Plugins menu. UNSET (default) ⇒ NO demo plugin, the Plugins menu shows nothing new, NO demo dock — the UI is byte-for-byte unchanged (the HARD parity constraint). Tests where unit-testable (menu items derived from contributions; routing already covered in manager tests). If F2's live host reverted, add a minimal host here or limit to render-only and note it.` },
  { key: 'f4', title: 'F4 startup discovery (feature-gated)', foundational: false,
    goal: `Under #[cfg(feature="plugins")], at startup call load_native_plugins_from_default_dirs() on the SESSION manager (F1) so dropping a .so/.dll/.dylib into <exe>/plugins or ~/.config/reclass/plugins loads it (the C++ deferred LoadPlugins folder scan; design §6 P3, §7.C). Surface load failures via load_errors() in the dialog. DEFAULT build (plugins off) = no-op ⇒ parity preserved.`,
    impl: `Add the startup discovery hook (main.rs / app init) behind the \`plugins\` feature, merging discovered plugins into the F1 session manager; ensure the default build is unaffected (the hook compiles to nothing without the feature). Test (under \`--features plugins\`) that the startup path loads a plugin from a temp dir + records failures. PARITY: no behavior change in the default build.` },
]

const fixResults = []
for (const f of FIXES) {
  fixResults.push({ key: f.key, title: f.title, r: await runFix(f, G[f.key]) })
}

// ── Phase 7: VERIFY (on-screen, the wired actions + default parity) ───────────
phase('Verify')
const verify = await safe(
  `FULL ON-SCREEN VERIFY of the wired plugin system.\n${VERIFY_HARNESS}\n\nVerify the LANDED wireups (skip any phase that reverted/no-op'd per: ${JSON.stringify(fixResults.map((x) => ({ phase: x.title, committed: x.r && x.r.integ && x.r.integ.committed, reverted: x.r && x.r.integ && x.r.integ.reverted, noop: x.r && x.r.skipped_noop })))}):\n` +
  `1) DEFAULT launch (no env): open Plugins ▸ Manage Plugins — rows render (file/buffer/snapshot/null) with metadata + permissions; toggle a row's enable/disable, CLOSE + REOPEN the dialog and confirm it STUCK (persistence), and confirm a disabled provider disappears from the Source/Process picker. Confirm the default UI is otherwise IDENTICAL to before (no demo menu/dock).\n` +
  `2) RECLASS_DEMO_PLUGIN=1 launch: a Plugins menu shows the demo Command (runs), the demo Panel renders in a dock (clicking a button updates it — Elm loop), the demo Dialog opens + submit routes back.\n` +
  `Capture 3-5 cropped screenshots. Return works + parity_default_unchanged + shots + detail. Use the SCALE rule; verify scale before trusting any negative. Stop the app when done.`,
  { label: 'verify:final', phase: 'Verify', schema: VERIFY_SCHEMA })

// ── Phase 8: FINAL GATE + honest report ───────────────────────────────────────
phase('Final')
const final = await safe(
  `FINAL GATE + honest report for the plugin verify+wireup (${OUT}).\n${PARITY}\n\n` +
  `Fix outcomes: ${JSON.stringify(fixResults.map((x) => ({ phase: x.title, committed: x.r && x.r.integ && x.r.integ.committed, reverted: x.r && x.r.integ && x.r.integ.reverted, noop: x.r && x.r.skipped_noop, review: x.r && x.r.review && { parity: x.r.review.parity_ok, design: x.r.review.matches_design, wired_live: x.r.review.wired_live, issues: (x.r.review.issues || []).length } })))}\n` +
  `On-screen verify: ${JSON.stringify(verify && { works: verify.works, parity_default_unchanged: verify.parity_default_unchanged, detail: (verify.detail || '').slice(0, 400) })}\n` +
  `Intended-deferred per plan: ${JSON.stringify((plan && plan.intended_deferred) || [])}\n\n` +
  `DO: confirm the FULL parity gate is green at HEAD; \`cargo fmt\`; if anything is uncommitted commit "chore(plugin): wf17 final". Then write an HONEST report: which fix phases LANDED (committed) vs trimmed vs reverted vs no-op (already-wired); what is now REACHABLE FROM THE LIVE APP vs still test-only; the residual gaps + intended-deferred (P5 Windows scaffold, live-memory stub, subprocess loader, disclosure-only permissions); the final test counts (ui / bare / plugins / full); and concrete human next-steps. Return {green, committed, landed:[...], trimmed:[...], reverted:[...], still_test_only:[...], deferred:[...], test_counts, shots:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, landed: { type: 'array', items: { type: 'string' } }, trimmed: { type: 'array', items: { type: 'string' } }, reverted: { type: 'array', items: { type: 'string' } }, still_test_only: { type: 'array', items: { type: 'string' } }, deferred: { type: 'array', items: { type: 'string' } }, test_counts: { type: 'string' }, shots: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'assessment'] } })

return {
  audits: audits.map((a) => ({ area: a.area, gaps: (a.gaps || []).length })),
  plan: plan && plan.summary,
  fixes: fixResults.map((x) => ({ phase: x.title, committed: x.r && x.r.integ && x.r.integ.committed, reverted: x.r && x.r.integ && x.r.integ.reverted, noop: x.r && x.r.skipped_noop, wired_live: x.r && x.r.review && x.r.review.wired_live })),
  verify: verify && { works: verify.works, parity_default_unchanged: verify.parity_default_unchanged },
  final,
}
