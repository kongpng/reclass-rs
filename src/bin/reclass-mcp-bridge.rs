//! `reclass-mcp-bridge` — a stdio ↔ local-socket bridge.
//!
//! Port of `tools/rcx-mcp-stdio.cpp`. Relays newline-delimited JSON-RPC between
//! a parent process's stdio and the running app's local socket
//! ([`reclass::mcp::K_SOCKET_NAME`]) — a named pipe on Windows, a Unix domain
//! socket elsewhere (via `interprocess`). MCP hosts spawn this; it forwards
//! stdin→socket and socket→stdout, line by line, KEEPING the trailing `\n` in
//! both directions (the C++ uses `left(idx+1)`).
//!
//! Differences from the C++ that are behaviorally equivalent (`mcp.md §2.1`):
//! - No 10 ms poll timer — a dedicated stdin thread does blocking reads.
//! - No `_setmode(_O_BINARY)` — Rust stdio doesn't translate newlines.
//! - Connect timeout 5 s; on failure print to stderr + exit code 1; on
//!   disconnect/socket error print to stderr + exit (0).

use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reclass::mcp::K_SOCKET_NAME;

fn main() -> ExitCode {
    // Connect with a 5 s deadline (interprocess connect is blocking; retry on
    // transient "still starting" errors within the window).
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match reclass::mcp::connect(K_SOCKET_NAME) {
            Ok(s) => break s,
            Err(e) => {
                if Instant::now() >= deadline {
                    eprintln!("[ReclassMcpBridge] Failed to connect to ReclassMcpBridge pipe: {e}");
                    return ExitCode::from(1);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    eprintln!("[ReclassMcpBridge] Connected to ReclassMcpBridge");

    let stream = Arc::new(stream);

    // Thread A: socket → stdout. Emit complete lines (INCLUDING '\n'), flush each.
    let sock_rd = stream.clone();
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut out = std::io::stdout();
        loop {
            match (&*sock_rd).read(&mut chunk) {
                Ok(0) => {
                    eprintln!("[ReclassMcpBridge] Disconnected from server");
                    break;
                }
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some(idx) = buf.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = buf.drain(..=idx).collect(); // keep '\n'
                        let _ = out.write_all(&line);
                        let _ = out.flush();
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("[ReclassMcpBridge] Socket error: {e}");
                    break;
                }
            }
        }
        std::process::exit(0);
    });

    // Thread B (main): stdin → socket. Blocking reads; forward complete lines
    // (KEEP '\n'); write+flush to socket. On EOF/err: quit.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut stdin = std::io::stdin();
    loop {
        match stdin.read(&mut chunk) {
            Ok(0) => break, // stdin EOF
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(idx) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=idx).collect(); // keep '\n'
                    if (&*stream).write_all(&line).is_err() {
                        return ExitCode::SUCCESS;
                    }
                    let _ = (&*stream).flush();
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = reader; // either direction ending exits the process.
    ExitCode::SUCCESS
}
