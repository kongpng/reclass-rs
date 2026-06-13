//! In-scope tool handlers + `resolve_tab` (`mcp_bridge.cpp:1175-2442`, `mcp.md §5`).
//!
//! Each tool takes `(args, host)` and returns a tool-result `Value` (via
//! [`make_text_result`] or with extra keys). The tab is resolved with
//! [`resolve_tab`] and borrowed via `host.with_tab`. The load-bearing edge
//! cases (clamps, the hex-dump format, the base-address case/0x asymmetry,
//! placeholder forward refs, the atomic undo macro) are ported 1:1.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use crate::core::command::OffsetAdj;
use crate::core::kind::{alignment_for, kind_from_string, kind_to_string};
use crate::core::node::{
    BitfieldMember, EvidenceEvent, EvidenceHypothesis, EvidenceProposal, Node, K_MAX_ARRAY_LEN,
};
use crate::core::value_history::now_ms;
use crate::core::{Command, NodeKind, NodeTree};
use crate::provider::Provider;
use crate::scanner::pointer::{
    build_pointer_map, find_pointer_chains, format_signed_offset, PointerChainRequest,
    PointerMapRequest, PointerMapSource,
};
use crate::scanner::AddressRange;

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

fn arg_bool_default(args: &Map<String, Value>, k: &str, default: bool) -> bool {
    arg(args, k).and_then(Value::as_bool).unwrap_or(default)
}

#[cfg(feature = "memflow-provider")]
fn arg_string_list(args: &Map<String, Value>, k: &str) -> Vec<String> {
    match arg(args, k) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => s
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
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
        // Evidence summary (`mcp_bridge.cpp:1488-1492`). C++ uses int QVector::size().
        state.insert(
            "evidence".into(),
            json!({
                "eventCount": tree.evidence_events.len() as i64,
                "hypothesisCount": tree.evidence_hypotheses.len() as i64,
                "proposalCount": tree.evidence_proposals.len() as i64,
            }),
        );

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
                "change_comment" => {
                    if let Some((tidx, node_id)) = lookup(tab, op_obj, "nodeId", &placeholders) {
                        let old = tab.data.tree.nodes[tidx].comment.clone();
                        tab.push_command(Command::ChangeComment {
                            node_id,
                            old_comment: old,
                            new_comment: arg_str(op_obj, "comment").trim().to_string(),
                        });
                        applied += 1;
                    } else {
                        skip(&mut skipped, i, "change_comment", op_obj, &placeholders);
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
                        // Controller groupIntoUnion is out-of-scope for the MCP
                        // TabState (no Controller surface here), so unlike C++
                        // we cannot restructure the tree. Skip honestly rather
                        // than reporting a false "Applied".
                        skipped.push(format!(
                            "op[{i}]: group_into_union not available in this build"
                        ));
                    } else {
                        skipped.push(format!("op[{i}]: group_into_union needs >= 2 nodeIds"));
                    }
                }
                "dissolve_union" => {
                    let (nid, _) = resolve_node_arg(op_obj, "nodeId", &placeholders);
                    let union_id = to_u64(&nid);
                    if tab.data.tree.index_of_id(union_id) >= 0 {
                        // Controller dissolveUnion is out-of-scope for the MCP
                        // TabState (no Controller surface here), so unlike C++
                        // we cannot restructure the tree. Skip honestly rather
                        // than reporting a false "Applied".
                        skipped.push(format!(
                            "op[{i}]: dissolve_union not available in this build"
                        ));
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

    let provider = arg_str(args, "provider").to_lowercase();
    let memflow_requested = provider == "memflow"
        || provider == "memflowprocessmemory"
        || args.contains_key("connector")
        || args.contains_key("connectorArgs")
        || args.contains_key("os")
        || args.contains_key("osArgs")
        || args.contains_key("pluginDirs")
        || args.contains_key("writable");
    if memflow_requested {
        #[cfg(feature = "memflow-provider")]
        return tool_source_switch_memflow(idx, args, host);
        #[cfg(not(feature = "memflow-provider"))]
        return make_text_result(
            "memflow live-process provider is not enabled in this build",
            true,
        );
    }

    if provider == "processmemory" || (provider.is_empty() && args.contains_key("pid")) {
        #[cfg(feature = "process-provider")]
        return tool_source_switch_process(idx, args, host);
        #[cfg(not(feature = "process-provider"))]
        return make_text_result(
            "local Process Memory provider is not enabled in this build; \
             use provider:\"memflow\" with a pid for live process access",
            true,
        );
    }

    if provider == "remoteprocessmemory" {
        #[cfg(feature = "remote-process-provider")]
        return tool_source_switch_remote(idx, args, host);
        #[cfg(not(feature = "remote-process-provider"))]
        return make_text_result(
            "Remote Process Memory provider is not enabled in this build",
            true,
        );
    }

    if provider == "kernelmemory" {
        #[cfg(all(windows, feature = "kernel-provider"))]
        return tool_source_switch_kernel(idx, args, host);
        #[cfg(not(all(windows, feature = "kernel-provider")))]
        return make_text_result(
            "Kernel Memory provider is available only on Windows builds with kernel-provider",
            true,
        );
    }

    if provider == "windbgmemory" {
        #[cfg(all(windows, feature = "windbg-provider"))]
        return tool_source_switch_windbg(idx, args, host);
        #[cfg(not(all(windows, feature = "windbg-provider")))]
        return make_text_result(
            "WinDbg Memory provider is available only on Windows builds with windbg-provider",
            true,
        );
    }

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
        return make_text_result(
            "Live process attach requires provider:\"processmemory\", \"remoteprocessmemory\", \"kernelmemory\", \"windbgmemory\", or \"memflow\"",
            true,
        );
    }

    if args.contains_key("filePath") {
        let path = arg_str(args, "filePath");
        host.with_tab(idx, &mut |tab: &mut TabState| {
            tab.load_data(&path);
        });
        return make_text_result(&format!("Loaded file: {path}"), false);
    }

    make_text_result(
        "Provide sourceIndex, filePath, provider:\"processmemory\", provider:\"kernelmemory\", provider:\"windbgmemory\", or provider:\"memflow\"",
        true,
    )
}

// ════════════════════════════════════════════════════════════════════
// source.modules
// ════════════════════════════════════════════════════════════════════

pub fn tool_source_modules(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let modules = tab
            .data
            .provider
            .enumerate_modules()
            .into_iter()
            .map(|m| {
                json!({
                    "name": m.name,
                    "fullPath": m.full_path,
                    "base": format!("0x{:X}", m.base),
                    "size": m.size,
                })
            })
            .collect::<Vec<_>>();
        out = make_text_result(&qt_pretty(&Value::Array(modules)), false);
    });
    if out.is_null() {
        make_text_result("No active tab", true)
    } else {
        out
    }
}

#[cfg(feature = "memflow-provider")]
fn tool_source_switch_memflow(
    idx: usize,
    args: &Map<String, Value>,
    host: &mut dyn McpHost,
) -> Value {
    use crate::provider::{MemflowAttachConfig, MemflowProvider};

    let pid = if args.contains_key("pid") {
        let raw = parse_integer(arg(args, "pid"), -1);
        if raw < 0 || raw > u32::MAX as i64 {
            return make_text_result("pid must be a non-negative u32", true);
        }
        Some(raw as u32)
    } else {
        None
    };
    let mut cfg = MemflowAttachConfig {
        connector: arg_str(args, "connector"),
        connector_args: arg_str(args, "connectorArgs"),
        os: arg_str_default(args, "os", "win32"),
        os_args: arg_str(args, "osArgs"),
        pid,
        process_name: arg_str(args, "processName"),
        writable: arg_bool(args, "writable"),
        inventory_dirs: arg_string_list(args, "pluginDirs"),
    };
    if cfg.os.trim().is_empty() {
        cfg.os = "win32".to_string();
    }
    if let Err(err) = cfg.validate() {
        return make_text_result(&err, true);
    }
    let target = match cfg.to_target() {
        Ok(target) => target,
        Err(err) => return make_text_result(&err, true),
    };
    let provider = match MemflowProvider::attach(cfg) {
        Ok(provider) => provider,
        Err(err) => return make_text_result(&format!("memflow attach failed: {err}"), true),
    };
    let name = provider.name();
    let provider = std::sync::Arc::new(provider);
    host.with_tab(idx, &mut |tab: &mut TabState| {
        tab.attach_provider_with_identifier(
            provider.clone(),
            "memflowprocessmemory",
            target.clone(),
        );
    });
    make_text_result(&format!("Attached memflow process: {name}"), false)
}

#[cfg(feature = "process-provider")]
fn tool_source_switch_process(
    idx: usize,
    args: &Map<String, Value>,
    host: &mut dyn McpHost,
) -> Value {
    use crate::provider::LocalProcessProvider;

    let raw = parse_integer(arg(args, "pid"), -1);
    if raw <= 0 || raw > u32::MAX as i64 {
        return make_text_result("pid must be a positive u32", true);
    }
    let pid = raw as u32;
    let process_name = arg_str(args, "processName");
    let target = if process_name.is_empty() {
        pid.to_string()
    } else {
        format!("{pid}:{process_name}")
    };
    let provider = match LocalProcessProvider::attach(&target) {
        Ok(provider) => provider,
        Err(err) => return make_text_result(&format!("process attach failed: {err}"), true),
    };
    let name = provider.name();
    let provider = std::sync::Arc::new(provider);
    host.with_tab(idx, &mut |tab: &mut TabState| {
        tab.attach_provider_with_identifier(provider.clone(), "processmemory", target.clone());
    });
    make_text_result(&format!("Attached process: {name}"), false)
}

#[cfg(feature = "remote-process-provider")]
fn tool_source_switch_remote(
    idx: usize,
    args: &Map<String, Value>,
    host: &mut dyn McpHost,
) -> Value {
    use crate::provider::RemoteProcessProvider;

    let raw = parse_integer(arg(args, "pid"), -1);
    if raw <= 0 || raw > u32::MAX as i64 {
        return make_text_result("pid must be a positive u32", true);
    }
    let pid = raw as u32;
    let process_name = arg_str(args, "processName");
    if process_name.is_empty() {
        return make_text_result("remoteprocessmemory requires processName", true);
    }
    let target = format!("rpm:{pid}:{process_name}");
    let provider = match RemoteProcessProvider::attach(&target) {
        Ok(provider) => provider,
        Err(err) => return make_text_result(&format!("remote attach failed: {err}"), true),
    };
    let name = provider.name();
    let provider = std::sync::Arc::new(provider);
    host.with_tab(idx, &mut |tab: &mut TabState| {
        tab.attach_provider_with_identifier(
            provider.clone(),
            "remoteprocessmemory",
            target.clone(),
        );
    });
    make_text_result(&format!("Attached remote process: {name}"), false)
}

#[cfg(all(windows, feature = "kernel-provider"))]
fn tool_source_switch_kernel(
    idx: usize,
    args: &Map<String, Value>,
    host: &mut dyn McpHost,
) -> Value {
    use crate::provider::KernelMemoryProvider;

    let target = if args.contains_key("target") {
        arg_str(args, "target")
    } else if args.contains_key("physicalBase") {
        let base = parse_integer(arg(args, "physicalBase"), -1);
        if base < 0 {
            return make_text_result("physicalBase must be a non-negative integer", true);
        }
        format!("phys:{base:X}")
    } else {
        let raw = parse_integer(arg(args, "pid"), -1);
        if raw <= 0 || raw > u32::MAX as i64 {
            return make_text_result("kernelmemory requires pid, target, or physicalBase", true);
        }
        let pid = raw as u32;
        let process_name = arg_str(args, "processName");
        if process_name.is_empty() {
            format!("km:{pid}:PID {pid}")
        } else {
            format!("km:{pid}:{process_name}")
        }
    };
    if target.trim().is_empty() {
        return make_text_result("kernelmemory target cannot be empty", true);
    }
    let provider = match KernelMemoryProvider::attach(&target) {
        Ok(provider) => provider,
        Err(err) => return make_text_result(&format!("kernel attach failed: {err}"), true),
    };
    let name = provider.name();
    let provider = std::sync::Arc::new(provider);
    host.with_tab(idx, &mut |tab: &mut TabState| {
        tab.attach_provider_with_identifier(provider.clone(), "kernelmemory", target.clone());
    });
    make_text_result(&format!("Attached kernel source: {name}"), false)
}

#[cfg(all(windows, feature = "windbg-provider"))]
fn tool_source_switch_windbg(
    idx: usize,
    args: &Map<String, Value>,
    host: &mut dyn McpHost,
) -> Value {
    use crate::provider::WinDbgMemoryProvider;

    let target = if args.contains_key("target") {
        arg_str(args, "target")
    } else {
        let raw = parse_integer(arg(args, "pid"), -1);
        if raw <= 0 || raw > u32::MAX as i64 {
            return make_text_result("windbgmemory requires target or pid", true);
        }
        format!("pid:{raw}")
    };
    if target.trim().is_empty() {
        return make_text_result("windbgmemory target cannot be empty", true);
    }
    let provider = match WinDbgMemoryProvider::attach(&target) {
        Ok(provider) => provider,
        Err(err) => return make_text_result(&format!("WinDbg attach failed: {err}"), true),
    };
    let name = provider.name();
    let provider = std::sync::Arc::new(provider);
    host.with_tab(idx, &mut |tab: &mut TabState| {
        tab.attach_provider_with_identifier(provider.clone(), "windbgmemory", target.clone());
    });
    make_text_result(&format!("Attached WinDbg source: {name}"), false)
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

// ════════════════════════════════════════════════════════════════════
// evidence.* — event-sourced reversing evidence (mcp_bridge.cpp:87-209, 2543-3087)
// ════════════════════════════════════════════════════════════════════

/// `stringListFromJson(value)` (`mcp_bridge.cpp:87-99`). Accepts an array of
/// strings or a single string; drops empties.
fn string_list_from_json(value: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Some(Value::Array(arr)) => {
            for v in arr {
                let s = v.as_str().unwrap_or("");
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
        }
        Some(Value::String(s)) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        _ => {}
    }
    out
}

/// `startsWithAny(s, prefixes)` (`mcp_bridge.cpp:101-106`).
fn starts_with_any(s: &str, prefixes: &[String]) -> bool {
    prefixes
        .iter()
        .any(|p| !p.is_empty() && s.starts_with(p.as_str()))
}

/// `offsetHex(offset)` (`mcp_bridge.cpp:108-112`). Negative → empty string.
fn offset_hex(offset: i32) -> String {
    if offset >= 0 {
        format!("0x{:X}", offset)
    } else {
        String::new()
    }
}

/// `nodeTypeName(n)` (`mcp_bridge.cpp:114-116`).
fn node_type_name(n: &Node) -> String {
    if n.struct_type_name.is_empty() {
        n.name.clone()
    } else {
        n.struct_type_name.clone()
    }
}

/// `rootIndexForNode(tree, idx)` (`mcp_bridge.cpp:118-127`). Walks `parentId`
/// to the root; cycle-guarded.
fn root_index_for_node(tree: &NodeTree, mut idx: i32) -> i32 {
    if idx < 0 || idx as usize >= tree.nodes.len() {
        return -1;
    }
    let mut seen: HashSet<u64> = HashSet::new();
    while idx >= 0 && (idx as usize) < tree.nodes.len() && tree.nodes[idx as usize].parent_id != 0 {
        let id = tree.nodes[idx as usize].id;
        if seen.contains(&id) {
            return -1;
        }
        seen.insert(id);
        idx = tree.index_of_id(tree.nodes[idx as usize].parent_id);
    }
    idx
}

/// `nodePath(tree, nodeId)` (`mcp_bridge.cpp:129-144`). Dotted root→node path.
fn node_path(tree: &NodeTree, node_id: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut cur = node_id;
    while cur != 0 && !seen.contains(&cur) {
        seen.insert(cur);
        let idx = tree.index_of_id(cur);
        if idx < 0 {
            break;
        }
        let n = &tree.nodes[idx as usize];
        let mut part = if n.name.is_empty() {
            node_type_name(n)
        } else {
            n.name.clone()
        };
        if part.is_empty() {
            part = "<unnamed>".to_string();
        }
        parts.insert(0, part);
        cur = n.parent_id;
    }
    parts.join(".")
}

/// `nodeEvidenceContext(tree, nodeId)` (`mcp_bridge.cpp:146-172`).
fn node_evidence_context(tree: &NodeTree, node_id: u64) -> Map<String, Value> {
    let idx = tree.index_of_id(node_id);
    if idx < 0 {
        return Map::new();
    }
    let n = &tree.nodes[idx as usize];
    let mut out = n.to_json().as_object().cloned().unwrap_or_default();
    out.insert("path".into(), json!(node_path(tree, node_id)));
    out.insert("kind".into(), json!(kind_to_string(n.kind)));
    let computed_offset = tree.compute_offset(idx);
    out.insert("computedOffset".into(), json!(computed_offset.to_string()));
    out.insert(
        "computedOffsetHex".into(),
        json!(offset_hex(computed_offset as i32)),
    );
    out.insert("computedSize".into(), json!(tree.total_byte_size(n)));
    let (addr, addr_ok) = tree.absolute_address(idx);
    if addr_ok {
        out.insert("absoluteAddress".into(), json!(format!("0x{:X}", addr)));
    }
    let root_idx = root_index_for_node(tree, idx);
    if root_idx >= 0 {
        let root = &tree.nodes[root_idx as usize];
        out.insert("rootNodeId".into(), json!(root.id.to_string()));
        out.insert("rootTypeName".into(), json!(node_type_name(root)));
        let field_offset = (computed_offset - tree.compute_offset(root_idx)) as i32;
        out.insert("fieldOffset".into(), json!(field_offset));
        out.insert("fieldOffsetHex".into(), json!(offset_hex(field_offset)));
    }
    out
}

/// `targetMatches(...)` (`mcp_bridge.cpp:174-181`).
fn target_matches(
    have_type: &str,
    have_node_id: u64,
    have_offset: i32,
    want_type: &str,
    want_node_id: u64,
    want_offset: i32,
) -> bool {
    if want_node_id != 0 && have_node_id == want_node_id {
        return true;
    }
    if !want_type.is_empty()
        && have_type == want_type
        && (want_offset < 0 || have_offset == want_offset)
    {
        return true;
    }
    want_node_id == 0 && want_type.is_empty() && want_offset < 0
}

/// `eventMatchesFilter(e, filter)` (`mcp_bridge.cpp:183-200`).
fn event_matches_filter(e: &EvidenceEvent, filter: &Map<String, Value>) -> bool {
    let source = arg_str(filter, "source");
    let kind = arg_str(filter, "kind");
    let kind_prefixes = string_list_from_json(filter.get("kindPrefix"));
    let type_name = arg_str(filter, "typeName");
    let node_id = to_u64(&arg_str_default(filter, "nodeId", "0"));
    let field_offset = if filter.contains_key("fieldOffset") {
        parse_integer(filter.get("fieldOffset"), -1) as i32
    } else {
        -1
    };
    let since_timestamp = arg_str_default(filter, "sinceTimestamp", "0")
        .trim()
        .parse::<i64>()
        .unwrap_or(0);

    if !source.is_empty() && e.source != source {
        return false;
    }
    if !kind.is_empty() && e.kind != kind {
        return false;
    }
    if !kind_prefixes.is_empty() && !starts_with_any(&e.kind, &kind_prefixes) {
        return false;
    }
    if !type_name.is_empty() && e.type_name != type_name {
        return false;
    }
    if node_id != 0 && e.node_id != node_id {
        return false;
    }
    if field_offset >= 0 && e.field_offset != field_offset {
        return false;
    }
    if since_timestamp > 0 && e.timestamp <= since_timestamp {
        return false;
    }
    true
}

/// `eventJsonForPacket(e, includeData)` (`mcp_bridge.cpp:202-209`).
fn event_json_for_packet(e: &EvidenceEvent, include_data: bool) -> Value {
    let mut o = e.to_json().as_object().cloned().unwrap_or_default();
    if !include_data {
        o.remove("data");
    }
    if e.field_offset >= 0 {
        o.insert("fieldOffsetHex".into(), json!(offset_hex(e.field_offset)));
    }
    Value::Object(o)
}

/// `toolEvidenceRecord` (`mcp_bridge.cpp:2543-2595`).
pub fn tool_evidence_record(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let mut event = EvidenceEvent::default();
        event.id = arg_str(args, "id");
        event.timestamp = arg_str_default(args, "timestamp", "0")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        event.source = arg_str_default(args, "source", "reclass");
        event.kind = arg_str(args, "kind").trim().to_string();
        event.summary = arg_str(args, "summary").trim().to_string();
        event.type_name = arg_str(args, "typeName");
        event.node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
        event.field_offset = if args.contains_key("fieldOffset") {
            parse_integer(arg(args, "fieldOffset"), -1) as i32
        } else {
            -1
        };
        event.address = arg_str(args, "address");
        event.function_name = arg_str(args, "functionName");
        event.function_address = arg_str(args, "functionAddress");
        event.instruction = arg_str(args, "instruction");
        event.confidence = if args.contains_key("confidence") {
            arg(args, "confidence")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0)
        } else {
            -1.0
        };
        event.tags = string_list_from_json(args.get("tags"));
        event.data = arg(args, "data")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null);

        if event.kind.is_empty() {
            out = make_text_result("kind is required.", true);
            return;
        }

        let tree = &tab.data.tree;
        let node_idx = if event.node_id != 0 {
            tree.index_of_id(event.node_id)
        } else {
            -1
        };
        if node_idx >= 0 {
            let ctx = node_evidence_context(tree, event.node_id);
            if event.type_name.is_empty() {
                event.type_name = ctx
                    .get("rootTypeName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
            if event.field_offset < 0 {
                if let Some(fo) = ctx.get("fieldOffset").and_then(Value::as_i64) {
                    event.field_offset = fo as i32;
                }
            }
            if event.summary.is_empty() {
                let label = ctx.get("path").and_then(Value::as_str).unwrap_or("");
                event.summary = if label.is_empty() {
                    event.kind.clone()
                } else {
                    format!("{}: {}", event.kind, label)
                };
            }
        }

        let stored = tab.data.tree.append_evidence_event(event, now_ms());
        tab.data.modified = true;

        let mut o = Map::new();
        o.insert("event".into(), event_json_for_packet(&stored, true));
        o.insert(
            "eventCount".into(),
            json!(tab.data.tree.evidence_events.len() as i64),
        );
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `toolEvidenceTimeline` (`mcp_bridge.cpp:2601-2633`).
pub fn tool_evidence_timeline(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let events = &tab.data.tree.evidence_events;
        let limit = parse_integer(arg(args, "limit"), 50).clamp(1, 500) as usize;
        let include_data = match arg(args, "includeData") {
            Some(v) => v.as_bool().unwrap_or(true),
            None => true,
        };
        let since_id = arg_str(args, "sinceId");

        let mut arr: Vec<Value> = Vec::new();
        let mut matched = 0i64;
        let mut after_since_id = since_id.is_empty();
        for event in events {
            if !after_since_id {
                if event.id == since_id {
                    after_since_id = true;
                }
                continue;
            }
            if !event_matches_filter(event, args) {
                continue;
            }
            matched += 1;
            arr.push(event_json_for_packet(event, include_data));
            if arr.len() > limit {
                arr.remove(0);
            }
        }

        let mut o = Map::new();
        let last_id = arr
            .last()
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let returned = arr.len() as i64;
        o.insert("events".into(), Value::Array(arr));
        o.insert("returned".into(), json!(returned));
        o.insert("matched".into(), json!(matched));
        o.insert("total".into(), json!(events.len() as i64));
        if let Some(id) = last_id {
            o.insert("lastEventId".into(), json!(id));
        }
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `toolEvidenceCaptureChanges` (`mcp_bridge.cpp:2639-2721`).
pub fn tool_evidence_capture_changes(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        // requested node-id set: explicit nodeIds → selection → all history keys.
        let mut requested: HashSet<u64> = HashSet::new();
        if let Some(arr) = arg(args, "nodeIds").and_then(Value::as_array) {
            for v in arr {
                let id = to_u64(v.as_str().unwrap_or(""));
                if id != 0 {
                    requested.insert(id);
                }
            }
        }
        if requested.is_empty() {
            // The C++ masks footer/array-elem/member sub-id bits off selection.
            // resolve_tab selection holds plain node ids here, so insert as-is.
            for &sid in &tab.data.selected_ids {
                if sid != 0 {
                    requested.insert(sid);
                }
            }
        }
        if requested.is_empty() {
            for &k in tab.data.value_history.keys() {
                requested.insert(k);
            }
        }

        let marker = arg_str(args, "marker");
        let kind = arg_str_default(args, "kind", "field_value_history");
        let include_unchanged = arg_bool(args, "includeUnchanged");
        let since_timestamp = arg_str_default(args, "sinceTimestamp", "0")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);

        // Sort the requested ids for deterministic output ordering.
        let mut req_sorted: Vec<u64> = requested.into_iter().collect();
        req_sorted.sort_unstable();

        let mut events_to_store: Vec<EvidenceEvent> = Vec::new();
        for node_id in req_sorted {
            let Some(hist) = tab.data.value_history.get(&node_id) else {
                continue;
            };
            if !include_unchanged && hist.unique_count() <= 1 {
                continue;
            }
            let mut entries: Vec<Value> = Vec::new();
            hist.for_each_with_time(|val, msec| {
                if since_timestamp > 0 && msec <= since_timestamp {
                    return;
                }
                entries.push(json!({"value": val, "timestamp": msec.to_string()}));
            });
            if entries.is_empty() {
                continue;
            }

            let ctx = node_evidence_context(&tab.data.tree, node_id);
            let path = ctx
                .get("path")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| node_id.to_string());
            let unique_count = hist.unique_count();
            let heat_level = hist.heat_level();

            let mut event = EvidenceEvent {
                source: "reclass".into(),
                kind: kind.clone(),
                type_name: ctx
                    .get("rootTypeName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                node_id,
                field_offset: ctx.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32,
                address: ctx
                    .get("absoluteAddress")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                confidence: 0.7,
                summary: format!("{path} changed {unique_count} time(s)"),
                ..EvidenceEvent::default()
            };
            event.tags = vec!["runtime".to_string(), "value-history".to_string()];
            if !marker.is_empty() {
                event.tags.push(marker.clone());
            }
            event.data = json!({
                "marker": marker,
                "entries": Value::Array(entries),
                "heatLevel": heat_level,
                "uniqueCount": unique_count,
                "node": Value::Object(ctx),
            });
            events_to_store.push(event);
        }

        let mut captured: Vec<Value> = Vec::new();
        for event in events_to_store {
            let stored = tab.data.tree.append_evidence_event(event, now_ms());
            captured.push(event_json_for_packet(&stored, true));
        }

        if !captured.is_empty() {
            tab.data.modified = true;
        }

        let count = captured.len() as i64;
        let mut o = Map::new();
        o.insert("captured".into(), Value::Array(captured));
        o.insert("count".into(), json!(count));
        o.insert("marker".into(), json!(marker));
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `toolEvidenceHypothesis` (`mcp_bridge.cpp:2727-2838`).
pub fn tool_evidence_hypothesis(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let action = arg_str_default(args, "action", "list");

        let find_idx = |tab: &TabState, id: &str| -> i32 {
            tab.data
                .tree
                .evidence_hypotheses
                .iter()
                .position(|h| h.id == id)
                .map_or(-1, |p| p as i32)
        };

        if action == "create" {
            let mut h = EvidenceHypothesis {
                claim: arg_str(args, "claim").trim().to_string(),
                label: arg_str(args, "label"),
                status: arg_str_default(args, "status", "open"),
                type_name: arg_str(args, "typeName"),
                node_id: to_u64(&arg_str_default(args, "nodeId", "0")),
                field_offset: if args.contains_key("fieldOffset") {
                    parse_integer(arg(args, "fieldOffset"), -1) as i32
                } else {
                    -1
                },
                confidence: arg(args, "confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                supporting_evidence_ids: string_list_from_json(args.get("supportingEvidenceIds")),
                contradicting_evidence_ids: string_list_from_json(
                    args.get("contradictingEvidenceIds"),
                ),
                recommended_validation: string_list_from_json(args.get("recommendedValidation")),
                notes: arg_str(args, "notes"),
                data: arg(args, "data")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or(Value::Null),
                ..EvidenceHypothesis::default()
            };

            if h.claim.is_empty() {
                out = make_text_result("claim is required for hypothesis create.", true);
                return;
            }
            if h.node_id != 0 {
                let ctx = node_evidence_context(&tab.data.tree, h.node_id);
                if h.type_name.is_empty() {
                    h.type_name = ctx
                        .get("rootTypeName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                }
                if h.field_offset < 0 {
                    h.field_offset =
                        ctx.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32;
                }
            }
            let stored = tab.data.tree.append_evidence_hypothesis(h, now_ms());
            tab.data.modified = true;
            out = make_text_result(&qt_pretty(&stored.to_json()), false);
            return;
        }

        if action == "update" {
            let id = arg_str(args, "id");
            let i = find_idx(tab, &id);
            if i < 0 {
                out = make_text_result(&format!("Hypothesis not found: {id}"), true);
                return;
            }
            let h = &mut tab.data.tree.evidence_hypotheses[i as usize];
            if args.contains_key("status") {
                h.status = arg_str(args, "status");
            }
            if args.contains_key("claim") {
                h.claim = arg_str(args, "claim");
            }
            if args.contains_key("label") {
                h.label = arg_str(args, "label");
            }
            if args.contains_key("typeName") {
                h.type_name = arg_str(args, "typeName");
            }
            if args.contains_key("nodeId") {
                h.node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
            }
            if args.contains_key("fieldOffset") {
                h.field_offset = parse_integer(arg(args, "fieldOffset"), -1) as i32;
            }
            if args.contains_key("confidence") {
                h.confidence = arg(args, "confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(h.confidence);
            }
            if args.contains_key("supportingEvidenceIds") {
                h.supporting_evidence_ids =
                    string_list_from_json(args.get("supportingEvidenceIds"));
            }
            if args.contains_key("contradictingEvidenceIds") {
                h.contradicting_evidence_ids =
                    string_list_from_json(args.get("contradictingEvidenceIds"));
            }
            if args.contains_key("addSupportingEvidenceIds") {
                for eid in string_list_from_json(args.get("addSupportingEvidenceIds")) {
                    h.supporting_evidence_ids.push(eid);
                }
            }
            if args.contains_key("addContradictingEvidenceIds") {
                for eid in string_list_from_json(args.get("addContradictingEvidenceIds")) {
                    h.contradicting_evidence_ids.push(eid);
                }
            }
            remove_duplicates(&mut h.supporting_evidence_ids);
            remove_duplicates(&mut h.contradicting_evidence_ids);
            if args.contains_key("recommendedValidation") {
                h.recommended_validation = string_list_from_json(args.get("recommendedValidation"));
            }
            if args.contains_key("notes") {
                h.notes = arg_str(args, "notes");
            }
            if args.contains_key("data") {
                h.data = arg(args, "data")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            h.updated_at = now_ms();
            let json = h.to_json();
            tab.data.modified = true;
            out = make_text_result(&qt_pretty(&json), false);
            return;
        }

        if action == "get" {
            let id = arg_str(args, "id");
            let i = find_idx(tab, &id);
            if i < 0 {
                out = make_text_result(&format!("Hypothesis not found: {id}"), true);
                return;
            }
            out = make_text_result(
                &qt_pretty(&tab.data.tree.evidence_hypotheses[i as usize].to_json()),
                false,
            );
            return;
        }

        if action != "list" {
            out = make_text_result(&format!("Unknown hypothesis action: {action}"), true);
            return;
        }

        let status = arg_str(args, "status");
        let type_name = arg_str(args, "typeName");
        let node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
        let field_offset = if args.contains_key("fieldOffset") {
            parse_integer(arg(args, "fieldOffset"), -1) as i32
        } else {
            -1
        };
        let limit = parse_integer(arg(args, "limit"), 50).clamp(1, 500) as usize;
        let mut arr: Vec<Value> = Vec::new();
        let mut matched = 0i64;
        for h in &tab.data.tree.evidence_hypotheses {
            if !status.is_empty() && h.status != status {
                continue;
            }
            if !target_matches(
                &h.type_name,
                h.node_id,
                h.field_offset,
                &type_name,
                node_id,
                field_offset,
            ) {
                continue;
            }
            matched += 1;
            arr.push(h.to_json());
            if arr.len() > limit {
                arr.remove(0);
            }
        }
        let returned = arr.len() as i64;
        let mut o = Map::new();
        o.insert("hypotheses".into(), Value::Array(arr));
        o.insert("returned".into(), json!(returned));
        o.insert("matched".into(), json!(matched));
        o.insert(
            "total".into(),
            json!(tab.data.tree.evidence_hypotheses.len() as i64),
        );
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `toolEvidenceProposal` (`mcp_bridge.cpp:2844-2960`). The `apply` action
/// re-invokes `tool_tree_apply` with the stored operations.
pub fn tool_evidence_proposal(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };

    let action = arg_str_default(args, "action", "list");

    // `apply` needs to call tool_tree_apply (which itself borrows the host),
    // so it is handled outside the with_tab borrow below.
    if action == "apply" {
        let id = arg_str(args, "id");
        // Look up the proposal + its operations.
        let mut found = Value::Null; // holds either operations array or error result
        host.with_tab(idx, &mut |tab: &mut TabState| {
            let i = tab
                .data
                .tree
                .evidence_proposals
                .iter()
                .position(|p| p.id == id);
            let Some(i) = i else {
                found = make_text_result(&format!("Proposal not found: {id}"), true);
                return;
            };
            let p = &tab.data.tree.evidence_proposals[i];
            let ops_empty = !p.operations.as_array().is_some_and(|a| !a.is_empty());
            if ops_empty {
                found = make_text_result(
                    &format!("Proposal has no tree.apply operations: {id}"),
                    true,
                );
                return;
            }
            // Stash the operations for the apply call.
            found = json!({ "__ops": p.operations.clone() });
        });
        if found.is_null() {
            return make_text_result("No active tab.", true);
        }
        if found.get("isError").and_then(Value::as_bool) == Some(true) {
            return found;
        }
        let ops = found
            .get("__ops")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));

        // Build tree.apply args.
        let mut apply_args = Map::new();
        apply_args.insert("operations".into(), ops);
        apply_args.insert(
            "macroName".into(),
            json!(format!("Apply evidence proposal {id}")),
        );
        if let Some(ti) = args.get("tabIndex") {
            apply_args.insert("tabIndex".into(), ti.clone());
        }
        let applied = tool_tree_apply(&apply_args, host);
        if applied.get("isError").and_then(Value::as_bool) == Some(true) {
            return applied;
        }

        // Mark the proposal applied and emit it.
        let mut out = Value::Null;
        host.with_tab(idx, &mut |tab: &mut TabState| {
            let Some(i) = tab
                .data
                .tree
                .evidence_proposals
                .iter()
                .position(|p| p.id == id)
            else {
                out = make_text_result(&format!("Proposal not found: {id}"), true);
                return;
            };
            let p = &mut tab.data.tree.evidence_proposals[i];
            p.status = "applied".to_string();
            p.updated_at = now_ms();
            let mut o = p.to_json().as_object().cloned().unwrap_or_default();
            tab.data.modified = true;
            o.insert(
                "applyResult".into(),
                applied
                    .get("content")
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new())),
            );
            out = make_text_result(&qt_pretty(&Value::Object(o)), false);
        });
        if out.is_null() {
            return make_text_result("No active tab.", true);
        }
        return out;
    }

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let find_idx = |tab: &TabState, id: &str| -> i32 {
            tab.data
                .tree
                .evidence_proposals
                .iter()
                .position(|p| p.id == id)
                .map_or(-1, |p| p as i32)
        };

        if action == "create" {
            let mut p = EvidenceProposal {
                title: arg_str(args, "title").trim().to_string(),
                action: arg_str(args, "proposalAction"),
                status: arg_str_default(args, "status", "pending"),
                type_name: arg_str(args, "typeName"),
                node_id: to_u64(&arg_str_default(args, "nodeId", "0")),
                field_offset: if args.contains_key("fieldOffset") {
                    parse_integer(arg(args, "fieldOffset"), -1) as i32
                } else {
                    -1
                },
                confidence: arg(args, "confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                evidence_ids: string_list_from_json(args.get("evidenceIds")),
                operations: arg(args, "operations")
                    .filter(|v| v.is_array())
                    .cloned()
                    .unwrap_or(Value::Null),
                data: arg(args, "data")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or(Value::Null),
                ..EvidenceProposal::default()
            };
            if p.title.is_empty() {
                out = make_text_result("title is required for proposal create.", true);
                return;
            }
            if p.node_id != 0 {
                let ctx = node_evidence_context(&tab.data.tree, p.node_id);
                if p.type_name.is_empty() {
                    p.type_name = ctx
                        .get("rootTypeName")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                }
                if p.field_offset < 0 {
                    p.field_offset =
                        ctx.get("fieldOffset").and_then(Value::as_i64).unwrap_or(-1) as i32;
                }
            }
            let stored = tab.data.tree.append_evidence_proposal(p, now_ms());
            tab.data.modified = true;
            out = make_text_result(&qt_pretty(&stored.to_json()), false);
            return;
        }

        if action == "update" {
            let id = arg_str(args, "id");
            let i = find_idx(tab, &id);
            if i < 0 {
                out = make_text_result(&format!("Proposal not found: {id}"), true);
                return;
            }
            let p = &mut tab.data.tree.evidence_proposals[i as usize];
            if args.contains_key("status") {
                p.status = arg_str(args, "status");
            }
            if args.contains_key("title") {
                p.title = arg_str(args, "title");
            }
            if args.contains_key("proposalAction") {
                p.action = arg_str(args, "proposalAction");
            }
            if args.contains_key("typeName") {
                p.type_name = arg_str(args, "typeName");
            }
            if args.contains_key("nodeId") {
                p.node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
            }
            if args.contains_key("fieldOffset") {
                p.field_offset = parse_integer(arg(args, "fieldOffset"), -1) as i32;
            }
            if args.contains_key("confidence") {
                p.confidence = arg(args, "confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(p.confidence);
            }
            if args.contains_key("evidenceIds") {
                p.evidence_ids = string_list_from_json(args.get("evidenceIds"));
            }
            if args.contains_key("operations") {
                p.operations = arg(args, "operations")
                    .filter(|v| v.is_array())
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            if args.contains_key("data") {
                p.data = arg(args, "data")
                    .filter(|v| v.is_object())
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            p.updated_at = now_ms();
            let json = p.to_json();
            tab.data.modified = true;
            out = make_text_result(&qt_pretty(&json), false);
            return;
        }

        if action == "get" {
            let id = arg_str(args, "id");
            let i = find_idx(tab, &id);
            if i < 0 {
                out = make_text_result(&format!("Proposal not found: {id}"), true);
                return;
            }
            out = make_text_result(
                &qt_pretty(&tab.data.tree.evidence_proposals[i as usize].to_json()),
                false,
            );
            return;
        }

        if action != "list" {
            out = make_text_result(&format!("Unknown proposal action: {action}"), true);
            return;
        }

        let status = arg_str(args, "status");
        let type_name = arg_str(args, "typeName");
        let node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
        let field_offset = if args.contains_key("fieldOffset") {
            parse_integer(arg(args, "fieldOffset"), -1) as i32
        } else {
            -1
        };
        let limit = parse_integer(arg(args, "limit"), 50).clamp(1, 500) as usize;
        let mut arr: Vec<Value> = Vec::new();
        let mut matched = 0i64;
        for p in &tab.data.tree.evidence_proposals {
            if !status.is_empty() && p.status != status {
                continue;
            }
            if !target_matches(
                &p.type_name,
                p.node_id,
                p.field_offset,
                &type_name,
                node_id,
                field_offset,
            ) {
                continue;
            }
            matched += 1;
            arr.push(p.to_json());
            if arr.len() > limit {
                arr.remove(0);
            }
        }
        let returned = arr.len() as i64;
        let mut o = Map::new();
        o.insert("proposals".into(), Value::Array(arr));
        o.insert("returned".into(), json!(returned));
        o.insert("matched".into(), json!(matched));
        o.insert(
            "total".into(),
            json!(tab.data.tree.evidence_proposals.len() as i64),
        );
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `toolEvidenceFocusPacket` (`mcp_bridge.cpp:2966-3087`).
pub fn tool_evidence_focus_packet(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab.", true);
    };
    let tab_index = idx as i64;

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let tree = &tab.data.tree;
        let mut node_id = to_u64(&arg_str_default(args, "nodeId", "0"));
        if node_id == 0 {
            // Pick the first selected node that still exists.
            let mut sel: Vec<u64> = tab.data.selected_ids.iter().copied().collect();
            sel.sort_unstable();
            for sid in sel {
                if tree.index_of_id(sid) >= 0 {
                    node_id = sid;
                    break;
                }
            }
        }

        let mut type_name = arg_str(args, "typeName");
        let mut field_offset = if args.contains_key("fieldOffset") {
            parse_integer(arg(args, "fieldOffset"), -1) as i32
        } else {
            -1
        };
        let mut node_ctx: Map<String, Value> = Map::new();
        if node_id != 0 {
            node_ctx = node_evidence_context(tree, node_id);
            if node_ctx.is_empty() {
                out = make_text_result(&format!("nodeId not found: {node_id}"), true);
                return;
            }
            if type_name.is_empty() {
                type_name = node_ctx
                    .get("rootTypeName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
            if field_offset < 0 {
                field_offset = node_ctx
                    .get("fieldOffset")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1) as i32;
            }
        }

        let function_name = arg_str(args, "functionName");
        let function_address = arg_str(args, "functionAddress");
        let limit = parse_integer(arg(args, "limit"), 40).clamp(1, 200) as usize;
        let include_data = match arg(args, "includeData") {
            Some(v) => v.as_bool().unwrap_or(true),
            None => true,
        };

        let mut relevant_events: Vec<Value> = Vec::new();
        let mut events_by_kind: Map<String, Value> = Map::new();
        let mut events_by_source: Map<String, Value> = Map::new();
        for e in &tree.evidence_events {
            let mut matches = false;
            if node_id != 0 && e.node_id == node_id {
                matches = true;
            }
            if !type_name.is_empty()
                && e.type_name == type_name
                && (field_offset < 0 || e.field_offset == field_offset)
            {
                matches = true;
            }
            if !function_name.is_empty() && e.function_name == function_name {
                matches = true;
            }
            if !function_address.is_empty() && e.function_address == function_address {
                matches = true;
            }
            if !matches {
                continue;
            }
            relevant_events.push(event_json_for_packet(e, include_data));
            if relevant_events.len() > limit {
                relevant_events.remove(0);
            }
            let kc = events_by_kind
                .get(&e.kind)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            events_by_kind.insert(e.kind.clone(), json!(kc));
            let sc = events_by_source
                .get(&e.source)
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + 1;
            events_by_source.insert(e.source.clone(), json!(sc));
        }

        let has_field_target = node_id != 0 || !type_name.is_empty() || field_offset >= 0;
        let mut hypotheses: Vec<Value> = Vec::new();
        for h in &tree.evidence_hypotheses {
            if has_field_target
                && target_matches(
                    &h.type_name,
                    h.node_id,
                    h.field_offset,
                    &type_name,
                    node_id,
                    field_offset,
                )
            {
                hypotheses.push(h.to_json());
            }
        }

        let mut proposals: Vec<Value> = Vec::new();
        for p in &tree.evidence_proposals {
            if p.status != "pending" {
                continue;
            }
            if has_field_target
                && target_matches(
                    &p.type_name,
                    p.node_id,
                    p.field_offset,
                    &type_name,
                    node_id,
                    field_offset,
                )
            {
                proposals.push(p.to_json());
            }
        }

        let mut history_obj: Map<String, Value> = Map::new();
        if node_id != 0 {
            let mut entries: Vec<Value> = Vec::new();
            if let Some(h) = tab.data.value_history.get(&node_id) {
                h.for_each_with_time(|val, msec| {
                    entries.push(json!({"value": val, "timestamp": msec.to_string()}));
                });
                history_obj.insert("heatLevel".into(), json!(h.heat_level()));
                history_obj.insert("uniqueCount".into(), json!(h.unique_count()));
            } else {
                history_obj.insert("heatLevel".into(), json!(0));
                history_obj.insert("uniqueCount".into(), json!(0));
            }
            history_obj.insert("entries".into(), Value::Array(entries));
        }

        let mut suggested: Vec<Value> = Vec::new();
        if node_id != 0 {
            suggested.push(json!("node.history"));
        }
        if type_name.is_empty() {
            suggested.push(json!("tree.search"));
        } else {
            suggested.push(json!("evidence.timeline(typeName, fieldOffset)"));
        }
        suggested.push(json!("evidence.hypothesis(create/update)"));
        suggested.push(json!(
            "evidence.proposal(create pending tree.apply operations)"
        ));

        let mut focus = Map::new();
        focus.insert("tabIndex".into(), json!(tab_index));
        if node_id != 0 {
            focus.insert("nodeId".into(), json!(node_id.to_string()));
        }
        if !type_name.is_empty() {
            focus.insert("typeName".into(), json!(type_name));
        }
        if field_offset >= 0 {
            focus.insert("fieldOffset".into(), json!(field_offset));
            focus.insert("fieldOffsetHex".into(), json!(offset_hex(field_offset)));
        }
        if !function_name.is_empty() {
            focus.insert("functionName".into(), json!(function_name));
        }
        if !function_address.is_empty() {
            focus.insert("functionAddress".into(), json!(function_address));
        }

        let event_count = relevant_events.len() as i64;
        let hypothesis_count = hypotheses.len() as i64;
        let pending_proposal_count = proposals.len() as i64;

        let mut o = Map::new();
        o.insert("focus".into(), Value::Object(focus));
        if !node_ctx.is_empty() {
            o.insert("node".into(), Value::Object(node_ctx));
        }
        if !history_obj.is_empty() {
            o.insert("valueHistory".into(), Value::Object(history_obj));
        }
        o.insert("recentEvidence".into(), Value::Array(relevant_events));
        o.insert("hypotheses".into(), Value::Array(hypotheses));
        o.insert("pendingProposals".into(), Value::Array(proposals));
        o.insert(
            "summary".into(),
            json!({
                "eventCount": event_count,
                "hypothesisCount": hypothesis_count,
                "pendingProposalCount": pending_proposal_count,
                "eventsByKind": Value::Object(events_by_kind),
                "eventsBySource": Value::Object(events_by_source),
            }),
        );
        o.insert("suggestedNextTools".into(), Value::Array(suggested));
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab.", true);
    }
    out
}

/// `QStringList::removeDuplicates()` — drop later duplicates, preserve order.
fn remove_duplicates(v: &mut Vec<String>) {
    let mut seen: HashSet<String> = HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

// ════════════════════════════════════════════════════════════════════
// tree.export_header (mcp_bridge.cpp:3436-3542)
// ════════════════════════════════════════════════════════════════════

/// `toolTreeExportHeader` (`mcp_bridge.cpp:3436-3542`).
pub fn tool_tree_export_header(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let mut out = Value::Null;
    host.with_tab(idx, &mut |tab: &mut TabState| {
        let tree = &tab.data.tree;
        let node_id_str = arg_str(args, "nodeId");
        let type_name = arg_str(args, "typeName");
        let with_children = match arg(args, "withChildren") {
            Some(v) => v.as_bool().unwrap_or(true),
            None => true,
        };

        let mut root_idx: i32 = -1;
        if !node_id_str.is_empty() {
            root_idx = root_index_for_node(tree, tree.index_of_id(to_u64(&node_id_str)));
        } else if !type_name.is_empty() {
            for (i, n) in tree.nodes.iter().enumerate() {
                if n.parent_id != 0 || n.kind != NodeKind::Struct {
                    continue;
                }
                let n_type = if n.struct_type_name.is_empty() {
                    &n.name
                } else {
                    &n.struct_type_name
                };
                if *n_type == type_name || n.name == type_name || n.struct_type_name == type_name {
                    root_idx = i as i32;
                    break;
                }
            }
        } else {
            // First selected node whose root is a Struct.
            let mut sel: Vec<u64> = tab.data.selected_ids.iter().copied().collect();
            sel.sort_unstable();
            for sid in sel {
                root_idx = root_index_for_node(tree, tree.index_of_id(sid));
                if root_idx >= 0 && tree.nodes[root_idx as usize].kind == NodeKind::Struct {
                    break;
                }
            }
        }

        if root_idx < 0 {
            for (i, n) in tree.nodes.iter().enumerate() {
                if n.parent_id == 0 && n.kind == NodeKind::Struct {
                    root_idx = i as i32;
                    break;
                }
            }
        }

        if root_idx < 0 || tree.nodes[root_idx as usize].kind != NodeKind::Struct {
            out = make_text_result("No root struct/union/enum found to export", true);
            return;
        }

        let root_id = tree.nodes[root_idx as usize].id;
        let aliases = if tab.data.type_aliases.is_empty() {
            None
        } else {
            Some(&tab.data.type_aliases)
        };
        let header = if with_children {
            crate::generator::render_cpp_tree(tree, root_id, aliases, false)
        } else {
            crate::generator::render_cpp(tree, root_id, aliases, false)
        };

        // Breadth-first collect of the root subtree + referenced (refId) subtrees.
        let mut collected: HashSet<u64> = HashSet::new();
        let mut queue: Vec<u64> = Vec::new();
        let enqueue_subtree = |id: u64, collected: &mut HashSet<u64>, queue: &mut Vec<u64>| {
            for si in tree.subtree_indices(id) {
                let nid = tree.nodes[si].id;
                if collected.insert(nid) {
                    queue.push(nid);
                }
            }
        };
        enqueue_subtree(root_id, &mut collected, &mut queue);
        let mut qi = 0;
        while qi < queue.len() {
            let nid = queue[qi];
            qi += 1;
            let nidx = tree.index_of_id(nid);
            if nidx < 0 {
                continue;
            }
            let ref_id = tree.nodes[nidx as usize].ref_id;
            if with_children && ref_id != 0 && !collected.contains(&ref_id) {
                enqueue_subtree(ref_id, &mut collected, &mut queue);
            }
        }

        // Emit collected nodes in queue order (insertion order, like the C++
        // QSet-then-iterate; we keep a deterministic insertion order).
        let mut node_arr: Vec<Value> = Vec::new();
        for &nid in &queue {
            let nidx = tree.index_of_id(nid);
            if nidx < 0 {
                continue;
            }
            let n = &tree.nodes[nidx as usize];
            let mut nj = n.to_json().as_object().cloned().unwrap_or_default();
            nj.insert(
                "computedOffset".into(),
                json!(tree.compute_offset(nidx).to_string()),
            );
            if matches!(n.kind, NodeKind::Struct | NodeKind::Array) {
                nj.insert("computedSize".into(), json!(tree.struct_span(n.id)));
            } else {
                nj.insert("computedSize".into(), json!(n.byte_size()));
            }
            node_arr.push(Value::Object(nj));
        }

        let root = &tree.nodes[root_idx as usize];
        let exported_type_name = if root.struct_type_name.is_empty() {
            root.name.clone()
        } else {
            root.struct_type_name.clone()
        };
        let node_count = node_arr.len() as i64;
        let mut o = Map::new();
        o.insert("nodeId".into(), json!(root.id.to_string()));
        o.insert("typeName".into(), json!(exported_type_name));
        o.insert("classKeyword".into(), json!(root.resolved_class_keyword()));
        o.insert("pointerSize".into(), json!(tree.pointer_size));
        o.insert("withChildren".into(), json!(with_children));
        o.insert("header".into(), json!(header));
        o.insert("nodes".into(), Value::Array(node_arr));
        o.insert("nodeCount".into(), json!(node_count));
        out = make_text_result(&qt_pretty(&Value::Object(o)), false);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

// ════════════════════════════════════════════════════════════════════
// analysis.pointer_chain
// ════════════════════════════════════════════════════════════════════

pub fn tool_analysis_pointer_chain(args: &Map<String, Value>, host: &mut dyn McpHost) -> Value {
    let Some(idx) = resolve_tab(args, host) else {
        return make_text_result("No active tab", true);
    };

    let mut out = Value::Null;
    host.with_tab_ref(idx, &mut |tab: &TabState| {
        let provider = tab.data.provider.clone();
        if provider.size() <= 0 && provider.enumerate_regions().is_empty() {
            out = make_text_result("No provider attached", true);
            return;
        }

        let base_relative = arg_bool(args, "baseRelative");
        let targets = match pointer_chain_targets_from_args(args, tab.data.tree.base_address) {
            Ok(targets) => targets
                .into_iter()
                .map(|target| {
                    if base_relative {
                        tab.data.tree.base_address.saturating_add(target)
                    } else {
                        target
                    }
                })
                .collect::<Vec<_>>(),
            Err(err) => {
                out = make_text_result(&err, true);
                return;
            }
        };

        let pointer_size = match provider.pointer_size() {
            4 => 4,
            _ => 8,
        };
        let request = PointerMapRequest {
            pointer_size,
            alignment: pointer_size,
            max_pointers: pointer_arg_usize(args, "maxPointers", 2_000_000, 0, 10_000_000),
            filter_executable: arg_bool(args, "filterExecutable"),
            filter_writable: arg_bool_default(args, "filterWritable", true),
            private_only: arg_bool(args, "privateOnly"),
            skip_system_modules: arg_bool(args, "skipSystemModules"),
            start_address: 0,
            end_address: if arg_bool(args, "userModeOnly") {
                if pointer_size == 8 {
                    0x0000_7FFF_FFFF_FFFF
                } else {
                    0x7FFF_FFFF
                }
            } else {
                0
            },
            constrain_regions: pointer_regions_arg(args),
            ..PointerMapRequest::default()
        };
        let abort = std::sync::atomic::AtomicBool::new(false);
        let map = match build_pointer_map(provider.as_ref(), &request, &abort) {
            Ok(map) => map,
            Err(err) => {
                out = make_text_result(&err, true);
                return;
            }
        };
        let chains = find_pointer_chains(
            &map,
            &PointerChainRequest {
                targets: targets.clone(),
                max_depth: pointer_arg_usize(args, "maxDepth", 3, 1, 8),
                max_offset: pointer_arg_u64(args, "maxOffset", 0x1000),
                max_results: pointer_arg_usize(args, "maxResults", 100, 1, 1000),
            },
            &abort,
        );
        let stats = map.stats();
        let source = match stats.source {
            PointerMapSource::GenericProvider => "provider",
            PointerMapSource::MemflowScanflow => "scanflow",
        };
        let chain_json = chains
            .chains
            .iter()
            .map(|chain| {
                json!({
                    "target": format!("0x{:X}", chain.target),
                    "baseAddress": format!("0x{:X}", chain.base_address()),
                    "display": chain.display(),
                    "steps": chain.steps.iter().map(|step| {
                        json!({
                            "pointerAddress": format!("0x{:X}", step.pointer_address),
                            "pointsTo": format!("0x{:X}", step.points_to),
                            "offset": step.offset,
                            "offsetHex": format_signed_offset(step.offset),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        let result = json!({
            "targets": targets.iter().map(|target| format!("0x{target:X}")).collect::<Vec<_>>(),
            "pointerMap": {
                "source": source,
                "pointersFound": stats.pointers_found,
                "regionsScanned": stats.regions_scanned,
                "truncated": stats.truncated,
            },
            "chains": chain_json,
            "chainCount": chains.chains.len(),
            "truncated": chains.truncated,
        });
        out = make_text_result(&qt_pretty(&result), false);
    });

    if out.is_null() {
        return make_text_result("No active tab", true);
    }
    out
}

fn pointer_chain_targets_from_args(
    args: &Map<String, Value>,
    default_base: u64,
) -> Result<Vec<u64>, String> {
    if let Some(Value::Array(items)) = arg(args, "targets") {
        let mut targets = Vec::new();
        for item in items {
            targets.push(parse_pointer_address_value(item).ok_or_else(|| {
                "analysis.pointer_chain targets must be hex strings or integers".to_string()
            })?);
        }
        if !targets.is_empty() {
            return Ok(targets);
        }
    }
    for key in ["target", "address"] {
        if let Some(value) = arg(args, key) {
            return parse_pointer_address_value(value)
                .map(|target| vec![target])
                .ok_or_else(|| format!("Invalid {key}"));
        }
    }
    if default_base != 0 {
        Ok(vec![default_base])
    } else {
        Err("analysis.pointer_chain requires target, targets, or address".to_string())
    }
}

fn parse_pointer_address_value(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => parse_pointer_address_str(s),
        _ => None,
    }
}

fn parse_pointer_address_str(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed)
        .chars()
        .filter(|ch| *ch != '`' && *ch != '_')
        .collect::<String>();
    if hex.is_empty() || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(&hex, 16).ok()
}

fn pointer_arg_u64(args: &Map<String, Value>, key: &str, default: u64) -> u64 {
    arg(args, key)
        .and_then(parse_pointer_address_value)
        .unwrap_or(default)
}

fn pointer_arg_usize(
    args: &Map<String, Value>,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> usize {
    let raw = match arg(args, key) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
    .unwrap_or(default as u64) as usize;
    raw.clamp(min, max)
}

fn pointer_regions_arg(args: &Map<String, Value>) -> Vec<AddressRange> {
    let Some(Value::Array(regions)) = arg(args, "regions") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for region in regions {
        let Value::Array(pair) = region else {
            continue;
        };
        if pair.len() < 2 {
            continue;
        }
        let Some(start) = parse_pointer_address_value(&pair[0]) else {
            continue;
        };
        let Some(end) = parse_pointer_address_value(&pair[1]) else {
            continue;
        };
        if end > start {
            out.push(AddressRange { start, end });
        }
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

    fn write_u64(buf: &mut [u8], addr: usize, value: u64) {
        buf[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
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
        // Evidence summary defaults to 0 on a fresh tree (`mcp_bridge.cpp:1488-1492`).
        assert_eq!(state["evidence"]["eventCount"], 0);
        assert_eq!(state["evidence"]["hypothesisCount"], 0);
        assert_eq!(state["evidence"]["proposalCount"], 0);
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
    fn tree_apply_change_comment_roundtrip() {
        // change_comment op (`mcp_bridge.cpp:1887-1899`): trims, pushes
        // ChangeComment, and a single undo reverts it.
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
                {"op": "change_comment", "nodeId": id.to_string(), "comment": "  IDA refs: sub_140001000  "}
            ]
        });
        let r = tool_tree_apply(&map(ops), &mut h);
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Applied 1 operation"));
        assert!(r.get("isError").is_none());
        // trimmed comment applied
        h.with_tab(0, &mut |t| {
            let idx = t.data.tree.index_of_id(id);
            assert_eq!(
                t.data.tree.nodes[idx as usize].comment,
                "IDA refs: sub_140001000"
            );
            assert!(t.can_undo());
        });
        // single undo reverts the comment back to empty
        h.with_tab(0, &mut |t| t.undo());
        h.with_tab(0, &mut |t| {
            let idx = t.data.tree.index_of_id(id);
            assert_eq!(t.data.tree.nodes[idx as usize].comment, "");
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
    fn tree_apply_union_ops_skip_not_applied() {
        // The MCP TabState holds no Controller, so the C++ controller calls
        // (groupIntoUnion / dissolveUnion at controller.cpp:1876/1959) are
        // out-of-scope here. Unlike C++ — which restructures and counts these
        // as applied — Rust must skip honestly rather than report a false
        // "Applied". applied stays 0 (isError) and the tree is untouched.
        let mut tab = TabState::new();
        let pi = tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "S".into(),
            ..Node::default()
        });
        let parent_id = tab.data.tree.nodes[pi].id;
        let a = tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "a".into(),
            parent_id,
            ..Node::default()
        });
        let b = tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "b".into(),
            parent_id,
            ..Node::default()
        });
        let id_a = tab.data.tree.nodes[a].id;
        let id_b = tab.data.tree.nodes[b].id;
        let before = tab.data.tree.nodes.len();
        let mut h = TestHost::with_tab(tab);

        // group_into_union with two valid sibling ids → skipped, not applied.
        let ops = json!({
            "operations": [
                {"op": "group_into_union", "nodeIds": [id_a.to_string(), id_b.to_string()]}
            ]
        });
        let r = tool_tree_apply(&map(ops), &mut h);
        assert_eq!(r["isError"], json!(true));
        let txt = r["content"][0]["text"].as_str().unwrap();
        assert!(txt.contains("Applied 0 operations"));
        assert!(txt.contains("group_into_union not available in this build"));

        // dissolve_union on an existing node → skipped, not applied.
        let ops = json!({
            "operations": [
                {"op": "dissolve_union", "nodeId": parent_id.to_string()}
            ]
        });
        let r = tool_tree_apply(&map(ops), &mut h);
        assert_eq!(r["isError"], json!(true));
        let txt = r["content"][0]["text"].as_str().unwrap();
        assert!(txt.contains("Applied 0 operations"));
        assert!(txt.contains("dissolve_union not available in this build"));

        // Tree was never restructured: node count is unchanged (the empty
        // tool_tree_apply macro still records an undo entry, as in C++
        // QUndoStack, so can_undo() is not a meaningful signal here).
        h.with_tab(0, &mut |t| {
            assert_eq!(t.data.tree.nodes.len(), before);
        });
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
        // bare pid routes to the local process provider when compiled in; in
        // feature sets without it, the tool points the user at the core memflow
        // provider (the always-compiled live-process path).
        let r = tool_source_switch(&map(json!({"pid": 1234})), &mut h);
        #[cfg(feature = "process-provider")]
        assert_eq!(r["isError"], json!(true));
        #[cfg(not(feature = "process-provider"))]
        {
            assert_eq!(r["isError"], json!(true));
            assert!(r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("provider:\"memflow\""));
        }
        // no args → error
        let r = tool_source_switch(&map(json!({})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(
            r["content"][0]["text"],
            "Provide sourceIndex, filePath, provider:\"processmemory\", provider:\"kernelmemory\", provider:\"windbgmemory\", or provider:\"memflow\""
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

    #[test]
    fn source_modules_empty_for_provider_without_modules() {
        let mut h = TestHost::new();
        h.project_new();
        let r = tool_source_modules(&map(json!({})), &mut h);
        assert!(r.get("isError").is_none());
        assert_eq!(r["content"][0]["text"], "[]");
    }

    // ════════════════════════════════════════════════════════════════
    // evidence.*  (mcp_bridge.cpp:2543-3087)
    // ════════════════════════════════════════════════════════════════

    /// A host with a Player struct → health Int32 field for evidence tests.
    fn evidence_host() -> (TestHost, u64, u64) {
        let mut tab = TabState::new();
        let ri = tab.data.tree.add_node(Node {
            kind: NodeKind::Struct,
            name: "Player".into(),
            ..Node::default()
        });
        let rid = tab.data.tree.nodes[ri].id;
        let ci = tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "health".into(),
            parent_id: rid,
            offset: 8,
            ..Node::default()
        });
        let cid = tab.data.tree.nodes[ci].id;
        (TestHost::with_tab(tab), rid, cid)
    }

    // ── evidence.record ──
    #[test]
    fn evidence_record_requires_kind() {
        let (mut h, _rid, _cid) = evidence_host();
        let r = tool_evidence_record(&map(json!({"summary": "no kind here"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], "kind is required.");
    }

    #[test]
    fn evidence_record_enriches_from_node_and_sets_modified() {
        let (mut h, _rid, cid) = evidence_host();
        let r = tool_evidence_record(
            &map(json!({"kind": "marker", "nodeId": cid.to_string()})),
            &mut h,
        );
        assert!(r.get("isError").is_none());
        let out = parse_text(&r);
        let ev = &out["event"];
        // id assigned, source defaulted, typeName + fieldOffset from node ctx.
        assert_eq!(ev["id"], "ev_1");
        assert_eq!(ev["source"], "reclass");
        assert_eq!(ev["typeName"], "Player");
        assert_eq!(ev["fieldOffset"], 8);
        assert_eq!(ev["fieldOffsetHex"], "0x8");
        // summary synthesized "kind: path"
        assert_eq!(ev["summary"], "marker: Player.health");
        assert_eq!(out["eventCount"], 1);
        h.with_tab(0, &mut |t| {
            assert!(t.data.modified);
            assert_eq!(t.data.tree.evidence_events.len(), 1);
        });
    }

    // ── evidence.timeline ──
    #[test]
    fn evidence_timeline_filters_and_paginates() {
        let (mut h, _rid, cid) = evidence_host();
        tool_evidence_record(&map(json!({"kind": "marker", "source": "user"})), &mut h);
        tool_evidence_record(
            &map(json!({"kind": "writer_hit", "source": "debugger", "nodeId": cid.to_string()})),
            &mut h,
        );

        // no filter → both, total 2
        let r = tool_evidence_timeline(&map(json!({})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["total"], 2);
        assert_eq!(out["returned"], 2);
        assert_eq!(out["lastEventId"], "ev_2");

        // filter by kind
        let r = tool_evidence_timeline(&map(json!({"kind": "writer_hit"})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["returned"], 1);
        assert_eq!(out["events"][0]["kind"], "writer_hit");

        // filter by source
        let r = tool_evidence_timeline(&map(json!({"source": "user"})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["returned"], 1);
        assert_eq!(out["events"][0]["source"], "user");

        // sinceId skips up to and including that id
        let r = tool_evidence_timeline(&map(json!({"sinceId": "ev_1"})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["returned"], 1);
        assert_eq!(out["events"][0]["id"], "ev_2");
    }

    // ── evidence.capture_changes ──
    #[test]
    fn evidence_capture_changes_promotes_value_history() {
        let (mut h, _rid, cid) = evidence_host();
        h.with_tab(0, &mut |t| {
            let mut vh = ValueHistory::new();
            vh.record("100");
            vh.record("80");
            vh.record("60");
            t.data.value_history.insert(cid, vh);
        });
        let r = tool_evidence_capture_changes(
            &map(json!({"nodeIds": [cid.to_string()], "marker": "after_damage"})),
            &mut h,
        );
        let out = parse_text(&r);
        assert_eq!(out["count"], 1);
        assert_eq!(out["marker"], "after_damage");
        let ev = &out["captured"][0];
        assert_eq!(ev["kind"], "field_value_history");
        assert_eq!(ev["typeName"], "Player");
        assert!(ev["summary"]
            .as_str()
            .unwrap()
            .contains("changed 3 time(s)"));
        // marker is appended as a tag.
        let tags: Vec<String> = ev["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(tags.contains(&"after_damage".to_string()));
        h.with_tab(0, &mut |t| assert!(t.data.modified));
    }

    #[test]
    fn evidence_capture_changes_skips_unchanged_by_default() {
        let (mut h, _rid, cid) = evidence_host();
        h.with_tab(0, &mut |t| {
            let mut vh = ValueHistory::new();
            vh.record("5"); // single unique value
            t.data.value_history.insert(cid, vh);
        });
        let r = tool_evidence_capture_changes(&map(json!({"nodeIds": [cid.to_string()]})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["count"], 0);
    }

    // ── evidence.hypothesis ──
    #[test]
    fn evidence_hypothesis_create_requires_claim() {
        let (mut h, _rid, _cid) = evidence_host();
        let r = tool_evidence_hypothesis(&map(json!({"action": "create"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(
            r["content"][0]["text"],
            "claim is required for hypothesis create."
        );
    }

    #[test]
    fn evidence_hypothesis_create_get_list_roundtrip() {
        let (mut h, _rid, cid) = evidence_host();
        let r = tool_evidence_hypothesis(
            &map(json!({
                "action": "create",
                "claim": "health is Int32 at +8",
                "nodeId": cid.to_string(),
                "confidence": 0.8
            })),
            &mut h,
        );
        let created = parse_text(&r);
        assert_eq!(created["id"], "hyp_1");
        assert_eq!(created["status"], "open");
        assert_eq!(created["typeName"], "Player"); // enriched from node
        assert_eq!(created["fieldOffset"], 8);

        // get
        let r = tool_evidence_hypothesis(&map(json!({"action": "get", "id": "hyp_1"})), &mut h);
        let got = parse_text(&r);
        assert_eq!(got["claim"], "health is Int32 at +8");

        // get missing
        let r = tool_evidence_hypothesis(&map(json!({"action": "get", "id": "hyp_99"})), &mut h);
        assert_eq!(r["isError"], json!(true));

        // list
        let r = tool_evidence_hypothesis(&map(json!({"action": "list"})), &mut h);
        let listed = parse_text(&r);
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["returned"], 1);

        // update + dedup of supporting evidence ids
        let r = tool_evidence_hypothesis(
            &map(json!({
                "action": "update",
                "id": "hyp_1",
                "status": "confirmed",
                "supportingEvidenceIds": ["ev_1", "ev_1", "ev_2"]
            })),
            &mut h,
        );
        let updated = parse_text(&r);
        assert_eq!(updated["status"], "confirmed");
        assert_eq!(
            updated["supportingEvidenceIds"].as_array().unwrap().len(),
            2
        );
    }

    // ── evidence.proposal ──
    #[test]
    fn evidence_proposal_create_requires_title() {
        let (mut h, _rid, _cid) = evidence_host();
        let r = tool_evidence_proposal(&map(json!({"action": "create"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert_eq!(
            r["content"][0]["text"],
            "title is required for proposal create."
        );
    }

    #[test]
    fn evidence_proposal_apply_runs_tree_apply() {
        let (mut h, _rid, cid) = evidence_host();
        // a proposal that renames + retypes the health field
        let ops = json!([
            {"op": "rename", "nodeId": cid.to_string(), "name": "hp"},
            {"op": "change_kind", "nodeId": cid.to_string(), "kind": "UInt32"}
        ]);
        let r = tool_evidence_proposal(
            &map(json!({"action": "create", "title": "Rename health", "operations": ops})),
            &mut h,
        );
        let created = parse_text(&r);
        assert_eq!(created["id"], "prop_1");
        assert_eq!(created["status"], "pending");

        // apply re-invokes tree.apply
        let r = tool_evidence_proposal(&map(json!({"action": "apply", "id": "prop_1"})), &mut h);
        let applied = parse_text(&r);
        assert_eq!(applied["status"], "applied");
        assert!(applied.get("applyResult").is_some());
        h.with_tab(0, &mut |t| {
            let idx = t.data.tree.index_of_id(cid);
            assert_eq!(t.data.tree.nodes[idx as usize].name, "hp");
            assert_eq!(t.data.tree.nodes[idx as usize].kind, NodeKind::UInt32);
        });
    }

    #[test]
    fn evidence_proposal_apply_no_operations_is_error() {
        let (mut h, _rid, _cid) = evidence_host();
        tool_evidence_proposal(&map(json!({"action": "create", "title": "empty"})), &mut h);
        let r = tool_evidence_proposal(&map(json!({"action": "apply", "id": "prop_1"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("no tree.apply operations"));
    }

    // ── evidence.focus_packet ──
    #[test]
    fn evidence_focus_packet_compiles_context() {
        let (mut h, _rid, cid) = evidence_host();
        // record one matching event + create a hypothesis on the same node
        tool_evidence_record(
            &map(json!({"kind": "marker", "nodeId": cid.to_string()})),
            &mut h,
        );
        tool_evidence_hypothesis(
            &map(json!({"action": "create", "claim": "is hp", "nodeId": cid.to_string()})),
            &mut h,
        );

        let r = tool_evidence_focus_packet(&map(json!({"nodeId": cid.to_string()})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["focus"]["nodeId"], cid.to_string());
        assert_eq!(out["focus"]["typeName"], "Player");
        assert_eq!(out["focus"]["fieldOffset"], 8);
        assert_eq!(out["node"]["path"], "Player.health");
        assert_eq!(out["summary"]["eventCount"], 1);
        assert_eq!(out["summary"]["hypothesisCount"], 1);
        // suggested next tools always include node.history when a node is set
        let suggested: Vec<String> = out["suggestedNextTools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(suggested.contains(&"node.history".to_string()));
    }

    #[test]
    fn evidence_focus_packet_missing_node_is_error() {
        let (mut h, _rid, _cid) = evidence_host();
        let r = tool_evidence_focus_packet(&map(json!({"nodeId": "99999"})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nodeId not found"));
    }

    // ════════════════════════════════════════════════════════════════
    // analysis.pointer_chain
    // ════════════════════════════════════════════════════════════════

    #[test]
    fn analysis_pointer_chain_uses_active_provider_pointer_map() {
        let mut bytes = vec![0u8; 0x100];
        write_u64(&mut bytes, 0x08, 0x20);
        write_u64(&mut bytes, 0x20, 0x80);
        let mut tab = TabState::new();
        tab.data.provider = Arc::new(BufferProvider::new(bytes, "ptr.bin"));
        let mut h = TestHost::with_tab(tab);

        let r = tool_analysis_pointer_chain(
            &map(json!({
                "target": "0x80",
                "maxDepth": 2,
                "maxOffset": "0x0",
                "filterWritable": false
            })),
            &mut h,
        );

        assert_ne!(r["isError"], json!(true));
        let out = parse_text(&r);
        assert_eq!(out["chainCount"], 2);
        let bases: Vec<String> = out["chains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chain| chain["baseAddress"].as_str().unwrap().to_string())
            .collect();
        assert!(bases.contains(&"0x8".to_string()));
        assert!(bases.contains(&"0x20".to_string()));
        assert_eq!(out["pointerMap"]["source"], "provider");
    }

    // ════════════════════════════════════════════════════════════════
    // tree.export_header  (mcp_bridge.cpp:3436-3542)
    // ════════════════════════════════════════════════════════════════

    #[test]
    fn export_header_by_node_id_emits_header_and_nodes() {
        let (mut h, rid, _cid) = evidence_host();
        let r = tool_tree_export_header(&map(json!({"nodeId": rid.to_string()})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["nodeId"], rid.to_string());
        assert_eq!(out["typeName"], "Player");
        assert_eq!(out["withChildren"], json!(true));
        assert_eq!(out["pointerSize"], 8);
        // header text contains the struct + the field name
        let header = out["header"].as_str().unwrap();
        assert!(header.contains("Player"));
        assert!(header.contains("health"));
        // nodes array covers the root + child (2 nodes)
        assert_eq!(out["nodeCount"], 2);
    }

    #[test]
    fn export_header_by_type_name_resolves_root() {
        let (mut h, rid, _cid) = evidence_host();
        let r = tool_tree_export_header(&map(json!({"typeName": "Player"})), &mut h);
        let out = parse_text(&r);
        assert_eq!(out["nodeId"], rid.to_string());
        assert_eq!(out["typeName"], "Player");
    }

    #[test]
    fn export_header_no_root_is_error() {
        let mut tab = TabState::new();
        // only a non-struct top-level node
        tab.data.tree.add_node(Node {
            kind: NodeKind::Int32,
            name: "lonely".into(),
            ..Node::default()
        });
        let mut h = TestHost::with_tab(tab);
        let r = tool_tree_export_header(&map(json!({})), &mut h);
        assert_eq!(r["isError"], json!(true));
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No root struct/union/enum found"));
    }
}
