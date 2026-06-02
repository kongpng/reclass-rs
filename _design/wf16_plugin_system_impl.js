export const meta = {
  name: 'reclass-plugin-system-impl',
  description: 'Implement the plugin system from _design/plugin_system_design.md, phase by phase, on the real working tree. Sequential phases (contract+providers → declarative UI host → native abi_stable loader+SDK → ReClass.NET native compat → ReClass.NET managed scaffold → management/permissions polish). Each phase: design → implement (+tests) → integrate behind a HARD PARITY GATE (the existing app + full suite must stay green; a phase that cannot reach green AUTO-REVERTS so the repo is never broken) → adversarial self-review; UI phases also verify on-screen. Honest about platform limits: managed-C# is Windows-only scaffold, the live reader stays a stub, [future] items deferred.',
  whenToUse: 'Build the plugin subsystem incrementally with parity always preserved.',
  phases: [
    { title: 'P1 contract+providers' },
    { title: 'P2 declarative UI host' },
    { title: 'P3 native loader+SDK' },
    { title: 'P4 reclassnet native' },
    { title: 'P5 reclassnet managed (scaffold)' },
    { title: 'P6 management+permissions' },
    { title: 'Final' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const DESIGN = `${OUT}/_design/plugin_system_design.md`
const REF = `${OUT}/_design/plugin_system_cpp_reference.md`
const QT = '/home/loke/Documents/Reclass'

// The non-negotiable gate every phase's integrate step runs.
const PARITY = `*** PARITY GATE — we MUST stay at parity; the EXISTING app + its tests must remain green. ***
Run ALL of these and require success:
  1) ui build:        \`cd ${OUT} && ${LP} cargo build 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished, 0 errors
  2) features build:  \`cargo build --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep -E '^error|Finished' | tail -3\`  → Finished
  3) full tests:      \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
  4) ui tests:        \`${LP} cargo test --lib "ui::" 2>&1 | grep 'test result' | tail -1\`  → 0 failed
  5) bare tests:      \`cargo test --no-default-features 2>&1 | grep 'test result' | awk '{p+=$4;f+=$6} END{print "pass="p" fail="f}'\`  → fail=0
If ALL pass (with your new tests included): \`cd ${OUT} && cargo fmt && git add -A && git commit -m "<msg>"\` and report green=true, committed=<short-hash>.
If you CANNOT reach green after reasonable fixing effort: \`cd ${OUT} && git reset --hard HEAD && git clean -fd\` to REVERT THIS PHASE ENTIRELY (the previous phase's commit is HEAD, so this restores full parity), and report green=false, reverted=true, with notes on what blocked it. NEVER leave the tree red or commit a red tree. NEVER touch target/ or .git config.`

const COMMON = `Authoritative spec: ${DESIGN} (READ the relevant section first). Verified C++ behavior: ${REF}. C++ source: ${QT}. Aesthetic for any UI = Zed (src/ui/design.rs tokens). The plugin CONTRACT must stay light (no heavy deps in the default build); gate abi_stable/libloading behind a \`plugins\` cargo feature so default builds are unaffected. Add #[cfg(test)] tests for all new logic. Do NOT modify unrelated modules; keep the existing public behavior intact (parity). Do NOT run git commit yourself in design/implement steps (the integrate step owns commits/reverts).`

const PLAN_SCHEMA = { type: 'object', additionalProperties: false, properties: {
  steps: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { file: { type: 'string' }, action: { type: 'string', enum: ['create', 'edit'] }, what: { type: 'string' } }, required: ['file', 'action', 'what'] } },
  new_deps: { type: 'array', items: { type: 'string' } }, feature_gating: { type: 'string' }, risks: { type: 'string' }, summary: { type: 'string' },
}, required: ['steps', 'summary'] }
const IMPL_SCHEMA = { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, changed_files: { type: 'array', items: { type: 'string' } }, new_tests: { type: 'integer' }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] }
const INTEG_SCHEMA = { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, reverted: { type: 'boolean' }, committed: { type: 'string' }, fail_counts: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'reverted', 'notes'] }
const REVIEW_SCHEMA = { type: 'object', additionalProperties: false, properties: { parity_ok: { type: 'boolean' }, matches_design: { type: 'boolean' }, issues: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['parity_ok', 'matches_design', 'assessment'] }
const VERIFY_SCHEMA = { type: 'object', additionalProperties: false, properties: { works: { type: 'boolean' }, shots: { type: 'array', items: { type: 'string' } }, detail: { type: 'string' } }, required: ['works', 'detail'] }

const VERIFY_HARNESS = `On-screen verify. Rebuild the ui binary: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\`. Launch: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`; maximize (\`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`); shot \`./scripts/ui.sh shot /tmp/p_X.png\`; crop with \`magick\` + Read. *** SCALE: compute SCALE = screen_width(\`xdotool getdisplaygeometry\`) / screenshot_width(\`magick identify -format '%w'\`); click REAL coords = screenshot-px × SCALE; the Read view may be shrunk — verify scale by clicking a known row + reading the status bar before trusting a negative. *** \`./scripts/ui.sh stop\` when done.`

let halted = false

async function runPhase(p) {
  phase(p.title)
  if (halted) { log(`SKIP ${p.title} — a foundational phase reverted; cannot proceed.`); return { skipped: true } }

  const plan = await agent(
    `DESIGN STEP for plugin-system ${p.title}.\n${COMMON}\n\nGoal of this phase:\n${p.goal}\n\nRead the design doc section + the verified C++ reference + the EXISTING Rust code you'll touch, and produce a concrete, ordered implementation plan: exact files to create/edit, the key trait/struct signatures, how it wires into existing code WITHOUT breaking current behavior, feature-gating, new deps, and risks. Keep increments small + compiling.`,
    { label: `design:${p.key}`, phase: p.title, schema: PLAN_SCHEMA })

  const impl = await agent(
    `IMPLEMENT plugin-system ${p.title} on the real working tree (${OUT}).\n${COMMON}\n\nPlan to execute:\n${JSON.stringify((plan && plan.steps) || []).slice(0, 4000)}\nSummary: ${plan ? plan.summary : ''}\n\n${p.impl}\n\nWrite real, compiling Rust + tests. Prefer a working, tested vertical slice over breadth. Verify your own files compile (\`cd ${OUT} && ${LP} cargo build 2>&1|grep -E '^error|Finished'|tail -5\` and, if you added a feature, also build with it). Do NOT commit. Return what you changed + whether it compiles + any deferred items.`,
    { label: `impl:${p.key}`, phase: p.title, schema: IMPL_SCHEMA })

  const integ = await agent(
    `INTEGRATE plugin-system ${p.title}. Make the WHOLE project green and commit, or revert.\n${PARITY}\n\nContext (impl notes): ${impl ? (impl.notes || '').slice(0, 600) : ''}; compiles=${impl && impl.compiles}; deferred=${impl ? JSON.stringify(impl.deferred || []) : '[]'}.\nFix any build/test breakage from this phase (seams with existing code, feature gating, fmt). The new plugin code is additive — if a small part can't be made to work, prefer trimming it to a compiling+tested subset over reverting the whole phase; only full-revert if the phase fundamentally can't integrate. Commit message: "feat(plugin): ${p.title} — <what landed>".`,
    { label: `integrate:${p.key}`, phase: p.title, schema: INTEG_SCHEMA })

  const review = await agent(
    `ADVERSARIAL SELF-REVIEW of plugin-system ${p.title} (READ-ONLY). Spec: ${DESIGN}; C++ truth: ${REF}. Inspect the phase's commit (\`cd ${OUT} && git show --stat HEAD\` + read the changed files) and check: (1) PARITY preserved — existing behavior/tests untouched, default build unaffected by new feature-gated deps; (2) the new code MATCHES the design + the verified C++ behavior (signatures, the ProviderRegistry wiring, the contract shape, the [fix] items for this phase); (3) real correctness issues, stubs that silently no-op, or places that diverge from the C++ reference. Be skeptical. Return parity_ok, matches_design, a concrete issues list, and an assessment.`,
    { label: `review:${p.key}`, phase: p.title, schema: REVIEW_SCHEMA })

  let verify = null
  if (p.uiVerify) {
    verify = await agent(
      `VISUAL VERIFY of plugin-system ${p.title}.\n${VERIFY_HARNESS}\n\nCheck: ${p.uiVerify}\nAlso confirm the existing app still looks/works right (parity). Return works + shots + detail.`,
      { label: `verify:${p.key}`, phase: p.title, schema: VERIFY_SCHEMA })
  }

  const reverted = integ && integ.reverted
  if (reverted && p.foundational) { halted = true }
  log(`${p.title}: ${reverted ? 'REVERTED' : (integ && integ.green ? 'GREEN @' + (integ.committed || '?') : 'UNCLEAR')}` +
    `${review ? ` | review parity=${review.parity_ok} design=${review.matches_design} issues=${(review.issues || []).length}` : ''}` +
    `${verify ? ` | ui works=${verify.works}` : ''}`)
  return { plan: plan && plan.summary, impl: impl && { compiles: impl.compiles, deferred: impl.deferred }, integ, review, verify }
}

// ── Phase specs ─────────────────────────────────────────────────────────────
const PHASES = [
  { key: 'p1', title: 'P1 contract+providers', foundational: true,
    goal: `Design doc §2,§3(types),§5,§6 Phase 1 + the [fix] items in §7.A. Define the core plugin CONTRACT in a new \`src/plugin/\` module: \`Plugin\`, \`Contribution\` (Provider + Command/Panel/Dialog/StatusItem variants), \`ProviderSpec\` (can_handle/create_provider/initial_base/select_target/enumerate_processes mapping to the existing \`Provider\` trait), \`PluginManifest\`, \`PluginHost\`, \`ViewTree\`, \`UiEvent\`, \`CommandSlot\`, \`DockSide\`. Add a \`PluginManager\` that registers \`Provider\` contributions into the EXISTING \`src/provider/registry.rs\` ProviderRegistry. Reimplement the built-in File/Buffer/Snapshot/Null providers as in-tree registered plugins through this contract. Centralize the identifier derivation; use ONE shared provider-list model for the Source picker. Keep this all CORE (no new heavy deps).`,
    impl: `Create \`src/plugin/\` (declare in src/lib.rs / src/main.rs as appropriate — note ui modules are pre-declared in src/ui/mod.rs, do not break that). Wire built-ins through the contract + into the existing ProviderRegistry so the existing Source picker + Manage Plugins dialog (src/ui/window.rs) read the real registry. Preserve ALL current behavior (the app must look/work identically). Add tests for the contract + the in-tree provider registration.` },

  { key: 'p2', title: 'P2 declarative UI host', uiVerify: `a demo in-tree plugin contributes a menu/command item, a dock Panel (rendered from a ViewTree), and a modal Dialog — confirm they render with Zed styling and that clicking routes a UiEvent/command back to the plugin (e.g. a button updates the panel).`,
    goal: `Design doc §3 + §6 Phase 2 + §7.E (auto-theme, settings pages). Build the host-side renderer that turns a plugin's \`ViewTree\` into Zed-styled gpui-component widgets, for a \`Command\` (menu/context/source-menu item), one \`Panel\` (dock), and one \`Dialog\` (modal). Route \`UiEvent\` + command dispatch back to the plugin (Elm-style: plugin returns an updated ViewTree).`,
    impl: `Add \`src/plugin/ui_host.rs\` (or similar) rendering ViewTree with gpui-component; wire a contributed Command into the menu/command system and a Panel into a dock + a Dialog into the modal layer (src/ui/window.rs). Ship a small demo in-tree plugin exercising all three. Keep it behind the ui feature. Tests for the ViewTree→widget mapping where unit-testable.` },

  { key: 'p3', title: 'P3 native loader+SDK', foundational: false,
    goal: `Design doc §2 (loading), §6 Phase 3, §7.G (SDK). Implement the native dynamic loader behind a \`plugins\` cargo feature: an \`abi_stable\` stable-ABI mirror of the contract (\`#[sabi_trait]\` Plugin/PluginHost, RBox/RString/RVec ViewTree), a root-module export, and a version/layout check on load via \`libloading\`/abi_stable. Discover + load \`kind = native\` plugins from a \`plugins/\` dir. Provide a \`reclass-plugin\` SDK surface (the trait + export glue; a \`#[plugin]\` macro is optional) and an EXAMPLE native provider plugin + an EXAMPLE native UI plugin (separate cdylib crates) to prove load+run.`,
    impl: `Add the loader + abi_stable types behind \`plugins\`. If crates can't be fetched (offline), say so and revert this phase (parity preserved) rather than leaving it red. Build example cdylib plugin crate(s) and load one in a test (or a headless smoke check). The DEFAULT build must NOT pull abi_stable/libloading — verify the parity gate's default builds are unaffected.` },

  { key: 'p4', title: 'P4 reclassnet native', foundational: false,
    goal: `Design doc §4 (UPDATED) + §6 Phase 4 + reference §8. ReClass.NET compat is a CORE host subsystem — NOT one of our plugins. The user just DROPS a ReClass.NET plugin DLL (+ any sidecar, e.g. memflow's \`memflow.toml\`) into the \`plugins/\` folder; the host's folder-scan discovery (from P3) AUTO-DETECTS it by sniffing its exports and bridges it. THIS PHASE = the native path: detect a DLL that exports the ReClass.NET CoreFunctions, \`libloading\`-resolve the 8 functions (mind __stdcall on Win32; plain elsewhere), the #pragma pack(1) UTF-16 callback structs, callback-based enumeration → Vecs, and wrap as a \`Provider\` registered under \`reclass.netcompatlayer\`. Canonical target shape: \`memflow-reclass-plugin\` (a Rust cdylib exporting the CoreFunctions). Apply the [fix]es vs C++: detect target bitness instead of hardcoding pointer_size=8 where determinable; expose REAL enumerate_regions from the section enumeration (don't discard sections); avoid the 0x10000 size sentinel where a real range is known.`,
    impl: `Add a CORE \`src/plugin/reclassnet/\` (native path) behind the \`plugins\` feature and wire its detection into the folder-scan discovery (sniff exports: a DLL exporting e.g. ReadRemoteMemory+OpenRemoteProcess+EnumerateProcesses → ReClass.NET native). This is core host code, NOT a wrapped Contribution::Provider plugin. Build a tiny TEST ReClass.NET-style native plugin cdylib (in examples/plugins/) that exports the 8 CoreFunctions over an in-memory buffer "process" (memflow itself needs real connectors, so use a buffer), load it via the folder scan, and assert read + section/module enumeration work end-to-end in a test. Cross-platform (no Windows-only code in this phase).` },

  { key: 'p5', title: 'P5 reclassnet managed (scaffold)', foundational: false,
    goal: `Design doc §4 (managed path, UPDATED) + §6 Phase 5 + reference §8. CORE subsystem (not a plugin). Scope DECIDED: managed MEMORY backends only (Windows). When the folder scan finds a .NET assembly (PE CLR header), on Windows host the .NET Framework CLR (mscoree COM via the \`windows\` crate, v4.0.30319, ICLRRuntimeHost::ExecuteInDefaultAppDomain) + a C# bridge that marshals \`ICoreProcessFunctions\` → the same CoreFunctions table → the same \`Provider\` as P4. A managed assembly that does NOT implement ICoreProcessFunctions (a node-type/UI plugin like FrostbitePlugin) must be DETECTED and SKIPPED with a logged "ReClass.NET node-type plugin unsupported" message — do NOT attempt to bridge node types/UI. Windows-only; CANNOT be built/tested on this Linux host → implement as a clean \`#[cfg(windows)]\` SCAFFOLD + PORT \`${QT}/plugins/RcNetPluginCompatLayer/bridge/RcNetBridge.cs\` into the repo (vendored).`,
    impl: `Add the core \`#[cfg(windows)]\` CLR-host path under \`src/plugin/reclassnet/\` + vendor the C# bridge + a build note + the node-type-plugin "unsupported" detection/log (this part is cross-platform and testable). CRITICAL: the DEFAULT Linux build + all existing tests stay GREEN (cfg(windows) code must be gated + unreferenced on non-Windows). Do NOT build/run the managed path here. If clean gating is impractical, revert (parity > scaffold).` },

  { key: 'p6', title: 'P6 management+permissions', uiVerify: `the Manage Plugins dialog shows the real registry (built-in + any discovered plugins) with metadata, an enable/disable toggle that persists, and disclosed permissions; load-from-path + unload still work; the Source picker still attaches providers. Confirm the existing app behavior is intact.`,
    goal: `Design doc §5 + §6 Phase 6 + §7.A/C (honor auto/manual, persist enable/disable, multiple plugin dirs, safe-unload) + §7 permission disclosure. Upgrade the Manage Plugins dialog from a read-only built-in list to a real manager: list built-in + discovered, metadata + declared permissions (our-format plugins carry a plugin.toml manifest; AUTO-DETECTED ReClass.NET compat plugins have NO our manifest — show them with their detected kind [reclassnet-native / reclassnet-managed] + ReClass.NET-derived name, registered under reclass.netcompatlayer), enable/disable with persistence (settings), honor load=auto|manual, reload, load-from-path + unload (keep C++'s), surface load/ABI-mismatch errors, and SAFE-UNLOAD (detach docs using a provider before unloading — fixing C++'s dangling-provider risk).`,
    impl: `Implement manifest parsing (plugin.toml), settings persistence of enabled state, the upgraded dialog (src/ui/window.rs), safe-unload, and multiple plugin dirs. Tests for manifest parse + enable/disable persistence + safe-unload bookkeeping.` },
]

// RESUME: P1–P3 already landed + committed in a prior run that crashed mid-P4
// (HEAD = e81f423). Re-running them would collide with the committed code, so this
// run continues from P4 on the existing tree; P1–P3 stay as their committed state.
const results = []
for (const p of PHASES.filter((x) => ['p4', 'p5', 'p6'].includes(x.key))) {
  results.push({ key: p.key, title: p.title, r: await runPhase(p) })
}

// ── Final gate + honest report ──────────────────────────────────────────────
phase('Final')
const final = await agent(
  `FINAL GATE + report for the plugin-system implementation (${OUT}).\n${PARITY}\n\n` +
  `Phase outcomes: ${JSON.stringify(results.map(x => ({ phase: x.title, r: x.r && { committed: x.r.integ && x.r.integ.committed, reverted: x.r.integ && x.r.integ.reverted, green: x.r.integ && x.r.integ.green, skipped: x.r.skipped, review: x.r.review && { parity: x.r.review.parity_ok, design: x.r.review.matches_design } } })))}\n\n` +
  `DO: confirm the FULL parity gate is green at HEAD; \`cargo fmt\`; if anything uncommitted, commit "chore(plugin): final integration". Capture 2-3 screenshots of working plugin UI if P2/P6 landed (use the scale rule). \`./scripts/ui.sh stop\`.\n` +
  `Then write an HONEST report: which phases landed (committed) vs trimmed vs reverted; what is REAL+TESTED here vs scaffolded (P5 Windows managed) vs deferred ([future] items, subprocess loader, the live-memory reader which stays a deliberate stub); the residual parity status (existing test counts); and the concrete next steps a human should review. Return {green, committed, landed:[...], reverted:[...], scaffolded:[...], deferred:[...], shots:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, landed: { type: 'array', items: { type: 'string' } }, reverted: { type: 'array', items: { type: 'string' } }, scaffolded: { type: 'array', items: { type: 'string' } }, deferred: { type: 'array', items: { type: 'string' } }, shots: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'assessment'] } })

return {
  phases: results.map(x => ({ phase: x.title, committed: x.r && x.r.integ && x.r.integ.committed, reverted: x.r && x.r.integ && x.r.integ.reverted, skipped: x.r && x.r.skipped, review: x.r && x.r.review && { parity_ok: x.r.review.parity_ok, matches_design: x.r.review.matches_design, issues: (x.r.review.issues || []).length }, ui_works: x.r && x.r.verify && x.r.verify.works })),
  final,
}
