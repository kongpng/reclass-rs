# Subsystem: MCP bridge (JSON-RPC 2.0 over named pipe)

Key: `mcp` — expected portability: **mostly-portable**

Source files studied (all read fully):
- `/home/loke/reclass-cpp/src/mcp/mcp_bridge.h`
- `/home/loke/reclass-cpp/src/mcp/mcp_bridge.cpp` (168 KB; in-scope handlers read fully, out-of-scope tool bodies skimmed)
- `/home/loke/reclass-cpp/tools/rcx-mcp-stdio.cpp` (the stdio↔pipe bridge binary)
- `/home/loke/reclass-cpp/tests/test_mcp.cpp` (protocol/transport tests via `MockMcpServer`)
Supporting references: `src/core.h` (Node/NodeKind/NodeTree/ValueHistory serialization), `src/providers/provider.h` (Provider interface), `src/main.cpp` (wiring/lifecycle).

---

## 1. Purpose

The MCP bridge turns the running Reclass GUI process into a **Model Context Protocol (MCP) server** so an LLM agent (Claude Desktop, an IDE plugin, etc.) can inspect and edit the live struct-layout document and read/write target memory.

It has two pieces:

1. **`McpBridge`** (in-process, inside the Reclass GUI): a `QLocalServer` that listens on a named pipe `"ReclassMcpBridge"`. It speaks **JSON-RPC 2.0**, one JSON object per line (newline-delimited / NDJSON), and implements the MCP methods `initialize`, `tools/list`, `tools/call`, plus server→client notifications.
2. **`rcx-mcp-stdio`** (separate child binary): a tiny stdio↔pipe shim. MCP hosts spawn it; it forwards stdin→pipe and pipe→stdout, line by line. This exists because MCP hosts launch a subprocess and talk over stdio, but the actual server lives inside the GUI behind the named pipe.

Transport contract (must be preserved bit-for-bit for parity):
- One JSON object per line, terminated by a single `\n`.
- Responses are JSON-RPC `{jsonrpc, id, result}` or `{jsonrpc, id, error}`.
- Notifications are JSON-RPC `{jsonrpc, method, params?}` with **no** `id`.
- All JSON serialized **compact** (no spaces) for the wire; tool *text payloads* are often pretty-printed JSON embedded in a `text` content block.

### Port scope note
Many tools in `tools/list` are tied to OUT-OF-SCOPE subsystems (live process plugins, PDB symbols, scanner, RTTI, type-inference, theming, UI introspection). For the Rust port the **in-scope set** is: `project.state`, `tree.apply`, `source.switch`, `hex.read`, `hex.write`, `status.set`, `ui.action`, `tree.search`, `node.history`, plus the two notifications. The rest are documented at schema level only (so `tools/list` parity is maintained) and their handlers can be thin stubs that go through the abstract `Provider` trait / return "not available" `isError` results. The transport, dispatch, JSON-RPC framing and the in-scope tool semantics are the load-bearing parts.

---

## 2. Transport / protocol layer

### 2.1 `rcx-mcp-stdio` (stdio bridge binary) — `tools/rcx-mcp-stdio.cpp`

`int main(int argc, char** argv)`. A `QCoreApplication` event-loop program.

Behavior:
1. (Windows only) `_setmode(_fileno(stdin/stdout), _O_BINARY)` so `\n` is not translated to `\r\n`. (`rcx-mcp-stdio.cpp:26-30`)
2. Create a `QLocalSocket`, with two read buffers: `readBuf` (pipe→stdout direction) and `stdinBuf` (stdin→pipe direction).
3. **Pipe→stdout**: on `readyRead`, append `socket->readAll()` to `readBuf`, then repeatedly split off complete lines (everything up to and including `\n`) and `fwrite` + `fflush` each line to stdout. (`:36-46`)
4. On socket `disconnected` → print to stderr, `app.quit()`. (`:48-51`)
5. On socket `errorOccurred` (Qt ≥5.15) / `error` (older) → print to stderr, `app.quit()`. (`:53-61`)
6. Connect: `socket->connectToServer("ReclassMcpBridge")`; if `!waitForConnected(5000)` → print error, `return 1`. (`:64-69`)
7. **Stdin→pipe**: a `QTimer` polling at **10 ms** intervals because stdin is not a socket on Windows. (`:74-122`)
   - **Windows**: `PeekNamedPipe(GetStdHandle(STD_INPUT_HANDLE), …, &avail, …)`. If Peek fails → pipe broken → `app.quit()`. If `avail==0` → return. Else `ReadFile` up to `min(avail, 4096)` bytes into `stdinBuf`; if read fails or returns 0 → `app.quit()`. (`:78-95`)
   - **Unix**: `select()` on `STDIN_FILENO` with zero timeout (non-blocking poll). If `<=0` return. Else `::read` up to 4096; if `<=0` → `app.quit()`. (`:96-110`)
   - Then split `stdinBuf` into complete lines and `socket->write(line)` + `socket->flush()` per line. **Each forwarded line keeps its trailing `\n`.** (`:111-120`)
8. `stdinTimer->start(); return app.exec();`

**Rust port:** Use `interprocess::local_socket` client. Connect to the same pipe name (`ReclassMcpBridge`). Two loops/tasks: stdin→socket and socket→stdout, both line-buffered on `\n`. A 10 ms poll on Windows is acceptable but Rust can simply do blocking reads on stdin in a dedicated thread (no `PeekNamedPipe` needed since we own the thread). Keep the binary mode behavior (don't translate newlines). Connect timeout 5 s; exit code 1 on connect failure.

### 2.2 `McpBridge` server — pipe name and lifecycle

`start()` (`mcp_bridge.cpp:108-127`):
- No-op if already started (`m_server != nullptr`).
- `m_server = new QLocalServer(this)`.
- `m_server->setSocketOptions(QLocalServer::WorldAccessOption)` — allow any user to connect. (Rust: `interprocess` security/permission equivalent; on Unix this is the socket file mode, on Windows the pipe SDDL.)
- `QLocalServer::removeServer("ReclassMcpBridge")` — delete a stale socket file left over on Linux/macOS (Windows pipes don't persist). (`:115`)
- `m_server->listen("ReclassMcpBridge")`; on failure: `qWarning`, delete server, set null, return (silent failure — app keeps running). (`:117-122`)
- Connect `QLocalServer::newConnection → onNewConnection`.

`stop()` (`:129-144`): For each client: disconnect signals, `disconnectFromServer()`, `deleteLater()`. Clear `m_clients`. Reset `m_currentSender=nullptr`, `m_processing=false`, clear `m_pendingRequests`. Close + delete server, set null.

`isRunning()` = `m_server != nullptr`.

**Lifecycle wiring** (`src/main.cpp`): `m_mcp = new McpBridge(this, this)` at startup (`main.cpp:1045`). Auto-started if `QSettings("Reclass","Reclass").value("autoStartMcp", true)` is true (`:5203-5204`). A toggle action restarts it (`stop()` then `start()`, `:4839-4844`). A "slow mode" menu action calls `setSlowMode(checked)` (`:1621`). Document changes wire `RcxDocument::documentChanged → m_mcp->notifyTreeChanged()` (`:3233-3234`).

The pipe name `"ReclassMcpBridge"` is a hard-coded shared constant between client and server — keep identical in Rust.

### 2.3 Constructor / notify timer

`McpBridge(MainWindow*, QObject* parent)` (`:91-102`):
- Stores `m_mainWindow`.
- Creates `m_notifyTimer` (single-shot, 100 ms). On timeout, **if clients exist**, sends `notifications/resources/updated` with `{"uri":"project://tree"}`. (NOTE: this timer is created but I found no `start()` call on it in this file — `notifyTreeChanged()`/`notifyDataChanged()` send directly without the timer. The timer is effectively a coalescing facility that is wired but not triggered in the current code; treat as a latent/no-op debounce. The Rust port can omit the timer or model it as an optional debounce, but the *direct* notify path must be preserved.)

`~McpBridge()` → `stop()`.

### 2.4 Connection handling & the serial request queue

State per server:
- `QVector<ClientState> m_clients` — `ClientState { QLocalSocket* socket; QByteArray readBuffer; bool initialized=false; }` (`mcp_bridge.h:32-36`).
- `QLocalSocket* m_currentSender` — set for the duration of one request so `sendJson` knows where to reply.
- `bool m_processing` + `QVector<PendingRequest> m_pendingRequests` where `PendingRequest { QLocalSocket* socket; QByteArray line; }` — a **serial request queue**.

`onNewConnection()` (`:167-179`): `nextPendingConnection()`; push `ClientState{socket,{},false}`; connect `readyRead→onReadyRead`, `disconnected→onDisconnected`.

`findClient(sock)` (`:150-154`): linear scan for matching socket, returns `ClientState*` or null.

`removeClient(sock)` (`:156-165`): find by socket, `disconnect(this)`, `deleteLater()`, `removeAt(i)`.

`onReadyRead()` (`:181-216`) — the core framing + queueing logic:
1. `sock = qobject_cast<QLocalSocket*>(sender())`; `cs = findClient(sock)`; if null return.
2. Append `sock->readAll()` to `cs->readBuffer`.
3. **Overflow guard**: if `readBuffer.size() > kMaxReadBuffer (10 MB)` → `qWarning`, `disconnectFromServer()`, return. (`:23`, `:187-191`)
4. Loop while client still exists (`findClient(sock)` re-checked each iteration since processing may delete clients):
   - Re-fetch `cs`, find `\n` index; if none → break.
   - `line = readBuffer.left(idx).trimmed()` (the newline is NOT included; surrounding whitespace trimmed); `readBuffer.remove(0, idx+1)`.
   - If `line.isEmpty()` → continue (skip blank lines).
   - **If `m_processing`** → push `{sock,line}` to `m_pendingRequests` and continue (queue it).
   - Else: set `m_processing=true`, `m_currentSender=sock`, `processLine(line)`, then `m_currentSender=nullptr`, `m_processing=false`, then `drainPendingRequests()`.

`drainPendingRequests()` (`:218-228`): while queue non-empty, `takeFirst()`; if its socket no longer a client → skip; else process it serially (same set/clear of `m_processing`/`m_currentSender`). **Does not re-drain recursively** (one flat pass; nested enqueues happen via `m_processing` flag during processing).

**Why this matters (subtle, load-bearing):** some tool handlers (`tree.apply`, scanner) spin nested Qt event loops (`QEventLoop`, `processEvents`). Without the serial queue, another client's `readyRead` could fire mid-handler and clobber `m_currentSender`, sending a reply to the wrong socket. The queue guarantees **one request in flight at a time, globally across all clients**. The Rust port should serialize request handling globally (e.g. a single worker / a mutex around request dispatch), not per-connection — this is a real behavioral requirement, exercised by `multiClient_serialRequests`.

`onDisconnected()` (`:230-239`): get `sock`; **purge queued requests from this socket** via `std::remove_if` on `m_pendingRequests`; then `removeClient(sock)`. (Tested by `multiClient_disconnectOne`, `allDisconnect_serverSurvives`, and the reconnect tests.)

### 2.5 JSON-RPC message construction

`okReply(id, result)` (`:245-251`) → `{"jsonrpc":"2.0","id":id,"result":result}`.

`errReply(id, code, msg)` (`:253-259`) → `{"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg}}`.

`sendJson(obj)` (`:261-269`): target = `m_currentSender`; if null or not a current client → **drop silently**. Serialize `QJsonDocument::Compact`, debug-log first 200 chars, append `\n`, `write` + `flush`. (Replies always go to the request's originating socket only.)

`sendNotification(method, params={})` (`:271-282`): build `{"jsonrpc":"2.0","method":method}`; if `params` non-empty add `"params"`. Compact + `\n`. **Broadcast to every client whose `initialized==true`** (`write`+`flush` each). Uninitialized clients receive nothing — verified by `multiClient_notificationBroadcast` (`test_mcp.cpp:224-243`, the third uninitialized client gets 0 lines).

`makeTextResult(text, isError=false)` (`:284-294`): builds an MCP tool result:
```json
{"content":[{"type":"text","text":"<text>"}], "isError": true?}
```
`isError` key only present when true. This is the canonical "tool result" shape; almost every tool returns through this.

### 2.6 Dispatch — `processLine(const QByteArray& line)` (`:300-340`)

Wrapped in `try/catch`. Steps:
1. Debug-log first 200 chars.
2. `QJsonDocument::fromJson(line)`; if **not an object** → `sendJson(errReply(null, -32700, "Parse error"))`, return. (Tested: `protocol_invalidJson` expects `-32700`.)
3. `req = doc.object()`, `id = req.value("id")`, `method = req.value("method").toString()`.
4. **Client notifications** `notifications/initialized` and `notifications/cancelled` → return with **no response**. (Tested: `protocol_notificationsIgnored`.)
5. `method == "initialize"` → set MCP status, `sendJson(handleInitialize(id, params))`, clear status.
6. `method == "tools/list"` → set status, `sendJson(handleToolsList(id))`, clear status.
7. `method == "tools/call"` → `sendJson(handleToolsCall(id, params))`.
8. Else → `sendJson(errReply(id, -32601, "Method not found: " + method))`.
9. `catch(std::exception&)` → `errReply(null, -32603, "Internal error: <what>")`; `catch(...)` → `errReply(null, -32603, "Internal error")`.

**Error codes used (JSON-RPC standard):**
- `-32700` Parse error (non-object / invalid JSON line)
- `-32601` Method not found / Unknown tool
- `-32603` Internal error (exception)
- (The real `McpBridge` does **not** emit `-32600` "Missing method" or `-32602` "Missing tool name"; the *test mock* does. See §8. For a faithful port of the real server, an empty/unknown method falls into the `-32601` "Method not found:" branch with an empty name string, and a `tools/call` with no `name` reaches `handleToolsCall` and falls through to `-32601 "Unknown tool: "`.)

Note: the real `id` is echoed unchanged in replies; it may be a number, string, or null. The `QJsonValue id` is passed through as-is.

---

## 3. MCP method handlers

### 3.1 `handleInitialize(id, params)` (`:346-382`)
- Marks the current client `initialized=true` (`findClient(m_currentSender)`).
- Returns:
```json
{
  "protocolVersion": "2024-11-05",
  "capabilities": {"tools": {"listChanged": false}},
  "serverInfo": {"name": "reclass-mcp", "version": "1.0.0"},
  "instructions": "<long multi-line guidance string>"
}
```
- The `instructions` string (verbatim, `:359-379`) explains STRUCTURE vs LIVE DATA, the rename+change_kind rule, reset_tracking workflow, baseRelative semantics, etc. Port it verbatim for parity.
- `params` are ignored entirely.

### 3.2 `handleToolsList(id)` (`:388-1074`)
Returns `{"tools": [ <tool descriptor>, … ]}`. Each descriptor is `{name, description, inputSchema}` where `inputSchema` is a JSON-Schema object (`type:"object"`, `properties:{…}`, optional `required:[…]`). **The order and exact text are part of the public contract**; reproduce them.

The full registered tool list (in registration order):
1. `project.state`
2. `tree.apply`
3. `source.switch`
4. `source.modules`
5. `hex.read`
6. `hex.write`
7. `bookmarks.list`, `bookmarks.add`, `bookmarks.remove`
8. `refs.find`
9. `status.set`
10. `ui.action`
11. `tree.search`
12. `node.history`
13. `scanner.scan`, `scanner.scan_pattern`
14. `mcp.reconnect`
15. `process.info`
16. `symbols.load`, `symbols.lookup`, `symbols.importType`
17. `node.read_value`
18. `analysis.infer_types`, `analysis.import_header`, `analysis.pointer_chain`, `analysis.find_overlaps`, `analysis.field_path`, `analysis.tree_summary`
19. `ui.byte_selection`, `ui.set_byte_selection`, `ui.inspect`
20. `theme.get`, `theme.set`, `theme.save`, `theme.revert`

The exact `inputSchema` for each in-scope tool is captured in §5. For out-of-scope tools the schema text is in the source at the cited lines; the Rust port should still advertise these schemas verbatim so the host UI shows the same toolset, but their handlers may return `isError` "not available in this build" results (or thin Provider-trait stubs where they degrade gracefully). Schema source line ranges: `project.state` 392-421, `tree.apply` 424-467, `source.switch` 470-488, `source.modules` 491-503, `hex.read` 506-531, `hex.write` 534-553, `bookmarks.*` 556-591, `refs.find` 594-609, `status.set` 612-627, `ui.action` 630-650, `tree.search` 653-672, `node.history` 675-693, `scanner.scan` 696-723, `scanner.scan_pattern` 726-749, `mcp.reconnect` 752-760, `process.info` 764-777, `symbols.*` 780-835, `node.read_value` 838-856, `analysis.*` 859-978, `ui.byte_selection` 981-995, `ui.set_byte_selection` 998-1015, `ui.inspect` 1018-1033, `theme.*` 1036-1071.

### 3.3 `handleToolsCall(id, params)` (`:1080-1151`)
1. `toolName = params.value("name").toString()`; `args = params.value("arguments").toObject()`.
2. Show status: `setMcpStatus("MCP: <toolName>")` then `QCoreApplication::processEvents(ExcludeUserInputEvents)` (pump UI). (Port: no-op or a status callback; not behaviorally required for the wire.)
3. Big `if/else` dispatch by name (`:1089-1124`) → produces a `QJsonObject result` (a tool result). Unknown tool → `errReply(id, -32601, "Unknown tool: " + toolName)` (returns early, NOT wrapped in `okReply`).
4. **Presentation-mode glow** (`:1127-1146`): if `presentationMode()` and tool != `tree.apply` and `args["nodeId"]` parses to nonzero, briefly focus+scroll editors to that node, run a 150 ms nested event loop, then clear focus. Pure UI; omit/stub in port.
5. `clearMcpStatus()`.
6. Return `okReply(id, result)` — i.e. **tool results are always wrapped as a successful JSON-RPC response**; tool-level failures are signaled by `result.isError==true` inside `content`, not by a JSON-RPC `error`. (This is standard MCP.)

---

## 4. Helpers shared by tools

### 4.1 `parseInteger(const QJsonValue& v, int64_t defaultVal=0)` (`:28-44`)
Tolerant integer parse used for `offset`, `length`, `pid`, `limit`, `tabIndex`, etc.:
- `undefined`/`null` → default.
- **String**: trim; empty → default; if starts with `"0x"` (case-insensitive) parse hex of `mid(2)`, else parse decimal (base 10), both `toLongLong`. On parse failure → default.
- **Double (JSON number)** → `static_cast<int64_t>` (truncates).
- Else → default.

Rust: a helper accepting `serde_json::Value` returning `i64`, supporting decimal/hex strings and numeric. Note JSON numbers are truncated toward zero.

### 4.2 `resolvePlaceholder(ref, placeholderMap, ok=nullptr)` (`:1157-1169`)
For `tree.apply` `$N` references. If `ref` starts with `'$'`: look up in `placeholderMap`; if found return decimal string of the reserved id; else set `*ok=false` and return `ref` unchanged. If not a `$` → return `ref` as-is (and `*ok=true`).

### 4.3 `resolveTab(args, resolvedIndex=nullptr) -> MainWindow::TabState*` (`:1175-1206`)
Smart tab resolution, in order:
1. If `args` has `tabIndex` → `tabByIndex(parseInteger(tabIndex))`; if found, use it.
2. Else `activeTab()` (the focused MDI sub-window); if non-null, use it (and find its index).
3. Else first tab (`tabByIndex(0)`) if `tabCount()>0`.
4. Else **auto-create a new project** (`project_new()`) and return tab 0.

Port: the document/tab model is its own subsystem; here, model `resolveTab` as "pick tab by index → active → first → create". The auto-create-on-demand behavior is part of the contract (a tool call never fails purely due to "no document open").

A `TabState` exposes `doc` (RcxDocument) and `ctrl` (Controller); `doc->tree` is the `NodeTree`, `doc->provider` the `Provider`, `doc->undoStack` the undo stack, `doc->filePath`, `doc->modified`, `doc->typeAliases`. `ctrl` exposes `viewRootId()`, `savedSources()`, `activeSourceIndex()`, `selectedIds()`, `valueHistory()`, `refresh()`, `setViewRootId()`, `switchSource()`, `setSuppressRefresh()`, and undo-command pushing helpers. These belong to the document/controller subsystems; MCP only consumes them.

---

## 5. In-scope tools — exact behavior

All tools take `QJsonObject args` and return a tool-result `QJsonObject` (via `makeTextResult` or with extra keys). Almost all begin with `tab = resolveTab(args); if (!tab) return makeTextResult("No active tab", true);`.

### 5.1 `project.state` — `toolProjectState` (`:1212-1360`)
**Schema** (`:402-420`): props `tabIndex` (int), `depth` (int, default 1), `parentId` (string), `includeTree` (bool, default true), `includeMembers` (bool, default false), `limit` (int, default 50, max 500), `offset` (int, default 0). No required.

Args parsed:
- `maxDepth = parseInteger(depth, 1)`.
- `includeTree` = present ? bool : true.
- `includeMembers = includeMembers.toBool(false)`.
- `limit = qBound(1, parseInteger(limit,50), 500)`.
- `offset = max(0, parseInteger(offset,0))`.
- `filterParentId` = `parentId` empty ? 0 : `parentId.toULongLong()` (decimal).

Builds a `state` JSON object (returned as **indented** JSON inside a text result, `:1358-1359`):
- `baseAddress`: `"0x" + upper-hex(tree.baseAddress)`.
- `baseAddressFormula`: present only if non-empty.
- `viewRootId`: decimal string of `ctrl->viewRootId()`.
- `nodeCount`: `tree.nodes.size()` (int).
- `provider`: `{}` if no provider, else `{name, writable(bool), live(bool), size(int), kind(string)}` from Provider virtuals.
- `sources`: array of `{index, kind, displayName, active(bool)}` over `ctrl->savedSources()` with `active = (i==activeSourceIndex())`.
- `selectedNodeIds`: array of decimal-string ids from `ctrl->selectedIds()`.
- `filePath`: `doc->filePath`.
- `modified`: bool.
- `undoAvailable` / `redoAvailable`: from `doc->undoStack.canUndo()/canRedo()`.
- `statusText`: `m_mainWindow->m_appStatus`.

**Tree (if `includeTree`)** — paginated BFS (`:1275-1356`):
1. Build `childMap: parentId → [node indices]` once over all nodes.
2. BFS queue seeded with `{filterParentId, depth=0}`. For each dequeued entry: skip if `depth > maxDepth`. For each child index:
   - `totalCount++` (counts every node matching the depth filter, regardless of pagination).
   - If `totalCount <= offset` → skip emit but enqueue children (if `depth+1 <= maxDepth`).
   - If `emitted >= limit` → skip emit but enqueue children.
   - Else emit: `nj = n.toJson()`. If `!includeMembers`: replace `enumMembers` array with `enumMemberCount`, and `bitfieldMembers` with `bitfieldMemberCount`. If kind is `Struct`/`Array`: add `computedSize = tree.structSpan(n.id, &childMap)` and `childCount = childMap[n.id].size()`. Append; `emitted++`. Enqueue children if within depth.
3. `treeObj` = `{baseAddress: hex(no 0x, lowercase), baseAddressFormula?, nextId: decimal-string(tree.m_nextId), nodes: [...], returned: emitted, total: totalCount, nextOffset?: offset+emitted (only if emitted<total)}`.
   - Note the inner `treeObj.baseAddress` is **lowercase hex without `0x`** (`QString::number(...,16)`), while the top-level `state.baseAddress` uses `"0x"+upperHex`. Preserve this inconsistency for byte parity.

**BFS subtlety:** it's a breadth-first, depth-limited walk; nodes are emitted in BFS order, and pagination counts ALL depth-matching nodes (even skipped ones) so `total`/`nextOffset` are stable across pages.

`Node::toJson()` field set (`core.h:263-313`) — relevant for the wire format:
- Always: `id`(dec-str), `kind`(name str), `name`, `parentId`(dec-str), `offset`(int), `arrayLen`(int), `strLen`(int), `collapsed`(bool), `refId`(dec-str), `elementKind`(name str).
- Conditional: `structTypeName` (if non-empty), `classKeyword` (if non-empty AND != "struct"), `isStatic` (only if true), `offsetExpr` (if non-empty), `isRelative` (only if true), `ptrDepth` (only if >0), `enumMembers` (array `[{name, value(dec-str)}]` if non-empty), `bitfieldMembers` (array `[{name, bitOffset(int), bitWidth(int)}]` if non-empty), `comment` (if non-empty), `bigEndian` (only if true).

### 5.2 `tree.apply` — `toolTreeApply` (`:1366-1782`)
**Schema** (`:457-466`): props `tabIndex` (int), `operations` (array of objects), `macroName` (string). `required: ["operations"]`.

Args: `ops = args["operations"].toArray()`, `macroName = args["macroName"].toString("MCP batch")`. If `ops.isEmpty()` → `makeTextResult("No operations provided", true)`.

**Phase 1 — reserve insert IDs** (`:1381-1388`): iterate ops; for every op with `op=="insert"`, call `tree.reserveId()` and store in `placeholders["$<i>"]` keyed by **operation index `i`** (NOT the count of inserts). So `$0` = the op at index 0 if it's an insert, etc. This lets later ops reference an insert by its index via `$N`.

**Phase 2 — execute inside an undo macro** (`:1390-1758`):
- If not slow mode: `ctrl->setSuppressRefresh(true)` (batch UI refresh).
- `doc->undoStack.beginMacro(macroName)`.
- Iterate ops by index `i`. Every 100 ops if `ops.size()>200`, update status + `processEvents(..., 5ms)` (keep UI alive).
- Dispatch on `op["op"]` string. Each op resolves its target `nodeId` via `resolvePlaceholder` then `indexOfId`; if not found, append a message to `skippedOps` and continue. Each successful op pushes an `RcxCommand` (undo command) onto the stack and increments `applied`.

Operation types (op object fields → behavior):
- **`insert`** (`:1409-1483`): builds a `Node`:
  - `id` = reserved `$i` (or a fresh `reserveId()` fallback).
  - `kind = kindFromString(op["kind"] or "Hex64")`.
  - `name = op["name"]`.
  - `parentId` resolved via placeholder of `op["parentId"]` (default "0"); if unresolved placeholder → skip; if non-zero and not found in tree → skip ("parentId '...' not found").
  - `offset = parseInteger(op["offset"],0)`.
  - `structTypeName`, `classKeyword` from strings.
  - `strLen = qBound(1, parseInteger(strLen,64), 1000000)`.
  - `elementKind = kindFromString(op["elementKind"] or "UInt8")`.
  - `arrayLen = qBound(1, parseInteger(arrayLen,1), kMaxArrayLen)`.
  - `ptrDepth = qBound(0, parseInteger(ptrDepth,0), 2)`.
  - `isStatic = op["isStatic"].toBool(false)`, `offsetExpr` string, `isRelative` bool.
  - `enumMembers`: array `[{name, value}]` → `QPair<QString,int64_t>` (value via `parseInteger`).
  - `bitfieldMembers`: array `[{name, bitOffset, bitWidth}]`; `bitOffset = qBound(0,…,255)` as uint8, `bitWidth = qBound(1,…,64)` as uint8.
  - `refId` resolved via placeholder (default "0"); unresolved → skip.
  - **Auto-place**: if `offset < 0`, compute `maxEnd` = max over siblings of `(sibling.offset + size)` (size via `structSpan` for containers else `byteSize`), then align up to `alignmentFor(kind)`: `offset = (maxEnd+align-1)/align*align`.
  - Push `RcxCommand(ctrl, cmd::Insert{n,{}})`. If `parentId==0 && kind==Struct` → remember `lastRootStructId=n.id`.
- **`remove`** (`:1484-1498`): find node; collect `subtreeIndices(id)` into a `QVector<Node> subtree`; push `cmd::Remove{id, subtree, {}}`.
- **`rename`** (`:1499-1510`): push `cmd::Rename{id, oldName, op["name"]}`.
- **`change_kind`** (`:1511-1522`): `newKind = kindFromString(op["kind"])`; push `cmd::ChangeKind{id, oldKind, newKind, {}}`.
- **`change_offset`** (`:1523-1534`): push `cmd::ChangeOffset{id, oldOffset, parseInteger(op["offset"])}`.
- **`change_base`** (`:1535-1542`): NOT keyed to a node. `newBase = op["baseAddress"].toString().toULongLong(nullptr,16)` (hex, NO `0x` handling — must be bare hex); `newFormula = op["formula"]` (optional); push `cmd::ChangeBase{tree.baseAddress(old), newBase, oldFormula, newFormula}`. Always counts as applied.
- **`change_struct_type`** (`:1543-1555`): push `cmd::ChangeStructTypeName{id, old, op["structTypeName"]}`.
- **`change_class_keyword`** (`:1556-1568`): push `cmd::ChangeClassKeyword{id, old, op["classKeyword"]}`.
- **`change_pointer_ref`** (`:1569-1581`): `refStr = resolvePlaceholder(op["refId"] or "0")`; push `cmd::ChangePointerRef{id, oldRefId, refStr.toULongLong()}`.
- **`change_array_meta`** (`:1582-1596`): `newElemKind = kindFromString(op["elementKind"])`; `newLen = qBound(1, parseInteger(arrayLen,1), kMaxArrayLen)`; push `cmd::ChangeArrayMeta{id, oldElemKind, newElemKind, oldArrayLen, newLen}`.
- **`collapse`** (`:1597-1608`): push `cmd::Collapse{id, oldCollapsed, op["collapsed"].toBool()}`.
- **`change_enum_members`** (`:1609-1627`): parse `op["members"]` array `[{name, value}]` into `QVector<QPair<QString,int64_t>>`; push `cmd::ChangeEnumMembers{id, old, new}`.
- **`change_offset_expr`** (`:1628-1640`): push `cmd::ChangeOffsetExpr{id, old, op["offsetExpr"]}`.
- **`toggle_static`** (`:1641-1653`): push `cmd::ToggleStatic{id, old, op["isStatic"].toBool()}`.
- **`toggle_relative`** (`:1654-1666`): push `cmd::ToggleRelative{id, old, op["isRelative"].toBool()}`.
- **`group_into_union`** (`:1667-1680`): collect `op["nodeIds"]` (each placeholder-resolved) into a `QSet<uint64_t>`; if `>=2` → `ctrl->groupIntoUnion(ids)` (NOT via undo command — direct controller call); else skip "needs >= 2 nodeIds".
- **`dissolve_union`** (`:1681-1691`): resolve `nodeId`; if found → `ctrl->dissolveUnion(unionId)` (direct controller call); else skip.
- **unknown op** → skip "unknown op '<type>'".

**Slow-mode visual feedback** (`:1696-1757`): if `m_slowMode && applied>0`, un-suppress + `refresh()` after each op, optionally do a 125 ms focus-glow animation per affected node (skips animation for a paired op on the same node), then re-suppress. Pure UI; the Rust port can ignore slow/presentation mode (gate behind a flag, no-op by default).

**Finalize** (`:1760-1781`):
- `endMacro()`; if not slow mode `setSuppressRefresh(false)`.
- If `lastRootStructId` → `ctrl->setViewRootId(lastRootStructId)` (auto-switch view to a newly created root struct).
- `ctrl->refresh()`.
- Build `assignedIds = {"$0":"id", …}` from the placeholder map (decimal-string ids).
- `msg = "Applied <applied> operations"`; if any skipped → `"\nSkipped <n>:\n" + skippedOps.join("\n")`.
- Result = `makeTextResult(msg, isError = (!skippedOps.isEmpty() && applied==0))` then add `result["assignedIds"] = assignedIds`. (isError only when ALL ops failed.)

**Invariant:** the whole batch is one undo macro (atomic for undo/redo). Operations are applied best-effort: invalid ops are skipped and reported, valid ones still apply. Insert IDs are pre-reserved so `$N` forward references work within the same batch.

`kindFromString` (`core.h:117-121`): matches `KindMeta.name` exactly (case-sensitive); **unknown → `NodeKind::Hex8`** (silent fallback). Valid names listed in the schema description (`:455-456`): Hex8/16/32/64, Int8/16/32/64, UInt8/16/32/64, Float, Double, Bool, Pointer32/64, FuncPtr32/64, Vec2/3/4, Mat4x4, UTF8, UTF16, Struct, Array. (Full enum also has Hex128/Int128/UInt128/Float16, `core.h:24-34`.)

### 5.3 `source.switch` — `toolSourceSwitch` (`:1788-1835`)
**Schema** (`:474-487`): `tabIndex`, `sourceIndex`(int), `filePath`(string), `pid`(int), `processName`(string), `allViews`(bool).

Resolution priority:
1. **`sourceIndex` present** → bounds-check against `ctrl->savedSources()`; out of range → error result. If `allViews` true → for every tab `t.ctrl->switchSource(idx)`, else `ctrl->switchSource(idx)`. Return "Switched to source <idx> (<displayName>)".
2. **`pid` present** → `attachViaPlugin("processmemory", "<pid>:<name>")` — **OUT OF SCOPE** (live process plugin). After attach, if provider has nonzero `base()`, set `tree.baseAddress = provider->base()`, clear formula, refresh. Return "Attached to process …". *Port: stub — return isError "live process attach not supported in this build", or route through Provider trait if a process provider exists.*
3. **`filePath` present** → `doc->loadData(path)` (load a binary file as the data source — IN SCOPE: the file provider), `ctrl->refresh()`, return "Loaded file: <path>".
4. Else → error "Provide sourceIndex, filePath, or pid".

### 5.4 `hex.read` — `toolHexRead` (`:1889-1990`)
**Schema** (`:514-530`): `tabIndex`, `offset`(int), `length`(int 1–4096 default 64), `baseRelative`(bool), `interpret`(bool). `required:["offset","length"]`.

Behavior:
- `prov = tab->doc->provider`; if null → "No provider" error.
- `offset = parseInteger(offset)`; `length = qBound(1, parseInteger(length,64), 4096)`; `baseRel = baseRelative.toBool()`.
- If `baseRel` → `offset += tree.baseAddress`.
- If `offset < 0 || !prov->isReadable(offset, length)` → "Cannot read at offset <offset>" error.
- `data = prov->readBytes(offset, length)` (returns zero-filled buffer if the read fails internally; see Provider §6).
- Build a **hex dump string** (16 bytes/line): each line `"<addr in 8-hex, zero-padded>: "` then 16 two-hex byte cells (`"%02x "`), with a gap space after the 8th byte (`if (j==7) dump += " "`), missing cells shown as three spaces; then `" |"` + printable-ASCII (`0x20..0x7e`, else `.`) + `"|\n"`. Address column is `offset + i` (already includes baseAddress if baseRel). (`:1909-1926`)
- **Interpretations block** (`:1929-1962`): appends `"\n--- Interpretations at offset ---\n"` then `u8`, and if length permits: `u16`, `u32`(+hex), `i32`, `f32`, `u64`(+hex), `f64`. For u64 ≥ base and < base+provSize adds `"ptr?: LIKELY (within provider range)\n"`. Then counts leading printable ASCII bytes; if ≥4 adds `"str?: <n> printable ASCII bytes\n"`. All little-endian via `memcpy`.
- **Per-field inference** (only if `interpret==true` and `data.size>=8`, `:1965-1987`): chunk size = 8 if pointerSize≥8 else 4; for each aligned chunk run `inferTypes` (type-inference engine — OUT OF SCOPE) and append `"+0xNN: [<label>] score=<n>  <preview>"`. *Port: gate `interpret` behind the type-inference subsystem; without it, omit this block.*
- Returns `makeTextResult(dump)` (not an error).

**Rust:** core read goes through `Provider::read/readBytes`/`is_readable`. The dump/interpretation formatting is pure string work — reproduce exactly (the text is the tool output the LLM reads).

### 5.5 `hex.write` — `toolHexWrite` (`:1996-2032`)
**Schema** (`:539-552`): `tabIndex`, `offset`(int), `hexBytes`(string), `baseRelative`(bool). `required:["offset","hexBytes"]`.

Behavior:
- `offset = parseInteger(offset)`; `hexStr = args["hexBytes"].toString().remove(' ')` (strip all spaces).
- If `baseRelative` → `offset += tree.baseAddress`.
- If `hexStr.size()` odd → "Hex string must have even length" error.
- Parse byte-by-byte: `hexStr.mid(i,2).toUInt(&ok,16)`; on parse failure → "Invalid hex at position <i>" error. Build `newBytes`.
- If `!prov || !prov->isWritable()` → "Provider is not writable" error.
- If `!prov->isReadable(offset, newBytes.size())` → "Offset out of range" error. (Uses isReadable as a range check even for write.)
- `oldBytes = prov->readBytes(offset, len)` (capture for undo); push `cmd::WriteBytes{offset, oldBytes, newBytes}` onto undo stack (`core.h:1090`: `struct WriteBytes { uint64_t addr; QByteArray oldBytes, newBytes; }`).
- Return "Wrote <n> bytes at offset 0x<hex>".

**Invariant:** writes go through the undo stack (reversible), capturing the old bytes first. Requires writable provider. Rust: `Provider::write`, wrapped in an undo command.

### 5.6 `status.set` — `toolStatusSet` (`:2038-2059`)
**Schema** (`:616-626`): `tabIndex`, `text`(string), `target`(enum: `commandRow`|`statusBar`|`both`). `required:["text"]`.
- `text = args["text"]`; `target = args["target"].toString("both")`.
- If `commandRow`/`both`: for each pane with an editor, `editor->setCommandRowText("[▸] [Claude: <text>]")` (the prefix is UTF-8 `\xE2\x96\xB8` = ▸). (UI — stub or route to a status callback.)
- If `statusBar`/`both`: `m_mainWindow->setAppStatus(text)` (sets `m_appStatus`, shown in window status bar).
- Returns "Status set: <text>".

### 5.7 `ui.action` — `toolUiAction` (`:2065-2178`)
**Schema** (`:639-649`): `tabIndex`, `action`(string), `nodeId`(string), `filePath`(string). `required:["action"]`.

`action = args["action"]`, `nodeIdStr = args["nodeId"]`. Resolve tab (doc, ctrl). Action handlers:
- `undo` → if no tab error; if `!canUndo()` "Nothing to undo" error; else `undoStack.undo()` → "Undo performed".
- `redo` → symmetric → "Redo performed".
- `refresh` → `ctrl->refresh()` → "Refreshed".
- `set_view_root` → `ctrl->setViewRootId(nodeIdStr.toULongLong())` → "View root set to <id>".
- `scroll_to_node` → `ctrl->scrollToNodeId(id)` → "Scrolled to node <id>".
- `export_cpp` → C++ codegen (generator subsystem). With optional `nodeId` → `renderCpp(tree, nid, aliases, asserts)` (single struct; empty → "Node not found or not a struct" error); else `renderCppAll(...)`. `asserts` from `QSettings("Reclass","Reclass").value("generatorAsserts", false)`. If code > 65536 bytes → truncate to 64 KB + "... truncated (N bytes total, showing first 64KB)\nUse nodeId param…". Returns the code as text. (Generator is a separate subsystem; MCP just calls it.)
- `save_file` → `m_mainWindow->project_save()` → "Saved".
- `new_file` → `project_new()` → "New project created".
- `open_file` → requires `filePath` (else error) → `project_open(path)` → "Opened: <path>".
- `collapse_node` / `expand_node` → find node (error if not found); push `cmd::Collapse{id, oldCollapsed, true/false}`; `refresh()` → "Collapsed/Expanded <id>".
- `select_node` → `ctrl->clearSelection()`; `editor=ctrl->primaryEditor()`; if editor `ctrl->handleNodeClick(editor, -1, nid, Qt::NoModifier)` → "Selected node <id>".
- `reset_tracking` → for **all** tabs, `ctrl->resetChangeTracking()` → "Value tracking reset on all N tabs." (Clears value histories used by `node.history`.)
- unknown action → "Unknown action: <action>" error.

The `action` set advertised in the schema description (`:633-637`): undo, redo, new_file, open_file, save_file, save_file_as (note: `save_file_as` is advertised but **not implemented** in the switch — falls to "Unknown action"), export_cpp, set_view_root, scroll_to_node, collapse_node, expand_node, select_node, refresh, reset_tracking.

### 5.8 `tree.search` — `toolTreeSearch` (`:2184-2242`)
**Schema** (`:659-671`): `tabIndex`, `query`(string), `kindFilter`(string), `limit`(int default 20 max 100).
- `query`, `kindFilter` strings; `limit = qBound(1, parseInteger(limit,20), 100)`.
- If both `query` and `kindFilter` empty → error "Provide 'query' … and/or 'kindFilter' …".
- Build `childCounts: parentId→count`.
- For each node: if `kindFilter` non-empty and `kindToString(n.kind) != kindFilter` → skip. If `query` non-empty: match if `n.name` OR `n.structTypeName` contains `query` (case-insensitive); else skip.
- Emit `{id(dec-str), name, kind(str), parentId(dec-str), offset(int)}` plus conditionals: `structTypeName` (if non-empty), `classKeyword` (if non-empty), `childCount` (if Struct/Array), `enumMemberCount` (if any), `bitfieldMemberCount` (if any). Stop at `limit`.
- Output `{results:[…], count:int, query:string, kindFilter?:string}` as **indented** JSON text.

### 5.9 `node.history` — `toolNodeHistory` (`:2248-2279`)
**Schema** (`:682-692`): `nodeIds`(array of strings), `tabIndex`. `required:["nodeIds"]`.
- `histMap = tab->ctrl->valueHistory()` — a `QHash<uint64_t, ValueHistory>`.
- `requestedIds = args["nodeIds"].toArray()`; if empty → "nodeIds array is required." error.
- For each requested id string → `nodeId = idStr.toULongLong()`; look up in histMap.
  - `entries`: if found, `forEachWithTime` (newest→oldest, up to `uniqueCount` ≤ 10) → `[{value:string, timestamp:int64-msec}]`.
  - `nodeResult = {entries, heatLevel: (found? hist.heatLevel():0), uniqueCount: (found? hist.uniqueCount():0)}`.
  - `result[idStr] = nodeResult` (keyed by the original id string).
- Output the whole `result` object as **compact** JSON text.

`ValueHistory` (`core.h:867-923`): ring buffer, `kCapacity=10`. `record(v)` ignores no-change writes (dedup vs last). `uniqueCount() = min(count, 10)`. `heatLevel()`: count≤1→0(static), ==2→1(cold), ≤4→2(warm), else→3(hot). `forEachWithTime` iterates newest→oldest. Timestamps are `QDateTime::currentMSecsSinceEpoch()` (ms since Unix epoch). **Port:** `Vec`/ring-buffer of `(String, i64-millis)`; the heat thresholds and dedup-on-same-value behavior are load-bearing.

### 5.10 Notifications — `notifyTreeChanged` / `notifyDataChanged` (`:3499-3509`)
- `notifyTreeChanged()`: if no clients return; else `sendNotification("notifications/resources/updated", {"uri":"project://tree"})`.
- `notifyDataChanged()`: same but `{"uri":"project://data"}`.
Both broadcast to all `initialized` clients (compact JSON + `\n`). Wired from `RcxDocument::documentChanged` (`main.cpp:3233`). MCP method name is the standard MCP `notifications/resources/updated`; the two `uri` values are the only distinction.

### 5.11 `mcp.reconnect` — `toolReconnect` (`:2432-2442`)
- If `m_currentSender` null → "No client connected." error.
- Schedule (`QTimer::singleShot(0)`): if the socket is still a client → `disconnectFromServer()`. This defers disconnect until AFTER the response is flushed (the reply is sent first, then the client is dropped). Returns "Disconnected. The MCP client will exit; …".
- **Tested** by `toolsCall_reconnect` and `toolsCall_reconnect_otherClientUnaffected`: the client receives the result *then* gets disconnected; other clients are unaffected. Port: send the reply, then close just that one connection on a deferred task.

### 5.12 Out-of-scope tool handlers (schema-only / stub)
`source.modules`, `scanner.scan`, `scanner.scan_pattern`, `process.info`, `symbols.load/lookup/importType`, `node.read_value`, `analysis.*`, `ui.byte_selection/set_byte_selection/inspect`, `theme.get/set/save/revert`, `bookmarks.*`, `refs.find`. These depend on live-process providers, PDB symbol stores, the scanner panel, the type-inference engine, RTTI, the editor/theme UI, or bookmarks/refs which are separate subsystems. For the port: keep their `tools/list` schemas verbatim; implement handlers as thin stubs returning `isError` "not available" (or, where they only need the `Provider` trait + tree, optionally implement them later). `source.modules`/`process.info` read `Provider::enumerateRegions()`/`peb()`/`tebs()` which are empty defaults for the benign providers.

---

## 6. Provider interface (consumed by hex.read/hex.write/project.state)

`class Provider` (`src/providers/provider.h:38-153`) — the abstraction MCP uses. Pure-virtual + defaulted virtuals:
- `bool read(uint64_t addr, void* buf, int len) const` = 0 — the only mandatory read.
- `int size() const` = 0.
- `bool write(uint64_t, const void*, int)` (default false / no-op).
- `bool isWritable() const` (default false).
- `QString name() const` (default empty).
- `bool isLive() const` (default false).
- `QString kind() const` (default `"File"`).
- `int pointerSize() const` (default 8).
- `uint64_t base() const` (default 0).
- `getSymbol`, `symbolToAddress`, `enumerateRegions` (default `{}`), `peb` (0), `tebs` ({}), `enumerateModules`, kernel-paging helpers — all OUT OF SCOPE / empty defaults for benign providers.

Non-virtual convenience used by MCP:
- `isReadable(addr, len)` (`:122-126`): `len<=0 → (len==0)`; else `addr <= size() && len <= size()-addr`. (Range check against the provider's flat size; for absolute-VA providers `size()` is the addressable span.)
- `readBytes(addr, len)` (`:142-148`): allocate `len` (uninitialized), `read(...)`; **on read failure fill with `\0`** (never returns garbage). Returns empty for `len<=0`.
- `writeBytes(addr, d)` → `write(d.constData(), d.size())`.

**Rust port:** define a `Provider` trait with `read(&self, addr: u64, buf: &mut [u8]) -> bool`, `size() -> u64/usize`, `write` (default false), `is_writable`, `is_live`, `name`, `kind`, `pointer_size`, `base`, and provided methods `is_readable`, `read_bytes` (zero-fill on failure), `write_bytes`. In-scope concrete providers: file-backed, in-memory buffer, snapshot, null. All MCP memory access goes through this trait — the OS/process plugins are stubs behind it.

---

## 7. Qt → Rust mapping

| Qt type / API | Role here | Rust equivalent |
|---|---|---|
| `QLocalServer` / `QLocalSocket` | named-pipe server + client | `interprocess::local_socket::{Listener, Stream}` (pipe name `ReclassMcpBridge`) |
| `QLocalServer::WorldAccessOption` | permissive ACL on pipe | `interprocess` listener options / SDDL (Windows), socket file mode (Unix) |
| `QLocalServer::removeServer(name)` | delete stale unix socket | `std::fs::remove_file` of the socket path (Unix); no-op Windows |
| `QByteArray` (`readBuffer`, lines) | byte buffers, line framing | `Vec<u8>` / `BytesMut`; split on `\n` |
| `QJsonObject`/`QJsonArray`/`QJsonValue`/`QJsonDocument` | JSON build/parse | `serde_json::{Value, Map, json!}`; `from_slice`, `to_vec` |
| `QJsonDocument::Compact` | wire serialization | `serde_json::to_vec` (compact by default) |
| `QJsonDocument::Indented` | pretty tool-text payloads | `serde_json::to_string_pretty` (Qt uses 4-space indent — match if exact bytes matter) |
| `QString` | text | `String` / `&str` |
| `QTimer` (10ms poll; singleShot(0)/(125)/(150)) | polling + deferred disconnect + UI delays | dedicated stdin thread (no poll needed); a deferred close for reconnect; UI delays can be dropped |
| `QCoreApplication::processEvents` | keep UI alive in long batches | N/A (port doesn't share the GUI thread the same way) |
| `QObject`/signals (`readyRead`, `disconnected`, `errorOccurred`) | async I/O callbacks | async tasks / mio / std threads reading the stream |
| `QSettings` | `autoStartMcp`, `generatorAsserts` | a config struct / settings file |
| `QDateTime::currentMSecsSinceEpoch()` | history timestamps | `SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64` |
| `qBound(lo,v,hi)` | clamp | `v.clamp(lo,hi)` |
| `QString::number(x,16)` | hex format | `format!("{:x}")` / `{:X}` (watch case + `0x` presence per call site) |
| `toULongLong()` / `toLongLong(&ok,base)` | string→int | `u64::from_str_radix` / `str::parse`, with the 0x-prefix logic in `parseInteger` |

`recommended_rust_crates`: `serde`, `serde_json`, `interprocess` (named pipes / local sockets), `bytes` (optional buffering), `thiserror` (error codes).

---

## 8. Concurrency / threading

- The real `McpBridge` runs **entirely on the Qt GUI thread**. It is NOT multithreaded; "concurrency" is via the Qt event loop. Multiple clients are multiplexed by the event loop, but **request processing is serialized globally** by `m_processing`/`m_pendingRequests` (see §2.4). Any handler that spins a nested event loop (`tree.apply` slow-mode, scanner) re-enters the event loop, which is exactly why the manual queue exists.
- `rcx-mcp-stdio` is single-threaded around a Qt event loop with a 10 ms stdin poll timer.
- **Rust port options:** (a) keep a single dispatch thread/task that owns the document and processes one request at a time (matches semantics most directly), with a per-connection reader feeding a shared queue; or (b) async with a global mutex/`Mutex<AppState>` held across each request. Either way: **global serialization of `tools/call` is required**, replies route only to the originating connection, and notifications broadcast to all initialized connections. The Rust port likely runs the MCP server on its own thread distinct from any UI thread, so it needs to marshal document access (channel to the UI/model thread) — but must still serialize requests.

---

## 9. Subtle behaviors the tests rely on (`tests/test_mcp.cpp`)

The test uses a **`MockMcpServer`** that re-implements the same multi-client architecture (not the real `McpBridge`), so a few error codes differ from the real server. Behaviors the port must satisfy:

1. **Line framing**: requests/responses are newline-delimited JSON; reader buffers partial data and splits on `\n`, trimming each line, skipping empty lines. (`MockMcpServer::processSocket`, mirrors `onReadyRead`.)
2. **Invalid JSON** (`protocol_invalidJson`): a non-object / unparseable line → response with `error.code == -32700`. (Matches real `processLine`.)
3. **initialize** returns `result` with `serverInfo.name` and marks the client initialized. `initializedCount` increments.
4. **tools/list** returns `result.tools` (non-empty array).
5. **Unknown method** (`singleClient_unknownMethod`) → `error.code == -32601`. (Matches real.)
6. **Multi-client**: two clients can connect and both initialize independently (`multiClient_bothInitialize`).
7. **Disconnect one**: disconnecting client 1 leaves client 2 working; `clientCount` decrements (`multiClient_disconnectOne`).
8. **Notification broadcast** (`multiClient_notificationBroadcast`): a `notifications/resources/updated` broadcast reaches only **initialized** clients; an uninitialized third client receives **zero** lines. Notifications have a `method` and no `id`.
9. **Serial requests** (`multiClient_serialRequests`): interleaved requests from two clients each get their correct `id` back (no cross-talk) — the global serialization invariant.
10. **Server survives all-disconnect** (`allDisconnect_serverSurvives`): after every client leaves, a new client can connect and initialize.
11. **Notifications ignored** (`protocol_notificationsIgnored`): `notifications/initialized` and `notifications/cancelled` produce **no** response.
12. **reconnect** (`toolsCall_reconnect`, `..._otherClientUnaffected`): `tools/call name=mcp.reconnect` returns a `result` whose `content[0].text` contains "Disconnected", THEN the server disconnects **only that** client (deferred until after the reply is flushed). Other clients keep working.

**Mock-only error codes (NOT emitted by the real `McpBridge`, do not port from the mock):** the mock returns `-32600` for a missing `method` (`protocol_missingMethod`) and `-32602` for a missing tool name (`toolsCall_missingToolName`). The real server: a request with no `method` hits the `-32601 "Method not found: "` branch; a `tools/call` with no tool name reaches `handleToolsCall` and returns `-32601 "Unknown tool: "`. For a *faithful port of the real server*, match the real behavior (`-32601`); the test's `-32600`/`-32602` expectations are artifacts of the mock and would only matter if porting the mock for tests. The Rust port should keep its own tests aligned with whichever server it ships — but document this discrepancy.

---

## 10. Constants / magic values to preserve
- Pipe name: `"ReclassMcpBridge"`.
- `kMaxReadBuffer = 10 * 1024 * 1024` (10 MB) per-client read buffer cap; exceed → disconnect.
- `protocolVersion`: `"2024-11-05"`.
- `serverInfo`: `{name:"reclass-mcp", version:"1.0.0"}`.
- Capabilities: `{tools:{listChanged:false}}`.
- Notification method: `notifications/resources/updated`; uris `project://tree`, `project://data`.
- Client notification methods ignored: `notifications/initialized`, `notifications/cancelled`.
- `project.state`: limit clamp 1..500 (default 50), depth default 1.
- `hex.read`: length clamp 1..4096 (default 64).
- `tree.search`: limit clamp 1..100 (default 20).
- `node.history` / `ValueHistory`: capacity 10; heat thresholds (≤1→0, ==2→1, ≤4→2, else 3).
- `tree.apply`: status/process-events cadence every 100 ops when >200 ops; slow-mode 125 ms per op; presentation glow 150 ms.
- `export_cpp` truncation at 65536 bytes.
- JSON-RPC error codes: -32700, -32601, -32603 (real server).
- `parseInteger`: hex requires `0x`/`0X` prefix; numbers truncate; strings trimmed.
- `change_base` parses `baseAddress` as **bare hex** (base 16, no 0x stripping in `toULongLong(nullptr,16)`).

---

## 11. Open questions / port notes
- The `m_notifyTimer` (100 ms single-shot) is constructed and wired but never `start()`ed in this file; it appears to be a latent debounce. Verify whether any other TU starts it. The Rust port can safely use the direct notify path (`notifyTreeChanged`/`notifyDataChanged`) and treat the timer as optional/unused.
- `status.set` command-row prefix uses the UTF-8 bytes `\xE2\x96\xB8` (▸); preserve if the command row is ported.
- The out-of-scope tools should still be advertised in `tools/list` verbatim so MCP hosts present the same toolset; their handlers degrade to `isError` results in this build.
- The real-vs-mock error-code discrepancy (§9) is the only place where the test file diverges from the real server; decide test expectations accordingly when porting tests.
