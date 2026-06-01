export const meta = {
  name: 'reclass-parity-audit-and-wire',
  description: 'Systematic feature-parity audit of the Rust GPUI port against the REAL C++ Qt Reclass (/home/loke/Documents/Reclass), then wire up everything that is missing/broken: source-chip click -> Data Source picker, header chevron -> Type Selector, source hover, inline-edit field offset, File>Examples open, and every other menu/editor/dialog/dock action that looks wired but is a no-op. Verify with screenshots.',
  whenToUse: 'When "a lot of stuff isn\'t wired up" — do an exhaustive C++-vs-Rust diff and close every gap.',
  phases: [
    { title: 'Audit', detail: '6 parallel read-only agents diff each domain (menus, editor, pickers, dialogs, docks, chrome) of the C++ Qt source vs the Rust port -> structured gap list' },
    { title: 'Synthesize', detail: 'merge + dedupe + prioritize gaps; assign each to a disjoint file-owner' },
    { title: 'Fix', detail: 'parallel fix agents (disjoint files) implement assigned gaps + the explicit user bugs' },
    { title: 'Integrate', detail: 'authoritative green build (both profiles) + tests + commit' },
    { title: 'Verify', detail: 'headless screenshots of the headline fixes + spot-checks -> punch-list' },
    { title: 'Polish', detail: 'fix punch-list per owner in parallel' },
    { title: 'Final', detail: 'green build, final screenshots, commit, PARITY REPORT' },
  ],
}

const OUT = '/home/loke/reclass-rs'
const QT = '/home/loke/Documents/Reclass/src'   // the REAL C++ Qt Reclass — authoritative behavior
const CREF = '/home/loke/Pictures/reclass_reference'
const LP = 'LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16'
const BUILD = `${LP} cargo build 2>&1 | grep -E '^error|^warning|Finished' | tail -40`

const GAP_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    domain: { type: 'string' },
    summary: { type: 'string' },
    gaps: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        properties: {
          feature: { type: 'string', description: 'the user-facing behavior, e.g. "click source chip opens Data Source picker"' },
          cpp_ref: { type: 'string', description: 'C++ file:symbol that defines the correct behavior' },
          rust_status: { type: 'string', enum: ['wired', 'partial', 'stub', 'missing', 'broken'] },
          fix_file: { type: 'string', description: 'the Rust file that should own the fix (e.g. src/ui/editor/mod.rs)' },
          fix_hint: { type: 'string' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
          user_reported: { type: 'boolean' },
        },
        required: ['feature', 'rust_status', 'fix_file', 'fix_hint', 'severity'],
      },
    },
  },
  required: ['domain', 'summary', 'gaps'],
}

// ── PHASE 1: AUDIT (6 parallel, read-only) ──────────────────────────────────
const AUDIT_HEADER =
  `You are auditing FEATURE PARITY of the Rust+GPUI port of "Reclass" (at ${OUT}/src) against the AUTHORITATIVE C++ Qt implementation (at ${QT}). The user reports "a lot of stuff isn't wired up — you keep missing stuff." Your job: read the C++ for your domain, read the corresponding Rust, and produce an EXHAUSTIVE, HONEST gap list — every user-facing behavior the C++ has, and whether the Rust port has it WIRED / PARTIAL / STUB / MISSING / BROKEN.\n\n` +
  `Be thorough and skeptical: "the menu item exists" is NOT "wired" — trace whether clicking it actually does the C++ thing. "the popup renders" is NOT "wired" — trace whether the trigger opens it and the result is applied. Read the C++ slot/handler, then find the Rust equivalent and verify the wire end-to-end. Prefer reading actual code over assuming. READ-ONLY — do not edit.\n\n` +
  `For each gap: feature (the behavior), cpp_ref (C++ file:symbol), rust_status, fix_file (the Rust file that should own the fix), fix_hint (what to change), severity, user_reported (true if it's one of the explicitly-reported bugs below).\n\n` +
  `EXPLICITLY USER-REPORTED BUGS (flag any you find in your domain with user_reported=true):\n` +
  `  • Clicking the class-header SOURCE chip ("source▾") does nothing/wrong — should open the DATA SOURCE picker (${CREF} image: Data Source popup — Filter sources, SAVED SOURCES with the active source checked, PROVIDERS: Open File / hyper-reV Memory / Kernel Memory / Process Memory / ReClass.NET Compat / Remote Process Memory / WinDbg Memory). C++: sourcechooserpopup.{h,cpp} + how editor.cpp opens it.\n` +
  `  • Clicking the header CHEVRON ("[▸]", just left of source) does nothing/wrong — should open the TYPE SELECTOR popup (${CREF} image: "Filter N structs", Hex/Int/Float/Ptr tabs, Recent/Type lists, +New, OK). C++: typeselectorpopup.{h,cpp}.\n` +
  `  • HOVER over the source chip does nothing — C++ shows a hover affordance/tooltip.\n` +
  `  • The inline-edit FIELD is horizontally OFFSET (the edit box appears to the right of the text it edits — e.g. the root class-name box lands after the "{"). C++ editor.cpp computes the edit-field geometry exactly; the Rust overlay column math is wrong.\n` +
  `  • File ▸ Examples ▸ <name> does NOTHING (should open the bundled example project).\n\n`

const AUDIT_DOMAINS = [
  {
    key: 'menus',
    cpp: ['mainwindow.h', 'commandpalette.h', 'main.cpp'],
    rust: ['src/ui/window.rs', 'src/ui/menubar.rs', 'src/ui/commandpalette.rs', 'src/ui/examples.rs'],
    focus: `MENUS & ACTIONS. Enumerate EVERY menu item the C++ builds (File/Edit/View/Tools/Plugins/Help, incl. submenus Import/Export/Examples/Data Source/Recent Files/Font/Theme) and EVERY keyboard shortcut. For each, find the C++ slot it triggers and verify the Rust run_menu_command (window.rs) actually does the equivalent (not a logged no-op). Trace File>Examples end-to-end (why does it "do nothing"? write_example_to_temp + open_project — is the example .rcx valid / does the fly-out leaf actually dispatch MenuCommand?). Cover Edit (undo/redo/cut/copy/paste/delete/select-all), View toggles (and whether they actually change rendering), Tools, Plugins, Help. Also the Data Source submenu -> source switching.`,
  },
  {
    key: 'editor',
    cpp: ['editor.cpp', 'editor.h'],
    rust: ['src/ui/editor/mod.rs', 'src/ui/editor/element.rs', 'src/ui/editor/hit_test.rs', 'src/ui/editor/inline_edit.rs', 'src/ui/editor/geometry.rs', 'src/ui/editor/selection.rs', 'src/ui/editor/tab_cycle.rs'],
    focus: `THE EDITOR SURFACE — the richest domain. Diff EVERY interaction in editor.cpp: mousePressEvent / mouseDoubleClickEvent / mouseMoveEvent (hover) / contextMenuEvent / keyPressEvent / wheel / drag-reorder. Command-row spans: chevron->TypeSelector popup, source->SourceChooser popup, address->BaseAddress edit, root type/name->edit (what opens for EACH, popup vs inline). Node rows: type/name/value edit, the node context menu (every item), fold/collapse, byte selection + drag, hex overwrite typing, pointer-follow, array index nav. HOVER popups: value-history, struct-preview, disasm/hex-dump (rcxtooltip). The inline-edit FIELD GEOMETRY: how editor.cpp positions the edit widget over the span (the exact x/width) vs the Rust overlay 'left' calc (the reported offset bug — give the precise C++ formula). Keyboard: P/F/S/U quick-type, F2 rename, T change-type, Ctrl+D dup, Del, Tab-cycle, arrows. Be exhaustive.`,
  },
  {
    key: 'pickers',
    cpp: ['sourcechooserpopup.h', 'sourcechooserpopup.cpp', 'typeselectorpopup.h', 'typeselectorpopup.cpp', 'hextoolbarpopup.h', 'hextoolbarpopup.cpp', 'commandpalette.h'],
    rust: ['src/ui/sourcechooser.rs', 'src/ui/typeselectorpopup.rs', 'src/ui/hextoolbar.rs', 'src/ui/enumpicker.rs', 'src/ui/commandpalette.rs', 'src/ui/contextmenu.rs', 'src/ui/tooltip.rs'],
    focus: `FLOATING PICKERS/POPUPS. For the SOURCE CHOOSER (sourcechooserpopup): what it lists (saved sources w/ active checkmark + the provider plugins), what selecting one DOES (switches the document's data source), and how it is TRIGGERED (the editor source-chip click). For the TYPE SELECTOR (typeselectorpopup): the filter tabs/groups/size bars/modifiers/+New/OK and what choosing one does (changes the node/root type), how it is TRIGGERED (the chevron click + Change Type + T). Hex toolbar popup, enum picker, command palette, context menu, hover tooltips. For EACH popup: does the Rust expose a view + event the editor/host can open + consume? Is the TRIGGER wired? Is the RESULT applied? This is where the source/chevron user bugs live — specify the exact open API + apply path.`,
  },
  {
    key: 'dialogs',
    cpp: ['optionsdialog.h', 'optionsdialog.cpp', 'gotoaddressdialog.h', 'processpicker.h', 'processpicker.cpp', 'profilerdialog.h', 'profilerdialog.cpp'],
    rust: ['src/ui/optionsdialog.rs', 'src/ui/gotoaddress.rs', 'src/ui/processpicker.rs', 'src/ui/findbar.rs', 'src/ui/messagebox.rs', 'src/ui/dialogs.rs'],
    focus: `MODAL DIALOGS. Options/settings dialog (every section + setting + whether it persists/applies), goto-address dialog (parse + navigate), process picker (list processes + attach — note if the live process backend is stubbed), profiler dialog, find bar. For each: is it reachable from its trigger, does it render the C++ content, and does OK/apply actually do the thing?`,
  },
  {
    key: 'docks',
    cpp: ['mainwindow.h', 'scannerpanel.h', 'scannerpanel.cpp', 'profiler.h', 'dock_tab_buttons.h', 'docksizereadout.h'],
    rust: ['src/ui/docks.rs', 'src/ui/panels.rs', 'src/ui/scannerpanel.rs', 'src/ui/modulespanel.rs', 'src/ui/bookmarkspanel.rs'],
    focus: `DOCKS & PANELS. Scanner panel (controls + scan execution over a provider — what's wired vs the "no data source" stub; Next-Scan/Undo-Scan multi-pass narrowing the C++ has), Modules/Symbols/Types panel (Download All + lists), Bookmarks panel (add/remove/goto — controller has add_bookmark/remove_bookmark), profiler, dock toolbars + size readout + tab buttons. For each: does it toggle, render real content, and act?`,
  },
  {
    key: 'chrome',
    cpp: ['titlebar.cpp', 'titlebar.h', 'mainwindow.h', 'tab_source_icon.h'],
    rust: ['src/ui/titlebar.rs', 'src/ui/tabs.rs', 'src/ui/workspace.rs', 'src/ui/startpage.rs', 'src/ui/statusbar.rs'],
    focus: `CHROME. Titlebar (menu/title/toggles/window controls), document tabs (new/close/reorder/source-icon/dirty/context menu), workspace project tree (every row action: open/rename/delete/duplicate/add, filter, context menu, drag), status bar (the live node readout + segments + key hints), start page (every card + recent + examples). For each: wired or no-op? The workspace tree mutation actions (rename/delete/duplicate) were noted as stubs — confirm against the C++.`,
  },
]

phase('Audit')
log(`Auditing ${AUDIT_DOMAINS.length} domains against the C++ Qt source in parallel.`)
const audits = await parallel(AUDIT_DOMAINS.map(d => () =>
  agent(
    AUDIT_HEADER +
    `YOUR DOMAIN: ${d.key}. ${d.focus}\n\n` +
    `READ the C++: ${d.cpp.map(f => `${QT}/${f}`).join(', ')} (+ anything they reference).\n` +
    `READ the Rust: ${d.rust.map(f => `${OUT}/${f}`).join(', ')}.\n` +
    `Also the reclass reference images in ${CREF}/*.png where relevant.\n\n` +
    `Produce the structured gap list (be exhaustive — aim for completeness over brevity; missing a gap is the failure mode we are fixing).`,
    { label: `audit:${d.key}`, phase: 'Audit', schema: GAP_SCHEMA },
  )
))
const allGaps = audits.filter(Boolean).flatMap(a => (a.gaps || []).map(g => ({ ...g, domain: a.domain })))
log(`Audit found ${allGaps.length} gaps total (${allGaps.filter(g => g.severity !== 'minor').length} blocker/major, ${allGaps.filter(g => g.user_reported).length} user-reported).`)

// ── PHASE 2: SYNTHESIZE — assign each gap to a disjoint file-owner ───────────
phase('Synthesize')
// Stable disjoint file-owner groups (every src/ui file belongs to exactly one).
const OWNERS = {
  editor: ['src/ui/editor/'],
  menus_wiring: ['src/ui/window.rs', 'src/ui/menubar.rs', 'src/ui/commandpalette.rs', 'src/ui/examples.rs'],
  pickers: ['src/ui/sourcechooser.rs', 'src/ui/typeselectorpopup.rs', 'src/ui/hextoolbar.rs', 'src/ui/enumpicker.rs', 'src/ui/contextmenu.rs', 'src/ui/tooltip.rs'],
  dialogs: ['src/ui/optionsdialog.rs', 'src/ui/gotoaddress.rs', 'src/ui/processpicker.rs', 'src/ui/findbar.rs', 'src/ui/messagebox.rs', 'src/ui/dialogs.rs'],
  docks_panels: ['src/ui/docks.rs', 'src/ui/panels.rs', 'src/ui/scannerpanel.rs', 'src/ui/modulespanel.rs', 'src/ui/bookmarkspanel.rs'],
  chrome: ['src/ui/titlebar.rs', 'src/ui/tabs.rs', 'src/ui/workspace.rs', 'src/ui/startpage.rs', 'src/ui/statusbar.rs'],
}
const ownerOf = (file) => {
  for (const [owner, prefixes] of Object.entries(OWNERS)) {
    if (prefixes.some(p => (file || '').includes(p) || (file || '').startsWith(p))) return owner
  }
  return 'menus_wiring' // default bucket for cross-cutting/window wiring
}
const synth = await agent(
  `You are the synthesis step of a C++-vs-Rust parity audit for the Reclass GPUI port. Here are ${allGaps.length} raw gaps from 6 domain auditors:\n\n` +
  JSON.stringify(allGaps.map((g, i) => ({ i, feature: g.feature, status: g.rust_status, fix_file: g.fix_file, severity: g.severity, user: !!g.user_reported, hint: (g.fix_hint || '').slice(0, 160) }))) +
  `\n\nDE-DUPLICATE (same underlying defect reported by multiple auditors → one item), drop anything already actually wired (status=wired with no real defect), and PRIORITIZE (user_reported + blocker first). Keep partial/stub/missing/broken. Return the cleaned, prioritized list; for each item include the SAME fields plus the owner bucket it belongs to (one of: editor, menus_wiring, pickers, dialogs, docks_panels, chrome) based on its fix_file. Preserve user_reported.`,
  {
    label: 'synthesize', phase: 'Synthesize',
    schema: {
      type: 'object', additionalProperties: false,
      properties: {
        items: { type: 'array', items: { type: 'object', additionalProperties: false, properties: {
          feature: { type: 'string' }, owner: { type: 'string', enum: ['editor', 'menus_wiring', 'pickers', 'dialogs', 'docks_panels', 'chrome'] },
          fix_file: { type: 'string' }, fix_hint: { type: 'string' }, cpp_ref: { type: 'string' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, user_reported: { type: 'boolean' },
        }, required: ['feature', 'owner', 'fix_hint', 'severity'] } },
        overview: { type: 'string' },
      },
      required: ['items', 'overview'],
    },
  })
const items = (synth && synth.items) || allGaps.map(g => ({ ...g, owner: ownerOf(g.fix_file) }))
log(`Synthesized ${items.length} actionable items. ${synth ? synth.overview : ''}`)

// ── PHASE 3: FIX (parallel by owner, disjoint files) ────────────────────────
phase('Fix')
const FIX_FILES = {
  editor: 'src/ui/editor/*',
  menus_wiring: 'src/ui/window.rs, src/ui/menubar.rs, src/ui/commandpalette.rs, src/ui/examples.rs',
  pickers: 'src/ui/sourcechooser.rs, src/ui/typeselectorpopup.rs, src/ui/hextoolbar.rs, src/ui/enumpicker.rs, src/ui/contextmenu.rs, src/ui/tooltip.rs',
  dialogs: 'src/ui/optionsdialog.rs, src/ui/gotoaddress.rs, src/ui/processpicker.rs, src/ui/findbar.rs, src/ui/messagebox.rs, src/ui/dialogs.rs',
  docks_panels: 'src/ui/docks.rs, src/ui/panels.rs, src/ui/scannerpanel.rs, src/ui/modulespanel.rs, src/ui/bookmarkspanel.rs',
  chrome: 'src/ui/titlebar.rs, src/ui/tabs.rs, src/ui/workspace.rs, src/ui/startpage.rs, src/ui/statusbar.rs',
}
const FIX_CPP = {
  editor: `${QT}/editor.cpp, ${QT}/editor.h`,
  menus_wiring: `${QT}/mainwindow.h, ${QT}/main.cpp, ${QT}/commandpalette.h`,
  pickers: `${QT}/sourcechooserpopup.cpp, ${QT}/typeselectorpopup.cpp, ${QT}/hextoolbarpopup.cpp`,
  dialogs: `${QT}/optionsdialog.cpp, ${QT}/gotoaddressdialog.h, ${QT}/processpicker.cpp`,
  docks_panels: `${QT}/scannerpanel.cpp, ${QT}/mainwindow.h, ${QT}/profiler.cpp`,
  chrome: `${QT}/titlebar.cpp, ${QT}/mainwindow.h`,
}
// Cross-agent CONTRACT: editor opens the pickers; pickers provide the open API + apply path.
const PICKER_CONTRACT =
  `CONTRACT (pickers PROVIDE, editor CONSUMES — for the source-chip + chevron user bugs):\n` +
  `  • SourceChooser: src/ui/sourcechooser.rs exposes a popup view + event so the editor can open it anchored under the source chip and apply the choice. Shape: \`SourceChooserPopup::view(window, cx) -> Entity<_>\` + \`enum SourceChooserEvent { Pick(<source/provider descriptor>), OpenFile, Clear, Cancel }\`. The editor opens it on a Source-span click and applies the pick through the controller/document data-source API (add a tiny pub accessor if required; mirror C++ sourcechooserpopup behavior).\n` +
  `  • TypeSelector: src/ui/typeselectorpopup.rs already has \`TypeSelectorPopup::view(current, window, cx)\` + \`TypeSelectorEvent::{Chosen{kind,modifier},Cancel}\` (round 3). The editor opens it on a chevron/TypeSelector-span click + Change-Type/T and applies via change_node_kind / the root-type change. Keep these signatures stable.\n` +
  `  • Both popups: Zed-styled elevated surfaces matching the reference images.\n`
const groups = {}
for (const it of items) (groups[it.owner] = groups[it.owner] || []).push(it)
const fixOwners = Object.keys(groups).filter(o => groups[o].length > 0)
log(`Fixing ${fixOwners.length} owner groups: ${fixOwners.map(o => `${o}(${groups[o].length})`).join(' ')}`)
const fixes = await parallel(fixOwners.map(owner => () =>
  agent(
    `You are wiring up missing/broken FEATURES in the Reclass GPUI port (${OUT}) to reach parity with the C++ Qt Reclass (${QT}). The logic layer is implemented — WIRE it; do NOT change logic modules (tiny pub accessor at most; say so). Aesthetic = ZED; use ${OUT}/src/ui/design.rs tokens + design::icon_* + ${OUT}/_design/zed_ui_spec.md.\n\n` +
    `YOUR OWNERSHIP (edit ONLY these): ${FIX_FILES[owner] || 'src/ui/*'}. Do NOT edit src/ui/mod.rs, logic modules, or other owners' files. Honor the CONTRACT below verbatim. Do NOT run git/cargo fmt (a later stage does). Keep existing #[cfg(test)] tests; add tests for pure helpers.\n\n` +
    `AUTHORITATIVE C++ for your domain: ${FIX_CPP[owner] || `${QT}/`}. READ it to implement the EXACT behavior; the reference images are in ${CREF}/*.png.\n\n` +
    (owner === 'editor' || owner === 'pickers' ? PICKER_CONTRACT + '\n' : '') +
    `BUILD (fast incremental): \`${BUILD}\`. Other agents edit other files; ignore errors localized to files you don't own.\n\n` +
    `IMPLEMENT these audited parity gaps (fix EACH; they are real defects found by diffing the C++):\n` +
    groups[owner].map((it, i) => `  ${i + 1}. [${it.severity}${it.user_reported ? ', USER-REPORTED' : ''}] ${it.feature}\n     cpp: ${it.cpp_ref || '(see your C++ files)'} — fix: ${it.fix_hint}`).join('\n') +
    `\n\nMake each behavior actually WORK end-to-end (trigger → action → result applied), matching the C++. Ensure your files compile and return the structured result.`,
    {
      label: `fix:${owner}`, phase: 'Fix',
      schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, fixed: { type: 'array', items: { type: 'string' } }, deferred: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] },
    },
  )
))
log(`Fix done: ${fixes.filter(Boolean).map((r, i) => `${fixOwners[i]}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 4: INTEGRATE ──────────────────────────────────────────────────────
phase('Integrate')
const integrate = await agent(
  `Parity-wiring fixes just landed across the Reclass GPUI port (${OUT}) from ${fixOwners.length} owner groups (editor interactions + source/type popups, menu action wiring + examples, dialogs, docks/panels, chrome). Make the FULL build authoritative-green + consistent.\n` +
  `Fix notes: ${JSON.stringify(fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], ok: r.compiles, notes: (r.notes || '').slice(0, 200) })))}\n\n` +
  `DO: 1) \`${BUILD}\` — fix ALL errors/warnings (seams: the SourceChooser/TypeSelector open contract editor⇄pickers; controller accessors; menu-id handlers). 2) Both builds: ui + \`cargo build --no-default-features 2>&1|tail -3\`. 3) Logic tests: \`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1 | tail -8\` + UI tests \`${LP} cargo test --lib ui:: 2>&1 | tail -6\`. 4) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: parity pass — wire source/type pickers, examples, menu actions, editor interactions, docks (C++-audited)"\`. Report green + committed=<hash> + contract mismatches fixed.`,
  { label: 'integrate', phase: 'Integrate', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['green', 'notes'] } })
log(`Integrate: ${integrate ? (integrate.green ? 'GREEN' : 'NOT green') : 'FAILED'}${integrate && integrate.committed ? ' @' + integrate.committed : ''}`)

// ── PHASE 5: VERIFY (serial, display) ───────────────────────────────────────
phase('Verify')
const VERIFY_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    shots: { type: 'array', items: { type: 'string' } },
    findings: { type: 'array', items: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string', enum: ['editor', 'menus_wiring', 'pickers', 'dialogs', 'docks_panels', 'chrome'] }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] }, issue: { type: 'string' }, fix_hint: { type: 'string' } }, required: ['owner', 'severity', 'issue', 'fix_hint'] } },
    overall: { type: 'string' },
  },
  required: ['shots', 'findings', 'overall'],
}
const verify = await agent(
  `VISUAL/FUNCTIONAL QA for the Reclass GPUI parity pass. You OWN the headless display. Build is green. Verify the HEADLINE user-reported fixes actually work, vs the C++ reference images ${CREF}/*.png.\n\n` +
  `HARNESS from ${OUT}: \`./scripts/ui.sh start /home/loke/reclass-cpp/src/examples/png.rcx\` ("app up"). Maximize: \`export DISPLAY=:99; WID=$(xdotool search --name Reclass|head -1); wmctrl -i -r "$WID" -b add,maximized_vert,maximized_horz 2>/dev/null; xdotool windowmove "$WID" 0 0; sleep 1\`. \`./scripts/ui.sh shot /tmp/p_X.png\`; CROP with \`convert IN -crop WxH+X+Y OUT\` then Read. Drive: \`./scripts/ui.sh key/click/type\`, right-click \`xdotool mousemove X Y click 3\`.\n\n` +
  `VERIFY (find coords from a full shot first):\n` +
  `  1. /tmp/p_source.png — click the class-header SOURCE chip ("source▾") → the Data Source picker opens (saved sources + providers). Compare to the C++ Data Source image. Escape.\n` +
  `  2. /tmp/p_chevron.png — click the header CHEVRON ("[▸]" left of source) → the Type Selector popup opens (filter tabs, struct/enum list, +New, OK). Escape.\n` +
  `  3. /tmp/p_inline.png — click a TYPE token on a node row (select then click) and the root class NAME → the inline-edit box lands DIRECTLY OVER the text (no horizontal offset / not after the "{"). Crop tight.\n` +
  `  4. /tmp/p_example.png — File ▸ Examples ▸ <pick EPROCESS> → the example LOADS (tree + editor change). \n` +
  `  5. /tmp/p_menus.png — spot-check 3-4 other menu actions that the audit said were newly wired (e.g. a View toggle changes rendering; an Edit action works) — confirm they DO something.\n` +
  `Be a harsh end-to-end critic — "opens" must mean the action completes, not just a flash. Return: shots; findings tagged owner∈{editor,menus_wiring,pickers,dialogs,docks_panels,chrome}, severity, issue, fix_hint; overall verdict (esp the 5 headline fixes). \`./scripts/ui.sh stop\` when done.`,
  { label: 'verify', phase: 'Verify', schema: VERIFY_SCHEMA })
const findings = (verify && verify.findings) || []
log(`Verify: ${findings.length} findings (${findings.filter(f => f.severity !== 'minor').length} blocker/major). ${verify ? verify.overall : ''}`)

// ── PHASE 6: POLISH (parallel by owner with findings) ───────────────────────
phase('Polish')
const byOwner = {}
for (const f of findings) (byOwner[f.owner] = byOwner[f.owner] || []).push(f)
const polishOwners = Object.keys(byOwner).filter(o => byOwner[o].length > 0)
log(`Polishing ${polishOwners.length}: ${polishOwners.map(o => `${o}(${byOwner[o].length})`).join(' ')}`)
const polish = await parallel(polishOwners.map(owner => () =>
  agent(
    `POLISH pass for the Reclass GPUI parity work (${OUT}). YOUR OWNERSHIP (edit ONLY): ${FIX_FILES[owner] || 'src/ui/*'}. Authoritative C++: ${FIX_CPP[owner] || QT}. Aesthetic = ZED (design.rs tokens). Do NOT run git/cargo fmt; do NOT touch other owners' files.\n` +
    (owner === 'editor' || owner === 'pickers' ? PICKER_CONTRACT + '\n' : '') +
    `Functional QA filed these from real screenshots — FIX EACH end-to-end:\n` +
    byOwner[owner].map((f, i) => `  ${i + 1}. [${f.severity}] ${f.issue}\n     hint: ${f.fix_hint}`).join('\n') +
    `\n\nEnsure your files compile (\`${BUILD}\`). Return the structured result.`,
    { label: `polish:${owner}`, phase: 'Polish', schema: { type: 'object', additionalProperties: false, properties: { owner: { type: 'string' }, compiles: { type: 'boolean' }, fixed: { type: 'array', items: { type: 'string' } }, notes: { type: 'string' } }, required: ['compiles', 'notes'] } },
  )
))
log(`Polish done: ${polish.filter(Boolean).map((r, i) => `${polishOwners[i]}${r.compiles ? '✓' : '✗'}`).join('  ')}`)

// ── PHASE 7: FINAL + PARITY REPORT ──────────────────────────────────────────
phase('Final')
const final = await agent(
  `Final gate for the Reclass GPUI parity pass (${OUT}).\n` +
  `DO: 1) \`${BUILD}\` FULL GREEN + \`cargo build --no-default-features 2>&1|tail -3\` + tests (\`cargo test --no-default-features --features disasm,symbols,imports,mcp 2>&1|tail -6\` and \`${LP} cargo test --lib ui:: 2>&1|tail -4\`). 2) \`cargo fmt\`, commit: \`cd ${OUT} && git add -A && git commit -m "ui: parity polish — final wiring + QA fixes"\`. 3) FINAL SCREENSHOTS (you own display): start with png.rcx, maximize, capture: /tmp/pfinal_source.png (Data Source picker open), /tmp/pfinal_chevron.png (Type Selector open), /tmp/pfinal_inline.png (inline edit aligned on text), /tmp/pfinal_example.png (an opened example). Read each. \`./scripts/ui.sh stop\`.\n` +
  `4) PARITY REPORT: of all the audited gaps, which are now WIRED, which remain PARTIAL/DEFERRED (and why — e.g. live-process backend stub). Be honest. Return green, committed, shots, remaining[], assessment.`,
  { label: 'final', phase: 'Final', schema: { type: 'object', additionalProperties: false, properties: { green: { type: 'boolean' }, committed: { type: 'string' }, shots: { type: 'array', items: { type: 'string' } }, remaining: { type: 'array', items: { type: 'string' } }, assessment: { type: 'string' } }, required: ['green', 'shots', 'assessment'] } })
log(`Final: ${final ? (final.green ? 'GREEN' : 'NOT green') : 'FAILED'}${final && final.committed ? ' @' + final.committed : ''}`)

return {
  audit: { total_gaps: allGaps.length, by_domain: audits.filter(Boolean).map(a => ({ domain: a.domain, gaps: (a.gaps || []).length, summary: (a.summary || '').slice(0, 160) })) },
  synthesized: items.length,
  fixes: fixes.filter(Boolean).map((r, i) => ({ owner: fixOwners[i], compiles: r.compiles, fixed: (r.fixed || []).length })),
  integrate: integrate && { green: integrate.green, committed: integrate.committed },
  verify: verify && { findings: findings.length, overall: verify.overall, shots: verify.shots },
  final: final && { green: final.green, committed: final.committed, shots: final.shots, remaining: final.remaining, assessment: final.assessment },
}
