# PORTING SPEC — Data-source abstraction (`Provider` trait) + registry + built-in file/buffer/snapshot/null

Subsystem key: **`providers`**. Status: mostly-portable, no UI required for the data layer.

This is the function-level implementation spec that drives the faithful Rust port of the C++
`Provider` abstraction (`src/providers/*`, `src/providerregistry.*`, `src/iplugin.h` interface
signatures). It is authored from:
- behavioral map `_design/understand/providers.md`,
- C++ headers `provider.h`, `buffer_provider.h`, `null_provider.h`, `snapshot_provider.h`,
- `providerregistry.h` / `providerregistry.cpp`, `iplugin.h`,
- target layout `_design/ARCHITECTURE.md`, crates `_design/crate_selection.md`,
- oracle `_oracle/RESULTS.md` (`test_provider` 35/0/0 PASS; `test_source_provider` 11/0/0 PASS;
  `test_source_management` part of the controller lifecycle; `test_refresh_speedups` snapshot
  unit cases),
- the in-scope C++ tests `tests/test_provider.cpp`, `tests/test_source_provider.cpp`,
  `tests/test_source_management.cpp`, and the two pure snapshot unit tests inside
  `tests/test_refresh_speedups.cpp`.

> SCOPE: live process / kernel / remote / WinDbg sources are OUT OF SCOPE. Implement only
> `BufferProvider`, `NullProvider`, `SnapshotProvider`, and the `ProviderRegistry`. The plugin
> interface is reserved as an (unimplemented) Rust trait slot in the registry so the registry's
> shape matches the original; no plugin is ever instantiated by in-scope code.

---

## 0. Target crate / module(s)

One package `reclass`, module tree under `src/provider/` (per `ARCHITECTURE.md` §2 / §4):

```
src/provider/
├─ mod.rs        # pub: Provider trait + helper methods; RegionType, MemoryRegion, VtopResult,
│                #   ThreadInfo, ModuleEntry; re-exports of the four built-ins; ProviderError
├─ buffer.rs     # BufferProvider  (in-memory bytes + from_file)        ← buffer_provider.h
├─ null.rs       # NullProvider                                          ← null_provider.h
├─ snapshot.rs   # SnapshotProvider (page cache)                         ← snapshot_provider.h
├─ registry.rs   # ProviderRegistry singleton + ProviderInfo + ProviderPlugin trait slot
│                #                                                        ← providerregistry.*
└─ native.rs     # process/kernel/remote/windbg = documented STUBS (cfg/feature) — NOT this spec
```

- Feature gate: `provider` is **always** compiled (the table in `ARCHITECTURE.md` §3 lists it as
  `always`, deps `libloading` only for the future plugin entrypoint — not needed for in-scope code,
  so do **not** pull `libloading` yet). Everything here builds under `--no-default-features` so the
  logic tests run without gpui.
- Dependencies actually used by this subsystem: `bytemuck` (POD `read_as<T>` casts; already in
  `core`'s dep set), `thiserror` (typed error, optional — see §6), `tracing` (replace
  `qDebug`/`qWarning` in the registry), `ahash`/`std::collections` (snapshot maps). No `serde` here
  (this subsystem has no on-disk format of its own; see §7).
- `mod.rs` is declared from `lib.rs` as `pub mod provider;`.

Naming convention: C++ camelCase methods → Rust snake_case (`isReadable`→`is_readable`,
`enumerateRegions`→`enumerate_regions`, `pointerSize`→`pointer_size`, `getSymbol`→`get_symbol`,
`symbolToAddress`→`symbol_to_address`, `markPermanent`→`mark_permanent`, etc.). Types stay PascalCase.

---

## 1. Item-by-item C++ → Rust mapping

### 1.1 `enum class RegionType : uint8_t` (`provider.h:13-17`) → `RegionType`
```rust
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RegionType {
    Image = 0,    // loaded module: code + .rdata + .data
    Mapped = 1,   // memory-mapped file / shared section
    Private = 2,  // heap / stack / VirtualAlloc — mutable user data
}
impl Default for RegionType { fn default() -> Self { RegionType::Private } }
```
Behavior: pure tag. Default is **`Private`** (matches the C++ field default; the enum itself has no
default in C++ but `MemoryRegion::type` defaults to `Private`). No `serde` needed here.

### 1.2 `struct MemoryRegion` (`provider.h:19-29`) → `MemoryRegion`
```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,            // default 0
    pub size: u64,            // default 0
    pub readable: bool,       // C++ default TRUE — see note
    pub writable: bool,       // default false
    pub executable: bool,     // default false
    pub module_name: String,  // QString → String, default empty
    pub region_type: RegionType, // default Private
}
```
**Important divergence from `#[derive(Default)]`:** the C++ default for `readable` is `true`
(`provider.h:22`), but `#[derive(Default)]` makes `bool` default `false`. So do **not** rely on the
derive for `readable`. Either hand-implement `Default` setting `readable=true`, or always construct
explicitly. The only constructors in scope (`BufferProvider::enumerate_regions`) set `readable=true`
explicitly, so the derive would be harmless there — but to stay faithful, hand-write `Default`:
```rust
impl Default for MemoryRegion {
    fn default() -> Self {
        Self { base: 0, size: 0, readable: true, writable: false, executable: false,
               module_name: String::new(), region_type: RegionType::Private }
    }
}
```
(The C++ comment about field ordering for positional initializers is moot in Rust — use named
fields. Document it but do not preserve the ordering.)

### 1.3 `struct VtopResult` (`provider.h:31-36`) → `VtopResult`
Out-of-scope kernel result type; must exist because `translate_address` returns it.
```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VtopResult {
    pub physical: u64,
    pub pml4e: u64, pub pdpte: u64, pub pde: u64, pub pte: u64,
    pub page_size: u8,  // 0=4KB, 1=2MB, 2=1GB
    pub valid: bool,    // default false
}
```
`#[derive(Default)]` is correct here (all-zero / false matches C++). Only kernel providers ever
return a non-default value; built-ins never do.

### 1.4 `Provider::ThreadInfo` (`provider.h:99`) → `ThreadInfo`
```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThreadInfo { pub teb_address: u64, pub thread_id: u32 }
```
Returned by `tebs()`. Built-ins return empty `Vec`.

### 1.5 `Provider::ModuleEntry` (`provider.h:102`) → `ModuleEntry`
```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleEntry { pub name: String, pub full_path: String, pub base: u64, pub size: u64 }
```
Returned by `enumerate_modules()`. Built-ins (Buffer/Null) return empty; `SnapshotProvider`
forwards to its real provider (used by RTTI's `find_owning_module`).

### 1.6 `class Provider` (`provider.h:38`) → `trait Provider`
See §2 for the full trait. Held behind `Arc<dyn Provider>` for the shared document handle
(replaces `std::shared_ptr<Provider>`); see §5.

### 1.7 `class BufferProvider` (`buffer_provider.h`) → `struct BufferProvider`
See §3. Fields `m_data: QByteArray`, `m_name: QString` → `data: Mutex<Vec<u8>>`, `name: String`
(the `Mutex` is for interior-mutable `write`; see §5).

### 1.8 `class NullProvider` (`null_provider.h`) → `struct NullProvider`
See §3. Zero-field unit struct: `pub struct NullProvider;`.

### 1.9 `class SnapshotProvider` (`snapshot_provider.h`) → `struct SnapshotProvider`
See §4. The most behaviorally intricate type in scope.

### 1.10 `class ProviderRegistry` + `ProviderInfo` (`providerregistry.*`) → §8.

### 1.11 `SavedSourceDisplay` (`providerregistry.h:13-16`) → `SavedSourceDisplay`
UI helper used only by `populate_source_menu` (deferred to UI). Define the struct for completeness:
```rust
#[derive(Clone, Debug, Default)]
pub struct SavedSourceDisplay { pub text: String, pub active: bool }
```

### 1.12 Plugin interfaces (`iplugin.h`) — reserved trait slot, IMPLEMENTATIONS OUT OF SCOPE
The registry must reference a plugin object. Model `IProviderPlugin` as a Rust trait `ProviderPlugin`
that the (out-of-scope) plugins would implement, but **never implement it in-scope**. Only the method
signatures the registry / source-picker contract touch are needed:
```rust
pub trait ProviderPlugin: Send + Sync {
    fn name(&self) -> String;
    fn version(&self) -> String;
    fn author(&self) -> String;
    fn description(&self) -> String;
    fn load_type(&self) -> LoadType { LoadType::Auto }   // k_ELoadTypeAuto / k_ELoadTypeManual
    fn can_handle(&self, target: &str) -> bool;
    fn create_provider(&self, target: &str) -> Result<Box<dyn Provider>, ProviderError>; // the only
                                                                                          // place a live
                                                                                          // Provider is born
    fn initial_base_address(&self, _target: &str) -> u64 { 0 }
    // selectTarget / enumerateProcesses / providesProcessList / populatePluginMenu / Icon are pure
    // UI/process concerns — omit until the source-picker UI exists.
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum LoadType { Auto, Manual }
```
- `RCX_PLUGIN_EXPORT` / `CreatePluginFunc` / `IPLUGIN_IID` (`iplugin.h:7-11,143,145`): C-ABI plugin
  loader plumbing — **out of scope**. If ever added, gate behind `#[cfg]` per-OS (`#[no_mangle]
  pub extern "C"` + `libloading`) so the Linux build keeps compiling. Do not add now.
- `PluginProcessInfo` (`iplugin.h:65`): process-picker row — UI; omit until process-picker UI exists.

---

## 2. The `Provider` trait — every method (signature + behavior + crate)

`addr` is always an **absolute** `u64`; built-in file/buffer treat it as a 0-based offset into the
bytes. In C++ `len` is signed `int`; in Rust the byte buffer carries its own length so most `len`
arguments disappear (see the `len`-sign note in §10).

```rust
pub trait Provider: Send + Sync {
    // ── Required (pure virtuals) ─────────────────────────────────────────
    /// Read exactly buf.len() bytes at `addr` into `buf`. Returns true on FULL success,
    /// false on any failure. On false the contents of `buf` are left UNTOUCHED (caller-visible
    /// — e.g. NullProvider leaves a 0xFF byte at 0xFF). C++: read(addr, void*, int).
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool;

    /// Logical size in bytes. 0 == empty/no source. C++ returns int (≤ INT_MAX); we use u64 and
    /// reproduce the exact bounds math (§ is_readable). C++: size().
    fn size(&self) -> u64;

    // ── Optional overrides (defaults shown) ──────────────────────────────
    /// Write buf.len() bytes at addr. Default false (read-only). NON-const in C++ (mutates);
    /// in Rust takes &self + interior mutability (see §5). C++: write(addr, const void*, int).
    fn write(&self, _addr: u64, _buf: &[u8]) -> bool { false }
    fn is_writable(&self) -> bool { false }                          // C++ :51
    fn name(&self) -> String { String::new() }                      // C++ :55, empty => "<Select Source>"
    fn is_live(&self) -> bool { false }                             // C++ :59, drives auto-refresh
    fn kind(&self) -> String { "File".to_string() }                // C++ :63 — default is "File", NOT empty
    fn pointer_size(&self) -> u32 { 8 }                            // C++ :67, 4=32-bit / 8=64-bit
    fn base(&self) -> u64 { 0 }                                    // C++ :72, initial base; always 0 for file/buffer
    fn get_symbol(&self, _addr: u64) -> String { String::new() }   // C++ :78, "ntdll.dll+0x1A30"; "" if unknown
    fn symbol_to_address(&self, _name: &str) -> u64 { 0 }          // C++ :85, reverse; 0 if not found
    fn enumerate_regions(&self) -> Vec<MemoryRegion> { Vec::new() } // C++ :93, empty => scanner uses [0,size())
    fn peb(&self) -> u64 { 0 }                                     // C++ :97, live-process only
    fn tebs(&self) -> Vec<ThreadInfo> { Vec::new() }               // C++ :100
    fn enumerate_modules(&self) -> Vec<ModuleEntry> { Vec::new() } // C++ :103
    // Kernel paging — override only in kernel providers (out of scope):
    fn has_kernel_paging(&self) -> bool { false }                  // C++ :106
    fn get_cr3(&self) -> u64 { 0 }                                 // C++ :107
    fn translate_address(&self, _va: u64) -> VtopResult { VtopResult::default() } // C++ :108
    fn read_page_table(&self, _phys: u64, _start_idx: i32, _count: i32) -> Vec<u64> { Vec::new() } // C++ :111 (defaults start=0,count=512 at call sites)

    // ── Derived convenience (provided; "is_readable" overridable, rest final) ──
    /// C++ :120 — NON-virtual. Empty source is invalid.
    fn is_valid(&self) -> bool { self.size() > 0 }

    /// C++ :122-126 — VIRTUAL (SnapshotProvider overrides). Underflow-safe bounds check.
    fn is_readable(&self, addr: u64, len: u64) -> bool {
        // len==0 → true (always readable); see §10 for the len<0 collapse.
        if len == 0 { return true; }
        addr <= self.size() && len <= self.size() - addr
    }

    /// C++ :128-133 — raw, native-endian reinterpret of size_of::<T>() bytes; returns a
    /// zero-initialized T on read failure (read()'s bool is ignored). bytemuck::Pod bound.
    fn read_as<T: bytemuck::Pod>(&self, addr: u64) -> T {
        let mut v = T::zeroed();
        self.read(addr, bytemuck::bytes_of_mut(&mut v));
        v   // zeroed on failure
    }
    fn read_u8 (&self, a: u64) -> u8  { self.read_as::<u8 >(a) }    // C++ :135
    fn read_u16(&self, a: u64) -> u16 { self.read_as::<u16>(a) }    // C++ :136
    fn read_u32(&self, a: u64) -> u32 { self.read_as::<u32>(a) }    // C++ :137
    fn read_u64(&self, a: u64) -> u64 { self.read_as::<u64>(a) }    // C++ :138
    fn read_f32(&self, a: u64) -> f32 { self.read_as::<f32>(a) }    // C++ :139
    fn read_f64(&self, a: u64) -> f64 { self.read_as::<f64>(a) }    // C++ :140

    /// C++ :142-148 — exactly `len` bytes for len>0; ZERO-FILLED on any read failure
    /// (length preserved, contents 0); EMPTY for len<=0 (len==0 in Rust).
    fn read_bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        if len == 0 { return Vec::new(); }
        let mut buf = vec![0u8; len];
        if !self.read(addr, &mut buf) { buf.iter_mut().for_each(|b| *b = 0); } // explicit zero-fill
        buf
    }

    /// C++ :150-152 — calls write; NON-const. (Interior mutability via &self.)
    fn write_bytes(&self, addr: u64, data: &[u8]) -> bool { self.write(addr, data) }
}
```

Notes / pinned behaviors:
- `read` returning `false` must NOT touch `buf` (test `nullProvider_readFails`: a `0xFF` byte stays
  `0xFF`). The provided `read_u8`/`read_bytes` helpers compensate by zeroing on failure.
- `read_as<T>` uses **native** endianness via `bytemuck` (raw memcpy reinterpret), matching the C++
  `T v{}; read(&v); return v;`. On the LE dev/test targets bytes `34 12` → `0x1234`
  (`buffer_readU16_littleEndian`). The `Pod` bound covers `u8/u16/u32/u64/f32/f64` and arbitrary
  `#[repr(C)] Pod` structs (`buffer_readAs_customStruct` → a `#[repr(C)] struct Pair { a: u16, b: u16 }`
  marked `#[derive(Pod, Zeroable)]`).
- `is_readable` is the only "convenience" method that is virtual/overridable in C++; keep it a normal
  trait method with a default body so `SnapshotProvider` can override it.
- All defaults match the C++ defaults exactly; in particular `kind()` defaults to **"File"** (not
  empty), `pointer_size()` to **8**, `base()` to **0**.

---

## 3. Built-in: `BufferProvider` and `NullProvider`

### 3.1 `BufferProvider` (`buffer_provider.h`) → `src/provider/buffer.rs`
Backs reads/writes with owned bytes; fully bounds-checked; fully writable; fixed size (writes never
grow it).

```rust
pub struct BufferProvider {
    data: Mutex<Vec<u8>>,   // QByteArray m_data — Mutex for interior-mutable write (see §5)
    name: String,           // QString m_name (display name / basename / empty)
}
```

Constructors / methods:
- **`pub fn new(data: Vec<u8>, name: impl Into<String>) -> Self`** ← C++ ctor `:13`
  `BufferProvider(QByteArray, name={})`. `name` defaults via a second ctor / `Default`: provide both
  `BufferProvider::new(data, name)` and `BufferProvider::with_data(data)` (= `new(data, "")`).
- **`pub fn from_file(path: impl AsRef<Path>) -> Self`** ← C++ `fromFile` `:17-22`.
  ```
  match std::fs::read(path) {
      Ok(bytes) => BufferProvider::new(bytes, path.file_name()...),  // name = basename only
      Err(_)    => BufferProvider::new(Vec::new(), ""),              // empty/invalid; NO error propagated
  }
  ```
  Name = `path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string()` (basename, no dir).
  **Never returns Result / never panics**; failure == an invalid (size 0, empty-name) provider, to
  match `buffer_fromFile_nonexistent`. Uses `std::fs::read` (crate_selection: file source = `std::fs`;
  do not pull `memmap2` for this benign path — that note is for the PDB importer).
- **`fn size(&self) -> u64`** ← `:24` — `self.data.lock().len() as u64`.
- **`fn read(&self, addr, buf) -> bool`** ← `:26-30`:
  ```
  let len = buf.len() as u64;
  if !self.is_readable(addr, len) { return false; }   // base is_readable vs size()
  let d = self.data.lock();
  buf.copy_from_slice(&d[addr as usize .. addr as usize + buf.len()]);
  true
  ```
  Partial out-of-range fails entirely (no partial copy), leaving `buf` untouched.
- **`fn is_writable(&self) -> bool { true }`** ← `:32`.
- **`fn write(&self, addr, buf) -> bool`** ← `:34-38`:
  ```
  let len = buf.len() as u64;
  if !self.is_readable(addr, len) { return false; }   // SAME bounds check as read
  let mut d = self.data.lock();
  d[addr as usize .. addr as usize + buf.len()].copy_from_slice(buf);
  true
  ```
  Past-end write fails and mutates nothing (`buffer_write_pastEndFails`). Fixed-size buffer.
- **`fn name(&self) -> String`** ← `:40` — `self.name.clone()`.
- **`fn kind(&self) -> String`** ← `:41` — `"File"` (same as default; override anyway for fidelity).
- **`fn enumerate_regions(&self) -> Vec<MemoryRegion>`** ← `:48-59`:
  ```
  let d = self.data.lock();
  if d.is_empty() { return Vec::new(); }
  vec![ MemoryRegion {
      base: 0, size: d.len() as u64, readable: true, writable: true, executable: false,
      module_name: if self.name.is_empty() { "[buffer]".into() } else { self.name.clone() },
      region_type: RegionType::Mapped,
  } ]
  ```
  Single synthetic region; `module_name` is the scanner's Module-column source-of-truth.
- **`pub fn data(&self) -> MutexGuard<Vec<u8>>`** / a read accessor ← `:61-62` `data()` const+mut.
  In Rust expose `pub fn data(&self) -> impl Deref<Target=[u8]>` (lock guard) for read, and rely on
  `write`/`write_bytes` for mutation, or expose `pub fn data_mut(&self) -> MutexGuard<Vec<u8>>` for
  direct manipulation parity. (Used elsewhere for direct buffer manipulation; keep an accessor.)

### 3.2 `NullProvider` (`null_provider.h`) → `src/provider/null.rs`
The "no source attached" placeholder. Controller initializes `provider` to a `NullProvider`
and `clear_sources()` resets to one.
```rust
pub struct NullProvider;
impl Provider for NullProvider {
    fn read(&self, _addr: u64, _buf: &mut [u8]) -> bool { false }  // always fails; leaves buf untouched
    fn size(&self) -> u64 { 0 }                                    // => is_valid() == false
    // everything else inherited: name()="", kind()="File", is_writable()=false,
    // get_symbol()="", enumerate_regions()=[], base()=0, pointer_size()=8, ...
}
```
Through `read_u8`/`read_bytes` the caller sees zeros (those helpers zero on failure), but a raw
`read` returns `false` and does not write `buf` (`nullProvider_readFails`).

---

## 4. Built-in: `SnapshotProvider` (`snapshot_provider.h`) → `src/provider/snapshot.rs`

A page-cache front over a real provider so the UI thread composes from a frozen page table with no
blocking I/O. In scope as a built-in; `m_real` may wrap any provider (Buffer/Null/out-of-scope live).

```rust
pub const PAGE_SIZE: u64 = 4096;            // kPageSize
pub const PAGE_MASK: u64 = !(PAGE_SIZE - 1); // kPageMask = 0xFFFF_FFFF_FFFF_F000

pub type PageMap = HashMap<u64, Box<[u8]>>;  // page-aligned addr → exactly-4096-byte page
                                             // (Box<[u8]> not [u8;4096] to avoid 4KB inline values;
                                             //  invariant: every value .len()==4096)

pub struct SnapshotProvider {
    real: Option<Arc<dyn Provider>>,          // std::shared_ptr<Provider> m_real (may be null → None)
    inner: Mutex<SnapshotInner>,              // page table + extent are mutated by update/merge/patch
    permanent_pages: Mutex<HashSet<u64>>,     // m_permanentPages (page-aligned addrs)
}
struct SnapshotInner {
    pages: PageMap,                           // m_pages
    main_extent: u64,                         // m_mainExtent (C++ int; use u64, see §10)
}
```

**Interior mutability:** `update_pages`/`merge_pages`/`patch_pages` and `write` mutate `pages`;
`mark_permanent`/`clear_permanent` mutate `permanent_pages`. The C++ does these on the UI thread
(single-threaded). In Rust, hold them behind `Mutex` so `&self` methods can mutate, matching the
`Arc<dyn Provider>` shared-handle model (see §5). `read`/`is_readable` lock `inner` for the duration.

### 4.1 Constructor ← C++ ctor `:35`
`pub fn new(real: Option<Arc<dyn Provider>>, pages: PageMap, main_extent: u64) -> Self` — moves the
three into fields (`inner = SnapshotInner { pages, main_extent }`, `permanent_pages` empty).
NOTE the test constructs `SnapshotProvider(/*real=*/{}, {}, 0)` → in Rust
`SnapshotProvider::new(None, PageMap::new(), 0)`.

### 4.2 `read` ← C++ `:40-72` — CORE ALGORITHM (page-split, read-through, zero-fill, always-true)
```
fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
    if buf.is_empty() { return false; }       // C++: len<=0 => false. (DIFFERS from base read_bytes,
                                              //  which treats len==0 as empty/ok. Keep this asymmetry.)
    let inner = self.inner.lock();
    let mut cur = addr;
    let mut off = 0usize;                     // index into buf
    let mut remaining = buf.len();
    while remaining > 0 {
        let page_addr = cur & PAGE_MASK;
        let page_off  = (cur - page_addr) as usize;            // 0..4095
        let chunk     = remaining.min(PAGE_SIZE as usize - page_off);
        match inner.pages.get(&page_addr) {
            Some(page) => buf[off..off+chunk]
                              .copy_from_slice(&page[page_off..page_off+chunk]),  // cache hit
            None => match &self.real {
                Some(r) => {
                    if !r.read(cur, &mut buf[off..off+chunk]) {                   // read-through
                        buf[off..off+chunk].iter_mut().for_each(|b| *b = 0);      // zero on real failure
                    }
                }
                None => buf[off..off+chunk].iter_mut().for_each(|b| *b = 0),      // no cache, no real → zeros
            },
        }
        off += chunk; cur += chunk; remaining -= chunk;
    }
    true   // ALWAYS true for non-empty buf
}
```
Pinned behaviors (must replicate exactly):
- Splits the read at 4096-byte page boundaries; each page served independently.
- Cache hit → copy from cached page. Miss + real present → read-through to `real` (zero that chunk
  if the real read fails). Miss + no real → zeros.
- **Always returns `true`** for non-empty buf (so `read_bytes`/`read_as` through a snapshot never see
  the failure path — they get the possibly-zero bytes). Contrast `BufferProvider::read`.
- Read-through exists for the auto-RTTI hint (`walkRttiItanium` peeking `vtable[-8]`/type_info bytes
  in `.rdata` not pre-fetched for collapsed pointers).
- `snapshotProviderMergeKeepsExisting` reads `0x0000` (→ `0xAA`) and `0x1000` (→ `0xCC`) through this
  path; with `real=None` and both pages present it is a pure cache-hit copy.

### 4.3 `is_readable` ← C++ `:74-91` — OVERRIDES base (overflow guard + real-deferral)
```
fn is_readable(&self, addr: u64, len: u64) -> bool {
    if len == 0 { return true; }              // same len==0 => true as base (see §10 for len<0 collapse)
    let end = addr.wrapping_add(len);
    if end < addr { return false; }           // overflow guard (C++ checks end < addr)
    let inner = self.inner.lock();
    let mut p = addr & PAGE_MASK;
    while p < end {
        if !inner.pages.contains_key(&p) {
            // first uncached page → defer to real's bounds check
            if let Some(r) = &self.real { if r.is_readable(addr, len) { return true; } }
            return false;
        }
        p += PAGE_SIZE;
    }
    true
}
```
All touched pages cached → readable. First uncached page → defer to `real.is_readable(addr,len)`
(a process provider returns true while its handle is open → enables read-through); no real or real
says no → false. Use `wrapping_add` to mimic the C++ `addr + len` overflow then the `end < addr`
guard (avoid a Rust debug-overflow panic).

### 4.4 Forwarders / scalars ← C++ `:93-120`
```
fn size(&self) -> u64               { self.inner.lock().main_extent }              // :93 — independent of #pages cached
fn is_writable(&self) -> bool       { self.real.as_ref().map_or(false, |r| r.is_writable()) }  // :94
fn is_live(&self) -> bool           { self.real.as_ref().map_or(false, |r| r.is_live()) }      // :95
fn name(&self) -> String            { self.real.as_ref().map_or(String::new(), |r| r.name()) } // :96
fn kind(&self) -> String            { self.real.as_ref().map_or("File".into(), |r| r.kind()) } // :97 — "File" if none
fn pointer_size(&self) -> u32       { self.real.as_ref().map_or(8, |r| r.pointer_size()) }     // :98
fn base(&self) -> u64               { self.real.as_ref().map_or(0, |r| r.base()) }             // :99
fn get_symbol(&self, a: u64) -> String { self.real.as_ref().map_or(String::new(), |r| r.get_symbol(a)) } // :100
fn symbol_to_address(&self, n: &str) -> u64 { self.real.as_ref().map_or(0, |r| r.symbol_to_address(n)) } // :103
fn enumerate_modules(&self) -> Vec<ModuleEntry> { self.real.as_ref().map_or_else(Vec::new, |r| r.enumerate_modules()) } // :111 — REQUIRED for RTTI find_owning_module
fn enumerate_regions(&self) -> Vec<MemoryRegion> { self.real.as_ref().map_or_else(Vec::new, |r| r.enumerate_regions()) } // :114
fn peb(&self) -> u64                { self.real.as_ref().map_or(0, |r| r.peb()) }              // :117
fn tebs(&self) -> Vec<ThreadInfo>   { self.real.as_ref().map_or_else(Vec::new, |r| r.tebs()) } // :118
```

### 4.5 `write` ← C++ `:122-127` (write-through to real + patch cache)
```
fn write(&self, addr: u64, buf: &[u8]) -> bool {
    let Some(r) = &self.real else { return false; };
    let ok = r.write(addr, buf);
    if ok { self.patch_pages(addr, buf); }     // reflect new bytes in the snapshot immediately
    ok
}
```

### 4.6 Snapshot-specific inherent methods (not on the trait)
- **`pub fn update_pages(&self, pages: PageMap, main_extent: u64)`** ← `:130-133`: wholesale replace
  `inner.pages` and `inner.main_extent`. Called after a full async read.
- **`pub fn merge_pages(&self, fresh: &PageMap, main_extent: u64)`** ← `:139-143`: for each entry in
  `fresh`, `inner.pages.insert(key, clone)` (overwrite-if-present); existing pages not in `fresh`
  survive; set `main_extent`. Used by per-tick refresh once page-skipping began.
  (`snapshotProviderMergeKeepsExisting`: merging `0x1000→0xCC` over an initial `{0x0000→0xAA,
  0x1000→0xBB}` leaves `0x0000→0xAA`, updates `0x1000→0xCC`.)
- **`pub fn mark_permanent(&self, page_addr: u64)`** ← `:148`:
  `self.permanent_pages.lock().insert(page_addr & PAGE_MASK)` (page-aligns input).
- **`pub fn is_permanent(&self, page_addr: u64) -> bool`** ← `:151`:
  `self.permanent_pages.lock().contains(&(page_addr & PAGE_MASK))`.
- **`pub fn clear_permanent(&self)`** ← `:154`: `self.permanent_pages.lock().clear()`.
- **`pub fn patch_pages(&self, addr: u64, buf: &[u8])`** ← `:157-173`: same page-split loop as `read`,
  but **only patches pages that already exist** in `inner.pages` (missing pages skipped; `off`/`cur`/
  `remaining` still advance). No `len<=0` guard (empty buf → loop never runs → harmless no-op). Used
  after a user value write and internally by `write`.
- **`pub fn pages(&self) -> MutexGuard<PageMap>`** (or a cloned snapshot) ← `:175` accessor.
- **`pub fn permanent_pages(&self) -> MutexGuard<HashSet<u64>>`** ← `:176` accessor.
  (C++ returns `const&`; in Rust return a lock guard or a clone — the controller only iterates them.)

### 4.7 Pseudocode for `patch_pages` (the tricky existing-page-only loop)
```
fn patch_pages(&self, addr: u64, buf: &[u8]) {
    let mut inner = self.inner.lock();
    let mut cur = addr;
    let mut off = 0usize;
    let mut remaining = buf.len();
    while remaining > 0 {
        let page_addr = cur & PAGE_MASK;
        let page_off  = (cur - page_addr) as usize;
        let chunk     = remaining.min(PAGE_SIZE as usize - page_off);
        if let Some(page) = inner.pages.get_mut(&page_addr) {
            page[page_off..page_off+chunk].copy_from_slice(&buf[off..off+chunk]); // existing pages only
        }
        // missing page → skip, but still advance:
        off += chunk; cur += chunk; remaining -= chunk;
    }
}
```

---

## 5. Ownership, interior mutability, concurrency (§6 of the behavioral map)

- **Shared handle:** the document holds `Arc<dyn Provider>` (replaces `std::shared_ptr<Provider>
  provider`). Built-ins constructed via `Arc::new(NullProvider)`, `Arc::new(BufferProvider::new(..))`,
  `Arc::new(SnapshotProvider::new(..))`. `SnapshotProvider.real: Option<Arc<dyn Provider>>`. The
  (out-of-scope) plugin path would return `Box<dyn Provider>` from `create_provider`, adopted into an
  `Arc`.
- **Mutation through a shared handle:** `write`/`write_bytes` and the snapshot's
  `update_pages`/`merge_pages`/`patch_pages`/`mark_permanent` all mutate through what is logically a
  shared pointer. C++ uses non-const methods on a non-const pointee. The Rust trait keeps `write`
  (and the helpers) as **`&self`**; the concrete impls use **interior mutability**:
  `BufferProvider { data: Mutex<Vec<u8>> }`, `SnapshotProvider { inner: Mutex<…>, permanent_pages:
  Mutex<…> }`. This preserves the `&self` read ergonomics and the `Arc`-shared model. (Rationale: the
  alternative `Arc<Mutex<dyn Provider>>` would force a lock on every read; rejected.)
- **`Provider: Send + Sync`** bound: required because `Arc<dyn Provider>` crosses the
  controller/refresh-thread boundary. `Mutex` (use `parking_lot::Mutex` if already a dep, else
  `std::sync::Mutex` — the map is uncontended in practice) gives `Sync`. `NullProvider` is trivially
  `Send + Sync`.
- **Threading model (snapshot):** an async refresh thread reads pages from the (possibly slow) real
  provider and hands a finished `PageMap` to the UI thread via `update_pages`/`merge_pages`; the UI
  thread composes purely from the cached pages (the only real-provider touch on the UI thread is the
  `read` read-through on a cache miss, a few qwords for RTTI). The C++ relies on this being marshalled
  to a single thread; the `Mutex` in Rust makes concurrent `update_pages` + `read` sound regardless.
- **Registry concurrency:** no locking in C++ (main-thread-only). Rust uses a `OnceLock`/`Mutex`
  for soundness (§8).
- **Lock-ordering note:** `read`/`is_readable`/`size` lock only `inner`; `mark_permanent`/
  `is_permanent`/`clear_permanent` lock only `permanent_pages`; `write` locks neither directly but
  calls `patch_pages` (locks `inner`) after `real.write`. No method holds both locks at once → no
  deadlock. Do not call back into `real` while holding `inner` except in `read`'s read-through (where
  `real` is a different object and never re-enters this snapshot).

---

## 6. Error-handling strategy

The C++ data layer is **error-code / sentinel based, never throwing**:
- `read`/`write`/`is_readable`/`is_writable` → `bool`.
- `read_as`/`read_u*`/`read_f*` → zero on failure.
- `read_bytes` → zero-filled buffer (len>0) or empty (len==0) on failure.
- `from_file` failure → an invalid (size 0) provider, **no error surfaced**.

Port faithfully: **keep the boolean/sentinel returns; do NOT introduce `Result` into the
`Provider` trait read/write path.** This is required for parity (callers branch on `false`/zero, and
`from_file` parity demands swallowing the I/O error). Specifically:
- `BufferProvider::from_file` swallows the `std::fs::read` error and returns an empty provider.
- `Provider::read`/`write` return `bool`; do not propagate `io::Error`.

Define a small `ProviderError` (thiserror) used **only** by the out-of-scope `ProviderPlugin::
create_provider` slot (the one place C++ has an out-param `QString* errorMsg`), so the registry's
plugin signature is faithful, but no in-scope code constructs or matches it:
```rust
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")] Message(String),
    #[error("provider not found: {0}")] NotFound(String),  // used by registry lookups if needed
}
```
Logging: replace `qDebug`/`qWarning` in the registry with `tracing::debug!`/`tracing::warn!`
(crate_selection: tracing). The exact log text need not match (not oracle-checked), but the
**behavior** the warnings accompany must (duplicate-register no-op; unregister-absent no-op).

---

## 7. Serialization / data formats

This subsystem has **no on-disk serialization of its own** — providers are runtime objects. So
**no `serde` derives** on `Provider` types. The adjacent `.rcx` `SavedSourceEntry`
(`kind/displayName/filePath/providerTarget/baseAddress/baseAddressFormula`) belongs to the
core/serialization subsystem and merely *names* which provider/identifier to re-attach. The only
"format" here is the `SnapshotProvider` page granularity (4096-byte page-aligned map), purely
in-memory. Keep `PAGE_SIZE`/`PAGE_MASK` as the only constants.

---

## 8. `ProviderRegistry` (`providerregistry.*`) → `src/provider/registry.rs`

Process-global singleton (Meyers singleton in C++; `instance()` returns `static ProviderRegistry&`).
Stores an **insertion-ordered** `Vec<ProviderInfo>` of descriptors (NOT live providers). Built-in
entries hold a factory closure; plugin entries hold a borrowed plugin reference.

### 8.1 `ProviderInfo` ← `providerregistry.h:29-44`
The two C++ constructors discriminate plugin vs builtin; model as an enum payload:
```rust
pub type BuiltinFactory = Box<dyn Fn(&mut String) -> bool + Send + Sync>;
//  ← std::function<bool(QWidget* parent, QString* target)>; drop the QWidget* UI parent
//    (headless): writes the chosen target into the &mut String, returns true if committed.

pub enum ProviderKind {
    Builtin(BuiltinFactory),               // isBuiltin == true
    Plugin(Arc<dyn ProviderPlugin>),       // isBuiltin == false; "borrowed" → use Arc (see note)
}

pub struct ProviderInfo {
    pub name: String,            // display name + routing key for selectSource (menu data == name)
    pub identifier: String,      // unique ID; lookup key for find_provider
    pub kind: ProviderKind,      // discriminator (== C++ isBuiltin + plugin/factory)
    pub dll_file_name: String,   // original DLL/SO filename (plugin only); empty for builtins
}
impl ProviderInfo {
    pub fn is_builtin(&self) -> bool { matches!(self.kind, ProviderKind::Builtin(_)) }
    pub fn plugin(&self) -> Option<&Arc<dyn ProviderPlugin>> {
        if let ProviderKind::Plugin(p) = &self.kind { Some(p) } else { None }
    }
}
```
- C++ plugin ctor `:37` `(name, id, IProviderPlugin* p, dll={})` → `register_provider(name, id, plugin,
  dll)` builds `ProviderInfo{ name, identifier, kind: Plugin(plugin), dll_file_name: dll }`.
- C++ builtin ctor `:42` `(name, id, BuiltinFactory f)` → `register_builtin_provider(name, id, f)`
  builds `ProviderInfo{ name, identifier, kind: Builtin(f), dll_file_name: String::new() }`.
- **Ownership note:** C++ stores a borrowed `IProviderPlugin*` (NOT owned — the plugin lives in the
  PluginManager). In Rust use `Arc<dyn ProviderPlugin>` (cheap shared, lifetime-safe) — since plugins
  are out of scope and never instantiated in-scope, this slot is never exercised by the ported tests
  except via a test-double (see §9 test plan). `BuiltinFactory` is `Send + Sync` so `ProviderInfo`
  (hence the registry) is `Send` for the `Mutex`.

### 8.2 `SavedSourceDisplay` ← `providerregistry.h:13-16` (defined in §1.11). UI-only.

### 8.3 Registry singleton + methods
```rust
pub struct ProviderRegistry { providers: Vec<ProviderInfo> }  // m_providers (ordered)

static REGISTRY: OnceLock<Mutex<ProviderRegistry>> = OnceLock::new();
impl ProviderRegistry {
    fn global() -> &'static Mutex<ProviderRegistry> {                         // ← instance() cpp:7
        REGISTRY.get_or_init(|| Mutex::new(ProviderRegistry { providers: Vec::new() }))
    }
    // all public ops are static-style: ProviderRegistry::register_provider(...), locking global().
}
```
Method behaviors (lock `global()` for each):
- **`register_provider(name, identifier, plugin: Arc<dyn ProviderPlugin>, dll_file_name)`**
  ← cpp:12-24: linear scan; if any existing entry has the same `identifier`,
  `tracing::warn!("Provider already registered: {identifier}")` and **return without adding**
  (first registration wins). Else push a `Plugin` `ProviderInfo`, `tracing::debug!`.
- **`register_builtin_provider(name, identifier, factory)`** ← cpp:26-37: same dup-identifier guard,
  then push a `Builtin` `ProviderInfo`.
- **`unregister_provider(identifier)`** ← cpp:39-48: linear scan; remove the **first** entry whose
  `identifier` matches (`providers.remove(i); return;`). If none found, `tracing::warn!("Provider
  not found: {identifier}")` (no-op-with-warning).
- **`providers() -> Vec<ProviderInfo>`** (or run a closure with `&[ProviderInfo]`) ← `:59`. C++
  returns `const QList&`; since the data is behind a `Mutex`, expose either a clone or a
  `with_providers(|slice| …)` accessor. (`ProviderInfo` is not trivially `Clone` due to the boxed
  factory — prefer a `with_providers(f)` borrow-style accessor, or have the tests query via
  `find_provider`. Provide `for_each_provider`/`with_providers` to mirror the read-only iteration in
  `testDllFileNameInProviderInfo` and `populate_source_menu`.)
- **`find_provider(identifier) -> Option<…>`** ← cpp:50-57: linear scan for matching `identifier`.
  C++ returns `const ProviderInfo*` (pointer into the list, valid until mutation). In Rust, because
  the list is `Mutex`-guarded, return an owned **snapshot** of the fields the tests assert:
  `find_provider` → `Option<ProviderInfoView>` where
  `ProviderInfoView { name, identifier, dll_file_name, is_builtin, has_plugin: bool }`
  (clone-friendly view). This matches every assertion in `testProviderRegistryRegisterAndFind`
  (name, identifier, dllFileName, !isBuiltin, plugin != null → `has_plugin == true`). Document this
  as a faithful adaptation of the `const*` return under `Mutex`.
- **`clear()`** ← cpp:59-61: `providers.clear()`.
- **`populate_source_menu(menu, saved_sources)`** ← cpp:63-115: **UI builder, lowest priority,
  DEFER** to the UI layer. When reimplemented, preserve the exact routing-key contract:
  1. icon map for `processmemory`→server-process.svg, `remoteprocessmemory`→remote.svg,
     `windbgmemory`→debug.svg, `reclass.netcompatlayer`→plug.svg (else extensions.svg);
  2. first action "File", icon file-binary.svg, `data = "File"`;
  3. one action per provider in list order: label = `name` if `dll_file_name` empty else
     `"{name}  ({dll})"`, `data = name` (routing key is **name**, not identifier); if plugin present,
     call `plugin.populate_plugin_menu(menu)`;
  4. if `saved_sources` non-empty: separator, one checkable action per saved source
     (`data = "#saved:{i}"`), separator, "Clear All" (clear-all.svg, `data = "#clear"`).
  The routing-key strings consumed by the controller's `select_source` are `"File"`, the provider
  `name`, `"#saved:<i>"`, `"#clear"` — keep these literal (the tests call `select_source("#clear")`).

### 8.4 Test isolation requirement
The C++ tests share one process-global registry and clean up with `unregister_provider` in
`cleanup()`. Because Rust runs `#[test]`s in parallel threads sharing one `static`, the ported
registry tests MUST serialize. Use one of: (a) a `#[serial_test::serial]` attribute on the registry
tests, (b) run with `cargo test -- --test-threads=1` for the registry test file, or (c) make each
test register a **unique identifier** and unregister it in a scope guard. Recommend (c) + a single
`static MUTEX` test guard (or `serial_test`) since the duplicate-register test specifically needs a
known starting state. Document this in the test module.

---

## 9. TEST PLAN — each C++ test → Rust `#[test]`

Run all under `cargo test --no-default-features` (no gpui needed). Oracle: `test_provider`
35/0/0 PASS, `test_source_provider` 11/0/0 PASS (`RESULTS.md`); the snapshot unit cases live in
`test_refresh_speedups` (a controller test target, green overall). Place provider-only tests in
`src/provider/{buffer,null,snapshot}.rs` `#[cfg(test)] mod tests` (or `tests/provider.rs`); registry
tests in `tests/provider_registry.rs` (serialized — §8.4). The controller-level
`test_source_management` / `test_source_provider` lifecycle assertions are ported in the
**controller** spec; here we port the provider/registry-pure parts and the provider-double contract.

### 9.1 From `tests/test_provider.cpp` (→ `provider` unit tests) — covers `test_provider` oracle

| C++ test fn | Rust `#[test]` | Assertion (golden) |
|---|---|---|
| `nullProvider_isNotValid` | `null_is_not_valid` | `!NullProvider.is_valid()`, `size()==0` |
| `nullProvider_readFails` | `null_read_fails_leaves_buf` | `buf=[0xFF]`; `!read(0,&mut buf)`; `buf[0]==0xFF` (untouched) |
| `nullProvider_readU8ReturnsZero` | `null_read_u8_zero` | `read_u8(0)==0` |
| `nullProvider_readBytesReturnsZeroed` | `null_read_bytes_zeroed` | `read_bytes(0,4)==vec![0;4]` (len 4) |
| `nullProvider_isNotWritable` | `null_not_writable` | `!is_writable()` |
| `nullProvider_nameIsEmpty` | `null_name_empty` | `name().is_empty()` |
| `nullProvider_getSymbolReturnsEmpty` | `null_get_symbol_empty` | `get_symbol(0x7FF00000).is_empty()` |
| `buffer_emptyIsNotValid` | `buffer_empty_invalid` | `BufferProvider::with_data(vec![])`: `!is_valid()`, `size()==0` |
| `buffer_nonEmptyIsValid` | `buffer_nonempty_valid` | 16 zero bytes: `is_valid()`, `size()==16` |
| `buffer_nameFromConstructor` | `buffer_name_from_ctor` | `new(vec![0;4],"dump.bin")`: `name()=="dump.bin"`, `kind()=="File"` |
| `buffer_nameEmptyByDefault` | `buffer_name_empty_default` | `with_data(vec![0;4])`: `name().is_empty()` |
| `buffer_readU8` | `buffer_read_u8` | byte[0]=0xAB → `read_u8(0)==0xAB` |
| `buffer_readU16_littleEndian` | `buffer_read_u16_le` | bytes `34 12` → `read_u16(0)==0x1234` |
| `buffer_readU32` | `buffer_read_u32` | LE-bytes of 0xDEADBEEF → `read_u32(0)==0xDEADBEEF` |
| `buffer_readU64` | `buffer_read_u64_offset` | u64 0x0102030405060708 at off 4 → `read_u64(4)==…` |
| `buffer_readF32` | `buffer_read_f32` | `3.14f32` round-trips |
| `buffer_readF64` | `buffer_read_f64` | `2.71828f64` round-trips |
| `buffer_readAs_customStruct` | `buffer_read_as_struct` | `#[repr(C)] #[derive(Pod,Zeroable)] Pair{a:u16,b:u16}` `{0x1111,0x2222}` → `read_as::<Pair>(0)` matches |
| `buffer_readBytes_full` | `buffer_read_bytes_full` | "Hello, World!" → `read_bytes(0,5)==b"Hello"` |
| `buffer_readBytes_offset` | `buffer_read_bytes_offset` | "ABCDEFGH" → `read_bytes(4,4)==b"EFGH"` |
| `buffer_readBytes_pastEnd` | `buffer_read_bytes_past_end` | size 4, `read_bytes(2,8)` → len 8, all zero |
| `buffer_readBytes_zeroLen` | `buffer_read_bytes_zero_len` | `read_bytes(0,0)` → len 0 |
| `buffer_isReadable_withinBounds` | `buffer_is_readable_within` | size16: `(0,16)`,`(15,1)`,`(0,0)` all true |
| `buffer_isReadable_outOfBounds` | `buffer_is_readable_oob` | size16: `(0,17)`,`(16,1)`,`(100,1)` all false |
| `buffer_isReadable_zeroSizeProvider` | `buffer_is_readable_empty` | empty: `(0,1)` false; `(0,0)` true |
| `buffer_isWritable` | `buffer_is_writable` | `is_writable()` true |
| `buffer_writeBytes` | `buffer_write_bytes` | write `AA BB CC DD` at 2 → `read_u8(2)==0xAA`, `read_u8(5)==0xDD` |
| `buffer_write_pastEndFails` | `buffer_write_past_end_fails` | size4, `write_bytes(0, 8 bytes)` → false |
| `buffer_write_thenRead` | `buffer_write_then_read` | write u32 0x12345678 at 0 → `read_u32(0)==0x12345678` |
| `buffer_fromFile_nonexistent` | `buffer_from_file_missing` | `from_file("…nonexistent…")`: `!is_valid()`, `size()==0` |
| `buffer_fromFile_valid` | `buffer_from_file_valid` | write 64×0xAB to a tempfile → `is_valid()`, `size()==64`, `read_u8(0)==0xAB`, `name()==basename` |
| `polymorphic_nullToBuffer` | `polymorphic_null_to_buffer` | `let mut p: Arc<dyn Provider> = Arc::new(NullProvider)`; assert invalid/empty-name; reassign to `Arc::new(BufferProvider::new(LE-bytes-of-0xCAFEBABE, "test.bin"))`; `is_valid()`, `read_u64(0)==0xCAFEBABE`, `name()=="test.bin"`, `kind()=="File"`, `get_symbol(0x1000).is_empty()` |
| `buffer_getSymbol_alwaysEmpty` | `buffer_get_symbol_empty` | `get_symbol(0)` and `get_symbol(0x7FF00000)` both empty |

Translation notes:
- `read` taking `&mut [u8]`: `null_read_fails_leaves_buf` uses `let mut buf = [0xFFu8]; assert!(!p.read(0, &mut buf)); assert_eq!(buf[0], 0xFF);` — verifies the no-touch-on-failure contract.
- `from_file` tempfile: use `std::env::temp_dir().join("rcx_test_buffer_provider.bin")`, write 64×0xAB, assert, then remove. basename via `Path::file_name`.
- `buffer_readU64_offset`: place the u64 at offset 4 in a 16-byte buffer (use `to_ne_bytes`), assert `read_u64(4)`.

### 9.2 SnapshotProvider unit tests (from `tests/test_refresh_speedups.cpp` pure-unit slots)

| C++ test fn | Rust `#[test]` | Assertion (golden) |
|---|---|---|
| `snapshotProviderPermanentSet` | `snapshot_permanent_set` | `SnapshotProvider::new(None, PageMap::new(), 0)`; `!is_permanent(0x1000)`; `mark_permanent(0x1000+17)`; `is_permanent(0x1000)`; `is_permanent(0x1FFF)`; `!is_permanent(0x2000)`; `clear_permanent()`; `!is_permanent(0x1000)` |
| `snapshotProviderMergeKeepsExisting` | `snapshot_merge_keeps_existing` | initial pages `{0x0000→[0xAA;4096], 0x1000→[0xBB;4096]}`, extent 8192; `merge_pages({0x1000→[0xCC;4096]}, 8192)`; `read(0x0000, &mut [0u8;4])` → first byte 0xAA; `read(0x1000, &mut [0u8;4])` → first byte 0xCC |

Additional snapshot tests to ADD (not in C++ but required to pin the algorithm faithfully; assert the
documented behavior, not new behavior):
- `snapshot_read_empty_buf_false` — `read(0, &mut [])` returns **false** (len<=0 path), unlike base.
- `snapshot_read_always_true_zerofill` — `new(None, empty, extent)`; `read(0x5000, &mut [0xFFu8;16])`
  returns **true** and fills all 16 bytes with 0 (miss + no real → zeros).
- `snapshot_read_through_to_real` — `new(Some(Arc::new(BufferProvider::new(vec![0x11;4096],""))),
  empty_pages, 4096)`; reading an uncached offset returns the real bytes (read-through), and reading
  a real-out-of-range offset returns true with zeros (real read fails → zero-fill that chunk).
- `snapshot_page_split_read` — pages `{0x0000→[0xAA;4096]}`, `real=None`; `read(0x0FFE, &mut [0u8;4])`
  → first 2 bytes 0xAA (page 0 tail), last 2 bytes 0x00 (page 0x1000 missing, no real). Pins the
  4096-boundary split.
- `snapshot_is_readable_overflow_and_deferral` — `is_readable(u64::MAX, 16)` → false (overflow guard);
  `is_readable(addr, 0)` → true; with all pages cached → true; with an uncached page and `real=None`
  → false; with `real=Some(buffer that is_readable)` → true (deferral).
- `snapshot_patch_pages_existing_only` — pages `{0x1000→[0;4096]}` only; `patch_pages(0x0FFE, &[1,2,3,4])`
  patches the part landing in page 0x1000 (offsets 0,1 of page 0x1000 become 3,4) and silently drops
  the part for the missing page 0x0000; verify via `read`. (Also covers `write` calling `patch_pages`
  on success when wrapping a writable real provider.)
- `snapshot_forwarders` — wrap a `BufferProvider::new(vec![…],"dump.bin")`: `kind()=="File"`,
  `name()=="dump.bin"`, `is_writable()==true`, `pointer_size()==8`, `base()==0`,
  `enumerate_regions()` forwards (1 Mapped region); with `real=None`: `kind()=="File"`, `name()==""`,
  `is_writable()==false`, `enumerate_modules()==[]`, `size()==main_extent`.

### 9.3 From `tests/test_source_provider.cpp` — registry contract (→ `tests/provider_registry.rs`)

The C++ uses a `SelfProcessPlugin : IProviderPlugin` test-double that `createProvider` returns a
`BufferProvider` with the pattern `DE AD BE EF CA FE BA BE` and `lastBase = 0x7FF000000000`. Port the
double as a Rust `struct SelfProcessPlugin: ProviderPlugin` whose `create_provider` returns
`Ok(Box::new(BufferProvider::new(<256-byte buffer with that prefix>, "self")))` and
`initial_base_address` returns `0x7FF000000000`. **Serialize these tests (§8.4).**

| C++ test fn | Rust `#[test]` | What to port |
|---|---|---|
| `testProviderRegistryRegisterAndFind` | `registry_register_and_find` | `register_provider("TestProcessMemory","testprocessmemory", plugin, "libTestPlugin.dll")`; `find_provider("testprocessmemory")` → Some view with `name=="TestProcessMemory"`, `identifier=="testprocessmemory"`, `dll_file_name=="libTestPlugin.dll"`, `!is_builtin`, `has_plugin`. Cleanup: `unregister_provider`. |
| `testProviderRegistryUnregister` | `registry_unregister` | register; `find_provider(..).is_some()`; `unregister_provider("testprocessmemory")`; `find_provider(..).is_none()` |
| `testDllFileNameInProviderInfo` | `registry_dll_filename_propagates` | register with `"MyPlugin.dll"`; iterate via `with_providers`/`for_each_provider` and find `identifier=="testprocessmemory"` with `dll_file_name=="MyPlugin.dll"` |
| `testProviderReadsCorrectData` | (controller spec) — provider half: `provider_double_reads_pattern` | the **provider double** returns a buffer where `read_u8(0..8)==[DE,AD,BE,EF,CA,FE,BA,BE]` and `read_u64(0)==0xBEBAFECAEFBEADDE` (LE). Assert directly on the `BufferProvider` the double creates. (The attach-via-controller part lives in the controller spec.) |
| `testProviderDataIsNotPEHeader` | `provider_double_not_pe_header` | the double's buffer `read_u16(0) != 0x5A4D` (it's 0xADDE); `read_u8(0)==0xDE` |
| `testAttachViaPluginPreservesBaseAddress` | controller spec | base-address-on-attach is a **controller** behavior; out of scope here, noted. |
| `testSelectSourceUpdatesBaseAddress` | controller spec | controller behavior; noted. |
| `testSourceMenuIconsLoad` | SKIP (UI/Qt resources) | `populate_source_menu` deferred; SVG-resource loading is a Qt-resource test, not portable. Record as "ported with the source-picker UI." |
| `testMenuActionIconVisibility` | SKIP (UI/Qt) | same. |

Add registry-only tests not directly in C++ but pinning documented behavior:
- `registry_duplicate_identifier_is_noop` — register id "x"; register a second time with a different
  name but same id "x" → list still has exactly one entry named with the first registration
  (first-wins, warn). Use a unique id to avoid cross-test interference; assert via `with_providers`.
- `registry_unregister_absent_is_noop` — `unregister_provider("never-registered")` does not panic and
  leaves the registry unchanged (warn-only).
- `registry_register_builtin` — `register_builtin_provider("Buf","builtin-x", Box::new(|t| { *t =
  "chosen".into(); true }))`; `find_provider("builtin-x")` → `is_builtin==true`, `has_plugin==false`,
  `dll_file_name==""`. Optionally invoke the factory: `let mut s = String::new(); assert!(factory(&mut
  s)); assert_eq!(s, "chosen");` (the factory is the headless analogue of `BuiltinFactory`).

### 9.4 From `tests/test_source_management.cpp` — controller lifecycle (mostly controller spec)

These exercise `RcxController`/`RcxDocument` (load/clear/switch), which belong to the **controller**
subsystem. The provider-pure assertions to mirror **here** (independent of the controller) are the
provider-state expectations they pin:
- Initial provider is `NullProvider` → `size()==0`, `!is_valid()`, `name().is_empty()`
  (`testInitialProviderIsNull`, `testNullProviderNameEmpty`) → covered by §9.1 null tests.
- `loadData(Vec<u8>)` yields a valid `BufferProvider` (`size()==len`, `read_u8(0)` matches)
  (`testLoadDataCreatesValidProvider`) → the provider half is the `BufferProvider::new` contract in
  §9.1; the controller wiring (reset base, clear undo, emit changed) is the controller spec.
- After `clearSources`, provider is `NullProvider`: `read` fails, `read_u8(0)==0`, `name()` empty
  (`testProviderReadFailsAfterClear`) → covered by §9.1 null tests.
- `clearSources` idempotent / `switchSource(-1|999)` no-op → controller spec.

Cite in the controller spec that these reuse the in-scope provider doubles defined here. No new
provider `#[test]` is needed beyond §9.1–9.3; cross-reference them.

### 9.5 Out-of-scope tests (do NOT port as behavioral tests here)
- `tests/test_provider_getSymbol.cpp` — entirely `#ifdef _WIN32` (ProcessProvider live self-process);
  on Linux it is a single `QSKIP`. Not registered in the oracle set; OUT OF SCOPE (live process).
- `tests/test_kernel_provider.cpp`, `tests/test_windbg_provider.cpp` — kernel / WinDbg providers, OOS.
- `tests/test_tutorial.cpp` (6 self-process skips), the Windows-only RTTI-through-snapshot regression
  in `test_rtti_hint`/`test_tutorial` — the snapshot **forwarding + read-through** contract those rely
  on is covered headlessly by the §9.2 snapshot tests (`snapshot_forwarders`,
  `snapshot_read_through_to_real`).

---

## 10. Divergences from C++ to document in code comments

1. **`len` signedness.** C++ uses signed `int len` everywhere; negative `len` is a guarded edge
   (`is_readable` → false, `read_bytes`/`SnapshotProvider::read` early-out). In Rust, `read`/`write`
   take `&mut [u8]`/`&[u8]` so `len` is the slice length (`usize`, no negatives); the `len<0` branches
   collapse. `is_readable` takes `len: u64`; `len==0` must stay "readable true / empty result". This
   is the documented, intentional divergence — note it at each call.
2. **`size()` width.** C++ `int` (≤ INT_MAX); Rust uses `u64`. Reproduce the exact `is_readable`
   arithmetic with `u64` so the boundary cases (`(size,1)` false, `(size-1,1)` true, `(0,size+1)`
   false, overflow) match. (Recommendation from the behavioral map §11, adopted.)
3. **`SnapshotProvider::read` len==0** returns **false** (not the base's "empty/ok"). Keep the
   asymmetry; add a comment pointing at `read_bytes` for contrast.
4. **`find_provider` return.** C++ returns a `const ProviderInfo*` into the list; Rust returns an
   owned `ProviderInfoView` snapshot because the list is `Mutex`-guarded. Faithful to every asserted
   field; document the adaptation.
5. **`MemoryRegion::readable` default** is `true` (hand-write `Default`, do not derive).
6. **Native endianness** for `read_as<T>` via `bytemuck` — `from_ne_bytes` semantics, LE on targets.
7. **Interior mutability** (`Mutex`) replaces C++ non-const-through-shared_ptr mutation (§5).

---

## 11. Ordered, independently-verifiable implementation steps

Each step ends with `cargo build --no-default-features` (and, from step 3 on, `cargo test
--no-default-features` for that step's tests) green.

1. **Types + trait skeleton** (`mod.rs`): `RegionType` (+manual `Default`), `MemoryRegion`
   (manual `Default` with `readable=true`), `VtopResult`, `ThreadInfo`, `ModuleEntry`,
   `ProviderError`. Define the `Provider` trait with required `read`/`size` and all default methods
   incl. provided helpers (`is_valid`, `is_readable`, `read_as`, `read_u*`/`read_f*`, `read_bytes`,
   `write_bytes`). No impls yet. Verify: compiles; `read_as` `Pod` bound resolves (`bytemuck`).
2. **`NullProvider`** (`null.rs`): impl `Provider` (read false, size 0). Verify: compiles.
3. **`BufferProvider`** (`buffer.rs`): `Mutex<Vec<u8>>` + name; `new`/`with_data`/`from_file`,
   `read`/`write`/`size`/`is_writable`/`name`/`kind`/`enumerate_regions`/`data` accessor. Port §9.1
   `#[test]`s (Null + Buffer). Verify: all §9.1 tests pass (matches `test_provider` 35-assert oracle).
4. **`SnapshotProvider`** (`snapshot.rs`): fields + `Mutex` inner; `new`; `read` (page-split loop),
   `is_readable` (overflow + deferral), `write`, all forwarders, `update_pages`/`merge_pages`/
   `patch_pages`/`mark_permanent`/`is_permanent`/`clear_permanent`/accessors. Port §9.2 tests
   (incl. the two C++ unit cases + the added algorithm-pinning tests). Verify: §9.2 tests pass.
5. **Plugin trait slot + `ProviderInfo` + `ProviderKind`** (`registry.rs`): `ProviderPlugin` trait,
   `LoadType`, `BuiltinFactory`, `ProviderInfo`, `ProviderInfoView`. No registry yet. Verify: compiles.
6. **`ProviderRegistry` singleton** (`registry.rs`): `OnceLock<Mutex<…>>`; `register_provider`,
   `register_builtin_provider` (dup-identifier warn+skip), `unregister_provider` (first-match remove,
   warn-if-absent), `find_provider` (→ view), `with_providers`/`for_each_provider`, `clear`. Port the
   §9.3 registry tests with the `SelfProcessPlugin` double, serialized (§8.4). Verify: registry tests
   pass (mirrors `test_source_provider` register/find/unregister/dll-name asserts).
7. **`SavedSourceDisplay` + deferred `populate_source_menu` stub**: define the struct and a
   `populate_source_menu` that is feature-gated to the UI layer or left as a documented `todo!()`
   reserved for the source-picker UI; ensure non-UI builds don't reference it. Record the routing-key
   contract (`"File"`, name, `"#saved:<i>"`, `"#clear"`) in a doc comment.
8. **Re-exports + lib wiring**: `pub use` the trait, types, and built-ins from `provider::mod`;
   declare `pub mod provider;` in `lib.rs`. Confirm the controller (later) can hold
   `Arc<dyn Provider>` and that `Arc::new(NullProvider)`/`BufferProvider`/`SnapshotProvider` all
   coerce to `Arc<dyn Provider>` (object-safety check: the trait has a generic `read_as<T>` provided
   method — keep `read_as` a **provided** method with a `where Self: Sized` bound OR a generic default;
   since `read_as` is generic it is NOT object-safe → mark it `where Self: Sized` so the trait stays
   object-safe, and have `read_u*`/`read_f*` call it; `read_u*` are non-generic so remain dispatchable
   through `dyn Provider`). **Verify object-safety:** `let _: Arc<dyn Provider> = Arc::new(NullProvider);`
   compiles. Run the full subsystem test set green.

> Object-safety pin (important): `read_as<T>` must be `fn read_as<T: bytemuck::Pod>(&self, addr: u64)
> -> T where Self: Sized` so `dyn Provider` is allowed; the concrete `read_u8/16/32/64`, `read_f32/64`
> are non-generic and remain callable on `dyn Provider`. Tests that need `read_as::<Pair>` call it on
> a concrete `BufferProvider` (sized), not through `dyn`.

---

## 12. Port checklist (mirrors behavioral map §10)

- [ ] `RegionType` (`#[repr(u8)]`, manual `Default=Private`), `MemoryRegion` (manual `Default`,
      `readable=true`), `VtopResult` (derive `Default`), `ThreadInfo`, `ModuleEntry`.
- [ ] `Provider` trait: required `read(&self,u64,&mut[u8])->bool`, `size(&self)->u64`; defaults for
      `write`,`is_writable`,`name`,`is_live`,`kind`(="File"),`pointer_size`(=8),`base`,`get_symbol`,
      `symbol_to_address`,`enumerate_regions`,`peb`,`tebs`,`enumerate_modules`,kernel stubs; provided
      `is_valid`,`is_readable`(overridable),`read_as<T:Pod>`(where Self:Sized),`read_u8/16/32/64`,
      `read_f32/64`,`read_bytes`,`write_bytes`. Exact bounds math + failure-zeroing. `Send+Sync`.
- [ ] `BufferProvider` (`Mutex<Vec<u8>>`+name; `from_file` returns empty on error, name=basename;
      bounds-checked write; single `Mapped` region or empty; `data` accessor).
- [ ] `NullProvider` (size 0, read false, rest default).
- [ ] `SnapshotProvider` (page-split read with read-through + zero-fill + always-true-for-nonempty;
      overriding `is_readable` w/ overflow guard + real-deferral; `size`=main_extent; forwarders;
      `update_pages`/`merge_pages`/`patch_pages`/`mark_permanent`/`is_permanent`/`clear_permanent`/
      accessors; write-through + cache patch). `PAGE_SIZE`/`PAGE_MASK` constants.
- [ ] `ProviderRegistry` singleton (`OnceLock<Mutex<…>>`): `register_provider`/
      `register_builtin_provider` (dup → warn+skip), `unregister_provider` (first-match, warn-if-
      absent), `find_provider`(→view), `with_providers`, `clear`. `ProviderInfo`/`ProviderKind`/
      `ProviderInfoView`; reserved `ProviderPlugin` trait slot.
- [ ] Defer `populate_source_menu` (Qt UI); preserve routing keys `"File"`/name/`"#saved:<i>"`/`"#clear"`.
- [ ] `RCX_PLUGIN_EXPORT`/C-ABI plugin loading: out of scope; gate behind `#[cfg]` per-OS if added.
- [ ] All tests in §9.1–§9.3 green under `cargo test --no-default-features`; controller-level
      lifecycle (§9.4) deferred to the controller spec, reusing these doubles.
