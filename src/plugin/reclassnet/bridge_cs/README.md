# `RcNetBridge` — the managed (C#) ReClass.NET compat bridge

This directory vendors the **C# bridge assembly** that the Windows-only managed
ReClass.NET compat path uses (design §4 managed path, §6 Phase 5; reference §8
"Managed (C#) path"). It is **build input**, not a cargo crate — Cargo never touches
it. It is compiled separately (by `csc`/`dotnet`) to **`RcNetBridge.dll`** and
shipped beside the managed plugin in the `plugins/` folder.

## What it does

The Rust CLR host (`../clr_host.rs`) starts the .NET Framework v4.0.30319 runtime and
calls this bridge's single entry point:

```text
ICLRRuntimeHost::ExecuteInDefaultAppDomain(
    "…/RcNetBridge.dll", "RcNetBridge.Bridge", "Initialize",
    "<hexptr-to-RcNetFunctions>|<pluginpath>")  ->  int  (the load return code)
```

`Bridge.Initialize`:

1. Registers an `AssemblyResolve` handler that hands the plugin **this** assembly
   when it asks for `ReClass.NET` (the stub types in this file satisfy a memory
   plugin's type references), and probes the plugin's dir for other deps.
2. `Assembly.LoadFrom`s the plugin and finds the first exported type implementing
   `ReClassNET.Core.ICoreProcessFunctions`.
3. Wraps each of the 8 memory `CoreFunctions` in an
   `[UnmanagedFunctionPointer(StdCall)]` delegate (pinned in a `static` list so the
   GC can't collect them) and `Marshal.WriteIntPtr`s them into the **8 consecutive
   pointer slots** of the native `RcNetFunctions` table at the address from the arg.

It returns: **0** ok · **1** bad arg · **2** no `ICoreProcessFunctions` · **3**
load/deps fail · **4** other. The Rust side (`../managed.rs`,
`ManagedSkip::from_return_code`) maps **2** to the logged *"ReClass.NET node-type
plugin unsupported"* skip — the decided memory-backends-only scope (design §8).

## ABI contract with the Rust side (load-bearing)

The hex pointer addresses a Rust `#[repr(C)] RcNetFunctions` (`../bridge.rs`) — 8
consecutive pointer-sized slots. `WriteFunctionPointers` writes slot `i` for `i` in
`0..8` in the **same order** as the Rust `CORE_FUNCTION_NAMES` (`../ffi.rs`):

```
0 EnumerateProcesses  1 OpenRemoteProcess  2 IsProcessValid  3 CloseRemoteProcess
4 ReadRemoteMemory    5 WriteRemoteMemory  6 EnumerateRemoteSectionsAndModules
7 ControlRemoteProcess
```

The hand-marshalled packed structs match the Rust `#[repr(C, packed)]` layouts
(`EnumerateProcessData` 1048 B, `EnumerateRemoteSectionData` 580 B,
`EnumerateRemoteModuleData` 536 B — pinned by the `../ffi.rs` layout tests). Delegates
are `StdCall`, matching the Rust `rc_extern!` selector (`extern "stdcall"` only on
32-bit Windows, `extern "C"` — i.e. the single x64 ABI — elsewhere).

## Building (out of cargo's scope; Windows packaging step)

Requires the .NET SDK (`dotnet`) **or** the legacy `csc`. The included
`RcNetBridge.csproj` targets `netstandard2.0` so it loads under the hosted .NET
Framework 4.x CLR:

```sh
# With the .NET SDK:
dotnet build RcNetBridge.csproj -c Release      # -> bin/Release/RcNetBridge.dll

# Or directly with the framework compiler (no SDK):
csc /target:library /out:RcNetBridge.dll RcNetBridge.cs
```

Then **ship `RcNetBridge.dll` into the same `plugins/` directory** as the managed
ReClass.NET plugin. If it is missing, `load_reclassnet_managed` (`../managed.rs`)
fails with a clear *"RcNetBridge.dll not found beside the plugin"* error (not a
crash). Wiring this build into the Windows packaging pipeline is left to the
integrate / packaging step; it does not affect the Linux default build (which never
compiles or ships the managed path).
