//! `reclass-mcp-bridge` — a stdio ↔ local-socket bridge.
//!
//! Port of `tools/rcx-mcp-stdio.cpp`. Relays newline-delimited JSON-RPC between
//! a parent process's stdio and the running app's local socket
//! ([`reclass::mcp::K_SOCKET_NAME`]) — a named pipe on Windows, a Unix domain
//! socket elsewhere (via the `interprocess` crate). Standalone binary, requires
//! the `mcp` feature.
//!
//! **SKELETON** — the relay loop is filled in by the dedicated `mcp` workflow
//! (ARCHITECTURE.md §9).

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!(
        socket = reclass::mcp::K_SOCKET_NAME,
        "reclass-mcp-bridge stdio<->socket relay (stub)"
    );
    // TODO(workflow: mcp): connect to the local socket, then pump stdin↔socket
    // line-by-line until EOF (interprocess::local_socket).
}
