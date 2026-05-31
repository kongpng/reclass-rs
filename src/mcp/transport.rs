//! Transport layer — line framing, the central event channel, per-connection
//! write, and the serial dispatch loop.
//!
//! Maps `onReadyRead`/`drainPendingRequests`/`onDisconnected`/`sendJson`/
//! `sendNotification` (`mcp_bridge.cpp:181-282`). Because `interprocess`
//! blocking streams aren't `QObject`s with signals, the faithful model
//! (`mcp.md §8 option (a)`) is:
//!
//! - **One acceptor thread** runs `Listener::accept()` in a loop. Each accepted
//!   stream gets a dense [`ClientId`] and a **reader thread** doing blocking
//!   reads, splitting on `\n`, forwarding each complete trimmed non-empty line
//!   to a single central [`Event`] channel.
//! - **One dispatch thread** owns all client write-halves, the `m_processing`
//!   equivalent, the host, and is the ONLY thread that touches the model. It
//!   processes `Event`s serially — exactly the C++ "single GUI thread + serial
//!   queue" guarantee, made explicit. The channel + single consumer *is* the
//!   `m_pendingRequests` queue (one request in flight globally, FIFO, replies
//!   route to origin).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use interprocess::local_socket::Stream;
use serde_json::{json, Value};

use super::dispatch::process_request;
use super::host::McpHost;

/// 10 MB per-client read-buffer cap (`kMaxReadBuffer`, `mcp_bridge.cpp:23`).
pub const K_MAX_READ_BUFFER: usize = 10 * 1024 * 1024;

const NOTIF_METHOD: &str = "notifications/resources/updated";
pub const URI_TREE: &str = "project://tree";
pub const URI_DATA: &str = "project://data";

/// Dense client id handed out on accept.
pub type ClientId = u64;

/// Events flowing to the single dispatch thread.
pub enum Event {
    /// A new accepted client + its write handle (shared with the reader thread
    /// only for closing; writes go through this `Arc<Stream>`).
    NewClient { id: ClientId, stream: Arc<Stream> },
    /// A complete framed line from a client (newline stripped, trimmed).
    Line { id: ClientId, line: Vec<u8> },
    /// A client's reader saw EOF/error or the 10 MB cap was exceeded.
    Disconnected { id: ClientId },
    /// Broadcast a `notifications/resources/updated` for `uri`.
    Notify { uri: &'static str },
    /// Shut down the dispatch loop.
    Shutdown,
}

/// Per-connection state owned by the dispatch thread.
struct ClientState {
    stream: Arc<Stream>,
    initialized: bool,
}

/// The reader thread: blocking reads, line framing (`onReadyRead :197-215`),
/// 10 MB overflow guard. Forwards framed lines to the central channel.
pub fn reader_loop(id: ClientId, stream: Arc<Stream>, tx: Sender<Event>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // `Read` is implemented for `&Stream`.
        let n = match (&*stream).read(&mut chunk) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        buf.extend_from_slice(&chunk[..n]);

        if buf.len() > K_MAX_READ_BUFFER {
            tracing::warn!("[MCP] Read buffer exceeded 10MB, disconnecting client");
            let _ = tx.send(Event::Disconnected { id });
            return;
        }

        loop {
            let Some(idx) = buf.iter().position(|&b| b == b'\n') else {
                break;
            };
            // line = buf[..idx] trimmed (QByteArray::trimmed strips bytes <= 0x20).
            let raw = &buf[..idx];
            let line = trim_ascii(raw).to_vec();
            buf.drain(..=idx);
            if line.is_empty() {
                continue;
            }
            if tx.send(Event::Line { id, line }).is_err() {
                return; // dispatch gone
            }
        }
    }
    let _ = tx.send(Event::Disconnected { id });
}

/// `QByteArray::trimmed()` — strip bytes `<= 0x20` from both ends.
fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|&c| c > b' ').unwrap_or(b.len());
    let end = b.iter().rposition(|&c| c > b' ').map_or(start, |p| p + 1);
    &b[start..end]
}

/// The single dispatch thread (`onReadyRead` tail + `drainPendingRequests`).
/// Owns the host and all client write-halves; serial by construction.
pub fn dispatch_loop(rx: Receiver<Event>, mut host: Box<dyn McpHost>) {
    let mut clients: HashMap<ClientId, ClientState> = HashMap::new();

    while let Ok(ev) = rx.recv() {
        match ev {
            Event::NewClient { id, stream } => {
                clients.insert(
                    id,
                    ClientState {
                        stream,
                        initialized: false,
                    },
                );
            }
            Event::Disconnected { id } => {
                clients.remove(&id);
            }
            Event::Notify { uri } => {
                send_notification(&mut clients, uri);
            }
            Event::Shutdown => break,
            Event::Line { id, line } => {
                if !clients.contains_key(&id) {
                    continue; // disconnected meanwhile
                }
                // current_sender = id for the duration of this request.
                let d = process_request(&line, host.as_mut(), true);
                if d.mark_initialized {
                    if let Some(cs) = clients.get_mut(&id) {
                        cs.initialized = true;
                    }
                }
                if let Some(reply) = &d.reply {
                    send_json(&mut clients, id, reply);
                }
                if d.close_after {
                    // Reply already flushed; drop just this one client.
                    clients.remove(&id);
                }
            }
        }
    }
}

/// `sendJson(obj)` (`mcp_bridge.cpp:261-269`) — reply to the originating client
/// only; compact JSON + `\n`. Write/flush errors are swallowed.
fn send_json(clients: &mut HashMap<ClientId, ClientState>, id: ClientId, obj: &Value) {
    let Some(cs) = clients.get(&id) else {
        return;
    };
    let mut data = serde_json::to_vec(obj).unwrap_or_default();
    data.push(b'\n');
    let _ = (&*cs.stream).write_all(&data);
    let _ = (&*cs.stream).flush();
}

/// `sendNotification(method, params)` (`mcp_bridge.cpp:271-282`) — broadcast to
/// every `initialized` client. The `params` object is non-empty here so it is
/// always included.
fn send_notification(clients: &mut HashMap<ClientId, ClientState>, uri: &str) {
    let n = json!({"jsonrpc": "2.0", "method": NOTIF_METHOD, "params": {"uri": uri}});
    let mut data = serde_json::to_vec(&n).unwrap_or_default();
    data.push(b'\n');
    for cs in clients.values() {
        if cs.initialized {
            let _ = (&*cs.stream).write_all(&data);
            let _ = (&*cs.stream).flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_ascii_strips_whitespace() {
        assert_eq!(trim_ascii(b"  hi \t"), b"hi");
        assert_eq!(trim_ascii(b"\r\n"), b"");
        assert_eq!(trim_ascii(b"x"), b"x");
        assert_eq!(trim_ascii(b""), b"");
    }
}
