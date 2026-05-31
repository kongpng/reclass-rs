# Reclass-rs — Crate Selection

Dependency selection for the C++/Qt6 → Rust port of Reclass
(github.com/IChooseYou/Reclass, MIT). Goal: idiomatic, well-maintained,
cross-platform crates so we hand-roll as little as possible while keeping
1:1 behavioral parity with the original public source.

All versions verified against crates.io on 2026-05-31. Targets: Windows
(primary), macOS, Linux. Only Linux is compile-verifiable here, so all
OS-specific crates must sit behind `#[cfg(...)]` and keep compiling on
every target.

Out of scope (do NOT pull crates for): the live OS process / kernel /
remote / WinDbg memory data-source plugins under `plugins/`. Those are
documented stubs behind the `Provider` trait. Only the benign built-in
data sources (file, in-memory buffer, snapshot, null) are implemented.

Legend: ✅ verified on crates.io · maturity = downloads + recency of last
release.

---

## Selection table

| Need (C++ origin) | Primary crate | Version | License | Cross-platform | Maturity (downloads / last release) | Alternatives |
|---|---|---|---|---|---|---|
| PDB parsing (`third_party/raw_pdb`, `src/imports/import_pdb.cpp`, `src/names/pdb_*_provider.cpp`) | **pdb2** | 0.10.1 | MIT OR Apache-2.0 | yes (pure Rust, parses files on any OS) | 0.3M dl, 2025-11-07 — maintained fork | `pdb` 0.8.0 (5.7M dl but UNMAINTAINED since 2022-06); `symbolic-debuginfo` (heavier) |
| PE / object / debug directories — file path (`pe_debug_info.cpp` file mode) | **object** | 0.39.1 | Apache-2.0 OR MIT | yes | 481M dl, 2026-04-21 | `goblin` 0.10.7 (52M dl, 2026-05-28); `pelite` |
| PE debug-directory extraction over a `Provider` (in-memory reads, `pe_debug_info.cpp`) | **hand-roll** (`zerocopy`/`bytemuck` for struct casts) | bytemuck 1.x / zerocopy 0.8.x | MIT/Apache/BSD | yes | n/a | `object` cannot parse from a non-contiguous `Provider`; the C++ already hand-parses MZ/PE/CV-RSDS via discrete reads — mirror that |
| x86/x64 disassembly (`third_party/fadec`, `src/disasm.cpp`) | **iced-x86** | 1.21.0 | MIT | yes (pure Rust) | 2.0M dl, 2024-01-20 | `capstone` 0.x (C bindgen, build complexity); `yaxpeax-x86` (pure Rust, fast) |
| C++ demangling — Itanium (`rtti.cpp` `__cxa_demangle`) | **cpp_demangle** | 0.5.1 | MIT OR Apache-2.0 | yes (pure Rust, no libstdc++) | 74M dl, 2025-11-05 | `symbolic-demangle` 13.1.1 (one crate for both ABIs) |
| C++ demangling — MSVC (`symbol_demangle.cpp` `UnDecorateSymbolName`) | **msvc-demangler** | 0.11.0 | MIT/NCSA | yes (pure Rust — works off-Windows, unlike the C++ which was Win-only) | 12M dl, 2025-02-06 | `symbolic-demangle` 13.1.1 |
| Unified demangler facade (optional) | **symbolic-demangle** | 13.1.1 | MIT | yes | 51M dl, 2026-05-28 (Sentry) | wraps cpp_demangle + msvc-demangler; use directly if we want one dep |
| C/C++ source import parser (`src/imports/import_source.cpp`) | **HAND-ROLL** (port the existing tokenizer + recursive-descent parser) | n/a | n/a | yes | n/a | tree-sitter-cpp / clang are massive overkill: the C++ is a ~1600-line bespoke tokenizer tuned to ReClass semantics (offset comments `0x..`, Win typedefs, bitfields). Use `regex` 1.x only for the offset-comment regexes |
| XML import (`import_reclass_xml.cpp`, `QXmlStreamReader`) | **quick-xml** | 0.40.1 | MIT | yes | 287M dl, 2026-05-15 | `roxmltree` 0.21.1 (DOM, read-only) |
| XML export (`export_reclass_xml.cpp`, `QXmlStreamWriter`) | **quick-xml** (`Writer`) | 0.40.1 | MIT | yes | same | hand-rolled writer (trivial) |
| JSON — native `.rcx` + MCP JSON-RPC (`mcp_bridge.cpp` `QJsonDocument`) | **serde** + **serde_json** | serde 1.x / serde_json 1.0.150 | MIT OR Apache-2.0 | yes | 939M dl, 2026-05-21 | none warranted |
| Local IPC for MCP bridge (`mcp_bridge.cpp` `QLocalServer`/`QLocalSocket`) | **interprocess** | 2.4.2 | 0BSD OR Apache-2.0 | yes (Win named pipes + Unix domain sockets — exactly the QLocalServer mapping) | 9.7M dl, 2026-04-19 | std `UnixListener` (Unix only) + manual `windows`-crate pipe (Win); `tokio::net` if we go async |
| Fuzzy matching (`src/widgets/fuzzy_match.h`, command palette, type picker) | **HAND-ROLL** (port the two-pass scorer) | n/a | n/a | yes | n/a | The C++ scorer is bespoke (Pass 1 contiguous substring with prefix/separator/CamelCase tiers; Pass 2 word-start initials with digit-boundary rules) and returns exact highlight positions. Off-the-shelf matchers change ranking → breaks parity. If a library is wanted: `nucleo` 0.5.0 (MPL-2.0, weak copyleft) or `fuzzy-matcher` 0.3.7 (MIT but stale since 2020) |
| Symbol-server download (`src/symbol_downloader.cpp`, `QNetworkAccessManager`) | **reqwest** (blocking feature) | 0.13.4 | MIT OR Apache-2.0 | yes | 503M dl, 2026-05-25 | `ureq` 3.3.0 (134M dl, lighter, no async/tokio — strong fit if we avoid tokio elsewhere) |
| TLS backend for the downloader | **rustls** (via reqwest/ureq `rustls-tls`) | rustls 0.23.x | MIT/Apache/ISC | yes (no OpenSSL build dep) | very high | native-tls (schannel/secure-transport/openssl) |
| Dynamic plugin loading (`src/pluginmanager.cpp`, `QLibrary` + `extern "C" CreatePlugin`) | **libloading** | 0.9.0 | ISC | yes | 386M dl, 2025-11-05 | `abi_stable` (only if we define a stable Rust plugin ABI — not needed: existing ABI is a C `CreatePlugin()` entrypoint) |
| Containers / hashing (QHash/QSet/QMap) | **std** `HashMap`/`HashSet`/`BTreeMap` + **ahash** + **indexmap** | ahash 0.8.12 / indexmap 2.14.0 | MIT OR Apache-2.0 | yes | ahash 595M / indexmap 1.1B dl | `hashbrown` (std already uses it); `fxhash` |
| Error type (libraries / subsystems) | **thiserror** | 2.0.18 | MIT OR Apache-2.0 | yes | 1.04B dl, 2026-01-18 | snafu, eyre |
| Error type (app / fallible glue) | **anyhow** | 1.0.102 | MIT OR Apache-2.0 | yes | 709M dl, 2026-02-20 | eyre/color-eyre |
| Logging / diagnostics (`qDebug`/`qWarning`) | **tracing** + **tracing-subscriber** | tracing 0.1.44 | MIT | yes | 624M dl, 2025-12-18 | `log` + `env_logger` |
| CLI argument parsing (`src/main.cpp` arg handling) | **clap** (derive) | 4.6.1 | MIT OR Apache-2.0 | yes | 861M dl, 2026-04-15 | `lexopt`, `pico-args` |
| Config / cache dirs (`QStandardPaths::AppLocalDataLocation` for SymbolCache) | **directories** | 6.0.0 | MIT OR Apache-2.0 | yes (Known Folder API / XDG / macOS dirs) | 55M dl, 2025-01-12 | `dirs` 6.x (lower-level) |
| Async runtime | **none required** — prefer blocking IPC/HTTP on worker threads | n/a | n/a | yes | n/a | `tokio` 1.x only if interprocess/reqwest are run in async mode. The MCP bridge is event-loop/line-driven and the downloader is a single GET — both fit blocking + a thread |
| Parallel memory scanning (`src/scanner.cpp` `QtConcurrent::run`) | **rayon** | 1.x | MIT OR Apache-2.0 | yes | very high | std threads + channels; `threadpool` |
| POD struct ↔ bytes casts (PE/PDB/CodeView headers, value formatting) | **bytemuck** (+ optionally **zerocopy**) | bytemuck 1.x | MIT/Apache/Zlib | yes | very high | manual `from_le_bytes`; `scroll` (used by goblin) |
| Regex (offset-comment parsing in source import; RTTI name cleanup) | **regex** | 1.x | MIT OR Apache-2.0 | yes | very high | hand-rolled scanners |

---

## Per-need notes & rationale

### PDB parsing — `pdb2`, not `pdb`
`import_pdb.cpp` (1411 lines) walks the TPI/DBI/Info streams of a PDB via
RawPDB to build types and symbols. The natural Rust replacement is the
`pdb` crate, **but `pdb` 0.8.0 has been unmaintained since June 2022**.
`pdb2` is the actively-maintained community fork (last release
2025-11-07), same API surface (TPI/DBI/symbol/type iteration), MIT/Apache.
PDB files are Windows-toolchain artifacts but the *parser* is pure Rust
and compiles/runs on every target, so PDB import can stay available
cross-platform (an improvement the C++ gated behind `_WIN32`). Keep PDB
import behind a `cfg`/feature only if we want to match the C++ gating;
the crate itself imposes no OS restriction.

### PE debug info — split into two paths
`pe_debug_info.cpp` does **not** parse a file with a PE library; it issues
discrete `Provider::read()` calls against a (possibly live, possibly
in-memory) module base, hand-decoding DOS → PE → optional header → debug
data directory → CodeView `RSDS` record. Because the bytes come through
the abstract `Provider`, neither `object` nor `goblin` (which expect a
contiguous slice/file) can drive it. Port this routine by hand using
`bytemuck`/`zerocopy` for the `#[repr(C, packed)]` header casts, exactly
mirroring the C++ struct layout and the mixed-endian GUID formatting.
Use **`object`** (preferred over goblin: far larger user base, first-class
PE + COFF + debug-dir support, dual MIT/Apache) only where we parse a PE
file straight off disk.

### Disassembly — `iced-x86`
`disasm.cpp` calls fadec's `fd_decode`/`fd_format` to produce
`addr  mnemonic operands` lines for 32/64-bit code. `iced-x86` is the
mature pure-Rust x86/x64 decoder+formatter with full Intel/NASM/GAS/masm
formatters, exact instruction lengths, and an RIP base address — a direct
behavioral match for the fadec loop. Pure Rust → no C build step, clean
cross-compile. `capstone` works but adds a C dependency and bindgen build
complexity; `yaxpeax-x86` is a viable pure-Rust alternative if iced's
binary size becomes a concern.

### Demangling — two pure-Rust crates (and a strictly-better-than-C++ result)
The C++ uses libstdc++ `__cxa_demangle` for Itanium (`rtti.cpp`) and the
Win32 `UnDecorateSymbolName` for MSVC (`symbol_demangle.cpp`, Windows-only,
explicitly returning raw names off-Windows). In Rust:
- Itanium → **`cpp_demangle`** (pure Rust, no libstdc++ link).
- MSVC → **`msvc-demangler`** (pure Rust) — this makes MSVC names from
  imported PDBs demangle on Linux/macOS too, removing the C++'s
  Windows-only limitation while preserving output behavior.
- The dispatch in `humanizeSymbolName` (RTTI `.?A`/`?A` → in-house RTTI
  parser; `_Z*` → Itanium; `?`/`_?` → MSVC) is ported as-is.
- `symbolic-demangle` (Sentry, very active) wraps both behind one API and
  is the alternative if we prefer a single dependency.

### C/C++ source import — HAND-ROLL (do not pull tree-sitter/clang)
`import_source.cpp` (1624 lines) is a self-contained tokenizer + recursive
-descent parser purpose-built for ReClass: it recognizes a curated type
table (stdint, C, Win32 typedefs, multi-word types), `0x...` offset
comments via small regexes, bitfields, nested/forward structs, arrays and
pointers, and maps them onto `NodeKind`. tree-sitter-cpp or libclang would
parse far more than this DSL needs, change error behavior, and add a heavy
(and for clang, native) dependency. Port the existing tokenizer/parser
1:1; use **`regex`** only for the handful of offset-comment patterns.

### XML — `quick-xml` for both directions
Import uses `QXmlStreamReader` (pull parser) and export uses
`QXmlStreamWriter`. `quick-xml` provides both a streaming `Reader` and a
`Writer`, matching the C++ streaming style closely (and the version-keyed
integer→`NodeKind` type maps port as plain tables). `roxmltree` is a fine
read-only DOM alternative but has no writer, so it can't cover export.

### JSON — serde / serde_json
Covers the native `.rcx` document format and the MCP JSON-RPC 2.0 wire
format (`QJsonDocument`/`QJsonObject` throughout `mcp_bridge.cpp`). Note
the C++ parses integers leniently (hex `0x..` or decimal strings as well
as JSON numbers) in `parseInteger`; reproduce that with a custom
deserialize helper rather than relying on default number parsing.

### MCP local IPC — `interprocess`
`mcp_bridge.cpp` listens on a `QLocalServer` named `"ReclassMcpBridge"`
(named pipe on Windows, Unix domain socket on Linux/macOS) and exchanges
newline-delimited JSON-RPC. `interprocess` is the cross-platform crate
that abstracts exactly this (`LocalSocketListener`/`LocalSocketStream`
over Win named pipes + Unix sockets), so the Windows/Unix split the C++
gets from Qt is preserved by one dependency. Run it blocking on a worker
thread to mirror the single-threaded serial request queue in the C++
(`m_processing` / `m_pendingRequests`); no async runtime required.

### Fuzzy matching — HAND-ROLL the scorer
`fuzzy_match.h` documents a deliberately *strict* two-pass algorithm
(contiguous case-insensitive substring with scored tiers for
prefix/separator/CamelCase, then word-start-initials with explicit
digit-boundary rules), replacing an older loose subsequence matcher that
produced false positives in large symbol lists. It also emits the exact
matched character indices for highlight painting. Ranking and highlight
positions are observable behavior, so port the algorithm verbatim. If a
library is later desired, `nucleo` (fast, but MPL-2.0 weak copyleft and a
different ranking) or `fuzzy-matcher` (MIT but stale since 2020) are the
options — both would change results and break parity.

### Symbol download — `reqwest` (or `ureq`) + `rustls`
`symbol_downloader.cpp` performs a single HTTPS GET to
`msdl.microsoft.com/download/symbols/{pdb}/{GUID}{age}/{pdb}` with the
`Microsoft-Symbol-Server/10.0.0.0` User-Agent, follows safe redirects,
checks HTTP 200, and writes into a `SymbolCache` dir. Either client works;
**`reqwest`** is the default (largest ecosystem, blocking + async). If we
want to avoid pulling tokio, **`ureq` 3.3.0** is a clean blocking-only fit.
Use the **`rustls`** TLS backend to avoid an OpenSSL/system-TLS build
dependency on the cross-compiled Windows/macOS targets.

### Plugin loading — `libloading`
`pluginmanager.cpp` scans a `Plugins` dir for `*.dll`/`*.dylib`/`*.so`,
`dlopen`s each, resolves the C entrypoint `CreatePlugin`, and calls it.
`libloading` is the standard safe wrapper over `dlopen`/`LoadLibrary` and
maps directly. `abi_stable` is unnecessary because the existing ABI is a
plain C function returning a vtable object — not a Rust-to-Rust ABI.
(Reminder: plugin *implementations* are out of scope; only the loader and
the `CreatePlugin` resolution are ported, and even that is gated behind the
`Provider`-trait boundary.)

### Containers / hashing
Replace `QHash`→`HashMap`, `QSet`→`HashSet`, `QMap`→`BTreeMap`. Where the
C++ relies on insertion order (some name/type registries iterate in stable
order) use **`indexmap`**. Use **`ahash`** as the default hasher for hot
maps (faster than SipHash; keep SipHash only where DoS-resistance matters,
which it doesn't for in-process editor state).

### Errors / logging / CLI / dirs
Idiomatic stack: **thiserror** for typed subsystem errors, **anyhow** at
the app boundary, **tracing**(+subscriber) to replace `qDebug`/`qWarning`
(the C++ logs liberally, e.g. `[MCP] ...`), **clap** for CLI, and
**directories** to replace `QStandardPaths` (notably the SymbolCache
under `AppLocalDataLocation`).

### Parallelism / async
The scanner (`scanner.cpp`) offloads work with `QtConcurrent::run` (a
thread pool over CPU-bound memory scans) — map to **rayon**, not an async
runtime. The MCP bridge and downloader are I/O that fits blocking calls on
worker threads. Adopt **tokio** only if we later choose the async variants
of `interprocess`/`reqwest`; it is not required for parity.

---

## Additional per-subsystem needs discovered

| Need (C++ origin) | Suggested crate | Notes |
|---|---|---|
| Source/struct code editor widget (`third_party/qscintilla`, `editor.cpp`, used for the C/C++ import editor + type editing) | depends on chosen GUI toolkit | QScintilla is a Qt syntax-highlighting code editor. Its replacement is dictated by the Rust UI layer (out of this dependency pass). If the UI is egui → `egui_code_editor`; if a Scintilla binding is wanted → `scintilla`/`scintilla-sys` (native). Flag for the UI design doc. |
| Syntax highlighting (if not provided by the editor widget) | `syntect` or `tree-sitter-highlight` | Only if the editor widget lacks built-in C/C++ highlighting. |
| Address/number parsing (`src/addressparser.cpp`, hex/decimal/expression) | hand-roll (small) | Bespoke parser; no crate needed. The MCP `parseInteger` lenient hex/dec rule lives here too. |
| Memory-mapped file reads for file data source + PDB (`MappedFile` in `import_pdb.cpp`) | `memmap2` | Cross-platform `mmap`/`MapViewOfFile` wrapper; replaces the hand-rolled `CreateFileMappingW`/`mmap`. |
| Half-precision / vector value formatting (`format.cpp`, Vec2/3/4, Mat4x4, float16 if any) | `half` (only if f16 is used) | Most formatting is hand-rolled; pull `half` only if a float16 node kind exists. |

---

## Summary of explicit HAND-ROLL decisions (parity-critical)

1. **C/C++ source import parser** — port the bespoke tokenizer/recursive-
   descent parser; do not adopt tree-sitter/clang.
2. **Fuzzy matcher** — port the two-pass strict scorer + highlight
   positions; do not adopt nucleo/fuzzy-matcher.
3. **PE-over-Provider debug-info extraction** — hand-decode headers via
   `bytemuck`/`zerocopy`; `object`/`goblin` only for on-disk PE files.
4. **Address/number parser** — small bespoke parser, no crate.
