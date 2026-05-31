//! Transport/protocol integration tests over a real `interprocess` loopback,
//! mirroring `tests/test_mcp.cpp`. Each test spins a real `McpBridge` over a
//! `TestHost` on a UNIQUE per-test socket name (the C++ uses `"ReclassMcpTest"`;
//! we add pid+counter to avoid cross-test collisions), then connects raw
//! `interprocess` client(s) and exchanges newline-delimited JSON-RPC.
//!
//! Where the C++ MockMcpServer diverges from the real server (the mock's
//! `-32600`/`-32602` codes), these tests assert the REAL server behavior
//! (`-32601`) per `mcp.md §9`. The one upstream FAILURE
//! (`multiClient_bothInitialize`, a headless-Linux socket-timing artifact —
//! `_oracle/logs/test_mcp.txt`) is asserted to PASS here (2 clients), as our
//! threaded transport makes multi-client reliable.

#![cfg(feature = "mcp")]

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::Stream;
use reclass::mcp::{McpBridge, TestHost};
use serde_json::{json, Value};

static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_socket() -> String {
    let n = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("ReclassMcpTest-{}-{}", std::process::id(), n)
}

/// Start a bridge over a fresh TestHost on a unique socket; returns (bridge, name).
fn start_bridge() -> (McpBridge, String) {
    let name = unique_socket();
    let mut bridge = McpBridge::with_socket_name(name.clone());
    bridge.start(Box::new(TestHost::new()));
    // Give the acceptor a moment to bind.
    std::thread::sleep(Duration::from_millis(50));
    (bridge, name)
}

/// `makeClient` — connect with retries.
fn make_client(name: &str) -> Stream {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match reclass::mcp::connect(name) {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("connect failed: {e}"),
        }
    }
}

/// `rpc(s, req)` — write one line, read one response line.
fn rpc(s: &Stream, req: Value) -> Value {
    let mut data = serde_json::to_vec(&req).unwrap();
    data.push(b'\n');
    (&*s).write_all(&data).unwrap();
    (&*s).flush().unwrap();
    read_line(s, 3000)
}

/// Read up to a `\n`, return the parsed object (or Null on timeout).
fn read_line(s: &Stream, ms: u64) -> Value {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut buf: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        let mut chunk = [0u8; 1024];
        match (&*s).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
                    return serde_json::from_slice(&buf[..idx]).unwrap_or(Value::Null);
                }
            }
            Err(_) => break,
        }
    }
    Value::Null
}

/// `initRpc(s)` — send an initialize request, return the response.
fn init_rpc(s: &Stream) -> Value {
    rpc(
        s,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test"}}}),
    )
}

/// `drain(s, ms)` — collect all complete lines arriving within `ms`.
fn drain(s: &Stream, ms: u64) -> Vec<Value> {
    s.set_nonblocking(true).ok();
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut buf: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        let mut chunk = [0u8; 1024];
        match (&*s).read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    let mut out = Vec::new();
    while let Some(idx) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=idx).collect();
        let trimmed = &line[..line.len() - 1];
        if !trimmed.is_empty() {
            if let Ok(v) = serde_json::from_slice::<Value>(trimmed) {
                out.push(v);
            }
        }
    }
    out
}

// ── Tests ──

#[test]
fn single_client_initialize() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    let r = init_rpc(&c);
    assert_eq!(r["id"], json!(1));
    assert!(r.get("result").is_some());
    // REAL server: serverInfo.name == "reclass-mcp" (not the mock's "mock-mcp").
    assert_eq!(r["result"]["serverInfo"]["name"], "reclass-mcp");
    bridge.stop();
}

#[test]
fn single_client_tools_list() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    init_rpc(&c);
    let r = rpc(&c, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(r["id"], json!(2));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    // Stronger parity: first tool is project.state.
    assert_eq!(tools[0]["name"], "project.state");
    bridge.stop();
}

#[test]
fn single_client_unknown_method() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    let r = rpc(&c, json!({"jsonrpc":"2.0","id":1,"method":"bogus"}));
    assert_eq!(r["error"]["code"], json!(-32601));
    bridge.stop();
}

#[test]
fn multi_client_both_initialize() {
    // Upstream FAILS here on headless Linux (clientCount==1, expected 2) — a
    // recorded golden flake (_oracle/logs/test_mcp.txt). Our threaded transport
    // makes this reliable, so we assert the *intended* behavior: both clients
    // connect and initialize.
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    let c2 = make_client(&name);
    let r1 = init_rpc(&c1);
    let r2 = init_rpc(&c2);
    assert!(r1.get("result").is_some());
    assert!(r2.get("result").is_some());
    bridge.stop();
}

#[test]
fn multi_client_disconnect_one() {
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    let c2 = make_client(&name);
    init_rpc(&c1);
    init_rpc(&c2);
    drop(c1); // disconnect client 1
    std::thread::sleep(Duration::from_millis(200));
    // client 2 still works
    let r = rpc(&c2, json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}));
    assert_eq!(r["id"], json!(5));
    assert!(!r["result"]["tools"].as_array().unwrap().is_empty());
    bridge.stop();
}

#[test]
fn multi_client_notification_broadcast() {
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    let c2 = make_client(&name);
    let c3 = make_client(&name); // not initialized
    init_rpc(&c1);
    init_rpc(&c2);
    // Let the server register all clients (incl. the uninitialized c3).
    std::thread::sleep(Duration::from_millis(100));

    bridge.notify_tree_changed();

    let l1 = drain(&c1, 300);
    let l2 = drain(&c2, 300);
    let l3 = drain(&c3, 300);
    assert!(!l1.is_empty());
    assert_eq!(
        l1.last().unwrap()["method"],
        "notifications/resources/updated"
    );
    assert!(!l2.is_empty());
    assert_eq!(
        l2.last().unwrap()["method"],
        "notifications/resources/updated"
    );
    // uninitialized client gets ZERO lines.
    assert_eq!(l3.len(), 0);
    bridge.stop();
}

#[test]
fn multi_client_serial_requests() {
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    let c2 = make_client(&name);
    init_rpc(&c1);
    init_rpc(&c2);
    let r1 = rpc(&c1, json!({"jsonrpc":"2.0","id":10,"method":"tools/list"}));
    let r2 = rpc(&c2, json!({"jsonrpc":"2.0","id":20,"method":"tools/list"}));
    assert_eq!(r1["id"], json!(10));
    assert_eq!(r2["id"], json!(20));
    bridge.stop();
}

#[test]
fn all_disconnect_server_survives() {
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    init_rpc(&c1);
    drop(c1);
    std::thread::sleep(Duration::from_millis(200));
    // fresh client connects + initializes
    let c2 = make_client(&name);
    let r = init_rpc(&c2);
    assert!(r.get("result").is_some());
    bridge.stop();
}

#[test]
fn protocol_invalid_json() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    (&c).write_all(b"this is not json\n").unwrap();
    (&c).flush().unwrap();
    let r = read_line(&c, 2000);
    assert_eq!(r["error"]["code"], json!(-32700));
    bridge.stop();
}

#[test]
fn protocol_missing_method() {
    // REAL server: -32601 "Method not found: " (mock used -32600).
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    let r = rpc(&c, json!({"jsonrpc":"2.0","id":1})); // no "method"
    assert_eq!(r["error"]["code"], json!(-32601));
    bridge.stop();
}

#[test]
fn protocol_notifications_ignored() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    init_rpc(&c);
    let n1 = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    let n2 = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}});
    let mut d1 = serde_json::to_vec(&n1).unwrap();
    d1.push(b'\n');
    let mut d2 = serde_json::to_vec(&n2).unwrap();
    d2.push(b'\n');
    (&c).write_all(&d1).unwrap();
    (&c).write_all(&d2).unwrap();
    (&c).flush().unwrap();
    let lines = drain(&c, 500);
    assert_eq!(lines.len(), 0); // no response for notifications
    bridge.stop();
}

#[test]
fn tools_call_unknown_tool() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    init_rpc(&c);
    let r = rpc(
        &c,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"nonexistent.tool","arguments":{}}}),
    );
    assert_eq!(r["error"]["code"], json!(-32601));
    bridge.stop();
}

#[test]
fn tools_call_missing_tool_name() {
    // REAL server: -32601 "Unknown tool: " (mock used -32602).
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    init_rpc(&c);
    let r = rpc(
        &c,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"arguments":{}}}),
    );
    assert_eq!(r["error"]["code"], json!(-32601));
    bridge.stop();
}

#[test]
fn tools_call_reconnect() {
    let (mut bridge, name) = start_bridge();
    let c = make_client(&name);
    init_rpc(&c);
    let r = rpc(
        &c,
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":"mcp.reconnect","arguments":{}}}),
    );
    assert_eq!(r["id"], json!(7));
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Disconnected"));

    // server-side disconnect happens after the reply flushes.
    std::thread::sleep(Duration::from_millis(300));

    // a reconnect works
    let c2 = make_client(&name);
    let r2 = init_rpc(&c2);
    assert!(r2.get("result").is_some());
    let r3 = rpc(&c2, json!({"jsonrpc":"2.0","id":8,"method":"tools/list"}));
    assert_eq!(r3["id"], json!(8));
    assert!(!r3["result"]["tools"].as_array().unwrap().is_empty());
    bridge.stop();
}

#[test]
fn tools_call_reconnect_other_client_unaffected() {
    let (mut bridge, name) = start_bridge();
    let c1 = make_client(&name);
    let c2 = make_client(&name);
    init_rpc(&c1);
    init_rpc(&c2);
    // c1 calls reconnect — only c1 should disconnect.
    rpc(
        &c1,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"mcp.reconnect","arguments":{}}}),
    );
    std::thread::sleep(Duration::from_millis(300));
    // c2 still works
    let r = rpc(&c2, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(r["id"], json!(2));
    assert!(!r["result"]["tools"].as_array().unwrap().is_empty());
    bridge.stop();
}

#[test]
fn stdio_bridge_relays_initialize() {
    // Smoke-test the standalone bin: start a real bridge, spawn the bin, pipe an
    // initialize line through its stdin, assert a result line on stdout.
    use std::process::{Command, Stdio};

    let (bridge, name) = start_bridge();
    // The bin connects to the fixed K_SOCKET_NAME, so bind on that for this test.
    drop(bridge);
    let mut bridge2 = McpBridge::with_socket_name(reclass::mcp::K_SOCKET_NAME);
    bridge2.start(Box::new(TestHost::new()));
    std::thread::sleep(Duration::from_millis(100));
    let _ = name;

    let bin = env!("CARGO_BIN_EXE_reclass-mcp-bridge");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bridge bin");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
    let mut data = serde_json::to_vec(&req).unwrap();
    data.push(b'\n');
    stdin.write_all(&data).unwrap();
    stdin.flush().unwrap();

    // read a line back from the bin's stdout (lines keep their '\n').
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut chunk = [0u8; 512];
    let resp = loop {
        if Instant::now() >= deadline {
            break Value::Null;
        }
        match stdout.read(&mut chunk) {
            Ok(0) => break Value::Null,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
                    break serde_json::from_slice(&buf[..idx]).unwrap_or(Value::Null);
                }
            }
            Err(_) => break Value::Null,
        }
    };
    let _ = child.kill();
    bridge2.stop();

    assert_eq!(resp["result"]["serverInfo"]["name"], "reclass-mcp");
}
