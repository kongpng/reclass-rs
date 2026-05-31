# Subsystem: Value/Pattern Search Engine (`scanner`)

Source of truth: `/home/loke/reclass-cpp/src/scanner.h` (174 lines), `/home/loke/reclass-cpp/src/scanner.cpp` (1138 lines).
Tests: `/home/loke/reclass-cpp/tests/test_scanner.cpp` (3004 lines), `/home/loke/reclass-cpp/tests/test_scanner_combinations.cpp` (377 lines).
Depends on: `/home/loke/reclass-cpp/src/providers/provider.h` (`Provider`, `MemoryRegion`, `RegionType`).

Expected portability: **pure** for the algorithm core. The async/threading + Qt-signal wiring is UI-adjacent and should be re-shaped idiomatically in Rust (see "Concurrency & API shape" below), but the *search/compare/aggregation algorithms over `&[u8]` are pure and must be ported byte-for-byte*.

---

## 1. Purpose

A Cheat-Engine-style generic search engine that scans byte buffers/regions (served by the abstract `Provider`/data source) for values and patterns. It supports:

- **Pattern parsing** (IDA-style signatures + C-style `\xAB`, with wildcards) → `(pattern, mask)`.
- **Typed value serialization** (int8..int64, uint8..uint64, float, double, vec2/3/4, UTF-8, UTF-16LE, hex bytes) → little-endian `pattern` bytes + all-`0xFF` mask.
- **First scan** (`runScan`): walks accepted memory regions, reads them in chunks via the provider, and finds matches via three code paths — capture-everything, inline typed-constant compare, and pattern matching (Boyer-Moore-Horspool when eligible, otherwise a naive masked matcher).
- **Rescan / refine** (`runRescan`): re-reads the addresses of a prior result set and filters them by ExactValue / Changed / Unchanged / Increased / Decreased / BiggerThan / SmallerThan / Between / IncreasedBy / DecreasedBy.
- **Region filtering**: executable-only, writable-only, private-only (skip Image/Mapped), skip-system-modules, address range cap, and multi-range constraint intersection.
- **Bookkeeping**: progress %, region-resolved counts, scan statistics, abortion, and a region-list cache.

All comparisons are little-endian and operate directly on raw provider bytes.

---

## 2. Key types / structs / enums

### `enum class ValueType` — `scanner.h:15-22`
Variants (in order, so the integer discriminant matters for the UI but the engine switches on the variant):
`Int8, Int16, Int32, Int64, UInt8, UInt16, UInt32, UInt64, Float, Double, Vec2, Vec3, Vec4, UTF8, UTF16, HexBytes`.
Used for typed compares (BiggerThan/SmallerThan/Between/IncreasedBy/DecreasedBy), serialization, natural alignment, and value byte-size.

### `enum class ScanCondition` — `scanner.h:26-38`
Cheat-Engine-style conditions, with exact semantics annotated in the header:
- `ExactValue` — first scan + rescan: match specific bytes.
- `UnknownValue` — first scan only: capture all aligned addresses.
- `Changed` — rescan: current != previous.
- `Unchanged` — rescan: current == previous.
- `Increased` — rescan: current > previous (numeric).
- `Decreased` — rescan: current < previous (numeric).
- `BiggerThan` — first scan + rescan: current > constant (typed).
- `SmallerThan` — first scan + rescan: current < constant (typed).
- `Between` — first scan + rescan: lo <= current <= hi (typed).
- `IncreasedBy` — rescan: current == previous + delta.
- `DecreasedBy` — rescan: current == previous - delta.

### `struct AddressRange` — `scanner.h:42-45`
- `uint64_t start = 0`
- `uint64_t end = 0` — **exclusive**.

### `struct ScanRequest` — `scanner.h:47-75`
| Field | Type | Default | Meaning |
|---|---|---|---|
| `pattern` | `QByteArray` | empty | Literal bytes to match (empty for UnknownValue). Also the lower/sole bound for typed-const compares, and the delta for IncreasedBy/DecreasedBy on first scan. |
| `mask` | `QByteArray` | empty | `0xFF`=must match, `0x00`=wildcard. Must equal `pattern.size()` for non-UnknownValue (validated in `start`). |
| `filterExecutable` | `bool` | `false` | Only scan `+x` regions. |
| `filterWritable` | `bool` | `false` | Only scan `+w` regions. |
| `privateOnly` | `bool` | `false` | Skip Image (DLL) + Mapped (file) regions (i.e. keep only `RegionType::Private`). |
| `skipSystemModules` | `bool` | `false` | Skip well-known system DLLs by `region.moduleName`. |
| `alignment` | `int` | `1` | 1=every byte, 4=dword, 8=qword. Clamped to >=1 via `qMax(1, alignment)`. |
| `maxResults` | `int` | `50000` | Hard cap on results; scan stops once reached. |
| `condition` | `ScanCondition` | `ExactValue` | Drives which scan path runs. |
| `valueSize` | `int` | `4` | Bytes per value for capture/unknown scans and typed-const compare value width. |
| `valueType` | `ValueType` | `Int32` | Used by typed compares. |
| `pattern2` | `QByteArray` | empty | Upper bound for Between; delta for IncreasedBy/DecreasedBy (on first scan paths; note first-scan IncreasedBy/DecreasedBy are treated as capture, see §6). |
| `startAddress` | `uint64_t` | `0` | `0` = no lower limit. |
| `endAddress` | `uint64_t` | `0` | `0` = no upper limit. |
| `constrainRegions` | `QVector<AddressRange>` | empty | If non-empty, only scan within these ranges (intersected with provider regions). |

### `struct ScanResult` — `scanner.h:77-82`
- `uint64_t address = 0` — absolute address of the match.
- `QString regionModule` — formatted region context (`"modulename+0xOFFSET"` or empty), via `formatRegionContext`.
- `QByteArray scanValue` — cached bytes at scan/update time.
- `QByteArray previousValue` — value before last update (set at the start of each rescan).

### `struct ScanStats` — `scanner.h:85-90`
- `int regionsScanned` — count of accepted regions.
- `uint64_t bytesScanned` — bytes advanced during the scan.
- `uint64_t bytesFailed` — sum of unreadable chunk sizes.
- `int msElapsed` — wall-clock elapsed ms.

### `MemoryRegion` — `providers/provider.h:19-29` (consumed, not owned here)
- `uint64_t base = 0`, `uint64_t size = 0`
- `bool readable = true`, `bool writable = false`, `bool executable = false`
- `QString moduleName`
- `RegionType type = RegionType::Private`
Region end = `base + size` (exclusive).

### `enum class RegionType : uint8_t` — `providers/provider.h:13-17`
`Image=0` (loaded module: code+rdata+data), `Mapped=1` (memory-mapped file / shared section), `Private=2` (heap/stack/VirtualAlloc).

### `class ScanEngine : public QObject` — `scanner.h:113-169`
Private state:
- `std::atomic<bool> m_abort{false}` — cooperative abort flag.
- `QFutureWatcher<QVector<ScanResult>>* m_watcher = nullptr` — async run handle.
- `mutable QVector<MemoryRegion> m_cachedRegions` — region cache.
- `mutable const Provider* m_cachedProvider = nullptr` — cache key (raw provider pointer).

`Q_DECLARE_METATYPE(QVector<rcx::ScanResult>)` and `Q_DECLARE_METATYPE(rcx::ScanStats)` at `scanner.h:173-174` register these for queued cross-thread signals (no Rust analog needed).

---

## 3. Free functions (public API)

### `bool parseSignature(const QString& input, QByteArray& pattern, QByteArray& mask, QString* errorMsg = nullptr)` — `scanner.cpp:128-214`
Parses an IDA-style or C-style signature into `(pattern, mask)`. Clears both out params first.

Behavior, in order:
1. `trimmed = input.trimmed()`. If empty → set `errorMsg = "Empty pattern"`, return `false`. (Test: `parse_emptyPattern`, `parse_spacesOnly`.)
2. **C-style** if `trimmed.startsWith("\\x")`: split on literal `"\\x"` with `SkipEmptyParts`. Each part must be exactly 2 chars; each char a valid hex nibble (via `hexVal`). On bad width → `"Invalid C-style byte: \\x<part>"`; on bad hex → `"Invalid hex char in: \\x<part>"`. Each byte → `pattern += (hi<<4)|lo`, `mask += 0xFF`. Returns `!pattern.isEmpty()`. (Test: `parse_cStyle`.)
3. Otherwise: `hasSpaces = trimmed.contains(' ')`.
   - **Space-separated**: split on `' '` with `SkipEmptyParts`. For each token:
     - `"??"` or `"?"` → wildcard: `pattern += 0x00`, `mask += 0x00`.
     - exactly 2 chars → hex byte; bad hex → `"Invalid hex byte: <tok>"`.
     - else → `"Invalid token: <tok> (expected 2 hex chars or wildcards)"`.
     (Tests: `parse_spaceSeparated`, `parse_withWildcards`, `parse_singleQuestionMark`, `parse_invalidTokenWidth`, `parse_leadingTrailingSpaces`, `parse_allWildcards`, `parse_lowercaseHex`, `parse_mixedCase`, `parse_invalidHex`.)
   - **Packed (no spaces)**: if `trimmed.size() % 2 != 0` → `"Odd number of characters in packed pattern"` (Test: `parse_oddCharsNoSpaces`). Walk by pairs `(c0,c1)`:
     - both `'?'` → wildcard (`0x00`/`0x00`).
     - else hex byte; bad → `"Invalid hex chars at position <i>: <c0><c1>"`.
     Note: only `"??"` is a wildcard in packed mode (a single `'?'` paired with a hex digit fails `hexVal`). (Test: `parse_packedNoSpaces`, `parse_singleByte` "AB".)
4. If `pattern.isEmpty()` after parsing → `"Empty pattern after parsing"`, return `false`.
5. Return `true`.

`hexVal(QChar)` — `scanner.cpp:120-126`: maps `0-9`,`a-f`,`A-F` → 0..15, else `-1`. Case-insensitive.

### `bool serializeValue(ValueType type, const QString& input, QByteArray& pattern, QByteArray& mask, QString* errorMsg = nullptr)` — `scanner.cpp:223-428`
Serializes a typed value to little-endian bytes for exact-match scanning. Clears out params first. `trimmed = input.trimmed()`; if empty → `"Empty value"`, return false (Test: `serialize_emptyValue`).

Helper `appendLE<T>` (`scanner.cpp:218-221`) appends `sizeof(T)` host-endian bytes — host is little-endian on all targets, so this is LE.

Per type (uses Qt's `QString::toInt/toUInt/toLongLong/toULongLong/toULong/toFloat/toDouble` with an `ok` flag, base 10 by default):
- `Int8`: `toInt`; range check `-128..127`; else `"Invalid int8 value"`. (Tests: `serialize_int8`, `serialize_int8_overflow`.)
- `Int16`: `toInt`; range `-32768..32767`; else `"Invalid int16 value"`. (Test: `serialize_int16`.)
- `Int32`: `toInt`; else `"Invalid int32 value"`. (Tests: `serialize_int32`, `serialize_int32_negative`, `serialize_invalidInt` "notanumber".)
- `Int64`: `toLongLong`; else `"Invalid int64 value"`. (Test: `serialize_int64`.)
- `UInt8`: `toUInt`; if fail OR >255, *and* input starts with `0x`/`0X`, retry `toUInt(&ok,16)`; if still fail or >255 → `"Invalid uint8 value"`. (Tests: `serialize_uint8`, `serialize_uint8_hex`.)
- `UInt16`: same pattern, range >65535 → `"Invalid uint16 value"`. (Test: `serialize_uint16`.)
- `UInt32`: `toULong`; if fail and `0x`-prefixed, retry `toULong(&ok,16)`; else `"Invalid uint32 value"`. (Tests: `serialize_uint32` "0xDEADBEEF".)
- `UInt64`: `toULongLong`; retry hex if `0x`-prefixed; else `"Invalid uint64 value"`. (Test: `serialize_uint64` "0xCAFEBABEDEADBEEF".)
- `Float`: `toFloat`; else `"Invalid float value"`. (Tests: `serialize_float`, `serialize_invalidFloat`.)
- `Double`: `toDouble`; else `"Invalid double value"`. (Test: `serialize_double`.)
- `Vec2/Vec3/Vec4`: split on regex `\s+` with `SkipEmptyParts`; require exactly 2/3/4 parts else `"VecN requires N space-separated floats"`; each part `toFloat`; bad → `"Invalid float in vecN: <p>"`. Output is N consecutive LE floats. (Tests: `serialize_vec2`, `serialize_vec3`, `serialize_vec3_wrongCount`, `serialize_vec4`.)
- `UTF8`: `pattern = trimmed.toUtf8()`; empty → `"Empty UTF-8 string"`. No NUL terminator. (Test: `serialize_utf8`.)
- `UTF16`: for each char, append `ushort unicode()` as LE uint16; empty → `"Empty UTF-16 string"`. UTF-16LE, no BOM, no terminator. (Test: `serialize_utf16`.)
- `HexBytes`: delegates to `parseSignature(trimmed, pattern, dummyMask, errorMsg)`; wildcards in the input are technically parsed but the comment marks HexBytes as exact-match. (Test: `serialize_hexBytes` "DE AD BE EF".)

At the end (`scanner.cpp:426`): `mask.fill(0xFF, pattern.size())` — overwrites the mask to all-`0xFF` of `pattern.size()` length (so even HexBytes wildcards get a `0xFF` mask). Returns `true`.

**Rust note:** Qt's `toInt`/`toFloat` accept surrounding whitespace already trimmed; they reject trailing garbage. `toUInt`/`toULong` for unsigned types also accept plain decimal. Mirror with `str::parse` plus an explicit `0x`-hex fallback for the unsigned types only. Qt `toFloat("3.14")` yields the nearest f32; the test asserts exact `3.14f` round-trip, so use Rust `f32::from_str`.

### `int naturalAlignment(ValueType type)` — `scanner.cpp:430-454`
Returns the default alignment for a value type: `Int8/UInt8/UTF8/HexBytes → 1`; `Int16/UInt16/UTF16 → 2`; `Int32/UInt32/Float/Vec2/Vec3/Vec4 → 4`; `Int64/UInt64/Double → 8`; default `1`. (Tests `alignment_*`.)

### `int valueSizeForType(ValueType type)` — `scanner.cpp:456-467`
`Int8/UInt8 → 1`; `Int16/UInt16 → 2`; `Int32/UInt32/Float → 4`; `Int64/UInt64/Double → 8`; `Vec2 → 8`; `Vec3 → 12`; `Vec4 → 16`; default `4` (covers UTF8/UTF16/HexBytes). Used by the combinations test to size synthetic buffers.

---

## 4. Internal helpers

### `static QString formatRegionContext(const MemoryRegion& region, uint64_t address)` — `scanner.cpp:22-28`
If `region.moduleName.isEmpty()` → returns empty `QString`. Else `off = (address >= base) ? address - base : 0`, returns `"<moduleName>+0x<off-in-lowercase-hex>"` (no leading zeros; `%2` arg `off, 0, 16`). Set on every `ScanResult.regionModule`.
Tests assert exact strings: `"code+0x0"`, `"region0+0x4"`, `"Game.exe+0x0"`. **Rust:** `format!("{}+0x{:x}", module, off)`.

### `static int compareTyped(const QByteArray& a, const QByteArray& b, ValueType vt)` — `scanner.cpp:471-512`
Three-way compare (`-1/0/1`) of two byte buffers interpreted as a typed numeric value. `sz = min(a.size, b.size)`. For each numeric `vt` with enough bytes, `memcpy` both into the native type and return `(va>vb)-(va<vb)`:
- Int8/UInt8 (sz>=1), Int16/UInt16 (>=2), Int32/UInt32 (>=4), Int64/UInt64 (>=8), Float (>=4), Double (>=8).
- **Default / not-enough-bytes fallback** (`scanner.cpp:511`): `memcmp(da, db, sz)` (raw byte compare). This fallback also applies to Vec*/UTF*/HexBytes value types and is used by `Changed`/`Unchanged` for byte sequences. **Rust:** for the byte fallback, return the sign of `a[..sz].cmp(&b[..sz])` (lexicographic unsigned).
- Float/double NaN: `(va>vb)-(va<vb)` yields `0` when either is NaN (both comparisons false). This matches IEEE partial-order semantics; replicate with `partial_cmp(...).map(Ordering as i32).unwrap_or(0)` or the explicit `(a>b) as i32 - (a<b) as i32` form.

### `static int hexVal(QChar)` — `scanner.cpp:120-126` (described above).
### `template<typename T> appendLE` — `scanner.cpp:218-221` (described above).

---

## 5. `ScanEngine` public methods

### `ScanEngine(QObject* parent = nullptr)` — `scanner.cpp:516-520`
Registers `QVector<ScanResult>` metatype. No Rust analog.

### `bool isRunning() const` — `scanner.cpp:522-524`
`m_watcher && m_watcher->isRunning()`.

### `void abort()` — `scanner.cpp:526-528`
`m_abort.store(true)`. Cooperative; the worker checks it periodically (see §6 abort cadence). Test `scan_abort` asserts the scan still emits `finished` exactly once after abort.

### `void invalidateRegionCache()` — `scanner.cpp:113-116`
Clears `m_cachedRegions` and nulls `m_cachedProvider`. Test `regionCache_reusesAcrossScans` proves: 1st scan enumerates (count 1), 2nd reuses (still 1), after `invalidateRegionCache` a 3rd re-enumerates (count 2).

### `void start(std::shared_ptr<Provider> provider, const ScanRequest& req)` — `scanner.cpp:530-560`
Front door for a first scan.
1. If `isRunning()` → return silently.
2. **Validation (synchronous, before any thread):** if `condition != UnknownValue`:
   - `pattern.isEmpty()` → `emit error("Empty pattern")`, return. (Test `scan_emptyPattern`.)
   - `pattern.size() != mask.size()` → `emit error("Pattern and mask size mismatch")`, return. (Test `scan_maskSizeMismatch`.)
3. `m_abort = false`; create `QFutureWatcher`, store in `m_watcher`. On `finished`: grab `watcher->result()`, `deleteLater`, null `m_watcher` if it's still this watcher, then `emit finished(results)`.
4. Launches `QtConcurrent::run([...] { return runScan(provider, req); })`.

### `void startRescan(std::shared_ptr<Provider> provider, QVector<ScanResult> results, int readSize, ScanCondition condition = ExactValue, ValueType valueType = Int32, const QByteArray& filterPattern = {}, const QByteArray& filterMask = {}, const QByteArray& filterPattern2 = {})` — `scanner.cpp:919-947`
Front door for a refine/rescan.
1. If `isRunning()` → return.
2. `m_abort=false`; new watcher → `m_watcher`. On finished → `emit rescanFinished(results)` (note: distinct signal from `finished`).
3. Launches `QtConcurrent::run` capturing `results` by move, calling `runRescan(...)`.

### Static helpers (also test-facing):

#### `static bool isSystemModule(const QString& moduleName)` — `scanner.cpp:35-79`
Case-insensitive membership test against a hard-coded `QSet<QString> kSystem` of well-known system module **stems** (extension-stripped, lowercased).
- Empty name → `false`.
- `name = moduleName.trimmed().toLower()`.
- Strip extensions iteratively from `stem` (a copy of `name`): find first `'.'` at index `dot>0`; if the suffix after it is `dll`/`exe`/`dylib`/`so`, **or** a numeric suffix of length <= 3 with `toInt()>0` (e.g. the `.6` in `libc.so.6`), chop at `dot` and repeat; else stop. So `"kernel32.dll" → "kernel32"`, `"libc.so.6" → "libc.so"` (the `.6` is stripped, then `.so` is NOT a sub-suffix because `stem` is now `libc.so` and stripping `.so` leaves `libc` — wait: the loop strips `.6` then re-finds `.` → suffix `so` → strips → `libc`; **but** `kSystem` contains both `"libc.so"` and `"libc.so.6"`, so either way matches).
- Returns `true` if `kSystem.contains(stem)` **or** `kSystem.contains(name)` (the full lowercased name with extension is also checked, which is how multi-dot Linux/macOS entries like `"libc.so.6"`, `"ld-linux-x86-64.so"`, `"libc++.1.dylib"` match).

The set (`scanner.cpp:37-61`) — **port verbatim**:
- Windows core: `kernel32, kernelbase, ntdll, win32u, user32, gdi32, gdi32full, advapi32, shell32, shlwapi, shcore, combase, ole32, oleaut32, rpcrt4, sechost, sspicli, msvcrt, ucrtbase, msvcp140, vcruntime140, vcruntime140_1, msvcp_win, bcrypt, bcryptprimitives, cryptbase, crypt32, imm32, dwmapi, uxtheme, comdlg32, comctl32, winmm, ws2_32, iphlpapi, wininet, winhttp, psapi, version, wldap32, secur32, msasn1, wintrust, kernel.appcore, twinapi, twinapi.appcore, windows.storage, wintypes, profapi, dnsapi, userenv, setupapi, cfgmgr32, devobj, powrprof, atl, atl120, atl140, msvcr120, msvcp120`.
- Qt6: `qt6core, qt6gui, qt6widgets, qt6concurrent, qt6network, qt6printsupport, qt6svg, qt6dbus, qt6xml`.
- Linux: `ld-linux-x86-64.so, libc.so, libc.so.6, libdl.so, libpthread.so, librt.so, libm.so, libstdc++.so, libgcc_s.so`.
- macOS: `libsystem_kernel.dylib, libsystem_c.dylib, libsystem_pthread.dylib, libsystem_malloc.dylib, libsystem_platform.dylib, libc++.1.dylib, libobjc.A.dylib, dyld`.

Tests `sysmod_*` cover: empty→false; `kernel32.dll`/`kernel32`→true; case-insensitive (`KERNEL32.DLL`, `Kernel32`, `nTdLl.dll`); Qt (`Qt6Core.dll`, `qt6gui`, `qt6widgets.dll`); CRT (`ucrtbase.dll`, `msvcrt`, `vcruntime140`); user binaries NOT system (`Reclass.exe`, `MyGame.exe`, `custom.dll`, `game_x64.exe`); Linux (`libc.so.6`, `ld-linux-x86-64.so`). Note `game_x64.exe` must be NON-system: extension `.exe` strips to `game_x64`, not in set.

#### `static int bmhFind(const char* data, int len, const char* pat, int patLen)` — `scanner.cpp:86-111`
Boyer-Moore-Horspool first-match search. Returns offset into `[data, data+len)` or `-1`.
1. `patLen <= 0 || patLen > len` → `-1`.
2. `patLen == 1` → `memchr(data, (unsigned char)pat[0], len)` result or `-1`.
3. Build `shift[256]`, all initialized to `patLen`; then for `i` in `0..patLen-1`: `shift[(unsigned char)pat[i]] = patLen-1-i` (last occurrence wins for the bad-char table over `pat[0..patLen-2]`; the final byte is intentionally excluded).
4. `last = patLen-1`; `i=0`; while `i <= len-patLen`: read `tail = data[i+last]`; if `tail == pat[last]`, verify `data[i+j]==pat[j]` for `j` in `0..last`; if all match return `i`. Then `i += shift[tail]` (shift keyed on the text tail byte).
Tests: `bmh_singleByte`, `bmh_longPattern`, `bmh_patternEqualsLength`, `bmh_patternLargerThanData`, `bmh_atEnd`, and the property test `bmh_equivalentToNaive` (random data, patterns length 4..16, must equal naive first-match). **Rust:** signature becomes `fn bmh_find(data: &[u8], pat: &[u8]) -> Option<usize>`; preserve the exact shift logic.

---

## 6. `runScan` — first-scan algorithm — `scanner.cpp:562-917`

Returns `QVector<ScanResult>`. Runs on a worker thread.

### Setup & mode classification
- Start `QElapsedTimer`.
- `isCapture` (`scanner.cpp:573-579`) = condition in {`UnknownValue, Changed, Unchanged, Increased, Decreased, IncreasedBy, DecreasedBy`}. Rationale: compare-against-previous conditions have no baseline on a first scan, so they capture every aligned address; the actual filter runs on rescan.
- `isTypedConst` (`scanner.cpp:582-584`) = condition in {`BiggerThan, SmallerThan, Between`}. These filter inline during capture.
- Early returns: `!prov` → empty. If not capture/typed-const and `pattern.isEmpty()` → empty. If typed-const and `pattern.isEmpty()` → empty. (`scanner.cpp:586-590`.)

### Region acquisition (`scanner.cpp:594-620`)
- **Cache:** if `m_cachedProvider == prov.get()` and `!m_cachedRegions.isEmpty()`, reuse `m_cachedRegions`; else call `prov->enumerateRegions()` and store into cache + `m_cachedProvider = prov.get()`.
- **Fallback:** if `regions.isEmpty()` after enumeration, synthesize one region `{base=0, size=prov->size(), readable=true, writable=true, executable=true, type=Private}`. (executable=true so exec filter doesn't exclude the only region.) This is the path for `BufferProvider` with empty data and `NullProvider` (size 0 → totalBytes 0 → early return, 0 results: tests `scan_emptyProvider`, `provider_nullProviderRegionsEmpty`). Non-empty `BufferProvider` returns a real `Mapped`-type region named `[buffer]`/its name, so the fallback usually doesn't trigger for buffer scans (Test `provider_defaultRegionsEmpty`).

### Derived locals (`scanner.cpp:622-639`)
- `patternLen = (isCapture || isTypedConst) ? req.valueSize : req.pattern.size()`.
- `pat = isCapture ? nullptr : req.pattern.constData()`; `msk = isCapture ? nullptr : req.mask.constData()`.
- `alignment = max(1, req.alignment)`.
- `valSize = (isCapture || isTypedConst) ? req.valueSize : patternLen`.
- `hasRange = (startAddress != 0 || endAddress != 0) && endAddress > startAddress`. **Edge:** if only `startAddress` is set with `endAddress==0`, `hasRange` is false (no clipping). The combinations test only uses `endAddress`.
- `bmhEligible = !isCapture && patternLen >= 4 && alignment == 1`; further disabled if any mask byte != `0xFF`. (Test `bmh_pathParity`: flipping `mask[0]` to `0x00` forces the naive path, same result count.)

### constrainRegions intersection (`scanner.cpp:642-674`)
If `req.constrainRegions` non-empty:
1. Copy + sort by `start` ascending.
2. Merge: skip degenerate `c.end <= c.start`; if `c.start <= merged.last().end` extend `merged.last().end = max(...)`, else append. (Merges overlapping/adjacent ranges — Test `scan_constrainRegions_overlappingConstraints`: `{4,20}+{12,28}` → `{4,28}`, no double counting.)
3. For each provider region, for each merged constraint, intersect `[max(base,c.start), min(rEnd,c.end))`; if non-empty, push a clone of the region with `base=iStart, size=iEnd-iStart`. Skips non-overlapping (`c.end <= base || c.start >= rEnd`).
4. `regions = clipped`.
Tests for this are extensive (`scan_constrainRegions_*`): gap-between-regions, partial overlap, mixed module/anon, fallback provider, adjacent regions, writable filter preserved, extends-before-and-after (clip to intersection), single range, with-start/end-address (double intersection), unknown-value scan, non-zero base, zero-size (0 results), inverted range (0 results), pattern at first/last byte, pattern one byte after end (doesn't fit → 0), region smaller than pattern (0), pattern exactly fits region (1), match at region boundaries, multibyte at clip boundary (doesn't fit → 0).

### `regionAccepted` lambda (`scanner.cpp:678-684`) — reused in pre-pass AND inner loop
Returns false if: (`filterExecutable && !r.executable`) || (`filterWritable && !r.writable`) || (`privateOnly && r.type != Private`) || (`skipSystemModules && isSystemModule(r.moduleName)`). Otherwise true.
Tests: `scan_filterExecutable`, `scan_filterWritable`, `scan_bothFilters` (must be BOTH x AND w), `regionType_privateOnly_*`, `skipSystem_*`, `skipSystem_combinesWithPrivateOnly` (filters compound), and the combinations test `regionFilter_combos`.

### Progress pre-pass (`scanner.cpp:689-709`)
Computes `totalBytes` (sum of accepted, range-clipped region sizes) and `acceptedRegions`. Emits `regionsResolved(acceptedRegions, totalBytes)` via queued connection. If `totalBytes == 0` → return empty (covers no-accepted-regions, zero-size constraints, range outside data — Tests `scan_addressRangeOutsideData`, `scan_constrainRegions_zeroSizeConstraint`, `scan_constrainRegions_invertedRange`).

### Main loop (`scanner.cpp:711-892`)
Per region (index `regionIndex`):
- Abort check at top (`m_abort.load()` → break).
- Skip if `!regionAccepted`.
- Clip `[regStart, regEnd)` to `[startAddress, endAddress)` if `hasRange`; entirely-outside → continue; `regSize = regEnd - regStart`; `regSize==0` → continue.
- If `patternLen > regSize`: add `regSize` to `scannedBytes` and continue (region too small — Tests `scan_patternLargerThanData`, `scan_constrainRegions_regionSmallerThanPattern`).
- `overlap = patternLen - 1`.
- **Adaptive chunk:** `kChunkBig = 2 MiB`, `kChunkMin = 64 KiB`. `targetChunk = min(2MiB, regSize)`; if `regSize < 64KiB`, `targetChunk = regSize`. Allocate `chunk` of `targetChunk` bytes. `regOffset = regStart - region.base` (unused beyond debug).
- `kAbortStride = 4096`.

Inner loop over `off` from 0 to `regSize`:
- Abort check.
- `remaining = regSize - off`; `readLen = min(chunk.size, remaining)`.
- `prov->read(regStart+off, chunk.data(), readLen)`: on failure, add `readLen` to `failedBytes` and `scannedBytes`, advance `off += readLen`, continue (skip unreadable chunk — never crashes; reflected in `ScanStats.bytesFailed`).
- `scanEnd = readLen - patternLen` (last valid match start within this chunk).
- `data = chunk.constData()`.

**Four match paths** (mutually exclusive):

1. **Capture** (`isCapture`, `scanner.cpp:777-792`): for `i` in `0..=scanEnd` step `alignment`, periodic abort (`(i & (kAbortStride-1))==0 && abort` → `goto done`). Build `ScanResult{address=regStart+off+i, regionModule=formatRegionContext(...), scanValue=QByteArray(data+i, valSize)}`. Push. If `results.size() >= maxResults` → `goto done`.
   - Test `scan_unknownWithAddressRange`: range `[8,24)`, align 4, valSize 4 → offsets 8,12,16,20 = 4 results. `condition_increasedBy_rescan`: 16 bytes align 4 → 4 captured slots seeded.

2. **Typed const** (`isTypedConst`, `scanner.cpp:793-820`): `loBuf=req.pattern`, `hiBuf=req.pattern2`. For each aligned `i`: `val = QByteArray(data+i, valSize)`; `cmpLo = compareTyped(val, loBuf, valueType)`. `BiggerThan: cmpLo>0`; `SmallerThan: cmpLo<0`; `Between (hiBuf non-empty): cmpLo>=0 && compareTyped(val,hiBuf,...)<=0`. On match push `ScanResult` with `scanValue = std::move(val)`. maxResults cap.
   - Tests `condition_biggerThan_firstScan`, `condition_smallerThan_firstScan`, `condition_between_firstScan`, `condition_between`, plus the matrix in combinations test. Note Between with `lo==hi` is exact-equality (Test `condition_between` "100..100" → 1).

3. **BMH** (`bmhEligible`, `scanner.cpp:821-839`): repeatedly `bmhFind(data+searchFrom, readLen-searchFrom, pat, patternLen)`; convert to absolute `absI=searchFrom+hit`; stop if `absI > scanEnd`; push `ScanResult` with `scanValue = QByteArray(data+absI, min(16, readLen-absI))` (note: BMH/naive cache up to 16 bytes, NOT valSize). `searchFrom = absI + 1` (finds overlapping matches). maxResults cap. Abort checked each iteration.

4. **Naive masked** (`else`, `scanner.cpp:840-862`): for aligned `i`, periodic abort; match iff for all `j` in `0..patternLen`: `(data[i+j] & msk[j]) == (pat[j] & msk[j])`. On match push `ScanResult` with `scanValue = QByteArray(data+i, min(16, readLen-i))`. maxResults cap.
   - Handles wildcards and alignment > 1. Tests: `scan_exactMatch`, `scan_wildcardMatch`, `scan_alignment4`, `scan_overlappingMatches` ("AA AA" in "AA AA AA" → matches at 0 and 1), `scan_allWildcardPattern` (mask all 0 → 7 positions in 8-byte buffer with 2-byte pattern), plus the combinations `signature_wildcards` and `fastScan_alignmentValues`.

### Chunk advance with overlap (`scanner.cpp:865-881`)
- If `readLen >= remaining` (last chunk): `advance = remaining` (no overlap; nothing follows).
- Else if `readLen > overlap`: `advance = readLen - overlap`; **if alignment > 1**, re-align: `nextOff = off + advance`, `aligned = ceil(nextOff/alignment)*alignment`, `advance = aligned - off` (so the next chunk start lands on the alignment grid).
- Else `advance = 1` (prevents infinite loop on tiny regions).
- `scannedBytes += advance; off += advance`.
- The `overlap = patternLen-1` carry-over ensures a pattern straddling a chunk boundary is still found. Test `scan_chunkBoundaryOverlap` places a 4-byte pattern at `256KiB - 2`. Test `adaptiveChunk_largeRegion` (3 MiB region, hit near end) verifies the multi-chunk path.

### Progress emission (`scanner.cpp:883-890`)
`pct = min(100, scannedBytes*100/totalBytes)`; emit `progress(pct)` via queued connection only when `pct` changes. Tests `scan_progressEmitted` (>=1 signal, last >=50), `scanStats_emitted`.

### Finalization (`done:` label, `scanner.cpp:894-916`)
Builds `ScanStats{regionsScanned=acceptedRegions, bytesScanned, bytesFailed, msElapsed}` and emits `scanStats` (queued). The trailing `if (cond == Changed || ...)` block (`scanner.cpp:909-915`) is a **defensive no-op** (the panel converts those to UnknownValue at first-scan time). Returns `results`.

**maxResults invariant:** the cap is checked *after* each push, so the returned vector size is exactly `min(matches, maxResults)`. Test `scan_maxResults` (1000 `0xAA` bytes, 1-byte pattern, cap 10 → exactly 10).

---

## 7. `runRescan` — refine algorithm — `scanner.cpp:949-1136`

Signature: `(prov, results, readSize, condition, valueType, filterPattern, filterMask, filterPattern2)`. Returns the filtered/updated `QVector<ScanResult>`.

1. Start timer. `total = results.size()`; if `0 || !prov` → return `results` unchanged (Test `rescan_emptySeed` → 0).
2. Classify filter mode (`scanner.cpp:961-971`):
   - `hasExactFilter = !filterPattern.isEmpty() && condition==ExactValue`.
   - `hasComparison = condition in {Changed, Unchanged, Increased, Decreased}`.
   - `hasTypedConst = condition in {BiggerThan, SmallerThan, Between}`.
   - `hasDelta = condition in {IncreasedBy, DecreasedBy}`.
   - `needsFilter = hasExactFilter || hasComparison || hasTypedConst || hasDelta`.
3. **Save previous values:** for each result `r.previousValue = r.scanValue` (`scanner.cpp:979-980`). This snapshot is the comparison baseline.
4. **Order by address:** build index vector `order[0..total)`, stable-`std::sort` by `results[idx].address` ascending. (Used for sequential chunked reads.)
5. `matched(total, !needsFilter)` — a `QVector<bool>` initialized to `true` when no filter (so unfiltered rescans keep everything), `false` otherwise.
6. **Span-chunked read loop** (`kChunk = 256 KiB`, `scanner.cpp:999-1116`):
   - Start span at sorted index `i`; `spanBase = results[order[i]].address`.
   - Extend `spanEnd` while the next result's `address + readSize - spanBase <= 256KiB` (group nearby addresses into one read).
   - `spanLast = results[order[spanEnd]].address`; `chunkLen = spanLast + readSize - spanBase`; alloc `chunk(chunkLen,'\0')`; `prov->read(spanBase, chunk.data(), chunkLen)` (return value ignored — failed reads leave zero-filled bytes).
   - For each `j` in `i..=spanEnd`: `idx=order[j]`; `off = r.address - spanBase`; `r.scanValue = chunk.mid(off, readSize)` (re-cache current bytes, exactly `readSize` long unless truncated at chunk end).
   - Apply the active filter to set `matched[idx]`:
     - **ExactValue** (`scanner.cpp:1022-1037`): if `r.scanValue.size() >= filterPattern.size()`, masked compare `(data[k]&msk[k])==(pat[k]&msk[k])` for all `k`; `matched[idx]=ok`.
     - **Comparison** (`scanner.cpp:1040-1049`): only if `!r.previousValue.isEmpty()`; `cmp = compareTyped(scanValue, previousValue, valueType)`; `Changed: cmp!=0`, `Unchanged: cmp==0`, `Increased: cmp>0`, `Decreased: cmp<0`.
     - **Typed const** (`scanner.cpp:1054-1065`): only if `!filterPattern.isEmpty()`; `cmpLo = compareTyped(scanValue, filterPattern, valueType)`; `BiggerThan: cmpLo>0`, `SmallerThan: cmpLo<0`, `Between (filterPattern2 non-empty): cmpLo>=0 && compareTyped(scanValue,filterPattern2)<=0`.
     - **Delta** (`scanner.cpp:1069-1102`): only if `!previousValue.isEmpty() && !filterPattern.isEmpty()`. `sz = min(previousValue.size, filterPattern.size)`; if `scanValue.size >= sz`, dispatch on `valueType` to a generic lambda `addAndCheck<T>`: memcpy `prev`, `delta`, `cur` as `T` (requires `sz >= sizeof(T)`); `expected = IncreasedBy ? T(prev+delta) : T(prev-delta)`; `matched[idx] = (cur == expected)`. Types handled: Int8/UInt8/Int16/UInt16/Int32/UInt32/Int64/UInt64/Float/Double. **Wrapping/IEEE arithmetic** is implied by the C++ cast `T(prev+delta)` — for unsigned/signed integers this is two's-complement wrap; for float/double it's IEEE add/sub with exact `==`. (Tests `condition_increasedBy_rescan` +5 → exactly the mutated slot; `condition_decreasedBy_rescan` -7.)
   - Accumulate `chunks`, `totalBytesRead`, `updated += spanEnd-i+1`; `i = spanEnd+1`.
   - Emit `progress(updated*100/total)` (queued) when it changes.
   - Abort: loop condition `while (i < total && !m_abort.load())`.
7. **Filter out** (`scanner.cpp:1119-1130`): if `needsFilter`, build `filtered` of `std::move(results[k])` where `matched[k]`; return it. Else return `results` (all updated, none dropped).

**Subtle rescan invariants:**
- A `Changed`/`Increased`/etc. comparison with an *empty* `previousValue` leaves `matched[idx]` at its default (`false` since `needsFilter` is true), i.e. it's dropped. Real seeds always have a `scanValue`, so `previousValue` is non-empty after step 3.
- `readSize` is caller-supplied (the rescan read width), independent of the original pattern length. The e2e test reads int32 with `readSize=4`.
- Reads in `runRescan` ignore the return value, so the rescan never aborts on an unreadable address; it just compares against zero bytes.

---

## 8. Qt usage → Rust equivalents

| Qt type / API | Use here | Rust equivalent |
|---|---|---|
| `QByteArray` | pattern/mask/scanValue/chunk buffers | `Vec<u8>` / `&[u8]`; `bytes::Bytes` if cheap clones needed. `QByteArray(data+i, n)` → `data[i..i+n].to_vec()`. `chunk.mid(off,len)` → `chunk[off..off+len.min(...)]`. |
| `QString` | input parsing, module names, error msgs | `String` / `&str`. `trimmed()`→`trim()`. `toLower()`→`to_lowercase()`. `split`/`SkipEmptyParts`→`split_whitespace`/`split(..).filter(!is_empty)`. |
| `QString::toInt/toUInt/toLongLong/...(&ok)` | value parsing | `i32::from_str` etc. with explicit `0x` hex fallback for unsigned types. |
| `QString::toFloat/toDouble` | float parsing | `f32::from_str`/`f64::from_str`. |
| `QStringLiteral("%1+0x%2").arg(...)` | region formatting, error strings | `format!`. Hex via `{:x}`. |
| `QVector<T>` | regions, results, constraints | `Vec<T>`. |
| `QSet<QString>` | system-module table | `HashSet<&'static str>` or a `phf` set / `matches!`. |
| `QRegularExpression("\\s+")` | vec split | `str::split_whitespace`. |
| `QObject`/signals/slots (`progress`, `finished`, `rescanFinished`, `error`, `scanStats`, `regionsResolved`) | progress & result delivery | callbacks / `crossbeam`/`std::sync::mpsc` channels, or a trait `ScanObserver`. The four signals carry: `progress(i32)`, `finished(Vec<ScanResult>)`, `rescanFinished(Vec<ScanResult>)`, `error(String)`, `scanStats(ScanStats)`, `regionsResolved(count:i32, totalBytes:u64)`. |
| `QtConcurrent::run` + `QFutureWatcher` | background scan thread | `std::thread::spawn` or a thread pool (`rayon`); deliver result on completion. |
| `QMetaObject::invokeMethod(..., Qt::QueuedConnection, ...)` | marshal progress signals to UI thread | send over channel. |
| `std::atomic<bool> m_abort` | cooperative abort | `Arc<AtomicBool>` with `Relaxed`/`Acquire` loads. |
| `QElapsedTimer` | ms elapsed | `std::time::Instant`. |
| `std::memchr`/`memcpy`/`memcmp` | BMH/compare/copy | `slice::iter().position`, `copy_from_slice`, `&a[..n]==&b[..n]` / `a[..n].cmp(&b[..n])`. |
| `Q_DECLARE_METATYPE` | queued-signal registration | n/a. |
| `qDebug()` | trace logging | `log`/`tracing` (optional). |

---

## 9. Platform-specific code

The scanner core (`scanner.cpp`/`.h`) is **fully portable** — no `#ifdef`. The only platform-specific code is in the **test harness**: `test_scanner.cpp:17-21,2802-3000` wrap a Win32 self-attach test (`WinSelfProvider`, `selfAttach_findMutateRevalidate`) in `#ifdef _WIN32` using `OpenProcess`/`ReadProcessMemory`/`VirtualQueryEx`/`EnumProcessModulesEx`. That is a *live process provider* test (out of scope per the task's plugin exclusion). The Rust port's scanner tests should mirror only the pure-buffer tests; any process-attach test stays behind `#[cfg(windows)]` and is out of scope. The `isSystemModule` table includes Windows/Linux/macOS module names but is selected at runtime by name string, not compiled conditionally — port it whole and it compiles on all targets.

---

## 10. Concurrency & threading

- One scan/rescan at a time: `start`/`startRescan` early-return if `isRunning()`.
- The heavy work runs on a worker thread (`QtConcurrent::run`); progress/stats/regionsResolved are marshaled back via queued signals; `finished`/`rescanFinished` fire on the watcher's `finished` (owner thread).
- Abort is a single `std::atomic<bool>` polled: at every region top, every chunk top, and every `kAbortStride` (4096) inner iterations (and every BMH iteration). `abort()` just flips the flag — the in-flight scan finishes promptly and still emits a (possibly partial) result set.
- The region cache (`m_cachedRegions`/`m_cachedProvider`) is `mutable` and touched only by the worker; no locking — relies on the single-scan-at-a-time invariant. **Rust:** model the engine as owning its cache; gate concurrency with a `running` flag; pass an `Arc<AtomicBool>` abort token into the worker.

---

## 11. Data / serialization formats

The engine itself has **no on-disk format**. The result-list JSON shape is owned by `ScannerPanel` (UI, out of this subsystem), but `test_scanner.cpp:2809-2826` (`scanResult_jsonShape`) documents the per-result schema the port should preserve if/when the panel serializes:
```json
{ "address": "<hex, no 0x, lowercase, %x of u64>",
  "value":   "<lowercase hex of scanValue bytes>",
  "module":  "<regionModule string>" }
```
- `address` = `QString::number(addr, 16)` (e.g. `0xDEADBEEFCAFEBABE` → `"deadbeefcafebabe"`), parsed back with base-16.
- `value` = `scanValue.toHex()`.
- `module` = `regionModule` verbatim.
Pattern/value *input* formats are documented under `parseSignature`/`serializeValue` (§3): IDA signatures (`"48 8B ?? 05"`), packed (`"488B??05"`), C-style (`"\x48\x8B"`), and per-type numeric/vector/string literals.

---

## 12. Subtle behaviors the tests rely on (checklist for the port)

1. **`scanValue` width differs by path:** capture & typed-const cache `valSize` (=`req.valueSize`) bytes; BMH & naive cache `min(16, readLen-i)` bytes. The e2e/rescan tests memcpy 4/8 bytes back out of `scanValue`, so for those (capture/UnknownValue seeds) `valueSize` bytes is essential. (`scanner.cpp:787,815,834,856`.)
2. **`maxResults` is post-push** — exact cap, not off-by-one. (Test `scan_maxResults`.)
3. **Overlapping matches are found** in naive & BMH paths (`searchFrom=absI+1`, naive uses `i+=alignment` with alignment 1). (Tests `scan_overlappingMatches`, `scan_allWildcardPattern`.)
4. **Alignment is measured from address 0 / the clipped sub-region's start**, advancing by `alignment` from `i=0` within each chunk where chunk start is re-aligned. Tests assert unaligned values are skipped (`scan_alignment4_skipsUnaligned`, `scan_alignment8_skipsUnaligned`, `scan_alignment2_findsAligned_skipsUnaligned`) and the alignment dropdown counts (`fastScan_alignmentValues`: align 1 and 4 both find all 64 four-byte slots; 8→32; 16→16; 32→8; 64→4).
5. **`hasRange` requires `endAddress > startAddress`**; `endAddress==0` disables clipping even if `startAddress!=0`.
6. **constrainRegions merge avoids double counting** and clips to the intersection with provider regions (and with start/end address simultaneously). A pattern that doesn't fully fit inside a clipped sub-region is NOT matched (clipping happens at the region level, so a 4-byte pattern needs all 4 bytes inside the clipped `[start,end)`).
7. **`privateOnly` keys on `RegionType`, not writability.** Note `BufferProvider`'s synthetic region is `RegionType::Mapped` — so `privateOnly` on a plain buffer scan would drop everything; tests that exercise `privateOnly` always use a custom provider returning `Private` regions.
8. **`skipSystemModules` + `privateOnly` compound** (a Private region inside a system module is still cut). (Test `skipSystem_combinesWithPrivateOnly`.)
9. **Both filters = AND** (region must be exec AND writable). (Test `scan_bothFilters`, `regionFilter_combos`.)
10. **compareTyped float/NaN** yields 0 (not-greater, not-less); byte fallback for unsupported types. Changed/Unchanged on multi-byte (vec/string) values use the byte fallback (`memcmp`).
11. **Between with `lo==hi` is exact equality** (`cmpLo>=0 && cmpHi<=0`). (Test `condition_between` "100..100"→1.)
12. **First-scan compare-against-previous conditions (Changed/Unchanged/Increased/Decreased/IncreasedBy/DecreasedBy) capture every aligned slot** (no filtering) — filtering only happens in `runRescan`. (E2e test seeds via UnknownValue then rescans.)
13. **IncreasedBy/DecreasedBy delta uses wrapping integer / IEEE float arithmetic and exact `==`** against `previous ± delta`. (`scanner.cpp:1083-1085`.)
14. **Rescan saves `previousValue = scanValue` before re-reading**, so the comparison baseline is the prior scan's cached bytes. Rescan returns the seed unchanged if `needsFilter` is false (e.g. ExactValue with empty filterPattern, or unknown condition). (`scanner.cpp:997,1135`.)
15. **Empty seed rescan is a no-op** returning empty. (Test `rescan_emptySeed`.)
16. **Region cache reuse** keyed on raw provider pointer; `invalidateRegionCache` forces re-enumeration. (Test `regionCache_reusesAcrossScans`.)
17. **Unreadable chunks are skipped, not fatal** — counted in `bytesFailed`; scan continues; `finished` still fires. (Stats test asserts `bytesFailed==0` on the happy path.)
18. **`regionModule` is `"name+0xoffset"` or empty** — exact lowercase-hex offset, no leading zeros. (Tests assert literal strings.)
19. **BMH ⇔ naive equivalence** is a property the port must keep (random-data test). Use the identical shift table (last byte excluded from bad-char table, ties resolve to the last occurrence among `pat[0..len-2]`).
20. **Validation errors are emitted synchronously** before the thread spawns (`"Empty pattern"`, `"Pattern and mask size mismatch"`). `UnknownValue` bypasses pattern/mask validation.

---

## 13. Recommended Rust shape (no code yet)

- Pure module `scanner::` with free fns `parse_signature`, `serialize_value`, `natural_alignment`, `value_size_for_type`, `compare_typed`, `bmh_find`, `is_system_module`, and `format_region_context`, all over `&[u8]`/`&str`.
- `ScanRequest`, `ScanResult`, `ScanStats`, `AddressRange`, `ValueType`, `ScanCondition`, `MemoryRegion`, `RegionType` as plain structs/enums (`MemoryRegion`/`RegionType` likely live in the `provider` module and are re-used).
- `run_scan(prov: &dyn Provider, req: &ScanRequest, abort: &AtomicBool, observer: &mut dyn ScanObserver) -> Vec<ScanResult>` and `run_rescan(...) -> Vec<ScanResult>` as pure-ish functions taking the abort token + an observer trait for progress/stats; the threading wrapper (`ScanEngine`) is a thin layer that spawns these and forwards to channels/callbacks.
- Crates: `std` only for the core; optionally `memchr` (single-byte fast path), `rayon`/`crossbeam-channel` for the async layer, `serde_json` for the panel's result JSON (out of this subsystem). No regex crate strictly needed (whitespace split suffices for vectors).
