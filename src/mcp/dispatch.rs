//! Top-level method routing + the `tools/call` name switch.
//!
//! Maps `processLine`/`handleInitialize`/`handleToolsCall`
//! (`mcp_bridge.cpp:300-340`, `:346-382`, `:1080-1151`). Pure of socket I/O:
//! [`process_request`] takes a parsed line and returns either a reply `Value`
//! (to be sent to the originating client) or `None` (notifications produce no
//! reply). It also reports whether the current client should be closed after
//! the reply is flushed (for `mcp.reconnect`).

use serde_json::{Map, Value};

use super::host::McpHost;
use super::schemas::handle_tools_list;
use super::stubs::{stub_not_available, STUB_TOOLS};
use super::tools;
use super::wire::{err_reply, make_text_result, ok_reply};

/// `instructions` string (`mcp_bridge.cpp:359-379`) — the LLM-facing contract,
/// copied byte-for-byte (the C++ string-literal fragments concatenate into one
/// string with embedded `\n`).
pub const INSTRUCTIONS: &str = "You are connected to ReClass, a live memory structure editor for reverse engineering. You have two types of data available:\n1. STRUCTURE: The node tree defines typed fields (project.state, tree.search, tree.apply). Each node has a kind (the data type: UInt32, Float, Hex64, etc.) and a name.\n2. LIVE DATA: The provider reads real memory from an attached process (hex.read, hex.write). node.history returns timestamped value changes with heat levels (0=static, 1=cold, 2=warm, 3=hot).\n\nCRITICAL RULES:\n- When labeling/identifying a field, ALWAYS change BOTH name AND kind in one tree.apply call. Example: [{op:'rename',nodeId:'X',name:'health'},{op:'change_kind',nodeId:'X',kind:'Int32'}]. A node named 'health' with kind Hex64 is WRONG — the kind must match the actual data type.\n- To detect what changed after an in-game event: call ui.action with action:'reset_tracking', then have the user perform the action, then call node.history on the relevant nodes to see which ones have new timestamped entries.\n- hex.read offset is an absolute virtual address by default. Use baseRelative=true to make it relative to the struct base address (0 = start of struct).\n- tree.apply operations are atomic (undo macro). Batch related changes into one call.\n- Use tree.search to quickly find nodes by name instead of paging through project.state.\n- project.state returns structure metadata only (kinds, names, offsets), NOT live values. Use hex.read for actual memory values and node.history for tracking changes over time.\n- Use evidence.record for structured observations from IDA, runtime breakpoints, scanners, or user experiment markers. Use evidence.focus_packet as the preferred LLM input: it combines node metadata, value history, relevant evidence, hypotheses, and pending proposals without dumping the whole project.";

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "reclass-mcp";
const SERVER_VERSION: &str = "1.0.0";

/// Outcome of dispatching one request line.
pub struct Dispatched {
    /// The reply to send to the originating client (None = no response).
    pub reply: Option<Value>,
    /// True if the current client should mark itself initialized.
    pub mark_initialized: bool,
    /// True if the current client should be closed *after* the reply flushes
    /// (`mcp.reconnect`).
    pub close_after: bool,
}

impl Dispatched {
    fn reply(v: Value) -> Self {
        Dispatched {
            reply: Some(v),
            mark_initialized: false,
            close_after: false,
        }
    }
    fn none() -> Self {
        Dispatched {
            reply: None,
            mark_initialized: false,
            close_after: false,
        }
    }
}

/// `processLine` (`mcp_bridge.cpp:300-340`). Parses the line, routes by method,
/// and returns the [`Dispatched`] outcome. `has_current_sender` mirrors
/// `m_currentSender != nullptr` (always true on the dispatch thread, but
/// `mcp.reconnect` checks it).
pub fn process_request(
    line: &[u8],
    host: &mut dyn McpHost,
    has_current_sender: bool,
) -> Dispatched {
    let parsed: Result<Value, _> = serde_json::from_slice(line);
    let req = match parsed {
        Ok(Value::Object(o)) => o,
        // Not an object OR invalid JSON → parse error with null id.
        _ => {
            return Dispatched::reply(err_reply(&Value::Null, -32700, "Parse error"));
        }
    };

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");

    // Client notifications (no response).
    if method == "notifications/initialized" || method == "notifications/cancelled" {
        return Dispatched::none();
    }

    let params: Map<String, Value> = req
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    match method {
        "initialize" => {
            let mut d = Dispatched::reply(handle_initialize(&id));
            d.mark_initialized = true;
            d
        }
        "tools/list" => Dispatched::reply(handle_tools_list(&id)),
        "tools/call" => {
            let (reply, close_after) = handle_tools_call(&id, &params, host, has_current_sender);
            Dispatched {
                reply: Some(reply),
                mark_initialized: false,
                close_after,
            }
        }
        _ => Dispatched::reply(err_reply(
            &id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

/// `handleInitialize` (`mcp_bridge.cpp:346-382`). `params` ignored.
pub fn handle_initialize(id: &Value) -> Value {
    let result = serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        "instructions": INSTRUCTIONS,
    });
    ok_reply(id, result)
}

/// `handleToolsCall` (`mcp_bridge.cpp:1080-1151`). Returns `(reply, close_after)`.
/// Tool-level failures are signaled by `result.isError`; only an unknown tool
/// name returns a JSON-RPC `error` (`-32601`).
pub fn handle_tools_call(
    id: &Value,
    params: &Map<String, Value>,
    host: &mut dyn McpHost,
    has_current_sender: bool,
) -> (Value, bool) {
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args: Map<String, Value> = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    host.set_app_status(&format!("MCP: {tool}"));

    let mut close_after = false;
    let result: Value = match tool {
        "project.state" => tools::tool_project_state(&args, host),
        "tree.apply" => tools::tool_tree_apply(&args, host),
        "source.switch" => tools::tool_source_switch(&args, host),
        "source.modules" => tools::tool_source_modules(&args, host),
        "hex.read" => tools::tool_hex_read(&args, host),
        "hex.write" => tools::tool_hex_write(&args, host),
        "status.set" => tools::tool_status_set(&args, host),
        "ui.action" => tools::tool_ui_action(&args, host),
        "tree.search" => tools::tool_tree_search(&args, host),
        "node.history" => tools::tool_node_history(&args, host),
        // evidence.* + tree.export_header (`mcp_bridge.cpp:1312-1325`).
        "evidence.record" => tools::tool_evidence_record(&args, host),
        "evidence.timeline" => tools::tool_evidence_timeline(&args, host),
        "evidence.capture_changes" => tools::tool_evidence_capture_changes(&args, host),
        "evidence.hypothesis" => tools::tool_evidence_hypothesis(&args, host),
        "evidence.proposal" => tools::tool_evidence_proposal(&args, host),
        "evidence.focus_packet" => tools::tool_evidence_focus_packet(&args, host),
        "tree.export_header" => tools::tool_tree_export_header(&args, host),
        "mcp.reconnect" => {
            // `toolReconnect` (`mcp_bridge.cpp:2432-2442`).
            if !has_current_sender {
                make_text_result("No client connected.", true)
            } else {
                close_after = true;
                make_text_result(
                    "Disconnected. The MCP client will exit; your IDE may restart it and reconnect to Reclass.",
                    false,
                )
            }
        }
        // Out-of-scope stubs (advertised in tools/list verbatim).
        t if STUB_TOOLS.contains(&t) => stub_not_available(t),
        // Unknown tool → JSON-RPC error (NOT okReply).
        _ => {
            return (
                err_reply(id, -32601, &format!("Unknown tool: {tool}")),
                false,
            )
        }
    };

    (ok_reply(id, result), close_after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::host::TestHost;
    use serde_json::json;

    fn line(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    #[test]
    fn initialize_exact() {
        let r = handle_initialize(&json!(1));
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "reclass-mcp");
        assert_eq!(r["result"]["serverInfo"]["version"], "1.0.0");
        assert_eq!(
            r["result"]["capabilities"]["tools"]["listChanged"],
            json!(false)
        );
        assert_eq!(r["result"]["instructions"], INSTRUCTIONS);
        assert!(INSTRUCTIONS.contains("STRUCTURE"));
        assert!(INSTRUCTIONS.contains("reset_tracking"));
        assert!(INSTRUCTIONS.contains("evidence.record"));
        assert!(INSTRUCTIONS.contains("focus_packet"));
    }

    #[test]
    fn unknown_method_is_32601() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","id":1,"method":"bogus"})),
            &mut h,
            true,
        );
        let r = d.reply.unwrap();
        assert_eq!(r["error"]["code"], json!(-32601));
        assert_eq!(r["error"]["message"], "Method not found: bogus");
    }

    #[test]
    fn missing_method_is_32601_empty() {
        let mut h = TestHost::new();
        let d = process_request(&line(json!({"jsonrpc":"2.0","id":1})), &mut h, true);
        let r = d.reply.unwrap();
        assert_eq!(r["error"]["code"], json!(-32601));
        assert_eq!(r["error"]["message"], "Method not found: ");
    }

    #[test]
    fn parse_error_is_32700() {
        let mut h = TestHost::new();
        let d = process_request(b"this is not json", &mut h, true);
        let r = d.reply.unwrap();
        assert_eq!(r["error"]["code"], json!(-32700));
        assert_eq!(r["id"], Value::Null);
        // Non-object JSON also → parse error.
        let d2 = process_request(b"[1,2,3]", &mut h, true);
        assert_eq!(d2.reply.unwrap()["error"]["code"], json!(-32700));
    }

    #[test]
    fn notifications_no_reply() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            &mut h,
            true,
        );
        assert!(d.reply.is_none());
        let d2 = process_request(
            &line(
                json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
            ),
            &mut h,
            true,
        );
        assert!(d2.reply.is_none());
    }

    #[test]
    fn unknown_tool_is_32601() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"nonexistent.tool","arguments":{}}})),
            &mut h,
            true,
        );
        let r = d.reply.unwrap();
        assert_eq!(r["error"]["code"], json!(-32601));
        assert_eq!(r["error"]["message"], "Unknown tool: nonexistent.tool");
    }

    #[test]
    fn missing_tool_name_is_32601() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"arguments":{}}})),
            &mut h,
            true,
        );
        let r = d.reply.unwrap();
        // REAL server: -32601 "Unknown tool: " (mock used -32602).
        assert_eq!(r["error"]["code"], json!(-32601));
        assert_eq!(r["error"]["message"], "Unknown tool: ");
    }

    #[test]
    fn reconnect_returns_result_and_closes() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                "params":{"name":"mcp.reconnect","arguments":{}}})),
            &mut h,
            true,
        );
        let r = d.reply.unwrap();
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Disconnected"));
        assert!(d.close_after);
    }

    #[test]
    fn stub_tool_is_error() {
        let mut h = TestHost::new();
        let d = process_request(
            &line(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"scanner.scan","arguments":{}}})),
            &mut h,
            true,
        );
        let r = d.reply.unwrap();
        assert_eq!(r["result"]["isError"], json!(true));
    }
}
