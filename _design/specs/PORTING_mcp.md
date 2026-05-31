# PORTING SPEC — MCP bridge (JSON-RPC 2.0 over named pipe)

Subsystem key: `mcp`. Target faithful 1:1 behavioral parity with the C++ source at
`/home/loke/reclass-cpp/src/mcp/mcp_bridge.{h,cpp}` and `tools/rcx-mcp-stdio.cpp`.

**Read alongside:** `_design/understand/mcp.md` (behavioral map — authoritative; this spec
references it by section), `_design/ARCHITECTURE.md` §2/§3 (module layout), `_design/crate_selection.md`
(interprocess/serde_json), `_oracle/RESULTS.md` (`test_mcp` golden: 16 pass / 1 fail).

> **Scope reminder.** The live OS/process/kernel/remote/WinDbg providers are OUT OF SCOPE.
> They are reached only through the abstract `Provider` trait. The MCP in-scope tool set is
> `project.state`, `tree.apply`, `source.switch` (file-only path), `hex.read` (core dump only),
> `hex.write`, `status.set`, `ui.action`, `tree.search`, `node.history`, the two notifications,
> and `mcp.reconnect`. The remaining tools are advertised verbatim in `tools/list` but their
> handlers are thin "not available in this build" `isError` stubs (or degrade through the
> `Provider`/tree).

---

## 0. Target crate / modules

Single package `reclass`, feature `mcp` (already declared:
`mcp = ["dep:interprocess"]`; deps `serde`/`serde_json` always on, `interprocess` optional).

| File | Maps C++ | Contents |
|---|---|---|
| `src/mcp/mod.rs` | `mcp_bridge.h` + lifecycle of `mcp_bridge.cpp` | `McpBridge` struct, `K_SOCKET_NAME`, public API (`start`/`stop`/`is_running`/`slow_mode`/`notify_*`), the listener accept loop, connection state. **Exists as a SKELETON — to be filled in.** |
| `src/mcp/transport.rs` | `onReadyRead`/`drainPendingRequests`/`onDisconnected`/`sendJson`/`sendNotification`/`okReply`/`errReply` | Line framing, the global serial request queue, JSON-RPC message construction, per-connection write. |
| `src/mcp/dispatch.rs` | `processLine`/`handleInitialize`/`handleToolsCall` | Top-level method routing + the `tools/call` name switch. |
| `src/mcp/schemas.rs` | `handleToolsList` (`:388-1074`) | The verbatim `tools/list` JSON (one builder fn per tool, returned in registration order). Pure data; no `Provider`/UI deps. |
| `src/mcp/tools.rs` | the in-scope `toolXxx` handlers + helpers `parseInteger`/`resolvePlaceholder`/`resolveTab`/`makeTextResult` | The load-bearing tool semantics. |
| `src/mcp/stubs.rs` | the out-of-scope `toolXxx` handlers | Each returns `make_text_result("<tool> is not available in this build", true)` (or a Provider/tree-only result where trivially possible — see §7). |
| `src/bin/reclass-mcp-bridge.rs` | `tools/rcx-mcp-stdio.cpp` | Standalone stdio↔socket relay binary. **Exists as a SKELETON.** |

`src/mcp/mod.rs` currently exports `K_SOCKET_NAME = "ReclassMcpBridge"` and a placeholder
`McpBridge { running, slow_mode }`. Keep `K_SOCKET_NAME` exactly; expand `McpBridge` as below.

**Module split rationale:** keep `schemas.rs` and the pure helpers free of any model/UI handle so
they unit-test under `--no-default-features` plus `--features mcp` without spinning a real socket.
The transport layer is tested over a real `interprocess` loopback (mirrors `test_mcp.cpp`).

---

## 1. The model-access boundary (the one porting decision the C++ doesn't force on us)

The C++ `McpBridge` lives **on the Qt GUI thread** and reaches into `MainWindow`/`TabState`/
`RcxDocument`/`Controller` synchronously. In Rust the bridge runs on **its own thread** (blocking
`interprocess`), so it cannot hold `&mut` to the app model directly. Model the boundary as a trait
the app implements; the bridge calls it while holding the global request lock. This preserves the
"one request in flight, globally" invariant and routes all model mutation through one place.

```rust
/// What the MCP bridge needs from the host app. Implemented by the app shell
/// (main.rs / controller layer). The bridge never touches gpui or MainWindow directly.
/// Mirrors the `MainWindow` + `TabState` surface that mcp_bridge.cpp consumes (mcp.md §4.3).
pub trait McpHost {
    fn tab_count(&self) -> usize;
    /// `MainWindow::activeTab()` index, or None.
    fn active_tab_index(&self) -> Option<usize>;
    /// `MainWindow::project_new()` — auto-create on demand (resolveTab step 4).
    fn project_new(&mut self) -> usize;          // returns the new tab's index
    fn project_open(&mut self, path: &str);
    fn project_save(&mut self);
    /// Borrow a tab's document+controller for one operation.
    fn with_tab<R>(&mut self, idx: usize, f: &mut dyn FnMut(&mut TabState) -> R) -> Option<R>;
    /// `m_appStatus` getter/setter (status.set, project.state.statusText).
    fn app_status(&self) -> String;
    fn set_app_status(&mut self, text: &str);
    /// UI-only; default no-op in headless tests.
    fn set_command_row_text(&mut self, tab: usize, text: &str) {}
    /// For ui.action reset_tracking across all tabs.
    fn reset_change_tracking_all(&mut self) -> usize; // returns tab count
}
```

`TabState` here is the Rust counterpart of `MainWindow::TabState`: it owns `doc: Document`
(which has `tree: NodeTree`, `provider: Option<Box<dyn Provider>>`, `undo_stack`, `file_path`,
`modified`, `type_aliases`) and `ctrl: Controller`. These already exist in `src/controller.rs` /
`src/core/`. The MCP module only *consumes* them; it does not define them. (If the controller
crate does not yet expose a single `TabState`, add a thin one there, not in `mcp`.)

For pure logic/transport tests, provide an in-memory `TestHost` implementing `McpHost` over a
`Vec<TabState>` — no GUI required.

---

## 2. Constants (preserve byte-for-byte — mcp.md §10)

```rust
pub const K_SOCKET_NAME: &str = "ReclassMcpBridge";          // already present
const K_MAX_READ_BUFFER: usize = 10 * 1024 * 1024;          // 10 MB per-client cap
const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "reclass-mcp";
const SERVER_VERSION: &str = "1.0.0";
const NOTIF_METHOD: &str = "notifications/resources/updated";
const URI_TREE: &str = "project://tree";
const URI_DATA: &str = "project://data";
// JSON-RPC error codes (REAL server — not the mock's): -32700, -32601, -32603.
```

The long `initialize.instructions` string (`mcp_bridge.cpp:359-379`) must be copied verbatim
(it is the LLM-facing contract). Put it in a `const INSTRUCTIONS: &str` in `dispatch.rs`. It is
reproduced in full in §6.1.

---

## 3. Transport / framing (`transport.rs`) — maps `onReadyRead` etc.

### 3.1 Data structures

```rust
struct ClientState {
    stream: interprocess::local_socket::Stream, // or a write-half handle
    read_buffer: Vec<u8>,
    initialized: bool,
    id: ClientId,                                // dense u64 handed out on accept
}
struct PendingRequest { client: ClientId, line: Vec<u8> }
```

Because `interprocess` blocking streams aren't `QObject`s with signals, the cleanest faithful
model (matches mcp.md §8 option (a)) is:

- **One acceptor thread** runs `Listener::accept()` in a loop. Each accepted stream gets a
  `ClientId` and a **reader thread** that does blocking reads, splitting on `\n`, and forwards each
  complete (trimmed, non-empty) line to a single **central `mpsc::Sender<Event>`**.
  `Event = NewClient{id, write_half} | Line{id, Vec<u8>} | Disconnected{id}`.
- **One dispatch thread** owns all `ClientState`, the `read_buffer`s, the `m_processing`-equivalent,
  the `m_pending_requests` queue, and the `McpHost`. It is the *only* thread that touches the model.
  It receives `Event`s on the channel and processes them serially — this is exactly the C++ "single
  GUI thread + serial queue" guarantee, made explicit.

The per-client `read_buffer` lives in the reader thread (line splitting happens there); the central
queue only carries already-framed lines. The 10 MB overflow guard is applied in the reader: if the
accumulated buffer exceeds `K_MAX_READ_BUFFER` before a `\n` is seen, log a warning, send
`Disconnected{id}`, and drop the stream (mirrors `onReadyRead` `:187-191`).

> **Why the serial queue still matters even though we have a channel:** the C++ queue exists
> because a handler can re-enter the event loop. In Rust the dispatch thread is strictly serial by
> construction (it processes one `Event` at a time and never re-enters), so the explicit
> `m_processing`/`m_pending_requests` machinery is **subsumed** by the single dispatch loop. Keep
> the behavior (one request in flight globally; replies route to origin; FIFO ordering of queued
> lines) but you do NOT need the literal `m_processing` boolean — the channel + single consumer is
> the queue. Document this equivalence in a comment. (`multiClient_serialRequests` passes because
> each line is handled to completion before the next.)

### 3.2 Line framing (the exact rule, `onReadyRead :197-215`)

In the reader thread, per chunk read:
1. Append bytes to `read_buffer`.
2. Loop: find first `\n`. If none, break (wait for more).
3. `line = read_buffer[..idx]` **trimmed** (C++ `QByteArray::trimmed()` strips ASCII whitespace
   ≤ 0x20 from both ends — replicate with a trim of bytes `<= b' '`). Remove `..=idx` from buffer.
4. If `line.is_empty()` after trim → skip (continue). Else emit `Line{id, line}`.

The newline is **not** part of the forwarded line. Blank lines are dropped.

### 3.3 Dispatch-thread loop (maps `onReadyRead` tail + `drainPendingRequests`)

```
loop over channel events:
  NewClient{id, write_half}     → clients.insert(id, ClientState{ initialized:false, ...})
  Disconnected{id}              → pending.retain(|r| r.client != id);  clients.remove(id)
  Line{id, line}:
      if !clients.contains(id) { drop }      // disconnected meanwhile
      current_sender = Some(id)
      process_line(&line)                    // §6: sets up replies via sendJson(current_sender)
      current_sender = None
      // no recursive drain needed: events already serialized on the channel
```

`onDisconnected` purges queued requests from that client BEFORE removing it (mcp.md §2.4) — with the
channel model, queued `Line` events from a gone client are simply skipped when dequeued
(`!clients.contains(id)`), which is equivalent. Exercised by `multiClient_disconnectOne`,
`allDisconnect_serverSurvives`.

### 3.4 `send_json` / `send_notification` (`:261-282`)

```rust
fn send_json(&mut self, obj: &serde_json::Value) {
    let Some(id) = self.current_sender else { return };          // null → drop
    let Some(cs) = self.clients.get_mut(&id) else { return };    // not a current client → drop
    let mut data = serde_json::to_vec(obj).unwrap();             // COMPACT (default)
    // tracing::debug!(">> {}", &String::from_utf8_lossy(&data)[..min(200,..)])
    data.push(b'\n');
    let _ = cs.stream.write_all(&data);
    let _ = cs.stream.flush();                                    // best-effort; ignore write errors
}

fn send_notification(&mut self, method: &str, params: serde_json::Value) {
    let mut n = json!({"jsonrpc":"2.0","method":method});
    if !(params.is_object() && params.as_object().unwrap().is_empty()) {
        n["params"] = params;                                     // omit "params" if empty (`:273`)
    }
    let mut data = serde_json::to_vec(&n).unwrap(); data.push(b'\n');
    for cs in self.clients.values_mut() {
        if cs.initialized { let _=cs.stream.write_all(&data); let _=cs.stream.flush(); }
    }
}
```

Replies go ONLY to the originating client. Notifications broadcast ONLY to `initialized` clients —
uninitialized clients get zero bytes (`multiClient_notificationBroadcast` third client gets 0
lines). Write errors are swallowed (matches Qt fire-and-forget `write`+`flush`).

**Compact serialization byte-parity note:** `serde_json::to_vec` emits compact JSON with keys in
insertion order *if* you build with `serde_json::Map` (preserve-order is the default for `Value`
when the `preserve_order` feature is on; otherwise `Value`'s map is a `BTreeMap` → sorted keys).
Qt's `QJsonObject` serializes keys **alphabetically sorted**. So for the *wire envelope* and any
object built with `serde_json::Map`/`json!`, enable `serde_json`'s default (BTreeMap) ordering to
match Qt's sorted-key output, OR build objects so the sorted order is intentional. **Decision:** do
NOT enable `preserve_order` for the MCP module's wire objects — Qt sorts keys, and `serde_json`'s
default `BTreeMap` also sorts keys, so default `serde_json` matches Qt. (Verify in tests by
comparing exact bytes for `okReply`/`errReply`.) For the pretty tool-text payloads see §6.3.

### 3.5 `ok_reply` / `err_reply` / `make_text_result` (`:245-294`)

```rust
fn ok_reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id": id, "result": result})
}
fn err_reply(id: &Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc":"2.0","id": id, "error": {"code": code, "message": msg}})
}
fn make_text_result(text: &str, is_error: bool) -> Value {
    let mut r = json!({"content":[{"type":"text","text": text}]});
    if is_error { r["isError"] = json!(true); }   // key present only when true
    r
}
```

`id` is echoed unchanged (number / string / null). For parse-error and exception cases the id is
JSON `null` (`Value::Null`), matching `QJsonValue()`.

---

## 4. McpBridge lifecycle (`mod.rs`) — maps `start`/`stop`/ctor/dtor

```rust
pub struct McpBridge {
    inner: Option<RunningBridge>,   // None = stopped (is_running == false)
    slow_mode: bool,
}
struct RunningBridge {
    shutdown: Arc<AtomicBool>,
    acceptor: JoinHandle<()>,
    event_tx: mpsc::Sender<Event>,  // dispatch thread owns the receiver + host
    dispatch: JoinHandle<()>,
}
```

`start()` (`:108-127`):
- No-op if already running (`inner.is_some()`).
- On Unix, remove a stale socket file (mcp.md §2.2; `interprocess` may need
  `NameType::reclaim`/explicit removal — use the crate's `Listener` builder with the
  reclaim/permissions option; on Windows named pipes don't persist so it's a no-op). World-access:
  use the `interprocess` listener option for permissive ACL (SDDL on Windows / socket file mode on
  Unix) to mirror `QLocalServer::WorldAccessOption`. If the listener fails to bind: `tracing::warn!`,
  leave `inner = None`, return (silent failure — app keeps running).
- Spawn acceptor + dispatch threads; store handles.

`stop()` (`:129-144`): set `shutdown`, drop the listener (unblocks accept), close all client
streams, join threads, clear queue, set `inner = None`.

`is_running()` = `inner.is_some()`. `slow_mode`/`set_slow_mode` = trivial getters (`:24-25`).

The constructor's `m_notifyTimer` (100 ms single-shot) is **constructed but never started** in the
C++ (mcp.md §2.3, §11): it is a latent debounce. **OMIT it.** Use the direct notify path only.

`notify_tree_changed()` / `notify_data_changed()` (`:3499-3509`): if no clients, return; else send a
notification message into the dispatch thread (an `Event::Notify{uri}`) so the broadcast happens on
the owning thread. Maps:
- tree → `send_notification(NOTIF_METHOD, json!({"uri": URI_TREE}))`
- data → `send_notification(NOTIF_METHOD, json!({"uri": URI_DATA}))`

Wiring (out of MCP scope, noted for main.rs): `autoStartMcp` (default true), the toggle action
(stop+start), `setSlowMode`, and `Document::documentChanged → notify_tree_changed` (`main.cpp`).

---

## 5. The stdio bridge binary (`src/bin/reclass-mcp-bridge.rs`) — maps `rcx-mcp-stdio.cpp`

A tiny standalone relay. `interprocess` client, two directions, line-buffered on `\n`.

```
fn main() -> ExitCode {
    // Windows: stdin/stdout are already binary in Rust (no CRLF translation) — no _setmode needed.
    let stream = match LocalSocketStream::connect(name(K_SOCKET_NAME), timeout=5s) {
        Ok(s) => s,
        Err(e) => { eprintln!("[ReclassMcpBridge] Failed to connect... {e}"); return ExitCode::from(1); }
    };
    eprintln!("[ReclassMcpBridge] Connected to ReclassMcpBridge");
    let (mut rd, mut wr) = stream.split();   // or clone via try_clone

    // Thread A: socket → stdout. Read chunks, append to buf, emit complete lines
    //   (INCLUDING the trailing '\n') to stdout, flush each. On EOF/err: eprintln, quit.
    // Thread B (main): stdin → socket. Blocking read of stdin in 4096 chunks; split complete
    //   lines (KEEP '\n'); write+flush to socket. On EOF/err: quit.
    // Either direction ending → process exits.
}
```

Differences from C++ that are behaviorally equivalent (mcp.md §2.1):
- **No 10 ms poll timer.** Rust owns a dedicated stdin thread doing *blocking* reads, so no
  `PeekNamedPipe`/`select` polling is needed. Same end-to-end behavior: lines forwarded as soon as a
  `\n` arrives.
- **Binary mode:** Rust stdio does not translate newlines on Windows, so the `_setmode(_O_BINARY)`
  is unnecessary; do NOT add CRLF handling.
- **Forwarded lines keep their trailing `\n`** in BOTH directions (C++ uses `left(idx+1)`). This is
  the one place where the newline is retained, unlike the server's `onReadyRead` which strips it.
- Connect timeout **5 s**; on failure print to stderr and **exit code 1**. On disconnect/socket
  error: print to stderr, exit (0).

Use a 5 s connect deadline; `interprocess` connect is blocking, so wrap with a retry/deadline loop
or its connect-with-timeout if available, else attempt-once and treat `WouldBlock`/`NotFound`
during a short retry window as "still starting".

---

## 6. Dispatch (`dispatch.rs`) — `process_line` / `handle_initialize` / `handle_tools_call`

### 6.1 `process_line(&mut self, line: &[u8])` (`:300-340`)

Wrap the body so any panic/error path produces an internal-error reply (Rust has no exceptions;
emulate the C++ try/catch by making handlers return `Result` and converting `Err` → `-32603`, and
optionally `catch_unwind` around the dispatch for true parity with `catch(...)`).

```
1. parse: serde_json::from_slice::<Value>(line)
     - if Err OR not an object → send_json(err_reply(&Value::Null, -32700, "Parse error")); return
2. let id = req.get("id").cloned().unwrap_or(Value::Null);   // pass through as-is
3. let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
4. if method == "notifications/initialized" || method == "notifications/cancelled" { return; } // no reply
5. match method:
     "initialize"  => { set status; send_json(handle_initialize(&id, params)); clear status; }
     "tools/list"  => { set status; send_json(handle_tools_list(&id)); clear status; }
     "tools/call"  => { send_json(handle_tools_call(&id, params)); }
     _             => { send_json(err_reply(&id, -32601, &format!("Method not found: {method}"))); }
6. on internal Err(e) → send_json(err_reply(&Value::Null, -32603, &format!("Internal error: {e}")))
```

`params` = `req.get("params").and_then(Value::as_object).cloned().unwrap_or_default()`.

**Error-code fidelity (mcp.md §9):** the *real* server uses only `-32700`, `-32601`, `-32603`.
A request with no `method` → empty string → `-32601 "Method not found: "`. A `tools/call` with no
tool name → reaches `handle_tools_call` → `-32601 "Unknown tool: "`. The test mock's `-32600`/
`-32602` are mock artifacts — our Rust tests assert the REAL behavior; see §8 test plan note.

The status set/clear calls (`setMcpStatus`/`clearMcpStatus`) are UI; route to
`host.set_app_status` style hooks or no-op. Not wire-observable.

### 6.2 `handle_initialize(&id, _params) -> Value` (`:346-382`)

- Mark current client `initialized = true`.
- Return `ok_reply(id, json!({...}))` with EXACTLY (key order is alphabetized by serde default,
  matching Qt):
```jsonc
{
  "protocolVersion": "2024-11-05",
  "capabilities": {"tools": {"listChanged": false}},
  "serverInfo": {"name": "reclass-mcp", "version": "1.0.0"},
  "instructions": "<verbatim INSTRUCTIONS const, mcp_bridge.cpp:360-378>"
}
```
`params` ignored. The `INSTRUCTIONS` string is the multi-line guidance in `:360-378` — copy it byte
for byte (it includes literal `\n` joins between the C++ string-literal fragments; the result is one
string with embedded newlines exactly as concatenated in the source).

### 6.3 `handle_tools_list(&id) -> Value` (`:388-1074`)

Return `ok_reply(id, json!({"tools": [ ...descriptors ]}))`. Descriptors are appended in the exact
registration order (mcp.md §3.2): `project.state`, `tree.apply`, `source.switch`, `source.modules`,
`hex.read`, `hex.write`, `bookmarks.list/add/remove`, `refs.find`, `status.set`, `ui.action`,
`tree.search`, `node.history`, `scanner.scan`, `scanner.scan_pattern`, `mcp.reconnect`,
`process.info`, `symbols.load/lookup/importType`, `node.read_value`, `analysis.infer_types/
import_header/pointer_chain/find_overlaps/tree_summary/field_path`, `ui.byte_selection/
set_byte_selection/inspect`, `theme.get/set/save/revert`.

Each descriptor is `{name, description, inputSchema}` where `inputSchema` is a JSON-Schema object.
**The `tools` array is an ARRAY → element order is preserved by serde.** Within each descriptor
object the keys are sorted by serde (name/description/inputSchema → alphabetized to
description/inputSchema/name) — that matches Qt. Copy each `description` and `inputSchema` verbatim
from the cited lines (full text for in-scope tools is in mcp.md §5 and the source; for out-of-scope
tools the schema text is at the line ranges in mcp.md §3.2). Implement as one `fn tool_<name>()
-> Value` per tool returning the descriptor, then push into a `Vec<Value>` in order. This keeps the
giant verbatim blobs reviewable.

> **Pretty-printing of tool-text payloads:** several tools embed *indented* JSON in their `text`
> field (`project.state`, `tree.search`, `source.modules`, `process.info`). Qt's
> `QJsonDocument::Indented` uses **4-space** indentation, sorted keys, and a specific compaction of
> empty containers. `serde_json::to_string_pretty` uses **2-space** indentation. To match Qt's exact
> bytes, provide a small `qt_pretty(value: &Value) -> String` helper using
> `serde_json::Serializer::with_formatter(PrettyFormatter::with_indent(b"    "))` (4 spaces) over a
> sorted-key `Value`. **The LLM reads this text, so exactness matters only for the oracle tests, not
> for protocol correctness.** Note Qt renders empty arrays as `[\n    ]` vs serde `[]`; if a golden
> test trips on this, post-process or accept that tool-text whitespace is not behaviorally
> load-bearing (document the deviation). The *structure* (which keys, which values) must match.

### 6.4 `handle_tools_call(&id, params) -> Value` (`:1080-1151`)

```
let tool = params["name"].as_str().unwrap_or("");
let args = params["arguments"].as_object().cloned().unwrap_or_default();
host.set_app_status(&format!("MCP: {tool}"));    // UI; no processEvents in Rust
let result: Value = match tool {
    "project.state"  => tool_project_state(&args, host),
    "tree.apply"     => tool_tree_apply(&args, host),
    "source.switch"  => tool_source_switch(&args, host),
    "hex.read"       => tool_hex_read(&args, host),
    "hex.write"      => tool_hex_write(&args, host),
    "status.set"     => tool_status_set(&args, host),
    "ui.action"      => tool_ui_action(&args, host),
    "tree.search"    => tool_tree_search(&args, host),
    "node.history"   => tool_node_history(&args, host),
    "mcp.reconnect"  => self.tool_reconnect(&args),   // needs current_sender + deferred close
    // out-of-scope stubs (§7): source.modules, scanner.*, process.info, symbols.*,
    //   node.read_value, analysis.*, ui.byte_selection/set_byte_selection/inspect,
    //   theme.*, bookmarks.*, refs.find
    "source.modules" | "process.info" | ... => stub_not_available(tool),
    _ => return err_reply(id, -32601, &format!("Unknown tool: {tool}")),   // early return, NOT ok_reply
};
// presentation-mode glow (`:1127-1146`) → OMIT (pure UI, gated behind a flag, default off)
host.clear_mcp_status();
ok_reply(id, result)                              // tool results always wrapped in ok_reply
```

Tool-level failures are signaled by `result.isError == true` inside `content`, never by a JSON-RPC
`error`. Only an unknown tool name returns a JSON-RPC `error` (`-32601`).

---

## 7. Helpers + in-scope tools (`tools.rs`) — maps `:28-44`, `:1157-1206`, §5

### 7.1 `parse_integer(v: &Value, default: i64) -> i64` (`:28-44`)

```rust
fn parse_integer(v: Option<&Value>, default: i64) -> i64 {
    match v {
        None => default,
        Some(Value::Null) => default,
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() { return default; }
            let (neg, body) = ...; // toLongLong handles a leading '-'
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                i64::from_str_radix(hex, 16).unwrap_or(default)
            } else {
                s.parse::<i64>().unwrap_or(default)   // base 10
            }
        }
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64).unwrap_or(default), // TRUNCATE toward 0
        _ => default,
    }
}
```

Hex requires a `0x`/`0X` prefix (case-insensitive); decimal otherwise; trimmed; JSON numbers
truncate toward zero (C++ `static_cast<int64_t>(double)`). Note: Qt's `toLongLong(&ok,16)` on `"0x"`
slices off `"0x"` then parses the rest; an empty-after-prefix or unparseable body → default. Match
that. This helper is shared with the address parser (crate_selection.md notes it).

### 7.2 `resolve_placeholder(ref, map) -> (String, bool)` (`:1157-1169`)

```rust
fn resolve_placeholder(r: &str, map: &HashMap<String, u64>) -> (String, bool) {
    if let Some(rest) = r.strip_prefix('$') {
        let _ = rest;
        match map.get(r) {                 // key is the WHOLE "$N" string, incl. the '$'
            Some(id) => (id.to_string(), true),
            None => (r.to_string(), false), // unresolved → ok=false, returns ref unchanged
        }
    } else {
        (r.to_string(), true)              // not a placeholder → as-is, ok=true
    }
}
```

### 7.3 `resolve_tab(args, host) -> Option<usize>` (`:1175-1206`)

Returns a tab index (the C++ returns a `TabState*`; in Rust we resolve to an index then borrow via
`host.with_tab`). Priority:
1. If `args` has `"tabIndex"` and `host` has that index → use it.
2. Else `host.active_tab_index()` if Some.
3. Else `0` if `host.tab_count() > 0`.
4. Else `host.project_new()` (auto-create) → returns its index (0).

Auto-create-on-demand is contract: a tool never fails purely for "no document open". Almost every
tool begins: `let Some(tab) = resolve_tab(args, host) else { return make_text_result("No active
tab", true); };` (the `None` branch is effectively unreachable after step 4 but kept for parity).

### 7.4 In-scope tool semantics

The exact behavior of each is in **mcp.md §5** (read it; this lists only the Rust signature, the
load-bearing edge cases, and which existing Rust API it calls). All take
`(args: &Map<String,Value>, host: &mut dyn McpHost) -> Value` and access the tab via
`host.with_tab(idx, |tab| { ... })`.

**`tool_project_state` (`:1212-1360`, mcp.md §5.1)**
- Args: `depth`(default 1), `includeTree`(default true), `includeMembers`(default false),
  `limit = parse_integer(limit,50).clamp(1,500)`, `offset = parse_integer(offset,0).max(0)`,
  `filterParentId = if parentId empty {0} else parentId.parse::<u64>()` (decimal).
- Top-level `state` object: `baseAddress = format!("0x{:X}", tree.base_address)` (UPPER, with 0x);
  `baseAddressFormula` only if non-empty; `viewRootId` = decimal string of `ctrl.view_root_id()`;
  `nodeCount` = `tree.nodes.len()` (integer); `provider` = `{}` if none else
  `{name, writable, live, size, kind}`; `sources` array `{index, kind, displayName, active}` over
  `ctrl.saved_sources()` (`active = i == ctrl.active_source_index()`); `selectedNodeIds` = decimal
  strings of `ctrl.selected_ids()`; `filePath`; `modified`; `undoAvailable`/`redoAvailable` from
  `doc.undo_stack.can_undo()/can_redo()`; `statusText = host.app_status()`.
- Tree (if includeTree): paginated depth-limited **BFS** (mcp.md §5.1 step 2 — copy exactly). Build
  `child_map: HashMap<u64, Vec<usize>>` once. Queue seeded `{filterParentId, 0}`. Counting rule:
  `total_count++` for every depth-matching child; emit only when `total_count > offset` and
  `emitted < limit`; always enqueue children while `depth+1 <= max_depth`. Per emitted node:
  `node.to_json()` (exists), then if `!includeMembers` replace `enumMembers`→`enumMemberCount` and
  `bitfieldMembers`→`bitfieldMemberCount`; if Struct/Array add `computedSize = tree.struct_span(id)`
  and `childCount = child_map[id].len()`.
  > **Note:** Rust `struct_span(id)` takes no `child_map` arg (the C++ passes `&childMap` only as an
  > optimization; the value is identical). Fine to call the no-arg version.
- `treeObj`: `baseAddress = format!("{:x}", tree.base_address)` (**lowercase, NO 0x** — deliberately
  different from the top-level field; preserve), `baseAddressFormula?`, `nextId` = decimal string of
  `tree.next_id` (the Rust field for `m_nextId`), `nodes`, `returned = emitted`,
  `total = total_count`, `nextOffset = offset + emitted` only if `emitted < total`.
- Return `make_text_result(&qt_pretty(&state), false)`.

**`tool_tree_apply` (`:1366-1782`, mcp.md §5.2)** — the largest. Two phases inside `with_tab`:
- Phase 1: iterate `operations`; for each op with `op == "insert"`, `tree.reserve_id()` and store in
  `placeholders["$<i>"]` keyed by the **operation index** `i` (not insert count).
- Phase 2: `doc.undo_stack.begin_macro(macroName)` (default `"MCP batch"`). For each op resolve its
  `nodeId` via `resolve_placeholder` then `tree.index_of_id`; missing → push a message to
  `skipped_ops` and continue. Each successful op constructs the matching `core::command::Command`
  variant (the enum already exists — `Insert{node, off_adjs}`, `Remove{node_id, subtree, off_adjs}`,
  `Rename`, `ChangeKind`, `ChangeOffset`, `ChangeBase`, `ChangeStructTypeName`, `ChangeClassKeyword`,
  `ChangePointerRef`, `ChangeArrayMeta`, `Collapse`, `ChangeEnumMembers`, `ChangeOffsetExpr`,
  `ToggleStatic`, `ToggleRelative`) and pushes it onto the undo stack via the controller's
  `RcxCommand` wrapper; `applied += 1`. `group_into_union`/`dissolve_union` call the controller
  directly (NOT undo commands), needing ≥2 ids for union.
- Build each `insert` Node from op fields with the exact clamps (mcp.md §5.2):
  `kind = kind_from_string(op.kind | "Hex64")`; `strLen = parse_integer(strLen,64).clamp(1,1_000_000)`;
  `elementKind = kind_from_string(op.elementKind | "UInt8")`;
  `arrayLen = parse_integer(arrayLen,1).clamp(1, K_MAX_ARRAY_LEN)`;
  `ptrDepth = parse_integer(ptrDepth,0).clamp(0,2)`; `bitOffset.clamp(0,255)`, `bitWidth.clamp(1,64)`;
  parentId/refId via `resolve_placeholder` (default `"0"`, unresolved → skip). **Auto-place:** if
  `offset < 0`, `maxEnd` = max over siblings of `(offset + size)` (size = `struct_span` for
  containers else `byte_size`), then `offset = round_up(maxEnd, alignment_for(kind))`. If
  `parentId==0 && kind==Struct`, remember `last_root_struct_id`.
- `change_base`: NOT keyed to a node; `newBase = u64::from_str_radix(op.baseAddress, 16)` (**bare
  hex, no 0x stripping** — `toULongLong(nullptr,16)`; if the string starts with `0x` Qt's base-16
  parse actually accepts the `0x` prefix — verify: `QString::toULongLong("0x400000",nullptr,16)`
  parses fine because Qt recognizes the `0x` for base 16. Use `u64::from_str_radix(s.trim_start_
  matches("0x"), 16)` to match). Always counts as applied.
- Finalize (`:1760-1781`): `end_macro()`; if `last_root_struct_id` → `ctrl.set_view_root_id(it)`;
  `ctrl.refresh()`; build `assignedIds = {"$0":"<id>", ...}` (decimal strings);
  `msg = format!("Applied {applied} operations")` + (if skipped) `"\nSkipped {n}:\n" + skipped.join("\n")`;
  `result = make_text_result(&msg, !skipped.is_empty() && applied == 0)` then
  `result["assignedIds"] = assignedIds`. (`isError` only when ALL ops failed.)
- Slow-mode / presentation glow / `setSuppressRefresh` / periodic `processEvents`: **OMIT** (UI;
  gate behind a default-off flag). The batch is one atomic undo macro; best-effort op application.
- `K_MAX_ARRAY_LEN = 1_000_000` (core has `kMaxArrayLen`); `kind_from_string` unknown → `Hex8`
  (silent fallback — already implemented in `core::kind::kind_from_string`).

**`tool_source_switch` (`:1788-1835`, mcp.md §5.3)** — in-scope ONLY for the `filePath` path:
- `sourceIndex` present → bounds-check `ctrl.saved_sources()`; OOB → error; else
  `ctrl.switch_source(idx)` (or all tabs if `allViews`); return `"Switched to source {idx} ({name})"`.
- `pid` present → **STUB**: return `make_text_result("Live process attach is not available in this
  build", true)` (the C++ calls `attachViaPlugin("processmemory", ...)` — out of scope).
- `filePath` present → `doc.load_data(path)` (file provider — in scope), `ctrl.refresh()`, return
  `"Loaded file: {path}"`.
- else → error `"Provide sourceIndex, filePath, or pid"`.

**`tool_hex_read` (`:1889-1990`, mcp.md §5.4)** — core dump in scope; `interpret` block out of scope:
- `prov` none → `"No provider"` error. `offset = parse_integer(offset)`;
  `length = parse_integer(length,64).clamp(1,4096)`; if `baseRelative` → `offset += tree.base_address`.
- If `offset < 0 || !prov.is_readable(offset as u64, length)` → `"Cannot read at offset {offset}"`.
- `data = prov.read_bytes(offset as u64, length)` (zero-fills on internal read failure — already
  the trait's provided behavior).
- **Hex dump** (16 bytes/line) — reproduce the format EXACTLY (the LLM reads it):
  - line addr: `format!("{:08x}: ", offset + i)` (8-wide zero-pad lowercase hex of `offset+i`).
  - 16 cells: byte → `format!("{:02x} ", b)`; missing → `"   "` (3 spaces); after the 8th cell
    (`j == 7`) append one extra `" "` (gap).
  - then `" |"`, printable ASCII (`0x20..=0x7e` else `.`) for the present bytes, `"|\n"`.
- **Interpretations block** (`data.len() >= 1`): `"\n--- Interpretations at offset ---\n"`, then
  `u8`, and if length permits `u16`, `u32`(+` (0x..)`), `i32`, `f32`, `u64`(+` (0x..)`), `f64`,
  all **little-endian** (`from_le_bytes` over the first N bytes). The `f32`/`f64` are formatted via
  `QString::number((double)fv)` / `QString::number(dv)` — Qt's default double formatting is
  `%.6g`-ish (shortest round-trip up to 6 significant digits). **Match Qt's `QString::number(double)`
  formatting** (use a `qt_number_double` helper, or assert against the oracle). For `u64` in
  `[base, base+provSize)` add `"ptr?: LIKELY (within provider range)\n"`. Count leading printable
  ASCII; if `>= 4` add `"str?: {n} printable ASCII bytes\n"`.
- **Per-field inference** (`interpret==true && len>=8`): OUT OF SCOPE (type-inference engine). Gate
  behind `cfg(feature="...")`/the typeinfer module; **without it, omit this block entirely** (the
  C++ default `interpret` is false, so the common case is unaffected).
- Return `make_text_result(&dump, false)`.

**`tool_hex_write` (`:1996-2032`, mcp.md §5.5)**
- `offset = parse_integer(offset)`; `hexStr = args.hexBytes.replace(' ', "")` (strip ALL spaces).
- if `baseRelative` → `offset += tree.base_address`.
- odd length → `"Hex string must have even length"` error.
- parse pairs: `u8::from_str_radix(&hexStr[i..i+2], 16)`; on err → `"Invalid hex at position {i}"`.
- `prov` none or `!prov.is_writable()` → `"Provider is not writable"`.
- `!prov.is_readable(offset as u64, len)` → `"Offset out of range"` (read-range check even for write).
- `old = prov.read_bytes(offset, len)`; push `Command::WriteBytes{addr: offset as u64, old_bytes:
  old, new_bytes}` onto the undo stack (the only command that touches the provider).
- return `"Wrote {n} bytes at offset 0x{offset:x}"` (lowercase hex).

**`tool_status_set` (`:2038-2059`, mcp.md §5.6)**
- `text`; `target = args.target | "both"`.
- commandRow/both → for each pane with an editor `host.set_command_row_text(tab, &format!("[\u{25B8}]
  [Claude: {text}]"))` (the ▸ is U+25B8 = UTF-8 `E2 96 B8`). UI; no-op in headless.
- statusBar/both → `host.set_app_status(&text)`.
- return `"Status set: {text}"`.

**`tool_ui_action` (`:2065-2178`, mcp.md §5.7)** — switch on `action`:
- `undo`/`redo` → guard `can_undo`/`can_redo` (else `"Nothing to undo/redo"`), then `undo()/redo()`.
- `refresh` → `ctrl.refresh()`.
- `set_view_root` → `ctrl.set_view_root_id(nodeId.parse::<u64>())`.
- `scroll_to_node` → `ctrl.scroll_to_node_id(id)`.
- `export_cpp` → generator subsystem: `renderCpp(tree, nid, aliases, asserts)` (single, empty →
  `"Node not found or not a struct"`) or `renderCppAll(...)`; `asserts` from settings
  (`generatorAsserts`, default false); if `code.len() > 65536` truncate to 64 KB + the exact
  `"\n\n... truncated ({total} bytes total, showing first 64KB)\nUse nodeId param to export a single
  struct."` suffix. Uses the `generator` module (already in the port).
- `save_file`/`new_file`/`open_file` → `host.project_save/new/open` (`open_file` needs `filePath`).
- `collapse_node`/`expand_node` → find node (error if missing) → push `Command::Collapse{node_id,
  old_state: node.collapsed, new_state: true/false}` → `ctrl.refresh()`.
- `select_node` → `ctrl.clear_selection()`; primary editor → `handleNodeClick(...)` (UI; the
  selection-set update is the observable part).
- `reset_tracking` → `host.reset_change_tracking_all()` for ALL tabs → `"Value tracking reset on all
  {n} tabs."`.
- unknown action → `"Unknown action: {action}"` error. (`save_file_as` is advertised but
  unimplemented → falls here, exactly like C++.)

**`tool_tree_search` (`:2184-2242`, mcp.md §5.8)**
- `query`, `kindFilter`; `limit = parse_integer(limit,20).clamp(1,100)`. Both empty → error
  `"Provide 'query' (name substring) and/or 'kindFilter' (e.g. 'Struct')"`.
- `child_counts: HashMap<u64,i32>` from all nodes' `parent_id`.
- For each node: if `kindFilter` non-empty and `kind_to_string(n.kind) != kindFilter` → skip; if
  `query` non-empty and neither `n.name` nor `n.struct_type_name` contains `query`
  (case-insensitive) → skip. Emit `{id, name, kind, parentId, offset}` + conditional
  `structTypeName`/`classKeyword`/`childCount`(Struct/Array)/`enumMemberCount`/`bitfieldMemberCount`.
  Stop at `limit`.
- Output `{results, count, query, kindFilter?}` via `qt_pretty`.

**`tool_node_history` (`:2248-2279`, mcp.md §5.9)**
- `histMap = ctrl.value_history()` (`&HashMap<u64, ValueHistory>` — exists).
- `nodeIds` array; empty → `"nodeIds array is required."` error.
- For each id string: `nodeId = idStr.parse::<u64>()`; look up. `entries` = (if found)
  `for_each_with_time(|val, msec| ...)` → `[{value, timestamp}]` (newest→oldest, ≤ uniqueCount ≤ 10).
  `nodeResult = {entries, heatLevel: found?h.heat_level():0, uniqueCount: found?h.unique_count():0}`.
  `result[idStr] = nodeResult` (keyed by original string).
- Output the whole object as **COMPACT** JSON text (not pretty): `make_text_result(&serde_json::
  to_string(&result), false)`. (`ValueHistory::for_each_with_time`, `heat_level`, `unique_count`
  already exist; heat thresholds ≤1→0, ==2→1, ≤4→2, else→3 and dedup-on-same-value are in
  `core::value_history`.)

**`self.tool_reconnect(&args)` (`:2432-2442`, mcp.md §5.11)** — needs bridge state, not just host:
- if `current_sender` is None → `"No client connected."` error.
- Schedule a deferred close of ONLY that client AFTER the reply is flushed: push an
  `Event::CloseClient{id}` onto the dispatch channel (it is processed after the current
  `send_json`), or set a `pending_close = Some(id)` that the dispatch loop honors right after
  sending this tool's reply. Either way the reply goes out first, then that one client is dropped;
  other clients are unaffected.
- return `"Disconnected. The MCP client will exit; your IDE may restart it and reconnect to Reclass."`.

---

## 7.5 Out-of-scope stubs (`stubs.rs`) — mcp.md §5.12

Advertised in `tools/list` verbatim, but handlers return `isError` "not available":

```rust
fn stub_not_available(tool: &str) -> Value {
    make_text_result(&format!("{tool} is not available in this build"), true)
}
```

Tools: `source.modules`, `scanner.scan`, `scanner.scan_pattern`, `process.info`, `symbols.load`,
`symbols.lookup`, `symbols.importType`, `node.read_value`, `analysis.infer_types`,
`analysis.import_header`, `analysis.pointer_chain`, `analysis.find_overlaps`, `analysis.field_path`,
`analysis.tree_summary`, `ui.byte_selection`, `ui.set_byte_selection`, `ui.inspect`, `theme.get`,
`theme.set`, `theme.save`, `theme.revert`, `bookmarks.list`, `bookmarks.add`, `bookmarks.remove`,
`refs.find`.

> Some of these (`analysis.find_overlaps`, `analysis.tree_summary`, `analysis.field_path`,
> `tree`-only `refs.find`/`bookmarks.*`, `source.modules` over the trait's empty
> `enumerate_regions`) need ONLY the tree/`Provider` trait and could be promoted to real handlers in
> a later pass without touching out-of-scope code. For THIS port they stay stubs. `analysis.
> import_header` would need the `imports` module (in scope under the `imports` feature) and could be
> wired later. Leave them all as `stub_not_available` for now; the spec flags the upgrade path.

---

## 8. Error-handling strategy

- **No exceptions:** handlers return `Value` (a tool result, possibly `isError`) and never propagate
  Rust errors to the caller. Genuinely fallible internal steps (model borrow, JSON shape) return an
  `isError` text result, not a JSON-RPC error.
- **JSON-RPC errors** are emitted ONLY for: parse failure / non-object (`-32700`), unknown method
  (`-32601`), unknown tool (`-32601`), and a catch-all internal error (`-32603`). Match the REAL
  server codes (mcp.md §9) — the mock's `-32600`/`-32602` are NOT ported.
- **Panic safety:** wrap `handle_tools_call`/`handle_initialize` invocation in `std::panic::
  catch_unwind` (mirrors C++ `catch(...)`); on panic emit `err_reply(null, -32603, "Internal
  error")`. Set a panic hook to keep the dispatch thread alive.
- **Write/flush errors** to a client stream are swallowed (the client likely disconnected; the
  disconnect event will clean it up). Matches Qt's fire-and-forget.
- **Connect failure** in the standalone bridge → stderr message + exit code 1; listener bind failure
  in the server → warn + silent no-op (app keeps running).

---

## 9. TEST PLAN — translating `tests/test_mcp.cpp`

`test_mcp.cpp` drives a **`MockMcpServer`** (a re-implementation of the multi-client architecture,
not the real `McpBridge`) — so a few mock error codes differ. **Golden (`_oracle/RESULTS.md`):
`test_mcp` = 16 pass / 1 fail.** The one upstream FAILURE is `multiClient_bothInitialize`
(`clientCount()==1`, expected 2) — a headless-Linux socket-timing artifact, recorded as golden. Our
Rust port should make multi-client work reliably, so our equivalent test asserts the *intended*
behavior (2 clients) and is allowed to pass where the C++ flaked. Document this in the test.

Two test layers:

### 9.1 Transport/protocol integration tests (real `interprocess` loopback)

Spin a real `McpBridge` over a `TestHost` on a **unique per-test socket name** (the C++ uses
`"ReclassMcpTest"`; use `format!("ReclassMcpTest-{pid}-{n}")` to avoid cross-test collisions), then
connect raw `interprocess` client(s) and exchange newline-delimited JSON. Provide Rust helpers
mirroring the C++ ones: `make_client`, `rpc(stream, req) -> Value` (write line, read one response
line), `init_rpc(stream)`, `drain(stream, ms) -> Vec<Value>`.

| C++ test (`test_mcp.cpp`) | Rust `#[test]` | Asserts |
|---|---|---|
| `singleClient_initialize` (`:173`) | `single_client_initialize` | `initialize` → `result.serverInfo.name == "reclass-mcp"` (NOTE: real server, not the mock's `"mock-mcp"`); id echoed (`1`); the client is now `initialized` (1 initialized client). |
| `singleClient_toolsList` (`:183`) | `single_client_tools_list` | `tools/list` → `result.tools` is a NON-empty array; id echoed (`2`). Real server returns ~36 tools (assert `>= 1` like the mock, plus optionally assert the first tool name is `"project.state"` for stronger parity). |
| `singleClient_unknownMethod` (`:192`) | `single_client_unknown_method` | method `"bogus"` → `error.code == -32601`. |
| `multiClient_bothInitialize` (`:200`) | `multi_client_both_initialize` | two clients connect; **`client_count() == 2`**; both `initialize` return `result`; `initialized_count() == 2`. (Upstream FAILS here on headless Linux — golden RESULTS.md; our impl must PASS. Add a comment citing the oracle.) |
| `multiClient_disconnectOne` (`:212`) | `multi_client_disconnect_one` | disconnect client 1 → `client_count() == 1`; client 2's `tools/list` still works (id `5`, non-empty tools). |
| `multiClient_notificationBroadcast` (`:224`) | `multi_client_notification_broadcast` | with 2 initialized + 1 uninitialized client, trigger `notify_tree_changed()`; clients 1 & 2 each receive ≥1 line whose last is `method == "notifications/resources/updated"`; client 3 receives **0** lines. (Use the bridge's real notify path instead of the mock's `broadcast`.) |
| `multiClient_serialRequests` (`:245`) | `multi_client_serial_requests` | interleaved `tools/list` from two clients each return their own id (`10`, `20`) — no cross-talk (the global-serialization invariant). |
| `allDisconnect_serverSurvives` (`:256`) | `all_disconnect_server_survives` | after all clients leave (`client_count()==0`), a fresh client connects and `initialize` returns `result`; `client_count()==1`. |
| `protocol_invalidJson` (`:268`) | `protocol_invalid_json` | send `"this is not json\n"` → response `error.code == -32700`. |
| `protocol_missingMethod` (`:285`) | `protocol_missing_method` | request with no `method` key → **REAL server: `error.code == -32601`** (message `"Method not found: "`). MOCK expects `-32600`; we assert the real server's `-32601` and add a comment per mcp.md §9. |
| `protocol_notificationsIgnored` (`:293`) | `protocol_notifications_ignored` | sending `notifications/initialized` and `notifications/cancelled` produces **0** response lines. |
| `toolsCall_unknownTool` (`:305`) | `tools_call_unknown_tool` | `tools/call name="nonexistent.tool"` → `error.code == -32601`. |
| `toolsCall_missingToolName` (`:315`) | `tools_call_missing_tool_name` | `tools/call` with no `name` → **REAL server: `error.code == -32601`** (`"Unknown tool: "`). MOCK expects `-32602`; assert real `-32601` + comment. |
| `toolsCall_reconnect` (`:325`) | `tools_call_reconnect` | `tools/call name="mcp.reconnect"` returns `result` whose `content[0].text` contains `"Disconnected"` (id `7`); THEN server disconnects that client (`client_count()==0` after a short wait); a reconnect works and `tools/list` returns tools. |
| `toolsCall_reconnect_otherClientUnaffected` (`:356`) | `tools_call_reconnect_other_client_unaffected` | c1 calls reconnect; after wait `client_count()==1`; c2's `tools/list` still works (id `2`). |

### 9.2 Pure-logic unit tests (no socket) — call handlers directly over a `TestHost`

These cover the in-scope tool *semantics* the C++ test file does NOT exercise (the mock has no real
tools). Drive `handle_tools_call`/the `tool_*` fns directly with a `TestHost` holding a small
hand-built `NodeTree` + a `BufferProvider`.

- `parse_integer`: `"0x10"`→16, `"0X1F"`→31, `"42"`→42, `"  7 "`→7, `""`→default, `"zzz"`→default,
  `3.9`(number)→3 (truncate), `-2.9`→-2, `null`/absent→default.
- `resolve_placeholder`: `"$0"` present → decimal id, ok=true; `"$9"` absent → `"$9"`, ok=false;
  `"123"` → `"123"`, ok=true.
- `make_text_result`: no `isError` key when false; `isError:true` present when true; content shape.
- `ok_reply`/`err_reply`: exact compact bytes including `jsonrpc`/`id`/`result`|`error`; id passthrough
  for number/string/null. (Assert against `serde_json::to_vec` — sorted keys match Qt.)
- `project.state`: build a 3-level tree; assert `nodeCount`, `baseAddress` top-level
  `"0x{:X}"` vs inner `treeObj.baseAddress` `"{:x}"` (the case/0x asymmetry), pagination
  (`limit`/`offset`/`returned`/`total`/`nextOffset`), `includeMembers` toggling
  `enumMembers`↔`enumMemberCount`, `computedSize`/`childCount` on containers, depth limiting via BFS
  ordering.
- `tree.apply`: a batch with `insert` (using `$0` parentId forward ref), `rename`+`change_kind` on
  the same node, a deliberately-bad op → assert `applied` count, `skipped` list, `assignedIds`,
  `isError` only when all fail, and that the whole batch is one undo macro (one `undo()` reverts
  everything). `change_base` bare-hex parse. Auto-place on `offset < 0`.
- `hex.read`: over a `BufferProvider` with known bytes — assert the exact dump string (addr column,
  the `j==7` gap, missing-cell spacing, ASCII gutter) and the Interpretations block
  (u8/u16/u32/i32/f32/u64/f64 values, `ptr?`/`str?` lines). This is the place to lock float
  formatting against a golden snapshot. `baseRelative` offset add. Out-of-range → `isError`.
- `hex.write`: even/odd length, bad hex position message, writable check, range check, undo capture
  (push `WriteBytes`, undo restores old bytes), return message hex format.
- `tree.search`: query substring (name + structTypeName, case-insensitive), `kindFilter`, `limit`
  clamp, both-empty error, conditional fields, output shape.
- `node.history`: record a few values into a `ValueHistory`, assert `entries` ordering (newest
  first), `heatLevel`/`uniqueCount`, dedup-on-same-value, COMPACT output, unknown id → empty
  entries + heat 0.
- `ui.action`: each action's message + effect (undo/redo guards, set_view_root, collapse/expand push
  + refresh, reset_tracking count, unknown action error, save_file_as → unknown).
- `resolve_tab`: tabIndex / active / first / auto-create precedence; auto-create on empty host.
- `handle_initialize` returns the exact `protocolVersion`/`serverInfo`/`capabilities` and the
  verbatim `instructions` (snapshot the string).
- `handle_tools_list` returns the tools in the exact registration order with the exact `name`s
  (snapshot the array of names; optionally snapshot one full descriptor's `inputSchema`).

No golden FIXTURE files exist for MCP in `_oracle/fixtures/` (the oracle only captured pass/fail
counts for `test_mcp`), so the in-scope-tool snapshots are authored from the C++ source semantics and
locked as Rust `insta`/inline snapshots; the transport tests mirror `test_mcp.cpp` directly.

---

## 10. Work order (small, independently verifiable steps)

1. **Constants + helpers** (`tools.rs`): `parse_integer`, `resolve_placeholder`, `make_text_result`,
   `ok_reply`/`err_reply`. Unit-test (§9.2 first 4 bullets). No socket, no host.
2. **`schemas.rs`**: verbatim `tools/list` (one fn per tool, in order) + `handle_tools_list`.
   Snapshot test for the name list + order. Pure data.
3. **`McpHost` trait + `TestHost`** (in `mcp` test support): in-memory tabs over `NodeTree` +
   `BufferProvider`. Enables logic tests without GUI.
4. **`dispatch.rs`**: `process_line`, `handle_initialize`, `handle_tools_call` skeleton routing to a
   `stub_not_available` for every tool. Verify method routing + error codes via direct calls.
5. **In-scope tools** (`tools.rs`), one at a time, each with its §9.2 unit test:
   `project.state` → `tree.search` → `node.history` → `hex.read` → `hex.write` → `ui.action` →
   `status.set` → `source.switch`(file path) → `tree.apply` (largest, do last). `mcp.reconnect`
   needs bridge state — stub its model side and finish it in step 7.
6. **`stubs.rs`**: the out-of-scope handlers (trivial). Verify each returns `isError`.
7. **Transport layer** (`transport.rs` + `mod.rs`): `interprocess` listener, acceptor + reader
   threads, central dispatch loop, `send_json`/`send_notification`, serial queue equivalence,
   per-client framing + 10 MB cap, disconnect purge, `mcp.reconnect` deferred close, `notify_*`.
   Run the §9.1 transport tests over a real loopback.
8. **Standalone bridge bin** (`src/bin/reclass-mcp-bridge.rs`): stdio↔socket relay (§5). Smoke-test:
   start a real bridge, spawn the bin, pipe an `initialize` line through it, assert a `result` line
   comes back on stdout (lines keep their `\n`).
9. **Wire `notify_*` + lifecycle** into the app shell (main.rs scope; out of this module): auto-start,
   toggle, document-changed → `notify_tree_changed`. Compile-check on Linux behind `--features mcp`.

Each step compiles and tests under `cargo test --no-default-features --features mcp` (no gpui), per
ARCHITECTURE.md §8. Keep all OS-specific socket bits behind `interprocess` (it handles the
Windows-pipe / Unix-socket split internally) so every step builds on Linux and stays correct on
Windows/macOS.
