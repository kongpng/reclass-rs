export const meta = {
  name: 'reclass-rs-foundation',
  description: 'Foundation for a faithful Rust+GPUI port of Reclass: reverse-engineer every subsystem, research GPUI + crates, design the crate architecture, scaffold a compiling workspace, capture golden oracle test outputs, and write per-subsystem porting specs.',
  whenToUse: 'Phase 1 of porting the C++/Qt Reclass binary editor to Rust+GPUI.',
  phases: [
    { title: 'Understand', detail: 'parallel readers, one per C++ subsystem -> structured behavioral maps' },
    { title: 'Research', detail: 'GPUI cookbook (with a real probe-build) + Rust crate scouting' },
    { title: 'Oracle', detail: 'build & run the original C++ logic tests, capture golden outputs' },
    { title: 'Design', detail: 'synthesize the cross-platform Rust crate architecture' },
    { title: 'Scaffold', detail: 'create a compiling Cargo workspace skeleton (reclass-core ported)' },
    { title: 'Specs', detail: 'per-subsystem function-level porting specs' },
  ],
}

const SRC = '/home/loke/reclass-cpp'        // stable copy of the C++ source (no .git)
const OUT = '/home/loke/reclass-rs'         // the Rust port + design artifacts

const subsystems = [
  { key: 'core-model',      title: 'Core node/type model',                         port: 'pure',
    files: 'src/core.h, src/commontypes.h, src/typeinfer.h',
    hint: 'The central data model: node tree, field kinds/types. Qt containers (QString/QVector/QHash/QJsonObject/QByteArray) map to std String/Vec/HashMap/serde_json::Value/Vec<u8>.' },
  { key: 'compose-undo',    title: 'Composition + undo/redo command stack',        port: 'pure',
    files: 'src/compose.cpp, src/core.h',
    hint: '15 undoable command types (ChangeKind, Rename, Collapse, Insert, Remove, ChangeBase, WriteBytes, ChangeArrayMeta, ChangePointerRef, ChangeStructTypeName, ChangeClassKeyword, ChangeOffset, ChangeEnumMembers, ChangeOffsetExpr, ToggleStatic) + batch macros. Pure logic — heavily tested in test_compose.cpp.' },
  { key: 'format-render',   title: 'Field formatting / line rendering',            port: 'pure',
    files: 'src/format.cpp, src/commontypes.h',
    hint: 'Turns nodes into the textual rows shown in the editor (columns, hex/ascii). Pure; tested in test_format.cpp.' },
  { key: 'addressparser',   title: 'Address expression parser',                    port: 'pure',
    files: 'src/addressparser.cpp, src/addressparser.h',
    hint: 'Evaluates expressions like <module.exe>+0x1A0, pointer-deref chains, symbol resolution. Pure; tested in test_addressparser.cpp.' },
  { key: 'rtti-symbols',    title: 'RTTI + symbol store + name providers',         port: 'mostly-portable',
    files: 'src/rtti.cpp, src/rtti.h, src/symbolstore.cpp, src/symbolstore.h, src/symbol_downloader.cpp, src/symbol_downloader.h, src/names/',
    hint: 'RTTI parsing, symbol storage/resolution, C++ name demangling (MSVC + Itanium), symbol-server download. Use cpp_demangle/msvc-demangler/symbolic, reqwest. Tested in test_rtti.cpp, test_rtti_hint.cpp.' },
  { key: 'scanner',         title: 'Value/pattern search engine over byte regions', port: 'pure',
    files: 'src/scanner.cpp, src/scanner.h',
    hint: 'A generic search engine that scans byte buffers/regions (supplied by the abstract data source) for values and patterns — the pure search/compare/aggregation ALGORITHMS over in-memory byte slices (int/float/string/array-of-bytes matching, comparison operators, refine/next-scan, result sets). Port the algorithms operating on &[u8]. Tested in test_scanner.cpp, test_scanner_combinations.cpp.' },
  { key: 'generator',       title: 'C/C++ source generator',                       port: 'pure',
    files: 'src/generator.cpp, src/generator.h',
    hint: 'Emits a C/C++ header from the node tree (optional static asserts). Pure; tested in test_generator.cpp (1760 lines of assertions).' },
  { key: 'providers',       title: 'Data-source abstraction (Provider trait) + registry + built-in file/buffer sources', port: 'mostly-portable',
    files: 'src/providers/buffer_provider.h, src/providers/null_provider.h, src/providers/provider.h, src/providers/snapshot_provider.h, src/providerregistry.cpp, src/providerregistry.h, src/iplugin.h (interface signatures only)',
    hint: 'The abstract data-source interface (read/write bytes at an offset, query regions/modules) and its BENIGN built-in implementations only: buffer (in-memory byte array), file, snapshot, null. Define a Rust Provider trait and port these built-in providers + the registry. The live OS process/kernel/remote/WinDbg sources are the OUT-OF-SCOPE plugins/ subsystem; the trait simply leaves room for them as future impls. Tested in test_provider.cpp, test_source_provider.cpp, test_source_management.cpp.' },
  { key: 'imports',         title: 'Importers/exporters (RCX JSON, C/C++, ReClass XML, PDB, PE)', port: 'mostly-portable',
    files: 'src/imports/import_source.cpp, src/imports/import_source.h, src/imports/import_reclass_xml.cpp, src/imports/import_reclass_xml.h, src/imports/export_reclass_xml.cpp, src/imports/export_reclass_xml.h, src/imports/import_pdb.cpp, src/imports/import_pdb.h, src/imports/pe_debug_info.cpp, src/imports/pe_debug_info.h',
    hint: 'RCX native JSON (serde_json), C/C++ source parse (tree-sitter-cpp or handroll), ReClass XML (quick-xml), PDB (pdb crate, replacing raw_pdb), PE debug dirs (object/goblin). Tested in test_import_source.cpp, test_import_xml.cpp, test_export_xml.cpp, test_import_pdb.cpp, test_roundtrip_winsdk.cpp.' },
  { key: 'disasm',          title: 'Disassembler integration',                     port: 'mostly-portable',
    files: 'src/disasm.cpp, src/disasm.h',
    hint: 'x86/x64 disassembly — replace the fadec submodule with iced-x86. Tested in test_disasm.cpp.' },
  { key: 'mcp',             title: 'MCP bridge (JSON-RPC 2.0 over named pipe)',    port: 'mostly-portable',
    files: 'src/mcp/mcp_bridge.cpp, src/mcp/mcp_bridge.h, tools/rcx-mcp-stdio.cpp',
    hint: 'Model Context Protocol server: a JSON-RPC 2.0 transport over a local named pipe, plus a stdio<->pipe bridge binary. Port the protocol/transport and the tool & notification SCHEMAS: projectState, treeApply, sourceSwitch, hexRead/hexWrite (read/write bytes of the active data source via the Provider trait), statusSet, uiAction, treeSearch, nodeHistory; notifications notifyTreeChanged/notifyDataChanged. Focus on the JSON-RPC message formats and tool schemas. Use serde_json + interprocess. Tested in test_mcp.cpp.' },
  { key: 'themes',          title: 'Theme system',                                 port: 'mostly-portable',
    files: 'src/themes/theme.cpp, src/themes/theme.h, src/themes/thememanager.cpp, src/themes/thememanager.h, src/themes/themeeditor.cpp, src/themes/themeeditor.h, src/themes/defaults/',
    hint: 'Theme model + manager + (GUI) editor + default themes. Model is serde; editor maps to GPUI. Tested in test_theme.cpp.' },
  { key: 'editor-surface',  title: 'Editor surface (QScintilla text grid)',        port: 'ui-heavy',
    files: 'src/editor.cpp, src/editor.h',
    hint: 'THE central UI: the structured-binary view rendered as formatted plain text with inline editing, tab-cycling, multi-select, split views, find bar, fold markers, hex/ASCII columns, per-byte change highlighting. Built on QScintilla. Reimplement in GPUI as a virtualized list of styled-text rows with interactive inline-editable regions + hit-testing. Hardest UI piece. Tested in test_editor.cpp (3531 lines), test_rendered_view.cpp, test_byte_selection*.cpp.' },
  { key: 'controller',      title: 'Editor controller / interaction logic',        port: 'mostly-portable',
    files: 'src/controller.cpp, src/controller.h',
    hint: 'Mediates the editor view and the document/data-source through the abstract Provider trait: the periodic refresh loop (default 660ms) that re-reads the current data source, the per-node value-history ring buffer + change-frequency heatmap, following pointer fields to read the referenced region (via the trait), applying user edits back through the trait, and selection state. Port as logic over the abstract data-source interface. Tested in test_controller.cpp, test_refresh_speedups.cpp.' },
  { key: 'app-shell',       title: 'App shell: main window, start page, titlebar, docks', port: 'ui-heavy',
    files: 'src/main.cpp, src/mainwindow.h, src/startpage.h, src/titlebar.cpp, src/titlebar.h, src/macos_titlebar.h, src/dockoverlay.h, src/docksizereadout.h, src/dock_tab_buttons.h',
    hint: 'Application entry, MDI multi-document tabs, dockable panels (workspace/scanner), custom titlebar, start page. Reimplement window/docking/tabs in GPUI. NOTE: main.cpp is 9627 lines — read it in sections.' },
  { key: 'widgets-dialogs', title: 'Widgets, dialogs, popups, panels',            port: 'ui-heavy',
    files: 'src/widgets/, src/optionsdialog.cpp, src/optionsdialog.h, src/processpicker.cpp, src/processpicker.h, src/gotoaddressdialog.h, src/commandpalette.h, src/typeselectorpopup.cpp, src/typeselectorpopup.h, src/sourcechooserpopup.cpp, src/sourcechooserpopup.h, src/hextoolbarpopup.cpp, src/hextoolbarpopup.h, src/rcxtooltip.h, src/rttibrowser.h, src/scannerpanel.cpp, src/scannerpanel.h, src/profilerdialog.cpp, src/profiler.cpp, src/clipboard.h, src/workspace_model.h, src/tooltip_bridge.h, src/tab_source_icon.h',
    hint: 'All themed dialogs, popups (type/enum/source pickers, command palette, hex toolbar), tooltips, scanner panel, options dialog, process picker, workspace model. Reimplement in GPUI; fuzzy_match -> nucleo. Many small tests (test_chips, test_command_palette, test_type_selector, test_options_dialog, test_tooltip*, test_goto_address, test_context_menu, test_overlay_*).' },
  { key: 'tests-catalog',   title: 'Test suite behavioral catalog (the fidelity oracle)', port: 'n/a',
    files: 'tests/ (ALL .cpp test files)',
    hint: 'Catalog EVERY test file: what behaviors/invariants it asserts, the public API it exercises, and which target Rust crate it maps to. This catalog is the fidelity contract for the whole port.' },
]

// ---------- schemas ----------
const UNDERSTAND_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    subsystem: { type: 'string' },
    purpose: { type: 'string' },
    key_types: { type: 'array', items: { type: 'object', additionalProperties: false,
      properties: { name: { type: 'string' }, kind: { type: 'string' }, summary: { type: 'string' }, cpp_location: { type: 'string' } },
      required: ['name', 'kind', 'summary'] } },
    public_api_count: { type: 'integer' },
    key_behaviors: { type: 'array', items: { type: 'string' } },
    qt_coupling: { type: 'string' },
    platform_specific: { type: 'string' },
    portability: { type: 'string', enum: ['pure', 'mostly-portable', 'ui-heavy', 'platform-specific', 'n/a'] },
    recommended_rust_crates: { type: 'array', items: { type: 'string' } },
    test_files: { type: 'array', items: { type: 'string' } },
    open_questions: { type: 'array', items: { type: 'string' } },
    report_path: { type: 'string' },
  },
  required: ['subsystem', 'purpose', 'key_types', 'portability', 'recommended_rust_crates', 'report_path'],
}

const GPUI_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    probe_build_succeeded: { type: 'boolean' },
    probe_build_notes: { type: 'string' },
    cargo_dependency_toml: { type: 'string' },
    minimal_main_rs: { type: 'string' },
    rust_toolchain_required: { type: 'string' },
    app_model: { type: 'string' },
    editor_surface_approach: { type: 'string' },
    list_virtualization: { type: 'string' },
    input_actions_focus: { type: 'string' },
    theming: { type: 'string' },
    docking_tabs_splits: { type: 'string' },
    popups_overlays_tooltips: { type: 'string' },
    gotchas: { type: 'array', items: { type: 'string' } },
    cookbook_path: { type: 'string' },
  },
  required: ['probe_build_succeeded', 'cargo_dependency_toml', 'editor_surface_approach', 'cookbook_path'],
}

const CRATE_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    selections: { type: 'array', items: { type: 'object', additionalProperties: false,
      properties: {
        need: { type: 'string' }, cpp_origin: { type: 'string' },
        crate: { type: 'string' }, version_or_git: { type: 'string' },
        verified_on_cratesio: { type: 'boolean' },
        rationale: { type: 'string' }, maturity: { type: 'string' },
        cross_platform: { type: 'string' }, license: { type: 'string' },
        alternatives: { type: 'array', items: { type: 'string' } },
      },
      required: ['need', 'crate', 'rationale'] } },
    table_path: { type: 'string' },
  },
  required: ['selections', 'table_path'],
}

const DESIGN_SCHEMA = {
  type: 'object', additionalProperties: false,
  properties: {
    crates: { type: 'array', items: { type: 'object', additionalProperties: false,
      properties: {
        name: { type: 'string' }, kind: { type: 'string', enum: ['lib', 'bin'] },
        purpose: { type: 'string' },
        depends_on_internal: { type: 'array', items: { type: 'string' } },
        external_crates: { type: 'array', items: { type: 'string' } },
        maps_cpp: { type: 'array', items: { type: 'string' } },
      },
      required: ['name', 'kind', 'purpose'] } },
    milestone_order: { type: 'array', items: { type: 'string' } },
    architecture_path: { type: 'string' },
    risks: { type: 'array', items: { type: 'string' } },
  },
  required: ['crates', 'milestone_order', 'architecture_path'],
}

// ---------- prompts ----------
const commonHeader =
  `You are helping PORT an existing, published open-source application from C++ to Rust. ` +
  `The application is "Reclass" (github.com/IChooseYou/Reclass, MIT-licensed) — an open-source structured-data editor and struct-layout inspector, a modern successor to ReClass.NET, used by software developers to view and annotate in-memory data layouts as typed structs/arrays/pointers while debugging. ` +
  `The COMPLETE original C++/Qt6 source is provided locally at ${SRC}. This is ordinary, faithful software-porting work on already-public source code: translating its data structures, file formats, public APIs, and UI into idiomatic Rust, reusing existing Rust crates where possible. The goal is 1:1 behavioral parity. The Rust port + all design docs live under ${OUT}. ` +
  `The app targets Windows primarily, plus macOS and Linux; only the Linux build can be compile-verified on this machine, so OS-specific code must stay behind #[cfg(...)] and keep compiling. ` +
  `Fidelity to the original public source is the priority; token/time cost is not a concern.\n\n` +
  `IDEMPOTENCY: this workflow may be re-run to fill gaps. Before doing work, check whether your designated output file already exists and is substantial; if so, read it, return its summary, and do NOT redo the analysis.\n\n` +
  `SCOPE NOTE (important): the live OS process/kernel/remote/WinDbg memory data-source plugins (everything under plugins/) are OUT OF SCOPE for this port. They are represented ONLY as documented stub implementations behind the data-source Provider trait. Do NOT analyze, design, or implement them. Implement only the benign built-in data sources: file, in-memory buffer, snapshot, and null. All other subsystems treat the data source purely through the abstract Provider trait.\n\n`

function understandPrompt(s) {
  const isTests = s.key === 'tests-catalog'
  return commonHeader +
    `IDEMPOTENCY: if ${OUT}/_design/understand/${s.key}.md already exists and is >3KB, read it and just return its structured summary — do not redo the work.\n\n` +
    `TASK: Analyze ONE subsystem of this public open-source codebase and produce a precise structural/behavioral map a Rust implementer can follow WITHOUT re-reading the C++. This is documentation of existing public code for a faithful port.\n\n` +
    `Subsystem: ${s.title}  (key: ${s.key}, expected portability: ${s.port})\n` +
    `Files to study (read them FULLY; large files in sections; follow references into other files as needed): ${s.files}\n` +
    `Porting hint: ${s.hint}\n\n` +
    (isTests
      ? `Because this is the TEST CATALOG: read every .cpp file under ${SRC}/tests. For EACH test file, document: which subsystem/API it exercises, the concrete behaviors/invariants/edge-cases it asserts (be specific — these are the fidelity contract), notable fixtures/inputs, and which target Rust crate the test should be translated into. Note which tests are pure-logic (headless, no GUI) vs. GUI/offscreen-Qt.\n\n`
      : `Document: purpose; EVERY key type/struct/enum (fields + meaning); EVERY public function/method (signature, params, return, and EXACT behavior incl. edge cases, invariants, error handling); important algorithms step-by-step; data/serialization formats; how it uses Qt and the Rust std/crate equivalent for each Qt type; platform-specific code; concurrency/threading; and subtle behaviors the tests rely on. Cite C++ as file:line.\n\n`) +
    `Write a thorough markdown report to: ${OUT}/_design/understand/${s.key}.md\n` +
    `Then return the structured summary. Do NOT write any Rust yet. Be exhaustive and accurate.`
}

const gpuiPrompt = commonHeader +
  `IDEMPOTENCY: if ${OUT}/_design/gpui_cookbook.md already exists and is substantial, read it and return its summary instead of redoing the research/probe-build.\n\n` +
  `TASK: Produce a definitive GPUI cookbook for this port, and PROVE the dependency builds here. (GPUI is the Rust UI framework from the Zed editor; this is standard Rust GUI research.)\n\n` +
  `AUTHORITATIVE SOURCE — read the ACTUAL gpui source, do NOT rely on web docs or memory (the API changes fast and online docs are stale). Clone (or reuse if already at /tmp/zed_src) the Zed repo: \`git clone --depth 1 https://github.com/zed-industries/zed /tmp/zed_src\`. Treat /tmp/zed_src/crates/gpui as the SINGLE SOURCE OF TRUTH: read crates/gpui/src (App, Window, Context, View/Entity, Element/IntoElement, div/styling, text/TextLayout, uniform_list, list, interactivity, focus, actions, keymap) and especially crates/gpui/examples/* for real, current, copy-pasteable usage patterns. Also skim how Zed's own crates use gpui (e.g. crates/ui, crates/editor) for real-world patterns: virtualized lists, custom Elements, inline-editable text, overlays, and theming. Every code pattern in the cookbook must be traceable to a file:line in /tmp/zed_src. Pin the cookbook's recommended gpui dependency to the exact commit you read/built (\`git -C /tmp/zed_src rev-parse HEAD\`).\n\n` +
  `Environment: Linux, rustc/cargo nightly 1.97 (2026-04). GPUI build prereqs are installed (Vulkan, X11/XCB/XKB, Wayland, fontconfig/freetype). Target platforms: Windows is the PRIMARY target, plus macOS + Linux; we can only compile-verify the Linux build here, so also confirm gpui supports Windows and note any Windows-specific setup (DirectX/Blade backend, build deps).\n\n` +
  `1) PROBE BUILD (critical): GPUI ships from the Zed git repo (zed-industries/zed), not crates.io. Determine the correct git dependency and a revision/branch that compiles with our toolchain. Create a throwaway crate at /tmp/gpui_probe, add the gpui dependency, write a minimal hello-window example, and run \`cargo build\` (allow a long time — gpui is large). Report: did it build? exact working Cargo.toml dependency line(s)? minimal working main.rs? the rust-toolchain it needs (check zed's rust-toolchain.toml; if our nightly mismatches, pin a zed commit that matches or document the exact override)? Also note what's required to build gpui on WINDOWS (the primary target). If it fails, capture the exact error + the concrete fix. This de-risks the whole UI layer.\n` +
  `2) Document the GPUI model: App/Window/Context, Views/Entities, Elements & the render trait, styling (flex / tailwind-like helpers), text rendering, input + key bindings + Actions, focus handling.\n` +
  `3) CRITICAL for Reclass: how to render a LARGE VIRTUALIZED list of styled-text rows where each row has multiple interactive, inline-EDITABLE regions (click-to-edit fields) and per-span styling/coloring, with correct hit-testing and keyboard tab-cycling between fields. Study how Zed implements uniform_list, its editor element, and custom Element layout/paint/hit-test. The original Reclass editor surface is a QScintilla text grid — we must reproduce it faithfully in GPUI.\n` +
  `4) Theming (runtime-switchable), multi-pane/split views over one document, dockable panels + tabs, popups/overlays/tooltips/context-menus, modal dialogs, text input widgets, fuzzy-filtered pickers.\n\n` +
  `Ground EVERYTHING in /tmp/zed_src/crates/gpui (source + examples) as described above; use WebSearch/WebFetch only to supplement, never as the primary source. Write a copy-pasteable cookbook to ${OUT}/_design/gpui_cookbook.md, citing /tmp/zed_src file:line for each pattern, and return the structured summary.`

const cratePrompt = commonHeader +
  `IDEMPOTENCY: if ${OUT}/_design/crate_selection.md already exists and is substantial, read it and return its summary instead of redoing the research.\n\n` +
  `TASK: This is a standard Rust dependency-selection task for the port. Pick the best-maintained, cross-platform Rust crates so we hand-roll as little as possible. For EACH need below: choose a primary crate (exact name + version or git), verify it exists (cargo search / WebFetch crates.io), justify (maturity, downloads, maintenance recency, license, cross-platform support), and list alternatives. Read ${SRC} where you need to understand the exact requirement.\n\n` +
  `Needs (map to C++ origins):\n` +
  `- PDB parsing — replace third_party/raw_pdb + src/imports/import_pdb.cpp (try the \`pdb\` crate)\n` +
  `- PE / object files / debug directories — src/imports/pe_debug_info.cpp (object, goblin)\n` +
  `- x86/x64 disassembly — replace third_party/fadec + src/disasm.cpp (iced-x86, capstone)\n` +
  `- C++ name demangling, MSVC + Itanium — src/names/symbol_demangle.cpp (symbolic-demangle, cpp_demangle, msvc-demangler)\n` +
  `- C/C++ source parsing for import — src/imports/import_source.cpp (tree-sitter + tree-sitter-cpp, or clang, or advise hand-roll)\n` +
  `- XML import/export (ReClass XML) — src/imports/*reclass_xml* (quick-xml, roxmltree)\n` +
  `- JSON (.rcx native + MCP JSON-RPC) — serde, serde_json\n` +
  `- Named pipes / local IPC for the MCP bridge — src/mcp/* (interprocess)\n` +
  `- (Note: the live OS process/kernel/remote/WinDbg data-source plugins are OUT OF SCOPE — do not research crates for them.)\n` +
  `- Fuzzy matching — src/widgets/fuzzy_match.h, command palette, type picker (nucleo, fuzzy-matcher)\n` +
  `- Symbol-server download — src/symbol_downloader.cpp (reqwest, ureq)\n` +
  `- Dynamic plugin loading (native DLL/so plugins) — src/pluginmanager.cpp (libloading, abi_stable)\n` +
  `- Containers/hashing replacing QHash/QSet/QMap — std + ahash/indexmap/hashbrown\n` +
  `- Error handling, logging, CLI, config/dirs, async runtime — recommend idiomatic choices (thiserror/anyhow, tracing, clap, directories, tokio if needed)\n` +
  `- Any other per-subsystem needs you discover.\n\n` +
  `Write a selection table to ${OUT}/_design/crate_selection.md and return structured selections.`

const oraclePrompt = commonHeader +
  `IDEMPOTENCY: ${OUT}/_oracle/logs/ may ALREADY contain captured per-test logs from a prior run, and ${OUT}/_oracle/test_sources/ the golden sources. If so, REUSE them: only (re)build/run any missing test targets, then (re)generate the summary ${OUT}/_oracle/RESULTS.md from all available logs. Do not rebuild what is already captured.\n\n` +
  `TASK: Build & run the ORIGINAL C++ test suite to capture GOLDEN behavioral outputs that the Rust port must match. (These are the existing test programs shipped with the public source.)\n\n` +
  `Environment has: Qt 6.10.3 (in /usr/lib64), g++ 16, cmake 4.3. The stable source copy at ${SRC} has NO .git and its git submodules (third_party/qscintilla, raw_pdb, fadec) are EMPTY.\n\n` +
  `Steps:\n` +
  `1) Get a buildable tree WITH submodules: clone fresh \`git clone --recurse-submodules https://github.com/IChooseYou/Reclass.git /tmp/reclass_oracle\` (network is available). If submodule fetch fails, init what you can; you can still build targets that don't need the missing ones.\n` +
  `2) Inspect /tmp/reclass_oracle/CMakeLists.txt for the test/build options. Configure cmake (prefer Ninja if available, else Unix Makefiles) with Qt6 enabled and testing ON. Qt6 should be auto-found; if not, set -DCMAKE_PREFIX_PATH=/usr/lib64/cmake.\n` +
  `3) Build and run the PURE-LOGIC / headless test targets (these are the behavioral oracle): test_core, test_compose, test_format, test_addressparser, test_rtti, test_rtti_hint, test_typeinfer, test_generator, test_scanner, test_scanner_combinations, test_import_source, test_import_xml, test_export_xml, test_disasm, test_clipboard, test_provider, test_source_provider, test_command_row, test_mcp, test_theme, and test_import_pdb if raw_pdb built. GUI tests can be attempted headless (QT_QPA_PLATFORM=offscreen) but are lower priority.\n` +
  `4) For each test target: record build success/failure, run it (capture full stdout/stderr + exit code), and save outputs. Use \`ctest --output-on-failure\` and/or run binaries directly.\n` +
  `5) Save EVERYTHING under ${OUT}/_oracle/ : per-test logs (e.g. _oracle/logs/test_core.txt), a summary ${OUT}/_oracle/RESULTS.md (table: target | built? | passed? | #assertions | notes), and copy any golden fixture/output files the tests read or produce. These golden outputs let the Rust port be verified for fidelity.\n\n` +
  `Be resilient: if a target won't build (missing submodule, etc.), note it and move on — capture as many golden outputs as possible. Return a concise text summary of what was captured and where.`

function designPrompt() {
  return commonHeader +
    `IDEMPOTENCY: if ${OUT}/_design/ARCHITECTURE.md already exists and is substantial, read it and return its structured summary instead of redoing it.\n` +
    `ROBUSTNESS: some input reports may be missing (a prior run may not have produced every file). Use whatever is present, infer sensibly from ${SRC} for any gaps, and explicitly list which inputs were missing in a "Gaps" section.\n\n` +
    `TASK: Synthesize the cross-platform Rust workspace ARCHITECTURE for the faithful port.\n\n` +
    `Read ALL of these inputs that exist:\n` +
    `- Every subsystem report: ${OUT}/_design/understand/*.md\n` +
    `- The GPUI cookbook (raw gpui, Zed-grounded): ${OUT}/_design/gpui_cookbook.md\n` +
    `- The gpui-component cookbook (component library on top of gpui): ${OUT}/_design/gpui_component_cookbook.md\n` +
    `- The crate selection table: ${OUT}/_design/crate_selection.md\n` +
    `- The oracle results: ${OUT}/_oracle/RESULTS.md (if present)\n\n` +
    `UI STRATEGY (decided): use the longbridge/gpui-component library for the standard UI chrome it provides (TitleBar; DockArea/Panel dock+tab+split with layout persistence; Tree; virtualized DataTable; List/Select/Combobox with async search; Dialog/AlertDialog; Popover/Tooltip/ContextMenu; notifications; runtime JSON Theme/ThemeRegistry) — map Reclass's MDI doc tabs, dockable workspace/scanner panels, workspace tree, scanner/process/profiler tables, options & goto dialogs, command palette, type/enum/source pickers, hex popup, tooltips, context menus, find bar, titlebar, and theme system onto these. Build the BESPOKE virtualized structured-editor surface (the inline-editable styled-text grid with per-span coloring and cross-row selection) on raw gpui as a custom Element — gpui-component cannot model it (confirmed). Other raw-gpui bits per the cookbook gaps: hex-toolbar/chip/size-bar custom painting, status-bar shimmer, Reclass's exact fuzzy tie-breaks (custom scorer into perform_search), source-icon tab chrome, SVG icon assets.\n` +
    `DEPENDENCY DECISION (use VERBATIM from ${OUT}/_design/gpui_component_cookbook.md — do NOT re-derive): declare gpui and gpui_platform as UNPINNED git deps on zed (the SAME source form gpui-component uses) so Cargo resolves ONE shared gpui copy; gpui_platform carries features ["font-kit","x11","wayland","runtime_shaders"]; gpui-component is pinned by rev. Do NOT add rev= to gpui/gpui_platform (that splits gpui into two incompatible copies → type errors). Reproducibility comes from COMMITTING Cargo.lock. Record this rationale in ARCHITECTURE.md.\n\n` +
    `Design a Cargo WORKSPACE of focused crates that mirror the subsystems, separating the portable logic (testable headless) from the GPUI UI. For each crate define: name, lib/bin, purpose, internal dependencies, external crates (from the selection table), and which C++ files it maps to. Respect the original module dependency graph (e.g. core <- {compose, format, addressparser, rtti, scanner, generator}; ui <- core + providers; app(bin) <- everything). The data source is fully abstracted by a Provider trait: design a providers crate that implements ONLY the benign built-in sources (file, in-memory buffer, snapshot, null) and registry. Reserve a clearly-named, documented stub crate/module (e.g. reclass-providers-native) as the extension point for the OUT-OF-SCOPE live OS process/kernel/remote/WinDbg sources — define its trait-conformant stub surface and #[cfg]/feature gating, but DO NOT design or implement those sources. The app targets Windows primarily plus macOS/Linux; the workspace must cargo-build on Linux (what we verify here).\n\n` +
    `Also produce: a milestone/implementation ORDER (leaf logic crates first, UI last), and a risk list (esp. GPUI editor-surface, PDB/disasm parity, platform providers).\n\n` +
    `Write a comprehensive ${OUT}/_design/ARCHITECTURE.md containing: the crate graph (with a dependency diagram), a full C++ file -> Rust crate/module MAPPING TABLE, the chosen external crate per crate, public-API sketches per crate, the cargo feature plan for platform code, and the milestone order. Return the structured design.`
}

function scaffoldPrompt() {
  return commonHeader +
    `TASK: Create a COMPILING SINGLE-PACKAGE skeleton for the port at ${OUT}, following ${OUT}/_design/ARCHITECTURE.md (read it first, plus ${OUT}/_design/gpui_cookbook.md and ${OUT}/_design/gpui_component_cookbook.md for the exact PROBE-VERIFIED UI deps + minimal window, and ${OUT}/_design/crate_selection.md for dependency choices).\n\n` +
    `STRUCTURE — mirror the monolithic C++ app: ONE package named "reclass", NOT a multi-crate workspace. If a stale ${OUT}/crates/ directory or a [workspace] ${OUT}/Cargo.toml exists from a prior attempt, DELETE them first. Each C++ subsystem becomes a MODULE under src/, exactly as laid out in ARCHITECTURE.md section 2 (core/, compose.rs, format.rs, addr.rs, generator.rs, scanner.rs, disasm.rs, rtti/, imports/, mcp/, theme/, controller.rs, provider/, ui/). The app is src/main.rs ([[bin]] reclass) and the MCP bridge is src/bin/reclass-mcp-bridge.rs.\n\n` +
    `Requirements:\n` +
    `1) Create ${OUT}/Cargo.toml as a SINGLE [package] (name = "reclass", edition 2021, the lib + the two [[bin]] targets). Put all external dependencies here directly (per crate_selection.md + the UI cookbooks). Define [features] with default = ["ui","imports","disasm","symbols","mcp"] gating the heavy/optional modules + their deps (ui ⇒ gpui/gpui_platform/gpui-component; imports ⇒ pdb2/object/quick-xml; disasm ⇒ iced-x86; symbols ⇒ demanglers/reqwest; mcp ⇒ interprocess) so a headless \`--no-default-features\` logic build needs no gpui. Add rust-toolchain.toml (nightly per the cookbook) and a README.md (project overview + C++→module mapping + build instructions incl. the Linux link env).\n` +
    `2) Create src/lib.rs declaring every module (\`pub mod core; pub mod compose; ...\`, with #[cfg(feature=...)] on the gated ones) and src/main.rs + src/bin/reclass-mcp-bridge.rs. Each module gets the KEY public type/trait/function SIGNATURES from its understand map + ARCHITECTURE.md, with minimal compiling bodies (todo!()/unimplemented!()/trivial defaults). Doc-comment each ported item with its C++ origin (file path).\n` +
    `3) GENUINELY PORT the core module (src/core/): translate the node tree + field kinds/types from ${SRC}/src/core.h and commontypes.h into real Rust types with serde derives and the obvious accessors/constructors — the base everything builds on, so make it real, not a stub. Also make the provider module real for the benign sources: the Provider trait + BufferProvider + FileProvider (file/buffer are trivial and benign); native sources are documented stub impls.\n` +
    `4) For the GPUI app/ui crates, use the EXACT build-verified dependency lines from ${OUT}/_design/gpui_component_cookbook.md: gpui and gpui_platform as UNPINNED git deps on zed (gpui_platform with features ["font-kit","x11","wayland","runtime_shaders"]), and gpui-component pinned by its rev. Do NOT add rev= to gpui/gpui_platform (it splits gpui into two incompatible copies). COMMIT Cargo.lock for reproducibility. Create a minimal window using gpui-component's setup pattern (application() -> gpui_component::init -> Root -> a Button/dock), compiling and ready to open; keep the bespoke raw-gpui editor-surface Element as a documented stub. LINUX BUILD PREREQS for the gpui link step (see the gpui_component_cookbook): ensure fontconfig-devel and libxkbcommon-x11-devel are installed, and export LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16 (for libstdc++.so) when running cargo build. If (and only if) the build still does NOT work here, put the UI crates behind a default-off cargo feature and document the blocker prominently in README.md + ${OUT}/_design/ARCHITECTURE.md so the rest of the workspace still builds.\n` +
    `5) Verify it compiles: first \`cargo build --no-default-features\` (headless engine, no gpui — must be GREEN), then the full \`cargo build\` (default features incl. ui; allow a long time for the first gpui build; export the Linux link env from the cookbook: LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16). Fix all errors until GREEN. If (and only if) gpui is the sole blocker, leave the headless build green and document the ui blocker. Commit a Cargo.lock (run a build so it's generated). Run \`cargo fmt\`.\n` +
    `6) Do NOT delete the ${OUT}/_design or ${OUT}/_oracle directories.\n\n` +
    `Report the final \`cargo build\` status for both feature sets (paste the tails), the module list created, and any blockers. Be meticulous — a clean compile of a SINGLE "reclass" package is the deliverable.`
}

function verifyPrompt() {
  return commonHeader +
    `TASK: Independently VERIFY the scaffolded SINGLE "reclass" package at ${OUT} compiles and is well-formed.\n\n` +
    `Run, from ${OUT}: \`cargo build --no-default-features 2>&1 | tail -40\` (headless engine — must be green), \`cargo build 2>&1 | tail -40\` (default features incl. ui; export LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16), \`cargo metadata --no-deps --format-version 1\` (confirm exactly ONE package "reclass" — NOT a multi-crate workspace), and \`cargo fmt --check\`.\n` +
    `Confirm: it is a single package (no stale crates/ dir, no [workspace] members); the src/ module layout matches ARCHITECTURE.md section 2; the core module contains REAL ported types (not just stubs) from core.h; both builds are green (or headless green + a documented ui/gpui blocker). If anything fails, FIX it and re-run until green. ` +
    `Append a short '## Build verification' section to ${OUT}/README.md with the final status and exact commands. Return a concise PASS/FAIL summary with the final build tails.`
}

function specPrompt(s) {
  return commonHeader +
    `IDEMPOTENCY: if ${OUT}/_design/specs/PORTING_${s.key}.md already exists and is substantial, read it and return its summary instead of redoing it.\n\n` +
    `TASK: Write a function-level PORTING SPEC for one subsystem, to drive its faithful Rust implementation in a later workflow.\n\n` +
    `Subsystem: ${s.title} (key: ${s.key}).\n` +
    `Read first: ${OUT}/_design/understand/${s.key}.md (the behavioral map), ${OUT}/_design/ARCHITECTURE.md (target crate + deps), ${OUT}/_design/crate_selection.md (crates to use), and ${OUT}/_oracle/RESULTS.md (golden test outputs). For UI subsystems (editor-surface, app-shell, widgets-dialogs, controller, themes) ALSO read ${OUT}/_design/gpui_component_cookbook.md (which gpui-component component backs each surface, with version/build notes) and ${OUT}/_design/gpui_cookbook.md (raw-gpui patterns for the bespoke editor Element). Consult the original C++ at ${SRC} (${s.files}) as needed.\n\n` +
    `Write ${OUT}/_design/specs/PORTING_${s.key}.md containing: the target Rust crate/module(s); an ITEM-BY-ITEM mapping of each C++ type/function to its Rust counterpart (signature + behavior notes + which external crate, if any, replaces it); the exact data structures (with serde/representation decisions); algorithm pseudocode for any tricky logic; error-handling strategy; and a TEST PLAN listing each original C++ test (from ${SRC}/tests) that covers this subsystem and how to translate it into a Rust #[test] (citing the golden oracle output where relevant). Order the work into small, independently-verifiable steps. Return a one-paragraph summary + the spec path.`
}

// ---------- orchestration ----------
log('Phase 1: Understand + Research + Oracle (concurrent)')
phase('Understand')

const [understand, gpui, crates, oracle] = await parallel([
  () => parallel(subsystems.map(s => () =>
    agent(understandPrompt(s), { phase: 'Understand', label: `understand:${s.key}`, schema: UNDERSTAND_SCHEMA }))),
  () => agent(gpuiPrompt, { phase: 'Research', label: 'research:gpui', schema: GPUI_SCHEMA }),
  () => agent(cratePrompt, { phase: 'Research', label: 'research:crates', schema: CRATE_SCHEMA }),
  () => agent(oraclePrompt, { phase: 'Oracle', label: 'oracle:cpp-tests' }),
])

const understandOk = (understand || []).filter(Boolean)
log(`Understand: ${understandOk.length}/${subsystems.length} subsystem maps produced. ` +
    `GPUI probe build: ${gpui && gpui.probe_build_succeeded ? 'OK' : 'see notes'}. ` +
    `Crates selected: ${crates ? (crates.selections || []).length : 0}.`)

// Defensive wrapper: a single agent hitting an error (incl. an automated policy block)
// must not abort the whole workflow — return null and let the run continue / be re-run.
async function tryAgent(p, opts) {
  try { return await agent(p, opts) } catch (e) { log(`agent ${opts && opts.label} failed: ${String(e).slice(0, 200)}`); return null }
}

// Design is HUMAN-AUTHORED in _design/ARCHITECTURE.md (a single "reclass" package, modules
// mirroring the C++ src/ tree). The design agent is intentionally DISABLED: it previously
// ignored idempotency and overwrote the doc with an over-split multi-crate layout. Do not
// re-enable it — ARCHITECTURE.md is the source of truth and is maintained by hand.
phase('Design')
const design = null
log('Design: using human-authored single-package ARCHITECTURE.md; design agent disabled.')

phase('Scaffold')
const scaffold = await tryAgent(scaffoldPrompt(), { label: 'scaffold:workspace' })
const verify = await tryAgent(verifyPrompt(), { label: 'scaffold:verify' })
log('Scaffold complete; build verified (see report).')

phase('Specs')
const specTargets = subsystems.filter(s => s.key !== 'tests-catalog')
const specs = await parallel(specTargets.map(s => () =>
  agent(specPrompt(s), { phase: 'Specs', label: `spec:${s.key}` })))
log(`Specs: ${(specs || []).filter(Boolean).length}/${specTargets.length} porting specs written.`)

return {
  understand_maps: understandOk.map(u => ({ subsystem: u.subsystem, portability: u.portability, report: u.report_path })),
  gpui: gpui ? { probe_build_succeeded: gpui.probe_build_succeeded, cookbook: gpui.cookbook_path, dep: gpui.cargo_dependency_toml } : null,
  crates_selected: crates ? (crates.selections || []).length : 0,
  crate_table: crates ? crates.table_path : null,
  oracle_summary: oracle,
  design: design ? { crate_count: (design.crates || []).length, order: design.milestone_order, architecture: design.architecture_path, risks: design.risks } : null,
  scaffold_report: scaffold,
  verify_report: verify,
  specs_written: (specs || []).filter(Boolean).length,
}
