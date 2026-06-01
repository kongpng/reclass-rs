export const meta = {
  name: 'reclass-downkey-grow-and-reorder',
  description: 'Achieve C++ parity for editor arrow-key behavior: Down past the end appends a field to the enclosing struct (works for ALL last-node shapes incl. arrays/structs, currently broken), Up-at-top is a no-op, and Ctrl+Shift+Up/Down reorders a field among siblings (currently missing). Diagnose against the C++, implement, verify on-screen.',
  whenToUse: 'Fix the Down-at-end grow + add the reorder shortcut to match the C++ keyPressEvent.',
  phases: [
    { title: 'Diagnose', detail: '2 parallel read-only agents trace the C++ append/move semantics + the Rust grow bug, cross-check the precise fix' },
    { title: 'Implement', detail: 'fix the grow path (append to enclosing struct) + add Ctrl+Shift+Up/Down reorder' },
    { title: 'Verify', detail: 'launch headless, drive Down-at-end on empty/primitive-end/array-end structs + reorder + up-at-top, screenshot' },
    { title: 'Final', detail: 'green build (both profiles) + tests + commit' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// ── PHASE 1: DIAGNOSE (2 parallel, read-only, adversarial) ──────────────────
phase('Diagnose')
const DIAG_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    side: { type: 'string' },
    grow_semantics: { type: 'string', description: 'exactly what Down-at-end does: which struct, what node kind/size, where inserted' },
    reorder_semantics: { type: 'string', description: 'exactly what Ctrl+Shift+Up/Down does + up-at-top behavior' },
    rust_root_cause: { type: 'string' },
    fix_plan: { type: 'string', description: 'the precise Rust change(s): which functions, which controller calls, signatures' },
    refs: { type: 'array', items: { type: 'string' } },
  },
  required: ['side', 'grow_semantics', 'fix_plan'],
}
const diags = await parallel([
  () => agent(
    `Read-only AUTHORITATIVE C++ trace for the Reclass editor arrow-key behavior (C++ Qt source at ${QT}). Read:\n` +
    `  • ${QT}/editor.cpp around lines 2938-2975 (keyPressEvent Key_Up/Key_Down): normal arrow = navigate between nodes; Down that walks off the end = \`emit appendSingleFieldRequested(lastNodeId)\`; Ctrl+Shift+Up/Down = \`emit moveNodeRequested(nodeIdx, dir)\`; Up-at-top = no-op.\n` +
    `  • ${QT}/controller.cpp around line 962 (the \`appendSingleFieldRequested\` connect/handler): what struct receives the field, what NODE KIND + SIZE the appended field is, and WHERE it is inserted (struct tail?).\n` +
    `  • ${QT}/controller.cpp around line 792 (the \`moveNodeRequested\` connect/handler): exactly how a field is reordered among its siblings (swap with neighbor? clamp at ends? offset recompute?).\n` +
    `Report the EXACT semantics (side="cpp"): grow_semantics (struct + kind/size + insert position), reorder_semantics (+ up-at-top), and a fix_plan note on what the Rust must replicate. Quote the C++ in refs (file:line).`,
    { label: 'diag:cpp', phase: 'Diagnose', schema: DIAG_SCHEMA },
  ),
  () => agent(
    `Read-only RUST trace for the editor grow/reorder bug (port at ${OUT}). The Down-at-end "grow" is implemented but BROKEN when the last visible node is an array/struct (the struct does not grow; a "sibling overlap" can occur). Reorder (Ctrl+Shift+Up/Down) appears MISSING. Read:\n` +
    `  • ${OUT}/src/ui/editor/mod.rs: \`navigate_node\` (the grow branch when the forward walk falls off the end — it calls \`insert_anchor\` + \`controller.insert_node\`), \`insert_anchor\` (returns (parent_id, n.offset + size_for_kind(n.kind)) — note size_for_kind returns the PRIMITIVE size, which is 0/wrong for an Array/Struct last node, so the insert lands inside the node's span instead of after it), and \`append_bytes_to_struct\` + \`on_footer_click\` (the footer "+Nh" pills — these append at the STRUCT TAIL correctly and are the mechanism to reuse).\n` +
    `  • ${OUT}/src/controller.rs: \`insert_node\`, \`insert_node_above\`, \`remove_node\`, and search for ANY move/reorder/swap-sibling op (likely none — confirm). Note what minimal controller op a reorder needs (move a node up/down among its siblings, recomputing offsets), or whether it can be done with existing ops.\n` +
    `Report (side="rust"): rust_root_cause (why the array-end grow fails), and a precise fix_plan: (1) the grow path should append ONE field to the ENCLOSING STRUCT of the last visible node (its parent_id, or the view-root for a top-level node) at the struct's tail — reuse append_bytes_to_struct / its underlying controller call rather than insert_anchor; (2) for reorder, the exact controller op to add (signature) + the editor key bindings (ctrl-shift-up / ctrl-shift-down) + handler. Cite refs (file:line).`,
    { label: 'diag:rust', phase: 'Diagnose', schema: DIAG_SCHEMA },
  ),
])
const cppDiag = diags.find(d => d && d.side === 'cpp') || diags[0]
const rustDiag = diags.find(d => d && d.side === 'rust') || diags[1]
log(`Diagnose: cpp grow="${(cppDiag && cppDiag.grow_semantics || '').slice(0, 120)}" | rust cause="${(rustDiag && rustDiag.rust_root_cause || '').slice(0, 120)}"`)

// ── PHASE 2: IMPLEMENT ──────────────────────────────────────────────────────
phase('Implement')
const impl = await agent(
  `Implement C++-parity editor arrow-key behavior in the Reclass Rust port (${OUT}). You may edit ${OUT}/src/ui/editor/* AND ${OUT}/src/controller.rs (a reorder op is a legitimate small logic addition — the C++ controller has it; keep it minimal + unit-tested). Do NOT touch other modules. Aesthetic = Zed (design.rs tokens).\n\n` +
  `AUTHORITATIVE C++ semantics (from the diagnosis):\n` +
  `  GROW (Down-at-end): ${cppDiag ? cppDiag.grow_semantics : 'append a single field to the enclosing struct at its tail'}\n` +
  `  REORDER (Ctrl+Shift+Up/Down): ${cppDiag ? cppDiag.reorder_semantics : 'move the field up/down among its siblings; clamp at ends'}\n` +
  `RUST ROOT CAUSE: ${rustDiag ? rustDiag.rust_root_cause : 'insert_anchor uses primitive size_for_kind, wrong for array/struct last nodes'}\n` +
  `FIX PLAN: ${rustDiag ? rustDiag.fix_plan : 'append one field to the enclosing struct via append_bytes_to_struct'}\n\n` +
  `DO:\n` +
  `1) FIX THE GROW PATH in \`navigate_node\` (mod.rs): when the forward walk falls off the end, append ONE field (a Hex64 / matching the C++ appendSingleField type) to the ENCLOSING STRUCT of the last visible data node (the last node's parent_id; for a top-level node that is the view-root struct id) at the struct's TAIL — reuse \`append_bytes_to_struct\` (or its underlying controller append-at-tail call) which the footer "+Nh" pills use correctly. Do NOT use the buggy \`insert_anchor\` (which adds size_for_kind(kind), 0 for arrays/structs → insert lands inside the node → overlap / no growth). The struct must visibly GROW (size in the "}; … // 0x.. (N)" footer increases; a new Hex64 row appears) for EVERY last-node shape: primitive, pointer, struct, and ARRAY. Up-at-top stays a silent no-op.\n` +
  `2) ADD REORDER: implement Ctrl+Shift+Up/Down = move the selected/active field up/down among its siblings (the C++ \`moveNodeRequested\`). Add the minimal controller op if none exists (e.g. \`pub fn move_node(&mut self, node_idx: usize, dir: i32)\` — swap with the adjacent sibling at the same depth, recompute offsets, mark dirty; clamp at the first/last sibling). Wire editor actions + key bindings \`ctrl-shift-up\` / \`ctrl-shift-down\` in the RcxEditor context (NOTE: the existing plain up/down arrows must still navigate; only the Ctrl+Shift combo reorders). Add #[cfg(test)] tests for the controller move op (moves, clamps at ends, offsets recompute).\n` +
  `3) Keep plain Up/Down navigation + the focus-on-click intact. Build green: \`${BUILD}\`. Return the structured result.`,
  { label: 'implement', phase: 'Implement', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, files: { type: 'array', items: { type: 'string' } }, controller_op_added: { type: 'string' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
log(`Implement: ${impl ? (impl.compiles ? 'compiles' : 'NOT compiling') : 'FAILED'} — ${impl ? (impl.notes || '').slice(0, 140) : ''}`)

// ── PHASE 3: VERIFY (serial, owns display) ──────────────────────────────────
phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the editor Down-at-end GROW + Ctrl+Shift reorder fixes. You OWN the headless display. Build with \`cd ${OUT} && ${LP} cargo build 2>&1 | tail -2\` first (the prior phase may have left a headless binary — always rebuild the ui binary).\n\n` +
  `HARNESS (rebuilds + launches the UI binary): \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/png.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/g_X.png\`; CROP with \`convert IN -crop WxH+X+Y OUT\` then Read. Drive: \`./scripts/ui.sh key <keys>\` (e.g. Down, ctrl+shift+Down, End), \`./scripts/ui.sh click X Y\`.\n\n` +
  `TEST (the png PNG_Header struct ENDS IN AN ARRAY 'chunk_data_tail uint8_t[7]' then the footer "}; … // 0x80 (128)" — the exact case that was broken):\n` +
  `  1. /tmp/g_grow.png — click a node in the editor to FOCUS it, press \`End\` (jump to last node), then \`Down\` 2-3 times. Confirm the struct GROWS: the footer sizeof "// 0x80 (128)" INCREASES (e.g. 0x88/0x90...) and new \`hex64\` rows appear at the end. Crop the footer region before/after to compare the sizeof number. THIS IS THE HEADLINE — it was a no-op before.\n` +
  `  2. /tmp/g_reorder.png — click a field (e.g. an early hex64 row) to select+focus it, press \`ctrl+shift+Down\` — confirm that field swaps places with the next sibling (its offset/order changes). Then \`ctrl+shift+Up\` to move it back.\n` +
  `  3. /tmp/g_uptop.png — select the first data node, press \`Up\` repeatedly — confirm it does NOT insert anything above (silent no-op; struct size unchanged).\n` +
  `  Also try on a FRESH empty struct if reachable (File ▸ New Class via the menu, or just note if not): Down should append the first field.\n` +
  `Be precise: read the footer sizeof number before vs after. Return: shots (paths), and findings = list of {item, severity('blocker'|'major'|'minor'), works(bool), detail}. overall = does Down-at-end grow for the array-end struct, does reorder work, is up-at-top a no-op? \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { item: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['item', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 4: POLISH if needed (serial) ──────────────────────────────────────
if (fails.length > 0) {
  phase('Implement') // reuse the Implement group for a fix-up pass
  const fixup = await agent(
    `Fix-up pass for the editor Down-grow/reorder work (${OUT}/src/ui/editor/* + ${OUT}/src/controller.rs). On-screen QA found these NOT working:\n` +
    fails.map((f, i) => `  ${i + 1}. [${f.severity}] ${f.item}: ${f.detail}`).join('\n') +
    `\n\nC++ grow semantics: ${cppDiag ? cppDiag.grow_semantics : ''}. Reorder: ${cppDiag ? cppDiag.reorder_semantics : ''}.\n` +
    `Fix each so it works end-to-end (the struct must visibly grow on Down-at-end incl. the array-end case; reorder swaps siblings; up-at-top no-op). Build green (\`${BUILD}\`). Return {compiles, notes}.`,
    { label: 'fixup', phase: 'Implement', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  log(`Fixup: ${fixup ? fixup.notes.slice(0, 140) : 'failed'}`)
}

// ── PHASE 5: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the editor Down-grow + reorder parity work (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` green + tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\` and \`${LP} cargo test --lib "ui::editor" 2>&1|tail -3\` and \`${LP} cargo test --lib "controller" 2>&1|tail -3\`. 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui(editor): Down-at-end appends a field to the enclosing struct (incl. array/struct-tail), + Ctrl+Shift+Up/Down reorder — C++ keyPressEvent parity"\`. 3) FINAL SCREENSHOT: start with png.rcx, focus the editor, End, Down x3, capture /tmp/gfinal_grow.png and Read it to confirm the struct grew (footer sizeof increased + new hex64 rows). \`./scripts/ui.sh stop\`.\n` +
  `Return {green, committed, grew (bool: did Down-at-end visibly grow the struct), notes}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, grew: { type: 'boolean' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''} grew=${final && final.grew}`)

return {
  diagnose: { cpp: cppDiag && cppDiag.grow_semantics, rust_cause: rustDiag && rustDiag.rust_root_cause },
  implement: impl && { compiles: impl.compiles, controller_op: impl.controller_op_added },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, grew: final.grew },
}
