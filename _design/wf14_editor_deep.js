export const meta = {
  name: 'reclass-editor-deep',
  description: 'EDITOR-ONLY deep parity sweep against the C++ Qt Scintilla-based editor (editor.cpp). Hunts everything still missing/diverging in the Rust editor surface — with two USER-REPORTED bugs as must-fix seeds: (1) Ctrl+mouse-wheel zoom in/out is missing (only keyboard Ctrl+=/-/0 zoom exists; C++ Scintilla zooms on Ctrl+wheel), and (2) arrow-key navigation does not descend into an EXPANDED nested class/struct defined inside another struct/class — determine the C++ intended behavior and match it. Also audits wheel/scroll, keyboard traversal, expand/collapse, mouse, inline-edit/pickers, hover/cursor, and remaining editor features. Implement + verify on-screen (compute the screenshot scale factor; do NOT assume 2x).',
  whenToUse: 'Deep dive on the editor surface alone — input handling (wheel/zoom/keyboard/mouse), tree navigation/expansion, inline edit, hover, and editor features vs the C++ editor.cpp.',
  phases: [
    { title: 'Audit', detail: '7 parallel read-only agents diff the C++ editor vs the Rust editor surface; two reported bugs are seeded must-investigates' },
    { title: 'Synthesize', detail: 'merge + dedupe + prioritize; assign each to a disjoint code-owner (mod.rs is a single owner — no concurrent edits)' },
    { title: 'Fix', detail: 'parallel owners implement the missing/diverging editor behaviors against the C++' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + full test suite + commit' },
    { title: 'Verify', detail: 'launch data-populated; scale-aware on-screen drive; MUST confirm Ctrl+wheel zoom + arrow-into-expanded-class -> punch-list' },
    { title: 'Polish', detail: 'fix the punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, editor-parity report' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

// Seeds: confirmed editor facts so audit agents don't re-derive from scratch.
const SEED = `CONFIRMED EDITOR FACTS (scouted in the live tree — build on these, don't contradict without evidence):
- The C++ editor is a QsciScintilla widget (editor.cpp \`m_sci\`); zoom + a lot of scroll/selection behavior is Scintilla-native (Ctrl+wheel zoom, SCI_SETZOOM, smooth scroll, keyboard caret nav).
- The Rust editor is a bespoke gpui surface: src/ui/editor/mod.rs (~5700+ L: Render, the actions/key-bindings, navigate_node, on_row_mouse_down, hover, context menu), geometry.rs (layout/columns), element.rs (RowElement paint), hit_test.rs, selection.rs, inline_edit.rs (FieldInput + EntityInputHandler), palette.rs.
- Rendering is fixed-cell monospace: every glyph advances cell_width; the line model is the controller's \`last_result().meta\` (Vec<LineMeta>) — Up/Down nav (navigate_node, mod.rs:2305) walks THESE visible lines, skipping footer/command-row/continuation rows.
- editor_font_size() = (EDITOR_SIZE + zoom_delta).clamp(6,48); zoom is wired to KEYBOARD only: Ctrl+= / Ctrl+- / Ctrl+0 -> EditorZoomIn/Out/Reset (mod.rs:262). Plain vertical scroll uses gpui UniformListScrollHandle. THERE IS NO scroll-wheel handler at all -> Ctrl+wheel zoom is absent (USER BUG #1).
- Already verified WORKING on-screen (do NOT re-report as broken): focus-on-click, arrow nav over visible rows, Down-at-end grows the struct, Shift+arrow multi-select, Ctrl+D duplicate, Ctrl+C/X/V node clipboard, Ctrl+Shift+Up/Down reorder, inline-edit text input, status bar, editor context menu, View toggles, big-endian, change-heat.`

const STUBS = `INTENTIONAL platform stubs — NOT editor gaps: live process/memory, RTTI/symbols/disasm over a live target, native plugins, native file dialogs (XDG portal unavailable headless). Don't report these.`

const REPORTED = `TWO USER-REPORTED behaviors that MUST be resolved this pass:
  BUG #1 — Ctrl+mouse-wheel does NOT zoom the editor in/out. The C++ Scintilla editor zooms font on Ctrl+wheel. The Rust editor has keyboard zoom (zoom_delta) but no scroll-wheel handler. FIX: add a scroll-wheel handler on the editor surface that, when Ctrl(/Cmd) is held, adjusts zoom_delta (reuse the existing zoom step/clamp + re-measure cell metrics) instead of scrolling; plain wheel still scrolls. Match the C++ zoom direction/step.
  BUG #2 — Define a new class/struct INSIDE another struct/class, EXPAND it, then press the arrow keys: selection does not descend into the expanded nested class's child fields (user: "it doesn't expand on that class, is that intended?"). DETERMINE the C++ behavior from editor.cpp keyPressEvent + the node-tree expansion/compose: in C++, does Down from a class-instance header move INTO the first expanded child? Does Right expand a collapsed node / Left collapse it? Then make the Rust match. Likely root cause is one of: (a) expanding a nested class instance doesn't emit child LineMeta rows into last_result().meta, or (b) navigate_node skips those child rows, or (c) the expand state doesn't propagate to a class-in-struct. Pin the exact cause with file:line.`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' }, summary: { type: 'string' },
    reported_bug_findings: { type: 'string', description: 'if this domain covers BUG #1 or #2, the C++ intended behavior + exact Rust root-cause file:line + the fix; else empty' },
    gaps: { type: 'array', items: {
      type: 'object', additionalProperties: false,
      properties: {
        feature: { type: 'string', description: 'a SPECIFIC editor behavior the C++ has and the Rust misses or does differently' },
        cpp_ref: { type: 'string' },
        rust_status: { type: 'string', enum: ['present', 'partial', 'wrong', 'stub', 'missing'] },
        fix_file: { type: 'string' },
        fix_hint: { type: 'string' },
        severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      },
      required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
    } },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT — 7 editor-input/behavior angles ─────────────────────────
const RUST_EDITOR = 'src/ui/editor/mod.rs, src/ui/editor/geometry.rs, src/ui/editor/element.rs, src/ui/editor/hit_test.rs, src/ui/editor/selection.rs, src/ui/editor/inline_edit.rs, src/ui/editor/palette.rs, src/controller.rs, src/core/tree.rs, src/core/node.rs'
const AUDIT_HEADER =
  `EDITOR-ONLY deep parity audit: the Rust+GPUI bespoke editor surface (${OUT}, src/ui/editor/*) vs the AUTHORITATIVE C++ Qt Scintilla editor (${QT}/editor.cpp 5077L + editor.h + rcxtooltip.h, and the navigation/expansion bits of ${QT}/controller.cpp + ${QT}/core.h). Read your C++ region THOROUGHLY (in chunks), then diff the Rust. Find every editor behavior the C++ has that the Rust MISSES or does DIFFERENTLY (rust_status:"wrong" for divergence). Be exhaustive — the user is sure a lot is still missing. READ-ONLY.\n\n` +
  SEED + `\n\n` + REPORTED + `\n\n` + STUBS + `\n\n` +
  `Rust editor files: ${RUST_EDITOR}.\nReference screenshots: ${CREF}/*.png.\nFor each gap: feature, cpp_ref (file:symbol/line), rust_status, fix_file, fix_hint, severity. If your domain owns a reported bug, fill reported_bug_findings.\n\n`

const AUDIT_DOMAINS = [
  { key: 'wheel_zoom_scroll', owns: 'BUG #1',
    focus: `WHEEL / ZOOM / SCROLL. Read the C++ editor's wheel + zoom + scroll path (Scintilla: Ctrl+wheel zoom, SCI_SETZOOM/SCI_ZOOMIN/OUT, wheel scroll, horizontal scroll, scroll-to-caret, scrollbar behavior, zoom bounds/step, any zoom reset). RESOLVE BUG #1: exactly how C++ zooms on Ctrl+wheel (direction, step, bounds) and the precise Rust fix (add an on_scroll_wheel handler: Ctrl held -> zoom via zoom_delta + re-measure cell metrics; else scroll). Also check: does plain wheel scroll work at the right speed? Shift+wheel horizontal? Does the view keep the selection visible on nav? Smooth vs paged scroll? Mouse-wheel over a collapsed/expanded tree?` },
  { key: 'keyboard_nav', owns: 'BUG #2',
    focus: `KEYBOARD NAVIGATION + SELECTION TRAVERSAL. Read editor.cpp keyPressEvent FULLY + the node-tree visible-order logic. Diff EXACT semantics vs Rust navigate_node/action_nav_*: Up/Down traversal ORDER over the visible tree INCLUDING descending into EXPANDED nested class/struct children (RESOLVE BUG #2 — does C++ Down enter the first expanded child of a class-instance? where does Rust diverge, file:line?); Left/Right (collapse/expand a tree node vs same-size type-cycle — which does C++ do?); Home/End/PageUp/PageDown; selection wrap; what is selected after each; Shift+arrow range extension semantics; Ctrl+arrow; caret/anchor model. Find every traversal divergence.` },
  { key: 'expand_collapse',
    focus: `EXPAND / COLLAPSE. Read the C++ expand/collapse model (node hasChildren, expanded flag, the +/- affordance, expandAll/collapseAll, auto-expand a newly-added class/struct, nested expansion state, lazy children). Diff vs Rust toggle_collapse + the line-model generation: when a class instance nested in a struct is expanded, are its child member rows EMITTED as LineMeta into last_result().meta (this is central to BUG #2)? Is collapse state preserved across refresh/mutation? Does adding a class auto-expand it like C++? Chevron alignment/affordance per depth. Find gaps.` },
  { key: 'mouse_interactions',
    focus: `MOUSE. Read editor.cpp mouse handlers (mousePressEvent/doubleClick/contextMenu/mouseMove/drag, Scintilla margin clicks). Diff vs Rust on_row_mouse_down + drag: single vs double vs triple click (double-click selects word/value? begins edit?), click targets per column (type/name/value/comment/offset/hex), drag-select rows, drag-to-reorder nodes, middle-click, the command/header row clicks, hover-row highlight, Ctrl/Shift-click selection, right-click context menu invocation + what's under the cursor. Find divergences + missing affordances.` },
  { key: 'inline_edit_pickers',
    focus: `INLINE EDITING + PICKER POPUPS. Read editor.cpp beginInlineEdit + the edit widgets + popups. Diff vs Rust inline_edit.rs + the pickers (type selector, enum picker, hex toolbar): every editable target (name/value/comment/offset/type/array-size/bitfield-width/enum-member) and its editor; commit on Enter / cancel on Esc / commit on click-away; Tab / Shift-Tab moving between fields; numeric vs hex value parsing + validation + error feedback; value write formats (signed/unsigned/float/hex/big-endian); editing while live-refresh runs. Find missing editable targets, wrong commit/cancel, missing validation.` },
  { key: 'hover_cursor',
    focus: `HOVER POPUPS + CURSOR. Read rcxtooltip.h + editor.cpp HoverPopup/TitleBodyPopup/ValueHistoryPopup + applyHoverCursor. Diff vs Rust hover/tooltip: what each hover shows (value-history graph, struct/type preview, pointer-target preview, disasm), hover delay + dismiss, the CURSOR SHAPE per target (pointer over a navigable type, text cursor over editable, etc.), hover over the chevron/source chip. Find missing hover content + wrong/missing cursor changes.` },
  { key: 'editor_features',
    focus: `REMAINING EDITOR FEATURES. Read editor.cpp for everything not covered above: find-in-editor (find bar: next/prev/wrap/highlight-all/match-field), Go-To-Definition (F12 open referenced struct in tab), bookmarks within the editor, node copy/paste/duplicate PLACEMENT rules, split view / multiple editor panes, multi-tab behavior, presentation mode, drag-drop into the editor, refresh/auto-refresh visuals, "select all in struct", any editor toolbar, address bar / base-address edit on the command row, the offset/address margin modes. Diff vs Rust. Find missing features.` },
]

phase('Audit')
log(`Editor-only audit: ${AUDIT_DOMAINS.length} angles in parallel (BUG #1 in wheel_zoom_scroll, BUG #2 in keyboard_nav/expand_collapse).`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}${d.owns ? ` (owns ${d.owns})` : ''}. ${d.focus}\n\n` +
    `Exhaustive editor-behavior gap list. Resolve any reported bug you own with concrete file:line root cause + C++ intended behavior.`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
const bugFindings = audits.filter(Boolean).map(a => a.reported_bug_findings).filter(s => s && s.trim())
log(`Audit: ${allGaps.length} editor gaps (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.rust_status === 'wrong').length} diverging, ${allGaps.filter(g => g.rust_status === 'missing').length} missing). Reported-bug findings: ${bugFindings.length}.`)
bugFindings.forEach((f, i) => log(`  bug-finding ${i + 1}: ${f.slice(0, 200)}`))

// ── PHASE 2: SYNTHESIZE ─────────────────────────────────────────────────────
phase('Synthesize')
// mod.rs MUST be a single owner (one big file — no concurrent edits). Owners by file.
const OWNER_FILES = {
  surface: 'src/ui/editor/mod.rs, src/ui/editor/palette.rs  (the editor surface: wheel/zoom handler, keyboard nav/traversal, mouse handlers, selection, context menu, hover invocation, actions/bindings)',
  geometry: 'src/ui/editor/geometry.rs, src/ui/editor/element.rs, src/ui/editor/hit_test.rs, src/ui/editor/selection.rs  (layout/columns, RowElement paint, hit-testing, scroll/zoom metrics)',
  inline: 'src/ui/editor/inline_edit.rs  (inline-edit field + IME input)',
  navcore: 'src/controller.rs, src/controller/, src/core/tree.rs, src/core/node.rs  (navigation/expansion/visible-line-model logic behind the editor)',
  support: 'src/ui/tooltip.rs, src/ui/findbar.rs, src/ui/statusbar.rs  (editor support widgets)',
}
const synth = await agent(
  `Synthesis of an EDITOR-ONLY parity audit (${allGaps.length} raw gaps). Reported-bug findings from the audit:\n${bugFindings.map((f, i) => `(${i + 1}) ${f}`).join('\n')}\n\nRaw gaps:\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feat: g.feature, st: g.rust_status, f: g.fix_file, sev: g.severity, hint: (g.fix_hint || '').slice(0, 120) }))) +
  `\n\nDE-DUPLICATE, drop known platform stubs / already-working items, and PRIORITIZE: the TWO user-reported bugs (Ctrl+wheel zoom; arrow-into-expanded-nested-class) are TOP priority and MUST be scheduled; then "wrong" divergences, then missing behaviors; blocker>major>minor. For each surviving item return: feature, owner (surface|geometry|inline|navcore|support — assign by fix_file; ALL src/ui/editor/mod.rs work goes to "surface" since that file cannot be edited by two agents at once), fix_file, fix_hint, severity. Keep it comprehensive. Group related gaps so one owner ships a coherent fix. Ensure the two reported bugs are explicitly present as items.`,
  { label: 'synthesize', phase: 'Synthesize', schema: {
    type: 'object', additionalProperties: false,
    properties: {
      items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
        feature: { type: 'string' }, owner: { type: 'string', enum: ['surface', 'geometry', 'inline', 'navcore', 'support'] },
        fix_file: { type: 'string' }, fix_hint: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
      bug1_plan: { type: 'string' }, bug2_plan: { type: 'string' }, overview: { type: 'string' },
    }, required: ['items', 'overview'],
  } })
const items = (synth && synth.items) || []
log(`Synthesized ${items.length} items. BUG1: ${synth ? (synth.bug1_plan || '').slice(0, 140) : ''} | BUG2: ${synth ? (synth.bug2_plan || '').slice(0, 140) : ''}`)

// ── PHASE 3: FIX (parallel by owner; mod.rs = single owner) ─────────────────
phase('Fix')
const CPP = `${QT}/editor.cpp, ${QT}/editor.h, ${QT}/rcxtooltip.h, ${QT}/controller.cpp, ${QT}/core.h`
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owners: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `Implement missing/diverging EDITOR behaviors in the Reclass Rust port (${OUT}) to match the C++ Qt Scintilla editor EXACTLY. Port precise semantics (zoom step/bounds, traversal order, selection post-conditions), not approximations. Add #[cfg(test)] tests pinning the new behavior. Aesthetic = Zed (design.rs tokens).\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${OWNER_FILES[owner]}. Do NOT touch src/ui/mod.rs or other owners' files (other agents edit them concurrently). Do NOT run git/cargo fmt. Keep the build green + existing tests passing (update a test only if it pinned OLD wrong behavior; say so).\n` +
    `AUTHORITATIVE C++: ${CPP}. Reference: ${CREF}/*.png.\n` +
    `${SEED}\n\n` +
    `IMPLEMENT these audited items (match C++ precisely; the two USER-REPORTED bugs among them are MUST-FIX):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}] ${it.feature}\n     fix: ${it.fix_hint}`).join('\n') +
    `\n\nNOTES on the reported bugs if they're yours:\n- BUG #1 (Ctrl+wheel zoom): add a scroll-wheel handler on the editor surface element; when Ctrl/Cmd is held, adjust zoom_delta by the C++ zoom step (re-measure cell metrics so columns/hit-test stay aligned) and DON'T scroll; otherwise let it scroll normally.\n- BUG #2 (arrow into expanded nested class): make Down/Up traverse INTO an expanded nested class/struct's child rows exactly as C++ does; if the cause is that expanding a nested class doesn't emit child LineMeta, fix the line-model so it does.\n\n` +
    `Work each to completion. If an item already matches C++ (false positive), skip + note it. Ensure your files compile (\`${BUILD}\`). Return {owner, compiles, implemented:[...], false_positives:[...], notes}.`,
    { label: `fix:${owner}`, phase: 'Fix', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, implemented: { type: 'array', items: { type: 'string' } }, false_positives: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}(${(r.implemented || []).length})`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `An EDITOR-ONLY parity pass just landed (${OUT}) — wheel/zoom, keyboard traversal, expand/collapse, mouse, inline-edit, hover, editor features. Make the FULL build authoritative-green + consistent.\n` +
  `Notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ o: fixOwners[i], ok: r.compiles, impl: (r.implemented || []).length, notes: (r.notes || '').slice(0, 150) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (likely seams: a new scroll-wheel handler in the Render/element, navigate_node + line-model changes shared between surface and navcore, new geometry metrics for zoom). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) FULL tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\` + \`${LP} cargo test --lib "ui::" 2>&1|tail -3\` — fix newly-broken tests (or update ones pinning old wrong behavior). 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "feat(editor): C++ parity — Ctrl+wheel zoom, arrow-into-expanded-nested-class, + editor input/traversal/hover gaps (audited)"\`. Report green + committed=<hash>.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display; scale-robust) ─────────────────────────
const VERIFY_HARNESS =
  `You OWN the headless display. ALWAYS rebuild the ui binary first: \`cd ${OUT} && ${LP} cargo build 2>&1|tail -2\` (the headless build overwrites the ui binary).\n` +
  `SETUP: \`mkdir -p /tmp/parity && cp ${OUT}/assets/examples/png.rcx ${OUT}/assets/examples/sample.png /tmp/parity/; ./scripts/ui.sh start /tmp/parity/png.rcx\`. Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz; xdotool windowmove "$WID" 0 0; sleep 1\`. Shot: \`./scripts/ui.sh shot /tmp/e_X.png\`; CROP with \`convert\` + Read.\n` +
  `*** COORDINATE SCALE — COMPUTE IT, DON'T ASSUME (a prior verifier hit a FALSE NEGATIVE by clicking un-scaled coords): screen size = \`xdotool getdisplaygeometry\` (e.g. 1680 1050); screenshot width = \`identify -format '%w' /tmp/e_X.png\`. SCALE = screen_width / screenshot_width (it has been 2.0 in one env and 1.0 in another). To click a row at screenshot pixel (sx,sy), run \`xdotool mousemove $((sx*SCALE_NUM/SCALE_DEN)) ... click 1\` using REAL coords. Verify your scale by clicking a known row and reading the status bar before trusting any negative result. ***\n` +
  `KEYBOARD/mouse confirmed reaching the app once a node is selected. Wheel: \`xdotool click 4\` = wheel-up, \`click 5\` = wheel-down; hold Ctrl with \`xdotool keydown ctrl; xdotool click 4; xdotool keyup ctrl\`.\n`

phase('Verify')
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the EDITOR-ONLY parity pass.\n\n` + VERIFY_HARNESS + `\n` +
  `MUST-VERIFY the two user-reported bugs FIRST:\n` +
  `  (A) Ctrl+WHEEL ZOOM: hold Ctrl and wheel-up over the editor a few times -> the editor font/rows must GROW (and Ctrl+wheel-down shrink); plain wheel (no Ctrl) must SCROLL, not zoom. Screenshot before/after.\n` +
  `  (B) ARROW INTO EXPANDED NESTED CLASS: build the repro — add/convert a node to a nested class or struct inside the root (via the type picker / context menu), EXPAND it (click its chevron or Right-arrow), select its header, then press Down: selection MUST descend into the expanded child fields (matching C++). Screenshot the selection landing inside the child. If the repro can't be built via UI, drive it and say exactly where it fails.\n` +
  `Then spot-check ~6 more of the highest-value editor corrections from the fix notes (e.g. Left/Right collapse-expand, double-click behavior, a new hover/cursor, a new inline-edit target, find-in-editor). For each: matches C++?\n` +
  `Return: shots, findings=[{feature, severity, works(bool), detail}], overall (call out bug A and bug B explicitly). \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: { type: 'object', additionalProperties: false, properties: { shots: { type: 'array', items: { type: 'string' } }, findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { feature: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, works: { type: 'boolean' }, detail: { type: 'string' } }, required: ['feature', 'works', 'detail'] } }, overall: { type: 'string' } }, required: ['shots', 'findings', 'overall'] } })
const fails = (verify && verify.findings || []).filter(f => !f.works)
log(`Verify: ${(verify && verify.findings || []).length} checks, ${fails.length} failing. ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner) ─────────────────────────────────────
phase('Polish')
const ownerOf = (f) => {
  const t = (f.feature + ' ' + f.detail).toLowerCase()
  if (/expand|collapse|line model|visible row|emit child|line_meta|tree node|nextid|navigate.*core/.test(t)) return 'navcore'
  if (/hover|tooltip|cursor|find bar|status bar|findbar/.test(t)) return 'support'
  if (/inline|edit field|ime|commit|cancel|validation|picker input/.test(t)) return 'inline'
  if (/layout|column|hit.?test|paint|metric|cell_width|geometry|element/.test(t)) return 'geometry'
  return 'surface'
}
const byOwner = {}
for (const f of fails) (byOwner[ownerOf(f)] = byOwner[ownerOf(f)] || []).push(f)
const polishOwners = Object.keys(byOwner)
log(`Polishing ${polishOwners.length}: ${polishOwners.map(o => `${o}(${byOwner[o].length})`).join(' ')}`)
if (polishOwners.length) {
  await parallel(polishOwners.map(owner => () =>
    agent(
      `Fix-up pass (${OUT}). YOUR OWNERSHIP (edit ONLY): ${OWNER_FILES[owner]}. C++: ${CPP}. Aesthetic = Zed. Do NOT run git/cargo fmt.\n` +
      `On-screen QA found these editor corrections NOT matching the C++ — fix each precisely (the two reported bugs are highest priority if listed):\n` +
      byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.feature}: ${f.detail}`).join('\n') +
      `\n\nBuild green (\`${BUILD}\`). Return {compiles, notes}.`,
      { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { compiles: { type: 'boolean' }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } })
  ))
}

// ── PHASE 7: FINAL ──────────────────────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the EDITOR-ONLY parity sweep (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -2\` + FULL tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -8\`, \`${LP} cargo test --lib "ui::" 2>&1|tail -3\`). 2) \`cargo fmt\`, commit any remaining: \`cd ${OUT} && git add -A && git commit -m "fix(editor): parity polish — QA corrections" || echo "nothing to commit"\`. 3) FINAL SCREENSHOTS (compute the scale factor as in the verify harness): data-populated png + the two reported bugs FIXED (Ctrl+wheel zoom larger/smaller; arrow descending into an expanded nested class child). \`./scripts/ui.sh stop\`.\n` +
  `4) EDITOR-PARITY REPORT: confirm BUG #1 + BUG #2 are fixed (or honestly not), how many editor gaps were corrected vs false-positives, and the honest remaining editor gap. Return {green, committed, bug1_fixed, bug2_fixed, corrected_count, shots, remaining:[...], assessment}.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, bug1_fixed: { type: 'boolean' }, bug2_fixed: { type: 'boolean' }, corrected_count: { type: 'integer' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''} bug1=${final && final.bug1_fixed} bug2=${final && final.bug2_fixed}`)

return {
  audit: { total: allGaps.length, wrong: allGaps.filter(g => g.rust_status === 'wrong').length, missing: allGaps.filter(g => g.rust_status === 'missing').length, bug_findings: bugFindings, by_domain: audits.filter(Boolean).map(a => ({ d: a.domain, n: (a.gaps || []).length, summary: (a.summary || '').slice(0, 120) })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, implemented: (r.implemented || []).length, false_positives: (r.false_positives || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { overall: verify.overall, failing: fails.length, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, bug1_fixed: final.bug1_fixed, bug2_fixed: final.bug2_fixed, corrected_count: final.corrected_count, remaining: final.remaining, assessment: final.assessment },
}
