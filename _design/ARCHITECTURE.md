# Reclass-rs — Architecture

A faithful, 1:1 Rust + GPUI port of **Reclass** (github.com/IChooseYou/Reclass, MIT) —
an open-source structured-data / struct-layout editor (a modern successor to ReClass.NET).

> **Structure mirrors the original.** The C++ app is a single monolithic executable built
> from all of `src/*.cpp` (plus a tiny `ReclassMcpBridge` tool and out-of-scope plugin DLLs).
> So the port is **ONE Rust package** with each subsystem as a **module under `src/`** — NOT a
> workspace of many crates. The libraries the C++ links statically (QScintilla, fadec, raw_pdb)
> become ordinary external crate dependencies (gpui/gpui-component, iced-x86, pdb2, …).
>
> This decision is intentional and final: do not re-split into per-subsystem crates. Layering is
> enforced by module discipline; the headless/heavy-dep separation is handled by **cargo features**
> (see §2), not crate walls.

Authored from the 17 subsystem maps in `_design/understand/`, the UI cookbooks
(`gpui_cookbook.md`, `gpui_component_cookbook.md`), `crate_selection.md`, and the golden
oracle in `_oracle/`.

---

## 1. Goals & constraints

| Decision | Resolution |
|---|---|
| **Fidelity** | Full 1:1 behavioral parity with the public C++ source — not a subset. |
| **Structure** | **One package**, subsystems as modules (mirrors the monolithic C++ app). UI is a module, not a separate crate. |
| **Platforms** | **Windows is primary**; macOS + Linux fully supported. OS-specific code sits behind `#[cfg(...)]` and must keep compiling everywhere. Only the Linux build is compile-verifiable on the dev machine. |
| **Reuse over hand-roll** | Prefer well-maintained crates (`pdb2`, `object`, `iced-x86`, `quick-xml`, `interprocess`, `cpp_demangle`/`msvc-demangler`, `libloading`, `reqwest`); hand-roll only where parity demands it (source-import parser, fuzzy scorer, PE-over-`Provider` decode). |
| **Oracle** | The original C++ tests are the behavioral contract. Golden outputs are in `_oracle/`; each C++ test → a Rust `#[test]`. |
| **UI** | GPUI (Zed) for rendering; **longbridge/gpui-component** for standard chrome; a bespoke raw-gpui `Element` for the structured-editor surface. |
| **Memory layer** | All data access goes through a `Provider` trait. Built-in **file / buffer / snapshot / null** sources are implemented. Live **process / kernel / remote / WinDbg** sources are documented **stubs** (blocked by Anthropic's automated cyber safeguard; unlockable via the Cyber Verification Program). |
| **Toolchain** | Rust **nightly** (gpui requirement, pinned via `rust-toolchain.toml`); reproducibility via a committed `Cargo.lock`. |

---

## 2. Package & module layout

A single package, `reclass`, whose `src/` mirrors the C++ `src/` tree:

```
reclass-rs/
├─ Cargo.toml                 # ONE [package] "reclass" (lib + 2 bins) — NOT [workspace]
├─ Cargo.lock                 # committed — pins the unpinned gpui git dep
├─ rust-toolchain.toml        # nightly (gpui)
├─ src/
│  ├─ lib.rs                  # declares the modules below; the shared engine API
│  ├─ main.rs                 # [[bin]] reclass        ← src/main.cpp (the app)
│  ├─ bin/
│  │  └─ reclass-mcp-bridge.rs# [[bin]] stdio↔pipe     ← tools/rcx-mcp-stdio.cpp
│  │
│  ├─ core/                   # ← core.h, commontypes.h, typeinfer.h
│  │  └─ (Node tree, NodeKind/type system, project/document model, .rcx serde, value history, clipboard)
│  ├─ compose.rs              # ← compose.cpp   (NodeTree+Provider → text + LineMeta; column geometry)
│  ├─ format.rs               # ← format.cpp    (value formatting, hex/ascii)
│  ├─ addr.rs                 # ← addressparser.* (address-expression evaluator)
│  ├─ generator.rs            # ← generator.*   (C/C++/… source emission)
│  ├─ scanner.rs              # ← scanner.*     (value/pattern search over byte slices)  [feature: scanner-parallel]
│  ├─ disasm.rs               # ← disasm.*      (iced-x86)                               [feature: disasm]
│  ├─ rtti/                   # ← rtti.*, symbolstore.*, symbol_downloader.*, names/*    [feature: symbols]
│  ├─ imports/                # ← imports/*     (.rcx, C/C++ source, ReClass XML, PDB, PE)[feature: imports]
│  ├─ mcp/                    # ← mcp/*         (JSON-RPC 2.0 server + schemas)          [feature: mcp]
│  ├─ theme/                  # ← themes/*      (runtime JSON themes)
│  ├─ controller.rs           # ← controller.*  (document, undo/redo apply, refresh, history/heatmap, write-back)
│  ├─ provider/               # ← providers/*, providerregistry.*, iplugin.h
│  │  ├─ mod.rs               #   the Provider trait + registry
│  │  ├─ file.rs, buffer.rs, snapshot.rs, null.rs   # built-in benign sources (implemented)
│  │  └─ native.rs            #   process/kernel/remote/windbg = documented STUBS (cfg/feature)
│  └─ ui/                     # ← editor.*, widgets/*, dialogs, popups, docks,
│     │                       #   mainwindow.h, startpage.h, titlebar.*            [feature: ui, default]
│     ├─ editor/              #   bespoke raw-gpui Element (the structured grid)
│     ├─ docks.rs, tabs.rs    #   via gpui-component DockArea/Panel
│     ├─ dialogs/, popups/    #   via gpui-component Dialog/Popover/ContextMenu/Select
│     ├─ scanner_panel.rs, process_picker.rs, command_palette.rs, …
│     └─ theme_apply.rs       #   map theme/ model → gpui-component ThemeRegistry
├─ tests/                     # integration tests translated from tests/*.cpp (oracle-backed)
├─ _design/                   # design docs (this file, cookbooks, specs, understand maps)
└─ _oracle/                   # captured golden outputs from the original C++ tests
```

**Layering (enforced by module discipline, not crate walls):** `core` is the foundation;
the logic modules build on it; `ui` builds on the logic; the `reclass` bin wires it together.
`provider` is the data-source abstraction every byte-reading module talks to (never a concrete source).

**Cargo features** give the "don't pay for what you don't use" benefit a multi-crate split was
reaching for, without the boilerplate:
`default = ["ui", "imports", "disasm", "symbols", "mcp"]`. `--no-default-features` yields a
headless engine build (no gpui) for fast logic-only testing; `imports`/`disasm`/`symbols`/`mcp`
gate the heavy external deps (pdb2/object/quick-xml, iced-x86, demanglers/reqwest, interprocess).

> Future: when the out-of-scope native memory providers and the ReClass.NET DLL/CLR compat
> layer are unblocked, they become separate `cdylib` plugin crates (mirroring the C++ plugin
> DLLs) and the package becomes a 1-member workspace. Not needed now.

---

## 3. Modules in detail

| Module | Maps C++ | Role | Feature | Notable deps |
|---|---|---|---|---|
| `core` | `core.h`, `commontypes.h`, `typeinfer.h` | Node tree, `NodeKind`/type system, project/document model, `.rcx` (de)serialization, value history, clipboard — the shared vocabulary. **Genuinely ported.** | always | serde, serde_json, ahash, indexmap, bytemuck, thiserror |
| `provider` | `providers/*`, `providerregistry.*`, `iplugin.h` | `Provider` trait (byte read/write at offset; region/module queries) + built-in **file/buffer/snapshot/null** + registry. Native OS sources = stubs. | always | libloading (entrypoint) |
| `compose` | `compose.cpp` | Tree+Provider → rendered rows (text + `LineMeta`, column geometry). | always | — |
| `format` | `format.cpp` | Value formatting, hex/ASCII previews. | always | bytemuck |
| `addr` | `addressparser.*` | Address-expression evaluator (`<mod>+0x10`, deref chains, symbols). | always | — |
| `generator` | `generator.*` | Emit C/C++/… source from the tree (multiple backends). | always | — |
| `controller` | `controller.*` | Document, 15-command undo/redo apply, refresh loop over the active `Provider`, value-history ring buffer + heatmap, pointer-follow, write-back, selection. | always | — |
| `theme` | `themes/*` | Runtime JSON theme model + manager (→ gpui-component `ThemeRegistry`). | always | serde, serde_json |
| `scanner` | `scanner.*` | Value/pattern search engine over `&[u8]` regions from a `Provider`. | always (rayon under `scanner-parallel`) | rayon |
| `disasm` | `disasm.*` | x86/x64 disassembly + hex dump (replaces fadec). | `disasm` | iced-x86 |
| `rtti` | `rtti.*`, `symbolstore.*`, `symbol_downloader.*`, `names/*` | RTTI parse, symbol store/resolve, demangle (Itanium+MSVC), symbol-server download. | `symbols` | cpp_demangle, msvc-demangler, reqwest+rustls, directories |
| `imports` | `imports/*` | `.rcx` JSON, C/C++ source parse, ReClass XML in/out, PDB import, PE debug-dir. | `imports` | serde_json, quick-xml, pdb2, object, regex, bytemuck |
| `mcp` | `mcp/*` | JSON-RPC 2.0 server over a local socket + tool/notification schemas. | `mcp` | serde_json, interprocess |
| `ui` | `editor.*`, `widgets/*`, dialogs, popups, docks, `mainwindow.h`, `startpage.h`, `titlebar.*` | GPUI views: bespoke editor `Element` + chrome via gpui-component; theme application; command palette. | `ui` (default) | gpui, gpui_platform, gpui-component, nucleo(opt) |
| `main.rs` | `main.cpp` | The `reclass` app binary: window, CLI, lifecycle, wiring. | bin | clap, tracing, anyhow |
| `bin/reclass-mcp-bridge.rs` | `tools/rcx-mcp-stdio.cpp` | stdio↔socket bridge binary. | bin | serde_json, interprocess |

---

## 4. C++ → Rust mapping (file → module)

| C++ source | Rust module |
|---|---|
| `src/core.h`, `commontypes.h`, `typeinfer.h` | `core` |
| `src/compose.cpp` | `compose` |
| `src/format.cpp` | `format` |
| `src/addressparser.*` | `addr` |
| `src/generator.*` | `generator` |
| `src/scanner.*` | `scanner` |
| `src/controller.*` | `controller` |
| `src/disasm.*` | `disasm` |
| `src/rtti.*`, `symbolstore.*`, `symbol_downloader.*`, `names/*` | `rtti` |
| `src/imports/*` | `imports` |
| `src/mcp/*` | `mcp` |
| `src/themes/*` | `theme` |
| `src/providers/*`, `providerregistry.*`, `iplugin.h`, `pluginmanager.*` | `provider` |
| `src/editor.*`, `widgets/*`, dialogs/popups, docks, `mainwindow.h`, `startpage.h`, `titlebar.*`, `commandpalette.h`, `typeselectorpopup.*`, `scannerpanel.*`, `optionsdialog.*`, `processpicker.*`, `gotoaddressdialog.h`, `rcxtooltip.h` | `ui` |
| `src/main.cpp` | `main.rs` |
| `tools/rcx-mcp-stdio.cpp` | `bin/reclass-mcp-bridge.rs` |
| `plugins/*` | **out of scope** — `provider::native` stubs now; future `cdylib` plugin crates |
| `third_party/{fadec,raw_pdb,qscintilla}` | external crates: `iced-x86` / `pdb2` / (GPUI editor surface) |

---

## 5. UI strategy

**gpui-component for standard chrome; raw gpui for the bespoke surface** (per `gpui_component_cookbook.md`):

| Reclass surface | gpui-component |
|---|---|
| MDI document tabs + dockable workspace/scanner panels (layout persistence) | `DockArea` / `DockItem` / `Panel` (`dump`/`load`) |
| Custom titlebar | `TitleBar` |
| Workspace tree | `Tree` |
| Scanner / process-picker / profiler tables | virtualized sortable `DataTable` |
| Command palette, type/enum/source pickers | `List` / `Select` / `Combobox` (+ async `perform_search`) |
| Options & goto-address dialogs, confirmations | `Dialog` / `AlertDialog` |
| Tooltips, hover popups, context menus | `Popover` / `Tooltip` / `ContextMenu` |
| Theme system | runtime JSON `Theme` / `ThemeRegistry` (≈1:1 with `ThemeManager`) |

**Stays raw-gpui** (confirmed gaps): the **structured-editor surface** (per-line styled grid,
independently-colored inline-**editable** spans, tab-cycling, fold markers, hex/ASCII columns,
per-byte change highlighting, cross-row selection, split views); hex-toolbar/chip/size-bar
custom painting; status-bar shimmer; Reclass's exact fuzzy tie-breaks (custom scorer into
`perform_search`); dock drop-zone overlay; the "+" sentinel / source-icon tab chrome; SVG icons.

---

## 6. UI dependency & version decision

GPUI is git-only and gpui-component depends on gpui as an **unpinned** git ref. If we pin our
own gpui with `rev=`, Cargo treats `git+url?rev=X` and `git+url` as different sources → **two
incompatible gpui copies** → type errors. Resolution (probe-build verified):

```toml
gpui           = { git = "https://github.com/zed-industries/zed" }                                   # UNPINNED, same form as gpui-component
gpui_platform  = { git = "https://github.com/zed-industries/zed", features = ["font-kit","x11","wayland","runtime_shaders"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "bd4174e4991eef4c0900cb6dfb7143e0889f5dd9" }
```
- gpui is split into `gpui` + `gpui_platform` (gpui `0.2.2`). As an unpinned git dep it re-resolves
  to Zed default-branch HEAD *at lock time*; the skeleton's committed `Cargo.lock` pins
  **`876ec5a8`** (HEAD on 2026-05-31; the cookbook's `09165c1…` was HEAD when the cookbook was
  written). There is exactly **one** `gpui` source in the graph (verified: `grep -c '^name = "gpui"$'
  Cargo.lock` → 1), so the version-split footgun is avoided and the full build links cleanly.
- Reproducibility via **committed `Cargo.lock`**, not a `rev=` on gpui. gpui-component *is* rev-pinned
  (`bd4174e…`).
- Linux build prereqs (installed): `fontconfig-devel`, `libxkbcommon-x11-devel`, and a discoverable
  `libstdc++.so` via `LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16` at link time (also baked into
  `.cargo/config.toml` as a `-L` rustflag for the linux-gnu target). **Build status: full default
  build (gpui UI) compiles GREEN on this Linux machine; no UI blocker — the `ui` feature stays
  default-on.**

Logic deps are pinned at the package level (see `crate_selection.md` for the full table + rationale).

---

## 7. `Provider` trait & the memory layer

```
trait Provider {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<usize>;
    fn write(&mut self, addr: u64, data: &[u8]) -> Result<usize>;
    fn regions(&self) -> &[Region];
    fn modules(&self) -> &[Module];
    fn capabilities(&self) -> Caps;
    // … (mirrors src/providers/provider.h)
}
```
- **Implemented (benign):** `BufferProvider` (in-memory), `FileProvider` (open a binary file —
  the original "File" source), `SnapshotProvider`, `NullProvider`.
- **Stubs (out of scope):** live `process`/`kernel`/`remote`/`windbg` sources + ReClass.NET
  DLL/CLR compat, in `provider::native`, `#[cfg]`/feature-gated. Blocked by Anthropic's
  automated cyber safeguard; unlockable via the **Cyber Verification Program** or user-supplied
  code — without touching any other module.

---

## 8. Testing / oracle strategy

- `_oracle/logs/` — captured stdout of the original C++ logic tests; `_oracle/test_sources/` —
  the original `.cpp` assertions to mirror; `tests-catalog.md` — index of what each test asserts.
- Each module's spec (`_design/specs/PORTING_*.md`) lists the covering C++ tests and how to
  translate each into a Rust `#[test]`, asserting against golden output where behavior is
  output-exact (`generator`, `format`, `disasm`).
- Logic tests run with `--no-default-features` (no `ui` ⇒ no gpui) for speed.

---

## 9. Implementation order (milestones)

1. `core` — node/type model + `.rcx` serde (foundation).
2. `controller`/`compose` — tree ops + undo/redo (heavily tested ⇒ high-confidence parity).
3. `format`, `addr` — rendering + address expressions.
4. `generator`, `disasm` — output-exact, easy oracle checks.
5. `provider` — trait + file/buffer/snapshot/null + registry.
6. `rtti`, `imports` — symbols/demangle, then RCX/XML/source/PDB/PE.
7. `scanner` — search over byte slices.
8. `mcp`, `theme` — JSON-RPC server; theme model.
9. `ui` — gpui-component chrome first, then the bespoke editor `Element`.
10. `main.rs` + bridge bin — window, CLI, wiring; end-to-end.

Each step is a small workflow scoped to **one module** (`cargo test --no-default-features`
against the oracle). **Safeguard rule:** never let one agent aggregate the data-layer
subsystems (provider/scanner/controller/mcp); per-module scope passes the classifier.

---

## 10. Risks & open items

- **Editor surface fidelity** — the bespoke gpui `Element` is the hardest UI work; prototype early.
- **gpui churn** — unpinned gpui; the committed `Cargo.lock` holds it. Re-pin deliberately.
- **PE-over-`Provider`** — hand-decode (object/goblin need a contiguous slice).
- **Fuzzy parity** — hand-roll the two-pass scorer (ranking + highlight positions).
- **Memory layer** — stubbed; full parity needs Cyber Verification approval.
- **Windows-only paths** — kernel/WinDbg/CLR compile behind `#[cfg(windows)]`, verified on Windows CI.
