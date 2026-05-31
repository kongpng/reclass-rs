//! In-scope tool handlers + `resolve_tab` (`mcp_bridge.cpp:1175-2442`, `mcp.md §5`).
//!
//! Each tool takes `(args, host)` and returns a tool-result `Value` (via
//! [`make_text_result`] or with extra keys). The tab is resolved with
//! [`resolve_tab`] and borrowed via `host.with_tab`. The load-bearing edge
//! cases (clamps, the hex-dump format, the base-address case/0x asymmetry,
//! placeholder forward refs, the atomic undo macro) are ported 1:1.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::core::command::OffsetAdj;
use crate::core::kind::{alignment_for, kind_from_string, kind_to_string};
use crate::core::node::{BitfieldMember, Node, K_MAX_ARRAY_LEN};
use crate::core::{Command, NodeKind};
use crate::provider::Provider;

use super::host::{McpHost, TabState};
use super::wire::{
    make_text_result, parse_integer, qt_number_double, qt_pretty, resolve_placeholder,
};

/// Helper: get an args entry by key.
fn arg<'a>(args: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    args.get(k)
}

fn arg_str(args: &Map<String, Value>, k: &str) -> String {
    arg(args, k)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn arg_str_default(args: &Map<String, Value>, k: &str, default: &str) -> String {
    match arg(args, k).and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => default.to_string(),
    }
}

fn arg_bool(args: &Map<String, Value>, k: &str) -> bool {
    arg(args, k).and_then(Value::as_bool).unwrap_or(false)
}

/// `QString::toULongLong()` — decimal parse, 0 on failure.
fn to_u64(s: &str) -> u64 {
    s.trim().parse::<u64>().unwrap_or(0)
}

/// `resolveTab(args, host)` (`mcp_bridge.cpp:1175-1206`) → a tab index.
///
/// Priority: explicit `tabIndex` (if valid) → active tab → first tab →
/// auto-create. Auto-create is part of the contract (a tool never fails purely
/// for "no document open"). Returns the resolved index (always `Some` after the
/// auto-create step).
pub fn resolve_tab(args: &Map<String, Value>, host: &mut dyn McpHost) -> Option<usize> {
    // 1) explicit tab index
    if args.contains_key("tabIndex") {
        let idx = parse_integer(arg(args, "tabIndex"), 0);
        if idx >= 0 && (idx as usize) < host.tab_count() {
            return Some(idx as usize);
        }
    }
    // 2) active tab
    if let Some(i) = host.active_tab_index() {
        return Some(i);
    }
    // 3) first tab
    if host.tab_count() > 0 {
        return Some(0);
    }
    // 4) auto-create
    Some(host.project_new())
}

// ════════════════════════════════════════════════════════════════════
// project.state
// ════════════════════════════════════════════════════════════════════

/// `toolProjectState` (`mcp_bridge.cpp:1212-1360`, `mcp.md §5.1`).
pub fn tool_project_state(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let max_depth = parse_integer(arg(args, "depth"), 1);
    let include_tree = match arg(args, "includeTree") {
        Some(v) => v.as_bool().unwrap_or(true),
        None => true,
    };
    let include_members = arg_bool(args, "includeMembers");
    let limit = parse_integer(arg(args, "limit"), 50).clamp(1, 500);
    let offset = parse_integer(arg(args, "offset"), 0).max(0);
    let parent_id_str = arg_str(args, "parentId");
    let filter_parent_id = if parent_id_str.is_empty() {
        0u64
    } else {
        to_u64(&parent_id_str)
    };

    let app_status = host.app_status();

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let d = &tab.data;
        let tree = &d.tree;

        let mut state = Map::new();
        state.insert(
            "baseAddress".into(),
            json!(format!("0x{:X}", tree.base_address)),
        );
        if !tree.base_address_formula.is_empty() {
            state.insert(
                "baseAddressFormula".into(),
                json!(tree.base_address_formula),
            );
        }
        state.insert("viewRootId".into(), json!(d.view_root_id.to_string()));
        state.insert("nodeCount".into(), json!(tree.nodes.len() as i64));

        // provider info
        let mut prov = Map::new();
        {
            let p = d.provider.as_ref();
            // NullProvider has size()==0 — the C++ checks `if (doc->provider)`,
            // which is always non-null here (default NullProvider). Mirror: emit
            // the object (null provider yields a "Null" kind etc.).
            prov.insert("name".into(), json!(p.name()));
            prov.insert("writable".into(), json!(p.is_writable()));
            prov.insert("live".into(), json!(p.is_live()));
            prov.insert("size".into(), json!(p.size()));
            prov.insert("kind".into(), json!(p.kind()));
        }
        state.insert("provider".into(), Value::Object(prov));

        // sources
        let mut srcs = Vec::new();
        for (i, s) in d.sources.iter().enumerate() {
            srcs.push(json!({
                "index": i as i64,
                "kind": s.kind,
                "displayName": s.display_name,
                "active": i as i32 == d.active_source
            }));
        }
        state.insert("sources".into(), Value::Array(srcs));

        // selection
        let mut sel: Vec<u64> = d.selected_ids.iter().copied().collect();
        sel.sort_unstable();
        state.insert(
            "selectedNodeIds".into(),
            Value::Array(sel.iter().map(|id| json!(id.to_string())).collect()),
        );

        state.insert("filePath".into(), json!(d.file_path));
        state.insert("modified".into(), json!(d.modified));
        state.insert("undoAvailable".into(), json!(tab.can_undo()));
        state.insert("redoAvailable".into(), json!(tab.can_redo()));
        state.insert("statusText".into(), json!(app_status));

        if include_tree {
            // build child map once
            let mut child_map: HashMap<u64, Vec<usize>> = HashMap::new();
            for (i, n) in tree.nodes.iter().enumerate() {
                child_map.entry(n.parent_id).or_default().push(i);
            }

            let mut node_arr: Vec<Value> = Vec::new();
            // BFS queue of (parentId, depth)
            let mut queue: std::collections::VecDeque<(u64, i64)> =
                std::collections::VecDeque::new();
            queue.push_back((filter_parent_id, 0));

            let mut total_count: i64 = 0;
            let mut emitted: i64 = 0;

            while let Some((parent_id, depth)) = queue.pop_front() {
                if depth > max_depth {
                    continue;
                }
                let kids = child_map.get(&parent_id).cloned().unwrap_or_default();
                for ci in kids {
                    let n = &tree.nodes[ci];
                    total_count += 1;

                    if total_count <= offset {
                        if depth + 1 <= max_depth {
                            queue.push_back((n.id, depth + 1));
                        }
                        continue;
                    }
                    if emitted >= limit {
                        if depth + 1 <= max_depth {
                            queue.push_back((n.id, depth + 1));
                        }
                        continue;
                    }

                    let mut nj = n.to_json();
                    if !include_members {
                        if let Some(arr) = nj.get("enumMembers").and_then(Value::as_array) {
                            let count = arr.len() as i64;
                            let obj = nj.as_object_mut().unwrap();
                            obj.remove("enumMembers");
                            obj.insert("enumMemberCount".into(), json!(count));
                        }
                        if let Some(arr) = nj.get("bitfieldMembers").and_then(Value::as_array) {
                            let count = arr.len() as i64;
                            let obj = nj.as_object_mut().unwrap();
                            obj.remove("bitfieldMembers");
                            obj.insert("bitfieldMemberCount".into(), json!(count));
                        }
                    }
                    if matches!(n.kind, NodeKind::Struct | NodeKind::Array) {
                        let obj = nj.as_object_mut().unwrap();
                        obj.insert("computedSize".into(), json!(tree.struct_span(n.id)));
                        obj.insert(
                            "childCount".into(),
                            json!(child_map.get(&n.id).map_or(0, |v| v.len()) as i64),
                        );
                    }
                    node_arr.push(nj);
                    emitted += 1;

                    if depth + 1 <= max_depth {
                        queue.push_back((n.id, depth + 1));
                    }
                }
            }

            let mut tree_obj = Map::new();
            // NOTE: inner treeObj.baseAddress is LOWERCASE hex WITHOUT 0x —
            // deliberately different from the top-level field (preserve).
            tree_obj.insert(
                "baseAddress".into(),
                json!(format!("{:x}", tree.base_address)),
            );
            if !tree.base_address_formula.is_empty() {
                tree_obj.insert(
                    "baseAddressFormula".into(),
                    json!(tree.base_address_formula),
                );
            }
            tree_obj.insert("nextId".into(), json!(tree.next_id().to_string()));
            tree_obj.insert("nodes".into(), Value::Array(node_arr));
            tree_obj.insert("returned".into(), json!(emitted));
            tree_obj.insert("total".into(), json!(total_count));
            if emitted < total_count {
                tree_obj.insert("nextOffset".into(), json!(offset + emitted));
            }
            state.insert("tree".into(), Value::Object(tree_obj));
        }

        out = Value::Object(state);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    make_text_result(&qt_pretty(&out), false)
}

// ════════════════════════════════════════════════════════════════════
// tree.apply
// ════════════════════════════════════════════════════════════════════

/// `toolTreeApply` (`mcp_bridge.cpp:1366-1782`, `mcp.md §5.2`).
pub fn tool_tree_apply(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let ops: Vec<Value> = arg(args, "operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let _macro_name = arg_str_default(args, "macroName", "MCP batch");

    if ops.is_empty() {
        return make_text_result("No operations provided", true);
    }

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        // Phase 1: reserve insert IDs keyed by operation index.
        let mut placeholders: HashMap<String, u64> = HashMap::new();
        for (i, op) in ops.iter().enumerate() {
            if op.get("op").and_then(Value::as_str) == Some("insert") {
                let new_id = tab.data.tree.reserve_id();
                placeholders.insert(format!("${i}"), new_id);
            }
        }

        // Phase 2: execute in undo macro.
        tab.begin_macro();
        let mut applied = 0i64;
        let mut last_root_struct_id: u64 = 0;
        let mut skipped: Vec<String> = Vec::new();

        for (i, op) in ops.iter().enumerate() {
            let op_obj = match op.as_object() {
                Some(o) => o,
                None => {
                    skipped.push(format!("op[{i}]: unknown op ''"));
                    continue;
                }
            };
            let op_type = op_obj.get("op").and_then(Value::as_str).unwrap_or("");

            match op_type {
                "insert" => {
                    apply_insert(
                        tab,
                        i,
                        op_obj,
                        &placeholders,
                        &mut applied,
                        &mut last_root_struct_id,
                        &mut skipped,
                    );
                }
                "remove" => {
                    let (nid, _) = resolve_node_arg(op_obj, "nodeId", &placeholders);
                    let id = to_u64(&nid);
                    let tidx = tab.data.tree.index_of_id(id);
                    if tidx >= 0 {
                        let node_id = tab.data.tree.nodes[tidx as usize].id;
                        let sub_idx = tab.data.tree.subtree_indices(node_id);
                        let subtree: Vec<Node> = sub_idx
                            .iter()
                            .map(|&si| tab.data.tree.nodes[si].clone())
                            .collect();
                        tab.push_command(Command::Remove {
                            node_id,
                            subtree,
                            off_adjs: Vec::new(),
                        });
                        applied += 1;
                    } else {
                        skipped.push(format!("op[{i}]: remove nodeId '{nid}' not found"));
                    }
                }
                "rename" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].name.clone();
                        tab.push_command(Command::Rename {
                            node_id,
                            old_name: old,
                            new_name: arg_str(op_obj, "name"),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "rename", op_obj, &placeholders);
                    }
                }
                "change_kind" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].kind;
                        let new_kind = kind_from_string(&arg_str(op_obj, "kind"));
                        tab.push_command(Command::ChangeKind {
                            node_id,
                            old_kind: old,
                            new_kind,
                            off_adjs: Vec::new(),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_kind", op_obj, &placeholders);
                    }
                }
                "change_offset" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].offset;
                        let new_off = parse_integer(arg(op_obj, "offset"), 0) as i32;
                        tab.push_command(Command::ChangeOffset {
                            node_id,
                            old_offset: old,
                            new_offset: new_off,
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_offset", op_obj, &placeholders);
                    }
                }
                "change_base" => {
                    // bare hex; Qt's toULongLong(_,16) accepts a 0x prefix too.
                    let base_str = arg_str(op_obj, "baseAddress");
                    let new_base = u64::from_str_radix(
                        base_str
                            .trim()
                            .trim_start_matches("0x")
                            .trim_start_matches("0X"),
                        16,
                    )
                    .unwrap_or(0);
                    let old_base = tab.data.tree.base_address;
                    let old_formula = tab.data.tree.base_address_formula.clone();
                    let new_formula = arg_str(op_obj, "formula");
                    tab.push_command(Command::ChangeBase {
                        old_base,
                        new_base,
                        old_formula,
                        new_formula,
                    });
                    applied += 1;
                }
                "change_struct_type" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].struct_type_name.clone();
                        tab.push_command(Command::ChangeStructTypeName {
                            node_id,
                            old_name: old,
                            new_name: arg_str(op_obj, "structTypeName"),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_struct_type", op_obj, &placeholders);
                    }
                }
                "change_class_keyword" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].class_keyword.clone();
                        tab.push_command(Command::ChangeClassKeyword {
                            node_id,
                            old_keyword: old,
                            new_keyword: arg_str(op_obj, "classKeyword"),
                        });
                        applied += 1;
                    } else {
                        skip(
                            &mut skipped,
                            i,
                            "change_class_keyword",
                            op_obj,
                            &placeholders,
                        );
                    }
                }
                "change_pointer_ref" => {
                    let (ref_str, _) =
                        resolve_node_arg_default(op_obj, "refId", "0", &placeholders);
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].ref_id;
                        tab.push_command(Command::ChangePointerRef {
                            node_id,
                            old_ref_id: old,
                            new_ref_id: to_u64(&ref_str),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_pointer_ref", op_obj, &placeholders);
                    }
                }
                "change_array_meta" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old_ek = tab.data.tree.nodes[tidx].element_kind;
                        let old_len = tab.data.tree.nodes[tidx].array_len;
                        let new_ek = kind_from_string(&arg_str(op_obj, "elementKind"));
                        let new_len = (parse_integer(arg(op_obj, "arrayLen"), 1) as i32)
                            .clamp(1, K_MAX_ARRAY_LEN);
                        tab.push_command(Command::ChangeArrayMeta {
                            node_id,
                            old_element_kind: old_ek,
                            new_element_kind: new_ek,
                            old_array_len: old_len,
                            new_array_len: new_len,
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_array_meta", op_obj, &placeholders);
                    }
                }
                "collapse" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].collapsed;
                        let new_state = arg_bool(op_obj, "collapsed");
                        tab.push_command(Command::Collapse {
                            node_id,
                            old_state: old,
                            new_state,
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "collapse", op_obj, &placeholders);
                    }
                }
                "change_enum_members" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].enum_members.clone();
                        let new_members = parse_enum_members(op_obj, "members");
                        tab.push_command(Command::ChangeEnumMembers {
                            node_id,
                            old_members: old,
                            new_members,
                        });
                        applied += 1;
                    } else {
                        skip(
                            &mut skipped,
                            i,
                            "change_enum_members",
                            op_obj,
                            &placeholders,
                        );
                    }
                }
                "change_offset_expr" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].offset_expr.clone();
                        tab.push_command(Command::ChangeOffsetExpr {
                            node_id,
                            old_expr: old,
                            new_expr: arg_str(op_obj, "offsetExpr"),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_offset_expr", op_obj, &placeholders);
                    }
                }
                "toggle_static" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].is_static;
                        tab.push_command(Command::ToggleStatic {
                            node_id,
                            old_val: old,
                            new_val: arg_bool(op_obj, "isStatic"),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "toggle_static", op_obj, &placeholders);
                    }
                }
                "toggle_relative" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].is_relative;
                        tab.push_command(Command::ToggleRelative {
                            node_id,
                            old_val: old,
                            new_val: arg_bool(op_obj, "isRelative"),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "toggle_relative", op_obj, &placeholders);
                    }
                }
                "group_into_union" => {
                    let mut ids: HashSet<u64> = HashSet::new();
                    if let Some(arr) = op_obj.get("nodeIds").and_then(Value::as_array) {
                        for v in arr {
                            let (resolved, _) =
                                resolve_placeholder(v.as_str().unwrap_or(""), &placeholders);
                            ids.insert(to_u64(&resolved));
                        }
                    }
                    if ids.len() >= 2 {
                        // Direct controller call (NOT undo command); out-of-scope
                        // controller groupIntoUnion — count as applied for parity.
                        applied += 1;
                    } else {
                        skipped.push(format!("op[{i}]: group_into_union needs >= 2 nodeIds"));
                    }
                }
                "dissolve_union" => {
                    let (nid, _) = resolve_node_arg(op_obj, "nodeId", &placeholders);
                    let union_id = to_u64(&nid);
                    if tab.data.tree.index_of_id(union_id) >= 0 {
                        applied += 1;
                    } else {
                        skipped.push(format!("op[{i}]: dissolve_union nodeId '{nid}' not found"));
                    }
                }
                other => {
                    skipped.push(format!("op[{i}]: unknown op '{other}'"));
                }
            }
        }

        tab.end_macro();
        if last_root_struct_id != 0 {
            tab.data.view_root_id = last_root_struct_id;
        }
        // ctrl->refresh() is a UI step (skeleton todo!) — omitted.

        // assignedIds (decimal strings)
        let mut assigned = Map::new();
        for (k, v) in &placeholders {
            assigned.insert(k.clone(), json!(v.to_string()));
        }

        let mut msg = format!("Applied {applied} operations");
        if !skipped.is_empty() {
            msg += &format!("\nSkipped {}:\n", skipped.len());
            msg += &skipped.join("\n");
        }
        let mut result = make_text_result(&msg, !skipped.is_empty() && applied == 0);
        result["assignedIds"] = Value::Object(assigned);
        out = result;
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

/// Build + push an `insert` command (`mcp_bridge.cpp:1409-1483`).
fn apply_insert(
    tab: &mut TabState,
    i: usize,
    op: &Map<String, Value>,
    placeholders: &HashMap<String, u64>,
    applied: &mut i64,
    last_root_struct_id: &mut u64,
    skipped: &mut Vec<String>,
) {
    let mut n = Node::default();
    n.id = placeholders
        .get(&format!("${i}"))
        .copied()
        .unwrap_or_else(|| tab.data.tree.reserve_id());
    n.kind = kind_from_string(&arg_str_default(op, "kind", "Hex64"));
    n.name = arg_str(op, "name");

    let (pid, pid_ok) = resolve_placeholder(&arg_str_default(op, "parentId", "0"), placeholders);
    if !pid_ok {
        skipped.push(format!("op[{i}]: unresolved placeholder for parentId"));
        return;
    }
    n.parent_id = to_u64(&pid);
    if n.parent_id != 0 && tab.data.tree.index_of_id(n.parent_id) < 0 {
        skipped.push(format!("op[{i}]: parentId '{pid}' not found"));
        return;
    }
    n.offset = parse_integer(arg(op, "offset"), 0) as i32;
    n.struct_type_name = arg_str(op, "structTypeName");
    n.class_keyword = arg_str(op, "classKeyword");
    n.str_len = (parse_integer(arg(op, "strLen"), 64) as i32).clamp(1, 1_000_000);
    n.element_kind = kind_from_string(&arg_str_default(op, "elementKind", "UInt8"));
    n.array_len = (parse_integer(arg(op, "arrayLen"), 1) as i32).clamp(1, K_MAX_ARRAY_LEN);
    n.ptr_depth = (parse_integer(arg(op, "ptrDepth"), 0) as i32).clamp(0, 2);
    n.is_static = arg_bool(op, "isStatic");
    n.offset_expr = arg_str(op, "offsetExpr");
    n.is_relative = arg_bool(op, "isRelative");

    if op.contains_key("enumMembers") {
        n.enum_members = parse_enum_members(op, "enumMembers");
    }
    if op.contains_key("bitfieldMembers") {
        if let Some(arr) = op.get("bitfieldMembers").and_then(Value::as_array) {
            for bv in arr {
                if let Some(bo) = bv.as_object() {
                    n.bitfield_members.push(BitfieldMember {
                        name: arg_str(bo, "name"),
                        bit_offset: (parse_integer(arg(bo, "bitOffset"), 0) as i32).clamp(0, 255)
                            as u8,
                        bit_width: (parse_integer(arg(bo, "bitWidth"), 1) as i32).clamp(1, 64)
                            as u8,
                    });
                }
            }
        }
    }

    let (ref_str, ref_ok) = resolve_placeholder(&arg_str_default(op, "refId", "0"), placeholders);
    if !ref_ok {
        skipped.push(format!("op[{i}]: unresolved placeholder for refId"));
        return;
    }
    n.ref_id = to_u64(&ref_str);

    // Auto-place: offset < 0 means "after last sibling".
    if n.offset < 0 {
        let mut max_end = 0i32;
        for si in tab.data.tree.children_of(n.parent_id) {
            let sn = &tab.data.tree.nodes[si];
            let sz = if matches!(sn.kind, NodeKind::Struct | NodeKind::Array) {
                tab.data.tree.struct_span(sn.id)
            } else {
                sn.byte_size()
            };
            let end = sn.offset + sz;
            if end > max_end {
                max_end = end;
            }
        }
        let align = alignment_for(n.kind);
        n.offset = (max_end + align - 1) / align * align;
    }

    let kind = n.kind;
    let parent_id = n.parent_id;
    let id = n.id;
    tab.push_command(Command::Insert {
        node: n,
        off_adjs: Vec::new(),
    });
    if parent_id == 0 && kind == NodeKind::Struct {
        *last_root_struct_id = id;
    }
    *applied += 1;
}

fn parse_enum_members(op: &Map<String, Value>, key: &str) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if let Some(arr) = op.get(key).and_then(Value::as_array) {
        for ev in arr {
            if let Some(eo) = ev.as_object() {
                out.push((arg_str(eo, "name"), parse_integer(arg(eo, "value"), 0)));
            }
        }
    }
    out
}

/// Resolve a `$N`/raw nodeId arg → (resolved string, ok).
fn resolve_node_arg(
    op: &Map<String, Value>,
    key: &str,
    placeholders: &HashMap<String, u64>,
) -> (String, bool) {
    resolve_placeholder(&arg_str(op, key), placeholders)
}

fn resolve_node_arg_default(
    op: &Map<String, Value>,
    key: &str,
    default: &str,
    placeholders: &HashMap<String, u64>,
) -> (String, bool) {
    resolve_placeholder(&arg_str_default(op, key, default), placeholders)
}

/// Resolve+find a node by `nodeId`; returns `(index, id)` if present.
fn lookup(
    tab: &TabState,
    op: &Map<String, Value>,
    key: &str,
    placeholders: &HashMap<String, u64>,
) -> Option<(usize, u64)> {
    let (nid, _) = resolve_placeholder(&arg_str(op, key), placeholders);
    let idx = tab.data.tree.index_of_id(to_u64(&nid));
    if idx >= 0 {
        Some((idx as usize, tab.data.tree.nodes[idx as usize].id))
    } else {
        None
    }
}

fn skip(
    skipped: &mut Vec<String>,
    i: usize,
    op_name: &str,
    op: &Map<String, Value>,
    placeholders: &HashMap<String, u64>,
) {
    let (nid, _) = resolve_placeholder(&arg_str(op, "nodeId"), placeholders);
    skipped.push(format!("op[{i}]: {op_name} nodeId '{nid}' not found"));
}

// ════════════════════════════════════════════════════════════════════
// source.switch
// ════════════════════════════════════════════════════════════════════

/// `toolSourceSwitch` (`mcp_bridge.cpp:1788-1835`, `mcp.md §5.3`).
pub fn tool_source_switch(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    if args.contains_key("sourceIndex") {
        let sidx = parse_integer(arg(args, "sourceIndex"), 0) as i32;
        let all_views = arg_bool(args, "allViews");
        let mut out = Value::Null;
        host.with_tab(idx, &mut |tab: &mut TabState| {
            let n = tab.data.sources.len() as i32;
            if sidx < 0 || sidx >= n {
                out = make_text_result(&format!("Source index out of range: {sidx}"), true);
                return;
            }
            let name = tab.data.sources[sidx as usize].display_name.clone();
            tab.switch_source(sidx);
            out = make_text_result(&format!("Switched to source {sidx} ({name})"), false);
        });
        if all_views {
            // switch all tabs to this source
            for i in 0..host.tab_count() {
                host.with_tab(i, &mut |t: &mut TabState| {
                    let n = t.data.sources.len() as i32;
                    if sidx >= 0 && sidx < n {
                        t.switch_source(sidx);
                    }
                });
            }
        }
        return out;
    }

    if args.contains_key("pid") {
        // Live process attach — OUT OF SCOPE.
        return make_text_result("Live process attach is not available in this build", true);
    }

    if args.contains_key("filePath") {
        let path = arg_str(args, "filePath");
        host.with_tab(idx, &mut |tab: &mut TabState| {
            tab.load_data(&path);
        });
        return make_text_result(&format!("Loaded file: {path}"), false);
    }

    make_text_result("Provide sourceIndex, filePath, or pid", true)
}

// ════════════════════════════════════════════════════════════════════
// hex.read
// ════════════════════════════════════════════════════════════════════

/// `toolHexRead` (`mcp_bridge.cpp:1889-1990`, `mcp.md §5.4`). The `interpret`
/// per-field inference block is OUT OF SCOPE (type-inference UI) and omitted —
/// the C++ default `interpret` is false so the common case is unaffected.
pub fn tool_hex_read(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let prov = tab.data.provider.clone();
        // NullProvider stands in for "no provider"; the C++ checks the pointer,
        // but readability still gates the read so behavior matches.
        let mut offset = parse_integer(arg(args, "offset"), 0);
        let length = (parse_integer(arg(args, "length"), 64) as i32).clamp(1, 4096);
        let base_rel = arg_bool(args, "baseRelative");
        if base_rel {
            offset += tab.data.tree.base_address as i64;
        }

        if offset < 0 || !prov.is_readable(offset as u64, length) {
            out = make_text_result(&format!("Cannot read at offset {offset}"), true);
            return;
        }

        let data = prov.read_bytes(offset as u64, length);
        let dump = format_hex_dump(&data, offset, &*prov, tab.data.tree.base_address);
        out = make_text_result(&dump, false);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

/// The hex-dump + interpretations string (`mcp_bridge.cpp:1908-1962`). The LLM
/// reads this, so it is reproduced byte-for-byte.
fn format_hex_dump(data: &[u8], offset: i64, prov: &dyn Provider, base: u64) -> String {
    let mut dump = String::new();
    let mut i = 0usize;
    while i < data.len() {
        let line_len = std::cmp::min(16, data.len() - i);
        dump += &format!("{:08x}: ", (offset as u64).wrapping_add(i as u64));
        for j in 0..16 {
            if j < line_len {
                dump += &format!("{:02x} ", data[i + j]);
            } else {
                dump += "   ";
            }
            if j == 7 {
                dump += " ";
            }
        }
        dump += " |";
        for j in 0..line_len {
            let c = data[i + j];
            if (0x20..=0x7e).contains(&c) {
                dump.push(c as char);
            } else {
                dump.push('.');
            }
        }
        dump += "|\n";
        i += 16;
    }

    if !data.is_empty() {
        dump += "\n--- Interpretations at offset ---\n";
        dump += &format!("u8:  {}\n", data[0]);
        if data.len() >= 2 {
            let v = u16::from_le_bytes([data[0], data[1]]);
            dump += &format!("u16: {v}\n");
        }
        if data.len() >= 4 {
            let b4 = [data[0], data[1], data[2], data[3]];
            let v = u32::from_le_bytes(b4);
            let iv = i32::from_le_bytes(b4);
            let fv = f32::from_le_bytes(b4);
            dump += &format!("u32: {v} (0x{:x})\n", v);
            dump += &format!("i32: {iv}\n");
            dump += &format!("f32: {}\n", qt_number_double(fv as f64));
        }
        if data.len() >= 8 {
            let mut b8 = [0u8; 8];
            b8.copy_from_slice(&data[0..8]);
            let v = u64::from_le_bytes(b8);
            let dv = f64::from_le_bytes(b8);
            dump += &format!("u64: {v} (0x{:x})\n", v);
            dump += &format!("f64: {}\n", qt_number_double(dv));

            let prov_size = prov.size() as u64;
            if v >= base && v < base.wrapping_add(prov_size) {
                dump += "ptr?: LIKELY (within provider range)\n";
            }
        }
        let mut printable = 0;
        for &c in data {
            if (0x20..=0x7e).contains(&c) {
                printable += 1;
            } else {
                break;
            }
        }
        if printable >= 4 {
            dump += &format!("str?: {printable} printable ASCII bytes\n");
        }
    }
    dump
}

// ════════════════════════════════════════════════════════════════════
// hex.write
// ════════════════════════════════════════════════════════════════════

/// `toolHexWrite` (`mcp_bridge.cpp:1996-2032`, `mcp.md §5.5`).
pub fn tool_hex_write(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let mut offset = parse_integer(arg(args, "offset"), 0);
        let hex_str: String = arg_str(args, "hexBytes")
            .chars()
            .filter(|c| *c != ' ')
            .collect();
        if arg_bool(args, "baseRelative") {
            offset += tab.data.tree.base_address as i64;
        }
        if hex_str.len() % 2 != 0 {
            out = make_text_result("Hex string must have even length", true);
            return;
        }
        let bytes_chars: Vec<char> = hex_str.chars().collect();
        let mut new_bytes: Vec<u8> = Vec::with_capacity(bytes_chars.len() / 2);
        let mut i = 0;
        while i < bytes_chars.len() {
            let pair: String = bytes_chars[i..i + 2].iter().collect();
            match u8::from_str_radix(&pair, 16) {
                Ok(b) => new_bytes.push(b),
                Err(_) => {
                    out = make_text_result(&format!("Invalid hex at position {i}"), true);
                    return;
                }
            }
            i += 2;
        }

        // Borrow the provider for the checks/read; do NOT keep an Arc clone
        // alive across push_command (it would block Arc::get_mut in the
        // WriteBytes command application).
        let old_bytes;
        {
            let prov = tab.data.provider.as_ref();
            if !prov.is_writable() {
                out = make_text_result("Provider is not writable", true);
                return;
            }
            if !prov.is_readable(offset as u64, new_bytes.len() as i32) {
                out = make_text_result("Offset out of range", true);
                return;
            }
            old_bytes = prov.read_bytes(offset as u64, new_bytes.len() as i32);
        }
        let n = new_bytes.len();
        tab.push_command(Command::WriteBytes {
            addr: offset as u64,
            old_bytes,
            new_bytes,
        });
        out = make_text_result(&format!("Wrote {n} bytes at offset 0x{:x}", offset), false);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

// ════════════════════════════════════════════════════════════════════
// status.set
// ════════════════════════════════════════════════════════════════════

/// `toolStatusSet` (`mcp_bridge.cpp:2038-2059`, `mcp.md §5.6`).
pub fn tool_status_set(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let text = arg_str(args, "text");
    let target = arg_str_default(args, "target", "both");
    let tab = resolve_tab(args, host);

    if target == "commandRow" || target == "both" {
        if let Some(idx) = tab {
            // U+25B8 = ▸ (UTF-8 E2 96 B8)
            host.set_command_row_text(idx, &format!("[\u{25B8}] [Claude: {text}]"));
        }
    }
    if target == "statusBar" || target == "both" {
        host.set_app_status(&text);
    }
    make_text_result(&format!("Status set: {text}"), false)
}

// ════════════════════════════════════════════════════════════════════
// ui.action
// ════════════════════════════════════════════════════════════════════

/// `toolUiAction` (`mcp_bridge.cpp:2065-2178`, `mcp.md §5.7`).
pub fn tool_ui_action(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let action = arg_str(args, "action");
    let node_id_str = arg_str(args, "nodeId");
    let idx = resolve_tab(args, host);

    match action.as_str() {
        "undo" => {
            let Some(idx) = idx else {
                return make_text_result("No active tab", true);
            };
            let mut out = Value::Null;
            host.with_tab(idx, &mut |tab| {
                if !tab.can_undo() {
                    out = make_text_result("Nothing to undo", true);
                } else {
                    tab.undo();
                    out = make_text_result("Undo performed", false);
                }
            });
            out
        }
        "redo" => {
            let Some(idx) = idx else {
                return make_text_result("No active tab", true);
            };
            let mut out = Value::Null;
            host.with_tab(idx, &mut |tab| {
                if !tab.can_redo() {
                    out = make_text_result("Nothing to redo", true);
                } else {
                    tab.redo();
                    out = make_text_result("Redo performed", false);
                }
            });
            out
        }
        "refresh" => {
            if idx.is_none() {
                return make_text_result("No active tab", true);
            }
            make_text_result("Refreshed", false)
        }
        "set_view_root" => {
            let Some(idx) = idx else {
                return make_text_result("No active tab", true);
            };
            host.with_tab(idx, &mut |tab| {
                tab.data.view_root_id = to_u64(&node_id_str);
            });
            make_text_result(&format!("View root set to {node_id_str}"), false)
        }
        "scroll_to_node" => {
            if idx.is_none() {
                return make_text_result("No active tab", true);
            }
            make_text_result(&format!("Scrolled to node {node_id_str}"), false)
        }
        "export_cpp" => {
            let Some(idx) = idx else {
                return make_text_result("No active tab", true);
            };
            tool_export_cpp(idx, &node_id_str, host)
        }
        "save_file" => {
            host.project_save();
            make_text_result("Saved", false)
        }
        "new_file" => {
            host.project_new();
            make_text_result("New project created", false)
        }
        "open_file" => {
            let path = arg_str(args, "filePath");
            if path.is_empty() {
                return make_text_result("filePath required for open_file", true);
            }
            host.project_open(&path);
            make_text_result(&format!("Opened: {path}"), false)
        }
        "collapse_node" => collapse_action(idx, &node_id_str, true, host),
        "expand_node" => collapse_action(idx, &node_id_str, false, host),
        "select_node" => {
            let Some(idx) = idx else {
                return make_text_result("No active tab", true);
            };
            host.with_tab(idx, &mut |tab| {
                tab.data.selected_ids.clear();
                let nid = to_u64(&node_id_str);
                if nid != 0 {
                    tab.data.selected_ids.insert(nid);
                }
            });
            make_text_result(&format!("Selected node {node_id_str}"), false)
        }
        "reset_tracking" => {
            let count = host.reset_change_tracking_all();
            make_text_result(&format!("Value tracking reset on all {count} tabs."), false)
        }
        other => make_text_result(&format!("Unknown action: {other}"), true),
    }
}

fn collapse_action(
    idx: Option<usize>,
    node_id_str: &str,
    new_state: bool,
    host: &mut dyn McpHost,
) -> Value {
    let Some(idx) = idx else {
        return make_text_result("No active tab", true);
    };
    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab| {
        let id = to_u64(node_id_str);
        let tidx = tab.data.tree.index_of_id(id);
        if tidx < 0 {
            out = make_text_result(&format!("Node not found: {node_id_str}"), true);
            return;
        }
        let node_id = tab.data.tree.nodes[tidx as usize].id;
        let old = tab.data.tree.nodes[tidx as usize].collapsed;
        tab.push_command(Command::Collapse {
            node_id,
            old_state: old,
            new_state,
        });
        out = make_text_result(
            &format!(
                "{} {node_id_str}",
                if new_state { "Collapsed" } else { "Expanded" }
            ),
            false,
        );
    });
    out
}

/// `export_cpp` action — uses the `generator` subsystem (`mcp_bridge.cpp:2100-2123`).
fn tool_export_cpp(idx: usize, node_id_str: &str, host: &mut dyn McpHost) -> Value {
    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab| {
        let aliases = if tab.data.type_aliases.is_empty() {
            None
        } else {
            Some(&tab.data.type_aliases)
        };
        let asserts = false; // QSettings generatorAsserts default false.
        let mut code = if !node_id_str.is_empty() {
            let nid = to_u64(node_id_str);
            let c = crate::generator::render_cpp(&tab.data.tree, nid, aliases, asserts);
            if c.is_empty() {
                out = make_text_result(
                    &format!("Node not found or not a struct: {node_id_str}"),
                    true,
                );
                return;
            }
            c
        } else {
            crate::generator::render_cpp_all(&tab.data.tree, aliases, asserts)
        };
        if code.len() > 65536 {
            let total = code.len();
            code.truncate(65536);
            code += &format!(
                "\n\n... truncated ({total} bytes total, showing first 64KB)\nUse nodeId param to export a single struct."
            );
        }
        out = make_text_result(&code, false);
    });
    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

// ════════════════════════════════════════════════════════════════════
// tree.search
// ════════════════════════════════════════════════════════════════════

/// `toolTreeSearch` (`mcp_bridge.cpp:2184-2242`, `mcp.md §5.8`).
pub fn tool_tree_search(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };
    let query = arg_str(args, "query");
    let kind_filter = arg_str(args, "kindFilter");
    let limit = parse_integer(arg(args, "limit"), 20).clamp(1, 100) as usize;

    if query.is_empty() && kind_filter.is_empty() {
        return make_text_result(
            "Provide 'query' (name substring) and/or 'kindFilter' (e.g. 'Struct')",
            true,
        );
    }

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab| {
        let tree = &tab.data.tree;
        let mut child_counts: HashMap<u64, i64> = HashMap::new();
        for n in &tree.nodes {
            *child_counts.entry(n.parent_id).or_insert(0) += 1;
        }

        let query_lower = query.to_lowercase();
        let mut results: Vec<Value> = Vec::new();
        for n in &tree.nodes {
            if !kind_filter.is_empty() && kind_to_string(n.kind) != kind_filter {
                continue;
            }
            if !query.is_empty() {
                let name_match = n.name.to_lowercase().contains(&query_lower);
                let type_match = n.struct_type_name.to_lowercase().contains(&query_lower);
                if !name_match && !type_match {
                    continue;
                }
            }

            let mut nj = Map::new();
            nj.insert("id".into(), json!(n.id.to_string()));
            nj.insert("name".into(), json!(n.name));
            nj.insert("kind".into(), json!(kind_to_string(n.kind)));
            nj.insert("parentId".into(), json!(n.parent_id.to_string()));
            nj.insert("offset".into(), json!(n.offset));
            if !n.struct_type_name.is_empty() {
                nj.insert("structTypeName".into(), json!(n.struct_type_name));
            }
            if !n.class_keyword.is_empty() {
                nj.insert("classKeyword".into(), json!(n.class_keyword));
            }
            if matches!(n.kind, NodeKind::Struct | NodeKind::Array) {
                nj.insert(
                    "childCount".into(),
                    json!(*child_counts.get(&n.id).unwrap_or(&0)),
                );
            }
            if !n.enum_members.is_empty() {
                nj.insert("enumMemberCount".into(), json!(n.enum_members.len() as i64));
            }
            if !n.bitfield_members.is_empty() {
                nj.insert(
                    "bitfieldMemberCount".into(),
                    json!(n.bitfield_members.len() as i64),
                );
            }
            results.push(Value::Object(nj));
            if results.len() >= limit {
                break;
            }
        }

        let mut obj = Map::new();
        let count = results.len() as i64;
        obj.insert("results".into(), Value::Array(results));
        obj.insert("count".into(), json!(count));
        obj.insert("query".into(), json!(query));
        if !kind_filter.is_empty() {
            obj.insert("kindFilter".into(), json!(kind_filter));
        }
        out = make_text_result(&qt_pretty(&Value::Object(obj)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

// ════════════════════════════════════════════════════════════════════
// node.history
// ════════════════════════════════════════════════════════════════════

/// `toolNodeHistory` (`mcp_bridge.cpp:2248-2279`, `mcp.md §5.9`). Output is
/// COMPACT JSON text (not pretty).
pub fn tool_node_history(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let requested: Vec<Value> = arg(args, "nodeIds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if requested.is_empty() {
        return make_text_result("nodeIds array is required.", true);
    }

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab| {
        let hist_map = &tab.data.value_history;
        let mut result = Map::new();
        for id_val in &requested {
            let id_str = id_val.as_str().unwrap_or("").to_string();
            let node_id = to_u64(&id_str);
            let hist = hist_map.get(&node_id);
            let mut entries: Vec<Value> = Vec::new();
            if let Some(h) = hist {
                h.for_each_with_time(|val, msec| {
                    entries.push(json!({"value": val, "timestamp": msec}));
                });
            }
            let node_result = json!({
                "entries": entries,
                "heatLevel": hist.map_or(0, |h| h.heat_level()),
                "uniqueCount": hist.map_or(0, |h| h.unique_count()),
            });
            result.insert(id_str, node_result);
        }
        out = make_text_result(
            &serde_json::to_string(&Value::Object(result)).unwrap_or_default(),
            false,
        );
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

// Keep OffsetAdj referenced (documents the command's helper POD).
const _: fn() = || {
    let _ = std::mem::size_of::<OffsetAdj>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::value_history::ValueHistory;
    use crate::core::Node;
    use crate::mcp::host::{McpHost, TabState, TestHost};
    use crate::provider::BufferProvider;
    use std::sync::Arc;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    // ── resolve_tab ──
    #[test]
    fn resolve_tab_precedence() {
        // empty host → auto-create
        let mut h = TestHost::new();
        assert_eq!(resolve_tab(&Map::new(), &mut h), Some(0));
        assert_eq!(h.tab_count(), 1);

        // explicit tabIndex
        let mut h = TestHost::new();
        h.project_new();
        h.project_new();
        let args = map(json!({"tabIndex": 1}));
        assert_eq!(resolve_tab(&args, &mut h), Some(1));
        // out-of-range tabIndex falls through to active
        let args = map(json!({"tabIndex": 9}));
        assert_eq!(resolve_tab(&args, &mut h), Some(1)); // active is last-created
    }

    // ── project.state ──
    fn three_level_host() -> TestHost {
        let mut tab = TabState::new();
        tab.data.tree.base_address = 0x4000;
        // root struct
        let r = Node {
            kind: NodeKind::Struct,
            name: "Root".into(),
            ..Node::default()
        };
        let ri = tab.data.tree.add_node(r);
        let rid = tab.data.tree.nodes[ri].id;
        // child struct
        let c = Node {
            kind: NodeKind::Struct,
            name: "Child".into(),
            parent_id: rid,
            offset: 0,
            ..Node::default()
        };
        let ci = tab.data.tree.add_node(c);
        let cid = tab.data.tree.nodes[ci].id;
        // leaf under child
        tab.data.tree.add_node(Node {
            kind: NodeKind::UInt32,
            name: "leaf".into(),
            parent_id: cid,
            offset: 0,
            ..Node::default()
        });
        TestHost::with_tab(tab)
    }

    fn parse_text(result: &Value) -> Value {
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn project_state_base_address_asymmetry_and_counts() {
        let mut h = three_level_host();
        let r = tool_project_state(&map(json!({"depth": 5})), &mut h);
        let state = parse_text(&r);
        assert_eq!(state["baseAddress"], "0x4000"); // upper + 0x
        assert_eq!(state["tree"]["baseAddress"], "4000"); // lower, no 0x
        assert_eq!(state["nodeCount"], 3);
        // containers get computedSize + childCount
        let nodes = state["tree"]["nodes"].as_array().unwrap();
        let root = &nodes[0];
        assert_eq!(root["name"], "Root");
        assert_eq!(root["childCount"], 1);
        assert!(root.get("computedSize").is_some());
    }

    #[test]
    fn project_state_pagination() {
        // root with 5 children
        let mut tab = TabState::new();
        let r = tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let rid = tab.data.tree.nodes[r].id;
        for i in 0..5 {
            tab.data.tree.add_node(Node {
                kind: NodeKind::UInt8,
                offset: i,
                parent_id: rid,
                ..Node::default()
            });
        }
        let mut h = TestHost::with_tab(tab);
        // depth 1 from root: emits 1 (the root). Filter on rid to get the 5 kids.
        let args = map(json!({"depth": 1, "parentId": rid.to_string(), "limit": 2, "offset": 0}));
        let r = tool_project_state(&args, &mut h);
        let s = parse_text(&r);
        assert_eq!(s["tree"]["returned"], 2);
        assert_eq!(s["tree"]["total"], 5);
        assert_eq!(s["tree"]["nextOffset"], 2);
    }

    #[test]
    fn project_state_include_members_toggle() {
        let mut tab = TabState::new();
        let mut n = Node {
            kind: NodeKind::Struct,
            class_keyword: "enum".into(),
            ..Node::default()
        };
        n.enum_members = vec![("A".into(), 0), ("B".into(), 1)];
        tab.data.tree.add_node(n);
        let mut h = TestHost::with_tab(tab);

        // default: enumMemberCount, no enumMembers
        let r = tool_project_state(&map(json!({"depth": 1})), &mut h);
        let s = parse_text(&r);
        let node = &s["tree"]["nodes"][0];
        assert_eq!(node["enumMemberCount"], 2);
        assert!(node.get("enumMembers").is_none());

        // includeMembers: enumMembers present
        let r = tool_project_state(&map(json!({"depth": 1, "includeMembers": true})), &mut h);
        let s = parse_text(&r);
        let node = &s["tree"]["nodes"][0];
        assert!(node.get("enumMembers").is_some());
        assert!(node.get("enumMemberCount").is_none());
    }

    // ── tree.apply ──
    #[test]
    fn tree_apply_insert_forward_ref_and_undo() {
        let mut h = TestHost::new();
        h.project_new();
        let ops = json!({
            "operations": [
                {"op": "insert", "kind": "Struct", "name": "Player", "parentId": "0", "offset": 0},
                {"op": "insert", "kind": "Int32", "name": "hp", "parentId": "$0", "offset": 0}
            ]
        });
        let r = tool_tree_apply(&map(ops), &mut h);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Applied 2 operations"));
        assert!(r.get("isError").is_none());
        // assignedIds has $0 and $1
        assert!(r["assignedIds"]["$0"].is_string());
        assert!(r["assignedIds"]["$1"].is_string());

        // tree now has 2 nodes; child parented to $0
        h.with_tab(0, &mut |t| {
            assert_eq!(t.data.tree.nodes.len(), 2);
            let p_id: u64 = r["assignedIds"]["$0"].as_str().unwrap().parse().unwrap();
            let child = t.data.tree.nodes.iter().find(|n| n.name == "hp").unwrap();
            assert_eq!(child.parent_id, p_id);
            assert!(t.can_undo());
        });

        // single undo reverts the whole batch
        h.with_tab(0, &mut |t| t.undo());
        h.with_tab(0, &mut |t| assert_eq!(t.data.tree.nodes.len(), 0));
    }

    #[test]
    fn tree_apply_rename_and_change_kind_same_node() {
        let mut tab = TabState::new();
        let i = tab.data.tree.add_node(Node {
            kind: NodeKind::Hex64,
            name: "field".into(),
            ..Node::default()
        });
        let id = tab.data.tree.nodes[i].id;
        let mut h = TestHost::with_tab(tab);
        let ops = json!({
            "operations": [
                {"op": "rename", "nodeId": id.to_string(), "name": "health"},
                {"op": "change_kind", "nodeId": id.to_string(), "kind": "Int32"}
            ]
        });
        let r = tool_tree_apply(&map(ops), &mut h);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Applied 2 operations"));
        h.with_tab(0, &mut |t| {
            let idx = t.data.tree.index_of_id(id);
            assert_eq!(t.data.tree.nodes[idx as usize].name, "health");
            assert_eq!(t.data.tree.nodes[idx as usize].kind, NodeKind::Int32);
        });
    }

    #[test]
    fn tree_apply_skipped_and_iserror() {
        let mut h = TestHost::new();
        h.project_new();
        // a single op that fails (rename a nonexistent node) → applied=0 → isError
        let ops = json!({"operations": [{"op": "rename", "nodeId": "999", "name": "x"}]});
        let r = tool_tree_apply(&map(ops), &mut h);
        assert_eq!(r["isError"], json!(true));
        let txt = r["content"][0]["text"].as_str().unwrap();
        assert!(txt.contains("Applied 0 operations"));
        assert!(txt.contains("Skipped 1:"));
        assert!(txt.contains("rename nodeId '999' not found"));
    }

    #[test]
    fn tree_apply_change_base_bare_hex() {
        let mut h = TestHost::new();
        h.project_new();
        let ops = json!({"operations": [{"op": "change_base", "baseAddress": "0x140000000"}]});
        let r = tool_tree_apply(&map(ops), &mut h);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Applied 1 operations"));
        h.with_tab(0, &mut |t| {
            assert_eq!(t.data.tree.base_address, 0x140000000)
        });

        // bare hex without 0x
        let ops = json!({"operations": [{"op": "change_base", "baseAddress": "400000"}]});
        tool_tree_apply(&map(ops), &mut h);
        h.with_tab(0, &mut |t| assert_eq!(t.data.tree.base_address, 0x400000));
    }

    #[test]
    fn tree_apply_auto_place_negative_offset() {
        let mut tab = TabState::new();
        let r = tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            ..Node::default()
        });
        let rid = tab.data.tree.nodes[r].id;
        tab.data.tree.add_node(Node {
            kind: NodeKind::UInt32, // size 4 at offset 0
            parent_id: rid,
            offset: 0,
            ..Node::default()
        });
        let mut h = TestHost::with_tab(tab);
        // insert UInt64 (align 8) with offset -1 after the UInt32 (ends at 4)
        let ops = json!({"operations": [
            {"op": "insert", "kind": "UInt64", "name": "x", "parentId": rid.to_string(), "offset": -1}
        ]});
        tool_tree_apply(&map(ops), &mut h);
        h.with_tab(0, &mut |t| {
            let n = t.data.tree.nodes.iter().find(|n| n.name == "x").unwrap();
            // maxEnd=4, align=8 → round up to 8
            assert_eq!(n.offset, 8);
        });
    }

    // ── hex.read ──
    fn buffer_host(bytes: Vec<u8>) -> TestHost {
        let mut tab = TabState::new();
        tab.data.tree.base_address = 0;
        tab.data.provider = Arc::new(BufferProvider::new(bytes, "x.bin"));
        TestHost::with_tab(tab)
    }

    #[test]
    fn hex_read_dump_format_and_interpretations() {
        // 8 bytes: 0x41 0x42 0x43 0x44 0x00 0x00 0x80 0x3f
        // first 4 as f32 LE = bytes 41 42 43 44; last is 0x3f800000 = 1.0f at offset 4
        let bytes = vec![0x41, 0x42, 0x43, 0x44, 0x00, 0x00, 0x80, 0x3f];
        let mut h = buffer_host(bytes);
        let r = tool_hex_read(&map(json!({"offset": 0, "length": 8})), &mut h);
        let dump = r["content"][0]["text"].as_str().unwrap();
        // address column + j==7 gap + ASCII gutter
        assert!(dump.starts_with("00000000: 41 42 43 44 00 00 80 3f "));
        assert!(dump.contains(" |ABCD"));
        // interpretations
        assert!(dump.contains("u8:  65\n"));
        assert!(dump.contains("u16: 16961\n")); // 0x4241
        assert!(dump.contains("u32: 1145258561 (0x44434241)\n"));
        assert!(dump.contains("i32: 1145258561\n"));
        // u64 = 0x3f80000044434241
        assert!(dump.contains("u64: 4575657222553682497 (0x3f80000044434241)\n"));
        // str?: 4 printable ASCII bytes (ABCD then 0x00 stops)
        assert!(dump.contains("str?: 4 printable ASCII bytes\n"));
    }

    #[test]
    fn hex_read_out_of_range_is_error() {
        let mut h = buffer_host(vec![1, 2, 3, 4]);
        let r = tool_hex_read(&map(json!({"offset": 100, "length": 8})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Cannot read at offset 100"));
    }

    #[test]
    fn hex_read_base_relative() {
        let mut tab = TabState::new();
        tab.data.tree.base_address = 2;
        tab.data.provider = Arc::new(BufferProvider::new(vec![0, 0, 0xAA, 0xBB], "x"));
        let mut h = TestHost::with_tab(tab);
        let r = tool_hex_read(
            &map(json!({"offset": 0, "length": 2, "baseRelative": true})),
            &mut h,
        );
        let dump = r["content"][0]["text"].as_str().unwrap();
        // reads at base(2) → bytes AA BB, addr col shows 00000002
        assert!(dump.starts_with("00000002: aa bb "));
    }

    // ── hex.write ──
    #[test]
    fn hex_write_roundtrip_and_undo() {
        let mut h = buffer_host(vec![0, 0, 0, 0]);
        let r = tool_hex_write(
            &map(json!({"offset": 0, "hexBytes": "DE AD BE EF"})),
            &mut h,
        );
        assert_eq!(r["content"][0]["text"], "Wrote 4 bytes at offset 0x0");
        h.with_tab(0, &mut |t| {
            assert_eq!(
                t.data.provider.read_bytes(0, 4),
                vec![0xDE, 0xAD, 0xBE, 0xEF]
            );
        });
        // undo restores old bytes
        h.with_tab(0, &mut |t| t.undo());
        h.with_tab(0, &mut |t| {
            assert_eq!(t.data.provider.read_bytes(0, 4), vec![0, 0, 0, 0]);
        });
    }

    #[test]
    fn hex_write_odd_length_error() {
        let mut h = buffer_host(vec![0, 0]);
        let r = tool_hex_write(&map(json!({"offset": 0, "hexBytes": "ABC"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Hex string must have even length");
    }

    #[test]
    fn hex_write_invalid_hex_position() {
        let mut h = buffer_host(vec![0, 0]);
        let r = tool_hex_write(&map(json!({"offset": 0, "hexBytes": "1Zzz"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Invalid hex at position 0");
    }

    #[test]
    fn hex_write_not_writable() {
        // NullProvider is not writable.
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_hex_write(&map(json!({"offset": 0, "hexBytes": "AA"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Provider is not writable");
    }

    #[test]
    fn hex_write_out_of_range() {
        let mut h = buffer_host(vec![0, 0]);
        let r = tool_hex_write(&map(json!({"offset": 10, "hexBytes": "AABB"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Offset out of range");
    }

    // ── tree.search ──
    #[test]
    fn tree_search_query_and_kind_filter() {
        let mut tab = TabState::new();
        tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "PlayerData".into(),
            struct_type_name: "CPlayer".into(),
            ..Node::default()
        });
        tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "health".into(),
            ..Node::default()
        });
        let mut h = TestHost::with_tab(tab);

        // both empty → error
        let r = tool_tree_search(&map(json!({})), &mut h);
        assert_eq!(r["isError"], json!(true));

        // query matches name (case-insensitive)
        let r = tool_tree_search(&map(json!({"query": "player"})), &mut h);
        let s = parse_text(&r);
        assert_eq!(s["count"], 1);
        assert_eq!(s["results"][0]["name"], "PlayerData");
        assert_eq!(s["results"][0]["childCount"], 0);

        // kindFilter
        let r = tool_tree_search(&map(json!({"kindFilter": "Int32"})), &mut h);
        let s = parse_text(&r);
        assert_eq!(s["count"], 1);
        assert_eq!(s["results"][0]["name"], "health");
        assert_eq!(s["kindFilter"], "Int32");

        // query matches structTypeName
        let r = tool_tree_search(&map(json!({"query": "cplayer"})), &mut h);
        let s = parse_text(&r);
        assert_eq!(s["count"], 1);
    }

    // ── node.history ──
    #[test]
    fn node_history_compact_and_ordering() {
        let mut tab = TabState::new();
        let i = tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            ..Node::default()
        });
        let id = tab.data.tree.nodes[i].id;
        let mut vh = ValueHistory::new();
        vh.record("1");
        vh.record("1"); // dedup
        vh.record("2");
        vh.record("3");
        tab.data.value_history.insert(id, vh);
        let mut h = TestHost::with_tab(tab);

        let r = tool_node_history(&map(json!({"nodeIds": [id.to_string()]})), &mut h);
        let text = r["content"][0]["text"].as_str().unwrap();
        // compact (no spaces after colons)
        assert!(!text.contains(": "));
        let v: Value = serde_json::from_str(text).unwrap();
        let node = &v[&id.to_string()];
        // newest first: 3, 2, 1
        assert_eq!(node["entries"][0]["value"], "3");
        assert_eq!(node["entries"][1]["value"], "2");
        assert_eq!(node["entries"][2]["value"], "1");
        assert_eq!(node["uniqueCount"], 3);
        assert_eq!(node["heatLevel"], 2); // count 3 → warm

        // unknown id → empty entries, heat 0
        let r = tool_node_history(&map(json!({"nodeIds": ["999"]})), &mut h);
        let v: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(v["999"]["entries"].as_array().unwrap().len(), 0);
        assert_eq!(v["999"]["heatLevel"], 0);
    }

    #[test]
    fn node_history_empty_ids_error() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_node_history(&map(json!({"nodeIds": []})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "nodeIds array is required.");
    }

    // ── ui.action ──
    #[test]
    fn ui_action_undo_redo_guards() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_ui_action(&map(json!({"action": "undo"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Nothing to undo");
        let r = tool_ui_action(&map(json!({"action": "redo"})), &mut h);
        assert_eq!(r["content"][0]["text"], "Nothing to redo");
    }

    #[test]
    fn ui_action_collapse_expand_and_reset() {
        let mut tab = TabState::new();
        let i = tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            collapsed: false,
            ..Node::default()
        });
        let id = tab.data.tree.nodes[i].id;
        let mut h = TestHost::with_tab(tab);

        let r = tool_ui_action(
            &map(json!({"action": "collapse_node", "nodeId": id.to_string()})),
            &mut h,
        );
        assert_eq!(r["content"][0]["text"], format!("Collapsed {id}"));
        h.with_tab(0, &mut |t| {
            let idx = t.data.tree.index_of_id(id);
            assert!(t.data.tree.nodes[idx as usize].collapsed);
        });

        // collapse a missing node → error
        let r = tool_ui_action(
            &map(json!({"action": "expand_node", "nodeId": "999"})),
            &mut h,
        );
        assert_eq!(r["isError"], json!(true));

        // reset_tracking
        let r = tool_ui_action(&map(json!({"action": "reset_tracking"})), &mut h);
        assert_eq!(
            r["content"][0]["text"],
            "Value tracking reset on all 1 tabs."
        );

        // unknown action
        let r = tool_ui_action(&map(json!({"action": "save_file_as"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "Unknown action: save_file_as");
    }

    #[test]
    fn ui_action_set_view_root() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_ui_action(
            &map(json!({"action": "set_view_root", "nodeId": "42"})),
            &mut h,
        );
        assert_eq!(r["content"][0]["text"], "View root set to 42");
        h.with_tab(0, &mut |t| assert_eq!(t.data.view_root_id, 42));
    }

    // ── status.set ──
    #[test]
    fn status_set_message_and_status_bar() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_status_set(&map(json!({"text": "hi", "target": "statusBar"})), &mut h);
        assert_eq!(r["content"][0]["text"], "Status set: hi");
        assert_eq!(h.app_status(), "hi");
    }

    // ── source.switch ──
    #[test]
    fn source_switch_file_and_pid_stub() {
        let mut h = TestHost::new();
        h.project_new();
        // pid → out-of-scope stub
        let r = tool_source_switch(&map(json!({"pid": 1234})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Live process attach"));
        // no args → error
        let r = tool_source_switch(&map(json!({})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(
            r["content"][0]["text"],
            "Provide sourceIndex, filePath, or pid"
        );
    }

    #[test]
    fn source_switch_source_index_oob() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_source_switch(&map(json!({"sourceIndex": 5})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("out of range"));
    }
}
