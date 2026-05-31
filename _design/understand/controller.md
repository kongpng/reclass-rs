# Subsystem: Editor controller / interaction logic (`controller`)

C++ sources: `src/controller.cpp` (6964 lines), `src/controller.h` (411 lines).
Tests: `tests/test_controller.cpp` (1865 lines), `tests/test_refresh_speedups.cpp` (389 lines).
Key dependencies: `src/core.h` (NodeTree, Node, Command, ComposeResult, ValueHistory),
`src/providers/provider.h` (abstract `Provider`), `src/providers/snapshot_provider.h`,
`src/providers/buffer_provider.h`, `src/providers/null_provider.h`.

> **Correction to the porting hint**: the hint says "periodic refresh loop (default 660ms)".
> This is **wrong**. The default base refresh interval is **200 ms** (`kDefaultRefreshMs = 200`,
> `core.h:1148`), read from `QSettings("Reclass","Reclass") key "refreshMs"` at
> `controller.cpp:6516`. There is no `660` anywhere in the controller. The loop is *adaptive*
> (backs off 200ms → 1500ms when idle/unfocused), see `applyAdaptiveInterval`.

---

## 1. Purpose

`RcxController` is the mediator between the editor view(s) (`RcxEditor`, one or more split
panes), the in-memory document (`RcxDocument`: the `NodeTree` + the attached data source), and
the abstract `Provider`. It owns:

- **The refresh pipeline**: re-reads the active data source on a timer (asynchronously on a worker
  thread), diffs pages for change-highlighting, composes the rendered text+metadata via
  `rcx::compose(...)`, and pushes the result into every editor.
- **The async snapshot**: a `SnapshotProvider` page table the UI reads from so compose never blocks
  on I/O. Pointer fields are followed to pull in the pages they reference.
- **Value history / heatmap**: a per-node ring buffer (`ValueHistory`) used to color "hot" fields.
- **All tree mutations** funneled through `Command` (a `std::variant`) on a `QUndoStack`, applied
  by `applyCommand`. This is where edits flow *back* to the provider (`cmd::WriteBytes`).
- **Selection state** (`m_selIds`, anchor line) and the multi-editor sync.
- **Source management**: the saved-source list, switching sources, attaching providers.
- **Refresh speedups**: viewport-bounded reads, per-page stability backoff, permanent (module/.rdata)
  page caching, and adaptive interval based on focus/visibility/idleness.

Out of scope for the port (per task brief): live OS process / kernel / WinDbg providers (under
`plugins/`). They are reached only through `Provider` + `ProviderRegistry`. The controller treats
all sources purely through the `Provider` trait. The benign built-in sources are File
(`BufferProvider`), in-memory buffer (`BufferProvider`), snapshot (`SnapshotProvider`), and null
(`NullProvider`).

---

## 2. Key types

### 2.1 `Provider` (abstract trait) — `providers/provider.h`

The single abstraction the controller talks to. Maps directly to a Rust trait.

Pure-virtual (must implement):
- `bool read(uint64_t addr, void* buf, int len) const` — read `len` bytes at absolute address. Returns success.
- `int size() const` — logical size; `0` means "no data" / invalid.

Virtual with defaults:
- `bool write(uint64_t addr, const void* buf, int len)` → default `false` (read-only).
- `bool isWritable() const` → default `false`.
- `QString name() const` → label (e.g. `"dump.bin"`); default empty → UI shows `<Select Source>`.
- `bool isLive() const` → default `false`. **Auto-refresh only ticks when `isLive()` is true.**
- `QString kind() const` → default `"File"`. Category tag ("File"/"Process"/"Socket").
- `int pointerSize() const` → default `8`. Target arch pointer width (4 or 8).
- `uint64_t base() const` → default `0`. Initial base (main module). For file/buffer = 0.
- `QString getSymbol(uint64_t addr) const` → default empty.
- `uint64_t symbolToAddress(const QString&) const` → default `0`.
- `QVector<MemoryRegion> enumerateRegions() const` → default empty.
- `uint64_t peb() const`, `tebs()`, `enumerateModules()` — process-only; default empty.
- Kernel-paging hooks (`hasKernelPaging`, `getCr3`, `translateAddress`, `readPageTable`) — default empty.

Non-virtual derived convenience (Rust: provide as default trait methods or free helpers):
- `bool isValid() const { return size() > 0; }`
- `bool isReadable(uint64_t addr, int len) const` (virtual; default bounds-checks against `size()`):
  `len<0`→false, `len==0`→true, else `addr <= size && len <= size-addr`.
- `template<T> T readAs(uint64_t)` + `readU8/16/32/64`, `readF32/F64` (little-endian raw reads via `read`).
- `QByteArray readBytes(uint64_t addr, int len)`: `len<=0`→empty; allocate `len`; if `read` fails, fill `\0`.
- `bool writeBytes(uint64_t addr, const QByteArray& d) { return write(addr, d.data(), d.size()); }`

`MemoryRegion`: `{ base, size, readable, writable, executable, moduleName, RegionType type }`.
`RegionType`: `Image=0, Mapped=1, Private=2`.

### 2.2 `BufferProvider` — `providers/buffer_provider.h` (in scope)

Flat in-memory `QByteArray` source. `size()` = data size. `read`/`write` are bounds-checked
`memcpy` (return false out-of-range). `isWritable()` = `true`. `kind()` = `"File"`. `isLive()` =
**false** (so auto-refresh does NOT tick for file/buffer sources). `enumerateRegions()` returns one
synthetic `Mapped` region named after the file (or `"[buffer]"`). `name()` = filename passed to ctor.
Rust: `Vec<u8>` + name string.

### 2.3 `NullProvider` — `providers/null_provider.h` (in scope)

`size()=0`, `read`→false. Empty name → command row shows `<Select Source>`. The default provider a
fresh `RcxDocument` holds (`std::make_shared<NullProvider>()`).

### 2.4 `SnapshotProvider` — `providers/snapshot_provider.h` (in scope)

A page-table-backed `Provider` the controller builds from async reads. Compose reads entirely from
this so the UI thread never does I/O. Holds:
- `std::shared_ptr<Provider> m_real` — the underlying real provider (for write-through + fall-through).
- `QHash<uint64_t, QByteArray> m_pages` — page-aligned addr → 4096-byte page.
- `int m_mainExtent` — logical `size()`.
- `QSet<uint64_t> m_permanentPages` — pages classified as read-only module memory; never re-read.
- Constants: `kPageSize = 4096`, `kPageMask = ~4095`.

Methods (exact behavior — important for the port):
- `read(addr, buf, len)`: `len<=0`→false. Walks page by page: for each chunk, if the page is in
  `m_pages`, `memcpy` from it at `pageOff`; else if `m_real` exists, fall through to
  `m_real->read(cur,...)` and zero-fill on failure; else zero-fill. Always returns `true`. (The
  real-provider fall-through is required by the RTTI auto-hint reading vtable/.rdata bytes that
  weren't pre-fetched.)
- `isReadable(addr,len)`: `len<=0`→`len==0`. Overflow guard. For each page in range: if absent from
  `m_pages`, defer to `m_real->isReadable(addr,len)` (true if the real provider says so), else false.
  All pages present → true.
- `size()` = `m_mainExtent`. `isWritable/isLive/name/kind/pointerSize/base/getSymbol/symbolToAddress/
  enumerateModules/enumerateRegions/peb/tebs` all forward to `m_real` (or sane default if null).
- `write(addr,buf,len)`: if no `m_real`→false; else `m_real->write(...)`; on success `patchPages(...)`.
- `updatePages(pages, mainExtent)`: replace whole table.
- `mergePages(const PageMap& fresh, mainExtent)`: insert each fresh page over existing; set extent.
  **Pages not in `fresh` keep their old bytes** (key invariant for viewport-bounded reads).
- `markPermanent(pageAddr)` / `isPermanent(pageAddr)` (both page-align the input) / `clearPermanent()`.
- `patchPages(addr,buf,len)`: overwrite bytes in existing pages only (no allocation for missing pages).
- `pages()`, `permanentPages()` accessors.

Rust equivalent: a struct holding `Arc<dyn Provider>` (real) + `HashMap<u64, [u8;4096]>` (or
`Vec<u8>` pages) + `HashSet<u64>` permanent + extent. Implements the same `Provider` trait.

### 2.5 `RcxDocument` (`controller.h:27`)

A `QObject` owning the data model. Fields:
- `NodeTree tree` — the struct layout (see core.h analysis).
- `std::shared_ptr<Provider> provider` — active data source (defaults to `NullProvider`).
- `QUndoStack undoStack` — undo/redo. `cleanChanged` signal wired to set `modified` (`controller.cpp:166`).
- `QString filePath` — the `.rcx` path.
- `QString dataPath` — attached binary path (cleared when a non-file provider attaches).
- `bool modified` — dirty flag.
- `QHash<NodeKind, QString> typeAliases` — per-kind display-name overrides (saved/loaded as `typeAliases`).
- `std::unique_ptr<uint8_t[]> m_ownedBuffer` + `size_t m_ownedBufferSize` — owned writable buffer for
  self-attached "New Class" projects (RPM/WPM target our own process memory). nullptr for real sources.
  *(Used by the live-self tutorial path, which is out of scope; keep the field but it's UI/process-only.)*
- `QJsonArray pendingSavedSources` — raw saved-source JSON from load, lifted by the controller once.
- `int m_loadOverlapCount` — number of sibling overlaps detected on last `load()`.

Inline method `resolveTypeName(NodeKind)`: returns `typeAliases[kind]` if non-empty, else
`kindMeta(kind)->typeName`, else `"???"`.

Methods:
- `compose(viewRootId=0, compactColumns, treeLines, braceWrap, typeHints, showComments, symbolLookup)`:
  thin wrapper over `rcx::compose(tree, *provider, ...)` (`controller.cpp:171`).
- `bool save(const QString& path)` (`controller.cpp:179`): `json = tree.toJson()`; add `typeAliases`
  object if any; write `QJsonDocument::Indented`; on success set `filePath`, `undoStack.setClean()`,
  `modified=false`, return true. Open failure → false.
- `bool load(const QString& path)` (`controller.cpp:201`):
  1. Open RO; failure → false.
  2. Read all bytes. **Empty file → fresh empty project** (parse as empty JSON object). Non-empty
     file MUST parse as a JSON object; if `QJsonDocument::fromJson` is null or not an object → log
     a `qWarning` and **return false** (refuse to swallow raw binaries as a placeholder struct).
  3. `undoStack.clear()`; `tree = NodeTree::fromJson(root)`.
  4. `tree.validate(repair=true)` (re-roots orphans, breaks cycles, renumbers dup ids); log if dirty.
  5. `tree.findOverlaps()`; store count in `m_loadOverlapCount`; log up to 5 overlap pairs as warnings.
  6. Load `typeAliases` (kindFromString keys, non-empty string values).
  7. `pendingSavedSources = root["savedSources"].toArray()`; resolve **relative** `filePath` entries
     against the `.rcx` directory (`QDir(rcxDir).absoluteFilePath`).
  8. Set `filePath`, `modified=false`, `emit documentChanged()`, return true.
- `void loadData(const QString& binaryPath)` (`controller.cpp:308`): open RO (silent return on failure);
  `undoStack.clear()`; `provider = BufferProvider(file.readAll(), fileName)`; `dataPath = binaryPath`;
  `tree.baseAddress = 0`; `emit documentChanged()`.
- `void loadData(const QByteArray& data)` (`controller.cpp:321`): same but from in-memory bytes,
  no `dataPath`.

Signal: `documentChanged()` — wired in the controller ctor to `RcxController::refresh`.

### 2.6 `RcxCommand : QUndoCommand` (`controller.h:87`)

Wraps one `Command` variant. `undo()` → `m_ctrl->applyCommand(m_cmd, /*isUndo=*/true)`;
`redo()` → `applyCommand(m_cmd, false)`. **If `applyCommand` returns false AND the command is not a
transient (`WriteBytes`), call `setObsolete(true)`** so `QUndoStack` drops it (`controller.cpp:342-353`).
Rationale: a failed tree mutation can't recover and should leave the stack; a failed `WriteBytes` is
usually transient (process gone, page protection) and is *kept* so a later redo on a re-attached
writable provider can succeed.

Rust equivalent: an undo stack of `Command` values; "obsolete" = drop the entry. The redo-on-push
semantics matter: `QUndoStack::push` immediately calls `redo()` (executes the command).

### 2.7 `SavedSourceEntry` (`controller.h:99`)

`{ QString kind; QString displayName; QString filePath; QString providerTarget; uint64_t
baseAddress; QString baseAddressFormula; }`. `kind` is `"File"` or a provider identifier
(e.g. `"processmemory"`). Serialized in/out of the `.rcx` `savedSources` array. `baseAddress`
is stored hex in JSON.

### 2.8 `ValueHistory` (`core.h:867`) — the per-node ring buffer / heatmap

```
kCapacity = 10
std::array<QString,10> values
std::array<qint64,10>  timestamps   // QDateTime::currentMSecsSinceEpoch()
int count = 0      // total recorded (NOT capped; saturates at INT_MAX)
int head  = 0      // next write index
```
- `record(v)`: if `count>0` and the *last* recorded value equals `v`, **no-op** (dedup consecutive
  duplicates). Else store `v` + timestamp at `head`, advance `head` mod 10, `count++` (capped at INT_MAX).
- `clear()`: `count=0; head=0` (does not erase the array contents).
- `uniqueCount()` = `min(count, 10)`.
- `heatLevel()`: `count<=1 → 0` (static); `count==2 → 1` (cold); `count<=4 → 2` (warm); else `3` (hot).
- `last()`: value at `(head+9) mod 10`, or empty if `count==0`.
- `forEach(fn)`: oldest→newest over `uniqueCount` entries. `forEachWithTime(fn)`: newest→oldest, with ts.

Rust: a fixed `[String;10]` + `[i64;10]` ring, or a small ring-buffer struct. Tested heavily
(`testValueHistoryRingBuffer`): `record("10");record("10")` keeps count==1; 2 unique→cold; 3→warm;
5+→hot; >10 records → uniqueCount stays 10 but count keeps growing; `forEach` yields exactly 10,
last == `last()`.

### 2.9 `Command` variant (`core.h:1080-1115`)

`std::variant` of 18 POD command structs. Each carries old+new values so `applyCommand` can run
forward (`isUndo=false`) or backward (`isUndo=true`). The full list (fields):
- `OffsetAdj { nodeId; oldOffset; newOffset }` — a sibling offset shift (embedded in several commands).
- `ChangeKind { nodeId; oldKind; newKind; QVector<OffsetAdj> offAdjs }`
- `Rename { nodeId; oldName; newName }`
- `Collapse { nodeId; oldState; newState }`
- `Insert { Node node; QVector<OffsetAdj> offAdjs }`
- `Remove { nodeId; QVector<Node> subtree; QVector<OffsetAdj> offAdjs }`
- `ChangeBase { oldBase; newBase; oldFormula; newFormula }`
- `WriteBytes { addr; QByteArray oldBytes; QByteArray newBytes }` — **the only command that touches the provider**.
- `ChangeArrayMeta { nodeId; oldElementKind; newElementKind; oldArrayLen; newArrayLen }`
- `ChangePointerRef { nodeId; oldRefId; newRefId }`
- `ChangeStructTypeName { nodeId; oldName; newName }`
- `ChangeClassKeyword { nodeId; oldKeyword; newKeyword }`
- `ChangeOffset { nodeId; oldOffset; newOffset }`
- `ChangeEnumMembers { nodeId; old/new QVector<QPair<QString,int64_t>> }`
- `ChangeOffsetExpr { nodeId; oldExpr; newExpr }`
- `ToggleStatic { nodeId; oldVal; newVal }`
- `ToggleRelative { nodeId; oldVal; newVal }` — *(declared but not handled in applyCommand — no-op currently)*
- `ToggleBigEndian { nodeId; oldVal; newVal }`
- `ChangeComment { nodeId; oldComment; newComment }`

Rust: an enum with one variant per struct. Note `ToggleRelative` has no `applyCommand` arm in
the C++ (a latent gap; preserve as a no-op for fidelity, or fix — but document).

### 2.10 `RcxController` private state (`controller.h:267-408`)

The fields a Rust port must hold (grouped):

View/selection:
- `RcxDocument* m_doc`, `QList<RcxEditor*> m_editors`, `ComposeResult m_lastResult`.
- `QSet<uint64_t> m_selIds`, `int m_anchorLine = -1`.
- `uint64_t m_viewRootId = 0` (0 = first root struct).
- Display toggles: `m_compactColumns, m_treeLines, m_braceWrap, m_typeHints` (false),
  `m_showComments` (false), `m_showRtti` (true), `m_showEnumChips` (true).
- `bool m_suppressRefresh = false` (batches mutations), `bool m_readOnlyOverride = false`.

Saved sources:
- `QVector<SavedSourceEntry> m_savedSources`, `int m_activeSourceIdx = -1`, `bool m_lastLive = false`.

Auto-refresh:
- `using PageMap = QHash<uint64_t, QByteArray>`.
- `QTimer* m_refreshTimer`, `QFutureWatcher<PageMap>* m_refreshWatcher`.
- `std::unique_ptr<SnapshotProvider> m_snapshotProv`.
- `PageMap m_prevPages` (previous-tick page bytes, for diffing — accumulated, merged).
- `QSet<int64_t> m_changedOffsets` (changed byte offsets relative to baseAddress this tick).
- `QHash<uint64_t, ValueHistory> m_valueHistory` + `QHash<uint64_t, uint64_t> m_lastValueAddr`.
- `bool m_trackValues = true`, `int m_valueTrackCooldown = 0`.
- `uint64_t m_refreshGen = 0`, `uint64_t m_readGen = 0`, `bool m_readInFlight = false`.

Refresh speedups:
- `QHash<uint64_t,int> m_pageStability`, `kStabilityThreshold = 5`.
- `uint64_t m_tickCount = 0`.
- `int m_idleTicks = 0`; `m_refreshIntervalBaseMs = 200`, `m_refreshIntervalMaxMs = 1500`,
  `m_refreshIntervalBlurMs = 1500`; `kIdleBackoffTicks = 8`.
- `bool m_windowFocused = true`, `bool m_windowVisible = true`.
- `kPointerSnapshotByteBudget = 64*1024*1024` (64 MB).

Misc:
- `QVector<RcxDocument*>* m_projectDocs` (cross-tab type visibility; pointer to project doc list).
- `QTimer* m_cycleMacroTimer`, `bool m_cycleMacroOpen` (undo grouping for rapid ←→ type cycling — UI).
- `QPointer<TypeSelectorPopup> m_cachedPopup`, `int m_typePopupGen`, `QStringList m_recentTypeNames`,
  `QPointer<SourceChooserPopup> m_cachedSourcePopup`, `QPointer<HexToolbarPopup> m_hexToolbar` (UI popups).

Signals: `nodeSelected(int nodeIdx)`, `selectionChanged(int count)`, `statusHint(QString)`,
`contextMenuAboutToShow(QMenu*,int)`, `requestOpenProviderTab(...)`, `requestOpenStructInNewTab(uint64_t)`,
`sourceLivenessChanged(bool)`.

---

## 3. Construction / destruction

`RcxController(RcxDocument* doc, QWidget* parent)` (`controller.cpp:357`):
1. `fmt::setTypeNameProvider(docTypeNameProvider)` — installs a global function hook so format.cpp
   resolves type display names through the *current compose document* (a `thread_local
   s_composeDoc`, set by `ComposeDocGuard` during compose).
2. `connect(m_doc, documentChanged, this, refresh)`.
3. `setupAutoRefresh()` (creates + starts the timer and the `QFutureWatcher`).
4. Connect own `nodeSelected` → `hideHexToolbar()` (UI).
5. `ingestPendingSavedSources()`.

`ingestPendingSavedSources()` (`controller.cpp:378`): if `m_doc->pendingSavedSources` empty → return.
Move it into a local, **clear the doc's copy (consume once)**. For each JSON object, build a
`SavedSourceEntry` (baseAddress parsed as hex). Then auto-activate index 0 via `switchToSavedSource(0)` —
**unless** entry 0 is a `"File"` whose `filePath` is non-empty and missing on disk (then log + skip,
so the user still sees the layout without bytes).

Destructor (`controller.cpp:408`): cancel + `waitForFinished()` the refresh watcher, then reset the
snapshot. **The async read must be joined before teardown.**

`resetProvider()` (`controller.cpp:417`): just `m_snapshotProv.reset()`.

Rust threading note: `QFutureWatcher<PageMap>` + `QtConcurrent::run` = a worker thread that returns a
`HashMap<u64,Vec<u8>>`; the `finished` callback (`onReadComplete`) runs on the UI thread. In Rust use
a thread/`spawn` + channel, or a thread pool, with the completion handled on the main loop. The
`shared_ptr<Provider>` is captured by value into the worker (so the provider must be `Send`+thread-safe
for reads).

`primaryEditor()` = first editor or null. `addSplitEditor(parent)` creates a `RcxEditor`, appends,
`connectEditor`, (sets up undo redirection — not shown but standard). `removeSplitEditor` removes from
the list. `editors()` returns the list.

---

## 4. The refresh pipeline

### 4.1 `refresh()` (`controller.cpp:1891`) — synchronous recompose + push to editors

This is the central rebuild. Steps:
1. `ComposeDocGuard composeGuard(m_doc)` — RAII sets the thread-local doc for type-name resolution,
   restores prior on scope exit (exception-safe). Rust: pass the doc/alias map explicitly into compose
   instead of a thread-local; cleaner.
2. Build a `symLookup` closure: if `g_nameLookupHook` is set (GUI), call it; else fall back to
   `SymbolStore::instance().getSymbolForAddress(addr, prov)`. (Tests leave the hook null.)
3. **Compose**: if `m_snapshotProv` exists, `rcx::compose(tree, *m_snapshotProv, m_viewRootId, ...,
   m_showRtti, m_showEnumChips)`; else `m_doc->compose(m_viewRootId, ...)` (the doc wrapper, which
   doesn't pass showRtti/showEnumChips — they default). Result stored in `m_lastResult`.
4. **Change-highlight pass** (only if `m_changedOffsets` non-empty): build a childMap once. For each
   `LineMeta` with a valid `nodeIdx`: compute `offset = lm.offsetAddr - tree.baseAddress`.
   - Hex-preview nodes: per-byte — for each byte index `b` in `[0, lm.lineByteCount)`, if
     `m_changedOffsets.contains(offset + b)`, append `b` to `lm.changedByteIndices` and set `lm.dataChanged`.
   - Otherwise: size = `structSpan` (containers) or `byteSize` (leaves); if any byte in `[offset,
     offset+sz)` is in `m_changedOffsets`, set `lm.dataChanged = true` (break).
5. **Value tracking pass**: determine `prov` — snapshot if `isLive()`, else real provider if
   `isValid() && isLive()`, else null. Decrement `m_valueTrackCooldown` if >0. If `m_trackValues &&
   prov && cooldown<=0`: for each `LineMeta` that is a non-synthetic, non-continuation `Field` line of
   a non-container, non-FuncPtr node: read `addr = lm.offsetAddr`, `sz = node.byteSize()`. Skip if
   `sz<=0` or `!prov->isReadable(addr,sz)`. `val = fmt::readValue(node, *prov, addr, lm.subLine)`.
   If non-empty: **if the node's address changed since last (`m_lastValueAddr[nodeId] != addr`),
   remove its history** (stale); update `m_lastValueAddr[nodeId]=addr`;
   `m_valueHistory[nodeId].record(val)`; `lm.heatLevel = m_valueHistory[nodeId].heatLevel()`.
6. **Prune stale selections**: for each id in `m_selIds`, strip footer/array/member bits and check
   `tree.indexOfId(nodeId) >= 0`; keep the original (bit-decorated) id if the base node still exists.
7. Collect `customTypes` = unique non-empty `structTypeName`s of Struct nodes (for the type picker).
8. Resolve `snapProv` (snapshot or real) and `realProv` (always real) for the disasm popup.
9. **Apply to each editor** (`refresh.applyToEditors`): `setCustomTypeNames`, `setValueHistoryRef(&m_valueHistory)`,
   `setProviderRef(snapProv, realProv, &tree)`, save view state, `applyDocument(m_lastResult)`, restore
   view state.
10. **Tail** (`refresh.tail`): `pushSavedSourcesToEditors()`, `updateCommandRow()`, `applySelectionOverlays()`
   (order matters: text-modifying passes before overlays so hover indicators survive).

Edge cases / invariants:
- `refresh()` is invoked from: `documentChanged`, `applyCommand` (when success && !suppress),
  `onReadComplete` (only when bytes changed or first snapshot), `setViewRootId`, all the display
  toggles, `setTrackValues(false)`, source-switch paths.
- When `m_suppressRefresh` is set (macros), individual mutators skip `refresh()` and the macro caller
  refreshes once at the end.

### 4.2 Async tick: `onRefreshTick()` (`controller.cpp:6586`)

Fires on `m_refreshTimer` timeout. Algorithm:
1. **Liveness flip detection (runs before early-returns)**: `nowLive = m_doc->provider &&
   m_doc->provider->isValid()`. If it differs from `m_lastLive`, update and `emit sourceLivenessChanged(nowLive)`.
2. Early returns (no read this tick): `m_readInFlight`; provider null or `!isLive()`; `m_suppressRefresh`;
   any editor `isEditing()`.
3. `++m_tickCount`. `extent = computeDataExtent()`; if `<=0` return.
4. Build `ranges` = `[(baseAddress, extent)]`. If `m_snapshotProv` exists, also
   `collectPointerRanges(rootId, baseAddress, depth=0, maxDepth=99, visited, ranges, budget)` where
   `rootId = m_viewRootId` (or first node id if 0), `budget = kPointerSnapshotByteBudget - extent`.
5. **Speedup 1 (viewport bounding)**: `firstSnapshot = !m_snapshotProv || m_prevPages.isEmpty()`.
   If not first, `viewport = viewportAddressRange()`.
6. Build `requestPages` (set of page-aligned addrs). For each range, for each page in
   `[rangeStart&mask, ceil(rangeEnd)&mask)`:
   - **Speedup 4**: skip if `m_snapshotProv->isPermanent(p)`.
   - If `viewport` is set: compute `lo = (viewport.first - 2*4096) & mask` (clamped to 0),
     `hi = ceil(viewport.second + 2*4096) & mask` (2-page overscan each side). `inViewport = lo<=p<hi`.
     `isMainRange = (rangeStart == baseAddress)`. If main range AND not in viewport:
     - **Speedup 2 (stability backoff)**: `stab = m_pageStability[p]`; `isStable = stab >= 5`;
       if stable AND `(m_tickCount & 1)` (odd tick) → **skip** (half-rate re-read of stable backstage pages).
   - Else insert `p` into `requestPages`.
7. If `requestPages` empty: everything stable+offscreen or permanent → `++m_idleTicks`,
   `applyAdaptiveInterval()`, **return without recompose** (snapshot already on screen).
8. Else: `m_readInFlight = true`; `m_readGen = m_refreshGen`. Capture `prov` (shared_ptr) + page list,
   launch `QtConcurrent::run` returning a `PageMap` of `prov->readBytes(p, 4096)` for each page.
   `m_refreshWatcher->setFuture(...)`.

### 4.3 Async completion: `onReadComplete()` (`controller.cpp:6698`)

Runs on UI thread when the future finishes.
1. `m_readInFlight = false`.
2. **Generation guard**: if `m_readGen != m_refreshGen`, **discard** (a mutation invalidated this read).
3. Get `newPages = watcher->result()`; on exception log + return.
4. **All-zero guard**: if `m_prevPages` non-empty and `newPages` contains page 0 that is all-zero,
   **discard the whole result and keep the stale snapshot** (a process likely vanished mid-read).
5. **Diff + stability update**: clear `m_changedOffsets`. `firstSnapshot = m_prevPages.isEmpty()`.
   For each fresh page: if absent from `m_prevPages` → set its stability to 0 and `continue` (first-sight
   is not a mutation). Else compare bytes over `min(len)`; record changed offsets into `m_changedOffsets`
   (as `pageAddr + i`); if any byte changed → stability=0, `anyChanged=true`; else
   `stability = min(kStabilityThreshold+16, stability+1)` (clamped, so it climbs but caps at 21).
6. **Adaptive idle counting**: if `anyChanged` → `m_idleTicks=0`; else if not first snapshot → `++m_idleTicks`.
   `applyAdaptiveInterval()`.
7. `mainExtent = computeDataExtent()`.
8. **Merge** fresh pages into `m_prevPages` (insert each — accumulates). Then into the snapshot:
   if `m_snapshotProv` exists → `mergePages(newPages, mainExtent)`; else construct a new
   `SnapshotProvider(m_doc->provider, newPages, mainExtent)`.
9. `classifyPermanentPages(newPages)` (Speedup 4, must run after snapshot exists).
10. If `anyChanged || firstSnapshot` → `refresh()` (recompose). Always `m_changedOffsets.clear()` at end.

### 4.4 `collectPointerRanges(...)` (`controller.cpp:6532`) — pointer following

Recursively gathers `(absoluteAddr, span)` ranges. Args: `structId`, `memBase` (where this struct's
data lives), `depth`, `maxDepth`, `visited` set of `(structId, memBase)`, `ranges` out, `budget` (in/out).
- Stop if `depth>=maxDepth`, `budget<=0`, or `(structId,memBase)` already visited.
- `span = tree.structSpan(structId)`; if `<=0` return (no point reading nothing). Append `(memBase, span)`;
  `budget -= span`; if `<=0` return.
- If no `m_snapshotProv`, return (can't read pointer values yet — bootstrap tick reads only main extent).
- For each child that is `Pointer32`/`Pointer64`, **non-collapsed, refId!=0** (Speedup 3: collapsed
  pointers are skipped): `ptrAddr = memBase + child.offset`; if `!snapshot.isReadable(ptrAddr, ptrSize)`
  skip; read `ptrVal` (U32 or U64); skip if `0` or `UINT64_MAX`; recurse into `(child.refId, ptrVal,
  depth+1, ...)`.
- Embedded struct ref (Struct node with `refId!=0` and no own children): recurse `(sn.refId, memBase,
  depth, ...)` (same depth — it's an inline expansion, not a pointer hop).

Rust: this reads pointer values **from the previous snapshot** (`m_snapshotProv`), not the real
provider, so the targets it follows lag by one tick. This is intentional and tested implicitly.

### 4.5 `viewportAddressRange()` (`controller.cpp:6798`)

Returns `Option<(lo, hi)>` covering the visible lines across all editors. For each editor's Scintilla:
`firstLine = firstVisibleLine()`, `onScreen = SCI_LINESONSCREEN`. For each visible *visual* line,
`docLine = SCI_DOCLINEFROMVISIBLE(v)`, `lm = metaForLine(docLine)`. Skip if no meta or
`lm->offsetAddr == 0` (synthetic/command-row). Track `lo = min(addr)`, `hi = max(addr+16)` (16 = largest
hex preview; overscan widens further in the caller). `nullopt` if nothing visible (during construction).
Rust: needs the editor's scroll/visible-line API; behind the same view abstraction.

### 4.6 `classifyPermanentPages(fresh)` (`controller.cpp:6832`)

If no snapshot or no provider → return. `regions = m_doc->provider->enumerateRegions()`; empty → return.
For each fresh page not already permanent: find a region with non-empty `moduleName` that **wholly
contains** the page (`page >= base && page+4096 <= base+size`) and is `executable`; if found,
`m_snapshotProv->markPermanent(page)`. (Test `permanentPagesMarkedAfterModuleRead` relies on this:
a synthetic executable module region → its pages become permanent and stop being re-read.)

### 4.7 `computeDataExtent()` (`controller.cpp:6856`)

Computes the byte span of the whole tree from baseAddress. For each node: `off = tree.computeOffset(i)`;
skip if `off<0`. `sz = structSpan` (containers) or `byteSize`. `treeExtent = max(treeExtent, off+sz)`.
If `treeExtent>0` return `min(treeExtent, 16 MB)`. Else if `provider->size()>0` return that. Else 0.
This is the `SnapshotProvider::size()` (the `mainExtent`).

### 4.8 `resetSnapshot()` (`controller.cpp:6876`)

`m_refreshGen++` (cancels in-flight reads via the gen guard); `m_readInFlight=false`;
`m_snapshotProv.reset()`; clear `m_prevPages`, `m_changedOffsets`, `m_valueHistory`, `m_lastValueAddr`,
`m_pageStability`; `m_idleTicks=0`; `m_tickCount=0`; `applyAdaptiveInterval()` (restarts a paused timer).
Called on base-address change and on every source switch.

### 4.9 Adaptive interval

`setupAutoRefresh()` (`controller.cpp:6515`): read `refreshMs` (default 200) from QSettings; set base,
max=`max(base,1500)`, blur=`max(base,1500)`; create `QTimer` with interval=base; connect timeout →
`onRefreshTick`; `start()`. Create the `QFutureWatcher`, connect `finished` → `onReadComplete`.

`setRefreshInterval(ms)` (`controller.cpp:6430`): `base = max(1,ms)`; recompute max/blur; `applyAdaptiveInterval()`.

`applyAdaptiveInterval()` (`controller.cpp:6441`): if no timer return.
- If `!m_windowVisible` → **stop the timer entirely** (minimized = nothing to draw) and return.
- Else `target =`:
  - `!m_windowFocused` → `m_refreshIntervalBlurMs`.
  - `m_idleTicks >= kIdleBackoffTicks (8)` → geometric backoff: `factor = 1 << min(4, (idleTicks-8)/8 + 1)`,
    `target = min(maxMs, baseMs*factor)`. (base=200 → after 8 idle:400, 16:800, 24:1500 capped.)
  - else → `m_refreshIntervalBaseMs`.
- If interval differs, `setInterval(target)`. If timer not active, `start()`.

`setWindowState(focused, visible)` (`controller.cpp:6469`): `focusGained = focused && !m_windowFocused`;
store both; if focusGained reset `m_idleTicks=0` (snap back to base immediately); `applyAdaptiveInterval()`.
Tests: blur → interval becomes 1500 and timer stays active; `visible=false` → timer inactive; restoring
visibility+focus → timer active at base (50 in the test).

Test accessors (`controller.h:240-248`): `valueHistory()`, `lastResult()`, `dataExtent()`,
`refreshIntervalMs()` (timer interval or 0), `refreshTimerActive()`, `idleTicks()`,
`pageStability(pageAddr)` (page-aligns input), `snapshotProv()`.

---

## 5. Applying edits — `applyCommand` (`controller.cpp:2827`)

`bool applyCommand(const Command& cmd, bool isUndo)`. Returns false only when the underlying op was
rejected (currently only `WriteBytes` failure, or `readOnlyOverride` blocking a write). Algorithm:
1. `tree.touch()` (bump generation) — every command does this, even WriteBytes/ChangeBase (a value
   cache keyed on (gen, base) must invalidate when base changes).
2. Two local lambdas:
   - `clearNodeHistory(id)`: remove from `m_valueHistory` and `m_lastValueAddr`.
   - `clearHistoryForAdjs(adjs)`: if empty return. **`m_refreshGen++`** (discard in-flight read whose
     layout is now stale). For each adj: `clearNodeHistory(adj.nodeId)`. If any adjusted node is a
     container, build a childMap once and BFS-clear all descendants' histories too.
3. `std::visit` over the variant. Per-variant behavior (offset/old/new chosen by `isUndo`):
   - **ChangeKind**: set `nodes[idx].kind`; apply `offAdjs` offsets; `m_refreshGen++`;
     `clearHistoryForAdjs(offAdjs)`. **Value history is intentionally KEPT across kind changes**
     (the hover trend is most useful right when you accept a TypeHint).
   - **Rename / Collapse / ChangeStructTypeName / ChangeClassKeyword / ChangeEnumMembers /
     ChangeOffsetExpr / ToggleStatic / ToggleBigEndian / ChangeComment**: set the single field.
   - **Insert**: undo → revert offAdjs, then remove `node.id` (+ invalidateIdCache);
     redo → `tree.addNode(node)`, then apply offAdjs. Then `clearHistoryForAdjs(offAdjs)`.
   - **Remove**: undo → re-add subtree nodes, then revert offAdjs;
     redo → apply offAdjs first, then `subtreeIndices(nodeId)` sorted **descending** (so removals don't
     invalidate earlier indices), clear each node's history, remove; `invalidateIdCache()`.
     Then `clearHistoryForAdjs(offAdjs)`.
   - **ChangeBase**: set `baseAddress` + `baseAddressFormula`; `resetSnapshot()`.
   - **WriteBytes**: choose `bytes = isUndo ? oldBytes : newBytes`. **If `m_readOnlyOverride` → set
     `success=false` and return** (refuse even on undo/redo so a queued write can't later stomp memory).
     Else write through `m_snapshotProv->write(...)` if present else `m_doc->provider->writeBytes(...)`.
     On failure: log, `emit statusHint("Write rejected at 0x..")`, `success=false`. (No optimistic visual
     leak — snapshot only patches on success.)
   - **ChangeArrayMeta**: set `elementKind` + `arrayLen`; clamp `viewIndex` to `< arrayLen`.
   - **ChangePointerRef**: set `refId`; if non-zero, force `collapsed=true`.
   - **ChangeOffset**: set `offset`; `m_refreshGen++`; clear this node's + all subtree nodes' history.
   - **ToggleRelative**: *(no arm — currently a no-op; latent bug to preserve or fix-and-document)*.
4. If `success && !m_suppressRefresh` → `refresh()`. Return `success`.

Invariant: when a WriteBytes fails, no refresh runs (UI keeps last good state). `RcxCommand::redo/undo`
read the return value: failure + not transient → `setObsolete(true)`.

### 5.1 `setNodeValue(nodeIdx, subLine, text, isAscii=false, resolvedAddr=0)` (`controller.cpp:3061`)

The user-facing "edit a field's value" path (inline editing). Steps:
1. Bounds-check `nodeIdx`. If `!provider->isWritable()` → return. If `m_readOnlyOverride` → silent return.
2. Resolve `addr`: use `resolvedAddr` if non-zero (correct for pointer-expanded children, supplied by the
   editor from the line's `offsetAddr`); else `baseAddress + computeOffset(nodeIdx)` (return if offset<0).
3. **Vec2/3/4**: if `subLine>=0`, `addr += subLine*4`, `editKind = Float`. **Mat4x4**: if `0<=subLine<16`,
   `addr += subLine*4`, `editKind = Float`.
4. Parse to bytes: if `isAscii` → `fmt::parseAsciiValue(text, sizeForKind(editKind), &ok)`; else build a
   temporary `Node` with `kind=editKind` (and the node's `bigEndian`) and `fmt::parseValue(editNode, text, &ok)`.
   If `!ok` return.
5. **Strings (UTF8/UTF16)**: pad/truncate `newBytes` to `node.byteSize()` (the full buffer).
6. If `newBytes` empty → return. `writeSize = newBytes.size()`.
7. If `!provider->isReadable(addr, writeSize)` → return (don't push a command that can't apply).
8. Read `oldBytes = provider->readBytes(addr, writeSize)` (for undo).
9. **Test the write first**: write through snapshot (if present) else provider. If it fails → log,
   `refresh()` (show real unchanged value), return (no command pushed).
10. On success: `undoStack.push(new RcxCommand(this, WriteBytes{addr, oldBytes, newBytes}))`.
    (The push immediately calls redo → writes again, harmless.)

Tests (`testSetNodeValueWritesData`, `...UndoRedo`, `...Float`, `...Hex`, `...Bool`, `...NegativeInt`)
verify the bytes land in the `BufferProvider`, and undo restores old bytes, redo re-applies.

### 5.2 `writeSelectedBytesToFile(addr, n, path, err)` (`controller.cpp:2789`) — public static-style helper

`n<=0` → err "No bytes to save", false. Pick `prov` = snapshot (if present) else real (else err "No
active provider"). If `!prov->isReadable(addr,n)` → err. `data = readBytes(addr,n)`; if short read → err.
Open `path` WriteOnly|Truncate (err on fail); write; err on short write. Returns true on full success.
Snapshot wins over real (matches copy paths). Pure logic — easy to port + unit test.

---

## 6. Selection logic (`controller.cpp:5094-5184`)

`m_selIds` holds **decorated** ids. Decoration bits (from core.h):
- Footers: `nodeId | kFooterIdBit (0x8000...)`.
- Array element: `makeArrayElemSelId(nodeId, elemIdx) = nodeId | kArrayElemBit(0x4000...) |
  ((elemIdx & 0xFFFFF) << 42)`.
- Member (enum/bitfield): `makeMemberSelId(nodeId, subLine) = nodeId | kMemberBit(0x2000...) |
  ((subLine & 0xFFFFF) << 42)`.
- Strip mask used everywhere: `~(kFooterIdBit | kArrayElemBit | kArrayElemMask | kMemberBit | kMemberSubMask)`.

`handleNodeClick(source, line, nodeId, mods)`:
- `effectiveId(line, nid)` looks at `m_lastResult.meta[line]`: Footer → `nid|kFooterIdBit`;
  array element → `makeArrayElemSelId`; member line → `makeMemberSelId`; else `nid`.
- `nodeId==0` → `clearSelection()`, return.
- Compute `selId = effectiveId(line, nodeId)`. Then by modifiers:
  - **No mod**: clear, insert selId, `m_anchorLine = line`.
  - **Ctrl** (no shift): toggle selId in/out of set; `m_anchorLine = line`.
  - **Shift** (no ctrl): if no anchor → like no-mod; else clear and insert `effectiveId(i, meta[i].nodeId)`
    for every line `i` in `[min(anchor,line), max(anchor,line)]` whose nodeId is non-zero and not the
    command row.
  - **Ctrl+Shift**: if no anchor → insert selId (additive) + set anchor; else add the whole range
    (additive, no clear).
- `updateCommandRow()`, `applySelectionOverlays()`.
- If exactly one selected → strip bits, look up index, `emit nodeSelected(idx)`.

`clearSelection()`: clear set, anchor=-1, updateCommandRow, applySelectionOverlays.
`applySelectionOverlays()`: each editor `applySelectionOverlay(m_selIds)`.
`selectedIds()` returns the set.

`updateCommandRow()` (`controller.cpp:5187`): builds line 0 text from provider metadata.
- Source label: `provider->name()` empty → `"source▾"`; else `"'<name>'▾"`.
- Address: `baseAddressFormula` if non-empty else `"0x" + uppercase hex of baseAddress`.
- Row = `"<src elided 40>  <addr elided 24>"`.
- Row2 (class line): if `m_viewRootId` resolves, `"<keyword> <typeName or 'Untitled'><brace>"`
  where brace = `""` if braceWrap else `" {"`, typeName = structTypeName or name. Fallback: first root
  Struct. Final fallback: `"struct Untitled<brace>"`.
- Combined = `"[▸] " + row + "  " + row2`. Push to each editor's command row text.
- `emit selectionChanged(m_selIds.size())`.

---

## 7. View root and bookmarks

- `setViewRootId(id)` (`controller.cpp:1859`): if unchanged return; set; `refresh()`.
- `viewRootId()` accessor; `scrollToNodeId(nodeId)` → primary editor `scrollToNodeId`.
- `navigateToFormula(formula, errOut)` (`controller.cpp:6913`): trim; empty → err+false. Build
  `AddressParserCallbacks` (resolveModule via `provider->symbolToAddress`, readPointer via
  `provider->read` of `pointerSize` bytes, resolveIdentifier via `SymbolStore::resolve`).
  `AddressParser::evaluate(f, pointerSize, &cbs)`. On `!ok` → err+false. Else set `baseAddress` +
  `baseAddressFormula`, `emit documentChanged()`, `refresh()`, return true.
- `addBookmark(name, formula)` (`controller.cpp:6946`): trim; if either empty return; append to
  `tree.bookmarks`; `modified=true`; `emit documentChanged()`.
- `removeBookmark(idx)` (`controller.cpp:6956`): bounds-check; remove; `modified=true`; emit.

---

## 8. Source management

- `attachViaPlugin(providerIdentifier, target, registerAsSavedSource=false)` (`controller.cpp:6102`):
  look up provider info in `ProviderRegistry`; if missing → themed warning, return. `createProvider(target,
  &err)`; if null → warn if err, return. `undoStack.clear()`; replace `m_doc->provider`; clear `dataPath`.
  **Don't overwrite baseAddress** (caller may have set it). Adopt `provider->pointerSize()`. If
  `baseAddressFormula` non-empty, re-evaluate it against the new provider (full callbacks incl. kernel
  paging if supported). `resetSnapshot()`. If `registerAsSavedSource`: dedup on (kind, providerTarget),
  update or append a `SavedSourceEntry`, set active index, `pushSavedSourcesToEditors()`.
  `emit documentChanged()`, `refresh()`. *(Plugin path is out of scope; the dedup/saved-source bookkeeping
  is in scope and portable.)*
- `switchToSavedSource(idx)` (`controller.cpp:6210`): bounds-check; no-op if already active. Save the
  current source's `baseAddress`+`baseAddressFormula` into its slot. Set active. If `"File"` → `loadData(filePath)`,
  restore saved base+formula, `refresh()`. Else if `providerTarget` non-empty → restore formula,
  `attachViaPlugin(kind, providerTarget)`, restore saved base if formula empty. `emit documentChanged()`.
- `selectSource(text)` (`controller.cpp:6242`): dispatch on a UI string:
  - `"#clear"` → `clearSources()`.
  - `"#saved:N"` → `switchToSavedSource(N)`.
  - `"File"` → open-file dialog; on a chosen path: save current base into active slot, `loadData(path)`,
    dedup/append a File `SavedSourceEntry`, set active, `emit documentChanged()`, `refresh()`.
  - else → look up provider by `text.toLower().replace(" ","")`; for built-in use `factory`, else plugin
    `selectTarget`; on a chosen target create the provider; on success: save current base, capture
    `newBase = provider->base()` + displayName, `undoStack.clear()`, swap provider, clear dataPath,
    adopt pointerSize, re-evaluate formula OR (only for a fresh/default project where
    `baseAddress == 0x00400000`) adopt `newBase`. `resetSnapshot()`, dedup/append saved source, set active,
    `emit documentChanged()` (twice — once before, once after setting active index, by design), `refresh()`.
- `clearSources()` (`controller.cpp:6398`): clear list, active=-1, `provider = NullProvider`, clear dataPath,
  `resetSnapshot()`, push to editors, `refresh()`.
- `copySavedSources(sources, activeIdx)`: replace list + active, push to editors (cross-tab share).
- `pushSavedSourcesToEditors()` (`controller.cpp:6414`): build `SavedSourceDisplay { text="<kind> '<name>'",
  active }` list, send to each editor.
- `savedSources()`, `activeSourceIndex()`, `switchSource(idx)` (= switchToSavedSource), accessors.

`testSourceSwitchPreservesBase` / `...FreshDocUsesProviderBase`: a non-zero baseAddress is preserved when
attaching a provider with a different base; a fresh doc (base==0) adopts the provider's base. (These
tests assert the *policy*, exercising the same conditional logic.)

`0x00400000` is the default `NodeTree::baseAddress`; the "fresh/default project" check in `selectSource`
keys off this magic value.

---

## 9. Value tracking control

- `trackValues()` accessor; `setTrackValues(on)` (`controller.cpp:1870`): set flag; if turning OFF,
  clear `m_valueHistory`, `m_lastValueAddr`, zero all `m_lastResult.meta[].heatLevel`, `refresh()`.
- `resetChangeTracking()` (`controller.cpp:1881`): clear `m_changedOffsets`, `m_valueHistory`,
  `m_lastValueAddr`, `m_prevPages`; set `m_valueTrackCooldown = 5` (suppress recording for ~5 ticks /
  ~1 s); zero all heat levels. (Does NOT call refresh — caller does.)
- `setReadOnlyOverride(v)` / `readOnlyOverride()`: gate writes off even on writable providers (tutorial safety).
- `setSuppressRefresh(v)`: MCP/batch bridge.

`testClearValueHistoryResetsHeat`: after seeding history and refreshing, the "Clear Value History"
flow (remove node + subtree from history, refresh) zeroes the line's heat; one re-record gives
count==1 → heat 0.
`testDeleteClearsHeatForShiftedNodes`: deleting a node clears the deleted node's history AND, via the
offAdj path, the shifted siblings' history; with a live provider, refresh re-records one value →
count 1 → heat 0.

---

## 10. Tree-mutation helpers (push Commands onto the undo stack)

All of these build `Command`s and `undoStack.push(...)`; most wrap multi-step ops in
`m_suppressRefresh=true` + `beginMacro/endMacro` + a single `refresh()` at the end. Selected ones with
non-trivial algorithms:

- `renameNode(idx, name)` → push `Rename`.
- `changeNodeKind(idx, newKind)` (`controller.cpp:2078`): compute `oldSize` (structSpan for containers),
  `newSize` (0 when converting to a container — size resolved later by `applyTypePopupResult`).
  - **Shrink** (`0 < newSize < oldSize`): macro "Change type". Capture origName/origOffset/needsRename
    **before** the ChangeKind push (push mutates the node in place). Push `ChangeKind{...,{}}`. If
    `needsRename` (hex→non-hex) push `Rename` to `"field_<offset hex4>"`. Then fill the gap with hex pads:
    hex→hex uses same-size pads (reversible join); otherwise largest-first (8/4/2/1). Each pad via
    `insertNode(parentId, padOffset, padKind, "pad_<offset>")`.
  - **Same/grow** (`delta = newSize - oldSize`): if `delta!=0 && oldSize>0 && newSize>0`, shift siblings
    at `offset >= node.offset+oldSize` by `delta` (build `offAdjs`). Optional rename macro for hex→non-hex.
    Push `ChangeKind{..., offAdjs}` (+ Rename inside macro).
  Tests: same-size (no pad), shrink (pad after), grow (siblings shift).
- `insertNode(parentId, offset, kind, name)` (`controller.cpp:2459`): if `offset<0`, auto-place after last
  sibling (max end, aligned up to `alignmentFor(kind)`). Reserve id; push `Insert{n}` (no offAdjs).
- `insertNodeAbove(beforeIdx, kind, name)` (`controller.cpp:2488`): new node at `before.offset`; shift
  every sibling with `offset >= before.offset` down by `sizeForKind(kind)` (offAdjs); push `Insert{n, adjs}`.
- `removeNode(idx)` (`controller.cpp:2513`): compute deletedSize (structSpan/byteSize), deletedEnd; for
  siblings with `offset >= deletedEnd` build offAdjs shifting them up by deletedSize (only if parent!=0).
  Collect subtree node copies; push `Remove{nodeId, subtree, adjs}`.
- `batchRemoveNodes(indices)` / `batchChangeKind(indices, kind)`: macro-wrapped loops.
- `deleteRootStruct(structId)` (`controller.cpp:2547`): only if it's a root Struct. Macro: null out every
  `refId == structId` (ChangePointerRef → 0), then re-lookup index and `removeNode`. If the deleted root
  was the view root, switch the view to the first remaining root Struct.
- `toggleCollapse(idx)` → push `Collapse`.
- `materializeRefChildren(idx)` (`controller.cpp:2737`): if node has a refId and no own children, clone
  the ref struct's children (new ids, parented to this node, collapsed); macro of `Insert`s; auto-expand
  the self-referential clone (one Collapse to false).
- `duplicateNode(idx)`, `splitHexNode`, `joinHexNodes`, `groupIntoUnion`, `dissolveUnion`,
  `extractByteSelectionToNewClass`, `convertToTypedPointer`, `attachRttiClassToPointer`, `toggleBitfieldBit`,
  `editBitfieldValue`, `insertStaticField` — all build macros of the above command types. (Their internal
  geometry is layout-detail; the porting-relevant pattern is "macro + suppressRefresh + final refresh".)
- `convertToTypedPointer(nodeId)` (`controller.cpp:3181`): generate unique name `NewClass`/`NewClass_N`;
  create a root `class`-keyword struct named that + `kDefaultFields(16)` hex children (Hex32/4 if 32-bit
  else Hex64/8); macro "Change to ptr*": changeNodeKind to Pointer (if needed), Insert struct, Insert
  children, ChangePointerRef to the new struct. Tested by `testConvertToTypedPointer`.
- `findOrCreateStructByName(typeName, depth=0)` (`controller.cpp:6037`): if a matching root struct exists,
  return its id. Else macro "Import type": build a root struct (name "instance"); if `findCommonType` knows
  the layout, insert its fields (recursing for pointer targets, depth-capped at 8); else 8 default Hex64
  fields. Returns the root id. `m_suppressRefresh` toggled around it.
- `applyTypePopupResult(mode, nodeIdx, entry, fullText)` (`controller.cpp:5789`): applies a type-picker
  selection (root keyword, field type, array element type, pointer target). Pushes the relevant Commands
  and post-adjusts sizes when converting to containers. Also `pushRecentType(displayName)` (cap 8,
  most-recent-first dedup). Largely UI-glue but the command emission is in scope.

---

## 11. Qt usage → Rust equivalents

| Qt type / API | Use here | Rust equivalent |
|---|---|---|
| `QObject` / signals & slots | controller + document are QObjects; signals notify UI | Trait + callback/closure, or a channel/event bus; for the port define controller as a plain struct with a list of observer callbacks or an event enum |
| `QTimer` | periodic refresh tick | `tokio`/`async-std` interval, or a winit/egui timer, or a simple "next deadline" the UI loop polls; must support stop + dynamic interval |
| `QFutureWatcher<PageMap>` + `QtConcurrent::run` | async page reads off the UI thread, completion on UI thread | `std::thread`/`rayon` + channel; the result `HashMap<u64, Vec<u8>>` sent back to the UI loop. Provider must be `Send + Sync` for reads |
| `QUndoStack` / `QUndoCommand` | undo/redo, macros, clean state, `setObsolete` | Custom undo stack of `Command`; macro = group; "clean index" for dirty tracking; "obsolete" = drop |
| `QHash<K,V>` | page maps, value history, caches | `HashMap` / `FxHashMap` |
| `QSet<T>` | selection ids, visited sets, changed offsets | `HashSet` |
| `QVector<T>` / `QList<T>` | nodes, ranges, editors | `Vec` |
| `QByteArray` | page bytes, value bytes | `Vec<u8>` / `Bytes` |
| `QString` | names, formulas, labels | `String` |
| `QJsonObject/Array/Document` | `.rcx` save/load | `serde_json` |
| `QSettings("Reclass","Reclass")` | refresh interval, editor font | `confy` / a config file; key `refreshMs` default 200 |
| `QPointer<T>` | non-owning popup refs | `Weak<RefCell<>>` or just option-of-handle (UI) |
| `QFile`/`QDir`/`QFileInfo` | load/save, relative path resolution | `std::fs`, `std::path` |
| `std::variant<...>` (Command) | command set | `enum Command { ... }` |
| `std::shared_ptr<Provider>` | shared ownership of source across doc + snapshot + worker | `Arc<dyn Provider + Send + Sync>` |
| `std::unique_ptr<SnapshotProvider>` | controller-owned snapshot | `Option<Box<SnapshotProvider>>` |
| `thread_local s_composeDoc` + `ComposeDocGuard` | type-name resolution during compose | pass the doc/alias map explicitly into compose (avoid thread-local) |
| `QtConcurrent` exception catch in `onReadComplete` | guard worker panics | `catch_unwind` / `Result` from the worker channel |
| `Qt::KeyboardModifiers` (Ctrl/Shift) | selection modifiers | a small `Modifiers { ctrl, shift }` struct |
| `QPoint` | popup positions | `(i32,i32)` (UI) |

---

## 12. Concurrency / threading model

- One worker thread per refresh round (`QtConcurrent::run`), guarded by `m_readInFlight` so at most one
  is in flight. The worker captures a `shared_ptr<Provider>` by value and a list of page addresses; it
  only **reads** (`readBytes`) — never mutates controller state. It returns a fresh `PageMap`.
- Completion (`onReadComplete`) runs on the UI thread (Qt queues the `finished` signal). All controller
  state mutation (snapshot merge, diff, history, recompose) happens there. **No locking is needed** because
  the worker never touches controller state and the completion is serialized onto the UI thread.
- **Generation guard** (`m_refreshGen` / `m_readGen`): any structural mutation that invalidates layout
  bumps `m_refreshGen` (ChangeKind/ChangeOffset/ChangeBase via resetSnapshot, and `clearHistoryForAdjs`).
  `onReadComplete` discards a result whose `m_readGen != m_refreshGen`. The Rust port must replicate this
  to avoid recording stale-layout values after an edit during an in-flight read.
- Destructor cancels + joins the worker before tearing down the snapshot.
- For Rust: the `Provider` trait used by the snapshot worker must be `Send + Sync` (reads are `&self`).
  `BufferProvider`/`SnapshotProvider`/`NullProvider` are all read-shareable. Writes go only through the
  UI thread (`applyCommand`/`setNodeValue`), so interior mutability for `write` can stay non-`Send` if the
  worker only ever reads — but since the same `Arc<dyn Provider>` is shared, the simplest model is a
  `Provider` whose `read` is `&self` and `write` takes `&self` with interior mutability (the C++ `write`
  is non-const, so model state changes via `RefCell`/`Mutex` or split read/write traits).

---

## 13. Serialization formats

`.rcx` is JSON (`QJsonDocument::Indented`). Top-level object:
- `tree.toJson()` fields: `baseAddress` (hex string), optional `baseAddressFormula`, optional
  `initialClass`, optional `pointerSize` (omitted when 8), `nextId` (string), `nodes` (array of Node
  json), optional `bookmarks` (array).
- `typeAliases` (object: kindName → alias) — added by `RcxDocument::save`.
- `savedSources` (array) — written by the UI elsewhere; **read** by `RcxDocument::load` into
  `pendingSavedSources` and lifted by the controller. Each entry: `kind`, `displayName`, `filePath`,
  `providerTarget`, `baseAddress` (hex string), `baseAddressFormula`.
- Node json: see `Node::toJson`/`fromJson` (`core.h:263-354`). Notably: ids are decimal strings;
  `kind`/`elementKind` are the `KindMeta::name`; defaults omitted; on load `collapsed` is **always true**;
  `arrayLen` clamped `[1, kMaxArrayLen]`, `strLen` `[1, 1000000]`, `ptrDepth` `[0,2]`; legacy `isHelper`
  read as fallback for `isStatic`.

Load is forgiving: empty file → empty project; non-JSON non-empty → refuse (false). After parse,
`validate(repair=true)` and `findOverlaps()` run. The Rust port should use serde with the same field
names, hex-string encoding for addresses/ids, and replicate the omit-defaults behavior for round-trip
parity (tested by `testNodeToJsonOmitsDefaults`, `testNodeToJsonIncludesIsRelative`).

---

## 14. Platform-specific code

The controller core is **mostly portable**. Platform/OS-specific concerns:
- The live process/kernel/WinDbg providers (out of scope) sit behind `ProviderRegistry` + `IPlugin`;
  the controller never names them. Built-in benign sources (File/buffer/snapshot/null) are
  cross-platform.
- `m_ownedBuffer` + `m_readOnlyOverride` exist for the Windows "attach to self" / live-self tutorial
  (RPM/WPM into our own process). On Linux this path is inert; keep the fields behind the abstract
  provider. No `#ifdef` in controller.cpp itself.
- `QSettings("Reclass","Reclass")` maps to the platform registry/ini; replace with a config file.
- Everything else (refresh logic, value history, command application, selection, source bookkeeping)
  is pure logic over `Provider` + `NodeTree` and ports without `cfg`.

---

## 15. Subtle behaviors the tests rely on (checklist for parity)

1. **ValueHistory dedup + heat thresholds** exactly as in §2.8 (`testValueHistoryRingBuffer`).
2. **Heat propagation through refresh**: heat is recorded only for live providers, non-container,
   non-FuncPtr, `Field` lines, and only when `trackValues && cooldown<=0`. Address-change clears history.
3. **Clearing heat**: `setTrackValues(false)`, `resetChangeTracking()` (cooldown=5), and the
   delete/offset-shift `clearHistoryForAdjs` path all zero heat; a single subsequent record yields heat 0
   (`testClearValueHistoryResetsHeat`, `testDeleteClearsHeatForShiftedNodes`).
4. **WriteBytes is tested first**, only pushed on success; undo restores old bytes, redo re-applies
   (`testSetNodeValue*`). `readOnlyOverride` blocks writes silently.
5. **changeNodeKind geometry**: same-size = no pad; shrink = hex pads after; grow = siblings shift;
   hex→non-hex auto-renames to `field_<offset hex4>`; the rename condition is captured **before** the
   ChangeKind push (`testQuickTypeChangeHex*`).
6. **insertNodeAbove / removeNode offset shifts** are reversible via offAdjs (`testInsertNodeAboveShiftsOffsets`).
7. **toggleCollapse round-trips** through undo (`testToggleCollapse`).
8. **Refresh speedups** (`test_refresh_speedups.cpp`):
   - Permanent pages: executable module pages marked permanent after first read, never re-read.
   - Collapsed pointers: target pages never requested.
   - Stability: a heap page's stability counter climbs (>=1) when bytes are constant.
   - Viewport bounding: far-end (off-screen) pages skipped on tick 2+.
   - Adaptive: idle backoff widens interval past base after ~8 idle ticks; blur → 1500 (active);
     `visible=false` → timer stops; restore → resumes at base.
   - SnapshotProvider primitives: `markPermanent` page-aligns; `mergePages` preserves un-merged pages.
9. **All-zero page-0 guard** in `onReadComplete`: a fresh all-zero page 0 is discarded if we already
   have data (process-vanished protection).
10. **Generation guard** discards stale async reads after layout-changing edits.
11. **Selection bit decoration** + the strip mask must match so footer/array/member selections survive
    refresh pruning and resolve back to node indices.
12. **Source-switch base-address policy**: preserve non-zero base; fresh doc (base 0 / default 0x00400000)
    adopts provider base.
