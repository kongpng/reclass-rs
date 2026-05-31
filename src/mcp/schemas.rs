//! Verbatim `tools/list` descriptors — maps `handleToolsList`
//! (`mcp_bridge.cpp:388-1074`).
//!
//! One `fn tool_<name>()` per tool returning its `{name, description,
//! inputSchema}` descriptor, pushed into a `Vec<Value>` in the exact C++
//! registration order. Pure data: no `Provider`/UI dependency. The `tools`
//! array order is preserved by serde; within each descriptor the keys are
//! sorted by serde's default `Value` map (matches Qt's sorted-key output).

use serde_json::{json, Value};

use super::wire::ok_reply;

/// `handleToolsList(id)` — returns `okReply(id, {"tools": [...]})` with the
/// descriptors in registration order (`mcp.md §3.2`).
pub fn handle_tools_list(id: &Value) -> Value {
    let tools = Value::Array(tool_descriptors());
    ok_reply(id, json!({ "tools": tools }))
}

/// The descriptors, in the exact C++ append order.
pub fn tool_descriptors() -> Vec<Value> {
    vec![
        tool_project_state(),
        tool_tree_apply(),
        tool_source_switch(),
        tool_source_modules(),
        tool_hex_read(),
        tool_hex_write(),
        tool_bookmarks_list(),
        tool_bookmarks_add(),
        tool_bookmarks_remove(),
        tool_refs_find(),
        tool_status_set(),
        tool_ui_action(),
        tool_tree_search(),
        tool_node_history(),
        tool_scanner_scan(),
        tool_scanner_scan_pattern(),
        tool_mcp_reconnect(),
        tool_process_info(),
        tool_symbols_load(),
        tool_symbols_lookup(),
        tool_symbols_import_type(),
        tool_node_read_value(),
        tool_analysis_infer_types(),
        tool_analysis_import_header(),
        tool_analysis_pointer_chain(),
        tool_analysis_find_overlaps(),
        tool_analysis_tree_summary(),
        tool_analysis_field_path(),
        tool_ui_byte_selection(),
        tool_ui_set_byte_selection(),
        tool_ui_inspect(),
        tool_theme_get(),
        tool_theme_set(),
        tool_theme_save(),
        tool_theme_revert(),
    ]
}

/// The names in registration order (for tests).
pub fn tool_names() -> Vec<String> {
    tool_descriptors()
        .iter()
        .map(|d| d["name"].as_str().unwrap_or("").to_string())
        .collect()
}

// ── per-tool descriptors (verbatim from mcp_bridge.cpp) ──

fn tool_project_state() -> Value {
    json!({
        "name": "project.state",
        "description": "Returns project state with paginated node tree. NOTE: This returns structure metadata only (kinds, names, offsets), NOT live memory values. Use hex.read to read actual values and node.history to track value changes over time. Responses return max 'limit' nodes (default 50). Use depth:1 first, then parentId to drill into a struct. Enum/bitfield member arrays are omitted by default (counts shown instead); pass includeMembers:true to get full arrays. Response includes returned/total/nextOffset for paging.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "depth": {"type": "integer", "description": "Max tree depth to return (default 1)."},
                "parentId": {"type": "string", "description": "Only return children of this node."},
                "includeTree": {"type": "boolean", "description": "If false, return only provider/source info, no tree. Default true."},
                "includeMembers": {"type": "boolean", "description": "If true, include full enumMembers/bitfieldMembers arrays. Default false (shows counts only)."},
                "limit": {"type": "integer", "description": "Max nodes to return (default 50, max 500)."},
                "offset": {"type": "integer", "description": "Skip this many nodes (for pagination). Use nextOffset from previous response."}
            }
        }
    })
}

fn tool_tree_apply() -> Value {
    json!({
        "name": "tree.apply",
        "description": "Apply batch of tree operations atomically (undo macro). IMPORTANT: When identifying/labeling a field, you MUST use BOTH rename AND change_kind in the same batch. A renamed node still has its original kind (e.g. Hex64) unless you explicitly change it. Example: [{op:'rename',nodeId:'ID',name:'health'},{op:'change_kind',nodeId:'ID',kind:'Int32'}]. Each op is a JSON object with an 'op' field for the operation type and 'nodeId' (string) for the target node. Operations: remove: {op:'remove', nodeId:'ID'}. rename: {op:'rename', nodeId:'ID', name:'newName'}. insert: {op:'insert', kind:'Hex64', name:'field', parentId:'ID', offset:0} — optional fields: structTypeName, classKeyword, strLen, elementKind, arrayLen, refId, ptrDepth (0=struct ptr, 1=prim*, 2=prim**), isStatic (bool), offsetExpr (string), isRelative (bool, RVA pointer), enumMembers ([{name:'X',value:0},...]), bitfieldMembers ([{name:'X',bitOffset:0,bitWidth:1},...]). change_kind: {op:'change_kind', nodeId:'ID', kind:'UInt32'}. change_offset: {op:'change_offset', nodeId:'ID', offset:16}. change_base: {op:'change_base', baseAddress:'0x400000', formula:'[0x233CA80]'} — formula is optional, enables auto-resolve on provider attach. change_struct_type: {op:'change_struct_type', nodeId:'ID', structTypeName:'Name'}. change_class_keyword: {op:'change_class_keyword', nodeId:'ID', classKeyword:'class'}. change_pointer_ref: {op:'change_pointer_ref', nodeId:'ID', refId:'targetID'}. change_array_meta: {op:'change_array_meta', nodeId:'ID', elementKind:'UInt32', arrayLen:10}. collapse: {op:'collapse', nodeId:'ID', collapsed:true}. change_enum_members: {op:'change_enum_members', nodeId:'ID', members:[{name:'X',value:0},...]}. change_offset_expr: {op:'change_offset_expr', nodeId:'ID', offsetExpr:'base + 0x10'}. toggle_static: {op:'toggle_static', nodeId:'ID', isStatic:true}. toggle_relative: {op:'toggle_relative', nodeId:'ID', isRelative:true}. group_into_union: {op:'group_into_union', nodeIds:['ID1','ID2',...]} — groups siblings into a union. dissolve_union: {op:'dissolve_union', nodeId:'ID'} — flattens a union back to parent scope. Insert ops get auto-assigned IDs; use $0, $1 etc. to reference them in later ops. Kinds: Hex8 Hex16 Hex32 Hex64 Int8 Int16 Int32 Int64 UInt8 UInt16 UInt32 UInt64 Float Double Bool Pointer32 Pointer64 FuncPtr32 FuncPtr64 Vec2 Vec3 Vec4 Mat4x4 UTF8 UTF16 Struct Array",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "operations": {"type": "array", "items": {"type": "object"}},
                "macroName": {"type": "string"}
            },
            "required": ["operations"]
        }
    })
}

fn tool_source_switch() -> Value {
    json!({
        "name": "source.switch",
        "description": "Switch active data source (provider). Use sourceIndex for saved sources, filePath to load a binary file, or pid to attach to a live process.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "sourceIndex": {"type": "integer"},
                "filePath": {"type": "string"},
                "pid": {"type": "integer", "description": "Process ID to attach to for live memory reading."},
                "processName": {"type": "string", "description": "Display name for the process (optional with pid)."},
                "allViews": {"type": "boolean"}
            }
        }
    })
}

fn tool_source_modules() -> Value {
    json!({
        "name": "source.modules",
        "description": "List modules for the current data source. Returns name, base (hex), and size for each module. Only available when the provider reports module info (e.g. after attaching to a process). Use these names in baseAddressFormula for tree base, e.g. '<Module.exe> + 0x1000'.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_hex_read() -> Value {
    json!({
        "name": "hex.read",
        "description": "Read raw bytes from provider (live process memory). Returns hex dump, ASCII, and multi-type interpretations (u8/u16/u32/u64/i32/f32/f64/ptr/string heuristics). Use this to see what actual values are in memory at any offset. By default offset is an absolute virtual address in the target process. Set baseRelative=true to make offset relative to the struct base address (e.g. offset=0 reads at baseAddress, offset=0x10 reads at baseAddress+0x10).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "offset": {"type": "integer", "description": "Address to read from. Absolute VA by default, or relative to struct base if baseRelative=true."},
                "length": {"type": "integer", "description": "Number of bytes to read (1-4096, default 64)."},
                "baseRelative": {"type": "boolean", "description": "If true, offset is relative to the tree's base address (added automatically). Default false (offset is absolute VA)."},
                "interpret": {"type": "boolean", "description": "If true, append per-field type inference results (8-byte aligned chunks analyzed by the inference engine). Returns scored suggestions like [float] score=80, [ptr64] score=75 for each chunk."}
            },
            "required": ["offset", "length"]
        }
    })
}

fn tool_hex_write() -> Value {
    json!({
        "name": "hex.write",
        "description": "Write raw bytes to provider (through undo stack). Hex string format: '4D5A9000'. By default offset is an absolute virtual address. Set baseRelative=true to make offset relative to the struct base address.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "offset": {"type": "integer", "description": "Address to write to. Absolute VA by default, or relative to struct base if baseRelative=true."},
                "hexBytes": {"type": "string", "description": "Hex byte string to write, e.g. '4D5A9000'. Spaces allowed."},
                "baseRelative": {"type": "boolean", "description": "If true, offset is relative to the tree's base address. Default false (absolute VA)."}
            },
            "required": ["offset", "hexBytes"]
        }
    })
}

fn tool_bookmarks_list() -> Value {
    json!({
        "name": "bookmarks.list",
        "description": "List bookmarks for the active project. Returns array of {name, addressFormula, address}.",
        "inputSchema": {
            "type": "object",
            "properties": {"tabIndex": {"type": "integer"}}
        }
    })
}

fn tool_bookmarks_add() -> Value {
    json!({
        "name": "bookmarks.add",
        "description": "Add a bookmark to the active project. addressFormula is an AddressParser expression such as '<game.exe>+0x12340' or '0x7ff7e0001234'. Survives base-address rebases.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer"},
                "name": {"type": "string"},
                "addressFormula": {"type": "string"}
            },
            "required": ["name", "addressFormula"]
        }
    })
}

fn tool_bookmarks_remove() -> Value {
    json!({
        "name": "bookmarks.remove",
        "description": "Remove a bookmark by name (first match) or by 0-based index.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer"},
                "name": {"type": "string"},
                "index": {"type": "integer"}
            }
        }
    })
}

fn tool_refs_find() -> Value {
    json!({
        "name": "refs.find",
        "description": "Find every field across all open documents that references a given struct type. Matches by nodeId (precise) and/or structTypeName (cross-doc, for imported types not linked by id). Returns array of {tabIndex, nodeId, owner, field, offset} entries.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodeId": {"type": "string", "description": "Target struct's node id. Optional if typeName is given."},
                "typeName": {"type": "string", "description": "Target struct's structTypeName. Optional if nodeId is given."}
            }
        }
    })
}

fn tool_status_set() -> Value {
    json!({
        "name": "status.set",
        "description": "Show status text to user. Updates command row (editor line 0) and/or the window status bar.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "text": {"type": "string"},
                "target": {"type": "string", "enum": ["commandRow", "statusBar", "both"]}
            },
            "required": ["text"]
        }
    })
}

fn tool_ui_action() -> Value {
    json!({
        "name": "ui.action",
        "description": "Trigger a UI action. Fallback for operations without dedicated tools. Actions: undo, redo, new_file, open_file, save_file, save_file_as, export_cpp, set_view_root, scroll_to_node, collapse_node, expand_node, select_node, refresh, reset_tracking. export_cpp accepts optional nodeId to export a single struct (recommended for large projects). reset_tracking clears all value change histories — use before an in-game event, then check node.history afterward to see what changed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "action": {"type": "string"},
                "nodeId": {"type": "string"},
                "filePath": {"type": "string"}
            },
            "required": ["action"]
        }
    })
}

fn tool_tree_search() -> Value {
    json!({
        "name": "tree.search",
        "description": "Search for nodes by name (substring, case-insensitive). Returns compact results: id, name, kind, parentId, offset, childCount. Use kindFilter to narrow (e.g. 'Struct'). Max 100 results. Much faster than paging through project.state to find a specific type.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "query": {"type": "string", "description": "Name substring to search for (case-insensitive)."},
                "kindFilter": {"type": "string", "description": "Filter by node kind (e.g. 'Struct', 'Hex64', 'Array')."},
                "limit": {"type": "integer", "description": "Max results to return (default 20, max 100)."}
            }
        }
    })
}

fn tool_node_history() -> Value {
    json!({
        "name": "node.history",
        "description": "Returns timestamped value change history (up to 10 entries) for specified nodes. Use this to detect what changed after an in-game event — no need to manually snapshot memory. Each node returns: entries[] with {value, timestamp}, heatLevel (0=static to 3=hot), and uniqueCount. Heat level 3 means the field is actively changing. Requires live provider with value tracking enabled.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodeIds": {"type": "array", "items": {"type": "string"}, "description": "Array of node IDs to get history for."},
                "tabIndex": {"type": "integer", "description": "MDI tab index. Omit for active tab."}
            },
            "required": ["nodeIds"]
        }
    })
}

fn tool_scanner_scan() -> Value {
    json!({
        "name": "scanner.scan",
        "description": "Run a value scan on the active tab's provider and wait for completion. Use after source.switch (e.g. attach to process). Value type: int8, int16, int32, int64, uint8, uint16, uint32, uint64, float, double. Results appear in the Scanner panel. For value scans (e.g. float 120) prefer scanning readable/writable (data) regions, not executable: set filterWritable: true and filterExecutable: false. Use 'regions' to restrict scan to specific address ranges (intersected with provider regions).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "valueType": {"type": "string", "description": "Value type: float, double, int32, uint32, int64, uint64, int16, uint16, int8, uint8."},
                "value": {"type": "string", "description": "Value to search for (e.g. \"120\" for float 120)."},
                "filterExecutable": {"type": "boolean", "description": "Only scan executable regions (default false). For value scans use false; use writable instead."},
                "filterWritable": {"type": "boolean", "description": "Only scan writable regions (default false). Recommended true for value scans to hit data/heap, not code."},
                "regions": {"type": "array", "description": "Restrict scan to these address ranges. Each element is [startHex, endHex], e.g. [[\"0x10000\",\"0x20000\"],[\"0x50000\",\"0x60000\"]]. Ranges are intersected with the provider's real memory regions.", "items": {"type": "array", "items": {"type": "string"}}}
            },
            "required": ["valueType", "value"]
        }
    })
}

fn tool_scanner_scan_pattern() -> Value {
    json!({
        "name": "scanner.scan_pattern",
        "description": "Run a pattern/signature scan on the active tab's provider and wait for completion. Pattern is space-separated hex bytes, e.g. '00 00 20 42 00 00 20 42'. Use ?? for wildcards. Results appear in the Scanner panel. Uses the same region list as value scans. Use 'regions' to restrict scan to specific address ranges (intersected with provider regions).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."},
                "pattern": {"type": "string", "description": "Hex pattern, e.g. '00 00 20 42 00 00 20 42 00 00 00 00 00 00 00 00'. Use ?? for wildcard bytes."},
                "filterExecutable": {"type": "boolean", "description": "Only scan executable regions (default false)."},
                "filterWritable": {"type": "boolean", "description": "Only scan writable regions (default false)."},
                "regions": {"type": "array", "description": "Restrict scan to these address ranges. Each element is [startHex, endHex], e.g. [[\"0x10000\",\"0x20000\"],[\"0x50000\",\"0x60000\"]]. Ranges are intersected with the provider's real memory regions.", "items": {"type": "array", "items": {"type": "string"}}}
            },
            "required": ["pattern"]
        }
    })
}

fn tool_mcp_reconnect() -> Value {
    json!({
        "name": "mcp.reconnect",
        "description": "Disconnect the current MCP client so it can reconnect to Reclass (e.g. after Reclass was restarted or to reset connection state). The client process will exit; your IDE may restart it automatically, reconnecting to Reclass like at startup.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

fn tool_process_info() -> Value {
    json!({
        "name": "process.info",
        "description": "Returns PEB address and enumerates all Thread Environment Blocks (TEBs) for the attached process. TEBs are discovered via NtQuerySystemInformation and NtQueryInformationThread. Each TEB entry includes: address, threadId. Requires a live process provider with PEB support.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_symbols_load() -> Value {
    json!({
        "name": "symbols.load",
        "description": "Load PDB symbols from a file path into the global symbol store. Symbols are used for address annotations (e.g. 'ntdll!RtlInitUnicodeString') and can be resolved via symbols.lookup. Returns the number of symbols loaded and the module name.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "pdbPath": {"type": "string", "description": "Absolute path to a .pdb file."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab. Used to refresh annotations after loading."}
            },
            "required": ["pdbPath"]
        }
    })
}

fn tool_symbols_lookup() -> Value {
    json!({
        "name": "symbols.lookup",
        "description": "Resolve a symbol name to an absolute virtual address in the attached process. Supports qualified names like 'ntdll!RtlInitUnicodeString' and bare names. Requires symbols to be loaded (via symbols.load or the UI) and a live provider.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "Symbol to resolve. Use 'module!name' for qualified lookup, or bare 'name' for unqualified."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["symbol"]
        }
    })
}

fn tool_symbols_import_type() -> Value {
    json!({
        "name": "symbols.importType",
        "description": "Import the type definition for a global symbol from its PDB into the active project. Given a qualified symbol like 'ntdll!g_pShimEngineModule', resolves its typeIndex from the PDB, follows pointer/modifier chains to find the underlying struct/class/union/enum, and imports it with full recursive child types. Requires symbols to be loaded first (via symbols.load). Returns the imported type name and node count, or an error if the symbol has no type info.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "symbol": {"type": "string", "description": "Qualified symbol name (e.g. 'ntdll!g_pShimEngineModule'). Must include 'module!' prefix."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["symbol"]
        }
    })
}

fn tool_node_read_value() -> Value {
    json!({
        "name": "node.read_value",
        "description": "Read the formatted typed value for one or more nodes. Unlike hex.read (which returns raw bytes), this returns the value as the user sees it in the editor: e.g. '120.0f' for Float, '0x7FF61234' for Pointer64, 'true' for Bool, '1.0, 2.0, 3.0' for Vec3. For Hex nodes returns the hex byte preview. For Struct/Array returns the computed size. Requires a live provider for meaningful values.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodeIds": {"type": "array", "items": {"type": "string"}, "description": "Array of node IDs to read values for."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["nodeIds"]
        }
    })
}

fn tool_analysis_infer_types() -> Value {
    json!({
        "name": "analysis.infer_types",
        "description": "Run the type inference engine on hex nodes to get scored type suggestions. Returns top candidates (e.g. float, ptr64, int32_t×2) with confidence scores (0-100) and strength levels (1=weak, 2=moderate, 3=strong). Uses value change history for better accuracy when available. Much more accurate than manually interpreting hex bytes.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodeIds": {"type": "array", "items": {"type": "string"}, "description": "Array of node IDs to analyze (should be Hex8/16/32/64 nodes)."},
                "useHistory": {"type": "boolean", "description": "Feed value change history into inference for better accuracy (default true)."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["nodeIds"]
        }
    })
}

fn tool_analysis_import_header() -> Value {
    json!({
        "name": "analysis.import_header",
        "description": "Import C/C++ struct definitions from source code into the active project. Accepts standard C/C++ struct/class/union/enum syntax with optional offset comments (// 0xNN). This is far more efficient than building structs field-by-field via tree.apply. Supports Windows types (DWORD, HANDLE, etc.), stdint types, pointers, arrays, bitfields, and nested structs.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "sourceCode": {"type": "string", "description": "C/C++ source code containing struct/class/union/enum definitions."},
                "pointerSize": {"type": "integer", "description": "Pointer size: 4 for 32-bit, 8 for 64-bit (default 8)."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["sourceCode"]
        }
    })
}

fn tool_analysis_pointer_chain() -> Value {
    json!({
        "name": "analysis.pointer_chain",
        "description": "Follow a chain of pointers from a starting address, returning hex dump + type inference + vtable detection + symbol annotations at each level. Stops on null, unreadable, or maxDepth. Use this to explore what a pointer points to without multiple sequential hex.read calls.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "address": {"type": "string", "description": "Starting address (hex string, e.g. '0x7FF618570000'). Or use baseRelative:true with an offset from struct base."},
                "baseRelative": {"type": "boolean", "description": "If true, address is relative to struct base address."},
                "maxDepth": {"type": "integer", "description": "Maximum pointer levels to follow (default 3, max 8)."},
                "readLength": {"type": "integer", "description": "Bytes to read and analyze at each level (default 64, max 512)."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            },
            "required": ["address"]
        }
    })
}

fn tool_analysis_find_overlaps() -> Value {
    json!({
        "name": "analysis.find_overlaps",
        "description": "Detect sibling-overlap bugs in the active tree: pairs of non-static, non-union sibling fields whose [offset, offset+size) byte ranges intersect. This is the most common bug introduced by manual offset edits. Returns pairs as {parentId, parentName, aId, aName, aOffset, aSize, bId, bName, bOffset, bSize}.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_analysis_tree_summary() -> Value {
    json!({
        "name": "analysis.tree_summary",
        "description": "One-shot health snapshot of the active tree: total nodes, top-level classes (parentId=0 Struct/Class), maximum depth, total bytes (sum of top-level class spans), and overlap count from findOverlaps(). Faster than enumerating every node via separate tools.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_analysis_field_path() -> Value {
    json!({
        "name": "analysis.field_path",
        "description": "Bidirectional path/id resolver. Pass nodeId to get the dot-separated path from the root class (e.g. \"Player.Stats.Health\"); pass path to get back the node id. Exactly one of nodeId / path is required.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "nodeId": {"type": "string", "description": "Node id as a decimal or hex string. Returns {path}."},
                "path": {"type": "string", "description": "Dot-path like \"Player.Stats.Health\". Returns {nodeId}. Matching is exact, case-sensitive."},
                "separator": {"type": "string", "description": "Path separator character. Default: \".\""},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_ui_byte_selection() -> Value {
    json!({
        "name": "ui.byte_selection",
        "description": "Return the active byte selection on the active editor pane: {lo, hi, size} as hex address strings, or {active:false} if no selection is active. Useful for AI agents that want to know which byte range the user is currently inspecting (e.g. before suggesting an interpretation, copying bytes, or referencing the range in chat).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_ui_set_byte_selection() -> Value {
    json!({
        "name": "ui.set_byte_selection",
        "description": "Set the active editor's byte selection to [lo, hi) so the user sees the same range you do. Pass lo and hi as hex address strings (0x prefix accepted) or decimals. Omit both lo and hi to clear the selection. Half-open: hi is exclusive, must be > lo.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "lo": {"type": "string", "description": "Inclusive start address (decimal or 0x-hex)."},
                "hi": {"type": "string", "description": "Exclusive end address (decimal or 0x-hex)."},
                "tabIndex": {"type": "integer", "description": "MDI tab index (0-based). Omit for active tab."}
            }
        }
    })
}

fn tool_ui_inspect() -> Value {
    json!({
        "name": "ui.inspect",
        "description": "Query the UI region the user selected via Ctrl+Shift+Click. Returns widget type, region name, theme colors that control it, and generic properties (fontSize, width, height, etc.). The user Ctrl+Shift+Clicks on any part of the window to select it, then you call this to see what they selected.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "region": {"type": "string", "description": "Query a named region directly (e.g. 'editor.typeColumn') without needing Ctrl+Click."},
                "clear": {"type": "boolean", "description": "Clear the current selection and hide overlay."}
            }
        }
    })
}

fn tool_theme_get() -> Value {
    json!({
        "name": "theme.get",
        "description": "Return all theme colors (30 fields). Each has key, current hex value, label, and group.",
        "inputSchema": {"type": "object", "properties": {}}
    })
}

fn tool_theme_set() -> Value {
    json!({
        "name": "theme.set",
        "description": "Change one or more theme colors with instant live preview. Non-destructive — use theme.save to persist or theme.revert to undo. Returns old values for each changed key.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "colors": {"type": "object", "description": "Map of theme field key → new hex color value. E.g. {\"textDim\": \"#999999\", \"hover\": \"#2A2A2A\"}. Use theme.get or ui.inspect to discover valid keys."}
            },
            "required": ["colors"]
        }
    })
}

fn tool_theme_save() -> Value {
    json!({
        "name": "theme.save",
        "description": "Persist the current previewed theme changes (from theme.set). Saves to disk.",
        "inputSchema": {"type": "object", "properties": {}}
    })
}

fn tool_theme_revert() -> Value {
    json!({
        "name": "theme.revert",
        "description": "Revert theme to state before any theme.set calls. Undoes all unsaved preview changes.",
        "inputSchema": {"type": "object", "properties": {}}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_registration_order() {
        let names = tool_names();
        assert_eq!(names.len(), 35);
        assert_eq!(names[0], "project.state");
        assert_eq!(names[1], "tree.apply");
        assert_eq!(names[2], "source.switch");
        assert_eq!(names[3], "source.modules");
        assert_eq!(names[4], "hex.read");
        assert_eq!(names[5], "hex.write");
        assert_eq!(
            &names[6..10],
            &[
                "bookmarks.list",
                "bookmarks.add",
                "bookmarks.remove",
                "refs.find"
            ]
        );
        assert_eq!(names[10], "status.set");
        assert_eq!(names[11], "ui.action");
        assert_eq!(names[12], "tree.search");
        assert_eq!(names[13], "node.history");
        assert_eq!(names.last().unwrap(), "theme.revert");
        assert_eq!(names[16], "mcp.reconnect");
    }

    #[test]
    fn each_descriptor_has_required_keys() {
        for d in tool_descriptors() {
            assert!(d.get("name").and_then(|v| v.as_str()).is_some());
            assert!(d.get("description").and_then(|v| v.as_str()).is_some());
            assert_eq!(d["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn tools_list_envelope() {
        let r = handle_tools_list(&json!(2));
        assert_eq!(r["id"], json!(2));
        let arr = r["result"]["tools"].as_array().unwrap();
        assert_eq!(arr.len(), 35);
        assert_eq!(arr[0]["name"], "project.state");
    }
}
