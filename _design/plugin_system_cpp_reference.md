# C++ Reclass plugin system — complete verified reference

Captured from a full read of the C++ source (`/home/loke/Documents/Reclass`).
File:line citations are to that tree. This is the authoritative "what the C++
actually does" reference behind the design doc. (The four bundled memory-reader
plugins' *internals* were not deep-read — a usage-policy filter blocked the
low-level memory code — but their *SDK usage* is fully captured from the host +
registry integration, which is what the port needs.)

## 0. Shape

- Plugins are external shared libs (`.dll`/`.so`/`.dylib`) in a `Plugins/` folder
  beside the exe (macOS also `Contents/PlugIns`), each exporting
  `extern "C" RCX_PLUGIN_EXPORT IPlugin* CreatePlugin()` (`iplugin.h:21,143`).
  `RCX_PLUGIN_EXPORT` = `__declspec(dllexport)` / `visibility("default")` (`iplugin.h:7-11`).
- One plugin type exists: `ProviderPlugin` (`iplugin.h:35-42`). Provider plugins are
  auto-registered into a global `ProviderRegistry` and surface as data sources.
- `IPLUGIN_IID = "com.reclass.IPlugin/1.0"` (`iplugin.h:145`) is **vestigial** — the
  single definition has no consumers (not wired to Qt plugin metadata).

## 1. The interface — `IPlugin` / `IProviderPlugin` (`iplugin.h`)

`IPlugin`: `Name/Version/Author/Description` (`std::string`), `QIcon Icon()` (default
empty), `k_EType Type()`, `k_ELoadType LoadType()` (`:23-56`). `k_EType =
{ProviderPlugin}`; `k_ELoadType = {Auto, Manual}`.

`IProviderPlugin : IPlugin` (`:83-140`):
- `Type()` → `ProviderPlugin` (`:85`)
- `bool canHandle(const QString& target)` (`:92`)
- `unique_ptr<rcx::Provider> createProvider(target, QString* errorMsg)` (`:100`)
- `uint64_t getInitialBaseAddress(target)` default 0 (`:108`)
- `bool selectTarget(QWidget* parent, QString* target)` (`:116`)
- `QVector<PluginProcessInfo> enumerateProcesses()` default {} (`:126`)
- `bool providesProcessList()` default false (`:132`)
- `void populatePluginMenu(QMenu*)` default no-op (`:139`)

`PluginProcessInfo { uint32_t pid; QString name, path; QIcon icon; bool is32Bit; }` (`:65-75`).

### Methods that are declared but the HOST never calls (important!)
- **`LoadType()` is never consulted** — `LoadPlugins()` loads *every* DLL in the
  folder unconditionally; `Auto`/`Manual` is dead (KernelMemory/RemoteProcessMemory
  return `Manual` and still auto-load). (`pluginmanager.cpp` has no `LoadType` ref.)
- **`getInitialBaseAddress()` is never called** — the host derives the base from
  `provider->base()` instead (`controller.cpp:5360-5364`).
- **`providesProcessList()` / `enumerateProcesses()` are not called by the host** —
  they are a *convention the plugin uses internally* (see §5).

## 2. PluginManager (`pluginmanager.{h,cpp}`)

Owned by `MainWindow` as a value member `m_pluginManager` (`mainwindow.h:173`) +
`bool m_pluginsLoaded` (`:174`). API: `LoadPlugins`, `plugins()` (non-owning
`QVector<IPlugin*>`), `providerPlugins()`, `FindPlugin(name)`, `LoadPluginFromPath`,
`UnloadPlugin(name)`, `UnloadPlugins`; private `LoadPlugin(path)` + `PluginEntry
{QLibrary* library; IPlugin* plugin;}` stored in `m_entries` parallel to `m_plugins`.

- **Deferred load**: not in the ctor. `main()` shows the window, then
  `QTimer::singleShot(0, …, loadPluginsDeferred)` (`main.cpp:9532-9534`); that runs
  one event-loop tick after first paint, **on the GUI thread**, calls
  `LoadPlugins()` then auto-starts MCP if the `autoStartMcp` setting is on
  (`main.cpp:4991-5001`). (~40 ms dlopen moved off the startup path.)
- **LoadPlugins** (`:13-66`): probe `Plugins/` (+ macOS `../PlugIns`); platform
  filter; **skip files whose basename starts with `rcx_payload`** (remote-inject
  payload, would spawn a rogue thread on Linux); `LoadPlugin` each.
- **LoadPlugin** (`:68-118`): `QLibrary(path).load()` → `resolve("CreatePlugin")` →
  factory; on any failure `qWarning` + cleanup. On success store entry; if
  `Type()==ProviderPlugin`, **auto-register**: `identifier = Name().toLower().
  replace(" ","")`, `dllFileName = QFileInfo(path).fileName()`,
  `ProviderRegistry::registerProvider(name, identifier, plugin, dllFileName)`.
- Runtime: `LoadPluginFromPath` (dedupe by filename), `UnloadPlugin(name)`
  (unregister by recomputed identifier, delete plugin, `unload()`+delete library —
  **dangling-provider risk** if a doc still holds that provider; the dialog only
  *warns* the user), `UnloadPlugins` (clear registry then delete all).
- **No QSettings persistence of plugin enable/disable or paths** — the loaded set is
  purely "DLLs in the folder" + runtime manual load/unload (not remembered across
  restarts).

## 3. ProviderRegistry (`providerregistry.{h,cpp}`) — global singleton

- `ProviderInfo { QString name; QString identifier; IProviderPlugin* plugin;
  BuiltinFactory factory; bool isBuiltin; QString dllFileName; }` (`providerregistry.h:29-44`).
  `BuiltinFactory = function<bool(QWidget*, QString*)>`. Storage `QList<ProviderInfo>`
  — **listing order = registration order**.
- `registerProvider` (plugin), `registerBuiltinProvider` (factory — **implemented but
  zero call sites**; the only built-in surfaced is "File", added directly by the
  menus, not via the registry), `unregisterProvider`, `findProvider(id)` (exact),
  `clear()`, `providers()`.
- **Identifier→icon table** `s_providerIcons` (`providerregistry.cpp:66-71`):
  `processmemory→server-process`, `remoteprocessmemory→remote`, `windbgmemory→debug`,
  `reclass.netcompatlayer→plug.svg`; fallback `extensions.svg`. (`kernelmemory` is
  **absent** here → generic icon in the *menu*, but present in the popup helper.)
- `static populateSourceMenu(QMenu*, savedSources)` (`:63-115`): adds **"File"**
  (`data="File"`); per provider an action (icon from table, label `name` or
  `"name  (dll)"`, `data = prov.name`); **for each plugin provider calls
  `prov.plugin->populatePluginMenu(menu)`** to inject plugin items (e.g.
  WinDbg "Unload Driver"), regenerated every time the menu shows; then saved sources
  as checkable `data="#saved:<i>"` + "Clear All" `data="#clear"`.

## 4. Two source-picker surfaces (both render the registry)

(a) **Menubar File ▸ Data Source** (`main.cpp:1170-1183`): rebuilt on `aboutToShow`
→ `populateSourceMenu` → `ProviderRegistry::populateSourceMenu`. `triggered` handler:
empty data = plugin's own item; `#clear`→`clearSources`; `#saved:N`→`switchSource`;
else `selectSource(name)`.

(b) **Inline editor `SourceChooserPopup`** (`sourcechooserpopup.{h,cpp}`,
`controller.cpp:4312-4455`): a fuzzy-filterable two-line-card list with "Saved
Sources" + "Providers" ("Open File" + one row per registry provider) + "Clear All";
shows PID/arch/base/stale badges; signals `sourceSelected(idx)`→`switchToSavedSource`,
`providerSelected(name)`→`selectSource`, `clearRequested`→`clearSources`; deferred
liveness probe greys dead sources. Helpers `iconForProvider`/`kindLabelFor`
(`sourcechooserpopup.h:19-42`) — include `kernelmemory→symbol-key/"Kernel"` and
`reclass.netcompatlayer→plug.svg`/(falls through to)`"Plugin"`.

## 5. The attach flow — `RcxController::selectSource` (`controller.cpp:5247-5401`)

Both surfaces pass the provider **name**; it's re-normalized to the identifier.
- `#clear`/`#saved:i`/`File` handled specially (File = QFileDialog → `loadData`).
- Else: `findProvider(name.toLower().replace(" ",""))`; built-in →
  `factory(parent,&target)`, plugin → **`plugin->selectTarget(parent,&target)`**; if a
  target came back → **`plugin->createProvider(target,&errorMsg)`**. On success: clear
  undo, swap `m_doc->provider`, adopt `provider->pointerSize()`; **base** = re-evaluate
  `baseAddressFormula` if set (wiring symbol/pointer + kernel-paging `vtop`/`cr3`/
  `physRead` callbacks when `provider->hasKernelPaging()`), else adopt
  `provider->base()` if non-zero and current base is the default `0x00400000`;
  `resetSnapshot`/`documentChanged`; dedupe/append a `SavedSourceEntry`, set active
  index. On failure → themed "Couldn't Attach" box.
- `attachViaPlugin(identifier,target)` (`:5139-5213`) is the programmatic path (no
  picker) used by saved-source restore, the kernel "open provider tab" (`phys:<cr3>`),
  self-test, and the MCP bridge.

### Process picker integration is PLUGIN-driven, not host-driven
The host `ProcessPicker` (`processpicker.{h,cpp}`, a `ThemedDialog`) has two ctors: a
default OS-enumerating one and **`ProcessPicker(const QList<ProcessInfo>&
customProcesses, …)`** (hides Refresh). Each plugin's own `selectTarget()` calls its
`enumerateProcesses()`, converts `PluginProcessInfo`→`ProcessInfo`, constructs the
custom-list `ProcessPicker`, and returns the chosen target string. The host never
calls `providesProcessList()`/`enumerateProcesses()` itself. So **plugins link
against the host's `ProcessPicker` class** — a host class dependency, not a pure
data interface. Target-string conventions (per plugin): `"pid:name"` (process/
kernel/remote), `"dllpath|pid:name"` (netcompat), `path` (File). Default sort PID
desc; pre-selects `QSettings("Reclass","Reclass")/lastAttachedProcess`; `is32Bit`
detection (WoW64 on Win, ELF class byte on Linux) → " (32-bit)" name suffix.

## 6. Manage Plugins dialog (`main.cpp:8821-8933`)

Plugins menu has exactly one item: "&Manage Plugins…" (no accelerator/icon,
`main.cpp:1574-1576`). The dialog is a `ThemedDialog` with a single `QListWidget`
showing per plugin `"<Name> v<Version>\n  <Description>\n  Type: <Provider>\n
Author: <Author>"` + `plugin->Icon()`; UserRole=name. Buttons: **"Load plugin…"**
(QFileDialog default `Plugins/`, filter `*.dll *.so *.dylib` → `LoadPluginFromPath`),
**"Unload selected"** (confirm dialog → `UnloadPlugin`), "Close". **It is
load/unload (dlopen/dlclose) only — no enable/disable, no persistence, no
auto/manual, no path column.** Errors → generic "check the console" warn box;
detail only to `qWarning`.

## 7. Saved sources persistence (`controller.cpp`)

- **Restore-only.** `RcxDocument::load` reads `"savedSources"` JSON into
  `pendingSavedSources` (relative `filePath` resolved against the .rcx dir);
  `ingestPendingSavedSources` lifts them into `m_savedSources` and **auto-attaches the
  first** via `switchToSavedSource(0)`.
- **`RcxDocument::save` does NOT write `savedSources`** — it writes only
  `tree.toJson()` + `typeAliases`. So a user who attaches a process and saves does
  *not* get the source persisted; `savedSources` is for shipped/hand-authored example
  `.rcx` (e.g. png.rcx auto-attaching its sibling .png). Active-source index is
  in-memory only. `copySavedSources()` clones the list when splitting/duplicating a tab.

## 8. ReClass.NET compat layer (`plugins/RcNetPluginCompatLayer/`)

Itself a provider plugin (Name "ReClass.NET Compat Layer" → identifier
`reclass.netcompatlayer`, LoadType Auto, trash placeholder icon). **Windows-only**
(PE parsing + mscoree). It bridges ReClass.NET's native **CoreFunctions** ABI into
`rcx::Provider`. Target string `"dllpath|pid:name"`. Discovery is **interactive**
(a `QFileDialog` for a `*.dll` inside `selectTarget`), NOT a folder scan.

### The 8 CoreFunctions (`ReClassNET_Plugin.hpp`)
`RC_CALLCONV = __stdcall` (Win) / empty. `RC_Pointer=void*`, `RC_Size=u64`,
`RC_UnicodeChar=char16_t`. `RcNetFunctions` table order = `EnumerateProcesses,
OpenRemoteProcess, IsProcessValid, CloseRemoteProcess, ReadRemoteMemory,
WriteRemoteMemory, EnumerateRemoteSectionsAndModules, ControlRemoteProcess`.
- `ReadRemoteMemory(handle, addr, buffer, int offset, int size) -> bool`
- `WriteRemoteMemory(...)-> bool`; `OpenRemoteProcess(RC_Size id, ProcessAccess)->handle`;
  `IsProcessValid(handle)->bool`; `CloseRemoteProcess(handle)`;
  `EnumerateProcesses(cb)`; `EnumerateRemoteSectionsAndModules(handle, secCb, modCb)`;
  `ControlRemoteProcess(handle, action)`.
- **Required**: ReadRemoteMemory, OpenRemoteProcess, CloseRemoteProcess,
  EnumerateProcesses. **Optional** (nullptr-tolerant): the other 4.
- Enumeration is **callback-based** with `#pragma pack(1)` UTF-16 structs:
  `EnumerateProcessData` (1048 B: Id u64 @0, Name[260]u16 @8, Path[260]u16 @528),
  `EnumerateRemoteSectionData` (580 B), `EnumerateRemoteModuleData` (536 B). No
  user-context ptr → the C++ smuggles context via `thread_local` collectors.
- Enums: `ProcessAccess{Read=0,Write=1,Full=2}`, `SectionProtection{No=0,R=1,W=2,X=4,
  Guard=8}`, `SectionType{Unknown,Private,Mapped,Image}`, `SectionCategory{Unknown,
  CODE,DATA,HEAP}`, `ControlRemoteProcessAction{Suspend,Resume,Terminate}`.
- **Debug/breakpoint functions are deliberately omitted** ("no debug types").

### Native path
`QLibrary` load + `resolve()` the 8 exports into `RcNetFunctions`; validate the 4
required; `m_isManaged=false`. Native/managed decided per-DLL by `isDotNetAssembly`
= PE parse of the CLR data directory (index 14) — managed iff its RVA+size non-zero
(loads with `DONT_RESOLVE_DLL_REFERENCES` so DllMain doesn't run).

### Managed (C#) path — `ClrHost` + `bridge/RcNetBridge.cs`
- Hosts **.NET Framework CLR v4.0.30319** in-process via dynamically-loaded
  `mscoree.dll` → `CLRCreateInstance(CLSID_CLRMetaHost) → ICLRMetaHost::GetRuntime
  (L"v4.0.30319") → ICLRRuntimeInfo::GetInterface(CLSID_CLRRuntimeHost) →
  ICLRRuntimeHost::Start()`. (Redeclares minimal COM vtables to avoid SDK headers.)
- `loadManagedPlugin` calls `ExecuteInDefaultAppDomain(bridgeDll, "RcNetBridge.Bridge",
  "Initialize", L"<hexptr-to-RcNetFunctions>|<pluginPath>")`. The bridge DLL is
  located at `applicationDirPath()/Plugins/RcNetBridge.dll`.
- **`RcNetBridge.cs`** (netstandard2.0, hosted under the FW4 CLR): redefines a subset
  of ReClass.NET's public types in their real namespaces (`ReClassNET.Core`,
  `.Memory`, `.Plugins`, `.Debugger`) incl. `interface ICoreProcessFunctions`; an
  `AssemblyResolve` handler returns the bridge itself for assembly `"ReClass.NET"`
  and probes for plugin deps; `Initialize` parses the ptr+path, `Assembly.LoadFrom`s
  the plugin, finds the first exported type implementing
  `ReClassNET.Core.ICoreProcessFunctions`, instantiates it (best-effort `Initialize`
  with a stub host), then writes 8 `[UnmanagedFunctionPointer(StdCall)]` delegate
  thunks (pinned in a `static` list to survive GC) into the native `RcNetFunctions`
  table via `Marshal.WriteIntPtr`. The Read/Write thunks `Marshal.Copy` between the
  native buffer and a managed `byte[]`; enumeration thunks hand-marshal to the exact
  pack(1) byte offsets above. Return codes: 0 ok, 1 bad arg, 2 no
  ICoreProcessFunctions, 3 load/deps fail, 4 other. **No AppDomain unload** → managed
  plugins leak for the process lifetime.
- Build: CMake detects `dotnet`; if an SDK exists, builds the bridge to
  `Plugins/RcNetBridge.dll` and defines `HAS_CLR_BRIDGE`; else managed support is
  disabled with a status message. Links `ole32`. The plugin DLL + bridge DLL land
  together in `Plugins/`.

### `RcNetCompatProvider` mapping (`Provider` ← CoreFunctions)
Holds the `RcNetFunctions` by value + an open handle (**always opened
`ProcessAccess::Full`**). `read`→ReadRemoteMemory (offset always 0, addr cast u64→ptr),
`write`→WriteRemoteMemory, `isWritable`→`WriteRemoteMemory!=null`, `name`→processName,
`kind`→`"RcNet"`, `isLive`→true, `base`→first enumerated module base,
`getSymbol`→linear module scan → `"mod+0xHEX"`, `symbolToAddress`→module-name match.
**`size()` is a hardcoded `0x10000` sentinel** (just to pass `isValid()`).
**Not overridden** (defaults): `pointerSize` → **8** (no bitness detection!),
`enumerateRegions`→empty (only modules cached internally for symbols; section
callback is empty), `peb`/kernel-paging → defaults. `ControlRemoteProcess` resolved
but never called. Module enumeration uses a `thread_local` collector.

## 9. Other host hooks a provider can rely on (`provider.h` defaults)

The `rcx::Provider` interface (which our `src/provider/mod.rs` mirrors) is richer
than the ReClass.NET CoreFunctions: beyond read/write/size, the host consults
`pointerSize`, `base`, `isLive`, `isWritable`, `kind`, `name`, `getSymbol`,
`symbolToAddress`, `enumerateRegions`, `peb`, and a **kernel-paging** group
(`hasKernelPaging`/`getCr3`/`translateAddress`/`readPageTable`) that the host wires
into base-formula evaluation and the "Browse Page Tables" provider-tab flow. A
native our-format provider plugin can implement all of these; the ReClass.NET compat
provider implements only the subset the CoreFunctions expose.

## 10. Porting gotchas (corrections to earlier design assumptions)

1. `LoadType` Auto/Manual is **dead** in C++ load logic — every DLL auto-loads. (Our
   port *may* honor it as an improvement, but don't claim it's C++ parity.)
2. `getInitialBaseAddress` is **never called** — host uses `provider->base()`.
3. Manage Plugins is **load/unload (dlopen/dlclose)**, not enable/disable, and there
   is **no persistence** of plugin state.
4. The process-picker integration is **plugin-driven** (plugin calls the host's
   `ProcessPicker` custom-list ctor), not a host-called `enumerateProcesses` hook.
5. There are **two** source-picker surfaces (menu + inline popup) — both must list
   plugin providers.
6. Provider **identifier** = `Name().toLower().replace(" ","")`, used everywhere as
   the routing key + icon-table key.
7. `populatePluginMenu` is called from `populateSourceMenu` **each time the source
   menu is shown** (inject conditional items like "Unload Driver").
8. `savedSources` is **restore-only** (load auto-attaches first; save never writes it).
9. ReClass.NET managed compat is **.NET Framework v4.0.30319 via mscoree COM
   (Windows-only) + a C# bridge** marshalling `ICoreProcessFunctions` → 8 StdCall
   thunks; memory backends only; managed plugins leak (no AppDomain unload).
10. `IPLUGIN_IID` is vestigial.
