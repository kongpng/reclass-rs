# Subsystem: RTTI + Symbol Store + Name Providers

**Key:** `rtti-symbols`  **Expected portability:** mostly-portable

This subsystem provides three intertwined capabilities for the Reclass struct-layout
inspector:

1. **RTTI walking** — given a candidate vtable address, byte-level parse the C++
   runtime type information that MSVC (RTTICompleteObjectLocator chain) or the
   Itanium ABI (`type_info` chain) emits, to recover the class name + base-class
   hierarchy + the vtable's virtual-method addresses. Pure parser over the
   abstract `Provider` read interface; works on any platform.
2. **Symbol store** — a process-global registry of PDB-extracted symbols
   (name↔RVA), keyed by canonical module name with aliasing, supporting forward
   resolution (`module!symbol` → absolute address) and nearest-symbol reverse
   lookup (address → `module!symbol+0xN`) via binary search.
3. **Name providers** — a pluggable `NameProvider` abstraction + `NameRegistry`
   aggregator that unifies PDB symbols, PDB types, RTTI discoveries, and
   user bookmarks behind one `nameFor()` / `addressFor()` / `entries()` interface
   for the "Symbols" panel, the editor's address→name comment column, and the
   expression parser.

Plus the **symbol downloader** (Microsoft public symbol-server PDB fetch) and the
**demangler façade** (`humanizeSymbolName`).

The data-source plugins are OUT OF SCOPE; everything here treats the data source
purely through `rcx::Provider` (`src/providers/provider.h`). For the port, the
relevant built-in providers are `BufferProvider` (in-memory/file) and
`SnapshotProvider`; the synthetic-RTTI tests use `BufferProvider`.

---

## File inventory

| File | Role |
|------|------|
| `src/rtti.h` / `src/rtti.cpp` | RTTI walkers (MSVC + Itanium), both demanglers, module lookup |
| `src/symbolstore.h` / `src/symbolstore.cpp` | `SymbolStore` singleton: PDB symbol/type storage + resolution |
| `src/symbol_downloader.h` / `src/symbol_downloader.cpp` | `SymbolDownloader` QObject: MS symbol-server PDB download + cache |
| `src/names/name_provider.h` / `.cpp` | `NamedAddress` struct + abstract `NameProvider` base class |
| `src/names/name_registry.h` / `.cpp` | `NameRegistry` singleton aggregator (QObject, signals) |
| `src/names/pdb_name_provider.h` / `.cpp` | `PdbNameProvider`: PDB symbols as named addresses |
| `src/names/pdb_type_provider.h` / `.cpp` | `PdbTypeProvider`: PDB TPI types (address-less) |
| `src/names/rtti_name_provider.h` / `.cpp` | `RttiNameProvider` singleton: accumulated RTTI hits |
| `src/names/bookmark_name_provider.h` / `.cpp` | `BookmarkNameProvider`: per-document user bookmarks |
| `src/names/symbol_demangle.h` / `.cpp` | `humanizeSymbolName()` façade (routes MSVC RTTI / Itanium / MSVC dbghelp) |

**Tests:** `tests/test_rtti.cpp`, `tests/test_rtti_hint.cpp`.

---

## Dependencies on other subsystems (for the port)

These types/functions are referenced but defined elsewhere; the port must already
have (or stub) them:

- `rcx::Provider` (`src/providers/provider.h`) — abstract data source. RTTI/symbol
  code uses: `read(addr, buf, len) -> bool`, `enumerateModules() -> QVector<ModuleEntry>`,
  `symbolToAddress(name) -> uint64_t`, `readAs<T>(addr) -> T`. `ModuleEntry{ QString name; QString fullPath; uint64_t base; uint64_t size; }`.
- `PdbTypeInfo`, `PdbSymbol`, `PdbSymbolResult`, `extractPdbSymbols`, `enumeratePdbTypes`,
  `importTypeForSymbol` (`src/imports/import_pdb.h`) — the PDB-parsing subsystem (separate port unit). `SymbolStore` only *stores* `PdbTypeInfo`; it does not parse PDBs.
- `RcxController`, `RcxDocument`, `NodeTree`, bookmarks (`src/controller.h`, `src/core.h`) — used only by `BookmarkNameProvider`.
- `AddressParser` (`src/addressparser.h`) — bookmark formula evaluation.
- `ThemeManager` (`src/themes/thememanager.h`) — `accent()` colors. **GUI-only**; in a QtCore-only / headless build these are not exercised. The accent values are packed `0xAARRGGBB` (`QColor::rgba()`).

`PdbTypeInfo` fields (relevant subset, from `import_pdb.h`):
```
uint32_t typeIndex; QString name; uint64_t size;
int childCount; bool isUnion; bool isEnum = false;
```

---

# Part 1 — RTTI (`src/rtti.h`, `src/rtti.cpp`)

## 1.1 Data types

### `struct RttiBaseClass` (`rtti.h:30`)
| Field | Type | Meaning |
|-------|------|---------|
| `rawName` | `QString` | Raw mangled type-descriptor name, e.g. `".?AVFoo@@"` |
| `demangledName` | `QString` | Demangled, e.g. `"Foo"` |
| `depth` | `int` (=0) | 0 = self, 1 = direct base, 2+ = grandparent. **NOTE:** in `walkRtti` this is actually set to the *loop index `i`* into the base-class array (`rtti.cpp:239`), not true hierarchy depth — see edge cases. |

### `struct RttiVirtualMethod` (`rtti.h:36`)
| Field | Type | Meaning |
|-------|------|---------|
| `slot` | `int` (=0) | Index in vtable |
| `address` | `uint64_t` (=0) | Absolute address of the virtual method |
| `symbol` | `QString` | Resolved via `SymbolStore::instance().getSymbolForAddress()` — empty when no PDB loaded |

### `struct RttiInfo` (`rtti.h:42`) — main result
| Field | Type | Meaning |
|-------|------|---------|
| `ok` | `bool` (=false) | Whether the walk succeeded |
| `error` | `QString` | Human-readable error when `!ok` |
| `abi` | `QString` | `"MSVC"` / `"Itanium"` — empty when `!ok` |
| `vtableAddress` | `uint64_t` (=0) | The input vtable address (always echoed back, even on failure) |
| `imageBase` | `uint64_t` (=0) | Image base of the module owning the COL (MSVC) / type_info (Itanium) |
| `moduleName` | `QString` | Name of that module (empty if not found) |
| `completeLocator` | `uint64_t` (=0) | MSVC: VA of COL; Itanium: VA of `type_info` |
| `offset` | `int` (=0) | MSVC: `COL.offset`; Itanium: `offset_to_top` |
| `rawName` | `QString` | Top class raw name (MSVC `.?AVFoo@@`, Itanium `3Foo`) |
| `demangledName` | `QString` | Top class demangled name (`Foo`) |
| `bases` | `QVector<RttiBaseClass>` | Class hierarchy |
| `vtable` | `QVector<RttiVirtualMethod>` | Enumerated vtable entries |

### `struct OwningModule` (`rtti.h:98`)
| Field | Type | Meaning |
|-------|------|---------|
| `name` | `QString` | Module short name |
| `fullPath` | `QString` | Full path |
| `base` | `uint64_t` (=0) | Module image base |
| `size` | `uint64_t` (=0) | Module image size |
| `valid` | `bool` (=false) | Whether a module was found |

## 1.2 Public functions

### `OwningModule findOwningModule(const Provider& prov, uint64_t addr)` (`rtti.cpp:20`)
- Iterates `prov.enumerateModules()`; returns the **first** module `m` where
  `addr >= m.base && addr < m.base + m.size`. Copies name/fullPath/base/size,
  sets `valid=true`, returns immediately.
- If no module matches, returns a default `OwningModule` (`valid=false`).
- **Edge:** with an empty module list (e.g. plain `BufferProvider`) always returns invalid.
- Rust: linear scan over `Vec<ModuleEntry>`; trivial.

### `QString demangleRttiName(const QString& mangled)` (`rtti.cpp:38`) — MSVC RTTI type-descriptor demangler
In-house parser (intentionally NOT `UnDecorateSymbolName`, which emits garbage on bare `.?AV` strings — `rtti.cpp:42-45`). Algorithm:
1. Empty input → return empty.
2. If `mangled` does NOT start with `".?A"` AND does NOT start with `"?A"` → return verbatim (`rtti.cpp:55`).
3. `s = mangled`; if it starts with `'.'` drop it; if it then starts with `'?'` drop it (`rtti.cpp:60-61`).
4. If `s.size() < 3` → return original `mangled` verbatim.
5. `body = s.mid(2)` — skips the 2-char prefix (`'A'` + class-kind char `V`/`U`/`W`/`X`).
6. Find `"@@"` terminator (`indexOf`). If absent (`< 0`) → return `mangled` verbatim.
7. `segments = body.left(term)`; split on `'@'` with `Qt::SkipEmptyParts`.
8. If parts empty → return `mangled`.
9. **Reverse** parts (segments are inner-most first) → join with `"::"`.

Examples (asserted by tests):
- `.?AVFoo@@` → `Foo`
- `.?AUStruct@@` → `Struct`
- `.?AVBar@Foo@@` → `Foo::Bar`
- `.?AVZ@Y@X@@` → `X::Y::Z`
- `plain_name` → `plain_name` (passthrough)
- `""` → `""`

Edge cases: any input not matching the form is returned **verbatim** (callers and tests rely on this). The class-kind char after `A` (`V`=class/`U`=struct/`W`=enum/`X`=void) is *not* validated or used — it is simply skipped.

Rust equivalent: hand-written string parser (matches better than off-the-shelf demanglers — `msvc-demangler` does full symbol demangling and chokes on bare type descriptors, exactly as the C++ comment warns about `UnDecorateSymbolName`). **Implement this by hand** for parity.

### `RttiInfo walkRtti(const Provider& prov, uint64_t vtableAddr, int pointerSize=8, int maxVtableSlots=64)` (`rtti.cpp:108`)
MSVC RTTI walker. Step-by-step:

1. `info.vtableAddress = vtableAddr`.
2. If `pointerSize` is neither 4 nor 8 → `error = "invalid pointer size"`, return.
3. **Read COL pointer at `vtable[-pointerSize]`** (`metaPtrAddr = vtableAddr - pointerSize`):
   - 8: read 8 bytes into `colAddr`. (Note `rtti.cpp:124` has a redundant `prov.readAs<uint64_t>` call whose result is overwritten by the `prov.read` on line 125 — harmless; the `read` return value `ok` is what matters.)
   - 4: read 4 bytes into a `uint32_t`, widen to `colAddr`.
   - On these platforms the COL pointer is an **absolute VA** (not an RVA) on both x86 and x64.
   - If `!ok || colAddr == 0` → `error = "could not read meta pointer at vtable[-1]"`, return.
4. `info.completeLocator = colAddr`.
5. **Determine image base** via `findOwningModule(prov, colAddr)`:
   - If found: set `moduleName`, `imageBase = owner.base`.
   - Else if `pointerSize==8`: **fallback** — read a `uint32_t` from `colAddr + 0x14` (the COL's `pSelf` self-image-base field) and use it as `imageBase` if nonzero. This is what makes synthetic-bytes tests work without an enumerable module. (`rtti.cpp:143-149`)
   - `info.imageBase = imageBase` (may be 0 on x86 with no module).
6. **Read COL signature** at `colAddr + 0x00` (`uint32_t`):
   - On read fail → `error = "could not read COL signature"`, return.
   - If `sig != 0 && sig != 1` → `error = "COL signature 0x<hex> not 0/1 — not MSVC RTTI"`, return. (sig 1 = relative/x64.)
7. `info.offset = (int) readU32At(colAddr + 0x04)` (COL.offset).
8. Read `pTypeDescriptor` at `colAddr + 0x0C` and `pClassHierarchy` at `colAddr + 0x10` (both `uint32_t` fields). On either read fail → `error = "could not read COL TypeDescriptor / CHD fields"`, return.
9. Resolve to addresses via `rttiResolve`: on `ptrSize==8`, `addr = imageBase + field` (RVA); on `ptrSize==4`, `addr = field` (absolute). (`rttiResolve` helper, `rtti.cpp:81`.)
10. **TypeDescriptor name**: name field is at `tdAddr + 2*pointerSize` (skips the type_info-vtable ptr at +0 and the spare ptr at +ptrSize). Read NUL-terminated C-string via `readCString` (max 512 bytes, UTF-8 decode). Set `info.rawName`, `info.demangledName = demangleRttiName(rawName)`.
    - If `rawName` empty → `error = "type descriptor name empty"`, return. (Note: `demangledName` is computed *before* the empty check.)
11. **CHD**: read `numBaseClasses` at `chdAddr + 0x08` and `pBaseClassArray` at `chdAddr + 0x0C`. On either read fail → `error = "could not read CHD"`, return.
    - If `numBases > 256` → `error = "CHD.numBaseClasses unreasonably large (<n>) — not RTTI"`, return. (Sanity guard; test feeds 9999 → error contains `"unreasonably"`.)
12. **Base-class array** at `bcaAddr` (resolved RVA/abs). `entrySize = (pointerSize==8) ? 4 : pointerSize` — each entry is a 32-bit RVA on x64, a pointer on x86. For `i` in `0..numBases`:
    - Read BCD pointer at `bcaAddr + i*entrySize`. On read fail → `break` (stop, keep what we have).
    - Resolve `bcdAddr`. Read `pTypeDescriptor` (BCD+0x00, `uint32_t`). On fail → `continue` (skip this base).
    - Resolve `bcdTdAddr`, read its name at `+2*ptrSize`. Append `RttiBaseClass{ rawName, demangleRttiName(rawName), depth=i }`.
    - **NOTE the discrepancy:** `depth` is set to the array index `i`, *not* the true `numContainedBases`-derived depth the header comment describes. Synthetic test has 3 flat bases (Foo/Bar/Baz) → depths 0/1/2; the BCD's `numContainedBases` at +0x04 is read but ignored.
    - The first base entry (`i==0`) is the class *itself* in real MSVC layout — test asserts `bases[0].demangledName == "Foo"` (same as top class).
13. **Vtable enumeration** (`slot` in `0..maxVtableSlots`):
    - `entryAddr = vtableAddr + slot*pointerSize`. Read pointer (8 or 4 bytes).
    - On read fail → `break`. If `target == 0` → `break` (null terminator stops enumeration).
    - **Module heuristic:** if `!owner.valid` (no enumerable modules, synthetic provider) treat as in-module (`inSomeModule = true` — trust the input). Else require `findOwningModule(prov, target).valid`. If not in any module → `break`.
    - Append `RttiVirtualMethod{ slot, address=target, symbol=SymbolStore::instance().getSymbolForAddress(target, &prov) }`.
    - With `maxVtableSlots==0` (compose hint path), the loop body never runs → empty vtable, fast.
14. `info.ok = true; info.abi = "MSVC"`. Return.

Tested invariants (synthetic, `test_rtti.cpp:73`): `ok`, `vtableAddress`, `completeLocator==imageBase+0x1900`, `imageBase==0x10000`, `offset==0`, top class `Foo`, 3 bases `Foo/Bar/Baz`, vtable size 5 (slot 5 is null), each slot address `imageBase + 0x100 + i*0x10`.

### `QString demangleItaniumName(const QString& mangled)` (`rtti.cpp:284`) — Itanium ABI type-name demangler
1. Empty → empty.
2. **Preferred path** (when `RCX_HAVE_CXA_DEMANGLE`, i.e. `__GNUG__` is defined — GCC/Clang/MinGW): call `abi::__cxa_demangle(utf8, nullptr, nullptr, &status)`. On `status==0` and non-null result → return `QString::fromUtf8(result).trimmed()`. The result must be `free()`d (uses `unique_ptr` with `std::free`). If `__cxa_demangle` rejects → **fall through** to the in-house parser.
3. **In-house fallback** (also the only path on MSVC builds). Handles:
   - `3Foo` → `Foo` (length-prefixed)
   - `N3Bar3FooE` → `Bar::Foo` (nested)
   - `St9type_info` → `std::type_info` (`St` shorthand → `std`)
   - Anything else → verbatim.
   Parser detail:
   - `parseLength`: reads consecutive ASCII digits into `n`; returns `-1` if no digit consumed.
   - `consumeSegment`: if current+next chars are `S` then `t` → push `"std"`, advance 2. Else parse a length `len`; if `len <= 0` or `pos+len > size` → fail (`false`). Else push `s.mid(pos, len)`, advance.
   - If `s[0]=='N'`: advance past `N`; loop consuming segments until `'E'`; if any `consumeSegment` fails OR no `'E'` found → return `mangled` verbatim.
   - Else: consume one segment; on fail → verbatim.
   - If `parts` empty → verbatim. Else join with `"::"`.

Tests assert: `3Foo`→`Foo`, `N3Bar3FooE`→`Bar::Foo`, `St9type_info` ends with `type_info`, `plain_text`→`plain_text`.

Rust equivalent: try `cpp_demangle` (or `symbolic-demangle`) first for full support, and **keep an equivalent hand-rolled fallback** for bare type-name forms that crate demanglers reject (they expect a `_Z`-prefixed *symbol*, while RTTI `__name` stores the bare mangle without the `_Z` prefix). The synthetic tests feed bare forms like `3Foo`, `N3Bar3FooE`, `St9type_info` — these will NOT be accepted by `cpp_demangle` directly, so the fallback parser is load-bearing for test parity.

### `RttiInfo walkRttiItanium(const Provider& prov, uint64_t vtableAddr, int pointerSize=8, int maxVtableSlots=64)` (`rtti.cpp:354`)
Itanium ABI walker. Unlike `walkRtti`, this **requires** real modules (no synthetic image-base fallback) — `findOwningModule` must succeed for every pointer it dereferences. Steps:

1. `info.vtableAddress = vtableAddr`. Validate `pointerSize` (4/8) else `"invalid pointer size"`.
2. **type_info pointer** at `vtable[-pointerSize]` → `tiAddr`. On read fail or 0 → `"could not read type_info pointer at vtable[-1]"`.
3. `findOwningModule(prov, tiAddr)` must be valid, else `"type_info pointer outside any module"`. Set `imageBase`, `moduleName`, `completeLocator = tiAddr`.
4. **offset_to_top** at `vtable[-2*pointerSize]` (signed; 8 bytes `int64_t` on x64, 4 bytes `int32_t` on x86; default 0 if read fails). If `|offsetToTop| > 0x1000000` (16 MB) → `"offset_to_top implausible — not Itanium RTTI"`. Set `info.offset`. (Test plants `0x7FFFFFFFFFFFFFFF` → rejected; error contains `"offset_to_top"`.)
5. **type_info[0]** = its own vtable ptr; read (8/4). On read fail → `"could not read type_info vtable ptr"`. If `0` or not in any module → `"type_info vtable not in any module"`. (Doesn't discriminate `__class_type_info`/`__si_class_type_info`/`__vmi_class_type_info` — v1 limitation.)
6. **type_info[pointerSize]** = `__name` char ptr → `namePtr`. On read fail → `"could not read __name pointer"`. If `0` or not in any module → `"__name pointer not in any module"`.
7. **Read mangled name** (up to 256 bytes): for each byte, on read fail → `break`; on NUL → `break`; if byte `< 0x20 || > 0x7E` (non-printable ASCII) → **clear collected bytes and break**. If `nameBytes.size() < 2` → `"__name string empty or non-printable"`.
8. **Vague-linkage `*` prefix:** GCC on MinGW / historical macOS prepends `'*'` to vague-linkage type names (so `type_info::operator==` falls back to pointer identity). `validateOff = (nameBytes[0]=='*') ? 1 : 0`. If `validateOff >= size` → `"__name is just a vague-linkage marker"`.
9. **Validate first real char**: `c0 = nameBytes[validateOff]`. Must be a digit OR one of `N S P K R`. Else → `"__name doesn't start with Itanium mangle marker"`. (Test `"not_a_mangle"` starts with `n` → rejected; error contains `"mangle"`.)
10. `info.rawName = QString::fromLatin1(nameBytes)` (**includes** the `*` prefix if present — so the UI shows the literal memory contents). `info.demangledName = demangleItaniumName(nameBytes.mid(validateOff))` (prefix stripped for the demangler).
11. **Vtable enumeration** (mirrors MSVC, but always uses real-module check, no synthetic trust): for `slot` in `0..maxVtableSlots`, read pointer; `break` on read fail / 0 / not-in-module. Append `RttiVirtualMethod{ slot, address, symbol=SymbolStore::getSymbolForAddress(...) }`.
12. `info.ok = true; info.abi = "Itanium"`. Return.

Tests (`test_rtti.cpp:178+`): `3Foo`→ok, abi `"Itanium"`, rawName `"3Foo"`, demangled `"Foo"`, vtable size 5; `N3Bar3FooE`→`Bar::Foo`.

## 1.3 Internal helpers (anon namespace, `rtti.cpp:78`)
- `uint64_t rttiResolve(uint32_t field, uint64_t imageBase, int ptrSize)` — `ptrSize==8`: `imageBase+field`; else `field`.
- `uint32_t readU32At(const Provider& p, uint64_t addr, bool* ok)` — `*ok = p.read(addr,&v,4)`, returns v (0 if read failed).
- `QString readCString(const Provider& p, uint64_t addr, int maxLen=512)` — reads byte-at-a-time, stops at NUL / read-fail / maxLen; UTF-8 decode (MSVC walker uses UTF-8, vs. the Itanium walker which inlines its own loop with Latin-1 decode + printable-ASCII filter — keep this difference).

## 1.4 RTTI layout reference (for the port)

**MSVC RTTICompleteObjectLocator (COL):**
```
+0x00 DWORD signature        (must be 0 or 1; 1 = relative/x64)
+0x04 DWORD offset           (subobject offset → RttiInfo.offset)
+0x08 DWORD cdOffset         (constructor displacement, ignored)
+0x0C DWORD pTypeDescriptor  (RVA x64 / abs x86)
+0x10 DWORD pClassHierarchy  (RVA x64 / abs x86)
+0x14 DWORD pSelf            (RVA to COL — x64 only; used as image-base fallback)
```
**RTTITypeDescriptor:** `+0`: type_info vtable ptr (ignored); `+ptrSize`: spare (0);
`+2*ptrSize`: NUL-terminated `.?AV...` name string.
**RTTIClassHierarchyDescriptor (CHD):** `+0x00` sig; `+0x04` attributes; `+0x08` numBaseClasses; `+0x0C` pBaseClassArray (→ array of BCD RVAs/ptrs).
**RTTIBaseClassDescriptor (BCD):** `+0x00` pTypeDescriptor; `+0x04` numContainedBases (read but ignored); `+0x08` PMD{mdisp,pdisp,vdisp}; `+0x14` attributes.

**Itanium vtable layout:** `vtable[-2*ptr]` = offset_to_top (ptrdiff_t); `vtable[-1*ptr]` = `type_info*`; `type_info[0]` = abi type_info vtable ptr; `type_info[ptr]` = `char* __name` (NUL-terminated mangled name, possibly `*`-prefixed).

## 1.5 Platform-specific code in RTTI
- `#if defined(__GNUG__)` → `#include <cxxabi.h>` + `RCX_HAVE_CXA_DEMANGLE` (`rtti.cpp:11`). The Itanium demangler uses `abi::__cxa_demangle` when available, else the fallback parser. **For the port:** use `cpp_demangle`/`symbolic` unconditionally (cross-platform), with the hand-rolled fallback for bare type names.
- Both walkers are byte-level over `Provider` and compile on every platform. The parsers themselves are fully portable.

---

# Part 2 — Symbol Store (`src/symbolstore.h`, `src/symbolstore.cpp`)

## 2.1 `struct PdbSymbolSet` (`symbolstore.h:14`)
Per-module symbol container.
| Field | Type | Meaning |
|-------|------|---------|
| `pdbPath` | `QString` | Source PDB path (empty marks an RTTI-only set, see `addRttiHits`) |
| `moduleName` | `QString` | Canonical lowercase name (e.g. `"ntoskrnl"`) |
| `nameToRva` | `QHash<QString, uint32_t>` | Symbol name → RVA. Forward lookup. |
| `nameToTypeIndex` | `QHash<QString, uint32_t>` | Symbol name → TPI typeIndex (0 = no type info) |
| `rvaToName` | `QVector<QPair<uint32_t, QString>>` | **Sorted by RVA** for binary-search reverse lookup |
| `types` | `QVector<PdbTypeInfo>` | Standalone TPI type definitions (address-less) |

`void sortRvaIndex()` (`symbolstore.h:26`): `std::sort` of `rvaToName` ascending by `.first` (RVA).
Rust: `HashMap<String,u32>` for `nameToRva`/`nameToTypeIndex`; `Vec<(u32,String)>` kept sorted for `rvaToName`; `Vec<PdbTypeInfo>` for `types`.

## 2.2 `class SymbolStore` (`symbolstore.h:32`) — process-global singleton

`static SymbolStore& instance()` — Meyers singleton (`symbolstore.h:34`). In the port this is a global (`OnceLock<Mutex<SymbolStore>>` or similar). **Note: the C++ class is NOT internally synchronized** — there is no mutex; access is assumed single-threaded (the GUI thread does PDB loads + compose runs). RTTI vtable resolution calls `getSymbolForAddress` from compose (UI thread). The only mutexed name store is `RttiNameProvider`. For the Rust port, wrap in a `Mutex`/`RwLock` to be safe, but the original has no concurrency here.

**Constructor** (`symbolstore.h:110`) seeds kernel aliases:
`nt→ntoskrnl`, `ntkrnlmp→ntoskrnl`, `ntkrnlpa→ntoskrnl`, `ntkrpamp→ntoskrnl`.

**Private members:** `QHash<QString,PdbSymbolSet> m_modules` (canonical name → set);
`QHash<QString,QString> m_aliases` (alias → canonical).

### Methods

**`QString resolveAlias(const QString& name) const`** (`symbolstore.h:100`, public inline)
1. `lower = name.toLower()`.
2. If `lower` ends with `.exe`/`.dll`/`.sys` → strip extension (`left(lastIndexOf('.'))`).
3. Look up in `m_aliases`; return the mapped canonical, else `lower` itself.
Pure string logic; central to all module lookups.

**`int addModule(moduleName, pdbPath, QVector<QPair<QString,uint32_t>> symbols)`** (`symbolstore.cpp:20`)
- `canonical = resolveAlias(moduleName)`.
- Build a fresh `PdbSymbolSet`; set `pdbPath`, `moduleName=canonical`, reserve.
- For each `(name, rva)`: **skip if `nameToRva` already contains the name** (first-wins dedupe); else insert into `nameToRva` and emplace `(rva,name)` into `rvaToName`.
- `sortRvaIndex()`. `count = nameToRva.size()`.
- Register the raw module name as an alias if it differs from canonical: lowercase the raw name, strip `.exe/.dll/.sys`, and if `!= canonical` set `m_aliases[rawLower] = canonical`.
- `m_modules[canonical] = std::move(set)` (**replaces** any existing set for that module).
- `qDebug()` log. Returns unique symbol count.

**`void addModuleTypeIndices(moduleName, QHash<QString,uint32_t> nameToTypeIndex)`** (`symbolstore.cpp:55`)
- Resolve alias; if module not present → **no-op return**. Else overwrite `it->nameToTypeIndex`.

**`void addModuleTypes(moduleName, QVector<PdbTypeInfo> types)`** (`symbolstore.cpp:63`)
- Resolve alias; if module not present → no-op. Else overwrite `it->types`.

**`void addRttiHits(moduleName, QVector<QPair<QString,uint32_t>> hits)`** (`symbolstore.cpp:71`)
- Resolve alias. If module not present, **create** an empty `PdbSymbolSet` (pdbPath empty = "RTTI-only") and insert it.
- For each `(name,rva)`: skip if name already in `nameToRva`; else insert + emplace into `rvaToName`.
- `sortRvaIndex()`.

**`uint32_t typeIndexForSymbol(const QString& qualifiedSymbol) const`** (`symbolstore.cpp:92`)
- Requires `module!symbol` form. `bangIdx = indexOf('!')`; if `bangIdx <= 0` or `bangIdx >= size-1` → return 0.
- `modPart = left(bangIdx)`, `symPart = mid(bangIdx+1)`. `canonical = resolveAlias(modPart)`.
- If module not found → 0. Else `nameToTypeIndex.value(symPart, 0)`.

**`void unloadModule(moduleName)`** (`symbolstore.cpp:104`) — `m_modules.remove(resolveAlias(moduleName))`.

**`uint64_t resolve(const QString& token, const Provider* provider, bool* ok) const`** (`symbolstore.cpp:109`) — expression-parser entry point
- `*ok = false` initially.
- **Qualified `module!symbol`** (when `bangIdx>0 && bangIdx<size-1`):
  - Resolve alias; if module missing → return 0 (`*ok` stays false).
  - If symbol not in `nameToRva` → return 0.
  - `rva = *symIt`. `moduleBase = getModuleBase(provider, canonical)`; if 0, retry with the *user-supplied* `modPart`. `*ok = true`; return `moduleBase + rva`. (Note: if base is 0, returns the bare RVA but still `ok`.)
- **Bare symbol** — scan all modules; count matches:
  - First match records `foundRva`/`foundModule`; if `matches > 1` → **return 0 immediately** (ambiguous, `*ok` stays false).
  - If exactly 1 match: `moduleBase = getModuleBase(provider, foundModule)`; `*ok=true`; return `base+rva`.
  - If 0 matches: **fallback** — treat the bare token as a module name. `canonical = resolveAlias(token)`; `base = getModuleBase(...)`; if `base != 0` → `*ok=true`, return base. Else return 0.

**`QString getSymbolForAddress(uint64_t addr, const Provider* provider) const`** (`symbolstore.cpp:172`) — reverse lookup (nearest symbol)
- If no modules or `!provider` → empty.
- For each module set:
  - `moduleBase = getModuleBase(provider, set.moduleName)`; if 0 → skip (module not attached to the live target — **prevents false positives** against unrelated user base addresses).
  - If `addr < moduleBase` → skip. `rva = (uint32_t)(addr - moduleBase)`.
  - If `rvaToName` empty → skip.
  - **Binary search** (`std::upper_bound` for first entry with RVA `> rva`); if at `begin()` → skip (target before all symbols). Decrement → last entry with RVA `<= rva`.
  - `displacement = rva - upper->first`. If `displacement > 0x1000` (`kMaxDisplacement`) → skip (too far past the symbol).
  - `displacement == 0` → return `"<module>!<name>"`; else `"<module>!<name>+0x<hex>"`.
- If no module produces a hit → empty.
- **Returns the first module's hit** (modules iterated in `QHash` order — non-deterministic, but in practice modules don't overlap).

**`uint64_t getModuleBase(const Provider* provider, const QString& canonical) const`** (`symbolstore.cpp:7`, private)
- `!provider` → 0. Try `provider->symbolToAddress(canonical)`, then with `.exe`, `.dll`, `.sys` suffixes appended, in that order. Returns the first nonzero or 0.

**`void addAlias(alias, canonicalModule)`** (`symbolstore.cpp:216`) — `m_aliases[alias.toLower()] = canonicalModule.toLower()`.

**Inline accessors:** `hasSymbols()` (= `!m_modules.isEmpty()`), `loadedModules()` (= `m_modules.keys()`), `moduleCount()` (= `m_modules.size()`), `moduleData(name)` (resolve alias → const ptr or nullptr).

## 2.3 Symbol store algorithms summary
- **Forward:** hash lookup by symbol name within (alias-resolved) module, or ambiguity-checked scan for bare names, plus module-name fallback. Adds `moduleBase + rva`.
- **Reverse:** per-module binary search over sorted `(rva,name)` for the greatest `rva <= target`, with a 0x1000-byte displacement cap, requiring the module be live-attached (base resolvable).

---

# Part 3 — Symbol Downloader (`src/symbol_downloader.h`, `.cpp`)

`class SymbolDownloader : public QObject` (`symbol_downloader.h:12`) — async PDB fetch from the Microsoft public symbol server.

### `struct DownloadRequest` (`symbol_downloader.h:17`)
| Field | Type | Meaning |
|-------|------|---------|
| `moduleName` | `QString` | Display name, e.g. `"ntoskrnl.exe"` |
| `pdbName` | `QString` | PDB filename, e.g. `"ntoskrnl.pdb"` |
| `guidString` | `QString` | 32 hex chars, no dashes |
| `age` | `uint32_t` (=0) | PDB age |

### Static / methods
- **`static QString cacheDir()`** (`symbol_downloader.cpp:19`): `QStandardPaths::writableLocation(AppLocalDataLocation) + "/SymbolCache"`. Rust: `directories`/`dirs` crate → local data dir + `/SymbolCache`.
- **`QString findCached(const DownloadRequest&) const`** (`.cpp:24`): build path `cacheDir()/<pdbName>/<guid><age-as-base16>/<pdbName>`; return it if the file exists, else empty. **Note age is formatted `QString::number(age,16)` — lowercase hex, no `0x`.**
- **`static QString findLocal(moduleFullPath, pdbName)`** (`.cpp:33`): if either arg empty → empty. Candidate = `<dir-of-module>/<pdbName>`; return if exists, else empty.
- **`void download(const DownloadRequest&)`** (`.cpp:44`): GET `https://msdl.microsoft.com/download/symbols/<pdbName>/<guid><age16>/<pdbName>`. Sets `User-Agent: Microsoft-Symbol-Server/10.0.0.0`, redirect policy `NoLessSafeRedirectPolicy`. Calls `cancel()` first (single active download). Captures req fields into lambdas (Qt signal/slot). On `downloadProgress` → `emit progress(moduleName, received, total)` (truncated to `int`). On `finished`:
  - Clear `m_activeReply`, `deleteLater()`.
  - If `reply->error() != NoError` → `emit finished(module, {}, false, "Download failed: <errorString>")`.
  - HTTP status != 200 → `emit finished(..., false, "HTTP <status>")`.
  - Empty body → `finished(..., false, "Empty response")`.
  - Else: `mkpath(cacheDir/<pdbName>/<guid><age16>)`, write to `<dir>/<pdbName>`. On open-fail → `finished(..., false, "Cannot write: <errorString>")`. On success → `emit finished(module, path, true, {})`.
- **`void cancel()`** (`.cpp:115`): if active reply, `abort()` + `deleteLater()`, null it.

### Signals
- `progress(QString moduleName, int bytesReceived, int bytesTotal)`
- `finished(QString moduleName, QString localPath, bool success, QString error)`

### Qt → Rust mapping
| Qt | Rust |
|----|------|
| `QNetworkAccessManager` / `QNetworkReply` | `reqwest` (async or blocking) |
| `QObject` signals (`progress`/`finished`) | callback closures / `tokio::sync::mpsc` / a small event enum |
| `QStandardPaths::AppLocalDataLocation` | `directories::ProjectDirs::data_local_dir()` |
| `QDir().mkpath` / `QFile` write | `std::fs::create_dir_all` + `std::fs::write` |
| `QNetworkRequest` UA header + redirect policy | `reqwest::header::USER_AGENT` + default redirect policy |

The download/cache path format (`<pdb>/<guid><age-hex>/<pdb>`) and the MS server URL are exact behavioral contracts — replicate verbatim. **Threading:** Qt's NAM is async on the GUI event loop; only one download active at a time (`cancel()` before new). In Rust this maps cleanly to a single async task with cancellation.

**Platform note:** the symbol server is Windows-PDB-centric but the HTTP download itself is cross-platform; nothing is `#[cfg]`-gated in this file. UI wiring lives in `main.cpp:8298+` (out of scope), but the contract is: download → `extractPdbSymbols` → `SymbolStore::addModule` / `addModuleTypeIndices` / `addModuleTypes`.

---

# Part 4 — Name Providers (`src/names/`)

## 4.1 `struct NamedAddress` (`name_provider.h:19`)
| Field | Type | Meaning |
|-------|------|---------|
| `name` | `QString` | Canonical identifier — used for reverse lookups |
| `displayName` | `QString` | Optional humanised form (e.g. demangled). Empty → consumers use `name` |
| `address` | `uint64_t` (=0) | **Absolute** address; `0` = "no live address" (address-less PDB types) — renders `—`, not navigable |
| `size` | `uint32_t` (=0) | 0 = unknown |
| `typeIndex` | `uint32_t` (=0) | Non-zero ⇒ "Import type" affordance |
| `source` | `QString` | Filled by aggregation (= provider id / module) |
| `kind` | `QString` | Sub-kind: `"symbol"`/`"type"`/`"bookmark"`/`"rtti"`/`"struct"`/`"union"`/`"enum"` |
| `meta` | `QString` | Provider-private (e.g. PDB path for type-import) |

## 4.2 `class NameProvider` (`name_provider.h:40`) — abstract base
Pure-virtual: `QString id()`, `QString displayName()`, `QVector<NamedAddress> entries(const Provider* active)`.
Virtual with defaults:
- `uint32_t accent()` → 0 ("no opinion"; packed `0xAARRGGBB`).
- `QString nameFor(uint64_t addr, const Provider* active)` (`name_provider.cpp:5`): default **linear scan** of `entries(active)`; returns `.name` of first entry with `.address == addr`; `addr==0` → empty.
- `uint64_t addressFor(const QString& name, const Provider* active)` (`name_provider.cpp:12`): default linear scan; first entry with matching `.name`; empty name → 0.
- `bool supportsAdd()` → false; `bool add(name, address)` → false.
- `bool supportsRemove()` → false; `bool remove(name)` → false.

Rust: a `trait NameProvider` with default-method `name_for`/`address_for` doing the scan; impls override for O(1).

## 4.3 `class NameRegistry : public QObject` (`name_registry.h:16`) — singleton aggregator
- `static NameRegistry& instance()` (Meyers singleton).
- `void registerProvider(shared_ptr<NameProvider>)` (`name_registry.cpp:10`): null → no-op. **Idempotent by `id()`**: if a provider with the same id exists, replace it in place; else append. Emits `providersChanged()`.
- `void unregisterProvider(const QString& id)`: remove first match by id; emit `providersChanged()`.
- `QVector<shared_ptr<NameProvider>> providers() const`.
- `QString nameFor(uint64_t addr, const Provider* active) const` (`name_registry.cpp:35`): `addr==0` → empty; iterate providers **in registration order**; return first non-empty `p->nameFor()`.
- `uint64_t addressFor(const QString& name, ...)`: empty name → 0; first nonzero `p->addressFor()`.
- `void emitChanged()` → `emit providersChanged()`.
- Signal `providersChanged()`.

Registration order (set in `main.cpp:1054`): **PdbNameProvider, PdbTypeProvider, RttiNameProvider, BookmarkNameProvider** — so PDB symbols win reverse lookups over RTTI/bookmarks. The RttiNameProvider singleton is registered with a no-op deleter shared_ptr.

Rust: `Vec<Arc<dyn NameProvider>>` behind a global `Mutex`; "first non-empty wins" iteration; a change-notification channel/callback in place of the Qt signal.

## 4.4 `PdbNameProvider` (`pdb_name_provider.{h,cpp}`)
- `id()="pdb-symbols"`, `displayName()="PDB Symbols"`, `accent()` = theme `syntaxKeyword.rgba()` (GUI-only).
- `entries(active)` (`pdb_name_provider.cpp:23`): for each loaded module's `nameToRva`: build `NamedAddress` with `name=symbolName`, `displayName=humanizeSymbolName(name)`, `address = base!=0 ? base+rva : 0` (where `base=moduleBaseFor(active, mod)`), `typeIndex` from `nameToTypeIndex` if present, `kind="symbol"`, `meta=pdbPath`.
  - **Critical correctness rule** (`pdb_name_provider.cpp:34-48`): when the owning module is NOT attached to the live target (`base==0`), `address` is set to 0 (not `0+rva`). Publishing `0+rva` would make the editor's address→name annotation false-match a fresh user document at the symbol's RVA. The row still appears (for browsing) but reverse lookup skips it.
- `nameFor(addr, active)` (`.cpp:68`): `addr==0` → empty. `raw = SymbolStore::getSymbolForAddress(addr, active)`; empty → empty. Splits `"module!"` prefix from the rest; splits the symbol from a `"+0xN"` suffix; runs the symbol through `humanizeSymbolName`; returns `prefix + (humanized or raw sym) + suffix`. (Routes through SymbolStore's live-attached, binary-search reverse lookup — never false-matches an unloaded PDB.)
- `addressFor(name, active)` (`.cpp:83`): delegates to `SymbolStore::resolve(name, active, &ok)`; returns address if ok, else 0. Accepts both `module!symbol` and bare.
- `moduleBaseFor(active, canonical)` (static, `.cpp:14`): same `symbolToAddress` + `.exe/.dll/.sys` cascade as `SymbolStore::getModuleBase`.

## 4.5 `PdbTypeProvider` (`pdb_type_provider.{h,cpp}`)
- `id()="pdb-types"`, `displayName()="PDB Types"`, `accent()` = theme `syntaxType.rgba()`.
- `entries(active /*ignored*/)` (`pdb_type_provider.cpp:16`): for each loaded module's `set->types`: `NamedAddress{ name=ti.name, displayName=humanizeSymbolName(ti.name), address=0 (always address-less), size=(uint32_t)ti.size, typeIndex=ti.typeIndex, kind = ti.isEnum?"enum":ti.isUnion?"union":"struct", meta=pdbPath }`.
- Uses inherited (linear-scan) `nameFor`/`addressFor`, but since all addresses are 0, `nameFor` always returns empty (`addr==0` guard) and `addressFor` returns 0 — effectively reverse-lookup-inert. They exist purely for the Symbols panel listing + type import.

## 4.6 `RttiNameProvider` (`rtti_name_provider.{h,cpp}`) — singleton, mutex-protected
- `static RttiNameProvider& instance()`. `id()="rtti"`, `displayName()="RTTI"`, `accent()` = theme `markerCycle.rgba()`.
- Members: `mutable QMutex m_lock`; `QVector<NamedAddress> m_hits`; `QHash<QString,int> m_byKey` (dedupe key `"<name>@<addr-hex>"` → index).
- `entries(active /*ignored*/)` (`.cpp:18`): lock, **return a copy** of `m_hits`.
- `void push(name, address, moduleName={})` (`.cpp:23`): if name empty OR address==0 → no-op. Build key `name + "@" + hex(address)`. Lock; if key already present → return (idempotent dedupe). Else create `NamedAddress{ name, address, kind="rtti", source=moduleName (if non-empty) }`, record index, append. Unlock; then `NameRegistry::instance().emitChanged()` (outside the lock).
- `void clear()` (`.cpp:41`): lock, clear `m_hits` + `m_byKey`; then `emitChanged()`.
- `void clearForModule(moduleName)` (`.cpp:50`): empty name → no-op. Lock; rebuild keeping only hits whose `.source != moduleName`, regenerating `m_byKey`; then `emitChanged()`.
- Uses inherited linear-scan `nameFor`/`addressFor` over the hits.

**Concurrency:** the only mutexed component in this subsystem — RTTI discoveries can be pushed from compose (potentially a worker, in the live app) while the panel reads. Rust: `Mutex<RttiHits>` with `entries` cloning out. **Important:** `emitChanged()` is always called *after* releasing the lock (avoids re-entrancy / deadlock if a listener calls back) — preserve this ordering.

## 4.7 `BookmarkNameProvider` (`bookmark_name_provider.{h,cpp}`)
- Constructed with `ActiveCtrlFn = std::function<RcxController*()>` (callback to fetch the active tab's controller — bookmarks are per-document).
- `id()="bookmark"`, `displayName()="Bookmarks"`, `accent()` = theme `indDataChanged.rgba()`.
- `entries(active)` (`.cpp:38`): get active controller + document; if none → empty. For each bookmark `b` in `document()->tree.bookmarks`: `NamedAddress{ name=b.name, address=evaluateFormula(b.addressFormula, active, ptrSize), kind="bookmark" }`. `ptrSize = document()->tree.pointerSize`.
- `supportsAdd()=true`; `add(name, address)` (`.cpp:54`): no controller → false. Builds formula `"0x<hex address>"`, calls `ctrl->addBookmark(name, formula)`, returns true.
- `supportsRemove()=true`; `remove(name)` (`.cpp:62`): find bookmark by name; `ctrl->removeBookmark(i)`; true; else false.
- `evaluateFormula(formula, prov, ptrSize)` (static, `.cpp:16`): wires `AddressParserCallbacks` (`resolveModule` → `prov->symbolToAddress`; `readPointer` → `prov->read`; `resolveIdentifier` → `SymbolStore::resolve`), calls `AddressParser::evaluate(formula, ptrSize?:8, &cbs)`; returns `result.value` on ok, else 0. Bookmark addresses are *formulas*, re-evaluated each `entries()` call against the live provider.

This provider is the only one that depends on `RcxController`/`NodeTree`/`AddressParser` — keep that dependency edge in the port. The others depend only on `SymbolStore` + `Provider`.

## 4.8 `humanizeSymbolName` (`symbol_demangle.{h,cpp}`) — demangler façade
`QString humanizeSymbolName(const QString& mangled)` (`symbol_demangle.cpp:11`):
1. Empty → empty.
2. **MSVC RTTI type descriptor** (`startsWith(".?A")` or `"?A"`): `d = demangleRttiName(mangled)`; return `d` if it changed, else empty.
3. **Itanium** (`startsWith("_Z")` — functions `_Z`, nested `_ZN`, vtable/typeinfo `_ZT`...): `d = demangleItaniumName(mangled)`; return `d` if changed and non-empty, else empty.
4. **MSVC function/method mangle** (`Q_OS_WIN` only): strip a leading `_`; require leading `?` (else empty). Call `UnDecorateSymbolName` with flags `UNDNAME_NAME_ONLY | UNDNAME_NO_ACCESS_SPECIFIERS | UNDNAME_NO_THISTYPE | UNDNAME_NO_RETURN_UDT_MODEL` into a 2048-byte buffer. If `n==0` → empty. If result empty or equals input → empty. Else return it.
5. **Non-Windows**: return empty (MSVC `?...` symbols stay raw; PDBs are Windows-specific; native ELF/Mach-O symbols are Itanium, handled in step 3).

**Contract:** returns **empty** when the input is already human-readable (extern "C", plain C names) — callers fall back to the raw name. **Important behavioral subtlety: a non-empty return always means "use this instead of raw"; an empty return means "keep raw".**

### Platform-specific demangle mapping (for the port)
| C++ path | Platform | Rust equivalent |
|----------|----------|-----------------|
| `demangleRttiName` in-house | all | hand-rolled MSVC RTTI type-descriptor parser |
| `demangleItaniumName` via `__cxa_demangle` | GCC/Clang/MinGW | `cpp_demangle` / `symbolic-demangle` (+ hand-rolled bare-type fallback) |
| `UnDecorateSymbolName` (dbghelp) | Windows only (`Q_OS_WIN`) | `msvc-demangler` crate — **cross-platform**, so the port can demangle MSVC `?...` symbols on Linux too (an improvement, but match the *empty-on-unchanged* contract) |

`Q_OS_WIN` gate (`symbol_demangle.cpp:4` includes `<windows.h>`/`<dbghelp.h>`). For Rust, replace the entire `#ifdef Q_OS_WIN` block with a cross-platform `msvc-demangler` call but **preserve the "return empty when unchanged / not a `?`-mangle" semantics**. Apply the same name-only flags conceptually (strip return types / access specifiers / `this` types) via `msvc_demangler::DemangleFlags`.

---

# Part 5 — Integration with `compose` (for context; compose itself is a separate port unit)

The RTTI walker is invoked from the layout-composer's auto-detect (`src/compose.cpp`). Behaviors the tests in `test_rtti_hint.cpp` depend on:

- **`rttiForVtable(state, prov, candidateAddr)`** (`compose.cpp:269`): per-compose-pass cache. `state.rttiCache: QHash<uint64_t, RttiInfo>` memoizes both successes AND failures (so an arbitrary qword isn't re-walked every refresh). Module list `state.rttiModules` is fetched **once per pass** (`rttiModulesCached` flag). Candidate must land inside some cached module (`base <= addr < base+size`) before any walk is attempted — else cached negative.
- It calls `walkRtti(prov, addr, ptrSize=8, maxVtableSlots=0)` first; on `!ok` falls back to `walkRttiItanium(...)` with the same args. **`maxVtableSlots=0`** means no vtable enumeration — just the class name (all the inline chip needs), and crucially fast.
- On success with non-empty `demangledName`, fires `g_rttiDiscoveryHook(demangledName, vtableAddress, moduleName)` (`core.h:391`), which `main.cpp:1069` wires to `RttiNameProvider::instance().push(...)`. Tests leave the hook null (silent).
- **`modulesEnumeratedFewTimesNotPerLine`** test: with 32 fields pointing at the same vtable, `enumerateModules()` must be called **O(1)** (asserts `<= 4`, and `< fieldCount`) — driven by the per-pass module cache plus the `RttiInfo` memo. Replicate this caching in the Rust composer.
- **`itaniumAutoDetectFallsBack`**: MSVC walker rejects an Itanium-shaped buffer (no COL signature), then Itanium fallback produces the chip. (The fake provider's `enumerateModules()` override is essential — without it Itanium's strict module check fails.)
- **`hintIndependentOfTypeHintsToggle`**: RTTI chip must appear even with `typeHints=false`.

The RTTI browser dialog (`main.cpp:4556 showRttiBrowser`) calls full `walkRtti` (default `maxVtableSlots=64`) — tries the snapshot provider first (its page cache holds the vtable/type_info/name pages from the compose pass), falling back to the live provider. The `SnapshotProvider::read` (`snapshot_provider.h:40`) falls through to the real provider for un-prefetched pages — required so the Itanium walker can peek `vtable[-8]`/`type_info` bytes in module `.rdata`.

---

# Part 6 — Qt → Rust type mapping (consolidated)

| Qt type | Usage here | Rust equivalent |
|---------|-----------|-----------------|
| `QString` | all names/errors | `String` |
| `QByteArray` | C-string reads, download body | `Vec<u8>` / `bytes::Bytes` |
| `QVector<T>` | bases, vtable, entries, types | `Vec<T>` |
| `QHash<K,V>` | nameToRva, modules, aliases, dedupe | `HashMap<K,V>` |
| `QPair<A,B>` | `(rva,name)`, `(name,rva)` | tuple `(A,B)` |
| `QStringList` | demangle segment split | `Vec<String>` |
| `QRegularExpression` | (included but unused in rtti.cpp) | — |
| `std::sort` / `std::upper_bound` | rvaToName sort + binary search | `Vec::sort_by` + `slice::partition_point`/`binary_search_by` |
| `QMutex` / `QMutexLocker` | RttiNameProvider only | `std::sync::Mutex` |
| `QObject` + signals (`NameRegistry`, `SymbolDownloader`) | change notification, download events | callback closures / `mpsc` channel / event enum |
| `QNetworkAccessManager`/`QNetworkReply` | symbol download | `reqwest` |
| `QStandardPaths` / `QDir` / `QFile` | cache dir + file IO | `directories`/`dirs` + `std::fs` |
| `QColor::rgba()` packed | provider `accent()` | `u32` (`0xAARRGGBB`); UI-only, can default to 0 in headless |
| `abi::__cxa_demangle` | Itanium demangle | `cpp_demangle` / `symbolic-demangle` |
| `UnDecorateSymbolName` (dbghelp) | MSVC symbol demangle | `msvc-demangler` |
| `qDebug()` | logging | `log`/`tracing` |
| `std::function<RcxController*()>` | bookmark active-ctrl callback | boxed closure `Box<dyn Fn() -> Option<...>>` |
| Meyers singleton (`instance()`) | SymbolStore, NameRegistry, RttiNameProvider | `OnceLock`/`Lazy` global + interior mutability |

---

# Part 7 — Edge cases & subtle behaviors checklist (test-load-bearing)

1. **`demangleRttiName` passthrough** on non-`.?A` input and on malformed (no `@@`) input — returned verbatim. Tests: `demangleMalformed`.
2. **Segment reversal**: `.?AVZ@Y@X@@` → `X::Y::Z` (inner-first → outer::inner).
3. **`demangleItaniumName` `St`→`std` shorthand**; `N...E` nesting; length-prefix; verbatim passthrough.
4. **COL signature guard** (0 or 1 only) → error contains `"signature"`. Test corrupts to `0xDEAD`.
5. **CHD `numBaseClasses > 256` guard** → error contains `"unreasonably"`. Test sets 9999.
6. **Vtable terminator**: stops at first null slot or non-module pointer; synthetic 5 methods + null slot → `vtable.size()==5`.
7. **Synthetic image-base fallback** (MSVC only): when no enumerable module, read COL+0x14 (`pSelf`) as image base. Itanium does NOT have this fallback (requires real modules).
8. **Itanium offset_to_top magnitude filter** (`> 0x1000000`) → error contains `"offset_to_top"`.
9. **Itanium name printable-ASCII filter** + first-char marker (`digit | N S P K R`) → error contains `"mangle"` on bad input.
10. **Itanium `*` vague-linkage prefix**: stored in `rawName`, stripped before demangle.
11. **`SymbolStore::resolve` ambiguity**: bare symbol matching >1 module returns 0 (not first match).
12. **`getSymbolForAddress` 0x1000 displacement cap** + live-attach requirement (base must resolve, else skip).
13. **`PdbNameProvider::entries` base==0 → address=0** (no false-match against user docs).
14. **`humanizeSymbolName` returns empty when unchanged** → callers keep raw name.
15. **`RttiNameProvider` dedupe** by `name@hex(addr)`; `push` ignores empty name / address 0; `emitChanged` called *after* unlocking.
16. **`NameRegistry::registerProvider` idempotency** by `id()` (replace in place).
17. **Compose per-pass module-enumeration caching** is O(1) in field count (test `modulesEnumeratedFewTimesNotPerLine`).
18. **`addModule` first-wins symbol dedupe** + auto-alias registration of the raw module name.

---

# Part 8 — Recommended Rust crates
- `cpp_demangle` (Itanium) — primary Itanium demangler.
- `msvc-demangler` — MSVC `?...` symbol demangling (cross-platform; replaces the Windows-only dbghelp path).
- `symbolic` / `symbolic-demangle` — alternative unified demangler façade (auto-detects ABI); could back `humanizeSymbolName`.
- `reqwest` — symbol-server HTTP download.
- `directories` (or `dirs`) — cache directory resolution.
- `byteorder` / `zerocopy` — little-endian struct field reads from the `Provider` buffer.
- Hand-rolled parsers required for: MSVC RTTI type-descriptor demangle (`.?AV...@@`) and the Itanium bare-type-name fallback (`3Foo`, `N3Bar3FooE`, `St9type_info`) — off-the-shelf crates expect full `_Z`-prefixed symbols, not RTTI's bare `__name`.

---

# Part 9 — Public API surface (count)

RTTI: `walkRtti`, `walkRttiItanium`, `demangleRttiName`, `demangleItaniumName`, `findOwningModule` (5 free functions) + 4 structs (`RttiBaseClass`, `RttiVirtualMethod`, `RttiInfo`, `OwningModule`).
SymbolStore: 1 class, ~16 public methods + `PdbSymbolSet` struct.
SymbolDownloader: 1 QObject class, 5 methods + 2 signals + `DownloadRequest` struct.
Names: `NamedAddress` struct + `NameProvider` base (10 virtuals) + `NameRegistry` (7 methods, 1 signal) + 4 concrete providers + `humanizeSymbolName`.

Total distinct public functions/methods (excluding inline trivial accessors): ~45.
