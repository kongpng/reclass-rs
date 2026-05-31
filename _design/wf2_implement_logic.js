export const meta = {
  name: 'reclass-rs-implement-logic',
  description: 'Implement the logic-module bodies of the Rust port, faithfully and test-driven against the captured C++ golden oracle. Leaf modules in parallel (copy-isolated) → integrate → dependent modules → integrate.',
  whenToUse: 'Phase 2 of the Reclass C++→Rust port: fill the todo!() module bodies with real, oracle-verified implementations.',
  phases: [
    { title: 'Implement L1', detail: 'leaf logic modules (depend only on core+provider), one copy-isolated agent each' },
    { title: 'Integrate L1', detail: 'merge deps/features, full workspace build+test, commit' },
    { title: 'Implement L2', detail: 'modules that build on L1 (controller/imports/mcp), copy-isolated' },
    { title: 'Integrate L2', detail: 'merge, full build+test, commit' },
  ],
}

const SRC = '/home/loke/reclass-cpp'
const OUT = '/home/loke/reclass-rs'

// Layer 1: depend only on the already-implemented core + provider modules.
const L1 = [
  { key: 'compose',   path: 'src/compose.rs',   feat: '', cpp: 'src/compose.cpp', spec: 'compose-undo',
    tests: 'test_compose.cpp, test_chips.cpp, test_static_fields.cpp, test_default_class_footer.cpp',
    oracle: 'test_compose, test_chips, test_static_fields, test_default_class_footer',
    notes: 'Includes the 15-command undo/redo stack + batch macros and the NodeTree+Provider→text/LineMeta composition. Golden compose dumps are in _oracle/fixtures/eprocess_normal.txt and eprocess_compact.txt — assert against them.' },
  { key: 'format',    path: 'src/format.rs',    feat: '', cpp: 'src/format.cpp', spec: 'format-render',
    tests: 'test_format.cpp', oracle: 'test_format', notes: 'Value formatting + hex/ASCII previews; output-exact — assert formatted strings against the oracle.' },
  { key: 'addr',      path: 'src/addr.rs',      feat: '', cpp: 'src/addressparser.cpp', spec: 'addressparser',
    tests: 'test_addressparser.cpp', oracle: 'test_addressparser', notes: 'Address-expression evaluator with module/symbol resolution via callbacks. (If PORTING_addressparser.md is absent, work from understand/addressparser.md + the C++ source.)' },
  { key: 'generator', path: 'src/generator.rs', feat: '', cpp: 'src/generator.cpp', spec: 'generator',
    tests: 'test_generator.cpp', oracle: 'test_generator', notes: 'NodeTree→source emission (multiple backends). Output-exact — assert generated text against the oracle (test_generator has ~1760 lines of assertions).' },
  { key: 'scanner',   path: 'src/scanner.rs',   feat: '', cpp: 'src/scanner.cpp', spec: 'scanner',
    tests: 'test_scanner.cpp, test_scanner_combinations.cpp', oracle: 'test_scanner, test_scanner_combinations',
    notes: 'A pure search engine over byte slices (&[u8]) supplied by the Provider trait: int/float/string/bytes matching, comparison operators, refine/next-scan, result sets. Port the algorithms; parallelize with rayon.' },
  { key: 'disasm',    path: 'src/disasm.rs',    feat: 'disasm', cpp: 'src/disasm.cpp', spec: 'disasm',
    tests: 'test_disasm.cpp', oracle: 'test_disasm', notes: 'x86/x64 decode+format via iced-x86 (replaces fadec). Output-exact lines — assert against the oracle.' },
  { key: 'rtti',      path: 'src/rtti/',        feat: 'symbols', cpp: 'src/rtti.cpp, src/symbolstore.cpp, src/symbol_downloader.cpp, src/names/*', spec: 'rtti-symbols',
    tests: 'test_rtti.cpp, test_rtti_hint.cpp', oracle: 'test_rtti, test_rtti_hint',
    notes: 'RTTI walkers, symbol store, name providers/registry, demanglers (cpp_demangle Itanium + msvc-demangler MSVC), symbol download (reqwest). Note: test_rtti has 1 Windows/Itanium-only skip per the oracle.' },
  { key: 'theme',     path: 'src/theme/',       feat: '', cpp: 'src/themes/*', spec: 'themes',
    tests: 'test_theme.cpp', oracle: 'test_theme',
    notes: 'Runtime JSON theme model + manager. IMPORTANT: match the SHIPPED golden theme JSON in _oracle/fixtures/themes/*.json (8 default themes). The C++ test_theme has 3 recorded failures where its hardcoded expectations DRIFTED from the shipped JSON — prefer the shipped JSON behavior and note the divergence; do not reproduce the C++ test bugs.' },
]

// Layer 2: build on Layer 1 (so they get a REAL compose/format/etc. once L1 is integrated).
const L2 = [
  { key: 'controller', path: 'src/controller.rs', feat: '', cpp: 'src/controller.cpp', spec: 'controller',
    tests: 'test_controller.cpp, test_refresh_speedups.cpp', oracle: 'test_controller, test_refresh_speedups',
    notes: 'Document + 15-command undo/redo apply, the periodic refresh loop that re-reads the active Provider, the per-node value-history ring buffer + change heatmap, following pointer fields to read the referenced offset via the Provider, applying edits back through the Provider, and selection. Build on the now-real compose/format/provider.' },
  { key: 'imports',    path: 'src/imports/',     feat: 'imports', cpp: 'src/imports/import_source.cpp, import_reclass_xml.cpp, export_reclass_xml.cpp, import_pdb.cpp, pe_debug_info.cpp', spec: 'imports',
    tests: 'test_import_source.cpp, test_import_xml.cpp, test_export_xml.cpp', oracle: 'test_import_source, test_import_xml, test_export_xml',
    notes: '.rcx JSON (serde_json), C/C++ source parse (hand-rolled tokenizer + regex), ReClass XML in/out (quick-xml), PDB import (pdb2) and PE debug-dir (hand-decode over Provider with bytemuck). PDB import is Windows-gated in C++ (test_import_pdb is if(WIN32)); keep the pdb2-based code cross-platform but it need not be oracle-tested here. Builds NodeTrees, so depends on the real compose/core.' },
  { key: 'mcp',        path: 'src/mcp/',         feat: 'mcp', cpp: 'src/mcp/mcp_bridge.cpp', spec: 'mcp',
    tests: 'test_mcp.cpp', oracle: 'test_mcp',
    notes: 'JSON-RPC 2.0 server over a local socket (interprocess) + the tool/notification schemas (projectState, treeApply, sourceSwitch, hexRead/hexWrite over the active Provider, statusSet, uiAction, treeSearch, nodeHistory; notifyTreeChanged/notifyDataChanged). Note: the C++ multiClient_bothInitialize test is a recorded flaky failure under headless Linux — do not chase it.' },
]

const MOD_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    module: { type: 'string' },
    green: { type: 'boolean' },
    passed: { type: 'integer' },
    failed: { type: 'integer' },
    files_copied: { type: 'array', items: { type: 'string' } },
    new_deps: { type: 'array', items: { type: 'string' } },
    new_features: { type: 'array', items: { type: 'string' } },
    lib_rs_changes_needed: { type: 'string' },
    notes: { type: 'string' },
  },
  required: ['module', 'green', 'notes'],
}

const header =
  `You are porting ONE module of the open-source application "Reclass" (github.com/IChooseYou/Reclass, MIT) from C++ to Rust, faithfully (1:1 behavior). This is ordinary software porting of already-public source. ` +
  `The C++ source is at ${SRC}. The Rust port is a SINGLE package "reclass" with subsystems as modules under src/ (see ${OUT}/_design/ARCHITECTURE.md). The core and provider modules are ALREADY implemented — build on them; do not change them. Implement ONLY your assigned module; all other modules are compiling skeletons (todo!()) and must be left untouched.\n\n`

function modPrompt(m) {
  const feat = m.feat ? '--features ' + m.feat + ' ' : ''
  return header +
    `MODULE: ${m.key}  (file(s): ${m.path};  maps C++ ${m.cpp})\n\n` +
    `ISOLATION (do this FIRST — work on a PRIVATE copy so parallel sibling agents can't clobber your in-progress edits). Do NOT copy the huge target/ build dir. Run:\n` +
    `    rm -rf /tmp/rcx-${m.key} && rsync -a --exclude=target ${OUT}/ /tmp/rcx-${m.key}/\n` +
    `Do ALL editing and \`cargo test\` INSIDE /tmp/rcx-${m.key}/ (it already contains the real core+provider and any integrated lower layers; .git is included so \`git status\` works). Only at the very END copy your finished files back to the MAIN checkout ${OUT}/.\n\n` +
    `READ FIRST (from either tree): ${OUT}/_design/specs/PORTING_${m.spec}.md (the implementation plan; if missing, use the understand map), ${OUT}/_design/understand/*.md (your subsystem's behavioral map), the C++ source (${m.cpp}) under ${SRC}, the current skeleton ${m.path}, ${OUT}/_design/crate_selection.md (which crates to use — do NOT hand-roll what a chosen crate covers), and the GOLDEN ORACLE: ${OUT}/_oracle/logs/ for [${m.oracle}] + the matching ${OUT}/_oracle/test_sources/*.cpp + any ${OUT}/_oracle/fixtures/.\n\n` +
    `IMPLEMENT FULLY: replace every todo!()/unimplemented!() in your module with real Rust reproducing the C++ behavior 1:1, using the chosen crates. Keep public signatures consistent with how core/lib.rs and other modules call this module (grep for usages). Doc-comment items with their C++ origin (file:line). ${m.notes}\n\n` +
    `TESTS (the fidelity contract): translate the covering C++ test(s) [${m.tests}] into Rust tests — a #[cfg(test)] mod in your module, and/or an integration test at tests/${m.key}_oracle.rs. Where behavior is output-exact, assert against the captured golden output/fixtures. Do NOT weaken assertions to make them pass.\n\n` +
    `VERIFY in /tmp/rcx-${m.key}/: run \`cargo test --no-default-features ${feat}\` until your module's tests are GREEN (the rest of the lib still compiles via skeleton todo!()s, which is fine). Iterate until green. If you need a crate/feature not in Cargo.toml, add it to /tmp/rcx-${m.key}/Cargo.toml to proceed, and REPORT it (the integrator adds it centrally).\n\n` +
    `MERGE OUT (when green): copy EVERY source file you created/modified under src/ and any test file under tests/ FROM /tmp/rcx-${m.key}/ INTO the MAIN checkout ${OUT}/ (same relative paths; overwrite; create dirs as needed). List exactly what you changed with \`git -C /tmp/rcx-${m.key} status --porcelain\`. Do NOT copy Cargo.toml or src/lib.rs to main — instead report any new deps/features/module-declarations in the structured result so the integrator applies them centrally (avoids clobbering parallel siblings). Finally clean up: \`rm -rf /tmp/rcx-${m.key}\`.\n\n` +
    `Return the structured result (module, green, passed, failed, files_copied, new_deps, new_features, lib_rs_changes_needed, notes).`
}

function integratorPrompt(layer, mods, featList) {
  return `You are the INTEGRATOR for the Rust port of "Reclass" (a single package "reclass"; see ${OUT}/_design/ARCHITECTURE.md), layer ${layer}, working in the MAIN repo ${OUT} (NOT a copy). ` +
    `The layer-${layer} module agents (${mods.map(m => m.key).join(', ')}) have copied their implementations + tests into src/ and tests/. Reconcile and verify the whole package.\n\n` +
    `1) Apply central changes the agents reported: add any new crate dependencies and [features] entries to ${OUT}/Cargo.toml, and any needed module declarations to src/lib.rs (with correct #[cfg(feature=...)] gates). Keep versions aligned with ${OUT}/_design/crate_selection.md.\n` +
    `2) BUILD + TEST: \`cargo build --no-default-features\`; \`cargo test --no-default-features\`; then \`cargo test --no-default-features --features ${featList}\`. Then \`cargo build\` (FULL, with ui/gpui — export LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16) to confirm the UI build still links.\n` +
    `3) FIX integration issues (cross-module API mismatches, duplicate/missing glue, feature-gate errors). Fix the CODE, never weaken tests. If a module came back incomplete or blocked, keep the build green (leave its remaining parts as skeleton) and clearly note what's unfinished.\n` +
    `4) \`cargo fmt\`, then — ONLY IF real module code was actually delivered (src/ changed beyond skeletons) — \`git add -A && git commit -m "implement logic layer ${layer} (${mods.map(m => m.key).join(', ')})"\`. If nothing was delivered, do NOT commit; report that instead.\n\n` +
    `Report: per-module pass/fail counts, total tests passing, the status of \`cargo test --no-default-features --features ${featList}\` and the full \`cargo build\`, and anything still stubbed/blocked.`
}

async function tryAgent(p, opts) {
  try { return await agent(p, opts) } catch (e) { log(`agent ${opts && opts.label} failed: ${String(e).slice(0, 160)}`); return null }
}

// ---------- run ----------
log('Phase 2 — implementing logic modules (copy-isolated, oracle-driven).')

phase('Implement L1')
const r1 = await parallel(L1.map(m => () =>
  agent(modPrompt(m), { phase: 'Implement L1', label: `impl:${m.key}`, schema: MOD_SCHEMA })))
const r1ok = (r1 || []).filter(Boolean)
log(`L1 implemented: ${r1ok.filter(r => r.green).map(r => r.module).join(', ') || 'none green'}; ` +
    `not-green: ${r1ok.filter(r => !r.green).map(r => r.module).join(', ') || 'none'}`)

phase('Integrate L1')
const i1 = await tryAgent(integratorPrompt(1, L1, 'disasm,symbols'), { label: 'integrate:L1' })

phase('Implement L2')
const r2 = await parallel(L2.map(m => () =>
  agent(modPrompt(m), { phase: 'Implement L2', label: `impl:${m.key}`, schema: MOD_SCHEMA })))
const r2ok = (r2 || []).filter(Boolean)
log(`L2 implemented: ${r2ok.filter(r => r.green).map(r => r.module).join(', ') || 'none green'}`)

phase('Integrate L2')
const i2 = await tryAgent(integratorPrompt(2, L2, 'disasm,symbols,imports,mcp'), { label: 'integrate:L2' })

return {
  layer1: r1ok.map(r => ({ module: r.module, green: r.green, passed: r.passed, failed: r.failed })),
  layer2: r2ok.map(r => ({ module: r.module, green: r.green, passed: r.passed, failed: r.failed })),
  integrate_l1: i1,
  integrate_l2: i2,
}
