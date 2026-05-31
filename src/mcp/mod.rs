//! MCP bridge — JSON-RPC 2.0 server over a local socket + tool/notification
//! schemas.
//!
//! Port of `src/mcp/mcp_bridge.{h,cpp}`. **SKELETON** — the JSON-RPC plumbing,
//! the serial request queue, and the tool handlers are filled in by the
//! dedicated `mcp` workflow (ARCHITECTURE.md §9). The C++ `QLocalServer`
//! (named pipe on Windows, Unix socket elsewhere) maps to the `interprocess`
//! crate. Gated behind the `mcp` feature.

/// The local-socket name the bridge listens on (`mcp_bridge.cpp`,
/// `QLocalServer("ReclassMcpBridge")`).
pub const K_SOCKET_NAME: &str = "ReclassMcpBridge";

/// `class McpBridge` (`mcp_bridge.h:14-...`). SKELETON: owns the listener and a
/// serial request queue; the real server is ported by the `mcp` workflow.
#[derive(Default)]
pub struct McpBridge {
    running: bool,
    slow_mode: bool,
}

impl McpBridge {
    pub fn new() -> Self {
        McpBridge::default()
    }

    /// `McpBridge::start()` (`mcp_bridge.h:20`). SKELETON.
    pub fn start(&mut self) {
        todo!("port mcp_bridge.cpp start (workflow: mcp)")
    }

    /// `McpBridge::stop()` (`mcp_bridge.h:21`).
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// `McpBridge::isRunning()` (`mcp_bridge.h:22`).
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// `McpBridge::slowMode()` / `setSlowMode()` (`mcp_bridge.h:24-25`).
    pub fn slow_mode(&self) -> bool {
        self.slow_mode
    }
    pub fn set_slow_mode(&mut self, v: bool) {
        self.slow_mode = v;
    }

    /// `McpBridge::notifyTreeChanged()` (`mcp_bridge.h:28`). SKELETON.
    pub fn notify_tree_changed(&mut self) {
        todo!("port mcp_bridge.cpp notifyTreeChanged (workflow: mcp)")
    }

    /// `McpBridge::notifyDataChanged()` (`mcp_bridge.h:29`). SKELETON.
    pub fn notify_data_changed(&mut self) {
        todo!("port mcp_bridge.cpp notifyDataChanged (workflow: mcp)")
    }
}
