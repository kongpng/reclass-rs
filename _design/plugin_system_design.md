# Plugin system — design & plan

## End game

Three goals, nothing more:

1. **Parity with the C++ plugin system** — native provider plugins (custom data
   sources / memory readers) with the full `IProviderPlugin` capability surface.
2. **+ the ability for plugins to add UI** — the documented next step the C++
   header anticipates but never built.
3. **+ backwards-compat with existing ReClass.NET plugins** — load both native and
   managed (C#) ReClass.NET plugins as memory sources, exactly as the C++ version's
   `RcNetPluginCompatLayer` does (memory backends only; not managed UI/node plugins).

Hard constraints carried into the design:

- **Plugins must read live process memory.** The killer use case is custom memory
  readers (kernel driver, DMA/PCILeech, hypervisor, remote bridge, emulator). The
  native reader is a deliberate stub (pending the Claude security program); the
  contract must be ready for real readers to drop in.
- **No WASM.** Its sandbox exists to deny host memory access — the opposite of what
  this tool needs. Plugins run **native, in-process** (optional subprocess loader
  for remote/privilege-separated sources).
- **UI stays decoupled from GPUI.** A native plugin built against a different
  gpui/gpui-component version is ABI-incompatible, so plugins contribute UI
  **declaratively** and the host renders it.
- **Rust ABI via `abi_stable`.** No hand-written C ABI for our own plugins (see §2);
  a plain-C header is an optional cross-language add-on, and the C-ABI we *consume*
  for ReClass.NET compat is ReClass.NET's, not ours (see §4).

Out of scope: importers/exporters, custom value formatters, symbol resolvers as
plugin contributions, and bridging managed ReClass.NET *UI/node-type* plugins (the
C++ version doesn't do that either — it only bridges the memory CoreFunctions).

> **Verified C++ behavior** is captured exhaustively (with file:line) in
> `plugin_system_cpp_reference.md`. This section is the parity summary; that doc is
> the source of truth, and the gotchas below correct earlier assumptions.

## 1. The C++ parity surface (the bar)

C++ (`iplugin.h`, `pluginmanager.cpp`): native `.dll`/`.so`/`.dylib` in `Plugins/`,
each exporting `extern "C" CreatePlugin() -> IPlugin*`; `PluginManager` loads via
`QLibrary` (deferred one event-loop tick after first paint, GUI thread) and
auto-registers provider plugins into a global `ProviderRegistry` under
`identifier = Name().toLower().replace(" ","")`.

Several `IProviderPlugin` methods are declared but the C++ host **never calls them**
— matching their *intent* matters more than the literal signature:
- `LoadType()` (Auto/Manual) is **dead** — every DLL in `Plugins/` auto-loads. Our
  port may honor it as an improvement, but that is *beyond* C++.
- `getInitialBaseAddress()` is **never called** — the host takes the base from
  `provider->base()` (and re-evaluates the base-address formula on attach).
- `providesProcessList()`/`enumerateProcesses()` are **not host-called hooks**: the
  plugin's own `selectTarget()` calls its `enumerateProcesses()`, feeds the host's
  `ProcessPicker` custom-list constructor, and returns the chosen target string. So
  plugins depend on the host's picker. Our declarative model replaces this with the
  plugin requesting a host-rendered process-picker dialog (cleaner, decoupled).

| C++ `IPlugin` / `IProviderPlugin` | Rust equivalent |
|---|---|
| `Name/Version/Author/Description/Icon` | `PluginManifest` fields |
| `Type() == ProviderPlugin` | `Contribution::Provider` |
| `LoadType() = Auto / Manual` | `manifest.load = Auto / Manual` |
| `canHandle(target) -> bool` | `ProviderSpec::can_handle(&self, target) -> bool` |
| `createProvider(target) -> Provider` | `create_provider(&self, target) -> Result<Box<dyn Provider>, String>` |
| `getInitialBaseAddress(target) -> u64` | `initial_base_address(&self, target) -> u64` (C++ host actually ignores this; uses `Provider::base()`) |
| `selectTarget(parent) -> target` | `select_target(&self, host) -> Option<String>` (plugin drives a host-rendered picker/dialog via `PluginHost`) |
| `enumerateProcesses()` + `providesProcessList()` | `enumerate_processes(&self) -> Option<Vec<ProcessInfo>>`; the host renders the picker (in C++ the plugin calls the host's `ProcessPicker` itself) |
| `populatePluginMenu(QMenu*)` | source-menu items via `Contribution::Command { slot: SourceMenu }` |
| `PluginManager` load/unload/find/registry | `PluginManager` + the existing `ProviderRegistry` |
| `CreatePlugin()` C export | `abi_stable` root-module export (one `extern "C"` bootstrap symbol) |

The memory-reading capability is the existing **`Provider` trait** (`src/provider/
mod.rs`): a provider plugin returns a `Provider` whose `read(addr, buf)` does the
real native read. Nothing sits between the plugin and the metal.

## 2. Architecture

One host-side contract; loaders are an implementation detail behind it.

```rust
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    fn contributions(&self) -> Vec<Contribution>;
    fn activate(&mut self, host: &mut dyn PluginHost) {}
    fn deactivate(&mut self) {}
    fn handle_command(&mut self, id: &str, args: Value, host: &mut dyn PluginHost) -> CommandResult;
    fn handle_ui_event(&mut self, view: &str, ev: UiEvent, host: &mut dyn PluginHost) -> Option<ViewTree>;
}

pub enum Contribution {
    Provider(ProviderSpec),                       // Goal 1 — full C++ IProviderPlugin parity
    Command { id, title, slot: CommandSlot },     // Goal 2 — Menu|EditorContext|SourceMenu|Toolbar|Palette
    Panel   { id, title, dock: DockSide, initial: ViewTree },
    Dialog  { id, title, initial: ViewTree },     //   modal; generalizes C++ selectTarget
    StatusItem { id, initial: ViewTree },
}
```

`PluginHost` is the controlled callback surface (read the document/tree, add nodes,
set the data source, read provider bytes, resolve a symbol, show a toast, open a
`Dialog`, persist plugin settings).

### Loading model — native-first

| Loader | In-proc speed | Dynamic | Live mem | Isolation | Role |
|---|---|---|---|---|---|
| **In-tree registry** | native | no (recompile) | yes | n/a | bootstrap: built-ins + the API + the future first-party live reader |
| **Native dynamic (`abi_stable`)** | native | **yes** | **yes** | none (full trust) | **the parity path** — third-party providers/UI |
| **ReClass.NET compat** (own loader, §4) | native | yes | yes | none | load existing ReClass.NET native + managed-C# plugins |
| **Subprocess / RPC (MCP)** | IPC latency | yes | yes (own privs) | process-isolated | optional: remote / untrusted / privilege-separated sources |

- **Native dynamic loading achieves our-format parity.** Use **`abi_stable`**
  (`#[sabi_trait]`, `RBox`/`RString`/`RVec`) — Rust types with a stable, documented
  layout plus **load-time layout checks that refuse a mismatched plugin** (strictly
  safer than C++'s raw `dlopen`). The only literal C symbol is the one `extern "C"`
  bootstrap export `abi_stable` generates; you write Rust, not a C API.
- A hand-written **plain-C header** is an *optional* add-on, only if we want C/C++/
  Zig authors to write *our-format* plugins. Skippable for a Rust-only ecosystem.
- **In-tree** is the bootstrap (zero ABI risk, proves the contract).
- **Subprocess** is optional/secondary — remote or untrusted/confined code.

Honest trade-off: a native in-process plugin is **full-trust** (a bug can crash the
host). Inherent to any tool reading arbitrary memory — C++ is the same with fewer
guardrails. We mitigate with `abi_stable` layout checks, disclosed permissions, and
the subprocess option; we don't pretend to sandbox a native reader.

## 3. The "add UI" model — declarative, host-rendered

Plugins describe UI in a small widget vocabulary; the **host renders** it with our
Zed-styled gpui-component widgets and routes events back (Elm-style). Only plain
data crosses the ABI, so native plugins stay decoupled from our GPUI version, and
the same model works for subprocess plugins.

```rust
pub enum ViewTree {
    Column(RVec<ViewTree>), Row(RVec<ViewTree>), Group { title, child },
    Label(RString), Button { id, label }, TextInput { id, value, placeholder },
    Checkbox { id, label, checked }, Dropdown { id, options, selected },
    Table { columns, rows, on_select }, Tree { nodes, on_select },
    KeyValue(RVec<Tuple2<RString, RString>>), Separator,
}   // `abi_stable`-derived so it crosses the native FFI boundary safely
pub enum UiEvent { Clicked(RString), Submitted { id, text }, Toggled { id, on }, RowSelected { table, row } }
```

Surfaces: **menu / context-menu / toolbar / palette / source-menu items** (covers +
generalizes C++ `populatePluginMenu`), **dock panels**, **modal dialogs** (this *is*
C++ `selectTarget`, generalized), and **status-bar items**. In-tree plugins get a
raw-`gpui::AnyView` escape hatch; native-dynamic and subprocess plugins use the
declarative path only (the default, and what we document).

## 4. ReClass.NET plugin compatibility (goal 3)

The C++ version ships this as a *plugin* (`plugins/RcNetPluginCompatLayer/`),
registered as the `reclass.netcompatlayer` provider. We mirror that: a **first-party
"ReClass.NET compat" plugin** in our own system — no special core machinery.

It bridges ReClass.NET's documented native **CoreFunctions** ABI (8 functions, the
memory-backend subset — no debug/breakpoint) into our `Provider`:

```
EnumerateProcesses(cb)                     OpenRemoteProcess(id, access) -> handle
IsProcessValid(handle)                     CloseRemoteProcess(handle)
ReadRemoteMemory(handle, addr, buf, off, size) -> bool      (→ Provider::read)
WriteRemoteMemory(handle, addr, buf, off, size) -> bool     (→ Provider::write)
EnumerateRemoteSectionsAndModules(handle, secCb, modCb)     (→ Provider::enumerate_regions)
ControlRemoteProcess(handle, action)       // suspend/resume/terminate
```
ABI notes to honor exactly: `__stdcall` on Win32 (plain on others); `RC_Pointer =
void*`, `RC_Size = u64`; UTF-16 (`char16_t`) fixed-size name/path arrays in the
`#pragma pack(1)` callback structs (`EnumerateProcessData`, `EnumerateRemoteSection
Data`, `EnumerateRemoteModuleData`); enumeration is **callback-based** (the plugin
calls our callback per process/section/module — we collect into Vecs).

Two sub-paths, matching C++:

- **Native ReClass.NET plugins** (DLLs exporting the 8 functions): load with
  **`libloading`**, resolve the exports into a `RcNetFunctions`-style fn table, wrap
  as a `Provider`. **Cross-platform** (Linux/macOS ReClass.NET native plugins exist).
- **Managed C# ReClass.NET plugins**: **host the .NET Framework CLR + a C# bridge**,
  exactly like C++'s `ClrHost`. ReClass.NET targets **.NET Framework 4.x (Windows)**,
  so this path is **Windows-only**: host via `mscoree`/`ICLRMetaHost ->
  ICLRRuntimeHost::ExecuteInDefaultAppDomain` (called from Rust through the `windows`
  crate / direct COM), loading a C# bridge assembly that marshals the managed
  plugin's `ICoreProcessFunctions` into the same 8 native function pointers → same
  `Provider` wrapper. We can **port the existing `RcNetBridge.cs`** verbatim (it's a
  pure C# bridge, runtime-agnostic to who hosts it).

Required vs optional (match C++): **required** = `ReadRemoteMemory`,
`OpenRemoteProcess`, `CloseRemoteProcess`, `EnumerateProcesses`; the other 4 are
nullptr-tolerant. Caveats to replicate (or improve on — see the reference doc §8):
the C++ provider opens with `ProcessAccess::Full`, returns a **hardcoded `0x10000`
size sentinel**, hardcodes **`pointer_size = 8`** (no target-bitness detection),
exposes **no `enumerate_regions`** (only caches modules for symbol resolution), and
**leaks managed plugins** (no AppDomain unload). We should at least fix bitness +
region enumeration in the port.

Scope (matches C++): **memory backends only.** Managed UI/node-type ReClass.NET
plugins are *not* bridged — they target ReClass.NET's managed model + WinForms.

Discovery (matches C++): **interactive, not a folder scan** — `select_target`
pops a file dialog to pick a ReClass.NET `*.dll`, validates/loads it (PE CLR-header
check decides native vs managed), then shows the bridged process list. The compat
plugin registers under our `reclass.netcompatlayer` identifier (icon `plug.svg`).
Target string format: `"<dllpath>|<pid>:<name>"`.

Managed path specifics (from the reference): host **.NET Framework v4.0.30319** via
`mscoree`/COM and call the C# bridge entry `RcNetBridge.Bridge.Initialize("<hexptr>|
<pluginpath>")`; the bridge marshals `ICoreProcessFunctions` into 8
`[UnmanagedFunctionPointer(StdCall)]` thunks written into the native table. We can
**port `RcNetBridge.cs` ~verbatim**; the open decision (§7) is .NET Framework via
`windows`-crate COM (exact parity, Windows-only) vs also `netcorehost` for modern .NET.

## 5. Discovery, manifest, management

- `plugins/<name>/plugin.toml`: `name`, `version`, `author`, `description`,
  `kind = builtin|native|reclassnet|process`, `entry`, `load = auto|manual`,
  `permissions = [read_memory, write_memory, network, filesystem, add_ui, add_provider]`.
- **Manage Plugins** dialog. *C++ baseline*: a list (Name/Version/Description/Type/
  Author/Icon) with **load-from-path** + **unload-selected** (literal dlopen/dlclose)
  and **no enable/disable, no persistence, no auto/manual** — the loaded set is just
  "DLLs in the folder" + runtime manual load/unload, forgotten on restart. *Our
  improvements (beyond C++)*: enable/disable + per-plugin persistence, honoring
  `load = auto|manual`, declared-permission disclosure, reload, and ABI-mismatch
  error surfacing. Keep C++'s load-from-path + unload.
- Permissions = disclosure + consent for native plugins (not a hard sandbox);
  subprocess plugins can be OS-confined. Prefer disable over runtime unload for
  native (`dlclose` is unsafe if state escaped — C++ has the dangling-provider risk;
  we detach affected docs first); fully unload on restart.

## 6. Implementation plan

**Phase 1 — Contract + in-tree registry (no loader risk).**
`src/plugin/`: `Plugin`, `Contribution`, `ProviderSpec`, `PluginManifest`,
`PluginHost`, `ViewTree`, `UiEvent`, `CommandSlot`, `DockSide`. `PluginManager`
wiring `Provider` contributions into the existing `ProviderRegistry`. Reimplement
File/Buffer/Snapshot/Null as in-tree plugins. Make the Source picker + Manage
Plugins dialog read the real registry (enable/disable). *Deliverable: built-ins flow
through the plugin path; dialog is real.*

**Phase 2 — Declarative UI host.** Render `ViewTree` → Zed-styled widgets for a
`Command` (menu/context/source-menu), one `Panel` (dock), one `Dialog` (modal);
route events/commands back. Re-express a built-in's target selection as a `Dialog`
(proves `select_target`). *Deliverable: an in-tree demo plugin adds a menu item, a
panel, and a dialog.*

**Phase 3 — Native dynamic loader (our-format parity).** `abi_stable` boundary
(`Plugin`/`PluginHost`/`Contribution`/`ViewTree` stable-ABI), root-module export,
version/layout check on load. Discover + load `kind = native` from `plugins/`;
load-from-path. Ship example native provider + UI plugins; optionally a C header for
C/C++ authors. *Deliverable: the §1 parity table satisfied by a real loaded `.so`/`.dll`.*

**Phase 4 — ReClass.NET native compat (goal 3a).** The compat plugin: `libloading`
ReClass.NET native DLLs, resolve the 8 CoreFunctions (mind `__stdcall`), callback-
based enumeration → Vecs, wrap as a `Provider`. Register under `reclass.netcompatlayer`.
*Deliverable: a real ReClass.NET native plugin loads and reads memory, cross-platform.*

**Phase 5 — ReClass.NET managed-C# compat (goal 3b, Windows-only).** Add the CLR
host (mscoree COM via the `windows` crate) + port `RcNetBridge.cs`; managed plugin's
`ICoreProcessFunctions` → the same `Provider`. *Deliverable: a real C# ReClass.NET
memory-backend plugin loads on Windows.*

**Phase 6 — Management + permissions polish.** Manifest parsing, permission
disclosure, auto/manual, reload, per-plugin enable persistence.

**Phase 7 — (Optional) subprocess/MCP loader** for remote/privilege-separated providers.

**Live reader** drops in as a first-party native provider on the Phase 1 API once the
backend is unstubbed; third parties ship readers via Phase 3 with no host changes.

## 7. Improvements over the C++ system (exhaustive)

Tags: **[fix]** = corrects a verified C++ wart while keeping the intent; **[+]** =
beyond-C++ enhancement recommended for the initial build; **[future]** = optional,
architecture leaves room. Phase references point at §6.

### A. Correctness fixes to verified C++ warts (see reference §10)
- **[fix]** Honor `load = auto|manual` (C++ ignores `LoadType` and auto-loads every
  DLL). Manual plugins load on demand / on enable. (Phase 6)
- **[fix]** Persist enabled/disabled + load-from-path set across restarts (C++ keeps
  nothing — the set is just "DLLs in the folder", forgotten each run). (Phase 6)
- **[fix]** **Safe unload**: track which open documents use a provider and detach them
  before unloading the backing library (C++ unloads with the provider still live — a
  dangling-pointer crash it only *warns* about). Prefer disable-not-unload while in
  use; truly unload on restart. (Phase 3/6)
- **[fix]** Centralize the identifier derivation (`Name().toLower().replace(" ","")`
  is duplicated in 3 C++ call sites that must stay in sync) into one helper. (Phase 1)
- **[fix]** One shared provider-list model for *both* source surfaces (the menubar
  Data-Source menu and the inline popup) — C++ has two icon/label tables that already
  diverge (`kernelmemory` present in one, missing in the other). (Phase 1)
- **[fix]** Structured, surfaced errors (load failure, ABI/version mismatch, attach
  failure) shown in the dialog/toast with detail — C++ logs to the console and shows a
  generic "check the console" box. (Phase 3/6)
- **[fix]** Drop or actually use `getInitialBaseAddress` (C++ declares it, never calls
  it). We standardize on `Provider::base()` + base-formula re-eval. (Phase 1)
- **[fix]** ReClass.NET compat provider: detect target **bitness** instead of
  hardcoding `pointer_size = 8`; expose real **`enumerate_regions`** instead of a
  `0x10000` size sentinel + discarded sections; **don't leak managed plugins** (unload
  the AppDomain / recycle the host). (Phase 4/5)

### B. Safety & robustness
- **[+]** Wrap every plugin entry point in `catch_unwind` so a plugin panic surfaces as
  a disabled plugin + error, not a host crash (C++: a plugin crash takes down the app).
- **[+]** Read **watchdog/timeout**: a hung provider `read()` must not freeze the UI —
  run reads off the GUI thread with a timeout and a "provider not responding" state
  (C++ does all reads synchronously on the GUI thread). (Phase 1 host plumbing)
- **[+]** `abi_stable` runtime **layout + API-semver check** refuses incompatible
  plugins gracefully (vs C++ silent `dlopen` UB). (Phase 3)
- **[future]** **Subprocess isolation** for untrusted plugins: a crash/leak is
  contained and restartable. (Phase 7)

### C. Loading & lifecycle
- **[+]** **Lazy load**: instantiate a plugin only when first used (provider selected),
  not all at startup (C++ defers one tick but still loads everything).
- **[+]** **Multiple plugin dirs** with precedence: bundled, system, and a user dir
  (`~/.config/reclass/plugins`) — C++ only checks `Plugins/` beside the exe.
- **[+]** **API-version field** in the manifest; host refuses/ warns on mismatch with a
  clear message (complements the abi_stable layout check).
- **[future]** **Hot reload** (watch the plugins dir) for plugin authors — safe for
  subprocess, dev-only for native.

### D. Provider capability (memory backends)
- **[+]** **Capability negotiation**: a provider advertises which optional features it
  supports (write, regions, symbols, kernel paging, debug events) so the UI greys out
  unsupported actions precisely (C++ infers ad hoc).
- **[+]** **Batched / vectored reads**: coalesce the many small per-field reads a
  refresh tick issues into one request — a big win for subprocess/remote providers
  (C++ reads one field at a time). Zero-copy `&[u8]`/`RVec<u8>` across the ABI.
- **[+]** **Provider read cache** with refresh-tick invalidation.
- **[future]** **Debug events / hardware breakpoints**: ReClass.NET's CoreFunctions
  include 5 debug functions C++ deliberately omits — a richer provider trait could
  expose them.
- **[future]** **Change notifications**: a provider that can push "memory changed"
  instead of pure polling.

### E. UI extension (the "add UI" goal)
- **[+]** Plugin UI is **auto-themed** by the host (free with the declarative model;
  C++ plugins hand-roll Qt styling).
- **[+]** **Plugin settings pages** rendered into the Options dialog from a declarative
  schema.
- **[+]** **Command-palette** contributions from plugins.
- **[+]** **Editor decorations**: a plugin can annotate nodes (badges/colors/inline
  hints) — e.g. an RTTI plugin tagging vtables.
- **[+]** **Background-task + progress UI** for long-running plugin work.
- **[future]** Richer widgets (value-history charts, hex view, live-eval address
  input), drag-drop, plugin-provided context menus, i18n of plugin strings.

### F. New contribution kinds (beyond C++'s provider-only)
*(Architecture allows these; out of the initial three goals — list as `Contribution`
growth, all [future].)*
- Importer / Exporter plugins (codegen targets + import formats).
- Custom value formatters / node renderers (GUID, timestamps, domain encodings).
- Symbol / RTTI resolver plugins.
- Scanner plugins (custom scan types / comparators).
- Address-expression function plugins (custom functions in the expression grammar).
- Theme plugins; automation/transform plugins over the node tree.

### G. Developer experience / SDK (C++ ships only headers)
- **[+]** A published **`reclass-plugin` SDK crate**: the trait + `abi_stable` glue +
  a `#[plugin]` proc-macro that generates the export boilerplate. (Phase 3)
- **[+]** **Example plugins** (provider, UI panel) + a `cargo-generate` template.
- **[+]** **`cbindgen`-generated C header** for cross-language authors (optional).
- **[+]** **Mock `PluginHost` + a conformance test-suite** a plugin can run against.
- **[future]** Plugin **dev mode**: hot reload, verbose per-plugin logs, a test harness.

### H. Distribution & management
- **[+]** A **plugin bundle** layout (manifest + artifact + icon/assets) per
  `plugins/<name>/`.
- **[+]** Per-plugin **enable/disable + ordering** in Manage Plugins, persisted.
- **[future]** Update check / install-from-URL / a small registry.

### I. Cross-platform
- **[+]** ReClass.NET **native** compat is **cross-platform** (the C++ compat layer is
  Windows-only because of `mscoree`; the native CoreFunctions path is not — Linux/macOS
  ReClass.NET native plugins like GDB readers become loadable). (Phase 4)
- **[future]** Managed compat via **`netcorehost` (.NET 5+/cross-platform)** as an
  option alongside the Windows-only .NET Framework path, for plugins targeting modern
  .NET. (Phase 5 decision)
- **[+]** One `abi_stable` ABI consistent across OSes.

### J. Performance
- **[+]** Lazy load (C), batched/zero-copy reads + read cache (D), off-GUI-thread reads
  (B) — collectively keep a fast scanner/refresh loop a plugin can't stall.
- **[+]** Avoid the per-read managed `byte[]` allocation the C++ ReClass.NET bridge
  does; reuse buffers.

### K. Testing & quality
- **[+]** Golden tests for declarative-UI rendering; unit tests for each built-in
  plugin on the new contract; fuzz the plugin ABI boundary.

> **Recommended for the initial build** (cheap, high value, mostly parity-correctness):
> all **[fix]** items, plus B (catch_unwind + read watchdog + layout check), D
> (capability negotiation + batched reads), E (auto-theme + settings pages), G (SDK
> crate + examples), and I (cross-platform native compat). The **[future]** items are
> deferred without changing the contract.

## 8. Decisions to confirm

- **`abi_stable` vs `stabby`** for our native ABI. Recommend `abi_stable` (mature).
- **C header for our-format plugins**: ship it (C/C++/Zig authors) or Rust-only? Default Rust-only.
- **Managed-C# CLR hosting**: mirror C++ exactly (.NET Framework via `mscoree` COM +
  port `RcNetBridge.cs`, Windows-only) — confirmed by "what the C++ supports". Open:
  do we *also* try .NET 5+ via `netcorehost` for plugins that target modern .NET, or
  strictly match ReClass.NET's .NET Framework target?
- **`Provider` across the ABI**: `RBox<dyn Provider_TO>` (sabi trait object, so
  plugin `read()` is a direct native call on the hot path). Recommend yes.

## 9. Summary

- **Goal 1 (our-format parity):** native provider plugins with the full
  `IProviderPlugin` surface and in-process **live-memory** access — §1 table, via the
  in-tree (Phase 1) + native `abi_stable` (Phase 3) loaders.
- **Goal 2 (add UI):** declarative, host-rendered contributions — menus, panels,
  dialogs (incl. the generalized target picker), status items — decoupled from GPUI.
- **Goal 3 (ReClass.NET compat):** a first-party compat plugin bridging ReClass.NET's
  8 native CoreFunctions to `Provider` — native plugins cross-platform (Phase 4),
  managed-C# plugins Windows-only via a hosted .NET Framework CLR + the ported C#
  bridge (Phase 5), memory backends only — exactly what the C++ version supports.
- **Native, no WASM**, `abi_stable` for a safer-than-C++ boundary, subprocess as an
  optional isolation path, built incrementally so the contract + built-ins land first
  and each loader extends it without changing the contract.
