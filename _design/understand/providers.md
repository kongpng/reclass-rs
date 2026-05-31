# Subsystem: Data-source abstraction (`Provider` trait) + registry + built-in file/buffer sources

Key (`providers`). Expected portability: **mostly-portable**.

This document maps the C++/Qt6 `Provider` abstraction, its benign built-in implementations
(buffer, file, snapshot, null), and the `ProviderRegistry`, precisely enough that a Rust
implementer can reproduce 1:1 behavior without re-reading the C++.

> SCOPE: The live OS/process/kernel/remote/WinDbg sources are OUT OF SCOPE plugins. They are
> represented here only as the *shape* the trait must leave room for (vtable/PEB/region/kernel
> methods that default to "not available"). Implement only: `BufferProvider` (in-memory bytes +
> file load), `SnapshotProvider` (page-cache), `NullProvider`, and the `ProviderRegistry`.

Source files studied (all read in full):
- `src/providers/provider.h` (abstract base + value helpers + region/module/thread/kernel structs)
- `src/providers/buffer_provider.h`
- `src/providers/null_provider.h`
- `src/providers/snapshot_provider.h`
- `src/providerregistry.h` / `src/providerregistry.cpp`
- `src/iplugin.h` (interface signatures only; plugin impls out of scope)
- Tests: `tests/test_provider.cpp`, `tests/test_source_provider.cpp`, `tests/test_source_management.cpp`
- Cross-references into `src/controller.h` / `src/controller.cpp` (how providers are constructed,
  swapped, and held) and `src/pluginmanager.cpp` (how identifiers are derived).

---

## 1. Purpose

A `Provider` is the single abstraction the rest of Reclass uses to read/write the bytes that a
struct tree is laid over. Everything above it (the node tree, the compose/render pipeline, the
scanner, RTTI walking) treats memory purely as "read `len` bytes at absolute address `addr`",
plus optional metadata (regions, modules, symbols, pointer size, base address). Concrete
data sources (a file's bytes, an in-memory buffer, a snapshot page-cache, or a live process via
a plugin) implement this one interface.

The base class `rcx::Provider` (`provider.h:38`) is an abstract C++ class with exactly **two pure
virtuals** (`read`, `size`) and a long list of virtual methods with sensible defaults. It also
carries **non-virtual convenience helpers** (`isValid`, `readAs<T>`, `readU8/16/32/64`,
`readF32/64`, `readBytes`, `writeBytes`) layered on top of `read`/`write`. The whole hierarchy is
header-only; there is no `provider.cpp`.

The Rust port should define a `Provider` **trait** with required methods `read` and `size` and
**default methods** for everything else, plus free helper methods (probably provided methods on
the trait or an extension trait) for the typed reads. Built-in implementors: `BufferProvider`,
`SnapshotProvider`, `NullProvider`. The registry is a separate, UI-facing global singleton.

---

## 2. Key types / structs / enums

### 2.1 `enum class RegionType : uint8_t` (`provider.h:13-17`)
Classification of a memory region; used by the scanner to skip uninteresting pages.
- `Image   = 0` — loaded module (PE/ELF/Mach-O): code + `.rdata` + `.data`.
- `Mapped  = 1` — memory-mapped file or shared section.
- `Private = 2` — heap/stack/VirtualAlloc; mutable user data lives here.

Rust: `#[repr(u8)] enum RegionType { Image = 0, Mapped = 1, Private = 2 }`. Default is `Private`.

### 2.2 `struct MemoryRegion` (`provider.h:19-29`)
| field        | type         | default            | meaning |
|--------------|--------------|--------------------|---------|
| `base`       | `uint64_t`   | `0`                | region start VA |
| `size`       | `uint64_t`   | `0`                | byte length |
| `readable`   | `bool`       | `true`             | |
| `writable`   | `bool`       | `false`            | |
| `executable` | `bool`       | `false`            | |
| `moduleName` | `QString`    | empty              | owning module name; source-of-truth for the scanner's Module column formatter (`scanner.cpp::formatRegionContext`) |
| `type`       | `RegionType` | `RegionType::Private` | **field is declared last** so legacy positional initializers `{base,size,r,w,x,name}` still compile and default `type` to `Private`. |

Rust: plain struct with `Default`. `moduleName` -> `String`. Keep field order only if you mimic
positional construction; in Rust use named fields / a builder so the ordering note is moot.

### 2.3 `struct VtopResult` (`provider.h:31-36`)
Kernel virtual->physical translation result. **Out-of-scope kernel feature**, but the type must
exist because `translateAddress` returns it.
- `physical` (`uint64_t`, 0)
- `pml4e, pdpte, pde, pte` (`uint64_t`, 0) — page-table entry values along the walk.
- `pageSize` (`uint8_t`, 0) — `0`=4KB, `1`=2MB, `2`=1GB.
- `valid` (`bool`, false).

Rust: struct with `Default` (all zero/false). Only ever produced by kernel providers (not built-in).

### 2.4 Nested `Provider::ThreadInfo` (`provider.h:99`)
`{ uint64_t tebAddress; uint32_t threadId; }`. Returned by `tebs()`. Built-in providers return empty.

### 2.5 Nested `Provider::ModuleEntry` (`provider.h:102`)
`{ QString name; QString fullPath; uint64_t base; uint64_t size; }`. Returned by
`enumerateModules()`. Built-in providers return empty; `SnapshotProvider` forwards to its real
provider.

### 2.6 `class Provider` (`provider.h:38`) — the abstraction
Abstract base. Virtual `~Provider()`. See §3 for every method.

### 2.7 `class BufferProvider : public Provider` (`buffer_provider.h:8`)
Fields:
- `QByteArray m_data` — the backing bytes (owned, mutable, contiguous).
- `QString m_name` — display name (filename, or empty).

### 2.8 `class NullProvider : public Provider` (`null_provider.h:6`)
No fields. `size()==0`, `read()==false`. Everything else inherited from base defaults.

### 2.9 `class SnapshotProvider : public Provider` (`snapshot_provider.h:16`)
Fields:
- `std::shared_ptr<Provider> m_real` — the underlying real provider (may be null).
- `QHash<uint64_t, QByteArray> m_pages` — map of **page-aligned address -> exactly-4096-byte page**.
- `int m_mainExtent = 0` — logical size of the main struct range; what `size()` returns.
- `QSet<uint64_t> m_permanentPages` — page-aligned addrs marked immutable for the snapshot's life
  (read-only module sections; skip re-reading every refresh tick).
- `static constexpr uint64_t kPageSize = 4096`.
- `static constexpr uint64_t kPageMask = ~(kPageSize - 1)` (i.e. `0xFFFF...F000`).
- Type alias `using PageMap = QHash<uint64_t, QByteArray>;`.

### 2.10 `class ProviderRegistry` (`providerregistry.h:24`) — global singleton
Holds a list of registered data-source descriptors for the Source-picker UI. Note: the registry
stores **descriptors** (`ProviderInfo`), not live `Provider` instances. Built-in entries store a
factory closure; plugin entries store a borrowed `IProviderPlugin*`.

#### `struct ProviderRegistry::ProviderInfo` (`providerregistry.h:29-44`)
| field         | type             | meaning |
|---------------|------------------|---------|
| `name`        | `QString`        | display name, e.g. "Process Memory". Also the **routing key** stored in the menu action's `data()` for `selectSource()`. |
| `identifier`  | `QString`        | unique ID, e.g. "process"/"processmemory". Lookup key for `findProvider`. |
| `plugin`      | `IProviderPlugin*` | borrowed plugin pointer if plugin-based, else `nullptr`. **Not owned.** |
| `factory`     | `BuiltinFactory` | `std::function<bool(QWidget* parent, QString* target)>` if built-in, else null. |
| `isBuiltin`   | `bool`           | discriminator. |
| `dllFileName` | `QString`        | original DLL/SO filename (plugin only); empty for built-ins. |

Two constructors:
- Plugin ctor (`:37`): `(name, identifier, IProviderPlugin* p, dll={})` -> sets `plugin=p`,
  `factory=nullptr`, `isBuiltin=false`, `dllFileName=dll`.
- Builtin ctor (`:42`): `(name, identifier, BuiltinFactory f)` -> sets `plugin=nullptr`,
  `factory=f`, `isBuiltin=true`. (`dllFileName` left default-empty.)

`BuiltinFactory` (`providerregistry.h:27`): `std::function<bool(QWidget* parent, QString* target)>`
— a callback that (typically) shows a picker dialog and writes the chosen target string into
`*target`, returning `true` if the user committed and `false` if cancelled. (Same contract as
`IProviderPlugin::selectTarget`.) Rust equivalent: `Box<dyn Fn(/* parent */, &mut String) -> bool>`
— but the parent is a `QWidget*` UI concern; in a headless port the factory degrades to
`Box<dyn Fn(&mut String) -> bool>` or is omitted until UI exists.

Private state: `QList<ProviderInfo> m_providers;` (insertion-ordered list).

### 2.11 `struct SavedSourceDisplay` (`providerregistry.h:13-16`)
UI helper passed to `populateSourceMenu`: `{ QString text; bool active = false; }`. One entry per
saved source; rendered as a checkable menu item.

### 2.12 Plugin interfaces (`iplugin.h`) — signatures only, IMPLEMENTATIONS OUT OF SCOPE
The registry references `IProviderPlugin*`. For the port, model these as a Rust trait the
out-of-scope plugins would implement; built-in code never instantiates them.

- `class IPlugin` (`iplugin.h:23`): `Name()/Version()/Author()/Description()` (`std::string`),
  `Icon()` (`QIcon`, default empty), `Type()` (`k_EType` enum: only `ProviderPlugin`), `LoadType()`
  (`k_ELoadType`: `k_ELoadTypeAuto` / `k_ELoadTypeManual`).
- `class IProviderPlugin : public IPlugin` (`iplugin.h:83`):
  - `Type()` overridden to return `ProviderPlugin`.
  - `bool canHandle(const QString& target) const` — pure.
  - `std::unique_ptr<rcx::Provider> createProvider(const QString& target, QString* errorMsg=nullptr)` — pure; the only place a *live* `Provider` is born.
  - `uint64_t getInitialBaseAddress(const QString& target) const` (default 0).
  - `bool selectTarget(QWidget* parent, QString* target)` — pure; picker dialog.
  - `QVector<PluginProcessInfo> enumerateProcesses()` (default empty).
  - `bool providesProcessList() const` (default false).
  - `void populatePluginMenu(QMenu*)` (default no-op) — called by the registry while building the source menu.
- `struct PluginProcessInfo` (`iplugin.h:65`): `{ uint32_t pid; QString name; QString path; QIcon icon; bool is32Bit=false; }`.
- `RCX_PLUGIN_EXPORT` macro: `__declspec(dllexport)` on `_WIN32`, else `__attribute__((visibility("default")))` (`iplugin.h:7-11`). **Only platform-specific code in this subsystem**; irrelevant to a Rust built-in-only port (gate behind `#[cfg]` if you ever add C-ABI plugin loading).
- `typedef IPlugin* (*CreatePluginFunc)();` and `#define IPLUGIN_IID "com.reclass.IPlugin/1.0"` — plugin loader plumbing, out of scope.

---

## 3. `Provider` — every method (signature, behavior, edge cases)

`addr` is always an **absolute address** (`uint64_t`); built-in file/buffer providers treat it as a
0-based offset into their bytes. `len` is a signed `int` throughout (Qt convention) — watch the
sign in the Rust port.

### 3.1 Pure virtuals (subclasses MUST implement)
- `virtual bool read(uint64_t addr, void* buf, int len) const = 0;` (`:43`)
  Read exactly `len` bytes at `addr` into `buf`. Returns `true` on full success, `false` on any
  failure (out of bounds, etc.). On `false` the contents of `buf` are **unspecified/untouched** by
  the base contract — see `readBytes` and `NullProvider` test below for how callers handle it.
- `virtual int size() const = 0;` (`:44`)
  Logical size in bytes. `0` means "no/empty source". Note: **`int`, not `uint64_t`** — capped at
  `INT_MAX` (~2 GB). Used by `isValid()` and the default `isReadable()` bounds check.

### 3.2 Optional overrides (defaults given)
- `virtual bool write(uint64_t addr, const void* buf, int len)` -> default `return false;` (`:47`).
  Note: **non-const** (mutates). `Q_UNUSED` on all params.
- `virtual bool isWritable() const` -> default `false` (`:51`).
- `virtual QString name() const` -> default `{}` (empty) (`:55`). Human label
  ("notepad.exe", "dump.bin", "tcp://..."). Empty name -> UI shows `<Select Source>` placeholder.
- `virtual bool isLive() const` -> default `false` (`:59`). True only for sources whose data can
  change externally (live process/socket). Drives whether auto-refresh runs.
- `virtual QString kind() const` -> default `QStringLiteral("File")` (`:63`). Category tag for the
  command-row Source span ("File"/"Process"/"Socket"). **The default is "File", not empty.**
- `virtual int pointerSize() const` -> default `8` (`:67`). 4 for 32-bit target, 8 for 64-bit.
- `virtual uint64_t base() const` -> default `0` (`:72`). Initial base address (e.g. main module
  base). Controller sets `tree.baseAddress` from this on first attach. **Always 0 for file/buffer.**
- `virtual QString getSymbol(uint64_t addr) const` -> default `{}` (`:78`). Resolve VA->symbol
  ("ntdll.dll+0x1A30"). Empty if unknown. `BufferProvider` returns "" (no symbols in flat files).
- `virtual uint64_t symbolToAddress(const QString& name) const` -> default `0` (`:85`). Reverse of
  `getSymbol`; 0 if not found.
- `virtual QVector<MemoryRegion> enumerateRegions() const` -> default `{}` (`:93`). Committed/readable
  regions for the scan engine. Empty -> scanner falls back to `[0, size())`.
- `virtual uint64_t peb() const` -> default `0` (`:97`). Live-process only.
- `virtual QVector<ThreadInfo> tebs() const` -> default `{}` (`:100`).
- `virtual QVector<ModuleEntry> enumerateModules() const` -> default `{}` (`:103`).
- Kernel paging (override only in kernel providers — out of scope):
  - `virtual bool hasKernelPaging() const` -> `false` (`:106`).
  - `virtual uint64_t getCr3() const` -> `0` (`:107`).
  - `virtual VtopResult translateAddress(uint64_t va) const` -> `{}` (`:108`).
  - `virtual QVector<uint64_t> readPageTable(uint64_t physAddr, int startIdx=0, int count=512) const` -> `{}` (`:111`).

### 3.3 Derived convenience (NON-virtual unless noted; "never override")
- `bool isValid() const { return size() > 0; }` (`:120`). **Non-virtual.** Empty source is invalid.
- `virtual bool isReadable(uint64_t addr, int len) const` (`:122-126`). **This one IS virtual**
  (despite the "never override" comment block above it; `SnapshotProvider` overrides it). Behavior:
  ```
  if (len <= 0) return (len == 0);                 // len==0 -> true (always readable);  len<0 -> false
  uint64_t ulen = (uint64_t)len;
  return addr <= (uint64_t)size() && ulen <= (uint64_t)size() - addr;
  ```
  Bounds check using `size()`. The subtraction `size() - addr` is only evaluated when
  `addr <= size()` (short-circuit `&&`), so no underflow. Equivalent to `addr + len <= size()`
  written underflow-safely. **Edge cases pinned by tests** (`test_provider.cpp`):
  - `isReadable(0,0)` -> `true` even on a zero-size provider (`buffer_isReadable_zeroSizeProvider`).
  - `isReadable(size(), 1)` -> `false` (one past end), `isReadable(size()-1, 1)` -> `true`.
  - `isReadable(0, size()+1)` -> `false`. `isReadable(100,1)` on size 16 -> `false`.
- `template<typename T> T readAs(uint64_t addr) const` (`:128-133`):
  ```
  T v{};                  // value-initialized (zeroed)
  read(addr, &v, sizeof(T));
  return v;               // returns zeroed v if read() failed
  ```
  Native-endian, raw `memcpy`-style reinterpretation of `sizeof(T)` bytes. **On read failure
  returns a zero-initialized `T`** (return value of `read` is ignored). The tests
  (`buffer_readU16_littleEndian`, etc.) confirm **little-endian** decoding on the x86/x64/ARM-LE
  targets: bytes `34 12` -> `0x1234`. Rust port must read native-endian (`from_ne_bytes`) to match;
  practically all targets are little-endian, but stay faithful to "native". Also handles arbitrary
  POD structs (`buffer_readAs_customStruct`).
- Typed shortcuts (`:135-140`), all `readAs<…>`:
  `readU8/U16/U32/U64`, `readF32` (`float`), `readF64` (`double`). **All return 0 on failed read**
  (because `readAs` zero-inits). E.g. `NullProvider::readU8(0) == 0` (`nullProvider_readU8ReturnsZero`).
- `QByteArray readBytes(uint64_t addr, int len) const` (`:142-148`):
  ```
  if (len <= 0) return {};                  // empty for len<=0
  QByteArray buf(len, Qt::Uninitialized);
  if (!read(addr, buf.data(), len)) buf.fill('\0');  // on failure, ZERO the whole buffer
  return buf;
  ```
  Always returns a buffer of exactly `len` bytes for `len>0`. **On any read failure, the entire
  returned buffer is zero-filled** (length preserved, contents `\0`). For `len<=0` returns empty
  (size 0). Pinned by `buffer_readBytes_pastEnd` (size 8, all `\0`) and `buffer_readBytes_zeroLen`,
  and `nullProvider_readBytesReturnsZeroed`.
- `bool writeBytes(uint64_t addr, const QByteArray& d)` (`:150-152`): `return write(addr, d.constData(), d.size());`. **Non-const** (calls `write`).

#### Rust mapping of the trait
- Required: `fn read(&self, addr: u64, buf: &mut [u8]) -> bool;` (replace `void*,int` with `&mut [u8]`; `len = buf.len()`), `fn size(&self) -> i64` or `usize` (decide: C++ caps at `int`; using `usize` avoids the sign issue but breaks the exact `int` arithmetic — keep `i64`/`u64` and clamp to match `size() > 0` and the bounds math). Recommend `fn size(&self) -> u64` and reproduce `isReadable` math with `u64`.
- Default methods for all optional virtuals returning the documented defaults.
- `write(&mut self, ...)` is a mutating method -> in Rust, interior mutability or `&mut self`. Note Reclass holds providers as `std::shared_ptr<Provider>` and writes through them (`writeBytes`), so the Rust equivalent likely needs `&self` + interior mutability (e.g. `Arc<dyn Provider>` with a `Mutex`/`RefCell` inside the buffer/snapshot impls) to keep the same `Arc`-shared semantics. See §6.
- Typed helpers (`read_u8/16/32/64`, `read_f32/64`, `read_as<T: FromBytes>`, `read_bytes`) as provided trait methods, each returning zero/empty on read failure exactly as above.

---

## 4. Built-in implementations

### 4.1 `BufferProvider` (`buffer_provider.h`)
Backs reads/writes with an owned `QByteArray`. Fully in-bounds-checked, fully writable.

- **Ctor** `explicit BufferProvider(QByteArray data, const QString& name = {})` (`:13`):
  `m_data = std::move(data)`, `m_name = name`.
- **`static BufferProvider fromFile(const QString& path)`** (`:17-22`):
  Opens `path` read-only. If open succeeds: `BufferProvider(f.readAll(), QFileInfo(path).fileName())`
  — the **name is the basename** (no directory). If open fails: `BufferProvider({})` — an
  empty/invalid buffer with empty name. **Never throws / never returns an error code**; failure is
  represented by an invalid (size 0) provider. Pinned by `buffer_fromFile_nonexistent` (invalid,
  size 0) and `buffer_fromFile_valid` (name == basename, reads back bytes).
  Rust: `BufferProvider::from_file(path) -> BufferProvider` reading the whole file via
  `std::fs::read`; on `Err` return an empty buffer (do NOT propagate the error, to match parity).
  Name = `Path::file_name()`.
- `int size() const override { return m_data.size(); }` (`:24`).
- `bool read(...) const override` (`:26-30`): `if (!isReadable(addr,len)) return false;` then
  `memcpy(buf, m_data.constData()+addr, len); return true;`. Uses the **base-class `isReadable`**
  (bounds vs `size()`). So a partially-out-of-range read fails entirely (no partial copy) and leaves
  `buf` untouched (caller-visible via `readBytes`/`readAs` zeroing).
- `bool isWritable() const override { return true; }` (`:32`).
- `bool write(...) override` (`:34-38`): `if (!isReadable(addr,len)) return false;` then
  `memcpy(m_data.data()+addr, buf, len); return true;`. **Writes are bounds-checked with the same
  `isReadable`** — a write that would extend past the buffer fails and changes nothing
  (`buffer_write_pastEndFails`). The buffer is **fixed-size**; writes never grow it.
- `QString name() const override { return m_name; }` (`:40`).
- `QString kind() const override { return QStringLiteral("File"); }` (`:41`). (Same as base default,
  overridden explicitly anyway.)
- `QVector<MemoryRegion> enumerateRegions() const override` (`:48-59`): if `m_data.isEmpty()` ->
  `{}`. Otherwise one synthetic region:
  `base=0, size=m_data.size(), readable=true, writable=true, executable=false,
   moduleName = m_name.isEmpty() ? "[buffer]" : m_name, type = RegionType::Mapped`.
  Purpose: scanner's Module column shows e.g. `issue.png+0x6A0A` instead of an empty cell.
- `const QByteArray& data() const` / `QByteArray& data()` (`:61-62`): direct accessors to the bytes
  (const + mutable). Used elsewhere for direct buffer manipulation.

Rust: `struct BufferProvider { data: Vec<u8> /* behind interior mutability for write */, name: String }`.
`size()` returns `data.len()`. `read` -> `isReadable` then `copy_from_slice`. `write` -> bounds-check
then overwrite slice in place. `enumerateRegions` -> single `Mapped` region or empty. Provide
`data()` accessor(s).

### 4.2 `NullProvider` (`null_provider.h`)
The "no source attached" placeholder. The document's `provider` is initialized to a `NullProvider`
(`controller.cpp:164`) and reset to one by `clearSources()` (`controller.cpp:6401`).
- `int size() const override { return 0; }` -> `isValid()` is `false`.
- `bool read(uint64_t, void*, int) const override { return false; }` -> always fails; leaves `buf`
  untouched (`nullProvider_readFails`: a `0xFF` byte stays `0xFF`). Through `readU8`/`readBytes` the
  caller sees zeros (because those helpers zero on failure).
- All other methods inherited from base: `name()` empty (-> `<Select Source>` UI),
  `kind()=="File"`, `isWritable()==false`, `getSymbol()` empty, `enumerateRegions()` empty, etc.

Rust: zero-field unit struct; `read` returns `false`, `size` returns 0, all else default.

### 4.3 `SnapshotProvider` (`snapshot_provider.h`)
A page-cache front over a real provider, so the UI thread composes from a frozen page table with no
blocking I/O. Built during async refresh; reads page-by-page. **In scope** as a built-in even though
its `m_real` is often a (out-of-scope) live provider — the port can wrap any `Arc<dyn Provider>`,
including `BufferProvider` (the tests/self-attach do exactly this) or `NullProvider`.

Constants: `kPageSize=4096`, `kPageMask = ~4095` (clears low 12 bits). "Page-aligned addr" =
`addr & kPageMask`.

- **Ctor** `SnapshotProvider(std::shared_ptr<Provider> real, PageMap pages, int mainExtent)` (`:35`):
  moves all three into fields.
- **`bool read(uint64_t addr, void* buf, int len) const override`** (`:40-72`) — core algorithm:
  ```
  if (len <= 0) return false;            // NOTE: differs from base readBytes (which treats len==0 as empty/ok)
  cur = addr; remaining = len; out = buf;
  while (remaining > 0):
      pageAddr = cur & kPageMask;
      pageOff  = cur - pageAddr;                       // 0..4095
      chunk    = min(remaining, kPageSize - pageOff);  // bytes within this page
      if pages contains pageAddr:
          memcpy(out, page.data() + pageOff, chunk);   // serve from cache
      else if (m_real):
          if (!m_real->read(cur, out, chunk)) memset(out, 0, chunk);  // fall through to real; zero on its failure
      else:
          memset(out, 0, chunk);                       // no cache, no real -> zeros
      out += chunk; cur += chunk; remaining -= chunk;
  return true;                                          // ALWAYS true for len>0
  ```
  Key behaviors:
  - Splits the read at 4096-byte page boundaries; each page served independently.
  - Cache hit -> copy from cached page. Miss + real provider present -> read-through to `m_real`
    (zero that chunk if the real read fails). Miss + no real -> zeros.
  - **Always returns `true`** for `len>0` (it always fills `buf`, possibly with zeros). Contrast
    `BufferProvider::read` which returns `false` out of bounds. So `readBytes`/`readAs` never see the
    failure path through a snapshot — they always get the (possibly zero) bytes.
  - The read-through (`else if m_real`) exists for the auto-RTTI hint: `walkRttiItanium` peeks
    `vtable[-8]`/type_info bytes in module `.rdata` that `collectPointerRanges` only pre-fetches for
    *expanded* typed pointers; collapsed pointers leave those pages out of the snapshot, so a handful
    of read-through qword reads happen on the UI thread.
- **`bool isReadable(uint64_t addr, int len) const override`** (`:74-91`) — overrides base:
  ```
  if (len <= 0) return (len == 0);              // same len==0 -> true as base
  end = addr + len; if (end < addr) return false;   // overflow guard
  for (p = addr & kPageMask; p < end; p += kPageSize):
      if (!m_pages.contains(p)):
          if (m_real && m_real->isReadable(addr, len)) return true;  // defer to real's bounds check
          return false;
  return true;
  ```
  All touched pages cached -> readable. First uncached page -> defer to `m_real->isReadable(addr,len)`
  (e.g. a process provider returns true while its handle is open, enabling the read-through path);
  if no real or real says no -> false. Has an explicit `end < addr` overflow guard.
- `int size() const override { return m_mainExtent; }` (`:93`). Logical extent of the main struct
  range, **independent of how many pages are cached**.
- `bool isWritable() const` -> `m_real ? m_real->isWritable() : false` (`:94`).
- `bool isLive() const` -> `m_real ? m_real->isLive() : false` (`:95`).
- `QString name() const` -> `m_real ? m_real->name() : QString()` (`:96`).
- `QString kind() const` -> `m_real ? m_real->kind() : "File"` (`:97`).
- `int pointerSize() const` -> `m_real ? ... : 8` (`:98`).
- `uint64_t base() const` -> `m_real ? ... : 0` (`:99`).
- `QString getSymbol(uint64_t)` -> forwards to `m_real` else empty (`:100`).
- `uint64_t symbolToAddress(const QString&)` -> forwards else 0 (`:103`).
- `QVector<ModuleEntry> enumerateModules() const` -> forwards to `m_real` else empty (`:111`).
  Comment: without this, compose's auto-RTTI `findOwningModule` gets an empty list and refuses to
  walk. Real provider already cached its module list at attach.
- `QVector<MemoryRegion> enumerateRegions() const` -> forwards else empty (`:114`).
- `uint64_t peb() const` / `QVector<ThreadInfo> tebs() const` -> forward else 0 / empty (`:117-120`).
- **`bool write(uint64_t addr, const void* buf, int len) override`** (`:122-127`):
  `if (!m_real) return false;` else `ok = m_real->write(...); if (ok) patchPages(addr,buf,len); return ok;`.
  Writes go to the real provider; on success the cached pages are patched so the snapshot reflects
  the new bytes immediately (write-through to cache).
- **`void updatePages(PageMap pages, int mainExtent)`** (`:130-133`): wholesale replace `m_pages`
  and `m_mainExtent`. Called after a full async read completes.
- **`void mergePages(const PageMap& fresh, int mainExtent)`** (`:139-143`): insert each fresh page
  (overwrite-if-present via `QHash::insert`), keep existing pages not present in `fresh`, set
  `m_mainExtent`. Used by per-tick refresh once page-skipping (permanent/stable/out-of-viewport)
  began — an unread page retains its previous bytes instead of vanishing.
- **`void markPermanent(uint64_t pageAddr)`** (`:148`): `m_permanentPages.insert(pageAddr & kPageMask)`.
- **`bool isPermanent(uint64_t pageAddr) const`** (`:151`): `m_permanentPages.contains(pageAddr & kPageMask)`.
- **`void clearPermanent()`** (`:154`): clears the set.
- **`void patchPages(uint64_t addr, const void* buf, int len)`** (`:157-173`): same page-splitting
  loop as `read`, but **only patches pages that already exist** in `m_pages` (`find` != end);
  missing pages are skipped (the `src`/`cur`/`remaining` still advance). Used after a user value write
  and internally by `write`. Note: no `len<=0` guard; with `len<=0` the `while (remaining>0)` never
  runs, so it's a harmless no-op.
- **`const PageMap& pages() const`** (`:175`) and **`const QSet<uint64_t>& permanentPages() const`** (`:176`) accessors.

Rust mapping for `SnapshotProvider`:
- `m_real: Option<Arc<dyn Provider>>` (the `std::shared_ptr` can be null -> `Option`).
- `m_pages: HashMap<u64, [u8; 4096]>` (or `HashMap<u64, Box<[u8]>>`; pages are exactly 4096 bytes).
- `m_main_extent: i64/u64`, `m_permanent_pages: HashSet<u64>`.
- Reproduce the page-split `read` loop exactly, including "always true for len>0", read-through, and
  zero-fill. Reproduce the overriding `isReadable` with the overflow guard and real-deferral.
- `write` needs `&mut self` (patches cache) plus calls `m_real`'s write — see §6 on interior mutability.
- `update_pages` / `merge_pages` (insert-overwrite + retain) / `mark_permanent` / `is_permanent` /
  `clear_permanent` / `patch_pages` / accessors.

---

## 5. `ProviderRegistry` — every method

A process-global singleton (Meyers singleton, `instance()` returns a `static` local
`ProviderRegistry&`, `providerregistry.cpp:7-10`). Default ctor is private (`providerregistry.h:74`).
Stores `QList<ProviderInfo> m_providers` (ordered, allows duplicates only by skipping on
duplicate identifier — see register).

- **`static ProviderRegistry& instance()`** (`cpp:7`): lazy global singleton.
- **`void registerProvider(name, identifier, IProviderPlugin* plugin, dllFileName={})`** (`cpp:12-24`):
  Linear scan; if any existing entry has the same `identifier`, log `qWarning("...already
  registered: <id>")` and **return without adding** (first registration wins; idempotent-ish). Else
  append a plugin `ProviderInfo` and `qDebug` it. Does **not** take ownership of `plugin`.
- **`void registerBuiltinProvider(name, identifier, BuiltinFactory factory)`** (`cpp:26-37`): same
  duplicate-identifier guard, then append a builtin `ProviderInfo`.
- **`void unregisterProvider(const QString& identifier)`** (`cpp:39-48`): linear scan; remove the
  **first** entry whose `identifier` matches (`removeAt(i); return;`). If none found, `qWarning`
  ("Provider not found: <id>"). No-op-with-warning when absent.
- **`const QList<ProviderInfo>& providers() const`** (`providerregistry.h:59`): the full ordered list.
- **`const ProviderInfo* findProvider(const QString& identifier) const`** (`cpp:50-57`): linear scan
  for matching `identifier`; returns pointer to the entry or `nullptr`. (Pointer into the `QList`;
  valid until the list mutates.)
- **`void clear()`** (`cpp:59-61`): empties `m_providers`.
- **`static void populateSourceMenu(QMenu* menu, const QVector<SavedSourceDisplay>& savedSources={})`**
  (`cpp:63-115`): UI builder. **Pure Qt-UI; lowest port priority.** Algorithm:
  1. Static icon map `s_providerIcons` (identifier -> SVG resource path) for the four known plugin
     identifiers: `processmemory`->`server-process.svg`, `remoteprocessmemory`->`remote.svg`,
     `windbgmemory`->`debug.svg`, `reclass.netcompatlayer`->`plug.svg`.
  2. Add a **"File"** action first: icon `file-binary.svg`, `setIconVisibleInMenu(true)`,
     `setData("File")` (routing key for `selectSource`).
  3. For each registered provider (in list order): pick icon from the map or fall back to
     `extensions.svg`. Label = `prov.name` if `dllFileName` empty, else `"<name>  (<dll>)"`. Add
     action with that icon+label, force icon visible, `setData(prov.name)` (note: routing key is
     **`name`**, not identifier). If `prov.plugin` is non-null, call `prov.plugin->populatePluginMenu(menu)`
     to append plugin-specific actions (e.g. "Unload Driver").
  4. If `savedSources` non-empty: add a separator, then one **checkable** action per saved source
     (text = `savedSources[i].text`, checked = `.active`, `setData("#saved:<i>")`). Then another
     separator and a **"Clear All"** action (icon `clear-all.svg`, `setData("#clear")`).

  Routing-key strings consumed by `RcxController::selectSource`: `"File"`, the provider `name`,
  `"#saved:<i>"`, `"#clear"`. These are the same strings the tests pass directly
  (`test_source_management.cpp` uses `selectSource("#clear")`).

Rust mapping for the registry:
- A global singleton -> `once_cell::sync::Lazy<Mutex<ProviderRegistry>>` (or `OnceLock`). The C++ has
  no locking — registration happens at startup / plugin (un)load on the main thread — but Rust's
  `static mut`-free idiom is a `Mutex`. Keep behavior: duplicate-identifier register is a no-op +
  warn; unregister removes first match + warn-if-absent; ordered `Vec<ProviderInfo>`.
- `ProviderInfo` -> enum-ish struct: `name: String, identifier: String, kind: { Plugin(plugin ref) | Builtin(factory) }, dll_file_name: String`. Since plugins are out of scope, the `Plugin` variant can hold a placeholder/`Box<dyn ProviderPlugin>` trait object reserved for the future; built-in entries hold the factory closure.
- `find_provider(&id) -> Option<&ProviderInfo>`, `providers() -> &[ProviderInfo]`, `clear()`.
- `populate_source_menu` is UI; defer until a Rust UI layer exists. Preserve the routing-key
  contract (`"File"`, name, `"#saved:N"`, `"#clear"`) wherever the source picker is reimplemented.

---

## 6. Ownership, concurrency, threading

- **Provider ownership.** The document holds `std::shared_ptr<Provider> provider` (`controller.h:33`).
  Built-ins are created with `std::make_shared<NullProvider>()` / `std::make_shared<BufferProvider>(...)`
  (`controller.cpp:164,314,324,6401`). `SnapshotProvider` holds a `std::shared_ptr<Provider> m_real`.
  Plugins return `std::unique_ptr<rcx::Provider>` from `createProvider`, later adopted into a shared_ptr.
  Rust: `Arc<dyn Provider>` for the shared document handle; `SnapshotProvider.m_real: Option<Arc<dyn Provider>>`.
- **Mutation through a shared handle.** `write`/`writeBytes`/`SnapshotProvider::updatePages/mergePages/
  patchPages/markPermanent` mutate through what is logically a shared pointer. In C++ these are
  non-const methods on the pointee; the shared_ptr is non-const. In Rust, to keep an `Arc`-shared
  provider mutable you need **interior mutability**: e.g. `BufferProvider { data: Mutex<Vec<u8>> }`
  and `SnapshotProvider { pages: Mutex<HashMap<...>>, ... }`, with the trait's `write`/etc. taking
  `&self`. Alternatively redesign so the controller holds `Arc<Mutex<dyn Provider>>` — but that
  changes the read path's `&self` ergonomics. Recommend `&self` + per-impl interior mutability to
  mirror the C++ method constness (read is `const`/`&self`; write is non-const but the impls just
  lock internally).
- **Threading.** The page-cache design (snapshot) is the concurrency story: the **async refresh
  thread** reads pages from the real (possibly slow/blocking) provider and hands a finished
  `PageMap` to the **UI thread**, which composes purely from `SnapshotProvider` (no blocking I/O on
  the UI thread, no fallback to the real provider for cached pages). `updatePages`/`mergePages` are
  the handoff points. The `read` read-through to `m_real` on a cache miss is the only place the UI
  thread touches the real provider (and only for a few qwords for RTTI). The `Provider` classes
  themselves contain **no locks/threads**; synchronization lives in the controller's refresh
  machinery (out of scope for this subsystem doc). For the port: the `SnapshotProvider`'s `read` must
  be safe to call concurrently with the build thread's `update_pages` only if you add a lock —
  faithfully, the C++ relies on the refresh being marshalled to the UI thread (single-threaded
  access), so a `Mutex` around the page map is the conservative Rust choice.
- **Registry concurrency.** No locking in C++; treated as main-thread-only. Use a `Mutex`/`OnceLock`
  in Rust for soundness.

---

## 7. Qt-type -> Rust mapping (this subsystem)

| Qt / C++ type                       | Used for                              | Rust equivalent |
|-------------------------------------|---------------------------------------|-----------------|
| `QByteArray`                        | buffer bytes, page bytes, read result | `Vec<u8>` / `Box<[u8]>` / `[u8; 4096]` for pages |
| `QString` / `QStringLiteral(...)`   | names, kinds, identifiers, symbols    | `String` / `&'static str` |
| `QVector<T>`                        | regions/modules/threads lists         | `Vec<T>` |
| `QHash<uint64_t, QByteArray>`       | `SnapshotProvider` page map           | `HashMap<u64, [u8;4096]>` |
| `QSet<uint64_t>`                    | permanent pages                       | `HashSet<u64>` |
| `QList<ProviderInfo>`               | registry storage                      | `Vec<ProviderInfo>` |
| `std::shared_ptr<Provider>`         | shared provider handle                | `Arc<dyn Provider>` |
| `std::unique_ptr<Provider>`         | plugin-created provider (oos)         | `Box<dyn Provider>` |
| `std::function<bool(QWidget*, QString*)>` | `BuiltinFactory`                | `Box<dyn Fn(&mut String) -> bool>` (drop the `QWidget*` UI parent until UI exists) |
| `QFile` / `QFileInfo`               | `BufferProvider::fromFile`, `loadData`| `std::fs::read` + `Path::file_name()` |
| `Qt::Uninitialized` (QByteArray)    | `readBytes` buffer alloc              | `vec![0u8; len]` (skip the uninit micro-opt) |
| `Q_UNUSED(x)`                       | suppress unused-param warnings        | `let _ = x;` or `_`-prefixed params |
| `QMenu` / `QAction` / `QIcon`       | `populateSourceMenu` (UI)             | future UI layer; defer |
| `qDebug()` / `qWarning()`           | registry logging                      | `log::debug!` / `log::warn!` |
| `__declspec(dllexport)` / `visibility`| `RCX_PLUGIN_EXPORT` (oos)           | `#[no_mangle] pub extern "C"` behind `#[cfg]` if ever needed |

No floating-point endianness surprises: `readAs<T>` is a raw `memcpy`, native-endian. Use Rust
`*_from_ne_bytes` (or `bytemuck`) to match exactly; little-endian results in the tests are simply the
LE target's native order.

---

## 8. Serialization / data formats

This subsystem has **no on-disk serialization of its own**. Providers are runtime objects.
Adjacent persistence (the `.rcx` JSON's `SavedSourceEntry` — `controller.h:99`, with fields
`kind, displayName, filePath, providerTarget, baseAddress, baseAddressFormula`, and
`RcxDocument::pendingSavedSources` raw JSON) belongs to the document/serialization subsystem, not
here; it merely *names* which provider/identifier to re-attach. The only "format" inside providers
is the `SnapshotProvider` page granularity (4096-byte page-aligned `QHash`), which is purely
in-memory.

---

## 9. Subtle behaviors the tests rely on (must replicate exactly)

From `test_provider.cpp`:
1. `NullProvider`: `!isValid()`, `size()==0`; `read` returns `false` and **does not touch `buf`**
   (a `0xFF` byte stays `0xFF`); `readU8`->0; `readBytes(0,4)` -> 4 zero bytes; `isWritable()` false;
   `name()` empty; `getSymbol(...)` empty.
2. `BufferProvider`: empty buffer -> invalid (size 0); name defaults empty; `kind()=="File"`;
   ctor name preserved.
3. Typed reads are **little-endian / native** and work for arbitrary POD (`readAs<Pair>`).
4. `readBytes` past end -> zeroed buffer of requested length; zero-len -> empty.
5. `isReadable` boundary truth table (see §3.3): `(0,size)`, `(size-1,1)`, `(0,0)` true;
   `(0,size+1)`, `(size,1)`, `(big,1)` false; `(0,0)` true even on empty provider.
6. Writes are bounds-checked (past-end write fails, no mutation); write-then-read round-trips;
   `writeBytes` partial-region write works.
7. `fromFile`: nonexistent -> invalid/size 0 (no error thrown); valid -> size matches, bytes read
   back, `name()` is the **basename only**.
8. Polymorphism: holding a `unique_ptr<Provider>`, swapping Null->Buffer, all virtuals dispatch
   correctly; `readU64(0)` of bytes `DE AD BE EF CA FE BA BE` == `0xBEBAFECAEFBEADDE` (LE).
9. `getSymbol` always empty on base/buffer.

From `test_source_provider.cpp`:
10. Plugin identifier derivation: a plugin named "TestProcessMemory" registers under identifier
    `"testprocessmemory"` (the loader does `name.toLower().replace(" ","")`, `pluginmanager.cpp:112`).
    Tests register explicitly with id `"testprocessmemory"`.
11. `findProvider(id)` returns an entry with `name`, `identifier`, `dllFileName`, `isBuiltin==false`,
    non-null `plugin`. `unregisterProvider` then makes `findProvider` return null.
12. `dllFileName` propagates through `ProviderInfo` unchanged.
13. (Controller-level, context) `attachViaPlugin` must NOT overwrite a pre-set `tree.baseAddress`,
    while user-initiated `selectSource` does. Bytes read after attach come from the provider, not a
    PE header (`readU16(0) != 0x5A4D`). This constrains the *controller*, but the provider's job is
    just to return the plugin-supplied bytes faithfully.

From `test_source_management.cpp`:
14. Initial document `provider` is a `NullProvider` (size 0, invalid), no saved sources,
    `activeSourceIndex()==-1`.
15. `loadData(QByteArray)` -> `BufferProvider` (valid; size==bytes; reads back), and resets
    `tree.baseAddress=0`, clears undo stack, emits `documentChanged` (`controller.cpp:321-327`).
16. `loadData(path)` -> opens file read-only (silent no-op if it can't open), `BufferProvider` with
    basename, sets `dataPath=path`, base 0 (`controller.cpp:308-318`).
17. `clearSources()` -> resets provider to `NullProvider`, clears `dataPath`, clears saved sources,
    `activeSourceIdx=-1`, resets snapshot/value-history, refresh; **idempotent** (3x safe);
    `selectSource("#clear")` routes to it. After clear: `read` fails, `readU8`->0, `name()` empty.
18. `switchSource(-1)` / `switchSource(999)` are no-ops (invalid index).

---

## 10. Port checklist (this subsystem)

- [ ] `RegionType` enum (`#[repr(u8)]`, default `Private`), `MemoryRegion` (Default), `VtopResult`
      (Default), `ThreadInfo`, `ModuleEntry`.
- [ ] `Provider` trait: required `read(&self, addr:u64, buf:&mut [u8]) -> bool`, `size(&self)->u64`;
      default methods for `write`, `is_writable`, `name`, `is_live`, `kind`(="File"), `pointer_size`
      (=8), `base`, `get_symbol`, `symbol_to_address`, `enumerate_regions`, `peb`, `tebs`,
      `enumerate_modules`, kernel stubs. Provided helpers: `is_valid`, `is_readable` (overridable),
      `read_as<T>`, `read_u8/16/32/64`, `read_f32/64` (zero on failure), `read_bytes` (zero-fill on
      failure, empty for len<=0), `write_bytes`. Match the exact bounds math and failure-zeroing.
- [ ] `BufferProvider` (in-memory bytes + `from_file` that returns an empty provider on failure,
      name=basename; writes bounds-checked; single `Mapped` region or empty).
- [ ] `NullProvider` (size 0, read false, rest default).
- [ ] `SnapshotProvider` (page-split read with read-through + zero-fill; overriding `is_readable`
      with overflow guard + real-deferral; `size`=mainExtent; forwarders to `m_real`;
      `update_pages`/`merge_pages`/`patch_pages`/`mark_permanent`/`is_permanent`/`clear_permanent`/
      accessors; write-through to real + cache patch).
- [ ] `ProviderRegistry` global singleton (`Mutex`/`OnceLock`): `register_provider` /
      `register_builtin_provider` (dup-identifier -> warn+skip), `unregister_provider`
      (first-match remove, warn-if-absent), `find_provider`, `providers`, `clear`. `ProviderInfo`
      with name/identifier/(plugin|factory)/is_builtin/dll_file_name. Reserve a `ProviderPlugin`
      trait slot for the out-of-scope plugin variant.
- [ ] Defer `populate_source_menu` (Qt UI) but preserve routing-key strings: `"File"`, provider
      `name`, `"#saved:<i>"`, `"#clear"`.
- [ ] `RCX_PLUGIN_EXPORT` / C-ABI plugin loading: out of scope; if added later, gate behind
      `#[cfg]` per-OS so the Linux build keeps compiling.

---

## 11. Open questions / notes for the implementer

- `Provider::size()` is `int` in C++ (≤2 GB). The Rust port should use `u64`/`usize` but must
  reproduce the exact `isReadable` arithmetic so the same boundary cases pass; decide one width and
  keep it consistent (recommend `u64`).
- `len` is signed `int` everywhere; negative `len` is meaningful (`isReadable` returns false,
  `readBytes`/`SnapshotProvider::read` early-out). In Rust, `read` taking `&mut [u8]` makes `len` a
  `usize` (no negatives), which collapses the `len<0` branches — acceptable because no caller passes
  a negative length except as a guarded edge; document the divergence. `len==0` must stay "readable
  true / empty result".
- `write` mutating through an `Arc`-shared provider requires interior mutability in Rust (see §6).
  This is the main structural design decision for the trait.
- The `BuiltinFactory` and `IProviderPlugin` carry `QWidget*` parents and `QMenu*` — pure UI. The
  benign built-in path never needs them at the data layer; only the (out-of-scope) source-picker UI
  does. Model the factory minimally now.
