//! MCP bridge — JSON-RPC 2.0 server over a local socket + the tool/notification
//! schemas.
//!
//! Faithful port of `src/mcp/mcp_bridge.{h,cpp}` (`mcp.md`, `PORTING_mcp.md`).
//! The C++ `QLocalServer` (named pipe on Windows, Unix socket elsewhere) maps to
//! the `interprocess` crate. The bridge runs on its own threads (acceptor +
//! per-client readers + a single serial dispatch thread that owns the model via
//! [`McpHost`]); see [`transport`]. Gated behind the `mcp` feature.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use interprocess::local_socket::traits::{Listener as _, Stream as _};
use interprocess::local_socket::{
    GenericNamespaced, ListenerNonblockingMode, ListenerOptions, ToNsName,
};

mod dispatch;
mod host;
mod schemas;
mod stubs;
mod tools;
mod transport;
mod wire;

pub use dispatch::{
    handle_initialize, handle_tools_call, process_request, Dispatched, INSTRUCTIONS,
};
pub use host::{McpHost, SavedSource, TabData, TabState, TestHost, UndoStack};
pub use schemas::{handle_tools_list, tool_descriptors, tool_names};
pub use transport::{Event, K_MAX_READ_BUFFER, URI_DATA, URI_TREE};
pub use wire::{
    err_reply, make_text_result, ok_reply, parse_integer, qt_number_double, qt_pretty,
    resolve_placeholder,
};

/// The local-socket name the bridge listens on (`QLocalServer("ReclassMcpBridge")`).
pub const K_SOCKET_NAME: &str = "ReclassMcpBridge";

/// Handle to the running threads (`m_server` + worker threads).
struct RunningBridge {
    event_tx: Sender<transport::Event>,
    shutdown: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    dispatch: Option<JoinHandle<()>>,
}

/// `class McpBridge` (`mcp_bridge.h:14`). Owns the listener + the serial
/// request pipeline. `inner == None` ⇔ stopped (`isRunning() == false`).
pub struct McpBridge {
    inner: Option<RunningBridge>,
    slow_mode: bool,
    socket_name: String,
}

impl Default for McpBridge {
    fn default() -> Self {
        McpBridge {
            inner: None,
            slow_mode: false,
            socket_name: K_SOCKET_NAME.to_string(),
        }
    }
}

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

impl McpBridge {
    pub fn new() -> Self {
        McpBridge::default()
    }

    /// Construct a bridge listening on a custom socket name (tests use a unique
    /// per-test name to avoid cross-test collisions, mirroring `test_mcp.cpp`'s
    /// `"ReclassMcpTest"`).
    pub fn with_socket_name(name: impl Into<String>) -> Self {
        McpBridge {
            inner: None,
            slow_mode: false,
            socket_name: name.into(),
        }
    }

    /// `McpBridge::start()` (`mcp_bridge.cpp:108-127`) — no-op if running; binds
    /// the listener (world-accessible, reclaiming a stale name); spawns the
    /// acceptor + dispatch threads. On bind failure: warn + silent no-op (the
    /// app keeps running). The `host` is the model boundary the dispatch thread
    /// owns.
    pub fn start(&mut self, host: Box<dyn McpHost>) {
        if self.inner.is_some() {
            return;
        }

        let name = match self.socket_name.as_str().to_ns_name::<GenericNamespaced>() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("[MCP] Invalid socket name: {e}");
                return;
            }
        };

        // WorldAccessOption equivalent + reclaim a stale name (Unix).
        let opts = ListenerOptions::new()
            .name(name)
            .reclaim_name(true)
            .try_overwrite(true);
        let listener = match opts.create_sync() {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("[MCP] Failed to start server: {e}");
                return;
            }
        };
        // Nonblocking accept so the acceptor loop can poll the shutdown flag and
        // exit promptly on stop().
        if listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .is_err()
        {
            tracing::warn!("[MCP] Failed to set nonblocking accept");
        }

        let (event_tx, event_rx) = mpsc::channel::<transport::Event>();
        let shutdown = Arc::new(AtomicBool::new(false));

        // Dispatch thread owns the host + all client write-halves.
        let dispatch = std::thread::Builder::new()
            .name("mcp-dispatch".into())
            .spawn(move || transport::dispatch_loop(event_rx, host))
            .expect("spawn mcp-dispatch");

        // Acceptor thread.
        let acc_tx = event_tx.clone();
        let acc_shutdown = shutdown.clone();
        let acceptor = std::thread::Builder::new()
            .name("mcp-acceptor".into())
            .spawn(move || acceptor_loop(listener, acc_tx, acc_shutdown))
            .expect("spawn mcp-acceptor");

        self.inner = Some(RunningBridge {
            event_tx,
            shutdown,
            acceptor: Some(acceptor),
            dispatch: Some(dispatch),
        });
        tracing::debug!("[MCP] Server listening on: {}", self.socket_name);
    }

    /// `McpBridge::stop()` (`mcp_bridge.cpp:129-144`).
    pub fn stop(&mut self) {
        if let Some(mut rb) = self.inner.take() {
            // Signal the acceptor (nonblocking poll) to stop, then tell the
            // dispatch loop to exit. Join both worker threads.
            rb.shutdown.store(true, Ordering::Relaxed);
            let _ = rb.event_tx.send(transport::Event::Shutdown);
            if let Some(j) = rb.acceptor.take() {
                let _ = j.join();
            }
            if let Some(j) = rb.dispatch.take() {
                let _ = j.join();
            }
        }
    }

    /// `McpBridge::isRunning()` (`mcp_bridge.h:22`).
    pub fn is_running(&self) -> bool {
        self.inner.is_some()
    }

    /// `slowMode()` / `setSlowMode()` (`mcp_bridge.h:24-25`).
    pub fn slow_mode(&self) -> bool {
        self.slow_mode
    }
    pub fn set_slow_mode(&mut self, v: bool) {
        self.slow_mode = v;
    }

    /// `notifyTreeChanged()` (`mcp_bridge.cpp:3499-3503`) — broadcast a
    /// `project://tree` update to initialized clients (no-op if not running).
    pub fn notify_tree_changed(&mut self) {
        if let Some(rb) = &self.inner {
            let _ = rb.event_tx.send(transport::Event::Notify { uri: URI_TREE });
        }
    }

    /// `notifyDataChanged()` (`mcp_bridge.cpp:3505-3509`).
    pub fn notify_data_changed(&mut self) {
        if let Some(rb) = &self.inner {
            let _ = rb.event_tx.send(transport::Event::Notify { uri: URI_DATA });
        }
    }
}

impl Drop for McpBridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The acceptor thread: nonblocking `accept()` poll; each accepted stream gets a
/// dense id + a reader thread, and a `NewClient` event. Exits when `shutdown`
/// is set (stop()) or the dispatch channel closes.
fn acceptor_loop(
    listener: interprocess::local_socket::Listener,
    tx: Sender<transport::Event>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok(stream) => {
                let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
                let arc = Arc::new(stream);
                if tx
                    .send(transport::Event::NewClient {
                        id,
                        stream: arc.clone(),
                    })
                    .is_err()
                {
                    return; // dispatch gone
                }
                let rtx = tx.clone();
                let rarc = arc.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("mcp-reader-{id}"))
                    .spawn(move || transport::reader_loop(id, rarc, rtx));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

// Re-export the stream connect helper used by the standalone bin.
pub use interprocess::local_socket::Stream as LocalStream;

/// Connect to the bridge socket by name (used by `reclass-mcp-bridge` + tests).
pub fn connect(name: &str) -> std::io::Result<interprocess::local_socket::Stream> {
    let ns = name
        .to_ns_name::<GenericNamespaced>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    interprocess::local_socket::Stream::connect(ns)
}
