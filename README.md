# reclass

A faithful, 1:1 Rust + [GPUI](https://www.gpui.rs/) port of **Reclass**
([github.com/IChooseYou/Reclass](https://github.com/IChooseYou/Reclass), MIT) —
an open-source structured-data / struct-layout editor and in-memory layout
inspector (a modern successor to ReClass.NET). It lets developers view and
annotate in-memory data layouts as typed structs / arrays / pointers while
debugging.

This is a **single Cargo package** (`reclass`) whose `src/` mirrors the
monolithic C++ app's `src/` tree — NOT a multi-crate workspace. The C++ app
links everything into one executable from a cleanly-layered `src/`, so the port
mirrors that layering as modules under one package. The "don't pay for what you
don't use" benefit a crate split would give is provided by **cargo features**
instead. See `_design/ARCHITECTURE.md` for the full design.

> Targets **Windows** (primary), **macOS**, and **Linux**. OS-specific code sits
> behind `#[cfg(...)]` and keeps compiling everywhere. Only the Linux build is
> compile-verifiable on the dev machine.

## Status

This is the **compiling skeleton**: the foundational `core` and `provider`
modules are genuinely ported (real types, serde, accessors, tests); every other
subsystem has its key public types/signatures with minimal compiling bodies
(`todo!()`), each doc-commented with its C++ origin and the workflow that fills
it in. Both feature sets build green on Linux.

## Layout & C++ → module mapping

| Rust module | C++ source | Role |
|---|---|---|
| `core/` (`kind`, `node`, `tree`, `value_history`, `linemeta`, `command`, `commontypes`, `clipboard`, `typeinfer`) | `core.h`, `commontypes.h`, `typeinfer.h`, `clipboard.h` | Node tree, `NodeKind`/type system, `.rcx` serde, value-history heatmap, line/render metadata, undo/redo command model, predefined struct templates, clipboard codec. **Genuinely ported.** |
| `provider/` (`mod`, `buffer`, `file`, `snapshot`, `null`, `registry`, `native`) | `providers/*`, `providerregistry.h`, `iplugin.h` | The `Provider` trait + built-in **file / buffer / snapshot / null** sources + registry. **Real for the benign sources.** Live OS sources = documented stubs in `native` (out of scope). |
| `compose.rs` | `compose.cpp` | Tree + Provider → rendered rows (text + `LineMeta`, column geometry). |
| `format.rs` | `format.cpp` | Value formatting, hex/ASCII previews, value read/parse/validate. |
| `addr.rs` | `addressparser.*` | Address-expression evaluator (`<mod>+0x10`, deref chains, symbols). |
| `generator.rs` | `generator.*` | C/C++/Rust/C#/Python/#define source emission. |
| `controller.rs` | `controller.*` | Document, undo/redo apply, async refresh loop, value-history/heatmap, selection, source management, write-back. |
| `scanner.rs` | `scanner.*` | Value/pattern search over `Provider` byte regions (`scanner-parallel` ⇒ rayon). |
| `disasm.rs` | `disasm.*` | x86/x64 disassembly + hex dump (iced-x86). [feature: `disasm`] |
| `rtti/` (`mod`, `demangle`) | `rtti.*`, `symbolstore.*`, `symbol_downloader.*`, `names/*` | RTTI walk, symbol store/resolve, demangle (Itanium + MSVC), symbol-server download. [feature: `symbols`] |
| `imports/` | `imports/*` | `.rcx` JSON, C/C++ source parse, ReClass XML in/out, PDB import, PE debug-dir. [feature: `imports`] |
| `mcp/` | `mcp/*` | JSON-RPC 2.0 server over a local socket + tool/notification schemas. [feature: `mcp`] |
| `theme/` | `themes/*` | Runtime JSON theme model + manager (→ gpui-component `ThemeRegistry`). |
| `ui/` (`mod`, `editor`) | `editor.*`, `widgets/*`, dialogs, popups, docks, `mainwindow.h`, `startpage.h`, `titlebar.*` | GPUI views: standard chrome via gpui-component + the bespoke raw-gpui editor `Element`. [feature: `ui`, default] |
| `main.rs` | `main.cpp` | The `reclass` app binary: window, CLI, lifecycle, wiring. |
| `bin/reclass-mcp-bridge.rs` | `tools/rcx-mcp-stdio.cpp` | stdio ↔ local-socket bridge binary. |
| — (out of scope) | `plugins/*` | Live OS process / kernel / remote / WinDbg sources — documented stubs only, behind `provider::native`. |

## Binaries

- **`reclass`** — the GUI app (`src/main.rs`). With the default `ui` feature it
  opens a GPUI window; with `--no-default-features` it builds as a headless
  engine entry point (no gpui).
- **`reclass-mcp-bridge`** — stdio ↔ local-socket relay (`src/bin/reclass-mcp-bridge.rs`),
  requires the `mcp` feature.

## Features

```
default = ["ui", "imports", "disasm", "symbols", "mcp"]
```

| Feature | Pulls | Gates |
|---|---|---|
| `ui` | `gpui`, `gpui_platform`, `gpui-component` | the whole GPUI UI surface (the ONLY feature that pulls gpui) |
| `imports` | `quick-xml`, `object`, `pdb2`, `regex`, `zerocopy` | source/XML/PDB/PE import + export |
| `disasm` | `iced-x86` | x86/x64 disassembly + hex dump |
| `symbols` | `cpp_demangle`, `msvc-demangler`, `reqwest`, `directories`, `regex` | RTTI/demangle/symbol-server |
| `mcp` | `interprocess` | JSON-RPC MCP server |
| `scanner-parallel` (off) | `rayon` | parallel scanner |
| `native-plugins` (off) | `libloading` | OUT-OF-SCOPE live-source plugin loader (stubs only) |

`--no-default-features` yields a **headless engine** build (no gpui) for fast
logic-only testing.

## Building

Requires the toolchain pinned in `rust-toolchain.toml` (stable `1.95.0`, which
also builds gpui; nightly works too — see `_design/gpui_cookbook.md` §1.3).

**Headless engine (no gpui) — fast, portable:**

```sh
cargo build  --no-default-features
cargo test   --no-default-features
```

**Full build (default, incl. the GPUI UI):**

```sh
cargo build
```

### Linux build prerequisites (for the gpui link step)

The full build pulls C/C++ deps (tree-sitter, fontconfig/font-kit, wgpu) that
need system libraries at link time (`_design/gpui_component_cookbook.md` §4):

- `fontconfig-devel` (pulls freetype/harfbuzz) — for the `font-kit` feature
- `libxkbcommon-x11-devel` — for X11
- a discoverable `libstdc++.so`

On Fedora the linker `libstdc++.so` symlink lives under
`/usr/lib/gcc/x86_64-redhat-linux/<ver>/`, not `/usr/lib64`. This repo's
`.cargo/config.toml` already adds that `-L` for `x86_64-unknown-linux-gnu`
(version `16`); if your GCC version differs, either edit that path or export the
env var when building:

```sh
LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16 cargo build
```

Windows uses native DirectX 11 (no Vulkan/wgpu/fontconfig) — these Linux libs
are not needed there.

## Running

The default build produces the GUI app at `target/debug/reclass`. It opens a
GPUI window (titlebar + docks + document tabs + the bespoke editor surface +
workspace/scanner panels + theme) and runs the event loop.

```sh
# Launch to the start page (no project).
./target/debug/reclass

# Open a Reclass project on launch (.rcx native JSON, or .xml ReClass-XML).
./target/debug/reclass path/to/project.rcx

# Open a project AND attach a binary file as its data source.
./target/debug/reclass path/to/project.rcx --data path/to/dump.bin

# CLI help / version (work without a display).
./target/debug/reclass --help
./target/debug/reclass --version
```

The positional `PROJECT` is loaded into the initial document tab once the window
is up (the C++ deferred `project_open(path)` after `window.show()`); `--data`
attaches a binary file as the document's data source. A `.xml` path is routed
through the ReClass-XML importer, anything else through the native `.rcx` JSON
loader; an unreadable/non-project file is refused and the blank document is kept.

Logging honours `RUST_LOG` (defaults to `info`), so the document-lifecycle trace
is visible out of the box:

```sh
RUST_LOG=debug ./target/debug/reclass demo.rcx
```

> **Display required for the GUI.** The window needs a Wayland or X11 display;
> on a headless box run it under a virtual display, e.g.
> `xvfb-run ./target/debug/reclass`. The `--help` / `--version` paths and the
> whole test suite run without a display.

A release build (`cargo build --release`) puts the binary at
`target/release/reclass`.

The MCP stdio bridge is a separate binary (built with the `mcp` feature):

```sh
cargo build --bin reclass-mcp-bridge --no-default-features --features mcp
./target/debug/reclass-mcp-bridge        # relays stdio ↔ the app's local socket
```

## UI dependency note (the one real footgun)

gpui is git-only and gpui-component depends on gpui as an **unpinned** git ref.
We declare `gpui` / `gpui_platform` the **same unpinned way** so the graph has a
single `gpui` source (pinning our own with `rev=` would split it into two
incompatible copies — see `_design/gpui_component_cookbook.md` §3).
Reproducibility comes from the **committed `Cargo.lock`** (it pins the exact gpui
commit); gpui-component itself *is* rev-pinned. Do not add `rev=` to
`gpui`/`gpui_platform`.

## Design docs

All design lives under `_design/` (`ARCHITECTURE.md`, `crate_selection.md`, the
two GPUI cookbooks, and the per-subsystem `understand/` maps). Captured golden
outputs from the original C++ tests live under `_oracle/`.

## Build verification

Independently verified on Linux (Fedora, GCC 16) on 2026-05-31 — **PASS**.

Single package confirmed: `cargo metadata --no-deps --format-version 1` reports
exactly one package `reclass` (targets `lib:reclass`, `bin:reclass`,
`bin:reclass-mcp-bridge`); `workspace_members` is just `reclass@0.1.0`. There is
no `crates/` directory and no `[workspace]` table. The `src/` module layout
matches `_design/ARCHITECTURE.md` §2, and `src/core/` holds real ported types
(e.g. `NodeKind`'s 31 variants in exact `core.h` order and the full `Node`
struct), not stubs.

Exact commands and results:

```sh
# Headless engine (no gpui) — GREEN
cargo build --no-default-features 2>&1 | tail -40
#   Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s   (exit 0)

# Full build (default features incl. ui/gpui) — GREEN
export LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16
cargo build 2>&1 | tail -40
#   Compiling reclass v0.1.0 (/home/loke/reclass-rs)
#   Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.66s   (exit 0)
#   → produces target/debug/reclass and target/debug/reclass-mcp-bridge

# Single-package check — exactly one package "reclass"
cargo metadata --no-deps --format-version 1

# Formatting — clean
cargo fmt --check   # (exit 0, no diff)
```

Both builds are green; no ui/gpui blocker. (Adjust the `LIBRARY_PATH` GCC
version if yours differs — see the Linux build prerequisites above.)

## License

MIT (matching the upstream Reclass project). gpui and gpui-component are
Apache-2.0.
