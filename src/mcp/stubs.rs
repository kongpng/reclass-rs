//! Out-of-scope tool handlers (`mcp.md §5.12`).
//!
//! These tools are advertised verbatim in `tools/list` so MCP hosts present the
//! same toolset, but their handlers depend on out-of-scope subsystems (live
//! process plugins, PDB symbol store, scanner panel, type-inference UI, RTTI,
//! theme UI, bookmarks/refs). Each returns an `isError` "not available" result.

use serde_json::Value;

use super::wire::make_text_result;

/// `make_text_result("<tool> is not available in this build", true)`.
pub fn stub_not_available(tool: &str) -> Value {
    make_text_result(&format!("{tool} is not available in this build"), true)
}

/// The set of out-of-scope tool names that route to [`stub_not_available`]
/// (`PORTING_mcp.md §7.5`).
pub const STUB_TOOLS: &[&str] = &[
    "source.modules",
    "scanner.scan",
    "scanner.scan_pattern",
    "process.info",
    "symbols.load",
    "symbols.lookup",
    "symbols.importType",
    "node.read_value",
    "analysis.infer_types",
    "analysis.import_header",
    "analysis.pointer_chain",
    "analysis.find_overlaps",
    "analysis.field_path",
    "analysis.tree_summary",
    "ui.byte_selection",
    "ui.set_byte_selection",
    "ui.inspect",
    "theme.get",
    "theme.set",
    "theme.save",
    "theme.revert",
    "bookmarks.list",
    "bookmarks.add",
    "bookmarks.remove",
    "refs.find",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stub_is_error() {
        let r = stub_not_available("scanner.scan");
        assert_eq!(r["isError"], json!(true));
        assert_eq!(
            r["content"][0]["text"],
            "scanner.scan is not available in this build"
        );
    }
}
